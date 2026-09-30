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

//! The host engine: registration, the lifecycle state machine, serialized start/stop with
//! single-flight starts, scoped cleanup, and the inventory. Deliberately free of business
//! branches — which plugins exist and what their instances do is `builtin.rs`'s composition
//! table (SPEC §host.plugins: "separate orchestration ... rather than put every business branch into
//! core host").
//!
//! Concurrency shape mirrors registry.rs: ONE tokio mutex per plugin serializes its lifecycle
//! ops (`op`); mutable metadata sits behind short std locks never held across an await; the
//! route-visible instance slot flips under a write lock for the length of an assignment.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::descriptor::{PluginDescriptor, PluginState};
use super::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use super::scope::PluginScope;
use crate::config_store::ConfigStore;

/// Cap on a single start. A wedged factory must not hold the plugin's op queue (and its
/// enable/disable calls) forever; the timeout lands the plugin in `failed` with the reason.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on a single stop. After it, the scope is torn down regardless — stop is guaranteed to
/// terminate, with an honest `lastError` when the instance made that happen by force.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// One registered plugin: its frozen descriptor, its factory, and the runtime slots. Shared
/// as one Arc between the host, the API routes and the boundary.
pub struct PluginEntry {
    descriptor: PluginDescriptor,
    factory: Arc<dyn PluginFactory>,
    state: Mutex<PluginState>,
    last_error: Mutex<Option<String>>,
    /// The config revision the CURRENT instance was started with — "actual" next to the
    /// store's desired revision (SPEC §host.lifecycle). 0 until the first successful start.
    config_revision: AtomicU64,
    /// Serializes this plugin's lifecycle operations (the registry's per-entry `op` queue,
    /// one per plugin).
    op: tokio::sync::Mutex<()>,
    /// The last IN-PLACE config apply's failure, if it failed (SPEC §jobs.apply): Some(err) means
    /// a row is persisted that the running instance has not taken - the visible
    /// desired-vs-actual gap. Cleared by the apply that succeeds (or the restart that
    /// supersedes it).
    apply_error: Mutex<Option<String>>,
    /// The live instance. `None` is the disabled/stopped state — the boundary and the client
    /// guards read presence here, so a stopped plugin is not parked behind a flag: the
    /// instance is GONE.
    slot: RwLock<Option<Arc<dyn PluginInstance>>>,
    /// The scope of the running instance; taken and shut down by stop.
    scope: Mutex<Option<PluginScope>>,
}

impl PluginEntry {
    pub fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    pub fn state(&self) -> PluginState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(PluginState::Failed)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }

    pub fn config_revision(&self) -> u64 {
        self.config_revision.load(Ordering::SeqCst)
    }

    /// The factory, for config validation ahead of a PUT.
    pub fn factory(&self) -> Arc<dyn PluginFactory> {
        self.factory.clone()
    }

    /// The failure of the last in-place config apply, if any (SPEC §jobs.apply): what the PUT
    /// response reports as `applied: false` and the inventory keeps as the gap between
    /// the persisted row and the running instance.
    pub fn apply_error(&self) -> Option<String> {
        self.apply_error.lock().ok().and_then(|e| e.clone())
    }

    fn set_apply_error(&self, err: Option<String>) {
        if let Ok(mut slot) = self.apply_error.lock() {
            *slot = err;
        }
    }

    fn set_state(&self, state: PluginState) {
        if let Ok(mut s) = self.state.lock() {
            *s = state;
        }
    }

    fn set_error(&self, err: Option<String>) {
        if let Ok(mut e) = self.last_error.lock() {
            *e = err;
        }
    }

    fn record_failure(&self, err: String) {
        self.set_state(PluginState::Failed);
        self.set_error(Some(err.clone()));
        swiss_core::log::error(
            "plugin start failed",
            Some(json!({ "plugin": self.descriptor.id, "err": err })),
        );
    }
}

/// How the inventory answers "is capability X provided right now?" — one closure over
/// the shared services (the connection catalog today).
pub type CapabilityProbe = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct PluginHost {
    store: Arc<ConfigStore>,
    plugins: RwLock<Vec<Arc<PluginEntry>>>,
    /// Set once by the composition root; absent means no capability is provided.
    capability_probe: RwLock<Option<CapabilityProbe>>,
}

impl PluginHost {
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self {
            store,
            plugins: RwLock::new(Vec::new()),
            capability_probe: RwLock::new(None),
        }
    }

    /// Install the capability probe the inventory's `requiresMet` answers through
    /// (SPEC §host.seats). Call once, at composition, before the first inventory read.
    pub fn set_capability_probe(&self, probe: CapabilityProbe) {
        *self
            .capability_probe
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(probe);
    }

    fn capability_met(&self, name: &str) -> bool {
        self.capability_probe
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|probe| probe(name))
            .unwrap_or(false)
    }

    /// Register one factory, in composition order. Registration order IS the inventory and
    /// page order; boot starts in the REVERSE so low-level connectivity comes up before the
    /// plugins that may ride it (tunnels before the MCP registry — the boot order server.rs
    /// used to hand-roll). Id and route-prefix conflicts are refused here, loudly, because a
    /// silent shadowed route would be a boundary hole (SPEC §panel.nav: conflicts are refused at
    /// registration).
    pub fn register(&mut self, factory: Arc<dyn PluginFactory>) -> Result<(), String> {
        let descriptor = factory.descriptor();
        {
            let plugins = self
                .plugins
                .read()
                .map_err(|_| "plugin registry poisoned".to_string())?;
            if let Some(first) = plugins.iter().find(|p| p.descriptor().id == descriptor.id) {
                return Err(format!(
                    "plugin id already registered: {} (kind {})",
                    descriptor.id,
                    first.descriptor().kind
                ));
            }
            for owned in &descriptor.routes {
                if let Some(reserved) = super::api::reserved_routes()
                    .find(|host_route| routes_overlap(host_route, owned))
                {
                    return Err(format!(
                        "route prefix {owned} of plugin {} is reserved by the host ({reserved})",
                        descriptor.id
                    ));
                }
                if let Some(first) = plugins.iter().find(|p| {
                    p.descriptor()
                        .routes
                        .iter()
                        .any(|other| routes_overlap(other, owned))
                }) {
                    return Err(format!(
                        "route prefix {owned} of plugin {} conflicts with plugin {}",
                        descriptor.id,
                        first.descriptor().id
                    ));
                }
            }
        }
        let entry = Arc::new(PluginEntry {
            descriptor,
            factory,
            state: Mutex::new(PluginState::Disabled),
            last_error: Mutex::new(None),
            config_revision: AtomicU64::new(0),
            op: tokio::sync::Mutex::new(()),
            apply_error: Mutex::new(None),
            slot: RwLock::new(None),
            scope: Mutex::new(None),
        });
        if let Ok(mut plugins) = self.plugins.write() {
            plugins.push(entry);
        }
        Ok(())
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<PluginEntry>> {
        self.plugins
            .read()
            .ok()
            .and_then(|p| p.iter().find(|e| e.descriptor().id == id).cloned())
    }

    pub fn plugins(&self) -> Vec<Arc<PluginEntry>> {
        self.plugins
            .read()
            .map(|p| p.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn store(&self) -> &Arc<ConfigStore> {
        &self.store
    }

    /// The store's current (desired) revision — the top-level `revision` of the inventory.
    pub fn revision(&self) -> u64 {
        self.store.snapshot().revision
    }

    /// Boot: bring every non-disabled plugin up, isolating failures. A plugin that fails to
    /// start is recorded (`failed` + `lastError`) and the boot CONTINUES — the host itself
    /// failing is the only thing allowed to stop the gateway (SPEC §host.lifecycle).
    pub async fn start_enabled(self: &Arc<Self>) {
        for entry in self.plugins().into_iter().rev() {
            let id = entry.descriptor().id.clone();
            if self.store.plugin_disabled(&id) {
                entry.set_state(PluginState::Disabled);
                swiss_core::log::log(
                    "info",
                    "plugin disabled",
                    Some(json!({
                        "plugin": id,
                        "row": format!("{{\"{id}\":{{\"disabled\":true}}}}"),
                        "note": "state files untouched; POST /api/plugins/{id}/enable to re-enable",
                    })),
                );
                continue;
            }
            if let Err(err) = self.reconcile(&id).await {
                // Already recorded on the row and logged; the loop moves on.
                let _ = err;
            }
        }
    }

    /// Bring one plugin to its desired state (the store row), serialized per plugin.
    /// Concurrent callers queue on the op mutex and re-read the world when their turn comes:
    /// two simultaneous starts of a stopped plugin run `create` ONCE (single-flight), and a
    /// disable racing an enable lands in whichever order the queue admits, then re-checks.
    pub async fn reconcile(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let entry = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin: {id}"))?;
        let _guard = entry.op.lock().await;
        self.reconcile_locked(&entry).await
    }

    async fn reconcile_locked(self: &Arc<Self>, entry: &Arc<PluginEntry>) -> Result<(), String> {
        let id = entry.descriptor().id.clone();
        let desired_enabled = !self.store.plugin_disabled(&id);
        let running = entry.slot.read().map(|s| s.is_some()).unwrap_or(false);
        match (desired_enabled, running) {
            (false, false) => {
                entry.set_state(PluginState::Disabled);
                Ok(())
            }
            (false, true) => self.stop_instance(entry).await,
            (true, false) => self.start_instance(entry).await,
            (true, true) => {
                let current = self.store.snapshot().revision;
                if entry.config_revision() != current {
                    // The running instance gets FIRST claim on the new row (SPEC §jobs.apply):
                    // an in-place apply never bounces the plugin - which for Jobs would
                    // mean cancelling every run it owns mid-flight.
                    let instance = entry.slot.read().ok().and_then(|s| s.clone());
                    if let Some(instance) = instance {
                        let config = self.store.plugin_config(&id);
                        match instance.apply_config(&config).await {
                            ApplyOutcome::Applied => {
                                entry.set_apply_error(None);
                                entry.config_revision.store(current, Ordering::SeqCst);
                                return Ok(());
                            }
                            ApplyOutcome::Failed(err) => {
                                // The instance stays Active and the revision does NOT move:
                                // desired and actual stay visibly apart until a later
                                // reconcile succeeds (SPEC §jobs.config). reconcile itself is not
                                // an error - the row IS persisted; the PUT body says
                                // `applied: false` and the inventory carries the reason.
                                entry.set_apply_error(Some(err));
                                entry.set_error(Some(
                                    "config apply failed; the previous config is still running"
                                        .to_string(),
                                ));
                                return Ok(());
                            }
                            ApplyOutcome::NotApplicable => {}
                        }
                    }
                    if entry.descriptor().restart_on_config_change {
                        // Config moved under a running instance that declares it restartable:
                        // stop, then start again on the new config (SPEC §host.lifecycle — apply vs
                        // restart is an explicit per-plugin decision, never a universal hot
                        // reload).
                        self.stop_instance(entry).await?;
                        self.start_instance(entry).await
                    } else {
                        // Nothing to bounce: note that the instance now corresponds to the
                        // current config (whatever the plugin does with it on its next start).
                        entry.set_apply_error(None);
                        entry.config_revision.store(current, Ordering::SeqCst);
                        Ok(())
                    }
                } else {
                    // Already at the current revision - nothing to apply or note.
                    Ok(())
                }
            }
        }
    }

    async fn start_instance(self: &Arc<Self>, entry: &Arc<PluginEntry>) -> Result<(), String> {
        let id = entry.descriptor().id.clone();
        let revision = self.store.snapshot().revision;
        let config = self.store.plugin_config(&id);
        // Validate BEFORE create: a bad row (hand-edited while disabled, say) must fail the
        // plugin, not half-construct an instance. The start-path hook, not the PUT one:
        // a plugin may need to drop-and-warn entries an older validator left behind.
        if let Err(err) = entry.factory.validate_config_for_start(&config) {
            let err = format!("invalid config: {err}");
            entry.record_failure(err.clone());
            return Err(err);
        }
        entry.set_state(PluginState::Starting);
        entry.set_error(None);
        let instance = match entry.factory.create(&config).await {
            Ok(instance) => instance,
            Err(err) => {
                entry.record_failure(err.clone());
                return Err(err);
            }
        };
        let mut scope = PluginScope::new(&id);
        match tokio::time::timeout(START_TIMEOUT, instance.clone().start(&mut scope)).await {
            Ok(Ok(())) => {
                if let Ok(mut slot) = entry.slot.write() {
                    *slot = Some(instance);
                }
                if let Ok(mut s) = entry.scope.lock() {
                    *s = Some(scope);
                }
                entry.config_revision.store(revision, Ordering::SeqCst);
                entry.set_state(PluginState::Active);
                swiss_core::log::log("info", "plugin started", Some(json!({ "plugin": id })));
                Ok(())
            }
            Ok(Err(err)) => {
                scope.shutdown().await;
                entry.record_failure(err.clone());
                Err(err)
            }
            Err(_) => {
                let err = format!(
                    "{id} start timed out after {} ms",
                    START_TIMEOUT.as_millis()
                );
                scope.shutdown().await;
                entry.record_failure(err.clone());
                Err(err)
            }
        }
    }

    /// Stop one plugin: out of the route slot FIRST (the entrance closes before anything
    /// else — SPEC §host.lifecycle's stop order), then the instance's own release under a timeout,
    /// then the scoped tasks. Desired state stays whatever the store row says.
    async fn stop_instance(self: &Arc<Self>, entry: &Arc<PluginEntry>) -> Result<(), String> {
        let id = entry.descriptor().id.clone();
        entry.set_state(PluginState::Stopping);
        let instance = entry.slot.write().ok().and_then(|mut s| s.take());
        if let Some(instance) = instance {
            if tokio::time::timeout(STOP_TIMEOUT, instance.stop())
                .await
                .is_err()
            {
                entry.set_error(Some(format!(
                    "{id} stop timed out after {} ms; its scope was torn down anyway",
                    STOP_TIMEOUT.as_millis()
                )));
                swiss_core::log::warn(
                    "plugin stop timed out — tearing its scope down anyway",
                    Some(json!({ "plugin": id })),
                );
            }
        }
        // Take the scope out from under the std lock BEFORE awaiting its shutdown: a guard
        // held across an await would make every caller's future non-Send.
        let scope = entry.scope.lock().ok().and_then(|mut slot| slot.take());
        if let Some(scope) = scope {
            scope.shutdown().await;
        }
        entry.set_state(PluginState::Disabled);
        swiss_core::log::log("info", "plugin stopped", Some(json!({ "plugin": id })));
        Ok(())
    }

    /// Shutdown: stop every serving plugin in reverse registration order (tunnels close
    /// before the MCP registry riding them — the boot sequence's documented teardown order).
    /// Used by the graceful sequence; idempotent alongside the registry's own close_all.
    pub async fn shutdown_all(self: &Arc<Self>) {
        for entry in self.plugins().into_iter().rev() {
            let _guard = entry.op.lock().await;
            let running = entry.slot.read().map(|s| s.is_some()).unwrap_or(false);
            if running {
                let _ = self.stop_instance(&entry).await;
            } else {
                entry.set_state(PluginState::Disabled);
            }
        }
    }

    /// The inventory: current revision, one row per plugin in registration order, then every
    /// contributed page in registration order (SPEC §panel.nav — the shell renders this list and
    /// never learns a plugin by name).
    pub fn inventory(&self) -> Value {
        let entries = self.plugins();
        let plugins: Vec<Value> = entries.iter().map(|e| self.row(e)).collect();
        let pages: Vec<Value> = entries
            .iter()
            .flat_map(|e| e.descriptor().pages.iter().map(|p| p.to_json()))
            .collect();
        json!({
            "revision": self.revision(),
            "plugins": plugins,
            "pages": pages,
        })
    }

    /// One plugin's inventory row. `lastError` is present only when there is one —
    /// absent-not-null, like every other admin shape here.
    pub fn row(&self, entry: &Arc<PluginEntry>) -> Value {
        let descriptor = entry.descriptor();
        let mut row = json!({
            "id": descriptor.id,
            "kind": descriptor.kind,
            // The human name and the build's version of it: the management page is the one
            // place a person reads this list, and an id is not a label.
            "label": descriptor.label,
            "version": descriptor.version,
            "enabled": !self.store.plugin_disabled(&descriptor.id),
            "state": entry.state().as_str(),
            "configRevision": entry.config_revision(),
            "pages": descriptor
                .pages
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
        });
        if let (Some(obj), Some(err)) = (row.as_object_mut(), entry.last_error()) {
            obj.insert("lastError".into(), json!(err));
        }
        // Capabilities this plugin needs (SPEC §host.seats): stated with a met/unmet verdict,
        // absent-not-null when the plugin needs nothing — most do not.
        if !descriptor.requires.is_empty() {
            let requires = descriptor.requires.clone();
            let met = requires.iter().all(|cap| self.capability_met(cap));
            if let Some(obj) = row.as_object_mut() {
                obj.insert("requires".into(), json!(requires));
                obj.insert("requiresMet".into(), json!(met));
            }
        }
        row
    }

    /// Which plugin owns this request path, if any — longest owned prefix wins, so
    /// `/api/jobs/x/run` guards as jobs even though `/api/jobs` also matches. `None` means
    /// no registered plugin claims the path (host-owned routes: /api/plugins, /health, ...).
    pub fn route_owner(&self, path: &str) -> Option<String> {
        let mut best: Option<(usize, String)> = None;
        for entry in self.plugins() {
            for prefix in &entry.descriptor().routes {
                if path_matches(path, prefix)
                    && best.as_ref().is_none_or(|(len, _)| prefix.len() > *len)
                {
                    best = Some((prefix.len(), entry.descriptor().id.clone()));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Whether a plugin is serving right now — the one question the boundary and the client
    /// guards ask per request.
    pub fn is_active(&self, id: &str) -> bool {
        self.plugin(id)
            .map(|p| p.state() == PluginState::Active)
            .unwrap_or(false)
    }
}

/// A path is owned by a prefix when it equals it or extends it at a segment boundary:
/// `/api/jobs` owns `/api/jobs` and `/api/jobs/x`, not `/api/jobsx`.
fn path_matches(path: &str, prefix: &str) -> bool {
    if !path.starts_with(prefix) {
        return false;
    }
    path.len() == prefix.len()
        || prefix.ends_with('/')
        || path.as_bytes().get(prefix.len()) == Some(&b'/')
}

/// Two route prefixes overlap when either is a prefix of the other at a segment boundary —
/// whichever plugin mounted first would shadow the other's requests.
fn routes_overlap(a: &str, b: &str) -> bool {
    path_matches(a, b) || path_matches(b, a)
}
