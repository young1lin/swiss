// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

//! `POST /api/llm/agents/run/stream` (SPEC §llm.api): one task for a builtin agent with
//! nobody at the panel - what `swiss agent run` sends. A script hands a builtin agent a task
//! over a directory, and the agent works through it with swiss's file tools:
//! - read, glob and grep reach the working directory;
//! - write and edit reach only the write directories the request names, and never a
//!   dot-named file or directory below one of them;
//! - no shell, no delegate, no memory tools.
//!
//! The run files under task `agent.run` (its label as subject), so History shows it live
//! while it works and whole after. The stream is the Test stream's frames. A reader that goes
//! away cancels, and so does `agents/test/cancel` by the run's id. No clock and no budget cut
//! the run short: it runs as long as its work takes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::Response;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use swiss_host::reply::admin_error;
use swiss_llm::agent::{AgentConfig, LoopOutcome, Runtime, ToolAnswer, ToolSpec, Toolbox};
use swiss_llm::history::{context, within, CancelFlag, RunContext, RunRecorder, Steer};
use swiss_llm::provider::Chat;
use swiss_llm::LlmState;

use crate::llm_api::{awaited_sink, bad, body_of, clip, field, refuse, sse_of, sse_send, SSE_FRAMES, WORKDIR};
use crate::llm_tools::{pinned, pinned_new, plain_path, read_schema, read_tool, FsTools};

/// The task key a run files under, and its telemetry consumer.
pub(crate) const TASK: &str = "agent.run";
/// A run's task text: room for a long brief, far below the body limit.
pub(crate) const PROMPT_MAX: usize = 200_000;
/// The subject a run files under.
const LABEL_MAX: usize = 200;
/// A label taken from the task's first line keeps this many characters.
const LABEL_FROM_PROMPT: usize = 120;
const WRITE_DIRS_MAX: usize = 16;
/// The tools a run offers, in catalog order; the agent's own selection narrows them.
pub(crate) const OFFER: [&str; 5] = ["read", "write", "edit", "glob", "grep"];

/// What one run works with, checked before anything is reserved or asked.
#[derive(Debug)]
pub(crate) struct RunPlan {
    /// The working directory, canonical: what read, glob and grep reach.
    pub(crate) root: PathBuf,
    /// The write directories, canonical, each inside the root.
    pub(crate) writable: Vec<PathBuf>,
    pub(crate) taken: Vec<&'static str>,
    pub(crate) label: String,
    pub(crate) prompt: String,
}

/// The agent a run asks: the one named, or the agent added last (never a db agent, see
/// [swiss_llm::LlmConfig::newest_agent]). Only a builtin agent runs: a chat agent has no
/// tools, and a spawned one brings its own instead of these.
pub(crate) fn agent_of(state: &LlmState, named: Option<String>) -> Result<AgentConfig, String> {
    let name = match named {
        Some(name) => name,
        None if !state.serving() => return Err("the llm plugin is off: enable it to run agents".into()),
        None => state
            .newest_agent()
            .ok_or("no agent is named and the llm plugin has none to fall back to; add a builtin agent on the Agents page")?,
    };
    let agent = state.agent(&name)?;
    match agent.runtime {
        Runtime::Builtin => Ok(agent),
        Runtime::Db => Err(format!("{name} is a DB agent: it serves the Data page alone")),
        other => Err(format!(
            "{name} is a {} agent; a run needs a builtin agent, which works through swiss's own tools - name one",
            other.as_str()
        )),
    }
}

impl RunPlan {
    pub(crate) fn of(agent: &AgentConfig, body: &Map<String, Value>, home: &Path) -> Result<RunPlan, String> {
        let prompt = field(body, "prompt")?.ok_or("prompt is required: the task the agent works on")?;
        if prompt.chars().count() > PROMPT_MAX {
            return Err(format!("prompt: at most {PROMPT_MAX} characters"));
        }
        let cwd = field(body, "cwd")?.ok_or("cwd is required: the directory the run works in")?;
        let root = pinned(home, &cwd).map_err(|why| {
            if why.contains("outside") { format!("cwd must be inside the home directory ({})", plain_path(home)) } else { format!("cwd: {why}") }
        })?;
        if !root.is_dir() {
            return Err("cwd must name a directory".into());
        }
        let writable = write_dirs(&root, body.get("write"))?;
        let offered = agent.narrow(&OFFER);
        let mut taken = offered.clone();
        let selected = body.get("tools").filter(|v| !v.is_null());
        let listed = selected.is_some();
        if let Some(selected) = selected {
            let selected = selected.as_array().ok_or("tools must be a list of tool names")?;
            let mut names = Vec::new();
            for item in selected {
                let name = item.as_str().ok_or("tools must be a list of tool names")?;
                if !OFFER.contains(&name) {
                    return Err(format!("a run offers {}; not {name:?}", crate::llm_api::spoken(&OFFER)));
                }
                if !offered.contains(&name) {
                    return Err(format!("this agent does not take tool {name:?}"));
                }
                if names.contains(&name) {
                    return Err(format!("tools lists {name:?} twice"));
                }
                names.push(name);
            }
            taken.retain(|name| names.contains(name));
        }
        if writable.is_empty() {
            if listed && taken.iter().any(|name| matches!(*name, "write" | "edit")) {
                return Err("write and edit need a write directory".into());
            }
            // Without one the run reads and changes nothing.
            taken.retain(|name| !matches!(*name, "write" | "edit"));
        }
        let label = match field(body, "label")? {
            Some(label) => clip(&label, LABEL_MAX),
            None => clip(prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim(), LABEL_FROM_PROMPT),
        };
        Ok(RunPlan { root, writable, taken, label, prompt })
    }

    /// The system prompt's second half: what the run would otherwise spend tool calls on
    /// (where it works, the repository, the day, which agent it is) and what its tools reach.
    pub(crate) fn environment(&self, agent: &AgentConfig, chat: &Chat) -> String {
        let facts = [
            format!("- {WORKDIR}{}", plain_path(&self.root)),
            format!("- Git repository: {}", crate::llm_api::repository(&self.root).unwrap_or_else(|| "none".into())),
            format!("- Platform: {} {}", std::env::consts::OS, std::env::consts::ARCH),
            format!("- Today's date: {}", chrono::Local::now().format("%Y-%m-%d (%A)")),
            format!("- You are the agent {}, on model {} from provider {}", agent.name, chat.model, chat.provider),
        ];
        format!(
            "{}\nThese facts were taken as this run began.\n{}\n\n# Tools\n{}",
            swiss_llm::conversation::SNAPSHOT_HEADING,
            facts.join("\n"),
            self.notes(),
        )
    }

    fn notes(&self) -> String {
        let mut text = String::from(
            "This run is headless: nobody reads along and nobody answers a question. Do the whole task, then close with a short summary of what you did.",
        );
        if self.taken.is_empty() {
            text.push_str(" You have no tools in this run: answer from what the task itself gives.");
            return text;
        }
        let looks: Vec<&str> = ["read", "glob", "grep"].into_iter().filter(|name| self.taken.contains(name)).collect();
        if !looks.is_empty() {
            let verb = if looks.len() == 1 { "works" } else { "work" };
            text.push_str(&format!(
                " {} {verb} inside the working directory: a path relative to it, or an absolute one inside it.",
                crate::llm_api::spoken(&looks)
            ));
        }
        let changes: Vec<&str> = ["write", "edit"].into_iter().filter(|name| self.taken.contains(name)).collect();
        if !changes.is_empty() {
            let dirs: Vec<String> = self.writable.iter().map(|dir| format!("\n- {}", shown(&self.root, dir))).collect();
            text.push_str(&format!(
                " {} change real files, and only inside these directories:{}\nBelow them, nothing whose name starts with a dot is writable. Read a file before you change it, and change only what the task asks.",
                crate::llm_api::spoken(&changes),
                dirs.concat(),
            ));
        } else {
            text.push_str(" You cannot change any file in this run.");
        }
        text.push_str(" Every call carries a description: one short sentence, in the user's language, saying why you are making it now.");
        text
    }
}

/// The write directories a body names, each resolved against the root (an absolute one
/// taken as given), proven inside it and existing.
fn write_dirs(root: &Path, value: Option<&Value>) -> Result<Vec<PathBuf>, String> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("write must be a list of directories".into()),
    };
    if items.len() > WRITE_DIRS_MAX {
        return Err(format!("write: at most {WRITE_DIRS_MAX} directories"));
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for item in items {
        let dir = item.as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("write must be a list of directories")?;
        let full = pinned(root, dir).map_err(|why| {
            if why.contains("outside") { format!("write: {dir} is outside the working directory") } else { format!("write: {why}") }
        })?;
        if !full.is_dir() {
            return Err(format!("write: {dir} is not a directory"));
        }
        if !dirs.contains(&full) {
            dirs.push(full);
        }
    }
    Ok(dirs)
}

/// A directory as the model and the reader see it: relative to the root where it can be.
fn shown(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => plain_path(dir),
    }
}

/// A run's toolbox: read over the root, the file toolbox's glob and grep over it, and its
/// write and edit only once the target proves to sit in a write directory.
pub(crate) struct RunTools {
    root: PathBuf,
    writable: Vec<PathBuf>,
    taken: Vec<&'static str>,
    files: FsTools,
}

impl RunTools {
    pub(crate) fn new(root: &Path, writable: &[PathBuf], taken: &[&'static str]) -> RunTools {
        RunTools { root: root.to_path_buf(), writable: writable.to_vec(), taken: taken.to_vec(), files: FsTools::new(root, taken) }
    }

    fn dirs(&self) -> String {
        self.writable.iter().map(|dir| shown(&self.root, dir)).collect::<Vec<_>>().join(", ")
    }

    /// Whether a write or edit may change what `args` names. A missing path passes: the
    /// tool itself says what it needs.
    fn may_change(&self, name: &str, args: &Value) -> Result<(), String> {
        let Some(path) = args.get("path").and_then(Value::as_str) else { return Ok(()) };
        let full = if name == "write" { pinned_new(&self.root, path)? } else { pinned(&self.root, path)? };
        let Some(dir) = self.writable.iter().find(|dir| full.starts_with(dir)) else {
            return Err(format!("{path} is outside the write directories ({}); this run changes files only there", self.dirs()));
        };
        let below = full.strip_prefix(dir).unwrap_or(&full);
        if below.components().any(|part| part.as_os_str().to_string_lossy().starts_with('.')) {
            return Err(format!("{path} is, or sits under, a dot-named file or directory; this run never writes those"));
        }
        Ok(())
    }
}

#[async_trait]
impl Toolbox for RunTools {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        if self.taken.contains(&"read") {
            specs.push(ToolSpec {
                name: "read".into(),
                description: "Read one file under the root, at most 64 KiB a read; from and lines read a long file a window at a time. Credential files refused; what it serves is redacted.".into(),
                schema: read_schema("path relative to the root, or absolute when inside it"),
            });
        }
        let dirs = self.dirs();
        for mut spec in self.files.specs() {
            if spec.name == "write" || spec.name == "edit" {
                spec.description.push_str(&format!(" Only inside the write directories ({dirs}), and never a dot-named file or directory below them."));
            }
            specs.push(spec);
        }
        specs
    }

    async fn call(&mut self, name: &str, args: &Value) -> ToolAnswer {
        match name {
            "read" if self.taken.contains(&"read") => {
                let text = read_tool(&self.root, args);
                ToolAnswer { ok: !text.starts_with("error:"), text }
            }
            "write" | "edit" if self.taken.contains(&name) => match self.may_change(name, args) {
                Ok(()) => self.files.call(name, args).await,
                Err(why) => ToolAnswer { text: format!("error: {why}"), ok: false },
            },
            // glob and grep, or the unknown-tool answer naming what this run takes.
            _ => self.files.call(name, args).await,
        }
    }
}

/// The closing `done` frame's body: the whole answer, never clipped - a script reads it.
fn answer_of(driven: Result<LoopOutcome, String>, chat: &Chat, run: &RunRecorder, started: Instant) -> Value {
    let ms = started.elapsed().as_millis() as u64;
    let mut answer = match driven {
        Ok(outcome) if outcome.cancelled => json!({
            "ok": false, "cancelled": true, "error": "cancelled", "ms": ms, "turns": outcome.turns,
            "tokensIn": outcome.tokens.input, "tokensOut": outcome.tokens.output,
        }),
        Ok(outcome) => json!({
            "ok": true, "text": outcome.summary, "cut": outcome.cut, "ms": ms, "turns": outcome.turns,
            "provider": chat.provider, "model": chat.model,
            "tokensIn": outcome.tokens.input, "tokensCacheRead": outcome.tokens.cache_read,
            "tokensCacheWrite": outcome.tokens.cache_creation, "tokensOut": outcome.tokens.output,
        }),
        // The loop noted and filed its own failure; the answer says it.
        Err(err) => json!({ "ok": false, "error": err, "ms": ms }),
    };
    answer["runId"] = json!(run.id());
    answer
}

/// POST /api/llm/agents/run/stream {agent?, prompt, cwd, write?, tools?, label?}: one
/// headless run (the module's doc says what it reaches). Refusals come before the stream:
/// a 400 naming what is wrong, 503 when the run row cannot be reserved.
pub(crate) async fn stream(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "prompt", "cwd", "write", "tools", "label"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let agent = match field(&body, "agent").and_then(|named| agent_of(&state, named)) {
        Ok(agent) => agent,
        Err(why) => return bad(why),
    };
    let plan = match RunPlan::of(&agent, &body, &swiss_core::paths::home_dir()) {
        Ok(plan) => plan,
        Err(why) => return bad(why),
    };
    let config = state.config();
    let chat = match agent.chat(&config.providers) {
        Ok(chat) => chat,
        Err(err) => return bad(err),
    }
    .recording(state.history(), Some(&agent.name));
    let session = state.session(&agent, TASK);
    let base = agent.system_prompt().map_or_else(|| swiss_llm::agent::DEFAULT_SYSTEM_PROMPT.to_string(), |s| s.into_owned());
    let system = format!("{base}\n\n{}", plan.environment(&agent, &chat));
    // The scope is captured by the recorder as it is made, and by every call inside it.
    let scope = RunContext { task: Some(TASK.into()), subject: Some(plan.label.clone()), ..context() };
    let mut run = within(scope.clone(), async { chat.run_recorder(agent.runtime.as_str()) }).await;
    // The row is reserved before the first ask: a refused reservation stops the ask.
    run.reserve().await;
    if let Some(why) = run.refusal() {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, &format!("the run could not be reserved: {why}"));
    }
    let cancel = CancelFlag::new();
    run.set_cancel(cancel.clone());
    // A seat of its own, keyed by the run, so `agents/test/cancel` finds it by id and the
    // plugin's stop waits for it - while runs of one agent go side by side, and beside its
    // Test. Nobody steers a headless run: its inbox is closed from the start.
    let seat = run.id().map(|id| (format!("{}\u{1f}run:{id}", agent.name), id));
    if let Some((key, id)) = &seat {
        let steer = Steer::new();
        steer.close();
        if !state.seat(key, *id, cancel.clone(), steer) {
            return refuse("busy", "this run is already streaming");
        }
    }
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    sse_send(&tx, "run", json!({
        "runId": run.id(), "agent": agent.name, "provider": chat.provider, "model": chat.model,
        "cwd": plain_path(&plan.root),
        "write": plan.writable.iter().map(|dir| shown(&plan.root, dir)).collect::<Vec<_>>(),
        "tools": plan.taken, "label": plan.label,
    }));
    let started = Instant::now();
    tokio::spawn(within(scope, async move {
        let mut tools = RunTools::new(&plan.root, &plan.writable, &plan.taken);
        let mut on_event = awaited_sink(&tx, &cancel);
        let driven = {
            let looped = swiss_llm::agent::run_loop_continued(
                &chat, &system, &[], &plan.prompt, &mut tools, agent.allows_tools(), &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            // A reader that went away while nothing flows still cancels; the drive is then
            // awaited to its end, so it files its honest tail.
            loop {
                tokio::select! {
                    _ = tx.closed(), if !cancel.requested() => cancel.request(),
                    outcome = &mut looped => break outcome,
                }
            }
        };
        let answer = answer_of(driven, &chat, &run, started);
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        if let Some((key, _)) = seat {
            state.unseat(&key);
        }
    }));
    sse_of(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_llm::{LlmConfig, LlmState};

    fn tree() -> PathBuf {
        tree_at(&swiss_core::paths::test_home())
    }

    fn tree_at(base: &Path) -> PathBuf {
        let root = base.join(format!("agent-run-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(root.join("docs/skill/.state")).expect("dirs");
        std::fs::create_dir_all(root.join("src")).expect("dirs");
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("file");
        std::fs::write(root.join("docs/skill/guide.md"), "# Guide\nold line\n").expect("file");
        std::fs::write(root.join("docs/skill/.state/queue.jsonl"), "{}\n").expect("file");
        root
    }

    fn state(agents: Value) -> std::sync::Arc<LlmState> {
        let state = LlmState::new();
        state
            .apply(
                LlmConfig::parse(&json!({
                    "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] } },
                    "agents": agents,
                }))
                .expect("config"),
            );
        state
    }

    fn body(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn the_agent_is_the_named_builtin_or_the_newest_and_never_a_db_or_spawned_one() {
        let state = state(json!({
            "first": { "provider": "local" },
            "talk": { "runtime": "chat", "provider": "local" },
            "data": { "runtime": "db", "provider": "local" },
        }));
        assert_eq!(agent_of(&state, Some("first".into())).expect("builtin").name, "first");
        // The newest that serves tasks is the chat one, which cannot run: the refusal says so.
        assert!(agent_of(&state, None).unwrap_err().contains("needs a builtin agent"));
        assert!(agent_of(&state, Some("data".into())).unwrap_err().contains("DB agent"));
        assert!(agent_of(&state, Some("ghost".into())).unwrap_err().contains("no agent named"));
        let only = self::state(json!({ "solo": { "provider": "local" }, "data": { "runtime": "db", "provider": "local" } }));
        assert_eq!(agent_of(&only, None).expect("newest builtin").name, "solo", "a db agent is never the fallback");
        let off = LlmState::new();
        assert!(agent_of(&off, None).unwrap_err().contains("plugin is off"));
    }

    #[test]
    fn a_plan_reads_the_root_and_writes_only_where_it_is_told() {
        let root = tree();
        let home = swiss_core::paths::test_home();
        let state = state(json!({ "worker": { "provider": "local" } }));
        let agent = state.agent("worker").expect("agent");
        let cwd = root.to_string_lossy().to_string();
        let plan = RunPlan::of(&agent, &body(json!({ "prompt": "\n  sync the docs\nmore", "cwd": cwd, "write": ["docs/skill", "docs/skill/"] })), &home).expect("plan");
        assert_eq!(plan.taken, OFFER.to_vec());
        assert_eq!(plan.writable.len(), 1, "one directory named twice is one");
        assert_eq!(plan.label, "sync the docs", "the first line that says something");
        // No write directory: nothing that changes a file is offered, and asking for it is refused.
        let read_only = RunPlan::of(&agent, &body(json!({ "prompt": "look", "cwd": cwd })), &home).expect("plan");
        assert_eq!(read_only.taken, vec!["read", "glob", "grep"]);
        let asked = RunPlan::of(&agent, &body(json!({ "prompt": "x", "cwd": cwd, "tools": ["read", "edit"] })), &home);
        assert!(asked.unwrap_err().contains("need a write directory"));
        let narrowed = RunPlan::of(&agent, &body(json!({ "prompt": "x", "cwd": cwd, "write": ["docs/skill"], "tools": ["read", "edit"] })), &home).expect("plan");
        assert_eq!(narrowed.taken, vec!["read", "edit"]);
        let refuse = |value: Value| RunPlan::of(&agent, &body(value), &home).unwrap_err();
        assert!(refuse(json!({ "prompt": "x", "cwd": cwd, "tools": ["bash"] })).contains("a run offers"));
        assert!(refuse(json!({ "prompt": "x", "cwd": cwd, "write": [".."] })).contains("outside the working directory"));
        assert!(refuse(json!({ "prompt": "x", "cwd": cwd, "write": ["src/a.rs"] })).contains("not a directory"));
        assert!(refuse(json!({ "prompt": "x", "cwd": cwd, "write": "docs" })).contains("list of directories"));
        assert!(refuse(json!({ "cwd": cwd })).contains("prompt is required"));
        assert!(refuse(json!({ "prompt": "x" })).contains("cwd is required"));
        assert!(refuse(json!({ "prompt": "x".repeat(PROMPT_MAX + 1), "cwd": cwd })).contains("at most"));
        let outside = if cfg!(windows) { "C:\\Windows" } else { "/etc" };
        assert!(refuse(json!({ "prompt": "x", "cwd": outside })).contains("inside the home directory"));
        // An agent that took only read offers only read.
        let picky = self::state(json!({ "reader": { "provider": "local", "tools": ["read", "grep"] } }));
        let reader = picky.agent("reader").expect("agent");
        let plan = RunPlan::of(&reader, &body(json!({ "prompt": "x", "cwd": cwd, "write": ["docs/skill"] })), &home).expect("plan");
        assert_eq!(plan.taken, vec!["read", "grep"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn writes_land_in_the_write_directories_and_never_under_a_dot_name() {
        let root = tree();
        let home = swiss_core::paths::test_home();
        let state = state(json!({ "worker": { "provider": "local" } }));
        let agent = state.agent("worker").expect("agent");
        let plan = RunPlan::of(&agent, &body(json!({ "prompt": "x", "cwd": root.to_string_lossy(), "write": ["docs/skill"] })), &home).expect("plan");
        let mut tools = RunTools::new(&plan.root, &plan.writable, &plan.taken);
        let names: Vec<String> = tools.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, OFFER.map(String::from).to_vec());
        assert!(tools.specs().iter().find(|s| s.name == "write").expect("write").description.contains("docs/skill"));

        let read = tools.call("read", &json!({ "path": "src/a.rs" })).await;
        assert!(read.ok && read.text.contains("fn a"), "reads reach the whole root: {}", read.text);
        let edited = tools.call("edit", &json!({ "path": "docs/skill/guide.md", "old_string": "old line", "new_string": "new line" })).await;
        assert!(edited.ok, "{}", edited.text);
        let made = tools.call("write", &json!({ "path": "docs/skill/area/new.md", "content": "# New\n" })).await;
        assert!(made.ok, "{}", made.text);
        assert!(root.join("docs/skill/area/new.md").exists());

        let outside = tools.call("edit", &json!({ "path": "src/a.rs", "old_string": "fn a", "new_string": "fn b" })).await;
        assert!(!outside.ok && outside.text.contains("outside the write directories"), "{}", outside.text);
        let source = tools.call("write", &json!({ "path": "src/new.rs", "content": "x" })).await;
        assert!(!source.ok && !root.join("src/new.rs").exists(), "{}", source.text);
        let state_file = tools.call("write", &json!({ "path": "docs/skill/.state/queue.jsonl", "content": "x" })).await;
        assert!(!state_file.ok && state_file.text.contains("dot-named"), "{}", state_file.text);
        let new_dot = tools.call("write", &json!({ "path": "docs/skill/.tmp/scratch.md", "content": "x" })).await;
        assert!(!new_dot.ok && !root.join("docs/skill/.tmp").exists(), "{}", new_dot.text);
        let dot_file = tools.call("write", &json!({ "path": "docs/skill/.hidden.md", "content": "x" })).await;
        assert!(!dot_file.ok, "{}", dot_file.text);
        let climbing = tools.call("write", &json!({ "path": "docs/skill/../../src/x.rs", "content": "x" })).await;
        assert!(!climbing.ok && !root.join("src/x.rs").exists(), "{}", climbing.text);
        let absolute = root.join("docs/skill/guide.md").to_string_lossy().to_string();
        let by_absolute = tools.call("edit", &json!({ "path": absolute, "old_string": "new line", "new_string": "newer line" })).await;
        assert!(by_absolute.ok, "an absolute path inside a write directory is fine: {}", by_absolute.text);
        assert_eq!(std::fs::read_to_string(root.join("src/a.rs")).expect("read"), "fn a() {}\n", "source untouched");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_snapshot_names_the_write_directories_and_says_the_run_is_headless() {
        let root = tree();
        let home = swiss_core::paths::test_home();
        let state = state(json!({ "worker": { "provider": "local" } }));
        let agent = state.agent("worker").expect("agent");
        let chat = agent.chat(&state.config().providers).expect("chat");
        let plan = RunPlan::of(&agent, &body(json!({ "prompt": "x", "cwd": root.to_string_lossy(), "write": ["docs/skill"] })), &home).expect("plan");
        let text = plan.environment(&agent, &chat);
        assert!(text.contains(WORKDIR), "{text}");
        assert!(text.contains("headless"), "{text}");
        assert!(text.contains("\n- docs/skill"), "{text}");
        assert!(text.contains("You are the agent worker, on model tiny"), "{text}");
        let read_only = RunPlan::of(&agent, &body(json!({ "prompt": "x", "cwd": root.to_string_lossy() })), &home).expect("plan");
        assert!(read_only.environment(&agent, &chat).contains("cannot change any file"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One OpenAI stream round that asks for one tool call.
    fn tool_round(id: &str, name: &str, arguments: Value) -> String {
        let call = json!({ "id": id, "object": "chat.completion.chunk", "model": "tiny", "choices": [{ "index": 0,
            "delta": { "role": "assistant", "tool_calls": [{ "index": 0, "id": format!("call_{id}"), "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() } }] }, "finish_reason": null }] });
        let end = json!({ "id": id, "object": "chat.completion.chunk", "model": "tiny",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11 } });
        format!("data: {call}

data: {end}

data: [DONE]

")
    }

    fn answer_round(text: &str) -> String {
        let say = json!({ "id": "a", "object": "chat.completion.chunk", "model": "tiny",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text }, "finish_reason": null }] });
        let end = json!({ "id": "a", "object": "chat.completion.chunk", "model": "tiny",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14 } });
        format!("data: {say}

data: {end}

data: [DONE]

")
    }

    /// The route end to end against a scripted provider: the edit inside the write directory
    /// lands, the write under a dot-named directory and the one outside are refused to the
    /// model (the run goes on), and History holds the run under its task and label.
    #[tokio::test]
    async fn a_run_streams_its_steps_changes_only_the_write_directories_and_files_in_history() {
        use crate::llm_api::tests::{call, call_text, events_of, mount, sse_wire_capturing};
        swiss_core::paths::test_home();
        let root = tree_at(&swiss_core::paths::home_dir());
        let (base, asks) = sse_wire_capturing(vec![
            tool_round("1", "edit", json!({ "path": "docs/skill/guide.md", "old_string": "old line", "new_string": "new line", "description": "update the guide" })),
            tool_round("2", "write", json!({ "path": "docs/skill/.state/queue.jsonl", "content": "x", "description": "touch the queue" })),
            tool_round("3", "write", json!({ "path": "src/b.rs", "content": "x", "description": "add code" })),
            answer_round("synced the guide"),
        ])
        .await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "worker": { "provider": "local", "maxTurns": 8 } },
        })).expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("agent-run-history-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);

        let refused = call(&app, "POST", "/api/llm/agents/run/stream", Some(json!({ "prompt": "x", "cwd": root.to_string_lossy(), "write": ["../.."] }))).await;
        assert_eq!(refused.0, StatusCode::BAD_REQUEST, "{}", refused.1);
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/agents/run/stream", Some(json!({
            "prompt": "sync the skill docs", "cwd": root.to_string_lossy(), "write": ["docs/skill"], "label": "skill sync",
        }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let (_, run) = events.iter().find(|(name, _)| name == "run").expect("a run frame");
        assert_eq!((&run["agent"], &run["write"]), (&json!("worker"), &json!(["docs/skill"])), "{run}");
        let results: Vec<bool> = events.iter().filter(|(name, _)| name == "toolResult").map(|(_, data)| data["ok"] == true).collect();
        assert_eq!(results, vec![true, false, false], "{text}");
        let (_, done) = events.iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!((&done["ok"], &done["text"], &done["turns"]), (&json!(true), &json!("synced the guide"), &json!(4)), "{done}");
        assert_eq!(std::fs::read_to_string(root.join("docs/skill/guide.md")).expect("read"), "# Guide
new line
");
        assert_eq!(std::fs::read_to_string(root.join("docs/skill/.state/queue.jsonl")).expect("read"), "{}
", "a dot-named directory stays");
        assert!(!root.join("src/b.rs").exists(), "nothing lands outside the write directories");

        // The model was told where it may write, and offered no tool past the five.
        let first = asks.lock().expect("asks")[0].clone();
        assert!(first.contains("headless") && first.contains("docs/skill"), "{first}");
        let ask: Value = serde_json::from_str(&first[first.find('{').expect("json")..]).expect("ask json");
        let mut offered: Vec<&str> = ask["tools"].as_array().expect("tools").iter().filter_map(|tool| tool["function"]["name"].as_str()).collect();
        offered.sort_unstable();
        assert_eq!(offered, vec!["edit", "glob", "grep", "read", "write"]);

        let run_id = done["runId"].as_i64().expect("a run id");
        let (status, runs) = call(&app, "GET", "/api/llm/history/runs?agent=worker", None).await;
        assert_eq!(status, StatusCode::OK, "{runs}");
        let row = runs.to_string();
        assert!(row.contains(&format!("\"id\":{run_id}")) && row.contains("\"task\":\"agent.run\"") && row.contains("\"subject\":\"skill sync\""), "{row}");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(&root);
    }
}
