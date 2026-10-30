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

//! OAuth authorize (SPEC §mcp.oauth): the flow end to end against a fake remote, and the MCP types
//! that add with only a name.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{row_named, setup, Harness};

// --- OAuth authorize (SPEC §mcp.oauth) ----------------------------------------------------------------
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

/// A recording forward proxy for plain http:// targets. A client sends a proxied request in
/// absolute form, so the handler sees the whole target URL: it notes the path and relays the
/// request on a proxy-less client. A request in origin form was never sent to a proxy, and
/// gets a 502.
async fn recording_proxy() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use axum::response::IntoResponse;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let relay = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("relay client");
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let log = log.clone();
        let relay = relay.clone();
        async move {
            let (parts, body) = req.into_parts();
            if parts.uri.authority().is_none() {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            log.lock().expect("seen").push(parts.uri.path().to_string());
            let body = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
            let mut out = relay.request(parts.method, parts.uri.to_string()).body(body);
            for (name, value) in &parts.headers {
                let hop = matches!(
                    name.as_str(),
                    "host" | "connection" | "proxy-connection" | "content-length"
                );
                if !hop {
                    out = out.header(name.clone(), value.clone());
                }
            }
            match out.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let content_type = resp.headers().get("content-type").cloned();
                    let bytes = resp.bytes().await.unwrap_or_default();
                    let mut reply = (status, bytes).into_response();
                    if let Some(content_type) = content_type {
                        reply.headers_mut().insert("content-type", content_type);
                    }
                    reply
                }
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}"), seen)
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
async fn the_authorize_flow_rides_the_defs_proxy() {
    // SPEC §mcp.oauth: discovery, registration and the code exchange go out the way the MCP's
    // own traffic does. The flow used to run on a proxy-less client, so a network that reaches
    // the provider only through the proxy could never finish the consent the MCP waits on.
    let h = setup();
    let (base, _remote) = fake_figma_remote().await;
    let (proxy, seen) = recording_proxy().await;
    h.register(
        "fig-px",
        json!({ "type": "http", "url": format!("{base}/mcp"), "auth": "oauth", "proxy": proxy }),
    );

    let (status, body) = h.post("/api/mcps/fig-px/authorize", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let ready = poll_authorize(&h, "fig-px", &["authorization_required"]).await;
    let authorization_url = ready["authorizationUrl"]
        .as_str()
        .expect("authorizationUrl")
        .to_string();
    approve_in_a_browser(&authorization_url).await;
    let done = poll_authorize(&h, "fig-px", &["approved"]).await;
    assert_eq!(done["tools"], json!(2), "{done}");

    // Every leg crossed the proxy: the flow's three and the auto-started MCP's own.
    let seen = seen.lock().expect("seen").clone();
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-authorization-server",
        "/register",
        "/token",
        "/mcp",
    ] {
        assert!(
            seen.iter().any(|p| p == path),
            "{path} never crossed the proxy: {seen:?}"
        );
    }
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
    // SPEC §mcp.figma: figma is its own type — the panel asks for a name, the type decides the
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

    // The proxy is not the type's to decide - it is this machine's route to Figma - so a figma
    // def takes one and stores it as written, still with no url or auth beside it.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "fig-proxy", "type": "figma", "proxy": " http://127.0.0.1:7890 " }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, d) = h.get("/api/mcps/fig-proxy/details").await;
    assert_eq!(d["config"]["proxy"], json!("http://127.0.0.1:7890"), "{d}");
    assert_eq!(d["config"].get("url"), None, "{d}");
    assert_eq!(d["config"].get("auth"), None, "{d}");
}
#[tokio::test]
async fn the_zai_vision_type_adds_and_keeps_the_key_a_reference() {
    // The zai-vision type is the @z_ai/mcp-server port compiled in: no child, no endpoint of
    // its own. What this pins is the SHAPE - the row tags zai-vision, the def keeps the apiKey
    // exactly as written (${...} reference stays a reference, SPEC §host.vault), a literal key is
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
