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

//! The edge every request crosses: the in-process session gate, the password route that does not
//! exist, and JSON body parsing.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use super::setup;
use super::{build_app, sandbox, AppContext, ManagedStore, Registry, TokenManager, TOKEN};
use swiss::session::{next_state, AdminSession, CLI_KEY_HEADER};

// --- the boundary --------------------------------------------------------------------------

#[tokio::test]
async fn serves_reads_and_mutations_in_process_without_a_credential() {
    // An in-process call (no socket, no ConnectInfo) passes the admin session gate the way it
    // passes the loopback guard - which is what lets this suite drive the router directly. The
    // socket side of the gate is pinned in tests/admin_session.rs (SPEC §host.session).
    let h = setup();
    assert_eq!(h.get("/api/mcps").await.0, StatusCode::OK);
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "x", "command": "node x" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn has_no_password_route_to_answer() {
    // A browser signs in with a one-time link on GET /?token= (SPEC §host.session) - there is no password
    // login to post to.
    let h = setup();
    let (status, _) = h
        .post(
            "/api/login",
            json!({ "username": "admin", "password": "admin" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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

// --- the real plugin trees behind the admin boundary ---------------------------------------
//
// knowledge.rs pins the memory tree's wrapper behaviour through the gateway tree, and
// tests/admin_session.rs pins the session gate with a synthetic extra route. What these
// pin is the join of the two: the REAL plugin mounts (llm, memory, process) join through
// `build_app`'s `extra` exactly as src/server.rs merges them, and a request that came
// through a socket meets the fail-closed session gate, the bearer rejection, the loopback
// guard and the cookie's origin port on those routes as on every /api route. No plugin
// route is an exemption, and none of them re-proves auth on its own.

/// One gateway with the three new trees mounted through `extra` (the boot sequence's
/// merge order from src/server.rs) and a session installed - the shape the production
/// router serves, minus every subsystem these cases do not touch. The llm state is never
/// applied: its status answers 200 with an empty row, and its history reads answer the
/// honest 503 of a store that is not open.
struct PluginGate {
    app: axum::Router,
    cli_key: String,
    /// `name=value` of a session cookie this gateway signed - what a signed-in browser sends.
    cookie: String,
}

fn plugin_gate() -> PluginGate {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-plugin-boundary-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry,
        tokens,
        store,
        calls,
        traffic,
        "SWISS_TOKEN",
        19999,
    );
    let state = next_state(None);
    let cli_key = state["cliKey"].as_str().expect("a cli key").to_string();
    let session = AdminSession::from_state(&state, 19999).expect("a session");
    let cookie = session
        .issue_cookie()
        .split(';')
        .next()
        .expect("name=value")
        .to_string();
    let _ = ctx.session.set(session);
    let llm = swiss_llm::LlmState::new();
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    let extra = swiss_plugins::llm_api::mount(llm.clone(), services)
        .merge(swiss_plugins::memory_api::mount(Arc::new(
            swiss_plugins::memory_api::MemoryState::with_llm(llm),
        )))
        .merge(swiss_process::api::mount(swiss_process::api::ProcessState::new()));
    PluginGate {
        app: build_app(ctx, Some(extra)),
        cli_key,
        cookie,
    }
}

/// A request that arrived through the listener: the ConnectInfo extension it records.
fn socket(mut req: Request<Body>) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(std::net::SocketAddr::from((
        [127, 0, 0, 1],
        50_321,
    ))));
    req
}

fn get_req(uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19999")
}

async fn status_of(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone()
        .oneshot(req)
        .await
        .expect("the router answers")
        .status()
}

#[tokio::test]
async fn the_plugin_trees_fail_closed_for_a_socket_request_without_a_credential() {
    let g = plugin_gate();
    for path in [
        "/api/llm/status",
        "/api/llm/history/calls",
        "/api/knowledge/status",
        "/api/process",
    ] {
        let req = socket(get_req(path).body(Body::empty()).expect("request"));
        assert_eq!(
            status_of(&g.app, req).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_valid_mcp_bearer_and_a_wrong_cli_key_do_not_open_the_plugin_trees() {
    let g = plugin_gate();
    // TOKEN is a bearer this gateway's TokenManager itself accepts on /mcp/* - which is
    // exactly why it must be nothing at all on /api/*.
    let req = socket(
        get_req("/api/llm/status")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::UNAUTHORIZED);
    // A CLI key that is not this gateway's is just as closed.
    let req = socket(
        get_req("/api/llm/status")
            .header(CLI_KEY_HEADER, "not-this-gateways-key")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::UNAUTHORIZED);
    // The right key passes the gate onto the route itself: the unapplied row answers 200.
    let req = socket(
        get_req("/api/llm/status")
            .header(CLI_KEY_HEADER, &g.cli_key)
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::OK);
}

#[tokio::test]
async fn a_non_loopback_host_is_refused_before_the_session_even_off_api() {
    let g = plugin_gate();
    // The guard answers before auth does: a plugin route with the CLI key in hand is
    // still refused, and so is the unauthenticated liveness route off /api.
    let req = socket(
        Request::builder()
            .method("GET")
            .uri("/api/llm/status")
            .header(header::HOST, "evil.example")
            .header(CLI_KEY_HEADER, &g.cli_key)
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::FORBIDDEN);
    let req = socket(
        Request::builder()
            .method("GET")
            .uri("/health")
            .header(header::HOST, "evil.example")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_cookie_opens_the_plugin_trees_only_from_the_panels_own_port() {
    let g = plugin_gate();
    // The cookie is signed for port 19999 and the Host says 19999: a page on another
    // local port is a different origin, and the browser attaches the cookie anyway
    // (same site) - the gate must refuse it on a plugin route as on every /api route.
    let req = socket(
        get_req("/api/llm/history/runs")
            .header(header::COOKIE, &g.cookie)
            .header(header::ORIGIN, "http://127.0.0.1:19998")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, req).await, StatusCode::FORBIDDEN);
    // From the panel's own origin, port included, the same cookie passes the gate and
    // reaches the route - whose honest answer with no store open is the 503, not 401.
    let req = socket(
        get_req("/api/llm/history/runs")
            .header(header::COOKIE, &g.cookie)
            .header(header::ORIGIN, "http://127.0.0.1:19999")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(
        status_of(&g.app, req).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn state_changing_plugin_routes_are_gated_as_well() {
    let g = plugin_gate();
    // The mutating shapes of the new trees - an llm Test, a Process stop, a knowledge
    // delete - sit behind the same gate: no credential, no route.
    let llm_test = socket(
        Request::builder()
            .method("POST")
            .uri("/api/llm/providers/test")
            .header(header::HOST, "127.0.0.1:19999")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, llm_test).await, StatusCode::UNAUTHORIZED);
    let stop = socket(
        Request::builder()
            .method("POST")
            .uri("/api/process/apps/nothing-here/stop")
            .header(header::HOST, "127.0.0.1:19999")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, stop).await, StatusCode::UNAUTHORIZED);
    let purge = socket(
        Request::builder()
            .method("DELETE")
            .uri("/api/knowledge/records/x?project=p")
            .header(header::HOST, "127.0.0.1:19999")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, purge).await, StatusCode::UNAUTHORIZED);
    // The mutating routes the backend is adding next - an agent-test cancel, a run's
    // close - are covered before they exist: the session gate sits in the layers that
    // also wrap the 404 fallback (axum's Router::layer covers the fallback; it is
    // route_layer that runs only for a matched route), so an unknown /api path through
    // a socket answers 401 first today, and the same 401 in front of the real handler
    // once the routes land.
    let cancel = socket(
        Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/cancel")
            .header(header::HOST, "127.0.0.1:19999")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{\"runId\":1}"))
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, cancel).await, StatusCode::UNAUTHORIZED);
    let close = socket(
        Request::builder()
            .method("POST")
            .uri("/api/llm/history/runs/1/close")
            .header(header::HOST, "127.0.0.1:19999")
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(status_of(&g.app, close).await, StatusCode::UNAUTHORIZED);
    // The same two routes WITH the right key: what answers must then be a REAL handler,
    // never the fallback. The close route's no-config answer is the history-not-open
    // 503; a cancel that reached a handler is anything but the fallback 404. These two
    // probes are red until the backend lands the routes - today the fallback 404s them,
    // which is exactly what they exist to catch.
    let cancel_authed = socket(
        Request::builder()
            .method("POST")
            .uri("/api/llm/agents/test/cancel")
            .header(header::HOST, "127.0.0.1:19999")
            .header(header::CONTENT_TYPE, "application/json")
            .header(CLI_KEY_HEADER, &g.cli_key)
            .body(Body::from("{\"runId\":1}"))
            .expect("request"),
    );
    let cancel_status = status_of(&g.app, cancel_authed).await;
    assert_ne!(
        cancel_status,
        StatusCode::NOT_FOUND,
        "the cancel route must be a real route behind the gate, not the fallback 404"
    );
    let close_authed = socket(
        Request::builder()
            .method("POST")
            .uri("/api/llm/history/runs/1/close")
            .header(header::HOST, "127.0.0.1:19999")
            .header(CLI_KEY_HEADER, &g.cli_key)
            .body(Body::empty())
            .expect("request"),
    );
    assert_eq!(
        status_of(&g.app, close_authed).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
