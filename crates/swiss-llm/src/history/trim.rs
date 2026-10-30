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

//! The size trim: drop the oldest history while the live pages pass the llm share's goal,
//! sweep the pieces left orphaned, and compact.

use super::*;

/// One size-trim pass (see [LlmHistory::trim]): measure, drop the oldest rounds of
/// history while the live pages are over the goal, sweep the pieces left orphaned,
/// compact, and refresh the store's usage snapshot so admission sees the new size.
/// Every write rides submit_exempt: a quota that refused its own trim could never
/// reclaim the bytes it is policing.
pub(super) async fn trim_store(store: &Arc<ModuleHistoryStore>, keys: &swiss_host::history::seal::ChunkKeys) -> Trimmed {
    let mut trimmed = Trimmed::default();
    let Some(share) = store.share().filter(|share| *share > 0) else { return trimmed };
    let start = share / 100 * TRIM_START_PCT;
    let goal = share / 100 * TRIM_GOAL_PCT;
    let on_disk = swiss_host::history::quota::measure(&store.policy().db_file).total();
    let Ok(mut live) = live_bytes(store).await else { return trimmed };
    if on_disk.max(live) <= start {
        return trimmed;
    }
    for _ in 0..TRIM_ROUNDS {
        if live <= goal {
            break;
        }
        let fraction = ((live - goal) as f64 / live as f64).max(TRIM_MIN_FRACTION);
        match drop_oldest(store, fraction).await {
            Ok((0, 0)) => break,
            Ok((calls, runs)) => {
                trimmed.calls += calls;
                trimmed.runs += runs;
            }
            Err(err) => {
                swiss_core::log::warn(&format!("llm.db trim failed: {err}"), None);
                break;
            }
        }
        live = match live_bytes(store).await {
            Ok(live) => live,
            Err(_) => break,
        };
    }
    if trimmed.calls + trimmed.runs == 0 && on_disk <= live + share / 10 {
        // Nothing could go - what is left rides open conversations - and the file holds
        // no free pages worth a rewrite: a sweep and a VACUUM now would buy nothing, and
        // the next refusal would pay for them again.
        return trimmed;
    }
    match sweep_pieces(store, keys).await {
        Ok(pieces) => trimmed.pieces = pieces,
        Err(err) => swiss_core::log::warn(&format!("llm.db piece sweep skipped: {err}"), None),
    }
    compact(store).await;
    if let Err(err) = store.maintain().await {
        swiss_core::log::warn(&format!("llm.db maintain after the trim failed: {err}"), None);
    }
    swiss_core::log::log(
        "info",
        "llm.db trimmed",
        Some(json!({
            "share": share,
            "before": on_disk,
            "after": swiss_host::history::quota::measure(&store.policy().db_file).total(),
            "calls": trimmed.calls,
            "runs": trimmed.runs,
            "pieces": trimmed.pieces,
        })),
    );
    trimmed
}

/// The bytes the database's pages hold, free pages left out: what a VACUUM would
/// shrink the file to.
pub(super) async fn live_bytes(store: &ModuleHistoryStore) -> Result<u64, String> {
    store
        .query(|pool| {
            Box::pin(async move {
                let pragma = |sql: &'static str| {
                    let pool = pool.clone();
                    async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await }
                };
                let size = pragma("PRAGMA page_size").await?;
                let count = pragma("PRAGMA page_count").await?;
                let free = pragma("PRAGMA freelist_count").await?;
                Ok::<u64, sqlx::Error>(((count - free).max(0) * size.max(0)) as u64)
            })
        })
        .await
        .map_err(|e| e.to_string())
}

/// Run one exempt write job and hand back what it answered.
pub(super) async fn exempt<T, F>(store: &ModuleHistoryStore, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(SqlitePool) -> futures_util::future::BoxFuture<'static, Result<T, String>> + Send + 'static,
{
    let (reply, receipt) = tokio::sync::oneshot::channel::<Result<T, String>>();
    store
        .submit_exempt(64, move |pool| {
            Box::pin(async move {
                let answer = job(pool).await;
                let failed = answer.as_ref().err().cloned();
                let _ = reply.send(answer);
                failed.map_or(Ok(()), Err)
            })
        })
        .map_err(|e| e.to_string())?;
    receipt.await.map_err(|_| "the trim job did not finish".to_string())?
}

/// One trim round: the oldest `fraction` of the droppable history goes, calls and
/// finished runs on one timeline, oldest first - (calls, runs) dropped. A run still
/// open or in flight stays, and so do its calls: a continuation replays from them.
pub(super) async fn drop_oldest(store: &ModuleHistoryStore, fraction: f64) -> Result<(u64, u64), String> {
    exempt(store, move |pool| {
        Box::pin(async move {
            let kept: HashSet<i64> = sqlx::query_scalar("SELECT id FROM run WHERE at_end IS NULL OR pending != 0")
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .collect();
            let calls: Vec<(i64, String, Option<i64>)> = sqlx::query_as("SELECT id, at, run_id FROM call")
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
            let runs: Vec<(i64, String)> =
                sqlx::query_as("SELECT id, COALESCE(at_end, at) FROM run WHERE at_end IS NOT NULL AND pending = 0")
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
            // (when, is a run, id); a time that does not parse sorts oldest.
            let when = |at: &str| swiss_core::util::parse_iso_ms(at).unwrap_or(0);
            let mut timeline: Vec<(i64, bool, i64)> = calls
                .iter()
                .filter(|(_, _, run)| !run.is_some_and(|run| kept.contains(&run)))
                .map(|(id, at, _)| (when(at), false, *id))
                .chain(runs.iter().map(|(id, at)| (when(at), true, *id)))
                .collect();
            timeline.sort_unstable();
            let take = ((timeline.len() as f64 * fraction).ceil() as usize).min(timeline.len());
            let (mut calls, mut runs) = (0u64, 0u64);
            let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
            for (_, is_run, id) in &timeline[..take] {
                let statements: &[&str] = if *is_run {
                    runs += 1;
                    &[
                        "DELETE FROM turn_body WHERE turn_id IN (SELECT id FROM turn WHERE run_id = ?)",
                        "DELETE FROM turn WHERE run_id = ?",
                        "DELETE FROM run WHERE id = ?",
                    ]
                } else {
                    calls += 1;
                    &["DELETE FROM call_body WHERE call_id = ?", "DELETE FROM call WHERE id = ?"]
                };
                for sql in statements {
                    sqlx::query(*sql).bind(id).execute(&mut *tx).await.map_err(|e| e.to_string())?;
                }
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok((calls, runs))
        })
    })
    .await
}

/// Drop every piece no remaining call body or run tail names - the count dropped.
/// The only honest orphan test is opening every body: one that will not open (a
/// key gone, a corrupt chunk) cannot say what it names, so the sweep stops with
/// nothing dropped rather than guess.
pub(super) async fn sweep_pieces(store: &ModuleHistoryStore, keys: &swiss_host::history::seal::ChunkKeys) -> Result<u64, String> {
    let keys = keys.clone();
    exempt(store, move |pool| {
        Box::pin(async move {
            let mut named: HashSet<String> = HashSet::new();
            let owners: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT call_id FROM call_body")
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
            for owner in owners {
                let rows = sqlx::query("SELECT part, codec, plain, block FROM call_body WHERE call_id = ? ORDER BY part")
                    .bind(owner)
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let rows = map_chunks(&rows).map_err(|e| e.to_string())?;
                let text = open_chunks(rows, &keys, "", owner).ok_or(format!("call {owner}'s body will not open"))?;
                // A v1 body names no piece; a v2 envelope names its pieces by hash.
                if let Ok(envelope) = serde_json::from_str::<Value>(&text) {
                    if envelope.get("v").and_then(Value::as_i64) == Some(2) {
                        named.extend(piece_hashes(&envelope));
                    }
                }
            }
            let tails: Vec<String> = sqlx::query_scalar("SELECT tail FROM run WHERE tail IS NOT NULL")
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
            for tail in tails {
                let tail: Value = serde_json::from_str(&tail).map_err(|_| "a run's tail reference will not decode".to_string())?;
                // A legacy tail holds its tools as objects and names no piece.
                for hash in tail.get("tools").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                    named.insert(hash.to_string());
                }
            }
            let held: Vec<String> = sqlx::query_scalar("SELECT hash FROM piece UNION SELECT hash FROM piece_body")
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
            let mut dropped = 0u64;
            let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
            for hash in held.iter().filter(|hash| !named.contains(*hash)) {
                for sql in ["DELETE FROM piece_body WHERE hash = ?", "DELETE FROM piece WHERE hash = ?"] {
                    sqlx::query(sql).bind(hash).execute(&mut *tx).await.map_err(|e| e.to_string())?;
                }
                dropped += 1;
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            Ok(dropped)
        })
    })
    .await
}

/// Shrink the file to its live pages: a VACUUM, then a TRUNCATE checkpoint - in WAL
/// mode the VACUUM writes the whole rewritten file into the log, and a log left at
/// that size would count against the share as much as the free pages did. A busy
/// reader makes either step fail; the next pass tries again.
pub(super) async fn compact(store: &ModuleHistoryStore) {
    let compacted = exempt(store, |pool| {
        Box::pin(async move {
            sqlx::query("VACUUM").execute(&pool).await.map_err(|e| e.to_string())?;
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await.map_err(|e| e.to_string())?;
            Ok(())
        })
    })
    .await;
    if let Err(err) = compacted {
        swiss_core::log::warn(&format!("llm.db compaction after the trim failed: {err}"), None);
    }
}
