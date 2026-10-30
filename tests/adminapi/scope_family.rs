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

//! The group-scope route family (SPEC §host.groups): one set of routes per scope, the tunnel
//! and remote target scopes, and the retired MCP-only routes.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use super::{field_of, row_named, sandbox, Harness, TOKEN};
use super::groups::with_mcps;
use swiss::app::{build_app, AppContext};
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::registry::Registry;

// --- the group-scope route family (SPEC §host.groups) ---------------------------------------------------
//
// One family over every scope; mcps is the one this harness wires, and its behavior pins the
// shape every other scope answers with.

#[tokio::test]
async fn the_family_replaces_the_whole_group_list_per_scope() {
    let h = with_mcps();
    let (status, body) = h
        .put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Docs"]));
    // The old shape and the new reach the same store — the family is the panel's one door.
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["default", "Docs"]));
    // Malformed bodies, duplicates and the last-group deletion are 400s.
    for groups in [json!("Docs"), json!(["a", "a"]), json!([])] {
        let (status, _) = h.put("/api/groups/mcps", json!({ "groups": groups })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{groups}");
    }
}

#[tokio::test]
async fn the_family_assigns_a_member_and_answers_the_canonical_group() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;

    let (status, body) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": "docs" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "Docs" })); // canonical casing, not what was typed

    let (status, _) = h
        .put(
            "/api/groups/mcps/members/context7",
            json!({ "group": null }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(row_named(&list, "context7")["group"], json!("default"));

    // 404 for a member nothing serves; 400 for a group nothing holds.
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
    assert_eq!(
        h.put("/api/groups/mcps/members/context7", json!({ "group": 3 }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_family_persists_the_scope_order() {
    let h = with_mcps();
    let (status, body) = h
        .put(
            "/api/groups/mcps/order",
            json!({ "order": ["deepwiki", "context7", "github"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!(["deepwiki", "context7", "github"]));
    let (_, list) = h.get("/api/mcps").await;
    assert_eq!(
        field_of(&list["mcps"], "name"),
        ["deepwiki", "context7", "github"]
    );
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": "context7" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .put("/api/groups/mcps/order", json!({ "order": [1, 2] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_scope_is_a_named_404_on_every_family_route() {
    let h = with_mcps();
    for (method, path, body) in [
        ("PUT", "/api/groups/nope", json!({ "groups": ["default"] })),
        (
            "POST",
            "/api/groups/nope/rename",
            json!({ "from": "a", "to": "b" }),
        ),
        (
            "PUT",
            "/api/groups/nope/members/x",
            json!({ "group": null }),
        ),
        ("PUT", "/api/groups/nope/order", json!({ "order": [] })),
    ] {
        let (status, resp) = match method {
            "PUT" => h.put(path, body).await,
            _ => h.post(path, body).await,
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(
            resp["error"],
            json!("unknown scope: nope"),
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn the_family_rename_body_is_from_to_and_missing_fields_are_400() {
    let h = with_mcps();
    h.put("/api/groups/mcps", json!({ "groups": ["default", "Docs"] }))
        .await;
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "to": "X" })) // no from
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "from": "Docs" })) // no to
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // The old body shape ({name}) is gone with the old route: it no longer parses as a rename.
    let (status, _) = h
        .post("/api/groups/mcps/rename", json!({ "name": "X" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// setup(), plus the two tunnel scopes registered over a scratch tunnels.json — the way
/// server.rs composes them after building the store. What comes back is the store handle,
/// for asserting what actually landed on disk.
fn setup_with_tunnels() -> (
    Harness,
    Arc<std::sync::Mutex<swiss_tunnels::tunnel::TunnelStore>>,
) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-tunnels-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let tun = Arc::new(std::sync::Mutex::new(
        swiss_tunnels::tunnel::TunnelStore::new(dir.join("tunnels.json"), 19999),
    ));
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        traffic.clone(),
        "SWISS_TOKEN",
        19999,
    );
    swiss_tunnels::tunnel::register_tunnel_scopes(&ctx.group_scopes, &tun);
    (
        Harness {
            app: build_app(ctx, None),
            registry,
            store,
            calls,
            traffic,
            path: dir.join("managed.json"),
        },
        tun,
    )
}

/// Seed one connection + two rules; the ids are what the family's member routes take.
fn seed_tunnels(
    tun: &Arc<std::sync::Mutex<swiss_tunnels::tunnel::TunnelStore>>,
) -> (String, String, String) {
    let mut s = tun.lock().unwrap();
    let c = s
        .add_connection(&swiss_tunnels::tunnel::store::ConnInput {
            name: "bastion".into(),
            host: "127.0.0.1".into(),
            port: 22.0,
            username: "u".into(),
            auth_type: swiss_tunnels::tunnel::types::AuthType::Password,
            password: Some("p".into()),
            ..Default::default()
        })
        .expect("a connection");
    let r1 = s
        .add_rule(&swiss_tunnels::tunnel::store::RuleInput {
            name: "pg".into(),
            connection_id: c.id.clone(),
            local_port: 5433.0,
            target_host: "127.0.0.1".into(),
            target_port: 5432.0,
            ..Default::default()
        })
        .expect("a rule");
    let r2 = s
        .add_rule(&swiss_tunnels::tunnel::store::RuleInput {
            name: "redis".into(),
            connection_id: c.id.clone(),
            local_port: 6379.0,
            target_host: "127.0.0.1".into(),
            target_port: 6379.0,
            ..Default::default()
        })
        .expect("a rule");
    (c.id, r1.id, r2.id)
}

#[tokio::test]
async fn the_family_serves_the_tunnel_scopes() {
    let (h, tun) = setup_with_tunnels();
    let (c, r1, r2) = seed_tunnels(&tun);

    // The whole-list mutation, on the scope word the family owns (conns, not connections).
    let (status, body) = h
        .put(
            "/api/groups/conns",
            json!({ "groups": ["default", "prod"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "prod"]));

    // Assignment is by tunnel id, answers the canonical casing, and lands in tunnels.json.
    let (status, body) = h
        .put(
            &format!("/api/groups/conns/members/{c}"),
            json!({ "group": "PROD" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "prod" }));
    assert_eq!(
        tun.lock().unwrap().connection(&c).unwrap().group,
        Some("prod".to_string())
    );

    // Rename keeps the slot and counts the members that rode along explicitly.
    h.put(
        "/api/groups/rules",
        json!({ "groups": ["default", "Learning"] }),
    )
    .await;
    h.put(
        &format!("/api/groups/rules/members/{r2}"),
        json!({ "group": "default" }),
    )
    .await;
    let (status, body) = h
        .post(
            "/api/groups/rules/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["Basics", "Learning"]));
    assert_eq!(body["moved"], 1, "r2 was an explicit member of default");
    assert_eq!(
        tun.lock().unwrap().rule(&r2).unwrap().group,
        Some("Basics".to_string())
    );
    // An implicit member needs no rewrite: the sink slot kept its place, so it follows.
    assert_eq!(tun.lock().unwrap().rule(&r1).unwrap().group, None);

    // The scope's flat order: one list per call now, not both at once.
    let (status, body) = h
        .put(
            "/api/groups/rules/order",
            json!({ "order": [r2.clone(), r1.clone()] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!([r2, r1]));
    assert_eq!(tun.lock().unwrap().rules()[0].name, "redis");

    // Unknown member is the family's 404, with the family's wording.
    let (status, body) = h
        .put("/api/groups/rules/members/nope", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], json!("unknown member: nope"));
}

/// setup(), plus the targets scope registered over a scratch sealed table - the way
/// server.rs composes it over the remote plugin's one system (SPEC §remote.targets). What comes
/// back is the system handle, for asserting what actually landed on disk.
fn setup_with_remote() -> (Harness, Arc<swiss_remote::RemoteSystem>) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-remote-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let system = swiss_remote::RemoteSystem::open(
        swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default()),
        dir.join("remote.json"),
    );
    system
        .with_store(|s| {
            s.add(swiss_remote::target::RemoteTarget {
                id: "build".into(),
                label: "Build box".into(),
                endpoint: "conn-1".into(),
                workspace_root: "/data/ws".into(),
                shell: "posix".into(),
                capabilities: vec!["exec".into()],
                default_timeout_ms: None,
                group: None,
            })
            .map(|_| ())
        })
        .expect("seed");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        traffic.clone(),
        "SWISS_TOKEN",
        19999,
    );
    swiss_remote::register_remote_scopes(&ctx.group_scopes, &system);
    (
        Harness {
            app: build_app(ctx, None),
            registry,
            store,
            calls,
            traffic,
            path: dir,
        },
        system,
    )
}

/// The seventh scope (SPEC §remote.targets): targets answers the same family routes the tunnel
/// scopes do, over the one sealed table, while the remote plugin itself is not even
/// registered here - grouping outlives start/stop by construction.
#[tokio::test]
async fn the_family_serves_the_targets_scope() {
    let (h, system) = setup_with_remote();

    // The whole-list mutation, on the scope word the family owns.
    let (status, body) = h
        .put("/api/groups/targets", json!({ "groups": ["default", "prod"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "prod"]));

    // Assignment is by target id, answers the canonical casing, and lands in the file.
    let (status, body) = h
        .put("/api/groups/targets/members/build", json!({ "group": "PROD" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "group": "prod" }));
    assert_eq!(
        system.with_store(|s| s.group_of("build")),
        "prod",
        "the sealed table answers the same word the family answered"
    );

    // Rename counts the explicit members that rode along.
    let (status, body) = h
        .post(
            "/api/groups/targets/rename",
            json!({ "from": "prod", "to": "Production" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!(["default", "Production"]));
    assert_eq!(body["moved"], 1);

    // The scope's flat order is array order in the file.
    let (status, body) = h
        .put(
            "/api/groups/targets/order",
            json!({ "order": ["build"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!(["build"]));

    // Unknown member is the family's 404, with the family's wording.
    let (status, body) = h
        .put("/api/groups/targets/members/ghost", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], json!("unknown member: ghost"));
}

#[tokio::test]
async fn the_retired_mcp_group_and_order_routes_are_gone() {
    let (h, _) = setup_with_tunnels();
    // SPEC §host.groups: one family, one door. The per-mcp group route, the bare /api/groups and the
    // unscoped /api/order all answered the same store the family does - keeping them is how
    // two copies of one protocol drift apart.
    for (method, path, body) in [
        ("PUT", "/api/order", json!({ "order": [] })),
        ("PUT", "/api/groups", json!({ "groups": ["default"] })),
        (
            "PUT",
            "/api/groups/mcps/members/ghost",
            json!({ "group": null }),
        ),
    ] {
        let (status, _) = match method {
            "PUT" => h.put(path, body).await,
            _ => h.post(path, body).await,
        };
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}
