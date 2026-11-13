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

//! The /api/lane routes (SPEC §lane.api).
//!
//! - `GET /api/lane`: the queue as the page shows it.
//! - `POST /api/lane/enter`: take a place. The answer is the lease itself, an NDJSON stream
//!   that stays open while the caller holds or waits for a slot: a `waiting` line whenever the
//!   place moves, one `running` line when the slot is granted, a bare newline every few seconds
//!   so a vanished caller is noticed, and `released` or `closed` as a last line. The caller's
//!   connection IS the lease: when it drops, the place is given up.
//! - `POST /api/lane/{id}/done`: the command ended; the slot passes on.
//! - `POST /api/lane/{id}/release`: the page lets a command go without waiting for it.
//!
//! The caller merges this router into the extra tree BEFORE build_app folds it under the
//! loopback guard and the plugin boundary, like every plugin's routes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Map, Value};
use tokio::sync::watch;
use tokio::time::Instant;

use swiss_host::reply::{admin_error, admin_error_code, admin_json, NodeBody};

use super::{Lane, LaneSlot, Seat, Ticket};

/// How long a lease stays quiet before it writes a bare newline. A write to a closed socket is
/// how the gateway learns the caller is gone, so this bounds how long a dead waiter can hold
/// up the queue.
pub const HEARTBEAT: Duration = Duration::from_secs(5);

pub fn mount(slot: Arc<LaneSlot>) -> Router<()> {
    Router::new()
        .route("/api/lane", get(view))
        .route("/api/lane/enter", post(enter))
        .route("/api/lane/{id}/done", post(done))
        .route("/api/lane/{id}/release", post(release))
        .with_state(slot)
}

/// The defensive branch: the boundary answers a stopped plugin first, but a mounted router
/// never assumes its mounter.
fn not_running() -> Response {
    admin_error(StatusCode::SERVICE_UNAVAILABLE, "the lane plugin is not running")
}

fn unknown(id: u64) -> Response {
    admin_error_code(
        StatusCode::NOT_FOUND,
        "lane.unknown",
        &format!("the lane holds no command {id}: it finished, was released, or never entered"),
    )
}

async fn view(State(slot): State<Arc<LaneSlot>>) -> Response {
    match slot.current() {
        Some(lane) => admin_json(StatusCode::OK, lane.snapshot()),
        None => not_running(),
    }
}

async fn enter(State(slot): State<Arc<LaneSlot>>, NodeBody(body): NodeBody) -> Response {
    let Some(lane) = slot.current() else {
        return not_running();
    };
    let (ticket, resume) = match Ticket::from_body(&body) {
        Ok(parsed) => parsed,
        Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
    };
    // Subscribe before entering, so the change that admits this entry cannot slip past.
    let changed = lane.subscribe();
    let id = lane.enter(ticket, resume);
    let holder = Holder {
        lane,
        id,
        changed,
        last: None,
        ended: false,
        quiet_until: Instant::now() + HEARTBEAT,
    };
    let stream = futures_util::stream::unfold(holder, next_line);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

async fn done(
    State(slot): State<Arc<LaneSlot>>,
    Path(id): Path<u64>,
    NodeBody(body): NodeBody,
) -> Response {
    let Some(lane) = slot.current() else {
        return not_running();
    };
    let exit = match body.get("exit") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64().and_then(|n| i32::try_from(n).ok()) {
            Some(code) => Some(code),
            None => return admin_error(StatusCode::BAD_REQUEST, "exit must be an exit code"),
        },
    };
    if lane.finish(id, exit) {
        admin_json(StatusCode::OK, json!({ "ok": true }))
    } else {
        unknown(id)
    }
}

async fn release(State(slot): State<Arc<LaneSlot>>, Path(id): Path<u64>) -> Response {
    let Some(lane) = slot.current() else {
        return not_running();
    };
    if lane.release(id) {
        admin_json(StatusCode::OK, json!({ "ok": true }))
    } else {
        unknown(id)
    }
}

/// One lease being streamed. Dropping it (the caller went away, or the stream ended) gives the
/// place up; after a `done` or a release that is a no-op.
struct Holder {
    lane: Arc<Lane>,
    id: u64,
    changed: watch::Receiver<u64>,
    /// The seat the caller last heard, so a change elsewhere in the queue that leaves this
    /// place as it was sends nothing.
    last: Option<Seat>,
    /// The last line went out; the next poll ends the stream.
    ended: bool,
    /// When the next heartbeat is due if nothing else is written first.
    quiet_until: Instant,
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.lane.leave(self.id);
    }
}

async fn next_line(mut holder: Holder) -> Option<(Result<Bytes, Infallible>, Holder)> {
    if holder.ended {
        return None;
    }
    loop {
        // Mark the revision seen BEFORE reading the seat: a change after the read wakes the
        // wait below instead of being lost between the two.
        holder.changed.borrow_and_update();
        let seat = holder.lane.seat(holder.id);
        // Caps reach a command when it is admitted (SPEC §lane.config): once the running line
        // went out, a new row's caps are not news to this holder, so it hears one running line.
        let heard = match (&holder.last, &seat) {
            (Some(Seat::Running { .. }), Seat::Running { .. }) => true,
            (last, seat) => last.as_ref() == Some(seat),
        };
        if !heard {
            let (line, last) = match seat_line(holder.id, &seat) {
                Some(line) => line,
                // Done and gone: the caller reported its end, nothing more to say.
                None => return None,
            };
            holder.ended = last;
            holder.last = Some(seat);
            holder.quiet_until = Instant::now() + HEARTBEAT;
            return Some((Ok(line), holder));
        }
        tokio::select! {
            moved = holder.changed.changed() => {
                if moved.is_err() {
                    return None;
                }
            }
            _ = tokio::time::sleep_until(holder.quiet_until) => {
                holder.quiet_until = Instant::now() + HEARTBEAT;
                return Some((Ok(Bytes::from_static(b"\n")), holder));
            }
        }
    }
}

/// The line a seat sends, and whether it is the stream's last. `None` for a place the lane no
/// longer knows: the command reported its end.
fn seat_line(id: u64, seat: &Seat) -> Option<(Bytes, bool)> {
    let (value, last) = match seat {
        Seat::Waiting { ahead, running } => (
            json!({ "id": id, "state": "waiting", "ahead": ahead, "running": running }),
            false,
        ),
        Seat::Running { env } => {
            let env: Map<String, Value> =
                env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
            (json!({ "id": id, "state": "running", "env": env }), false)
        }
        Seat::Released => (json!({ "id": id, "state": "released" }), true),
        Seat::Closed => (json!({ "id": id, "state": "closed" }), true),
        Seat::Unknown => return None,
    };
    let mut line = value.to_string();
    line.push('\n');
    Some((Bytes::from(line), last))
}
