/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The Data agent (SPEC §data.agent): a `db` agent - the runtime made for this page and run
//! by nothing else - working the one connection the Data page has open, MySQL, PostgreSQL,
//! MongoDB or Redis. The conversation opens under the runtime's DB_AGENT_PROMPT, the agent's
//! own prompt after it, and a snapshot of the connection; its tools are that kind's (DbKind)
//! and reach that connection only: find and describe what holds the data, run ONE read - a
//! statement in a read-only transaction, a read command document, a read command line - whose
//! full result streams to the page as a `db` frame while the model reads its first rows or
//! documents, and stage a write in the console for the reader to run. Redis adds its streams
//! (a window newest first, the consumer groups) and its pub/sub (the channels with a
//! subscriber, and a few seconds' listening), which no key scan shows. Every kind keeps a
//! memory (llm_db_memory): the opening carries its index, and the agent reads, saves and
//! deletes notes on where the connection's data lives and how to reach it.
//!
//! Each connection keeps its own conversation: the run is filed under the task
//! `data.agent` with the connection as its subject, and the one-open rule, a continuation
//! and the open lookup all key on that scope, so two connections never share a dialogue
//! and the agent's own Test never joins one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use swiss_host::dbbrowser::{pubsub_watch, BrowserFlavor, DbBrowser, RedisBrowser, WriteRefusal};
use swiss_host::mongobrowser::MongoBrowser;
use swiss_host::reply::admin_error;
use swiss_host::services::catalog::{CatalogRegistry, ConnectionLease};
use swiss_host::services::RuntimeServices;
use swiss_llm::agent::{db_tools_on, db_tools_shared, AgentConfig, Effort, Runtime, ToolAnswer, ToolSpec, Toolbox};
use swiss_mcp::adapters::mongo::relaxed_of;
use swiss_mcp::adapters::sql::assert_read_statement;
use swiss_llm::conversation::{self, LaneError, COMPACT_FRAMING, SNAPSHOT_HEADING};
use swiss_llm::history::{within, CancelFlag, LlmHistory, ReplayState, RunContext, Steer};
use swiss_llm::provider::{Chat, ModelKind};
use swiss_llm::LlmState;

use crate::llm_api::{awaited_sink, bad, body_of, clip, field, loop_answer, ok, refuse, sse_of, sse_send, LOOP_TIMEOUT, SSE_FRAMES};
use crate::llm_db_memory::DbMemory;

/// The task a Data conversation files under; its subject is the connection.
pub const TASK: &str = "data.agent";
/// The lease holder name the catalog sees.
const HOLDER: &str = "data.agent";
/// Rows a run_sql fetches when the statement sets no LIMIT of its own and the page named
/// no row limit; the page sends its console's (rowLimit), so a run brings back what the
/// console would. The server's ceiling (MAX_ROW_LIMIT) holds either way.
const RUN_LIMIT: i64 = 200;
/// How many of a result's rows the model reads back, each cell clipped; the page shows
/// them all.
const ROWS_TO_MODEL: usize = 50;
const CELL_MAX: usize = 200;
/// The longest line one document takes in what the model reads of a run_mongo.
const DOC_LINE_MAX: usize = 1_000;
/// The COUNT one scan_keys page asks SCAN for first, and the keys it walks on to hold.
const KEYS_SCANNED: i64 = 200;
/// The entries one read_stream brings when the model names no count, and the most it may.
const STREAM_ENTRIES: i64 = 20;
const STREAM_ENTRIES_MAX: i64 = 100;
/// The channels the model reads by name from list_channels, and the snapshot names.
const CHANNELS_TO_MODEL: usize = 200;
const CHANNELS_NAMED: usize = 20;
/// How long listen_channels listens when the model names no time, and the longest it may: the
/// loop waits on it, and pub/sub keeps nothing, so a long wait only makes the reader wait.
const LISTEN_SECONDS: u64 = 5;
const LISTEN_SECONDS_MAX: u64 = 15;
/// The messages a listen reads when the model names no count, and the most it may.
const LISTEN_MESSAGES: i64 = 20;
const LISTEN_MESSAGES_MAX: i64 = 50;
/// How many times a listen opens its feed when the connect or the SUBSCRIBE times out: a link
/// busy with another feed's flood (a tunnel carrying a wide pattern's messages) can hold a new
/// connection past its 5 s, and the next try usually gets through.
const LISTEN_OPEN_TRIES: u32 = 3;
/// A message payload as the model reads it, in characters.
const PAYLOAD_SHOWN: usize = 300;
/// The most databases a MongoDB snapshot names.
const DATABASES_NAMED: usize = 50;
/// The ceiling on any one tool answer the model reads.
const ANSWER_TEXT_MAX: usize = 12_000;
/// The most tables the opening snapshot names: a database with more is counted there, never
/// listed - a thousand names would weigh down every request of the conversation - and
/// list_tables finds them by name. And how many list_tables answers.
const TABLES_NAMED: usize = 100;
const TABLES_LISTED: i64 = 500;
/// The longest statement or command a tool takes, and the longest prompt (a pasted query fits).
const SQL_MAX: usize = 20_000;
const PROMPT_MAX: usize = 8_000;
/// The longest focus a compaction takes, and how many conversations a History page lists.
const FOCUS_MAX: usize = 2_000;
const HISTORY_PAGE: i64 = 20;
/// The one answer to a second send on a connection the agent is already working.
const BUSY: &str = "this agent is already working on this connection";

/// What a compaction asks after the conversation it condenses (SPEC §data.agent Compact):
/// a checkpoint in fixed sections that carries a database dialogue on without its turns.
pub const DB_COMPACT_INSTRUCTION: &str = "Condense the conversation ABOVE into a checkpoint that lets you carry this dialogue on with no loss of what matters: the turns themselves will be gone, the checkpoint stays in their place.

Output EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write \"(none)\" for an empty section - never drop a section.

## Requests and Intent
- [what the reader asked, first to latest; quote them where the exact wording matters]

## Database Facts
- [the tables, columns, keys, types, row counts and value shapes learned so far; the dialect quirks met]

## Statements
- [each statement that mattered, exactly as run, in a sql block, and what its result showed - counts, shapes, the conclusion; never the rows themselves]

## Errors and Fixes
- [an error a statement met, quoted exactly, and how it was fixed]

## Pending
- [what the reader asked that is not done yet]

## Current Work
- [precisely where the dialogue stood: the last question and the answer it got]

## Critical Context
- [the reader's corrections and preferences, decisions and their reasons, constraints, open questions]

Rules:
- Keep exact table and column names, statements, error strings and numbers.
- Write no row values beyond the few a later statement needs (an id, a type name).
- Write in the language the reader writes in.
- Do NOT mention this request or that the conversation was condensed.
- Output only the checkpoint text: call no tool.
- If the conversation opens with an earlier checkpoint, do not copy it forward verbatim: keep what is still true, drop what is stale and merge the newer turns into one checkpoint under the same structure.";

/// What a dialect needs said before the model writes for it.
fn dialect_notes(dialect: &str) -> &'static str {
    match dialect {
        "pg" => "PostgreSQL. Double-quote an identifier that needs it; in LIKE, _ and % are wildcards (escape them with a backslash: 'custom\\_%'); top-N per group is ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) filtered in an outer query; a table outside the search_path is schema-qualified.",
        "mysql" => "MySQL. Backtick an identifier that needs it; in LIKE, _ and % are wildcards (escape them with a backslash); top-N per group is ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) on MySQL 8, filtered in an outer query; a table in another database is db.table.",
        "mongo" => "MongoDB. run_mongo takes a database command document, never shell syntax, its first key naming the command: {\"find\": \"posts\", \"filter\": {...}, \"projection\": {...}, \"sort\": {...}, \"limit\": 20}, {\"aggregate\": \"posts\", \"pipeline\": [...]}, {\"count\": \"posts\", \"query\": {...}}, {\"distinct\": \"posts\", \"key\": \"type\"}. Values are Extended JSON: {\"$oid\": \"...\"} for an ObjectId, {\"$date\": \"2026-01-01T00:00:00Z\"} for a date. A $regex is a regular expression: escape . * + ? ( ) [ ] and the like to match them literally; top-N per group is an aggregate - $sort, then $group with $push and $slice ($topN on 5.2+), or $setWindowFields; a find without a limit brings back the page's row limit at most. Every tool takes db, defaulting to the database in view.",
        "redis" => "Redis. run_redis takes ONE command line as the console reads it (GET user:1, HGETALL post:9, ZRANGE board 0 9 REV WITHSCORES), an argument with spaces in quotes. Never KEYS: scan_keys, or SCAN with MATCH and COUNT. The connection sits on one logical database and SELECT is refused; writes (SET, DEL, EXPIRE, ...) and scripts are staged. A stream is a key of type stream (scan_keys with type stream finds them; read_stream reads one and its consumer groups). A pub/sub channel is no key: list_channels names those with a subscriber, listen_channels hears them for a few seconds; SUBSCRIBE and XREAD are never run as a command line.",
        _ => "SQL.",
    }
}

/// The kinds of connection the Data agent works on, each with its own tools
/// (swiss-llm's db_tools_on names them by [DbKind::as_str]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DbKind {
    Sql,
    Mongo,
    Redis,
}

impl DbKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DbKind::Sql => "sql",
            DbKind::Mongo => "mongo",
            DbKind::Redis => "redis",
        }
    }
}

/// One connection's browser, of whichever kind it is.
enum Browser {
    Sql(Arc<dyn DbBrowser>),
    Mongo(Arc<dyn MongoBrowser>),
    Redis(Arc<dyn RedisBrowser>),
}

impl Browser {
    fn kind(&self) -> DbKind {
        match self {
            Browser::Sql(_) => DbKind::Sql,
            Browser::Mongo(_) => DbKind::Mongo,
            Browser::Redis(_) => DbKind::Redis,
        }
    }
}

/// Lease the connection for one call and take its browser; the lease is the point
/// (SPEC §host.seats): it stays in the caller's scope until the browser call ends, so a
/// draining provider waits for it, and nothing holds it between calls.
fn lease(catalog: &CatalogRegistry, conn: &str) -> Result<(ConnectionLease, Browser), String> {
    let lease = catalog.lease(conn, HOLDER).map_err(|e| e.to_string())?;
    let browser = match lease.flavor() {
        BrowserFlavor::Db(db) => Browser::Sql(db.clone()),
        BrowserFlavor::Mongo(mongo) => Browser::Mongo(mongo.clone()),
        BrowserFlavor::Redis(redis) => Browser::Redis(redis.clone()),
        BrowserFlavor::None => {
            return Err(format!(
                "{conn} is a {} connection; the Data agent works on MySQL, PostgreSQL, MongoDB and Redis",
                lease.dialect()
            ))
        }
    };
    Ok((lease, browser))
}

/// A connection that is no longer of the kind its conversation's tools speak (it was
/// reconfigured since): the call is refused, never run in another language.
fn not_of(conn: &str, kind: DbKind) -> String {
    let named = match kind {
        DbKind::Sql => "a MySQL or PostgreSQL",
        DbKind::Mongo => "a MongoDB",
        DbKind::Redis => "a Redis",
    };
    format!("{conn} is no longer {named} connection; start a new conversation")
}

/// Whether a name passes a list's grep, the grammar list_tables speaks: `a,b` both, `a|b`
/// either, `*` a wildcard that anchors the term ("user*" starts with user), else a
/// substring; case-insensitive. An empty grep passes everything.
fn grep_passes(grep: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    grep.split(',').map(str::trim).filter(|term| !term.is_empty()).all(|term| {
        term.split('|').map(str::trim).filter(|alt| !alt.is_empty()).any(|alt| {
            let alt = alt.to_lowercase();
            if alt.contains('*') { wildcard(&alt, &name) } else { name.contains(&alt) }
        })
    })
}

/// A whole-name match where `*` stands for any run of characters.
fn wildcard(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// A row as one compact JSON line, every long string clipped.
fn row_line(columns: &[String], row: &Value) -> String {
    let cells: Vec<Value> = columns
        .iter()
        .map(|column| match &row[column.as_str()] {
            Value::String(text) => Value::String(clip(text, CELL_MAX)),
            other => {
                let shown = other.to_string();
                if shown.chars().count() > CELL_MAX { Value::String(clip(&shown, CELL_MAX)) } else { other.clone() }
            }
        })
        .collect();
    Value::Array(cells).to_string()
}

/// What the model reads of a run_sql reply: the count, the columns, the first rows.
fn result_text(reply: &Value) -> String {
    let columns: Vec<String> = reply["columns"]
        .as_array()
        .map(|cols| cols.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let rows = reply["rows"].as_array().cloned().unwrap_or_default();
    let mut text = format!("{} row(s)", rows.len());
    if let Some(note) = reply["note"].as_str() {
        text.push_str(&format!(" ({note})"));
    }
    text.push_str(". The whole result is open on the user's page.");
    if rows.is_empty() {
        return text;
    }
    text.push_str(&format!("\nColumns: {}", columns.join(", ")));
    let shown = rows.len().min(ROWS_TO_MODEL);
    if shown < rows.len() {
        text.push_str(&format!("\nThe first {shown}, one JSON array per row:"));
    } else {
        text.push_str("\nRows, one JSON array each:");
    }
    for row in rows.iter().take(shown) {
        text.push('\n');
        text.push_str(&row_line(&columns, row));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of a write run_sql ran: committed, the rows it changed, the id an
/// insert took, and any rows it returned.
fn written_text(reply: &Value) -> String {
    let changed = reply["affectedRows"].as_u64().unwrap_or(0);
    let mut text = format!("It ran and was committed: {changed} row(s) changed");
    if let Some(id) = reply["insertId"].as_u64() {
        text.push_str(&format!(", the insert took id {id}"));
    }
    text.push('.');
    if reply["rows"].as_array().is_some_and(|rows| !rows.is_empty()) {
        text.push_str(" It returned ");
        text.push_str(&result_text(reply));
    }
    text
}

/// What the model reads of a table listing: one table a line.
fn tables_text(listing: &Value) -> String {
    let tables = listing["tables"].as_array().cloned().unwrap_or_default();
    let total = listing["total"].as_i64().unwrap_or(tables.len() as i64);
    if tables.is_empty() {
        return "No table matches.".into();
    }
    let mut text = format!("{} of {total} table(s): schema.name, type, about how many rows", tables.len());
    for table in &tables {
        let rows = table["approxRows"].as_i64().map(|n| format!("~{n} rows")).unwrap_or_else(|| "rows unknown".into());
        text.push_str(&format!(
            "\n{}.{}  {}  {rows}",
            table["schema"].as_str().unwrap_or_default(),
            table["name"].as_str().unwrap_or_default(),
            table["type"].as_str().unwrap_or_default(),
        ));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of a table's structure: its columns, indexes and foreign keys.
fn structure_text(detail: &Value) -> String {
    let mut text = format!("{}.{}", detail["schema"].as_str().unwrap_or_default(), detail["table"].as_str().unwrap_or_default());
    text.push_str("\nColumns:");
    for column in detail["columns"].as_array().into_iter().flatten() {
        let mut line = format!("\n- {} {}", column["name"].as_str().unwrap_or_default(), column["dataType"].as_str().unwrap_or_default());
        if column["isPrimaryKey"] == true {
            line.push_str(" PRIMARY KEY");
        }
        if column["nullable"] == false {
            line.push_str(" NOT NULL");
        }
        if let Some(default) = column["defaultValue"].as_str() {
            line.push_str(&format!(" DEFAULT {}", clip(default, 80)));
        }
        if let Some(comment) = column["comment"].as_str().filter(|c| !c.is_empty()) {
            line.push_str(&format!("  -- {}", clip(comment, 160)));
        }
        text.push_str(&line);
    }
    let indexes = detail["indexes"].as_array().cloned().unwrap_or_default();
    if !indexes.is_empty() {
        text.push_str("\nIndexes:");
        for index in &indexes {
            let columns: Vec<&str> = index["columns"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let kind = if index["primary"] == true { " primary" } else if index["unique"] == true { " unique" } else { "" };
            text.push_str(&format!("\n- {} ({}){kind}", index["name"].as_str().unwrap_or_default(), columns.join(", ")));
        }
    }
    let keys = detail["foreignKeys"].as_array().cloned().unwrap_or_default();
    if !keys.is_empty() {
        text.push_str("\nForeign keys:");
        for key in &keys {
            text.push_str(&format!(
                "\n- {} -> {}.{}({})",
                key["column"].as_str().unwrap_or_default(),
                key["refSchema"].as_str().unwrap_or_default(),
                key["refTable"].as_str().unwrap_or_default(),
                key["refColumn"].as_str().unwrap_or_default(),
            ));
        }
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of a database's collections: one a line, those the grep passes.
fn collections_text(db: &str, listing: &Value, grep: Option<&str>) -> String {
    let all = listing["collections"].as_array().cloned().unwrap_or_default();
    let found: Vec<&Value> = all
        .iter()
        .filter(|c| grep.is_none_or(|grep| grep_passes(grep, c["name"].as_str().unwrap_or_default())))
        .collect();
    if found.is_empty() {
        return format!("No collection in {db} matches.");
    }
    let shown = found.len().min(TABLES_LISTED as usize);
    let mut text = format!("{shown} of {} collection(s) in {db}: name, type, about how many documents", found.len());
    for collection in found.iter().take(shown) {
        let docs = match collection["count"].as_i64() {
            Some(n) => format!("  ~{n} docs"),
            None if collection["type"] == "view" => format!("  on {}", collection["viewOn"].as_str().unwrap_or("?")),
            None => String::new(),
        };
        text.push_str(&format!(
            "\n{}  {}{docs}",
            collection["name"].as_str().unwrap_or_default(),
            collection["type"].as_str().unwrap_or("collection"),
        ));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// A value with every long string clipped, for the model's reading.
fn clipped(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(clip(text, CELL_MAX)),
        Value::Array(items) => Value::Array(items.iter().map(clipped).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(key, v)| (key.clone(), clipped(v))).collect()),
        other => other.clone(),
    }
}

/// What the model reads of a run_mongo answer: a cursor command's document count and first
/// documents in relaxed Extended JSON, one a line; any other command's reply.
fn documents_text(answer: &Value) -> String {
    let reply = &answer["reply"];
    let Some(docs) = reply["cursor"]["firstBatch"].as_array() else {
        let shown = clipped(&relaxed_of(reply));
        return clip(&format!("The command answered (the whole reply is open on the user's page):\n{shown}"), ANSWER_TEXT_MAX);
    };
    let mut text = format!("{} document(s)", docs.len());
    if answer["more"] == true {
        text.push_str(", and more past the row limit");
    }
    text.push_str(". The whole result is open on the user's page.");
    if docs.is_empty() {
        return text;
    }
    let shown = docs.len().min(ROWS_TO_MODEL);
    if shown < docs.len() {
        text.push_str(&format!("\nThe first {shown}, one JSON document per line:"));
    } else {
        text.push_str("\nDocuments, one JSON document per line:");
    }
    for doc in docs.iter().take(shown) {
        text.push('\n');
        text.push_str(&clip(&clipped(&relaxed_of(doc)).to_string(), DOC_LINE_MAX));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of one page of a key scan: where the scan stands first, then one
/// key a line.
fn keys_text(page: &Value) -> String {
    let keys = page["keys"].as_array().cloned().unwrap_or_default();
    let mut text = format!("{} key(s) on this page of the scan", keys.len());
    if let Some(total) = page["total"].as_i64() {
        text.push_str(&format!(" (the database holds {total})"));
    }
    if page["done"] == true {
        text.push_str("; the scan is complete.");
    } else {
        text.push_str(&format!(
            "; it goes on: call scan_keys again with cursor \"{}\" and the same pattern and type.",
            page["cursor"].as_str().unwrap_or("0")
        ));
    }
    if keys.is_empty() {
        return text;
    }
    text.push_str("\nkey, type, TTL:");
    for key in &keys {
        let ttl = match key["ttl"].as_i64() {
            Some(seconds) if seconds >= 0 => format!("expires in {seconds}s"),
            _ => "no TTL".into(),
        };
        text.push_str(&format!("\n{}  {}  {ttl}", key["key"].as_str().unwrap_or_default(), key["type"].as_str().unwrap_or_default()));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of a stream window: the stream's length and ends, the entries newest
/// first one a line, where an older page starts, then the consumer groups.
fn stream_text(window: &Value, groups: Option<&Value>) -> String {
    let key = window["key"].as_str().unwrap_or_default();
    let entries = window["entries"].as_array().cloned().unwrap_or_default();
    let id = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(|| "none".into());
    let mut text = format!(
        "Stream {key}: {} entries, first id {}, last id {}.",
        window["length"].as_i64().unwrap_or(0),
        id(&window["firstId"]),
        id(&window["lastId"])
    );
    if let Some(scanned) = window["scanned"].as_i64() {
        text.push_str(&format!(
            "\n{} matching entr(ies) among the {scanned} examined, from {} back to {}.",
            entries.len(),
            id(&window["scannedFrom"]),
            id(&window["scannedTo"])
        ));
    }
    if entries.is_empty() {
        text.push_str("\nNo entry in this window.");
    } else {
        text.push_str(&format!("\n{} entr(ies), newest first, one a line - id, then field=value:", entries.len()));
        for entry in &entries {
            let mut line = entry["id"].as_str().unwrap_or_default().to_string();
            for (field, value) in entry["fields"].as_object().into_iter().flatten() {
                let value = match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                line.push_str(&format!("  {field}={}", clip(&value, CELL_MAX)));
            }
            text.push('\n');
            text.push_str(&clip(&line, DOC_LINE_MAX));
        }
    }
    if window["more"] == true {
        // A filtered walk goes on from the oldest id it examined, a plain window from its oldest entry.
        let next = window["scannedTo"].as_str().or_else(|| entries.last().and_then(|entry| entry["id"].as_str())).unwrap_or_default();
        text.push_str(&format!("\nOlder entries go on: call read_stream again with before \"{next}\"."));
    }
    match groups.and_then(|groups| groups["groups"].as_array()) {
        Some(rows) if !rows.is_empty() => {
            text.push_str(&format!("\nConsumer groups ({}): name, consumers, pending, lag, last delivered id:", rows.len()));
            for row in rows {
                let lag = row["lag"].as_i64().map_or("unknown".to_string(), |lag| lag.to_string());
                text.push_str(&format!(
                    "\n{}  {}  {}  {lag}  {}",
                    row["name"].as_str().unwrap_or_default(),
                    row["consumers"].as_i64().unwrap_or(0),
                    row["pending"].as_i64().unwrap_or(0),
                    id(&row["last-delivered-id"])
                ));
            }
        }
        Some(_) => text.push_str("\nNo consumer group reads it."),
        None => text.push_str("\nIts consumer groups could not be read."),
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// What the model reads of a channel listing: how many have a subscriber and how many
/// pattern subscriptions the server holds, then one channel a line with its subscribers.
fn channels_text(listing: &Value) -> String {
    let channels = listing["channels"].as_array().cloned().unwrap_or_default();
    let total = listing["total"].as_i64().unwrap_or(channels.len() as i64);
    let patterns = listing["patterns"].as_i64().unwrap_or(0);
    let under = match listing["pattern"].as_str() {
        Some("*") | None => String::new(),
        Some(pattern) => format!(" matching {pattern}"),
    };
    let mut text = if total == 0 {
        format!(
            "No channel{under} has a subscriber right now; the server holds {patterns} pattern subscription(s) (PSUBSCRIBE). A channel only published to, with nobody subscribed, is not listed: listen_channels still hears it."
        )
    } else {
        format!(
            "{total} channel(s){under} with a subscriber right now; the server holds {patterns} pattern subscription(s) (PSUBSCRIBE), whose channels are not counted here."
        )
    };
    if !channels.is_empty() {
        let shown = channels.len().min(CHANNELS_TO_MODEL);
        if (shown as i64) < total {
            text.push_str(&format!(" The first {shown} by name:"));
        }
        text.push_str("\nchannel, subscribers:");
        for row in channels.iter().take(shown) {
            text.push_str(&format!("\n{}  {}", row["name"].as_str().unwrap_or_default(), row["subscribers"].as_i64().unwrap_or(0)));
        }
    }
    clip(&text, ANSWER_TEXT_MAX)
}

/// The names a listen asks for under one side - an array, or one name as a string.
fn names_arg(args: &Value, key: &str) -> Value {
    match args.get(key) {
        Some(Value::String(name)) => json!([name]),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

/// What one listen heard, as the model reads it: what it listened to and for how long, each
/// message a line, then what it missed.
struct Heard {
    what: String,
    seconds: f64,
    messages: Vec<Value>,
    cap: Option<i64>,
    dropped: i64,
    ended: Option<String>,
}

/// What a listen shows on the page (SPEC §data.agent): the names it subscribed to, for the
/// Pub/Sub tab's watch set, and the messages as the feed gave them - each its channel, pattern,
/// payload, size and time - for the tab's log, with how long it listened and what it lost.
fn heard_frame(channels: &[String], patterns: &[String], heard: &Heard) -> Value {
    json!({
        "kind": "listen", "channels": channels, "patterns": patterns, "seconds": heard.seconds,
        "messages": heard.messages, "dropped": heard.dropped, "ended": heard.ended,
    })
}

fn heard_text(heard: &Heard) -> String {
    let mut text = if heard.messages.is_empty() {
        format!(
            "No message was published on {} in {:.1} s. Pub/sub keeps nothing: only what is published while someone listens is heard.",
            heard.what, heard.seconds
        )
    } else {
        format!("Listened {:.1} s to {}: {} message(s)", heard.seconds, heard.what, heard.messages.len())
    };
    if !heard.messages.is_empty() {
        if let Some(cap) = heard.cap {
            text.push_str(&format!(", stopped at the {cap} asked for"));
        }
        text.push_str(". One a line - channel (and the pattern that heard it), then the payload:");
        for message in &heard.messages {
            let mut line = message["channel"].as_str().unwrap_or_default().to_string();
            if let Some(pattern) = message["pattern"].as_str() {
                line.push_str(&format!(" (via {pattern})"));
            }
            let payload = message["payload"].as_str().unwrap_or_default();
            line.push_str(&format!("  {}", clip(payload, PAYLOAD_SHOWN)));
            if message["binary"] == true {
                line.push_str("  [binary]");
            }
            text.push('\n');
            text.push_str(&line);
        }
    }
    if heard.dropped > 0 {
        text.push_str(&format!("\n{} more message(s) came past the feed's rate cap and were not read.", heard.dropped));
    }
    if let Some(why) = &heard.ended {
        text.push_str(&format!("\nThe subscription ended early: {why}."));
    }
    clip(&text, ANSWER_TEXT_MAX)
}

fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn refused(text: impl Into<String>) -> ToolAnswer {
    ToolAnswer { text: text.into(), ok: false }
}

fn answered(text: impl Into<String>) -> ToolAnswer {
    ToolAnswer { text: text.into(), ok: true }
}

/// What a stage answers, whatever the kind.
const STAGED: &str = "Placed in the user's console. It has NOT run: the user reviews it and runs it themselves.";
/// The longest name a console tab of the agent's takes (open_console, a run's or stage's tab).
const TAB_MAX: usize = 40;
/// The most rows a write of the agent's changes on its own in auto mode (SPEC §data.agent
/// Writes): one that changes more is rolled back and staged for the user.
const WRITE_ROWS_MAX: u64 = 1_000;
/// The head of the snapshot's line saying how writes go, in the opening and in a later note.
const WRITES: &str = "Writes: ";

/// How the agent's writes go on a SQL connection (SPEC §data.agent Writes), the pane's switch
/// beside Send: Staged, the default - every write placed in the console for the user to run -
/// or Auto, where run_sql runs a narrow INSERT, REPLACE, UPDATE or DELETE itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Writes {
    #[default]
    Staged,
    Auto,
}

impl Writes {
    /// The body's writes: stage (or absent) or auto.
    fn of(body: &serde_json::Map<String, Value>) -> Result<Writes, String> {
        match field(body, "writes")?.as_deref() {
            None | Some("stage") => Ok(Writes::Staged),
            Some("auto") => Ok(Writes::Auto),
            Some(other) => Err(format!("writes must be stage or auto, got {other:?}")),
        }
    }

    /// The snapshot's line saying how writes go - what the prompt's Auto writes rule reads.
    fn fact(self) -> String {
        match self {
            Writes::Staged => format!("{WRITES}staged - run_sql runs reads only; stage_sql places every write in the console for the user to run"),
            Writes::Auto => format!(
                "{WRITES}auto - run_sql also runs one INSERT, REPLACE, UPDATE or DELETE itself and commits it when it changes at most {WRITE_ROWS_MAX} rows; one it holds is placed in the console unrun, and its result says why"
            ),
        }
    }
}

/// The console tab a run, a stage or open_console names: none for the Agent tab; else a name
/// of one line, at most TAB_MAX characters.
fn tab_of(args: &Value) -> Result<Option<String>, String> {
    let Some(tab) = string_arg(args, "tab") else { return Ok(None) };
    if tab.chars().count() > TAB_MAX {
        return Err(format!("tab: a short name, at most {TAB_MAX} characters"));
    }
    if tab.chars().any(char::is_control) {
        return Err("tab: a short name on one line".into());
    }
    Ok(Some(tab))
}

/// A run's or stage's frame to the page, naming the console tab it lands in when it names one.
fn in_tab(mut frame: Value, tab: &Option<String>) -> Value {
    if let Some(tab) = tab {
        frame["tab"] = json!(tab);
    }
    frame
}

/// The command document a run_mongo or stage_mongo names - an object, or the same as JSON
/// text (a model sometimes sends it so) - and its text as the page shows it.
fn command_of(args: &Value) -> Result<(serde_json::Map<String, Value>, String), String> {
    const SHAPE: &str = "a database command document, its first key the command, e.g. {\"find\": \"posts\", \"filter\": {}, \"limit\": 5}";
    let command = match args.get("command") {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            _ => return Err(format!("command must be one JSON object: {SHAPE}")),
        },
        _ => return Err(format!("command is required: {SHAPE}")),
    };
    if command.is_empty() {
        return Err(format!("command is empty: {SHAPE}"));
    }
    let text = Value::Object(command.clone()).to_string();
    if text.chars().count() > SQL_MAX {
        return Err(format!("command: at most {SQL_MAX} characters"));
    }
    Ok((command, text))
}

/// The command line a run_redis or stage_redis names: one line.
fn line_of(args: &Value) -> Result<String, String> {
    let Some(line) = string_arg(args, "command") else { return Err("command is required: one command line, e.g. GET user:1".into()) };
    if line.chars().count() > SQL_MAX {
        return Err(format!("command: at most {SQL_MAX} characters"));
    }
    if line.contains(['\n', '\r']) {
        return Err("command: ONE command line - call again for the next".into());
    }
    Ok(line)
}

/// The Data agent's tools, on one connection of one kind. `page` is the stream to the Data
/// page: each statement or command the agent runs or stages goes there as a `db` frame
/// (awaited, so it lands between the call's tool and toolResult frames).
pub struct DbTools {
    catalog: Arc<CatalogRegistry>,
    conn: String,
    kind: DbKind,
    page: Option<mpsc::Sender<Event>>,
    /// The most rows or documents a run brings back when it sets no limit of its own; a
    /// model's own limit argument may only lower it.
    limit: i64,
    /// MongoDB: the database a tool works in when the model names none - the one in view on
    /// the page; without it, the connection's own.
    db: Option<String>,
    /// The connection's notes, what read_memory, save_memory and delete_memory work on.
    memory: DbMemory,
    /// How a write given to run_sql goes: refused (Staged), or run when narrow (Auto).
    writes: Writes,
}

impl DbTools {
    pub fn new(catalog: Arc<CatalogRegistry>, conn: &str, kind: DbKind, page: Option<mpsc::Sender<Event>>) -> DbTools {
        DbTools { catalog, conn: conn.to_string(), kind, page, limit: RUN_LIMIT, db: None, memory: DbMemory::of(conn), writes: Writes::Staged }
    }

    /// Notes kept somewhere other than the home (a test's own folder).
    #[cfg(test)]
    fn remembering(mut self, memory: DbMemory) -> DbTools {
        self.memory = memory;
        self
    }

    /// The page's row limit in place of the default.
    pub fn limited(mut self, rows: i64) -> DbTools {
        self.limit = rows.max(1);
        self
    }

    /// The MongoDB database in view on the page, the tools' default.
    pub fn in_db(mut self, db: Option<String>) -> DbTools {
        self.db = db;
        self
    }

    /// How its writes go: Auto lets run_sql run a narrow one itself.
    pub fn writing(mut self, writes: Writes) -> DbTools {
        self.writes = writes;
        self
    }

    async fn to_page(&self, frame: Value) {
        if let Some(page) = &self.page {
            // A reader that went away is the loop's cancel to notice, not this call's failure.
            let _ = page.send(Event::default().event("db").data(frame.to_string())).await;
        }
    }

    fn sql(&self) -> Result<(ConnectionLease, Arc<dyn DbBrowser>), String> {
        match lease(&self.catalog, &self.conn)? {
            (held, Browser::Sql(browser)) => Ok((held, browser)),
            _ => Err(not_of(&self.conn, DbKind::Sql)),
        }
    }

    fn mongo(&self) -> Result<(ConnectionLease, Arc<dyn MongoBrowser>), String> {
        match lease(&self.catalog, &self.conn)? {
            (held, Browser::Mongo(browser)) => Ok((held, browser)),
            _ => Err(not_of(&self.conn, DbKind::Mongo)),
        }
    }

    fn redis(&self) -> Result<(ConnectionLease, Arc<dyn RedisBrowser>), String> {
        match lease(&self.catalog, &self.conn)? {
            (held, Browser::Redis(browser)) => Ok((held, browser)),
            _ => Err(not_of(&self.conn, DbKind::Redis)),
        }
    }

    /// The rows or documents a run brings back: the model's own limit, never past the page's.
    fn run_limit(&self, args: &Value) -> i64 {
        args.get("limit").and_then(Value::as_i64).map_or(self.limit, |asked| asked.clamp(1, self.limit))
    }

    async fn list_tables(&self, args: &Value) -> ToolAnswer {
        let (_lease, browser) = match self.sql() { Ok(held) => held, Err(why) => return refused(why) };
        let grep = string_arg(args, "grep");
        let mut o = json!({ "limit": TABLES_LISTED });
        if let Some(grep) = &grep { o["grep"] = json!(grep); }
        if let Some(schema) = string_arg(args, "schema") { o["schema"] = json!(schema); }
        match browser.list_tables(&o).await {
            Ok(listing) => {
                // What the agent finds the reader sees: the sidebar's search takes its grep.
                self.to_page(json!({ "kind": "list", "grep": grep.unwrap_or_default() })).await;
                answered(tables_text(&listing))
            }
            Err(why) => refused(why),
        }
    }

    async fn describe_table(&self, args: &Value) -> ToolAnswer {
        let Some(table) = string_arg(args, "table") else { return refused("table is required") };
        let (_lease, browser) = match self.sql() { Ok(held) => held, Err(why) => return refused(why) };
        let mut o = json!({ "table": table });
        if let Some(schema) = string_arg(args, "schema") { o["schema"] = json!(schema); }
        match browser.describe_table(&o).await {
            Ok(detail) => {
                // The table opens on the page at its Structure, under the schema the tree names it by.
                self.to_page(json!({ "kind": "table", "table": table, "schema": detail["schema"] })).await;
                answered(structure_text(&detail))
            }
            Err(why) => refused(why),
        }
    }

    async fn run_sql(&self, args: &Value) -> ToolAnswer {
        let Some(sql) = string_arg(args, "sql") else { return refused("sql is required") };
        if sql.chars().count() > SQL_MAX { return refused(format!("sql: at most {SQL_MAX} characters")); }
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        let limit = self.run_limit(args);
        let (_lease, browser) = match self.sql() { Ok(held) => held, Err(why) => return refused(why) };
        // In auto mode what is no read is a write to try: run_agent_write's gate and cap decide.
        if self.writes == Writes::Auto && assert_read_statement(&sql).is_err() {
            return self.write_sql(browser.as_ref(), &sql, &tab).await;
        }
        let started = Instant::now();
        match browser.run_read_query(&sql, Some(&json!(limit))).await {
            Ok(mut reply) => {
                reply["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
                let text = result_text(&reply);
                self.to_page(in_tab(json!({ "kind": "run", "sql": sql, "reply": reply }), &tab)).await;
                answered(text)
            }
            Err(why) => {
                self.to_page(in_tab(json!({ "kind": "run", "sql": sql, "error": why }), &tab)).await;
                refused(format!("the statement failed: {why}"))
            }
        }
    }

    /// A write run_sql was given in auto mode: run and committed within the cap, else held -
    /// placed in the console unrun as stage_sql would, the frame and the answer saying why.
    async fn write_sql(&self, browser: &dyn DbBrowser, sql: &str, tab: &Option<String>) -> ToolAnswer {
        let started = Instant::now();
        match browser.run_agent_write(sql, WRITE_ROWS_MAX).await {
            Ok(mut reply) => {
                reply["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
                let text = written_text(&reply);
                self.to_page(in_tab(json!({ "kind": "run", "sql": sql, "reply": reply, "wrote": true }), tab)).await;
                answered(text)
            }
            Err(WriteRefusal::Held(why)) => {
                self.to_page(in_tab(json!({ "kind": "stage", "sql": sql, "held": why }), tab)).await;
                answered(format!("Held, not run: {why}. {STAGED}"))
            }
            Err(WriteRefusal::Failed(why)) => {
                self.to_page(in_tab(json!({ "kind": "run", "sql": sql, "error": why }), tab)).await;
                refused(format!("the statement failed: {why}"))
            }
        }
    }

    async fn stage_sql(&self, args: &Value) -> ToolAnswer {
        let Some(sql) = string_arg(args, "sql") else { return refused("sql is required") };
        if sql.chars().count() > SQL_MAX { return refused(format!("sql: at most {SQL_MAX} characters")); }
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        self.to_page(in_tab(json!({ "kind": "stage", "sql": sql }), &tab)).await;
        answered(STAGED)
    }

    /// A console tab of the agent's own on the page, under the name the model gives it: the
    /// page opens it, or brings it to the front when this conversation opened it before. A
    /// statement given with it is staged there, as the kind's stage tool would.
    async fn open_console(&self, args: &Value) -> ToolAnswer {
        let tab = match tab_of(args) {
            Ok(Some(tab)) => tab,
            Ok(None) => return refused("tab is required: the tab's name, e.g. Revenue report"),
            Err(why) => return refused(why),
        };
        let (statement, run, stage) = match self.kind {
            DbKind::Sql => ("sql", "run_sql", "stage_sql"),
            DbKind::Mongo => ("command", "run_mongo", "stage_mongo"),
            DbKind::Redis => ("command", "run_redis", "stage_redis"),
        };
        let then = format!("Pass tab \"{tab}\" to {run} and {stage} to land there; without it they land in the Agent tab.");
        if args.get(statement).is_some_and(|given| !given.is_null()) {
            // The stage's own checks and frame; the frame names the tab, which opens it.
            let staged = match self.kind {
                DbKind::Sql => self.stage_sql(args).await,
                DbKind::Mongo => self.stage_mongo(args).await,
                DbKind::Redis => self.stage_redis(args).await,
            };
            if !staged.ok {
                return staged;
            }
            return answered(format!("Opened the console tab {tab} with the {statement} placed in it. It has NOT run. {then}"));
        }
        self.to_page(json!({ "kind": "console", "tab": tab })).await;
        answered(format!("Opened the console tab {tab}. {then}"))
    }

    /// The database a MongoDB call works in: the model's db, else the one in view on the
    /// page, else the connection's own (its URL's, or the first user database).
    async fn mongo_db(&self, browser: &dyn MongoBrowser, args: &Value) -> Result<String, String> {
        if let Some(db) = string_arg(args, "db").or_else(|| self.db.clone()) {
            return Ok(db);
        }
        let listing = browser.list_databases().await?;
        listing["current"].as_str().map(str::to_string).ok_or_else(|| "db is required: the connection names no database of its own".into())
    }

    async fn list_collections(&self, args: &Value) -> ToolAnswer {
        let (_lease, browser) = match self.mongo() { Ok(held) => held, Err(why) => return refused(why) };
        let db = match self.mongo_db(browser.as_ref(), args).await { Ok(db) => db, Err(why) => return refused(why) };
        match browser.list_collections(&db).await {
            Ok(listing) => {
                let grep = string_arg(args, "grep");
                self.to_page(json!({ "kind": "list", "grep": grep.as_deref().unwrap_or_default(), "db": db })).await;
                answered(collections_text(&db, &listing, grep.as_deref()))
            }
            Err(why) => refused(why),
        }
    }

    async fn describe_collection(&self, args: &Value) -> ToolAnswer {
        let Some(collection) = string_arg(args, "collection") else { return refused("collection is required") };
        let (_lease, browser) = match self.mongo() { Ok(held) => held, Err(why) => return refused(why) };
        let db = match self.mongo_db(browser.as_ref(), args).await { Ok(db) => db, Err(why) => return refused(why) };
        match browser.describe_for_agent(&db, &collection).await {
            Ok(detail) => {
                // The collection opens on the page at its Schema, sampled there as Analyze samples it.
                self.to_page(json!({ "kind": "coll", "db": db, "coll": collection })).await;
                answered(clip(&clipped(&detail).to_string(), ANSWER_TEXT_MAX))
            }
            Err(why) => refused(why),
        }
    }

    async fn run_mongo(&self, args: &Value) -> ToolAnswer {
        let (command, text) = match command_of(args) { Ok(both) => both, Err(why) => return refused(why) };
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        let limit = self.run_limit(args);
        let (_lease, browser) = match self.mongo() { Ok(held) => held, Err(why) => return refused(why) };
        let db = match self.mongo_db(browser.as_ref(), args).await { Ok(db) => db, Err(why) => return refused(why) };
        let started = Instant::now();
        match browser.run_read_command(&db, &command, limit).await {
            Ok(mut reply) => {
                reply["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
                if reply["more"] == true {
                    // The page says where the result was cut, as it does for a statement's row limit.
                    reply["limitApplied"] = json!(limit);
                }
                let answer = documents_text(&reply);
                self.to_page(in_tab(json!({ "kind": "run", "sql": text, "db": db, "reply": reply }), &tab)).await;
                answered(answer)
            }
            Err(why) => {
                self.to_page(in_tab(json!({ "kind": "run", "sql": text, "db": db, "error": why }), &tab)).await;
                refused(format!("the command failed: {why}"))
            }
        }
    }

    async fn stage_mongo(&self, args: &Value) -> ToolAnswer {
        let (_command, text) = match command_of(args) { Ok(both) => both, Err(why) => return refused(why) };
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        let db = string_arg(args, "db").or_else(|| self.db.clone());
        self.to_page(in_tab(json!({ "kind": "stage", "sql": text, "db": db }), &tab)).await;
        answered(STAGED)
    }

    async fn scan_keys(&self, args: &Value) -> ToolAnswer {
        let (_lease, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        // The page fills to KEYS_SCANNED keys before it comes back, so a sparse pattern is not
        // a call per hit.
        let mut o = json!({ "count": KEYS_SCANNED, "fill": KEYS_SCANNED });
        for key in ["pattern", "type", "cursor"] {
            if let Some(value) = string_arg(args, key) { o[key] = json!(value); }
        }
        match browser.list_keys(&o).await {
            Ok(page) => answered(keys_text(&page)),
            Err(why) => refused(why),
        }
    }

    async fn read_key(&self, args: &Value) -> ToolAnswer {
        // A key is read as named: its spaces are part of it.
        let Some(key) = args.get("key").and_then(Value::as_str).filter(|key| !key.is_empty()) else { return refused("key is required") };
        let (_lease, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        match browser.read_key(key).await {
            Ok(value) => {
                // What the agent reads the reader sees: the key opens in its own tab on the page.
                if value["type"] != "none" {
                    self.to_page(json!({ "kind": "key", "key": key, "type": value["type"] })).await;
                }
                answered(clip(&value.to_string(), ANSWER_TEXT_MAX))
            }
            Err(why) => refused(why),
        }
    }

    async fn read_stream(&self, args: &Value) -> ToolAnswer {
        let Some(key) = args.get("key").and_then(Value::as_str).filter(|key| !key.is_empty()) else { return refused("key is required") };
        let count = args.get("count").and_then(Value::as_i64).map_or(STREAM_ENTRIES, |asked| asked.clamp(1, STREAM_ENTRIES_MAX));
        let mut o = json!({ "count": count });
        for name in ["before", "match"] {
            if let Some(value) = string_arg(args, name) { o[name] = json!(value); }
        }
        let (_lease, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        let window = match browser.read_stream(key, &o).await {
            Ok(window) => window,
            Err(why) => return refused(why),
        };
        if window["type"] == "none" {
            return answered(format!("There is no key named {key}."));
        }
        self.to_page(json!({ "kind": "key", "key": key, "type": "stream" })).await;
        let groups = browser.stream_groups(key).await.ok();
        answered(stream_text(&window, groups.as_ref()))
    }

    async fn list_channels(&self, args: &Value) -> ToolAnswer {
        let (_lease, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        let mut o = json!({});
        if let Some(pattern) = string_arg(args, "pattern") { o["pattern"] = json!(pattern); }
        match browser.pubsub_channels(&o).await {
            Ok(listing) => {
                // The page's Pub/Sub tab opens on the listing the agent read, under its pattern.
                self.to_page(json!({ "kind": "channels", "listing": listing })).await;
                answered(channels_text(&listing))
            }
            Err(why) => refused(why),
        }
    }

    /// Subscribe on a connection of the feed's own (the shared one never leaves its mode) and
    /// read what is published until the time or the count runs out; the feed's drop ends the
    /// subscription. A provider stopping recalls the lease, and the listen gives it back.
    async fn listen_channels(&self, args: &Value) -> ToolAnswer {
        let watch = match pubsub_watch(&json!({ "channels": names_arg(args, "channels"), "patterns": names_arg(args, "patterns") })) {
            Ok(watch) => watch,
            Err(why) => return refused(why),
        };
        let seconds = args.get("seconds").and_then(Value::as_f64).map_or(LISTEN_SECONDS, |asked| asked.ceil().clamp(1.0, LISTEN_SECONDS_MAX as f64) as u64);
        let most = args.get("messages").and_then(Value::as_i64).map_or(LISTEN_MESSAGES, |asked| asked.clamp(1, LISTEN_MESSAGES_MAX));
        let (held, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        let mut tries = 0;
        let mut feed = loop {
            tries += 1;
            match browser.subscribe(&watch).await {
                Ok(feed) => break feed,
                Err(why) if why.contains("timed out") && tries < LISTEN_OPEN_TRIES => continue,
                Err(why) if why.contains("timed out") => {
                    return refused(format!(
                        "{why}, {tries} tries in a row: the link to the server is busy - try again, or listen on fewer names"
                    ))
                }
                Err(why) => return refused(why),
            }
        };
        let mut sides = Vec::new();
        if !watch.channels.is_empty() { sides.push(format!("channel(s) {}", watch.channels.join(", "))); }
        if !watch.patterns.is_empty() { sides.push(format!("pattern(s) {}", watch.patterns.join(", "))); }
        // Tokio's clock, so the time it says it listened is the time its deadline counted.
        let started = tokio::time::Instant::now();
        let deadline = tokio::time::sleep(Duration::from_secs(seconds));
        tokio::pin!(deadline);
        let recalled = held.recalled();
        tokio::pin!(recalled);
        let mut heard = Heard { what: sides.join(" and "), seconds: 0.0, messages: Vec::new(), cap: None, dropped: 0, ended: None };
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                _ = &mut recalled => {
                    heard.ended = Some("the connection is stopping".into());
                    break;
                }
                event = feed.recv() => match event {
                    None => {
                        heard.ended = Some("the feed closed".into());
                        break;
                    }
                    Some(event) => match event["event"].as_str() {
                        Some("message") => {
                            heard.messages.push(event);
                            if heard.messages.len() as i64 >= most {
                                heard.cap = Some(most);
                                break;
                            }
                        }
                        Some("dropped") => heard.dropped += event["count"].as_i64().unwrap_or(0),
                        Some("closed") => {
                            heard.ended = Some(event["reason"].as_str().unwrap_or("the server closed it").to_string());
                            break;
                        }
                        _ => {}
                    },
                },
            }
        }
        drop(feed);
        heard.seconds = started.elapsed().as_secs_f64();
        // The page's Pub/Sub tab watches the same names and logs what was heard, its own feed
        // carrying on from there.
        self.to_page(heard_frame(&watch.channels, &watch.patterns, &heard)).await;
        answered(heard_text(&heard))
    }

    async fn run_redis(&self, args: &Value) -> ToolAnswer {
        let line = match line_of(args) { Ok(line) => line, Err(why) => return refused(why) };
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        let (_lease, browser) = match self.redis() { Ok(held) => held, Err(why) => return refused(why) };
        let started = Instant::now();
        match browser.run_read_command(&line).await {
            Ok(reply) => {
                let answer = clip(&format!("Reply (the whole reply is open on the user's page):\n{}", clipped(&reply)), ANSWER_TEXT_MAX);
                let ms = started.elapsed().as_millis() as u64;
                self.to_page(in_tab(json!({ "kind": "run", "sql": line, "reply": { "reply": reply, "elapsedMs": ms } }), &tab)).await;
                answered(answer)
            }
            Err(why) => {
                self.to_page(in_tab(json!({ "kind": "run", "sql": line, "error": why }), &tab)).await;
                refused(format!("the command failed: {why}"))
            }
        }
    }

    async fn stage_redis(&self, args: &Value) -> ToolAnswer {
        let line = match line_of(args) { Ok(line) => line, Err(why) => return refused(why) };
        let tab = match tab_of(args) { Ok(tab) => tab, Err(why) => return refused(why) };
        self.to_page(in_tab(json!({ "kind": "stage", "sql": line }), &tab)).await;
        answered(STAGED)
    }

    /// The index without a name, one note whole with it.
    fn read_memory(&self, args: &Value) -> ToolAnswer {
        let Some(name) = string_arg(args, "name") else { return answered(self.memory.index_text()) };
        match self.memory.read(&name) {
            Ok(note) => {
                let saved = note.head.updated.get(..10).map(|day| format!(" (saved {day})")).unwrap_or_default();
                answered(format!("# {}{saved}\n{}\n\n{}", note.head.name, note.head.description, note.body))
            }
            Err(why) => refused(format!("{why}. {}", self.memory.index_text())),
        }
    }

    /// Save a note; the page hears it, so an open Memory view shows it.
    async fn save_memory(&self, args: &Value) -> ToolAnswer {
        let (name, description, body) = (string_arg(args, "name"), string_arg(args, "description"), string_arg(args, "body"));
        let saved = self.memory.save(name.as_deref().unwrap_or_default(), description.as_deref().unwrap_or_default(), body.as_deref().unwrap_or_default());
        match saved {
            Ok((name, replaced)) => {
                self.to_page(json!({ "kind": "memory", "op": "saved", "name": name })).await;
                let how = if replaced { "replaced the earlier one" } else { "a new note" };
                answered(format!("Saved {name} ({how}). The next conversation on this connection reads it in its index."))
            }
            Err(why) => refused(why),
        }
    }

    async fn delete_memory(&self, args: &Value) -> ToolAnswer {
        let Some(name) = string_arg(args, "name") else { return refused("name is required: the note to delete") };
        match self.memory.delete(&name) {
            Ok(true) => {
                self.to_page(json!({ "kind": "memory", "op": "deleted", "name": name })).await;
                answered(format!("Deleted {name}."))
            }
            Ok(false) => refused(format!("no note named {name}. {}", self.memory.index_text())),
            Err(why) => refused(why),
        }
    }
}

/// The memory's three, every kind's.
fn memory_specs() -> Vec<ToolSpec> {
    let name = json!({ "type": "string", "description": "The note's name: a short kebab-case slug for the kind of question, e.g. live-room-bound-user." });
    vec![
        spec(
            "read_memory",
            "Read this connection's memory: what earlier conversations on it learned about where its data lives and how to reach it. Without name, the index - every note's name and the question it answers; with name, that note whole. Read the note that covers the ask before you explore, then follow its route, still checking with your tools.",
            json!({ "type": "object", "properties": { "name": name } }),
        ),
        spec(
            "save_memory",
            "Save a note to this connection's memory for later conversations, replacing the one of that name. Rarely: most conversations save nothing. Only a route that took several lookups and that a query you ran verified (which tables, collections or keys hold the answer, how they link, the filters and the query that worked), a pitfall that cost a failed attempt, or what the user asked you to remember - never what one list or describe call shows, a route you did not run, or an overview of what you can do. One note a conversation at most unless the user asks; correct the note that covers that kind of question rather than add another. The way, never the data: no values read from rows, no personal data, no credentials. Do not mention the save in your reply.",
            json!({ "type": "object", "properties": {
                "name": name,
                "description": { "type": "string", "description": "One line: the question the note answers, as the index lists it." },
                "body": { "type": "string", "description": "Markdown, at most 8000 characters: the steps, the names, the query in a code block." }
            }, "required": ["name", "description", "body"] }),
        ),
        spec(
            "delete_memory",
            "Delete a note of this connection's memory that no longer holds - what it names is gone, or its route proved wrong.",
            json!({ "type": "object", "properties": { "name": name }, "required": ["name"] }),
        ),
    ]
}

/// open_console on a connection of this kind: the tab's name, and the kind's statement to
/// stage in it.
fn open_console_spec(kind: DbKind) -> ToolSpec {
    let name = json!({ "type": "string", "description": format!("The tab's name, short (at most {TAB_MAX} characters): what the work in it is, e.g. Revenue report.") });
    let (run, stage, statement) = match kind {
        DbKind::Sql => ("run_sql", "stage_sql", json!({ "sql": { "type": "string", "description": "Statements to place in it, not run." } })),
        DbKind::Mongo => ("run_mongo", "stage_mongo", json!({
            "command": { "type": "object", "description": "A command document to place in it, not run." },
            "db": { "type": "string", "description": "The command's database (default: the one in view on the user's page)." }
        })),
        DbKind::Redis => ("run_redis", "stage_redis", json!({ "command": { "type": "string", "description": "A command line to place in it, not run." } })),
    };
    let mut properties = json!({ "tab": name });
    if let (Some(all), Some(more)) = (properties.as_object_mut(), statement.as_object()) {
        all.extend(more.clone());
    }
    spec(
        "open_console",
        &format!("Open a console tab of yours on the user's page under a name - a new one, or the one of that name you opened before brought to the front - for work the user should see apart from the Agent tab: when they ask for a new tab, or a separate piece of work (a report's queries, a script to review). Then pass the same tab to {run} and {stage}: what they run or place lands there, its results under it. Given a statement, it is placed in the tab without running, as {stage} would."),
        json!({ "type": "object", "properties": properties, "required": ["tab"] }),
    )
}

fn spec(name: &str, description: &str, schema: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.into(), schema }
}

#[async_trait]
impl Toolbox for DbTools {
    fn specs(&self) -> Vec<ToolSpec> {
        let grep = json!({ "type": "string", "description": "Part of a name." });
        let tab = json!({ "type": "string", "description": "The console tab it lands in: a name open_console gave (none: the Agent tab)." });
        let mut specs = match self.kind {
            DbKind::Sql => vec![
                spec(
                    "list_tables",
                    "List the connection's tables (schema.name, type, approximate rows). grep narrows by name: a substring, `a,b` for both, `a|b` for either, * as a wildcard.",
                    json!({ "type": "object", "properties": {
                        "grep": grep,
                        "schema": { "type": "string", "description": "Only this schema (PostgreSQL) or database (MySQL)." }
                    } }),
                ),
                spec(
                    "describe_table",
                    "A table's columns (type, key, nullability, default, comment), indexes and foreign keys.",
                    json!({ "type": "object", "properties": {
                        "table": { "type": "string" },
                        "schema": { "type": "string", "description": "The table's schema (PostgreSQL, default public) or database (MySQL)." }
                    }, "required": ["table"] }),
                ),
                spec(
                    "run_sql",
                    "Run ONE statement. A read (SELECT, WITH, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN) runs in a read-only transaction that is rolled back. A write is refused here - stage it - unless the snapshot says Writes: auto: then one INSERT, REPLACE, UPDATE or DELETE (an UPDATE or DELETE only with a WHERE that narrows it) runs and is committed when it changes no more rows than that line allows; any other write, or one past that, is not run but placed in the console, and the result says why. It lands in the user's console and its whole result opens on their page; you read the row count, the columns and the first rows.",
                    json!({ "type": "object", "properties": {
                        "sql": { "type": "string", "description": "One statement." },
                        "limit": { "type": "integer", "description": "Rows to fetch when the statement has no LIMIT of its own (default and most: the page's row limit)." },
                        "tab": tab
                    }, "required": ["sql"] }),
                ),
                spec(
                    "stage_sql",
                    "Place a statement in the user's console without running it - for anything that writes or changes schema and run_sql does not run itself. The user reviews and runs it.",
                    json!({ "type": "object", "properties": { "sql": { "type": "string" }, "tab": tab }, "required": ["sql"] }),
                ),
            ],
            DbKind::Mongo => {
                let db = json!({ "type": "string", "description": "The database (default: the one in view on the user's page)." });
                let command = json!({ "type": "object", "description": "One database command document, its first key the command: {\"find\": \"posts\", \"filter\": {\"type\": \"x\"}, \"limit\": 5}." });
                vec![
                    spec(
                        "list_collections",
                        "List a database's collections and views (name, type, about how many documents). grep narrows by name: a substring, `a,b` for both, `a|b` for either, * as a wildcard.",
                        json!({ "type": "object", "properties": { "grep": grep, "db": db } }),
                    ),
                    spec(
                        "describe_collection",
                        "A collection's fields as a sample of its documents holds them (each one's types and how often it is present), its indexes, its size and document count.",
                        json!({ "type": "object", "properties": { "collection": { "type": "string" }, "db": db }, "required": ["collection"] }),
                    ),
                    spec(
                        "run_mongo",
                        "Run ONE read command document (find, aggregate, count, distinct, listCollections, listIndexes, collStats, dbStats, explain, ...). It lands in the user's console and its whole result opens on their page; you read the document count and the first documents. A write (insert, update, delete, findAndModify, an aggregate with $out or $merge, ...) is refused here: stage it.",
                        json!({ "type": "object", "properties": {
                            "command": command,
                            "db": db,
                            "limit": { "type": "integer", "description": "Documents to bring back from a find or aggregate (default and most: the page's row limit)." },
                            "tab": tab
                        }, "required": ["command"] }),
                    ),
                    spec(
                        "stage_mongo",
                        "Place a command document in the user's console without running it - for anything that writes or changes the database (insert, update, delete, createIndexes, drop, ...). The user reviews and runs it.",
                        json!({ "type": "object", "properties": { "command": command, "db": db, "tab": tab }, "required": ["command"] }),
                    ),
                ]
            }
            DbKind::Redis => {
                let line = json!({ "type": "string", "description": "One command line as the console reads it, an argument with spaces in quotes." });
                vec![
                    spec(
                        "scan_keys",
                        "Page through the keyspace with SCAN (never KEYS): each key's type and TTL, then the cursor to go on from. pattern is a MATCH glob (user:*); type one of string, list, set, zset, hash, stream. A page walks on until it holds up to 200 matching keys or about two seconds pass, so one call covers a sparse pattern; go on with the cursor only when the page says the scan goes on.",
                        json!({ "type": "object", "properties": {
                            "pattern": { "type": "string", "description": "A MATCH glob, e.g. post:*." },
                            "type": { "type": "string", "description": "Only keys of this type." },
                            "cursor": { "type": "string", "description": "Where the last page left off (\"0\" or none starts over)." }
                        } }),
                    ),
                    spec(
                        "read_key",
                        "Read one key whatever its type - a string's value, a hash's fields, a list's, set's, sorted set's or stream's first entries - with its type and TTL; a long value comes back windowed. The key opens in its own tab on the user's page: read it to show it to them.",
                        json!({ "type": "object", "properties": { "key": { "type": "string" } }, "required": ["key"] }),
                    ),
                    spec(
                        "read_stream",
                        "Read a stream key: its length, first and last ids, a window of entries newest first (each id with its fields) and its consumer groups (consumers, pending, lag, last delivered id). before pages to older entries; match keeps the entries a filter line names - field=value exact, field~text inside one field, a bare word anywhere - over a bounded walk. The stream opens in its own tab on the user's page.",
                        json!({ "type": "object", "properties": {
                            "key": { "type": "string" },
                            "count": { "type": "integer", "description": "Entries to bring (default 20, at most 100)." },
                            "before": { "type": "string", "description": "An entry id: only entries older than it (the last answer says which to pass)." },
                            "match": { "type": "string", "description": "A filter line, e.g. symbol=NVDA or error." }
                        }, "required": ["key"] }),
                    ),
                    spec(
                        "list_channels",
                        "List the pub/sub channels with a subscriber right now (PUBSUB CHANNELS), each with how many subscribe, and how many pattern subscriptions the server holds. A channel is not a key: scan_keys never finds one, and one only published to with nobody subscribed is not listed. The user's Pub/Sub page opens on this listing.",
                        json!({ "type": "object", "properties": {
                            "pattern": { "type": "string", "description": "A glob over channel names, e.g. news.* (default every channel)." }
                        } }),
                    ),
                    spec(
                        "listen_channels",
                        "Subscribe to channels and/or patterns on a connection of its own for a few seconds and read the messages published meanwhile (channel, the pattern that heard it, the payload clipped). Pub/sub keeps nothing: a quiet channel answers no messages. Name what list_channels found, or a pattern. The user's Pub/Sub page watches the same names, shows what was heard and goes on listening live.",
                        json!({ "type": "object", "properties": {
                            "channels": { "type": "array", "items": { "type": "string" }, "description": "Exact channel names (SUBSCRIBE)." },
                            "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns (PSUBSCRIBE), e.g. news.*." },
                            "seconds": { "type": "integer", "description": "How long to listen (default 5, at most 15)." },
                            "messages": { "type": "integer", "description": "Stop after this many messages (default 20, at most 50)." }
                        } }),
                    ),
                    spec(
                        "run_redis",
                        "Run ONE read command line (GET, MGET, HGETALL, HSCAN, LRANGE, SMEMBERS, SSCAN, ZRANGE, XRANGE, XINFO, PUBSUB NUMSUB, TYPE, TTL, SCAN, INFO, ...). It lands in the user's console and its reply opens on their page; you read it back. A write, a blocking command (XREAD, SUBSCRIBE, BLPOP, ...), a script, KEYS, SELECT and CONFIG are refused here: stage a write.",
                        json!({ "type": "object", "properties": { "command": line, "tab": tab }, "required": ["command"] }),
                    ),
                    spec(
                        "stage_redis",
                        "Place a command line in the user's console without running it - for anything that writes (SET, DEL, EXPIRE, HSET, ...). The user reviews and runs it.",
                        json!({ "type": "object", "properties": { "command": line, "tab": tab }, "required": ["command"] }),
                    ),
                ]
            }
        };
        specs.push(open_console_spec(self.kind));
        specs.extend(memory_specs());
        specs
    }

    async fn call(&mut self, name: &str, args: &Value) -> ToolAnswer {
        // Only the connection kind's own and the memory: another kind's name is no tool here.
        if !db_tools_on(self.kind.as_str()).iter().chain(db_tools_shared()).any(|tool| tool.name == name) {
            return refused(format!("no tool named {name}"));
        }
        match name {
            "list_tables" => self.list_tables(args).await,
            "describe_table" => self.describe_table(args).await,
            "run_sql" => self.run_sql(args).await,
            "stage_sql" => self.stage_sql(args).await,
            "list_collections" => self.list_collections(args).await,
            "describe_collection" => self.describe_collection(args).await,
            "run_mongo" => self.run_mongo(args).await,
            "stage_mongo" => self.stage_mongo(args).await,
            "scan_keys" => self.scan_keys(args).await,
            "read_key" => self.read_key(args).await,
            "read_stream" => self.read_stream(args).await,
            "list_channels" => self.list_channels(args).await,
            "listen_channels" => self.listen_channels(args).await,
            "run_redis" => self.run_redis(args).await,
            "stage_redis" => self.stage_redis(args).await,
            "open_console" => self.open_console(args).await,
            "read_memory" => self.read_memory(args),
            "save_memory" => self.save_memory(args).await,
            "delete_memory" => self.delete_memory(args).await,
            other => refused(format!("no tool named {other}")),
        }
    }
}

/// The scope a connection's conversation files and is found under.
fn scope(conn: &str) -> RunContext {
    RunContext { task: Some(TASK.into()), subject: Some(conn.into()), ..RunContext::default() }
}

/// The system prompt a new Data conversation opens with: the agent's prompt in force (`base`),
/// then the snapshot - the connection and its dialect, what is in view on the page (a SQL
/// schema, a MongoDB database), what holds the data (the tables, the collections, the
/// keyspace), the day, the agent - and the connection's memory: how to use it, its index. A
/// continuation replays the one it opened with.
async fn opening(catalog: &CatalogRegistry, conn: &str, schema: Option<&str>, base: &str, agent: &str, chat: &swiss_llm::provider::Chat, writes: Writes) -> String {
    let mut facts = Vec::new();
    match lease(catalog, conn) {
        Ok((held, browser)) => {
            let dialect = held.dialect().to_string();
            match browser {
                Browser::Sql(browser) => {
                    let dialect = browser.dialect().as_str();
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes(dialect)));
                    facts.push(format!("- {}", writes.fact()));
                    if let Some(schema) = schema {
                        facts.push(format!("- Schema in view on the page: {schema}"));
                    }
                    let mut o = json!({ "limit": TABLES_NAMED });
                    if let Some(schema) = schema { o["schema"] = json!(schema); }
                    match browser.list_tables(&o).await {
                        Ok(listing) => facts.push(tables_fact(&listing)),
                        Err(why) => facts.push(format!("- Tables: not listed as the conversation began ({why})")),
                    }
                }
                Browser::Mongo(browser) => {
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes("mongo")));
                    mongo_facts(browser.as_ref(), schema, &mut facts).await;
                }
                Browser::Redis(browser) => {
                    facts.push(format!("- Connection: {conn} ({dialect}, {})", browser.label()));
                    facts.push(format!("- Dialect: {}", dialect_notes("redis")));
                    match browser.list_databases().await {
                        Ok(listing) => facts.push(keyspace_fact(&listing)),
                        Err(why) => facts.push(format!("- Keyspace: not read as the conversation began ({why})")),
                    }
                    match browser.pubsub_channels(&json!({})).await {
                        Ok(listing) => facts.push(pubsub_fact(&listing)),
                        Err(why) => facts.push(format!("- Pub/sub: not read as the conversation began ({why})")),
                    }
                }
            }
        }
        Err(why) => facts.push(format!("- Connection: {conn} (not reachable as the conversation began: {why})")),
    }
    facts.push(format!("- Today's date: {}", chrono::Local::now().format("%Y-%m-%d (%A)")));
    facts.push(format!("- {}", answering(agent, chat)));
    format!(
        "{base}\n\n{SNAPSHOT_HEADING}\nThese facts were taken as this conversation began; check with your tools before relying on one that may have changed since.\n{}\n\n{}",
        facts.join("\n"),
        DbMemory::of(conn).prompt_section()
    )
}

/// The snapshot's tables line from a listing asked for TABLES_NAMED: every name when they
/// all came back, else only how many there are.
fn tables_fact(listing: &Value) -> String {
    let names: Vec<String> = listing["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| format!("{}.{}", t["schema"].as_str().unwrap_or_default(), t["name"].as_str().unwrap_or_default()))
        .collect();
    let total = listing["total"].as_i64().unwrap_or(names.len() as i64);
    if names.len() as i64 >= total {
        format!("- Tables ({total}): {}", names.join(", "))
    } else {
        format!("- Tables: {total}, too many to name here; list_tables finds them by name (grep)")
    }
}

/// The snapshot's MongoDB lines: the database the tools default to (the one in view, else
/// the connection's own), its collections, and the connection's other databases.
async fn mongo_facts(browser: &dyn MongoBrowser, in_view: Option<&str>, facts: &mut Vec<String>) {
    let listing = browser.list_databases().await;
    let db = in_view
        .map(str::to_string)
        .or_else(|| listing.as_ref().ok().and_then(|listing| listing["current"].as_str().map(str::to_string)));
    match &db {
        Some(db) if in_view.is_some() => facts.push(format!("- Database in view on the page (the tools' default): {db}")),
        Some(db) => facts.push(format!("- Database (the tools' default): {db}")),
        None => facts.push("- Database: none in view; name db in each call".into()),
    }
    if let Some(db) = &db {
        match browser.list_collections(db).await {
            Ok(listing) => facts.push(collections_fact(&listing)),
            Err(why) => facts.push(format!("- Collections: not listed as the conversation began ({why})")),
        }
    }
    match listing {
        Ok(listing) => {
            let names: Vec<&str> = listing["databases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["system"] != true)
                .filter_map(|row| row["name"].as_str())
                .collect();
            if names.len() <= DATABASES_NAMED {
                facts.push(format!("- Databases ({}): {}", names.len(), names.join(", ")));
            } else {
                facts.push(format!("- Databases: {}", names.len()));
            }
        }
        Err(why) => facts.push(format!("- Databases: not listed as the conversation began ({why})")),
    }
}

/// The snapshot's collections line: every name while there are TABLES_NAMED or fewer, else
/// only how many - as tables_fact.
fn collections_fact(listing: &Value) -> String {
    let names: Vec<&str> = listing["collections"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).collect();
    if names.len() <= TABLES_NAMED {
        format!("- Collections ({}): {}", names.len(), names.join(", "))
    } else {
        format!("- Collections: {}, too many to name here; list_collections finds them by name (grep)", names.len())
    }
}

/// The snapshot's Redis line: the logical database the connection sits on and its key count,
/// and the others that hold keys, out of this connection's reach.
fn keyspace_fact(listing: &Value) -> String {
    let rows = listing["databases"].as_array().cloned().unwrap_or_default();
    let current = listing["current"].as_str().unwrap_or("0");
    let keys = |row: &Value| row["tables"].as_i64().unwrap_or(0);
    let here = rows.iter().find(|row| row["name"] == current).map(keys).unwrap_or(0);
    let mut fact = format!("- Database: db{current}, {here} key(s)");
    let others: Vec<String> = rows
        .iter()
        .filter(|row| row["name"] != current)
        .map(|row| format!("db{} {}", row["name"].as_str().unwrap_or("?"), keys(row)))
        .collect();
    if !others.is_empty() {
        fact.push_str(&format!("; others hold keys ({}), out of this connection's reach", others.join(", ")));
    }
    fact
}

/// The snapshot's pub/sub line: the channels with a subscriber - named while there are
/// CHANNELS_NAMED or fewer - and the pattern subscriptions.
fn pubsub_fact(listing: &Value) -> String {
    let names: Vec<&str> = listing["channels"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).collect();
    let total = listing["total"].as_i64().unwrap_or(names.len() as i64);
    let patterns = listing["patterns"].as_i64().unwrap_or(0);
    let channels = if total == 0 {
        "no channel with a subscriber".to_string()
    } else if total as usize <= CHANNELS_NAMED && names.len() as i64 == total {
        format!("{total} channel(s) with a subscriber ({})", names.join(", "))
    } else {
        format!("{total} channel(s) with a subscriber")
    };
    format!("- Pub/sub: {channels}, {patterns} pattern subscription(s); list_channels and listen_channels read them")
}

/// The snapshot line naming the agent and the model answering, in the opening and in a
/// later turn's note alike: the last one the dialogue holds is what the model was last told.
const ANSWERING: &str = "You are the agent ";

fn answering(agent: &str, chat: &Chat) -> String {
    format!("{ANSWERING}{agent}, on model {} from provider {}", chat.model, chat.provider)
}

/// The words a send asks with: the reader's, and a note after them for what the dialogue
/// last named otherwise - the model answering, when the reader moved the conversation to
/// another in the pane's menu, and on a SQL connection (`writes`) how writes go, when the
/// reader flipped the pane's switch. The system replays as noted, so the opening's lines
/// stay; the last line standing is what the model reads as true.
fn asked(prompt: &str, system: &str, prior: &[Value], agent: &str, chat: &Chat, writes: Option<Writes>) -> String {
    let users = prior.iter().filter(|entry| entry["role"] == "user").filter_map(|entry| entry["text"].as_str());
    let lines: Vec<&str> = std::iter::once(system).chain(users).flat_map(str::lines).map(|line| line.trim_start_matches("- ")).collect();
    let told = |head: &str| lines.iter().rfind(|line| line.starts_with(head)).copied();
    let mut notes = Vec::new();
    let now = answering(agent, chat);
    if told(ANSWERING).is_some_and(|told| told != now) {
        notes.push(format!("The model answering changed since you were last told.\n- {now}"));
    }
    if let Some(writes) = writes {
        let now = writes.fact();
        // A conversation older than the switch was never told: its writes were staged.
        if told(WRITES).map_or(writes != Writes::Staged, |told| told != now) {
            notes.push(format!("The write mode changed since you were last told.\n- {now}"));
        }
    }
    if notes.is_empty() {
        return prompt.to_string();
    }
    format!("{prompt}\n\n<environment>\n{}\n</environment>", notes.join("\n"))
}

/// The model and the effort the reader picked in the pane's menu for what a route starts
/// (SPEC §data.agent Model); each one absent is the agent's own.
#[derive(Default)]
struct Pick {
    provider: Option<String>,
    model: Option<String>,
    effort: Option<Effort>,
}

impl Pick {
    /// The body's provider, model and effort (low, medium, high or max).
    fn of(body: &serde_json::Map<String, Value>) -> Result<Pick, String> {
        let effort = match field(body, "effort")? {
            None => None,
            Some(word) => Some(Effort::parse(&word).ok_or_else(|| format!("effort must be low, medium, high or max, got {word:?}"))?),
        };
        Ok(Pick { provider: field(body, "provider")?, model: field(body, "model")?, effort })
    }

    /// Whether it names a model: the conversation it continues moves to it (see
    /// [conversation::retarget]).
    fn moves(&self) -> bool {
        self.provider.is_some() || self.model.is_some()
    }
}

/// What every Data route that drives a conversation works from: the db agent, its prompt
/// in force (DB_AGENT_PROMPT and the agent's own after it - what a continuation must still
/// match), the history, the chat recording into it, the connection's seat - one stream per
/// agent and connection, the agent's other connections beside it - and its kind, which
/// names the conversation's tools.
struct Desk {
    agent: AgentConfig,
    base: String,
    history: Arc<LlmHistory>,
    chat: Chat,
    seat: String,
    kind: DbKind,
}

/// The checks every such route makes before anything reserves: a db agent, a MySQL,
/// PostgreSQL, MongoDB or Redis connection, the history open, a chat for the agent's provider
/// and model at its effort - or the ones the reader picked: a provider swiss has (its first
/// chat model unless the pick names one, when it is not the agent's), one of its chat models.
#[allow(clippy::result_large_err)]
fn desk(state: &LlmState, services: &RuntimeServices, name: &str, conn: &str, pick: &Pick) -> Result<Desk, Response> {
    let mut agent = state.agent(name).map_err(bad)?;
    if agent.runtime != Runtime::Db {
        return Err(bad(format!(
            "{name} is a {} agent; the Data page works with a DB agent only (runtime db), made for it on the Agents page",
            agent.runtime.as_str()
        )));
    }
    let base = agent.system_prompt().map(|prompt| prompt.into_owned()).unwrap_or_default();
    let kind = lease(&services.catalog, conn).map_err(bad)?.1.kind();
    let Some(history) = state.history() else {
        return Err(admin_error(StatusCode::SERVICE_UNAVAILABLE, "the Data agent needs the llm history store; this home has none"));
    };
    let config = state.config();
    if let Some(provider) = &pick.provider {
        if !config.providers.contains_key(provider) {
            return Err(bad(format!("provider: swiss has no provider {provider:?}")));
        }
        if agent.provider.as_ref() != Some(provider) {
            agent.model = None;
        }
        agent.provider = Some(provider.clone());
    }
    if let Some(model) = &pick.model {
        let provider = agent.provider.clone().unwrap_or_default();
        let served = config.providers.get(&provider).is_some_and(|entry| entry.models_of(ModelKind::Chat).any(|one| one == model));
        if !served {
            return Err(bad(format!("model: {model:?} is not a chat model of provider {provider:?}")));
        }
        agent.model = Some(model.clone());
    }
    if pick.effort.is_some() {
        agent.effort = pick.effort;
    }
    let chat = agent.chat(&config.providers).map_err(bad)?.recording(Some(history.clone()), Some(&agent.name));
    Ok(Desk { agent, base, history, chat, seat: format!("{name}@data:{conn}"), kind })
}

/// The body's runId: absent or null is None.
fn run_id_of(body: &serde_json::Map<String, Value>) -> Result<Option<i64>, &'static str> {
    match body.get("runId") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or("runId must be an integer"),
    }
}

/// The body's agent and connection, both required.
#[allow(clippy::result_large_err)]
fn agent_and_conn(body: &serde_json::Map<String, Value>) -> Result<(String, String), Response> {
    match (field(body, "agent"), field(body, "conn")) {
        (Ok(Some(name)), Ok(Some(conn))) => Ok((name, conn)),
        (Ok(None), _) => Err(bad("agent is required")),
        (_, Ok(None)) => Err(bad("conn is required")),
        (Err(why), _) | (_, Err(why)) => Err(bad(why)),
    }
}

/// A conversation guard's refusal as the route answers it.
fn lane_refusal(why: LaneError) -> Response {
    match why {
        LaneError::Refused { code, message } => refuse(code, message),
        LaneError::Unavailable(why) => admin_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

/// A seat held for a step that streams nothing (a reopen) or takes no words (a
/// compaction): its steer inbox shut from the start, so a queued line is never posted
/// into it.
fn shut_steer() -> Arc<Steer> {
    let steer = Steer::new();
    steer.close();
    steer
}

/// POST /api/llm/db/stream {agent, conn, prompt, runId?, schema?, rowLimit?, provider?, model?,
/// effort?, writes?}: one send of a Data
/// conversation, as server-sent events - the Test stream's frames (run, delta, tool,
/// toolResult, final, done) plus `db` for each statement or command the agent runs ({kind:
/// "run", sql, db?, tab?, reply | error}) or stages ({kind: "stage", sql, db?, tab?}) and each
/// console tab it opens ({kind: "console", tab}) - sql is the text the console takes, a
/// MongoDB command's JSON or a Redis line, db a MongoDB command's database, and tab the name of
/// the agent's console tab it lands in (none: the Agent tab). Without runId it opens the
/// connection's conversation (an open one refuses 409 open-run-exists); with it, it
/// continues that one, which must be this agent's on this connection (else 409 gone).
/// Only a db agent works here, and only on a MySQL, PostgreSQL, MongoDB or Redis connection.
/// schema is the page's schema in view, or its database on MongoDB. rowLimit is the
/// console's: a run without a limit of its own brings back that many rows or documents.
/// provider, model and effort are the pane's menu (each absent the agent's own): a
/// continuation moves to that model, the model told so after the reader's words. writes is
/// the pane's switch on a SQL connection, stage (absent) or auto (SPEC §data.agent Writes): in
/// auto a write run_sql is given runs when narrow ({kind: "run", ..., wrote: true}) or is held
/// ({kind: "stage", ..., held}); the model is told a change the same way.
pub async fn stream(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "prompt", "runId", "schema", "rowLimit", "provider", "model", "effort", "writes"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn, schema) = match (field(&body, "agent"), field(&body, "conn"), field(&body, "schema")) {
        (Ok(Some(name)), Ok(Some(conn)), Ok(schema)) => (name, conn, schema),
        (Ok(None), _, _) => return bad("agent is required"),
        (_, Ok(None), _) => return bad("conn is required"),
        (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => return bad(why),
    };
    let prompt = match field(&body, "prompt") {
        Ok(Some(prompt)) if prompt.chars().count() <= PROMPT_MAX => prompt,
        Ok(Some(_)) => return bad(format!("prompt: at most {PROMPT_MAX} characters")),
        Ok(None) => return bad("prompt is required"),
        Err(why) => return bad(why),
    };
    let joining = match run_id_of(&body) {
        Ok(joining) => joining,
        Err(why) => return bad(why),
    };
    let row_limit = match body.get("rowLimit") {
        None | Some(Value::Null) => RUN_LIMIT,
        Some(value) => match value.as_i64() {
            Some(rows) if rows >= 1 => rows,
            _ => return bad("rowLimit must be a positive integer"),
        },
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let writes = match Writes::of(&body) {
        Ok(writes) => writes,
        Err(why) => return bad(why),
    };
    let Desk { agent, base, history, chat, seat, kind } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    if writes == Writes::Auto && kind != DbKind::Sql {
        return bad("writes: auto runs on a MySQL or PostgreSQL connection only");
    }
    if state.seat_run_of(&seat).is_some() {
        return refuse("busy", BUSY);
    }
    if let Some(id) = joining.filter(|_| pick.moves()) {
        if let Err(why) = within(scope(&conn), conversation::retarget(&history, id, &name, &chat)).await {
            return lane_refusal(why);
        }
    }
    let lane = match within(scope(&conn), conversation::join(&history, joining, &name, agent.runtime.as_str(), &chat, &base)).await {
        Ok(lane) => lane,
        Err(why) => return lane_refusal(why),
    };
    let conversation::Lane { mut run, run_id, continued, prior, system: noted } = lane;
    let system = match noted {
        Some(noted) => noted,
        None => opening(&services.catalog, &conn, schema.as_deref(), &base, &name, &chat, writes).await,
    };
    let prompt = asked(&prompt, &system, &prior, &name, &chat, (kind == DbKind::Sql).then_some(writes));
    let cancel = CancelFlag::new();
    let steer = Steer::new();
    if !state.seat(&seat, run_id, cancel.clone(), steer.clone()) {
        return refuse("busy", BUSY);
    }
    run.set_cancel(cancel.clone());
    run.set_steer(steer.clone());
    let session = state.session(&agent, TASK);
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    sse_send(&tx, "run", json!({
        "runId": run_id, "agent": name, "conn": conn, "provider": chat.provider, "model": chat.model,
        "continued": continued,
    }));
    let started = Instant::now();
    let max_turns = agent.turns();
    tokio::spawn(async move {
        // A MongoDB conversation works in the database in view unless the model names another.
        let in_view = if kind == DbKind::Mongo { schema } else { None };
        let mut tools = DbTools::new(services.catalog.clone(), &conn, kind, Some(tx.clone())).limited(row_limit).in_db(in_view).writing(writes);
        let mut on_event = awaited_sink(&tx, &cancel);
        // The drive in its own block, as the Test's: the pinned future holds the recorder,
        // and the timeout filing below borrows it only once the future has dropped.
        let driven = {
            let looped = swiss_llm::agent::run_loop_continued(
                &chat, &system, &prior, &prompt, &mut tools, max_turns, &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            tokio::time::timeout(LOOP_TIMEOUT, async {
                loop {
                    tokio::select! {
                        _ = tx.closed(), if !cancel.requested() => cancel.request(),
                        outcome = &mut looped => return outcome,
                    }
                }
            })
            .await
        };
        steer.close();
        let answer = loop_answer(driven, &chat, &mut run, &system, &prompt, LOOP_TIMEOUT, started);
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        state.unseat(&seat);
    });
    sse_of(rx)
}

/// GET /api/llm/db/conversation?agent&conn: the connection's open conversation with
/// that agent, {runId} (null when none) - what the Data page restores on open.
pub async fn open_conversation(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(agent), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    let open = within(scope(conn), history.continuable_open(agent)).await;
    ok(json!({ "ok": true, "runId": open }))
}

/// GET /api/llm/db/conversations?agent&conn&page: the connection's conversations with that
/// agent, newest first, 20 to a page - the pane's History, open, closed and failed alike,
/// in the history's run shape ({runs, page, pageSize, more}); a compaction's run names the
/// one it condensed in its source (`compact:<id>`).
pub async fn conversations(
    State(state): State<Arc<LlmState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(agent), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let page = match params.get("page").map(|page| page.parse::<i64>()) {
        None => 0,
        Some(Ok(page)) if page >= 0 => page,
        Some(_) => return bad("page must be a whole number"),
    };
    let Some(history) = state.history() else {
        return admin_error(StatusCode::SERVICE_UNAVAILABLE, "the llm history is not open: the plugin is off or failed to open its store");
    };
    match within(scope(conn), history.read_conversations(agent, page, HISTORY_PAGE)).await {
        Ok(mut found) => {
            found["ok"] = json!(true);
            ok(found)
        }
        Err(why) => admin_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

/// GET /api/llm/db/context?agent&conn&runId: how full the conversation's next request starts -
/// the system prompt it opened under, the connection kind's tool definitions and the dialogue
/// it replays, in bytes against the agent's budget (maxContextKiB): {bytes, cap, system,
/// tools}. The pane's context meter reads it for a conversation it restores, reopens or
/// compacts; a send's usage frames say it round by round.
pub async fn context(
    State(state): State<Arc<LlmState>>,
    Extension(services): Extension<Arc<RuntimeServices>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (Some(name), Some(conn)) = (params.get("agent").filter(|a| !a.is_empty()), params.get("conn").filter(|c| !c.is_empty())) else {
        return bad("agent and conn are required");
    };
    let Some(run_id) = params.get("runId").and_then(|id| id.parse::<i64>().ok()) else {
        return bad("runId is required: the conversation to weigh");
    };
    let Desk { base, history, chat, kind, .. } = match desk(&state, &services, name, conn, &Pick::default()) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    let (system, prior) = within(scope(conn), async {
        let system = history.system_note_of(run_id).await.unwrap_or(base);
        let prior = match history.replay_state(run_id).await {
            ReplayState::Continuable { prior } => prior,
            _ => Vec::new(),
        };
        (system, prior)
    })
    .await;
    let tools = swiss_llm::agent::tools_bytes(&DbTools::new(services.catalog.clone(), conn, kind, None).specs());
    let dialogue = serde_json::to_vec(&prior).map(|bytes| bytes.len()).unwrap_or(0);
    ok(json!({
        "ok": true, "bytes": system.len() + tools + dialogue, "cap": chat.context_cap(),
        "system": system.len(), "tools": tools,
    }))
}

/// POST /api/llm/db/reopen {agent, conn, runId, provider?, model?, effort?}: carry an earlier
/// conversation of this connection on (the pane's History, Continue here). The open one, if
/// another, closes - it stays in History - and runId reopens when it is a closed conversation
/// of this agent here that passes the continuation guards, moved to the model the pane's
/// menu names when it names one; {ok, runId}. 409 busy while a send or compaction
/// works the connection, gone for a run that cannot go on (failed, a round left
/// unresolved, another connection's), context-changed when the agent's prompt changed
/// since it opened - both runs then stay as they were.
pub async fn reopen(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "runId", "provider", "model", "effort"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn) = match agent_and_conn(&body) {
        Ok(both) => both,
        Err(refused) => return refused,
    };
    let run_id = match run_id_of(&body) {
        Ok(Some(id)) => id,
        Ok(None) => return bad("runId is required: the conversation to carry on"),
        Err(why) => return bad(why),
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let Desk { agent, base, history, chat, seat, .. } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    // The seat for the swap's length: no send opens or continues the connection's
    // conversation half-way through it.
    if !state.seat(&seat, run_id, CancelFlag::new(), shut_steer()) {
        return refuse("busy", BUSY);
    }
    let reopened = within(scope(&conn), conversation::reopen(&history, run_id, &name, agent.runtime.as_str(), &chat, &base, pick.moves())).await;
    state.unseat(&seat);
    match reopened {
        Ok(()) => ok(json!({ "ok": true, "runId": run_id })),
        Err(why) => lane_refusal(why),
    }
}

/// POST /api/llm/db/compact {agent, conn, runId, focus?, provider?, model?, effort?}: condense
/// the connection's open
/// conversation into a checkpoint and carry the dialogue on from it (SPEC §data.agent
/// Compact), as server-sent events: run ({runId: the new conversation, from: the one
/// condensed, compacting: true}), the checkpoint's thinking and delta frames, final, done
/// ({ok, compacted, runId: the conversation open after it, from}). The model reads the
/// whole dialogue and DB_COMPACT_INSTRUCTION after it (focus, the reader's words after
/// /compact, closing it), and the checkpoint opens a new conversation in the same scope:
/// its next send replays COMPACT_FRAMING and the checkpoint, never the condensed turns,
/// which History keeps whole under the closed run. Any failure - the model's error, a
/// cancel, a checkpoint cut at maxTokens or never written, the time bound - files the new
/// run failed and reopens the old one as it was (done: compacted false, runId the old one,
/// or null when it could not reopen).
pub async fn compact(State(state): State<Arc<LlmState>>, Extension(services): Extension<Arc<RuntimeServices>>, bytes: Bytes) -> Response {
    let body = match body_of(&bytes, &["agent", "conn", "runId", "focus", "provider", "model", "effort"]) {
        Ok(body) => body,
        Err(why) => return bad(why),
    };
    let (name, conn) = match agent_and_conn(&body) {
        Ok(both) => both,
        Err(refused) => return refused,
    };
    let joining = match run_id_of(&body) {
        Ok(Some(id)) => id,
        Ok(None) => return bad("runId is required: the conversation to compact"),
        Err(why) => return bad(why),
    };
    let focus = match field(&body, "focus") {
        Ok(focus) if focus.as_ref().is_none_or(|focus| focus.chars().count() <= FOCUS_MAX) => focus,
        Ok(_) => return bad(format!("focus: at most {FOCUS_MAX} characters")),
        Err(why) => return bad(why),
    };
    let pick = match Pick::of(&body) {
        Ok(pick) => pick,
        Err(why) => return bad(why),
    };
    let Desk { agent, base, history, chat, seat, kind } = match desk(&state, &services, &name, &conn, &pick) {
        Ok(desk) => desk,
        Err(refused) => return refused,
    };
    if state.seat_run_of(&seat).is_some() {
        return refuse("busy", BUSY);
    }
    if pick.moves() {
        if let Err(why) = within(scope(&conn), conversation::retarget(&history, joining, &name, &chat)).await {
            return lane_refusal(why);
        }
    }
    let fold = match within(scope(&conn), conversation::fold(&history, joining, &name, agent.runtime.as_str(), &chat, &base)).await {
        Ok(fold) => fold,
        Err(why) => return lane_refusal(why),
    };
    let conversation::Fold { lane, prior, system, from } = fold;
    let conversation::Lane { mut run, run_id, .. } = lane;
    let cancel = CancelFlag::new();
    if !state.seat(&seat, run_id, cancel.clone(), shut_steer()) {
        run.release();
        let _ = within(scope(&conn), conversation::undo_fold(&history, run_id, from, &name)).await;
        return refuse("busy", BUSY);
    }
    run.set_cancel(cancel.clone());
    let session = state.session(&agent, TASK);
    let instruction = match &focus {
        Some(focus) => format!("{DB_COMPACT_INSTRUCTION}\n\nThe reader asks the checkpoint to keep in particular: {focus}"),
        None => DB_COMPACT_INSTRUCTION.to_string(),
    };
    // The conversation's own tools, declared and never served (see run_compaction).
    let tools = DbTools::new(services.catalog.clone(), &conn, kind, None).specs();
    let (tx, rx) = mpsc::channel::<Event>(SSE_FRAMES);
    sse_send(&tx, "run", json!({
        "runId": run_id, "from": from, "agent": name, "conn": conn, "provider": chat.provider, "model": chat.model,
        "compacting": true,
    }));
    let started = Instant::now();
    tokio::spawn(async move {
        let mut on_event = awaited_sink(&tx, &cancel);
        let driven = {
            let looped = swiss_llm::agent::run_compaction(
                &chat, &system, &prior, &instruction, COMPACT_FRAMING, tools, &session, &mut run, &mut on_event,
            );
            tokio::pin!(looped);
            tokio::time::timeout(LOOP_TIMEOUT, async {
                loop {
                    tokio::select! {
                        _ = tx.closed(), if !cancel.requested() => cancel.request(),
                        outcome = &mut looped => return outcome,
                    }
                }
            })
            .await
        };
        // A round that answered but left no checkpoint to carry on from fails too.
        let spoiled = match &driven {
            Ok(Ok(outcome)) if outcome.cancelled => None,
            Ok(Ok(outcome)) if outcome.cut => Some("the checkpoint stopped at the model's output limit (maxTokens): nothing was compacted"),
            Ok(Ok(outcome)) if outcome.capped || outcome.summary.trim().is_empty() => Some("the model wrote no checkpoint: nothing was compacted"),
            _ => None,
        };
        let landed = matches!(&driven, Ok(Ok(outcome)) if !outcome.cancelled) && spoiled.is_none();
        let mut answer = loop_answer(driven, &chat, &mut run, &system, &instruction, LOOP_TIMEOUT, started);
        if let Some(why) = spoiled {
            run.fail(why);
            answer = json!({ "ok": false, "error": why, "ms": answer["ms"] });
        }
        answer["from"] = json!(from);
        answer["compacted"] = json!(landed);
        if !landed {
            let back = within(scope(&conn), conversation::undo_fold(&history, run_id, from, &name)).await;
            answer["runId"] = if matches!(back, Ok(true)) { json!(from) } else { Value::Null };
        }
        let _ = tx.send(Event::default().event("done").data(answer.to_string())).await;
        state.unseat(&seat);
    });
    sse_of(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_api::tests::{call, call_text, events_of, one_round_answer, sse_wire_capturing};
    use swiss_host::dbbrowser::{DbDialect, EditError, PubSubFeed, PubSubWatch, SqlDump};
    use swiss_host::mongobrowser::{
        MongoAggregate, MongoCollectionOp, MongoCount, MongoDump, MongoEdit, MongoExplain, MongoFind, MongoImport, MongoIndexOp, MongoNs,
    };
    use swiss_host::services::catalog::{CatalogError, ConnectionCatalog, ConnectionInfo, LeaseTracker};
    use swiss_llm::agent::DB_AGENT_PROMPT;
    use swiss_llm::LlmConfig;
    use swiss_mcp::adapters::sql::CappedWrite;

    /// A PostgreSQL connection with one table, whose reads answer `rows` rows and whose
    /// read path refuses a DELETE as the real gate does; its writes pass the real auto gate,
    /// change 3 rows - 5000 for a WHERE on type - and fail on a table named missing.
    struct StubPg {
        rows: usize,
        ran: std::sync::Mutex<Vec<(String, Option<i64>)>>,
        wrote: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DbBrowser for StubPg {
        fn dialect(&self) -> DbDialect { DbDialect::Pg }
        fn label(&self) -> String { "mm @ db:5432".into() }
        async fn list_tables(&self, _o: &Value) -> Result<Value, String> {
            Ok(json!({ "tables": [{ "schema": "public", "name": "posts", "type": "table", "approxRows": 42, "size": "8 kB" }], "total": 1 }))
        }
        async fn read_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn describe_table(&self, o: &Value) -> Result<Value, String> {
            Ok(json!({ "schema": "public", "table": o["table"], "primaryKey": ["id"],
                "columns": [{ "name": "id", "dataType": "text", "nullable": false, "isPrimaryKey": true, "defaultValue": null, "comment": null },
                            { "name": "type", "dataType": "text", "nullable": true, "isPrimaryKey": false, "defaultValue": null, "comment": "post kind" }],
                "indexes": [{ "name": "posts_pkey", "unique": true, "primary": true, "columns": ["id"] }], "foreignKeys": [] }))
        }
        async fn apply_edits(&self, _o: &Value) -> Result<Value, EditError> { Err(EditError::Bad("unused".into())) }
        async fn run_query(&self, _sql: &str, _limit: Option<&Value>) -> Result<Value, String> { Err("the agent never runs the console's path".into()) }
        async fn run_read_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
            self.ran.lock().expect("ran").push((sql.to_string(), limit.and_then(Value::as_i64)));
            if sql.to_ascii_lowercase().starts_with("delete") {
                return Err("only a read runs here (SELECT, WITH, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN), not DELETE".into());
            }
            let rows: Vec<Value> = (0..self.rows).map(|i| json!({ "id": format!("p{i}"), "type": "custom_images".repeat(30) })).collect();
            Ok(json!({ "columns": ["id", "type"], "rowCount": rows.len(), "rows": rows }))
        }
        async fn run_agent_write(&self, sql: &str, cap: u64) -> Result<Value, WriteRefusal> {
            let sql = swiss_mcp::adapters::sql::assert_auto_write(sql).map_err(WriteRefusal::Held)?;
            self.wrote.lock().expect("wrote").push(sql.clone());
            if sql.contains("missing") {
                return Err(WriteRefusal::Failed("relation \"missing\" does not exist".into()));
            }
            let changed = if sql.contains("type") { 5000 } else { 3 };
            let outcome = if changed > cap {
                CappedWrite::Past { changed }
            } else {
                CappedWrite::Kept { changed, rows: Vec::new(), insert_id: sql.starts_with("INSERT").then_some(41) }
            };
            swiss_mcp::adapters::sql::capped_reply(outcome, cap)
        }
        async fn export_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn export_sql_dump(&self, _o: &Value) -> Result<SqlDump, String> { Err("unused".into()) }
        async fn import_table(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn ddl_op(&self, _o: &Value) -> Result<Value, String> { Err("unused".into()) }
        async fn activity(&self) -> Result<Value, String> { Err("unused".into()) }
        async fn activity_kill(&self, _pid: i64, _terminate: bool) -> Result<Value, String> { Err("unused".into()) }
        async fn completion(&self, _sql: &str, _caret: usize) -> Result<Value, String> { Err("unused".into()) }
    }

    /// A MongoDB connection with three collections in `app`, whose reads answer up to 120
    /// documents and whose read path refuses a delete as the real gate does.
    struct StubMongo {
        ran: std::sync::Mutex<Vec<(String, Value, i64)>>,
    }

    #[async_trait]
    impl MongoBrowser for StubMongo {
        fn label(&self) -> String { "app @ mongo:27017".into() }
        async fn server_info(&self) -> Result<Value, String> { Ok(json!({ "defaultDb": "app" })) }
        async fn list_databases(&self) -> Result<Value, String> {
            Ok(json!({ "primary": "app", "current": "app", "databases": [
                { "name": "app", "system": false }, { "name": "logs", "system": false }, { "name": "admin", "system": true }
            ] }))
        }
        async fn list_collections(&self, db: &str) -> Result<Value, String> {
            Ok(json!({ "db": db, "collections": [
                { "name": "posts", "type": "collection", "count": 1200 },
                { "name": "users", "type": "collection", "count": 3 },
                { "name": "recent", "type": "view", "viewOn": "posts" }
            ] }))
        }
        async fn find(&self, _q: &MongoFind) -> Result<Value, String> { Err("unused".into()) }
        async fn count(&self, _q: &MongoCount) -> Result<Value, String> { Err("unused".into()) }
        async fn aggregate(&self, _q: &MongoAggregate) -> Result<Value, String> { Err("unused".into()) }
        async fn explain(&self, _q: &MongoExplain) -> Result<Value, String> { Err("unused".into()) }
        async fn sample(&self, _ns: &MongoNs, _filter: &serde_json::Map<String, Value>, _size: i64) -> Result<Vec<Value>, String> { Err("unused".into()) }
        async fn indexes(&self, _ns: &MongoNs) -> Result<Value, String> { Err("unused".into()) }
        async fn index_op(&self, _ns: &MongoNs, _op: &MongoIndexOp) -> Result<Value, String> { Err("unused".into()) }
        async fn apply_edits(&self, _ns: &MongoNs, _edits: &[MongoEdit]) -> Result<Value, EditError> { Err(EditError::Bad("unused".into())) }
        async fn run_command(&self, _db: &str, _command: &serde_json::Map<String, Value>) -> Result<Value, String> { Err("the agent never runs the console's path".into()) }
        async fn collection_op(&self, _db: &str, _op: &MongoCollectionOp) -> Result<Value, String> { Err("unused".into()) }
        async fn export(&self, _q: &MongoFind, _relaxed: bool) -> Result<MongoDump, String> { Err("unused".into()) }
        async fn import(&self, _q: &MongoImport) -> Result<Value, String> { Err("unused".into()) }
        async fn activity(&self) -> Result<Value, String> { Err("unused".into()) }
        async fn activity_kill(&self, _opid: i64) -> Result<Value, String> { Err("unused".into()) }
        async fn run_read_command(&self, db: &str, command: &serde_json::Map<String, Value>, limit: i64) -> Result<Value, String> {
            self.ran.lock().expect("ran").push((db.to_string(), Value::Object(command.clone()), limit));
            if command.contains_key("delete") {
                return Err("the agent runs reads only (find, aggregate, count, ...), not delete".into());
            }
            if !command.contains_key("find") {
                return Ok(json!({ "reply": { "n": { "$numberInt": "3" }, "ok": { "$numberDouble": "1.0" } } }));
            }
            let docs: Vec<Value> = (0..limit.min(120))
                .map(|i| {
                    let kind = if i == 0 { "custom_images".repeat(30) } else { "post".to_string() };
                    json!({ "_id": { "$oid": format!("{i:024x}") }, "n": { "$numberInt": i.to_string() }, "type": kind })
                })
                .collect();
            Ok(json!({ "reply": { "cursor": { "firstBatch": docs, "id": 0, "ns": format!("{db}.posts") }, "ok": 1.0 }, "more": limit < 120 }))
        }
        async fn describe_for_agent(&self, db: &str, coll: &str) -> Result<Value, String> {
            if coll != "posts" {
                return Err(format!("no collection or view named \"{coll}\" in {db}"));
            }
            Ok(json!({ "db": db, "collection": coll, "type": "collection", "count": 1200,
                "indexes": [{ "name": "_id_", "key": { "_id": 1 } }],
                "schema": [{ "path": "type", "types": ["string"], "share": 1.0 }] }))
        }
    }

    /// A Redis connection on db0 whose reads answer a hash and whose read path refuses a
    /// DEL as the real gate does; a stream named feed with one consumer group; two channels
    /// with a subscriber, and feeds that answer by the channel they are asked for.
    struct StubRedis {
        ran: std::sync::Mutex<Vec<String>>,
        scans: std::sync::Mutex<Vec<Value>>,
        windows: std::sync::Mutex<Vec<Value>>,
        /// The quiet feeds' senders, held so the feed stays open until the listen lets go.
        open: std::sync::Mutex<Vec<mpsc::Sender<Value>>>,
        /// The feeds asked for so far: "slow" times out on its first two, "jam" on every one.
        opens: std::sync::Mutex<u32>,
    }

    #[async_trait]
    impl RedisBrowser for StubRedis {
        fn label(&self) -> String { "cache @ redis:6379 db 0".into() }
        async fn list_keys(&self, o: &Value) -> Result<Value, String> {
            self.scans.lock().expect("scans").push(o.clone());
            Ok(json!({ "keys": [{ "key": "post:1", "type": "hash", "ttl": -1 }, { "key": "session:9", "type": "string", "ttl": 120 }],
                "cursor": "17", "done": false, "total": 2400 }))
        }
        async fn read_key(&self, key: &str) -> Result<Value, String> {
            Ok(json!({ "key": key, "type": "hash", "ttl": -1, "value": { "title": "hi" } }))
        }
        async fn run_command(&self, _line: &str) -> Result<Value, String> { Err("the agent never runs the console's path".into()) }
        async fn run_read_command(&self, line: &str) -> Result<Value, String> {
            self.ran.lock().expect("ran").push(line.to_string());
            if line.to_ascii_uppercase().starts_with("DEL") {
                return Err("only a read runs here, and DEL is not one this gate knows".into());
            }
            Ok(json!(["title", "hi", "type", "custom"]))
        }
        async fn run_pipeline(&self, _commands: &[Vec<String>]) -> Result<Value, String> { Err("unused".into()) }
        async fn list_databases(&self) -> Result<Value, String> {
            Ok(json!({ "primary": "0", "current": "0", "databases": [{ "name": "0", "tables": 2400 }, { "name": "2", "tables": 170 }] }))
        }
        async fn read_stream(&self, key: &str, o: &Value) -> Result<Value, String> {
            self.windows.lock().expect("windows").push(o.clone());
            match key {
                "feed" => Ok(json!({ "key": "feed", "type": "stream", "length": 5000, "firstId": "1-0", "lastId": "1700000000600-0",
                    "entries": [
                        { "id": "1700000000600-0", "fields": { "price": "1.5", "symbol": "NVDA" } },
                        { "id": "1700000000500-0", "fields": { "symbol": "AAPL" } }
                    ], "more": true })),
                "nope" => Ok(json!({ "key": "nope", "type": "none", "ttl": -2, "value": null })),
                other => Err(format!("{other} is a hash, not a stream")),
            }
        }
        async fn stream_groups(&self, key: &str) -> Result<Value, String> {
            Ok(json!({ "key": key, "groups": [{ "name": "g1", "consumers": 1, "pending": 7, "lag": 9993, "last-delivered-id": "1700000000600-0" }] }))
        }
        async fn pubsub_channels(&self, o: &Value) -> Result<Value, String> {
            let pattern = o["pattern"].as_str().unwrap_or("*");
            if pattern == "zzz" {
                return Ok(json!({ "pattern": pattern, "channels": [], "total": 0, "truncated": false, "patterns": 1 }));
            }
            Ok(json!({ "pattern": pattern, "channels": [{ "name": "news", "subscribers": 3 }, { "name": "orders", "subscribers": 1 }],
                "total": 2, "truncated": false, "patterns": 1 }))
        }
        async fn subscribe(&self, watch: &PubSubWatch) -> Result<PubSubFeed, String> {
            let opened = {
                let mut opens = self.opens.lock().expect("opens");
                *opens += 1;
                *opens
            };
            match watch.channels.first().map(String::as_str) {
                Some("slow") if opened <= 2 => return Err("redis connect timed out after 5s".into()),
                Some("jam") => return Err("redis connect timed out after 5s".into()),
                Some("denied") => return Err("NOPERM this user has no permissions to access the 'denied' channel".into()),
                _ => {}
            }
            let (tx, rx) = mpsc::channel(16);
            let send = |event: Value| tx.try_send(event).expect("feed room");
            send(json!({ "event": "subscribed", "channels": watch.channels, "patterns": watch.patterns }));
            match watch.channels.first().map(String::as_str) {
                Some("news") => {
                    send(json!({ "event": "dropped", "count": 3 }));
                    send(json!({ "event": "message", "channel": "news", "payload": "hello", "size": 5 }));
                    send(json!({ "event": "message", "channel": "news.eu", "pattern": "news.*", "payload": "x".repeat(400), "size": 400 }));
                    send(json!({ "event": "message", "channel": "news", "payload": "third", "size": 5 }));
                    self.open.lock().expect("open").push(tx);
                }
                Some("gone") => send(json!({ "event": "closed", "reason": "the server closed the connection" })),
                _ => self.open.lock().expect("open").push(tx),
            }
            Ok(rx)
        }
    }

    struct Stub {
        pg: Arc<StubPg>,
        mongo: Arc<StubMongo>,
        redis: Arc<StubRedis>,
        tracker: Arc<LeaseTracker>,
    }

    impl Stub {
        fn new(rows: usize) -> Stub {
            Stub {
                pg: Arc::new(StubPg { rows, ran: std::sync::Mutex::new(Vec::new()), wrote: std::sync::Mutex::new(Vec::new()) }),
                mongo: Arc::new(StubMongo { ran: std::sync::Mutex::new(Vec::new()) }),
                redis: Arc::new(StubRedis {
                    ran: std::sync::Mutex::new(Vec::new()),
                    scans: std::sync::Mutex::new(Vec::new()),
                    windows: std::sync::Mutex::new(Vec::new()),
                    open: std::sync::Mutex::new(Vec::new()),
                    opens: std::sync::Mutex::new(0),
                }),
                tracker: LeaseTracker::new(),
            }
        }
    }

    impl ConnectionCatalog for Stub {
        fn list(&self) -> Vec<ConnectionInfo> {
            vec![ConnectionInfo { id: "mm".into(), label: self.pg.label(), dialect: "pg".into(), state: "running".into() }]
        }
        fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError> {
            match id {
                "mm" | "mm2" => Ok(self.tracker.grant(id, holder, "pg", BrowserFlavor::Db(self.pg.clone()))),
                "docs" => Ok(self.tracker.grant(id, holder, "mongo", BrowserFlavor::Mongo(self.mongo.clone()))),
                "cache" => Ok(self.tracker.grant(id, holder, "redis", BrowserFlavor::Redis(self.redis.clone()))),
                "web" => Ok(self.tracker.grant(id, holder, "http", BrowserFlavor::None)),
                _ => Err(CatalogError::Unknown(format!("unknown MCP: {id}"))),
            }
        }
    }

    fn catalog(rows: usize) -> (Arc<CatalogRegistry>, Arc<StubPg>) {
        let (catalog, stub) = stubs(rows);
        (catalog, stub.pg.clone())
    }

    /// The catalog with every kind of connection, and the stub that serves them.
    fn stubs(rows: usize) -> (Arc<CatalogRegistry>, Arc<Stub>) {
        let stub = Arc::new(Stub::new(rows));
        let catalog = Arc::new(CatalogRegistry::new());
        catalog.register(stub.clone(), "mcp").expect("register");
        (catalog, stub)
    }

    /// The frames the page was sent, each as its Debug form with the quotes unescaped.
    fn sent(rx: &mut mpsc::Receiver<Event>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(format!("{event:?}").replace("\\\"", "\""));
        }
        out
    }

    fn frames(rx: &mut mpsc::Receiver<Event>) -> usize {
        let mut seen = 0;
        while rx.try_recv().is_ok() {
            seen += 1;
        }
        seen
    }

    #[tokio::test]
    async fn a_read_reaches_the_page_whole_and_the_model_reads_its_first_rows_clipped() {
        let (catalog, pg) = catalog(80);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "mm", DbKind::Sql, Some(tx));
        let answer = tools.call("run_sql", &json!({ "sql": "SELECT * FROM posts" })).await;
        assert!(answer.ok, "{}", answer.text);
        assert!(answer.text.starts_with("80 row(s). The whole result is open on the user's page."), "{}", answer.text);
        assert!(answer.text.contains("The first 50, one JSON array per row:"), "{}", answer.text);
        assert_eq!(answer.text.lines().filter(|l| l.starts_with('[')).count(), 50);
        assert!(answer.text.contains('…'), "a long cell is clipped: {}", answer.text);
        assert_eq!(frames(&mut rx), 1, "one db frame for the page");
        // A refused write is the model's answer and the page's failed tab, never a run.
        let refused = tools.call("run_sql", &json!({ "sql": "DELETE FROM posts" })).await;
        assert!(!refused.ok && refused.text.contains("not DELETE"), "{}", refused.text);
        assert_eq!(frames(&mut rx), 1);
        // A staged write goes to the console and nowhere near the connection.
        let staged = tools.call("stage_sql", &json!({ "sql": "DELETE FROM posts WHERE id = 'p1'" })).await;
        assert!(staged.ok && staged.text.contains("has NOT run"));
        assert_eq!(frames(&mut rx), 1);
        assert_eq!(pg.ran.lock().expect("ran").len(), 2, "the stage ran nothing");
        // The page's row limit is the default and the most; a model may only ask fewer.
        let mut tools = DbTools::new(tools.catalog.clone(), "mm", DbKind::Sql, None).limited(500);
        tools.call("run_sql", &json!({ "sql": "SELECT 1" })).await;
        tools.call("run_sql", &json!({ "sql": "SELECT 2", "limit": 9999 })).await;
        tools.call("run_sql", &json!({ "sql": "SELECT 3", "limit": 20 })).await;
        let limits: Vec<Option<i64>> = pg.ran.lock().expect("ran").iter().skip(2).map(|(_, limit)| *limit).collect();
        assert_eq!(limits, [Some(500), Some(500), Some(20)]);
    }

    /// Staged writes (the default) never reach the write path: run_sql refuses one as the read
    /// gate does.
    #[tokio::test]
    async fn a_staged_write_never_reaches_the_write_path() {
        let (catalog, pg) = catalog(2);
        let mut staged = DbTools::new(catalog, "mm", DbKind::Sql, None);
        let refused = staged.call("run_sql", &json!({ "sql": "DELETE FROM posts WHERE id = 'p1'" })).await;
        assert!(!refused.ok && refused.text.contains("not DELETE"), "{}", refused.text);
        assert!(pg.wrote.lock().expect("wrote").is_empty());
    }

    /// The snapshot names the tables while they are few, and only counts a database with
    /// more than TABLES_NAMED: a thousand names never ride every request.
    #[test]
    fn the_snapshot_names_a_few_tables_and_counts_many() {
        let table = |name: &str| json!({ "schema": "public", "name": name });
        let few = json!({ "tables": [table("posts"), table("users")], "total": 2 });
        assert_eq!(tables_fact(&few), "- Tables (2): public.posts, public.users");
        let many: Vec<Value> = (0..TABLES_NAMED).map(|i| table(&format!("t{i}"))).collect();
        let fact = tables_fact(&json!({ "tables": many, "total": 1200 }));
        assert_eq!(fact, "- Tables: 1200, too many to name here; list_tables finds them by name (grep)");
    }

    #[tokio::test]
    async fn the_structure_and_the_listing_read_as_lines() {
        let (catalog, _pg) = catalog(0);
        let mut tools = DbTools::new(catalog, "mm", DbKind::Sql, None);
        let listed = tools.call("list_tables", &json!({ "grep": "post" })).await;
        assert_eq!(listed.text, "1 of 1 table(s): schema.name, type, about how many rows\npublic.posts  table  ~42 rows");
        let described = tools.call("describe_table", &json!({ "table": "posts" })).await;
        assert_eq!(
            described.text,
            "public.posts\nColumns:\n- id text PRIMARY KEY NOT NULL\n- type text  -- post kind\nIndexes:\n- posts_pkey (id) primary"
        );
        let empty = tools.call("run_sql", &json!({ "sql": "SELECT 1 WHERE false" })).await;
        assert_eq!(empty.text, "0 row(s). The whole result is open on the user's page.");
        assert!(!tools.call("describe_table", &json!({})).await.ok);
        let elsewhere = DbTools::new(tools.catalog.clone(), "web", DbKind::Sql, None).call("list_tables", &json!({})).await;
        assert!(!elsewhere.ok && elsewhere.text.contains("works on MySQL, PostgreSQL, MongoDB and Redis"), "{}", elsewhere.text);
    }

    /// What the agent finds and describes the reader sees: a listing sends its grep for the
    /// sidebar's search (none clears it), a description opens the table or the collection; a
    /// refused call tells the page nothing.
    #[tokio::test]
    async fn a_listing_and_a_description_show_on_the_page() {
        let (catalog, _pg) = catalog(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "mm", DbKind::Sql, Some(tx));
        tools.call("list_tables", &json!({ "grep": "post" })).await;
        tools.call("list_tables", &json!({})).await;
        tools.call("describe_table", &json!({ "table": "posts" })).await;
        tools.call("describe_table", &json!({})).await;
        let frames = sent(&mut rx);
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert!(frames[0].contains(r#""kind":"list""#) && frames[0].contains(r#""grep":"post""#), "{}", frames[0]);
        assert!(frames[1].contains(r#""grep":"""#), "{}", frames[1]);
        assert!(frames[2].contains(r#""kind":"table""#) && frames[2].contains(r#""table":"posts""#) && frames[2].contains(r#""schema":"public""#), "{}", frames[2]);
        let (catalog, _stub) = stubs(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "docs", DbKind::Mongo, Some(tx));
        tools.call("list_collections", &json!({ "grep": "post|rec" })).await;
        tools.call("describe_collection", &json!({ "collection": "posts" })).await;
        tools.call("describe_collection", &json!({ "collection": "nope" })).await;
        let frames = sent(&mut rx);
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert!(frames[0].contains(r#""kind":"list""#) && frames[0].contains(r#""grep":"post|rec""#) && frames[0].contains(r#""db":"app""#), "{}", frames[0]);
        assert!(frames[1].contains(r#""kind":"coll""#) && frames[1].contains(r#""db":"app""#) && frames[1].contains(r#""coll":"posts""#), "{}", frames[1]);
    }

    /// A connection's kind names its own tools: another kind's name is no tool there, and
    /// a connection that is no longer of the conversation's kind refuses the call.
    #[tokio::test]
    async fn each_kind_serves_its_own_tools() {
        let (catalog, _stub) = stubs(0);
        for (kind, conn) in [(DbKind::Sql, "mm"), (DbKind::Mongo, "docs"), (DbKind::Redis, "cache")] {
            let tools = DbTools::new(catalog.clone(), conn, kind, None);
            let names: Vec<String> = tools.specs().into_iter().map(|spec| spec.name).collect();
            let catalogued: Vec<&str> = db_tools_on(kind.as_str()).iter().chain(db_tools_shared()).map(|tool| tool.name).collect();
            assert_eq!(names, catalogued, "{kind:?}");
        }
        let mut sql = DbTools::new(catalog.clone(), "mm", DbKind::Sql, None);
        let foreign = sql.call("run_mongo", &json!({ "command": { "find": "posts" } })).await;
        assert!(!foreign.ok && foreign.text == "no tool named run_mongo", "{}", foreign.text);
        let moved = DbTools::new(catalog.clone(), "cache", DbKind::Sql, None).call("list_tables", &json!({})).await;
        assert!(!moved.ok && moved.text.contains("no longer a MySQL or PostgreSQL connection"), "{}", moved.text);
        let none = DbTools::new(catalog, "web", DbKind::Redis, None).call("scan_keys", &json!({})).await;
        assert!(!none.ok && none.text.contains("works on MySQL, PostgreSQL, MongoDB and Redis"), "{}", none.text);
    }

    #[test]
    fn a_grep_speaks_the_listing_grammar() {
        assert!(grep_passes("post", "Posts"));
        assert!(grep_passes("user|post", "posts") && !grep_passes("user|log", "posts"));
        assert!(grep_passes("po, ts", "posts") && !grep_passes("po,x", "posts"));
        assert!(grep_passes("po*", "posts") && !grep_passes("os*", "posts"), "a * anchors the term");
        assert!(grep_passes("*s", "posts") && grep_passes("p*t*s", "posts") && !grep_passes("p*x*s", "posts"));
        assert!(grep_passes("", "posts"));
    }

    /// run_mongo: the page gets the whole reply with its database, the model the count and
    /// the first documents in relaxed Extended JSON, long strings clipped; a write is refused
    /// and a stage runs nothing.
    #[tokio::test]
    async fn a_mongo_read_reaches_the_page_and_the_model_reads_its_first_documents() {
        let (catalog, stub) = stubs(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog.clone(), "docs", DbKind::Mongo, Some(tx)).limited(100).in_db(Some("app".into()));
        let found = tools.call("run_mongo", &json!({ "command": { "find": "posts", "filter": {} } })).await;
        assert!(found.ok, "{}", found.text);
        assert!(found.text.starts_with("100 document(s), and more past the row limit. The whole result is open on the user's page."), "{}", found.text);
        assert!(found.text.contains("The first 50, one JSON document per line:"), "{}", found.text);
        let lines: Vec<&str> = found.text.lines().filter(|l| l.starts_with('{')).collect();
        assert_eq!(lines.len(), 50);
        assert!(lines[1].contains("\"n\":1") && lines[1].contains("{\"$oid\":"), "relaxed: {}", lines[1]);
        assert!(found.text.contains('…'), "a long string is clipped");
        assert_eq!(frames(&mut rx), 1, "one db frame for the page");
        // The command as JSON text reads the same; a command that is no cursor reads its reply.
        let counted = tools.call("run_mongo", &json!({ "command": "{\"count\": \"posts\"}", "db": "logs" })).await;
        assert!(counted.ok && counted.text.starts_with("The command answered") && counted.text.contains("\"n\":3"), "{}", counted.text);
        assert!(!tools.call("run_mongo", &json!({ "command": "find posts" })).await.ok);
        let refused = tools.call("run_mongo", &json!({ "command": { "delete": "posts", "deletes": [] } })).await;
        assert!(!refused.ok && refused.text.contains("not delete"), "{}", refused.text);
        let staged = tools.call("stage_mongo", &json!({ "command": { "delete": "posts", "deletes": [{ "q": {}, "limit": 1 }] } })).await;
        assert!(staged.ok && staged.text.contains("has NOT run"));
        assert_eq!(frames(&mut rx), 3, "the count, the refused write and the stage");
        let ran = stub.mongo.ran.lock().expect("ran").clone();
        let dbs: Vec<&str> = ran.iter().map(|(db, _, _)| db.as_str()).collect();
        assert_eq!(dbs, ["app", "logs", "app"], "the model's db, else the one in view; the stage ran nothing");
        // Without a database in view the connection's own is the default; a model's limit only lowers.
        let mut tools = DbTools::new(catalog, "docs", DbKind::Mongo, None).limited(100);
        tools.call("run_mongo", &json!({ "command": { "find": "posts" }, "limit": 9999 })).await;
        tools.call("run_mongo", &json!({ "command": { "find": "posts" }, "limit": 5 })).await;
        let tail: Vec<(String, i64)> = stub.mongo.ran.lock().expect("ran").iter().skip(3).map(|(db, _, limit)| (db.clone(), *limit)).collect();
        assert_eq!(tail, [("app".to_string(), 100), ("app".to_string(), 5)]);
    }

    #[tokio::test]
    async fn the_collections_and_a_description_read_as_lines() {
        let (catalog, _stub) = stubs(0);
        let mut tools = DbTools::new(catalog, "docs", DbKind::Mongo, None);
        let listed = tools.call("list_collections", &json!({ "grep": "post|rec" })).await;
        assert_eq!(listed.text, "2 of 2 collection(s) in app: name, type, about how many documents\nposts  collection  ~1200 docs\nrecent  view  on posts");
        let none = tools.call("list_collections", &json!({ "grep": "zzz", "db": "logs" })).await;
        assert_eq!(none.text, "No collection in logs matches.");
        let described = tools.call("describe_collection", &json!({ "collection": "posts" })).await;
        assert!(described.ok && described.text.contains("\"collection\":\"posts\"") && described.text.contains("\"_id_\""), "{}", described.text);
        assert!(!tools.call("describe_collection", &json!({ "collection": "nope" })).await.ok);
        assert!(!tools.call("describe_collection", &json!({})).await.ok);
    }

    /// The Redis keys: a scan page says where it stands before its keys, a key reads whole,
    /// a read command's reply reaches the page and the model, a write is refused and a stage
    /// runs nothing.
    #[tokio::test]
    async fn a_redis_read_reaches_the_page_and_a_scan_says_where_it_stands() {
        let (catalog, stub) = stubs(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "cache", DbKind::Redis, Some(tx));
        let scanned = tools.call("scan_keys", &json!({ "pattern": "post:*" })).await;
        assert_eq!(
            scanned.text,
            "2 key(s) on this page of the scan (the database holds 2400); it goes on: call scan_keys again with cursor \"17\" and the same pattern and type.\nkey, type, TTL:\npost:1  hash  no TTL\nsession:9  string  expires in 120s"
        );
        assert_eq!(stub.redis.scans.lock().expect("scans")[0], json!({ "count": KEYS_SCANNED, "fill": KEYS_SCANNED, "pattern": "post:*" }));
        assert_eq!(frames(&mut rx), 0, "finding shows nothing on the page");
        let read = tools.call("read_key", &json!({ "key": "post:1" })).await;
        assert!(read.ok && read.text.contains("\"title\":\"hi\""), "{}", read.text);
        assert_eq!(frames(&mut rx), 1, "the key read opens on the page");
        let ran = tools.call("run_redis", &json!({ "command": "HGETALL post:1" })).await;
        assert!(ran.ok && ran.text.starts_with("Reply (the whole reply is open on the user's page):\n[\"title\",\"hi\""), "{}", ran.text);
        assert_eq!(frames(&mut rx), 1, "the read's db frame");
        let refused = tools.call("run_redis", &json!({ "command": "DEL post:1" })).await;
        assert!(!refused.ok && refused.text.contains("DEL is not one"), "{}", refused.text);
        assert!(!tools.call("run_redis", &json!({ "command": "GET a\nGET b" })).await.ok, "one line");
        let staged = tools.call("stage_redis", &json!({ "command": "DEL post:1" })).await;
        assert!(staged.ok && staged.text.contains("has NOT run"));
        assert_eq!(frames(&mut rx), 2, "the refused write and the stage");
        assert_eq!(*stub.redis.ran.lock().expect("ran"), ["HGETALL post:1", "DEL post:1"]);
    }

    /// The memory's three on every kind: a save files a note and tells the page, a read
    /// without a name lists the index and with one gives the note whole, a delete removes it;
    /// a missing note or a bad name is refused, nothing told to the page.
    #[tokio::test]
    async fn the_memory_saves_reads_and_deletes_a_note_and_the_page_hears_each_change() {
        let (catalog, _stub) = stubs(0);
        let root = swiss_core::paths::test_home().join(format!("db-memory-{}", swiss_core::util::random_hex(8)));
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "cache", DbKind::Redis, Some(tx)).remembering(DbMemory::at(&root, "cache"));
        let empty = tools.call("read_memory", &json!({})).await;
        assert_eq!((empty.ok, empty.text.as_str()), (true, "No notes yet on this connection."));
        let saved = tools
            .call("save_memory", &json!({ "name": "live-room-bound-user", "description": "live room -> its bound user", "body": "HGET live_room:{id} anchor_uid, then user:{uid}" }))
            .await;
        assert!(saved.ok && saved.text.starts_with("Saved live-room-bound-user (a new note)."), "{}", saved.text);
        assert_eq!(frames(&mut rx), 1, "the page hears the save");
        let listed = tools.call("read_memory", &json!({})).await;
        assert!(listed.text.starts_with("1 notes on this connection:\n- live-room-bound-user: live room -> its bound user (saved "), "{}", listed.text);
        let read = tools.call("read_memory", &json!({ "name": "live-room-bound-user" })).await;
        assert!(read.ok && read.text.starts_with("# live-room-bound-user (saved "), "{}", read.text);
        assert!(read.text.ends_with("live room -> its bound user\n\nHGET live_room:{id} anchor_uid, then user:{uid}"), "{}", read.text);
        let replaced = tools.call("save_memory", &json!({ "name": "live-room-bound-user", "description": "the bound user", "body": "live_room:{id}" })).await;
        assert!(replaced.text.contains("(replaced the earlier one)"), "{}", replaced.text);
        let missing = tools.call("read_memory", &json!({ "name": "orders" })).await;
        assert!(!missing.ok && missing.text.starts_with("no note named orders. 1 notes"), "{}", missing.text);
        let unnamed = tools.call("save_memory", &json!({ "description": "x", "body": "y" })).await;
        assert!(!unnamed.ok && unnamed.text.starts_with("name:"), "{}", unnamed.text);
        assert_eq!(frames(&mut rx), 1, "the replace; nothing for a refusal");
        let deleted = tools.call("delete_memory", &json!({ "name": "live-room-bound-user" })).await;
        assert_eq!((deleted.ok, deleted.text.as_str()), (true, "Deleted live-room-bound-user."));
        assert!(!tools.call("delete_memory", &json!({ "name": "live-room-bound-user" })).await.ok, "gone already");
        assert_eq!(frames(&mut rx), 1, "the delete");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// read_stream: the stream's length and ends, its entries newest first one a line, where
    /// the older ones go on, its groups; the count is bounded, a missing key says so and a key
    /// of another type is refused. A stream read opens on the page, a missing key does not.
    #[tokio::test]
    async fn a_stream_reads_newest_first_with_its_consumer_groups() {
        let (catalog, stub) = stubs(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "cache", DbKind::Redis, Some(tx));
        let read = tools.call("read_stream", &json!({ "key": "feed" })).await;
        assert!(read.ok, "{}", read.text);
        assert_eq!(frames(&mut rx), 1, "the stream opens on the page");
        assert_eq!(
            read.text,
            "Stream feed: 5000 entries, first id 1-0, last id 1700000000600-0.\n2 entr(ies), newest first, one a line - id, then field=value:\n1700000000600-0  price=1.5  symbol=NVDA\n1700000000500-0  symbol=AAPL\nOlder entries go on: call read_stream again with before \"1700000000500-0\".\nConsumer groups (1): name, consumers, pending, lag, last delivered id:\ng1  1  7  9993  1700000000600-0"
        );
        tools.call("read_stream", &json!({ "key": "feed", "count": 500, "before": "1700000000500-0", "match": "symbol=NVDA" })).await;
        assert_eq!(
            *stub.redis.windows.lock().expect("windows"),
            [json!({ "count": STREAM_ENTRIES }), json!({ "count": STREAM_ENTRIES_MAX, "before": "1700000000500-0", "match": "symbol=NVDA" })]
        );
        let missing = tools.call("read_stream", &json!({ "key": "nope" })).await;
        assert_eq!((missing.ok, missing.text.as_str()), (true, "There is no key named nope."));
        let other = tools.call("read_stream", &json!({ "key": "post:1" })).await;
        assert!(!other.ok && other.text.contains("not a stream"), "{}", other.text);
        assert!(!tools.call("read_stream", &json!({})).await.ok);
        assert_eq!(frames(&mut rx), 1, "the second read opens it again; a missing key or another type opens nothing");
    }

    /// list_channels names the channels with their subscribers; listen_channels reads what a
    /// feed hears - stopping at the count asked, at the time, or when the feed closes - and
    /// says what it missed; a listen that names nothing is refused. Each one that ran shows on
    /// the page's Pub/Sub tab, a listen with its names and messages.
    #[tokio::test(start_paused = true)]
    async fn the_channels_and_what_a_listen_hears_read_as_lines() {
        let (catalog, stub) = stubs(0);
        let (tx, mut rx) = mpsc::channel::<Event>(8);
        let mut tools = DbTools::new(catalog, "cache", DbKind::Redis, Some(tx));
        let listed = tools.call("list_channels", &json!({})).await;
        assert_eq!(
            listed.text,
            "2 channel(s) with a subscriber right now; the server holds 1 pattern subscription(s) (PSUBSCRIBE), whose channels are not counted here.\nchannel, subscribers:\nnews  3\norders  1"
        );
        let none = tools.call("list_channels", &json!({ "pattern": "zzz" })).await;
        assert!(none.text.starts_with("No channel matching zzz has a subscriber right now;"), "{}", none.text);
        let heard = tools.call("listen_channels", &json!({ "channels": ["news"], "patterns": "news.*", "messages": 2 })).await;
        assert!(heard.ok, "{}", heard.text);
        let lines: Vec<&str> = heard.text.lines().collect();
        assert_eq!(
            lines[0],
            "Listened 0.0 s to channel(s) news and pattern(s) news.*: 2 message(s), stopped at the 2 asked for. One a line - channel (and the pattern that heard it), then the payload:"
        );
        assert_eq!(lines[1], "news  hello");
        assert_eq!(lines[2], format!("news.eu (via news.*)  {}…", "x".repeat(PAYLOAD_SHOWN)));
        assert_eq!(lines[3], "3 more message(s) came past the feed's rate cap and were not read.");
        assert_eq!(lines.len(), 4);
        let quiet = tools.call("listen_channels", &json!({ "channels": "quiet", "seconds": 1 })).await;
        assert_eq!(
            quiet.text,
            "No message was published on channel(s) quiet in 1.0 s. Pub/sub keeps nothing: only what is published while someone listens is heard."
        );
        let gone = tools.call("listen_channels", &json!({ "channels": ["gone"] })).await;
        assert!(gone.text.ends_with("The subscription ended early: the server closed the connection."), "{}", gone.text);
        let nothing = tools.call("listen_channels", &json!({})).await;
        assert!(!nothing.ok && nothing.text.contains("at least one channel or pattern"), "{}", nothing.text);
        // A connect that times out is tried again, three tries in all; another failure is not.
        *stub.redis.opens.lock().expect("opens") = 0;
        let slow = tools.call("listen_channels", &json!({ "channels": ["slow"], "seconds": 1 })).await;
        assert!(slow.ok && slow.text.starts_with("No message was published on channel(s) slow"), "{}", slow.text);
        assert_eq!(*stub.redis.opens.lock().expect("opens"), 3, "two timeouts, then the feed");
        *stub.redis.opens.lock().expect("opens") = 0;
        let jam = tools.call("listen_channels", &json!({ "channels": ["jam"] })).await;
        assert_eq!(
            (jam.ok, jam.text.as_str()),
            (false, "redis connect timed out after 5s, 3 tries in a row: the link to the server is busy - try again, or listen on fewer names")
        );
        assert_eq!(*stub.redis.opens.lock().expect("opens"), 3);
        *stub.redis.opens.lock().expect("opens") = 0;
        let denied = tools.call("listen_channels", &json!({ "channels": ["denied"] })).await;
        assert!(!denied.ok && denied.text.starts_with("NOPERM"), "{}", denied.text);
        assert_eq!(*stub.redis.opens.lock().expect("opens"), 1, "a refusal is not tried again");
        assert!(stub.redis.open.lock().expect("open").iter().all(mpsc::Sender::is_closed), "every listen let its feed go");
        // Each listing and each listen opens the page's Pub/Sub tab (the slow one once, on the try
        // that got through); a refused listen does not.
        assert_eq!(frames(&mut rx), 6);
        let heard = Heard {
            what: String::new(), seconds: 2.5, messages: vec![json!({ "event": "message", "channel": "news", "payload": "hi", "size": 2, "at": 7 })],
            cap: None, dropped: 3, ended: Some("the feed closed".into()),
        };
        assert_eq!(
            heard_frame(&["news".to_string()], &["news.*".to_string()], &heard),
            json!({ "kind": "listen", "channels": ["news"], "patterns": ["news.*"], "seconds": 2.5,
                "messages": [{ "event": "message", "channel": "news", "payload": "hi", "size": 2, "at": 7 }], "dropped": 3, "ended": "the feed closed" })
        );
    }

    #[test]
    fn the_snapshot_counts_a_keyspace_and_names_a_few_collections() {
        let listing = json!({ "current": "0", "databases": [{ "name": "0", "tables": 2400 }, { "name": "2", "tables": 170 }] });
        assert_eq!(keyspace_fact(&listing), "- Database: db0, 2400 key(s); others hold keys (db2 170), out of this connection's reach");
        assert_eq!(keyspace_fact(&json!({ "current": "0", "databases": [] })), "- Database: db0, 0 key(s)");
        let channels = json!({ "channels": [{ "name": "news" }, { "name": "orders" }], "total": 2, "patterns": 1 });
        assert_eq!(pubsub_fact(&channels), "- Pub/sub: 2 channel(s) with a subscriber (news, orders), 1 pattern subscription(s); list_channels and listen_channels read them");
        let crowd: Vec<Value> = (0..=CHANNELS_NAMED).map(|i| json!({ "name": format!("c{i}") })).collect();
        let total = CHANNELS_NAMED + 1;
        assert_eq!(
            pubsub_fact(&json!({ "channels": crowd, "total": total, "patterns": 0 })),
            format!("- Pub/sub: {total} channel(s) with a subscriber, 0 pattern subscription(s); list_channels and listen_channels read them")
        );
        assert!(pubsub_fact(&json!({ "channels": [], "total": 0, "patterns": 0 })).starts_with("- Pub/sub: no channel with a subscriber, 0 pattern"));
        let few = json!({ "collections": [{ "name": "posts" }, { "name": "users" }] });
        assert_eq!(collections_fact(&few), "- Collections (2): posts, users");
        let many: Vec<Value> = (0..=TABLES_NAMED).map(|i| json!({ "name": format!("c{i}") })).collect();
        assert_eq!(
            collections_fact(&json!({ "collections": many })),
            format!("- Collections: {}, too many to name here; list_collections finds them by name (grep)", TABLES_NAMED + 1)
        );
    }

    /// A tool ask in the OpenAI stream shape: one call of `name` with `arguments`.
    fn tool_ask(name: &str, arguments: Value) -> String {
        let call = json!({ "id": "c1", "object": "chat.completion.chunk", "model": "tiny", "choices": [{ "index": 0,
            "delta": { "role": "assistant", "tool_calls": [{ "index": 0, "id": "call_1", "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() } }] }, "finish_reason": null }] });
        let end = json!({ "id": "c1", "object": "chat.completion.chunk", "model": "tiny",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11 } });
        format!("data: {call}\n\ndata: {end}\n\ndata: [DONE]\n\n")
    }

    /// A MongoDB send: the snapshot names the database in view, its collections and the
    /// others, the model is handed the MongoDB four, and the command's db frame carries the
    /// database it ran in.
    #[tokio::test]
    async fn a_mongo_send_opens_on_the_database_in_view_with_the_mongo_tools() {
        let ask = tool_ask("run_mongo", json!({ "command": { "find": "posts", "limit": 2 } }));
        let (app, captured, _pg) = app(vec![ask, one_round_answer()], db_agent()).await;
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/stream",
            Some(json!({ "agent": "data", "conn": "docs", "prompt": "two posts", "schema": "app", "rowLimit": 50 }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["run", "usage", "tool", "db", "toolResult", "delta", "usage", "final", "done"], "{text}");
        let db = &events[3].1;
        assert_eq!((db["kind"].as_str(), db["db"].as_str()), (Some("run"), Some("app")), "{db}");
        assert_eq!(db["sql"].as_str(), Some("{\"find\":\"posts\",\"limit\":2}"));
        assert_eq!(db["reply"]["reply"]["cursor"]["firstBatch"].as_array().map(Vec::len), Some(50), "the stub ignores the find's own limit");
        assert_eq!(db["reply"]["limitApplied"].as_i64(), Some(50), "the page says where the result was cut");
        let first: Value = serde_json::from_str(&captured.lock().expect("captured")[0]).expect("ask");
        let system = first["messages"][0]["content"].as_str().unwrap_or_default().to_string();
        assert!(system.contains("- Connection: docs (mongo, app @ mongo:27017)"), "{system}");
        assert!(system.contains("- Dialect: MongoDB. run_mongo takes a database command document"), "{system}");
        assert!(system.contains("- Database in view on the page (the tools' default): app"), "{system}");
        assert!(system.contains("- Collections (3): posts, users, recent"), "{system}");
        assert!(system.contains("- Databases (2): app, logs"), "{system}");
        let tools: Vec<&str> = first["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).collect();
        assert_eq!(tools, ["list_collections", "describe_collection", "run_mongo", "stage_mongo", "open_console", "read_memory", "save_memory", "delete_memory"]);
    }

    /// A Redis send: the snapshot reads the keyspace and its pub/sub, and the model is handed
    /// the Redis tools.
    #[tokio::test]
    async fn a_redis_send_opens_on_the_keyspace_with_the_redis_tools() {
        let (app, captured, _pg) = app(vec![one_round_answer()], db_agent()).await;
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "cache", "prompt": "how many keys" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let first: Value = serde_json::from_str(&captured.lock().expect("captured")[0]).expect("ask");
        let system = first["messages"][0]["content"].as_str().unwrap_or_default().to_string();
        assert!(system.contains("- Connection: cache (redis, cache @ redis:6379 db 0)"), "{system}");
        assert!(system.contains("- Dialect: Redis. run_redis takes ONE command line"), "{system}");
        assert!(system.contains("- Database: db0, 2400 key(s); others hold keys (db2 170)"), "{system}");
        assert!(system.contains("- Pub/sub: 2 channel(s) with a subscriber (news, orders), 1 pattern subscription(s)"), "{system}");
        assert!(system.contains("A pub/sub channel is no key"), "{system}");
        let tools: Vec<&str> = first["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).collect();
        assert_eq!(
            tools,
            ["scan_keys", "read_key", "read_stream", "list_channels", "listen_channels", "run_redis", "stage_redis", "open_console", "read_memory", "save_memory", "delete_memory"]
        );
    }

    fn run_sql_ask() -> String {
        concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"run_sql\",\"arguments\":\"{\\\"sql\\\":\\\"SELECT id FROM posts\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2,\"total_tokens\":11}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string()
    }

    async fn app(frames: Vec<String>, agents: Value) -> (axum::Router, Arc<std::sync::Mutex<Vec<String>>>, Arc<StubPg>) {
        let (base, captured) = sse_wire_capturing(frames).await;
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": {
                "local": { "style": "openai", "baseUrl": format!("{base}/v1"), "models": [{ "id": "tiny" }] },
                "other": { "style": "openai", "baseUrl": format!("{base}/v1"), "models": [{ "id": "huge" }, { "id": "wide" }] }
            },
            "agents": agents
        }))
        .expect("row"));
        let dir = swiss_core::paths::test_home().join(format!("llm-db-{}", swiss_core::util::random_hex(8)));
        state.install_history(swiss_llm::history::LlmHistory::open(&dir).await.expect("open"));
        let services = RuntimeServices::new(swiss_host::services::BootInfo::default());
        let stub = Arc::new(Stub::new(3));
        let pg = stub.pg.clone();
        services.catalog.register(stub, "mcp").expect("register");
        (crate::llm_api::mount(state, services), captured, pg)
    }

    /// The whole send: the run frame, the tool ask, the db frame with the full result
    /// between it and its answer, done; the conversation opened under the Data prompt and
    /// the connection's snapshot, filed under the connection - found from it, and from no
    /// other connection.
    #[tokio::test]
    async fn a_send_runs_the_read_shows_it_on_the_page_and_files_under_the_connection() {
        let (app, captured, pg) = app(vec![run_sql_ask(), one_round_answer()], json!({ "data": { "runtime": "db", "provider": "local", "maxTurns": 4, "systemPrompt": "Answer in Chinese." } })).await;
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/stream",
            Some(json!({ "agent": "data", "conn": "mm", "prompt": "five posts per type", "schema": "public", "rowLimit": 1000 }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["run", "usage", "tool", "db", "toolResult", "delta", "usage", "final", "done"], "{text}");
        let (_, db) = &events[3];
        assert_eq!((db["kind"].as_str(), db["sql"].as_str()), (Some("run"), Some("SELECT id FROM posts")));
        assert_eq!(db["reply"]["rows"].as_array().map(Vec::len), Some(3));
        assert!(db["reply"]["elapsedMs"].is_u64());
        assert_eq!(pg.ran.lock().expect("ran")[0], ("SELECT id FROM posts".to_string(), Some(1000)), "the console's row limit");
        let run_id = events[0].1["runId"].as_i64().expect("run id");
        assert_eq!(events[8].1["runId"].as_i64(), Some(run_id));
        let first: Value = serde_json::from_str(&captured.lock().expect("captured")[0]).expect("ask");
        let system = first["messages"][0]["content"].as_str().unwrap_or_default().to_string();
        assert!(system.starts_with(&format!("{DB_AGENT_PROMPT}\n\nAnswer in Chinese.\n\n{SNAPSHOT_HEADING}")), "{system}");
        // Each round's usage frame weighs its request against the agent's budget, the system
        // prompt and the tool definitions named apart; the second round carries the first's
        // dialogue on top.
        let (_, first_round) = &events[1];
        let (_, second_round) = &events[6];
        let weight = |frame: &Value, key: &str| frame[key].as_u64().unwrap_or(0);
        assert_eq!(weight(first_round, "cap"), 1024 * 1024, "{first_round}");
        assert_eq!(weight(first_round, "system"), system.len() as u64, "{first_round}");
        let tools_weight = weight(first_round, "tools");
        assert!(tools_weight > 0 && weight(first_round, "bytes") > weight(first_round, "system") + tools_weight, "{first_round}");
        assert!(weight(second_round, "bytes") > weight(first_round, "bytes"), "{second_round}");
        assert!(["tokensIn", "tokensCacheRead", "tokensCacheWrite", "tokensOut", "ms", "ttftMs"].iter().all(|key| first_round[*key].is_u64()), "{first_round}");
        assert!(events[4].1["ms"].is_u64(), "a tool's answer says how long it took: {}", events[4].1);
        // The context route weighs what the next send replays, on the same terms.
        let (_, weighed) = call(&app, "GET", &format!("/api/llm/db/context?agent=data&conn=mm&runId={run_id}"), None).await;
        assert_eq!(
            (weighed["cap"].as_u64(), weighed["system"].as_u64(), weighed["tools"].as_u64()),
            (Some(1024 * 1024), Some(system.len() as u64), Some(tools_weight)),
            "{weighed}"
        );
        assert!(weight(&weighed, "bytes") > weight(&weighed, "system") + tools_weight, "{weighed}");
        let (status, missing) = call(&app, "GET", "/api/llm/db/context?agent=data&conn=mm", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
        assert!(system.contains("- Connection: mm (pg, mm @ db:5432)"), "{system}");
        assert!(system.contains("- Schema in view on the page: public"), "{system}");
        assert!(system.contains("- Tables (1): public.posts"), "{system}");
        let tools: Vec<&str> = first["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).collect();
        assert_eq!(tools, ["list_tables", "describe_table", "run_sql", "stage_sql", "open_console", "read_memory", "save_memory", "delete_memory"]);
        // The page serves exactly the catalog the Agents page shows for a db agent on SQL.
        assert_eq!(tools, db_tools_on("sql").iter().chain(db_tools_shared()).map(|t| t.name).collect::<Vec<_>>());
        // The opening ends on the connection's memory: how to use it, then its index.
        assert!(system.contains("\n\n## Memory of this connection\nThe notes saved so far"), "{system}");
        let (_, found) = call(&app, "GET", "/api/llm/db/conversation?agent=data&conn=mm", None).await;
        assert_eq!(found["runId"].as_i64(), Some(run_id));
        let (_, other) = call(&app, "GET", "/api/llm/db/conversation?agent=data&conn=pg2", None).await;
        assert!(other["runId"].is_null(), "{other}");
        // Another connection's send cannot continue this dialogue: it reads as gone there.
        let (status, gone) = call(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm2", "prompt": "x", "runId": run_id }))).await;
        assert_eq!((status, gone["code"].as_str()), (StatusCode::CONFLICT, Some("gone")), "{gone}");
        let (_, runs) = call(&app, "GET", "/api/llm/history/runs?agent=data", None).await;
        let row = runs["runs"].as_array().and_then(|runs| runs.iter().find(|r| r["id"] == run_id)).cloned().unwrap_or_default();
        assert_eq!((row["task"].as_str(), row["subject"].as_str()), (Some(TASK), Some("mm")), "{runs}");
    }

    /// open_console opens a console tab of the agent's under a name, and a run or stage naming
    /// it lands there: each db frame carries the tab, one without a tab carries none (the
    /// Agent tab), and a tab past the name's length or on two lines is refused before anything
    /// reaches the page or the connection.
    #[tokio::test]
    async fn open_console_opens_a_named_tab_and_a_run_naming_it_lands_there() {
        let asks = vec![
            tool_ask("open_console", json!({ "tab": "Revenue report" })),
            tool_ask("run_sql", json!({ "sql": "SELECT id FROM posts", "tab": "Revenue report" })),
            tool_ask("open_console", json!({ "tab": "Cleanup", "sql": "DELETE FROM posts WHERE id = 'p1'" })),
            tool_ask("stage_sql", json!({ "sql": "SELECT 1", "tab": "x".repeat(TAB_MAX + 1) })),
            tool_ask("run_sql", json!({ "sql": "SELECT 1", "tab": "two\nlines" })),
            tool_ask("open_console", json!({})),
            tool_ask("stage_sql", json!({ "sql": "SELECT 2" })),
            one_round_answer(),
        ];
        let (app, _captured, pg) = app(asks, json!({ "data": { "runtime": "db", "provider": "local", "maxTurns": 10 } })).await;
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm", "prompt": "a revenue tab" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let db: Vec<&Value> = events.iter().filter(|(name, _)| name == "db").map(|(_, frame)| frame).collect();
        assert_eq!(db.len(), 4, "{text}");
        assert_eq!(*db[0], json!({ "kind": "console", "tab": "Revenue report" }));
        assert_eq!((db[1]["kind"].as_str(), db[1]["tab"].as_str()), (Some("run"), Some("Revenue report")), "{}", db[1]);
        assert_eq!(db[1]["reply"]["rows"].as_array().map(Vec::len), Some(3));
        assert_eq!(*db[2], json!({ "kind": "stage", "sql": "DELETE FROM posts WHERE id = 'p1'", "tab": "Cleanup" }));
        assert_eq!(*db[3], json!({ "kind": "stage", "sql": "SELECT 2" }), "no tab: the Agent tab");
        let answers: Vec<(bool, &str)> = events
            .iter()
            .filter(|(name, _)| name == "toolResult")
            .map(|(_, frame)| (frame["ok"].as_bool().unwrap_or(false), frame["text"].as_str().unwrap_or_default()))
            .collect();
        assert_eq!(answers.len(), 7, "{text}");
        assert!(answers[0].0 && answers[0].1.starts_with("Opened the console tab Revenue report. Pass tab \"Revenue report\" to run_sql and stage_sql"), "{:?}", answers[0]);
        assert!(answers[2].0 && answers[2].1.contains("with the sql placed in it. It has NOT run."), "{:?}", answers[2]);
        assert!(!answers[3].0 && answers[3].1.contains("at most 40 characters"), "{:?}", answers[3]);
        assert!(!answers[4].0 && answers[4].1.contains("on one line"), "{:?}", answers[4]);
        assert!(!answers[5].0 && answers[5].1.starts_with("tab is required"), "{:?}", answers[5]);
        assert_eq!(pg.ran.lock().expect("ran").len(), 1, "the refused run reached no connection");
    }

    /// The pane's menu: a send names the provider, model and effort to answer with - a
    /// continuation moving to them, the model told so after the reader's words once, the
    /// run frame naming them - and what it names must exist; a send that names none is the
    /// agent's own, which the moved conversation no longer is.
    #[tokio::test]
    async fn the_pane_picks_the_model_and_a_continuation_moves_to_it() {
        let (app, captured, _pg) = app(vec![answer_with("one"), answer_with("two"), answer_with("three")], db_agent()).await;
        let send = |body: Value| call_text(&app, "POST", "/api/llm/db/stream", Some(body));
        let (status, _ct, text) = send(json!({ "agent": "data", "conn": "mm", "prompt": "first" })).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        assert_eq!((events[0].1["provider"].as_str(), events[0].1["model"].as_str()), (Some("local"), Some("tiny")));
        let run_id = events[0].1["runId"].as_i64().expect("run id");
        for (body, why) in [
            (json!({ "provider": "nope" }), "provider"),
            (json!({ "provider": "other", "model": "tiny" }), "model"),
            (json!({ "model": "huge" }), "model"),
            (json!({ "effort": "extreme" }), "effort"),
        ] {
            let mut body = body;
            body["agent"] = json!("data");
            body["conn"] = json!("mm");
            body["prompt"] = json!("x");
            let (status, refused) = call(&app, "POST", "/api/llm/db/stream", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
            assert!(refused["error"].as_str().unwrap_or_default().starts_with(why), "{refused}");
        }
        let picked = json!({ "agent": "data", "conn": "mm", "prompt": "second", "runId": run_id, "provider": "other", "model": "wide", "effort": "low" });
        let (status, _ct, text) = send(picked.clone()).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        assert_eq!((events[0].1["runId"].as_i64(), events[0].1["provider"].as_str(), events[0].1["model"].as_str()), (Some(run_id), Some("other"), Some("wide")));
        let asked: Value = serde_json::from_str(&captured.lock().expect("captured")[1]).expect("ask");
        assert_eq!((asked["model"].as_str(), asked["reasoning_effort"].as_str()), (Some("wide"), Some("low")), "{asked}");
        let told = spoken(&captured.lock().expect("captured")[1]).pop().expect("the ask").1;
        assert_eq!(
            told,
            "second

<environment>
The model answering changed since you were last told.
- You are the agent data, on model wide from provider other
</environment>"
        );
        let mut again = picked.clone();
        again["prompt"] = json!("third");
        let (status, _ct, text) = send(again).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(spoken(&captured.lock().expect("captured")[2]).pop().expect("the ask").1, "third", "told once, never again");
        let (status, changed) = call(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm", "prompt": "x", "runId": run_id }))).await;
        assert_eq!((status, changed["code"].as_str()), (StatusCode::CONFLICT, Some("context-changed")), "{changed}");
    }

    /// The pane's switch: a SQL conversation's snapshot says how writes go, a send that flips
    /// the switch tells the model once after the reader's words, and auto is refused on a
    /// connection that is not SQL as is a word that is neither.
    #[tokio::test]
    async fn the_write_switch_is_in_the_snapshot_and_a_flip_is_told_once() {
        let (app, captured, _pg) = app(vec![answer_with("one"), answer_with("two"), answer_with("three")], db_agent()).await;
        let send = |body: Value| call_text(&app, "POST", "/api/llm/db/stream", Some(body));
        let (status, _ct, text) = send(json!({ "agent": "data", "conn": "mm", "prompt": "first", "writes": "auto" })).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let run_id = events_of(&text)[0].1["runId"].as_i64().expect("run id");
        let opened = spoken(&captured.lock().expect("captured")[0]);
        assert!(opened[0].1.contains(&format!("\n- {}\n", Writes::Auto.fact())), "{}", opened[0].1);
        assert_eq!(opened.last().expect("the ask").1, "first", "the opening told it already");
        let (status, _ct, text) = send(json!({ "agent": "data", "conn": "mm", "prompt": "second", "runId": run_id })).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(
            spoken(&captured.lock().expect("captured")[1]).pop().expect("the ask").1,
            format!("second\n\n<environment>\nThe write mode changed since you were last told.\n- {}\n</environment>", Writes::Staged.fact())
        );
        let (status, _ct, text) = send(json!({ "agent": "data", "conn": "mm", "prompt": "third", "runId": run_id, "writes": "stage" })).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(spoken(&captured.lock().expect("captured")[2]).pop().expect("the ask").1, "third", "told once, never again");
        for (body, why) in [
            (json!({ "conn": "mm", "writes": "yes" }), "writes must be stage or auto"),
            (json!({ "conn": "docs", "writes": "auto" }), "writes: auto runs on a MySQL or PostgreSQL connection only"),
        ] {
            let mut body = body;
            body["agent"] = json!("data");
            body["prompt"] = json!("x");
            let (status, refused) = call(&app, "POST", "/api/llm/db/stream", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
            assert!(refused["error"].as_str().is_some_and(|error| error.starts_with(why)), "{refused}");
        }
    }

    /// Auto writes (SPEC §data.agent Writes): run_sql runs a narrow write and commits it, the
    /// page's run frame marked as a write; one the gate or the cap holds lands in the console
    /// unrun, the frame and the answer saying why; one the server fails is a failed run; a
    /// read stays the read-only path.
    #[tokio::test]
    async fn an_auto_write_runs_when_narrow_and_is_held_past_the_gate_or_the_cap() {
        let held = [
            ("DELETE FROM posts", "DELETE without a WHERE changes every row"),
            ("DELETE FROM posts WHERE 1 = 1", "names no column"),
            ("DROP TABLE posts", "not DROP"),
            ("UPDATE posts SET hidden = true WHERE type = 'old'", "it changes 5000 rows and a write changes at most 1000 on its own, so it was rolled back"),
        ];
        let mut asks = vec![
            tool_ask("run_sql", json!({ "sql": "UPDATE posts SET hidden = true WHERE id = 'p1'", "tab": "Cleanup" })),
            tool_ask("run_sql", json!({ "sql": "INSERT INTO posts (id) VALUES ('p9')" })),
        ];
        asks.extend(held.iter().map(|(sql, _)| tool_ask("run_sql", json!({ "sql": sql }))));
        asks.push(tool_ask("run_sql", json!({ "sql": "DELETE FROM missing WHERE id = 1" })));
        asks.push(tool_ask("run_sql", json!({ "sql": "SELECT id FROM posts" })));
        asks.push(one_round_answer());
        let (app, _captured, pg) = app(asks, json!({ "data": { "runtime": "db", "provider": "local", "maxTurns": 12 } })).await;
        let body = json!({ "agent": "data", "conn": "mm", "prompt": "tidy up", "writes": "auto" });
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let db: Vec<&Value> = events.iter().filter(|(name, _)| name == "db").map(|(_, frame)| frame).collect();
        let answers: Vec<(bool, &str)> = events
            .iter()
            .filter(|(name, _)| name == "toolResult")
            .map(|(_, frame)| (frame["ok"].as_bool().unwrap_or(false), frame["text"].as_str().unwrap_or_default()))
            .collect();
        assert_eq!((db.len(), answers.len()), (8, 8), "{text}");
        assert_eq!((db[0]["kind"].as_str(), db[0]["wrote"].as_bool(), db[0]["tab"].as_str()), (Some("run"), Some(true), Some("Cleanup")), "{}", db[0]);
        assert_eq!(db[0]["reply"]["affectedRows"], 3);
        assert_eq!(answers[0], (true, "It ran and was committed: 3 row(s) changed."));
        assert_eq!(db[1]["reply"]["insertId"], 41);
        assert_eq!(answers[1], (true, "It ran and was committed: 3 row(s) changed, the insert took id 41."));
        for (i, (sql, why)) in held.iter().enumerate() {
            let (frame, answer) = (db[2 + i], answers[2 + i]);
            assert_eq!((frame["kind"].as_str(), frame["sql"].as_str()), (Some("stage"), Some(*sql)), "{frame}");
            assert!(frame["held"].as_str().is_some_and(|held| held.contains(why)), "{frame}");
            assert!(answer.0 && answer.1.starts_with("Held, not run: ") && answer.1.contains(why) && answer.1.ends_with(STAGED), "{sql}: {answer:?}");
        }
        assert!(db[6]["error"].is_string(), "{}", db[6]);
        assert_eq!(answers[6], (false, "the statement failed: relation \"missing\" does not exist"));
        assert!(db[7]["wrote"].is_null() && answers[7].0 && answers[7].1.starts_with("3 row(s)"), "{:?}", answers[7]);
        assert_eq!(pg.wrote.lock().expect("wrote").len(), 4, "the gate held three before the connection");
        assert_eq!(pg.ran.lock().expect("ran").len(), 1);
    }

    /// One streamed answer whose text is `text` (empty: a round that wrote nothing).
    fn answer_with(text: &str) -> String {
        let delta = if text.is_empty() { json!({ "role": "assistant" }) } else { json!({ "role": "assistant", "content": text }) };
        let opening = json!({ "id": "c1", "object": "chat.completion.chunk", "model": "tiny", "choices": [{ "index": 0, "delta": delta, "finish_reason": null }] });
        let closing = json!({ "id": "c1", "object": "chat.completion.chunk", "model": "tiny",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 } });
        format!("data: {opening}\n\ndata: {closing}\n\ndata: [DONE]\n\n")
    }

    fn db_agent() -> Value {
        json!({ "data": { "runtime": "db", "provider": "local", "maxTurns": 4 } })
    }

    /// The text of a captured request's messages, role by role.
    fn spoken(asked: &str) -> Vec<(String, String)> {
        let asked: Value = serde_json::from_str(asked).expect("ask");
        asked["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let text = match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
                    _ => String::new(),
                };
                (m["role"].as_str().unwrap_or_default().to_string(), text)
            })
            .collect()
    }

    /// /compact: the model reads the whole dialogue then the instruction, the reader's focus
    /// closing it, with the page's tools declared; the checkpoint opens a new conversation in
    /// the connection's scope naming the old one, and the next send replays the framing and
    /// the checkpoint - never the condensed turns - under the system the dialogue opened
    /// with. History lists both; Continue on the old one swaps them back.
    #[tokio::test]
    async fn a_compaction_carries_the_dialogue_on_from_its_checkpoint() {
        let frames = vec![one_round_answer(), answer_with("## Requests and Intent\n- count the posts"), one_round_answer()];
        let (app, captured, _pg) = app(frames, db_agent()).await;
        let send = |prompt: &str, run_id: Option<i64>| json!({ "agent": "data", "conn": "mm", "prompt": prompt, "runId": run_id });
        let (_, _, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(send("count the posts", None))).await;
        let from = events_of(&text)[0].1["runId"].as_i64().expect("run id");
        let (status, _ct, text) = call_text(&app, "POST", "/api/llm/db/compact",
            Some(json!({ "agent": "data", "conn": "mm", "runId": from, "focus": "the post types" }))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let events = events_of(&text);
        let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["run", "delta", "usage", "final", "done"], "{text}");
        let next = events[0].1["runId"].as_i64().expect("the new run");
        assert_eq!((events[0].1["from"].as_i64(), events[0].1["compacting"].as_bool()), (Some(from), Some(true)));
        let done = &events[4].1;
        assert_eq!((done["ok"].as_bool(), done["compacted"].as_bool(), done["runId"].as_i64(), done["from"].as_i64()), (Some(true), Some(true), Some(next), Some(from)), "{done}");
        let asked = captured.lock().expect("captured").clone();
        let condensing = spoken(&asked[1]);
        let opened = condensing[0].1.clone();
        assert!(opened.starts_with(DB_AGENT_PROMPT), "the dialogue's own system");
        assert_eq!(condensing[1], ("user".to_string(), "count the posts".to_string()));
        assert_eq!(condensing[2].0, "assistant");
        let (role, instruction) = condensing.last().cloned().unwrap_or_default();
        assert_eq!(role, "user");
        assert!(instruction.starts_with(DB_COMPACT_INSTRUCTION) && instruction.ends_with("keep in particular: the post types"), "{instruction}");
        let condensing_ask: Value = serde_json::from_str(&asked[1]).expect("ask");
        assert_eq!(condensing_ask["tools"].as_array().map(Vec::len), Some(8), "the page's tools declared, open_console and the memory's three among them");
        let (_, found) = call(&app, "GET", "/api/llm/db/conversation?agent=data&conn=mm", None).await;
        assert_eq!(found["runId"].as_i64(), Some(next), "the new conversation is the open one");
        // The next send replays the framing and the checkpoint under the same system.
        let (_, _, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(send("and per type?", Some(next)))).await;
        assert_eq!(events_of(&text).last().map(|(_, done)| done["ok"].as_bool()), Some(Some(true)), "{text}");
        let carried = spoken(&captured.lock().expect("captured")[2]);
        assert_eq!(carried[0].1, opened);
        assert_eq!(
            carried[1..].to_vec(),
            [
                ("user".to_string(), COMPACT_FRAMING.to_string()),
                ("assistant".to_string(), "## Requests and Intent\n- count the posts".to_string()),
                ("user".to_string(), "and per type?".to_string()),
            ]
        );
        // History: both, newest first, the new one naming the old; another connection's is empty.
        let (_, listed) = call(&app, "GET", "/api/llm/db/conversations?agent=data&conn=mm", None).await;
        let ids: Vec<i64> = listed["runs"].as_array().into_iter().flatten().filter_map(|run| run["id"].as_i64()).collect();
        assert_eq!(ids, [next, from], "{listed}");
        assert_eq!(listed["runs"][0]["source"].as_str(), Some(format!("compact:{from}").as_str()));
        let (_, elsewhere) = call(&app, "GET", "/api/llm/db/conversations?agent=data&conn=mm2", None).await;
        assert_eq!(elsewhere["runs"].as_array().map(Vec::len), Some(0), "{elsewhere}");
        // Continue on the old one: it is the open one again, and the new one closed.
        let (status, reopened) = call(&app, "POST", "/api/llm/db/reopen", Some(json!({ "agent": "data", "conn": "mm", "runId": from }))).await;
        assert_eq!((status, reopened["runId"].as_i64()), (StatusCode::OK, Some(from)), "{reopened}");
        let (_, found) = call(&app, "GET", "/api/llm/db/conversation?agent=data&conn=mm", None).await;
        assert_eq!(found["runId"].as_i64(), Some(from));
        // From another connection it reads as gone.
        let (status, gone) = call(&app, "POST", "/api/llm/db/reopen", Some(json!({ "agent": "data", "conn": "mm2", "runId": next }))).await;
        assert_eq!((status, gone["code"].as_str()), (StatusCode::CONFLICT, Some("gone")), "{gone}");
    }

    /// A compaction that leaves no checkpoint files its run failed and reopens the
    /// conversation it would have replaced, as it was; the failed run never reopens.
    #[tokio::test]
    async fn a_failed_compaction_reopens_the_old_conversation() {
        let (app, _captured, _pg) = app(vec![one_round_answer(), answer_with("")], db_agent()).await;
        let (_, _, text) = call_text(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm", "prompt": "count the posts" }))).await;
        let from = events_of(&text)[0].1["runId"].as_i64().expect("run id");
        let (_, _, text) = call_text(&app, "POST", "/api/llm/db/compact", Some(json!({ "agent": "data", "conn": "mm", "runId": from }))).await;
        let events = events_of(&text);
        let failed = events[0].1["runId"].as_i64().expect("the new run");
        let done = &events.last().expect("done").1;
        assert_eq!((done["ok"].as_bool(), done["compacted"].as_bool(), done["runId"].as_i64()), (Some(false), Some(false), Some(from)), "{done}");
        assert!(done["error"].as_str().unwrap_or_default().contains("no checkpoint"), "{done}");
        let (_, found) = call(&app, "GET", "/api/llm/db/conversation?agent=data&conn=mm", None).await;
        assert_eq!(found["runId"].as_i64(), Some(from), "the old conversation is open again");
        let (_, listed) = call(&app, "GET", "/api/llm/db/conversations?agent=data&conn=mm", None).await;
        assert_eq!(listed["runs"][0]["id"].as_i64(), Some(failed));
        assert_eq!(listed["runs"][0]["ok"], json!(false), "{listed}");
        let (status, gone) = call(&app, "POST", "/api/llm/db/reopen", Some(json!({ "agent": "data", "conn": "mm", "runId": failed }))).await;
        assert_eq!((status, gone["code"].as_str()), (StatusCode::CONFLICT, Some("gone")), "{gone}");
        // A compaction names the conversation it condenses, one of this agent's here.
        let (status, _) = call(&app, "POST", "/api/llm/db/compact", Some(json!({ "agent": "data", "conn": "mm" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "runId is required");
        let (status, body) = call(&app, "POST", "/api/llm/db/compact", Some(json!({ "agent": "data", "conn": "mm", "runId": 987_654 }))).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("gone")), "{body}");
    }

    #[tokio::test]
    async fn only_a_db_agent_on_a_data_connection_streams() {
        let agents = json!({ "data": { "runtime": "db", "provider": "local" }, "lead": { "provider": "local" }, "talk": { "runtime": "chat", "provider": "local" } });
        let (app, _captured, _pg) = app(vec![], agents).await;
        let send = |agent: &str, conn: &str| json!({ "agent": agent, "conn": conn, "prompt": "hi" });
        // A task agent, builtin or chat, never works the Data page.
        for agent in ["lead", "talk"] {
            let (status, body) = call(&app, "POST", "/api/llm/db/stream", Some(send(agent, "mm"))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap_or_default().contains("works with a DB agent only"), "{body}");
        }
        // And the Data page's agent runs no Test: it serves the page alone.
        for route in ["/api/llm/agents/test", "/api/llm/agents/test/stream"] {
            let (status, body) = call(&app, "POST", route, Some(json!({ "agent": "data", "prompt": "hi" }))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{route}");
            assert!(body["error"].as_str().unwrap_or_default().contains("serves the Data page alone"), "{route}: {body}");
        }
        let (status, body) = call(&app, "POST", "/api/llm/db/stream", Some(send("data", "web"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap_or_default().contains("works on MySQL, PostgreSQL, MongoDB and Redis"), "{body}");
        let (status, _) = call(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a prompt is required");
        let (status, body) = call(&app, "POST", "/api/llm/db/stream", Some(json!({ "agent": "data", "conn": "mm", "prompt": "hi", "rowLimit": 0 }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}
