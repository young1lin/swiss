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

//! The release of a stale in-flight round (SPEC llm.history): a conversation whose
//! activity mark no drive holds any more - a Stop whose round could not file, a history
//! write that failed, a deadline that dropped the drive, a gateway that restarted
//! mid-round - settles at the door with an honest tail, so the reader continues it
//! instead of starting a New one.
//!
//! A mark is in flight only while the drive that set it lives. Every mark names its
//! holder (`<pid>:<process start ms>:<handle>`); the door releases it when that holder
//! is provably gone: this handle's own drive ended, another handle of this process holds
//! no drive for the run, or the holder's process is no longer the one that set it.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{LazyLock, Mutex, Weak};

use super::runs::LastRound;
use super::*;

/// The answer a released round gives a tool call whose result never landed.
pub(super) const RELEASED_RESULT: &str =
    "execution unknown - the round was interrupted before its result was recorded";

/// Every history handle open in this process, by its number: a mark another handle of
/// this process set is in flight only while that handle's live registry holds the run.
static HANDLES: LazyLock<Mutex<HashMap<u64, Weak<LlmHistory>>>> = LazyLock::new(Default::default);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
/// This process's own start, in Unix milliseconds: half its identity against pid reuse.
static STARTED_MS: LazyLock<u64> =
    LazyLock::new(|| swiss_core::platform::process_created_at(std::process::id()).unwrap_or(0));

/// A new handle's number and the holder name its marks carry.
pub(super) fn mint_holder() -> (u64, String) {
    let number = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    (number, format!("{}:{}:{number}", std::process::id(), *STARTED_MS))
}

/// Enroll an opened handle, so another handle of this process can ask whether a mark
/// it set is still driven. Handles already dropped leave the registry here.
pub(super) fn enroll(history: &Arc<LlmHistory>) {
    if let Ok(mut handles) = HANDLES.lock() {
        handles.retain(|_, handle| handle.strong_count() > 0);
        handles.insert(history.handle, Arc::downgrade(history));
    }
}

/// Whether the drive behind a mark held by `holder` on run `run_id` may still be
/// running. A holder that does not parse fails closed (held).
pub(super) fn holder_drives(holder: &str, run_id: i64) -> bool {
    let mut parts = holder.splitn(3, ':');
    let (Some(pid), Some(started), Some(number)) = (parts.next(), parts.next(), parts.next()) else {
        return true;
    };
    let (Ok(pid), Ok(started), Ok(number)) = (pid.parse::<u32>(), started.parse::<u64>(), number.parse::<u64>()) else {
        return true;
    };
    if pid != std::process::id() || started != *STARTED_MS {
        // Another process (or an earlier one under a reused pid): it drives only while
        // that very process lives.
        if !swiss_core::platform::pid_alive(pid) {
            return false;
        }
        return started == 0 || swiss_core::platform::process_created_at(pid).is_none_or(|now| now == started);
    }
    let handle = HANDLES.lock().ok().and_then(|handles| handles.get(&number).and_then(Weak::upgrade));
    handle.is_some_and(|handle| handle.live.has(run_id))
}

/// What the door did with an in-flight mark.
pub(super) enum Released {
    /// The mark was stale and is cleared: the run continues from its last sealed state.
    Yes,
    /// A drive still holds it, or the row moved under the release: wait, then retry.
    Busy,
    /// The run's last sealed round cannot be read, so no honest tail can be built.
    Unprovable,
}

impl LlmHistory {
    /// Release run `run_id`'s in-flight mark at `attempt` when no drive holds it. The
    /// release is the round's completion the drive never filed: the mark clears, token
    /// matched, and the results tail answers every tool call of the last sealed round by
    /// its wire id - the result in hand when the run's tail carries it, an UNKNOWN verdict
    /// when it does not - or no tail at all when that round asked no tools. The attempt
    /// stays: a resumed recorder claims its next round on it as on any settled one.
    pub(super) async fn release_stale_round(&self, run_id: i64, attempt: i64) -> Released {
        let read = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT holder, tail FROM run WHERE id = ? AND pending = 1 AND attempt = ?")
                        .bind(run_id)
                        .bind(attempt)
                        .fetch_optional(&pool)
                        .await?;
                    let Some(row) = row else { return Ok::<_, sqlx::Error>(None) };
                    let holder: Option<String> = row.try_get(0)?;
                    let tail: Option<String> = row.try_get(1)?;
                    let last_call = sqlx::query("SELECT id FROM call WHERE run_id = ? ORDER BY id DESC LIMIT 1")
                        .bind(run_id)
                        .fetch_optional(&pool)
                        .await?
                        .map(|row| row.try_get::<i64, _>(0))
                        .transpose()?;
                    Ok(Some((holder, tail, last_call)))
                })
            })
            .await;
        let Ok(Some((holder, tail, last_call))) = read else { return Released::Busy };
        // A mark from before holders were stamped was set by a build no longer running.
        if self.live.has(run_id) || holder.as_deref().is_some_and(|holder| holder_drives(holder, run_id)) {
            return Released::Busy;
        }
        // The last sealed round's tool calls, as they flew: past a deadline's stand-in,
        // the round it cut (see last_round). An answer that failed asked none; a body
        // that will not read leaves nothing honest to pair. The token below stays the
        // newest call of any kind, so a call filed meanwhile still moves it.
        let calls: Vec<Value> = match self.last_round(run_id, None).await {
            Ok(LastRound::None) => Vec::new(),
            Ok(LastRound::Sealed(sealed)) => match sealed.pointer("/response") {
                Some(response) if response.get("error").is_none() => {
                    response["toolCalls"].as_array().cloned().unwrap_or_default()
                }
                _ => Vec::new(),
            },
            Ok(LastRound::Gone | LastRound::Undecodable) => return Released::Unprovable,
            Err(_) => return Released::Busy,
        };
        // What the run's tail already answers, by wire id: a settled earlier round's
        // results, or the audit tail an interrupted dispatch queued. An unreadable tail
        // answers nothing - every call then reads UNKNOWN, never a guess.
        let answered: Vec<Value> = match self.decode_tail(tail.as_deref()).await {
            Ok(Some(decoded)) => decoded["tools"].as_array().cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        let tail = (!calls.is_empty()).then(|| {
            let tools: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let id = &call["callId"];
                    answered
                        .iter()
                        .find(|tool| &tool["id"] == id)
                        .cloned()
                        .unwrap_or_else(|| json!({ "id": id, "name": call["name"], "text": RELEASED_RESULT }))
                })
                .collect();
            json!({ "tools": tools })
        });
        let (envelope_text, pieces, weight) = match &tail {
            Some(tail) => {
                let (envelope, pieces) = split_tail(tail);
                let text = envelope.to_string();
                let weight = text.len() + pieces.iter().map(|piece| piece.text.len()).sum::<usize>();
                (Some(text), pieces, weight)
            }
            None => (None, Vec::new(), 0),
        };
        let keys = self.keys.clone();
        let (reply, receipt) = tokio::sync::oneshot::channel::<bool>();
        let submitted = self.submit(weight + 256, move |pool| {
            Box::pin(async move {
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                for piece in &pieces {
                    let filed = sqlx::query("INSERT OR IGNORE INTO piece (hash, chars) VALUES (?, ?)")
                        .bind(&piece.hash)
                        .bind(piece.text.chars().count() as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    if filed.rows_affected() > 0 {
                        write_piece_chunks(&mut tx, &piece.hash, &piece.text, &keys)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
                // Token matched on everything the tail was built from: the same mark,
                // the same holder, the same last call. Anything moved, nothing clears.
                let cleared = sqlx::query(
                    "UPDATE run SET pending = 0, tail = ?, tail_plain = ?
                     WHERE id = ? AND pending = 1 AND attempt = ? AND holder IS ?
                       AND (SELECT max(id) FROM call WHERE run_id = ?) IS ?",
                )
                .bind(&envelope_text)
                .bind(weight as i64)
                .bind(run_id)
                .bind(attempt)
                .bind(&holder)
                .bind(run_id)
                .bind(last_call)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
                tx.commit().await.map_err(|e| e.to_string())?;
                let _ = reply.send(cleared.rows_affected() > 0);
                Ok(())
            })
        });
        if submitted.is_err() {
            return Released::Unprovable;
        }
        match receipt.await {
            Ok(true) => {
                swiss_core::log::info(&format!(
                    "llm conversation {run_id}: released the round interrupted at attempt {attempt}"
                ));
                Released::Yes
            }
            Ok(false) => Released::Busy,
            Err(_) => Released::Unprovable,
        }
    }
}
