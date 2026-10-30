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

//! /api/knowledge through the whole gateway tree: the memory plugin's routes were
//! only ever exercised bare (mount + oneshot inside the plugin crate), never behind
//! the loopback guard and the body limit the panel's requests cross. These pin the
//! wrapper: 200 while serving, 503 when withdrawn, and the sync queue answering
//! through the tree.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use swiss_plugins::memory_api::{mount, MemoryState};
use swiss_memory::service::MemoryService;

use super::{build_app, sandbox, AppContext, ManagedStore, Registry, TokenManager, TOKEN};

/// The gateway tree with the memory routes mounted as `extra`, the way the boot
/// sequence mounts them. `serving` decides whether the plugin's slot holds a
/// service, the difference between a 200 and a 503.
async fn knowledge_app(serving: bool) -> (axum::Router, Arc<MemoryState>, std::path::PathBuf) {
    sandbox();
    let dir = std::env::temp_dir().join(format!("swiss-knowledge-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("scratch");
    let state = Arc::new(MemoryState::new());
    if serving {
        state.install(Arc::new(MemoryService::at(&dir.join("store.sqlite3"))));
    }
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(registry, tokens, store, calls, traffic, "SWISS_TOKEN", 19999);
    (build_app(ctx, Some(mount(state.clone()))), state, dir)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let mut req = req;
    req.headers_mut()
        .entry(header::HOST)
        .or_insert(header::HeaderValue::from_static("127.0.0.1:19999"));
    let res = app.clone().oneshot(req).await.expect("router answers");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(app, Request::get(path).body(Body::empty()).expect("request")).await
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

#[tokio::test]
async fn knowledge_answers_through_the_gateway_tree() {
    let (app, _state, _dir) = knowledge_app(true).await;
    let (status, body) = get(&app, "/api/knowledge/status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(&app, "/api/knowledge/projects", json!({ "name": "K" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let project = body["project"]["id"].as_str().expect("id").to_string();
    let (status, body) = post(&app, "/api/knowledge/sync", json!({ "project": project })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["run"]["state"], json!("queued"), "{body}");
}

#[tokio::test]
async fn knowledge_is_503_when_the_plugin_is_withdrawn() {
    let (app, _state, _dir) = knowledge_app(false).await;
    let (status, _) = get(&app, "/api/knowledge/status").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = post(&app, "/api/knowledge/sync", json!({ "project": "x" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_sync_overview_answers_through_the_tree() {
    let (app, _state, dir) = knowledge_app(true).await;
    let (status, body) = post(
        &app,
        "/api/knowledge/projects",
        json!({ "name": "S", "path": swiss_memory::scope::display(&dir), "exact": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let project = body["project"]["id"].as_str().expect("id").to_string();
    let (status, _) = post(&app, "/api/knowledge/sync", json!({ "project": project })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = get(&app, &format!("/api/knowledge/sync?project={project}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runs"].as_array().map(Vec::len), Some(1), "{body}");
    assert_eq!(body["state"]["head"], Value::Null, "nothing walked yet");
}
