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

//! The console history's routes (SPEC §data.history) and the one helper every console route
//! records through. A run is filed when the route answers it, failures too, under the
//! database the request reached: the browser's own label, a mongo command's `db` in front of
//! the connection's hosts. The read pair names its database the same way, so a list never
//! holds another database's statements.
//!
//! - `GET /api/db/{name}/history?q=&before=&limit=&db=` answers `{target, runs, more}`,
//!   newest first; `before` is the oldest id of the previous page, `db` is a mongo
//!   connection's database.
//! - `DELETE /api/db/{name}/history?db=` forgets that database's runs: `{target, cleared}`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use serde_json::{json, Value};

use swiss_core::log;
use swiss_host::dbbrowser::BrowserFlavor;
use swiss_host::mongobrowser::assert_db_name;
use swiss_host::services::catalog::CatalogRegistry;

use crate::dbbrowser_api::{catalog_guard, lease_fail, reply, Fail};
use crate::history::{DataHistory, NewRun, Run};

/// The history as a console route sees it: absent in a composition that mounts none (the
/// route suites), where a run records nothing and the read pair answers 503.
pub(crate) type HistoryLayer = Option<Extension<Arc<DataHistory>>>;

/// A page's size when the request names none.
const PAGE_DEFAULT: usize = 100;

/// A mongo command's database as its history files it: the command's own `db` in front of
/// the hosts the connection's label names (`shop @ db1:27017 +2` -> `orders @ db1:27017 +2`).
pub(crate) fn mongo_target(label: &str, db: &str) -> String {
    let hosts = label.split_once(" @ ").map_or(label, |(_, hosts)| hosts);
    format!("{db} @ {hosts}")
}

/// File one console run when a history is mounted. `target` is asked only then; the clock
/// is the route's own, the one its `elapsedMs` reads.
pub(crate) fn record(
    history: &HistoryLayer,
    target: impl FnOnce() -> String,
    conn: &str,
    dialect: &str,
    stmt: &str,
    out: &Result<Value, String>,
    started: Instant,
) {
    if let Some(Extension(h)) = history {
        let ms = started.elapsed().as_millis() as u64;
        h.record(NewRun::of(target(), conn, dialect, stmt, out, ms));
    }
}

fn unavailable(message: String) -> Fail {
    Fail {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message,
        conflict: None,
    }
}

fn mounted(history: &HistoryLayer) -> Result<&Arc<DataHistory>, Fail> {
    history
        .as_ref()
        .map(|Extension(h)| h)
        .ok_or_else(|| unavailable("the console history is not open (the Data view is disabled)".into()))
}

/// The database a read names, the way the console routes file it.
fn target_of(catalog: &CatalogRegistry, name: &str, q: &HashMap<String, String>) -> Result<String, Fail> {
    catalog_guard(catalog)?;
    let lease = catalog.lease(name, "data").map_err(lease_fail)?;
    match lease.flavor() {
        BrowserFlavor::Db(b) => Ok(b.label()),
        BrowserFlavor::Redis(rb) => Ok(rb.label()),
        BrowserFlavor::Mongo(mb) => {
            let db = q.get("db").map(|s| s.trim()).unwrap_or("");
            assert_db_name(db).map_err(Fail::bad)?;
            Ok(mongo_target(&mb.label(), db))
        }
        BrowserFlavor::None => Err(Fail {
            status: StatusCode::NOT_FOUND,
            message: format!("MCP '{name}' ({}) has no console", lease.dialect()),
            conflict: None,
        }),
    }
}

async fn list(
    history: &HistoryLayer,
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let h = mounted(history)?;
    let target = target_of(catalog, name, q)?;
    let before = match q.get("before").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => Some(s.parse::<i64>().map_err(|_| Fail::bad("before must be a run id"))?),
        None => None,
    };
    let limit = match q.get("limit").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| Fail::bad("limit must be a positive integer"))?,
        None => PAGE_DEFAULT,
    };
    let (runs, more) = h
        .page(&target, q.get("q").map(String::as_str), before, limit)
        .await
        .map_err(unavailable)?;
    Ok(json!({
        "target": target,
        "runs": runs.iter().map(Run::to_json).collect::<Vec<_>>(),
        "more": more,
    }))
}

async fn clear(
    history: &HistoryLayer,
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let h = mounted(history)?;
    let target = target_of(catalog, name, q)?;
    let cleared = h.clear(&target).await.map_err(unavailable)?;
    log::log(
        "info",
        "data history cleared",
        Some(json!({ "name": name, "target": target, "runs": cleared })),
    );
    Ok(json!({ "target": target, "cleared": cleared }))
}

async fn list_route(
    history: HistoryLayer,
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(list(&history, &catalog, &name, &q).await)
}

async fn clear_route(
    history: HistoryLayer,
    Extension(catalog): Extension<Arc<CatalogRegistry>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(clear(&history, &catalog, &name, &q).await)
}

pub(crate) fn history_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route("/api/db/{name}/history", get(list_route).delete(clear_route))
}

#[cfg(test)]
mod tests {
    use super::mongo_target;

    #[test]
    fn a_mongo_command_is_filed_under_its_own_database() {
        assert_eq!(mongo_target("shop @ db1:27017 +2", "orders"), "orders @ db1:27017 +2");
        assert_eq!(mongo_target("db1:27017", "orders"), "orders @ db1:27017", "a label without a database");
    }
}
