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

//! The /api/analysis routes (SPEC §data.analysis): the panel's and the agent's window onto the
//! analysis ledger. Same gate as the rest of /api (the admin session), and the same rule as the
//! Data browser's routes: a driver's own error text is the useful answer, so it comes back
//! verbatim behind a 400.
//!
//! Nothing here returns a whole result. Rows come back one bounded page at a time, and a chart's
//! numbers are computed on the server over the stored rows, so a panel showing page 1 can never
//! mistake that page for the total.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};

use swiss_host::reply::admin_json;
use swiss_host::services::RuntimeServices;

use crate::analysis::{AnalysisStore, CatalogRunner, Runner, PAGE_MAX};
use crate::llm_api::{bad, body_of, field, ok};

/// Mount the analysis routes. The store is opened once at boot (and reconciled there); the
/// connection catalog arrives through the shared runtime services, so executing an analysis
/// leases exactly the way the Data agent's own reads do.
pub fn mount(store: Arc<AnalysisStore>, services: Arc<RuntimeServices>) -> Router {
    Router::new()
        .route("/api/analysis", get(list).post(create))
        .route("/api/analysis/{id}", get(one).delete(drop_task))
        .route("/api/analysis/{id}/revisions", post(revise))
        .route("/api/analysis/{id}/save", post(save))
        .route("/api/analysis/{id}/execute", post(execute))
        .route("/api/analysis/{id}/executions/{run}/cancel", post(cancel))
        .route("/api/analysis/{id}/compare", post(compare))
        .route("/api/analysis/{id}/results/{result}/rows", get(rows))
        .route("/api/analysis/{id}/results/{result}/series", get(series))
        .route("/api/analysis/{id}/results/{result}/checks", post(check))
        .route("/api/analysis/{id}/results/{result}/validate", post(validate))
        .route("/api/analysis/{id}/results/{result}/chart", post(chart))
        .layer(Extension(store))
        .layer(Extension(services))
}

async fn list(Extension(store): Extension<Arc<AnalysisStore>>, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    ok(json!({ "analyses": store.list(q.get("conn").map(String::as_str)) }))
}

async fn one(Extension(store): Extension<Arc<AnalysisStore>>, Path(id): Path<String>) -> Response {
    match store.get(&id) {
        Ok(task) => task_out(task, axum::http::StatusCode::OK),
        Err(why) => bad(why),
    }
}

async fn drop_task(Extension(store): Extension<Arc<AnalysisStore>>, Path(id): Path<String>) -> Response {
    match store.remove(&id) {
        Ok(()) => ok(json!({ "deleted": id })),
        Err(why) => bad(why),
    }
}

async fn create(Extension(store): Extension<Arc<AnalysisStore>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["conn", "kind", "title", "sql", "conditions", "params", "note"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let conn = match required(&body, "conn") {
        Ok(v) => v,
        Err(why) => return bad(why),
    };
    let sql = match required(&body, "sql") {
        Ok(v) => v,
        Err(why) => return bad(why),
    };
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or("postgres");
    let title = body.get("title").and_then(Value::as_str).unwrap_or_default();
    let note = body.get("note").and_then(Value::as_str).unwrap_or_default();
    let conditions = body.get("conditions").cloned().unwrap_or_else(|| json!({}));
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    match store.create(&conn, kind, title, &sql, conditions, params, note) {
        Ok(task) => task_out(task, axum::http::StatusCode::CREATED),
        Err(why) => bad(why),
    }
}

async fn revise(Extension(store): Extension<Arc<AnalysisStore>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["baseRevision", "sql", "conditions", "params", "note"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let base = match field(&body, "baseRevision") {
        Ok(Some(base)) => base,
        Ok(None) => return bad("baseRevision is required"),
        Err(why) => return bad(why),
    };
    let sql = match required(&body, "sql") {
        Ok(v) => v,
        Err(why) => return bad(why),
    };
    let note = body.get("note").and_then(Value::as_str).unwrap_or_default();
    let conditions = body.get("conditions").cloned().unwrap_or_else(|| json!({}));
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    match store.revise(&id, &base, &sql, conditions, params, note) {
        Ok(task) => task_out(task, axum::http::StatusCode::OK),
        Err(why) => bad(why),
    }
}

async fn save(Extension(store): Extension<Arc<AnalysisStore>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["saved"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let saved = body.get("saved").and_then(Value::as_bool).unwrap_or(true);
    match store.save(&id, saved) {
        Ok(task) => task_out(task, axum::http::StatusCode::OK),
        Err(why) => bad(why),
    }
}

/// Run a revision and commit its result. The caller waits for the answer; a long statement is
/// bounded by the store's timeout, and the cancel route ends the wait early. The response is the
/// execution plus the result's metadata - never the rows.
async fn execute(
    Extension(store): Extension<Arc<AnalysisStore>>,
    Extension(services): Extension<Arc<RuntimeServices>>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let body = match body_of(&bytes, &["revisionId", "maxRows", "timeoutMs"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let revision = body.get("revisionId").and_then(Value::as_str);
    let max_rows = body.get("maxRows").and_then(Value::as_u64).map(|n| n.min(u64::from(u32::MAX)) as u32);
    let timeout = body.get("timeoutMs").and_then(Value::as_u64);
    let conn = match store.get(&id) {
        Ok(task) => task.conn,
        Err(why) => return bad(why),
    };
    let runner = CatalogRunner::new(services.catalog.clone(), &conn);
    run_with(&store, &runner, &id, revision, max_rows, timeout).await
}

/// The execute handler's body, split from the route so a test drives it with a stub runner.
async fn run_with(store: &AnalysisStore, runner: &dyn Runner, id: &str, revision: Option<&str>, max_rows: Option<u32>, timeout: Option<u64>) -> Response {
    match store.execute(runner, id, revision, max_rows, timeout).await {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn cancel(Extension(store): Extension<Arc<AnalysisStore>>, Path((id, run)): Path<(String, String)>) -> Response {
    let belongs = store.get(&id).ok().is_some_and(|t| t.executions.iter().any(|e| e.id == run));
    ok(json!({ "execution": run, "cancelled": belongs && store.cancel(&run), "databaseCancellationConfirmed": false }))
}

async fn compare(Extension(store): Extension<Arc<AnalysisStore>>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["left", "right", "keys"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (left, right) = match (required(&body, "left"), required(&body, "right")) {
        (Ok(l), Ok(r)) => (l, r),
        (Err(why), _) | (_, Err(why)) => return bad(why),
    };
    let keys: Vec<String> = body.get("keys").and_then(Value::as_array).map(|ks| {
        ks.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()
    }).unwrap_or_default();
    match store.compare(&id, &left, &right, &keys) {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn rows(
    Extension(store): Extension<Arc<AnalysisStore>>,
    Path((id, result)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let offset = q.get("offset").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let limit = q.get("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(50).clamp(1, PAGE_MAX);
    match store.rows(&id, &result, offset, limit) {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn series(
    Extension(store): Extension<Arc<AnalysisStore>>,
    Path((id, result)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(x) = q.get("x").cloned() else { return bad("x is required") };
    let y: Vec<String> = q.get("y").map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let out = match q.get("mode").map(String::as_str) {
        None => store.series(&id, &result, &x, &y),
        Some("scatter") => store.scatter_series(&id, &result, &x, &y),
        Some(_) => Err("mode: scatter is the only explicit mode".into()),
    };
    match out {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn check(Extension(store): Extension<Arc<AnalysisStore>>, Path((id, result)): Path<(String, String)>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["kind", "state", "note", "by", "evidence"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let kind = match required(&body, "kind") {
        Ok(v) => v,
        Err(why) => return bad(why),
    };
    let state = match required(&body, "state") {
        Ok(v) => v,
        Err(why) => return bad(why),
    };
    let note = body.get("note").and_then(Value::as_str).unwrap_or_default();
    let by = body.get("by").and_then(Value::as_str).unwrap_or("user");
    let evidence = body.get("evidence").and_then(Value::as_str);
    match store.record_check(&id, &result, crate::analysis::CheckDraft { kind: &kind, state: &state, note, by, evidence }) {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn validate(Extension(store): Extension<Arc<AnalysisStore>>, Path((id, result)): Path<(String, String)>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["type", "columns", "expected"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    match store.validate(&id, &result, &Value::Object(body)) {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

async fn chart(Extension(store): Extension<Arc<AnalysisStore>>, Path((id, result)): Path<(String, String)>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["kind", "x", "y", "title"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    match store.set_chart(&id, &result, &Value::Object(body)) {
        Ok(out) => ok(out),
        Err(why) => bad(why),
    }
}

fn task_out(task: crate::analysis::Task, status: axum::http::StatusCode) -> Response {
    match serde_json::to_value(task) {
        Ok(body) => admin_json(status, body),
        Err(why) => bad(why.to_string()),
    }
}

fn required(body: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match field(body, key) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(format!("{key} is required")),
        Err(why) => Err(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use swiss_core::util::random_hex;
    use swiss_host::dbbrowser::{BrowserFlavor, DbBrowser, DbDialect, EditError, SqlDump};
    use swiss_host::services::catalog::{CatalogError, CatalogRegistry, ConnectionCatalog, ConnectionInfo, ConnectionLease, LeaseTracker};
    use tower::ServiceExt;

    fn store() -> Arc<AnalysisStore> {
        AnalysisStore::at(std::env::temp_dir().join(format!("swiss-analysis-api-{}", random_hex(6))))
    }

    fn app(store: Arc<AnalysisStore>) -> Router {
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        mount(store, services)
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let payload = body.map(|b| serde_json::to_vec(&b).unwrap_or_default()).unwrap_or_default();
        let request = Request::builder().method(method).uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(payload)).expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn an_analysis_is_created_revised_saved_and_listed_over_http() {
        let store = store();
        let app = app(store.clone());
        let (status, task) = call(&app, "POST", "/api/analysis", Some(json!({
            "conn": "mm", "kind": "postgres", "title": "VIP customers",
            "sql": "select 1", "conditions": { "minOrders": 3 }, "note": "first",
        }))).await;
        assert_eq!(status, StatusCode::CREATED);
        let id = task["id"].as_str().expect("id").to_string();
        let revision = task["current"].as_str().expect("revision").to_string();

        let (status, revised) = call(&app, "POST", &format!("/api/analysis/{id}/revisions"), Some(json!({
            "baseRevision": revision, "sql": "select 2", "conditions": { "minOrders": 4 },
        }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(revised["revisions"].as_array().map(|r| r.len()), Some(2));
        assert_eq!(revised["conditions"], Value::Null, "conditions live on the revisions, not the task");

        let (status, bad_rev) = call(&app, "POST", &format!("/api/analysis/{id}/revisions"), Some(json!({
            "baseRevision": "nope", "sql": "select 3",
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(bad_rev["error"].as_str().unwrap_or_default().contains("baseRevision"));

        let (status, _) = call(&app, "POST", &format!("/api/analysis/{id}/save"), Some(json!({ "saved": true }))).await;
        assert_eq!(status, StatusCode::OK);
        let (_, listed) = call(&app, "GET", "/api/analysis?conn=mm", None).await;
        assert_eq!(listed["analyses"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(listed["analyses"][0]["saved"], json!(true));
        let (_, other) = call(&app, "GET", "/api/analysis?conn=nobody", None).await;
        assert_eq!(other["analyses"], json!([]));

        let (status, _) = call(&app, "DELETE", &format!("/api/analysis/{id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&app, "GET", &format!("/api/analysis/{id}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn an_unknown_field_is_refused_rather_than_ignored() {
        let store = store();
        let app = app(store.clone());
        let (status, body) = call(&app, "POST", "/api/analysis", Some(json!({
            "conn": "mm", "sql": "select 1", "admin": true,
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap_or_default().contains("admin"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_task_id_that_is_a_path_is_not_found() {
        let store = store();
        let app = app(store.clone());
        let (status, _) = call(&app, "GET", "/api/analysis/..%2F..%2Fgateway.config", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(store.root());
    }

    // --- execute against a stub connection, through the real catalog lease -------------------

    struct StubDb {
        rows: usize,
    }

    #[async_trait::async_trait]
    impl DbBrowser for StubDb {
        fn dialect(&self) -> DbDialect { DbDialect::Pg }
        fn label(&self) -> String { "mm @ db:5432".into() }
        async fn list_tables(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn read_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn describe_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn apply_edits(&self, _o: &Value) -> Result<Value, EditError> { Err(EditError::Bad("unused".into())) }
        async fn run_query(&self, _sql: &str, _limit: Option<&Value>) -> Result<Value, String> { Err("unused".into()) }
        async fn run_read_query(&self, _sql: &str, limit: Option<&Value>) -> Result<Value, String> {
            let cap = limit.and_then(Value::as_u64).unwrap_or(10) as usize;
            let rows: Vec<Value> = (0..self.rows.min(cap)).map(|i| json!([i + 1, (i + 1) * 10])).collect();
            Ok(json!({ "columns": ["id", "total"], "rows": rows }))
        }
        async fn export_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn export_sql_dump(&self, _o: &Value) -> Result<SqlDump, String> { Err("unused".into()) }
        async fn import_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn ddl_op(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn activity(&self) -> Result<Value, String> { Err("unused".into()) }
        async fn activity_kill(&self, _pid: i64, _terminate: bool) -> Result<Value, String> { Err("unused".into()) }
        async fn completion(&self, _sql: &str, _caret: usize) -> Result<Value, String> { Err("unused".into()) }
    }

    struct StubCatalog {
        tracker: Arc<LeaseTracker>,
        db: Arc<StubDb>,
    }

    impl ConnectionCatalog for StubCatalog {
        fn list(&self) -> Vec<ConnectionInfo> {
            vec![ConnectionInfo { id: "mm".into(), label: self.db.label(), dialect: "pg".into(), state: "running".into() }]
        }
        fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError> {
            if id == "mm" {
                Ok(self.tracker.grant(id, holder, "pg", BrowserFlavor::Db(self.db.clone())))
            } else {
                Err(CatalogError::Unknown(format!("unknown MCP: {id}")))
            }
        }
    }

    fn live_app(rows: usize) -> (Router, Arc<AnalysisStore>) {
        let store = store();
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        let catalog: Arc<CatalogRegistry> = services.catalog.clone();
        catalog.register(Arc::new(StubCatalog { tracker: LeaseTracker::new(), db: Arc::new(StubDb { rows }) }), "mcp").expect("register");
        (mount(store.clone(), services), store)
    }

    #[tokio::test]
    async fn a_run_over_http_leases_the_connection_and_serves_pages_not_a_dump() {
        let (app, store) = live_app(5);
        let (_, task) = call(&app, "POST", "/api/analysis", Some(json!({ "conn": "mm", "sql": "select * from orders" }))).await;
        let id = task["id"].as_str().expect("id").to_string();
        let (status, ran) = call(&app, "POST", &format!("/api/analysis/{id}/execute"), Some(json!({ "maxRows": 3 }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ran["status"], json!("completed"));
        assert_eq!(ran["result"]["rowCount"], json!(3));
        assert_eq!(ran["result"]["truncated"], json!(true));
        let result = ran["result"]["id"].as_str().expect("result").to_string();
        assert!(ran.get("rows").is_none(), "the execute answer carries no rows");

        let (_, page) = call(&app, "GET", &format!("/api/analysis/{id}/results/{result}/rows?offset=0&limit=2"), None).await;
        assert_eq!(page["rows"], json!([[1, 10], [2, 20]]));
        assert_eq!(page["total"], json!(3));
        assert_eq!(page["more"], json!(true));
        let (_, page2) = call(&app, "GET", &format!("/api/analysis/{id}/results/{result}/rows?offset=2&limit=2"), None).await;
        assert_eq!(page2["rows"], json!([[3, 30]]));
        assert_eq!(page2["more"], json!(false));

        let (_, chart) = call(&app, "POST", &format!("/api/analysis/{id}/results/{result}/chart"), Some(json!({
            "kind": "bar", "x": "id", "y": ["total"], "title": "Totals",
        }))).await;
        assert_eq!(chart["chart"]["kind"], json!("bar"));
        let (_, series) = call(&app, "GET", &format!("/api/analysis/{id}/results/{result}/series?x=id&y=total"), None).await;
        assert_eq!(series["points"].as_array().map(|p| p.len()), Some(3));

        let execution = ran["execution"].as_str().expect("execution").to_string();
        // A confirmation with nothing behind it is refused at the route too, not only in the store.
        let (status, refused) = call(&app, "POST", &format!("/api/analysis/{id}/results/{result}/checks"), Some(json!({
            "kind": "rowCount", "state": "confirmed", "note": "matches the source",
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refused["error"].as_str().unwrap_or_default().contains("evidence"), "{refused}");

        let (status, _) = call(&app, "POST", &format!("/api/analysis/{id}/results/{result}/checks"), Some(json!({
            "kind": "rowCount", "state": "confirmed", "note": "matches the source", "evidence": execution,
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a successful query does not prove a claim");
        let (_, check) = call(&app, "POST", &format!("/api/analysis/{id}/results/{result}/validate"), Some(json!({
            "type": "rowCount", "expected": 3,
        }))).await;
        assert_eq!(check["verification"], json!("partial"));
        assert_eq!(check["checks"][0]["evidence"], json!(execution));

        let (_, again) = call(&app, "POST", &format!("/api/analysis/{id}/execute"), Some(json!({}))).await;
        let second = again["result"]["id"].as_str().expect("second").to_string();
        assert_ne!(second, result);
        let (_, diff) = call(&app, "POST", &format!("/api/analysis/{id}/compare"), Some(json!({
            "left": result, "right": second,
        }))).await;
        assert_eq!(diff["shared"], json!(3));
        // The second run had no row cap, so it holds two rows the first (capped at 3) did not.
        assert_eq!(diff["added"], json!(2));
        assert_eq!(diff["removed"], json!(0));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn an_unknown_connection_fails_the_run_without_committing_a_result() {
        let (app, store) = live_app(1);
        let (_, task) = call(&app, "POST", "/api/analysis", Some(json!({ "conn": "nobody", "sql": "select 1" }))).await;
        let id = task["id"].as_str().expect("id").to_string();
        let (status, body) = call(&app, "POST", &format!("/api/analysis/{id}/execute"), Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap_or_default().contains("unknown MCP"));
        let (_, task) = call(&app, "GET", &format!("/api/analysis/{id}"), None).await;
        assert_eq!(task["executions"][0]["status"], json!("failed"));
        assert_eq!(task["results"], json!([]));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn cancelling_an_execution_nobody_runs_answers_false() {
        let (app, store) = live_app(1);
        let (_, body) = call(&app, "POST", "/api/analysis/abc/executions/nope/cancel", Some(json!({}))).await;
        assert_eq!(body["cancelled"], json!(false));
        let _ = std::fs::remove_dir_all(store.root());
    }
}
