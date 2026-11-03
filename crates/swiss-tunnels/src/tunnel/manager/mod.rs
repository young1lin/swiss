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
//!
//! `impl TunnelManager` is spread over a file per concern, each with its own `impl` block:
//!
//! - `client` - the slice of an SSH client the manager drives (`SshLike`), and the real one.
//! - `pool` - one refcounted client per connection: dial or reuse, take and give back references.
//! - `jump` - chained connections through a jump host (SPEC §tunnels.jump).
//! - `lifecycle` - starting and stopping one rule.
//! - `bulk` - many rules at once: boot, start-all and stop-all, shutdown.
//! - `recovery` - transport loss, and the reconnect backoff.
//! - `edits` - rule and connection edits and deletes, the connection test, host-key trust.
//! - `links` - the MCP linkage: display and the stop guard rail, never control.
//! - `report` - the rule and connection rows the panel reads.
//! - `shell` - interactive shells on a connection (SPEC §terminal.remote).
//! - `lease` - an operation's reference on a client, for remote exec and files (SPEC §remote).
//!
//! The tests run the lifecycle over `fake::FakeConn`, a client whose channels are plain TCP
//! sockets; the harness they share is `test_util`.

mod client;
mod pool;
mod jump;
mod lifecycle;
mod bulk;
mod recovery;
mod edits;
mod links;
mod report;
mod shell;
mod lease;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod test_util;

pub use self::client::SshLike;
pub use self::links::{McpView, TunnelLinks};
pub use self::lease::{ConnectionLease, ShellSessionGuard};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use self::client::{real_connections, ConnFactory};
use super::forward::Forward;
use super::store::TunnelStore;
use super::types::{PortOwner, RuleDef, RuleState};

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
    /// Linked MCPs whose pools were dropped for the current `reconnected_at`. The detail
    /// page asks for a restart only for a name that is not in this list.
    pools_released: Vec<String>,
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
            pools_released: Vec::new(),
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
}
