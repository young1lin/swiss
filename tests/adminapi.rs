/*
 * Copyright 2026 The swiss authors
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

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use async_trait::async_trait;
use swiss::app::{build_app, AppContext};
use swiss_host::config::ServerDef;
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::adapters::direct::DirectAdapter;
use swiss_mcp::adapters::resources::{
    ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate,
};
use swiss_mcp::adapters::tool_server::{Engine, ServerMeta, ToolDef};
use swiss_mcp::adapters::{make_adapter, Adapter};
use swiss_mcp::registry::{Registry, Source};

const TOKEN: &str = "admin-tok-0123456789abcdef";

/// Point the whole test binary at a scratch data dir and one deterministic master key, before
/// anything can read either. The call log lives on disk under the data dir, and the state files
/// are sealed — without this the suite would write into the operator's real `~/.mcp-gateway` and
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
            std::env::set_var("MCP_GATEWAY_HOME", &home);
            std::env::set_var("MCP_GATEWAY_MASTER_KEY", "ab".repeat(32));
        }
    });
}

/// One gateway: an empty registry, a managed store on a fresh file, and the router over both.
struct Harness {
    app: axum::Router,
    registry: Arc<Registry>,
    store: Arc<ManagedStore>,
    calls: Arc<swiss_mcp::calls::CallLog>,
    path: std::path::PathBuf,
}

fn setup() -> Harness {
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
        "MCP_GATEWAY_TOKEN",
        19999,
    );
    Harness {
        app: build_app(ctx, None),
        registry,
        store,
        calls,
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
        let adapter = make_adapter(&d, name, &self.calls).expect("adapter");
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

// --- the boundary --------------------------------------------------------------------------

#[tokio::test]
async fn serves_reads_and_mutations_without_any_credentials() {
    // The panel has no login: the loopback guard ahead of every route IS the boundary, so /api
    // answers any caller that got this far (i.e. this machine). The MCP endpoints stay gated.
    let h = setup();
    assert_eq!(h.get("/api/mcps").await.0, StatusCode::OK);
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "x", "command": "node x" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn has_no_login_route_to_answer() {
    // The credential check is gone, not bypassed — there is nothing there to post to.
    let h = setup();
    let (status, _) = h
        .post(
            "/api/login",
            json!({ "username": "admin", "password": "admin" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- the list ------------------------------------------------------------------------------

#[tokio::test]
async fn tags_each_mcp_row_with_how_it_is_launched() {
    let h = setup();
    let cases: Vec<(&str, Value, &str)> = vec![
        ("t-echo", json!({ "type": "echo" }), "echo"),
        (
            "t-http",
            json!({ "type": "http", "url": "https://example.invalid/mcp" }),
            "http",
        ),
        ("t-rest", rest_def("https://example.invalid"), "rest"),
        (
            "t-npx",
            json!({ "type": "proc", "command": "npx -y @some/server" }),
            "npx",
        ),
        (
            "t-uvx",
            json!({ "type": "proc", "command": "uvx mcp-server-x" }),
            "uvx",
        ),
        (
            "t-docker",
            json!({ "type": "proc", "command": "docker run -i mcp/x" }),
            "docker",
        ),
        (
            "t-cmd",
            json!({ "type": "proc", "command": "node server.js" }),
            "proc",
        ),
        (
            "t-mysql",
            json!({ "type": "mysql", "host": "127.0.0.1" }),
            "mysql",
        ),
    ];
    for (name, d, _) in &cases {
        h.register(name, d.clone());
    }
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["mcps"].as_array().expect("mcps array");
    for (name, _, tag) in &cases {
        let row = rows
            .iter()
            .find(|m| m["name"] == json!(name))
            .unwrap_or_else(|| panic!("no row for {name}"));
        assert_eq!(row["tag"], json!(tag), "tag of {name}");
    }
}

#[tokio::test]
async fn sorts_by_name_until_the_user_arranges_the_list() {
    let h = setup();
    // Registered in an order nobody would choose to look at, to prove registration order is not it.
    for n in ["zeta", "mid", "alpha"] {
        h.register(n, json!({ "type": "echo" }));
    }
    assert_eq!(h.names().await, ["alpha", "mid", "zeta"]);
}

#[tokio::test]
async fn appends_mcps_the_user_has_never_positioned_in_name_order() {
    let h = setup();
    for n in ["a", "zeta", "mid", "alpha"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put("/api/groups/mcps/order", json!({ "order": ["zeta", "a"] }))
        .await;
    // The arrangement wins for the two it names; the rest sort among themselves rather than
    // landing wherever the registry happened to start them.
    assert_eq!(h.names().await, ["zeta", "a", "alpha", "mid"]);
}

#[tokio::test]
async fn goes_back_to_name_order_when_the_arrangement_is_cleared() {
    let h = setup();
    for n in ["b", "c", "a"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put(
        "/api/groups/mcps/order",
        json!({ "order": ["c", "b", "a"] }),
    )
    .await;
    h.put("/api/groups/mcps/order", json!({ "order": [] }))
        .await;
    assert_eq!(h.names().await, ["a", "b", "c"]);
}

#[tokio::test]
async fn persists_a_panel_order_and_lists_by_it_appending_unknown_names() {
    let h = setup();
    for n in ["a", "b", "c"] {
        h.register(n, json!({ "type": "echo" }));
    }
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": ["c", "a"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.names().await, ["c", "a", "b"], "b is not in the order");
    // It survives a restart of the store, because it is on disk and not in the registry.
    assert_eq!(
        ManagedStore::open_at(h.path.clone()).get_order(),
        ["c", "a"]
    );
}

#[tokio::test]
async fn rejects_a_malformed_order_payload() {
    let h = setup();
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": "c,a" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn keeps_a_renamed_mcps_position_and_prunes_a_deleted_one_from_the_order() {
    let h = setup();
    for n in ["a", "b", "c"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put(
        "/api/groups/mcps/order",
        json!({ "order": ["a", "b", "c"] }),
    )
    .await;
    let (status, _) = h.post("/api/mcps/a/rename", json!({ "name": "z" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.store.get_order(), ["z", "b", "c"]);
    // A deleted managed MCP leaves the order; config MCPs cannot be deleted, so prune via the
    // store the way the delete route does.
    h.store.remove("b").expect("remove b");
    assert_eq!(h.store.get_order(), ["z", "c"]);
}

// --- proxies -------------------------------------------------------------------------------

#[tokio::test]
async fn stores_and_returns_a_per_mcp_proxy_on_http_and_rest_mcps() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "prox-http", "type": "http",
                "url": "https://example.invalid/mcp",
                "proxy": "http://127.0.0.1:7890",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, det) = h.get("/api/mcps/prox-http/details").await;
    assert_eq!(det["config"]["proxy"], json!("http://127.0.0.1:7890"));

    let mut rest = rest_def("https://example.invalid");
    rest["name"] = json!("prox-rest");
    rest["proxy"] = json!("${MY_PROXY}");
    let (status, _) = h.post("/api/mcps", rest).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, det) = h.get("/api/mcps/prox-rest/details").await;
    // An env ref stays a ref: the panel never sees the resolved value, and neither does the file.
    assert_eq!(det["config"]["proxy"], json!("${MY_PROXY}"));
}

#[tokio::test]
async fn rejects_a_proxy_that_is_not_an_http_url() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "bad-proxy", "type": "http",
                "url": "https://example.invalid/mcp",
                "proxy": "127.0.0.1:7890",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("proxy"),
        "{body}"
    );

    let mut rest = rest_def("https://example.invalid");
    rest["name"] = json!("bad-proxy-rest");
    rest["proxy"] = json!("socks5://x");
    let (status, body) = h.post("/api/mcps", rest).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("proxy"),
        "{body}"
    );
}

// --- a fixture MCP -----------------------------------------------------------------------------

/// An in-process MCP with a tool, a resource and a health probe — the Rust stand-in for the Node
/// suite's `fixtures/stdio-echo.mjs`.
///
/// The Node build spawned a child `node` whenever a test needed something with resources to page
/// and a probe that answers. A Rust test has no `node` to lean on and needs none: `DirectAdapter`
/// serves any `Engine`, and nothing in the admin API can tell this apart from a database adapter.
struct Fixture {
    name: std::sync::RwLock<String>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self {
            name: std::sync::RwLock::new(name.to_string()),
        }
    }

    fn name(&self) -> String {
        self.name.read().map(|n| n.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Engine for Fixture {
    fn kind(&self) -> &'static str {
        "fixture"
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "echo".into(),
            description: "echo back the message".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"],
            }),
        }]
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        if tool != "echo" {
            return Err(format!("unknown tool: {tool}"));
        }
        Ok(json!({ "echo": args.get("msg").cloned().unwrap_or(Value::Null) }))
    }

    fn meta(&self) -> ServerMeta {
        ServerMeta {
            name: Some(self.name()),
            description: None,
            target: None,
            limits: None,
        }
    }

    fn resources(&self) -> Option<Arc<dyn ResourceProvider>> {
        Some(Arc::new(FixtureResources {
            uri: format!("echo://{}", self.name()),
        }))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        Some(Ok(()))
    }

    fn rename(&self, next: &str) {
        if let Ok(mut name) = self.name.write() {
            *name = next.to_string();
        }
    }
}

/// One resource, named after the MCP — enough to prove the resources page is served from the
/// engine rather than from a cache.
struct FixtureResources {
    uri: String,
}

#[async_trait]
impl ResourceProvider for FixtureResources {
    async fn list(&self, _cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        Ok(ResourcePage {
            resources: vec![ResourceEntry {
                uri: self.uri.clone(),
                name: "echo".into(),
                description: None,
                mime_type: Some("text/plain".into()),
            }],
            next_cursor: None,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        if uri != self.uri {
            return Err(ResourceFault(format!("unknown resource: {uri}")));
        }
        Ok(vec![ResourceBody {
            uri: self.uri.clone(),
            mime_type: Some("text/plain".into()),
            text: "hello".into(),
        }])
    }
}

// --- lifecycle, listings and details ------------------------------------------------------------

#[tokio::test]
async fn adds_an_mcp_and_starts_it_while_the_list_stays_status_only() {
    let h = setup();
    let (status, body) = h
        .post("/api/mcps", json!({ "name": "echo1", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["lifecycle"], json!("started"));

    // The live gateway probes on boot; here the probe is triggered explicitly.
    h.registry.check_all().await;

    // The list is light: state only, never the tools and resources arrays — the panel polls it.
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    let row = row_named(&body, "echo1");
    assert_eq!(row["lifecycle"], json!("started"));
    assert!(row.get("tools").is_none(), "{row}");
    assert!(row.get("resources").is_none(), "{row}");
}

#[tokio::test]
async fn reports_a_started_mcp_as_up_once_its_probe_answers() {
    let h = setup();
    h.register_fixture("f1");
    h.registry.start("f1").await.expect("start");
    h.registry.check_all().await;

    let (_, body) = h.get("/api/mcps").await;
    assert_eq!(row_named(&body, "f1")["state"], json!("up"));
}

#[tokio::test]
async fn pages_tools_and_resources_on_demand_while_details_carries_logs() {
    let h = setup();
    h.register_fixture("f2");
    h.registry.start("f2").await.expect("start");

    let (status, tools) = h.get("/api/mcps/f2/tools").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&tools["tools"], "name").contains(&"echo".to_string()),
        "{tools}"
    );
    // Everything fits on one page, so there is nothing to continue with.
    assert!(tools.get("nextCursor").is_none(), "{tools}");

    let (status, resources) = h.get("/api/mcps/f2/resources").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&resources["resources"], "uri").contains(&"echo://f2".to_string()),
        "{resources}"
    );

    // Details carries the captured output and the config — never the listings.
    let (status, details) = h.get("/api/mcps/f2/details").await;
    assert_eq!(status, StatusCode::OK);
    assert!(details.get("tools").is_none(), "{details}");
    assert!(details["logs"].is_string(), "{details}");
}

#[tokio::test]
async fn answers_a_listing_for_an_mcp_that_was_never_started() {
    // Not a Node case: the Node fixture always started. The Rust panel polls a stopped MCP too,
    // and the answer must be the 503 it renders as "disabled", never a 500.
    let h = setup();
    h.register_fixture("f3");
    assert_eq!(
        h.get("/api/mcps/f3/tools").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        h.get("/api/mcps/ghost/tools").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.get("/api/mcps/f3/nonsense").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn forwards_mcp_traffic_to_the_added_server() {
    let _lock = traffic_lock().await;
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "e2", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h
        .mcp(
            "/mcp/e2",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

// --- tokens ---------------------------------------------------------------------------------

#[tokio::test]
async fn exposes_the_tokens_env_var_name_never_the_token() {
    let h = setup();
    let (status, body) = h.get("/api/info").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tokenEnv"], json!("MCP_GATEWAY_TOKEN"));
    assert!(!body.to_string().contains(TOKEN), "{body}");
    // The build stamp (docs/16 H3) — present, and never a secret either.
    assert_eq!(body["build"]["hash"], json!(env!("SWISS_GIT_HASH")));
    assert!(!body["build"].to_string().contains(TOKEN), "{body}");
}

#[tokio::test]
async fn lists_creates_revokes_and_rotates_named_tokens() {
    // The MCP posts below answer through the traffic ring; without the lock a parallel
    // traffic-counting test can read one of this test's rows (seen once as 11-vs-10 in a
    // full-parallel gate run). The lock is the file's rule for any MCP-driving test.
    let _lock = traffic_lock().await;
    let h = setup();
    // The migrated seed is present as the "default" token, and the list carries no secrets.
    let (status, body) = h.get("/api/tokens").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&body["tokens"], "label").contains(&"default".to_string()),
        "{body}"
    );
    assert!(!body["tokens"].to_string().contains(TOKEN), "{body}");

    // Create a named token; its secret comes back once and authenticates an MCP request.
    let (status, made) = h
        .post("/api/tokens", json!({ "label": "claude-code" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(made["label"], json!("claude-code"));
    let secret = made["secret"].as_str().expect("a secret").to_string();
    assert!(
        secret.len() == 48 && secret.chars().all(|c| c.is_ascii_hexdigit()),
        "{secret}"
    );
    let id = made["id"].as_str().expect("an id").to_string();
    // 503, not 401: the token is good, the MCP name is not one this gateway serves.
    assert_eq!(
        h.mcp("/mcp/anything", &secret, json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // Rotate: a new secret under the same id; the old one stops working at the gate.
    let (status, rotated) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rotated["id"], json!(id));
    let next = rotated["secret"].as_str().expect("a secret").to_string();
    assert_ne!(next, secret);
    assert_eq!(
        h.mcp("/mcp/anything", &secret, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.mcp("/mcp/anything", &next, json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // Revoke: gone, no longer authenticates, 404 on a second attempt.
    assert_eq!(
        h.delete(&format!("/api/tokens/{id}")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        h.mcp("/mcp/anything", &next, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.delete(&format!("/api/tokens/{id}")).await.0,
        StatusCode::NOT_FOUND
    );

    // Persisted: a fresh read has the default token, the created one having been revoked.
    let labels: Vec<String> = ManagedStore::open_at(h.path.clone())
        .get_tokens()
        .iter()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(labels, ["default"]);
}

#[tokio::test]
async fn hands_a_stored_secret_back_so_a_connect_command_needs_no_rotate() {
    let h = setup();
    let (status, made) = h
        .post("/api/tokens", json!({ "label": "claude-code" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = made["id"].as_str().expect("an id").to_string();

    let (status, got) = h.get(&format!("/api/tokens/{id}/secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        got,
        json!({ "id": id, "label": "claude-code", "secret": made["secret"] })
    );
}

#[tokio::test]
async fn keeps_secrets_out_of_the_list_and_the_secret_route_open() {
    let h = setup();
    let (_, made) = h.post("/api/tokens", json!({ "label": "a" })).await;
    let id = made["id"].as_str().expect("an id").to_string();

    // The list is what the panel polls; reading a secret stays a separate, explicit request.
    let (_, list) = h.get("/api/tokens").await;
    for token in list["tokens"].as_array().expect("tokens array") {
        assert!(token.get("secret").is_none(), "{token}");
    }
    // There is no credential gate: the loopback guard is the boundary for the secret route too.
    assert_eq!(
        h.get(&format!("/api/tokens/{id}/secret")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn answers_404_for_an_unknown_token_id() {
    let h = setup();
    assert_eq!(
        h.get("/api/tokens/nope/secret").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn returns_the_new_secret_after_a_rotate_not_the_old_one() {
    let h = setup();
    let (_, made) = h.post("/api/tokens", json!({ "label": "a" })).await;
    let id = made["id"].as_str().expect("an id").to_string();
    let (_, rotated) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;

    let (_, got) = h.get(&format!("/api/tokens/{id}/secret")).await;
    assert_eq!(got["secret"], rotated["secret"]);
    assert_ne!(got["secret"], made["secret"]);
}

// --- traffic ---------------------------------------------------------------------------------

/// The traffic ring is one process-wide log, exactly as it is in the running gateway: two tests
/// writing into it at once would read each other's rows. Every test that reads it — and every
/// test that drives an MCP endpoint, since answering one records a row — takes this first and
/// starts from an empty ring. A panicking test must not lock the rest out, so the poison is
/// stepped over — a tokio mutex has no poison to propagate, and the ring it guards is rebuilt by
/// the next `clear` anyway. It is a tokio mutex rather than a std one because the guard is held
/// across the awaits of a whole test.
async fn traffic_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    swiss_mcp::traffic::clear_traffic(None);
    guard
}

/// The bearer headers a hosted MCP client sends.
fn init_frame(name: &str, version: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-05",
            "capabilities": {},
            "clientInfo": { "name": name, "version": version },
        },
    })
}

/// The entry for one method, out of the traffic listing.
fn entry_for(body: &Value, method: &str) -> Value {
    body["entries"]
        .as_array()
        .and_then(|rows| rows.iter().find(|e| e["method"] == json!(method)))
        .cloned()
        .unwrap_or_else(|| panic!("no {method} entry in {body}"))
}

/// One echo MCP, started and reachable at `/mcp/echo`.
async fn with_echo() -> Harness {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "echo", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h
}

#[tokio::test]
async fn records_mcp_traffic_attributed_to_the_token_and_the_self_reported_client() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;
    h.mcp("/mcp/echo", TOKEN, init_frame("claude-code", "1.2.3"))
        .await;

    let (status, body) = h.get("/api/traffic").await;
    assert_eq!(status, StatusCode::OK);
    let hit = entry_for(&body, "initialize");
    assert_eq!(hit["client"], json!("default")); // the token label that authenticated
    assert_eq!(hit["clientName"], json!("claude-code")); // self-reported during initialize
    assert_eq!(hit["clientVersion"], json!("1.2.3"));
}

#[tokio::test]
async fn attributes_a_server_discover_frame_via_its_meta_client_info() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;
    // server/discover (Claude Code's capability probe) carries clientInfo nested in _meta, not at
    // the top level like initialize — the split must lift it from there too.
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "server/discover",
            "params": { "_meta": { "io.modelcontextprotocol/clientInfo": {
                "name": "claude-code", "version": "2.1.232",
            } } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    let hit = entry_for(&body, "server/discover");
    assert_eq!(hit["clientName"], json!("claude-code"));
    assert_eq!(hit["clientVersion"], json!("2.1.232"));
}

#[tokio::test]
async fn attributes_a_tokens_later_frames_to_the_name_it_announced_at_initialize() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;
    // initialize announces the client name once; the tools/list after it carries no clientInfo,
    // but it is the same token — so it inherits "claude-code" rather than appearing as a second,
    // unknown client.
    h.mcp("/mcp/echo", TOKEN, init_frame("claude-code", "2.1.88"))
        .await;
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    assert_eq!(
        entry_for(&body, "tools/list")["clientName"],
        json!("claude-code")
    );
}

#[tokio::test]
async fn stores_the_full_redacted_request_body_for_the_expandable_raw_view() {
    let _lock = traffic_lock().await;
    let h = with_echo().await;
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "hi", "password": "shh" } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    let hit = entry_for(&body, "tools/call");
    // The list row carries no payload — only the flag the collapsed row needs. The body arrives
    // from the per-entry endpoint when the row is expanded.
    assert!(hit.get("body").is_none(), "{hit}");
    assert!(hit.get("response").is_none(), "{hit}");
    assert_eq!(hit["hasResponse"], json!(true));

    let seq = hit["seq"].as_u64().expect("a seq");
    let (status, one) = h.get(&format!("/api/traffic/{seq}")).await;
    assert_eq!(status, StatusCode::OK);
    let raw = one["body"].as_str().expect("a stored body");
    assert!(raw.contains(r#""method":"tools/call""#), "{raw}"); // the whole envelope, not just params
    assert!(raw.contains(r#""msg":"hi""#), "{raw}");
    assert!(!raw.contains("shh"), "{raw}"); // a secret value is never stored
    assert!(raw.contains(r#""password":"•••""#), "{raw}"); // key kept, value redacted
    let reply = one["response"].as_str().expect("a stored reply");
    assert!(reply.contains("result"), "{reply}"); // the reply is captured too, not only the request
    assert!(reply.contains("hi"), "{reply}"); // the echoed text
}

#[tokio::test]
async fn pages_the_activity_log_newest_first_and_filters_to_actions_server_side() {
    let _lock = traffic_lock().await;
    let h = setup();
    // 7 protocol frames + 3 actions, interleaved so a naive slice cannot pass by accident.
    for i in 0..10 {
        let action = i % 3 == 2;
        swiss_mcp::traffic::record_traffic(
            "m",
            Some(&json!({
                "jsonrpc": "2.0", "id": i,
                "method": if action { "tools/call" } else { "tools/list" },
            })),
            Some("pg"),
            true,
            1,
            None,
        );
    }

    let (_, p0) = h.get("/api/traffic?pageSize=4").await;
    assert_eq!(p0["entries"].as_array().map(Vec::len), Some(4));
    assert_eq!(p0["total"], json!(10));
    assert_eq!(p0["more"], json!(true));
    // Newest first, and page 1 continues where page 0 stopped rather than repeating it.
    let seqs0: Vec<u64> = p0["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["seq"].as_u64().unwrap_or_default())
        .collect();
    let mut descending = seqs0.clone();
    descending.sort_by(|a, b| b.cmp(a));
    assert_eq!(seqs0, descending);
    let (_, p1) = h.get("/api/traffic?pageSize=4&page=1").await;
    let seqs1: Vec<u64> = p1["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["seq"].as_u64().unwrap_or_default())
        .collect();
    assert_eq!(seqs1, seqs0.iter().map(|s| s - 4).collect::<Vec<_>>());

    // The last page reports no more, and a page past the end is empty rather than an error.
    let (_, p2) = h.get("/api/traffic?pageSize=4&page=2").await;
    assert_eq!(p2["entries"].as_array().map(Vec::len), Some(2));
    assert_eq!(p2["more"], json!(false));
    let (status, p9) = h.get("/api/traffic?pageSize=4&page=9").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p9["entries"], json!([]));

    // actions=1 filters BEFORE paging: the count is of matching rows, not of rows on this page.
    let (_, act) = h.get("/api/traffic?actions=1").await;
    assert!(
        act["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|e| e["method"] == json!("tools/call")),
        "{act}"
    );
    assert_eq!(act["total"], json!(3));
    assert_eq!(act["totalUnfiltered"], json!(10)); // the "3 of 10 interactions" readout
}

#[tokio::test]
async fn folds_clients_over_the_whole_ring_not_over_the_returned_page() {
    let _lock = traffic_lock().await;
    let h = setup();
    let record = |mcp: &str, body: Value, token: &str| {
        swiss_mcp::traffic::record_traffic(mcp, Some(&body), Some(token), true, 1, None);
    };
    record(
        "m1",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "clientInfo": { "name": "app-a", "version": "1" } } }),
        "ta",
    );
    record(
        "m2",
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        "ta",
    );
    for i in 0..8 {
        record(
            "m3",
            json!({ "jsonrpc": "2.0", "id": 10 + i, "method": "tools/list" }),
            "tb",
        );
    }

    // Page 0 at this size holds only tb's rows; ta must still appear in the summary.
    let (_, body) = h.get("/api/traffic?pageSize=3").await;
    assert!(
        body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|e| e["client"] == json!("tb")),
        "{body}"
    );
    let keys = field_of(&body["clients"], "key");
    assert!(keys.contains(&"n:app-a".to_string()), "{body}"); // named by its initialize
    assert!(keys.contains(&"t:tb".to_string()), "{body}");
    let a = body["clients"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["key"] == json!("n:app-a")))
        .cloned()
        .expect("a row for app-a");
    assert_eq!(a["label"], json!("app-a 1"));
    assert_eq!(a["count"], json!(2)); // both of ta's rows, though neither is on this page
    assert_eq!(a["mcps"], json!(["m1", "m2"]));
    assert_eq!(a["tokens"], json!(["ta"]));
}

#[tokio::test]
async fn answers_404_for_a_traffic_entry_that_has_rolled_out_of_the_ring() {
    let _lock = traffic_lock().await;
    let h = setup();
    assert_eq!(h.get("/api/traffic/999999").await.0, StatusCode::NOT_FOUND);
    // A seq that is not a number is a bad request, not a miss.
    assert_eq!(h.get("/api/traffic/nope").await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn clears_one_clients_traffic_leaving_the_others_intact() {
    let _lock = traffic_lock().await;
    let h = setup();
    for token in ["clrA", "clrB"] {
        swiss_mcp::traffic::record_traffic(
            "m",
            Some(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })),
            Some(token),
            true,
            1,
            None,
        );
    }
    let (_, before) = h.get("/api/traffic").await;
    assert_eq!(before["entries"].as_array().map(Vec::len), Some(2));

    // The panel sends the prefixed key it was given in the clients summary.
    let (status, deleted) = h.delete("/api/traffic?client=t:clrA").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["client"], json!("t:clrA"));
    let (_, after) = h.get("/api/traffic").await;
    assert_eq!(field_of(&after["entries"], "client"), ["clrB"]);

    // A bare DELETE (no client) still clears everything.
    assert_eq!(h.delete("/api/traffic").await.0, StatusCode::OK);
    let (_, none) = h.get("/api/traffic").await;
    assert_eq!(none["entries"], json!([]));
}

// --- the call log ----------------------------------------------------------------------------

/// One started echo MCP under a name of the caller's choosing. The call log is filed by MCP name
/// and lives on disk, so each test picks its own name and never sees another test's rows.
async fn with_echo_named(name: &str) -> Harness {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": name, "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h
}

#[tokio::test]
async fn pages_the_call_log_and_serves_one_reply_in_full() {
    // The panel /call posts below land in the shared traffic ring; without the lock a parallel
    // traffic test saw one of these rows as a stray "default" client (2026-09-16, full-suite
    // run after the docs/31 binary shifted thread interleaving) — the same family as the
    // 11-vs-10 note above. Serialize like every other ring writer.
    let _lock = traffic_lock().await;
    let h = with_echo_named("pg1").await;
    for i in 0..3 {
        let (status, _) = h
            .post(
                "/api/mcps/pg1/call",
                json!({ "tool": "echo", "arguments": { "msg": format!("m{i}") } }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, page) = h.get("/api/mcps/pg1/calls?page=0").await;
    assert_eq!(status, StatusCode::OK);
    let calls = page["calls"].as_array().expect("calls array").clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(page["page"], json!(0));
    assert_eq!(page["more"], json!(false));
    assert_eq!(calls[0]["output"], json!("m2")); // newest first

    let seq = calls[0]["seq"].as_u64().expect("a seq");
    let (status, one) = h.get(&format!("/api/mcps/pg1/calls/{seq}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["call"]["output"], json!("m2"));
    assert_eq!(
        h.get("/api/mcps/pg1/calls/9999").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn records_every_tool_call_with_its_source_and_clears_on_request() {
    let _lock = traffic_lock().await;
    let h = with_echo_named("cl").await;

    let (status, empty) = h.get("/api/mcps/cl/calls").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["calls"], json!([]));

    // Once through the panel, once as an MCP client on the HTTP endpoint.
    let (_, run) = h
        .post(
            "/api/mcps/cl/call",
            json!({ "tool": "echo", "arguments": { "msg": "from-panel" } }),
        )
        .await;
    assert_eq!(run["text"], json!("from-panel"));
    h.mcp(
        "/mcp/cl",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "from-client" } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/mcps/cl/calls").await;
    let calls = body["calls"].as_array().expect("calls array").clone();
    assert_eq!(calls.len(), 2);
    // Newest first: the client's call, then the panel's.
    assert_eq!(calls[0]["tool"], json!("echo"));
    assert_eq!(calls[0]["ok"], json!(true));
    assert_eq!(calls[0]["via"], json!("mcp"));
    assert!(
        calls[0]["args"]
            .as_str()
            .unwrap_or_default()
            .contains("from-client"),
        "{}",
        calls[0]
    );
    assert_eq!(calls[0]["output"], json!("from-client"));
    assert!(calls[0]["ms"].is_number(), "{}", calls[0]);
    assert_eq!(calls[1]["tool"], json!("echo"));
    assert_eq!(calls[1]["via"], json!("panel"));

    let (status, _) = h.delete("/api/mcps/cl/calls").await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = h.get("/api/mcps/cl/calls").await;
    assert_eq!(after["calls"], json!([]));
}

#[tokio::test]
async fn serves_one_tools_recent_runs_for_the_run_tab_dropdown() {
    // The Run tab's refill dropdown: one tool's newest runs, newest first, both sources included.
    // The full arguments of a picked entry come back from the per-seq route — the history itself
    // carries only a one-line preview, so 300 entries stay light.
    let _lock = traffic_lock().await;
    let h = with_echo_named("hist").await;
    h.post(
        "/api/mcps/hist/call",
        json!({ "tool": "echo", "arguments": { "msg": "from-panel" } }),
    )
    .await;
    h.mcp(
        "/mcp/hist",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "from-client" } },
        }),
    )
    .await;

    let (status, hist) = h.get("/api/mcps/hist/tool-history?tool=echo&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    let entries = hist["entries"].as_array().expect("entries").clone();
    assert_eq!(entries.len(), 1); // limit honoured: only the newest
    assert_eq!(entries[0]["via"], json!("mcp"));
    assert!(
        entries[0]["args"]
            .as_str()
            .unwrap_or_default()
            .contains("from-client"),
        "{}",
        entries[0]
    );

    // The dropdown's search box: q narrows the list to runs whose FULL arguments contain it.
    let (status, filtered) = h
        .get("/api/mcps/hist/tool-history?tool=echo&q=from-client")
        .await;
    assert_eq!(status, StatusCode::OK);
    // "from-panel" does not contain "from-client".
    assert_eq!(filtered["entries"].as_array().map(Vec::len), Some(1));
    let (status, missed) = h
        .get("/api/mcps/hist/tool-history?tool=echo&q=no-such-text")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(missed["entries"], json!([]));

    // A picked entry's complete arguments, via the route the dropdown fetches on selection.
    let seq = entries[0]["seq"].as_u64().expect("a seq");
    let (_, one) = h.get(&format!("/api/mcps/hist/calls/{seq}")).await;
    let args: Value = serde_json::from_str(one["call"]["args"].as_str().expect("args"))
        .expect("the stored arguments are JSON");
    assert_eq!(args, json!({ "msg": "from-client" }));

    // tool is required, and an unknown MCP is a miss rather than an empty history.
    assert_eq!(
        h.get("/api/mcps/hist/tool-history").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.get("/api/mcps/nope/tool-history?tool=x").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn reads_one_resource_from_the_panel_without_handing_it_the_token() {
    // The counterpart of the tool runner, for the other primitive: the browser never holds the
    // gateway token, so a resource preview goes through /api rather than the MCP endpoint.
    let h = setup();
    h.register_fixture("res");
    h.registry.start("res").await.expect("start");

    let (status, body) = h
        .post("/api/mcps/res/resource", json!({ "uri": "echo://res" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["text"].as_str().unwrap_or_default().contains("hello"),
        "{body}"
    );

    assert_eq!(
        h.post("/api/mcps/res/resource", json!({})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.post("/api/mcps/ghost/resource", json!({ "uri": "echo://res" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

// --- editing, lifecycle and validation -------------------------------------------------------

/// The mask the panel receives in place of a credential, and sends back unchanged when the
/// operator saves a form without touching it.
const MASK: &str = "••••••••";

#[tokio::test]
async fn edits_a_managed_mcp_and_restarts_with_the_new_config() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "ed", "type": "mysql", "host": "127.0.0.1",
                "user": "app", "database": "v1",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, put) = h
        .put(
            "/api/mcps/ed",
            json!({ "type": "mysql", "host": "127.0.0.1", "user": "app", "database": "v2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // The edit rebuilds the adapter and leaves it running, rather than dropping the MCP.
    assert_eq!(put["lifecycle"], json!("started"));
    let (_, details) = h.get("/api/mcps/ed/details").await;
    assert_eq!(details["config"]["database"], json!("v2"));
    // The edit is on disk, not only in the registry.
    let stored = ManagedStore::open_at(h.path.clone());
    let entry = stored
        .all()
        .into_iter()
        .find(|m| m.name == "ed")
        .expect("a stored entry");
    assert_eq!(entry.def.get("database"), Some(&json!("v2")));
}

#[tokio::test]
async fn edits_a_config_file_mcp_and_persists_an_override() {
    let h = setup();
    h.register("cfg", json!({ "type": "echo" }));
    h.registry.start("cfg").await.expect("start");

    let (status, put) = h.put("/api/mcps/cfg", json!({ "type": "echo" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["lifecycle"], json!("started"));
    assert!(
        h.store.has("cfg"),
        "the override is persisted to managed.json"
    );
}

#[tokio::test]
async fn stop_frees_the_server_and_start_brings_it_back() {
    let h = setup();
    h.register_fixture("e4");
    h.registry.start("e4").await.expect("start");
    assert!(h.has_server("e4"));

    let (status, _) = h.post("/api/mcps/e4/stop", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.has_server("e4"));

    let (status, _) = h.post("/api/mcps/e4/start", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(h.has_server("e4"));

    // A restart is the pair of the two, and lands started.
    let (status, body) = h.post("/api/mcps/e4/restart", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], json!("started"));
    assert!(h.has_server("e4"));
}

#[tokio::test]
async fn renames_a_managed_mcp() {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "e5", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h
        .post("/api/mcps/e5/rename", json!({ "name": "e5b" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("e5"));
    assert!(h.registry.has("e5b"));
    assert!(!h.store.has("e5") && h.store.has("e5b"));
}

#[tokio::test]
async fn deletes_a_managed_mcp_and_updates_the_store() {
    let h = setup();
    h.post("/api/mcps", json!({ "name": "e6", "type": "echo" }))
        .await;
    assert!(h.store.has("e6"));
    let (status, _) = h.delete("/api/mcps/e6").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("e6"));
    assert!(!h.store.has("e6"));
}

#[tokio::test]
async fn deletes_a_config_mcp_even_when_no_config_file_holds_it() {
    // The old contract (refuse: deleting a config MCP left its file entry behind, so it
    // resurrected) became a real delete: the endpoint removes the gateway.config.json entry too.
    // With NO file present — this test's data dir has none — the runtime half still deletes.
    let h = setup();
    h.register("cfg", json!({ "type": "echo" }));
    let (status, _) = h.delete("/api/mcps/cfg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("cfg"));
}

#[tokio::test]
async fn an_mcp_named_test_or_import_stays_deletable_and_editable() {
    // The collection-level actions ("test this def", "import defs") used to sit at
    // /api/mcps/test and /api/mcps/import — static segments at the {name} position of the
    // router. axum prefers a static match over the parameter, so DELETE /api/mcps/test landed
    // on the POST-only test route and fell to the terminal 404 ("no route for DELETE
    // /api/mcps/test"): an MCP named "test" (or "import") could be created but never edited
    // or deleted. Those actions now live under /api/mcpdefs/*, a namespace no MCP name can
    // shadow; this test pins the per-name verbs for both poisoned words.
    //
    // echo, not the http def the sighting came with: the shadow is about the NAME, and an
    // http add fires a real outbound handshake whose traffic row would leak into the
    // process-global ring other tests count.
    let h = setup();
    for name in ["test", "import"] {
        let (status, body) = h
            .post("/api/mcps", json!({ "name": name, "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create {name}: {body}");

        // PUT (edit) rides the same shadowed path as DELETE; both must reach the handlers.
        let (status, body) = h
            .put(&format!("/api/mcps/{name}"), json!({ "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::OK, "edit {name}: {body}");

        let (status, body) = h.delete(&format!("/api/mcps/{name}")).await;
        assert_eq!(status, StatusCode::OK, "delete {name}: {body}");
        assert_eq!(body["deleted"], json!(true));
        assert!(!h.registry.has(name), "the registry drops {name}");
        assert!(!h.store.has(name), "the managed store drops {name}");
    }
}

#[tokio::test]
async fn persists_added_mcps_to_managed_json() {
    let h = setup();
    h.post("/api/mcps", json!({ "name": "persist1", "type": "echo" }))
        .await;
    let reloaded = ManagedStore::open_at(h.path.clone());
    assert!(reloaded.all().iter().any(|m| m.name == "persist1"));
}

#[tokio::test]
async fn plugin_domain_names_need_no_host_reservation() {
    // docs/24 P2: inside /mcp/* the host's root reserved words are gone — health/api/admin/
    // mcp are ordinary MCP names there, creatable and reachable, while the host's own
    // routes at the root keep answering untouched.
    let h = setup();
    for name in ["health", "api", "admin", "mcp"] {
        let (status, body) = h
            .post("/api/mcps", json!({ "name": name, "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{name}: {body}");
    }
    // Reachable: /mcp/health is the MCP, /health is the host liveness route.
    let (status, _) = h
        .mcp(
            "/mcp/health",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/health").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rejects_an_invalid_name_and_a_duplicate() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({ "name": "bad name!", "command": "node x" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.post("/api/mcps", json!({ "name": "dup", "type": "echo" }))
        .await;
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "dup", "command": "node x" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn keeps_cwd_and_the_expose_flags_across_an_edit() {
    // Regression: the panel had no cwd field, so build_def never received one and editing a proc
    // MCP silently dropped it (same for exposeResources/exposePrompts).
    let h = setup();
    let here = std::env::temp_dir().to_string_lossy().into_owned();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "keep", "command": "node server.js", "cwd": here,
                "exposeResources": false, "exposePrompts": false, "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.def_of("keep")["cwd"], json!(here));
    assert_eq!(h.def_of("keep")["exposeResources"], json!(false));

    let (status, _) = h
        .put(
            "/api/mcps/keep",
            json!({
                "type": "proc", "command": "node server.js", "cwd": here,
                "exposeResources": false, "exposePrompts": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.def_of("keep")["cwd"], json!(here));
    assert_eq!(h.def_of("keep")["exposeResources"], json!(false));
    assert_eq!(h.def_of("keep")["exposePrompts"], json!(false));
}

#[tokio::test]
async fn adds_a_remote_http_mcp_and_never_hands_its_key_to_the_browser() {
    // A remote MCP is configured, not launched: url + the headers its key travels in. Registered
    // disabled so the test never reaches for a network.
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "remote", "type": "http",
                "url": "https://mcp.context7.com/mcp",
                "headers": { "Authorization": "Bearer sk-live-1" },
                "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        h.def_of("remote")["url"],
        json!("https://mcp.context7.com/mcp")
    );
    assert_eq!(
        h.def_of("remote")["headers"]["Authorization"],
        json!("Bearer sk-live-1")
    );

    // The key travels in a header, so /details has to mask it there — the panel edits this
    // definition and must never receive the credential.
    let (_, details) = h.get("/api/mcps/remote/details").await;
    assert_eq!(details["config"]["headers"]["Authorization"], json!(MASK));
    assert_eq!(
        details["config"]["url"],
        json!("https://mcp.context7.com/mcp")
    );
}

#[tokio::test]
async fn adds_a_declared_rest_mcp_and_keeps_its_tool_declarations() {
    // A rest MCP's `tools` is an array of declarations, so build_def has to keep it whole —
    // without that branch, opening a config-defined rest MCP in the panel and pressing save
    // answers "unknown type".
    let h = setup();
    let tools = json!([{
        "name": "get_repo",
        "description": "Get a public GitHub repo.",
        "input": { "owner": { "type": "string", "required": true },
                   "repo": { "type": "string", "required": true } },
        "request": { "method": "GET", "path": "/repos/{{owner}}/{{repo}}" },
        "pick": ["full_name", "description", "stargazers_count"],
    }]);
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "github-rest", "type": "rest",
                "baseUrl": "https://api.github.com",
                "headers": { "Authorization": "Bearer sk-live-2" },
                "tools": tools, "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.def_of("github-rest")["tools"], tools);
    assert_eq!(
        h.def_of("github-rest")["baseUrl"],
        json!("https://api.github.com")
    );

    // Same rule as every other adapter: the key does not go to the browser.
    let (_, details) = h.get("/api/mcps/github-rest/details").await;
    assert_eq!(details["config"]["headers"]["Authorization"], json!(MASK));
}

#[tokio::test]
async fn rejects_a_rest_mcp_with_no_tools_and_an_http_mcp_with_no_url() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "notools", "type": "rest", "baseUrl": "https://x.test", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("tools"),
        "{body}"
    );

    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "nourl", "type": "http", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("url is required"),
        "{body}"
    );
}

// --- secret masking --------------------------------------------------------------------------

#[tokio::test]
async fn masks_secrets_in_details_and_keeps_the_stored_value_when_the_mask_returns() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "db", "type": "mysql", "host": "127.0.0.1",
                "user": "app", "password": "hunter2", "database": "d", "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, details) = h.get("/api/mcps/db/details").await;
    assert_eq!(details["config"]["password"], json!(MASK));
    assert_eq!(details["config"]["user"], json!("app")); // non-secret fields are untouched

    // Saving the form unchanged must not overwrite the password with the mask.
    let (status, _) = h
        .put(
            "/api/mcps/db",
            json!({
                "type": "mysql", "host": "127.0.0.1", "user": "app",
                "password": MASK, "database": "d",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.def_of("db")["password"], json!("hunter2"));

    // A real edit still goes through.
    h.put(
        "/api/mcps/db",
        json!({
            "type": "mysql", "host": "127.0.0.1", "user": "app",
            "password": "newpass", "database": "d",
        }),
    )
    .await;
    assert_eq!(h.def_of("db")["password"], json!("newpass"));
}

#[tokio::test]
async fn never_stores_the_mask_sentinel_when_the_type_changes_under_an_edit() {
    let h = setup();
    h.post(
        "/api/mcps",
        json!({
            "name": "db", "type": "mysql", "host": "127.0.0.1",
            "user": "app", "password": "hunter2", "database": "d", "enabled": false,
        }),
    )
    .await;
    // The panel's type dropdown can submit a pg url against a def that has no url to restore
    // from. Writing the sentinel through would replace a credential with dots and report success.
    let (status, _) = h
        .put(
            "/api/mcps/db",
            json!({ "type": "pg", "url": format!("postgresql://u:{MASK}@127.0.0.1:5432/d") }),
        )
        .await;
    assert!(
        !h.def_of("db").to_string().contains('•'),
        "{}",
        h.def_of("db")
    );
    // No usable url survived, so the edit is refused out loud.
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn masks_only_the_password_inside_a_connection_url_and_restores_it_on_save() {
    let h = setup();
    h.post(
        "/api/mcps",
        json!({
            "name": "pgx", "type": "pg",
            "url": "postgresql://chatuser:s3cret@127.0.0.1:5432/chat", "enabled": false,
        }),
    )
    .await;
    let (_, details) = h.get("/api/mcps/pgx/details").await;
    assert_eq!(
        details["config"]["url"],
        json!(format!("postgresql://chatuser:{MASK}@127.0.0.1:5432/chat"))
    );

    h.put(
        "/api/mcps/pgx",
        json!({ "type": "pg", "url": format!("postgresql://chatuser:{MASK}@127.0.0.1:5432/other") }),
    )
    .await;
    // The host and database of the edit are kept; only the password comes back from the store.
    assert_eq!(
        h.def_of("pgx")["url"],
        json!("postgresql://chatuser:s3cret@127.0.0.1:5432/other")
    );
}

// --- memory ----------------------------------------------------------------------------------

#[tokio::test]
async fn reports_gateway_memory_without_walking_a_process_tree_when_nothing_was_spawned() {
    let h = setup();
    let (status, body) = h.get("/api/memory").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["gatewayMb"].as_f64().unwrap_or(0.0) > 0.0, "{body}");
    assert_eq!(body["childrenMb"].as_f64(), Some(0.0));
    assert_eq!(body["childrenPending"], json!(false));
    assert_eq!(body["processCount"], json!(1));
}

// --- prompts ---------------------------------------------------------------------------------

#[tokio::test]
async fn lists_prompts_paged_even_for_a_server_that_publishes_none() {
    // The panel's Prompts tab polls every started MCP. A server without the capability must answer
    // an empty page, not an error the tab would render as a broken MCP.
    let h = setup();
    h.register_fixture("p1");
    h.registry.start("p1").await.expect("start");
    let (status, body) = h.get("/api/mcps/p1/prompts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["prompts"], json!([]));
}

// --- sidebar groups --------------------------------------------------------------------------

/// Three MCPs, none of them assigned to a group.
fn with_mcps() -> Harness {
    let h = setup();
    for name in ["context7", "deepwiki", "github"] {
        h.register(name, json!({ "type": "echo" }));
    }
    h
}

/// Every row's group, in the order the panel would draw them.
async fn groups_of(h: &Harness) -> Vec<String> {
    let (_, body) = h.get("/api/mcps").await;
    field_of(&body["mcps"], "group")
}

#[tokio::test]
async fn starts_with_the_default_group_and_every_mcp_in_it() {
    let h = with_mcps();
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    // The list is never empty: `default` is materialized, ordinary, first.
    assert_eq!(body["groups"], json!(["default"]));
    assert_eq!(groups_of(&h).await, ["default", "default", "default"]);
}

#[tokio::test]
async fn creates_groups_and_reports_them_on_the_list() {
    let h = with_mcps();
    let (status, put) = h
        .put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["Docs", "Search"]));

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Docs", "Search"]));
}

/// The panel's "Move up" sends the whole list back with the moved group first (groups.js
/// moveGroupBy -> saveGroupNames). A reorder is order-only: members that rendered in the
/// demoted first group by DEFAULT - no explicit entry, the sink slot was their only home -
/// must not follow the slot to the new front. Regression shape: g1 held three, g2 held
/// none, one Move up on g2 and all three answered g2.
#[tokio::test]
async fn reordering_groups_never_rehomes_the_first_groups_default_members() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["g1", "g2"] }))
        .await;
    assert_eq!(groups_of(&h).await, ["g1", "g1", "g1"]);

    let (status, put) = h
        .put("/api/groups/mcps", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(
        list["groups"],
        json!(["g2", "g1"]),
        "the order itself changed"
    );
    assert_eq!(
        groups_of(&h).await,
        ["g1", "g1", "g1"],
        "but nobody re-homed"
    );
}

/// The same Move up shape through the tokens scope: this store owns its member set, so the
/// pinning needs no outside help - a token that rendered in the demoted first group by
/// default stays with the name.
#[tokio::test]
async fn reordering_token_groups_never_rehomes_the_first_groups_default_members() {
    let h = setup();
    let id = h
        .post("/api/tokens", json!({ "label": "reorder-probe" }))
        .await
        .1["id"]
        .as_str()
        .expect("the id")
        .to_string();
    h.put("/api/groups/tokens", json!({ "groups": ["g1", "g2"] }))
        .await;

    let (status, put) = h
        .put("/api/groups/tokens", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/tokens").await;
    assert_eq!(list["tokenGroups"][&id], json!("g1"), "nobody re-homed");
}

#[tokio::test]
async fn rejects_a_malformed_duplicate_empty_or_last_group_destroying_list() {
    let h = with_mcps();
    let (status, _) = h.put("/api/groups/mcps", json!({ "groups": "Docs" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for groups in [json!(["Docs", "docs"]), json!([" "]), json!([])] {
        let (status, _) = h.put("/api/groups/mcps", json!({ "groups": groups })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{groups}");
    }
    // `default` is an ordinary name now — a list of just it is fine.
    let (status, _) = h
        .put("/api/groups/mcps", json!({ "groups": ["default"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    // A rejected call must not have half-applied.
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["default"]));
}

#[tokio::test]
async fn assigns_a_config_sourced_mcp_to_a_group_and_back_to_default() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;

    let (status, put) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "Docs" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        put,
        json!({ "group": "Docs" }),
        "the family answers the canonical group"
    );

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("Docs"));
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("default"));

    let (status, _) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("default"));
}

#[tokio::test]
async fn survives_a_restart_because_the_assignment_is_on_disk_not_in_the_registry() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs"] }))
        .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "Docs" }),
    )
    .await;

    let reloaded = ManagedStore::open_at(h.path.clone());
    assert_eq!(reloaded.get_groups(), ["Docs"]);
    assert_eq!(reloaded.group_of("context7"), "Docs");
}

#[tokio::test]
async fn answers_404_for_an_unknown_mcp_and_400_for_an_unknown_group() {
    let h = with_mcps();
    assert_eq!(
        h.put("/api/groups/mcps/members/ghost", json!({ "group": null }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "Nope" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn renames_a_group_and_carries_its_members() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    for name in ["context7", "deepwiki"] {
        h.put(
            &format!("/api/groups/mcps/members/{name}"),
            json!({ "group": "Docs" }),
        )
        .await;
    }

    let (status, put) = h
        .post(
            "/api/groups/mcps/rename",
            json!({ "from": "Docs", "to": "Reference" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["Reference", "Search"]));
    assert_eq!(put["moved"], json!(2)); // context7 and deepwiki rode along explicitly

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Reference", "Search"])); // keeps its slot
    assert_eq!(row_named(&list, "context7")["group"], json!("Reference"));
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("Reference"));
}

#[tokio::test]
async fn refuses_a_rename_onto_an_existing_group_or_a_missing_one() {
    let h = with_mcps();
    h.put(
        "/api/groups/mcps",
        json!({ "groups": ["default", "Docs", "Search"] }),
    )
    .await;

    for (from, to, expected) in [
        ("Docs", "Search", StatusCode::BAD_REQUEST),
        ("Ghost", "X", StatusCode::NOT_FOUND),
    ] {
        let (status, _) = h
            .post("/api/groups/mcps/rename", json!({ "from": from, "to": to }))
            .await;
        assert_eq!(status, expected, "renaming {from} to {to}");
    }
}

#[tokio::test]
async fn renames_default_like_any_other_group_carrying_the_first_slot_with_it() {
    let h = with_mcps();
    h.put(
        "/api/groups/mcps",
        json!({ "groups": ["default", "Docs", "Search"] }),
    )
    .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "default" }),
    )
    .await;

    let (status, _) = h
        .post(
            "/api/groups/mcps/rename",
            json!({ "from": "default", "to": "主力" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["主力", "Docs", "Search"]));
    assert_eq!(row_named(&list, "context7")["group"], json!("主力"));
    // An unassigned MCP followed the renamed slot on its own.
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("主力"));
}

#[tokio::test]
async fn deleting_a_group_by_omission_moves_its_mcps_to_the_first_remaining_group() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "Docs" }),
    )
    .await;
    h.put(
        "/api/groups/mcps/members/github",
        json!({ "group": "Search" }),
    )
    .await;

    h.put("/api/groups/mcps", json!({ "groups": ["Search"] }))
        .await;

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Search"]));
    assert_eq!(list["mcps"].as_array().map(Vec::len), Some(3)); // nothing was deleted
                                                                // Members of the dropped group land in the FIRST remaining group — Search, the only one.
    assert_eq!(row_named(&list, "context7")["group"], json!("Search"));
    assert_eq!(row_named(&list, "github")["group"], json!("Search"));
}

#[tokio::test]
async fn carries_an_mcps_group_through_a_rename_and_drops_it_on_delete() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({ "name": "a", "type": "echo", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h.put("/api/groups/mcps", json!({ "groups": ["Docs"] }))
        .await;
    h.put("/api/groups/mcps/members/a", json!({ "group": "Docs" }))
        .await;

    h.post("/api/mcps/a/rename", json!({ "name": "b" })).await;
    assert_eq!(h.store.group_of("b"), "Docs");

    h.delete("/api/mcps/b").await;
    assert!(h.store.get_mcp_groups().is_empty());
}

// --- the group-scope route family (docs/20 §3) ---------------------------------------------------
//
// One family over every scope; mcps is the one this harness wires, and its behavior pins the
// shape every other scope answers with.

#[tokio::test]
async fn the_family_replaces_the_whole_group_list_per_scope() {
    let h = with_mcps();
    let (status, body) = h
        .put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Docs"]));
    // The old shape and the new reach the same store — the family is the panel's one door.
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["default", "Docs"]));
    // Malformed bodies, duplicates and the last-group deletion are 400s.
    for groups in [json!("Docs"), json!(["a", "a"]), json!([])] {
        let (status, _) = h.put("/api/groups/mcps", json!({ "groups": groups })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{groups}");
    }
}

#[tokio::test]
async fn the_family_assigns_a_member_and_answers_the_canonical_group() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;

    let (status, body) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "docs" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "Docs" })); // canonical casing, not what was typed

    let (status, _) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("default"));

    // 404 for a member nothing serves; 400 for a group nothing holds.
    assert_eq!(
        h.put("/api/groups/mcps/members/ghost", json!({ "group": null }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "Nope" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.put("/api/groups/mcps/members/context7", json!({ "group": 3 }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_family_persists_the_scope_order() {
    let h = with_mcps();
    let (status, body) = h
        .put(
            "/api/groups/mcps/order",
            json!({ "order": ["deepwiki", "context7", "github"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!(["deepwiki", "context7", "github"]));
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(
        field_of(&list["mcps"], "name"),
        ["deepwiki", "context7", "github"]
    );
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": "context7" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": [1, 2] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_scope_is_a_named_404_on_every_family_route() {
    let h = with_mcps();
    for (method, path, body) in [
        ("PUT", "/api/groups/nope", json!({ "groups": ["default"] })),
        (
            "POST",
            "/api/groups/nope/rename",
            json!({ "from": "a", "to": "b" }),
        ),
        (
            "PUT",
            "/api/groups/nope/members/x",
            json!({ "group": null }),
        ),
        ("PUT", "/api/groups/nope/order", json!({ "order": [] })),
    ] {
        let (status, resp) = match method {
            "PUT" => h.put(path, body).await,
            _ => h.post(path, body).await,
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(
            resp["error"],
            json!("unknown scope: nope"),
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn the_family_rename_body_is_from_to_and_missing_fields_are_400() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "to": "X" })) // no from
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "from": "Docs" })) // no to
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // The old body shape ({name}) is gone with the old route: it no longer parses as a rename.
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "name": "X" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// setup(), plus the two tunnel scopes registered over a scratch tunnels.json — the way
/// server.rs composes them after building the store. What comes back is the store handle,
/// for asserting what actually landed on disk.
fn setup_with_tunnels() -> (
    Harness,
    Arc<std::sync::Mutex<swiss_tunnels::tunnel::TunnelStore>>,
) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-tunnels-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let tun = Arc::new(std::sync::Mutex::new(
        swiss_tunnels::tunnel::TunnelStore::new(dir.join("tunnels.json"), 19999),
    ));
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        "MCP_GATEWAY_TOKEN",
        19999,
    );
    swiss_tunnels::tunnel::register_tunnel_scopes(&ctx.group_scopes, &tun);
    (
        Harness {
            app: build_app(ctx, None),
            registry,
            store,
            calls,
            path: dir.join("managed.json"),
        },
        tun,
    )
}

/// Seed one connection + two rules; the ids are what the family's member routes take.
fn seed_tunnels(
    tun: &Arc<std::sync::Mutex<swiss_tunnels::tunnel::TunnelStore>>,
) -> (String, String, String) {
    let mut s = tun.lock().unwrap();
    let c = s
        .add_connection(&swiss_tunnels::tunnel::store::ConnInput {
            name: "bastion".into(),
            host: "127.0.0.1".into(),
            port: 22.0,
            username: "u".into(),
            auth_type: swiss_tunnels::tunnel::types::AuthType::Password,
            password: Some("p".into()),
            ..Default::default()
        })
        .expect("a connection");
    let r1 = s
        .add_rule(&swiss_tunnels::tunnel::store::RuleInput {
            name: "pg".into(),
            connection_id: c.id.clone(),
            local_port: 5433.0,
            target_host: "127.0.0.1".into(),
            target_port: 5432.0,
            ..Default::default()
        })
        .expect("a rule");
    let r2 = s
        .add_rule(&swiss_tunnels::tunnel::store::RuleInput {
            name: "redis".into(),
            connection_id: c.id.clone(),
            local_port: 6379.0,
            target_host: "127.0.0.1".into(),
            target_port: 6379.0,
            ..Default::default()
        })
        .expect("a rule");
    (c.id, r1.id, r2.id)
}

#[tokio::test]
async fn the_family_serves_the_tunnel_scopes() {
    let (h, tun) = setup_with_tunnels();
    let (c, r1, r2) = seed_tunnels(&tun);

    // The whole-list mutation, on the scope word the family owns (conns, not connections).
    let (status, body) = h
        .put(
            "/api/groups/conns",
            json!({ "groups": ["default", "prod"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "prod"]));

    // Assignment is by tunnel id, answers the canonical casing, and lands in tunnels.json.
    let (status, body) = h
        .put(
            &format!("/api/groups/conns/members/{c}"),
            json!({ "group": "PROD" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "prod" }));
    assert_eq!(
        tun.lock().unwrap().connection(&c).unwrap().group,
        Some("prod".to_string())
    );

    // Rename keeps the slot and counts the members that rode along explicitly.
    h.put(
        "/api/groups/rules",
        json!({ "groups": ["default", "Learning"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/rules/members/{r2}"),
        json!({ "group": "default" }),
    )
    .await;
    let (status, body) = h
        .post(
            "/api/groups/rules/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["Basics", "Learning"]));
    assert_eq!(body["moved"], 1, "r2 was an explicit member of default");
    assert_eq!(
        tun.lock().unwrap().rule(&r2).unwrap().group,
        Some("Basics".to_string())
    );
    // An implicit member needs no rewrite: the sink slot kept its place, so it follows.
    assert_eq!(tun.lock().unwrap().rule(&r1).unwrap().group, None);

    // The scope's flat order: one list per call now, not both at once.
    let (status, body) = h
        .put(
            "/api/groups/rules/order",
            json!({ "order": [r2.clone(), r1.clone()] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!([r2, r1]));
    assert_eq!(tun.lock().unwrap().rules()[0].name, "redis");

    // Unknown member is the family's 404, with the family's wording.
    let (status, body) = h
        .put("/api/groups/rules/members/nope", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], json!("unknown member: nope"));
}

/// setup(), plus the targets scope registered over a scratch sealed table - the way
/// server.rs composes it over the remote plugin's one system (docs/34 R8). What comes
/// back is the system handle, for asserting what actually landed on disk.
fn setup_with_remote() -> (Harness, Arc<swiss_remote::RemoteSystem>) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-remote-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let system = swiss_remote::RemoteSystem::open(
        swiss_host::services::RuntimeServices::new(),
        dir.join("remote.json"),
    );
    system
        .with_store(|s| {
            s.add(swiss_remote::target::RemoteTarget {
                id: "build".into(),
                label: "Build box".into(),
                endpoint: "conn-1".into(),
                workspace_root: "/data/ws".into(),
                shell: "posix".into(),
                capabilities: vec!["exec".into()],
                default_timeout_ms: None,
                group: None,
            })
            .map(|_| ())
        })
        .expect("seed");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        "MCP_GATEWAY_TOKEN",
        19999,
    );
    swiss_remote::register_remote_scopes(&ctx.group_scopes, &system);
    (
        Harness {
            app: build_app(ctx, None),
            registry,
            store,
            calls,
            path: dir,
        },
        system,
    )
}

/// The seventh scope (docs/34 R8): targets answers the same family routes the tunnel
/// scopes do, over the one sealed table, while the remote plugin itself is not even
/// registered here - grouping outlives start/stop by construction.
#[tokio::test]
async fn the_family_serves_the_targets_scope() {
    let (h, system) = setup_with_remote();

    // The whole-list mutation, on the scope word the family owns.
    let (status, body) = h
        .put("/api/groups/targets", json!({ "groups": ["default", "prod"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "prod"]));

    // Assignment is by target id, answers the canonical casing, and lands in the file.
    let (status, body) = h
        .put("/api/groups/targets/members/build", json!({ "group": "PROD" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "prod" }));
    assert_eq!(
        system.with_store(|s| s.group_of("build")),
        "prod",
        "the sealed table answers the same word the family answered"
    );

    // Rename counts the explicit members that rode along.
    let (status, body) = h
        .post(
            "/api/groups/targets/rename",
            json!({ "from": "prod", "to": "Production" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Production"]));
    assert_eq!(body["moved"], 1);

    // The scope's flat order is array order in the file.
    let (status, body) = h
        .put(
            "/api/groups/targets/order",
            json!({ "order": ["build"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!(["build"]));

    // Unknown member is the family's 404, with the family's wording.
    let (status, body) = h
        .put("/api/groups/targets/members/ghost", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], json!("unknown member: ghost"));
}

#[tokio::test]
async fn the_retired_mcp_group_and_order_routes_are_gone() {
    let (h, _) = setup_with_tunnels();
    // docs/20 §3: one family, one door. The per-mcp group route, the bare /api/groups and the
    // unscoped /api/order all answered the same store the family does - keeping them is how
    // two copies of one protocol drift apart.
    for (method, path, body) in [
        ("PUT", "/api/order", json!({ "order": [] })),
        ("PUT", "/api/groups", json!({ "groups": ["default"] })),
        (
            "PUT",
            "/api/groups/mcps/members/ghost",
            json!({ "group": null }),
        ),
    ] {
        let (status, _) = match method {
            "PUT" => h.put(path, body).await,
            _ => h.post(path, body).await,
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

// --- request parsing -------------------------------------------------------------------------

#[tokio::test]
async fn parses_a_json_body_sent_with_an_uppercase_content_type() {
    // Media types are case-insensitive per RFC 7231; a case-sensitive check dropped the body, so
    // the handler saw {} and rejected a perfectly valid request with a confusing message.
    let h = setup();
    let (status, _) = h
        .send(
            Request::post("/api/mcps")
                .header(header::CONTENT_TYPE, "Application/JSON")
                .body(Body::from(
                    json!({ "name": "ct", "type": "echo", "enabled": false }).to_string(),
                ))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

// --- import ----------------------------------------------------------------------------------

#[tokio::test]
async fn imports_stdio_and_http_entries_suffixes_collisions_and_skips_this_gateways_urls() {
    let h = setup();
    h.register("redis", json!({ "type": "echo" }));

    let (status, body) = h
        .post(
            "/api/mcpdefs/import",
            json!({
                "mcpServers": {
                    "redis": { "type": "http", "url": "https://example.invalid/a" },
                    "docs": {
                        "type": "http", "url": "https://example.invalid/b",
                        "headers": { "Authorization": "Bearer ${K}" },
                    },
                    "already": { "url": "http://127.0.0.1:19999/mysql" },
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut imported = field_of(&body["imported"], "name");
    imported.sort();
    assert_eq!(imported, ["docs", "redis-1"]);
    assert!(
        field_of(&body["skipped"], "name").contains(&"already".to_string()),
        "{body}"
    );
    assert!(h.registry.has("redis-1"));
    assert!(h.registry.has("docs"));
}

#[tokio::test]
async fn imports_a_name_the_root_once_reserved_without_renaming() {
    // docs/24 P2: the RESERVED rename-on-import rule retired with the root-level route — an
    // imported "health" keeps its name under the MCP plugin's /mcp/ domain.
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcpdefs/import",
            json!({
                "mcpServers": {
                    "health": { "type": "http", "url": "https://example.invalid/h" },
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // "from" is the wanted name as it appeared in the file; it must equal the landed name.
    assert_eq!(field_of(&body["imported"], "from"), ["health"]);
    assert_eq!(field_of(&body["imported"], "name"), ["health"]);
    assert!(h.registry.has("health"));
}

#[tokio::test]
async fn imports_without_credentials_like_every_other_api_route() {
    let h = setup();
    let (status, _) = h
        .post("/api/mcpdefs/import", json!({ "mcpServers": {} }))
        .await;
    assert_eq!(status, StatusCode::OK);
}

// --- deleting a CONFIG-sourced MCP -------------------------------------------------------------

#[tokio::test]
async fn removes_the_entry_from_gateway_config_json_and_the_runtime_registry() {
    // The delete must remove the server from gateway.config.json as well, or the next gateway
    // start resurrects it — which is why the panel used to hide Delete for config MCPs. The file
    // edit is surgical: only the named key goes, every other entry (and its ${ENV} refs) survives.
    //
    // Unlike the Node suite, the data dir is not swapped per test — one sandbox home serves the
    // whole binary — so this is the only test that touches gateway.config.json.
    let h = setup();
    let config = swiss_host::config::config_path();
    swiss_core::secure::statefile::write_secure_json(
        &config,
        &json!({
            "port": 19999,
            "tokenEnv": "MCP_GATEWAY_TOKEN",
            "servers": {
                "doomed": { "type": "echo" },
                "keeper": { "type": "echo", "password": "${KEEPER_PASS}" },
            },
        }),
    )
    .expect("seal a config file");
    h.register("doomed", json!({ "type": "echo" }));
    h.register("keeper", json!({ "type": "echo" }));

    let (status, body) = h.delete("/api/mcps/doomed").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("doomed"));
    assert_eq!(body["deleted"], json!(true));
    assert!(!h.registry.has("doomed"));

    let after = swiss_core::secure::statefile::read_secure_json(&config)
        .expect("the config is readable")
        .expect("the config is there");
    assert!(after["servers"].get("doomed").is_none(), "{after}");
    // Untouched, reference intact.
    assert_eq!(
        after["servers"]["keeper"],
        json!({ "type": "echo", "password": "${KEEPER_PASS}" })
    );
    let _ = std::fs::remove_file(&config);
}

// --- the secret vault (docs/19) ------------------------------------------------------------------
// The real router, the real sealed file under the sandbox home: names only on GET, write-only
// PUT, rev-checked mutations, and the value observable through no response. One tokio mutex
// serializes these five — the vault is process-global state, and two tests planning mutations
// against the same rev would race each other into honest 409s.
static VAULT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn the_vault_lists_names_never_values() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-list",
        json!({ "value": "sk_live_never_in_a_response", "rev": rev }),
    )
    .await;

    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK);
    let names = body["secrets"].as_array().expect("a names array");
    assert!(
        names.iter().any(|n| n == "panel-list"),
        "the name is listed: {names:?}"
    );
    assert!(
        !body.to_string().contains("sk_live_never_in_a_response"),
        "GET never carries a value"
    );
}

#[tokio::test]
async fn the_vault_stores_overwrites_and_deletes_by_rev() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");

    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "first", "rev": rev }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "store: {body}");
    let rev2 = body["rev"].as_u64().expect("the bumped rev");

    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "second", "rev": rev2 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "overwrite: {body}");

    // The stale rev the panel was planning on is a 409 carrying both numbers.
    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "third", "rev": rev }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale rev: {body}");
    assert_eq!(body["have"], rev2 + 1);
    assert_eq!(body["saw"], rev);

    let resolved =
        swiss_core::secure::refs::resolve("${secret://panel-roundtrip}").expect("stored");
    assert_eq!(resolved, "second", "the overwrite won");

    let (status, _) = h.delete("/api/secrets/panel-roundtrip").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a rev-less delete is refused"
    );

    let rev_now = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, _) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_now}"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        swiss_core::secure::secretstore::vault_lookup("panel-roundtrip").is_none(),
        "gone from the vault"
    );

    // Deleting again with the OLD rev answers the rev mismatch (the rev gate comes before
    // the existence check — a stale caller learns its rev is stale, not what is stored).
    let (status, body) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_now}"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale rev delete: {body}");

    // With the fresh rev, the absent name is an honest 404.
    let rev_after = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, _) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_after}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already gone, honestly");
}

#[tokio::test]
async fn the_vault_refuses_names_outside_the_grammar() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    for bad in ["BadName", "under_score", "1digit", "-dash"] {
        let (status, body) = h
            .put(
                &format!("/api/secrets/{bad}"),
                json!({ "value": "v", "rev": 0 }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("lowercase kebab"),
            "{bad} quotes the grammar: {msg}"
        );
    }
}

#[tokio::test]
async fn an_mcp_builder_refuses_a_missing_vault_reference() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    // The strict contract's first wiring (docs/19 D4): make_adapter resolves vault refs and
    // REFUSES the build when one names a secret this machine does not hold — the error names
    // the MCP, the JSON path, and the reference, never a value.
    let d = def(json!({
        "type": "http",
        "url": "https://example.test/mcp",
        "headers": { "Authorization": "Bearer ${secret://context7}" }
    }));
    let err = match make_adapter(&d, "context7", &h.calls) {
        Err(e) => e,
        Ok(_) => panic!("a missing vault reference must refuse the build"),
    };
    assert!(
        err.contains("context7")
            && err.contains("headers.Authorization")
            && err.contains("secret://context7")
            && err.contains("not in the vault"),
        "names the surface and the reference: {err}"
    );

    // With the secret stored, the same definition builds and the header carries the value.
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/context7",
        json!({ "value": "tok-context7", "rev": rev }),
    )
    .await;
    let adapter = make_adapter(&d, "context7", &h.calls).expect("builds once stored");
    assert_eq!(adapter.kind().to_string(), "http");
}

#[tokio::test]
async fn the_rest_connection_test_fails_honestly_on_a_missing_vault_reference() {
    // The rest branch of /api/mcpdefs/test resolves the WHOLE def before its one plain GET
    // (docs/19 D4): a missing vault reference answers ok:false naming the JSON path — no
    // request is made, no reference text ships anywhere.
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcpdefs/test",
            json!({
                "type": "rest",
                "baseUrl": "https://example.test/api",
                "tools": [{ "name": "ping", "template": { "method": "GET", "path": "/x" } }],
                "headers": { "Authorization": "Bearer ${secret://rest-test-gone}" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the test endpoint answers 200");
    assert_eq!(body["ok"], false, "and reports the failure: {body}");
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("headers.Authorization") && err.contains("secret://rest-test-gone"),
        "names the surface and the reference: {err}"
    );
}

#[tokio::test]
async fn a_vault_reference_masks_like_an_env_ref() {
    // is_env_ref is what the whole masking stack keys off; a vault reference must read as a
    // reference so the panel edit forms show it as authored, never hold the value. Both the
    // envelope form (docs/25 E1) and the legacy whole-value bare form (still on disks the
    // loaders have not re-saved, docs/25 E2) pass; anything mixed or malformed must not.
    use swiss_host::config::is_env_ref;
    assert!(is_env_ref(&json!("secret://stripe-key")));
    assert!(is_env_ref(&json!("${secret://stripe-key}")));
    assert!(is_env_ref(&json!("${STRIPE_KEY}")));
    assert!(!is_env_ref(&json!("secret://")));
    assert!(!is_env_ref(&json!("secret://BadName")));
    assert!(!is_env_ref(&json!("${secret://}")));
    assert!(!is_env_ref(&json!("${secret://BadName}")));
    assert!(!is_env_ref(&json!("Bearer ${secret://stripe-key}")));
    assert!(!is_env_ref(&json!("plain text")));
}
// --- the secrets scope over the family (docs/20 G6) ----------------------------------------------

#[tokio::test]
async fn the_family_serves_the_secrets_scope() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();

    // Two secrets to move around.
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-a",
        json!({ "value": "v-one", "rev": rev }),
    )
    .await;
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-b",
        json!({ "value": "v-two", "rev": rev }),
    )
    .await;

    // The whole list, then a member assign that keeps the canonical casing.
    let (status, body) = h
        .put(
            "/api/groups/secrets",
            json!({ "groups": ["default", "Ops"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/panel-g6-a",
            json!({ "group": "ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));

    // The listing answers the labels and NEVER a value: the write-only rule (docs/19 D5)
    // is about values, and a group label is a folder name.
    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    assert_eq!(body["secretGroups"]["panel-g6-a"], json!("Ops"));
    assert_eq!(
        body["secretGroups"]["panel-g6-b"],
        json!("default"),
        "unassigned sinks"
    );
    let text = body.to_string();
    assert!(!text.contains("v-one"), "no value ever crosses: {text}");

    // A rename carries the members and answers how many moved. The vault is one shared
    // table across this binary's tests, so the honest expectation is the count the LISTING
    // reports under "default" right before the rename - unassigned secrets render there
    // (the sink), and the slot carries them with it.
    let before = h.get("/api/secrets").await.1;
    let in_default = before["secretGroups"]
        .as_object()
        .map(|m| m.values().filter(|g| g.as_str() == Some("default")).count())
        .unwrap_or(0);
    assert!(
        in_default >= 1,
        "panel-g6-b at least renders under default: {before}"
    );
    let (status, body) = h
        .post(
            "/api/groups/secrets/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["Basics", "Ops"]));
    assert_eq!(
        body["moved"],
        json!(in_default),
        "the whole first slot moved"
    );

    // docs/26: the order is the model's third list. Unknown names drop out, so a stale
    // panel cannot plant a ghost row; the landed order comes back.
    let (status, body) = h
        .put(
            "/api/groups/secrets/order",
            json!({ "order": ["panel-g6-a", "ghost", "panel-g6-a"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["order"], json!(["panel-g6-a"]));

    // The listing carries the stored order raw (docs/26 E2).
    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["order"], json!(["panel-g6-a"]));

    // Unknown member: the family's one 404.
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/ghost",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_secrets_regroup_is_one_rev_bump_and_values_survive() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev0 = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-rev",
        json!({ "value": "keep-me", "rev": rev0 }),
    )
    .await;
    h.put(
        "/api/groups/secrets",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    let before = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/panel-g6-rev",
            json!({ "group": "Ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    assert_eq!(
        after,
        before + 1,
        "a regroup is ONE rev bump, like a value write"
    );
    // The value the regroup rode along with is intact - the file still seals it.
    assert_eq!(
        swiss_core::secure::secretstore::vault_lookup("panel-g6-rev").as_deref(),
        Some("keep-me"),
        "the group move never touches values"
    );
}
// --- the tokens scope over the family (docs/20 G7) ----------------------------------------------

#[tokio::test]
async fn the_family_serves_the_tokens_scope() {
    let h = setup();
    let (status, body) = h.post("/api/tokens", json!({ "label": "g7-probe" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("the id").to_string();

    let (status, body) = h
        .put(
            "/api/groups/tokens",
            json!({ "groups": ["default", "Lab"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Lab"]));

    let (status, body) = h
        .put(
            &format!("/api/groups/tokens/members/{id}"),
            json!({ "group": "lab" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Lab"), "canonical casing comes back");

    // The listing answers the two lists beside the tokens (docs/20 §2.3): a group is a
    // folder a token sits in; it says nothing about whether the token is in use.
    let (status, body) = h.get("/api/tokens").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Lab"]));
    assert_eq!(body["tokenGroups"][&id], json!("Lab"));

    let (status, body) = h
        .post(
            "/api/groups/tokens/rename",
            json!({ "from": "Lab", "to": "Ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    assert_eq!(body["moved"], json!(1), "the one assigned member moved");

    // Creation time is the order (docs/20 §2.1): like tokens, no manual order exists
    // (secrets gained one - docs/26).
    let (status, body) = h
        .put("/api/groups/tokens/order", json!({ "order": [id] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("tokens have no manual order"));

    let (status, _) = h
        .put("/api/groups/tokens/members/nope", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown member");
}

#[tokio::test]
async fn token_lifecycle_never_touches_the_groups() {
    let h = setup();
    let id = h.post("/api/tokens", json!({ "label": "g7-life" })).await.1["id"]
        .as_str()
        .expect("the id")
        .to_string();
    h.put(
        "/api/groups/tokens",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/tokens/members/{id}"),
        json!({ "group": "Ops" }),
    )
    .await;

    // A rotate keeps the id, so the assignment rides along untouched.
    let (status, body) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = h.get("/api/tokens").await.1;
    assert_eq!(
        body["tokenGroups"][&id],
        json!("Ops"),
        "a rotate keeps the group"
    );

    // Deleting the group sinks its tokens into the first one - the token itself is
    // untouched, still listed, still authenticating (docs/20 G7).
    let (status, body) = h
        .put("/api/groups/tokens", json!({ "groups": ["default"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = h.get("/api/tokens").await.1;
    assert_eq!(
        body["groups"],
        json!(["default"]),
        "the whole list, Ops omitted"
    );
    assert!(
        body["tokenGroups"].get(&id).is_none(),
        "the sunk token carries no explicit entry: {body}"
    );
    assert!(
        body["tokens"]
            .as_array()
            .map(|a| a.iter().any(|t| t["id"] == json!(id)))
            .unwrap_or(false),
        "the token still exists"
    );

    // A revoke forgets the member: no ghost entry outlives its token.
    h.put(
        "/api/groups/tokens",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/tokens/members/{id}"),
        json!({ "group": "Ops" }),
    )
    .await;
    let (status, _) = h.delete(&format!("/api/tokens/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    let body = h.get("/api/tokens").await.1;
    assert!(
        body["tokenGroups"].get(&id).is_none(),
        "no ghost member: {body}"
    );
}

// --- the jobs scope over the family (docs/20 G4) --------------------------------------------------
/// The jobs-scope harness: a real JobSystem over a real config store, its row seeded with
/// two manual jobs, registered into the family the way server.rs composes it. What comes
/// back is the system handle, for asserting the applied table directly.
fn setup_with_jobs() -> (Harness, Arc<swiss_jobs::jobs::JobSystem>) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-jobs-{}",
        swiss_core::util::random_hex(8),
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let config_store = swiss_host::config_store::ConfigStore::from_loaded(
        dir.join("gateway.config.json"),
        json!({}),
    );
    let services = swiss_host::services::RuntimeServices::new();
    // The one capability a definition can lean on in a test: the built-in legacy command
    // action, registered the way the jobs plugin's own tests do it.
    services
        .actions
        .register(Arc::new(
            swiss_host::services::actions::LegacyCommandAction::new(services.supervisor.clone()),
        ))
        .expect("the legacy command capability registers once");
    let jobs =
        swiss_jobs::jobs::JobSystem::open(dir.join("jobs.json"), services, config_store.clone());
    config_store
        .update_plugin(
            "jobs",
            config_store.snapshot().revision,
            json!({
                "definitions": {
                    "vacuum": {
                        "trigger": { "kind": "manual" },
                        "action": { "type": "process.legacy-command", "input": { "command": "echo v" } },
                    },
                    "report": {
                        "trigger": { "kind": "manual" },
                        "action": { "type": "process.legacy-command", "input": { "command": "echo r" } },
                    },
                },
            }),
        )
        .expect("seed the jobs row");
    jobs.apply_config(&config_store.plugin_config("jobs"))
        .expect("apply the seed");
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        "MCP_GATEWAY_TOKEN",
        19999,
    );
    swiss_jobs::jobs::groups::register_job_scopes(&ctx.group_scopes, &jobs);
    // The jobs routes are the plugin's, merged the way server.rs merges them - the family
    // alone is not enough to prove /api/jobs rows carry the group.
    let app = build_app(ctx, None).merge(swiss_jobs::jobs::api::mount(jobs.clone()));
    (
        Harness {
            app,
            registry,
            store,
            calls,
            path: dir.join("managed.json"),
        },
        jobs,
    )
}

#[tokio::test]
async fn the_family_serves_the_jobs_scope() {
    let (h, jobs) = setup_with_jobs();

    // The whole list, one request shape like every scope.
    let (status, body) = h
        .put("/api/groups/jobs", json!({ "groups": ["default", "Ops"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));

    // Assign keeps the canonical casing and lands in the row the scheduler reads.
    let (status, body) = h
        .put("/api/groups/jobs/members/vacuum", json!({ "group": "ops" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));

    // The list answers the applied table: rows carry their group, the top level the names.
    let (status, list) = h.get("/api/jobs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["groups"], json!(["default", "Ops"]));
    let row = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == json!("vacuum"))
        .expect("the vacuum row");
    assert_eq!(row["group"], json!("Ops"));

    // A rename carries the members and answers how many moved.
    let (status, body) = h
        .post(
            "/api/groups/jobs/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["Basics", "Ops"]));

    // The flat order rewrites the definition order in the row.
    let before: Vec<String> = jobs.job_ids();
    let (status, body) = h
        .put(
            "/api/groups/jobs/order",
            json!({ "order": [before[1].clone(), before[0].clone()] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["order"],
        json!([before[1].clone(), before[0].clone()]),
        "the persisted order is the answer"
    );
    assert_eq!(jobs.job_ids(), vec![before[1].clone(), before[0].clone()]);

    // Unknown member: the family's one 404.
    let (status, body) = h
        .put("/api/groups/jobs/members/ghost", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body["error"],
        json!("unknown member: ghost"),
        "the family words it, not the scope"
    );
}

/// The same Move up shape through the jobs scope: definitions that render in the first
/// group by default (no group key in the row) must stay there when the list reorders.
#[tokio::test]
async fn reordering_job_groups_never_rehomes_the_first_groups_default_members() {
    let (h, _jobs) = setup_with_jobs();
    h.put("/api/groups/jobs", json!({ "groups": ["g1", "g2"] }))
        .await;
    h.put("/api/groups/jobs/members/vacuum", json!({ "group": "g2" }))
        .await;

    let (status, put) = h
        .put("/api/groups/jobs", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/jobs").await;
    let group_of = |name: &str| {
        list["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == json!(name))
            .unwrap_or_else(|| panic!("no row for {name}"))["group"]
            .clone()
    };
    assert_eq!(
        group_of("vacuum"),
        json!("g2"),
        "the explicit member never moves"
    );
    assert_eq!(group_of("report"), json!("g1"), "the default member stayed");
}

#[tokio::test]
async fn moving_a_job_between_groups_advances_the_config_revision_in_place() {
    // docs/11 8's guarantee, restated for groups (docs/20 G4): a group mutation is a config
    // edit, so the running table moves without a restart - configRevision advances and the
    // definitions all survive the apply.
    let (h, jobs) = setup_with_jobs();
    h.put("/api/groups/jobs", json!({ "groups": ["default", "Ops"] }))
        .await;
    let before = jobs.applied_revision();
    let (status, body) = h
        .put("/api/groups/jobs/members/report", json!({ "group": "Ops" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));
    let after = jobs.applied_revision();
    assert!(
        after != before,
        "revision {before} -> {after}: the row is content-revised, so a change means the\n        new row was written AND applied, not parked"
    );
    assert_eq!(jobs.job_ids().len(), 2, "the table survived the apply");
}

// --- OAuth authorize (docs/24 D5) ----------------------------------------------------------------
//
// One loopback server plays the whole Figma-shaped cast: the protected resource (bearer-gated
// /mcp), the authorization server (well-knowns, dynamic registration, both token grants) and —
// via the redirect the test drives itself — the browser. What runs here is the real POST/GET
// pair the panel will run.

/// Minimal percent-decoding for the test's own URL parsing (the crate's form_decode is
/// pub(crate); the redirect_uri and state in the authorization URL are percent-encoded).
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The fake remote's shared state: which bearer /mcp accepts, and what registrations it saw.
#[derive(Default)]
struct FakeOauthState {
    current: std::sync::Mutex<Option<String>>,
    registered_names: std::sync::Mutex<Vec<String>>,
}

async fn fake_figma_remote() -> (String, std::sync::Arc<FakeOauthState>) {
    let state = std::sync::Arc::new(FakeOauthState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let base = format!("http://127.0.0.1:{port}");
    let mk_meta = |b: String| {
        axum::routing::get(move || {
            let base = b.clone();
            async move {
                axum::Json(json!({
                    "issuer": base,
                    "authorization_endpoint": format!("{base}/oauth/authorize"),
                    "token_endpoint": format!("{base}/token"),
                    "registration_endpoint": format!("{base}/register"),
                    "response_types_supported": ["code"],
                    "code_challenge_methods_supported": ["S256"],
                }))
            }
        })
    };
    let protected = {
        let base = base.clone();
        axum::routing::get(move || {
            let base = base.clone();
            async move {
                axum::Json(json!({
                    "resource": format!("{base}/mcp"),
                    "authorization_servers": [base],
                }))
            }
        })
    };
    let reg_state = state.clone();
    let register = axum::routing::post(move |body: String| {
        let s = reg_state.clone();
        async move {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            s.registered_names.lock().expect("state").push(
                v.get("client_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            axum::Json(json!({
                "client_id": "e2e-cid",
                "client_secret": "e2e-sec",
                "token_endpoint_auth_method": "none",
            }))
        }
    });
    let token_state = state.clone();
    let token_base = base.clone();
    let token = axum::routing::post(move |body: String| {
        let s = token_state.clone();
        let base = token_base.clone();
        async move {
            let form: std::collections::HashMap<String, String> = body
                .split('&')
                .filter_map(|p| p.split_once('='))
                .map(|(k, v)| (url_decode(k), url_decode(v)))
                .collect();
            let grant = form.get("grant_type").cloned().unwrap_or_default();
            match grant.as_str() {
                // The code came in through the loopback redirect; minting mints the SAME
                // access token /mcp then demands, so the auto-start connects for real.
                "authorization_code" => {
                    *s.current.lock().expect("state") = Some("e2e-access".into());
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({
                            "access_token": "e2e-access",
                            "refresh_token": "e2e-refresh",
                            "expires_in": 3600,
                            "resource": format!("{base}/mcp"),
                        })),
                    )
                }
                "refresh_token" => (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "access_token": "e2e-refreshed",
                        "refresh_token": "e2e-refresh-2",
                        "expires_in": 3600,
                    })),
                ),
                _ => (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(json!({ "error": "unsupported_grant_type" })),
                ),
            }
        }
    });
    let mcp_state = state.clone();
    let mcp = axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
        let s = mcp_state.clone();
        async move {
            let expected = s.current.lock().expect("state").clone().unwrap_or_default();
            let got = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if got != format!("Bearer {expected}") {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    axum::Json(json!({ "error": "unauthorized" })),
                );
            }
            let message: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if message.get("id").is_none() {
                return (axum::http::StatusCode::OK, axum::Json(Value::Null));
            }
            match message.get("method").and_then(Value::as_str) {
                Some("initialize") => (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id"),
                        "result": {
                            "protocolVersion": "2025-06-18",
                            "capabilities": {},
                        },
                    })),
                ),
                _ => (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "jsonrpc": "2.0",
                        "id": message.get("id"),
                        "result": {
                            "tools": [
                                { "name": "get_file", "inputSchema": { "type": "object" } },
                                { "name": "get_code", "inputSchema": { "type": "object" } },
                            ],
                        },
                    })),
                ),
            }
        }
    });
    let app = axum::Router::new()
        .route("/.well-known/oauth-protected-resource", protected)
        .route(
            "/.well-known/oauth-authorization-server",
            mk_meta(base.clone()),
        )
        .route("/register", register)
        .route("/token", token)
        .route("/mcp", mcp);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, state)
}

/// Poll the authorize GET until the status is one of `want` (or the cap kills the test — the
/// flow task runs on this same runtime, so progress needs the await points the loop provides).
async fn poll_authorize(h: &Harness, name: &str, want: &[&str]) -> Value {
    for _ in 0..200 {
        let (status, body) = h.get(&format!("/api/mcps/{name}/authorize")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let now = body["status"].as_str().unwrap_or_default().to_string();
        if want.contains(&now.as_str()) {
            return body;
        }
        if now == "error" {
            panic!("flow errored: {body}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("authorize flow never reached {want:?}");
}

/// The browser's part: open the authorization URL's own redirect_uri with code + state.
async fn approve_in_a_browser(authorization_url: &str) {
    let query = authorization_url.split_once('?').expect("a query").1;
    let params: std::collections::HashMap<String, String> = query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (url_decode(k), url_decode(v)))
        .collect();
    let redirect = params.get("redirect_uri").expect("redirect_uri").clone();
    let state = params.get("state").expect("state").clone();
    let browser = reqwest::Client::new();
    let resp = browser
        .get(format!("{redirect}?code=e2e-code&state={state}"))
        .send()
        .await
        .expect("the loopback callback answers");
    assert_eq!(resp.status(), 200, "the callback page is the success page");
}

#[tokio::test]
async fn an_oauth_authorize_flow_runs_end_to_end() {
    let h = setup();
    let (base, _remote) = fake_figma_remote().await;
    h.register(
        "fig-e2e",
        json!({ "type": "http", "url": format!("{base}/mcp"), "auth": "oauth" }),
    );

    // Before any flow: the badge says needs-auth, exactly what lights the button.
    let (_, body) = h.get("/api/mcps").await;
    assert_eq!(row_named(&body, "fig-e2e")["oauth"], json!("needs-auth"));

    let (status, body) = h.post("/api/mcps/fig-e2e/authorize", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let flow_id = body["flowId"].as_str().expect("flowId").to_string();

    let ready = poll_authorize(&h, "fig-e2e", &["authorization_required"]).await;
    assert_eq!(ready["flowId"], json!(flow_id), "one flow, one id");
    let authorization_url = ready["authorizationUrl"]
        .as_str()
        .expect("authorizationUrl")
        .to_string();
    assert!(authorization_url.contains("code_challenge="));
    assert!(authorization_url.contains("state="));

    approve_in_a_browser(&authorization_url).await;

    let done = poll_authorize(&h, "fig-e2e", &["approved"]).await;
    // The auto-start's tools count — the two tools the fake announces.
    assert_eq!(done["tools"], json!(2), "{done}");
    assert!(h.has_server("fig-e2e"), "approval started the MCP");

    // The badge flipped without a page reload.
    let (_, body) = h.get("/api/mcps").await;
    assert_eq!(row_named(&body, "fig-e2e")["oauth"], json!("authorized"));
    // "unknown" is the honest state: http MCPs have no health ping on purpose (a metered
    // third-party endpoint must not be probed every 15s), and the handshake that DID succeed
    // is what has_server above asserted.
    assert_eq!(row_named(&body, "fig-e2e")["state"], json!("unknown"));

    // Deleting the MCP drops its grant: a future same-named MCP must not inherit it.
    let (status, body) = h.delete("/api/mcps/fig-e2e").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(swiss_mcp::oauth::credentials("fig-e2e").is_none());
}

#[tokio::test]
async fn authorize_refuses_non_oauth_mcps() {
    let h = setup();
    h.register_fixture("plain-e2e");
    let (status, body) = h.post("/api/mcps/plain-e2e/authorize", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("OAuth"));
}

#[tokio::test]
async fn a_second_post_while_live_hands_back_the_same_flow() {
    let h = setup();
    let (base, remote) = fake_figma_remote().await;
    h.register(
        "fig-sf",
        json!({ "type": "http", "url": format!("{base}/mcp"), "auth": "oauth" }),
    );
    let (_, first) = h.post("/api/mcps/fig-sf/authorize", json!({})).await;
    let flow_id = first["flowId"].as_str().expect("flowId").to_string();
    poll_authorize(&h, "fig-sf", &["authorization_required"]).await;
    // The flow is live (waiting on the browser); a second POST must not register a second
    // client or bind a second port — it hands back the SAME flow.
    let (status, again) = h.post("/api/mcps/fig-sf/authorize", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["flowId"], json!(flow_id));
    assert_eq!(again["status"], json!("authorization_required"));
    let names = remote.registered_names.lock().expect("state").clone();
    // One dynamic registration, not two: the allowlisted-name DCR is not free to spam.
    assert_eq!(names.len(), 1);
}

#[tokio::test]
async fn polling_without_a_flow_is_a_404() {
    let h = setup();
    let (status, _) = h.get("/api/mcps/nobody/authorize").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_figma_type_adds_with_nothing_but_a_name() {
    // docs/24 rev: figma is its own type — the panel asks for a name, the type decides the
    // endpoint and the OAuth mode. What this pins is the SHAPE: the row tags figma even though
    // the adapter it builds is http, the badge rides the shared OAuth predicate, and the
    // stored def stays one field long. (The authorize flow itself is not driven here: the
    // figma endpoint is the real Figma one, and the flow against a fake remote is already
    // covered by the http+oauth end-to-end test above — the same code path.)
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "fig-type", "type": "figma", "description": "design files" }),
        )
        .await;
    // CREATED with lifecycle error: the add succeeded, the auto-start answers needs-auth
    // — exactly the http+oauth behavior, reached with none of the fields.
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["lifecycle"], json!("error"), "{body}");

    let (_, rows) = h.get("/api/mcps").await;
    let row = row_named(&rows, "fig-type");
    assert_eq!(row["type"], json!("figma"), "{row}");
    assert_eq!(row["tag"], json!("figma"), "{row}");
    assert_eq!(row["oauth"], json!("needs-auth"), "{row}");

    // The stored def keeps its one-field shape — no url, no auth key the user never wrote.
    let (_, d) = h.get("/api/mcps/fig-type/details").await;
    assert_eq!(d["config"]["type"], json!("figma"), "{d}");
    assert_eq!(d["config"].get("url"), None, "{d}");
    assert_eq!(d["config"].get("auth"), None, "{d}");

    // A url on a figma def is a second way to say what the type already says — refused,
    // so the def cannot be configured wrong.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "fig-two", "type": "figma", "url": "https://example.com/mcp" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("decided by the figma type"),
        "{body}"
    );
}
#[tokio::test]
async fn the_zai_vision_type_adds_and_keeps_the_key_a_reference() {
    // The zai-vision type is the @z_ai/mcp-server port compiled in: no child, no endpoint of
    // its own. What this pins is the SHAPE - the row tags zai-vision, the def keeps the apiKey
    // exactly as written (${...} reference stays a reference, docs/19), a literal key is
    // refused at add, mode is validated, and a reference that cannot expand fails the build
    // at the engine. Tool calls are not driven here: the engine's request shape is pinned in
    // swiss-mcp's unit tests against a fake OpenAI-compatible server, and every call costs
    // real tokens.
    std::env::set_var("ZAI_E2E_KEY", "e2e-key");
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "zai-type",
                "type": "zai-vision",
                "apiKey": "${ZAI_E2E_KEY}",
                "mode": "ZHIPU",
                "description": "GLM vision tools"
            }),
        )
        .await;
    // CREATED: the engine builds without touching the network (the HTTP client is lazy and
    // there is no ping - a metered API must not be probed), so the add lands started.
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["lifecycle"], json!("started"), "{body}");

    let (_, rows) = h.get("/api/mcps").await;
    let row = row_named(&rows, "zai-type");
    assert_eq!(row["type"], json!("zai-vision"), "{row}");
    assert_eq!(row["tag"], json!("zai-vision"), "{row}");

    // The stored def keeps the ${...} reference verbatim - never the expanded literal.
    let (_, d) = h.get("/api/mcps/zai-type/details").await;
    assert_eq!(d["config"]["apiKey"], json!("${ZAI_E2E_KEY}"), "{d}");
    assert_eq!(d["config"]["mode"], json!("ZHIPU"), "{d}");

    // A literal key is refused: it would ride to the panel unmasked, and the sealed env
    // store is where a credential belongs.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "zai-literal", "type": "zai-vision", "apiKey": "sk-live-123" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("must be a ${"),
        "{body}"
    );

    // A reference that cannot expand (the variable is absent) builds an empty key, and the
    // engine refuses it - the add fails with the engine's reason.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "zai-broken", "type": "zai-vision", "apiKey": "${ZAI_E2E_KEY_MISSING}" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("apiKey is required"),
        "{body}"
    );

    // The validation edge: no key, or a mode that is not one of the two, is a 400 at add time.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "zai-nokey", "type": "zai-vision" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("apiKey is required"),
        "{body}"
    );
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "zai-badmode", "type": "zai-vision", "apiKey": "${ZAI_E2E_KEY}", "mode": "nope" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown mode"),
        "{body}"
    );
}

// --- start-at-sign-in (src/autostart.rs) ---------------------------------------------------------
//
// The OS registration is real machine state, so the suite exercises the read and the "off"
// write only: "off" is idempotent on every platform (a missing Run value / plist / unit is
// success), while "on" would register the TEST binary as a login item on whatever machine
// runs this file — not something a green suite should leave behind.

#[tokio::test]
async fn autostart_route_reads_the_os_registration() {
    let h = setup();
    let (status, body) = h.get("/api/autostart").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["enabled"].is_boolean(),
        "enabled must be a bool: {body}"
    );
    assert!(
        body["detail"].is_string(),
        "detail must say where the registration lives: {body}"
    );
    assert!(
        body["command"].is_string(),
        "command must say what the entry runs: {body}"
    );
}

#[tokio::test]
async fn autostart_route_turns_off_idempotently() {
    let h = setup();
    let (status, body) = h.put("/api/autostart", json!({ "enabled": false })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false, "{body}");
}

#[tokio::test]
async fn autostart_route_refuses_a_body_without_a_verdict() {
    let h = setup();
    let (status, body) = h.put("/api/autostart", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].is_string(), "{body}");
}

// ---- docs/28 D1: replace / restore / revisions ----------------------------------------------
// The operator's flow this serves: park the current def of a name, install a new one under
// the SAME name, roll back with one click. "One active def per name" is structural — the
// revision list is inert data and never reaches the registry or the boot path.

async fn add_echo(h: &Harness, name: &str, enabled: bool) {
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": name, "type": "echo", "enabled": enabled }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "add {name}: {body}");
}

#[tokio::test]
async fn replace_parks_the_old_def_and_serves_the_new_one() {
    let _lock = traffic_lock().await; // the admin posts below land in the shared traffic ring
    let h = setup();
    add_echo(&h, "m", true).await;
    let (status, body) = h
        .post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp", "note": "swap one" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["parked"], json!(true));
    assert_eq!(body["type"], json!("http"));
    assert_eq!(body["revisions"], json!(1));
    // x.test does not resolve: the swap stands, the start failure rides in the body — the
    // rollback path must be reachable, not buried under a 500.
    assert!(body["restartError"].is_string(), "{body}");
    assert!(!h.has_server("m"));
    // The live def is the new one; exactly one row answers to the name (one active per name).
    assert_eq!(h.def_of("m")["type"], json!("http"));
    assert_eq!(h.names().await, vec!["m".to_string()]);
    // The parked snapshot is the OLD def, note and all.
    let (status, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["revisions"][0]["type"], json!("echo"));
    assert_eq!(list["revisions"][0]["note"], json!("swap one"));
    assert!(list["revisions"][0]["at"].as_i64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn a_failing_replace_changes_nothing() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", true).await;
    // A rest def with no tools fails build_def — before anything is written.
    let (status, body) = h
        .post("/api/mcps/m/replace", json!({ "type": "rest" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(h.def_of("m")["type"], json!("echo"));
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn replace_leaves_a_stopped_mcp_stopped() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", false).await;
    assert!(!h.has_server("m"), "enabled:false never started it");
    let (status, body) = h
        .post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["lifecycle"],
        json!("stopped"),
        "a def swap is not a start"
    );
    assert!(!h.has_server("m"));
}

#[tokio::test]
async fn restore_swaps_back_and_parks_the_live_def() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", true).await;
    h.post(
        "/api/mcps/m/replace",
        json!({ "type": "http", "url": "https://x.test/mcp", "note": "swap" }),
    )
    .await;
    let (status, body) = h.post("/api/mcps/m/revisions/0/restore", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restored"], json!(0));
    assert_eq!(
        h.def_of("m")["type"],
        json!("echo"),
        "the parked def is live again"
    );
    // The replaced def was parked in turn — rollback is itself reversible.
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["revisions"][0]["type"], json!("http"));
    // Restoring an index that is not there refuses loudly instead of guessing.
    let (status, body) = h.post("/api/mcps/m/revisions/5/restore", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn six_replaces_keep_only_the_last_five_snapshots() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", true).await;
    for i in 0..6 {
        let (status, _) = h
            .post(
                "/api/mcps/m/replace",
                json!({ "type": "http", "url": "https://x.test/mcp", "note": format!("gen {i}") }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    let notes: Vec<String> = list["revisions"]
        .as_array()
        .expect("revisions")
        .iter()
        .map(|r| r["note"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(notes.len(), 5, "the cap holds through the routes");
    assert_eq!(notes[0], "gen 1", "the oldest was evicted");
}

#[tokio::test]
async fn rename_carries_revisions_and_delete_clears_them() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", true).await;
    h.post(
        "/api/mcps/m/replace",
        json!({ "type": "http", "url": "https://x.test/mcp", "note": "rides" }),
    )
    .await;
    let (status, _) = h.post("/api/mcps/m/rename", json!({ "name": "m2" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = h.get("/api/mcps/m2/revisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["revisions"][0]["note"], json!("rides"));
    let (status, _) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the old name is gone");
    // Deleting the MCP drops its parked snapshots with it — a future same-named MCP starts
    // clean (same rule the OAuth credentials already follow above).
    let (status, _) = h.delete("/api/mcps/m2").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/api/mcps/m2/revisions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_revision_can_be_dropped_without_touching_the_rest() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", true).await;
    for note in ["a", "b"] {
        h.post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp", "note": note }),
        )
        .await;
    }
    let (status, _) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["revisions"][0]["note"], json!("b"));
    let (status, _) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::OK);
    // The list is empty now; deleting again says so (404: nothing parked) instead of erroring.
    let (status, body) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_disabled_mcp_refuses_clients_with_the_disabled_wording() {
    let _lock = traffic_lock().await;
    let h = setup();
    add_echo(&h, "m", false).await;
    // docs/28 D2: disabled means the client sees nothing of it — every method answers the
    // same refusal, and the wording names the operator's verb and the way out.
    let (status, body) = h
        .mcp(
            "/mcp/m",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = body.to_string();
    assert!(
        text.contains("disabled"),
        "wording must say disabled: {text}"
    );
    assert!(text.contains("enable it from the panel"), "{text}");
}

#[tokio::test]
async fn a_mariadb_def_builds_and_carries_its_own_tag() {
    let _lock = traffic_lock().await;
    let h = setup();
    // docs/29: MariaDB is its own type — the def, the tag, the seal — on the mysql engine.
    // Disabled so the test never depends on a server existing anywhere.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "mdb", "type": "mariadb", "host": "127.0.0.1", "port": 3306,
                    "user": "u", "password": "${MDB_PW}", "database": "d", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, list) = h.get("/api/mcps").await;
    let row = list["mcps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "mdb")
        .expect("row")
        .clone();
    assert_eq!(row["tag"], "mariadb");
}
