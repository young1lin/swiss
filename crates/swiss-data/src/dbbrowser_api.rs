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
struct Fail {
    status: StatusCode,
    message: String,
    /// SPEC §data.edits: a lost optimistic-lock race's 409 extras — present only there. The
    /// body adds `conflictColumns` and the buffered `row` beside `error`, so the grid
    /// knows exactly which cells to paint red. Boxed: Fail crosses a dozen small helpers
    /// and stays one pointer wide beyond its status and message.
    conflict: Option<Box<FailConflict>>,
}

/// The 409's extra body fields: the moved columns, and the row the race was over.
#[derive(Clone)]
struct FailConflict {
    columns: Vec<String>,
    row: Map<String, Value>,
}

impl Fail {
    fn bad(message: impl Into<String>) -> Self {
        Fail {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            conflict: None,
        }
    }

    /// A lost edit race answers 409, the moved columns and the row named in the body.
    fn conflict(message: impl Into<String>, columns: Vec<String>, row: Map<String, Value>) -> Self {
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
fn lease_fail(err: CatalogError) -> Fail {
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
fn catalog_guard(catalog: &CatalogRegistry) -> Result<(), Fail> {
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

fn lease_redis(
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

fn reply(out: Result<Value, Fail>) -> Response {
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
struct LeaseBody {
    rx: tokio::sync::mpsc::Receiver<DumpPiece>,
    lease: Option<ConnectionLease>,
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
async fn query(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    let sql = coerced_str(body, "sql");
    if sql.trim().is_empty() {
        return Err(Fail::bad("sql is required"));
    }
    // The elapsed clock wraps the browser call: what the user waited on is the driver round
    // trip, not the JSON hop around it (SPEC §data.console).
    let started = std::time::Instant::now();
    let out = b
        .run_query(&sql, body.get("limit"))
        .await
        .map_err(Fail::bad)?;
    Ok(with_elapsed_ms(out, started))
}

/// Attach `elapsedMs` (whole milliseconds, a JS-friendly integer) to a console reply. A
/// browser answer that is somehow not an object passes through untouched rather than being
/// reshaped around the one field that was added for it.
fn with_elapsed_ms(mut v: Value, started: std::time::Instant) -> Value {
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
async fn command(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
    let line = coerced_str(body, "command");
    if line.trim().is_empty() {
        return Err(Fail::bad("command is required"));
    }
    // Same elapsed contract as the SQL console (SPEC §data.console): the clock wraps the call.
    let started = std::time::Instant::now();
    let reply = rb.run_command(&line).await.map_err(Fail::bad)?;
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
/// while it is open. The reply shape is shared by both dialects (dbbrowser.rs).
async fn activity(catalog: &CatalogRegistry, name: &str) -> Result<Value, Fail> {
    let (_lease, b) = lease_db(catalog, name)?;
    b.activity().await.map_err(Fail::bad)
}

/// Cancel (pg_cancel_backend / KILL QUERY) or terminate (pg_terminate_backend / KILL) one
/// session (SPEC §data.activity). The mode word and the pid are validated here — a kill is the one
/// route in this router that interrupts someone else's work, so it logs what it did.
async fn activity_kill(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Value, Fail> {
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
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(command(&catalog, &name, &body.0).await)
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
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    reply(query(&catalog, &name, &body.0).await)
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
/// connection catalog and the sidebar-order source arrive as [`Extension`] layers).
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
        .layer(Extension(catalog))
        .layer(Extension(order))
        .layer(Extension(groups))
}

#[cfg(test)]
mod tests {
    // Ported from dbbrowser.test.ts's "data browser API" describe: a stub browser over the real
    // router, so the routes and the payload contract are tested end-to-end without a live
    // MySQL/Postgres/Redis.
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use serde_json::json;
    use std::sync::Mutex;
    use swiss_host::dbbrowser::{
        browse_offset, browse_page_size, map_import_rows, BROWSE_DEFAULT_PAGE,
    };
    use swiss_host::services::catalog::{
        ConnectionCatalog, ConnectionInfo, ConnectionLease, LeaseTracker,
    };
    use tower::ServiceExt;

    /// What the stub recorded of the last call — the Node suite's `seen` object.
    #[derive(Default)]
    struct Seen {
        edits: Option<Value>,
        query: Option<String>,
        filters: Option<Value>,
        export_opts: Option<Value>,
        /// Body pieces the SQL dump's producer handed to the channel (SPEC §data.export) — the
        /// route test's proof that the dump streamed instead of folding one string.
        dump_pieces: usize,
        imported: Option<Value>,
        ddl: Option<Value>,
        tables_opts: Option<Value>,
        /// The command list the redis pipeline route forwarded (SPEC §data.redis).
        pipeline: Option<Vec<Vec<String>>>,
        /// The (pid, terminate) the activity-kill route forwarded (SPEC §data.activity).
        activity_kill: Option<(i64, bool)>,
        /// The (sql, caret) the completion route forwarded (SPEC §data.completion).
        completion: Option<(String, usize)>,
        /// The option object the stream route forwarded (SPEC §data.streams).
        stream_opts: Option<Value>,
        /// The key the stream groups route forwarded (SPEC §data.streams).
        stream_groups_key: Option<String>,
    }

    type SeenRef = Arc<Mutex<Seen>>;

    fn stub_columns() -> Value {
        Value::Array(vec![
            json!({ "name": "id", "dataType": "int", "nullable": false, "isPrimaryKey": true, "defaultValue": null, "comment": null }),
            json!({ "name": "name", "dataType": "text", "nullable": true, "isPrimaryKey": false, "defaultValue": null, "comment": null }),
        ])
    }

    /// A DbBrowser with an in-memory table, mirroring the Node suite's stubBrowser.
    /// The databases field opts the stub into the SPEC §data.databases catalog route: None keeps
    /// the trait DEFAULT (the empty catalog — exactly what a stub that never heard of
    /// the route must serve), Some answers as the test needs.
    struct StubDb {
        seen: SeenRef,
        databases: Option<Result<Value, String>>,
    }

    #[async_trait]
    impl DbBrowser for StubDb {
        fn dialect(&self) -> swiss_host::dbbrowser::DbDialect {
            swiss_host::dbbrowser::DbDialect::Mysql
        }
        fn label(&self) -> String {
            "stub @ localhost".into()
        }
        async fn list_tables(&self, o: &Value) -> Result<Value, String> {
            // The real browsers vet sort/dir inside listTables (mysql_browser/pg_browser); the
            // stub does the same, so the route tests see the same 400 a bad key earns live.
            swiss_host::dbbrowser::browse_table_sort(
                o.get("sort").and_then(Value::as_str),
                o.get("dir").and_then(Value::as_str),
            )?;
            if let Ok(mut seen) = self.seen.lock() {
                seen.tables_opts = Some(o.clone());
            }
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
                // SPEC §data.browse: the adapter fetched limit+1 and truncated; the flag is what the
                // panel's next-page arrow listens to.
                "nextPage": false,
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
        async fn apply_edits(&self, o: &Value) -> Result<Value, swiss_host::dbbrowser::EditError> {
            let edits = o.get("edits").cloned().unwrap_or(Value::Array(vec![]));
            if let Ok(mut seen) = self.seen.lock() {
                seen.edits = Some(edits.clone());
            }
            // SPEC §data.edits: the optimistic lock needs a stand-in answer too — an update
            // that carries a "__conflict__" change (an array of column names) plays the
            // loser of the race, so the route test can pin the 409 shape the panel paints
            // red cells from.
            for e in edits.as_array().unwrap_or(&vec![]) {
                if e.get("op").and_then(Value::as_str) == Some("update") {
                    if let Some(cols) = e.pointer("/changes/__conflict__").and_then(Value::as_array)
                    {
                        return Err(swiss_host::dbbrowser::EditError::Conflict(
                            swiss_host::dbbrowser::EditConflict {
                                message: "row was changed by another writer — columns: "
                                    .to_string()
                                    + &cols
                                        .iter()
                                        .map(|c| c.as_str().unwrap_or_default())
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                columns: cols
                                    .iter()
                                    .map(|c| c.as_str().unwrap_or_default().to_string())
                                    .collect(),
                                row: e.get("pk").and_then(Value::as_object).cloned(),
                            },
                        ));
                    }
                }
            }
            let results: Vec<Value> = edits
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|e| {
                    // SPEC §data.edits: the adapter reads every committed update/insert back in the
                    // same transaction and adds "row" — a delete stays null. The stub mirrors
                    // that shape so the route test can pin that the field survives the route.
                    let op = e.get("op").and_then(Value::as_str).unwrap_or("");
                    json!({
                        "op": op,
                        "affected": 1,
                        "row": if op == "delete" {
                            Value::Null
                        } else {
                            json!({ "id": 1, "name": "tru" })
                        },
                    })
                })
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
            if let Ok(mut seen) = self.seen.lock() {
                seen.export_opts = Some(o.clone());
            }
            let json_out = o.get("format") == Some(&json!("json"));
            Ok(json!({
                "format": if json_out { "json" } else { "csv" },
                "columns": ["id", "name"],
                "rows": 2,
                "capped": false,
                "body": if json_out { "{\"id\":1}\n{\"id\":2}" } else { "id,name\r\n1,a" },
            }))
        }
        async fn export_sql_dump(
            &self,
            o: &Value,
        ) -> Result<swiss_host::dbbrowser::SqlDump, String> {
            use swiss_host::dbbrowser::{
                export_row_limit, sql_dump_foot, sql_dump_head, sql_literal, SqlInsertBatch,
            };
            if let Ok(mut seen) = self.seen.lock() {
                seen.export_opts = Some(o.clone());
            }
            // A wide synthetic table driven through the REAL batching logic: 400 rows of
            // ~6 KB literals force the 1 MB threshold several times, so the route test can
            // prove the body arrived as multiple pieces (and multiple statements) instead of
            // one folded string. The peak the producer holds is one statement, never the 2.4 MB
            // the whole table would cost folded.
            const TOTAL: i64 = 400;
            let cap = export_row_limit(o.get("limit"));
            let rows = TOTAL.min(cap);
            let capped = TOTAL > rows;
            let head = sql_dump_head(
                swiss_host::dbbrowser::DbDialect::Mysql,
                Some("app"),
                "stub",
                "CREATE TABLE `stub` (\n  `id` int NOT NULL,\n  `pad` text\n)",
            )?;
            let columns = vec!["id".to_string(), "pad".to_string()];
            let dump_columns = columns.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<DumpPiece>(2);
            let seen_ref = self.seen.clone();
            tokio::spawn(async move {
                let mut pieces = 0usize;
                let mut batch = SqlInsertBatch::new(
                    swiss_host::dbbrowser::DbDialect::Mysql,
                    Some("app"),
                    "stub",
                    &columns,
                )
                .expect("stub identifiers are legal");
                if tx.send(Ok(head.into_bytes())).await.is_err() {
                    return;
                }
                pieces += 1;
                for i in 0..rows {
                    let row = json!({ "id": i, "pad": "x".repeat(6_000) });
                    let literals: Vec<String> = columns
                        .iter()
                        .map(|c| sql_literal(row.get(c.as_str())))
                        .collect();
                    if let Some(stmt) = batch.push(&format!("({})", literals.join(", "))) {
                        if tx.send(Ok(stmt.into_bytes())).await.is_err() {
                            return;
                        }
                        pieces += 1;
                    }
                }
                if let Some(tail) = batch.finish() {
                    if tx.send(Ok(tail.into_bytes())).await.is_err() {
                        return;
                    }
                    pieces += 1;
                }
                if tx
                    .send(Ok(sql_dump_foot(swiss_host::dbbrowser::DbDialect::Mysql)
                        .as_bytes()
                        .to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
                pieces += 1;
                // Written before the sender drops (the receiver's None), so a consumer that
                // drained the body always sees the final count.
                if let Ok(mut seen) = seen_ref.lock() {
                    seen.dump_pieces = pieces;
                }
            });
            Ok(swiss_host::dbbrowser::SqlDump {
                columns: dump_columns,
                rows,
                capped,
                body: rx,
            })
        }
        async fn import_table(&self, o: &Value) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.imported = Some(json!({
                    "header": o.get("header"),
                    "lines": o.get("lines"),
                    "mapping": o.get("mapping"),
                    "mode": o.get("mode"),
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
            // SPEC §data.ddl: the stub mirrors the real adapters — the create ops build through
            // the shared builder, so the route test can pin preview == commit byte-for-byte.
            let op = o.get("op").and_then(Value::as_str).unwrap_or("");
            if matches!(op, "create_table" | "add_column" | "create_index") {
                let stmts = swiss_host::dbbrowser::build_ddl_create(
                    swiss_host::dbbrowser::DbDialect::Mysql,
                    op,
                    o,
                )?;
                return Ok(json!({ "ran": swiss_host::dbbrowser::ddl_script(&stmts) }));
            }
            let to = o.get("to").and_then(Value::as_str).unwrap_or("");
            if o.get("op") == Some(&json!("rename")) && to.is_empty() {
                return Err("rename needs the new table name".into());
            }
            Ok(
                json!({ "ran": format!("stub {}", o.get("op").and_then(Value::as_str).unwrap_or("")) }),
            )
        }
        async fn activity(&self) -> Result<Value, String> {
            Ok(json!({ "rows": [
                { "pid": 101, "user": "app", "state": "active", "wait": "", "seconds": 4, "query": "SELECT pg_sleep(60)", "own": false, "blockedBy": null },
                { "pid": 102, "user": "root", "state": "idle", "wait": "", "seconds": 0, "query": "", "own": true, "blockedBy": null },
            ] }))
        }
        async fn activity_kill(&self, pid: i64, terminate: bool) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.activity_kill = Some((pid, terminate));
            }
            Ok(json!({ "ok": true }))
        }
        async fn completion(&self, sql: &str, caret: usize) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.completion = Some((sql.to_string(), caret));
            }
            Ok(json!({ "items": [
                { "label": "users", "kind": "table", "detail": "table" },
                { "label": "UNION", "kind": "keyword", "detail": "keyword" },
            ] }))
        }
        async fn list_databases(&self) -> Result<Value, String> {
            match &self.databases {
                Some(r) => r.clone(),
                // The trait default in the flesh: a db stub that never heard of the route
                // still answers, with the empty catalog.
                None => Ok(json!({ "primary": null, "current": null, "databases": [] })),
            }
        }
    }

    /// The Node suite's fake redis browser.
    struct StubRedis {
        seen: SeenRef,
        /// SPEC §data.databases: the redis flavor's catalog answer for the databases route; None
        /// keeps the trait default (empty catalog).
        databases: Option<Value>,
    }

    #[async_trait]
    impl RedisBrowser for StubRedis {
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
        async fn run_pipeline(&self, commands: &[Vec<String>]) -> Result<Value, String> {
            // The adapter vets every command through the console guard BEFORE the socket; the
            // stub stands for the far side of that and answers one reply per command.
            if let Ok(mut seen) = self.seen.lock() {
                seen.pipeline = Some(commands.to_vec());
            }
            let replies: Vec<Value> = commands.iter().map(|c| json!(c.len())).collect();
            Ok(json!({ "replies": replies }))
        }
        async fn read_stream(&self, key: &str, o: &Value) -> Result<Value, String> {
            // The same host helper the real browser runs: the stub validates exactly
            // what the real one validates, so the route's 400s are pinned here
            // without a redis (SPEC §data.streams).
            swiss_host::dbbrowser::redis_stream_opts(o)?;
            if let Ok(mut seen) = self.seen.lock() {
                seen.stream_opts = Some(o.clone());
            }
            Ok(json!({
                "key": key, "type": "stream", "length": 3,
                "firstId": "1-1", "lastId": "3-3",
                "entries": [
                    { "id": "3-3", "ts": "2023-11-14T22:13:20.300Z", "fields": { "f": "c" } },
                    { "id": "2-2", "ts": "2023-11-14T22:13:20.200Z", "fields": { "f": "b" } },
                    { "id": "1-1", "ts": "2023-11-14T22:13:20.100Z", "fields": { "f": "a" } },
                ],
                "columns": ["f"], "more": false,
            }))
        }
        async fn stream_groups(&self, key: &str) -> Result<Value, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.stream_groups_key = Some(key.to_string());
            }
            Ok(json!({ "key": key, "groups": [
                { "name": "feed", "consumers": 1, "pending": 7, "lag": 2, "last-delivered-id": "1-1" },
            ] }))
        }
        async fn list_databases(&self) -> Result<Value, String> {
            match &self.databases {
                Some(v) => Ok(v.clone()),
                // The trait default in the flesh: a redis stub that never heard of the
                // route still answers, with the empty catalog.
                None => Ok(json!({ "primary": null, "current": null, "databases": [] })),
            }
        }
    }

    /// One row as the stub provider serves it — the old seam's shape, moved provider-side.
    struct StubRow {
        name: String,
        adapter_type: String,
        state: String,
        browser: BrowserFlavor,
    }

    fn flavor_clone(f: &BrowserFlavor) -> BrowserFlavor {
        match f {
            BrowserFlavor::Db(db) => BrowserFlavor::Db(db.clone()),
            BrowserFlavor::Redis(rb) => BrowserFlavor::Redis(rb.clone()),
            BrowserFlavor::None => BrowserFlavor::None,
        }
    }

    fn row_facts(row: &StubRow) -> (String, String) {
        match &row.browser {
            BrowserFlavor::Db(db) => (db.dialect().as_str().to_string(), db.label()),
            BrowserFlavor::Redis(rb) => ("redis".into(), rb.label()),
            BrowserFlavor::None => ("none".into(), row.name.clone()),
        }
    }

    /// The provider the tests browse through: rows list and lease exactly the way the real
    /// RegistryCatalog does (same messages, same dialect mapping), over the Node suite's
    /// stub browsers. Leases are backed by one tracker per router.
    struct StubProvider {
        rows: Vec<Arc<StubRow>>,
        tracker: Arc<LeaseTracker>,
    }

    impl ConnectionCatalog for StubProvider {
        fn list(&self) -> Vec<ConnectionInfo> {
            self.rows
                .iter()
                .map(|row| {
                    let (dialect, label) = row_facts(row);
                    ConnectionInfo {
                        id: row.name.clone(),
                        label,
                        dialect,
                        state: row.state.clone(),
                    }
                })
                .collect()
        }

        fn lease(
            &self,
            id: &str,
            holder: &str,
        ) -> Result<ConnectionLease, swiss_host::services::catalog::CatalogError> {
            use swiss_host::services::catalog::CatalogError;
            let Some(row) = self.rows.iter().find(|r| r.name == id) else {
                return Err(CatalogError::Unknown(format!("unknown MCP: {id}")));
            };
            if matches!(row.browser, BrowserFlavor::None) {
                return Err(CatalogError::NotBrowsable(format!(
                    "MCP '{}' ({}) has no database to browse",
                    id, row.adapter_type
                )));
            }
            let (dialect, _) = row_facts(row);
            Ok(self
                .tracker
                .grant(id, holder, &dialect, flavor_clone(&row.browser)))
        }
    }

    /// Build a router over the given connections (the Node suite's setup() + registry.register),
    /// served by a stub catalog provider registered as "mcp".
    fn router_of(rows: Vec<Arc<StubRow>>) -> Router<()> {
        catalog_router_of(|tracker| StubProvider { rows, tracker })
    }

    /// router_of plus a sidebar order (Node's PUT /api/order list).
    fn router_ordered_of(rows: Vec<Arc<StubRow>>, order: Vec<String>) -> Router<()> {
        catalog_ordered_router_of(|tracker| StubProvider { rows, tracker }, order)
    }

    /// Register a provider as "mcp" and mount the router over it — the composition the
    /// real app performs (MCP plugin provides, build_app mounts).
    fn catalog_router_of(
        provider: impl FnOnce(Arc<LeaseTracker>) -> StubProvider + 'static,
    ) -> Router<()> {
        catalog_ordered_router_of(provider, Vec::new())
    }

    /// Same, with a sidebar order to rank the picker by (Node's PUT /api/order list).
    fn catalog_ordered_router_of(
        provider: impl FnOnce(Arc<LeaseTracker>) -> StubProvider + 'static,
        order: Vec<String>,
    ) -> Router<()> {
        catalog_grouped_router_of(provider, order, Vec::new())
    }

    /// Same, with the name -> group labels the composition reads from the store (SPEC §host.groups).
    fn catalog_grouped_router_of(
        provider: impl FnOnce(Arc<LeaseTracker>) -> StubProvider + 'static,
        order: Vec<String>,
        groups: Vec<(String, String)>,
    ) -> Router<()> {
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(Arc::new(provider(LeaseTracker::new())), "mcp")
            .expect("test provider registers");
        dbbrowser_router(
            catalog,
            Arc::new(move || order.clone()),
            Arc::new(move |n: &str| {
                groups
                    .iter()
                    .find(|(name, _)| name == n)
                    .map(|(_, g)| g.clone())
                    .unwrap_or_else(|| "default".to_string())
            }),
        )
    }

    /// The catalog WITHOUT a provider — the MCP-disabled composition.
    fn empty_router_after_mcp() -> (Router<()>, Arc<CatalogRegistry>) {
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(
                Arc::new(StubProvider {
                    rows: vec![],
                    tracker: LeaseTracker::new(),
                }),
                "mcp",
            )
            .expect("test provider registers");
        // The provider's stop choreography: withdraw, drain (nothing out), clear.
        catalog.begin_withdraw();
        catalog.clear();
        let router = dbbrowser_router(
            catalog.clone(),
            Arc::new(Vec::new),
            Arc::new(|_| "default".to_string()),
        );
        (router, catalog)
    }

    fn db_entry(name: &str, browser: Arc<dyn DbBrowser>) -> Arc<StubRow> {
        Arc::new(StubRow {
            name: name.into(),
            adapter_type: "mysql".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Db(browser),
        })
    }

    fn plain_entry(name: &str) -> Arc<StubRow> {
        Arc::new(StubRow {
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
                    databases: None,
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
    async fn rows_carry_their_group_in_visual_order() {
        // SPEC §host.groups: /api/db rows label each connection with the sidebar group it renders
        // under, and the picker's order stays the VISUAL order the composition passes (groups
        // in stored order, members by flat rank inside them) - the label rides the row, it
        // never re-sorts anything.
        let stub = || {
            Arc::new(StubDb {
                seen: Arc::new(Mutex::new(Seen::default())),
                databases: None,
            }) as Arc<dyn DbBrowser>
        };
        let rows = vec![
            db_entry("alpha-conn", stub()),
            db_entry("beta-conn", stub()),
        ];
        // The visual order the composition computes: beta's group is stored first.
        let order = vec!["beta-conn".into(), "alpha-conn".into()];
        let groups = vec![
            ("alpha-conn".to_string(), "Alpha".to_string()),
            ("beta-conn".to_string(), "Beta".to_string()),
        ];
        let app =
            catalog_grouped_router_of(|tracker| StubProvider { rows, tracker }, order, groups);
        let (_, _, body, _) = call(app, "GET", "/api/db", None).await;
        let body = body.expect("json");
        let conns = body["connections"].as_array().expect("connections");
        let seen: Vec<(String, String)> = conns
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().expect("name").to_string(),
                    c["group"]
                        .as_str()
                        .expect("the row carries its group")
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("beta-conn".to_string(), "Beta".to_string()),
                ("alpha-conn".to_string(), "Alpha".to_string()),
            ],
            "visual order first, the group labelling it"
        );
    }

    #[tokio::test]
    async fn the_picker_ranks_by_the_sidebars_manual_order() {
        let stub = || {
            Arc::new(StubDb {
                seen: Arc::new(Mutex::new(Seen::default())),
                databases: None,
            }) as Arc<dyn DbBrowser>
        };
        let rows = vec![
            db_entry("db-b", stub()),
            db_entry("db-a", stub()),
            db_entry("db-c", stub()),
        ];
        // No manual order yet — plain name order, like /api/mcps.
        let app = router_of(rows.clone());
        let (_, _, body, _) = call(app, "GET", "/api/db", None).await;
        fn names(b: &Value) -> Vec<String> {
            b["connections"]
                .as_array()
                .expect("connections")
                .iter()
                .map(|c| c["name"].as_str().expect("name").to_string())
                .collect()
        }
        assert_eq!(names(&body.expect("json")), vec!["db-a", "db-b", "db-c"]);
        // The order a sidebar drag persists reorders the picker; a name the order never
        // covers (db-b, never dragged) keeps its name-order slot at the end.
        let app = router_ordered_of(rows, vec!["db-c".into(), "db-a".into(), "db-never".into()]);
        let (_, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(names(&body.expect("json")), vec!["db-c", "db-a", "db-b"]);
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
                databases: None,
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
                databases: None,
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
        // SPEC §data.browse: the page reply carries the nextPage probe alongside the COUNT total.
        assert_eq!(body["nextPage"], false);
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
    async fn forwards_the_tables_sort_and_rejects_unknown_keys() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, _, _) = call(
            app,
            "GET",
            "/api/db/db/tables?page=0&limit=2000&sort=rows&dir=desc",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let opts = seen.lock().expect("seen").tables_opts.take();
        assert_eq!(
            opts,
            Some(json!({ "page": "0", "limit": "2000", "sort": "rows", "dir": "desc" }))
        );

        // A nonsense key or direction is the caller's whole mistake — 400, not a silent default.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/tables?sort=evil", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("unknown key"), "{err}");
    }

    #[tokio::test]
    async fn forwards_the_schema_filter_to_the_table_list() {
        // SPEC §data.browse: the schema picker rides the /tables request as a plain param; MySQL
        // ignores it (one database), Postgres narrows the catalog walk to that schema.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, _, _) = call(app, "GET", "/api/db/db/tables?page=0&schema=app", None).await;
        assert_eq!(status, StatusCode::OK);
        let opts = seen.lock().expect("seen").tables_opts.take();
        assert_eq!(opts, Some(json!({ "page": "0", "schema": "app" })));
    }

    #[tokio::test]
    async fn forwards_filters_and_rejects_malformed_ones() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
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
                databases: None,
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
                databases: None,
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
                databases: None,
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
                databases: None,
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
        assert_eq!(headers["x-export-format"].to_str().unwrap(), "csv");
        assert!(text.contains("id,name"));

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
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
    async fn an_export_carries_the_grids_filters_to_the_browser() {
        // SPEC §data.export: the grid's filters ride /export exactly as they ride /data, so the
        // download and the grid describe the same filtered set (and x-export-rows counts it).
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let terms = json!([{ "column": "name", "op": "like", "value": "al" }]).to_string();
        let (status, _, _, _) = call(
            app,
            "GET",
            &format!(
                "/api/db/db/export?table=users&format=csv&filters={}",
                urlencode(&terms)
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let opts = seen
            .lock()
            .expect("seen")
            .export_opts
            .take()
            .expect("export opts recorded");
        assert_eq!(
            opts["filters"],
            json!([{ "column": "name", "op": "like", "value": "al" }])
        );
        assert_eq!(opts["table"], "users");

        // Malformed filters are the route's own 400, same rule as /data.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/db/export?table=users&filters=not-json",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("JSON array"), "{err}");
    }

    #[tokio::test]
    async fn a_sql_export_streams_a_replayable_dump_in_pieces() {
        // SPEC §data.export: format=sql answers a dump — CREATE TABLE head, the dialect's FK
        // stance, then the rows as ~1 MB multi-value INSERTs — streamed as several body
        // pieces instead of one folded string, so the producer never holds the whole table.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, headers, _, text) =
            call(app, "GET", "/api/db/db/export?table=users&format=sql", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-export-format"].to_str().unwrap(), "sql");
        assert_eq!(headers["content-type"], "application/sql; charset=utf-8");
        assert!(headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename=\"sql-users\""));
        assert_eq!(headers["x-export-rows"].to_str().unwrap(), "400");
        assert_eq!(headers["x-export-capped"].to_str().unwrap(), "0");
        assert!(text.starts_with("-- swiss SQL dump\n"), "{text:.120}");
        assert!(text.contains("SET FOREIGN_KEY_CHECKS=0;\n"));
        assert!(text.contains("CREATE TABLE `stub`"));
        assert!(text.ends_with("SET FOREIGN_KEY_CHECKS=1;\n"));
        // 400 rows of ~6 KB cannot ride one statement under the 1 MB threshold: the dump
        // folds them into several INSERTs, and the channel carried them as several pieces
        // (head, each statement, foot) — the streaming the folded formats cannot offer.
        assert!(text.len() > 2 * swiss_host::dbbrowser::EXPORT_SQL_MAX_PACKET);
        let statements = text.matches("INSERT INTO `app`.`stub`").count();
        assert!(
            statements >= 2,
            "{statements} statements — the 1 MB threshold should have split them"
        );
        for stmt in text.split("INSERT INTO `app`.`stub`").skip(1) {
            assert!(
                stmt.len() <= swiss_host::dbbrowser::EXPORT_SQL_MAX_PACKET + 10,
                "a statement crossed the flush threshold"
            );
        }
        assert_eq!(text.matches("\n(").count(), 400);
        let pieces = seen.lock().expect("seen").dump_pieces;
        assert!(
            pieces >= statements + 2,
            "head and foot ship as pieces too: {pieces}"
        );
        let opts = seen
            .lock()
            .expect("seen")
            .export_opts
            .clone()
            .expect("export opts recorded");
        assert_eq!(opts["format"], json!("sql"));
        assert_eq!(opts["table"], "users");

        // The grid's filters ride the sql arm exactly as they ride csv (SPEC §data.export): the
        // WHERE the dump streams is the WHERE the page counted.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let terms = json!([{ "column": "pad", "op": "like", "value": "x" }]).to_string();
        let (status, _, _, _) = call(
            app,
            "GET",
            &format!(
                "/api/db/db/export?table=users&format=sql&filters={}",
                urlencode(&terms)
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen.lock()
                .expect("seen")
                .export_opts
                .as_ref()
                .expect("export opts recorded")["filters"],
            json!([{ "column": "pad", "op": "like", "value": "x" }])
        );

        // The row cap (SPEC §data.export): EXPORT_ROW_CAP stays the ceiling; a lower limit
        // truncates the dump and flags it, exactly as the folded formats do.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, headers, _, text) = call(
            app,
            "GET",
            "/api/db/db/export?table=users&format=sql&limit=50",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-export-rows"].to_str().unwrap(), "50");
        assert_eq!(headers["x-export-capped"].to_str().unwrap(), "1");
        assert_eq!(text.matches("\n(").count(), 50);
    }

    #[tokio::test]
    async fn imports_csv_rows_and_rejects_malformed_payloads() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
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
                databases: None,
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
                databases: None,
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
    async fn an_import_can_ask_for_upsert_and_refuses_unknown_modes() {
        // SPEC §data.export: mode "upsert" rides the payload to the browser; anything but the two
        // known spellings is the route's own 400, the same rule ddl's op follows.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({
                "table": "users",
                "header": ["id", "name"],
                "lines": ["1,alice", "2,bob"],
                "mapping": ["id", "name"],
                "mode": "upsert"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json"), json!({ "inserted": 2 }));
        assert_eq!(
            seen.lock()
                .expect("seen")
                .imported
                .take()
                .expect("imported")["mode"],
            json!("upsert")
        );

        // The default stays the default: no mode field reaches the browser at all, so an
        // old panel riding a new backend keeps byte-identical import requests.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, _, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({
                "table": "users",
                "header": ["id"],
                "lines": ["1"],
                "mapping": ["id"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(seen
            .lock()
            .expect("seen")
            .imported
            .take()
            .expect("imported")["mode"]
            .is_null());

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/import",
            Some(json!({
                "table": "users",
                "header": ["id"],
                "lines": ["1"],
                "mapping": ["id"],
                "mode": "merge"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("mode must be insert or upsert"), "{err}");
    }

    #[tokio::test]
    async fn runs_structure_operations_and_rejects_unknown_ops() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
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
                databases: None,
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

    // --- SPEC §data.ddl: preview and commit share one builder -------------------------------------------

    /// The body both W4.6 endpoints take: (op, payload). The stub browser builds the create
    /// ops the way the real adapters do (the shared builder), so this pins the contract the
    /// sheet relies on: /ddl's "ran" is byte-for-byte /ddl-preview's "sql".
    #[tokio::test]
    async fn ddl_preview_and_ddl_run_the_same_text() {
        let payload = json!({
            "schema": "app",
            "table": "cfg",
            "columns": [
                { "name": "order", "type": "int", "nullable": false, "comment": "序号" }
            ]
        });
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl-preview",
            Some(json!({ "op": "create_table", "payload": payload.clone() })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let sql = body.expect("json")["sql"].as_str().unwrap().to_string();
        assert_eq!(
            sql,
            "CREATE TABLE `app`.`cfg` (\n    `order` int NOT NULL COMMENT '序号'\n);"
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl",
            Some(json!({ "op": "create_table", "payload": payload })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // The commit echoes the exact text the preview showed — same builder, same input.
        assert_eq!(body.expect("json")["ran"].as_str().unwrap(), sql);
        // The route flattened (op, payload) into the browser contract's one flat object.
        assert_eq!(
            seen.lock().expect("seen").ddl.take(),
            Some(json!({
                "op": "create_table",
                "schema": "app",
                "table": "cfg",
                "columns": [
                    { "name": "order", "type": "int", "nullable": false, "comment": "序号" }
                ]
            }))
        );
    }

    #[tokio::test]
    async fn ddl_preview_validates_op_and_identifiers() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl-preview",
            Some(json!({ "op": "truncate", "payload": { "table": "users" } })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(
            err.contains("op must be create_table, add_column or create_index"),
            "{err}"
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/ddl-preview",
            Some(json!({ "op": "create_index", "payload": { "table": "t", "index": "x y", "columns": ["a"] } })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("not a valid MySQL identifier"), "{err}");
    }

    #[tokio::test]
    async fn ddl_preview_is_sql_only() {
        // redis connections have no W4.6 entry: the preview leases a Db browser and refuses.
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/cache/ddl-preview",
            Some(json!({ "op": "create_table", "payload": { "table": "t", "columns": [{ "name": "a", "type": "int" }] } })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let err = body.expect("json")["error"].as_str().unwrap().to_string();
        assert!(err.contains("has no database to browse"), "{err}");
    }

    #[tokio::test]
    async fn forwards_edit_batches_and_rejects_empty_ones() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
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
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/edits",
            Some(json!({ "table": "users", "edits": [{ "op": "update", "pk": { "id": 1 }, "changes": { "name": "x" } }] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // SPEC §data.edits: each result also carries the committed row as the adapter read it back
        // (same transaction) — the route passes the field through untouched.
        assert_eq!(
            body.expect("json")["results"],
            json!([{ "op": "update", "affected": 1, "row": { "id": 1, "name": "tru" } }])
        );
        assert_eq!(
            seen.lock().expect("seen").edits.take(),
            Some(json!([{ "op": "update", "pk": { "id": 1 }, "changes": { "name": "x" } }]))
        );
    }

    #[tokio::test]
    async fn a_lost_optimistic_lock_answers_409_with_the_column_names() {
        // SPEC §data.edits: the loser of the edit race answers 409 — not 400, the request was
        // well-formed and the row was real; another writer moved it. The body names the
        // columns whose buffered originals no longer match, which is what the grid paints
        // red while keeping the whole buffer for a retry.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/edits",
            Some(json!({
                "table": "users",
                "edits": [{
                    "op": "update",
                    "pk": { "id": 1, "name": "old", "city": "la" },
                    "changes": { "name": "new", "city": "sf", "__conflict__": ["name", "city"] }
                }]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let b = body.expect("json");
        let err = b["error"].as_str().unwrap().to_string();
        assert!(err.contains("name") && err.contains("city"), "{err}");
        assert_eq!(b["conflictColumns"], json!(["name", "city"]));
        // The buffered row the race was over rides along, so the grid finds the exact
        // entry to paint without guessing by column names.
        assert_eq!(b["row"], json!({ "id": 1, "name": "old", "city": "la" }));
    }

    #[tokio::test]
    async fn runs_the_console_reads_and_writes_alike() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
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

        // A write runs too: the console's only rule is one statement per run, and the route
        // forwards whatever the browser accepts.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/query",
            Some(json!({ "sql": "UPDATE t SET x = 1", "limit": 10 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["rowCount"], 1);
        assert_eq!(
            seen.lock().expect("seen").query.take(),
            Some("UPDATE t SET x = 1".to_string())
        );

        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
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

    fn redis_entry(name: &str) -> Arc<StubRow> {
        redis_entry_with(name, SeenRef::default())
    }

    /// redis_entry over a shared seen, so a test can read back what the pipeline route
    /// forwarded (SPEC §data.redis).
    fn redis_entry_with(name: &str, seen: SeenRef) -> Arc<StubRow> {
        Arc::new(StubRow {
            name: name.into(),
            adapter_type: "redis".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Redis(Arc::new(StubRedis { seen, databases: None })),
        })
    }

    #[tokio::test]
    async fn a_redis_structured_edit_commit_rides_one_pipeline() {
        // SPEC §data.redis: the typed value view's whole buffer posts as ONE round trip; args stay
        // separate strings (a value with spaces is one argument, never re-split), and the
        // replies come back in order.
        let seen = SeenRef::default();
        let app = router_of(vec![redis_entry_with("cache", seen.clone())]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/cache/redis-pipeline",
            Some(json!({ "commands": [["HSET", "h:1", "f", "v"], ["HDEL", "h:1", "gone"]] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["replies"], json!([4, 3]));
        assert_eq!(
            seen.lock().expect("seen").pipeline.take(),
            Some(vec![
                vec!["HSET".into(), "h:1".into(), "f".into(), "v".into()],
                vec!["HDEL".into(), "h:1".into(), "gone".into()],
            ])
        );
    }

    #[tokio::test]
    async fn a_redis_pipeline_body_is_shape_checked_at_the_route() {
        // A bare number stays a VALID argument (a score, a list index) — only values that
        // are neither string nor number are the caller's mistake.
        for bad in [
            json!({}),
            json!({ "commands": [] }),
            json!({ "commands": [[]] }),
            json!({ "commands": [["HSET", true]] }),
        ] {
            let app = router_of(vec![redis_entry("cache")]);
            let (status, _, body, _) = call(
                app,
                "POST",
                "/api/db/cache/redis-pipeline",
                Some(bad.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            let err = body.expect("json")["error"]
                .as_str()
                .expect("error text")
                .to_string();
            assert!(
                err.starts_with("commands must be") || err.starts_with("each command must be"),
                "{bad} -> {err}"
            );
        }
    }

    #[tokio::test]
    async fn completion_forwards_the_console_text_and_caret() {
        // SPEC §data.completion: the caret is a byte offset; the panel converts its UTF-16 index.
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }) as Arc<dyn DbBrowser>,
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/completion",
            Some(json!({ "sql": "SELECT * FROM users WHERE u", "caret": 28 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = body.expect("json")["items"]
            .as_array()
            .expect("items")
            .clone();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["kind"], "table");
        assert_eq!(items[1]["kind"], "keyword");
        // The caret sent (28) sits one past the text's end — the route clamps it to the
        // text's own length rather than passing a pointer at nothing.
        assert_eq!(
            seen.lock().expect("seen").completion,
            Some(("SELECT * FROM users WHERE u".to_string(), 27))
        );
    }

    #[tokio::test]
    async fn completion_validates_the_body_before_touching_the_browser() {
        let seen = SeenRef::default();
        let make = || {
            router_of(vec![db_entry(
                "db",
                Arc::new(StubDb { seen: seen.clone(), databases: None }) as Arc<dyn DbBrowser>,
            )])
        };
        for bad in [
            json!({ "caret": 3 }),
            json!({ "sql": 5, "caret": 3 }),
            json!(null),
        ] {
            let (status, _, body, _) =
                call(make(), "POST", "/api/db/db/completion", Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body.expect("json")["error"], "sql is required");
        }
        assert!(seen.lock().expect("seen").completion.is_none());
    }

    #[tokio::test]
    async fn completion_is_sql_only() {
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/cache/completion",
            Some(json!({ "sql": "GE", "caret": 2 })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'cache' (redis) has no database to browse"
        );
    }

    #[tokio::test]
    async fn the_activity_page_lists_sessions_in_the_shared_shape() {
        // SPEC §data.activity: one reply shape for both dialects — the panel renders one table.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }) as Arc<dyn DbBrowser>,
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/activity", None).await;
        assert_eq!(status, StatusCode::OK);
        let rows = body.expect("json")["rows"]
            .as_array()
            .expect("rows")
            .clone();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["pid"], json!(101));
        assert_eq!(rows[0]["query"], "SELECT pg_sleep(60)");
        assert_eq!(rows[1]["own"], json!(true));
    }

    #[tokio::test]
    async fn an_activity_kill_validates_mode_and_pid_before_touching_the_browser() {
        let seen = SeenRef::default();
        let make = || {
            router_of(vec![db_entry(
                "db",
                Arc::new(StubDb { seen: seen.clone(), databases: None }) as Arc<dyn DbBrowser>,
            )])
        };
        for bad in [
            json!({ "pid": 5 }),
            json!({ "pid": 5, "mode": "kill" }),
            json!({ "pid": 5, "mode": "CANCEL" }),
            json!({ "mode": "cancel" }),
            json!({ "pid": 0, "mode": "cancel" }),
            json!({ "pid": -3, "mode": "terminate" }),
            json!({ "pid": "7", "mode": "cancel" }),
        ] {
            let (status, _, body, _) = call(
                make(),
                "POST",
                "/api/db/db/activity-kill",
                Some(bad.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            let err = body.expect("json")["error"]
                .as_str()
                .expect("error")
                .to_string();
            assert!(
                err == "mode must be cancel or terminate"
                    || err == "pid must be a session id (positive integer)",
                "{bad} -> {err}"
            );
        }
        assert!(seen.lock().expect("seen").activity_kill.is_none());
    }

    #[tokio::test]
    async fn an_activity_kill_forwards_the_mode_word_to_the_browser() {
        let seen = SeenRef::default();
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb { seen: seen.clone(), databases: None }) as Arc<dyn DbBrowser>,
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/activity-kill",
            Some(json!({ "pid": 4242, "mode": "terminate" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json")["ok"], json!(true));
        assert_eq!(seen.lock().expect("seen").activity_kill, Some((4242, true)));
    }

    #[tokio::test]
    async fn the_activity_page_is_sql_only() {
        // A redis connection has no sessions page: the 404 names what it is, as ever.
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/activity", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'cache' (redis) has no database to browse"
        );
    }

    #[tokio::test]
    async fn the_redis_pipeline_is_redis_only() {
        // A SQL connection answers the redis 404 with the adapter type named, exactly as the
        // key routes do — no half-work on the wrong flavour.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }) as Arc<dyn DbBrowser>,
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/redis-pipeline",
            Some(json!({ "commands": [["SET", "k", "v"]] })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'db' (mysql) is not a redis connection"
        );
    }

    #[tokio::test]
    async fn serves_the_redis_key_browser() {
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::OK);
        let row = body.expect("json")["connections"][0].clone();
        assert_eq!(row["dialect"], "redis");
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
    async fn stream_routes_validate_forward_and_stay_redis_only() {
        // SPEC §data.streams route row: the five parameter cases, forwarding recorded, groups
        // served, and the routes staying redis-only — a db connection 404s them with
        // the same wording as every other redis route. Reads are never audited, so
        // there is no log line to assert, only this route's silence in the code.
        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/rdb/stream", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.expect("json")["error"], "key is required");

        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/rdb/stream?key=s&before=1-0&after=2-0",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"].as_str().expect("text").to_string();
        assert!(err.contains("opposite directions"), "{err}");

        for bad in ["abc", "0"] {
            let app = router_of(vec![redis_entry("rdb")]);
            let (status, _, _, _) = call(
                app,
                "GET",
                &format!("/api/db/rdb/stream?key=s&count={bad}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "count={bad}");
        }

        let seen = SeenRef::default();
        let app = router_of(vec![redis_entry_with("rdb", seen.clone())]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/rdb/stream?key=stream:ticks&after=1700000999900-0&count=40",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["type"], "stream");
        assert_eq!(body["entries"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["columns"], json!(["f"]));
        assert_eq!(body["more"], json!(false));
        assert_eq!(
            seen.lock().expect("seen").stream_opts.clone().expect("forwarded"),
            json!({ "after": "1700000999900-0", "count": "40" })
        );

        // SPEC §data.streams: the filter rides the same option object, and a filter line
        // that cannot be split is the caller's mistake — refused before the lease,
        // like every other bad parameter on this route.
        let seen = SeenRef::default();
        let app = router_of(vec![redis_entry_with("rdb", seen.clone())]);
        let (status, _, _, _) = call(
            app,
            "GET",
            "/api/db/rdb/stream?key=stream:ticks&match=symbol%3DNVDA",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen.lock()
                .expect("seen")
                .stream_opts
                .clone()
                .expect("forwarded"),
            json!({ "match": "symbol=NVDA" })
        );

        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/rdb/stream?key=s&match=symbol%3D%22NVDA",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body.expect("json")["error"]
            .as_str()
            .expect("text")
            .to_string();
        assert!(err.contains("unbalanced quote"), "{err}");

        let seen = SeenRef::default();
        let app = router_of(vec![redis_entry_with("rdb", seen.clone())]);
        let (status, _, body, _) = call(
            app,
            "GET",
            "/api/db/rdb/stream/groups?key=stream:ticks",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let groups = body.expect("json")["groups"].clone();
        assert_eq!(groups[0]["name"], json!("feed"));
        assert_eq!(groups[0]["pending"], json!(7));
        assert_eq!(
            seen.lock().expect("seen").stream_groups_key.clone().expect("forwarded"),
            "stream:ticks"
        );

        // SQL-only symmetry: a db connection 404s the stream routes rather than
        // half-working.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/stream?key=s", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'db' (mysql) is not a redis connection"
        );
    }

    #[tokio::test]
    async fn a_query_answer_carries_how_long_it_took() {
        // SPEC §data.console: the console's meta line shows server-measured elapsed time. The clock
        // wraps the browser call itself, so the number is the driver round trip the user
        // actually waited on, not the JSON hop around it.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/db/query",
            Some(json!({ "sql": "SELECT 1" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        let ms = body["elapsedMs"].as_u64().expect("elapsedMs is an integer");
        assert!(ms < 60_000, "{ms} — a stub call cannot take a minute");
        // The reply itself passes through untouched.
        assert_eq!(body["rowCount"], json!(1));
        assert!(body["columns"].is_array());
    }

    #[tokio::test]
    async fn a_redis_command_answer_carries_how_long_it_took() {
        // Same contract on the redis console: {"reply": ..., "elapsedMs": N}.
        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "GET mykey" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = body.expect("json");
        assert_eq!(body["reply"], json!("value"));
        let ms = body["elapsedMs"].as_u64().expect("elapsedMs is an integer");
        assert!(ms < 60_000, "{ms}");
    }

    #[tokio::test]
    async fn runs_a_redis_command_reads_and_writes_alike() {
        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, body, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "GET mykey" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // SPEC §data.console grew the answer by one field; the reply itself is unchanged.
        let body = body.expect("json");
        assert_eq!(body["reply"], json!("value"));
        assert!(body["elapsedMs"].is_u64());

        let app = router_of(vec![redis_entry("rdb")]);
        let (_status, _, body, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "LRANGE mylist 0 -1" })),
        )
        .await;
        assert_eq!(body.expect("json")["reply"], json!(["a", "b"]));

        // A write runs too: there is no readonly gate anywhere in the console.
        let app = router_of(vec![redis_entry("rdb")]);
        let (status, _, _, _) = call(
            app,
            "POST",
            "/api/db/rdb/command",
            Some(json!({ "command": "SET k v" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

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
    async fn non_sql_kinds_answer_the_flavored_404s() {
        // The flavored lookups name what the MCP is NOT: the cross-dialect case is a redis
        // connection on an SQL route. (A document-store flavour once had /collections and
        // /docs routes here - ADR-012 - and those now fall to the router's 404.)
        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/tables", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'cache' (redis) has no database to browse"
        );

        let app = router_of(vec![redis_entry("cache")]);
        let (status, _, _body, _) = call(
            app,
            "POST",
            "/api/db/cache/query",
            Some(json!({ "sql": "SELECT 1" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn no_provider_is_a_503_that_names_the_missing_plugin() {
        // SPEC §host.seats's MCP-disabled case: the message names the provider that IS the
        // connection source, so the panel can say WHO to enable instead of "no rows".
        let (app, _catalog) = empty_router_after_mcp();
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body.expect("json")["error"],
            "the mcp plugin provides database connections and is currently disabled"
        );

        // Every sub-route answers the same honest 503, not a per-route 404 masquerade.
        let (app, _catalog) = empty_router_after_mcp();
        let (status, _, body, _) = call(app, "GET", "/api/db/db/tables", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body.expect("json")["error"],
            "the mcp plugin provides database connections and is currently disabled"
        );
    }

    #[tokio::test]
    async fn a_provider_that_never_registered_is_a_generic_503() {
        let catalog = Arc::new(CatalogRegistry::new());
        let app = dbbrowser_router(
            catalog,
            Arc::new(Vec::new),
            Arc::new(|_| "default".to_string()),
        );
        let (status, _, body, _) = call(app, "GET", "/api/db", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body.expect("json")["error"],
            "no plugin provides the connection catalog; database browsing is unavailable"
        );
    }

    #[tokio::test]
    async fn a_draining_provider_refuses_with_a_stopping_503() {
        // The withdraw half of a provider stop: existing leases drain, new requests get
        // a 503 that says the plugin is stopping (SPEC §host.seats).
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(
                Arc::new(StubProvider {
                    rows: vec![db_entry(
                        "db",
                        Arc::new(StubDb {
                            seen: Arc::new(Mutex::new(Seen::default())),
                            databases: None,
                        }),
                    )],
                    tracker: LeaseTracker::new(),
                }),
                "mcp",
            )
            .expect("registers");
        catalog.begin_withdraw();
        let app = dbbrowser_router(
            catalog,
            Arc::new(Vec::new),
            Arc::new(|_| "default".to_string()),
        );
        let (status, _, body, _) = call(app, "GET", "/api/db/db/tables", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body.expect("json")["error"],
            "the mcp plugin is stopping; database browsing is momentarily unavailable"
        );
    }

    #[tokio::test]
    async fn requests_release_their_leases_when_they_finish() {
        // The request-scope rule: after N requests over M connections, the provider's
        // ledger must read exactly zero — /api/db must never leak a lease, because a
        // leaked lease is a connection a provider stop would have to close over.
        let tracker = LeaseTracker::new();
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(
                Arc::new(StubProvider {
                    rows: vec![
                        db_entry(
                            "db",
                            Arc::new(StubDb {
                                seen: Arc::new(Mutex::new(Seen::default())),
                                databases: None,
                            }),
                        ),
                        redis_entry("cache"),
                    ],
                    tracker: tracker.clone(),
                }),
                "mcp",
            )
            .expect("registers");
        let app = dbbrowser_router(
            catalog.clone(),
            Arc::new(Vec::new),
            Arc::new(|_| "default".to_string()),
        );
        for uri in [
            "/api/db",
            "/api/db/db/tables",
            "/api/db/db/data?table=users",
            "/api/db/db/schema?table=users",
            "/api/db/cache/keys",
        ] {
            let (status, _, _, _) = call(app.clone(), "GET", uri, None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
        }
        assert_eq!(
            tracker.outstanding(),
            0,
            "every request gave its lease back"
        );
        // And the catalog still serves the next request cleanly.
        let lease = catalog.lease("db", "probe").expect("still leasable");
        assert_eq!(tracker.outstanding(), 1);
        drop(lease);
        assert_eq!(tracker.outstanding(), 0);
    }

    // --- SPEC §data.databases: the database catalog route ----------------------------------------------

    #[tokio::test]
    async fn the_databases_route_passes_the_browsers_catalog_through_whole() {
        // The panel's database selector is this body, byte for byte: primary first, the
        // rest with their browsability and the server's own reason sentence.
        let catalog = json!({
            "primary": "acme_app_dev",
            "current": "acme_app_dev",
            "databases": [
                { "name": "acme_app_dev", "primary": true,  "browsable": true,  "system": false, "tables": 1712 },
                { "name": "mysql",        "primary": false, "browsable": true,  "system": true,  "tables": 31 },
                { "name": "template_app", "primary": false, "browsable": false, "system": false,
                  "reason": "A Postgres connection is bound to one database; browsing this one needs its own connection." },
            ],
        });
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: Some(Ok(catalog.clone())),
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/databases", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json"), catalog);
    }

    #[tokio::test]
    async fn a_browser_error_on_the_catalog_maps_to_the_shared_400() {
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: Some(Err("catalog is being rebuilt".into())),
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/databases", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.expect("json")["error"], "catalog is being rebuilt");
    }

    #[tokio::test]
    async fn a_browser_that_never_heard_of_the_route_serves_the_empty_catalog() {
        // The trait default is the compatibility story (SPEC §data.databases): a flavor or a stub
        // without a database axis compiles unchanged and answers empty — the panel shows
        // no selector, nothing 500s.
        let app = router_of(vec![db_entry(
            "db",
            Arc::new(StubDb {
                seen: SeenRef::default(),
                databases: None,
            }),
        )]);
        let (status, _, body, _) = call(app, "GET", "/api/db/db/databases", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.expect("json"),
            json!({ "primary": null, "current": null, "databases": [] })
        );
    }

    #[tokio::test]
    async fn the_redis_flavor_answers_the_catalog_route_too() {
        // INFO keyspace + CLIENT INFO (SPEC §data.databases): current is the db number the connection
        // SELECTed, every other dbN is listed with its key count and not browsable.
        let catalog = json!({
            "primary": "0",
            "current": "0",
            "databases": [
                { "name": "0", "primary": true,  "browsable": true,  "system": false, "tables": 3160, "reason": null },
                { "name": "5", "primary": false, "browsable": false, "system": false, "tables": 2,
                  "reason": "SELECT would break the shared connection; opening another database needs its own connection." },
            ],
        });
        let seen = SeenRef::default();
        let app = router_of(vec![Arc::new(StubRow {
            name: "cache".into(),
            adapter_type: "redis".into(),
            state: "stopped".into(),
            browser: BrowserFlavor::Redis(Arc::new(StubRedis {
                seen,
                databases: Some(catalog.clone()),
            })),
        })]);
        let (status, _, body, _) = call(app, "GET", "/api/db/cache/databases", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.expect("json"), catalog);
    }

    #[tokio::test]
    async fn a_non_browsable_mcp_404s_on_the_catalog_route() {
        let app = router_of(vec![plain_entry("echo")]);
        let (status, _, body, _) = call(app, "GET", "/api/db/echo/databases", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.expect("json")["error"],
            "MCP 'echo' (echo) has no database to browse"
        );
    }
}
