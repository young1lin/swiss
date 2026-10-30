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

#[tokio::test]
async fn shipped_skill_status_is_read_only_and_reports_all_three_roots() {
    let h = setup_machine(FakeUserPath::default());
    let (status, body) = h.get("/api/skills").await;
    assert_eq!(status, StatusCode::OK);
    let skills = body["skills"].as_array().expect("skills array");
    assert_eq!(skills.len(), 3);
    for skill in skills {
        let locations = skill["locations"].as_array().expect("locations");
        assert_eq!(locations.len(), 3);
        for location in locations {
            assert!(location["installed"].is_boolean() || location["installed"].is_null());
            assert!(location["path"].as_str().is_some_and(|p| p.ends_with("SKILL.md")));
        }
    }
    let (status, _) = h.put("/api/skills", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unregistered methods follow the host's 404 contract");
}

#[tokio::test]
async fn selected_skill_install_refuses_arbitrary_names_roots_and_paths() {
    let h = setup_machine(FakeUserPath::default());
    for body in [json!({}), json!({ "name": "../other", "root": ".codex" }),
        json!({ "name": "swiss", "root": "../other" }),
        json!({ "name": "swiss", "root": ".codex", "path": "C:/other" })] {
        let (status, _) = h.post("/api/skills/install", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
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
