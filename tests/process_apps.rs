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

//! The Process page's backend, end to end through the real app (SPEC §process.apps):
//! the inventory row with its page, the 202/start/outcome/history flow, the whole-save
//! refusal of an invalid PUT, the log route reading only the configured files, a run
//! child stopped through the coordinator's hook, and the disable-is-not-a-kill rule.
//!
//! The app-level flows use REAL processes where a process is the point (a spawn that
//! cannot happen, a child that must outlive a disable) and real time in small doses:
//! the start window's settle is 2 s and the tests poll like the panel does.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;

use swiss::app::build_app;
use swiss::builtin;
use swiss_host::config_store::ConfigStore;
use swiss_host::host::PluginHost;
use swiss_host::managed::ManagedStore;
use swiss_host::token::single_token_manager;
use swiss_jobs::jobs::JobSystem;
use swiss_mcp::registry::Registry;
use swiss_tunnels::tunnel::manager::TunnelManager;

const TOKEN: &str = "test-token-0123456789abcdef";

/// The tests here read and write the shared scratch home (the record file, the
/// history, the captures), so the whole file serializes on the same DATA_DIR_LOCK
/// the composition tests use - one home serves the whole binary.
async fn seat() -> tokio::sync::MutexGuard<'static, ()> {
    let _ = swiss_core::paths::test_home();
    // The lock is a static, so its guard is 'static: the caller holds the
    // returned guard for the test's life and the file stays serialized.
    swiss_core::paths::DATA_DIR_LOCK.lock().await
}

/// Pin this test binary to one deterministic master key, so the sealed record file
/// never reaches DPAPI or the machine id (the same trick plugin_host.rs uses).
fn test_master_key() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // Safety: one write, under a OnceLock, of a variable nothing caches.
        unsafe { std::env::set_var(swiss_core::secure::key::MASTER_KEY_ENV_SWISS, "ab".repeat(32)) }
    });
}

/// A program that runs for about as long as the test needs it alive, spelled the
/// way this platform spells it. ${...} never appears in it - these are literals.
fn sleep_program() -> Value {
    if cfg!(windows) {
        json!({ "program": "cmd", "args": ["/c", "ping", "-n", "40", "127.0.0.1"] })
    } else {
        json!({ "program": "sh", "args": ["-c", "sleep 40"] })
    }
}

/// A program that exits non-zero at once - the autostart app whose start line
/// is all the boot pass ever sees of it.
fn failing_program() -> Value {
    if cfg!(windows) {
        json!({ "program": "cmd", "args": ["/c", "exit", "/b", "3"] })
    } else {
        json!({ "program": "sh", "args": ["-c", "exit 3"] })
    }
}

/// The boot in miniature (the plugin_host.rs harness, minus the echo MCP): the
/// built-ins plus the process plugin, the host booted, the boundary layered.
async fn full_app(tag: &str, raw: Value) -> (axum::Router, Arc<PluginHost>) {
    full_app_boot(tag, raw, false).await
}

/// The same rig with the boot attribution chosen: `daemon` is the detaching
/// `swiss start`'s shape, the only one "start with swiss" ever runs on.
async fn full_app_boot(tag: &str, raw: Value, daemon: bool) -> (axum::Router, Arc<PluginHost>) {
    test_master_key();
    let dir = std::env::temp_dir().join(format!("swiss-process-apps-{tag}-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("scratch directory");

    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let registry = Registry::new(3_600_000, calls.clone());
    let managed = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo {
        port: 19998,
        is_daemon: daemon,
    });
    let jobs = JobSystem::open(dir.join("jobs.json"), services.clone(), ConfigStore::memory(json!({})));
    let tunnel_store = Arc::new(std::sync::Mutex::new(swiss_tunnels::tunnel::TunnelStore::new(
        dir.join("tunnels.json"),
        19998,
    )));
    let tunnel_manager = TunnelManager::new(tunnel_store.clone(), None);
    let tunnels = Arc::new(swiss_tunnels::tunnel::api::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        mcp_display: None,
    });

    let ctx = swiss::app::AppContext::new(
        registry.clone(),
        Arc::new(single_token_manager(TOKEN)),
        managed.clone(),
        calls,
        Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
        "SWISS_TOKEN",
        19998,
    );
    let _ = ctx.catalog.set(services.catalog.clone());
    let process_state = swiss_process::api::ProcessState::new();
    // The trees the boundary gates: tunnels, jobs, the shared services (the
    // /api/runs surface the child-stop test drives), and the process tree.
    let mut host = PluginHost::new(ConfigStore::memory(raw));
    builtin::register_all(
        &mut host,
        &builtin::BuiltinDeps {
            registry: registry.clone(),
            managed,
            jobs: jobs.clone(),
            tunnels: tunnels.clone(),
            tunnel_manager: tunnel_manager.clone(),
            services: services.clone(),
            // The MCP module's history seam: inert here (the process tests never
            // touch the call log), but the deps struct demands the fields.
            call_log: Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            traffic: Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
            logs_dir: dir.join("logs"),
            process_state: process_state.clone(),
            data_history: swiss_data::history::DataHistory::new(dir.join("logs")),
        },
    )
    .expect("the built-ins register");
    let host = Arc::new(host);
    host.start_enabled().await;
    assert!(ctx.plugin_host.set(host.clone()).is_ok(), "host set once");

    let extra = swiss_tunnels::tunnel::api::mount(tunnels.clone())
        .merge(swiss_jobs::jobs::api::mount(jobs.clone()))
        .merge(swiss_host::services::api::mount(services.clone()))
        .merge(swiss_process::api::mount(process_state));
    let app = build_app(ctx, Some(extra));
    (app, host)
}

fn local(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19999")
        .header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        )
        .body(Body::empty())
        .expect("a request")
}

fn json_body(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19999")
        .header(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("a request")
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Option<Value>, String) {
    let response = app.clone().oneshot(req).await.expect("router answers");
    let status = response.status();
    let text = String::from_utf8_lossy(&axum::body::to_bytes(response.into_body(), 8 << 20).await.unwrap()).into_owned();
    let json = serde_json::from_str::<Value>(&text).ok();
    (status, json, text)
}

/// One app row by name, the way the panel polls it.
async fn row_of(app: &axum::Router, name: &str) -> Value {
    let (_, body, text) = send(app, local("GET", "/api/process")).await;
    let body = body.expect("JSON");
    body["apps"]
        .as_array()
        .unwrap_or_else(|| panic!("apps array missing: {text}"))
        .iter()
        .find(|row| row["name"] == json!(name))
        .cloned()
        .unwrap_or_else(|| panic!("no row for {name}: {text}"))
}

/// The whole app config as one plugin config value.
fn config_with_apps(apps: Value) -> Value {
    json!({ "process": { "config": { "apps": apps } } })
}

#[tokio::test]
async fn the_inventory_carries_the_process_page_and_routes() {
    let _seat = seat().await;
    let (app, _host) = full_app("inventory", config_with_apps(json!([]))).await;

    let (_, body, text) = send(&app, local("GET", "/api/plugins")).await;
    let body = body.expect("JSON");
    let row = body["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .find(|p| p["id"] == json!("process"))
        .cloned()
        .unwrap_or_else(|| panic!("no process row: {text}"));
    // The version that grew the page and the routes (SPEC §host.plugins).
    assert_eq!(row["version"], json!("0.2"), "{row}");
    assert_eq!(row["pages"], json!(["process"]), "{row}");
    assert_eq!(row["enabled"], json!(true), "{row}");
    assert_eq!(row["state"], json!("active"), "{row}");

    // The page itself, in the nav order the panel renders: order 55, a plain
    // content body, the committed entry the shell dynamic-imports.
    let page = body["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .find(|p| p["id"] == json!("process"))
        .cloned()
        .expect("the process page");
    assert_eq!(page["pluginId"], json!("process"), "{page}");
    assert_eq!(page["path"], json!("#process"), "{page}");
    assert_eq!(page["entry"], json!("/admin/js/views/process.js"), "{page}");
    assert_eq!(page["order"], json!(55), "{page}");
    assert_eq!(page["layout"], json!("page"), "{page}");
    assert_eq!(page["sidebar"], json!(false), "{page}");

    // And the routes are LIVE, not just declared: the tree answers through the
    // boundary while the plugin serves (the off case is the next test).
    let (status, _, text) = send(&app, local("GET", "/api/process")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _, text) = send(&app, local("GET", "/api/process/apps/ghost/log")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown app is 404, live: {text}");
}

#[tokio::test]
async fn every_process_route_is_a_structured_503_while_the_plugin_is_off() {
    let _seat = seat().await;
    let (app, _host) = full_app(
        "off",
        json!({ "process": { "disabled": true, "config": { "apps": [] } } }),
    )
    .await;

    // Every route of the tree (SPEC §process.apps): the boundary owns the whole
    // /api/process prefix, so each answers the structured 503 naming the plugin.
    let routes: [(&str, &str); 8] = [
        ("GET", "/api/process"),
        ("GET", "/api/process/history"),
        ("GET", "/api/process/children"),
        ("GET", "/api/process/apps/x/log"),
        ("POST", "/api/process/apps/x/start"),
        ("POST", "/api/process/apps/x/stop"),
        ("POST", "/api/process/apps/x/restart"),
        ("POST", "/api/process/children/1/stop"),
    ];
    for (method, uri) in routes {
        let (status, body, text) = send(&app, local(method, uri)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {uri}: {text}");
        let error = body.expect("JSON")["error"].as_str().unwrap_or_default().to_string();
        assert!(
            error.contains("process plugin"),
            "the 503 names the process plugin: {error}"
        );
    }

    // The page is still in the nav (a disabled plugin is a row, not an absence),
    // and re-enabling brings the tree back - the boundary's whole promise.
    let (status, _, text) = send(&app, json_body("POST", "/api/plugins/process/enable", json!({}))).await;
    assert!(status.is_success(), "enable answers: {text}");
    let (status, _, text) = send(&app, local("GET", "/api/process")).await;
    assert_eq!(status, StatusCode::OK, "back on air: {text}");
}

#[tokio::test]
async fn start_answers_202_and_the_outcome_lands_in_the_row_and_the_history() {
    let _seat = seat().await;
    let app = "spawn-nope";
    let (app_router, _host) = full_app(
        "start-202",
        config_with_apps(json!([{
            "name": app,
            "start": { "program": "definitely-not-a-program-4f91c", "args": [] },
            "ports": [23456],
            "image": "nope.exe"
        }])),
    )
    .await;

    // The submission is answered NOW (SPEC §process.apps): 202, the action runs
    // out-of-band, the row and the history are where the outcome lands.
    let (status, body, text) = send(
        &app_router,
        json_body("POST", &format!("/api/process/apps/{app}/start"), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(body.expect("JSON")["accepted"], json!(true), "{text}");

    // Poll like the panel: the row keeps the last action's say-so, the history
    // keeps the failure with the program named. A spawn that cannot happen fails
    // fast - no 30 s window for it.
    let mut row = row_of(&app_router, app).await;
    for _ in 0..200 {
        if row["lastAction"]["action"] == json!("start") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        row = row_of(&app_router, app).await;
    }
    assert_eq!(row["lastAction"]["action"], json!("start"), "{row}");
    assert_eq!(row["lastAction"]["ok"], json!(false), "{row}");
    assert!(
        row["lastAction"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("definitely-not-a-program-4f91c"),
        "the outcome's sentence is the row's say-so: {row}"
    );
    assert_eq!(row["status"], json!("stopped"), "a failed spawn is a stop: {row}");

    let (_, history, text) = send(
        &app_router,
        local("GET", &format!("/api/process/history?app={app}")),
    )
    .await;
    let entries = history.expect("JSON")["entries"].as_array().cloned().unwrap_or_default();
    let start = entries
        .iter()
        .find(|e| e["action"] == json!("start"))
        .cloned()
        .unwrap_or_else(|| panic!("no start line: {text}"));
    assert_eq!(start["ok"], json!(false), "{start}");
    assert!(
        start["message"].as_str().unwrap_or_default().contains("definitely-not-a-program-4f91c"),
        "the failure names the program: {start}"
    );

    // A second start while nothing runs is NOT a 409 - the previous action is
    // over. And an unknown app is a 404, synchronously.
    let (status, _, text) = send(
        &app_router,
        json_body("POST", &format!("/api/process/apps/{app}/start"), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let (status, _, text) = send(
        &app_router,
        json_body("POST", "/api/process/apps/ghost/start", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

#[tokio::test]
async fn an_invalid_put_is_refused_whole_and_nothing_runs() {
    let _seat = seat().await;
    let (app, _host) = full_app("put-refused", config_with_apps(json!([]))).await;

    // One bad field in one app refuses the WHOLE save, with every error named
    // (SPEC §process.apps): nothing half-loads, nothing runs.
    let (status, body, text) = send(
        &app,
        json_body(
            "PUT",
            "/api/plugins/process/config",
            json!({ "config": { "apps": [
                { "name": "fine", "start": { "program": "cmd" } },
                { "name": "UPPER", "start": { "program": "cmd" }, "ports": [19999] }
            ] } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    let error = body.expect("JSON")["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("apps[1].name"), "names the bad name: {error}");
    assert!(
        error.contains("apps[1].image"),
        "every bad field is named, not just the first: {error}"
    );

    // The table is unchanged: the first, valid app did NOT half-load.
    let (_, after, _) = send(&app, local("GET", "/api/process")).await;
    assert_eq!(after.expect("JSON")["apps"], json!([]), "refused whole");

    // And nothing ran: no capture file, no history line.
    let capture = swiss_core::paths::data_path(&["logs", "apps", "fine.log"]);
    assert!(!capture.exists(), "no capture for a refused app");
    let (_, history, _) = send(&app, local("GET", "/api/process/history")).await;
    let entries = history.expect("JSON")["entries"].as_array().cloned().unwrap_or_default();
    assert!(
        entries.iter().all(|e| e["app"] != json!("fine")),
        "no history line for a refused app"
    );
}

#[tokio::test]
async fn a_saved_config_names_what_is_storable_but_not_runnable() {
    let _seat = seat().await;
    // A program nothing resolves on the spawn PATH is storable (the PUT takes
    // it) but not runnable: the save's answer says so (SPEC §process.apps),
    // and the form renders the warning under the fields.
    let (app, _host) = full_app("config-warnings", config_with_apps(json!([]))).await;
    let (status, body, text) = send(
        &app,
        json_body(
            "PUT",
            "/api/plugins/process/config",
            json!({ "config": { "apps": [
                { "name": "gone-tool", "start": { "program": "definitely-not-on-path-71c4" } }
            ] } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let warnings = body.expect("JSON")["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .contains("definitely-not-on-path-71c4")),
        "names the unresolvable program: {warnings:?}"
    );
}

#[tokio::test]
async fn the_log_route_reads_only_the_configured_files() {
    let _seat = seat().await;
    // The app log is a file the TEST writes where it wants (inside the scratch
    // dir); the capture is where the plugin puts it. The route reads by CONFIG,
    // never by request (SPEC §process.apps).
    let dir = swiss_core::paths::test_home().join("process-apps-log-test");
    std::fs::create_dir_all(&dir).expect("log dir");
    let app_log = dir.join("service-2026.log");
    std::fs::write(&app_log, "boot ok
serving
").expect("app log");

    let app = "logapp";
    // A REF value (SPEC §process.apps): only what the config RESOLVES is masked
    // out of the tail - a literal is the config's own choice to show. The env
    // var is set for this process, the route resolves it at read time.
    // Safety: one write, before the app boots, of a uniquely-named variable.
    unsafe { std::env::set_var("PROC_APPS_SECRET_VALUE", "resolved-secret-77ffca0011") };
    let (router, _host) = full_app(
        "log-route",
        config_with_apps(json!([{
            "name": app,
            "start": { "program": "cmd" },
            "log": app_log.to_string_lossy(),
            "env": { "TOKEN": "${PROC_APPS_SECRET_VALUE}" }
        }])),
    )
    .await;

    // source=app reads the CONFIGURED file.
    let (status, body, text) = send(
        &router,
        local("GET", &format!("/api/process/apps/{app}/log?source=app")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body = body.expect("JSON");
    assert_eq!(body["lines"], json!(["boot ok", "serving"]), "{body}");

    // source=capture reads the plugin's own capture, and masks the RESOLVED
    // value the config carried (SPEC §process.apps).
    let capture = swiss_core::paths::data_path(&["logs", "apps", &format!("{app}.log")]);
    std::fs::create_dir_all(capture.parent().unwrap()).expect("capture dir");
    std::fs::write(
        &capture,
        "--- started ---
token=resolved-secret-77ffca0011
",
    )
    .expect("capture");
    let (status, body, text) = send(
        &router,
        local("GET", &format!("/api/process/apps/{app}/log")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let lines = body.expect("JSON")["lines"].as_array().cloned().unwrap();
    let joined = lines
        .iter()
        .map(|l| l.as_str().unwrap_or_default())
        .collect::<String>();
    assert!(
        !joined.contains("resolved-secret-77ffca0011"),
        "the resolved value is masked out of the capture: {joined}"
    );
    assert!(
        joined.contains('•'),
        "the mask is the bullet: {joined}"
    );

    // A request can never name a file: the only doors are source=capture|app.
    let (status, _, text) = send(
        &router,
        local("GET", &format!("/api/process/apps/{app}/log?source=app&path={}", app_log.to_string_lossy())),
    )
    .await;
    // The unknown query key is ignored by design (source=app wins); the proof is
    // that no source ever reads outside the two doors: an unknown source is 400.
    let _ = (status, text);
    let (status, _, text) = send(
        &router,
        local("GET", &format!("/api/process/apps/{app}/log?source=anywhere")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    std::fs::remove_file(&app_log).ok();
    std::fs::remove_file(&capture).ok();
}

#[tokio::test]
async fn stopping_a_run_child_cancels_it_through_the_coordinator() {
    let _seat = seat().await;
    let (app, _host) = full_app("child-stop", json!({})).await;

    // A slow run through the shared coordinator: its child is in the registry
    // (SPEC §process.children), and the ONLY stop path is the owner's hook.
    let (status, body, text) = send(
        &app,
        json_body("POST", "/api/runs", json!({ "action": "process.exec", "label": "slow", "input": sleep_program() })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let run_id = body.expect("JSON")["runId"].as_u64().expect("a runId");

    // The child appears with its run owner; find it by label.
    let mut pid = None;
    for _ in 0..200 {
        let (_, body, _) = send(&app, local("GET", "/api/process/children")).await;
        let rows = body.expect("JSON")["children"].as_array().cloned().unwrap_or_default();
        if let Some(row) = rows.iter().find(|r| r["label"].as_str().unwrap_or("").contains("slow")) {
            assert_eq!(row["owner"], json!("manual"), "a manual run's child: {row}");
            assert_eq!(row["kind"], json!("run"), "{row}");
            pid = row["pid"].as_u64();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let pid = pid.unwrap_or_else(|| panic!("the run child never appeared"));

    // The memory column the reader adds: a live child carries a working-set
    // reading, never a confident absence (SPEC §process.children).
    {
        let (_, body, _) = send(&app, local("GET", "/api/process/children")).await;
        let rows = body.expect("JSON")["children"].as_array().cloned().unwrap_or_default();
        let row = rows
            .iter()
            .find(|r| r["pid"] == json!(pid))
            .unwrap_or_else(|| panic!("the child row vanished"));
        assert!(row.get("memMb").is_some(), "a live child has a reading");
    }

    // Stopping it through the coordinator's hook: 200 with {ok, message} -
    // the spec's shape, so the panel can never read success as an error -
    // then the row goes away, the run reaches a terminal state (canceled).
    let (status, body, text) = send(&app, json_body("POST", &format!("/api/process/children/{pid}/stop"), json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let stop_body = body.expect("JSON");
    assert_eq!(stop_body["ok"], json!(true), "{text}");
    assert_eq!(stop_body["message"], json!("stopped"), "{text}");

    let (_, body, _) = send(&app, local("GET", "/api/process/children")).await;
    let rows = body.expect("JSON")["children"].as_array().cloned().unwrap_or_default();
    assert!(
        rows.iter().all(|r| r["pid"] != json!(pid)),
        "the stopped child is gone: {rows:?}"
    );

    for _ in 0..200 {
        let (_, body, _) = send(&app, local("GET", &format!("/api/runs/{run_id}?output=0"))).await;
        let state = body.expect("JSON")["state"].as_str().unwrap_or_default().to_string();
        if !matches!(state.as_str(), "queued" | "running") {
            assert_eq!(state, "canceled", "killed through the hook, not reaped: {state}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // An unknown pid is a 404, not a kill of whatever has the number next.
    let (status, _, text) = send(&app, json_body("POST", "/api/process/children/4294967295/stop", json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

#[tokio::test]
async fn disabling_the_plugin_leaves_a_started_app_running() {
    let _seat = seat().await;
    let app_name = "stayer";
    let (app, _host) = full_app(
        "disable-keeps",
        config_with_apps(json!([{
            "name": app_name,
            "start": sleep_program()
        }])),
    )
    .await;

    // Start it (no ports: alive at the settle is running), then take the pid.
    let (status, _, text) = send(
        &app,
        json_body("POST", &format!("/api/process/apps/{app_name}/start"), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let mut row = row_of(&app, app_name).await;
    let mut pid = None;
    for _ in 0..300 {
        row = row_of(&app, app_name).await;
        if row["status"] == json!("running") {
            pid = row["pid"].as_u64();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pid = pid.unwrap_or_else(|| panic!("the app never reached running: {row}")) as u32;

    // Disable the plugin: the routes 503, but the app is NOT ours to kill
    // (SPEC §process.apps) - it keeps running, someone stopped it or it ends.
    let (status, _, text) = send(&app, json_body("POST", "/api/plugins/process/disable", json!({}))).await;
    assert!(status.is_success(), "{text}");
    let (status, body, text) = send(&app, local("GET", "/api/process")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert!(
        body.expect("JSON")["error"].as_str().unwrap_or_default().contains("process plugin"),
        "{text}"
    );

    // The honest proof: the pid is still a live process on this machine.
    assert!(
        swiss_core::platform::process_name(pid).is_some(),
        "pid {pid} is still alive after the disable"
    );

    // Cleanup: kill the tree, never leave a 40-second ping behind.
    let _ = swiss_core::platform::tree_kill(pid);
}

#[tokio::test]
async fn a_disable_enable_cycle_is_not_a_reboot_for_start_with_swiss() {
    let _seat = seat().await;
    let app_name = "flaky";
    let (app, _host) = full_app_boot(
        "enable-no-boot",
        config_with_apps(json!([{
            "name": app_name,
            "start": failing_program(),
            "autostart": true
        }])),
        true,
    )
    .await;

    // The boot pass records exactly one by:boot start line for the autostart
    // app (it spawns, exits 3, the pump files the exit - the start line is
    // what this test counts).
    let boot_starts = |history: &Value| {
        history["entries"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter(|l| l["action"] == "start" && l["by"] == "boot")
                    .count()
            })
            .unwrap_or(0)
    };
    let mut first = 0;
    let mut seen = json!([]);
    for _ in 0..300 {
        let (_, history, _) = send(&app, local("GET", "/api/process/history")).await;
        seen = history.expect("JSON");
        first = boot_starts(&seen);
        if first == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (_, page, _) = send(&app, local("GET", "/api/process")).await;
    assert_eq!(
        first, 1,
        "the boot pass ran exactly once; history: {seen}, page: {}",
        page.map(|p| p.to_string()).unwrap_or_default()
    );

    // Disable, then enable: SPEC §process.apps - "Enabling the plugin later or
    // applying config never starts one either". The stopped autostart app must
    // NOT be restarted by the cycle.
    let (status, _, text) =
        send(&app, json_body("POST", "/api/plugins/process/disable", json!({}))).await;
    assert!(status.is_success(), "{text}");
    let (status, _, text) =
        send(&app, json_body("POST", "/api/plugins/process/enable", json!({}))).await;
    assert!(status.is_success(), "{text}");

    // A would-be second pass records its start line within milliseconds; a
    // second is a generous window for it to be seen NOT arriving.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (_, history, _) = send(&app, local("GET", "/api/process/history")).await;
    let history = history.expect("JSON");
    assert_eq!(
        boot_starts(&history),
        1,
        "a disable/enable cycle never re-runs start with swiss: {history}"
    );
}

// --- the Program picker's listing (SPEC §process.panel) -----------------------

/// A directory path as a query value: separators percent-encoded, the way the
/// panel's encodeURIComponent spells a Windows path.
fn q_dir(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "%5C").replace(' ', "%20")
}

#[tokio::test]
async fn fs_listing_is_read_only_directories_first_and_marks_executables() {
    let _lock = seat().await;
    let dir = std::env::temp_dir().join(format!("swiss-pick-{}", swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("inner")).expect("scratch");
    std::fs::write(dir.join("tool.exe"), b"x").expect("exe");
    std::fs::write(dir.join("run.BAT"), b"x").expect("bat");
    std::fs::write(dir.join("note.txt"), b"x").expect("txt");

    let (app, _host) = full_app("pick", json!({})).await;
    let uri = format!("/api/process/fs?dir={}", q_dir(&dir));
    let (status, body, text) = send(&app, local("GET", &uri)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body = body.expect("JSON");

    // Folders first, then files, each set name-sorted; the shape flags say
    // what the picker may fill the Program field with.
    let names: Vec<(String, bool, bool)> = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap_or_default().to_string(),
                e["dir"].as_bool().unwrap_or(false),
                e["exe"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("inner".to_string(), true, false),
            ("note.txt".to_string(), false, false),
            ("run.BAT".to_string(), false, true),
            ("tool.exe".to_string(), false, true),
        ],
        "{text}"
    );
    // The parent rides along for the Up button, the echoed dir for the input.
    assert!(body["parent"].is_string(), "{text}");
    assert_eq!(body["truncated"], json!(false));
    // The separator rides along so the panel never hard-codes a backslash.
    assert_eq!(body["sep"], json!(if cfg!(windows) { "\\" } else { "/" }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fs_listing_refuses_what_is_not_a_directory() {
    let _lock = seat().await;
    let file = std::env::temp_dir().join(format!("swiss-pick-file-{}", swiss_core::util::random_hex(6)));
    std::fs::write(&file, b"x").expect("scratch file");

    let (app, _host) = full_app("pick400", json!({})).await;
    let uri = format!("/api/process/fs?dir={}", q_dir(&file));
    let (status, body, text) = send(&app, local("GET", &uri)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(
        body.expect("JSON")["error"].as_str().unwrap_or_default().contains("not a directory"),
        "{text}"
    );

    let _ = std::fs::remove_file(&file);
}
