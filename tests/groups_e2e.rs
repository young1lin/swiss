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

//! End-to-end for the group family: the real app on a REAL loopback socket, driven by a real
//! HTTP client, across a RESTART of the whole app over the same home.
//!
//! The oneshot integration tests in adminapi.rs pin each route contract in isolation. What
//! only this layer can prove: the wire (a real client's Host header through the loopback
//! guard, real JSON over the socket), and the boot-after-boot half of the pinning fix - the
//! members a reorder pinned must still render under their group when a FRESH app opens the
//! state files the old one wrote. The panel reads exactly the two fields asserted here
//! (list["groups"], row["group"] - groups.js slice/groupOf), so this is the user's Move up
//! flow, end to end, including the restart the operator never sees.

use std::sync::Arc;

use serde_json::{json, Value};

use swiss::app::{build_app, AppContext};
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::adapters::make_adapter;
use swiss_mcp::registry::{Registry, Source};

/// One app over one scratch home, served on an ephemeral loopback port. Dropping the guard
/// aborts the server task; the store persists synchronously per mutation, so nothing is lost.
struct Served {
    base: String,
    _server: tokio::task::JoinHandle<()>,
}

impl Served {
    async fn boot(dir: &std::path::Path, names: &[&str]) -> Self {
        let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
        let registry = Registry::new(60_000, calls.clone());
        for name in names {
            let def = swiss_host::config::ServerDef(
                json!({ "type": "echo" })
                    .as_object()
                    .expect("object")
                    .clone(),
            );
            let adapter = make_adapter(&def, name, &calls).expect("adapter");
            registry
                .register(name, Source::Config, def, adapter)
                .expect("register");
        }
        let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
        let tokens = Arc::new(TokenManager::new(store.clone(), None));
        let ctx = AppContext::new(
            registry.clone(),
            tokens,
            store,
            calls,
            "MCP_GATEWAY_TOKEN",
            19999,
        );
        let app = build_app(ctx, None);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Served {
            base: format!("http://127.0.0.1:{}", addr.port()),
            _server: server,
        }
    }

    async fn get(&self, path: &str) -> Value {
        let res = reqwest::Client::new()
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .expect("GET over the wire");
        assert_eq!(res.status(), reqwest::StatusCode::OK, "{path}");
        res.json().await.expect("json body")
    }

    async fn put_groups(&self, groups: &[&str]) {
        let res = reqwest::Client::new()
            .put(format!("{}/api/groups/mcps", self.base))
            .json(&json!({ "groups": groups }))
            .send()
            .await
            .expect("PUT over the wire");
        assert_eq!(res.status(), reqwest::StatusCode::OK);
    }
}

/// The group of every named MCP, as the panel reads it.
fn groups_of(list: &Value) -> Vec<(String, String)> {
    list["mcps"]
        .as_array()
        .expect("mcps array")
        .iter()
        .map(|row| {
            (
                row["name"].as_str().expect("name").to_string(),
                row["group"].as_str().expect("group").to_string(),
            )
        })
        .collect()
}

/// The user's exact report, end to end: g1 first holding members nobody ever assigned, g2
/// empty, one Move up (the whole list back with g2 first) - and then the part no other layer
/// covers: the app is RESTARTED over the home the reorder wrote, and the members stay put.
#[tokio::test]
async fn move_up_keeps_members_across_the_wire_and_a_restart() {
    let dir = std::env::temp_dir().join(format!(
        "swiss-groups-e2e-{}",
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).expect("scratch home");
    // Safety: this test binary's own scratch home, set once before any state file opens, the
    // same pin every suite uses so nothing touches the operator's real home or OS keystore.
    unsafe {
        std::env::set_var("MCP_GATEWAY_HOME", &dir);
        std::env::set_var("MCP_GATEWAY_MASTER_KEY", "cd".repeat(32));
    }
    let names = ["context7", "deepwiki", "github"];

    let a = Served::boot(&dir, &names).await;
    a.put_groups(&["g1", "g2"]).await;
    let list = a.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["g1", "g2"]));
    assert!(groups_of(&list).iter().all(|(_, g)| g == "g1"));

    // The Move up: the panel's ellipsis menu sends exactly this whole-list replace.
    a.put_groups(&["g2", "g1"]).await;
    let list = a.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["g2", "g1"]), "the order changed");
    assert!(
        groups_of(&list).iter().all(|(_, g)| g == "g1"),
        "but nobody re-homed: {:?}",
        groups_of(&list)
    );

    // A fresh app over the same home - the restart an operator's next boot is.
    drop(a);
    let b = Served::boot(&dir, &names).await;
    let list = b.get("/api/mcps").await;
    assert_eq!(list["groups"], json!(["g2", "g1"]));
    assert!(
        groups_of(&list).iter().all(|(_, g)| g == "g1"),
        "the pins survived the restart: {:?}",
        groups_of(&list)
    );
    std::fs::remove_dir_all(&dir).ok();
}
