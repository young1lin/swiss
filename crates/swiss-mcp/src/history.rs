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

//! The MCP module's own history database (SPEC `mcp.calls, `mcp.traffic): one
//! `logs/mcp.db` for every call and every traffic row of every hosted MCP — Calls and
//! Traffic share the MCP owner's database, resources add rows, never files or pools
//! (SPEC `host.history). Everything that made the jsonl era honest survives as wire
//! shape: the per-MCP sequence, the preview/body/bodyGone flags, renames, the metadata-
//! only traffic pages and the client memory that outlives a clear.
//!
//! The legacy jsonl tree (`logs/calls/*.jsonl`, `logs/calls/bodies/*`,
//! `logs/traffic.jsonl`) is an IMPORT SOURCE, inventoried and left in place — an
//! operator retires it explicitly after the cutover is verified; the importer never
//! deletes anything it has not proven redundant (SPEC `formats.logs).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::sqlite::SqlitePool;
use sqlx::{Row, SqliteConnection};

use swiss_host::history::body::{self, BodyCodec};
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{Durability, ModuleHistoryStore, OwnerPolicy};

use crate::calls::{CallEntry, CallRecord, CallSource};
use crate::traffic::TrafficEntry;

/// The MCP owner's schema, version 1. The version discipline is the host's
/// (`history::schema`): DDL and stamp commit in one transaction, and an unknown newer
/// version refuses the open with the store untouched.
static MCP_SCHEMA: &[SchemaStep] = &[SchemaStep::new(
    1,
    &[
        // Calls: the index row is the metadata + preview a page shows; the full reply
        // lives in call_body, chunked and codec-tagged. legacy_seq is the per-MCP wire
        // sequence the panel has always sorted by; id is the internal identity that
        // survives clears without reuse (SPEC `host.history).
        "CREATE TABLE call (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mcp TEXT NOT NULL,
            legacy_seq INTEGER NOT NULL,
            at TEXT NOT NULL,
            tool TEXT NOT NULL,
            via TEXT NOT NULL,
            client TEXT,
            ok INTEGER NOT NULL,
            ms INTEGER NOT NULL,
            args TEXT NOT NULL,
            preview_text TEXT NOT NULL,
            chars INTEGER NOT NULL,
            preview_flag INTEGER,
            body_flag INTEGER,
            UNIQUE (mcp, legacy_seq)
        )",
        "CREATE INDEX call_page ON call (mcp, id DESC)",
        "CREATE TABLE call_body (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            call_id INTEGER NOT NULL,
            part INTEGER NOT NULL,
            codec INTEGER NOT NULL,
            plain INTEGER NOT NULL,
            block BLOB NOT NULL,
            UNIQUE (call_id, part)
        )",
        // Traffic: every recorded interaction, not a bounded ring — the archive beyond
        // the live window is queryable with the same filters the ring had.
        "CREATE TABLE traffic (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            legacy_seq INTEGER NOT NULL UNIQUE,
            at TEXT NOT NULL,
            mcp TEXT NOT NULL,
            method TEXT NOT NULL,
            client TEXT,
            client_name TEXT,
            client_version TEXT,
            params TEXT,
            body TEXT NOT NULL,
            response TEXT,
            ok INTEGER NOT NULL,
            ms INTEGER NOT NULL,
            client_key TEXT NOT NULL
        )",
        "CREATE INDEX traffic_page ON traffic (id DESC)",
        "CREATE INDEX traffic_client ON traffic (client_key)",
        // The client memory: "who is which token" survives a clear of the rows, exactly
        // like known_client survived the ring's clear in the jsonl era.
        "CREATE TABLE client_dim (
            client_key TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            tokens TEXT NOT NULL,
            mcps TEXT NOT NULL,
            count INTEGER NOT NULL,
            last_at TEXT NOT NULL,
            last_seq INTEGER NOT NULL
        )",
    ],
)];

/// The client identity the panel groups rows by — the same rule as TrafficLog's
/// `client_key_of`; kept beside the column it feeds so the two cannot drift silently.
fn client_key_of(client: Option<&str>, client_name: Option<&str>) -> String {
    if let Some(name) = client_name {
        return format!("n:{name}");
    }
    if let Some(client) = client {
        return format!("t:{client}");
    }
    "?".into()
}

/// One module-owned history database for MCP. Cheap to construct the factory for, opened
/// only inside the plugin's enabled scope; the bounded writer queue, generations and the
/// engine gate are the host store's (SPEC `host.history).
pub struct McpHistory {
    store: Arc<ModuleHistoryStore>,
    /// The home's logs directory — the legacy tree lives under it, kept as the inventoried
    /// import source it now is.
    logs_dir: PathBuf,
    /// The meta keys this process has committed (import state, torn-tail accounting) —
    /// reads that must not cross the writer queue read the cached copy instead.
    meta_cache: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// The SCAN SNAPSHOT epoch, distinct from the store's worker-identity generation:
    /// a clear or sweep bumps it, so the scan cursors those deletes invalidate are
    /// refused by name instead of silently serving rows that no longer exist (Q4).
    /// Never bumps the store generation - that would retire the drain worker.
    snapshot: std::sync::atomic::AtomicU64,
    /// The chunk sealing keys (SPEC host.history): body parts seal at write and open
    /// at read; an empty candidate list is the documented degraded plain-block mode.
    keys: swiss_host::history::seal::ChunkKeys,
}

impl McpHistory {
    /// Open (and if needed import into) `<logs>/mcp.db`. The caller is the plugin's
    /// start — never a constructor — so a disabled MCP opens nothing at all.
    pub async fn open(logs_dir: &Path) -> Result<Arc<Self>, String> {
        // The module owns its database file, so it owns the seat too: a fresh home (or a
        // scratch home in tests) has no logs directory until the first open makes one.
        std::fs::create_dir_all(logs_dir).map_err(|e| format!("creating the logs directory: {e}"))?;
        let policy = OwnerPolicy {
            db_file: logs_dir.join("mcp.db"),
            queue_byte_bound: 16 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Normal,
            schema: MCP_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("mcp", policy);
        store
            .open()
            .await
            .map_err(|e| format!("opening the MCP history database: {e}"))?;
        // Enroll in the physical quota (SPEC host.history): the declared default
        // shares, no protected window - MCP bodies are convenience history, and the
        // honest over-share denial is the plain reason.
        store.set_quota(swiss_host::history::quota::QuotaPolicy::default(), false);
        let history = Arc::new(McpHistory {
            store,
            logs_dir: logs_dir.to_path_buf(),
            meta_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            snapshot: std::sync::atomic::AtomicU64::new(1),
            keys: swiss_host::history::seal::ChunkKeys::resolve(),
        });
        history
            .import_legacy()
            .await
            .map_err(|e| format!("importing the legacy jsonl tree: {e}"))?;
        Ok(history)
    }

    /// The store's state label — the lifecycle surfaces' honest status.
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

    /// The newest traffic sequence the archive holds — the ring's floor on attach, so
    /// rows recorded after the attach never renumber archived ones.
    pub async fn traffic_seq_floor(&self) -> u64 {
        self.scalar("SELECT COALESCE(MAX(legacy_seq), 0) FROM traffic")
            .await
            .unwrap_or(0)
    }

    /// Wait until every admitted write has completed: the read paths' visibility
    /// contract (a recorded call is visible to the very next read).
    pub async fn flush(&self) {
        let _ = self.store.flush_barrier().await;
    }

    /// The bounded close (withdraw, drain, release) — the plugin's stop.
    pub async fn close(&self) -> Result<(), String> {
        self.store.close().await.map_err(|e| e.to_string())
    }

    /// What the module holds right now: (calls, traffic rows, database bytes on disk,
    /// legacy import state). Diagnostics and tests — never a capacity claim (that is the
    /// retention phase's physical accounting).
    pub async fn usage(&self) -> (u64, u64, u64, String) {
        let calls = self.scalar("SELECT count(*) FROM call").await.unwrap_or(0);
        let traffic = self.scalar("SELECT count(*) FROM traffic").await.unwrap_or(0);
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
        (calls, traffic, bytes, state)
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

    // --- calls ---------------------------------------------------------------------------------

    /// Record one call. Admission is synchronous and bounded; the write itself is one
    /// queued transaction (row + body chunks), so a tool call never waits on disk and
    /// concurrent calls cannot claim the same sequence.
    pub fn record_call(&self, mcp: &str, source: &CallSource, rec: CallRecord) {
        let (entry, full_output, _long) = crate::calls::build_entry(source, rec);
        let mcp = mcp.to_string();
        let keys = self.keys.clone();
        let submitted = self.store.submit(
            entry.args.len() + entry.output.len() + full_output.len() + 256,
            move |pool| {
                Box::pin(async move {
                    insert_call(&pool, &mcp, &entry, &full_output, &keys)
                        .await
                        .map_err(|e| e.to_string())
                })
            },
        );
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db call admission", &e.to_string());
        }
    }

    /// One page of an MCP's calls, newest first — the wire shape `read_calls` always
    /// answered (calls/page/pageSize/more), now over the durable index.
    pub async fn read_calls(&self, mcp: &str, page: i64, page_size: i64, q: Option<&str>) -> Value {
        self.flush().await;
        let size = page_size.clamp(1, 100) as usize;
        let page = page.max(0) as usize;
        let from = page * size;
        let needle = q.unwrap_or("").trim().to_ascii_lowercase();
        let mcp = mcp.to_string();
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let (rows, total) = if needle.is_empty() {
                        let total = sqlx::query("SELECT count(*) FROM call WHERE mcp = ?")
                            .bind(&mcp)
                            .fetch_one(&pool)
                            .await?
                            .try_get::<i64, _>(0)? as usize;
                        let rows = sqlx::query(
                            "SELECT legacy_seq, at, tool, via, client, ok, ms, args, \
                             preview_text, chars, preview_flag, body_flag \
                             FROM call WHERE mcp = ? ORDER BY id DESC LIMIT ? OFFSET ?",
                        )
                        .bind(&mcp)
                        .bind(size as i64)
                        .bind(from as i64)
                        .fetch_all(&pool)
                        .await?;
                        (rows, total)
                    } else {
                        // The same match rule the jsonl scan had: tool, FULL arguments,
                        // stored reply text — ASCII-folded contains, never FTS grammar.
                        let rows = sqlx::query(
                            "SELECT legacy_seq, at, tool, via, client, ok, ms, args, \
                             preview_text, chars, preview_flag, body_flag \
                             FROM call WHERE mcp = ? AND ( \
                                 instr(lower(tool), ?) > 0 \
                              OR instr(lower(args), ?) > 0 \
                              OR instr(lower(preview_text), ?) > 0) \
                             ORDER BY id DESC",
                        )
                        .bind(&mcp)
                        .bind(&needle)
                        .bind(&needle)
                        .bind(&needle)
                        .fetch_all(&pool)
                        .await?;
                        let total = rows.len();
                        let rows = rows
                            .into_iter()
                            .skip(from)
                            .take(size)
                            .collect::<Vec<_>>();
                        (rows, total)
                    };
                    let calls: Vec<Value> = rows
                        .iter()
                        .map(call_row_to_value)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok::<Value, sqlx::Error>(json!({
                        "calls": calls,
                        "page": page,
                        "pageSize": size,
                        "more": total > from + size,
                    }))
                })
            })
            .await
            .unwrap_or_else(|_| {
                json!({ "calls": [], "page": page, "pageSize": size, "more": false })
            })
    }

    /// One call with its reply in full: the body chunks reassemble, a pruned body comes
    /// back flagged `bodyGone` — never a silently short result.
    pub async fn read_call(&self, mcp: &str, seq: u64) -> Option<CallEntry> {
        self.flush().await;
        let mcp_owned = mcp.to_string();
        let row = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query(
                            "SELECT id, legacy_seq, at, tool, via, client, ok, ms, args, \
                             preview_text, chars, preview_flag, body_flag \
                             FROM call WHERE mcp = ? AND legacy_seq = ?",
                        )
                        .bind(&mcp_owned)
                        .bind(seq as i64)
                        .fetch_optional(&pool)
                        .await
                })
            })
            .await
            .ok()??;
        let id: i64 = row.try_get("id").ok()?;
        let body_flag: Option<i64> = row.try_get("body_flag").ok()?;
        let mut entry = call_row_to_entry(&row).ok()?;
        if body_flag != Some(1) {
            return Some(entry);
        }
        let chunks = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT part, codec, plain, block FROM call_body WHERE call_id = ? ORDER BY part",
                    )
                    .bind(id)
                    .fetch_all(&pool)
                    .await?;
                    let chunks: Vec<(i64, i64, usize, Vec<u8>)> = rows
                        .into_iter()
                        .map(|r| {
                            let part: i64 = r.try_get(0)?;
                            let codec: i64 = r.try_get(1)?;
                            let plain: i64 = r.try_get(2)?;
                            let block: Vec<u8> = r.try_get(3)?;
                            Ok::<(i64, i64, usize, Vec<u8>), sqlx::Error>((
                                part,
                                codec,
                                plain as usize,
                                block,
                            ))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(chunks)
                })
            })
            .await
            .ok()
            .unwrap_or_default();
        // Parts join as RAW bytes first, then ONE decode: parts are cut on 64 KiB
        // byte boundaries and a multi-byte character can straddle the cut - decoding
        // each part alone would turn every straddle into two U+FFFD (the bug
        // swiss_core::utf8 was paid for once already).
        let mut raw: Vec<u8> = Vec::new();
        let mut ofs: u64 = 0;
        for (part, codec, declared, block) in chunks {
            // Unseal first (a plain block is a legacy or degraded row), decode second:
            // the binding pins the row, part and byte range this chunk must answer for.
            let binding = swiss_host::history::seal::ChunkBinding::row(
                "mcp", "", id, part, ofs, declared, codec as u8,
            );
            let coded = match self.keys.open_block(&block, &binding) {
                swiss_host::history::seal::OpenBlock::Sealed(bytes) => bytes,
                swiss_host::history::seal::OpenBlock::Plain => block,
                swiss_host::history::seal::OpenBlock::Refused => {
                    ofs += declared as u64;
                    continue;
                }
            };
            let decoded: Option<Vec<u8>> = BodyCodec::from_u8(codec as u8)
                .and_then(|c| body::decode_chunk(c, &coded, declared).ok());
            if let Some(bytes) = decoded {
                raw.extend_from_slice(&bytes);
                ofs += bytes.len() as u64;
            } else {
                ofs += declared as u64;
            }
        }
        if raw.is_empty() {
            entry.body_gone = Some(true);
            return Some(entry);
        }
        entry.output = String::from_utf8_lossy(&raw).into_owned();
        entry.preview = None;
        Some(entry)
    }

    /// The newest DISTINCT argument sets of one tool — the Run tab's refill dropdown.
    pub async fn read_tool_history(
        &self,
        mcp: &str,
        tool: &str,
        limit: usize,
        q: Option<&str>,
    ) -> Vec<Value> {
        self.flush().await;
        let size = limit.clamp(1, crate::calls::TOOL_HISTORY_MAX);
        let needle = q.unwrap_or("").trim().to_ascii_lowercase();
        let mcp = mcp.to_string();
        let tool = tool.to_string();
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT legacy_seq, at, via, client, ok, ms, args FROM call \
                         WHERE id IN (SELECT MAX(id) FROM call \
                             WHERE mcp = ? AND tool = ? AND instr(lower(args), ?) > 0 GROUP BY args) \
                         ORDER BY id DESC LIMIT ?",
                    )
                    .bind(&mcp)
                    .bind(&tool)
                    .bind(&needle)
                    .bind(size as i64)
                    .fetch_all(&pool)
                    .await?;
                    let mut out: Vec<Value> = Vec::new();
                    for row in rows {
                        let args: String = row.try_get("args")?;
                        let mut value = json!({
                            "seq": row.try_get::<i64, _>("legacy_seq")?,
                            "at": row.try_get::<String, _>("at")?,
                            "via": row.try_get::<String, _>("via")?,
                            "ok": row.try_get::<i64, _>("ok")? != 0,
                            "ms": row.try_get::<i64, _>("ms")?,
                            "args": crate::calls::CallLog::one_line(&args),
                        });
                        if let Some(client) = row.try_get::<Option<String>, _>("client")? {
                            value["client"] = json!(client);
                        }
                        out.push(value);
                    }
                    Ok::<Vec<Value>, sqlx::Error>(out)
                })
            })
            .await
            .unwrap_or_default()
    }

    /// The internal row ids of one MCP's calls, ascending — how tests observe the
    /// identity floor (a clear resets the WIRE sequence, never the internal ids).
    pub async fn internal_call_ids(&self, mcp: &str) -> Vec<i64> {
        let mcp = mcp.to_string();
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT id FROM call WHERE mcp = ? ORDER BY id")
                            .bind(&mcp)
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

    /// Drop one MCP's calls. The internal identity floor survives: new calls continue
    /// from internal ids this clear never reuses, while the per-MCP wire sequence starts
    /// over exactly as the jsonl clear always did.
    pub async fn clear_calls(&self, mcp: &str) {
        let mcp = mcp.to_string();
        let submitted = self.store.submit(64, move |pool| {
            Box::pin(async move {
                clear_calls_tx(&pool, &mcp)
                    .await
                    .map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db clear admission", &e.to_string());
        }
        self.flush().await;
        // The rows are gone: expire every live scan cursor over them (Q4).
        self.snapshot.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Move every call of one MCP to its new name — the rename the panel performs.
    pub async fn rename_calls(&self, from: &str, to: &str) {
        let from = from.to_string();
        let to = to.to_string();
        let submitted = self.store.submit(64, move |pool| {
            Box::pin(async move {
                rename_calls_tx(&pool, &from, &to)
                    .await
                    .map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db rename admission", &e.to_string());
        }
        self.flush().await;
    }

    /// One maintenance pass (SPEC host.history): the store's bounded checkpoint /
    /// VACUUM and ledger reconciliation, then the age sweep. The hourly retention
    /// timer calls this through the maintenance seat; it never opens anything.
    pub async fn maintain(&self, age_ms: i64) {
        // A failed maintain pass would be invisible forever, and the quota usage
        // snapshot it refreshes would go stale - the store contract says so out
        // loud, so the failure is said out loud too.
        if let Err(err) = self.store.maintain().await {
            swiss_core::log::warn("mcp history maintain failed", Some(json!({ "err": err.to_string() })));
        }
        self.sweep(age_ms).await;
    }

    /// Age-based retention over the durable index — the hourly sweep's new body. Byte
    /// budgets are physical accounting and land with the retention phase. Timestamps are
    /// PARSED for the comparison, the same rule the jsonl sweep_age used, rather than
    /// compared as text.
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
                            swiss_core::util::parse_iso_ms(at)
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
                let traffic: Vec<(i64, String)> = sqlx::query_as("SELECT id, at FROM traffic")
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
                for id in expired(traffic) {
                    sqlx::query("DELETE FROM traffic WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                tx.commit().await.map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db sweep admission", &e.to_string());
        }
        self.flush().await;
        // Swept rows are gone: expire every live scan cursor over them (Q4).
        self.snapshot.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    // --- traffic -------------------------------------------------------------------------------

    /// Record traffic rows (the ring stays the live window; this is the durable
    /// authority beside it). One queued transaction for the batch.
    pub fn record_traffic(&self, entries: Vec<TrafficEntry>) {
        if entries.is_empty() {
            return;
        }
        let estimate: usize = entries.iter().map(|e| e.body.len() + 512).sum();
        let submitted = self.store.submit(estimate, move |pool| {
            Box::pin(async move {
                insert_traffic(&pool, &entries)
                    .await
                    .map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db traffic admission", &e.to_string());
        }
    }

    /// One newest-first page of interactions over the ARCHIVE, with the ring's exact
    /// wire shape — rows carry no body/response (each is capped at 8 KB; the panel
    /// fetches one row's payload on expand).
    pub async fn read_traffic(&self, q: &crate::traffic::TrafficQuery<'_>) -> Value {
        // The read-visibility contract, same as every other read: a query racing a
        // fresh record must see the row, not a phantom empty page.
        self.flush().await;
        let page_size = q.page_size.unwrap_or(crate::traffic::PAGE_SIZE).clamp(1, 500);
        let mcp = q.mcp.map(str::to_string);
        let client = q.client.map(str::to_string);
        let method = q.method.map(str::to_string);
        let actions_only = q.actions_only;
        let page = q.page;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    // One static predicate shape, filtered by NULL-able binds: sqlx only
                    // takes audited literals, and the action-method list is a fixed set
                    // spelled out so no string is ever assembled at runtime.
                    const FILTERED_COUNT: &str = "SELECT count(*) FROM traffic \
                         WHERE (?1 IS NULL OR mcp = ?1) \
                           AND (?2 IS NULL OR client_key = ?2) \
                           AND (?3 IS NULL OR method = ?3) \
                           AND (?4 = 0 OR method IN ('tools/call', 'resources/read', \
                               'resources/subscribe', 'resources/unsubscribe', \
                               'prompts/get', 'completion/complete', 'logging/setLevel'))";
                    const FILTERED_PAGE: &str = "SELECT legacy_seq, at, mcp, method, client, \
                         client_name, client_version, params, body, response, ok, ms \
                         FROM traffic \
                         WHERE (?1 IS NULL OR mcp = ?1) \
                           AND (?2 IS NULL OR client_key = ?2) \
                           AND (?3 IS NULL OR method = ?3) \
                           AND (?4 = 0 OR method IN ('tools/call', 'resources/read', \
                               'resources/subscribe', 'resources/unsubscribe', \
                               'prompts/get', 'completion/complete', 'logging/setLevel')) \
                         ORDER BY id DESC LIMIT ?5 OFFSET ?6";
                    let actions = if actions_only { 1i64 } else { 0i64 };
                    let total: usize = sqlx::query_scalar::<_, i64>(FILTERED_COUNT)
                        .bind(&mcp)
                        .bind(&client)
                        .bind(&method)
                        .bind(actions)
                        .fetch_one(&pool)
                        .await? as usize;
                    let unfiltered: usize =
                        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic")
                            .fetch_one(&pool)
                            .await? as usize;
                    let rows = sqlx::query(FILTERED_PAGE)
                        .bind(&mcp)
                        .bind(&client)
                        .bind(&method)
                        .bind(actions)
                        .bind(page_size as i64)
                        .bind((page * page_size) as i64)
                        .fetch_all(&pool)
                        .await?;
                    let entries: Vec<Value> = rows
                        .iter()
                        .map(traffic_row_to_list_value)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok::<Value, sqlx::Error>(json!({
                        "entries": entries,
                        "total": total,
                        "totalUnfiltered": unfiltered,
                        "page": page,
                        "pageSize": page_size,
                        "more": total > (page + 1) * page_size,
                    }))
                })
            })
            .await
            .unwrap_or_else(|_| {
                json!({ "entries": [], "total": 0, "totalUnfiltered": 0, "page": page, "pageSize": page_size, "more": false })
            })
    }

    /// One traffic row's raw request and reply — the expand fetch.
    pub async fn read_traffic_entry(&self, seq: u64) -> Option<Value> {
        self.flush().await;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let row =
                        sqlx::query("SELECT body, response FROM traffic WHERE legacy_seq = ?")
                            .bind(seq as i64)
                            .fetch_optional(&pool)
                            .await?;
                    Ok::<Option<Value>, sqlx::Error>(row.map(|row| {
                        let body: String = row.try_get(0).unwrap_or_default();
                        let mut out = json!({ "body": body });
                        if let Ok(Some(response)) = row.try_get::<Option<String>, _>(1) {
                            out["response"] = json!(response);
                        }
                        out
                    }))
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// Every client the archive has seen, folded to one row each — computed from the
    /// client dimension, which a clear of the rows never touches.
    pub async fn traffic_clients(&self) -> Vec<Value> {
        self.flush().await;
        self.store
            .query(|pool| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT client_key, label, tokens, mcps, count, last_at, last_seq \
                         FROM client_dim ORDER BY last_seq DESC",
                    )
                    .fetch_all(&pool)
                    .await?;
                    rows.iter()
                        .map(|row| {
                            let tokens: String = row.try_get(2)?;
                            let mcps: String = row.try_get(3)?;
                            Ok(json!({
                                "key": row.try_get::<String, _>(0)?,
                                "label": row.try_get::<String, _>(1)?,
                                "tokens": serde_json::from_str::<Value>(&tokens)
                                    .unwrap_or_else(|_| json!([])),
                                "mcps": serde_json::from_str::<Value>(&mcps)
                                    .unwrap_or_else(|_| json!([])),
                                "count": row.try_get::<i64, _>(4)?,
                                "lastAt": row.try_get::<String, _>(5)?,
                                "lastSeq": row.try_get::<i64, _>(6)?,
                            }))
                        })
                        .collect()
                })
            })
            .await
            .unwrap_or_default()
    }

    /// Drop recorded interactions — one client's rows, or everything. The client
    /// dimension survives either way: Clear wipes the log, not the gateway's memory of
    /// which token is which client.
    pub async fn clear_traffic(&self, client_key: Option<&str>) {
        let key = client_key.map(str::to_string);
        let submitted = self.store.submit(64, move |pool| {
            Box::pin(async move {
                clear_traffic_tx(&pool, key.as_deref())
                    .await
                    .map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            crate::calls::on_error("mcp.db traffic clear admission", &e.to_string());
        }
        self.flush().await;
        // The rows are gone: expire every live scan cursor over them (Q4).
        self.snapshot.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    // --- legacy import -------------------------------------------------------------------------

    /// Import the jsonl tree once, idempotently, and never delete it. Each file commits
    /// as one batch; a crash mid-import resumes safely (every row carries the natural
    /// key the OR IGNORE dedupes on) and the sources stay exactly where they were.
    pub async fn import_legacy(self: &Arc<Self>) -> Result<(), String> {
        let state = self
            .store
            .query(|pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT value FROM meta WHERE key = 'legacy_import'")
                        .fetch_optional(&pool)
                        .await?;
                    row.map(|r| r.try_get::<String, _>(0)).transpose()
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        if state.as_deref() == Some("done") {
            self.meta_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("legacy_import".into(), "done".into());
            return Ok(());
        }
        self.set_meta_queued("legacy_import", "active").await;

        let calls_dir = self.logs_dir.join("calls");
        let mut torn = 0u64;
        // Any admission refusal leaves the import at "active": the next open retries
        // the refused batches instead of losing them behind a done stamp.
        let mut refused = false;
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&calls_dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".jsonl") {
                    files.push(entry.path());
                }
            }
        }
        files.sort();
        for file in files {
            let mcp = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(tail) = crate::calls::tail_lines(&file, usize::MAX).ok() else {
                continue;
            };
            let mut entries = Vec::new();
            for line in &tail.lines {
                match serde_json::from_str::<CallEntry>(line) {
                    Ok(e) => entries.push(e),
                    Err(_) => torn += 1,
                }
            }
            if tail.more {
                torn += 1;
            }
            // tail_lines yields newest first; insert OLDEST first so internal ids follow
            // the same chronology the jsonl sequence did (pages are id-ordered).
            entries.reverse();
            let bodies_dir = calls_dir.join("bodies").join(&mcp);
            let bodies: Vec<(u64, String)> = entries
                .iter()
                .filter(|e| e.body == Some(true))
                .filter_map(|e| {
                    std::fs::read_to_string(bodies_dir.join(format!("{}.txt", e.seq)))
                        .ok()
                        .map(|text| (e.seq, text))
                })
                .collect();
            let estimate: usize = entries
                .iter()
                .map(|e| e.args.len() + e.output.len() + 256)
                .sum::<usize>()
                + bodies.iter().map(|(_, t)| t.len()).sum::<usize>();
            let done = Arc::new(tokio::sync::Notify::new());
            let notified = done.notified();
            let done_for_job = done.clone();
            let mcp_name = mcp.clone();
            let keys = self.keys.clone();
            // The job's own outcome rides out through here: an ADMITTED write that
            // then failed at execution left nothing in the database either, and the
            // one-shot "done" stamp must not cover it (the same lost-forever class
            // as a refused admission, one step later).
            let failure = Arc::new(std::sync::Mutex::new(None::<String>));
            let failure_for_job = failure.clone();
            let submitted = self.store.submit(estimate, move |pool| {
                Box::pin(async move {
                    let result = import_calls_batch(&pool, &mcp_name, &entries, &bodies, &keys).await;
                    if let Err(e) = &result {
                        *failure_for_job.lock().unwrap() = Some(e.to_string());
                    }
                    done_for_job.notify_waiters();
                    result.map_err(|e| e.to_string())
                })
            });
            match submitted {
                Ok(()) => {
                    // A close racing the import can drop the job without ever
                    // firing the notify (the worker's Dropped arm), and a panicking
                    // job kills its own wake: the wait is bounded, and an elapsed
                    // deadline is a refusal - the meta stays active and the next
                    // open retries, never a hang.
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(35),
                        notified,
                    )
                    .await
                    .is_err()
                    {
                        refused = true;
                        crate::calls::on_error(
                            "mcp.db legacy import execution",
                            "the import write did not finish in time",
                        );
                    }
                    if let Some(err) = failure.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        refused = true;
                        crate::calls::on_error("mcp.db legacy import execution", &err);
                    }
                }
                Err(e) => {
                    // A refused admission must NOT be folded into "done": the rows are
                    // still jsonl-only, and the one-shot stamp would make them invisible
                    // forever. The meta stays "active" so the next open retries.
                    refused = true;
                    crate::calls::on_error("mcp.db legacy import admission", &e.to_string());
                }
            }
        }

        let traffic_file = self.logs_dir.join("traffic.jsonl");
        if let Ok(tail) = crate::calls::tail_lines(&traffic_file, usize::MAX) {
            let mut entries = Vec::new();
            for line in &tail.lines {
                match serde_json::from_str::<TrafficEntry>(line) {
                    Ok(e) => entries.push(e),
                    Err(_) => torn += 1,
                }
            }
            if tail.more {
                torn += 1;
            }
            // Newest-first tail, oldest-first insertion: id order matches seq order.
            entries.reverse();
            if !entries.is_empty() {
                let estimate: usize = entries.iter().map(|e| e.body.len() + 512).sum();
                let done = Arc::new(tokio::sync::Notify::new());
                let notified = done.notified();
                let done_for_job = done.clone();
                let failure = Arc::new(std::sync::Mutex::new(None::<String>));
                let failure_for_job = failure.clone();
                let submitted = self.store.submit(estimate, move |pool| {
                    Box::pin(async move {
                        let result = insert_traffic(&pool, &entries).await;
                        if let Err(e) = &result {
                            *failure_for_job.lock().unwrap() = Some(e.to_string());
                        }
                        done_for_job.notify_waiters();
                        result.map_err(|e| e.to_string())
                    })
                });
                match submitted {
                    Ok(()) => {
                        // The same bounded wait as the calls batch: no hang on a
                        // raced close or a dead worker, a refusal instead.
                        if tokio::time::timeout(
                            std::time::Duration::from_secs(35),
                            notified,
                        )
                        .await
                        .is_err()
                        {
                            refused = true;
                            crate::calls::on_error(
                                "mcp.db traffic import execution",
                                "the import write did not finish in time",
                            );
                        }
                        if let Some(err) = failure.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            refused = true;
                            crate::calls::on_error("mcp.db traffic import execution", &err);
                        }
                    }
                    Err(e) => {
                        refused = true;
                        crate::calls::on_error("mcp.db traffic import admission", &e.to_string());
                    }
                }
            }
        }

        if refused {
            // The stamp stays "active": what was refused is retried on the next open,
            // never silently skipped as if it had been imported.
            return Ok(());
        }
        self.set_meta_queued("legacy_import", "done").await;
        if torn > 0 {
            let torn_str = torn.to_string();
            self.set_meta_queued("legacy_torn_tail", &torn_str).await;
        }
        Ok(())
    }

    async fn set_meta_queued(&self, key: &str, value: &str) {
        let key = key.to_string();
        let value = value.to_string();
        let done = Arc::new(tokio::sync::Notify::new());
        let notified = done.notified();
        let done_for_job = done.clone();
        let key_for_job = key.clone();
        let value_for_job = value.clone();
        let submitted = self.store.submit(64, move |pool| {
            Box::pin(async move {
                let result = sqlx::query(
                    "INSERT INTO meta (key, value) VALUES (?, ?) \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                )
                .bind(&key_for_job)
                .bind(&value_for_job)
                .execute(&pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
                done_for_job.notify_waiters();
                result
            })
        });
        if submitted.is_ok() {
            notified.await;
            self.meta_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, value);
        }
    }
}

// --- transaction bodies ------------------------------------------------------------------------

async fn insert_call(
    pool: &SqlitePool,
    mcp: &str,
    entry: &CallEntry,
    full_output: &str,
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let next: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(legacy_seq), 0) + 1 FROM call WHERE mcp = ?")
            .bind(mcp)
            .fetch_one(&mut *tx)
            .await?;
    let result = sqlx::query(
        "INSERT INTO call (mcp, legacy_seq, at, tool, via, client, ok, ms, args, \
         preview_text, chars, preview_flag, body_flag) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(mcp)
    .bind(next)
    .bind(&entry.at)
    .bind(&entry.tool)
    .bind(&entry.via)
    .bind(&entry.client)
    .bind(entry.ok)
    .bind(entry.ms as i64)
    .bind(&entry.args)
    .bind(&entry.output)
    .bind(entry.chars as i64)
    .bind(entry.preview.map(|_| 1i64))
    .bind(entry.body.map(|_| 1i64))
    .execute(&mut *tx)
    .await?;
    let call_id = result.last_insert_rowid();
    if entry.body == Some(true) {
        let mut ofs = 0u64;
        for (part, chunk_bytes) in body::chunk(full_output.as_bytes()).enumerate() {
            let plain = chunk_bytes.len();
            let (codec, coded) = body::encode_chunk(chunk_bytes);
            // Sealing is the LAST step (SPEC host.history): the coded bytes enter the
            // envelope, the envelope enters the row, and the row identity rides inside
            // the authenticated plaintext so a moved envelope cannot serve another row.
            // resource stays "": the read paths know the call id but not the mcp
            // name, and event+part+range+codec already pin the row's identity.
            let binding = swiss_host::history::seal::ChunkBinding::row(
                "mcp",
                "",
                call_id,
                part as i64,
                ofs,
                plain,
                codec.as_u8(),
            );
            let block = keys.seal_block("history:mcp:call_body", &binding, &coded);
            ofs += plain as u64;
            sqlx::query(
                "INSERT INTO call_body (call_id, part, codec, plain, block) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(call_id)
            .bind(part as i64)
            .bind(codec as u8 as i64)
            .bind(plain as i64)
            .bind(&block)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await
}

async fn clear_calls_tx(pool: &SqlitePool, mcp: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM call_body WHERE call_id IN (SELECT id FROM call WHERE mcp = ?)")
        .bind(mcp)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM call WHERE mcp = ?")
        .bind(mcp)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

async fn rename_calls_tx(pool: &SqlitePool, from: &str, to: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE call SET mcp = ? WHERE mcp = ?")
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

async fn insert_traffic(pool: &SqlitePool, entries: &[TrafficEntry]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for e in entries {
        let key = client_key_of(e.client.as_deref(), e.client_name.as_deref());
        let result = sqlx::query(
            "INSERT OR IGNORE INTO traffic (legacy_seq, at, mcp, method, client, \
             client_name, client_version, params, body, response, ok, ms, client_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.seq as i64)
        .bind(&e.at)
        .bind(&e.mcp)
        .bind(&e.method)
        .bind(&e.client)
        .bind(&e.client_name)
        .bind(&e.client_version)
        .bind(&e.params)
        .bind(&e.body)
        .bind(&e.response)
        .bind(e.ok)
        .bind(e.ms as i64)
        .bind(&key)
        .execute(&mut *tx)
        .await?;
        // Only a row that really inserted moves the count: an ignored duplicate
        // (crash-resume re-import) must not re-increment the client's tally.
        let inserted = result.rows_affected() > 0;
        upsert_client_dim(&mut tx, e, &key, inserted).await?;
    }
    tx.commit().await
}

async fn upsert_client_dim(
    tx: &mut SqliteConnection,
    e: &TrafficEntry,
    key: &str,
    inserted: bool,
) -> Result<(), sqlx::Error> {
    let label = if let Some(name) = &e.client_name {
        match &e.client_version {
            Some(v) => format!("{name} {v}"),
            None => name.clone(),
        }
    } else if let Some(client) = &e.client {
        format!("token {client}")
    } else {
        "unknown client".into()
    };
    let existing = sqlx::query(
        "SELECT label, tokens, mcps, count, last_at, last_seq FROM client_dim WHERE client_key = ?",
    )
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    let (mut best_label, tokens, mcps, mut count, mut last_at, mut last_seq) = match existing {
        Some(row) => (
            row.try_get::<String, _>(0)?,
            row.try_get::<String, _>(1)?,
            row.try_get::<String, _>(2)?,
            row.try_get::<i64, _>(3)?,
            row.try_get::<String, _>(4)?,
            row.try_get::<i64, _>(5)?,
        ),
        None => (label.clone(), "[]".into(), "[]".into(), 0, e.at.clone(), 0),
    };
    // The label can improve mid-stream: frames before the handshake land under
    // "token X", the handshake then names the client — take the better one.
    if e.client_name.is_some() {
        best_label = label;
    }
    let mut token_set: std::collections::BTreeSet<String> =
        serde_json::from_str(&tokens).unwrap_or_default();
    if let Some(client) = &e.client {
        token_set.insert(client.clone());
    }
    let mut mcp_set: std::collections::BTreeSet<String> =
        serde_json::from_str(&mcps).unwrap_or_default();
    mcp_set.insert(e.mcp.clone());
    if inserted {
        count += 1;
    }
    if (e.seq as i64) > last_seq {
        last_seq = e.seq as i64;
        last_at = e.at.clone();
    }
    sqlx::query(
        "INSERT INTO client_dim (client_key, label, tokens, mcps, count, last_at, last_seq) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (client_key) DO UPDATE SET \
           label = excluded.label, tokens = excluded.tokens, mcps = excluded.mcps, \
           count = excluded.count, last_at = excluded.last_at, last_seq = excluded.last_seq",
    )
    .bind(key)
    .bind(&best_label)
    .bind(serde_json::to_string(&token_set).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&mcp_set).unwrap_or_else(|_| "[]".into()))
    .bind(count)
    .bind(&last_at)
    .bind(last_seq)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn clear_traffic_tx(pool: &SqlitePool, client_key: Option<&str>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    match client_key {
        Some(key) => {
            sqlx::query("DELETE FROM traffic WHERE client_key = ?")
                .bind(key)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM traffic").execute(&mut *tx).await?;
        }
    }
    tx.commit().await
}

async fn import_calls_batch(
    pool: &SqlitePool,
    mcp: &str,
    entries: &[CallEntry],
    bodies: &[(u64, String)],
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for e in entries {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO call (mcp, legacy_seq, at, tool, via, client, ok, ms, \
             args, preview_text, chars, preview_flag, body_flag) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(mcp)
        .bind(e.seq as i64)
        .bind(&e.at)
        .bind(&e.tool)
        .bind(&e.via)
        .bind(&e.client)
        .bind(e.ok)
        .bind(e.ms as i64)
        .bind(&e.args)
        .bind(&e.output)
        .bind(e.chars as i64)
        .bind(e.preview.map(|_| 1i64))
        .bind(e.body.map(|_| 1i64))
        .execute(&mut *tx)
        .await?;
        // last_insert_rowid is only the new row's id when the insert really inserted:
        // an ignored duplicate leaves a STALE rowid (whatever this connection inserted
        // last - often another call's body part), and binding parts to it orphans them
        // on every crash-resume re-import. A duplicate call row already carries its
        // first-import body (row and parts share one transaction), so parts insert ONLY
        // with a fresh row: first writer wins, byte-exact, never double-bound.
        if result.rows_affected() == 0 {
            continue;
        }
        let call_id = result.last_insert_rowid();
        if let Some((_, text)) = bodies.iter().find(|(seq, _)| *seq == e.seq) {
            let mut ofs = 0u64;
            for (part, chunk_bytes) in body::chunk(text.as_bytes()).enumerate() {
                let plain = chunk_bytes.len();
                let (codec, coded) = body::encode_chunk(chunk_bytes);
                let binding = swiss_host::history::seal::ChunkBinding::row(
                    "mcp",
                    "",
                    call_id,
                    part as i64,
                    ofs,
                    plain,
                    codec.as_u8(),
                );
                let block = keys.seal_block("history:mcp:call_body", &binding, &coded);
                ofs += plain as u64;
                sqlx::query(
                    "INSERT OR IGNORE INTO call_body (call_id, part, codec, plain, block) \
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(call_id)
                .bind(part as i64)
                .bind(codec as u8 as i64)
                .bind(plain as i64)
                .bind(&block)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await
}

// --- row mapping --------------------------------------------------------------------------------

fn call_row_to_value(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    Ok(serde_json::to_value(call_row_to_entry(row)?).unwrap_or(Value::Null))
}

fn call_row_to_entry(row: &sqlx::sqlite::SqliteRow) -> Result<CallEntry, sqlx::Error> {
    Ok(CallEntry {
        seq: row.try_get::<i64, _>("legacy_seq")? as u64,
        at: row.try_get("at")?,
        tool: row.try_get("tool")?,
        via: row.try_get("via")?,
        client: row.try_get("client")?,
        ok: row.try_get::<i64, _>("ok")? != 0,
        ms: row.try_get::<i64, _>("ms")? as u64,
        args: row.try_get("args")?,
        output: row.try_get("preview_text")?,
        chars: row.try_get::<i64, _>("chars")? as usize,
        preview: row.try_get::<Option<i64>, _>("preview_flag")?.map(|v| v != 0),
        body: row.try_get::<Option<i64>, _>("body_flag")?.map(|v| v != 0),
        body_gone: None,
    })
}

fn traffic_row_to_list_value(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    let entry = TrafficEntry {
        seq: row.try_get::<i64, _>(0)? as u64,
        at: row.try_get(1)?,
        mcp: row.try_get(2)?,
        method: row.try_get(3)?,
        client: row.try_get(4)?,
        client_name: row.try_get(5)?,
        client_version: row.try_get(6)?,
        params: row.try_get(7)?,
        body: row.try_get(8)?,
        response: row.try_get(9)?,
        ok: row.try_get::<i64, _>(10)? != 0,
        ms: row.try_get::<i64, _>(11)? as u64,
    };
    let mut out = serde_json::to_value(&entry).unwrap_or(Value::Null);
    let has_response = out.get("response").is_some();
    if let Some(o) = out.as_object_mut() {
        o.remove("body");
        o.remove("response");
    }
    out["hasResponse"] = json!(has_response);
    Ok(out)
}

/// The scan source over this database's call bodies (SPEC §host.history). Parts are
/// fixed 64 KiB slices, so a part index IS a byte offset divided by 65 536: the
/// stream reports virtual byte offsets, and boundary-straddling hits across parts
/// are found exactly like any other chunk join.
impl swiss_host::history::search::BodySource for McpHistory {
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<swiss_host::history::search::StoredChunk>, String>> + Send + '_>> {
        let store = self.store.clone();
        Box::pin(async move {
            // The bounded writer queue commits before the scan reads: a query racing
            // a fresh record must see the chunk, not a phantom empty stream.
            let _ = store.flush_barrier().await;
            store
                .query::<Vec<swiss_host::history::search::StoredChunk>, _>(move |pool| {
                    Box::pin(async move {
                        // The stream speaks virtual byte offsets (ofs = part * 65_536):
                        // compare in those units so a resume anchored at ANY part - not
                        // just part 0 - still returns that call's later parts. The
                        // carry-rebuild call passes the anchor chunk's own start (the
                        // anchor is included); the batch call passes start + 1 (strictly
                        // after it). Both stay correct because every part boundary is a
                        // multiple of 65_536.
                        sqlx::query("SELECT call_id, part, codec, plain, block FROM call_body WHERE (call_id > ?1 OR (call_id = ?1 AND part * 65_536 >= ?2)) ORDER BY call_id, part LIMIT ?3")
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
                                            ofs: part * 65_536,
                                            part,
                                            codec: r.try_get::<i64, _>(2)?,
                                            plain: r.try_get::<i64, _>(3)?,
                                            block: r.try_get::<Vec<u8>, _>(4)?,
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
            "mcp",
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

/// The body reader for the cross-module endpoint (SPEC §host.history): one call's
/// stored reply by internal call id, joined across parts, byte-cursored and
/// bounded. Pruned bodies say gone, never a silently short text.
pub struct McpBodyReader {
    history: std::sync::Arc<McpHistory>,
}

impl McpBodyReader {
    pub fn new(history: std::sync::Arc<McpHistory>) -> Self {
        McpBodyReader { history }
    }
}

#[async_trait::async_trait]
impl swiss_host::services::history_query::HistoryBodyReader for McpBodyReader {
    async fn read_body(
        &self,
        id: i64,
        after: u64,
        max: usize,
    ) -> Result<swiss_host::services::history_query::BodySlice, String> {
        self.history.read_call_body(id, after, max).await
    }
}

impl McpHistory {
    /// One call's stored reply by internal id: parts joined in order, decoded,
    /// served after the byte cursor up to max bytes with the next frontier.
    pub async fn read_call_body(
        &self,
        id: i64,
        after: u64,
        max: usize,
    ) -> Result<swiss_host::services::history_query::BodySlice, String> {
        let _ = self.store.flush_barrier().await;
        let id2 = id;
        let keys = self.keys.clone();
        self.store
            .query::<Option<swiss_host::services::history_query::BodySlice>, _>(move |pool| {
                Box::pin(async move {
                    let exists = sqlx::query("SELECT body_flag FROM call WHERE id = ?")
                        .bind(id2)
                        .fetch_optional(&pool)
                        .await?;
                    let Some(flag) = exists else {
                        return Ok(None);
                    };
                    if flag.try_get::<Option<i64>, _>("body_flag")? != Some(1) {
                        // The reply never crossed the preview line: there is no
                        // stored body, and that is gone in the endpoint's terms.
                        return Ok(Some(swiss_host::services::history_query::BodySlice {
                            text: String::new(),
                            next: None,
                            gone: true,
                        }));
                    }
                    let rows = sqlx::query(
                        "SELECT part, codec, plain, block FROM call_body WHERE call_id = ? ORDER BY part",
                    )
                    .bind(id2)
                    .fetch_all(&pool)
                    .await?;
                    // Raw bytes first, ONE decode last, and the page edge never splits
                    // a character: the cursor advances by RAW bytes (a lossy-inflated
                    // length would desync every later page), and any trailing partial
                    // sequence is trimmed back to a boundary so its bytes return on the
                    // next page - the same shape the remote store's output() uses.
                    let mut buf: Vec<u8> = Vec::new();
                    // ofs mirrors the WRITE-time offset: every part contributes its
                    // declared plain length whether it opens or decodes or not, while
                    // buf accumulates only served bytes. The seal binding is built
                    // from ofs - building it from buf.len() would let one refused
                    // part cascade into refusing every later part of the body.
                    let mut ofs: u64 = 0;
                    for row in rows {
                        let part = row.try_get::<i64, _>(0)?;
                        let codec = row.try_get::<i64, _>(1)?;
                        let plain = row.try_get::<i64, _>(2)? as usize;
                        let block = row.try_get::<Vec<u8>, _>(3)?;
                        let binding = swiss_host::history::seal::ChunkBinding::row(
                            "mcp", "", id2, part, ofs, plain, codec as u8,
                        );
                        ofs += plain as u64;
                        let coded = match keys.open_block(&block, &binding) {
                            swiss_host::history::seal::OpenBlock::Sealed(bytes) => bytes,
                            swiss_host::history::seal::OpenBlock::Plain => block,
                            swiss_host::history::seal::OpenBlock::Refused => {
                                continue;
                            }
                        };
                        let decoded = swiss_host::history::body::BodyCodec::from_u8(codec as u8)
                            .and_then(|c| swiss_host::history::body::decode_chunk(c, &coded, plain).ok())
                            .unwrap_or_default();
                        buf.extend_from_slice(&decoded);
                        if buf.len() as u64 >= after + max as u64 {
                            break;
                        }
                    }
                    if after >= buf.len() as u64 {
                        return Ok(Some(swiss_host::services::history_query::BodySlice {
                            text: String::new(),
                            next: None,
                            gone: false,
                        }));
                    }
                    let end = (after as usize + max).min(buf.len());
                    // A hand-set cursor can sit inside a multi-byte character: the
                    // leading partial bytes are unservable, so skip to the next
                    // character boundary and serve from there - never drip
                    // replacement characters one byte at a time (the round-3
                    // audit's mid-character cursor).
                    let mut start = after as usize;
                    if start < end && (buf[start] & 0xC0) == 0x80 {
                        start += 1;
                        while start < buf.len() && (buf[start] & 0xC0) == 0x80 {
                            start += 1;
                        }
                    }
                    let window = &buf[start..end.max(start)];
                    let keep = swiss_core::utf8::char_boundary_end(window);
                    let raw = &window[..keep];
                    let text = String::from_utf8_lossy(raw).into_owned();
                    let next = if start + keep < buf.len() {
                        Some(start as u64 + keep as u64)
                    } else {
                        None
                    };
                    Ok(Some(swiss_host::services::history_query::BodySlice { text, next, gone: false }))
                })
            })
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no call {id}"))
    }
}
