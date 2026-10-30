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

//! The MCP list's order: name order until the user arranges it, then the stored order.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{rest_def, setup};
use swiss_host::managed::ManagedStore;

// --- the list ------------------------------------------------------------------------------

#[tokio::test]
async fn tags_each_mcp_row_with_how_it_is_launched() {
    let h = setup();
    let cases: Vec<(&str, Value, &str)> = vec![
        ("t-echo", json!({ "type": "echo" }), "echo"),
        (
            "t-http",
            json!({ "type": "http", "url": "https://example.invalid/mcp" }),
            "http",
        ),
        ("t-rest", rest_def("https://example.invalid"), "rest"),
        (
            "t-npx",
            json!({ "type": "proc", "command": "npx -y @some/server" }),
            "npx",
        ),
        (
            "t-uvx",
            json!({ "type": "proc", "command": "uvx mcp-server-x" }),
            "uvx",
        ),
        (
            "t-docker",
            json!({ "type": "proc", "command": "docker run -i mcp/x" }),
            "docker",
        ),
        (
            "t-cmd",
            json!({ "type": "proc", "command": "node server.js" }),
            "proc",
        ),
        (
            "t-mysql",
            json!({ "type": "mysql", "host": "127.0.0.1" }),
            "mysql",
        ),
    ];
    for (name, d, _) in &cases {
        h.register(name, d.clone());
    }
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["mcps"].as_array().expect("mcps array");
    for (name, _, tag) in &cases {
        let row = rows
            .iter()
            .find(|m| m["name"] == json!(name))
            .unwrap_or_else(|| panic!("no row for {name}"));
        assert_eq!(row["tag"], json!(tag), "tag of {name}");
    }
}

#[tokio::test]
async fn sorts_by_name_until_the_user_arranges_the_list() {
    let h = setup();
    // Registered in an order nobody would choose to look at, to prove registration order is not it.
    for n in ["zeta", "mid", "alpha"] {
        h.register(n, json!({ "type": "echo" }));
    }
    assert_eq!(h.names().await, ["alpha", "mid", "zeta"]);
}

#[tokio::test]
async fn appends_mcps_the_user_has_never_positioned_in_name_order() {
    let h = setup();
    for n in ["a", "zeta", "mid", "alpha"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put("/api/groups/mcps/order", json!({ "order": ["zeta", "a"] }))
        .await;
    // The arrangement wins for the two it names; the rest sort among themselves rather than
    // landing wherever the registry happened to start them.
    assert_eq!(h.names().await, ["zeta", "a", "alpha", "mid"]);
}

#[tokio::test]
async fn goes_back_to_name_order_when_the_arrangement_is_cleared() {
    let h = setup();
    for n in ["b", "c", "a"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put(
        "/api/groups/mcps/order",
        json!({ "order": ["c", "b", "a"] }),
    )
    .await;
    h.put("/api/groups/mcps/order", json!({ "order": [] }))
        .await;
    assert_eq!(h.names().await, ["a", "b", "c"]);
}

#[tokio::test]
async fn persists_a_panel_order_and_lists_by_it_appending_unknown_names() {
    let h = setup();
    for n in ["a", "b", "c"] {
        h.register(n, json!({ "type": "echo" }));
    }
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": ["c", "a"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.names().await, ["c", "a", "b"], "b is not in the order");
    // It survives a restart of the store, because it is on disk and not in the registry.
    assert_eq!(
        ManagedStore::open_at(h.path.clone()).get_order(),
        ["c", "a"]
    );
}

#[tokio::test]
async fn rejects_a_malformed_order_payload() {
    let h = setup();
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": "c,a" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn keeps_a_renamed_mcps_position_and_prunes_a_deleted_one_from_the_order() {
    let h = setup();
    for n in ["a", "b", "c"] {
        h.register(n, json!({ "type": "echo" }));
    }
    h.put(
        "/api/groups/mcps/order",
        json!({ "order": ["a", "b", "c"] }),
    )
    .await;
    let (status, _) = h.post("/api/mcps/a/rename", json!({ "name": "z" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.store.get_order(), ["z", "b", "c"]);
    // A deleted managed MCP leaves the order; config MCPs cannot be deleted, so prune via the
    // store the way the delete route does.
    h.store.remove("b").expect("remove b");
    assert_eq!(h.store.get_order(), ["z", "c"]);
}
