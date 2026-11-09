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

//! The /api/remote management surface (SPEC §remote): which endpoints the transport
//! can see, and the target table CRUD. The CLI and the #remote targets page are
//! both primary clients - they drive the SAME routes - and the group list rides
//! with the rows so the page paints the SPEC §host.groups grouped list from one response.
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
        .route("/api/remote/runs/{id}/content", get(run_content))
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct RunsQuery {
    before: Option<String>,
    limit: Option<String>,
    target: Option<String>,
    /// SPEC §remote.history: epoch ms or ISO-8601, against endedAt; since inclusive, until exclusive.
    since: Option<String>,
    until: Option<String>,
    actor: Option<String>,
}

#[derive(serde::Deserialize)]
struct RunOutputQuery {
    after: Option<String>,
    max: Option<String>,
}

/// GET /api/remote/runs?before=&limit=&target=&since=&until=&actor=: one page of
/// recorded runs, newest first, plus the remote runs the coordinator holds right now
/// that are NOT yet in the record (queued / running) so the pane paints from one
/// read. `nextBefore` is the cursor for the older page; absent on the last one. The
/// budgets and what they currently hold ride along so the page can say what "kept"
/// means. A malformed `since` / `until` is 400: an audit that silently widened its
/// window would be worse than one that failed.
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
    let actor = query.actor.as_deref().filter(|a| !a.is_empty());
    let mut window = [None, None];
    for (slot, (name, raw)) in window
        .iter_mut()
        .zip([("since", &query.since), ("until", &query.until)])
    {
        let Some(raw) = raw.as_deref().filter(|r| !r.trim().is_empty()) else {
            continue;
        };
        match crate::history::parse_instant(raw) {
            Some(ms) => *slot = Some(ms),
            None => {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    &format!(
                        "{name} must be epoch milliseconds or an ISO-8601 instant, not {raw:?}"
                    ),
                )
            }
        }
    }
    let [since, until] = window;
    let history = system.history();
    let (runs, next_before) = history.query_records(crate::history::PageQuery {
        before,
        limit,
        target,
        since,
        until,
        actor,
    })
    .await;
    // Active = not terminal, less the ids this page's `runs` already carries: the
    // record commits a moment before the coordinator retires the run, and a read
    // in that moment listed it twice, once per list. They are happening now, so a
    // window that reaches now includes them; the actor predicate applies as it
    // does to the record.
    let now = swiss_core::util::now_ms();
    let recorded: std::collections::HashSet<u64> = runs
        .iter()
        .filter_map(|row| row.get("runId").and_then(Value::as_u64))
        .collect();
    let active: Vec<Value> = system
        .services()
        .runs
        .list()
        .into_iter()
        .filter(|v| v.action_type.starts_with("remote.") && !v.state.is_terminal())
        .filter(|v| !recorded.contains(&v.run_id))
        .map(|v| {
            let mut row = v.to_json(false);
            if let Some(input) = history.in_flight_input(v.run_id) {
                row["input"] = input;
            }
            row
        })
        .filter(|row| {
            target.is_none_or(|t| row["input"]["target"].as_str() == Some(t))
                && actor.is_none_or(|a| row["actor"].as_str() == Some(a))
                && until.is_none_or(|u| now < u)
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
            "auditWindowMs": limits.audit_window_ms,
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
    match system.history().get_record(run_id).await {
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
    let chunk = match history.output_record(run_id, after, max).await {
        Some(chunk) => chunk,
        None => {
            // No file: either the run never wrote (a record exists) or it is unknown.
            if history.get_record(run_id).await.is_none() {
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
    system.history().clear_records().await;
    admin_json(StatusCode::OK, json!({ "ok": true }))
}

/// GET /api/remote/runs/{id}/content: a remote.write's body, unsealed for the signed-in
/// panel (SPEC §remote.history). 404 when the record kept none - not a write, or the size budget
/// evicted it.
async fn run_content(State(state): State<Arc<RemoteState>>, Path(id): Path<String>) -> Response {
    let Some(system) = state.live() else {
        return not_running();
    };
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    match system.history().content_record(run_id).await {
        Ok(Some(kept)) => admin_json(
            StatusCode::OK,
            json!({
                "runId": run_id,
                "content": kept.text,
                "truncated": kept.truncated,
            }),
        ),
        Ok(None) => admin_error(
            StatusCode::NOT_FOUND,
            &format!("remote run {id} kept no written content"),
        ),
        Err(err) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &err),
    }
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
    // The group list rides with the rows (SPEC §remote.targets): the panel reads ONE response
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
    let written = system.with_store_saving(|store| match id {
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
    match system.with_store_saving(|store| store.remove(&id)) {
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
        // The names ride with the rows (SPEC §remote.targets): one response paints the grouped
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
        let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
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

    /// A sink that acknowledges only when released: the window this file's dedup
    /// lives in — the record has the run, the coordinator still holds it active —
    /// stays open for as long as the test needs to read it.
    struct HoldAck {
        asked: std::sync::Arc<std::sync::atomic::AtomicBool>,
        gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl swiss_host::services::runs::RunHistorySink for HoldAck {
        fn begin(
            &self,
            _run_id: u64,
            _request: &swiss_host::services::runs::SubmitRequest,
        ) -> Option<Arc<dyn swiss_host::services::action::RunOutputTee>> {
            Some(Arc::new(NullTee))
        }
        fn finish<'a>(
            &'a self,
            _view: swiss_host::services::runs::RunView,
            _request: swiss_host::services::runs::SubmitRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = swiss_host::services::runs::HistoryAck> + Send + 'a>,
        > {
            Box::pin(async move {
                self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
                let gate = self.gate.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                swiss_host::services::runs::HistoryAck::Recorded
            })
        }
    }

    struct NullTee;
    impl swiss_host::services::action::RunOutputTee for NullTee {
        fn write(&self, _bytes: &[u8]) {}
    }

    // SPEC §remote.history: the record commits a moment before the coordinator retires
    // the run; a read in that moment listed it twice, once in `runs` and once in
    // `active`. The ids the page already carries are left out.
    #[tokio::test]
    async fn a_run_already_recorded_is_not_listed_as_active() {
        let (system, fake) = crate::testing::system_with_fake();
        fake.program(
            "make",
            crate::testing::FakeProgram {
                argv0: "make".into(),
                stdout: b"ok\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        crate::actions::register_all(system.clone(), &system.services().actions).unwrap();
        // The record first (it files the run), the hold after it (it pins the run in
        // the coordinator past the commit): sink order is finish order.
        let record: Arc<dyn swiss_host::services::runs::RunHistorySink> =
            system.history().clone();
        system.services().runs.add_history_sink(record);
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let hold = Arc::new(HoldAck {
            asked: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            gate: std::sync::Mutex::new(Some(gate)),
        });
        system.services().runs.add_history_sink(hold.clone());
        let state = RemoteState::new();
        state.install(system.clone());
        let router = mount(state);

        let submitted = system
            .services()
            .runs
            .submit(swiss_host::services::runs::SubmitRequest {
                owner: "manual".into(),
                label: "build".into(),
                action_type: "remote.exec".into(),
                input: json!({ "target": "dev", "argv": ["make"] }),
                timeout_ms: 30_000,
                queue_if_busy: false,
                actor: "test".into(),
            })
            .expect("submitted");
        for _ in 0..500 {
            if hold.asked.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert!(
            hold.asked.load(std::sync::atomic::Ordering::SeqCst),
            "the run reached its sinks"
        );

        // The moment both lists could see it: the record holds it AND the coordinator
        // still reports it running. It appears exactly once.
        let (status, page) = get_json(&router, "/api/remote/runs").await;
        assert_eq!(status, StatusCode::OK);
        let listed = page["runs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(page["active"].as_array().unwrap())
            .filter(|row| row["runId"].as_u64() == Some(submitted.run_id))
            .count();
        assert_eq!(listed, 1, "the run is not listed twice: {page}");
        assert_eq!(page["runs"].as_array().unwrap().len(), 1);
        assert_eq!(page["active"], json!([]));

        release.send(()).expect("release the hold");
        let view = submitted.done.await.expect("the final view");
        assert_eq!(view.state.as_str(), "succeeded");
        let _ = std::fs::remove_dir_all(
            system
                .history()
                .dir()
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap(),
        );
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

    /// REMOTE.TARGETS notices (SPEC §remote.targets): a subscribed link, its frames
    /// decoded. Same harness shape as the host's seat-notice tests.
    mod target_notices {
        use super::*;
        use prost::Message as _;
        use swiss_bus::IoMsg;
        use swiss_bus::pb::v1;
        use tokio::sync::mpsc;

        struct Channel {
            rx: mpsc::Receiver<IoMsg>,
            tx: mpsc::Sender<IoMsg>,
        }

        impl swiss_bus::Io for Channel {
            fn recv(
                &mut self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<IoMsg>> + Send + '_>>
            {
                Box::pin(async move { self.rx.recv().await })
            }
            fn send(
                &mut self,
                msg: IoMsg,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
                Box::pin(async move { self.tx.send(msg).await.is_ok() })
            }
        }

        /// One link subscribed to the target table's notices, opened before the write.
        struct Peer {
            tx: mpsc::Sender<IoMsg>,
            rx: mpsc::Receiver<IoMsg>,
            seq: u32,
        }

        impl Peer {
            async fn open(bus: &Arc<swiss_bus::Bus>) -> Self {
                let (tx, rx) = mpsc::channel(32);
                let (out, incoming) = mpsc::channel(32);
                tokio::spawn(swiss_bus::run(bus.clone(), Channel { rx, tx: out }));
                let mut peer = Self { tx, rx: incoming, seq: 1 };
                let welcome = peer
                    .request_at(
                        0,
                        v1::BusSub::Link as u32,
                        v1::LinkId::Hello as u32,
                        v1::Hello {
                            protocol: 1,
                            page_id: "page-a".
                                into(),
                            build: "test".into(),
                        }
                        .encode_to_vec(),
                    )
                    .await;
                assert!(welcome.fault.is_none());
                let answer = peer
                    .request_at(
                        0,
                        v1::BusSub::Topic as u32,
                        v1::TopicId::Subscribe as u32,
                        v1::Topics {
                            topics: vec![v1::Topic {
                                module: v1::Module::Remote as u32,
                                sub: v1::RemoteSub::Targets as u32,
                                key: String::new(),
                            }],
                        }
                        .encode_to_vec(),
                    )
                    .await;
                assert!(answer.fault.is_none());
                peer
            }

            async fn next(&mut self) -> v1::Frame {
                loop {
                    match self.rx.recv().await.expect("link frame") {
                        IoMsg::Binary(bytes) => {
                            return v1::Frame::decode(bytes.as_slice()).expect("frame");
                        }
                        IoMsg::Ping(_) | IoMsg::Pong(_) => continue,
                        other => panic!("expected binary, got {other:?}"),
                    }
                }
            }

            async fn request_at(&mut self, module: u32, sub: u32, id: u32, body: Vec<u8>) -> v1::Frame {
                let seq = self.seq;
                self.seq += 1;
                self.tx
                    .send(IoMsg::Binary(
                        v1::Frame {
                            module,
                            sub,
                            id,
                            kind: v1::Kind::Request as i32,
                            seq,
                            body,
                            ..Default::default()
                        }
                        .encode_to_vec(),
                    ))
                    .await
                    .expect("send");
                loop {
                    let frame = self.next().await;
                    if frame.kind == v1::Kind::Event as i32 {
                        continue;
                    }
                    assert_eq!(frame.reply_to, seq);
                    return frame;
                }
            }

            async fn notice(&mut self) {
                let frame = self.next().await;
                assert_eq!(frame.module, v1::Module::Remote as u32);
                assert_eq!(frame.sub, v1::RemoteSub::Targets as u32);
                assert_eq!(frame.id, v1::TargetsId::Changed as u32);
                assert_eq!(frame.kind, v1::Kind::Event as i32);
                assert!(frame.body.is_empty(), "a notice carries no body");
            }

            fn drained(&mut self) -> bool {
                matches!(self.rx.try_recv(), Err(mpsc::error::TryRecvError::Empty))
            }
        }

        async fn settle() {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }

        #[tokio::test]
        async fn every_target_write_publishes_once_and_a_refusal_publishes_nothing() {
            let (state, _system, router) = app();
            let bus = state.live().expect("the system").services().bus.clone();
            let mut peer = Peer::open(&bus).await;

            let (status, _body) = send(
                &router,
                "POST",
                "/api/remote/targets",
                row("build", "conn-1"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            settle().await;
            peer.notice().await;

            let mut edited = row("build", "conn-1");
            edited["workspaceRoot"] = json!("/srv/other");
            let (status, _body) = send(
                &router,
                "POST",
                "/api/remote/targets/build",
                edited,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let (status, _body) =
                send(&router, "DELETE", "/api/remote/targets/build", Value::Null).await;
            assert_eq!(status, StatusCode::OK);
            settle().await;
            peer.notice().await;
            peer.notice().await;

            // A duplicate add is refused at the door: the table did not move, so the
            // topic stays silent (SPEC §remote.targets).
            let (status, _body) = send(
                &router,
                "POST",
                "/api/remote/targets",
                row("dev", "conn-1"),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            // A parse refusal either.
            let mut bad = row("x", "conn-1");
            bad["password"] = json!("hunter2");
            let (status, _body) = send(&router, "POST", "/api/remote/targets", bad).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            settle().await;
            assert!(peer.drained(), "refused writes announce nothing");
        }

        #[tokio::test]
        async fn a_group_write_publishes_like_a_route_write() {
            let (state, system, router) = app();
            let bus = state.live().expect("the system").services().bus.clone();
            let mut peer = Peer::open(&bus).await;

            let (status, _body) = get_json(&router, "/api/remote/targets").await;
            assert_eq!(status, StatusCode::OK);
            settle().await;
            assert!(peer.drained(), "a read announces nothing");

            // The targets scope's family operations go through the same saving path
            // (groups.rs), so a rename moves the topic too (SPEC §remote.targets).
            system
                .with_store_saving(|s| s.set_group_names(&["default".into(), "prod".into()]))
                .expect("groups");
            settle().await;
            peer.notice().await;
            assert!(peer.drained());
        }
    }
}
