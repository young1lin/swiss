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


//! How a tool answer is spelled for the model, and the lease that hands a call its browser
//! (SPEC §data.agent). Split out of the Data agent module; behavior is unchanged.

use std::sync::Arc;

use serde_json::{json, Value};

use swiss_host::dbbrowser::{BrowserFlavor, DbBrowser, RedisBrowser};
use swiss_host::mongobrowser::MongoBrowser;
use swiss_host::services::catalog::{CatalogRegistry, ConnectionLease};
use swiss_llm::agent::ToolAnswer;
use swiss_mcp::adapters::mongo::relaxed_of;

use crate::llm_api::clip;

use super::{
    ANSWER_TEXT_MAX, CELL_MAX, CHANNELS_TO_MODEL, DOC_LINE_MAX, HOLDER, PAYLOAD_SHOWN, ROWS_TO_MODEL,
    TABLES_LISTED,
};

/// What a dialect needs said before the model writes for it.
pub(super) fn dialect_notes(dialect: &str) -> &'static str {
    match dialect {
        "pg" => "PostgreSQL. Double-quote an identifier that needs it; in LIKE, _ and % are wildcards (escape them with a backslash: 'custom\\_%'); top-N per group is ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) filtered in an outer query; a table outside the search_path is schema-qualified.",
        "mysql" => "MySQL. Backtick an identifier that needs it; in LIKE, _ and % are wildcards (escape them with a backslash); top-N per group is ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) on MySQL 8, filtered in an outer query; a table in another database is db.table.",
        "sqlite" => "SQLite, the built-in memory store, query-only. Double-quote an identifier that needs it. SELECT, WITH, EXPLAIN run; INSERT, UPDATE, DELETE, DDL, PRAGMA and ANALYZE are refused. There is one database, main.",
        "mongo" => "MongoDB. run_mongo takes a database command document, never shell syntax: the command with its collection, then its options - {\"find\": \"posts\", \"filter\": {...}, \"projection\": {...}, \"sort\": {...}, \"limit\": 20}, {\"aggregate\": \"posts\", \"pipeline\": [...]}, {\"count\": \"posts\", \"query\": {...}}, {\"distinct\": \"posts\", \"key\": \"type\"}; stage_mongo a write the same way, {\"insert\": \"orders\", \"documents\": [...]}, {\"update\": \"orders\", \"updates\": [{\"q\": {...}, \"u\": {\"$set\": {...}}}]}. Values are Extended JSON: {\"$oid\": \"...\"} for an ObjectId, {\"$date\": \"2026-01-01T00:00:00Z\"} for a date, {\"$numberDecimal\": \"299.00\"} for a Decimal128 (its digits a string). The database goes in db, never in the document. A $regex is a regular expression: escape . * + ? ( ) [ ] and the like to match them literally; top-N per group is an aggregate - $sort, then $group with $push and $slice ($topN on 5.2+), or $setWindowFields; a find without a limit brings back the page's row limit at most. Every tool takes db, defaulting to the database in view.",
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
pub(super) enum Browser {
    Sql(Arc<dyn DbBrowser>),
    Mongo(Arc<dyn MongoBrowser>),
    Redis(Arc<dyn RedisBrowser>),
}

impl Browser {
    pub(super) fn kind(&self) -> DbKind {
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
pub(super) fn lease(catalog: &CatalogRegistry, conn: &str) -> Result<(ConnectionLease, Browser), String> {
    let lease = catalog.lease(conn, HOLDER).map_err(|e| e.to_string())?;
    let browser = match lease.flavor() {
        BrowserFlavor::Db(db) => Browser::Sql(db.clone()),
        BrowserFlavor::Mongo(mongo) => Browser::Mongo(mongo.clone()),
        BrowserFlavor::Redis(redis) => Browser::Redis(redis.clone()),
        BrowserFlavor::None => {
            return Err(format!(
                "{conn} is a {} connection; the Data agent works on MySQL, PostgreSQL, SQLite, MongoDB and Redis",
                lease.dialect()
            ))
        }
    };
    Ok((lease, browser))
}

/// A connection that is no longer of the kind its conversation's tools speak (it was
/// reconfigured since): the call is refused, never run in another language.
pub(super) fn not_of(conn: &str, kind: DbKind) -> String {
    let named = match kind {
        DbKind::Sql => "a MySQL, PostgreSQL or SQLite",
        DbKind::Mongo => "a MongoDB",
        DbKind::Redis => "a Redis",
    };
    format!("{conn} is no longer {named} connection; start a new conversation")
}

/// Whether a name passes a list's grep, the grammar list_tables speaks: `a,b` both, `a|b`
/// either, `*` a wildcard that anchors the term ("user*" starts with user), else a
/// substring; case-insensitive. An empty grep passes everything.
pub(super) fn grep_passes(grep: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    grep.split(',').map(str::trim).filter(|term| !term.is_empty()).all(|term| {
        term.split('|').map(str::trim).filter(|alt| !alt.is_empty()).any(|alt| {
            let alt = alt.to_lowercase();
            if alt.contains('*') { wildcard(&alt, &name) } else { name.contains(&alt) }
        })
    })
}

/// A whole-name match where `*` stands for any run of characters.
pub(super) fn wildcard(pattern: &str, name: &str) -> bool {
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
pub(super) fn row_line(columns: &[String], row: &Value) -> String {
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
pub(super) fn result_text(reply: &Value) -> String {
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
pub(super) fn written_text(reply: &Value) -> String {
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
pub(super) fn tables_text(listing: &Value) -> String {
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
pub(super) fn structure_text(detail: &Value) -> String {
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
pub(super) fn collections_text(db: &str, listing: &Value, grep: Option<&str>) -> String {
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
pub(super) fn clipped(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(clip(text, CELL_MAX)),
        Value::Array(items) => Value::Array(items.iter().map(clipped).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(key, v)| (key.clone(), clipped(v))).collect()),
        other => other.clone(),
    }
}

/// What the model reads of a run_mongo answer: a cursor command's document count and first
/// documents in relaxed Extended JSON, one a line; any other command's reply.
pub(super) fn documents_text(answer: &Value) -> String {
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
pub(super) fn keys_text(page: &Value) -> String {
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
pub(super) fn stream_text(window: &Value, groups: Option<&Value>) -> String {
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
pub(super) fn channels_text(listing: &Value) -> String {
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
pub(super) fn names_arg(args: &Value, key: &str) -> Value {
    match args.get(key) {
        Some(Value::String(name)) => json!([name]),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

/// What one listen heard, as the model reads it: what it listened to and for how long, each
/// message a line, then what it missed.
pub(super) struct Heard {
    pub(super) what: String,
    pub(super) seconds: f64,
    pub(super) messages: Vec<Value>,
    pub(super) cap: Option<i64>,
    pub(super) dropped: i64,
    pub(super) ended: Option<String>,
}

/// What a listen shows on the page (SPEC §data.agent): the names it subscribed to, for the
/// Pub/Sub tab's watch set, and the messages as the feed gave them - each its channel, pattern,
/// payload, size and time - for the tab's log, with how long it listened and what it lost.
pub(super) fn heard_frame(channels: &[String], patterns: &[String], heard: &Heard) -> Value {
    json!({
        "kind": "listen", "channels": channels, "patterns": patterns, "seconds": heard.seconds,
        "messages": heard.messages, "dropped": heard.dropped, "ended": heard.ended,
    })
}

pub(super) fn heard_text(heard: &Heard) -> String {
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

pub(super) fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub(super) fn refused(text: impl Into<String>) -> ToolAnswer {
    ToolAnswer { text: text.into(), ok: false }
}

pub(super) fn answered(text: impl Into<String>) -> ToolAnswer {
    ToolAnswer { text: text.into(), ok: true }
}
