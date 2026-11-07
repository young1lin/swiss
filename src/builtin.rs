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

//! The composition table: the built-in plugins of this build, their descriptors, and the
//! wrappers that bind the EXISTING subsystems (registry, tunnel manager, job system, db
//! browser) to the host lifecycle. Everything business-shaped lives here, not in the host
//! core — adding a plugin is one factory plus one `register` line (SPEC §host.plugins).
//!
//! Every wrapper documents, in its own comments, what disable REALLY does to its subsystem.
//! That is the honesty rule of SPEC §host.lifecycle: a wrapper may stop short of a full unload, but it
//! must never claim one it does not perform.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_core::log;
use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::engine::PluginHost;
use swiss_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::managed::ManagedStore;
use swiss_host::services::RuntimeServices;
use swiss_jobs::jobs::JobSystem;
use swiss_mcp::registry::{is_lazy, Registry, Source};
use swiss_tunnels::tunnel::manager::TunnelManager;

pub const MCP_ID: &str = "mcp";
pub const TUNNELS_ID: &str = "tunnels";
pub const DATA_ID: &str = "data";
pub const JOBS_ID: &str = "jobs";
pub use swiss_plugins::process::PROCESS_ID;

/// One built-in page: id-derived hash path (`#mcps`) and entry (`/admin/js/views/<id>.js`),
/// sidebar only for the primary MCPs view — the built-in view table of this build. The
/// layout names how the shell frames the PAGE BODY (resource/page/workspace, SPEC §panel.nav as
/// revised): resource = master-detail owning the shell sidebar, page = ordinary content
/// body, workspace = full-bleed body. The shell always draws its own chrome either way;
/// every built-in states the layout so the wire never has to guess from sidebar alone.
fn page(
    id: &str,
    plugin_id: &str,
    label: &str,
    order: i64,
    sidebar: bool,
    layout: &'static str,
) -> PageDescriptor {
    PageDescriptor {
        id: id.to_string(),
        plugin_id: plugin_id.to_string(),
        label: label.to_string(),
        order,
        path: format!("#{id}"),
        entry: format!("/admin/js/views/{id}.js"),
        sidebar,
        layout,
    }
}

/// Everything the built-in wrappers need from the boot sequence. Shared downstream objects
/// (the registry, stores, the managers) are constructed ONCE by server.rs and wrapped here —
/// the plugins own their RUNTIME state (started/stopped), not the objects' construction.
pub struct BuiltinDeps {
    pub registry: Arc<Registry>,
    pub managed: Arc<ManagedStore>,
    pub jobs: Arc<JobSystem>,
    pub tunnels: Arc<swiss_tunnels::tunnel::api::Tunnels>,
    pub tunnel_manager: Arc<TunnelManager>,
    /// The shared action registry, run pool, process supervisor and connection catalog.
    /// Capability plugins register INTO these; the host's /api/actions, /api/runs and
    /// the Data plugin's leases read them.
    pub services: Arc<RuntimeServices>,
    /// The MCP module's history seam (SPEC §host.history): the shared call log, traffic
    /// log and the home's logs directory. The MCP plugin opens `logs/mcp.db` in
    /// its enabled scope and attaches both — construction alone stays I/O-free (L1).
    pub call_log: Arc<swiss_mcp::calls::CallLog>,
    pub traffic: Arc<swiss_mcp::traffic::TrafficLog>,
    pub logs_dir: std::path::PathBuf,
    /// The /api/process routes' state slot (SPEC §process.apps): built by the boot
    /// beside the terminal and remote seats, handed both to the process factory
    /// (which installs) and to the mounted router (which reads).
    pub process_state: Arc<swiss_process::api::ProcessState>,
    /// The consoles' per-database history (SPEC §data.history), built closed: the Data plugin
    /// opens it in start and closes it in stop, so a disabled Data view holds no file.
    pub data_history: Arc<swiss_data::history::DataHistory>,
}

/// The row of a plugin that reads nothing from it refuses every key, so a typo in a manifest or a
/// script is told it did nothing rather than saved and ignored (SPEC §host.config).
fn takes_no_keys(id: &str, config: &Value) -> Result<(), String> {
    let fields = config
        .as_object()
        .ok_or_else(|| format!("{id} config must be an object"))?;
    match fields.keys().next() {
        Some(key) => Err(format!("{id} config has no {key:?} field")),
        None => Ok(()),
    }
}

/// Register every built-in of this build, in inventory/page order: MCP+Traffic, Tunnels,
/// Data, Jobs. Boot STARTS them in the reverse (see PluginHost::register) so tunnels come up
/// before the MCPs that may ride them.
pub fn register_all(host: &mut PluginHost, deps: &BuiltinDeps) -> Result<(), String> {
    host.register(Arc::new(McpPlugin {
        registry: deps.registry.clone(),
        managed: deps.managed.clone(),
        services: deps.services.clone(),
        call_log: deps.call_log.clone(),
        traffic: deps.traffic.clone(),
        logs_dir: deps.logs_dir.clone(),
    }))?;
    host.register(Arc::new(TunnelsPlugin {
        tunnels: deps.tunnels.clone(),
        manager: deps.tunnel_manager.clone(),
        services: deps.services.clone(),
    }))?;
    host.register(Arc::new(DataPlugin {
        history: deps.data_history.clone(),
        services: deps.services.clone(),
    }))?;
    host.register(Arc::new(JobsPlugin {
        jobs: deps.jobs.clone(),
    }))?;
    // Registered LAST so it STARTS FIRST (boot walks the reverse): a capability provider
    // must be registered before anything can submit a run against it. The factory lives
    // in swiss-plugins (it has a page and routes now, like terminal and remote).
    host.register(Arc::new(swiss_plugins::process::ProcessPlugin::new(
        deps.services.clone(),
        deps.process_state.clone(),
    )))
}

// --- MCP + Traffic (one plugin, two views) ---

struct McpPlugin {
    registry: Arc<Registry>,
    managed: Arc<ManagedStore>,
    /// The connection catalog lives here (SPEC §host.seats): MCP is the one provider, Data the
    /// one consumer, and neither reaches into the other's objects anymore.
    services: Arc<RuntimeServices>,
    /// The module's history seam (SPEC §host.history): opened on start, closed on stop.
    call_log: Arc<swiss_mcp::calls::CallLog>,
    traffic: Arc<swiss_mcp::traffic::TrafficLog>,
    logs_dir: std::path::PathBuf,
}

#[async_trait]
impl PluginFactory for McpPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: MCP_ID.into(),
            kind: "mcp".into(),
            label: "MCP".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object", "properties": {} }),
            pages: vec![
                page("mcps", MCP_ID, "Servers", 10, true, "resource"),
                PageDescriptor {
                    path: "#mcps-traffic".into(),
                    ..page("traffic", MCP_ID, "Traffic", 20, false, "page")
                },
                // Token management sits beside Servers and Traffic because the token exists
                // FOR MCP clients — every connect command embeds one, Traffic attributes by
                // it. The PAGE belongs to the MCP group; the /api/tokens routes stay HOST-
                // owned (below), so the credential keeps working whatever a plugin does.
                PageDescriptor {
                    path: "#mcps-tokens".into(),
                    ..page("tokens", MCP_ID, "Token", 30, false, "page")
                },
            ],
            routes: vec!["/api/mcps".into(), "/api/traffic".into()],
            // The plugin reads nothing from a config row today; restarting every hosted MCP
            // because a row moved would be destruction disguised as "applying config".
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        takes_no_keys(MCP_ID, config)
    }

    /// A row an older build saved reads as nothing here, so it cannot stop the plugin at boot.
    fn validate_config_for_start(&self, _config: &Value) -> Result<(), String> {
        Ok(())
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(McpInstance {
            registry: self.registry.clone(),
            managed: self.managed.clone(),
            services: self.services.clone(),
            call_log: self.call_log.clone(),
            traffic: self.traffic.clone(),
            logs_dir: self.logs_dir.clone(),
            history: tokio::sync::Mutex::new(None),
            tracker: swiss_host::services::catalog::LeaseTracker::new(),
        }))
    }
}

/// The MCP+Traffic instance.
///
/// WHAT DISABLE REALLY DOES (the SPEC §host.lifecycle honesty rule, applied): stopping this plugin
/// closes EVERY hosted MCP — `adapter.close()` tree-kills proc children and tears down DB
/// pools, which is the real memory this process spends — and stops the health/idle timer.
/// While stopped, the client catch-all and the /api/mcps + /api/traffic trees answer the
/// host's 503; no lazy proc is spawned behind them.
///
/// WHAT IT DOES NOT DO: unregister anything. Definitions, sidebar order, call logs and
/// managed.json all survive by design; enabling re-runs the boot start decision (panel Stop
/// honoured, lazy MCPs stay idle). That is a stop-and-release, NOT an unload of the registry
/// — and it is not labelled one.
struct McpInstance {
    registry: Arc<Registry>,
    managed: Arc<ManagedStore>,
    services: Arc<RuntimeServices>,
    call_log: Arc<swiss_mcp::calls::CallLog>,
    traffic: Arc<swiss_mcp::traffic::TrafficLog>,
    logs_dir: std::path::PathBuf,
    /// The module database this instance opened on start — held so stop can close the
    /// very pool it opened, not whatever another instance later opened.
    history: tokio::sync::Mutex<Option<Arc<swiss_mcp::history::McpHistory>>>,
    /// The lease ledger backing the catalog this instance registers. One per instance:
    /// a stopped instance drains ITS leases, never the next one's.
    tracker: Arc<swiss_host::services::catalog::LeaseTracker>,
}

#[async_trait]
impl PluginInstance for McpInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // The history database opens FIRST (SPEC §host.history): boot-gated, migrated,
        // legacy-imported and attached before any hosted MCP can record a call, so the
        // very first row of a fresh cutover lands in the module's own authority. A failed
        // open degrades to the jsonl fallback — one module's storage decision is a log
        // line, never a failed start.
        match swiss_mcp::history::McpHistory::open(&self.logs_dir).await {
            Ok(history) => {
                let (calls, traffic, _bytes, import) = history.usage().await;
                self.call_log.attach_history(history.clone());
                self.traffic.attach_history(history.clone()).await;
                // The cross-module query seat (SPEC §host.history): searchable while
                // this database is open, reported skipped by name once it is not.
                let _ = self.services.history_query.register(
                    swiss_host::services::history_query::HistoryQueryAdapter {
                        owner: "mcp".into(),
                        db_file: self.logs_dir.join("mcp.db"),
                        source: history.clone(),
                        bodies: Some(std::sync::Arc::new(
                            swiss_mcp::history::McpBodyReader::new(history.clone()),
                        )),
                    },
                );
                // The hourly maintenance seat (SPEC §host.history): checkpoint,
                // conditional VACUUM, ledger reconciliation and the age sweep, run by
                // the server's retention timer while this database is open.
                {
                    let history = history.clone();
                    self.services.maintenance.register(
                        "mcp",
                        std::sync::Arc::new(move || {
                            let history = history.clone();
                            Box::pin(async move {
                                history.maintain(180 * 24 * 60 * 60 * 1000).await;
                            })
                        }),
                    );
                }
                *self.history.lock().await = Some(history);
                swiss_core::log::log(
                    "info",
                    "mcp history database attached",
                    Some(serde_json::json!({
                        "calls": calls,
                        "traffic": traffic,
                        "legacyImport": import,
                    })),
                );
            }
            Err(err) => {
                swiss_core::log::warn(
                    "mcp history database unavailable; logging falls back to jsonl",
                    Some(serde_json::json!({ "err": err })),
                );
            }
        }
        self.registry.start_timer();
        start_hosted_mcps(&self.registry, &self.managed).await;
        // Register the connection catalog LAST, so a failed start never leaves a catalog
        // behind that a stopped registry is supposedly serving (SPEC §host.seats).
        self.services
            .catalog
            .register(
                Arc::new(
                    swiss_mcp::registry::RegistryCatalog::new(
                        self.registry.clone(),
                        self.tracker.clone(),
                    )
                    .with_module_dbs(self.services.module_dbs.clone()),
                ),
                MCP_ID,
            )
            .map_err(|err| format!("connection catalog registration: {err}"))?;
        Ok(())
    }

    async fn stop(&self) {
        // The W3 order (SPEC §host.seats): withdraw first (no NEW leases reach a pool that is about
        // to close), drain what is in flight with an honest warning when the wait was not
        // clean, close the pools, and only then free the catalog seat for the next start.
        self.services.catalog.begin_withdraw();
        // Async, never the condvar: the leases being drained are held by /api/db futures on
        // THIS runtime's one thread — a blocking wait here would freeze the whole gateway
        // (health, panel, MCP traffic) for the drain window, with the leases structurally
        // unable to come back. See LeaseTracker::wait_idle_async.
        swiss_host::services::catalog::drain_leases(
            &self.tracker,
            std::time::Duration::from_millis(swiss_host::services::catalog::DRAIN_TIMEOUT_MS),
            MCP_ID,
        )
        .await;
        // close_all stops the health/idle timer together with every entry (registry.rs).
        self.registry.close_all().await;
        self.services.catalog.clear();
        // The history database closes LAST, after the adapters that recorded into it:
        // detach (later traffic falls back to jsonl instead of vanishing), then the
        // bounded close — an unresolved close is logged as what it is, never swallowed.
        self.call_log.detach_history();
        self.traffic.detach_history();
        // The query seat goes with the sinks: a stopped module is reported skipped
        // by name, and its database is never auto-opened by a query.
        self.services.history_query.unregister("mcp");
        // The maintenance hook goes with the seat: a stopped module's closed database
        // is never touched by the hourly timer.
        self.services.maintenance.unregister("mcp");
        let history = self.history.lock().await.take();
        if let Some(history) = history {
            if let Err(err) = history.close().await {
                swiss_core::log::warn(
                    "mcp history database close incomplete",
                    Some(serde_json::json!({ "err": err })),
                );
            }
        }
    }
}

/// The boot-start decision for every REGISTERED MCP — the same gate the boot loop in
/// server.rs used to run inline: honour a panel Stop (managed.json's enabled map), leave a
/// lazy MCP idle for its first request, start the rest. One failure is logged and isolated;
/// the remaining MCPs still come up.
pub async fn start_hosted_mcps(registry: &Arc<Registry>, managed: &Arc<ManagedStore>) {
    for name in registry.names() {
        let Some(entry) = registry.get(&name) else {
            continue;
        };
        if entry.deleted.load(Ordering::SeqCst) {
            continue; // delete() won a race — never build on a tombstoned entry
        }
        let (source, def) = {
            let Ok(d) = entry.data.read() else {
                continue;
            };
            (d.source, d.def.clone())
        };
        let label = match source {
            Source::Config => "config mcp",
            Source::Managed => "managed mcp",
        };
        let result = async {
            if managed.enabled_for(&name) == Some(false) {
                log::log(
                    "info",
                    &format!("{label} stays stopped (panel Stop)"),
                    Some(json!({ "name": name, "type": def.type_() })),
                );
                return Ok(());
            }
            if is_lazy(&def) {
                // The memory rule: an idle npx/uvx child is 50-150 MB of nothing, so lazy
                // MCPs wait for the first request even when the plugin is serving.
                log::log(
                    "info",
                    label,
                    Some(json!({ "name": name, "type": def.type_(), "state": "idle" })),
                );
                return Ok(());
            }
            registry.start(&name).await?;
            log::log(
                "info",
                label,
                Some(json!({ "name": name, "type": def.type_(), "state": "ready" })),
            );
            Ok::<(), String>(())
        }
        .await;
        if let Err(err) = result {
            log::error(
                &format!("{label} init failed"),
                Some(json!({ "name": name, "err": err })),
            );
        }
    }
}

// --- Tunnels ---

struct TunnelsPlugin {
    tunnels: Arc<swiss_tunnels::tunnel::api::Tunnels>,
    manager: Arc<TunnelManager>,
    services: Arc<RuntimeServices>,
}

#[async_trait]
impl PluginFactory for TunnelsPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: TUNNELS_ID.into(),
            kind: "tunnels".into(),
            label: "Tunnels".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object", "properties": {} }),
            // Two L2 pages, one group (SPEC §panel.nav): the old page-local
            // SSH Connections / Port Forwards segmented control became real sibling
            // pages. `#tunnels` keeps its saved links as SSH Connections;
            // `#tunnel-forwards` is the new sibling at the adjacent order, and
            // the rail still holds ONE tunnels seat.
            pages: vec![
                page("tunnels", TUNNELS_ID, "SSH Connections", 30, false, "page"),
                page(
                    "tunnel-forwards",
                    TUNNELS_ID,
                    "Port Forwards",
                    35,
                    false,
                    "page",
                ),
            ],
            routes: vec!["/api/tunnels".into()],
            restart_on_config_change: true,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        takes_no_keys(TUNNELS_ID, config)
    }

    /// A row an older build saved reads as nothing here, so it cannot stop the plugin at boot.
    fn validate_config_for_start(&self, _config: &Value) -> Result<(), String> {
        Ok(())
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(TunnelsInstance {
            tunnels: self.tunnels.clone(),
            manager: self.manager.clone(),
            services: self.services.clone(),
            shells: swiss_tunnels::tunnel::shell::TunnelShells::new(self.manager.clone()),
            remote: swiss_tunnels::tunnel::remote::TunnelRemote::new(self.manager.clone()),
        }))
    }
}

/// What disable really does: `close_all` ends every SSH session and every forward — every
/// local port is released — while tunnels.json's enabled flags survive untouched, so the
/// next start brings back exactly the set that was running. The first-run forward-port
/// import also moved INSIDE start: a disabled tunnels plugin no longer touches tunnels.json
/// at boot (server.rs used to do the import before any lifecycle existed).
struct TunnelsInstance {
    tunnels: Arc<swiss_tunnels::tunnel::api::Tunnels>,
    manager: Arc<TunnelManager>,
    services: Arc<RuntimeServices>,
    /// The interactive-shell provider this instance registers (SPEC §terminal.remote). One per
    /// INSTANCE: a stopped instance drains ITS sessions, never the next one's.
    shells: Arc<swiss_tunnels::tunnel::shell::TunnelShells>,
    /// The remote-execution transport this instance registers (SPEC §remote.transport). Same
    /// manager, same connection inventory, same refcounting — a remote exec shares the
    /// client a tunnel or a terminal already holds.
    remote: Arc<swiss_tunnels::tunnel::remote::TunnelRemote>,
}

#[async_trait]
impl PluginInstance for TunnelsInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        if let Ok(mut store) = self.tunnels.store.lock() {
            if store.is_fresh() {
                if let Some((rules, connections)) =
                    swiss_tunnels::tunnel::import::import_forward_port(&mut store, None)
                {
                    log::log(
                        "info",
                        "imported forward-port tunnels on first run",
                        Some(json!({ "rules": rules, "connections": connections })),
                    );
                }
            }
        }
        // Boot handshakes take seconds and fail independently — a SCOPED task now, tracked
        // and cancelled by the host on stop (this used to be a detached tokio::spawn the
        // shutdown path could not account for).
        // Lower the close_all gate synchronously, before the routes mount: a Start click that
        // beats the boot task to the manager must not be refused as "closed".
        self.manager.reopen();
        let manager = self.manager.clone();
        scope.spawn(async move {
            for result in manager.start_enabled().await {
                if !result.ok {
                    log::warn(
                        "tunnel boot failed",
                        Some(json!({ "rule": result.name, "error": result.error })),
                    );
                }
            }
        });
        // Register the shell capability LAST, for the same reason the MCP plugin registers
        // its catalog last: a failed start must never leave a provider behind that a
        // stopped subsystem is supposedly serving (SPEC §host.seats, SPEC §terminal.remote). The remote
        // transport registers just before it under the same rule: if either
        // registration fails, this start returns Err having left nothing behind.
        self.services
            .remote
            .register(self.remote.clone(), TUNNELS_ID)
            .map_err(|err| format!("remote transport registration: {err}"))?;
        self.services
            .shells
            .register(self.shells.clone(), TUNNELS_ID)
            .map_err(|err| {
                // The remote provider registered above must not survive a failed start.
                self.services.remote.clear();
                format!("interactive shell registration: {err}")
            })?;
        Ok(())
    }

    async fn stop(&self) {
        // The same withdraw-drain-close order the catalog uses (SPEC §host.seats): no NEW
        // terminals reach a client that is about to close, the live ones get a short
        // window, and the warning names how many were closed over. The window is short on
        // purpose — an attached terminal does not hand itself back (SPEC §terminal.remote).
        //
        // The remote transport withdraws FIRST (SPEC §remote.transport): it must reject new
        // operations before anything closes, but it does NOT get a drain window — a
        // remote exec is a RUN with its own deadline and its own cancel path, and the
        // connections closing underneath turn any survivor into an honestly-failed run.
        self.services.remote.begin_withdraw();
        self.services.shells.begin_withdraw();
        self.shells.ledger().begin_withdraw();
        swiss_host::services::shell::drain_sessions(
            self.shells.ledger(),
            std::time::Duration::from_millis(swiss_host::services::shell::DRAIN_TIMEOUT_MS),
            TUNNELS_ID,
        )
        .await;
        self.manager.close_all().await;
        self.services.remote.clear();
        self.services.shells.clear();
    }
}

// --- Data ---

/// The resolver seam is gone, replaced by the connection catalog in RuntimeServices (SPEC
/// §host.seats): Data LEASES what it browses. Its one field is the consoles' history (SPEC
/// §data.history), the one file Data owns.
struct DataPlugin {
    history: Arc<swiss_data::history::DataHistory>,
    services: Arc<RuntimeServices>,
}

#[async_trait]
impl PluginFactory for DataPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: DATA_ID.into(),
            kind: "data".into(),
            label: "Data".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object", "properties": {} }),
            // workspace (SPEC §panel.nav): a full-bleed page BODY — the dense
            // explorer consumes everything under the context bar. The shell still draws
            // its own chrome; this only frames the body.
            pages: vec![page("data", DATA_ID, "Data", 40, false, "workspace")],
            routes: vec!["/api/db".into()],
            restart_on_config_change: false,
            // The honest dependency, stated as data (SPEC §host.seats): Data browses through the
            // connection catalog the MCP plugin provides. The inventory carries this with
            // a met/unmet verdict, so "disable MCP" no longer silently paralyses Data.
            requires: vec!["connection-catalog".into()],
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        takes_no_keys(DATA_ID, config)
    }

    /// A row an older build saved reads as nothing here, so it cannot stop the plugin at boot.
    fn validate_config_for_start(&self, _config: &Value) -> Result<(), String> {
        Ok(())
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(DataInstance {
            history: self.history.clone(),
            services: self.services.clone(),
        }))
    }
}

/// What disable really does, stated precisely: /api/db stops answering (the host boundary) and
/// the console history (logs/data.db) closes. Data owns no connection: every browse/query
/// opens through the OWNING MCP adapter's existing pool or child and drops back, so the
/// connections stay with the MCP plugin that owns them, and disabling MCP takes the shared
/// connectivity away from Data with it.
struct DataInstance {
    history: Arc<swiss_data::history::DataHistory>,
    services: Arc<RuntimeServices>,
}

#[async_trait]
impl PluginInstance for DataInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // /api/db takes a REQUEST-SCOPED lease per call and drops it with the request.
        // Starting without a catalog provider is legal — the routes answer an honest 503
        // naming who is missing, and the inventory row says the requirement is unmet (SPEC
        // §host.seats). The history is a record, not a gate: a store that will not open is
        // a warning, and the consoles still run (SPEC §data.history).
        if let Err(err) = self.history.open().await {
            swiss_core::log::warn(&format!("the console history is unavailable: {err}"), None);
        } else {
            self.services
                .module_dbs
                .publish(DATA_ID, self.history.database_file());
            self.services.catalog.sql_history.install(self.history.clone());
        }
        Ok(())
    }

    async fn stop(&self) {
        // Request leases die with their requests, and the boundary dropping /api/db IS their
        // release; leases still held elsewhere are the provider's drain's business. The one
        // thing Data holds is its history, closed bounded here.
        self.services.module_dbs.withdraw(DATA_ID);
        self.services.catalog.sql_history.clear();
        if let Err(err) = self.history.close().await {
            swiss_core::log::warn(&format!("closing the console history: {err:?}"), None);
        }
    }
}

// --- Jobs ---

struct JobsPlugin {
    jobs: Arc<JobSystem>,
}

/// The jobs config row is validated through the v2 definition model (SPEC §jobs.config, §jobs.migrate):
/// this is the server-side authority for every PUT /api/plugins/jobs/config and every
/// plugin start. Errors carry the dotted field path so the panel can point its form at
/// the offender, and action.input is checked against the RESOLVED capability's own
/// schema (S3) - a saved job can never hold an input its first run would reject. An
/// unregistered action type is not an error here; it is a warning on the PUT response
/// and `actionAvailable: false` in the listing, because disabling a provider must not
/// lock its jobs' config.
fn validate_jobs_config(
    config: &Value,
    actions: &swiss_host::services::action::ActionRegistry,
) -> Result<(), String> {
    swiss_jobs::jobs::def::JobsConfig::parse_with_actions(config, actions)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[async_trait]
impl PluginFactory for JobsPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: JOBS_ID.into(),
            kind: "jobs".into(),
            label: "Jobs".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            // Schema-lite metadata (the real authority is validate_config -> the v2
            // model), mirroring it field for field so the panel's form offers exactly what
            // the scheduler runs (SPEC §jobs.config).
            config_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "schemaVersion": {
                        "type": "integer",
                        "enum": [2],
                        "description": "The jobs config row's schema; absent means 2, and no other value is parsed."
                    },
                    "maxConcurrentRuns": {
                        "type": "integer", "minimum": 1, "maximum": 64, "default": 2,
                    },
                    "maxQueuedRuns": {
                        "type": "integer", "minimum": 0, "maximum": 1024, "default": 32,
                    },
                    "retention": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "days": { "type": "integer", "minimum": 1, "maximum": 3650, "default": 180 },
                            "maxBytesPerJob": { "type": "integer", "minimum": 65536, "maximum": 67108864, "default": 2097152 },
                            "maxHistoryBytes": { "type": "integer", "minimum": 1048576, "maximum": 1073741824, "default": 67108864 }
                        }
                    },
                    "definitions": {
                        "type": "object",
                        "maxProperties": 512,
                        "description": "v2 job definitions keyed by stable id (SPEC §jobs.config). This row is the scheduler's source of definitions; editing it applies in place, without restarting the plugin.",
                        "additionalProperties": { "$ref": "#/definitions/jobDefinition" }
                    }
                },
                "definitions": {
                    "jobDefinition": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["trigger", "action"],
                        "properties": {
                            "title": { "type": "string", "maxLength": 200, "description": "Display name; defaults to the id." },
                            "labels": { "type": "array", "maxItems": 16, "items": { "type": "string", "maxLength": 64 } },
                            "disabled": { "type": "boolean", "default": false, "description": "Disabled jobs keep their history and still support Run now." },
                            "trigger": {
                                "oneOf": [
                                    { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "manual" } } },
                                    { "type": "object", "additionalProperties": false, "required": ["kind", "everyMs"], "properties": {
                                        "kind": { "const": "interval" },
                                        "everyMs": { "type": "integer", "minimum": 1000, "maximum": 31536000000u64 },
                                        "firstRun": { "enum": ["after-interval", "immediate"], "default": "after-interval" } } },
                                    { "type": "object", "additionalProperties": false, "required": ["kind", "expression"], "properties": {
                                        "kind": { "const": "cron" },
                                        "expression": { "type": "string", "description": "5-field cron expression, local wall-clock time." },
                                        "timezone": { "const": "local", "default": "local", "description": "Only local is supported." } } }
                                ]
                            },
                            "action": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["type"],
                                "properties": {
                                    "type": { "type": "string", "description": "Capability id; it need not be registered to save (the provider may be disabled)." },
                                    "input": { "type": "object", "description": "Validated by the capability itself; env-var references resolve at run time and never enter the config." },
                                    "schemaVersion": { "type": "integer", "enum": [1], "default": 1 }
                                }
                            },
                            "timeoutMs": { "type": "integer", "minimum": 1000, "maximum": 86400000, "default": 600000, "description": "Deadline per attempt; maxAttempts * (timeoutMs + delayMs) must stay within 24 h." },
                            "overlap": { "enum": ["skip", "queue-one"], "default": "skip", "description": "An occurrence while the previous run is still going: skip it, or queue one successor (SPEC §jobs.triggers)." },
                            "misfire": { "enum": ["skip", "run-once"], "default": "skip", "description": "Occurrences missed while the gateway was down: record them, or run the newest once (SPEC §jobs.triggers)." },
                            "retry": {
                                "type": "object",
                                "additionalProperties": false,
                                "description": "Retry of one occurrence; the default is one attempt, nothing retried (SPEC §jobs.triggers).",
                                "properties": {
                                    "maxAttempts": { "type": "integer", "minimum": 1, "maximum": 10, "default": 1 },
                                    "delayMs": { "type": "integer", "minimum": 0, "maximum": 3600000, "default": 0 },
                                    "backoff": { "enum": ["fixed", "exponential"], "default": "fixed" },
                                    "retryOn": { "type": "array", "items": { "enum": ["failure", "timeout"] }, "default": ["failure"] }
                                }
                            },
                            "output": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "capture": { "enum": ["tail", "none"], "default": "tail", "description": "tail keeps the last maxBytes of output; none keeps no output (SPEC §jobs.triggers)." },
                                    "maxBytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 16384 }
                                }
                            }
                        }
                    }
                }
            }),
            pages: vec![page("jobs", JOBS_ID, "Jobs", 50, false, "page")],
            routes: vec!["/api/jobs".into()],
            // FALSE from S3 (SPEC §jobs.apply): the row is applied in place through
            // [JobsInstance::apply_config]. A restart here would run
            // [JobsInstance::stop] - cancelling and awaiting every in-flight run - on
            // every edit of any job; the in-place apply is the whole point of the
            // third semantics.
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        validate_jobs_config(config, self.jobs.actions())
    }

    fn config_warnings(&self, config: &Value) -> Vec<String> {
        match swiss_jobs::jobs::def::JobsConfig::parse_with_actions(config, self.jobs.actions()) {
            Ok((_, warnings)) => warnings,
            Err(_) => Vec::new(), // a rejected row never reaches the warning path
        }
    }

    /// Boot path: same row, older possible author. A pre-v2 gateway accepted ANY object
    /// under definitions, so its leftovers (`{"x": {}}` placeholders and the like) are
    /// dropped with a warn here instead of failing the plugin - one stale entry must not
    /// take the scheduler down for every job. A PUT of the same body stays a 400 via
    /// [validate_jobs_config]: a human saving it NOW is doing something new, and
    /// accepting it would be the accepted-but-unexecuted lie (SPEC §arch.rules). The
    /// warn itself is asserted at the def.rs unit level (parse_boot reports the drops
    /// that feed it); log output is println and not capturable from integration tests.
    fn validate_config_for_start(&self, config: &Value) -> Result<(), String> {
        match swiss_jobs::jobs::def::JobsConfig::parse_boot_with_actions(
            config,
            self.jobs.actions(),
        ) {
            Ok((boot, _)) => {
                for (id, reason) in boot.dropped {
                    log::warn(
                        "ignoring a config job definition saved by an older validator",
                        Some(json!({ "id": id, "reason": reason })),
                    );
                }
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        }
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        // The v1 table migrates FIRST (SPEC §jobs.migrate): jobs.json rows merge into the
        // config row and their run facts seed the state file, before any apply. A failed
        // step fails this create - the plugin lands failed with the reason, jobs.json
        // stays untouched and unmarked, and the scheduler never ticks over a
        // half-migrated tree.
        swiss_jobs::jobs::migrate::run(&self.jobs)?;
        // Apply the row the STORE holds now - the migration may have merged v1
        // definitions into it, and the `config` argument is the pre-migration
        // snapshot. Still before the tick can start, still cheap by contract.
        let row = self.jobs.current_config();
        let row = if row.as_object().is_some_and(|m| m.is_empty()) {
            config.clone()
        } else {
            row
        };
        self.jobs.apply_config(&row)?;
        Ok(Arc::new(JobsInstance {
            jobs: self.jobs.clone(),
        }))
    }
}

/// What disable really does: the ONE scheduler tick task is aborted, so no new runs fire and
/// the timer's memory is gone, AND every run this scheduler owns is cancelled and awaited
/// through the shared coordinator — child tree killed, pipe readers joined — before stop
/// returns. Since the scheduler became a producer of the shared run service, "stop" can mean
/// what it says.
///
/// What it does not do: touch runs of other producers (a manual /api/runs submission keeps
/// going), or the job table. Definitions, lastRunAt/lastOk and every run log survive; enabling
/// starts the tick again and the next due occurrence fires normally.
struct JobsInstance {
    jobs: Arc<JobSystem>,
}

#[async_trait]
impl PluginInstance for JobsInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        self.jobs.start();
        Ok(())
    }

    async fn stop(&self) {
        let canceled = self.jobs.shutdown().await;
        if canceled > 0 {
            log::log(
                "info",
                "jobs plugin stopped; in-flight runs canceled",
                Some(json!({ "runs": canceled })),
            );
        }
    }

    /// The in-place row apply (SPEC §jobs.apply): definitions, capacity and retention move
    /// under the run; a run already claimed keeps its snapshot to the end. Failure is
    /// reported, never guessed at - the host keeps the instance Active and the
    /// desired-vs-actual gap visible.
    async fn apply_config(&self, config: &Value) -> ApplyOutcome {
        match self.jobs.apply_config(config) {
            Ok(()) => ApplyOutcome::Applied,
            Err(err) => ApplyOutcome::Failed(err),
        }
    }
}

// --- Process ---------------------------------------------------------------
// The Process plugin grew a page, routes and the apps system (SPEC §process.apps):
// its factory moved to the swiss-plugins crate's process.rs beside terminal's and remote's,
// keeping this file to the wrappers of the subsystems the boot built itself.
// It still registers LAST here so it STARTS FIRST (boot walks the reverse):
// a capability provider must be up before anything submits a run against it.

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniquely-named scratch paths under the temp dir: the factory constructors want a path,
    /// and two tests (or two runs) must never share one.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "swiss-builtin-{}-{}-{}",
            tag,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst),
        ))
    }

    /// The two-level navigation (SPEC §panel.nav) shows the PLUGIN label on level one and the PAGE
    /// labels on level two, so a plugin whose label duplicates one of its own pages would
    /// render the same word twice in one bar - "MCPs" over "MCPs", which is exactly what the
    /// MCPs to MCP / MCPs to Servers rename fixed. Should the duplication ever come back, it
    /// comes back as this failure. (Single-page plugins are fine either way: their level-two
    /// location label is the plugin name itself, so the duplication cannot bite there.)
    /// The MCP group's third page (panel: the toolbar token button is gone). Pinned here
    /// because the entry path is a contract with the panel: the shell dynamic-imports
    /// exactly `/admin/js/views/<id>.js`, so a renamed id strands the page. The routes
    /// assertion pins the other half of the design: /api/tokens is HOST-owned and never
    /// joins the plugin's routes, so disabling the MCP plugin takes the PAGE off the air
    /// but leaves the credential API (and the CLI) serving.
    #[test]
    fn mcp_descriptor_contributes_the_token_page_but_keeps_its_routes_host_owned() {
        let dir = scratch_dir("tokens-page");
        let factory = McpPlugin {
            registry: Registry::new(
                60_000,
                Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            ),
            managed: Arc::new(ManagedStore::open_at(dir.join("managed.json"))),
            services: swiss_host::services::RuntimeServices::new(
                swiss_host::services::BootInfo::default(),
            ),
            call_log: Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            traffic: Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
            logs_dir: dir.join("logs"),
        };
        let descriptor = factory.descriptor();
        let tokens = descriptor
            .pages
            .iter()
            .find(|p| p.id == "tokens")
            .expect("the MCP plugin contributes the tokens page");
        assert_eq!(tokens.label, "Token");
        assert_eq!(tokens.entry, "/admin/js/views/tokens.js");
        assert_eq!(tokens.path, "#mcps-tokens");
        assert!(!tokens.sidebar, "the Token page has no sidebar of its own");
        assert!(
            !descriptor.routes.iter().any(|r| r.contains("tokens")),
            "host-owned /api/tokens must not become a plugin route: {:?}",
            descriptor.routes
        );
    }

    #[test]
    fn tunnels_contributes_two_sibling_pages_sharing_one_group() {
        let dir = scratch_dir("tunnels-pages");
        let tunnel_store = Arc::new(std::sync::Mutex::new(
            swiss_tunnels::tunnel::TunnelStore::new(dir.join("tunnels.json"), 19997),
        ));
        let manager = TunnelManager::new(tunnel_store.clone(), None);
        let factory = TunnelsPlugin {
            tunnels: Arc::new(swiss_tunnels::tunnel::api::Tunnels {
                store: tunnel_store,
                manager: manager.clone(),
                mcp_display: None,
            }),
            manager,
            services: swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default()),
        };
        let descriptor = factory.descriptor();
        // The group label stays the plugin's own name; the two PAGE labels name their scope.
        assert_eq!(descriptor.label, "Tunnels");
        let pages: Vec<(&str, &str, i64)> = descriptor
            .pages
            .iter()
            .map(|p| (p.id.as_str(), p.label.as_str(), p.order))
            .collect();
        assert_eq!(
            pages,
            vec![
                ("tunnels", "SSH Connections", 30),
                ("tunnel-forwards", "Port Forwards", 35)
            ],
            "adjacent orders, connections first — the deep link #tunnels keeps its meaning"
        );
        // The entry contract: the shell dynamic-imports exactly this path per page id.
        assert_eq!(
            descriptor.pages[1].entry,
            "/admin/js/views/tunnel-forwards.js"
        );
        assert_eq!(descriptor.pages[1].path, "#tunnel-forwards");
        for page in &descriptor.pages {
            assert_ne!(
                descriptor.label, page.label,
                "level-one duplicates level-two; navigation labels must differ"
            );
        }
    }

    #[test]
    fn mcp_plugin_label_differs_from_every_page_label() {
        let dir = scratch_dir("labels");
        let factory = McpPlugin {
            registry: Registry::new(
                60_000,
                Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            ),
            managed: Arc::new(ManagedStore::open_at(dir.join("managed.json"))),
            services: swiss_host::services::RuntimeServices::new(
                swiss_host::services::BootInfo::default(),
            ),
            call_log: Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            traffic: Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
            logs_dir: dir.join("logs"),
        };
        let descriptor = factory.descriptor();
        assert_eq!(descriptor.id, MCP_ID);
        assert!(
            !descriptor.pages.is_empty(),
            "the MCP plugin is the multi-page case the contract exists for"
        );
        for page in &descriptor.pages {
            assert_ne!(
                descriptor.label, page.label,
                "level-one '{}' duplicates level-two '{}'; navigation labels must differ",
                descriptor.label, page.label,
            );
        }
    }

    /// SPEC §host.history L1, the module half: a plugin that exists but has not started
    /// owns no database. The open happens on start (and a failed open degrades to jsonl),
    /// so a registered-but-idle MCP plugin leaves the home's logs tree exactly as it was
    /// — disabled means zero, not "a little".
    #[test]
    fn a_registered_mcp_plugin_that_never_started_opens_no_history_database() {
        let dir = scratch_dir("idle-no-db");
        let factory = McpPlugin {
            registry: Registry::new(
                60_000,
                Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            ),
            managed: Arc::new(ManagedStore::open_at(dir.join("managed.json"))),
            services: swiss_host::services::RuntimeServices::new(
                swiss_host::services::BootInfo::default(),
            ),
            call_log: Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls"))),
            traffic: Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
            logs_dir: dir.join("logs"),
        };
        let _descriptor = factory.descriptor();
        assert!(
            !dir.join("logs").join("mcp.db").exists(),
            "no start, no database: the module's history file appears only on start",
        );
    }
}
