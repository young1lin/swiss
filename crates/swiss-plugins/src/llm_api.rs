/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The /api/llm routes (SPEC §llm.api): what the three pages show beside the row they
//! edit, and the Test each of them offers. The row itself travels through the host's
//! /api/plugins/llm/config (a GET for the revision, the CAS PUT); nothing here writes it.
//!
//! A Test (and a provider's model listing) asks the SAVED row, never a body's endpoint: the
//! request carries names, not URLs or keys. A provider's refusal is the answer (`ok: false` and why, 200), not a failure
//! of the route; a name the row lacks is a 400. Keys appear only as the references the row
//! holds, and every error a provider sends back is scrubbed of the resolved key before it
//! gets here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use swiss_host::services::RuntimeServices;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use swiss_host::reply::{admin_error, admin_json};
use swiss_llm::agent::{db_tools_on, AgentConfig, LoopOutcome, Runtime, DB_KINDS};
use swiss_llm::conversation::{self, LaneError};
use swiss_llm::embedding::EmbedClient;
use swiss_llm::history::RunRecorder;
use swiss_llm::provider::{self, Chat, ModelEntry, ModelKind};
use swiss_llm::telemetry::{self, Session};
use swiss_llm::LlmState;

/// A provider Test waits this long for its answer.
const ASK_TIMEOUT: Duration = Duration::from_secs(90);
/// A spawned runtime's `--version` check.
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
/// The answer's ceiling for a Test: room for a reasoning model to think before it speaks.
const TEST_TOKENS: u32 = 1024;
/// How much of an answer a Test shows.
const ANSWER_MAX: usize = 400;
const PROMPT_MAX: usize = 2000;
const DEFAULT_PROMPT: &str = "Reply with one short sentence that names the model you are.";
/// The live Test's event channel: bounded, so a reader that went away cannot
/// pile frames without end. Delivery is awaited - backpressure under a full
/// channel, never a skipped frame - and a reader that disappears cancels the
/// run; the closing frame always waits its turn.
pub(crate) const SSE_FRAMES: usize = 32;

/// The /api/llm routes over the plugin's own stores. The caller passes the ConsoleStore the
/// plugin serves on the bus, never a fresh one: a second store on the same directory would hold
/// a second write lock, and its saves would publish unordered against the first store's.
pub fn mount_with_analysis(state: Arc<LlmState>, services: Arc<RuntimeServices>, analysis: Arc<crate::analysis::AnalysisStore>, consoles: Arc<crate::db_consoles::ConsoleStore>) -> Router {
    Router::new()
        .route("/api/llm/status", get(status))
        .route("/api/llm/providers/test", post(provider_test))
        .route("/api/llm/providers/models", post(provider_models))
        .route("/api/llm/embedding/test", post(embedding_test))
        .route("/api/llm/agents/test", post(agent_test))
        .route("/api/llm/agents/test/stream", post(agent_test_stream))
        .route("/api/llm/agents/test/cancel", post(agent_test_cancel))
        .route("/api/llm/agents/test/steer", post(agent_test_steer))
        .route("/api/llm/agents/test/steer/withdraw", post(agent_test_withdraw))
        .route("/api/llm/db/stream", post(crate::llm_db::stream))
        .route("/api/llm/db/conversation", get(crate::llm_db::open_conversation))
        .route("/api/llm/db/conversations", get(crate::llm_db::conversations))
        .route("/api/llm/db/context", get(crate::llm_db::context))
        .route("/api/llm/db/reopen", post(crate::llm_db::reopen))
        .route("/api/llm/db/compact", post(crate::llm_db::compact))
        .route("/api/llm/db/memory", get(crate::llm_db_memory::notes))
        .route("/api/llm/db/memory/note", get(crate::llm_db_memory::note))
        .route("/api/llm/db/memory/save", post(crate::llm_db_memory::save))
        .route("/api/llm/db/memory/delete", post(crate::llm_db_memory::delete))
        .route("/api/llm/telemetry/test", post(telemetry_test))
        .route("/api/llm/history/calls", get(history_calls))
        .route("/api/llm/history/calls/{id}/body", get(history_call_body))
        .route("/api/llm/history/runs", get(history_runs))
        .route("/api/llm/history/runs/{id}/turns", get(history_run_turns))
        .route("/api/llm/history/runs/{id}/calls", get(history_run_calls))
        .route("/api/llm/history/runs/{id}/dialogue", get(history_run_dialogue))
        .route("/api/llm/history/runs/{id}/close", post(history_run_close))
        .route("/api/llm/history/turns/{run_id}/{seq}/body", get(history_turn_body))
        .with_state(state)
        .layer(Extension(services))
        .layer(Extension(analysis))
        .layer(Extension(consoles))
}

pub(crate) fn bad(message: impl AsRef<str>) -> Response {
    admin_error(StatusCode::BAD_REQUEST, message.as_ref())
}

pub(crate) fn ok(body: Value) -> Response {
    admin_json(StatusCode::OK, body)
}

/// A JSON object body holding only `allowed` keys; an empty body is an empty object. The
/// helpers below answer the 400's text; the handler turns it into the response.
pub(crate) fn body_of(bytes: &[u8], allowed: &[&str]) -> Result<Map<String, Value>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    let map = match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return Err("the body must be a JSON object".into()),
        Err(err) => return Err(format!("the body is not JSON: {err}")),
    };
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("the body has no {key:?} field"));
    }
    Ok(map)
}

/// A string field, trimmed; blank reads as None.
pub(crate) fn field(body: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn prompt_of(body: &Map<String, Value>) -> Result<String, String> {
    let prompt = field(body, "prompt")?.unwrap_or_else(|| DEFAULT_PROMPT.into());
    if prompt.chars().count() > PROMPT_MAX {
        return Err(format!("prompt: at most {PROMPT_MAX} characters"));
    }
    Ok(prompt)
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// One listed model as the status shows it: what the entry states, the defaults filled
/// (`thinking` true, `image` false), a figure it leaves out null.
fn model_row(m: &ModelEntry) -> Value {
    json!({
        "id": m.id, "kind": m.kind.as_str(), "name": m.name, "contextWindow": m.context_window,
        "maxOutput": m.max_output, "image": m.image, "thinking": m.thinking,
    })
}

fn ms_since(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Every runtime's tool vocabulary, the checkboxes an agent's `tools` picks from.
const RUNTIMES: [Runtime; 5] = [Runtime::Builtin, Runtime::Chat, Runtime::Claude, Runtime::Pi, Runtime::Db];

/// Each runtime's tools; a db tool also says which kind of connection it serves (`on`: sql,
/// mongo or redis), so the Data page names only the four of the connection in front.
fn catalog() -> Value {
    let by_runtime: Map<String, Value> = RUNTIMES
        .iter()
        .map(|r| {
            let tools: Vec<Value> = r
                .tools()
                .iter()
                .map(|t| {
                    let mut tool = json!({ "name": t.name, "summary": t.summary });
                    if *r == Runtime::Db {
                        if let Some(kind) = DB_KINDS.iter().find(|kind| db_tools_on(kind).contains(t)) {
                            tool["on"] = json!(kind);
                        }
                    }
                    tool
                })
                .collect();
            (r.as_str().to_string(), json!(tools))
        })
        .collect();
    Value::Object(by_runtime)
}

/// Each runtime's system prompt when an agent sets none (null: the CLI's own, nothing
/// appended) - what a new agent's sheet shows before there is a row to read it from.
fn default_prompts() -> Value {
    Value::Object(RUNTIMES.iter().map(|r| (r.as_str().to_string(), json!(r.default_prompt()))).collect())
}

/// GET /api/llm/status: what each page shows beside the row - the providers as the row
/// holds them (keys as their references, models with their kinds), the embedding endpoint
/// and whether text leaves the machine, every agent with how it would run and report (its
/// system prompt in force and the tools it takes), every runtime's tool catalog, and the
/// exporter's counters.
async fn status(State(state): State<Arc<LlmState>>) -> Response {
    let config = state.config();
    let telemetry_on = config.telemetry.as_ref().is_some_and(|t| t.enabled);
    let providers: Vec<Value> = config
        .providers
        .values()
        .map(|p| {
            json!({
                "name": p.name,
                "style": p.style.as_str(),
                "baseUrl": p.base_url,
                "models": p.models.iter().map(model_row).collect::<Vec<_>>(),
                "apiKey": p.api_key,
                "proxy": p.proxy,
                "url": p.chat_url(),
            })
        })
        .collect();
    let embedding = config.embedding.as_ref().map(|e| {
        let mut shown = e.to_json();
        shown["fingerprint"] = json!(e.fingerprint());
        shown
    });
    let agents: Vec<Value> = config
        .agents
        .values()
        .map(|a| {
            // Who reports a run: swiss itself for a provider-asking runtime, the child for
            // a Claude Code (it gets the OTEL env), nobody otherwise.
            let reports = match (telemetry_on && a.report, a.runtime) {
                (false, _) => "off",
                (true, Runtime::Builtin | Runtime::Chat | Runtime::Db) => "swiss",
                (true, Runtime::Claude) => "child",
                (true, Runtime::Pi) => "off",
            };
            let mut shown = json!({
                "name": a.name,
                "runtime": a.runtime.as_str(),
                "provider": a.provider,
                "model": a.model,
                "concurrency": a.concurrency,
                "report": a.report,
                "reports": reports,
                // The agent's own prompt (null when unset) and the default sent ahead of it.
                "systemPrompt": a.system_prompt,
                "systemPromptDefault": a.runtime.default_prompt(),
                // The agent's list (null: every tool a task offers) and what that leaves of
                // the runtime's catalog.
                "tools": a.tools,
                "toolsInForce": a.narrow(&a.runtime.tools().iter().map(|t| t.name).collect::<Vec<_>>()),
            });
            if a.runtime.asks_provider() {
                shown["maxTokens"] = json!(a.max_tokens);
                shown["maxContextKiB"] = json!(a.max_context_kib);
                shown["effort"] = json!(a.effort.map(|e| e.as_str()));
                if a.runtime == Runtime::Builtin {
                    // The grant as written, and the stored names it reaches now.
                    shown["secrets"] = json!(a.secrets);
                    shown["secretsInForce"] = json!(swiss_core::secure::secretstore::granted(&a.secrets).into_iter().map(|(name, _)| name).collect::<Vec<_>>());
                }
            } else {
                shown["command"] = json!(a.command);
                match a.program() {
                    Ok(path) => shown["program"] = json!(path.display().to_string()),
                    Err(err) => shown["programError"] = json!(err),
                }
            }
            shown
        })
        .collect();
    ok(json!({
        "serving": state.serving(),
        "providers": providers,
        "embedding": embedding,
        "agents": agents,
        "catalog": catalog(),
        "defaultPrompts": default_prompts(),
        "telemetry": state.reporter().status(),
    }))
}

/// POST /api/llm/providers/test {provider, model?, prompt?}: one prompt to a saved
/// provider, unreported (a provider is no agent), with no system message. The model is one
/// of the provider's chat models, its first when the body names none.
async fn provider_test(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["provider", "model", "prompt"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, model, prompt) = match (field(&body, "provider"), field(&body, "model"), prompt_of(&body)) {
        (Ok(Some(name)), Ok(model), Ok(prompt)) => (name, model, prompt),
        (Ok(None), _, _) => return bad("provider is required"),
        (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => return bad(why),
    };
    let config = state.config();
    let Some(entry) = config.provider(&name) else {
        return bad(format!("the llm plugin has no provider named {name:?}; save the row first"));
    };
    let chat = match entry.pick(model.as_deref(), ModelKind::Chat, "model").and_then(|model| Chat::new(entry, Some(&model), TEST_TOKENS)) {
        Ok(chat) => chat,
        Err(err) => return ok(json!({ "ok": false, "error": err, "ms": 0 })),
    }
    // A panel probe records like any other call (SPEC llm.history): agent None says a
    // provider test, not an agent run.
    .recording(state.history(), None);
    ok(ask_once(&chat, None, &prompt, &Session::disabled()).await)
}

/// POST /api/llm/providers/models {provider}: the saved provider's own model listing, each
/// id with the kind its name suggests, for the panel to offer - nothing is saved here.
async fn provider_models(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["provider"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let name = match field(&body, "provider") {
        Ok(Some(name)) => name,
        Ok(None) => return bad("provider is required"),
        Err(why) => return bad(why),
    };
    let config = state.config();
    let Some(entry) = config.provider(&name) else {
        return bad(format!("the llm plugin has no provider named {name:?}; save the row first"));
    };
    let started = Instant::now();
    match provider::discover(entry).await {
        Ok(models) => {
            let listed: Vec<Value> = models.iter().map(|m| json!({ "id": m.id, "kind": m.kind.as_str() })).collect();
            ok(json!({ "ok": true, "url": entry.models_url(), "models": listed, "ms": ms_since(started) }))
        }
        Err(err) => ok(json!({ "ok": false, "url": entry.models_url(), "error": err, "ms": ms_since(started) })),
    }
}

/// One question with the Test's timeout, answered as `{ok, model, text|error, ms}`.
async fn ask_once(chat: &Chat, system: Option<&str>, prompt: &str, session: &Session) -> Value {
    let started = Instant::now();
    let outcome = tokio::time::timeout(ASK_TIMEOUT, provider::ask(chat, system, prompt, session)).await;
    let ms = ms_since(started);
    match outcome {
        Ok(Ok(text)) => json!({ "ok": true, "model": chat.model, "text": clip(text.trim(), ANSWER_MAX), "ms": ms }),
        Ok(Err(err)) => json!({ "ok": false, "model": chat.model, "error": err, "ms": ms }),
        Err(_) => {
            // The ask was in flight when the deadline hit: no answer path will record,
            // so the wait itself is the round trip's record (SPEC llm.history).
            chat.record_timeout(None, system, prompt, ASK_TIMEOUT.as_millis() as u64);
            json!({ "ok": false, "model": chat.model, "error": format!("no answer within {} s", ASK_TIMEOUT.as_secs()), "ms": ms })
        }
    }
}

/// POST /api/llm/embedding/test {text?}: one query vector from the saved endpoint - whether
/// it answers, how wide, how fast.
async fn embedding_test(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["text"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let text = match field(&body, "text") {
        Ok(text) => text.unwrap_or_else(|| "llm embedding test".into()),
        Err(why) => return bad(why),
    };
    let Some(config) = state.config().embedding.clone() else {
        return bad("embedding is off: name a provider and a model on the Embedding page first");
    };
    let client = match EmbedClient::build(&config) {
        Ok(client) => client,
        Err(err) => return ok(json!({ "ok": false, "model": config.model, "error": err, "ms": 0 })),
    }
    .recording(Some(state.clone()), None);
    let started = Instant::now();
    // One input, prefix on it - the same string the round trip asks and the row records.
    let asked = format!("{}{text}", config.query_prefix);
    let outcome = tokio::time::timeout(client.timeout(), client.embed(vec![asked.clone()])).await;
    let ms = ms_since(started);
    let answer = match outcome {
        Ok(Ok(vectors)) => match vectors.first() {
            Some(vector) if config.dimensions.is_some_and(|d| d != vector.len()) => json!({
                "ok": false, "model": config.model, "dims": vector.len(), "ms": ms,
                "error": format!("the provider returned {} dimensions where {} were asked for", vector.len(), config.dimensions.unwrap_or_default()),
            }),
            Some(vector) => json!({ "ok": true, "model": config.model, "dims": vector.len(), "ms": ms }),
            None => json!({ "ok": false, "model": config.model, "error": "the provider returned no vector", "ms": ms }),
        },
        Ok(Err(err)) => json!({ "ok": false, "model": config.model, "error": err, "ms": ms }),
        Err(_) => {
            // In flight when the deadline hit: file the wait, the round trip happened
            // (SPEC llm.history). The panel test carries no agent name.
            if let Some(history) = state.history() {
                let waited = format!("no answer within {} ms", client.timeout().as_millis());
                history.record_embedding(swiss_llm::history::EmbedCall {
                    provider: &config.provider,
                    model: &config.model,
                    agent: None,
                    texts: std::slice::from_ref(&asked),
                    ms,
                    outcome: Err(waited.as_str()),
                });
            }
            json!({ "ok": false, "model": config.model, "error": format!("no answer within {} ms", client.timeout().as_millis()), "ms": ms })
        }
    };
    ok(answer)
}

/// The snapshot line naming the working directory, in the Test's environment and in a
/// later turn's moved note alike: the last one the dialogue holds is what the model was
/// last told.
const WORKDIR: &str = "Working directory: ";

/// What the Test's tools reach and how to use them, closing its environment snapshot.
/// Without it the model guesses at the pin (users paste absolute paths into prompts)
/// and at what it may do. `secrets` is the vault names (and groups) the agent's grant
/// covers as the conversation begins: the snapshot is replayed as noted, so it holds for
/// the whole conversation.
fn tool_notes(taken: &[&str], secrets: &[(String, String)]) -> String {
    if taken.is_empty() {
        return "You have no tools in this conversation: say what you would need to look at, and answer from what the task itself gives.".into();
    }
    let files: Vec<&str> = ["read", "write", "edit", "glob", "grep"].into_iter().filter(|name| taken.contains(name)).collect();
    let mut text = String::new();
    if !files.is_empty() {
        let verb = if files.len() == 1 { "works" } else { "work" };
        text.push_str(&format!(" {} {verb} inside the working directory: a path relative to it, or an absolute one inside it.", spoken(&files)));
    }
    if taken.contains(&"write") || taken.contains(&"edit") {
        text.push_str(" Writes and edits change real files on the user's machine: read a file before you change it, and change only what the task asks.");
    }
    let (tool, shell) = (crate::llm_shell::TOOL, crate::llm_shell::SHELL);
    if taken.contains(&"bash") { text.push_str(&format!(" {tool} executes general commands in a fresh {shell} session (write {shell} syntax) starting in the working directory; pass workdir (or cwd) to choose another directory, timeoutMs for a deadline, and run_in_background to return a job_id.")); }
    if taken.contains(&"bash") && !secrets.is_empty() {
        let named: Vec<String> = secrets.iter().map(|(name, group)| if group == "default" { name.clone() } else { format!("{name} ({group})") }).collect();
        text.push_str(&format!(" Secrets granted to you (name, and group where not default): {}. Write ${{secret://name}} in a {tool} command where {shell} expands a variable (unquoted or inside double quotes, never single quotes): the value reaches the command through its environment, you never see it, and output masks it. Never print, encode or save a value; to write the reference itself into a file, single-quote it.", named.join(", ")));
    }
    if taken.contains(&"job_output") { text.push_str(&format!(" job_output reads a background {shell} job by job_id; job_kill stops its process tree.")); }
    if taken.contains(&"sync") { text.push_str(" sync validates proposal operations in this test only; they are not applied to project memory."); }
    if taken.contains(&"delegate") { text.push_str(" delegate sends real tasks to a worker using your configured model; the worker answers once without tools."); }
    text.push_str(" Every call carries a description: one short sentence, in the user's language, saying why you are making it now - it is shown beside the tool's name as you work.");
    text.push_str(" Check with your tools rather than guess.");
    text.trim_start().to_string()
}

/// Names as a person lists them: "a", "a and b", "a, b and c".
fn spoken(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// The repository a directory sits in and where its HEAD stands - the branch, or the
/// commit a detached HEAD names - read from the files git keeps: no git process for a
/// fact this small. A worktree's or submodule's `.git` file names its real directory.
fn repository(dir: &std::path::Path) -> Option<String> {
    let top = dir.ancestors().find(|at| at.join(".git").exists())?;
    let dot = top.join(".git");
    let git = if dot.is_file() {
        let named = std::fs::read_to_string(&dot).ok()?;
        let named = std::path::PathBuf::from(named.trim().strip_prefix("gitdir:")?.trim());
        if named.is_absolute() { named } else { top.join(named) }
    } else {
        dot
    };
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let at = match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => format!("branch {branch}"),
        None => format!("detached at {}", head.chars().take(12).collect::<String>()),
    };
    Some(format!("{} ({at})", crate::llm_tools::plain_path(top)))
}

/// The working directory a dialogue last told the model: its snapshot's, or a later
/// user turn's moved note ([WORKDIR] lines, the last one standing).
fn told_directory(system: &str, prior: &[Value]) -> Option<String> {
    let users = prior.iter().filter(|entry| entry["role"] == "user").filter_map(|entry| entry["text"].as_str());
    std::iter::once(system)
        .chain(users)
        .flat_map(str::lines)
        .filter_map(|line| line.trim_start_matches("- ").strip_prefix(WORKDIR))
        .next_back()
        .map(|rest| rest.trim().to_string())
}

/// How a provider-asking agent's Test drives the loop, the same in every lane
/// (blocking, live, store-less): a chat agent asks once with no tools; a builtin
/// agent runs the loop a consumer runs with the tools it takes of the Test's offer.
/// No lane caps the time or round count: a loop runs as long as its work takes, and only
/// Stop or the reader going away ends it early.
struct TestPlan {
    runtime: Runtime,
    taken: Vec<&'static str>,
    allow_tool_rounds: bool,
    root: std::path::PathBuf,
    agent: AgentConfig,
    services: Arc<RuntimeServices>,
}

impl TestPlan {
    fn of(agent: &AgentConfig, body: &Map<String, Value>, services: Arc<RuntimeServices>) -> Result<TestPlan, String> {
        let offered: Vec<&'static str> = Runtime::Builtin.tools().iter().map(|t| t.name).collect();
        let mut taken = agent.narrow(&offered);
        if let Some(selected) = body.get("tools") {
            if agent.runtime != Runtime::Builtin { return Err("tools applies only to builtin tests".into()); }
            let selected = selected.as_array().ok_or("tools must be a list of tool names")?;
            let mut names = Vec::new();
            for item in selected {
                let name = item.as_str().ok_or("tools must be a list of tool names")?;
                if !taken.contains(&name) { return Err(format!("this agent does not take tool {name:?}")); }
                if names.contains(&name) { return Err(format!("tools lists {name:?} twice")); }
                names.push(name);
            }
            taken.retain(|name| names.contains(name));
        }
        let home = swiss_core::paths::home_dir();
        let root = match field(body, "cwd")? {
            Some(path) => {
                let root = crate::llm_tools::pinned(&home, &path)?;
                if !root.is_dir() { return Err("cwd must name a directory".into()); }
                root
            }
            None => home,
        };
        Ok(TestPlan {
            runtime: agent.runtime,
            taken,
            allow_tool_rounds: agent.allows_tools(),
            root, agent: agent.clone(), services,
        })
    }

    fn tools<'a>(&'a self, chat: &'a Chat, session: &'a Session, cancel: Option<Arc<swiss_llm::history::CancelFlag>>, run_id: Option<i64>) -> crate::llm_test_tools::TestTools<'a> {
        let shell = crate::llm_shell::ShellTools::new(&self.root, &self.taken, &self.services, crate::llm_shell::scope(&self.agent.name, run_id), cancel)
            .granting(&self.agent.secrets);
        crate::llm_test_tools::TestTools::new(&self.root, &self.taken, &self.agent, chat, session, shell)
    }

    /// The system prompt a Test conversation opens with: the agent's own (`base`), and
    /// for a builtin agent the environment snapshot after it. A chat agent asks as its
    /// runtime does, under its prompt alone.
    fn opening(&self, base: &str, chat: &Chat) -> String {
        if self.runtime != Runtime::Builtin {
            return base.to_string();
        }
        format!("{base}\n\n{}", self.environment(chat))
    }

    /// The snapshot (SPEC §llm.agents Test): what the model would otherwise spend tool
    /// calls finding out - where it works, the repository, the machine, the shell, the
    /// day, which agent and model it is - and what its tools reach. Taken as the
    /// conversation opens; a continuation replays it as noted.
    fn environment(&self, chat: &Chat) -> String {
        let plain = crate::llm_tools::plain_path;
        let mut facts = vec![
            format!("- {WORKDIR}{}", plain(&self.root)),
            format!("- Git repository: {}", repository(&self.root).unwrap_or_else(|| "none".into())),
            format!("- Home directory: {}", plain(&swiss_core::paths::home_dir())),
            format!("- Platform: {} {}", std::env::consts::OS, std::env::consts::ARCH),
        ];
        if self.taken.contains(&"bash") {
            facts.push(format!("- Shell: {}, through the {} tool", crate::llm_shell::shell_named(), crate::llm_shell::TOOL));
        }
        facts.push(format!("- Today's date: {}", chrono::Local::now().format("%Y-%m-%d (%A)")));
        facts.push(format!("- You are the agent {}, on model {} from provider {}", self.agent.name, chat.model, chat.provider));
        let notes = tool_notes(&self.taken, &swiss_core::secure::secretstore::granted(&self.agent.secrets));
        format!(
            "{}\nThese facts were taken as this conversation began; check with your tools before relying on one that may have changed since.\n{}\n\n# Tools\n{notes}",
            swiss_llm::conversation::SNAPSHOT_HEADING,
            facts.join("\n"),
        )
    }

    /// The Test's user turn: the prompt as given. When a builtin conversation's working
    /// directory is no longer the one its dialogue last told the model (`system` and
    /// `prior` are what the send replays), the turn closes with a note naming the new
    /// one - the snapshot itself never changes mid-dialogue.
    fn task(&self, prompt: &str, system: &str, prior: &[Value]) -> String {
        let now = crate::llm_tools::plain_path(&self.root);
        if self.runtime != Runtime::Builtin || told_directory(system, prior).as_deref() == Some(now.as_str()) {
            return prompt.to_string();
        }
        format!(
            "{prompt}\n\n<environment>\nThe working directory changed since you were last told; the file tools and the shell now start from it.\n{WORKDIR}{now}\n</environment>"
        )
    }
}

/// A Test drive's answer, whichever end it met: an answer, a cancel or the loop's
/// own failure. The loop files every one of them itself, and nothing outside it
/// cuts a drive short, so the answer only reports.
pub(crate) fn loop_answer(driven: Result<LoopOutcome, String>, chat: &Chat, run: &RunRecorder, started: Instant) -> Value {
    let mut answer = match driven {
        Ok(outcome) if outcome.cancelled => {
            json!({ "ok": false, "error": "cancelled", "cancelled": true, "ms": ms_since(started) })
        }
        Ok(outcome) => json!({
            "ok": true, "model": chat.model, "ms": ms_since(started),
            "text": clip(&outcome.summary, ANSWER_MAX),
            "turns": outcome.turns, "tokensIn": outcome.tokens.input, "tokensOut": outcome.tokens.output,
        }),
        // The loop noted and filed its own failure; the answer says it.
        Err(err) => json!({ "ok": false, "error": err, "ms": ms_since(started) }),
    };
    if let Some(id) = run.id() {
        answer["runId"] = json!(id);
    }
    answer
}

/// POST /api/llm/agents/test {agent, prompt?}: a provider-asking agent answers one prompt
/// under its system prompt, through the same session a real run gets (consumer "llm.test"),
/// so a Test with telemetry on proves the whole path to the collector. A spawned runtime is
/// checked without a task: its binary answers `--version`, and its env resolves (the names
/// come back, never values). Either way the Test is a ONE-SHOT run row, closed at its outcome.
/// A db agent is refused: it serves the Data page alone (SPEC §data.agent).
async fn agent_test(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "prompt", "tools", "cwd"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, prompt) = match (field(&body, "agent"), prompt_of(&body)) {
        (Ok(Some(name)), Ok(prompt)) => (name, prompt),
        (Ok(None), _) => return bad("agent is required"),
        (Err(why), _) | (_, Err(why)) => return bad(why),
    };
    let agent = match state.agent(&name) {
        Ok(agent) => agent,
        Err(err) => return bad(err),
    };
    if !agent.runtime.serves_tasks() {
        return bad(format!("{name} is a DB agent: it serves the Data page alone; ask it there, on a connection"));
    }
    let config = state.config();
    if !agent.runtime.asks_provider() {
        // The external Test's row says it ran (the audit link); native context is
        // the child's own - never claimed continuable here.
        let mut shot = match state.history() {
            Some(history) => history.run_recorder(&name, agent.runtime.as_str(), None, None),
            None => RunRecorder::none(),
        };
        shot.reserve().await;
        let mut answer = version_check(&agent, config.telemetry.as_ref()).await;
        shot.finish(answer["ok"].as_bool().unwrap_or(false));
        answer["runId"] = json!(shot.id());
        return ok(answer);
    }
    let chat = match agent.chat(&config.providers) {
        Ok(chat) => chat,
        Err(err) => return ok(json!({ "ok": false, "error": err, "ms": 0 })),
    }
    // An agent's Test records as the agent's call (SPEC llm.history).
    .recording(state.history(), Some(&agent.name));
    let session = state.session(&agent, "llm.test");
    let started = Instant::now();
    let plan = match TestPlan::of(&agent, &body, services) { Ok(plan) => plan, Err(why) => return bad(why) };
    let base = agent.system_prompt();
    let system = plan.opening(base.as_deref().unwrap_or(swiss_llm::agent::DEFAULT_SYSTEM_PROMPT), &chat);
    let tasked = plan.task(&prompt, &system, &[]);
    let mut run = chat.run_recorder(agent.runtime.as_str());
    // BLOCKING ORDER (SPEC llm.history): the one-shot's row is RESERVED and its
    // refusal heard BEFORE the provider ask - an admitted reserve is what
    // authorizes a paid outbound, and a refused quota stops the ask, never trails
    // it. The blocking route is not exempt.
    run.reserve().await;
    if let Some(why) = run.refusal() {
        return ok(json!({
            "ok": false,
            "runId": run.id(),
            "error": format!("the run could not be reserved: {why}"),
            "ms": ms_since(started),
        }));
    }
    // The blocking Test answers whole (the panel's dialog streams through
    // agent_test_stream); the sink stays a no-op.
    let mut on_event = |_event: swiss_llm::agent::LoopEvent| {};
    // This blocking request cannot be continued. Its jobs end with the request even
    // when the one-shot transcript is recorded in history.
    let mut tools = plan.tools(&chat, &session, None, None);
    let driven = swiss_llm::agent::run_loop(&chat, &system, &tasked, &mut tools, plan.allow_tool_rounds, &session, &mut run, &mut on_event).await;
    let mut answer = loop_answer(driven, &chat, &run, started);
    if session.reporting() {
        answer["session"] = json!(session.id());
    }
    ok(answer)
}

/// A spawned runtime's Test without a task: the binary answers `--version` and the
/// env resolves - `{ok, program, text|error, env, ms}`, env names only, never values.
async fn version_check(agent: &AgentConfig, telemetry: Option<&swiss_llm::telemetry::TelemetryConfig>) -> Value {
    let started = Instant::now();
    let program = match agent.program() {
        Ok(program) => program,
        Err(err) => return json!({ "ok": false, "error": err, "ms": 0 }),
    };
    let shown = program.display().to_string();
    let env = match agent.child_env(telemetry) {
        Ok(env) => env,
        Err(err) => return json!({ "ok": false, "program": shown, "error": err, "ms": 0 }),
    };
    let env_names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    let mut command = tokio::process::Command::new(&program);
    command.arg("--version").stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        // The daemon has no console; without this the child flashes one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let outcome = tokio::time::timeout(VERSION_TIMEOUT, command.output()).await;
    let ms = ms_since(started);
    match outcome {
        Ok(Ok(out)) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_string();
            json!({ "ok": true, "program": shown, "text": clip(&version, ANSWER_MAX), "env": env_names, "ms": ms })
        }
        Ok(Ok(out)) => {
            let why = String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>();
            json!({ "ok": false, "program": shown, "error": format!("--version exited with {}: {why}", out.status.code().unwrap_or(-1)), "ms": ms })
        }
        Ok(Err(err)) => json!({ "ok": false, "program": shown, "error": format!("could not start: {err}"), "ms": ms }),
        Err(_) => json!({ "ok": false, "program": shown, "error": format!("--version took longer than {} s", VERSION_TIMEOUT.as_secs()), "ms": ms }),
    }
}

/// One event onto the live Test's stream: a named SSE frame with a JSON body.
/// For the frames the LANE itself owns (the opening run frame); every loop
/// event goes through the awaited deliveries below instead.
pub(crate) fn sse_send(tx: &mpsc::Sender<Event>, name: &str, data: Value) {
    let _ = tx.try_send(Event::default().event(name).data(data.to_string()));
}

/// One loop event as its SSE frame (name, JSON body).
fn event_frame(event: swiss_llm::agent::LoopEvent) -> (&'static str, Value) {
    match event {
        swiss_llm::agent::LoopEvent::Thinking(one) => ("delta", json!({ "kind": "thinking", "text": one })),
        swiss_llm::agent::LoopEvent::Text(one) => ("delta", json!({ "kind": "text", "text": one })),
        swiss_llm::agent::LoopEvent::ToolCall { name, arguments } => ("tool", json!({ "name": name, "arguments": arguments })),
        swiss_llm::agent::LoopEvent::ToolResult { name, ok, text, ms } => {
            ("toolResult", json!({ "name": name, "ok": ok, "text": text, "ms": ms }))
        }
        swiss_llm::agent::LoopEvent::Steered { id, text } => ("steer", json!({ "id": id, "text": text })),
        swiss_llm::agent::LoopEvent::Final(text) => ("final", json!({ "text": text })),
        swiss_llm::agent::LoopEvent::Usage(usage) => ("usage", json!({
            "bytes": usage.bytes, "cap": usage.cap, "system": usage.system, "tools": usage.tools,
            "tokensIn": usage.tokens.input, "tokensCacheRead": usage.tokens.cache_read,
            "tokensCacheWrite": usage.tokens.cache_creation, "tokensOut": usage.tokens.output, "ms": usage.ms,
            "ttftMs": usage.ttft_ms,
        })),
    }
}

/// The AWAITED delivery closure over the bounded channel: a full queue
/// backpressures the wire (the loop waits - no delta is ever dropped, no
/// buffer ever grows past the bound), and a dropped receiver resolves false
/// after requesting the run's cancel - the loop then files its honest tail
/// instead of streaming headless.
pub(crate) fn awaited_sink(
    tx: &mpsc::Sender<Event>,
    cancel: &Arc<swiss_llm::history::CancelFlag>,
) -> impl FnMut(swiss_llm::agent::LoopEvent) -> swiss_llm::agent::Delivered {
    // The closure OWNS its dependencies: the sender handle and the shared
    // cancel handle are cloned here, once, OUTSIDE the move - then cloned
    // again per delivery off the owned ones. Nothing borrowed escapes into
    // the 'static Delivered future; the flag stays the ONE shared flag.
    let tx = tx.clone();
    let cancel = Arc::clone(cancel);
    move |event| {
        let tx = tx.clone();
        let cancel = Arc::clone(&cancel);
        Box::pin(async move {
            let (name, data) = event_frame(event);
            let frame = Event::default().event(name).data(data.to_string());
            match tx.send(frame).await {
                Ok(()) => true,
                Err(_) => {
                    cancel.request();
                    false
                }
            }
        }) as swiss_llm::agent::Delivered
    }
}

/// A prestream refusal: HTTP 409 with a machine code - the panel routes on
/// the code, never on a message it must parse.
pub(crate) fn refuse(code: &str, message: impl AsRef<str>) -> Response {
    admin_json(StatusCode::CONFLICT, json!({ "ok": false, "code": code, "error": message.as_ref() }))
}

/// POST /api/llm/agents/test/stream {agent, prompt?, runId?}: the Test as a LIVE
/// CONVERSATION - server-sent events, one per happening. `run` opens the stream
/// with the conversation's id and continuity facts; `delta` carries the
/// typewriter (each reasoning or answer piece the moment it lands on the wire);
/// `tool` and `toolResult` carry the loop's tool traffic; `steer` says words the
/// reader sent mid-run ([agent_test_steer]) went in at a round boundary; `final` is
/// the closing text whole; `done` closes the send with its outcome. A send without `runId`
/// starts a new conversation (an agent's existing open one refuses 409
/// open-run-exists); a send with it continues THAT conversation - the earlier
/// turns replay as the request's opening messages (assistant text beside its
/// tool calls, answers paired by call id, reasoning only where the target
/// requires it), the Test addendum rides only the first user turn, and the run
/// row stays open for the next send. A chat agent's send is one request with no
/// tools, exactly as its runtime asks. Prestream refusals are 409 with a machine
/// code (the lane's guards live in [swiss_llm::conversation]); without a history
/// store there are no run semantics, and the stream is the stateless one-shot it
/// always was.
async fn agent_test_stream(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "prompt", "runId", "tools", "cwd"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, prompt) = match (field(&body, "agent"), prompt_of(&body)) {
        (Ok(Some(name)), Ok(prompt)) => (name, prompt),
        (Ok(None), _) => return bad("agent is required"),
        (Err(why), _) | (_, Err(why)) => return bad(why),
    };
    let joining = match body.get("runId") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(id) => Some(id),
            None => return bad("runId must be an integer"),
        },
    };
    let agent = match state.agent(&name) {
        Ok(agent) => agent,
        Err(err) => return bad(err),
    };
    if !agent.runtime.serves_tasks() {
        return bad(format!("{name} is a DB agent: it serves the Data page alone; ask it there, on a connection"));
    }
    // A spawned runtime has no wire to stream: the blocking Test answers it whole.
    if !agent.runtime.asks_provider() {
        return bad("a spawned runtime has nothing to stream; use the blocking test");
    }
    let config = state.config();
    // Fail closed before any stream opens: a provider or model the row no longer
    // resolves is a rejection naming it (the panel reads a non-2xx as {error}),
    // never a 200 body the event reader cannot parse.
    let chat = match agent.chat(&config.providers) {
        Ok(chat) => chat,
        Err(err) => return bad(err),
    }
    .recording(state.history(), Some(&agent.name));
    let session = state.session(&agent, "llm.test");
    let started = Instant::now();
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    let plan = match TestPlan::of(&agent, &body, services) { Ok(plan) => plan, Err(why) => return bad(why) };
    let base = agent.system_prompt().map_or_else(|| swiss_llm::agent::DEFAULT_SYSTEM_PROMPT.to_string(), |s| s.into_owned());
    // A new conversation opens with the snapshot taken now; a continued one replays
    // the one it opened with (the lane hands it back).
    let system = plan.opening(&base, &chat);
    // The conversation lane owns the run row; without a history store there are
    // no run semantics - EXCEPT that an explicit runId is a contract this home
    // cannot honor: it refuses before any provider ask, never a silent
    // stateless fallback that would fork the conversation it named.
    let Some(history) = state.history() else {
        if joining.is_some() {
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "a run id needs the llm history store; this home has none",
            );
        }
        tokio::spawn(stateless_test(chat, plan, system, prompt, session, started, tx));
        return sse_of(rx);
    };
    // One stream per agent at a time: the second never even reserves.
    if state.seat_run_of(&name).is_some() {
        return refuse("busy", "this agent's test is already streaming");
    }
    let lane = match conversation::join(&history, joining, &name, agent.runtime.as_str(), &chat, &base).await {
        Ok(lane) => lane,
        Err(LaneError::Refused { code, message }) => return refuse(code, message),
        Err(LaneError::Unavailable(why)) => return admin_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    };
    let conversation::Lane { mut run, run_id, continued, prior, system: noted } = lane;
    let system = noted.unwrap_or(system);
    let cancel = swiss_llm::history::CancelFlag::new();
    let steer = swiss_llm::history::Steer::new();
    if !state.seat(&name, run_id, cancel.clone(), steer.clone()) {
        return refuse("busy", "this agent's test is already streaming");
    }
    run.set_cancel(cancel.clone());
    run.set_steer(steer.clone());
    // FIRST FRAME: the run the reader joined - its id and its continuity facts.
    sse_send(&tx, "run", json!({
        "runId": run_id, "agent": name, "provider": chat.provider, "model": chat.model,
        "continued": continued,
    }));
    let state = state.clone();
    tokio::spawn(async move {
        let mut tools = plan.tools(&chat, &session, Some(cancel.clone()), Some(run_id));
        let mut on_event = awaited_sink(&tx, &cancel);
        let tasked = plan.task(&prompt, &system, &prior);
        // The drive lives in its OWN lexical block: the pinned future holds
        // the run recorder mutably, and the answer below may only borrow run
        // once the future has actually dropped.
        let driven = {
            let looped = swiss_llm::agent::run_loop_continued(
                &chat, &system, &prior, &tasked, &mut tools, plan.allow_tool_rounds, &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            // Silent-wire watch (SPEC llm.agents Test): a reader that went away
            // while no event flows still cancels - tx.closed() fires the moment
            // every response body drops. The flag is requested, then the drive is
            // awaited to completion: IT files the honest tail; the engine is never
            // dropped whole. Once the cancel is requested the watch arm disarms,
            // and the drive is the only thing left to wait for. No clock cuts the
            // drive: it runs as long as its work takes.
            loop {
                tokio::select! {
                    _ = tx.closed(), if !cancel.requested() => {
                        cancel.request();
                    }
                    outcome = &mut looped => break outcome,
                }
            }
        };
        if cancel.requested() {
            crate::llm_shell::cancel_conversation(&plan.services.runs, run_id).await;
        }
        // No round takes words from here on: a post now is refused, and what is left
        // the reader sends as its next turn (every steer frame flew before done).
        steer.close();
        let answer = loop_answer(driven, &chat, &run, started);
        // The closing frame waits its turn (bounded); a closed channel here is
        // the disconnect the cancel already handled.
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        state.unseat(&name);
    });
    sse_of(rx)
}

/// The SSE relay: frames as they arrive off the bounded channel, a keep-alive
/// while the reader waits.
pub(crate) fn sse_of(rx: mpsc::Receiver<Event>) -> Response {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (Ok::<_, std::convert::Infallible>(event), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// The store-less stream: today's shape, for a plugin whose history never
/// opened - the dialogue plays live, nothing persists, no run semantics attach.
async fn stateless_test(
    chat: Chat,
    plan: TestPlan,
    system: String,
    prompt: String,
    session: Session,
    started: Instant,
    tx: mpsc::Sender<Event>,
) {
    // No run semantics attach, but the DELIVERY contract is the conversation
    // lane's - every event is an awaited take on the bounded channel (no drop,
    // no unbounded growth), and a reader that goes away resolves false so the
    // drive ends instead of streaming headless. The flag is only the loop
    // observer here; nobody else holds it.
    let cancel = swiss_llm::history::CancelFlag::new();
    let mut on_event = awaited_sink(&tx, &cancel);
    let mut tools = plan.tools(&chat, &session, Some(cancel.clone()), None);
    let tasked = plan.task(&prompt, &system, &[]);
    let mut run = RunRecorder::none();
    let driven = {
        let looped = swiss_llm::agent::run_loop_continued(
            &chat, &system, &[], &tasked, &mut tools, plan.allow_tool_rounds, &session, &mut run, &mut on_event,
        );
        tokio::pin!(looped);
        loop {
            tokio::select! {
                _ = tx.closed(), if !cancel.requested() => cancel.request(),
                outcome = &mut looped => break outcome,
            }
        }
    };
    let answer = loop_answer(driven, &chat, &run, started);
    let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
}

/// POST /api/llm/agents/test/cancel {runId}: cancel a run in flight -
/// truthfully. The round in flight is dropped wherever it stood (provider
/// stream, between rounds, tool dispatch), a running tool completes (its
/// result is real), the notes that are real are filed, and the conversation
/// STAYS OPEN - the reader continues or closes it; unresolved shapes need a
/// close or New. A known but idle run cancels nothing ({cancelled:false});
/// an unknown id is a plain 404.
async fn agent_test_cancel(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["runId"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let Some(run_id) = body.get("runId").and_then(Value::as_i64) else {
        return bad("runId must be an integer");
    };
    if let Some(cancel) = state.seat_of(run_id) {
        cancel.request();
        crate::llm_shell::cancel_conversation(&services.runs, run_id).await;
        return ok(json!({ "ok": true, "cancelled": true }));
    }
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match history.read_turns(run_id).await {
        // Known but idle: nothing is in flight to cancel - the truth, not an error.
        Ok(Some(_)) => {
            let stopped = crate::llm_shell::cancel_conversation(&services.runs, run_id).await;
            ok(json!({ "ok": true, "cancelled": stopped > 0 }))
        },
        Ok(None) => admin_json(
            StatusCode::NOT_FOUND,
            json!({ "ok": false, "code": "gone", "error": format!("run {run_id} is not in the llm history") }),
        ),
        Err(err) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("the llm history read failed: {err}")),
    }
}

/// The longest id a reader names a queued message by.
const STEER_ID_MAX: usize = 64;

/// The run and the message a steer body names.
fn steer_ids(body: &Map<String, Value>) -> Result<(i64, String), String> {
    let run_id = body.get("runId").and_then(Value::as_i64).ok_or("runId must be an integer")?;
    let id = field(body, "id")?.ok_or("id is required")?;
    if id.chars().count() > STEER_ID_MAX {
        return Err(format!("id: at most {STEER_ID_MAX} characters"));
    }
    Ok((run_id, id))
}

/// The refusal a post to a run that no longer takes words gets: the reader sends
/// them as its next turn instead.
fn steer_idle() -> Response {
    refuse("idle", "that conversation is not streaming: send it as the next turn")
}

/// POST /api/llm/agents/test/steer {runId, id, text}: words the reader sent while the
/// run streams (SPEC llm.agents Test), queued for the loop's next round boundary, where
/// the model reads them after the round's tool answers; the stream's `steer` frame,
/// under the same id, says they went in. A run not in flight, or whose loop has ended,
/// is 409 `idle`, and a full inbox 409 `full`: the reader keeps the words and sends
/// them as the next turn.
async fn agent_test_steer(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["runId", "id", "text"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (run_id, id) = match steer_ids(&body) {
        Ok(ids) => ids,
        Err(why) => return bad(why),
    };
    let text = match field(&body, "text") {
        Ok(Some(text)) if text.chars().count() <= PROMPT_MAX => text,
        Ok(Some(_)) => return bad(format!("text: at most {PROMPT_MAX} characters")),
        Ok(None) => return bad("text is required"),
        Err(why) => return bad(why),
    };
    let Some(steer) = state.steer_of(run_id) else { return steer_idle() };
    match steer.post(&id, &text) {
        Ok(()) => ok(json!({ "ok": true, "queued": true })),
        Err(swiss_llm::history::SteerRefused::Closed) => steer_idle(),
        Err(swiss_llm::history::SteerRefused::Full) => refuse(
            "full",
            format!("{} messages already wait for the next step", swiss_llm::history::STEER_MAX),
        ),
    }
}

/// POST /api/llm/agents/test/steer/withdraw {runId, id}: take a queued message back
/// before the loop reaches it → `{ok: true, withdrawn}`; false when the loop already
/// took it (its steer frame says so) or the run is no longer in flight.
async fn agent_test_withdraw(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["runId", "id"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (run_id, id) = match steer_ids(&body) {
        Ok(ids) => ids,
        Err(why) => return bad(why),
    };
    let withdrawn = state.steer_of(run_id).is_some_and(|steer| steer.withdraw(&id));
    ok(json!({ "ok": true, "withdrawn": withdrawn }))
}

/// POST /api/llm/history/runs/{id}/close: the reader pressed New - the
/// conversation closes NEUTRALLY (no failure filed: ok stays NULL), idempotent,
/// read-only afterwards, and refused 409 busy while its stream is still in
/// flight (cancel first).
async fn history_run_close(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, Path(run_id): Path<i64>) -> Response {
    if state.seat_of(run_id).is_some() {
        return refuse("busy", "that conversation is still streaming: cancel it before closing");
    }
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match history.close_run(run_id).await {
        Ok(Some(())) => {
            crate::llm_shell::cancel_conversation(&services.runs, run_id).await;
            ok(json!({ "ok": true }))
        },
        Ok(None) => admin_json(
            StatusCode::NOT_FOUND,
            json!({ "ok": false, "code": "gone", "error": format!("run {run_id} is not a conversation") }),
        ),
        Err(err) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("the llm history write failed: {err}")),
    }
}

/// POST /api/llm/telemetry/test: one empty export to the saved collector - it stores
/// nothing, and proves the endpoint, the wire and the headers.
async fn telemetry_test(State(state): State<Arc<LlmState>>, bytes: Bytes) -> Response {
    if let Err(why) = body_of(&bytes, &[]) {
        return bad(why);
    }
    let Some(config) = state.config().telemetry.clone() else {
        return bad("the row has no telemetry block: set a protocol and an endpoint on the Agents page first");
    };
    match telemetry::probe(&config).await {
        Ok(ms) => ok(json!({ "ok": true, "protocol": config.protocol.as_str(), "endpoint": config.endpoint, "ms": ms })),
        Err(err) => ok(json!({ "ok": false, "protocol": config.protocol.as_str(), "endpoint": config.endpoint, "error": err })),
    }
}

// --- the history reads (SPEC llm.history) ----------------------------------------------------

/// One int query parameter, clamped into its range; a malformed value reads as the
/// default rather than a 400 (the panel always sends sane numbers).
fn int_param(params: &std::collections::HashMap<String, String>, key: &str, default: i64, min: i64, max: i64) -> i64 {
    params
        .get(key)
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

/// GET /api/llm/history/calls?page&pageSize&kind&q: one newest-first page of provider
/// round trips - chat and embedding, every consumer alike. 503-shaped honesty: with the
/// plugin stopped there is no store to read, and the route says so.
async fn history_calls(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    let page = history
        .read_calls(
            int_param(&params, "page", 0, 0, 1_000_000),
            int_param(&params, "pageSize", 25, 1, 100),
            params.get("kind").map(String::as_str),
            params.get("q").map(String::as_str),
        )
        .await;
    match page {
        // A failed read is an error said out loud, never an invented empty page
        // (the same honesty rule the 503 answers).
        Ok(page) => ok(page),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/calls/{id}/body: one call body in full - the request digest and
/// the answer, sealed at rest, gone flagged when pruned.
async fn history_call_body(State(state): State<Arc<LlmState>>, Path(id): Path<i64>) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match history.read_call_body(id).await {
        Ok(Some(body)) => ok(body),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, &format!("call {id} is not in the llm history")),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/runs?page&pageSize: one newest-first page of agent conversations,
/// the running ones first. `source` (memory:<sync run id>) lists instead every run that
/// job started, unpaged (SPEC llm.history Live).
async fn history_runs(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    let read = match params.get("source").filter(|source| !source.trim().is_empty()) {
        Some(source) => history.read_runs_by_source(source.trim().to_string()).await,
        None => {
            history
                .read_runs(
                    int_param(&params, "page", 0, 0, 1_000_000),
                    int_param(&params, "pageSize", 25, 1, 100),
                    params.get("agent").cloned(),
                    params.get("open").map(|open| open == "true"),
                )
                .await
        }
    };
    match read {
        Ok(runs) => ok(runs),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/runs/{id}/turns: one conversation with every turn previewed.
async fn history_run_turns(State(state): State<Arc<LlmState>>, Path(id): Path<i64>) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match history.read_turns(id).await {
        Ok(Some(run)) => ok(run),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, &format!("run {id} is not in the llm history")),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/runs/{id}/dialogue: one conversation in one fetch - the run,
/// every turn's full text, and every provider call with its prompt and answer digest.
/// The sheet that renders a dialogue does one request, not one per turn.
async fn history_run_dialogue(State(state): State<Arc<LlmState>>, Path(id): Path<i64>) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin owning it is off");
    };
    match history.read_dialogue(id).await {
        Ok(Some(dialogue)) => ok(dialogue),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, &format!("run {id} is not in the llm history")),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/runs/{id}/calls: the provider calls inside one run - the
/// per-round-trip request and response a conversation was built from, each with its
/// own body route. The agent's window is the join; the store's comment says the
/// serial-runs limit out loud.
async fn history_run_calls(State(state): State<Arc<LlmState>>, Path(id): Path<i64>) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin owning it is off");
    };
    match history.read_calls_of_run(id).await {
        Ok(Some(calls)) => ok(calls),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, &format!("run {id} is not in the llm history")),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

/// GET /api/llm/history/turns/{runId}/{seq}/body: one turn in full.
async fn history_turn_body(State(state): State<Arc<LlmState>>, Path((run_id, seq)): Path<(i64, i64)>) -> Response {
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match history.read_turn_body(run_id, seq).await {
        Ok(Some(body)) => ok(body),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, &format!("run {run_id} turn {seq} is not in the llm history")),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the llm history read failed: {err}"),
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn live_tool_io_and_final_match_the_complete_recorded_text() {
        let arguments = format!("{{\"sql\":\"{} END\"}}", "列".repeat(600));
        let result = format!("{} RESULT_END", "数据".repeat(600));
        let answer = format!("{} ANSWER_END", "回答".repeat(2000));
        let (_, call) = event_frame(swiss_llm::agent::LoopEvent::ToolCall { name: "run_sql".into(), arguments: arguments.clone() });
        let (_, output) = event_frame(swiss_llm::agent::LoopEvent::ToolResult { name: "run_sql".into(), ok: true, text: result.clone(), ms: 42 });
        let (_, final_frame) = event_frame(swiss_llm::agent::LoopEvent::Final(answer.clone()));
        assert_eq!(call["arguments"], arguments);
        assert_eq!(output["text"], result);
        assert_eq!(output["ms"], 42);
        assert_eq!(final_frame["text"], answer);
    }

    pub(crate) fn mount(state: Arc<LlmState>) -> Router {
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        crate::llm_shell::register(&services).expect("shell action");
        let analysis = crate::analysis::AnalysisStore::home();
        let consoles = crate::db_consoles::ConsoleStore::at(analysis.root().with_file_name("consoles"), services.bus.clone());
        mount_with_analysis(state, services, analysis, consoles)
    }

    use axum::body::Body;
    use axum::http::Request;
    use swiss_llm::LlmConfig;
    use tower::ServiceExt;

    pub(crate) async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// The same drive, for the route whose answer is a stream, not JSON: the status,
    /// the content type, and the whole body as text (the event frames as they flew).
    pub(crate) async fn call_text(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, String, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A one-shot SSE wire (the OpenAI chat stream shape): answers the one request
    /// it takes with the given event frames, framed by content-length as one body.
    async fn sse_wire(frames: Vec<String>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // One answer per request, in order: a loop that calls a tool and asks
            // again receives the next frame group as its next turn.
            for frames in frames {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 { break; }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        frames.len(),
                        frames
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write");
                    break;
                }
            }
            }
        });
        url
    }

    /// The same wire as [sse_wire], but every request body is captured: the tests
    /// that must see what the loop RESENT (round preservation) read the asks back.
    pub(crate) async fn sse_wire_capturing(frames: Vec<String>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        sse_wire_capturing_after(Duration::ZERO, frames).await
    }

    /// [sse_wire_capturing] with a slow provider: each answer waits `delay` after its
    /// request has arrived. The clock stands still only while the provider thinks
    /// (paused, it jumps the wait at once, then runs on), so the wait costs no real time,
    /// a history store keeps its own clock outside it, and any timer started before it -
    /// a cap on the drive - sees the whole delay pass. Needs a current-thread runtime.
    pub(crate) async fn sse_wire_capturing_after(delay: Duration, frames: Vec<String>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // One answer per request, in order, with the ask recorded whole.
            for frames in frames {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.expect("read");
                    if n == 0 { break; }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let length = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + length {
                        if let Some(body) = buf.get(head_end + 4..head_end + 4 + length) {
                            sink.lock().expect("captured").push(String::from_utf8_lossy(body).into_owned());
                        }
                        if !delay.is_zero() {
                            tokio::time::pause();
                            tokio::time::sleep(delay).await;
                            tokio::time::resume();
                        }
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            frames.len(),
                            frames
                        );
                        socket.write_all(answer.as_bytes()).await.expect("write");
                        break;
                    }
                }
            }
        });
        (url, captured)
    }

    /// One SSE frame group -> (event name, its JSON data), in wire order.
    pub(crate) fn events_of(text: &str) -> Vec<(String, Value)> {
        text.split("\n\n")
            .filter_map(|block| {
                let name = block.lines().find(|l| l.starts_with("event:"))?.strip_prefix("event:")?.trim().to_string();
                let data = block.lines().find(|l| l.starts_with("data:"))?.strip_prefix("data:")?.trim().to_string();
                Some((name, serde_json::from_str(&data).unwrap_or(Value::Null)))
            })
            .collect()
    }

    /// An assistant message's text as the OpenAI wire spells it: a plain string or
    /// an array of typed parts.
    fn text_of(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter(|p| p["type"] == "text")
                .map(|p| p["text"].as_str().unwrap_or_default())
                .collect(),
            _ => String::new(),
        }
    }

    fn row() -> LlmConfig {
        LlmConfig::parse(&json!({
            "providers": {
                "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "apiKey": "${secret://llm-test-key}",
                           "models": [{ "id": "tiny" }, { "id": "bge-m3", "kind": "embedding", "name": "BGE M3" },
                                      { "id": "seer", "name": "Seer 2", "contextWindow": 262144, "maxOutput": 65536, "image": true, "thinking": false }] }
            },
            "embedding": { "provider": "local", "model": "bge-m3", "dimensions": 1024 },
            "agents": {
                "distiller": { "provider": "local", "maxTurns": 8, "tools": ["sync", "read"] },
                "cc": { "runtime": "claude", "command": "swiss-no-such-binary", "systemPrompt": "Prefer Grep." }
            },
            "telemetry": { "protocol": "grpc", "endpoint": "http://127.0.0.1:4317", "enabled": false }
        }))
        .expect("row")
    }

    #[tokio::test]
    async fn status_shows_the_row_with_references_and_how_agents_report() {
        let state = LlmState::new();
        state.apply(row());
        let app = mount(state);
        let (status, body) = call(&app, "GET", "/api/llm/status", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["serving"], true);
        assert_eq!(body["providers"][0]["apiKey"], "${secret://llm-test-key}");
        assert_eq!(body["providers"][0]["url"], "http://127.0.0.1:9/v1/chat/completions");
        // Each model with what its entry states, the defaults filled: a bare one thinks, reads
        // text only and states no figure.
        assert_eq!(body["providers"][0]["models"], json!([
            { "id": "tiny", "kind": "chat", "name": null, "contextWindow": null, "maxOutput": null, "image": false, "thinking": true },
            { "id": "bge-m3", "kind": "embedding", "name": "BGE M3", "contextWindow": null, "maxOutput": null, "image": false, "thinking": true },
            { "id": "seer", "kind": "chat", "name": "Seer 2", "contextWindow": 262144, "maxOutput": 65536, "image": true, "thinking": false },
        ]));
        assert_eq!(body["embedding"]["remote"], false);
        assert!(body["embedding"]["fingerprint"].as_str().unwrap_or_default().starts_with("openai:bge-m3:1024:ep-"), "{body}");
        let agents = body["agents"].as_array().expect("agents");
        let cc = agents.iter().find(|a| a["name"] == "cc").expect("cc");
        assert!(cc["programError"].as_str().unwrap_or_default().contains("not found"), "{cc}");
        assert_eq!(cc["reports"], "off", "telemetry is disabled");
        assert_eq!((cc["systemPrompt"].as_str(), &cc["systemPromptDefault"]), (Some("Prefer Grep."), &Value::Null), "{cc}");
        assert_eq!((&cc["tools"], &cc["toolsInForce"]), (&Value::Null, &json!(["Read", "Grep", "Glob"])), "no list: the whole catalog");
        let distiller = agents.iter().find(|a| a["name"] == "distiller").expect("distiller");
        assert!(distiller.get("maxTurns").is_none());
        assert_eq!(distiller["model"].as_str(), Some("tiny"));
        assert_eq!(distiller["systemPrompt"], Value::Null);
        assert!(distiller["systemPromptDefault"].as_str().unwrap_or_default().starts_with("You are an agent"), "{distiller}");
        assert_eq!((&distiller["tools"], &distiller["toolsInForce"]), (&json!(["read", "sync"]), &json!(["read", "sync"])));
        let builtin: Vec<&str> = body["catalog"]["builtin"].as_array().expect("catalog").iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(builtin, ["read", "write", "edit", "glob", "grep", "bash", "job_output", "job_kill", "sync", "delegate"]);
        assert_eq!(body["catalog"]["chat"], json!([]));
        assert!(body["catalog"]["builtin"][0].get("on").is_none(), "only a db tool names a connection kind");
        let db: Vec<(&str, &str)> = body["catalog"]["db"].as_array().expect("db catalog").iter().map(|t| (t["name"].as_str().unwrap_or_default(), t["on"].as_str().unwrap_or_default())).collect();
        assert_eq!(db.len(), 25);
        assert_eq!((db[0], db[5], db[15], db[16]), (("list_tables", "sql"), ("list_collections", "mongo"), ("stage_redis", "redis"), ("stop_listen", "redis")));
        // A console's tabs and the memory serve every kind: they name none.
        assert_eq!(&db[17..], [
            ("open_console", ""),
            ("list_console_tabs", ""),
            ("read_console_tab", ""),
            ("edit_console_sql", ""),
            ("delete_console_sql", ""),
            ("read_memory", ""),
            ("save_memory", ""),
            ("delete_memory", ""),
        ]);
        assert_eq!(body["defaultPrompts"]["builtin"], distiller["systemPromptDefault"]);
        assert_eq!((&body["defaultPrompts"]["claude"], &body["defaultPrompts"]["pi"]), (&Value::Null, &Value::Null));
        assert!(body["catalog"]["pi"][0]["summary"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(body["telemetry"]["enabled"], false);
    }

    #[tokio::test]
    async fn tests_name_saved_entries_and_say_why_they_fail() {
        let state = LlmState::new();
        state.apply(row());
        let app = mount(state);
        let (status, body) = call(&app, "POST", "/api/llm/providers/test", Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (status, _) = call(&app, "POST", "/api/llm/providers/test", Some(json!({ "provider": "nope" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "POST", "/api/llm/providers/test", Some(json!({ "provider": "local", "baseUrl": "http://x" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a body never names an endpoint");
        // The key reference does not resolve: the answer says so, naming the reference only.
        let (status, body) = call(&app, "POST", "/api/llm/providers/test", Some(json!({ "provider": "local" }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap_or_default().contains("llm-test-key"), "{body}");
        // A model the provider does not list as a chat model is refused before any call.
        let (_, body) = call(&app, "POST", "/api/llm/providers/test", Some(json!({ "provider": "local", "model": "bge-m3" }))).await;
        assert!(body["error"].as_str().unwrap_or_default().contains("is an embedding model"), "{body}");
        let (_, body) = call(&app, "POST", "/api/llm/providers/test", Some(json!({ "provider": "local", "model": "huge" }))).await;
        assert!(body["error"].as_str().unwrap_or_default().contains("does not list \"huge\""), "{body}");
        // The listing names a saved provider too, and says why it could not ask.
        let (status, _) = call(&app, "POST", "/api/llm/providers/models", Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "POST", "/api/llm/providers/models", Some(json!({ "provider": "nope" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = call(&app, "POST", "/api/llm/providers/models", Some(json!({ "provider": "local" }))).await;
        assert_eq!((status, body["ok"].as_bool()), (StatusCode::OK, Some(false)), "{body}");
        assert_eq!(body["url"], "http://127.0.0.1:9/v1/models");
        assert!(body["error"].as_str().unwrap_or_default().contains("llm-test-key"), "{body}");
        let (status, _) = call(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "nope" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = call(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "cc" }))).await;
        assert_eq!((status, body["ok"].as_bool()), (StatusCode::OK, Some(false)));
        assert!(body["error"].as_str().unwrap_or_default().contains("not found"), "{body}");
    }

    #[tokio::test]
    async fn the_agent_test_streams_its_dialogue_as_server_sent_events() {
        // The wire the row points at: one OpenAI-style chat stream - reasoning,
        // two text pieces, a stop with usage. The route relays each happening as
        // an event frame, then closes with the run's numbers.
        let frames = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"weighing it\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"I am \"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"GLM.\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let base = sse_wire(vec![frames]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1",
                                       "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 2, "tools": [] } }
        }))
        .expect("row"));
        let app = mount(state);
        // An unknown agent never starts; a spawned runtime has nothing to stream.
        let (status, _) = call(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "nope" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The live dialogue: the frames arrive as the model speaks, and the run
        // closes with its own numbers.
        let (status, content_type, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(content_type.starts_with("text/event-stream"), "{content_type}");
        assert!(text.contains("event: delta"), "{text}");
        assert!(text.contains("\"kind\":\"thinking\""), "{text}");
        assert!(text.contains("weighing it"), "{text}");
        assert!(text.contains("\"kind\":\"text\""), "{text}");
        assert!(text.contains("I am "), "{text}");
        assert!(text.contains("event: final"), "{text}");
        assert!(text.contains("GLM."), "{text}");
        assert!(text.contains("event: done"), "{text}");
        assert!(text.contains("\"ok\":true"), "{text}");
        assert!(text.contains("\"turns\":1"), "{text}");
    }

    #[tokio::test]
    async fn a_streamed_tool_call_runs_the_loop_not_one_turn() {
        // Turn one: the model asks for a tool (streamed as argument chunks);
        // turn two: fed the result, it answers. The wire sees two requests, the
        // frames see the tool ask, its result and a two-turn done.
        let ask = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"notes.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2,\"total_tokens\":11}}\n\n",
            "data: [DONE]\n\n"
        ).to_string();
        let answer = concat!(
            "data: {\"id\":\"c2\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"read it\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c2\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":2,\"total_tokens\":14}}\n\n",
            "data: [DONE]\n\n"
        ).to_string();
        let base = sse_wire(vec![ask, answer]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1",
                                       "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let app = mount(state);
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(text.contains("event: tool"), "{text}");
        assert!(text.contains("\"name\":\"read\""), "{text}");
        assert!(text.contains("notes.md"), "{text}");
        assert!(text.contains("event: toolResult"), "{text}");
        assert!(text.contains("event: final"), "{text}");
        assert!(text.contains("read it"), "{text}");
        assert!(text.contains("\"turns\":2"), "{text}");
    }

    #[tokio::test]
    async fn the_telemetry_test_needs_a_block() {
        let state = LlmState::new();
        state.apply(LlmConfig::default());
        let app = mount(state.clone());
        let (status, _) = call(&app, "POST", "/api/llm/telemetry/test", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "POST", "/api/llm/embedding/test", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "embedding off");
    }

    #[tokio::test]
    async fn history_routes_say_503_without_a_store_and_404_for_missing_rows() {
        // The 503 half: no plugin start, no store - the route says the history is
        // not open rather than an invented empty page (SPEC llm.api).
        let state = LlmState::new();
        state.apply(row());
        let app = mount(state.clone());
        let (status, body) = call(&app, "GET", "/api/llm/history/calls", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body["error"].as_str().unwrap_or_default().contains("not open"), "{body}");
        let (status, _) = call(&app, "GET", "/api/llm/history/runs", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // The 404 half: a store IS open (an empty one), a row that is not there
        // is absent - not gone, not empty.
        let dir = swiss_core::paths::test_home().join(format!("llm-api-history-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let (status, body) = call(&app, "GET", "/api/llm/history/calls?page=0&pageSize=5", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["calls"], json!([]), "a genuinely empty store, read honestly");
        let (status, body) = call(&app, "GET", "/api/llm/history/calls/41/body", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = call(&app, "GET", "/api/llm/history/runs/9/turns", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, body) = call(&app, "GET", "/api/llm/history/turns/9/7/body", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        state.withdraw_history();
        history.close().await.expect("close");
    }

    /// One plain final-answer round: an assistant text, a stop with usage.
    pub(crate) fn one_round_answer() -> String {
        concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"done.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string()
    }

    /// An app with the row pointed at a capturing wire and a real history store:
    /// the streamed runs file, so run ids exist to drive.
    async fn streamed_app(frames: Vec<String>) -> (Router, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let (base, captured) = sse_wire_capturing(frames).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history);
        (mount(state), captured)
    }

    #[tokio::test]
    async fn an_assistant_round_keeps_its_text_beside_its_tool_calls() {
        // The contract: a finished round's text and its tool calls ride ONE
        // assistant message on the next request - ids intact - with the tool
        // answer paired by call id.
        let ask = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Checking the notes.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"notes.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (app, captured) = streamed_app(vec![ask, one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let asks = captured.lock().expect("captured").clone();
        assert_eq!(asks.len(), 2, "the wire saw both rounds");
        let second: Value = serde_json::from_str(&asks[1]).expect("the second request is JSON");
        let messages = second["messages"].as_array().cloned().unwrap_or_default();
        let round = messages
            .iter()
            .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
            .expect("the resent round is an assistant message with its tool calls");
        assert_eq!(
            text_of(&round["content"]),
            "Checking the notes.",
            "the round's text must ride the same assistant message as its calls: {round:#?}"
        );
        assert!(round["tool_calls"].to_string().contains("call_1"), "the call id survives: {round:#?}");
        assert!(
            messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_1"),
            "the tool answer pairs by call id: {messages:#?}"
        );
    }

    #[tokio::test]
    async fn the_stream_opens_with_a_run_event_and_done_repeats_its_id() {
        // The contract: the first frame is a "run" event naming the stable id,
        // so the panel can cancel before the first answer lands; done repeats
        // that id.
        let (app, _captured) = streamed_app(vec![one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let (first, first_data) = events.first().expect("at least one frame").clone();
        assert_eq!(first, "run", "the first frame names the run: {text}");
        let run_id = first_data["runId"].as_i64().expect("runId is a number");
        let (_, done) = events.iter().find(|(name, _)| name == "done").expect("a done frame closes the stream");
        assert_eq!(done["runId"].as_i64(), Some(run_id), "done repeats the stable id: {done}");
    }

    /// The granted names stand in the addendum only beside the shell, which is
    /// the one place a reference resolves; the default group goes unsaid.
    #[test]
    fn the_tool_notes_name_the_granted_secrets_beside_the_shell() {
        let secrets = [("api-key".to_string(), "default".to_string()), ("deploy-token".to_string(), "Deploy".to_string())];
        let text = tool_notes(&["read", "bash"], &secrets);
        assert!(text.contains("Secrets granted to you (name, and group where not default): api-key, deploy-token (Deploy)."), "{text}");
        assert!(text.contains("Write ${secret://name} in a") && text.contains("never single quotes"), "{text}");
        assert!(!tool_notes(&["read"], &secrets).contains("secret"));
        assert!(!tool_notes(&["bash"], &[]).contains("secret"));
        let files = tool_notes(&["read", "edit", "glob"], &[]);
        assert!(files.starts_with("read, edit and glob work inside the working directory"), "{files}");
        assert!(files.contains("read a file before you change it"), "{files}");
        assert!(tool_notes(&["grep"], &[]).starts_with("grep works inside"));
        assert!(!tool_notes(&["read", "glob"], &[]).contains("change real files"));
        assert!(tool_notes(&[], &[]).starts_with("You have no tools"));
    }

    /// The snapshot names the directory once; a send from another directory closes its
    /// turn with a note naming the new one, and the send after it, from there, says
    /// nothing more. A chat agent opens under its prompt alone and asks as given.
    #[test]
    fn a_moved_directory_is_told_in_the_turn_and_a_kept_one_is_not() {
        // The scratch home first: its first use moves the home every read below compares.
        let _ = swiss_core::paths::test_home();
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "tools": ["read", "bash"] }, "asker": { "runtime": "chat", "provider": "local" } }
        }))
        .expect("row"));
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        let agent = state.agent("distiller").expect("agent");
        let chat = agent.chat(&state.config().providers).expect("chat");
        let mut plan = TestPlan::of(&agent, &Map::new(), services.clone()).expect("plan");
        let system = plan.opening("be brief", &chat);
        let home = crate::llm_tools::plain_path(&swiss_core::paths::home_dir());
        assert!(system.starts_with("be brief\n\n# Environment\n"), "{system}");
        assert!(system.contains(&format!("\n- {WORKDIR}{home}\n")), "{system}");
        assert!(system.contains("\n- Shell: ") && system.contains("\n- Today's date: 20"), "{system}");
        assert!(system.contains("\n- You are the agent distiller, on model tiny from provider local\n"), "{system}");
        assert!(system.contains("\n\n# Tools\nread works inside the working directory"), "{system}");
        assert_eq!(plan.task("go", &system, &[]), "go", "the snapshot told the directory");
        let moved = swiss_core::paths::test_home().join(format!("moved-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&moved).expect("dir");
        plan.root = moved.clone();
        let told = plan.task("go on", &system, &[]);
        let now = crate::llm_tools::plain_path(&moved);
        assert!(told.starts_with("go on\n\n<environment>\n"), "{told}");
        assert!(told.ends_with(&format!("\n{WORKDIR}{now}\n</environment>")), "{told}");
        let prior = vec![json!({ "role": "user", "text": told }), json!({ "role": "assistant", "text": "ok" })];
        assert_eq!(plan.task("and then", &system, &prior), "and then", "the moved note told it");
        let asker = state.agent("asker").expect("asker");
        let asking = TestPlan::of(&asker, &Map::new(), services).expect("plan");
        assert_eq!(asking.opening("be brief", &chat), "be brief");
        assert_eq!(asking.task("go", "be brief", &[]), "go");
        let _ = std::fs::remove_dir_all(moved);
    }

    /// No clock cuts a Test: a provider that answers only after 61 s - past the 60 s the
    /// row's retired timeoutMs still names, which once cut it - lands its answer, in the
    /// blocking lane, in the live one the panel streams and in the store-less one. The
    /// wire's paused clock skips the wait; the elapsed check proves each lane really waited.
    #[tokio::test]
    async fn a_test_runs_as_long_as_its_work_takes() {
        let slow = Duration::from_secs(61);
        let (base, _captured) = sse_wire_capturing_after(slow, vec![one_round_answer(); 3]).await;
        let row = json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [], "timeoutMs": 60_000 } }
        });
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&row).expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-long-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        let storeless = LlmState::new();
        storeless.apply(LlmConfig::parse(&row).expect("row"));
        let storeless = mount(storeless);
        let waited = tokio::time::Instant::now();
        let (status, body) = call(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "distiller", "prompt": "take your time" }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((&body["ok"], &body["text"]), (&json!(true), &json!("done.")), "{body}");
        assert!(waited.elapsed() >= slow, "the blocking lane waited the whole 61 s");
        let waited = tokio::time::Instant::now();
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "prompt": "take your time" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (_, done) = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!((&done["ok"], &done["text"]), (&json!(true), &json!("done.")), "{done}");
        assert!(waited.elapsed() >= slow, "the live lane waited the whole 61 s");
        let waited = tokio::time::Instant::now();
        let (status, _ct, text) =
            call_text(&storeless, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "prompt": "take your time" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (_, done) = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!((&done["ok"], &done["text"], &done["runId"]), (&json!(true), &json!("done."), &Value::Null), "{done}");
        assert!(waited.elapsed() >= slow, "the store-less lane waited the whole 61 s");
        history.close().await.expect("close");
    }

    #[test]
    fn the_repository_reads_its_branch_or_detached_head_from_git_files() {
        let top = swiss_core::paths::test_home().join(format!("repo-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(top.join(".git")).expect("git dir");
        std::fs::create_dir_all(top.join("src").join("deep")).expect("dirs");
        std::fs::write(top.join(".git").join("HEAD"), "ref: refs/heads/feature/x\n").expect("head");
        let named = repository(&top.join("src").join("deep")).expect("inside a repository");
        assert_eq!(named, format!("{} (branch feature/x)", crate::llm_tools::plain_path(&top)));
        std::fs::write(top.join(".git").join("HEAD"), "0123456789abcdef0123\n").expect("head");
        assert!(repository(&top).expect("repository").ends_with("(detached at 0123456789ab)"));
        // A worktree's .git file names its real directory.
        let tree = swiss_core::paths::test_home().join(format!("tree-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&tree).expect("tree");
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", top.join(".git").display())).expect("gitdir");
        assert!(repository(&tree).expect("worktree").ends_with("(detached at 0123456789ab)"));
        let _ = (std::fs::remove_dir_all(top), std::fs::remove_dir_all(tree));
    }

    /// The system and user texts of one captured OpenAI-style ask.
    fn system_and_user(ask: &Value) -> (String, String) {
        let messages = ask["messages"].as_array().expect("messages");
        let system = messages.iter().find(|m| m["role"] == "system").expect("a system message");
        let user = messages.iter().rev().find(|m| m["role"] == "user").expect("a user message");
        (text_of(&system["content"]), text_of(&user["content"]))
    }

    /// A builtin conversation opens with its environment snapshot closing the system
    /// prompt - real newlines, the working directory, the tools' notes - and asks the
    /// prompt as given; the run files that prompt. Its continuation replays the system
    /// byte for byte, so the snapshot holds, and from the same directory adds no note.
    #[tokio::test]
    async fn a_streamed_conversation_opens_with_its_snapshot_and_replays_it() {
        let (base, captured) = sse_wire_capturing(vec![one_round_answer(), one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 2, "tools": ["read"] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-sep-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history);
        let app = mount(state);
        let prompt = "scan the notes under the project root";
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "prompt": prompt }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let first: Value = serde_json::from_str(&captured.lock().expect("captured")[0]).expect("the request is JSON");
        let (system, user) = system_and_user(&first);
        let home = crate::llm_tools::plain_path(&swiss_core::paths::home_dir());
        assert!(system.starts_with(swiss_llm::agent::DEFAULT_SYSTEM_PROMPT), "{system}");
        assert!(system.contains("\n\n# Environment\n") && system.contains(&format!("\n- {WORKDIR}{home}\n")), "{system}");
        assert!(system.contains("\n\n# Tools\nread works inside the working directory"), "{system}");
        assert!(!system.contains("\\n"), "no literal backslash-n separator anywhere: {system}");
        assert_eq!(user, prompt, "the prompt is asked as given");
        // The FILED request prompt is the same: read through the public history routes,
        // which flush the owning store.
        let events = events_of(&text);
        let run_id = events.first().expect("the run frame").1["runId"].as_i64().expect("runId");
        let (status, calls) = call(&app, "GET", &format!("/api/llm/history/runs/{run_id}/calls"), None).await;
        assert_eq!(status, StatusCode::OK, "{calls}");
        let id = calls["calls"].as_array().expect("calls").first().expect("the round's call row")["id"].as_i64().expect("call id");
        let (status, body) = call(&app, "GET", &format!("/api/llm/history/calls/{id}/body"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let decoded: Value = serde_json::from_str(body["body"].as_str().expect("sealed body text")).expect("the body decodes");
        assert_eq!(decoded["request"]["prompt"].as_str(), Some(prompt));
        let (status, _ct, text) = call_text(
            &app, "POST", "/api/llm/agents/test/stream",
            Some(json!({ "agent": "distiller", "runId": run_id, "prompt": "and the second ask" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let second: Value = serde_json::from_str(&captured.lock().expect("captured")[1]).expect("the request is JSON");
        let (replayed, asked) = system_and_user(&second);
        assert_eq!(replayed, system, "the continuation replays the snapshot it opened with");
        assert_eq!(asked, "and the second ask", "the same directory adds no note");
    }

    /// The stateless branch (no history store) opens with the same snapshot and asks the
    /// prompt as given.
    #[tokio::test]
    async fn a_stateless_stream_opens_with_the_same_snapshot() {
        let (base, captured) = sse_wire_capturing(vec![one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 2, "tools": ["read"] } }
        }))
        .expect("row"));
        let app = mount(state);
        let prompt = "name the first entry of the home directory";
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "prompt": prompt }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let ask: Value = serde_json::from_str(captured.lock().expect("captured").last().expect("the ask")).expect("the request is JSON");
        let (system, user) = system_and_user(&ask);
        let home = crate::llm_tools::plain_path(&swiss_core::paths::home_dir());
        assert!(system.contains("\n\n# Environment\n") && system.contains(&format!("\n- {WORKDIR}{home}\n")), "{system}");
        assert_eq!(user, prompt, "the stateless ask is the prompt as given");
    }

    /// A chat agent's Test asks exactly as its runtime does in every lane: one
    /// request, no tools, the prompt as given. The live lane once offered it the
    /// Test's tools and the builtin loop; the blocking lane filed its run
    /// with no turns. Both are observed on the wire and in the record.
    #[tokio::test]
    async fn the_test_offers_every_configured_tool_and_honors_a_selected_subset() {
        for selected in [None, Some(json!(["bash", "sync", "delegate"])), Some(json!([]))] {
            let (base, captured) = sse_wire_capturing(vec![one_round_answer()]).await;
            let state = LlmState::new();
            state.apply(LlmConfig::parse(&json!({
                "providers": {"local": {"style": "openai", "baseUrl": base + "/v1", "models": [{"id": "tiny"}]}},
                "agents": {"distiller": {"provider": "local", "maxTurns": 2}}
            })).expect("row"));
            let app = mount(state);
            let mut body = json!({"agent": "distiller"});
            if let Some(tools) = &selected { body["tools"] = tools.clone(); }
            let (status, _, text) = call_text(&app, "POST", "/api/llm/agents/test/stream", Some(body)).await;
            assert_eq!(status, StatusCode::OK, "{text}");
            let asks = captured.lock().expect("captured");
            let ask: Value = serde_json::from_str(&asks[0]).expect("wire");
            let names: Vec<&str> = ask["tools"].as_array().into_iter().flatten()
                .filter_map(|t| t["function"]["name"].as_str()).collect();
            let expected: Vec<&str> = match &selected {
                Some(tools) => tools.as_array().expect("tools").iter().filter_map(Value::as_str).collect(),
                None => Runtime::Builtin.tools().iter().map(|t| t.name).collect(),
            };
            // The bash key reaches the model under the shell's own name (powershell on Windows).
            let expected: Vec<&str> = expected.into_iter().map(|n| if n == "bash" { crate::llm_shell::TOOL } else { n }).collect();
            assert_eq!(names, expected, "the Test must exercise what the agent can take");
        }
    }

    #[tokio::test]
    async fn a_test_refuses_tools_outside_the_agents_selection_before_asking() {
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": {"local": {"style": "openai", "baseUrl": "http://127.0.0.1:1/v1", "models": [{"id": "tiny"}]}},
            "agents": {"test": {"provider": "local", "tools": ["read"]}}
        })).expect("config"));
        let app = mount(state);
        for options in [json!({"tools": ["bash"]}), json!({"tools": ["missing"]}), json!({"tools": ["read", "read"]}),
                        json!({"tools": "read"}), json!({"cwd": "../outside-home"})] {
            for path in ["/api/llm/agents/test", "/api/llm/agents/test/stream"] {
                let mut body = options.clone();
                body["agent"] = json!("test");
                let (status, why) = call(&app, "POST", path, Some(body)).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {why}");
            }
        }
    }

    #[tokio::test]
    async fn a_chat_agent_tests_as_one_request_with_no_tools_in_every_lane() {
        let (base, captured) = sse_wire_capturing(vec![one_round_answer(), one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "ask": { "runtime": "chat", "provider": "local" } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-chat-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        let prompt = "say hello";
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "ask", "prompt": prompt }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (_, done) = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!((done["ok"].clone(), done["turns"].clone()), (json!(true), json!(1)), "{done}");
        let (status, body) =
            call(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "ask", "prompt": prompt }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["ok"].clone(), body["turns"].clone()), (json!(true), json!(1)), "{body}");
        let asks = captured.lock().expect("captured").clone();
        assert_eq!(asks.len(), 2, "one request per lane");
        for ask in &asks {
            let ask: Value = serde_json::from_str(ask).expect("the request is JSON");
            assert!(ask.get("tools").is_none(), "a chat agent is offered no tools: {ask:#?}");
            let user = ask["messages"].as_array().expect("messages").iter().rev().find(|m| m["role"] == "user").expect("a user message");
            assert_eq!(text_of(&user["content"]), prompt, "a chat agent asks the prompt as given");
        }
        let run_id = body["runId"].as_i64().expect("the one-shot's run id");
        let (_, turns) = call(&app, "GET", &format!("/api/llm/history/runs/{run_id}/turns"), None).await;
        assert!(turns.to_string().contains("done."), "the blocking one-shot files its answer as a turn: {turns}");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancel_answers_for_a_known_idle_run_and_404_for_an_unknown_one() {
        // The contract: cancel answers 200 {ok:true, cancelled:false} for a
        // known idle run, 404 for one that is not there.
        let (app, _captured) = streamed_app(vec![one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?page=0&pageSize=10", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let run_id = body["runs"][0]["id"].as_i64().expect("the streamed run filed");
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/cancel", Some(json!({ "runId": run_id }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["ok"].clone(), body["cancelled"].clone()), (json!(true), json!(false)), "{body}");
        let (status, _) = call(&app, "POST", "/api/llm/agents/test/cancel", Some(json!({ "runId": 987654 }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn closing_a_run_is_neutral_and_idempotent() {
        // The locked contract (v1.1): a completed send leaves its conversation
        // OPEN and discoverable; a close is not a failure, answers 200
        // idempotently, and 404 code 'gone' names a run that is not there.
        let (app, _captured) = streamed_app(vec![one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (_, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        let run_id = body["runs"][0]["id"].as_i64().expect("the finished send left its conversation open");
        let uri = format!("/api/llm/history/runs/{run_id}/close");
        let (status, body) = call(&app, "POST", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], json!(true), "{body}");
        let (status, body) = call(&app, "POST", &uri, None).await;
        assert_eq!((status, body["ok"].clone()), (StatusCode::OK, json!(true)), "a second close is a no-op");
        // Closed means closed: the discovery read no longer finds it.
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        assert_eq!((status, body["runs"].clone()), (StatusCode::OK, json!([])), "{body}");
        let (status, body) = call(&app, "POST", "/api/llm/history/runs/987654/close", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["code"], json!("gone"), "{body}");
    }

    #[tokio::test]
    async fn the_runs_list_filters_by_agent_and_open_state() {
        // The decided discovery: the existing envelope, two filters (agent,
        // open), an empty list (never 404) when nothing matches.
        let (app, _captured) = streamed_app(vec![one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1), "{body}");
        // The completed send left its conversation OPEN - continuable across a
        // reload, discoverable by exactly this read (the locked v1.1 contract).
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1), "the conversation is continuable: {body}");
        assert_eq!(body["more"], json!(false), "{body}");
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?agent=nobody&open=true", None).await;
        assert_eq!((status, body["runs"].clone()), (StatusCode::OK, json!([])), "{body}");
        // The reader pressed New: the close empties the open read, the plain read stays whole.
        let (_, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        let run_id = body["runs"][0]["id"].as_i64().expect("still open before the close");
        let (status, _) = call(&app, "POST", &format!("/api/llm/history/runs/{run_id}/close"), None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        assert_eq!((status, body["runs"].clone()), (StatusCode::OK, json!([])), "{body}");
        let (_, body) = call(&app, "GET", "/api/llm/history/runs?agent=distiller", None).await;
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1), "a closed conversation stays readable: {body}");
    }
    /// A wire answer of many text deltas in one burst: the producer never
    /// awaits between the pieces, so forwarding must apply backpressure - a
    /// full channel may slow the stream, never eat real answer text.
    fn burst_answer(pieces: usize) -> String {
        let mut frames = String::new();
        for i in 0..pieces {
            // json! serialization: the literal-form escape dance ({{ }} vs
            // an unmatched }) is gone; the frame is built as data.
            frames.push_str(&format!(
                "data: {}\n\n",
                json!({
                    "id": "c1",
                    "object": "chat.completion.chunk",
                    "model": "tiny",
                    "choices": [{
                        "index": 0,
                        "delta": { "content": format!("p{i} ") },
                        "finish_reason": null
                    }]
                })
            ));
        }
        frames.push_str("data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n");
        frames.push_str("data: [DONE]\n\n");
        frames
    }

    #[tokio::test]
    async fn the_bounded_stream_never_drops_a_burst_of_deltas() {
        // The contract: every delta arrives, in order, and final and done tell
        // the truth about what was said - backpressure, never a skip.
        let pieces = 200;
        let (app, _captured) = streamed_app(vec![burst_answer(pieces)]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let mut flew = String::new();
        for (name, data) in events_of(&text) {
            if name == "delta" && data["kind"] == "text" {
                flew.push_str(data["text"].as_str().unwrap_or_default());
            }
        }
        let expected = (0..pieces).map(|i| format!("p{i} ")).collect::<String>();
        assert_eq!(flew, expected, "every piece of the burst flew, in order");
        let final_frame = events_of(&text).into_iter().find(|(name, _)| name == "final").expect("a final frame");
        assert_eq!(text_of(&final_frame.1["text"]), expected, "final carries the whole answer");
        let done = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!(done.1["ok"], json!(true), "done only says ok when nothing was lost: {:?}", done.1);
    }

    /// A wire that speaks its opening deltas and then falls silent forever: the
    /// promised body never completes, so the only honest end is a cancel that
    /// drops the in-flight request.
    async fn sse_wire_hanging(prefix: String) -> String {
        let (url, _, _) = sse_wire_hanging_ready(prefix, None).await;
        url
    }

    async fn sse_wire_hanging_ready(prefix: String, first: Option<String>) -> (String, tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let (ready, received) = tokio::sync::oneshot::channel();
        let (next_request, captured) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Some(first) = first {
                let (mut socket, _) = listener.accept().await.expect("first round");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.expect("read first round");
                    if n == 0 { return; }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..head_end]);
                    let length = head.lines().find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1)).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                    if buf.len() < head_end + 4 + length { continue; }
                    let answer = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", first.len(), first);
                    socket.write_all(answer.as_bytes()).await.expect("answer first round");
                    break;
                }
            }
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 { break; }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    let tail = "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}
\ndata: [DONE]
\n";
                    // The tail is PROMISED (its length rides the header) but never
                    // sent: the reader can only finish by giving up.
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        prefix.len() + tail.len(),
                        prefix
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write");
                    let _ = ready.send(());
                    break;
                }
            }
            // Keep the first connection open while accepting the continuation.
            let _first = socket;
            let (mut second, _) = listener.accept().await.expect("continuation");
            let mut buf = Vec::new();
            loop {
                let n = second.read(&mut chunk).await.expect("read continuation");
                if n == 0 { return; }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]);
                let length = head.lines().find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1)).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                if buf.len() < head_end + 4 + length { continue; }
                let _ = next_request.send(String::from_utf8_lossy(&buf[head_end + 4..head_end + 4 + length]).into_owned());
                let frames = one_round_answer();
                let answer = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", frames.len(), frames);
                second.write_all(answer.as_bytes()).await.expect("answer continuation");
                break;
            }
        });
        (url, received, captured)
    }

    #[tokio::test]
    async fn cancel_before_any_model_output_keeps_the_conversation_continuable() {
        empty_cancel_remains_continuable(false).await;
    }

    #[tokio::test]
    async fn cancel_after_a_tool_keeps_its_result_in_the_continuation() {
        empty_cancel_remains_continuable(true).await;
    }

    async fn empty_cancel_remains_continuable(after_tool: bool) {
        use futures_util::StreamExt;
        use swiss_llm::history::ReplayState;
        let tool_round = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (base, ready, captured) = sse_wire_hanging_ready(String::new(), after_tool.then(|| tool_round.to_string())).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        })).expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-cancel-empty-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        let request = Request::builder().method("POST").uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "distiller", "prompt": "keep this request" }).to_string())).expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let mut stream = response.into_body().into_data_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await.expect("run frame").expect("frame").expect("bytes");
        let run_id = events_of(&String::from_utf8_lossy(&first)).into_iter()
            .find(|(name, _)| name == "run").and_then(|(_, data)| data["runId"].as_i64()).expect("run id");
        tokio::time::timeout(std::time::Duration::from_secs(10), ready).await.expect("provider was contacted").expect("wire");
        let (status, result) = call(&app, "POST", "/api/llm/agents/test/cancel", Some(json!({ "runId": run_id }))).await;
        assert_eq!((status, result["cancelled"].clone()), (StatusCode::OK, json!(true)));
        tokio::time::timeout(std::time::Duration::from_secs(10), async { while stream.next().await.is_some() {} }).await.expect("cancel settles");
        history.flush().await;
        match history.replay_state(run_id).await {
            ReplayState::Continuable { prior } => {
                assert!(prior.iter().any(|message| message["role"] == "user" && message.to_string().contains("keep this request")), "the cancelled user request remains: {prior:?}");
                if after_tool {
                    assert!(prior.iter().any(|message| message["role"] == "tool" && message["id"] == "call_1"), "the actual tool result remains paired: {prior:?}");
                } else {
                    assert!(!prior.iter().any(|message| message["role"] == "assistant"), "no assistant answer is invented: {prior:?}");
                }
            }
            ReplayState::Refused(why) => panic!("an explicitly cancelled provider wait should remain continuable: {why}"),
            ReplayState::Fresh => panic!("the cancelled request must not disappear"),
        }
        let (status, _, resumed) = call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({
            "agent": "distiller", "runId": run_id, "prompt": "continue now"
        }))).await;
        assert_eq!(status, StatusCode::OK, "{resumed}");
        assert!(events_of(&resumed).iter().any(|(name, data)| name == "done" && data["ok"] == true), "{resumed}");
        let ask = captured.await.expect("the provider saw the continuation");
        assert!(ask.contains("keep this request") && ask.contains("continue now"), "both user requests reach the provider");
        if after_tool {
            let ask: Value = serde_json::from_str(&ask).expect("provider request");
            assert!(ask["messages"].as_array().expect("messages").iter().any(|message| message["role"] == "tool" && message["tool_call_id"] == "call_1"), "the tool result was replayed with its call id");
        }
    }

    #[tokio::test]
    async fn cancel_keeps_the_partial_text_already_seen() {
        // The contract: the partial answer is filed as the round's assistant
        // turn, done says cancelled, and the conversation stays open for a
        // later continuation.
        let prefix = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"The notes name \"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"three ports.\"},\"finish_reason\":null}]}\n\n"
        )
        .to_string();
        let base = sse_wire_hanging(prefix).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history);
        let app = mount(state);

        let request = Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "distiller" }).to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        // Read frame blocks as they fly: first the run id, then both deltas.
        let mut buffer = String::new();
        let mut run_id: Option<i64> = None;
        let mut deltas_seen = 0usize;
        // Bounded even on broken behavior: a hanging wire may never speak, and
        // no red test may run forever - the guard fails it in seconds instead.
        let head = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while run_id.is_none() || deltas_seen < 2 {
                use futures_util::StreamExt;
                // The stream yields Option<Result<Bytes, axum::Error>>: the Option
                // ends the body, the Result must carry bytes - an errored chunk
                // fails with its reason, never silently skipped.
                let Some(piece) = stream.next().await else { break };
                let chunk = piece.expect("the SSE chunk must decode, not error");
                buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find("\n\n") {
                let block: String = buffer.drain(..pos + 2).collect();
                for (name, data) in events_of(&block) {
                    if name == "run" {
                        run_id = data["runId"].as_i64();
                    }
                    if name == "delta" && data["kind"] == "text" {
                        deltas_seen += 1;
                    }
                }
            }
        }
        })
        .await;
        assert!(head.is_ok(), "the run frame and both deltas must arrive, not hang");
        let run_id = run_id.expect("the run frame named its id before the first delta");
        assert_eq!(
            deltas_seen, 2,
            "the panel watched the partial answer fly - response so far: {buffer}"
        );
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/cancel", Some(json!({ "runId": run_id }))).await;
        assert_eq!((status, body["cancelled"].clone()), (StatusCode::OK, json!(true)), "{body}");
        // Drain: the loop aborts its in-flight request, says done cancelled, and
        // the body ends. Bounded: a broken cancel that never settles fails here
        // in seconds instead of hanging the suite.
        let drained = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            use futures_util::StreamExt;
            let mut tail = String::new();
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.expect("the drain must end cleanly, not error");
                tail.push_str(&String::from_utf8_lossy(&bytes));
            }
            tail
        })
        .await;
        assert!(drained.is_ok(), "the stream must END after a cancel - done, then close, never a hang");
        let tail = drained.unwrap_or_default();
        let done = events_of(&tail).into_iter().find(|(name, _)| name == "done").expect("done after the cancel");
        assert_eq!(done.1["cancelled"], json!(true), "{:?}", done.1);
        // The partial text the panel already saw now lives in the record.
        let (_, turns) = call(&app, "GET", &format!("/api/llm/history/runs/{run_id}/turns"), None).await;
        let joined = turns.to_string();
        assert!(joined.contains("The notes name "), "the partial answer is filed, not lost: {joined}");
        assert!(joined.contains("three ports."), "its whole streamed prefix is filed: {joined}");
        let (_, open) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        assert_eq!(open["runs"].as_array().map(Vec::len), Some(1), "a cancelled conversation stays open");
    }

    #[tokio::test]
    async fn words_sent_mid_stream_queue_withdraw_and_are_refused_once_it_ends() {
        // The steer contract: a run in flight takes words for its next round and gives
        // a waiting one back; a run not in flight, or one whose loop has ended, is 409
        // idle - the reader sends the words as its next turn.
        let prefix = "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Looking.\"},\"finish_reason\":null}]}\n\n".to_string();
        let base = sse_wire_hanging(prefix).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-steer-{}", swiss_core::util::random_hex(8)));
        state.install_history(swiss_llm::history::LlmHistory::open(&dir).await.expect("open"));
        let app = mount(state);
        let steer = |run_id: i64, id: &str| json!({ "runId": run_id, "id": id, "text": "also list the ports" });

        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(steer(987654, "a"))).await;
        assert_eq!((status, body["code"].clone()), (StatusCode::CONFLICT, json!("idle")), "{body}");
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(json!({ "runId": 1, "text": "x" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "an id is required: {body}");
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(json!({ "runId": 1, "id": "a" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "so is text: {body}");

        let request = Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "distiller" }).to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut buffer = String::new();
        let mut run_id: Option<i64> = None;
        let mut spoke = false;
        let head = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            use futures_util::StreamExt;
            while run_id.is_none() || !spoke {
                let Some(piece) = stream.next().await else { break };
                buffer.push_str(&String::from_utf8_lossy(&piece.expect("the SSE chunk must decode")));
                while let Some(pos) = buffer.find("\n\n") {
                    let block: String = buffer.drain(..pos + 2).collect();
                    for (name, data) in events_of(&block) {
                        if name == "run" {
                            run_id = data["runId"].as_i64();
                        }
                        spoke |= name == "delta";
                    }
                }
            }
        })
        .await;
        assert!(head.is_ok(), "the run frame and a delta must arrive, not hang");
        let run_id = run_id.expect("the run frame named its id");

        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(steer(run_id, "a"))).await;
        assert_eq!((status, body["queued"].clone()), (StatusCode::OK, json!(true)), "{body}");
        let withdraw = json!({ "runId": run_id, "id": "a" });
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer/withdraw", Some(withdraw.clone())).await;
        assert_eq!((status, body["withdrawn"].clone()), (StatusCode::OK, json!(true)), "{body}");
        let (_, body) = call(&app, "POST", "/api/llm/agents/test/steer/withdraw", Some(withdraw)).await;
        assert_eq!(body["withdrawn"], json!(false), "only once: {body}");
        let (status, _) = call(&app, "POST", "/api/llm/agents/test/steer", Some(steer(run_id, "b"))).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(&app, "POST", "/api/llm/agents/test/cancel", Some(json!({ "runId": run_id }))).await;
        assert_eq!(status, StatusCode::OK);
        let drained = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            use futures_util::StreamExt;
            let mut tail = String::new();
            while let Some(chunk) = stream.next().await {
                tail.push_str(&String::from_utf8_lossy(&chunk.expect("the drain must end cleanly")));
            }
            tail
        })
        .await
        .expect("the stream must end after a cancel");
        assert!(
            !events_of(&drained).iter().any(|(name, _)| name == "steer"),
            "no round followed: the words never went in - {drained}"
        );
        assert!(events_of(&drained).iter().any(|(name, _)| name == "done"), "{drained}");
        let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(steer(run_id, "c"))).await;
        assert_eq!((status, body["code"].clone()), (StatusCode::CONFLICT, json!("idle")), "the loop has ended: {body}");
    }

    #[test]
    fn a_steered_event_is_its_own_frame() {
        let (name, data) = event_frame(swiss_llm::agent::LoopEvent::Steered { id: "q1".into(), text: "and the ports".into() });
        assert_eq!((name, data), ("steer", json!({ "id": "q1", "text": "and the ports" })));
    }

    /// The steer path against a real model (SWISS_LIVE_LLM_*, as memory_live reads them):
    /// words posted while the shell tool sleeps go in after its answer, and the model's
    /// closing answer follows them. Never in a default run.
    /// `cargo test -j 4 -p swiss-plugins --lib live_model_reads -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "calls a real chat model: set SWISS_LIVE_LLM_*"]
    async fn live_model_reads_words_steered_while_its_tool_runs() {
        let Some(live) = crate::memory_live::Live::from_env("LLM") else {
            eprintln!("SWISS_LIVE_LLM_BASE_URL or SWISS_LIVE_LLM_MODEL unset: nothing to ask");
            return;
        };
        let mut provider = json!({ "style": live.style, "baseUrl": live.base_url, "models": [{ "id": live.model, "kind": "chat" }] });
        if let Some(key) = live.key() {
            provider["apiKey"] = json!(key);
        }
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "live": provider },
            "agents": { "steered": { "provider": "live", "maxTurns": 6, "effort": "low" } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-live-steer-{}", swiss_core::util::random_hex(8)));
        state.install_history(swiss_llm::history::LlmHistory::open(&dir).await.expect("open"));
        let app = mount(state);
        let prompt = format!(
            "Call the {} tool exactly once with this command: Start-Sleep -Seconds 6; 'slept'. After it returns, answer in one short sentence.",
            crate::llm_shell::TOOL
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "steered", "prompt": prompt, "tools": ["bash"] }).to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut buffer = String::new();
        let mut run_id: Option<i64> = None;
        let mut events: Vec<(String, Value)> = Vec::new();
        let mut steered_at: Option<usize> = None;
        let words = "One more thing: end your final answer with the exact word PINEAPPLE.";
        let ended = tokio::time::timeout(std::time::Duration::from_secs(240), async {
            use futures_util::StreamExt;
            while let Some(piece) = stream.next().await {
                buffer.push_str(&String::from_utf8_lossy(&piece.expect("the SSE chunk must decode")));
                while let Some(pos) = buffer.find("\n\n") {
                    let block: String = buffer.drain(..pos + 2).collect();
                    for (name, data) in events_of(&block) {
                        if name == "run" {
                            run_id = data["runId"].as_i64();
                        }
                        // The tool is asked: post while it sleeps.
                        if name == "tool" && steered_at.is_none() {
                            let id = run_id.expect("the run frame comes first");
                            let (status, body) = call(&app, "POST", "/api/llm/agents/test/steer", Some(json!({ "runId": id, "id": "live-1", "text": words }))).await;
                            assert_eq!((status, body["queued"].clone()), (StatusCode::OK, json!(true)), "{body}");
                            steered_at = Some(events.len());
                        }
                        if name != "delta" {
                            eprintln!("[{name}] {}", data.to_string().chars().take(160).collect::<String>());
                        }
                        events.push((name, data));
                    }
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "the stream must end within its bound");
        assert!(steered_at.is_some(), "the model never asked the tool: {events:?}");
        let steer = events.iter().position(|(name, _)| name == "steer").expect("a steer frame: the loop took the words");
        assert_eq!(events[steer].1, json!({ "id": "live-1", "text": words }));
        let result = events.iter().position(|(name, _)| name == "toolResult").expect("the tool answered");
        assert!(result < steer, "the words go in after the tool's answer");
        let done = events.iter().position(|(name, _)| name == "done").expect("done");
        assert!(steer < done && events[done].1["ok"] == json!(true), "{:?}", events[done].1);
        let fin = events.iter().find(|(name, _)| name == "final").map(|(_, d)| d["text"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
        eprintln!("final: {fin}");
        assert!(fin.to_uppercase().contains("PINEAPPLE"), "the model followed the steered words: {fin}");
        let (_, turns) = call(&app, "GET", &format!("/api/llm/history/runs/{}/turns", run_id.expect("run")), None).await;
        assert!(turns.to_string().contains("PINEAPPLE"), "the words are filed as a turn: {turns}");
    }

    #[tokio::test]
    async fn continuation_replays_the_exact_wire_ids_verbatim() {
        // The contract: the follow-up request carries the earlier messages
        // EXACTLY as they flew - both call ids verbatim (']', ':' and Unicode
        // inside), their arguments intact, results paired by exact id,
        // nothing synthesized.
        let ask = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Two files.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call]:x:1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"a.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call-ünï\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"b.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (app, captured) = streamed_app(vec![ask, one_round_answer(), one_round_answer()]).await;
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let (_, runs) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        let run_id = runs["runs"][0]["id"].as_i64().expect("the finished send left its conversation open");
        // Engagement guards (additive): the first send must have COMPLETED
        // both tool rounds - done.ok true, turns 2 - and the wire must hold
        // exactly the two round-trip requests before the continuation starts.
        // A silent first-send abort (the choice-index fault class) stops
        // here instead of masquerading as a replay verdict.
        let first_done = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("the first send's done frame");
        assert_eq!(first_done.1["ok"], json!(true), "the first send must complete both tool rounds, not abort: {text}");
        assert_eq!(first_done.1["turns"], json!(2), "two turns: the tool ask, then the answered final: {text}");
        let asks_before = captured.lock().expect("captured").len();
        assert_eq!(asks_before, 2, "two tool-round requests flew before the continuation");
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "runId": run_id }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let asks = captured.lock().expect("captured").clone();
        assert_eq!(asks.len(), 3, "the continuation sent its own request - its stream answered: {text}");
        let third: Value = serde_json::from_str(&asks[2]).expect("the continuation request is JSON");
        let messages = third["messages"].as_array().cloned().unwrap_or_default();
        let round = messages
            .iter()
            .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
            .expect("the earlier round rides along");
        let calls = round["tool_calls"].as_array().cloned().unwrap_or_default();
        assert_eq!(calls.len(), 2, "both calls survive: {round:#?}");
        // ALL assistant tool_calls on the third request, not merely the first
        // matching message: a leaked answered call would pass first-match.
        let mut replayed_ids: Vec<String> = Vec::new();
        for message in &messages {
            if message["role"] == "assistant" {
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        replayed_ids.push(call["id"].as_str().unwrap_or_default().to_string());
                    }
                }
            }
        }
        assert_eq!(replayed_ids.len(), 2, "exactly the two original calls ride along, no duplicates: {messages:#?}");
        assert_eq!(replayed_ids.iter().filter(|id| *id == "call]:x:1").count(), 1, "each original id exactly once: {replayed_ids:?}");
        assert_eq!(replayed_ids.iter().filter(|id| *id == "call-ünï").count(), 1, "each original id exactly once: {replayed_ids:?}");
        assert_eq!(calls[0]["id"], json!("call]:x:1"), "an id with ']' and ':' survives verbatim");
        assert_eq!(calls[0]["function"]["arguments"], json!("{\"path\":\"a.md\"}"), "arguments intact, not guessed back from note text");
        assert_eq!(calls[1]["id"], json!("call-ünï"), "a Unicode id survives verbatim");
        assert_eq!(calls[1]["function"]["arguments"], json!("{\"path\":\"b.md\"}"));
        for id in ["call]:x:1", "call-ünï"] {
            assert!(
                messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == json!(id)),
                "results pair by exact id: {id} in {messages:#?}"
            );
        }
        assert!(!asks[2].contains("replay-"), "no synthesized ids: {}", asks[2]);
    }

    /// Observe an async producer settle: bounded query + yield_now polling of
    /// the HISTORY (side-effect free, no real sleeps - each yield lets the
    /// producer run between looks). This only OBSERVES the settle - order is
    /// proven by the busy gate and the drop act in each test, never by time.
    async fn until_turns_note(app: &Router, run_id: i64, needle: &str, bound: std::time::Duration) -> Option<Value> {
        tokio::time::timeout(bound, async {
            loop {
                let (_, turns) = call(app, "GET", &format!("/api/llm/history/runs/{run_id}/turns"), None).await;
                if turns.to_string().contains(needle) {
                    return Some(turns);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or(None)
    }

    /// A wire that speaks its prefix, then holds the tail behind a oneshot the
    /// TEST controls: the release order is a gate, not a timing guess. Once
    /// released, the tail lands on a connection whose reader may already be
    /// gone - exactly the same-poll buffered-End shape.
    async fn sse_wire_gated(prefix: String, tail: String, gate: tokio::sync::oneshot::Receiver<()>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    let whole = format!("{prefix}{tail}");
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        whole.len(),
                        prefix
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write the prefix");
                    break;
                }
            }
            // Hold the tail until the test opens the gate.
            let _ = gate.await;
            let _ = socket.write_all(tail.as_bytes()).await;
            std::future::pending::<()>().await;
        });
        url
    }

    /// The shared spine of the disconnect reds: open the stream, read the run
    /// frame and one delta (bounded), prove the producer is mid-step (a second
    /// stream for the same agent answers 409 busy), drop the whole reader - and
    /// hand the run id back. What happens after the drop is each test's
    /// contract; nothing is ever asserted off the deleted receiver.
    async fn disconnect_after_first_delta(
        app: &Router,
    ) -> i64 {
        use futures_util::StreamExt;
        let request = Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "distiller" }).to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut buffer = String::new();
        let mut run_id: Option<i64> = None;
        let mut deltas = 0usize;
        let head = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while run_id.is_none() || deltas < 1 {
                let Some(piece) = stream.next().await else { break };
                let chunk = piece.expect("the SSE chunk must decode, not error");
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find("\n\n") {
                    let block: String = buffer.drain(..pos + 2).collect();
                    for (name, data) in events_of(&block) {
                        if name == "run" {
                            run_id = data["runId"].as_i64();
                        }
                        if name == "delta" && data["kind"] == "text" {
                            deltas += 1;
                        }
                    }
                }
            }
        })
        .await;
        assert!(head.is_ok(), "the run frame and first delta must arrive: {buffer}");
        let run_id = run_id.expect("the run frame named its id");
        // The producer is provably mid-step while the reader lives. The gate
        // input reuses THIS run's id: a runId-less second request would be
        // refused as open-run-exists for a different reason and mask the seat
        // signal - continuing the SAME in-flight run can only answer busy.
        let (status, body) =
            call(app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "runId": run_id }))).await;
        assert_eq!((status, body["code"].clone()), (StatusCode::CONFLICT, json!("busy")), "gate: the producer is in flight: {body}");
        // The reader vanishes - the whole body, gone.
        drop(stream);
        run_id
    }

    #[tokio::test]
    async fn a_disconnect_on_a_silent_wire_cancels_the_wait() {
        // The contract: tx.closed() is watched concurrently, the in-flight
        // step aborts, the cancellation is filed truthfully, the seat is
        // released, the conversation stays open. No SSE is asserted - the
        // receiver is deleted.
        let prefix = "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"The notes name \"},\"finish_reason\":null}]}\n\n".to_string();
        let base = sse_wire_hanging(prefix).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history);
        let app = mount(state);
        let run_id = disconnect_after_first_delta(&app).await;
        // Settle observation only (bounded): the cancellation must reach the
        // record within seconds - today it never does, the producer is still
        // waiting on a wire that will never speak again.
        let settled = until_turns_note(&app, run_id, "cancel", std::time::Duration::from_secs(10)).await;
        assert!(settled.is_some(), "the disconnect must cancel the silent wait and file it truthfully");
        // The seat is free again: closing the just-cancelled run answers 200,
        // not 409 busy.
        let (status, body) = call(&app, "POST", &format!("/api/llm/history/runs/{run_id}/close"), None).await;
        assert_eq!((status, body["ok"].clone()), (StatusCode::OK, json!(true)), "seat released after the disconnect cancel: {body}");
    }

    #[tokio::test]
    async fn a_disconnect_releases_a_buffered_end_without_forging_success() {
        // The contract: a round whose delivery broke is CANCELLED in the
        // record - completed facts stay, success is never forged, the run
        // stays open.
        let prefix = "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"The notes name \"},\"finish_reason\":null}]}\n\n".to_string();
        let tail = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"three ports.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let base = sse_wire_gated(prefix, tail, gate_rx).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history);
        let app = mount(state);
        let run_id = disconnect_after_first_delta(&app).await;
        // NOW release the end onto a connection whose reader is already gone -
        // the gate proves the drop happened first.
        let _ = gate_tx.send(());
        let settled = until_turns_note(&app, run_id, "cancel", std::time::Duration::from_secs(10)).await;
        assert!(
            settled.is_some(),
            "a broken delivery must be recorded as cancelled - the buffered end may not forge a successful round after the reader vanished"
        );
        // Completed facts stay; success is not forged; the run stays open.
        let joined = settled.expect("settled").to_string();
        assert!(joined.contains("The notes name "), "the observed prefix stays filed: {joined}");
        let (_, open) = call(&app, "GET", "/api/llm/history/runs?agent=distiller&open=true", None).await;
        assert_eq!(open["runs"].as_array().map(Vec::len), Some(1), "the run stays open for a later send");
    }

    #[tokio::test]
    async fn a_refused_reservation_means_no_provider_ask() {
        // The contract (ordering + admission): an unadmittable send means ZERO
        // provider traffic - the reservation and its pre-outbound activity
        // fact are admitted BEFORE anything flies. The wedge is a zero-share
        // quota policy: every admitted submit refuses deterministically,
        // forever - no queue filling, no drain race across the route's awaits.
        let (base, captured) = sse_wire_capturing(vec![one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        // From here on, no admitted write can land.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
            std::collections::BTreeMap::new(),
        ));
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        let _ = (status, text);
        let asks = captured.lock().expect("captured").clone();
        assert!(
            asks.is_empty(),
            "a send whose writes are not admissible must never reach the provider - {} ask(s) flew: {}",
            asks.len(),
            asks.first().map(|a| a.chars().take(120).collect::<String>()).unwrap_or_default()
        );
    }

    #[tokio::test]
    async fn a_failed_blocking_test_still_files_its_run() {
        // The contract (blocking route ordering): the reservation precedes the
        // provider, so even a failed ask is filed as its own one-shot run.
        // Deterministic: the provider endpoint is a dead port (connection
        // refused instantly).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let base = format!("http://127.0.0.1:{port}");
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        let sent = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            call_text(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "distiller" }))).await
        })
        .await;
        assert!(sent.is_ok(), "the refused-connection ask must fail fast, not hang");
        let (_, page) = call(&app, "GET", "/api/llm/history/runs?agent=distiller", None).await;
        let rows = page["runs"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            rows.len(),
            1,
            "a failed provider ask must still be filed as its run - reserve before the ask: {page}"
        );
    }

    #[tokio::test]
    async fn an_explicit_run_id_without_a_history_store_refuses_before_asking() {
        // The contract: with the history store unavailable, a fresh send stays
        // allowed, but an explicit runId refuses BEFORE any provider call -
        // never a silent stateless one-shot under that id (a fork by another
        // door).
        let (base, captured) = sse_wire_capturing(vec![one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        // Deliberately NO history store.
        let app = mount(state);
        let (status, _ct, text) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "runId": 12345 }))).await
        })
        .await
        .expect("bounded: the refusal or the stateless stream must settle within seconds");
        let asks = captured.lock().expect("captured").clone();
        assert!(
            asks.is_empty(),
            "an explicit runId without a history store must never reach the provider - {} ask(s) flew: {}",
            asks.len(),
            asks.first().map(|a| a.chars().take(120).collect::<String>()).unwrap_or_default()
        );
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "the refusal belongs to the no-history family, not a silent stateless 200: {text}"
        );
    }

    #[tokio::test]
    async fn the_stateless_stream_never_drops_a_burst_of_deltas() {
        // The bounded-history lane witnesses losslessness on the HISTORY-BACKED
        // path; this is the same burst through the no-store branch, so both
        // native paths carry the lossless contract rather than a
        // source-similarity claim.
        let pieces = 200;
        let (base, _captured) = sse_wire_capturing(vec![burst_answer(pieces)]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        // No history store: the stateless lane serves this stream.
        let app = mount(state);
        let (status, _ct, text) =
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let mut flew = String::new();
        for (name, data) in events_of(&text) {
            if name == "delta" && data["kind"] == "text" {
                flew.push_str(data["text"].as_str().unwrap_or_default());
            }
        }
        let expected = (0..pieces).map(|i| format!("p{i} ")).collect::<String>();
        assert_eq!(flew, expected, "every piece of the stateless burst flew, in order");
        let done = events_of(&text).into_iter().find(|(name, _)| name == "done").expect("a done frame");
        assert_eq!(done.1["ok"], json!(true), "done only says ok when nothing was lost: {:?}", done.1);
    }

    /// The quota-flip no-fork wire: serves the FIRST response only after the
    /// test releases it (receipt-gated), then serves the SECOND response to
    /// the next connection. Both requests are captured - the ask counter is
    /// the outbound oracle.
    async fn sse_wire_holding(
        held: String,
        second: String,
        ask_seen: std::sync::Arc<tokio::sync::Notify>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let held_captured = captured.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let responses = [held, second];
            let mut release = Some(release);
            for (i, body) in responses.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.expect("read");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let length = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + length {
                        held_captured.lock().expect("captured").push(String::from_utf8_lossy(&buf).into_owned());
                        if i == 0 {
                            // Receipt gate: the ask is provably on the wire
                            // BEFORE the test flips the quota.
                            ask_seen.notify_one();
                            if let Some(gate) = release.take() {
                                let _ = gate.await;
                            }
                        }
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        socket.write_all(answer.as_bytes()).await.expect("write");
                        break;
                    }
                }
            }
            std::future::pending::<()>().await;
        });
        (url, captured)
    }

    #[tokio::test]
    async fn a_used_run_whose_writes_were_all_refused_still_continues() {
        // The contract: a used-but-undurable run - its outbound REALLY
        // happened (receipt-gated on the wire) but its call/finish writes
        // were all refused against a quota emptied mid-flight - is never a
        // dead end. Its drive is gone, so the door releases the stale mark
        // and the next send continues the same conversation; the model's
        // context starts there, since no round of it was ever sealed.
        use futures_util::StreamExt;
        let ask_seen = std::sync::Arc::new(tokio::sync::Notify::new());
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let (base, captured) = sse_wire_holding(one_round_answer(), one_round_answer(), ask_seen.clone(), gate_rx).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        let request = Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/stream")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "agent": "distiller" }).to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut buffer = String::new();
        let mut run_id: Option<i64> = None;
        let head = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            // The run frame is enough here: the receipt gate below proves the
            // outbound independently of any delta timing.
            while run_id.is_none() {
                let Some(piece) = stream.next().await else { break };
                let chunk = piece.expect("the SSE chunk must decode, not error");
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find("\n\n") {
                    let block: String = buffer.drain(..pos + 2).collect();
                    for (name, data) in events_of(&block) {
                        if name == "run" {
                            run_id = data["runId"].as_i64();
                        }
                    }
                }
            }
        })
        .await;
        assert!(head.is_ok(), "the run frame must arrive: {buffer}");
        let run_id = run_id.expect("the run frame named its id");
        // ORDER GATE: the provider request is provably on the wire before the
        // quota flips - no sleeps, no counters.
        let seen = tokio::time::timeout(std::time::Duration::from_secs(10), ask_seen.notified()).await;
        assert!(seen.is_ok(), "the outbound must be witnessed on the wire");
        // NOW every admitted write refuses: the completing call/finish
        // records of this very real send cannot land.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
            std::collections::BTreeMap::new(),
        ));
        let _ = gate_tx.send(());
        // Settle: the stream itself must end (bounded).
        let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(piece) = stream.next().await {
                let chunk = piece.expect("the drain must end cleanly, not error");
                buffer.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await;
        assert!(drained.is_ok(), "the held response must complete the stream within the bound");
        assert!(buffer.contains("event: done"), "the producer must settle honestly: {buffer}");
        // Restore admission; the refusal under test is about DURABILITY, not
        // about the quota still being wedged.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::default());
        let (status, _ct, text) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            call_text(&app, "POST", "/api/llm/agents/test/stream", Some(json!({ "agent": "distiller", "runId": run_id }))).await
        })
        .await
        .expect("bounded: the continuation must settle within seconds");
        let asks = captured.lock().expect("captured").clone();
        assert_eq!(status, StatusCode::OK, "the stranded conversation continues: {text}");
        assert!(text.contains("event: done"), "the continuation runs to its end: {text}");
        assert_eq!(asks.len(), 2, "the continuation asked the provider again under the same id");
    }

    #[tokio::test]
    async fn a_refused_blocking_reservation_means_no_provider_ask() {
        // The contract (blocking route): the reservation - and its
        // pre-outbound activity fact - precede the provider, so a refused
        // write means ZERO outbound on the BLOCKING test too. The dead-port
        // test is the ordering guard; this is the refusal-safety witness.
        let (base, captured) = sse_wire_capturing(vec![one_round_answer()]).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } },
            "agents": { "distiller": { "provider": "local", "maxTurns": 4, "tools": [] } }
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-api-red-{}", swiss_core::util::random_hex(8)));
        let history = swiss_llm::history::LlmHistory::open(&dir).await.expect("open");
        state.install_history(history.clone());
        let app = mount(state);
        // Every admitted write refuses, deterministically, from here on.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
            std::collections::BTreeMap::new(),
        ));
        let sent = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            call_text(&app, "POST", "/api/llm/agents/test", Some(json!({ "agent": "distiller" }))).await
        })
        .await;
        assert!(sent.is_ok(), "bounded: the refusal or the answer must settle within seconds");
        let asks = captured.lock().expect("captured").clone();
        assert!(
            asks.is_empty(),
            "an unadmittable blocking test must never reach the provider - {} ask(s) flew: {}",
            asks.len(),
            asks.first().map(|a| a.chars().take(120).collect::<String>()).unwrap_or_default()
        );
    }}
