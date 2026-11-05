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

//! A per-MCP proxy on HTTP and REST MCPs, and the connection fields a Mongo definition keeps.

use axum::http::StatusCode;
use serde_json::json;

use super::{rest_def, setup};

// --- proxies -------------------------------------------------------------------------------

#[tokio::test]
async fn stores_and_returns_a_per_mcp_proxy_on_http_and_rest_mcps() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "prox-http", "type": "http",
                "url": "https://example.invalid/mcp",
                "proxy": "http://127.0.0.1:7890",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, det) = h.get("/api/mcps/prox-http/details").await;
    assert_eq!(det["config"]["proxy"], json!("http://127.0.0.1:7890"));

    let mut rest = rest_def("https://example.invalid");
    rest["name"] = json!("prox-rest");
    rest["proxy"] = json!("${MY_PROXY}");
    let (status, _) = h.post("/api/mcps", rest).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, det) = h.get("/api/mcps/prox-rest/details").await;
    // An env ref stays a ref: the panel never sees the resolved value, and neither does the file.
    assert_eq!(det["config"]["proxy"], json!("${MY_PROXY}"));
}

#[tokio::test]
async fn a_mongo_def_keeps_its_connection_fields_and_needs_a_url() {
    // SPEC §mcp.db: the URL is the whole address, so a mongo def without one is refused rather
    // than left to point at whatever a default would guess.
    let h = setup();
    let (status, body) = h.post("/api/mcps", json!({ "name": "m0", "type": "mongo" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("url is required for a mongo MCP"));

    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "m1", "type": "mongo",
                "url": "mongodb://app:${MONGO_PW}@127.0.0.1:27017/?authSource=admin",
                "database": "shop", "maxRows": 50, "allowDestructive": "true",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, det) = h.get("/api/mcps/m1/details").await;
    let config = &det["config"];
    assert_eq!(config["type"], json!("mongo"));
    assert_eq!(config["database"], json!("shop"));
    assert_eq!(config["maxRows"], json!(50));
    // A form's "true" lands as the boolean.
    assert_eq!(config["allowDestructive"], json!(true));

    // SPEC §mcp.defs: a field the type does not read is refused, not dropped.
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "m2", "type": "mongo", "url": "mongodb://127.0.0.1:27017/",
                "allowEval": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or_default().contains("'allowEval'"), "{body}");
}

#[tokio::test]
async fn rejects_a_proxy_that_is_not_an_http_url() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({
                "name": "bad-proxy", "type": "http",
                "url": "https://example.invalid/mcp",
                "proxy": "127.0.0.1:7890",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("proxy"),
        "{body}"
    );

    let mut rest = rest_def("https://example.invalid");
    rest["name"] = json!("bad-proxy-rest");
    rest["proxy"] = json!("socks5://x");
    let (status, body) = h.post("/api/mcps", rest).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("proxy"),
        "{body}"
    );
}
