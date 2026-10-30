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

//! Sidebar groups: create, reorder, rename and delete them, and assign MCPs and tokens.

use axum::http::StatusCode;
use serde_json::json;

use super::{field_of, row_named, setup, Harness};
use swiss_host::managed::ManagedStore;

// --- sidebar groups --------------------------------------------------------------------------

/// Three MCPs, none of them assigned to a group.
pub(super) fn with_mcps() -> Harness {
    let h = setup();
    for name in ["context7", "deepwiki", "github"] {
        h.register(name, json!({ "type": "echo" }));
    }
    h
}

/// Every row's group, in the order the panel would draw them.
async fn groups_of(h: &Harness) -> Vec<String> {
    let (_, body) = h.get("/api/mcps").await;
    field_of(&body["mcps"], "group")
}

#[tokio::test]
async fn starts_with_the_default_group_and_every_mcp_in_it() {
    let h = with_mcps();
    let (status, body) = h.get("/api/mcps").await;
    assert_eq!(status, StatusCode::OK);
    // The list is never empty: `default` is materialized, ordinary, first.
    assert_eq!(body["groups"], json!(["default"]));
    assert_eq!(groups_of(&h).await, ["default", "default", "default"]);
}

#[tokio::test]
async fn creates_groups_and_reports_them_on_the_list() {
    let h = with_mcps();
    let (status, put) = h
        .put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["Docs", "Search"]));

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Docs", "Search"]));
}

/// The panel's "Move up" sends the whole list back with the moved group first (groups.js
/// moveGroupBy -> saveGroupNames). A reorder is order-only: members that rendered in the
/// demoted first group by DEFAULT - no explicit entry, the sink slot was their only home -
/// must not follow the slot to the new front. Regression shape: g1 held three, g2 held
/// none, one Move up on g2 and all three answered g2.
#[tokio::test]
async fn reordering_groups_never_rehomes_the_first_groups_default_members() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["g1", "g2"] }))
        .await;
    assert_eq!(groups_of(&h).await, ["g1", "g1", "g1"]);

    let (status, put) = h
        .put("/api/groups/mcps", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(
        list["groups"],
        json!(["g2", "g1"]),
        "the order itself changed"
    );
    assert_eq!(
        groups_of(&h).await,
        ["g1", "g1", "g1"],
        "but nobody re-homed"
    );
}

/// The same Move up shape through the tokens scope: this store owns its member set, so the
/// pinning needs no outside help - a token that rendered in the demoted first group by
/// default stays with the name.
#[tokio::test]
async fn reordering_token_groups_never_rehomes_the_first_groups_default_members() {
    let h = setup();
    let id = h
        .post("/api/tokens", json!({ "label": "reorder-probe" }))
        .await
        .1["id"]
        .as_str()
        .expect("the id")
        .to_string();
    h.put("/api/groups/tokens", json!({ "groups": ["g1", "g2"] }))
        .await;

    let (status, put) = h
        .put("/api/groups/tokens", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/tokens").await;
    assert_eq!(list["tokenGroups"][&id], json!("g1"), "nobody re-homed");
}

#[tokio::test]
async fn rejects_a_malformed_duplicate_empty_or_last_group_destroying_list() {
    let h = with_mcps();
    let (status, _) = h.put("/api/groups/mcps", json!({ "groups": "Docs" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for groups in [json!(["Docs", "docs"]), json!([" "]), json!([])] {
        let (status, _) = h.put("/api/groups/mcps", json!({ "groups": groups })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{groups}");
    }
    // `default` is an ordinary name now — a list of just it is fine.
    let (status, _) = h
        .put("/api/groups/mcps", json!({ "groups": ["default"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    // A rejected call must not have half-applied.
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["default"]));
}

#[tokio::test]
async fn assigns_a_config_sourced_mcp_to_a_group_and_back_to_default() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;

    let (status, put) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "Docs" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        put,
        json!({ "group": "Docs" }),
        "the family answers the canonical group"
    );

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("Docs"));
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("default"));

    let (status, _) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("default"));
}

#[tokio::test]
async fn survives_a_restart_because_the_assignment_is_on_disk_not_in_the_registry() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs"] }))
        .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "Docs" }),
    )
    .await;

    let reloaded = ManagedStore::open_at(h.path.clone());
    assert_eq!(reloaded.get_groups(), ["Docs"]);
    assert_eq!(reloaded.group_of("context7"), "Docs");
}

#[tokio::test]
async fn answers_404_for_an_unknown_mcp_and_400_for_an_unknown_group() {
    let h = with_mcps();
    assert_eq!(
        h.put("/api/groups/mcps/members/ghost", json!({ "group": null }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "Nope" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn renames_a_group_and_carries_its_members() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    for name in ["context7", "deepwiki"] {
        h.put(
            &format!("/api/groups/mcps/members/{name}"),
            json!({ "group": "Docs" }),
        )
        .await;
    }

    let (status, put) = h
        .post(
            "/api/groups/mcps/rename",
            json!({ "from": "Docs", "to": "Reference" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["groups"], json!(["Reference", "Search"]));
    assert_eq!(put["moved"], json!(2)); // context7 and deepwiki rode along explicitly

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Reference", "Search"])); // keeps its slot
    assert_eq!(row_named(&list, "context7")["group"], json!("Reference"));
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("Reference"));
}

#[tokio::test]
async fn refuses_a_rename_onto_an_existing_group_or_a_missing_one() {
    let h = with_mcps();
    h.put(
        "/api/groups/mcps",
        json!({ "groups": ["default", "Docs", "Search"] }),
    )
    .await;

    for (from, to, expected) in [
        ("Docs", "Search", StatusCode::BAD_REQUEST),
        ("Ghost", "X", StatusCode::NOT_FOUND),
    ] {
        let (status, _) = h
            .post("/api/groups/mcps/rename", json!({ "from": from, "to": to }))
            .await;
        assert_eq!(status, expected, "renaming {from} to {to}");
    }
}

#[tokio::test]
async fn renames_default_like_any_other_group_carrying_the_first_slot_with_it() {
    let h = with_mcps();
    h.put(
        "/api/groups/mcps",
        json!({ "groups": ["default", "Docs", "Search"] }),
    )
    .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "default" }),
    )
    .await;

    let (status, _) = h
        .post(
            "/api/groups/mcps/rename",
            json!({ "from": "default", "to": "主力" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["主力", "Docs", "Search"]));
    assert_eq!(row_named(&list, "context7")["group"], json!("主力"));
    // An unassigned MCP followed the renamed slot on its own.
    assert_eq!(row_named(&list, "deepwiki")["group"], json!("主力"));
}

#[tokio::test]
async fn deleting_a_group_by_omission_moves_its_mcps_to_the_first_remaining_group() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["Docs", "Search"] }))
        .await;
    h.put(
        "/api/groups/mcps/members/context7",
        json!({ "group": "Docs" }),
    )
    .await;
    h.put(
        "/api/groups/mcps/members/github",
        json!({ "group": "Search" }),
    )
    .await;

    h.put("/api/groups/mcps", json!({ "groups": ["Search"] }))
        .await;

    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["Search"]));
    assert_eq!(list["mcps"].as_array().map(Vec::len), Some(3)); // nothing was deleted
                                                                // Members of the dropped group land in the FIRST remaining group — Search, the only one.
    assert_eq!(row_named(&list, "context7")["group"], json!("Search"));
    assert_eq!(row_named(&list, "github")["group"], json!("Search"));
}

#[tokio::test]
async fn carries_an_mcps_group_through_a_rename_and_drops_it_on_delete() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({ "name": "a", "type": "echo", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    h.put("/api/groups/mcps", json!({ "groups": ["Docs"] }))
        .await;
    h.put("/api/groups/mcps/members/a", json!({ "group": "Docs" }))
        .await;

    h.post("/api/mcps/a/rename", json!({ "name": "b" })).await;
    assert_eq!(h.store.group_of("b"), "Docs");

    h.delete("/api/mcps/b").await;
    assert!(h.store.get_mcp_groups().is_empty());
}
