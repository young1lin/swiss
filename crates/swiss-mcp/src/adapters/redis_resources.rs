/*
 * Copyright 2026 The swiss authors
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

//! Redis keyspace overview as one MCP resource — port of `adapters/redis-resources.ts`. No
//! per-key resources and no key template, deliberately: a resource list is something a person
//! reads in a picker, and neither hundreds of thousands of entries nor a template over opaque
//! session ids is readable; key discovery stays with `redis_scan`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use super::direct::Lazy;
use super::redis::{value_string, RedisHandle};
use super::resources::{
    split_uri, ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider,
    ResourceTemplate,
};

/// Sampling bounds. A busy instance can hold hundreds of thousands of keys, so "describe this
/// database" must never mean "enumerate it": the sample is capped in both keys and round trips,
/// and the round-trip cap is what guarantees termination when SCAN's cursor keeps coming back
/// non-zero.
const MAX_KEYS: usize = 2000;
const MAX_ROUNDS: usize = 20;
const SCAN_COUNT: i64 = 500;
/// Keys whose type is probed.
const TYPE_SAMPLE: usize = 30;

/// Collapse a key into its shape, so a huge keyspace of opaque keys becomes a handful of
/// legible buckets.
///
/// `app:session:4711:token` → `app:session:#:*`
/// `job1760000000000`       → `job#`
///
/// Digit runs become `#` because the variable part of a key is usually numeric — a timestamp, a
/// snowflake id, a shard number — and without folding them each key is its own bucket.
pub fn key_shape(key: &str) -> String {
    let parts: Vec<&str> = key.split(':').collect();
    let head = parts.iter().take(3).cloned().collect::<Vec<_>>().join(":");
    // `\d+` → `#`, hand-rolled (ADR-007: no regex engine).
    let mut shape = String::with_capacity(head.len());
    let mut in_digits = false;
    for c in head.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                shape.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            shape.push(c);
        }
    }
    if parts.len() > 3 {
        format!("{shape}:*")
    } else {
        shape
    }
}

/// Pull `redis_version:6.2.20` style fields out of an INFO reply (`^key:(.*)$` multiline).
fn info_field(info: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    info.lines()
        .find_map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .strip_prefix(&prefix)
                .map(|rest| rest.trim().to_string())
        })
        .filter(|v| !v.is_empty())
}

pub struct RedisResources {
    label: String,
    conn: Arc<Lazy<RedisHandle>>,
}

impl RedisResources {
    pub fn new(label: String, conn: Arc<Lazy<RedisHandle>>) -> Self {
        Self { label, conn }
    }

    async fn sample(&self) -> Result<(Vec<String>, usize, bool), String> {
        let client = self.conn.get().await?;
        let mut keys: Vec<String> = Vec::new();
        let mut cursor = "0".to_string();
        let mut rounds = 0usize;
        loop {
            let reply = RedisHandle::call(
                &client,
                "SCAN",
                &[&cursor, "MATCH", "*", "COUNT", &SCAN_COUNT.to_string()],
            )
            .await?;
            let (next, found) = match &reply {
                Value::Array(pair) if pair.len() >= 2 => (
                    value_string(&pair[0]),
                    match &pair[1] {
                        Value::Array(items) => items.iter().map(value_string).collect::<Vec<_>>(),
                        _ => Vec::new(),
                    },
                ),
                _ => ("0".into(), Vec::new()),
            };
            keys.extend(found);
            cursor = next;
            rounds += 1;
            if cursor == "0" || rounds >= MAX_ROUNDS || keys.len() >= MAX_KEYS {
                break;
            }
        }
        keys.truncate(MAX_KEYS);
        let complete = cursor == "0";
        Ok((keys, rounds, complete))
    }

    async fn overview(&self) -> Result<String, ResourceFault> {
        let client = self.conn.get().await.map_err(ResourceFault)?;
        // DBSIZE and INFO failures degrade to their defaults (0 / "") — the overview is still
        // worth serving against a degraded server.
        let dbsize = RedisHandle::call(&client, "DBSIZE", &[])
            .await
            .ok()
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let info = RedisHandle::call(&client, "INFO", &["server"])
            .await
            .ok()
            .map(|v| value_string(&v))
            .unwrap_or_default();
        let (keys, rounds, complete) = self.sample().await.map_err(ResourceFault)?;

        let mut shapes: HashMap<String, u64> = HashMap::new();
        for k in &keys {
            *shapes.entry(key_shape(k)).or_insert(0) += 1;
        }
        let mut ranked: Vec<(String, u64)> = shapes.into_iter().collect();
        ranked.sort_by_key(|a| std::cmp::Reverse(a.1));
        ranked.truncate(20);

        let mut types: Map<String, Value> = Map::new();
        for k in keys.iter().take(TYPE_SAMPLE) {
            let t = RedisHandle::call(&client, "TYPE", &[k])
                .await
                .map(|v| value_string(&v))
                .unwrap_or_else(|_| "unknown".into());
            // Insertion order = first-seen order, matching Object.fromEntries in the Node build.
            let next = match types.get(&t) {
                Some(Value::Number(n)) => {
                    n.as_i64().map(|i| json!(i + 1)).unwrap_or_else(|| json!(1))
                }
                _ => json!(1),
            };
            types.insert(t, next);
        }

        let scale = if !keys.is_empty() && dbsize > 0 {
            dbsize as f64 / keys.len() as f64
        } else {
            1.0
        };
        let key_shapes: Vec<Value> = ranked
            .iter()
            .map(|(shape, n)| {
                json!({
                    "shape": shape,
                    "sampled": n,
                    "estimatedKeys": if complete { json!(n) } else { json!((*n as f64 * scale).round() as u64) },
                })
            })
            .collect();
        let types_out = Value::Object(types);
        let notes: Vec<String> = vec![
            "Digit runs in a key shape are folded to `#`; a trailing `:*` means the key has more segments.".into(),
            if complete {
                "The sample covered the whole keyspace, so these counts are exact.".into()
            } else {
                format!(
                    "Sampled {} of {dbsize} keys over {rounds} SCAN rounds; estimatedKeys is extrapolated and approximate.",
                    keys.len()
                )
            },
            "Use redis_scan with a pattern to list real keys — they are not exposed as resources.".into(),
        ];
        let mut out = Map::new();
        out.insert("redis".into(), json!(self.label));
        if let Some(version) = info_field(&info, "redis_version") {
            out.insert("version".into(), json!(version));
        }
        out.insert("keys".into(), json!(dbsize));
        out.insert("sampled".into(), json!(keys.len()));
        out.insert("sampleIsWholeKeyspace".into(), json!(complete));
        out.insert("keyShapes".into(), Value::Array(key_shapes));
        out.insert("types".into(), types_out);
        out.insert("notes".into(), json!(notes));
        serde_json::to_string_pretty(&Value::Object(out)).map_err(|e| ResourceFault(e.to_string()))
    }
}

#[async_trait]
impl ResourceProvider for RedisResources {
    async fn list(&self, _cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        Ok(ResourcePage {
            resources: vec![ResourceEntry {
                uri: format!("redis://{}/overview", self.label),
                name: format!("{} overview", self.label),
                description: Some(
                    "Key count, key-shape histogram and value types, sampled live".into(),
                ),
                mime_type: Some("application/json".into()),
            }],
            next_cursor: None,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let (authority, rest) = split_uri(uri, "redis")?;
        if authority != self.label || rest.as_deref() != Some("overview") {
            return Err(ResourceFault(format!("no such resource: {uri}")));
        }
        let text = self.overview().await?;
        Ok(vec![ResourceBody {
            uri: format!("redis://{}/overview", self.label),
            mime_type: Some("application/json".into()),
            text,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shapes_fold_digits() {
        assert_eq!(key_shape("app:session:4711:token"), "app:session:#:*");
        assert_eq!(key_shape("job1760000000000"), "job#");
        assert_eq!(key_shape("a:b:c"), "a:b:c");
        assert_eq!(key_shape("plain"), "plain");
    }

    #[test]
    fn info_fields_parse() {
        let info = "# Server\r\nredis_version:6.2.20\r\nredis_mode:standalone\r\n";
        assert_eq!(info_field(info, "redis_version").as_deref(), Some("6.2.20"));
        assert_eq!(info_field(info, "missing"), None);
    }
}
