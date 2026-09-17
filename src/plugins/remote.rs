/*
 * Copyright 2026 The swiss authors
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

//! The remote plugin's factory (docs/32): descriptor, config validation, and the
//! instance that owns the remote system's lifetime.
//!
//! A plugin built purely on the public contracts, like the terminal plugin before
//! it: everything the host needs is said here in data. The instance's whole job is
//! to open the [RemoteSystem] (the sealed target table plus the three actions),
//! install it into the state slot the /api/remote routes read, and take it back on
//! stop. The transport itself belongs to the tunnels plugin - this plugin declares
//! NO capability requirement on purpose: the target table must stay writable while
//! tunnels is off, and the exec actions say honestly what is missing (docs/32).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::RuntimeServices;
use swiss_remote::api::RemoteState;
use swiss_remote::RemoteSystem;

/// The plugin id, also the config row key and the routes' owner.
pub const PLUGIN_ID: &str = "remote";

pub struct RemotePlugin {
    services: Arc<RuntimeServices>,
    /// The slot the instance publishes the system into; the routes read the same one.
    state: Arc<RemoteState>,
    /// Where the sealed target table lives; overridden in tests.
    targets_path: std::path::PathBuf,
}

impl RemotePlugin {
    pub fn new(services: Arc<RuntimeServices>, state: Arc<RemoteState>) -> Self {
        RemotePlugin {
            services,
            state,
            targets_path: swiss_core::paths::data_path(&["remote.json"]),
        }
    }

    #[cfg(test)]
    fn with_targets_path(mut self, path: std::path::PathBuf) -> Self {
        self.targets_path = path;
        self
    }
}

#[async_trait]
impl PluginFactory for RemotePlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "remote".into(),
            label: "Remote".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            // Deliberately empty: there is nothing to configure yet, and an empty
            // schema still refuses unknown keys, so a typo fails at save.
            config_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            // The targets page (docs/32 R6): the CLI stays the primary client,
            // but a target no longer needs a terminal to exist. Before the
            // plugins page's 1000, after Terminal's 70.
            pages: vec![PageDescriptor {
                id: "remote".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "Remote Targets".into(),
                order: 75,
                path: "#remote".into(),
                entry: "/admin/js/views/remote.js".into(),
                sidebar: false,
                layout: "page",
            }],
            routes: vec!["/api/remote".into()],
            // The targets/routes have nothing to re-read; a config PUT does not
            // restart the instance.
            restart_on_config_change: false,
            // NOT ["remote-transport"] (docs/32): the target table must be
            // editable while tunnels is off - an unmet requirement would park
            // the whole plugin in waitingDependency and lock the table. Exec
            // actions answer honestly when the transport is missing.
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        let obj = config
            .as_object()
            .ok_or_else(|| "remote config must be an object".to_string())?;
        if let Some(key) = obj.keys().find(|k| k.as_str() != "enabled") {
            return Err(format!("remote config has no {key:?} field"));
        }
        Ok(())
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(RemoteInstance {
            state: self.state.clone(),
            services: self.services.clone(),
            targets_path: self.targets_path.clone(),
        }))
    }
}

struct RemoteInstance {
    state: Arc<RemoteState>,
    services: Arc<RuntimeServices>,
    targets_path: std::path::PathBuf,
}

#[async_trait]
impl PluginInstance for RemoteInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // One system for the whole plugin: the sealed target table and the three
        // capabilities registered into the shared action registry - a remote.exec
        // IS a run through the coordinator, not a new job system (docs/32).
        let system = RemoteSystem::open(self.services.clone(), self.targets_path.clone());
        swiss_remote::actions::register_all(system.clone(), &self.services.actions)?;
        self.state.install(system);
        Ok(())
    }

    async fn stop(&self) {
        // Withdraw first so no request finds a system that is about to die, then
        // unregister the capabilities so a disabled plugin answers "unknown action"
        // instead of running an action whose transport seat may be gone.
        if let Some(system) = self.state.withdraw() {
            for name in ["remote.exec", "remote.sync", "remote.pull"] {
                system.services().actions.unregister(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_carries_one_page_and_no_requirements() {
        let d = RemotePlugin::new(RuntimeServices::new(), RemoteState::new()).descriptor();
        assert_eq!(d.id, "remote");
        assert_eq!(d.pages.len(), 1);
        assert_eq!(d.pages[0].id, "remote");
        assert_eq!(d.pages[0].entry, "/admin/js/views/remote.js");
        assert_eq!(d.routes, vec!["/api/remote".to_string()]);
        assert!(d.requires.is_empty(), "must start without the transport");
    }

    #[test]
    fn unknown_config_keys_are_refused() {
        let plugin = RemotePlugin::new(RuntimeServices::new(), RemoteState::new());
        assert!(plugin.validate_config(&json!({})).is_ok());
        assert!(plugin.validate_config(&json!({ "endpoints": [] })).is_err());
    }

    #[tokio::test]
    async fn start_installs_stop_withdraws_and_unregisters() {
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-rplug-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let services = RuntimeServices::new();
        let state = RemoteState::new();
        let plugin = RemotePlugin::new(services.clone(), state.clone())
            .with_targets_path(dir.join("remote.json"));

        // The instance path: create through the factory (the host only wraps this),
        // start through a real scope, stop through the same method the host calls.
        let instance = plugin.create(&json!({})).await.expect("creates");
        let mut scope = PluginScope::new("remote");
        instance.clone().start(&mut scope).await.expect("starts");
        assert!(
            services
                .actions
                .list()
                .iter()
                .any(|a| a.type_name == "remote.exec"),
            "the capabilities registered",
        );
        instance.stop().await;
        assert!(
            !services
                .actions
                .list()
                .iter()
                .any(|a| a.type_name == "remote.exec"),
            "the capabilities unregistered",
        );
        // And the routes see the empty slot again.
        assert!(
            state.withdraw().is_none(),
            "stop already withdrew the system"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
