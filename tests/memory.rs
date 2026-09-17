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

//! The RSS guard — the one number this project exists for.
//!
//! docs/01 states the case for the port as memory: the Node build idles at ~86 MB and reaches
//! ~118 MB loaded, and the Rust build is supposed to sit near 16-20 MB. Nothing in the suite
//! guarded that, so a change that started buffering whole payloads, or leaking a session per
//! call, would have shipped silently and only shown up on a user's machine.
//!
//! What this file can and cannot assert:
//!
//! - It CANNOT assert an absolute ceiling. The measurement is this test binary's working set, and
//!   that includes the test harness, an rmcp client, reqwest and every dev-dependency — none of
//!   which are in the shipping `swiss`. An absolute number here would be measuring the wrong
//!   process. The shipped figure is measured from the binary itself (docs/01).
//! - It CAN assert that serving does not GROW the footprint. That is the actual regression shape:
//!   a leak, an unbounded buffer, or a payload-proportional allocation on a forwarding path. A
//!   delta needs no knowledge of the baseline, which is what makes it honest here.
//!
//! This lives in its own file on purpose: cargo gives each integration test file its own PROCESS,
//! so nothing else in the suite is allocating into the number being read. Everything below runs
//! in ONE test for the same reason — two `#[test]`s in one file run on parallel threads, and
//! would measure each other.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use swiss::app::{build_app, AppContext, BODY_LIMIT};
use swiss_core::platform::self_working_set;
use swiss_host::config::ServerDef;
use swiss_host::managed::ManagedStore;
use swiss_host::token::single_token_manager;
use swiss_mcp::adapters::make_adapter;
use swiss_mcp::registry::{Registry, Source};

const TOKEN: &str = "test-token-0123456789abcdef";

/// Requests per phase, and the per-request budget each phase is held to. The ceilings are
/// derived from these rather than written out, so raising or lowering the counts keeps the
/// assertions meaningful instead of quietly loosening them.
///
/// The counts are a deliberate compromise: high enough that a real leak clears the noise floor,
/// low enough that the whole file stays around half a minute in a debug build. A guard nobody
/// runs because it is slow is not a guard.
const REQUESTS: usize = 1_500;

/// A leak worth catching is a retained allocation per request. The smallest thing that leaks here
/// in practice — an MCP session with its spawned task — is far larger than this.
const KB_PER_REQUEST: f64 = 6.0;

/// Calls carrying a large payload. Fewer, because each one moves ~1 MB through the adapter both
/// ways — if the forwarding path copied per request without releasing, this alone would add
/// hundreds of MB.
const BIG_CALLS: usize = 60;

/// With one request in flight the footprint should track the LARGEST SINGLE payload, not the
/// total moved. The multiple is slack for the allocator, not for a payload-proportional design.
const PAYLOAD_MULTIPLE: f64 = 24.0;

/// Half the body limit: comfortably a "large" payload, with no risk of tripping the 413 this
/// test is not about.
const BIG_PAYLOAD: usize = BODY_LIMIT / 2;

fn mb(bytes: u64) -> f64 {
    (bytes as f64) / 1024.0 / 1024.0
}

/// Working-set growth since `before`, in MB. Saturating, because a working set can legitimately
/// SHRINK: the OS trims it under pressure, and the allocator returns pages. A negative delta is
/// not a failure, it is the best possible outcome.
fn growth_mb(before: u64) -> f64 {
    mb(self_working_set().saturating_sub(before))
}

async fn app_with_echo() -> axum::Router {
    let scratch =
        std::env::temp_dir().join(format!("swiss-memory-{}", swiss_core::util::random_hex(8)));
    // The call log is real, and the panel-call path writes to it. The app's OWN instance
    // points at scratch, so the test neither touches the data dir nor measures the absence
    // of logging.
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(scratch.join("calls")));
    let registry = Registry::new(3_600_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(scratch.join("managed.json")));
    let def = ServerDef(json!({ "type": "echo" }).as_object().cloned().unwrap());
    let adapter = make_adapter(&def, "echo", &calls).expect("echo adapter");
    registry
        .register("echo", Source::Config, def, adapter)
        .expect("register");
    registry.start("echo").await.expect("start");
    let tokens = Arc::new(single_token_manager(TOKEN));
    let ctx = AppContext::new(registry, tokens, store, calls, "MCP_GATEWAY_TOKEN", 19998);
    build_app(ctx, None)
}

async fn health(app: &axum::Router) {
    let req = Request::get("/health")
        .header(header::HOST, "127.0.0.1:19999")
        .body(Body::empty())
        .expect("a request");
    let res = app.clone().oneshot(req).await.expect("router answers");
    assert_eq!(res.status(), StatusCode::OK);
    // Drain: an undrained body would hold its buffer and make every phase below look like a leak
    // that is really just this test's own doing.
    let _ = axum::body::to_bytes(res.into_body(), BODY_LIMIT).await;
}

/// One panel tool call carrying `payload` bytes, through the registry, the adapter, an in-memory
/// MCP session and the call log — the deepest path a single request can take here.
async fn call(app: &axum::Router, payload: &str) {
    let body = json!({ "tool": "echo", "arguments": { "msg": payload } }).to_string();
    let req = Request::post("/api/mcps/echo/call")
        .header(header::HOST, "127.0.0.1:19999")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("a request");
    let res = app.clone().oneshot(req).await.expect("router answers");
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), BODY_LIMIT)
        .await
        .expect("body");
    // The echo really did carry the payload back: without this the test could pass by measuring
    // a path that quietly stopped doing any work.
    assert!(bytes.len() >= payload.len(), "the payload came back");
}

#[tokio::test(flavor = "current_thread")]
async fn serving_does_not_grow_the_footprint() {
    let app = app_with_echo().await;

    // Warm up first. The first requests through any path pay one-off costs — lazy statics, the
    // allocator reaching steady state, the call log's first file — and charging those to the
    // measurement would make the ceilings meaningless.
    for _ in 0..200 {
        health(&app).await;
    }
    call(&app, "warmup").await;

    // --- phase 1: many small requests ----------------------------------------------------------
    let before = self_working_set();
    println!("baseline after warmup: {:.1} MB", mb(before));
    for _ in 0..REQUESTS {
        health(&app).await;
    }
    let small = growth_mb(before);
    let ceiling = REQUESTS as f64 * KB_PER_REQUEST / 1024.0;
    println!("after {REQUESTS} health requests: +{small:.1} MB (ceiling {ceiling:.1})");
    assert!(
        small < ceiling,
        "{REQUESTS} trivial requests grew the working set by {small:.1} MB — \
         that is {:.2} KB per request retained, which is a leak, not noise",
        small * 1024.0 / REQUESTS as f64
    );

    // --- phase 2: the full call path, small payloads --------------------------------------------
    let before = self_working_set();
    for _ in 0..REQUESTS {
        call(&app, "hello").await;
    }
    let calls = growth_mb(before);
    // Twice the trivial-request budget: a call really does allocate more, it just must not
    // RETAIN more.
    let ceiling = REQUESTS as f64 * KB_PER_REQUEST * 2.0 / 1024.0;
    println!("after {REQUESTS} tool calls: +{calls:.1} MB (ceiling {ceiling:.1})");
    assert!(
        calls < ceiling,
        "{REQUESTS} tool calls grew the working set by {calls:.1} MB. Each call opens an \
         in-memory MCP session and spawns a server task; a session that outlives its client \
         leaks exactly like this"
    );

    // --- phase 3: large payloads ----------------------------------------------------------------
    // The architectural claim under test is AGENTS.md's "no serde_json::Value on a forwarding
    // path": payloads ride as `&RawValue` and are never re-parsed into an owned tree. If that
    // regressed, moving 200 MB through here would show up as a footprint proportional to the
    // payload rather than to the concurrency, which is 1.
    let payload = "x".repeat(BIG_PAYLOAD);
    let before = self_working_set();
    for _ in 0..BIG_CALLS {
        call(&app, &payload).await;
    }
    let big = growth_mb(before);
    let moved = mb((BIG_PAYLOAD * BIG_CALLS * 2) as u64);
    let ceiling = mb(BIG_PAYLOAD as u64) * PAYLOAD_MULTIPLE;
    println!(
        "after {BIG_CALLS} calls moving {moved:.0} MB total: +{big:.1} MB (ceiling {ceiling:.1})"
    );
    assert!(
        big < ceiling,
        "moving {moved:.0} MB through the gateway grew the working set by {big:.1} MB. \
         With one request in flight at a time the footprint should track the largest single \
         payload ({:.1} MB), not the total moved",
        mb(BIG_PAYLOAD as u64)
    );

    println!("final working set: {:.1} MB", mb(self_working_set()));
}
