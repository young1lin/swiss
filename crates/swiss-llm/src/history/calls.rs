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

//! The call rows of [LlmHistory]: one chat round trip, a timed-out one, and one embedding,
//! each admitted at once and filed by one queued transaction.

use super::*;

impl LlmHistory {
    /// One chat round trip - the seam [answer](crate::provider::answer) and the
    /// builtin loop already report telemetry from. The row carries the tokens and
    /// latency; the sealed body holds the request digest and the answer (texts,
    /// tool calls, usage) or the error. Admission is synchronous and bounded; the
    /// write is one queued transaction, so an ask never waits on disk.
    pub fn record_chat(&self, call: ChatCall<'_>) {
        self.record_chat_scoped(None, call);
    }

    /// The same round trip, associated with a RESERVED run: the call row carries
    /// the id the reservation allocated (never a time-window join).
    pub fn record_chat_in_run(&self, run_id: i64, call: ChatCall<'_>) {
        self.record_chat_scoped(Some(run_id), call);
    }

    pub(super) fn record_chat_scoped(&self, run_id: Option<i64>, call: ChatCall<'_>) {
        let at = swiss_core::log::iso_now();
        let mut body = json!({ "request": { "prompt": call.prompt, "turn": call.turn } });
        if let Some(system) = call.system {
            body["request"]["system"] = json!(system);
        }
        // The complete ask: the wire facts (endpoint, headers with credentials named
        // but never valued, options) and every message and tool the request carried.
        if let Some(wire) = call.wire {
            for key in ["endpoint", "headers", "options"] {
                if let Some(value) = wire.get(key) {
                    body["request"][key] = value.clone();
                }
            }
        }
        if let Some(asked) = call.request {
            for key in ["messages", "tools"] {
                if let Some(value) = asked.get(key).filter(|v| !v.is_null()) {
                    body["request"][key] = value.clone();
                }
            }
        }
        // The canonical replay log rides the request: the structural prior
        // context, so the NEXT send rebuilds from sealed structure instead of
        // parsing display notes back into meaning.
        if let Some(replay) = call.replay {
            body["request"]["replay"] = replay.clone();
        }
        let row = match &call.outcome {
            ChatOutcome::Ok { texts, thinking, tool_calls, cut, tokens, ms, ttft_ms, request_id, stop_reason, contents } => {
                body["response"] = json!({
                    "texts": texts,
                    "toolCalls": tool_calls.iter().map(|(name, arguments, call_id)| json!({ "name": name, "arguments": arguments, "callId": call_id })).collect::<Vec<_>>(),
                    "cut": cut,
                    "tokens": tokens_json(tokens),
                    "requestId": request_id,
                });
                if let Some(stop) = stop_reason {
                    body["response"]["stopReason"] = json!(stop);
                }
                if let Some(ttft) = ttft_ms {
                    body["response"]["ttftMs"] = json!(ttft);
                }
                // The complete answer beside the curated keys: every content part as
                // the adapter parsed it (a parts list - an array - or an object),
                // when the answer held any.
                let has_contents = contents.as_object().is_some_and(|o| !o.is_empty())
                    || contents.as_array().is_some_and(|a| !a.is_empty());
                if has_contents {
                    body["response"]["contents"] = (*contents).clone();
                }
                // The reasoning rides beside the texts it preceded, not inside them:
                // the panel greys it out, and a consumer that wants the answer alone
                // still reads texts as the answer.
                if !thinking.is_empty() {
                    body["response"]["thinking"] = json!(thinking);
                }
                CallRow {
                    at,
                    kind: "chat",
                    provider: call.provider.into(),
                    model: call.model.into(),
                    agent: call.agent.map(str::to_string),
                    ok: true,
                    ms: *ms,
                    status: None,
                    tokens: *tokens,
                    request_id: Some(request_id.to_string()),
                }
            }
            ChatOutcome::Err(err) => {
                body["response"] = json!({ "error": err.message, "status": err.status, "kind": err.kind });
                CallRow {
                    at,
                    kind: "chat",
                    provider: call.provider.into(),
                    model: call.model.into(),
                    agent: call.agent.map(str::to_string),
                    ok: false,
                    ms: err.duration_ms,
                    status: err.status,
                    tokens: Tokens::default(),
                    request_id: None,
                }
            }
        };
        // The settlement policy (SPEC llm.history): only a TERMINAL round
        // clears its activity mark at insert - a plain answer, an observed
        // cancelled prefix, an error, a timeout: the sealed body IS the
        // whole truth of that round. An Ok round WITH tool calls is NOT
        // terminal: its RPC results are still outstanding, the mark stays
        // in flight, and only the dispatch's authoritative results tail
        // (settle_round) completes it - so no continuation and no second
        // outbound may ever be authorized over dangling calls.
        let settle = match (run_id, call.attempt) {
            (Some(run_id), Some(attempt)) => {
                let terminal = match &call.outcome {
                    ChatOutcome::Err(_) => true,
                    ChatOutcome::Ok { tool_calls, .. } => tool_calls.is_empty(),
                };
                terminal.then_some((run_id, attempt as i64))
            }
            _ => None,
        };
        self.submit_call_scoped(run_id, row, &body.to_string(), call.attempt, settle);
    }

    /// A round trip a consumer-side deadline dropped: the provider call was still in
    /// flight when a timeout cancelled it, so no answer path will ever record - this
    /// files the wait itself (SPEC llm.history: every round trip, including the ones
    /// that never answered).
    pub fn record_chat_timeout(&self, call: ChatTimeout<'_>) {
        self.record_chat_timeout_scoped(None, call);
    }

    /// The same dropped round trip, associated with the RESERVED run it served:
    /// the row joins its run by id like every other round of it. It settles no
    /// attempt - the outbound may have flown and nothing seals it, so a
    /// conversation's round stays in flight and continuation refuses.
    pub fn record_chat_timeout_in_run(&self, run_id: i64, call: ChatTimeout<'_>) {
        self.record_chat_timeout_scoped(Some(run_id), call);
    }

    pub(super) fn record_chat_timeout_scoped(&self, run_id: Option<i64>, call: ChatTimeout<'_>) {
        let err = CallError {
            // Neutral on purpose: the deadline may have dropped the round trip itself,
            // or a tool call after it - this row says what is certain, the wait.
            message: format!("cancelled at the {} ms deadline with the round trip unanswered", call.waited_ms),
            status: None,
            kind: "timeout",
            retryable: false,
            duration_ms: call.waited_ms,
        };
        self.record_chat_scoped(run_id, ChatCall {
            provider: call.provider,
            model: call.model,
            agent: call.agent,
            system: call.system,
            prompt: call.prompt,
            turn: call.turn,
            // The dropped round trip's request is not known here - the deadline
            // hit before it answered - so no wire facts are claimed for it.
            wire: None,
            request: None,
            replay: None,
            attempt: None,
            outcome: ChatOutcome::Err(&err),
        });
    }

    /// One embedding round trip: the asked texts in full, and the answer as its
    /// width and every vector rounded to six decimals (see [EmbedCall]) - or the error.
    pub fn record_embedding(&self, call: EmbedCall<'_>) {
        let EmbedCall { provider, model, agent, texts, ms, outcome } = call;
        let at = swiss_core::log::iso_now();
        let count = texts.len();
        let chars: usize = texts.iter().map(|text| text.chars().count()).sum();
        let mut body = json!({ "request": { "batch": count, "chars": chars, "texts": texts } });
        let row = match outcome {
            Ok(vectors) => {
                let width = vectors.first().map(|v| v.len()).unwrap_or(0);
                // Every value, rounded to six decimals: the whole answer stays inspectable -
                // drift, dead dimensions, sign flips - without the 17-digit noise f64 printing
                // adds to numbers that were f32 on the wire. A 1024-wide batch is a few KB of
                // text per vector; the caller still holds the floats it got.
                let rounded: Vec<Vec<f64>> = vectors
                    .iter()
                    .map(|v| v.iter().map(|x| (f64::from(*x) * 1e6).round() / 1e6).collect())
                    .collect();
                body["response"] = json!({ "vectors": rounded, "dimensions": width });
                CallRow {
                    at,
                    kind: "embedding",
                    provider: provider.into(),
                    model: model.into(),
                    agent: agent.map(str::to_string),
                    ok: true,
                    ms,
                    status: None,
                    tokens: Tokens::default(),
                    request_id: None,
                }
            }
            Err(err) => {
                body["response"] = json!({ "error": err });
                CallRow {
                    at,
                    kind: "embedding",
                    provider: provider.into(),
                    model: model.into(),
                    agent: agent.map(str::to_string),
                    ok: false,
                    ms,
                    status: None,
                    tokens: Tokens::default(),
                    request_id: None,
                }
            }
        };
        self.submit_call_scoped(None, row, &body.to_string(), None, None);
    }

    /// The queued call write. run_id is the RESERVED run's id when the ask
    /// served one - the explicit relation - and NULL for legacy rows, where the
    /// run's own finish transaction still stamps the window.
    pub(super) fn submit_call_scoped(
        &self,
        run_id: Option<i64>,
        row: CallRow,
        body_text: &str,
        attempt: Option<u64>,
        settle: Option<(i64, i64)>,
    ) {
        let keys = self.keys.clone();
        let body_text = body_text.to_string();
        let estimate = body_text.len() + 512;
        let submitted = self.submit(estimate, move |pool| {
            let row = row.clone();
            let body_text = body_text.clone();
            Box::pin(async move {
                // One transaction: the row, its sealed chunks and (for a
                // terminal round) the token-matched settle commit together -
                // a refused admission or a failed SQL leaves the mark in
                // flight, fail-closed, with no half-written round.
                insert_call(&pool, run_id, &row, &body_text, &keys, attempt.map(|token| token as i64), settle)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        if let Err(e) = submitted {
            swiss_core::log::warn(&format!("llm.db call admission refused: {e}"), None);
        }
    }
}
