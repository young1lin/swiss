//! Redis Data-view browser — the port of Node's `redisBrowser()`: page keys by SCAN, read one
//! key type-aware, run one console command under the adapter's own guard.

use super::direct::Lazy;
use super::redis::{
    assert_command_allowed, type_aware_read, value_i64, value_string, RedisHandle, SCAN_TYPES,
};
use crate::dbbrowser::RedisBrowser;
use async_trait::async_trait;
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
        let handle = self.conn.get().await?;
        let cursor = o
            .get("cursor")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_string();
        let count = match o.get("count").and_then(Value::as_i64) {
            Some(n) => n.clamp(1, 1000),
            None => 200,
        };
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
        let mut scan_args: Vec<String> = vec![
            cursor.clone(),
            "MATCH".into(),
            pattern,
            "COUNT".into(),
            count.to_string(),
        ];
        if !type_.is_empty() {
            scan_args.push("TYPE".into());
            scan_args.push(type_);
        }
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
        assert_command_allowed(command, args, self.readonly, self.allow_destructive, self.allow_eval)?;
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
