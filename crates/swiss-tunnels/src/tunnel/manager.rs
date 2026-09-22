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
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::forward::{ByteStream, ChannelOpener, Forward, ForwardTarget};
use super::port;
use super::ssh::{JumpDefs, JumpDialer, SshConnection, SshHooks};
use super::store::{ConnInput, RuleInput, TunnelStore};
use super::types::{
    is_retryable, ConnState, FailureKind, PortOwner, RuleDef, RuleState, SshConnDef, TunnelError,
};
use swiss_core::log;
use swiss_host::config::is_env_ref;
use swiss_host::mask::MASK;
use swiss_host::services::shell::{PtyEndpoint, PtySize};

/// Cap on reconnect backoff. A sustained outage backs off toward this interval rather than
/// dialing every `reconnectInterval` seconds forever (which, against a host that is truly down,
/// is thrash).
const RECONNECT_CAP_MS: f64 = 5.0 * 60.0 * 1000.0;
/// Clamp the exponent before the cap does, so 2 ** retries can't overflow on a long outage.
const RECONNECT_MAX_EXP: u32 = 8;

/// One connection operation's future. The crate carries no `async-trait`, and boxing a future
/// per dial or channel open costs nothing beside the network round trip inside it.
type ConnFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The slice of an SSH client this manager drives. `SshConnection` is the only implementor that
/// ships — the indirection exists so the tests can plug in a client whose channels are plain TCP
/// sockets, and run the whole rule lifecycle (sharing, transport loss, backoff) with no sshd
/// anywhere. It is the Node build's `SshLike` interface, which the manager took the same way.
pub trait SshLike: Send + Sync {
    fn id(&self) -> String;
    fn state(&self) -> ConnState;
    fn reason(&self) -> Option<String>;
    /// Adopt an edited definition without dropping the live session.
    fn set_def(&self, def: SshConnDef);
    /// How many rules hold this client. A plain field on `SshConnection`; a method here because
    /// a trait cannot have one.
    fn refs(&self) -> &AtomicI64;
    /// Dial, or return at once when already connected. Takes `Arc<Self>` because a real connect
    /// hands the client to a watcher task that outlives the call.
    fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>>;
    fn open_channel(
        &self,
        host: String,
        port: u16,
    ) -> ConnFuture<'_, Result<ByteStream, TunnelError>>;
    /// Open an interactive PTY and pump it against `endpoint` (docs/14 T2). Resolves when
    /// the shell has started, not when it ends.
    ///
    /// `hold` is this session's reference on the client, minted by
    /// [TunnelManager::open_shell]. Passing it in rather than letting the implementation
    /// mint its own is the point: reference counting has exactly one owner (`put_ref`
    /// below), and a second implementation of it would eventually disagree with the first
    /// — the way that disagreement shows up in the product is a terminal closing somebody
    /// else's tunnel.
    fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> ConnFuture<'_, Result<(), TunnelError>>;
    /// Run one non-interactive command to completion over a session channel
    /// (docs/34 §11), streaming stdout/stderr into `events` as they arrive and
    /// honouring `cancel` by closing the channel. `hold` is the operation's
    /// reference on this client, owned by the returned future.
    fn exec(
        &self,
        request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<i32, TunnelError>>;
    /// Stat one remote path over SFTP; None when it does not exist.
    fn stat_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>;
    /// Open one remote path for streaming read. The returned reader owns `hold`.
    fn open_read(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>;
    /// Create/truncate one remote path for streaming write. The writer owns `hold`.
    fn create_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>;
    /// Create one remote path (and missing parents) over SFTP.
    fn mkdir_p(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>>;
    /// List one remote directory's immediate children over SFTP readdir.
    fn list_dir(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>>;
    fn end(&self) -> ConnFuture<'_, ()>;
}

impl SshLike for SshConnection {
    fn id(&self) -> String {
        SshConnection::id(self)
    }
    fn state(&self) -> ConnState {
        SshConnection::state(self)
    }
    fn reason(&self) -> Option<String> {
        SshConnection::reason(self)
    }
    fn set_def(&self, def: SshConnDef) {
        SshConnection::set_def(self, def);
    }
    fn refs(&self) -> &AtomicI64 {
        &self.refs
    }
    fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::connect(&self).await })
    }
    fn open_channel(
        &self,
        host: String,
        port: u16,
    ) -> ConnFuture<'_, Result<ByteStream, TunnelError>> {
        Box::pin(async move { SshConnection::open_channel(self, &host, port).await })
    }
    fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::open_shell(self, size, endpoint, hold).await })
    }
    /// Run one non-interactive command to completion over a session channel
    /// (docs/34 §11), streaming stdout/stderr into `events` as they arrive and
    /// honouring `cancel` by closing the channel. `hold` is the operation's
    /// reference on this client, owned by the returned future.
    fn exec(
        &self,
        request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<i32, TunnelError>> {
        Box::pin(async move { SshConnection::exec(self, request, events, cancel, hold).await })
    }
    /// Stat one remote path over SFTP; None when it does not exist.
    fn stat_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>
    {
        Box::pin(async move { SshConnection::stat_file(self, path, hold).await })
    }
    /// Open one remote path for streaming read. The returned reader owns `hold`.
    fn open_read(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>
    {
        Box::pin(async move { SshConnection::open_read(self, path, hold).await })
    }
    /// Create/truncate one remote path for streaming write. The writer owns `hold`.
    fn create_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>
    {
        Box::pin(async move { SshConnection::create_file(self, path, hold).await })
    }
    /// Create one remote path (and missing parents) over SFTP.
    fn mkdir_p(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::mkdir_p(self, path, hold).await })
    }
    /// List one remote directory's immediate children over SFTP readdir.
    fn list_dir(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>> {
        Box::pin(async move { SshConnection::list_dir(self, path, hold).await })
    }
    fn end(&self) -> ConnFuture<'_, ()> {
        Box::pin(SshConnection::end(self))
    }
}

/// One operation's reference on an SSH client, released when the operation ends
/// (docs/34 §10).
///
/// Born as the shell session's guard (docs/14 T2) and generalized when remote exec and
/// SFTP needed the same discipline: whatever the operation — a rule, a PTY, an exec, a
/// file transfer — it counts its reference through the ONE path (`put_ref`) that knows
/// "the last one out ends the client". Dropping the lease is the release; the task that
/// owns it drops it when the work is over. `ShellSessionGuard` remains as an alias so
/// existing imports keep their meaning.
pub struct ConnectionLease {
    manager: Arc<TunnelManager>,
    conn: Arc<dyn SshLike>,
}

/// The pre-remote name of [ConnectionLease], kept so the shell-side vocabulary stays
/// readable where only shells are meant.
pub type ShellSessionGuard = ConnectionLease;

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let conn = self.conn.clone();
        // Drop cannot await, and the release ends the client when this was the last
        // reference — a network round trip. Hand it to the runtime instead.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { manager.put_ref(&conn).await });
            }
            // Only reachable if a guard outlives the runtime during shutdown. Return the
            // count anyway so the ledger stays honest; the transport dies with the
            // runtime regardless, and panicking here would abort the process.
            Err(_) => {
                conn.refs().fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// Builds the client for one connection definition, with the manager's hooks already attached.
type ConnFactory = Arc<dyn Fn(&SshConnDef, SshHooks) -> Arc<dyn SshLike> + Send + Sync>;

/// One live chain's reference on its jump hop (docs/27 §3.2) — ShellSessionGuard's
/// discipline for the jump case, so a chained connection counts references through the
/// SAME path a rule and a shell do. It travels inside the connection's Live; dropping it
/// with that Live is the release.
struct JumpHold {
    manager: Arc<TunnelManager>,
    conn: Arc<dyn SshLike>,
}

impl Drop for JumpHold {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let conn = self.conn.clone();
        // Drop cannot await, and the release ends the client when this was the last
        // reference — a network round trip. Hand it to the runtime instead.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { manager.put_ref(&conn).await });
            }
            // Only reachable if a hold outlives the runtime during shutdown. Return the
            // count anyway so the ledger stays honest; the transport dies with the
            // runtime regardless, and panicking here would abort the process.
            Err(_) => {
                conn.refs().fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// The factory every non-test caller gets: a real russh client.
fn real_connections() -> ConnFactory {
    Arc::new(|def: &SshConnDef, hooks: SshHooks| {
        SshConnection::new(def.clone(), hooks) as Arc<dyn SshLike>
    })
}

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
    holding: Option<Arc<dyn SshLike>>,
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
    conns: Mutex<HashMap<String, Arc<dyn SshLike>>>,
    /// A fingerprint a connection presented when it failed verification, so it can be trusted later.
    mismatches: Mutex<HashMap<String, String>>,
    mcps: Option<Box<dyn McpView>>,
    make_connection: ConnFactory,
    /// Raised by `close_all`, lowered by the next `start_enabled`. A start that lands in
    /// between is refused: the boot task's per-connection groups are plain spawns that
    /// outlive the plugin scope, and one of them starting a rule AFTER close_all had passed
    /// it left a listening forward on a client close_all then ended — "up" in the panel, every
    /// connection through it "ssh connection is not established" (a rapid Restart of the
    /// tunnels plugin did exactly this to two Redis rules).
    closed: AtomicBool,
}

impl TunnelManager {
    pub fn new(store: Arc<Mutex<TunnelStore>>, mcps: Option<Box<dyn McpView>>) -> Arc<Self> {
        Self::with_connections(store, mcps, real_connections())
    }

    /// The same manager over a different client factory — the tests' way in.
    fn with_connections(
        store: Arc<Mutex<TunnelStore>>,
        mcps: Option<Box<dyn McpView>>,
        make_connection: ConnFactory,
    ) -> Arc<Self> {
        Arc::new(TunnelManager {
            store,
            runtimes: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            mismatches: Mutex::new(HashMap::new()),
            mcps,
            make_connection,
            closed: AtomicBool::new(false),
        })
    }

    /// Lower the `close_all` gate so rules may start again. The plugin's start path calls
    /// this before it spawns the boot task; `start_enabled` calls it too, for callers that
    /// go straight to the manager.
    pub fn reopen(&self) {
        self.closed.store(false, Ordering::SeqCst);
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

    fn make_conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<dyn SshLike> {
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
        // docs/27 §3.1: the dialer this connection's dial calls back into when its def
        // carries a jump. Weak so the connection table never closes into an Arc cycle
        // (manager -> client -> hook -> manager); a manager that is already gone fails
        // the hop as config rather than holding shutdown open.
        let weak_jump = Arc::downgrade(self);
        let jump: JumpDialer = Arc::new(move |jump_id: &str, host: &str, port: u16| {
            let weak_jump = weak_jump.clone();
            let jump_id = jump_id.to_string();
            let host = host.to_string();
            Box::pin(async move {
                let Some(mgr) = weak_jump.upgrade() else {
                    return Err(TunnelError::new(
                        "the tunnel manager is shutting down",
                        FailureKind::Config,
                    ));
                };
                mgr.dial_jump(&jump_id, &host, port).await
            })
        });
        (self.make_connection)(
            def,
            SshHooks {
                on_host_key: Some(on_host_key),
                on_lost: Some(on_lost),
                jump: Some(jump),
            },
        )
    }

    fn conn(self: &Arc<Self>, def: &SshConnDef) -> Arc<dyn SshLike> {
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

    /// Connect (or reuse) the connection, WITHOUT taking a reference on it.
    ///
    /// Split out of `acquire` for the shell capability: a terminal session needs the same
    /// dial and the same host-key-mismatch bookkeeping, but its reference is session-
    /// scoped rather than rule-scoped, so the two diverge only after this point.
    async fn dial(self: &Arc<Self>, def: &SshConnDef) -> Result<Arc<dyn SshLike>, TunnelError> {
        let c = self.conn(def);
        if let Err(te) = c.clone().connect().await {
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
        Ok(c)
    }

    /// docs/27 §3.1/§3.2 — one hop of a live chain. The jump's own connection is dialed
    /// (or reused) through the SAME connection table a rule start goes through —
    /// single-flight, and shared with any rules riding that hop — then a direct-tcpip
    /// channel to this connection's host:port is opened on it and handed back for the
    /// caller's SSH handshake. The returned hold is the reference the caller's Live
    /// keeps on the hop; dropping it (a deliberate end or the transport watcher)
    /// releases the hop through put_ref, exactly like a shell session's reference.
    async fn dial_jump(
        self: &Arc<Self>,
        jump_id: &str,
        host: &str,
        port: u16,
    ) -> Result<(ByteStream, Box<dyn Send + 'static>), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(jump_id)) else {
            // Save-time validation keeps jumps pointing at real rows and deletion is
            // refused while a dependent exists, so this only fires on a hand-edited
            // file; reporting it as config keeps it out of the retry loop.
            return Err(TunnelError::new(
                format!("jump connection not found: {jump_id}"),
                FailureKind::Config,
            ));
        };
        let name = def.name.clone();
        // docs/27 §3.3: the hop's failure keeps its OWN kind (auth and hostkey are never
        // retried) while the message names the hop. The pair is constructed directly —
        // the prefix must never flow through classify_message, whose substring scan
        // would wash the kind away. The inner detail is dropped on purpose: a hop's
        // host-key mismatch is recorded against the HOP by dial above; carrying it
        // outward would offer "trust this key" for the wrong connection.
        let via =
            |err: TunnelError| TunnelError::new(format!("via {name}: {}", err.message), err.kind);
        let hop = self.dial(&def).await.map_err(via)?;
        hop.refs().fetch_add(1, Ordering::SeqCst);
        match hop.open_channel(host.to_string(), port).await {
            Ok(stream) => Ok((
                stream,
                Box::new(JumpHold {
                    manager: self.clone(),
                    conn: hop.clone(),
                }),
            )),
            Err(err) => {
                self.put_ref(&hop).await;
                Err(via(err))
            }
        }
    }

    /// Connect (or reuse) the connection and take a reference for this rule.
    async fn acquire(
        self: &Arc<Self>,
        def: &SshConnDef,
        rt: &Arc<RuleRuntime>,
    ) -> Result<Arc<dyn SshLike>, TunnelError> {
        let c = self.dial(def).await?;
        // A rule holds at most one reference, however many times it is started — taken on THIS
        // exact client. A holding left over from a swap (the old client ended and the map
        // rebuilt one under the same id) is released against the old client, never the new.
        let stale = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            match &data.holding {
                Some(h) if Arc::ptr_eq(h, &c) => None, // already holding this very client
                _ => {
                    c.refs().fetch_add(1, Ordering::SeqCst);
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
    async fn put_ref(&self, c: &Arc<dyn SshLike>) {
        if c.refs().fetch_sub(1, Ordering::SeqCst) <= 1 {
            let id = c.id();
            // Scoped: the std lock is never held across the end() await below.
            {
                let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
                let same = conns
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, c));
                if same {
                    conns.remove(&id);
                }
            }
            c.end().await;
        }
    }

    // --- interactive shells (docs/14 T2) ----------------------------------------------------------

    /// The connection definitions, for a capability that needs the host list without
    /// reaching through the store lock itself.
    pub fn connections(&self) -> Vec<SshConnDef> {
        self.with_store(|s| s.connections())
    }

    /// The live state of one connection, for the shell target list. A definition with no
    /// client yet is `Idle` — it has never been dialed, which is different from failing.
    pub fn conn_state(&self, id: &str) -> ConnState {
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|c| c.state())
            .unwrap_or(ConnState::Idle)
    }

    /// Open an interactive PTY on one connection, dialing it if nothing holds it yet.
    ///
    /// The session takes a reference the same way a rule does — through `put_ref`, the
    /// one place that knows "the last one out ends the client". A terminal on a host that
    /// already has tunnels up therefore shares their session rather than dialing a second
    /// one, and closing the terminal does not take their tunnels down with it.
    ///
    /// The reference travels INTO the pump (see [SshLike::open_shell]) rather than coming
    /// back to the caller: the pump is the only thing that knows when the session is
    /// really over, and a guard handed back to the provider would have nowhere to live.
    pub async fn open_shell(
        self: &Arc<Self>,
        conn_id: &str,
        size: PtySize,
        endpoint: PtyEndpoint,
    ) -> Result<(), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(conn_id)) else {
            return Err(TunnelError::new(
                format!("unknown ssh connection: {conn_id}"),
                FailureKind::Config,
            ));
        };
        let c = self.dial(&def).await?;
        c.refs().fetch_add(1, Ordering::SeqCst);
        let hold = ConnectionLease {
            manager: self.clone(),
            conn: c.clone(),
        };
        // On failure the guard drops here and gives the reference straight back, so a
        // refused PTY cannot leave a connection pinned open with nothing using it.
        c.open_shell(size, endpoint, Box::new(hold)).await
    }

    /// Dial (or reuse) one connection and hand back the client TOGETHER with its
    /// operation-scoped lease (docs/34 §10): the remote capability's entry point, used
    /// by exec and by every SFTP operation. The caller moves the lease into whatever
    /// owns the operation — the exec future or the file reader/writer — so the reference
    /// lives exactly as long as the work does.
    pub async fn lease_connection(
        self: &Arc<Self>,
        conn_id: &str,
    ) -> Result<(Arc<dyn SshLike>, ConnectionLease), TunnelError> {
        let Some(def) = self.with_store(|s| s.connection(conn_id)) else {
            return Err(TunnelError::new(
                format!("unknown ssh connection: {conn_id}"),
                FailureKind::Config,
            ));
        };
        let c = self.dial(&def).await?;
        c.refs().fetch_add(1, Ordering::SeqCst);
        Ok((
            c.clone(),
            ConnectionLease {
                manager: self.clone(),
                conn: c,
            },
        ))
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
        if self.closed.load(Ordering::SeqCst) {
            return Err(TunnelError::new(
                "tunnels are closed (the plugin is stopped or stopping)",
                FailureKind::Config,
            ));
        }
        let listening_on_live_client = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            let listening = data.forward.as_ref().is_some_and(|f| f.listening());
            let live = data
                .holding
                .as_ref()
                .is_some_and(|c| c.state() == ConnState::Connected);
            if listening && live {
                data.state = RuleState::Up;
                return Ok(());
            }
            listening
        };
        // A listener whose SSH client is gone forwards nothing: every accept fails to open a
        // channel. Rebuild it rather than report a port that is bound to a dead session.
        if listening_on_live_client {
            self.teardown(&rt).await;
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
                Box::pin(async move { ssh.open_channel(host, port).await })
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
                let now = swiss_core::log::iso_now();
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
        self.reopen();
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
        // docs/27 §3.2: connections that jump through this one ride its transport, so
        // their running rules restart with it — conservatively, without diffing which
        // fields changed: the old client is going away either way, and every dependent
        // redials the whole chain through the fresh one.
        let dependent_rules: Vec<RuleDef> = self
            .with_store(|s| s.connections())
            .into_iter()
            .filter(|c| c.jump.as_deref() == Some(id))
            .flat_map(|c| self.with_store(|s| s.rules_for_connection(&c.id)))
            .filter(|r| self.is_active(&r.id))
            .collect();
        for r in was_running.iter().chain(&dependent_rules) {
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
        for r in was_running.iter().chain(&dependent_rules) {
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
        // docs/27 §3.2: a connection other definitions jump through cannot be deleted.
        // Unlike the rules case below there is no confirm/force path — deleting would
        // leave every dependent's chain pointing at nothing — so this is a plain error
        // naming the dependents (a 400, not the 409 + structured list).
        let jump_users: Vec<String> = self
            .with_store(|s| s.connections())
            .iter()
            .filter(|c| c.jump.as_deref() == Some(id))
            .map(|c| c.name.clone())
            .collect();
        if !jump_users.is_empty() {
            return Err(OpError::msg(format!(
                "connection is used as a jump by: {}",
                jump_users.join(", ")
            )));
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
        // docs/27 §3.2: the probe resolves its hop definitions read-only out of the
        // store and dials a private throwaway chain — nothing lands in the shared
        // connection table and no hop learns a host key.
        let store = self.store.clone();
        let defs: JumpDefs = Arc::new(move |jump_id: &str| {
            store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .connection(jump_id)
        });
        let res = SshConnection::test(&def, Some(defs)).await;
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
                // docs/27 §4 addendum: the panel's READ surface for the proxy/jump fields is
                // this row (the POST/PUT echo is read once and dropped). Proxy, proxyUsername
                // and the jump id ride plaintext, absent-when-unset — the panel resolves the
                // jump id to a name from this same list. proxyPassword rides as the MASK
                // sentinel, the same rule mask_conn applies to the POST/PUT echo: the row
                // never carries a secret, and an edit that leaves the field untouched sends
                // the sentinel back through unmask_conn, keeping the stored value. The
                // connection's key path joins this read surface too: plaintext, absent
                // when unset — the edit sheet prefills it from the row.
                if let Some(url) = &c.proxy {
                    row.insert("proxy".into(), json!(url));
                }
                if let Some(u) = &c.proxy_username {
                    row.insert("proxyUsername".into(), json!(u));
                }
                if let Some(pw) = &c.proxy_password {
                    let sentinel = !is_env_ref(&Value::String(pw.clone())) && !pw.is_empty();
                    row.insert(
                        "proxyPassword".into(),
                        json!(if sentinel { MASK } else { pw.as_str() }),
                    );
                }
                if let Some(jump) = &c.jump {
                    row.insert("jump".into(), json!(jump));
                }
                // The key path rides the same read surface: the edit sheet prefills from
                // this row, so a custom path must arrive here or a save would rewrite it
                // to the default. Plaintext (a path, not a secret), absent when unset,
                // appended after the frozen prefix — the docs/27 §4 addendum rule.
                if let Some(kp) = &c.key_path {
                    row.insert("keyPath".into(), json!(kp));
                }
                Value::Object(row)
            })
            .collect();
        json!({ "connections": Value::Array(connections), "rules": Value::Array(rules) })
    }

    /// Shut everything down without touching `enabled`, so the next boot restores the same set.
    /// Must finish inside the shutdown budget: closes are local and the SSH end() has its own
    /// 1.5s cap.
    pub async fn close_all(self: &Arc<Self>) {
        // Raised BEFORE the stops: a start that queues behind one of them (or arrives after
        // the sweep) must find the gate up, or it rebuilds what this is tearing down.
        self.closed.store(true, Ordering::SeqCst);
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
        let live: Vec<Arc<dyn SshLike>> = self
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

/// What the MCP admin API is allowed to do with tunnels: read the links, and keep them pointing
/// at the right MCP name. Deliberately this narrow — an MCP operation must never start or stop
/// a tunnel (the port of Node's `TunnelLinks`), and it lives HERE so the admin side depends on
/// the tunnel side's vocabulary, never the reverse.
pub trait TunnelLinks: Send + Sync {
    /// Tunnel rows this MCP's traffic depends on, for the detail page's "tunnels" list.
    fn tunnels_for_mcp(&self, name: &str) -> Vec<Value>;
    /// A tunnel that declares it serves this MCP must follow a rename, or the link silently dies.
    fn rename_mcp(&self, from: &str, to: &str);
    /// No MCP by that name any more, so no rule may claim to serve it.
    fn forget_mcp(&self, name: &str);
}

impl TunnelLinks for TunnelManager {
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

#[cfg(test)]
mod tests {
    //! Ported from the Node build's `test/tunnel-manager.test.ts`, one case per `it(...)`.
    //!
    //! The whole rule lifecycle runs here with no sshd anywhere: the manager builds `FakeConn`
    //! instead of `SshConnection` (see `SshLike`), and a fake channel is a plain TCP socket to a
    //! local echo server. So "the tunnel carries traffic" is asserted by sending bytes through
    //! the bound local port and reading them back, exactly as a user would.
    use super::*;
    use crate::tunnel::sshtest;
    use crate::tunnel::types::AuthType;
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicI32, AtomicU32};
    use std::time::Duration as StdDuration;

    use swiss_host::services::shell::{
        drain_sessions, PtyEvent, PtyInput, ShellError, ShellProvider, ShellRegistry,
        OUTPUT_QUEUE_CHUNKS,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    // --- the fake SSH client ----------------------------------------------------------------

    /// Every client one manager built, newest last — the Node suite's `built` array, but scoped
    /// to a single manager so tests never see each other's connections.
    type Built = Arc<Mutex<Vec<Arc<FakeConn>>>>;

    /// A stand-in for `SshConnection`: `open_channel` dials the echo server over loopback.
    struct FakeConn {
        def: Mutex<SshConnDef>,
        hooks: SshHooks,
        refs: AtomicI64,
        state: Mutex<ConnState>,
        reason: Mutex<Option<String>>,
        /// Dials that actually reached the "server" — a connect on a live client is not one.
        dials: AtomicU32,
        ended: AtomicU32,
        /// Interactive shells opened on this client (docs/14 T2).
        shells: AtomicU32,
        /// Non-interactive execs run on this client (docs/34).
        execs: AtomicU32,
        /// The exit status the next fake exec reports.
        exec_exit: AtomicI32,
        /// Bytes the next fake exec streams to stdout before exiting.
        exec_output: Mutex<Vec<u8>>,
        /// The in-memory remote filesystem the fake file ops serve.
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        /// Set to make every shell request fail - a server with PTYs disabled.
        refuse_shell: AtomicBool,
        /// Set to make every connect fail with this error.
        fail_with: Mutex<Option<TunnelError>>,
    }

    impl FakeConn {
        fn new(def: &SshConnDef, hooks: SshHooks) -> Arc<FakeConn> {
            Arc::new(FakeConn {
                def: Mutex::new(def.clone()),
                hooks,
                refs: AtomicI64::new(0),
                state: Mutex::new(ConnState::Idle),
                reason: Mutex::new(None),
                dials: AtomicU32::new(0),
                ended: AtomicU32::new(0),
                shells: AtomicU32::new(0),
                execs: AtomicU32::new(0),
                exec_exit: AtomicI32::new(0),
                exec_output: Mutex::new(Vec::new()),
                files: Arc::new(Mutex::new(HashMap::new())),
                refuse_shell: AtomicBool::new(false),
                fail_with: Mutex::new(None),
            })
        }

        fn dials(&self) -> u32 {
            self.dials.load(Ordering::SeqCst)
        }

        fn ended(&self) -> u32 {
            self.ended.load(Ordering::SeqCst)
        }

        fn ref_count(&self) -> i64 {
            self.refs.load(Ordering::SeqCst)
        }

        fn shells(&self) -> u32 {
            self.shells.load(Ordering::SeqCst)
        }

        fn username(&self) -> String {
            self.def.lock().unwrap().username.clone()
        }

        fn is_connected(&self) -> bool {
            *self.state.lock().unwrap() == ConnState::Connected
        }

        /// The transport dies under whatever rules are riding it.
        fn die(&self, kind: FailureKind) {
            *self.state.lock().unwrap() = ConnState::Error;
            if let Some(on_lost) = &self.hooks.on_lost {
                on_lost(TunnelError::new(
                    format!("ssh connection lost: {} failure", kind.as_str()),
                    kind,
                ));
            }
        }
    }

    impl SshLike for FakeConn {
        fn id(&self) -> String {
            self.def.lock().unwrap().id.clone()
        }
        fn state(&self) -> ConnState {
            *self.state.lock().unwrap()
        }
        fn reason(&self) -> Option<String> {
            self.reason.lock().unwrap().clone()
        }
        fn set_def(&self, def: SshConnDef) {
            *self.def.lock().unwrap() = def;
        }
        fn refs(&self) -> &AtomicI64 {
            &self.refs
        }
        fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>> {
            Box::pin(async move {
                if self.is_connected() {
                    return Ok(());
                }
                self.dials.fetch_add(1, Ordering::SeqCst);
                let failure = self.fail_with.lock().unwrap().clone();
                if let Some(err) = failure {
                    *self.state.lock().unwrap() = ConnState::Error;
                    *self.reason.lock().unwrap() = Some(err.message.clone());
                    return Err(err);
                }
                *self.state.lock().unwrap() = ConnState::Connected;
                *self.reason.lock().unwrap() = None;
                Ok(())
            })
        }
        fn open_channel(
            &self,
            host: String,
            port: u16,
        ) -> ConnFuture<'_, Result<ByteStream, TunnelError>> {
            Box::pin(async move {
                if !self.is_connected() {
                    return Err(TunnelError::new("not established", FailureKind::Network));
                }
                let stream = TcpStream::connect((host.as_str(), port))
                    .await
                    .map_err(|err| TunnelError::new(err.to_string(), FailureKind::Network))?;
                Ok(Box::pin(stream) as ByteStream)
            })
        }
        fn open_shell(
            &self,
            size: PtySize,
            endpoint: PtyEndpoint,
            hold: Box<dyn Send + 'static>,
        ) -> ConnFuture<'_, Result<(), TunnelError>> {
            Box::pin(async move {
                if !self.is_connected() {
                    return Err(TunnelError::new("not established", FailureKind::Network));
                }
                if self.refuse_shell.load(Ordering::SeqCst) {
                    return Err(TunnelError::new(
                        "the server refused a pseudo-terminal",
                        FailureKind::Network,
                    ));
                }
                self.shells.fetch_add(1, Ordering::SeqCst);
                // The same shape as the real pump: two directions, the reference travelling
                // with the task, and the task ending when either end goes away.
                tokio::spawn(async move {
                    let _hold = hold;
                    let (out, mut input) = endpoint.split();
                    let greeting = format!("{}x{}$ ", size.cols, size.rows).into_bytes();
                    if out.send(PtyEvent::Data(greeting)).await.is_err() {
                        return;
                    }
                    loop {
                        let echo = tokio::select! {
                            next = input.next() => match next {
                                Some(PtyInput::Data(bytes)) => bytes,
                                Some(PtyInput::Resize(s)) => {
                                    format!("{}x{}", s.cols, s.rows).into_bytes()
                                }
                                None => break,
                            },
                            _ = out.closed() => break,
                        };
                        if out.send(PtyEvent::Data(echo)).await.is_err() {
                            return;
                        }
                    }
                    let _ = out.send(PtyEvent::Exit { code: Some(0) }).await;
                });
                Ok(())
            })
        }
        fn end(&self) -> ConnFuture<'_, ()> {
            Box::pin(async move {
                self.ended.fetch_add(1, Ordering::SeqCst);
                *self.state.lock().unwrap() = ConnState::Idle;
            })
        }
        fn exec(
            &self,
            _request: swiss_host::services::remote::RemoteExecRequest,
            events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
            cancel: swiss_host::services::action::CancelHandle,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<i32, TunnelError>> {
            Box::pin(async move {
                if !self.is_connected() {
                    return Err(TunnelError::new("not established", FailureKind::Network));
                }
                self.execs.fetch_add(1, Ordering::SeqCst);
                // Stream the canned output in small pieces, parking between them, so
                // a test can exercise mid-run observation and cancel.
                let payload = self.exec_output.lock().unwrap().clone();
                for chunk in payload.chunks(7) {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            drop(hold);
                            return Err(TunnelError::new(
                                "the remote command was canceled".to_string(),
                                FailureKind::Canceled,
                            ));
                        }
                        sent = events.send(
                            swiss_host::services::remote::RemoteExecEvent::Stdout(chunk.to_vec()),
                        ) => {
                            if sent.is_err() {
                                drop(hold);
                                return Err(TunnelError::new(
                                    "the run reading this output went away".to_string(),
                                    FailureKind::Canceled,
                                ));
                            }
                        }
                    }
                }
                drop(hold);
                Ok(self.exec_exit.load(Ordering::SeqCst))
            })
        }
        fn stat_file(
            &self,
            path: String,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>
        {
            Box::pin(async move {
                drop(hold);
                Ok(self.files.lock().unwrap().get(&path).map(|b| {
                    swiss_host::services::remote::RemoteFileStat {
                        size: b.len() as u64,
                        mtime_ms: Some(1_000),
                        is_dir: false,
                    }
                }))
            })
        }
        fn open_read(
            &self,
            path: String,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>
        {
            Box::pin(async move {
                let bytes = self
                    .files
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| {
                        TunnelError::new(format!("no such file: {path}"), FailureKind::Network)
                    })?;
                Ok(Box::new(FakeFileRead {
                    bytes,
                    at: 0,
                    _hold: Some(hold),
                })
                    as Box<dyn swiss_host::services::remote::RemoteRead>)
            })
        }
        fn create_file(
            &self,
            path: String,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>
        {
            Box::pin(async move {
                Ok(Box::new(FakeFileWrite {
                    files: self.files.clone(),
                    path,
                    buf: Vec::new(),
                    hold: Some(hold),
                })
                    as Box<dyn swiss_host::services::remote::RemoteWrite>)
            })
        }
        fn mkdir_p(
            &self,
            path: String,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<(), TunnelError>> {
            Box::pin(async move {
                drop(hold);
                self.files
                    .lock()
                    .unwrap()
                    .insert(format!("{path}/"), Vec::new());
                Ok(())
            })
        }
        fn list_dir(
            &self,
            path: String,
            hold: ConnectionLease,
        ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>>
        {
            Box::pin(async move {
                drop(hold);
                // The map holds files (and "path/" markers from mkdir); the
                // immediate children of the listed path are the first segments
                // of deeper keys.
                let files = self.files.lock().unwrap();
                let prefix = format!("{}/", path.trim_end_matches('/'));
                let mut names: Vec<String> = Vec::new();
                for key in files.keys() {
                    let Some(rest) = key.strip_prefix(prefix.as_str()) else {
                        continue;
                    };
                    if rest.is_empty() {
                        continue; // the directory's own marker, not a child
                    }
                    let name = rest.split('/').next().unwrap_or_default().to_string();
                    if !name.is_empty() && !names.contains(&name) {
                        names.push(name);
                    }
                }
                names.sort();
                Ok(names
                    .into_iter()
                    .map(|name| {
                        let child = format!("{prefix}{name}");
                        swiss_host::services::remote::RemoteListing {
                            is_dir: files.keys().any(|k| k.starts_with(&format!("{child}/"))),
                            size: files.get(&child).map(|b| b.len() as u64).unwrap_or(0),
                            name,
                        }
                    })
                    .collect())
            })
        }
    }

    /// The fake reader/writer halves behind the fake SFTP - chunked, lease-owning,
    /// shaped exactly like the real ones in ssh.rs.
    struct FakeFileRead {
        bytes: Vec<u8>,
        at: usize,
        _hold: Option<ConnectionLease>,
    }

    #[async_trait]
    impl swiss_host::services::remote::RemoteRead for FakeFileRead {
        async fn read_chunk(
            &mut self,
        ) -> Result<Option<Vec<u8>>, swiss_host::services::remote::RemoteError> {
            if self.at >= self.bytes.len() {
                self._hold.take(); // release once the stream truly ends
                return Ok(None);
            }
            let end =
                (self.at + swiss_host::services::remote::REMOTE_CHUNK_BYTES).min(self.bytes.len());
            let chunk = self.bytes[self.at..end].to_vec();
            self.at = end;
            Ok(Some(chunk))
        }
    }

    struct FakeFileWrite {
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        path: String,
        buf: Vec<u8>,
        hold: Option<ConnectionLease>,
    }

    #[async_trait]
    impl swiss_host::services::remote::RemoteWrite for FakeFileWrite {
        async fn write_chunk(
            &mut self,
            bytes: &[u8],
        ) -> Result<(), swiss_host::services::remote::RemoteError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
        async fn finish(&mut self) -> Result<(), swiss_host::services::remote::RemoteError> {
            self.files
                .lock()
                .unwrap()
                .insert(self.path.clone(), std::mem::take(&mut self.buf));
            self.hold.take(); // commit done, connection reference back
            Ok(())
        }
    }

    // --- harness ----------------------------------------------------------------------------

    /// A store on a fresh temp file. The tunnels file is sealed like every other state file, so
    /// the whole test binary is pinned to one deterministic master key rather than reaching for
    /// DPAPI or the machine id.
    fn scratch() -> (PathBuf, Arc<Mutex<TunnelStore>>) {
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-tmgr-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        let store = TunnelStore::new(dir.join("tunnels.json"), 19999);
        (dir, Arc::new(Mutex::new(store)))
    }

    fn manager(store: &Arc<Mutex<TunnelStore>>) -> (Arc<TunnelManager>, Built) {
        manager_with(store, None, None)
    }

    /// `seed` makes every client this manager builds fail its dials — a host that is simply down.
    fn manager_with(
        store: &Arc<Mutex<TunnelStore>>,
        mcps: Option<Box<dyn McpView>>,
        seed: Option<TunnelError>,
    ) -> (Arc<TunnelManager>, Built) {
        let built: Built = Arc::new(Mutex::new(Vec::new()));
        let sink = built.clone();
        let factory: ConnFactory = Arc::new(move |def: &SshConnDef, hooks: SshHooks| {
            let conn = FakeConn::new(def, hooks);
            if let Some(err) = &seed {
                *conn.fail_with.lock().unwrap() = Some(err.clone());
            }
            sink.lock().unwrap().push(conn.clone());
            conn as Arc<dyn SshLike>
        });
        (
            TunnelManager::with_connections(store.clone(), mcps, factory),
            built,
        )
    }

    fn built_len(built: &Built) -> usize {
        built.lock().unwrap().len()
    }

    fn nth(built: &Built, i: usize) -> Arc<FakeConn> {
        built.lock().unwrap()[i].clone()
    }

    /// Every dial this manager made, across however many clients it built.
    fn total_dials(built: &Built) -> u32 {
        built.lock().unwrap().iter().map(|c| c.dials()).sum()
    }

    fn conn_input() -> ConnInput {
        ConnInput {
            name: "srv".into(),
            host: "10.0.0.1".into(),
            port: 22.0,
            username: "deploy".into(),
            auth_type: AuthType::Key,
            key_path: Some("C:/keys/id_rsa".into()),
            ..Default::default()
        }
    }

    fn rule_input(name: &str, conn_id: &str, local: u16, target: u16) -> RuleInput {
        RuleInput {
            name: name.into(),
            connection_id: conn_id.into(),
            local_port: local as f64,
            target_host: "127.0.0.1".into(),
            target_port: target as f64,
            ..Default::default()
        }
    }

    fn add_conn(store: &Arc<Mutex<TunnelStore>>) -> SshConnDef {
        store
            .lock()
            .unwrap()
            .add_connection(&conn_input())
            .expect("add the connection")
    }

    fn add_rule(store: &Arc<Mutex<TunnelStore>>, input: RuleInput) -> RuleDef {
        store
            .lock()
            .unwrap()
            .add_rule(&input)
            .expect("add the rule")
    }

    fn stored_rule(store: &Arc<Mutex<TunnelStore>>, id: &str) -> RuleDef {
        store.lock().unwrap().rule(id).expect("the stored rule")
    }

    fn row_of(m: &Arc<TunnelManager>, id: &str) -> Value {
        m.rule_row(id).expect("a row for the rule")
    }

    fn state_of(m: &Arc<TunnelManager>, id: &str) -> String {
        row_of(m, id)
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn field(row: &Value, key: &str) -> Value {
        row.get(key).cloned().unwrap_or(Value::Null)
    }

    /// A local TCP server that echoes what it receives, upper-cased — the "remote service" the
    /// tunnel carries traffic to. It lives until the test binary exits, which is what a service
    /// on the far side of a tunnel does anyway.
    async fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
        let port = listener.local_addr().expect("echo addr").port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                let up = buf[..n].to_ascii_uppercase();
                                if sock.write_all(&up).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        port
    }

    /// A port nothing holds right now — bound and released, the way the Node suite picked one.
    async fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind probe");
        listener.local_addr().expect("probe addr").port()
    }

    /// Hold a port the way a foreign process would, so a start has to fail on it.
    async fn squat() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind squatter");
        let port = listener.local_addr().expect("squatter addr").port();
        (listener, port)
    }

    async fn round_trip(port: u16, text: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect to the tunnel");
        c.write_all(text.as_bytes()).await.expect("write");
        let mut buf = vec![0u8; text.len()];
        c.read_exact(&mut buf).await.expect("read the echo back");
        String::from_utf8(buf).expect("utf-8 echo")
    }

    async fn port_is_free(port: u16) -> bool {
        crate::tunnel::port::probe_port(port, "127.0.0.1").await
    }

    /// Poll until `cond` holds. Recovery runs on spawned tasks, so a state change lands a few
    /// scheduler turns — and, for a reconnect, one backoff delay — after the event behind it.
    async fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if cond() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The MCP registry view the linkage tests run against.
    struct View {
        started: Vec<String>,
    }

    impl View {
        fn boxed(started: &[&str]) -> Option<Box<dyn McpView>> {
            Some(Box::new(View {
                started: started.iter().map(|s| s.to_string()).collect(),
            }))
        }
    }

    impl McpView for View {
        fn has(&self, name: &str) -> bool {
            matches!(name, "pg-analytics" | "redis-b-6380")
        }
        fn state_of(&self, name: &str) -> Option<String> {
            Some(if self.is_started(name) {
                "up".into()
            } else {
                "stopped".into()
            })
        }
        fn is_started(&self, name: &str) -> bool {
            self.started.iter().any(|s| s == name)
        }
        fn started_at(&self, name: &str) -> Option<String> {
            self.is_started(name)
                .then(|| "2026-08-13T10:00:00.000Z".to_string())
        }
    }

    // --- start and stop ---------------------------------------------------------------------

    #[tokio::test]
    async fn binds_the_port_carries_traffic_and_frees_the_port_on_stop() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("start");
        assert_eq!(state_of(&m, &r.id), "up");
        assert_eq!(round_trip(port, "ping").await, "PING");
        assert!(stored_rule(&store, &r.id).enabled);

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        assert!(!stored_rule(&store, &r.id).enabled);
    }

    /// docs/27 §2.6.6: a proxied connection through the REAL manager and the real russh
    /// client — start, one byte round trip riding proxy -> ssh server -> direct-tcpip ->
    /// echo, then stop releases the local port. The existing port-binding invariant,
    /// exercised on the proxied path.
    #[tokio::test]
    async fn a_proxied_connection_forwards_through_the_manager_lifecycle() {
        let ssh_port = crate::tunnel::sshtest::spawn_ssh_server().await;
        let (proxy_port, mut obs) =
            crate::tunnel::sshtest::spawn_socks5_proxy(crate::tunnel::sshtest::SocksAuth::None, 0)
                .await;
        let echo = echo_server().await;
        let (dir, store) = scratch();
        let mut input = ConnInput {
            host: "localhost".into(),
            port: ssh_port as f64,
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            proxy: Some(format!("socks5://127.0.0.1:{proxy_port}")),
            ..conn_input()
        };
        input.key_path = None; // password auth has no key
        let c = store
            .lock()
            .unwrap()
            .add_connection(&input)
            .expect("add the proxied connection");
        let port = free_port().await;
        let r = add_rule(&store, rule_input("via-proxy", &c.id, port, echo));
        let m = TunnelManager::new(store.clone(), None);

        m.start_rule(&r.id).await.expect("start through the proxy");
        assert_eq!(state_of(&m, &r.id), "up");
        // The SSH session itself must have gone through the proxy — without this, a dialer
        // that silently ignored the proxy field would still pass on the direct route.
        let o = obs.recv().await.expect("the proxy saw the ssh dial");
        assert_eq!(
            (o.atyp, o.host.as_slice(), o.port),
            (0x03, b"localhost".as_slice(), ssh_port)
        );
        assert_eq!(round_trip(port, "proxied").await, "PROXIED");

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn reports_the_holder_when_the_local_port_is_taken_by_someone_else() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let (squatter, port) = squat().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        let err = m.start_rule(&r.id).await.expect_err("the port is taken");
        assert!(err.message().contains("local port"), "{}", err.message());
        let row = row_of(&m, &r.id);
        assert_eq!(field(&row, "state"), json!("error"));
        let reason = field(&row, "reason")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(reason.contains(&port.to_string()), "{reason}");
        // Only Windows can name the holder (port.rs has no POSIX lookup), and there the holder
        // is this very test process — so the manager first reclaims what it thinks is its own
        // stale listener, fails again, and reports the pid rather than a bare EADDRINUSE.
        #[cfg(windows)]
        {
            assert!(reason.contains("held by pid"), "{reason}");
            assert_eq!(
                field(&row, "portOwner").get("pid").cloned(),
                Some(json!(std::process::id()))
            );
        }
        drop(squatter);
    }

    #[tokio::test]
    async fn fails_a_rule_whose_connection_is_unknown_without_touching_the_port() {
        let (dir, seed) = scratch();
        let echo = echo_server().await;
        let c = add_conn(&seed);
        let port = free_port().await;
        let mut r = add_rule(&seed, rule_input("orphan", &c.id, port, echo));
        // Break the link the way a hand-edited tunnels.json does — `remove_connection` refuses
        // while a rule still points at one, so the file is written directly and read back. A
        // rule whose connection is unknown is KEPT on load: one bad hand-edit must not silently
        // discard the other 17 rules, so the manager has to report it rather than crash.
        r.connection_id = "gone".into();
        seed.lock()
            .unwrap()
            .replace_all(Vec::new(), vec![r.clone()])
            .expect("hand-edit the file");
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        assert_eq!(
            store.lock().unwrap().rules().len(),
            1,
            "the rule survived load"
        );
        let (m, built) = manager(&store);

        let err = m.start_rule(&r.id).await.expect_err("no connection");
        assert!(
            err.message().contains("unknown SSH connection"),
            "{}",
            err.message()
        );
        assert_eq!(state_of(&m, &r.id), "error");
        assert!(port_is_free(port).await);
        assert_eq!(built_len(&built), 0, "it must not dial anything");
    }

    #[tokio::test]
    async fn is_idempotent_a_second_start_is_a_no_op_and_so_is_a_second_stop() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("first start");
        m.start_rule(&r.id).await.expect("second start");
        assert_eq!(nth(&built, 0).dials(), 1);

        m.stop_rule(&r.id, true, false).await.expect("first stop");
        m.stop_rule(&r.id, true, false).await.expect("second stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
    }

    #[tokio::test]
    async fn serializes_a_start_and_a_stop_that_arrive_together() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        let (a, b, d) = tokio::join!(
            m.start_rule(&r.id),
            m.stop_rule(&r.id, true, false),
            m.start_rule(&r.id)
        );
        let _ = (a, b, d);
        // Whatever order they landed in, the reported state and the actual port agree.
        let up = state_of(&m, &r.id) == "up";
        assert_eq!(up, !port_is_free(port).await);
        m.close_all().await;
    }

    // --- connection sharing -----------------------------------------------------------------

    #[tokio::test]
    async fn dials_once_for_many_rules_and_ends_the_client_when_the_last_one_stops() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut rules = Vec::new();
        for i in 0..3 {
            let port = free_port().await;
            rules.push(add_rule(
                &store,
                rule_input(&format!("r{i}"), &c.id, port, echo),
            ));
        }

        let results = m.start_all().await;
        assert!(results.iter().all(|x| x.ok), "{results:?}");
        assert_eq!(built_len(&built), 1);
        assert_eq!(nth(&built, 0).dials(), 1);
        assert_eq!(nth(&built, 0).ref_count(), 3);
        // The panel's connection row reads the live client through the same seam.
        let rows = m.rows();
        let conn_row = &rows["connections"][0];
        assert_eq!(conn_row["state"], json!("connected"));
        assert_eq!(conn_row["ruleCount"], json!(3));
        assert_eq!(conn_row["activeRules"], json!(3));

        m.stop_rule(&rules[0].id, true, false)
            .await
            .expect("stop 0");
        assert_eq!(nth(&built, 0).ended(), 0, "two rules still use it");
        m.stop_rule(&rules[1].id, true, false)
            .await
            .expect("stop 1");
        m.stop_rule(&rules[2].id, true, false)
            .await
            .expect("stop 2");
        assert_eq!(nth(&built, 0).ref_count(), 0);
        assert_eq!(nth(&built, 0).ended(), 1);
    }

    /// docs/27 §4 addendum: the connection row is the panel's READ surface for the
    /// proxy/jump fields — the sheet's echo and the row badges render from rows(), not
    /// from the POST/PUT response the panel reads once and drops. Plaintext,
    /// absent-when-unset, for proxy/proxyUsername/jump (the jump is an id; the panel
    /// resolves the name from this same list) and the MASK sentinel for a set
    /// proxyPassword — an env ref passes, the reference is not the secret — so an
    /// untouched edit echoes the sentinel back through unmask_conn and keeps the stored
    /// secret. The new keys append after the existing ones; the historical prefix is
    /// frozen. The connection's key path follows the same rule — the edit sheet prefills
    /// from this row, so a custom path must ride along or a save would rewrite it to the
    /// default.
    #[test]
    fn connection_rows_carry_the_proxy_jump_and_key_path_fields_for_the_panel() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let _plain = add_conn(&store);
        let _clash = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "clash".into(),
                host: "10.0.0.2".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                proxy: Some("socks5://127.0.0.1".into()), // portless: stored as :1080
                proxy_username: Some("pxuser".into()),
                proxy_password: Some("px-pass".into()),
                ..Default::default()
            })
            .expect("the proxied connection");
        let bastion = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "bastion".into(),
                host: "10.0.0.3".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                ..Default::default()
            })
            .expect("the jump host");
        let _db = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "db".into(),
                host: "10.0.0.9".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                jump: Some(bastion.id.clone()),
                ..Default::default()
            })
            .expect("the jumping connection");
        let _refconn = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "refproxy".into(),
                host: "10.0.0.4".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Password,
                password: Some("p".into()),
                proxy: Some("http://proxy.lan".into()),
                proxy_password: Some("${secret://proxy-pass}".into()),
                ..Default::default()
            })
            .expect("the env-ref connection");
        let _keyed = store
            .lock()
            .unwrap()
            .add_connection(&ConnInput {
                name: "keyed".into(),
                host: "10.0.0.5".into(),
                port: 22.0,
                username: "u".into(),
                auth_type: AuthType::Key,
                key_path: Some("~/.ssh/alt_ed25519".into()),
                ..Default::default()
            })
            .expect("the keyed connection");

        let rows = m.rows();
        let by_name = |n: &str| {
            rows["connections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == json!(n))
                .unwrap()
                .clone()
        };
        let keys = |c: &Value| c.as_object().unwrap().keys().cloned().collect::<Vec<_>>();

        // A plain row keeps the exact historical shape: nothing new appears. bastion is
        // the plain case here — password auth, no proxy, no jump, no key path; the shared
        // add_conn helper happens to carry a key path, so it no longer qualifies.
        let plain_row = by_name("bastion");
        assert_eq!(
            keys(&plain_row),
            vec![
                "id",
                "name",
                "host",
                "port",
                "username",
                "authType",
                "state",
                "ruleCount",
                "activeRules",
            ]
        );
        assert_eq!(plain_row["id"], json!(bastion.id));

        // The proxy trio: plaintext URL and username, the sentinel — never the secret.
        let clash_row = by_name("clash");
        assert_eq!(clash_row["proxy"], json!("socks5://127.0.0.1:1080"));
        assert_eq!(clash_row["proxyUsername"], json!("pxuser"));
        assert_eq!(clash_row["proxyPassword"], json!(swiss_host::mask::MASK));
        assert!(clash_row.get("jump").is_none());
        // Appended after the frozen prefix, in the §1.2 field order.
        let clash_keys = keys(&clash_row);
        assert_eq!(clash_keys[..9], keys(&plain_row)[..]);
        assert_eq!(
            clash_keys[9..],
            vec!["proxy", "proxyUsername", "proxyPassword"]
        );

        // The jump: a plaintext id, and no proxy keys on a jumping row.
        let db_row = by_name("db");
        assert_eq!(db_row["jump"], json!(bastion.id));
        assert!(db_row.get("proxy").is_none());
        assert!(db_row.get("proxyPassword").is_none());

        // An env-ref proxy password is a reference, not a secret: it rides as itself.
        let ref_row = by_name("refproxy");
        assert_eq!(ref_row["proxyPassword"], json!("${secret://proxy-pass}"));

        // The key path: absent-when-unset on a plain row, present and plaintext on a keyed
        // one, appended after the frozen prefix — the edit sheet prefills from this row, so
        // a save must never rewrite a custom path to the default.
        let keyed_row = by_name("keyed");
        assert_eq!(keyed_row["keyPath"], json!("~/.ssh/alt_ed25519"));
        assert!(plain_row.get("keyPath").is_none());
        let keyed_keys = keys(&keyed_row);
        assert_eq!(keyed_keys[..9], keys(&plain_row)[..]);
        assert_eq!(keyed_keys[9..], vec!["keyPath"]);
    }

    #[tokio::test]
    async fn does_not_double_count_a_reference_when_a_rule_is_started_twice() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("first start");
        m.start_rule(&r.id).await.expect("second start");
        assert_eq!(nth(&built, 0).ref_count(), 1);

        m.stop_rule(&r.id, true, false).await.expect("stop");
        assert_eq!(nth(&built, 0).ended(), 1);
    }

    // --- transport loss ---------------------------------------------------------------------

    #[tokio::test]
    async fn releases_the_port_and_schedules_a_reconnect_when_auto_reconnect_is_on() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        // The port must be free while the tunnel is down — this is the whole point.
        assert!(port_is_free(port).await);

        assert!(
            wait_until(Duration::from_secs(10), || state_of(&m, &r.id) == "up").await,
            "the retry never brought it back up"
        );
        assert!(!port_is_free(port).await);
        assert!(field(&row_of(&m, &r.id), "reconnectedAt").is_string());
        assert_eq!(round_trip(port, "back").await, "BACK");
        // The dead client was ended and released; the retry dialed a fresh one.
        assert_eq!(nth(&built, 0).ended(), 1);
        assert_eq!(built_len(&built), 2);
        m.close_all().await;
    }

    #[tokio::test]
    async fn does_not_schedule_a_reconnect_for_an_auth_failure() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Auth);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id) == "error").await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(port_is_free(port).await);
        // Past the interval: hammering an auth failure every 10s earns a fail2ban ban, so
        // nothing may retry — not even once.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(built_len(&built), 1, "a retry would have built a client");
        assert_eq!(nth(&built, 0).dials(), 1);
    }

    #[tokio::test]
    async fn leaves_a_rule_in_error_port_released_when_auto_reconnect_is_off() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id) == "error").await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(port_is_free(port).await);
        let reason = field(&row_of(&m, &r.id), "reason")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(reason.contains("lost"), "{reason}");
    }

    #[tokio::test]
    async fn brings_every_rule_on_a_shared_connection_down_together() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut ports = Vec::new();
        let mut ids = Vec::new();
        for i in 0..2 {
            let port = free_port().await;
            ids.push(add_rule(&store, rule_input(&format!("r{i}"), &c.id, port, echo)).id);
            ports.push(port);
        }
        m.start_all().await;

        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || ids
                .iter()
                .all(|id| state_of(&m, id) == "error"))
            .await,
            "one rule stayed up under a dead transport"
        );
        for port in ports {
            assert!(port_is_free(port).await);
        }
    }

    // --- reconnect policy -------------------------------------------------------------------

    #[tokio::test]
    async fn does_not_retry_a_port_failure() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let (squatter, port) = squat().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect_err("the port is taken");
        assert_eq!(state_of(&m, &r.id), "error", "not 'reconnecting'");
        // Whatever the start itself spent (on Windows it reclaims what looks like its own stale
        // listener and tries once more), nothing may happen after the failure is reported: a
        // port held by someone else will not free itself, so retrying only thrashes — and runs
        // netstat + tasklist each attempt. Wait well past the interval.
        let (clients, dials) = (built_len(&built), total_dials(&built));
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(
            built_len(&built),
            clients,
            "a retry would have built a client"
        );
        assert_eq!(total_dials(&built), dials, "the failed start, nothing more");
        drop(squatter);
    }

    #[tokio::test(start_paused = true)]
    async fn spaces_network_reconnects_out_with_backoff_and_never_gives_up() {
        // On a paused clock: the retries below are scheduled against tokio's timer, so the
        // suite pays microseconds for what is a quarter of an hour of simulated outage.
        let (_dir, store) = scratch();
        let (m, built) = manager_with(
            &store,
            None,
            Some(TunnelError::new("host unreachable", FailureKind::Network)),
        );
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, 9)
            },
        );
        let _ = port;

        let t0 = tokio::time::Instant::now();
        m.start_rule(&r.id).await.expect_err("every dial fails");
        assert_eq!(state_of(&m, &r.id), "reconnecting");
        // A failed dial leaves the client in the map with no references, so every retry lands on
        // the same one: its dial count IS the number of attempts.
        assert!(
            wait_until(Duration::from_secs(20), || nth(&built, 0).dials() >= 5).await,
            "only {} attempts",
            nth(&built, 0).dials()
        );
        let elapsed = t0.elapsed();
        // 1s, 2s, 4s, 8s (±20% jitter) — at least 12s of simulated time for four retries. A flat
        // `reconnectInterval` loop would have spent 4s and dialed a host that is down 15 times.
        assert!(
            elapsed >= Duration::from_secs(11),
            "four retries took only {elapsed:?} — that is a storm, not backoff"
        );
        assert!(elapsed <= Duration::from_secs(25), "{elapsed:?}");
        assert_eq!(
            state_of(&m, &r.id),
            "reconnecting",
            "a network outage is retried forever"
        );
        m.close_all().await;
    }

    // --- edits ------------------------------------------------------------------------------

    #[tokio::test]
    async fn restarts_a_running_rule_on_the_new_port_and_frees_the_old_one() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let old_port = free_port().await;
        let new_port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, old_port, echo));
        m.start_rule(&r.id).await.expect("start");

        m.apply_rule_update(&r.id, &rule_input("pg", &c.id, new_port, echo))
            .await
            .expect("edit");
        assert_eq!(state_of(&m, &r.id), "up");
        assert!(port_is_free(old_port).await);
        assert_eq!(round_trip(new_port, "hi").await, "HI");
        m.close_all().await;
    }

    #[tokio::test]
    async fn leaves_a_stopped_rule_stopped_after_an_edit() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.apply_rule_update(&r.id, &rule_input("pg2", &c.id, port, echo))
            .await
            .expect("edit");
        assert_eq!(state_of(&m, &r.id), "stopped");
        assert!(port_is_free(port).await);
        assert_eq!(built_len(&built), 0, "an edit must not dial");
    }

    #[tokio::test]
    async fn restarts_only_the_rules_that_were_running_when_a_connection_is_edited() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let p1 = free_port().await;
        let p2 = free_port().await;
        let running = add_rule(&store, rule_input("on", &c.id, p1, echo));
        let stopped = add_rule(&store, rule_input("off", &c.id, p2, echo));
        m.start_rule(&running.id).await.expect("start");

        m.apply_connection_update(
            &c.id,
            &ConnInput {
                username: "someone-else".into(),
                ..conn_input()
            },
        )
        .await
        .expect("edit the connection");

        assert_eq!(state_of(&m, &running.id), "up");
        assert_eq!(
            state_of(&m, &stopped.id),
            "stopped",
            "a rule the user stopped must not come up because a neighbour was edited"
        );
        // The old client was ended and a new one dialed with the new definition.
        assert_eq!(built_len(&built), 2);
        assert_eq!(nth(&built, 0).ended(), 1);
        assert_eq!(nth(&built, 1).username(), "someone-else");
        m.close_all().await;
    }

    #[tokio::test]
    async fn refuses_to_delete_a_connection_with_rules_and_succeeds_once_they_are_gone() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        // Structured, like rule deletion — the API answers 409 + a confirm, not a flat string.
        match m.delete_connection(&c.id).await {
            Err(OpError::Dependents(names)) => assert_eq!(names, vec!["pg".to_string()]),
            other => panic!("expected a Dependents refusal, got {other:?}"),
        }
        m.delete_rule(&r.id, false).await.expect("delete the rule");
        m.delete_connection(&c.id)
            .await
            .expect("delete the connection");
        assert!(store.lock().unwrap().is_empty());
    }

    // --- jump chains (docs/27 §3) -------------------------------------------------------------

    /// Every REAL client this manager built, newest last — the jump tests' window into
    /// the shared connection table (refcounts, live state) without opening the manager up.
    type RealBuilt = Arc<Mutex<Vec<Arc<SshConnection>>>>;

    /// A manager over real russh clients, recorded as they are built: the fake-client
    /// harness cannot prove SSH-over-SSH, so the §3.4 tests run the genuine stack against
    /// the in-process servers and only observe through this list.
    fn real_manager(store: &Arc<Mutex<TunnelStore>>) -> (Arc<TunnelManager>, RealBuilt) {
        let built: RealBuilt = Arc::new(Mutex::new(Vec::new()));
        let sink = built.clone();
        let factory: ConnFactory = Arc::new(move |def: &SshConnDef, hooks: SshHooks| {
            let conn = SshConnection::new(def.clone(), hooks);
            sink.lock().unwrap().push(conn.clone());
            conn as Arc<dyn SshLike>
        });
        (
            TunnelManager::with_connections(store.clone(), None, factory),
            built,
        )
    }

    fn real_conns_with(built: &RealBuilt, id: &str) -> Vec<Arc<SshConnection>> {
        built
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.id() == id)
            .cloned()
            .collect()
    }

    /// The refcount of the one client built for this id — the hold assertions' shape.
    fn refs_of(built: &RealBuilt, id: &str) -> i64 {
        let conns = real_conns_with(built, id);
        assert_eq!(conns.len(), 1, "exactly one client for {id}");
        conns[0].refs.load(Ordering::SeqCst)
    }

    fn add_conn_with(store: &Arc<Mutex<TunnelStore>>, input: ConnInput) -> SshConnDef {
        store
            .lock()
            .unwrap()
            .add_connection(&input)
            .expect("add the connection")
    }

    /// A password-auth def pointing at a test server — the servers accept any
    /// credentials, and password auth keeps these tests off the filesystem (key paths).
    fn jump_pw_conn(name: &str, host: &str, port: u16, jump: Option<&str>) -> ConnInput {
        ConnInput {
            name: name.into(),
            host: host.into(),
            port: port as f64,
            username: "u".into(),
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            jump: jump.map(str::to_string),
            ..Default::default()
        }
    }

    /// docs/27 §3.4.1: connection A dials the jump server S1, connection B rides A's
    /// direct-tcpip channel to reach the target server S2, and the rule's traffic flows
    /// through both — the full SSH-over-SSH path, pinned by the jump's live client
    /// (built, connected, held) and the echoed bytes.
    #[tokio::test]
    async fn a_jump_connection_rides_the_jump_servers_channel_end_to_end() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &b.id, port, echo));

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(state_of(&m, &r.id), "up");
        // The jump really carried the target: its shared client is live, and exactly one
        // reference is out — the target's hold on its hop.
        let a_clients = real_conns_with(&built, &a.id);
        assert_eq!(a_clients.len(), 1, "the jump dialed once");
        assert_eq!(a_clients[0].state(), ConnState::Connected);
        assert_eq!(a_clients[0].refs.load(Ordering::SeqCst), 1);
        assert_eq!(round_trip(port, "hi").await, "HI");
        m.close_all().await;
    }

    /// docs/27 §3.4.2: a two-level chain proves both the recursion (each hop dials
    /// through the one below it) and the hold discipline — stopping the target unwinds
    /// the whole chain, leaving every hop's refcount at zero.
    #[tokio::test]
    async fn a_two_level_chain_releases_every_hop_when_the_target_stops() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let s3 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("hop-a", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("hop-b", "127.0.0.1", s2, Some(&a.id)));
        let c = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s3, Some(&b.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(round_trip(port, "chain").await, "CHAIN");
        // One reference per hop: hop-b's session holds a, db's session holds hop-b, and
        // the rule holds db.
        assert_eq!(refs_of(&built, &a.id), 1, "held by hop-b's session");
        assert_eq!(refs_of(&built, &b.id), 1, "held by db's session");
        assert_eq!(refs_of(&built, &c.id), 1, "held by the rule");

        m.stop_rule(&r.id, true, false)
            .await
            .expect("stop the target");
        // The unwind is async (each release ends the client that holds the next hop);
        // every hop must land at zero — a leaked hold anywhere leaves the hop below live.
        assert!(
            wait_until(Duration::from_secs(5), || {
                [a.id.as_str(), b.id.as_str(), c.id.as_str()]
                    .into_iter()
                    .all(|id| {
                        real_conns_with(&built, id)
                            .iter()
                            .all(|c| c.refs.load(Ordering::SeqCst) == 0)
                    })
            })
            .await,
            "a hop kept a reference after the chain unwound"
        );
        m.close_all().await;
    }

    /// docs/27 §3.4.3: killing the jump server takes the whole chain down — the target's
    /// own watcher reports the loss and releases the local port — and a restarted jump
    /// on the same port lets auto-reconnect rebuild the chain.
    #[tokio::test]
    async fn a_lost_jump_releases_the_port_and_a_restarted_jump_recovers_the_chain() {
        let (_dir, store) = scratch();
        let (m, _built) = real_manager(&store);
        let s1 = sshtest::spawn_killable_ssh_server(sshtest::TestAuth::AcceptAll);
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(
            &store,
            jump_pw_conn("bastion", "127.0.0.1", s1.port(), None),
        );
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &b.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("the chained start");
        assert_eq!(round_trip(port, "up").await, "UP");

        s1.kill().await;
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "the chain stayed {} after the jump died",
            state_of(&m, &r.id)
        );
        assert!(
            port_is_free(port).await,
            "the port goes down with the chain"
        );

        s1.restart().await;
        assert!(
            wait_until(Duration::from_secs(15), || state_of(&m, &r.id) == "up").await,
            "the retry never rebuilt the chain"
        );
        assert_eq!(round_trip(port, "back").await, "BACK");
        m.close_all().await;
    }

    /// docs/27 §3.4.4: a jump whose credentials are refused is an auth failure of the
    /// target — never retried — and the message names the hop it failed on.
    #[tokio::test]
    async fn a_bad_jump_password_is_an_auth_failure_named_after_the_hop_and_never_retried() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s_bad = sshtest::spawn_ssh_server_with(sshtest::TestAuth::RejectAll).await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s_bad, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &b.id, port, echo)
            },
        );

        let err = m
            .start_rule(&r.id)
            .await
            .expect_err("the jump refuses the credentials");
        assert!(
            err.message().contains("via bastion:"),
            "names the hop: {}",
            err.message()
        );
        assert_eq!(state_of(&m, &r.id), "error", "auth is never retried");
        // The kind itself, pinned where it is still structured: the throwaway Test
        // client reports the same failure with kind == auth.
        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["kind"], json!("auth"), "{res}");
        assert!(
            res["error"]
                .as_str()
                .unwrap_or_default()
                .contains("via bastion:"),
            "{res}"
        );
        // Past the reconnect interval: nothing may have retried the refused credentials.
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(
            real_conns_with(&built, &a.id).len(),
            1,
            "the jump was dialed exactly once"
        );
        m.close_all().await;
    }

    /// docs/27 §3.4.5: editing a jump reconnects everything that rides it, and deleting
    /// one is refused while a dependent exists — the error names the dependent.
    #[tokio::test]
    async fn editing_a_jump_reconnects_its_dependents_and_delete_lists_them() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_ssh_server().await;
        let s2 = sshtest::spawn_ssh_server().await;
        let echo = echo_server().await;
        let a = add_conn_with(&store, jump_pw_conn("bastion", "127.0.0.1", s1, None));
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &b.id, port, echo));
        m.start_rule(&r.id).await.expect("the chained start");

        m.apply_connection_update(
            &a.id,
            &ConnInput {
                username: "someone-else".into(),
                ..jump_pw_conn("bastion", "127.0.0.1", s1, None)
            },
        )
        .await
        .expect("edit the jump");

        assert_eq!(
            state_of(&m, &r.id),
            "up",
            "the dependent came back with the jump"
        );
        assert_eq!(round_trip(port, "after").await, "AFTER");
        // A fresh jump client was built and the old one ended; the dependent redialed
        // through the fresh one.
        let a_clients = real_conns_with(&built, &a.id);
        assert!(a_clients.len() >= 2, "the edit redialed the jump");
        assert!(!a_clients[0].connected(), "the pre-edit client is gone");
        assert!(a_clients.last().unwrap().connected());

        // Deleting the jump is refused while a dependent references it — a plain error
        // naming the dependent (no confirm/force path: the chain would dangle).
        match m.delete_connection(&a.id).await {
            Err(OpError::Msg(msg)) => assert!(msg.contains("db"), "{msg}"),
            other => panic!("expected a plain refusal naming the dependent, got {other:?}"),
        }
        // With the dependent gone the delete goes through.
        m.stop_rule(&r.id, true, true).await.expect("stop");
        m.delete_rule(&r.id, false).await.expect("delete the rule");
        m.delete_connection(&b.id)
            .await
            .expect("delete the dependent");
        m.delete_connection(&a.id).await.expect("delete the jump");
        assert!(store.lock().unwrap().is_empty());
    }

    /// docs/27 §3.2: POST /connections/:id/test rides a PRIVATE throwaway chain — one
    /// one-off client per hop, nothing pinned in the shared table, no host key learned —
    /// and a failing hop is named in the error.
    #[tokio::test]
    async fn testing_a_jump_connection_uses_a_throwaway_chain_and_names_the_hop_on_failure() {
        let (_dir, store) = scratch();
        let (m, built) = real_manager(&store);
        let s1 = sshtest::spawn_killable_ssh_server(sshtest::TestAuth::AcceptAll);
        let s2 = sshtest::spawn_ssh_server().await;
        let a = add_conn_with(
            &store,
            jump_pw_conn("bastion", "127.0.0.1", s1.port(), None),
        );
        let b = add_conn_with(&store, jump_pw_conn("db", "127.0.0.1", s2, Some(&a.id)));

        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["ok"], json!(true), "{res}");
        assert_eq!(built.lock().unwrap().len(), 0, "no shared client was built");
        assert_eq!(m.conn_state(&b.id), ConnState::Idle);
        // A successful Test of a keyless hop stores nothing (the TOFU note, §3.2).
        assert!(store
            .lock()
            .unwrap()
            .connection(&a.id)
            .unwrap()
            .host_key
            .is_none());

        s1.kill().await;
        let res = m.test_connection(&b.id).await.expect("the test call");
        assert_eq!(res["ok"], json!(false), "{res}");
        assert_eq!(res["kind"], json!("network"), "{res}");
        assert!(
            res["error"]
                .as_str()
                .unwrap_or_default()
                .contains("via bastion:"),
            "{res}"
        );
    }

    #[tokio::test]
    async fn frees_the_port_when_a_running_rule_is_deleted() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));
        m.start_rule(&r.id).await.expect("start");

        m.delete_rule(&r.id, false).await.expect("delete");
        assert!(port_is_free(port).await);
        assert!(store.lock().unwrap().rules().is_empty());
    }

    // --- MCP linkage ------------------------------------------------------------------------

    #[tokio::test]
    async fn blocks_a_stop_while_a_linked_mcp_is_started_and_obeys_force() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");

        match m.stop_rule(&r.id, true, false).await {
            Err(OpError::Dependents(names)) => {
                assert_eq!(names, vec!["pg-analytics".to_string()])
            }
            other => panic!("expected a Dependents refusal, got {other:?}"),
        }
        assert_eq!(
            state_of(&m, &r.id),
            "up",
            "not stopped behind the user's back"
        );
        m.stop_rule(&r.id, true, true).await.expect("forced stop");
        assert_eq!(state_of(&m, &r.id), "stopped");
    }

    #[tokio::test]
    async fn does_not_block_when_the_linked_mcp_is_not_running() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&[]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");
        m.stop_rule(&r.id, true, false).await.expect("stop");
    }

    #[tokio::test]
    async fn collects_every_dependent_into_one_error_for_stop_all() {
        let (_dir, store) = scratch();
        let (m, _built) =
            manager_with(&store, View::boxed(&["pg-analytics", "redis-b-6380"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let pa = free_port().await;
        let pb = free_port().await;
        add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("a", &c.id, pa, echo)
            },
        );
        add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["redis-b-6380".into()]),
                ..rule_input("b", &c.id, pb, echo)
            },
        );
        m.start_all().await;

        let mut blocked = m.stop_all(false).await.expect_err("both are in use");
        blocked.sort();
        assert_eq!(blocked, vec!["pg-analytics", "redis-b-6380"]);

        let results = m.stop_all(true).await.expect("forced stop-all");
        assert!(results.iter().all(|x| x.ok), "{results:?}");
    }

    #[tokio::test]
    async fn reports_link_state_and_an_unknown_mcp_name_honestly() {
        let (_dir, store) = scratch();
        let (m, _built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                mcps: Some(vec!["pg-analytics".into(), "ghost".into()]),
                ..rule_input("pg", &c.id, port, 5432)
            },
        );

        assert_eq!(
            field(&row_of(&m, &r.id), "mcpRows"),
            json!([
                { "name": "pg-analytics", "state": "up", "known": true },
                { "name": "ghost", "state": "stopped", "known": false },
            ])
        );
    }

    #[tokio::test]
    async fn flags_a_stale_pool_only_when_the_reconnect_came_after_the_mcp_started() {
        let (_dir, store) = scratch();
        let (m, built) = manager_with(&store, View::boxed(&["pg-analytics"]), None);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                mcps: Some(vec!["pg-analytics".into()]),
                ..rule_input("pg", &c.id, port, echo)
            },
        );
        m.start_rule(&r.id).await.expect("start");
        assert_eq!(
            m.tunnels_for_mcp("pg-analytics")[0]["stalePool"],
            json!(false)
        );

        nth(&built, 0).die(FailureKind::Network);
        // Down first, then up again — without the first wait this races the spawned recovery
        // and reads the state the rule still had before the transport died.
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(
            wait_until(Duration::from_secs(10), || state_of(&m, &r.id) == "up").await,
            "the retry never brought it back up"
        );
        // The MCP started at 10:00 on 2026-08-13; this reconnect is now, so its pool may still
        // hold sockets of the dead session.
        assert_eq!(
            m.tunnels_for_mcp("pg-analytics")[0]["stalePool"],
            json!(true)
        );
        m.close_all().await;
    }

    // --- boot and shutdown ------------------------------------------------------------------

    #[tokio::test]
    async fn starts_only_enabled_rules_and_never_throws_when_one_fails() {
        let (_dir, store) = scratch();
        let (m, _built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let good = add_rule(&store, rule_input("good", &c.id, free_port().await, echo));
        let off = add_rule(&store, rule_input("off", &c.id, free_port().await, echo));
        let (squatter, taken) = squat().await;
        let bad = add_rule(&store, rule_input("bad", &c.id, taken, echo));
        {
            let mut s = store.lock().unwrap();
            s.set_enabled(&good.id, true).expect("enable good");
            s.set_enabled(&bad.id, true).expect("enable bad");
        }

        let results = m.start_enabled().await;
        let mut names: Vec<&str> = results.iter().map(|x| x.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["bad", "good"]);
        assert!(results.iter().any(|x| x.name == "good" && x.ok));
        assert!(results.iter().any(|x| x.name == "bad" && !x.ok));
        assert_eq!(state_of(&m, &off.id), "stopped");
        drop(squatter);
        m.close_all().await;
    }

    /// The tunnels plugin's Restart, clicked twice in a row: the first boot task is still
    /// starting rules when close_all runs. Every rule it starts AFTER close_all passed it used
    /// to keep a listening forward on the client close_all then ended — reported "up", every
    /// connection through it refused with "ssh connection is not established".
    #[tokio::test]
    async fn a_start_that_lands_during_or_after_close_all_is_refused_not_leaked() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut rules = Vec::new();
        for i in 0..6 {
            let r = add_rule(
                &store,
                rule_input(&format!("r{i}"), &c.id, free_port().await, echo),
            );
            store
                .lock()
                .unwrap()
                .set_enabled(&r.id, true)
                .expect("enable");
            rules.push(r);
        }

        // Boot in the background, close while it is still working through the group.
        let booting = tokio::spawn({
            let m = m.clone();
            async move { m.start_enabled().await }
        });
        tokio::task::yield_now().await;
        m.close_all().await;
        let _ = booting.await;
        // The boot's group task is a detached spawn; give it every chance to run past close_all.
        tokio::time::sleep(Duration::from_millis(50)).await;

        for r in &rules {
            assert_ne!(
                state_of(&m, &r.id),
                "up",
                "{} came up after close_all",
                r.name
            );
            assert!(
                port_is_free(r.local_port).await,
                "{} still holds its port",
                r.name
            );
        }
        for i in 0..built_len(&built) {
            assert!(
                !nth(&built, i).is_connected(),
                "a client survived close_all"
            );
        }
        // A rule asked to start behind the gate is refused, not half-built.
        let refused = m.start_rule(&rules[0].id).await.expect_err("closed");
        assert!(
            refused.message().contains("closed"),
            "{}",
            refused.message()
        );
        assert_eq!(state_of(&m, &rules[0].id), "stopped");

        // The next boot lowers the gate and everything comes back.
        let results = m.start_enabled().await;
        assert!(results.iter().all(|x| x.ok), "{results:?}");
        for r in &rules {
            assert_eq!(state_of(&m, &r.id), "up");
        }
        m.close_all().await;
    }

    /// A listening forward whose client has been ended (a stray `end()` from any path) is
    /// rebuilt on the next start rather than reported "up" over a dead session.
    #[tokio::test]
    async fn a_listener_on_a_dead_client_is_rebuilt_on_start_not_reported_up() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));
        m.start_rule(&r.id).await.expect("start");
        let first = nth(&built, 0);
        first.end().await; // the session is gone; the listener is not
        assert!(
            !port_is_free(port).await,
            "the stale listener is still bound"
        );

        m.start_rule(&r.id)
            .await
            .expect("restart over the dead client");
        assert_eq!(state_of(&m, &r.id), "up");
        // The dead client was the rule's last reference, so it left the map; the restart
        // built a fresh one instead of reviving it.
        assert!(!first.is_connected(), "the ended client stays ended");
        assert!(
            nth(&built, built_len(&built) - 1).is_connected(),
            "a live client replaced it"
        );
        assert_eq!(round_trip(port, "hello").await, "HELLO");
        m.close_all().await;
    }

    #[tokio::test]
    async fn close_all_frees_every_port_but_leaves_enabled_alone() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut ports = Vec::new();
        let mut ids = Vec::new();
        for i in 0..2 {
            let port = free_port().await;
            ids.push(add_rule(&store, rule_input(&format!("r{i}"), &c.id, port, echo)).id);
            ports.push(port);
        }
        m.start_all().await;

        m.close_all().await;
        for port in ports {
            assert!(port_is_free(port).await);
        }
        // Enabled is untouched, so the next boot restores exactly this set.
        for id in &ids {
            assert!(stored_rule(&store, id).enabled);
        }
        assert_eq!(nth(&built, 0).ended(), 1);
    }

    // --- interactive shells (docs/14 T2) ------------------------------------------------------
    //
    // These live here rather than beside `TunnelShells` because the harness does: a provider is
    // only interesting over a manager whose clients can be driven, and FakeConn is this module's.

    fn shells(m: &Arc<TunnelManager>) -> Arc<crate::tunnel::shell::TunnelShells> {
        crate::tunnel::shell::TunnelShells::new(m.clone())
    }

    fn pty_size() -> PtySize {
        PtySize::new(80, 24).expect("in range")
    }

    /// Wait for something a detached pump settles on its own — a reference handed back by a
    /// guard's Drop, for instance. Polling, because there is no handle out here to await.
    async fn eventually(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if done() {
                return true;
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
        done()
    }

    #[tokio::test]
    async fn a_shell_takes_a_reference_and_gives_it_back_when_the_session_drops() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);

        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("the shell opens");
        let conn = nth(&built, 0);
        assert_eq!(conn.shells(), 1, "one PTY, on the one client");
        assert_eq!(conn.ref_count(), 1, "the session holds a reference");
        assert_eq!(conn.ended(), 0);
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec())),
            "the far side was told the size it was opened with"
        );

        drop(session);
        // Dropping the session is the whole release path: the pump notices, drops the guard,
        // the guard returns the reference through put_ref, and the last one out ends the client.
        assert!(
            eventually(|| conn.ref_count() == 0 && conn.ended() == 1).await,
            "refs={} ended={}",
            conn.ref_count(),
            conn.ended()
        );
    }

    #[tokio::test]
    async fn a_terminal_shares_a_rules_connection_and_does_not_close_it() {
        // The invariant a user feels: opening a terminal on a host that already has tunnels up
        // must not dial twice, and closing the terminal must not take the tunnels down.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let port = echo_server().await;
        let rule = add_rule(&store, rule_input("db", &def.id, free_port().await, port));
        let (m, built) = manager(&store);
        m.start_rule(&rule.id).await.expect("the rule starts");
        let conn = nth(&built, 0);
        assert_eq!(conn.ref_count(), 1, "the rule holds one");

        let provider = shells(&m);
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("the shell opens");
        assert_eq!(
            built_len(&built),
            1,
            "one client, shared - not a second dial"
        );
        assert_eq!(conn.ref_count(), 2, "and now two holders");

        drop(session);
        assert!(
            eventually(|| conn.ref_count() == 1).await,
            "the terminal reference came back"
        );
        assert_eq!(conn.ended(), 0, "the rule tunnel is untouched");
        assert_eq!(state_of(&m, &rule.id), "up");
    }

    #[tokio::test]
    async fn a_refused_pty_returns_the_reference_instead_of_pinning_the_client() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);
        // Dial once so the client exists, then make the next PTY request fail.
        let first = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        let conn = nth(&built, 0);
        conn.refuse_shell.store(true, Ordering::SeqCst);

        match provider.open(&def.id, "terminal", pty_size()).await {
            Err(ShellError::Unavailable(msg)) => assert!(msg.contains("refused"), "{msg}"),
            Ok(_) => panic!("the server refused; the open must fail"),
            Err(other) => panic!("wrong error: {other}"),
        }
        // The guard drops on the failure path, and its release is a spawned task (Drop
        // cannot await): the reference comes back promptly, not synchronously.
        assert!(
            eventually(|| conn.ref_count() == 1).await,
            "the refused open gave its reference back; refs={}",
            conn.ref_count()
        );
        assert_eq!(
            conn.ended(),
            0,
            "the first session still holds the client open"
        );
        drop(first);
        assert!(eventually(|| conn.ref_count() == 0 && conn.ended() == 1).await);
    }

    #[tokio::test]
    async fn an_unknown_target_is_refused_without_minting_a_lease() {
        let (_dir, store) = scratch();
        add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        match provider.open("no-such-conn", "terminal", pty_size()).await {
            Err(ShellError::Unknown(msg)) => {
                assert!(msg.contains("no-such-conn"), "{msg}");
                assert!(
                    msg.contains("tunnels"),
                    "the message names the plugin: {msg}"
                );
            }
            Ok(_) => panic!("unknown target, got a session"),
            Err(other) => panic!("wrong error: {other}"),
        }
        assert_eq!(
            provider.ledger().outstanding(),
            0,
            "a typo must not show up in the drain count"
        );
    }

    #[tokio::test]
    async fn withdrawing_refuses_new_shells_and_names_the_plugin() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        provider.ledger().begin_withdraw();
        match provider.open(&def.id, "terminal", pty_size()).await {
            Err(ShellError::Withdrawing(msg)) => {
                assert!(
                    msg.contains("tunnels"),
                    "the message names the plugin: {msg}"
                )
            }
            Ok(_) => panic!("a withdrawing provider must not open a session"),
            Err(other) => panic!("wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn the_drain_reports_a_terminal_the_stop_had_to_close_over() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        provider.ledger().begin_withdraw();
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_millis(50), "tunnels").await,
            1,
            "an attached terminal does not hand itself back"
        );
        drop(session);
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_secs(5), "tunnels").await,
            0
        );
    }

    #[tokio::test]
    async fn the_target_list_reports_the_live_connection_state() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let before = provider.list();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id, def.id);
        assert_eq!(before[0].username, "deploy");
        assert_eq!(
            before[0].state, "idle",
            "never dialed is idle, which is not the same as failing"
        );
        let session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        assert_eq!(provider.list()[0].state, "connected");
        drop(session);
    }

    #[tokio::test]
    async fn typing_reaches_the_far_side_and_a_resize_follows_it_in_order() {
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec()))
        );
        session.write(b"ls\n".to_vec()).await.expect("typed");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"ls\n".to_vec()))
        );
        let bigger = PtySize::new(132, 43).expect("in range");
        session.resize(bigger).await.expect("resized");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"132x43".to_vec()))
        );
        assert_eq!(session.size(), bigger);
    }

    #[tokio::test]
    async fn the_registry_hands_the_terminal_a_session_without_it_knowing_about_ssh() {
        // End to end through the host contract, which is the only path the terminal plugin
        // will ever use: register the provider, open through the registry, withdraw, clear.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, built) = manager(&store);
        let provider = shells(&m);
        let registry = ShellRegistry::new();
        registry
            .register(provider.clone(), "tunnels")
            .expect("registers");
        assert!(registry.has_provider());
        assert_eq!(registry.list().len(), 1);

        let mut session = registry
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opened through the registry");
        assert!(session.next_event().await.is_some());
        assert_eq!(nth(&built, 0).ref_count(), 1);

        registry.begin_withdraw();
        provider.ledger().begin_withdraw();
        assert!(matches!(
            registry.open(&def.id, "terminal", pty_size()).await,
            Err(ShellError::Withdrawing(_))
        ));
        drop(session);
        assert_eq!(
            drain_sessions(provider.ledger(), StdDuration::from_secs(5), "tunnels").await,
            0,
            "the drain is clean once the tab is gone"
        );
        registry.clear();
        assert!(!registry.has_provider());
        assert!(eventually(|| nth(&built, 0).ended() == 1).await);
    }

    #[tokio::test]
    async fn a_typed_interrupt_gets_through_while_the_output_queue_is_full() {
        // docs/14 section 10 item 7, at this layer: the provider pumps the two directions in
        // separate tasks, so a terminal that is flooding still accepts Ctrl-C.
        let (_dir, store) = scratch();
        let def = add_conn(&store);
        let (m, _built) = manager(&store);
        let provider = shells(&m);
        let mut session = provider
            .open(&def.id, "terminal", pty_size())
            .await
            .expect("opens");
        // Never read the greeting: the fake echoes, so typing is what fills the queue.
        let typed = OUTPUT_QUEUE_CHUNKS + 8;
        for i in 0..typed {
            session
                .write(vec![b'a' + (i % 26) as u8])
                .await
                .expect("typed");
        }
        session.write(vec![0x03]).await.expect("Ctrl-C is accepted");
        // Drain: the greeting, then every byte typed, in order, none lost.
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"80x24$ ".to_vec()))
        );
        for i in 0..typed {
            assert_eq!(
                session.next_event().await,
                Some(PtyEvent::Data(vec![b'a' + (i % 26) as u8])),
                "byte {i} arrived out of order or was dropped"
            );
        }
        assert_eq!(session.next_event().await, Some(PtyEvent::Data(vec![0x03])));
    }

    // --- remote execution leases + provider (docs/34) --------------------------------------

    use swiss_host::services::action::CancelSource;
    use swiss_host::services::remote::{
        RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteTransportProvider,
    };

    /// A minimal request helper: argv only, the shape every lease test needs.
    fn exec_req(argv: &[&str]) -> RemoteExecRequest {
        RemoteExecRequest {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            cwd: None,
        }
    }

    fn collect(mut rx: tokio::sync::mpsc::Receiver<RemoteExecEvent>) -> (String, String) {
        let (mut out, mut err) = (String::new(), String::new());
        loop {
            match rx.try_recv() {
                Ok(RemoteExecEvent::Stdout(b)) => out.push_str(&String::from_utf8_lossy(&b)),
                Ok(RemoteExecEvent::Stderr(b)) => err.push_str(&String::from_utf8_lossy(&b)),
                Err(_) => break, // empty or the sender side dropped: both mean "done"
            }
        }
        (out, err)
    }

    /// Real-transport helper: store + connection row against a spawned test server port.
    fn real_store_with(dir: &Path, port: u16) -> (Arc<Mutex<TunnelStore>>, SshConnDef) {
        swiss_core::secure::key::use_test_master_key();
        let store = Arc::new(Mutex::new(TunnelStore::new(
            dir.join("tunnels.json"),
            19999,
        )));
        let input = ConnInput {
            name: "srv".into(),
            host: "127.0.0.1".into(),
            port: port as f64,
            username: "deploy".into(),
            auth_type: AuthType::Password,
            password: Some("pw".into()),
            ..Default::default()
        };
        let conn = store.lock().unwrap().add_connection(&input).expect("add");
        (store, conn)
    }

    #[tokio::test]
    async fn a_remote_exec_shares_the_rule_client_and_releases_its_lease() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let echo = echo_server().await;
        let rule = add_rule(&store, rule_input("r1", &conn.id, free_port().await, echo));
        m.start_rule(&rule.id).await.expect("starts");
        let refs_with_rule = nth(&built, 0).ref_count();

        // The provider leases while the exec runs and gives the reference back.
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        *nth(&built, 0).exec_output.lock().unwrap() = b"hello\nworld\n".to_vec();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                exec_req(&["make"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("execs");
        assert_eq!(result.exit_code, 0);
        let (out, _) = collect(rx);
        assert_eq!(out, "hello\nworld\n");
        assert_eq!(nth(&built, 0).execs.load(Ordering::SeqCst), 1);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == refs_with_rule).await,
            "the lease returned to the rule's reference count"
        );
        assert_eq!(
            nth(&built, 0).dials(),
            1,
            "no second dial: the client is shared"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_result_not_an_error() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        // No rule here: a setup lease dials the client and holds it while the exec
        // rides the same connection, which is the interesting half of the contract.
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        nth(&built, 0).exec_exit.store(7, Ordering::SeqCst);
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                exec_req(&["false"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("a nonzero exit is still a completed exec");
        assert_eq!(result.exit_code, 7);
        // The lease release is a spawned task (Drop cannot await): poll for it.
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "back to the setup lease only"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0).await,
            "no leaked lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_canceled_exec_stops_and_releases_the_lease() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        // Enough output chunks that the cancel lands mid-stream.
        *nth(&built, 0).exec_output.lock().unwrap() = vec![b'x'; 7 * 200];
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let source = CancelSource::new();
        let (tx, rx) = tokio::sync::mpsc::channel(4); // small: the pump parks, cancel wins
        let handle = source.handle();
        let exec = provider.exec(
            &conn.id,
            "remote.exec",
            exec_req(&["long-build"]),
            tx,
            handle,
        );
        // Cancel while the first chunks are still in flight: the fake checks the
        // cancel flag between every chunk, so a mid-stream cancel is deterministic.
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        source.cancel();
        match exec.await {
            Err(RemoteError::Canceled(m)) => assert!(m.contains("canceled"), "{m}"),
            Ok(_) => panic!("canceled, not a result"),
            Err(other) => panic!("wrong error: {other}"),
        }
        drop(rx);
        // The exec's own reference is back even though the command "never finished";
        // the setup lease then unwinds the client itself.
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "canceled released the exec lease"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0 && nth(&built, 0).ended() == 1).await,
            "the client unwound after its last lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_exec_on_an_unknown_endpoint_is_named() {
        let (dir, store) = scratch();
        let (m, _) = manager(&store);
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        match provider
            .exec(
                "ghost",
                "remote.exec",
                exec_req(&["make"]),
                tx,
                CancelSource::new().handle(),
            )
            .await
        {
            Err(RemoteError::Unknown(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown endpoint, got a result"),
            Err(other) => panic!("wrong error: {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn file_operations_round_trip_and_release_their_leases() {
        let (dir, store) = scratch();
        let (m, built) = manager(&store);
        let conn = add_conn(&store);
        let (_client, lease) = m.lease_connection(&conn.id).await.expect("dial");
        let provider = crate::tunnel::remote::TunnelRemote::new(m.clone());
        provider
            .mkdir_p(&conn.id, "sync", "/data/ws/proj/src")
            .await
            .expect("mkdir");
        let mut writer = provider
            .create(&conn.id, "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("create");
        // The lease is held WHILE the writer is open: refs on the client are >= 2
        // (the setup lease plus the writer's own).
        assert!(
            nth(&built, 0).ref_count() >= 2,
            "an open file holds its lease on top of the setup lease"
        );
        writer.write_chunk(b"int main(){}\n").await.expect("write");
        writer.finish().await.expect("finish");
        drop(writer);
        let stat = provider
            .stat(&conn.id, "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("stat")
            .expect("present");
        assert_eq!(stat.size, 13, "the written byte count");
        let mut reader = provider
            .open_read(&conn.id, "pull", "/data/ws/proj/src/main.c")
            .await
            .expect("read");
        let mut back = Vec::new();
        while let Some(chunk) = reader.read_chunk().await.expect("chunk") {
            back.extend_from_slice(&chunk);
        }
        drop(reader);
        assert_eq!(back, b"int main(){}\n");
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 1).await,
            "every file lease returned to the setup lease's count"
        );
        drop(lease);
        assert!(
            eventually(|| nth(&built, 0).ref_count() == 0).await,
            "the client unwound after its last lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full russh stack against the fake exec server (sshtest): connect, session
    /// channel, exec request, streaming Data/ExtendedData, exit status.
    #[tokio::test]
    async fn the_real_client_execs_against_a_real_russh_server() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-ssh-exec-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let port = sshtest::spawn_exec_ssh_server(sshtest::ExecScript {
            stdout: b"hello\nworld\n".to_vec(),
            stderr: b"warn".to_vec(),
            exit: Some(0),
        })
        .await;
        let (store, conn) = real_store_with(&dir, port);
        let m = TunnelManager::new(store.clone(), None);
        let provider = crate::tunnel::remote::TunnelRemote::new(m);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let result = provider
            .exec(
                &conn.id,
                "remote.exec",
                RemoteExecRequest {
                    argv: vec!["echo".into(), "one two".into(), "it's".into()],
                    env: vec![("BOARD".into(), "a;b".into())],
                    cwd: Some("/tmp/x y".into()),
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("exec over the real transport");
        assert_eq!(result.exit_code, 0);
        let (stdout, stderr) = collect(rx);
        assert_eq!(stdout, "hello\nworld\n");
        assert_eq!(stderr, "warn");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancellation against the real transport: the server stalls between its output
    /// and the exit status; the client closes the channel and reports canceled.
    #[tokio::test]
    async fn the_real_client_cancels_a_stalled_exec() {
        let dir =
            std::env::temp_dir().join(format!("swiss-ssh-cxl-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        // A wedged command: output, then nothing — no exit status, no close. The
        // only ways out are the client's cancel or its deadline.
        let port = sshtest::spawn_exec_ssh_server(sshtest::ExecScript {
            stdout: b"partial".to_vec(),
            stderr: Vec::new(),
            exit: None,
        })
        .await;
        let (store, conn) = real_store_with(&dir, port);
        let m = TunnelManager::new(store.clone(), None);
        let provider = crate::tunnel::remote::TunnelRemote::new(m);
        let source = CancelSource::new();
        let handle = source.handle();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        // SPAWN the exec: an unpinned future is never polled, and on the current_thread
        // runtime only a spawned task can stream while the test waits for the first
        // bytes.
        let exec = tokio::spawn(async move {
            provider
                .exec(&conn.id, "remote.exec", exec_req(&["yocto"]), tx, handle)
                .await
        });
        // Park until the first stdout arrives, then cancel into the stall.
        let streamed = loop {
            match rx.recv().await {
                Some(RemoteExecEvent::Stdout(b)) if !b.is_empty() => break true,
                Some(_) => continue,
                None => break false,
            }
        };
        // Cancel only matters while the exec still runs; if it already ended, the
        // await below returns immediately and the assert names what it returned.
        if streamed {
            source.cancel();
        }
        let outcome = exec.await.expect("the exec task finished");
        assert!(
            streamed,
            "the stalled exec streamed its output first; it finished instead: {outcome:?}"
        );
        match outcome {
            Err(RemoteError::Canceled(_)) => {}
            Ok(_) => panic!("a stalled exec must not finish"),
            Err(other) => panic!("wrong error: {other}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
