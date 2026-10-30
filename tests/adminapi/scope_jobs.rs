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

//! The jobs scope over the group-scope family (SPEC §host.groups).

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use super::{sandbox, Harness, TOKEN};
use swiss::app::{build_app, AppContext};
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::registry::Registry;

// --- the jobs scope over the family (SPEC §host.groups) --------------------------------------------------
/// The jobs-scope harness: a real JobSystem over a real config store, its row seeded with
/// two manual jobs, registered into the family the way server.rs composes it. What comes
/// back is the system handle, for asserting the applied table directly.
fn setup_with_jobs() -> (Harness, Arc<swiss_jobs::jobs::JobSystem>) {
    sandbox();
    let dir = std::env::temp_dir().join(format!(
        "swiss-adminapi-jobs-{}",
        swiss_core::util::random_hex(8),
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    let traffic = Arc::new(swiss_mcp::traffic::TrafficLog::memory());
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(dir.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    let config_store = swiss_host::config_store::ConfigStore::from_loaded(
        dir.join("gateway.config.json"),
        json!({}),
    );
    let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
    // The one capability a definition can lean on in a test: the built-in legacy command
    // action, registered the way the jobs plugin's own tests do it.
    services
        .actions
        .register(Arc::new(
            swiss_host::services::actions::LegacyCommandAction::new(services.supervisor.clone()),
        ))
        .expect("the legacy command capability registers once");
    let jobs =
        swiss_jobs::jobs::JobSystem::open(dir.join("jobs.json"), services, config_store.clone());
    config_store
        .update_plugin(
            "jobs",
            config_store.snapshot().revision,
            json!({
                "definitions": {
                    "vacuum": {
                        "trigger": { "kind": "manual" },
                        "action": { "type": "process.legacy-command", "input": { "command": "echo v" } },
                    },
                    "report": {
                        "trigger": { "kind": "manual" },
                        "action": { "type": "process.legacy-command", "input": { "command": "echo r" } },
                    },
                },
            }),
        )
        .expect("seed the jobs row");
    jobs.apply_config(&config_store.plugin_config("jobs"))
        .expect("apply the seed");
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        traffic.clone(),
        "SWISS_TOKEN",
        19999,
    );
    swiss_jobs::jobs::groups::register_job_scopes(&ctx.group_scopes, &jobs);
    // The jobs routes are the plugin's, merged the way server.rs merges them - the family
    // alone is not enough to prove /api/jobs rows carry the group.
    let app = build_app(ctx, None).merge(swiss_jobs::jobs::api::mount(jobs.clone()));
    (
        Harness {
            app,
            registry,
            store,
            calls,
            traffic,
            path: dir.join("managed.json"),
        },
        jobs,
    )
}

#[tokio::test]
async fn the_family_serves_the_jobs_scope() {
    let (h, jobs) = setup_with_jobs();

    // The whole list, one request shape like every scope.
    let (status, body) = h
        .put("/api/groups/jobs", json!({ "groups": ["default", "Ops"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["default", "Ops"]));

    // Assign keeps the canonical casing and lands in the row the scheduler reads.
    let (status, body) = h
        .put("/api/groups/jobs/members/vacuum", json!({ "group": "ops" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));

    // The list answers the applied table: rows carry their group, the top level the names.
    let (status, list) = h.get("/api/jobs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["groups"], json!(["default", "Ops"]));
    let row = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["name"] == json!("vacuum"))
        .expect("the vacuum row");
    assert_eq!(row["group"], json!("Ops"));

    // A rename carries the members and answers how many moved.
    let (status, body) = h
        .post(
            "/api/groups/jobs/rename",
            json!({ "from": "default", "to": "Basics" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"], json!(["Basics", "Ops"]));

    // The flat order rewrites the definition order in the row.
    let before: Vec<String> = jobs.job_ids();
    let (status, body) = h
        .put(
            "/api/groups/jobs/order",
            json!({ "order": [before[1].clone(), before[0].clone()] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["order"],
        json!([before[1].clone(), before[0].clone()]),
        "the persisted order is the answer"
    );
    assert_eq!(jobs.job_ids(), vec![before[1].clone(), before[0].clone()]);

    // Unknown member: the family's one 404.
    let (status, body) = h
        .put("/api/groups/jobs/members/ghost", json!({ "group": null }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body["error"],
        json!("unknown member: ghost"),
        "the family words it, not the scope"
    );
}

/// The same Move up shape through the jobs scope: definitions that render in the first
/// group by default (no group key in the row) must stay there when the list reorders.
#[tokio::test]
async fn reordering_job_groups_never_rehomes_the_first_groups_default_members() {
    let (h, _jobs) = setup_with_jobs();
    h.put("/api/groups/jobs", json!({ "groups": ["g1", "g2"] }))
        .await;
    h.put("/api/groups/jobs/members/vacuum", json!({ "group": "g2" }))
        .await;

    let (status, put) = h
        .put("/api/groups/jobs", json!({ "groups": ["g2", "g1"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    assert_eq!(put["groups"], json!(["g2", "g1"]));

    let (_, list) = h.get("/api/jobs").await;
    let group_of = |name: &str| {
        list["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == json!(name))
            .unwrap_or_else(|| panic!("no row for {name}"))["group"]
            .clone()
    };
    assert_eq!(
        group_of("vacuum"),
        json!("g2"),
        "the explicit member never moves"
    );
    assert_eq!(group_of("report"), json!("g1"), "the default member stayed");
}

#[tokio::test]
async fn moving_a_job_between_groups_advances_the_config_revision_in_place() {
    // SPEC §jobs 8's guarantee, restated for groups (SPEC §host.groups): a group mutation is a config
    // edit, so the running table moves without a restart - configRevision advances and the
    // definitions all survive the apply.
    let (h, jobs) = setup_with_jobs();
    h.put("/api/groups/jobs", json!({ "groups": ["default", "Ops"] }))
        .await;
    let before = jobs.applied_revision();
    let (status, body) = h
        .put("/api/groups/jobs/members/report", json!({ "group": "Ops" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["group"], json!("Ops"));
    let after = jobs.applied_revision();
    assert!(
        after != before,
        "revision {before} -> {after}: the row is content-revised, so a change means the\n        new row was written AND applied, not parked"
    );
    assert_eq!(jobs.job_ids().len(), 2, "the table survived the apply");
}
