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

//! The in-process Redis adapter — port of `adapters/redis.ts`: one multiplexed auto-reconnecting
//! connection behind a purposeful three-tool set, with the command policy that keeps a shared
//! connection shared.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{StreamBound, StreamQuery, STREAM_SCAN_BUDGET, STREAM_SCAN_PAGE};

use super::direct::{def_bool, BoxFut, Lazy};
use super::redis_resources::RedisResources;
use super::sql::as_i64;
use super::tool_server::{Engine, ServerMeta, ToolDef};

fn key_arg() -> Value {
    json!({ "type": "string", "description": "The Redis key." })
}

/// Three tools, not fourteen, and not one.
///
/// A tool's schema is re-sent on every request, and one gateway often hosts several Redis
/// endpoints — so a broad tool set is paid for several times over, in every session, forever. A
/// model already knows Redis commands; what it cannot know is the shape of the keys in front of
/// it. What a generic tool alone cannot do is:
///
/// - stop the catastrophic default — a busy instance can hold hundreds of thousands of keys, and
///   `KEYS *` both blocks the server and returns megabytes, so SCAN has to be the obvious path;
/// - answer "what is this key, and what is in it?" in one call — GET on a hash is a WRONGTYPE
///   dead-end, and over the raw command channel every collection arrives as a flat array;
/// - keep one call from breaking the endpoint — the adapter holds a SINGLE shared connection, so
///   `SUBSCRIBE`/`MONITOR` leaves it in a mode it never exits and `BLPOP key 0` hangs forever.
///
/// Everything else lives behind `redis_command`, whose description states the return
/// conventions: one line of prose is far cheaper than ten JSON schemas. It also states what THIS
/// instance refuses, since `allowEval` / `allowDestructive` move commands across that line.
fn tools(allow_destructive: bool, allow_eval: bool) -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "redis_scan".into(),
            description: concat!(
                "Incrementally list keys matching a glob pattern (cursor-based SCAN — safe on large ",
                "instances, unlike KEYS, which blocks the server and can return hundreds of thousands of ",
                "keys). One call runs SCAN rounds until it holds about `limit` keys, the keyspace ends, or ",
                "about 2 s pass — so it can return a few more than limit, or under a sparse pattern fewer, ",
                "even none, with done false. Returns { cursor, keys, done }; while done is false, pass the ",
                "cursor back to continue. A key can repeat across calls (SCAN's own guarantee).",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. `session:*`. Omit for all keys." },
                    "cursor": { "type": "string", "description": "Cursor from a previous call. Omit to start." },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": SCAN_LIMIT_MAX,
                        "description": format!("Keys to collect before returning (default {SCAN_LIMIT_DEFAULT}, max {SCAN_LIMIT_MAX})."),
                    },
                    "type": { "type": "string", "enum": SCAN_TYPES, "description": "Only keys of this type." },
                },
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "redis_read".into(),
            description: concat!(
                "Read one key of ANY type and get the value already shaped to it — no WRONGTYPE errors, no ",
                "guessing which command fits. Returns { key, type, ttl, length, truncated?, value }: string → ",
                "its text; hash → { field: value }; list → array; set → sorted array; zset → { member: score } ",
                "in rank order; stream → [{ id, fields }]; a missing key → type \"none\", value null. ttl is ",
                "in seconds (-1 = no expiry). offset/limit page the ordered types (list, zset, stream; default ",
                "0/100, max 1000); hash and set have no rank to page by, so they return at most 1000 entries ",
                "with truncated true when there were more.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": key_arg(),
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "First entry to return, by rank (list, zset, stream). Default 0.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": READ_WINDOW_MAX,
                        "description": format!("Entries to return (list, zset, stream). Default 100, max {READ_WINDOW_MAX}."),
                    },
                },
                "required": ["key"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "redis_command".into(),
            description: command_description(allow_destructive, allow_eval),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command name, e.g. SET / TTL / XRANGE / INFO." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Command arguments, in order." },
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        },
    ]
}

/// redis_command's description, closing with what this instance refuses — so a model on an
/// `allowEval` MCP is not told EVAL is off, and one on a default MCP is not left guessing whether
/// FLUSHDB "is permitted here". The refusals themselves are assert_command_allowed's.
fn command_description(allow_destructive: bool, allow_eval: bool) -> String {
    let mut rejected = vec!["KEYS (use redis_scan)"];
    rejected.push(if allow_eval {
        "script management (SCRIPT/FUNCTION)"
    } else {
        "Lua and functions (EVAL/FCALL/SCRIPT/FUNCTION)"
    });
    rejected.push("transactions (MULTI/EXEC/WATCH)");
    rejected.push(
        "anything that would break this shared connection (SUBSCRIBE/MONITOR/BLPOP…) or the server \
         (SHUTDOWN/DEBUG/CONFIG SET/ACL/MODULE/SAVE…)",
    );
    if !allow_destructive {
        rejected.push("FLUSHALL/FLUSHDB");
    }
    let (last, rest) = rejected.split_last().expect("never empty");
    let mut text = format!(
        "Run any other Redis command: {{ command: \"SET\", args: [\"k\", \"v\", \"EX\", \"60\"] }}. Covers \
         SET/DEL/EXISTS/TTL/TYPE/EXPIRE/INCR, MGET, HSET/HGET/HDEL, LPUSH/LRANGE/LLEN, SADD/SMEMBERS, \
         ZADD/ZRANGE, XADD/XRANGE, INFO/DBSIZE/CONFIG GET, and the rest. Bound your range reads \
         (`LRANGE key 0 99`) — output is capped either way. Return conventions: status replies come \
         back as \"OK\"/\"PONG\", integer replies as numbers, a missing value as null, and collections \
         as FLAT arrays — `ZRANGE k 0 -1 WITHSCORES` is [member, score, member, score]; prefer \
         redis_read when you want the shaped version. Rejected: {}, and {last}.",
        rest.join(", ")
    );
    if allow_eval {
        text.push_str(" This MCP allows EVAL/EVALSHA/FCALL with the script inline.");
    }
    if allow_destructive {
        text.push_str(" This MCP allows FLUSHALL/FLUSHDB.");
    }
    text
}

/// Commands that put the shared connection into a mode it never leaves, or block it indefinitely.
const CONNECTION_BREAKING: &[&str] = &[
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "PSUBSCRIBE",
    "PUNSUBSCRIBE",
    "SSUBSCRIBE",
    "SUNSUBSCRIBE",
    "MONITOR",
    "RESET",
    "SELECT",
    "BLPOP",
    "BRPOP",
    "BLMOVE",
    "BLMPOP",
    "BRPOPLPUSH",
    "BZPOPMIN",
    "BZPOPMAX",
    "BZMPOP",
    "WAIT",
    "WAITAOF",
    "XREAD",
    "XREADGROUP",
];
/// Commands that damage the server itself, not just this connection.
const SERVER_BREAKING: &[&str] = &[
    "SHUTDOWN",
    "DEBUG",
    "SLAVEOF",
    "REPLICAOF",
    "FAILOVER",
    "MIGRATE",
    "SWAPDB",
];
/// Destructive but legitimate on a dev instance — allowed only with `allowDestructive: true`.
const DESTRUCTIVE: &[&str] = &["FLUSHALL", "FLUSHDB"];

/// Lua and functions — the one family every other rule in this file is blind to. `EVAL` takes
/// its program as an argument, so `EVAL "redis.call('FLUSHALL')" 0` is a rejected command
/// wearing an accepted command's name. Rejecting what cannot be inspected is the whole reason
/// this set is separate from the ones above: those name a specific harm, this one names an
/// unbounded one. Opt in per MCP with `allowEval`.
const SCRIPTING: &[&str] = &[
    "EVAL",
    "EVALSHA",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL",
    "FCALL_RO",
];
/// Script/function management, which stays shut even when scripts are allowed — see the message.
const SCRIPT_ADMIN: &[&str] = &["SCRIPT", "FUNCTION"];

/// TYPE values SCAN can filter by — validated before a socket is opened, so a typo costs nothing.
pub const SCAN_TYPES: &[&str] = &["string", "hash", "list", "set", "zset", "stream"];
/// Keys one redis_scan call collects by default, and at most — the output budget's item cap.
const SCAN_LIMIT_DEFAULT: i64 = 100;
const SCAN_LIMIT_MAX: i64 = 1000;
/// Ceiling on one round's COUNT hint: past it, one round blocks the server for longer than a
/// page is worth.
const SCAN_COUNT_MAX: i64 = 10_000;
/// Rounds, and wall time, one redis_scan call spends before handing the cursor back. A raw SCAN
/// round under a sparse pattern often matches nothing while the cursor is far from done, and a
/// model reading `keys: []` concludes there are none — so the call keeps scanning, but never
/// for long: a huge keyspace still pages, the cursor carrying the rest.
const SCAN_ROUNDS_MAX: usize = 32;
const SCAN_BUDGET: Duration = Duration::from_secs(2);

/// The next round's COUNT hint, scaled by how densely the last round matched: a sparse pattern
/// widens fast, a dense one stops close to `limit`, and a round that matched nothing doubles.
fn next_scan_count(count: i64, found: usize, wanted: usize) -> i64 {
    let next = if found == 0 {
        count.saturating_mul(2)
    } else {
        let found = found as i64;
        (count.saturating_mul(wanted as i64) + found - 1) / found
    };
    next.clamp(1, SCAN_COUNT_MAX)
}

/// Commands rejected outright, each with the reason the model sees. A rejection has to say what
/// to do instead, or the model simply tries the next bad idea: an unexplained "rejected" turns
/// `KEYS *` into `SCAN` plus five retries, while naming `redis_scan` ends it in one.
const REJECTED: &[(&[&str], &str)] = &[
    (&["MODULE"], "MODULE LOAD runs native code inside the Redis server."),
    (&["ACL"], "it reads and rewrites this server's credentials — one SETUSER can lock this gateway out of it."),
    (
        &["SYNC", "PSYNC", "REPLCONF"],
        "it turns this connection into a replication stream, which never returns and delivers the whole dataset.",
    ),
    (&["SAVE"], "it blocks the entire server until the dataset is written to disk."),
    (
        &["BGSAVE", "BGREWRITEAOF"],
        "it starts a full disk write on the server; that is an operator's decision, not a debugging step.",
    ),
    (
        &["MULTI", "EXEC", "DISCARD", "WATCH", "UNWATCH"],
        "this adapter shares one connection, so a transaction opened by one call would swallow every \
         other call's commands until EXEC, and they would receive QUEUED instead of an answer. Send \
         the commands individually.",
    ),
    (
        &["KEYS"],
        "it walks the entire keyspace in one blocking pass, which stalls the server on a large \
         instance. Use redis_scan instead — it is cursor-based and reaches the same keys.",
    ),
];

/// Commands whose first argument decides, because one name covers both a harmless read and real
/// damage: `CONFIG GET` reports, `CONFIG SET dir` is the first half of the classic Redis
/// takeover. Subcommands outside the two lists are refused — a new Redis release cannot quietly
/// add a subcommand that slips through.
const CONTAINERS: &[(&str, &[&str], &[&str])] = &[
    ("CONFIG", &["GET"], &[]),
    (
        "CLIENT",
        &["ID", "INFO", "LIST", "GETNAME"],
        &["SETNAME", "SETINFO"],
    ),
    (
        "CLUSTER",
        &[
            "INFO",
            "MYID",
            "NODES",
            "SHARDS",
            "SLOTS",
            "LINKS",
            "KEYSLOT",
            "COUNTKEYSINSLOT",
            "REPLICAS",
            "SLAVES",
        ],
        &[],
    ),
    (
        "MEMORY",
        &["USAGE", "STATS", "DOCTOR", "MALLOC-STATS"],
        &["PURGE"],
    ),
];

/// Ceiling for the ordered reads' window — mirrors the output budget's per-reply item cap.
pub const READ_WINDOW_MAX: i64 = 1000;
/// Entries hash/set reads collect before stopping: these types have no rank to page by.
pub const UNORDERED_CAP: usize = 1000;
/// Fields/members per HSCAN/SSCAN round — small batches, so collection stops soon after the cap.
pub const SCAN_BATCH: i64 = 200;

fn set_has(set: &[&str], word: &str) -> bool {
    set.contains(&word)
}

// --- replies → JSON --------------------------------------------------------------------------------

/// A redis reply as the JSON ioredis would have handed the Node build: status as text ("OK"),
/// integers as numbers, nil as null, collections as (possibly nested) flat arrays.
pub fn redis_value_to_json(v: redis::Value) -> Value {
    match v {
        redis::Value::Nil => Value::Null,
        redis::Value::Int(i) => json!(i),
        redis::Value::BulkString(b) => json!(String::from_utf8_lossy(&b).into_owned()),
        redis::Value::SimpleString(s) => Value::String(s),
        redis::Value::Okay => json!("OK"),
        redis::Value::Array(items) => {
            Value::Array(items.into_iter().map(redis_value_to_json).collect())
        }
        // A RESP3 map flattened to the [k, v, k, v] every RESP2 reply already is.
        redis::Value::Map(pairs) => Value::Array(
            pairs
                .into_iter()
                .flat_map(|(k, val)| [redis_value_to_json(k), redis_value_to_json(val)])
                .collect(),
        ),
        redis::Value::Attribute { data, .. } => redis_value_to_json(*data),
        _ => Value::Null,
    }
}

/// One reply as a plain string (TYPE's status reply, SCAN's cursor, INFO's bulk).
pub fn value_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

pub fn value_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn value_string_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items.iter().map(value_string).collect(),
        _ => Vec::new(),
    }
}

/// Pair a flat `[field, value, field, value]` reply into an object.
pub fn pair_flat(flat: &[Value]) -> Map<String, Value> {
    let mut out = Map::new();
    let mut i = 0;
    while i + 1 < flat.len() {
        out.insert(
            value_string(&flat[i]),
            Value::String(value_string(&flat[i + 1])),
        );
        i += 2;
    }
    out
}

/// Scores arrive as strings and become numbers, except inf/-inf/nan which have no JSON number —
/// those stay strings so the value survives the round trip intact.
fn to_score(raw: &Value) -> Value {
    match raw {
        Value::String(s) => match s.parse::<f64>() {
            Ok(n) if n.is_finite() => json!(n),
            _ => Value::String(s.clone()),
        },
        other => other.clone(),
    }
}

/// An XRANGE reply is `[id, [field, value, …]]` entries; each becomes `{ id, fields }`.
fn stream_entries(reply: &[Value]) -> Vec<Value> {
    reply
        .iter()
        .map(|entry| {
            let (id, flat) = match entry {
                Value::Array(pair) if !pair.is_empty() => (value_string(&pair[0]), pair.get(1)),
                other => (value_string(other), None),
            };
            let fields = flat
                .map(|f| match f {
                    Value::Array(items) => pair_flat(items),
                    _ => Map::new(),
                })
                .unwrap_or_default();
            json!({ "id": id, "fields": Value::Object(fields) })
        })
        .collect()
}

// --- stream windows (SPEC §data.streams) ----------------------------------------------------------------------

/// A stream entry id is "<ms>-<seq>", and the ms half IS the entry's timestamp
/// (SPEC §data.streams) — redis guarantees it at XADD time. None for anything that is not
/// a plain id: the special ids ("$", "+", "-"), malformed strings, and a ms half
/// that overflows i64 all mean “derive nothing”, because a wrong timestamp is worse
/// than a blank one.
pub fn stream_id_ms(id: &str) -> Option<i64> {
    let (ms, _seq) = id.split_once('-')?;
    ms.parse::<i64>().ok()
}

/// The id's ms half as ISO 8601 with millisecond precision and a Z (SPEC §data.streams),
/// derived ONCE on the server because every consumer — panel now, agent later —
/// would otherwise re-implement id parsing, and the panel never should: ids are
/// redis's wire vocabulary, the timestamp is our display vocabulary, and the
/// translation belongs where the data enters the process.
pub fn stream_id_to_ts(id: &str) -> Option<String> {
    let ms = stream_id_ms(id)?;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// The XREVRANGE argument list for one window (SPEC §data.streams). Why XREVRANGE for
/// BOTH directions (D6): XREAD would answer “newer” natively, but it is a
/// blocking command and this adapter's one shared connection must never be held
/// (SPEC §data), so the follow tick is a poll — and a poll wants the NEWEST n
/// entries, which XRANGE (+ after-cursor) would answer with the OLDEST of the
/// backlog instead. The exclusive “(” bounds keep the cursor row itself out:
/// it is already on screen, and an inclusive bound would duplicate it on every
/// page. Newest-first is XREVRANGE's native order, so nothing between redis and
/// the DOM ever reverses a page.
pub fn stream_window_args(key: &str, bound: &StreamBound, count: i64) -> Vec<String> {
    let (start, end) = stream_range_ends(bound);
    vec![
        key.to_string(),
        start,
        end,
        "COUNT".into(),
        count.to_string(),
    ]
}

/// The XREVRANGE (start, end) pair one bound names — start is the NEWER end,
/// because XREVRANGE walks downwards. Split out for SPEC §data.streams's filtered walk,
/// which keeps the bound's own low end fixed and moves only the start as it
/// pages: a follow tick that filtered must never walk back past its cursor.
pub fn stream_range_ends(bound: &StreamBound) -> (String, String) {
    match bound {
        StreamBound::Newest => ("+".to_string(), "-".to_string()),
        StreamBound::Before(id) => (format!("({id}"), "-".to_string()),
        StreamBound::After(id) => ("+".to_string(), format!("({id}")),
    }
}

/// The union of field names across a window, in first-seen order scanning
/// newest-first (SPEC §data.streams). A feed's field set is stable, so the union is
/// usually the one set; when entries interleave (seed `stream:ragged`),
/// first-seen keeps the column order STABLE as pages and ticks merge into one
/// table — an alphabetized union would reshuffle the header on every arrival,
/// and a last-writer order would depend on fetch timing.
pub fn stream_columns(entries: &[Value]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut have = BTreeSet::new();
    for entry in entries {
        let Some(fields) = entry.get("fields").and_then(Value::as_object) else {
            continue;
        };
        for k in fields.keys() {
            if have.insert(k.clone()) {
                order.push(k.clone());
            }
        }
    }
    order
}

/// XINFO STREAM's RESP2 reply is one flat [key, value, ...] array; the window
/// needs only first-entry / last-entry (each an [id, [fields]] pair, or nil on
/// an empty stream). Returned as the raw JSON value — string id or null — so
/// the caller inserts without branching, and a mangled reply degrades to nulls
/// rather than an error: the ends are header decoration, not window data.
pub fn stream_ends(info_reply: &Value) -> (Value, Value) {
    let flat = info_reply.as_array();
    let end = |name: &str| -> Value {
        let mut out = Value::Null;
        if let Some(items) = flat {
            let mut i = 0;
            while i + 1 < items.len() {
                if value_string(&items[i]) == name {
                    out = items[i + 1]
                        .as_array()
                        .and_then(|pair| pair.first().cloned())
                        .unwrap_or(Value::Null);
                    break;
                }
                i += 2;
            }
        }
        out
    };
    (end("first-entry"), end("last-entry"))
}

/// XINFO GROUPS' RESP2 reply: one flat [key, value, ...] array per group,
/// parsed into the panel's table rows (SPEC §data.streams). `lag` exists from redis
/// 7; a 6.2 server omits the key and the row carries null — null is “unknown
/// here”, not zero, because zero would claim the group is caught up. `pending`
/// absent reads as 0: a reply without it comes from proxies, and “no pending
/// reported” is the honest reading of a missing counter.
pub fn parse_xinfo_groups(reply: &[Value]) -> Vec<Value> {
    reply
        .iter()
        .map(|row| {
            let flat = row.as_array().cloned().unwrap_or_default();
            let mut group = Map::new();
            group.insert("name".into(), Value::Null);
            group.insert("consumers".into(), json!(0));
            group.insert("pending".into(), json!(0));
            group.insert("lag".into(), Value::Null);
            group.insert("last-delivered-id".into(), Value::Null);
            let mut i = 0;
            while i + 1 < flat.len() {
                let k = value_string(&flat[i]);
                let v = &flat[i + 1];
                match k.as_str() {
                    "name" => {
                        group.insert("name".into(), v.clone());
                    }
                    "consumers" | "pending" => {
                        group.insert(k.clone(), json!(value_i64(v)));
                    }
                    "lag" => {
                        group.insert("lag".into(), json!(value_i64(v)));
                    }
                    "last-delivered-id" => {
                        group.insert("last-delivered-id".into(), v.clone());
                    }
                    _ => {}
                }
                i += 2;
            }
            Value::Object(group)
        })
        .collect()
}

/// Assemble the window reply the panel codes against (SPEC §data.streams) from the
/// three pipelined answers. Pure so the SHAPE — the contract — is pinned by
/// unit tests without a server; the async wrapper only pipelines and maps
/// errors. `more` is “the page was full”, deliberately not a count: the exact
/// answer would cost a fourth command on every tick, and the Load-earlier
/// button's worst case is one empty page it renders honestly.
pub fn stream_window_shape(
    key: &str,
    entries_reply: &Value,
    len_reply: &Value,
    info_reply: &Value,
    count: i64,
) -> Value {
    let entries = stream_decode(entries_reply);
    let more = (entries.len() as i64) == count;
    stream_window_of(key, entries, len_reply, info_reply, more, None)
}

/// One XREVRANGE reply into the panel's entry objects: `{ id, ts, fields }`,
/// newest first, with the timestamp derived from the id server-side so no two
/// clients disagree about when an entry happened. Split out of
/// [`stream_window_shape`] for SPEC §data.streams: a filtered walk decodes every page it
/// examines and keeps only the rows that match.
pub fn stream_decode(entries_reply: &Value) -> Vec<Value> {
    let raw = entries_reply.as_array().cloned().unwrap_or_default();
    stream_entries(&raw)
        .into_iter()
        .map(|e| {
            let id = e.get("id").map(value_string).unwrap_or_default();
            let ts = stream_id_to_ts(&id)
                .map(Value::String)
                .unwrap_or(Value::Null);
            let mut obj = e.as_object().cloned().unwrap_or_default();
            obj.insert("ts".into(), ts);
            Value::Object(obj)
        })
        .collect()
}

/// What a filtered walk has to admit to (SPEC §data.streams). Redis indexes nothing
/// inside a stream entry, so a filter is a bounded backward read: this says how
/// many entries it examined and which ids it got as far as, and the panel prints
/// that instead of implying the stream holds nothing else. `from` is the NEWEST
/// id examined — the follow tick's next cursor, so a quiet filter does not
/// re-read the same thousands of non-matching entries every second — and `to`
/// is the OLDEST, which is where Load-earlier resumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamScan {
    pub scanned: i64,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// The window reply itself, given entries that are already decoded and a `more`
/// somebody else decided: SPEC §data.streams's plain window computes it from a full page,
/// SPEC §data.streams's filtered walk from whether the range ran out.
pub fn stream_window_of(
    key: &str,
    entries: Vec<Value>,
    len_reply: &Value,
    info_reply: &Value,
    more: bool,
    scan: Option<StreamScan>,
) -> Value {
    let (first, last) = stream_ends(info_reply);
    let mut out = json!({
        "key": key,
        "type": "stream",
        "length": value_i64(len_reply),
        "firstId": first,
        "lastId": last,
        "columns": stream_columns(&entries),
        "entries": entries,
        "more": more,
    });
    if let Some(s) = scan {
        out["scanned"] = json!(s.scanned);
        out["scannedFrom"] = s.from.map(Value::String).unwrap_or(Value::Null);
        out["scannedTo"] = s.to.map(Value::String).unwrap_or(Value::Null);
    }
    out
}
/// The stream window over the shared handle (SPEC §data.streams): XREVRANGE (the
/// window) + XLEN (the length the panel's rate readout differences between
/// ticks) + XINFO STREAM (first/last ids for the header), ONE pipeline — one
/// round trip, exactly three commands. Exactly three is a budget every open
/// panel pays per second, pinned by follow_polling_costs_three_commands_per_tick;
/// a fourth command needs a better reason than convenience. Error mapping:
/// WRONGTYPE is answered with one follow-up TYPE that names the real type (the
/// error path only — the healthy tick never pays it), and “no such key” becomes
/// the same { type: "none" } fact the /key route already reports for a missing
/// key, because XREVRANGE alone cannot tell “gone” from “quiet”.
pub async fn read_stream_window(
    handle: &RedisHandle,
    key: &str,
    q: &StreamQuery,
) -> Result<Value, String> {
    // The first round trip is the same three commands with or without a filter:
    // an unfiltered window asks for exactly the page it will return, a filtered
    // one asks for a scan page and keeps what matches.
    let page = if q.filter.is_some() {
        STREAM_SCAN_PAGE.min(STREAM_SCAN_BUDGET)
    } else {
        q.count
    };
    let commands = vec![
        (
            "XREVRANGE".to_string(),
            stream_window_args(key, &q.bound, page),
        ),
        ("XLEN".to_string(), vec![key.to_string()]),
        (
            "XINFO".to_string(),
            vec!["STREAM".to_string(), key.to_string()],
        ),
    ];
    let replies = match handle.pipeline(&commands).await {
        Ok(r) => r,
        Err(e) => {
            if e.contains("WRONGTYPE") {
                let type_ = handle
                    .call("TYPE", &[key])
                    .await
                    .map(|v| value_string(&v))
                    .unwrap_or_else(|_| "non-stream".into());
                return Err(format!("{key} is a {type_}, not a stream"));
            }
            if e.to_lowercase().contains("no such key") {
                return Ok(json!({ "key": key, "type": "none", "ttl": -2, "value": null }));
            }
            return Err(e);
        }
    };
    let first = replies.first().unwrap_or(&Value::Null).clone();
    let len = replies.get(1).unwrap_or(&Value::Null).clone();
    let info = replies.get(2).unwrap_or(&Value::Null).clone();
    let Some(filter) = q.filter.as_ref() else {
        let entries = stream_decode(&first);
        let more = (entries.len() as i64) == q.count;
        return Ok(stream_window_of(key, entries, &len, &info, more, None));
    };
    // SPEC §data.streams: the filtered walk. Redis has no index inside an entry, so the
    // only honest answer is to read backwards and keep what matches — bounded by
    // STREAM_SCAN_BUDGET, and the answer says how far it got. Each further page is
    // one more XREVRANGE from the last id examined, with the BOUND'S OWN low end
    // kept: a follow tick that pages must not walk back past its cursor.
    let (_, end) = stream_range_ends(&q.bound);
    let mut kept: Vec<Value> = Vec::new();
    let mut scanned: i64 = 0;
    let mut newest: Option<String> = None;
    let mut oldest: Option<String> = None;
    let mut exhausted = false;
    let mut reply = first;
    let mut want = page; // what THIS page asked for: a short answer means the range ended
    loop {
        let rows = stream_decode(&reply);
        let got = rows.len() as i64;
        scanned += got;
        for row in rows {
            let id = row.get("id").map(value_string).unwrap_or_default();
            if newest.is_none() {
                newest = Some(id.clone());
            }
            oldest = Some(id);
            if kept.len() < q.count as usize && filter.matches(&row) {
                kept.push(row);
            }
        }
        if got < want {
            exhausted = true; // the range itself ran out, not the budget
            break;
        }
        if kept.len() >= q.count as usize || scanned >= STREAM_SCAN_BUDGET {
            break;
        }
        let Some(cursor) = oldest.as_ref() else { break };
        want = page.min(STREAM_SCAN_BUDGET - scanned);
        let args = vec![
            key.to_string(),
            format!("({cursor}"),
            end.clone(),
            "COUNT".into(),
            want.to_string(),
        ];
        reply = handle
            .pipeline(&[("XREVRANGE".to_string(), args)])
            .await?
            .into_iter()
            .next()
            .unwrap_or(Value::Null);
    }
    let scan = StreamScan {
        scanned,
        from: newest,
        to: oldest,
    };
    Ok(stream_window_of(
        key,
        kept,
        &len,
        &info,
        !exhausted,
        Some(scan),
    ))
}
/* --- SPEC §data.redis-console: the command catalog the CONNECTED server describes ------------------------------- */

/// One flat RESP2 map ([k, v, k, v, ...]) as pairs. Redis answers COMMAND DOCS this way on
/// RESP2, and every level of it — the command, an argument, a nested argument — is the same
/// shape. Pure; a ragged array (odd length, a non-string key) contributes what it can rather
/// than failing the whole catalog.
fn resp_map(v: &Value) -> Vec<(String, Value)> {
    let Some(items) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len() / 2);
    let mut i = 0;
    while i + 1 < items.len() {
        out.push((value_string(&items[i]), items[i + 1].clone()));
        i += 2;
    }
    out
}

fn map_get<'a>(map: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// One argument of a command as the syntax line spells it (SPEC §data.redis). The shapes come
/// straight from COMMAND DOCS' own vocabulary: a `pure-token` IS its token (NX), a `oneof`
/// is its children separated by |, a `block` is its children in order, and anything else is
/// its name. Any of the last three may carry a token, which leads it: `[LIMIT offset count]`
/// is a block, `STREAMS key [key ...] id [id ...]` too, and dropping the token there printed
/// `[offset count]`. `optional` wraps in brackets; `multiple` repeats the part after the
/// token (`[WEIGHTS weight [weight ...]]`), and `multiple_token` repeats the token with it
/// (`[GET pattern [GET pattern ...]]`) — the way redis' own documentation writes each.
/// Pure, and depth-bounded: a malformed nested spec cannot recurse for ever.
fn arg_syntax(arg: &Value, depth: usize) -> String {
    if depth > 6 {
        return String::new();
    }
    let map = resp_map(arg);
    let name = map_get(&map, "name").map(value_string).unwrap_or_default();
    let token = map_get(&map, "token").map(value_string);
    let kind = map_get(&map, "type").map(value_string).unwrap_or_default();
    let flags = map_get(&map, "flags")
        .map(value_string_array)
        .unwrap_or_default();
    let optional = flags.iter().any(|f| f == "optional");
    let multiple = flags.iter().any(|f| f == "multiple");
    let multiple_token = flags.iter().any(|f| f == "multiple_token");
    let children: Vec<String> = map_get(&map, "arguments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|a| arg_syntax(a, depth + 1))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let token = token.filter(|t| !t.is_empty());
    let body = if kind == "pure-token" {
        let t = token.unwrap_or(name);
        if multiple {
            format!("{t} [{t} ...]")
        } else {
            t
        }
    } else {
        let inner = match kind.as_str() {
            "oneof" => children.join("|"),
            "block" => children.join(" "),
            _ => name,
        };
        match token {
            _ if inner.is_empty() => token.unwrap_or_default(),
            Some(t) if multiple && multiple_token => {
                let unit = format!("{t} {inner}");
                format!("{unit} [{unit} ...]")
            }
            Some(t) if multiple => format!("{t} {inner} [{inner} ...]"),
            Some(t) => format!("{t} {inner}"),
            None if multiple => format!("{inner} [{inner} ...]"),
            None => inner,
        }
    };
    if body.is_empty() {
        return String::new();
    }
    if optional {
        format!("[{body}]")
    } else {
        body
    }
}

/// Every literal token in one argument tree (`NX`, `EX`, `MATCH`, `WITHSCORES`): what the
/// console completes once the command and its keys are typed. Pure.
fn arg_tokens(arg: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 6 {
        return;
    }
    let map = resp_map(arg);
    if let Some(t) = map_get(&map, "token").map(value_string) {
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    if let Some(items) = map_get(&map, "arguments").and_then(Value::as_array) {
        for a in items {
            arg_tokens(a, out, depth + 1);
        }
    }
}

/// COMMAND DOCS into the catalog rows the console codes against (SPEC §data.redis). The reply is
/// a flat [name, spec, name, spec, ...] array; a container's subcommands are nested under it
/// and are flattened here as "XINFO STREAM" rows of their own, because that is what somebody
/// types. Pure, so the shape is pinned without a server.
pub fn parse_command_docs(reply: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (name, spec) in resp_map(reply) {
        push_command_doc(&name.to_uppercase(), &spec, &mut out, 0);
    }
    out.sort_by(|a, b| value_string(&a["name"]).cmp(&value_string(&b["name"])));
    out
}

fn push_command_doc(name: &str, spec: &Value, out: &mut Vec<Value>, depth: usize) {
    if depth > 2 {
        return;
    }
    let map = resp_map(spec);
    let args: Vec<String> = map_get(&map, "arguments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|a| arg_syntax(a, 0))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut tokens: Vec<String> = Vec::new();
    if let Some(items) = map_get(&map, "arguments").and_then(Value::as_array) {
        for a in items {
            arg_tokens(a, &mut tokens, 0);
        }
    }
    let subs = map_get(&map, "subcommands").cloned().unwrap_or(Value::Null);
    // A container (CONFIG, XINFO, CLIENT) has no syntax of its own worth printing: what it
    // takes is one of its subcommands, and those are rows in their own right.
    let has_subs = subs.as_array().map(|a| !a.is_empty()).unwrap_or(false);
    out.push(json!({
        "name": name,
        "syntax": args.join(" "),
        "summary": map_get(&map, "summary").map(value_string).unwrap_or_default(),
        "since": map_get(&map, "since").map(value_string).unwrap_or_default(),
        "group": map_get(&map, "group").map(value_string).unwrap_or_default(),
        "tokens": tokens,
        "container": has_subs,
    }));
    for (sub, sub_spec) in resp_map(&subs) {
        // COMMAND DOCS spells a subcommand "config|get"; a console types "CONFIG GET".
        let leaf = sub.rsplit('|').next().unwrap_or(&sub).to_uppercase();
        push_command_doc(&format!("{name} {leaf}"), &sub_spec, out, depth + 1);
    }
}

/// COMMAND INFO into the key positions the console needs (SPEC §data.redis): first key, last key
/// and step, the same three numbers redis-cli reads to know which words are key names. The
/// reply is one row per command — [name, arity, flags, first, last, step, …] — and a row that
/// is not that shape is skipped rather than guessed at. Redis 7 carries a container's
/// subcommands as a tenth cell of full rows of the same shape (named "xinfo|stream"), and a
/// container's OWN numbers are zeros: the key positions of `XINFO STREAM` are only in there,
/// so the nesting is walked rather than read as the end of the row. Pure.
pub fn parse_command_info(reply: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    push_command_info(reply, "", &mut out, 0);
    out
}

fn push_command_info(reply: &Value, parent: &str, out: &mut Vec<Value>, depth: usize) {
    if depth > 2 {
        return;
    }
    let Some(rows) = reply.as_array() else {
        return;
    };
    for row in rows {
        let Some(cells) = row.as_array() else { continue };
        if cells.len() < 6 {
            continue;
        }
        // "xinfo|stream" is what redis calls the row; "XINFO STREAM" is what a person types.
        let leaf = value_string(&cells[0]);
        let leaf = leaf.rsplit('|').next().unwrap_or(&leaf).to_uppercase();
        let name = if parent.is_empty() {
            leaf
        } else {
            format!("{parent} {leaf}")
        };
        let specs: Vec<Value> = cells
            .get(8)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(parse_key_spec).collect())
            .unwrap_or_default();
        out.push(json!({
            "name": name.clone(),
            "arity": value_i64(&cells[1]),
            "firstKey": value_i64(&cells[3]),
            "lastKey": value_i64(&cells[4]),
            "step": value_i64(&cells[5]),
            "keySpecs": specs,
        }));
        if let Some(subs) = cells.get(9) {
            push_command_info(subs, &name, out, depth + 1);
        }
    }
}

/// One key specification (redis 7, the ninth cell of a COMMAND INFO row) in the compact form
/// the console evaluates: where the key search BEGINS - at a fixed argument (`index`) or after
/// a keyword (`keyword`, e.g. XREAD's STREAMS) - and how the keys are FOUND from there - a
/// `range` (last key, step, and for a key list that runs to the end a `limit` that keeps only
/// its first 1/limit, which is how XREAD's keys stop where its ids start) or a `keynum`
/// (ZUNION's and EVAL's count argument). These are what make the keys of XREAD, ZUNION,
/// EVAL, LMPOP and friends completable: their legacy first/last/step say "no key" because
/// the keys move. A spec of a kind redis calls `unknown` is dropped - no guess is offered
/// where the server itself does not know. Pure.
fn parse_key_spec(spec: &Value) -> Option<Value> {
    let map = resp_map(spec);
    let begin = resp_map(map_get(&map, "begin_search")?);
    let find = resp_map(map_get(&map, "find_keys")?);
    let begin_type = map_get(&begin, "type").map(value_string)?;
    let find_type = map_get(&find, "type").map(value_string)?;
    let bspec = resp_map(map_get(&begin, "spec").unwrap_or(&Value::Null));
    let fspec = resp_map(map_get(&find, "spec").unwrap_or(&Value::Null));
    let num = |m: &[(String, Value)], k: &str| map_get(m, k).map(value_i64).unwrap_or(0);
    let begin = match begin_type.as_str() {
        "index" => json!({ "type": "index", "index": num(&bspec, "index") }),
        "keyword" => json!({
            "type": "keyword",
            "keyword": map_get(&bspec, "keyword").map(value_string).unwrap_or_default(),
            "startfrom": num(&bspec, "startfrom"),
        }),
        _ => return None,
    };
    let find = match find_type.as_str() {
        "range" => json!({
            "type": "range",
            "lastkey": num(&fspec, "lastkey"),
            "keystep": num(&fspec, "keystep"),
            "limit": num(&fspec, "limit"),
        }),
        "keynum" => json!({
            "type": "keynum",
            "keynumidx": num(&fspec, "keynumidx"),
            "firstkey": num(&fspec, "firstkey"),
            "keystep": num(&fspec, "keystep"),
        }),
        _ => return None,
    };
    Some(json!({ "begin": begin, "find": find }))
}

/// Merge the two answers into one catalog (SPEC §data.redis). DOCS carries the words (summary,
/// syntax, group), INFO the numbers (arity, key positions); a command either side missed
/// still ships with what the other knew, because half a row is what makes the difference
/// between completing a command and not offering it at all. Pure.
pub fn merge_command_catalog(docs: Vec<Value>, info: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for mut d in docs {
        let name = value_string(&d["name"]);
        if let Some(i) = info.iter().find(|i| value_string(&i["name"]) == name) {
            d["arity"] = i["arity"].clone();
            d["firstKey"] = i["firstKey"].clone();
            d["lastKey"] = i["lastKey"].clone();
            d["step"] = i["step"].clone();
            d["keySpecs"] = i["keySpecs"].clone();
        } else if name.contains(' ') {
            // A subcommand inherits its container's key positions: COMMAND INFO reports the
            // container alone, and "XINFO STREAM key" takes its key exactly where XINFO does.
            let head = name.split(' ').next().unwrap_or("").to_string();
            if let Some(i) = info.iter().find(|i| value_string(&i["name"]) == head) {
                d["firstKey"] = i["firstKey"].clone();
                d["lastKey"] = i["lastKey"].clone();
                d["step"] = i["step"].clone();
            }
        }
        out.push(d);
    }
    // A server old enough to have no COMMAND DOCS (redis 6) still answers COMMAND INFO: its
    // rows become name-and-key-positions entries, so the console completes names and keys
    // even where it has nothing to say about arguments.
    for i in info {
        let name = value_string(&i["name"]);
        if out.iter().any(|d| value_string(&d["name"]) == name) {
            continue;
        }
        out.push(json!({
            "name": name,
            "syntax": "",
            "summary": "",
            "since": "",
            "group": "",
            "tokens": [],
            "container": false,
            "arity": i["arity"].clone(),
            "firstKey": i["firstKey"].clone(),
            "lastKey": i["lastKey"].clone(),
            "step": i["step"].clone(),
            "keySpecs": i["keySpecs"].clone(),
        }));
    }
    out.sort_by(|a, b| value_string(&a["name"]).cmp(&value_string(&b["name"])));
    out
}

/// The console's command catalog, read from the server that will run the commands (SPEC §data.redis-console).
/// One pipeline, two introspection commands: COMMAND DOCS for the words and COMMAND INFO for
/// the key positions. A server without DOCS (redis < 7, or a proxy) answers an error or an
/// empty array and the catalog is INFO's alone — names and key positions, which is still a
/// working console. Read-only, and nothing here is cached on the gateway: the panel asks once
/// per connection and keeps the answer for as long as it keeps the connection.
pub async fn read_command_catalog(handle: &RedisHandle) -> Result<Value, String> {
    // Two sends, not one pipeline: a server that does not know DOCS fails that command, and
    // redis answers a pipeline's failures together - taking the whole batch down with it.
    let docs_cmd = [("COMMAND".to_string(), vec!["DOCS".to_string()])];
    let info_cmd = [("COMMAND".to_string(), vec!["INFO".to_string()])];
    let docs = match handle.pipeline(&docs_cmd).await {
        Ok(r) => r.into_iter().next().unwrap_or(Value::Null),
        Err(_) => Value::Null, // pre-7.0, or a proxy: INFO alone still makes a console
    };
    // An ACL can grant one introspection command and not the other: DOCS alone is still a
    // catalog of names and syntax, so INFO failing only fails the call when DOCS did too.
    let info = match handle.pipeline(&info_cmd).await {
        Ok(r) => r.into_iter().next().unwrap_or(Value::Null),
        Err(e) if docs.is_null() => return Err(e),
        Err(_) => Value::Null,
    };
    let documented = parse_command_docs(&docs);
    let has_docs = !documented.is_empty();
    let catalog = merge_command_catalog(documented, parse_command_info(&info));
    Ok(json!({
        "commands": catalog,
        // True only when DOCS gave rows: an answer that parsed to nothing documents nothing.
        "documented": has_docs,
    }))
}

/// XINFO GROUPS, read-only (SPEC §data.streams): the consumer-group table behind the
/// groups fold. A key that vanished mid-view answers an empty table rather than
/// an error — the stream view above it owns that story — while a non-stream
/// key is refused with its real type, same wording as the window.
pub async fn read_stream_groups(handle: &RedisHandle, key: &str) -> Result<Value, String> {
    match handle.call("XINFO", &["GROUPS", key]).await {
        Ok(Value::Array(rows)) => Ok(json!({ "key": key, "groups": parse_xinfo_groups(&rows) })),
        Ok(_) => Ok(json!({ "key": key, "groups": [] })),
        Err(e) => {
            if e.to_lowercase().contains("no such key") {
                return Ok(json!({ "key": key, "groups": [] }));
            }
            if e.contains("WRONGTYPE") {
                let type_ = handle
                    .call("TYPE", &[key])
                    .await
                    .map(|v| value_string(&v))
                    .unwrap_or_else(|_| "non-stream".into());
                return Err(format!("{key} is a {type_}, not a stream"));
            }
            Err(e)
        }
    }
}

/// Clamp the paging args every ordered read shares; absent or nonsensical values fall back to
/// 0/100.
pub fn read_window(args: &Value) -> (i64, i64) {
    let offset = args.get("offset").and_then(as_i64).unwrap_or(0).max(0);
    let limit = args
        .get("limit")
        .and_then(as_i64)
        .unwrap_or(100)
        .clamp(1, READ_WINDOW_MAX);
    (offset, limit)
}

// --- the type-aware reader -------------------------------------------------------------------------

/// The slice of the client the type-aware reader needs, as a trait so the dispatch is testable
/// without a Redis — the same seam the Node build's `RedisReadClient` cut.
#[async_trait]
pub trait RedisReadClient: Send + Sync {
    async fn type_of(&self, key: &str) -> Result<String, String>;
    async fn ttl(&self, key: &str) -> Result<i64, String>;
    async fn get(&self, key: &str) -> Result<Option<String>, String>;
    async fn llen(&self, key: &str) -> Result<i64, String>;
    async fn lrange(&self, key: &str, start: i64, stop: i64) -> Result<Vec<String>, String>;
    async fn hlen(&self, key: &str) -> Result<i64, String>;
    async fn scard(&self, key: &str) -> Result<i64, String>;
    async fn zcard(&self, key: &str) -> Result<i64, String>;
    async fn xlen(&self, key: &str) -> Result<i64, String>;
    async fn call(&self, command: &str, args: &[&str]) -> Result<Value, String>;
}

/// One HSCAN/SSCAN round through the raw channel, whose reply is already `[cursor, flatArray]`.
async fn scan_round(
    client: &dyn RedisReadClient,
    command: &str,
    key: &str,
    cursor: &str,
) -> Result<(String, Vec<Value>), String> {
    let reply = client
        .call(command, &[key, cursor, "COUNT", &SCAN_BATCH.to_string()])
        .await?;
    match &reply {
        Value::Array(pair) if pair.len() >= 2 => Ok((
            value_string(&pair[0]),
            match &pair[1] {
                Value::Array(items) => items.clone(),
                _ => Vec::new(),
            },
        )),
        _ => Ok(("0".into(), Vec::new())),
    }
}

/// The dispatch behind redis_read: TYPE and TTL first — one round trip that answers "what is
/// this key" — then the read shaped to what the key actually is.
pub async fn type_aware_read(
    client: &dyn RedisReadClient,
    key: &str,
    offset: i64,
    limit: i64,
) -> Result<Value, String> {
    let (type_, ttl) = tokio::join!(client.type_of(key), client.ttl(key));
    let type_ = type_?;
    let ttl = ttl?;
    let mut base = Map::new();
    base.insert("key".into(), json!(key));
    base.insert("type".into(), json!(type_));
    base.insert("ttl".into(), json!(ttl));

    match type_.as_str() {
        // Redis' own answer for a missing key — a fact to report, not an error to invent.
        "none" => {
            base.insert("value".into(), Value::Null);
            Ok(Value::Object(base))
        }
        "string" => {
            let value = client.get(key).await?;
            base.insert(
                "value".into(),
                match value {
                    Some(s) => json!(s),
                    None => Value::Null,
                },
            );
            Ok(Value::Object(base))
        }
        "hash" => {
            // HSCAN, not HGETALL: collection stops at the cap, so a hash with a hundred
            // thousand fields never materializes in one reply.
            let length = client.hlen(key).await?;
            let mut value = Map::new();
            let mut cursor = "0".to_string();
            loop {
                let (next, flat) = scan_round(client, "HSCAN", key, &cursor).await?;
                for (k, v) in pair_flat(&flat) {
                    value.insert(k, v);
                }
                cursor = next;
                if cursor == "0" || value.len() >= UNORDERED_CAP {
                    break;
                }
            }
            let got = value.len();
            base.insert("length".into(), json!(length));
            if (got as i64) < length {
                base.insert("truncated".into(), json!(true));
                base.insert(
                    "note".into(),
                    json!(format!(
                        "showing {got} of {length} fields — read one field with redis_command {{command:\"HGET\"}}"
                    )),
                );
            }
            base.insert("value".into(), Value::Object(value));
            Ok(Value::Object(base))
        }
        "list" => {
            let (length, value) = tokio::join!(
                client.llen(key),
                client.lrange(key, offset, offset + limit - 1)
            );
            let (length, value) = (length?, value?);
            base.insert("length".into(), json!(length));
            if offset + (value.len() as i64) < length {
                base.insert("truncated".into(), json!(true));
            }
            base.insert("value".into(), json!(value));
            Ok(Value::Object(base))
        }
        "set" => {
            let length = client.scard(key).await?;
            let mut found: Vec<String> = Vec::new();
            let mut cursor = "0".to_string();
            loop {
                let (next, batch) = scan_round(client, "SSCAN", key, &cursor).await?;
                for item in &batch {
                    found.push(value_string(item));
                }
                cursor = next;
                if cursor == "0" || found.len() >= UNORDERED_CAP {
                    break;
                }
            }
            // SCAN order is the server's internal ordering; dedup + sort so the same key reads
            // the same twice.
            let unique: BTreeSet<String> = found.into_iter().take(UNORDERED_CAP).collect();
            let value: Vec<String> = unique.into_iter().collect();
            base.insert("length".into(), json!(length));
            if value.len() < length as usize {
                base.insert("truncated".into(), json!(true));
                base.insert(
                    "note".into(),
                    json!(format!(
                        "showing {} of {length} members — check one with redis_command {{command:\"SISMEMBER\"}}",
                        value.len()
                    )),
                );
            }
            base.insert("value".into(), json!(value));
            Ok(Value::Object(base))
        }
        "zset" => {
            let zrange_args = [
                key.to_string(),
                offset.to_string(),
                (offset + limit - 1).to_string(),
                "WITHSCORES".to_string(),
            ];
            let zrange_refs: Vec<&str> = zrange_args.iter().map(String::as_str).collect();
            let (length, reply) =
                tokio::join!(client.zcard(key), client.call("ZRANGE", &zrange_refs));
            let (length, reply) = (length?, reply?);
            let flat = match &reply {
                Value::Array(items) => items.clone(),
                _ => Vec::new(),
            };
            let mut value = Map::new();
            for (member, score) in pair_flat(&flat) {
                value.insert(member, to_score(&score));
            }
            let got = value.len();
            base.insert("length".into(), json!(length));
            if offset + (got as i64) < length {
                base.insert("truncated".into(), json!(true));
            }
            base.insert("value".into(), Value::Object(value));
            Ok(Value::Object(base))
        }
        "stream" => {
            // XRANGE has no offset, so the window is fetched and then skipped — one bounded
            // overshoot.
            let xrange_args = [
                key.to_string(),
                "-".to_string(),
                "+".to_string(),
                "COUNT".to_string(),
                (offset + limit).to_string(),
            ];
            let xrange_refs: Vec<&str> = xrange_args.iter().map(String::as_str).collect();
            let (length, reply) =
                tokio::join!(client.xlen(key), client.call("XRANGE", &xrange_refs));
            let (length, reply) = (length?, reply?);
            let entries = match &reply {
                Value::Array(items) => items.clone(),
                _ => Vec::new(),
            };
            let windowed: Vec<Value> = stream_entries(&entries)
                .into_iter()
                .skip(offset.max(0) as usize)
                .take(limit.max(0) as usize)
                .collect();
            base.insert("length".into(), json!(length));
            if offset + (windowed.len() as i64) < length {
                base.insert("truncated".into(), json!(true));
            }
            base.insert("value".into(), Value::Array(windowed));
            Ok(Value::Object(base))
        }
        // Module types (ReJSON-RL, TSDB-TYPE, …): name the way out rather than failing opaquely.
        other => {
            base.insert("value".into(), Value::Null);
            base.insert(
                "note".into(),
                json!(format!(
                    "a {other} value needs its module's own commands — use redis_command (e.g. JSON.GET / TS.RANGE)"
                )),
            );
            Ok(Value::Object(base))
        }
    }
}

// --- the command policy ----------------------------------------------------------------------------

/// Reject a command that would damage the shared connection, the server, or the data — before
/// any connection is opened, so a bad call costs nothing and the rule is testable without a
/// Redis. `args` are the command's own arguments, needed only because a container command's
/// first argument is the one that decides (CONFIG GET vs CONFIG SET).
pub fn assert_command_allowed(
    command: &str,
    args: &[&str],
    allow_destructive: bool,
    allow_eval: bool,
) -> Result<(), String> {
    let upper = command.trim().to_uppercase();
    if upper.is_empty() {
        return Err("command is required".into());
    }
    if set_has(SCRIPT_ADMIN, &upper) {
        return Err(format!(
            "{upper} is rejected: script management is never exposed. Pass the script inline with EVAL — and note \
             that a runaway script has to be killed with redis-cli, since this shared connection would already be \
             blocked waiting for it."
        ));
    }
    if set_has(SCRIPTING, &upper) && !allow_eval {
        return Err(format!(
            "{upper} is rejected: the gateway cannot see inside a script, so none of its other rules apply to \
             one — a single line of Lua can flush the keyspace, block the server in a loop, or run any command \
             this MCP refuses. Set \"allowEval\": true on this MCP if you accept that."
        ));
    }
    for (commands, why) in REJECTED {
        if commands.contains(&upper.as_str()) {
            return Err(format!("{upper} is rejected: {why}"));
        }
    }
    if set_has(CONNECTION_BREAKING, &upper) {
        return Err(format!(
            "{upper} is rejected: this adapter shares one connection across all requests, and {upper} would leave \
             it unusable for every later call. Use redis_scan / the dedicated tools instead."
        ));
    }
    if set_has(SERVER_BREAKING, &upper) {
        return Err(format!(
            "{upper} is rejected: it would disrupt the Redis server itself"
        ));
    }
    if set_has(DESTRUCTIVE, &upper) && !allow_destructive {
        return Err(format!(
            "{upper} is rejected: set \"allowDestructive\": true on this MCP to permit it"
        ));
    }
    for (name, read, write) in CONTAINERS {
        if *name == upper {
            let sub = args
                .first()
                .map(|s| s.trim().to_uppercase())
                .unwrap_or_default();
            let permitted: Vec<&str> = read.iter().chain(write.iter()).copied().collect();
            if sub.is_empty() {
                return Err(format!(
                    "{upper} requires a subcommand, one of: {}",
                    permitted.join(", ")
                ));
            }
            if read.contains(&sub.as_str()) || write.contains(&sub.as_str()) {
                return Ok(());
            }
            return Err(format!(
                "{upper} {sub} is rejected: only these subcommands are permitted: {}",
                permitted.join(", ")
            ));
        }
    }
    Ok(())
}

// --- the shared connection -------------------------------------------------------------------------

/// One multiplexed, auto-reconnecting connection (ioredis's role). Every clone shares the
/// underlying socket; the manager reconnects on its own when the server comes back.
pub struct RedisHandle {
    manager: redis::aio::ConnectionManager,
}

impl RedisHandle {
    pub async fn call(&self, command: &str, args: &[&str]) -> Result<Value, String> {
        self.call_raw(command, args).await.map(redis_value_to_json)
    }

    /// The reply as redis sent it: bytes stay bytes. A key's name is binary-safe in redis and
    /// not necessarily UTF-8 - Spring's JDK serializer writes `\xac\xed\x00\x05...` - and the
    /// JSON form turns such a name into U+FFFD, a name no command can address (2026-09-29).
    pub async fn call_raw(&self, command: &str, args: &[&str]) -> Result<redis::Value, String> {
        let mut conn = self.manager.clone();
        let mut cmd = redis::cmd(command);
        for arg in args {
            cmd.arg(arg);
        }
        // commandTimeout: 10s — bound how long a command may wait, which also bounds the
        // offline queue (a down server accumulates work otherwise).
        tokio::time::timeout(
            Duration::from_secs(10),
            cmd.query_async::<redis::Value>(&mut conn),
        )
        .await
        .map_err(|_| format!("{command} timed out after 10s"))?
        .map_err(|e| e.to_string())
    }

    /// One pipelined round trip: every command rides a single network request, replies come back
    /// in order — ioredis's `pipeline()`, which cut a 200-key Data-view page from ~880 ms to
    /// tens of ms versus serialized TYPE/TTL calls. The same 10s commandTimeout bounds it.
    pub async fn pipeline(&self, commands: &[(String, Vec<String>)]) -> Result<Vec<Value>, String> {
        self.pipeline_args(commands).await
    }

    /// `pipeline` over any byte-like arguments: the key walk asks TYPE and TTL of the exact
    /// bytes SCAN named, UTF-8 or not.
    pub async fn pipeline_args<A: AsRef<[u8]> + Sync>(
        &self,
        commands: &[(String, Vec<A>)],
    ) -> Result<Vec<Value>, String> {
        let mut conn = self.manager.clone();
        let mut pipe = redis::pipe();
        for (command, args) in commands {
            let mut cmd = pipe.cmd(command.as_str());
            for arg in args {
                cmd = cmd.arg(arg.as_ref());
            }
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            pipe.query_async::<Vec<redis::Value>>(&mut conn),
        )
        .await
        .map_err(|_| "pipeline timed out after 10s".to_string())?
        .map_err(|e| e.to_string())
        .map(|replies| replies.into_iter().map(redis_value_to_json).collect())
    }
}

/// A key name that is not UTF-8, printed the way redis-cli prints it (sdscatrepr): quoted,
/// printable ASCII as itself, `\\` and `\"` escaped, the usual control escapes, every other
/// byte as `\xHH`. Display only - the panel cannot address such a key by this text.
pub fn redis_cli_repr(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push('"');
    out
}

fn int_of(v: &Value) -> Result<i64, String> {
    Ok(value_i64(v))
}

#[async_trait]
impl RedisReadClient for RedisHandle {
    async fn type_of(&self, key: &str) -> Result<String, String> {
        Ok(value_string(&self.call("TYPE", &[key]).await?))
    }
    async fn ttl(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("TTL", &[key]).await?)
    }
    async fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.call("GET", &[key]).await? {
            Value::Null => Ok(None),
            v => Ok(Some(value_string(&v))),
        }
    }
    async fn llen(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("LLEN", &[key]).await?)
    }
    async fn lrange(&self, key: &str, start: i64, stop: i64) -> Result<Vec<String>, String> {
        Ok(value_string_array(
            &self
                .call("LRANGE", &[key, &start.to_string(), &stop.to_string()])
                .await?,
        ))
    }
    async fn hlen(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("HLEN", &[key]).await?)
    }
    async fn scard(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("SCARD", &[key]).await?)
    }
    async fn zcard(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("ZCARD", &[key]).await?)
    }
    async fn xlen(&self, key: &str) -> Result<i64, String> {
        int_of(&self.call("XLEN", &[key]).await?)
    }
    async fn call(&self, command: &str, args: &[&str]) -> Result<Value, String> {
        RedisHandle::call(self, command, args).await
    }
}

// --- the engine ------------------------------------------------------------------------------------

pub struct RedisEngine {
    def: ServerDef,
    name: Arc<std::sync::RwLock<String>>,
    conn: Arc<Lazy<RedisHandle>>,
}

impl RedisEngine {
    pub fn new(def: &ServerDef, name: &str) -> Self {
        let def = def.clone();
        let host = def.get_str("host").unwrap_or("localhost").to_string();
        let port = def.get_number("port").map(|p| p as u16).unwrap_or(6379);
        let db = def.get_number("db").map(|d| d as i64).unwrap_or(0);
        let password = def
            .get_str("password")
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let conn = Lazy::new(move || {
            let mut settings = redis::RedisConnectionInfo::default()
                .set_db(db)
                .set_protocol(redis::ProtocolVersion::RESP2);
            if let Some(password) = &password {
                settings = settings.set_password(password);
            }
            let addr = redis::ConnectionAddr::Tcp(host.clone(), port);
            Box::pin(async move {
                let info = redis::IntoConnectionInfo::into_connection_info(addr)
                    .map_err(|e| e.to_string())?
                    .set_redis_settings(settings);
                let client = redis::Client::open(info).map_err(|e| e.to_string())?;
                // One plain connection first. The manager retries every failure of its first
                // connect - a refused AUTH included - so a wrong password surfaced only as the
                // timeout below, 5s later ("redis connect timed out"), never as what Redis
                // said (2026-09-28). The probe fails at once with the refusal itself; only a
                // server that accepted us gets the reconnecting manager.
                tokio::time::timeout(Duration::from_secs(5), client.get_multiplexed_async_connection())
                    .await
                    .map_err(|_| "redis connect timed out after 5s".to_string())?
                    .map_err(|e| e.to_string())?;
                // connectTimeout 5000 (the eager first connect) over the manager, which then
                // keeps reconnecting on its own — ioredis's retryStrategy, capped.
                let manager =
                    tokio::time::timeout(Duration::from_secs(5), client.get_connection_manager())
                        .await
                        .map_err(|_| "redis connect timed out after 5s".to_string())?
                        .map_err(|e| e.to_string())?;
                Ok(RedisHandle { manager })
            }) as BoxFut<Result<RedisHandle, String>>
        });
        Self {
            def,
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            conn: Arc::new(conn),
        }
    }

    fn allow_destructive(&self) -> bool {
        def_bool(&self.def, "allowDestructive")
    }
    fn allow_eval(&self) -> bool {
        def_bool(&self.def, "allowEval")
    }

    /// Redis MCPs advertise the same tools; without this, the path is the only clue which
    /// instance is behind one.
    fn where_(&self) -> String {
        format!(
            "{}:{} db {}",
            self.def.get_str("host").unwrap_or("localhost"),
            self.def
                .get_number("port")
                .map(|p| p as i64)
                .unwrap_or(6379),
            self.def.get_number("db").map(|d| d as i64).unwrap_or(0),
        )
    }

    async fn call_scan(&self, args: &Value) -> Result<Value, String> {
        let cursor = args
            .get("cursor")
            .map(value_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".into());
        let limit = args
            .get("limit")
            .and_then(as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(SCAN_LIMIT_DEFAULT)
            .clamp(1, SCAN_LIMIT_MAX) as usize;
        let pattern = args
            .get("pattern")
            .map(value_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "*".into());
        let type_ = args
            .get("type")
            .map(|v| value_string(v).trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        if !type_.is_empty() && !SCAN_TYPES.contains(&type_.as_str()) {
            return Err(format!(
                "type must be one of {} — got \"{type_}\"",
                SCAN_TYPES.join(", ")
            ));
        }
        let client = self.conn.get().await?;
        let started = Instant::now();
        let mut cursor = cursor;
        let mut count = limit as i64;
        let mut keys: Vec<String> = Vec::new();
        // SCAN may hand a key back twice within one walk; one call's list stays unique.
        let mut seen: HashSet<String> = HashSet::new();
        for _ in 0..SCAN_ROUNDS_MAX {
            // Through the raw channel: the TYPE filter only exists as an option token, and
            // SCAN's raw reply is already [cursor, keys].
            let mut scan_args: Vec<String> = vec![
                cursor.clone(),
                "MATCH".into(),
                pattern.clone(),
                "COUNT".into(),
                count.to_string(),
            ];
            if !type_.is_empty() {
                scan_args.push("TYPE".into());
                scan_args.push(type_.clone());
            }
            let refs: Vec<&str> = scan_args.iter().map(String::as_str).collect();
            let reply = RedisHandle::call(&client, "SCAN", &refs).await?;
            let (next, batch) = match &reply {
                Value::Array(pair) if pair.len() >= 2 => (
                    value_string(&pair[0]),
                    match &pair[1] {
                        Value::Array(items) => items.iter().map(value_string).collect::<Vec<_>>(),
                        _ => Vec::new(),
                    },
                ),
                _ => ("0".into(), Vec::new()),
            };
            cursor = next;
            let found = batch.len();
            for key in batch {
                if seen.insert(key.clone()) {
                    keys.push(key);
                }
            }
            if cursor == "0" || keys.len() >= limit || started.elapsed() >= SCAN_BUDGET {
                break;
            }
            count = next_scan_count(count, found, limit - keys.len());
        }
        let done = cursor == "0";
        Ok(json!({ "cursor": cursor, "keys": keys, "done": done }))
    }

    async fn call_read(&self, args: &Value) -> Result<Value, String> {
        let key = args.get("key").map(value_string).unwrap_or_default();
        if key.is_empty() {
            return Err("key is required".into());
        }
        let (offset, limit) = read_window(args);
        let client = self.conn.get().await?;
        type_aware_read(client.as_ref(), &key, offset, limit).await
    }

    async fn call_command(&self, args: &Value) -> Result<Value, String> {
        // Parsed up front because the guard needs the arguments too — a container command's
        // first argument is what decides whether it is a read or a rewrite of the server.
        let command = args
            .get("command")
            .map(value_string)
            .unwrap_or_default()
            .trim()
            .to_string();
        let rest: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(value_string).collect())
            .unwrap_or_default();
        // Validate before connecting: a refused call should never open a socket.
        let rest_refs: Vec<&str> = rest.iter().map(String::as_str).collect();
        assert_command_allowed(
            &command,
            &rest_refs,
            self.allow_destructive(),
            self.allow_eval(),
        )?;
        let client = self.conn.get().await?;
        let arg_refs: Vec<&str> = rest.iter().map(String::as_str).collect();
        RedisHandle::call(&client, &command, &arg_refs).await
    }
}

#[async_trait]
impl Engine for RedisEngine {
    fn kind(&self) -> &'static str {
        "redis"
    }

    fn tools(&self) -> Vec<ToolDef> {
        tools(self.allow_destructive(), self.allow_eval())
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "redis_scan" => self.call_scan(args).await,
            "redis_read" => self.call_read(args).await,
            "redis_command" => self.call_command(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn meta(&self) -> ServerMeta {
        let name = self.name.read().ok().map(|g| g.clone());
        ServerMeta {
            name,
            description: self.def.get_str("description").map(str::to_string),
            target: Some(self.where_()),
            limits: None,
        }
    }

    /// One resource: the shape of this keyspace. Labelled with the MCP name rather than
    /// host:port, since that is what a client shows beside the URI and what stays stable if
    /// the tunnel's port moves.
    fn resources(&self) -> Option<Arc<dyn super::resources::ResourceProvider>> {
        let label = self
            .name
            .read()
            .ok()
            .and_then(|g| if g.is_empty() { None } else { Some(g.clone()) })
            .unwrap_or_else(|| self.where_());
        Some(Arc::new(RedisResources::new(label, self.conn.clone())))
    }

    /// The Data view: page keys by SCAN, read one key type-aware, run one guarded console
    /// command. Carries the adapter's own policy flags so the console refuses exactly what the
    /// redis_command tool refuses.
    fn browser(&self) -> Option<swiss_host::dbbrowser::BrowserFlavor> {
        Some(swiss_host::dbbrowser::BrowserFlavor::Redis(Arc::new(
            super::redis_browser::RedisDataBrowser::new(
                self.where_(),
                self.allow_destructive(),
                self.allow_eval(),
                self.conn.clone(),
            ),
        )))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = async {
            let client = self.conn.get().await?;
            match RedisHandle::call(&client, "PING", &[]).await? {
                Value::String(s) if s == "PONG" => Ok(()),
                other => Err(format!("redis PING returned {}", value_string(&other))),
            }
        }
        .await;
        Some(result)
    }

    async fn close(&self) {
        self.conn
            .dispose(|client| {
                Box::pin(async move {
                    // Best-effort QUIT (the connection is closing either way), mirroring
                    // ioredis's disconnect() — which never waits long: a server that does not
                    // answer QUIT must not hold an MCP stop (and a gateway shutdown) hostage.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(250),
                        RedisHandle::call(&client, "QUIT", &[]),
                    )
                    .await;
                }) as BoxFut<()>
            })
            .await;
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_round_widens_with_how_sparse_the_pattern_matched() {
        // Nothing matched: double, up to the ceiling.
        assert_eq!(next_scan_count(100, 0, 100), 200);
        assert_eq!(next_scan_count(SCAN_COUNT_MAX, 0, 100), SCAN_COUNT_MAX);
        // Dense: 95 of 100 came back, so the last 5 need a narrow round.
        assert_eq!(next_scan_count(100, 95, 5), 6);
        // Sparse: one match per hundred slots, 99 still wanted.
        assert_eq!(next_scan_count(100, 1, 99), 9900);
        assert_eq!(next_scan_count(100, 1, 999), SCAN_COUNT_MAX);
        let scan = tools(false, false).remove(0);
        assert_eq!(scan.name, "redis_scan");
        assert_eq!(scan.input_schema["properties"]["limit"]["maximum"], json!(SCAN_LIMIT_MAX));
        assert!(scan.input_schema["properties"].get("count").is_none());
    }

    #[tokio::test]
    async fn a_refused_password_answers_as_an_auth_failure_not_a_timeout() {
        // A server that refuses every command the way Redis refuses a wrong AUTH.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        // One refusal per command: the client pipelines its setup (AUTH, the
                        // CLIENT SETINFO pair) and reads a reply for each, as Redis sends.
                        let got = &buf[..n];
                        let commands = got
                            .iter()
                            .enumerate()
                            .filter(|(i, b)| **b == b'*' && (*i == 0 || got[i - 1] == b'\n'))
                            .count()
                            .max(1);
                        let refusal = b"-WRONGPASS invalid username-password pair or user is disabled.\r\n";
                        if sock.write_all(&refusal.repeat(commands)).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let Value::Object(def) = json!({ "type": "redis", "host": "127.0.0.1", "port": port, "password": "wrong" }) else {
            unreachable!()
        };
        let engine = RedisEngine::new(&ServerDef(def), "r");
        let t0 = std::time::Instant::now();
        let err = engine.ping().await.expect("redis pings").expect_err("refused");
        // redis-rs names a refused AUTH itself ("Password authentication failed").
        assert!(err.to_lowercase().contains("authentication failed"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(3), "answered at once, not at the timeout: {:?}", t0.elapsed());
    }

    #[test]
    fn policy_rejects_the_named_harms() {
        let (ad, ae) = (false, false);
        // KEYS points at the alternative.
        let err = assert_command_allowed("keys", &[], ad, ae).unwrap_err();
        assert!(err.contains("redis_scan"), "{err}");
        assert!(assert_command_allowed("SUBSCRIBE", &[], ad, ae)
            .unwrap_err()
            .contains("shares one connection"));
        assert!(assert_command_allowed("SHUTDOWN", &[], ad, ae)
            .unwrap_err()
            .contains("server itself"));
        assert!(assert_command_allowed("FLUSHALL", &[], ad, ae)
            .unwrap_err()
            .contains("allowDestructive"));
        assert!(assert_command_allowed("EVAL", &["..."], ad, ae)
            .unwrap_err()
            .contains("cannot see inside a script"));
        assert!(assert_command_allowed("", &[], ad, ae)
            .unwrap_err()
            .contains("required"));
    }

    #[test]
    fn policy_lets_the_opt_ins_through() {
        let (ad, ae) = (true, true);
        assert!(assert_command_allowed("FLUSHALL", &[], ad, ae).is_ok());
        assert!(assert_command_allowed("EVAL", &["return 1", "0"], ad, ae).is_ok());
    }

    #[test]
    fn ordinary_writes_run_there_is_no_readonly_gate() {
        // SET/DEL/EXPIRE pass with no opt-in: there is no readonly flag anymore, the operator's
        // data is the operator's to change. RENAME/PERSIST are the Data view's key-sheet ops
        // (SPEC §data.redis) — pinned here so a future deny-list sweep cannot quietly break the
        // panel's Rename / Set-TTL sheets.
        assert!(assert_command_allowed("SET", &["k", "v"], false, false).is_ok());
        assert!(assert_command_allowed("DEL", &["k"], false, false).is_ok());
        assert!(assert_command_allowed("EXPIRE", &["k", "60"], false, false).is_ok());
        assert!(assert_command_allowed("RENAME", &["k", "k2"], false, false).is_ok());
        assert!(assert_command_allowed("PERSIST", &["k"], false, false).is_ok());
    }

    #[test]
    fn the_command_description_states_what_this_instance_refuses() {
        // The description and the guard must agree for every flag combination: a model told
        // EVAL is off never tries it, and one told it is on must not meet a refusal.
        for (destructive, eval) in [(false, false), (true, false), (false, true), (true, true)] {
            let text = command_description(destructive, eval);
            let refused = text.split("Rejected: ").nth(1).expect("a Rejected clause");
            let refused = refused.split(". ").next().unwrap_or(refused);
            for (command, args) in [("EVAL", &["return 1", "0"][..]), ("FLUSHDB", &[][..])] {
                let allowed = assert_command_allowed(command, args, destructive, eval).is_ok();
                assert_eq!(
                    refused.contains(command),
                    !allowed,
                    "{command} with allowDestructive={destructive} allowEval={eval}: {text}"
                );
            }
            assert!(refused.contains("SCRIPT/FUNCTION"), "management stays shut: {text}");
        }
    }

    #[test]
    fn container_policy_still_decides_per_subcommand() {
        // CONFIG has no write list at all, so CONFIG SET hits the permitted-list error (Node
        // behavior), while CLIENT's listed write subcommands pass without any opt-in.
        let (ad, ae) = (false, false);
        assert!(assert_command_allowed("CONFIG", &["GET", "maxmemory"], ad, ae).is_ok());
        assert!(
            assert_command_allowed("CONFIG", &["SET", "dir", "/x"], ad, ae)
                .unwrap_err()
                .contains("only these subcommands")
        );
        assert!(assert_command_allowed("CLIENT", &["SETNAME", "x"], ad, ae).is_ok());
        assert!(assert_command_allowed("CONFIG", &["RESETSTAT"], ad, ae)
            .unwrap_err()
            .contains("only these subcommands"));
        assert!(assert_command_allowed("CONFIG", &[], ad, ae)
            .unwrap_err()
            .contains("requires a subcommand"));
    }

    #[test]
    fn flats_pair_and_scores_convert() {
        let flat = vec![json!("f1"), json!("v1"), json!("f2"), json!("v2")];
        let paired = pair_flat(&flat);
        assert_eq!(paired.len(), 2);
        assert_eq!(paired.get("f1"), Some(&json!("v1")));

        assert_eq!(to_score(&json!("12.5")), json!(12.5));
        assert_eq!(to_score(&json!("inf")), json!("inf"));
    }

    #[test]
    fn stream_ids_split_and_derive() {
        // SPEC §data.streams: the ms half is the timestamp; everything that is not a
        // plain id derives nothing rather than a wrong time.
        assert_eq!(stream_id_ms("1700000999900-0"), Some(1_700_000_999_900));
        assert_eq!(stream_id_ms("0-0"), Some(0));
        assert_eq!(stream_id_ms("5-18446744073709551615"), Some(5));
        assert_eq!(stream_id_ms("99999999999999999999-0"), None, "ms overflows i64");
        assert_eq!(stream_id_ms("1700000000000"), None, "no seq half");
        assert_eq!(stream_id_ms(""), None);
        for special in ["$", "+", "-"] {
            assert_eq!(stream_id_ms(special), None, "special id {special}");
        }
        assert_eq!(
            stream_id_to_ts("1700000999900-0").as_deref(),
            Some("2023-11-14T22:29:59.900Z")
        );
        assert_eq!(
            stream_id_to_ts("1700000000000-0").as_deref(),
            Some("2023-11-14T22:13:20.000Z")
        );
        assert_eq!(stream_id_to_ts("not-an-id"), None);
    }

    #[test]
    fn stream_window_args_spell_the_three_bounds() {
        // SPEC §data.streams: both directions are XREVRANGE with exclusive “(” bounds,
        // so the cursor row never repeats and every page arrives newest-first.
        assert_eq!(
            stream_window_args("s", &StreamBound::Newest, 100),
            vec!["s", "+", "-", "COUNT", "100"]
        );
        assert_eq!(
            stream_window_args("s", &StreamBound::Before("7-1".into()), 5),
            vec!["s", "(7-1", "-", "COUNT", "5"]
        );
        assert_eq!(
            stream_window_args("s", &StreamBound::After("7-1".into()), 100),
            vec!["s", "+", "(7-1", "COUNT", "100"]
        );
    }

    #[test]
    fn stream_columns_union_in_first_seen_order() {
        // SPEC §data.streams: newest-first scan, first-seen order — the ragged seed's five
        // entries (a / a,b / b,c / c / a,c,d newest-first) union to a,c,d,b, and a
        // repeated field name never appears twice.
        let entries = vec![
            json!({ "fields": { "a": "5", "c": "5", "d": "5" } }),
            json!({ "fields": { "c": "4" } }),
            json!({ "fields": { "b": "3", "c": "3" } }),
            json!({ "fields": { "a": "2", "b": "2" } }),
            json!({ "fields": { "a": "1" } }),
        ];
        assert_eq!(stream_columns(&entries), vec!["a", "c", "d", "b"]);
        assert_eq!(stream_columns(&[]), Vec::<String>::new());
        let one = vec![json!({ "fields": { "sym": "AAA", "px": "1.00" } })];
        assert_eq!(stream_columns(&one), vec!["sym", "px"]);
        assert_eq!(stream_columns(&[json!({ "id": "1-0" })]), Vec::<String>::new());
    }

    #[test]
    fn stream_entries_survives_odd_shapes() {
        // The XRANGE/XREVRANGE pair shape: [id, [f, v, ...]] rows; a ragged row, an
        // odd-length field array (the tail has no pair), a non-array reply and
        // non-string scalars all degrade instead of panicking.
        let reply = vec![
            json!(["1-0", ["f", "v"]]),
            json!(["2-0", ["odd"]]),
            json!("3-0"),
        ];
        let out = stream_entries(&reply);
        assert_eq!(out[0]["fields"]["f"], json!("v"));
        assert_eq!(out[1]["fields"].as_object().map(|m| m.len()), Some(0), "tail dropped");
        assert_eq!(out[2]["fields"].as_object().map(|m| m.len()), Some(0));
        assert_eq!(stream_entries(&[]), Vec::<Value>::new());
        let scalars = stream_entries(&[json!(["4-0", ["n", 42]])]);
        assert_eq!(scalars[0]["fields"]["n"], json!("42"), "scalars stringify");
    }

    #[test]
    fn parse_xinfo_groups_tolerates_62_and_proxies() {
        // SPEC §data.streams: lag exists from redis 7 (6.2 omits it -> null, never 0:
        // zero would claim “caught up”); pending missing reads 0; an empty reply is
        // an empty table.
        let with_lag = vec![json!([
            "name", "feed", "consumers", 1, "pending", 7, "lag", 9993,
            "last-delivered-id", "1700000000600-0",
        ])];
        let groups = parse_xinfo_groups(&with_lag);
        assert_eq!(groups[0]["name"], json!("feed"));
        assert_eq!(groups[0]["pending"], json!(7));
        assert_eq!(groups[0]["lag"], json!(9993));
        assert_eq!(groups[0]["last-delivered-id"], json!("1700000000600-0"));
        let no_lag = vec![json!(["name", "old", "consumers", 2, "pending", 0,
            "last-delivered-id", "0-0"])];
        assert_eq!(parse_xinfo_groups(&no_lag)[0]["lag"], Value::Null);
        let no_pending = vec![json!(["name", "px", "consumers", 0,
            "last-delivered-id", "0-0"])];
        assert_eq!(parse_xinfo_groups(&no_pending)[0]["pending"], json!(0));
        assert_eq!(parse_xinfo_groups(&[]), Vec::<Value>::new());
    }

    #[test]
    fn stream_ends_read_first_and_last() {
        let info = json!([
            "length", 2, "radix-tree-keys", 2, "radix-tree-nodes", 4,
            "last-generated-id", "2-0",
            "first-entry", ["1-0", ["a", "1"]],
            "last-entry", ["2-0", ["a", "2"]],
        ]);
        let (first, last) = stream_ends(&info);
        assert_eq!(first, json!("1-0"));
        assert_eq!(last, json!("2-0"));
        let empty = json!(["length", 0, "first-entry", null, "last-entry", null]);
        let (first, last) = stream_ends(&empty);
        assert_eq!(first, Value::Null);
        assert_eq!(last, Value::Null);
        let (first, last) = stream_ends(&json!("mangled"));
        assert_eq!((first, last), (Value::Null, Value::Null));
    }

    #[test]
    fn stream_window_shape_is_the_contract() {
        // SPEC §data.streams: the reply the panel codes against — newest-first entries,
        // server-derived ts, the column union, the length, and `more` as
        // "the page was full", never a count.
        let entries = json!([
            ["1700000999900-0", ["sym", "AAA", "px", "9.99", "qty", "50", "side", "s"]],
            ["1700000999800-0", ["sym", "BBB", "px", "9.98", "qty", "49", "side", "b"]],
        ]);
        let info = json!([
            "length", 10000,
            "first-entry", ["1700000000000-0", ["sym", "AAA"]],
            "last-entry", ["1700000999900-0", ["sym", "AAA"]],
        ]);
        let v = stream_window_shape("stream:ticks", &entries, &json!(10_000), &info, 100);
        assert_eq!(v["type"], "stream");
        assert_eq!(v["length"], json!(10_000));
        assert_eq!(v["firstId"], json!("1700000000000-0"));
        assert_eq!(v["lastId"], json!("1700000999900-0"));
        assert_eq!(v["more"], json!(false), "a partial page is not more");
        assert_eq!(v["columns"], json!(["sym", "px", "qty", "side"]));
        assert_eq!(v["entries"][0]["id"], json!("1700000999900-0"));
        assert_eq!(v["entries"][0]["ts"], json!("2023-11-14T22:29:59.900Z"));
        assert_eq!(v["entries"][1]["ts"], json!("2023-11-14T22:29:59.800Z"));
        let full = json!([["1-0", ["a", "1"]]]);
        let v = stream_window_shape("k", &full, &json!(1), &info, 1);
        assert_eq!(v["more"], json!(true), "a full page probably has more");
        // SPEC §data.streams: an unfiltered window says nothing about scanning — there was
        // none. Absent, not zero: zero would claim a walk that examined nothing.
        assert_eq!(v.get("scanned"), None);
    }

    #[test]
fn a_filtered_window_reports_how_far_it_walked() {
    // SPEC §data.streams: the filter's answer carries its own bound — how many entries
    // were examined and the newest/oldest ids reached — so the panel can say "3 of
    // the newest 20,000" instead of implying the stream holds nothing else. The
    // two cursors are the two directions: `scannedFrom` is the follow tick's next
    // `after`, `scannedTo` is where Load-earlier resumes.
    let info = json!(["length", 9, "first-entry", null, "last-entry", null]);
    let kept = stream_decode(&json!([["9-0", ["sym", "AAA"]]]));
    let v = stream_window_of(
        "k",
        kept,
        &json!(9),
        &info,
        true,
        Some(StreamScan {
            scanned: 20_000,
            from: Some("9-0".into()),
            to: Some("1-0".into()),
        }),
    );
    assert_eq!(v["scanned"], json!(20_000));
    assert_eq!(v["scannedFrom"], json!("9-0"));
    assert_eq!(v["scannedTo"], json!("1-0"));
    assert_eq!(v["more"], json!(true), "the budget ran out, not the stream");
    assert_eq!(v["entries"][0]["id"], json!("9-0"));
    assert_eq!(v["columns"], json!(["sym"]));
    // A walk that examined nothing at all still answers both cursors as null
    // rather than leaving the panel to guess at a missing key.
    let empty = stream_window_of(
        "k",
        vec![],
        &json!(0),
        &info,
        false,
        Some(StreamScan {
            scanned: 0,
            from: None,
            to: None,
        }),
    );
    assert_eq!(empty["scannedFrom"], Value::Null);
    assert_eq!(empty["scannedTo"], Value::Null);
}
    #[test]
    fn command_docs_become_syntax_lines_and_tokens() {
        // SPEC §data.redis: the syntax a console prints is BUILT from the server's own argument
        // spec, in redis' own notation - optional in brackets, a choice with |, a repeat
        // saying so once. This is SET as redis 7 describes it, trimmed to the shapes that
        // matter: a key, a value, a one-of of pure tokens, and a token with an argument.
        let docs = json!([
            "set", [
                "summary", "Set the string value of a key",
                "since", "1.0.0",
                "group", "string",
                "arguments", [
                    ["name", "key", "type", "key"],
                    ["name", "value", "type", "string"],
                    ["name", "condition", "type", "oneof", "flags", ["optional"], "arguments", [
                        ["name", "nx", "type", "pure-token", "token", "NX"],
                        ["name", "xx", "type", "pure-token", "token", "XX"],
                    ]],
                    ["name", "expiration", "type", "oneof", "flags", ["optional"], "arguments", [
                        ["name", "seconds", "type", "integer", "token", "EX"],
                        ["name", "keepttl", "type", "pure-token", "token", "KEEPTTL"],
                    ]],
                ],
            ],
            "del", [
                "summary", "Delete a key",
                "group", "generic",
                "arguments", [["name", "key", "type", "key", "flags", ["multiple"]]],
            ],
        ]);
        let rows = parse_command_docs(&docs);
        let set = rows.iter().find(|r| r["name"] == json!("SET")).expect("SET");
        assert_eq!(set["syntax"], json!("key value [NX|XX] [EX seconds|KEEPTTL]"));
        assert_eq!(set["summary"], json!("Set the string value of a key"));
        assert_eq!(set["group"], json!("string"));
        assert_eq!(set["since"], json!("1.0.0"));
        assert_eq!(
            set["tokens"],
            json!(["NX", "XX", "EX", "KEEPTTL"]),
            "the tokens are what the console completes once the key is typed"
        );
        let del = rows.iter().find(|r| r["name"] == json!("DEL")).expect("DEL");
        assert_eq!(del["syntax"], json!("key [key ...]"), "multiple says so once");
    }

    #[test]
    fn a_container_command_contributes_its_subcommands_as_rows() {
        // SPEC §data.redis: nobody types "XINFO"; they type "XINFO STREAM". A container keeps its
        // own row (marked as one, so the console can say it needs a subcommand) and every
        // subcommand becomes a row under the name a person would write.
        let docs = json!([
            "xinfo", [
                "summary", "A container for stream introspection commands",
                "group", "stream",
                "subcommands", [
                    "xinfo|stream", [
                        "summary", "Get information about a stream",
                        "group", "stream",
                        "arguments", [["name", "key", "type", "key"]],
                    ],
                    "xinfo|groups", [
                        "summary", "List the consumer groups of a stream",
                        "group", "stream",
                        "arguments", [["name", "key", "type", "key"]],
                    ],
                ],
            ],
        ]);
        let rows = parse_command_docs(&docs);
        let names: Vec<String> = rows.iter().map(|r| value_string(&r["name"])).collect();
        assert_eq!(names, vec!["XINFO", "XINFO GROUPS", "XINFO STREAM"]);
        let container = rows.iter().find(|r| r["name"] == json!("XINFO")).expect("XINFO");
        assert_eq!(container["container"], json!(true));
        let stream = rows
            .iter()
            .find(|r| r["name"] == json!("XINFO STREAM"))
            .expect("XINFO STREAM");
        assert_eq!(stream["syntax"], json!("key"));
        assert_eq!(stream["container"], json!(false));
    }

    #[test]
    fn command_info_gives_the_key_positions_and_merges_with_the_words() {
        // SPEC §data.redis: DOCS has the words, INFO has the numbers - which ARGUMENT is a key.
        // A redis 7 row carries a container's subcommands in its tenth cell, each a full row
        // of the same shape, because the container's own numbers are zeros: `XINFO STREAM`'s
        // key position exists nowhere else.
        let info = json!([
            ["set", 3, ["write", "denyoom"], 1, 1, 1],
            ["mset", -3, ["write"], 1, -1, 2],
            ["xinfo", -2, ["readonly"], 0, 0, 0, ["@slow"], [], [], [
                ["xinfo|stream", -3, ["readonly"], 2, 2, 1],
            ]],
            ["ping", -1, ["fast"], 0, 0, 0],
            ["ragged"],
        ]);
        let parsed = parse_command_info(&info);
        assert_eq!(parsed.len(), 5, "a row that is not the shape is skipped, not guessed");
        let mset = parsed.iter().find(|r| r["name"] == json!("MSET")).expect("MSET");
        assert_eq!((mset["firstKey"].clone(), mset["lastKey"].clone(), mset["step"].clone()),
                   (json!(1), json!(-1), json!(2)));
        let nested = parsed
            .iter()
            .find(|r| r["name"] == json!("XINFO STREAM"))
            .expect("the subcommand is a row of its own, under the name a person types");
        assert_eq!(nested["firstKey"], json!(2), "the container itself says 0");

        let docs = parse_command_docs(&json!([
            "set", ["summary", "Set a key", "group", "string",
                    "arguments", [["name", "key", "type", "key"]]],
            "xinfo", ["summary", "A container", "group", "stream", "subcommands", [
                "xinfo|stream", ["summary", "Info", "group", "stream",
                                 "arguments", [["name", "key", "type", "key"]]],
            ]],
        ]));
        let merged = merge_command_catalog(docs.clone(), parsed);
        let names: Vec<String> = merged.iter().map(|r| value_string(&r["name"])).collect();
        assert!(names.contains(&"PING".to_string()), "INFO-only commands still ship: {names:?}");
        let set = merged.iter().find(|r| r["name"] == json!("SET")).expect("SET");
        assert_eq!(set["firstKey"], json!(1));
        assert_eq!(set["summary"], json!("Set a key"), "both halves survive the merge");
        let sub = merged
            .iter()
            .find(|r| r["name"] == json!("XINFO STREAM"))
            .expect("XINFO STREAM");
        assert_eq!(sub["firstKey"], json!(2));
        assert_eq!(sub["summary"], json!("Info"), "the words come from DOCS, the numbers from INFO");
        let ping = merged.iter().find(|r| r["name"] == json!("PING")).expect("PING");
        assert_eq!(ping["summary"], json!(""), "no words for it, but it is offered");
        assert_eq!(ping["firstKey"], json!(0));

        // Redis 6 nests nothing: there the container's own numbers ARE the subcommand's, and
        // the merge falls back to inheriting them rather than leaving the key uncompletable.
        let old = parse_command_info(&json!([["xinfo", -2, ["readonly"], 2, 2, 1]]));
        let merged = merge_command_catalog(docs, old);
        let sub = merged
            .iter()
            .find(|r| r["name"] == json!("XINFO STREAM"))
            .expect("XINFO STREAM");
        assert_eq!(sub["firstKey"], json!(2), "inherited from the container");
    }

    #[test]
    fn a_token_leads_its_block_and_its_oneof_and_repeats_only_when_redis_says_so() {
        // 2026-09-29: the first version dropped the token of a block, so ZRANGEBYSCORE read
        // `[offset count]` and XREAD lost STREAMS. The four shapes below are redis 7's own
        // COMMAND DOCS for ZRANGEBYSCORE's LIMIT, XREAD's STREAMS, ZINTER's WEIGHTS and
        // AGGREGATE, and SORT's GET - one per rule.
        let docs = json!([
            "zrangebyscore", ["summary", "s", "group", "sorted-set", "arguments", [
                ["name", "key", "type", "key"],
                ["name", "min", "type", "double"],
                ["name", "max", "type", "double"],
                ["name", "withscores", "type", "pure-token", "token", "WITHSCORES", "flags", ["optional"]],
                ["name", "limit", "type", "block", "token", "LIMIT", "flags", ["optional"], "arguments", [
                    ["name", "offset", "type", "integer"],
                    ["name", "count", "type", "integer"],
                ]],
            ]],
            "xread", ["summary", "s", "group", "stream", "arguments", [
                ["name", "count", "type", "integer", "token", "COUNT", "flags", ["optional"]],
                ["name", "streams", "type", "block", "token", "STREAMS", "arguments", [
                    ["name", "key", "type", "key", "flags", ["multiple"]],
                    ["name", "id", "type", "string", "flags", ["multiple"]],
                ]],
            ]],
            "zinter", ["summary", "s", "group", "sorted-set", "arguments", [
                ["name", "numkeys", "type", "integer"],
                ["name", "key", "type", "key", "flags", ["multiple"]],
                ["name", "weight", "type", "integer", "token", "WEIGHTS", "flags", ["optional", "multiple"]],
                ["name", "aggregate", "type", "oneof", "token", "AGGREGATE", "flags", ["optional"], "arguments", [
                    ["name", "sum", "type", "pure-token", "token", "SUM"],
                    ["name", "min", "type", "pure-token", "token", "MIN"],
                    ["name", "max", "type", "pure-token", "token", "MAX"],
                ]],
            ]],
            "sort", ["summary", "s", "group", "generic", "arguments", [
                ["name", "key", "type", "key"],
                ["name", "pattern", "type", "pattern", "token", "GET", "flags", ["optional", "multiple", "multiple_token"]],
            ]],
        ]);
        let rows = parse_command_docs(&docs);
        let syntax = |n: &str| {
            value_string(&rows.iter().find(|r| r["name"] == json!(n)).expect(n)["syntax"])
        };
        assert_eq!(syntax("ZRANGEBYSCORE"), "key min max [WITHSCORES] [LIMIT offset count]");
        assert_eq!(syntax("XREAD"), "[COUNT count] STREAMS key [key ...] id [id ...]");
        assert_eq!(
            syntax("ZINTER"),
            "numkeys key [key ...] [WEIGHTS weight [weight ...]] [AGGREGATE SUM|MIN|MAX]",
            "multiple repeats the value, not the token"
        );
        assert_eq!(syntax("SORT"), "key [GET pattern [GET pattern ...]]", "multiple_token repeats both");
    }

    #[test]
    fn key_specs_say_where_moving_keys_are_and_an_unknown_spec_is_dropped() {
        // 2026-09-29: XREAD's keys follow STREAMS, ZUNION's follow a count - their legacy
        // first/last/step say nothing. These are the specs redis 7 answers, verbatim in shape.
        let info = json!([
            ["xread", -4, ["readonly"], 0, 0, 0, ["@read"], [], [
                ["flags", ["RO", "access"],
                 "begin_search", ["type", "keyword", "spec", ["keyword", "STREAMS", "startfrom", 1]],
                 "find_keys", ["type", "range", "spec", ["lastkey", -1, "keystep", 1, "limit", 2]]],
            ]],
            ["zunion", -3, ["readonly"], 0, 0, 0, ["@read"], [], [
                ["flags", ["RO", "access"],
                 "begin_search", ["type", "index", "spec", ["index", 1]],
                 "find_keys", ["type", "keynum", "spec", ["keynumidx", 0, "firstkey", 1, "keystep", 1]]],
            ]],
            ["odd", -2, [], 0, 0, 0, [], [], [
                ["flags", [], "begin_search", ["type", "unknown", "spec", []],
                 "find_keys", ["type", "range", "spec", ["lastkey", 0, "keystep", 1, "limit", 0]]],
                ["flags", []], // no begin, no find: not a spec at all
            ]],
            ["get", 2, ["readonly"], 1, 1, 1],
        ]);
        let parsed = parse_command_info(&info);
        let by = |n: &str| parsed.iter().find(|r| r["name"] == json!(n)).expect(n).clone();
        assert_eq!(
            by("XREAD")["keySpecs"],
            json!([{ "begin": { "type": "keyword", "keyword": "STREAMS", "startfrom": 1 },
                     "find": { "type": "range", "lastkey": -1, "keystep": 1, "limit": 2 } }])
        );
        assert_eq!(
            by("ZUNION")["keySpecs"],
            json!([{ "begin": { "type": "index", "index": 1 },
                     "find": { "type": "keynum", "keynumidx": 0, "firstkey": 1, "keystep": 1 } }])
        );
        assert_eq!(by("ODD")["keySpecs"], json!([]), "unknown and shapeless specs are not guessed");
        assert_eq!(by("GET")["keySpecs"], json!([]), "a redis 6 row has none");
        // And the merge carries them onto the catalog row the console reads.
        let merged = merge_command_catalog(
            parse_command_docs(&json!(["xread", ["summary", "Read", "group", "stream"]])),
            parsed,
        );
        let xread = merged.iter().find(|r| r["name"] == json!("XREAD")).expect("XREAD");
        assert_eq!(xread["keySpecs"][0]["begin"]["keyword"], json!("STREAMS"));
        assert_eq!(xread["summary"], json!("Read"));
    }

    #[test]
    fn a_catalog_survives_a_server_that_documents_nothing() {
        // SPEC §data.redis: redis 6 has no COMMAND DOCS, and some proxies answer neither in the
        // shape the spec promises. Junk in is an empty catalog, never a panic.
        assert_eq!(parse_command_docs(&json!("nonsense")), Vec::<Value>::new());
        assert_eq!(parse_command_docs(&json!([])), Vec::<Value>::new());
        assert_eq!(parse_command_info(&json!(null)), Vec::<Value>::new());
        assert_eq!(merge_command_catalog(vec![], vec![]), Vec::<Value>::new());
        // A name with no spec at all still names a command.
        let only_names = parse_command_docs(&json!(["get", []]));
        assert_eq!(only_names[0]["name"], json!("GET"));
        assert_eq!(only_names[0]["syntax"], json!(""));
    }

    #[test]
    fn read_windows_clamp() {
        assert_eq!(read_window(&json!({})), (0, 100));
        assert_eq!(
            read_window(&json!({ "offset": -5, "limit": 5000 })),
            (0, 1000)
        );
        assert_eq!(
            read_window(&json!({ "offset": "40", "limit": "10" })),
            (40, 10)
        );
    }

    #[test]
    fn a_name_that_is_not_utf8_prints_as_redis_cli_prints_it() {
        // Spring Data Redis' JDK serializer: AC ED 00 05, `t`, a two-byte length, the text.
        assert_eq!(
            redis_cli_repr(b"\xac\xed\x00\x05t\x00\x04user"),
            r#""\xac\xed\x00\x05t\x00\x04user""#
        );
        assert_eq!(redis_cli_repr(b"a\"b\\c\n\r\t\x07\x08\x7f"), r#""a\"b\\c\n\r\t\a\b\x7f""#);
        assert_eq!(redis_cli_repr(b""), r#""""#);
    }
}
