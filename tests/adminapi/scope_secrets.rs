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

//! The secrets scope over the group-scope family (SPEC §host.groups).

use axum::http::StatusCode;
use serde_json::json;

use super::setup;
use super::vault::VAULT_LOCK;

// --- the secrets scope over the family (SPEC §host.groups) ----------------------------------------------

#[tokio::test]
async fn the_family_serves_the_secrets_scope() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();

    // Two secrets to move around.
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-a",
        json!({ "value": "v-one", "rev": rev }),
    )
    .await;
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-b",
        json!({ "value": "v-two", "rev": rev }),
    )
    .await;

    // The whole list, then a member assign that keeps the canonical casing.
    let (status, body) = h
        .put(
            "/api/groups/secrets",
            json!({ "groups": ["default", "Ops"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/panel-g6-a",
            json!({ "group": "ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));

    // The listing answers the labels and NEVER a value: the write-only rule (SPEC §host.vault)
    // is about values, and a group label is a folder name.
    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Ops"]));
    assert_eq!(body["secretGroups"]["panel-g6-a"], json!("Ops"));
    assert_eq!(
        body["secretGroups"]["panel-g6-b"],
        json!("default"),
        "unassigned sinks"
    );
    let text = body.to_string();
    assert!(!text.contains("v-one"), "no value ever crosses: {text}");

    // A rename carries the members and answers how many moved. The vault is one shared
    // table across this binary's tests, so the honest expectation is the count the LISTING
    // reports under "default" right before the rename - unassigned secrets render there
    // (the sink), and the slot carries them with it.
    let before = h.get("/api/secrets").await.1;
    let in_default = before["secretGroups"]
        .as_object()
        .map(|m| m.values().filter(|g| g.as_str() == Some("default")).count())
        .unwrap_or(0);
    assert!(
        in_default >= 1,
        "panel-g6-b at least renders under default: {before}"
    );
    let (status, body) = h
        .post(
            "/api/groups/secrets/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["Basics", "Ops"]));
    assert_eq!(
        body["moved"],
        json!(in_default),
        "the whole first slot moved"
    );

    // SPEC §host.vault: the order is the model's third list. Unknown names drop out, so a stale
    // panel cannot plant a ghost row; the landed order comes back.
    let (status, body) = h
        .put(
            "/api/groups/secrets/order",
            json!({ "order": ["panel-g6-a", "ghost", "panel-g6-a"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["order"], json!(["panel-g6-a"]));

    // The listing carries the stored order raw (SPEC §host.vault).
    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["order"], json!(["panel-g6-a"]));

    // Unknown member: the family's one 404.
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/ghost",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_secrets_regroup_is_one_rev_bump_and_values_survive() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev0 = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-g6-rev",
        json!({ "value": "keep-me", "rev": rev0 }),
    )
    .await;
    h.put(
        "/api/groups/secrets",
        json!({ "groups": ["default", "Ops"] }),
    )
    .await;
    let before = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, body) = h
        .put(
            "/api/groups/secrets/members/panel-g6-rev",
            json!({ "group": "Ops" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    assert_eq!(
        after,
        before + 1,
        "a regroup is ONE rev bump, like a value write"
    );
    // The value the regroup rode along with is intact - the file still seals it.
    assert_eq!(
        swiss_core::secure::secretstore::vault_lookup("panel-g6-rev").as_deref(),
        Some("keep-me"),
        "the group move never touches values"
    );
}
