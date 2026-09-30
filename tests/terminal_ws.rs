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

//! SPEC §terminal.api's acceptance surface: the /api/terminal routes over a REAL socket.
//!
//! The handshake decisions cannot be driven through tower's oneshot — axum's upgrade
//! extractor needs hyper's OnUpgrade state, which only a live connection carries — so
//! this file boots the composition server.rs boots, serves it on an ephemeral loopback
//! port, and talks to it with a real WebSocket client (tokio-tungstenite: the same
//! crate axum's ws feature compiled in, TLS features off — the target is 127.0.0.1).
//! Plain HTTP assertions still go through oneshot on the same router.
//!
//! The far side of every session is a fake shell provider sitting in the host's shell
//! seat — the seat the tunnels plugin holds in the real gateway; that plugin is
//! disabled in these configs so the fake can take the chair — which is what lets a
//! wrong ticket, a replay and a reconnect be asserted exactly, with no SSH server and
//! no ConPTY child in sight.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::protocol::Message;
use tower::ServiceExt;

use swiss_host::config_store::ConfigStore;
use swiss_host::managed::ManagedStore;
use swiss_host::services::shell::{
    PtyEvent, PtyIn, PtyInput, PtyOut, PtySession, PtySize, SessionLedger, ShellError,
    ShellProvider, ShellTarget,
};
use swiss_host::token::{single_token_manager, TokenManager};
use swiss_mcp::calls::CallLog;
use swiss_mcp::registry::Registry;

const TOKEN: &str = "test-terminal-token-0123456789";

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// --- the fake shell -------------------------------------------------------------------------------

/// The far side of one fake session: what the test writes output into and reads
/// keystrokes out of (the same shape swiss-terminal's own session tests use).
struct FarSide {
    out: PtyOut,
    input: PtyIn,
}

impl FarSide {
    async fn say(&self, text: &str) {
        self.out
            .send(PtyEvent::Data(text.as_bytes().to_vec()))
            .await
            .expect("the session is listening");
    }

    async fn exit(&self, code: i32) {
        let _ = self.out.send(PtyEvent::Exit { code: Some(code) }).await;
    }

    /// The next thing the session typed, or None after five seconds.
    async fn heard(&mut self) -> Option<PtyInput> {
        tokio::time::timeout(Duration::from_secs(5), self.input.next())
            .await
            .ok()
            .flatten()
    }
}

struct FakeShells {
    ledger: Arc<SessionLedger>,
    far: mpsc::UnboundedSender<FarSide>,
}

#[async_trait]
impl ShellProvider for FakeShells {
    fn list(&self) -> Vec<ShellTarget> {
        vec![ShellTarget {
            id: "box-one".into(),
            label: "the one box".into(),
            host: "box-one.example".into(),
            port: 22,
            username: "dev".into(),
            state: "connected".into(),
        }]
    }

    async fn open(&self, id: &str, holder: &str, size: PtySize) -> Result<PtySession, ShellError> {
        let lease = self.ledger.grant(id, holder);
        let (session, endpoint) = PtySession::duplex(format!("remote-{id}"), id, size, lease);
        let (out, input) = endpoint.split();
        let _ = self.far.send(FarSide { out, input });
        Ok(session)
    }
}

// --- the rig: the boot sequence in miniature, served on a real port -------------------------------

struct Rig {
    app: axum::Router,
    base: String,
    far: mpsc::UnboundedReceiver<FarSide>,
}

/// Pin the test binary's data dir (the terminal plugin's start mkdirs
/// data_path("terminal"); without this a test would touch the real home) and install
/// the deterministic master key every sealed state file in this repo's tests uses.
fn pin_home_and_key() {
    let _ = swiss_core::paths::test_home();
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| unsafe {
        std::env::set_var(swiss_core::secure::key::MASTER_KEY_ENV_SWISS, "cd".repeat(32))
    });
}

/// Timers off, recording off — the defaults for anything not about time or recording
/// (the same "quiet" shape swiss-terminal's session tests use).
fn quiet() -> Value {
    json!({ "idleTimeoutMinutes": 0, "stallSeconds": 0, "recording": false })
}

/// The full composition with the terminal plugin registered and its routes mounted.
/// Tunnels stays disabled so its provider does not hold the shell seat the fake needs.
async fn rig(tag: &str, terminal_config: Value) -> Rig {
    pin_home_and_key();
    let dir = std::env::temp_dir().join(format!(
        "swiss-terminal-ws-{tag}-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("scratch directory");

    let raw = json!({
        "plugins": {
            "tunnels": { "disabled": true },
            "terminal": { "config": terminal_config }
        }
    });
    // memory() already returns an Arc; one more wrapper would not unwrap anywhere.
    let config_store = ConfigStore::memory(raw);

    let calls = Arc::new(CallLog::at(dir.join("calls")));
    let registry = Registry::new(3_600_000, calls.clone());
    let managed = Arc::new(ManagedStore::open_at(dir.join("managed.json")));

    let services = swiss_host::services::RuntimeServices::new();
    let (far_tx, far_rx) = mpsc::unbounded_channel();
    services
        .shells
        .register(
            Arc::new(FakeShells {
                ledger: SessionLedger::new(),
                far: far_tx,
            }),
            "tunnels",
        )
        .expect("the shell seat was free (tunnels is disabled)");

    let jobs = swiss_jobs::jobs::JobSystem::open(
        dir.join("jobs.json"),
        services.clone(),
        config_store.clone(),
    );
    let tunnel_store = Arc::new(Mutex::new(swiss_tunnels::tunnel::TunnelStore::new(
        dir.join("tunnels.json"),
        19997,
    )));
    let tunnel_manager = swiss_tunnels::tunnel::TunnelManager::new(tunnel_store.clone(), None);
    let tunnels = Arc::new(swiss_tunnels::tunnel::api::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        mcp_display: None,
    });

    let ctx = swiss::app::AppContext::new(
        registry.clone(),
        Arc::new(single_token_manager(TOKEN)) as Arc<TokenManager>,
        managed.clone(),
        calls,
        Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
        "SWISS_TOKEN",
        19997,
    );
    let _ = ctx.catalog.set(services.catalog.clone());

    let terminal_state = swiss::plugins::terminal_api::TerminalState::new();
    let mut host = swiss_host::host::PluginHost::new(config_store);
    let deps = swiss::builtin::BuiltinDeps {
        registry,
        managed,
        jobs: jobs.clone(),
        tunnels: tunnels.clone(),
        tunnel_manager,
        services: services.clone(),
    };
    swiss::builtin::register_all(&mut host, &deps).expect("the built-ins register");
    host.register(Arc::new(swiss::plugins::terminal::TerminalPlugin::new(
        services.clone(),
        terminal_state.clone(),
    )))
    .expect("the terminal plugin registers");
    let host = Arc::new(host);
    host.start_enabled().await;
    assert!(ctx.plugin_host.set(host.clone()).is_ok(), "host set once");

    let extra = swiss_tunnels::tunnel::api::mount(tunnels)
        .merge(swiss_jobs::jobs::api::mount(jobs))
        .merge(swiss_host::services::api::mount(services))
        .merge(swiss::plugins::terminal_api::mount(terminal_state));
    let app = swiss::app::build_app(ctx, Some(extra));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    let served = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served).await;
    });
    Rig {
        app,
        base: format!("ws://127.0.0.1:{port}"),
        far: far_rx,
    }
}

impl Rig {
    /// A plain HTTP call through the same router the socket is served from.
    async fn http(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Option<Value>, String) {
        let req = match body {
            Some(value) => Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:19999")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&value).expect("serializes")))
                .expect("a request"),
            None => Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:19999")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .expect("a request"),
        };
        let response = self.app.clone().oneshot(req).await.expect("router answers");
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

    /// Open a session on the fake target; returns (id, ticket).
    async fn open(&self) -> (String, String) {
        let (status, body, text) = self
            .http(
                "POST",
                "/api/terminal/sessions",
                Some(json!({ "target": "box-one", "cols": 80, "rows": 24 })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let body = body.expect("JSON");
        (
            body["id"].as_str().expect("an id").to_string(),
            body["ticket"].as_str().expect("a ticket").to_string(),
        )
    }

    /// The far side of the session that was just opened.
    fn far(&mut self) -> FarSide {
        self.far.try_recv().expect("the fake shell was opened")
    }

    /// A real WebSocket handshake against the stream route.
    async fn connect(
        &self,
        suffix: &str,
    ) -> Result<(Ws, axum::http::Response<Option<Vec<u8>>>), tungstenite::Error> {
        let url = format!("{}{suffix}", self.base);
        tokio_tungstenite::connect_async(url).await
    }

    async fn connect_session(
        &self,
        id: &str,
        ticket: &str,
    ) -> Result<(Ws, axum::http::Response<Option<Vec<u8>>>), tungstenite::Error> {
        self.connect(&format!(
            "/api/terminal/sessions/{id}/stream?ticket={ticket}"
        ))
        .await
    }
}

/// The status and body a refused handshake came back with.
fn refusal(err: tungstenite::Error) -> (StatusCode, String) {
    let tungstenite::Error::Http(resp) = &err else {
        panic!("expected an HTTP refusal, got: {err}");
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).expect("a known status");
    let body = resp
        .body()
        .clone()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    (status, body)
}

/// The next client frame, or a panic naming what was being waited for.
async fn recv(ws: &mut Ws, what: &str) -> Message {
    tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("the socket is still open")
        .expect("a valid frame")
}

/// Wait until the session listing reports the session detached (the server side has
/// noticed the client going away — the precondition for the grace window).
async fn wait_detached(rig: &Rig, id: &str) {
    for _ in 0..500 {
        let (_, body, _) = rig.http("GET", "/api/terminal/sessions", None).await;
        let detached = body
            .and_then(|v| v.as_array().cloned())
            .map(|rows| {
                rows.iter()
                    .any(|r| r["id"] == id && r["attached"] == json!(false))
            })
            .unwrap_or(false);
        if detached {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the session never reported itself detached");
}

// --- the routes -----------------------------------------------------------------------------------

#[tokio::test]
async fn the_enabled_plugin_lists_targets_and_local_is_off() {
    let mut rig = rig("targets", quiet()).await;
    let (status, body, text) = rig.http("GET", "/api/terminal/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body = body.expect("JSON");
    // The local switch is OFF by default (SPEC §terminal.config) and the reported program is
    // the truth about this host's real shell seam (the plugin builds the real
    // LocalShells; no test override exists, by design).
    assert_eq!(body["local"]["enabled"], json!(false));
    assert!(
        !body["local"]["shell"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "the local program must be the host's own default shell"
    );
    // The fake provider holds the seat: serving, with its one host listed.
    assert_eq!(body["remote"]["presence"], json!("serving"));
    let target = &body["remote"]["targets"][0];
    assert_eq!(target["id"], json!("box-one"));
    assert_eq!(target["host"], json!("box-one.example"));
    assert_eq!(target["username"], json!("dev"));
    assert!(body["remote"].get("reason").is_none());
    let _ = rig.far.try_recv(); // nothing opened; just proving the channel idles
}

#[tokio::test]
async fn the_disabled_terminal_plugin_answers_the_structured_503() {
    pin_home_and_key();
    // The boundary's 503, not one of our own: the routes vanish behind the plugin's
    // state, exactly as they do for every other plugin-owned tree.
    let raw = json!({
        "plugins": {
            "tunnels": { "disabled": true },
            "terminal": { "disabled": true, "config": {} }
        }
    });
    // Same boot as rig(), with the terminal row disabled.
    let rig = disabled_rig("disabled", raw).await;
    let (status, _, text) = rig.http("GET", "/api/terminal/targets", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert!(text.contains("terminal plugin is disabled"), "{text}");
    assert!(text.contains("/api/plugins/terminal/enable"), "{text}");
    // And the socket route is behind the same boundary, before any ticket logic.
    let (status, _, text) = rig
        .http(
            "POST",
            "/api/terminal/sessions",
            Some(json!({ "target": "box-one", "cols": 80, "rows": 24 })),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
}

#[tokio::test]
async fn cols_out_of_range_is_refused_not_clamped() {
    let rig = rig("cols", quiet()).await;
    // SPEC §terminal.api: a 0-column PTY is undefined behaviour on the far side; out of range
    // is a 400 naming the axis, never a silent default.
    let (status, body, text) = rig
        .http(
            "POST",
            "/api/terminal/sessions",
            Some(json!({ "target": "box-one", "cols": 0, "rows": 24 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("cols must be between 1 and 1000"), "{text}");
    let (status, _, text) = rig
        .http(
            "POST",
            "/api/terminal/sessions",
            Some(json!({ "target": "box-one", "cols": 80, "rows": 0 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("rows must be between 1 and 1000"), "{text}");
    // Past u16: rejected as a number, not wrapped by a cast.
    let (status, _, text) = rig
        .http(
            "POST",
            "/api/terminal/sessions",
            Some(json!({ "target": "box-one", "cols": 70000, "rows": 24 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("got 70000"), "{text}");
    let _ = body;
}

#[tokio::test]
async fn a_ticket_route_answers_404_for_a_missing_session() {
    let rig = rig("ticket404", quiet()).await;
    let (status, _, text) = rig
        .http("POST", "/api/terminal/sessions/deadbeef/ticket", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert!(text.contains("no such terminal session"), "{text}");
}

#[tokio::test]
async fn a_good_ticket_upgrades_and_bytes_flow_both_ways() {
    let mut rig = rig("pump", quiet()).await;
    let (id, ticket) = rig.open().await;
    let mut far = rig.far();

    let (mut ws, response) = rig.connect_session(&id, &ticket).await.expect("upgrades");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    // Output arrives as raw binary — no base64, no JSON wrapping (SPEC §terminal.api).
    far.say(
        "hello
",
    )
    .await;
    let message = recv(&mut ws, "the shell's greeting").await;
    match message {
        Message::Binary(data) => assert_eq!(
            data.as_ref(),
            b"hello
"
        ),
        other => panic!("expected binary output, got {other:?}"),
    }

    // Keystrokes go the other way the same way.
    ws.send(Message::Binary(b"ls\r".to_vec().into()))
        .await
        .expect("sends");
    match far.heard().await.expect("the keystroke arrived") {
        PtyInput::Data(bytes) => assert_eq!(bytes, b"ls\r"),
        other => panic!("expected input data, got {other:?}"),
    }

    // A resize text frame reaches the same resize the HTTP channel uses.
    ws.send(Message::Text(
        r#"{"t":"resize","cols":100,"rows":30}"#.into(),
    ))
    .await
    .expect("sends the resize");
    match far.heard().await.expect("the resize arrived") {
        PtyInput::Resize(size) => {
            assert_eq!(size, PtySize::new(100, 30).expect("legal"));
        }
        other => panic!("expected a resize, got {other:?}"),
    }

    // An unknown control object is ignored, not fatal (SPEC §terminal.api) — the next real
    // frame still flows after the junk one.
    ws.send(Message::Text(r#"{"t":"ping"}"#.into()))
        .await
        .expect("sends junk");
    far.say("still here\r").await;
    let message = recv(&mut ws, "output after junk control frame").await;
    match message {
        Message::Binary(data) => assert_eq!(data.as_ref(), b"still here\r"),
        other => panic!("expected binary output, got {other:?}"),
    }

    // The shell exits: the code arrives as text, then the socket closes.
    far.exit(0).await;
    let message = recv(&mut ws, "the exit frame").await;
    match message {
        Message::Text(text) => {
            let parsed: Value = serde_json::from_str(text.as_str()).expect("one JSON object");
            assert_eq!(parsed["t"], json!("exit"));
            assert_eq!(parsed["code"], json!(0));
        }
        other => panic!("expected the exit text frame, got {other:?}"),
    }
    let end = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    match end {
        Ok(None) | Ok(Some(Ok(Message::Close(_)))) => {}
        // A transport error here is the close handshake racing TCP teardown (the
        // server drops the socket right after its Close frame): the exit frame was
        // delivered, which is what the user actually sees.
        Ok(Some(Err(_))) => {}
        Ok(Some(Ok(other))) => panic!("nothing after the exit frame, got {other:?}"),
        Err(_) => panic!("the socket never closed after the exit frame"),
    }
}

#[tokio::test]
async fn a_ticket_borrowed_from_another_session_is_refused() {
    let rig = rig("wrong-session", quiet()).await;
    let (id_a, ticket_a) = rig.open().await;
    let (id_b, _ticket_b) = rig.open().await;

    let err = rig
        .connect_session(&id_b, &ticket_a)
        .await
        .expect_err("a ticket for another session must not upgrade");
    let (status, body) = refusal(err);
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("issued for a different session"), "{body}");
    let _ = id_a;
}

#[tokio::test]
async fn a_replayed_ticket_is_refused() {
    let rig = rig("replay", quiet()).await;
    let (id, ticket) = rig.open().await;

    let (_ws, response) = rig.connect_session(&id, &ticket).await.expect("upgrades");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    // The replay of a captured query string buys nothing (single use).
    let err = rig
        .connect_session(&id, &ticket)
        .await
        .expect_err("a replayed ticket must not upgrade");
    let (status, body) = refusal(err);
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("already been used"), "{body}");
}

#[tokio::test(start_paused = true)]
async fn an_expired_ticket_is_refused() {
    // The 10-second TTL on the paused clock (real sockets still work — the handshake
    // is I/O, not timers). Grace is a DAY because the paused clock creeps forward a
    // second at a time on every idle park (the jobs tick is always the nearest timer),
    // and a never-attached session is on the grace clock from birth: under parallel
    // load that creep can pass 60 seconds during the TCP handshake, killing the
    // session — and a dead session forgets its tickets, which turns this into the
    // wrong refusal. One day is unreachable by creep; 11 seconds still expires the
    // ticket.
    let mut config = quiet();
    config["graceSeconds"] = json!(86_400);
    let rig = rig("expired", config).await;
    let (id, ticket) = rig.open().await;
    tokio::time::advance(Duration::from_secs(11)).await;
    let err = rig
        .connect_session(&id, &ticket)
        .await
        .expect_err("an expired ticket must not upgrade");
    let (status, body) = refusal(err);
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("expired"), "{body}");
}

#[tokio::test]
async fn a_stream_without_a_ticket_is_a_400() {
    let rig = rig("no-ticket", quiet()).await;
    let (id, _ticket) = rig.open().await;
    let err = rig
        .connect(&format!("/api/terminal/sessions/{id}/stream"))
        .await
        .expect_err("no ticket, no socket");
    let (status, body) = refusal(err);
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("a one-time ticket is required"), "{body}");
}

#[tokio::test]
async fn a_reconnect_within_the_grace_picks_up_the_catch_up() {
    let mut rig = rig("reconnect", quiet()).await;
    let (id, ticket) = rig.open().await;
    let far = rig.far();

    let (mut ws, response) = rig.connect_session(&id, &ticket).await.expect("upgrades");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    far.say(
        "before the drop
",
    )
    .await;
    match recv(&mut ws, "output before the drop").await {
        Message::Binary(data) => assert_eq!(
            data.as_ref(),
            b"before the drop
"
        ),
        other => panic!("expected binary output, got {other:?}"),
    }

    // The laptop lid: the socket dies, the session must not (SPEC §terminal.sessions).
    drop(ws);
    wait_detached(&rig, &id).await;

    // Output produced while nobody is watching lands in the bounded catch-up buffer.
    far.say(
        "caught up while away
",
    )
    .await;

    // The reconnect flow T5 exists to serve: a FRESH ticket (the old one is long
    // spent), then the socket.
    let (status, body, text) = rig
        .http("POST", &format!("/api/terminal/sessions/{id}/ticket"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let fresh = body.expect("JSON")["ticket"]
        .as_str()
        .expect("a ticket")
        .to_string();
    let (mut ws, response) = rig.connect_session(&id, &fresh).await.expect("re-upgrades");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    match recv(&mut ws, "the catch-up bytes").await {
        Message::Binary(data) => {
            let text = String::from_utf8_lossy(&data).into_owned();
            assert!(text.contains("caught up while away"), "{text:?}");
        }
        other => panic!("expected the catch-up as binary, got {other:?}"),
    }
    far.exit(0).await;
    let _ = recv(&mut ws, "the exit frame").await;
}

#[tokio::test]
async fn resize_over_the_redundant_http_channel_reaches_the_session() {
    let mut rig = rig("http-resize", quiet()).await;
    let (id, _ticket) = rig.open().await;
    let mut far = rig.far();

    let (status, _, text) = rig
        .http(
            "POST",
            &format!("/api/terminal/sessions/{id}/resize"),
            Some(json!({ "cols": 132, "rows": 43 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    match far.heard().await.expect("the resize arrived") {
        PtyInput::Resize(size) => {
            assert_eq!(size, PtySize::new(132, 43).expect("legal"));
        }
        other => panic!("expected a resize, got {other:?}"),
    }

    // The redundant channel is still the strict one: cols out of range is a 400.
    let (status, _, text) = rig
        .http(
            "POST",
            &format!("/api/terminal/sessions/{id}/resize"),
            Some(json!({ "cols": 0, "rows": 43 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
}

#[tokio::test]
async fn delete_closes_the_session_and_the_listing_empties() {
    let mut rig = rig("delete", quiet()).await;
    let (id, _ticket) = rig.open().await;
    let mut far = rig.far();

    let (status, _, text) = rig
        .http("DELETE", &format!("/api/terminal/sessions/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // The far side ends: the session's PTY is dropped, which closes the fake's queues.
    let mut saw_end = false;
    for _ in 0..200 {
        if matches!(
            tokio::time::timeout(Duration::from_millis(25), far.input.next()).await,
            Ok(None)
        ) {
            saw_end = true;
            break;
        }
    }
    assert!(saw_end, "the session never closed behind the fake");

    for _ in 0..200 {
        let (_, body, _) = rig.http("GET", "/api/terminal/sessions", None).await;
        if body.map(|v| v.as_array().map(Vec::is_empty).unwrap_or(false)) == Some(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the session never left the listing");
}

#[tokio::test]
async fn the_session_listing_is_camel_case_and_counts_bytes() {
    let mut rig = rig("listing", quiet()).await;
    let (id, _ticket) = rig.open().await;
    let far = rig.far();

    far.say("count me").await;
    for _ in 0..300 {
        let (_, body, _) = rig.http("GET", "/api/terminal/sessions", None).await;
        let listed = body.and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let row = listed.iter().find(|r| r["id"] == json!(id)).cloned();
        if let Some(row) = row {
            if row["bytesOut"].as_u64().unwrap_or(0) >= 8 {
                assert_eq!(row["target"], json!("box-one"));
                assert_eq!(row["label"], json!("the one box"));
                assert_eq!(row["attached"], json!(false));
                assert_eq!(row["recording"], Value::Null);
                assert!(row["opened"].as_str().is_some(), "an ISO timestamp");
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the listing never counted the bytes");
}

#[tokio::test]
async fn a_recording_is_written_when_configured() {
    // recording defaults ON; this rig turns it back on explicitly to pin the open
    // response's recording field against a real file.
    let mut rig = rig("recording", json!({ "recording": true })).await;
    let (status, body, text) = rig
        .http(
            "POST",
            "/api/terminal/sessions",
            Some(json!({ "target": "box-one", "cols": 80, "rows": 24 })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let body = body.expect("JSON");
    let path = std::path::PathBuf::from(body["recording"].as_str().expect("a recording path"));
    assert!(path.is_file(), "the cast file exists: {}", path.display());
    let _ = rig.far();
}

#[tokio::test]
async fn stopping_the_plugin_closes_live_sessions_with_a_visible_reason() {
    let mut rig = rig("stop", quiet()).await;
    let (id, ticket) = rig.open().await;
    let _far = rig.far();

    let (mut ws, response) = rig.connect_session(&id, &ticket).await.expect("upgrades");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    // Disable through the real admin route: the boundary takes the routes away and the
    // instance's stop closes the session — the client must be TOLD, not dropped.
    let (status, _, text) = rig
        .http("POST", "/api/plugins/terminal/disable", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let mut saw_reason = false;
    let mut saw_error = false;
    for _ in 0..3 {
        let message = match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(None) | Ok(Some(Err(_))) => break,
            Err(_) => panic!("timed out waiting for the close frames"),
        };
        match message {
            Message::Binary(data) => {
                let line = String::from_utf8_lossy(&data).into_owned();
                if line.contains("the terminal plugin is stopping") {
                    saw_reason = true;
                }
            }
            Message::Text(text) => {
                let parsed: Value = serde_json::from_str(text.as_str()).expect("JSON");
                if parsed["t"] == json!("error")
                    && parsed["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("the terminal plugin is stopping"))
                {
                    saw_error = true;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    assert!(saw_reason, "a visible [gateway] line before the close");
    assert!(saw_error, "an error frame naming the stop");

    // And every /api/terminal route is now behind the structured 503.
    let (status, _, text) = rig.http("GET", "/api/terminal/targets", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
}

// --- the disabled rig ----------------------------------------------------------------------------

/// rig() with the terminal row disabled — kept separate so the common path stays
/// readable; the config arrives fully formed because the disabled variant is the one
/// difference.
async fn disabled_rig(tag: &str, raw: Value) -> Rig {
    pin_home_and_key();
    let dir = std::env::temp_dir().join(format!(
        "swiss-terminal-ws-{tag}-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    // memory() already returns an Arc; one more wrapper would not unwrap anywhere.
    let config_store = ConfigStore::memory(raw);

    let calls = Arc::new(CallLog::at(dir.join("calls")));
    let registry = Registry::new(3_600_000, calls.clone());
    let managed = Arc::new(ManagedStore::open_at(dir.join("managed.json")));

    let services = swiss_host::services::RuntimeServices::new();
    let jobs = swiss_jobs::jobs::JobSystem::open(
        dir.join("jobs.json"),
        services.clone(),
        config_store.clone(),
    );
    let tunnel_store = Arc::new(Mutex::new(swiss_tunnels::tunnel::TunnelStore::new(
        dir.join("tunnels.json"),
        19997,
    )));
    let tunnel_manager = swiss_tunnels::tunnel::TunnelManager::new(tunnel_store.clone(), None);
    let tunnels = Arc::new(swiss_tunnels::tunnel::api::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        mcp_display: None,
    });

    let ctx = swiss::app::AppContext::new(
        registry.clone(),
        Arc::new(single_token_manager(TOKEN)) as Arc<TokenManager>,
        managed.clone(),
        calls,
        Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
        "SWISS_TOKEN",
        19997,
    );
    let _ = ctx.catalog.set(services.catalog.clone());

    let terminal_state = swiss::plugins::terminal_api::TerminalState::new();
    let mut host = swiss_host::host::PluginHost::new(config_store);
    let deps = swiss::builtin::BuiltinDeps {
        registry,
        managed,
        jobs: jobs.clone(),
        tunnels: tunnels.clone(),
        tunnel_manager,
        services: services.clone(),
    };
    swiss::builtin::register_all(&mut host, &deps).expect("the built-ins register");
    host.register(Arc::new(swiss::plugins::terminal::TerminalPlugin::new(
        services.clone(),
        terminal_state.clone(),
    )))
    .expect("the terminal plugin registers");
    let host = Arc::new(host);
    host.start_enabled().await;
    assert!(ctx.plugin_host.set(host.clone()).is_ok(), "host set once");

    let extra = swiss_tunnels::tunnel::api::mount(tunnels)
        .merge(swiss_jobs::jobs::api::mount(jobs))
        .merge(swiss_host::services::api::mount(services))
        .merge(swiss::plugins::terminal_api::mount(terminal_state));
    let app = swiss::app::build_app(ctx, Some(extra));

    // No shell provider and no real socket needed — the boundary answers first.
    Rig {
        app,
        base: String::new(),
        far: mpsc::unbounded_channel().1,
    }
}
