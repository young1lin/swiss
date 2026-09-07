//! Owns the live state of every SSH connection and forwarding rule — port of
//! `tunnels/manager.ts`.
//!
//! Two invariants drive the shape of this type:
//!
//!  - A local port is bound only while its tunnel can carry traffic. Every path that leaves the
//!    working state closes the Forward (which destroys sockets and verifies the release) BEFORE
//!    it reports a new state.
//!  - One SSH client per connection, refcounted. "Start all" over 14 rules on one host dials
//!    once.
//!
//! Concurrency shape: the Node build serialized each rule's lifecycle on a promise chain
//! (`enqueue`); here it is a per-rule tokio mutex held for the whole operation. The mutable
//! runtime state sits behind a short std lock that is NEVER held across an await — snapshot out,
//! await, write back.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::forward::{ChannelOpener, Forward, ForwardTarget};
use super::port;
use super::ssh::{SshConnection, SshHooks};
use super::store::{ConnInput, RuleInput, TunnelStore};
use super::types::{
    is_retryable, FailureKind, PortOwner, RuleDef, RuleState, SshConnDef, TunnelError,
};
use crate::log;

/// Cap on reconnect backoff. A sustained outage backs off toward this interval rather than
/// dialing every `reconnectInterval` seconds forever (which, against a host that is truly down,
/// is thrash).
const RECONNECT_CAP_MS: f64 = 5.0 * 60.0 * 1000.0;
/// Clamp the exponent before the cap does, so 2 ** retries can't overflow on a long outage.
const RECONNECT_MAX_EXP: u32 = 8;

/// The slice of the MCP registry the tunnel subsystem reads. Display and guard rail only —
/// nothing here can start or stop an MCP.
pub trait McpView: Send + Sync {
    fn has(&self, name: &str) -> bool;
    /// Display state ("up" | "down" | "stopped" | "error" | …), or None when unknown.
    fn state_of(&self, name: &str) -> Option<String>;
    /// True when the MCP is actually running — what the stop guard asks about.
    fn is_started(&self, name: &str) -> bool;
    /// ISO timestamp of the MCP's current run, for the stale-pool notice.
    fn started_at(&self, name: &str) -> Option<String>;
}

/// A manager operation's failure. `Dependents` is the structured refusal the API answers with
/// 409 + a confirm; plain messages starting with "unknown " answer 404, everything else 400 —
/// the Node build's single fail() funnel.
#[derive(Debug, Clone)]
pub enum OpError {
    Msg(String),
    Dependents(Vec<String>),
}

impl OpError {
    pub fn msg(message: impl Into<String>) -> OpError {
        OpError::Msg(message.into())
    }

    pub fn message(&self) -> String {
        match self {
            OpError::Msg(m) => m.clone(),
            OpError::Dependents(d) => format!("in use by: {}", d.join(", ")),
        }
    }
}

/// One row of a start-all/stop-all result.
#[derive(Debug, Clone)]
pub struct OpResult {
    pub id: String,
    pub name: String,
    pub ok: bool,
    pub error: Option<String>,
}

impl OpResult {
    fn ok(rule: &RuleDef) -> OpResult {
        OpResult {
            id: rule.id.clone(),
            name: rule.name.clone(),
            ok: true,
            error: None,
        }
    }

    fn err(rule: &RuleDef, message: String) -> OpResult {
        OpResult {
            id: rule.id.clone(),
            name: rule.name.clone(),
            ok: false,
            error: Some(message),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("ok".into(), json!(self.ok));
        if let Some(e) = &self.error {
            m.insert("error".into(), json!(e));
        }
        Value::Object(m)
    }
}

/// The mutable half of one rule's runtime; guarded by a std lock, never held across an await.
struct RuleData {
    state: RuleState,
    reason: Option<String>,
    forward: Option<Arc<Forward>>,
    started_at: Option<String>,
    reconnected_at: Option<String>,
    port_owner: Option<PortOwner>,
    /// True between a transport loss and the next successful start, so it reports as a reconnect.
    lost: bool,
    /// Consecutive failed reconnect attempts since the last successful connect — drives backoff.
    retries: u32,
    /// The connection this rule currently holds a reference on — the Arc itself, not its id:
    /// an id lookup at release time can hit a REPLACEMENT connection built in between and
    /// decrement a ref it never took, tearing the new session down under a live rule. Pointer
    /// identity pairs every increment with its own decrement.
    holding: Option<Arc<SshConnection>>,
}

impl Default for RuleData {
    fn default() -> Self {
        RuleData {
            state: RuleState::Stopped,
            reason: None,
            forward: None,
            started_at: None,
            reconnected_at: None,
            port_owner: None,
            lost: false,
            retries: 0,
            holding: None,
        }
    }
}

struct RuleRuntime {
    /// Serializes this rule's lifecycle operations (the Node build's enqueue promise chain).
    op: tokio::sync::Mutex<()>,
    retry: Mutex<Option<tokio::task::JoinHandle<()>>>,
    data: Mutex<RuleData>,
}

pub struct TunnelManager {
    store: Arc<Mutex<TunnelStore>>,
    runtimes: Mutex<HashMap<String, Arc<RuleRuntime>>>,
    conns: Mutex<HashMap<String, Arc<SshConnection>>>,
    /// A fingerprint a connection presented when it failed verification, so it can be trusted later.
    mismatches: Mutex<HashMap<String, String>>,
    mcps: Option<Box<dyn McpView>>,
}

impl TunnelManager {
    pub fn new(store: Arc<Mutex<TunnelStore>>, mcps: Option<Box<dyn McpView>>) -> Arc<Self> {
        Arc::new(TunnelManager {
            store,
            runtimes: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            mismatches: Mutex::new(HashMap::new()),
            mcps,
        })
    }

    // --- runtime bookkeeping ----------------------------------------------------------------------

    fn rt(&self, id: &str) -> Arc<RuleRuntime> {
        let mut runtimes = self.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        runtimes
            .entry(id.to_string())
            .or_insert_with(|| {
                Arc::new(RuleRuntime {
                    op: tokio::sync::Mutex::new(()),
                    retry: Mutex::new(None),
                    data: Mutex::new(RuleData::default()),
                })
            })
            .clone()
    }

    fn with_store<T>(&self, f: impl FnOnce(&mut TunnelStore) -> T) -> T {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut store)
    }

    fn clear_retry(&self, rt: &Arc<RuleRuntime>) {
        if let Ok(mut slot) = rt.retry.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }

    // --- connections ------------------------------------------------------------------------------

    fn make_conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<SshConnection> {
        let store = self.store.clone();
        let conn_id = def.id.clone();
        let conn_name = def.name.clone();
        let on_host_key: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |fp: &str| {
            match store.lock().map(|mut s| s.set_host_key(&conn_id, fp)) {
                Ok(Ok(())) => {}
                Ok(Err(err)) => log::warn(
                    "could not persist host key",
                    Some(json!({ "connection": conn_name, "err": err })),
                ),
                Err(_) => {}
            }
        });
        let weak = Arc::downgrade(self);
        let lost_id = def.id.clone();
        let on_lost: Arc<dyn Fn(TunnelError) + Send + Sync> = Arc::new(move |err: TunnelError| {
            if let Some(mgr) = weak.upgrade() {
                mgr.on_connection_lost(&lost_id, &err);
            }
        });
        SshConnection::new(
            def.clone(),
            SshHooks {
                on_host_key: Some(on_host_key),
                on_lost: Some(on_lost),
            },
        )
    }

    fn conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<SshConnection> {
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = conns.get(&def.id) {
            existing.set_def(def.clone());
            existing.clone()
        } else {
            let made = self.make_conn(def);
            conns.insert(def.id.clone(), made.clone());
            made
        }
    }

    /// Connect (or reuse) the connection and take a reference for this rule.
    async fn acquire(
        self: &Arc<Self>,
        def: &SshConnDef,
        rt: &Arc<RuleRuntime>,
    ) -> Result<Arc<SshConnection>, TunnelError> {
        let c = self.conn(def);
        if let Err(te) = c.connect().await {
            if te.kind == FailureKind::HostKey {
                if let Some(actual) = te
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("actual"))
                    .and_then(Value::as_str)
                {
                    self.mismatches
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(def.id.clone(), actual.to_string());
                }
            }
            return Err(te);
        }
        // A rule holds at most one reference, however many times it is started — taken on THIS
        // exact client. A holding left over from a swap (the old client ended and the map
        // rebuilt one under the same id) is released against the old client, never the new.
        let stale = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            match &data.holding {
                Some(h) if Arc::ptr_eq(h, &c) => None, // already holding this very client
                _ => {
                    c.refs.fetch_add(1, Ordering::SeqCst);
                    data.holding.replace(c.clone())
                }
            }
        };
        if let Some(stale) = stale {
            self.put_ref(&stale).await;
        }
        Ok(c)
    }

    /// Drop this rule's reference; the last one out ends the client and frees its session.
    async fn release(&self, rt: &Arc<RuleRuntime>) {
        let held = rt
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .holding
            .take();
        if let Some(c) = held {
            self.put_ref(&c).await;
        }
    }

    /// Return one reference on `c`. The last one out removes it from the connection map — only
    /// if the map still holds this exact client (a replacement built in between must not be
    /// swept out from under its own holders) — and ends the session.
    async fn put_ref(&self, c: &Arc<SshConnection>) {
        if c.refs.fetch_sub(1, Ordering::SeqCst) <= 1 {
            let id = c.id();
            // Scoped: the std lock is never held across the end() await below.
            {
                let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
                let same = conns.get(&id).is_some_and(|current| Arc::ptr_eq(current, c));
                if same {
                    conns.remove(&id);
                }
            }
            c.end().await;
        }
    }

    /// Close the forward (releasing the port) and drop the SSH reference.
    async fn teardown(&self, rt: &Arc<RuleRuntime>) {
        let fwd = rt
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forward
            .take();
        if let Some(fwd) = fwd {
            fwd.close().await;
        }
        self.release(rt).await;
    }

    /// A transport died under one or more running rules.
    ///
    /// Ports come down first, always: a rule reported "reconnecting" while still holding its
    /// port is the exact confusion this feature was built to remove. Synchronous — each rule's
    /// recovery is queued through its own op chain, exactly like the Node build's fire-and-forget
    /// `void this.onConnectionLost(...)`.
    fn on_connection_lost(self: &Arc<Self>, conn_id: &str, err: &TunnelError) {
        let rules = self.with_store(|s| s.rules_for_connection(conn_id));
        for rule in rules {
            let rt = self.rt(&rule.id);
            {
                let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                if data.state != RuleState::Up && data.state != RuleState::Starting {
                    continue;
                }
            }
            let weak = Arc::downgrade(self);
            let err = err.clone();
            let rule = rule.clone();
            tokio::spawn(async move {
                let Some(mgr) = weak.upgrade() else { return };
                let rt = mgr.rt(&rule.id);
                let _guard = rt.op.lock().await;
                // Re-check under the lock: a user stop may have won the race while this op waited.
                let still = {
                    let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.state == RuleState::Up || data.state == RuleState::Starting
                };
                if !still {
                    return;
                }
                mgr.teardown(&rt).await;
                let retrying = rule.auto_reconnect && is_retryable(err.kind);
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.lost = true;
                    data.reason = Some(err.message.clone());
                    // An auth or host-key failure must not be retried: hammering the server every
                    // 10s earns a fail2ban ban, and a changed host key needs a human.
                    data.state = if retrying {
                        RuleState::Reconnecting
                    } else {
                        RuleState::Error
                    };
                }
                if retrying {
                    mgr.schedule_retry(&rule);
                }
                log::warn(
                    "tunnel down",
                    Some(json!({
                        "rule": rule.name,
                        "state": rt.data.lock().map(|d| d.state.as_str()).unwrap_or("stopped"),
                        "reason": err.message,
                    })),
                );
            });
        }
    }

    fn schedule_retry(self: &Arc<Self>, rule: &RuleDef) {
        let rt = self.rt(&rule.id);
        self.clear_retry(&rt);
        let retries = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.retries += 1;
            data.retries
        };
        let base = (rule.reconnect_interval.max(1) as f64) * 1000.0;
        // Exponential backoff capped at RECONNECT_CAP_MS: a sustained outage dials less and less
        // often instead of every `interval` seconds forever. ±20% jitter keeps rules on one host
        // from retrying in lockstep. (Auth/host-key never reach here; port failures no longer do
        // either — see is_retryable. Per-connection coalescing of sibling retries is a future
        // improvement; the SSH single-flight already collapses near-simultaneous dials.)
        let exp =
            (base * 2f64.powi((retries - 1).min(RECONNECT_MAX_EXP) as i32)).min(RECONNECT_CAP_MS);
        let delay = (exp * (0.8 + rand::random::<f64>() * 0.4)).round() as u64;
        let weak = Arc::downgrade(self);
        let id = rule.id.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let Some(mgr) = weak.upgrade() else { return };
            let rt = mgr.rt(&id);
            if let Ok(mut slot) = rt.retry.lock() {
                *slot = None; // the timer fired; a later clear must not abort the start itself
            }
            // State and reason are already recorded — the result of the retry lands there too.
            let _ = mgr.start_rule(&id).await;
        });
        let retry_slot = rt.retry.lock();
        if let Ok(mut slot) = retry_slot {
            *slot = Some(task);
        }
    }

    // --- rule lifecycle ---------------------------------------------------------------------------

    pub async fn start_rule(self: &Arc<Self>, id: &str) -> Result<(), OpError> {
        let Some(rule) = self.with_store(|s| s.rule(id)) else {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        };
        let rt = self.rt(id);
        let _guard = rt.op.lock().await;
        self.do_start(rule, false)
            .await
            .map_err(|te| OpError::msg(te.message))
    }

    async fn do_start(
        self: &Arc<Self>,
        rule: RuleDef,
        retried_own_port: bool,
    ) -> Result<(), TunnelError> {
        let rt = self.rt(&rule.id);
        self.clear_retry(&rt);
        {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            if data.forward.as_ref().is_some_and(|f| f.listening()) {
                data.state = RuleState::Up;
                return Ok(());
            }
        }
        let conn = self.with_store(|s| s.connection(&rule.connection_id));
        let Some(conn) = conn else {
            let reason = format!(
                "unknown SSH connection: {}",
                if rule.connection_id.is_empty() {
                    "(none)"
                } else {
                    rule.connection_id.as_str()
                }
            );
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Error;
            data.reason = Some(reason.clone());
            return Err(TunnelError::new(reason, FailureKind::Config));
        };
        {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Starting;
            data.reason = None;
            data.port_owner = None;
        }

        let started = async {
            let ssh = self.acquire(&conn, &rt).await?;
            let ssh_for_open = ssh.clone();
            let opener: ChannelOpener = Arc::new(move |host: &str, port: u16| {
                let ssh = ssh_for_open.clone();
                let host = host.to_string();
                Box::pin(async move { ssh.open_channel(&host, port).await })
            });
            let fwd = Forward::new(
                ForwardTarget {
                    local_port: rule.local_port,
                    target_host: rule.target_host.clone(),
                    target_port: rule.target_port,
                },
                opener,
            );
            fwd.listen().await?;
            Ok::<Arc<Forward>, TunnelError>(fwd)
        }
        .await;

        match started {
            Ok(fwd) => {
                let now = crate::log::iso_now();
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.forward = Some(fwd);
                    data.state = RuleState::Up;
                    data.retries = 0; // a successful connect resets the backoff for the next outage
                    if data.lost {
                        data.reconnected_at = Some(now);
                        data.lost = false;
                    } else {
                        data.started_at = Some(now);
                    }
                }
                // The tunnel is UP; failing to persist the enabled flag is a disk problem, not a
                // tunnel problem. Tearing down a working forward over a transient tunnels.json
                // write error trades a warning for a real outage (and a reconnect loop, since the
                // in-memory flag is already set).
                let persist = self.with_store(|s| s.set_enabled(&rule.id, true));
                if let Err(err) = persist {
                    log::warn(
                        "tunnel up, but persisting enabled failed",
                        Some(json!({ "rule": rule.name, "err": err })),
                    );
                }
                log::log(
                    "info",
                    "tunnel up",
                    Some(json!({
                        "rule": rule.name,
                        "local": rule.local_port,
                        "target": format!("{}:{}", rule.target_host, rule.target_port),
                    })),
                );
                Ok(())
            }
            Err(te) => {
                self.teardown(&rt).await;
                let mut owner: Option<PortOwner> = None;
                if te.kind == FailureKind::Port {
                    let found = port::port_owner(rule.local_port).await;
                    // Our own stale listener is the gateway's problem to clean up, not the user's:
                    // close it and try once more. Anything else is reported with the holder named,
                    // so the panel can offer Force free rather than a bare EADDRINUSE.
                    if found.as_ref().is_some_and(|o| o.pid == std::process::id())
                        && !retried_own_port
                    {
                        log::warn(
                            "reclaiming a port held by our own stale listener",
                            Some(json!({ "port": rule.local_port })),
                        );
                        self.close_stray_on(rule.local_port, &rule.id).await;
                        return Box::pin(self.do_start(rule, true)).await;
                    }
                    owner = found;
                }
                let reason = match &owner {
                    Some(o) if te.kind == FailureKind::Port => format!(
                        "local port {} is held by pid {} ({})",
                        rule.local_port, o.pid, o.name
                    ),
                    _ => te.message.clone(),
                };
                let retrying = rule.auto_reconnect && is_retryable(te.kind);
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.port_owner = owner;
                    data.state = if retrying {
                        RuleState::Reconnecting
                    } else {
                        RuleState::Error
                    };
                    data.reason = Some(reason.clone());
                }
                if retrying {
                    self.schedule_retry(&rule);
                }
                log::warn(
                    "tunnel start failed",
                    Some(json!({ "rule": rule.name, "kind": te.kind.as_str(), "reason": reason })),
                );
                Err(TunnelError::new(reason, te.kind))
            }
        }
    }

    /// Close any forward of ours that still holds `port` (state desync recovery). The teardown
    /// runs through each victim's OWN queue: the direct version bypassed the per-rule
    /// serialization every other mutation goes through, so a stop/lost op racing on that rule
    /// could interleave with it.
    async fn close_stray_on(self: &Arc<Self>, port_no: u16, except_rule_id: &str) {
        let victims: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.id != except_rule_id && r.local_port == port_no)
            .collect();
        for victim in victims {
            let rt = self.rt(&victim.id);
            if rt.data.lock().map(|d| d.forward.is_none()).unwrap_or(true) {
                continue;
            }
            let _guard = rt.op.lock().await;
            // Already torn down while this op waited in the queue.
            if rt.data.lock().map(|d| d.forward.is_none()).unwrap_or(true) {
                continue;
            }
            self.teardown(&rt).await;
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Stopped;
        }
    }

    /// Stop a rule. `persist: false` keeps its `enabled` flag, which is how shutdown leaves the
    /// set of rules that should come back up on the next boot.
    pub async fn stop_rule(
        self: &Arc<Self>,
        id: &str,
        persist: bool,
        force: bool,
    ) -> Result<(), OpError> {
        let Some(rule) = self.with_store(|s| s.rule(id)) else {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        };
        if !force {
            let dependents = self.dependents_of(id);
            if !dependents.is_empty() {
                return Err(OpError::Dependents(dependents));
            }
        }
        let rt = self.rt(id);
        let _guard = rt.op.lock().await;
        self.clear_retry(&rt);
        self.teardown(&rt).await;
        {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.state = RuleState::Stopped;
            data.reason = None;
            data.lost = false;
            data.retries = 0;
            data.started_at = None;
            data.reconnected_at = None;
            data.port_owner = None;
        }
        if persist {
            self.with_store(|s| s.set_enabled(id, false))
                .map_err(OpError::msg)?;
        }
        log::log("info", "tunnel stopped", Some(json!({ "rule": rule.name })));
        Ok(())
    }

    /// Start every rule marked enabled. Never throws: one bad rule must not hold up the gateway.
    pub async fn start_enabled(self: &Arc<Self>) -> Vec<OpResult> {
        let rules: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.enabled)
            .collect();
        if rules.is_empty() {
            return Vec::new();
        }
        self.run_many(rules, RunOp::Start).await
    }

    pub async fn start_all(self: &Arc<Self>) -> Vec<OpResult> {
        let rules = self.with_store(|s| s.rules());
        self.run_many(rules, RunOp::Start).await
    }

    pub async fn stop_all(self: &Arc<Self>, force: bool) -> Result<Vec<OpResult>, Vec<String>> {
        let running: Vec<RuleDef> = self
            .with_store(|s| s.rules())
            .into_iter()
            .filter(|r| {
                let state = self
                    .runtimes
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&r.id).cloned())
                    .and_then(|rt| rt.data.lock().ok().map(|d| d.state));
                matches!(
                    state,
                    Some(RuleState::Up)
                        | Some(RuleState::Starting)
                        | Some(RuleState::Reconnecting)
                        | Some(RuleState::Error)
                )
            })
            .collect();
        if !force {
            let mut blocked: Vec<String> = Vec::new();
            for r in &running {
                for d in self.dependents_of(&r.id) {
                    if !blocked.contains(&d) {
                        blocked.push(d);
                    }
                }
            }
            if !blocked.is_empty() {
                return Err(blocked);
            }
        }
        Ok(self.run_many(running, RunOp::StopForce).await)
    }

    /// Run an operation over many rules, grouped so rules sharing a connection are serialized
    /// behind one dial rather than racing 14 handshakes at the same host. (The groups run
    /// concurrently; within a group, sequentially.)
    async fn run_many(self: &Arc<Self>, rules: Vec<RuleDef>, op: RunOp) -> Vec<OpResult> {
        #[derive(Clone, Copy)]
        enum Which {
            Start,
            Stop,
        }
        let which = match op {
            RunOp::Start => Which::Start,
            RunOp::StopForce => Which::Stop,
        };
        let mut groups: Vec<(String, Vec<RuleDef>)> = Vec::new();
        for r in rules {
            match groups.iter_mut().find(|(k, _)| *k == r.connection_id) {
                Some((_, list)) => list.push(r),
                None => groups.push((r.connection_id.clone(), vec![r])),
            }
        }
        let mut handles = Vec::new();
        for (_, group) in groups {
            let weak = Arc::downgrade(self);
            handles.push(tokio::spawn(async move {
                let mgr = match weak.upgrade() {
                    Some(m) => m,
                    None => return Vec::new(),
                };
                let mut out = Vec::new();
                for rule in group {
                    let result = match which {
                        Which::Start => mgr.start_rule(&rule.id).await.map_err(|e| e.message()),
                        Which::Stop => mgr
                            .stop_rule(&rule.id, true, true)
                            .await
                            .map_err(|e| e.message()),
                    };
                    out.push(match result {
                        Ok(()) => OpResult::ok(&rule),
                        Err(message) => OpResult::err(&rule, message),
                    });
                }
                out
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            if let Ok(group_results) = handle.await {
                results.extend(group_results);
            }
        }
        results
    }

    // --- edits and deletes ------------------------------------------------------------------------

    /// Save a rule edit, restarting it when it was running. A restart failure must not turn
    /// into a PUT error: the edit itself already saved, and the row carries the error state.
    pub async fn apply_rule_update(
        self: &Arc<Self>,
        id: &str,
        input: &RuleInput,
    ) -> Result<RuleDef, OpError> {
        let was_running = self.is_active(id);
        if was_running {
            self.stop_rule(id, false, true).await?;
        }
        let def = self
            .with_store(|s| s.update_rule(id, input))
            .map_err(OpError::msg)?;
        if was_running {
            let _ = self.start_rule(id).await; // the rule holds its own error state
        }
        Ok(def)
    }

    /// Save a connection edit: stop its rules, replace the client, restart the ones that were
    /// running. Only those — a rule the user had deliberately stopped must not come up because a
    /// neighbour was edited.
    pub async fn apply_connection_update(
        self: &Arc<Self>,
        id: &str,
        input: &ConnInput,
    ) -> Result<SshConnDef, OpError> {
        let was_running: Vec<RuleDef> = self
            .with_store(|s| s.rules_for_connection(id))
            .into_iter()
            .filter(|r| self.is_active(&r.id))
            .collect();
        for r in &was_running {
            self.stop_rule(&r.id, false, true).await?;
        }
        let existing = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(existing) = existing {
            existing.end().await;
        }
        self.mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let def = self
            .with_store(|s| s.update_connection(id, input))
            .map_err(OpError::msg)?;
        for r in &was_running {
            let _ = self.start_rule(&r.id).await; // the rule holds its own error state
        }
        Ok(def)
    }

    pub async fn delete_rule(self: &Arc<Self>, id: &str, force: bool) -> Result<(), OpError> {
        if self.with_store(|s| s.rule(id)).is_none() {
            return Err(OpError::msg(format!("unknown rule: {id}")));
        }
        if !force {
            let dependents = self.dependents_of(id);
            if !dependents.is_empty() {
                return Err(OpError::Dependents(dependents));
            }
        }
        self.stop_rule(id, false, true).await?;
        self.runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.with_store(|s| s.remove_rule(id))
            .map_err(OpError::msg)?;
        Ok(())
    }

    pub async fn delete_connection(self: &Arc<Self>, id: &str) -> Result<(), OpError> {
        if self.with_store(|s| s.connection(id)).is_none() {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        }
        // Rules still riding this connection block the delete — as Dependents, so the API
        // answers 409 + a structured list exactly like rule deletion.
        let used_by: Vec<String> = self
            .with_store(|s| s.rules_for_connection(id))
            .iter()
            .map(|r| r.name.clone())
            .collect();
        if !used_by.is_empty() {
            return Err(OpError::Dependents(used_by));
        }
        self.with_store(|s| s.remove_connection(id))
            .map_err(OpError::msg)?;
        let existing = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(existing) = existing {
            existing.end().await;
        }
        self.mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        Ok(())
    }

    /// Dial a throwaway client to prove credentials and host key end to end.
    pub async fn test_connection(self: &Arc<Self>, id: &str) -> Result<Value, OpError> {
        let Some(def) = self.with_store(|s| s.connection(id)) else {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        };
        let res = SshConnection::test(&def).await;
        if !res.ok && res.kind.as_deref() == Some("hostkey") {
            if let Some(fp) = res.fingerprint.clone() {
                self.mismatches
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.to_string(), fp);
            }
        }
        let mut out = res.to_json();
        if res.ok && def.host_key.is_none() {
            // The throwaway test client runs WITHOUT the persistence hook, so a successful Test
            // of a keyless connection stores nothing — the first real connect (a rule start) is
            // what learns and persists the fingerprint. Re-read only so the caller sees whatever
            // IS stored.
            if let Some(stored) = self
                .with_store(|s| s.connection(id))
                .and_then(|c| c.host_key)
            {
                out.as_object_mut()
                    .map(|m| m.insert("hostKey".into(), json!(stored)));
            }
        }
        Ok(out)
    }

    /// Accept the fingerprint a connection presented. With a recorded mismatch we store exactly
    /// that key; otherwise we clear the stored one so the next connect learns it (trust on
    /// first use).
    pub fn trust_host_key(&self, id: &str) -> Result<Option<String>, OpError> {
        if self.with_store(|s| s.connection(id)).is_none() {
            return Err(OpError::msg(format!("unknown SSH connection: {id}")));
        }
        let presented = self
            .mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        match presented {
            Some(fp) => {
                self.with_store(|s| s.set_host_key(id, &fp))
                    .map_err(OpError::msg)?;
                let fresh = self.with_store(|s| s.connection(id));
                if let (Some(c), Some(def)) = (
                    self.conns
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(id)
                        .cloned(),
                    fresh,
                ) {
                    c.set_def(def);
                }
                Ok(Some(fp))
            }
            // Not update_connection({...def, hostKey: undefined}): the store deliberately
            // re-adopts a learned key when host/port are unchanged, which would silently undo
            // this clear.
            None => {
                self.with_store(|s| s.clear_host_key(id))
                    .map_err(OpError::msg)?;
                Ok(None)
            }
        }
    }

    // --- MCP linkage (display + guard rail only) --------------------------------------------------

    /// Started MCPs that this rule declares it serves.
    pub fn dependents_of(&self, rule_id: &str) -> Vec<String> {
        let Some(rule) = self.with_store(|s| s.rule(rule_id)) else {
            return Vec::new();
        };
        let Some(view) = self.mcps.as_ref() else {
            return Vec::new();
        };
        rule.mcps
            .iter()
            .filter(|n| view.is_started(n))
            .cloned()
            .collect()
    }

    /// An MCP was renamed: carry every link over so it does not silently point at a dead name.
    pub fn rename_mcp(&self, from: &str, to: &str) -> Result<(), String> {
        self.with_store(|s| s.rename_mcp(from, to))
    }

    /// An MCP was deleted: no rule may go on claiming to serve it.
    pub fn forget_mcp(&self, name: &str) -> Result<(), String> {
        self.with_store(|s| s.forget_mcp(name))
    }

    /// Tunnels declaring they serve `mcp`, for the MCP detail page.
    pub fn tunnels_for_mcp(&self, mcp: &str) -> Vec<Value> {
        let started_at = self.mcps.as_ref().and_then(|v| v.started_at(mcp));
        self.with_store(|s| s.rules())
            .into_iter()
            .filter(|r| r.mcps.iter().any(|m| m == mcp))
            .map(|r| {
                let runtime = self.rt(&r.id);
                let data = runtime.data.lock().unwrap_or_else(|e| e.into_inner());
                let mut row = Map::new();
                row.insert("id".into(), json!(r.id));
                row.insert("name".into(), json!(r.name));
                row.insert("state".into(), json!(data.state.as_str()));
                row.insert("localPort".into(), json!(r.local_port));
                row.insert("targetHost".into(), json!(r.target_host));
                row.insert("targetPort".into(), json!(r.target_port));
                if let Some(reason) = &data.reason {
                    row.insert("reason".into(), json!(reason));
                }
                if let Some(at) = &data.reconnected_at {
                    row.insert("reconnectedAt".into(), json!(at));
                }
                // A reconnect after the MCP started means its pool may still hold sockets of the
                // old SSH session. Information only — nothing here restarts an MCP.
                let stale = data
                    .reconnected_at
                    .as_ref()
                    .zip(started_at.as_ref())
                    .is_some_and(|(reconn, started)| reconn > started);
                row.insert("stalePool".into(), json!(stale));
                Value::Object(row)
            })
            .collect()
    }

    // --- reporting --------------------------------------------------------------------------------

    pub fn is_active(&self, id: &str) -> bool {
        let state = self
            .runtimes
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .and_then(|rt| rt.data.lock().ok().map(|d| d.state));
        matches!(
            state,
            Some(RuleState::Up) | Some(RuleState::Starting) | Some(RuleState::Reconnecting)
        )
    }

    /// One rule's API row (definition + live state), or None when the rule no longer exists.
    pub fn rule_row(&self, id: &str) -> Option<Value> {
        let rule = self.with_store(|s| s.rule(id))?;
        Some(self.rule_row_of(&rule))
    }

    fn rule_row_of(&self, rule: &RuleDef) -> Value {
        let rt = self.rt(&rule.id);
        let (state, reason_field, started_at, reconnected_at, port_owner, stats) = {
            let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            let stats = data.forward.as_ref().map(|f| f.stats());
            (
                data.state,
                data.reason.clone(),
                data.started_at.clone(),
                data.reconnected_at.clone(),
                data.port_owner.clone(),
                stats,
            )
        };
        let conn_name = self
            .with_store(|s| s.connection(&rule.connection_id))
            .map(|c| c.name)
            .unwrap_or_else(|| "(unknown)".into());
        let mut row = rule.to_json().as_object().cloned().unwrap_or_default();
        row.insert("state".into(), json!(state.as_str()));
        // rt.reason wins; a live listener's lastError fills in when the rule has no reason.
        let reason = match (&reason_field, &stats) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(s)) => s.last_error.clone(),
            (None, None) => None,
        };
        if let Some(r) = reason {
            row.insert("reason".into(), json!(r));
        }
        row.insert("connectionName".into(), json!(conn_name));
        if let Some(at) = started_at {
            row.insert("startedAt".into(), json!(at));
        }
        if let Some(at) = reconnected_at {
            row.insert("reconnectedAt".into(), json!(at));
        }
        row.insert(
            "sockets".into(),
            json!(stats.as_ref().map_or(0, |s| s.sockets)),
        );
        row.insert(
            "bytesIn".into(),
            json!(stats.as_ref().map_or(0, |s| s.bytes_in)),
        );
        row.insert(
            "bytesOut".into(),
            json!(stats.as_ref().map_or(0, |s| s.bytes_out)),
        );
        row.insert(
            "channelFailures".into(),
            json!(stats.as_ref().map_or(0, |s| s.channel_failures)),
        );
        if let Some(owner) = port_owner {
            row.insert("portOwner".into(), owner.to_json());
        }
        // Linked MCPs with their live state; known:false means the name has no registered MCP.
        let mcp_rows: Vec<Value> = rule
            .mcps
            .iter()
            .map(|name| {
                let view = self.mcps.as_ref();
                json!({
                    "name": name,
                    "state": view.and_then(|v| v.state_of(name)).unwrap_or_else(|| "unknown".into()),
                    "known": view.map(|v| v.has(name)).unwrap_or(false),
                })
            })
            .collect();
        row.insert("mcpRows".into(), Value::Array(mcp_rows));
        Value::Object(row)
    }

    /// Both lists in one in-memory read — safe for the panel's poll.
    pub fn rows(&self) -> Value {
        let all_rules = self.with_store(|s| s.rules());
        let rules: Vec<Value> = all_rules.iter().map(|r| self.rule_row_of(r)).collect();
        let connections: Vec<Value> = self
            .with_store(|s| s.connections())
            .iter()
            .map(|c| {
                let live = self
                    .conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&c.id)
                    .cloned();
                // Rows of THIS connection, for the counts.
                let mine: Vec<&RuleDef> = all_rules
                    .iter()
                    .filter(|r| r.connection_id == c.id)
                    .collect();
                let active = mine
                    .iter()
                    .filter(|r| {
                        matches!(
                            self.rt(&r.id).data.lock().map(|d| d.state),
                            Ok(RuleState::Up)
                        )
                    })
                    .count();
                let mut row = Map::new();
                row.insert("id".into(), json!(c.id));
                row.insert("name".into(), json!(c.name));
                row.insert("host".into(), json!(c.host));
                row.insert("port".into(), json!(c.port));
                row.insert("username".into(), json!(c.username));
                row.insert("authType".into(), json!(c.auth_type.as_str()));
                if let Some(g) = &c.group {
                    row.insert("group".into(), json!(g));
                }
                row.insert(
                    "state".into(),
                    json!(live.as_ref().map(|l| l.state().as_str()).unwrap_or("idle")),
                );
                if let Some(reason) = live.as_ref().and_then(|l| l.reason()) {
                    row.insert("reason".into(), json!(reason));
                }
                if let Some(hk) = &c.host_key {
                    row.insert("hostKey".into(), json!(hk));
                }
                row.insert("ruleCount".into(), json!(mine.len()));
                row.insert("activeRules".into(), json!(active));
                Value::Object(row)
            })
            .collect();
        json!({ "connections": Value::Array(connections), "rules": Value::Array(rules) })
    }

    /// Shut everything down without touching `enabled`, so the next boot restores the same set.
    /// Must finish inside the shutdown budget: closes are local and the SSH end() has its own
    /// 1.5s cap.
    pub async fn close_all(self: &Arc<Self>) {
        let rts: Vec<Arc<RuleRuntime>> = self
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for rt in &rts {
            self.clear_retry(rt);
        }
        let rules = self.with_store(|s| s.rules());
        let mut handles = Vec::new();
        for r in rules {
            let weak = Arc::downgrade(self);
            let id = r.id.clone();
            handles.push(tokio::spawn(async move {
                if let Some(mgr) = weak.upgrade() {
                    let _ = mgr.stop_rule(&id, false, true).await; // best effort
                }
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
        let live: Vec<Arc<SshConnection>> = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, c)| c)
            .collect();
        for c in live {
            let _ = c.end().await; // best effort
        }
    }
}

/// Which operation `run_many` applies.
enum RunOp {
    Start,
    StopForce,
}

impl crate::adminapi::TunnelLinks for TunnelManager {
    fn tunnels_for_mcp(&self, name: &str) -> Vec<Value> {
        self.tunnels_for_mcp(name)
    }

    fn rename_mcp(&self, from: &str, to: &str) {
        let _ = TunnelManager::rename_mcp(self, from, to);
    }

    fn forget_mcp(&self, name: &str) {
        let _ = TunnelManager::forget_mcp(self, name);
    }
}
