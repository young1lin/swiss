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

//! The runs of [LlmHistory]: a conversation's recorder, opening and resuming one, what a
//! continuation may rebuild its context from, and closing it.

use super::*;

impl LlmHistory {
    /// The recorder of one agent conversation. The loop (or a single-ask runner) notes
    /// every turn as it happens; [finish](RunRecorder::finish) files the run row and all
    /// its turns in one queued transaction. Without a history (unit tests, a disabled
    /// plugin) every note is a no-op - an Option that never reads like one at the seams.
    pub fn run_recorder(
        self: &Arc<Self>,
        agent: &str,
        runtime: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> RunRecorder {
        RunRecorder {
            history: Some(self.clone()),
            agent: agent.into(),
            runtime: runtime.into(),
            provider: provider.map(str::to_string),
            model: model.map(str::to_string),
            started: Instant::now(),
            begun: swiss_core::log::iso_now(),
            notes: Notes::default(),
            kind: None,
            id: None,
            refusal: None,
            attempt: None,
            basis: None,
            cancel: None,
            steer: None,
            context: context(),
        }
    }

    /// Start a NEW conversation (SPEC llm.agents Test): the run row is born OPEN
    /// (kind 1) with a database-allocated id - continuable across sends, reloads
    /// and restarts until the reader closes it or a failure files its outcome.
    pub async fn open_conversation(
        self: &Arc<Self>,
        agent: &str,
        runtime: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> RunRecorder {
        let mut run = self.run_recorder(agent, runtime, provider, model);
        run.kind = Some(1);
        run.reserve().await;
        run
    }

    /// Reopen an OPEN conversation on its own row: same id, same turns - the
    /// recorder notes only what is new. Refused ([ResumeError] says why; the
    /// route maps it to its 409 code) when the row is not an open conversation,
    /// belongs to another agent or another scope (task and subject, see
    /// [super::within]), or its provider or model no longer match the
    /// resolution in force - unless the reader moved it there first
    /// ([Self::retarget_conversation]).
    pub async fn resume_conversation(
        self: &Arc<Self>,
        run_id: i64,
        agent: &str,
        runtime: &str,
        provider: Option<&str>,
        model: Option<&str>,
        wire: Option<&Value>,
    ) -> Result<RunRecorder, ResumeError> {
        // The run's own writes must be visible to the provenance read below: a
        // reservation or call still queued would read as absent and the guard
        // would pass on evidence it has not seen.
        self.flush().await;
        let agent = agent.to_string();
        let runtime = runtime.to_string();
        let provider = provider.map(str::to_string);
        let model = model.map(str::to_string);
        let scope = super::context().clipped();
        let held = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query(
                        "SELECT agent, runtime, provider, model, ok, at_end, kind, pending, attempt, task, subject FROM run WHERE id = ?",
                    )
                    .bind(run_id)
                    .fetch_optional(&pool)
                    .await?
                    .ok_or(sqlx::Error::RowNotFound)?;
                    Ok::<(String, String, Option<String>, Option<String>, Option<i64>, Option<String>, Option<i64>, i64, i64, Option<String>, Option<String>), sqlx::Error>((
                        row.try_get(0)?,
                        row.try_get(1)?,
                        row.try_get(2)?,
                        row.try_get(3)?,
                        row.try_get(4)?,
                        row.try_get(5)?,
                        row.try_get(6)?,
                        row.try_get(7)?,
                        row.try_get(8)?,
                        row.try_get(9)?,
                        row.try_get(10)?,
                    ))
                })
            })
            .await;
        // A store that cannot answer cannot vouch for the run either - the
        // refusal is honest either way (the reader starts a New run).
        let (row_agent, row_runtime, row_provider, row_model, ok, at_end, kind, pending, attempt, row_task, row_subject) =
            held.map_err(|_| ResumeError::Gone)?;
        if kind != Some(1) || ok.is_some() || at_end.is_some() {
            return Err(ResumeError::Closed);
        }
        if row_agent != agent || row_runtime != runtime || row_task != scope.task || row_subject != scope.subject {
            // Another agent's run, or another scope's (a Data connection's dialogue
            // from a Test, one connection's from another), reads as absent here.
            return Err(ResumeError::Gone);
        }
        if row_provider != provider || row_model != model {
            return Err(ResumeError::ContextChanged);
        }
        // The EFFECTIVE context, not just its name: the endpoint and the header
        // fingerprint (which carries the wire style) sealed beside the run's
        // latest call to the provider and model in force. A same-name entry
        // edited to another endpoint - or another style on the same URL - is a
        // different context: old thinking provenance is invalid and a new vendor
        // would read the old dialogue. A run whose body there was pruned cannot
        // prove its provenance and refuses the same way; a run with no call to
        // them yet - none at all, or the reader just moved it to them - has
        // nothing to contradict. A deadline's stand-in claims no wire, so the
        // round it cut is the one that testifies (see [Self::last_round]).
        if let Some(wire) = wire {
            let held_to = provider.as_deref().zip(model.as_deref());
            match self.last_round(run_id, held_to).await {
                Ok(LastRound::Sealed(asked)) => {
                    let held = asked.get("request");
                    let endpoint_matches = held
                        .and_then(|request| request.get("endpoint"))
                        .is_some_and(|held| held == wire.get("endpoint").unwrap_or(&Value::Null));
                    let headers_match = held
                        .and_then(|request| request.get("headers"))
                        .is_some_and(|held| held == wire.get("headers").unwrap_or(&Value::Null));
                    if !endpoint_matches || !headers_match {
                        return Err(ResumeError::ContextChanged);
                    }
                }
                // Pruned or unreadable: the provenance cannot be verified.
                Ok(LastRound::Gone | LastRound::Undecodable) => return Err(ResumeError::ContextChanged),
                Ok(LastRound::None) | Err(_) => {}
            }
        }
        // The CANONICAL activity gate (SPEC llm.history). A mark still in
        // flight is either driven - a round of this conversation is running,
        // so the reader waits - or stale: the drive that set it is gone (a
        // Stop whose round could not file, a failed write, a deadline, a
        // restart). A stale mark is released here with an honest tail and the
        // conversation continues from its last sealed round; an interruption
        // never strands it. The legacy-ambiguous mark and a pruned or
        // pre-protocol last body still refuse: no sealed state to stand on.
        if pending == 1 {
            match self.release_stale_round(run_id, attempt).await {
                release::Released::Yes => {}
                release::Released::Busy => return Err(ResumeError::Busy),
                release::Released::Unprovable => return Err(ResumeError::NoCanonical),
            }
        } else if pending != 0 {
            return Err(ResumeError::NoCanonical);
        }
        match self.last_round(run_id, None).await.map_err(|_| ResumeError::Gone)? {
            // No sealed round at all - unused, or every round was interrupted
            // before its call could file: the dialogue the model knows starts
            // with the next send (see [Self::replay_state]).
            LastRound::None => {}
            LastRound::Sealed(canonical) => {
                if canonical.pointer("/request/replay").is_none() {
                    return Err(ResumeError::NoCanonical);
                }
            }
            LastRound::Gone | LastRound::Undecodable => return Err(ResumeError::NoCanonical),
        }
        let mut run = self.run_recorder(&agent, &runtime, row_provider.as_deref(), row_model.as_deref());
        run.kind = Some(1);
        run.id = Some(run_id);
        // The stale-prior basis: THIS resume observed the row's activity
        // version; later rounds of this recorder claim against exactly it.
        run.basis = Some(attempt);
        self.live.begin(run_id, run.notes.clone());
        Ok(run)
    }


    /// Move the open conversation `run_id` to another provider and model, the reader
    /// having picked them for what comes next (the Data agent's model menu): its row
    /// names them, so the continuation guards hold the dialogue to the pick and the
    /// wire check waits for a call to them (see [Self::resume_conversation]). Only an
    /// open conversation of this agent in the scope in force moves. Ok(Some) the
    /// provider and model it named before, to put back when what follows refuses;
    /// Ok(None) no such conversation here - the guards that follow say why.
    pub async fn retarget_conversation(
        &self,
        run_id: i64,
        agent: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<Option<(Option<String>, Option<String>)>, String> {
        self.flush().await;
        let agent = agent.to_string();
        let provider = provider.map(str::to_string);
        let model = model.map(str::to_string);
        let scope = super::context().clipped();
        type Before = Option<(Option<String>, Option<String>)>;
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<Before, String>>();
        // The reopen's lane (submit_exempt): the reader's own lifecycle write.
        let submitted = self.store.submit_exempt(64, move |pool| {
            Box::pin(async move {
                let before = sqlx::query(
                    "SELECT provider, model FROM run
                      WHERE id = ? AND agent = ? AND task IS ? AND subject IS ? AND kind = 1 AND ok IS NULL AND at_end IS NULL",
                )
                    .bind(run_id)
                    .bind(&agent)
                    .bind(&scope.task)
                    .bind(&scope.subject)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let Some(before) = before else {
                    let _ = reply.send(Ok(None));
                    return Ok(());
                };
                let before: (Option<String>, Option<String>) =
                    (before.try_get(0).map_err(|e| e.to_string())?, before.try_get(1).map_err(|e| e.to_string())?);
                sqlx::query(
                    "UPDATE run SET provider = ?, model = ?
                      WHERE id = ? AND agent = ? AND task IS ? AND subject IS ? AND kind = 1 AND ok IS NULL AND at_end IS NULL",
                )
                    .bind(&provider)
                    .bind(&model)
                    .bind(run_id)
                    .bind(&agent)
                    .bind(&scope.task)
                    .bind(&scope.subject)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let _ = reply.send(Ok(Some(before)));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(answer) => answer,
                Err(_) => Err("the retarget reply was lost; the run state is unproven".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// The provider and model the run's latest call went to; None before its first, or
    /// when the store cannot answer.
    pub async fn last_call_model(&self, run_id: i64) -> Option<(String, String)> {
        self.flush().await;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT provider, model FROM call WHERE run_id = ? ORDER BY id DESC LIMIT 1")
                        .bind(run_id)
                        .fetch_optional(&pool)
                        .await?
                        .map(|row| Ok::<_, sqlx::Error>((row.try_get(0)?, row.try_get(1)?)))
                        .transpose()
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// The one LIVE open conversation of an agent in the scope in force (its
    /// task and subject, see [super::within]), by id: the one-open oracle
    /// (SPEC llm.history). A run whose round is IN FLIGHT (pending 1) is as
    /// live as a settled one - a second conversation must not open beside
    /// either, and only an explicit close or New recovers. Rows the v4
    /// migration marked ambiguous (-1) stay open for audit but are not live -
    /// they never answer here, and so never block a fresh conversation.
    pub async fn continuable_open(&self, agent: &str) -> Option<i64> {
        self.flush().await;
        let agent = agent.to_string();
        let scope = super::context().clipped();
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query(
                        "SELECT id FROM run WHERE agent = ? AND task IS ? AND subject IS ? AND kind = 1 AND ok IS NULL AND at_end IS NULL AND pending IN (0, 1)
                         ORDER BY id LIMIT 1",
                    )
                    .bind(&agent)
                    .bind(&scope.task)
                    .bind(&scope.subject)
                    .fetch_optional(&pool)
                    .await?
                    .map(|row| row.try_get::<i64, _>(0))
                    .transpose()
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// The system note the run filed on its first send (None when it filed
    /// none): the continuation guard - a changed first word is a New run,
    /// never a mid-dialogue swap.
    /// It reads the turn list and that one turn's body - never the whole dialogue, whose
    /// every call body a long conversation would decode on each send.
    pub async fn system_note_of(&self, run_id: i64) -> Option<String> {
        let run = self.read_turns(run_id).await.ok()??;
        let turn = run["turns"].as_array()?.iter().find(|turn| turn["role"] == "system")?;
        if !turn["hasBody"].as_bool().unwrap_or(false) {
            return turn["preview"].as_str().map(str::to_string);
        }
        // A body that will not read is a note that differs, as the dialogue's empty text
        // was: the guard fails closed.
        let seq = turn["seq"].as_i64().unwrap_or(0);
        let body = self.read_turn_body(run_id, seq).await.ok().flatten();
        Some(body.and_then(|v| v["body"].as_str().map(str::to_string)).unwrap_or_default())
    }

    /// The continuation canonical prior context (see [ReplayState]): the last
    /// call's sealed replay log plus its response, the partial tail when a
    /// cancelled round left one, with the activity protocol gating every
    /// ambiguous shape closed. Read-only.
    pub async fn replay_state(&self, run_id: i64) -> ReplayState {
        self.flush().await;
        let held = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query(
                        "SELECT pending, tail FROM run WHERE id = ? AND kind = 1 AND ok IS NULL AND at_end IS NULL",
                    )
                    .bind(run_id)
                    .fetch_optional(&pool)
                    .await?;
                    let Some(row) = row else { return Ok::<Option<(i64, Option<String>)>, sqlx::Error>(None) };
                    let pending: i64 = row.try_get(0)?;
                    let tail: Option<String> = row.try_get(1)?;
                    Ok(Some((pending, tail)))
                })
            })
            .await;
        let Some((pending, tail)) = held.ok().flatten() else {
            return ReplayState::Refused("the run is not an open conversation");
        };
        // The activity protocol's closed shapes: a mark still in flight (an
        // outbound may have flown with no sealed completion) and the v4
        // migration's legacy-ambiguous mark.
        if pending == -1 {
            return ReplayState::Refused("this conversation predates the activity protocol and cannot prove its tail: start a New one");
        }
        if pending == 1 {
            // Driven, or stale until the next resume releases it (see
            // [Self::resume_conversation]).
            return ReplayState::Refused("this conversation has a round in flight: wait for it, or Stop it, and send again");
        }
        // Activity exists: the canonical body must exist with it, or the tail
        // cannot be proven - fail closed rather than replay a guess. A
        // present-but-corrupt id column fails the read the same way, never
        // silently an absent call.
        let sealed = match self.last_round(run_id, None).await {
            Ok(LastRound::Sealed(sealed)) => sealed,
            // No sealed round: reserved and never asked, or every round was
            // interrupted before its call filed (noted turns, nothing the
            // model answered on record). Either way the context the model
            // knows is empty and the next send is its first word - the noted
            // turns stay in the transcript, never replayed as a guess.
            Ok(LastRound::None) => return ReplayState::Fresh,
            Ok(LastRound::Undecodable) => {
                return ReplayState::Refused("this conversation's last body did not decode: start a New one");
            }
            Ok(LastRound::Gone) | Err(_) => {
                return ReplayState::Refused("this conversation's last round has no sealed body: start a New one");
            }
        };
        let mut prior: Vec<Value> = sealed
            .pointer("/request/replay")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if prior.is_empty() {
            // A pre-protocol body (no structural replay log): same one-time
            // policy as the migration - honest refusal, never a note parse.
            return ReplayState::Refused("this conversation predates the replay protocol: start a New one");
        }
        // The last round's sealed response joins the log: its texts, its
        // thinking, its tool calls - ids and arguments exactly as they flew.
        if let Some(response) = sealed.pointer("/response") {
            if response.get("error").is_none() {
                let text = response["texts"]
                    .as_array()
                    .map(|texts| texts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(""))
                    .unwrap_or_default();
                let thinking = response["thinking"]
                    .as_array()
                    .and_then(|all| all.first())
                    .and_then(Value::as_str)
                    .map(|one| one.to_string());
                let calls: Vec<Value> = response["toolCalls"]
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call["callId"],
                                    "name": call["name"],
                                    "arguments": call["arguments"],
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // The round's signed thinking rides in its sealed content parts.
                let signed = response["contents"]
                    .as_array()
                    .and_then(|parts| parts.iter().find_map(|part| part["ThoughtSignature"].as_str()));
                if !text.is_empty() || !calls.is_empty() {
                    prior.push(crate::agent::log_round(&text, thinking.as_deref(), signed, calls));
                }
            }
        }
        // The last round's calls must be ANSWERED before its context may
        // replay: an Ok round with tool calls is not terminal - its results
        // tail (every wire id, completed answer or UNKNOWN verdict alike) is
        // the authority that completes it. The tail reads through the
        // CANONICAL sealed decode: a present-but-corrupt reference refuses,
        // never silently skips; a missing tail with dangling calls refuses;
        // the ids pair EXACTLY - no unanswered call, no answer without its
        // call. Anything else would ask the provider a request it cannot
        // answer truthfully.
        let round_call_ids: Vec<Value> = sealed
            .pointer("/response/toolCalls")
            .and_then(Value::as_array)
            .map(|calls| calls.iter().map(|call| call["callId"].clone()).collect())
            .unwrap_or_default();
        let answered: Vec<Value> = match self.decode_tail(tail.as_deref()).await {
            Ok(Some(decoded)) => decoded["tools"].as_array().cloned().unwrap_or_default(),
            Ok(None) => Vec::new(),
            Err(_) => {
                return ReplayState::Refused(
                    "this conversation's results tail is present but unreadable: start a New one",
                );
            }
        };
        if !round_call_ids.is_empty() {
            let expected: std::collections::HashSet<&Value> = round_call_ids.iter().collect();
            let got: std::collections::HashSet<&Value> = answered.iter().map(|tool| &tool["id"]).collect();
            if expected != got || answered.len() != round_call_ids.len() {
                return ReplayState::Refused(
                    "this conversation's last round has calls without their authoritative results: start a New one",
                );
            }
            for tool in &answered {
                prior.push(json!({
                    "role": "tool",
                    "id": tool["id"],
                    "text": tool["text"],
                }));
            }
        } else if !answered.is_empty() {
            // A tail answering calls the last round never made: damage, not
            // context - refuse rather than invent a pairing.
            return ReplayState::Refused(
                "this conversation's results tail answers calls its last round never made: start a New one",
            );
        }
        ReplayState::Continuable { prior }
    }

    /// The run's newest round, read past any deadline stand-ins ([stands_in_for_a_deadline]);
    /// `of` narrows it to one provider and model. Every continuation guard reads the dialogue's
    /// end through here, so a deadline strands no conversation (SPEC llm.history). A body that
    /// is gone or will not decode cannot prove it is a stand-in and answers as itself.
    pub(super) async fn last_round(&self, run_id: i64, of: Option<(&str, &str)>) -> Result<LastRound, String> {
        let of = of.map(|(provider, model)| (provider.to_string(), model.to_string()));
        let mut before = i64::MAX;
        loop {
            let held_to = of.clone();
            // Fallible decode with transpose: a present-but-corrupt id column
            // propagates as the query error, never silently an absent call.
            let next = self
                .store
                .query(move |pool| {
                    Box::pin(async move {
                        let row = match &held_to {
                            Some((provider, model)) => {
                                sqlx::query(
                                    "SELECT id FROM call WHERE run_id = ? AND id < ? AND provider IS ? AND model IS ? ORDER BY id DESC LIMIT 1",
                                )
                                .bind(run_id)
                                .bind(before)
                                .bind(provider)
                                .bind(model)
                                .fetch_optional(&pool)
                                .await?
                            }
                            None => {
                                sqlx::query("SELECT id FROM call WHERE run_id = ? AND id < ? ORDER BY id DESC LIMIT 1")
                                    .bind(run_id)
                                    .bind(before)
                                    .fetch_optional(&pool)
                                    .await?
                            }
                        };
                        row.map(|row| row.try_get::<i64, _>(0)).transpose()
                    })
                })
                .await
                .map_err(|e| e.to_string())?;
            let Some(id) = next else { return Ok(LastRound::None) };
            let body = match self.read_call_body(id).await {
                Ok(Some(body)) if body["bodyGone"] == false => body,
                _ => return Ok(LastRound::Gone),
            };
            let Some(sealed) = body["body"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok()) else {
                return Ok(LastRound::Undecodable);
            };
            if !stands_in_for_a_deadline(&sealed) {
                return Ok(LastRound::Sealed(sealed));
            }
            before = id;
        }
    }

    /// Close an open conversation NEUTRALLY (the reader pressed New): at_end now,
    /// ok stays NULL - a user's clear is not a failure - idempotent, read-only
    /// afterwards. Ok(None) = no such conversation.
    pub async fn close_run(&self, run_id: i64) -> Result<Option<()>, String> {
        let now = swiss_core::log::iso_now();
        // The reader's close rides submit_exempt, not a query-side mutation:
        // queued, generation-stamped, unpriced - the same lifecycle precedent
        // as the session-END stamp (a quota that refuses a reader's close
        // would wedge the run open forever). Withdrawal still refuses it: a
        // closed store answers NotOpen, and the route maps that honestly.
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<Option<()>, String>>();
        let submitted = self.store.submit_exempt(64, move |pool| {
            Box::pin(async move {
                let closed = sqlx::query("UPDATE run SET at_end = ? WHERE id = ? AND kind = 1 AND ok IS NULL")
                    .bind(&now)
                    .bind(run_id)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                if closed.rows_affected() > 0 {
                    let _ = reply.send(Ok(Some(())));
                    return Ok(());
                }
                let exists = sqlx::query("SELECT 1 FROM run WHERE id = ? AND kind = 1")
                    .bind(run_id)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let _ = reply.send(Ok(exists.is_some().then_some(())));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(answer) => answer,
                Err(_) => Err("the close reply was lost; the run state is unproven".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// Reopen a conversation the reader closed (a Data connection's History, Continue):
    /// its own row, turns and calls as they stand, at_end cleared and nothing else touched.
    /// Only a neutrally closed conversation of this agent in the scope in force, with no
    /// round left unresolved, reopens - and only while that scope holds no other open one:
    /// the one-open rule, checked in the same statement. Ok(true) reopened; Ok(false) no
    /// such closed conversation here, or another is open. The continuation guards still
    /// judge its next send.
    pub async fn reopen_run(&self, run_id: i64, agent: &str) -> Result<bool, String> {
        self.flush().await;
        let agent = agent.to_string();
        let scope = super::context().clipped();
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<bool, String>>();
        // The close's lane (submit_exempt): a lifecycle write a quota must not wedge.
        let submitted = self.store.submit_exempt(64, move |pool| {
            Box::pin(async move {
                let reopened = sqlx::query(
                    "UPDATE run SET at_end = NULL
                      WHERE id = ? AND agent = ? AND task IS ? AND subject IS ? AND kind = 1 AND ok IS NULL AND at_end IS NOT NULL AND pending = 0
                        AND NOT EXISTS (SELECT 1 FROM run r WHERE r.agent = ? AND r.task IS ? AND r.subject IS ? AND r.kind = 1
                                          AND r.ok IS NULL AND r.at_end IS NULL AND r.pending IN (0, 1))",
                )
                .bind(run_id)
                .bind(&agent)
                .bind(&scope.task)
                .bind(&scope.subject)
                .bind(&agent)
                .bind(&scope.task)
                .bind(&scope.subject)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                let _ = reply.send(Ok(reopened.rows_affected() > 0));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(answer) => answer,
                Err(_) => Err("the reopen reply was lost; the run state is unproven".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// Close a conversation as FAILED: at_end now, ok false - a run that must never
    /// continue or reopen (a compaction whose checkpoint did not land). Idempotent.
    pub async fn fail_conversation(&self, run_id: i64) -> Result<(), String> {
        // The run's own queued writes (its finish) land first: this one closes over them.
        self.flush().await;
        let now = swiss_core::log::iso_now();
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let submitted = self.store.submit_exempt(64, move |pool| {
            Box::pin(async move {
                sqlx::query("UPDATE run SET ok = 0, at_end = coalesce(at_end, ?) WHERE id = ? AND kind = 1")
                    .bind(&now)
                    .bind(run_id)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                let _ = reply.send(Ok(()));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => receipt.await.unwrap_or_else(|_| Err("the failure reply was lost; the run state is unproven".to_string())),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The run's size facts: turn rows, the run's decoded FULL-PLAIN record bytes
    /// (turn bodies plus call bodies, the materialization the database actually
    /// holds - never chars, never previews, never a physical-dedup fiction), and
    /// the run's provider calls. The continuation guard rejects on the turn and
    /// call counts; the record bytes are no request's size (every call re-sends
    /// the dialogue before it), so its byte budget weighs the replay instead.
    pub async fn conversation_size(&self, run_id: i64) -> Result<(i64, i64, i64), String> {
        self.flush().await;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let turns: i64 = sqlx::query("SELECT count(*) FROM turn WHERE run_id = ?")
                        .bind(run_id)
                        .fetch_one(&pool)
                        .await?
                        .try_get(0)?;
                    let turn_bytes: i64 = sqlx::query(
                        "SELECT COALESCE(sum(tb.plain), 0) FROM turn_body tb
                           JOIN turn t ON t.id = tb.turn_id WHERE t.run_id = ?",
                    )
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await?
                    .try_get(0)?;
                    // The decoded FULL-PLAIN record bytes, writer-known: call
                    // bodies count their ADDITIVE reference weight (envelope
                    // plus every content-addressed piece they re-send - a
                    // physically deduplicated dialogue is still re-read per
                    // round, so it is charged per round), never the chunk
                    // sums a shared piece store would underreport. The run's
                    // referenced tail counts with its own plain weight.
                    let call_bytes: i64 = sqlx::query(
                        "SELECT COALESCE(sum(body_plain), 0) FROM call WHERE run_id = ?",
                    )
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await?
                    .try_get(0)?;
                    let tail_bytes: i64 = sqlx::query("SELECT COALESCE(tail_plain, 0) FROM run WHERE id = ?")
                        .bind(run_id)
                        .fetch_one(&pool)
                        .await?
                        .try_get(0)?;
                    let calls: i64 = sqlx::query("SELECT count(*) FROM call WHERE run_id = ?")
                        .bind(run_id)
                        .fetch_one(&pool)
                        .await?
                        .try_get(0)?;
                    Ok::<(i64, i64, i64), sqlx::Error>((turns, turn_bytes + call_bytes + tail_bytes, calls))
                })
            })
            .await
            .map_err(|e| e.to_string())
    }
}

/// What [LlmHistory::last_round] found at the end of a run's dialogue.
pub(super) enum LastRound {
    /// No round on record, deadline stand-ins aside.
    None,
    /// The newest round: its sealed body, decoded.
    Sealed(Value),
    /// The newest call's body is pruned or will not read.
    Gone,
    /// The newest call's body is on record but does not decode.
    Undecodable,
}

/// A deadline's stand-in ([LlmHistory::record_chat_timeout_in_run]): the round trip a
/// deadline dropped, filed with the wait and no wire facts, no replay and no answer. It
/// seals nothing the model was told or said, so the guards read past it to the round it
/// cut. A round that flew and timed out on the wire carries its replay and stays itself.
fn stands_in_for_a_deadline(sealed: &Value) -> bool {
    sealed.pointer("/response/kind").and_then(Value::as_str) == Some("timeout")
        && sealed.pointer("/request/replay").is_none()
        && sealed.pointer("/request/endpoint").is_none()
}
