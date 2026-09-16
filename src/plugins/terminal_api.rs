/*
 * Copyright 2026 The swiss authors
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

//! The /api/terminal routes (docs/14 §8) — the HTTP and WebSocket half of the session
//! machine that lives in [swiss_terminal] (docs/14 T4).
//!
//! Mounted through the extra tree in server.rs, which is the whole reason this file sits
//! in the composition crate and not in swiss-terminal: build_app layers the loopback guard
//! and the plugin boundary over the extra tree at mount time, and axum's layer() only
//! covers routes that exist when it is called — merging anything later would leave every
//! /api/terminal route outside the only auth this tree has. Keeping swiss-terminal free of
//! axum is also what lets its 43 session tests run with no server, no socket, no timing.
//!
//! ## State: one slot the plugin lifecycle owns
//!
//! [TerminalState] is a single shared slot following the ctx.tunnel_links precedent: the
//! routes are mounted once at boot, but the [TerminalSessions] they drive exists only
//! while the terminal plugin is serving — start installs it, stop withdraws it. In the
//! serving gateway the plugin boundary answers the structured 503 before any handler
//! here can observe an empty slot; the handlers check anyway, because a mounted router
//! must never assume who mounted it.
//!
//! ## The socket pump, and where ownership has to sit
//!
//! One WebSocket per attach, split into two independent halves (the axum chat pattern):
//!
//! - the WRITE half owns the sink plus the attachment's frame receiver and stalled
//!   watcher. It is the only place the attachment exists, so when it returns — client
//!   gone, socket unwritable, or the driver closed — the attachment halves drop and the
//!   session's reconnect grace window STARTS (docs/14 §6.7). Leaking them into a
//!   longer-lived task would tell the driver its client is still attached.
//! - the READ half owns the stream, because keystrokes must keep flowing while the write
//!   half is parked on a client that stopped reading. That independence IS the
//!   backpressure design (docs/14 §6.8), not an optimization: a Ctrl-C typed into a
//!   flooding terminal has to get through.
//!
//! There is no extra outbound buffer on purpose: the queue that matters is the driver's
//! own (CLIENT_QUEUE_FRAMES), and its stall flag stays honest only while this side parks
//! rather than absorbs.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use futures_util::sink::SinkExt;
use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};

use swiss_host::reply::{admin_error, admin_json, NodeBody};
use swiss_host::services::shell::{PtySize, MAX_PTY_AXIS, MIN_PTY_AXIS};
use swiss_terminal::terminal::{Attachment, ClientFrame, TerminalError, TerminalSessions};

/// The /api/terminal router's state: the seat the terminal plugin's instance sits in
/// while it serves. Constructed once by the boot sequence, handed both to the plugin
/// factory (which fills it) and to [mount] (which reads it).
pub struct TerminalState {
    sessions: RwLock<Option<Arc<TerminalSessions>>>,
}

impl TerminalState {
    pub fn new() -> Arc<Self> {
        Arc::new(TerminalState {
            sessions: RwLock::new(None),
        })
    }

    /// The plugin's start: publish the session machine the routes will talk to.
    pub fn install(&self, sessions: Arc<TerminalSessions>) {
        let mut slot = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(sessions);
    }

    /// The plugin's stop: take the machine back so no request can reach a dying one.
    /// Returns it — the caller still owns the shutdown.
    pub fn withdraw(&self) -> Option<Arc<TerminalSessions>> {
        self.sessions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    fn live(&self) -> Option<Arc<TerminalSessions>> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Mount the /api/terminal tree. The caller merges this into the extra tree BEFORE
/// build_app folds it under the loopback guard and the plugin boundary — see the module
/// note for why merging it anywhere else would be an auth hole.
pub fn mount(state: Arc<TerminalState>) -> Router<()> {
    Router::new()
        .route("/api/terminal/targets", get(targets))
        .route(
            "/api/terminal/sessions",
            post(open_session).get(list_sessions),
        )
        .route("/api/terminal/sessions/{id}/ticket", post(mint_ticket))
        .route("/api/terminal/sessions/{id}/stream", get(stream))
        .route("/api/terminal/sessions/{id}/resize", post(resize_session))
        .route("/api/terminal/sessions/{id}", delete(close_session))
        .with_state(state)
}

/// The one error funnel: the session machine's variants already carry user-facing text,
/// so this picks a status code and passes the message through verbatim — rewriting it
/// here would fork the wording the panel shows (NoSession 404 / Ticket 403 / Refused
/// 409 / Shell 502).
fn fail(err: &TerminalError) -> Response {
    let status = match err {
        TerminalError::NoSession => StatusCode::NOT_FOUND,
        TerminalError::Ticket(_) => StatusCode::FORBIDDEN,
        TerminalError::Refused(_) => StatusCode::CONFLICT,
        TerminalError::Shell(_) => StatusCode::BAD_GATEWAY,
    };
    admin_error(status, &err.to_string())
}

/// The defensive branch: the boundary should have answered this, but a mounted router
/// never assumes its mounter.
fn not_running() -> Response {
    admin_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "the terminal plugin is not running",
    )
}

/// A view the session machine already derives Serialize for, or a 500 naming it. These
/// structs are plain data, so failure is not expected — but a type with a serde typo
/// would otherwise surface as a panic instead of an answer.
fn view_json(view: impl serde::Serialize) -> Response {
    match serde_json::to_value(view) {
        Ok(value) => admin_json(StatusCode::OK, value),
        Err(err) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the terminal view could not be serialized: {err}"),
        ),
    }
}

/// One PTY axis, rejected as a 400 rather than clamped (docs/14 §8: a 0-column PTY is
/// undefined behaviour on the far side, so out of range is a refusal, not a default).
/// Checked as u64 BEFORE the cast — 70000 as u16 would wrap straight past the range.
fn axis(value: Option<&Value>, name: &str) -> Result<u16, String> {
    let Some(raw) = value else {
        return Err(format!("{name} is required"));
    };
    let number = raw.as_u64().ok_or_else(|| {
        format!("{name} must be a whole number between {MIN_PTY_AXIS} and {MAX_PTY_AXIS}")
    })?;
    if !((MIN_PTY_AXIS as u64)..=(MAX_PTY_AXIS as u64)).contains(&number) {
        return Err(format!(
            "{name} must be between {MIN_PTY_AXIS} and {MAX_PTY_AXIS}, got {number}"
        ));
    }
    Ok(number as u16)
}

/// cols/rows out of a request body, through the same bounds PtySize enforces.
fn geometry(body: &Value) -> Result<PtySize, String> {
    let cols = axis(body.get("cols"), "cols")?;
    let rows = axis(body.get("rows"), "rows")?;
    // Infallible after axis(), but PtySize::new stays the single authority for the bound.
    PtySize::new(cols, rows)
}

// --- the plain routes ---------------------------------------------------------------------------

async fn targets(State(t): State<Arc<TerminalState>>) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    view_json(sessions.targets())
}

async fn list_sessions(State(t): State<Arc<TerminalState>>) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    view_json(sessions.list())
}

async fn open_session(State(t): State<Arc<TerminalState>>, body: NodeBody) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    let Some(obj) = body.0.as_object() else {
        return admin_error(StatusCode::BAD_REQUEST, "the body must be a JSON object");
    };
    for key in obj.keys() {
        if !matches!(key.as_str(), "target" | "cols" | "rows" | "shell") {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown field {key:?} (known: target, cols, rows, shell)"),
            );
        }
    }
    let target = match obj
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(target) => target.to_string(),
        None => {
            return admin_error(
                StatusCode::BAD_REQUEST,
                "target: a non-empty string is required",
            )
        }
    };
    let size = match geometry(&body.0) {
        Ok(size) => size,
        Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
    };
    let shell = match obj.get("shell") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(_) => return admin_error(StatusCode::BAD_REQUEST, "shell: must be a string"),
    };
    match sessions.open(&target, size, shell.as_deref()).await {
        Ok(opened) => match serde_json::to_value(opened) {
            Ok(value) => admin_json(StatusCode::CREATED, value),
            Err(err) => admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("the open response could not be serialized: {err}"),
            ),
        },
        Err(err) => fail(&err),
    }
}

/// The reconnect half of the ticket flow — the one route docs/14 §8 does not list. A
/// ticket lives TICKET_TTL (10 s) and a grace window lives graceSeconds (60 s), so the
/// ticket minted at open can never serve a reconnect: the panel must mint a fresh one
/// here, then open the socket with it.
async fn mint_ticket(State(t): State<Arc<TerminalState>>, Path(id): Path<String>) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    match sessions.ticket(&id) {
        Ok(ticket) => admin_json(StatusCode::OK, json!({ "ticket": ticket })),
        Err(err) => fail(&err),
    }
}

async fn resize_session(
    State(t): State<Arc<TerminalState>>,
    Path(id): Path<String>,
    body: NodeBody,
) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    let Some(obj) = body.0.as_object() else {
        return admin_error(StatusCode::BAD_REQUEST, "the body must be a JSON object");
    };
    for key in obj.keys() {
        if !matches!(key.as_str(), "cols" | "rows") {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown field {key:?} (known: cols, rows)"),
            );
        }
    }
    let size = match geometry(&body.0) {
        Ok(size) => size,
        Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
    };
    // The redundant channel on purpose (docs/14 §8): during a reconnect gap there is no
    // socket to send a resize frame on, but the panel's window already changed.
    match sessions.resize(&id, size).await {
        Ok(()) => admin_json(StatusCode::OK, json!({ "id": id, "resized": true })),
        Err(err) => fail(&err),
    }
}

async fn close_session(State(t): State<Arc<TerminalState>>, Path(id): Path<String>) -> Response {
    let Some(sessions) = t.live() else {
        return not_running();
    };
    match sessions.close(&id).await {
        Ok(()) => admin_json(StatusCode::OK, json!({ "id": id, "closed": true })),
        Err(err) => fail(&err),
    }
}

// --- the WebSocket -------------------------------------------------------------------------------

/// The stream route. The upgrade extractor is taken as a Result so a handshake that
/// cannot upgrade answers the standard rejection BEFORE a ticket is spent — a refused
/// handshake must not cost the client the ticket it minted — and so ticket verdicts
/// (403/404) are decided here, ahead of the 101.
async fn stream(
    State(t): State<Arc<TerminalState>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(upgrade) = ws else {
        return ws.unwrap_err().into_response();
    };
    let Some(sessions) = t.live() else {
        return not_running();
    };
    let Some(ticket) = q.get("ticket").map(|t| t.trim()).filter(|s| !s.is_empty()) else {
        return admin_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "a one-time ticket is required: POST /api/terminal/sessions/{id}/ticket, then pass it as ?ticket="
            ),
        );
    };
    // attach spends the ticket and joins the session's output (a fan-out: a second
    // panel tab attaching must not blind the first). Doing this before
    // on_upgrade (rather than inside the upgraded callback) is what lets a wrong,
    // expired or replayed ticket be refused as an HTTP status the panel can show,
    // instead of as a socket that opens and immediately dies.
    match sessions.attach(&id, ticket).await {
        Ok(attachment) => {
            upgrade.on_upgrade(move |socket| pump(socket, attachment, sessions, id))
        }
        Err(err) => fail(&err),
    }
}

/// One attached socket: pump until either side is done. See the module note for why the
/// halves are split the way they are.
async fn pump(
    socket: WebSocket,
    attachment: Attachment,
    sessions: Arc<TerminalSessions>,
    id: String,
) {
    let (sink, stream) = socket.split();
    let Attachment { frames, stalled } = attachment;
    let (gone, gone_rx) = oneshot::channel::<()>();
    let reader = tokio::spawn(read_side(stream, sessions, id, gone));
    // Await the write side IN line: when it returns, the attachment halves it owns drop
    // (the grace window starts), and the reader — now pointless — is aborted.
    write_side(sink, frames, stalled, gone_rx).await;
    reader.abort();
    let _ = reader.await;
}

/// The outbound half: driver frames and stall edges to the client. Owns the
/// attachment's receivers; returning IS the detach.
async fn write_side(
    mut sink: SplitSink<WebSocket, Message>,
    mut frames: mpsc::Receiver<ClientFrame>,
    mut stalled: watch::Receiver<bool>,
    mut gone: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            // Data, exit and error share one ordered channel so an exit is never
            // reported before the last bytes the shell wrote (the session machine's
            // own ordering, preserved straight onto the wire).
            frame = frames.recv() => match frame {
                Some(ClientFrame::Data(bytes)) => {
                    if sink.send(Message::Binary(Bytes::from(bytes))).await.is_err() {
                        break;
                    }
                }
                Some(ClientFrame::Exit { code }) => {
                    let _ = sink
                        .send(Message::Text(control("exit", &[("code", &json!(code))]).into()))
                        .await;
                    break;
                }
                Some(ClientFrame::Error(message)) => {
                    let _ = sink
                        .send(Message::Text(
                            control("error", &[("message", &json!(message))]).into(),
                        ))
                        .await;
                    break;
                }
                None => break, // the driver is gone; the socket has nothing left to say
            },
            // Out of band on purpose: the data channel being stuck is exactly what this
            // reports, so it cannot travel on it. Only true edges arrive — the session
            // machine sends the flag with send_if_modified.
            edge = stalled.changed() => {
                if edge.is_ok()
                    && *stalled.borrow_and_update()
                    && sink.send(Message::Text(control("stalled", &[]).into()))
                        .await
                        .is_err()
                {
                    break;
                }
                // Err = the driver dropped the flag's sender; frames closing is the real
                // end signal, so fall through and keep serving.
            }
            // The client went away. Dropping the attachment halves on the way out is
            // what starts the grace window — see the module note.
            _ = &mut gone => break,
        }
    }
    // A clean close when the socket still takes one; after Exit/Error this is the frame
    // that tells the panel the socket will not reopen.
    let _ = sink.close().await;
}

/// The inbound half: keystrokes and resize frames to the session. Independent of
/// write_side so a parked outbound send never blocks keystrokes (docs/14 §6.8).
async fn read_side(
    mut stream: SplitStream<WebSocket>,
    sessions: Arc<TerminalSessions>,
    id: String,
    gone: oneshot::Sender<()>,
) {
    while let Some(Ok(message)) = stream.next().await {
        match message {
            // Raw PTY bytes, both directions, no wrapping (docs/14 §8): a terminal can
            // produce megabytes a second and every layer of framing is pure cost.
            Message::Binary(bytes) => {
                if sessions.input(&id, bytes.to_vec()).await.is_err() {
                    break; // the session is gone; nothing left to type into
                }
            }
            Message::Text(text) => {
                if let Some(size) = parse_control(&text) {
                    if sessions.resize(&id, size).await.is_err() {
                        break;
                    }
                }
                // Anything else — unknown t, missing fields, wrong types, absurd
                // geometry — is ignored, strictly. A control frame a client improvised
                // must never crash the pump (docs/14 §8).
            }
            // Ping/Pong are answered by the protocol layer itself; a Close falls
            // through to the None below.
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = gone.send(());
}

/// One control object. Internally tagged, so an unknown t simply fails to decode —
/// which is the ignore rule above. This is the ONLY JSON parsed on the socket path, and
/// it is one small struct per resize, never per byte.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum Control {
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
}

fn parse_control(text: &str) -> Option<PtySize> {
    match serde_json::from_str::<Control>(text) {
        Ok(Control::Resize { cols, rows }) => PtySize::new(cols, rows).ok(),
        Err(_) => None,
    }
}

/// A control object for the client: a t, then the named fields. Built with json! (not
/// hand-formatted) because Error text can carry quotes that would break the framing.
fn control(t: &str, fields: &[(&str, &Value)]) -> String {
    let mut body = json!({ "t": t });
    if let Some(obj) = body.as_object_mut() {
        for (key, value) in fields {
            obj.insert((*key).to_string(), (*value).clone());
        }
    }
    body.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn geometry_is_refused_not_clamped() {
        // docs/14 §8: a 0-column PTY is undefined behaviour on the far side.
        assert!(geometry(&json!({ "cols": 80, "rows": 24 })).is_ok());
        let err = geometry(&json!({ "cols": 0, "rows": 24 })).unwrap_err();
        assert!(err.contains("cols must be between 1 and 1000"), "{err}");
        // A value past u16 would wrap straight through a naive cast — checked as u64.
        let err = geometry(&json!({ "cols": 70000, "rows": 24 })).unwrap_err();
        assert!(err.contains("got 70000"), "{err}");
        let err = geometry(&json!({ "cols": 80 })).unwrap_err();
        assert!(err.contains("rows is required"), "{err}");
        let err = geometry(&json!({ "cols": 1.5, "rows": 24 })).unwrap_err();
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn control_frames_parse_strictly() {
        let size =
            parse_control(r#"{"t":"resize","cols":120,"rows":30}"#).expect("a legal resize decodes");
        assert_eq!(size, PtySize::new(120, 30).expect("legal"));
        // Unknown t, missing fields, wrong types, absurd geometry: all ignored.
        assert!(parse_control(r#"{"t":"ping"}"#).is_none());
        assert!(parse_control(r#"{"t":"resize","cols":120}"#).is_none());
        assert!(parse_control(r#"{"t":"resize","cols":"wide","rows":30}"#).is_none());
        assert!(parse_control(r#"{"t":"resize","cols":0,"rows":30}"#).is_none());
        assert!(parse_control("not json at all").is_none());
    }

    /// docs/15 §2.3: the targets route's local object carries the candidate shells (an
    /// array of {program,label}) and a shell that is a resolved absolute path, so the
    /// panel's `local · …` label and the settings sheet's dropdown are the truth about
    /// this host. Driven through the mounted router with a fixed fake local shell.
    #[tokio::test]
    async fn the_targets_route_lists_local_shells_and_a_resolved_shell() {
        use axum::body::Body;
        use swiss_host::services::shell::{PtySession, PtySize, ShellError, ShellRegistry};
        use swiss_terminal::terminal::{LocalShell, TerminalConfig, TerminalSessions};
        use tower::util::ServiceExt;

        struct FixedLocal;
        impl LocalShell for FixedLocal {
            fn program(&self) -> String {
                r"C:\shells\pwsh.exe".to_string()
            }
            fn candidates(&self) -> Vec<swiss_core::platform::pty::ShellCandidate> {
                vec![
                    swiss_core::platform::pty::ShellCandidate {
                        program: r"C:\shells\pwsh.exe".to_string(),
                        label: "PowerShell 7".to_string(),
                    },
                    swiss_core::platform::pty::ShellCandidate {
                        program: r"C:\Windows\system32\cmd.exe".to_string(),
                        label: "cmd".to_string(),
                    },
                ]
            }
            fn open(
                &self,
                _session_id: &str,
                _size: PtySize,
                _shell: Option<&str>,
            ) -> Result<PtySession, ShellError> {
                Err(ShellError::Failed("this test never opens a session".into()))
            }
        }

        let state = TerminalState::new();
        let dir = std::env::temp_dir().join(format!("swiss-targets-{}", swiss_core::util::random_hex(8)));
        state.install(TerminalSessions::new(
            TerminalConfig::default(),
            Arc::new(ShellRegistry::new()),
            Arc::new(FixedLocal),
            dir,
        ));
        let app = mount(state);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/terminal/targets")
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("a response");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("a body");
        let body: Value = serde_json::from_slice(&bytes).expect("the targets view");
        let shells = body["local"]["shells"]
            .as_array()
            .expect("local.shells is an array");
        assert_eq!(shells.len(), 2, "{shells:?}");
        assert!(shells.iter().all(|s| {
            !s["program"].as_str().unwrap_or("").is_empty() && !s["label"].as_str().unwrap_or("").is_empty()
        }));
        let shell = body["local"]["shell"].as_str().expect("local.shell is a string");
        assert_eq!(shell, r"C:\shells\pwsh.exe");
        assert!(
            std::path::Path::new(shell).is_absolute(),
            "resolved to an absolute path, got {shell}"
        );
        // The old fields are untouched — an older panel still reads them (docs/15 §2.2).
        assert_eq!(body["local"]["enabled"], json!(false));
    }

    #[test]
    fn server_control_objects_are_exact() {
        assert_eq!(control("stalled", &[]), r#"{"t":"stalled"}"#);
        assert_eq!(
            control("exit", &[("code", &json!(0))]),
            r#"{"t":"exit","code":0}"#
        );
        // Message text with quotes must survive as JSON, not break the framing.
        let built = control("error", &[("message", &json!(r#"said "hi""#))]);
        let parsed: Value = serde_json::from_str(&built).expect("still one JSON object");
        assert_eq!(parsed["t"], "error");
        assert_eq!(parsed["message"], r#"said "hi""#);
        assert_eq!(
            control("exit", &[("code", &json!(Option::<i32>::None))]),
            r#"{"t":"exit","code":null}"#
        );
    }
}
