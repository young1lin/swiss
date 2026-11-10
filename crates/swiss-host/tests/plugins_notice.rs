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

//! HOST.PLUGINS (SPEC §host.plugins): enable, disable and a failed start each
//! publish once, a pass that sets the state already held publishes nothing, and
//! a config put publishes once. A refused put publishes nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::{json, Value};

use swiss_host::config_store::ConfigStore;
use swiss_host::host::factory::{PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::host::{PluginDescriptor, PluginHost, PluginState};

struct Fake {
    fail: AtomicBool,
}

struct Inst {
    fail: bool,
}

#[async_trait::async_trait]
impl PluginFactory for Fake {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: "alpha".into(),
            kind: "fake".into(),
            label: "alpha".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object" }),
            pages: vec![],
            routes: vec!["/api/alpha".into()],
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        if config.get("nope").is_some() {
            return Err("refused".into());
        }
        Ok(())
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(Inst { fail: self.fail.load(Ordering::SeqCst) }))
    }
}

#[async_trait::async_trait]
impl PluginInstance for Inst {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        if self.fail {
            return Err("boom".into());
        }
        Ok(())
    }
}

struct Rig {
    host: Arc<PluginHost>,
    notes: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new(fail: bool) -> Self {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let mut host = PluginHost::new(ConfigStore::memory(json!({
            "plugins": { "alpha": { "disabled": true } }
        })));
        host.register(Arc::new(Fake { fail: AtomicBool::new(fail) }))
            .expect("the fake plugin registers");
        let host = Arc::new(host);
        let sink = notes.clone();
        host.set_plugins_notice(Some(Arc::new(move |id: &str| {
            sink.lock().expect("notices").push(id.to_owned());
        })));
        Self { host, notes }
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.notes.lock().expect("notices"))
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        use axum::body::Body;
        use tower::util::ServiceExt;
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).expect("body")))
            .expect("request");
        let response = swiss_host::host::api::mount(self.host.clone())
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("bytes");
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }
}

// SPEC §host.plugins: a pass that sets the state the plugin already holds publishes nothing.
#[tokio::test]
async fn a_set_state_to_the_same_state_publishes_nothing() {
    let rig = Rig::new(false);
    rig.host.reconcile("alpha").await.expect("already disabled");
    assert!(rig.take().is_empty());
    assert_eq!(rig.host.plugin("alpha").expect("alpha").state(), PluginState::Disabled);
}

// SPEC §host.plugins: enable publishes once, keyed by the plugin id.
#[tokio::test]
async fn enable_publishes_once() {
    let rig = Rig::new(false);
    let (status, _) = rig.call("POST", "/api/plugins/alpha/enable", json!({ "revision": rig.host.revision() })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.take(), vec!["alpha".to_owned()]);
    assert_eq!(rig.host.plugin("alpha").expect("alpha").state(), PluginState::Active);
    // Settled: a second pass writes Active over Active and publishes nothing.
    rig.host.reconcile("alpha").await.expect("already active");
    assert!(rig.take().is_empty());
}

// SPEC §host.plugins: disable publishes once.
#[tokio::test]
async fn disable_publishes_once() {
    let rig = Rig::new(false);
    let rev = rig.host.revision();
    rig.call("POST", "/api/plugins/alpha/enable", json!({ "revision": rev })).await;
    rig.take();
    let rev = rig.host.revision();
    let (status, _) = rig.call("POST", "/api/plugins/alpha/disable", json!({ "revision": rev })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.take(), vec!["alpha".to_owned()]);
    assert_eq!(rig.host.plugin("alpha").expect("alpha").state(), PluginState::Disabled);
}

// SPEC §host.plugins: a failed start publishes once, with the plugin's id.
#[tokio::test]
async fn a_failed_start_publishes_once() {
    let rig = Rig::new(true);
    let (status, _) = rig.call("POST", "/api/plugins/alpha/enable", json!({ "revision": rig.host.revision() })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.take(), vec!["alpha".to_owned()]);
    let entry = rig.host.plugin("alpha").expect("alpha");
    assert_eq!(entry.state(), PluginState::Failed);
    assert!(entry.last_error().is_some());
}

// SPEC §host.plugins: a config put publishes once, and a refused put publishes nothing.
#[tokio::test]
async fn a_config_put_publishes_once_and_a_refused_put_publishes_nothing() {
    let rig = Rig::new(false);
    let (status, _) = rig
        .call("PUT", "/api/plugins/alpha/config", json!({
            "revision": rig.host.revision(),
            "config": { "nope": true }
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(rig.take().is_empty(), "a refused write publishes nothing");

    let (status, _) = rig
        .call("PUT", "/api/plugins/alpha/config", json!({
            "revision": rig.host.revision(),
            "config": {}
        }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.take(), vec!["alpha".to_owned()]);
}
