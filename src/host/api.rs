//! The `/api/plugins` management surface and the host route boundary.
//!
//! The boundary is the docs/09 §4 answer to "axum routes must not capture an expensive
//! instance and then only flip an enabled bool": the routes stay MOUNTED (stable — same paths
//! whether the plugin serves or not), and each request asks the host one cheap question — is
//! the owning plugin serving? A disabled plugin's instance is not parked behind that answer;
//! it was dropped by the stop, so there is nothing left to accidentally serve. Every guarded
//! route sits inside the loopback guard like the rest of /api — this layer never widens who
//! may ask.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};

use super::builtin;
use super::engine::PluginHost;
use crate::app::{admin_error, admin_json, NodeBody};
use crate::config_store::ConfigStoreError;

/// The prefixes the HOST serves itself. A plugin that claimed one would shadow the very
/// surface that enables it again, so registration refuses them (see `PluginHost::register`).
pub const HOST_ROUTES: &[&str] = &["/api/plugins"];

/// Every prefix a plugin may NOT claim: the management surface above plus the shared
/// execution surface (`/api/actions`, `/api/runs`), which is host-owned for the same reason —
/// it must answer while the providers whose capabilities it lists are disabled.
pub fn reserved_routes() -> impl Iterator<Item = &'static str> {
    HOST_ROUTES
        .iter()
        .chain(crate::services::api::ROUTES)
        .copied()
}

/// Mount the management routes. Always live: managing a plugin must not depend on the plugin
/// it manages (a disabled plugin's own API is 503, so it cannot re-enable itself).
pub fn mount(host: Arc<PluginHost>) -> Router {
    Router::new()
        .route("/api/plugins", get(list))
        .route("/api/plugins/{id}/enable", post(enable))
        .route("/api/plugins/{id}/disable", post(disable))
        .route(
            "/api/plugins/{id}/config",
            get(get_config).put(put_config),
        )
        .with_state(host)
}

/// The boundary middleware. Applied over every merged tree next to the loopback guard; it
/// only ever answers 503 or passes through — never widens access.
pub async fn plugin_boundary(
    State(host): State<Arc<PluginHost>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if let Some(id) = host.route_owner(&path) {
        if !host.is_active(&id) {
            return unavailable(&host, &id);
        }
    }
    next.run(req).await
}

/// The MCP client catch-all (POST /{name}) is one segment naming a hosted MCP — no static
/// prefix for the boundary to match — so the handler consults the host directly. `Some`
/// means: answer this, do not touch the registry, and in particular do NOT spawn a lazy proc
/// behind a disabled plugin.
pub fn client_path_guard(host: &PluginHost) -> Option<Response> {
    if host.is_active(builtin::MCP_ID) {
        None
    } else {
        Some(unavailable(host, builtin::MCP_ID))
    }
}

/// The structured 503 a not-serving plugin answers on every path it owns. Same `{error}`
/// shape subsystems.rs answers for boot-time absence — the panel's toast shows it verbatim —
/// with the live state named: disabled, starting, stopping, or failed (with the reason).
pub fn unavailable(host: &PluginHost, id: &str) -> Response {
    let state = host.plugin(id).map(|e| (e.state(), e.last_error()));
    let message = match state {
        Some((super::descriptor::PluginState::Starting, _)) => {
            format!("{id} plugin is starting; retry shortly.")
        }
        Some((super::descriptor::PluginState::Stopping, _)) => {
            format!("{id} plugin is stopping.")
        }
        Some((super::descriptor::PluginState::Failed, Some(err))) => format!(
            "{id} plugin failed to start: {err}. See GET /api/plugins for the row; \
             POST /api/plugins/{id}/enable to retry."
        ),
        _ => format!(
            "{id} plugin is disabled by gateway.config.json (row {{\"{id}\":{{\"disabled\":true}}}}). \
             POST /api/plugins/{id}/enable to re-enable; its saved state is untouched."
        ),
    };
    admin_error(StatusCode::SERVICE_UNAVAILABLE, &message)
}

async fn list(State(host): State<Arc<PluginHost>>) -> Response {
    admin_json(StatusCode::OK, host.inventory())
}

async fn enable(
    State(host): State<Arc<PluginHost>>,
    Path(id): Path<String>,
    body: NodeBody,
) -> Response {
    toggle(&host, &id, body.0, true).await
}

async fn disable(
    State(host): State<Arc<PluginHost>>,
    Path(id): Path<String>,
    body: NodeBody,
) -> Response {
    toggle(&host, &id, body.0, false).await
}

/// Enable/disable: persist the DESIRED state first (revision-checked), then reconcile. A
/// failed start after an enable is NOT a failed request — the enable stands, and the failure
/// is on the row (`state: "failed"` + `lastError`), exactly where GET /api/plugins shows it.
async fn toggle(host: &Arc<PluginHost>, id: &str, body: Value, enabling: bool) -> Response {
    if host.plugin(id).is_none() {
        return admin_error(StatusCode::NOT_FOUND, &format!("unknown plugin: {id}"));
    }
    // An explicit `revision` in the body is honored as the CAS expectation; absent means "the
    // revision I just read" — the panel always has a fresh one from the inventory.
    let expected = body
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| host.revision());
    match host.store().set_plugin_disabled(id, expected, !enabling) {
        Err(ConfigStoreError::Conflict) => admin_error(
            StatusCode::CONFLICT,
            "configuration changed; reload before saving",
        ),
        Err(ConfigStoreError::Invalid(message)) => admin_error(StatusCode::BAD_REQUEST, &message),
        Err(ConfigStoreError::Persist(message)) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("persist failed: {message}"),
        ),
        Ok(snapshot) => {
            let _ = host.reconcile(id).await;
            let row = host
                .plugin(id)
                .map(|e| host.row(&e))
                .unwrap_or(Value::Null);
            admin_json(
                StatusCode::OK,
                json!({ "revision": snapshot.revision, "plugin": row }),
            )
        }
    }
}

async fn get_config(State(host): State<Arc<PluginHost>>, Path(id): Path<String>) -> Response {
    let Some(entry) = host.plugin(&id) else {
        return admin_error(StatusCode::NOT_FOUND, &format!("unknown plugin: {id}"));
    };
    admin_json(
        StatusCode::OK,
        json!({
            "revision": host.revision(),
            "config": host.store().plugin_config(&id),
            "schema": entry.descriptor().config_schema,
        }),
    )
}

async fn put_config(
    State(host): State<Arc<PluginHost>>,
    Path(id): Path<String>,
    body: NodeBody,
) -> Response {
    let Some(entry) = host.plugin(&id) else {
        return admin_error(StatusCode::NOT_FOUND, &format!("unknown plugin: {id}"));
    };
    let Some(config) = body.0.get("config").cloned() else {
        return admin_error(StatusCode::BAD_REQUEST, "config is required");
    };
    // Validate against the plugin BEFORE anything persists (docs/09 §5 — known-schema fields
    // are checked server-side; the frontend is never the authority).
    if let Err(err) = entry.factory().validate_config(&config) {
        return admin_error(StatusCode::BAD_REQUEST, &format!("invalid config: {err}"));
    }
    let expected = body
        .0
        .get("revision")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| host.revision());
    match host.store().update_plugin(&id, expected, config) {
        Err(ConfigStoreError::Conflict) => admin_error(
            StatusCode::CONFLICT,
            "configuration changed; reload before saving",
        ),
        Err(ConfigStoreError::Invalid(message)) => admin_error(StatusCode::BAD_REQUEST, &message),
        Err(ConfigStoreError::Persist(message)) => admin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("persist failed: {message}"),
        ),
        Ok(snapshot) => {
            // Reconcile applies what the plugin declares applicable — a restart for the
            // plugins whose config is read at start, a noted revision for the rest.
            let _ = host.reconcile(&id).await;
            admin_json(
                StatusCode::OK,
                json!({
                    "revision": snapshot.revision,
                    "config": host.store().plugin_config(&id),
                    "schema": entry.descriptor().config_schema,
                }),
            )
        }
    }
}
