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

//! GET /api/update/check through a fake seam. The real GitHub client stays unbuilt.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;
use serde_json::json;

use super::{setup, setup_with};

enum Script {
    Release { tag: String, url: String, newer: bool },
    None,
    Err(String),
}

struct FakeUpdate {
    calls: AtomicUsize,
    script: Script,
}

impl FakeUpdate {
    fn release(tag: &str, url: &str, newer: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            script: Script::Release {
                tag: tag.to_string(),
                url: url.to_string(),
                newer,
            },
        }
    }

    fn none() -> Self {
        Self { calls: AtomicUsize::new(0), script: Script::None }
    }

    fn err(message: &str) -> Self {
        Self { calls: AtomicUsize::new(0), script: Script::Err(message.to_string()) }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl swiss::update_check::UpdateCheck for FakeUpdate {
    async fn check(&self) -> Result<swiss::update_check::UpdateInfo, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (latest, newer) = match &self.script {
            Script::Release { tag, url, newer } => (
                Some(swiss::update_check::Release { tag: tag.clone(), url: url.clone() }),
                *newer,
            ),
            Script::None => (None, false),
            Script::Err(message) => return Err(message.clone()),
        };
        Ok(swiss::update_check::UpdateInfo {
            current: swiss::update_check::current_version(),
            latest,
            newer,
            plan: None,
        })
    }
}

fn mount(fake: Arc<FakeUpdate>) -> super::Harness {
    setup_with(move |ctx| {
        let _ = ctx.update_check.set(fake);
    })
}

#[tokio::test]
async fn unset_update_check_answers_503_without_calling_out() {
    let h = setup();
    // check() waits up to 10s on GitHub. An answer inside 1s is the proof the route
    // returned before building that client.
    let answered = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        h.get("/api/update/check"),
    )
    .await;
    let (status, body) = answered.expect("an unset seam must answer without waiting on the network");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("update check is not available in this composition"));
    assert!(body.get("latest").is_none(), "{body}");
}

#[tokio::test]
async fn update_check_reports_a_newer_release_and_does_not_cache_it() {
    let fake = Arc::new(FakeUpdate::release("v9.9.9", "https://example.com/rel", true));
    let h = mount(fake.clone());
    let (status, body) = h.get("/api/update/check").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(body["currentVersion"], json!(swiss::update_check::current_version()));
    assert_ne!(body["current"], body["currentVersion"]);
    assert_eq!(body["newer"], json!(true));
    assert_eq!(body["latest"]["tag"], json!("v9.9.9"));
    assert_eq!(body["latest"]["url"], json!("https://example.com/rel"));
    let (again, _) = h.get("/api/update/check").await;
    assert_eq!(again, StatusCode::OK);
    assert_eq!(fake.calls(), 2, "each click checks again; the route holds no cached release");
}

#[tokio::test]
async fn update_check_reports_no_releases() {
    let fake = Arc::new(FakeUpdate::none());
    let h = mount(fake.clone());
    let (status, body) = h.get("/api/update/check").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["latest"].is_null(), "{body}");
    assert_eq!(body["newer"], json!(false));
    assert_eq!(body["current"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn update_check_reports_unreachable() {
    let fake = Arc::new(FakeUpdate::err("cannot reach github: offline"));
    let h = mount(fake.clone());
    let (status, body) = h.get("/api/update/check").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], json!("update.unreachable"));
    assert_eq!(body["error"], json!("cannot reach github: offline"));
    assert_eq!(fake.calls(), 1);
}
