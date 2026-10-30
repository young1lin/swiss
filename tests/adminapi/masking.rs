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

//! Secret masking: details carry the mask, a returned mask keeps the stored value, and the mask is
//! never stored.

use axum::http::StatusCode;
use serde_json::json;

use super::editing::MASK;
use super::setup;

// --- secret masking --------------------------------------------------------------------------

#[tokio::test]
async fn masks_secrets_in_details_and_keeps_the_stored_value_when_the_mask_returns() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "db", "type": "mysql", "host": "127.0.0.1",
                "user": "app", "password": "hunter2", "database": "d", "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, details) = h.get("/api/mcps/db/details").await;
    assert_eq!(details["config"]["password"], json!(MASK));
    assert_eq!(details["config"]["user"], json!("app")); // non-secret fields are untouched

    // Saving the form unchanged must not overwrite the password with the mask.
    let (status, _) = h
        .put(
            "/api/mcps/db",
            json!({
                "type": "mysql", "host": "127.0.0.1", "user": "app",
                "password": MASK, "database": "d",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.def_of("db")["password"], json!("hunter2"));

    // A real edit still goes through.
    h.put(
        "/api/mcps/db",
        json!({
            "type": "mysql", "host": "127.0.0.1", "user": "app",
            "password": "newpass", "database": "d",
        }),
    )
    .await;
    assert_eq!(h.def_of("db")["password"], json!("newpass"));
}

#[tokio::test]
async fn never_stores_the_mask_sentinel_when_the_type_changes_under_an_edit() {
    let h = setup();
    h.post(
        "/api/mcps",
        json!({
            "name": "db", "type": "mysql", "host": "127.0.0.1",
            "user": "app", "password": "hunter2", "database": "d", "enabled": false,
        }),
    )
    .await;
    // The panel's type dropdown can submit a pg url against a def that has no url to restore
    // from. Writing the sentinel through would replace a credential with dots and report success.
    let (status, _) = h
        .put(
            "/api/mcps/db",
            json!({ "type": "pg", "url": format!("postgresql://u:{MASK}@127.0.0.1:5432/d") }),
        )
        .await;
    assert!(
        !h.def_of("db").to_string().contains('•'),
        "{}",
        h.def_of("db")
    );
    // No usable url survived, so the edit is refused out loud.
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn masks_only_the_password_inside_a_connection_url_and_restores_it_on_save() {
    let h = setup();
    h.post(
        "/api/mcps",
        json!({
            "name": "pgx", "type": "pg",
            "url": "postgresql://chatuser:s3cret@127.0.0.1:5432/chat", "enabled": false,
        }),
    )
    .await;
    let (_, details) = h.get("/api/mcps/pgx/details").await;
    assert_eq!(
        details["config"]["url"],
        json!(format!("postgresql://chatuser:{MASK}@127.0.0.1:5432/chat"))
    );

    h.put(
        "/api/mcps/pgx",
        json!({ "type": "pg", "url": format!("postgresql://chatuser:{MASK}@127.0.0.1:5432/other") }),
    )
    .await;
    // The host and database of the edit are kept; only the password comes back from the store.
    assert_eq!(
        h.def_of("pgx")["url"],
        json!("postgresql://chatuser:s3cret@127.0.0.1:5432/other")
    );
}
