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

//! Importing other clients' MCP configs: stdio and HTTP entries, collisions, and this gateway's
//! own URLs.

use axum::http::StatusCode;
use serde_json::json;

use super::{field_of, setup};

// --- import ----------------------------------------------------------------------------------

#[tokio::test]
async fn imports_stdio_and_http_entries_suffixes_collisions_and_skips_this_gateways_urls() {
    let h = setup();
    h.register("redis", json!({ "type": "echo" }));

    let (status, body) = h
        .post(
            "/api/mcpdefs/import",
            json!({
                "mcpServers": {
                    "redis": { "type": "http", "url": "https://example.invalid/a" },
                    "docs": {
                        "type": "http", "url": "https://example.invalid/b",
                        "headers": { "Authorization": "Bearer ${K}" },
                    },
                    "already": { "url": "http://127.0.0.1:19999/mysql" },
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut imported = field_of(&body["imported"], "name");
    imported.sort();
    assert_eq!(imported, ["docs", "redis-1"]);
    assert!(
        field_of(&body["skipped"], "name").contains(&"already".to_string()),
        "{body}"
    );
    assert!(h.registry.has("redis-1"));
    assert!(h.registry.has("docs"));
}

#[tokio::test]
async fn imports_a_name_the_root_once_reserved_without_renaming() {
    // SPEC §mcp.endpoint: the RESERVED rename-on-import rule retired with the root-level route — an
    // imported "health" keeps its name under the MCP plugin's /mcp/ domain.
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcpdefs/import",
            json!({
                "mcpServers": {
                    "health": { "type": "http", "url": "https://example.invalid/h" },
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // "from" is the wanted name as it appeared in the file; it must equal the landed name.
    assert_eq!(field_of(&body["imported"], "from"), ["health"]);
    assert_eq!(field_of(&body["imported"], "name"), ["health"]);
    assert!(h.registry.has("health"));
}

#[tokio::test]
async fn imports_without_credentials_like_every_other_api_route() {
    let h = setup();
    let (status, _) = h
        .post("/api/mcpdefs/import", json!({ "mcpServers": {} }))
        .await;
    assert_eq!(status, StatusCode::OK);
}
