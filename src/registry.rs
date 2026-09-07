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
use crate::calls::{clear_calls, forget_call_alias, rename_calls};
use crate::config::ServerDef;
use crate::log;
use crate::paging::PageCache;

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
    pub fn new(interval_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
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
        forget_call_alias(name);
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
            Some(crate::util::now_ms() + ms)
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
                crate::mem::invalidate_memory_cache();
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
        crate::mem::invalidate_memory_cache();
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
        rename_calls(old_name, new_name).await;
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
        clear_calls(name).await; // nothing left to show it against
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
                Some((d.idle_deadline? <= crate::util::now_ms()) && d.server.is_some())
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
        let now = crate::util::now_ms();
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
