//! Integration tests for the plugin host workstream (docs/09 §10, P1): default-on rows, a
//! disabled plugin never creating its instance, real resource release on stop, single-flight
//! starts, repeated enable/disable, failure isolation, the route guard over every mounted
//! route, config errors, and API compatibility for the trees the host now gates.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

use local_mcp_gateway::adapters::make_adapter;
use local_mcp_gateway::app::build_app;
use local_mcp_gateway::config::ServerDef;
use local_mcp_gateway::config_store::ConfigStore;
use local_mcp_gateway::host::builtin;
use local_mcp_gateway::host::descriptor::{PluginDescriptor, PluginState};
use local_mcp_gateway::host::factory::{PluginFactory, PluginInstance};
use local_mcp_gateway::host::scope::PluginScope;
use local_mcp_gateway::host::PluginHost;
use local_mcp_gateway::jobs::JobSystem;
use local_mcp_gateway::managed::ManagedStore;
use local_mcp_gateway::registry::{Registry, Source};
use local_mcp_gateway::token::single_token_manager;
use local_mcp_gateway::tunnel::manager::TunnelManager;

const TOKEN: &str = "test-token-0123456789abcdef";

// --- a counting fake plugin: the generic factory contract under test -----------------------------

/// What a fake instance does to the world, shared with the test.
#[derive(Default)]
struct Counters {
    creates: AtomicUsize,
    stops: AtomicUsize,
    live: AtomicUsize,
    /// Set by the instance's scoped task when it observes the host's cancel signal.
    task_cleaned: AtomicBool,
}

struct FakeFactory {
    descriptor: PluginDescriptor,
    counters: Arc<Counters>,
    fail_start: AtomicBool,
    start_delay_ms: u64,
}

impl FakeFactory {
    #[allow(dead_code)]
    fn new(id: &str, counters: Arc<Counters>) -> Arc<Self> {
        Arc::new(Self {
            descriptor: PluginDescriptor {
                id: id.into(),
                kind: "fake".into(),
                label: id.into(),
                version: "0.1".into(),
                config_schema_version: 1,
                config_schema: json!({ "type": "object", "properties": {} }),
                pages: vec![],
                routes: vec![format!("/api/{id}")],
                restart_on_config_change: true,
            },
            counters,
            fail_start: AtomicBool::new(false),
            start_delay_ms: 0,
        })
    }
}

struct FakeInstance {
    counters: Arc<Counters>,
    start_delay_ms: u64,
    /// Read from the factory at build time: a later flip only affects the NEXT instance.
    fail_start: bool,
}

impl Drop for FakeInstance {
    fn drop(&mut self) {
        self.counters.live.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl PluginFactory for FakeFactory {
    fn descriptor(&self) -> PluginDescriptor {
        self.descriptor.clone()
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        self.counters.creates.fetch_add(1, Ordering::SeqCst);
        self.counters.live.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeInstance {
            counters: self.counters.clone(),
            start_delay_ms: self.start_delay_ms,
            fail_start: self.fail_start.load(Ordering::SeqCst),
        }))
    }
}

#[async_trait::async_trait]
impl PluginInstance for FakeInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        if self.start_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.start_delay_ms)).await;
        }
        if self.fail_start {
            return Err("boom: the fake plugin was told to fail its start".into());
        }
        let counters = self.counters.clone();
        let rx = scope.cancel_signal();
        if *rx.borrow() {
            return Err("stale scope: this instance was handed an already-cancelled scope".into());
        }
        scope.spawn(async move {
            let mut rx = rx;
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() {
                            counters.task_cleaned.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
            }
        });
        Ok(())
    }

    async fn stop(&self) {
        self.counters.stops.fetch_add(1, Ordering::SeqCst);
    }
}

fn fake_descriptor(id: &str) -> PluginDescriptor {
    PluginDescriptor {
        id: id.into(),
        kind: "fake".into(),
        label: id.into(),
        version: "0.1".into(),
        config_schema_version: 1,
        config_schema: json!({ "type": "object", "properties": {} }),
        pages: vec![],
        routes: vec![format!("/api/{id}")],
        restart_on_config_change: true,
    }
}

// --- host-level lifecycle tests ------------------------------------------------------------------

fn new_host(raw: Value) -> (Arc<PluginHost>, Arc<Counters>) {
    let counters = Arc::new(Counters::default());
    let mut host = PluginHost::new(ConfigStore::memory(raw));
    host.register(Arc::new(FakeFactory {
        descriptor: fake_descriptor("alpha"),
        counters: counters.clone(),
        fail_start: AtomicBool::new(false),
        start_delay_ms: 0,
    }))
    .expect("registers");
    (Arc::new(host), counters)
}

fn host_with(raw: Value, factory: Arc<FakeFactory>) -> Arc<PluginHost> {
    let mut host = PluginHost::new(ConfigStore::memory(raw));
    host.register(factory).expect("registers");
    Arc::new(host)
}

#[tokio::test]
async fn rows_are_default_on_and_a_disabled_row_never_creates_its_instance() {
    // Default-on: an empty config leaves every registered plugin ON (the global rule).
    let (host, counters) = new_host(json!({}));
    host.start_enabled().await;
    let entry = host.plugin("alpha").expect("registered");
    assert_eq!(entry.state(), PluginState::Active);
    assert_eq!(counters.creates.load(Ordering::SeqCst), 1);
    let row = host.row(&entry);
    assert_eq!(row["enabled"], json!(true));
    assert_eq!(row["state"], json!("active"));

    // A literal disabled:true row: the factory is never even asked for an instance.
    let counters2 = Arc::new(Counters::default());
    let factory = Arc::new(FakeFactory {
        descriptor: fake_descriptor("alpha"),
        counters: counters2.clone(),
        fail_start: AtomicBool::new(false),
        start_delay_ms: 0,
    });
    let host2 = host_with(json!({ "alpha": { "disabled": true } }), factory);
    host2.start_enabled().await;
    let entry2 = host2.plugin("alpha").unwrap();
    assert_eq!(entry2.state(), PluginState::Disabled);
    assert_eq!(
        counters2.creates.load(Ordering::SeqCst),
        0,
        "a disabled plugin must not construct its instance"
    );
    assert_eq!(host2.row(&entry2)["enabled"], json!(false));
}

#[tokio::test]
async fn disable_really_releases_the_instance_and_its_scoped_tasks() {
    let (host, counters) = new_host(json!({}));
    host.start_enabled().await;
    assert_eq!(counters.live.load(Ordering::SeqCst), 1);

    let revision = host.revision();
    host.store()
        .set_plugin_disabled("alpha", revision, true)
        .expect("persist the disable");
    host.reconcile("alpha").await.expect("stops");

    // The instance's own release ran, the instance is DROPPED (not parked behind a flag),
    // and the task it spawned through its scope observed the cancel signal and exited.
    assert_eq!(counters.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        counters.live.load(Ordering::SeqCst),
        0,
        "the dropped instance must not be retained by the host"
    );
    assert!(
        counters.task_cleaned.load(Ordering::SeqCst),
        "the scoped task must be cancelled by the stop"
    );
    let entry = host.plugin("alpha").unwrap();
    assert_eq!(entry.state(), PluginState::Disabled);
}

#[tokio::test]
async fn concurrent_starts_are_single_flighted() {
    let counters = Arc::new(Counters::default());
    let factory = Arc::new(FakeFactory {
        descriptor: fake_descriptor("alpha"),
        counters: counters.clone(),
        fail_start: AtomicBool::new(false),
        // Two reconciles race; create() runs under the op queue, so the delay widens the
        // window the second caller would have to double-build in if the host were racy.
        start_delay_ms: 50,
    });
    let host = host_with(json!({}), factory);

    let (a, b) = tokio::join!(host.reconcile("alpha"), host.reconcile("alpha"));
    a.expect("first start");
    b.expect("second start");
    assert_eq!(
        counters.creates.load(Ordering::SeqCst),
        1,
        "a second concurrent start must join the first, never build a twin"
    );
    assert_eq!(host.plugin("alpha").unwrap().state(), PluginState::Active);
}

#[tokio::test]
async fn enable_disable_round_trips_are_repeatable() {
    let (host, counters) = new_host(json!({}));
    host.start_enabled().await;
    for round in 0..3u64 {
        let snapshot = host.store().snapshot();
        host.store()
            .set_plugin_disabled("alpha", snapshot.revision, true)
            .expect("disable persists");
        host.reconcile("alpha").await.expect("stops");
        assert_eq!(counters.live.load(Ordering::SeqCst), 0, "round {round}");

        let snapshot = host.store().snapshot();
        host.store()
            .set_plugin_disabled("alpha", snapshot.revision, false)
            .expect("enable persists");
        host.reconcile("alpha").await.expect("restarts");
        assert_eq!(
            counters.creates.load(Ordering::SeqCst),
            round as usize + 2,
            "each enable builds a FRESH instance"
        );
        assert_eq!(host.plugin("alpha").unwrap().state(), PluginState::Active);
    }
}

#[tokio::test]
async fn one_plugin_failing_is_a_row_never_a_failed_boot() {
    let counters_bad = Arc::new(Counters::default());
    let counters_good = Arc::new(Counters::default());
    let mut host = PluginHost::new(ConfigStore::memory(json!({})));
    host.register(Arc::new(FakeFactory {
        descriptor: fake_descriptor("broken"),
        counters: counters_bad.clone(),
        fail_start: AtomicBool::new(true),
        start_delay_ms: 0,
    }))
    .expect("registers");
    host.register(Arc::new(FakeFactory {
        descriptor: fake_descriptor("works"),
        counters: counters_good.clone(),
        fail_start: AtomicBool::new(false),
        start_delay_ms: 0,
    }))
    .expect("registers");
    let host = Arc::new(host);

    host.start_enabled().await;

    // The failure is explicit: state failed, lastError present in the inventory row.
    let broken = host.plugin("broken").unwrap();
    assert_eq!(broken.state(), PluginState::Failed);
    let row = host.row(&broken);
    assert_eq!(row["state"], json!("failed"));
    assert_eq!(row["enabled"], json!(true), "desired state still stands");
    assert!(
        row["lastError"]
            .as_str()
            .unwrap_or_default()
            .contains("boom"),
        "the failure names itself: {row}"
    );

    // ...and the healthy plugin next to it came up anyway.
    assert_eq!(host.plugin("works").unwrap().state(), PluginState::Active);
    assert_eq!(counters_good.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_plugin_can_be_retried_by_enabling_again() {
    let counters = Arc::new(Counters::default());
    let factory = Arc::new(FakeFactory {
        descriptor: fake_descriptor("alpha"),
        counters: counters.clone(),
        fail_start: AtomicBool::new(true),
        start_delay_ms: 0,
    });
    let host = host_with(json!({}), factory.clone());
    host.start_enabled().await;
    assert_eq!(host.plugin("alpha").unwrap().state(), PluginState::Failed);

    // The condition clears (config fixed, dependency back); one enable retries the start.
    factory.fail_start.store(false, Ordering::SeqCst);
    host.reconcile("alpha").await.expect("retry");
    assert_eq!(host.plugin("alpha").unwrap().state(), PluginState::Active);
    assert_eq!(counters.creates.load(Ordering::SeqCst), 2, "a fresh build");
    let row = host.row(&host.plugin("alpha").unwrap());
    assert!(
        row.get("lastError").is_none(),
        "a recovered plugin carries no stale error: {row}"
    );
}

#[tokio::test]
async fn config_driven_restart_only_for_plugins_that_declare_it() {
    let counters = Arc::new(Counters::default());
    let factory = Arc::new(FakeFactory {
        descriptor: fake_descriptor("alpha"),
        counters: counters.clone(),
        fail_start: AtomicBool::new(false),
        start_delay_ms: 0,
    });
    let host = host_with(json!({}), factory);
    host.start_enabled().await;
    let before = host.plugin("alpha").unwrap().config_revision();

    let snapshot = host.store()
        .update_plugin("alpha", host.revision(), json!({ "tuned": true }))
        .expect("config update");
    host.reconcile("alpha").await.expect("reconciles");

    // restart_on_config_change: the instance is recycled on the new config.
    assert_eq!(counters.stops.load(Ordering::SeqCst), 1);
    assert_eq!(counters.creates.load(Ordering::SeqCst), 2);
    assert_eq!(host.plugin("alpha").unwrap().config_revision(), snapshot.revision);
    assert_ne!(snapshot.revision, before, "the row moved the revision");

    // ...and a plugin that does NOT declare it keeps the instance it has. The revision still
    // advances to the current one — the host records what the row says, it just does not
    // bounce a plugin that never asked to be bounced (docs/09 §4: apply vs restart is an
    // explicit per-plugin decision, never a universal hot reload).
    let counters = Arc::new(Counters::default());
    let mut descriptor = fake_descriptor("beta");
    descriptor.restart_on_config_change = false;
    let host = host_with(
        json!({}),
        Arc::new(FakeFactory {
            descriptor,
            counters: counters.clone(),
            fail_start: AtomicBool::new(false),
            start_delay_ms: 0,
        }),
    );
    host.start_enabled().await;
    let snapshot = host
        .store()
        .update_plugin("beta", host.revision(), json!({ "tuned": true }))
        .expect("config update");
    host.reconcile("beta").await.expect("reconciles");
    assert_eq!(counters.stops.load(Ordering::SeqCst), 0, "no bounce");
    assert_eq!(counters.creates.load(Ordering::SeqCst), 1, "the same instance");
    assert_eq!(host.plugin("beta").unwrap().config_revision(), snapshot.revision);
}

// --- the full composition: the real app with the host booted --------------------------------------

/// Pin this test binary to one deterministic master key, so the sealed state files never
/// reach DPAPI or the machine id. The key module reads the variable fresh on every seal, so
/// installing it once here covers every later store in the process.
fn test_master_key() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // Safety: one write, under a OnceLock, of a variable nothing in this binary caches.
        unsafe { std::env::set_var(local_mcp_gateway::secure::key::MASTER_KEY_ENV, "ab".repeat(32)) }
    });
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    // The sealed state files (managed.json, jobs.json, tunnels.json) need a test master key,
    // and every write must stay inside this test's own directory.
    test_master_key();
    let dir = std::env::temp_dir().join(format!("lmg-plugins-{tag}-{}",
        local_mcp_gateway::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn echo_def() -> ServerDef {
    ServerDef(json!({ "type": "echo" }).as_object().cloned().unwrap())
}

/// The boot sequence in miniature — the exact composition server.rs runs, minus the port:
/// one echo MCP registered (NOT started: that is the mcp plugin's job now), tunnels and jobs
/// opened unconditionally and wrapped by the built-in plugins, the host booted, the boundary
/// layered over every merged tree.
async fn full_app(tag: &str, raw: Value) -> (axum::Router, Arc<PluginHost>, Arc<Registry>) {
    let dir = scratch_dir(tag);
    let registry = Registry::new(3_600_000);
    let managed = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let adapter = make_adapter(&echo_def(), "echo").expect("echo adapter");
    registry
        .register("echo", Source::Config, echo_def(), adapter)
        .expect("register echo");

    let services = local_mcp_gateway::services::RuntimeServices::new();
    let jobs = JobSystem::open(dir.join("jobs.json"), services.clone());
    let tunnel_store = Arc::new(Mutex::new(local_mcp_gateway::tunnel::TunnelStore::new(
        dir.join("tunnels.json"),
        19998,
    )));
    let tunnel_manager = TunnelManager::new(tunnel_store.clone(), None);
    let tunnels = Arc::new(local_mcp_gateway::tunnel::api::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        registry: Some(registry.clone()),
    });

    let ctx = local_mcp_gateway::app::AppContext::new(
        registry.clone(),
        Arc::new(single_token_manager(TOKEN)),
        managed.clone(),
        "MCP_GATEWAY_TOKEN",
        19998,
    );
    let mut host = PluginHost::new(ConfigStore::memory(raw));
    let deps = builtin::BuiltinDeps {
        registry: registry.clone(),
        managed: managed.clone(),
        jobs: jobs.clone(),
        tunnels: tunnels.clone(),
        tunnel_manager: tunnel_manager.clone(),
        browse: ctx.browser_resolver(),
        services: services.clone(),
    };
    builtin::register_all(&mut host, &deps)
        .expect("the built-ins register without conflicts");
    let host = Arc::new(host);
    host.start_enabled().await;
    assert!(ctx.plugin_host.set(host.clone()).is_ok(), "host set once");

    let extra = local_mcp_gateway::tunnel::api::mount(tunnels)
        .merge(local_mcp_gateway::jobs::api::mount(jobs))
        .merge(local_mcp_gateway::services::api::mount(services));
    let app = build_app(ctx, Some(extra));
    (app, host, registry)
}

async fn send(
    app: &axum::Router,
    req: Request<Body>,
) -> (StatusCode, Option<Value>, String) {
    let response = app.clone().oneshot(req).await.expect("router answers");
    let status = response.status();
    let text = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("body"),
    )
    .into_owned();
    let json = serde_json::from_str::<Value>(&text).ok();
    (status, json, text)
}

/// A local request: loopback Host, bearer when the route is token-gated.
fn local(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap())
        .body(Body::empty())
        .expect("a request")
}

fn json_body(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("a request")
}

#[tokio::test]
async fn inventory_shape_is_exact_and_the_mcp_plugin_started_the_mcps() {
    let (app, host, registry) = full_app("shape", json!({})).await;

    // The plugin did the boot-start the old register_one did: the echo MCP is up.
    let lifecycle = registry
        .get("echo")
        .unwrap()
        .data
        .read()
        .unwrap()
        .lifecycle;
    assert_eq!(
        local_mcp_gateway::registry::Lifecycle::Started,
        lifecycle,
        "the mcp plugin boots eligible MCPs"
    );

    let (status, body, _) = send(&app, local("GET", "/api/plugins")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("JSON");
    let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["pages", "plugins", "revision"]);

    let ids: Vec<&str> = body["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    // The process plugin is in the inventory but contributes no page: a capability provider
    // is managed like any other plugin, it just has nothing to navigate to.
    assert_eq!(ids, vec!["mcp", "tunnels", "data", "jobs", "process"]);
    for plugin in body["plugins"].as_array().unwrap() {
        assert_eq!(plugin["enabled"], json!(true), "{plugin}");
        assert_eq!(plugin["state"], json!("active"), "{plugin}");
        assert_eq!(plugin["configRevision"], body["revision"], "{plugin}");
        assert!(plugin.get("lastError").is_none(), "absent when clean: {plugin}");
    }

    // Pages in registration order; the fixed view table of this build.
    let pages: Vec<(String, String, String, bool, i64)> = body["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["pluginId"].as_str().unwrap().to_string(),
                p["path"].as_str().unwrap().to_string(),
                p["sidebar"].as_bool().unwrap(),
                p["order"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pages,
        vec![
            ("mcps".into(), "mcp".into(), "#mcps".into(), true, 10),
            ("traffic".into(), "mcp".into(), "#traffic".into(), false, 20),
            ("tunnels".into(), "tunnels".into(), "#tunnels".into(), false, 30),
            ("data".into(), "data".into(), "#data".into(), false, 40),
            ("jobs".into(), "jobs".into(), "#jobs".into(), false, 50),
        ],
    );
    let entries: Vec<String> = body["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["entry"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        entries,
        vec![
            "/admin/js/views/mcps.js",
            "/admin/js/views/traffic.js",
            "/admin/js/views/tunnels.js",
            "/admin/js/views/data.js",
            "/admin/js/views/jobs.js",
        ]
    );

    // The host knows its own revision straight from the store.
    assert_eq!(body["revision"], json!(host.revision()));
}

#[tokio::test]
async fn boot_disabled_plugins_guard_every_route_they_own() {
    let (app, _host, registry) = full_app(
        "guarded",
        json!({
            "mcp": { "disabled": true },
            "tunnels": { "disabled": true },
            "data": { "disabled": true },
            "jobs": { "disabled": true },
        }),
    )
    .await;

    // Nothing started: the registry is populated (routes stable) but idle.
    let lifecycle = registry
        .get("echo")
        .unwrap()
        .data
        .read()
        .unwrap()
        .lifecycle;
    assert_ne!(
        local_mcp_gateway::registry::Lifecycle::Started,
        lifecycle,
        "a disabled mcp plugin must not boot MCPs"
    );

    // EVERY mounted route of every disabled plugin answers the same structured 503, naming
    // the plugin and its config row — deep paths and every method included.
    for (method, uri, plugin) in [
        ("GET", "/api/mcps", "mcp"),
        ("GET", "/api/mcps/echo/details", "mcp"),
        ("POST", "/api/mcps/echo/start", "mcp"),
        ("GET", "/api/traffic", "mcp"),
        ("GET", "/api/tunnels", "tunnels"),
        ("GET", "/api/tunnels/some-conn/rules", "tunnels"),
        ("POST", "/api/tunnels/connections", "tunnels"),
        ("GET", "/api/db", "data"),
        ("GET", "/api/db/conn/table/rows?table=t", "data"),
        ("GET", "/api/jobs", "jobs"),
        ("PUT", "/api/jobs/nightly", "jobs"),
        ("POST", "/api/jobs/nightly/run", "jobs"),
        ("GET", "/api/jobs/nightly/runs", "jobs"),
    ] {
        let (status, body, _) = send(&app, local(method, uri)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}");
        let msg = body.expect("JSON")["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(msg.contains(plugin), "{method} {uri} names {plugin}: {msg}");
        assert!(msg.contains("disabled"), "{method} {uri}: {msg}");
    }

    // The client catch-all is guarded by its handler (no static prefix to match): a bearer
    // client POSTing an MCP path gets the plugin 503, and no lazy proc spawns behind it.
    let (status, body, _) = send(
        &app,
        json_body("POST", "/echo", json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("mcp plugin is disabled"),
    );

    // Host-owned routes are untouched, and the new management surface is reachable.
    for uri in ["/health", "/api/plugins"] {
        let (status, _, _) = send(&app, local("GET", uri)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }

    // ...and the loopback boundary still answers BEFORE any plugin state: the /api/plugins
    // routes are behind the same guard as everything else.
    let req = Request::get("/api/plugins")
        .header(header::HOST, "evil.test:19999")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn enable_disable_over_the_api_is_live_and_repeatable() {
    let (app, _host, _registry) = full_app("toggle", json!({})).await;

    // Enabled at boot: the jobs API answers its normal shape.
    let (status, body, _) = send(&app, local("GET", "/api/jobs")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.unwrap().get("jobs").is_some(), "the jobs shape is unchanged");

    for round in 0..2 {
        // Disable: persisted (the row moves), reconciled (the API goes away) — same answer
        // shape subsystems.rs used to give for boot-time absence.
        let (status, body, _) = send(&app, local("POST", "/api/plugins/jobs/disable")).await;
        assert_eq!(status, StatusCode::OK, "round {round}");
        let body = body.expect("JSON");
        assert_eq!(body["plugin"]["enabled"], json!(false));
        assert_eq!(body["plugin"]["state"], json!("disabled"));
        let (status, body, _) = send(&app, local("GET", "/api/jobs")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("jobs plugin is disabled"));

        // Enable: the SAME routes come back serving, no restart anywhere.
        let (status, body, _) = send(&app, local("POST", "/api/plugins/jobs/enable")).await;
        assert_eq!(status, StatusCode::OK, "round {round}");
        assert_eq!(body.expect("JSON")["plugin"]["state"], json!("active"));
        let (status, body, _) = send(&app, local("GET", "/api/jobs")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.unwrap().get("jobs").is_some());
    }
}

#[tokio::test]
async fn unknown_plugins_and_stale_revisions_are_refused() {
    let (app, _host, _registry) = full_app("cas", json!({})).await;

    for uri in [
        "/api/plugins/ghost/enable",
        "/api/plugins/ghost/disable",
        "/api/plugins/ghost/config",
    ] {
        let (status, _, _) = send(&app, local("POST", uri)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }

    // A stale revision is a conflict, not a silent overwrite (the store's CAS).
    let (_, body, _) = send(&app, local("GET", "/api/plugins")).await;
    let stale = body.unwrap()["revision"].as_u64().unwrap();
    let (_, body, _) = send(
        &app,
        json_body("PUT", "/api/plugins/jobs/config", json!({ "config": {} })),
    )
    .await;
    let fresh = body.unwrap()["revision"].as_u64().unwrap();
    assert_ne!(fresh, stale);
    let (status, body, _) = send(
        &app,
        json_body(
            "PUT",
            "/api/plugins/jobs/config",
            json!({ "revision": stale, "config": { "late": true } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("configuration changed"));
    let (status, _, _) = send(
        &app,
        json_body("POST", "/api/plugins/jobs/disable", json!({ "revision": stale })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn config_api_validates_before_persisting_and_serves_the_schema() {
    let (app, _host, _registry) = full_app("config", json!({})).await;

    // GET: revision + config + schema, one shape.
    let (status, body, _) = send(&app, local("GET", "/api/plugins/jobs/config")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("JSON");
    let keys: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, vec!["revision", "config", "schema"]);
    assert_eq!(body["config"], json!({}));
    assert_eq!(body["schema"]["type"], json!("object"));

    // PUT a value the plugin's validator rejects: nothing is persisted, the revision holds.
    let before = body["revision"].as_u64().unwrap();
    let (status, body, _) = send(
        &app,
        json_body(
            "PUT",
            "/api/plugins/jobs/config",
            json!({ "config": { "definitions": { "a": "not an object" } } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("definitions.a"));
    let (_, body, _) = send(&app, local("GET", "/api/plugins/jobs/config")).await;
    assert_eq!(body.unwrap()["revision"], json!(before), "nothing persisted");

    // A valid PUT persists, bumps the revision, and echoes the round-trip.
    let (status, body, _) = send(
        &app,
        json_body(
            "PUT",
            "/api/plugins/jobs/config",
            json!({ "config": { "definitions": { "nightly": { "command": "x" } } } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("JSON");
    assert_ne!(body["revision"], json!(before));
    assert_eq!(
        body["config"]["definitions"]["nightly"]["command"],
        json!("x"),
    );
    assert_eq!(body["schema"]["type"], json!("object"));

    // And a non-object config is refused by the store itself (shape gate).
    let (status, _, _) = send(
        &app,
        json_body("PUT", "/api/plugins/data/config", json!({ "config": [1, 2] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn enabled_plugins_keep_every_api_shape_they_had() {
    let (app, _host, _registry) = full_app("compat", json!({})).await;

    // The trees the host now gates answer exactly as before when their plugin serves —
    // the boundary is invisible to an enabled plugin's clients.
    let (status, body, _) = send(&app, local("GET", "/api/jobs")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.unwrap().get("jobs").is_some());

    let (status, _, _) = send(&app, local("GET", "/api/tunnels")).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = send(&app, local("GET", "/api/db")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.unwrap().get("connections").is_some(), "the /api/db shape");

    let (status, body, _) = send(&app, local("GET", "/api/mcps")).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.expect("JSON");
    assert!(
        rows["mcps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == json!("echo") && m["lifecycle"] == json!("started")),
        "echo is listed and started: {rows}",
    );

    // The MCP endpoint itself still serves (bearer, JSON-RPC) — the plugin boundary passed
    // it through because the mcp plugin is active.
    let (status, _, _) = send(
        &app,
        json_body(
            "POST",
            "/echo",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
        ),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an active plugin's client path is never 503",
    );
    assert_ne!(status, StatusCode::UNAUTHORIZED, "the bearer was valid");
}

#[test]
fn route_ownership_is_longest_prefix_at_segment_boundaries() {
    let dir = scratch_dir("owner");
    let _ = dir;
    let mut host = PluginHost::new(ConfigStore::memory(json!({})));
    builtin::register_all(
        &mut host,
        &builtin::BuiltinDeps {
            registry: Registry::new(60_000),
            managed: Arc::new(ManagedStore::open_at(dir.join("managed.json"))),
            jobs: JobSystem::open(
                dir.join("jobs.json"),
                local_mcp_gateway::services::RuntimeServices::new(),
            ),
            tunnels: Arc::new(local_mcp_gateway::tunnel::api::Tunnels {
                store: Arc::new(Mutex::new(local_mcp_gateway::tunnel::TunnelStore::new(
                    dir.join("tunnels.json"),
                    19998,
                ))),
                manager: TunnelManager::new(
                    Arc::new(Mutex::new(local_mcp_gateway::tunnel::TunnelStore::new(
                        dir.join("tunnels2.json"),
                        19998,
                    ))),
                    None,
                ),
                registry: None,
            }),
            tunnel_manager: TunnelManager::new(
                Arc::new(Mutex::new(local_mcp_gateway::tunnel::TunnelStore::new(
                    dir.join("tunnels3.json"),
                    19998,
                ))),
                None,
            ),
            browse: Arc::new(Vec::new),
            services: local_mcp_gateway::services::RuntimeServices::new(),
        },
    )
    .expect("built-ins register");
    let host = host;
    assert_eq!(host.route_owner("/api/jobs"), Some("jobs".into()));
    assert_eq!(host.route_owner("/api/jobs/x/run"), Some("jobs".into()));
    assert_eq!(host.route_owner("/api/jobsx"), None, "segment boundary");
    assert_eq!(host.route_owner("/api/traffic"), Some("mcp".into()));
    assert_eq!(host.route_owner("/api/mcps/a/calls"), Some("mcp".into()));
    assert_eq!(host.route_owner("/api/db"), Some("data".into()));
    assert_eq!(host.route_owner("/api/plugins"), None, "host-owned");
    assert_eq!(host.route_owner("/health"), None);
}
// --- the shared execution surface: /api/actions and /api/runs (docs/10 §7) -----------------

/// An echo through whichever process capability this platform can spell.
fn echo_input(text: &str) -> Value {
    if cfg!(windows) {
        json!({ "program": "cmd", "args": ["/c", "echo", text] })
    } else {
        json!({ "program": "sh", "args": ["-c", format!("echo {text}")] })
    }
}

/// Poll a run to its terminal state, the way the panel does. Fails loudly rather than hanging
/// the suite: a run that never finishes is the bug this whole layer exists to prevent.
async fn await_run(app: &axum::Router, run_id: u64) -> Value {
    for _ in 0..600 {
        let (status, body, text) = send(app, local("GET", &format!("/api/runs/{run_id}"))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let view = body.expect("JSON run view");
        let state = view["state"].as_str().unwrap_or_default().to_string();
        if !matches!(state.as_str(), "queued" | "running") {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {run_id} never reached a terminal state");
}

#[tokio::test]
async fn the_execution_surface_lists_capabilities_and_runs_them() {
    let (app, _host, _registry) = full_app("actions", json!({})).await;

    let (status, body, text) = send(&app, local("GET", "/api/actions")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let actions = body.expect("JSON")["actions"].as_array().cloned().unwrap();
    let types: Vec<&str> = actions.iter().map(|a| a["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        vec!["process.exec", "process.legacy-command"],
        "the capabilities of this build, contributed by the process plugin",
    );
    for action in &actions {
        assert_eq!(action["provider"], json!("process"), "{action}");
        assert_eq!(action["cancelable"], json!(true), "{action}");
        assert!(
            action["schema"]["properties"].is_object(),
            "a capability publishes the schema a form is written against: {action}"
        );
        assert!(action["title"].as_str().unwrap_or_default().len() > 4, "{action}");
    }

    // A submission is answered NOW with a runId; the work outlives the request.
    let (status, body, text) = send(
        &app,
        json_body(
            "POST",
            "/api/runs",
            json!({ "action": "process.exec", "label": "greeting", "input": echo_input("hi-from-run") }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let body = body.expect("JSON");
    let run_id = body["runId"].as_u64().expect("a runId");
    assert_eq!(body["run"]["label"], json!("greeting"));
    assert_eq!(body["run"]["owner"], json!("manual"));

    let view = await_run(&app, run_id).await;
    assert_eq!(view["state"], json!("succeeded"), "{view}");
    assert_eq!(view["exitCode"], json!(0), "{view}");
    assert!(
        view["output"].as_str().unwrap_or_default().contains("hi-from-run"),
        "the captured tail is readable: {view}"
    );
    assert!(view["ms"].as_u64().is_some(), "{view}");

    // ?output=0 is the poller's read: state without the tail.
    let (status, body, _) = send(&app, local("GET", &format!("/api/runs/{run_id}?output=0"))).await;
    assert_eq!(status, StatusCode::OK);
    let lean = body.expect("JSON");
    assert_eq!(lean["state"], json!("succeeded"));
    assert!(lean.get("output").is_none(), "the tail is opt-out: {lean}");

    // The list carries the pool's bounds next to the rows — a panel can show the ceiling it
    // will be refused at before it submits.
    let (status, body, _) = send(&app, local("GET", "/api/runs")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("JSON");
    let ids: Vec<u64> = body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["runId"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![run_id]);
    assert!(body["capacity"]["maxConcurrentRuns"].as_u64().unwrap() >= 1);
    assert!(body["capacity"]["maxQueuedRuns"].as_u64().is_some());
}

#[tokio::test]
async fn a_submission_refusal_is_typed_and_leaves_no_run_behind() {
    let (app, _host, _registry) = full_app("refusals", json!({})).await;

    let cases: Vec<(Value, StatusCode, &str)> = vec![
        (json!({}), StatusCode::BAD_REQUEST, "action is required"),
        (
            json!({ "action": "process.nope", "input": {} }),
            StatusCode::BAD_REQUEST,
            "/api/actions",
        ),
        (
            json!({ "action": "process.exec", "input": echo_input("x"), "timeoutMs": 0 }),
            StatusCode::BAD_REQUEST,
            "timeoutMs",
        ),
    ];
    for (body, expected, needle) in cases {
        let (status, json, text) = send(&app, json_body("POST", "/api/runs", body.clone())).await;
        assert_eq!(status, expected, "{body} -> {text}");
        let message = json.expect("JSON")["error"].as_str().unwrap_or_default().to_string();
        assert!(message.contains(needle), "{body} -> {message}");
    }

    let (_, body, _) = send(&app, local("GET", "/api/runs")).await;
    assert!(
        body.expect("JSON")["runs"].as_array().unwrap().is_empty(),
        "a refused submission never becomes a row"
    );

    for uri in ["/api/runs/12345", "/api/runs/not-a-number"] {
        let (status, body, text) = send(&app, local("GET", uri)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} -> {text}");
        assert!(body.expect("JSON")["error"].as_str().unwrap().contains("no run"));
    }
    let (status, _, _) = send(&app, local("POST", "/api/runs/12345/cancel")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cancelling nothing is not a success");
}

#[tokio::test]
async fn disabling_the_process_plugin_withdraws_its_capabilities_but_keeps_the_history() {
    let (app, _host, _registry) = full_app("withdraw", json!({})).await;

    let (_, body, _) = send(
        &app,
        json_body(
            "POST",
            "/api/runs",
            json!({ "action": "process.exec", "input": echo_input("before-disable") }),
        ),
    )
    .await;
    let run_id = body.expect("JSON")["runId"].as_u64().expect("a runId");
    let view = await_run(&app, run_id).await;
    assert_eq!(view["state"], json!("succeeded"), "{view}");

    let (status, _, text) = send(&app, local("POST", "/api/plugins/process/disable")).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // The capability list is the LIVE truth, not a compiled-in catalogue...
    let (status, body, _) = send(&app, local("GET", "/api/actions")).await;
    assert_eq!(status, StatusCode::OK, "the surface itself is host-owned and stays up");
    assert!(
        body.expect("JSON")["actions"].as_array().unwrap().is_empty(),
        "a stopped provider's capabilities leave the listing"
    );

    // ...so a submission against a withdrawn capability is refused, not silently dropped.
    let (status, body, _) = send(
        &app,
        json_body(
            "POST",
            "/api/runs",
            json!({ "action": "process.exec", "input": echo_input("after-disable") }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.expect("JSON")["error"]
        .as_str()
        .unwrap()
        .contains("process.exec"));

    // The history of runs whose provider has since been disabled stays readable in full.
    let (status, body, _) = send(&app, local("GET", &format!("/api/runs/{run_id}"))).await;
    assert_eq!(status, StatusCode::OK);
    let view = body.expect("JSON");
    assert_eq!(view["state"], json!("succeeded"));
    assert!(view["output"].as_str().unwrap().contains("before-disable"));

    // Enabling brings the same capabilities back, and the pool accepts work again.
    let (status, _, text) = send(&app, local("POST", "/api/plugins/process/enable")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (_, body, _) = send(&app, local("GET", "/api/actions")).await;
    let types: Vec<String> = body.expect("JSON")["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(types, vec!["process.exec", "process.legacy-command"]);

    let (status, body, _) = send(
        &app,
        json_body(
            "POST",
            "/api/runs",
            json!({ "action": "process.legacy-command", "input": { "command": legacy_echo("after-enable") } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = body.expect("JSON")["runId"].as_u64().expect("a runId");
    let view = await_run(&app, run_id).await;
    assert_eq!(view["state"], json!("succeeded"), "{view}");
    assert!(view["output"].as_str().unwrap().contains("after-enable"), "{view}");
}

/// The same echo, spelled as the pre-v2 "command" string the legacy capability tokenizes.
fn legacy_echo(text: &str) -> String {
    if cfg!(windows) {
        format!("cmd /c echo {text}")
    } else {
        format!("sh -c \"echo {text}\"")
    }
}

#[tokio::test]
async fn a_long_run_is_cancelable_over_the_api_and_reaped_before_the_answer() {
    let (app, _host, _registry) = full_app("cancel", json!({})).await;
    let input = if cfg!(windows) {
        // A sleep that needs no shell built-in: ping's interval is the portable Windows wait.
        json!({ "program": "cmd", "args": ["/c", "ping", "-n", "60", "127.0.0.1"] })
    } else {
        json!({ "program": "sh", "args": ["-c", "sleep 60"] })
    };
    let (status, body, text) = send(
        &app,
        json_body(
            "POST",
            "/api/runs",
            json!({ "action": "process.exec", "label": "slow", "input": input }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let run_id = body.expect("JSON")["runId"].as_u64().expect("a runId");

    // Wait until it is actually running, so the cancel races the child and not the spawn.
    for _ in 0..300 {
        let (_, body, _) = send(&app, local("GET", &format!("/api/runs/{run_id}?output=0"))).await;
        if body.expect("JSON")["state"] == json!("running") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (status, body, text) = send(&app, local("POST", &format!("/api/runs/{run_id}/cancel"))).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let view = body.expect("JSON");
    assert_eq!(
        view["state"],
        json!("canceled"),
        "cancel WAITS: when it answers, the run is terminal and its child reaped: {view}"
    );

    // First-wins: a second cancel reports the same terminal view rather than an error.
    let (status, body, _) = send(&app, local("POST", &format!("/api/runs/{run_id}/cancel"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("JSON")["state"], json!("canceled"));
}

/// A page contribution is a PROMISE that a module exists at that URL. The panel discovers pages
/// from this inventory and dynamically imports their entry; a descriptor pointing at a file the
/// build does not embed is a tab that can only fail when clicked.
#[tokio::test]
async fn every_contributed_page_entry_is_actually_served() {
    let (app, _host, _registry) = full_app("entries", json!({})).await;

    let (_, body, _) = send(&app, local("GET", "/api/plugins")).await;
    let body = body.expect("JSON");
    let mut entries: Vec<String> = body["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["entry"].as_str().unwrap().to_string())
        .collect();
    assert!(!entries.is_empty(), "this build contributes pages");
    // The host contributes no management page of its own: page-registry.js appends the Plugins
    // tab whenever /api/plugins answers, so a plugin-aware gateway must serve this one too.
    entries.push("/admin/js/views/plugins.js".into());

    for entry in entries {
        let (status, _, _) = send(&app, local("GET", &entry)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a page descriptor points at {entry}, which this build does not serve"
        );
    }

    // The management view draws rows from these fields; an id is not a label.
    for plugin in body["plugins"].as_array().unwrap() {
        assert!(plugin["label"].as_str().unwrap_or_default().len() > 1, "{plugin}");
        assert!(plugin["version"].as_str().is_some(), "{plugin}");
        assert!(plugin["kind"].as_str().is_some(), "{plugin}");
    }
}
