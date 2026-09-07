//! The /api/db routes behind the panel's Data view — port of `dbbrowser-api.ts`: which
//! connections can be browsed, their (paged, greppable) table lists, one bounded page of rows per
//! table, the read-only SQL console, and the transactional edit commit.
//!
//! Same boundary as the rest of /api: the router's loopback guard is the only gate. Errors from
//! the database are handed back verbatim (driver messages never embed credentials) with a 400,
//! because for someone staring at a grid the driver's "Data too long for column 'x'" IS the
//! useful answer.
//!
//! # The resolver seam
//!
//! Node's `mountDbBrowseApi(r, registry)` reached each adapter through the registry:
//! `registry.get(name).adapter.dbBrowser?.()`, with `redisBrowser?.()` and `mongoBrowser?.()` as
//! the next choices. The Rust `Adapter` trait does not (yet) carry those accessors, so this
//! router takes a [`BrowseResolver`] closure instead — the app must supply one that maps the
//! live registry onto [`BrowsableConnection`] rows:
//!
//! - one row per REGISTERED MCP (browsable or not), so the 404s can name the adapter type the
//!   way Node's did ("MCP 'x' (echo) has no database to browse");
//! - `browser` picks the FIRST flavor the adapter offers in Node's priority order —
//!   db, then redis, then mongo — or [`BrowserFlavor::None`] when it offers none;
//! - `state` is Node's `stateOf(entry)`: the lifecycle string, or the health status once the
//!   entry has started (see adminapi's sidebar rows for the same rule).
//!
//! Once the adapters implement [`crate::dbbrowser::DbBrowser`] /
//! [`crate::dbbrowser::RedisBrowser`] / [`crate::dbbrowser::MongoBrowser`] and the registry
//! exposes them, the closure becomes a trivial mapping in app.rs.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Extension;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::app::{admin_error, admin_json};
use crate::dbbrowser::{js_to_string, DbBrowser, MongoBrowser, RedisBrowser};
use crate::log;

/// The browser flavor one registered MCP exposes.
pub enum BrowserFlavor {
    Db(Arc<dyn DbBrowser>),
    Redis(Arc<dyn RedisBrowser>),
    Mongo(Arc<dyn MongoBrowser>),
    /// Registered but nothing to browse (echo / proc / http / rest). Present so the lookup
    /// errors can name the adapter type exactly as Node's did; GET /api/db skips these rows.
    None,
}

/// One registered MCP as the resolver seam hands it over (see the module doc).
pub struct BrowsableConnection {
    pub name: String,
    /// The adapter's type string — Node's `entry.adapter.type`, used in 404 messages.
    pub adapter_type: String,
    /// `stateOf(entry)` — the lifecycle string, or the health status once started.
    pub state: String,
    pub browser: BrowserFlavor,
}

/// How the /api/db routes reach the registry: one call, every registered entry, browsable or
/// not. Cheap to call per request (the Node build walked `registry.all()` per request too).
pub type BrowseResolver = Arc<dyn Fn() -> Vec<Arc<BrowsableConnection>> + Send + Sync>;

/// One browsable-connection row: everything the panel's picker needs, nothing secret. The
/// redis flavour rides the same list with dialect "redis" and editable false (the key browser
/// is read-only by design; writes go through the MCP's redis_command tool).
pub fn browsable_connections(resolver: &BrowseResolver) -> Vec<Value> {
    let mut rows: Vec<(String, Value)> = resolver()
        .iter()
        .filter_map(|c| {
            let (dialect, label, readonly, editable) = match &c.browser {
                BrowserFlavor::Db(db) => (
                    db.dialect().as_str().to_string(),
                    db.label(),
                    db.readonly(),
                    true,
                ),
                BrowserFlavor::Redis(rb) => ("redis".to_string(), rb.label(), rb.readonly(), false),
                BrowserFlavor::Mongo(mb) => ("mongo".to_string(), mb.label(), mb.readonly(), false),
                BrowserFlavor::None => return None,
            };
            let row = json!({
                "name": c.name,
                "dialect": dialect,
                "label": label,
                "readonly": readonly,
                "state": c.state,
                "editable": editable,
            });
            Some((c.name.clone(), row))
        })
        .collect();
    // Node's `(a.name < b.name ? -1 : a.name > b.name ? 1 : 0)` sort — plain name order.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// Reply with the browser's message: 404 where Node attached a status to the error, 400 for
/// everything the database or a guard refused.
struct Fail {
    status: StatusCode,
    message: String,
}

impl Fail {
    fn bad(message: impl Into<String>) -> Self {
        Fail {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

/// A single edit batch is bounded so one commit stays one small transaction.
const MAX_EDITS: usize = 1000;

/// Cap on filters per page request — a picker the panel never exceeds, but a hand-written
/// URL should not be able to ask for a 500-term WHERE either.
const MAX_FILTERS: usize = 16;

// --- little coercion helpers (the Node route literals, spelled once) ------------------------------

/// `req.query.get(k) ?? ""` — present-but-empty stays "".
fn q_or_empty(q: &HashMap<String, String>, k: &str) -> String {
    q.get(k).cloned().unwrap_or_default()
}

/// `req.query.get(k) || undefined` — absent AND present-but-empty both become "not sent".
fn q_non_empty(q: &HashMap<String, String>, k: &str) -> Option<Value> {
    q.get(k).filter(|s| !s.is_empty()).map(|s| json!(s))
}

/// The raw pass-through Node used for numbers: the query string as-is, or nothing.
fn q_raw(q: &HashMap<String, String>, k: &str) -> Option<Value> {
    q.get(k).map(|s| json!(s))
}

/// `typeof body[k] === "string" && body[k] ? body[k] : undefined`.
fn nonempty_str_field(body: &Value, k: &str) -> Option<String> {
    body.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `String(body[k] ?? "")` — absent/null read as "", anything else JS-stringifies.
fn coerced_str(body: &Value, k: &str) -> String {
    match body.get(k) {
        None | Some(Value::Null) => String::new(),
        Some(v) => js_to_string(Some(v)),
    }
}

/// The request body the Node router handed every handler: parsed JSON when the client sent one,
/// null when it did not.
fn body_or_null(body: Result<Json<Value>, JsonRejection>) -> Value {
    body.map(|Json(v)| v).unwrap_or(Value::Null)
}

/// Parse the filters query param (a JSON array of {column, op, value}). Structural validation
/// only — unknown columns/operators are refused by build_filter_where inside the adapter, which
/// keeps the rule next to the SQL it guards.
fn parse_filters(raw: Option<&String>) -> Result<Option<Value>, Fail> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed: Value =
        serde_json::from_str(raw).map_err(|_| Fail::bad("filters must be a JSON array"))?;
    if !parsed.as_array().is_some_and(|a| a.len() <= MAX_FILTERS) {
        return Err(Fail::bad(format!(
            "filters must be an array of at most {MAX_FILTERS} terms"
        )));
    }
    Ok(Some(parsed))
}

// --- resolver lookups ---------------------------------------------------------------------------

fn find(resolver: &BrowseResolver, name: &str) -> Result<Arc<BrowsableConnection>, Fail> {
    resolver()
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .ok_or_else(|| Fail {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown MCP: {name}"),
        })
}

fn browser_of(resolver: &BrowseResolver, name: &str) -> Result<Arc<dyn DbBrowser>, Fail> {
    let entry = find(resolver, name)?;
    match &entry.browser {
        BrowserFlavor::Db(db) => Ok(db.clone()),
        _ => Err(Fail {
            status: StatusCode::NOT_FOUND,
            message: format!(
                "MCP '{}' ({}) has no database to browse",
                name, entry.adapter_type
            ),
        }),
    }
}

fn mongo_of(resolver: &BrowseResolver, name: &str) -> Result<Arc<dyn MongoBrowser>, Fail> {
    let entry = find(resolver, name)?;
    match &entry.browser {
        BrowserFlavor::Mongo(mb) => Ok(mb.clone()),
        _ => Err(Fail {
            status: StatusCode::NOT_FOUND,
            message: format!(
                "MCP '{}' ({}) is not a mongo connection",
                name, entry.adapter_type
            ),
        }),
    }
}

fn redis_of(resolver: &BrowseResolver, name: &str) -> Result<Arc<dyn RedisBrowser>, Fail> {
    let entry = find(resolver, name)?;
    match &entry.browser {
        BrowserFlavor::Redis(rb) => Ok(rb.clone()),
        _ => Err(Fail {
            status: StatusCode::NOT_FOUND,
            message: format!(
                "MCP '{}' ({}) is not a redis connection",
                name, entry.adapter_type
            ),
        }),
    }
}

fn reply(out: Result<Value, Fail>) -> Response {
    match out {
        Ok(body) => admin_json(StatusCode::OK, body),
        Err(f) => admin_error(f.status, &f.message),
    }
}

// --- handler bodies ------------------------------------------------------------------------------

async fn tables(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let mut o = Map::new();
    if let Some(grep) = q_non_empty(q, "grep") {
        o.insert("grep".into(), grep);
    }
    if let Some(page) = q_raw(q, "page") {
        o.insert("page".into(), page);
    }
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    b.list_tables(&Value::Object(o)).await.map_err(Fail::bad)
}

async fn data(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let mut o = Map::new();
    o.insert("table".into(), json!(q_or_empty(q, "table")));
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    if let Some(offset) = q_raw(q, "offset") {
        o.insert("offset".into(), offset);
    }
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    if let Some(order) = q_non_empty(q, "order") {
        o.insert("order".into(), order);
    }
    if let Some(dir) = q_non_empty(q, "dir") {
        o.insert("dir".into(), dir);
    }
    // `filters` carries the grid's field-level filters as JSON; the total counts the FILTERED
    // set, so the pager always describes what the grid is showing.
    if let Some(filters) = parse_filters(q.get("filters"))? {
        o.insert("filters".into(), filters);
    }
    b.read_table(&Value::Object(o)).await.map_err(Fail::bad)
}

async fn schema(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let mut o = Map::new();
    o.insert("table".into(), json!(q_or_empty(q, "table")));
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    b.describe_table(&Value::Object(o)).await.map_err(Fail::bad)
}

/// Whole-table export as a download (CSV or newline-JSON), capped at EXPORT_ROW_CAP rows. The
/// capped flag rides along as a header so the panel can warn without parsing the body.
async fn export(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Response, Fail> {
    let b = browser_of(resolver, name)?;
    let format = if q.get("format").map(String::as_str) == Some("json") {
        "json"
    } else {
        "csv"
    };
    let mut o = Map::new();
    o.insert("table".into(), json!(q_or_empty(q, "table")));
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    o.insert("format".into(), json!(format));
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    let out = b.export_table(&Value::Object(o)).await.map_err(Fail::bad)?;
    // The reply's own format decides the download's shape, not the request's spelling.
    let out_format = if out.get("format") == Some(&json!("json")) {
        "json"
    } else {
        "csv"
    };
    let file = format!(
        "{}-{}",
        out_format,
        q.get("table").cloned().unwrap_or_else(|| "table".into())
    );
    let content_type = if out_format == "json" {
        "application/x-ndjson; charset=utf-8"
    } else {
        "text/csv; charset=utf-8"
    };
    let body = out
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\""),
            ),
            (
                header::HeaderName::from_static("x-export-rows"),
                out.get("rows").unwrap_or(&json!(0)).to_string(),
            ),
            (
                header::HeaderName::from_static("x-export-capped"),
                if out.get("capped").and_then(Value::as_bool).unwrap_or(false) {
                    "1".into()
                } else {
                    "0".into()
                },
            ),
        ],
        body,
    )
        .into_response())
}

/// CSV import: header + lines + mapping arrive as JSON (the panel parsed and previewed them
/// already); the whole batch inserts inside ONE transaction on one connection.
/// `Array.isArray(v) && v.length && v.every(x => typeof x === "string")`.
fn str_array(v: Option<&Value>) -> Option<&Vec<Value>> {
    v.and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.iter().all(|x| x.is_string()))
}

async fn import(resolver: &BrowseResolver, name: &str, body: &Value) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let header = str_array(body.get("header"));
    let lines = str_array(body.get("lines"));
    if header.is_none() {
        return Err(Fail::bad(
            "header must be a non-empty array of column names",
        ));
    }
    if lines.is_none() {
        return Err(Fail::bad(
            "lines must be a non-empty array of CSV row strings",
        ));
    }
    // Both proven non-empty arrays by the checks above.
    let header = header.cloned().unwrap_or_default();
    let lines = lines.cloned().unwrap_or_default();
    if lines.len() > crate::dbbrowser::IMPORT_ROW_CAP {
        return Err(Fail::bad(format!(
            "too many rows for one import (max {})",
            crate::dbbrowser::IMPORT_ROW_CAP
        )));
    }
    let mapping_ok = body
        .get("mapping")
        .and_then(Value::as_array)
        .is_some_and(|m| m.len() == header.len() && m.iter().all(|x| x.is_null() || x.is_string()));
    if !mapping_ok {
        return Err(Fail::bad(
            "mapping must be an array (one per CSV column) of column names or null",
        ));
    }
    log::log(
        "info",
        "data view import",
        Some(json!({ "name": name, "table": coerced_str(body, "table"), "rows": lines.len() })),
    );
    let mut o = Map::new();
    o.insert("table".into(), json!(coerced_str(body, "table")));
    if let Some(schema) = nonempty_str_field(body, "schema") {
        o.insert("schema".into(), json!(schema));
    }
    o.insert("header".into(), Value::Array(header.clone()));
    o.insert("lines".into(), Value::Array(lines.clone()));
    o.insert(
        "mapping".into(),
        body.get("mapping").cloned().unwrap_or(Value::Null),
    );
    b.import_table(&Value::Object(o)).await.map_err(Fail::bad)
}

/// Structure operations: rename / truncate / drop. Destructive by nature — the readonly
/// refusal happens inside the adapter, and the panel additionally demands a typed
/// confirmation before it ever posts here.
async fn ddl(resolver: &BrowseResolver, name: &str, body: &Value) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let op = body.get("op").and_then(Value::as_str);
    let op_ok = op.is_some_and(|op| op == "rename" || op == "truncate" || op == "drop");
    if !op_ok {
        return Err(Fail::bad("op must be rename, truncate or drop"));
    }
    let op = op.unwrap_or_default();
    log::log(
        "warn",
        "data view ddl",
        Some(json!({
            "name": name,
            "op": op,
            "table": coerced_str(body, "table"),
            "to": coerced_str(body, "to"),
        })),
    );
    let mut o = Map::new();
    o.insert("op".into(), json!(op));
    o.insert("table".into(), json!(coerced_str(body, "table")));
    if let Some(schema) = nonempty_str_field(body, "schema") {
        o.insert("schema".into(), json!(schema));
    }
    if let Some(to) = nonempty_str_field(body, "to") {
        o.insert("to".into(), json!(to));
    }
    b.ddl_op(&Value::Object(o)).await.map_err(Fail::bad)
}

/// The read-only SQL console. The browser itself enforces read-only-ness; the route just
/// forwards, so the rule cannot drift between transport and model.
async fn query(resolver: &BrowseResolver, name: &str, body: &Value) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let sql = coerced_str(body, "sql");
    if sql.trim().is_empty() {
        return Err(Fail::bad("sql is required"));
    }
    b.run_query(&sql, body.get("limit"))
        .await
        .map_err(Fail::bad)
}

/// Commit a buffered edit list. ALL statements run in one transaction on one connection —
/// the first failure rolls the whole batch back, so the grid never half-applies.
async fn edits(resolver: &BrowseResolver, name: &str, body: &Value) -> Result<Value, Fail> {
    let b = browser_of(resolver, name)?;
    let Some(edit_list) = body
        .get("edits")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    else {
        return Err(Fail::bad("edits must be a non-empty array"));
    };
    if edit_list.len() > MAX_EDITS {
        return Err(Fail::bad(format!(
            "too many edits in one batch (max {MAX_EDITS})"
        )));
    }
    log::log(
        "info",
        "data view commit",
        Some(
            json!({ "name": name, "table": coerced_str(body, "table"), "edits": edit_list.len() }),
        ),
    );
    let mut o = Map::new();
    o.insert("table".into(), json!(coerced_str(body, "table")));
    if let Some(schema) = nonempty_str_field(body, "schema") {
        o.insert("schema".into(), json!(schema));
    }
    o.insert("edits".into(), Value::Array(edit_list.clone()));
    b.apply_edits(&Value::Object(o)).await.map_err(Fail::bad)
}

// --- the mongo collection browser (same /api/db namespace; dialect "mongo") ------------------------

async fn collections(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let mb = mongo_of(resolver, name)?;
    let mut o = Map::new();
    if let Some(grep) = q_non_empty(q, "grep") {
        o.insert("grep".into(), grep);
    }
    let list = mb
        .list_collections(&Value::Object(o))
        .await
        .map_err(Fail::bad)?;
    Ok(json!({ "collections": list }))
}

async fn docs(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let mb = mongo_of(resolver, name)?;
    let mut o = Map::new();
    o.insert("collection".into(), json!(q_or_empty(q, "collection")));
    if let Some(filter) = q_non_empty(q, "filter") {
        o.insert("filterJson".into(), filter);
    }
    if let Some(offset) = q_raw(q, "offset") {
        o.insert("offset".into(), offset);
    }
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    mb.read_collection(&Value::Object(o))
        .await
        .map_err(Fail::bad)
}

// --- the redis key browser (same /api/db namespace; dialect "redis") ------------------------------

async fn keys(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let rb = redis_of(resolver, name)?;
    let mut o = Map::new();
    if let Some(pattern) = q_non_empty(q, "pattern") {
        o.insert("pattern".into(), pattern);
    }
    if let Some(cursor) = q_non_empty(q, "cursor") {
        o.insert("cursor".into(), cursor);
    }
    if let Some(count) = q_raw(q, "count") {
        o.insert("count".into(), count);
    }
    if let Some(ty) = q_non_empty(q, "type") {
        o.insert("type".into(), ty);
    }
    rb.list_keys(&Value::Object(o)).await.map_err(Fail::bad)
}

/// The redis command console: ONE command per call, validated by the adapter guard
/// (readonly, no KEYS, no connection/server-breaking commands) before the socket is touched.
async fn command(resolver: &BrowseResolver, name: &str, body: &Value) -> Result<Value, Fail> {
    let rb = redis_of(resolver, name)?;
    let line = coerced_str(body, "command");
    if line.trim().is_empty() {
        return Err(Fail::bad("command is required"));
    }
    let reply = rb.run_command(&line).await.map_err(Fail::bad)?;
    Ok(json!({ "reply": reply }))
}

/// One key, read type-aware (the shape redis_read returns).
async fn key(
    resolver: &BrowseResolver,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let rb = redis_of(resolver, name)?;
    let key = q_or_empty(q, "key");
    if key.is_empty() {
        return Err(Fail::bad("key is required"));
    }
    rb.read_key(&key).await.map_err(Fail::bad)
}

// --- the axum handlers ----------------------------------------------------------------------------

async fn connections(Extension(res): Extension<BrowseResolver>) -> Response {
    admin_json(
        StatusCode::OK,
        json!({ "connections": browsable_connections(&res) }),
    )
}

async fn tables_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(tables(&res, &name, &q).await)
}

async fn data_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(data(&res, &name, &q).await)
}

async fn schema_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(schema(&res, &name, &q).await)
}

async fn export_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match export(&res, &name, &q).await {
        Ok(response) => response,
        Err(f) => admin_error(f.status, &f.message),
    }
}

async fn import_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    reply(import(&res, &name, &body_or_null(body)).await)
}

async fn collections_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(collections(&res, &name, &q).await)
}

async fn docs_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(docs(&res, &name, &q).await)
}

async fn keys_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(keys(&res, &name, &q).await)
}

async fn command_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    reply(command(&res, &name, &body_or_null(body)).await)
}

async fn key_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(key(&res, &name, &q).await)
}

async fn ddl_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    reply(ddl(&res, &name, &body_or_null(body)).await)
}

async fn query_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    reply(query(&res, &name, &body_or_null(body)).await)
}

async fn edits_route(
    Extension(res): Extension<BrowseResolver>,
    Path(name): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    reply(edits(&res, &name, &body_or_null(body)).await)
}

/// Mount the /api/db routes — the port of Node's `mountDbBrowseApi`. Generic over the state so
/// app.rs can merge it into the gateway router unchanged (no handler here reads the state; the
/// resolver arrives as an [`Extension`] layer).
pub fn dbbrowser_router<S>(resolver: BrowseResolver) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        // Which MCPs the Data view can browse is supplied by each adapter's browser hook.
        .route("/api/db", get(connections))
        // The lazy table list: one bounded page plus a counted total, with an optional name filter.
        .route("/api/db/{name}/tables", get(tables_route))
        // One grid page: columns, rows, total, and whether (and why not) the table is editable.
        .route("/api/db/{name}/data", get(data_route))
        // The Structure tabs: columns, indexes, foreign keys and DDL for one table.
        .route("/api/db/{name}/schema", get(schema_route))
        .route("/api/db/{name}/export", get(export_route))
        .route("/api/db/{name}/import", post(import_route))
        // --- mongo collection browser (same /api/db namespace; dialect "mongo") ---
        .route("/api/db/{name}/collections", get(collections_route))
        .route("/api/db/{name}/docs", get(docs_route))
        // --- redis key browser (same /api/db namespace; dialect "redis") ---
        .route("/api/db/{name}/keys", get(keys_route))
        .route("/api/db/{name}/command", post(command_route))
        .route("/api/db/{name}/key", get(key_route))
        .route("/api/db/{name}/ddl", post(ddl_route))
        .route("/api/db/{name}/query", post(query_route))
        .route("/api/db/{name}/edits", post(edits_route))
        .layer(Extension(resolver))
}

#[cfg(test)]
mod tests {
    // Ported from dbbrowser.test.ts's "data browser API" describe: a stub browser over the real
    // router, so the routes and the payload contract are tested end-to-end without a live
    // MySQL/Postgres/Redis/Mongo.
    use super::*;
    use crate::dbbrowser::{browse_offset, browse_page_size, map_import_rows, BROWSE_DEFAULT_PAGE};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use serde_json::json;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// What the stub recorded of the last call — the Node suite's `seen` object.
    #[derive(Default)]
    struct Seen {
        edits: Option<Value>,
        query: Option<String>,
        filters: Option<Value>,
        imported: Option<Value>,
        ddl: Option<Value>,
    }

    type SeenRef = Arc<Mutex<Seen>>;

    fn stub_columns() -> Value {
        Value::Array(vec![
            json!({ "name": "id", "dataType": "int", "nullable": false, "isPrimaryKey": true, "defaultValue": null, "comment": null }),
            json!({ "name": "name", "dataType": "text", "nullable": true, "isPrimaryKey": false, "defaultValue": null, "comment": null }),
        ])
    }

    /// A DbBrowser with an in-memory table, mirroring the Node suite's stubBrowser.
    struct StubDb {
        seen: SeenRef,
    }

    #[async_trait]
    impl DbBrowser for StubDb {
        fn dialect(&self) -> crate::dbbrowser::DbDialect {
            crate::dbbrowser::DbDialect::Mysql
        }
        fn readonly(&self) -> bool {
            false
        }
        fn label(&self) -> String {
            "stub @ localhost".into()
        }
        async fn list_tables(&self, o: &Value) -> Result<Value, String> {
            Ok(json!({
                "tables": [{ "schema": "app", "name": "users", "type": "table", "approxRows": 12, "size": "16 KB" }],
                "total": 1,
                "page": o.get("page").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64,
                "limit": 200,
                "more": false,
            }))
        }
        async fn read_table(&self, o: &Value) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.filters = o.get("filters").cloned();
            }
            let limit = browse_page_size(o.get("limit"), BROWSE_DEFAULT_PAGE) as usize;
            let offset = browse_offset(o.get("offset"));
            let mut rows = vec![
                json!({ "id": offset + 1, "name": "a" }),
                json!({ "id": offset + 2, "name": null }),
            ];
            rows.truncate(limit);
            Ok(json!({
                "schema": "app",
                "table": o.get("table").and_then(Value::as_str).unwrap_or(""),
                "columns": stub_columns(),
                "rows": rows,
                "total": 2,
                "offset": offset,
                "limit": limit,
                "primaryKey": ["id"],
                "editable": true,
            }))
        }
        async fn describe_table(&self, o: &Value) -> Result<Value, String> {
            Ok(json!({
                "schema": "app",
                "table": o.get("table").and_then(Value::as_str).unwrap_or(""),
                "columns": stub_columns(),
                "primaryKey": ["id"],
                "indexes": [{ "name": "PRIMARY", "unique": true, "primary": true, "columns": ["id"] }],
                "foreignKeys": [],
                "ddl": "CREATE TABLE stub",
            }))
        }
        async fn apply_edits(&self, o: &Value) -> Result<Value, String> {
            let edits = o.get("edits").cloned().unwrap_or(Value::Array(vec![]));
            if let Ok(mut seen) = self.seen.lock() {
                seen.edits = Some(edits.clone());
            }
            let results: Vec<Value> = edits
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|e| json!({ "op": e.get("op").and_then(Value::as_str).unwrap_or(""), "affected": 1 }))
                .collect();
            Ok(json!({ "results": results }))
        }
        async fn run_query(&self, sql: &str, _limit: Option<&Value>) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.query = Some(sql.to_string());
            }
            Ok(json!({ "columns": ["id"], "rows": [{ "id": 1 }], "rowCount": 1 }))
        }
        async fn export_table(&self, o: &Value) -> Result<Value, String> {
            let json_out = o.get("format") == Some(&json!("json"));
            Ok(json!({
                "format": if json_out { "json" } else { "csv" },
                "columns": ["id", "name"],
                "rows": 2,
                "capped": false,
                "body": if json_out { "{\"id\":1}\n{\"id\":2}" } else { "id,name\r\n1,a" },
            }))
        }
        async fn import_table(&self, o: &Value) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.imported = Some(json!({
                    "header": o.get("header"),
                    "lines": o.get("lines"),
                    "mapping": o.get("mapping"),
                }));
            }
            let header: Vec<String> = o
                .get("header")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let lines: Vec<String> = o
                .get("lines")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let mapping: Vec<Option<String>> = o
                .get("mapping")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let rows = map_import_rows(&header, &lines, &mapping).unwrap_or_default();
            Ok(json!({ "inserted": rows.len() }))
        }
        async fn ddl_op(&self, o: &Value) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.ddl = Some(o.clone());
            }
            let to = o.get("to").and_then(Value::as_str).unwrap_or("");
            if o.get("op") == Some(&json!("rename")) && to.is_empty() {
                return Err("rename needs the new table name".into());
            }
            Ok(
                json!({ "ran": format!("stub {}", o.get("op").and_then(Value::as_str).unwrap_or("")) }),
            )
        }
    }

    /// The Node suite's fake redis browser.
    struct StubRedis;

    #[async_trait]
    impl RedisBrowser for StubRedis {
        fn readonly(&self) -> bool {
            false
        }
        fn label(&self) -> String {
            "cache @ localhost:6379 db 0".into()
        }
        async fn list_keys(&self, _o: &Value) -> Result<Value, String> {
            Ok(json!({
                "keys": [
                    { "key": "session:1", "type": "string", "ttl": -1 },
                    { "key": "h:1", "type": "hash", "ttl": 60 },
                ],
                "cursor": "42",
                "done": false,
                "total": 2,
            }))
        }
        async fn read_key(&self, key: &str) -> Result<Value, String> {
            Ok(json!({ "key": key, "type": "string", "ttl": -1, "value": "hello" }))
        }
        async fn run_command(&self, line: &str) -> Result<Value, String> {
            if line.to_uppercase().starts_with("GET") {
                Ok(json!("value"))
            } else {
                Ok(json!(["a", "b"]))
            }
        }
    }

    /// The Node suite's fake mongo browser.
    struct StubMongo;

    #[async_trait]
    impl MongoBrowser for StubMongo {
        fn readonly(&self) -> bool {
            false
        }
        fn label(&self) -> String {
            "app @ localhost:27017".into()
        }
        async fn list_collections(&self, o: &Value) -> Result<Value, String> {
            let mut collections = vec![
                json!({ "name": "users", "type": "collection", "approxDocs": 12, "size": "4 KB" }),
            ];
            if o.get("grep").is_none() {
                collections.push(json!({ "name": "events", "type": "collection", "approxDocs": 9000, "size": "1.2 MB" }));
            }
            Ok(Value::Array(collections))
        }
        async fn read_collection(&self, o: &Value) -> Result<Value, String> {
            Ok(json!({
                "collection": o.get("collection").and_then(Value::as_str).unwrap_or(""),
                "documents": [{ "_id": 1, "name": "a" }, { "_id": 2, "name": "b" }],
                "total": 2,
                "offset": 0,
                "limit": 50,
                "fields": ["_id", "name"],
            }))
        }
    }

    /// Build a router over the given connections (the Node suite's setup() + registry.register).
    fn router_of(rows: Vec<Arc<BrowsableConnection>>) -> Router<()> {
        dbbrowser_router(Arc::new(move || rows.clone()))
    }

    fn db_entry(name: &str, browser: Arc<dyn DbBrowser>) -> Arc<BrowsableConnection> {
        Arc::new(BrowsableConnection {
            name: name.into(),
            adapter_type: "mysql".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Db(browser),
        })
    }

    fn plain_entry(name: &str) -> Arc<BrowsableConnection> {
        Arc::new(BrowsableConnection {
            name: name.into(),
            adapter_type: "echo".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::None,
        })
    }

    /// Drive one request and hand back status, headers, parsed-JSON-if-any and raw text.
    async fn call(
        router: Router<()>,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Option<Value>, String) {
        let builder = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => builder
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .expect("request literal"),
            None => builder.body(Body::empty()).expect("request literal"),
        };
        let resp = router.oneshot(req).await.expect("infallible router");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("test body");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_str(&text).ok();
        (status, headers, json, text)
    }

    #[tokio::test]
    async fn lists_only_browsable_mcps() {
        let app = router_of(vec![
            db_entry(
                "db-one",
                Arc::new(StubDb {
                    seen: Arc::new(Mutex::new(Seen::default())),
                }),
            ),
            plain_entry("plain"),
        ]);
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        let names: Vec<&str> = body["connections"]
            .as_array()
            .expect("connections")
            .iter()
            .map(|c| c["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, vec!["db-one"]);
        assert_eq!(body["connections"][0]["dialect"], "mysql");
        assert_eq!(body["connections"][0]["label"], "stub @ localhost");
    }

    #[tokio::test]
    async fn unknown_and_non_browsable_mcps_are_404() {
        let app = router_of(vec![plain_entry("plain")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/nope/tables", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.expect("json")["error"], "unknown MCP: nope");
        let app = router_of(vec![plain_entry("plain")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/plain/tables", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'plain' (echo) has no database to browse"
        );
    }

    #[tokio::test]
    async fn serves_paged_tables_and_rows() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: Arc::new(Mutex::new(Seen::default())),
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/tables?page=0&grep=us", None).await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["total"], 1);
        assert_eq!(body["more"], false);
        assert_eq!(body["tables"][0]["schema"], "app");
        assert_eq!(body["tables"][0]["name"], "users");
        assert_eq!(body["tables"][0]["approxRows"], 12);

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: Arc::new(Mutex::new(Seen::default())),
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/db/data?table=users&limit=20&offset=0&order=id&dir=desc",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["table"], "users");
        assert_eq!(body["total"], 2);
        assert_eq!(body["primaryKey"], json!(["id"]));
        assert_eq!(body["editable"], true);
        let names: Vec<&str> = body["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["id", "name"]);
    }

    #[tokio::test]
    async fn forwards_filters_and_rejects_malformed_ones() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone() }),
        )]);
        let terms = json!([{ "column": "name", "op": "like", "value": "al" }]).to_string();
        let (status, _, _, _) = call(
            app,
            "GET",
            &format!("/api/db/db/data?table=users&filters={}", urlencode(&terms)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen.lock().expect("seen").filters.take(),
            Some(json!([{ "column": "name", "op": "like", "value": "al" }]))
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/db/data?table=users&filters=not-json",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("JSON array"), "{err}");

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, _, _) = call(
            app,
            "GET",
            "/api/db/db/data?table=users&filters=%7B%7D",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Minimal percent-encoding for the query values the tests build.
    fn urlencode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    #[tokio::test]
    async fn serves_the_structure_detail() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/schema?table=users", None).await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["schema"], "app");
        assert_eq!(body["table"], "users");
        assert_eq!(body["primaryKey"], json!(["id"]));
        assert_eq!(body["ddl"], "CREATE TABLE stub");
        assert_eq!(body["indexes"][0]["name"], "PRIMARY");
        assert_eq!(body["indexes"][0]["unique"], true);
        assert_eq!(body["indexes"][0]["columns"], json!(["id"]));
    }

    #[tokio::test]
    async fn streams_an_export_with_download_headers() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, headers, _, text) =
            call(app, "GET", "/api/db/db/export?table=users&format=csv", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers["content-type"]
            .to_str()
            .unwrap()
            .contains("text/csv"));
        assert!(headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename=\"csv-users\""));
        assert_eq!(headers["x-export-rows"].to_str().unwrap(), "2");
        assert!(text.contains("id,name"));

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, headers, _, text) = call(
            app,
            "GET",
            "/api/db/db/export?table=users&format=json",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers["content-type"]
            .to_str()
            .unwrap()
            .contains("application/x-ndjson"));
        assert_eq!(text.split('\n').count(), 2);
    }

    #[tokio::test]
    async fn imports_csv_rows_and_rejects_malformed_payloads() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone() }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({ "table": "users", "header": ["id", "name"], "lines": ["1,alice", "2,bob"], "mapping": ["id", "name"] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json"), json!({ "inserted": 2 }));
        assert_eq!(
            seen.lock()
                .expect("seen")
                .imported
                .take()
                .expect("imported")["mapping"],
            json!(["id", "name"])
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, _, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({ "table": "users", "header": [], "lines": [], "mapping": [] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({ "table": "users", "header": ["a"], "lines": ["1"], "mapping": [] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("mapping"), "{err}");
    }

    #[tokio::test]
    async fn runs_structure_operations_and_rejects_unknown_ops() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone() }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl",
            Some(json!({ "op": "truncate", "table": "users" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json"), json!({ "ran": "stub truncate" }));
        assert_eq!(
            seen.lock().expect("seen").ddl.take(),
            Some(json!({ "op": "truncate", "table": "users" }))
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl",
            Some(json!({ "op": "explode", "table": "users" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("rename, truncate or drop"), "{err}");
    }

    #[tokio::test]
    async fn forwards_edit_batches_and_rejects_empty_ones() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, _, _) = call(
            app,
            "POST",
            "/api/db/db/edits",
            Some(json!({ "table": "users", "edits": [] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone() }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/edits",
            Some(json!({ "table": "users", "edits": [{ "op": "update", "pk": { "id": 1 }, "changes": { "name": "x" } }] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.expect("json")["results"],
            json!([{ "op": "update", "affected": 1 }])
        );
        assert_eq!(
            seen.lock().expect("seen").edits.take(),
            Some(json!([{ "op": "update", "pk": { "id": 1 }, "changes": { "name": "x" } }]))
        );
    }

    #[tokio::test]
    async fn runs_the_read_only_console() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone() }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/query",
            Some(json!({ "sql": "SELECT 1", "limit": 10 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["rowCount"], 1);
        assert_eq!(
            seen.lock().expect("seen").query.take(),
            Some("SELECT 1".to_string())
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/query",
            Some(json!({ "sql": "   " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.expect("json")["error"], "sql is required");
    }

    fn redis_entry(name: &str) -> Arc<BrowsableConnection> {
        Arc::new(BrowsableConnection {
            name: name.into(),
            adapter_type: "redis".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Redis(Arc::new(StubRedis)),
        })
    }

    fn mongo_entry(name: &str) -> Arc<BrowsableConnection> {
        Arc::new(BrowsableConnection {
            name: name.into(),
            adapter_type: "mongo".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Mongo(Arc::new(StubMongo)),
        })
    }

    #[tokio::test]
    async fn serves_the_redis_key_browser() {
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::OK);
        let row = body.expect("json")["connections"][0].clone();
        assert_eq!(row["dialect"], "redis");
        assert_eq!(row["editable"], false);
        assert_eq!(row["label"], "cache @ localhost:6379 db 0");

        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) =
            call(app, "GET", "/api/db/cache/keys?pattern=session:*", None).await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["cursor"], "42");
        assert_eq!(body["done"], false);
        assert_eq!(body["total"], 2);
        assert_eq!(
            body["keys"][0],
            json!({ "key": "session:1", "type": "string", "ttl": -1 })
        );

        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/key?key=session:1", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.expect("json"),
            json!({ "key": "session:1", "type": "string", "ttl": -1, "value": "hello" })
        );

        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, _, _) = call(app, "GET", "/api/db/cache/key", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // SQL-only routes 404 on a redis connection rather than half-working.
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/tables", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'cache' (redis) has no database to browse"
        );
    }

    #[tokio::test]
    async fn runs_a_read_only_redis_command() {
        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "GET mykey" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_ref().expect("json"), &json!({ "reply": "value" }));

        let app = router_of(vec![redis_entry("rdb")]);
        let (_status, _, body, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "LRANGE mylist 0 -1" })),
        )
        .await;
        assert_eq!(body.expect("json")["reply"], json!(["a", "b"]));

        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, _, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "  " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn serves_the_mongo_collection_browser() {
        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::OK);
        let row = body.expect("json")["connections"][0].clone();
        assert_eq!(row["dialect"], "mongo");
        assert_eq!(row["editable"], false);

        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, body, _) =
            call(app, "GET", "/api/db/mgo/collections?grep=user", None).await;
        assert_eq!(status, StatusCode::OK);
        let collections = body.expect("json")["collections"]
            .as_array()
            .expect("collections")
            .clone();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0]["name"], "users");
        assert_eq!(collections[0]["approxDocs"], 12);

        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/mgo/docs?collection=users&limit=50",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["collection"], "users");
        assert_eq!(body["total"], 2);
        assert_eq!(body["fields"], json!(["_id", "name"]));

        // SQL-only route refuses a mongo connection.
        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/mgo/tables", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'mgo' (mongo) has no database to browse"
        );
    }

    #[tokio::test]
    async fn mongo_and_redis_kinds_answer_the_flavored_404s() {
        // The flavored lookups name what the MCP is NOT.
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/collections", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'cache' (redis) is not a mongo connection"
        );

        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/mgo/keys", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'mgo' (mongo) is not a redis connection"
        );

        let app = router_of(vec![mongo_entry("mgo")]);
        let (status, _, _body, _) = call(
            app,
            "POST",
            "/api/db/mgo/command",
            Some(json!({ "command": "GET x" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
