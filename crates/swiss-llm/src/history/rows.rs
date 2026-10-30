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

//! The rows and their writes: the call and run columns a recording supplies, the queued
//! inserts with their sealed chunks, and a row as the panel reads it.

use super::*;

/// The call columns one recording supplies.
#[derive(Clone)]
pub(super) struct CallRow {
    pub(super) at: String,
    pub(super) kind: &'static str,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) agent: Option<String>,
    pub(super) ok: bool,
    pub(super) ms: u64,
    pub(super) status: Option<u16>,
    pub(super) tokens: Tokens,
    pub(super) request_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct RunRow {
    pub(super) at: String,
    pub(super) at_end: String,
    pub(super) agent: String,
    pub(super) runtime: String,
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) ok: bool,
    pub(super) ms: u64,
    pub(super) turns: i64,
    /// The conversation's start stamp - the window whose calls the insert stamps.
    pub(super) begun: String,
    /// What the run was for and who started it.
    pub(super) context: RunContext,
}

/// The usage of one reply, as the body and the panel show it.
pub(super) fn tokens_json(tokens: &Tokens) -> Value {
    json!({
        "input": tokens.input,
        "output": tokens.output,
        "cacheRead": tokens.cache_read,
        "cacheWrite": tokens.cache_creation,
    })
}
/// One queued write: the call row plus its sealed body chunks, one transaction - the
/// same shape the mcp owner uses. A chat body splits into its pieces first (see
/// [split_body]); the envelope the call row stores carries only the hashes, and a
/// piece another call already filed is not written again.
pub(super) async fn insert_call(
    pool: &SqlitePool,
    run_id: Option<i64>,
    row: &CallRow,
    body_text: &str,
    keys: &swiss_host::history::seal::ChunkKeys,
    attempt: Option<i64>,
    settle: Option<(i64, i64)>,
) -> Result<(), sqlx::Error> {
    let (preview, chars) = preview_of(body_text);
    let (envelope, pieces) = serde_json::from_str::<Value>(body_text)
        .map(|body| split_body(&body))
        .unwrap_or_else(|_| (Value::String(body_text.to_string()), Vec::new()));
    let envelope_text = envelope.to_string();
    // The WRITER-KNOWN decoded weight (SPEC llm.history): the envelope plus
    // every piece this body references, plain UTF-8 bytes. Pieces are
    // content-addressed and stored once however many rounds re-send them,
    // so the additive reference weight - computed here, where the writer
    // already holds every byte - is what the run's decoded size sums;
    // never a decode-on-read, never a physical-dedup undercount.
    let weight = envelope_text.len() + pieces.iter().map(|piece| piece.text.len()).sum::<usize>();
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO call (at, kind, provider, model, agent, ok, ms, status,          tokens_in, tokens_out, tokens_cache_read, tokens_cache_write, request_id,          preview_text, chars, body_flag, body_plain, run_id, attempt)          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(&row.at)
    .bind(row.kind)
    .bind(&row.provider)
    .bind(&row.model)
    .bind(&row.agent)
    .bind(row.ok as i64)
    .bind(row.ms as i64)
    .bind(row.status.map(|s| s as i64))
    .bind(row.tokens.input as i64)
    .bind(row.tokens.output as i64)
    .bind(row.tokens.cache_read as i64)
    .bind(row.tokens.cache_creation as i64)
    .bind(&row.request_id)
    .bind(&preview)
    .bind(chars as i64)
    .bind(weight as i64)
    .bind(run_id)
    .bind(attempt)
    .execute(&mut *tx)
    .await?;
    let call_id = result.last_insert_rowid();
    for piece in &pieces {
        // The address is the content: a piece another call filed writes nothing -
        // the resent dialogue is stored once, not once per round trip.
        let filed = sqlx::query("INSERT OR IGNORE INTO piece (hash, chars) VALUES (?, ?)")
            .bind(&piece.hash)
            .bind(piece.text.chars().count() as i64)
            .execute(&mut *tx)
            .await?;
        if filed.rows_affected() > 0 {
            write_piece_chunks(&mut tx, &piece.hash, &piece.text, keys).await?;
        }
    }
    write_chunks_tx(&mut tx, "call_body", "call_id", call_id, &envelope_text, keys).await?;
    // A TERMINAL round settles INSIDE this same transaction: the sealed body
    // and the cleared activity mark commit together, never a second pool
    // autocommit that could land between them. Token-matched (a stale
    // completion cannot erase a newer attempt's mark) and None for the
    // rounds whose tool results are still outstanding - those settle only
    // when their authoritative results tail lands (see settle_round).
    if let Some((run_id, attempt)) = settle {
        // The cleared reference leaves NO stale decoded weight behind: the
        // run's decoded size sums tail_plain, so a NULL tail must read zero or
        // a superseded round keeps counting after it is gone.
        sqlx::query("UPDATE run SET pending = 0, tail = NULL, tail_plain = 0 WHERE id = ? AND pending = 1 AND attempt = ?")
            .bind(run_id)
            .bind(attempt)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// The finish of a RESERVED run: update its own row - outcome, wall time and
/// turn count ADDED across sessions - and append this recorder's turns after the
/// ones already there. The row was born at reservation; its calls already carry
/// its id; no window is joined.
pub(super) async fn finish_run(
    pool: &SqlitePool,
    run_id: i64,
    conversation: bool,
    run: &RunRow,
    notes: &[Note],
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    if conversation {
        // Open stays open: turns and wall time accumulate, no outcome is filed.
        sqlx::query("UPDATE run SET ms = ms + ?, turns = turns + ? WHERE id = ?")
            .bind(run.ms as i64)
            .bind(run.turns)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("UPDATE run SET ok = ?, at_end = ?, ms = ms + ?, turns = turns + ? WHERE id = ?")
            .bind(run.ok as i64)
            .bind(&run.at_end)
            .bind(run.ms as i64)
            .bind(run.turns)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
    }
    // A continued conversation appends; the base keeps earlier sessions' seqs.
    let base: i64 = sqlx::query("SELECT COALESCE(MAX(seq), 0) FROM turn WHERE run_id = ?")
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?
        .try_get(0)?;
    for (seq, note) in notes.iter().enumerate() {
        let (preview, chars) = preview_of(&note.text);
        let result = sqlx::query(
            "INSERT INTO turn (run_id, seq, at, role, preview_text, chars, body_flag)              VALUES (?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(run_id)
        .bind(base + seq as i64 + 1)
        .bind(&note.at)
        .bind(&note.role)
        .bind(&preview)
        .bind(chars as i64)
        .execute(&mut *tx)
        .await?;
        let turn_id = result.last_insert_rowid();
        write_chunks_tx(&mut tx, "turn_body", "turn_id", turn_id, &note.text, keys).await?;
    }
    tx.commit().await
}

/// One queued write: the run row plus every turn and its sealed body chunks.
pub(super) async fn insert_run(
    pool: &SqlitePool,
    run: &RunRow,
    notes: &[Note],
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO run (at, at_end, agent, runtime, provider, model, ok, ms, turns, task, subject, source, parent)          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run.at)
    .bind(&run.at_end)
    .bind(&run.agent)
    .bind(&run.runtime)
    .bind(&run.provider)
    .bind(&run.model)
    .bind(run.ok as i64)
    .bind(run.ms as i64)
    .bind(run.turns)
    .bind(&run.context.task)
    .bind(&run.context.subject)
    .bind(&run.context.source)
    .bind(run.context.parent)
    .execute(&mut *tx)
    .await?;
    let run_id = result.last_insert_rowid();
    // The session id lands on every same-agent call inside the conversation's window
    // (runs of one agent are serial in practice - the assumption the design states).
    // Calls filed before the column existed keep their backfilled answer; new calls
    // carry NULL until their run files, and this stamp is the only writer.
    sqlx::query("UPDATE call SET run_id = ? WHERE agent = ? AND run_id IS NULL AND at >= ? AND at <= ?")
        .bind(run_id)
        .bind(&run.agent)
        .bind(&run.begun)
        .bind(&run.at_end)
        .execute(&mut *tx)
        .await?;
    for (seq, note) in notes.iter().enumerate() {
        let (preview, chars) = preview_of(&note.text);
        let result = sqlx::query(
            "INSERT INTO turn (run_id, seq, at, role, preview_text, chars, body_flag)              VALUES (?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(run_id)
        .bind(seq as i64 + 1)
        .bind(&note.at)
        .bind(&note.role)
        .bind(&preview)
        .bind(chars as i64)
        .execute(&mut *tx)
        .await?;
        let turn_id = result.last_insert_rowid();
        write_chunks_tx(&mut tx, "turn_body", "turn_id", turn_id, &note.text, keys).await?;
    }
    tx.commit().await
}

/// One piece's chunk tail: the same shape [write_chunks_tx] lays down, but the
/// rows belong to a hash, not a call id, and the seal binding carries that hash as
/// its resource - a piece opens only as itself.
pub(super) async fn write_piece_chunks(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    hash: &str,
    text: &str,
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    let mut ofs = 0u64;
    for (part, chunk_bytes) in body::chunk(text.as_bytes()).enumerate() {
        let plain = chunk_bytes.len();
        let (codec, coded) = body::encode_chunk(chunk_bytes);
        let binding =
            swiss_host::history::seal::ChunkBinding::row("llm", hash, 0, part as i64, ofs, plain, codec.as_u8());
        let block = keys.seal_block("history:llm:body", &binding, &coded);
        ofs += plain as u64;
        sqlx::query("INSERT INTO piece_body (hash, part, codec, plain, block) VALUES (?, ?, ?, ?, ?)")
            .bind(hash)
            .bind(part as i64)
            .bind(codec.as_u8() as i64)
            .bind(plain as i64)
            .bind(&block)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// A fetched body chunk: (part, codec, plain, block).
pub(super) type ChunkRow = (i64, i64, i64, Vec<u8>);

/// Fetched chunk rows as (part, codec, plain, block), in part order.
pub(super) fn map_chunks(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<ChunkRow>, sqlx::Error> {
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get::<i64, _>(0)?,
                row.try_get::<i64, _>(1)?,
                row.try_get::<i64, _>(2)?,
                row.try_get::<Vec<u8>, _>(3)?,
            ))
        })
        .collect()
}

/// Decode one fetched chunk chain back to text. `resource` names the owner in the
/// seal binding ("" for a body row id, the hash for a piece); a corrupt row (codec
/// out of range, negative length) is gone, never a wrap-around read: try_from
/// refuses the truncation `as` would hide.
pub(super) fn open_chunks(
    rows: Vec<(i64, i64, i64, Vec<u8>)>,
    keys: &swiss_host::history::seal::ChunkKeys,
    resource: &str,
    event: i64,
) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    for (part, codec, plain, block) in rows {
        let codec = u8::try_from(codec).ok().and_then(body::BodyCodec::from_u8)?;
        let plain = plain.max(0) as usize;
        let binding = swiss_host::history::seal::ChunkBinding::row(
            "llm",
            resource,
            event,
            part,
            buf.len() as u64,
            plain,
            codec.as_u8(),
        );
        let bytes = match keys.open_block(&block, &binding) {
            swiss_host::history::seal::OpenBlock::Sealed(bytes) => bytes,
            swiss_host::history::seal::OpenBlock::Plain => block,
            swiss_host::history::seal::OpenBlock::Refused => return None,
        };
        match body::decode_chunk(codec, &bytes, plain) {
            Ok(mut decoded) => buf.append(&mut decoded),
            Err(_) => return None,
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub(super) async fn write_chunks_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
    id_col: &str,
    owner: i64,
    text: &str,
    keys: &swiss_host::history::seal::ChunkKeys,
) -> Result<(), sqlx::Error> {
    // Same audit marker as the reads: {table}/{id_col} are module-owned literals.
    // The marker is per-call (sqlx takes it by value), so the small string clones per
    // chunk - bodies split into a handful of parts at most.
    let insert = format!("INSERT INTO {table} ({id_col}, part, codec, plain, block) VALUES (?, ?, ?, ?, ?)");
    let mut ofs = 0u64;
    for (part, chunk_bytes) in body::chunk(text.as_bytes()).enumerate() {
        let plain = chunk_bytes.len();
        let (codec, coded) = body::encode_chunk(chunk_bytes);
        let binding = swiss_host::history::seal::ChunkBinding::row(
            "llm",
            "",
            owner,
            part as i64,
            ofs,
            plain,
            codec.as_u8(),
        );
        let block = keys.seal_block("history:llm:body", &binding, &coded);
        ofs += plain as u64;
        sqlx::query(sqlx::AssertSqlSafe(insert.clone()))
            .bind(owner)
            .bind(part as i64)
            .bind(codec.as_u8() as i64)
            .bind(plain as i64)
            .bind(&block)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// One run row as the panel reads it (the columns of `run_select!`).
pub(super) fn run_row_to_value(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    Ok(json!({
        "id": row.try_get::<i64, _>(0)?,
        "at": row.try_get::<String, _>(1)?,
        "atEnd": row.try_get::<Option<String>, _>(2)?,
        "agent": row.try_get::<String, _>(3)?,
        "runtime": row.try_get::<String, _>(4)?,
        "provider": row.try_get::<Option<String>, _>(5)?,
        "model": row.try_get::<Option<String>, _>(6)?,
        "ok": row.try_get::<Option<i64>, _>(7)?.map(|v| v == 1),
        "ms": row.try_get::<i64, _>(8)?,
        "turns": row.try_get::<i64, _>(9)?,
        "kind": row.try_get::<Option<i64>, _>(10)?,
        "task": row.try_get::<Option<String>, _>(11)?,
        "subject": row.try_get::<Option<String>, _>(12)?,
        "source": row.try_get::<Option<String>, _>(13)?,
        "parent": row.try_get::<Option<i64>, _>(14)?,
        "head": row.try_get::<Option<String>, _>(15)?,
        "rounds": row.try_get::<i64, _>(16)?,
        "tokensIn": row.try_get::<i64, _>(17)?,
        "tokensCacheRead": row.try_get::<i64, _>(18)?,
        "tokensOut": row.try_get::<i64, _>(19)?,
        "children": row.try_get::<i64, _>(20)?,
    }))
}

/// The tool use of each run in `runs`, as `tools: {name: count}`: one read of
/// the assistant tool-call notes in the runs' id range (`tool call NAME [ID]:
/// ...`, the loop's own format), counted only for the runs asked about.
pub(super) async fn with_tools(pool: &SqlitePool, runs: &mut [Value]) -> Result<(), sqlx::Error> {
    let ids: Vec<i64> = runs.iter().filter_map(|run| run["id"].as_i64()).collect();
    let (Some(&low), Some(&high)) = (ids.iter().min(), ids.iter().max()) else { return Ok(()) };
    let rows = sqlx::query(
        "SELECT run_id, substr(preview_text, 11, 96) FROM turn
         WHERE run_id BETWEEN ? AND ? AND role = 'assistant' AND preview_text LIKE 'tool call %'",
    )
    .bind(low)
    .bind(high)
    .fetch_all(pool)
    .await?;
    let mut tally: std::collections::HashMap<i64, std::collections::BTreeMap<String, i64>> =
        std::collections::HashMap::new();
    for row in &rows {
        let run_id = row.try_get::<i64, _>(0)?;
        let rest = row.try_get::<String, _>(1)?;
        let name = rest.split([' ', ':']).next().unwrap_or("").trim();
        if !name.is_empty() && ids.contains(&run_id) {
            *tally.entry(run_id).or_default().entry(name.to_string()).or_default() += 1;
        }
    }
    for run in runs.iter_mut() {
        let used = run["id"].as_i64().and_then(|id| tally.remove(&id)).unwrap_or_default();
        run["tools"] = json!(used);
    }
    Ok(())
}

/// One call row as the panel reads it.
pub(super) fn call_row_to_value(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    Ok(json!({
        "id": row.try_get::<i64, _>(0)?,
        "at": row.try_get::<String, _>(1)?,
        "kind": row.try_get::<String, _>(2)?,
        "provider": row.try_get::<String, _>(3)?,
        "model": row.try_get::<String, _>(4)?,
        "agent": row.try_get::<Option<String>, _>(5)?,
        "ok": row.try_get::<i64, _>(6)? == 1,
        "ms": row.try_get::<i64, _>(7)?,
        "status": row.try_get::<Option<i64>, _>(8)?,
        "tokensIn": row.try_get::<i64, _>(9)?,
        "tokensOut": row.try_get::<i64, _>(10)?,
        "tokensCacheRead": row.try_get::<i64, _>(11)?,
        "tokensCacheWrite": row.try_get::<i64, _>(12)?,
        "requestId": row.try_get::<Option<String>, _>(13)?,
        "preview": row.try_get::<String, _>(14)?,
        "chars": row.try_get::<i64, _>(15)?,
        "hasBody": row.try_get::<i64, _>(16)? == 1,
        "runId": row.try_get::<Option<i64>, _>(17)?,
    }))
}
