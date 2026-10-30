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

//! MCP traffic: attribution to tokens and clients, the raw view, the activity log's paging and
//! filters, and clearing one client's entries.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{field_of, setup, Harness, TOKEN};

// --- traffic ---------------------------------------------------------------------------------
// Each harness carries its own traffic ring (see Harness::traffic), so these tests run free in
// parallel: what a test's MCP endpoints record is visible only to that test's /api/traffic.

/// The bearer headers a hosted MCP client sends.
fn init_frame(name: &str, version: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-05",
            "capabilities": {},
            "clientInfo": { "name": name, "version": version },
        },
    })
}

/// The entry for one method, out of the traffic listing.
fn entry_for(body: &Value, method: &str) -> Value {
    body["entries"]
        .as_array()
        .and_then(|rows| rows.iter().find(|e| e["method"] == json!(method)))
        .cloned()
        .unwrap_or_else(|| panic!("no {method} entry in {body}"))
}

/// One echo MCP, started and reachable at `/mcp/echo`.
async fn with_echo() -> Harness {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "echo", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h
}

#[tokio::test]
async fn records_mcp_traffic_attributed_to_the_token_and_the_self_reported_client() {
    let h = with_echo().await;
    h.mcp("/mcp/echo", TOKEN, init_frame("claude-code", "1.2.3"))
        .await;

    let (status, body) = h.get("/api/traffic").await;
    assert_eq!(status, StatusCode::OK);
    let hit = entry_for(&body, "initialize");
    assert_eq!(hit["client"], json!("default")); // the token label that authenticated
    assert_eq!(hit["clientName"], json!("claude-code")); // self-reported during initialize
    assert_eq!(hit["clientVersion"], json!("1.2.3"));
}

#[tokio::test]
async fn attributes_a_server_discover_frame_via_its_meta_client_info() {
    let h = with_echo().await;
    // server/discover (Claude Code's capability probe) carries clientInfo nested in _meta, not at
    // the top level like initialize — the split must lift it from there too.
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "server/discover",
            "params": { "_meta": { "io.modelcontextprotocol/clientInfo": {
                "name": "claude-code", "version": "2.1.232",
            } } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    let hit = entry_for(&body, "server/discover");
    assert_eq!(hit["clientName"], json!("claude-code"));
    assert_eq!(hit["clientVersion"], json!("2.1.232"));
}

#[tokio::test]
async fn attributes_a_tokens_later_frames_to_the_name_it_announced_at_initialize() {
    let h = with_echo().await;
    // initialize announces the client name once; the tools/list after it carries no clientInfo,
    // but it is the same token — so it inherits "claude-code" rather than appearing as a second,
    // unknown client.
    h.mcp("/mcp/echo", TOKEN, init_frame("claude-code", "2.1.88"))
        .await;
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    assert_eq!(
        entry_for(&body, "tools/list")["clientName"],
        json!("claude-code")
    );
}

#[tokio::test]
async fn stores_the_full_redacted_request_body_for_the_expandable_raw_view() {
    let h = with_echo().await;
    h.mcp(
        "/mcp/echo",
        TOKEN,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "msg": "hi", "password": "shh" } },
        }),
    )
    .await;

    let (_, body) = h.get("/api/traffic").await;
    let hit = entry_for(&body, "tools/call");
    // The list row carries no payload — only the flag the collapsed row needs. The body arrives
    // from the per-entry endpoint when the row is expanded.
    assert!(hit.get("body").is_none(), "{hit}");
    assert!(hit.get("response").is_none(), "{hit}");
    assert_eq!(hit["hasResponse"], json!(true));

    let seq = hit["seq"].as_u64().expect("a seq");
    let (status, one) = h.get(&format!("/api/traffic/{seq}")).await;
    assert_eq!(status, StatusCode::OK);
    let raw = one["body"].as_str().expect("a stored body");
    assert!(raw.contains(r#""method":"tools/call""#), "{raw}"); // the whole envelope, not just params
    assert!(raw.contains(r#""msg":"hi""#), "{raw}");
    assert!(!raw.contains("shh"), "{raw}"); // a secret value is never stored
    assert!(raw.contains(r#""password":"•••""#), "{raw}"); // key kept, value redacted
    let reply = one["response"].as_str().expect("a stored reply");
    assert!(reply.contains("result"), "{reply}"); // the reply is captured too, not only the request
    assert!(reply.contains("hi"), "{reply}"); // the echoed text
}

#[tokio::test]
async fn pages_the_activity_log_newest_first_and_filters_to_actions_server_side() {
    let h = setup();
    // 7 protocol frames + 3 actions, interleaved so a naive slice cannot pass by accident.
    for i in 0..10 {
        let action = i % 3 == 2;
        h.traffic.record_traffic(
            "m",
            Some(&json!({
                "jsonrpc": "2.0", "id": i,
                "method": if action { "tools/call" } else { "tools/list" },
            })),
            Some("pg"),
            true,
            1,
            None,
        );
    }

    let (_, p0) = h.get("/api/traffic?pageSize=4").await;
    assert_eq!(p0["entries"].as_array().map(Vec::len), Some(4));
    assert_eq!(p0["total"], json!(10));
    assert_eq!(p0["more"], json!(true));
    // Newest first, and page 1 continues where page 0 stopped rather than repeating it.
    let seqs0: Vec<u64> = p0["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["seq"].as_u64().unwrap_or_default())
        .collect();
    let mut descending = seqs0.clone();
    descending.sort_by(|a, b| b.cmp(a));
    assert_eq!(seqs0, descending);
    let (_, p1) = h.get("/api/traffic?pageSize=4&page=1").await;
    let seqs1: Vec<u64> = p1["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["seq"].as_u64().unwrap_or_default())
        .collect();
    assert_eq!(seqs1, seqs0.iter().map(|s| s - 4).collect::<Vec<_>>());

    // The last page reports no more, and a page past the end is empty rather than an error.
    let (_, p2) = h.get("/api/traffic?pageSize=4&page=2").await;
    assert_eq!(p2["entries"].as_array().map(Vec::len), Some(2));
    assert_eq!(p2["more"], json!(false));
    let (status, p9) = h.get("/api/traffic?pageSize=4&page=9").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p9["entries"], json!([]));

    // actions=1 filters BEFORE paging: the count is of matching rows, not of rows on this page.
    let (_, act) = h.get("/api/traffic?actions=1").await;
    assert!(
        act["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|e| e["method"] == json!("tools/call")),
        "{act}"
    );
    assert_eq!(act["total"], json!(3));
    assert_eq!(act["totalUnfiltered"], json!(10)); // the "3 of 10 interactions" readout
}

#[tokio::test]
async fn folds_clients_over_the_whole_ring_not_over_the_returned_page() {
    let h = setup();
    let record = |mcp: &str, body: Value, token: &str| {
        h.traffic.record_traffic(mcp, Some(&body), Some(token), true, 1, None);
    };
    record(
        "m1",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "clientInfo": { "name": "app-a", "version": "1" } } }),
        "ta",
    );
    record(
        "m2",
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        "ta",
    );
    for i in 0..8 {
        record(
            "m3",
            json!({ "jsonrpc": "2.0", "id": 10 + i, "method": "tools/list" }),
            "tb",
        );
    }

    // Page 0 at this size holds only tb's rows; ta must still appear in the summary.
    let (_, body) = h.get("/api/traffic?pageSize=3").await;
    assert!(
        body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|e| e["client"] == json!("tb")),
        "{body}"
    );
    let keys = field_of(&body["clients"], "key");
    assert!(keys.contains(&"n:app-a".to_string()), "{body}"); // named by its initialize
    assert!(keys.contains(&"t:tb".to_string()), "{body}");
    let a = body["clients"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["key"] == json!("n:app-a")))
        .cloned()
        .expect("a row for app-a");
    assert_eq!(a["label"], json!("app-a 1"));
    assert_eq!(a["count"], json!(2)); // both of ta's rows, though neither is on this page
    assert_eq!(a["mcps"], json!(["m1", "m2"]));
    assert_eq!(a["tokens"], json!(["ta"]));
}

#[tokio::test]
async fn answers_404_for_a_traffic_entry_that_has_rolled_out_of_the_ring() {
    let h = setup();
    assert_eq!(h.get("/api/traffic/999999").await.0, StatusCode::NOT_FOUND);
    // A seq that is not a number is a bad request, not a miss.
    assert_eq!(h.get("/api/traffic/nope").await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn clears_one_clients_traffic_leaving_the_others_intact() {
    let h = setup();
    for token in ["clrA", "clrB"] {
        h.traffic.record_traffic(
            "m",
            Some(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })),
            Some(token),
            true,
            1,
            None,
        );
    }
    let (_, before) = h.get("/api/traffic").await;
    assert_eq!(before["entries"].as_array().map(Vec::len), Some(2));

    // The panel sends the prefixed key it was given in the clients summary.
    let (status, deleted) = h.delete("/api/traffic?client=t:clrA").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["client"], json!("t:clrA"));
    let (_, after) = h.get("/api/traffic").await;
    assert_eq!(field_of(&after["entries"], "client"), ["clrB"]);

    // A bare DELETE (no client) still clears everything.
    assert_eq!(h.delete("/api/traffic").await.0, StatusCode::OK);
    let (_, none) = h.get("/api/traffic").await;
    assert_eq!(none["entries"], json!([]));
}
