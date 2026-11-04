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


//! The Data agent's HTTP routes: one connection's conversation, its stream, and compaction
//! (SPEC §data.agent).

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use swiss_host::mongobrowser::MongoBrowser;
use swiss_host::reply::admin_error;
use swiss_host::services::catalog::CatalogRegistry;
use swiss_host::services::RuntimeServices;
use swiss_llm::agent::{AgentConfig, Effort, Runtime, Toolbox};
use swiss_llm::conversation::{self, LaneError, COMPACT_FRAMING, SNAPSHOT_HEADING};
use swiss_llm::history::{within, CancelFlag, LlmHistory, ReplayState, RunContext, Steer};
use swiss_llm::provider::{Chat, ModelKind};
use swiss_llm::LlmState;

use crate::llm_api::{awaited_sink, bad, body_of, field, loop_answer, ok, refuse, sse_of, sse_send, LOOP_TIMEOUT, SSE_FRAMES};
use crate::llm_db_memory::DbMemory;

use super::{
    BUSY, CHANNELS_NAMED, DATABASES_NAMED, DB_COMPACT_INSTRUCTION, FOCUS_MAX, HISTORY_PAGE, PROMPT_MAX,
    RUN_LIMIT, TABLES_NAMED, TASK,
};
use super::text::*;
use super::tools::*;

/// The scope a connection's conversation files and is found under.
pub(super) fn scope(conn: &str) -> RunContext {
    RunContext { task: Some(TASK.into()), subject: Some(conn.into()), ..RunContext::default() }
}

/// The system prompt a new Data conversation opens with: the agent's prompt in force (`base`),
/// then the snapshot - the connection and its dialect, what is in view on the page (a SQL
/// schema, a MongoDB database), what holds the data (the tables, the collections, the
/// keyspace), the day, the agent - and the connection's memory: how to use it, its index. A
/// continuation replays the one it opened with.
pub(super) async fn opening(catalog: &CatalogRegistry, conn: &str, schema: Option<&str>, base: &str, agent: &str, chat: &swiss_llm::provider::Chat, writes: Writes) -> String {
    let mut facts = Vec::new();
    match lease(catalog, conn) {
        Ok((held, browser)) => {
            let dialect = held.dialect().to_string();
            match browser {
                Browser::Sql(browser) => {
                    let dialect = browser.dialect().as_str();
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes(dialect)));
                    facts.push(format!("- {}", writes.fact()));
                    if let Some(schema) = schema {
                        facts.push(format!("- Schema in view on the page: {schema}"));
                    }
                    let mut o = json!({ "limit": TABLES_NAMED });
                    if let Some(schema) = schema { o["schema"] = json!(schema); }
                    match browser.list_tables(&o).await {
                        Ok(listing) => facts.push(tables_fact(&listing)),
                        Err(why) => facts.push(format!("- Tables: not listed as the conversation began ({why})")),
                    }
                }
                Browser::Mongo(browser) => {
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes("mongo")));
                    mongo_facts(browser.as_ref(), schema, &mut facts).await;
                }
                Browser::Redis(browser) => {
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes("redis")));
                    match browser.list_databases().await {
                        Ok(listing) => facts.push(keyspace_fact(&listing)),
                        Err(why) => facts.push(format!("- Keyspace: not read as the conversation began ({why})")),
                    }
                    match browser.pubsub_channels(&json!({})).await {
                        Ok(listing) => facts.push(pubsub_fact(&listing)),
                        Err(why) => facts.push(format!("- Pub/sub: not read as the conversation began ({why})")),
                    }
                }
            }
        }
        Err(why) => facts.push(format!("- Connection: {conn} (not reachable as the conversation began: {why})")),
    }
    facts.push(format!("- Today's date: {}", chrono::Local::now().format("%Y-%m-%d (%A)")));
    facts.push(format!("- {}", answering(agent, chat)));
    format!(
        "{base}\n\n{SNAPSHOT_HEADING}\nThese facts were taken as this conversation began; check with your tools before relying on one that may have changed since.\n{}\n\n{}",
        facts.join("\n"),
        DbMemory::of(conn).prompt_section()
    )
}

/// The snapshot's tables line from a listing asked for TABLES_NAMED: every name when they
/// all came back, else only how many there are.
pub(super) fn tables_fact(listing: &Value) -> String {
    let names: Vec<String> = listing["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| format!("{}.{}", t["schema"].as_str().unwrap_or_default(), t["name"].as_str().unwrap_or_default()))
        .collect();
    let total = listing["total"].as_i64().unwrap_or(names.len() as i64);
    if names.len() as i64 >= total {
        format!("- Tables ({total}): {}", names.join(", "))
    } else {
        format!("- Tables: {total}, too many to name here; list_tables finds them by name (grep)")
    }
}

/// The snapshot's MongoDB lines: the database the tools default to (the one in view, else
/// the connection's own), its collections, and the connection's other databases.
pub(super) async fn mongo_facts(browser: &dyn MongoBrowser, in_view: Option<&str>, facts: &mut Vec<String>) {
    let listing = browser.list_databases().await;
    let db = in_view
        .map(str::to_string)
        .or_else(|| listing.as_ref().ok().and_then(|listing| listing["current"].as_str().map(str::to_string)));
    match &db {
        Some(db) if in_view.is_some() => facts.push(format!("- Database in view on the page (the tools' default): {db}")),
        Some(db) => facts.push(format!("- Database (the tools' default): {db}")),
        None => facts.push("- Database: none in view; name db in each call".into()),
    }
    if let Some(db) = &db {
        match browser.list_collections(db).await {
            Ok(listing) => facts.push(collections_fact(&listing)),
            Err(why) => facts.push(format!("- Collections: not listed as the conversation began ({why})")),
        }
    }
    match listing {
        Ok(listing) => {
            let names: Vec<&str> = listing["databases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["system"] != true)
                .filter_map(|row| row["name"].as_str())
                .collect();
            if names.len() <= DATABASES_NAMED {
                facts.push(format!("- Databases ({}): {}", names.len(), names.join(", ")));
            } else {
                facts.push(format!("- Databases: {}", names.len()));
            }
        }
        Err(why) => facts.push(format!("- Databases: not listed as the conversation began ({why})")),
    }
}

/// The snapshot's collections line: every name while there are TABLES_NAMED or fewer, else
/// only how many - as tables_fact.
pub(super) fn collections_fact(listing: &Value) -> String {
    let names: Vec<&str> = listing["collections"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).collect();
    if names.len() <= TABLES_NAMED {
        format!("- Collections ({}): {}", names.len(), names.join(", "))
    } else {
        format!("- Collections: {}, too many to name here; list_collections finds them by name (grep)", names.len())
    }
}

/// The snapshot's Redis line: the logical database the connection sits on and its key count,
/// and the others that hold keys, out of this connection's reach.
pub(super) fn keyspace_fact(listing: &Value) -> String {
    let rows = listing["databases"].as_array().cloned().unwrap_or_default();
    let current = listing["current"].as_str().unwrap_or("0");
    let keys = |row: &Value| row["tables"].as_i64().unwrap_or(0);
    let here = rows.iter().find(|row| row["name"] == current).map(keys).unwrap_or(0);
    let mut fact = format!("- Database: db{current}, {here} key(s)");
    let others: Vec<String> = rows
        .iter()
        .filter(|row| row["name"] != current)
        .map(|row| format!("db{} {}", row["name"].as_str().unwrap_or("?"), keys(row)))
        .collect();
    if !others.is_empty() {
        fact.push_str(&format!("; others hold keys ({}), out of this connection's reach", others.join(", ")));
    }
    fact
}

/// The snapshot's pub/sub line: the channels with a subscriber - named while there are
/// CHANNELS_NAMED or fewer - and the pattern subscriptions.
pub(super) fn pubsub_fact(listing: &Value) -> String {
    let names: Vec<&str> = listing["channels"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).collect();
    let total = listing["total"].as_i64().unwrap_or(names.len() as i64);
    let patterns = listing["patterns"].as_i64().unwrap_or(0);
    let channels = if total == 0 {
        "no channel with a subscriber".to_string()
    } else if total as usize <= CHANNELS_NAMED && names.len() as i64 == total {
        format!("{total} channel(s) with a subscriber ({})", names.join(", "))
    } else {
        format!("{total} channel(s) with a subscriber")
    };
    format!("- Pub/sub: {channels}, {patterns} pattern subscription(s); list_channels and listen_channels read them")
}

/// The snapshot line naming the agent and the model answering, in the opening and in a
/// later turn's note alike: the last one the dialogue holds is what the model was last told.
pub(super) const ANSWERING: &str = "You are the agent ";

pub(super) fn answering(agent: &str, chat: &Chat) -> String {
    format!("{ANSWERING}{agent}, on model {} from provider {}", chat.model, chat.provider)
}

/// The words a send asks with: the reader's, and a note after them for what the dialogue
/// last named otherwise - the model answering, when the reader moved the conversation to
/// another in the pane's menu, and on a SQL connection (`writes`) how writes go, when the
/// reader flipped the pane's switch. The system replays as noted, so the opening's lines
/// stay; the last line standing is what the model reads as true.
pub(super) fn asked(prompt: &str, system: &str, prior: &[Value], agent: &str, chat: &Chat, writes: Option<Writes>) -> String {
    let users = prior.iter().filter(|entry| entry["role"] == "user").filter_map(|entry| entry["text"].as_str());
    let lines: Vec<&str> = std::iter::once(system).chain(users).flat_map(str::lines).map(|line| line.trim_start_matches("- ")).collect();
    let told = |head: &str| lines.iter().rfind(|line| line.starts_with(head)).copied();
    let mut notes = Vec::new();
    let now = answering(agent, chat);
    if told(ANSWERING).is_some_and(|told| told != now) {
        notes.push(format!("The model answering changed since you were last told.\n- {now}"));
    }
    if let Some(writes) = writes {
        let now = writes.fact();
        // A conversation older than the switch was never told: its writes were staged.
        if told(WRITES).map_or(writes != Writes::Staged, |told| told != now) {
            notes.push(format!("The write mode changed since you were last told.\n- {now}"));
        }
    }
    if notes.is_empty() {
        return prompt.to_string();
    }
    format!("{prompt}\n\n<environment>\n{}\n</environment>", notes.join("\n"))
}

/// The model and the effort the reader picked in the pane's menu for what a route starts
/// (SPEC §data.agent Model); each one absent is the agent's own.
#[derive(Default)]
pub(super) struct Pick {
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<Effort>,
}

impl Pick {
    /// The body's provider, model and effort (low, medium, high or max).
    pub(super) fn of(body: &serde_json::Map<String, Value>) -> Result<Pick, String> {
        let effort = match field(body, "effort")? {
            None => None,
            Some(word) => Some(Effort::parse(&word).ok_or_else(|| format!("effort must be low, medium, high or max, got {word:?}"))?),
        };
        Ok(Pick { provider: field(body, "provider")?, model: field(body, "model")?, effort })
    }

    /// Whether it names a model: the conversation it continues moves to it (see
    /// [conversation::retarget]).
    pub(super) fn moves(&self) -> bool {
        self.provider.is_some() || self.model.is_some()
    }
}

/// What every Data route that drives a conversation works from: the db agent, its prompt
/// in force (DB_AGENT_PROMPT and the agent's own after it - what a continuation must still
/// match), the history, the chat recording into it, the connection's seat - one stream per
/// agent and connection, the agent's other connections beside it - and its kind, which
/// names the conversation's tools.
pub(super) struct Desk {
    pub(super) agent: AgentConfig,
    pub(super) base: String,
    pub(super) history: Arc<LlmHistory>,
    pub(super) chat: Chat,
    pub(super) seat: String,
    pub(super) kind: DbKind,
}

/// The checks every such route makes before anything reserves: a db agent, a MySQL,
/// PostgreSQL, SQLite, MongoDB or Redis connection, the history open, a chat for the agent's provider
/// and model at its effort - or the ones the reader picked: a provider swiss has (its first
/// chat model unless the pick names one, when it is not the agent's), one of its chat models.
#[allow(clippy::result_large_err)]
pub(super) fn desk(state: &LlmState, services: &RuntimeServices, name: &str, conn: &str, pick: &Pick) -> Result<Desk, Response> {
    let mut agent = state.agent(name).map_err(bad)?;
    if agent.runtime != Runtime::Db {
        return Err(bad(format!(
            "{name} is a {} agent; the Data page works with a DB agent only (runtime db), made for it on the Agents page",
            agent.runtime.as_str()
        )));
    }
    let base = agent.system_prompt().map(|prompt| prompt.into_owned()).unwrap_or_default();
    let kind = lease(&services.catalog, conn).map_err(bad)?.1.kind();
    let Some(history) = state.history() else {
        return Err(admin_error(StatusCode::SERVICE_UNAVAILABLE, "the Data agent needs the llm history store; this home has none"));
    };
    let config = state.config();
    if let Some(provider) = &pick.provider {
        if !config.providers.contains_key(provider) {
            return Err(bad(format!("provider: swiss has no provider {provider:?}")));
        }
        if agent.provider.as_ref() != Some(provider) {
            agent.model = None;
        }
        agent.provider = Some(provider.clone());
    }
    if let Some(model) = &pick.model {
        let provider = agent.provider.clone().unwrap_or_default();
        let served = config.providers.get(&provider).is_some_and(|entry| entry.models_of(ModelKind::Chat).any(|one| one == model));
        if !served {
            return Err(bad(format!("model: {model:?} is not a chat model of provider {provider:?}")));
        }
        agent.model = Some(model.clone());
    }
    if pick.effort.is_some() {
        agent.effort = pick.effort;
    }
    let chat = agent.chat(&config.providers).map_err(bad)?.recording(Some(history.clone()), Some(&agent.name));
    Ok(Desk { agent, base, history, chat, seat: format!("{name}@data:{conn}"), kind })
}

/// The body's runId: absent or null is None.
pub(super) fn run_id_of(body: &serde_json::Map<String, Value>) -> Result<Option<i64>, &'static str> {
    match body.get("runId") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or("runId must be an integer"),
    }
}

/// The body's agent and connection, both required.
#[allow(clippy::result_large_err)]
pub(super) fn agent_and_conn(body: &serde_json::Map<String, Value>) -> Result<(String, String), Response> {
    match (field(body, "agent"), field(body, "conn")) {
        (Ok(Some(name)), Ok(Some(conn))) => Ok((name, conn)),
        (Ok(None), _) => Err(bad("agent is required")),
        (_, Ok(None)) => Err(bad("conn is required")),
        (Err(why), _) | (_, Err(why)) => Err(bad(why)),
    }
}

/// A conversation guard's refusal as the route answers it.
pub(super) fn lane_refusal(why: LaneError) -> Response {
    match why {
        LaneError::Refused { code, message } => refuse(code, message),
        LaneError::Unavailable(why) => admin_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

/// A seat held for a step that streams nothing (a reopen) or takes no words (a
/// compaction): its steer inbox shut from the start, so a queued line is never posted
/// into it.
pub(super) fn shut_steer() -> Arc<Steer> {
    let steer = Steer::new();
    steer.close();
    steer
}

/// POST /api/llm/db/stream {agent, conn, prompt, runId?, schema?, rowLimit?, provider?, model?,
/// effort?, writes?, consoleTabs?}: one send of a Data
/// conversation, as server-sent events - the Test stream's frames (run, delta, tool,
/// toolResult, final, done) plus `db` for each statement or command the agent runs ({kind:
/// "run", sql, db?, tab?, reply | error}) or stages ({kind: "stage", sql, db?, tab?}) and each
/// console tab it opens ({kind: "console", tab, tabId?}) - sql is the text the console takes, a
/// MongoDB command's JSON or a Redis line, db a MongoDB command's database, and tab the name of
/// the agent's console tab it lands in (none: the Agent tab). One statement of a console tab
/// edited or dropped is {kind: "consoleEdit", tabId, op: "replace" | "delete", index, sql?}.
/// Without runId it opens the
/// connection's conversation (an open one refuses 409 open-run-exists); with it, it
/// continues that one, which must be this agent's on this connection (else 409 gone).
/// Only a db agent works here, and only on a MySQL, PostgreSQL, MongoDB or Redis connection.
/// schema is the page's schema in view, or its database on MongoDB. rowLimit is the
/// console's: a run without a limit of its own brings back that many rows or documents.
/// consoleTabs is the console's tabs, open and closed, with their ids, names and statements -
/// the page's own record, which list_console_tabs and read_console_tab answer from.
/// provider, model and effort are the pane's menu (each absent the agent's own): a
/// continuation moves to that model, the model told so after the reader's words. writes is
/// the pane's switch on a SQL connection, stage (absent) or auto (SPEC §data.agent Writes): in
/// auto a write run_sql is given runs when narrow ({kind: "run", ..., wrote: true}) or is held
/// ({kind: "stage", ..., held}); the model is told a change the same way.
pub async fn stream(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, Extension(analysis): Extension<Arc<crate::analysis::AnalysisStore>>, Extension(consoles): Extension<Arc<crate::db_consoles::ConsoleStore>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "prompt", "runId", "schema", "rowLimit", "provider", "model", "effort", "writes", "consoleTabs", "consoleTabsPartial", "analysisView", "pageContext", "consoleStore"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn, schema) = match (field(&body, "agent"), field(&body, "conn"), field(&body, "schema")) {
        (Ok(Some(name)), Ok(Some(conn)), Ok(schema)) => (name, conn, schema),
        (Ok(None), _, _) => return bad("agent is required"),
        (_, Ok(None), _) => return bad("conn is required"),
        (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => return bad(why),
    };
    let prompt = match field(&body, "prompt") {
        Ok(Some(prompt)) if prompt.chars().count() <= PROMPT_MAX => prompt,
        Ok(Some(_)) => return bad(format!("prompt: at most {PROMPT_MAX} characters")),
        Ok(None) => return bad("prompt is required"),
        Err(why) => return bad(why),
    };
    let joining = match run_id_of(&body) {
        Ok(joining) => joining,
        Err(why) => return bad(why),
    };
    let mut references = serde_json::Map::new();
    if let Some(view) = body.get("analysisView").filter(|v| !v.is_null()) {
        match crate::llm_db_analysis::view_context(&analysis, &conn, view) {
            Ok(context) => { references.insert("analysis".into(), context); },
            Err(why) => return bad(why),
        }
    }
    match page_context(&body, &conn) {
        Ok(Some(_)) => {
            if let Some(page) = body["pageContext"].as_object() { references.extend(page.clone()); }
        },
        Ok(None) => {},
        Err(why) => return bad(why),
    }
    let row_limit = match body.get("rowLimit") {
        None | Some(Value::Null) => RUN_LIMIT,
        Some(value) => match value.as_i64() {
            Some(rows) if rows >= 1 => rows,
            _ => return bad("rowLimit must be a positive integer"),
        },
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let writes = match Writes::of(&body) {
        Ok(writes) => writes,
        Err(why) => return bad(why),
    };
    let use_console_store = match body.get("consoleStore") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        _ => return bad("consoleStore must be a boolean"),
    };
    // The console's tabs, open and closed, as the page holds them: the only place they exist.
    let console_tabs = match console_tabs_of(&body) {
        Ok(tabs) => tabs,
        Err(why) => return bad(why),
    };
    let console_tabs_partial = match body.get("consoleTabsPartial") {
        None => false,
        Some(Value::Bool(partial)) => *partial,
        _ => return bad("consoleTabsPartial must be a boolean"),
    };
    let Desk { agent, base, history, chat, seat, kind } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    if writes == Writes::Auto && kind != DbKind::Sql {
        return bad("writes: auto runs on a MySQL or PostgreSQL connection only");
    }
    if state.seat_run_of(&seat).is_some() {
        return refuse("busy", BUSY);
    }
    if let Some(id) = joining.filter(|_| pick.moves()) {
        if let Err(why) = within(scope(&conn), conversation::retarget(&history, id, &name, &chat)).await {
            return lane_refusal(why);
        }
    }
    let lane = match within(scope(&conn), conversation::join(&history, joining, &name, agent.runtime.as_str(), &chat, &base)).await {
        Ok(lane) => lane,
        Err(why) => return lane_refusal(why),
    };
    let conversation::Lane { mut run, run_id, continued, prior, system: noted } = lane;
    let system = match noted {
        Some(noted) => noted,
        None => opening(&services.catalog, &conn, schema.as_deref(), &base, &name, &chat, writes).await,
    };
    let (prompt, reference_patch) = page_references(&prompt, &prior, references);
    let prompt = asked(&prompt, &system, &prior, &name, &chat, (kind == DbKind::Sql).then_some(writes));
    let cancel = CancelFlag::new();
    let steer = Steer::new();
    if !state.seat(&seat, run_id, cancel.clone(), steer.clone()) {
        return refuse("busy", BUSY);
    }
    run.set_cancel(cancel.clone());
    run.set_steer(steer.clone());
    let session = state.session(&agent, TASK);
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    sse_send(&tx, "run", json!({
        "runId": run_id, "agent": name, "conn": conn, "provider": chat.provider, "model": chat.model,
        "continued": continued,
    }));
    // What went with the reader's words, as the model reads it: the page shows the same patch
    // under the asked turn that a restored conversation parses back out of the prompt.
    if let Some(sent) = reference_patch.as_ref().and_then(|patch| patch.get(TASK)) {
        sse_send(&tx, "references", json!({ "references": sent }));
    }
    let started = Instant::now();
    let max_turns = agent.turns();
    tokio::spawn(async move {
        // A MongoDB conversation works in the database in view unless the model names another.
        let in_view = if kind == DbKind::Mongo { schema } else { None };
        let mut tools = DbTools::new(services.catalog.clone(), &conn, kind, Some(tx.clone()))
            .limited(row_limit)
            .in_db(in_view)
            .writing(writes)
            .showing(console_tabs)
            .partially_showing(console_tabs_partial);
        tools.analysis = Some(analysis);
        if use_console_store { tools.consoles = Some(consoles); }
        tools.references = reference_patch;
        let mut on_event = awaited_sink(&tx, &cancel);
        // The drive in its own block, as the Test's: the pinned future holds the recorder,
        // and the timeout filing below borrows it only once the future has dropped.
        let driven = {
            let looped = swiss_llm::agent::run_loop_continued(
                &chat, &system, &prior, &prompt, &mut tools, max_turns, &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            tokio::time::timeout(LOOP_TIMEOUT, async {
                loop {
                    tokio::select! {
                        _ = tx.closed(), if !cancel.requested() => cancel.request(),
                        outcome = &mut looped => return outcome,
                    }
                }
            })
            .await
        };
        steer.close();
        let answer = loop_answer(driven, &chat, &mut run, &system, &prompt, LOOP_TIMEOUT, started);
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        state.unseat(&seat);
    });
    sse_of(rx)
}

/// GET /api/llm/db/conversation?agent&conn: the connection's open conversation with
/// that agent, {runId} (null when none) - what the Data page restores on open.
pub async fn open_conversation(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(agent), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    let open = within(scope(conn), history.continuable_open(agent)).await;
    ok(json!({ "ok": true, "runId": open }))
}

/// GET /api/llm/db/conversations?agent&conn&page: the connection's conversations with that
/// agent, newest first, 20 to a page - the pane's History, open, closed and failed alike,
/// in the history's run shape ({runs, page, pageSize, more}); a compaction's run names the
/// one it condensed in its source (`compact:<id>`).
pub async fn conversations(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(agent), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let page = match params.get("page").map(|page| page.parse::<i64>()) {
        None => 0,
        Some(Ok(page)) if page >= 0 => page,
        Some(_) => return bad("page must be a whole number"),
    };
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match within(scope(conn), history.read_conversations(agent, page, HISTORY_PAGE)).await {
        Ok(mut found) => {
            found["ok"] = json!(true);
            ok(found)
        }
        Err(why) => admin_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

/// GET /api/llm/db/context?agent&conn&runId: how full the conversation's next request starts -
/// the system prompt it opened under, the connection kind's tool definitions and the dialogue
/// it replays, in bytes against the agent's budget (maxContextKiB): {bytes, cap, system,
/// tools}. The pane's context meter reads it for a conversation it restores, reopens or
/// compacts; a send's usage frames say it round by round. Without runId it weighs the base
/// prompt and tools, keeping the fresh conversation's meter visible without any DB/model I/O.
pub async fn context(
    State(state): State<Arc<LlmState>>,
    Extension(services): Extension<Arc<RuntimeServices>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(name), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let run_id = match params.get("runId") {
        Some(id) => match id.parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => return bad("runId must be an integer: the conversation to weigh"),
        },
        None => None,
    };
    let Desk { base, history, chat, kind, .. } = match desk(&state, &services, name, conn, &Pick::default()) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    let (system, prior) = if let Some(run_id) = run_id {
        within(scope(conn), async {
            let system = history.system_note_of(run_id).await.unwrap_or(base);
            let prior = match history.replay_state(run_id).await {
                ReplayState::Continuable { prior } => prior,
                _ => Vec::new(),
            };
            (system, prior)
        })
        .await
    } else {
        // Before the first send only the base prompt and tool definitions exist. Do not query
        // the database to make the connection snapshot just to paint an idle context meter.
        (base, Vec::new())
    };
    // The next send's own definitions, intention parameter and all: weighing the bare specs made
    // the estimate read under what the stream's first round reported (agent::intentioned).
    let specs = swiss_llm::agent::intentioned(DbTools::new(services.catalog.clone(), conn, kind, None).specs());
    let tools = swiss_llm::agent::tools_bytes(&specs);
    let dialogue = serde_json::to_vec(&prior).map(|bytes| bytes.len()).unwrap_or(0);
    ok(json!({
        "ok": true, "bytes": system.len() + tools + dialogue, "cap": chat.context_cap(),
        "system": system.len(), "tools": tools,
    }))
}

/// POST /api/llm/db/reopen {agent, conn, runId, provider?, model?, effort?}: carry an earlier
/// conversation of this connection on (the pane's History, Continue here). The open one, if
/// another, closes - it stays in History - and runId reopens when it is a closed conversation
/// of this agent here that passes the continuation guards, moved to the model the pane's
/// menu names when it names one; {ok, runId}. 409 busy while a send or compaction
/// works the connection, gone for a run that cannot go on (failed, a round left
/// unresolved, another connection's), context-changed when the agent's prompt changed
/// since it opened - both runs then stay as they were.
pub async fn reopen(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "runId", "provider", "model", "effort"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn) = match agent_and_conn(&body) {
        Ok(both) => both,
        Err(refused) => return refused,
    };
    let run_id = match run_id_of(&body) {
        Ok(Some(id)) => id,
        Ok(None) => return bad("runId is required: the conversation to carry on"),
        Err(why) => return bad(why),
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let Desk { agent, base, history, chat, seat, .. } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    // The seat for the swap's length: no send opens or continues the connection's
    // conversation half-way through it.
    if !state.seat(&seat, run_id, CancelFlag::new(), shut_steer()) {
        return refuse("busy", BUSY);
    }
    let reopened = within(scope(&conn), conversation::reopen(&history, run_id, &name, agent.runtime.as_str(), &chat, &base, pick.moves())).await;
    state.unseat(&seat);
    match reopened {
        Ok(()) => ok(json!({ "ok": true, "runId": run_id })),
        Err(why) => lane_refusal(why),
    }
}

/// POST /api/llm/db/compact {agent, conn, runId, focus?, provider?, model?, effort?}: condense
/// the connection's open
/// conversation into a checkpoint and carry the dialogue on from it (SPEC §data.agent
/// Compact), as server-sent events: run ({runId: the new conversation, from: the one
/// condensed, compacting: true}), the checkpoint's thinking and delta frames, final, done
/// ({ok, compacted, runId: the conversation open after it, from}). The model reads the
/// whole dialogue and DB_COMPACT_INSTRUCTION after it (focus, the reader's words after
/// /compact, closing it), and the checkpoint opens a new conversation in the same scope:
/// its next send replays COMPACT_FRAMING and the checkpoint, never the condensed turns,
/// which History keeps whole under the closed run. Any failure - the model's error, a
/// cancel, a checkpoint cut at maxTokens or never written, the time bound - files the new
/// run failed and reopens the old one as it was (done: compacted false, runId the old one,
/// or null when it could not reopen).
pub async fn compact(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "runId", "focus", "provider", "model", "effort"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn) = match agent_and_conn(&body) {
        Ok(both) => both,
        Err(refused) => return refused,
    };
    let joining = match run_id_of(&body) {
        Ok(Some(id)) => id,
        Ok(None) => return bad("runId is required: the conversation to compact"),
        Err(why) => return bad(why),
    };
    let focus = match field(&body, "focus") {
        Ok(focus) if focus.as_ref().is_none_or(|focus| focus.chars().count() <= FOCUS_MAX) => focus,
        Ok(_) => return bad(format!("focus: at most {FOCUS_MAX} characters")),
        Err(why) => return bad(why),
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let Desk { agent, base, history, chat, seat, kind } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    if state.seat_run_of(&seat).is_some() {
        return refuse("busy", BUSY);
    }
    if pick.moves() {
        if let Err(why) = within(scope(&conn), conversation::retarget(&history, joining, &name, &chat)).await {
            return lane_refusal(why);
        }
    }
    let fold = match within(scope(&conn), conversation::fold(&history, joining, &name, agent.runtime.as_str(), &chat, &base)).await {
        Ok(fold) => fold,
        Err(why) => return lane_refusal(why),
    };
    let conversation::Fold { lane, prior, system, from } = fold;
    let conversation::Lane { mut run, run_id, .. } = lane;
    let cancel = CancelFlag::new();
    if !state.seat(&seat, run_id, cancel.clone(), shut_steer()) {
        run.release();
        let _ = within(scope(&conn), conversation::undo_fold(&history, run_id, from, &name)).await;
        return refuse("busy", BUSY);
    }
    run.set_cancel(cancel.clone());
    let session = state.session(&agent, TASK);
    let instruction = match &focus {
        Some(focus) => format!("{DB_COMPACT_INSTRUCTION}\n\nThe reader asks the checkpoint to keep in particular: {focus}"),
        None => DB_COMPACT_INSTRUCTION.to_string(),
    };
    // The conversation's own tools, declared and never served (see run_compaction).
    let tools = DbTools::new(services.catalog.clone(), &conn, kind, None).specs();
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    sse_send(&tx, "run", json!({
        "runId": run_id, "from": from, "agent": name, "conn": conn, "provider": chat.provider, "model": chat.model,
        "compacting": true,
    }));
    let started = Instant::now();
    tokio::spawn(async move {
        let mut on_event = awaited_sink(&tx, &cancel);
        let driven = {
            let looped = swiss_llm::agent::run_compaction(
                &chat, &system, &prior, &instruction, COMPACT_FRAMING, tools, &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            tokio::time::timeout(LOOP_TIMEOUT, async {
                loop {
                    tokio::select! {
                        _ = tx.closed(), if !cancel.requested() => cancel.request(),
                        outcome = &mut looped => return outcome,
                    }
                }
            })
            .await
        };
        // A round that answered but left no checkpoint to carry on from fails too.
        let spoiled = match &driven {
            Ok(Ok(outcome)) if outcome.cancelled => None,
            Ok(Ok(outcome)) if outcome.cut => Some("the checkpoint stopped at the model's output limit (maxTokens): nothing was compacted"),
            Ok(Ok(outcome)) if outcome.capped || outcome.summary.trim().is_empty() => Some("the model wrote no checkpoint: nothing was compacted"),
            _ => None,
        };
        let landed = matches!(&driven, Ok(Ok(outcome)) if !outcome.cancelled) && spoiled.is_none();
        let mut answer = loop_answer(driven, &chat, &mut run, &system, &instruction, LOOP_TIMEOUT, started);
        if let Some(why) = spoiled {
            run.fail(why);
            answer = json!({ "ok": false, "error": why, "ms": answer["ms"] });
        }
        answer["from"] = json!(from);
        answer["compacted"] = json!(landed);
        if !landed {
            let back = within(scope(&conn), conversation::undo_fold(&history, run_id, from, &name)).await;
            answer["runId"] = if matches!(back, Ok(true)) { json!(from) } else { Value::Null };
        }
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        state.unseat(&seat);
    });
    sse_of(rx)
}
