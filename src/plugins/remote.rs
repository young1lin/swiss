/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The remote plugin's factory (docs/34): descriptor, config validation, and the
//! instance that owns the remote system's lifetime.
//!
//! A plugin built purely on the public contracts, like the terminal plugin before
//! it: everything the host needs is said here in data. The instance's whole job is
//! to open the [RemoteSystem] (the sealed target table plus the remote actions),
//! install it into the state slot the /api/remote routes read, mount the same
//! surface as the builtin "remote" MCP under /mcp/remote (R7), and take both back
//! on stop. The transport itself belongs to the tunnels plugin - this plugin
//! declares NO capability requirement on purpose: the target table must stay
//! writable while tunnels is off, and the exec actions say honestly what is
//! missing (docs/34).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_core::log;
use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::RuntimeServices;
use swiss_mcp::calls::CallLog;
use swiss_mcp::registry::{Registry, Source};
use swiss_remote::api::RemoteState;
use swiss_remote::RemoteSystem;

/// The plugin id, also the config row key and the routes' owner.
pub const PLUGIN_ID: &str = "remote";

/// The name the instance registers its builtin MCP under (R7): /mcp/remote,
/// mounted like every other builtin, owned by this plugin's lifecycle.
const MCP_NAME: &str = "remote";

pub struct RemotePlugin {
    services: Arc<RuntimeServices>,
    /// The slot the instance publishes the system into; the routes read the same one.
    state: Arc<RemoteState>,
    /// The one system for the whole factory lifetime: the sealed target table, opened
    /// once here (not per start) so the targets group scope can register over it at
    /// the composition point and outlive start/stop - the same shape as the tunnel
    /// scopes over tunnels.json (docs/34 R8). A restart installs the SAME Arc; stop
    /// only withdraws it from the routes' slot.
    system: Arc<RemoteSystem>,
    /// The app's MCP registry, where the instance mounts its builtin entry (R7).
    registry: Arc<Registry>,
    /// The app's one call log, shared with every adapter so the Logs tab sees
    /// remote tool traffic in the same place as every other MCP's.
    call_log: Arc<CallLog>,
}

impl RemotePlugin {
    pub fn new(
        services: Arc<RuntimeServices>,
        state: Arc<RemoteState>,
        registry: Arc<Registry>,
        call_log: Arc<CallLog>,
    ) -> Self {
        RemotePlugin {
            system: RemoteSystem::open(
                services.clone(),
                swiss_core::paths::data_path(&["remote.json"]),
            ),
            services,
            state,
            registry,
            call_log,
        }
    }

    /// The system the targets group scope registers over (docs/34 R8): the SAME
    /// sealed table the plugin serves, so /api/groups/targets and /api/remote
    /// targets can never disagree about what is on disk.
    pub fn system(&self) -> Arc<RemoteSystem> {
        self.system.clone()
    }

    #[cfg(test)]
    fn with_targets_path(self, path: std::path::PathBuf) -> Self {
        RemotePlugin {
            system: RemoteSystem::open(self.services.clone(), path),
            services: self.services,
            state: self.state,
            registry: self.registry,
            call_log: self.call_log,
        }
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
            // Two pages, switched in the context bar (the panel's L2): the targets
            // page (docs/34 R6 - the CLI stays the primary client, but a target no
            // longer needs a terminal to exist) and the runs page (history.rs - what
            // ran, with its output, after the coordinator's ring forgot it). Before
            // the plugins page's 1000, after Terminal's 70.
            pages: vec![
                PageDescriptor {
                    id: "remote".into(),
                    plugin_id: PLUGIN_ID.into(),
                    label: "Targets".into(),
                    order: 75,
                    path: "#remote".into(),
                    entry: "/admin/js/views/remote.js".into(),
                    sidebar: false,
                    layout: "page",
                },
                PageDescriptor {
                    id: "remote-runs".into(),
                    plugin_id: PLUGIN_ID.into(),
                    label: "Runs".into(),
                    order: 76,
                    path: "#remote-runs".into(),
                    entry: "/admin/js/views/remote-runs.js".into(),
                    sidebar: false,
                    layout: "page",
                },
            ],
            routes: vec!["/api/remote".into()],
            // The targets/routes have nothing to re-read; a config PUT does not
            // restart the instance.
            restart_on_config_change: false,
            // NOT ["remote-transport"] (docs/34): the target table must be
            // editable while tunnels is off - `requires` is a functional
            // dependency, not a hard gate (WaitingDependency was never
            // implemented, docs/09 as-built; an unmet require is a plain route-
            // level refusal). Exec actions answer honestly when the transport
            // is missing.
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
            system: self.system.clone(),
            registry: self.registry.clone(),
            call_log: self.call_log.clone(),
        }))
    }
}

struct RemoteInstance {
    state: Arc<RemoteState>,
    services: Arc<RuntimeServices>,
    system: Arc<RemoteSystem>,
    registry: Arc<Registry>,
    call_log: Arc<CallLog>,
}

/// Whether the registry's "remote" entry is OURS (def type "remote"): a user MCP
/// that happens to carry the name must not be hijacked by a restart.
fn mcp_is_ours(registry: &Arc<Registry>) -> bool {
    registry
        .get(MCP_NAME)
        .and_then(|entry| entry.data.read().ok().map(|d| d.def.type_() == "remote"))
        .unwrap_or(false)
}

#[async_trait]
impl PluginInstance for RemoteInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // The factory's one system (docs/34 R8): the sealed target table and the
        // remote capabilities registered into the shared action registry - a
        // remote.exec IS a run through the coordinator, not a new job system
        // (docs/34). Reused across restarts so the group scope and the routes
        // always see the same table.
        let system = self.system.clone();
        swiss_remote::actions::register_all(system.clone(), &self.services.actions)?;
        // The durable run log (docs/34 follow-up, history.rs): from here every remote.*
        // run the coordinator starts is teed to disk and recorded at its end. A seat
        // like the others - stop() takes it back.
        let sink: Arc<dyn swiss_host::services::runs::RunHistorySink> = system.history().clone();
        self.services.runs.add_history_sink(sink);
        self.state.install(system.clone());

        // R7 (docs/34): the same table as an MCP under /mcp/remote - five thin
        // tools that run the actions just registered through the same
        // coordinator, so a model can drive a target with no shell. Plugin-owned
        // on purpose: the entry mounts here and comes down with stop(), so
        // disabling Remote withdraws the tools together with the actions they
        // dispatch to. The alias seam reads the LIVE table, so a target added on
        // the page shows up in the tool descriptions on the next tools/list.
        let adapter = Arc::new(swiss_mcp::adapters::remote::RemoteAdapter::new(
            self.services.clone(),
            self.call_log.clone(),
            Arc::new(move || {
                system.with_store(|store| {
                    store
                        .list()
                        .iter()
                        .map(|t| t.id.clone())
                        .collect::<Vec<_>>()
                })
            }),
        ));
        let def = swiss_host::config::ServerDef(
            json!({
                "type": "remote",
                "description": "Builtin: the gateway's remote targets as five tools (remote_exec, remote_sync, remote_pull, remote_cat, remote_write)."
            })
            .as_object()
            .cloned()
            .expect("the literal above is an object"),
        );
        if self.registry.has(MCP_NAME) {
            if mcp_is_ours(&self.registry) {
                // A restart after stop: swap in this instance's adapter (the old
                // one still holds the previous system's alias seam) and bring the
                // entry back up.
                self.registry
                    .update_def(MCP_NAME, def, adapter, true)
                    .await
                    .map_err(|err| format!("remote mcp restart: {err}"))?;
            } else {
                // A user MCP owns the name: theirs wins, ours stays unmounted,
                // and the rest of the plugin is unaffected.
                log::warn(
                    "the /mcp/remote path is taken by a user MCP; the builtin remote tools stay unmounted",
                    None,
                );
            }
        } else {
            self.registry
                .register(MCP_NAME, Source::Config, def, adapter)
                .map_err(|err| format!("remote mcp registration: {err}"))?;
            self.registry
                .start(MCP_NAME)
                .await
                .map_err(|err| format!("remote mcp start: {err}"))?;
        }
        Ok(())
    }

    async fn stop(&self) {
        // Withdraw first so no request finds a system that is about to die, then
        // unregister the capabilities so a disabled plugin answers "unknown action"
        // instead of running an action whose transport seat may be gone. By
        // prefix rather than a fixed list: actions this system added later
        // (remote.cat, remote.write, ...) withdraw without this list growing.
        if let Some(system) = self.state.withdraw() {
            for info in system.services().actions.list() {
                if info.type_name.starts_with("remote.") {
                    system.services().actions.unregister(&info.type_name);
                }
            }
            let sink: Arc<dyn swiss_host::services::runs::RunHistorySink> =
                system.history().clone();
            system.services().runs.remove_history_sink(&sink);
        }
        // The MCP entry follows the plugin down - stopped, not deleted: a user
        // panel stop of the entry survives a plugin bounce the same way, and the
        // restart path above swaps the adapter back in.
        if let Err(err) = self.registry.stop(MCP_NAME).await {
            log::warn(
                "the remote mcp entry did not stop cleanly",
                Some(json!({ "err": err })),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_mcp::registry::Lifecycle;

    /// A registry + call log pair that writes nowhere a test would mourn.
    fn test_registry() -> (Arc<Registry>, Arc<CallLog>) {
        let calls = Arc::new(CallLog::at(std::env::temp_dir().join(format!(
            "swiss-remoteplug-calls-{}",
            swiss_core::util::random_hex(8)
        ))));
        (Registry::new(15_000, calls.clone()), calls)
    }

    fn lifecycle_of(registry: &Arc<Registry>, name: &str) -> Lifecycle {
        registry
            .get(name)
            .and_then(|e| e.data.read().ok().map(|d| d.lifecycle))
            .expect("the entry exists")
    }

    #[test]
    fn the_descriptor_carries_two_pages_and_no_requirements() {
        let (registry, calls) = test_registry();
        let d = RemotePlugin::new(RuntimeServices::new(), RemoteState::new(), registry, calls)
            .descriptor();
        assert_eq!(d.id, "remote");
        // Targets and Runs are sibling pages of one plugin: the context bar switches
        // them (swiss-ui-design §5), so both ride the descriptor, targets first.
        assert_eq!(d.pages.len(), 2);
        assert_eq!(d.pages[0].id, "remote");
        assert_eq!(d.pages[0].entry, "/admin/js/views/remote.js");
        assert_eq!(d.pages[1].id, "remote-runs");
        assert_eq!(d.pages[1].entry, "/admin/js/views/remote-runs.js");
        assert!(d.pages[0].order < d.pages[1].order);
        assert_eq!(d.routes, vec!["/api/remote".to_string()]);
        assert!(d.requires.is_empty(), "must start without the transport");
    }

    #[test]
    fn unknown_config_keys_are_refused() {
        let (registry, calls) = test_registry();
        let plugin = RemotePlugin::new(RuntimeServices::new(), RemoteState::new(), registry, calls);
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
        let (registry, calls) = test_registry();
        let plugin = RemotePlugin::new(services.clone(), state.clone(), registry.clone(), calls)
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
        // R7: the plugin's start also mounted the builtin MCP, serving.
        assert!(registry.has("remote"), "the builtin MCP registered");
        assert_eq!(lifecycle_of(&registry, "remote"), Lifecycle::Started);
        assert_eq!(services.runs.history_sinks(), 1, "the run log took its seat");

        instance.stop().await;
        assert!(
            !services
                .actions
                .list()
                .iter()
                .any(|a| a.type_name == "remote.exec"),
            "the capabilities unregistered",
        );
        assert_eq!(services.runs.history_sinks(), 0, "stop gave the seat back");
        assert_ne!(
            lifecycle_of(&registry, "remote"),
            Lifecycle::Started,
            "the MCP entry stopped with the plugin"
        );
        // And the routes see the empty slot again.
        assert!(
            state.withdraw().is_none(),
            "stop already withdrew the system"
        );

        // A restart reuses the registered entry rather than colliding with it.
        let mut scope = PluginScope::new("remote");
        instance.clone().start(&mut scope).await.expect("restarts");
        assert_eq!(lifecycle_of(&registry, "remote"), Lifecycle::Started);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
