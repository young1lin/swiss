//! Redis Data-view browser — the port of Node's `redisBrowser()`: page keys by SCAN, read one
//! key type-aware, run one console command under the adapter's own guard.

use super::direct::Lazy;
use super::redis::{
    assert_command_allowed, type_aware_read, value_i64, value_string, RedisHandle, SCAN_TYPES,
};
use async_trait::async_trait;
use swiss_host::dbbrowser::RedisBrowser;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

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
fn scan_args(o: &Value, scan_type_supported: bool) -> Result<(Vec<String>, Option<String>), String> {
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
        type_aware_read(handle.as_ref(), key, 0, 1000).await
    }

    async fn run_command(&self, line: &str) -> Result<Value, String> {
        // Split on whitespace (quoted args not offered — redis args are rarely spaced; the MCP
        // redis_command tool remains the full-featured path). The adapter's own guard still
        // refuses KEYS, blocking and server-breaking commands before anything reaches the
        // socket; writes run.
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return Err("type a command, e.g. GET mykey".into());
        }
        let command = parts[0];
        let args = &parts[1..];
        assert_command_allowed(command, args, self.allow_destructive, self.allow_eval)?;
        let handle = self.conn.get().await?;
        let reply = handle.call(command, args).await?;
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
        assert!(!a.iter().any(|x| x == "TYPE"), "TYPE must not reach SCAN: {a:?}");
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
}
