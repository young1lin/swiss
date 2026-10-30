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

//! The /api/knowledge routes (SPEC §memory.api): the panel's typed surface behind the
//! admin session, over the same service the CLI and /mcp/memory drive.
//!
//! State: one slot the plugin lifecycle owns - the terminal/remote precedent. The
//! routes are mounted once at boot (inside the loopback guard and the plugin
//! boundary); the service they drive exists only while the memory plugin is serving.
//! The boundary answers the structured 503 for a disabled plugin; the handlers check
//! the slot anyway, because a mounted router must never assume who mounted it.
//!
//! Every record, domain, search, graph, brief and link route takes the `project` selector
//! (a query parameter on GET/DELETE, a body field otherwise) and the service rechecks
//! every id against it. Errors are `{error: <code>, message, …extra}` with the code's
//! HTTP status.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Map, Value};
use swiss_host::reply::admin_error;
use swiss_memory::model::{is_project_id, Actor, Kind, LinkKind, Status, Verdict};
use swiss_memory::service::{
    BriefRequest, GraphRequest, LinkSpec, ListRequest, RelatedRequest, SaveRequest, Target, UpdateRequest,
};
use swiss_memory::docs::{self, DocKind};
use swiss_memory::embed::Purpose;
use swiss_memory::survey::{Draw, Survey};
use swiss_memory::{
    Checkout, Code, DomainRequest, DomainUpdate, EvidenceSpec, ExportScope, GitIssue, ImportOptions, ImportStrategy, MemError,
    MemResult, MemoryService, MissRequest, SearchQuery, Seed,
};

use swiss_llm::embedding::EmbeddingConfig;
use swiss_llm::LlmState;

use crate::memory_preview as preview;
use crate::memory_sync::{self, SyncConfig};
use swiss_memory::settings::Choice;

/// Where the builtin /mcp/memory endpoint stands (SPEC §memory.mcp collision rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSeat {
    /// Mounted and answering.
    Serving,
    /// A user MCP owns the name; ours stays unmounted until it is renamed.
    Conflicted,
    Stopped,
}

impl McpSeat {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpSeat::Serving => "serving",
            McpSeat::Conflicted => "conflicted",
            McpSeat::Stopped => "stopped",
        }
    }
}

/// The llm plugin's embedding block memory has plugged in, and why it is not serving
/// when it is not (a client that could not be built, a width the store cannot hold).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbeddingSeat {
    pub config: Option<EmbeddingConfig>,
    pub error: Option<String>,
}

/// The `sync` block in force, and why the worker would refuse a run when it would.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncSeat {
    pub config: Option<SyncConfig>,
    pub error: Option<String>,
}

/// The slot: Some while the memory plugin is serving.
#[derive(Clone)]
pub struct MemoryState {
    slot: Arc<RwLock<Option<Arc<MemoryService>>>>,
    seat: Arc<RwLock<McpSeat>>,
    embedding: Arc<RwLock<EmbeddingSeat>>,
    /// The llm plugin's applied row: the embedding endpoint and the agent a sync run asks.
    llm: Arc<LlmState>,
    sync: Arc<RwLock<SyncSeat>>,
    /// The queue worker sleeps on this; an enqueue or a config edit wakes it.
    sync_wake: Arc<tokio::sync::Notify>,
    /// The running runs' stops by run id (SPEC §memory.sync Cancel): the worker seats one
    /// per run, the cancel route raises it.
    sync_stops: Arc<std::sync::Mutex<HashMap<String, memory_sync::Stop>>>,
}

impl Default for MemoryState {
    fn default() -> Self {
        MemoryState {
            slot: Arc::default(),
            seat: Arc::new(RwLock::new(McpSeat::Stopped)),
            embedding: Arc::default(),
            llm: LlmState::new(),
            sync: Arc::default(),
            sync_wake: Arc::new(tokio::sync::Notify::new()),
            sync_stops: Arc::default(),
        }
    }
}

impl MemoryState {
    /// A state over its own llm row, off until something applies one (the tests).
    pub fn new() -> MemoryState {
        MemoryState::default()
    }

    /// The composition's state: the llm plugin's row, shared.
    pub fn with_llm(llm: Arc<LlmState>) -> MemoryState {
        MemoryState { llm, ..MemoryState::default() }
    }

    pub fn llm(&self) -> &Arc<LlmState> {
        &self.llm
    }

    /// Install the serving service (plugin start).
    pub fn install(&self, service: Arc<MemoryService>) {
        if let Ok(mut slot) = self.slot.write() {
            *slot = Some(service);
        }
    }

    /// Withdraw the serving service (plugin stop).
    pub fn withdraw(&self) -> Option<Arc<MemoryService>> {
        let service = self.slot.write().unwrap_or_else(|err| err.into_inner()).take();
        self.set_seat(McpSeat::Stopped);
        service
    }

    /// The serving service, if the plugin is up.
    pub fn current(&self) -> Option<Arc<MemoryService>> {
        self.slot.read().ok().and_then(|slot| slot.clone())
    }

    pub fn set_seat(&self, seat: McpSeat) {
        if let Ok(mut current) = self.seat.write() {
            *current = seat;
        }
    }

    pub fn seat(&self) -> McpSeat {
        self.seat.read().map(|s| *s).unwrap_or(McpSeat::Stopped)
    }

    /// Record the embedding block the plugin plugged in (and its error, if any).
    pub fn set_embedding(&self, config: Option<EmbeddingConfig>, error: Option<String>) {
        if let Ok(mut current) = self.embedding.write() {
            *current = EmbeddingSeat { config, error };
        }
    }

    pub fn embedding(&self) -> EmbeddingSeat {
        self.embedding.read().map(|e| e.clone()).unwrap_or_default()
    }

    /// Record the `sync` block the plugin applied (and its error, if any).
    pub fn set_sync(&self, config: Option<SyncConfig>, error: Option<String>) {
        if let Ok(mut current) = self.sync.write() {
            *current = SyncSeat { config, error };
        }
    }

    pub fn sync_seat(&self) -> SyncSeat {
        self.sync.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// The `sync` block the worker runs by (None: the row has none).
    pub fn sync_config(&self) -> Option<SyncConfig> {
        self.sync_seat().config
    }

    /// Wake the queue worker (an enqueue or a config edit).
    pub fn wake_sync(&self) {
        self.sync_wake.notify_one();
    }

    /// The worker's sleeper: a clone of the notify the enqueues wake.
    pub fn sync_wake(&self) -> Arc<tokio::sync::Notify> {
        self.sync_wake.clone()
    }

    /// Run `id`'s stop, seated on first ask: the worker's run and a cancel that lands
    /// before the worker seats it share one.
    pub fn sync_stop(&self, id: &str) -> memory_sync::Stop {
        let mut stops = self.sync_stops.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        stops.entry(id.to_string()).or_default().clone()
    }

    /// Run `id` landed: its stop leaves the seat.
    pub fn sync_stop_done(&self, id: &str) {
        self.sync_stops.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(id);
    }

    /// Whether run `id` was told to stop and has not landed yet.
    pub fn sync_stopping(&self, id: &str) -> bool {
        let stops = self.sync_stops.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        stops.get(id).is_some_and(|stop| stop.raised())
    }
}

/// Mount the routes; the plugin boundary in server.rs folds them under /api/knowledge.
pub fn mount(state: Arc<MemoryState>) -> Router {
    Router::new()
        .route("/api/knowledge/status", get(status))
        .route("/api/knowledge/projects", get(projects).post(create_project))
        .route("/api/knowledge/projects/{id}", axum::routing::patch(rename_project).delete(delete_project))
        .route("/api/knowledge/projects/{id}/roots", post(bind_root).put(rebind_root).delete(unbind_root))
        .route("/api/knowledge/projects/{id}/settings", get(project_settings).patch(patch_project_settings))
        .route("/api/knowledge/brief", post(brief))
        .route("/api/knowledge/records", get(records).post(save))
        .route("/api/knowledge/records/file", post(file))
        .route("/api/knowledge/records/{id}", get(record).put(update).delete(purge))
        .route("/api/knowledge/records/{id}/mark", post(mark))
        .route("/api/knowledge/records/{id}/confirm", post(confirm))
        .route("/api/knowledge/records/{id}/restore", post(restore))
        .route("/api/knowledge/domains", get(domains).post(create_domain))
        .route("/api/knowledge/domains/autofile", post(autofile))
        .route("/api/knowledge/domains/coverage", get(coverage))
        .route("/api/knowledge/domains/{id}", get(domain).put(update_domain).delete(delete_domain))
        .route("/api/knowledge/domains/{id}/confirm", post(confirm_domain))
        .route("/api/knowledge/domains/{id}/verify", post(verify_domain))
        .route("/api/knowledge/links", post(link).delete(unlink))
        .route("/api/knowledge/search", get(search))
        .route("/api/knowledge/related", post(related))
        .route("/api/knowledge/graph", get(graph))
        .route("/api/knowledge/review", get(review))
        .route("/api/knowledge/misses", get(misses).post(record_miss))
        .route("/api/knowledge/misses/export", get(export_misses))
        .route("/api/knowledge/misses/{id}", axum::routing::delete(drop_miss))
        .route("/api/knowledge/export", get(export))
        .route("/api/knowledge/import", post(import))
        .route("/api/knowledge/backup", post(backup))
        .route("/api/knowledge/embedding", get(embedding))
        .route("/api/knowledge/embedding/test", post(embedding_test))
        .route("/api/knowledge/embedding/reindex", post(embedding_reindex))
        .route("/api/knowledge/code", get(code_status).delete(code_drop))
        .route("/api/knowledge/code/index", post(code_index))
        .route("/api/knowledge/code/find", get(code_find))
        .route("/api/knowledge/code/file", get(code_file))
        .route("/api/knowledge/docs", get(docs_index))
        .route("/api/knowledge/docs/read", get(docs_read))
        .route("/api/knowledge/preview/file", get(preview_file))
        .route("/api/knowledge/preview/files", get(preview_files))
        .route("/api/knowledge/preview/log", get(preview_log))
        .route("/api/knowledge/preview/commit", get(preview_commit))
        .route("/api/knowledge/preview/diff", get(preview_diff))
        .route("/api/knowledge/sync", get(sync_overview).post(sync_start))
        .route("/api/knowledge/sync/nudge", post(sync_nudge))
        .route("/api/knowledge/sync/map", get(sync_map_plan))
        .route("/api/knowledge/sync/full", get(sync_full_plan))
        .route("/api/knowledge/sync/runs/{id}", get(sync_run))
        .route("/api/knowledge/sync/runs/{id}/cancel", post(sync_cancel))
        .route("/api/knowledge/ask", post(knowledge_ask))
        .with_state(state)
}

// --- plumbing ---------------------------------------------------------------------------

type Params = HashMap<String, String>;

/// A missing service is the plugin boundary's 503 territory; answering it here again
/// is belt-and-braces for direct mounts (tests).
fn unavailable() -> Response {
    admin_error(StatusCode::SERVICE_UNAVAILABLE, "the memory plugin is not serving")
}

/// `{error: <code>, message, …extra}` with the code's status.
fn fail(err: &MemError) -> Response {
    let status = StatusCode::from_u16(err.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut body = Map::new();
    body.insert("error".into(), json!(err.code.as_str()));
    body.insert("message".into(), json!(err.message));
    for (key, value) in &err.extra {
        body.insert(key.clone(), value.clone());
    }
    (status, Json(Value::Object(body))).into_response()
}

fn ok(body: impl Serialize) -> MemResult<Response> {
    Ok((StatusCode::OK, Json(body)).into_response())
}

fn reply(outcome: MemResult<Response>) -> Response {
    outcome.unwrap_or_else(|err| fail(&err))
}

/// A JSON object body; an empty body is an empty object.
fn body_of(bytes: &[u8]) -> MemResult<Map<String, Value>> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(MemError::invalid("the body must be a JSON object")),
        Err(err) => Err(MemError::invalid(format!("the body is not JSON: {err}"))),
    }
}

/// The `project` selector as a target.
fn selector(raw: Option<&str>) -> MemResult<Target> {
    let id = raw.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| MemError::invalid("project is required"))?;
    if !is_project_id(id) {
        return Err(MemError::invalid("project must be a project id (p-…) or \"global\""));
    }
    Ok(Target::project(id))
}

fn param<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params.get(key).map(String::as_str).map(str::trim).filter(|v| !v.is_empty())
}

fn param_flag(params: &Params, key: &str) -> bool {
    matches!(param(params, key), Some("true" | "1"))
}

fn param_number(params: &Params, key: &str) -> MemResult<Option<u64>> {
    param(params, key)
        .map(|v| v.parse::<u64>().map_err(|_| MemError::invalid(format!("{key} must be a non-negative integer"))))
        .transpose()
}

/// Comma-separated closed-vocabulary values.
fn param_list<T>(params: &Params, key: &str, parse: fn(&str) -> Option<T>, choices: fn() -> String) -> MemResult<Vec<T>> {
    let Some(raw) = param(params, key) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| parse(v).ok_or_else(|| MemError::invalid(format!("{key}: {v:?} is not one of {}", choices()))))
        .collect()
}

/// Strict field access over a JSON object, shared with the MCP adapter: every
/// error names the field, and unknown fields are refused.
pub(crate) struct Fields<'a>(pub &'a Map<String, Value>);

impl Fields<'_> {
    pub fn only(&self, allowed: &[&str]) -> MemResult<()> {
        match self.0.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(key) => Err(MemError::invalid(format!("unknown field {key:?}"))),
            None => Ok(()),
        }
    }

    pub fn text(&self, key: &str) -> MemResult<Option<String>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(MemError::invalid(format!("{key} must be a string"))),
        }
    }

    pub fn required(&self, key: &str) -> MemResult<String> {
        self.text(key)?.ok_or_else(|| MemError::invalid(format!("{key} is required")))
    }

    pub fn flag(&self, key: &str) -> MemResult<Option<bool>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(MemError::invalid(format!("{key} must be a boolean"))),
        }
    }

    pub fn number(&self, key: &str) -> MemResult<Option<u64>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| MemError::invalid(format!("{key} must be a non-negative integer"))),
        }
    }

    pub fn texts(&self, key: &str) -> MemResult<Option<Vec<String>>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or_else(|| MemError::invalid(format!("{key} must be strings"))))
                .collect::<MemResult<Vec<_>>>()
                .map(Some),
            Some(_) => Err(MemError::invalid(format!("{key} must be an array of strings"))),
        }
    }

    pub fn kind(&self, key: &str) -> MemResult<Option<Kind>> {
        self.text(key)?
            .map(|v| Kind::parse(&v).ok_or_else(|| MemError::invalid(format!("{key} must be one of {}", Kind::choices()))))
            .transpose()
    }

    pub fn link_kind(&self, key: &str) -> MemResult<LinkKind> {
        let raw = self.required(key)?;
        LinkKind::parse(&raw).ok_or_else(|| MemError::invalid(format!("{key} must be one of {}", LinkKind::choices())))
    }

    pub fn verdict(&self, key: &str) -> MemResult<Verdict> {
        let raw = self.required(key)?;
        Verdict::parse(&raw).ok_or_else(|| MemError::invalid(format!("{key} must be one of {}", Verdict::choices())))
    }

    /// `[{to, kind, note?}]`.
    pub fn links(&self, key: &str) -> MemResult<Vec<LinkSpec>> {
        let items = match self.0.get(key) {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Array(items)) => items,
            Some(_) => return Err(MemError::invalid(format!("{key} must be an array"))),
        };
        items
            .iter()
            .map(|item| {
                let map = item.as_object().ok_or_else(|| MemError::invalid(format!("{key}: each link is an object")))?;
                let fields = Fields(map);
                fields.only(&["to", "kind", "note"])?;
                Ok(LinkSpec { to: fields.required("to")?, kind: fields.link_kind("kind")?, note: fields.text("note")? })
            })
            .collect()
    }

    /// `[{sourceType, relation?, path?, symbol?, commitSha?, sessionId?, runId?, excerpt?}]`
    /// (ADR-032); absent is none.
    pub fn evidence(&self, key: &str) -> MemResult<Vec<EvidenceSpec>> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(value) => EvidenceSpec::list_from_json(value),
        }
    }

    /// A save request from `kind, title, content, anchors?, refs?, keywords?, links?, replaces?, allowSimilar?,
    /// domain?, evidence?`.
    pub fn save_request(&self) -> MemResult<SaveRequest> {
        Ok(SaveRequest {
            kind: self.kind("kind")?.ok_or_else(|| MemError::invalid(format!("kind is required: {}", Kind::choices())))?,
            title: self.required("title")?,
            content: self.required("content")?,
            anchors: self.texts("anchors")?.unwrap_or_default(),
            refs: self.texts("refs")?.unwrap_or_default(),
            keywords: self.texts("keywords")?.unwrap_or_default(),
            links: self.links("links")?,
            replaces: self.text("replaces")?,
            allow_similar: self.flag("allowSimilar")?.unwrap_or(false),
            domain: self.text("domain")?,
            evidence: self.evidence("evidence")?,
        })
    }

    /// An update from `expectedRevision, kind?, title?, content?, anchors?, refs?, keywords?,
    /// reason?, domain?, evidence?`. Filing or evidence alone (`domain`, `""` to unfile) leaves the
    /// text as it is: it needs no revision.
    pub fn update_request(&self) -> MemResult<UpdateRequest> {
        let given = |key: &str| !matches!(self.0.get(key), None | Some(Value::Null));
        let filing_only =
            (given("domain") || given("evidence")) && !["kind", "title", "content", "anchors", "refs", "keywords"].iter().any(|k| given(k));
        let expected_revision = match self.number("expectedRevision")? {
            Some(revision) => revision,
            None if filing_only => 0,
            None => return Err(MemError::invalid("expectedRevision is required (the revision you read)")),
        };
        Ok(UpdateRequest {
            expected_revision,
            kind: self.kind("kind")?,
            title: self.text("title")?,
            content: self.text("content")?,
            anchors: self.texts("anchors")?,
            refs: self.texts("refs")?,
            keywords: self.texts("keywords")?,
            reason: self.text("reason")?,
            domain: self.text("domain")?,
            evidence: self.evidence("evidence")?,
        })
    }

    /// A recorded miss from `query, via?, style?, shown?, wrong?, wanted?, want?, paths?`.
    pub fn miss_request(&self) -> MemResult<MissRequest> {
        Ok(MissRequest {
            query: self.required("query")?,
            via: self.text("via")?,
            style: self.text("style")?,
            shown: self.texts("shown")?.unwrap_or_default(),
            wrong: self.texts("wrong")?.unwrap_or_default(),
            wanted: self.texts("wanted")?.unwrap_or_default(),
            want: self.text("want")?,
            paths: self.texts("paths")?.unwrap_or_default(),
        })
    }

    /// A domain from `path, title, summary?, body?, owns?, keywords?`.
    pub fn domain_request(&self) -> MemResult<DomainRequest> {
        Ok(DomainRequest {
            path: self.required("path")?,
            title: self.required("title")?,
            summary: self.text("summary")?,
            body: self.text("body")?,
            owns: self.texts("owns")?.unwrap_or_default(),
            keywords: self.texts("keywords")?.unwrap_or_default(),
        })
    }

    /// A domain edit from `expectedRevision, title?, summary?, body?, owns?, keywords?, reason?`, the
    /// new path read from `path_key` (the API's `path`, the MCP tool's `newPath`).
    pub fn domain_update(&self, path_key: &str) -> MemResult<DomainUpdate> {
        Ok(DomainUpdate {
            expected_revision: self
                .number("expectedRevision")?
                .ok_or_else(|| MemError::invalid("expectedRevision is required (the revision you read)"))?,
            path: self.text(path_key)?,
            title: self.text("title")?,
            summary: self.text("summary")?,
            body: self.text("body")?,
            owns: self.texts("owns")?,
            keywords: self.texts("keywords")?,
            reason: self.text("reason")?,
        })
    }
}

/// Run a handler body against the serving service.
macro_rules! serve {
    ($state:expr, |$service:ident| $body:expr) => {{
        let Some($service) = $state.current() else {
            return unavailable();
        };
        reply(async { $body }.await)
    }};
}

// --- status and projects ----------------------------------------------------------------

async fn status(State(state): State<Arc<MemoryState>>) -> Response {
    serve!(state, |service| {
        let status = service.status().await?;
        ok(json!({
            "dbPath": status.db_path,
            "schemaVersion": status.schema_version,
            "mcp": { "state": state.seat().as_str(), "path": "/mcp/memory" },
        }))
    })
}

async fn projects(State(state): State<Arc<MemoryState>>) -> Response {
    serve!(state, |service| ok(json!({ "projects": service.projects().await? })))
}

async fn create_project(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["name", "path", "exact"])?;
        let name = f.text("name")?;
        let path = f.text("path")?.filter(|p| !p.trim().is_empty());
        let project = service
            .create_project(name.as_deref(), path.as_deref(), f.flag("exact")?.unwrap_or(false), None)
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "project": project }))).into_response())
    })
}

async fn rename_project(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["name"])?;
        ok(json!({ "project": service.rename_project(&id, &f.required("name")?).await? }))
    })
}

/// GET /api/knowledge/projects/{id}/settings: the project's own settings, null where a
/// field inherits (SPEC §memory.settings).
async fn project_settings(State(state): State<Arc<MemoryState>>, Path(id): Path<String>) -> Response {
    serve!(state, |service| {
        ok(json!({ "settings": service.project_settings(&id).await?.to_json() }))
    })
}

/// PATCH /api/knowledge/projects/{id}/settings: a merge patch - a field the body carries is
/// set (null inherits again), one it leaves out is kept - answered as saved.
async fn patch_project_settings(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let saved = service.patch_project_settings(&id, &Value::Object(body)).await?;
        // A queued run reads the settings when it starts: the worker looks again now.
        state.wake_sync();
        ok(json!({ "settings": saved.to_json() }))
    })
}

async fn delete_project(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let (records, links) = service.delete_project(&id, param(&params, "confirm").unwrap_or_default()).await?;
        ok(json!({ "deleted": { "records": records, "links": links } }))
    })
}

async fn bind_root(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["path", "exact"])?;
        let project = service.bind_root(&id, &f.required("path")?, f.flag("exact")?.unwrap_or(false), None).await?;
        ok(json!({ "project": project }))
    })
}

/// Change a directory in one step: the notes follow the project's id, never a path.
async fn rebind_root(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["identity", "path", "exact"])?;
        let (identity, path) = (f.required("identity")?, f.required("path")?);
        let project = service.rebind_root(&id, &identity, &path, f.flag("exact")?.unwrap_or(false), None).await?;
        ok(json!({ "project": project }))
    })
}

async fn unbind_root(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let identity = param(&params, "identity").ok_or_else(|| MemError::invalid("identity is required"))?;
        ok(json!({ "project": service.unbind_root(&id, identity).await? }))
    })
}

// --- brief ------------------------------------------------------------------------------

async fn brief(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "paths", "task", "maxChars", "includeGlobal"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let request = BriefRequest {
            paths: f.texts("paths")?.unwrap_or_default(),
            task: f.text("task")?,
            max_chars: f.number("maxChars")?.map(|n| n as usize),
            exclude_global: !f.flag("includeGlobal")?.unwrap_or(true),
        };
        ok(service.brief(&target, &request).await?)
    })
}

// --- records ----------------------------------------------------------------------------

async fn records(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let request = ListRequest {
            kinds: param_list(&params, "kind", Kind::parse, Kind::choices)?,
            statuses: param_list(&params, "status", Status::parse, Status::choices)?,
            unconfirmed: param_flag(&params, "unconfirmed"),
            history: param_flag(&params, "history"),
            limit: param_number(&params, "limit")?.map(|n| n as usize),
            domain: param(&params, "domain").map(str::to_string),
        };
        let (items, truncated) = service.list(&target, &request).await?;
        ok(json!({ "items": items, "truncated": truncated }))
    })
}

async fn save(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "kind", "title", "content", "anchors", "refs", "keywords", "links", "replaces", "allowSimilar", "domain", "evidence"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let outcome = service.save(&target, &Actor::panel(), f.save_request()?).await?;
        let status = if outcome.duplicate { StatusCode::OK } else { StatusCode::CREATED };
        Ok((status, Json(outcome)).into_response())
    })
}

async fn record(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let (record, revisions) = service.get(&target, &id, param_flag(&params, "history")).await?;
        let mut body = json!({ "record": record });
        if let Some(revisions) = revisions {
            body["revisions"] = json!(revisions);
        }
        ok(body)
    })
}

async fn update(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "expectedRevision", "kind", "title", "content", "anchors", "refs", "keywords", "reason", "domain", "evidence"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let outcome = service.update_checked(&target, &Actor::panel(), &id, f.update_request()?).await?;
        let mut body = json!({ "record": outcome.record });
        if let Some(code) = outcome.code.filter(|c| !c.findings.is_empty()) {
            body["code"] = json!(code);
        }
        ok(body)
    })
}

async fn purge(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        service.purge(&target, &id, param(&params, "confirm").unwrap_or_default()).await?;
        ok(json!({ "deleted": true }))
    })
}

async fn mark(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "verdict", "reason", "evidence"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let reason = f.text("reason")?;
        let evidence = f.evidence("evidence")?;
        let record =
            service.mark_with(&target, &Actor::panel(), &id, f.verdict("verdict")?, reason.as_deref(), evidence).await?;
        ok(json!({ "record": record }))
    })
}

async fn confirm(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(json!({ "record": service.confirm(&target, &Actor::panel(), &id).await? }))
    })
}

async fn restore(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "reason"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let reason = f.text("reason")?.unwrap_or_default();
        ok(json!({ "record": service.restore(&target, &Actor::panel(), &id, &reason).await? }))
    })
}

// --- domains ----------------------------------------------------------------------------

/// The map: every domain with its counts, and the unfiled count.
async fn domains(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| ok(service.domains(&selector(param(&params, "project"))?).await?))
}

async fn create_domain(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "path", "title", "summary", "body", "owns", "keywords"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let domain = service.create_domain(&target, &Actor::panel(), f.domain_request()?).await?;
        Ok((StatusCode::CREATED, Json(json!({ "domain": domain }))).into_response())
    })
}

/// One domain: its hub, ancestors, sub-domains and notes; its revisions with `history`.
async fn domain(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(json!({ "domain": service.domain(&target, &id, param_flag(&params, "history")).await? }))
    })
}

/// An edit (CAS on `expectedRevision`); a changed `path` moves the subtree.
async fn update_domain(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "expectedRevision", "path", "title", "summary", "body", "owns", "keywords", "reason"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(json!({ "domain": service.update_domain(&target, &Actor::panel(), &id, f.domain_update("path")?).await? }))
    })
}

/// Delete by typing the path (`confirm`): the notes go to the parent.
async fn delete_domain(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(service.delete_domain(&target, &id, param(&params, "confirm").unwrap_or_default()).await?)
    })
}

/// `all`: the domain whole - the domains under it and their unconfirmed active notes too.
async fn confirm_domain(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "all"])?;
        let target = selector(f.text("project")?.as_deref())?;
        if f.flag("all")?.unwrap_or(false) {
            return ok(json!(service.confirm_tree(&target, &Actor::panel(), &id).await?));
        }
        ok(json!({ "domain": service.confirm_domain(&target, &Actor::panel(), &id).await? }))
    })
}

/// The hub is true to the code again: `verifiedMs` is now.
async fn verify_domain(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(json!({ "domain": service.verify_domain(&target, &Actor::panel(), &id).await? }))
    })
}

/// File every unfiled live note by its anchors.
async fn autofile(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(json!({ "filed": service.autofile(&target).await? }))
    })
}

/// Coverage over the project's checkout; an unreachable one keeps the counts.
async fn coverage(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let read = match checkout(&service, &params).await {
            Ok(root) => preview::coverage_read(&root).await,
            Err(err) if err.code == Code::NotFound => {
                Checkout { issue: Some(GitIssue::NoDirectory), reason: Some(err.message), ..Checkout::default() }
            }
            Err(err) => return Err(err),
        };
        ok(service.coverage(&target, read).await?)
    })
}

/// File notes by hand: `{ids, domain}` (null or "none" unfiles them).
async fn file(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "ids", "domain"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let ids = f.texts("ids")?.ok_or_else(|| MemError::invalid("ids is required"))?;
        let domain = f.text("domain")?;
        ok(service.file(&target, &ids, domain.as_deref()).await?)
    })
}

// --- links ------------------------------------------------------------------------------

async fn link(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "from", "to", "kind", "note"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let note = f.text("note")?;
        let (edge, changed) = service
            .link(&target, &Actor::panel(), &f.required("from")?, &f.required("to")?, f.link_kind("kind")?, note.as_deref())
            .await?;
        ok(json!({ "link": edge, "changed": changed }))
    })
}

async fn unlink(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let from = param(&params, "from").ok_or_else(|| MemError::invalid("from is required"))?;
        let to = param(&params, "to").ok_or_else(|| MemError::invalid("to is required"))?;
        let kind = param(&params, "kind")
            .and_then(LinkKind::parse)
            .ok_or_else(|| MemError::invalid(format!("kind must be one of {}", LinkKind::choices())))?;
        ok(json!({ "changed": service.unlink(&target, from, to, kind).await? }))
    })
}

// --- reads ------------------------------------------------------------------------------

async fn search(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let query = SearchQuery {
            text: param(&params, "q").unwrap_or_default().to_string(),
            kinds: param_list(&params, "kind", Kind::parse, Kind::choices)?,
            include_history: param_flag(&params, "history"),
            limit: param_number(&params, "limit")?.map(|n| n as usize).unwrap_or(swiss_memory::search::DEFAULT_LIMIT),
            domain: param(&params, "domain").map(str::to_string),
            // `semantic=off`: the word tiers alone (an evaluation's A/B); `on`, the default.
            words_only: match param(&params, "semantic") {
                None | Some("on") => false,
                Some("off") => true,
                Some(_) => return Err(MemError::invalid("semantic must be on or off")),
            },
        };
        // `{items, semantic}`: the panel says when the semantic tier did not run.
        ok(service.hybrid_search(&target, &query).await?)
    })
}

async fn related(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "id", "text", "depth", "limit", "includeHistory"])?;
        let target = selector(f.text("project")?.as_deref())?;
        let seed = match (f.text("id")?, f.text("text")?) {
            (Some(id), None) => Seed::Id(id),
            (None, Some(text)) => Seed::Text(text),
            _ => return Err(MemError::invalid("pass exactly one of id or text")),
        };
        let request = RelatedRequest {
            seed,
            depth: f.number("depth")?.map(|n| n.min(u32::MAX as u64) as u32),
            limit: f.number("limit")?.map(|n| n as usize),
            include_history: f.flag("includeHistory")?.unwrap_or(false),
        };
        // `semantic` says why the similar tier did not run (§memory.embedding).
        let outcome = service.related(&target, &request).await?;
        let mut reply = json!({ "items": outcome.items });
        if let Some(semantic) = &outcome.semantic {
            reply["semantic"] = json!(semantic);
        }
        ok(reply)
    })
}

async fn graph(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let request = GraphRequest {
            center: param(&params, "center").map(str::to_string),
            depth: param_number(&params, "depth")?.map(|n| n.min(u32::MAX as u64) as u32),
            include_history: param_flag(&params, "history"),
            limit: param_number(&params, "limit")?.map(|n| n as usize),
        };
        ok(service.graph(&target, &request).await?)
    })
}

async fn review(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(json!({ "items": service.review(&target).await? }))
    })
}

// --- misses (SPEC §memory.evaluation Misses) --------------------------------------------

/// `{project, query, via?, style?, shown?, wrong?, wanted?, want?, paths?}` → the miss.
async fn record_miss(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "query", "via", "style", "shown", "wrong", "wanted", "want", "paths"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(service.record_miss(&target, &Actor::panel(), f.miss_request()?).await?)
    })
}

async fn misses(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(json!({ "items": service.misses(&target).await? }))
    })
}

/// `{fixture, queries, gaps}`: the fixture the evaluation loads, and the misses no note holds.
async fn export_misses(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(service.export_misses(&target).await?)
    })
}

async fn drop_miss(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        ok(json!({ "dropped": service.drop_misses(&target, &[id]).await? }))
    })
}

// --- transfer ---------------------------------------------------------------------------

/// `?project=&domain=`: the project's JSONL, or one domain's subtree (its id or path).
async fn export(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let id = param(&params, "project").ok_or_else(|| MemError::invalid("project is required"))?;
        let (scope, part) = match param(&params, "domain") {
            None => (ExportScope::Project(id.to_string()), String::new()),
            Some(domain) => {
                let part: String = domain.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect();
                (ExportScope::Domain { project: id.to_string(), domain: domain.to_string() }, format!("-{part}"))
            }
        };
        let text = service.export(&scope).await?;
        let file = format!("attachment; filename=\"memory-{id}{part}.jsonl\"");
        Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, file)],
            text,
        )
            .into_response())
    })
}

/// `{project, text, dryRun?, strategy?}` → the import report. The file lands in `project`
/// (the export of one project, or of one of its domains, from this store or another); its
/// conflicts come back for a person to read before the real run.
async fn import(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "text", "dryRun", "strategy"])?;
        let project = f.required("project")?;
        selector(Some(&project))?;
        let strategy = match f.text("strategy")? {
            None => ImportStrategy::Skip,
            Some(raw) => ImportStrategy::parse(&raw).ok_or_else(|| MemError::invalid("strategy must be skip or fail"))?,
        };
        let options = ImportOptions { dry_run: f.flag("dryRun")?.unwrap_or(false), strategy, into: Some(project) };
        ok(service.import_with(&f.required("text")?, &options).await?)
    })
}

async fn backup(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        // The request names no path: the server picks one under the home (SPEC §memory.security).
        Fields(&body_of(&bytes)?).only(&[])?;
        let path = service.backup(None).await?;
        ok(json!({ "path": swiss_memory::scope::display(&path) }))
    })
}

// --- embedding (SPEC §memory.embedding) ------------------------------------------------

/// The llm plugin's embedding block memory serves by (the key as its reference, never a
/// value), whether note text leaves this machine, and the index's counts. The block is
/// edited on the llm plugin's Embedding page.
async fn embedding(State(state): State<Arc<MemoryState>>) -> Response {
    serve!(state, |service| {
        let seat = state.embedding();
        let config = seat.config.as_ref().map(EmbeddingConfig::to_json).unwrap_or_else(|| json!({ "provider": "off" }));
        let remote = seat.config.as_ref().is_some_and(EmbeddingConfig::remote);
        ok(json!({
            "config": config,
            "remote": remote,
            "configError": seat.error,
            "status": service.embedding_status().await?,
        }))
    })
}

/// One query vector from the provider in force: whether it answers, how wide, how
/// fast. A provider's refusal is the answer (`ok: false` and why), not a failure of
/// this route.
async fn embedding_test(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["text"])?;
        let text = f.text("text")?.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "memory embedding test".into());
        let Some(embedder) = service.embedder() else {
            return Err(MemError::invalid("embedding is off: configure an embedding provider first"));
        };
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(embedder.query_timeout(), embedder.embed(&[text], Purpose::Query)).await;
        let ms = started.elapsed().as_millis() as u64;
        let answer = match outcome {
            Ok(Ok(vectors)) => match vectors.first() {
                Some(v) => json!({ "ok": true, "model": embedder.id(), "dims": v.len(), "ms": ms }),
                None => json!({ "ok": false, "model": embedder.id(), "error": "the provider returned no vector", "ms": ms }),
            },
            Ok(Err(err)) => json!({ "ok": false, "model": embedder.id(), "error": err.message, "ms": ms }),
            Err(_) => json!({ "ok": false, "model": embedder.id(), "error": format!("no answer within {} ms", embedder.query_timeout().as_millis()), "ms": ms }),
        };
        ok(answer)
    })
}

/// Queue the index: `drop: true` first discards the provider's vectors (every note
/// is embedded again). The plugin's indexer does the work; the answer is the status
/// as it stands now.
async fn embedding_reindex(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["drop"])?;
        if service.embedder().is_none() {
            return Err(MemError::invalid("embedding is off: configure an embedding provider first"));
        }
        let dropped = if f.flag("drop")?.unwrap_or(false) { service.drop_vectors().await? } else { 0 };
        service.changes().notify_one();
        ok(json!({ "dropped": dropped, "status": service.embedding_status().await? }))
    })
}

// --- the code index (SPEC §memory.code) -------------------------------------------------------

/// A project's code index at a glance (`indexed: false` before the first build).
async fn code_status(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| ok(service.code_status(&selector(param(&params, "project"))?).await?))
}

/// Build or refresh a project's code index: `{project}`.
async fn code_index(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project"])?;
        let target = selector(f.text("project")?.as_deref())?;
        ok(service.code_index(&target).await?)
    })
}

/// Delete a project's code index (`all=true`: every project's); no note changes.
async fn code_drop(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let dropped = if param_flag(&params, "all") {
            service.code_drop_all().await?
        } else {
            service.code_drop(&selector(param(&params, "project"))?).await?
        };
        ok(json!({ "dropped": dropped }))
    })
}

/// Where a name is defined and which files mention it: `?project=&name=`.
async fn code_find(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let name = param(&params, "name").ok_or_else(|| MemError::invalid("name is required"))?;
        ok(service.code_find(&target, name).await?)
    })
}

/// One indexed file: its hash, definitions and imports: `?project=&path=`.
async fn code_file(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let path = param(&params, "path").ok_or_else(|| MemError::invalid("path is required"))?;
        ok(service.code_file(&target, path).await?)
    })
}

// --- documents (SPEC §memory.docs) ----------------------------------------------------------

/// The documents a project keeps: `?project=` lists them with their cards; with `q=`
/// the sections that match, best first (`kind=` keeps one kind, `limit=` caps the hits).
async fn docs_index(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let kind = match param(&params, "kind") {
            None => None,
            Some(kind) => Some(
                DocKind::parse(kind).ok_or_else(|| MemError::invalid(format!("kind must be one of {}", DocKind::choices())))?,
            ),
        };
        let limit = param_number(&params, "limit")?.map(|n| n as usize).unwrap_or(docs::SEARCH_DEFAULT);
        match param(&params, "q").map(str::trim).filter(|q| !q.is_empty()) {
            Some(query) => ok(json!({ "hits": service.docs_search(&target, query, limit, kind).await? })),
            None if kind.is_some() || params.contains_key("limit") => {
                Err(MemError::invalid("kind and limit go with q: a list shows every document"))
            }
            None => ok(service.docs_list(&target).await?),
        }
    })
}

/// One document: its card, outline and text - whole when short, else `section=`:
/// `?project=&doc=` (an id, a path or a unique tail of one).
async fn docs_read(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let target = selector(param(&params, "project"))?;
        let key = param(&params, "doc").map(str::trim).filter(|k| !k.is_empty());
        let key = key.ok_or_else(|| MemError::invalid("doc is required"))?;
        let section = param_number(&params, "section")?.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        ok(service.doc_view(&target, key, section).await?)
    })
}

// --- previews (SPEC §memory.preview) --------------------------------------------------------

/// The checkout of the `project` selector's project: its first reachable root.
async fn checkout(service: &MemoryService, params: &Params) -> MemResult<std::path::PathBuf> {
    selector(param(params, "project"))?;
    preview::root_of(&service.project(param(params, "project").unwrap_or_default()).await?)
}

/// The file or directory at `path` ("" or absent: the root).
async fn preview_file(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let root = checkout(&service, &params).await?;
        ok(preview::read(&root, &preview::relative(param(&params, "path").unwrap_or_default())?)?)
    })
}

/// The files a glob anchor matches now.
async fn preview_files(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let root = checkout(&service, &params).await?;
        let pattern = param(&params, "glob").ok_or_else(|| MemError::invalid("glob is required"))?;
        ok(preview::matching(&root, pattern).await?)
    })
}

async fn preview_log(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let root = checkout(&service, &params).await?;
        let rel = preview::relative(param(&params, "path").unwrap_or_default())?;
        let limit = param_number(&params, "limit")?.unwrap_or(preview::LOG_LIMIT_DEFAULT);
        ok(preview::history(&root, &rel, limit).await?)
    })
}

async fn preview_commit(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let root = checkout(&service, &params).await?;
        let rev = param(&params, "rev").ok_or_else(|| MemError::invalid("rev is required"))?;
        let path = param(&params, "path").map(preview::relative).transpose()?.filter(|p| !p.is_empty());
        ok(preview::commit(&root, rev, path.as_deref()).await?)
    })
}

async fn preview_diff(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let root = checkout(&service, &params).await?;
        ok(preview::working(&root, &preview::relative(param(&params, "path").unwrap_or_default())?).await?)
    })
}


// --- sync (SPEC §memory.api) -------------------------------------------------------------

/// The project id a sync query works on: the `project` selector, never `global`.
fn sync_project(params: &Params) -> MemResult<String> {
    match selector(param(params, "project"))? {
        Target::Project(id) if id != "global" => Ok(id),
        _ => Err(MemError::invalid("project must be a project id (p-…); the global scope has no checkout to sync")),
    }
}

/// The same from a POST body.
fn sync_body_project(f: &Fields) -> MemResult<String> {
    let id = f
        .text("project")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MemError::invalid("project is required"))?;
    if !is_project_id(&id) || id == "global" {
        return Err(MemError::invalid("project must be a project id (p-…); the global scope has no checkout to sync"));
    }
    Ok(id)
}

/// GET /api/knowledge/sync: the blocks in force, where the project stands, what is
/// pending, and the recent runs - the sync sheet's whole world in one answer.
async fn sync_overview(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let project_id = sync_project(&params)?;
        let views = service.projects().await?;
        let Some(view) = views.iter().find(|v| v.id == project_id) else {
            return Err(MemError::not_found(format!("no project {project_id}")));
        };
        let seat = state.sync_seat();
        // The workers and the lead a run would ask now (SPEC §memory.settings), each as the
        // llm row holds it; an error is the sentence a run would fail with (the plugin off,
        // no such agent, a provider gone).
        let settings = service.project_settings(&project_id).await?;
        let chosen = memory_sync::choose(&Choice::Inherit, &Choice::Inherit, false, &settings, seat.config.as_ref(), state.llm());
        let shown = |name: &String| match (state.llm().agent(name), memory_sync::runner(state.llm(), name)) {
            (Ok(agent), Ok(_)) => json!({ "name": name, "runtime": agent.runtime.as_str(), "model": agent.model }),
            (Ok(agent), Err(err)) => json!({ "name": name, "runtime": agent.runtime.as_str(), "error": err }),
            (Err(err), _) => json!({ "name": name, "error": err }),
        };
        let workers: Vec<Value> = chosen.agents.names.iter().map(shown).collect();
        let lead = chosen.lead.names.first().map(shown);
        let root = preview::root_of(view)?;
        let roots: Vec<String> = view.roots.iter().map(|r| r.path.clone()).collect();
        let dirs = seat.config.as_ref().map(|c| c.transcript_dir_list()).unwrap_or_default();
        let sync_state = service.sync_state(&project_id).await?;
        let (pending_commits, pending_bytes) = memory_sync::pending(&root, &dirs, &roots, &sync_state).await;
        let runs = service.sync_runs(&project_id, 10).await?;
        // A running run told to stop still says running until it reaches its next question.
        let stopping: Vec<&str> = runs
            .iter()
            .filter(|r| r.state == swiss_memory::sync::RunState::Running && state.sync_stopping(&r.id))
            .map(|r| r.id.as_str())
            .collect();
        ok(json!({
            "config": seat.config.as_ref().map(|c| c.to_json()),
            "configError": seat.error,
            "agent": workers.first(),
            "workers": workers,
            "lead": lead,
            "chosen": { "agents": chosen.agents.to_json(), "lead": chosen.lead.to_json() },
            "settings": settings.to_json(),
            "project": { "id": view.id, "name": view.name },
            "state": {
                "head": sync_state.head,
                "lastFinishedMs": sync_state.last_finished_ms,
                "pendingCommits": pending_commits,
                "pendingBytes": pending_bytes,
            },
            "runs": runs,
            "stopping": stopping,
        }))
    })
}

/// POST /api/knowledge/ask: one natural-language question answered from a
/// project's memory (SPEC §memory.rag) by the agent its layers name - the ask's
/// own `agent`, the project's settings, the sync block, the agent added last; a
/// read, so the newest may answer. `project` (a project id, or global) or an
/// absolute `cwd` selects the scope, exactly one of them; `worktree` (a checkout's
/// absolute path, or a branch one has checked out) names the working tree the code is
/// read in when it is not the cwd's own.
async fn knowledge_ask(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["project", "cwd", "scope", "question", "agent", "worktree"])?;
        // The question's shape is checked where every surface lands (memory_ask::answer).
        let question = f.required("question")?;
        let target = match (f.text("project")?, f.text("cwd")?) {
            (Some(id), None) => Target::project(id),
            (None, Some(cwd)) => match f.text("scope")?.as_deref() {
                None | Some("project") => Target::dir(cwd),
                Some("global") => Target::global(cwd),
                Some(other) => {
                    return Err(MemError::invalid(format!("scope must be project or global, not {other:?}")))
                }
            },
            _ => return Err(MemError::invalid("project or cwd: pass exactly one")),
        };
        let agent = f.text("agent")?.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        let worktree = f.text("worktree")?.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
        let answered = crate::memory_ask::answer(
            &state,
            &service,
            &target,
            &question,
            agent.as_deref(),
            worktree.as_deref(),
        )
        .await?;
        Ok((
            StatusCode::OK,
            Json(json!({ "answer": answered.text, "agent": answered.agent, "model": answered.model, "mode": answered.mode })),
        )
            .into_response())
    })
}

/// POST /api/knowledge/sync: queue a manual run now - 202 with the run (or the
/// queued run it merged into), the worker picks it up. `map: true` queues a map run
/// instead (SPEC §memory.map), `full: true` a full initialization (SPEC §memory.init),
/// either in `language` when given and redrawing what an earlier map drew when `redraw`;
/// `recheck: true` a recheck of the notes nobody judged since a model wrote them (SPEC
/// §memory.init The recheck); `maxAsks` caps a full run's knowledge questions or a
/// recheck's questions. `docs: true` queues a docs run: read the documents the project's
/// settings name and card them (SPEC §memory.docs), `maxAsks` capping its questions.
/// `review: true` queues a review run: every pending note in the review inbox judged
/// against the code, what it bears out confirmed and the contradictions settled (SPEC
/// §memory.init The recheck), `maxAsks` capping its questions; with `changed: true` only
/// the inbox's notes whose anchored files changed (SPEC §memory.init The review run).
/// `agents` (a name, a list, or "off") and `lead` (a name or "off") pick this run's own,
/// over the project's settings and the plugin's block (SPEC §memory.settings).
async fn sync_start(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&[
            "project", "dryRun", "since", "map", "full", "recheck", "docs", "review", "changed", "language", "redraw", "maxAsks",
            "agents", "lead",
        ])?;
        let agents = Choice::parse(body.get("agents"), "agents", false).map_err(MemError::invalid)?;
        let lead = Choice::parse(body.get("lead"), "lead", true).map_err(MemError::invalid)?;
        let project_id = sync_body_project(&f)?;
        let since = f.text("since")?.filter(|s| !s.trim().is_empty());
        let dry_run = f.flag("dryRun")?.unwrap_or(false);
        let language = f.text("language")?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let redraw = f.flag("redraw")?.unwrap_or(false);
        let max_asks = f.number("maxAsks")?.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let (map, full) = (f.flag("map")?.unwrap_or(false), f.flag("full")?.unwrap_or(false));
        let recheck = f.flag("recheck")?.unwrap_or(false);
        let (docs, review) = (f.flag("docs")?.unwrap_or(false), f.flag("review")?.unwrap_or(false));
        let changed = f.flag("changed")?.unwrap_or(false);
        if changed && !review {
            return Err(MemError::invalid("changed goes with review: it narrows a review run to the notes whose code changed"));
        }
        if docs || review {
            if [map, full, recheck, docs, review].iter().filter(|asked| **asked).count() > 1 {
                return Err(MemError::invalid("map, full, recheck, docs and review are five runs: ask for one"));
            }
            let (run_is, now) = match review {
                true => ("a review run", "judges the inbox as it is now"),
                false => ("a docs run", "reads the documents as they are now"),
            };
            if since.is_some() || redraw {
                return Err(MemError::invalid(format!("{run_is} {now}: it takes no since or redraw")));
            }
            if !lead.is_inherit() {
                return Err(MemError::invalid(format!("{run_is} asks its workers only: it has no lead")));
            }
            let mut request = match review {
                true => memory_sync::review_enqueue(dry_run, language, max_asks, changed),
                false => memory_sync::docs_enqueue(dry_run, language, max_asks),
            };
            request.agents = agents;
            let (run, merged) = service.sync_enqueue(&project_id, request).await?;
            state.wake_sync();
            return Ok((StatusCode::ACCEPTED, Json(json!({ "run": run, "merged": merged }))).into_response());
        }
        let request = match (map, full, recheck) {
            (true, true, _) | (true, _, true) | (_, true, true) => {
                return Err(MemError::invalid("map, full and recheck are three runs: ask for one"))
            }
            (true, false, false) => memory_sync::map_enqueue(dry_run, language, redraw),
            (false, true, false) => memory_sync::full_enqueue(dry_run, language, redraw, max_asks),
            (false, false, true) if redraw => {
                return Err(MemError::invalid("redraw goes with map or full: a recheck draws no map"))
            }
            (false, false, true) if since.is_some() => {
                return Err(MemError::invalid("a recheck judges the notes as they are now: it takes no since"))
            }
            (false, false, true) => memory_sync::recheck_enqueue(dry_run, language, max_asks),
            (false, false, false) if language.is_some() => {
                return Err(MemError::invalid(
                    "language goes with map, full, recheck, docs or review: a sync writes in the language the notes are in",
                ))
            }
            (false, false, false) if redraw => return Err(MemError::invalid("redraw goes with map or full: a sync draws no map")),
            (false, false, false) => memory_sync::manual_enqueue(dry_run, since.clone()),
        };
        if request.trigger.draws() && since.is_some() {
            return Err(MemError::invalid("a map is drawn from the code as it is now: it takes no since"));
        }
        if max_asks.is_some() && !full && !recheck {
            return Err(MemError::invalid("maxAsks goes with full, recheck, docs or review: only they ask per slice or per batch"));
        }
        if !lead.is_inherit() && !map && !full {
            return Err(MemError::invalid("lead goes with map or full: only they hand judgment to a lead"));
        }
        let mut request = request;
        request.agents = agents;
        request.lead = lead;
        let (run, merged) = service.sync_enqueue(&project_id, request).await?;
        state.wake_sync();
        Ok((StatusCode::ACCEPTED, Json(json!({ "run": run, "merged": merged }))).into_response())
    })
}

/// The survey a map or full run would start from now, no model asked - the redraw's
/// domains set aside when asked - and its plan: the areas, the files they own, how many
/// questions the run would ask, whether the top level goes to review, and how many code
/// files the map the project has already owns (an empty plan over a covered map).
async fn drawn_plan(service: &MemoryService, project_id: &str, redraw: bool) -> MemResult<(Survey, Vec<String>, Value)> {
    let removed = if redraw { service.map_redraw_plan(project_id).await? } else { Vec::new() };
    let survey = service.map_survey(project_id, &Draw { regroup: None, ignore: removed.clone() }).await?;
    let plan = json!({
        "plan": survey.plan(),
        "asks": survey.asks(),
        "review": survey.review_prompt(None).is_some(),
        "removed": removed,
        "codeFiles": survey.code_files,
        "ownedFiles": survey.owned_files,
        "mappedFiles": survey.mapped_files,
        "truncated": survey.truncated,
    });
    Ok((survey, removed, plan))
}

/// GET /api/knowledge/sync/map: the map a map run would draw now ([drawn_plan]);
/// `redraw=1` plans a redraw.
async fn sync_map_plan(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let project_id = sync_project(&params)?;
        let (_, _, plan) = drawn_plan(&service, &project_id, param_flag(&params, "redraw")).await?;
        ok(plan)
    })
}

/// GET /api/knowledge/sync/full: what a full run would do now, no model asked - the map
/// plan ([drawn_plan]), then the code and docs cut into slices: how many it would ask
/// (`maxAsks`, 200 when absent), how many an earlier full run answered and are unchanged,
/// how many wait for a later run - and the semantic index the notes would land in (off:
/// lexical search only).
async fn sync_full_plan(State(state): State<Arc<MemoryState>>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let project_id = sync_project(&params)?;
        let cap = match param_number(&params, "maxAsks")? {
            Some(0) => return Err(MemError::invalid("maxAsks is at least 1")),
            Some(n) => usize::try_from(n).unwrap_or(usize::MAX),
            None => swiss_memory::init::ASKS_DEFAULT,
        };
        let (survey, removed, mut plan) = drawn_plan(&service, &project_id, param_flag(&params, "redraw")).await?;
        // The slices are cut against the map the run would leave: the one there, redrawn.
        let current = service.map_domains(&project_id).await?;
        let domains = swiss_memory::init::drawn_domains(&current, &removed, &survey);
        let sliced = service.knowledge_slices(&project_id, survey.sources.clone(), Some(domains)).await?;
        let answered = service.knowledge_answered(&project_id).await?;
        let (knowledge, todo) = swiss_memory::init::knowledge_plan(&sliced, &answered, cap);
        plan["knowledge"] = json!({
            "files": knowledge.files,
            "bytes": knowledge.bytes,
            "slices": knowledge.slices,
            "asks": todo.len(),
            "unchanged": knowledge.unchanged,
            "more": knowledge.more,
            "unread": sliced.unread.len(),
        });
        // The semantic index the notes would land in - none, when the project's settings
        // turn embedding off (§memory.embedding): its notes are no embedding input.
        if service.embedding_on(&project_id).await? {
            let embedding = service.embedding_status().await?;
            plan["embedding"] = json!(swiss_memory::sync::EmbeddingScope::from(&embedding));
        } else {
            plan["embedding"] = json!(swiss_memory::sync::EmbeddingScope { state: "off".into(), ..Default::default() });
        }
        ok(plan)
    })
}

/// POST /api/knowledge/sync/nudge: a hook's or a session-end's nudge - the cwd says
/// the project, and the answer says whether a run was queued for it (a guard that
/// says no names why, in `reason`).
async fn sync_nudge(State(state): State<Arc<MemoryState>>, bytes: Bytes) -> Response {
    serve!(state, |service| {
        let body = body_of(&bytes)?;
        let f = Fields(&body);
        f.only(&["cwd"])?;
        let cwd = f.required("cwd")?;
        let seat = state.sync_seat();
        let Some(config) = seat.config else {
            return ok(json!({ "queued": false, "reason": "no-sync-block" }));
        };
        if !config.auto {
            return ok(json!({ "queued": false, "reason": "auto-off" }));
        }
        let Some(project) = service.resolve(&Target::dir(cwd)).await? else {
            return ok(json!({ "queued": false, "reason": "not-a-project" }));
        };
        if project.id == swiss_memory::GLOBAL_PROJECT_ID {
            return ok(json!({ "queued": false, "reason": "not-a-project" }));
        }
        // A nudge's run asks only an agent someone named: never the newest as a fallback.
        let settings = service.project_settings(&project.id).await?;
        let chosen = memory_sync::choose(&Choice::Inherit, &Choice::Inherit, true, &settings, Some(&config), state.llm());
        if chosen.agents.names.is_empty() {
            return ok(json!({ "queued": false, "reason": "no-agent" }));
        }
        let sync_state = service.sync_state(&project.id).await?;
        let request = memory_sync::nudge_enqueue(sync_state.last_finished_ms, config.min_interval_sec);
        let due = request.due_ms;
        let (run, merged) = service.sync_enqueue(&project.id, request).await?;
        state.wake_sync();
        ok(json!({ "queued": true, "merged": merged, "dueMs": due, "run": run }))
    })
}

/// GET /api/knowledge/sync/runs/{id}: one run, `project` selecting it.
async fn sync_run(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let project_id = sync_project(&params)?;
        let run = service.sync_run(&project_id, &id).await?;
        ok(json!({ "run": run }))
    })
}

/// POST /api/knowledge/sync/runs/{id}/cancel: stop a run, `project` selecting it - or, left
/// out, the run found by its id alone (the LLM page's agent runs know only their memory run).
/// A queued run is cancelled at once and never starts; a running one is told to stop
/// (`stopping`) and ends cancelled between its questions, keeping what it saved. An ended run
/// is refused.
async fn sync_cancel(State(state): State<Arc<MemoryState>>, Path(id): Path<String>, Query(params): Query<Params>) -> Response {
    serve!(state, |service| {
        let project_id = match param(&params, "project") {
            Some(_) => Some(sync_project(&params)?),
            None => None,
        };
        let run = service.sync_cancel(project_id.as_deref(), &id).await?;
        let stopping = run.state == swiss_memory::sync::RunState::Running;
        if stopping {
            state.sync_stop(&run.id).raise();
        }
        ok(json!({ "run": run, "stopping": stopping }))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn app(label: &str) -> (Router, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "swiss-memapi-{label}-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let dir = dir.canonicalize().expect("canonical scratch");
        let state = Arc::new(MemoryState::new());
        state.install(Arc::new(MemoryService::at(&dir.join("store.sqlite3"))));
        (mount(state), dir)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn knowledge_api_project_and_note_crud() {
        let (app, dir) = app("crud").await;
        let (status, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Docs" }))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();

        let note = json!({ "project": project, "kind": "decision", "title": "Use WAL", "content": "No other reader blocks." });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(note.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["record"]["id"].as_str().expect("record id").to_string();
        assert_eq!(body["record"]["actor"], "panel");
        assert_eq!(body["record"]["confirmed"], true, "a note typed in the panel starts confirmed");
        assert_eq!(body["formation"][0]["rule"], "quantifier", "an unscoped \"no other\" is a hint, and the save landed: {body}");
        let long = json!({ "project": project, "kind": "knowledge", "title": "Long", "content": "Readers wait. ".repeat(100) });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(long)).await;
        assert_eq!((status, body["formation"][0]["rule"].as_str()), (StatusCode::CREATED, Some("long")), "{body}");
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
        assert_eq!((status, body["duplicate"].as_bool()), (StatusCode::OK, Some(true)));

        let similar = json!({ "project": project, "kind": "decision", "title": "use wal", "content": "Other." });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(similar)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "memory_similar_exists");
        assert_eq!(body["similar"]["id"], id.as_str());

        let edit = json!({ "project": project, "expectedRevision": 9, "title": "WAL mode" });
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/records/{id}"), Some(edit)).await;
        assert_eq!((status, body["error"].as_str(), body["current"].as_u64()), (StatusCode::CONFLICT, Some("memory_revision_conflict"), Some(1)));

        let (status, body) = call(&app, "GET", &format!("/api/knowledge/records?project={project}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"].as_array().map(Vec::len), Some(2), "Use WAL and Long");

        let (status, body) = call(&app, "GET", "/api/knowledge/records", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")));

        let (status, _) = call(&app, "GET", &format!("/api/knowledge/records/{id}?project=global"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "ids are rechecked against the selector");

        let (status, body) = call(&app, "DELETE", &format!("/api/knowledge/records/{id}?project={project}&confirm={id}"), None).await;
        assert_eq!((status, body["deleted"].as_bool()), (StatusCode::OK, Some(true)));

        let (status, body) = call(&app, "DELETE", &format!("/api/knowledge/projects/{project}?confirm={project}"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Provenance and evidence on the wire (ADR-032): a person typed the note in the panel,
    /// and a verified mark names what it was checked against.
    #[tokio::test]
    async fn knowledge_api_provenance_and_evidence() {
        let (app, dir) = app("evidence").await;
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Docs" }))).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let note = json!({
            "project": project, "kind": "decision", "title": "Retries reuse the key", "content": "So a replay is idempotent.",
            "evidence": [{ "sourceType": "session", "sessionId": "s-1", "excerpt": "we agreed" }]
        });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let record = &body["record"];
        assert_eq!((record["producer"].as_str(), record["transport"].as_str()), (Some("human"), Some("panel")));
        assert_eq!((record["confirmed"].as_bool(), record["sourceVerified"].as_bool()), (Some(true), Some(false)));
        let piece = &record["evidence"][0];
        assert_eq!((piece["relation"].as_str(), piece["current"].as_bool()), (Some("derived_from"), Some(true)), "{piece}");
        assert!(piece["excerptHash"].as_str().is_some_and(|h| h.starts_with("sha256:")) && piece.get("excerpt").is_none(), "{piece}");
        let id = record["id"].as_str().expect("id").to_string();

        let lined = json!({ "project": project, "verdict": "verified", "evidence": [{ "sourceType": "code", "path": "src/a.rs", "line": 12 }] });
        let (status, body) = call(&app, "POST", &format!("/api/knowledge/records/{id}/mark"), Some(lined)).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")), "a line number cannot ride along: {body}");
        let verified = json!({ "project": project, "verdict": "verified", "evidence": [{ "sourceType": "test", "path": "tests/retry.rs", "symbol": "reuses_the_key" }] });
        let (status, body) = call(&app, "POST", &format!("/api/knowledge/records/{id}/mark"), Some(verified)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["record"]["sourceVerified"], true, "{body}");
        assert_eq!(body["record"]["evidence"][1]["relation"], "verified_by");

        let more = json!({ "project": project, "evidence": [{ "sourceType": "commit", "relation": "supports", "commitSha": "ABC1234" }] });
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/records/{id}"), Some(more)).await;
        assert_eq!(status, StatusCode::OK, "evidence alone needs no expectedRevision: {body}");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/records/{id}?project={project}"), None).await;
        assert_eq!((body["record"]["revision"].as_u64(), body["record"]["evidence"].as_array().map(Vec::len)), (Some(2), Some(3)), "{body}");
        assert_eq!(body["record"]["evidence"][2]["commitSha"], "abc1234");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/records?project={project}"), None).await;
        assert_eq!(body["items"][0]["sourceVerified"], true, "the list says so too: {body}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn knowledge_api_graph_related_and_status() {
        let (app, _dir) = app("graph").await;
        let mut ids = Vec::new();
        for (title, content) in [("Payment retries", "Retry the payment call with backoff."), ("Payment idempotency", "Retries reuse one payment key.")] {
            let note = json!({ "project": "global", "kind": "knowledge", "title": title, "content": content });
            let (_, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
            ids.push(body["record"]["id"].as_str().expect("id").to_string());
        }
        let link = json!({ "project": "global", "from": ids[0], "to": ids[1], "kind": "depends_on", "note": "same key" });
        let (status, body) = call(&app, "POST", "/api/knowledge/links", Some(link)).await;
        assert_eq!((status, body["changed"].as_bool(), body["link"]["directed"].as_bool()), (StatusCode::OK, Some(true), Some(true)));

        let (_, body) = call(&app, "GET", &format!("/api/knowledge/graph?project=global&center={}&depth=2", ids[0]), None).await;
        assert_eq!(body["nodes"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["nodes"][0]["hop"], 0);

        let (_, body) = call(&app, "POST", "/api/knowledge/related", Some(json!({ "project": "global", "id": ids[0] }))).await;
        assert_eq!(body["items"][0]["id"], ids[1].as_str());
        assert_eq!(body["items"][0]["reasons"][0]["type"], "link");

        let (status, _) = call(&app, "POST", "/api/knowledge/related", Some(json!({ "project": "global" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, body) = call(&app, "GET", "/api/knowledge/status", None).await;
        assert_eq!(body["schemaVersion"], swiss_memory::store::SCHEMA_VERSION);
        assert_eq!(body["mcp"]["state"], "stopped");

        let (status, body) = call(&app, "POST", "/api/knowledge/brief", Some(json!({ "project": "global", "task": "payment" }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["markdown"].as_str().unwrap_or_default().contains(&ids[0]));
    }

    #[tokio::test]
    async fn knowledge_api_misses_record_list_export_and_drop() {
        let (app, _dir) = app("misses").await;
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Shop" }))).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let mut ids = Vec::new();
        for (title, content) in [("Retries reuse the idempotency key", "A retried charge sends the same key."), ("Support answers timeouts", "A customer timeout goes to support.")] {
            let note = json!({ "project": project, "kind": "knowledge", "title": title, "content": content });
            let (_, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
            ids.push(body["record"]["id"].as_str().expect("id").to_string());
        }
        let miss = json!({ "project": project, "query": "customer billed twice", "via": "search", "shown": [ids[1]], "wrong": [ids[1]], "wanted": [ids[0]] });
        let (status, body) = call(&app, "POST", "/api/knowledge/misses", Some(miss)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let miss_id = body["id"].as_str().expect("miss id").to_string();
        assert_eq!((body["actor"].as_str(), body["atMs"].is_i64()), (Some("panel"), true), "{body}");

        let (status, body) = call(&app, "POST", "/api/knowledge/misses", Some(json!({ "project": project, "query": "x" }))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")), "nothing wanted: {body}");
        let (status, _) = call(&app, "POST", "/api/knowledge/misses", Some(json!({ "project": project, "query": "x", "want": "y", "extra": 1 }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "strict body");

        let (_, body) = call(&app, "GET", &format!("/api/knowledge/misses?project={project}"), None).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1), "{body}");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/misses/export?project={project}"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["queries"], 1);
        let fixture = swiss_memory::eval::Fixture::parse(&body["fixture"].to_string()).expect("a fixture");
        assert_eq!((fixture.queries[0].expect.clone(), fixture.queries[0].avoid.clone()), (vec![ids[0].clone()], vec![ids[1].clone()]));

        let (status, body) = call(&app, "DELETE", &format!("/api/knowledge/misses/{miss_id}?project={project}"), None).await;
        assert_eq!((status, body["dropped"].as_u64()), (StatusCode::OK, Some(1)), "{body}");
        let (status, _) = call(&app, "DELETE", &format!("/api/knowledge/misses/{miss_id}?project={project}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// One domain from one store to another over the panel's routes: the export names the
    /// subtree in its file name and header, the dry run brings the conflict back to read.
    #[tokio::test]
    async fn knowledge_api_domain_export_and_import_report() {
        let (alice, _a) = app("transfer-a").await;
        let (bob, _b) = app("transfer-b").await;
        let (_, body) = call(&alice, "POST", "/api/knowledge/projects", Some(json!({ "name": "Shop" }))).await;
        let theirs = body["project"]["id"].as_str().expect("id").to_string();
        let area = json!({ "project": theirs, "path": "billing", "title": "Billing" });
        let (status, body) = call(&alice, "POST", "/api/knowledge/domains", Some(area)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        for (title, content, domain) in [("Refunds retry twice", "Then a person looks.", Some("billing")), ("Logs rotate daily", "Kept a week.", None)] {
            let note = json!({ "project": theirs, "kind": "decision", "title": title, "content": content, "domain": domain });
            let (status, body) = call(&alice, "POST", "/api/knowledge/records", Some(note)).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        let request = Request::builder()
            .uri(format!("/api/knowledge/export?project={theirs}&domain=billing"))
            .body(Body::empty())
            .expect("request");
        let response = alice.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let disposition = response.headers()[header::CONTENT_DISPOSITION].to_str().unwrap_or_default().to_string();
        assert!(disposition.contains(&format!("memory-{theirs}-billing.jsonl")), "{disposition}");
        let text = String::from_utf8(axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body").to_vec()).expect("utf-8");
        assert!(text.lines().next().is_some_and(|h| h.contains("\"domain\":\"billing\"")) && !text.contains("Logs rotate daily"), "{text}");

        let (_, body) = call(&bob, "POST", "/api/knowledge/projects", Some(json!({ "name": "Shop" }))).await;
        let mine = body["project"]["id"].as_str().expect("id").to_string();
        let note = json!({ "project": mine, "kind": "decision", "title": "Refunds retry twice", "content": "Three times, here." });
        call(&bob, "POST", "/api/knowledge/records", Some(note)).await;
        let (status, body) = call(&bob, "POST", "/api/knowledge/import", Some(json!({ "project": mine, "text": text, "dryRun": true }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["dryRun"].as_bool(), body["into"].as_str(), body["source"]["domain"].as_str()), (Some(true), Some(mine.as_str()), Some("billing")));
        assert_eq!((body["domainsCreated"].as_u64(), body["recordsCreated"].as_u64()), (Some(1), Some(0)), "{body}");
        let conflict = &body["conflicts"][0];
        assert_eq!((conflict["kind"].as_str(), conflict["stored"]["excerpt"].as_str()), (Some("title"), Some("Three times, here.")), "{body}");
        let (status, body) = call(&bob, "POST", "/api/knowledge/import", Some(json!({ "project": mine, "text": text, "strategy": "fail" }))).await;
        assert_eq!(body["error"].as_str(), Some("memory_similar_exists"), "{status} {body}");
        let (status, body) = call(&bob, "POST", "/api/knowledge/import", Some(json!({ "project": "global", "text": text }))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")), "a project's file never lands global");
        let (status, _) = call(&bob, "POST", "/api/knowledge/import", Some(json!({ "project": mine, "text": text, "into": mine }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "strict body");
        let (_, body) = call(&bob, "GET", &format!("/api/knowledge/records?project={mine}"), None).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1), "nothing was written: {body}");
    }

    /// Two features (does the text say "retry", does it say "lock") as a vector: a
    /// provider a test can predict, or one that always refuses.
    struct Stand {
        refuse: bool,
    }

    #[async_trait::async_trait]
    impl swiss_memory::Embedder for Stand {
        fn id(&self) -> &str {
            "stand:two:2"
        }

        async fn embed(&self, texts: &[String], _purpose: Purpose) -> MemResult<Vec<Vec<f32>>> {
            if self.refuse {
                return Err(MemError::invalid("the provider answered 401 Unauthorized: bad key"));
            }
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    vec![if t.contains("retry") { 1.0 } else { 0.1 }, if t.contains("lock") { 1.0 } else { 0.1 }]
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn knowledge_api_embedding_status_test_and_semantic_search() {
        let dir = std::env::temp_dir().join(format!("swiss-memapi-embed-{}-{}", std::process::id(), swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let state = Arc::new(MemoryState::new());
        let service = Arc::new(MemoryService::at(&dir.join("store.sqlite3")));
        state.install(service.clone());
        let app = mount(state.clone());

        // Off: said so everywhere, and the test and reindex verbs refuse.
        let (status, body) = call(&app, "GET", "/api/knowledge/embedding", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["config"]["provider"].as_str(), body["status"]["state"].as_str()), (Some("off"), Some("off")));
        assert_eq!(body["remote"], false);
        let (status, _) = call(&app, "POST", "/api/knowledge/embedding/test", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "POST", "/api/knowledge/embedding/reindex", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let note = json!({ "project": "global", "kind": "pitfall", "title": "Idempotent order calls", "content": "Send the same key on every retry." });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (_, body) = call(&app, "GET", "/api/knowledge/search?project=global&q=retry", None).await;
        assert_eq!(body["semantic"]["state"], "off");
        assert_eq!(body["items"][0]["title"], "Idempotent order calls");

        // A provider in force: the block names its provider (the key stays on the llm
        // row's provider entry, never here), a remote endpoint is flagged, the test
        // answers its width.
        let config = swiss_llm::LlmConfig::parse(&json!({
            "providers": { "hosted": { "style": "openai", "baseUrl": "https://api.example.com/v1", "apiKey": "${secret://embed}",
                                       "models": [{ "id": "two", "kind": "embedding" }] } },
            "embedding": { "provider": "hosted", "model": "two" }
        }))
        .expect("config")
        .embedding;
        state.set_embedding(config, None);
        service.set_embedder(Some(Arc::new(Stand { refuse: false })));
        let (_, body) = call(&app, "GET", "/api/knowledge/embedding", None).await;
        assert_eq!(body["config"]["provider"], "hosted");
        assert!(body["config"].get("apiKey").is_none(), "{body}");
        assert_eq!(body["remote"], true);
        assert_eq!((body["status"]["pending"].as_u64(), body["status"]["indexed"].as_u64()), (Some(1), Some(0)));
        let (_, body) = call(&app, "POST", "/api/knowledge/embedding/test", Some(json!({ "text": "retry" }))).await;
        assert_eq!((body["ok"].as_bool(), body["dims"].as_u64(), body["model"].as_str()), (Some(true), Some(2), Some("stand:two:2")));

        // Indexed: a query sharing no word with the note ("retrying" is not "retry"
        // to the tokenizer) finds it by meaning alone.
        let (_, body) = call(&app, "GET", "/api/knowledge/search?project=global&q=retrying", None).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0), "{body}");
        let report = service.index_pending(64).await.expect("index");
        assert_eq!((report.embedded, report.pending), (1, 0));
        let (_, body) = call(&app, "GET", "/api/knowledge/search?project=global&q=retrying", None).await;
        assert_eq!(body["semantic"]["state"], "on", "{body}");
        assert_eq!(body["items"][0]["title"], "Idempotent order calls", "{body}");
        // `semantic=off`: the words alone, with the provider still in force (an A/B).
        let (_, body) = call(&app, "GET", "/api/knowledge/search?project=global&q=retrying&semantic=off", None).await;
        assert_eq!(body["semantic"]["state"], "off", "{body}");
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0), "{body}");
        let (status, _) = call(&app, "GET", "/api/knowledge/search?project=global&q=retrying&semantic=maybe", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, body) = call(&app, "POST", "/api/knowledge/embedding/reindex", Some(json!({ "drop": true }))).await;
        assert_eq!((body["dropped"].as_u64(), body["status"]["pending"].as_u64()), (Some(1), Some(1)));
        assert_eq!((body["status"]["incompatible"].as_u64(), body["status"]["dims"].as_u64()), (Some(0), Some(2)), "{body}");

        // A refusing provider: the test says why, search stays lexical and says so.
        service.set_embedder(Some(Arc::new(Stand { refuse: true })));
        let (status, body) = call(&app, "POST", "/api/knowledge/embedding/test", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap_or_default().contains("401"), "{body}");
        let (_, body) = call(&app, "GET", "/api/knowledge/search?project=global&q=retry", None).await;
        assert_eq!(body["items"][0]["title"], "Idempotent order calls");
        assert_ne!(body["semantic"]["state"], "on");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The code index over the API (SPEC §memory.code): build, status, find, file and drop; a
    /// panel save is a person's, so a name the checkout lacks is told, never held.
    #[tokio::test]
    async fn knowledge_api_code_index() {
        let dir = std::env::temp_dir().join(format!("swiss-memapi-code-{}-{}", std::process::id(), swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(dir.join("checkout").join("src")).expect("checkout");
        let dir = dir.canonicalize().expect("canonical scratch");
        std::fs::write(dir.join("checkout/src/lib.rs"), "pub fn fit_rail_labels() {}\n").expect("write");
        let state = Arc::new(MemoryState::new());
        let service = Arc::new(MemoryService::at(&dir.join("store.sqlite3")));
        service.set_lister(Some(Arc::new(preview::CheckoutLister)));
        state.install(service);
        let app = mount(state);
        let body = json!({ "name": "Code", "path": dir.join("checkout").display().to_string(), "exact": true });
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(body)).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();

        let (status, body) = call(&app, "GET", &format!("/api/knowledge/code?project={project}"), None).await;
        assert_eq!((status, body["indexed"].as_bool()), (StatusCode::OK, Some(false)), "{body}");
        let (status, body) = call(&app, "POST", "/api/knowledge/code/index", Some(json!({ "project": project }))).await;
        assert_eq!((status, body["read"].as_u64()), (StatusCode::OK, Some(1)), "{body}");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/code/find?project={project}&name=fit_rail_labels"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["defs"][0]["path"].as_str(), body["defs"][0]["kind"].as_str()), (Some("src/lib.rs"), Some("fn")), "{body}");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/code/file?project={project}&path=src/lib.rs"), None).await;
        assert_eq!((status, body["lang"].as_str()), (StatusCode::OK, Some("rust")), "{body}");
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/code/find?project={project}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a find names what it looks for");
        let (status, _) = call(&app, "GET", "/api/knowledge/code?project=global", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "the global notes have no checkout");

        let note = json!({ "project": project, "kind": "knowledge", "title": "Rail", "content": "fit_rail_labelz sizes captions." });
        let (status, body) = call(&app, "POST", "/api/knowledge/records", Some(note)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let told = (body["code"]["findings"][0]["rule"].as_str(), body["code"]["held"].as_bool());
        assert_eq!(told, (Some("unknown_name"), Some(false)), "{body}");
        assert_eq!(body["record"]["status"], "active");

        let (status, body) = call(&app, "DELETE", &format!("/api/knowledge/code?project={project}"), None).await;
        assert_eq!((status, body["dropped"].as_u64()), (StatusCode::OK, Some(1)), "{body}");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/code?project={project}"), None).await;
        assert_eq!(body["indexed"], false);
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/records?project={project}"), None).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1), "dropping the index leaves every note: {body}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn knowledge_api_previews_stay_inside_the_checkout() {
        let (app, dir) = app("preview").await;
        let checkout = dir.join("checkout");
        std::fs::create_dir_all(checkout.join("src")).expect("checkout");
        std::fs::write(checkout.join("src/lib.rs"), "pub fn one() {}\n").expect("write");
        std::fs::write(checkout.join(".env"), "TOKEN=x\n").expect("write");
        let body = json!({ "name": "Code", "path": checkout.display().to_string(), "exact": true });
        let (status, body) = call(&app, "POST", "/api/knowledge/projects", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();

        let (status, body) = call(&app, "GET", &format!("/api/knowledge/preview/file?project={project}&path=src/lib.rs"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["kind"].as_str(), body["text"].as_str(), body["lines"].as_u64()), (Some("file"), Some("pub fn one() {}\n"), Some(1)));
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/preview/file?project={project}"), None).await;
        assert_eq!((status, body["kind"].as_str()), (StatusCode::OK, Some("dir")), "{body}");

        for (path, code) in [(".env", "memory_sensitive_content"), ("..%2Fstore.sqlite3", "memory_invalid"), ("nope.rs", "memory_not_found")] {
            let (status, body) = call(&app, "GET", &format!("/api/knowledge/preview/file?project={project}&path={path}"), None).await;
            assert_eq!(body["error"], code, "{path}: {status} {body}");
        }
        let (status, body) = call(&app, "GET", "/api/knowledge/preview/file?project=global&path=a", None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("memory_not_found")), "{body}");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/preview/commit?project={project}&rev=HEAD"), None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn knowledge_api_domains_crud_and_filing() {
        let (app, dir) = app("domains").await;
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Shop" }))).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let domain = |path: &str, owns: &[&str]| json!({ "project": project, "path": path, "title": path.to_uppercase(), "owns": owns });
        let (status, body) = call(&app, "POST", "/api/knowledge/domains", Some(domain("app", &["src"]))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["domain"]["confirmed"], true, "a domain made in the panel starts confirmed");
        let app_id = body["domain"]["id"].as_str().expect("id").to_string();
        let (_, body) = call(&app, "POST", "/api/knowledge/domains", Some(domain("app/api", &["src/api"]))).await;
        let api_id = body["domain"]["id"].as_str().expect("id").to_string();
        let (status, body) = call(&app, "POST", "/api/knowledge/domains", Some(domain("app", &[]))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("memory_domain_exists")));
        let global = json!({ "project": "global", "path": "x", "title": "X" });
        let (status, _) = call(&app, "POST", "/api/knowledge/domains", Some(global)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "the global scope has no map");

        // Filed by anchors, by hand, or not at all.
        let note = |title: &str, extra: Value| {
            let mut n = json!({ "project": project, "kind": "knowledge", "title": title, "content": "x" });
            if let (Some(n), Some(extra)) = (n.as_object_mut(), extra.as_object()) {
                n.extend(extra.clone());
            }
            n
        };
        let (_, body) = call(&app, "POST", "/api/knowledge/records", Some(note("Orders validate", json!({ "anchors": ["src/api/orders.rs"] })))).await;
        assert_eq!(body["record"]["domain"], "app/api", "{body}");
        let anchored = body["record"]["id"].as_str().expect("id").to_string();
        let (_, body) = call(&app, "POST", "/api/knowledge/records", Some(note("App boots once", json!({ "domain": "app" })))).await;
        assert_eq!(body["record"]["domain"], "app", "{body}");
        let (_, body) = call(&app, "POST", "/api/knowledge/records", Some(note("Loose", json!({ "anchors": ["docs/guide.md"] })))).await;
        let loose = body["record"]["id"].as_str().expect("id").to_string();
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/records?project={project}&domain={app_id}"), None).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(2), "a domain's list holds its subtree: {body}");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/records?project={project}&domain=none"), None).await;
        assert_eq!(body["items"][0]["id"], loose.as_str());
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/search?project={project}&q=orders&domain={api_id}"), None).await;
        assert_eq!(body["items"][0]["id"], anchored.as_str(), "{body}");

        let (_, body) = call(&app, "GET", &format!("/api/knowledge/domains?project={project}"), None).await;
        assert_eq!((body["items"].as_array().map(Vec::len), body["unfiled"]["live"].as_u64()), (Some(2), Some(1)), "{body}");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/domains/{api_id}?project={project}&history=true"), None).await;
        assert_eq!((body["domain"]["notes"][0]["id"].as_str(), body["domain"]["parent"].as_str()), (Some(anchored.as_str()), Some("app")), "{body}");
        assert_eq!(body["domain"]["history"][0]["op"], "create");

        // Filing needs no revision; anything else still does.
        let refile = json!({ "project": project, "domain": "app/api" });
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/records/{loose}"), Some(refile)).await;
        assert_eq!((status, body["record"]["domain"].as_str(), body["record"]["revision"].as_u64()), (StatusCode::OK, Some("app/api"), Some(1)), "{body}");
        let retitle = json!({ "project": project, "title": "Renamed" });
        let (status, _) = call(&app, "PUT", &format!("/api/knowledge/records/{loose}"), Some(retitle)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let unfile = json!({ "project": project, "ids": [loose], "domain": null });
        let (_, body) = call(&app, "POST", "/api/knowledge/records/file", Some(unfile)).await;
        assert_eq!((body["moved"].as_u64(), body["domain"].is_null()), (Some(1), true), "{body}");

        // Confirmed whole: the counts beside the domain (a panel note starts confirmed: none move).
        let whole = json!({ "project": project, "all": true });
        let (status, body) = call(&app, "POST", &format!("/api/knowledge/domains/{app_id}/confirm"), Some(whole)).await;
        let counts = (body["domains"].as_u64(), body["notes"].as_u64(), body["held"].as_u64());
        assert_eq!((status, body["domain"]["path"].as_str(), counts), (StatusCode::OK, Some("app"), (Some(0), Some(0), Some(0))), "{body}");
        let odd = json!({ "project": project, "all": "yes" });
        let (status, _) = call(&app, "POST", &format!("/api/knowledge/domains/{app_id}/confirm"), Some(odd)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Move, verify, delete.
        let moved = json!({ "project": project, "expectedRevision": 1, "path": "core" });
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/domains/{app_id}"), Some(moved.clone())).await;
        assert_eq!((status, body["domain"]["path"].as_str()), (StatusCode::OK, Some("core")), "{body}");
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/domains/{app_id}"), Some(moved)).await;
        assert_eq!((status, body["current"].as_u64()), (StatusCode::CONFLICT, Some(2)));
        let (status, body) = call(&app, "POST", &format!("/api/knowledge/domains/{api_id}/verify"), Some(json!({ "project": project }))).await;
        assert_eq!((status, body["domain"]["path"].as_str(), body["domain"]["revision"].as_u64()), (StatusCode::OK, Some("core/api"), Some(3)), "{body}");
        let (status, _) = call(&app, "DELETE", &format!("/api/knowledge/domains/{app_id}?project={project}&confirm=core"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "sub-domains first");
        let (status, body) = call(&app, "DELETE", &format!("/api/knowledge/domains/{api_id}?project={project}&confirm=core/api"), None).await;
        assert_eq!((status, body["refiled"].as_u64(), body["to"].as_str()), (StatusCode::OK, Some(1), Some("core")), "{body}");
        // A map drawn after the notes: autofile files them by their anchors.
        call(&app, "POST", "/api/knowledge/domains", Some(domain("docs", &["docs"]))).await;
        let (_, body) = call(&app, "POST", "/api/knowledge/domains/autofile", Some(json!({ "project": project }))).await;
        assert_eq!(body["filed"], 1, "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn knowledge_api_projects_change_a_directory() {
        let (app, dir) = app("rebind").await;
        for name in ["old", "new"] {
            std::fs::create_dir_all(dir.join(name)).expect("dir");
        }
        let at = |name: &str| dir.join(name).display().to_string();
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Moved", "path": at("old"), "exact": true }))).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let identity = body["project"]["roots"][0]["identity"].as_str().expect("identity").to_string();
        let rebind = json!({ "identity": identity, "path": at("new"), "exact": true });
        let (status, body) = call(&app, "PUT", &format!("/api/knowledge/projects/{project}/roots"), Some(rebind)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["project"]["roots"][0]["path"].as_str().unwrap_or_default().ends_with("new"), "{body}");
        let (status, body) = call(&app, "POST", "/api/knowledge/projects", Some(json!({ "name": "Thief", "path": at("new"), "exact": true }))).await;
        assert_eq!((status, body["error"].as_str(), body["projectId"].as_str()), (StatusCode::CONFLICT, Some("memory_root_bound"), Some(project.as_str())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn git(root: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn knowledge_api_domains_coverage() {
        let (app, dir) = app("coverage").await;
        let checkout = dir.join("checkout");
        for file in ["src/api/a.rs", "src/api/b.rs", "src/web/c.ts", "lib/d.rs", "README.md"] {
            let path = checkout.join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(&path, "x\n").expect("write");
        }
        let body = json!({ "name": "Code", "path": checkout.display().to_string(), "exact": true });
        let (_, body) = call(&app, "POST", "/api/knowledge/projects", Some(body)).await;
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let api = json!({ "project": project, "path": "api", "title": "API", "owns": ["src/api"] });
        let (_, body) = call(&app, "POST", "/api/knowledge/domains", Some(api)).await;
        assert_eq!(body["domain"]["path"], "api", "{body}");
        let url = format!("/api/knowledge/domains/coverage?project={project}");

        // Outside Git: the walk counts the files, drift is said to need Git.
        let (status, body) = call(&app, "GET", &url, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["git"].as_bool(), body["domains"][0]["files"].as_u64()), (Some(false), Some(2)), "{body}");
        assert!(body["reason"].as_str().unwrap_or_default().contains("not a git checkout"), "{body}");
        assert_eq!(body["issue"], "not_a_repository", "the panel words it from the code: {body}");
        assert!(body["domains"][0].get("drift").is_none() && body["uncovered"].as_array().is_some_and(Vec::is_empty), "{body}");
        // A preview there says the same, as the error's issue.
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/preview/log?project={project}"), None).await;
        assert_eq!((status, body["error"].as_str(), body["issue"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("memory_git_unavailable"), Some("not_a_repository")), "{body}");

        if !git(&checkout, &["init", "-q"]) {
            eprintln!("git is not available; the rest is skipped");
            return;
        }
        assert!(git(&checkout, &["add", "-A"]) && git(&checkout, &["commit", "-q", "-m", "init", "--date", "2001-01-01T00:00:00"]));
        let (_, body) = call(&app, "GET", &url, None).await;
        assert_eq!((body["git"].as_bool(), body["domains"][0]["drift"].as_u64()), (Some(true), Some(0)), "{body}");
        assert!(body.get("issue").is_none() && body.get("reason").is_none(), "{body}");
        let uncovered: Vec<(&str, u64)> = body["uncovered"]
            .as_array()
            .expect("uncovered")
            .iter()
            .map(|d| (d["path"].as_str().unwrap_or_default(), d["files"].as_u64().unwrap_or_default()))
            .collect();
        assert_eq!(uncovered, [("lib", 1), ("src/web", 1)], "a root file is no directory to own");

        // A commit after the hub was verified touching owned code is drift.
        std::fs::write(checkout.join("src/api/a.rs"), "y\n").expect("write");
        assert!(git(&checkout, &["commit", "-q", "-am", "api: change a", "--date", "2099-01-01T00:00:00"]));
        let (_, body) = call(&app, "GET", &url, None).await;
        assert_eq!((body["domains"][0]["drift"].as_u64(), body["domains"][0]["latest"]["subject"].as_str()), (Some(1), Some("api: change a")), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn knowledge_api_answers_503_when_withdrawn() {
        let state = Arc::new(MemoryState::new());
        let app = mount(state);
        let (status, _) = call(&app, "GET", "/api/knowledge/projects", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}

#[cfg(test)]
mod sync_routes_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    async fn sync_app(label: &str) -> (Router, Arc<MemoryState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "swiss-memapi-sync-{label}-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let dir = dir.canonicalize().expect("canonical");
        let state = Arc::new(MemoryState::new());
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        service.set_lister(Some(Arc::new(crate::memory_preview::CheckoutLister)));
        state.install(Arc::new(service));
        (mount(state.clone()), state, dir)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (axum::http::StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn a_run_cancels_queued_at_once_and_running_by_its_stop() {
        let (app, state, dir) = sync_app("cancel").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Cancelled", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let cancel = |id: &str| format!("/api/knowledge/sync/runs/{id}/cancel?project={project}");

        // Queued: cancelled at once, and it never starts.
        let (_, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        let queued = body["run"]["id"].as_str().expect("run id").to_string();
        let (status, body) = call(&app, "POST", &cancel(&queued), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!((body["run"]["state"].as_str(), body["stopping"].as_bool()), (Some("cancelled"), Some(false)), "{body}");
        let (status, body) = call(&app, "POST", &cancel(&queued), None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("already ended (cancelled)"), "{body}");
        let service = state.current().expect("service");
        assert_eq!(service.sync_next(i64::MAX).await.expect("next"), swiss_memory::sync::Next::Idle, "the queue let it go");

        // Running: its stop is raised; the run itself ends cancelled when it sees it.
        let (_, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        let running = body["run"]["id"].as_str().expect("run id").to_string();
        assert!(matches!(service.sync_next(i64::MAX).await.expect("next"), swiss_memory::sync::Next::Run(_)));
        let stop = state.sync_stop(&running);
        let (status, body) = call(&app, "POST", &cancel(&running), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!((body["run"]["state"].as_str(), body["stopping"].as_bool()), (Some("running"), Some(true)), "{body}");
        assert!(stop.raised(), "the worker's seat sees the raise");
        let (_, body) = call(&app, "GET", &format!("/api/knowledge/sync?project={project}"), None).await;
        assert_eq!(body["stopping"], json!([running]), "the overview says it is stopping: {body}");
        state.sync_stop_done(&running);
        assert!(!state.sync_stop(&running).raised(), "a landed run's stop leaves the seat");

        let (status, _) = call(&app, "POST", &cancel("r-00000000"), None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

        // By its id alone (the LLM page's Stop): found without the project; another project's
        // selector still misses it.
        let (_, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        let bare = body["run"]["id"].as_str().expect("run id").to_string();
        let (status, _) = call(&app, "POST", &format!("/api/knowledge/sync/runs/{bare}/cancel?project=p-00000000"), None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "another project does not reach it");
        let (status, body) = call(&app, "POST", &format!("/api/knowledge/sync/runs/{bare}/cancel"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["run"]["state"].as_str(), Some("cancelled"), "{body}");
        let (status, _) = call(&app, "POST", "/api/knowledge/sync/runs/r-00000000/cancel", None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_sync_routes_queue_and_read_runs() {
        let (app, _state, dir) = sync_app("queue").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Synced", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["run"]["state"], json!("queued"));
        assert_eq!(body["merged"], json!(false));
        let id = body["run"]["id"].as_str().expect("run id").to_string();
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        assert_eq!(
            (status, body["merged"].as_bool()),
            (axum::http::StatusCode::ACCEPTED, Some(true)),
            "one queued run per project",
        );
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/sync?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["state"]["head"], Value::Null);
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/sync/runs/{id}?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/sync/runs/nope?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
        let (status, _) = call(&app, "GET", "/api/knowledge/sync?project=global", None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "the global scope cannot sync");
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/sync",
            Some(json!({ "project": "global", "dryRun": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");

        // A map run queues beside the sync run, never merged into it.
        let map = json!({ "project": project, "map": true, "language": "zh" });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(map.clone())).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!((body["run"]["trigger"].as_str(), body["merged"].as_bool()), (Some("map"), Some(false)));
        assert_eq!(body["run"]["scope"]["language"], json!("zh"));
        let (_, body) = call(&app, "POST", "/api/knowledge/sync", Some(map)).await;
        assert_eq!(body["merged"], json!(true), "one queued map run per project");
        let refused = [
            json!({ "project": project, "language": "zh" }),
            json!({ "project": project, "map": true, "since": "HEAD~3" }),
            json!({ "project": project, "map": true, "language": "zh CN; drop" }),
        ];
        for request in refused {
            let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(request.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{request}: {body}");
        }

        // The plan a map run would draw, asking no model.
        for (area, files) in [("billing", ["invoice", "refund", "tax"]), ("orders", ["cart", "checkout", "order"])] {
            std::fs::create_dir_all(dir.join("src").join(area)).expect("dir");
            for file in files {
                std::fs::write(dir.join("src").join(area).join(format!("{file}.rs")), "pub fn f() {}").expect("source");
            }
        }
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/sync/map?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!((body["asks"].as_u64(), body["codeFiles"].as_u64()), (Some(2), Some(6)), "{body}");
        assert_eq!(body["mappedFiles"].as_u64(), Some(0), "no domain owns code yet: {body}");
        assert_eq!(body["plan"][1]["owns"], json!(["src/orders"]), "{body}");
        let (status, _) = call(&app, "GET", "/api/knowledge/sync/map?project=global", None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    }

    /// A `recheck` request is a run kind of its own (SPEC §memory.init The recheck): it
    /// queues beside a waiting full run, a second request merges into it, and it takes
    /// neither a map's redraw nor a sync's since.
    #[tokio::test]
    async fn a_recheck_request_queues_a_recheck_beside_the_other_runs() {
        let (app, _state, dir) = sync_app("recheck").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Rechecked", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project, "full": true }))).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let full_id = body["run"]["id"].as_str().expect("full run id").to_string();

        let recheck = json!({ "project": project, "recheck": true, "language": "zh", "maxAsks": 5 });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(recheck)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["run"]["trigger"], json!("recheck"), "{body}");
        assert_eq!((body["run"]["scope"]["language"].as_str(), body["run"]["scope"]["maxAsks"].as_u64()), (Some("zh"), Some(5)));
        assert_eq!(body["merged"], json!(false), "a queue of its own, beside the full run");
        let recheck_id = body["run"]["id"].as_str().expect("recheck id").to_string();
        assert_ne!(recheck_id, full_id);
        let again = json!({ "project": project, "recheck": true, "maxAsks": 9 });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(again)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!((body["merged"].as_bool(), body["run"]["id"].as_str()), (Some(true), Some(recheck_id.as_str())), "{body}");
        assert_eq!(body["run"]["scope"]["maxAsks"], json!(9), "the newer cap: {body}");
        let refused = [
            json!({ "project": project, "recheck": true, "full": true }),
            json!({ "project": project, "recheck": true, "map": true }),
            json!({ "project": project, "recheck": true, "redraw": true }),
            json!({ "project": project, "recheck": true, "since": "HEAD~1" }),
            json!({ "project": project, "recheck": true, "maxAsks": 0 }),
            json!({ "project": project, "recheck": "yes" }),
        ];
        for request in refused {
            let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(request.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{request}: {body}");
        }

        // A review run (`review --resolve`): a queue of its own, beside the recheck.
        let review = json!({ "project": project, "review": true, "language": "zh", "maxAsks": 4 });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(review)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["run"]["trigger"], json!("review"), "{body}");
        assert_eq!((body["run"]["scope"]["language"].as_str(), body["run"]["scope"]["maxAsks"].as_u64()), (Some("zh"), Some(4)));
        assert_eq!(body["merged"], json!(false), "{body}");
        assert_ne!(body["run"]["id"].as_str(), Some(recheck_id.as_str()));
        let review_id = body["run"]["id"].as_str().unwrap_or_default().to_string();
        // `changed` narrows a review to the notes whose code changed: its own dry run says
        // so, and the queued whole review already holds those notes.
        let changed = json!({ "project": project, "review": true, "changed": true, "dryRun": true });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(changed)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!((body["merged"].clone(), body["run"]["scope"]["changed"].clone()), (json!(false), json!(true)), "{body}");
        let changed = json!({ "project": project, "review": true, "changed": true });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(changed)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["merged"], json!(true), "{body}");
        assert_eq!(body["run"]["id"].as_str(), Some(review_id.as_str()));
        assert!(body["run"]["scope"].get("changed").is_none(), "the whole review stays whole: {body}");
        for request in [
            json!({ "project": project, "recheck": true, "changed": true }),
            json!({ "project": project, "changed": true }),
            json!({ "project": project, "review": true, "changed": "yes" }),
            json!({ "project": project, "review": true, "recheck": true }),
            json!({ "project": project, "review": true, "docs": true }),
            json!({ "project": project, "review": true, "since": "HEAD~1" }),
            json!({ "project": project, "review": true, "redraw": true }),
            json!({ "project": project, "review": true, "lead": "a" }),
            json!({ "project": project, "review": true, "maxAsks": 0 }),
        ] {
            let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(request.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{request}: {body}");
        }
    }

    /// A project's settings name the documents it keeps; a `docs` request queues the run
    /// that reads and cards them, and the docs routes list, search and read what was kept
    /// (SPEC §memory.docs).
    #[tokio::test]
    async fn a_docs_request_queues_the_read_and_the_docs_routes_serve_it() {
        let (app, state, dir) = sync_app("docs").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Kept", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let skill = dir.join(".claude/skills/logic");
        std::fs::create_dir_all(&skill).expect("skill dir");
        std::fs::write(skill.join("stats-job.md"), "# Stats jobs\n## Inactive copy check\nPause a copy after 30 idle days.\n")
            .expect("doc");

        let settings = format!("/api/knowledge/projects/{project}/settings");
        let (status, body) = call(&app, "PATCH", &settings, Some(json!({ "docs": [".claude/skills/logic/"] }))).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["settings"]["docs"], json!([".claude/skills/logic"]), "{body}");
        for bad in [json!({ "docs": ["/etc"] }), json!({ "docs": ["../up"] }), json!({ "docs": "logic" })] {
            let (status, body) = call(&app, "PATCH", &settings, Some(bad.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        let (status, _) = call(&app, "PATCH", "/api/knowledge/projects/global/settings", Some(json!({ "docs": ["docs"] }))).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "the global scope keeps no documents");

        // The request queues a docs run with its cap; a second merges into it.
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project, "docs": true, "maxAsks": 3 }))).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!((body["run"]["trigger"].as_str(), body["run"]["scope"]["maxAsks"].as_u64()), (Some("docs"), Some(3)), "{body}");
        let run_id = body["run"]["id"].as_str().expect("run id").to_string();
        let (_, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project, "docs": true }))).await;
        assert_eq!((body["merged"].as_bool(), body["run"]["id"].as_str()), (Some(true), Some(run_id.as_str())), "{body}");
        for refused in [
            json!({ "project": project, "docs": true, "full": true }),
            json!({ "project": project, "docs": true, "recheck": true }),
            json!({ "project": project, "docs": true, "since": "HEAD~1" }),
            json!({ "project": project, "docs": true, "redraw": true }),
            json!({ "project": project, "docs": true, "lead": "a" }),
            json!({ "project": project, "docs": "yes" }),
        ] {
            let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(refused.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{refused}: {body}");
        }

        // What a run's read keeps, the routes serve.
        let service = state.current().expect("service");
        service.docs_read(&project, false).await.expect("read");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/docs?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["sources"], json!([".claude/skills/logic"]), "{body}");
        assert_eq!(body["docs"][0]["path"], json!(".claude/skills/logic/stats-job.md"), "{body}");
        assert_eq!(body["docs"][0]["title"], json!("Stats jobs"), "{body}");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/docs?project={project}&q=idle%20days&limit=3"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["hits"][0]["heading"], json!("Stats jobs › Inactive copy check"), "{body}");
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/docs?project={project}&q=idle&kind=roadmap"), None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/docs?project={project}&kind=design"), None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "kind filters a search, not the list");
        let (status, body) = call(&app, "GET", &format!("/api/knowledge/docs/read?project={project}&doc=stats-job.md&section=1"), None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert!(body["text"].as_str().unwrap_or_default().contains("30 idle days"), "{body}");
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/docs/read?project={project}"), None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/docs/read?project={project}&doc=nowhere.md"), None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    /// A `full` request is its own run kind (SPEC §memory.init): it queues a full run -
    /// neither a map run nor an incremental sync - beside whatever else waits. The queue
    /// boundary only: the receipt the CLI's `--wait` and the panel read, never the
    /// workflow behind it.
    #[tokio::test]
    async fn a_full_request_queues_a_full_run() {
        let (app, _state, dir) = sync_app("full").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Booted", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        // An incremental sync and a map run wait first: the full init queues beside both.
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project }))).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let sync_id = body["run"]["id"].as_str().expect("sync run id").to_string();
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(json!({ "project": project, "map": true }))).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        let map_id = body["run"]["id"].as_str().expect("map run id").to_string();

        let full = json!({ "project": project, "full": true, "language": "zh" });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(full)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["run"]["trigger"], json!("full"), "neither map nor manual: {body}");
        assert_eq!(body["run"]["scope"]["language"], json!("zh"), "{body}");
        assert_eq!(body["merged"], json!(false), "a run of its own, not merged into a waiting kind");
        let full_id = body["run"]["id"].as_str().expect("full run id").to_string();
        assert!(full_id != sync_id && full_id != map_id, "queued apart from the sync and map runs");
        let again = json!({ "project": project, "full": true, "redraw": true, "maxAsks": 12 });
        let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(again)).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        assert_eq!((body["merged"].as_bool(), body["run"]["id"].as_str()), (Some(true), Some(full_id.as_str())), "{body}");
        assert_eq!(body["run"]["scope"]["redraw"], json!(true), "{body}");
        assert_eq!(body["run"]["scope"]["maxAsks"], json!(12), "{body}");
        let refused = [
            json!({ "project": project, "map": true, "full": true }),
            json!({ "project": project, "full": true, "since": "HEAD~1" }),
            json!({ "project": project, "redraw": true }),
            json!({ "project": project, "map": true, "maxAsks": 3 }),
            json!({ "project": project, "full": true, "maxAsks": 0 }),
        ];
        for request in refused {
            let (status, body) = call(&app, "POST", "/api/knowledge/sync", Some(request.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{request}: {body}");
        }

        // The plan a full run would follow, asking no model: the map's, then the code cut
        // into slices - one per area here - the asks capped, and the index the notes land in.
        for (area, files) in [("billing", ["invoice", "refund", "tax"]), ("orders", ["cart", "checkout", "order"])] {
            std::fs::create_dir_all(dir.join("src").join(area)).expect("dir");
            for file in files {
                std::fs::write(dir.join("src").join(area).join(format!("{file}.rs")), "pub fn f() {}").expect("source");
            }
        }
        let uri = format!("/api/knowledge/sync/full?project={project}&maxAsks=1");
        let (status, body) = call(&app, "GET", &uri, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!((body["asks"].as_u64(), body["review"].as_bool()), (Some(2), Some(false)), "two areas: no review: {body}");
        let knowledge = &body["knowledge"];
        assert_eq!((knowledge["files"].as_u64(), knowledge["slices"].as_u64()), (Some(6), Some(2)), "{body}");
        assert_eq!((knowledge["asks"].as_u64(), knowledge["more"].as_u64()), (Some(1), Some(1)), "{body}");
        assert_eq!(body["embedding"]["state"], json!("off"), "no model: lexical search only: {body}");
        let (status, _) = call(&app, "GET", &format!("/api/knowledge/sync/full?project={project}&maxAsks=0"), None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    }

    /// A project's settings name its agents and lead; the overview says whom a run would
    /// ask and which layer decided; a run's own pick is checked as it is queued; a nudge
    /// asks only a named agent (SPEC §memory.settings).
    #[tokio::test]
    async fn the_project_settings_pick_whom_a_run_asks() {
        let (app, state, dir) = sync_app("settings").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Picked", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        let settings = format!("/api/knowledge/projects/{project}/settings");
        let overview = format!("/api/knowledge/sync?project={project}");

        let (status, body) = call(&app, "GET", &settings, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["settings"], json!({ "agents": null, "lead": null, "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
        let (_, body) = call(&app, "GET", &overview, None).await;
        assert_eq!(body["chosen"]["agents"], json!({ "names": [], "from": null }), "no llm agent at all: {body}");
        assert_eq!((body["agent"].clone(), body["workers"].clone()), (Value::Null, json!([])), "{body}");

        // Named nowhere, a run asks the agent added last; a lead never falls back.
        state.llm().apply(
            swiss_llm::config::LlmConfig::parse(&json!({
                "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] } },
                "agents": { "a": { "provider": "local" }, "b": { "provider": "local" } }
            }))
            .expect("row"),
        );
        let (_, body) = call(&app, "GET", &overview, None).await;
        assert_eq!(body["chosen"]["agents"], json!({ "names": ["b"], "from": "newest" }), "{body}");
        assert_eq!((body["agent"]["name"].as_str(), body["agent"]["model"].as_str()), (Some("b"), Some("tiny")), "{body}");
        assert_eq!((body["lead"].clone(), body["chosen"]["lead"]["from"].clone()), (Value::Null, Value::Null), "{body}");

        // The sync block names one; the project's settings, over it, name two and a lead.
        let block = SyncConfig::parse(&json!({ "agent": "a", "transcripts": "off" })).expect("block");
        state.set_sync(Some(block), None);
        let (_, body) = call(&app, "GET", &overview, None).await;
        assert_eq!(body["chosen"]["agents"], json!({ "names": ["a"], "from": "plugin" }), "{body}");
        let (status, body) = call(&app, "PATCH", &settings, Some(json!({ "agents": ["a", "b"], "lead": "b", "embedding": false }))).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["settings"], json!({ "agents": ["a", "b"], "lead": "b", "embedding": false, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
        let (_, body) = call(&app, "GET", &overview, None).await;
        assert_eq!(body["chosen"]["agents"], json!({ "names": ["a", "b"], "from": "project" }), "{body}");
        assert_eq!(body["workers"].as_array().map(Vec::len), Some(2), "{body}");
        assert_eq!((body["agent"]["name"].as_str(), body["lead"]["name"].as_str()), (Some("a"), Some("b")), "{body}");
        assert_eq!(body["settings"], json!({ "agents": ["a", "b"], "lead": "b", "embedding": false, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
        // A merge patch: null inherits again, a field left out is kept.
        let (_, body) = call(&app, "PATCH", &settings, Some(json!({ "lead": null, "embedding": null }))).await;
        assert_eq!(body["settings"], json!({ "agents": ["a", "b"], "lead": null, "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
        for (bad, says) in [
            (json!({ "bogus": 1 }), "bogus"),
            (json!({ "agents": [] }), "names no agent"),
            (json!({ "lead": ["a", "b"] }), "lead must be"),
            (json!({ "agents": "Not A Name" }), "names llm agents"),
            (json!({ "embedding": "yes" }), "true, false or null"),
        ] {
            let (status, body) = call(&app, "PATCH", &settings, Some(bad.clone())).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{bad}: {body}");
            assert!(body.to_string().contains(says), "{bad}: {body}");
        }
        let (status, _) = call(&app, "PATCH", "/api/knowledge/projects/global/settings", Some(json!({ "agents": "a" }))).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "the global scope names no agents");
        let (status, body) = call(&app, "PATCH", "/api/knowledge/projects/global/settings", Some(json!({ "embedding": false }))).await;
        assert_eq!(status, axum::http::StatusCode::OK, "the global scope may set embedding: {body}");
        assert_eq!(body["settings"]["embedding"], json!(false), "{body}");
        let (status, _) = call(&app, "GET", "/api/knowledge/projects/p-nope/settings", None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

        // A run's own pick rides on the queued run; a lead only with map or full.
        let queue = |body: Value| {
            let app = app.clone();
            async move { call(&app, "POST", "/api/knowledge/sync", Some(body)).await }
        };
        let (status, body) = queue(json!({ "project": project, "recheck": true, "lead": "b" })).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("lead goes with map or full"), "{body}");
        let (status, body) = queue(json!({ "project": project, "map": true, "agents": ["a", "a"] })).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("twice"), "{body}");
        let (status, body) = queue(json!({ "project": project, "map": true, "dryRun": true, "agents": "off", "lead": "a" })).await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        // The record keeps a choice as null, "off" or the list of names.
        assert_eq!((body["run"]["scope"]["agents"].clone(), body["run"]["scope"]["lead"].clone()), (json!("off"), json!(["a"])), "{body}");

        // A nudge asks only a named agent: the newest is no fallback for it.
        let cwd = json!({ "cwd": swiss_memory::scope::display(&dir) });
        let (_, body) = call(&app, "PATCH", &settings, Some(json!({ "agents": null }))).await;
        assert_eq!(body["settings"]["agents"], Value::Null, "{body}");
        state.set_sync(Some(SyncConfig::parse(&json!({ "transcripts": "off" })).expect("block")), None);
        let (status, body) = call(&app, "POST", "/api/knowledge/sync/nudge", Some(cwd.clone())).await;
        assert_eq!((status, body["reason"].as_str()), (axum::http::StatusCode::OK, Some("no-agent")), "{body}");
        let (_, _) = call(&app, "PATCH", &settings, Some(json!({ "agents": "a" }))).await;
        let (_, body) = call(&app, "POST", "/api/knowledge/sync/nudge", Some(cwd)).await;
        assert_eq!(body["queued"], json!(true), "{body}");
    }

    /// An embedder that answers anything: the full plan's index scope, asked only for
    /// its shape here, never called.
    struct Stuffed;

    #[async_trait::async_trait]
    impl swiss_memory::Embedder for Stuffed {
        fn id(&self) -> &str {
            "fake:plan:8"
        }

        async fn embed(&self, texts: &[String], _purpose: swiss_memory::Purpose) -> swiss_memory::MemResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    /// The full plan says the semantic index the notes would land in; a project whose
    /// settings turn embedding off lands none (§memory.embedding), whatever the index.
    #[tokio::test]
    async fn the_full_plan_says_when_a_projects_embedding_is_off() {
        let (app, state, dir) = sync_app("embed-off").await;
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Unembedded", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let project = body["project"]["id"].as_str().expect("id").to_string();
        state.current().expect("service").set_embedder(Some(std::sync::Arc::new(Stuffed)));
        let plan = format!("/api/knowledge/sync/full?project={project}");
        let (_, body) = call(&app, "GET", &plan, None).await;
        assert_eq!(body["embedding"]["state"], json!("ready"), "an embedder is set: {body}");
        let settings = format!("/api/knowledge/projects/{project}/settings");
        let (_, body) = call(&app, "PATCH", &settings, Some(json!({ "embedding": false }))).await;
        assert_eq!(body["settings"]["embedding"], json!(false), "{body}");
        let (_, body) = call(&app, "GET", &plan, None).await;
        assert_eq!((body["embedding"]["state"].as_str(), body["embedding"]["model"].is_null()), (Some("off"), true), "{body}");
    }

    #[tokio::test]
    async fn a_nudge_guards_itself() {
        let (app, state, dir) = sync_app("nudge").await;
        // Without a sync block there is nothing to nudge.
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/sync/nudge",
            Some(json!({ "cwd": swiss_memory::scope::display(&dir) })),
        )
        .await;
        assert_eq!((status, body["reason"].as_str()), (axum::http::StatusCode::OK, Some("no-sync-block")));
        // With a block, an unregistered directory stays quiet.
        let config = SyncConfig::parse(&json!({ "agent": "x", "transcripts": "off" })).expect("config");
        state.set_sync(Some(config.clone()), None);
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/sync/nudge",
            Some(json!({ "cwd": swiss_memory::scope::display(&std::env::temp_dir()) })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["reason"], json!("not-a-project"), "{body}");
        // A registered project queues, due no sooner than the delay allows.
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/projects",
            Some(json!({ "name": "Nudged", "path": swiss_memory::scope::display(&dir), "exact": true })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let (status, body) = call(
            &app,
            "POST",
            "/api/knowledge/sync/nudge",
            Some(json!({ "cwd": swiss_memory::scope::display(&dir) })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["queued"], json!(true), "{body}");
        let due = body["dueMs"].as_i64().expect("dueMs");
        assert!(due >= unix_now() + 25_000, "the delay is kept: {due}");
        // auto off: the nudge is heard and declined.
        let mut off = config.clone();
        off.auto = false;
        state.set_sync(Some(off), None);
        let (_status, body) = call(
            &app,
            "POST",
            "/api/knowledge/sync/nudge",
            Some(json!({ "cwd": swiss_memory::scope::display(&dir) })),
        )
        .await;
        assert_eq!(body["reason"], json!("auto-off"), "{body}");
    }

    /// POST /api/knowledge/ask refuses a malformed ask before any agent is chosen, says
    /// a missing project is 404, and says no agent to answer is
    /// memory_agent_unavailable's 503 (SPEC §memory.rag).
    #[tokio::test]
    async fn the_ask_route_refuses_a_malformed_ask_and_says_no_agent_is_unavailable() {
        let (app, _state, dir) = sync_app("ask").await;
        let cwd = swiss_memory::scope::display(&dir);
        let ask = |body: Value| {
            let app = app.clone();
            async move { call(&app, "POST", "/api/knowledge/ask", Some(body)).await }
        };
        let long = "x".repeat(crate::memory_ask::QUESTION_MAX + 1);
        for (body, why) in [
            (json!({ "project": "global", "cwd": cwd, "question": "why?" }), "both scopes"),
            (json!({ "question": "why?" }), "no scope"),
            (json!({ "cwd": cwd, "question": "  " }), "an empty question"),
            (json!({ "cwd": cwd }), "no question"),
            (json!({ "cwd": cwd, "question": long }), "a question past the bound"),
            (json!({ "cwd": cwd, "question": "why?", "limit": 3 }), "an unknown field"),
            (json!({ "cwd": cwd, "question": "why?", "agent": "Not A Name" }), "a bad agent name"),
            (json!({ "cwd": cwd, "question": "why?", "scope": "team" }), "an unknown scope"),
        ] {
            let (status, body) = ask(body).await;
            assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("memory_invalid")), "{why}: {body}");
        }
        let (status, body) = ask(json!({ "project": "p-nope", "question": "why?" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a malformed project id: {body}");
        let (status, body) = ask(json!({ "project": "p-00000000", "question": "why?" })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("memory_not_found")), "{body}");
        let (status, body) = ask(json!({ "cwd": cwd, "question": "why?" })).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("memory_agent_unavailable")), "{body}");
        assert!(body["message"].as_str().is_some_and(|m| m.contains("no llm agent to ask")), "the sentence says why: {body}");
    }
}
