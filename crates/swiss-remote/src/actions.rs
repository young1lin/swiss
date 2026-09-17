/*
 * Copyright 2026 The swiss authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The three remote capabilities (docs/32): remote.exec, remote.sync, remote.pull.
//! One file because they share the resolve-stream-report skeleton, and none of them
//! is large enough alone to deserve a module of plumbing.
//!
//! Every action is a RUN through the shared coordinator: the input is validated here
//! (strictly - unknown fields refused, the convention everywhere), the target row is
//! resolved under the store lock, and the work itself runs lock-free against the
//! transport registry. Output streams into the run's live buffer as it arrives; the
//! OUTCOME carries a bounded tail plus counters, never the whole log.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use swiss_host::services::action::{
    Action, ActionContext, ActionError, ActionOutcome, CancelHandle, RunOutputSink,
};
use swiss_host::services::remote::{
    RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteTransportRegistry,
};

use crate::sync::{pull_one, sync_tree, SyncOptions};
use crate::target::{safe_join, RemoteTarget};
use crate::RemoteSystem;

/// The owner string remote runs are registered under (cancel scoping, like jobs').
pub const REMOTE_OWNER: &str = "remote";
/// How much output tail an OUTCOME keeps. The live buffer holds the streaming story;
/// the outcome is the summary a finished-run row shows.
const OUTCOME_TAIL_BYTES: usize = 64 * 1024;

/// The streaming accumulator every remote action shares: forwards to the live sink
/// when there is one, while always keeping a bounded tail and the total byte count.
struct Tap<'a> {
    sink: Option<&'a RunOutputSink>,
    tail: Vec<u8>,
    chars: usize,
}

impl<'a> Tap<'a> {
    fn live(sink: &'a RunOutputSink) -> Self {
        Tap {
            sink: Some(sink),
            tail: Vec::new(),
            chars: 0,
        }
    }

    fn quiet() -> Tap<'static> {
        Tap {
            sink: None,
            tail: Vec::new(),
            chars: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.chars += bytes.len();
        if let Some(sink) = self.sink {
            sink.append_bytes(bytes);
        }
        if self.tail.len() < OUTCOME_TAIL_BYTES {
            let room = OUTCOME_TAIL_BYTES - self.tail.len();
            let take = room.min(bytes.len());
            self.tail.extend_from_slice(&bytes[..take]);
        }
    }

    fn truncated(&self) -> bool {
        self.chars > self.tail.len()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.tail).into_owned()
    }
}

/// Map a transport refusal into the honest action error. The enable-tunnels hint
/// is the most useful sentence this plugin can say: the target is fine, the
/// transport seat is empty.
fn transport_error(err: RemoteError) -> ActionError {
    let hint = |msg: String| {
        ActionError::Failed(format!(
            "{msg}; enable and start the Tunnels plugin if it is off"
        ))
    };
    match err {
        RemoteError::Unavailable(m) | RemoteError::Withdrawing(m) => hint(m),
        other => ActionError::Failed(other.to_string()),
    }
}

fn text_field(obj: &Map<String, Value>, key: &str) -> Result<String, ActionError> {
    match obj.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(_) => Err(ActionError::InvalidInput(format!(
            "{key} must be a non-empty string"
        ))),
        None => Err(ActionError::InvalidInput(format!("{key} is required"))),
    }
}

fn opt_text_field(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ActionError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(ActionError::InvalidInput(format!(
            "{key} must be a non-empty string when present"
        ))),
    }
}

fn env_map(obj: &Map<String, Value>) -> Result<Vec<(String, String)>, ActionError> {
    let mut pairs = Vec::new();
    match obj.get("env") {
        None | Some(Value::Null) => return Ok(pairs),
        Some(Value::Object(map)) => {
            for (name, value) in map {
                if name.is_empty()
                    || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || name.starts_with(|c: char| c.is_ascii_digit())
                {
                    return Err(ActionError::InvalidInput(format!(
                        "env name {name:?} is not a shell identifier"
                    )));
                }
                let value = value.as_str().ok_or_else(|| {
                    ActionError::InvalidInput(format!("env.{name} must be a string"))
                })?;
                pairs.push((name.clone(), value.to_string()));
            }
        }
        Some(_) => return Err(ActionError::InvalidInput("env must be an object".into())),
    }
    // Deterministic order: the command string must not depend on map iteration.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs)
}

fn refuse_unknown(obj: &Map<String, Value>, known: &[&str], path: &str) -> Result<(), ActionError> {
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            return Err(ActionError::InvalidInput(format!(
                "{path}.{key} is not a known field"
            )));
        }
    }
    Ok(())
}

fn object_of(input: &Value) -> Result<Map<String, Value>, ActionError> {
    input
        .as_object()
        .cloned()
        .ok_or_else(|| ActionError::InvalidInput("input must be an object".into()))
}

// --- remote.exec --------------------------------------------------------------------------

/// Run one command on a target, streaming output (docs/32). THE action of this
/// plugin: argv arrives as an array and crosses the transport as argv - the quoting
/// into a POSIX command string happens once, inside the tunnels crate, through its
/// tested quote_posix. Nothing here ever builds a shell command itself.
pub struct RemoteExecAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemoteExecAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemoteExecAction { system, registry }
    }

    /// The parsed, validated input (also the validate_input surface).
    fn parse(&self, input: &Value) -> Result<ParsedExec, ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &EXEC_FIELDS, "input")?;
        let target = text_field(&obj, "target")?;
        let argv =
            match obj.get("argv") {
                Some(Value::Array(items)) if !items.is_empty() => {
                    let mut words = Vec::with_capacity(items.len());
                    for item in items {
                        words.push(item.as_str().map(str::to_string).ok_or_else(|| {
                            ActionError::InvalidInput("argv must be strings".into())
                        })?);
                    }
                    words
                }
                _ => {
                    return Err(ActionError::InvalidInput(
                        "argv must be a non-empty array of strings".into(),
                    ))
                }
            };
        // The workspace guardrail (docs/32): sudo escalates past everything the
        // target row promises, so it is refused at the door with the reason. An
        // agent that needs root configures it on the machine, not per command.
        if argv.first().is_some_and(|first| first == "sudo") {
            return Err(ActionError::InvalidInput(
                "argv[0] must not be sudo: remote targets run unprivileged; configure what needs root on the machine itself".into(),
            ));
        }
        let env = env_map(&obj)?;
        let cwd = opt_text_field(&obj, "cwd")?;
        if let Some(rel) = &cwd {
            if rel.starts_with('/') || rel.contains("..") {
                return Err(ActionError::InvalidInput(format!(
                    "cwd {rel:?} must be relative to the target's workspaceRoot and must not contain .."
                )));
            }
        }
        Ok(ParsedExec {
            target,
            argv,
            env,
            cwd,
        })
    }
}

/// How one exec ended: a real exit code, or a cancel with its reason.
enum ExecEnd {
    Done(i32),
    Canceled(String),
}

struct ParsedExec {
    target: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<String>,
}

const EXEC_FIELDS: [&str; 5] = ["target", "argv", "env", "cwd", "timeoutMs"];

/// Stream one exec to completion: spawn the transport call, forward events to the
/// tap, return the exit result. Shared by both entry points so the unstreaming
/// execute() cannot drift from execute_with_context().
async fn drive_exec<'a>(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    request: RemoteExecRequest,
    cancel: CancelHandle,
    mut tap: Tap<'a>,
) -> Result<(ExecEnd, Tap<'a>), ActionError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(swiss_host::services::remote::EXEC_EVENT_QUEUE);
    let endpoint = target.endpoint.clone();
    let holder = format!("{}:{}", REMOTE_OWNER, target.id);
    let registry = registry.clone();
    let exec =
        tokio::spawn(async move { registry.exec(&endpoint, &holder, request, tx, cancel).await });
    while let Some(event) = rx.recv().await {
        match event {
            RemoteExecEvent::Stdout(bytes) => tap.push(&bytes),
            RemoteExecEvent::Stderr(bytes) => tap.push(&bytes),
        }
    }
    let result = exec
        .await
        .map_err(|err| ActionError::Failed(format!("the transport task ended: {err}")))?;
    let end = match result {
        Ok(result) => ExecEnd::Done(result.exit_code),
        // A cancel is a completed run that was stopped on purpose, not a failure:
        // the coordinator reads outcome.canceled and marks the run Canceled.
        Err(RemoteError::Canceled(why)) => ExecEnd::Canceled(why),
        Err(err) => return Err(transport_error(err)),
    };
    Ok((end, tap))
}

fn exec_outcome(target: &RemoteTarget, end: ExecEnd, ms: u64, tap: &Tap<'_>) -> ActionOutcome {
    let mut meta = Map::new();
    meta.insert("target".into(), json!(target.id));
    meta.insert("endpoint".into(), json!(target.endpoint));
    if tap.truncated() {
        meta.insert("outputTruncated".into(), json!(true));
    }
    let (ok, exit_code, canceled, error) = match end {
        ExecEnd::Done(code) => (code == 0, Some(code), false, None),
        ExecEnd::Canceled(why) => (false, None, true, Some(why)),
    };
    ActionOutcome {
        ok,
        ms,
        pid: None,
        exit_code,
        timed_out: false,
        canceled,
        error,
        output: tap.text(),
        chars: tap.chars,
        meta,
    }
}

#[async_trait]
impl Action for RemoteExecAction {
    fn type_name(&self) -> &'static str {
        "remote.exec"
    }

    fn title(&self) -> String {
        "Run a command on a remote target".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target", "argv"],
            "properties": {
                "target": { "type": "string", "description": "Target id from 'swiss remote target list'." },
                "argv": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Program and arguments AS ARGV - never a shell string. Quoting happens inside the transport, once."
                },
                "env": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Extra environment for the command; names must be identifiers."
                },
                "cwd": { "type": "string", "description": "Working directory relative to the target's workspaceRoot." },
                "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 86400000, "description": "Run deadline; the submit path owns it." }
            },
            "additionalProperties": false
        })
    }

    fn validate_input(&self, input: &Value) -> Result<(), String> {
        self.parse(input).map(|_| ()).map_err(|err| err.to_string())
    }

    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let parsed = self.parse(input)?;
        let target = self
            .system
            .resolve(&parsed.target)
            .map_err(ActionError::InvalidInput)?;
        let cwd = parsed
            .cwd
            .as_deref()
            .map(|rel| safe_join(&target.workspace_root, rel))
            .transpose()
            .map_err(ActionError::InvalidInput)?;
        let request = RemoteExecRequest {
            argv: parsed.argv,
            env: parsed.env,
            cwd,
        };
        let (end, tap) = drive_exec(&self.registry, &target, request, cancel, Tap::quiet()).await?;
        Ok(exec_outcome(
            &target,
            end,
            started.elapsed().as_millis() as u64,
            &tap,
        ))
    }

    async fn execute_with_context(
        &self,
        input: &Value,
        ctx: ActionContext,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let parsed = self.parse(input)?;
        let target = self
            .system
            .resolve(&parsed.target)
            .map_err(ActionError::InvalidInput)?;
        let cwd = parsed
            .cwd
            .as_deref()
            .map(|rel| safe_join(&target.workspace_root, rel))
            .transpose()
            .map_err(ActionError::InvalidInput)?;
        let request = RemoteExecRequest {
            argv: parsed.argv,
            env: parsed.env,
            cwd,
        };
        let (end, tap) = drive_exec(
            &self.registry,
            &target,
            request,
            ctx.cancel.clone(),
            Tap::live(&ctx.output),
        )
        .await?;
        Ok(exec_outcome(
            &target,
            end,
            started.elapsed().as_millis() as u64,
            &tap,
        ))
    }
}

// --- remote.sync -------------------------------------------------------------------------

const SYNC_FIELDS: [&str; 4] = ["target", "source", "exclude", "verbose"];

/// Upload a local tree to the target workspace (docs/32 §20). One-way, no deletes:
/// changed files (by size) stream up; up-to-date files are skipped; the summary
/// line is the last thing the run prints.
pub struct RemoteSyncAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemoteSyncAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemoteSyncAction { system, registry }
    }

    fn parse(&self, input: &Value) -> Result<(String, PathBuf, Vec<String>, bool), ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &SYNC_FIELDS, "input")?;
        let target = text_field(&obj, "target")?;
        let source = match obj.get("source") {
            None | Some(Value::Null) => PathBuf::from("."),
            Some(Value::String(s)) if !s.trim().is_empty() => PathBuf::from(s),
            Some(_) => {
                return Err(ActionError::InvalidInput(
                    "source must be a non-empty path when present".into(),
                ))
            }
        };
        let mut extra = Vec::new();
        match obj.get("exclude") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for item in items {
                    extra.push(item.as_str().map(str::to_string).ok_or_else(|| {
                        ActionError::InvalidInput("exclude must be strings".into())
                    })?);
                }
            }
            Some(_) => return Err(ActionError::InvalidInput("exclude must be an array".into())),
        }
        let verbose = matches!(obj.get("verbose"), Some(Value::Bool(true)));
        Ok((target, source, extra, verbose))
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (target_id, source, extra, verbose) = self.parse(input)?;
        let target = self
            .system
            .resolve(&target_id)
            .map_err(ActionError::InvalidInput)?;
        let opts = SyncOptions {
            source,
            extra_excludes: extra,
            verbose,
        };
        let report = {
            let mut lines = String::new();
            let report = sync_tree(
                &self.registry,
                &target,
                &opts,
                |line: &str| lines.push_str(line),
                &cancel,
            )
            .await
            .map_err(ActionError::Failed)?;
            if let Some(sink) = sink {
                sink.append(&lines);
            }
            report
        };
        let mut meta = Map::new();
        meta.insert("target".into(), json!(target.id));
        meta.insert("scanned".into(), json!(report.scanned));
        meta.insert("uploaded".into(), json!(report.uploaded));
        meta.insert("skipped".into(), json!(report.skipped));
        meta.insert("bytes".into(), json!(report.bytes));
        let ok = report.failures.is_empty();
        let summary = format!(
            "{} scanned, {} uploaded ({} bytes), {} up to date",
            report.scanned, report.uploaded, report.bytes, report.skipped,
        );
        let chars = summary.len();
        Ok(ActionOutcome {
            ok,
            ms: started.elapsed().as_millis() as u64,
            pid: None,
            exit_code: Some(if ok { 0 } else { 1 }),
            timed_out: false,
            canceled: false,
            error: report.failures.first().cloned(),
            output: summary,
            chars,
            meta,
        })
    }
}

#[async_trait]
impl Action for RemoteSyncAction {
    fn type_name(&self) -> &'static str {
        "remote.sync"
    }

    fn title(&self) -> String {
        "Upload a local tree to a remote target".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn cancelable(&self) -> bool {
        true
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": { "type": "string" },
                "source": { "type": "string", "description": "Local directory to upload; default '.'." },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Extra excludes on top of .git/.swiss/target/node_modules. Trailing slash = directory prefix."
                },
                "verbose": { "type": "boolean", "description": "One line per uploaded file; default summary only." }
            },
            "additionalProperties": false
        })
    }

    fn validate_input(&self, input: &Value) -> Result<(), String> {
        self.parse(input).map(|_| ()).map_err(|err| err.to_string())
    }

    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError> {
        self.run(input, cancel, None).await
    }

    async fn execute_with_context(
        &self,
        input: &Value,
        ctx: ActionContext,
    ) -> Result<ActionOutcome, ActionError> {
        self.run(input, ctx.cancel.clone(), Some(&ctx.output)).await
    }
}
// --- remote.pull -------------------------------------------------------------------------

const PULL_FIELDS: [&str; 3] = ["target", "remote", "to"];

/// Download one file from the target workspace (docs/32 §20): streams chunk by
/// chunk into a local path (default: the same relative path under the current
/// directory). The remote path is workspace-relative; escaping is refused.
pub struct RemotePullAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemotePullAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemotePullAction { system, registry }
    }

    fn parse(&self, input: &Value) -> Result<(String, String, PathBuf), ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &PULL_FIELDS, "input")?;
        let target = text_field(&obj, "target")?;
        let remote = text_field(&obj, "remote")?;
        if remote.starts_with('/') || remote.contains("..") {
            return Err(ActionError::InvalidInput(format!(
                "remote {remote:?} must be relative to the target's workspaceRoot and must not contain .."
            )));
        }
        let to = match obj.get("to") {
            None | Some(Value::Null) => PathBuf::from(&remote),
            Some(Value::String(s)) if !s.trim().is_empty() => PathBuf::from(s),
            Some(_) => {
                return Err(ActionError::InvalidInput(
                    "to must be a non-empty path when present".into(),
                ))
            }
        };
        Ok((target, remote, to))
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (target_id, remote, to) = self.parse(input)?;
        let target = self
            .system
            .resolve(&target_id)
            .map_err(ActionError::InvalidInput)?;
        let total = {
            let mut lines = String::new();
            let total = pull_one(
                &self.registry,
                &target,
                &remote,
                &to,
                |line: &str| lines.push_str(line),
                &cancel,
            )
            .await
            .map_err(ActionError::Failed)?;
            if let Some(sink) = sink {
                sink.append(&lines);
            }
            total
        };
        let mut meta = Map::new();
        meta.insert("target".into(), json!(target.id));
        meta.insert("bytes".into(), json!(total));
        meta.insert("to".into(), json!(to.display().to_string()));
        let summary = format!("pulled {remote} ({total} bytes)");
        let chars = summary.len();
        Ok(ActionOutcome {
            ok: true,
            ms: started.elapsed().as_millis() as u64,
            pid: None,
            exit_code: Some(0),
            timed_out: false,
            canceled: false,
            error: None,
            output: summary,
            chars,
            meta,
        })
    }
}

#[async_trait]
impl Action for RemotePullAction {
    fn type_name(&self) -> &'static str {
        "remote.pull"
    }

    fn title(&self) -> String {
        "Download one file from a remote target".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target", "remote"],
            "properties": {
                "target": { "type": "string" },
                "remote": { "type": "string", "description": "Workspace-relative path on the target." },
                "to": { "type": "string", "description": "Local destination path; default the same relative path." }
            },
            "additionalProperties": false
        })
    }

    fn validate_input(&self, input: &Value) -> Result<(), String> {
        self.parse(input).map(|_| ()).map_err(|err| err.to_string())
    }

    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError> {
        self.run(input, cancel, None).await
    }

    async fn execute_with_context(
        &self,
        input: &Value,
        ctx: ActionContext,
    ) -> Result<ActionOutcome, ActionError> {
        self.run(input, ctx.cancel.clone(), Some(&ctx.output)).await
    }
}

/// Register all three capabilities (the plugin's start calls this).
pub fn register_all(
    system: Arc<RemoteSystem>,
    registry: &Arc<swiss_host::services::action::ActionRegistry>,
) -> Result<(), String> {
    registry.register(Arc::new(RemoteExecAction::new(system.clone())))?;
    registry.register(Arc::new(RemoteSyncAction::new(system.clone())))?;
    registry.register(Arc::new(RemotePullAction::new(system)))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{system_with_fake, FakeProgram};
    use swiss_host::services::action::CancelSource;
    use swiss_host::services::runs::SubmitRequest;
    use swiss_host::services::RuntimeServices;

    fn exec_input(target: &str, argv: &[&str]) -> Value {
        json!({ "target": target, "argv": argv })
    }

    #[test]
    fn sudo_is_refused_at_the_door() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        let err = action
            .validate_input(&exec_input("dev", &["sudo", "rm", "/etc/hosts"]))
            .unwrap_err();
        assert!(err.contains("sudo"), "{err}");
    }

    #[test]
    fn unknown_fields_are_refused() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        let err = action
            .validate_input(&json!({ "target": "dev", "argv": ["make"], "cmdd": "x" }))
            .unwrap_err();
        assert!(err.contains("input.cmdd"), "{err}");
    }

    #[test]
    fn argv_must_be_an_array_of_strings() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        assert!(action
            .validate_input(&json!({ "target": "dev", "argv": "make" }))
            .is_err());
        assert!(action
            .validate_input(&json!({ "target": "dev", "argv": [] }))
            .is_err());
        assert!(action.validate_input(&json!({ "target": "dev" })).is_err());
    }

    #[test]
    fn env_names_must_be_identifiers() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        assert!(action
            .validate_input(
                &json!({ "target": "dev", "argv": ["make"], "env": { "OK_NAME": "1" } })
            )
            .is_ok());
        for bad in ["2fast", "with-dash", "semi;colon"] {
            assert!(
                action
                    .validate_input(
                        &json!({ "target": "dev", "argv": ["make"], "env": { bad: "1" } })
                    )
                    .is_err(),
                "{bad} must not pass",
            );
        }
    }

    #[test]
    fn cwd_must_stay_inside_the_workspace() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        assert!(action
            .validate_input(&json!({ "target": "dev", "argv": ["make"], "cwd": "build" }))
            .is_ok());
        for bad in ["/etc", "a/../..", ".."] {
            assert!(
                action
                    .validate_input(&json!({ "target": "dev", "argv": ["make"], "cwd": bad }))
                    .is_err(),
                "{bad} must not pass"
            );
        }
    }

    #[tokio::test]
    async fn an_exec_streams_and_reports_the_exit_code() {
        let (system, fake) = system_with_fake();
        fake.program(
            "make",
            FakeProgram {
                argv0: "make".into(),
                stdout: b"building...\ndone\n".to_vec(),
                stderr: b"1 warning\n".to_vec(),
                exit: 0,
                delay_ms: 0,
            },
        );
        let action = RemoteExecAction::new(system.clone());
        let out = action
            .execute(
                &exec_input("dev", &["make", "-j8"]),
                CancelSource::new().handle(),
            )
            .await
            .expect("the exec runs");
        assert!(out.ok);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.output.contains("building..."), "{}", out.output);
        assert_eq!(out.meta.get("target").unwrap(), "dev");
        // The transport saw ARGV, not a command string.
        let calls = fake.exec_calls.lock().unwrap();
        assert_eq!(calls[0].argv, vec!["make", "-j8"]);
        assert!(calls[0].cwd.is_none());
    }

    #[tokio::test]
    async fn an_exec_resolves_cwd_under_the_workspace_root() {
        let (system, fake) = system_with_fake();
        fake.program(
            "ninja",
            FakeProgram {
                argv0: "ninja".into(),
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        let action = RemoteExecAction::new(system.clone());
        let input = json!({ "target": "dev", "argv": ["ninja"], "cwd": "build/arm", "env": { "BOARD": "rpi" } });
        action
            .execute(&input, CancelSource::new().handle())
            .await
            .expect("runs");
        let calls = fake.exec_calls.lock().unwrap();
        assert_eq!(calls[0].cwd.as_deref(), Some("/data/ws/proj/build/arm"));
        assert_eq!(calls[0].env, vec![("BOARD".to_string(), "rpi".to_string())]);
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_failed_but_completed_run() {
        let (system, fake) = system_with_fake();
        fake.program(
            "fail",
            FakeProgram {
                argv0: "fail".into(),
                stdout: b"started\n".to_vec(),
                stderr: b"boom".to_vec(),
                exit: 2,
                delay_ms: 0,
            },
        );
        let action = RemoteExecAction::new(system);
        let out = action
            .execute(&exec_input("dev", &["fail"]), CancelSource::new().handle())
            .await
            .expect("a nonzero exit is still an outcome");
        assert!(!out.ok);
        assert_eq!(out.exit_code, Some(2));
        assert!(out.output.contains("boom"));
    }

    #[tokio::test]
    async fn an_unknown_target_is_invalid_input_not_a_transport_error() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        match action
            .execute(
                &exec_input("ghost", &["make"]),
                CancelSource::new().handle(),
            )
            .await
        {
            Err(ActionError::InvalidInput(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown target, got a run"),
            Err(other) => panic!("wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn exec_streams_into_the_live_buffer() {
        // Through the shared run coordinator, like a real submit: the buffer is
        // what /api/runs/{id}/output reads.
        let (system, fake) = system_with_fake();
        fake.program(
            "make",
            FakeProgram {
                argv0: "make".into(),
                stdout: b"step1\nstep2\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        crate::actions::register_all(system.clone(), &system.services().actions).unwrap();
        let submitted = system.services().runs.submit(SubmitRequest {
            owner: REMOTE_OWNER.into(),
            label: "test".into(),
            action_type: "remote.exec".into(),
            input: exec_input("dev", &["make"]),
            timeout_ms: 30_000,
            queue_if_busy: false,
        });
        let submitted = match submitted {
            Ok(s) => s,
            Err(err) => panic!("submit refused: {err:?}"),
        };
        let (_done, streamed) = tokio::join!(submitted.done, async {
            loop {
                match system
                    .services()
                    .runs
                    .output(submitted.run_id, 0, u32::MAX as usize)
                {
                    Some((_, chunk)) if chunk.text.contains("step2") => break Some(chunk),
                    Some((state, _)) if state.is_terminal() => break None,
                    _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                }
            }
        });
        assert!(
            streamed.is_some(),
            "the output streamed while the run was live"
        );
    }

    #[tokio::test]
    async fn a_canceled_exec_stops_honestly() {
        let (system, fake) = system_with_fake();
        fake.program(
            "longbuild",
            FakeProgram {
                argv0: "longbuild".into(),
                stdout: vec![b'x'; 3 * 60], // 60 chunks at 5ms each: cancel lands mid-stream
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 5,
            },
        );
        let action = RemoteExecAction::new(system);
        let exec = tokio::spawn(async move {
            action
                .execute(&exec_input("dev", &["longbuild"]), source_handle())
                .await
        });
        // Give the stream a moment to start, then cancel.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        CANCEL.with(|c| c.cancel());
        let out = exec
            .await
            .expect("task")
            .expect("a cancel is an outcome, not an error");
        assert!(!out.ok);
        assert!(out.canceled, "the outcome says canceled");
        assert!(
            out.output.chars().count() < 180,
            "stopped early: {}",
            out.output.chars().count()
        );
    }

    // The cancel plumbing for the test above: one thread-local source the spawn
    // reads its handle from.
    thread_local! {
        static CANCEL: CancelSource = CancelSource::new();
    }
    fn source_handle() -> CancelHandle {
        CANCEL.with(|c| c.handle())
    }

    #[tokio::test]
    async fn no_transport_means_an_honest_enable_tunnels_error() {
        swiss_core::secure::key::use_test_master_key();
        let services = RuntimeServices::new();
        let dir =
            std::env::temp_dir().join(format!("swiss-rnone-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let system = RemoteSystem::open(services, dir.join("remote.json"));
        system
            .with_store(|store| {
                store.add(crate::target::RemoteTarget {
                    id: "dev".into(),
                    label: "dev".into(),
                    endpoint: "conn-1".into(),
                    workspace_root: "/data/ws".into(),
                    shell: "posix".into(),
                    capabilities: vec!["exec".into()],
                    default_timeout_ms: None,
                })
            })
            .expect("target adds");
        let action = RemoteExecAction::new(system);
        match action
            .execute(&exec_input("dev", &["make"]), CancelSource::new().handle())
            .await
        {
            Err(ActionError::Failed(m)) => assert!(m.contains("Tunnels"), "{m}"),
            Ok(_) => panic!("no transport, got a run"),
            Err(other) => panic!("wrong error: {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sync_uploads_changed_files_and_skips_up_to_date_ones() {
        let (system, fake) = system_with_fake();
        // A pre-existing remote file with the SAME size must be skipped.
        fake.files
            .lock()
            .unwrap()
            .insert("/data/ws/proj/same.txt".into(), b"same".to_vec());
        let dir =
            std::env::temp_dir().join(format!("swiss-syn-{}", swiss_core::util::random_hex(8)));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).expect("tree");
        std::fs::write(src.join("same.txt"), b"same").expect("w");
        std::fs::write(src.join("sub/new.txt"), b"new content").expect("w");
        std::fs::create_dir_all(src.join(".git")).expect("git");
        std::fs::write(src.join(".git/config"), b"ignored").expect("w");
        let action = RemoteSyncAction::new(system);
        let out = action
            .execute(
                &json!({ "target": "dev", "source": src.display().to_string() }),
                CancelSource::new().handle(),
            )
            .await
            .expect("the sync runs");
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert_eq!(
            out.meta.get("scanned").unwrap(),
            2,
            "the .git dir is excluded"
        );
        assert_eq!(out.meta.get("uploaded").unwrap(), 1);
        assert_eq!(out.meta.get("skipped").unwrap(), 1);
        let files = fake.files.lock().unwrap();
        assert_eq!(
            files.get("/data/ws/proj/sub/new.txt").map(Vec::as_slice),
            Some(&b"new content"[..]),
            "the changed file landed under the workspace root",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pull_streams_one_file_to_a_local_path() {
        let (system, fake) = system_with_fake();
        fake.files
            .lock()
            .unwrap()
            .insert("/data/ws/proj/out.bin".into(), b"artifact-bytes".to_vec());
        let dir =
            std::env::temp_dir().join(format!("swiss-pull-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let action = RemotePullAction::new(system);
        let out = action
            .execute(
                &json!({ "target": "dev", "remote": "out.bin", "to": dir.join("got.bin").display().to_string() }),
                CancelSource::new().handle(),
            )
            .await
            .expect("the pull runs");
        assert!(out.ok);
        assert_eq!(out.meta.get("bytes").unwrap(), 14);
        assert_eq!(
            std::fs::read(dir.join("got.bin")).expect("pulled"),
            b"artifact-bytes".to_vec(),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_refuses_paths_that_escape_the_workspace() {
        let (system, _fake) = system_with_fake();
        let action = RemotePullAction::new(system);
        for bad in ["/etc/passwd", "../../etc/passwd"] {
            assert!(
                action
                    .validate_input(&json!({ "target": "dev", "remote": bad }))
                    .is_err(),
                "{bad} must not pass"
            );
        }
    }

    #[test]
    fn register_all_registers_the_three_capabilities() {
        let (system, _fake) = system_with_fake();
        crate::actions::register_all(system.clone(), &system.services().actions)
            .expect("registers");
        let names: Vec<String> = system
            .services()
            .actions
            .list()
            .iter()
            .map(|info| info.type_name.to_string())
            .collect();
        for name in ["remote.exec", "remote.sync", "remote.pull"] {
            assert!(
                names.contains(&name.to_string()),
                "{name} missing: {names:?}"
            );
        }
    }
}
