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

//! The reads of [LlmHistory]: the panel's pages of calls and runs, a call's body, a run's
//! dialogue and turns, and the opening of the sealed pieces they are made of.

use super::*;

/// The most runs one job's read lists, newest first.
const SOURCE_RUNS_MAX: i64 = 200;

/// The run select every run read shares (an audited literal per shape: `$tail`
/// is the WHERE and ORDER of the caller, values ride binds): the row, then what
/// a reader needs to tell one run from another without opening it - the head of
/// its first user turn, its chat round trips, the tokens they cost, and how many
/// runs its tool calls started. Column order is [run_row_to_value]'s.
macro_rules! run_select {
    ($tail:literal) => {
        concat!(
            "SELECT id, at, at_end, agent, runtime, provider, model, ok, ms, turns, kind, task, subject, source, parent,
                (SELECT substr(t.preview_text, 1, 400) FROM turn t WHERE t.run_id = run.id AND t.role = 'user' ORDER BY t.seq LIMIT 1),
                (SELECT count(*) FROM call c WHERE c.run_id = run.id AND c.kind = 'chat'),
                (SELECT coalesce(sum(c.tokens_in), 0) FROM call c WHERE c.run_id = run.id),
                (SELECT coalesce(sum(c.tokens_cache_read), 0) FROM call c WHERE c.run_id = run.id),
                (SELECT coalesce(sum(c.tokens_out), 0) FROM call c WHERE c.run_id = run.id),
                (SELECT count(*) FROM run r WHERE r.parent = run.id)
             FROM run ",
            $tail
        )
    };
}

impl LlmHistory {
    /// One newest-first page of calls: the panel wire shape (calls, page, pageSize,
    /// more). `kind` narrows to chat or embedding; `q` matches provider, model and
    /// agent by ASCII-folded contains, never FTS grammar - the same rule the mcp owner
    /// uses.
    pub async fn read_calls(&self, page: i64, page_size: i64, kind: Option<&str>, q: Option<&str>) -> Result<Value, String> {
        self.flush().await;
        let size = page_size.clamp(1, 100) as usize;
        let page = page.max(0) as usize;
        let from = page * size;
        let needle = q.unwrap_or("").trim().to_ascii_lowercase();
        let kind = kind.filter(|k| !k.is_empty()).map(str::to_string);
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let (rows, total) = if needle.is_empty() {
                        let total: usize = match &kind {
                            Some(kind) => {
                                sqlx::query("SELECT count(*) FROM call WHERE kind = ?")
                                    .bind(kind)
                                    .fetch_one(&pool)
                                    .await?
                                    .try_get::<i64, _>(0)? as usize
                            }
                            None => {
                                sqlx::query("SELECT count(*) FROM call")
                                    .fetch_one(&pool)
                                    .await?
                                    .try_get::<i64, _>(0)? as usize
                            }
                        };
                        let rows = sqlx::query(
                            "SELECT id, at, kind, provider, model, agent, ok, ms, status,\n                              tokens_in, tokens_out, tokens_cache_read, tokens_cache_write,\n                              request_id, preview_text, chars, body_flag, run_id,\n                              (SELECT task FROM run WHERE run.id = call.run_id),\n                              (SELECT subject FROM run WHERE run.id = call.run_id)\n                              FROM call\n                              WHERE (kind = ? OR ? = '') ORDER BY id DESC LIMIT ? OFFSET ?",
                        )
                        .bind(kind.clone().unwrap_or_default())
                        .bind(kind.clone().unwrap_or_default())
                        .bind(size as i64)
                        .bind(from as i64)
                        .fetch_all(&pool)
                        .await?;
                        (rows, total)
                    } else {
                        // The needle branch counts first, then fetches ONLY its page:
                        // a broad needle must not materialize the whole filtered table
                        // (2 KiB previews included) on the admin path.
                        let needle_where =
                            "(kind = ? OR ? = '') AND (\n                               instr(lower(provider), ?) > 0\n                            OR instr(lower(model), ?) > 0\n                            OR instr(lower(coalesce(agent, '')), ?) > 0)";
                        let total: i64 = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM call WHERE {needle_where}")))
                            .bind(kind.clone().unwrap_or_default())
                            .bind(kind.clone().unwrap_or_default())
                            .bind(&needle)
                            .bind(&needle)
                            .bind(&needle)
                            .fetch_one(&pool)
                            .await?
                            .try_get::<i64, _>(0)?;
                        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                            "SELECT id, at, kind, provider, model, agent, ok, ms, status,\n                              tokens_in, tokens_out, tokens_cache_read, tokens_cache_write,\n                              request_id, preview_text, chars, body_flag, run_id,\n                              (SELECT task FROM run WHERE run.id = call.run_id),\n                              (SELECT subject FROM run WHERE run.id = call.run_id)\n                              FROM call WHERE {needle_where} ORDER BY id DESC LIMIT ? OFFSET ?",
                        )))
                        .bind(kind.clone().unwrap_or_default())
                        .bind(kind.clone().unwrap_or_default())
                        .bind(&needle)
                        .bind(&needle)
                        .bind(&needle)
                        .bind(size as i64)
                        .bind(from as i64)
                        .fetch_all(&pool)
                        .await?;
                        (rows, total as usize)
                    };
                    // Each call says what its run was for, so a page of calls reads
                    // as work done, not as a wall of request JSON.
                    let calls: Vec<Value> = rows
                        .iter()
                        .map(|row| {
                            let mut call = call_row_to_value(row)?;
                            call["task"] = json!(row.try_get::<Option<String>, _>(18)?);
                            call["subject"] = json!(row.try_get::<Option<String>, _>(19)?);
                            Ok(call)
                        })
                        .collect::<Result<Vec<_>, sqlx::Error>>()?;
                    Ok::<Value, sqlx::Error>(json!({
                        "calls": calls,
                        "page": page,
                        "pageSize": size,
                        "more": total > from + size,
                    }))
                })
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// One call body in full: the chunks reassemble and unseal, a pruned body comes back
    /// flagged gone - never a silently short result.
    pub async fn read_call_body(&self, id: i64) -> Result<Option<Value>, String> {
        self.flush().await;
        let flag = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT body_flag, preview_text FROM call WHERE id = ?")
                        .bind(id)
                        .fetch_optional(&pool)
                        .await?;
                    Ok::<Option<(i64, String)>, sqlx::Error>(
                        row.map(|r| (r.try_get::<i64, _>(0).unwrap_or(0), r.try_get::<String, _>(1).unwrap_or_default())),
                    )
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some(flag) = flag else {
            return Ok(None);
        };
        if flag.0 != 1 {
            return Ok(Some(json!({ "id": id, "body": flag.1, "bodyGone": false })));
        }
        // Some(text) - even empty text - is an opened chain (body false); None is a
        // refused or corrupt one: a row that exists and cannot be read, gone-flagged,
        // never routed as absent (the 404 is for rows that are not there at all).
        match self.body_text("call_body", "call_id", id).await {
            Some(text) => {
                // A v2 envelope names its pieces by hash (see [split_body]); the whole
                // body the reader was promised comes back from them. The pieces open
                // first, as a bag the sync resolve reads; one that will not open says
                // gone - never a silently short body. Anything else is a plain v1 body
                // (a whole ask, an embedding, rows written before the split existed).
                let envelope = serde_json::from_str::<Value>(&text).ok();
                let is_v2 = envelope
                    .as_ref()
                    .and_then(|v| v.get("v").and_then(Value::as_i64))
                    .is_some_and(|v| v == 2);
                if is_v2 {
                    let envelope = envelope.unwrap_or_default();
                    let mut bag = std::collections::HashMap::new();
                    let mut whole = true;
                    for hash in piece_hashes(&envelope) {
                        match self.piece_text(&hash).await {
                            Some(text) => {
                                bag.insert(hash, text);
                            }
                            None => {
                                whole = false;
                                break;
                            }
                        }
                    }
                    if !whole {
                        return Ok(Some(json!({ "id": id, "body": "", "bodyGone": true })));
                    }
                    let body = resolve_body(&envelope, &|hash| bag.get(hash).cloned())
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    if body.is_empty() {
                        return Ok(Some(json!({ "id": id, "body": "", "bodyGone": true })));
                    }
                    return Ok(Some(json!({ "id": id, "body": body, "bodyGone": false })));
                }
                Ok(Some(json!({ "id": id, "body": text, "bodyGone": false })))
            }
            None => Ok(Some(json!({ "id": id, "body": "", "bodyGone": true }))),
        }
    }

    /// One newest-first page of runs. An agent name narrows to that agent's runs;
    /// open=true keeps only OPEN conversations (kind 1, no outcome, no at_end) -
    /// the discovery read, where an empty list is a plain empty page, never a
    /// 404. A pending one-shot reservation hides everywhere public: kind NULL
    /// with no outcome yet is a private row that finish (or nothing) completes.
    pub async fn read_runs(
        &self,
        page: i64,
        page_size: i64,
        agent: Option<String>,
        open: Option<bool>,
    ) -> Result<Value, String> {
        self.flush().await;
        let size = page_size.clamp(1, 100) as usize;
        let page = page.max(0) as usize;
        let from = page * size;
        // The WHERE stays an AUDITED literal per shape - sqlx refuses built
        // strings on purpose - so the four filter combinations each pick their
        // fixed statement and every value rides a bind, never spelled SQL.
        let open_only = open == Some(true);
        // The runs in flight no list shows yet - a one-shot hides until it files: the
        // first page leads with them (SPEC llm.history Live).
        let live = if page == 0 && !open_only { self.live.ids() } else { Vec::new() };
        // A run a tool call started lists under its parent (its sheet's children),
        // never beside it on the unfiltered page; an agent's own page lists them all.
        let (count_sql, select_sql, by_agent) = match (agent.is_some(), open_only) {
            (false, false) => (
                "SELECT count(*) FROM run WHERE (run.kind = 1 OR run.ok IS NOT NULL) AND run.parent IS NULL",
                run_select!("WHERE (run.kind = 1 OR run.ok IS NOT NULL) AND run.parent IS NULL ORDER BY id DESC LIMIT ? OFFSET ?"),
                false,
            ),
            (true, false) => (
                "SELECT count(*) FROM run WHERE run.agent = ? AND (run.kind = 1 OR run.ok IS NOT NULL)",
                run_select!("WHERE run.agent = ? AND (run.kind = 1 OR run.ok IS NOT NULL) ORDER BY id DESC LIMIT ? OFFSET ?"),
                true,
            ),
            (false, true) => (
                "SELECT count(*) FROM run WHERE run.kind = 1 AND run.ok IS NULL AND run.at_end IS NULL",
                run_select!("WHERE run.kind = 1 AND run.ok IS NULL AND run.at_end IS NULL ORDER BY id DESC LIMIT ? OFFSET ?"),
                false,
            ),
            (true, true) => (
                "SELECT count(*) FROM run WHERE run.agent = ? AND run.kind = 1 AND run.ok IS NULL AND run.at_end IS NULL",
                run_select!("WHERE run.agent = ? AND run.kind = 1 AND run.ok IS NULL AND run.at_end IS NULL ORDER BY id DESC LIMIT ? OFFSET ?"),
                true,
            ),
        };
        let mut found = self
            .store
            .query(move |pool| {
                let agent = agent.clone();
                let live = live.clone();
                Box::pin(async move {
                    let mut counted = sqlx::query(count_sql);
                    let mut selected = sqlx::query(select_sql);
                    if by_agent {
                        if let Some(agent) = &agent {
                            counted = counted.bind(agent);
                            selected = selected.bind(agent);
                        }
                    }
                    let total: usize =
                        counted.fetch_one(&pool).await?.try_get::<i64, _>(0)? as usize;
                    let rows = selected
                        .bind(size as i64)
                        .bind(from as i64)
                        .fetch_all(&pool)
                        .await?;
                    let listed: Vec<Value> = rows.iter().map(run_row_to_value).collect::<Result<Vec<_>, sqlx::Error>>()?;
                    // A live one-shot under the page's own filter: its agent's, and on
                    // the unfiltered page only one no tool call started.
                    let mut runs = Vec::new();
                    for id in live.into_iter().filter(|id| !listed.iter().any(|run| run["id"] == *id)) {
                        let Some(row) = sqlx::query(run_select!("WHERE id = ?")).bind(id).fetch_optional(&pool).await? else {
                            continue;
                        };
                        let run = run_row_to_value(&row)?;
                        let fits = match &agent {
                            Some(agent) => run["agent"] == agent.as_str(),
                            None => run["parent"].is_null(),
                        };
                        if fits && run["ok"].is_null() && run["kind"] != 1 {
                            runs.push(run);
                        }
                    }
                    runs.sort_by_key(|run| std::cmp::Reverse(run["id"].as_i64()));
                    runs.extend(listed);
                    with_tools(&pool, &mut runs).await?;
                    Ok::<Value, sqlx::Error>(json!({
                        "runs": runs,
                        "page": page,
                        "pageSize": size,
                        "more": total > from + size,
                    }))
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some(runs) = found["runs"].as_array_mut() {
            self.with_live(runs);
        }
        Ok(found)
    }

    /// One newest-first page of an agent's conversations in the scope in force (its task
    /// and subject, see [super::within]) - a Data connection's History: open, closed and
    /// failed alike, in read_runs' shape (runs, page, pageSize, more).
    pub async fn read_conversations(&self, agent: &str, page: i64, page_size: i64) -> Result<Value, String> {
        self.flush().await;
        let size = page_size.clamp(1, 100) as usize;
        let page = page.max(0) as usize;
        let from = page * size;
        let agent = agent.to_string();
        let scope = super::context().clipped();
        let mut found = self
            .store
            .query(move |pool| {
                let agent = agent.clone();
                let scope = scope.clone();
                Box::pin(async move {
                    let total: usize = sqlx::query(
                        "SELECT count(*) FROM run WHERE run.agent = ? AND run.task IS ? AND run.subject IS ? AND run.kind = 1",
                    )
                    .bind(&agent)
                    .bind(&scope.task)
                    .bind(&scope.subject)
                    .fetch_one(&pool)
                    .await?
                    .try_get::<i64, _>(0)? as usize;
                    let rows = sqlx::query(run_select!(
                        "WHERE run.agent = ? AND run.task IS ? AND run.subject IS ? AND run.kind = 1 ORDER BY id DESC LIMIT ? OFFSET ?"
                    ))
                    .bind(&agent)
                    .bind(&scope.task)
                    .bind(&scope.subject)
                    .bind(size as i64)
                    .bind(from as i64)
                    .fetch_all(&pool)
                    .await?;
                    let mut runs: Vec<Value> = rows.iter().map(run_row_to_value).collect::<Result<Vec<_>, sqlx::Error>>()?;
                    with_tools(&pool, &mut runs).await?;
                    Ok::<Value, sqlx::Error>(json!({
                        "runs": runs,
                        "page": page,
                        "pageSize": size,
                        "more": total > from + size,
                    }))
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some(runs) = found["runs"].as_array_mut() {
            self.with_live(runs);
        }
        Ok(found)
    }

    /// Every run one job started (`source` as its [RunContext] carried it: memory:<sync
    /// run id>, ...), newest first, the running ones with their live facts - the trace from
    /// a job to the agents it asked. A one-shot lists once filed or while live; one a
    /// stopped gateway stranded never filed and stays hidden, as on every list.
    pub async fn read_runs_by_source(&self, source: String) -> Result<Value, String> {
        self.flush().await;
        let live = self.live.ids();
        let mut found = self
            .store
            .query(move |pool| {
                let source = source.clone();
                let live = live.clone();
                Box::pin(async move {
                    let rows = sqlx::query(run_select!("WHERE run.source = ? ORDER BY id DESC LIMIT ?"))
                        .bind(&source)
                        .bind(SOURCE_RUNS_MAX)
                        .fetch_all(&pool)
                        .await?;
                    let mut runs = Vec::new();
                    for row in &rows {
                        let run = run_row_to_value(row)?;
                        let listed = run["kind"] == 1
                            || !run["ok"].is_null()
                            || run["id"].as_i64().is_some_and(|id| live.contains(&id));
                        if listed {
                            runs.push(run);
                        }
                    }
                    with_tools(&pool, &mut runs).await?;
                    Ok::<Value, sqlx::Error>(json!({ "runs": runs }))
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some(runs) = found["runs"].as_array_mut() {
            self.with_live(runs);
        }
        Ok(found)
    }

    /// Every running run of a read wears its live facts as `live` (SPEC llm.history Live):
    /// its turns and tool calls so far join the filed ones, and one with no filed turn yet
    /// is named by the opening of its first ask.
    fn with_live(&self, runs: &mut [Value]) {
        for run in runs.iter_mut() {
            let Some(mut view) = run["id"].as_i64().and_then(|id| self.live.view(id)) else { continue };
            let Some(facts) = view.as_object_mut() else { continue };
            // A list row counts its turns; a dialogue already holds the noted ones.
            let noted = facts.remove("turns").and_then(|n| n.as_i64()).unwrap_or(0);
            if let Some(filed) = run["turns"].as_i64() {
                run["turns"] = json!(filed + noted);
            }
            if let Some(Value::Object(more)) = facts.remove("tools") {
                let mut tools = run["tools"].as_object().cloned().unwrap_or_default();
                for (name, n) in more {
                    let had = tools.get(&name).and_then(Value::as_i64).unwrap_or(0);
                    tools.insert(name, json!(had + n.as_i64().unwrap_or(0)));
                }
                run["tools"] = Value::Object(tools);
            }
            let head = facts.remove("head").unwrap_or(Value::Null);
            if run["head"].is_null() {
                run["head"] = head;
            }
            run["live"] = view;
        }
    }

    /// The provider calls of one run (the conversation's session id on every call,
    /// stamped by the run's own finish transaction; rows older than the column got
    /// the same answer from the schema backfill). Newest last, so they read in
    /// dialogue order.
    pub async fn read_calls_of_run(&self, run_id: i64) -> Result<Option<Value>, String> {
        self.flush().await;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let run = sqlx::query("SELECT id FROM run WHERE id = ?")
                        .bind(run_id)
                        .fetch_optional(&pool)
                        .await?;
                    if run.is_none() {
                        return Ok::<Option<Value>, sqlx::Error>(None);
                    }
                    let rows = sqlx::query("SELECT id, at, kind, provider, model, agent, ok, ms, status, tokens_in, tokens_out, tokens_cache_read, tokens_cache_write, request_id, preview_text, chars, body_flag, run_id FROM call WHERE run_id = ? ORDER BY at")
                    .bind(run_id)
                    .fetch_all(&pool)
                    .await?;
                    let calls: Vec<Value> = rows.iter().map(call_row_to_value).collect::<Result<Vec<_>, _>>()?;
                    Ok(Some(json!({ "calls": calls })))
                })
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// One whole conversation, one fetch: the run, every turn with its FULL text (the
    /// dialogue the reader came for - no per-turn fetch rhythm), and every provider
    /// call of the run with its prompt and answer digested out of the body (the full
    /// wire stays one click away, in the call sheet). Ok(None) = no such run.
    pub async fn read_dialogue(&self, run_id: i64) -> Result<Option<Value>, String> {
        let Some(run) = self.read_turns(run_id).await? else {
            return Ok(None);
        };
        // The turns' bodies, read in one pass: each is one chain of sealed chunks.
        let mut turns = Vec::new();
        for turn in run["turns"].as_array().cloned().unwrap_or_default() {
            let seq = turn["seq"].as_i64().unwrap_or(0);
            let role = turn["role"].as_str().unwrap_or("").to_string();
            let text = if turn["hasBody"].as_bool().unwrap_or(false) {
                self.read_turn_body(run_id, seq)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v["body"].as_str().map(str::to_string))
                    .unwrap_or_default()
            } else {
                turn["preview"].as_str().unwrap_or("").to_string()
            };
            turns.push(json!({
                "seq": seq,
                "at": turn["at"],
                "role": role,
                "text": text,
            }));
        }
        // The calls with their ask and answer: the body's curated fields, not the
        // whole wire - the sheet shows the dialogue, the call sheet shows the bytes.
        // A loop's every call repeats the ask's prompt, so a call carries only its
        // opening (DIGEST_OPENING chars): the whole prompt is the run's user turn
        // once, and a 128 KiB prompt over 24 rounds stays out of the reply 24 times.
        let opening = |text: &str| text.chars().take(DIGEST_OPENING).collect::<String>();
        let mut calls = Vec::new();
        if let Some(found) = self.read_calls_of_run(run_id).await? {
            for call in found["calls"].as_array().cloned().unwrap_or_default() {
                let id = call["id"].as_i64().unwrap_or(0);
                let mut with_digest = call.clone();
                if let Some(preview) = call["preview"].as_str() {
                    with_digest["preview"] = json!(opening(preview));
                }
                if let Some(body) = self.read_call_body(id).await? {
                    if let Some(parsed) = body["body"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()) {
                        if let Some(prompt) = parsed.pointer("/request/prompt").and_then(Value::as_str) {
                            with_digest["prompt"] = json!(opening(prompt));
                        }
                        if let Some(turn) = parsed.pointer("/request/turn").and_then(Value::as_i64) {
                            with_digest["turn"] = json!(turn);
                        }
                        if let Some(answer) = parsed.pointer("/response/texts").and_then(Value::as_array) {
                            with_digest["answer"] = json!(answer);
                        }
                        if let Some(stop) = parsed.pointer("/response/stopReason").and_then(Value::as_str) {
                            with_digest["stopReason"] = json!(stop);
                        }
                        if let Some(ttft) = parsed.pointer("/response/ttftMs").and_then(Value::as_u64) {
                            with_digest["ttftMs"] = json!(ttft);
                        }
                        if let Some(tool_calls) = parsed.pointer("/response/toolCalls").and_then(Value::as_array) {
                            with_digest["toolCalls"] = json!(tool_calls);
                        }
                    }
                }
                calls.push(with_digest);
            }
        }
        // A running run's turns so far follow the filed ones (SPEC llm.history Live): the
        // recorder ends the live entry before it files, so no turn reads twice.
        if let Some(noted) = self.live.notes(run_id) {
            let next = turns.iter().filter_map(|turn| turn["seq"].as_i64()).max().map_or(0, |last| last + 1);
            for (seq, (at, role, text)) in (next..).zip(noted) {
                turns.push(json!({ "seq": seq, "at": at, "role": role, "text": text }));
            }
        }
        let mut whole = run.clone();
        whole["turns"] = Value::Array(turns);
        whole["calls"] = Value::Array(calls);
        self.with_live(std::slice::from_mut(&mut whole));
        if let Some(children) = whole["children"].as_array_mut() {
            self.with_live(children);
        }
        Ok(Some(whole))
    }

    /// One run's turns, each with its preview. Ok(None) = no such run; Err = the
    /// read itself failed (a 500, never a dressed-up 404). The full texts live in
    /// read_dialogue, which reads every body in one pass.
    pub async fn read_turns(&self, run_id: i64) -> Result<Option<Value>, String> {
        self.flush().await;
        self.store
            .query(move |pool| {
                Box::pin(async move {
                    let run = sqlx::query(run_select!("WHERE id = ?"))
                        .bind(run_id)
                        .fetch_optional(&pool)
                        .await?;
                    let Some(run) = run else {
                        return Ok::<Option<Value>, sqlx::Error>(None);
                    };
                    let rows = sqlx::query(
                        "SELECT seq, at, role, preview_text, chars, body_flag FROM turn WHERE run_id = ? ORDER BY seq",
                    )
                    .bind(run_id)
                    .fetch_all(&pool)
                    .await?;
                    let turns: Vec<Value> = rows
                        .iter()
                        .map(|row| {
                            Ok(json!({
                                "seq": row.try_get::<i64, _>(0)?,
                                "at": row.try_get::<String, _>(1)?,
                                "role": row.try_get::<String, _>(2)?,
                                "preview": row.try_get::<String, _>(3)?,
                                "chars": row.try_get::<i64, _>(4)?,
                                "hasBody": row.try_get::<i64, _>(5)? == 1,
                            }))
                        })
                        .collect::<Result<Vec<_>, sqlx::Error>>()?;
                    let mut whole = vec![run_row_to_value(&run)?];
                    with_tools(&pool, &mut whole).await?;
                    let mut whole = whole.pop().unwrap_or_default();
                    // Who started it, and what it started: the parent's own line and
                    // every run its tool calls began, oldest first.
                    if let Some(parent) = whole["parent"].as_i64() {
                        let row = sqlx::query(run_select!("WHERE id = ?"))
                            .bind(parent)
                            .fetch_optional(&pool)
                            .await?;
                        if let Some(row) = row {
                            whole["parentRun"] = run_row_to_value(&row)?;
                        }
                    }
                    let rows = sqlx::query(run_select!("WHERE parent = ? ORDER BY id"))
                        .bind(run_id)
                        .fetch_all(&pool)
                        .await?;
                    let mut children = rows.iter().map(run_row_to_value).collect::<Result<Vec<_>, sqlx::Error>>()?;
                    with_tools(&pool, &mut children).await?;
                    whole["children"] = Value::Array(children);
                    whole["turns"] = Value::Array(turns);
                    Ok(Some(whole))
                })
            })
            .await
            // A failed query is an error said out loud (a 500 on the route); only an
            // absent run is a 404 - the same honesty split the list reads made.
            .map_err(|e| e.to_string())
    }

    /// One turn body in full. Ok(None) = no such turn; Err = the read failed.
    pub async fn read_turn_body(&self, run_id: i64, seq: i64) -> Result<Option<Value>, String> {
        self.flush().await;
        let id = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row =
                        sqlx::query("SELECT id, body_flag, preview_text FROM turn WHERE run_id = ? AND seq = ?")
                            .bind(run_id)
                            .bind(seq)
                            .fetch_optional(&pool)
                            .await?;
                    Ok::<Option<(i64, i64, String)>, sqlx::Error>(row.map(|r| {
                        (
                            r.try_get::<i64, _>(0).unwrap_or(0),
                            r.try_get::<i64, _>(1).unwrap_or(0),
                            r.try_get::<String, _>(2).unwrap_or_default(),
                        )
                    }))
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some(id) = id else {
            return Ok(None);
        };
        // The same twin rule as the call body: a row without chunks answers its
        // preview; an OPENED chain answers its text even when that text is empty (an
        // assistant turn that was all tool calls - the mainline middle of a loop);
        // only a chain that will not open says gone (SPEC llm.api: gone flagged,
        // never a silently short result).
        if id.1 != 1 {
            return Ok(Some(json!({ "runId": run_id, "seq": seq, "body": id.2, "bodyGone": false })));
        }
        match self.body_text("turn_body", "turn_id", id.0).await {
            Some(text) => Ok(Some(json!({ "runId": run_id, "seq": seq, "body": text, "bodyGone": false }))),
            None => Ok(Some(json!({ "runId": run_id, "seq": seq, "body": "", "bodyGone": true }))),
        }
    }

    /// Reassemble, unseal and decode one owner body table chunk chain. A refused or
    /// pruned chain answers None - gone, never a silently short result.
    pub(super) async fn body_text(&self, table: &'static str, id_col: &'static str, owner: i64) -> Option<String> {
        let rows = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    // The interpolated names are this module own table and column
                    // literals, never caller data; every value still binds. AssertSqlSafe
                    // is the sqlx 0.9 audit marker the tree already uses (pg.rs, seed.rs).
                    let sql = format!("SELECT part, codec, plain, block FROM {table} WHERE {id_col} = ? ORDER BY part");
                    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owner).fetch_all(&pool).await?;
                    map_chunks(&rows)
                })
            })
            .await
            .ok()?;
        open_chunks(rows, &self.keys, "", owner)
    }

    /// Decode the sealed tail reference - the CANONICAL read path (SPEC
    /// llm.history). The run column carries hashes; each piece opens
    /// authenticated, and the exact filed value rebuilds. Errors - a corrupt
    /// reference, a missing or seal-failing piece, an undecodable entry -
    /// propagate as Err so the callers refuse, never silently skip. A raw
    /// column whose entries are OBJECTS is a legacy pre-sealing plaintext
    /// tail (already on disk in that shape): it reads back as-is, audit
    /// preserved. Ok(None) = no tail at all.
    pub(super) async fn decode_tail(&self, raw: Option<&str>) -> Result<Option<Value>, String> {
        let Some(raw) = raw else { return Ok(None) };
        let parsed: Value = serde_json::from_str(raw)
            .map_err(|_| "the run's tail reference did not decode".to_string())?;
        let Some(tools) = parsed.get("tools").and_then(Value::as_array) else {
            return Err("the run's tail reference carries no tools list".to_string());
        };
        if tools.first().is_some_and(|tool| tool.is_object()) {
            return Ok(Some(parsed));
        }
        let mut opened = Vec::with_capacity(tools.len());
        for hash in tools {
            let hash = hash
                .as_str()
                .ok_or("the run's tail reference names a piece that is not a hash")?
                .to_string();
            let text = self
                .piece_text(&hash)
                .await
                .ok_or("a sealed tail piece is missing or failed its seal")?;
            opened.push(
                serde_json::from_str::<Value>(&text)
                    .map_err(|_| "a sealed tail piece did not decode".to_string())?,
            );
        }
        Ok(Some(json!({ "tools": opened })))
    }

    /// One content-addressed piece's chain (see [split_body]): the rows the hash
    /// addresses, sealed under the hash itself as the binding resource. None =
    /// refused, pruned or corrupt - the caller says gone, never short.
    pub(super) async fn piece_text(&self, hash: &str) -> Option<String> {
        let key = hash.to_string();
        let rows = self
            .store
            .query(move |pool| {
                let key = key.clone();
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT part, codec, plain, block FROM piece_body WHERE hash = ? ORDER BY part",
                    )
                    .bind(&key)
                    .fetch_all(&pool)
                    .await?;
                    map_chunks(&rows)
                })
            })
            .await
            .ok()?;
        open_chunks(rows, &self.keys, hash, 0)
    }
}
