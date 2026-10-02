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

//! The MongoDB flavour of /api/db (SPEC §data.mongo): documents, the query bar, the pipeline
//! builder, schema analysis, indexes, the command console, edits, import and export — all under
//! `/api/db/{name}/mongo/…`, all through the same catalog lease as the SQL and redis routes.
//!
//! The house rules of `dbbrowser_api` hold here unchanged: a caller's mistake is a 400 BEFORE
//! any lease is taken (every option is parsed first), a database's refusal is a 400 carrying
//! the server's own words, a lost edit race is a 409 naming the fields, and the lease lives as
//! long as the request — or, for an export, as long as its streaming body.
//!
//! Documents travel as canonical Extended JSON both ways (SPEC §data.mongo). Option bodies are
//! POSTed, because a filter is a JSON document, and a JSON document in a query string is a
//! percent-encoding exercise nobody should have to read in a log; the two downloads/listings that
//! a plain GET serves (collections, indexes, export) take their documents JSON-encoded.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Map, Value};

use swiss_core::log;
use swiss_host::dbbrowser::{csv_escape, BrowserFlavor, DumpPiece, EditError};
use swiss_host::mongobrowser::{
    assert_db_name, flatten_doc, infer_schema, mongo_aggregate_of, mongo_collection_op_of,
    mongo_count_of, mongo_edits_of, mongo_explain_of, mongo_find_of, mongo_import_of,
    mongo_index_op_of, mongo_ns_of, mongo_sample_of, schema_paths, MongoBrowser,
    MongoCollectionOp, MongoIndexOp, MONGO_EXPORT_CAP, MONGO_MAX_TIME_MAX_MS, MONGO_PAGE_MAX,
};
use swiss_host::services::catalog::{CatalogRegistry, ConnectionLease};

use crate::dbbrowser_api::{catalog_guard, lease_fail, reply, with_elapsed_ms, Fail, LeaseBody};

/// Documents the CSV export buffers to learn its columns when the caller names none: the
/// union of their flattened paths becomes the header. A field first seen after them has no
/// column — the panel passes the schema's paths as `fields` to cover a ragged collection.
const CSV_HEAD_SAMPLE: usize = 1_000;
/// Bytes an export body gathers before it hands the chunk on.
const EXPORT_CHUNK: usize = 64 * 1024;

// --- the lease -----------------------------------------------------------------------------------

fn not_mongo(name: &str, dialect: &str) -> Fail {
    Fail {
        status: StatusCode::NOT_FOUND,
        message: format!("MCP '{name}' ({dialect}) is not a MongoDB connection"),
        conflict: None,
    }
}

/// A rented connection and its document browser; the lease lives as long as the pair.
pub(crate) type MongoLease = (ConnectionLease, Arc<dyn MongoBrowser>);

/// Lease one connection as "data" and pull its document browser — the twin of `lease_db`.
pub(crate) fn lease_mongo(catalog: &CatalogRegistry, name: &str) -> Result<MongoLease, Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    let browser = match lease.flavor() {
        BrowserFlavor::Mongo(mb) => mb.clone(),
        _ => return Err(not_mongo(name, lease.dialect())),
    };
    Ok((lease, browser))
}

/// The lease when the connection IS a mongo one, `None` when it is another flavour — for the
/// shared routes (activity) that serve every flavour in one shape.
pub(crate) fn lease_mongo_if(catalog: &CatalogRegistry, name: &str) -> Result<Option<MongoLease>, Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    let browser = match lease.flavor() {
        BrowserFlavor::Mongo(mb) => mb.clone(),
        _ => return Ok(None),
    };
    Ok(Some((lease, browser)))
}

/// POST /activity-kill on a mongo connection: `killOp` of one opid. `mode` is accepted and
/// ignored — MongoDB has one way to end an operation, and the panel posts the same body to
/// every flavour.
pub(crate) async fn activity_kill(name: &str, mb: &Arc<dyn MongoBrowser>, body: &Value) -> Result<Value, Fail> {
    let opid = body
        .get("pid")
        .and_then(Value::as_i64)
        .filter(|p| *p >= 0)
        .ok_or_else(|| Fail::bad("pid must be an operation id (a non-negative integer)"))?;
    log::log("warn", "data view activity kill", Some(json!({ "name": name, "opid": opid })));
    mb.activity_kill(opid).await.map_err(Fail::bad)
}

// --- query-string documents ------------------------------------------------------------------------

/// A JSON-encoded option from a query string — `filter={"a":1}` — as the value the option
/// parsers read. Absent and empty are "not sent"; text that does not parse is the caller's 400.
fn q_json(q: &HashMap<String, String>, key: &str) -> Result<Option<Value>, Fail> {
    match q.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(raw) => serde_json::from_str(raw)
            .map(Some)
            .map_err(|_| Fail::bad(format!("{key} must be JSON"))),
    }
}

fn q_opts(q: &HashMap<String, String>, json_keys: &[&str], text_keys: &[&str]) -> Result<Value, Fail> {
    let mut o = Map::new();
    for k in text_keys {
        if let Some(v) = q.get(*k) {
            o.insert((*k).into(), json!(v));
        }
    }
    for k in json_keys {
        if let Some(v) = q_json(q, k)? {
            o.insert((*k).into(), v);
        }
    }
    Ok(Value::Object(o))
}

// --- handler bodies --------------------------------------------------------------------------------

async fn info(catalog: &CatalogRegistry, name: &str) -> Result<Value, Fail> {
    let (_lease, b) = lease_mongo(catalog, name)?;
    b.server_info().await.map_err(Fail::bad)
}

async fn collections(catalog: &CatalogRegistry, name: &str, q: &HashMap<String, String>) -> Result<Value, Fail> {
    let db = q.get("db").map(|s| s.trim().to_string()).unwrap_or_default();
    assert_db_name(&db).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    b.list_collections(&db).await.map_err(Fail::bad)
}

/// One documents page. The elapsed clock wraps the browser call — what the user waited on.
async fn find(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let q = mongo_find_of(body, MONGO_PAGE_MAX).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    let started = std::time::Instant::now();
    let out = b.find(&q).await.map_err(Fail::bad)?;
    Ok(with_elapsed_ms(out, started))
}

async fn count(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let q = mongo_count_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    b.count(&q).await.map_err(Fail::bad)
}

async fn aggregate(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let q = mongo_aggregate_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    if swiss_host::mongobrowser::pipeline_writes(&q.pipeline) {
        log::log(
            "info",
            "data view aggregate write",
            Some(json!({ "name": name, "db": q.ns.db, "collection": q.ns.coll, "stages": q.pipeline.len() })),
        );
    }
    let started = std::time::Instant::now();
    let out = b.aggregate(&q).await.map_err(Fail::bad)?;
    Ok(with_elapsed_ms(out, started))
}

async fn explain(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let q = mongo_explain_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    b.explain(&q).await.map_err(Fail::bad)
}

/// SPEC §data.mongo-schema: sample, then infer here — the MCP tool and the panel run the same
/// inference over the same `$sample`. `paths` is the query bar's completion list.
async fn schema(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (ns, filter, size) = mongo_sample_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    let started = std::time::Instant::now();
    let docs = b.sample(&ns, &filter, size).await.map_err(Fail::bad)?;
    let mut out = infer_schema(&docs);
    out["paths"] = json!(schema_paths(&out));
    out["requested"] = json!(size);
    Ok(with_elapsed_ms(out, started))
}

async fn indexes(catalog: &CatalogRegistry, name: &str, q: &HashMap<String, String>) -> Result<Value, Fail> {
    let ns = mongo_ns_of(&q_opts(q, &[], &["db", "collection"])?).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    b.indexes(&ns).await.map_err(Fail::bad)
}

async fn index(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let ns = mongo_ns_of(body).map_err(Fail::bad)?;
    let op = mongo_index_op_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    let what = match &op {
        MongoIndexOp::Create { .. } => "create",
        MongoIndexOp::Drop { .. } => "drop",
        MongoIndexOp::Hide { hidden: true, .. } => "hide",
        MongoIndexOp::Hide { hidden: false, .. } => "unhide",
    };
    log::log(
        "info",
        "data view index",
        Some(json!({ "name": name, "db": ns.db, "collection": ns.coll, "op": what })),
    );
    b.index_op(&ns, &op).await.map_err(Fail::bad)
}

/// SPEC §data.mongo-edits: a 400 for a refused request, a 409 with the moved fields and the
/// document's `_id` when another writer got there first.
async fn edits(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let ns = mongo_ns_of(body).map_err(Fail::bad)?;
    let list = mongo_edits_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    log::log(
        "info",
        "data view commit",
        Some(json!({ "name": name, "db": ns.db, "collection": ns.coll, "edits": list.len() })),
    );
    b.apply_edits(&ns, &list).await.map_err(|e| match e {
        EditError::Bad(m) => Fail::bad(m),
        EditError::Conflict(c) => Fail::conflict(c.message, c.columns, c.row.unwrap_or_default()),
    })
}

/// The console (SPEC §data.mongo): one command document under the engine's guard.
async fn command(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let db = body.get("db").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    assert_db_name(&db).map_err(Fail::bad)?;
    let cmd = match body.get("command") {
        Some(Value::Object(m)) if !m.is_empty() => m.clone(),
        _ => return Err(Fail::bad("command must be a command document, e.g. {\"ping\": 1}")),
    };
    let (_lease, b) = lease_mongo(catalog, name)?;
    let started = std::time::Instant::now();
    let out = b.run_command(&db, &cmd).await.map_err(Fail::bad)?;
    Ok(with_elapsed_ms(out, started))
}

async fn collection(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let op = mongo_collection_op_of(body).map_err(Fail::bad)?;
    let db = body.get("db").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let (_lease, b) = lease_mongo(catalog, name)?;
    let (what, level) = match &op {
        MongoCollectionOp::Create { .. } => ("create", "info"),
        MongoCollectionOp::CreateView { .. } => ("createView", "info"),
        MongoCollectionOp::Rename { .. } => ("rename", "info"),
        MongoCollectionOp::Drop { .. } => ("drop", "warn"),
    };
    log::log(
        level,
        "data view collection",
        Some(json!({ "name": name, "db": db, "collection": body.get("name"), "op": what })),
    );
    b.collection_op(&db, &op).await.map_err(Fail::bad)
}

/// SPEC §data.mongo import: one chunk of documents (the panel splits a file under the body
/// limit), inserted unordered, each failure itemised by its index in the chunk.
async fn import(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let q = mongo_import_of(body).map_err(Fail::bad)?;
    let (_lease, b) = lease_mongo(catalog, name)?;
    log::log(
        "info",
        "data view import",
        Some(json!({ "name": name, "db": q.ns.db, "collection": q.ns.coll, "docs": q.docs.len() })),
    );
    b.import(&q).await.map_err(Fail::bad)
}

// --- export ----------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportFormat {
    /// One JSON array — what Compass writes and what an import reads back.
    Json,
    /// One document per line — mongoexport's default, streamable into anything.
    Ndjson,
    /// Flattened paths as columns (Compass's CSV).
    Csv,
}

impl ExportFormat {
    fn of(s: Option<&str>) -> Result<Self, Fail> {
        match s.unwrap_or("json") {
            "" | "json" => Ok(Self::Json),
            "ndjson" => Ok(Self::Ndjson),
            "csv" => Ok(Self::Csv),
            other => Err(Fail::bad(format!("format must be json, ndjson or csv — got \"{other}\""))),
        }
    }
    fn word(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Csv => "csv",
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json; charset=utf-8",
            Self::Ndjson => "application/x-ndjson; charset=utf-8",
            Self::Csv => "text/csv; charset=utf-8",
        }
    }
}

/// A chunked byte sink in front of the body channel: documents are small, sending each one as
/// its own body chunk would cost a frame apiece.
struct Chunks {
    tx: tokio::sync::mpsc::Sender<DumpPiece>,
    buf: Vec<u8>,
}

impl Chunks {
    /// False once the client has gone: the caller stops producing.
    async fn push(&mut self, bytes: &[u8]) -> bool {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= EXPORT_CHUNK {
            return self.flush().await;
        }
        true
    }
    async fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        let chunk = std::mem::take(&mut self.buf);
        self.tx.send(Ok(chunk)).await.is_ok()
    }
    async fn fail(&mut self, message: String) {
        let _ = self.flush().await;
        let _ = self.tx.send(Err(message)).await;
    }
}

fn csv_line(cells: &[String]) -> String {
    let mut line = cells.iter().map(|c| csv_escape(Some(&json!(c)))).collect::<Vec<_>>().join(",");
    line.push_str("\r\n");
    line
}

/// Turn the browser's document stream into the download's bytes. Runs beside the response,
/// so the handler can answer the headers first; a failure mid-stream aborts the body, the
/// honest end of a download whose headers already went out.
async fn write_export(
    mut docs: tokio::sync::mpsc::Receiver<Result<Value, String>>,
    tx: tokio::sync::mpsc::Sender<DumpPiece>,
    format: ExportFormat,
    fields: Option<Vec<String>>,
) {
    let mut out = Chunks { tx, buf: Vec::with_capacity(EXPORT_CHUNK) };
    match format {
        ExportFormat::Json | ExportFormat::Ndjson => {
            let json = format == ExportFormat::Json;
            if json && !out.push(b"[").await {
                return;
            }
            let mut first = true;
            while let Some(item) = docs.recv().await {
                let doc = match item {
                    Ok(d) => d,
                    Err(e) => return out.fail(e).await,
                };
                let mut text = String::new();
                if json {
                    text.push_str(if first { "\n" } else { ",\n" });
                }
                text.push_str(&doc.to_string());
                if !json {
                    text.push('\n');
                }
                first = false;
                if !out.push(text.as_bytes()).await {
                    return;
                }
            }
            if json && !out.push(if first { b"]\n" as &[u8] } else { b"\n]\n" }).await {
                return;
            }
        }
        ExportFormat::Csv => {
            let mut head: Vec<Value> = Vec::new();
            let columns = match fields {
                Some(f) => f,
                None => {
                    // Learn the columns from the first documents, then replay them.
                    let mut cols: Vec<String> = Vec::new();
                    while head.len() < CSV_HEAD_SAMPLE {
                        match docs.recv().await {
                            Some(Ok(d)) => {
                                if let Value::Object(m) = &d {
                                    for (p, _) in flatten_doc(m) {
                                        if !cols.contains(&p) {
                                            cols.push(p);
                                        }
                                    }
                                }
                                head.push(d);
                            }
                            Some(Err(e)) => return out.fail(e).await,
                            None => break,
                        }
                    }
                    cols
                }
            };
            if !out.push(csv_line(&columns).as_bytes()).await {
                return;
            }
            let row_of = |d: &Value| -> String {
                let cells: HashMap<String, String> = match d {
                    Value::Object(m) => flatten_doc(m).into_iter().collect(),
                    _ => HashMap::new(),
                };
                csv_line(&columns.iter().map(|c| cells.get(c).cloned().unwrap_or_default()).collect::<Vec<_>>())
            };
            for d in &head {
                if !out.push(row_of(d).as_bytes()).await {
                    return;
                }
            }
            while let Some(item) = docs.recv().await {
                let d = match item {
                    Ok(d) => d,
                    Err(e) => return out.fail(e).await,
                };
                if !out.push(row_of(&d).as_bytes()).await {
                    return;
                }
            }
        }
    }
    let _ = out.flush().await;
}

/// GET /mongo/export: the documents a find describes — the documents view's filter, sort and
/// projection — streamed as JSON, NDJSON or CSV, `ejson=canonical` for a lossless dump (CSV is
/// always plain text). Without a limit the export runs to MONGO_EXPORT_CAP, under the longest
/// server-side time limit a browse request may ask for.
async fn export(catalog: &CatalogRegistry, name: &str, q: &HashMap<String, String>) -> Result<Response, Fail> {
    let mut o = q_opts(q, &["filter", "projection", "sort", "collation", "hint"], &["db", "collection", "skip"])?;
    if let Value::Object(m) = &mut o {
        m.insert("limit".into(), match q.get("limit").filter(|s| !s.is_empty()) {
            Some(l) => json!(l),
            None => json!(MONGO_EXPORT_CAP),
        });
        m.insert("maxTimeMS".into(), json!(MONGO_MAX_TIME_MAX_MS));
    }
    let find = mongo_find_of(&o, MONGO_EXPORT_CAP).map_err(Fail::bad)?;
    let format = ExportFormat::of(q.get("format").map(String::as_str))?;
    let relaxed = match q.get("ejson").map(String::as_str) {
        None | Some("") | Some("relaxed") => true,
        Some("canonical") => format == ExportFormat::Csv,
        Some(other) => return Err(Fail::bad(format!("ejson must be relaxed or canonical — got \"{other}\""))),
    };
    let fields: Option<Vec<String>> = match q_json(q, "fields")? {
        None => None,
        Some(Value::Array(a)) if a.iter().all(Value::is_string) && !a.is_empty() => {
            Some(a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        }
        Some(_) => return Err(Fail::bad("fields must be a non-empty JSON array of field paths")),
    };
    let (lease, b) = lease_mongo(catalog, name)?;
    let dump = b.export(&find, relaxed).await.map_err(Fail::bad)?;
    let (tx, rx) = tokio::sync::mpsc::channel::<DumpPiece>(8);
    tokio::spawn(write_export(dump.docs, tx, format, fields));
    let ext = match format {
        ExportFormat::Json => "json",
        ExportFormat::Ndjson => "ndjson",
        ExportFormat::Csv => "csv",
    };
    let file = format!("{}.{}.{ext}", find.ns.db, find.ns.coll);
    let body = LeaseBody { rx, lease: Some(lease) };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}\"")),
            (header::HeaderName::from_static("x-export-rows"), dump.rows.to_string()),
            (
                header::HeaderName::from_static("x-export-capped"),
                if dump.capped { "1".into() } else { "0".into() },
            ),
            (header::HeaderName::from_static("x-export-format"), format.word().to_string()),
        ],
        axum::body::Body::from_stream(body),
    )
        .into_response())
}

// --- the axum handlers -----------------------------------------------------------------------------

type Catalog = Extension<Arc<CatalogRegistry>>;
type Body = swiss_host::reply::NodeBody;

async fn info_route(Extension(catalog): Catalog, Path(name): Path<String>) -> Response {
    reply(info(&catalog, &name).await)
}

async fn collections_route(Extension(catalog): Catalog, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(collections(&catalog, &name, &q).await)
}

async fn find_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(find(&catalog, &name, &body.0).await)
}

async fn count_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(count(&catalog, &name, &body.0).await)
}

async fn aggregate_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(aggregate(&catalog, &name, &body.0).await)
}

async fn explain_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(explain(&catalog, &name, &body.0).await)
}

async fn schema_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(schema(&catalog, &name, &body.0).await)
}

async fn indexes_route(Extension(catalog): Catalog, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    reply(indexes(&catalog, &name, &q).await)
}

async fn index_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(index(&catalog, &name, &body.0).await)
}

async fn edits_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(edits(&catalog, &name, &body.0).await)
}

async fn command_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(command(&catalog, &name, &body.0).await)
}

async fn collection_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(collection(&catalog, &name, &body.0).await)
}

async fn import_route(Extension(catalog): Catalog, Path(name): Path<String>, body: Body) -> Response {
    reply(import(&catalog, &name, &body.0).await)
}

async fn export_route(Extension(catalog): Catalog, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    match export(&catalog, &name, &q).await {
        Ok(response) => response,
        Err(f) => swiss_host::reply::admin_error(f.status, &f.message),
    }
}

/// The mongo routes, merged into `dbbrowser_router` — which layers the catalog over them.
pub(crate) fn mongo_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        // Version, topology, transactions, and whether this MCP allows destroying data.
        .route("/api/db/{name}/mongo/info", get(info_route))
        // The collections of one database, with storage statistics (GET ?db=).
        .route("/api/db/{name}/mongo/collections", get(collections_route))
        .route("/api/db/{name}/mongo/find", post(find_route))
        .route("/api/db/{name}/mongo/count", post(count_route))
        // SPEC §data.mongo-pipeline: run, preview (`stages: n`) and explain a pipeline.
        .route("/api/db/{name}/mongo/aggregate", post(aggregate_route))
        .route("/api/db/{name}/mongo/explain", post(explain_route))
        // SPEC §data.mongo-schema: sampled schema analysis.
        .route("/api/db/{name}/mongo/schema", post(schema_route))
        .route("/api/db/{name}/mongo/indexes", get(indexes_route))
        .route("/api/db/{name}/mongo/index", post(index_route))
        // SPEC §data.mongo-edits: the optimistic, transactional document commit.
        .route("/api/db/{name}/mongo/edits", post(edits_route))
        .route("/api/db/{name}/mongo/command", post(command_route))
        .route("/api/db/{name}/mongo/collection", post(collection_route))
        .route("/api/db/{name}/mongo/import", post(import_route))
        .route("/api/db/{name}/mongo/export", get(export_route))
}

#[cfg(test)]
mod tests {
    //! The mongo routes end to end over a stub browser: the payload contract, the 400-before-
    //! lease rule, the 409 body, and the export's three formats — no live server.
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body as HttpBody;
    use axum::http::{HeaderMap, Request};
    use std::sync::Mutex;
    use swiss_host::dbbrowser::EditConflict;
    use swiss_host::mongobrowser::{
        MongoAggregate, MongoCount, MongoDump, MongoEdit, MongoExplain, MongoFind, MongoImport, MongoNs,
    };
    use swiss_host::services::catalog::{ConnectionCatalog, ConnectionInfo, LeaseTracker};
    use tower::ServiceExt;

    /// What the stub saw — one line per call, so a test can assert both what was forwarded and
    /// that a refused request never reached the browser at all.
    #[derive(Default)]
    struct Seen {
        calls: Vec<String>,
        find: Option<MongoFind>,
        aggregate: Option<MongoAggregate>,
        edits: Option<(MongoNs, Vec<MongoEdit>)>,
        command: Option<(String, Map<String, Value>)>,
        import: Option<MongoImport>,
        kill: Option<i64>,
        export_relaxed: Option<bool>,
    }

    struct StubMongo {
        seen: Arc<Mutex<Seen>>,
        conflict: bool,
    }

    impl StubMongo {
        fn note(&self, call: &str) {
            self.seen.lock().unwrap().calls.push(call.to_string());
        }
    }

    fn docs() -> Vec<Value> {
        vec![
            json!({ "_id": { "$oid": "650000000000000000000001" }, "name": "Ada", "addr": { "city": "London" } }),
            json!({ "_id": { "$oid": "650000000000000000000002" }, "name": "Bob, \"B\"", "n": { "$numberLong": "7" } }),
        ]
    }

    #[async_trait]
    impl MongoBrowser for StubMongo {
        fn label(&self) -> String {
            "app @ localhost:27017".into()
        }
        async fn server_info(&self) -> Result<Value, String> {
            self.note("info");
            Ok(json!({ "version": "8.0.0", "topology": "replicaSet", "transactions": true }))
        }
        async fn list_databases(&self) -> Result<Value, String> {
            self.note("databases");
            Ok(json!({ "primary": "app", "current": "app", "databases": [{ "name": "app", "primary": true, "browsable": true, "system": false }] }))
        }
        async fn list_collections(&self, db: &str) -> Result<Value, String> {
            self.note(&format!("collections {db}"));
            Ok(json!({ "db": db, "collections": [{ "name": "users", "type": "collection" }], "statsOmitted": 0 }))
        }
        async fn find(&self, q: &MongoFind) -> Result<Value, String> {
            self.note("find");
            self.seen.lock().unwrap().find = Some(q.clone());
            Ok(json!({ "docs": docs(), "more": false }))
        }
        async fn count(&self, q: &MongoCount) -> Result<Value, String> {
            self.note(&format!("count exact={}", q.exact));
            Ok(json!({ "total": 2, "estimated": !q.exact }))
        }
        async fn aggregate(&self, q: &MongoAggregate) -> Result<Value, String> {
            self.note("aggregate");
            self.seen.lock().unwrap().aggregate = Some(q.clone());
            Ok(json!({ "docs": [], "more": false }))
        }
        async fn explain(&self, q: &MongoExplain) -> Result<Value, String> {
            self.note(&format!("explain {}", q.verbosity));
            Ok(json!({ "explain": {}, "summary": { "collscan": true } }))
        }
        async fn sample(&self, ns: &MongoNs, _filter: &Map<String, Value>, size: i64) -> Result<Vec<Value>, String> {
            self.note(&format!("sample {}.{} {size}", ns.db, ns.coll));
            Ok(docs())
        }
        async fn indexes(&self, ns: &MongoNs) -> Result<Value, String> {
            self.note(&format!("indexes {}.{}", ns.db, ns.coll));
            Ok(json!({ "indexes": [{ "name": "_id_", "key": { "_id": { "$numberInt": "1" } } }] }))
        }
        async fn index_op(&self, _ns: &MongoNs, op: &MongoIndexOp) -> Result<Value, String> {
            self.note(&format!("index {op:?}"));
            Ok(json!({ "ok": true }))
        }
        async fn apply_edits(&self, ns: &MongoNs, edits: &[MongoEdit]) -> Result<Value, EditError> {
            self.note("edits");
            self.seen.lock().unwrap().edits = Some((ns.clone(), edits.to_vec()));
            if self.conflict {
                return Err(EditError::Conflict(EditConflict {
                    message: "edit 1: the document was changed by another writer — fields: name".into(),
                    columns: vec!["name".into()],
                    row: Some(Map::from_iter([("_id".to_string(), json!({ "$numberInt": "1" }))])),
                }));
            }
            Ok(json!({ "ok": true, "applied": edits.len(), "transaction": true }))
        }
        async fn run_command(&self, db: &str, command: &Map<String, Value>) -> Result<Value, String> {
            self.note("command");
            self.seen.lock().unwrap().command = Some((db.to_string(), command.clone()));
            if command.contains_key("shutdown") {
                return Err("shutdown is rejected: it stops the MongoDB server.".into());
            }
            Ok(json!({ "reply": { "ok": { "$numberDouble": "1.0" } } }))
        }
        async fn collection_op(&self, db: &str, op: &MongoCollectionOp) -> Result<Value, String> {
            self.note(&format!("collection {db} {op:?}"));
            Ok(json!({ "ok": true }))
        }
        async fn export(&self, _q: &MongoFind, relaxed: bool) -> Result<MongoDump, String> {
            self.note("export");
            self.seen.lock().unwrap().export_relaxed = Some(relaxed);
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                for d in docs() {
                    if tx.send(Ok(d)).await.is_err() {
                        return;
                    }
                }
            });
            Ok(MongoDump { rows: 2, capped: false, docs: rx })
        }
        async fn import(&self, q: &MongoImport) -> Result<Value, String> {
            self.note("import");
            self.seen.lock().unwrap().import = Some(q.clone());
            Ok(json!({ "inserted": q.docs.len(), "errorCount": 0, "errors": [] }))
        }
        async fn activity(&self) -> Result<Value, String> {
            self.note("activity");
            Ok(json!({ "rows": [{ "pid": 42, "state": "active", "query": "{}", "own": false, "killable": true }] }))
        }
        async fn activity_kill(&self, opid: i64) -> Result<Value, String> {
            self.note("kill");
            self.seen.lock().unwrap().kill = Some(opid);
            Ok(json!({ "ok": true }))
        }
    }

    struct Provider {
        browser: Arc<dyn MongoBrowser>,
        tracker: Arc<LeaseTracker>,
    }

    impl ConnectionCatalog for Provider {
        fn list(&self) -> Vec<ConnectionInfo> {
            vec![ConnectionInfo {
                id: "m".into(),
                label: self.browser.label(),
                dialect: "mongo".into(),
                state: "running".into(),
            }]
        }
        fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, swiss_host::services::catalog::CatalogError> {
            if id != "m" {
                return Err(swiss_host::services::catalog::CatalogError::Unknown(format!("unknown MCP: {id}")));
            }
            Ok(self.tracker.grant(id, holder, "mongo", BrowserFlavor::Mongo(self.browser.clone())))
        }
    }

    fn app(conflict: bool) -> (Router<()>, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(
                Arc::new(Provider {
                    browser: Arc::new(StubMongo { seen: seen.clone(), conflict }),
                    tracker: LeaseTracker::new(),
                }),
                "mcp",
            )
            .expect("test provider registers");
        let router = crate::dbbrowser_api::dbbrowser_router(catalog, Arc::new(Vec::new), Arc::new(|_| "default".to_string()));
        (router, seen)
    }

    async fn call(router: Router<()>, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, HeaderMap, Option<Value>, String) {
        let builder = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => builder.header("content-type", "application/json").body(HttpBody::from(v.to_string())).expect("request"),
            None => builder.body(HttpBody::empty()).expect("request"),
        };
        let resp = router.oneshot(req).await.expect("infallible router");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (status, headers, serde_json::from_str(&text).ok(), text)
    }

    fn calls(seen: &Arc<Mutex<Seen>>) -> Vec<String> {
        seen.lock().unwrap().calls.clone()
    }

    #[tokio::test]
    async fn the_connection_lists_as_mongo_and_answers_the_shared_routes() {
        let (router, seen) = app(false);
        let (_, _, body, _) = call(router.clone(), "GET", "/api/db", None).await;
        assert_eq!(body.expect("json")["connections"][0]["dialect"], json!("mongo"));
        let (status, _, body, _) = call(router.clone(), "GET", "/api/db/m/databases", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["primary"], json!("app"));
        let (_, _, body, _) = call(router.clone(), "GET", "/api/db/m/mongo/info", None).await;
        assert_eq!(body.expect("json")["topology"], json!("replicaSet"));
        let (_, _, body, _) = call(router.clone(), "GET", "/api/db/m/activity", None).await;
        assert_eq!(body.expect("json")["rows"][0]["pid"], json!(42));
        let (status, _, _, _) = call(router, "POST", "/api/db/m/activity-kill", Some(json!({ "pid": 42, "mode": "cancel" }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen.lock().unwrap().kill, Some(42));
    }

    #[tokio::test]
    async fn a_find_forwards_its_parsed_options_and_reports_elapsed_time() {
        let (router, seen) = app(false);
        let (status, _, body, _) = call(
            router,
            "POST",
            "/api/db/m/mongo/find",
            Some(json!({ "db": "app", "collection": "users", "filter": { "age": { "$gt": 30 } }, "sort": { "age": -1 }, "skip": 40, "limit": 9999 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert!(body["elapsedMs"].is_u64());
        assert_eq!(body["docs"].as_array().map(Vec::len), Some(2));
        let q = seen.lock().unwrap().find.clone().expect("forwarded");
        assert_eq!(q.ns, MongoNs { db: "app".into(), coll: "users".into() });
        assert_eq!(q.skip, 40);
        // The page cap holds whatever the caller asks for.
        assert_eq!(q.limit, MONGO_PAGE_MAX);
        assert_eq!(q.filter.get("age"), Some(&json!({ "$gt": 30 })));
    }

    #[tokio::test]
    async fn a_malformed_request_is_a_400_before_any_lease() {
        let (router, seen) = app(false);
        for (uri, body) in [
            ("/api/db/m/mongo/find", json!({ "db": "a b", "collection": "users" })),
            ("/api/db/m/mongo/find", json!({ "db": "app", "collection": "users", "filter": [1] })),
            ("/api/db/m/mongo/aggregate", json!({ "db": "app", "collection": "c", "pipeline": [{ "$match": {}, "$sort": {} }] })),
            ("/api/db/m/mongo/explain", json!({ "db": "app", "collection": "c", "pipeline": [{ "$out": "x" }] })),
            ("/api/db/m/mongo/edits", json!({ "db": "app", "collection": "c", "edits": [{ "op": "delete", "original": { "a": 1 } }] })),
            ("/api/db/m/mongo/index", json!({ "db": "app", "collection": "c", "op": "drop", "name": "_id_" })),
            ("/api/db/m/mongo/command", json!({ "db": "app", "command": {} })),
            ("/api/db/m/mongo/collection", json!({ "db": "app", "op": "rename", "name": "a", "to": "a" })),
            ("/api/db/m/mongo/import", json!({ "db": "app", "collection": "c", "docs": [] })),
        ] {
            let (status, _, reply, _) = call(router.clone(), "POST", uri, Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {body}");
            assert!(reply.expect("json")["error"].is_string());
        }
        let (status, _, _, _) = call(router.clone(), "GET", "/api/db/m/mongo/collections", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _, _) = call(router, "GET", "/api/db/m/mongo/export?db=app&collection=c&filter=%7Bnot", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(calls(&seen).is_empty(), "nothing reached the browser: {:?}", calls(&seen));
    }

    #[tokio::test]
    async fn a_lost_edit_race_is_a_409_naming_the_fields_and_the_document() {
        let (router, seen) = app(true);
        let (status, _, body, _) = call(
            router,
            "POST",
            "/api/db/m/mongo/edits",
            Some(json!({
                "db": "app", "collection": "users",
                "edits": [{ "op": "replace", "original": { "_id": { "$numberInt": "1" }, "name": "a" }, "doc": { "name": "b" } }],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let body = body.expect("json");
        assert_eq!(body["conflictColumns"], json!(["name"]));
        assert_eq!(body["row"], json!({ "_id": { "$numberInt": "1" } }));
        let (ns, edits) = seen.lock().unwrap().edits.clone().expect("forwarded");
        assert_eq!(ns.coll, "users");
        assert_eq!(edits[0].op(), "replace");
    }

    #[tokio::test]
    async fn schema_analysis_infers_over_the_sample_and_lists_paths() {
        let (router, seen) = app(false);
        let (status, _, body, _) = call(router, "POST", "/api/db/m/mongo/schema", Some(json!({ "db": "app", "collection": "users", "sample": 50 }))).await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["sampled"], json!(2));
        assert_eq!(body["requested"], json!(50));
        let paths: Vec<&str> = body["paths"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(paths, ["_id", "name", "addr", "addr.city", "n"]);
        assert_eq!(calls(&seen), ["sample app.users 50"]);
    }

    #[tokio::test]
    async fn the_pipeline_preview_and_the_console_forward_what_they_were_given() {
        let (router, seen) = app(false);
        let (status, _, _, _) = call(
            router.clone(),
            "POST",
            "/api/db/m/mongo/aggregate",
            Some(json!({ "db": "app", "collection": "c", "stages": 1, "pipeline": [{ "$match": {} }, { "$group": { "_id": "$a" } }] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen.lock().unwrap().aggregate.clone().expect("forwarded").pipeline.len(), 1);
        let (status, _, body, _) = call(router.clone(), "POST", "/api/db/m/mongo/command", Some(json!({ "db": "app", "command": { "ping": 1 } }))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.expect("json")["elapsedMs"].is_u64());
        assert_eq!(seen.lock().unwrap().command.clone().expect("forwarded").0, "app");
        // The browser's guard refusal is the console's 400, in its own words.
        let (status, _, body, _) = call(router, "POST", "/api/db/m/mongo/command", Some(json!({ "db": "admin", "command": { "shutdown": 1 } }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.expect("json")["error"].as_str().unwrap().contains("rejected"));
    }

    #[tokio::test]
    async fn listings_take_their_namespace_from_the_query_string() {
        let (router, seen) = app(false);
        let (status, _, _, _) = call(router.clone(), "GET", "/api/db/m/mongo/collections?db=app", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, body, _) = call(router.clone(), "GET", "/api/db/m/mongo/indexes?db=app&collection=users", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["indexes"][0]["name"], json!("_id_"));
        let (status, _, _, _) = call(router.clone(), "POST", "/api/db/m/mongo/count", Some(json!({ "db": "app", "collection": "users", "exact": true }))).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _, _) = call(
            router.clone(),
            "POST",
            "/api/db/m/mongo/index",
            Some(json!({ "db": "app", "collection": "users", "op": "create", "keys": { "email": 1 }, "options": { "unique": true } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _, _) = call(router.clone(), "POST", "/api/db/m/mongo/collection", Some(json!({ "db": "app", "op": "rename", "name": "a", "to": "b" }))).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, body, _) = call(router, "POST", "/api/db/m/mongo/import", Some(json!({ "db": "app", "collection": "c", "docs": [{ "a": 1 }, { "a": 2 }] }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["inserted"], json!(2));
        let seen_calls = calls(&seen);
        assert_eq!(seen_calls[0], "collections app");
        assert_eq!(seen_calls[1], "indexes app.users");
        assert_eq!(seen_calls[2], "count exact=true");
        assert!(seen_calls[3].starts_with("index Create"), "{}", seen_calls[3]);
        assert!(seen_calls[4].starts_with("collection app Rename"), "{}", seen_calls[4]);
    }

    #[tokio::test]
    async fn an_export_streams_json_ndjson_and_csv() {
        let (router, seen) = app(false);
        let (status, headers, body, _) = call(router.clone(), "GET", "/api/db/m/mongo/export?db=app&collection=users&ejson=canonical", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-export-rows"], "2");
        assert_eq!(headers["x-export-format"], "json");
        assert_eq!(headers["content-disposition"], "attachment; filename=\"app.users.json\"");
        assert_eq!(body.expect("a JSON array").as_array().map(Vec::len), Some(2));
        assert_eq!(seen.lock().unwrap().export_relaxed, Some(false));

        let (_, _, _, text) = call(router.clone(), "GET", "/api/db/m/mongo/export?db=app&collection=users&format=ndjson", None).await;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(serde_json::from_str::<Value>(lines[1]).is_ok());

        let (_, headers, _, text) = call(router.clone(), "GET", "/api/db/m/mongo/export?db=app&collection=users&format=csv", None).await;
        assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
        let rows: Vec<&str> = text.split("\r\n").collect();
        // The header is the union of the documents' flattened paths, in first-seen order.
        assert_eq!(rows[0], "\"_id\",\"name\",\"addr.city\",\"n\"");
        assert_eq!(rows[1], "\"650000000000000000000001\",\"Ada\",\"London\",\"\"");
        assert_eq!(rows[2], "\"650000000000000000000002\",\"Bob, \"\"B\"\"\",\"\",\"7\"");

        // Named fields decide the columns, in their order.
        let (_, _, _, text) = call(
            router.clone(),
            "GET",
            "/api/db/m/mongo/export?db=app&collection=users&format=csv&fields=%5B%22name%22%2C%22_id%22%5D",
            None,
        )
        .await;
        assert!(text.starts_with("\"name\",\"_id\"\r\n\"Ada\",\"650000000000000000000001\"\r\n"), "{text}");

        let (status, _, _, _) = call(router, "GET", "/api/db/m/mongo/export?db=app&collection=users&format=xml", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
