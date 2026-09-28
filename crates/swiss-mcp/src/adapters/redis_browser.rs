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

//! Redis Data-view browser — the port of Node's `redisBrowser()`: page keys by SCAN, read one
//! key type-aware, run one console command under the adapter's own guard.

use super::direct::Lazy;
use super::redis::{
    assert_command_allowed, read_stream_groups, read_stream_window, type_aware_read, value_i64,
    value_string, RedisHandle, RedisReadClient, SCAN_TYPES,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use swiss_host::dbbrowser::{redis_stream_opts, RedisBrowser};

/// One console line into the arguments redis will receive — quotes, escapes and all.
///
/// The splitting is shlex's (2026-09-28), not ours: the console used to `split_whitespace()`,
/// so `SET k "{\"a\": \"b\"}" NX EX 100` reached redis as five broken words and the value was
/// never the JSON the operator typed. The owner asked for a mature library rather than a second
/// hand-written parser here, and shlex is one the build already carries; its rules are the ones
/// the operator knows from a shell and from redis-cli — a double-quoted argument keeps its
/// spaces, `\"` is a quote inside it, a single-quoted one is literal, and everything else is a
/// bare word, `#` and `:` and `*` included (shlex has no comment syntax).
///
/// An unbalanced quote is the one refusal: shlex answers None, and the operator hears which
/// mistake it was instead of watching redis receive half a value.
pub fn split_command_line(line: &str) -> Result<Vec<String>, String> {
    shlex::split(line).ok_or_else(|| {
        "unbalanced quote — close it, or write a literal quote as \\\" inside the argument"
            .to_string()
    })
}

pub struct RedisDataBrowser {
    label: String,
    allow_destructive: bool,
    allow_eval: bool,
    conn: Arc<Lazy<RedisHandle>>,
    /// The server's (major, minor), probed once via INFO and cached (docs/22 closeout B1):
    /// SCAN's TYPE option needs >= 6.0, and probing per page would add a round trip to
    /// every More click for a fact that never changes under us.
    server_version: OnceLock<(u64, u64)>,
}

/// Array replies ship at most this many items to the browser: a console "LRANGE key 0 -1" on a
/// million-element list must not ship a million strings.
const REPLY_CAP: usize = 1000;

/// The version SCAN grew its TYPE option - older servers answer "Unknown option" and the
/// whole walk dies with a 400 (docs/22 closeout B1).
const SCAN_TYPE_SINCE: (u64, u64) = (6, 0);

/// The (major, minor) out of an INFO reply's redis_version line. None when the line is
/// absent or mangled: the caller then degrades to local type filtering, which is correct
/// on every version - only slower on a big keyspace.
fn parse_redis_version(info: &str) -> Option<(u64, u64)> {
    let line = info
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("redis_version:"))?;
    let rest = line.trim_start_matches("redis_version:").trim();
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The full SCAN argument list for one page request, cursor first, plus - when the server
/// is too old for SCAN TYPE - the type brought back for LOCAL filtering.
///
/// Split out from the paging so the coercions can be asserted without a server: the panel sends
/// every query-string param as a JSON STRING (Node coerced with `Number()`), so `count` arrives
/// as `"500"` at least as often as `500`, and an uncoerced or unclamped value is either a SCAN
/// syntax error or a request for the whole keyspace in one round trip.
fn scan_args(
    o: &Value,
    scan_type_supported: bool,
) -> Result<(Vec<String>, Option<String>), String> {
    let cursor = o
        .get("cursor")
        .and_then(Value::as_str)
        .unwrap_or("0")
        .to_string();
    let count = o
        .get("count")
        .and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        })
        .map(|n| n.clamp(1, 1000))
        .unwrap_or(200);
    let pattern = o
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or("*")
        .to_string();
    let type_ = o
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !type_.is_empty() && !SCAN_TYPES.contains(&type_.as_str()) {
        return Err(format!(
            "type must be one of {} — got \"{}\"",
            SCAN_TYPES.join(", "),
            type_
        ));
    }
    let mut args: Vec<String> = vec![
        cursor,
        "MATCH".into(),
        pattern,
        "COUNT".into(),
        count.to_string(),
    ];
    let mut local_type: Option<String> = None;
    if !type_.is_empty() {
        if scan_type_supported {
            args.push("TYPE".into());
            args.push(type_);
        } else {
            // Pre-6.0 server (docs/22 closeout B1): the option would 400 the whole walk, so
            // the type filters LOCALLY over the per-key TYPEs the page pipeline fetches anyway.
            local_type = Some(type_);
        }
    }
    Ok((args, local_type))
}

/// The console guard applied to every command of a structured-edit pipeline (docs/22
/// W3.3), BEFORE any connection is opened: one bad command refuses the whole batch, so a
/// buffered edit never half-applies. Split out from run_pipeline so the rule is testable
/// without a server, exactly like assert_command_allowed itself.
fn vet_pipeline(
    commands: &[Vec<String>],
    allow_destructive: bool,
    allow_eval: bool,
) -> Result<Vec<(&str, Vec<&str>)>, String> {
    let mut vetted = Vec::with_capacity(commands.len());
    for cmd in commands {
        let (verb, args) = cmd
            .split_first()
            .ok_or_else(|| "each command must be an array: [verb, arg...]".to_string())?;
        if verb.trim().is_empty() {
            return Err("command is required".into());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_command_allowed(verb, &arg_refs, allow_destructive, allow_eval)?;
        vetted.push((verb.as_str(), arg_refs));
    }
    Ok(vetted)
}

impl RedisDataBrowser {
    pub fn new(
        label: String,
        allow_destructive: bool,
        allow_eval: bool,
        conn: Arc<Lazy<RedisHandle>>,
    ) -> Self {
        Self {
            label,
            allow_destructive,
            allow_eval,
            conn,
            server_version: OnceLock::new(),
        }
    }
}

impl RedisDataBrowser {
    /// Whether the server understands SCAN TYPE (>= 6.0). Probed once with INFO and cached;
    /// an unprobed or failed probe degrades to LOCAL filtering, which is correct everywhere.
    async fn supports_scan_type(&self, handle: &RedisHandle) -> bool {
        if let Some(v) = self.server_version.get() {
            return *v >= SCAN_TYPE_SINCE;
        }
        let supported = handle
            .call("INFO", &["server"])
            .await
            .ok()
            .and_then(|v| parse_redis_version(&value_string(&v)))
            .map(|v| {
                let _ = self.server_version.set(v);
                v >= SCAN_TYPE_SINCE
            })
            .unwrap_or(false);
        supported
    }
}

#[async_trait]
impl RedisBrowser for RedisDataBrowser {
    fn label(&self) -> String {
        self.label.clone()
    }

    async fn list_keys(&self, o: &Value) -> Result<Value, String> {
        // Built BEFORE the connection: a bad `type` is the caller's mistake, and reporting it as
        // a connection error when the server happens to be down helps nobody. The discarded
        // build only validates - the real one below decides TYPE by server version.
        scan_args(o, true)?;
        let handle = self.conn.get().await?;
        // docs/22 closeout B1: SCAN TYPE needs >= 6.0; on an older server the type comes back
        // for local filtering instead of riding the SCAN.
        let supported = self.supports_scan_type(handle.as_ref()).await;
        let (scan_args, local_type) = scan_args(o, supported)?;
        let arg_refs: Vec<&str> = scan_args.iter().map(String::as_str).collect();
        let reply = handle.call("SCAN", &arg_refs).await?;
        // SCAN answers [cursor, keys]; anything else cannot be paged.
        let (next, keys) = match &reply {
            Value::Array(pair) if pair.len() == 2 => {
                let next = value_string(&pair[0]);
                let keys = pair[1]
                    .as_array()
                    .map(|a| a.iter().map(value_string).collect::<Vec<_>>())
                    .unwrap_or_default();
                (next, keys)
            }
            _ => return Err("unexpected SCAN reply shape".into()),
        };
        // Type and TTL per key, sent through ONE pipeline per chunk: the commands ride a single
        // network round trip instead of 2×keys serialized ones (measured: a 200-key page fell
        // from ~880 ms to tens of ms). Chunked at 200 pairs so one pipeline is never unbounded;
        // DBSIZE rides the FIRST chunk — the whole page costs two sequential round trips.
        const PIPE_CHUNK: usize = 200;
        let mut infos: Vec<Value> = Vec::new();
        let mut total: Option<i64> = None;
        let mut chunks: Vec<usize> = (0..keys.len()).step_by(PIPE_CHUNK).collect();
        if keys.is_empty() {
            chunks.push(0); // an empty page still asks for DBSIZE once
        }
        for (n, start) in chunks.iter().enumerate() {
            let start = *start;
            let end = (start + PIPE_CHUNK).min(keys.len());
            let chunk = &keys[start..end];
            let mut commands: Vec<(String, Vec<String>)> = Vec::with_capacity(chunk.len() * 2 + 1);
            for k in chunk {
                commands.push(("TYPE".into(), vec![k.clone()]));
                commands.push(("TTL".into(), vec![k.clone()]));
            }
            if n == 0 {
                commands.push(("DBSIZE".into(), Vec::new()));
            }
            let results = handle.pipeline(&commands).await?;
            for (j, k) in chunk.iter().enumerate() {
                let type_ = results
                    .get(j * 2)
                    .map(value_string)
                    .unwrap_or_else(|| "none".into());
                let ttl = results.get(j * 2 + 1).map(value_i64).unwrap_or(-2);
                infos.push(json!({"key": k, "type": type_, "ttl": ttl}));
            }
            if n == 0 {
                if let Some(dbsize) = results.get(chunk.len() * 2) {
                    total = Some(value_i64(dbsize));
                }
            }
        }
        if let Some(want) = &local_type {
            // Pre-6.0 filtering (docs/22 closeout B1): every key's TYPE is already in hand, so
            // the filter is a retain. A page left empty by it is NORMAL - the walk continues
            // from the raw cursor, and done is cursor == 0, never the key count.
            infos.retain(|i| i.get("type").and_then(Value::as_str) == Some(want.as_str()));
        }
        Ok(json!({"keys": infos, "cursor": next, "done": next == "0", "total": total}))
    }

    async fn read_key(&self, key: &str) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        // docs/45 §2.1: a stream key's /key answer is the newest-first window, not
        // type_aware_read's oldest-first page — the panel opens a key without knowing
        // its type, so the first response already carries the right shape. TYPE (and
        // TTL, the one fact every other type's /key answer carries) ride ahead;
        // type_aware_read itself is untouched: the MCP redis_read tool keeps its
        // published oldest-first contract (docs/45 D8). The extra TYPE round trip is a
        // one-click cost, never a per-tick one.
        let type_ = handle.as_ref().type_of(key).await?;
        if type_ == "stream" {
            let ttl = handle.as_ref().ttl(key).await?;
            let mut window = self.read_stream(key, &json!({})).await?;
            if let Some(map) = window.as_object_mut() {
                map.insert("ttl".into(), json!(ttl));
            }
            return Ok(window);
        }
        type_aware_read(handle.as_ref(), key, 0, 1000).await
    }

    async fn read_stream(&self, key: &str, o: &Value) -> Result<Value, String> {
        // docs/45 §2.1: the window over the leased shared handle. Options go through
        // the same host helper the route validates with — one rule, no drift between
        // transport and model.
        if key.is_empty() {
            return Err("key is required".into());
        }
        let (bound, count) = redis_stream_opts(o)?;
        let handle = self.conn.get().await?;
        read_stream_window(handle.as_ref(), key, &bound, count).await
    }

    async fn stream_groups(&self, key: &str) -> Result<Value, String> {
        // docs/45 §2.4: the read-only consumer-group fold — XINFO GROUPS is the
        // whole story; no XACK/XCLAIM/XTRIM ever leaves this surface.
        if key.is_empty() {
            return Err("key is required".into());
        }
        let handle = self.conn.get().await?;
        read_stream_groups(handle.as_ref(), key).await
    }

    async fn run_command(&self, line: &str) -> Result<Value, String> {
        // The adapter's own guard still refuses KEYS, blocking and server-breaking commands
        // before anything reaches the socket; writes run.
        let parts = split_command_line(line)?;
        let Some((command, rest)) = parts.split_first() else {
            return Err("type a command, e.g. GET mykey".into());
        };
        let args: Vec<&str> = rest.iter().map(String::as_str).collect();
        assert_command_allowed(command, &args, self.allow_destructive, self.allow_eval)?;
        let handle = self.conn.get().await?;
        let reply = handle.call(command, &args).await?;
        if let Value::Array(items) = &reply {
            if items.len() > REPLY_CAP {
                return Ok(json!({
                    "truncated": true,
                    "note": format!("showing first {REPLY_CAP} of {}", items.len()),
                    "items": &items[..REPLY_CAP],
                }));
            }
        }
        Ok(reply)
    }

    async fn run_pipeline(&self, commands: &[Vec<String>]) -> Result<Value, String> {
        // Guarded whole, before the connection is opened: the refusal names the command, and
        // a batch that cannot run in full does not run at all (docs/22 W3.3).
        let vetted = vet_pipeline(commands, self.allow_destructive, self.allow_eval)?;
        let handle = self.conn.get().await?;
        let owned: Vec<(String, Vec<String>)> = vetted
            .into_iter()
            .map(|(verb, args)| {
                (
                    verb.to_string(),
                    args.into_iter().map(str::to_string).collect(),
                )
            })
            .collect();
        let replies = handle.pipeline(&owned).await?;
        Ok(json!({ "replies": replies }))
    }

    /// docs/43 M3 (ADR-027 option b): INFO keyspace names every dbN with its key count and
    /// CLIENT INFO says which one this connection SELECTed at connect time — that number is
    /// both primary and current. Every OTHER database is listed but not browsable: SELECT is
    /// a connection-breaking command on the shared handle (it would permanently re-mode the
    /// pooled connection), so opening one needs its own connection, and the reason here is
    /// the sentence the panel shows on the disabled entry.
    ///
    /// CLIENT INFO is refused by proxy-fronted redis (Codis and friends answer "unknown
    /// subcommand ... Try CLIENT HELP"), and a catalog must not 500 on that: the fallback is
    /// db0 — the connect-time SELECT's own default — and the keyspace rows still list.
    async fn list_databases(&self) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let (ks, client) = tokio::join!(
            handle.call("INFO", &["keyspace"]),
            handle.call("CLIENT", &["INFO"])
        );
        let ks = value_string(&ks?);
        // "db=3 " inside the CLIENT INFO id-addr-db-... line; missing field or refused
        // command both fall back to "0" (the db an unconfigured connection sits on).
        let current = match client {
            Ok(v) => value_string(&v)
                .split_whitespace()
                .find(|f| f.starts_with("db="))
                .and_then(|f| f.trim_start_matches("db=").parse::<i64>().ok())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "0".to_string()),
            Err(_) => "0".to_string(),
        };
        Ok(json!({
            "primary": current,
            "current": current,
            "databases": keyspace_databases(&ks, &current),
        }))
    }
}

/// The catalog rows of one INFO keyspace section. Keyspace lines look like
/// "db2:keys=170,expires=4,avg_ttl=1200" — name is the number after db, tables carries the
/// key count (the redis twin of a table count). INFO keyspace names only the databases that
/// HOLD keys, so the one this connection sits on is added at 0 keys when it is empty: on an
/// empty instance the section is bare, and a catalog without the current database hid the
/// panel's database row until the first write.
fn keyspace_databases(ks: &str, current: &str) -> Vec<Value> {
    let row = |name: &str, keys: i64| {
        let is_current = name == current;
        json!({
            "name": name,
            "primary": is_current,
            "browsable": is_current,
            "system": false,
            "tables": keys,
            "reason": if is_current { Value::Null } else {
                json!("SELECT would break the shared connection; opening another database needs its own connection.")
            },
        })
    };
    let mut databases: Vec<Value> = Vec::new();
    for line in ks.lines() {
        let line = line.trim();
        let Some((db, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(name) = db.strip_prefix("db") else {
            continue;
        };
        let keys = rest
            .split(',')
            .find_map(|f| f.strip_prefix("keys=").and_then(|n| n.parse::<i64>().ok()))
            .unwrap_or(0);
        databases.push(row(name, keys));
    }
    if !databases.iter().any(|d| d["name"] == current) {
        databases.push(row(current, 0));
    }
    databases
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(o: Value) -> Vec<String> {
        scan_args(&o, true).expect("valid params").0
    }

    fn value_at(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    #[test]
    fn an_empty_keyspace_still_lists_the_database_the_connection_sits_on() {
        // What INFO keyspace answers on an empty instance: the header and nothing else.
        let rows = keyspace_databases("# Keyspace\r\n", "0");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["name"], "0");
        assert_eq!(rows[0]["primary"], true);
        assert_eq!(rows[0]["browsable"], true);
        assert_eq!(rows[0]["tables"], 0);

        // Keys elsewhere only: the current db is added empty beside the listed one.
        let rows = keyspace_databases("# Keyspace\r\ndb3:keys=5,expires=0,avg_ttl=0\r\n", "0");
        let names: Vec<&str> = rows.iter().filter_map(|r| r["name"].as_str()).collect();
        assert_eq!(names, ["3", "0"]);
        assert_eq!(rows[0]["browsable"], false);
        assert_eq!(rows[1]["tables"], 0);

        // Listed already: never twice.
        let rows = keyspace_databases("db0:keys=1,expires=0,avg_ttl=0\n", "0");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tables"], 1);
    }

    #[test]
    fn an_empty_request_pages_from_the_start_of_the_whole_keyspace() {
        // What the panel sends when the Data tab is first opened.
        let a = args(json!({}));
        assert_eq!(a[0], "0", "cursor 0 is the start of a SCAN");
        assert_eq!(value_at(&a, "MATCH").as_deref(), Some("*"));
        assert_eq!(value_at(&a, "COUNT").as_deref(), Some("200"));
        assert!(!a.iter().any(|x| x == "TYPE"), "no type filter: {a:?}");
    }

    #[test]
    fn a_count_arriving_as_a_string_is_still_a_count() {
        // The route forwards query-string params as JSON strings, so this is the COMMON case, not
        // the exotic one. An uncoerced "500" would reach redis as a SCAN syntax error.
        assert_eq!(
            value_at(&args(json!({"count": "500"})), "COUNT").as_deref(),
            Some("500")
        );
        assert_eq!(
            value_at(&args(json!({"count": 500})), "COUNT").as_deref(),
            Some("500")
        );
        assert_eq!(
            value_at(&args(json!({"count": " 42 "})), "COUNT").as_deref(),
            Some("42")
        );
    }

    #[test]
    fn a_count_is_clamped_at_both_ends() {
        // 0 and negatives are SCAN syntax errors; an unbounded COUNT asks redis for the whole
        // keyspace in one round trip and blocks the server while it walks it.
        assert_eq!(
            value_at(&args(json!({"count": 0})), "COUNT").as_deref(),
            Some("1")
        );
        assert_eq!(
            value_at(&args(json!({"count": -7})), "COUNT").as_deref(),
            Some("1")
        );
        assert_eq!(
            value_at(&args(json!({"count": 1_000_000})), "COUNT").as_deref(),
            Some("1000")
        );
        assert_eq!(
            value_at(&args(json!({"count": "999999"})), "COUNT").as_deref(),
            Some("1000")
        );
    }

    #[test]
    fn a_count_that_is_not_a_number_falls_back_rather_than_failing_the_page() {
        // A junk param should not take the Data tab down; the default page is the safe answer.
        for junk in [
            json!("abc"),
            json!(null),
            json!(true),
            json!([1]),
            json!({}),
        ] {
            assert_eq!(
                value_at(&args(json!({ "count": junk })), "COUNT").as_deref(),
                Some("200"),
                "{junk}"
            );
        }
    }

    #[test]
    fn a_cursor_and_pattern_ride_through_as_given() {
        // The cursor is redis's own opaque token — parsing or renumbering it would break paging.
        let a = args(json!({"cursor": "17408", "pattern": "session:*"}));
        assert_eq!(a[0], "17408");
        assert_eq!(value_at(&a, "MATCH").as_deref(), Some("session:*"));
    }

    #[test]
    fn a_type_filter_is_lowercased_and_appended() {
        let a = args(json!({"type": "Hash"}));
        assert_eq!(value_at(&a, "TYPE").as_deref(), Some("hash"));
        let a = args(json!({"type": "  ZSET  "}));
        assert_eq!(value_at(&a, "TYPE").as_deref(), Some("zset"));
    }

    #[test]
    fn every_type_redis_knows_is_accepted() {
        for t in SCAN_TYPES {
            assert_eq!(
                value_at(&args(json!({ "type": t })), "TYPE").as_deref(),
                Some(*t)
            );
        }
        // Blank means "no filter", not "a type called empty".
        for blank in ["", "   "] {
            let a = args(json!({ "type": blank }));
            assert!(!a.iter().any(|x| x == "TYPE"), "{blank:?} -> {a:?}");
        }
    }

    #[test]
    fn an_unknown_type_is_refused_by_name_before_anything_is_dialled() {
        // The message has to list the valid options: the panel offers a dropdown, but the MCP
        // tool path lets a model pass anything, and "invalid" alone just makes it guess again.
        let err = scan_args(&json!({"type": "sorted-set"}), true).expect_err("refused");
        assert!(err.contains("sorted-set"), "{err}");
        for t in SCAN_TYPES {
            assert!(err.contains(t), "{err} should name {t}");
        }
    }

    // --- docs/22 closeout B1: SCAN TYPE needs server >= 6.0 -------------------------------------
    // On an older server the option is a 400 and the whole walk dies; the type then filters
    // LOCALLY, over the TYPEs the page pipeline already fetches per key.

    #[test]
    fn the_redis_version_is_parsed_out_of_an_info_reply() {
        let info = r#"# Server
redis_version:5.0.5
redis_git_sha1:abc
redis_mode:standalone
"#;
        assert_eq!(parse_redis_version(info), Some((5, 0)));
        assert_eq!(parse_redis_version("redis_version:7.2.4"), Some((7, 2)));
        assert_eq!(parse_redis_version("redis_version:6.0.0"), Some((6, 0)));
        // No version line (or a mangled one) is None: the caller degrades to local filtering.
        assert_eq!(parse_redis_version("redis_mode:standalone"), None);
        assert_eq!(parse_redis_version("redis_version:x.y.z"), None);
        assert_eq!(parse_redis_version(""), None);
    }

    #[test]
    fn an_older_server_filters_locally_the_type_never_reaches_scan() {
        // The pre-6.0 path: no TYPE token in the args, the type rides back for local filtering.
        let (a, local) = scan_args(&json!({"type": "hash"}), false).expect("valid params");
        assert!(
            !a.iter().any(|x| x == "TYPE"),
            "TYPE must not reach SCAN: {a:?}"
        );
        assert_eq!(local.as_deref(), Some("hash"));
        // A 6.0+ server keeps the server-side filter and filters nothing locally.
        let (a, local) = scan_args(&json!({"type": "hash"}), true).expect("valid params");
        assert_eq!(value_at(&a, "TYPE").as_deref(), Some("hash"));
        assert_eq!(local, None);
        // The type whitelist applies on BOTH paths — local filtering is not a way around it.
        assert!(scan_args(&json!({"type": "bogus"}), false).is_err());
        // No type filter: neither path carries one.
        let (a, local) = scan_args(&json!({}), false).expect("valid params");
        assert!(!a.iter().any(|x| x == "TYPE"));
        assert_eq!(local, None);
    }

    fn batch(parts: &[&[&str]]) -> Vec<Vec<String>> {
        parts
            .iter()
            .map(|c| c.iter().map(|p| p.to_string()).collect())
            .collect()
    }

    #[test]
    fn a_structured_edit_batch_passes_the_same_guard_as_the_console() {
        // Every verb the typed value editors can buffer (docs/22 W3.3), under the strictest
        // flags a config can set — the panel's Commit must not depend on allow* to work.
        let cmds = batch(&[
            &["SET", "k", "v"],
            &["HSET", "h", "field", "a value with spaces"],
            &["HDEL", "h", "field"],
            &["ZADD", "z", "1.5", "member"],
            &["ZREM", "z", "member"],
            &["LSET", "l", "0", "value"],
            &["RPUSH", "l", "value"],
            &["SADD", "s", "member"],
            &["SREM", "s", "member"],
            &["EXPIRE", "k", "60"],
        ]);
        let vetted = vet_pipeline(&cmds, false, false).expect("the edit vocabulary passes");
        assert_eq!(vetted.len(), cmds.len());
        // The vetted list borrows the batch: args survive untouched, spaces included.
        assert_eq!(vetted[1].1, vec!["h", "field", "a value with spaces"]);
    }

    #[test]
    fn a_pipeline_is_refused_whole_when_any_command_is_on_the_deny_list() {
        // One KEYS buried mid-batch refuses everything before a socket is touched: a buffered
        // edit must never half-apply (docs/22 W3.3).
        let cmds = batch(&[
            &["HSET", "h", "f", "v"],
            &["KEYS", "*"] as &[&str],
            &["HDEL", "h", "g"],
        ]);
        let err = vet_pipeline(&cmds, true, true).expect_err("refused whole");
        assert!(err.contains("KEYS is rejected"), "{err}");
    }

    #[test]
    fn a_destructive_pipeline_command_needs_the_same_flag_as_alone() {
        let cmds = batch(&[&["FLUSHALL"] as &[&str]]);
        assert!(vet_pipeline(&cmds, false, false).is_err());
        assert!(vet_pipeline(&cmds, true, false).is_ok());
    }

    /* --- the console's argument splitting (2026-09-28) -----------------------------------------
    The owner typed `SET test11 "{\"test\": \"123\"}" NX EX 100` and the console sent redis
    five broken words, because the line was split on whitespace. The rules below are shlex's;
    the cases are the ones a redis console actually meets — JSON values, spaces, apostrophes,
    binary-ish escapes, glob patterns and the half-typed quote. */

    fn split(line: &str) -> Vec<String> {
        split_command_line(line).expect("splits")
    }

    #[test]
    fn a_quoted_json_value_reaches_redis_as_one_argument() {
        assert_eq!(
            split(r#"SET test11 "{\"test\": \"123\"}" NX EX 100"#),
            vec!["SET", "test11", r#"{"test": "123"}"#, "NX", "EX", "100"]
        );
    }

    #[test]
    fn the_shapes_a_console_line_comes_in() {
        // A bare word, the plainest line there is.
        assert_eq!(split("GET mykey"), vec!["GET", "mykey"]);
        // Runs of whitespace, tabs and a trailing newline are separators, not arguments.
        assert_eq!(split("  GET \t mykey \n"), vec!["GET", "mykey"]);
        // A double-quoted argument keeps its spaces and its punctuation.
        assert_eq!(
            split(r#"HSET h field "a value with spaces""#),
            vec!["HSET", "h", "field", "a value with spaces"]
        );
        // A single-quoted one is literal: the apostrophe problem in reverse.
        assert_eq!(
            split(r#"SET note 'it is Ada"s'"#),
            vec!["SET", "note", r#"it is Ada"s"#]
        );
        // An empty argument is an argument — SET k "" is a real command.
        assert_eq!(split(r#"SET k """#), vec!["SET", "k", ""]);
        // Quotes glued to a word join it: redis sees one key, not two arguments.
        assert_eq!(split(r#"GET "user":"1""#), vec!["GET", "user:1"]);
        // A backslash outside quotes escapes the next character, space included.
        assert_eq!(split(r"GET my\ key"), vec!["GET", "my key"]);
        // Nothing in a key's own vocabulary is magic: #, :, *, {} and - are bare characters.
        assert_eq!(
            split("SCAN 0 MATCH user:*:#tag{1}-x"),
            vec!["SCAN", "0", "MATCH", "user:*:#tag{1}-x"]
        );
        // The empty line and a line of spaces split to nothing — the caller says what to type.
        assert!(split("").is_empty());
        assert!(split("   ").is_empty());
    }

    #[test]
    fn an_unbalanced_quote_is_named_instead_of_half_sent() {
        for line in [
            r#"SET k "half"#,
            r#"SET k 'half"#,
            r#"SET k "a\""#,
            r"SET k tail\",
        ] {
            let err = split_command_line(line).expect_err("refused");
            assert!(err.contains("unbalanced quote"), "{line}: {err}");
        }
    }

    #[test]
    fn a_line_that_is_only_a_command_still_carries_no_arguments() {
        assert_eq!(split("DBSIZE"), vec!["DBSIZE"]);
        assert_eq!(split("  PING  "), vec!["PING"]);
    }

    #[test]
    fn utf8_survives_the_split_whole() {
        // A CJK value in quotes, and one bare: both reach redis as typed.
        assert_eq!(split("SET 城市 \"上 海\""), vec!["SET", "城市", "上 海"]);
    }
}
