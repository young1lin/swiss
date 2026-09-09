//! The gateway HTTP server — port of `router.ts` (with `http.ts`'s routing contract carried by
//! axum): the panel, health, the management API, and the MCP endpoints as a catch-all
//! single-segment route resolved dynamically from the registry, so paths can be added and
//! removed at runtime.
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

use lmg_host::auth;
use lmg_host::local_only::remote_request_reason;
use lmg_host::managed::ManagedStore;
use lmg_host::token::TokenManager;
use lmg_mcp::adapters::HttpMcp;
use lmg_mcp::registry::{EntryInner, Lifecycle, Registry};
use lmg_mcp::traffic::record_traffic;
use lmg_panel::admin;

// BODY_LIMIT comes in through the pub use below; a second private use would collide.

/// Enough of the HTTP response body to parse a typical JSON-RPC reply whole; larger replies are
/// previewed in the traffic log (the Node build's capture cap).
const RESPONSE_CAPTURE: usize = 16 * 1024;

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
    pub calls: Arc<lmg_mcp::calls::CallLog>,
    /// Name of the env var the token was seeded from. Safe to show in client configs.
    pub token_env: String,
    /// The port this instance listens on — import detection reads it to recognize entries that
    /// point back at this very gateway.
    pub port: u16,
    /// The read-only tunnel view the MCP admin API consults (details/rename/delete). Set once by
    /// the boot sequence after the tunnel manager comes up, before the listener accepts requests;
    /// None on a gateway with no tunnel subsystem mounted.
    pub tunnel_links: std::sync::RwLock<Option<Arc<dyn lmg_tunnels::TunnelLinks>>>,
    /// Signalled by POST /api/shutdown; the server's main loop selects on it and runs the
    /// graceful sequence (the Node build emitted SIGTERM at itself — there is no signal on
    /// Windows, so this channel IS the signal).
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// The plugin host, set once by the boot sequence before the listener accepts requests.
    /// `None` in compositions that predate the host (tests, the http adapter's import probe):
    /// every host-dependent guard treats absence as "no plugin gating", which is exactly the
    /// pre-host behavior.
    pub plugin_host: std::sync::OnceLock<Arc<lmg_host::host::PluginHost>>,
    /// The connection catalog the /api/db routes lease through (docs/12 W3). Set once by
    /// the boot sequence from RuntimeServices — the SAME instance the MCP plugin
    /// registers into — before the listener accepts requests. Absent in compositions that
    /// mount no Data plugin: /api/db then answers an honest 503 rather than pretending.
    pub catalog: std::sync::OnceLock<Arc<lmg_host::services::catalog::CatalogRegistry>>,
    handlers: Mutex<HashMap<String, CachedHandler>>,
}

impl AppContext {
    pub fn new(
        registry: Arc<Registry>,
        tokens: Arc<TokenManager>,
        store: Arc<ManagedStore>,
        calls: Arc<lmg_mcp::calls::CallLog>,
        token_env: impl Into<String>,
        port: u16,
    ) -> Arc<Self> {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let ctx = Arc::new(Self {
            registry,
            tokens,
            store,
            calls,
            token_env: token_env.into(),
            port,
            tunnel_links: std::sync::RwLock::new(None),
            shutdown,
            plugin_host: std::sync::OnceLock::new(),
            catalog: std::sync::OnceLock::new(),
            handlers: Mutex::new(HashMap::new()),
        });
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
    lmg_host::host::api::client_path_guard(host)
}

/// The bearer gate. Runs BEFORE the request body is read: an MCP POST can carry up to BODY_LIMIT
/// and none of it needs to be read to know the caller cannot be served. The matched token is
/// returned so the call log and the traffic log can attribute the request to the client.
fn verify_bearer(ctx: &AppContext, headers: &HeaderMap) -> Option<lmg_host::token::TokenRec> {
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
            lmg_core::log::warn(
                "refused a non-local request",
                Some(
                    json!({ "reason": reason, "method": req.method().as_str(), "url": req.uri().path() }),
                ),
            );
            admin_error(StatusCode::FORBIDDEN, &reason)
        }
    }
}

/// The dashboard shell. no-store: always serve the latest, so a rebuilt panel needs no restart.
async fn panel_root() -> Response {
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
    admin_json(StatusCode::OK, json!({ "ok": true }))
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

/// MCP endpoint: POST /<mcp-name>. Served through the per-generation cached StreamableHttpService
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
        let state = entry
            .data
            .read()
            .ok()
            .map(|d| d.lifecycle.as_str())
            .unwrap_or("unknown");
        return json_rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("MCP '{name}' is not started (state: {state})"),
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
    let response = lmg_mcp::calls::with_call_client_mcp(client.label.clone(), async {
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

    record_traffic(
        &name,
        parsed_body.as_ref(),
        Some(&client.label),
        ok,
        t0.elapsed().as_millis() as u64,
        Some(&String::from_utf8_lossy(&captured_prefix)),
    );
    lmg_core::log::log(
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

/// The `/api/plugins` tree with this build's 404-for-a-wrong-method rule applied, so the
/// management routes answer like every other admin route rather than axum's default 405.
fn host_api_tree(host: Arc<lmg_host::host::PluginHost>) -> Router<()> {
    lmg_host::host::api::mount(host).method_not_allowed_fallback(fallback_404)
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
        "/{path}",
        // No GET on an MCP path: the modern protocol serves notifications through the client's
        // own POST stream, so a GET is not a route — it falls to the same 404 every unhandled
        // method gets (the Node build had no GET handler either).
        post(mcp_post).delete(mcp_delete).get(fallback_404),
    );

    let guard_ctx = ctx.clone();
    let plugin_host = ctx.plugin_host.get().cloned();
    let boundary = plugin_host.as_ref().map(|host| {
        middleware::from_fn_with_state(host.clone(), lmg_host::host::api::plugin_boundary)
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
        .route("/admin/{*path}", get(panel_asset))
        .route("/health", get(health))
        .route("/health/check", get(health_check))
        .merge(
            lmg_data::dbbrowser_api::dbbrowser_router::<Arc<AppContext>>(
                // The W3 seam (docs/12): /api/db leases through the connection catalog the MCP
                // plugin provides. Compositions that never set one (no Data plugin mounted)
                // get an empty registry — every route then answers the honest 503.
                ctx.catalog.get().cloned().unwrap_or_else(|| {
                    Arc::new(lmg_host::services::catalog::CatalogRegistry::new())
                }),
            ),
        )
        .merge(crate::adminapi::mount(ctx.clone()))
        // Before the MCP catch-all, which would otherwise swallow /api/tunnels.
        .merge(mcp_routes)
        .fallback(fallback_404)
        // A path that exists under another method answers 404 in the Node build (its router
        // matched method+pattern together), not axum's default 405.
        .method_not_allowed_fallback(fallback_404);
    let app = match boundary.clone() {
        Some(layer) => app.layer(layer),
        None => app,
    };
    let app = app
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
                .layer(middleware::from_fn_with_state(guard_ctx, loopback_guard))
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        ),
        (Some(extra), None) => app.merge(
            extra
                .layer(middleware::from_fn_with_state(guard_ctx, loopback_guard))
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
        ),
        (None, _) => app,
    }
}
