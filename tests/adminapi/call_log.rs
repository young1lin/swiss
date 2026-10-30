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

//! The call log: tool calls with their source, one reply in full, a tool's recent runs, and
//! reading a resource from the panel.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{setup, Harness, TOKEN};

// --- the call log ----------------------------------------------------------------------------

/// One started echo MCP under a name of the caller's choosing. The call log is filed by MCP name
/// and lives on disk, so each test picks its own name and never sees another test's rows.
async fn with_echo_named(name: &str) -> Harness {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": name, "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h
}

#[tokio::test]
async fn pages_the_call_log_and_serves_one_reply_in_full() {
    let h = with_echo_named("pg1").await;
    for i in 0..3 {
        let (status, _) = h
            .post(
                "/api/mcps/pg1/call",
                json!({ "tool": "echo", "arguments": { "msg": format!("m{i}") } }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, page) = h.get("/api/mcps/pg1/calls?page=0").await;
    assert_eq!(status, StatusCode::OK);
    let calls = page["calls"].as_array().expect("calls array").clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(page["page"], json!(0));
    assert_eq!(page["more"], json!(false));
    assert_eq!(calls[0]["output"], json!("m2")); // newest first

    let seq = calls[0]["seq"].as_u64().expect("a seq");
    let (status, one) = h.get(&format!("/api/mcps/pg1/calls/{seq}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["call"]["output"], json!("m2"));
    assert_eq!(
        h.get("/api/mcps/pg1/calls/9999").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn records_every_tool_call_with_its_source_and_clears_on_request() {
    let h = with_echo_named("cl").await;

    let (status, empty) = h.get("/api/mcps/cl/calls").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["calls"], json!([]));

    // Once through the panel, once as an MCP client on the HTTP endpoint.
    let (_, run) = h
        .post(
            "/api/mcps/cl/call",
            json!({ "tool": "echo", "arguments": { "msg": "from-panel" } }),
        )
        .await;
    assert_eq!(run["text"], json!("from-panel"));
    h.mcp(
        "/mcp/cl",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "from-client" } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/mcps/cl/calls").await;
    let calls = body["calls"].as_array().expect("calls array").clone();
    assert_eq!(calls.len(), 2);
    // Newest first: the client's call, then the panel's.
    assert_eq!(calls[0]["tool"], json!("echo"));
    assert_eq!(calls[0]["ok"], json!(true));
    assert_eq!(calls[0]["via"], json!("mcp"));
    assert!(
        calls[0]["args"]
            .as_str()
            .unwrap_or_default()
            .contains("from-client"),
        "{}",
        calls[0]
    );
    assert_eq!(calls[0]["output"], json!("from-client"));
    assert!(calls[0]["ms"].is_number(), "{}", calls[0]);
    assert_eq!(calls[1]["tool"], json!("echo"));
    assert_eq!(calls[1]["via"], json!("panel"));

    let (status, _) = h.delete("/api/mcps/cl/calls").await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = h.get("/api/mcps/cl/calls").await;
    assert_eq!(after["calls"], json!([]));
}

#[tokio::test]
async fn serves_one_tools_recent_runs_for_the_run_tab_dropdown() {
    // The Run tab's refill dropdown: one tool's newest runs, newest first, both sources included.
    // The full arguments of a picked entry come back from the per-seq route — the history itself
    // carries only a one-line preview, so 300 entries stay light.
    let h = with_echo_named("hist").await;
    h.post(
        "/api/mcps/hist/call",
        json!({ "tool": "echo", "arguments": { "msg": "from-panel" } }),
    )
    .await;
    h.mcp(
        "/mcp/hist",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "from-client" } },
        }),
    )
    .await;

    let (status, hist) = h.get("/api/mcps/hist/tool-history?tool=echo&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    let entries = hist["entries"].as_array().expect("entries").clone();
    assert_eq!(entries.len(), 1); // limit honoured: only the newest
    assert_eq!(entries[0]["via"], json!("mcp"));
    assert!(
        entries[0]["args"]
            .as_str()
            .unwrap_or_default()
            .contains("from-client"),
        "{}",
        entries[0]
    );

    // The dropdown's search box: q narrows the list to runs whose FULL arguments contain it.
    let (status, filtered) = h
        .get("/api/mcps/hist/tool-history?tool=echo&q=from-client")
        .await;
    assert_eq!(status, StatusCode::OK);
    // "from-panel" does not contain "from-client".
    assert_eq!(filtered["entries"].as_array().map(Vec::len), Some(1));
    let (status, missed) = h
        .get("/api/mcps/hist/tool-history?tool=echo&q=no-such-text")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(missed["entries"], json!([]));

    // A picked entry's complete arguments, via the route the dropdown fetches on selection.
    let seq = entries[0]["seq"].as_u64().expect("a seq");
    let (_, one) = h.get(&format!("/api/mcps/hist/calls/{seq}")).await;
    let args: Value = serde_json::from_str(one["call"]["args"].as_str().expect("args"))
        .expect("the stored arguments are JSON");
    assert_eq!(args, json!({ "msg": "from-client" }));

    // tool is required, and an unknown MCP is a miss rather than an empty history.
    assert_eq!(
        h.get("/api/mcps/hist/tool-history").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.get("/api/mcps/nope/tool-history?tool=x").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn reads_one_resource_from_the_panel_without_handing_it_the_token() {
    // The counterpart of the tool runner, for the other primitive: the browser never holds the
    // gateway token, so a resource preview goes through /api rather than the MCP endpoint.
    let h = setup();
    h.register_fixture("res");
    h.registry.start("res").await.expect("start");

    let (status, body) = h
        .post("/api/mcps/res/resource", json!({ "uri": "echo://res" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["text"].as_str().unwrap_or_default().contains("hello"),
        "{body}"
    );

    assert_eq!(
        h.post("/api/mcps/res/resource", json!({})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.post("/api/mcps/ghost/resource", json!({ "uri": "echo://res" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
