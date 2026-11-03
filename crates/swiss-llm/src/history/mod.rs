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

//! The llm module's own history database (SPEC `llm.history): one `logs/llm.db` holding
//! every provider round trip - chat and embedding, from every consumer - and every agent
//! conversation turn, on the shared machinery of SPEC `host.history`. A call row answers
//! "what did the model cost and say"; a run's turns answer "what was the whole dialogue".
//! Nothing here dispatches on a consumer's identity: the recording rides the same seams
//! telemetry does (a call at the ask, a turn at the loop), so a disabled telemetry
//! collector never silences the local record.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

use swiss_host::history::body;
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{Durability, HistoryWriteError, ModuleHistoryStore, OwnerPolicy};

use crate::provider::{CallError, Tokens};

/// How much of a body a page row carries inline; the rest waits in the body chunks.
const PREVIEW_MAX: usize = 2 * 1024;
/// How much of a call's prompt and preview a dialogue carries: the opening, as a run
/// row's `head` (the whole prompt is the run's user turn; the wire, the call's body).
const DIGEST_OPENING: usize = 400;

/// The size trim's bounds, as percents of the llm share (SPEC llm.history): a pass
/// starts once the file or its live pages pass the first, and drops the oldest
/// history until the live pages fit the second - room for days of writes before the
/// next pass, never a trim on every write.
const TRIM_START_PCT: u64 = 90;
const TRIM_GOAL_PCT: u64 = 70;
/// Rounds per pass: a piece a newer call still names stays, so one cut can free
/// less than its share of the rows; each round measures again.
const TRIM_ROUNDS: usize = 8;
/// The least one round drops, as a fraction of the droppable rows.
const TRIM_MIN_FRACTION: f64 = 0.1;
/// A refused write asks for a pass at most this often.
const TRIM_GAP_MS: i64 = 60_000;

mod calls;
mod live;
mod pieces;
mod reads;
mod recorder;
mod rows;
mod runs;
mod trim;

use self::live::*;
pub use self::live::LiveTap;
use self::pieces::*;
pub use self::recorder::*;
use self::rows::*;
use self::trim::*;

#[cfg(test)]
mod version_tests;
#[cfg(test)]
mod authority_tests;
/// The private lifecycle seam: close_run must be a QUEUED, unpriced write
/// (the submit_exempt precedent), not a query-side mutation. Observable only
/// from inside the crate: the store's own admitted/completed counters change
/// for a queued write and NEVER for a query. The withdrawal guard in the
/// integration suite pairs with this; it is not this proof.
#[cfg(test)]
mod close_queue_seam;
#[cfg(test)]
mod size_trim;
#[cfg(test)]
mod cell_seam;
#[cfg(test)]
mod v4_migration;
#[cfg(test)]
mod tool_round_settle_seam;
/// Lost-proof contracts (SPEC llm.history fail-closed family), pinned by a
/// PRIVATE test-only seam: [LlmHistory::drop_reply_after_commit] skips ONE
/// reply after its SQL committed and before the send, on the REAL jobs -
/// the store's actual reservation and activity writes, no copied protocol,
/// no global flag, zero release surface (cfg(test)). Every await is bound;
/// drains serve VISIBILITY of committed state only, never authority.
#[cfg(test)]
mod lost_reply_seam;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tail_privacy_seam;

/// The llm owner's schema (the version discipline is the host's:
/// `history::schema`: DDL and stamp commit in one transaction; an unknown newer version
/// refuses the open with the store untouched).
static LLM_SCHEMA: &[SchemaStep] = &[SchemaStep::new(
    1,
    &[
        // Every provider round trip, chat and embedding alike. The row is the metadata a
        // page sorts by; the sealed body holds the request digest and the answer.
        "CREATE TABLE call (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at TEXT NOT NULL,
            kind TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            agent TEXT,
            ok INTEGER NOT NULL,
            ms INTEGER NOT NULL,
            status INTEGER,
            tokens_in INTEGER NOT NULL DEFAULT 0,
            tokens_out INTEGER NOT NULL DEFAULT 0,
            tokens_cache_read INTEGER NOT NULL DEFAULT 0,
            tokens_cache_write INTEGER NOT NULL DEFAULT 0,
            request_id TEXT,
            preview_text TEXT NOT NULL,
            chars INTEGER NOT NULL,
            body_flag INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE INDEX call_page ON call (id DESC)",
        "CREATE INDEX call_provider ON call (provider)",
        // One agent conversation: the run the turns belong to.
        "CREATE TABLE run (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at TEXT NOT NULL,
            at_end TEXT,
            agent TEXT NOT NULL,
            runtime TEXT NOT NULL,
            provider TEXT,
            model TEXT,
            ok INTEGER,
            ms INTEGER NOT NULL DEFAULT 0,
            turns INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE INDEX run_page ON run (id DESC)",
        // The dialogue itself: one row per note (user, assistant, tool, final, error),
        // the full text in turn_body, chunked and sealed like every other body.
        "CREATE TABLE turn (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL,
            seq INTEGER NOT NULL,
            at TEXT NOT NULL,
            role TEXT NOT NULL,
            preview_text TEXT NOT NULL,
            chars INTEGER NOT NULL,
            body_flag INTEGER NOT NULL DEFAULT 0,
            UNIQUE (run_id, seq)
        )",
        "CREATE INDEX turn_run ON turn (run_id, seq)",
        // The sealed body parts (the same shape every owner uses): the whole text of a
        // call body or a turn body, chunked at 64 KiB, codec-picked, sealed at write.
        "CREATE TABLE call_body (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            call_id INTEGER NOT NULL,
            part INTEGER NOT NULL,
            codec INTEGER NOT NULL,
            plain INTEGER NOT NULL,
            block BLOB NOT NULL,
            UNIQUE (call_id, part)
        )",
        "CREATE TABLE turn_body (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            turn_id INTEGER NOT NULL,
            part INTEGER NOT NULL,
            codec INTEGER NOT NULL,
            plain INTEGER NOT NULL,
            block BLOB NOT NULL,
            UNIQUE (turn_id, part)
        )",
    ],
), SchemaStep::new(
    2,
    &[
        // The session id on every call (SPEC llm.history): a call filed while a run
        // of the same agent was in flight belongs to that run. New calls are stamped
        // by the run's own finish transaction; this backfill gives the rows written
        // before the column existed the same answer, by the same window the read
        // used to join on (runs of one agent are serial in practice - said here, not
        // hidden in a schema change).
        "ALTER TABLE call ADD COLUMN run_id INTEGER",
        "CREATE INDEX call_run ON call (run_id)",
        "UPDATE call SET run_id = (
            SELECT id FROM run
            WHERE run.agent = call.agent AND call.at >= run.at
              AND call.at <= COALESCE(run.at_end, run.at)
            ORDER BY run.at DESC LIMIT 1
        ) WHERE run_id IS NULL",
        // The content-addressed pieces a chat body refers to (the dsh-request-log
        // design, adapted to this owner): a loop resends the whole prior conversation
        // every round trip, so whole-body storage grows with the square of the
        // dialogue while the UNIQUE content grows linearly. Each structural piece
        // (the wire facts, the system prompt, the tools array, one message) is
        // serialized once, sha256-addressed, chunked and sealed once, and every
        // round trip that resends it carries only its hash. A piece nothing points
        // at any more is inert bytes, not corruption - the quota still bounds the file.
        "CREATE TABLE piece (
            hash TEXT PRIMARY KEY,
            chars INTEGER NOT NULL
        )",
        "CREATE TABLE piece_body (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            hash TEXT NOT NULL,
            part INTEGER NOT NULL,
            codec INTEGER NOT NULL,
            plain INTEGER NOT NULL,
            block BLOB NOT NULL,
            UNIQUE (hash, part)
        )",
    ],
), SchemaStep::new(
    3,
    &[
        // Conversations vs one-shot runs (SPEC llm.history): kind 1 marks a Test
        // conversation - born OPEN, discoverable while ok IS NULL and at_end IS
        // NULL, continuable across sends, reloads and restarts until the reader
        // closes it. NULL keeps the one-shot lifecycle exactly as before: a
        // pending reservation hides from the public lists until finish writes
        // its outcome. The index serves the discovery filter in one walk.
        "ALTER TABLE run ADD COLUMN kind INTEGER",
        "CREATE INDEX run_open ON run (kind, ok, at_end, agent)",
    ],
), SchemaStep::new(
    4,
    &[
        // The durable activity protocol (SPEC llm.history). pending marks a run
        // whose ADMITTED, COMMITTED activity mark still lacks a matching
        // authoritative completion: an outbound may have flown that no sealed body
        // records, so continuation fails closed until the round files. 0 is the
        // settled state - a reserved-but-unused run included, because a reservation
        // alone is not activity and must stay continuable. attempt is the token a
        // completion must match, so a stale finish can never clear a newer
        // attempt mark. tail holds the canonical partial context of a round that
        // ended before its next ask (a streamed prefix, tool results in hand).
        // One-time policy, stated honestly: every conversation OPEN at migration
        // time predates the protocol, and its durable rows cannot prove an unfiled
        // outbound never flew. Those runs are marked -1 (ambiguous) and their
        // continuation refuses in favor of a New conversation; the rows, bodies
        // and audit trail are preserved untouched.
        "ALTER TABLE run ADD COLUMN pending INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE run ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE run ADD COLUMN tail TEXT",
        "UPDATE run SET pending = -1 WHERE kind = 1 AND ok IS NULL AND at_end IS NULL",
    ],
), SchemaStep::new(
    5,
    &[
        // The writer-known DECODED weight of what a row references (SPEC
        // llm.history): a call body's pieces are content-addressed and
        // physically stored once however many rounds re-send them, so the
        // chunk sums alone would undercharge every repeated reference. The
        // additive plain-byte weight lands at write time - envelope plus
        // every referenced piece - and the run's decoded size sums THESE,
        // never a decode-on-read of arbitrarily large bodies. Legacy rows
        // default to 0: their undercount is the honest migration cost, and
        // the outbound context cap bounds what may fly.
        "ALTER TABLE call ADD COLUMN body_plain INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE run ADD COLUMN tail_plain INTEGER NOT NULL DEFAULT 0",
    ],
), SchemaStep::new(
    6,
    &[
        // SETTLEMENT AUTHORITY (SPEC llm.history): every conversation call
        // carries the activity attempt it flew under, so a results
        // settlement can prove - inside its own transaction - that THIS
        // attempt's canonical call (the sealed Ok-with-tools body whose
        // dispatch produced those results) actually landed. The call row
        // commits only together with its sealed body chunks in
        // insert_call's one transaction, so the row's presence is the
        // body's presence; the attempt equality keeps a prior or refused
        // round's call from ever blessing a newer mark. NULL is the
        // pre-v6 legacy: audit untouched, and no live attempt can match
        // it - the proof fails closed, never guesses.
        "ALTER TABLE call ADD COLUMN attempt INTEGER",
    ],
), SchemaStep::new(
    7,
    &[
        // What a run was for and who started it (SPEC llm.history): the task
        // a consumer names (memory.recheck, memory.task, ...), its subject
        // (the domain, the file, the question's first line), the source that
        // triggered it (memory:<sync run id>), and the run whose tool call
        // started it. Rows written before the step say nothing - NULL, never
        // a guess from the prompt text.
        "ALTER TABLE run ADD COLUMN task TEXT",
        "ALTER TABLE run ADD COLUMN subject TEXT",
        "ALTER TABLE run ADD COLUMN source TEXT",
        "ALTER TABLE run ADD COLUMN parent INTEGER",
        "CREATE INDEX run_parent ON run (parent)",
    ],
), SchemaStep::new(
    8,
    &[
        // A job's runs read by its source (SPEC llm.history Live: memory:<sync
        // run id> lists every agent run that sync run started).
        "CREATE INDEX run_source ON run (source)",
    ],
)];

/// The preview of a body: the first PREVIEW_MAX characters, the char count of the whole.
fn preview_of(text: &str) -> (String, usize) {
    let chars = text.chars().count();
    let head: String = text.chars().take(PREVIEW_MAX).collect();
    (head, chars)
}


/// One chat round trip as a recording seam captured it: the request side, whose
/// agent asked (a panel test carries none), and the outcome in
/// [ChatOutcome]. One struct, not an argument list - the call sites read as a
/// sentence and the row builder reads one place (clippy's argument ceiling said
/// so first).
pub struct ChatCall<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub agent: Option<&'a str>,
    pub system: Option<&'a str>,
    pub prompt: &'a str,
    /// The loop turn this round trip served, when a loop served it.
    pub turn: Option<u32>,
    /// The wire facts of the chat that asked - endpoint, headers (every credential
    /// header named, its value never recorded) and the per-request options - so a
    /// call's body keeps the full parameter set of the ask, not a digest.
    pub wire: Option<&'a Value>,
    /// The full request as composed for this round trip: every message and every
    /// offered tool, serialized as the client held it.
    pub request: Option<&'a Value>,
    /// The loop's canonical replay log AS OF THIS ASK - the structural messages
    /// that came before this round, ids and arguments intact. Filed beside the
    /// response it is everything a continuation needs: no note-text guessing.
    pub replay: Option<&'a Value>,
    /// The in-flight activity attempt this round trip settles, when it served a
    /// conversation that marked one (SPEC llm.history: the completion must match
    /// the attempt it answers, or it leaves the mark alone).
    pub attempt: Option<u64>,
    pub outcome: ChatOutcome<'a>,
}

/// How the round trip ended. The ok side carries what the answer held; the err
/// side carries the error the caller saw, whose own duration and status become
/// the row's.
pub enum ChatOutcome<'a> {
    Ok {
        texts: &'a [String],
        /// The reasoning the model did on the way, when the wire carried it (DeepSeek,
        /// kimi, Claude thinking). Empty when there was none - the body omits the key.
        thinking: &'a [String],
        /// The tool calls the answer asked for, as (name, raw arguments, call id).
        tool_calls: &'a [(String, String, String)],
        /// Whether the stop reason cut the answer (max tokens).
        cut: bool,
        tokens: Tokens,
        ms: u64,
        /// How long the first output (thinking, text or a tool call) took, when the stream
        /// said; the body keeps it as ttftMs and the dialogue digest reads it back.
        ttft_ms: Option<u64>,
        request_id: &'a str,
        /// The provider's stop reason as one plain word, when the answer carried one.
        stop_reason: Option<&'a str>,
        /// The full response content as the adapter parsed it - every part, not just
        /// the curated keys above. The complete answer, beside the complete ask.
        contents: &'a Value,
    },
    Err(&'a CallError),
}

/// What a consumer-side deadline still knows about the round trip it dropped:
/// everything but an answer, so [LlmHistory::record_chat_timeout] files the wait
/// itself as the outcome.
pub struct ChatTimeout<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub agent: Option<&'a str>,
    pub system: Option<&'a str>,
    pub prompt: &'a str,
    pub turn: Option<u32>,
    pub waited_ms: u64,
}

/// One embedding round trip, as [LlmHistory::record_embedding] takes it: the
/// texts as asked, the latency, the vectors or the error. `agent` names the
/// consumer (memory's indexer) - absent for a panel test. The texts ride the
/// sealed body the way chat's prompt does; the vectors ride in full, each
/// rounded to six decimals - the whole answer stays inspectable (drift, dead
/// dimensions, sign flips) at a few KB per vector.
pub struct EmbedCall<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub agent: Option<&'a str>,
    pub texts: &'a [String],
    pub ms: u64,
    pub outcome: Result<&'a [Vec<f32>], &'a str>,
}

/// One module-owned history database for llm. Cheap to name, opened only inside the
/// plugin enabled scope (the caller is the plugin start, never a constructor); the
/// bounded writer queue, generations and the engine gate are the host store.
pub struct LlmHistory {
    store: Arc<ModuleHistoryStore>,
    /// The chunk sealing keys (SPEC host.history): body parts seal at write and open at
    /// read; an empty candidate list is the documented degraded plain-block mode.
    keys: swiss_host::history::seal::ChunkKeys,
    /// TEST-ONLY fault injection (cfg(test), zero release surface): drop ONE
    /// reply AFTER its SQL committed and BEFORE the send - the lost-proof
    /// shape the fail-closed contracts are pinned against. Interior
    /// mutability because the history is shared behind an Arc; scoped to
    /// this instance only, so unrelated tests never see it.
    #[cfg(test)]
    drop_reply_after_commit: std::sync::Mutex<Option<&'static str>>,
    /// The size trim a refused write starts (see [LlmHistory::trim_soon]).
    trim_gate: Arc<TrimGate>,
    /// The runs in flight: what each is doing now (SPEC llm.history Live).
    live: LiveRuns,
}

/// One trim at a time, and not again within [TRIM_GAP_MS] of the last.
#[derive(Default)]
struct TrimGate {
    running: AtomicBool,
    last_ms: AtomicI64,
}

/// What one size trim dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Trimmed {
    pub calls: u64,
    pub runs: u64,
    pub pieces: u64,
}

/// What a continuation may rebuild its prior context from (SPEC llm.history:
/// ACTIVE replay is structural - sealed request/response state, never note-text
/// guessing). The lane maps Refused reasons to the unresolved-tail 409.
pub enum ReplayState {
    /// A reserved run that never asked: the next send IS its first send. A
    /// reservation alone is not activity - this stays the positive path.
    Fresh,
    /// The canonical prior context: the last call's structural replay log plus
    /// its sealed response (and the partial tail a cancelled round left).
    Continuable { prior: Vec<Value> },
    /// Durable ambiguity: an in-flight or unanswered activity mark, a run the
    /// v4 migration marked legacy-ambiguous, a body pruned while activity
    /// exists, or a pre-protocol body without a replay log. Fail closed.
    Refused(&'static str),
}

/// The llm body-part tables as the writers lay them down (rows.rs): a call or turn body
/// binds its row id as the event, a piece binds its hash as the resource; no table
/// stores the offset, so it is the earlier parts' plain lengths summed.
static LLM_BODY_TABLES: &[swiss_host::history::cell::BodyPartTable] = &[
    swiss_host::history::cell::BodyPartTable {
        table: "call_body",
        record_col: "call_id",
        record: "call",
        binding: swiss_host::history::cell::RecordBinding::Event,
        has_ofs: false,
        key: &["id"],
    },
    swiss_host::history::cell::BodyPartTable {
        table: "turn_body",
        record_col: "turn_id",
        record: "turn",
        binding: swiss_host::history::cell::RecordBinding::Event,
        has_ofs: false,
        key: &["id"],
    },
    swiss_host::history::cell::BodyPartTable {
        table: "piece_body",
        record_col: "hash",
        record: "piece",
        binding: swiss_host::history::cell::RecordBinding::Resource,
        has_ofs: false,
        key: &["id"],
    },
];

impl LlmHistory {
    /// The Data page's decoder for this file's sealed body parts (SPEC §data.sqlite
    /// "Decoded cells"): it reads through this store and opens with these keys.
    pub fn cell_decoder(&self) -> std::sync::Arc<dyn swiss_host::services::module_db::ModuleCellDecoder> {
        std::sync::Arc::new(swiss_host::history::cell::BodyPartDecoder::new(
            "llm",
            self.store.clone(),
            self.keys.clone(),
            LLM_BODY_TABLES,
        ))
    }

    /// Open `<logs>/llm.db`. A fresh home has no logs directory until the first open
    /// makes one - the module owns its database file, so it owns the seat too.
    pub async fn open(logs_dir: &Path) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(logs_dir).map_err(|e| format!("creating the logs directory: {e}"))?;
        let policy = OwnerPolicy {
            db_file: logs_dir.join("llm.db"),
            queue_byte_bound: 8 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Normal,
            schema: LLM_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("llm", policy);
        store
            .open()
            .await
            .map_err(|e| format!("opening the llm history database: {e}"))?;
        // Enroll in the physical quota (SPEC host.history): the declared default shares,
        // no protected window - llm history is convenience history, and the honest
        // over-share denial is the plain reason.
        store.set_quota(swiss_host::history::quota::QuotaPolicy::default(), false);
        Ok(Arc::new(LlmHistory {
            store,
            keys: swiss_host::history::seal::ChunkKeys::resolve(),
            #[cfg(test)]
            drop_reply_after_commit: std::sync::Mutex::new(None),
            trim_gate: Arc::new(TrimGate::default()),
            live: LiveRuns::default(),
        }))
    }

    /// Arm or clear the TEST-ONLY lost-reply fault (see the field; cfg(test)).
    #[cfg(test)]
    fn test_set_drop_reply(&self, fault: Option<&'static str>) {
        *self
            .drop_reply_after_commit
            .lock()
            .expect("the test fault lock") = fault;
    }

    /// The store state label - the lifecycle surfaces honest status.
    pub fn state_label(&self) -> &'static str {
        self.store.state_label()
    }

    /// Wait until every admitted write has completed: the read paths visibility
    /// contract (a recorded call is visible to the very next read).
    pub async fn flush(&self) {
        let _ = self.store.flush_barrier().await;
    }

    /// The bounded close (withdraw, drain, release) - the plugin stop.
    pub async fn close(&self) -> Result<(), String> {
        self.store.close().await.map_err(|e| e.to_string())
    }

    /// Re-enroll under a different share map: the store's own seam, exposed so a
    /// test can drive its share to exhaustion within a test's lifetime. Production
    /// only ever passes [swiss_host::history::quota::QuotaPolicy::default].
    pub fn set_quota(&self, policy: swiss_host::history::quota::QuotaPolicy) {
        self.store.set_quota(policy, false);
    }

    /// One maintenance pass (SPEC host.history): the store's bounded checkpoint /
    /// VACUUM, ledger reconciliation and usage-snapshot refresh, then the age sweep
    /// and the size trim.
    /// The maintenance seat calls this hourly while the database is open - the
    /// refreshed snapshot is what makes the admission bound real at runtime (the
    /// one-shot measure at open goes stale with every landed write).
    pub async fn maintain(&self, age_ms: i64) {
        if let Err(err) = self.store.maintain().await {
            swiss_core::log::warn(
                &format!("llm.db maintain failed; the quota snapshot may be stale: {err}"),
                None,
            );
        }
        self.sweep(age_ms).await;
        self.trim().await;
    }

    /// The age sweep: calls older than the cutoff go (row and body), whole runs
    /// whose last turn ended before the cutoff go (run, turns and bodies).
    /// Content-addressed pieces stay behind: a piece is referenced by hash inside
    /// sealed bodies, so the only honest orphan test is decoding every remaining
    /// body - the size trim does that, once the file nears its share.
    pub async fn sweep(&self, age_ms: i64) {
        // The sweep rides submit_exempt, not submit: a quota that refuses the
        // sweep can never reclaim the bytes it is policing - the share would
        // wedge with no exit (the store contract, host history store.rs).
        let submitted = self.store.submit_exempt(64, move |pool| {
            Box::pin(async move {
                let cutoff = swiss_core::util::now_ms() as i64 - age_ms;
                let expired = |rows: Vec<(i64, String)>| -> Vec<i64> {
                    rows.into_iter()
                        .filter(|(_, at)| {
                            swiss_core::util::parse_iso_ms(at.as_str())
                                .map(|t| t < cutoff)
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| id)
                        .collect()
                };
                let calls: Vec<(i64, String)> = sqlx::query_as("SELECT id, at FROM call")
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let runs: Vec<(i64, String)> =
                    sqlx::query_as("SELECT id, COALESCE(at_end, at) FROM run")
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                for id in expired(calls) {
                    sqlx::query("DELETE FROM call_body WHERE call_id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query("DELETE FROM call WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                for id in expired(runs) {
                    let turns: Vec<i64> = sqlx::query("SELECT id FROM turn WHERE run_id = ?")
                        .bind(id)
                        .fetch_all(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .map(|r| r.try_get::<i64, _>(0))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    for turn in turns {
                        sqlx::query("DELETE FROM turn_body WHERE turn_id = ?")
                            .bind(turn)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    sqlx::query("DELETE FROM turn WHERE run_id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    sqlx::query("DELETE FROM run WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                tx.commit().await.map_err(|e| e.to_string())
            })
        });
        if let Err(err) = submitted {
            swiss_core::log::warn(&format!("llm.db sweep admission refused: {err}"), None);
        }
        self.flush().await;
    }

    /// The size trim (SPEC llm.history): once the file or its live pages pass
    /// [TRIM_START_PCT] of the share, the oldest history goes until the live pages fit
    /// [TRIM_GOAL_PCT], pieces nothing names any more go with it, and the file is
    /// compacted. The age sweep alone never bounds the size - an agent that records
    /// hundreds of MB a week would sit at its share, every write refused, for months.
    /// A refusal right after this pass waits out [TRIM_GAP_MS] like one after its own.
    pub async fn trim(&self) -> Trimmed {
        let trimmed = trim_store(&self.store, &self.keys).await;
        self.trim_gate.last_ms.store(swiss_core::util::now_ms() as i64, Ordering::Release);
        trimmed
    }

    /// A priced write refused over the share asks for a trim now, not at the next
    /// hourly pass: one in flight at a time, at most one per [TRIM_GAP_MS]. Outside a
    /// runtime (a sync test) there is nothing to run it on, and the hour still comes.
    fn trim_soon(&self) {
        let now = swiss_core::util::now_ms() as i64;
        if now - self.trim_gate.last_ms.load(Ordering::Acquire) < TRIM_GAP_MS {
            return;
        }
        if self.trim_gate.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.trim_gate.running.store(false, Ordering::Release);
            return;
        };
        let store = self.store.clone();
        let keys = self.keys.clone();
        let gate = self.trim_gate.clone();
        runtime.spawn(async move {
            trim_store(&store, &keys).await;
            gate.last_ms.store(swiss_core::util::now_ms() as i64, Ordering::Release);
            gate.running.store(false, Ordering::Release);
        });
    }

    /// Admit one priced write; a refusal over the share starts a trim (see
    /// [trim_soon](Self::trim_soon)) so the next writes land again.
    fn submit<F>(&self, estimated_bytes: usize, job: F) -> Result<(), HistoryWriteError>
    where
        F: FnOnce(SqlitePool) -> futures_util::future::BoxFuture<'static, Result<(), String>> + Send + 'static,
    {
        let submitted = self.store.submit(estimated_bytes, job);
        if let Err(HistoryWriteError::QuotaDenied { .. }) = &submitted {
            self.trim_soon();
        }
        submitted
    }

    /// What the module holds right now: (calls, runs, database bytes on disk).
    pub async fn usage(&self) -> (u64, u64, u64) {
        let calls = self.scalar("SELECT count(*) FROM call").await.unwrap_or(0);
        let runs = self.scalar("SELECT count(*) FROM run").await.unwrap_or(0);
        let bytes = std::fs::metadata(self.store.policy().db_file.clone())
            .map(|m| m.len())
            .unwrap_or(0);
        (calls, runs, bytes)
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
}
