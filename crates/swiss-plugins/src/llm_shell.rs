// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

//! Interactive Test's real shell capability: PowerShell on Windows, Bash elsewhere.
//! Memory's automatic readers retain their separate read-only Git implementation. The
//! host owns admission, bounded output, deadlines, child trees and cancellation; no
//! process or worker starts at boot.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use serde_json::{json, Value};
use swiss_host::services::action::{Action, ActionContext, ActionError, ActionOutcome, CancelHandle};
use swiss_host::services::process::{ProcLimits, ProcSpec, Supervisor};
use swiss_host::services::runs::{RunCoordinator, RunView, SubmitRequest};
use swiss_host::services::RuntimeServices;
use swiss_llm::agent::{ToolAnswer, ToolSpec};
use swiss_llm::history::CancelFlag;
use crate::llm_secrets::{prepare, Dialect};

pub(crate) const ACTION: &str = "llm.test.shell";
const OUTPUT_MAX: usize = 64 * 1024;
const TIMEOUT_DEFAULT: u64 = 120_000;
const TIMEOUT_MAX: u64 = 7_200_000;

pub(crate) fn register(services: &RuntimeServices) -> Result<(), String> {
    services.actions.register(Arc::new(ShellAction { supervisor: services.supervisor.clone() }))
}

struct ShellAction { supervisor: Arc<Supervisor> }

/// The shell tool's name as the model sees it. The selection key stays `bash` (agent
/// config, the panel's picker, a Test's `tools` list); the model is told the shell it
/// really gets, so it writes that shell's syntax.
pub(crate) const TOOL: &str = if cfg!(windows) { "powershell" } else { "bash" };
pub(crate) const SHELL: &str = if cfg!(windows) { "PowerShell" } else { "Bash" };

/// The selection key a model-facing name stands for; `bash` is no name on Windows.
fn key(name: &str) -> Option<&'static str> {
    match name {
        "job_output" => Some("job_output"),
        "job_kill" => Some("job_kill"),
        n if n == TOOL => Some("bash"),
        _ => None,
    }
}

/// The program the shell tool runs and, for PowerShell, whether it is 7+ (pwsh, whose
/// `&&` and `||` chain commands) or the built-in Windows PowerShell 5.1 (which has neither).
struct Shell { program: String, pwsh: bool }

fn shell_program() -> Result<Shell, String> {
    #[cfg(windows)]
    {
        // PowerShell, never Git Bash or the System32 bash launcher that boots WSL: every
        // Windows carries Windows PowerShell, so the tool works without anything installed.
        let candidates = swiss_core::platform::pty::shell_candidates();
        let named = |exe: &str| candidates.iter()
            .find(|s| Path::new(&s.program).file_name().is_some_and(|n| n.eq_ignore_ascii_case(exe)))
            .map(|s| s.program.clone());
        if let Some(program) = named("pwsh.exe") { return Ok(Shell { program, pwsh: true }); }
        if let Some(program) = named("powershell.exe") { return Ok(Shell { program, pwsh: false }); }
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let builtin = Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        if builtin.is_file() { return Ok(Shell { program: builtin.to_string_lossy().into_owned(), pwsh: false }); }
        Err("PowerShell was not found; install PowerShell 7 or restore Windows PowerShell".into())
    }
    #[cfg(not(windows))]
    {
        let path = std::env::var("PATH").unwrap_or_default();
        swiss_host::pathenv::resolve_command("bash", &path).map(|p| Shell { program: p.to_string_lossy().into_owned(), pwsh: false })
            .ok_or("Bash was not found on PATH".into())
    }
}

/// The shell the tool runs, as a conversation's environment names it.
pub(crate) fn shell_named() -> String {
    match shell_program() {
        Ok(Shell { pwsh: true, .. }) => "PowerShell 7 (pwsh)".into(),
        Ok(_) if cfg!(windows) => "Windows PowerShell 5.1".into(),
        Ok(_) => "Bash".into(),
        Err(_) => format!("{SHELL} (not found)"),
    }
}

/// Runs ahead of a PowerShell command: UTF-8 out, so the supervisor reads Chinese file
/// names and git's UTF-8 as written (5.1 without a console may refuse the console
/// encoding; the pipe encoding still holds), and no progress bars or ANSI colour in
/// captured text.
const PS_PRELUDE: &str = "try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false) } catch {}\n\
$OutputEncoding = [Text.UTF8Encoding]::new($false)\n\
$ProgressPreference = 'SilentlyContinue'\n\
if ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }";
/// Runs after it: a failed last command exits with the native exit code, as Bash's `-c`
/// does, where PowerShell would report 1 (or 0 for a failed native command).
const PS_TRAILER: &str = "if (-not $?) { if ($LASTEXITCODE) { exit $LASTEXITCODE }; exit 1 }";

/// The arguments that run `command`. No -EncodedCommand or -ExecutionPolicy: antivirus
/// heuristics flag both, and an inline -Command is not subject to execution policy.
fn shell_args(command: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), format!("{PS_PRELUDE}\n{command}\n{PS_TRAILER}")]
    } else {
        vec!["--noprofile".into(), "--norc".into(), "-c".into(), command.into()]
    }
}

/// What the model is told the shell tool does: on Windows, the edition and the PowerShell
/// idioms that replace the Bash habits a model brings.
fn describe(root: &Path, granted: bool) -> String {
    let mut tail = format!("Default working directory: {}. Use workdir for another directory. Background jobs return a job_id: read with job_output and stop with job_kill. The execution deadline applies to foreground and background commands; it kills their process tree.", crate::llm_tools::plain_path(root));
    if granted {
        tail.push_str(" A secret this conversation named is written ${secret://name} where the shell expands a variable (unquoted or inside double quotes): its value reaches the command through the environment, never the command line, and output masks it. Inside single quotes the reference stays text, so a file can keep the reference itself.");
    }
    if !cfg!(windows) { return format!("Execute a command in a fresh Bash shell, including pipelines and general programs. {tail}"); }
    let chain = match shell_program() {
        Ok(Shell { pwsh: true, .. }) => "PowerShell 7 (pwsh) session on Windows. Write PowerShell, not Bash: `;` separates statements and `&&` / `||` chain commands",
        _ => "Windows PowerShell 5.1 session on Windows. Write PowerShell, not Bash: `;` separates statements and there is no `&&` or `||` (write `a; if ($?) { b }`)",
    };
    format!("Execute a command in a fresh {chain}; `$env:NAME` reads an environment variable; Get-ChildItem, Select-String, Get-Content -TotalCount N and New-Item stand in for ls, grep, head and touch. Native programs (git, cargo, node) run as usual; if execution policy refuses a tool's .ps1, call its .cmd (npm.cmd). {tail}")
}

/// The parser the shell tool's command meets, for its vault references.
const DIALECT: Dialect = if cfg!(windows) { Dialect::PowerShell } else { Dialect::Bash };

fn shell_spec(input: &Value) -> Result<ProcSpec, ActionError> {
    let command = input.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty())
        .ok_or_else(|| ActionError::InvalidInput("command must be a non-empty string".into()))?;
    let cwd = input.get("cwd").and_then(Value::as_str)
        .ok_or_else(|| ActionError::InvalidInput("cwd must name a directory".into()))?;
    let root = Path::new(cwd).canonicalize().map_err(|e| ActionError::InvalidInput(format!("cwd: {e}")))?;
    if !root.is_dir() { return Err(ActionError::InvalidInput("cwd must name a directory".into())); }
    let grant: Vec<String> = match input.get("secrets") {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone()).map_err(|_| ActionError::InvalidInput("secrets must be a list of names".into()))?,
    };
    // Resolved here, at the run itself: the values live only in this child's environment,
    // never in the run's stored input, and its output masks them.
    let prepared = prepare(command, DIALECT, &grant).map_err(ActionError::InvalidInput)?;
    let shell = shell_program().map_err(ActionError::InvalidInput)?;
    let mut spec = ProcSpec::simple(shell.program, shell_args(&prepared.command));
    spec.cwd = Some(root.to_string_lossy().into_owned());
    spec.env = prepared.env;
    spec.secrets = prepared.masked;
    Ok(spec)
}

fn outcome(result: swiss_host::services::process::ProcResult, spec: &ProcSpec) -> ActionOutcome {
    let mut meta = serde_json::Map::new();
    meta.insert("cwd".into(), json!(spec.cwd));
    meta.insert("shell".into(), json!(spec.program));
    meta.insert("outputTruncated".into(), json!(result.output_truncated));
    ActionOutcome { ok: result.ok, ms: result.ms, pid: result.pid, exit_code: result.exit_code,
        timed_out: result.timed_out, canceled: result.canceled, error: result.error,
        output: result.output, chars: result.chars, meta }
}

#[async_trait]
impl Action for ShellAction {
    fn type_name(&self) -> &'static str { ACTION }
    fn provider(&self) -> &'static str { "llm" }
    fn title(&self) -> String { format!("Execute an interactive agent's {SHELL} command") }
    fn schema(&self) -> Value { json!({"type":"object","required":["command","cwd"],"properties":{"command":{"type":"string"},"cwd":{"type":"string"},"secrets":{"type":"array","items":{"type":"string"}}}}) }
    async fn execute(&self, input: &Value, cancel: CancelHandle) -> Result<ActionOutcome, ActionError> {
        let spec = shell_spec(input)?;
        let result = self.supervisor.run(spec.clone(), ProcLimits { timeout_ms: u64::MAX, output_max_bytes: OUTPUT_MAX }, cancel).await;
        Ok(outcome(result, &spec))
    }
    async fn execute_with_context(&self, input: &Value, ctx: ActionContext) -> Result<ActionOutcome, ActionError> {
        let mut spec = shell_spec(input)?;
        spec.child = Some(ctx.child);
        let result = self.supervisor.run_streamed(spec.clone(), ProcLimits { timeout_ms: u64::MAX, output_max_bytes: OUTPUT_MAX }, ctx.cancel, ctx.output).await;
        Ok(outcome(result, &spec))
    }
}

pub(crate) fn scope(agent: &str, run: Option<i64>) -> String {
    format!("test:{agent}:run:{}", run.map(|id| id.to_string()).unwrap_or_else(|| format!("stateless:{}", swiss_core::util::random_hex(8))))
}

pub(crate) async fn cancel_conversation(runs: &Arc<RunCoordinator>, id: i64) -> usize {
    let suffix = format!(":run:{id}");
    let ids: Vec<u64> = runs.list().into_iter().filter(|v| v.owner == "llm" && v.action_type == ACTION && v.label.ends_with(&suffix) && !v.state.is_terminal()).map(|v| v.run_id).collect();
    for id in &ids { runs.cancel(*id).await; }
    ids.len()
}

/// The execution and inspection paths share the same model-facing definitions.
pub(crate) fn specs(root: &Path, taken: &[&'static str], granted: bool) -> Vec<ToolSpec> {
    vec![
        ToolSpec { name: TOOL.into(), description: describe(root, granted),
            schema: json!({"type":"object","additionalProperties":false,"required":["command"],"properties":{
                "command":{"type":"string"},"description":{"type":"string","description":"A short explanation of this command"},
                "workdir":{"type":"string","description":"Absolute directory, or relative to the test working directory"},
                "cwd":{"type":"string","description":"Alias for workdir; supply only one"},
                "timeoutMs":{"type":"integer","minimum":1,"maximum":TIMEOUT_MAX,"default":TIMEOUT_DEFAULT},
                "run_in_background":{"type":"boolean","default":false}
            }}) },
        ToolSpec { name: "job_output".into(), description: format!("Read a {SHELL} job's state and bounded output from cursor. timeoutMs waits up to 30 seconds for new output or completion. Jobs belong to this conversation."),
            schema: json!({"type":"object","additionalProperties":false,"required":["job_id"],"properties":{"job_id":{"type":"integer","minimum":0},"cursor":{"type":"integer","minimum":0},"timeoutMs":{"type":"integer","minimum":0,"maximum":30000,"default":0}}}) },
        ToolSpec { name: "job_kill".into(), description: format!("Cancel a {SHELL} job and its process tree, then return its final state and output."),
            schema: json!({"type":"object","additionalProperties":false,"required":["job_id"],"properties":{"job_id":{"type":"integer","minimum":0}}}) },
    ].into_iter().filter(|s| key(&s.name).is_some_and(|k| taken.contains(&k))).collect()
}

pub(crate) struct ShellTools {
    root: PathBuf,
    taken: Vec<&'static str>,
    runs: Arc<RunCoordinator>,
    label: String,
    cancel: Option<Arc<CancelFlag>>,
    /// The agent's `secrets`: the vault names its commands may reference (SPEC §llm.agents).
    grant: Vec<String>,
}

impl ShellTools {
    pub(crate) fn new(root: &Path, taken: &[&'static str], services: &RuntimeServices, label: String, cancel: Option<Arc<CancelFlag>>) -> Self {
        Self { root: root.to_path_buf(), taken: taken.to_vec(), runs: services.runs.clone(), label, cancel, grant: Vec::new() }
    }

    pub(crate) fn granting(mut self, grant: &[String]) -> Self {
        self.grant = grant.to_vec();
        self
    }

    pub(crate) fn specs(&self) -> Vec<ToolSpec> {
        specs(&self.root, &self.taken, !self.grant.is_empty())
    }

    fn owned(&self, id: u64) -> Result<RunView, String> {
        self.runs.get(id).filter(|v| v.owner == "llm" && v.label == self.label && v.action_type == ACTION)
            .ok_or_else(|| format!("job {id} is not available in this conversation"))
    }

    fn snapshot(&self, id: u64, cursor: u64) -> Result<Value, String> {
        let view = self.owned(id)?;
        let mut value = view.to_json(false);
        value["job_id"] = json!(id);
        if let Some((_, chunk)) = self.runs.output(id, cursor, OUTPUT_MAX) {
            value["output"] = json!(swiss_memory::secrets::redact(&chunk.text));
            value["next_cursor"] = json!(chunk.next_cursor);
            value["truncated"] = json!(chunk.truncated);
        }
        Ok(value)
    }

    /// Runs one tool by its selection key: `bash` for the shell, whatever its model-facing name.
    async fn dispatch(&self, name: &str, args: &Value) -> Result<Value, String> {
        let fields: &[&str] = match name { "bash" => &["command","description","workdir","cwd","timeoutMs","run_in_background"], "job_output" => &["job_id","cursor","timeoutMs"], "job_kill" => &["job_id"], _ => return Err(format!("unknown tool {name}")) };
        let object = args.as_object().ok_or("arguments must be an object")?;
        if let Some(key) = object.keys().find(|k| !fields.contains(&k.as_str())) { return Err(format!("unknown argument {key}")); }
        if name == "bash" {
            let command = args.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty()).ok_or("command must be a non-empty string")?;
            if args.get("description").is_some_and(|v| !v.is_string()) { return Err("description must be a string".into()); }
            if args.get("cwd").is_some() && args.get("workdir").is_some() { return Err("supply workdir or cwd, not both".into()); }
            let dir = match args.get("workdir").or_else(|| args.get("cwd")) {
                Some(value) => {
                    let value = value.as_str().filter(|s| !s.is_empty()).ok_or("workdir must be a directory string")?;
                    if Path::new(value).is_absolute() { PathBuf::from(value) } else { self.root.join(value) }
                }
                None => self.root.clone(),
            }.canonicalize().map_err(|e| format!("workdir: {e}"))?;
            if !dir.is_dir() { return Err("workdir must name a directory".into()); }
            let timeout = number(args, "timeoutMs", TIMEOUT_DEFAULT, 1, TIMEOUT_MAX)?;
            let background = match args.get("run_in_background") { None => false, Some(v) => v.as_bool().ok_or("run_in_background must be a boolean")? };
            if background && !self.taken.contains(&"job_output") { return Err("select job_output to read a background job".into()); }
            // Refuse an ungranted or missing reference before a job exists; the action
            // resolves again at the run, so no value is held here.
            prepare(command, DIALECT, &self.grant)?;
            let mut input = json!({"command":command,"cwd":dir.to_string_lossy()});
            if !self.grant.is_empty() { input["secrets"] = json!(self.grant); }
            let submitted = self.runs.submit(SubmitRequest { owner: "llm".into(), label: self.label.clone(), actor: "llm.test".into(), action_type: ACTION.into(),
                input, timeout_ms: timeout, queue_if_busy: false }).map_err(|e| e.to_string())?;
            let id = submitted.run_id;
            if background { return self.snapshot(id, 0); }
            let mut guard = Foreground { runs: self.runs.clone(), id, armed: true };
            if let Some(cancel) = &self.cancel {
                tokio::select! {
                    _ = cancel.wait() => { self.runs.cancel(id).await; }
                    result = submitted.done => { result.map_err(|_| "the shell job ended without an outcome")?; }
                }
            } else { submitted.done.await.map_err(|_| "the shell job ended without an outcome")?; }
            guard.armed = false;
            return self.snapshot(id, 0);
        }
        let id = args.get("job_id").and_then(Value::as_u64).ok_or("job_id must be a non-negative integer")?;
        self.owned(id)?;
        if name == "job_kill" { self.runs.cancel(id).await; return self.snapshot(id, 0); }
        let cursor = number(args, "cursor", 0, 0, u64::MAX)?;
        let wait = number(args, "timeoutMs", 0, 0, 30_000)?;
        let until = tokio::time::Instant::now() + Duration::from_millis(wait);
        loop {
            let value = self.snapshot(id, cursor)?;
            if self.owned(id)?.state.is_terminal() || value.get("output").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) || tokio::time::Instant::now() >= until { return Ok(value); }
            tokio::time::sleep_until(until.min(tokio::time::Instant::now() + Duration::from_millis(100))).await;
        }
    }

    pub(crate) async fn call(&self, name: &str, args: &Value) -> ToolAnswer {
        let Some(key) = key(name) else { return ToolAnswer { ok: false, text: format!("error: unknown tool {name}") } };
        if !self.taken.contains(&key) { return ToolAnswer { ok: false, text: format!("error: tool {name} was not selected") }; }
        match self.dispatch(key, args).await {
            Ok(value) => {
                let failed = key != "job_kill" && value.get("state").and_then(Value::as_str).is_some_and(|s| matches!(s, "failed" | "timeout" | "canceled"));
                ToolAnswer { ok: !failed, text: value.to_string() }
            }
            Err(why) => ToolAnswer { ok: false, text: format!("error: {why}") },
        }
    }
}

fn number(args: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    let value = match args.get(key) { Some(value) => value.as_u64().ok_or_else(|| format!("{key} must be an integer"))?, None => default };
    if value < min || value > max { return Err(format!("{key} must be between {min} and {max}")); }
    Ok(value)
}

struct Foreground { runs: Arc<RunCoordinator>, id: u64, armed: bool }
impl Drop for Foreground {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let runs = self.runs.clone(); let id = self.id;
                runtime.spawn(async move { runs.cancel(id).await; });
            }
        }
    }
}

impl Drop for ShellTools {
    fn drop(&mut self) {
        // Store-less tests cannot resume a job after this request. Persisted conversations
        // keep theirs across sends until Stop, New, close or plugin shutdown.
        if self.label.contains(":run:stateless:") {
            let ids: Vec<u64> = self.runs.list().into_iter().filter(|v| v.label == self.label && v.action_type == ACTION && !v.state.is_terminal()).map(|v| v.run_id).collect();
            if !ids.is_empty() {
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    let runs = self.runs.clone();
                    runtime.spawn(async move { for id in ids { runs.cancel(id).await; } });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_host::services::BootInfo;

    fn root() -> PathBuf {
        let path = swiss_core::paths::test_home().join(format!("llm-shell-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(path.join("repo with spaces")).expect("directory");
        path
    }

    fn tools(root: &Path, services: &RuntimeServices, run: i64, cancel: Option<Arc<CancelFlag>>) -> ShellTools {
        ShellTools::new(root, &["bash", "job_output", "job_kill"], services, scope("a", Some(run)), cancel)
    }

    /// Plant a vault value under a test-unique name; the vault is process-wide.
    async fn plant(name: &'static str, value: &'static str) {
        tokio::task::spawn_blocking(move || {
            let _guard = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
            let path = swiss_core::paths::test_home().join("secrets.json");
            swiss_core::secure::secretstore::inject_vault(&path);
            let rev = swiss_core::secure::secretstore::vault_rev();
            swiss_core::secure::secretstore::put_secret(&path, name, value, rev).expect("plant");
        }).await.expect("planted");
    }

    /// The value arrives (rewritten, so the proof is not itself masked), stdout and stderr
    /// mask it, the run record never holds it, a single-quoted reference is written as the
    /// reference, and an agent without the grant is refused before a job exists.
    async fn a_granted_reference_runs_through_the_environment(name: &'static str, value: &'static str, command: &str) {
        plant(name, value).await;
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        register(&services).expect("action");
        let shell = tools(&root, &services, 1, None).granting(&[name.to_string()]);
        assert!(shell.specs()[0].description.contains("${secret://name}"));
        assert!(!tools(&root, &services, 1, None).specs()[0].description.contains("secret://"));
        let reply = shell.call(TOOL, &json!({"command":command,"timeoutMs":60000})).await;
        assert!(reply.ok, "{}", reply.text);
        let output = serde_json::from_str::<Value>(&reply.text).expect("result")["output"].as_str().expect("output").to_string();
        let mask = swiss_core::secure::literal::MASK;
        assert!(output.contains(&value.replace("value", "seen")), "{output}");
        assert!(output.contains(&format!("said {mask}")) && output.contains(&format!("err {mask}")), "{output}");
        let stored = services.runs.list().into_iter().map(|v| v.to_json(true).to_string()).collect::<String>();
        assert!(!reply.text.contains(value) && !stored.contains(value), "{stored}");
        assert_eq!(std::fs::read_to_string(root.join("ref.txt")).expect("file"), format!("${{secret://{name}}}"));
        let before = services.runs.list().len();
        let refused = tools(&root, &services, 2, None).call(TOOL, &json!({"command":format!("echo \"${{secret://{name}}}\"")})).await;
        assert!(!refused.ok && refused.text.contains("is not granted to this agent"), "{}", refused.text);
        assert_eq!(services.runs.list().len(), before, "a refused command starts no job");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_granted_reference_reaches_powershell_through_the_environment_and_never_its_output() {
        a_granted_reference_runs_through_the_environment("shell-ps-key", "shell-ps-value-0042",
            "(\"${secret://shell-ps-key}\").Replace('value', 'seen'); Write-Output \"said ${secret://shell-ps-key}\"; \
             [Console]::Error.WriteLine(\"err ${secret://shell-ps-key}\"); Set-Content -NoNewline -Path ref.txt -Value '${secret://shell-ps-key}'").await;
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_granted_reference_reaches_bash_through_the_environment_and_never_its_output() {
        a_granted_reference_runs_through_the_environment("shell-sh-key", "shell-sh-value-0042",
            "v=\"${secret://shell-sh-key}\"; echo \"${v/value/seen}\"; echo \"said ${secret://shell-sh-key}\"; \
             echo \"err ${secret://shell-sh-key}\" >&2; printf '%s' '${secret://shell-sh-key}' > ref.txt").await;
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_real_bash_runs_pipelines_writes_and_git_in_a_selected_subdirectory() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        register(&services).expect("action");
        let shell = tools(&root, &services, 1, None);
        let command = "printf 'hello shell' | tr a-z A-Z; printf 'saved' > proof.txt; git init -q; git -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -qm shell-proof; git log --oneline -1";
        let reply = shell.call(TOOL, &json!({"command":command,"workdir":"repo with spaces","timeoutMs":30000})).await;
        assert!(reply.ok, "{}", reply.text);
        let reply: Value = serde_json::from_str(&reply.text).expect("result");
        assert_eq!(reply["exitCode"], 0);
        assert!(reply["output"].as_str().expect("output").contains("HELLO SHELL"));
        assert!(reply["output"].as_str().expect("output").contains("shell-proof"));
        assert_eq!(std::fs::read_to_string(root.join("repo with spaces/proof.txt")).expect("written file"), "saved");
        let nested = shell.call(TOOL, &json!({"command":"git -C 'repo with spaces' log --oneline -1"})).await;
        assert!(nested.ok && nested.text.contains("shell-proof"), "{}", nested.text);
        for args in [json!({"command":"echo no","workdir":"missing"}), json!({"command":"echo no","workdir":".","cwd":"."}),
                     json!({"command":"echo no","timeoutMs":0}), json!({"command":"echo no","run_in_background":"yes"})] {
            assert!(!shell.call(TOOL, &args).await.ok, "{args}");
        }
        assert!(!shell.call(TOOL, &json!({"command":"exit 7"})).await.ok);
        assert_eq!(services.runs.count_for_label(&scope("a", Some(1))), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_real_powershell_runs_pipelines_writes_and_git_in_a_selected_subdirectory() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        register(&services).expect("action");
        let shell = tools(&root, &services, 1, None);
        assert_eq!(shell.specs().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["powershell", "job_output", "job_kill"]);
        assert!(shell.specs()[0].description.contains("Write PowerShell, not Bash"));
        assert!(!shell.call("bash", &json!({"command":"echo no"})).await.ok, "bash is no tool name on Windows");
        let command = "'hello shell'.ToUpper() | Write-Output; 'saved' | Out-File -NoNewline -Encoding ascii proof.txt; New-Item -ItemType Directory 目录 | Out-Null; (Get-ChildItem -Directory).Name; git init -q; git -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -qm 中文提交; git log --format=%s -1";
        let reply = shell.call(TOOL, &json!({"command":command,"workdir":"repo with spaces","timeoutMs":60000})).await;
        assert!(reply.ok, "{}", reply.text);
        let reply: Value = serde_json::from_str(&reply.text).expect("result");
        assert_eq!(reply["exitCode"], 0);
        let output = reply["output"].as_str().expect("output");
        for seen in ["HELLO SHELL", "目录", "中文提交"] { assert!(output.contains(seen), "{seen} in {output}"); }
        assert_eq!(std::fs::read_to_string(root.join("repo with spaces/proof.txt")).expect("written file"), "saved");
        let nested = shell.call(TOOL, &json!({"command":"git -C 'repo with spaces' log --format=%s -1"})).await;
        assert!(nested.ok && nested.text.contains("中文提交"), "{}", nested.text);
        let failed: Value = serde_json::from_str(&shell.call(TOOL, &json!({"command":"cmd /c exit 7"})).await.text).expect("result");
        assert_eq!((failed["state"].as_str(), failed["exitCode"].as_i64()), (Some("failed"), Some(7)), "{failed}");
        assert_eq!(services.runs.count_for_label(&scope("a", Some(1))), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Both editions a Windows may carry, driven straight through the supervisor (the
    /// daemon's own console-less spawn): UTF-8 out, quotes and lines intact, Bash-like
    /// exit codes, no ANSI colour in an error.
    #[cfg(windows)]
    #[tokio::test]
    async fn either_powershell_edition_prints_utf8_and_hands_back_exit_codes() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let mut programs = vec![Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe").to_string_lossy().into_owned()];
        if let Ok(Shell { program, pwsh: true }) = shell_program() { programs.push(program); }
        for program in &programs {
            let run = |command: &str| {
                let mut spec = ProcSpec::simple(program.clone(), shell_args(command));
                spec.cwd = Some(root.to_string_lossy().into_owned());
                services.supervisor.run(spec, ProcLimits { timeout_ms: 60_000, output_max_bytes: OUTPUT_MAX }, CancelHandle::never())
            };
            // PowerShell's own text and a native program's UTF-8 in one capture: either
            // one mis-encoded and the whole capture stops being UTF-8.
            let said = run("Write-Output '中文输出'; $v = \"quoted\"; Write-Output \"a $v word\"\nWrite-Output 'second line'\n\
                git init -q; git -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -qm 中文提交; git log --format=%s -1").await;
            assert!(said.ok, "{program}: {said:?}");
            for seen in ["中文输出", "a quoted word", "second line", "中文提交"] { assert!(said.output.contains(seen), "{program}: {seen} in {}", said.output); }
            assert_eq!(run("cmd /c exit 7").await.exit_code, Some(7), "{program}");
            assert_eq!(run("Write-Output first; exit 3").await.exit_code, Some(3), "{program}");
            let missing = run("Get-Item no-such-file").await;
            assert_eq!(missing.exit_code, Some(1), "{program}: {missing:?}");
            assert!(!missing.output.contains('\u{1b}'), "{program}: {}", missing.output);
            assert_eq!(run("cmd /c exit 7; Write-Output after").await.exit_code, Some(0), "{program}: the last command decides");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A controlled pending action proves job ownership, deadlines and cancellation without
    /// sleeping or leaving a real child blocked. The real shell is exercised above and live.
    struct Pending;
    #[async_trait]
    impl Action for Pending {
        fn type_name(&self) -> &'static str { ACTION }
        fn schema(&self) -> Value { json!({}) }
        async fn execute(&self, _input: &Value, cancel: CancelHandle) -> Result<ActionOutcome, ActionError> {
            cancel.cancelled().await;
            Ok(ActionOutcome { ok:false, ms:1, pid:None, exit_code:None, timed_out:false, canceled:true,
                error:None, output:"stopped".into(), chars:7, meta:serde_json::Map::new() })
        }
        async fn execute_with_context(&self, input: &Value, ctx: ActionContext) -> Result<ActionOutcome, ActionError> {
            ctx.output.append("ready");
            self.execute(input, ctx.cancel).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn store_less_requests_have_distinct_scopes_and_release_their_jobs_on_drop() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        services.actions.register(Arc::new(Pending)).expect("action");
        let a = ShellTools::new(&root, &["bash", "job_output"], &services, scope("a", None), None);
        let b = ShellTools::new(&root, &["bash", "job_output"], &services, scope("a", None), None);
        let started = a.call(TOOL, &json!({"command":"pending","run_in_background":true})).await;
        let id = serde_json::from_str::<Value>(&started.text).expect("result")["job_id"].as_u64().expect("id");
        assert!(!b.call("job_output", &json!({"job_id":id})).await.ok);
        drop(a);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !services.runs.get(id).expect("job").state.is_terminal() { tokio::task::yield_now().await; }
        }).await.expect("drop releases the job");
        assert_eq!(services.runs.get(id).expect("job").state, swiss_host::services::runs::RunState::Canceled);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(start_paused = true)]
    async fn background_jobs_are_readable_scoped_cancellable_and_deadline_bound() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        services.actions.register(Arc::new(Pending)).expect("action");
        let shell = tools(&root, &services, 1, None);
        let started = shell.call(TOOL, &json!({"command":"pending","run_in_background":true,"timeoutMs":1000})).await;
        assert!(started.ok, "{}", started.text);
        let started: Value = serde_json::from_str(&started.text).expect("result");
        let id = started["job_id"].as_u64().expect("job id");
        tokio::task::yield_now().await;
        let read = shell.call("job_output", &json!({"job_id":id})).await;
        assert!(read.ok, "{}", read.text);
        let read: Value = serde_json::from_str(&read.text).expect("output");
        assert_eq!(read["output"], "ready");
        assert_eq!(read["state"], "running");
        let next = shell.call("job_output", &json!({"job_id":id,"cursor":read["next_cursor"]})).await;
        assert_eq!(serde_json::from_str::<Value>(&next.text).expect("cursor read")["output"], "");
        let other = tools(&root, &services, 2, None);
        assert!(!other.call("job_output", &json!({"job_id":id})).await.ok);
        assert!(!other.call("job_kill", &json!({"job_id":id})).await.ok);
        let killed = shell.call("job_kill", &json!({"job_id":id})).await;
        assert!(killed.ok, "{}", killed.text);
        assert_eq!(services.runs.get(id).expect("job").state.as_str(), "canceled");
        let second = shell.call(TOOL, &json!({"command":"pending","run_in_background":true,"timeoutMs":100})).await;
        let id = serde_json::from_str::<Value>(&second.text).expect("job")["job_id"].as_u64().expect("id");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(101)).await;
        for _ in 0..20 { tokio::task::yield_now().await; }
        assert_eq!(services.runs.get(id).expect("deadline result").state.as_str(), "timeout");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stop_or_a_dropped_foreground_call_releases_the_host_job() {
        let root = root();
        let services = RuntimeServices::new(BootInfo::default());
        services.actions.register(Arc::new(Pending)).expect("action");
        let flag = CancelFlag::new();
        let shell = tools(&root, &services, 1, Some(flag.clone()));
        let task = tokio::spawn(async move { shell.call(TOOL, &json!({"command":"pending"})).await });
        while services.runs.list().is_empty() { tokio::task::yield_now().await; }
        flag.request();
        assert!(!task.await.expect("foreground result").ok);
        assert_eq!(services.runs.count_for_label(&scope("a", Some(1))), 0);
        let shell = tools(&root, &services, 2, None);
        let task = tokio::spawn(async move { shell.call(TOOL, &json!({"command":"pending"})).await });
        while services.runs.count_for_label(&scope("a", Some(2))) == 0 { tokio::task::yield_now().await; }
        task.abort();
        let _ = task.await;
        for _ in 0..20 { tokio::task::yield_now().await; }
        assert_eq!(services.runs.count_for_label(&scope("a", Some(2))), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
