//! Integration tests against the real router — the successor of the Phase 0 spike tests. The
//! full HTTP stack is driven with a real rmcp streamable-HTTP client, exactly as a hosted MCP
//! client would, plus the admin API and the loopback boundary.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use local_mcp_gateway::adapters::make_adapter;
use local_mcp_gateway::app::{build_app, AppContext, BODY_LIMIT};
use local_mcp_gateway::config::ServerDef;
use local_mcp_gateway::managed::ManagedStore;
use local_mcp_gateway::registry::{Registry, Source};
use local_mcp_gateway::token::single_token_manager;

const TOKEN: &str = "test-token-0123456789abcdef";

fn echo_def() -> ServerDef {
    ServerDef(json!({ "type": "echo" }).as_object().cloned().unwrap())
}

/// A router with one started echo MCP, ready to serve.
async fn app_with_echo() -> axum::Router {
    let registry = Registry::new(3_600_000);
    let store = Arc::new(ManagedStore::open_at(
        std::env::temp_dir()
            .join(format!(
                "lmg-app-{}",
                local_mcp_gateway::util::random_hex(8)
            ))
            .join("managed.json"),
    ));
    let adapter = make_adapter(&echo_def(), "echo").expect("echo adapter");
    registry
        .register("echo", Source::Config, echo_def(), adapter)
        .expect("register");
    registry.start("echo").await.expect("start");
    let tokens = Arc::new(single_token_manager(TOKEN));
    let ctx = AppContext::new(registry, tokens, store, "MCP_GATEWAY_TOKEN", 19998);
    build_app(ctx, None)
}

/// One raw request against the built app (loopback headers included, as a local client sends).
async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Option<Value>, String) {
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

fn local(req: Request<Body>) -> Request<Body> {
    let host = req
        .headers()
        .get(header::HOST)
        .cloned()
        .unwrap_or_else(|| header::HeaderValue::from_static("127.0.0.1:19999"));
    let mut req = req;
    req.headers_mut().insert(header::HOST, host);
    req
}

// --- the boundary -------------------------------------------------------------------------------

#[tokio::test]
async fn refuses_a_foreign_host_header() {
    let app = app_with_echo().await;
    // The DNS-rebinding shape: a Host naming another site on a local connection.
    let req = Request::get("/health")
        .header(header::HOST, "evil.test:19999")
        .body(Body::empty())
        .unwrap();
    let (status, json, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(json.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("Host must name this machine"));
}

#[tokio::test]
async fn refuses_a_foreign_origin() {
    let app = app_with_echo().await;
    let req = Request::get("/health")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::ORIGIN, "http://evil.test")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --- health + panel ----------------------------------------------------------------------------

#[tokio::test]
async fn health_and_panel_serve() {
    let app = app_with_echo().await;
    let (status, json, _) = send(
        &app,
        local(Request::get("/health").body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json.unwrap(), json!({ "ok": true }));

    let (status, _, text) = send(&app, local(Request::get("/").body(Body::empty()).unwrap())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("<!doctype html") || text.contains("<!DOCTYPE html"),
        "the panel shell"
    );

    let (status, _, css) = send(
        &app,
        local(
            Request::get("/admin/styles/base.css")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!css.is_empty());

    // The page registry and its lazily imported view modules are served like every other panel
    // module: no-store is already the default, and the content type must be exact so browsers
    // accept them as module scripts in subdirectories too.
    for path in ["/admin/js/page-registry.js", "/admin/js/page-core.js", "/admin/js/views/jobs.js"] {
        let (status, _, body) = send(
            &app,
            local(Request::get(path).body(Body::empty()).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path} must serve");
        assert!(body.contains("export "), "{path} must be a module");
    }
    // A page module outside the panel tree never resolves, even with a valid extension.
    let (status, _, _) = send(
        &app,
        local(
            Request::get("/admin/js/views/../../../Cargo.toml")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Traversal and unknown extensions are refused, not guessed.
    let (status, _, _) = send(
        &app,
        local(
            Request::get("/admin/../Cargo.toml")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_routes_answer_the_node_404_shape() {
    let app = app_with_echo().await;
    let (status, json, _) = send(
        &app,
        local(Request::get("/nope").body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json.unwrap()["error"], "no route for GET /nope");
}

// --- the admin API -----------------------------------------------------------------------------

#[tokio::test]
async fn admin_api_lists_mcps_and_info() {
    let app = app_with_echo().await;
    let (status, json, _) = send(
        &app,
        local(Request::get("/api/mcps").body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mcps = json.unwrap()["mcps"].as_array().cloned().unwrap();
    assert_eq!(mcps.len(), 1);
    assert_eq!(mcps[0]["name"], "echo");
    assert_eq!(mcps[0]["type"], "echo");
    assert_eq!(mcps[0]["lifecycle"], "started");
    assert_eq!(mcps[0]["state"], "unknown"); // echo has no ping; a started one shows health

    let (status, json, _) = send(
        &app,
        local(Request::get("/api/info").body(Body::empty()).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json.unwrap()["tokenEnv"], "MCP_GATEWAY_TOKEN");
}

#[tokio::test]
async fn tools_paging_via_the_admin_api() {
    let app = app_with_echo().await;
    let (status, json, _) = send(
        &app,
        local(
            Request::get("/api/mcps/echo/tools")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "paging endpoint serves");
    let body = json.unwrap();
    let tools = body["tools"].as_array().cloned().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "echo");
    assert_eq!(body["pageSize"], 50);
}

#[tokio::test]
async fn panel_call_runs_a_tool_and_logs_it() {
    let app = app_with_echo().await;
    // Point the call log at a temp dir so the test never touches the real data dir.
    local_mcp_gateway::calls::set_call_log_dir(std::env::temp_dir().join(format!(
        "lmg-calls-{}",
        local_mcp_gateway::util::random_hex(8)
    )));
    let req = Request::post("/api/mcps/echo/call")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "tool": "echo", "arguments": { "msg": "from the panel" } }).to_string(),
        ))
        .unwrap();
    let (status, json, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let body = json.unwrap();
    assert_eq!(body["ok"], true, "panel call succeeds");
    assert_eq!(body["text"], "from the panel");
    // The call landed in the log under the panel source.
    let (status, json, _) = send(
        &app,
        local(
            Request::get("/api/mcps/echo/calls")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let calls = json.unwrap()["calls"].as_array().cloned().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["via"], "panel");
    assert_eq!(calls[0]["tool"], "echo");
    assert_eq!(calls[0]["ok"], true);
}

// --- the MCP endpoint through the real router ---------------------------------------------------

#[tokio::test]
async fn mcp_endpoint_rejects_missing_bearer_in_jsonrpc_shape() {
    let app = app_with_echo().await;
    let req = Request::post("/echo")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "method": "tools/list", "id": 1 }).to_string(),
        ))
        .unwrap();
    let (status, json, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The pre-body refusal shape: Node's `wrapped.refuse` fast path answered
    // {error:"Unauthorized"} — plain admin shape, not JSON-RPC (the SDK-era jsonError only
    // fired once the handler itself ran, past this gate).
    let body = json.unwrap();
    assert_eq!(body["error"], "Unauthorized");
}

#[tokio::test]
async fn mcp_delete_acknowledges_204() {
    let app = app_with_echo().await;
    let req = Request::delete("/echo")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.expect("router answers");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unknown_mcp_path_answers_503_jsonrpc() {
    let app = app_with_echo().await;
    let req = Request::post("/nope")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "method": "tools/list", "id": 1 }).to_string(),
        ))
        .unwrap();
    let (status, json, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json.unwrap()["error"]["message"], "Unknown MCP path: nope");
}

/// A real rmcp streamable-HTTP client driving the echo endpoint through the router — the full
/// 2026 protocol path, exactly what a hosted client does. Served on a real loopback socket for
/// the lifetime of the test (the client transport only speaks URLs).
#[tokio::test]
async fn mcp_endpoint_serves_a_real_client() {
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    };

    let app = app_with_echo().await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind ephemeral");
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("serve");
    });

    let config = StreamableHttpClientTransportConfig::with_uri(format!(
        "http://127.0.0.1:{}/echo",
        addr.port()
    ))
    .auth_header(TOKEN);
    let transport = StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
    let client = rmcp::service::serve_client(
        local_mcp_gateway::introspect::GatewayIntrospectClient,
        transport,
    )
    .await
    .expect("initialize against the gateway endpoint");

    let tools = client.list_tools(None).await.expect("tools/list");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "echo");

    let mut params = CallToolRequestParams::default();
    params.name = "echo".into();
    params.arguments = Some(
        json!({ "msg": "hi over http" })
            .as_object()
            .cloned()
            .unwrap(),
    );
    let result = client.call_tool(params).await.expect("tools/call");
    let text = result
        .content
        .first()
        .map(|c| match c {
            rmcp::model::ContentBlock::Text(t) => t.text.clone(),
            other => format!("{other:?}"),
        })
        .unwrap_or_default();
    assert_eq!(text.trim(), "hi over http");
    let _ = client.cancel().await;
    server.abort();
}

// --- the body limit ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_mcp_body_is_refused() {
    let app = app_with_echo().await;
    let big = "x".repeat(BODY_LIMIT + 1);
    let req = Request::post("/echo")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(big))
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
