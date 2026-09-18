//! The /api/remote management surface (docs/34): which endpoints the transport
//! can see, and the target table CRUD. The CLI and the #remote targets page are
//! both primary clients - they drive the SAME routes - and the group list rides
//! with the rows so the page paints the docs/20 grouped list from one response.
//!
//! ## State: one slot the plugin lifecycle owns
//!
//! [RemoteState] follows the terminal plugin precedent: the routes are mounted
//! once at boot (inside the loopback guard and the plugin boundary, in server.rs),
//! but the [RemoteSystem] they drive exists only while the remote plugin is
//! serving - start installs it, stop withdraws it. The plugin boundary answers
//! the structured 503 for a disabled plugin; the handlers check the slot anyway,
//! because a mounted router must never assume who mounted it.
//!
//! ## Endpoint validation policy
//!
//! When a transport provider is SERVING, a target may only name an endpoint the
//! provider lists (a typo fails at save, not at exec). When no provider is serving,
//! a non-empty id is accepted: the tunnels plugin being off must not lock the target
//! table, and exec says honestly what is missing when it runs.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use swiss_host::reply::{admin_error, admin_json};

use crate::history::{OutputChunk, PAGE_SIZE};

use crate::target::RemoteTarget;
use crate::RemoteSystem;

/// The slot the remote plugin installs the system into and the routes read.
pub struct RemoteState {
    system: RwLock<Option<Arc<RemoteSystem>>>,
}

impl RemoteState {
    pub fn new() -> Arc<Self> {
        Arc::new(RemoteState {
            system: RwLock::new(None),
        })
    }

    /// The plugin's start: publish the system the routes will talk to.
    pub fn install(&self, system: Arc<RemoteSystem>) {
        let mut slot = self.system.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(system);
    }

    /// The plugin's stop: take the system back so no request can reach a dying one.
    pub fn withdraw(&self) -> Option<Arc<RemoteSystem>> {
        self.system
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    fn live(&self) -> Option<Arc<RemoteSystem>> {
        self.system
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Mount the /api/remote tree. The caller merges this into the extra tree BEFORE
/// build_app folds it under the loopback guard and the plugin boundary, for the
/// same reason the terminal tree does: a later merge would sit outside the auth.
pub fn mount(state: Arc<RemoteState>) -> Router<()> {
    Router::new()
        .route("/api/remote/endpoints", get(endpoints))
        .route("/api/remote/targets", get(list_targets).post(add_target))
        .route(
            "/api/remote/targets/{id}",
            get(get_target).post(update_target).delete(delete_target),
        )
        // The run log (history.rs): the durable record the panel's Runs pane reads.
        // Live runs stay on the host's /api/runs; a history row is the same shape.
        .route("/api/remote/runs", get(list_runs).delete(clear_runs))
        .route("/api/remote/runs/{id}", get(get_run))
        .route("/api/remote/runs/{id}/output", get(run_output))
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct RunsQuery {
    before: Option<String>,
    limit: Option<String>,
    target: Option<String>,
}

#[derive(serde::Deserialize)]
struct RunOutputQuery {
    after: Option<String>,
    max: Option<String>,
}

/// GET /api/remote/runs?before=&limit=&target=: one page of recorded runs, newest
/// first, plus the remote runs the coordinator holds right now that are NOT yet in
/// the record (queued / running) so the pane paints from one read. `nextBefore` is
/// the cursor for the older page; absent on the last one. The budgets and what they
/// currently hold ride along so the page can say what "kept" means.
async fn list_runs(
    State(state): State<Arc<RemoteState>>,
    Query(query): Query<RunsQuery>,
) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let before = query.before.as_deref().and_then(|b| b.parse::<u64>().ok());
    let limit = query
        .limit
        .as_deref()
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(PAGE_SIZE);
    let target = query.target.as_deref().filter(|t| !t.is_empty());
    let history = system.history();
    let (runs, next_before) = history.page(before, limit, target);
    // Active = not terminal: the record only ever holds finished runs, so there is
    // no overlap to reconcile.
    let active: Vec<Value> = system
        .services()
        .runs
        .list()
        .into_iter()
        .filter(|v| v.action_type.starts_with("remote.") && !v.state.is_terminal())
        .map(|v| {
            let mut row = v.to_json(false);
            if let Some(input) = history.in_flight_input(v.run_id) {
                row["input"] = input;
            }
            row
        })
        .filter(|row| {
            target.is_none_or(|t| row["input"]["target"].as_str() == Some(t))
        })
        .collect();
    let limits = history.limits();
    let (bytes, count) = history.usage();
    let mut body = json!({
        "runs": runs,
        "active": active,
        "limits": {
            "maxAgeMs": limits.max_age_ms,
            "maxTotalBytes": limits.max_total_bytes,
            "maxRuns": limits.max_runs,
            "maxOutputBytes": limits.max_output_bytes,
        },
        "usage": { "bytes": bytes, "runs": count },
    });
    if let Some(next) = next_before {
        body["nextBefore"] = json!(next);
    }
    admin_json(StatusCode::OK, body)
}

/// GET /api/remote/runs/{id}: one recorded run — the row plus `tail` when its output
/// file was capped.
async fn get_run(State(state): State<Arc<RemoteState>>, Path(id): Path<String>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    match system.history().get(run_id) {
        Some(record) => admin_json(StatusCode::OK, record),
        None => unknown_run(&id),
    }
}

/// GET /api/remote/runs/{id}/output?after=&max=: the recorded output from a byte
/// cursor, in the live route's shape (`cursor` / `nextCursor` / `output` / `terminal`)
/// plus `total`, so the panel reads a finished run with the reader it follows a live
/// one with. A run that ran silently answers an empty chunk; an unknown id 404s.
async fn run_output(
    State(state): State<Arc<RemoteState>>,
    Path(id): Path<String>,
    Query(query): Query<RunOutputQuery>,
) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    let after = query.after.and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
    let max = query
        .max
        .and_then(|m| m.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let history = system.history();
    let chunk = match history.output(run_id, after, max) {
        Some(chunk) => chunk,
        None => {
            // No file: either the run never wrote (a record exists) or it is unknown.
            if history.get(run_id).is_none() {
                return unknown_run(&id);
            }
            OutputChunk {
                text: String::new(),
                cursor: 0,
                next_cursor: 0,
                total: 0,
            }
        }
    };
    admin_json(
        StatusCode::OK,
        json!({
            "runId": run_id,
            "cursor": chunk.cursor,
            "nextCursor": chunk.next_cursor,
            "output": chunk.text,
            "total": chunk.total,
            "truncated": false,
            "terminal": true,
        }),
    )
}

/// DELETE /api/remote/runs: forget the whole record — the pane's Clear.
async fn clear_runs(State(state): State<Arc<RemoteState>>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    system.history().clear();
    admin_json(StatusCode::OK, json!({ "ok": true }))
}

fn unknown_run(id: &str) -> Response {
    admin_error(
        StatusCode::NOT_FOUND,
        &format!("no recorded remote run {id}"),
    )
}

/// The defensive branch: the boundary should have answered this, but a mounted
/// router never assumes its mounter.
fn not_running() -> Response {
    admin_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "the remote plugin is not running",
    )
}

/// GET /api/remote/endpoints: the transport's live endpoint list plus its
/// presence, so a client can say "start Tunnels" instead of guessing.
async fn endpoints(State(state): State<Arc<RemoteState>>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let registry = &system.services().remote;
    let presence = registry.presence();
    admin_json(
        StatusCode::OK,
        json!({
            "presence": presence.as_str(),
            "provider": match &presence {
                swiss_host::services::remote::RemotePresence::Serving(id)
                | swiss_host::services::remote::RemotePresence::Stopping(id)
                | swiss_host::services::remote::RemotePresence::Absent(Some(id)) => json!(id),
                _ => Value::Null,
            },
            "endpoints": registry
                .list()
                .into_iter()
                .map(|e| json!({ "id": e.id, "label": e.label, "state": e.state }))
                .collect::<Vec<_>>(),
        }),
    )
}

async fn list_targets(State(state): State<Arc<RemoteState>>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let (rows, groups) = system.with_store(|store| {
        (
            store
                .list()
                .iter()
                .map(RemoteTarget::to_json)
                .collect::<Vec<_>>(),
            store.group_names(),
        )
    });
    // The group list rides with the rows (docs/34 R8): the panel reads ONE response
    // to paint the grouped page, exactly like /api/tunnels carries connGroups.
    admin_json(StatusCode::OK, json!({ "targets": rows, "groups": groups }))
}

async fn get_target(State(state): State<Arc<RemoteState>>, Path(id): Path<String>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    match system.with_store(|store| store.get(&id).map(RemoteTarget::to_json)) {
        Some(row) => admin_json(StatusCode::OK, row),
        None => admin_error(
            StatusCode::NOT_FOUND,
            &format!("target {id} does not exist"),
        ),
    }
}

/// The shared write path: strict parse, endpoint check, store write. 400 with the
/// named reason on refusal - the convention across every management route here.
async fn write_target(state: &Arc<RemoteState>, id: Option<&str>, body: Value) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let target = match RemoteTarget::from_json(&body) {
        Ok(target) => target,
        Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
    };
    if let Some(id) = id {
        if target.id != id {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!("cannot rename target {id} to {}", target.id),
            );
        }
    }
    // Endpoint validation only when the transport can actually answer it.
    let registry = &system.services().remote;
    if registry.has_provider() {
        let known: Vec<String> = registry.list().into_iter().map(|e| e.id).collect();
        if !known.contains(&target.endpoint) {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "endpoint {} is not one the transport serves (known: {})",
                    target.endpoint,
                    known.join(", "),
                ),
            );
        }
    }
    let written = system.with_store(|store| match id {
        Some(id) => store.update(id, target.clone()).map(|_| ()),
        None => store.add(target.clone()).map(|_| ()),
    });
    match written {
        Ok(()) => admin_json(StatusCode::OK, target.to_json()),
        Err(err) => admin_error(StatusCode::BAD_REQUEST, &err),
    }
}

async fn add_target(State(state): State<Arc<RemoteState>>, Json(body): Json<Value>) -> Response {
    write_target(&state, None, body).await
}

async fn update_target(
    State(state): State<Arc<RemoteState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    write_target(&state, Some(&id), body).await
}

async fn delete_target(State(state): State<Arc<RemoteState>>, Path(id): Path<String>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    match system.with_store(|store| store.remove(&id)) {
        Ok(()) => admin_json(StatusCode::OK, json!({ "removed": id })),
        Err(message) => admin_error(StatusCode::NOT_FOUND, &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A state with the fake transport installed, the shape the plugin produces.
    fn app() -> (Arc<RemoteState>, Arc<RemoteSystem>, Router) {
        let (system, _fake) = crate::testing::system_with_fake();
        let state = RemoteState::new();
        state.install(system.clone());
        let router = mount(state.clone());
        (state, system, router)
    }

    /// A state with NOTHING installed: the plugin-not-serving shape.
    fn empty_app() -> Router {
        mount(RemoteState::new())
    }

    async fn get_json(router: &Router, path: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn send(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn row(id: &str, endpoint: &str) -> Value {
        json!({
            "id": id,
            "endpoint": endpoint,
            "workspaceRoot": "/data/ws/proj",
            "shell": "posix",
            "capabilities": ["exec", "sync"],
        })
    }

    #[tokio::test]
    async fn an_empty_slot_answers_service_unavailable_not_a_panic() {
        let router = empty_app();
        let (status, _body) = get_json(&router, "/api/remote/endpoints").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let (status, _body) = get_json(&router, "/api/remote/targets").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn endpoints_reports_the_fake_and_its_presence() {
        let (_state, _system, router) = app();
        let (status, body) = get_json(&router, "/api/remote/endpoints").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["presence"], "serving");
        assert_eq!(
            body["endpoints"][0]["id"], "conn-1",
            "the endpoint list is the transport's own",
        );
    }

    #[tokio::test]
    async fn targets_round_trip_and_duplicates_are_refused() {
        let (_state, _system, router) = app();
        let (status, _body) = send(
            &router,
            "POST",
            "/api/remote/targets",
            row("build", "conn-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = send(
            &router,
            "POST",
            "/api/remote/targets",
            row("build", "conn-1"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("already exists"));
        let (status, body) = get_json(&router, "/api/remote/targets").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["targets"].as_array().unwrap().len(), 2, "dev + build");
        let (status, body) = get_json(&router, "/api/remote/targets/build").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "build");
    }
    #[tokio::test]
    async fn the_list_carries_the_group_names_and_rows_can_join() {
        let (_state, system, router) = app();
        // The names ride with the rows (docs/34 R8): one response paints the grouped
        // page, and an ungrouped table answers the single default group.
        let (status, body) = get_json(&router, "/api/remote/targets").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["groups"], json!(["default"]));

        // A group exists (the family PUT owns that); a row can be born INTO it, and
        // the casing the caller typed is canonicalized at the door.
        system
            .with_store(|s| s.set_group_names(&["default".into(), "prod".into()]))
            .expect("groups");
        let mut born = row("build", "conn-1");
        born["group"] = json!("PROD");
        let (status, _body) = send(&router, "POST", "/api/remote/targets", born).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = get_json(&router, "/api/remote/targets/build").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["group"], json!("prod"), "the canonical spelling");

        // A group the list does not carry is a named 400, not a silent drop.
        let mut stray = row("flash", "conn-1");
        stray["group"] = json!("ghost");
        let (status, body) = send(&router, "POST", "/api/remote/targets", stray).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap().contains("unknown group"),
            "{}",
            body["error"]
        );
    }

    #[tokio::test]
    async fn an_unknown_endpoint_is_refused_while_the_transport_serves() {
        let (_state, _system, router) = app();
        let (status, body) =
            send(&router, "POST", "/api/remote/targets", row("build", "typo")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err = body["error"].as_str().unwrap();
        assert!(err.contains("typo"), "{err}");
        assert!(
            err.contains("conn-1"),
            "the message names what IS known: {err}"
        );
    }

    #[tokio::test]
    async fn without_a_transport_any_nonempty_endpoint_is_accepted() {
        // The plugin-missing shape: the table stays writable, exec will explain.
        swiss_core::secure::key::use_test_master_key();
        let services = swiss_host::services::RuntimeServices::new();
        let dir =
            std::env::temp_dir().join(format!("swiss-rapi-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let system = RemoteSystem::open(services, dir.join("remote.json"));
        let state = RemoteState::new();
        state.install(system);
        let router = mount(state);
        let (status, _body) = send(
            &router,
            "POST",
            "/api/remote/targets",
            row("build", "later-conn"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn updates_cannot_rename_and_delete_removes() {
        let (_state, _system, router) = app();
        send(
            &router,
            "POST",
            "/api/remote/targets",
            row("build", "conn-1"),
        )
        .await;
        let (status, _body) = send(
            &router,
            "POST",
            "/api/remote/targets/build",
            row("other", "conn-1"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let mut edited = row("build", "conn-1");
        edited["workspaceRoot"] = json!("/srv/other");
        let (status, body) = send(&router, "POST", "/api/remote/targets/build", edited).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["workspaceRoot"], "/srv/other");
        let (status, _body) =
            send(&router, "DELETE", "/api/remote/targets/build", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _body) = get_json(&router, "/api/remote/targets/build").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn credential_fields_are_refused_by_the_route_too() {
        let (_state, _system, router) = app();
        let mut body = row("build", "conn-1");
        body["password"] = json!("hunter2");
        let (status, value) = send(&router, "POST", "/api/remote/targets", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(value["error"].as_str().unwrap().contains("Tunnels"));
    }
}
