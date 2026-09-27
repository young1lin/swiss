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

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::StreamBound;

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
/// conventions: one line of prose is far cheaper than ten JSON schemas.
fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "redis_scan".into(),
            description: concat!(
                "Incrementally list keys matching a glob pattern (cursor-based SCAN — safe on large ",
                "instances, unlike KEYS, which blocks the server and can return hundreds of thousands of ",
                "keys). Returns { cursor, keys, done }; pass the returned cursor back to continue until ",
                "done is true.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. `session:*`. Omit for all keys." },
                    "cursor": { "type": "string", "description": "Cursor from a previous call. Omit to start." },
                    "count": { "type": "number", "description": "Keys scanned per iteration (default 100)." },
                    "type": { "type": "string", "description": "Only keys of this type: string, hash, list, set, zset or stream." },
                },
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
                    "offset": { "type": "number", "description": "First entry to return, by rank (list, zset, stream). Default 0." },
                    "limit": { "type": "number", "description": "Entries to return (list, zset, stream). Default 100, max 1000." },
                },
                "required": ["key"],
            }),
        },
        ToolDef {
            name: "redis_command".into(),
            description: concat!(
                "Run any other Redis command: { command: \"SET\", args: [\"k\", \"v\", \"EX\", \"60\"] }. Covers ",
                "SET/DEL/EXISTS/TTL/TYPE/EXPIRE/INCR, MGET, HSET/HGET/HDEL, LPUSH/LRANGE/LLEN, SADD/SMEMBERS, ",
                "ZADD/ZRANGE, XADD/XRANGE, INFO/DBSIZE/CONFIG GET, and the rest. Bound your range reads ",
                "(`LRANGE key 0 99`) — output is capped either way. Return conventions: status replies come ",
                "back as \"OK\"/\"PONG\", integer replies as numbers, a missing value as null, and collections ",
                "as FLAT arrays — `ZRANGE k 0 -1 WITHSCORES` is [member, score, member, score]; prefer ",
                "redis_read when you want the shaped version. Rejected: KEYS (use redis_scan), Lua and ",
                "functions (EVAL/SCRIPT/FCALL), transactions (MULTI/EXEC/WATCH), anything that would break this ",
                "shared connection (SUBSCRIBE/MONITOR/BLPOP…) or the server (SHUTDOWN/DEBUG/CONFIG SET/ACL/",
                "MODULE/SAVE…), and FLUSHALL/FLUSHDB unless this MCP permits them.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command name, e.g. SET / TTL / XRANGE / INFO." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Command arguments, in order." },
                },
                "required": ["command"],
            }),
        },
    ]
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

// --- stream windows (docs/45) ----------------------------------------------------------------------

/// A stream entry id is "<ms>-<seq>", and the ms half IS the entry's timestamp
/// (docs/45 §2.1) — redis guarantees it at XADD time. None for anything that is not
/// a plain id: the special ids ("$", "+", "-"), malformed strings, and a ms half
/// that overflows i64 all mean “derive nothing”, because a wrong timestamp is worse
/// than a blank one.
pub fn stream_id_ms(id: &str) -> Option<i64> {
    let (ms, _seq) = id.split_once('-')?;
    ms.parse::<i64>().ok()
}

/// The id's ms half as ISO 8601 with millisecond precision and a Z (docs/45 §2.1),
/// derived ONCE on the server because every consumer — panel now, agent later —
/// would otherwise re-implement id parsing, and the panel never should: ids are
/// redis's wire vocabulary, the timestamp is our display vocabulary, and the
/// translation belongs where the data enters the process.
pub fn stream_id_to_ts(id: &str) -> Option<String> {
    let ms = stream_id_ms(id)?;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// The XREVRANGE argument list for one window (docs/45 §2.1). Why XREVRANGE for
/// BOTH directions (D6): XREAD would answer “newer” natively, but it is a
/// blocking command and this adapter's one shared connection must never be held
/// (docs/22), so the follow tick is a poll — and a poll wants the NEWEST n
/// entries, which XRANGE (+ after-cursor) would answer with the OLDEST of the
/// backlog instead. The exclusive “(” bounds keep the cursor row itself out:
/// it is already on screen, and an inclusive bound would duplicate it on every
/// page. Newest-first is XREVRANGE's native order, so nothing between redis and
/// the DOM ever reverses a page.
pub fn stream_window_args(key: &str, bound: &StreamBound, count: i64) -> Vec<String> {
    let (start, end) = match bound {
        StreamBound::Newest => ("+".to_string(), "-".to_string()),
        StreamBound::Before(id) => (format!("({id}"), "-".to_string()),
        StreamBound::After(id) => ("+".to_string(), format!("({id}")),
    };
    vec![
        key.to_string(),
        start,
        end,
        "COUNT".into(),
        count.to_string(),
    ]
}

/// The union of field names across a window, in first-seen order scanning
/// newest-first (docs/45 D3). A feed's field set is stable, so the union is
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
/// parsed into the panel's table rows (docs/45 §2.4). `lag` exists from redis
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

/// Assemble the window reply the panel codes against (docs/45 §2.1) from the
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
    let raw = entries_reply.as_array().cloned().unwrap_or_default();
    let entries: Vec<Value> = stream_entries(&raw)
        .into_iter()
        .map(|e| {
            let id = e.get("id").map(value_string).unwrap_or_default();
            let ts = stream_id_to_ts(&id).map(Value::String).unwrap_or(Value::Null);
            let mut obj = e.as_object().cloned().unwrap_or_default();
            obj.insert("ts".into(), ts);
            Value::Object(obj)
        })
        .collect();
    let more = (entries.len() as i64) == count;
    let (first, last) = stream_ends(info_reply);
    json!({
        "key": key,
        "type": "stream",
        "length": value_i64(len_reply),
        "firstId": first,
        "lastId": last,
        "entries": entries,
        "columns": stream_columns(&entries),
        "more": more,
    })
}

/// The stream window over the shared handle (docs/45 §2.1): XREVRANGE (the
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
    bound: &StreamBound,
    count: i64,
) -> Result<Value, String> {
    let args = stream_window_args(key, bound, count);
    let commands = vec![
        ("XREVRANGE".to_string(), args),
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
    Ok(stream_window_shape(
        key,
        replies.first().unwrap_or(&Value::Null),
        replies.get(1).unwrap_or(&Value::Null),
        replies.get(2).unwrap_or(&Value::Null),
        count,
    ))
}

/// XINFO GROUPS, read-only (docs/45 §2.4): the consumer-group table behind the
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
        .map(redis_value_to_json)
        .map_err(|e| e.to_string())
    }

    /// One pipelined round trip: every command rides a single network request, replies come back
    /// in order — ioredis's `pipeline()`, which cut a 200-key Data-view page from ~880 ms to
    /// tens of ms versus serialized TYPE/TTL calls. The same 10s commandTimeout bounds it.
    pub async fn pipeline(&self, commands: &[(String, Vec<String>)]) -> Result<Vec<Value>, String> {
        let mut conn = self.manager.clone();
        let mut pipe = redis::pipe();
        for (command, args) in commands {
            let mut cmd = pipe.cmd(command.as_str());
            for arg in args {
                cmd = cmd.arg(arg.as_str());
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
            let info = redis::ConnectionInfo {
                addr: redis::ConnectionAddr::Tcp(host.clone(), port),
                redis: redis::RedisConnectionInfo {
                    db,
                    username: None,
                    password: password.clone(),
                    protocol: redis::ProtocolVersion::RESP2,
                },
            };
            Box::pin(async move {
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
        let count = args
            .get("count")
            .and_then(as_i64)
            // Node's `Number(...) || 100`: a zero (or unparseable) count is "not sent" — SCAN
            // with COUNT 0 is a protocol error, never a useful ask.
            .filter(|n| *n > 0)
            .unwrap_or(100)
            .clamp(1, 10_000);
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
        // Through the raw channel: the TYPE filter only exists as an option token, and SCAN's
        // raw reply is already [cursor, keys].
        let mut scan_args: Vec<String> = vec![
            cursor,
            "MATCH".into(),
            pattern,
            "COUNT".into(),
            count.to_string(),
        ];
        if !type_.is_empty() {
            scan_args.push("TYPE".into());
            scan_args.push(type_);
        }
        let refs: Vec<&str> = scan_args.iter().map(String::as_str).collect();
        let reply = RedisHandle::call(&client, "SCAN", &refs).await?;
        let (next, keys) = match &reply {
            Value::Array(pair) if pair.len() >= 2 => (
                value_string(&pair[0]),
                match &pair[1] {
                    Value::Array(items) => items.iter().map(value_string).collect::<Vec<_>>(),
                    _ => Vec::new(),
                },
            ),
            _ => ("0".into(), Vec::new()),
        };
        let done = next == "0";
        Ok(json!({ "cursor": next, "keys": keys, "done": done }))
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
        tools()
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
        // (docs/22 W1.3) — pinned here so a future deny-list sweep cannot quietly break the
        // panel's Rename / Set-TTL sheets.
        assert!(assert_command_allowed("SET", &["k", "v"], false, false).is_ok());
        assert!(assert_command_allowed("DEL", &["k"], false, false).is_ok());
        assert!(assert_command_allowed("EXPIRE", &["k", "60"], false, false).is_ok());
        assert!(assert_command_allowed("RENAME", &["k", "k2"], false, false).is_ok());
        assert!(assert_command_allowed("PERSIST", &["k"], false, false).is_ok());
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
        // docs/45 §2.1: the ms half is the timestamp; everything that is not a
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
        // docs/45 §2.1/D6: both directions are XREVRANGE with exclusive “(” bounds,
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
        // docs/45 D3: newest-first scan, first-seen order — the ragged seed's five
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
        // docs/45 §2.4: lag exists from redis 7 (6.2 omits it -> null, never 0:
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
        // docs/45 §2.1: the reply the panel codes against — newest-first entries,
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
}
