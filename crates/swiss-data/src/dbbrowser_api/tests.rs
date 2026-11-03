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

//! The data browser API's tests, ported from dbbrowser.test.ts's "data browser API" describe:
//! a stub browser over the real router, so the routes and the payload contract are tested
//! end-to-end without a live MySQL/Postgres/Redis.

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
    /// The (table, column, key) the decode route forwarded (SPEC §data.sqlite).
    decoded: Option<(String, String, Map<String, Value>)>,
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
    async fn decode_cell(&self, table: &str, column: &str, key: &Map<String, Value>) -> Result<Option<Value>, String> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.decoded = Some((table.to_string(), column.to_string(), key.clone()));
        }
        match (table, column) {
            ("call_body", "block") => Ok(Some(json!({
                "kind": "text", "text": "hello", "record": "call", "recordId": "42",
                "chunk": { "part": 0, "parts": 1, "codec": "lz4", "sealed": true, "plain": 5, "stored": 9, "ofs": 0 },
            }))),
            ("refused", _) => Err("the sealed part did not open".into()),
            _ => Ok(None),
        }
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
        BrowserFlavor::Mongo(mb) => BrowserFlavor::Mongo(mb.clone()),
        BrowserFlavor::None => BrowserFlavor::None,
    }
}

fn row_facts(row: &StubRow) -> (String, String) {
    match &row.browser {
        BrowserFlavor::Db(db) => (db.dialect().as_str().to_string(), db.label()),
        BrowserFlavor::Redis(rb) => ("redis".into(), rb.label()),
        BrowserFlavor::Mongo(mb) => ("mongo".into(), mb.label()),
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
async fn a_mongo_route_on_a_sql_connection_is_a_404_naming_its_dialect() {
    let app = router_of(vec![db_entry(
        "db-one",
        Arc::new(StubDb { seen: Arc::new(Mutex::new(Seen::default())), databases: None }),
    )]);
    let (status, _, body, _) = call(
        app,
        "POST",
        "/api/db/db-one/mongo/find",
        Some(json!({ "db": "app", "collection": "users" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body.expect("json")["error"], json!("MCP 'db-one' (mysql) is not a MongoDB connection"));
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

/// SPEC §data.history: a console run is filed under the database it reached - the browser's
/// label - so two names over one database read one list, a redis command never shows under a
/// SQL database, and a composition without a history answers 503 instead of an empty list.
#[tokio::test]
async fn console_runs_are_filed_by_the_database_they_reached() {
    let dir = swiss_core::paths::test_home().join("data-history").join("routes");
    let _ = std::fs::remove_dir_all(&dir);
    let history = crate::history::DataHistory::new(dir);
    history.open().await.expect("open");
    let rows = || {
        vec![
            db_entry("db", Arc::new(StubDb { seen: SeenRef::default(), databases: None })),
            db_entry("db-alias", Arc::new(StubDb { seen: SeenRef::default(), databases: None })),
            redis_entry("cache"),
        ]
    };
    let app = || crate::history::mounted(router_of(rows()), Some(history.clone()));
    let (status, _, _, _) =
        call(app(), "POST", "/api/db/db/query", Some(json!({ "sql": "SELECT 1" }))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _, _) =
        call(app(), "POST", "/api/db/cache/command", Some(json!({ "command": "PING" }))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body, _) = call(app(), "GET", "/api/db/db-alias/history", None).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("json");
    assert_eq!(body["target"], "stub @ localhost");
    assert_eq!(body["more"], false);
    let runs = body["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "{body}");
    assert_eq!(runs[0]["stmt"], "SELECT 1");
    assert_eq!(runs[0]["conn"], "db", "the name that ran it, under the database both names reach");
    assert_eq!((runs[0]["ok"].clone(), runs[0]["rows"].clone()), (json!(true), json!(1)));

    let (_, _, body, _) = call(app(), "GET", "/api/db/cache/history", None).await;
    let body = body.expect("json");
    assert_eq!(body["target"], "cache @ localhost:6379 db 0");
    let runs = body["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "{body}");
    assert_eq!(runs[0]["stmt"], "PING");
    assert!(runs[0].get("rows").is_none(), "a redis reply files no row count");

    let (status, _, body, _) = call(app(), "GET", "/api/db/db/history?limit=0", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
    let (status, _, body, _) = call(app(), "DELETE", "/api/db/db/history", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("json")["cleared"], 1);
    let (_, _, body, _) = call(app(), "GET", "/api/db/db/history", None).await;
    assert_eq!(body.expect("json")["runs"], json!([]));
    let (_, _, body, _) = call(app(), "GET", "/api/db/cache/history", None).await;
    assert_eq!(body.expect("json")["runs"].as_array().map(Vec::len), Some(1), "a clear forgets one database");

    let (status, _, _, _) = call(router_of(rows()), "GET", "/api/db/db/history", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "no history mounted is not an empty one");
    history.close().await.expect("close");
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

#[tokio::test]
async fn decode_forwards_the_row_key_and_answers_what_the_module_read() {
    // SPEC §data.sqlite "Decoded cells": the route hands the table, column and primary key to
    // the browser; the bytes come from the module's own read, never from the request.
    let seen: SeenRef = Arc::new(Mutex::new(Seen::default()));
    let app = || router_of(vec![db_entry("swiss-llm", Arc::new(StubDb { seen: seen.clone(), databases: None }))]);
    let key = "%7B%22id%22%3A42%7D"; // {"id":42}
    let (status, _, body, _) =
        call(app(), "GET", &format!("/api/db/swiss-llm/decode?table=call_body&column=block&key={key}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("json");
    assert_eq!(body["decoded"], json!(true));
    assert_eq!(body["text"], json!("hello"));
    assert_eq!(body["chunk"]["codec"], json!("lz4"));
    let (table, column, sent) = seen.lock().unwrap().decoded.clone().expect("forwarded");
    assert_eq!((table.as_str(), column.as_str()), ("call_body", "block"));
    assert_eq!(Value::Object(sent), json!({ "id": 42 }));

    let (status, _, body, _) =
        call(app(), "GET", &format!("/api/db/swiss-llm/decode?table=call&column=preview_text&key={key}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("json"), json!({ "decoded": false }), "a cell nothing decodes keeps the grid's view");

    let (status, _, body, _) =
        call(app(), "GET", &format!("/api/db/swiss-llm/decode?table=refused&column=block&key={key}"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body.expect("json")["error"], json!("the sealed part did not open"));

    for bad in ["", "&key=%5B1%5D", "&key=%7B%7D", "&key=nope"] {
        let (status, _, body, _) =
            call(app(), "GET", &format!("/api/db/swiss-llm/decode?table=call_body&column=block{bad}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(body.expect("json")["error"].as_str().unwrap().contains("key must be a JSON object"), "{bad}");
    }
    let (status, _, _, _) = call(app(), "GET", &format!("/api/db/swiss-llm/decode?column=block&key={key}"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a table is required");
}
