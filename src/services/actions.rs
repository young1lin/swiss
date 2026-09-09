//! The built-in process capability: "process.exec" and "process.legacy-command" (docs/10 §2,
//! §6). Both run over the shared supervisor; they differ ONLY in how the input names the
//! command line:
//!
//! - process.exec is the typed form: program + args ARRAY + cwd + env. Each string may carry
//!   env references; they are resolved strictly (a missing variable is an error NAMING it)
//!   and each argument stays ONE argument — a value containing spaces is never re-split
//!   into several. No shell is involved, ever.
//! - process.legacy-command is the compatibility form: the old "command" string through the
//!   old tokenizer (quoting rules), refs resolved leniently exactly as the legacy runner
//!   did — an unset variable resolves to empty, not to an error — so an existing jobs.json
//!   command keeps its exact historical meaning.
//!
//! Neither form puts a shell behind the user's back: a shell is an explicit "cmd /c" or
//! "sh -c" in the command itself. Solved input is never logged back with its secrets: every
//! resolved ref value is registered for output masking.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::services::action::{Action, ActionError, ActionOutcome, CancelHandle};
use crate::services::process::{
    resolve_refs_strict, mask_secrets, ProcLimits, ProcSpec, Supervisor, DEFAULT_OUTPUT_MAX_BYTES,
};

/// The supervisor deadline for action-driven runs: the run coordinator owns deadline
/// policy (it cancels through the handle, so the tree kill and reader joins all happen
/// inside the supervisor's cancel path). A "never" deadline here keeps ONE timeout owner
/// per path — direct callers (the legacy runner) pass their own.
const COORDINATED_TIMEOUT_MS: u64 = u64::MAX;

/// Names mentioned as env references in a string, in order, deduplicated. Same scanner as
/// the resolvers; used to register resolved values for output masking.
fn ref_names(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let mut j = i + 2;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' && j > i + 2 {
                let name = value[i + 2..j].to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Resolve one input string strictly and remember what the refs resolved to (for masking).
fn resolve_with_secrets(
    value: &str,
    field: &str,
    secrets: &mut Vec<String>,
) -> Result<String, ActionError> {
    for name in ref_names(value) {
        if let Ok(v) = std::env::var(&name) {
            if v.len() >= 8 && !secrets.contains(&v) {
                secrets.push(v);
            }
        }
    }
    resolve_refs_strict(value).map_err(|e| ActionError::InvalidInput(format!("{field}: {e}")))
}

/// Validate the typed input strictly: known keys only, right types, non-empty program.
/// Returns the resolved spec plus the values to mask from captured output.
fn parse_exec_input(input: &Value) -> Result<ProcSpec, ActionError> {
    let invalid = |m: String| ActionError::InvalidInput(m);
    let obj = input.as_object().ok_or_else(|| invalid("input must be an object".into()))?;
    for key in obj.keys() {
        if !matches!(key.as_str(), "program" | "args" | "cwd" | "env") {
            return Err(invalid(format!("input: unknown field {key:?} (known: program, args, cwd, env)")));
        }
    }
    let program_raw = obj
        .get("program")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("input.program: required non-empty string".into()))?;
    let mut secrets: Vec<String> = Vec::new();
    let program = resolve_with_secrets(program_raw, "input.program", &mut secrets)?;
    let mut args = Vec::new();
    match obj.get("args") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                let raw = item
                    .as_str()
                    .ok_or_else(|| invalid(format!("input.args[{i}]: must be a string")))?;
                // One arg stays one arg: substitution never re-tokenizes (docs/10 §6).
                args.push(resolve_with_secrets(raw, &format!("input.args[{i}]"), &mut secrets)?);
            }
        }
        Some(_) => return Err(invalid("input.args: must be an array of strings".into())),
    }
    let cwd = match obj.get("cwd") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => {
            Some(resolve_with_secrets(s, "input.cwd", &mut secrets)?)
        }
        Some(_) => return Err(invalid("input.cwd: must be a string".into())),
    };
    let mut env = Vec::new();
    match obj.get("env") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, v) in map {
                let raw = v
                    .as_str()
                    .ok_or_else(|| invalid(format!("input.env.{k}: must be a string")))?;
                env.push((k.clone(), resolve_with_secrets(raw, &format!("input.env.{k}"), &mut secrets)?));
            }
        }
        Some(_) => return Err(invalid("input.env: must be an object of strings".into())),
    }
    // Resolve the program through PATH(+PATHEXT) like every spawner here, so "cargo" finds
    // cargo.exe and an unresolvable name fails at spawn with the OS's own error.
    let path = std::env::var("PATH").unwrap_or_default();
    let resolved = crate::pathenv::resolve_command(&program, &path)
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| program.into());
    Ok(ProcSpec {
        program: resolved.to_string_lossy().into_owned(),
        args,
        cwd,
        env,
        secrets,
    })
}

/// The typed process capability ("process.exec").
pub struct ProcessExecAction {
    supervisor: Arc<Supervisor>,
}

impl ProcessExecAction {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        ProcessExecAction { supervisor }
    }
}

#[async_trait]
impl Action for ProcessExecAction {
    fn type_name(&self) -> &'static str {
        "process.exec"
    }

    fn title(&self) -> String {
        "Run a program (argv array, no shell)".to_string()
    }

    fn provider(&self) -> &'static str {
        "process"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["program"],
            "additionalProperties": false,
            "properties": {
                "program": { "type": "string", "description": "program name or path; resolved through PATH" },
                "args": { "type": "array", "items": { "type": "string" }, "description": "argv; each entry stays ONE argument" },
                "cwd": { "type": "string", "description": "working directory" },
                "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "environment additions on top of the inherited environment" }
            },
            "x-env-refs": "${ENV_VAR} references are resolved at run time; a missing variable is an error naming it"
        })
    }

    async fn execute(&self, input: &Value, cancel: CancelHandle) -> Result<ActionOutcome, ActionError> {
        let spec = parse_exec_input(input)?;
        let limits = ProcLimits {
            timeout_ms: COORDINATED_TIMEOUT_MS,
            output_max_bytes: DEFAULT_OUTPUT_MAX_BYTES,
        };
        let r = self.supervisor.run(spec, limits, cancel).await;
        let mut out = ActionOutcome {
            ok: r.ok,
            ms: r.ms,
            pid: r.pid,
            exit_code: r.exit_code,
            timed_out: r.timed_out,
            canceled: r.canceled,
            error: r.error,
            chars: r.chars,
            output: r.output,
            meta: Map::new(),
        };
        if r.output_truncated {
            out.meta.insert("outputTruncated".into(), json!(true));
        }
        Ok(out)
    }
}

/// The compatibility capability ("process.legacy-command"): the pre-v2 "command" string,
/// tokenized and resolved exactly as the legacy runner always did.
pub struct LegacyCommandAction {
    supervisor: Arc<Supervisor>,
}

impl LegacyCommandAction {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        LegacyCommandAction { supervisor }
    }
}

#[async_trait]
impl Action for LegacyCommandAction {
    fn type_name(&self) -> &'static str {
        "process.legacy-command"
    }

    fn title(&self) -> String {
        "Run a legacy command line (old tokenizer)".to_string()
    }

    fn provider(&self) -> &'static str {
        "process"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "additionalProperties": false,
            "properties": {
                "command": { "type": "string", "description": "ARGV-style command line; quoted per the legacy tokenizer; no shell unless the command is one" },
                "cwd": { "type": "string", "description": "working directory" }
            },
            "x-env-refs": "${ENV_VAR} references resolve leniently (an unset variable becomes empty), preserving legacy behaviour"
        })
    }

    async fn execute(&self, input: &Value, cancel: CancelHandle) -> Result<ActionOutcome, ActionError> {
        let invalid = |m: String| ActionError::InvalidInput(m);
        let obj = input
            .as_object()
            .ok_or_else(|| invalid("input must be an object".to_string()))?;
        for key in obj.keys() {
            if !matches!(key.as_str(), "command" | "cwd") {
                return Err(invalid(format!("input: unknown field {key:?} (known: command, cwd)")));
            }
        }
        let command = obj
            .get("command")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid("input.command: required non-empty string".into()))?;
        // Lenient resolution (unset -> empty) and post-resolution tokenization are BOTH the
        // documented legacy behaviour; changing either would silently rewrite old jobs.
        let expanded = crate::config::resolve_env_refs(command);
        // Mask what the refs resolved to (a credential referenced in a command must not
        // survive into captured output just because the legacy path is lenient).
        let mut secrets: Vec<String> = Vec::new();
        for name in ref_names(command) {
            if let Ok(v) = std::env::var(&name) {
                if v.len() >= 8 && !secrets.contains(&v) {
                    secrets.push(v);
                }
            }
        }
        let argv = crate::adapters::proc::tokenize_command(&expanded);
        let Some((program, args)) = argv.split_first() else {
            return Err(invalid("input.command: expands to an empty command line".into()));
        };
        let path = std::env::var("PATH").unwrap_or_default();
        let resolved = crate::pathenv::resolve_command(program, &path)
            .map(|p| p.into_os_string())
            .unwrap_or_else(|| program.as_str().into());
        let cwd = match obj.get("cwd") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(crate::config::resolve_env_refs(s)),
            Some(_) => return Err(invalid("input.cwd: must be a string".into())),
        };
        let spec = ProcSpec {
            program: resolved.to_string_lossy().into_owned(),
            args: args.to_vec(),
            cwd,
            env: Vec::new(),
            secrets,
        };
        let limits = ProcLimits {
            timeout_ms: COORDINATED_TIMEOUT_MS,
            output_max_bytes: DEFAULT_OUTPUT_MAX_BYTES,
        };
        let r = self.supervisor.run(spec, limits, cancel).await;
        let mut out = ActionOutcome {
            ok: r.ok,
            ms: r.ms,
            pid: r.pid,
            exit_code: r.exit_code,
            timed_out: r.timed_out,
            canceled: r.canceled,
            error: r.error,
            chars: r.chars,
            output: r.output,
            meta: Map::new(),
        };
        if r.output_truncated {
            out.meta.insert("outputTruncated".into(), json!(true));
        }
        Ok(out)
    }
}

/// Register resolved ref values for masking — shared by the run coordinator when it wraps
/// action input it did not parse (a parent action's input may carry refs too). Currently
/// only the two process actions resolve refs; exposed for symmetry and tests.
pub fn mask_ref_values(text: &mut String, values: &[String]) {
    mask_secrets(text, values);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::action::CancelSource;

    fn echo_cmd() -> Value {
        if cfg!(windows) {
            json!({ "program": "cmd", "args": ["/c", "echo", "hi-from-exec"] })
        } else {
            json!({ "program": "sh", "args": ["-c", "echo hi-from-exec"] })
        }
    }

    #[tokio::test]
    async fn typed_exec_runs_and_captures_output() {
        let action = ProcessExecAction::new(Supervisor::new());
        let out = action.execute(&echo_cmd(), CancelHandle::never()).await.expect("runs");
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("hi-from-exec"), "{out:?}");
        assert_eq!(out.exit_code, Some(0));
    }

    #[tokio::test]
    async fn typed_input_is_validated_strictly() {
        let action = ProcessExecAction::new(Supervisor::new());
        for bad in [
            json!({}),                                       // no program
            json!({ "program": "" }),                        // empty program
            json!({ "program": "x", "wat": 1 }),             // unknown field
            json!({ "program": "x", "args": "not-array" }),  // wrong type
            json!({ "program": "x", "args": [1] }),          // non-string arg
            json!({ "program": "x", "env": { "A": 1 } }),    // non-string env value
            json!({ "program": "x", "cwd": "${LMG_EXEC_DEFINITELY_UNSET_3}" }), // missing required ref
        ] {
            let err = action.execute(&bad, CancelHandle::never()).await.unwrap_err();
            assert!(matches!(err, ActionError::InvalidInput(_)), "{bad} -> {err}");
        }
        let err = action
            .execute(&json!({ "program": "x", "cwd": "${LMG_EXEC_DEFINITELY_UNSET_3}" }), CancelHandle::never())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("LMG_EXEC_DEFINITELY_UNSET_3"), "names the variable: {err}");
    }

    #[tokio::test]
    async fn an_arg_with_spaces_stays_one_argument() {
        // The substituted value contains a space; substitution must NOT re-tokenize it
        // (docs/10 §6). Asserted on the argv the action builds, because no child reports
        // that portably: `cmd /c echo a b` prints the same line whether it was handed one
        // argument or two.
        unsafe { std::env::set_var("LMG_EXEC_SPACED", "two words") };
        let spec = parse_exec_input(&json!({
            "program": "cmd",
            "args": ["/c", "${LMG_EXEC_SPACED}"],
        }))
        .expect("valid input");
        assert_eq!(spec.args, vec!["/c".to_string(), "two words".to_string()]);
        // The spawn below uses a SHORT value on purpose: a resolved reference of 8+
        // characters is treated as a credential and masked out of captured output (the
        // next test), which would hide exactly what this assertion reads.
        unsafe { std::env::set_var("LMG_EXEC_SHORT", "a b") };
        let action = ProcessExecAction::new(Supervisor::new());
        // POSIX: hand the substituted value as one positional parameter and print it.
        let input = if cfg!(windows) {
            json!({ "program": "cmd", "args": ["/c", "echo", "${LMG_EXEC_SHORT}"] })
        } else {
            json!({ "program": "sh", "args": ["-c", "printf '%s\\n' \"$1\"", "sh", "${LMG_EXEC_SHORT}"] })
        };
        let out = action.execute(&input, CancelHandle::never()).await.expect("runs");
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("a b"), "{out:?}");
    }

    #[tokio::test]
    async fn a_resolved_reference_is_masked_out_of_captured_output() {
        // A child that echoes a credential it was handed must not write it into run
        // history (docs/10 §6): every value a reference resolved to is masked on the way
        // out. The 8-character floor keeps ordinary short values (a port, a count) that
        // happen to appear in the text from shredding the whole output.
        unsafe { std::env::set_var("LMG_EXEC_SECRET", "s3cr3t-value") };
        let action = ProcessExecAction::new(Supervisor::new());
        let input = if cfg!(windows) {
            json!({ "program": "cmd", "args": ["/c", "echo", "${LMG_EXEC_SECRET}"] })
        } else {
            json!({ "program": "sh", "args": ["-c", "printf '%s\n' \"$1\"", "sh", "${LMG_EXEC_SECRET}"] })
        };
        let out = action.execute(&input, CancelHandle::never()).await.expect("runs");
        assert!(out.ok, "{out:?}");
        assert!(!out.output.contains("s3cr3t-value"), "the secret leaked: {out:?}");
        assert!(out.output.contains(crate::mask::MASK), "{out:?}");
    }

    #[tokio::test]
    async fn legacy_command_tokenizes_like_the_old_runner() {
        // Short on purpose: an 8+ character resolved reference is masked out of the
        // captured output (see the masking test).
        unsafe { std::env::set_var("LMG_LEGACY_WORD", "exp") };
        let action = LegacyCommandAction::new(Supervisor::new());
        let input = if cfg!(windows) {
            json!({ "command": "cmd /c echo ${LMG_LEGACY_WORD}" })
        } else {
            json!({ "command": "sh -c 'echo ${LMG_LEGACY_WORD}'" })
        };
        let out = action.execute(&input, CancelHandle::never()).await.expect("runs");
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("exp"), "{out:?}");
        // Unknown fields rejected, empty command rejected.
        assert!(action.execute(&json!({ "command": "x", "extra": 1 }), CancelHandle::never()).await.is_err());
        assert!(action.execute(&json!({ "command": "   " }), CancelHandle::never()).await.is_err());
    }

    /// Both halves of "a failure is a RECORD, not an exception" — the property the jobs
    /// runner used to own before execution moved into this shared capability.
    #[tokio::test]
    async fn a_failing_legacy_command_is_an_outcome_not_an_error() {
        let action = LegacyCommandAction::new(Supervisor::new());

        // A non-zero exit RAN: not ok, the code is kept, and there is no spawn error.
        let cmd = if cfg!(windows) { "cmd /c exit 3" } else { "sh -c 'exit 3'" };
        let out = action
            .execute(&json!({ "command": cmd }), CancelHandle::never())
            .await
            .expect("ran");
        assert!(!out.ok, "{out:?}");
        assert_eq!(out.exit_code, Some(3));
        assert!(out.error.is_none(), "the command ran; it just failed: {out:?}");

        // A program that cannot be spawned is reported BY NAME, so the job log says what was
        // missing instead of "failed".
        let out = action
            .execute(
                &json!({ "command": "definitely-not-a-real-program-xyz --flag" }),
                CancelHandle::never(),
            )
            .await
            .expect("reported, not raised");
        assert!(!out.ok, "{out:?}");
        assert!(
            out.error
                .as_deref()
                .unwrap_or_default()
                .contains("definitely-not-a-real-program-xyz"),
            "{out:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_stops_a_typed_run_and_reports_it() {
        let action = ProcessExecAction::new(Supervisor::new());
        let src = CancelSource::new();
        let cmd = if cfg!(windows) {
            json!({ "program": "ping", "args": ["-n", "30", "127.0.0.1"] })
        } else {
            json!({ "program": "sleep", "args": ["30"] })
        };
        // The cancel lands WHILE the child runs: the run takes its handle first, the test
        // cancels from the outside a moment later.
        let handle = src.handle();
        let started = std::time::Instant::now();
        let task = tokio::spawn(async move { action.execute(&cmd, handle).await.expect("outcome") });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        src.cancel();
        let out = task.await.expect("task");
        assert!(out.canceled, "{out:?}");
        assert!(!out.ok);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "must not wait out the child");
    }

    #[tokio::test]
    async fn a_cancel_that_lands_before_the_run_takes_its_handle_is_not_lost() {
        // The coordinator can cancel between submitting a run and the run subscribing; the
        // flag must still read as set when the handle arrives, or the child runs to
        // completion under a cancel nobody ever sees.
        let action = ProcessExecAction::new(Supervisor::new());
        let src = CancelSource::new();
        src.cancel();
        let cmd = if cfg!(windows) {
            json!({ "program": "ping", "args": ["-n", "30", "127.0.0.1"] })
        } else {
            json!({ "program": "sleep", "args": ["30"] })
        };
        let started = std::time::Instant::now();
        let out = action.execute(&cmd, src.handle()).await.expect("outcome");
        assert!(out.canceled, "{out:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{out:?}");
    }

    #[test]
    fn ref_names_scan_in_order_and_deduplicate() {
        assert_eq!(
            ref_names("a-${A_B}-${C}-${A_B}-end"),
            vec!["A_B".to_string(), "C".to_string()]
        );
        assert!(ref_names("no refs").is_empty());
    }
}
