//! Redis Data-view browser — the port of Node's `redisBrowser()`: page keys by SCAN, read one
//! key type-aware, run one console command under the adapter's own guard.

use super::direct::Lazy;
use super::redis::{
    assert_command_allowed, type_aware_read, value_i64, value_string, RedisHandle, SCAN_TYPES,
};
use async_trait::async_trait;
use lmg_host::dbbrowser::RedisBrowser;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct RedisDataBrowser {
    label: String,
    readonly: bool,
    allow_destructive: bool,
    allow_eval: bool,
    conn: Arc<Lazy<RedisHandle>>,
}

/// Array replies ship at most this many items to the browser: a console "LRANGE key 0 -1" on a
/// million-element list must not ship a million strings.
const REPLY_CAP: usize = 1000;

/// The full SCAN argument list for one page request, `cursor` first.
///
/// Split out from the paging so the coercions can be asserted without a server: the panel sends
/// every query-string param as a JSON STRING (Node coerced with `Number()`), so `count` arrives
/// as `"500"` at least as often as `500`, and an uncoerced or unclamped value is either a SCAN
/// syntax error or a request for the whole keyspace in one round trip.
fn scan_args(o: &Value) -> Result<Vec<String>, String> {
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
    if !type_.is_empty() {
        args.push("TYPE".into());
        args.push(type_);
    }
    Ok(args)
}

impl RedisDataBrowser {
    pub fn new(
        label: String,
        readonly: bool,
        allow_destructive: bool,
        allow_eval: bool,
        conn: Arc<Lazy<RedisHandle>>,
    ) -> Self {
        Self {
            label,
            readonly,
            allow_destructive,
            allow_eval,
            conn,
        }
    }
}

#[async_trait]
impl RedisBrowser for RedisDataBrowser {
    fn readonly(&self) -> bool {
        self.readonly
    }
    fn label(&self) -> String {
        self.label.clone()
    }

    async fn list_keys(&self, o: &Value) -> Result<Value, String> {
        // Built BEFORE the connection: a bad `type` is the caller's mistake, and reporting it as
        // a connection error when the server happens to be down helps nobody.
        let scan_args = scan_args(o)?;
        let handle = self.conn.get().await?;
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
        Ok(json!({"keys": infos, "cursor": next, "done": next == "0", "total": total}))
    }

    async fn read_key(&self, key: &str) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        type_aware_read(handle.as_ref(), key, 0, 1000).await
    }

    async fn run_command(&self, line: &str) -> Result<Value, String> {
        // Split on whitespace (quoted args not offered — redis args are rarely spaced; the MCP
        // redis_command tool remains the full-featured path). The adapter's own guard rejects
        // writes, KEYS, blocking and server-breaking commands before anything reaches the socket.
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return Err("type a command, e.g. GET mykey".into());
        }
        let command = parts[0];
        let args = &parts[1..];
        assert_command_allowed(
            command,
            args,
            self.readonly,
            self.allow_destructive,
            self.allow_eval,
        )?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(o: Value) -> Vec<String> {
        scan_args(&o).expect("valid params")
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
        let err = scan_args(&json!({"type": "sorted-set"})).expect_err("refused");
        assert!(err.contains("sorted-set"), "{err}");
        for t in SCAN_TYPES {
            assert!(err.contains(t), "{err} should name {t}");
        }
    }
}
