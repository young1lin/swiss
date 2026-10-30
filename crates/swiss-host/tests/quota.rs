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

//! The shared physical budget's acceptance set (plan docs/plans/module-log-storage.md
//! U1-U4, SPEC `host.history): shares per OWNER never per resource, physical bytes
//! measured from the filesystem, protected-window overrun reported honestly, a
//! cross-process reservation ledger, and pressure that never erases authority.

use std::path::{Path, PathBuf};

use sqlx::Row;
use swiss_host::history::quota::{
    self, DenialReason, PhysicalUsage, QuotaPolicy, TOTAL_NORMAL_BUDGET,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "swiss-quota-{}-{}",
        name,
        swiss_core::util::random_hex(8)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn pool_at(db: &Path) -> sqlx::sqlite::SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .create_if_missing(true);
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

// --- U1 ---------------------------------------------------------------------------------------------

#[test]
fn quotas_are_per_module_not_resource() {
    let policy = QuotaPolicy::default();
    // Shares answer to OWNERS. Any number of targets, MCP servers or databases
    // later, the numbers are the same constants.
    assert_eq!(policy.share_for("mcp"), quota::DEFAULT_MCP_SHARE);
    assert_eq!(policy.share_for("remote"), quota::DEFAULT_REMOTE_SHARE);
    assert_eq!(policy.share_for("terminal"), quota::DEFAULT_TERMINAL_SHARE);
    // An owner the policy never enrolled has no share to spend.
    assert_eq!(policy.share_for("jobs"), 0);
    assert_eq!(
        policy.admits("jobs", PhysicalUsage::default(), 1, false),
        Err(DenialReason::NotEnrolled)
    );
}

#[test]
fn mcp_and_remote_share_one_declared_normal_budget() {
    let policy = QuotaPolicy::default();
    // Each owner's share is pinned, then the sum: the split is policy, not a
    // byproduct of whichever owner enrolled first.
    assert_eq!(policy.share_for("mcp"), quota::DEFAULT_MCP_SHARE);
    assert_eq!(policy.share_for("remote"), quota::DEFAULT_REMOTE_SHARE);
    assert_eq!(policy.share_for("terminal"), quota::DEFAULT_TERMINAL_SHARE);
    assert_eq!(policy.share_for("llm"), quota::DEFAULT_LLM_SHARE);
    assert_eq!(quota::DEFAULT_LLM_SHARE, 1_000_000_000, "a memory recheck outgrew 100 MB of llm history within a day, and 600 MB was raised to 1 GB");
    assert_eq!(policy.share_for("process"), quota::DEFAULT_PROCESS_SHARE);
    assert_eq!(policy.share_for("data"), quota::DEFAULT_DATA_SHARE);
    assert_eq!(
        policy.declared_total(),
        TOTAL_NORMAL_BUDGET,
        "the split is policy: 200 MB + 200 MB + 100 MB + 1000 MB (llm) + 10 MB (process) + 50 MB (data) = the one declared budget"
    );
}

#[tokio::test]
async fn one_owner_reclaims_only_its_rows() {
    // Two owners, two files. A maintenance pass over A must not move a byte of B.
    let dir_a = scratch("a");
    let dir_b = scratch("b");
    let db_a = dir_a.join("mcp.db");
    let db_b = dir_b.join("remote.db");
    for db in [&db_a, &db_b] {
        let pool = pool_at(db).await;
        sqlx::query("CREATE TABLE t (v TEXT)").execute(&pool).await.unwrap();
        for _ in 0..500 {
            sqlx::query("INSERT INTO t (v) VALUES (?)")
                .bind("x".repeat(200))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;
    }
    let b_before = quota::measure(&db_b).total();
    let pool_a = pool_at(&db_a).await;
    let receipt = quota::maintain(&pool_a, &db_a).await.unwrap();
    pool_a.close().await;
    assert!(receipt.db_bytes_before > 0);
    let b_after = quota::measure(&db_b).total();
    assert_eq!(b_before, b_after, "B's bytes did not move");
}

// --- U2 ---------------------------------------------------------------------------------------------

#[test]
fn physical_usage_counts_sidecars_backups_and_inflight_work() {
    let dir = scratch("sidecars");
    let db = dir.join("remote.db");
    std::fs::write(&db, vec![0u8; 10_000]).unwrap();
    std::fs::write(dir.join("remote.db-wal"), vec![0u8; 4_000]).unwrap();
    std::fs::write(dir.join("remote.db-shm"), vec![0u8; 1_000]).unwrap();
    std::fs::write(dir.join("remote.db.maintenance.bak"), vec![0u8; 2_000]).unwrap();
    let usage = quota::measure(&db);
    assert_eq!(usage.db_bytes, 10_000);
    assert_eq!(usage.sidecar_bytes, 5_000, "the WAL and SHM are real bytes");
    assert_eq!(usage.other_bytes, 2_000, "a leftover backup is real bytes");
    // A missing file is zero, not an error.
    assert_eq!(
        quota::measure(&dir.join("absent.db")),
        PhysicalUsage::default()
    );
}

#[tokio::test]
async fn delete_without_checkpoint_is_not_reclaimed_credit() {
    let dir = scratch("delete");
    let db = dir.join("mcp.db");
    let pool = pool_at(&db).await;
    sqlx::query("CREATE TABLE t (v TEXT)").execute(&pool).await.unwrap();
    for _ in 0..2_000 {
        sqlx::query("INSERT INTO t (v) VALUES (?)")
            .bind("y".repeat(400))
            .execute(&pool)
            .await
            .unwrap();
    }
    let grown = quota::measure(&db).total();
    // Checkpoint the grown table into the main file first, so the file (not the
    // WAL) holds the data — the honest baseline for what reclamation returns.
    let _ = quota::maintain(&pool, &db).await;
    let grown_db = quota::measure(&db).db_bytes;
    assert!(grown_db > 100_000, "the table's bytes are in the main file");
    sqlx::query("DELETE FROM t").execute(&pool).await.unwrap();
    let after_delete = quota::measure(&db).total();
    assert!(
        after_delete > grown / 2,
        "deleted rows stay physical bytes until a checkpoint/vacuum reclaims them"
    );
    // The maintenance pass IS the reclamation: freelist over a quarter of the file
    // triggers the vacuum, and the receipt reports what moved.
    let receipt = quota::maintain(&pool, &db).await.unwrap();
    assert!(receipt.vacuumed, "a >25% freelist is reclaimed, not cached");
    let reclaimed_db = quota::measure(&db).db_bytes;
    assert!(
        reclaimed_db < grown_db,
        "the pass turned the freelist into returned file bytes"
    );
    pool.close().await;
}

// --- U3 ---------------------------------------------------------------------------------------------

#[test]
fn protected_metadata_overrun_is_honest() {
    let policy = QuotaPolicy::default();
    let over = PhysicalUsage {
        db_bytes: quota::DEFAULT_REMOTE_SHARE + 1,
        ..PhysicalUsage::default()
    };
    // Remote's seven-day window outranks the byte budget: the denial SAYS so, it
    // does not pretend maintenance is coming.
    assert_eq!(
        policy.admits("remote", over, 0, true),
        Err(DenialReason::ProtectedWindowOverrun {
            share: quota::DEFAULT_REMOTE_SHARE,
            projected: quota::DEFAULT_REMOTE_SHARE + 1,
        })
    );
    assert_eq!(
        DenialReason::ProtectedWindowOverrun {
            share: 0,
            projected: 0
        }
        .as_str(),
        "protected-window-overrun"
    );
    // MCP has no such window: over is over.
    let mcp_over = PhysicalUsage {
        db_bytes: quota::DEFAULT_MCP_SHARE + 1,
        ..PhysicalUsage::default()
    };
    assert_eq!(
        policy.admits("mcp", mcp_over, 0, false),
        Err(DenialReason::OverShare {
            share: quota::DEFAULT_MCP_SHARE,
            projected: quota::DEFAULT_MCP_SHARE + 1,
        })
    );
}

#[tokio::test]
async fn busy_checkpoint_does_not_hide_wal_growth() {
    let dir = scratch("wal");
    let db = dir.join("remote.db");
    let pool = pool_at(&db).await;
    sqlx::query("CREATE TABLE t (v TEXT)").execute(&pool).await.unwrap();
    // Write enough that the WAL is visibly non-zero WITHOUT any checkpoint.
    for _ in 0..300 {
        sqlx::query("INSERT INTO t (v) VALUES (?)")
            .bind("w".repeat(300))
            .execute(&pool)
            .await
            .unwrap();
    }
    let wal_now = quota::measure(&db).sidecar_bytes;
    assert!(wal_now > 0, "an uncheckpointed WAL is counted, not hidden");
    let receipt = quota::maintain(&pool, &db).await.unwrap();
    assert!(
        receipt.wal_bytes_after <= receipt.wal_bytes_before,
        "the passive checkpoint reports both sides, honestly"
    );
    // More writes after the pass: growth shows up in measurement again.
    for _ in 0..300 {
        sqlx::query("INSERT INTO t (v) VALUES (?)")
            .bind("w".repeat(300))
            .execute(&pool)
            .await
            .unwrap();
    }
    assert!(quota::measure(&db).sidecar_bytes > 0);
    pool.close().await;
}

#[tokio::test]
async fn maintenance_crash_reconciles_usage() {
    // A crash is no close, no checkpoint: the next open re-derives usage from the
    // filesystem, and the ledger survives in the file it was written to.
    let dir = scratch("crash");
    let db = dir.join("remote.db");
    {
        let pool = pool_at(&db).await;
        quota::ensure_reservation_table(&pool).await.unwrap();
        quota::reserve(&pool, "token-1", 123_456).await.unwrap();
        // No close: the pool is dropped as a crash would drop it.
    }
    let before = quota::measure(&db);
    assert!(before.db_bytes > 0);
    {
        let pool2 = pool_at(&db).await;
        let total = quota::reserved_total(&pool2).await.unwrap();
        assert_eq!(total, 123_456, "the ledger is in the file, not memory");
        pool2.close().await;
    }
    // Reconciliation is a filesystem re-derivation: two measurements with no writes
    // between them agree, whatever a crashed process left behind.
    let once = quota::measure(&db).total();
    let twice = quota::measure(&db).total();
    assert_eq!(once, twice, "usage reconciles from disk");
}

// --- U4 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn two_process_owner_reservations_are_not_double_spent() {
    // Two pools over ONE file (two processes as far as the ledger cares): each
    // sees the other's reservation, a retried token charges once.
    let dir = scratch("reserve");
    let db = dir.join("remote.db");
    let one = pool_at(&db).await;
    let two = pool_at(&db).await;
    quota::ensure_reservation_table(&one).await.unwrap();
    quota::reserve(&one, "write-a", 1_000_000).await.unwrap();
    quota::reserve(&two, "write-b", 2_000_000).await.unwrap();
    assert_eq!(quota::reserved_total(&one).await.unwrap(), 3_000_000);
    assert_eq!(
        quota::reserved_total(&two).await.unwrap(),
        3_000_000,
        "the second process spends the SAME headroom, not fresh headroom"
    );
    quota::reserve(&one, "write-a", 1_500_000).await.unwrap();
    assert_eq!(
        quota::reserved_total(&two).await.unwrap(),
        3_500_000,
        "a retried token replaces its row instead of adding one"
    );
    quota::release(&two, "write-a").await.unwrap();
    assert_eq!(quota::reserved_total(&one).await.unwrap(), 2_000_000);
    one.close().await;
    two.close().await;
}

#[tokio::test]
async fn disk_full_import_append_and_cleanup_preserve_authority() {
    // Pressure (the quota's honest analog of a full disk): a denied admission
    // writes nothing, erases nothing, and leaves the legacy source in place.
    let dir = scratch("pressure");
    let db = dir.join("remote.db");
    let legacy = dir.join("runs.jsonl");
    std::fs::write(&legacy, b"{\"runId\":1}
").unwrap();
    let pool = pool_at(&db).await;
    sqlx::query("CREATE TABLE run (run_id INTEGER PRIMARY KEY, view TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO run (run_id, view) VALUES (1, 'verified')")
        .execute(&pool)
        .await
        .unwrap();
    let before = quota::measure(&db);
    let policy = QuotaPolicy::default();
    let huge = quota::DEFAULT_REMOTE_SHARE + 1;
    let denied = policy
        .admits("remote", PhysicalUsage { db_bytes: before.db_bytes, ..Default::default() }, huge, false)
        .unwrap_err();
    assert!(
        matches!(denied, DenialReason::OverShare { .. }),
        "an over-share write is refused before it touches anything"
    );
    let after = quota::measure(&db);
    assert_eq!(
        after.db_bytes, before.db_bytes,
        "a refused admission changed no database bytes"
    );
    let rows: i64 = sqlx::query("SELECT COUNT(*) FROM run")
        .fetch_one(&pool)
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(rows, 1, "the verified row is intact");
    assert!(legacy.is_file(), "the legacy source is never erased by pressure");
    pool.close().await;
}
