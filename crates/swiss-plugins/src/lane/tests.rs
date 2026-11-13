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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::queue::{clip, default_env, Lane, LaneConfig, Seat, Ticket, CMD_MAX, RECENT_MAX};
use super::*;

fn ticket(cmd: &str) -> Ticket {
    Ticket { cmd: cmd.into(), cwd: "~/dev/swiss".into(), by: "claude".into(), session: None, pid: None }
}

fn lane(capacity: usize) -> Lane {
    Lane::new(LaneConfig { capacity, env: vec![("CARGO_BUILD_JOBS".into(), "2".into())] }, None)
}

fn ahead(seat: Seat) -> usize {
    match seat {
        Seat::Waiting { ahead, .. } => ahead,
        other => panic!("expected waiting, got {other:?}"),
    }
}

// --- config ---------------------------------------------------------------------------------

#[test]
fn an_empty_row_is_one_slot_with_the_default_caps() {
    let config = LaneConfig::parse(&json!({})).unwrap();
    assert_eq!(config.capacity, 1);
    assert_eq!(config.env, default_env());
    assert_eq!(LaneConfig::parse(&Value::Null).unwrap(), config);
    // The host's own key rides along in some rows and means nothing here.
    assert_eq!(LaneConfig::parse(&json!({ "enabled": true })).unwrap(), config);
}

#[test]
fn the_default_caps_hold_cargo_to_two_jobs_and_vitest_to_two_through_four_workers() {
    let env = default_env();
    assert!(env.contains(&("CARGO_BUILD_JOBS".into(), "2".into())), "{env:?}");
    let workers: usize = env
        .iter()
        .find(|(k, _)| k == "VITEST_MAX_WORKERS")
        .map(|(_, v)| v.parse().unwrap())
        .expect("a vitest cap");
    assert!((2..=4).contains(&workers), "{workers}");
}

#[test]
fn an_explicit_empty_env_means_no_caps() {
    let config = LaneConfig::parse(&json!({ "capacity": 2, "env": {} })).unwrap();
    assert_eq!(config, LaneConfig { capacity: 2, env: Vec::new() });
}

#[test]
fn a_bad_row_is_refused_with_the_key_named() {
    for (row, needle) in [
        (json!({ "capacity": 0 }), "capacity"),
        (json!({ "capacity": 17 }), "capacity"),
        (json!({ "capacity": "1" }), "capacity"),
        (json!({ "env": ["CARGO_BUILD_JOBS"] }), "env"),
        (json!({ "env": { "CARGO_BUILD_JOBS": 2 } }), "env.CARGO_BUILD_JOBS"),
        (json!({ "env": { "1BAD": "x" } }), "1BAD"),
        (json!({ "env": { "A-B": "x" } }), "A-B"),
        (json!({ "slots": 2 }), "slots"),
        (json!([1]), "object"),
    ] {
        let err = LaneConfig::parse(&row).unwrap_err();
        assert!(err.contains(needle), "{row} -> {err}");
    }
}

#[test]
fn the_schema_matches_the_parser() {
    let plugin = LanePlugin::new(
        RuntimeServices::new(swiss_host::services::BootInfo::default()),
        LaneSlot::new(),
    );
    let descriptor = plugin.descriptor();
    let props = &descriptor.config_schema["properties"];
    assert_eq!(props["capacity"]["default"], json!(LaneConfig::default().capacity));
    assert_eq!(props["capacity"]["maximum"], json!(queue::CAPACITY_MAX));
    let mut keys: Vec<&str> = props.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["capacity", "env"]);
    assert_eq!(descriptor.routes, vec!["/api/lane".to_string()]);
    assert!(!descriptor.restart_on_config_change);
}

// --- tickets --------------------------------------------------------------------------------

#[test]
fn a_ticket_needs_a_command_and_clips_long_text() {
    assert!(Ticket::from_body(&json!({})).unwrap_err().contains("cmd"));
    assert!(Ticket::from_body(&json!({ "cmd": "  " })).unwrap_err().contains("cmd"));
    assert!(Ticket::from_body(&json!({ "cmd": 3 })).unwrap_err().contains("cmd"));
    assert!(Ticket::from_body(&json!({ "cmd": "x", "pid": -1 })).unwrap_err().contains("pid"));
    assert!(Ticket::from_body(&json!({ "cmd": "x", "resume": "yes" })).unwrap_err().contains("resume"));

    let long = "cargo ".repeat(200);
    let (t, resume) = Ticket::from_body(&json!({ "cmd": long, "pid": 42, "resume": true })).unwrap();
    assert_eq!(t.cmd.chars().count(), CMD_MAX + 1, "clipped plus the ellipsis");
    assert!(t.cmd.ends_with('…'));
    assert_eq!(t.by, "cli", "an anonymous caller is the CLI");
    assert_eq!(t.pid, Some(42));
    assert!(resume);
}

#[test]
fn clip_cuts_on_a_character_boundary() {
    assert_eq!(clip("短命令", 2), "短命…");
    assert_eq!(clip("abc", 3), "abc");
}

// --- the queue ------------------------------------------------------------------------------

#[test]
fn one_slot_admits_in_arrival_order_and_passes_on_when_a_command_ends() {
    let lane = lane(1);
    let a = lane.enter(ticket("cargo build"), false);
    let b = lane.enter(ticket("npx vitest run"), false);
    let c = lane.enter(ticket("cargo test"), false);
    assert!(matches!(lane.seat(a), Seat::Running { .. }));
    assert_eq!(ahead(lane.seat(b)), 1);
    assert_eq!(ahead(lane.seat(c)), 2);

    assert!(lane.finish(a, Some(0)));
    assert!(matches!(lane.seat(b), Seat::Running { .. }));
    assert_eq!(ahead(lane.seat(c)), 1);
    assert_eq!(lane.seat(a), Seat::Unknown);
    assert!(!lane.finish(a, Some(0)), "a second report finds nothing");
}

#[test]
fn a_waiter_sees_what_runs_ahead_of_it() {
    let lane = lane(1);
    lane.enter(ticket("cargo build -j 2"), false);
    let b = lane.enter(ticket("npm run check"), false);
    let Seat::Waiting { running, .. } = lane.seat(b) else { panic!("waiting") };
    assert_eq!(running.len(), 1);
    assert_eq!(running[0]["cmd"], "cargo build -j 2");
    assert_eq!(running[0]["by"], "claude");
    assert!(running[0]["startedAt"].as_u64().is_some());
}

#[test]
fn a_waiter_that_leaves_drops_out_and_a_holder_that_leaves_is_recorded_gone() {
    let lane = lane(1);
    let a = lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), false);
    let c = lane.enter(ticket("c"), false);
    lane.leave(b);
    assert_eq!(ahead(lane.seat(c)), 1);
    lane.leave(a);
    assert!(matches!(lane.seat(c), Seat::Running { .. }));
    let snap = lane.snapshot();
    let recent = snap["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1, "a waiter that left never ran, so it is not history: {recent:?}");
    assert_eq!(recent[0]["cmd"], "a");
    assert_eq!(recent[0]["outcome"], "gone");
}

#[test]
fn a_release_frees_the_slot_and_tells_the_holder() {
    let lane = lane(1);
    let a = lane.enter(ticket("stuck"), false);
    let b = lane.enter(ticket("next"), false);
    assert!(lane.release(a));
    assert_eq!(lane.seat(a), Seat::Released);
    assert!(matches!(lane.seat(b), Seat::Running { .. }));
    assert!(!lane.release(a));
    // The holder's later end is not reported twice.
    assert!(!lane.finish(a, Some(0)));
    let snap = lane.snapshot();
    assert_eq!(snap["recent"][0]["outcome"], "released");
}

#[test]
fn a_resumed_command_runs_at_once_even_over_capacity() {
    let lane = lane(1);
    let a = lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), true);
    assert!(matches!(lane.seat(a), Seat::Running { .. }));
    assert!(matches!(lane.seat(b), Seat::Running { .. }));
    let c = lane.enter(ticket("c"), false);
    assert_eq!(ahead(lane.seat(c)), 2);
    lane.finish(a, Some(0));
    assert!(matches!(lane.seat(c), Seat::Waiting { .. }), "b still holds the only slot");
    lane.finish(b, Some(0));
    assert!(matches!(lane.seat(c), Seat::Running { .. }));
    assert_eq!(lane.snapshot()["recent"][0]["resumed"], true);
}

#[test]
fn more_capacity_admits_waiters_in_place_and_less_lets_holders_finish() {
    let lane = lane(1);
    let a = lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), false);
    let c = lane.enter(ticket("c"), false);
    lane.apply(LaneConfig { capacity: 2, env: Vec::new() });
    assert!(matches!(lane.seat(b), Seat::Running { ref env } if env.is_empty()));
    assert_eq!(ahead(lane.seat(c)), 2);
    lane.apply(LaneConfig { capacity: 1, env: Vec::new() });
    assert!(matches!(lane.seat(a), Seat::Running { .. }), "a holder is never taken back");
    lane.finish(a, Some(0));
    assert!(matches!(lane.seat(c), Seat::Waiting { .. }), "one slot, b holds it");
}

#[test]
fn closing_ends_every_place() {
    let lane = lane(1);
    let a = lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), false);
    lane.close();
    assert_eq!(lane.seat(a), Seat::Closed);
    assert_eq!(lane.seat(b), Seat::Closed);
    assert_eq!(lane.snapshot()["running"], json!([]));
}

#[test]
fn history_keeps_the_newest_and_the_exit_code() {
    let lane = lane(1);
    for n in 0..RECENT_MAX + 5 {
        let id = lane.enter(ticket(&format!("run {n}")), false);
        lane.finish(id, Some(n as i32));
    }
    let id = lane.enter(ticket("signalled"), false);
    lane.finish(id, None);
    let snap = lane.snapshot();
    let recent = snap["recent"].as_array().unwrap();
    assert_eq!(recent.len(), RECENT_MAX);
    assert_eq!(recent[0]["cmd"], "signalled");
    assert!(recent[0].get("exit").is_none(), "no code, no field");
    assert_eq!(recent[1]["exit"], json!(RECENT_MAX + 4));
    assert_eq!(recent[1]["outcome"], "exit");
}

#[test]
fn every_change_publishes_one_notice_and_moves_the_revision() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let lane = Lane::new(
        LaneConfig::default(),
        Some(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })),
    );
    let a = lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), false);
    lane.finish(a, Some(0));
    lane.leave(b);
    lane.leave(b);
    assert_eq!(count.load(Ordering::SeqCst), 4, "a no-op leave says nothing");
    assert_eq!(lane.snapshot()["revision"], 4);
}

#[test]
fn the_snapshot_masks_nothing_it_was_not_given() {
    let lane = lane(1);
    lane.enter(
        Ticket { session: Some("s-1".into()), pid: Some(7), ..ticket("cargo build") },
        false,
    );
    lane.enter(ticket("cargo test"), false);
    let snap = lane.snapshot();
    assert_eq!(snap["capacity"], 1);
    assert_eq!(snap["env"], json!({ "CARGO_BUILD_JOBS": "2" }));
    assert_eq!(snap["running"][0]["session"], "s-1");
    assert_eq!(snap["running"][0]["pid"], 7);
    assert_eq!(snap["running"][0]["cwd"], "~/dev/swiss");
    assert_eq!(snap["waiting"][0]["cmd"], "cargo test");
    assert!(snap["waiting"][0].get("startedAt").is_none());
}

// --- the routes -----------------------------------------------------------------------------

fn serving(capacity: usize) -> (Arc<LaneSlot>, axum::Router) {
    let slot = LaneSlot::new();
    slot.install(Arc::new(lane(capacity)));
    let app = api::mount(slot.clone());
    (slot, app)
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Open a lease; the stream yields its lines (blank heartbeats skipped).
async fn lease(app: &axum::Router, cmd: &str) -> impl futures_util::Stream<Item = Value> + Unpin {
    let request = Request::builder()
        .method("POST")
        .uri("/api/lane/enter")
        .body(Body::from(json!({ "cmd": cmd, "by": "codex" }).to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"].to_str().unwrap().starts_with("application/x-ndjson"));
    Box::pin(
        response
            .into_body()
            .into_data_stream()
            .filter_map(|chunk| async move {
                let chunk = chunk.ok()?;
                let text = String::from_utf8_lossy(&chunk).trim().to_string();
                (!text.is_empty()).then(|| serde_json::from_str(&text).unwrap())
            }),
    )
}

async fn next(stream: &mut (impl futures_util::Stream<Item = Value> + Unpin)) -> Value {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("a line within 5 s")
        .expect("the stream is open")
}

#[tokio::test]
async fn a_stopped_lane_answers_503() {
    let app = api::mount(LaneSlot::new());
    let (status, body) = call(&app, "GET", "/api/lane", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("lane"));
    let (status, _) = call(&app, "POST", "/api/lane/enter", Some(json!({ "cmd": "x" }))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_second_lease_waits_and_runs_when_the_first_reports_done() {
    let (_slot, app) = serving(1);
    let mut first = lease(&app, "cargo build").await;
    let line = next(&mut first).await;
    assert_eq!(line["state"], "running");
    assert_eq!(line["env"], json!({ "CARGO_BUILD_JOBS": "2" }));
    let first_id = line["id"].as_u64().unwrap();

    let mut second = lease(&app, "npx vitest run").await;
    let line = next(&mut second).await;
    assert_eq!(line["state"], "waiting");
    assert_eq!(line["ahead"], 1);
    assert_eq!(line["running"][0]["cmd"], "cargo build");

    let (_, snap) = call(&app, "GET", "/api/lane", None).await;
    assert_eq!(snap["running"][0]["by"], "codex");
    assert_eq!(snap["waiting"][0]["cmd"], "npx vitest run");

    let (status, _) = call(&app, "POST", &format!("/api/lane/{first_id}/done"), Some(json!({ "exit": 3 }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next(&mut second).await["state"], "running");
    assert!(
        tokio::time::timeout(Duration::from_secs(5), first.next()).await.unwrap().is_none(),
        "the finished lease ends"
    );
    let (_, snap) = call(&app, "GET", "/api/lane", None).await;
    assert_eq!(snap["recent"][0]["exit"], 3);
}

#[tokio::test]
async fn a_dropped_lease_gives_its_place_up() {
    let (slot, app) = serving(1);
    let mut first = lease(&app, "a").await;
    next(&mut first).await;
    let mut second = lease(&app, "b").await;
    assert_eq!(next(&mut second).await["state"], "waiting");
    drop(first);
    assert_eq!(next(&mut second).await["state"], "running");
    let snap = slot.current().unwrap().snapshot();
    assert_eq!(snap["recent"][0]["outcome"], "gone");
}

#[tokio::test]
async fn a_release_ends_the_lease_with_a_last_line() {
    let (_slot, app) = serving(1);
    let mut first = lease(&app, "stuck").await;
    let id = next(&mut first).await["id"].as_u64().unwrap();
    let (status, _) = call(&app, "POST", &format!("/api/lane/{id}/release"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next(&mut first).await["state"], "released");
    assert!(tokio::time::timeout(Duration::from_secs(5), first.next()).await.unwrap().is_none());
    let (status, body) = call(&app, "POST", &format!("/api/lane/{id}/release"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "lane.unknown");
}

#[tokio::test]
async fn a_closing_lane_tells_every_holder() {
    let (slot, app) = serving(1);
    let mut first = lease(&app, "a").await;
    next(&mut first).await;
    let mut second = lease(&app, "b").await;
    next(&mut second).await;
    slot.withdraw().unwrap().close();
    assert_eq!(next(&mut first).await["state"], "closed");
    assert_eq!(next(&mut second).await["state"], "closed");
}

#[tokio::test]
async fn bad_bodies_are_400s() {
    let (_slot, app) = serving(1);
    let (status, body) = call(&app, "POST", "/api/lane/enter", Some(json!({ "by": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("cmd"));
    let (status, _) = call(&app, "POST", "/api/lane/1/done", Some(json!({ "exit": "zero" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = call(&app, "POST", "/api/lane/99/done", Some(json!({ "exit": 0 }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "lane.unknown");
}

#[tokio::test(start_paused = true)]
async fn a_quiet_lease_writes_heartbeats() {
    let (_slot, app) = serving(1);
    let request = Request::builder()
        .method("POST")
        .uri("/api/lane/enter")
        .body(Body::from(json!({ "cmd": "long" }).to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let mut chunks = response.into_body().into_data_stream();
    let first = chunks.next().await.unwrap().unwrap();
    assert!(first.starts_with(b"{"), "the running line first");
    let beat = chunks.next().await.unwrap().unwrap();
    assert_eq!(&beat[..], b"\n");
}

#[tokio::test]
async fn the_gateway_stopping_closes_every_lease_before_the_plugin_stops() {
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    let slot = LaneSlot::new();
    let plugin = LanePlugin::new(services.clone(), slot.clone());
    let instance = plugin.create(&json!({})).await.unwrap();
    let mut scope = swiss_host::host::scope::PluginScope::new(PLUGIN_ID);
    instance.clone().start(&mut scope).await.unwrap();
    let lane = slot.current().expect("the started instance installs its lane");
    let id = lane.enter(ticket("cargo build"), false);
    services.bus.shutdown().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while lane.seat(id) != Seat::Closed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the lease is closed by the stop signal alone");
    instance.stop().await;
    assert!(slot.current().is_none());
    scope.shutdown().await;
}

#[tokio::test]
async fn a_new_row_applies_in_place() {
    let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
    let slot = LaneSlot::new();
    let plugin = LanePlugin::new(services, slot.clone());
    let instance = plugin.create(&json!({})).await.unwrap();
    let mut scope = swiss_host::host::scope::PluginScope::new(PLUGIN_ID);
    instance.clone().start(&mut scope).await.unwrap();
    let lane = slot.current().unwrap();
    lane.enter(ticket("a"), false);
    let b = lane.enter(ticket("b"), false);
    assert!(matches!(instance.apply_config(&json!({ "capacity": 2, "env": {} })).await, ApplyOutcome::Applied));
    assert!(matches!(lane.seat(b), Seat::Running { ref env } if env.is_empty()));
    assert!(matches!(instance.apply_config(&json!({ "capacity": 0 })).await, ApplyOutcome::Failed(_)));
    instance.stop().await;
    scope.shutdown().await;
}
