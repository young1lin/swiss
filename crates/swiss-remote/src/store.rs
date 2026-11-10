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

//! The Remote module's own history database (SPEC `remote.history): one
//! `logs/remote.db` for every target's runs — targets add rows, never files or pools
//! (SPEC `host.history). The wire shapes the runs page reads never change: the record
//! view exactly as `/api/runs` showed it, the byte-cursored output reads, the sealed
//! write body.
//!
//! While attached, the legacy jsonl tree stays a WRITE-THROUGH spill: the tee file and
//! the index line land exactly as before (the tree remains a complete fallback and the
//! recovery source for a crash between spill and commit — the importer folds it back
//! idempotently), and this database is the query authority. The importer never deletes
//! anything: the legacy files stay on disk until the operator retires them explicitly
//! (SPEC `formats.logs).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

use swiss_core::secure::envelope::{self, Sealed};
use swiss_core::secure::key;
use swiss_host::history::body::{self, BodyCodec};
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{Durability, ModuleHistoryStore, OwnerPolicy};

use crate::history::{OutputChunk, PageQuery, StoredContent, MAX_READ_BYTES};

/// The Remote owner's schema, version 2 (the host's version discipline: DDL and stamp
/// commit in one transaction; an unknown newer version refuses the open untouched).
static REMOTE_SCHEMA: &[SchemaStep] = &[
    SchemaStep::new(
        1,
        &[
            // One row per finished run: run_id is the coordinator's wire id (UNIQUE — the
            // importer's natural key), ended_ms/target/actor are the page's indexed
            // predicates, view is the exact wire record the panel has always read.
            "CREATE TABLE run (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL UNIQUE,
                ended_ms INTEGER NOT NULL,
                target TEXT,
                actor TEXT,
                view TEXT NOT NULL
            )",
            "CREATE INDEX run_page ON run (run_id DESC)",
            // The output stream, chunked and codec-tagged with its byte offset — a byte
            // cursor read touches only the chunks it needs (SPEC `remote.history).
            "CREATE TABLE run_output (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL,
                part INTEGER NOT NULL,
                ofs INTEGER NOT NULL,
                codec INTEGER NOT NULL,
                plain INTEGER NOT NULL,
                block BLOB NOT NULL,
                UNIQUE (run_id, part)
            )",
            "CREATE INDEX run_output_span ON run_output (run_id, ofs)",
            // A remote.write's body, as the SEALED envelope text verbatim — the database
            // never holds a new plaintext copy of a sensitive file (SPEC `host.refs).
            "CREATE TABLE run_content (
                run_id INTEGER PRIMARY KEY,
                envelope TEXT NOT NULL
            )",
            // The meta table is the host store's own (schema versioning, import markers):
            // shared infrastructure, never redefined per module.
        ],
    ),
    // v2: a run's trail (SPEC §remote.history "Provenance"), keyed by the wire run id
    // and filed apart from the run row - it usually lands while the run is still going.
    SchemaStep::new(
        2,
        &["CREATE TABLE run_trail (
            run_id INTEGER PRIMARY KEY,
            at_ms INTEGER NOT NULL,
            trail TEXT NOT NULL
        )"],
    ),
];

/// The version of [RemoteHistory::mask_recorded_literals] a database last ran - bumped when
/// `swiss_core::secure::literal` learns a slot the rows filed before it may hold.
const LITERAL_MASK_PASS: &str = "1";

pub struct RemoteHistory {
    store: Arc<ModuleHistoryStore>,
    /// The home's logs root — the legacy tree lives at `<logs>/remote`.
    logs_dir: PathBuf,
    /// Meta keys this process committed (import state, torn tails) — readers that must
    /// not cross the writer queue read the cached copy.
    meta_cache: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// The SCAN SNAPSHOT epoch, distinct from the store's worker-identity generation:
    /// a clear or sweep bumps it, so the scan cursors those deletes invalidate are
    /// refused by name instead of silently serving rows that no longer exist (Q4).
    snapshot: std::sync::atomic::AtomicU64,
    /// The chunk sealing keys (SPEC host.history): run_output parts seal at write and
    /// open at read; an empty candidate list is the degraded plain-block mode.
    keys: swiss_host::history::seal::ChunkKeys,
    /// The REMOTE.HISTORY notice (SPEC §remote.history), forwarded by the spill this
    /// database attaches to: the hourly sweep removes rows on its own, outside any
    /// route or run, and it owes the notice itself. Publishes and reads nothing else.
    notice: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The open import moved rows before any notice could exist (the database opens
    /// before the spill attaches it, SPEC §remote.history); the first notice
    /// publishes for them.
    rewrote_at_open: std::sync::atomic::AtomicBool,
}

/// The output stream's parts as the run writer lays them down: the run id is the event
/// and the row stores its offset. run_content is a sealed envelope kept as TEXT on
/// purpose (a sensitive file's body): it is not listed, so it is never decoded.
static REMOTE_BODY_TABLES: &[swiss_host::history::cell::BodyPartTable] = &[swiss_host::history::cell::BodyPartTable {
    table: "run_output",
    record_col: "run_id",
    record: "run",
    binding: swiss_host::history::cell::RecordBinding::Event,
    has_ofs: true,
    key: &["id"],
}];

impl RemoteHistory {
    /// The REMOTE.HISTORY notice, forwarded by the owning spill at attach.
    pub fn set_notice(&self, notice: Arc<dyn Fn() + Send + Sync>) {
        *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice);
        if self
            .rewrote_at_open
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.changed();
        }
    }

    fn changed(&self) {
        let notice = self
            .notice
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(notice) = notice {
            notice();
        }
    }

    /// The Data page's decoder for this file's sealed output parts (SPEC §data.sqlite
    /// "Decoded cells").
    pub fn cell_decoder(&self) -> Arc<dyn swiss_host::services::module_db::ModuleCellDecoder> {
        Arc::new(swiss_host::history::cell::BodyPartDecoder::new(
            "remote",
            self.store.clone(),
            self.keys.clone(),
            REMOTE_BODY_TABLES,
        ))
    }

    /// Open (and if needed import into) `<logs>/remote.db`. The caller is the plugin's
    /// start — never a constructor — so a disabled Remote module opens nothing at all.
    /// FULL durability (SPEC `host.history): this database is the audit surface.
    pub async fn open(logs_dir: &Path) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(logs_dir).map_err(|e| format!("creating the logs directory: {e}"))?;
        let policy = OwnerPolicy {
            db_file: logs_dir.join("remote.db"),
            queue_byte_bound: 16 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Full,
            schema: REMOTE_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("remote", policy);
        store
            .open()
            .await
            .map_err(|e| format!("opening the remote history database: {e}"))?;
        let history = Arc::new(RemoteHistory {
            store,
            logs_dir: logs_dir.to_path_buf(),
            meta_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            snapshot: std::sync::atomic::AtomicU64::new(1),
            keys: swiss_host::history::seal::ChunkKeys::resolve(),
            notice: std::sync::Mutex::new(None),
            rewrote_at_open: std::sync::atomic::AtomicBool::new(false),
        });
        // Enroll in the physical quota (SPEC host.history): the declared default
        // shares with the protected window flag - an over-share denial still denies,
        // but the wire carries the honest protected-window reason.
        history.store.set_quota(swiss_host::history::quota::QuotaPolicy::default(), true);
        if let Err(e) = history.import_legacy(&history.logs_dir.join("remote")).await {
            // The store already opened (worker spawned, pool built, handles taken): a
            // bare `?` here would leak all of it for the process lifetime. Withdraw
            // through the bounded close, THEN report the failure — a later open on the
            // same directory must start from a released store.
            let _ = history.close().await;
            return Err(format!("importing the legacy run log: {e}"));
        }
        // Rows the first writer filed carry no target column (their line keeps it
        // under meta/input only): fill each from its own view, once. A row whose
        // line names no target stays NULL - the filter reads that as "not this
        // target", exactly as it did before.
        if let Err(e) = history.backfill_target().await {
            swiss_core::log::warn(&format!("backfilling run targets: {e}"), None);
        }
        // The ongoing crash recovery - distinct from the one-time migration above: the
        // spill-before-commit window exists for every run, so every open folds any
        // spill-only records back in (best effort; retention fences what may return).
        history.recover_spill(&history.logs_dir.join("remote")).await;
        // After the recovery, so a spill-only row it just filed is masked with the rest.
        if let Err(e) = history.mask_recorded_literals().await {
            swiss_core::log::warn(&format!("masking credential literals in run records: {e}"), None);
        }
        Ok(history)
    }

    /// [Self::open]'s pass over rows filed before records masked a credential typed into a
    /// command line (SPEC §remote.security): each row whose view holds one is rewritten the way
    /// [crate::history::mask_record] masks a record today - the spill's lines get the same pass
    /// in [crate::history::RunHistory::open]. Once per database (meta `literal_mask`; a new
    /// [LITERAL_MASK_PASS] runs it again when the detector learns a slot), in pages, with
    /// secure_delete on so the replaced text is overwritten rather than left in free pages.
    async fn mask_recorded_literals(&self) -> Result<(), String> {
        if self.meta("literal_mask").await.as_deref() == Some(LITERAL_MASK_PASS) {
            return Ok(());
        }
        self.store
            .query(|pool| {
                Box::pin(async move {
                    let mut conn = pool.acquire().await?;
                    let secure: i64 = sqlx::query_scalar("PRAGMA secure_delete")
                        .fetch_one(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA secure_delete = ON").execute(&mut *conn).await?;
                    let mut after = i64::MIN;
                    loop {
                        let rows = sqlx::query(
                            "SELECT run_id, view FROM run WHERE run_id > ?1 ORDER BY run_id LIMIT 256",
                        )
                        .bind(after)
                        .fetch_all(&mut *conn)
                        .await?;
                        let Some(last) = rows.last() else { break };
                        after = last.try_get(0)?;
                        for row in rows {
                            let run_id: i64 = row.try_get(0)?;
                            let view: String = row.try_get(1)?;
                            let Ok(Value::Object(mut v)) = serde_json::from_str::<Value>(&view) else {
                                continue;
                            };
                            if crate::history::mask_record(&mut v) {
                                sqlx::query("UPDATE run SET view = ?1 WHERE run_id = ?2")
                                    .bind(Value::Object(v).to_string())
                                    .bind(run_id)
                                    .execute(&mut *conn)
                                    .await?;
                            }
                        }
                    }
                    let restore = match secure {
                        0 => "PRAGMA secure_delete = OFF",
                        2 => "PRAGMA secure_delete = FAST",
                        _ => "PRAGMA secure_delete = ON",
                    };
                    sqlx::query(restore).execute(&mut *conn).await?;
                    Ok::<(), sqlx::Error>(())
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        self.set_meta("literal_mask", LITERAL_MASK_PASS).await;
        Ok(())
    }

    /// [Self::open]'s one-time repair of rows whose target column never got filled
    /// ([record_run]'s first writer read a top-level field the coordinator never
    /// writes). Reruns are no-ops: the WHERE NULL guard skips every repaired row.
    async fn backfill_target(&self) -> Result<(), String> {
        self.store
            .query(|pool| {
                Box::pin(async move {
                    let rows = sqlx::query("SELECT run_id, view FROM run WHERE target IS NULL")
                        .fetch_all(&pool)
                        .await?;
                    for row in rows {
                        let run_id: i64 = row.try_get(0)?;
                        let view: String = row.try_get(1)?;
                        let Ok(v) = serde_json::from_str::<Value>(&view) else { continue };
                        let target = v
                            .get("meta")
                            .and_then(|m| m.get("target"))
                            .or_else(|| v.get("target"))
                            .or_else(|| v.get("input").and_then(|i| i.get("target")))
                            .and_then(Value::as_str);
                        if let Some(target) = target {
                            sqlx::query("UPDATE run SET target = ?1 WHERE run_id = ?2")
                                .bind(target)
                                .bind(run_id)
                                .execute(&pool)
                                .await?;
                        }
                    }
                    Ok::<(), sqlx::Error>(())
                })
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub fn state_label(&self) -> &'static str {
        self.store.state_label()
    }

    /// Re-enroll under a different share map: the store's own seam, exposed so a
    /// test can drive its share to exhaustion within a test's lifetime (the
    /// declared shares are hundreds of megabytes). Production only ever passes
    /// [swiss_host::history::quota::QuotaPolicy::default].
    pub fn set_quota(&self, policy: swiss_host::history::quota::QuotaPolicy) {
        self.store.set_quota(policy, false);
    }

    /// Wait until every admitted write has completed — the read visibility contract.
    /// The barrier's verdict is the CALLER's to see: a writer that failed must surface,
    /// never be flattened into success by an unconditional discard here.
    pub async fn flush(&self) -> Result<(), String> {
        self.store.flush_barrier().await.map_err(|e| e.to_string())
    }

    /// The bounded close (withdraw, drain, release) — the plugin's stop.
    pub async fn close(&self) -> Result<(), String> {
        self.store.close().await.map_err(|e| e.to_string())
    }

    /// (runs, database bytes on disk, legacy import state). Diagnostics; capacity is
    /// the retention phase's physical accounting, never this count.
    pub async fn usage(&self) -> (u64, u64, String) {
        let runs = self.scalar("SELECT count(*) FROM run").await.unwrap_or(0);
        let bytes = std::fs::metadata(self.store.policy().db_file.clone())
            .map(|m| m.len())
            .unwrap_or(0);
        let state = self
            .meta_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("legacy_import")
            .cloned()
            .unwrap_or_else(|| "pending".into());
        (runs, bytes, state)
    }

    async fn scalar(&self, sql: &'static str) -> Result<u64, String> {
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query(sql).fetch_one(&pool).await?;
                    Ok::<u64, sqlx::Error>(row.try_get::<i64, _>(0)? as u64)
                })
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// The highest wire run id held — the coordinator's numbering floor, so a restart
    /// never files two runs under one number.
    pub async fn last_run_id(&self) -> Option<u64> {
        self.scalar("SELECT COALESCE(MAX(run_id), 0) FROM run")
            .await
            .ok()
            .filter(|v| *v > 0)
    }

    /// The internal row ids, ascending — how tests observe the identity floor.
    pub async fn internal_run_ids(&self) -> Vec<i64> {
        self.store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query("SELECT id FROM run ORDER BY id")
                        .fetch_all(&pool)
                        .await?
                        .into_iter()
                        .map(|r| r.try_get::<i64, _>(0))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .await
            .unwrap_or_default()
    }

    // --- recording ------------------------------------------------------------------------------

    /// A line's end time: endedAtMs first, an ISO endedAt second, 0 when the line
    /// carries neither.
    fn ended_ms_of(line: &Value) -> u64 {
        line.get("endedAtMs")
            .and_then(Value::as_u64)
            .or_else(|| {
                line.get("endedAt")
                    .and_then(|v| v.as_str())
                    .and_then(swiss_core::util::parse_iso_ms)
                    .and_then(|ms| u64::try_from(ms).ok())
            })
            .unwrap_or(0)
    }

    /// File one finished run: the wire line (already appended to the legacy spill by
    /// [crate::history::RunHistory]), its output file and its sealed content file. One
    /// queued transaction, idempotent on (run_id, part) — a crash between spill and
    /// commit re-imports exactly once on the next open.
    pub async fn record_run(
        &self,
        line: &Value,
        out_path: Option<&Path>,
        content_path: Option<&Path>,
    ) -> Result<(), String> {
        let run_id = line
            .get("runId")
            .and_then(Value::as_u64)
            .ok_or("the run record has no runId")?;
        let ended_ms = Self::ended_ms_of(line);
        // The target a line names: the resolved meta first, the top-level field the
        // fixtures and legacy writers carried second, the input (what was asked) last.
        // The first writer read only the top level - and the coordinator's own lines
        // never carry it there, so every recorded row filed target NULL and the
        // target filter matched nothing while the unfiltered page showed every run.
        let target = line
            .get("meta")
            .and_then(|m| m.get("target"))
            .or_else(|| line.get("target"))
            .or_else(|| line.get("input").and_then(|i| i.get("target")))
            .and_then(Value::as_str)
            .map(str::to_string);
        let actor = line.get("actor").and_then(Value::as_str).map(str::to_string);
        let view = line.to_string();
        let out_path = out_path.map(Path::to_path_buf);
        let content_path = content_path.map(Path::to_path_buf);
        let mut estimate = view.len() + 256;
        if let Some(p) = &out_path {
            estimate += std::fs::metadata(p).map(|m| m.len() as usize).unwrap_or(0);
        }
        if let Some(p) = &content_path {
            estimate += std::fs::metadata(p).map(|m| m.len() as usize).unwrap_or(0);
        }
        let keys = self.keys.clone();
        self.store
            .submit(estimate, move |pool| {
                Box::pin(async move {
                    insert_run(
                        &pool,
                        &keys,
                        run_id,
                        ended_ms,
                        (target.as_deref(), actor.as_deref()),
                        &view,
                        out_path.as_deref(),
                        content_path.as_deref(),
                    )
                    .await
                    .map_err(|e| e.to_string())
                })
            })
            .map_err(|e| e.to_string())?;
        // The acknowledged completion contract: Recorded claims durable visibility,
        // so the admission above is not enough — wait for the commit, and a failed
        // barrier is a FAILED record, never a false Recorded.
        self.flush().await?;
        Ok(())
    }

    // --- reading --------------------------------------------------------------------------------

    /// A page of records, newest first — every predicate in one statement, the cursor
    /// is the next older run id (the wire contract the pane pages with).
    pub async fn page(&self, q: &PageQuery<'_>) -> (Vec<Value>, Option<u64>) {
        let before = q.before;
        let since = q.since;
        let until = q.until;
        let limit = q.limit.clamp(1, 100) as i64 + 1;
        let target = q.target.map(str::to_string);
        let actor = q.actor.map(str::to_string);
        let rows = self
            .store
            .query::<Vec<String>, _>(move |pool| {
                Box::pin(async move {
                    sqlx::query(
                        "SELECT view FROM run \
                         WHERE (?1 IS NULL OR run_id < ?1) \
                           AND (?2 IS NULL OR ended_ms >= ?2) \
                           AND (?3 IS NULL OR ended_ms < ?3) \
                           AND (?4 IS NULL OR target = ?4) \
                           AND (?5 IS NULL OR actor = ?5) \
                         ORDER BY run_id DESC LIMIT ?6",
                    )
                    .bind(before.map(|v| v as i64))
                    .bind(since.map(|v| v as i64))
                    .bind(until.map(|v| v as i64))
                    .bind(target.as_deref())
                    .bind(actor.as_deref())
                    .bind(limit)
                    .fetch_all(&pool)
                    .await
                    .and_then(|rows| {
                        rows.into_iter()
                            .map(|r| r.try_get::<String, _>(0))
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
            })
            .await
            .unwrap_or_default();
        // The probe row that proves more exists is NEVER the cursor itself: deriving
        // `next` from the probe swallows it — every page boundary would silently drop
        // exactly one run (100 runs at page size 20 showed 96). The cursor is the LAST
        // ROW SERVED, so the next page starts strictly after it and the probe row comes
        // back as the next page's first record.
        let has_more = rows.len() as i64 >= limit;
        let mut records: Vec<Value> = rows
            .iter()
            .filter_map(|t| serde_json::from_str(t).ok())
            .collect();
        records.truncate(limit as usize - 1);
        let next = if has_more {
            records
                .last()
                .and_then(|v| v.get("runId"))
                .and_then(Value::as_u64)
        } else {
            None
        };
        (records, next)
    }

    /// One record by wire run id.
    pub async fn get(&self, run_id: u64) -> Option<Value> {
        self.store
            .query::<Option<String>, _>(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT view FROM run WHERE run_id = ?")
                        .bind(run_id as i64)
                        .fetch_optional(&pool)
                        .await?;
                    row.map(|r| r.try_get::<String, _>(0)).transpose()
                })
            })
            .await
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str(&t).ok())
    }

    /// The recorded output from byte `after`, at most `max` (clamped as the legacy
    /// route did). Chunked storage: only the chunks the window touches are read.
    /// UTF-8 boundaries keep the follower's contract (SPEC `remote.utf8).
    pub async fn output(&self, run_id: u64, after: u64, max: usize) -> Option<OutputChunk> {
        let max = max.clamp(1, MAX_READ_BYTES);
        let run_id_i = run_id as i64;
        let upto = after.saturating_add(max as u64);
        let chunks = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT ofs, part, codec, plain, block FROM run_output \
                         WHERE run_id = ? AND ofs < ? ORDER BY ofs",
                    )
                    .bind(run_id_i)
                    .bind(upto as i64)
                    .fetch_all(&pool)
                    .await?;
                    let mut out: Vec<(u64, i64, i64, usize, Vec<u8>)> = Vec::new();
                    for r in rows {
                        let ofs: i64 = r.try_get(0)?;
                        let part: i64 = r.try_get(1)?;
                        let codec: i64 = r.try_get(2)?;
                        let plain: i64 = r.try_get(3)?;
                        let block: Vec<u8> = r.try_get(4)?;
                        out.push((ofs as u64, part, codec, plain as usize, block));
                    }
                    Ok::<Vec<(u64, i64, i64, usize, Vec<u8>)>, sqlx::Error>(out)
                })
            })
            .await
            .ok()?;
        if chunks.is_empty() {
            // No stored stream: unknown run vs ran-silently is the caller's call via get.
            return None;
        }
        // The stream's true length, not the fetched window's: the follower loops on
        // this number, and a gap must leave it visibly larger than what a read returns.
        let total: u64 = self
            .store
            .query::<i64, _>(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query(
                        "SELECT COALESCE(SUM(plain), 0) FROM run_output WHERE run_id = ?",
                    )
                    .bind(run_id as i64)
                    .fetch_one(&pool)
                    .await?;
                    row.try_get::<i64, _>(0)
                })
            })
            .await
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);
        let mut buf: Vec<u8> = Vec::with_capacity(max);
        let mut joined_from = after;
        for (ofs, part, codec, plain, block) in chunks {
            let span_end = ofs + plain as u64;
            if span_end <= after {
                continue;
            }
            if ofs > joined_from {
                // A gap (a missing middle chunk): the read stops at the hole — head
                // and tail never join as if they were one stream.
                break;
            }
            // Unseal first (a plain block is a legacy or degraded row), decode second:
            // the binding pins the run, part and byte range this chunk must answer for.
            let binding = swiss_host::history::seal::ChunkBinding::row(
                "remote",
                "",
                run_id_i,
                part,
                ofs,
                plain,
                codec as u8,
            );
            let coded = match self.keys.open_block(&block, &binding) {
                swiss_host::history::seal::OpenBlock::Sealed(bytes) => bytes,
                swiss_host::history::seal::OpenBlock::Plain => block,
                swiss_host::history::seal::OpenBlock::Refused => {
                    joined_from = span_end;
                    continue;
                }
            };
            let decoded: Option<Vec<u8>> = BodyCodec::from_u8(codec as u8)
                .and_then(|c| body::decode_chunk(c, &coded, plain).ok());
            let decoded = decoded.unwrap_or_default();
            let skip = (joined_from - ofs) as usize;
            if skip < decoded.len() {
                buf.extend_from_slice(&decoded[skip..]);
                joined_from = ofs + decoded.len() as u64;
            }
            if buf.len() >= max {
                break;
            }
        }
        if buf.len() > max {
            buf.truncate(max);
            joined_from = after + max as u64;
        }
        let at_end = joined_from >= total;
        // A hand-set cursor inside a multi-byte character: the leading partial
        // bytes are unservable - drain them and serve from the next boundary, the
        // same policy as the mcp body read, instead of serving a replacement
        // character and swallowing the real one lossy. The drained bytes count
        // toward the cursor: next must point past BOTH them and the served text.
        // The at_end window state is irrelevant here: a leading continuation byte
        // means the CURSOR sat inside a character, not that the writer truncated
        // one (that tail case belongs to the truncate below).
        let mut drained = 0usize;
        if !buf.is_empty() && (buf[0] & 0xC0) == 0x80 {
            let mut probe = 0usize;
            while probe < buf.len() && (buf[probe] & 0xC0) == 0x80 {
                probe += 1;
            }
            drained = probe;
            buf.drain(..probe);
        }
        let keep = swiss_core::utf8::char_boundary_end(&buf);
        if keep < buf.len() && keep > 0 && !at_end {
            buf.truncate(keep);
        }
        let next = after + drained as u64 + buf.len() as u64;
        Some(OutputChunk {
            text: String::from_utf8_lossy(&buf).into_owned(),
            cursor: after.min(total),
            next_cursor: next,
            total,
        })
    }

    /// A remote.write's sealed body. The envelope text comes out of the database and
    /// opens with this machine's key candidates — the same rule read_secure_json
    /// applies to the file, minus writing anything back.
    pub async fn content(&self, run_id: u64) -> Result<Option<StoredContent>, String> {
        let text = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT envelope FROM run_content WHERE run_id = ?")
                        .bind(run_id as i64)
                        .fetch_optional(&pool)
                        .await?;
                    row.map(|r| r.try_get::<String, _>(0)).transpose()
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some(text) = text else {
            return Ok(None);
        };
        content_from_envelope_text(&text).map(Some)
    }

    /// File a run's trail (SPEC §remote.history "Provenance"), masked and bounded by
    /// [crate::trail::filed]. Best effort: a refused write is logged, never retried - the
    /// run itself is already recorded or still running.
    pub async fn record_trail(&self, run_id: u64, trail: &Value) {
        let text = crate::trail::filed(trail).to_string();
        let at = swiss_core::util::now_ms() as i64;
        let submitted = self.store.submit(text.len() + 64, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO run_trail (run_id, at_ms, trail) VALUES (?, ?, ?)                      ON CONFLICT(run_id) DO UPDATE SET at_ms = excluded.at_ms, trail = excluded.trail",
                )
                .bind(run_id as i64)
                .bind(at)
                .bind(text)
                .execute(&pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
            })
        });
        if let Err(err) = submitted {
            swiss_core::log::warn("remote trail admission", Some(serde_json::json!({ "err": err.to_string() })));
            return;
        }
        if self.flush().await.is_ok() {
            self.changed();
        }
    }

    /// The trail filed for one run, if any.
    pub async fn trail(&self, run_id: u64) -> Option<Value> {
        self.store
            .query::<Option<String>, _>(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT trail FROM run_trail WHERE run_id = ?")
                        .bind(run_id as i64)
                        .fetch_optional(&pool)
                        .await?;
                    row.map(|r| r.try_get::<String, _>(0)).transpose()
                })
            })
            .await
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str(&t).ok())
    }

    /// Drop every record, output and content row, answering how many `run` rows went.
    /// Internal row identity survives: the next recorded run gets a fresh internal id
    /// while its WIRE id comes from the coordinator, which numbers from
    /// [Self::last_run_id] and never reuses one. The count decides the clear's notice
    /// (SPEC §remote.history): the spill ledger alone cannot - an empty spill does not
    /// mean an empty database.
    pub async fn clear(&self) -> Result<u64, String> {
        // The store's job future is pinned to Result<(), String> by the host's
        // contract, so the count travels through a cell the transaction writes only
        // after its commit - a rolled-back clear reports zero.
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counted = gone.clone();
        self.store
            .submit(128, move |pool| {
                Box::pin(async move {
                    let work = async {
                        let mut tx = pool.begin().await?;
                        sqlx::query("DELETE FROM run_output").execute(&mut *tx).await?;
                        sqlx::query("DELETE FROM run_content").execute(&mut *tx).await?;
                        sqlx::query("DELETE FROM run_trail").execute(&mut *tx).await?;
                        let removed =
                            sqlx::query("DELETE FROM run").execute(&mut *tx).await?.rows_affected();
                        // The rows are gone by hand: stamp the sweep fence so a spill
                        // line still on disk cannot bring a wiped run back.
                        sqlx::query(
                            "INSERT INTO meta (key, value) VALUES ('swept_before_ms', ?) \
                             ON CONFLICT(key) DO UPDATE SET value = \
                             MAX(CAST(meta.value AS INTEGER), CAST(excluded.value AS INTEGER))",
                        )
                        .bind(swiss_core::util::now_ms().to_string())
                        .execute(&mut *tx)
                        .await?;
                        tx.commit().await?;
                        counted.store(removed, std::sync::atomic::Ordering::Release);
                        Ok::<(), sqlx::Error>(())
                    };
                    work.await.map_err(|e| e.to_string())
                })
            })
            .map_err(|e| e.to_string())?;
        self.flush().await?;
        self.meta_cache.lock().unwrap_or_else(|e| e.into_inner()).remove("swept_before_ms");
        // The rows are gone: expire every live scan cursor over them (Q4).
        self.snapshot.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(gone.load(std::sync::atomic::Ordering::Acquire))
    }

    /// One maintenance pass (SPEC host.history): the store's bounded checkpoint /
    /// VACUUM and ledger reconciliation, then the age sweep. The hourly retention
    /// timer calls this through the maintenance seat; it never opens anything.
    pub async fn maintain(&self, age_ms: u64) {
        // A failed maintain pass would be invisible forever, and the quota usage
        // snapshot it refreshes would go stale - the store contract says so out
        // loud, so the failure is said out loud too.
        if let Err(err) = self.store.maintain().await {
            swiss_core::log::warn("remote history maintain failed", Some(serde_json::json!({ "err": err.to_string() })));
        }
        self.sweep(age_ms).await;
    }

    /// The age sweep: whole records older than the cutoff go (the audit window floor
    /// and the byte budgets are the retention phase's physical accounting).
    pub async fn sweep(&self, age_ms: u64) {
        let cutoff = swiss_core::util::now_ms().saturating_sub(age_ms) as i64;
        // How many rows this pass removes — a sweep that removed nothing publishes
        // nothing (SPEC §remote.history): the hourly timer must not wake the Runs
        // page every hour it changes no row.
        let gone_rows = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = gone_rows.clone();
        // The sweep rides submit_exempt, not submit: a quota that refuses the
        // sweep can never reclaim the bytes it is policing - the share would
        // wedge with no exit (the store contract, host history store.rs).
        let submitted = self
            .store
            .submit_exempt(64, move |pool| {
                Box::pin(async move {
                    let work = async {
                        let mut tx = pool.begin().await?;
                        let gone: Vec<i64> =
                            sqlx::query("SELECT run_id FROM run WHERE ended_ms < ?")
                                .bind(cutoff)
                                .fetch_all(&mut *tx)
                                .await?
                                .into_iter()
                                .map(|r| r.try_get::<i64, _>(0))
                                .collect::<Result<Vec<_>, _>>()?;
                        counted.store(gone.len(), std::sync::atomic::Ordering::SeqCst);
                        for id in &gone {
                            sqlx::query("DELETE FROM run_output WHERE run_id = ?")
                                .bind(id)
                                .execute(&mut *tx)
                                .await?;
                            sqlx::query("DELETE FROM run_content WHERE run_id = ?")
                                .bind(id)
                                .execute(&mut *tx)
                                .await?;
                            sqlx::query("DELETE FROM run_trail WHERE run_id = ?")
                                .bind(id)
                                .execute(&mut *tx)
                                .await?;
                        }
                        // A trail whose run never got recorded (or was cleared) ages out too.
                        sqlx::query(
                            "DELETE FROM run_trail WHERE at_ms < ? AND run_id NOT IN (SELECT run_id FROM run)",
                        )
                        .bind(cutoff)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query("DELETE FROM run WHERE ended_ms < ?")
                            .bind(cutoff)
                            .execute(&mut *tx)
                            .await?;
                        // Stamp the cutoff in the same transaction: the spill's crash
                        // recovery fences on it, so a swept run is never resurrected
                        // from its still-on-disk line (and the stamp exists exactly
                        // when the deletes do).
                        sqlx::query(
                            "INSERT INTO meta (key, value) VALUES ('swept_before_ms', ?) \
                             ON CONFLICT(key) DO UPDATE SET value = \
                             MAX(CAST(meta.value AS INTEGER), CAST(excluded.value AS INTEGER))",
                        )
                        .bind(cutoff.to_string())
                        .execute(&mut *tx)
                        .await?;
                        tx.commit().await?;
                        Ok::<(), sqlx::Error>(())
                    };
                    work.await.map_err(|e| e.to_string())
                })
            });
        if let Err(err) = submitted {
            swiss_core::log::warn("remote history sweep admission", Some(serde_json::json!({ "err": err.to_string() })));
        }
        // Swept rows are gone: flush (best effort — the sweep never claims
        // durability), then expire every live scan cursor over them (Q4).
        if self.flush().await.is_ok() {
            self.meta_cache.lock().unwrap_or_else(|e| e.into_inner()).remove("swept_before_ms");
        }
        self.snapshot.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if gone_rows.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            self.changed();
        }
    }

    // --- legacy import --------------------------------------------------------------------------

    /// Fold the legacy tree (`<logs>/remote/runs.jsonl` + `out/*`) in, idempotently:
    /// natural-key INSERT OR IGNORE, import state in meta, torn tails counted, sources
    /// never deleted. Runs once per database lifetime (meta `legacy_import=done`).
    pub async fn import_legacy(&self, remote_dir: &Path) -> Result<(), String> {
        let state = self.meta("legacy_import").await;
        if state.as_deref() == Some("done") {
            return Ok(());
        }
        self.set_meta("legacy_import", "active").await;
        let index = remote_dir.join("runs.jsonl");
        let mut torn: u64 = 0;
        let mut imported: u64 = 0;
        if let Ok(text) = std::fs::read_to_string(&index) {
            // Streaming forward, oldest first, so internal ids follow the wire order.
            for line in text.lines() {
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    torn += 1; // a kill mid-append: counted, never fatal
                    continue;
                };
                let Some(run_id) = v.get("runId").and_then(Value::as_u64) else {
                    torn += 1;
                    continue;
                };
                let out = remote_dir.join("out").join(format!("{run_id}.txt"));
                let content = remote_dir.join("out").join(format!("{run_id}.content"));
                let out_path = if out.is_file() { Some(out) } else { None };
                let content_path = if content.is_file() { Some(content) } else { None };
                if let Err(e) = self.record_run(&v, out_path.as_deref(), content_path.as_deref()).await {
                    return Err(format!("importing run {run_id}: {e}"));
                }
                imported += 1;
            }
            if !text.is_empty() && !text.ends_with('\n') {
                torn += 1;
            }
        }
        if torn > 0 {
            self.set_meta("legacy_torn_tail", &torn.to_string()).await;
        }
        self.set_meta("legacy_import", "done").await;
        // Rows the import rewrote (SPEC §remote.history: the open at start) — the
        // first notice publishes for them, once the spill attaches one.
        if imported > 0 {
            self.rewrote_at_open
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    /// The spill's crash recovery (SPEC §remote.history): a run's line and its out
    /// file are on disk before the database commit is asked for, so a crash between the
    /// two leaves a spill-only record - and that window exists for every run, not only
    /// the migration's first day ([import_legacy]'s marker covers just that one). Every
    /// open reads the index and files the runs whose rows are missing.
    ///
    /// Recovery never fights retention: a line whose run ended before the cutoff of the
    /// last completed sweep - or of a Clear, which stamps the same mark - stays gone, so
    /// expired and deliberately wiped records are not resurrected. Torn lines are
    /// skipped like the importer skips them; each filing is [record_run], idempotent on
    /// the natural key. Best effort: one bad line is logged and skipped, never fatal to
    /// the open.
    async fn recover_spill(&self, remote_dir: &Path) {
        let swept_before = self
            .meta("swept_before_ms")
            .await
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let known = self.run_ids().await;
        let Ok(text) = std::fs::read_to_string(remote_dir.join("runs.jsonl")) else {
            return;
        };
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue; // a kill mid-append: the importer's rule
            };
            let Some(run_id) = v.get("runId").and_then(Value::as_u64) else {
                continue;
            };
            if known.contains(&(run_id as i64)) {
                continue; // already committed
            }
            if swept_before > 0 && (Self::ended_ms_of(&v) as i64) < swept_before {
                continue; // swept or cleared: stays gone
            }
            let out = remote_dir.join("out").join(format!("{run_id}.txt"));
            let content = remote_dir.join("out").join(format!("{run_id}.content"));
            let out_path = if out.is_file() { Some(out) } else { None };
            let content_path = if content.is_file() { Some(content) } else { None };
            if let Err(e) = self.record_run(&v, out_path.as_deref(), content_path.as_deref()).await {
                swiss_core::log::warn(&format!("recovering spilled run {run_id}: {e}"), None);
            }
        }
        // What the recovery filed is visible the moment open returns - the store's
        // read-visibility contract, met with its own barrier (best effort: a failed
        // barrier is logged by the barrier's own surface, and the rows still land).
        let _ = self.flush().await;
    }

    /// Every run id the database holds, for the recovery's missing-row test.
    async fn run_ids(&self) -> std::collections::HashSet<i64> {
        self.store
            .query(|pool| {
                Box::pin(async move {
                    let rows = sqlx::query("SELECT run_id FROM run").fetch_all(&pool).await?;
                    rows.into_iter().map(|r| r.try_get::<i64, _>(0)).collect()
                })
            })
            .await
            .unwrap_or_default()
    }

    async fn meta(&self, key: &str) -> Option<String> {
        if let Some(v) = self
            .meta_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return Some(v.clone());
        }
        let owned = key.to_string();
        let v = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row =
                        sqlx::query("SELECT value FROM meta WHERE key = ?")
                            .bind(&owned)
                            .fetch_optional(&pool)
                            .await?;
                    row.map(|r| r.try_get::<String, _>(0)).transpose()
                })
            })
            .await
            .ok()
            .flatten();
        if let Some(v) = &v {
            self.meta_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key.to_string(), v.clone());
        }
        v
    }

    async fn set_meta(&self, key: &str, value: &str) {
        let owned_key = key.to_string();
        let owned_value = value.to_string();
        let _ = self
            .store
            .submit(64, move |pool| {
                Box::pin(async move {
                    let work = async {
                        sqlx::query(
                            "INSERT INTO meta (key, value) VALUES (?, ?) \
                             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        )
                        .bind(&owned_key)
                        .bind(&owned_value)
                        .execute(&pool)
                        .await?;
                        Ok::<(), sqlx::Error>(())
                    };
                    work.await.map_err(|e| e.to_string())
                })
            });
        self.meta_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), value.to_string());
    }
}

/// One queued transaction: the run row, its chunked output, its sealed content.
// Eight parameters is the row this one transaction writes (run, both dims, view,
// both spill files) plus the pool and the sealing keys; a parameter object would
// carry one use site's shape as a permanent type.
#[allow(clippy::too_many_arguments)]
async fn insert_run(
    pool: &SqlitePool,
    keys: &swiss_host::history::seal::ChunkKeys,
    run_id: u64,
    ended_ms: u64,
    dims: (Option<&str>, Option<&str>),
    view: &str,
    out_path: Option<&Path>,
    content_path: Option<&Path>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT OR IGNORE INTO run (run_id, ended_ms, target, actor, view) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(run_id as i64)
    .bind(ended_ms as i64)
    .bind(dims.0)
    .bind(dims.1)
    .bind(view)
    .execute(&mut *tx)
    .await?;
    if let Some(path) = out_path {
        if let Ok(bytes) = std::fs::read(path) {
            let mut ofs = 0u64;
            for (part, chunk) in body::chunk(&bytes).enumerate() {
                let plain = chunk.len();
                let (codec, coded) = body::encode_chunk(chunk);
                // Sealing is the LAST step: the coded bytes enter the envelope and the
                // row identity rides inside the authenticated plaintext, so a genuine
                // envelope moved to another run, part or byte range refuses to open.
                let binding = swiss_host::history::seal::ChunkBinding::row(
                    "remote",
                    "",
                    run_id as i64,
                    part as i64,
                    ofs,
                    plain,
                    codec.as_u8(),
                );
                let block = keys.seal_block("history:remote:run_output", &binding, &coded);
                sqlx::query(
                    "INSERT OR IGNORE INTO run_output \
                     (run_id, part, ofs, codec, plain, block) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(run_id as i64)
                .bind(part as i64)
                .bind(ofs as i64)
                .bind(codec as u8 as i64)
                .bind(plain as i64)
                .bind(&block)
                .execute(&mut *tx)
                .await?;
                ofs += plain as u64;
            }
        }
    }
    if let Some(path) = content_path {
        if let Ok(text) = std::fs::read_to_string(path) {
            sqlx::query("INSERT OR IGNORE INTO run_content (run_id, envelope) VALUES (?, ?)")
                .bind(run_id as i64)
                .bind(&text)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Open a stored envelope: sealed text unseals with this machine's key candidates (the
/// tag decides); legacy plaintext JSON reads as-is. Never writes anything back.
fn content_from_envelope_text(text: &str) -> Result<StoredContent, String> {
    let raw: Value =
        serde_json::from_str(text).map_err(|e| format!("stored content is not JSON: {e}"))?;
    let value = if envelope::is_sealed(&raw) {
        let sealed: Sealed = serde_json::from_value(raw)
            .map_err(|e| format!("malformed envelope: {e}"))?;
        let candidates = key::master_key_candidates()?;
        let mut opened = Err("no master key candidate".to_string());
        for cand in &candidates {
            match envelope::unseal(&cand.key, &sealed) {
                Ok(plaintext) => {
                    opened = serde_json::from_str(&plaintext)
                        .map_err(|e| format!("decrypted payload is not JSON: {e}"));
                    break;
                }
                Err(_) => continue,
            }
        }
        opened?
    } else {
        raw
    };
    Ok(StoredContent {
        text: value["content"].as_str().unwrap_or_default().to_string(),
        truncated: value["truncated"].as_bool().unwrap_or(false),
    })
}

/// The scan source over this database's output chunks (SPEC host.history): one
/// literal-search stream per module, ordered by (run_id, offset).
impl swiss_host::history::search::BodySource for RemoteHistory {
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<swiss_host::history::search::StoredChunk>, String>> + Send + '_>> {
        let store = self.store.clone();
        Box::pin(async move {
            store
                .query::<Vec<swiss_host::history::search::StoredChunk>, _>(move |pool| {
                    Box::pin(async move {
                        sqlx::query("SELECT run_id, part, ofs, codec, plain, block FROM run_output WHERE (run_id > ?1 OR (run_id = ?1 AND ofs >= ?2)) ORDER BY run_id, ofs LIMIT ?3")
                            .bind(from.0)
                            .bind(from.1)
                            .bind(limit as i64)
                            .fetch_all(&pool)
                            .await
                            .and_then(|rows| {
                                rows.into_iter()
                                    .map(|r| {
                                        let part = r.try_get::<i64, _>(1)?;
                                        Ok(swiss_host::history::search::StoredChunk {
                                            run_id: r.try_get::<i64, _>(0)?,
                                            part,
                                            ofs: r.try_get::<i64, _>(2)?,
                                            codec: r.try_get::<i64, _>(3)?,
                                            plain: r.try_get::<i64, _>(4)?,
                                            block: r.try_get::<Vec<u8>, _>(5)?,
                                        })
                                    })
                                    .collect::<Result<Vec<_>, sqlx::Error>>()
                            })
                    })
                })
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn generation(&self) -> u64 {
        // The scan snapshot epoch: bumped by clears and sweeps, distinct from the
        // store's worker-identity generation.
        self.snapshot.load(std::sync::atomic::Ordering::Acquire)
    }

    fn decode(&self, chunk: &swiss_host::history::search::StoredChunk) -> Vec<u8> {
        let binding = swiss_host::history::seal::ChunkBinding::row(
            "remote",
            "",
            chunk.run_id,
            chunk.part,
            chunk.ofs as u64,
            chunk.plain as usize,
            chunk.codec as u8,
        );
        let coded = match self.keys.open_block(&chunk.block, &binding) {
            swiss_host::history::seal::OpenBlock::Sealed(bytes) => bytes,
            swiss_host::history::seal::OpenBlock::Plain => chunk.block.clone(),
            // A refused envelope is GONE for the scan: the engine skips the chunk.
            swiss_host::history::seal::OpenBlock::Refused => return Vec::new(),
        };
        swiss_host::history::body::BodyCodec::from_u8(chunk.codec as u8)
            .and_then(|c| swiss_host::history::body::decode_chunk(c, &coded, chunk.plain as usize).ok())
            .unwrap_or_default()
    }
}

/// The body reader for the cross-module endpoint (SPEC §host.history): one
/// run's stored output by run id, byte-cursored and bounded exactly like the
/// legacy output route - gaps stop the read and the next frontier is honest.
pub struct RemoteBodyReader {
    history: std::sync::Arc<RemoteHistory>,
}

impl RemoteBodyReader {
    pub fn new(history: std::sync::Arc<RemoteHistory>) -> Self {
        RemoteBodyReader { history }
    }
}

#[async_trait::async_trait]
impl swiss_host::services::history_query::HistoryBodyReader for RemoteBodyReader {
    async fn read_body(
        &self,
        id: i64,
        after: u64,
        max: usize,
    ) -> Result<swiss_host::services::history_query::BodySlice, String> {
        let id = u64::try_from(id).map_err(|_| format!("run id {id} is out of range"))?;
        match self.history.output(id, after, max).await {
            Some(chunk) => {
                let next = if chunk.next_cursor < chunk.total {
                    Some(chunk.next_cursor)
                } else {
                    None
                };
                Ok(swiss_host::services::history_query::BodySlice {
                    text: chunk.text,
                    next,
                    gone: false,
                })
            }
            None => Err(format!("no stored output for run {id}")),
        }
    }
}
