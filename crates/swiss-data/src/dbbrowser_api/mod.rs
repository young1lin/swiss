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

//! The /api/db routes behind the panel's Data view — port of `dbbrowser-api.ts`: which
//! connections can be browsed, their (paged, greppable) table lists, one bounded page of rows per
//! table, the SQL console, and the transactional edit commit.
//!
//! Same boundary as the rest of /api: the router's loopback guard is the only gate. Errors from
//! the database are handed back verbatim (driver messages never embed credentials) with a 400,
//! because for someone staring at a grid the driver's "Data too long for column 'x'" IS the
//! useful answer.
//!
//! # The catalog seam (SPEC §host.seats)
//!
//! Node's `mountDbBrowseApi(r, registry)` reached each adapter through the registry; the Rust
//! port first replaced that with a resolver closure over the live registry. W3 removes the
//! last coupling: this router leases through the [`CatalogRegistry`] the MCP plugin
//! registers into — Data no longer knows who holds the connections, only that a provider
//! must exist:
//!
//! - every handler takes a REQUEST-SCOPED [`ConnectionLease`] and holds it until the browser
//!   call completes; that scope is exactly what a draining provider waits on before closing
//!   pools, so "disable MCP" can never yank a page out from under an open request;
//! - no provider (MCP disabled) answers a 503 that NAMES who is missing — "the mcp plugin
//!   provides database connections and is currently disabled" — instead of an empty list
//!   that would read as "you have none";
//! - the old 404 contracts survive verbatim: "unknown MCP: {name}" and "MCP 'x' (echo)
//!   has no database to browse" now come from the provider side of the catalog.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Extension;
use axum::Router;
use serde_json::{json, Map, Value};

use swiss_core::log;
use swiss_host::dbbrowser::{js_to_string, BrowserFlavor, DbBrowser, DumpPiece, RedisBrowser};
use swiss_host::reply::{admin_error, admin_json};
use swiss_host::services::catalog::{
    CatalogError, CatalogPresence, CatalogRegistry, ConnectionLease,
};

use crate::history_api::{record, HistoryLayer};

/// The sidebar's manual order, read live per request — the same list PUT /api/order stores.
/// A closure (not a snapshot) so a drag in the panel reorders the picker on the next poll.
pub type OrderSource = std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>;
/// The name -> group label source (SPEC §host.groups): the composition reads the sidebar's group
/// assignment live, so a regroup shows in the picker without a restart. Unassigned names
/// answer whatever the caller's sink rule gives - in practice the first group.
pub type GroupSource = std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>;

/// One browsable-connection row: everything the panel's picker needs, nothing secret. The
/// redis flavour rides the same list with the same shape; its key grid is browsing only and
/// writes go through the console — there is no readonly flag anywhere: the operator's
/// databases are the operator's to change.
///
/// The picker follows the sidebar's manual order (`order`): one manual order for the same
/// MCPs everywhere, so dragging a row in the sidebar reorders the Data dropdown too. Names
/// the order never covers — a freshly added MCP, or the whole list until the first drag —
/// fall back to name order, exactly like GET /api/mcps.
pub fn browsable_connections(
    catalog: &CatalogRegistry,
    order: &[String],
    group_of: &dyn Fn(&str) -> String,
) -> Vec<Value> {
    let rank: std::collections::HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut rows: Vec<(String, Value)> = catalog
        .list()
        .into_iter()
        .filter_map(|c| {
            // "none" = registered but nothing to browse (echo / proc / http / rest): GET
            // /api/db skips these rows; the lookups still see them so 404s can name the type.
            if c.dialect == "none" {
                return None;
            }
            let row = json!({
                "name": c.id,
                "dialect": c.dialect,
                "label": c.label,
                "state": c.state,
                // The sidebar group the connection renders under (SPEC §host.groups): a label the
                // picker's optgroups read - never a sort key, the order is already visual.
                "group": group_of(&c.id),
            });
            Some((c.id, row))
        })
        .collect();
    // Rank-first like /api/mcps: compare ranks only when they actually differ (both unranked
    // would tie at MAX), and let names decide between equals — Node's comparator guard.
    rows.sort_by(|a, b| {
        let ra = rank.get(a.0.as_str()).copied().unwrap_or(usize::MAX);
        let rb = rank.get(b.0.as_str()).copied().unwrap_or(usize::MAX);
        if ra != rb {
            ra.cmp(&rb)
        } else {
            a.0.cmp(&b.0)
        }
    });
    rows.into_iter().map(|(_, row)| row).collect()
}

/// Reply with the browser's message: 404 where Node attached a status to the error, 400 for
/// everything the database or a guard refused.
pub(crate) struct Fail {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    /// SPEC §data.edits: a lost optimistic-lock race's 409 extras — present only there. The
    /// body adds `conflictColumns` and the buffered `row` beside `error`, so the grid
    /// knows exactly which cells to paint red. Boxed: Fail crosses a dozen small helpers
    /// and stays one pointer wide beyond its status and message.
    pub(crate) conflict: Option<Box<FailConflict>>,
}

/// The 409's extra body fields: the moved columns, and the row the race was over.
#[derive(Clone)]
pub(crate) struct FailConflict {
    columns: Vec<String>,
    row: Map<String, Value>,
}

impl Fail {
    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Fail {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            conflict: None,
        }
    }

    /// A lost edit race answers 409, the moved columns and the row named in the body.
    pub(crate) fn conflict(message: impl Into<String>, columns: Vec<String>, row: Map<String, Value>) -> Self {
        Fail {
            status: StatusCode::CONFLICT,
            message: message.into(),
            conflict: Some(Box::new(FailConflict { columns, row })),
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

/// Parse the filters query param (a JSON array of {column, op, value}). Structural validation
/// only — unknown columns/operators are refused by build_filter_where inside the adapter, which
/// keeps the rule next to the SQL it guards.
fn parse_filters(raw: Option<&String>) -> Result<Option<Value>, Fail> {
    // Node's `if (!raw) return undefined`: absent AND present-but-empty both mean "not sent".
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
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

// --- catalog lookups -----------------------------------------------------------------------------

/// Map a catalog refusal onto the HTTP contract: unknown and not-browsable keep Node's 404
/// messages (now produced provider-side), a withdrawing provider is a 503.
pub(crate) fn lease_fail(err: CatalogError) -> Fail {
    match err {
        CatalogError::Unknown(m) | CatalogError::NotBrowsable(m) => Fail {
            status: StatusCode::NOT_FOUND,
            message: m,
            conflict: None,
        },
        CatalogError::Withdrawing(m) => Fail {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: m,
            conflict: None,
        },
    }
}

/// The catalog-wide 503 (SPEC §host.seats): no provider is NOT "zero connections". The message
/// names who is missing — "the mcp plugin provides database connections and is currently
/// disabled" — so disabling MCP reads as a consequence, not as an empty toolbox.
pub(crate) fn catalog_guard(catalog: &CatalogRegistry) -> Result<(), Fail> {
    match catalog.presence() {
        CatalogPresence::Serving(_) => Ok(()),
        CatalogPresence::Stopping(id) => Err(Fail {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "the {id} plugin is stopping; database browsing is momentarily unavailable"
            ),
            conflict: None,
        }),
        CatalogPresence::Absent(Some(id)) => Err(Fail {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "the {id} plugin provides database connections and is currently disabled"
            ),
            conflict: None,
        }),
        CatalogPresence::Absent(None) => Err(Fail {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "no plugin provides the connection catalog; database browsing is unavailable"
                .to_string(),
            conflict: None,
        }),
    }
}

/// Lease one connection as "data" for the life of the request and pull its SQL browser. The
/// returned LEASE is the point (SPEC §host.seats): callers keep it in scope until the browser call
/// completes — that scope is exactly what a draining provider waits on, so no pool closes
/// under a live page. The dialect string in the mismatch message is Node's adapter type for
/// every real dialect (mysql/pg/redis), so the 404 text survives the seam change.
fn lease_db(
    catalog: &CatalogRegistry,
    name: &str,
) -> Result<(ConnectionLease, Arc<dyn DbBrowser>), Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    let browser = match lease.flavor() {
        BrowserFlavor::Db(db) => db.clone(),
        _ => {
            return Err(Fail {
                status: StatusCode::NOT_FOUND,
                message: format!(
                    "MCP '{}' ({}) has no database to browse",
                    name,
                    lease.dialect()
                ),
                conflict: None,
            })
        }
    };
    Ok((lease, browser))
}

pub(crate) fn lease_redis(
    catalog: &CatalogRegistry,
    name: &str,
) -> Result<(ConnectionLease, Arc<dyn RedisBrowser>), Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    let browser = match lease.flavor() {
        BrowserFlavor::Redis(rb) => rb.clone(),
        _ => {
            return Err(Fail {
                status: StatusCode::NOT_FOUND,
                message: format!(
                    "MCP '{}' ({}) is not a redis connection",
                    name,
                    lease.dialect()
                ),
                conflict: None,
            })
        }
    };
    Ok((lease, browser))
}

pub(crate) fn reply(out: Result<Value, Fail>) -> Response {
    match out {
        Ok(body) => admin_json(StatusCode::OK, body),
        Err(f) => match f.conflict {
            // The 409 body names the moved columns and the row — the cells to paint red
            // (W4.2).
            Some(c) => admin_json(
                f.status,
                json!({ "error": f.message, "conflictColumns": c.columns, "row": c.row }),
            ),
            None => admin_error(f.status, &f.message),
        },
    }
}

// --- handler bodies ------------------------------------------------------------------------------

/// GET /api/db/{name}/databases (SPEC §data.databases): the connection's database catalog — the
/// configured primary, the one the pool sits on, and the rest with an honest per-entry
/// reason when browsing one needs its own connection. Serves BOTH flavors: the mysql/pg
/// adapters answer from information_schema / pg_database, redis from INFO keyspace.
async fn databases(catalog: &CatalogRegistry, name: &str) -> Result<Value, Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    let out = match lease.flavor() {
        BrowserFlavor::Db(db) => db.clone().list_databases().await,
        BrowserFlavor::Redis(rb) => rb.clone().list_databases().await,
        BrowserFlavor::Mongo(mb) => mb.clone().list_databases().await,
        BrowserFlavor::None => {
            return Err(Fail {
                status: StatusCode::NOT_FOUND,
                message: format!(
                    "MCP '{}' ({}) has no database to browse",
                    name,
                    lease.dialect()
                ),
                conflict: None,
            })
        }
    };
    out.map_err(Fail::bad)
}

async fn tables(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let mut o = Map::new();
    if let Some(grep) = q_non_empty(q, "grep") {
        o.insert("grep".into(), grep);
    }
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    if let Some(page) = q_raw(q, "page") {
        o.insert("page".into(), page);
    }
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    if let Some(sort) = q_non_empty(q, "sort") {
        o.insert("sort".into(), sort);
    }
    if let Some(dir) = q_non_empty(q, "dir") {
        o.insert("dir".into(), dir);
    }
    b.list_tables(&Value::Object(o)).await.map_err(Fail::bad)
}

async fn data(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
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
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let mut o = Map::new();
    o.insert("table".into(), json!(q_or_empty(q, "table")));
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    b.describe_table(&Value::Object(o)).await.map_err(Fail::bad)
}

/// Whole-table export as a download (CSV, newline-JSON or a streaming SQL dump), capped at
/// EXPORT_ROW_CAP rows. The capped flag rides along as a header so the panel can warn without
/// parsing the body; x-export-format names the shape the panel saved.
async fn export(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Response, Fail> {
    // format=sql streams its body in pieces (SPEC §data.export) — it cannot ride the folded path.
    if q.get("format").map(String::as_str) == Some("sql") {
        return export_sql(catalog, name, q).await;
    }
    let (_lease, b) = lease_db(catalog, name)?;
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
    // The grid's filters ride /export exactly as they ride /data (SPEC §data.export): the download
    // and the grid describe the same filtered set, and x-export-rows counts that set.
    if let Some(filters) = parse_filters(q.get("filters"))? {
        o.insert("filters".into(), filters);
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
            (
                header::HeaderName::from_static("x-export-format"),
                out_format.to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

/// The dump body as a Stream: each channel piece becomes one body chunk, and the connection
/// lease rides along inside it. The handler returns while the dump is still being produced —
/// holding the lease in the body (not the handler's frame) is what keeps a draining provider
/// from closing the pool under the last pieces (SPEC §host.seats, SPEC §data.export).
pub(crate) struct LeaseBody {
    pub(crate) rx: tokio::sync::mpsc::Receiver<DumpPiece>,
    pub(crate) lease: Option<ConnectionLease>,
}

impl futures_core::Stream for LeaseBody {
    type Item = Result<axum::body::Bytes, axum::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from(bytes))))
            }
            std::task::Poll::Ready(Some(Err(message))) => {
                std::task::Poll::Ready(Some(Err(axum::Error::new(message))))
            }
            // The producer closed the channel: the dump is done, the lease may go.
            std::task::Poll::Ready(None) => {
                this.lease = None;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// The format=sql arm of /export (SPEC §data.export): the dump's metadata comes back before the
/// body starts — a bad table still answers its usual 400 — then the body streams through
/// [`axum::body::Body::from_stream`] as the producer finishes each ~1 MB statement.
async fn export_sql(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Response, Fail> {
    let (lease, b) = lease_db(catalog, name)?;
    let mut o = Map::new();
    o.insert("table".into(), json!(q_or_empty(q, "table")));
    if let Some(schema) = q_non_empty(q, "schema") {
        o.insert("schema".into(), schema);
    }
    o.insert("format".into(), json!("sql"));
    if let Some(limit) = q_raw(q, "limit") {
        o.insert("limit".into(), limit);
    }
    // The grid's filters ride /export exactly as they ride /data (SPEC §data.export).
    if let Some(filters) = parse_filters(q.get("filters"))? {
        o.insert("filters".into(), filters);
    }
    let dump = b
        .export_sql_dump(&Value::Object(o))
        .await
        .map_err(Fail::bad)?;
    let file = format!(
        "sql-{}",
        q.get("table").cloned().unwrap_or_else(|| "table".into())
    );
    let body = LeaseBody {
        rx: dump.body,
        lease: Some(lease),
    };
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/sql; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\""),
            ),
            (
                header::HeaderName::from_static("x-export-rows"),
                dump.rows.to_string(),
            ),
            (
                header::HeaderName::from_static("x-export-capped"),
                if dump.capped { "1".into() } else { "0".into() },
            ),
            (
                header::HeaderName::from_static("x-export-format"),
                "sql".to_string(),
            ),
        ],
        axum::body::Body::from_stream(body),
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

async fn import(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
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
    if lines.len() > swiss_host::dbbrowser::IMPORT_ROW_CAP {
        return Err(Fail::bad(format!(
            "too many rows for one import (max {})",
            swiss_host::dbbrowser::IMPORT_ROW_CAP
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
    // SPEC §data.export: the panel picks the statement form — insert stays the default, and a
    // payload without the field rides exactly as it did before the mode existed.
    let mode = swiss_host::dbbrowser::parse_import_mode(body.get("mode")).map_err(Fail::bad)?;
    let mut logged =
        json!({ "name": name, "table": coerced_str(body, "table"), "rows": lines.len() });
    if mode == swiss_host::dbbrowser::ImportMode::Upsert {
        logged["mode"] = json!("upsert");
    }
    log::log("info", "data view import", Some(logged));
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
    if mode == swiss_host::dbbrowser::ImportMode::Upsert {
        o.insert("mode".into(), json!("upsert"));
    }
    b.import_table(&Value::Object(o)).await.map_err(Fail::bad)
}

/// Structure operations: rename / truncate / drop. Destructive by nature — the panel demands
/// a typed confirmation before it ever posts here.
async fn ddl(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let op = body.get("op").and_then(Value::as_str);
    // SPEC §data.ddl: the create ops carry their facts in a payload object; /ddl-preview showed
    // the exact statements this runs (same builder, same input), so Commit is the preview.
    if op.is_some_and(|op| matches!(op, "create_table" | "add_column" | "create_index")) {
        let op = op.unwrap_or_default();
        let Some(Value::Object(payload)) = body.get("payload") else {
            return Err(Fail::bad("payload must be an object"));
        };
        log::log(
            "warn",
            "data view ddl",
            Some(json!({
                "name": name,
                "op": op,
                "table": payload.get("table").and_then(Value::as_str).unwrap_or(""),
            })),
        );
        // Flatten (op, payload) into the one flat object the browser contract speaks.
        let mut o = Map::new();
        o.insert("op".into(), json!(op));
        o.extend(payload.iter().map(|(k, v)| (k.clone(), v.clone())));
        return b.ddl_op(&Value::Object(o)).await.map_err(Fail::bad);
    }
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

/// The form-to-SQL preview behind the New table / Add column / New index sheets (SPEC §data.ddl).
/// The SAME build_ddl_create the commit path runs, on the leased connection's own
/// dialect — pgAdmin's msql flow: one SQL producer serves preview and save, so the text the
/// sheet shows is byte-for-byte the text /ddl executes on Commit.
async fn ddl_preview(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let op = body
        .get("op")
        .and_then(Value::as_str)
        .filter(|op| matches!(*op, "create_table" | "add_column" | "create_index"))
        .ok_or_else(|| Fail::bad("op must be create_table, add_column or create_index"))?;
    let payload = body.get("payload").cloned().unwrap_or(Value::Null);
    let stmts =
        swiss_host::dbbrowser::build_ddl_create(b.dialect(), op, &payload).map_err(Fail::bad)?;
    Ok(json!({ "sql": swiss_host::dbbrowser::ddl_script(&stmts) }))
}

/// The SQL console. The browser itself enforces the one-statement-per-run rule; the route
/// just forwards, so the rule cannot drift between transport and model. Writes run.
async fn query(
    catalog: &CatalogRegistry,
    history: &HistoryLayer,
    name: &str,
    body: &Value,
) -> Result<Value, Fail> {
    let (lease, b) = lease_db(catalog, name)?;
    let sql = coerced_str(body, "sql");
    if sql.trim().is_empty() {
        return Err(Fail::bad("sql is required"));
    }
    // The elapsed clock wraps the browser call: what the user waited on is the driver round
    // trip, not the JSON hop around it (SPEC §data.console).
    let started = std::time::Instant::now();
    let out = b.run_query(&sql, body.get("limit")).await;
    // Filed under the database the statement reached, a failure too (SPEC §data.history).
    record(history, || b.label(), name, lease.dialect(), &sql, &out, started);
    Ok(with_elapsed_ms(out.map_err(Fail::bad)?, started))
}

/// Attach `elapsedMs` (whole milliseconds, a JS-friendly integer) to a console reply. A
/// browser answer that is somehow not an object passes through untouched rather than being
/// reshaped around the one field that was added for it.
pub(crate) fn with_elapsed_ms(mut v: Value, started: std::time::Instant) -> Value {
    if let Value::Object(map) = &mut v {
        map.insert(
            "elapsedMs".into(),
            json!(started.elapsed().as_millis() as u64),
        );
    }
    v
}

/// Commit a buffered edit list. ALL statements run in one transaction on one connection —
/// the first failure rolls the whole batch back, so the grid never half-applies.
async fn edits(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
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
    b.apply_edits(&Value::Object(o)).await.map_err(|e| match e {
        // Whose fault the failure is decides the status (SPEC §data.edits): a refused request
        // is 400, a lost race against another writer is 409 with the moved columns.
        swiss_host::dbbrowser::EditError::Bad(m) => Fail::bad(m),
        swiss_host::dbbrowser::EditError::Conflict(c) => {
            Fail::conflict(c.message, c.columns, c.row.unwrap_or_default())
        }
    })
}

// --- the redis key browser (same /api/db namespace; dialect "redis") ------------------------------

async fn keys(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
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

/// The redis command console: ONE command per call, validated by the adapter guard (no KEYS,
/// no connection/server-breaking commands) before the socket is touched. Writes run.
async fn command(
    catalog: &CatalogRegistry,
    history: &HistoryLayer,
    name: &str,
    body: &Value,
) -> Result<Value, Fail> {
    let (lease, rb) = lease_redis(catalog, name)?;
    let line = coerced_str(body, "command");
    if line.trim().is_empty() {
        return Err(Fail::bad("command is required"));
    }
    // Same elapsed contract as the SQL console (SPEC §data.console): the clock wraps the call.
    let started = std::time::Instant::now();
    let out = rb.run_command(&line).await;
    // A redis reply has no row count: whatever shape it takes, the run files none.
    let filed = out.as_ref().map(|_| Value::Null).map_err(Clone::clone);
    record(history, || rb.label(), name, lease.dialect(), &line, &filed, started);
    let reply = out.map_err(Fail::bad)?;
    Ok(with_elapsed_ms(json!({ "reply": reply }), started))
}

/// Server-side completion for the console (SPEC §data.completion): candidates for the word ending at
/// the caret. The body carries the WHOLE console text and the caret as a byte offset (the
/// panel converts its UTF-16 selection index); the browser folds keywords, table names and
/// the FROM-nearest table's columns into the reply.
async fn completion(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let sql = body
        .get("sql")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Fail::bad("sql is required"))?;
    let caret = body
        .get("caret")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, sql.len() as i64) as usize;
    b.completion(sql, caret).await.map_err(Fail::bad)
}

/// Live sessions on the connection's server (SPEC §data.activity) — the Activity page, polled
/// while it is open. The reply shape is shared by both dialects (dbbrowser/activity.rs).
async fn activity(catalog: &CatalogRegistry, name: &str) -> Result<Value, Fail> {
    // A mongo connection answers from $currentOp in the same row shape (SPEC §data.mongo).
    if let Some((_lease, mb)) = crate::mongo_api::lease_mongo_if(catalog, name)? {
        return mb.activity().await.map_err(Fail::bad);
    }
    let (_lease, b) = lease_db(catalog, name)?;
    b.activity().await.map_err(Fail::bad)
}

/// Cancel (pg_cancel_backend / KILL QUERY) or terminate (pg_terminate_backend / KILL) one
/// session (SPEC §data.activity). The mode word and the pid are validated here — a kill is the one
/// route in this router that interrupts someone else's work, so it logs what it did.
async fn activity_kill(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    if let Some((_lease, mb)) = crate::mongo_api::lease_mongo_if(catalog, name)? {
        return crate::mongo_api::activity_kill(name, &mb, body).await;
    }
    let (_lease, b) = lease_db(catalog, name)?;
    let pid = body
        .get("pid")
        .and_then(Value::as_i64)
        .filter(|p| *p > 0)
        .ok_or_else(|| Fail::bad("pid must be a session id (positive integer)"))?;
    let terminate = match body.get("mode").and_then(Value::as_str) {
        Some("cancel") => false,
        Some("terminate") => true,
        _ => return Err(Fail::bad("mode must be cancel or terminate")),
    };
    log::log(
        "warn",
        "data view activity kill",
        Some(json!({
            "name": name,
            "pid": pid,
            "mode": if terminate { "terminate" } else { "cancel" },
        })),
    );
    b.activity_kill(pid, terminate).await.map_err(Fail::bad)
}

/// Commit a buffered redis structured-edit batch (SPEC §data.redis): the typed value view's
/// whole buffer as ONE pipelined round trip. Shape is checked here; WHICH commands may run
/// stays with the adapter's console guard, applied per command before the socket is touched —
/// the first refusal rejects the batch whole, so a buffered edit never half-applies.
async fn redis_pipeline(
    catalog: &CatalogRegistry,
    name: &str,
    body: &Value,
) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
    let commands = swiss_host::dbbrowser::redis_pipeline_commands(body).map_err(Fail::bad)?;
    log::log(
        "info",
        "data view redis commit",
        Some(json!({ "name": name, "commands": commands.len() })),
    );
    rb.run_pipeline(&commands).await.map_err(Fail::bad)
}

/// One key, read type-aware (the shape redis_read returns).
async fn key(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
    let key = q_or_empty(q, "key");
    if key.is_empty() {
        return Err(Fail::bad("key is required"));
    }
    rb.read_key(&key).await.map_err(Fail::bad)
}

/// GET /api/db/{name}/stream — one newest-first window of one stream key (SPEC §data.streams):
/// the opening page by default, `before` pages strictly older (“load
/// earlier”), `after` catches up strictly newer (the Follow tick). `match` keeps
/// only the entries a filter line names (SPEC §data.streams) — a bounded backward walk,
/// because redis indexes nothing inside an entry. Query params
/// become the option object and are validated by the host helper BEFORE the lease
/// — a caller's mistake is reported as such and costs no socket, the house rule
/// every browser route follows.
async fn stream(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let key = q_or_empty(q, "key");
    if key.is_empty() {
        return Err(Fail::bad("key is required"));
    }
    let o = stream_opts_of(q);
    swiss_host::dbbrowser::redis_stream_opts(&o).map_err(Fail::bad)?;
    let (_lease, rb) = lease_redis(catalog, name)?;
    rb.read_stream(&key, &o).await.map_err(Fail::bad)
}

/// GET /api/db/{name}/redis-commands — the command catalog of the redis this connection
/// speaks to (SPEC §data.redis-console): COMMAND DOCS for the words, COMMAND INFO for the key positions. The
/// console's completion is built from it, so it offers the commands that server actually has
/// — its modules and its version included — instead of a table kept by hand in the panel.
async fn redis_commands(catalog: &CatalogRegistry, name: &str) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
    rb.command_catalog().await.map_err(Fail::bad)
}

/// GET /api/db/{name}/stream/groups — the read-only consumer-group table of one
/// stream key (SPEC §data.streams): name / consumers / pending / lag / last-delivered-id,
/// nothing writable.
async fn stream_groups(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let key = q_or_empty(q, "key");
    if key.is_empty() {
        return Err(Fail::bad("key is required"));
    }
    let (_lease, rb) = lease_redis(catalog, name)?;
    rb.stream_groups(&key).await.map_err(Fail::bad)
}

/// The query params that become read_stream's option object, shaped as the Value
/// the trait consumes. Only non-empty values ride — an empty `after=` is the
/// opening page, same as no param at all (the panel's URL builders leave blanks
/// in).
fn stream_opts_of(q: &HashMap<String, String>) -> Value {
    let mut o = serde_json::Map::new();
    for p in ["before", "after", "count", "match"] {
        if let Some(v) = q.get(p).map(|s| s.trim()).filter(|v| !v.is_empty()) {
            o.insert(p.into(), json!(v));
        }
    }
    Value::Object(o)
}

// --- the axum handlers ----------------------------------------------------------------------------

async fn connections(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Extension(order): Extension<OrderSource>,
    Extension(groups): Extension<GroupSource>,
) -> Response {
    // The honest 503 first (SPEC §host.seats): with no provider, an empty list would read as
    // "you have no connections" — the message names who is missing instead.
    if let Err(f) = catalog_guard(&catalog) {
        return admin_error(f.status, &f.message);
    }
    let order = order();
    admin_json(
        StatusCode::OK,
        json!({ "connections": browsable_connections(&catalog, &order, &*groups) }),
    )
}

async fn tables_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(tables(&catalog, &name, &q).await)
}

async fn databases_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
) -> Response {
    reply(databases(&catalog, &name).await)
}

async fn data_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(data(&catalog, &name, &q).await)
}

async fn schema_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(schema(&catalog, &name, &q).await)
}

async fn export_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match export(&catalog, &name, &q).await {
        Ok(response) => response,
        Err(f) => admin_error(f.status, &f.message),
    }
}

async fn import_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(import(&catalog, &name, &body.0).await)
}

async fn keys_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(keys(&catalog, &name, &q).await)
}

async fn command_route(
    history: HistoryLayer,
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(command(&catalog, &history, &name, &body.0).await)
}

async fn completion_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(completion(&catalog, &name, &body.0).await)
}

async fn activity_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
) -> Response {
    reply(activity(&catalog, &name).await)
}

async fn activity_kill_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(activity_kill(&catalog, &name, &body.0).await)
}

async fn redis_pipeline_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(redis_pipeline(&catalog, &name, &body.0).await)
}

async fn redis_commands_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
) -> Response {
    reply(redis_commands(&catalog, &name).await)
}

async fn stream_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(stream(&catalog, &name, &q).await)
}

async fn stream_groups_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(stream_groups(&catalog, &name, &q).await)
}

async fn key_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(key(&catalog, &name, &q).await)
}

async fn ddl_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(ddl(&catalog, &name, &body.0).await)
}

async fn ddl_preview_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(ddl_preview(&catalog, &name, &body.0).await)
}

async fn query_route(
    history: HistoryLayer,
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(query(&catalog, &history, &name, &body.0).await)
}

async fn edits_route(
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(edits(&catalog, &name, &body.0).await)
}

/// Mount the /api/db routes — the port of Node's `mountDbBrowseApi`. Generic over the state so
/// app.rs can merge it into the gateway router unchanged (no handler here reads the state; the
/// connection catalog and the sidebar-order source arrive as [`Extension`] layers). The console
/// history (SPEC §data.history) is the caller's own `Extension<Arc<DataHistory>>` layer on
/// the result: without one the consoles record nothing and its routes answer 503.
pub fn dbbrowser_router<S>(
    catalog: Arc<CatalogRegistry>,
    order: OrderSource,
    groups: GroupSource,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        // Which MCPs the Data view can browse is supplied by each adapter's browser hook.
        .route("/api/db", get(connections))
        // The lazy table list: one bounded page plus a counted total, with an optional name filter.
        .route("/api/db/{name}/tables", get(tables_route))
        // The database axis (SPEC §data.databases): primary, current, and the rest with per-entry
        // browsability — the panel's database selector is this body, whole.
        .route("/api/db/{name}/databases", get(databases_route))
        // One grid page: columns, rows, total, and whether (and why not) the table is editable.
        .route("/api/db/{name}/data", get(data_route))
        // The Structure tabs: columns, indexes, foreign keys and DDL for one table.
        .route("/api/db/{name}/schema", get(schema_route))
        .route("/api/db/{name}/export", get(export_route))
        .route("/api/db/{name}/import", post(import_route))
        // --- redis key browser (same /api/db namespace; dialect "redis") ---
        .route("/api/db/{name}/keys", get(keys_route))
        .route("/api/db/{name}/command", post(command_route))
        // The buffered structured-edit commit (SPEC §data.redis): one pipeline, one round trip.
        .route("/api/db/{name}/redis-pipeline", post(redis_pipeline_route))
        // SPEC §data.redis-console: the console's completion source, read from the server itself.
        .route("/api/db/{name}/redis-commands", get(redis_commands_route))
        // Live sessions and the cancel/terminate pair (SPEC §data.activity).
        .route("/api/db/{name}/activity", get(activity_route))
        .route("/api/db/{name}/activity-kill", post(activity_kill_route))
        // The console's completion (SPEC §data.completion) — server-side, on the leased connection.
        .route("/api/db/{name}/completion", post(completion_route))
        .route("/api/db/{name}/key", get(key_route))
        // SPEC §data.streams: the stream window (newest-first, cursor paging) and its read-only
        // consumer groups — both GET, neither audited, like every other read here.
        .route("/api/db/{name}/stream", get(stream_route))
        .route("/api/db/{name}/stream/groups", get(stream_groups_route))
        .route("/api/db/{name}/ddl", post(ddl_route))
        // The W4.6 sheets' live preview: the statements /ddl would run for this (op, payload).
        .route("/api/db/{name}/ddl-preview", post(ddl_preview_route))
        .route("/api/db/{name}/query", post(query_route))
        .route("/api/db/{name}/edits", post(edits_route))
        // SPEC §data.mongo: the document flavour's routes, under /api/db/{name}/mongo/.
        .merge(crate::mongo_api::mongo_router())
        // SPEC §data.pubsub: the channel listing and the live NDJSON feed.
        .merge(crate::pubsub_api::pubsub_router())
        // SPEC §data.history: one database's console runs, read and cleared.
        .merge(crate::history_api::history_router())
        .layer(Extension(catalog))
        .layer(Extension(order))
        .layer(Extension(groups))
}

#[cfg(test)]
mod tests;
