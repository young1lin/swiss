//! The composition table: the built-in plugins of this build, their descriptors, and the
//! wrappers that bind the EXISTING subsystems (registry, tunnel manager, job system, db
//! browser) to the host lifecycle. Everything business-shaped lives here, not in the host
//! core — adding a plugin is one factory plus one `register` line (docs/09 §9).
//!
//! Every wrapper documents, in its own comments, what disable REALLY does to its subsystem.
//! That is the honesty rule of docs/09 §4: a wrapper may stop short of a full unload, but it
//! must never claim one it does not perform.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::descriptor::{PageDescriptor, PluginDescriptor};
use super::factory::{PluginFactory, PluginInstance};
use super::engine::PluginHost;
use super::scope::PluginScope;
use crate::jobs::JobSystem;
use crate::log;
use crate::managed::ManagedStore;
use crate::registry::{is_lazy, Registry, Source};
use crate::services::actions::{LegacyCommandAction, ProcessExecAction};
use crate::services::RuntimeServices;
use crate::tunnel::manager::TunnelManager;

pub const MCP_ID: &str = "mcp";
pub const TUNNELS_ID: &str = "tunnels";
pub const DATA_ID: &str = "data";
pub const JOBS_ID: &str = "jobs";
pub const PROCESS_ID: &str = "process";

/// One built-in page: id-derived hash path (`#mcps`) and entry (`/admin/js/views/<id>.js`),
/// sidebar only for the primary MCPs view — the built-in view table of this build.
fn page(id: &str, plugin_id: &str, label: &str, order: i64, sidebar: bool) -> PageDescriptor {
    PageDescriptor {
        id: id.to_string(),
        plugin_id: plugin_id.to_string(),
        label: label.to_string(),
        order,
        path: format!("#{id}"),
        entry: format!("/admin/js/views/{id}.js"),
        sidebar,
    }
}

/// Everything the built-in wrappers need from the boot sequence. Shared downstream objects
/// (the registry, stores, the managers) are constructed ONCE by server.rs and wrapped here —
/// the plugins own their RUNTIME state (started/stopped), not the objects' construction.
pub struct BuiltinDeps {
    pub registry: Arc<Registry>,
    pub managed: Arc<ManagedStore>,
    pub jobs: Arc<JobSystem>,
    pub tunnels: Arc<crate::tunnel::api::Tunnels>,
    pub tunnel_manager: Arc<TunnelManager>,
    /// The dbbrowser resolver seam — a closure over the live registry (AppContext's
    /// browser_resolver), handed to the Data plugin so /api/db can keep resolving rows.
    pub browse: crate::dbbrowser_api::BrowseResolver,
    /// The shared action registry, run pool and process supervisor. Capability plugins
    /// register INTO these; the host's /api/actions and /api/runs read them.
    pub services: Arc<RuntimeServices>,
}

/// Register every built-in of this build, in inventory/page order: MCP+Traffic, Tunnels,
/// Data, Jobs. Boot STARTS them in the reverse (see PluginHost::register) so tunnels come up
/// before the MCPs that may ride them.
pub fn register_all(host: &mut PluginHost, deps: &BuiltinDeps) -> Result<(), String> {
    host.register(Arc::new(McpPlugin {
        registry: deps.registry.clone(),
        managed: deps.managed.clone(),
    }))?;
    host.register(Arc::new(TunnelsPlugin {
        tunnels: deps.tunnels.clone(),
        manager: deps.tunnel_manager.clone(),
    }))?;
    host.register(Arc::new(DataPlugin {
        browse: deps.browse.clone(),
    }))?;
    host.register(Arc::new(JobsPlugin {
        jobs: deps.jobs.clone(),
    }))?;
    // Registered LAST so it STARTS FIRST (boot walks the reverse): a capability provider
    // must be registered before anything can submit a run against it.
    host.register(Arc::new(ProcessPlugin {
        services: deps.services.clone(),
    }))
}

// --- MCP + Traffic (one plugin, two views) ---

struct McpPlugin {
    registry: Arc<Registry>,
    managed: Arc<ManagedStore>,
}

#[async_trait]
impl PluginFactory for McpPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: MCP_ID.into(),
            kind: "mcp".into(),
            label: "MCPs".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object", "properties": {} }),
            pages: vec![
                page("mcps", MCP_ID, "MCPs", 10, true),
                page("traffic", MCP_ID, "Traffic", 20, false),
            ],
            routes: vec!["/api/mcps".into(), "/api/traffic".into()],
            // The plugin reads nothing from a config row today; restarting every hosted MCP
            // because a row moved would be destruction disguised as "applying config".
            restart_on_config_change: false,
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(McpInstance {
            registry: self.registry.clone(),
            managed: self.managed.clone(),
        }))
    }
}

/// The MCP+Traffic instance.
///
/// WHAT DISABLE REALLY DOES (the docs/09 §4 honesty rule, applied): stopping this plugin
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
}

#[async_trait]
impl PluginInstance for McpInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        self.registry.start_timer();
        start_hosted_mcps(&self.registry, &self.managed).await;
        Ok(())
    }

    async fn stop(&self) {
        // close_all stops the health/idle timer together with every entry (registry.rs).
        self.registry.close_all().await;
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
    tunnels: Arc<crate::tunnel::api::Tunnels>,
    manager: Arc<TunnelManager>,
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
            pages: vec![page("tunnels", TUNNELS_ID, "Tunnels", 30, false)],
            routes: vec!["/api/tunnels".into()],
            restart_on_config_change: true,
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(TunnelsInstance {
            tunnels: self.tunnels.clone(),
            manager: self.manager.clone(),
        }))
    }
}

/// What disable really does: `close_all` ends every SSH session and every forward — every
/// local port is released — while tunnels.json's enabled flags survive untouched, so the
/// next start brings back exactly the set that was running. The first-run forward-port
/// import also moved INSIDE start: a disabled tunnels plugin no longer touches tunnels.json
/// at boot (server.rs used to do the import before any lifecycle existed).
struct TunnelsInstance {
    tunnels: Arc<crate::tunnel::api::Tunnels>,
    manager: Arc<TunnelManager>,
}

#[async_trait]
impl PluginInstance for TunnelsInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        if let Ok(mut store) = self.tunnels.store.lock() {
            if store.is_fresh() {
                if let Some((rules, connections)) =
                    crate::tunnel::import::import_forward_port(&mut store, None)
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
        Ok(())
    }

    async fn stop(&self) {
        self.manager.close_all().await;
    }
}

// --- Data ---

struct DataPlugin {
    /// Kept on the factory so the resolver's lifetime is visibly bound to this plugin; the
    /// /api/db router itself is mounted by build_app and gated by the host boundary.
    #[allow(dead_code)]
    browse: crate::dbbrowser_api::BrowseResolver,
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
            pages: vec![page("data", DATA_ID, "Data", 40, false)],
            routes: vec!["/api/db".into()],
            restart_on_config_change: false,
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(DataInstance {
            browse: self.browse.clone(),
        }))
    }
}

/// What disable really does, stated precisely: /api/db stops answering (the host boundary),
/// which is this plugin's entire runtime footprint — because Data OWNS NO EAGER RESOURCES.
/// Every browse/query opens through the OWNING MCP adapter's existing pool or child and
/// drops back; the connections stay with the MCP plugin that owns them. That sharing is
/// exactly what docs/09 §3's ConnectionCatalog work (P4) will turn into explicit leases;
/// until then this wrapper does NOT claim independent Data resources, and disabling MCP
/// takes the shared connectivity away from Data with it.
struct DataInstance {
    #[allow(dead_code)]
    browse: crate::dbbrowser_api::BrowseResolver,
}

#[async_trait]
impl PluginInstance for DataInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // Nothing eager to start: the routes are mounted, the resolver reads the live
        // registry per request. Honest no-op — see the type's comment.
        Ok(())
    }

    async fn stop(&self) {
        // Nothing retained to release — the boundary dropping /api/db IS the release.
    }
}

// --- Jobs ---

struct JobsPlugin {
    jobs: Arc<JobSystem>,
}

/// Config-side definitions, as far as this build takes them: an optional object of job
/// definitions under `definitions`, each an object. (The scheduler itself reads jobs.json;
/// consuming config-side definitions is the config-driven jobs work's seam — see the
/// JobsInstance comment.)
fn validate_jobs_config(config: &Value) -> Result<(), String> {
    let Some(map) = config.get("definitions").and_then(Value::as_object) else {
        return Ok(());
    };
    for (name, def) in map {
        if !def.is_object() {
            return Err(format!("definitions.{name} must be an object"));
        }
    }
    Ok(())
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
            config_schema: json!({
                "type": "object",
                "properties": {
                    "definitions": {
                        "type": "object",
                        "description": "Optional config-side job definitions (config-driven jobs work); the scheduler reads jobs.json today.",
                    },
                },
            }),
            pages: vec![page("jobs", JOBS_ID, "Jobs", 50, false)],
            routes: vec!["/api/jobs".into()],
            restart_on_config_change: true,
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        validate_jobs_config(config)
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
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
}

// --- Process: a capability plugin with no page of its own ---

/// The capabilities this plugin contributes. Withdrawn again by its stop, which is also
/// what scopes the cancellation of runs already executing them.
const PROCESS_ACTIONS: &[&str] = &["process.exec", "process.legacy-command"];

struct ProcessPlugin {
    services: Arc<RuntimeServices>,
}

#[async_trait]
impl PluginFactory for ProcessPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PROCESS_ID.into(),
            kind: "process".into(),
            label: "Process".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({ "type": "object", "properties": {} }),
            // No page and no routes of its own (docs/09 §3): a capability plugin is reached
            // through the host's /api/actions and /api/runs, and adding a navigation tab for
            // it would be a page nobody asked for.
            pages: vec![],
            routes: vec![],
            restart_on_config_change: false,
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(ProcessInstance {
            services: self.services.clone(),
        }))
    }
}

/// What disable really does: the two capabilities leave the registry, so a new submission
/// (manual or scheduled) fails with "unknown action" instead of silently doing nothing, and
/// every run ALREADY executing one of them is cancelled and AWAITED — child tree killed,
/// pipe readers joined — before stop returns.
///
/// What it does not do: touch runs of other capabilities, or the producers themselves. A
/// disabled process plugin leaves the Jobs scheduler ticking; its process-backed jobs report
/// the missing capability, which is exactly the dependency-unavailable outcome docs/09 §9
/// asks for.
struct ProcessInstance {
    services: Arc<RuntimeServices>,
}

#[async_trait]
impl PluginInstance for ProcessInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        let supervisor = self.services.supervisor.clone();
        self.services
            .actions
            .register(Arc::new(ProcessExecAction::new(supervisor.clone())))?;
        // A half-registered provider is a broken one: undo the first registration rather
        // than leave a plugin reporting active with one of its two capabilities missing.
        if let Err(err) = self
            .services
            .actions
            .register(Arc::new(LegacyCommandAction::new(supervisor)))
        {
            self.services.actions.unregister("process.exec");
            return Err(err);
        }
        Ok(())
    }

    async fn stop(&self) {
        for name in PROCESS_ACTIONS {
            self.services.actions.unregister(name);
        }
        // Withdraw FIRST, then cancel: nothing can start a new run of a capability that is
        // no longer resolvable while the in-flight ones are being reaped.
        let canceled = self.services.runs.cancel_action_types(PROCESS_ACTIONS).await;
        if canceled > 0 {
            log::log(
                "info",
                "process plugin stopped; in-flight runs canceled",
                Some(json!({ "runs": canceled })),
            );
        }
    }
}
