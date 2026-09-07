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

use crate::adapters::HttpMcp;
use crate::admin;
use crate::auth;
use crate::local_only::remote_request_reason;
use crate::managed::ManagedStore;
use crate::registry::{EntryInner, Lifecycle, Registry};
use crate::token::TokenManager;
use crate::traffic::record_traffic;

/// Max JSON request body accepted — `BODY_LIMIT` in the Node router. Enforced once, on the
/// router, for every route (axum's DefaultBodyLimit).
pub const BODY_LIMIT: usize = 2 * 1024 * 1024;

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
    /// Name of the env var the token was seeded from. Safe to show in client configs.
    pub token_env: String,
    /// The port this instance listens on — import detection reads it to recognize entries that
    /// point back at this very gateway.
    pub port: u16,
    /// The read-only tunnel view the MCP admin API consults (details/rename/delete). Set once by
    /// the boot sequence after the tunnel manager comes up, before the listener accepts requests;
    /// None on a gateway with no tunnel subsystem mounted.
    pub tunnel_links: std::sync::RwLock<Option<Arc<dyn crate::adminapi::TunnelLinks>>>,
    /// Signalled by POST /api/shutdown; the server's main loop selects on it and runs the
    /// graceful sequence (the Node build emitted SIGTERM at itself — there is no signal on
    /// Windows, so this channel IS the signal).
    pub shutdown: tokio::sync::watch::Sender<bool>,
    handlers: Mutex<HashMap<String, CachedHandler>>,
}

impl AppContext {
    pub fn new(
        registry: Arc<Registry>,
        tokens: Arc<TokenManager>,
        store: Arc<ManagedStore>,
        token_env: impl Into<String>,
        port: u16,
    ) -> Arc<Self> {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let ctx = Arc::new(Self {
            registry,
            tokens,
            store,
            token_env: token_env.into(),
            port,
            tunnel_links: std::sync::RwLock::new(None),
            shutdown,
            handlers: Mutex::new(HashMap::new()),
        });
        // Rename/delete abandon an entry's name; the endpoint cached under it would then leak,
        // because the POST path that normally evicts a stale handler (generation mismatch, or a
        // stopped entry) never runs for a name that no longer resolves. The registry calls this
        // back so the endpoint is cancelled and dropped.
        let evict_ctx = ctx.clone();
        ctx.registry.set_evictor(Box::new(move |name| {
            if let Ok(mut handlers) = evict_ctx.handlers.lock() {
                if let Some(cached) = handlers.remove(name) {
                    cached.http.cancel();
                }
            }
        }));
        ctx
    }

    /// Build the database-browser view from the current registry entries.
    pub fn browser_resolver(self: &Arc<Self>) -> crate::dbbrowser_api::BrowseResolver {
        let registry = self.registry.clone();
        Arc::new(move || {
            registry
                .all()
                .into_iter()
                .filter_map(|entry| {
                    let data = entry.data.read().ok()?;
                    let browser = data.adapter.browser()?;
                    Some(Arc::new(crate::dbbrowser_api::BrowsableConnection {
                        name: data.name.clone(),
                        adapter_type: data.def.type_().to_string(),
                        state: if data.lifecycle == Lifecycle::Started {
                            data.status.as_str().to_string()
                        } else {
                            data.lifecycle.as_str().to_string()
                        },
                        browser,
                    }))
                })
                .collect()
        })
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

/// An admin-route error: `{error}`.
pub fn admin_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub fn admin_json(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// The bearer gate. Runs BEFORE the request body is read: an MCP POST can carry up to BODY_LIMIT
/// and none of it needs to be read to know the caller cannot be served. The matched token is
/// returned so the call log and the traffic log can attribute the request to the client.
fn verify_bearer(ctx: &AppContext, headers: &HeaderMap) -> Option<crate::token::TokenRec> {
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
            crate::log::warn(
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
    if verify_bearer(&ctx, &headers).is_none() {
        return json_rpc_error(StatusCode::UNAUTHORIZED, "Unauthorized");
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
        None => return json_rpc_error(StatusCode::UNAUTHORIZED, "Unauthorized"),
        Some(rec) => rec,
    };
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
    let response = crate::calls::with_call_client_mcp(client.label.clone(), async {
        handler.handle(forwarded).await
    })
    .await;

    // Capture the bytes written as the HTTP response body (clipped) so the traffic log can
    // record what was answered, not only what was asked. Buffered whole: a stateless-mode reply
    // is one JSON body, bounded by the adapters' own render caps.
    let (parts, body) = response.into_parts();
    let response_bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => axum::body::to_bytes(Body::empty(), 0)
            .await
            .unwrap_or_default(),
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
    crate::log::log(
        "info",
        "request",
        Some(
            json!({ "path": name, "ms": t0.elapsed().as_millis() as u64, "status": status.as_u16() }),
        ),
    );
    response
}

/// Nothing handled this: the router's terminal 404, same shape as the Node build.
async fn fallback_404(req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    admin_error(
        StatusCode::NOT_FOUND,
        &format!("no route for {method} {path}"),
    )
}

/// Build the gateway router. The caller serves it with
/// `into_make_service_with_connect_info::<SocketAddr>()` — the loopback guard needs the peer
/// address.
pub fn build_app(ctx: Arc<AppContext>) -> Router {
    let mcp_routes = Router::new().route(
        "/{path}",
        // No GET on an MCP path: the modern protocol serves notifications through the client's
        // own POST stream, so a GET is not a route — it falls to the same 404 every unhandled
        // method gets (the Node build had no GET handler either).
        post(mcp_post).delete(mcp_delete).get(fallback_404),
    );

    Router::new()
        .route("/", get(panel_root))
        .route("/admin/{*path}", get(panel_asset))
        .route("/health", get(health))
        .route("/health/check", get(health_check))
        .merge(crate::dbbrowser_api::dbbrowser_router::<Arc<AppContext>>(
            ctx.browser_resolver(),
        ))
        .merge(crate::adminapi::mount(ctx.clone()))
        .merge(mcp_routes)
        .fallback(fallback_404)
        // A path that exists under another method answers 404 in the Node build (its router
        // matched method+pattern together), not axum's default 405.
        .method_not_allowed_fallback(fallback_404)
        .layer(middleware::from_fn_with_state(ctx.clone(), loopback_guard))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(ctx)
}
