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

//! MCP revisions (SPEC §mcp.revisions): replace, restore, the snapshot cap, and revisions across
//! a rename and a delete.

use axum::http::StatusCode;
use serde_json::json;

use super::{setup, Harness, TOKEN};

// ---- SPEC §mcp.revisions: replace / restore / revisions ----------------------------------------------
// The operator's flow this serves: park the current def of a name, install a new one under
// the SAME name, roll back with one click. "One active def per name" is structural — the
// revision list is inert data and never reaches the registry or the boot path.

async fn add_echo(h: &Harness, name: &str, enabled: bool) {
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": name, "type": "echo", "enabled": enabled }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "add {name}: {body}");
}

#[tokio::test]
async fn replace_parks_the_old_def_and_serves_the_new_one() {
    let h = setup();
    add_echo(&h, "m", true).await;
    let (status, body) = h
        .post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp", "note": "swap one" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["parked"], json!(true));
    assert_eq!(body["type"], json!("http"));
    assert_eq!(body["revisions"], json!(1));
    // x.test does not resolve: the swap stands, the start failure rides in the body — the
    // rollback path must be reachable, not buried under a 500.
    assert!(body["restartError"].is_string(), "{body}");
    assert!(!h.has_server("m"));
    // The live def is the new one; exactly one row answers to the name (one active per name).
    assert_eq!(h.def_of("m")["type"], json!("http"));
    assert_eq!(h.names().await, vec!["m".to_string()]);
    // The parked snapshot is the OLD def, note and all.
    let (status, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["revisions"][0]["type"], json!("echo"));
    assert_eq!(list["revisions"][0]["note"], json!("swap one"));
    assert!(list["revisions"][0]["at"].as_i64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn a_failing_replace_changes_nothing() {
    let h = setup();
    add_echo(&h, "m", true).await;
    // A rest def with no tools fails build_def — before anything is written.
    let (status, body) = h
        .post("/api/mcps/m/replace", json!({ "type": "rest" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(h.def_of("m")["type"], json!("echo"));
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn replace_leaves_a_stopped_mcp_stopped() {
    let h = setup();
    add_echo(&h, "m", false).await;
    assert!(!h.has_server("m"), "enabled:false never started it");
    let (status, body) = h
        .post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["lifecycle"],
        json!("stopped"),
        "a def swap is not a start"
    );
    assert!(!h.has_server("m"));
}

#[tokio::test]
async fn restore_swaps_back_and_parks_the_live_def() {
    let h = setup();
    add_echo(&h, "m", true).await;
    h.post(
        "/api/mcps/m/replace",
        json!({ "type": "http", "url": "https://x.test/mcp", "note": "swap" }),
    )
    .await;
    let (status, body) = h.post("/api/mcps/m/revisions/0/restore", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restored"], json!(0));
    assert_eq!(
        h.def_of("m")["type"],
        json!("echo"),
        "the parked def is live again"
    );
    // The replaced def was parked in turn — rollback is itself reversible.
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["revisions"][0]["type"], json!("http"));
    // Restoring an index that is not there refuses loudly instead of guessing.
    let (status, body) = h.post("/api/mcps/m/revisions/5/restore", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn six_replaces_keep_only_the_last_five_snapshots() {
    let h = setup();
    add_echo(&h, "m", true).await;
    for i in 0..6 {
        let (status, _) = h
            .post(
                "/api/mcps/m/replace",
                json!({ "type": "http", "url": "https://x.test/mcp", "note": format!("gen {i}") }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    let notes: Vec<String> = list["revisions"]
        .as_array()
        .expect("revisions")
        .iter()
        .map(|r| r["note"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(notes.len(), 5, "the cap holds through the routes");
    assert_eq!(notes[0], "gen 1", "the oldest was evicted");
}

#[tokio::test]
async fn rename_carries_revisions_and_delete_clears_them() {
    let h = setup();
    add_echo(&h, "m", true).await;
    h.post(
        "/api/mcps/m/replace",
        json!({ "type": "http", "url": "https://x.test/mcp", "note": "rides" }),
    )
    .await;
    let (status, _) = h.post("/api/mcps/m/rename", json!({ "name": "m2" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = h.get("/api/mcps/m2/revisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["revisions"][0]["note"], json!("rides"));
    let (status, _) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the old name is gone");
    // Deleting the MCP drops its parked snapshots with it — a future same-named MCP starts
    // clean (same rule the OAuth credentials already follow above).
    let (status, _) = h.delete("/api/mcps/m2").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/api/mcps/m2/revisions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_revision_can_be_dropped_without_touching_the_rest() {
    let h = setup();
    add_echo(&h, "m", true).await;
    for note in ["a", "b"] {
        h.post(
            "/api/mcps/m/replace",
            json!({ "type": "http", "url": "https://x.test/mcp", "note": note }),
        )
        .await;
    }
    let (status, _) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps/m/revisions").await;
    assert_eq!(list["revisions"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["revisions"][0]["note"], json!("b"));
    let (status, _) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::OK);
    // The list is empty now; deleting again says so (404: nothing parked) instead of erroring.
    let (status, body) = h.delete("/api/mcps/m/revisions/0").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_disabled_mcp_refuses_clients_with_the_disabled_wording() {
    let h = setup();
    add_echo(&h, "m", false).await;
    // SPEC §mcp.revisions: disabled means the client sees nothing of it — every method answers the
    // same refusal, and the wording names the operator's verb and the way out.
    let (status, body) = h
        .mcp(
            "/mcp/m",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = body.to_string();
    assert!(
        text.contains("disabled"),
        "wording must say disabled: {text}"
    );
    assert!(text.contains("enable it from the panel"), "{text}");
}

#[tokio::test]
async fn a_mariadb_def_builds_and_carries_its_own_tag() {
    let h = setup();
    // SPEC §mcp.panel: MariaDB is its own type — the def, the tag, the seal — on the mysql engine.
    // Disabled so the test never depends on a server existing anywhere.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "mdb", "type": "mariadb", "host": "127.0.0.1", "port": 3306,
                    "user": "u", "password": "${MDB_PW}", "database": "d", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, list) = h.get("/api/mcps").await;
    let row = list["mcps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "mdb")
        .expect("row")
        .clone();
    assert_eq!(row["tag"], "mariadb");
}
