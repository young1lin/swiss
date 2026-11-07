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

//! L2/L3/L4 — host log-store tests (SPEC §host.history): owners are isolated in
//! life and death, generations retire stale writes, barriers cover unpolled admissions,
//! and a stop that misses its deadline never claims to have closed.

use std::time::Duration;

use futures_core::future::BoxFuture;
use sqlx::{Row, SqlitePool};
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{
    HistoryQueryError, HistoryWriteError, ModuleHistoryStore, OwnerPolicy,
};

static NOTES: &[SchemaStep] =
    &[SchemaStep::new(1, &["CREATE TABLE note (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"])];

/// One directory per CALL, never per binary: these tests run concurrently, and they once
/// shared `test_home()/logs` — until the two-stores case below wiped that shared dir
/// mid-flight and seven siblings died inside `create_dir_all` (ERROR_ALREADY_EXISTS /
/// ERROR_ACCESS_DENIED) while their databases were open in the wiped directory. A private
/// dir per call makes one test's filesystem footprint unable to touch another's, open
/// database or not.
fn scratch_dir() -> std::path::PathBuf {
    // A per-process counter beside the clock: Windows' coarse timer can hand two parallel
    // calls the same nanos, and uniqueness must not rest on its resolution.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "swiss-history-store-{}-{}-{seq}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).expect("create the private scratch dir");
    dir
}

fn scratch_policy(name: &str) -> OwnerPolicy {
    OwnerPolicy::for_tests(scratch_dir().join(name), NOTES)
}

/// The guard for the comment above: if this fails, someone handed the tests a directory
/// they share again, and a sibling's cleanup can delete another test's open database.
#[test]
fn the_scratch_dir_is_private_to_each_call() {
    let a = scratch_policy("guard-a.db");
    let b = scratch_policy("guard-b.db");
    assert_ne!(
        a.db_file.parent(),
        b.db_file.parent(),
        "scratch dirs must be unique per call: a shared dir lets one test delete another's database"
    );
}

fn factory() -> std::sync::Arc<HistoryStoreFactory> {
    HistoryStoreFactory::new()
}

/// A write that inserts one note row. One transaction per item: the store owns admission
/// and ordering, the job owns its SQL.
fn insert_note(text: String) -> impl FnOnce(SqlitePool) -> BoxFuture<'static, Result<(), String>> {
    move |pool| {
        Box::pin(async move {
            sqlx::query("INSERT INTO note (text) VALUES (?)")
                .bind(&text)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}

/// A write that waits for its gate, then records that it ran without touching the
/// database — the stuck-writer shape a close deadline must survive, made observable
/// without depending on the retired pool still accepting writes.
fn gated_marker(
    ran: std::sync::Arc<std::sync::atomic::AtomicU32>,
    gate: tokio::sync::oneshot::Receiver<()>,
) -> impl FnOnce(SqlitePool) -> BoxFuture<'static, Result<(), String>> {
    move |_pool| {
        Box::pin(async move {
            let _ = gate.await;
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}

async fn note_count(store: &ModuleHistoryStore) -> i64 {
    store
        .query(move |pool| {
            Box::pin(async move {
                let row = sqlx::query("SELECT count(*) FROM note").fetch_one(&pool).await?;
                row.try_get(0)
            })
        })
        .await
        .expect("query the live store")
}

#[tokio::test]
async fn closing_one_module_keeps_other_module_writes_and_reads_live() {
    let f = factory();
    let mcp = f.create("mcp", scratch_policy("l2-mcp.db"));
    let remote = f.create("remote", scratch_policy("l2-remote.db"));

    mcp.open().await.expect("open mcp");
    remote.open().await.expect("open remote");
    mcp.submit(64, insert_note("mcp one".into())).expect("admit mcp");
    remote.submit(64, insert_note("remote one".into())).expect("admit remote");
    mcp.flush_barrier().await.expect("mcp flush");
    remote.flush_barrier().await.expect("remote flush");
    assert_eq!(note_count(&mcp).await, 1);
    assert_eq!(note_count(&remote).await, 1);

    // Stopping MCP is MCP's own business.
    mcp.close().await.expect("close mcp");

    // The survivor keeps writing, flushing and reading.
    remote.submit(64, insert_note("remote two".into())).expect("admit after the sibling closed");
    remote.flush_barrier().await.expect("flush after the sibling closed");
    assert_eq!(note_count(&remote).await, 2);

    // The closed owner refuses without touching its file.
    let err = mcp.submit(64, insert_note("no".into())).unwrap_err();
    assert!(
        matches!(err, HistoryWriteError::NotOpen { state: "closed", .. }),
        "{err}"
    );
    let err = mcp
        .query(move |pool| {
            Box::pin(async move { sqlx::query("SELECT count(*) FROM note").fetch_one(&pool).await.map(|_| 0) })
        })
        .await
        .unwrap_err();
    assert!(matches!(err, HistoryQueryError::NotOpen { state: "closed", .. }), "{err}");
    remote.close().await.expect("close remote");
}

#[tokio::test]
async fn failed_open_rolls_back_only_its_module() {
    let f = factory();
    // A database file whose parent directory does not exist: create_if_missing cannot make
    // directories, so the open fails before a file appears.
    let nowhere = scratch_dir().join("does-not-exist").join("broken.db");
    let broken = f.create("broken", OwnerPolicy::for_tests(nowhere, NOTES));
    let healthy = f.create("healthy", scratch_policy("l2-open-ok.db"));

    let err = broken.open().await.unwrap_err();
    assert!(
        err.to_string().contains("opening history for broken failed"),
        "{err}"
    );
    assert_eq!(broken.state_label(), "disabled", "a failed open returns to disabled, never open");
    assert!(
        !broken.policy().db_file.exists(),
        "no half-created database file survived"
    );

    // The unrelated owner opened, wrote and read normally through the sibling's failure.
    healthy.open().await.expect("open healthy");
    healthy.submit(64, insert_note("still here".into())).expect("admit healthy");
    healthy.flush_barrier().await.expect("flush healthy");
    assert_eq!(note_count(&healthy).await, 1);
    healthy.close().await.expect("close healthy");
}

#[tokio::test]
async fn flush_barrier_includes_unpolled_accepted_writes() {
    let f = factory();
    let store = f.create("mcp", scratch_policy("l3-barrier.db"));
    store.open().await.expect("open");

    // On the current-thread runtime the worker has not run once yet: every admission below
    // is unpolled. The barrier must still cover them all.
    for i in 0..25 {
        store.submit(64, insert_note(format!("note {i}"))).expect("admit");
    }
    store.flush_barrier().await.expect("the barrier covers unpolled admissions");
    assert_eq!(note_count(&store).await, 25, "every accepted write executed exactly once");
    let (admitted, completed, stale, unresolved) = store.counters();
    assert_eq!((admitted, completed, stale, unresolved), (25, 25, 0, 0));
    store.close().await.expect("close");
}

#[tokio::test]
async fn stale_generation_handles_cannot_write_after_close() {
    let f = factory();
    // The default 2 s deadline, not a fast one: the FIRST close must still miss it (the
    // gated write never completes), but this test's final close must succeed, and a pool
    // release on a loaded CI runner can outlast a deadline meant only for the first
    // ("pool release timed out" with nothing unresolved, twice on windows runners).
    let store = f.create("mcp", scratch_policy("l3-stale.db"));
    store.open().await.expect("open");
    let first_generation = store.generation();

    // Item one blocks the single worker; item two waits behind it, admitted but unpolled.
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    store.submit(64, gated_marker(ran.clone(), gate_rx)).expect("admit the gated write");
    store.submit(64, insert_note("queued behind".into())).expect("admit the follower");
    // Give the worker one await so it pops the gated item (still blocked inside).
    let _ = tokio::time::timeout(Duration::from_millis(50), store.flush_barrier()).await;

    let close_err = store.close().await.unwrap_err();
    assert!(
        close_err.to_string().contains("hit its deadline"),
        "{close_err}"
    );
    assert_eq!(store.state_label(), "failed", "the deadline left it failed, never closed");
    let (_, unresolved_at_failure) = store.failure_detail().expect("failure detail");
    assert_eq!(unresolved_at_failure, 2, "both accepted writes were unresolved");

    // Old handles (this same long-lived Arc is what the facades hold) cannot write.
    let err = store.submit(64, insert_note("stale".into())).unwrap_err();
    assert!(matches!(err, HistoryWriteError::NotOpen { state: "failed", .. }), "{err}");

    // Reopen: a new generation, and the follower still queued from the dead generation is
    // dropped, never executed into the new one.
    store.open().await.expect("reopen from failed");
    assert_eq!(store.generation(), first_generation + 1, "the generation moved on");
    store.submit(64, insert_note("fresh generation".into())).expect("admit into the new generation");

    // Release the gate BEFORE any barrier: the stuck write completes once, against its
    // own (retired) pool handle — it was accepted under the old generation and its late
    // completion is the truth of an overrun, not a mutation of the new epoch.
    drop(gate_tx);
    store.flush_barrier().await.expect("the barrier covers the whole admitted history");
    assert_eq!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the accepted write completed exactly once"
    );

    // The follower was dropped as stale; only the fresh-generation write is in the table.
    assert_eq!(note_count(&store).await, 1);
    let (admitted, completed, stale, _) = store.counters();
    assert_eq!(stale, 1, "the dead generation's queued write was accounted, not executed");
    assert_eq!(admitted, 3);
    assert!(completed >= 3, "gated, stale drop and fresh write all completed: {completed}");

    store.close().await.expect("close after reopen");
}

#[tokio::test]
async fn stop_deadline_failure_never_claims_disabled_or_closed() {
    let f = factory();
    let mut policy = scratch_policy("l4-deadline.db");
    policy.close_timeout = Duration::from_millis(120);
    let store = f.create("remote", policy);
    store.open().await.expect("open");

    let (_gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let never_ran = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    store
        .submit(64, gated_marker(never_ran.clone(), gate_rx))
        .expect("admit");
    let _ = tokio::time::timeout(Duration::from_millis(40), store.flush_barrier()).await;

    let err = store.close().await.unwrap_err();
    assert!(err.to_string().contains("unresolved"), "{err}");
    // The honest states after a missed deadline: failed with a reason and a count —
    // never "disabled" (it was live) and never "closed" (the pool was not released).
    assert_eq!(store.state_label(), "failed");
    let (reason, unresolved) = store.failure_detail().expect("failure detail");
    assert!(!reason.is_empty());
    assert_eq!(unresolved, 1);
}

#[tokio::test]
async fn disabled_storage_queries_never_open_or_reenable() {
    let f = factory();
    let policy = scratch_policy("l4-disabled.db");
    let db_file = policy.db_file.clone();
    let store = f.create("remote", policy);

    // Before any open: writes and queries refuse, and the refusal must not create so much
    // as an empty database file.
    let err = store.submit(64, insert_note("no".into())).unwrap_err();
    assert!(matches!(err, HistoryWriteError::NotOpen { state: "disabled", .. }), "{err}");
    let err = store
        .query(move |pool| {
            Box::pin(async move { sqlx::query("SELECT 1").fetch_one(&pool).await.map(|_| 0) })
        })
        .await
        .unwrap_err();
    assert!(matches!(err, HistoryQueryError::NotOpen { state: "disabled", .. }), "{err}");
    assert!(
        !db_file.exists(),
        "a disabled owner's refusal must not create its database file"
    );

    // Close of a never-opened store is a clean no-op that still does not open anything.
    store.close().await.expect("closing a disabled store is clean");
    assert_eq!(store.state_label(), "closed");
    assert!(!db_file.exists());
}


// --- code-review C4: a barrier never Ok across a generation -----------------------------------------

#[tokio::test]
async fn a_barrier_never_claims_persistence_across_a_failed_reopen() {
    let f = factory();
    let mut policy = scratch_policy("c4-barrier-gen.db");
    policy.close_timeout = Duration::from_millis(150);
    let store = f.create("mcp", policy);
    store.open().await.expect("open");

    // The gated write blocks the worker; the follower waits queued behind it.
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    store.submit(64, gated_marker(ran.clone(), gate_rx)).expect("admit the gated write");
    store.submit(64, insert_note("queued behind".into())).expect("admit the follower");
    // Park the barrier: it covers BOTH writes and must not resolve until they do.
    let mut barrier = std::pin::pin!(store.flush_barrier());
    let _ = tokio::time::timeout(Duration::from_millis(50), barrier.as_mut()).await;

    // The close misses its deadline: failed, then reopened under a NEW generation.
    store.close().await.expect_err("the gated write holds the deadline open");
    assert_eq!(store.state_label(), "failed");
    store.open().await.expect("reopen from failed");

    // The follower from the dead generation is stale-dropped (completed moves), and a
    // fresh write executes into the new generation. Counters can reach the parked
    // barrier's target — but the gated write NEVER executed and the follower was
    // dropped. The barrier must refuse, never resolve Ok on accounting alone.
    store.submit(64, insert_note("fresh generation".into())).expect("admit into the new generation");
    drop(gate_tx);
    store.flush_barrier().await.expect("the accounted writes flush");

    let verdict = tokio::time::timeout(Duration::from_millis(500), barrier.as_mut()).await;
    let verdict = verdict.expect("the parked barrier resolves");
    assert!(verdict.is_err(), "a barrier that crossed a failed reopen must fail, never Ok: {verdict:?}");
    store.close().await.ok();
}

// --- C7: the physical quota is wired into admission, the ledger, maintenance ------

/// Admission runs at submit: a share the projection would exceed is refused with the
/// honest reason, and the store never queues the write.
#[tokio::test]
async fn a_write_past_the_declared_share_is_refused_at_admission() {
    let store = factory().create("notes", scratch_policy("quota-denied"));
    store.open().await.expect("open");
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), 64u64);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    let refused = store.submit(4_096, insert_note("too big".into()));
    match refused {
        Err(HistoryWriteError::QuotaDenied { reason, .. }) => {
            assert_eq!(reason, "over-owner-share");
        }
        other => panic!("the write must refuse at admission, got {other:?}"),
    }
    // The refusal left nothing in flight and nothing stored.
    store.flush_barrier().await.expect("barrier");
    assert_eq!(note_count(&store).await, 0);
    let _ = store.close().await;
}

/// The reservation ledger is real: an admitted write holds its row while it runs and
/// releases it on completion, so the quiescent total is zero.
#[tokio::test]
async fn a_completed_write_releases_its_reservation() {
    let store = factory().create("notes", scratch_policy("quota-ledger"));
    store.open().await.expect("open");
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), 50_000_000u64);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    store.submit(64, insert_note("held then released".into())).expect("admit");
    store.flush_barrier().await.expect("barrier");
    let reserved = store
        .query(|pool| {
            Box::pin(async move {
                swiss_host::history::quota::reserved_total(&pool).await
            })
        })
        .await
        .expect("the ledger table exists after a write");
    assert_eq!(reserved, 0, "a completed write leaves no reservation behind");
    let _ = store.close().await;
}

/// Maintenance reconciles the ledger: once this process is quiescent, a token a
/// crashed peer left behind cannot outlive the pass (SPEC host.history).
#[tokio::test]
async fn maintain_reconciles_a_crashed_peers_stale_token() {
    let store = factory().create("notes", scratch_policy("quota-reconcile"));
    store.open().await.expect("open");
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), 50_000_000u64);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    store
        .query(|pool| {
            Box::pin(async move {
                swiss_host::history::quota::ensure_reservation_table(&pool).await?;
                swiss_host::history::quota::reserve(&pool, "crashed-peer-1", 4_096).await
            })
        })
        .await
        .expect("seed the stale token");
    let receipt = store.maintain().await.expect("maintain");
    assert!(receipt.db_bytes_after > 0, "the pass ran against a live file");
    let reserved = store
        .query(|pool| {
            Box::pin(async move { swiss_host::history::quota::reserved_total(&pool).await })
        })
        .await
        .expect("read the ledger");
    assert_eq!(reserved, 0, "the stale token is gone after one quiescent pass");
    let _ = store.close().await;
}

/// Completion sees this worker's writes; maintenance must also discover a peer's
/// later writes to the same database, without an intervening local write.
#[tokio::test]
async fn maintain_refreshes_physical_growth_from_a_peer() {
    let db = scratch_dir().join("quota-peer-refresh.db");
    let a = factory().create("notes", OwnerPolicy::for_tests(db.clone(), NOTES));
    let b = factory().create("notes", OwnerPolicy::for_tests(db.clone(), NOTES));
    a.open().await.expect("first owner");
    b.open().await.expect("peer owner");
    let baseline = swiss_host::history::quota::measure(&db).total();
    let share = baseline + 262_144;
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), share);
    a.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    b.submit(524_288, insert_note("x".repeat(524_288))).expect("the peer is not enrolled");
    b.flush_barrier().await.expect("peer payload lands");
    a.maintain().await.expect("refresh peer growth");
    assert!(swiss_host::history::quota::measure(&db).total() > share);
    assert!(matches!(
        a.submit(512, insert_note("past the refreshed share".into())),
        Err(HistoryWriteError::QuotaDenied { .. }),
    ), "maintenance must replace the opening snapshot before the next admission");
    a.close().await.expect("close first owner");
    b.close().await.expect("close peer");
}


/// The projection is BOTH sides: the measured usage alone stays under the share here,
/// so this denial can only fire through the estimated in-flight bytes - a projection
/// that dropped either half would admit.
#[tokio::test]
async fn admission_projects_the_measured_and_the_estimated_bytes_together() {
    let policy = scratch_policy("quota-projection");
    let db_file = policy.db_file.clone();
    let store = factory().create("notes", policy);
    store.open().await.expect("open");
    // One small write lands, so the measured usage is real and non-zero.
    store.submit(512, insert_note("seed".into())).expect("seed admits");
    store.flush_barrier().await.expect("seed lands");
    let measured = swiss_host::history::quota::measure(&db_file).total();
    assert!(measured > 0, "the seed write left physical bytes");
    // A share the MEASURED bytes alone fit under, but the projected write crosses:
    // only the estimate side can refuse this one.
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), measured + 1_024);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    let refused = store.submit(4_096, insert_note("too big".into()));
    match refused {
        Err(HistoryWriteError::QuotaDenied { reason, .. }) => {
            assert_eq!(reason, "over-owner-share");
        }
        other => panic!("the projected write must refuse, got {other:?}"),
    }
    // The same share admits a write that fits the projection - the refusal above was
    // the projection, not a closed door.
    store
        .submit(512, insert_note("fits".into()))
        .expect("a fitting write admits");
    store.flush_barrier().await.expect("the fitting write lands");
    assert_eq!(note_count(&store).await, 2);
    let _ = store.close().await;
}

/// Conservative admission estimates expire when the bytes land. Small or compressed
/// writes must not exhaust a physical share merely by accumulating logical traffic.
#[tokio::test]
async fn completed_estimates_are_replaced_by_physical_usage() {
    let policy = scratch_policy("quota-physical-completion");
    let db_file = policy.db_file.clone();
    let store = factory().create("notes", policy);
    store.open().await.expect("open");
    store.submit(512, insert_note("seed".into())).expect("seed");
    store.flush_barrier().await.expect("seed lands and initializes the ledger");
    let estimate = 1_048_576usize;
    let share = swiss_host::history::quota::measure(&db_file).total() + 3 * estimate as u64;
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), share);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    for n in 0..8 {
        let physical = swiss_host::history::quota::measure(&db_file).total();
        assert!(physical + (estimate as u64) < share, "the next write really fits");
        store.submit(estimate, insert_note(format!("small landed row {n}")))
            .expect("completed estimates cannot consume physical headroom");
        store.flush_barrier().await.expect("the row lands");
    }
    assert_eq!(note_count(&store).await, 9);
    let _ = store.close().await;
}

/// Landed physical bytes remain in the next projection after their reservations end.
#[tokio::test]
async fn completed_writes_stay_spent_in_the_projection() {
    let policy = scratch_policy("quota-handoff");
    let db_file = policy.db_file.clone();
    let store = factory().create("notes", policy);
    store.open().await.expect("open");
    // Admit both before polling the worker, then test admission after their real
    // payloads land. A stale opening snapshot would wrongly admit the third write.
    let measured = swiss_host::history::quota::measure(&db_file).total();
    let mut shares = std::collections::BTreeMap::new();
    let payload = "x".repeat(131_072);
    shares.insert("notes".to_string(), measured + 2 * 131_072 + 512);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    store.submit(131_072, insert_note(payload.clone())).expect("the first admits");
    store.submit(131_072, insert_note(payload.clone())).expect("the second admits beside it");
    store.flush_barrier().await.expect("both land");
    match store.submit(131_072, insert_note(payload)) {
        Err(HistoryWriteError::QuotaDenied { reason, .. }) => assert_eq!(reason, "over-owner-share"),
        Ok(()) => panic!("the third write must see the first two spent"),
        other => panic!("unexpected: {other:?}"),
    }
    store.flush_barrier().await.expect("barrier");
    assert_eq!(note_count(&store).await, 2);
    let _ = store.close().await;
}

/// A failed write releases its estimate. Ledger/WAL metadata still costs its measured
/// physical bytes; it cannot retain the failed payload's much larger reservation.
#[tokio::test]
async fn a_failed_write_spends_nothing_of_the_share() {
    let policy = scratch_policy("quota-failed");
    let db_file = policy.db_file.clone();
    let store = factory().create("notes", policy);
    store.open().await.expect("open");
    let measured = swiss_host::history::quota::measure(&db_file).total();
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), measured + 1_048_576 + 65_536);
    store.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    store
        .submit(1_048_576, |_pool| {
            Box::pin(async move { Err::<(), String>("boom: nothing lands".into()) })
        })
        .expect("the failing write admits");
    // An error barrier still waits for its covered writes to complete.
    store.flush_barrier().await.expect_err("the covered write failed");
    store
        .submit(1_048_576, insert_note("after the failure".into()))
        .expect("a failed write spent none of the share");
    store.flush_barrier().await.expect_err("the earlier failure remains covered, but the new write completed");
    assert_eq!(note_count(&store).await, 1, "the clean write lands");
    let _ = store.close().await;
}




/// Two processes holding the same file open both start their generation and
/// admitted counters from the same values, so a "{generation}-{admitted}" token
/// collides across processes and INSERT OR REPLACE would collapse two in-flight
/// writes into one ledger row - undercounting exactly the headroom the ledger
/// exists to share. The boot instance id keeps the rows distinct.
#[tokio::test]
async fn two_stores_on_one_file_keep_their_reservation_rows_distinct() {
    // This test's own private dir (scratch_dir above): the file cannot pre-exist, so the
    // shared-dir wipe this test once performed is gone with it — a wipe that raced every
    // sibling's open database in the same binary. The two stores still share ONE file; the
    // reservation semantics under test are untouched.
    let db = scratch_dir().join("quota-two-stores.db");
    let a = factory().create("notes", OwnerPolicy::for_tests(db.clone(), NOTES));
    let b = factory().create("notes", OwnerPolicy::for_tests(db, NOTES));
    a.open().await.expect("open the first store");
    b.open().await.expect("open the second store");
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("notes".to_string(), 50_000_000u64);
    a.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares.clone()), false);
    b.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares), false);
    // Both stores are at generation 1, admitted 1: identical pre-fix tokens. The
    // writes are gated so both stay in flight - the ledger row exists exactly
    // while a write is executing, so the rows must be counted mid-flight.
    let (ga, ra) = tokio::sync::oneshot::channel::<()>();
    let (gb, rb) = tokio::sync::oneshot::channel::<()>();
    let ran_a = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let ran_b = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    a.submit(512, gated_marker(ran_a, ra)).expect("first store admits");
    b.submit(512, gated_marker(ran_b, rb)).expect("second store admits");
    let mut rows = 0i64;
    let mut total = 0i64;
    for _ in 0..200 {
        let seen: (i64, i64) = b
            .query(|pool| {
                Box::pin(async move {
                    // The ledger table is created lazily on first use; make sure
                    // it exists before counting (the workers may not have run yet).
                    swiss_host::history::quota::ensure_reservation_table(&pool).await?;
                    let rows: i64 =
                        sqlx::query_scalar("SELECT count(*) FROM quota_reservation")
                            .fetch_one(&pool)
                            .await?;
                    let total: i64 = sqlx::query_scalar(
                        "SELECT COALESCE(SUM(bytes), 0) FROM quota_reservation",
                    )
                    .fetch_one(&pool)
                    .await?;
                    Ok((rows, total))
                })
            })
            .await
            .expect("read the shared ledger");
        rows = seen.0;
        total = seen.1;
        if rows >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(rows, 2, "each in-flight write owns its own ledger row");
    assert_eq!(total, 1_024, "the shared headroom sees BOTH flights");
    drop(ga);
    drop(gb);
    a.flush_barrier().await.expect("flush a");
    b.flush_barrier().await.expect("flush b");
    let _ = a.close().await;
    let _ = b.close().await;
}
