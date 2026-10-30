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

//! An MCP's life on the panel: add and start, probe state, the paged tool, resource and prompt
//! listings, details, forwarded traffic; and the gateway's memory gauge.

use axum::http::StatusCode;
use serde_json::json;

use super::{field_of, row_named, setup, TOKEN};

// --- lifecycle, listings and details ------------------------------------------------------------

#[tokio::test]
async fn adds_an_mcp_and_starts_it_while_the_list_stays_status_only() {
    let h = setup();
    let (status, body) = h
        .post("/api/mcps", json!({ "name": "echo1", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["lifecycle"], json!("started"));

    // The live gateway probes on boot; here the probe is triggered explicitly.
    h.registry.check_all().await;

    // The list is light: state only, never the tools and resources arrays — the panel polls it.
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    let row = row_named(&body, "echo1");
    assert_eq!(row["lifecycle"], json!("started"));
    assert!(row.get("tools").is_none(), "{row}");
    assert!(row.get("resources").is_none(), "{row}");
}

#[tokio::test]
async fn reports_a_started_mcp_as_up_once_its_probe_answers() {
    let h = setup();
    h.register_fixture("f1");
    h.registry.start("f1").await.expect("start");
    h.registry.check_all().await;

    let (_, body) = h.get("/api/mcps").await;
    assert_eq!(row_named(&body, "f1")["state"], json!("up"));
}

#[tokio::test]
async fn pages_tools_and_resources_on_demand_while_details_carries_logs() {
    let h = setup();
    h.register_fixture("f2");
    h.registry.start("f2").await.expect("start");

    let (status, tools) = h.get("/api/mcps/f2/tools").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&tools["tools"], "name").contains(&"echo".to_string()),
        "{tools}"
    );
    // Everything fits on one page, so there is nothing to continue with.
    assert!(tools.get("nextCursor").is_none(), "{tools}");

    let (status, resources) = h.get("/api/mcps/f2/resources").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&resources["resources"], "uri").contains(&"echo://f2".to_string()),
        "{resources}"
    );

    // Details carries the captured output and the config — never the listings.
    let (status, details) = h.get("/api/mcps/f2/details").await;
    assert_eq!(status, StatusCode::OK);
    assert!(details.get("tools").is_none(), "{details}");
    assert!(details["logs"].is_string(), "{details}");
}

#[tokio::test]
async fn answers_a_listing_for_an_mcp_that_was_never_started() {
    // Not a Node case: the Node fixture always started. The Rust panel polls a stopped MCP too,
    // and the answer must be the 503 it renders as "disabled", never a 500.
    let h = setup();
    h.register_fixture("f3");
    assert_eq!(
        h.get("/api/mcps/f3/tools").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        h.get("/api/mcps/ghost/tools").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.get("/api/mcps/f3/nonsense").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn forwards_mcp_traffic_to_the_added_server() {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "e2", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h
        .mcp(
            "/mcp/e2",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

// --- memory ----------------------------------------------------------------------------------

#[tokio::test]
async fn reports_gateway_memory_without_walking_a_process_tree_when_nothing_was_spawned() {
    let h = setup();
    let (status, body) = h.get("/api/memory").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["gatewayMb"].as_f64().unwrap_or(0.0) > 0.0, "{body}");
    assert_eq!(body["childrenMb"].as_f64(), Some(0.0));
    assert_eq!(body["childrenPending"], json!(false));
    assert_eq!(body["processCount"], json!(1));
}

// --- prompts ---------------------------------------------------------------------------------

#[tokio::test]
async fn lists_prompts_paged_even_for_a_server_that_publishes_none() {
    // The panel's Prompts tab polls every started MCP. A server without the capability must answer
    // an empty page, not an error the tab would render as a broken MCP.
    let h = setup();
    h.register_fixture("p1");
    h.registry.start("p1").await.expect("start");
    let (status, body) = h.get("/api/mcps/p1/prompts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["prompts"], json!([]));
}
