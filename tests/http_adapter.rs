//! The http-adapter tests that need a REAL gateway: the adapter proxies this very binary
//! serving echo over HTTP on an ephemeral loopback port, exactly as the Node suite did.
//! Lived inside adapters/http.rs before the workspace split; the composition crate owns the
//! app these tests build, so they moved to its integration suite.

use std::sync::Arc;

use lmg_host::config::ServerDef;
use lmg_mcp::adapters::http::HttpAdapter;
use lmg_mcp::adapters::Adapter as _;
use serde_json::{json, Value};

const TOKEN: &str = "remote-token-0123456789abcdef";

fn def(v: Value) -> ServerDef {
    match v {
        Value::Object(o) => ServerDef(o),
        other => panic!("a server def is an object, got {other}"),
    }
}

// ---- the proxy, against a real gateway ---------------------------------------------

/// The gateway itself, serving one echo MCP over HTTP on an ephemeral loopback port.
struct Remote {
    url: String,
    registry: Arc<lmg_mcp::registry::Registry>,
    server: tokio::task::JoinHandle<()>,
}

impl Remote {
    async fn stop(self) {
        self.server.abort();
        self.registry.close_all().await;
    }
}

async fn remote_echo() -> Remote {
    use lmg_mcp::registry::{Registry, Source};
    let echo = def(json!({ "type": "echo" }));
    let registry = Registry::new(3_600_000, lmg_mcp::calls::test_log());
    let adapter = lmg_mcp::adapters::make_adapter(&echo, "echo", &lmg_mcp::calls::test_log())
        .expect("echo adapter");
    registry
        .register("echo", Source::Config, echo, adapter)
        .expect("register");
    registry.start("echo").await.expect("start");
    let store = Arc::new(lmg_host::managed::ManagedStore::open_at(
        std::env::temp_dir()
            .join(format!("lmg-http-{}", lmg_core::util::random_hex(8)))
            .join("managed.json"),
    ));
    let tokens = Arc::new(lmg_host::token::single_token_manager(TOKEN));
    let ctx = lmg::app::AppContext::new(
        registry.clone(),
        tokens,
        store,
        lmg_mcp::calls::test_log(),
        "MCP_GATEWAY_TOKEN",
        19998,
    );
    let app = lmg::app::build_app(ctx, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Remote {
        url: format!("http://127.0.0.1:{port}/echo"),
        registry,
        server,
    }
}

fn proxy_of(url: &str, token: &str) -> HttpAdapter {
    HttpAdapter::new(
        &def(json!({
            "type": "http",
            "url": url,
            "headers": { "Authorization": format!("Bearer {token}") },
        })),
        "r",
        lmg_mcp::calls::test_log(),
    )
    .expect("adapter")
}

#[tokio::test]
async fn lists_the_remotes_tools_through_the_proxy() {
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, TOKEN);
    let endpoint = proxy.build().await.expect("the handshake succeeds");
    let (tools, _) = endpoint
        .probe
        .list("tools", None)
        .await
        .expect("tools/list");
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(names, vec!["echo"]);
    proxy.close().await;
    remote.stop().await;
}

#[tokio::test]
async fn fails_to_connect_when_the_configured_headers_do_not_authenticate() {
    // build() runs the initialize handshake, so a wrong bearer is a start error rather than
    // a green MCP whose every call fails.
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, "wrong-token");
    assert!(proxy.build().await.is_err());
    proxy.close().await;
    remote.stop().await;
}

#[tokio::test]
async fn round_trips_a_tool_call_to_the_remote() {
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, TOKEN);
    let endpoint = proxy.build().await.expect("handshake");
    let result = endpoint
        .probe
        .call_tool("echo", json!({ "msg": "hi" }))
        .await
        .expect("the remote answers");
    assert!(
        result.to_string().contains("hi"),
        "the remote's own text must come back: {result}"
    );
    proxy.close().await;
    remote.stop().await;
}

#[tokio::test]
async fn announces_only_the_capabilities_the_remote_has() {
    // echo serves only tools; announcing resources or prompts would make every client ask for
    // lists that cannot exist — round trips billed by a metered remote for a known-empty
    // answer.
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, TOKEN);
    proxy.build().await.expect("handshake");
    let caps = proxy.session_caps().await.expect("the connection is open");
    assert!(caps.get("tools").is_some(), "{caps}");
    assert!(caps.get("resources").is_none(), "{caps}");
    assert!(caps.get("prompts").is_none(), "{caps}");
    proxy.close().await;
    remote.stop().await;
}

#[tokio::test]
async fn declares_no_ping_so_a_metered_remote_is_never_probed() {
    // The registry probes every started MCP every 15 s; against a third-party endpoint that
    // is thousands of unrequested requests a day. `None` makes the registry report "unknown",
    // which is the truth. Reachability was proven once, by the handshake in build().
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, TOKEN);
    proxy.build().await.expect("handshake");
    assert!(proxy.ping().await.is_none());
    proxy.close().await;
    remote.stop().await;
}

#[tokio::test]
async fn close_drops_the_connection_so_the_next_build_reconnects() {
    let remote = remote_echo().await;
    let proxy = proxy_of(&remote.url, TOKEN);
    proxy.build().await.expect("handshake");
    proxy.close().await;
    // The remote is gone: a re-connect must fail rather than serve a stale session.
    remote.stop().await;
    assert!(proxy.build().await.is_err());
}
