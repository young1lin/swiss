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

//! The machine settings (src/autostart.rs, src/userpath.rs) through fake providers.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use super::{setup, setup_with, Harness};

// --- the machine settings (src/autostart.rs, src/userpath.rs) ------------------------------------
//
// Both write the real machine, so the suite never mounts the OS providers: these fakes hold the
// switch in memory. The suite once drove /api/autostart against the real registration and left
// the operator's start-at-sign-in removed after every run.

#[derive(Default)]
struct FakeAutoStart(std::sync::Mutex<bool>);

impl swiss::autostart::AutoStart for FakeAutoStart {
    fn status(&self) -> swiss::autostart::AutoStartState {
        swiss::autostart::AutoStartState {
            enabled: *self.0.lock().expect("fake lock"),
            detail: "fake registration".to_string(),
            command: "\"swiss\" start --no-open".to_string(),
        }
    }

    fn set(&self, enabled: bool) -> Result<swiss::autostart::AutoStartState, String> {
        *self.0.lock().expect("fake lock") = enabled;
        Ok(self.status())
    }
}

/// `refuse` makes every write fail the way a locked-down profile would.
#[derive(Default)]
struct FakeUserPath {
    on: std::sync::Mutex<bool>,
    refuse: bool,
}

impl swiss::userpath::UserPath for FakeUserPath {
    fn status(&self) -> swiss::userpath::UserPathState {
        swiss::userpath::UserPathState {
            enabled: *self.on.lock().expect("fake lock"),
            detail: "fake user PATH".to_string(),
            dir: "/opt/swiss".to_string(),
        }
    }

    fn set(&self, enabled: bool) -> Result<swiss::userpath::UserPathState, String> {
        if self.refuse {
            return Err("could not write the user PATH".to_string());
        }
        *self.on.lock().expect("fake lock") = enabled;
        Ok(self.status())
    }
}

fn setup_machine(user_path: FakeUserPath) -> Harness {
    setup_with(|ctx| {
        let _ = ctx.autostart.set(Arc::new(FakeAutoStart::default()));
        let _ = ctx.user_path.set(Arc::new(user_path));
    })
}

/// Serializes the tests that touch the shared skill directories on disk. A taker holds it
/// for the whole test, across its awaits - that breadth is the point - so each carries
/// `#[allow(clippy::await_holding_lock)]`.
fn skill_files_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn shipped_skill_status_is_read_only_and_reports_both_roots() {
    let _guard = skill_files_lock();
    let h = setup_machine(FakeUserPath::default());
    let (status, body) = h.get("/api/skills").await;
    assert_eq!(status, StatusCode::OK);
    let skills = body["skills"].as_array().expect("skills array");
    assert_eq!(skills.len(), 3);
    for skill in skills {
        assert!(skill["description"].as_str().is_some_and(|d| !d.is_empty()), "{skill}");
        assert!(skill["loadsByItself"].is_boolean(), "{skill}");
        let locations = skill["locations"].as_array().expect("locations");
        assert_eq!(locations.iter().map(|l| l["root"].as_str()).collect::<Vec<_>>(), [Some(".claude"), Some(".agents")]);
        for location in locations {
            assert!(location["installed"].is_boolean() || location["installed"].is_null());
            assert!(location["state"].is_string(), "{location}");
            assert!(location["path"].as_str().is_some_and(|p| p.ends_with("SKILL.md")));
            assert!(std::path::Path::new(location["path"].as_str().expect("path")).starts_with(swiss_core::paths::data_dir()), "skill inspection must stay in the sandbox");
        }
    }
    let (status, _) = h.put("/api/skills", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unregistered methods follow the host's 404 contract");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selected_skill_install_refuses_arbitrary_names_roots_and_paths() {
    let _guard = skill_files_lock();
    let h = setup_machine(FakeUserPath::default());
    for body in [json!({}), json!({ "name": "../other", "root": ".agents" }),
        json!({ "name": "swiss", "root": "../other" }),
        json!({ "name": "swiss", "root": ".codex" }),
        json!({ "name": "swiss", "root": ".cursor" }),
        json!({ "name": "swiss", "root": ".agents", "path": "C:/other" })] {
        let (status, _) = h.post("/api/skills/install", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    // A refusal the panel can show carries the code it translates (SPEC §panel.i18n).
    for (body, code) in [
        (json!({ "name": "../other", "root": ".agents" }), "skills.unknownSkill"),
        (json!({ "name": "swiss", "root": ".codex" }), "skills.unknownDir"),
        (json!({ "name": "../other", "dir": "C:/other" }), "skills.unknownDir"),
    ] {
        let (status, reply) = h.post("/api/skills/install", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
        assert_eq!(reply["code"], code, "{reply}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn skill_seats_are_listed_added_refreshed_and_forgotten() {
    let _guard = skill_files_lock();
    let h = setup_machine(FakeUserPath::default());
    let user = std::path::PathBuf::from(std::env::var_os("USERPROFILE").expect("sandbox home"));
    let ledger = swiss_core::paths::data_dir().join("skill-seats.json");
    let _ = std::fs::remove_file(&ledger);

    let (status, body) = h.get("/api/skills").await;
    assert_eq!(status, StatusCode::OK);
    let seats = body["seats"].as_array().expect("seats");
    assert!(seats.len() >= 2, "{body}");
    for seat in seats {
        let dir = seat["dir"].as_str().expect("dir");
        assert!(std::path::Path::new(dir).starts_with(swiss_core::paths::data_dir()), "{dir}");
        assert!(seat["exists"].is_boolean());
        assert!(seat["source"].is_string());
    }

    let (status, body) = h.post("/api/skills/install", json!({
        "name": "swiss",
        "dir": user.join("not-a-seat"),
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "skills.unknownDir", "{body}");

    let (status, body) = h.post("/api/skills/seats", json!({ "kind": "claude", "dir": "relative/claude" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "skills.seatInvalid", "{body}");

    let config = user.join("claude-b");
    std::fs::create_dir_all(&config).expect("mkdir");
    let (status, body) = h.post("/api/skills/seats", json!({ "kind": "claude", "dir": &config })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let added = body["seats"].as_array().expect("seats").iter().find(|seat| seat["source"] == "ledger");
    let added = added.expect("the new seat").clone();
    assert_eq!(added["kind"], "claude");
    assert!(std::path::Path::new(added["dir"].as_str().expect("dir")).starts_with(swiss_core::paths::data_dir()));
    // The config directory exists and its skills/ does not yet: that is a live seat to install into.
    assert_eq!(added["exists"], true, "{body}");
    let added_dir = std::path::PathBuf::from(added["dir"].as_str().expect("dir"));
    assert!(!added_dir.exists());
    let (status, body) = h.post("/api/skills/install", json!({ "name": "swiss", "dir": &added_dir })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(added_dir.join("swiss").join("SKILL.md").is_file());

    let planted = user.join(".agents").join("skills").join("swiss").join("SKILL.md");
    std::fs::create_dir_all(planted.parent().expect("dir")).expect("mkdir");
    std::fs::write(&planted, "an older copy").expect("plant");
    let (status, body) = h.post("/api/skills/refresh", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["replaced"].as_array().expect("replaced").iter().any(|p| p.as_str() == Some(&planted.to_string_lossy())), "{body}");
    assert!(body["skills"].is_array());

    let (status, body) = h.delete_json("/api/skills/seats", json!({
        "dir": user.join(".agents").join("skills"),
    })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "skills.seatDefault", "{body}");

    // With the config directory gone the seat is missing, and an install never recreates it.
    std::fs::remove_dir_all(&config).expect("remove the config dir");
    let (status, body) = h.get("/api/skills").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let gone = body["seats"].as_array().expect("seats").iter().find(|seat| seat["source"] == "ledger").expect("still listed");
    assert_eq!(gone["exists"], false, "{body}");
    let (status, body) = h.post("/api/skills/install", json!({ "name": "swiss", "dir": &added_dir })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "skills.seatInvalid", "{body}");
    assert!(!config.exists(), "the refused install created nothing");

    let (status, body) = h.delete_json("/api/skills/seats", json!({
        "dir": added["dir"].as_str().expect("dir"),
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seats"].as_array().expect("seats").iter().all(|seat| seat["source"] != "ledger"));
    assert!(body.get("ledger").is_none(), "a readable ledger adds no note: {body}");

    // A ledger that cannot be parsed hides its seats, so the inspection says why.
    std::fs::write(&ledger, "{ not json").expect("break the ledger");
    let (status, body) = h.get("/api/skills").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["ledger"]["error"].as_str().is_some_and(|e| e.contains("was not changed")), "{body}");
    assert!(body["ledger"]["path"].as_str().is_some_and(|p| p.ends_with("skill-seats.json")), "{body}");
    let (status, body) = h.post("/api/skills/refresh", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["skipped"].as_array().expect("skipped").iter().any(|s| s.as_str().is_some_and(|s| s.contains("was not changed"))), "{body}");
    assert_eq!(std::fs::read_to_string(&ledger).expect("kept"), "{ not json", "the broken ledger is left byte for byte");

    let _ = std::fs::remove_file(&ledger);
    let _ = std::fs::remove_dir_all(user.join(".agents"));
    let _ = std::fs::remove_dir_all(&config);
}

#[tokio::test]
async fn machine_settings_round_trip_through_their_providers() {
    let h = setup_machine(FakeUserPath::default());
    for route in ["/api/autostart", "/api/user-path"] {
        let (status, body) = h.get(route).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
        assert_eq!(body["enabled"], false, "{route}: {body}");
        assert!(body["detail"].is_string(), "{route}: detail says where it lives: {body}");

        let (status, body) = h.put(route, json!({ "enabled": true })).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
        assert_eq!(body["enabled"], true, "{route}: the answer is the state read back: {body}");
        let (_, body) = h.get(route).await;
        assert_eq!(body["enabled"], true, "{route}: {body}");

        let (status, body) = h.put(route, json!({ "enabled": false })).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
        assert_eq!(body["enabled"], false, "{route}: {body}");
    }
    let (_, body) = h.get("/api/autostart").await;
    assert!(body["command"].is_string(), "command says what the entry runs: {body}");
    let (_, body) = h.get("/api/user-path").await;
    assert_eq!(body["dir"], "/opt/swiss", "dir says what a terminal will find: {body}");
}

#[tokio::test]
async fn machine_settings_refuse_a_body_without_a_verdict() {
    let h = setup_machine(FakeUserPath::default());
    for route in ["/api/autostart", "/api/user-path"] {
        for body in [json!({}), json!({ "enabled": "yes" })] {
            let (status, answer) = h.put(route, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {answer}");
            assert!(answer["error"].is_string(), "{route}: {answer}");
        }
    }
}

#[tokio::test]
async fn a_failed_machine_write_is_a_500_that_says_why() {
    let h = setup_machine(FakeUserPath {
        refuse: true,
        ..FakeUserPath::default()
    });
    let (status, body) = h.put("/api/user-path", json!({ "enabled": true })).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "could not write the user PATH", "{body}");
}

#[tokio::test]
async fn machine_settings_without_a_provider_answer_503_and_write_nothing() {
    // The composition every other test in this file uses: nothing mounted, so nothing on the
    // host can be read or written through these routes.
    let h = setup();
    for route in ["/api/autostart", "/api/user-path"] {
        let (status, body) = h.get(route).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{route}: {body}");
        let (status, body) = h.put(route, json!({ "enabled": false })).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{route}: {body}");
    }
}
