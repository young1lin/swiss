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

use lmg_core::log;
use lmg_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use lmg_host::host::engine::PluginHost;
use lmg_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use lmg_host::host::scope::PluginScope;
use lmg_host::managed::ManagedStore;
use lmg_host::services::actions::{LegacyCommandAction, ProcessExecAction};
use lmg_host::services::RuntimeServices;
use lmg_jobs::jobs::JobSystem;
use lmg_mcp::registry::{is_lazy, Registry, Source};
use lmg_tunnels::tunnel::manager::TunnelManager;

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
    pub tunnels: Arc<lmg_tunnels::tunnel::api::Tunnels>,
    pub tunnel_manager: Arc<TunnelManager>,
    /// The shared action registry, run pool, process supervisor and connection catalog.
    /// Capability plugins register INTO these; the host's /api/actions, /api/runs and
    /// the Data plugin's leases read them.
    pub services: Arc<RuntimeServices>,
}

/// Register every built-in of this build, in inventory/page order: MCP+Traffic, Tunnels,
/// Data, Jobs. Boot STARTS them in the reverse (see PluginHost::register) so tunnels come up
/// before the MCPs that may ride them.
pub fn register_all(host: &mut PluginHost, deps: &BuiltinDeps) -> Result<(), String> {
    host.register(Arc::new(McpPlugin {
        registry: deps.registry.clone(),
        managed: deps.managed.clone(),
        services: deps.services.clone(),
    }))?;
    host.register(Arc::new(TunnelsPlugin {
        tunnels: deps.tunnels.clone(),
        manager: deps.tunnel_manager.clone(),
    }))?;
    host.register(Arc::new(DataPlugin))?;
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
    /// The connection catalog lives here (docs/12 W3): MCP is the one provider, Data the
    /// one consumer, and neither reaches into the other's objects anymore.
    services: Arc<RuntimeServices>,
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
                page("mcps", MCP_ID, "Servers", 10, true),
                page("traffic", MCP_ID, "Traffic", 20, false),
            ],
            routes: vec!["/api/mcps".into(), "/api/traffic".into()],
            // The plugin reads nothing from a config row today; restarting every hosted MCP
            // because a row moved would be destruction disguised as "applying config".
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(McpInstance {
            registry: self.registry.clone(),
            managed: self.managed.clone(),
            services: self.services.clone(),
            tracker: lmg_host::services::catalog::LeaseTracker::new(),
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
    services: Arc<RuntimeServices>,
    /// The lease ledger backing the catalog this instance registers. One per instance:
    /// a stopped instance drains ITS leases, never the next one's.
    tracker: Arc<lmg_host::services::catalog::LeaseTracker>,
}

#[async_trait]
impl PluginInstance for McpInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        self.registry.start_timer();
        start_hosted_mcps(&self.registry, &self.managed).await;
        // Register the connection catalog LAST, so a failed start never leaves a catalog
        // behind that a stopped registry is supposedly serving (docs/12 W3).
        self.services
            .catalog
            .register(
                Arc::new(lmg_mcp::registry::RegistryCatalog::new(
                    self.registry.clone(),
                    self.tracker.clone(),
                )),
                MCP_ID,
            )
            .map_err(|err| format!("connection catalog registration: {err}"))?;
        Ok(())
    }

    async fn stop(&self) {
        // The W3 order (docs/12): withdraw first (no NEW leases reach a pool that is about
        // to close), drain what is in flight with an honest warning when the wait was not
        // clean, close the pools, and only then free the catalog seat for the next start.
        self.services.catalog.begin_withdraw();
        lmg_host::services::catalog::drain_leases(
            &self.tracker,
            std::time::Duration::from_millis(lmg_host::services::catalog::DRAIN_TIMEOUT_MS),
            MCP_ID,
        );
        // close_all stops the health/idle timer together with every entry (registry.rs).
        self.registry.close_all().await;
        self.services.catalog.clear();
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
    tunnels: Arc<lmg_tunnels::tunnel::api::Tunnels>,
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
            requires: Vec::new(),
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
    tunnels: Arc<lmg_tunnels::tunnel::api::Tunnels>,
    manager: Arc<TunnelManager>,
}

#[async_trait]
impl PluginInstance for TunnelsInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        if let Ok(mut store) = self.tunnels.store.lock() {
            if store.is_fresh() {
                if let Some((rules, connections)) =
                    lmg_tunnels::tunnel::import::import_forward_port(&mut store, None)
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

/// No fields anymore: the resolver seam is gone, replaced by the connection catalog in
/// RuntimeServices (docs/12 W3). Data LEASES what it browses; it no longer holds a
/// closure into the MCP registry.
struct DataPlugin;

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
            // The honest dependency, stated as data (docs/12 W3): Data browses through the
            // connection catalog the MCP plugin provides. The inventory carries this with
            // a met/unmet verdict, so "disable MCP" no longer silently paralyses Data.
            requires: vec!["connection-catalog".into()],
        }
    }

    async fn create(&self, _config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(DataInstance))
    }
}

/// What disable really does, stated precisely: /api/db stops answering (the host boundary),
/// which is this plugin's entire runtime footprint — because Data OWNS NO EAGER RESOURCES.
/// Every browse/query opens through the OWNING MCP adapter's existing pool or child and
/// drops back; the connections stay with the MCP plugin that owns them. That sharing is
/// exactly what docs/09 §3's ConnectionCatalog work (P4) will turn into explicit leases;
/// until then this wrapper does NOT claim independent Data resources, and disabling MCP
/// takes the shared connectivity away from Data with it.
struct DataInstance;

#[async_trait]
impl PluginInstance for DataInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // Nothing eager to start: /api/db takes a REQUEST-SCOPED lease per call and drops
        // it with the request. Starting without a catalog provider is legal — the routes
        // answer an honest 503 naming who is missing, and the inventory row says the
        // requirement is unmet (docs/12 W3).
        Ok(())
    }

    async fn stop(&self) {
        // Nothing retained to release: request leases die with their requests, and the
        // boundary dropping /api/db IS the release. Leases still held elsewhere are the
        // provider's drain's business — never a cross-plugin teardown.
    }
}

// --- Jobs ---

struct JobsPlugin {
    jobs: Arc<JobSystem>,
}

/// The jobs config row is validated through the v2 definition model (docs/11 §3/§9):
/// this is the server-side authority for every PUT /api/plugins/jobs/config and every
/// plugin start. Errors carry the dotted field path so the panel can point its form at
/// the offender, and action.input is checked against the RESOLVED capability's own
/// schema (S3) - a saved job can never hold an input its first run would reject. An
/// unregistered action type is not an error here; it is a warning on the PUT response
/// and `actionAvailable: false` in the listing, because disabling a provider must not
/// lock its jobs' config.
fn validate_jobs_config(
    config: &Value,
    actions: &lmg_host::services::action::ActionRegistry,
) -> Result<(), String> {
    lmg_jobs::jobs::def::JobsConfig::parse_with_actions(config, actions)
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
            // model). The definitions note is the honesty rule: this build validates and
            // stores definitions but does not schedule from them yet, and the four
            // not-yet-implemented semantics say so in place (docs/11 §9 S1).
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
                        "description": "v2 job definitions keyed by stable id (docs/11 §3). This row is the scheduler's source of definitions; editing it applies in place, without restarting the plugin.",
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
                            "overlap": { "enum": ["skip"], "description": "queue-one is not implemented until S5 (docs/11 §2 rule 5)." },
                            "misfire": { "enum": ["skip"], "description": "run-once is not implemented until S5 (docs/11 §2 rule 5)." },
                            "retry": {
                                "type": "object",
                                "additionalProperties": false,
                                "description": "Retry is not implemented until S5; only the default (no automatic retry) is accepted.",
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
                                    "capture": { "enum": ["tail"], "default": "tail", "description": "none is not implemented until S5 (docs/11 §2 rule 5)." },
                                    "maxBytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 16384 }
                                }
                            }
                        }
                    }
                }
            }),
            pages: vec![page("jobs", JOBS_ID, "Jobs", 50, false)],
            routes: vec!["/api/jobs".into()],
            // FALSE from S3 (docs/11 §8): the row is applied in place through
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
        match lmg_jobs::jobs::def::JobsConfig::parse_with_actions(config, self.jobs.actions()) {
            Ok((_, warnings)) => warnings,
            Err(_) => Vec::new(), // a rejected row never reaches the warning path
        }
    }

    /// Boot path: same row, older possible author. A pre-v2 gateway accepted ANY object
    /// under definitions, so its leftovers (`{"x": {}}` placeholders and the like) are
    /// dropped with a warn here instead of failing the plugin - one stale entry must not
    /// take the scheduler down for every job. A PUT of the same body stays a 400 via
    /// [validate_jobs_config]: a human saving it NOW is doing something new, and
    /// accepting it would be the accepted-but-unexecuted lie (docs/11 §2 rule 5). The
    /// warn itself is asserted at the def.rs unit level (parse_boot reports the drops
    /// that feed it); log output is println and not capturable from integration tests.
    fn validate_config_for_start(&self, config: &Value) -> Result<(), String> {
        match lmg_jobs::jobs::def::JobsConfig::parse_boot_with_actions(config, self.jobs.actions())
        {
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
        // The v1 table migrates FIRST (docs/11 §5.1): jobs.json rows merge into the
        // config row and their run facts seed the state file, before any apply. A failed
        // step fails this create - the plugin lands failed with the reason, jobs.json
        // stays untouched and unmarked, and the scheduler never ticks over a
        // half-migrated tree.
        lmg_jobs::jobs::migrate::run(&self.jobs)?;
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

    /// The in-place row apply (docs/11 §8): definitions, capacity and retention move
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
            requires: Vec::new(),
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
        let canceled = self
            .services
            .runs
            .cancel_action_types(PROCESS_ACTIONS)
            .await;
        if canceled > 0 {
            log::log(
                "info",
                "process plugin stopped; in-flight runs canceled",
                Some(json!({ "runs": canceled })),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniquely-named scratch paths under the temp dir: the factory constructors want a path,
    /// and two tests (or two runs) must never share one.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "lmg-builtin-{}-{}-{}",
            tag,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst),
        ))
    }

    /// The two-level navigation (docs/13 N4) shows the PLUGIN label on level one and the PAGE
    /// labels on level two, so a plugin whose label duplicates one of its own pages would
    /// render the same word twice in one bar - "MCPs" over "MCPs", which is exactly what the
    /// MCPs to MCP / MCPs to Servers rename fixed. Should the duplication ever come back, it
    /// comes back as this failure. (Single-page plugins are fine either way: their level-two
    /// bar never renders, so this pins the contract only where it bites.)
    #[test]
    fn mcp_plugin_label_differs_from_every_page_label() {
        let dir = scratch_dir("labels");
        let factory = McpPlugin {
            registry: Registry::new(
                60_000,
                Arc::new(lmg_mcp::calls::CallLog::at(dir.join("calls"))),
            ),
            managed: Arc::new(ManagedStore::open_at(dir.join("managed.json"))),
            services: lmg_host::services::RuntimeServices::new(),
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
}
