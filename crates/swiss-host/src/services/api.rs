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

//! The generic execution surface: `/api/actions` and `/api/runs` (docs/10 §7).
//!
//! Host-owned, not plugin-owned. The capability LIST changes as providers come and go — a
//! stopped process plugin withdraws its actions and they leave the listing — but the routes
//! themselves stay up: a caller must always be able to ask what is available and to read the
//! history of runs whose provider has since been disabled.
//!
//! Two rules shape the handlers:
//!
//! - A submission returns a runId IMMEDIATELY (202). An HTTP request never owns a background
//!   task's lifetime; closing the page does not cancel the run, and the run does not hold a
//!   connection open. Progress is read back through `/api/runs/{id}`.
//! - A refusal is VISIBLE and typed: an unknown capability is a 400, a full pool is a 429
//!   naming the bound. Nothing ever reports a run it did not start.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};

use crate::reply::{admin_error, admin_json, NodeBody};
use crate::services::runs::{SubmitError, SubmitRequest};
use crate::services::RuntimeServices;

/// The prefixes this tree owns, reserved against plugin route claims like `/api/plugins`.
pub const ROUTES: &[&str] = &["/api/actions", "/api/runs"];

/// Manual runs are attributed to this producer; cancellation and shutdown are scoped by it,
/// so stopping the Jobs plugin never touches a run someone started by hand.
pub const MANUAL_OWNER: &str = "manual";

/// The default deadline for a manual run, matching the docs' 10-minute job default. A
/// request may lower it; it may not remove it.
const DEFAULT_TIMEOUT_MS: u64 = 600_000;
/// Ceiling on a caller-supplied deadline: a run that cannot be given up on is exactly the
/// wedge the deadline exists to prevent.
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

pub fn mount(services: Arc<RuntimeServices>) -> Router {
    Router::new()
        .route("/api/actions", get(list_actions))
        .route("/api/runs", get(list_runs).post(submit_run))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/runs/{id}/cancel", post(cancel_run))
        .route("/api/runs/{id}/output", get(run_output))
        .with_state(services)
}

/// Every capability currently registered, with the schema a form or a job definition is
/// written against. Providers that are stopped are simply absent — the listing is the live
/// truth, never a compiled-in catalogue.
async fn list_actions(State(services): State<Arc<RuntimeServices>>) -> Response {
    let actions: Vec<Value> = services
        .actions
        .list()
        .into_iter()
        .map(|a| {
            json!({
                "type": a.type_name,
                "title": a.title,
                "provider": a.provider,
                "cancelable": a.cancelable,
                "schema": a.schema,
            })
        })
        .collect();
    admin_json(StatusCode::OK, json!({ "actions": actions }))
}

async fn list_runs(State(services): State<Arc<RuntimeServices>>) -> Response {
    let capacity = services.runs.capacity();
    let runs: Vec<Value> = services
        .runs
        .list()
        .iter()
        .map(|v| v.to_json(false))
        .collect();
    admin_json(
        StatusCode::OK,
        json!({
            "runs": runs,
            "capacity": {
                "maxConcurrentRuns": capacity.max_concurrent,
                "maxQueuedRuns": capacity.max_queued,
                "maxRemoteRunsPerTarget": services.runs.remote_per_target(),
            },
        }),
    )
}

/// Submit one manual run. 202 + the runId: the work outlives this request by design.
async fn submit_run(State(services): State<Arc<RuntimeServices>>, body: NodeBody) -> Response {
    let Some(action_type) = body.0.get("action").and_then(Value::as_str) else {
        return admin_error(
            StatusCode::BAD_REQUEST,
            "action is required (see GET /api/actions)",
        );
    };
    let timeout_ms = match body.0.get("timeoutMs") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_MS,
        Some(v) => match v.as_u64() {
            Some(ms) if ms > 0 && ms <= MAX_TIMEOUT_MS => ms,
            _ => {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    &format!("timeoutMs must be a positive integer up to {MAX_TIMEOUT_MS}"),
                )
            }
        },
    };
    // Who is asking (docs/41 A1). /api's credential (docs/48) says "this machine's CLI or a
    // signed-in browser", not which person, so this is the caller's own word: the CLI sends cli:<user>@<host>, the panel sends
    // panel; anything else that omits it is recorded as api.
    let actor = body
        .0
        .get("actor")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|a| !a.is_empty() && a.len() <= 128)
        .unwrap_or("api")
        .to_string();
    let request = SubmitRequest {
        owner: MANUAL_OWNER.to_string(),
        label: body
            .0
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(action_type)
            .to_string(),
        action_type: action_type.to_string(),
        input: body.0.get("input").cloned().unwrap_or_else(|| json!({})),
        timeout_ms,
        // A manual submission is answered now: it queues only if the caller says so, and
        // otherwise gets the capacity error rather than an invisible wait.
        queue_if_busy: body
            .0
            .get("queueIfBusy")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        actor,
    };
    match services.runs.submit(request) {
        Ok(submitted) => {
            let view = services
                .runs
                .get(submitted.run_id)
                .map(|v| v.to_json(false))
                .unwrap_or(Value::Null);
            admin_json(
                StatusCode::ACCEPTED,
                json!({ "runId": submitted.run_id, "run": view }),
            )
        }
        Err(SubmitError::Action(message)) => admin_error(StatusCode::BAD_REQUEST, &message),
        Err(SubmitError::Capacity(message)) => admin_error(StatusCode::TOO_MANY_REQUESTS, &message),
    }
}

#[derive(serde::Deserialize)]
struct RunQuery {
    /// `?output=0` drops the tail from a single-run read (a poller that only watches state).
    output: Option<String>,
}

async fn get_run(
    State(services): State<Arc<RuntimeServices>>,
    Path(id): Path<String>,
    Query(query): Query<RunQuery>,
) -> Response {
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    let include_output = !matches!(query.output.as_deref(), Some("0") | Some("false"));
    match services.runs.get(run_id) {
        Some(view) => admin_json(StatusCode::OK, view.to_json(include_output)),
        None => unknown_run(&id),
    }
}

/// Cancel and WAIT: when this answers the run is terminal — its child reaped, its readers
/// joined. A run that already finished answers its finished view (first-wins), not an error.
async fn cancel_run(
    State(services): State<Arc<RuntimeServices>>,
    Path(id): Path<String>,
) -> Response {
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    match services.runs.cancel(run_id).await {
        Some(view) => admin_json(StatusCode::OK, view.to_json(false)),
        None => unknown_run(&id),
    }
}

fn unknown_run(id: &str) -> Response {
    admin_error(StatusCode::NOT_FOUND, &format!("no run {id}"))
}

#[derive(serde::Deserialize)]
struct OutputQuery {
    /// The cursor a previous read ended on; absent = from the oldest retained byte.
    after: Option<String>,
    /// A caller's own cap on one read; the server clamps it to its maximum anyway.
    max: Option<String>,
}

/// One bounded slice of a run's LIVE output (docs/34 §17): while the run is executing,
/// what it appended so far; once finished, the retained tail. The response carries a
/// monotonic cursor — poll again with it as `?after`. A cursor older than the retained
/// window reads the oldest kept bytes with `truncated: true`, never a silent gap.
async fn run_output(
    State(services): State<Arc<RuntimeServices>>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> Response {
    let Some(run_id) = id.parse::<u64>().ok() else {
        return unknown_run(&id);
    };
    let after = query.after.and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
    let max = query
        .max
        .and_then(|m| m.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    match services.runs.output(run_id, after, max) {
        Some((state, chunk)) => admin_json(
            StatusCode::OK,
            json!({
                "runId": run_id,
                "state": state.as_str(),
                "cursor": chunk.cursor,
                "nextCursor": chunk.next_cursor,
                "output": chunk.text,
                "truncated": chunk.truncated,
                "terminal": state.is_terminal(),
            }),
        ),
        None => unknown_run(&id),
    }
}
