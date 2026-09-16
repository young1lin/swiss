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

//! Owns every hosted MCP — port of `registry.ts`: registration, lifecycle
//! (start/stop/restart/rename/delete) and a periodic health probe. The single source of truth
//! the router and admin API read from.
//!
//! Concurrency shape: the Node build serialized each entry's lifecycle ops on a promise chain
//! (`op`); here it is a per-entry tokio mutex held for the whole operation. The mutable entry
//! state sits behind a short std lock that is NEVER held across an await — snapshot out, await,
//! write back. `gen` is the staleness fence for health probes, and the tombstone flag is set
//! before the stop is awaited so a queued start cannot build on a deleted entry.
//!
//! One deliberate deviation: the Node build's per-handler notifier (tool/resource toggle fan-out
//! to `subscriptions/listen` streams) has no counterpart under rmcp's stateless session mode —
//! toggles take effect on the client's next list, and the panel re-polls. See docs/06.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};

use crate::adapters::{Adapter, McpEndpoint};
use crate::paging::PageCache;
use swiss_core::log;
use swiss_host::config::ServerDef;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Config,
    Managed,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Config => "config",
            Source::Managed => "managed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lifecycle {
    Starting,
    Started,
    Stopping,
    Stopped,
    Idle,
    Error,
}

impl Lifecycle {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lifecycle::Starting => "starting",
            Lifecycle::Started => "started",
            Lifecycle::Stopping => "stopping",
            Lifecycle::Stopped => "stopped",
            Lifecycle::Idle => "idle",
            Lifecycle::Error => "error",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Up,
    Down,
    Unknown,
}

impl Health {
    pub fn as_str(&self) -> &'static str {
        match self {
            Health::Up => "up",
            Health::Down => "down",
            Health::Unknown => "unknown",
        }
    }
}

/// Is this MCP lazy — idle at boot, woken by its first request (see `ensure_started`)?
///
/// A proc MCP is lazy BY DEFAULT: its child process is the one thing here that costs real memory
/// (50-150MB each, versus ~0 for the in-process DB drivers and remote proxies), so an idle child
/// is money nobody asked to spend. Every other type defaults to start-at-boot — but `lazy: true`
/// opts ANY type in, which is what the panel's "Start automatically" checkbox writes.
pub fn is_lazy(def: &ServerDef) -> bool {
    match def.get_bool("lazy") {
        Some(explicit) => explicit,
        None => def.type_() == "proc",
    }
}

/// How long a woken lazy proc stays up with no traffic before its child is reaped. 0 disables.
pub const DEFAULT_IDLE_MS: u64 = 600_000;

/// One hosted MCP: its definition, adapter, live endpoint (when started), and last health probe.
/// `name` lives inside `data` and follows the map key; every field is read through the lock and
/// mutated only in short critical sections.
pub struct EntryData {
    pub name: String,
    pub source: Source,
    pub def: ServerDef,
    pub adapter: Arc<dyn Adapter>,
    pub server: Option<McpEndpoint>,
    pub lifecycle: Lifecycle,
    pub started_at: Option<String>,
    /// lifecycle error (e.g. start failed)
    pub error: Option<String>,
    pub status: Health,
    pub latency_ms: Option<u64>,
    pub last_check: Option<String>,
    /// health-check failure reason
    pub last_error: Option<String>,
    pub res_page: Option<Arc<PageCache>>,
    pub tool_page: Option<Arc<PageCache>>,
    pub prompt_page: Option<Arc<PageCache>>,
    /// Idle-reap deadline for a woken lazy proc (see `arm_idle`); None when no reap is armed.
    /// The Node build held a setTimeout here — one sweeper task serves every entry instead.
    pub idle_deadline: Option<u64>,
}

pub struct EntryInner {
    pub data: RwLock<EntryData>,
    /// Serializes this entry's lifecycle operations (the Node build's `op` promise chain).
    pub op: tokio::sync::Mutex<()>,
    /// Bumped by every start and stop, so a health probe can tell whether its result still
    /// describes the run it was issued against (see `check_one`).
    pub generation: AtomicU64,
    /// Set by delete() while a queued start may still hold a reference to this entry: do_start
    /// refuses to build on a tombstoned entry, so a server built after the delete cannot leak
    /// outside the registry with no entry left to close it.
    pub deleted: AtomicBool,
}

impl EntryInner {
    pub fn name(&self) -> String {
        self.data.read().map(|d| d.name.clone()).unwrap_or_default()
    }
}

type Evictor = Box<dyn Fn(&str) + Send + Sync>;

pub struct Registry {
    entries: RwLock<HashMap<String, Arc<EntryInner>>>,
    /// The call log every registered MCP records into - shared with the adapters (handed out
    /// at make_adapter time) and used here for rename/delete following and alias cleanup.
    calls: Arc<crate::calls::CallLog>,
    /// Called when an entry's name is abandoned (rename's old name, or delete) so the router can
    /// close the endpoint it cached under that name. Without it the handler leaks: the POST path
    /// that normally evicts a stale handler never runs for a name that no longer resolves.
    evictor: RwLock<Option<Evictor>>,
    timer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    sweeper: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    interval_ms: u64,
}

fn msg(err: &str) -> String {
    err.to_string()
}

impl Registry {
    pub fn new(interval_ms: u64, calls: Arc<crate::calls::CallLog>) -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            calls,
            evictor: RwLock::new(None),
            timer: std::sync::Mutex::new(None),
            sweeper: std::sync::Mutex::new(None),
            interval_ms,
        })
    }

    pub fn register(
        &self,
        name: &str,
        source: Source,
        def: ServerDef,
        adapter: Arc<dyn Adapter>,
    ) -> Result<Arc<EntryInner>, String> {
        let mut entries = self.entries.write().map_err(|_| "registry poisoned")?;
        if entries.contains_key(name) {
            return Err(format!("MCP already registered: {name}"));
        }
        // This name now belongs to this MCP, so any leftover rename redirect for it must go —
        // otherwise a new MCP reusing a freed name would have its calls filed under the MCP that
        // vacated it.
        self.calls.forget_alias(name);
        // A lazy proc's resting state is idle (spawnable on demand); everything else starts stopped.
        let lifecycle = if is_lazy(&def) {
            Lifecycle::Idle
        } else {
            Lifecycle::Stopped
        };
        let entry = Arc::new(EntryInner {
            data: RwLock::new(EntryData {
                name: name.to_string(),
                source,
                def,
                adapter,
                server: None,
                lifecycle,
                started_at: None,
                error: None,
                status: Health::Unknown,
                latency_ms: None,
                last_check: None,
                last_error: None,
                res_page: None,
                tool_page: None,
                prompt_page: None,
                idle_deadline: None,
            }),
            op: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
            deleted: AtomicBool::new(false),
        });
        entries.insert(name.to_string(), entry.clone());
        Ok(entry)
    }

    pub fn get(&self, name: &str) -> Option<Arc<EntryInner>> {
        self.entries.read().ok().and_then(|e| e.get(name).cloned())
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries
            .read()
            .map(|e| e.contains_key(name))
            .unwrap_or(false)
    }

    pub fn names(&self) -> Vec<String> {
        self.entries
            .read()
            .map(|e| e.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn all(&self) -> Vec<Arc<EntryInner>> {
        self.entries
            .read()
            .map(|e| e.values().cloned().collect())
            .unwrap_or_default()
    }

    fn require(&self, name: &str) -> Result<Arc<EntryInner>, String> {
        self.get(name).ok_or_else(|| format!("unknown MCP: {name}"))
    }

    /// Register the router's handler-eviction callback (see `evictor`). Called once at wiring time.
    pub fn set_evictor(&self, f: Box<dyn Fn(&str) + Send + Sync>) {
        if let Ok(mut slot) = self.evictor.write() {
            *slot = Some(f);
        }
    }

    fn fire_evictor(&self, name: &str) {
        if let Ok(slot) = self.evictor.read() {
            if let Some(f) = slot.as_ref() {
                f(name);
            }
        }
    }

    /// Build the endpoint and mark started. Idempotent if already started. Errors on build failure.
    pub async fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let entry = self.require(name)?;
        // One queue slot per operation — see the module notes: two concurrent starts both passing
        // an unlocked check-then-act is what leaked pools and stray children in the Node build.
        let _guard = entry.op.lock().await;
        self.do_start(&entry).await
    }

    /// Wake an idle lazy proc and return its (now started) entry — what the MCP route calls when a
    /// client reaches a proc the gateway never spawned. The wait is the contract: AI clients do
    /// not retry gracefully, so the request that wakes the child is the request that gets served,
    /// bounded by the adapter's own start timeout. Concurrent wakeups coalesce on the lifecycle
    /// queue into a single spawn (the second caller's do_start finds a server and no-ops).
    pub async fn ensure_started(self: &Arc<Self>, name: &str) -> Result<Arc<EntryInner>, String> {
        let entry = self.require(name)?;
        let idle = entry
            .data
            .read()
            .map(|d| d.lifecycle == Lifecycle::Idle)
            .unwrap_or(false);
        if idle {
            self.start(name).await?;
            return self.require(name);
        }
        Ok(entry)
    }

    /// Note that this MCP just served traffic, pushing a lazy proc's idle-reap deadline back.
    /// Cheap and safe on every type: it only arms a deadline for an entry that is running AND
    /// lazy.
    pub fn note_activity(&self, name: &str) {
        let Some(entry) = self.get(name) else { return };
        let running = entry
            .data
            .read()
            .map(|d| d.server.is_some())
            .unwrap_or(false);
        if running {
            self.arm_idle(&entry); // no-op unless lazy (the guard lives there, once)
        }
    }

    /// (Re)arm the idle-reap deadline for a RUNNING LAZY MCP. `idleMs: 0` opts out entirely.
    /// The is_lazy guard is the whole point: this timer once armed for every started entry,
    /// silently stopping http MCPs and DB pools ten minutes after boot — the reaper is for lazy
    /// entries only.
    fn arm_idle(&self, entry: &Arc<EntryInner>) {
        let Ok(mut d) = entry.data.write() else {
            return;
        };
        if !is_lazy(&d.def) {
            return;
        }
        let requested = d.def.get_number("idleMs");
        let ms = requested
            .filter(|n| *n >= 0.0)
            .map(|n| n as u64)
            .unwrap_or(DEFAULT_IDLE_MS);
        d.idle_deadline = if ms == 0 {
            None
        } else {
            Some(swiss_core::util::now_ms() + ms)
        };
    }

    fn clear_idle(&self, entry: &Arc<EntryInner>) {
        if let Ok(mut d) = entry.data.write() {
            d.idle_deadline = None;
        }
    }

    async fn do_start(&self, entry: &Arc<EntryInner>) -> Result<(), String> {
        let (name, has_server, adapter) = {
            let Ok(d) = entry.data.read() else {
                return Err("entry poisoned".into());
            };
            (d.name.clone(), d.server.is_some(), d.adapter.clone())
        };
        if entry.deleted.load(Ordering::SeqCst) {
            return Err(format!("unknown MCP: {name}")); // delete() won the race — this entry is gone
        }
        if has_server {
            return Ok(()); // already running
        }
        entry.generation.fetch_add(1, Ordering::SeqCst);
        {
            let Ok(mut d) = entry.data.write() else {
                return Err("entry poisoned".into());
            };
            d.lifecycle = Lifecycle::Starting;
            d.error = None;
        }
        match adapter.build().await {
            Ok(endpoint) => {
                {
                    let Ok(mut d) = entry.data.write() else {
                        return Ok(());
                    };
                    d.server = Some(endpoint);
                    d.lifecycle = Lifecycle::Started;
                    d.started_at = Some(log::iso_now());
                    // fresh page cache for the new server instance
                    d.res_page = None;
                    d.tool_page = None;
                    d.prompt_page = None;
                }
                self.arm_idle(entry); // a woken lazy proc starts its idle-reap countdown now
                log::log(
                    "info",
                    "mcp started",
                    Some(json!({ "name": name, "type": adapter.kind() })),
                );
                swiss_host::mem::invalidate_memory_cache();
                Ok(())
            }
            Err(err) => {
                let error = msg(&err);
                {
                    let Ok(mut d) = entry.data.write() else {
                        return Err(error);
                    };
                    d.lifecycle = Lifecycle::Error;
                    d.error = Some(error.clone());
                }
                log::log(
                    "error",
                    "mcp start failed",
                    Some(json!({ "name": name, "err": error })),
                );
                Err(err)
            }
        }
    }

    /// Close the underlying connection/process and free memory. Idempotent if already stopped.
    pub async fn stop(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let entry = self.require(name)?;
        let _guard = entry.op.lock().await;
        self.do_stop(&entry).await
    }

    async fn do_stop(&self, entry: &Arc<EntryInner>) -> Result<(), String> {
        let (name, has_server, adapter) = {
            let Ok(d) = entry.data.read() else {
                return Err("entry poisoned".into());
            };
            (d.name.clone(), d.server.is_some(), d.adapter.clone())
        };
        if !has_server {
            return Ok(()); // already stopped (or idle — no child to stop)
        }
        self.clear_idle(entry); // the reap that is firing, or a plain stop: either way the deadline is done
        entry.generation.fetch_add(1, Ordering::SeqCst);
        {
            let Ok(mut d) = entry.data.write() else {
                return Err("entry poisoned".into());
            };
            d.lifecycle = Lifecycle::Stopping;
        }
        log::log("info", "mcp stopping", Some(json!({ "name": name })));
        let _ = adapter.close().await; // best effort by contract
        {
            let Ok(mut d) = entry.data.write() else {
                return Ok(());
            };
            d.server = None;
            // A lazy proc's resting state is idle again — the next request is welcome to re-wake
            // it. An explicit stop lands here too: for a lazy entry there is no meaningful "off"
            // short of disabling or deleting it, which is what those switches are for.
            d.lifecycle = if is_lazy(&d.def) {
                Lifecycle::Idle
            } else {
                Lifecycle::Stopped
            };
            d.status = Health::Unknown;
            d.latency_ms = None;
            d.last_error = None;
            d.res_page = None;
            d.tool_page = None;
            d.prompt_page = None;
        }
        log::log("info", "mcp stopped", Some(json!({ "name": name })));
        swiss_host::mem::invalidate_memory_cache();
        Ok(())
    }

    /// Replace an entry's def + adapter and restart it (used by config edit).
    ///
    /// `start` defaults to true — an edit from the panel is expected to take effect immediately.
    /// Pass false to swap the definition and leave the MCP stopped: boot does that for an override
    /// on an MCP the user has stopped, and the edit route does it for one that was already
    /// stopped, neither of which should be started as a side effect of a definition change.
    pub async fn update_def(
        self: &Arc<Self>,
        name: &str,
        def: ServerDef,
        adapter: Arc<dyn Adapter>,
        start: bool,
    ) -> Result<(), String> {
        let entry = self.require(name)?;
        // One queue slot for the whole swap, calling do_stop/do_start directly: going through the
        // public stop()/start() from in here would wait on a queue this operation already holds.
        let _guard = entry.op.lock().await;
        self.do_stop(&entry).await?;
        // Carry the live toggles from the old adapter to the new one — `def` came from the form
        // and carries none, and the toggles live in the adapter's shared state, not the def.
        // Without this, a connection edit would silently re-enable every tool and every resource
        // the user had turned off.
        let (prev_disabled, prev_resources) = {
            let Ok(d) = entry.data.read() else {
                return Err("entry poisoned".into());
            };
            let prev_disabled = d.adapter.tool_toggle().and_then(|t| {
                t.read()
                    .ok()
                    .map(|set| set.iter().cloned().collect::<Vec<_>>())
            });
            let prev_resources = d
                .adapter
                .resource_toggle()
                .map(|r| r.load(Ordering::SeqCst));
            (prev_disabled, prev_resources)
        };
        if let Some(disabled) = prev_disabled.as_deref() {
            if let Some(toggle) = adapter.tool_toggle() {
                if let Ok(mut set) = toggle.write() {
                    *set = disabled.iter().cloned().collect();
                }
            }
        }
        if let Some(on) = prev_resources {
            if let Some(toggle) = adapter.resource_toggle() {
                toggle.store(on, Ordering::SeqCst);
            }
        }
        let kind = adapter.kind().to_string();
        {
            let Ok(mut d) = entry.data.write() else {
                return Err("entry poisoned".into());
            };
            d.def = def;
            d.adapter = adapter;
        }
        if start {
            self.do_start(&entry).await?;
        }
        log::log(
            "info",
            "mcp config updated",
            Some(json!({ "name": name, "type": kind, "started": start })),
        );
        Ok(())
    }

    pub async fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let entry = self.require(name)?;
        let _guard = entry.op.lock().await;
        self.do_stop(&entry).await?;
        self.do_start(&entry).await
    }

    pub async fn rename(self: &Arc<Self>, old_name: &str, new_name: &str) -> Result<(), String> {
        if old_name == new_name {
            return Ok(());
        }
        let entry = self.require(old_name)?;
        {
            let mut entries = self.entries.write().map_err(|_| "registry poisoned")?;
            if entries.contains_key(new_name) {
                return Err(format!("name already exists: {new_name}"));
            }
            if let Ok(mut d) = entry.data.write() {
                d.name = new_name.to_string();
            }
            entries.remove(old_name);
            entries.insert(new_name.to_string(), entry.clone());
        }
        self.fire_evictor(old_name); // the handler cached under the old name is now unreachable — close + free it
                                     // Carry the call history over and re-key the adapter, so a rename doesn't split one MCP's
                                     // log into a dead half and an empty half.
        self.calls.rename_calls(old_name, new_name).await;
        if let Ok(d) = entry.data.read() {
            d.adapter.rename(new_name);
        }
        Ok(())
    }

    /// Remove an MCP, whatever its source: stop it first, then drop the runtime entry and its
    /// call log. (Config MCPs used to be refused here — deleting one left its gateway.config.json
    /// entry behind, so it resurrected on the next start. The admin API removes the file entry
    /// too, which is what makes deleting a config MCP honest; the runtime half flows through here.)
    pub async fn delete(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let entry = self.require(name)?;
        let source = entry
            .data
            .read()
            .map(|d| d.source)
            .unwrap_or(Source::Config);
        // Tombstone BEFORE awaiting the stop: a start already queued on this entry (or arriving
        // during the stop) would otherwise run do_start on the detached entry after the map
        // remove — building a pool/child nothing will ever close. The queued start checks the
        // flag and refuses.
        entry.deleted.store(true, Ordering::SeqCst);
        self.stop(name).await?;
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(name);
        }
        self.fire_evictor(name); // free the cached handler — the POST path won't, for a name that no longer exists
        self.calls.clear_calls(name).await; // nothing left to show it against
        log::log(
            "info",
            "mcp deleted",
            Some(json!({ "name": name, "source": source.as_str() })),
        );
        Ok(())
    }

    // --- health probing (started entries only) ---

    /// Start the health-probe interval and the idle-reap sweeper (one task each; stop_timer
    /// aborts both).
    pub fn start_timer(self: &Arc<Self>) {
        let this = self.clone();
        let interval = self.interval_ms;
        let handle = tokio::spawn(async move {
            this.check_all().await;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
                this.check_all().await;
            }
        });
        if let Ok(mut slot) = self.timer.lock() {
            *slot = Some(handle);
        }
        let this = self.clone();
        let sweep = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                this.reap_idle().await;
            }
        });
        if let Ok(mut slot) = self.sweeper.lock() {
            *slot = Some(sweep);
        }
    }

    pub fn stop_timer(&self) {
        if let Ok(mut slot) = self.timer.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
        if let Ok(mut slot) = self.sweeper.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }

    pub async fn check_all(&self) {
        self.sweep_page_caches();
        // Promise.all in the Node build: spawn per entry so one slow DB ping does not delay the
        // probes of every other MCP behind it.
        let mut spawned = Vec::new();
        for entry in self.all() {
            let this_entry = entry.clone();
            spawned.push(tokio::spawn(async move {
                Self::check_one(&this_entry).await;
            }));
        }
        for task in spawned {
            let _ = task.await;
        }
    }

    /// Reap lazy entries whose idle deadline passed (the Node build's per-entry setTimeout,
    /// centralized into one 1s walk). Takes the entry's op queue, like every lifecycle change.
    async fn reap_idle(&self) {
        for entry in self.all() {
            let due = entry.data.read().ok().and_then(|d| {
                Some((d.idle_deadline? <= swiss_core::util::now_ms()) && d.server.is_some())
            });
            if due != Some(true) {
                continue;
            }
            let (name, idle_ms) = entry
                .data
                .read()
                .ok()
                .map(|d| {
                    let ms = d
                        .def
                        .get_number("idleMs")
                        .filter(|n| *n >= 0.0)
                        .map(|n| n as u64)
                        .unwrap_or(DEFAULT_IDLE_MS);
                    (d.name.clone(), ms)
                })
                .unwrap_or_else(|| (String::new(), DEFAULT_IDLE_MS));
            log::log(
                "info",
                "mcp idle — reaping child",
                Some(json!({ "name": name, "idleMs": idle_ms })),
            );
            let _guard = entry.op.lock().await;
            let _ = self.do_stop(&entry).await;
        }
    }

    /// Drop tool/resource/prompt page caches nobody has paged through lately, so a server that
    /// dumps thousands of resources in one go doesn't keep them resident forever.
    pub fn sweep_page_caches(&self) {
        let now = swiss_core::util::now_ms();
        for entry in self.all() {
            let Ok(mut d) = entry.data.write() else {
                continue;
            };
            if d.res_page
                .as_ref()
                .is_some_and(|c| crate::paging::is_page_cache_stale(c, now))
            {
                d.res_page = None;
            }
            if d.tool_page
                .as_ref()
                .is_some_and(|c| crate::paging::is_page_cache_stale(c, now))
            {
                d.tool_page = None;
            }
            if d.prompt_page
                .as_ref()
                .is_some_and(|c| crate::paging::is_page_cache_stale(c, now))
            {
                d.prompt_page = None;
            }
        }
    }

    /// Root PIDs of every spawned child subtree (proc MCPs only). Empty when nothing was spawned
    /// — which is the signal that a memory measurement needs no process-tree walk at all.
    pub fn child_pids(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for entry in self.all() {
            let Ok(d) = entry.data.read() else { continue };
            for pid in d.adapter.pids() {
                if pid != 0 {
                    out.push(pid);
                }
            }
        }
        out
    }

    async fn check_one(entry: &Arc<EntryInner>) {
        let (lifecycle, adapter, name) = {
            let Ok(d) = entry.data.read() else { return };
            (d.lifecycle, d.adapter.clone(), d.name.clone())
        };
        if lifecycle != Lifecycle::Started {
            if let Ok(mut d) = entry.data.write() {
                d.status = Health::Unknown;
            }
            return;
        }
        // The run this probe describes. A stop landing while the ping is in flight resets status
        // and last_error, and writing a late result over that reset left a stopped MCP showing a
        // connection error — permanently, since check_one only clears `status` for a non-started
        // entry.
        let probe_gen = entry.generation.load(Ordering::SeqCst);
        let t0 = std::time::Instant::now();
        let pinged = adapter.ping().await;
        let Some(result) = pinged else {
            // This kind deliberately has no ping — http/rest are metered third-party endpoints
            // and must not be polled every 15 s. "unknown", not "down".
            if let Ok(mut d) = entry.data.write() {
                d.status = Health::Unknown;
                d.last_check = Some(log::iso_now());
            }
            return;
        };
        if let Ok(mut d) = entry.data.write() {
            d.last_check = Some(log::iso_now());
        }
        match result {
            Ok(()) => {
                if entry.generation.load(Ordering::SeqCst) != probe_gen {
                    return;
                }
                let mut d = entry.data.write().ok();
                if let Some(d) = d.as_deref_mut() {
                    d.status = Health::Up;
                    d.latency_ms = Some(t0.elapsed().as_millis() as u64);
                    d.last_error = None;
                }
            }
            Err(err) => {
                if entry.generation.load(Ordering::SeqCst) != probe_gen {
                    return;
                }
                let mut d = entry.data.write().ok();
                if let Some(d) = d.as_deref_mut() {
                    d.status = Health::Down;
                    d.latency_ms = None;
                    d.last_error = Some(err.clone());
                }
                log::log(
                    "warn",
                    "health check failed",
                    Some(json!({ "name": name, "err": err })),
                );
            }
        }
    }

    /// A flat row for the dashboard / API, with a resolved display state.
    pub fn status(&self) -> Vec<Value> {
        self.all()
            .iter()
            .filter_map(|entry| {
                let d = entry.data.read().ok()?;
                Some(json!({
                    "name": d.name,
                    "source": d.source.as_str(),
                    "type": d.adapter.kind(),
                    "lifecycle": d.lifecycle.as_str(),
                    // stopped | starting | error | up | down | unknown (lifecycle overrides health unless started)
                    "state": if d.lifecycle == Lifecycle::Started { d.status.as_str() } else { d.lifecycle.as_str() },
                    "latencyMs": d.latency_ms,
                    "lastCheck": d.last_check,
                    "reason": if d.lifecycle == Lifecycle::Error { d.error.clone() } else { d.last_error.clone() },
                    "startedAt": d.started_at,
                }))
            })
            .collect()
    }

    /// Stop the timer and close every entry. Used on shutdown.
    pub async fn close_all(self: &Arc<Self>) {
        self.stop_timer();
        let names = self.names();
        let mut tasks = Vec::new();
        for name in names {
            let this = self.clone();
            tasks.push(tokio::spawn(async move {
                let _ = this.stop(&name).await;
            }));
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}
// --- the connection catalog provider (docs/12 W3) -------------------------------------------------

/// The registry AS a [swiss_host::services::catalog::ConnectionCatalog]: the browsable half of
/// every registered MCP, offered for lease. This is the logic app.rs's browser_resolver
/// closure used to own, promoted from an anonymous callback to a typed capability the
/// host can see — registered by the MCP plugin on start, withdrawn (drained, then
/// cleared) on stop, so Data's dependency is a contract instead of an accident.
pub struct RegistryCatalog {
    registry: Arc<Registry>,
    tracker: Arc<swiss_host::services::catalog::LeaseTracker>,
}

impl RegistryCatalog {
    /// The tracker is shared with the plugin instance that registered this catalog, so its
    /// stop can drain the same leases this grants.
    pub fn new(
        registry: Arc<Registry>,
        tracker: Arc<swiss_host::services::catalog::LeaseTracker>,
    ) -> Self {
        RegistryCatalog { registry, tracker }
    }

    pub fn tracker(&self) -> Arc<swiss_host::services::catalog::LeaseTracker> {
        self.tracker.clone()
    }

    /// One entry's browser + the row facts, or None when the entry is gone/locked. The
    /// mapping is app.rs's old browser_resolver body: one row per REGISTERED entry,
    /// browsable or not, with the lifecycle-or-health state string.
    fn row_of(
        &self,
        entry: &Arc<EntryInner>,
    ) -> Option<(swiss_host::dbbrowser::BrowserFlavor, String, String, String)> {
        let data = entry.data.read().ok()?;
        let flavor = data
            .adapter
            .browser()
            .unwrap_or(swiss_host::dbbrowser::BrowserFlavor::None);
        let state = if data.lifecycle == Lifecycle::Started {
            data.status.as_str().to_string()
        } else {
            data.lifecycle.as_str().to_string()
        };
        Some((
            flavor,
            data.name.clone(),
            data.def.type_().to_string(),
            state,
        ))
    }

    /// The browser-level facts of one flavor: dialect, label — the pair browsable_connections'
    /// rows and every lease's mismatch message are built from.
    fn facts_of(flavor: &swiss_host::dbbrowser::BrowserFlavor) -> (String, String) {
        use swiss_host::dbbrowser::BrowserFlavor;
        match flavor {
            BrowserFlavor::Db(db) => (db.dialect().as_str().to_string(), db.label()),
            BrowserFlavor::Redis(rb) => ("redis".to_string(), rb.label()),
            // Nothing to browse: dialect "none", no label (the caller falls back to the
            // entry's name) — kept in the list so lease() can NAME the adapter type in its
            // refusal, exactly as the old resolver's 404s did.
            BrowserFlavor::None => ("none".to_string(), String::new()),
        }
    }
}

impl swiss_host::services::catalog::ConnectionCatalog for RegistryCatalog {
    fn list(&self) -> Vec<swiss_host::services::catalog::ConnectionInfo> {
        self.registry
            .all()
            .into_iter()
            .filter_map(|entry| {
                let (flavor, name, _adapter_type, state) = self.row_of(&entry)?;
                let (dialect, label) = Self::facts_of(&flavor);
                // A browser-less entry has no browser to ask for a label: its name is it.
                let label = if label.is_empty() {
                    name.clone()
                } else {
                    label
                };
                Some(swiss_host::services::catalog::ConnectionInfo {
                    id: name,
                    label,
                    dialect,
                    state,
                })
            })
            .collect()
    }

    fn lease(
        &self,
        id: &str,
        holder: &str,
    ) -> Result<
        swiss_host::services::catalog::ConnectionLease,
        swiss_host::services::catalog::CatalogError,
    > {
        use swiss_host::services::catalog::CatalogError;
        let Some(entry) = self.registry.get(id) else {
            return Err(CatalogError::Unknown(format!("unknown MCP: {id}")));
        };
        let Some((flavor, name, adapter_type, _state)) = self.row_of(&entry) else {
            return Err(CatalogError::Unknown(format!("unknown MCP: {id}")));
        };
        if matches!(flavor, swiss_host::dbbrowser::BrowserFlavor::None) {
            return Err(CatalogError::NotBrowsable(format!(
                "MCP '{}' ({}) has no database to browse",
                name, adapter_type
            )));
        }
        let (dialect, _label) = Self::facts_of(&flavor);
        Ok(self.tracker.grant(id, holder, &dialect, flavor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{ResourceToggle, ToolToggle};
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn def(v: Value) -> ServerDef {
        ServerDef(v.as_object().cloned().unwrap_or_default())
    }

    /// The call log every registry test drives: the test binary's shared scratch instance.
    /// Delete/rename follow it, and a unit test must never write into the real
    /// `~/.mcp-gateway`.
    fn use_temp_call_log() -> Arc<crate::calls::CallLog> {
        crate::calls::test_log()
    }

    /// The Rust shape of the Node suite fakeAdapter / countingAdapter: builds a real echo
    /// endpoint, counts builds and closes so a double build is observable, and can be given a
    /// ping outcome, a slow build, or a close that parks until released.
    #[derive(Default)]
    struct Fake {
        builds: AtomicUsize,
        closes: AtomicUsize,
        build_delay_ms: u64,
        ping: Option<Result<(), String>>,
        ping_delay_ms: u64,
        close_gate: Option<Arc<tokio::sync::Notify>>,
        tools: Option<ToolToggle>,
        resources: Option<ResourceToggle>,
    }

    impl Fake {
        fn arc(self) -> Arc<Fake> {
            Arc::new(self)
        }
        fn builds(&self) -> usize {
            self.builds.load(Ordering::SeqCst)
        }
        fn closes(&self) -> usize {
            self.closes.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Adapter for Fake {
        fn kind(&self) -> &str {
            "fake"
        }
        async fn build(&self) -> Result<McpEndpoint, String> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            if self.build_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.build_delay_ms)).await;
            }
            crate::adapters::echo::EchoAdapter::new("fake", crate::calls::test_log())
                .build()
                .await
        }
        async fn ping(&self) -> Option<Result<(), String>> {
            if self.ping_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.ping_delay_ms)).await;
            }
            self.ping.clone()
        }
        async fn close(&self) {
            if let Some(gate) = self.close_gate.clone() {
                gate.notified().await;
            }
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
        fn tool_toggle(&self) -> Option<ToolToggle> {
            self.tools.clone()
        }
        fn resource_toggle(&self) -> Option<ResourceToggle> {
            self.resources.clone()
        }
    }

    fn reg() -> Arc<Registry> {
        Registry::new(60_000, crate::calls::test_log())
    }

    fn server_of(r: &Registry, name: &str) -> Option<McpEndpoint> {
        r.get(name)?.data.read().ok()?.server.clone()
    }

    fn is_running(r: &Registry, name: &str) -> bool {
        server_of(r, name).is_some()
    }

    fn row(r: &Registry, name: &str) -> Value {
        r.status()
            .into_iter()
            .find(|s| s["name"] == name)
            .unwrap_or(Value::Null)
    }

    fn deadline(r: &Registry, name: &str) -> Option<u64> {
        r.get(name)?.data.read().ok()?.idle_deadline
    }

    fn lifecycle(r: &Registry, name: &str) -> Lifecycle {
        r.get(name).unwrap().data.read().unwrap().lifecycle
    }

    // --- lifecycle ---

    #[tokio::test]
    async fn starts_and_exposes_a_server_then_stops_and_clears_it() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        assert!(!is_running(&r, "a"));
        r.start("a").await.unwrap();
        assert!(is_running(&r, "a"));
        r.stop("a").await.unwrap();
        assert!(!is_running(&r, "a"));
    }

    #[tokio::test]
    async fn start_is_idempotent_and_does_not_rebuild() {
        let r = reg();
        let fake = Fake::default().arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            fake.clone(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        let first = server_of(&r, "a").unwrap();
        r.start("a").await.unwrap();
        let second = server_of(&r, "a").unwrap();
        assert!(Arc::ptr_eq(&first.http, &second.http));
        assert_eq!(fake.builds(), 1);
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        r.stop("a").await.unwrap();
        assert!(r.stop("a").await.is_ok());
    }

    #[tokio::test]
    async fn restart_rebuilds_the_server() {
        let r = reg();
        let fake = Fake::default().arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            fake.clone(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        let before = server_of(&r, "a").unwrap();
        r.restart("a").await.unwrap();
        let after = server_of(&r, "a").unwrap();
        assert!(!Arc::ptr_eq(&before.http, &after.http));
        assert_eq!((fake.builds(), fake.closes()), (2, 1));
    }

    #[tokio::test]
    async fn rename_moves_the_entry_under_a_new_key() {
        use_temp_call_log();
        let r = reg();
        r.register(
            "old",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("old").await.unwrap();
        r.rename("old", "new").await.unwrap();
        assert!(!r.has("old"));
        assert!(r.has("new"));
        assert!(is_running(&r, "new"));
    }

    #[tokio::test]
    async fn rejects_a_rename_onto_an_existing_name() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.register(
            "b",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        let err = r.rename("a", "b").await.unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(r.has("a") && r.has("b"));
    }

    #[tokio::test]
    async fn renaming_to_the_same_name_is_a_no_op() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.rename("a", "a").await.unwrap();
        assert!(r.has("a"));
    }

    #[tokio::test]
    async fn deletes_a_managed_mcp() {
        use_temp_call_log();
        let r = reg();
        let fake = Fake::default().arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            fake.clone(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        r.delete("a").await.unwrap();
        assert!(!r.has("a"));
        assert_eq!(fake.closes(), 1, "delete stops before it drops the entry");
    }

    // The old refusal existed because deleting a config MCP left its gateway.config.json entry
    // behind, so it resurrected on restart. The admin API removes the file entry too — the
    // registry half deletes any source.
    #[tokio::test]
    async fn deletes_a_config_mcp_as_well() {
        use_temp_call_log();
        let r = reg();
        r.register(
            "a",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        r.delete("a").await.unwrap();
        assert!(!r.has("a"));
    }

    #[tokio::test]
    async fn refuses_a_duplicate_registration() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        // `register` hands back the entry on success, which has no Debug — match, do not unwrap.
        let err = match r.register(
            "a",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        ) {
            Err(err) => err,
            Ok(_) => panic!("a duplicate registration must be refused, not shadowed"),
        };
        assert!(err.contains("already registered"), "{err}");
    }

    #[tokio::test]
    async fn the_evictor_fires_for_every_abandoned_name() {
        use_temp_call_log();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let r = reg();
        r.set_evictor(Box::new(move |name| {
            if let Ok(mut v) = sink.lock() {
                v.push(name.to_string());
            }
        }));
        r.register(
            "old",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.rename("old", "new").await.unwrap();
        r.delete("new").await.unwrap();
        // A handler cached under a name that no longer resolves is never evicted by the POST
        // path, so both the rename old name and the deleted name must come through here.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, vec!["old".to_string(), "new".to_string()]);
    }

    #[tokio::test]
    async fn update_def_carries_the_live_toggles_onto_the_new_adapter() {
        let r = reg();
        let old = Fake {
            tools: Some(Arc::new(std::sync::RwLock::new(
                ["hidden".to_string()].into_iter().collect(),
            ))),
            resources: Some(Arc::new(AtomicBool::new(false))),
            ..Default::default()
        }
        .arc();
        let new = Fake {
            tools: Some(Arc::new(std::sync::RwLock::new(Default::default()))),
            resources: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        }
        .arc();
        r.register("a", Source::Managed, def(json!({"type":"fake"})), old)
            .unwrap();
        r.start("a").await.unwrap();
        r.update_def(
            "a",
            def(json!({"type":"fake","host":"x"})),
            new.clone(),
            true,
        )
        .await
        .unwrap();
        // A connection edit must not silently re-enable every tool and resource the user turned off.
        assert!(new
            .tools
            .as_ref()
            .unwrap()
            .read()
            .unwrap()
            .contains("hidden"));
        assert!(!new.resources.as_ref().unwrap().load(Ordering::SeqCst));
        assert!(is_running(&r, "a"));
    }

    #[tokio::test]
    async fn update_def_with_start_false_swaps_the_definition_and_leaves_it_stopped() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        let next = Fake::default().arc();
        r.update_def(
            "a",
            def(json!({"type":"fake","port":1})),
            next.clone(),
            false,
        )
        .await
        .unwrap();
        assert!(!is_running(&r, "a"));
        assert_eq!(next.builds(), 0);
        let port = r
            .get("a")
            .unwrap()
            .data
            .read()
            .unwrap()
            .def
            .get_number("port");
        assert_eq!(port, Some(1.0));
    }

    // --- health ---

    #[tokio::test]
    async fn marks_a_passing_ping_as_up_with_latency() {
        let r = reg();
        r.register(
            "ok",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake {
                ping: Some(Ok(())),
                ping_delay_ms: 5,
                ..Default::default()
            }
            .arc(),
        )
        .unwrap();
        r.start("ok").await.unwrap();
        r.check_all().await;
        let row = row(&r, "ok");
        assert_eq!(row["state"], "up");
        assert!(row["latencyMs"].is_number());
        assert!(row["reason"].is_null());
    }

    #[tokio::test]
    async fn captures_the_raw_failure_reason_when_ping_fails() {
        let r = reg();
        r.register(
            "bad",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake {
                ping: Some(Err("connect ECONNREFUSED 127.0.0.1:6379".into())),
                ..Default::default()
            }
            .arc(),
        )
        .unwrap();
        r.start("bad").await.unwrap();
        r.check_all().await;
        let row = row(&r, "bad");
        assert_eq!(row["state"], "down");
        assert!(row["reason"].as_str().unwrap().contains("ECONNREFUSED"));
    }

    // http/rest are metered third-party endpoints with no ping on purpose: unknown, not down.
    #[tokio::test]
    async fn reports_unknown_when_an_adapter_has_no_ping() {
        let r = reg();
        r.register(
            "none",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("none").await.unwrap();
        r.check_all().await;
        assert_eq!(row(&r, "none")["state"], "unknown");
    }

    #[tokio::test]
    async fn does_not_probe_a_stopped_entry_and_reports_state_stopped() {
        let r = reg();
        r.register(
            "off",
            Source::Config,
            def(json!({"type":"fake"})),
            Fake {
                ping: Some(Err("should not be called".into())),
                ..Default::default()
            }
            .arc(),
        )
        .unwrap();
        r.check_all().await; // never started
        assert_eq!(row(&r, "off")["state"], "stopped");
        assert!(row(&r, "off")["reason"].is_null());
    }

    #[tokio::test]
    async fn discards_a_probe_result_that_lands_after_the_mcp_was_stopped() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake {
                ping: Some(Err("connect ECONNREFUSED 127.0.0.1:6379".into())),
                ping_delay_ms: 60,
                ..Default::default()
            }
            .arc(),
        )
        .unwrap();
        r.start("a").await.unwrap();

        let probing = {
            let r = r.clone();
            tokio::spawn(async move { r.check_all().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        r.stop("a").await.unwrap(); // the user stops it before the ping settles
        probing.await.unwrap(); // the stale failure arrives here

        // Stopping cleared the reason; nothing resets last_error for a stopped entry, so a stale
        // write would have stuck until a restart.
        let entry = r.get("a").unwrap();
        let d = entry.data.read().unwrap();
        assert_eq!(d.last_error, None);
        assert_eq!(d.status, Health::Unknown);
    }

    // --- concurrent lifecycle ---

    #[tokio::test]
    async fn builds_once_when_two_starts_arrive_together() {
        let r = reg();
        let fake = Fake {
            build_delay_ms: 30,
            ..Default::default()
        }
        .arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"counting"})),
            fake.clone(),
        )
        .unwrap();
        let (x, y) = tokio::join!(r.start("a"), r.start("a"));
        x.unwrap();
        y.unwrap();
        // A second build would be orphaned: never assigned to the entry, never closed. For a proc
        // adapter that is a stray child process.
        assert_eq!(fake.builds(), 1);
        assert!(is_running(&r, "a"));
    }

    #[tokio::test]
    async fn does_not_leave_a_server_behind_when_stop_races_start() {
        let r = reg();
        let fake = Fake {
            build_delay_ms: 30,
            ..Default::default()
        }
        .arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"counting"})),
            fake.clone(),
        )
        .unwrap();
        let (x, y) = tokio::join!(r.start("a"), r.stop("a"));
        x.unwrap();
        y.unwrap();
        // Whichever order they run in, the entry is either cleanly started or cleanly stopped —
        // never built-but-lost.
        if is_running(&r, "a") {
            assert_eq!(fake.builds() - fake.closes(), 1);
        } else {
            assert_eq!(fake.builds(), fake.closes());
        }
    }

    #[tokio::test]
    async fn serializes_restart_against_a_concurrent_start() {
        let r = reg();
        let fake = Fake {
            build_delay_ms: 30,
            ..Default::default()
        }
        .arc();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"counting"})),
            fake.clone(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        let (x, y) = tokio::join!(r.restart("a"), r.start("a"));
        x.unwrap();
        y.unwrap();
        assert!(is_running(&r, "a"));
        // Every build that happened was either closed or is the live one.
        assert_eq!(fake.builds() - fake.closes(), 1);
    }

    #[tokio::test]
    async fn refuses_a_start_that_slips_into_the_delete_window() {
        use_temp_call_log();
        let gate = Arc::new(tokio::sync::Notify::new());
        let fake = Fake {
            close_gate: Some(gate.clone()),
            ..Default::default()
        }
        .arc();
        let r = reg();
        r.register(
            "x",
            Source::Managed,
            def(json!({"type":"fake"})),
            fake.clone(),
        )
        .unwrap();
        r.start("x").await.unwrap();
        assert_eq!(fake.builds(), 1);

        // delete() parks inside adapter.close(); a start arriving in that window queues behind the
        // stop and runs AFTER the map removal — on the detached entry, where do_start must refuse.
        let del = {
            let r = r.clone();
            tokio::spawn(async move { r.delete("x").await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let late = {
            let r = r.clone();
            tokio::spawn(async move { r.start("x").await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_one();

        del.await.unwrap().unwrap();
        let err = late.await.unwrap().unwrap_err();
        assert!(err.contains("unknown MCP: x"), "{err}");
        assert_eq!(fake.builds(), 1); // nothing was built that no entry would ever close
        assert!(!r.has("x"));
    }

    // --- lazy entries and the idle reaper ---

    #[test]
    fn a_proc_mcp_is_lazy_by_default_and_every_other_type_is_not() {
        assert!(is_lazy(&def(json!({"type":"proc"}))));
        assert!(!is_lazy(&def(json!({"type":"mysql"}))));
        // `lazy` opts ANY type in, and opts proc out — it is the panel Start-automatically box.
        assert!(is_lazy(&def(json!({"type":"mysql","lazy":true}))));
        assert!(!is_lazy(&def(json!({"type":"proc","lazy":false}))));
    }

    #[tokio::test]
    async fn a_lazy_entry_rests_at_idle_and_returns_to_idle_when_stopped() {
        let r = reg();
        r.register(
            "p",
            Source::Managed,
            def(json!({"type":"proc"})),
            Fake::default().arc(),
        )
        .unwrap();
        assert_eq!(lifecycle(&r, "p"), Lifecycle::Idle);
        r.start("p").await.unwrap();
        r.stop("p").await.unwrap();
        // For a lazy entry there is no meaningful off short of disabling or deleting it.
        assert_eq!(lifecycle(&r, "p"), Lifecycle::Idle);
        assert_eq!(row(&r, "p")["state"], "idle");
    }

    #[tokio::test]
    async fn ensure_started_wakes_an_idle_entry_and_passes_a_running_one_through() {
        let r = reg();
        let fake = Fake::default().arc();
        r.register(
            "p",
            Source::Managed,
            def(json!({"type":"proc"})),
            fake.clone(),
        )
        .unwrap();
        r.ensure_started("p").await.unwrap();
        assert!(is_running(&r, "p"));
        r.ensure_started("p").await.unwrap();
        assert_eq!(fake.builds(), 1);
    }

    #[tokio::test]
    async fn the_idle_reaper_only_arms_for_lazy_entries() {
        let r = reg();
        r.register(
            "eager",
            Source::Managed,
            def(json!({"type":"mysql"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("eager").await.unwrap();
        r.note_activity("eager");
        // This timer once armed for every started entry, silently stopping http MCPs and DB pools
        // ten minutes after boot.
        assert_eq!(deadline(&r, "eager"), None);

        r.register(
            "lazy",
            Source::Managed,
            def(json!({"type":"proc"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("lazy").await.unwrap();
        assert!(deadline(&r, "lazy").is_some());
    }

    #[tokio::test]
    async fn idle_ms_zero_opts_out_of_reaping_entirely() {
        let r = reg();
        let d = def(json!({"type":"proc","idleMs":0}));
        r.register("p", Source::Managed, d, Fake::default().arc())
            .unwrap();
        r.start("p").await.unwrap();
        assert_eq!(deadline(&r, "p"), None);
        r.reap_idle().await;
        assert!(is_running(&r, "p"));
    }

    #[tokio::test]
    async fn the_sweeper_reaps_a_lazy_child_whose_deadline_passed() {
        let r = reg();
        let fake = Fake::default().arc();
        let d = def(json!({"type":"proc","idleMs":1}));
        r.register("p", Source::Managed, d, fake.clone()).unwrap();
        r.start("p").await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        r.reap_idle().await;
        assert!(!is_running(&r, "p"));
        assert_eq!(fake.closes(), 1);
        // Reaped, not stopped: the next request is welcome to wake it again.
        assert_eq!(lifecycle(&r, "p"), Lifecycle::Idle);
    }

    #[tokio::test]
    async fn activity_pushes_the_reap_deadline_back() {
        let r = reg();
        let d = def(json!({"type":"proc","idleMs":50}));
        r.register("p", Source::Managed, d, Fake::default().arc())
            .unwrap();
        r.start("p").await.unwrap();
        let first = deadline(&r, "p").unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        r.note_activity("p");
        assert!(deadline(&r, "p").unwrap() > first);
    }

    #[tokio::test]
    async fn child_pids_is_empty_when_nothing_was_spawned() {
        let r = reg();
        r.register(
            "a",
            Source::Managed,
            def(json!({"type":"fake"})),
            Fake::default().arc(),
        )
        .unwrap();
        r.start("a").await.unwrap();
        // Empty is the signal that a memory measurement needs no process-tree walk at all.
        assert!(r.child_pids().is_empty());
    }

    #[tokio::test]
    async fn close_all_stops_every_entry() {
        let r = reg();
        let a = Fake::default().arc();
        let b = Fake::default().arc();
        r.register("a", Source::Managed, def(json!({"type":"fake"})), a.clone())
            .unwrap();
        r.register("b", Source::Managed, def(json!({"type":"fake"})), b.clone())
            .unwrap();
        r.start("a").await.unwrap();
        r.start("b").await.unwrap();
        r.close_all().await;
        assert_eq!((a.closes(), b.closes()), (1, 1));
        assert!(!is_running(&r, "a") && !is_running(&r, "b"));
    }

    #[tokio::test]
    async fn a_failed_start_lands_in_error_with_the_reason_the_adapter_gave() {
        struct Broken;
        #[async_trait]
        impl Adapter for Broken {
            fn kind(&self) -> &str {
                "broken"
            }
            async fn build(&self) -> Result<McpEndpoint, String> {
                Err("ER_ACCESS_DENIED_ERROR: bad password".into())
            }
        }
        let r = reg();
        r.register(
            "x",
            Source::Config,
            def(json!({"type":"broken"})),
            Arc::new(Broken),
        )
        .unwrap();
        let err = r.start("x").await.unwrap_err();
        assert!(err.contains("ER_ACCESS_DENIED_ERROR"));
        let row = row(&r, "x");
        assert_eq!(row["state"], "error");
        // `reason` shows the lifecycle error while in error, not the (absent) health failure.
        assert!(row["reason"].as_str().unwrap().contains("bad password"));
    }

    #[tokio::test]
    async fn unknown_names_are_refused_by_every_lifecycle_operation() {
        let r = reg();
        for err in [
            r.start("nope").await.unwrap_err(),
            r.stop("nope").await.unwrap_err(),
            r.restart("nope").await.unwrap_err(),
            r.delete("nope").await.unwrap_err(),
            r.rename("nope", "other").await.unwrap_err(),
        ] {
            assert!(err.contains("unknown MCP: nope"), "{err}");
        }
    }

    // --- the connection catalog provider (docs/12 W3) ---

    #[test]
    fn the_catalog_lists_one_row_per_entry_and_keys_them_by_definition() {
        // docs/09 §3's identity rule, at the provider: two definitions that share every
        // endpoint field (host, port) but differ in NAME — which is where credentials and
        // the whole definition live — are TWO connections. Never merged, never shadowed.
        let r = reg();
        let same_endpoint = json!({"type":"fake","host":"db.local","port":3306});
        r.register(
            "prod-ro",
            Source::Managed,
            def(same_endpoint.clone()),
            Fake::default().arc(),
        )
        .unwrap();
        r.register(
            "prod-rw",
            Source::Managed,
            def(same_endpoint),
            Fake::default().arc(),
        )
        .unwrap();
        let catalog =
            RegistryCatalog::new(r.clone(), swiss_host::services::catalog::LeaseTracker::new());
        let rows = swiss_host::services::catalog::ConnectionCatalog::list(&catalog);
        let ids: Vec<&str> = rows.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"prod-ro") && ids.contains(&"prod-rw"));
        // The Fake adapter has no browser: dialect "none", and a lease attempt names the
        // adapter type exactly like the old resolver's 404 did.
        assert!(rows.iter().all(|c| c.dialect == "none"));
        match swiss_host::services::catalog::ConnectionCatalog::lease(&catalog, "prod-ro", "data") {
            Err(swiss_host::services::catalog::CatalogError::NotBrowsable(m)) => {
                assert_eq!(m, "MCP 'prod-ro' (fake) has no database to browse");
            }
            Ok(_) => panic!("not browsable, got a lease"),
            Err(other) => panic!("not browsable, wrong error: {other}"),
        }
        match swiss_host::services::catalog::ConnectionCatalog::lease(&catalog, "ghost", "data") {
            Err(swiss_host::services::catalog::CatalogError::Unknown(m)) => {
                assert!(m.contains("unknown MCP: ghost"), "{m}");
            }
            Ok(_) => panic!("unknown, got a lease"),
            Err(other) => panic!("unknown, wrong error: {other}"),
        }
    }
}
