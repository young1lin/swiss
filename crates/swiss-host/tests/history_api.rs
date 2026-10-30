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

//! The cross-module query routes' acceptance set (plan Q5 plus the route halves of
//! S3 and the usage report): a union that cannot drop an owner's hits, a merge
//! that waits for slower streams, disabled modules reported as skipped, and
//! tampered cursors refused by name without failing the page.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use swiss_host::history::body;
use swiss_host::history::search::{BodySource, StoredChunk};
use swiss_host::services::api::mount;
use swiss_host::services::history_query::{HistoryQueryAdapter, HistoryQuerySeat};
use swiss_host::services::RuntimeServices;

#[derive(Clone, Default)]
struct MemOwner {
    chunks: Arc<Mutex<Vec<StoredChunk>>>,
}

impl MemOwner {
    fn add(&self, run_id: i64, ofs: i64, plain: &[u8]) {
        let (codec, block) = body::encode_chunk(plain);
        self.chunks.lock().unwrap().push(StoredChunk {
            run_id,
            ofs,
            part: 0,
            codec: codec.as_u8() as i64,
            plain: plain.len() as i64,
            block,
        });
        self.chunks.lock().unwrap().sort_by_key(|c| (c.run_id, c.ofs));
    }
}

impl BodySource for MemOwner {
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>
    {
        let chunks = self.chunks.clone();
        Box::pin(async move {
            let all = chunks.lock().unwrap();
            Ok(all
                .iter()
                .filter(|c| (c.run_id, c.ofs) >= from)
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn generation(&self) -> u64 {
        0
    }
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (axum::http::StatusCode, Value) {
    use axum::body::Body;
    use tower::util::ServiceExt;
    let request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(serde_json::to_string(&body).unwrap())
        })
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn wired() -> (axum::Router, Arc<HistoryQuerySeat>) {
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    (mount(services.clone()), services.history_query.clone())
}

/// An owner whose stream FAILS: the engine's sentence must reach the route's
/// errors[] by name — a failed stream is a named 200-level fact, never a 500 and
/// never a silent absence (the cross-checker's route-level gap: the engine tests
/// pinned the sentence, but nothing proved the route forwarded it).
struct FailOwner;

impl BodySource for FailOwner {
    fn chunks_from(
        &self,
        _from: (i64, i64),
        _limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>
    {
        Box::pin(async move { Err("database disk image is malformed".to_string()) })
    }

    fn generation(&self) -> u64 {
        0
    }
}

#[tokio::test]
async fn a_failing_owner_is_reported_scan_failed_on_the_route() {
    let (app, seat) = wired();
    seat.register(HistoryQueryAdapter {
        owner: "mcp".into(),
        db_file: std::env::temp_dir().join("nowhere-fail.db"),
        source: Arc::new(FailOwner),
        bodies: None,
    })
    .unwrap();
    let (status, page) = call(&app, "POST", "/api/history/query", json!({ "q": "SECRET" })).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let errors = page["errors"].as_array().expect("errors[] present");
    let named = errors
        .iter()
        .find(|e| e["module"] == json!("mcp"))
        .expect("the failing owner is named");
    let reason = named["reason"].as_str().expect("a reason string");
    assert!(
        reason.starts_with("scan-failed: ") && reason.contains("malformed"),
        "the engine's sentence rides the route verbatim: {reason}"
    );
    assert!(page["hits"].as_array().unwrap().is_empty(), "{page}");
}

// --- Q5 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn mixed_index_and_scan_union_does_not_drop_ciphertext_only_hits() {
    let (app, seat) = wired();
    let mcp = MemOwner::default();
    mcp.add(1, 0, b"mcp body holds SECRET once");
    let remote = MemOwner::default();
    remote.add(4, 0, b"remote out holds SECRET too");
    seat.register(HistoryQueryAdapter {
        owner: "mcp".into(),
        db_file: std::env::temp_dir().join("nowhere-mcp.db"),
        source: Arc::new(mcp),
        bodies: None,
    })
    .unwrap();
    seat.register(HistoryQueryAdapter {
        owner: "remote".into(),
        db_file: std::env::temp_dir().join("nowhere-remote.db"),
        source: Arc::new(remote),
        bodies: None,
    })
    .unwrap();
    let (status, page) = call(&app, "POST", "/api/history/query", json!({ "q": "SECRET" })).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "both owners' hits survive the union: {page}");
    assert!(hits.iter().any(|h| h["module"] == "mcp"));
    assert!(hits.iter().any(|h| h["module"] == "remote"));
    assert_eq!(page["complete"], json!(true), "{page}");
}

#[tokio::test]
async fn merge_waits_for_other_stream_frontiers() {
    let (app, seat) = wired();
    // mcp has more chunks than one batch: its stream is NOT done; remote is. The
    // merged page must stay incomplete and carry mcp's resume token.
    let mcp = MemOwner::default();
    for i in 0..130i64 {
        mcp.add(1, i * 9, b"filler SECRET");
    }
    let remote = MemOwner::default();
    remote.add(2, 0, b"one SECRET");
    seat.register(HistoryQueryAdapter {
        owner: "mcp".into(),
        db_file: std::env::temp_dir().join("nowhere.db"),
        source: Arc::new(mcp),
        bodies: None,
    })
    .unwrap();
    seat.register(HistoryQueryAdapter {
        owner: "remote".into(),
        db_file: std::env::temp_dir().join("nowhere2.db"),
        source: Arc::new(remote),
        bodies: None,
    })
    .unwrap();
    let (status, page) = call(&app, "POST", "/api/history/query", json!({ "q": "SECRET" })).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(page["complete"], json!(false), "the merge waits for mcp's frontier: {page}");
    assert!(page["next"].get("mcp").is_some(), "mcp's token rides along");
    assert!(page["next"].get("remote").is_none(), "remote is done");
    // Continue until every stream reaches its frontier: no hit dropped on the way.
    let mut cursors = page["next"].clone();
    let mut total = page["hits"].as_array().unwrap().len();
    for _ in 0..10 {
        let (_, page) = call(
            &app,
            "POST",
            "/api/history/query",
            json!({ "q": "SECRET", "cursors": cursors }),
        )
        .await;
        total += page["hits"].as_array().unwrap().len();
        if page["complete"] == json!(true) {
            break;
        }
        cursors = page["next"].clone();
    }
    assert_eq!(total, 131, "130 mcp hits + 1 remote hit, none dropped");
}

#[tokio::test]
async fn disabled_module_is_reported_skipped() {
    let (app, seat) = wired();
    let remote = MemOwner::default();
    remote.add(1, 0, b"SECRET");
    seat.register(HistoryQueryAdapter {
        owner: "remote".into(),
        db_file: std::env::temp_dir().join("nowhere3.db"),
        source: Arc::new(remote),
        bodies: None,
    })
    .unwrap();
    // mcp stopped: its adapter is gone. A query that names it gets a named skip,
    // never an auto-open, never a silent absence.
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "modules": ["mcp", "remote"] }),
    )
    .await;
    assert_eq!(page["skipped"], json!(["mcp"]), "{page}");
    assert_eq!(page["hits"].as_array().unwrap().len(), 1);
}

// --- S3 route half ----------------------------------------------------------------------------------

#[tokio::test]
async fn tampered_cursors_are_refused_by_name_without_failing_the_page() {
    let (app, seat) = wired();
    let mcp = MemOwner::default();
    mcp.add(1, 0, b"SECRET");
    let remote = MemOwner::default();
    remote.add(1, 0, b"SECRET");
    seat.register(HistoryQueryAdapter {
        owner: "mcp".into(),
        db_file: std::env::temp_dir().join("nowhere4.db"),
        source: Arc::new(mcp),
        bodies: None,
    })
    .unwrap();
    seat.register(HistoryQueryAdapter {
        owner: "remote".into(),
        db_file: std::env::temp_dir().join("nowhere5.db"),
        source: Arc::new(remote),
        bodies: None,
    })
    .unwrap();
    // A real page first, then tamper one owner's token: that owner is refused by
    // name, the OTHER owner's hits still arrive, and the page says incomplete.
    // A fresh page first, then shrink the limit so both streams carry tokens.
    let (_, _first) = call(&app, "POST", "/api/history/query", json!({ "q": "SECRET" })).await;
    let (_, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 1 }),
    )
    .await;
    let mut cursors = page["next"].clone();
    let forged = cursors["remote"].as_str().unwrap().to_string();
    let flip = if forged.starts_with('0') { '1' } else { '0' };
    let forged = format!("{flip}{}", &forged[1..]);
    cursors["remote"] = json!(forged);
    let (status, page) = call(
        &app,
        "POST",
        "/api/history/query",
        json!({ "q": "SECRET", "limit": 1, "cursors": cursors }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{page}");
    let errors = page["errors"].as_array().unwrap();
    assert!(
        errors.iter().any(|e| e["module"] == "remote" && e["reason"] == "cursor-invalid"),
        "{page}"
    );
    assert_eq!(page["complete"], json!(false));
}

// --- the usage report -------------------------------------------------------------------------------

#[tokio::test]
async fn usage_reports_measured_bytes_and_drops_stopped_owners() {
    let (app, seat) = wired();
    let dir = std::env::temp_dir().join(format!("swiss-histapi-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("remote.db");
    std::fs::write(&db, vec![0u8; 5_000]).unwrap();
    std::fs::write(dir.join("remote.db-wal"), vec![0u8; 1_500]).unwrap();
    let remote = MemOwner::default();
    seat.register(HistoryQueryAdapter {
        owner: "remote".into(),
        db_file: db.clone(),
        source: Arc::new(remote.clone()),
        bodies: None,
    })
    .unwrap();
    let (status, usage) = call(&app, "GET", "/api/history/usage", Value::Null).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{usage}");
    let owners = usage["owners"].as_array().unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0]["dbBytes"], json!(5_000));
    assert_eq!(owners[0]["sidecarBytes"], json!(1_500));
    assert_eq!(owners[0]["share"], json!(200_000_000), "the llm share was carved from remote (SPEC llm.history)");
    assert_eq!(usage["declaredTotal"], json!(1_560_000_000), "the process, llm and data shares grew the declared budget (SPEC host.history)");
    // The module stops: its row leaves the report (its database is not opened).
    seat.unregister("remote");
    let (_, usage) = call(&app, "GET", "/api/history/usage", Value::Null).await;
    assert_eq!(usage["owners"].as_array().unwrap().len(), 0);
    let _ = remote;
}
