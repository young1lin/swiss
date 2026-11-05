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

//! `swiss setup`, driven against the real router on a real loopback listener (SPEC §host.setup), so
//! what the CLI sends is what the routes answer, with nothing faked between them. A layer records
//! each request's method and path, which is what "writes nothing" is measured by.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use serde_json::{json, Value};

use swiss::app::{build_app, AppContext};
use swiss::setup_cli::{apply_at, plan_at, Input, Options};
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::registry::Registry;

const TOKEN: &str = "setup-tok-0123456789abcdef";

/// Point the whole test binary at a scratch data dir and one deterministic master key before
/// anything can read either, so nothing lands in the operator's real `~/.swiss`.
fn sandbox() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("swiss-setup-home-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&home).expect("create the scratch home");
        // Safety: this runs once, before any test has touched the paths or key modules.
        unsafe {
            std::env::set_var("SWISS_HOME", &home);
            std::env::set_var("USERPROFILE", home.join("user"));
            std::env::set_var("HOME", home.join("user"));
            std::env::set_var("CLAUDE_CONFIG_DIR", home.join("user/.claude"));
            std::env::set_var("SWISS_MASTER_KEY", "ab".repeat(32));
        }
    });
}

type Requests = Arc<Mutex<Vec<(String, String)>>>;

struct Gateway {
    port: u16,
    registry: Arc<Registry>,
    requests: Requests,
}

impl Gateway {
    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("the request log").clone()
    }

    fn writes(&self) -> Vec<(String, String)> {
        self.requests().into_iter().filter(|(method, _)| method != "GET").collect()
    }
}

async fn record(requests: Requests, request: Request<Body>, next: Next) -> Response {
    let line = (request.method().to_string(), request.uri().path().to_string());
    requests.lock().expect("the request log").push(line);
    next.run(request).await
}

async fn gateway() -> Gateway {
    sandbox();
    let dir = std::env::temp_dir().join(format!("swiss-setup-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(registry.clone(), tokens, store, calls, traffic, "SWISS_TOKEN", 19999);
    let requests = Requests::default();
    let seen = requests.clone();
    let app = build_app(ctx, None).layer(middleware::from_fn(move |request, next| record(seen.clone(), request, next)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("a bound address").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Gateway { port, registry, requests }
}

fn input(manifest: Value, base: &Path) -> Input {
    Input { text: manifest.to_string(), base: base.to_path_buf(), file: "swiss.setup.json".into() }
}

fn scratch_with(file: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("swiss-setup-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    std::fs::write(dir.join(file), text).expect("write the scratch file");
    dir
}

fn item<'a>(document: &'a Value, kind: &str, name: &str) -> &'a Value {
    document["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["kind"] == json!(kind) && i["name"] == json!(name)))
        .unwrap_or_else(|| panic!("no {kind} {name} in {document}"))
}

// SPEC §host.setup: `plan` "writes nothing, starts nothing".
#[tokio::test]
async fn plan_writes_nothing_and_starts_nothing() {
    let gw = gateway().await;
    let base = scratch_with("value", "a-source-value\n");
    let manifest = json!({
        "version": 1,
        "secrets": { "plan-one": "file:value", "plan-asked": "ask" },
        "mcps": {
            "tool": { "command": "nosuchprogram", "args": ["--port", "1"] },
            "web": { "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": { "Authorization": "Bearer ${secret://plan-one}" } },
            "db": { "type": "mysql", "password": "${secret://plan-asked}" },
        },
    });

    let (document, code) = plan_at(gw.port, &input(manifest, &base), Options::default()).await;

    assert_eq!(code, 4, "{document}");
    assert_eq!(document["ok"], json!(false));
    assert_eq!(document["mode"], json!("plan"));
    assert_eq!(document["gateway"]["port"], json!(gw.port));
    assert_eq!(item(&document, "secret", "plan-one")["status"], json!("will-create"));
    assert_eq!(item(&document, "secret", "plan-asked")["status"], json!("needs-input"));
    assert_eq!(item(&document, "secret", "plan-asked")["code"], json!("secret.ask"));
    assert_eq!(item(&document, "mcp", "tool")["status"], json!("will-create"));
    assert_eq!(item(&document, "mcp", "tool")["executes"], json!("nosuchprogram --port 1"));
    assert_eq!(item(&document, "mcp", "web")["status"], json!("will-create"));
    assert_eq!(item(&document, "mcp", "db")["status"], json!("blocked"));
    assert_eq!(item(&document, "mcp", "db")["by"], json!(["secret:plan-asked"]));
    assert_eq!(document["summary"]["willCreate"], json!(3));
    assert_eq!(document["summary"]["needsInput"], json!(1));
    assert_eq!(document["summary"]["blocked"], json!(1));
    assert_eq!(document["next"][0]["who"], json!("human"));
    assert_eq!(document["next"][1]["do"], json!("swiss setup apply swiss.setup.json"));

    assert!(gw.writes().is_empty(), "{:?}", gw.writes());
    assert!(!gw.requests().is_empty());
    assert!(gw.registry.get("tool").is_none(), "a plan adds nothing");
    let (status, listing) = gw_get(gw.port, "/api/secrets").await;
    assert_eq!(status, 200);
    assert!(!listing["secrets"].as_array().expect("names").contains(&json!("plan-one")));
    assert!(!document.to_string().contains("a-source-value"), "{document}");
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().expect("a client")
}

async fn gw_get(port: u16, path: &str) -> (u16, Value) {
    let answer = client().get(format!("http://127.0.0.1:{port}{path}")).send().await.expect("the gateway answers");
    let status = answer.status().as_u16();
    (status, answer.json().await.expect("a JSON answer"))
}

/// One write through the real route, as the panel or `swiss api` would make it.
async fn gw_write(port: u16, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
    let answer = client()
        .request(method, format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .send()
        .await
        .expect("the gateway answers");
    let status = answer.status().as_u16();
    (status, answer.json().await.expect("a JSON answer"))
}

// SPEC §host.setup: an item is compared with the masked read-back, so what `POST /api/mcps` stored
// from the same words reads as the same definition, for every kind a manifest can name.
#[tokio::test]
async fn a_definition_already_on_the_gateway_reads_as_unchanged() {
    let gw = gateway().await;
    let mcps = json!({
        "tool": { "command": "nosuchprogram", "args": ["-y", "pkg"], "env": { "API_KEY": "${secret://shared-key}" } },
        "web": { "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": { "Authorization": "Bearer ${secret://shared-key}" } },
        "db": { "type": "mysql", "host": "127.0.0.1", "port": 13306, "password": "${secret://shared-key}" },
    });
    let (status, _) = gw_write(
        gw.port,
        reqwest::Method::PUT,
        "/api/secrets/shared-key",
        json!({ "value": "x", "rev": gw_get(gw.port, "/api/secrets").await.1["rev"] }),
    )
    .await;
    assert_eq!(status, 200);
    for (name, def) in mcps.as_object().expect("an object") {
        let mut body = def.clone();
        body["name"] = json!(name);
        body["start"] = json!(false);
        let (status, answer) = gw_write(gw.port, reqwest::Method::POST, "/api/mcps", body).await;
        assert_eq!(status, 201, "{answer}");
    }
    let before = gw.writes().len();

    let same = input(json!({ "mcps": mcps }), Path::new(""));
    let (document, code) = plan_at(gw.port, &same, Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["ok"], json!(true));
    assert_eq!(document["summary"]["unchanged"], json!(3), "{document}");
    assert!(document["warnings"].as_array().expect("warnings").is_empty(), "{document}");

    // One changed word is one update, and only that one.
    let mut changed = mcps.clone();
    changed["db"]["port"] = json!(13307);
    let (document, code) = plan_at(gw.port, &input(json!({ "mcps": changed }), Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "mcp", "db")["status"], json!("will-update"));
    assert_eq!(item(&document, "mcp", "web")["status"], json!("unchanged"));
    assert_eq!(gw.writes().len(), before, "a plan writes nothing");
}

// SPEC §host.setup: exit 2, "every problem listed", and the gateway is not asked anything.
#[tokio::test]
async fn a_refused_manifest_asks_the_gateway_nothing() {
    let gw = gateway().await;
    let manifest = json!({ "mcps": { "db": { "type": "mysql", "password": "S3ntinelValue9" } } });
    let (document, code) = plan_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 2, "{document}");
    assert_eq!(document["ok"], json!(false));
    assert_eq!(document["problems"][0]["path"], json!("mcps.db.password"));
    assert!(!document.to_string().contains("S3ntinelValue9"), "{document}");
    assert!(gw.requests().is_empty(), "{:?}", gw.requests());
}

// SPEC §host.setup: exit 3 is a gateway that is not there.
#[tokio::test]
async fn no_gateway_exits_3() {
    sandbox();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("a bound address").port();
    drop(listener);
    let (document, code) = plan_at(port, &input(json!({}), Path::new("")), Options::default()).await;
    assert_eq!(code, 3, "{document}");
    assert_eq!(document["ok"], json!(false));
    assert!(document["error"].is_string(), "{document}");
}

const NO_CHECK: Options = Options { rotate_secrets: false, no_check: true };

/// A rest MCP answering on the gateway itself: `POST /api/mcpdefs/test` takes any HTTP answer as
/// reachable, so this one checks out with no network at all.
fn rest_here(port: u16, secret: &str) -> Value {
    json!({
        "type": "rest",
        "baseUrl": format!("http://127.0.0.1:{port}/api/info"),
        "headers": { "Authorization": format!("Bearer ${{secret://{secret}}}") },
        "tools": [{ "name": "x", "request": { "method": "GET", "path": "/x" } }],
    })
}

// SPEC §host.setup: "the same manifest applied twice reports only `unchanged` the second time".
#[tokio::test]
async fn apply_twice_is_all_unchanged_and_writes_nothing() {
    let gw = gateway().await;
    let base = scratch_with("value", "twice-value\n");
    let mut manifest = json!({
        "version": 1,
        "secrets": { "twice-key": "file:value" },
        "mcps": {
            "twice-echo": { "type": "echo" },
            "twice-tool": { "command": "nosuchprogram", "args": ["--flag", "1"] },
            "twice-rest": rest_here(gw.port, "twice-key"),
        },
    });

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["ok"], json!(true));
    assert_eq!(document["mode"], json!("apply"));
    assert_eq!(document["summary"]["created"], json!(4), "{document}");
    assert_eq!(item(&document, "mcp", "twice-tool")["executes"], json!("nosuchprogram --flag 1"));
    assert!(item(&document, "mcp", "twice-tool").get("check").is_none(), "{document}");
    assert!(gw.registry.has("twice-tool"));
    let lifecycle = |name: &str| {
        let entry = gw.registry.get(name).expect("registered");
        let lifecycle = entry.data.read().expect("the entry").lifecycle.as_str();
        lifecycle
    };
    // `--no-check` leaves a child to the first request that wants it, and the rest are up.
    assert_eq!(lifecycle("twice-tool"), "idle");
    assert_eq!(lifecycle("twice-echo"), "started");
    assert_eq!(lifecycle("twice-rest"), "started");

    let before = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(manifest.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["summary"]["unchanged"], json!(4), "{document}");
    assert_eq!(document["summary"]["created"], json!(0), "{document}");
    assert_eq!(gw.writes().len(), before, "{:?}", gw.writes());

    // A check is not a write, and an unchanged item is checked all the same: only the throwaway test
    // reaches the gateway, because what is already running needs no start.
    manifest["mcps"].as_object_mut().expect("mcps").remove("twice-tool");
    let (document, code) = apply_at(gw.port, &input(manifest, &base), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "mcp", "twice-echo")["status"], json!("unchanged"));
    assert_eq!(item(&document, "mcp", "twice-echo")["check"]["ok"], json!(true), "{document}");
    assert!(
        item(&document, "mcp", "twice-echo")["check"]["detail"].as_str().expect("a detail").ends_with("tools"),
        "{document}"
    );
    assert_eq!(item(&document, "mcp", "twice-rest")["check"]["ok"], json!(true), "{document}");
    assert_eq!(
        gw.writes()[before..],
        [("POST".to_string(), "/api/mcpdefs/test".to_string())],
        "{:?}",
        gw.writes()
    );
}

// SPEC §host.setup: an item is keyed by name, and only the one that differs is written.
#[tokio::test]
async fn a_changed_definition_is_updated_and_only_that_one() {
    let gw = gateway().await;
    let mut manifest = json!({ "mcps": {
        "same": { "command": "nosuchprogram" },
        "moved": { "command": "nosuchprogram", "args": ["--port", "1"] },
    } });
    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");

    manifest["mcps"]["moved"]["args"] = json!(["--port", "2"]);
    let before = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "mcp", "same")["status"], json!("unchanged"));
    assert_eq!(item(&document, "mcp", "moved")["status"], json!("updated"));
    assert_eq!(document["summary"]["updated"], json!(1));
    assert_eq!(gw.writes()[before..], [("PUT".to_string(), "/api/mcps/moved".to_string())]);
    let entry = gw.registry.get("moved").expect("registered");
    let command = entry.data.read().expect("the entry").def.get_str("command").map(str::to_string);
    assert_eq!(command.as_deref(), Some("nosuchprogram --port 2"));
}

// SPEC §host.setup: a secret with no source blocks what uses it, the rest go ahead, and the same file
// converges once a human has stored it.
#[tokio::test]
async fn a_missing_secret_blocks_its_users_and_exits_4() {
    let gw = gateway().await;
    let manifest = json!({
        "secrets": { "wait-key": "ask" },
        "mcps": { "wait-rest": rest_here(gw.port, "wait-key"), "wait-free": { "type": "echo" } },
    });

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 4, "{document}");
    assert_eq!(document["ok"], json!(false));
    assert_eq!(item(&document, "secret", "wait-key")["status"], json!("needs-input"));
    assert_eq!(item(&document, "mcp", "wait-rest")["status"], json!("blocked"));
    assert_eq!(item(&document, "mcp", "wait-rest")["by"], json!(["secret:wait-key"]));
    assert_eq!(item(&document, "mcp", "wait-free")["status"], json!("created"));
    assert_eq!(item(&document, "mcp", "wait-free")["check"]["ok"], json!(true), "{document}");
    assert!(!gw.registry.has("wait-rest"), "a blocked item writes nothing");
    assert!(gw.registry.has("wait-free"), "a blocked item stops nothing else");
    assert_eq!(document["next"][0]["who"], json!("human"));
    assert_eq!(document["next"][1]["do"], json!("swiss setup apply swiss.setup.json"));

    let rev = gw_get(gw.port, "/api/secrets").await.1["rev"].clone();
    let body = json!({ "value": "stored-by-a-human", "rev": rev });
    let (status, _) = gw_write(gw.port, reqwest::Method::PUT, "/api/secrets/wait-key", body).await;
    assert_eq!(status, 200);

    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "secret", "wait-key")["status"], json!("unchanged"));
    assert_eq!(item(&document, "mcp", "wait-rest")["status"], json!("created"));
    assert_eq!(item(&document, "mcp", "wait-rest")["check"]["ok"], json!(true), "{document}");
    assert_eq!(document["next"], json!([]));
    assert!(!document.to_string().contains("stored-by-a-human"), "{document}");
}

// SPEC §host.setup: "A saved item whose check fails keeps its status and sets `ok` false".
#[tokio::test]
async fn a_failed_check_exits_1_with_the_item_saved() {
    let gw = gateway().await;
    let manifest = json!({ "mcps": {
        "gone-web": { "type": "http", "url": "http://127.0.0.1:1/mcp" },
        "gone-tool": { "command": "nosuchprogram" },
        "fine": { "type": "echo" },
    } });

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 1, "{document}");
    assert_eq!(document["ok"], json!(false));
    for name in ["gone-web", "gone-tool"] {
        let saved = item(&document, "mcp", name);
        assert_eq!(saved["status"], json!("created"), "{document}");
        assert_eq!(saved["check"]["ok"], json!(false), "{document}");
        assert!(saved["check"]["code"].as_str().expect("a code").starts_with("check."), "{document}");
        assert!(saved["check"]["error"].is_string(), "{document}");
        assert!(gw.registry.has(name), "the definition stays saved");
    }
    assert_eq!(item(&document, "mcp", "fine")["check"]["ok"], json!(true), "{document}");

    // Nothing is left to write, and the same answer comes back until the fix lands.
    let before = gw.writes().iter().filter(|(_, path)| !is_probe(path)).count();
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 1, "{document}");
    assert_eq!(item(&document, "mcp", "gone-web")["status"], json!("unchanged"));
    assert_eq!(item(&document, "mcp", "gone-web")["check"]["ok"], json!(false), "{document}");
    assert_eq!(gw.writes().iter().filter(|(_, path)| !is_probe(path)).count(), before);
}

fn is_probe(path: &str) -> bool {
    path == "/api/mcpdefs/test" || path.ends_with("/start")
}

// SPEC §host.setup: an OAuth MCP "is not checked and reads `needs-input` until it is authorized".
#[tokio::test]
async fn an_oauth_mcp_is_saved_and_waits_for_a_sign_in() {
    let gw = gateway().await;
    let manifest = json!({ "mcps": { "design": { "type": "figma" } } });
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 4, "{document}");
    let waiting = item(&document, "mcp", "design");
    assert_eq!(waiting["status"], json!("needs-input"));
    assert_eq!(waiting["code"], json!("mcp.oauth"));
    assert!(waiting.get("check").is_none(), "{document}");
    assert!(gw.registry.has("design"), "the definition is saved");
    assert_eq!(gw.registry.get("design").expect("registered").data.read().expect("the entry").lifecycle.as_str(), "stopped");
    assert!(document["next"][0]["do"].as_str().expect("a step").contains("design"), "{document}");
    assert_eq!(document["next"][1]["who"], json!("agent"));
}

// SPEC §host.setup: a stop is someone's choice, and neither an edit nor a check undoes it.
#[tokio::test]
async fn what_someone_stopped_stays_stopped_and_the_document_says_so() {
    let gw = gateway().await;
    let manifest = json!({ "mcps": { "held": { "type": "echo" } } });
    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    let (status, _) = gw_write(gw.port, reqwest::Method::POST, "/api/mcps/held/stop", json!({})).await;
    assert_eq!(status, 200);

    let before = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "mcp", "held")["status"], json!("unchanged"));
    assert_eq!(document["warnings"][0]["code"], json!("mcp.left-stopped"));
    assert_eq!(document["warnings"][0]["path"], json!("mcps.held"));
    assert_eq!(gw.writes().len(), before, "{:?}", gw.writes());
    assert_eq!(gw.registry.get("held").expect("registered").data.read().expect("the entry").lifecycle.as_str(), "stopped");
}

// SPEC §host.setup: "`--rotate-secrets` rewrites those with a readable source".
#[tokio::test]
async fn a_rotation_rewrites_only_what_was_asked_for() {
    let gw = gateway().await;
    let base = scratch_with("value", "first\n");
    let manifest = json!({ "secrets": { "turned-key": "file:value" } });
    let (document, code) = apply_at(gw.port, &input(manifest.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "secret", "turned-key")["status"], json!("created"));
    // The vault is per process and other tests write it, so this reads the value, never the rev.
    let stored = || swiss_core::secure::secretstore::vault_lookup("turned-key");
    assert_eq!(stored().as_deref(), Some("first"));
    std::fs::write(base.join("value"), "second\n").expect("a new value at the source");

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "secret", "turned-key")["status"], json!("unchanged"));
    assert_eq!(stored().as_deref(), Some("first"), "a stored secret is not rewritten unasked");

    let rotating = Options { rotate_secrets: true, no_check: true };
    let (document, code) = apply_at(gw.port, &input(manifest, &base), rotating).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "secret", "turned-key")["status"], json!("updated"));
    assert_eq!(stored().as_deref(), Some("second"));
}

// SPEC §host.setup: exit 2 writes nothing, under `apply` as under `plan`.
#[tokio::test]
async fn a_refused_manifest_applies_nothing() {
    let gw = gateway().await;
    let manifest = json!({ "mcps": { "db": { "type": "mysql", "password": "S3ntinelValue9" }, "fine": { "type": "echo" } } });
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 2, "{document}");
    assert_eq!(document["mode"], json!("apply"));
    assert!(!document.to_string().contains("S3ntinelValue9"), "{document}");
    assert!(gw.requests().is_empty(), "{:?}", gw.requests());
    assert!(!gw.registry.has("fine"));
}
