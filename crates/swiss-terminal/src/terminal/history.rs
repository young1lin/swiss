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

//! The terminal module history (SPEC §terminal.recording, ADR-035).
//!
//! One SQLite database at `<home>/logs/terminal.db` through the host's module-history
//! store: the session index plus the recording's asciicast event lines, stored VERBATIM
//! as sealed body parts. Storing the lines themselves — not a re-encoded form — is the
//! load-bearing choice: timing and text round-trip through zero second formats, so the
//! export route reconstructs a byte-faithful `.cast` and `asciinema play` keeps working
//! on what the gateway owes it (ADR-035's first bullet).
//!
//! ## The three contracts this file keeps
//!
//! **Streaming writes are parts, not PTY reads.** A session's driver buffers event lines
//! and hands the store one 64 KiB part per flush, so the bounded writer queue sees a
//! handful of admissions per busy session, never one per keystroke echo. The part's
//! plaintext offsets are the offsets users see on the body route.
//!
//! **The integer session key exists for the engine, not for people.** The deep-search
//! cursor and the generic body route speak `(i64 record id, byte offset)`; every
//! session — live and imported alike — derives its key deterministically from the
//! session id's first 15 hex characters (60 bits, see [`session_uid`]), so a re-import
//! maps onto the same rows a live session took. Collision odds are birthday-negligible
//! and the UNIQUE row catches the remainder as an honest skipped recording. The string
//! session id stays the public identity everywhere else.
//!
//! **ADR-033's import contract, verbatim.** First open folds every legacy
//! `~/.swiss/terminal/<sessionId>.cast` in idempotently — natural key the session id,
//! `INSERT OR IGNORE` everywhere, import state in meta, torn tails counted never fatal —
//! and the source files are LEFT IN PLACE: retiring them is the operator's explicit call.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlx::Row;
use swiss_host::history::body::{self, BodyCodec};
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::search::{BodySource, StoredChunk};
use swiss_host::history::seal::{ChunkBinding, ChunkKeys, OpenBlock};
use swiss_host::history::store::{Durability, ModuleHistoryStore, OwnerPolicy};

/// The sessions directory's legacy tree (the import source, never touched beyond read).
pub const SESSIONS_DIR: &str = "terminal";

/// One stored part's plaintext bound — also the recorder's flush threshold. The
/// recorder's 8 MB per-session cap lives in recording.rs, next to its enforcement
/// — one constant, one home.
pub const PART_BYTES: usize = 64 * 1024;

/// The export route's whole-recording guard. Live recordings cannot exceed the 8 MB cap,
/// but a hand-crafted legacy `.cast` could; past this the export refuses and points at
/// the body route, which serves bounded slices instead.
const EXPORT_GUARD_BYTES: u64 = 64 * 1024 * 1024;

/// meta key holding the legacy-tree import state ("active" while folding, "done" after).
const IMPORT_KEY: &str = "cast-import";
/// meta key counting torn or refused legacy files (a diagnostic, not an error):
/// a crash-torn tail, an oversized archive, an unparseable header — counted, never fatal.
const TORN_KEY: &str = "cast-torn";

static TERMINAL_SCHEMA: &[SchemaStep] = &[SchemaStep::new(
    1,
    &[
        "CREATE TABLE IF NOT EXISTS session (\
         uid INTEGER PRIMARY KEY, \
         sid TEXT NOT NULL UNIQUE, \
         started_ms INTEGER NOT NULL, \
         cols INTEGER NOT NULL, \
         rows INTEGER NOT NULL, \
         term TEXT NOT NULL, \
         ended_ms INTEGER, \
         end_reason TEXT, \
         capped INTEGER NOT NULL DEFAULT 0, \
         imported INTEGER NOT NULL DEFAULT 0)",
        "CREATE TABLE IF NOT EXISTS chunk (\
         session INTEGER NOT NULL, \
         part INTEGER NOT NULL, \
         ofs INTEGER NOT NULL, \
         plain INTEGER NOT NULL, \
         codec INTEGER NOT NULL, \
         block BLOB NOT NULL, \
         PRIMARY KEY (session, part))",
    ],
),
SchemaStep::new(
    2,
    &[
        // Who began the session: the process pid and its creation time. A row that
        // never ended names its owner, so a crash between begin and finish is
        // tellable from a recording still being written (the adoption pass,
        // adopt_orphans below).
        "ALTER TABLE session ADD COLUMN owner_pid INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE session ADD COLUMN owner_created INTEGER NOT NULL DEFAULT 0",
    ],
),
];

/// Owner identity facts may be unavailable even when the process is alive.
fn recording_owner_gone(owner_pid: i64, owner_created: i64, alive: bool, created: Option<u64>) -> bool {
    owner_pid == 0 || !alive || (owner_created > 0 && created.is_some_and(|created| created as i64 != owner_created))
}

/// One terminal history database (one per enabled plugin instance). Cheap to construct,
/// opened only inside the plugin's enabled scope (SPEC §host.history lifecycle).
pub struct TerminalHistory {
    store: Arc<ModuleHistoryStore>,
    /// The legacy sessions directory — the `.cast` tree this store imported from and
    /// leaves in place (ADR-035).
    sessions_dir: PathBuf,
    /// The SCAN SNAPSHOT epoch: a sweep bumps it so the scan cursors those deletes
    /// invalidate are refused by name (Q4's rule, the same as MCP's).
    snapshot: AtomicU64,
    /// The chunk sealing keys (SPEC §host.history); an empty candidate list is the
    /// documented degraded plain-block mode.
    keys: ChunkKeys,
}

/// One raw chunk row: (part, codec, plain_len, sealed_block).
type ChunkRow = (i64, i64, i64, Vec<u8>);

/// One session-listing row as it comes off the query, before [SessionRow] shaping.
type SessionTupleRow = (
    i64,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    i64,
    i64,
    i64,
);

impl TerminalHistory {
    /// Open (and if needed import into) `<logs>/terminal.db`. The caller is the plugin's
    /// start — never a constructor — so a disabled terminal opens nothing at all.
    pub async fn open(sessions_dir: &Path, logs_dir: &Path) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(logs_dir)
            .map_err(|e| format!("creating the logs directory: {e}"))?;
        let policy = OwnerPolicy {
            db_file: logs_dir.join("terminal.db"),
            queue_byte_bound: 8 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Normal,
            schema: TERMINAL_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("terminal", policy);
        store
            .open()
            .await
            .map_err(|e| format!("opening the terminal history database: {e}"))?;
        // Enroll in the physical quota (SPEC §host.history): the declared share table
        // including this module's 100,000,000, no protected window — a recording is
        // convenience history, and the honest over-share denial is the plain reason.
        store.set_quota(swiss_host::history::quota::QuotaPolicy::default(), false);
        let history = Arc::new(TerminalHistory {
            store,
            sessions_dir: sessions_dir.to_path_buf(),
            snapshot: AtomicU64::new(1),
            keys: ChunkKeys::resolve(),
        });
        history
            .import_cast_tree()
            .await
            .map_err(|e| format!("importing the legacy cast tree: {e}"))?;
        history.adopt_orphans().await;
        Ok(history)
    }

    /// The store's state label — the lifecycle surfaces' honest status.
    pub fn state_label(&self) -> &'static str {
        self.store.state_label()
    }

    /// Wait until every admitted write has completed: the export and body reads' visibility
    /// contract (a finished session is complete to the very next read). A barrier that
    /// errors says some admitted write FAILED to land — silent here would mean an export
    /// serving a silently short recording and a session that looks live forever, so it
    /// warns instead of swallowing.
    pub async fn flush(&self) {
        if let Err(e) = self.store.flush_barrier().await {
            self.on_write_error("terminal.db flush barrier", &e);
        }
    }

    /// Close bounded (SPEC §host.history): withdraw from the queue, drain, release. An
    /// unresolved close is the caller's warning, never a swallow.
    pub async fn close(&self) -> Result<(), swiss_host::history::store::OwnerLifecycleError> {
        self.store.close().await
    }

    // --- the write paths (all through the bounded queue; all idempotent by row key) ----

    /// Register a session row at open. `INSERT OR IGNORE`: on the (birthday-negligible)
    /// uid collision the second session's INDEX row is skipped but its chunks still
    /// land under the survivor's key — the collision is honest in the log, and the
    /// derivation (60 bits, see [session_uid]) makes it vanishingly rare.
    /// The admission result travels back: a refused begin means the recording does
    /// not exist, and the recorder must not go on writing parts against no row.
    pub fn begin(
        &self,
        uid: i64,
        sid: &str,
        cols: u16,
        rows: u16,
        term: &str,
    ) -> Result<(), swiss_host::history::store::HistoryWriteError> {
        let sid = sid.to_string();
        let term = term.to_string();
        let submitted = self.store.submit(96, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT OR IGNORE INTO session \
                     (uid, sid, started_ms, cols, rows, term, owner_pid, owner_created) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(uid)
                .bind(&sid)
                .bind(swiss_core::util::now_ms() as i64)
                .bind(cols as i64)
                .bind(rows as i64)
                .bind(&term)
                // The owner identity: a row that never ends names who began it, so
                // the adoption pass can tell a crash from a live recording.
                .bind(std::process::id() as i64)
                .bind(
                    swiss_core::platform::process_created_at(std::process::id())
                        .map(|v| v as i64)
                        .unwrap_or(0),
                )
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        match submitted {
            Err(e) => {
                self.on_write_error("terminal.db session admission", &e);
                Err(e)
            }
            Ok(()) => Ok(()),
        }
    }

    /// Store one sealed part of the recording's event lines. `ofs` is the part's
    /// plaintext start within the whole recording (the offsets the body route serves).
    /// The admission result travels back to the recorder so a refusal (over-share,
    /// queue bound) can stop the recording in-band instead of being dropped part
    /// after part with no marker.
    pub fn append_part(
        &self,
        uid: i64,
        part: i64,
        ofs: u64,
        plain: &[u8],
    ) -> Result<(), swiss_host::history::store::HistoryWriteError> {
        let plain_len = plain.len();
        let (codec, coded) = body::encode_chunk(plain);
        // Sealing is the LAST step (SPEC §host.history): the coded bytes enter the
        // envelope, the envelope enters the row, and the row identity rides inside the
        // authenticated plaintext so a moved envelope cannot serve another row. resource
        // stays "": event+part+range+codec already pin the row's identity (mcp's rule).
        let binding = ChunkBinding::row(
            "terminal",
            "",
            uid,
            part,
            ofs,
            plain_len,
            codec.as_u8(),
        );
        let block = self.keys.seal_block("history:terminal:chunk", &binding, &coded);
        let submitted = self.store.submit(plain_len + 96, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT OR IGNORE INTO chunk \
                     (session, part, ofs, plain, codec, block) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(uid)
                .bind(part)
                .bind(ofs as i64)
                .bind(plain_len as i64)
                .bind(codec as u8 as i64)
                .bind(&block)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        match submitted {
            Err(e) => {
                self.on_write_error("terminal.db part admission", &e);
                Err(e)
            }
            Ok(()) => Ok(()),
        }
    }

    /// Stamp the session row at close: when it ended, why, and whether the recorder
    /// stopped at the cap (the marker line itself is ordinary recorded output).
    /// Also returns the admission result: a refused finish leaves the row looking
    /// live, and the caller logs it loudly rather than pretending it landed.
    /// QUOTA-EXEMPT (still queue-bounded): the stamp is what puts a session on the
    /// sweep clock — a share-exhausted store that refuses it strands the row as
    /// live-forever, holding the very bytes that exhausted the share.
    pub fn finish(
        &self,
        uid: i64,
        reason: &str,
        capped: bool,
    ) -> Result<(), swiss_host::history::store::HistoryWriteError> {
        let reason = reason.to_string();
        let submitted = self.store.submit_exempt(96, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "UPDATE session SET ended_ms = ?, end_reason = ?, capped = ? WHERE uid = ?",
                )
                .bind(swiss_core::util::now_ms() as i64)
                .bind(&reason)
                .bind(capped as i64)
                .bind(uid)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        match submitted {
            Err(e) => {
                self.on_write_error("terminal.db finish admission", &e);
                Err(e)
            }
            Ok(()) => Ok(()),
        }
    }

    fn on_write_error(&self, what: &'static str, e: &swiss_host::history::store::HistoryWriteError) {
        swiss_core::log::warn(
            what,
            Some(serde_json::json!({ "error": e.to_string() })),
        );
    }

    // --- the read paths ----------------------------------------------------------------

    /// One session's stored recording decoded and joined, parts in order. Bounded by the
    /// export guard: a recording past it is a hand-crafted file, and the caller serves the
    /// body route's slices instead of pretending a 64 MB string is a response.
    async fn whole_recording(&self, uid: i64) -> Result<Option<Vec<u8>>, String> {
        // The reads' visibility contract (mcp's rule): every admitted write has
        // completed before a read looks, so a finished session is complete to the very
        // next read. The barrier's failure is the ONE signal that an admitted write
        // failed to land — swallowing it here would serve a silently short recording.
        self.flush().await;
        // The guard runs on the SUM, from the engine, BEFORE the blocks are fetched:
        // a hand-crafted over-long recording must be refused by number, not by first
        // materializing its every sealed block and then refusing.
        let total: Option<i64> = self
            .store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query_scalar::<_, i64>("SELECT SUM(plain) FROM chunk WHERE session = ?")
                        .bind(uid)
                        .fetch_optional(&pool)
                        .await
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        match total {
            None => return Ok(None),
            Some(0) => return Ok(None),
            Some(total) if total as u64 > EXPORT_GUARD_BYTES => {
                return Err("recording-too-large-for-export".into())
            }
            Some(_) => {}
        }
        let fetched: Option<Vec<ChunkRow>> = self
            .store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query_as::<_, (i64, i64, i64, Vec<u8>)>(
                        "SELECT part, codec, plain, block FROM chunk WHERE session = ? ORDER BY part",
                    )
                    .bind(uid)
                    .fetch_all(&pool)
                    .await
                    .map(Some)
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some(rows) = fetched else { return Ok(None) };
        if rows.is_empty() {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(rows.iter().map(|r| r.2).sum::<i64>().max(0) as usize);
        for (part, codec, plain, block) in rows {
            // The binding mirrors append_part exactly: the row identity the envelope
            // must repeat for the tag to verify.
            let binding = ChunkBinding::row(
                "terminal",
                "",
                uid,
                part,
                out.len() as u64,
                plain as usize,
                codec as u8,
            );
            let coded = match self.keys.open_block(&block, &binding) {
                OpenBlock::Sealed(bytes) => bytes,
                OpenBlock::Plain => block,
                // A refused envelope is a hole the reader reports honestly, never
                // text from another row.
                OpenBlock::Refused => return Err("chunk-envelope-refused".into()),
            };
            let bytes = BodyCodec::from_u8(codec as u8)
                .and_then(|c| body::decode_chunk(c, &coded, plain as usize).ok())
                .ok_or_else(|| "chunk-codec-refused".to_string())?;
            out.extend_from_slice(&bytes);
        }
        Ok(Some(out))
    }

    /// The byte-faithful `.cast` export (SPEC §terminal.recording): the header line the
    /// format wants, then the stored event lines verbatim. Timing, text and the end marker
    /// survive because the lines ARE the storage format.
    pub async fn export_cast(&self, sid: &str) -> Result<Option<String>, String> {
        let uid = self.uid_for(sid).await?;
        match uid {
            None => Ok(None),
            Some(uid) => {
                let row: Option<(i64, i64, i64, String, bool)> = self
                    .store
                    .query(|pool| {
                        Box::pin(async move {
                            sqlx::query_as::<_, (i64, i64, i64, String, bool)>(
                                "SELECT started_ms, cols, rows, term, \
                                     EXISTS(SELECT 1 FROM chunk WHERE session = ?1) \
                                     FROM session WHERE uid = ?1",
                            )
                            .bind(uid)
                            .fetch_optional(&pool)
                            .await
                        })
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                let Some((started_ms, cols, rows, term, has_chunks)) = row else {
                    return Ok(None);
                };
                if !has_chunks {
                    return Ok(None);
                }
                let bytes = self.whole_recording(uid).await?;
                let Some(bytes) = bytes else { return Ok(None) };
                let header = serde_json::json!({
                    "version": 2,
                    "width": cols,
                    "height": rows,
                    "timestamp": started_ms / 1000,
                    "env": { "TERM": term },
                });
                Ok(Some(format!("{header}\n{}", String::from_utf8_lossy(&bytes))))
            }
        }
    }

    /// Resolve the string session id to the integer row key. None = no such session.
    async fn uid_for(&self, sid: &str) -> Result<Option<i64>, String> {
        self.flush().await;
        let sid = sid.to_string();
        self.store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query_scalar::<_, i64>("SELECT uid FROM session WHERE sid = ?")
                        .bind(sid)
                        .fetch_optional(&pool)
                        .await
                })
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// The session list for the panel: newest first, with each recording's measured bytes.
    pub async fn list_sessions(&self, limit: i64) -> Result<Vec<SessionRow>, String> {
        // The read-visibility contract: flush first, so a just-finished session is
        // complete to the next listing (ended_ms/capped/bytes settled), same as
        // every other read path. flush() also warns when the barrier fails.
        self.flush().await;
        let rows: Vec<SessionTupleRow> = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query_as::<_, (i64, String, i64, i64, i64, Option<i64>, Option<String>, i64, i64, i64)>(
                        "SELECT s.uid, s.sid, s.started_ms, s.cols, s.rows, s.ended_ms, \
                         s.end_reason, s.capped, s.imported, \
                         COALESCE((SELECT SUM(plain) FROM chunk WHERE session = s.uid), 0) \
                         FROM session s ORDER BY s.started_ms DESC, s.uid DESC LIMIT ?",
                    )
                    .bind(limit)
                    .fetch_all(&pool)
                    .await
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(|(uid, sid, started_ms, cols, rows, ended_ms, end_reason, capped, imported, bytes)| SessionRow {
                uid,
                sid,
                started_ms,
                cols,
                rows,
                ended_ms,
                end_reason,
                capped: capped != 0,
                imported: imported != 0,
                bytes,
            })
            .collect())
    }

    /// One maintenance pass (SPEC §host.history): the store's bounded checkpoint / VACUUM
    /// and ledger reconciliation, then the age sweep. The hourly retention timer calls
    /// this through the maintenance seat; it never opens anything.
    pub async fn maintain(&self, age_ms: i64) {
        // A persistently failing maintain pass (locked db, full disk) would otherwise be
        // invisible forever, and the quota usage snapshot it refreshes would go stale —
        // so it warns, like every other write-side failure here.
        if let Err(e) = self.store.maintain().await {
            swiss_core::log::warn(
                "terminal.db maintain",
                Some(serde_json::json!({ "error": e.to_string() })),
            );
        }
        // The adoption precedes the sweep: a row it stamps ends on the sweep clock the
        // same pass, not the next one.
        self.adopt_orphans().await;
        self.sweep(age_ms).await;
    }

    /// Adopt the recordings a crash interrupted (SPEC §terminal.recording): a row that
    /// never ended and whose owner is gone ended the moment its owner did - stamped
    /// ended at its start with a crash note, back on the sweep clock. The owner is gone
    /// when its pid no longer exists, when the pid exists but is a DIFFERENT process
    /// (creation time moved: the pid was reused), or when the row predates the owner
    /// columns (0: the process that began it cannot be named, and after a restart none
    /// of this process's rows are that process's). A row whose owner still lives -
    /// this process, or a peer sharing the database - is genuinely still recording and
    /// stays untouched. QUOTA-EXEMPT like finish: adoption exists to put bytes BACK on
    /// the sweep clock, and a share-exhausted store refusing it wedges them forever.
    pub async fn adopt_orphans(&self) {
        let adopted = Arc::new(AtomicU64::new(0));
        let adopted_job = Arc::clone(&adopted);
        let submitted = self.store.submit_exempt(64, move |pool| {
            let adopted_job = adopted_job;
            Box::pin(async move {
                let orphans: Vec<(i64, i64, i64)> = sqlx::query_as(
                    "SELECT uid, owner_pid, owner_created FROM session WHERE ended_ms IS NULL",
                )
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                for (uid, owner_pid, owner_created) in orphans {
                    let gone = recording_owner_gone(
                        owner_pid, owner_created,
                        swiss_core::platform::pid_alive(owner_pid as u32),
                        swiss_core::platform::process_created_at(owner_pid as u32),
                    );
                    if !gone {
                        continue;
                    }
                    sqlx::query(
                        "UPDATE session SET ended_ms = started_ms, end_reason = ? \
                         WHERE uid = ? AND ended_ms IS NULL",
                    )
                    .bind("the recording process crashed before this session ended")
                    .bind(uid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                    adopted_job.fetch_add(1, Ordering::Release);
                }
                tx.commit().await.map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        if let Err(e) = submitted {
            self.on_write_error("terminal.db adoption admission", &e);
        }
        self.flush().await;
        let n = adopted.load(Ordering::Acquire);
        if n > 0 {
            swiss_core::log::warn(
                "terminal.db adopted crash-interrupted recordings",
                Some(serde_json::json!({ "adopted": n })),
            );
        }
    }

    /// Age-based retention (SPEC §terminal.recording: the 90-day sweep). Ended sessions
    /// older than the cutoff go entirely — index row and parts in one transaction; a LIVE
    /// session's row carries no ended_ms and is never a sweep candidate, because deleting
    /// the index of a recording still being written would orphan its parts.
    pub async fn sweep(&self, age_ms: i64) {
        // QUOTA-EXEMPT (still queue-bounded): the sweep is the reclamation path —
        // a share-exhausted store refusing it could never shrink again, wedging
        // the owner's retention promise with the very bytes it polices.
        let swept = Arc::new(AtomicU64::new(0));
        let swept_job = Arc::clone(&swept);
        let submitted = self.store.submit_exempt(64, move |pool| {
            let swept_job = swept_job;
            Box::pin(async move {
                let cutoff = swiss_core::util::now_ms() as i64 - age_ms;
                let expired: Vec<i64> = sqlx::query_scalar::<_, i64>(
                    "SELECT uid FROM session WHERE ended_ms IS NOT NULL AND ended_ms < ?",
                )
                .bind(cutoff)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
                // Defense-in-depth against orphaned parts: a chunk whose session row
                // never landed (a refused begin, a lost race) is invisible to every
                // reader and would hold its bytes against the share forever.
                sqlx::query("DELETE FROM chunk WHERE session NOT IN (SELECT uid FROM session)")
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                for uid in &expired {
                    sqlx::query("DELETE FROM chunk WHERE session = ?")
                        .bind(uid)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query("DELETE FROM session WHERE uid = ?")
                        .bind(uid)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                tx.commit().await.map_err(|e| e.to_string())?;
                swept_job.store(expired.len() as u64, Ordering::Release);
                Ok(())
            })
        });
        if let Err(e) = submitted {
            self.on_write_error("terminal.db sweep admission", &e);
        }
        self.flush().await;
        // Recordings vanishing is the retention working — but an operator whose
        // history disappears deserves the number, same as the import logs its files.
        let swept = swept.load(Ordering::Acquire);
        if swept > 0 {
            swiss_core::log::log(
                "info",
                "terminal history sweep deleted expired recordings",
                Some(serde_json::json!({ "sessions": swept })),
            );
        }
        // Swept rows are gone: expire every live scan cursor over them (Q4).
        self.snapshot.fetch_add(1, Ordering::Release);
    }

    /// One bounded, cursor-addressed window of a session's recording, decoded
    /// across parts on demand. The body route never materializes a whole
    /// recording (the family contract: one read costs the window, not the
    /// corpus). Page edges never split a UTF-8 character — a cursor inside a
    /// multi-byte sequence skips to its end, a window ending inside one is
    /// trimmed back so its bytes return on the next page (swiss-core utf8's
    /// rule, the same edge handling every other owner's reader carries).
    async fn read_window(
        &self,
        uid: i64,
        after: u64,
        max: usize,
    ) -> Result<Option<(String, Option<u64>)>, String> {
        self.flush().await;
        let mut window: Vec<u8> = Vec::with_capacity(max);
        let mut page_cursor = after as i64;
        let mut truncated = false;
        let mut more = true;
        while window.len() < max && more {
            let rows: Vec<(i64, i64, i64, i64, Vec<u8>)> = self
                .store
                .query(move |pool| {
                    Box::pin(async move {
                        sqlx::query_as::<_, (i64, i64, i64, i64, Vec<u8>)>(
                            "SELECT ofs, part, codec, plain, block FROM chunk \
                             WHERE session = ? AND ofs + plain > ? ORDER BY ofs LIMIT 64",
                        )
                        .bind(uid)
                        .bind(page_cursor)
                        .fetch_all(&pool)
                        .await
                    })
                })
                .await
                .map_err(|e| e.to_string())?;
            if rows.is_empty() {
                break;
            }
            more = rows.len() == 64;
            'feed: for (ofs, part, codec, plain, block) in rows {
                if ofs + plain <= page_cursor {
                    continue;
                }
                let binding =
                    ChunkBinding::row("terminal", "", uid, part, ofs as u64, plain as usize, codec as u8);
                let coded = match self.keys.open_block(&block, &binding) {
                    OpenBlock::Sealed(bytes) => bytes,
                    OpenBlock::Plain => block,
                    OpenBlock::Refused => return Err("chunk-envelope-refused".into()),
                };
                let bytes = BodyCodec::from_u8(codec as u8)
                    .and_then(|c| body::decode_chunk(c, &coded, plain as usize).ok())
                    .ok_or_else(|| "chunk-codec-refused".to_string())?;
                let skip = if ofs > page_cursor {
                    0
                } else {
                    (page_cursor - ofs) as usize
                };
                let slice = &bytes[skip.min(bytes.len())..];
                let take = slice.len().min(max - window.len());
                window.extend_from_slice(&slice[..take]);
                page_cursor = ofs + skip as i64 + take as i64;
                if window.len() >= max {
                    truncated = true;
                    break 'feed;
                }
            }
        }
        if window.is_empty() && page_cursor == after as i64 {
            // No part covers the cursor: swept, never-recorded, or no such session.
            return Ok(None);
        }
        // Start edge: the cursor may have landed inside a multi-byte character —
        // those leading continuation bytes are unservable and are consumed here.
        let mut lead = 0;
        while lead < window.len() && (window[lead] & 0xC0) == 0x80 {
            lead += 1;
        }
        // End edge: trim a trailing partial sequence back to a boundary so its
        // bytes return on the next page instead of becoming two U+FFFD halves.
        let body = &window[lead..];
        let keep = swiss_core::utf8::char_boundary_end(body);
        let text = String::from_utf8_lossy(&body[..keep]).into_owned();
        let consumed = lead + keep;
        let next = if consumed < window.len() || truncated {
            Some(after + consumed as u64)
        } else {
            None
        };
        Ok(Some((text, next)))
    }

}

/// One session's index row, as the panel list and the export route see it.
#[derive(Debug, serde::Serialize)]
pub struct SessionRow {
    pub uid: i64,
    pub sid: String,
    pub started_ms: i64,
    pub cols: i64,
    pub rows: i64,
    pub ended_ms: Option<i64>,
    pub end_reason: Option<String>,
    pub capped: bool,
    pub imported: bool,
    /// The recording's measured plaintext bytes (the quota counts physical bytes; this is
    /// the honest content size).
    pub bytes: i64,
}

impl BodySource for TerminalHistory {
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>> {
        let store = self.store.clone();
        Box::pin(async move {
            // The bounded writer queue commits before the scan reads: a query racing a
            // fresh part must see the chunk, not a phantom empty stream. A failing
            // barrier means a write failed to land — warn, never scan silently short.
            if let Err(e) = store.flush_barrier().await {
                swiss_core::log::warn(
                    "terminal.db body-scan barrier",
                    Some(serde_json::json!({ "error": e.to_string() })),
                );
            }
            store
                .query::<Vec<StoredChunk>, _>(move |pool| {
                    Box::pin(async move {
                        // The cursor speaks stored part offsets, so a resume anchored at
                        // ANY part — not just part 0 — still returns that session's
                        // later parts. INCLUSIVE, like every other owner: the engine's
                        // carry-rebuild passes the anchor chunk's OWN key and expects
                        // the anchor back, then asks for start + 1 (mcp's comment
                        // verbatim). An exclusive predicate makes the rebuild fetch
                        // the chunk AFTER the anchor, the carry never lines up at the
                        // boundary, and every match straddling it is silently lost.
                        sqlx::query(
                            "SELECT session, ofs, part, codec, plain, block FROM chunk \
                             WHERE (session > ?1 OR (session = ?1 AND ofs >= ?2)) \
                             ORDER BY session, ofs LIMIT ?3",
                        )
                        .bind(from.0)
                        .bind(from.1)
                        .bind(limit as i64)
                        .fetch_all(&pool)
                        .await
                        .and_then(|rows| {
                            rows.into_iter()
                                .map(|r| {
                                    Ok(StoredChunk {
                                        run_id: r.try_get::<i64, _>(0)?,
                                        ofs: r.try_get::<i64, _>(1)?,
                                        part: r.try_get::<i64, _>(2)?,
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
        // The scan snapshot epoch: bumped by sweeps, distinct from the store's
        // worker-identity generation.
        self.snapshot.load(Ordering::Acquire)
    }

    fn decode(&self, chunk: &StoredChunk) -> Vec<u8> {
        let binding = ChunkBinding::row(
            "terminal",
            "",
            chunk.run_id,
            chunk.part,
            chunk.ofs as u64,
            chunk.plain as usize,
            chunk.codec as u8,
        );
        let coded = match self.keys.open_block(&chunk.block, &binding) {
            OpenBlock::Sealed(bytes) => bytes,
            OpenBlock::Plain => chunk.block.clone(),
            // A refused envelope is GONE for the scan: the engine skips the chunk —
            // but never silently, or a broken corpus scans as "complete, zero hits"
            // while the export route refuses it loudly.
            OpenBlock::Refused => {
                swiss_core::log::warn(
                    "terminal deep search skipped a chunk whose envelope refused",
                    Some(serde_json::json!({ "session": chunk.run_id, "part": chunk.part })),
                );
                return Vec::new();
            }
        };
        match BodyCodec::from_u8(chunk.codec as u8)
            .and_then(|c| body::decode_chunk(c, &coded, chunk.plain as usize).ok())
        {
            Some(bytes) => bytes,
            None => {
                swiss_core::log::warn(
                    "terminal deep search skipped an undecodable chunk",
                    Some(serde_json::json!({ "session": chunk.run_id, "part": chunk.part })),
                );
                Vec::new()
            }
        }
    }
}

/// The body reader for the cross-module endpoint (SPEC §host.history): one session's
/// recording joined across parts, byte-cursored and bounded. A swept (or never-recorded)
/// session says gone, never a silently short text.
pub struct TerminalBodyReader {
    history: Arc<TerminalHistory>,
}

impl TerminalBodyReader {
    pub fn new(history: Arc<TerminalHistory>) -> Self {
        TerminalBodyReader { history }
    }
}

#[async_trait::async_trait]
impl swiss_host::services::history_query::HistoryBodyReader for TerminalBodyReader {
    async fn read_body(
        &self,
        id: i64,
        after: u64,
        max: usize,
    ) -> Result<swiss_host::services::history_query::BodySlice, String> {
        match self.history.read_window(id, after, max).await? {
            None => Ok(swiss_host::services::history_query::BodySlice {
                text: String::new(),
                next: None,
                gone: true,
            }),
            Some((text, next)) => Ok(swiss_host::services::history_query::BodySlice {
                text,
                next,
                gone: false,
            }),
        }
    }
}

impl TerminalHistory {
    // --- the legacy .cast import (ADR-033's contract, ADR-035's third bullet) ------------

    /// Fold every legacy `.cast` into the store, once and idempotently. Torn tails are
    /// counted in meta and never fatal; the source files are left in place.
    async fn import_cast_tree(self: &Arc<Self>) -> Result<(), String> {
        if self.meta_get(IMPORT_KEY).await.as_deref() == Some("done") {
            return Ok(());
        }
        self.meta_put(IMPORT_KEY, "active").await;
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            // No legacy directory: nothing to fold, and that is a done import, not an
            // error. A directory that EXISTS but cannot be read (a lock, permissions)
            // is the opposite: stamping done would permanently disable the one-shot
            // import, so the marker stays "active" and the next boot retries.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.meta_put(IMPORT_KEY, "done").await;
                return Ok(());
            }
            Err(e) => {
                swiss_core::log::warn(
                    "terminal history legacy import deferred - the tree is unreadable",
                    Some(serde_json::json!({ "error": e.to_string() })),
                );
                return Ok(());
            }
        };
        // The listing gate is the gateway's own file shape (<sessionId>.cast, the
        // 32-hex stem random_hex(16) has always written). A .cast that misses the
        // shape is COUNTED as skipped, never silently vanished: the operator
        // reconciling disk against the folded count has the difference in the log.
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut skipped: u64 = 0;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let is_cast = path
                .extension()
                .map(|x| x.eq_ignore_ascii_case("cast"))
                .unwrap_or(false);
            if !is_cast {
                continue;
            }
            if path.file_stem().map(|s| s.to_string_lossy().len() == 32).unwrap_or(false) {
                paths.push(path);
            } else {
                skipped += 1;
            }
        }
        paths.sort();
        // The whole read loop runs OFF the runtime thread: a slow or hostile disk
        // (a FIFO behind a symlink, a network home) must park a blocking-pool
        // thread, never the single-threaded executor a START_TIMEOUT would need
        // to fire on. Each file is opened ONCE, sized from the handle (a symlink
        // or device target is refused by file type, not by a followable stat),
        // and read through a bounded take — the guard is enforced, not advisory.
        let store = self.store.clone();
        let keys = self.keys.clone();
        let import_paths = paths;
        let (files, torn, refused) = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut torn: u64 = 0;
            let mut refused: u64 = 0;
            for path in &import_paths {
                let files = import_paths.len() as u64;
                let _ = files;
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let sid = stem.to_string_lossy().into_owned();
                let uid = session_uid(&sid);
                let Ok(file) = std::fs::File::open(path) else {
                    torn += 1;
                    continue;
                };
                let Ok(meta) = file.metadata() else {
                    torn += 1;
                    continue;
                };
                // Only a regular file is a recording; a symlink, FIFO or device
                // behind a 32-hex name is planted, not recorded.
                if !meta.is_file() || meta.len() > EXPORT_GUARD_BYTES {
                    torn += 1;
                    continue;
                }
                let mut raw = Vec::with_capacity(meta.len() as usize);
                let read = (&file)
                    .take(EXPORT_GUARD_BYTES + 1)
                    .read_to_end(&mut raw);
                if read.is_err() || raw.len() as u64 > EXPORT_GUARD_BYTES {
                    torn += 1;
                    continue;
                }
                match parse_cast(&raw) {
                    Ok((header, lines, file_torn)) => {
                        torn += file_torn;
                        // Honest weight: the admission estimate carries the file's
                        // real size, so the queue bound and the quota projection
                        // see what actually lands (a constant would let a whole
                        // tree queue as resident closures the bound never sees).
                        if let Err(e) = submit_imported(
                            &store,
                            &keys,
                            uid,
                            &sid,
                            header,
                            lines,
                            raw.len() + 96,
                        ) {
                            refused += 1;
                            swiss_core::log::warn(
                                "terminal.db import admission",
                                Some(serde_json::json!({
                                    "file": sid,
                                    "error": e.to_string(),
                                })),
                            );
                        }
                    }
                    Err(()) => {
                        torn += 1;
                    }
                }
            }
            (import_paths.len() as u64, torn, refused)
        })
        .await
        .map_err(|e| format!("the import task failed to join: {e}"))?;
        // A refusal (queue bound, quota) never entered the queue, so the barrier
        // below proves nothing about it: stamping "done" anyway would permanently
        // skip those files. Count them, leave the marker "active", retry next boot.
        if refused > 0 {
            swiss_core::log::warn(
                "terminal history legacy import left unfinished - files were refused admission, the next open retries them",
                Some(serde_json::json!({ "refused": refused })),
            );
            return Ok(());
        }
        // The barrier BEFORE the stamp: an import write that failed to land must not
        // hide behind "done" — the next boot retries the whole tree (INSERT OR IGNORE
        // makes that free) rather than silently losing that file forever.
        if let Err(e) = self.store.flush_barrier().await {
            self.on_write_error("terminal history legacy import barrier", &e);
            swiss_core::log::warn(
                "terminal history legacy import left unfinished - the next open retries it",
                None,
            );
            return Ok(());
        }
        self.meta_put(IMPORT_KEY, "done").await;
        self.meta_put(TORN_KEY, &torn.to_string()).await;
        if files > 0 || skipped > 0 {
            swiss_core::log::log(
                "info",
                "terminal history legacy import folded",
                Some(serde_json::json!({ "files": files, "torn": torn, "skipped": skipped })),
            );
        } else {
            // Zero files on a fresh home consumed the one-shot import silently until
            // this line: an operator who copies a legacy tree in later can now see
            // from the boot log WHY it never folded.
            swiss_core::log::log(
                "info",
                "terminal history legacy import found no source recordings; marked done",
                None,
            );
        }
        Ok(())
    }
}

/// Queue one parsed legacy file, weighted by its real size. A module-level fn so
/// the import's blocking thread can submit without borrowing the history handle;
/// the admission result travels back — a refused file must not hide behind "done".
fn submit_imported(
    store: &ModuleHistoryStore,
    keys: &ChunkKeys,
    uid: i64,
    sid: &str,
    header: CastHeader,
    lines: Vec<Vec<u8>>,
    estimated: usize,
) -> Result<(), swiss_host::history::store::HistoryWriteError> {
    let sid = sid.to_string();
    let started_ms = header.timestamp_ms;
    let cols = header.cols as i64;
    let rows = header.rows as i64;
    let term = header.term;
    let keys = keys.clone();
    store.submit(estimated, move |pool| {
        Box::pin(async move {
                sqlx::query(
                    "INSERT OR IGNORE INTO session \
                     (uid, sid, started_ms, cols, rows, term, ended_ms, end_reason, imported) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, 'legacy import', 1)",
                )
                .bind(uid)
                .bind(&sid)
                .bind(started_ms)
                .bind(cols)
                .bind(rows)
                .bind(&term)
                // A legacy recording is ENDED the moment it is folded in: ended_ms =
                // started_ms puts it on the 90-day sweep clock immediately (an ended_ms
                // of NULL means "live" to the sweep, and would exempt exactly the
                // oldest pile from the retention this store exists to enforce).
                .bind(started_ms)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                let mut ofs: u64 = 0;
                for (part, plain) in lines.iter().enumerate() {
                    let plain_len = plain.len();
                    let (codec, coded) = body::encode_chunk(plain);
                    let binding =
                        ChunkBinding::row("terminal", "", uid, part as i64, ofs, plain_len, codec.as_u8());
                    // The import seals with the SAME keys a live recording uses: one
                    // corpus, one envelope discipline (SPEC §host.history).
                    let block = keys.seal_block("history:terminal:chunk", &binding, &coded);
                    sqlx::query(
                        "INSERT OR IGNORE INTO chunk \
                         (session, part, ofs, plain, codec, block) VALUES (?, ?, ?, ?, ?, ?)",
                    )
                    .bind(uid)
                    .bind(part as i64)
                    .bind(ofs as i64)
                    .bind(plain_len as i64)
                    .bind(codec as u8 as i64)
                    .bind(&block)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                    ofs += plain_len as u64;
                }
                Ok(())
        })
    })
}

impl TerminalHistory {
    async fn meta_get(&self, key: &'static str) -> Option<String> {
        match self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query_scalar::<_, String>("SELECT value FROM meta WHERE key = ?")
                        .bind(key)
                        .fetch_optional(&pool)
                        .await
                })
            })
            .await
        {
            Ok(v) => v,
            // A failed read is not "not done": it warns so the symptom (the import
            // re-running, idempotently) has a visible cause next to it.
            Err(e) => {
                swiss_core::log::warn(
                    "terminal history meta read failed",
                    Some(serde_json::json!({ "key": key, "error": e.to_string() })),
                );
                None
            }
        }
    }

    async fn meta_put(&self, key: &'static str, value: &str) {
        let value = value.to_string();
        let submitted = self.store.submit(48, move |pool| {
            Box::pin(async move {
                sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
                    .bind(key)
                    .bind(&value)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        if let Err(e) = submitted {
            self.on_write_error("terminal.db meta admission", &e);
        }
        self.flush().await;
    }
}
/// The row key every session uses, live and imported alike: the first 15 hex characters
/// of the session id (60 bits, always positive). One derivation, so a re-import maps onto
/// the same rows a live session would have taken; the UNIQUE constraint catches a birthday
/// accident as one skipped recording, never a live-session failure. A stem that is not
/// hex, or whose byte 15 falls inside a multibyte character (possible in the lossy-converted
/// legacy filenames), derives from a digest instead of panicking: never fatal, never the
/// same fallback for every such file (which would silently fold them onto one row).
pub fn session_uid(sid: &str) -> i64 {
    // get(..15) returns None exactly when byte 15 is not a char boundary.
    let prefix = sid.get(..15).unwrap_or(sid);
    match u64::from_str_radix(prefix, 16) {
        Ok(v) => v as i64,
        // Non-hex or cut mid-character: fold the WHOLE id through a default hasher so
        // distinct ids stay distinct instead of colliding on a constant.
        Err(_) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            sid.hash(&mut h);
            (h.finish() & 0x0fff_ffff_ffff_ffff) as i64
        }
    }
}


/// What a legacy `.cast` header told us, in the store's own columns.
struct CastHeader {
    timestamp_ms: i64,
    cols: u16,
    rows: u16,
    term: String,
}

/// Parse one legacy `.cast`: the header object, then the event lines' RAW bytes (they
/// are stored verbatim — the whole point of the format-preserving store). A header that
/// is not a JSON object, or a trailing line without its newline (a torn tail from a crash
/// mid-write), refuses the file: Err(()) — the importer counts it and moves on.
fn parse_cast(raw: &[u8]) -> Result<(CastHeader, Vec<Vec<u8>>, u64), ()> {
    let text = std::str::from_utf8(raw).map_err(|_| ())?;
    let mut lines = text.split_inclusive('\n');
    let header_line = lines.next().ok_or(())?;
    let header: serde_json::Value =
        serde_json::from_str(header_line.trim_end_matches(['\r', '\n'])).map_err(|_| ())?;
    if !header.is_object() {
        return Err(());
    }
    // asciicast v2 timestamps are SECONDS since the epoch. A header missing the
    // field falls back to now — in seconds, so the x1000 to milliseconds below
    // lands in this era rather than the year 55,976. Some foreign writers emit
    // the integer as a float; a float that cannot be read AT ALL would fall back
    // to now and hand an old recording today's date, so the integral float is
    // tolerated (1700000000.0 reads as 1700000000).
    let timestamp_s = header
        .get("timestamp")
        .and_then(|t| t.as_i64())
        .or_else(|| {
            header
                .get("timestamp")
                .and_then(|t| t.as_f64())
                .map(|f| f as i64)
        })
        .unwrap_or_else(|| (swiss_core::util::now_ms() / 1000) as i64);
    // Geometry is refused, never clamped (the API route's own rule: a value past
    // u16 would wrap straight through a naive cast — width 100000 imports as
    // 34464, width 65536 as a 0-column header). The same 1..=1000 window the
    // open route enforces; a header outside it is a foreign file, counted torn.
    let in_window = |v: Option<u64>| -> Result<u16, ()> {
        match v {
            Some(n) if (1..=1000).contains(&n) => Ok(n as u16),
            _ => Err(()),
        }
    };
    let header = CastHeader {
        timestamp_ms: timestamp_s.saturating_mul(1000),
        cols: in_window(header.get("width").and_then(|w| w.as_u64()))?,
        rows: in_window(header.get("height").and_then(|h| h.as_u64()))?,
        term: header
            .pointer("/env/TERM")
            .and_then(|t| t.as_str())
            .unwrap_or("xterm-256color")
            .to_string(),
    };
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut buf: Vec<u8> = Vec::with_capacity(PART_BYTES);
    let mut torn: u64 = 0;
    for line in lines {
        let bytes = line.as_bytes();
        // A trailing chunk without its newline is the torn tail of a crash mid-write:
        // dropped and counted, never imported half a JSON value.
        if !bytes.ends_with(b"\n") {
            torn += 1;
            break;
        }
        buf.extend_from_slice(bytes);
        // Parts are cut BY BYTES, not at line boundaries: one event line can be
        // arbitrarily long (JSON escape expansion multiplies control bytes 6x),
        // and a part carrying a whole such line would blow past the 128 KiB cap
        // every reader refuses — the recording would import and then never export.
        // The stream is the storage format; line boundaries inside a part are
        // nothing any reader ever looks at.
        while buf.len() >= PART_BYTES {
            let rest = buf.split_off(PART_BYTES);
            parts.push(std::mem::replace(&mut buf, rest));
        }
    }
    if !buf.is_empty() {
        parts.push(buf);
    }
    Ok((header, parts, torn))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo's scratch-home pattern: a unique subtree under SWISS_HOME (pinned by
    /// test-utils), so a test database never touches a real home.
    fn temp_home(tag: &str) -> (PathBuf, PathBuf) {
        let root = swiss_core::paths::test_home().join("terminal-history").join(tag);
        let sessions = root.join("terminal");
        let logs = root.join("logs");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::create_dir_all(&logs).expect("logs dir");
        (sessions, logs)
    }

    fn write_cast(dir: &Path, sid: &str, body: &str) {
        std::fs::write(dir.join(format!("{sid}.cast")), body).expect("write cast");
    }

    #[test]
    fn session_uid_survives_non_hex_and_multibyte_stems() {
        // The OLD derivation sliced byte 15 blind — a multibyte stem PANICKED
        // inside plugin start — and folded every non-hex stem onto uid 1 (a
        // silent mass collision). Both now derive deterministically, distinctly.
        let a = session_uid(&"ü".repeat(40));
        let b = session_uid("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
        let c = session_uid("0123456789abcdeffffffffffffffff");
        assert_ne!(a, b, "non-hex stems no longer share uid 1");
        assert_ne!(a, c);
        assert_eq!(a, session_uid(&"ü".repeat(40)), "deterministic");
        assert!(a > 0 && b > 0 && c > 0, "positive i64 keys");
    }

    #[test]
    fn a_header_without_a_timestamp_lands_in_this_era() {
        // The old fallback multiplied now_ms() (already ms) by 1000 — the year
        // 55,976, sorting first forever.
        let raw = b"{\"version\":2,\"width\":80,\"height\":24}\n[1.0, \"o\", \"x\"]\n";
        let (h, _, _) = parse_cast(raw).expect("parses");
        let now = swiss_core::util::now_ms() as i64;
        assert!(
            h.timestamp_ms > now - 60_000 && h.timestamp_ms <= now,
            "fallback is now, not the year 55,976: {} vs {}",
            h.timestamp_ms,
            now
        );
        // A float timestamp (some foreign writers) reads as its integer seconds;
        // falling back to now would hand an old recording today's date and a
        // 90-day lease from import day.
        let raw = b"{\"version\":2,\"width\":80,\"height\":24,\"timestamp\":1700000000.0}\n[1.0, \"o\", \"x\"]\n";
        let (h, _, _) = parse_cast(raw).expect("parses");
        assert_eq!(h.timestamp_ms, 1_700_000_000_000);
    }

    #[test]
    fn a_cast_with_geometry_past_the_window_is_refused() {
        // The open route refuses, never clamps (its own pinned rule: a value past
        // u16 wraps straight through a naive cast — width 100000 as 34464). The
        // import path honors the same window now.
        let raw = b"{\"version\":2,\"width\":100000,\"height\":24}\n[1.0, \"o\", \"x\"]\n";
        assert!(parse_cast(raw).is_err());
        let raw = b"{\"version\":2,\"width\":80,\"height\":65536}\n[1.0, \"o\", \"x\"]\n";
        assert!(parse_cast(raw).is_err());
        let raw = b"{\"version\":2,\"width\":80,\"height\":24}\n[1.0, \"o\", \"x\"]\n";
        assert!(parse_cast(raw).is_ok());
    }

    #[tokio::test]
    async fn imported_recordings_join_the_sweep_at_their_recorded_age() {
        let (sessions, logs) = temp_home("import-sweep");
        // A recording from 1970: past the 90-day clock the moment it folds in —
        // ended_ms = started_ms is the deliberate enrollment of the legacy pile.
        write_cast(
            &sessions,
            &"b".repeat(32),
            "{\"version\":2,\"width\":80,\"height\":24,\"timestamp\":100}\n[1.0, \"o\", \"old\"]\n",
        );
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        assert_eq!(h.list_sessions(10).await.expect("list").len(), 1);
        h.maintain(90 * 24 * 60 * 60 * 1000).await;
        assert!(
            h.list_sessions(10).await.expect("list").is_empty(),
            "the imported row swept with its own age"
        );
        // The source stays on disk: retiring it is the operator's call (ADR-035).
        assert!(sessions.join(format!("{}.cast", "b".repeat(32))).exists());
    }

    #[tokio::test]
    async fn the_sweep_collects_chunks_whose_session_row_never_landed() {
        let (sessions, logs) = temp_home("orphan-sweep");
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        let uid = session_uid(&"c".repeat(32));
        h.begin(uid, &"c".repeat(32), 80, 24, "xterm").expect("begin");
        let _ = h.append_part(uid, 0, 0, b"[1.0, \"o\", \"ghost\"]\n");
        h.flush().await;
        // Simulate the orphan the defense targets: the chunk landed, its session
        // row is gone (refused begin, a lost race) — invisible to every reader.
        let gone = uid;
        let _ = h
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("DELETE FROM session WHERE uid = ?")
                        .bind(gone)
                        .execute(&pool)
                        .await
                })
            })
            .await;
        h.maintain(90 * 24 * 60 * 60 * 1000).await;
        let left: i64 = h
            .store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query_scalar("SELECT COUNT(*) FROM chunk WHERE session = ?")
                        .bind(gone)
                        .fetch_one(&pool)
                        .await
                })
            })
            .await
            .expect("count");
        assert_eq!(left, 0, "no session row means the sweep collects the parts");
    }

    #[tokio::test]
    async fn a_legacy_file_past_the_export_guard_is_refused_before_reading() {
        // The import's size refusal runs on METADATA: a file past the 64 MB export
        // guard is skipped before a single byte of it is read into memory. A
        // sparse file of guard+1 bytes pins the branch without materializing the
        // corpus. The refusal counts as torn (the "not imported" diagnostic), the
        // source stays on disk, and the fold still completes - an oversized
        // stranger in the tree must not wedge the one-shot import forever.
        let (sessions, logs) = temp_home("oversized");
        let path = sessions.join(format!("{}.cast", "a".repeat(32)));
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(EXPORT_GUARD_BYTES + 1).expect("a sparse length costs nothing");
        drop(file);
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        assert!(
            h.list_sessions(10).await.expect("list").is_empty(),
            "a file past the export guard never becomes a session"
        );
        // Skipped, not refused: only an ADMISSION refusal leaves the marker
        // active for the next boot. The fold is done.
        assert_eq!(
            h.meta_get(IMPORT_KEY).await.as_deref(),
            Some("done"),
            "an oversized file is skipped, never wedges the import"
        );
        assert!(path.exists(), "the oversized source stays for the operator");
        h.close().await.expect("close");
    }

    #[tokio::test]
    async fn a_resumed_scan_finds_needles_across_the_page_anchor() {
        let (sessions, logs) = temp_home("resume");
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        let uid = session_uid(&"d".repeat(32));
        h.begin(uid, &"d".repeat(32), 80, 24, "xterm").expect("begin");
        // 64 full parts, then a needle straddling parts 64 and 65: the first
        // scan page ends at chunk 64 and the RESUME's carry rebuild must return
        // the anchor chunk itself — the inclusive-cursor contract the exclusive
        // predicate broke (matches straddling the anchor were silently lost).
        let filler = format!("[1.0, \"o\", \"{}\"]\n", "f".repeat(100));
        let mut ofs = 0u64;
        let mut part = 0i64;
        for _ in 0..64 {
            let mut c: Vec<u8> = Vec::new();
            while c.len() + filler.len() <= PART_BYTES {
                c.extend_from_slice(filler.as_bytes());
            }
            let _ = h.append_part(uid, part, ofs, &c);
            ofs += c.len() as u64;
            part += 1;
        }
        let mut head: Vec<u8> = Vec::new();
        while head.len() + filler.len() <= PART_BYTES - 9 {
            head.extend_from_slice(filler.as_bytes());
        }
        head.extend_from_slice(b"NEEDLEAC");
        let tail = b"ROSSTHEEDGE\n".to_vec();
        let _ = h.append_part(uid, part, ofs, &head);
        let _ = h.append_part(uid, part + 1, ofs + head.len() as u64, &tail);
        h.finish(uid, "done", false).expect("finish");
        h.flush().await;
        // The engine pages at 64 chunks: follow the cursor as a real reader does,
        // page by page, until the stream completes.
        let mut cursor: Option<swiss_host::history::search::ScanCursor> = None;
        let mut found = 0usize;
        for _ in 0..6 {
            let reply = swiss_host::history::search::scan(
                h.as_ref(),
                "terminal",
                "NEEDLEACROSSTHEEDGE",
                cursor.as_ref(),
                100,
            )
            .await
            .expect("scan");
            found += reply.hits.len();
            match reply.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(found, 1, "the straddling needle is found exactly once across pages");
    }

    #[tokio::test]
    async fn a_live_recording_round_trips_byte_faithfully_through_the_export() {
        let (sessions, logs) = temp_home("roundtrip");
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        let uid = 4242i64;
        let _ = h.begin(uid, "a".repeat(32).as_str(), 100, 30, "xterm-256color");
        // Two parts: one full 64 KiB and a tail — the recorder's exact flush shape.
        let big_line = format!("[1.5, \"o\", \"{}\"]\n", "x".repeat(100));
        let mut part0: Vec<u8> = Vec::new();
        while part0.len() + big_line.len() <= PART_BYTES {
            part0.extend_from_slice(big_line.as_bytes());
        }
                let _ = h.append_part(uid, 0, 0, &part0);
        let tail = b"[2.0, \"o\", \"bye\"]\n";
                let _ = h.append_part(uid, 1, part0.len() as u64, tail);
                let _ = h.finish(uid, "closed", false);
        h.flush().await;
        let cast = h
            .export_cast(&"a".repeat(32))
            .await
            .expect("export")
            .expect("some recording");
        let mut lines = cast.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 100);
        assert_eq!(header["height"], 30);
        let rest: String = cast[cast.find('\n').unwrap() + 1..].to_string();
        assert_eq!(rest.len(), part0.len() + tail.len());
        assert!(rest.ends_with("[2.0, \"o\", \"bye\"]\n"));
    }

    /// A garbage database file (no sqlite magic anywhere) must refuse to open, so
    /// plugin start degrades to recording-off instead of registering a broken
    /// seat. Found while probing the live corrupted-store case: a garbage MAIN
    /// file beside a healthy -wal sidecar still opens (sqlite replays the wal),
    /// and only fails reads mid-scan — that half is covered by the host-side
    /// ScanFailed regression; this one pins the pure-garbage half.
    #[tokio::test]
    async fn a_garbage_database_file_refuses_to_open() {
        let (sessions, logs) = temp_home("garbage");
        std::fs::write(
            logs.join("terminal.db"),
            "this is not a sqlite database at all".repeat(80),
        )
        .expect("write garbage");
        assert!(
            TerminalHistory::open(&sessions, &logs).await.is_err(),
            "a garbage file must not open as a terminal history store"
        );
    }

    #[tokio::test]
    async fn a_torn_tail_is_dropped_and_counted_and_the_rest_imports() {
        let (sessions, logs) = temp_home("torn");
        let sid = "0123456789abcdef0123456789abcdef";
        write_cast(
            &sessions,
            sid,
            "{\"version\":2,\"width\":90,\"height\":25,\"timestamp\":1700000000,\"env\":{\"TERM\":\"xterm-256color\"}}\n[0.1, \"o\", \"hello\"]\n[0.2, \"o\", \"wor",
        );
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        h.flush().await;
        let cast = h.export_cast(sid).await.expect("export").expect("some");
        assert!(cast.contains("hello"));
        assert!(!cast.contains("wor"));
        let torn = h.meta_get(TORN_KEY).await;
        assert_eq!(torn.as_deref(), Some("1"));
        let list = h.list_sessions(10).await.expect("list");
        assert_eq!(list.len(), 1);
        assert!(list[0].imported);
        assert_eq!(list[0].cols, 90);
        // Idempotent: a second open maps onto the same rows, not duplicates.
        drop(h);
        let h2 = TerminalHistory::open(&sessions, &logs).await.expect("reopen");
        h2.flush().await;
        let list2 = h2.list_sessions(10).await.expect("list2");
        assert_eq!(list2.len(), 1);
    }

    #[tokio::test]
    async fn the_sweep_takes_ended_old_sessions_and_never_live_ones() {
        // The aging write goes through the owner's queue like every write.
        let (sessions, logs) = temp_home("sweep");
        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        let old = 11i64;
        let live = 12i64;
        let _ = h.begin(old, &"b".repeat(32), 80, 24, "xterm-256color");
                let _ = h.append_part(old, 0, 0, b"[0.1, \"o\", \"old\"]\n");
                let _ = h.finish(old, "closed", false);
        let _ = h.begin(live, &"c".repeat(32), 80, 24, "xterm-256color");
                let _ = h.append_part(live, 0, 0, b"[0.1, \"o\", \"live\"]\n");
        h.flush().await;
        // Age the old session's ended_ms past any retention window by writing it back.
        {
            let store = h.store.clone();
            let submitted = store.submit(48, move |pool| {
                Box::pin(async move {
                    sqlx::query("UPDATE session SET ended_ms = 1 WHERE uid = ?")
                        .bind(old)
                        .execute(&pool)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            });
            assert!(submitted.is_ok(), "the aging write was admitted");
        }
        h.flush().await;
        h.sweep(90 * 24 * 60 * 60 * 1000).await;
        let list = h.list_sessions(10).await.expect("list");
        let sids: Vec<&str> = list.iter().map(|r| r.sid.as_str()).collect();
        assert_eq!(sids, vec!["c".repeat(32).as_str()]);
        // The cursor epoch bumped with the sweep.
        assert_eq!(h.generation(), 2);
    }

    #[test]
    fn an_unreadable_creation_time_does_not_end_a_live_recording() {
        assert!(!recording_owner_gone(42, 0, true, None), "macOS/BSD cannot read creation times");
        assert!(!recording_owner_gone(42, 100, true, None), "a transient read failure is not death");
        assert!(!recording_owner_gone(42, 0, true, Some(100)), "an unknown stored identity cannot prove reuse");
        assert!(!recording_owner_gone(42, 100, true, Some(100)));
        assert!(recording_owner_gone(42, 100, true, Some(200)), "a known changed identity proves reuse");
        assert!(recording_owner_gone(42, 0, false, None), "a missing process really ended");
        assert!(recording_owner_gone(0, 0, false, None), "legacy ownerless recordings are adoptable");
    }

    /// A crash between begin and finish leaves a row that never ends: ended_ms NULL,
    /// invisible to the sweep (which only reads ended rows), holding its bytes against
    /// the share forever. The owner columns name who began it: every maintenance pass
    /// adopts a row whose owner is gone - stamped ended at its start, with a crash
    /// note, back on the sweep clock - while a row whose owner still lives (this
    /// process, or a peer sharing the database) stays exactly as live as it is.
    #[tokio::test]
    async fn crash_interrupted_recordings_rejoin_the_sweep_clock() {
        let (sessions, logs) = temp_home("crash-adopt");
        // A short-lived process plays the crashed owner: its pid and creation are read
        // while it lives; its exit is the crash.
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.arg("/c").arg("exit 0");
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg("exit 0");
            c
        };
        let mut child = command.spawn().expect("spawn the owner");
        let owner_pid = child.id();
        let owner_created = swiss_core::platform::process_created_at(owner_pid).unwrap_or(0);
        child.wait().expect("the owner exits");

        let h = TerminalHistory::open(&sessions, &logs).await.expect("open");
        let dead = 21i64;
        let _ = h.begin(dead, &"d".repeat(32), 80, 24, "xterm-256color");
        let mine = 22i64;
        let _ = h.begin(mine, &"e".repeat(32), 80, 24, "xterm-256color");
        h.flush().await;
        // The dead owner's identity onto the first row: what begin would have stamped
        // had that process been the one to run it.
        {
            let store = h.store.clone();
            let (owner_pid, owner_created) = (owner_pid as i64, owner_created as i64);
            store
                .submit(48, move |pool| {
                    Box::pin(async move {
                        sqlx::query("UPDATE session SET owner_pid = ?, owner_created = ? WHERE uid = ?")
                            .bind(owner_pid)
                            .bind(owner_created)
                            .bind(dead)
                            .execute(&pool)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                })
                .expect("stamp the dead owner");
        }
        h.flush().await;
        // Adoption only: a year of retention age sweeps nothing yet.
        h.maintain(365 * 24 * 60 * 60 * 1000).await;
        let rows = h.list_sessions(10).await.expect("list");
        let dead_row = rows.iter().find(|r| r.sid == "d".repeat(32)).expect("the dead row lists");
        assert!(dead_row.ended_ms.is_some(), "adopted at its owner's death");
        assert!(
            dead_row.end_reason.as_deref().unwrap_or_default().contains("crash"),
            "the reason says what happened: {:?}",
            dead_row.end_reason
        );
        let mine_row = rows.iter().find(|r| r.sid == "e".repeat(32)).expect("the live row lists");
        assert!(mine_row.ended_ms.is_none(), "this process's live row stays live");

        // Back on the sweep clock: with the age at zero the adopted row goes, the live
        // one never does.
        h.maintain(0).await;
        let list = h.list_sessions(10).await.expect("list");
        let sids: Vec<&str> = list.iter().map(|r| r.sid.as_str()).collect();
        assert_eq!(sids, vec!["e".repeat(32).as_str()], "the crashed owner's row swept; the live row stays");
    }

    #[test]
    fn session_uids_are_deterministic_and_positive() {
        let a = session_uid("0123456789abcdef0123456789abcdef");
        assert_eq!(a, session_uid("0123456789abcdef0123456789abcdef"));
        assert!(a > 0);
        assert_ne!(a, session_uid("ffffffffffffffffffffffffffffffff"));
    }

    #[test]
    fn parse_cast_rejects_a_non_object_header() {
        assert!(parse_cast(b"[]\n[0.1, \"o\", \"x\"]\n").is_err());
        assert!(parse_cast(b"not json\n").is_err());
    }
}
