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

//! Editing, lifecycle and validation: edits and renames, deletes from the store and from
//! gateway.config.json, stop and start, names, remote and REST definitions.

use axum::http::StatusCode;
use serde_json::json;

use super::{setup, TOKEN};
use swiss_host::managed::ManagedStore;

// --- editing, lifecycle and validation -------------------------------------------------------

/// The mask the panel receives in place of a credential, and sends back unchanged when the
/// operator saves a form without touching it.
pub(super) const MASK: &str = "••••••••";

#[tokio::test]
async fn edits_a_managed_mcp_and_restarts_with_the_new_config() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "ed", "type": "mysql", "host": "127.0.0.1",
                "user": "app", "database": "v1",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, put) = h
        .put(
            "/api/mcps/ed",
            json!({ "type": "mysql", "host": "127.0.0.1", "user": "app", "database": "v2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // The edit rebuilds the adapter and leaves it running, rather than dropping the MCP.
    assert_eq!(put["lifecycle"], json!("started"));
    let (_, details) = h.get("/api/mcps/ed/details").await;
    assert_eq!(details["config"]["database"], json!("v2"));
    // The edit is on disk, not only in the registry.
    let stored = ManagedStore::open_at(h.path.clone());
    let entry = stored
        .all()
        .into_iter()
        .find(|m| m.name == "ed")
        .expect("a stored entry");
    assert_eq!(entry.def.get("database"), Some(&json!("v2")));
}

#[tokio::test]
async fn edits_a_config_file_mcp_and_persists_an_override() {
    let h = setup();
    h.register("cfg", json!({ "type": "echo" }));
    h.registry.start("cfg").await.expect("start");

    let (status, put) = h.put("/api/mcps/cfg", json!({ "type": "echo" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put["lifecycle"], json!("started"));
    assert!(
        h.store.has("cfg"),
        "the override is persisted to managed.json"
    );
}

#[tokio::test]
async fn stop_frees_the_server_and_start_brings_it_back() {
    let h = setup();
    h.register_fixture("e4");
    h.registry.start("e4").await.expect("start");
    assert!(h.has_server("e4"));

    let (status, _) = h.post("/api/mcps/e4/stop", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.has_server("e4"));

    let (status, _) = h.post("/api/mcps/e4/start", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(h.has_server("e4"));

    // A restart is the pair of the two, and lands started.
    let (status, body) = h.post("/api/mcps/e4/restart", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], json!("started"));
    assert!(h.has_server("e4"));
}

#[tokio::test]
async fn renames_a_managed_mcp() {
    let h = setup();
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "e5", "type": "echo" }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h
        .post("/api/mcps/e5/rename", json!({ "name": "e5b" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("e5"));
    assert!(h.registry.has("e5b"));
    assert!(!h.store.has("e5") && h.store.has("e5b"));
}

#[tokio::test]
async fn deletes_a_managed_mcp_and_updates_the_store() {
    let h = setup();
    h.post("/api/mcps", json!({ "name": "e6", "type": "echo" }))
        .await;
    assert!(h.store.has("e6"));
    let (status, _) = h.delete("/api/mcps/e6").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("e6"));
    assert!(!h.store.has("e6"));
}

#[tokio::test]
async fn deletes_a_config_mcp_even_when_no_config_file_holds_it() {
    // The old contract (refuse: deleting a config MCP left its file entry behind, so it
    // resurrected) became a real delete: the endpoint removes the gateway.config.json entry too.
    // With NO file present — this test's data dir has none — the runtime half still deletes.
    let h = setup();
    h.register("cfg", json!({ "type": "echo" }));
    let (status, _) = h.delete("/api/mcps/cfg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.registry.has("cfg"));
}

#[tokio::test]
async fn an_mcp_named_test_or_import_stays_deletable_and_editable() {
    // The collection-level actions ("test this def", "import defs") used to sit at
    // /api/mcps/test and /api/mcps/import — static segments at the {name} position of the
    // router. axum prefers a static match over the parameter, so DELETE /api/mcps/test landed
    // on the POST-only test route and fell to the terminal 404 ("no route for DELETE
    // /api/mcps/test"): an MCP named "test" (or "import") could be created but never edited
    // or deleted. Those actions now live under /api/mcpdefs/*, a namespace no MCP name can
    // shadow; this test pins the per-name verbs for both poisoned words.
    //
    // echo, not the http def the sighting came with: the shadow is about the NAME, and an
    // http add fires a real outbound handshake whose traffic row would leak into the
    // process-global ring other tests count.
    let h = setup();
    for name in ["test", "import"] {
        let (status, body) = h
            .post("/api/mcps", json!({ "name": name, "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create {name}: {body}");

        // PUT (edit) rides the same shadowed path as DELETE; both must reach the handlers.
        let (status, body) = h
            .put(&format!("/api/mcps/{name}"), json!({ "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::OK, "edit {name}: {body}");

        let (status, body) = h.delete(&format!("/api/mcps/{name}")).await;
        assert_eq!(status, StatusCode::OK, "delete {name}: {body}");
        assert_eq!(body["deleted"], json!(true));
        assert!(!h.registry.has(name), "the registry drops {name}");
        assert!(!h.store.has(name), "the managed store drops {name}");
    }
}

#[tokio::test]
async fn persists_added_mcps_to_managed_json() {
    let h = setup();
    h.post("/api/mcps", json!({ "name": "persist1", "type": "echo" }))
        .await;
    let reloaded = ManagedStore::open_at(h.path.clone());
    assert!(reloaded.all().iter().any(|m| m.name == "persist1"));
}

#[tokio::test]
async fn plugin_domain_names_need_no_host_reservation() {
    // SPEC §mcp.endpoint: inside /mcp/* the host's root reserved words are gone — health/api/admin/
    // mcp are ordinary MCP names there, creatable and reachable, while the host's own
    // routes at the root keep answering untouched.
    let h = setup();
    for name in ["health", "api", "admin", "mcp"] {
        let (status, body) = h
            .post("/api/mcps", json!({ "name": name, "type": "echo" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{name}: {body}");
    }
    // Reachable: /mcp/health is the MCP, /health is the host liveness route.
    let (status, _) = h
        .mcp(
            "/mcp/health",
            TOKEN,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/health").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rejects_an_invalid_name_and_a_duplicate() {
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({ "name": "bad name!", "command": "node x" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.post("/api/mcps", json!({ "name": "dup", "type": "echo" }))
        .await;
    let (status, _) = h
        .post("/api/mcps", json!({ "name": "dup", "command": "node x" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn keeps_cwd_and_the_expose_flags_across_an_edit() {
    // Regression: the panel had no cwd field, so build_def never received one and editing a proc
    // MCP silently dropped it (same for exposeResources/exposePrompts).
    let h = setup();
    let here = std::env::temp_dir().to_string_lossy().into_owned();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "keep", "command": "node server.js", "cwd": here,
                "exposeResources": false, "exposePrompts": false, "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.def_of("keep")["cwd"], json!(here));
    assert_eq!(h.def_of("keep")["exposeResources"], json!(false));

    let (status, _) = h
        .put(
            "/api/mcps/keep",
            json!({
                "type": "proc", "command": "node server.js", "cwd": here,
                "exposeResources": false, "exposePrompts": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.def_of("keep")["cwd"], json!(here));
    assert_eq!(h.def_of("keep")["exposeResources"], json!(false));
    assert_eq!(h.def_of("keep")["exposePrompts"], json!(false));
}

#[tokio::test]
async fn adds_a_remote_http_mcp_and_never_hands_its_key_to_the_browser() {
    // A remote MCP is configured, not launched: url + the headers its key travels in. Registered
    // disabled so the test never reaches for a network.
    let h = setup();
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "remote", "type": "http",
                "url": "https://mcp.context7.com/mcp",
                "headers": { "Authorization": "Bearer sk-live-1" },
                "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        h.def_of("remote")["url"],
        json!("https://mcp.context7.com/mcp")
    );
    assert_eq!(
        h.def_of("remote")["headers"]["Authorization"],
        json!("Bearer sk-live-1")
    );

    // The key travels in a header, so /details has to mask it there — the panel edits this
    // definition and must never receive the credential.
    let (_, details) = h.get("/api/mcps/remote/details").await;
    assert_eq!(details["config"]["headers"]["Authorization"], json!(MASK));
    assert_eq!(
        details["config"]["url"],
        json!("https://mcp.context7.com/mcp")
    );
}

#[tokio::test]
async fn adds_a_declared_rest_mcp_and_keeps_its_tool_declarations() {
    // A rest MCP's `tools` is an array of declarations, so build_def has to keep it whole —
    // without that branch, opening a config-defined rest MCP in the panel and pressing save
    // answers "unknown type".
    let h = setup();
    let tools = json!([{
        "name": "get_repo",
        "description": "Get a public GitHub repo.",
        "input": { "owner": { "type": "string", "required": true },
                   "repo": { "type": "string", "required": true } },
        "request": { "method": "GET", "path": "/repos/{{owner}}/{{repo}}" },
        "pick": ["full_name", "description", "stargazers_count"],
    }]);
    let (status, _) = h
        .post(
            "/api/mcps",
            json!({
                "name": "github-rest", "type": "rest",
                "baseUrl": "https://api.github.com",
                "headers": { "Authorization": "Bearer sk-live-2" },
                "tools": tools, "enabled": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.def_of("github-rest")["tools"], tools);
    assert_eq!(
        h.def_of("github-rest")["baseUrl"],
        json!("https://api.github.com")
    );

    // Same rule as every other adapter: the key does not go to the browser.
    let (_, details) = h.get("/api/mcps/github-rest/details").await;
    assert_eq!(details["config"]["headers"]["Authorization"], json!(MASK));
}

#[tokio::test]
async fn rejects_a_rest_mcp_with_no_tools_and_an_http_mcp_with_no_url() {
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "notools", "type": "rest", "baseUrl": "https://x.test", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("tools"),
        "{body}"
    );

    let (status, body) = h
        .post(
            "/api/mcps",
            json!({ "name": "nourl", "type": "http", "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("url is required"),
        "{body}"
    );
}

// --- deleting a CONFIG-sourced MCP -------------------------------------------------------------

#[tokio::test]
async fn removes_the_entry_from_gateway_config_json_and_the_runtime_registry() {
    // The delete must remove the server from gateway.config.json as well, or the next gateway
    // start resurrects it — which is why the panel used to hide Delete for config MCPs. The file
    // edit is surgical: only the named key goes, every other entry (and its ${ENV} refs) survives.
    //
    // Unlike the Node suite, the data dir is not swapped per test — one sandbox home serves the
    // whole binary — so this is the only test that touches gateway.config.json.
    let h = setup();
    let config = swiss_host::config::config_path();
    swiss_core::secure::statefile::write_secure_json(
        &config,
        &json!({
            "port": 19999,
            "tokenEnv": "SWISS_TOKEN",
            "servers": {
                "doomed": { "type": "echo" },
                "keeper": { "type": "echo", "password": "${KEEPER_PASS}" },
            },
        }),
    )
    .expect("seal a config file");
    h.register("doomed", json!({ "type": "echo" }));
    h.register("keeper", json!({ "type": "echo" }));

    let (status, body) = h.delete("/api/mcps/doomed").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("doomed"));
    assert_eq!(body["deleted"], json!(true));
    assert!(!h.registry.has("doomed"));

    let after = swiss_core::secure::statefile::read_secure_json(&config)
        .expect("the config is readable")
        .expect("the config is there");
    assert!(after["servers"].get("doomed").is_none(), "{after}");
    // Untouched, reference intact.
    assert_eq!(
        after["servers"]["keeper"],
        json!({ "type": "echo", "password": "${KEEPER_PASS}" })
    );
    let _ = std::fs::remove_file(&config);
}
