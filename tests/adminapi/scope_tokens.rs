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

//! The tokens scope over the group-scope family (SPEC §host.groups).

use axum::http::StatusCode;
use serde_json::json;

use super::setup;

// --- the tokens scope over the family (SPEC §host.groups) ----------------------------------------------

#[tokio::test]
async fn the_family_serves_the_tokens_scope() {
    let h = setup();
    let (status, body) = h.post("/api/tokens", json!({ "label": "g7-probe" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("the id").to_string();

    let (status, body) = h
        .put(
            "/api/groups/tokens",
            json!({ "groups": ["default", "Lab"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Lab"]));

    let (status, body) = h
        .put(
            &format!("/api/groups/tokens/members/{id}"),
            json!({ "group": "lab" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Lab"), "canonical casing comes back");

    // The listing answers the two lists beside the tokens (SPEC §host.groups): a group is a
    // folder a token sits in; it says nothing about whether the token is in use.
    let (status, body) = h.get("/api/tokens").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Lab"]));
    assert_eq!(body["tokenGroups"][&id], json!("Lab"));

    let (status, body) = h
        .post(
            "/api/groups/tokens/rename",
            json!({ "from": "Lab", "to": "Ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    assert_eq!(body["moved"], json!(1), "the one assigned member moved");

    // Creation time is the order (SPEC §host.groups): like tokens, no manual order exists
    // (secrets gained one - SPEC §host.vault).
    let (status, body) = h
        .put("/api/groups/tokens/order", json!({ "order": [id] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("tokens have no manual order"));

    let (status, _) = h
        .put("/api/groups/tokens/members/nope", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown member");
}

#[tokio::test]
async fn token_lifecycle_never_touches_the_groups() {
    let h = setup();
    let id = h.post("/api/tokens", json!({ "label": "g7-life" })).await.1["id"]
        .as_str()
        .expect("the id")
        .to_string();
    h.put(
        "/api/groups/tokens",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/tokens/members/{id}"),
        json!({ "group": "Ops" }),
    )
    .await;

    // A rotate keeps the id, so the assignment rides along untouched.
    let (status, body) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = h.get("/api/tokens").await.1;
    assert_eq!(
        body["tokenGroups"][&id],
        json!("Ops"),
        "a rotate keeps the group"
    );

    // Deleting the group sinks its tokens into the first one - the token itself is
    // untouched, still listed, still authenticating (SPEC §host.groups).
    let (status, body) = h
        .put("/api/groups/tokens", json!({ "groups": ["default"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = h.get("/api/tokens").await.1;
    assert_eq!(
        body["groups"],
        json!(["default"]),
        "the whole list, Ops omitted"
    );
    assert!(
        body["tokenGroups"].get(&id).is_none(),
        "the sunk token carries no explicit entry: {body}"
    );
    assert!(
        body["tokens"]
            .as_array()
            .map(|a| a.iter().any(|t| t["id"] == json!(id)))
            .unwrap_or(false),
        "the token still exists"
    );

    // A revoke forgets the member: no ghost entry outlives its token.
    h.put(
        "/api/groups/tokens",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/tokens/members/{id}"),
        json!({ "group": "Ops" }),
    )
    .await;
    let (status, _) = h.delete(&format!("/api/tokens/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    let body = h.get("/api/tokens").await.1;
    assert!(
        body["tokenGroups"].get(&id).is_none(),
        "no ghost member: {body}"
    );
}
