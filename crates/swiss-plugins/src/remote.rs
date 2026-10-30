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

//! The remote plugin's factory (SPEC §remote): descriptor, config validation, and the
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
//! missing (SPEC §remote).

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
    /// scopes over tunnels.json (SPEC §remote.targets). A restart installs the SAME Arc; stop
    /// only withdraws it from the routes' slot.
    system: Arc<RemoteSystem>,
    /// The app's MCP registry, where the instance mounts its builtin entry (R7).
    registry: Arc<Registry>,
    /// The app's one call log, shared with every adapter so the Logs tab sees
    /// remote tool traffic in the same place as every other MCP's.
    call_log: Arc<CallLog>,
    /// The home's logs root — where the module's `logs/remote.db` lives. The db opens on
    /// start (never at construction: SPEC `§host.history` L1) and closes bounded on stop.
    logs_dir: std::path::PathBuf,
}

impl RemotePlugin {
    pub fn new(
        services: Arc<RuntimeServices>,
        state: Arc<RemoteState>,
        registry: Arc<Registry>,
        call_log: Arc<CallLog>,
        logs_dir: std::path::PathBuf,
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
            logs_dir,
        }
    }

    /// The system the targets group scope registers over (SPEC §remote.targets): the SAME
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
            logs_dir: self.logs_dir,
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
            // page (SPEC §remote.panel - the CLI stays the primary client, but a target no
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
            // NOT ["remote-transport"] (SPEC §remote): the target table must be
            // editable while tunnels is off - `requires` is a functional
            // dependency, not a hard gate (WaitingDependency was never
            // implemented, SPEC §host.plugins; an unmet require is a plain route-
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
            logs_dir: self.logs_dir.clone(),
        }))
    }
}

struct RemoteInstance {
    state: Arc<RemoteState>,
    services: Arc<RuntimeServices>,
    system: Arc<RemoteSystem>,
    registry: Arc<Registry>,
    call_log: Arc<CallLog>,
    logs_dir: std::path::PathBuf,
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
        // The factory's one system (SPEC §remote.targets): the sealed target table and the
        // remote capabilities registered into the shared action registry - a
        // remote.exec IS a run through the coordinator, not a new job system
        // (SPEC §remote). Reused across restarts so the group scope and the routes
        // always see the same table.
        let system = self.system.clone();
        // The module's own database (SPEC `§remote.history`, `§host.history`): opens FIRST,
        // so the legacy import folds before any new record can land and the very
        // first finish of a fresh cutover commits into the module's authority. A
        // `failed` open degrades to the jsonl path — a storage decision is a log line,
        // never a failed module start.
        match swiss_remote::store::RemoteHistory::open(&self.logs_dir).await {
            Ok(history) => {
                let (runs, bytes, import) = history.usage().await;
                system.history().attach_history(history.clone()).await;
                // The cross-module query seat (SPEC §host.history): this database is
                // searchable while it is open, and reported skipped once it is not.
                let _ = system.services().history_query.register(
                    swiss_host::services::history_query::HistoryQueryAdapter {
                        owner: "remote".into(),
                        db_file: self.logs_dir.join("remote.db"),
                        source: history.clone(),
                        bodies: Some(std::sync::Arc::new(
                            swiss_remote::store::RemoteBodyReader::new(history.clone()),
                        )),
                    },
                );
                // The hourly maintenance seat (SPEC §host.history): checkpoint,
                // conditional VACUUM, ledger reconciliation and the age sweep, run by
                // the server's retention timer while this database is open.
                {
                    let history = history.clone();
                    system.services().maintenance.register(
                        "remote",
                        std::sync::Arc::new(move || {
                            let history = history.clone();
                            Box::pin(async move {
                                history
                                    .maintain(7 * 24 * 60 * 60 * 1000)
                                    .await;
                            })
                        }),
                    );
                }
                swiss_core::log::log(
                    "info",
                    "remote history database attached",
                    Some(json!({ "runs": runs, "bytes": bytes, "legacyImport": import })),
                );
            }
            Err(err) => swiss_core::log::warn(
                "the remote history database did not open; recording stays on the jsonl path",
                Some(json!({ "err": err })),
            ),
        }
        swiss_remote::actions::register_all(system.clone(), &self.services.actions)?;
        // The durable run log (SPEC §remote.history, history.rs): from here every remote.*
        // run the coordinator starts is teed to disk and recorded at its end. A seat
        // like the others - stop() takes it back.
        let sink: Arc<dyn swiss_host::services::runs::RunHistorySink> = system.history().clone();
        // add_history_sink may raise the numbering floor through the same
        // cross-process claim as an allocation - off the runtime thread, like the
        // submit route.
        let handle = tokio::runtime::Handle::current();
        let runs_for_floor = self.services.runs.clone();
        if let Err(join) = tokio::task::spawn_blocking(move || {
            let _rt = handle.enter();
            runs_for_floor.add_history_sink(sink);
        })
        .await
        {
            // A sink that failed to attach stops teeing remote runs to disk: that
            // must be visible, not swallowed.
            swiss_core::log::error(
                "the remote history sink did not attach",
                Some(json!({ "reason": join.to_string() })),
            );
        }
        self.state.install(system.clone());

        // R7 (SPEC §remote): the same table as an MCP under /mcp/remote - five thin
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
            // The query seat goes with the sink: a stopped module is reported
            // skipped by name, and its database is never auto-opened by a query.
            system.services().history_query.unregister("remote");
            // The maintenance hook goes with the seat: a stopped module's closed
            // database is never touched by the hourly timer.
            system.services().maintenance.unregister("remote");
            // R2 (SPEC `§remote.history`): the actions are withdrawn, so runs still executing
            // one of them are canceled BY ACTION IDENTITY — whoever submitted them
            // `(manual, jobs, another MCP)` — and awaited before the module's storage closes:
            // no accepted remote work outlives its owner, and no unrelated run (a
            // terminal, a process) is touched.
            let _ = system
                .services()
                .runs
                .cancel_action_types(&[
                    "remote.exec",
                    "remote.sync",
                    "remote.pull",
                    "remote.cat",
                    "remote.write",
                ])
                .await;
        }
        // Detach the module's database, then close it bounded: accepted writes are
        // acknowledged through the sink above; an incomplete close is a warning,
        // never a claimed release (R3).
        if let Some(history) = self.system.history().detach_history().await {
            if let Err(err) = history.close().await {
                swiss_core::log::warn(
                    "the remote history database did not close within its bound",
                    Some(json!({ "err": err })),
                );
            }
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

    /// SPEC `§host.history` L1, the remote module half: constructing the plugin (composition,
    /// a disabled module) opens no history path — no jsonl tree, no database. The open
    /// happens on start.
    #[test]
    fn a_constructed_remote_plugin_opens_no_history() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-remoteplug-l1-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (registry, calls) = test_registry();
        let plugin = RemotePlugin::new(
            RuntimeServices::new(swiss_host::services::BootInfo::default()),
            RemoteState::new(),
            registry,
            calls,
            dir.join("logs"),
        );
        let _ = plugin.system(); // the composition point's own read
        assert!(
            !dir.join("logs").join("remote").exists(),
            "no jsonl tree before start",
        );
        assert!(
            !dir.join("logs").join("remote.db").exists(),
            "no module database before start",
        );
    }

    #[test]
    fn the_descriptor_carries_two_pages_and_no_requirements() {
        let (registry, calls) = test_registry();
        let d = RemotePlugin::new(
            RuntimeServices::new(swiss_host::services::BootInfo::default()),
            RemoteState::new(),
            registry,
            calls,
            std::env::temp_dir(),
        )
            .descriptor();
        assert_eq!(d.id, "remote");
        // Targets and Runs are sibling pages of one plugin: the context bar switches
        // them (SPEC §panel.nav), so both ride the descriptor, targets first.
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
        let plugin = RemotePlugin::new(
            RuntimeServices::new(swiss_host::services::BootInfo::default()),
            RemoteState::new(),
            registry,
            calls,
            std::env::temp_dir(),
        );
        assert!(plugin.validate_config(&json!({})).is_ok());
        assert!(plugin.validate_config(&json!({ "endpoints": [] })).is_err());
    }

    #[tokio::test]
    async fn start_installs_stop_withdraws_and_unregisters() {
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-rplug-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        let state = RemoteState::new();
        let (registry, calls) = test_registry();
        let plugin = RemotePlugin::new(
            services.clone(),
            state.clone(),
            registry.clone(),
            calls,
            std::env::temp_dir(),
        )
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
