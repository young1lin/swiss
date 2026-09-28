/*
 * Copyright 2026 young1lin
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

//! The three remote capabilities (docs/34): remote.exec, remote.sync, remote.pull.
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

use swiss_host::mask::StreamMask;
use swiss_host::services::action::{
    Action, ActionContext, ActionError, ActionOutcome, CancelHandle, RunLane, RunOutputSink,
};
use swiss_host::services::remote::{
    RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteTransportRegistry,
};

use crate::sync::{pull_one, pull_tree, sync_tree, SyncOptions};
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
    /// The values vault references resolved to: masked before a byte reaches the live
    /// buffer, the record or the outcome (docs/34, 2026-09-28 addendum).
    mask: StreamMask,
}

impl<'a> Tap<'a> {
    fn live(sink: &'a RunOutputSink) -> Self {
        Tap {
            sink: Some(sink),
            tail: Vec::new(),
            chars: 0,
            mask: StreamMask::new(&[]),
        }
    }

    fn quiet() -> Tap<'static> {
        Tap {
            sink: None,
            tail: Vec::new(),
            chars: 0,
            mask: StreamMask::new(&[]),
        }
    }

    fn masking(mut self, secrets: &[String]) -> Self {
        self.mask = StreamMask::new(secrets);
        self
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.mask.is_empty() {
            self.emit(bytes);
        } else {
            let safe = self.mask.feed(bytes);
            self.emit(&safe);
        }
    }

    /// The stream ended: release what the mask held back as a possible secret start.
    fn finish(&mut self) {
        let rest = self.mask.finish();
        self.emit(&rest);
    }

    fn emit(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
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
        // The head kept by byte count may end inside a character (docs/41 U3).
        String::from_utf8_lossy(swiss_core::utf8::window(&self.tail)).into_owned()
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
        None | Some(Value::Null) => return Ok(with_utf8_locale(pairs)),
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
    Ok(with_utf8_locale(pairs))
}

/// The locale every remote command runs under unless the caller says otherwise
/// (docs/41 U4): `LANG` and `LC_ALL` set to `C.UTF-8`, so a remote login user whose
/// shell profile leaves `LANG` empty (or on a GB18030 box) still gets UTF-8 from `ls`,
/// `git`, `python` and friends, and argv bytes are read as the UTF-8 they are.
pub const UTF8_LOCALE: &str = "C.UTF-8";

/// Prepend the UTF-8 locale defaults to a caller's env. The caller's own `LANG`,
/// `LC_ALL` or any `LC_*` wins: a default is added only when the name is absent, and
/// it goes FIRST so `exec_command_string` exports it before anything the caller set
/// (a later `export` of the same name overrides an earlier one). The `export` happens
/// after the login shell started and before `exec`, so a shell whose box lacks the
/// locale never prints a setlocale warning of its own; a program that cannot load it
/// falls back to C silently and still passes UTF-8 bytes through unchanged.
fn with_utf8_locale(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let has = |name: &str| pairs.iter().any(|(n, _)| n == name);
    let mut defaults: Vec<(String, String)> = Vec::new();
    if !has("LANG") {
        defaults.push(("LANG".to_string(), UTF8_LOCALE.to_string()));
    }
    if !has("LC_ALL") && !pairs.iter().any(|(n, _)| n.starts_with("LC_")) {
        defaults.push(("LC_ALL".to_string(), UTF8_LOCALE.to_string()));
    }
    defaults.append(&mut pairs);
    defaults
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

/// Run one command on a target, streaming output (docs/34). THE action of this
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
        let env = env_map(&obj)?;
        let cwd = opt_text_field(&obj, "cwd")?;
        if let Some(rel) = &cwd {
            // Absolute cwds are used as-is (the caller typed the full path - the same
            // trust an ssh command line gets); relative ones resolve under the root.
            // Only `..` is refused, in either form.
            if rel.split('/').any(|s| s == "..") {
                return Err(ActionError::InvalidInput(format!(
                    "cwd {rel:?} must not contain ..; relative cwds resolve under the target's workspaceRoot, absolute cwds are used as-is"
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

/// Resolve the vault references in an exec's argv, env values and cwd (docs/34 R10):
/// `${secret://name}` becomes the stored value, `${secret://name:default}` the default when
/// the vault does not hold the name, and a missing name without a default refuses the run
/// before anything is sent. `${UPPER}` stays text - the far side's shell owns it. This runs
/// INSIDE the action, after the run was recorded, so the run list, the audit and the panel
/// keep the reference as typed; the values come back for the output mask.
fn resolve_exec_refs(parsed: ParsedExec) -> Result<(ParsedExec, Vec<String>), ActionError> {
    let mut secrets: Vec<String> = Vec::new();
    let mut one = |text: &str, field: &str| -> Result<String, ActionError> {
        let (out, got) = swiss_core::secure::refs::resolve_secrets_collect(text)
            .map_err(|err| ActionError::InvalidInput(format!("{field} {err}")))?;
        for v in got {
            if !secrets.contains(&v) {
                secrets.push(v);
            }
        }
        Ok(out)
    };
    let mut argv = Vec::with_capacity(parsed.argv.len());
    for (i, word) in parsed.argv.iter().enumerate() {
        argv.push(one(word, &format!("argv[{i}]"))?);
    }
    let mut env = Vec::with_capacity(parsed.env.len());
    for (name, value) in &parsed.env {
        env.push((name.clone(), one(value, &format!("env.{name}"))?));
    }
    let cwd = match &parsed.cwd {
        Some(c) => Some(one(c, "cwd")?),
        None => None,
    };
    Ok((
        ParsedExec {
            target: parsed.target,
            argv,
            env,
            cwd,
        },
        secrets,
    ))
}

/// The validation half of [resolve_exec_refs]: every reference well-formed, named by its
/// field - and the vault never read. validate_input also runs when jobs.json is saved and at
/// boot, where a failing definition is DROPPED; a job naming a secret stored later must
/// survive both, and fails at its run if the name is still missing.
fn check_exec_refs(parsed: &ParsedExec) -> Result<(), ActionError> {
    let check = |text: &str, field: String| {
        swiss_core::secure::refs::check_secret_refs(text)
            .map_err(|err| ActionError::InvalidInput(format!("{field} {err}")))
    };
    for (i, word) in parsed.argv.iter().enumerate() {
        check(word, format!("argv[{i}]"))?;
    }
    for (name, value) in &parsed.env {
        check(value, format!("env.{name}"))?;
    }
    if let Some(cwd) = &parsed.cwd {
        check(cwd, "cwd".to_string())?;
    }
    Ok(())
}

const EXEC_FIELDS: [&str; 5] = ["target", "argv", "env", "cwd", "timeoutMs"];

/// The parsed, validated sync input (also the validate_input surface).
struct ParsedSync {
    target: String,
    source: PathBuf,
    extra_excludes: Vec<String>,
    verbose: bool,
    to: Option<String>,
}

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
    tap.finish();
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

    fn lane(&self, input: &Value) -> RunLane {
        target_lane(input)
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
                "cwd": { "type": "string", "description": "Working directory: relative resolves under the target's workspaceRoot." },
                "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 86400000, "description": "Run deadline; the submit path owns it." }
            },
            "additionalProperties": false
        })
    }

    fn validate_input(&self, input: &Value) -> Result<(), String> {
        self.parse(input)
            .and_then(|parsed| check_exec_refs(&parsed))
            .map_err(|err| err.to_string())
    }

    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (parsed, secrets) = resolve_exec_refs(self.parse(input)?)?;
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
            cancel,
            Tap::quiet().masking(&secrets),
        )
        .await?;
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
        let (parsed, secrets) = resolve_exec_refs(self.parse(input)?)?;
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
            Tap::live(&ctx.output).masking(&secrets),
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

const SYNC_FIELDS: [&str; 5] = ["target", "source", "exclude", "verbose", "to"];

/// Upload a local tree to the target workspace (docs/34 §20). One-way, no deletes:
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

    fn parse(&self, input: &Value) -> Result<ParsedSync, ActionError> {
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
        let to = opt_text_field(&obj, "to")?;
        Ok(ParsedSync {
            target,
            source,
            extra_excludes: extra,
            verbose,
            to,
        })
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let parsed = self.parse(input)?;
        let target = self
            .system
            .resolve(&parsed.target)
            .map_err(ActionError::InvalidInput)?;
        let opts = SyncOptions {
            source: parsed.source,
            extra_excludes: parsed.extra_excludes,
            to: parsed.to,
            verbose: parsed.verbose,
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

    fn lane(&self, input: &Value) -> RunLane {
        target_lane(input)
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
                "source": { "type": "string", "description": "Local directory (or single file) to upload; default '.'." },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Extra excludes on top of .git/.swiss/target/node_modules. Trailing slash = directory prefix."
                },
                "verbose": { "type": "boolean", "description": "One line per uploaded file; default summary only." },
                "to": { "type": "string", "description": "Remote name for a single-file upload; default the source file's own name." }
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

const PULL_FIELDS: [&str; 4] = ["target", "remote", "to", "verbose"];

/// Download a file or a whole directory tree from the target workspace (docs/34
/// §20): the remote path is statted first - a file streams chunk by chunk into a
/// local path (default: the same relative path under the current directory), a
/// directory walks down recursively. The remote path is workspace-relative;
/// escaping is refused.
pub struct RemotePullAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemotePullAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemotePullAction { system, registry }
    }

    fn parse(&self, input: &Value) -> Result<(String, String, PathBuf, bool), ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &PULL_FIELDS, "input")?;
        let target = text_field(&obj, "target")?;
        let remote = text_field(&obj, "remote")?;
        // Absolute remote paths are used as-is (the caller typed the full path);
        // relative ones resolve under the workspaceRoot. Only `..` is refused.
        if remote.split('/').any(|s| s == "..") {
            return Err(ActionError::InvalidInput(format!(
                "remote {remote:?} must not contain ..; relative paths resolve under the target's workspaceRoot, absolute paths are used as-is"
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
        let verbose = matches!(obj.get("verbose"), Some(Value::Bool(true)));
        Ok((target, remote, to, verbose))
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (target_id, remote, to, verbose) = self.parse(input)?;
        let target = self
            .system
            .resolve(&target_id)
            .map_err(ActionError::InvalidInput)?;
        // File or tree? The stat decides: a directory pulls recursively through
        // pull_tree, a file streams exactly as it always did.
        let remote_path =
            safe_join(&target.workspace_root, &remote).map_err(ActionError::InvalidInput)?;
        let holder = format!("{}:{}", REMOTE_OWNER, target.id);
        let stat = self
            .registry
            .stat(&target.endpoint, &holder, &remote_path)
            .await
            .map_err(transport_error)?;
        let mut meta = Map::new();
        let summary = if stat.is_some_and(|s| s.is_dir) {
            let mut lines = String::new();
            let report = pull_tree(
                &self.registry,
                &target,
                &remote,
                &to,
                verbose,
                |line: &str| lines.push_str(line),
                &cancel,
            )
            .await
            .map_err(ActionError::Failed)?;
            if let Some(sink) = sink {
                sink.append(&lines);
            }
            meta.insert("files".into(), json!(report.files));
            meta.insert("bytes".into(), json!(report.bytes));
            meta.insert("dirs".into(), json!(report.dirs));
            format!(
                "pull: {} files ({} bytes), {} dirs",
                report.files, report.bytes, report.dirs
            )
        } else {
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
            meta.insert("bytes".into(), json!(total));
            format!("pulled {remote} ({total} bytes)")
        };
        meta.insert("target".into(), json!(target.id));
        meta.insert("to".into(), json!(to.display().to_string()));
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
        "Download a file or folder from a remote target".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn lane(&self, input: &Value) -> RunLane {
        target_lane(input)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target", "remote"],
            "properties": {
                "target": { "type": "string" },
                "remote": { "type": "string", "description": "Workspace-relative file or directory path on the target." },
                "to": { "type": "string", "description": "Local destination path; default the same relative path." },
                "verbose": { "type": "boolean", "description": "One line per pulled file; default summary only." }
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

/// remote.cat - print one remote file as the run output. The model-facing read
/// primitive: no local file is created, the content streams chunk-by-chunk into
/// the run output (and into the outcome text for the CLI). The same path rule as
/// pull: relative resolves under the workspaceRoot, absolute passes as-is.
pub struct RemoteCatAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemoteCatAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemoteCatAction { system, registry }
    }

    fn parse(&self, input: &Value) -> Result<(String, String), ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &["target", "remote"], "input")?;
        let target = text_field(&obj, "target")?;
        let remote = text_field(&obj, "remote")?;
        if remote.split('/').any(|s| s == "..") {
            return Err(ActionError::InvalidInput(format!(
                "remote {remote:?} must not contain ..; relative paths resolve under the target's workspaceRoot, absolute paths are used as-is"
            )));
        }
        Ok((target, remote))
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (target_id, remote) = self.parse(input)?;
        let target = self
            .system
            .resolve(&target_id)
            .map_err(ActionError::InvalidInput)?;
        if !target
            .capabilities
            .iter()
            .any(|c| c == "files" || c == "sync")
        {
            return Err(ActionError::InvalidInput(format!(
                "target {} does not declare the files or sync capability",
                target.id
            )));
        }
        let remote_path =
            safe_join(&target.workspace_root, &remote).map_err(ActionError::InvalidInput)?;
        let holder = format!("{}:{}", REMOTE_OWNER, target.id);
        let mut reader = self
            .registry
            .open_read(&target.endpoint, &holder, &remote_path)
            .await
            .map_err(transport_error)?;
        // Bytes first, text once: a chunk boundary is an SSH read boundary, never a
        // character boundary, so decoding per chunk turned a 汉字 split across two reads
        // into two U+FFFD (docs/41 U3). The cap is 128 KiB, so holding the bytes is free.
        let mut bytes: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(ActionError::InvalidInput("the cat was canceled".into()));
            }
            let chunk = reader.read_chunk().await.map_err(transport_error)?;
            let Some(chunk) = chunk else { break };
            total += chunk.len() as u64;
            if total > 128 * 1024 {
                return Err(ActionError::InvalidInput(format!(
                    "{remote} is larger than the 128 KiB cat limit; use remote.pull instead"
                )));
            }
            if let Some(sink) = sink {
                sink.append_bytes(chunk.as_slice());
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut meta = Map::new();
        meta.insert("target".into(), json!(target.id));
        meta.insert("remote".into(), json!(remote));
        meta.insert("bytes".into(), json!(total));
        Ok(ActionOutcome {
            ok: true,
            ms: started.elapsed().as_millis() as u64,
            pid: None,
            exit_code: Some(0),
            timed_out: false,
            canceled: false,
            error: None,
            output: text,
            chars: total as usize,
            meta,
        })
    }
}

#[async_trait::async_trait]
impl Action for RemoteCatAction {
    fn type_name(&self) -> &'static str {
        "remote.cat"
    }

    fn title(&self) -> String {
        "Print one remote file as the run output".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn lane(&self, input: &Value) -> RunLane {
        target_lane(input)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target", "remote"],
            "properties": {
                "target": { "type": "string" },
                "remote": { "type": "string", "description": "Workspace-relative file path; absolute paths pass as-is." }
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

/// remote.write - write a content string to one remote file (create or overwrite).
/// The model-facing write primitive: whole-content semantics, the mirror of cat.
pub struct RemoteWriteAction {
    system: Arc<RemoteSystem>,
    registry: Arc<RemoteTransportRegistry>,
}

impl RemoteWriteAction {
    pub fn new(system: Arc<RemoteSystem>) -> Self {
        let registry = system.services().remote.clone();
        RemoteWriteAction { system, registry }
    }

    fn parse(&self, input: &Value) -> Result<(String, String, String), ActionError> {
        let obj = object_of(input)?;
        refuse_unknown(&obj, &["target", "remote", "content"], "input")?;
        let target = text_field(&obj, "target")?;
        let remote = text_field(&obj, "remote")?;
        let content = text_field(&obj, "content")?;
        if remote.split('/').any(|s| s == "..") {
            return Err(ActionError::InvalidInput(format!(
                "remote {remote:?} must not contain ..; relative paths resolve under the target's workspaceRoot, absolute paths are used as-is"
            )));
        }
        Ok((target, remote, content))
    }

    async fn run(
        &self,
        input: &Value,
        cancel: CancelHandle,
        sink: Option<&RunOutputSink>,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let (target_id, remote, content) = self.parse(input)?;
        let target = self
            .system
            .resolve(&target_id)
            .map_err(ActionError::InvalidInput)?;
        if !target
            .capabilities
            .iter()
            .any(|c| c == "files" || c == "sync")
        {
            return Err(ActionError::InvalidInput(format!(
                "target {} does not declare the files or sync capability",
                target.id
            )));
        }
        let remote_path =
            safe_join(&target.workspace_root, &remote).map_err(ActionError::InvalidInput)?;
        let holder = format!("{}:{}", REMOTE_OWNER, target.id);
        let mut writer = self
            .registry
            .create(&target.endpoint, &holder, &remote_path)
            .await
            .map_err(transport_error)?;
        for chunk in content.as_bytes().chunks(64 * 1024) {
            if cancel.is_cancelled() {
                return Err(ActionError::InvalidInput("the write was canceled".into()));
            }
            writer.write_chunk(chunk).await.map_err(transport_error)?;
        }
        writer.finish().await.map_err(transport_error)?;
        let summary = format!("wrote {} ({} bytes)", remote, content.len());
        let chars = summary.len();
        if let Some(sink) = sink {
            sink.append_bytes(summary.as_bytes());
            sink.append_bytes(b"\n");
        }
        let mut meta = Map::new();
        meta.insert("target".into(), json!(target.id));
        meta.insert("remote".into(), json!(remote));
        meta.insert("bytes".into(), json!(content.len()));
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

#[async_trait::async_trait]
impl Action for RemoteWriteAction {
    fn type_name(&self) -> &'static str {
        "remote.write"
    }

    fn title(&self) -> String {
        "Write a content string to one remote file".into()
    }

    fn provider(&self) -> &'static str {
        "remote"
    }

    fn lane(&self, input: &Value) -> RunLane {
        target_lane(input)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target", "remote", "content"],
            "properties": {
                "target": { "type": "string" },
                "remote": { "type": "string", "description": "Workspace-relative file path; absolute paths pass as-is." },
                "content": { "type": "string", "description": "The full file content; create or overwrite." }
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

/// Every remote capability runs in its target's lane (docs/34 R11): the work happens on the
/// far machine, one session channel on the target's connection here - not a local slot.
fn target_lane(input: &Value) -> RunLane {
    match input.get("target").and_then(Value::as_str) {
        Some(target) => RunLane::Remote(target.to_string()),
        None => RunLane::Local, // no target: the run fails its parse at once
    }
}

/// Register every capability (the plugin's start calls this).
pub fn register_all(
    system: Arc<RemoteSystem>,
    registry: &Arc<swiss_host::services::action::ActionRegistry>,
) -> Result<(), String> {
    registry.register(Arc::new(RemoteExecAction::new(system.clone())))?;
    registry.register(Arc::new(RemoteSyncAction::new(system.clone())))?;
    registry.register(Arc::new(RemotePullAction::new(system.clone())))?;
    registry.register(Arc::new(RemoteCatAction::new(system.clone())))?;
    registry.register(Arc::new(RemoteWriteAction::new(system)))?;
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

    #[tokio::test]
    async fn sudo_runs_like_any_other_argv0() {
        // sudo is argv[0] like any other program now. There is no PTY, so an
        // interactive password prompt cannot work - non-interactive sudo (-n,
        // or passwordless config on the machine) is the supported shape.
        let (system, fake) = system_with_fake();
        fake.program(
            "sudo",
            FakeProgram {
                argv0: "sudo".into(),
                stdout: b"ok\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        let action = RemoteExecAction::new(system);
        action
            .validate_input(&exec_input("dev", &["sudo", "-n", "true"]))
            .expect("sudo validates like any other argv[0]");
        let out = action
            .execute(
                &exec_input("dev", &["sudo", "-n", "true"]),
                CancelSource::new().handle(),
            )
            .await
            .expect("the exec runs");
        assert!(out.ok);
        assert_eq!(out.exit_code, Some(0));
        let calls = fake.exec_calls.lock().unwrap();
        assert_eq!(calls[0].argv, vec!["sudo", "-n", "true"]);
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
    fn every_exec_carries_the_utf8_locale_unless_the_caller_set_one() {
        // docs/41 U4. No env at all: both defaults, in a fixed order.
        let none = env_map(&Map::new()).unwrap();
        assert_eq!(
            none,
            vec![
                ("LANG".to_string(), UTF8_LOCALE.to_string()),
                ("LC_ALL".to_string(), UTF8_LOCALE.to_string())
            ]
        );
        // A caller env: the defaults go FIRST so a later export of the same name wins.
        let mut obj = Map::new();
        obj.insert("env".into(), json!({ "Z": "1", "A": "2" }));
        let names: Vec<String> = env_map(&obj).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["LANG", "LC_ALL", "A", "Z"]);
        // The caller's own LANG is not touched and LC_ALL is still defaulted.
        obj.insert("env".into(), json!({ "LANG": "zh_CN.GB18030" }));
        let pairs = env_map(&obj).unwrap();
        assert_eq!(pairs.iter().filter(|(n, _)| n == "LANG").count(), 1);
        assert_eq!(pairs[0], ("LC_ALL".to_string(), UTF8_LOCALE.to_string()));
        assert_eq!(pairs[1], ("LANG".to_string(), "zh_CN.GB18030".to_string()));
        // Any LC_* from the caller means they are managing categories: no LC_ALL default,
        // which would override every category they set.
        obj.insert("env".into(), json!({ "LC_CTYPE": "en_US.UTF-8" }));
        let names: Vec<String> = env_map(&obj).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["LANG", "LC_CTYPE"]);
    }

    #[test]
    fn cwd_must_stay_inside_the_workspace() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        assert!(action
            .validate_input(&json!({ "target": "dev", "argv": ["make"], "cwd": "build" }))
            .is_ok());
        // An absolute cwd is an explicit path the caller typed in full - ssh-level
        // trust; only `..` is refused in either form (docs/34 superset rule).
        assert!(action
            .validate_input(
                &json!({ "target": "dev", "argv": ["make"], "cwd": "/home/dev/app" })
            )
            .is_ok());
        for bad in ["a/../..", ".."] {
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
        // The UTF-8 locale defaults ride first (docs/41 U4), the caller's env after.
        assert_eq!(
            calls[0].env,
            vec![
                ("LANG".to_string(), UTF8_LOCALE.to_string()),
                ("LC_ALL".to_string(), UTF8_LOCALE.to_string()),
                ("BOARD".to_string(), "rpi".to_string())
            ]
        );
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
            actor: "test".to_string(),
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
                    group: None,
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
    async fn sync_of_a_single_file_uploads_just_that_file() {
        let (system, fake) = system_with_fake();
        let dir =
            std::env::temp_dir().join(format!("swiss-psh-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let src = dir.join("one.txt");
        std::fs::write(&src, b"single").expect("w");
        std::fs::write(dir.join("sibling.txt"), b"not this one").expect("w");
        let action = RemoteSyncAction::new(system);
        let out = action
            .execute(
                &json!({ "target": "dev", "source": src.display().to_string() }),
                CancelSource::new().handle(),
            )
            .await
            .expect("the sync runs");
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert_eq!(out.meta.get("scanned").unwrap(), 1);
        assert_eq!(out.meta.get("uploaded").unwrap(), 1);
        {
            let files = fake.files.lock().unwrap();
            assert_eq!(
                files.get("/data/ws/proj/one.txt").map(Vec::as_slice),
                Some(&b"single"[..]),
                "the one named file landed under its own name",
            );
            assert!(
                !files.contains_key("/data/ws/proj/sibling.txt"),
                "a sibling of the source file is not uploaded"
            );
        }
        // With "to": the same single file under the caller's remote name.
        let out = action
            .execute(
                &json!({
                    "target": "dev",
                    "source": src.display().to_string(),
                    "to": "renamed.txt"
                }),
                CancelSource::new().handle(),
            )
            .await
            .expect("the sync runs");
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert_eq!(
            fake.files
                .lock()
                .unwrap()
                .get("/data/ws/proj/renamed.txt")
                .map(Vec::as_slice),
            Some(&b"single"[..]),
            "the file landed under the to name",
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

    #[tokio::test]
    async fn pull_walks_a_remote_directory_tree_into_a_local_one() {
        let (system, fake) = system_with_fake();
        {
            let mut files = fake.files.lock().unwrap();
            files.insert("/data/ws/proj/a/x.txt".into(), b"xx".to_vec());
            files.insert("/data/ws/proj/a/sub/y.txt".into(), b"yyy".to_vec());
        }
        let dir =
            std::env::temp_dir().join(format!("swiss-pte-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let action = RemotePullAction::new(system);
        let out = action
            .execute(
                &json!({ "target": "dev", "remote": "a", "to": dir.join("got").display().to_string() }),
                CancelSource::new().handle(),
            )
            .await
            .expect("the pull runs");
        assert!(out.ok, "{}", out.error.clone().unwrap_or_default());
        assert_eq!(out.meta.get("files").unwrap(), 2, "both files counted");
        assert_eq!(out.meta.get("bytes").unwrap(), 5, "2 + 3 bytes");
        assert_eq!(
            std::fs::read(dir.join("got/x.txt")).expect("x landed"),
            b"xx".to_vec(),
        );
        assert_eq!(
            std::fs::read(dir.join("got/sub/y.txt")).expect("y landed"),
            b"yyy".to_vec(),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_refuses_paths_that_escape_the_workspace() {
        let (system, _fake) = system_with_fake();
        let action = RemotePullAction::new(system);
        // Absolute remote paths pass (caller typed them in full); only `..` climbs.
        assert!(action
            .validate_input(&json!({ "target": "dev", "remote": "/etc/passwd" }))
            .is_ok());
        assert!(action
            .validate_input(&json!({ "target": "dev", "remote": "../../etc/passwd" }))
            .is_err());
    }

    #[tokio::test]
    async fn write_then_cat_round_trips_through_the_fake_transport() {
        let (system, fake) = system_with_fake();
        let cancel = CancelSource::new().handle();
        let write = RemoteWriteAction::new(system.clone());
        let out = write
            .execute(
                &json!({ "target": "dev", "remote": "notes/hello.txt", "content": "hi from write" }),
                cancel.clone(),
            )
            .await
            .expect("write runs");
        assert!(out.ok);
        assert_eq!(
            fake.files
                .lock()
                .unwrap()
                .get("/data/ws/proj/notes/hello.txt")
                .cloned(),
            Some(b"hi from write".to_vec())
        );
        let cat = RemoteCatAction::new(system);
        let out = cat
            .execute(
                &json!({ "target": "dev", "remote": "notes/hello.txt" }),
                cancel,
            )
            .await
            .expect("cat runs");
        assert!(out.ok);
        assert_eq!(out.output, "hi from write");
    }

    #[tokio::test]
    async fn cat_decodes_once_so_a_character_split_across_read_chunks_survives() {
        // docs/41 U3: the fake reads in 64 KiB chunks; a file whose 65 535th byte begins
        // a 汉字 puts that character's bytes in two chunks. Decoding per chunk made it
        // two U+FFFD; decoding the whole once keeps it.
        let (system, fake) = system_with_fake();
        let mut bytes = vec![b'x'; 64 * 1024 - 1];
        bytes.extend_from_slice("中文 ok\n".as_bytes());
        fake.files
            .lock()
            .unwrap()
            .insert("/data/ws/proj/big.txt".to_string(), bytes.clone());
        let cat = RemoteCatAction::new(system);
        let out = cat
            .execute(
                &json!({ "target": "dev", "remote": "big.txt" }),
                CancelSource::new().handle(),
            )
            .await
            .expect("cat runs");
        assert!(out.ok);
        assert!(!out.output.contains('\u{FFFD}'));
        assert!(out.output.ends_with("中文 ok\n"));
        assert_eq!(out.output.len(), bytes.len());
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

    // --- vault references in an exec (docs/34, 2026-09-28 addendum) ----------------------

    /// Plant a vault value under a test-unique name. The vault is process-wide and its
    /// scratch home is locked by a blocking mutex, so the write leaves the async runtime.
    async fn plant_secret(name: &'static str, value: &'static str) {
        tokio::task::spawn_blocking(move || {
            let _guard = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
            let path = swiss_core::paths::test_home().join("secrets.json");
            swiss_core::secure::secretstore::inject_vault(&path);
            let rev = swiss_core::secure::secretstore::vault_rev();
            swiss_core::secure::secretstore::put_secret(&path, name, value, rev).expect("plant");
        })
        .await
        .expect("the plant task");
    }

    #[tokio::test]
    async fn a_vault_reference_reaches_the_far_side_resolved_and_comes_back_masked() {
        plant_secret("remote-exec-token", "s3cr3t-token-value").await;
        let (system, fake) = system_with_fake();
        fake.program(
            "show",
            FakeProgram {
                argv0: "show".into(),
                stdout: b"token=s3cr3t-token-value done\n".to_vec(),
                stderr: b"echoed s3cr3t-token-value\n".to_vec(),
                exit: 0,
                delay_ms: 0,
            },
        );
        let action = RemoteExecAction::new(system);
        let input = json!({
            "target": "dev",
            "argv": ["show", "--token=${secret://remote-exec-token}",
                     "${secret://remote-exec-absent:plain-default}", "${HOME}"],
            "env": { "REDISCLI_AUTH": "${secret://remote-exec-token}" },
        });
        action
            .validate_input(&input)
            .expect("a held reference and a defaulted one both validate");
        let out = action
            .execute(&input, CancelSource::new().handle())
            .await
            .expect("the exec runs");
        {
            let calls = fake.exec_calls.lock().unwrap();
            // The far side gets values; ${HOME} is the remote shell's to expand, untouched.
            assert_eq!(
                calls[0].argv,
                [
                    "show",
                    "--token=s3cr3t-token-value",
                    "plain-default",
                    "${HOME}"
                ]
            );
            assert!(calls[0].env.contains(&(
                "REDISCLI_AUTH".to_string(),
                "s3cr3t-token-value".to_string()
            )));
        }
        // The value never comes back: the fake streams 3-byte chunks, so the secret is
        // split across chunks on both streams and is masked anyway.
        assert!(!out.output.contains("s3cr3t-token-value"), "{}", out.output);
        assert!(
            out.output
                .contains(&format!("token={} done", swiss_host::mask::MASK)),
            "{}",
            out.output
        );
        assert!(
            out.output
                .contains(&format!("echoed {}", swiss_host::mask::MASK)),
            "{}",
            out.output
        );
    }

    #[test]
    fn every_remote_action_runs_in_its_targets_lane() {
        let (system, _fake) = system_with_fake();
        let registry = Arc::new(swiss_host::services::action::ActionRegistry::new());
        register_all(system, &registry).expect("registered");
        let input = json!({ "target": "dev", "argv": ["true"] });
        for name in [
            "remote.exec",
            "remote.sync",
            "remote.pull",
            "remote.cat",
            "remote.write",
        ] {
            let action = registry.get(name).expect(name);
            assert_eq!(action.lane(&input), RunLane::Remote("dev".into()), "{name}");
        }
    }

    #[tokio::test]
    async fn a_missing_vault_reference_is_refused_before_anything_runs() {
        let (system, fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        let input = json!({ "target": "dev", "env": { "PASS": "${secret://remote-exec-nowhere}" }, "argv": ["true"] });
        // Validation is syntax only: jobs.json validates at save AND drops a failing
        // definition at boot, so a job naming a secret stored later must survive both.
        assert!(action.validate_input(&input).is_ok());
        let err = action
            .execute(&input, CancelSource::new().handle())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("env.PASS") && err.contains("secret://remote-exec-nowhere"),
            "{err}"
        );
        assert!(
            fake.exec_calls.lock().unwrap().is_empty(),
            "nothing reached the transport"
        );
    }

    #[test]
    fn a_malformed_reference_is_refused_by_validation_naming_its_field() {
        let (system, _fake) = system_with_fake();
        let action = RemoteExecAction::new(system);
        let err = action
            .validate_input(&json!({ "target": "dev", "argv": ["echo", "${secret://Bad_Name}"] }))
            .unwrap_err();
        assert!(err.contains("argv[1]"), "{err}");
        assert!(action
            .validate_input(
                &json!({ "target": "dev", "cwd": "${secret://x:/srv/app}", "argv": ["ls"] })
            )
            .is_ok());
    }

    #[tokio::test]
    async fn the_live_buffer_is_masked_too() {
        // What /api/runs/{id}/output streams to the panel and the CLI while the run is live.
        plant_secret("remote-exec-live", "live-secret-value").await;
        let (system, fake) = system_with_fake();
        fake.program(
            "leak",
            FakeProgram {
                argv0: "leak".into(),
                stdout: b"a live-secret-value b\nend\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        crate::actions::register_all(system.clone(), &system.services().actions).unwrap();
        let submitted = match system.services().runs.submit(SubmitRequest {
            owner: REMOTE_OWNER.into(),
            label: "test".into(),
            action_type: "remote.exec".into(),
            input: json!({ "target": "dev", "argv": ["leak", "${secret://remote-exec-live}"] }),
            timeout_ms: 30_000,
            queue_if_busy: false,
            actor: "test".to_string(),
        }) {
            Ok(s) => s,
            Err(err) => panic!("submit refused: {err:?}"),
        };
        let run_id = submitted.run_id;
        let _ = submitted.done.await;
        let (_, chunk) = system
            .services()
            .runs
            .output(run_id, 0, u32::MAX as usize)
            .expect("the run's output");
        assert!(!chunk.text.contains("live-secret-value"), "{}", chunk.text);
        assert!(
            chunk
                .text
                .contains(&format!("a {} b", swiss_host::mask::MASK)),
            "{}",
            chunk.text
        );
    }
}
