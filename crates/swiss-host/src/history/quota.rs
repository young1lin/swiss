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

//! The shared physical budget across the enrolled module histories (plan
//! docs/plans/module-log-storage.md D4, SPEC `host.history). One declared normal budget
//! — 1,560,000,000 decimal bytes — split into OWNER shares (MCP 200,000,000, Remote
//! 200,000,000, Terminal 100,000,000, LLM 1,000,000,000, Process 10,000,000, Data 50,000,000
//! by default; ADR-035 raised the declared budget when recordings joined rather than silently
//! cutting the audit store's share, §llm.history carved its first seat from Remote, and its
//! growth to 600,000,000 and then 1,000,000,000 grew the budget with it, as the Data console
//! history's seat did).
//! Shares belong to owners, never to resources: adding a target or an MCP server
//! multiplies nothing. This is a SOFT budget: Remote's
//! seven-day protected window may hold a steady-state overrun past eligible
//! reclamation, and the accounting says so instead of pretending maintenance will
//! fix it.

use std::collections::BTreeMap;
use std::path::Path;

use sqlx::sqlite::SqlitePool;
use sqlx::Row;

/// The declared normal budget for the enrolled module histories, decimal bytes.
pub const TOTAL_NORMAL_BUDGET: u64 = 1_560_000_000;

/// Default owner shares. The split is policy, not arithmetic: each seat is sized to
/// what its owner records, not to an even cut.
pub const DEFAULT_MCP_SHARE: u64 = 200_000_000;
/// Remote gave 100,000,000 to the llm call and conversation store's first seat
/// (§llm.history).
pub const DEFAULT_REMOTE_SHARE: u64 = 200_000_000;
/// The terminal recordings' share (§terminal.recording, ADR-035): 100,000,000 bytes.
pub const DEFAULT_TERMINAL_SHARE: u64 = 100_000_000;
/// The llm history share (§llm.history): every provider round trip and agent
/// conversation, 1,000,000,000 bytes. 100,000,000 held under a day of a memory recheck,
/// whose prompts inline the code they ask about, so the trim kept cutting what was
/// still being read; at 600,000,000 the store already held 393 MB (2026-10-27), two
/// thirds of its seat, and the operator raised the seat rather than trim sooner.
pub const DEFAULT_LLM_SHARE: u64 = 1_000_000_000;
/// The process outcomes' share (§process.apps): 10,000,000 bytes - the row bound
/// holds the store near one MiB, so the share is ten times the steady state,
/// headroom for imports and reindexes, not a seat that can drift idle.
pub const DEFAULT_PROCESS_SHARE: u64 = 10_000_000;
/// The Data console history's share (§data.history): 50,000,000 bytes - its row bound
/// (100,000 statements) holds it at a few tens of MB, and the share is that plus headroom.
pub const DEFAULT_DATA_SHARE: u64 = 50_000_000;

/// The sidecar suffixes a live SQLite database may carry next to its file. All of
/// them are physical bytes the home pays for, so all of them are counted.
pub const SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Physical bytes for one owner's database, measured from the filesystem — never
/// from cached or logical counters. A file that does not exist is zero, not an
/// error: a not-yet-opened owner honestly costs nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalUsage {
    pub db_bytes: u64,
    pub sidecar_bytes: u64,
    /// Backup/maintenance scratch files the owner wrote next to the database.
    pub other_bytes: u64,
    /// Bytes admitted for in-flight work (the reservation ledger's total).
    pub reserved_bytes: u64,
}

impl PhysicalUsage {
    pub fn total(&self) -> u64 {
        self.db_bytes + self.sidecar_bytes + self.other_bytes + self.reserved_bytes
    }
}

/// Measure one owner's database footprint: the file, its live sidecars, and any
/// `*.bak`/`*.tmp` siblings a maintenance pass left behind.
pub fn measure(db_file: &Path) -> PhysicalUsage {
    let mut usage = PhysicalUsage {
        db_bytes: file_len(db_file),
        ..PhysicalUsage::default()
    };
    if let Some(dir) = db_file.parent() {
        let stem = db_file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for suffix in SIDECAR_SUFFIXES {
            usage.sidecar_bytes += file_len(&dir.join(format!("{stem}{suffix}")));
        }
        for extra in ["maintenance.bak", "maintenance.tmp"] {
            usage.other_bytes += file_len(&dir.join(format!("{stem}.{extra}")));
        }
    }
    usage
}

/// The enrolled owners and their shares. Constructed by the composition root so
/// the policy lives in ONE place; nothing dispatches on owner identity beyond the
/// declared map.
#[derive(Debug, Clone)]
pub struct QuotaPolicy {
    shares: BTreeMap<String, u64>,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        let mut shares = BTreeMap::new();
        shares.insert("mcp".into(), DEFAULT_MCP_SHARE);
        shares.insert("remote".into(), DEFAULT_REMOTE_SHARE);
        shares.insert("terminal".into(), DEFAULT_TERMINAL_SHARE);
        shares.insert("llm".into(), DEFAULT_LLM_SHARE);
        shares.insert("process".into(), DEFAULT_PROCESS_SHARE);
        shares.insert("data".into(), DEFAULT_DATA_SHARE);
        QuotaPolicy { shares }
    }
}

impl QuotaPolicy {
    /// A hand-declared share map (tests and future composition roots): the same
    /// structure as the default, never a second policy mechanism.
    pub fn from_shares(shares: BTreeMap<String, u64>) -> Self {
        QuotaPolicy { shares }
    }
}

impl QuotaPolicy {
    /// The owner's share. Resources (targets, MCP servers) never appear here —
    /// a share is per module, so adding either cannot multiply it.
    pub fn share_for(&self, owner: &str) -> u64 {
        self.shares.get(owner).copied().unwrap_or(0)
    }

    /// Every enrolled owner with its share, for the usage report.
    pub fn shares(&self) -> &BTreeMap<String, u64> {
        &self.shares
    }

    /// The declared total (the sum of shares; pinned to [TOTAL_NORMAL_BUDGET] by
    /// construction in [Self::default]).
    pub fn declared_total(&self) -> u64 {
        self.shares.values().sum()
    }

    /// The admission decision, pure and honest: an owner over its share is denied
    /// EXCEPT while its protected window holds the excess — Remote's audit promise
    /// outranks the budget, and the denial reason must say which one fired.
    pub fn admits(
        &self,
        owner: &str,
        usage: PhysicalUsage,
        estimated: u64,
        protected_window: bool,
    ) -> Result<(), DenialReason> {
        let share = self.share_for(owner);
        if share == 0 {
            return Err(DenialReason::NotEnrolled);
        }
        let projected = usage.total() + estimated;
        if projected <= share {
            return Ok(());
        }
        if protected_window {
            // Protected metadata outranks the byte budget (SPEC `remote.history): the
            // overrun is reported as the steady state it is, never hidden.
            Err(DenialReason::ProtectedWindowOverrun {
                share,
                projected,
            })
        } else {
            Err(DenialReason::OverShare {
                share,
                projected,
            })
        }
    }
}

/// Why an admission was denied — wire-shaped strings, never table names or SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialReason {
    NotEnrolled,
    OverShare { share: u64, projected: u64 },
    ProtectedWindowOverrun { share: u64, projected: u64 },
}

impl DenialReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DenialReason::NotEnrolled => "not-enrolled",
            DenialReason::OverShare { .. } => "over-owner-share",
            DenialReason::ProtectedWindowOverrun { .. } => "protected-window-overrun",
        }
    }
}

/// One reservation row, in the owner's own database: the cross-process ledger that
/// keeps two owners-of-the-same-file from double-spending share headroom. A token
/// is one admitted write; it is released when the write commits (or rolls back).
pub const RESERVATION_TABLE: &str = "quota_reservation";

/// Create the ledger table if this database has none yet. Idempotent.
pub async fn ensure_reservation_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS quota_reservation (token TEXT PRIMARY KEY, bytes INTEGER NOT NULL)",
    )
    .execute(pool)
    .await
    .map(|_| ())
}

/// Reserve bytes for one admitted write. `INSERT OR REPLACE`: a retried token keeps
/// one row, so a retry never double-charges.
pub async fn reserve(
    pool: &SqlitePool,
    token: &str,
    bytes: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR REPLACE INTO quota_reservation (token, bytes) VALUES (?, ?)")
        .bind(token)
    .bind(bytes as i64)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Release a token's reservation (the write committed or failed for good).
pub async fn release(pool: &SqlitePool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM quota_reservation WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await
        .map(|_| ())
}

/// The ledger's total — visible to every process with the file open, which is the
/// point: reservations are spent once, no matter how many pools see them.
pub async fn reserved_total(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let row =
        sqlx::query("SELECT COALESCE(SUM(bytes), 0) FROM quota_reservation")
            .fetch_one(pool)
            .await?;
    Ok(row.try_get::<i64, _>(0)?.max(0) as u64)
}

/// One bounded maintenance pass's receipt: what was measured before and after, and
/// what moved. Numbers are observed from the filesystem, never estimated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaintainReceipt {
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    pub vacuumed: bool,
    pub db_bytes_before: u64,
    pub db_bytes_after: u64,
}

async fn pragma_i64(pool: &SqlitePool, name: &str) -> Result<i64, sqlx::Error> {
    // A fixed set of literal statements: PRAGMA text is SQL like any other, so it
    // stays static here rather than formatted from the caller.
    let row = match name {
        "page_size" => sqlx::query("PRAGMA page_size").fetch_one(pool).await?,
        "page_count" => sqlx::query("PRAGMA page_count").fetch_one(pool).await?,
        "freelist_count" => {
            sqlx::query("PRAGMA freelist_count").fetch_one(pool).await?
        }
        _ => return Err(sqlx::Error::Configuration("unknown pragma".into())),
    };
    row.try_get::<i64, _>(0)
}

/// One bounded maintenance pass over an owner's database (SPEC `host.history):
/// a PASSIVE checkpoint (it never blocks on busy writers — a busy checkpoint must
/// not hide WAL growth, so the receipt reports both sides), then a VACUUM only when
/// the freelist holds more than a quarter of the file — deleted rows are not
/// reclaimed credit until this pass actually reclaims them.
pub async fn maintain(pool: &SqlitePool, db_file: &Path) -> Result<MaintainReceipt, sqlx::Error> {
    let mut receipt = MaintainReceipt::default();
    let wal = db_file.with_extension("db-wal");
    let wal = if wal == db_file {
        // with_extension mangles names ending in .db differently than SQLite does;
        // sidecars are strict siblings.
        let s = db_file.as_os_str().to_string_lossy().into_owned();
        Path::new(&format!("{s}-wal")).to_path_buf()
    } else {
        wal
    };
    receipt.wal_bytes_before = file_len(&wal);
    receipt.db_bytes_before = file_len(db_file);
    // Passive checkpoint: moves what it can, reports busy as a number, never waits.
    let _ = sqlx::query("PRAGMA wal_checkpoint(PASSIVE)").execute(pool).await;
    receipt.wal_bytes_after = file_len(&wal);
    // Freelist: the file's honest reclaimable space.
    let page_size = pragma_i64(pool, "page_size").await?;
    let page_count = pragma_i64(pool, "page_count").await?;
    let freelist = pragma_i64(pool, "freelist_count").await?;
    if page_size > 0 && freelist * page_size > page_count * page_size / 4 {
        // In WAL mode a peer's open read snapshot makes VACUUM return SQLITE_BUSY.
        // That is not a failure of the pass: skipping the reconcile and the usage
        // refresh for the whole hour over a contended window would trade a
        // cosmetic reclaim for real accounting. A busy VACUUM - or any VACUUM
        // failure at all - skips the reclaim and leaves receipt.vacuumed false;
        // the next pass tries again.
        if sqlx::query("VACUUM").execute(pool).await.is_ok() {
            receipt.vacuumed = true;
        }
        // In WAL mode VACUUM writes the rewritten pages to the WAL: a second
        // passive checkpoint is what lands the smaller file on disk.
        let _ = sqlx::query("PRAGMA wal_checkpoint(PASSIVE)").execute(pool).await;
        receipt.wal_bytes_after = file_len(&wal);
    }
    receipt.db_bytes_after = file_len(db_file);
    Ok(receipt)
}
