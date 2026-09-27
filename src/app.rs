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

//! The gateway HTTP server — port of `router.ts` (with `http.ts`'s routing contract carried by
//! axum): the panel, health, the management API, and the MCP endpoints under the `/mcp/` prefix
//! as a catch-all single-segment route resolved dynamically from the registry, so paths can be
//! added and removed at runtime. The prefix is the MCP plugin's domain (docs/24, ADR-018): the
//! root stays free for host chrome and whatever a future plugin claims there.
//!
//! The boundary, installed ahead of every route (the port of the router guard): this gateway
//! answers its own machine and nothing else — peer address, `Host` and `Origin` — and it covers
//! the unauthenticated routes too, because the panel at `/` and `/health` have no token in front
//! of them. See `local_only` for why loopback binding alone is not enough.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use swiss_host::auth;
use swiss_host::local_only::remote_request_reason;
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::adapters::HttpMcp;
use swiss_mcp::registry::{EntryInner, Lifecycle, Registry};
use swiss_panel::admin;

// BODY_LIMIT comes in through the pub use below; a second private use would collide.

/// Enough of the HTTP response body to parse a typical JSON-RPC reply whole; larger replies are
/// previewed in the traffic log (the Node build's capture cap).
const RESPONSE_CAPTURE: usize = 16 * 1024;

/// Which build is running — the stamp build.rs planted at compile time (docs/16 H3). Served on
/// every surface an operator can check (/health, /api/info, the status command, --version), so
/// "is the running daemon the binary I just built?" is answerable without guessing. A short git
/// hash and an RFC 3339 time, nothing more: this is a loopback-only service, and a commit id
/// names code, not the machine or its user.
pub fn build_info() -> Value {
    json!({ "hash": env!("SWISS_GIT_HASH"), "time": env!("SWISS_BUILD_TIME") })
}

/// One MCP endpoint per started MCP, cached by the entry's generation — the port of the Node
/// build's `handlers` map. Rebuilt when the entry restarts (generation bump): the endpoint is
/// built by `adapter.build()` at start, so a config edit that swapped the adapter is served by a
/// fresh endpoint and the old one is cancelled (which tears down any in-flight exchanges).
struct CachedHandler {
    generation: u64,
    http: Arc<dyn HttpMcp>,
}

pub struct AppContext {
    pub registry: Arc<Registry>,
    pub tokens: Arc<TokenManager>,
    pub store: Arc<ManagedStore>,
    /// The call log this app's adapters write and its admin API reads - one instance per
    /// app, threaded everywhere it is needed (the S2 instantiation: no process-global
    /// log directory, so two apps in one process never see each other's calls).
    pub calls: Arc<swiss_mcp::calls::CallLog>,
    /// The traffic ring this app's proxy layer writes and its admin API reads - the same
    /// S2 instantiation as `calls` beside it: no process-global ring, so two apps in one
    /// process never see each other's traffic.
    pub traffic: Arc<swiss_mcp::traffic::TrafficLog>,
    /// Name of the env var the token was seeded from. Safe to show in client configs.
    pub token_env: String,
    /// The port this instance listens on — import detection reads it to recognize entries that
    /// point back at this very gateway.
    pub port: u16,
    /// The read-only tunnel view the MCP admin API consults (details/rename/delete). Set once by
    /// the boot sequence after the tunnel manager comes up, before the listener accepts requests;
    /// None on a gateway with no tunnel subsystem mounted.
    pub tunnel_links: std::sync::RwLock<Option<Arc<dyn swiss_tunnels::TunnelLinks>>>,
    /// Signalled by POST /api/shutdown; the server's main loop selects on it and runs the
    /// graceful sequence (the Node build emitted SIGTERM at itself — there is no signal on
    /// Windows, so this channel IS the signal).
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// The plugin host, set once by the boot sequence before the listener accepts requests.
    /// `None` in compositions that predate the host (tests, the http adapter's import probe):
    /// every host-dependent guard treats absence as "no plugin gating", which is exactly the
    /// pre-host behavior.
    pub plugin_host: std::sync::OnceLock<Arc<swiss_host::host::PluginHost>>,
    /// The connection catalog the /api/db routes lease through (docs/12 W3). Set once by
    /// the boot sequence from RuntimeServices — the SAME instance the MCP plugin
    /// registers into — before the listener accepts requests. Absent in compositions that
    /// mount no Data plugin: /api/db then answers an honest 503 rather than pretending.
    pub catalog: std::sync::OnceLock<Arc<swiss_host::services::catalog::CatalogRegistry>>,
    /// The group-scope table behind /api/groups/{scope} (docs/20 §2.2). The scopes AppContext
    /// owns natively (mcps) register in new(); the subsystem scopes (conns/rules/jobs/secrets)
    /// register from the boot sequence as their objects come up. A scope that never registered
    /// answers 404 "unknown scope", which is the honest state on a composition without it.
    pub group_scopes: swiss_host::groups::GroupScopes,
    /// OAuth authorize flows by MCP name (docs/24 D5): the authorize POST plants one
    /// (single-flight per name — a live flow is returned, a terminal one replaced), the GET
    /// polls it. Written by the flow task, read by the poll route; entries leave with their
    /// MCP (delete) or its rename.
    pub oauth_flows:
        std::sync::RwLock<HashMap<String, std::sync::Arc<swiss_mcp::oauth::FlowHandle>>>,
    /// The admin session (docs/48): the browser cookie and the CLI key that open /api/*. Set
    /// once by the boot sequence before the listener accepts requests. Unset, a request that
    /// came through a socket is refused - the gate fails closed; in-process requests (tests
    /// driving the router with oneshot) carry no ConnectInfo and pass, as they pass the
    /// loopback guard.
    pub session: std::sync::OnceLock<Arc<crate::session::AdminSession>>,
    handlers: Mutex<HashMap<String, CachedHandler>>,
}

impl AppContext {
    pub fn new(
        registry: Arc<Registry>,
        tokens: Arc<TokenManager>,
        store: Arc<ManagedStore>,
        calls: Arc<swiss_mcp::calls::CallLog>,
        traffic: Arc<swiss_mcp::traffic::TrafficLog>,
        token_env: impl Into<String>,
        port: u16,
    ) -> Arc<Self> {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let ctx = Arc::new(Self {
            registry,
            tokens,
            store,
            calls,
            traffic,
            token_env: token_env.into(),
            port,
            tunnel_links: std::sync::RwLock::new(None),
            shutdown,
            plugin_host: std::sync::OnceLock::new(),
            catalog: std::sync::OnceLock::new(),
            group_scopes: swiss_host::groups::GroupScopes::new(),
            oauth_flows: std::sync::RwLock::new(HashMap::new()),
            session: std::sync::OnceLock::new(),
            handlers: Mutex::new(HashMap::new()),
        });
        // The scopes this context owns natively (docs/20 §2.2): the managed store holds the
        // groups, the registry answers "is this a real MCP". Subsystem scopes join from
        // server.rs. Registered here so the admin API tests serve the same family the gateway
        // does, without each harness repeating the wiring.
        crate::subsystems::register_host_scopes(
            &ctx.group_scopes,
            ctx.registry.clone(),
            ctx.store.clone(),
        );
        // Rename/delete abandon an entry's name; the endpoint cached under it would then leak,
        // because the POST path that normally evicts a stale handler (generation mismatch, or a
        // stopped entry) never runs for a name that no longer resolves. The registry calls this
        // back so the endpoint is cancelled and dropped.
        //
        // The back-reference is a Weak on purpose: a strong one closed the cycle
        // AppContext -> Registry -> evictor -> AppContext, which kept the whole context (and
        // with it every Arc it holds) alive for as long as the registry existed — i.e. forever
        // for a process-wide singleton. The upgrade inside only pins the context for the
        // duration of one eviction.
        let evict_ctx = Arc::downgrade(&ctx);
        ctx.registry.set_evictor(Box::new(move |name| {
            if let Some(ctx) = evict_ctx.upgrade() {
                if let Ok(mut handlers) = ctx.handlers.lock() {
                    if let Some(cached) = handlers.remove(name) {
                        cached.http.cancel();
                    }
                }
            }
        }));
        ctx
    }

    /// The cached HTTP half for a started entry, or None when the entry is not serving.
    fn handler_for(&self, entry: &Arc<EntryInner>) -> Option<Arc<dyn HttpMcp>> {
        let (name, server, generation) = {
            let Ok(d) = entry.data.read() else {
                return None;
            };
            (
                d.name.clone(),
                d.server.clone(),
                entry.generation.load(std::sync::atomic::Ordering::SeqCst),
            )
        };
        let Some(server) = server else {
            // Entry is stopped. Drop a handler left over from a previous run so it cannot be
            // reused if the entry restarts under a new adapter before the generation check runs.
            if let Ok(mut handlers) = self.handlers.lock() {
                if let Some(stale) = handlers.remove(&name) {
                    stale.http.cancel();
                }
            }
            return None;
        };
        let mut handlers = self.handlers.lock().ok()?;
        if let Some(cached) = handlers.get(&name) {
            if cached.generation == generation {
                return Some(cached.http.clone());
            }
            let stale = handlers.remove(&name); // superseded by a restart
            if let Some(stale) = stale {
                stale.http.cancel();
            }
        }
        let http = server.http;
        handlers.insert(
            name.clone(),
            CachedHandler {
                generation,
                http: http.clone(),
            },
        );
        Some(http)
    }
}

/// An MCP-endpoint rejection in JSON-RPC shape, exactly as the Node build's `jsonError` writes
/// it. MCP endpoints answer JSON-RPC; admin routes answer `{error}` — the two are deliberately
/// not unified (see docs/05 §3).
pub fn json_rpc_error(status: StatusCode, message: &str) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "error": { "code": -32603, "message": message },
        "id": null,
    });
    (status, Json(body)).into_response()
}

pub use crate::reply::BODY_LIMIT;
pub use crate::reply::{admin_error, admin_json, NodeBody, NodeBodyRejection};

/// The MCP client catch-all cannot be matched by the boundary's static prefixes (one segment
/// naming a hosted MCP), so the handlers consult the host directly. Absent host — the
/// pre-host compositions (tests, the import probe) — means no gating, exactly as before.
fn plugin_client_guard(ctx: &AppContext) -> Option<Response> {
    let host = ctx.plugin_host.get()?;
    swiss_host::host::api::client_path_guard(host)
}

/// The bearer gate. Runs BEFORE the request body is read: an MCP POST can carry up to BODY_LIMIT
/// and none of it needs to be read to know the caller cannot be served. The matched token is
/// returned so the call log and the traffic log can attribute the request to the client.
fn verify_bearer(ctx: &AppContext, headers: &HeaderMap) -> Option<swiss_host::token::TokenRec> {
    let presented = auth::bearer_secret(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    );
    ctx.tokens.verify(presented)
}

/// The boundary — port of the router guard. Runs before route matching consumed anything, for
/// every route, including the unauthenticated ones.
async fn loopback_guard(State(ctx): State<Arc<AppContext>>, req: Request, next: Next) -> Response {
    let _ = &ctx;
    // The IP only — Node's socket.remoteAddress carries no port, and a port-bearing string
    // would fail the loopback name check below. A request with no ConnectInfo at all did not
    // come through a socket (an in-process test driving the Router directly), so it is by
    // definition local; the Host/Origin checks below still apply to it — they are the ones
    // that catch DNS rebinding, which is what a peer address cannot reveal.
    let peer = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match remote_request_reason(Some(peer.as_str()), host.as_deref(), origin.as_deref()) {
        None => next.run(req).await,
        Some(reason) => {
            swiss_core::log::warn(
                "refused a non-local request",
                Some(
                    json!({ "reason": reason, "method": req.method().as_str(), "url": req.uri().path() }),
                ),
            );
            admin_error(StatusCode::FORBIDDEN, &reason)
        }
    }
}

/// Whether a request came through a socket. The real listener always records the peer
/// (`into_make_service_with_connect_info`); a request without it was driven in-process.
fn via_socket(req: &Request) -> bool {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .is_some()
}

/// The admin session gate (docs/48), inside the loopback guard: every /api/* request needs
/// the CLI key or a signed session cookie. Everything else passes - the panel shell answers
/// its own sign-in (panel_root), /admin/* is code without data, /health is liveness, and
/// /mcp/* has its own bearer gate.
async fn session_gate(State(ctx): State<Arc<AppContext>>, req: Request, next: Next) -> Response {
    if !req.uri().path().starts_with("/api/") || !via_socket(&req) {
        return next.run(req).await;
    }
    match ctx.session.get() {
        Some(session) if session.authorized(req.headers()) => next.run(req).await,
        _ => admin_error(
            StatusCode::UNAUTHORIZED,
            "not signed in - run `swiss open` to sign this browser in",
        ),
    }
}

#[derive(serde::Deserialize)]
struct RootQuery {
    token: Option<String>,
}

/// The dashboard shell. no-store: always serve the latest, so a rebuilt panel needs no restart.
///
/// Also the one place a browser signs in (docs/48): `/?token=<login token>` spends the token,
/// sets the session cookie and redirects to a clean `/` - the token never stays in the address
/// bar or reaches an API path. Without a valid session the shell is not served; the lock page
/// says how to get one.
async fn panel_root(State(ctx): State<Arc<AppContext>>, req: Request) -> Response {
    let token = axum::extract::Query::<RootQuery>::try_from_uri(req.uri())
        .ok()
        .and_then(|q| q.0.token);
    if via_socket(&req) {
        let Some(session) = ctx.session.get() else {
            return lock_page(false);
        };
        if let Some(token) = token {
            if !session.redeem_ticket(&token) {
                return lock_page(true);
            }
            return (
                StatusCode::SEE_OTHER,
                [
                    (header::LOCATION, "/".to_string()),
                    (header::SET_COOKIE, session.issue_cookie()),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
            )
                .into_response();
        }
        if !session.authorized(req.headers()) {
            return lock_page(false);
        }
    }
    match admin::admin_html() {
        Some(html) => (
            [(header::CACHE_CONTROL, "no-store")],
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        None => admin_error(StatusCode::NOT_FOUND, "panel not embedded"),
    }
}

/// What a browser without a session sees: one instruction, in both languages the panel
/// speaks. Server-rendered and self-contained - it must work before any panel code loads.
fn lock_page(spent_link: bool) -> Response {
    let (en, zh) = if spent_link {
        (
            "This sign-in link has already been used or has expired.",
            "这个登录链接已经用过或已过期。",
        )
    } else {
        ("This swiss panel needs a sign-in.", "这个 swiss 面板需要登录。")
    };
    let html = format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
            "<title>swiss - sign in</title><style>",
            "body{{font:15px/1.6 system-ui,sans-serif;margin:0;display:grid;place-items:center;",
            "min-height:100vh;background:#f7f7f8;color:#1f2328}}",
            "main{{max-width:34rem;padding:2rem}}p{{margin:.4rem 0}}",
            "code{{background:#eaeef2;padding:.15rem .4rem;border-radius:6px;font-size:14px}}",
            "@media (prefers-color-scheme:dark){{body{{background:#101114;color:#e6e6e6}}",
            "code{{background:#26282e}}}}</style></head><body><main>",
            "<p><strong>{en}</strong></p>",
            "<p>Run <code>swiss open</code> in a terminal - it opens a one-time sign-in link.</p>",
            "<p lang=\"zh\"><strong>{zh}</strong></p>",
            "<p lang=\"zh\">在终端运行 <code>swiss open</code>，它会打开一次性的登录链接。</p>",
            "</main></body></html>"
        ),
        en = en,
        zh = zh
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::CACHE_CONTROL, "no-store")],
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// POST /api/session/ticket - a fresh single-use login token and the URL that spends it. Behind
/// the session gate like every /api/* route: the CLI mints with its key (`swiss open`, `swiss
/// start`), a signed-in browser with its cookie.
async fn session_ticket(State(ctx): State<Arc<AppContext>>) -> Response {
    let Some(session) = ctx.session.get() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "sessions are not set up");
    };
    let token = session.mint_ticket();
    admin_json(
        StatusCode::OK,
        json!({ "token": token, "url": format!("http://127.0.0.1:{}/?token={token}", ctx.port) }),
    )
}

/// The panel's assets (styles/js ES modules), served from the same tree with the same freshness
/// rule.
async fn panel_asset(Path(path): Path<String>) -> Response {
    match admin::admin_asset(&path) {
        Some((body, mime)) => (
            [(header::CACHE_CONTROL, "no-store")],
            [(header::CONTENT_TYPE, mime)],
            body,
        )
            .into_response(),
        None => admin_error(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn health() -> Response {
    // ok plus the build stamp, deliberately on the unauthenticated route: a liveness probe is
    // exactly where "up, but up since before your deploy" wants to be visible (docs/16 H3).
    admin_json(StatusCode::OK, json!({ "ok": true, "build": build_info() }))
}

async fn health_check(State(ctx): State<Arc<AppContext>>) -> Response {
    ctx.registry.check_all().await;
    admin_json(StatusCode::OK, json!({ "ok": true }))
}

/// Session teardown for MCP clients: stateless, so there is nothing to tear down — acknowledge
/// with 204 so the client closes cleanly. (No GET handler: the modern protocol serves
/// notifications through the client's own POST stream — a GET falls through to a 404.)
async fn mcp_delete(
    State(ctx): State<Arc<AppContext>>,
    Path(_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(answer) = plugin_client_guard(&ctx) {
        return answer;
    }
    if verify_bearer(&ctx, &headers).is_none() {
        // The pre-body refusal shape — Node's `wrapped.refuse` fast path answered
        // `{error:"Unauthorized"}`, not JSON-RPC (the SDK-era jsonError only fired once the
        // handler itself ran, past this gate).
        return admin_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// MCP endpoint: POST /mcp/<name> (docs/24: the /mcp/ prefix is the MCP plugin's domain — the
/// root belongs to host chrome and future plugins). Served through the per-generation cached
/// StreamableHttpService
/// so the endpoint answers BOTH protocol eras: the modern per-request-envelope path (native
/// `server/discover`) and the legacy 2025-era initialize handshake.
async fn mcp_post(
    State(ctx): State<Arc<AppContext>>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    let t0 = Instant::now();
    let client = match verify_bearer(&ctx, req.headers()) {
        // Pre-body refusal, in Node's `wrapped.refuse` shape — see mcp_delete.
        None => return admin_error(StatusCode::UNAUTHORIZED, "Unauthorized"),
        Some(rec) => rec,
    };
    // The MCP plugin owns the client paths: when it is not serving (disabled at boot, or
    // disabled at runtime), every endpoint answers the host's structured 503 — and in
    // particular a lazy proc is NOT spawned behind a disabled plugin. The guard sits between
    // auth and the ensure_started await, so the answer to a stranger stays the plain 401 it
    // always was.
    if let Some(answer) = plugin_client_guard(&ctx) {
        return answer;
    }
    let Some(entry) = ctx.registry.get(&name) else {
        return json_rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("Unknown MCP path: {name}"),
        );
    };
    let lifecycle = entry.data.read().ok().map(|d| d.lifecycle);
    // A lazy proc idle at boot (or reaped since): this request is the wakeup. The await IS the
    // contract — the client's request waits for the spawn and then gets served by it, bounded by
    // the adapter's own start timeout, because AI clients do not retry a 503 helpfully.
    if lifecycle == Some(Lifecycle::Idle) {
        if let Err(err) = ctx.registry.ensure_started(&name).await {
            return json_rpc_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("MCP '{name}' failed to start: {err}"),
            );
        }
    }
    ctx.registry.note_activity(&name); // served traffic pushes a lazy proc's idle-reap deadline back
    let Some(handler) = ctx.handler_for(&entry) else {
        // docs/28 D2: this state is a disable — it persists across boots and refuses every
        // client method. The wording says the operator's word and the way out, not the
        // lifecycle's internal noun.
        return json_rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("MCP '{name}' is disabled — enable it from the panel"),
        );
    };

    // Read the body once here (bounded by the router's 2 MB limit): the traffic log needs the
    // JSON-RPC method, and the MCP service needs the identical bytes back.
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(err) => return admin_error(StatusCode::PAYLOAD_TOO_LARGE, &err.to_string()),
    };
    let parsed_body: Option<Value> = serde_json::from_slice(&bytes).ok();
    let forwarded = Request::from_parts(parts, Body::from(bytes));

    // Every request this gateway answers runs attributed to the client that made it, so the call
    // log and the traffic log can tell clients apart (with_call_client scopes the task-local the
    // tool handlers read).
    let response = swiss_mcp::calls::with_call_client_mcp(client.label.clone(), async {
        handler.handle(forwarded).await
    })
    .await;

    // Capture the bytes written as the HTTP response body (clipped) so the traffic log can
    // record what was answered, not only what was asked. Buffered whole: a stateless-mode reply
    // is one JSON body, bounded by the adapters' own render caps. A body that cannot be read
    // back (over the 16 MB cap) cannot be replayed either: re-emitting the ORIGINAL headers
    // with an empty body would hand the client a 200 with a truncated payload, so the honest
    // answer is an error of our own.
    let (parts, body) = response.into_parts();
    let response_bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(err) => {
            return admin_error(
                StatusCode::BAD_GATEWAY,
                &format!("response body could not be captured: {err}"),
            )
        }
    };
    let captured_prefix: Vec<u8> = response_bytes
        .iter()
        .copied()
        .take(RESPONSE_CAPTURE)
        .collect();
    let ok = parts.status.as_u16() < 400;
    let status = parts.status;
    let response = Response::from_parts(parts, Body::from(response_bytes));

    ctx.traffic.record_traffic(
        &name,
        parsed_body.as_ref(),
        Some(&client.label),
        ok,
        t0.elapsed().as_millis() as u64,
        Some(&String::from_utf8_lossy(&captured_prefix)),
    );
    swiss_core::log::log(
        "info",
        "request",
        Some(
            json!({ "path": name, "ms": t0.elapsed().as_millis() as u64, "status": status.as_u16() }),
        ),
    );
    response
}

/// Nothing handled this: the router's terminal 404, same shape as the Node build.
pub(crate) async fn fallback_404(req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    admin_error(
        StatusCode::NOT_FOUND,
        &format!("no route for {method} {path}"),
    )
}

/// The terminal 404, with the migration hint (docs/24 P4): a POST or DELETE aimed at a
/// single-segment root path that names a REGISTERED MCP is almost certainly a client still
/// configured for the retired root-level endpoint shape. The cutover stays hard — this is
/// still a 404, never an alias — but the body names the new home, so re-pointing the client
/// is a copy-paste instead of a reread of the docs. Everything else keeps the plain shape
/// (unknown names included: a hint about a name that does not exist would be noise).
async fn moved_hint_404(State(ctx): State<Arc<AppContext>>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let root_name = path
        .strip_prefix('/')
        .filter(|rest| !rest.is_empty() && !rest.contains('/'));
    if let Some(name) = root_name {
        if matches!(
            method,
            axum::http::Method::POST | axum::http::Method::DELETE
        ) && ctx.registry.get(name).is_some()
        {
            return admin_error(
                StatusCode::NOT_FOUND,
                &format!(
                    "no route for {method} {path} — moved to /mcp/{name}, update the client URL"
                ),
            );
        }
    }
    admin_error(
        StatusCode::NOT_FOUND,
        &format!("no route for {method} {path}"),
    )
}

/// The `/api/plugins` tree with this build's 404-for-a-wrong-method rule applied, so the
/// management routes answer like every other admin route rather than axum's default 405.
fn host_api_tree(host: Arc<swiss_host::host::PluginHost>) -> Router<()> {
    swiss_host::host::api::mount(host).method_not_allowed_fallback(fallback_404)
}

/// Build the gateway router. The caller serves it with
/// `into_make_service_with_connect_info::<SocketAddr>()` — the loopback guard needs the peer
/// address.
///
/// `extra` carries a stateless router to mount alongside the rest (the tunnel and jobs APIs,
/// their own state already applied). axum's `Router::layer` covers only routes present at the
/// call, so the extra tree gets its OWN copy of the loopback guard and the body limit below —
/// merging it unguarded would leave every /api/tunnels route outside the boundary, and the
/// guard is the only auth that tree has.
///
/// When the boot sequence installed a plugin host, both trees ALSO get the plugin boundary:
/// each request asks the host whether a plugin owns the path and, if so, whether that plugin
/// is serving. The boundary sits INSIDE the loopback guard (security answers before plugin
/// state does) and over every plugin-owned path — stable routes, live dispatch.
pub fn build_app(ctx: Arc<AppContext>, extra: Option<Router<()>>) -> Router {
    let mcp_routes = Router::new().route(
        "/mcp/{path}",
        // No GET on an MCP path: the modern protocol serves notifications through the client's
        // own POST stream, so a GET is not a route — it falls to the same 404 every unhandled
        // method gets (the Node build had no GET handler either).
        post(mcp_post).delete(mcp_delete).get(fallback_404),
    );

    let guard_ctx = ctx.clone();
    let plugin_host = ctx.plugin_host.get().cloned();
    let boundary = plugin_host.as_ref().map(|host| {
        middleware::from_fn_with_state(host.clone(), swiss_host::host::api::plugin_boundary)
    });
    // The host's own management surface travels with the extra tree: same loopback guard,
    // same body limit. It stays reachable while every plugin is down — no plugin may claim a
    // prefix under /api/plugins (host::PluginHost::register refuses it), so the boundary over
    // this tree can never answer 503 for the routes that enable a plugin again.
    let extra = match (extra, plugin_host) {
        (Some(extra), Some(host)) => Some(extra.merge(host_api_tree(host))),
        (None, Some(host)) => Some(host_api_tree(host)),
        (extra, None) => extra,
    };
    let app = Router::new()
        .route("/", get(panel_root))
        .route("/api/session/ticket", post(session_ticket))
        .route("/admin/{*path}", get(panel_asset))
        .route("/health", get(health))
        .route("/health/check", get(health_check))
        .merge(
            swiss_data::dbbrowser_api::dbbrowser_router::<Arc<AppContext>>(
                // The W3 seam (docs/12): /api/db leases through the connection catalog the MCP
                // plugin provides. Compositions that never set one (no Data plugin mounted)
                // get an empty registry — every route then answers the honest 503.
                ctx.catalog.get().cloned().unwrap_or_else(|| {
                    Arc::new(swiss_host::services::catalog::CatalogRegistry::new())
                }),
                // The picker ranks by the sidebar's VISUAL order — groups in their stored
                // order (the first is the fallback home), members in the flat manual order
                // (PUT /api/order) within each group — read live per request. Ranked by the raw
                // flat order instead, a connection would jump its group neighbours the moment
                // members of two groups interleave, which any cross-group drag produces.
                {
                    let ctx = ctx.clone();
                    Arc::new(move || {
                        let mut names: Vec<String> = ctx
                            .registry
                            .all()
                            .iter()
                            .filter_map(|entry| entry.data.read().ok().map(|d| d.name.clone()))
                            .collect();
                        visual_order(&ctx.store, &mut names)
                    })
                },
                // The row's group label (docs/20 G5): the same store answer visual_order
                // slices by, read live per request - ManagedStore::group_of already sinks an
                // unassigned name into the first group, which is the rendering rule too.
                {
                    let ctx = ctx.clone();
                    Arc::new(move |n: &str| ctx.store.group_of(n))
                },
            ),
        )
        .merge(crate::adminapi::mount(ctx.clone()))
        // After the /api tree. The /mcp/ prefix already keeps the catch-all off /api/tunnels;
        // merge order stays host-first as belt and braces for whatever claims the root next.
        .merge(mcp_routes)
        .fallback(moved_hint_404)
        // A path that exists under another method answers 404 in the Node build (its router
        // matched method+pattern together), not axum's default 405.
        .method_not_allowed_fallback(fallback_404);
    let app = match boundary.clone() {
        Some(layer) => app.layer(layer),
        None => app,
    };
    let app = app
        .layer(middleware::from_fn_with_state(guard_ctx.clone(), session_gate))
        .layer(middleware::from_fn_with_state(
            guard_ctx.clone(),
            loopback_guard,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(ctx);
    match (extra, boundary) {
        (Some(extra), Some(layer)) => app.merge(
            extra
                .layer(layer)
                .layer(middleware::from_fn_with_state(guard_ctx.clone(), session_gate))
                .layer(middleware::from_fn_with_state(guard_ctx, loopback_guard))
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        ),
        (Some(extra), None) => app.merge(
            extra
                .layer(middleware::from_fn_with_state(guard_ctx.clone(), session_gate))
                .layer(middleware::from_fn_with_state(guard_ctx, loopback_guard))
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        ),
        (None, _) => app,
    }
}

/// The Data picker's ranking: the sidebar's VISUAL order, not the flat one. `names` arrives in
/// whatever order the registry happened to start things in; it leaves ranked by the manual flat
/// order (PUT /api/order, unranked names last in name order — exactly like /api/mcps) and then
/// sliced by group: groups in their stored order, members keeping their flat rank inside their
/// group. Ranked flat alone, a connection would jump its group neighbours the moment members of
/// two groups interleave — which any cross-group drag produces. Unassigned names land in the
/// FIRST group (the sink slot), mirroring `ManagedStore::group_of`.
fn visual_order(store: &ManagedStore, names: &mut [String]) -> Vec<String> {
    let rank: HashMap<String, usize> = store
        .get_order()
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, i))
        .collect();
    names.sort_by(|a, b| {
        let ra = rank.get(a).copied().unwrap_or(usize::MAX);
        let rb = rank.get(b).copied().unwrap_or(usize::MAX);
        // Compare ranks only when they actually differ; names decide otherwise.
        if ra != rb {
            ra.cmp(&rb)
        } else {
            a.cmp(b)
        }
    });
    let groups = store.get_groups();
    let mut by_group: HashMap<String, Vec<String>> = HashMap::new();
    for n in names.iter() {
        by_group
            .entry(store.group_of(n))
            .or_default()
            .push(n.clone());
    }
    let mut out = Vec::new();
    for g in &groups {
        out.extend(by_group.remove(g).unwrap_or_default());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_store() -> ManagedStore {
        // A unique scratch path per call; the test never needs to clean up, only to not collide.
        let dir = std::env::temp_dir().join(format!(
            "swiss-visual-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        ManagedStore::open_at(dir.join("managed.json"))
    }

    #[test]
    fn visual_order_slices_the_flat_rank_by_group() {
        let store = scratch_store();
        // Two groups whose flat order interleaves members — exactly what a cross-group drag
        // makes. Both memberships are explicit: redis-gang is FIRST, so an unassigned name
        // would render in it, which is not what this test is about.
        store
            .set_groups(vec!["redis-gang".into(), "default".into()])
            .unwrap();
        for (n, g) in [
            ("redis-a", "redis-gang"),
            ("redis-b", "redis-gang"),
            ("mysql-a", "default"),
            ("mysql-b", "default"),
        ] {
            store.set_mcp_group(n, Some(g)).unwrap();
        }
        store
            .set_order(vec![
                "mysql-a".into(),
                "redis-a".into(),
                "mysql-b".into(),
                "redis-b".into(),
            ])
            .unwrap();
        // Registry order is deliberately unrelated; unranked names would fall to name order.
        let mut names: Vec<String> = ["redis-b", "mysql-b", "redis-a", "mysql-a"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            visual_order(&store, &mut names),
            ["redis-a", "redis-b", "mysql-a", "mysql-b"]
        );
    }

    #[test]
    fn visual_order_keeps_unranked_names_last_in_name_order_inside_their_group() {
        let store = scratch_store();
        store
            .set_groups(vec!["default".into(), "new".into()])
            .unwrap();
        store.set_mcp_group("zebra", Some("new")).unwrap();
        store.set_mcp_group("alpha", Some("new")).unwrap();
        // No manual order at all: every name is unranked, so plain name order inside each group;
        // unassigned names render in the first group, like the sidebar draws them.
        let mut names: Vec<String> = ["zeta", "zebra", "alpha"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(visual_order(&store, &mut names), ["zeta", "alpha", "zebra"]);
    }
}
