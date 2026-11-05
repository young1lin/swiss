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

//! The admin API, driven through the real router — port of the Node build's
//! `test/adminapi.test.ts`.
//!
//! Every case here goes through `build_app`, so the loopback guard, the JSON body parser and the
//! route table are all in the picture; nothing calls a handler directly. What the panel sees is
//! what these assert.
//!
//! This file is the harness every area shares (`sandbox`, `Harness`, `setup`, the request
//! helpers); each area of the API has a file of its own.

mod boundary;
mod list_order;
mod proxies;
mod fixture;
mod listings;
mod tokens;
mod traffic;
mod call_log;
mod editing;
mod masking;
mod groups;
mod scope_family;
mod import;
mod vault;
mod scope_secrets;
mod scope_tokens;
mod scope_jobs;
mod oauth;
mod machine;
mod mcp_keys;
mod revisions;
mod knowledge;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use self::fixture::Fixture;
use swiss::app::{build_app, AppContext};
use swiss_host::config::ServerDef;
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::adapters::direct::DirectAdapter;
use swiss_mcp::adapters::{make_adapter, Adapter};
use swiss_mcp::registry::{Registry, Source};

const TOKEN: &str = "admin-tok-0123456789abcdef";

/// Point the whole test binary at a scratch data dir and one deterministic master key, before
/// anything can read either. The call log lives on disk under the data dir, and the state files
/// are sealed — without this the suite would write into the operator's real `~/.swiss` and
/// would need DPAPI (or a machine id) to seal with.
fn sandbox() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let home = std::env::temp_dir().join(format!(
            "swiss-adminapi-home-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&home).expect("create the scratch home");
        // Safety: this runs once, before any test has touched the paths or key modules, and both
        // variables are read fresh on every use rather than cached.
        unsafe {
            std::env::set_var("SWISS_HOME", &home);
            std::env::set_var("USERPROFILE", home.join("user"));
            std::env::set_var("HOME", home.join("user"));
            std::env::set_var("CLAUDE_CONFIG_DIR", home.join("user/.claude"));
            std::env::set_var("SWISS_MASTER_KEY", "ab".repeat(32));
        }
    });
}

/// One gateway: an empty registry, a managed store on a fresh file, and the router over both.
struct Harness {
    app: axum::Router,
    registry: Arc<Registry>,
    store: Arc<ManagedStore>,
    calls: Arc<swiss_mcp::calls::CallLog>,
    /// This harness's own traffic ring — memory-only, like every test composition here. What
    /// an MCP endpoint records is what /api/traffic of THIS harness reads, and no other test's
    /// rows can leak in, so the suite needs no process-level traffic lock any more.
    traffic: Arc<swiss_mcp::traffic::TrafficLog>,
    path: std::path::PathBuf,
}

fn setup() -> Harness {
    setup_with(|_| {})
}

/// setup(), with a hook on the context before the router is built over it — where a test mounts
/// the seams the boot sequence would set (the machine settings, below).
fn setup_with(configure: impl FnOnce(&AppContext)) -> Harness {
    sandbox();
    let path = std::env::temp_dir()
        .join(format!(
            "swiss-adminapi-{}",
            swiss_core::util::random_hex(8)
        ))
        .join("managed.json");
    std::fs::create_dir_all(path.parent().expect("the scratch file has a parent"))
        .expect("create the scratch directory");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(
        path.parent()
            .expect("the scratch file has a parent")
            .join("calls"),
    ));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(path.clone()));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    // The gateway's own port: the import planner uses it to recognise (and skip) entries that
    // already point at this gateway.
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        traffic.clone(),
        "SWISS_TOKEN",
        19999,
    );
    configure(&ctx);
    Harness {
        app: build_app(ctx, None),
        registry,
        store,
        calls,
        traffic,
        path,
    }
}

fn def(v: Value) -> ServerDef {
    ServerDef(v.as_object().cloned().expect("a def is an object"))
}

impl Harness {
    /// Register an MCP the way a config file would — no managed entry, no start.
    fn register(&self, name: &str, d: Value) {
        let d = def(d);
        let adapter = make_adapter(&d, name, &self.calls, None).expect("adapter");
        self.registry
            .register(name, Source::Config, d, adapter)
            .expect("register");
    }

    /// Register the fixture MCP under `name`, the way a config file would.
    fn register_fixture(&self, name: &str) {
        let d = def(json!({ "type": "fixture" }));
        let adapter: Arc<dyn Adapter> = Arc::new(DirectAdapter::new(
            &d,
            name,
            Fixture::new(name),
            self.calls.clone(),
        ));
        self.registry
            .register(name, Source::Config, d, adapter)
            .expect("register");
    }

    /// This MCP's live definition, as the registry holds it.
    fn def_of(&self, name: &str) -> Value {
        let entry = self.registry.get(name).expect("a registered MCP");
        let def = entry.data.read().expect("entry readable").def.clone();
        serde_json::to_value(def).unwrap_or(Value::Null)
    }

    /// Whether this MCP currently holds a built server — what stop frees and start rebuilds.
    fn has_server(&self, name: &str) -> bool {
        self.registry
            .get(name)
            .and_then(|e| e.data.read().ok().map(|d| d.server.is_some()))
            .unwrap_or(false)
    }

    /// One MCP request, as a hosted client sends it: bearer, and both reply media types.
    async fn mcp(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.send(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(Request::get(path).body(Body::empty()).expect("request"))
            .await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(json_req(Request::post(path), body)).await
    }

    async fn put(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(json_req(Request::put(path), body)).await
    }

    async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.send(Request::delete(path).body(Body::empty()).expect("request"))
            .await
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let mut req = req;
        // Every local client sends a Host; the loopback guard ahead of the routes reads it.
        req.headers_mut()
            .entry(header::HOST)
            .or_insert(header::HeaderValue::from_static("127.0.0.1:19999"));
        let res = self.app.clone().oneshot(req).await.expect("router answers");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
            .await
            .expect("body");
        let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// The names the panel would draw, in the order it would draw them.
    async fn names(&self) -> Vec<String> {
        let (status, body) = self.get("/api/mcps").await;
        assert_eq!(status, StatusCode::OK);
        body["mcps"]
            .as_array()
            .expect("mcps array")
            .iter()
            .map(|m| m["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// One row of a listing, by name — the panel finds a row the same way.
fn row_named(body: &Value, name: &str) -> Value {
    body["mcps"]
        .as_array()
        .and_then(|rows| rows.iter().find(|m| m["name"] == json!(name)))
        .cloned()
        .unwrap_or_else(|| panic!("no row for {name} in {body}"))
}

/// One string field lifted out of every element of an array.
fn field_of(items: &Value, key: &str) -> Vec<String> {
    items
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x[key].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn json_req(builder: axum::http::request::Builder, body: Value) -> Request<Body> {
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn rest_def(base: &str) -> Value {
    json!({
        "type": "rest",
        "baseUrl": base,
        "tools": [{ "name": "x", "request": { "method": "GET", "path": "/x" } }],
    })
}
