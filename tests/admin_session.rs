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

//! The admin session gate (SPEC §host.session), through the real router.
//!
//! A request that came through a socket carries ConnectInfo - the listener records it - so
//! these cases plant one to look like the real thing; the rest of the suite drives the router
//! in-process without it, and the last case pins that such a request still passes.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use serde_json::Value;
use tower::ServiceExt;

use swiss::app::{build_app, AppContext};
use swiss::session::{next_state, AdminSession, CLI_KEY_HEADER};
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::registry::Registry;

const TOKEN: &str = "session-tok-0123456789abcdef";

fn sandbox() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let home = std::env::temp_dir().join(format!(
            "swiss-session-home-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&home).expect("create the scratch home");
        // Safety: once, before anything reads either variable.
        unsafe {
            std::env::set_var("SWISS_HOME", &home);
            std::env::set_var("SWISS_MASTER_KEY", "ab".repeat(32));
        }
    });
}

struct Gate {
    app: axum::Router,
    cli_key: String,
}

/// A gateway with a session installed (or not), and one extra-tree route to prove the second
/// router is behind the gate too.
fn gateway(with_session: bool) -> Gate {
    sandbox();
    let dir = std::env::temp_dir().join(format!("swiss-session-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry,
        tokens,
        store,
        calls,
        Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
        "SWISS_TOKEN",
        19999,
    );
    let state = next_state(None);
    let cli_key = state["cliKey"].as_str().expect("a cli key").to_string();
    if with_session {
        let _ = ctx
            .session
            .set(AdminSession::from_state(&state, 19999).expect("a session"));
    }
    let extra = axum::Router::new().route("/api/extra-tree", get(|| async { "extra" }));
    Gate {
        app: build_app(ctx, Some(extra)),
        cli_key,
    }
}

fn socket(mut req: Request<Body>) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 50_123))));
    req
}

fn get_req(uri: &str) -> axum::http::request::Builder {
    Request::builder().method("GET").uri(uri).header(header::HOST, "127.0.0.1:19999")
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.clone().oneshot(req).await.expect("the router answers");
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("a body");
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn a_socket_request_without_a_credential_is_refused_on_both_api_trees() {
    let g = gateway(true);
    for path in ["/api/mcps", "/api/extra-tree", "/api/session/ticket"] {
        let (status, _, body) = send(&g.app, socket(get_req(path).body(Body::empty()).unwrap())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
        assert!(body.contains("swiss open"), "{path}: {body}");
    }
    // Liveness and the panel's code stay reachable; the shell itself is the lock page.
    let (status, _, _) = send(&g.app, socket(get_req("/health").body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = send(&g.app, socket(get_req("/").body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("swiss open") && body.contains("需要登录"), "{body}");
}

#[tokio::test]
async fn the_cli_key_opens_the_api_and_the_mcp_bearer_does_not() {
    let g = gateway(true);
    let (status, _, _) = send(
        &g.app,
        socket(get_req("/api/extra-tree").header(CLI_KEY_HEADER, &g.cli_key).body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &g.app,
        socket(
            get_req("/api/mcps")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "an MCP client's token is not an admin credential");
}

#[tokio::test]
async fn a_login_token_signs_a_browser_in_once() {
    let g = gateway(true);
    // The CLI mints the link.
    let (status, _, body) = send(
        &g.app,
        socket(
            Request::builder()
                .method("POST")
                .uri("/api/session/ticket")
                .header(header::HOST, "127.0.0.1:19999")
                .header(CLI_KEY_HEADER, &g.cli_key)
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let minted: Value = serde_json::from_str(&body).expect("json");
    let token = minted["token"].as_str().expect("a token").to_string();
    assert_eq!(minted["url"], format!("http://127.0.0.1:19999/?token={token}"));

    // The browser spends it: a clean redirect and the cookie.
    let (status, headers, _) =
        send(&g.app, socket(get_req(&format!("/?token={token}")).body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
    let set_cookie = headers[header::SET_COOKIE].to_str().expect("ascii").to_string();
    assert!(set_cookie.starts_with("swiss_session_19999="), "{set_cookie}");
    let cookie = set_cookie.split(';').next().expect("name=value").to_string();

    // The cookie opens the shell and the API.
    let (status, _, body) =
        send(&g.app, socket(get_req("/").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("needs a sign-in"));
    let (status, _, _) = send(
        &g.app,
        socket(get_req("/api/mcps").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The same link a second time: spent.
    let (status, headers, body) =
        send(&g.app, socket(get_req(&format!("/?token={token}")).body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.get(header::SET_COOKIE).is_none());
    assert!(body.contains("already been used"), "{body}");
}

#[tokio::test]
async fn without_a_session_installed_the_gate_fails_closed() {
    let g = gateway(false);
    let (status, _, _) = send(
        &g.app,
        socket(get_req("/api/mcps").header(CLI_KEY_HEADER, &g.cli_key).body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&g.app, socket(get_req("/").body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_in_process_request_passes_as_it_passes_the_loopback_guard() {
    // What every other suite relies on: oneshot without ConnectInfo is not a socket.
    let g = gateway(true);
    let (status, _, _) = send(&g.app, get_req("/api/mcps").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
}
