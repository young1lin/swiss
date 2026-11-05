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
//! what the CLI sends is what the routes answer, with nothing faked between them. Its plugins run on
//! the real host, as the server composes them. A layer records each request's method and target, which
//! is what "writes nothing" is measured by.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use serde_json::{json, Value};

use swiss::app::{build_app, AppContext};
use swiss::builtin;
use swiss::setup_cli::{apply_at, plan_at, plugin_schema_at, Input, Options};
use swiss_core::secure::secretstore;
use swiss_host::config_store::ConfigStore;
use swiss_host::host::PluginHost;
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_jobs::jobs::JobSystem;
use swiss_mcp::registry::Registry;
use swiss_tunnels::tunnel::manager::TunnelManager;

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

/// What runs just before the next write to a target, so the real route answers as it does to a writer
/// who lost a race.
type Hooks = Arc<Mutex<Vec<(String, Box<dyn FnOnce() + Send>)>>>;

struct Gateway {
    port: u16,
    registry: Arc<Registry>,
    host: Arc<PluginHost>,
    requests: Requests,
    hooks: Hooks,
}

impl Gateway {
    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("the request log").clone()
    }

    /// What can change something: a `?dryRun=1` only asks.
    fn writes(&self) -> Vec<(String, String)> {
        self.requests().into_iter().filter(|(method, target)| method != "GET" && !target.ends_with("?dryRun=1")).collect()
    }

    fn count(&self, method: &str, target: &str) -> usize {
        self.requests().iter().filter(|(m, t)| m == method && t == target).count()
    }

    fn before_next(&self, target: &str, hook: impl FnOnce() + Send + 'static) {
        self.hooks.lock().expect("the hooks").push((target.to_string(), Box::new(hook)));
    }
}

async fn record(requests: Requests, hooks: Hooks, request: Request<Body>, next: Next) -> Response {
    let target = request.uri().path_and_query().map_or_else(|| request.uri().path().to_string(), ToString::to_string);
    let writing = request.method() != axum::http::Method::GET;
    requests.lock().expect("the request log").push((request.method().to_string(), target.clone()));
    let hook = if writing {
        let mut hooks = hooks.lock().expect("the hooks");
        hooks.iter().position(|(wanted, _)| *wanted == target).map(|at| hooks.remove(at).1)
    } else {
        None
    };
    if let Some(hook) = hook {
        hook();
    }
    next.run(request).await
}

async fn gateway() -> Gateway {
    compose(json!({}), true, false).await
}

/// A gateway booted on this config file.
async fn gateway_with(raw: Value) -> Gateway {
    compose(raw, true, false).await
}

/// The llm plugin opens one database under the home this binary shares, so only the tests about it
/// compose it.
async fn gateway_with_llm(raw: Value) -> Gateway {
    compose(raw, true, true).await
}

/// The built-ins on the real host, composed as `src/server.rs` composes them. `start` false leaves the
/// boot to the test: the port is open and no plugin has started, which is how a new gateway first reads.
async fn compose(raw: Value, start: bool, llm: bool) -> Gateway {
    sandbox();
    let dir = std::env::temp_dir().join(format!("swiss-setup-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let managed = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(managed.clone(), Some(TOKEN)));
    let ctx = AppContext::new(registry.clone(), tokens, managed.clone(), calls.clone(), traffic.clone(), "SWISS_TOKEN", 19999);

    let config_store = ConfigStore::from_loaded(dir.join("gateway.config.json"), raw);
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    let jobs = JobSystem::open(dir.join("jobs.json"), services.clone(), config_store.clone());
    let tunnel_store = Arc::new(Mutex::new(swiss_tunnels::tunnel::TunnelStore::new(dir.join("tunnels.json"), 19998)));
    let tunnel_manager = TunnelManager::new(tunnel_store.clone(), None);
    let tunnels = Arc::new(swiss_tunnels::tunnel::api::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        mcp_display: Some(swiss::mcp_link::registry_display(registry.clone())),
    });
    let _ = ctx.catalog.set(services.catalog.clone());
    let process_state = swiss_process::api::ProcessState::new();
    let mut host = PluginHost::new(config_store);
    let deps = builtin::BuiltinDeps {
        registry: registry.clone(),
        managed,
        jobs: jobs.clone(),
        tunnels: tunnels.clone(),
        tunnel_manager,
        services: services.clone(),
        call_log: calls,
        traffic,
        logs_dir: dir.join("logs"),
        process_state: process_state.clone(),
        data_history: swiss_data::history::DataHistory::new(dir.join("logs")),
    };
    builtin::register_all(&mut host, &deps).expect("the built-ins register without conflicts");
    let mut extra = swiss_tunnels::tunnel::api::mount(tunnels)
        .merge(swiss_jobs::jobs::api::mount(jobs))
        .merge(swiss_host::services::api::mount(services.clone()))
        .merge(swiss_process::api::mount(process_state));
    if llm {
        let state = swiss_llm::LlmState::new();
        let analysis = swiss_plugins::analysis::AnalysisStore::at(dir.join("analysis"));
        let consoles = swiss_plugins::db_consoles::ConsoleStore::at(dir.join("consoles"), services.bus.clone());
        let plugin = swiss_plugins::llm::LlmPlugin::new(services.clone(), state.clone(), consoles.clone());
        host.register(Arc::new(plugin)).expect("the llm plugin registers");
        extra = extra.merge(swiss_plugins::llm_api::mount_with_analysis(state, services.clone(), analysis, consoles));
    }
    host.set_capability_probe({
        let catalog = services.catalog.clone();
        Arc::new(move |cap: &str| cap == "connection-catalog" && catalog.has_provider())
    });
    let host = Arc::new(host);
    if start {
        host.start_enabled().await;
    }
    assert!(ctx.plugin_host.set(host.clone()).is_ok(), "the host is set once");

    let requests = Requests::default();
    let hooks = Hooks::default();
    let (seen, waiting) = (requests.clone(), hooks.clone());
    let app = build_app(ctx, Some(extra))
        .layer(middleware::from_fn(move |request, next| record(seen.clone(), waiting.clone(), request, next)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("a bound address").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Gateway { port, registry, host, requests, hooks }
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
        "plugins": {
            "jobs": { "config": { "definitions": { "twice-job": {
                "trigger": { "kind": "manual" },
                "action": { "type": "process.exec", "input": { "program": "nosuchprogram", "args": ["--flag", "2"] } },
            } } } },
            "tunnels": { "enabled": true },
        },
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
    assert_eq!(document["summary"]["updated"], json!(1), "{document}");
    assert_eq!(document["summary"]["unchanged"], json!(1), "{document}");
    assert_eq!(item(&document, "plugin", "jobs")["status"], json!("updated"));
    assert_eq!(item(&document, "plugin", "jobs")["executes"]["twice-job"], json!("nosuchprogram --flag 2"));
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("unchanged"));
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
    assert_eq!(document["summary"]["unchanged"], json!(6), "{document}");
    assert_eq!(document["summary"]["created"], json!(0), "{document}");
    assert_eq!(document["summary"]["updated"], json!(0), "{document}");
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

fn a_job(program: &str) -> Value {
    json!({ "trigger": { "kind": "manual" }, "action": { "type": "process.exec", "input": { "program": program } } })
}

// SPEC §host.setup: "`config` is a merge patch over the plugin's row": what it names lands, an array
// replaces, what it does not name stays, and a row never reaches the document.
#[tokio::test]
async fn a_plugin_patch_keeps_the_rest_of_the_row() {
    let gw = gateway().await;
    let seed = json!({
        "maxConcurrentRuns": 7,
        "groups": ["default", "night"],
        "definitions": { "kept": { "title": "a held title", "labels": ["old"], "trigger": { "kind": "manual" },
            "action": { "type": "process.exec", "input": { "program": "nosuchprogram" } } } },
    });
    let (status, answer) = gw_write(gw.port, reqwest::Method::PUT, "/api/plugins/jobs/config", json!({ "config": seed })).await;
    assert_eq!(status, 200, "{answer}");
    let before = gw.writes().len();
    let revision = gw.host.revision();
    let manifest = json!({ "plugins": { "jobs": { "config": {
        "groups": ["other"],
        "definitions": {
            "backup": { "trigger": { "kind": "manual" }, "action": { "type": "process.exec", "input": { "program": "nosuchprogram", "args": ["backup", "."] } } },
            "kept": { "labels": ["new"] },
        },
    } } } });

    let (document, code) = plan_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "jobs")["status"], json!("will-update"));
    assert_eq!(item(&document, "plugin", "jobs")["executes"], json!({ "backup": "nosuchprogram backup ." }));
    assert_eq!(gw.count("PUT", "/api/plugins/jobs/config?dryRun=1"), 1, "{:?}", gw.requests());
    assert_eq!(gw.writes().len(), before, "a plan writes nothing: {:?}", gw.writes());
    assert_eq!(gw.host.revision(), revision);

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "jobs")["status"], json!("updated"));
    let row = gw.host.store().plugin_config("jobs");
    assert_eq!(row["maxConcurrentRuns"], json!(7), "{row}");
    assert_eq!(row["groups"], json!(["other"]), "an array replaces: {row}");
    assert_eq!(row["definitions"]["kept"]["title"], json!("a held title"), "{row}");
    assert_eq!(row["definitions"]["kept"]["labels"], json!(["new"]), "{row}");
    assert_eq!(row["definitions"]["backup"]["action"]["input"]["args"], json!(["backup", "."]), "{row}");
    for held in ["maxConcurrentRuns", "a held title"] {
        assert!(!document.to_string().contains(held), "a row stays out of the document: {document}");
    }

    let wrote = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "jobs")["status"], json!("unchanged"));
    assert_eq!(gw.writes().len(), wrote, "{:?}", gw.writes());
}

/// Store a name in the vault as another writer would, whatever else this process is writing to it.
fn another_writer_stores(name: &str, value: &str) {
    let path = secretstore::secret_store_path();
    let stored = (0..16).any(|_| secretstore::put_secret(&path, name, value, secretstore::vault_rev()).is_ok());
    assert!(stored, "the vault stayed busy");
}

// SPEC §host.setup: "a 409 on a vault `rev` or a plugin `revision` re-reads, re-merges and retries".
// Each race is the real route's own 409, and what the other writer saved stays.
#[tokio::test]
async fn a_conflict_rereads_and_retries() {
    let gw = gateway().await;

    let host = gw.host.clone();
    gw.before_next("/api/plugins/jobs/config", move || {
        let row = json!({ "maxConcurrentRuns": 9, "definitions": { "theirs": a_job("nosuchprogram") } });
        host.store().update_plugin("jobs", host.revision(), row).expect("the other writer saves first");
    });
    let manifest = json!({ "plugins": { "jobs": { "config": { "definitions": { "mine": a_job("nosuchprogram") } } } } });
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(gw.count("PUT", "/api/plugins/jobs/config"), 2, "{:?}", gw.requests());
    let row = gw.host.store().plugin_config("jobs");
    assert_eq!(row["maxConcurrentRuns"], json!(9), "{row}");
    assert!(row["definitions"]["theirs"].is_object(), "{row}");
    assert!(row["definitions"]["mine"].is_object(), "{row}");

    let host = gw.host.clone();
    gw.before_next("/api/plugins/tunnels/disable", move || {
        let row = json!({ "maxConcurrentRuns": 3 });
        host.store().update_plugin("jobs", host.revision(), row).expect("the other writer saves first");
    });
    let manifest = json!({ "plugins": { "tunnels": { "enabled": false } } });
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(gw.count("POST", "/api/plugins/tunnels/disable"), 2, "{:?}", gw.requests());
    assert!(gw.host.store().plugin_disabled("tunnels"));

    let base = scratch_with("value", "mine\n");
    gw.before_next("/api/secrets/race-mine", || another_writer_stores("race-theirs", "theirs"));
    let manifest = json!({ "secrets": { "race-mine": "file:value" } });
    let (document, code) = apply_at(gw.port, &input(manifest, &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert!(gw.count("PUT", "/api/secrets/race-mine") >= 2, "{:?}", gw.requests());
    assert_eq!(secretstore::vault_lookup("race-mine").as_deref(), Some("mine"));
    assert_eq!(secretstore::vault_lookup("race-theirs").as_deref(), Some("theirs"));
}

// SPEC §host.setup: a patch its plugin refuses fails that item, under `plan` as under `apply`, and the
// rest go ahead.
#[tokio::test]
async fn a_refused_patch_stops_only_itself() {
    let gw = gateway().await;
    let manifest = json!({
        "plugins": { "jobs": { "config": { "zz-unknown": 1 } }, "tunnels": { "enabled": false } },
        "mcps": { "after-refusal": { "type": "echo" } },
    });

    let (document, code) = plan_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 1, "{document}");
    let refused = item(&document, "plugin", "jobs");
    assert_eq!(refused["status"], json!("failed"));
    assert_eq!(refused["code"], json!("plugin.invalid"));
    assert!(refused["error"].as_str().expect("an error").contains("zz-unknown"), "{document}");
    assert!(gw.writes().is_empty(), "{:?}", gw.writes());

    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 1, "{document}");
    assert_eq!(item(&document, "plugin", "jobs")["code"], json!("plugin.invalid"));
    assert_eq!(gw.host.store().plugin_config("jobs"), json!({}));
    assert!(gw.count("PUT", "/api/plugins/jobs/config") == 0, "{:?}", gw.requests());
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("updated"));
    assert!(gw.host.store().plugin_disabled("tunnels"));
    assert!(gw.registry.has("after-refusal"));
}

// SPEC §host.setup: an id this gateway does not have is a failed item, and never part of a URL.
#[tokio::test]
async fn an_unknown_plugin_is_named_and_never_asked_for() {
    let gw = gateway().await;
    let manifest = json!({ "plugins": { "job": { "enabled": true, "config": { "a": 1 } }, "tunnels": { "enabled": true } } });
    for run in 0..2 {
        let (document, code) = if run == 0 {
            plan_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await
        } else {
            apply_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await
        };
        assert_eq!(code, 1, "{document}");
        let unknown = item(&document, "plugin", "job");
        assert_eq!(unknown["status"], json!("failed"));
        assert_eq!(unknown["code"], json!("plugin.unknown"));
        assert!(unknown["error"].as_str().expect("an error").contains("did you mean jobs?"), "{document}");
        assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("unchanged"));
    }
    assert!(!gw.requests().iter().any(|(_, target)| target.starts_with("/api/plugins/job/")), "{:?}", gw.requests());
    assert!(gw.writes().is_empty(), "{:?}", gw.writes());
}

// SPEC §host.setup: a plugin turned off stops serving, the row says so, and the same file does nothing
// the second time.
#[tokio::test]
async fn a_plugin_is_turned_off_and_on_once() {
    let gw = gateway().await;
    let off = json!({ "plugins": { "tunnels": { "enabled": false } } });

    let (document, code) = plan_at(gw.port, &input(off.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("will-update"));
    assert!(gw.writes().is_empty(), "{:?}", gw.writes());
    assert_eq!(gw_get(gw.port, "/api/tunnels").await.0, 200);

    let (document, code) = apply_at(gw.port, &input(off.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("updated"));
    assert_eq!(gw_get(gw.port, "/api/tunnels").await.0, 503);

    let wrote = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(off, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("unchanged"));
    assert_eq!(gw.writes().len(), wrote, "{:?}", gw.writes());

    let on = json!({ "plugins": { "tunnels": { "enabled": true } } });
    let (document, code) = apply_at(gw.port, &input(on, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "tunnels")["status"], json!("updated"));
    assert_eq!(gw_get(gw.port, "/api/tunnels").await.0, 200);
}

// SPEC §host.setup: a gateway whose mcp plugin someone turned off fails what needs it, says how to turn
// it on, and leaves the rest of the manifest alone.
#[tokio::test]
async fn mcps_fail_on_a_plugin_someone_turned_off() {
    let gw = gateway_with(json!({ "plugins": { "mcp": { "disabled": true } } })).await;
    let base = scratch_with("value", "off-value\n");
    let manifest = json!({ "secrets": { "off-key": "file:value" }, "mcps": { "off-echo": { "type": "echo" } } });

    let (document, code) = plan_at(gw.port, &input(manifest.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 1, "{document}");
    let waiting = item(&document, "mcp", "off-echo");
    assert_eq!(waiting["status"], json!("failed"));
    assert_eq!(waiting["code"], json!("mcp.plugin-off"));
    assert!(waiting["error"].as_str().expect("an error").contains("plugins.mcp.enabled"), "{document}");

    let (document, code) = apply_at(gw.port, &input(manifest, &base), NO_CHECK).await;
    assert_eq!(code, 1, "{document}");
    assert_eq!(item(&document, "secret", "off-key")["status"], json!("created"));
    assert_eq!(item(&document, "mcp", "off-echo")["code"], json!("mcp.plugin-off"));
    assert!(!gw.registry.has("off-echo"));
    assert_eq!(gw.count("POST", "/api/mcps"), 0, "{:?}", gw.requests());

    let secrets_only = json!({ "secrets": { "off-key": "file:value" } });
    let (document, code) = apply_at(gw.port, &input(secrets_only, &base), NO_CHECK).await;
    assert_eq!(code, 0, "a manifest naming no MCP does not ask for them: {document}");
}

// SPEC §host.setup: "Secrets, plugins (config, then enable, ...), then MCPs": a file can turn
// on the plugin its own MCPs live in, in one run.
#[tokio::test]
async fn a_manifest_turns_on_the_plugin_its_mcps_need() {
    let gw = gateway_with(json!({ "plugins": { "mcp": { "disabled": true } } })).await;
    let manifest = json!({
        "plugins": { "mcp": { "enabled": true } },
        "mcps": { "on-echo": { "type": "echo" } },
    });
    let has_warning = |document: &Value| {
        document["warnings"].as_array().expect("warnings").iter().any(|warning| warning["code"] == json!("mcp.plugin-off"))
    };

    let (document, code) = plan_at(gw.port, &input(manifest.clone(), Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert!(has_warning(&document), "{document}");
    assert_eq!(item(&document, "plugin", "mcp")["status"], json!("will-update"));
    assert_eq!(item(&document, "mcp", "on-echo")["status"], json!("will-create"));
    assert!(gw.writes().is_empty(), "{:?}", gw.writes());

    let (document, code) = apply_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert!(!has_warning(&document), "said and done, so no longer true: {document}");
    assert_eq!(item(&document, "plugin", "mcp")["status"], json!("updated"));
    assert_eq!(item(&document, "mcp", "on-echo")["status"], json!("created"));
    assert_eq!(item(&document, "mcp", "on-echo")["check"]["ok"], json!(true), "{document}");
    assert!(gw.registry.has("on-echo"));

    let wrote = gw.writes().len();
    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["summary"]["unchanged"], json!(2), "{document}");
    assert_eq!(gw.writes().len(), wrote, "{:?}", gw.writes());
}

// SPEC §host.plugins: the plugins start behind the open port, so a command that has just started a
// gateway waits for the boot rather than reading "not started yet" as no gateway.
#[tokio::test]
async fn a_gateway_mid_boot_is_waited_for() {
    let gw = compose(json!({}), false, false).await;
    let manifest = json!({ "mcps": { "early-echo": { "type": "echo" } } });
    assert_eq!(gw_get(gw.port, "/api/mcps").await.0, 503, "nothing has started");
    let host = gw.host.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        host.start_enabled().await;
    });

    let (document, code) = plan_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "mcp", "early-echo")["status"], json!("will-create"));
    assert_eq!(gw_get(gw.port, "/api/mcps").await.0, 200);
}

/// A provider whose listing answers `status`, recording each request's path and bearer.
async fn fake_provider(status: u16) -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |request: Request<Body>| {
        let log = log.clone();
        async move {
            let bearer = request.headers().get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default().to_string();
            log.lock().expect("the provider's log").push((request.uri().path().to_string(), bearer));
            let status = axum::http::StatusCode::from_u16(status).expect("a status");
            (status, axum::Json(json!({ "data": [{ "id": "m1" }, { "id": "m2" }] })))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("a bound address").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, seen)
}

// SPEC §host.setup: a provider is checked by its listing, "Never `providers/test`, which is billed", and
// a failure names the provider without its address or its key.
#[tokio::test]
async fn a_provider_is_checked_by_its_listing() {
    let gw = gateway_with_llm(json!({})).await;
    let (port, seen) = fake_provider(200).await;
    let base = scratch_with("value", "provider-key-value\n");
    let manifest = |url: String| {
        json!({
            "secrets": { "provider-key": "file:value" },
            "plugins": { "llm": { "config": { "providers": { "fake": { "style": "openai", "baseUrl": url, "apiKey": "${secret://provider-key}" } } } } },
        })
    };
    let good = manifest(format!("http://127.0.0.1:{port}/v1"));

    let (document, code) = apply_at(gw.port, &input(good.clone(), &base), NO_CHECK).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "llm")["status"], json!("updated"));
    assert!(item(&document, "plugin", "llm").get("check").is_none(), "{document}");
    assert_eq!(gw.count("POST", "/api/llm/providers/models"), 0, "{:?}", gw.requests());

    let (document, code) = apply_at(gw.port, &input(good, &base), Options::default()).await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(item(&document, "plugin", "llm")["status"], json!("unchanged"));
    assert_eq!(item(&document, "plugin", "llm")["check"], json!({ "ok": true, "detail": "fake: 2 models" }));
    assert_eq!(gw.count("POST", "/api/llm/providers/test"), 0, "{:?}", gw.requests());
    assert_eq!(
        *seen.lock().expect("the provider's log"),
        [("/v1/models".to_string(), "Bearer provider-key-value".to_string())],
        "the vault's value reached the provider and nothing was billed"
    );

    let (port, _seen) = fake_provider(401).await;
    let refusing = manifest(format!("http://127.0.0.1:{port}/v1"));
    let (document, code) = apply_at(gw.port, &input(refusing, &base), Options::default()).await;
    assert_eq!(code, 1, "{document}");
    let llm = item(&document, "plugin", "llm");
    assert_eq!(llm["status"], json!("updated"), "saved, and it does not work: {document}");
    assert_eq!(llm["check"]["ok"], json!(false), "{document}");
    assert_eq!(llm["check"]["code"], json!("check.provider"), "{document}");
    assert!(llm["check"]["error"].as_str().expect("an error").starts_with("fake: "), "{document}");
    let text = document.to_string();
    assert!(!text.contains(&format!("127.0.0.1:{port}")), "{text}");
    assert!(!text.contains("provider-key-value"), "{text}");
}

// SPEC §host.setup: validation is the plugin's, and a reference is not resolved by it, so a plan can
// read a provider whose secret a human has yet to store. What uses that secret waits for it.
#[tokio::test]
async fn a_provider_can_be_planned_before_its_secret_exists() {
    let gw = gateway_with_llm(json!({})).await;
    let manifest = json!({
        "secrets": { "later-key": "ask" },
        "plugins": { "llm": { "config": { "providers": { "glm": {
            "style": "anthropic", "baseUrl": "https://example.invalid/api/anthropic", "apiKey": "${secret://later-key}",
        } } } } },
    });

    let (document, code) = plan_at(gw.port, &input(manifest.clone(), Path::new("")), Options::default()).await;
    assert_eq!(code, 4, "{document}");
    assert_eq!(item(&document, "secret", "later-key")["status"], json!("needs-input"));
    let waiting = item(&document, "plugin", "llm");
    assert_eq!(waiting["status"], json!("blocked"));
    assert_eq!(waiting["by"], json!(["secret:later-key"]));
    assert_eq!(gw.count("PUT", "/api/plugins/llm/config?dryRun=1"), 1, "{:?}", gw.requests());

    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), Options::default()).await;
    assert_eq!(code, 4, "{document}");
    assert_eq!(item(&document, "plugin", "llm")["status"], json!("blocked"));
    assert!(gw.writes().is_empty(), "{:?}", gw.writes());
    assert_eq!(gw.count("POST", "/api/llm/providers/models"), 0, "{:?}", gw.requests());
}

// SPEC §host.setup: asked for on and failed is a failed check, under `--no-check` too, and a rerun retries
// the start.
#[tokio::test]
async fn a_plugin_that_will_not_start_fails_its_check() {
    let gw = gateway_with_llm(json!({ "plugins": { "llm": { "config": { "zz-unknown": 1 } } } })).await;
    assert_eq!(gw.host.plugin("llm").expect("registered").state().as_str(), "failed");
    let manifest = json!({ "plugins": { "llm": { "enabled": true } } });

    let (document, code) = apply_at(gw.port, &input(manifest, Path::new("")), NO_CHECK).await;
    assert_eq!(code, 1, "{document}");
    let llm = item(&document, "plugin", "llm");
    assert_eq!(llm["status"], json!("updated"), "{document}");
    assert_eq!(llm["check"]["code"], json!("plugin.start"), "{document}");
    assert!(llm["check"]["error"].as_str().expect("an error").contains("zz-unknown"), "{document}");
    assert_eq!(gw.count("POST", "/api/plugins/llm/enable"), 1, "{:?}", gw.requests());
}

// SPEC §host.setup: "`setup schema` needs a gateway only for `plugins.<id>`", and the row stays home.
#[tokio::test]
async fn a_plugin_schema_comes_from_the_gateway_and_the_row_does_not() {
    let gw = gateway().await;
    let (status, answer) = gw_write(
        gw.port,
        reqwest::Method::PUT,
        "/api/plugins/jobs/config",
        json!({ "config": { "definitions": { "held-name": a_job("nosuchprogram") } } }),
    )
    .await;
    assert_eq!(status, 200, "{answer}");

    let (document, code) = plugin_schema_at(gw.port, "jobs").await;
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["ok"], json!(true));
    assert_eq!(document["section"], json!("plugins.jobs"));
    assert!(document["schema"]["properties"]["definitions"].is_object(), "{document}");
    assert!(!document.to_string().contains("held-name"), "{document}");

    let seen = gw.requests().len();
    let (document, code) = plugin_schema_at(gw.port, "job").await;
    assert_eq!(code, 2, "{document}");
    assert!(document["error"].as_str().expect("an error").contains("did you mean jobs?"), "{document}");
    assert_eq!(gw.requests()[seen..], [("GET".to_string(), "/api/plugins".to_string())]);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("a bound address").port();
    drop(listener);
    let (document, code) = plugin_schema_at(port, "jobs").await;
    assert_eq!(code, 3, "{document}");
}
