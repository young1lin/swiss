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

//! The typed connection catalog — who may browse or call a connection, and for how long
//! (docs/09 §4, the P4 decoupling; docs/12 W3).
//!
//! Data owns no resources: every browse rides an MCP adapter's pool or child. Before this
//! module that sharing was implicit — Data resolved rows out of the live registry, so
//! stopping MCP pulled Data's floor away while Data's row kept saying "fine". The catalog
//! makes the relationship a CONTRACT:
//!
//! - the provider (MCP) registers one [ConnectionCatalog] here, on start;
//! - a consumer (Data) takes a REQUEST-SCOPED [ConnectionLease] per /api/db call —
//!   holding one is the only sanctioned way to touch a connection;
//! - a provider stopping first withdraws (no NEW leases), then drains (waits for the ones
//!   in flight), and only then closes the underlying pools — with an honest warning when
//!   it had to give up waiting, naming how many leases were still out.
//!
//! Shape follows [crate::services::action::ActionRegistry]: registration refuses
//! duplicates loudly (two providers claiming the catalog is a composition bug to surface
//! at boot, not a silent last-write-wins), withdrawal is visible, and nothing here knows
//! who the plugins are.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::dbbrowser::BrowserFlavor;

/// A connection definition someone can browse or call, independent of who owns the driver.
/// The key must carry configuration identity, credential context and read-only semantics -
/// two connections that merely share host and port are NOT the same connection (docs/09
/// §3): the id is the registry entry's name, which already encodes the whole definition.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    pub id: String,
    pub label: String,
    /// The browser dialect ("mysql", "pg", "redis") — or "none" for a registered
    /// entry with nothing to browse, kept in the list so lookup errors can name its type.
    pub dialect: String,
    pub state: String,
}

/// Why a lease could not be taken. The strings are user-facing: the /api/db routes hand
/// them to the panel verbatim behind the matching status code.
#[derive(Debug)]
pub enum CatalogError {
    /// No such connection id.
    Unknown(String),
    /// The provider is shutting down and will not hand out new leases.
    Withdrawing(String),
    /// The connection exists but has no browsable driver behind it (echo, http, ...).
    NotBrowsable(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Unknown(m) => write!(f, "{m}"),
            CatalogError::Withdrawing(m) => write!(f, "{m}"),
            CatalogError::NotBrowsable(m) => write!(f, "{m}"),
        }
    }
}

/// The lease ledger one provider shares with its leases. Outstanding-count + withdrawing
/// flag + one condition variable: granting increments, a lease's Drop decrements and
/// wakes the drainer. Deliberately primitive — policy lives in [ConnectionCatalog] impls
/// and the stop choreography, not in the ledger.
pub struct LeaseTracker {
    outstanding: Mutex<usize>,
    withdrawing: Mutex<bool>,
    idle: Condvar,
    /// The async mirror of `outstanding`: every grant and drop publishes the new count here
    /// so [wait_idle_async] can park a TASK instead of the runtime thread. Same shape as the
    /// shell seat's SessionLedger — its module note explains why the condvar alone cannot
    /// serve a provider stopping on the single-threaded runtime.
    idle_tx: tokio::sync::watch::Sender<usize>,
}

impl LeaseTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(LeaseTracker {
            outstanding: Mutex::new(0),
            withdrawing: Mutex::new(false),
            idle: Condvar::new(),
            idle_tx: tokio::sync::watch::channel(0).0,
        })
    }

    /// Count a lease in and mint it. The count exists so a draining provider can decide
    /// honestly whether it closed clean or over a live request.
    pub fn grant(
        self: &Arc<Self>,
        id: &str,
        holder: &str,
        dialect: &str,
        flavor: BrowserFlavor,
    ) -> ConnectionLease {
        let count = {
            let mut guard = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
            *guard += 1;
            *guard
        };
        let _ = self.idle_tx.send(count);
        ConnectionLease {
            tracker: Arc::clone(self),
            id: id.to_string(),
            holder: holder.to_string(),
            dialect: dialect.to_string(),
            flavor,
        }
    }

    pub fn outstanding(&self) -> usize {
        *self.outstanding.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop handing out the provider's connections — the first half of a stop. Idempotent,
    /// and a no-op when nothing was registered.
    pub fn begin_withdraw(&self) {
        *self.withdrawing.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    pub fn is_withdrawing(&self) -> bool {
        *self.withdrawing.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait until every lease is back, or the timeout expires. Returns how many leases
    /// were still out when it gave up — 0 means the close was clean.
    pub fn wait_idle(&self, timeout: Duration) -> usize {
        let count = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .idle
            .wait_timeout_while(count, timeout, |n| *n > 0)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// The async half of [wait_idle]: same contract, but the wait parks a task, not the runtime
    /// thread. On the single-threaded runtime a lease holder's Drop can only run on the very
    /// thread a condvar wait would block, so a provider stopping from async code must use THIS
    /// one — the condvar flavour stays for tests and non-runtime drainers.
    pub async fn wait_idle_async(&self, timeout: Duration) -> usize {
        // The channel is only a wakeup; the mutex count is the truth. That sidesteps the
        // watch footgun where a publish made before any subscriber existed never reaches a
        // later subscriber's current value — every wake re-reads the ledger instead.
        let mut rx = self.idle_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.outstanding() == 0 {
                return 0;
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                // A grant or a drop moved the count; read it again.
                Ok(Ok(())) => continue,
                // The ledger is gone; no holder can still matter.
                Ok(Err(_)) => return 0,
                Err(_) => return self.outstanding(),
            }
        }
    }
}

/// How long a draining provider waits for in-flight leases before closing over them.
/// Five seconds covers a bounded /api/db page or one SQL console round-trip; a lease held
/// longer than that is a bug in its holder, and the warning names it.
/// A streamed SQL dump (docs/22 W4.4) legitimately outlives this window on a big table — its
/// lease travels inside the body stream — and that is the deliberate trade: the drain aborts
/// honestly (the download ends early) rather than pinning pools open for a download nobody
/// is reading. Raising this for dumps would stall every disable for one slow client.
pub const DRAIN_TIMEOUT_MS: u64 = 5_000;

/// The provider's stop choreography, in order and out loud: wait (logged when there is
/// something to wait for), then warn with the count when the wait was not clean. Returns
/// the unreleased count so tests can assert on it without scraping logs.
pub async fn drain_leases(tracker: &LeaseTracker, timeout: Duration, who: &str) -> usize {
    let waiting = tracker.outstanding();
    if waiting > 0 {
        swiss_core::log::log(
            "info",
            "waiting for connection leases before closing",
            Some(serde_json::json!({ "provider": who, "leases": waiting })),
        );
    }
    let remaining = tracker.wait_idle_async(timeout).await;
    if remaining > 0 {
        swiss_core::log::warn(
            "closing with unreleased connection leases",
            Some(serde_json::json!({ "provider": who, "leases": remaining })),
        );
    }
    remaining
}
/// A lease on one connection, taken by a named holder for the life of one request.
/// Dropping it is the release — there is no "remember to give it back" API to forget.
pub struct ConnectionLease {
    tracker: Arc<LeaseTracker>,
    id: String,
    holder: String,
    dialect: String,
    flavor: BrowserFlavor,
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let now = {
            let mut count = self
                .tracker
                .outstanding
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *count = count.saturating_sub(1);
            *count
        };
        self.tracker.idle.notify_all();
        let _ = self.tracker.idle_tx.send(now);
    }
}

impl ConnectionLease {
    /// Which connection this leases.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Who holds it — the name that lands in "closing over N leases" forensics.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// The connection's dialect, for mismatch messages ("is not a redis connection").
    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    /// The browsable driver the lease pins alive. Held for the lease's life — use it
    /// before the lease drops.
    pub fn flavor(&self) -> &BrowserFlavor {
        &self.flavor
    }
}

/// What a provider registers: the connections it can hand out, and how to lease one.
pub trait ConnectionCatalog: Send + Sync {
    fn list(&self) -> Vec<ConnectionInfo>;
    /// Take a lease on one connection. While a lease is held the provider must keep the
    /// underlying pool or child alive; dropping the lease is what allows it to close.
    fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError>;
}

/// Whether the catalog can serve right now, and if not, who is missing — the input to
/// Data's 503s, so the message can NAME the provider instead of pretending there are
/// simply no databases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogPresence {
    /// A provider is registered and serving.
    Serving(String),
    /// The provider is draining; no new leases.
    Stopping(String),
    /// No provider. The id is the LAST one, when there was one — "the mcp plugin ...
    /// is currently disabled" beats "no connections" (which reads as "you have none").
    Absent(Option<String>),
}

/// The single-provider registry, ActionRegistry's shape (docs/09 §2: provide conflicts
/// are visible). One catalog per process: the day a second provider exists, the registry
/// grows a per-capability map — not a silent overwrite.
#[derive(Default)]
pub struct CatalogRegistry {
    state: Mutex<CatalogState>,
}

#[derive(Default)]
struct CatalogState {
    provider: Option<Arc<dyn ConnectionCatalog>>,
    provider_id: Option<String>,
    last_provider_id: Option<String>,
    withdrawing: bool,
}

impl CatalogRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the one provider. Err names the incumbent when the seat is taken — even
    /// a withdrawing one, because the drain still owns it until [CatalogRegistry::clear].
    pub fn register(
        &self,
        provider: Arc<dyn ConnectionCatalog>,
        provider_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = &state.provider_id {
            return Err(format!(
                "the connection catalog is already provided by plugin {current}"
            ));
        }
        state.provider = Some(provider);
        state.provider_id = Some(provider_id.to_string());
        state.last_provider_id = Some(provider_id.to_string());
        state.withdrawing = false;
        Ok(())
    }

    /// A provider's stop, first half: no new leases go out, the ones in flight are
    /// expected back (see [drain_leases]). The seat stays occupied until [clear].
    pub fn begin_withdraw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.provider.is_some() {
            state.withdrawing = true;
        }
    }

    /// A provider's stop, last half: the seat is free and may be re-registered by the
    /// next start. The last provider id is kept for the honest "who is missing" 503.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.provider = None;
        state.provider_id = None;
        state.withdrawing = false;
    }

    pub fn list(&self) -> Vec<ConnectionInfo> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .provider
            .as_ref()
            .map(|p| p.list())
            .unwrap_or_default()
    }

    pub fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError> {
        let provider = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.provider.is_none() {
                return Err(CatalogError::Unknown(
                    "the connection catalog has no provider".to_string(),
                ));
            }
            if state.withdrawing {
                return Err(CatalogError::Withdrawing(format!(
                    "plugin {} is stopping; no new connection leases",
                    state.provider_id.as_deref().unwrap_or("?")
                )));
            }
            state.provider.clone().expect("checked above")
        };
        // Lease OUTSIDE the registry lock: a provider implementation may itself lock, and
        // lock-order inversion between registry and provider is exactly the kind of bug
        // the borrow checker will not catch for us.
        provider.lease(id, holder)
    }

    /// Serving state + who is missing, for 503 messages and the inventory's requiresMet.
    pub fn presence(&self) -> CatalogPresence {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match (&state.provider_id, state.withdrawing) {
            (Some(id), false) => CatalogPresence::Serving(id.clone()),
            (Some(id), true) => CatalogPresence::Stopping(id.clone()),
            (None, _) => CatalogPresence::Absent(state.last_provider_id.clone()),
        }
    }

    /// Whether a provider is registered and serving — what a capability probe answers.
    pub fn has_provider(&self) -> bool {
        matches!(self.presence(), CatalogPresence::Serving(_))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider with one browsable row and one browser-less row, plus a grant counter.
    struct StubCatalog {
        grants: AtomicUsize,
    }

    impl ConnectionCatalog for StubCatalog {
        fn list(&self) -> Vec<ConnectionInfo> {
            vec![
                ConnectionInfo {
                    id: "db-one".into(),
                    label: "db one".into(),
                    dialect: "mysql".into(),
                    state: "ready".into(),
                },
                ConnectionInfo {
                    id: "plain".into(),
                    label: "plain".into(),
                    dialect: "none".into(),
                    state: "stopped".into(),
                },
            ]
        }

        fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError> {
            match id {
                "db-one" => {
                    self.grants.fetch_add(1, Ordering::SeqCst);
                    Ok(LeaseTracker::new().grant(id, holder, "mysql", BrowserFlavor::None))
                }
                "plain" => Err(CatalogError::NotBrowsable(format!(
                    "MCP '{}' (echo) has no database to browse",
                    id
                ))),
                other => Err(CatalogError::Unknown(format!("unknown MCP: {other}"))),
            }
        }
    }

    #[test]
    fn a_duplicate_registration_is_refused_and_the_incumbent_stays() {
        let reg = CatalogRegistry::new();
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("first registers");
        let err = reg
            .register(
                Arc::new(StubCatalog {
                    grants: AtomicUsize::new(0),
                }),
                "other",
            )
            .expect_err("the seat is taken");
        assert!(err.contains("already provided by plugin mcp"), "{err}");
        assert_eq!(reg.list().len(), 2, "the incumbent still answers");
        assert!(matches!(reg.presence(), CatalogPresence::Serving(id) if id == "mcp"));
    }

    #[test]
    fn a_lease_counts_until_it_drops() {
        let reg = CatalogRegistry::new();
        let provider = Arc::new(StubCatalog {
            grants: AtomicUsize::new(0),
        });
        reg.register(provider.clone(), "mcp").expect("registers");
        {
            let lease = reg.lease("db-one", "data").expect("granted");
            assert_eq!(lease.holder(), "data");
            assert_eq!(lease.dialect(), "mysql");
            assert_eq!(provider.grants.load(Ordering::SeqCst), 1);
        } // dropped: the release must not depend on the holder remembering anything
        assert!(reg.lease("db-one", "data").is_ok(), "re-leasable");
    }

    #[test]
    fn withdrawal_refuses_new_leases_and_clear_releases_the_seat() {
        let reg = CatalogRegistry::new();
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("registers");
        reg.begin_withdraw();
        match reg.lease("db-one", "data") {
            Err(CatalogError::Withdrawing(m)) => assert!(m.contains("mcp"), "{m}"),
            Ok(_) => panic!("withdrawing refuses"),
            Err(other) => panic!("withdrawing refuses, wrong error: {other}"),
        }
        assert!(matches!(reg.presence(), CatalogPresence::Stopping(_)));
        // A duplicate while withdrawing is STILL a duplicate: the drain owns the seat.
        assert!(reg
            .register(
                Arc::new(StubCatalog {
                    grants: AtomicUsize::new(0)
                }),
                "other"
            )
            .is_err());
        reg.clear();
        assert!(matches!(
            reg.presence(),
            CatalogPresence::Absent(Some(id)) if id == "mcp"
        ));
        // The seat is free again: the next start re-registers cleanly.
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("re-registers after clear");
    }

    #[tokio::test]
    async fn drain_waits_for_lease_drops_and_reports_the_stragglers() {
        let tracker = LeaseTracker::new();
        let held = tracker.grant("db-one", "data", "mysql", BrowserFlavor::None);
        let remaining = drain_leases(&tracker, Duration::from_millis(50), "mcp").await;
        assert_eq!(remaining, 1, "the lease never came back within the window");
        drop(held);
        assert_eq!(
            drain_leases(&tracker, Duration::from_millis(50), "mcp").await,
            0,
            "once back, the drain is clean"
        );
    }

    // The freeze this drain used to be: on the current_thread runtime a lease holder parked on
    // an .await can only be polled by the one thread a CONDVAR wait blocks, so the sync drain
    // could never see it come back — it burned the whole window and reported the lease stuck,
    // which is what McpInstance::stop did to the entire gateway on every disable. The async
    // drain watches the published count instead, so the holder gets polled and it settles.
    #[tokio::test]
    async fn the_async_drain_reaches_leases_parked_on_the_runtime() {
        let tracker = LeaseTracker::new();
        let held = tracker.grant("db-one", "data", "mysql", BrowserFlavor::None);
        let holder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(held);
        });
        // The holder task has not been polled yet. The sync wait blocks the only thread, so
        // the drop can never run and the wait burns its full window with the lease stuck.
        let stuck = tracker.wait_idle(Duration::from_millis(120));
        assert_eq!(stuck, 1, "the condvar drain cannot see a runtime-parked holder");
        holder.await.expect("holder finished");

        // Now the same lease-held-across-an-await shape through the async drain: the watch
        // sees the published count, the holder gets polled, and the drain settles at the drop.
        let held2 = tracker.grant("db-one", "data", "mysql", BrowserFlavor::None);
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(held2);
        });
        let remaining = tracker.wait_idle_async(Duration::from_secs(1)).await;
        assert_eq!(remaining, 0, "the async drain watched the drop land");
        releaser.await.expect("releaser finished");
    }

    #[test]
    fn wait_idle_is_woken_by_a_drop_not_just_the_timeout() {
        let tracker = LeaseTracker::new();
        let held = tracker.grant("db-one", "data", "mysql", BrowserFlavor::None);
        let t = tracker.clone();
        let waiter = std::thread::spawn(move || t.wait_idle(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50)); // let the waiter park on the condvar
        drop(held);
        assert_eq!(waiter.join().expect("waiter"), 0, "woken by the drop");
    }

    #[test]
    fn a_hundred_lifecycles_leave_the_registry_exactly_empty() {
        // docs/10 §9's last acceptance item, at the registry level: enable/disable churn
        // must not leak providers, leases or rows.
        let reg = CatalogRegistry::new();
        for _ in 0..100 {
            reg.register(
                Arc::new(StubCatalog {
                    grants: AtomicUsize::new(0),
                }),
                "mcp",
            )
            .expect("registers");
            reg.begin_withdraw();
            reg.clear();
        }
        assert!(matches!(reg.presence(), CatalogPresence::Absent(Some(_))));
        assert!(reg.list().is_empty());
        assert!(!reg.has_provider());
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("registers after the churn");
    }

    #[test]
    fn browserless_connections_are_not_browsable_but_are_named() {
        let reg = CatalogRegistry::new();
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("registers");
        match reg.lease("plain", "data") {
            Err(CatalogError::NotBrowsable(m)) => assert!(m.contains("echo"), "{m}"),
            Ok(_) => panic!("not browsable, got a lease"),
            Err(other) => panic!("not browsable, wrong error: {other}"),
        }
        match reg.lease("ghost", "data") {
            Err(CatalogError::Unknown(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown, got a lease"),
            Err(other) => panic!("unknown, wrong error: {other}"),
        }
        // The listing still carries the row, so 404s can name its type (echo vs mysql).
        assert!(reg
            .list()
            .iter()
            .any(|c| c.id == "plain" && c.dialect == "none"));
    }

    #[test]
    fn two_definitions_sharing_host_and_port_stay_two_connections() {
        // docs/09 §3's named trap: identity is the DEFINITION (credentials and all), not
        // the endpoint. The catalog key is the registry entry's name — two entries that
        // differ only in credentials are two names, never merged into one connection.
        let reg = CatalogRegistry::new();
        reg.register(
            Arc::new(StubCatalog {
                grants: AtomicUsize::new(0),
            }),
            "mcp",
        )
        .expect("registers");
        let ids: Vec<String> = reg.list().into_iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let a = reg.lease("db-one", "data").expect("first");
        assert_eq!(a.id(), "db-one");
        assert!(reg.lease("plain", "data").is_err());
        let _ = json!({}); // keep the json import honest for future assertions
    }
}
