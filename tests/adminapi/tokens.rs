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

//! Named access tokens: list, create, revoke, rotate, and the secret route.

use axum::http::StatusCode;
use serde_json::json;

use super::{field_of, setup, TOKEN};
use swiss_host::managed::ManagedStore;

// --- tokens ---------------------------------------------------------------------------------

#[tokio::test]
async fn exposes_the_tokens_env_var_name_never_the_token() {
    let h = setup();
    let (status, body) = h.get("/api/info").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tokenEnv"], json!("SWISS_TOKEN"));
    assert!(!body.to_string().contains(TOKEN), "{body}");
    // The build stamp (SPEC §host.daemon) — present, and never a secret either.
    assert_eq!(body["build"]["hash"], json!(env!("SWISS_GIT_HASH")));
    assert!(!body["build"].to_string().contains(TOKEN), "{body}");
}

#[tokio::test]
async fn lists_creates_revokes_and_rotates_named_tokens() {
    let h = setup();
    // The migrated seed is present as the "default" token, and the list carries no secrets.
    let (status, body) = h.get("/api/tokens").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        field_of(&body["tokens"], "label").contains(&"default".to_string()),
        "{body}"
    );
    assert!(!body["tokens"].to_string().contains(TOKEN), "{body}");

    // Create a named token; its secret comes back once and authenticates an MCP request.
    let (status, made) = h
        .post("/api/tokens", json!({ "label": "claude-code" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(made["label"], json!("claude-code"));
    let secret = made["secret"].as_str().expect("a secret").to_string();
    assert!(
        secret.len() == 48 && secret.chars().all(|c| c.is_ascii_hexdigit()),
        "{secret}"
    );
    let id = made["id"].as_str().expect("an id").to_string();
    // 503, not 401: the token is good, the MCP name is not one this gateway serves.
    assert_eq!(
        h.mcp("/mcp/anything", &secret, json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // Rotate: a new secret under the same id; the old one stops working at the gate.
    let (status, rotated) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rotated["id"], json!(id));
    let next = rotated["secret"].as_str().expect("a secret").to_string();
    assert_ne!(next, secret);
    assert_eq!(
        h.mcp("/mcp/anything", &secret, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.mcp("/mcp/anything", &next, json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // Revoke: gone, no longer authenticates, 404 on a second attempt.
    assert_eq!(
        h.delete(&format!("/api/tokens/{id}")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        h.mcp("/mcp/anything", &next, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.delete(&format!("/api/tokens/{id}")).await.0,
        StatusCode::NOT_FOUND
    );

    // Persisted: a fresh read has the default token, the created one having been revoked.
    let labels: Vec<String> = ManagedStore::open_at(h.path.clone())
        .get_tokens()
        .iter()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(labels, ["default"]);
}

#[tokio::test]
async fn hands_a_stored_secret_back_so_a_connect_command_needs_no_rotate() {
    let h = setup();
    let (status, made) = h
        .post("/api/tokens", json!({ "label": "claude-code" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = made["id"].as_str().expect("an id").to_string();

    let (status, got) = h.get(&format!("/api/tokens/{id}/secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        got,
        json!({ "id": id, "label": "claude-code", "secret": made["secret"] })
    );
}

#[tokio::test]
async fn keeps_secrets_out_of_the_list_and_the_secret_route_open() {
    let h = setup();
    let (_, made) = h.post("/api/tokens", json!({ "label": "a" })).await;
    let id = made["id"].as_str().expect("an id").to_string();

    // The list is what the panel polls; reading a secret stays a separate, explicit request.
    let (_, list) = h.get("/api/tokens").await;
    for token in list["tokens"].as_array().expect("tokens array") {
        assert!(token.get("secret").is_none(), "{token}");
    }
    // There is no credential gate: the loopback guard is the boundary for the secret route too.
    assert_eq!(
        h.get(&format!("/api/tokens/{id}/secret")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn answers_404_for_an_unknown_token_id() {
    let h = setup();
    assert_eq!(
        h.get("/api/tokens/nope/secret").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn returns_the_new_secret_after_a_rotate_not_the_old_one() {
    let h = setup();
    let (_, made) = h.post("/api/tokens", json!({ "label": "a" })).await;
    let id = made["id"].as_str().expect("an id").to_string();
    let (_, rotated) = h.post(&format!("/api/tokens/{id}/rotate"), json!({})).await;

    let (_, got) = h.get(&format!("/api/tokens/{id}/secret")).await;
    assert_eq!(got["secret"], rotated["secret"]);
    assert_ne!(got["secret"], made["secret"]);
}
