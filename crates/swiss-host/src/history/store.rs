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

//! One module's history store: the owner-scoped pool, writer queue and generation the host
//! provides and one module owns alone (SPEC §host.history).
//!
//! Invariants this module is the only enforcer of:
//! - construction performs no I/O, and a store that was never opened answers every query
//!   and write with an error, never a side-effecting open;
//! - `open` passes the engine gate (the WAL-reset fix) before any file exists, migrates
//!   the schema transactionally, and bumps the generation;
//! - admitted writes are byte-bounded; a barrier waits for everything admitted, including
//!   items the worker has not polled yet;
//! - `close` is bounded: on the deadline it reports failure with the unresolved count and
//!   the store is Failed, never "closed" — and a stale-generation queue entry can never
//!   execute into a newer generation.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_core::future::BoxFuture;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::history::engine::{self, EngineCaps};
use crate::history::schema::{self, SchemaStep};

/// The durability an owner declares for its database. Both run WAL — the engine gate made
/// that safe; the difference is how much a power failure may cost (SQLite's synchronous
/// level, applied per owner, never as a global pragma).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// WAL + NORMAL: acknowledged commits are visible, the latest ones may be lost on a
    /// power failure. The MCP history default.
    Normal,
    /// WAL + FULL: every acknowledged commit survives. The Remote history default, whose
    /// audit rows outrank throughput.
    Full,
}

/// One owner's declared policy. The factory stores it; only `open` acts on it.
pub struct OwnerPolicy {
    /// The database file, resolved by the composition root under the Swiss home
    /// (for example `<home>/logs/mcp.db`).
    pub db_file: PathBuf,
    /// The writer queue's byte bound. Admissions beyond it are refused — overflow is an
    /// honest error and a counter, never an unbounded buffer.
    pub queue_byte_bound: usize,
    /// The whole close operation's deadline (withdraw, drain, release) — not a fresh
    /// timeout per internal step.
    pub close_timeout: Duration,
    /// The owner's durability (see [Durability]).
    pub durability: Durability,
    /// The owner's schema steps, applied in order on open. `migrate` owns the version
    /// discipline; an unknown newer version stops the open with the store untouched.
    pub schema: &'static [SchemaStep],
}

impl OwnerPolicy {
    /// A policy sized for tests: small bounds, a fast close deadline.
    pub fn for_tests(db_file: PathBuf, schema: &'static [SchemaStep]) -> Self {
        OwnerPolicy {
            db_file,
            queue_byte_bound: 8 * 1024 * 1024,
            close_timeout: Duration::from_secs(2),
            durability: Durability::Normal,
            schema,
        }
    }
}

/// The opaque module identity ("mcp", "remote"). Display-only; nothing dispatches on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleId(Arc<str>);

impl ModuleId {
    pub fn new(id: &str) -> Self {
        ModuleId(Arc::from(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a write was not admitted, or how an admitted one failed.
#[derive(Debug, thiserror::Error)]
pub enum HistoryWriteError {
    #[error("history for {module} is {state}; the write was refused without touching storage")]
    NotOpen { module: String, state: &'static str },
    #[error("the write queue for {module} is at its byte bound ({bound}); the write was refused")]
    QueueFull { module: String, bound: usize },
    #[error("the physical share for {module} is exhausted ({reason}); the write was refused")]
    QuotaDenied { module: String, reason: String },
    #[error("an admitted write failed: {0}")]
    Failed(String),
    #[error("the barrier broke: {0}")]
    Barrier(String),
}

/// Why a query cannot run.
#[derive(Debug, thiserror::Error)]
pub enum HistoryQueryError {
    #[error("history for {module} is {state}; queries never open or re-enable it")]
    NotOpen { module: String, state: &'static str },
    #[error("the query failed: {0}")]
    Failed(String),
}

/// The enrolled quota's in-memory half: the declared policy, whether this owner's
/// metadata is inside its protected window (the denial REASON only - both paths deny),
/// and the last measured physical usage.
struct QuotaState {
    policy: crate::history::quota::QuotaPolicy,
    protected_window: bool,
    usage: crate::history::quota::PhysicalUsage,
}

/// Why an open or a close failed. A close timeout leaves the owner Failed with unresolved
/// work — reported, never labelled a successful release.
#[derive(Debug, thiserror::Error)]
pub enum OwnerLifecycleError {
    #[error("opening history for {module} failed: {reason}")]
    OpenFailed { module: String, reason: String },
    #[error("closing history for {module} hit its deadline with {unresolved} write(s) unresolved: {reason}")]
    CloseDeadline { module: String, unresolved: u64, reason: String },
    #[error("closing history for {module} failed: {reason}")]
    CloseFailed { module: String, reason: String },
    #[error("history for {module} is already {state}")]
    WrongState { module: String, state: &'static str },
}

/// One unit of admitted work: the owner's SQL against its own pool, one transaction per
/// item. The store never inspects the SQL; it owns admission, ordering and the generation
/// check.
pub type WriteJob = Box<dyn FnOnce(SqlitePool) -> BoxFuture<'static, Result<(), String>> + Send>;

struct QueuedWrite {
    generation: u64,
    estimated_bytes: usize,
    /// The reservation-ledger token (generation + admission seq): deterministic, so a
    /// write retried after a crash replaces its own row instead of adding another.
    token: String,
    job: WriteJob,
}

#[derive(Default)]
struct QueueInner {
    items: std::collections::VecDeque<QueuedWrite>,
    bytes: usize,
    admitted: u64,
    completed: u64,
    /// The highest failed seq and its reason, for barriers covering it.
    last_failure: Option<(u64, String)>,
    /// Writes dropped because their generation went stale (a failed close, a reopen):
    /// accounted, never executed.
    stale_dropped: u64,
}

enum OwnerState {
    /// Constructed, never opened. Every operation refuses without I/O.
    Disabled,
    /// Serving: the pool is live and the worker drains the queue.
    Open { pool: SqlitePool, caps: EngineCaps },
    /// Withdrawn: new work is refused, accepted work still drains against the pool.
    Stopping { pool: SqlitePool },
    /// Released cleanly: the pool was awaited closed and the queue was empty.
    Closed,
    /// A close that hit its deadline, or an open that failed after touching a file. The
    /// generation is invalid; only a fresh `open` starts a new one. Never reported as
    /// closed — the unresolved count and reason stay visible.
    Failed { reason: String, unresolved: u64 },
}

impl OwnerState {
    fn label(&self) -> &'static str {
        match self {
            OwnerState::Disabled => "disabled",
            OwnerState::Open { .. } => "open",
            OwnerState::Stopping { .. } => "stopping",
            OwnerState::Closed => "closed",
            OwnerState::Failed { .. } => "failed",
        }
    }

    fn pool(&self) -> Option<&SqlitePool> {
        match self {
            OwnerState::Open { pool, .. } | OwnerState::Stopping { pool } => Some(pool),
            _ => None,
        }
    }
}

struct StoreInner {
    state: OwnerState,
    queue: QueueInner,
}

/// The store one module owns. Cheap to hold as a long-lived facade: until `open` it is
/// state and bounds only, and after `close` its generation rejects every write.
pub struct ModuleHistoryStore {
    id: ModuleId,
    policy: OwnerPolicy,
    inner: Mutex<StoreInner>,
    generation: AtomicU64,
    /// Per-boot identity for reservation tokens: two processes can hold this file
    /// open at once and both start their generation/admitted counters from the
    /// same values, so "{generation}-{admitted}" alone collides across processes
    /// and INSERT OR REPLACE would let two in-flight writes share one ledger row.
    instance: String,
    /// The enrolled physical-quota state (None until the owner opts in at open). The
    /// usage snapshot is refreshed by maintain(); admission reads it plus the
    /// in-flight bytes below, never the live filesystem (submit is synchronous).
    quota: std::sync::RwLock<Option<QuotaState>>,
    /// Bytes admitted but not yet completed - the in-process half of the reservation
    /// ledger (the cross-process half is the owner database's quota_reservation rows).
    reserved_in_flight: AtomicU64,
    /// Wakes the worker: a submit, a close's withdrawal, or a fail that must end it.
    worker_wake: tokio::sync::Notify,
    /// Wakes barriers and the drain loop when writes complete.
    progressed: tokio::sync::Notify,
}

impl ModuleHistoryStore {
    /// Pure construction — no I/O of any kind (the factory's contract).
    pub fn new(id: ModuleId, policy: OwnerPolicy) -> Arc<Self> {
        Arc::new(ModuleHistoryStore {
            id,
            policy,
            inner: Mutex::new(StoreInner {
                state: OwnerState::Disabled,
                queue: QueueInner::default(),
            }),
            generation: AtomicU64::new(0),
            instance: swiss_core::util::random_hex(8),
            quota: std::sync::RwLock::new(None),
            reserved_in_flight: AtomicU64::new(0),
            worker_wake: tokio::sync::Notify::new(),
            progressed: tokio::sync::Notify::new(),
        })
    }

    pub fn id(&self) -> &ModuleId {
        &self.id
    }

    /// The current generation: 0 before the first open, bumped by every open. Carried by
    /// queued writes so a stale one can never execute into a newer generation. This is
    /// the WORKER IDENTITY sequence: the drain loop compares it to its own - owners that
    /// need to expire scan snapshots (a clear) bump their OWN snapshot counter instead,
    /// never this one.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The owner's declared policy (path, bounds, durability), for diagnostics and usage.
    pub fn policy(&self) -> &OwnerPolicy {
        &self.policy
    }

    /// The observed engine identity this store opened under, when open.
    pub fn engine_caps(&self) -> Option<EngineCaps> {
        match &self.inner.lock().unwrap_or_else(|e| e.into_inner()).state {
            OwnerState::Open { caps, .. } => Some(caps.clone()),
            _ => None,
        }
    }

    /// Make the store live: engine gate, pool, schema migration, worker. A failure before
    /// any file was touched leaves the store Disabled; after, Failed. A fresh open bumps
    /// the generation, which retires every still-queued stale write.
    pub async fn open(self: &Arc<Self>) -> Result<(), OwnerLifecycleError> {
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.state {
                OwnerState::Disabled | OwnerState::Closed | OwnerState::Failed { .. } => {}
                ref other => {
                    return Err(OwnerLifecycleError::WrongState {
                        module: self.id.to_string(),
                        state: other.label(),
                    })
                }
            }
        }

        // The engine gate BEFORE any file exists: an engine this build cannot vouch for
        // never creates so much as an empty database.
        let caps = engine::probe().await.map_err(|e| OwnerLifecycleError::OpenFailed {
            module: self.id.to_string(),
            reason: format!("engine probe: {e}"),
        })?;
        caps.history_ready().map_err(|e| OwnerLifecycleError::OpenFailed {
            module: self.id.to_string(),
            reason: e.to_string(),
        })?;

        let opts = SqliteConnectOptions::new()
            .filename(&self.policy.db_file)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(match self.policy.durability {
                Durability::Normal => SqliteSynchronous::Normal,
                Durability::Full => SqliteSynchronous::Full,
            });
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| OwnerLifecycleError::OpenFailed {
                module: self.id.to_string(),
                reason: format!("open {}: {e}", self.policy.db_file.display()),
            })?;
        if let Err(e) = schema::migrate(&pool, self.policy.schema).await {
            pool.close().await;
            return Err(OwnerLifecycleError::OpenFailed {
                module: self.id.to_string(),
                reason: e.to_string(),
            });
        }

        let new_generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.state = OwnerState::Open { pool: pool.clone(), caps };
        }
        // The worker serves exactly its own generation: a reopen's worker retires any
        // survivor of a failed close instead of racing it for the queue.
        tokio::spawn(Self::worker(self.clone(), new_generation));
        self.worker_wake.notify_one();
        Ok(())
    }

    /// Enroll this owner in the physical quota (SPEC host.history): admission runs
    /// from now on against the declared share, and maintain() refreshes the usage
    /// snapshot. Called by the owner facade at open, never mid-flight.
    pub fn set_quota(&self, policy: crate::history::quota::QuotaPolicy, protected_window: bool) {
        let usage = crate::history::quota::measure(&self.policy.db_file);
        let mut quota = self.quota.write().unwrap_or_else(|e| e.into_inner());
        *quota = Some(QuotaState {
            policy,
            protected_window,
            usage,
        });
    }

    /// The enrolled share in bytes, None before enrollment: what an owner's own size
    /// trim aims under (the store denies past it but never deletes on its own).
    pub fn share(&self) -> Option<u64> {
        let quota = self.quota.read().unwrap_or_else(|e| e.into_inner());
        quota.as_ref().map(|state| state.policy.share_for(&self.id.0))
    }

    /// Admit one write against the CURRENT generation. Bounded, synchronous admission:
    /// the queue either takes the item now or refuses it; callers never block on the
    /// database.
    pub fn submit<F>(&self, estimated_bytes: usize, job: F) -> Result<(), HistoryWriteError>
    where
        F: FnOnce(SqlitePool) -> BoxFuture<'static, Result<(), String>> + Send + 'static,
    {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Only Open admits: withdrawal (Stopping) and every dead state refuse new work
        // before looking at the queue.
        if !matches!(inner.state, OwnerState::Open { .. }) {
            return Err(HistoryWriteError::NotOpen {
                module: self.id.to_string(),
                state: inner.state.label(),
            });
        }
        if inner.queue.bytes + estimated_bytes > self.policy.queue_byte_bound {
            return Err(HistoryWriteError::QueueFull {
                module: self.id.to_string(),
                bound: self.policy.queue_byte_bound,
            });
        }
        // Physical admission (SPEC host.history quotas): the declared share answers to
        // the owner, the projected bytes are the measured file plus what this process
        // has admitted but not yet landed. Both denial paths refuse; the protected
        // window only changes WHICH honest reason the wire carries.
        {
            let quota = self.quota.read().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = quota.as_ref() {
                let mut projected = state.usage;
                projected.reserved_bytes += self.reserved_in_flight.load(Ordering::Acquire);
                if let Err(reason) = state.policy.admits(
                    &self.id.0,
                    projected,
                    estimated_bytes as u64,
                    state.protected_window,
                ) {
                    return Err(HistoryWriteError::QuotaDenied {
                        module: self.id.to_string(),
                        reason: reason.as_str().to_string(),
                    });
                }
            }
        }
        self.reserved_in_flight
            .fetch_add(estimated_bytes as u64, Ordering::AcqRel);
        inner.queue.bytes += estimated_bytes;
        inner.queue.admitted += 1;
        let generation = self.generation.load(Ordering::Acquire);
        // Instance-scoped: the ledger row must exist per WRITE, and two processes
        // mint the same "{generation}-{admitted}" - the boot id keeps the rows of
        // concurrent processes distinct so reserved_total counts both flights.
        let token = format!("{}-{generation}-{}", self.instance, inner.queue.admitted);
        inner.queue.items.push_back(QueuedWrite {
            generation,
            estimated_bytes,
            token,
            job: Box::new(job),
        });
        drop(inner);
        self.worker_wake.notify_one();
        Ok(())
    }

    /// Admit one write that outranks the byte budget: the session-END stamp and
    /// the retention sweep. Both are tiny, both are the mechanism that keeps the
    /// store's own invariants alive - a quota that refuses the finish stamp leaves
    /// a session looking live forever, and a quota that refuses the sweep can
    /// never reclaim the bytes it is policing (the share wedges with no exit).
    /// Everything else about admission is unchanged: Only Open admits, and the
    /// queue bound still applies.
    pub fn submit_exempt<F>(&self, estimated_bytes: usize, job: F) -> Result<(), HistoryWriteError>
    where
        F: FnOnce(SqlitePool) -> BoxFuture<'static, Result<(), String>> + Send + 'static,
    {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(inner.state, OwnerState::Open { .. }) {
            return Err(HistoryWriteError::NotOpen {
                module: self.id.to_string(),
                state: inner.state.label(),
            });
        }
        if inner.queue.bytes + estimated_bytes > self.policy.queue_byte_bound {
            return Err(HistoryWriteError::QueueFull {
                module: self.id.to_string(),
                bound: self.policy.queue_byte_bound,
            });
        }
        self.reserved_in_flight
            .fetch_add(estimated_bytes as u64, Ordering::AcqRel);
        inner.queue.bytes += estimated_bytes;
        inner.queue.admitted += 1;
        let generation = self.generation.load(Ordering::Acquire);
        let token = format!("{}-{generation}-{}", self.instance, inner.queue.admitted);
        inner.queue.items.push_back(QueuedWrite {
            generation,
            estimated_bytes,
            token,
            job: Box::new(job),
        });
        drop(inner);
        self.worker_wake.notify_one();
        Ok(())
    }

    /// One bounded maintenance pass over the owner's database (SPEC host.history):
    /// the passive checkpoint + conditional VACUUM, then the usage snapshot refresh
    /// and, when the local queue is fully drained, the reservation ledger's
    /// reconciliation (a crashed process's stale tokens cannot outlive one maintenance
    /// interval of quiescence). Refuses like any query when the owner is not open.
    pub async fn maintain(
        &self,
    ) -> Result<crate::history::quota::MaintainReceipt, HistoryQueryError> {
        let db_file = self.policy.db_file.clone();
        let receipt = self
            .query(move |pool| {
                Box::pin(async move {
                    crate::history::quota::ensure_reservation_table(&pool).await?;
                    crate::history::quota::maintain(&pool, &db_file).await
                })
            })
            .await?;
        let (admitted, completed, _, _) = self.counters();
        if admitted == completed {
            // Quiescence was CHECKED, not held: a write admitted after the counter
            // snapshot can land its row between the check and the DELETE, and a
            // LIVE peer's row is indistinguishable from a crashed one from here.
            // Both are absorbed by the documented soft budget: this process's own
            // admission is never affected (its counter is in-process), and the
            // deleted rows are collateral the next usage refresh re-prices.
            let _ = self
                .query(|pool| {
                    Box::pin(async move {
                        sqlx::query("DELETE FROM quota_reservation")
                            .execute(&pool)
                            .await
                            .map(|_| ())
                    })
                })
                .await;
        }
        let mut usage = crate::history::quota::measure(&self.policy.db_file);
        if let Ok(reserved) = self
            .query(|pool| {
                Box::pin(async move { crate::history::quota::reserved_total(&pool).await })
            })
            .await
        {
            usage.reserved_bytes = reserved;
        }
        let mut quota = self.quota.write().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = quota.as_mut() {
            state.usage = usage;
        }
        Ok(receipt)
    }

    /// Wait until every write admitted BEFORE this call — polled or not — has completed.
    /// Fails when any covered write failed; the reason is that range's last failure.
    pub async fn flush_barrier(&self) -> Result<(), HistoryWriteError> {
        let (target, generation) = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (inner.queue.admitted, self.generation.load(Ordering::Acquire))
        };
        loop {
            // Create the future BEFORE checking, so a completion racing this barrier
            // cannot slip past the wakeup (Notify wakes registered waiters only).
            let notified = self.progressed.notified();
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                // Refuse BEFORE any accounting says otherwise. A failed owner — or one
                // that reopened under a new generation — can reach this barrier's
                // target through stale-drop accounting: writes that were DROPPED with
                // the dead generation, never persisted. Ok must not cross either, so
                // these checks come before the completed-counter shortcut.
                if let OwnerState::Failed { reason, .. } = &inner.state {
                    return Err(HistoryWriteError::Barrier(reason.clone()));
                }
                if self.generation.load(Ordering::Acquire) != generation {
                    return Err(HistoryWriteError::Barrier(
                        "the owner reopened under this barrier: its covered writes were dropped with the dead generation".into(),
                    ));
                }
                if inner.queue.completed >= target {
                    if let Some((seq, reason)) = &inner.queue.last_failure {
                        if *seq <= target {
                            return Err(HistoryWriteError::Barrier(reason.clone()));
                        }
                    }
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    /// Run one read against the owner's pool. Never opens, never re-enables: a store that
    /// is not exactly live answers with its state, and the file stays untouched.
    pub async fn query<T, F>(&self, f: F) -> Result<T, HistoryQueryError>
    where
        F: FnOnce(SqlitePool) -> BoxFuture<'static, Result<T, sqlx::Error>>,
    {
        let pool = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.state.pool() {
                Some(pool) => pool.clone(),
                None => {
                    return Err(HistoryQueryError::NotOpen {
                        module: self.id.to_string(),
                        state: inner.state.label(),
                    })
                }
            }
        };
        f(pool).await.map_err(|e| HistoryQueryError::Failed(e.to_string()))
    }

    /// Withdraw, drain and release within the policy's deadline. On success the pool was
    /// awaited closed and the queue is empty. On the deadline the store stays Failed with
    /// the unresolved count — a truthful stopping state, never a claimed close. WAL/SHM
    /// sidecars are left in place: unlinking live sidecars is corruption, not cleanup.
    pub async fn close(&self) -> Result<(), OwnerLifecycleError> {
        let pool = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let pool = match &inner.state {
                OwnerState::Open { pool, .. } => Some(pool.clone()),
                OwnerState::Disabled => {
                    inner.state = OwnerState::Closed;
                    return Ok(());
                }
                _ => None,
            };
            match pool {
                Some(pool) => {
                    inner.state = OwnerState::Stopping { pool: pool.clone() };
                    pool
                }
                None => {
                    return Err(OwnerLifecycleError::WrongState {
                        module: self.id.to_string(),
                        state: inner.state.label(),
                    })
                }
            }
        };
        self.worker_wake.notify_one();

        let deadline = tokio::time::Instant::now() + self.policy.close_timeout;
        // Drain phase: everything admitted completes (or fails) or the deadline breaks.
        loop {
            let notified = self.progressed.notified();
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.queue.items.is_empty() && inner.queue.completed >= inner.queue.admitted {
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let unresolved = self.unresolved();
                self.fail_close("the writer did not finish in time", unresolved);
                return Err(OwnerLifecycleError::CloseDeadline {
                    module: self.id.to_string(),
                    unresolved,
                    reason: format!("drain deadline after {:?}", self.policy.close_timeout),
                });
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }

        // Release phase: awaiting the pool's close IS the evidence the SQL worker
        // released — an aborted future or a dropped Arc is not. It runs under whatever
        // budget the one deadline has left.
        match tokio::time::timeout_at(deadline, pool.close()).await {
            Ok(()) => {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.state = OwnerState::Closed;
                Ok(())
            }
            Err(_) => {
                let unresolved = self.unresolved();
                self.fail_close("the pool did not close within the deadline", unresolved);
                Err(OwnerLifecycleError::CloseDeadline {
                    module: self.id.to_string(),
                    unresolved,
                    reason: "pool release timed out".into(),
                })
            }
        }
    }

    /// Writes admitted but not completed — the close deadline's unresolved count.
    pub fn unresolved(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.queue.admitted - inner.queue.completed
    }

    /// The failed owner's reason and unresolved count at the deadline, when it failed.
    /// This is the honest stopping state: a Failed owner reports both, never "closed".
    pub fn failure_detail(&self) -> Option<(String, u64)> {
        match &self.inner.lock().unwrap_or_else(|e| e.into_inner()).state {
            OwnerState::Failed { reason, unresolved } => Some((reason.clone(), *unresolved)),
            _ => None,
        }
    }

    /// The state's label for diagnostics and tests ("disabled", "open", "stopping",
    /// "closed", "failed").
    pub fn state_label(&self) -> &'static str {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).state.label()
    }

    /// Counters for diagnostics: admitted, completed, stale-dropped, unresolved.
    pub fn counters(&self) -> (u64, u64, u64, u64) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let q = &inner.queue;
        (q.admitted, q.completed, q.stale_dropped, q.admitted - q.completed)
    }

    fn fail_close(&self, reason: &str, unresolved: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.state = OwnerState::Failed { reason: reason.into(), unresolved };
        drop(inner);
        // Wake the worker so a parked loop observes the failure and exits instead of
        // outliving the store it serves.
        self.worker_wake.notify_one();
    }

    /// The drain loop: one task per open, serving exactly its own generation. Exits when
    /// withdrawal finds the queue empty, when a newer generation takes over, or
    /// immediately when the store failed or closed — a failed write completes its item
    /// with an error; it never stalls the drain.
    async fn worker(self: Arc<Self>, generation: u64) {
        loop {
            if self.generation.load(Ordering::Acquire) != generation {
                return;
            }
            let next = {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(item) = inner.queue.items.pop_front() {
                    inner.queue.bytes = inner.queue.bytes.saturating_sub(item.estimated_bytes);
                    let current = self.generation.load(Ordering::Acquire);
                    let dead_state = !matches!(
                        inner.state,
                        OwnerState::Open { .. } | OwnerState::Stopping { .. }
                    );
                    if item.generation != current || dead_state {
                        // A write from a dead generation or a dead owner: accounted as
                        // dropped and completed — never executed, never a hung barrier.
                        inner.queue.stale_dropped += 1;
                        inner.queue.completed += 1;
                        // Its in-flight hold ends here; the ledger row was never written.
                        self.reserved_in_flight
                            .fetch_sub(item.estimated_bytes as u64, Ordering::AcqRel);
                        WorkerNext::Dropped
                    } else {
                        let pool = inner
                            .state
                            .pool()
                            .expect("open or stopping always holds a pool")
                            .clone();
                        WorkerNext::Job((pool, item))
                    }
                } else {
                    match inner.state {
                        OwnerState::Open { .. } => WorkerNext::Idle,
                        // Withdrawn and drained: the close's release phase takes it from
                        // here, and this task ends.
                        OwnerState::Stopping { .. }
                        | OwnerState::Closed
                        | OwnerState::Failed { .. }
                        | OwnerState::Disabled => return,
                    }
                }
            };
            match next {
                WorkerNext::Idle => self.worker_wake.notified().await,
                WorkerNext::Dropped => self.progressed.notify_waiters(),
                WorkerNext::Job((pool, item)) => {
                    // The reservation ledger (SPEC host.history): the token's row exists
                    // exactly while the write is between execution and completion (the
                    // submit queue itself costs nothing), so every process with this
                    // file open spends the same headroom. The table is ensured here,
                    // lazily: a database whose first-ever write has not run yet has no
                    // ledger, and creating it at open would make an empty db file cost
                    // a write cycle.
                    let ledger_pool = pool.clone();
                    let _ = crate::history::quota::ensure_reservation_table(&pool).await;
                    let _ = crate::history::quota::reserve(
                        &pool,
                        &item.token,
                        item.estimated_bytes as u64,
                    )
                    .await;
                    // A panicking job must not take the worker (and with it every
                    // barrier, and with those the hourly pass of every owner) down:
                    // isolate like the maintenance hooks do, account it as a failed
                    // write. The Dropped branch's release/sub/completed shape runs
                    // on BOTH arms - the ledger row and the in-flight hold end here
                    // whatever the outcome was.
                    let outcome = futures_util::FutureExt::catch_unwind(
                        std::panic::AssertUnwindSafe((item.job)(pool)),
                    )
                    .await
                    .unwrap_or_else(|panic| {
                        let reason = panic
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "an unknown panic".to_string());
                        swiss_core::log::error(
                            "a history write job panicked; the queue continues",
                            Some(serde_json::json!({
                                "module": self.id.0.as_ref(),
                                "reason": reason,
                            })),
                        );
                        Err(format!("the write job panicked: {reason}"))
                    });
                    let _ = crate::history::quota::release(&ledger_pool, &item.token).await;
                    let reserved = crate::history::quota::reserved_total(&ledger_pool).await;
                    // Replace the estimate with physical bytes before releasing the
                    // local hold. Compression, duplicate inserts and reclamation do
                    // not spend their logical size; failures may still grow the WAL.
                    // The ledger read retains any peer's outstanding reservations.
                    {
                        let mut quota = self.quota.write().unwrap_or_else(|e| e.into_inner());
                        if let Some(state) = quota.as_mut() {
                            let mut usage = crate::history::quota::measure(&self.policy.db_file);
                            usage.reserved_bytes = reserved.unwrap_or(state.usage.reserved_bytes);
                            state.usage = usage;
                        }
                        self.reserved_in_flight
                            .fetch_sub(item.estimated_bytes as u64, Ordering::AcqRel);
                    }
                    // A failed write is said in the log, not only kept for a barrier
                    // to read: most writes are queued and forgotten, and a row that
                    // silently never lands (a conversation's settling call, say)
                    // must leave a reason someone can find.
                    if let Err(reason) = &outcome {
                        swiss_core::log::warn(
                            "a history write failed",
                            Some(serde_json::json!({
                                "module": self.id.0.as_ref(),
                                "reason": reason,
                            })),
                        );
                    }
                    {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.queue.completed += 1;
                        if let Err(reason) = outcome {
                            inner.queue.last_failure = Some((inner.queue.completed, reason));
                        }
                    }
                    self.progressed.notify_waiters();
                }
            }
        }
    }
}

enum WorkerNext {
    Idle,
    Dropped,
    Job((SqlitePool, QueuedWrite)),
}
