//! The in-process Redis adapter — port of `adapters/redis.ts`: one multiplexed auto-reconnecting
//! connection behind a purposeful three-tool set, with the command policy that keeps a shared
//! connection shared.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::config::ServerDef;

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
/// takeover. `read` subcommands survive readonly, `write` ones do not, and anything unlisted is
/// refused — a new Redis release cannot quietly add a subcommand that slips through.
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

/// Read-only commands permitted through redis_command when the MCP is marked readonly. Both
/// dedicated tools (redis_scan, redis_read) are pure reads, so this is the only place a write
/// can enter. Container commands are absent on purpose: their own policy above decides, per
/// subcommand.
const READ_COMMANDS: &[&str] = &[
    "GET",
    "MGET",
    "GETRANGE",
    "STRLEN",
    "EXISTS",
    "TTL",
    "PTTL",
    "TYPE",
    "SCAN",
    "RANDOMKEY",
    "HGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HMGET",
    "HEXISTS",
    "HSCAN",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "SMEMBERS",
    "SCARD",
    "SISMEMBER",
    "SSCAN",
    "SRANDMEMBER",
    "ZRANGE",
    "ZREVRANGE",
    "ZRANGEBYSCORE",
    "ZCARD",
    "ZSCORE",
    "ZCOUNT",
    "ZSCAN",
    "ZRANK",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XINFO",
    "GETBIT",
    "BITCOUNT",
    "OBJECT",
    "INFO",
    "DBSIZE",
    "PING",
    "TIME",
    "LOLWUT",
    "COMMAND",
    "LASTSAVE",
    "DUMP",
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
    readonly: bool,
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
    if set_has(SCRIPTING, &upper) {
        if !allow_eval {
            return Err(format!(
                "{upper} is rejected: the gateway cannot see inside a script, so none of its other rules apply to \
                 one — a single line of Lua can flush the keyspace, block the server in a loop, or run any command \
                 this MCP refuses. Set \"allowEval\": true on this MCP if you accept that."
            ));
        }
        if readonly {
            return Err(format!(
                "{upper} is rejected: \"allowEval\" and \"readonly\" contradict each other on this MCP — a script \
                 the gateway cannot read cannot be held to readonly. Clear one of the two."
            ));
        }
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
            if read.contains(&sub.as_str()) {
                return Ok(()); // a read, so readonly does not apply
            }
            if write.contains(&sub.as_str()) {
                if readonly {
                    return Err(format!(
                        "{upper} {sub} is rejected: this Redis MCP is configured readonly"
                    ));
                }
                return Ok(());
            }
            return Err(format!(
                "{upper} {sub} is rejected: only these subcommands are permitted: {}",
                permitted.join(", ")
            ));
        }
    }
    if readonly && !set_has(READ_COMMANDS, &upper) {
        return Err(format!(
            "{upper} is rejected: this Redis MCP is configured readonly"
        ));
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

    fn readonly(&self) -> bool {
        def_bool(&self.def, "readonly")
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
            self.readonly(),
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
    fn browser(&self) -> Option<crate::dbbrowser_api::BrowserFlavor> {
        Some(crate::dbbrowser_api::BrowserFlavor::Redis(Arc::new(
            super::redis_browser::RedisDataBrowser::new(
                self.where_(),
                self.readonly(),
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

    fn def_bools(readonly: bool, allow_destructive: bool, allow_eval: bool) -> (bool, bool, bool) {
        (readonly, allow_destructive, allow_eval)
    }

    #[test]
    fn policy_rejects_the_named_harms() {
        let (ro, ad, ae) = def_bools(false, false, false);
        // KEYS points at the alternative.
        let err = assert_command_allowed("keys", &[], ro, ad, ae).unwrap_err();
        assert!(err.contains("redis_scan"), "{err}");
        assert!(assert_command_allowed("SUBSCRIBE", &[], ro, ad, ae)
            .unwrap_err()
            .contains("shares one connection"));
        assert!(assert_command_allowed("SHUTDOWN", &[], ro, ad, ae)
            .unwrap_err()
            .contains("server itself"));
        assert!(assert_command_allowed("FLUSHALL", &[], ro, ad, ae)
            .unwrap_err()
            .contains("allowDestructive"));
        assert!(assert_command_allowed("EVAL", &["..."], ro, ad, ae)
            .unwrap_err()
            .contains("cannot see inside a script"));
        assert!(assert_command_allowed("", &[], ro, ad, ae)
            .unwrap_err()
            .contains("required"));
    }

    #[test]
    fn policy_lets_the_opt_ins_through() {
        let (_, ad, ae) = def_bools(false, true, true);
        assert!(assert_command_allowed("FLUSHALL", &[], false, ad, ae).is_ok());
        assert!(assert_command_allowed("EVAL", &["return 1", "0"], false, ad, ae).is_ok());
        // allowEval + readonly still contradict.
        assert!(
            assert_command_allowed("EVAL", &["return 1", "0"], true, ad, ae)
                .unwrap_err()
                .contains("contradict")
        );
    }

    #[test]
    fn readonly_permits_reads_only() {
        let (ro, ad, ae) = def_bools(true, false, false);
        assert!(assert_command_allowed("GET", &["k"], ro, ad, ae).is_ok());
        assert!(assert_command_allowed("SET", &["k", "v"], ro, ad, ae)
            .unwrap_err()
            .contains("readonly"));
        // Container reads survive; CONFIG has no write list at all, so CONFIG SET hits the
        // permitted-list error (Node behavior), while CLIENT's write subcommand hits readonly.
        assert!(assert_command_allowed("CONFIG", &["GET", "maxmemory"], ro, ad, ae).is_ok());
        assert!(
            assert_command_allowed("CONFIG", &["SET", "dir", "/x"], ro, ad, ae)
                .unwrap_err()
                .contains("only these subcommands")
        );
        assert!(
            assert_command_allowed("CLIENT", &["SETNAME", "x"], ro, ad, ae)
                .unwrap_err()
                .contains("readonly")
        );
        assert!(assert_command_allowed("CONFIG", &["RESETSTAT"], ro, ad, ae)
            .unwrap_err()
            .contains("only these subcommands"));
        assert!(assert_command_allowed("CONFIG", &[], ro, ad, ae)
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
