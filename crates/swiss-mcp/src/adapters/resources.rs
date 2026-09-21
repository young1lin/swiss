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

//! MCP resources for the DB adapters — port of `adapters/resources.ts`: the schema a client
//! attaches with `@`, instead of spending a tool call on it.
//!
//! Two decisions shape everything here.
//!
//! **Nothing is cached, anywhere.** A listing is one live catalog query — fast enough to run
//! through an SSH tunnel on every attach — and a live database keeps gaining tables. A TTL would
//! be wrong more often than useful.
//!
//! **The field set is deliberately narrow.** Claude Code negotiates protocol 2024-11-05 over
//! HTTP, where `title`, `icons`, `ttlMs` and `cacheScope` do not exist — and strict parsing of an
//! unknown key rejects the whole reply. Everything worth saying goes into `description`, which
//! every revision has and every client shows.

use serde_json::Value;

/// One listed resource. Fields limited to those present in protocol 2024-11-05.
#[derive(Debug, Clone)]
pub struct ResourceEntry {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// One block of resource content, as returned by resources/read.
#[derive(Debug, Clone)]
pub struct ResourceBody {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: String,
}

/// A parameterized resource, for the tables too numerous to list individually.
#[derive(Debug, Clone)]
pub struct ResourceTemplate {
    pub uri_template: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// A page of resources plus the cursor of the next one.
pub struct ResourcePage {
    pub resources: Vec<ResourceEntry>,
    pub next_cursor: Option<String>,
}

/// What an adapter implements to expose resources.
#[async_trait::async_trait]
pub trait ResourceProvider: Send + Sync {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault>;
    fn templates(&self) -> Vec<ResourceTemplate>;
    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault>;
}

/// A URI that names nothing, or a cursor that means nothing.
///
/// Carried as its own type so the server layer can answer `-32602` as the spec requires, rather
/// than the `-32603` a bare error would become. The spec also forbids expressing "no such
/// resource" as an empty `contents` array, since that is ambiguous with "exists but is empty".
#[derive(Debug, Clone)]
pub struct ResourceFault(pub String);

impl std::fmt::Display for ResourceFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Entries per page of resources/list.
pub const PAGE_SIZE: usize = 200;

/// Cut one page out of a list, cursor being the decimal offset of the next one.
///
/// The offset is safe as a cursor precisely because nothing is cached: each page is a fresh
/// query with a stable ORDER BY, so a table added between two pages shifts the tail rather than
/// corrupting it.
pub fn page<'a, T>(
    items: &'a [T],
    cursor: Option<&str>,
) -> Result<(Vec<&'a T>, Option<String>), ResourceFault> {
    let mut start = 0usize;
    if let Some(cursor) = cursor {
        if !cursor.is_empty() {
            if cursor.is_empty() || !cursor.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ResourceFault(format!("invalid cursor: {cursor}")));
            }
            start = cursor
                .parse::<usize>()
                .map_err(|_| ResourceFault(format!("invalid cursor: {cursor}")))?;
            if start > items.len() {
                return Err(ResourceFault(format!(
                    "cursor is past the end of the list: {cursor}"
                )));
            }
        }
    }
    let end = (start + PAGE_SIZE).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    Ok((items[start..end].iter().collect(), next))
}

/// A table as the catalog reports it, before families are collapsed.
#[derive(Debug, Clone)]
pub struct TableFact {
    pub name: String,
    /// Approximate rows; 0 when the catalog has no estimate.
    pub rows: u64,
    /// On-disk bytes; 0 when unknown.
    pub bytes: u64,
}

/// A logical table: either one physical table, or a shard set collapsed into a single entry.
#[derive(Debug, Clone)]
pub struct Family {
    /// Display name — the base table when one exists, else `prefix_*`.
    pub name: String,
    /// The physical table to read DDL from.
    pub representative: String,
    pub members: Vec<String>,
    pub rows: u64,
    pub bytes: u64,
}

/// How many same-prefix tables it takes to be called a shard set.
///
/// A busy database can hold a thousand `events_*` tables that are a single logical events table,
/// and listing them individually makes every other table unfindable. But a numeric suffix is not
/// proof of sharding — `daily_1 … _4` are ordinary distinct tables. Eight is comfortably above
/// the incidental cases and far below any real shard set.
pub const SHARD_MIN: usize = 8;

/// Strip a trailing shard suffix: `events_1013`, `stats_202601` → `events`, `stats`.
fn family_key(name: &str) -> String {
    // `_?\d{1,8}$` — an optional underscore then 1-8 digits at the very end.
    let trimmed = name.as_bytes();
    let mut end = trimmed.len();
    let mut digits = 0usize;
    while end > 0 && trimmed[end - 1].is_ascii_digit() && digits < 8 {
        end -= 1;
        digits += 1;
    }
    if digits == 0 {
        return name.to_string();
    }
    if end > 0 && trimmed[end - 1] == b'_' {
        end -= 1;
    }
    let key = &name[..end];
    // A name that is only digits (or only a suffix) has no prefix to group by — leave it alone.
    if key.is_empty() {
        name.to_string()
    } else {
        key.to_string()
    }
}

/// `3.2 GB`, `12.4 MB`, `812 KB` — for a description a human skims in a picker.
pub fn human_bytes(n: u64) -> String {
    if n == 0 {
        return "0 B".into();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0usize;
    let mut v = n as f64;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 || i == 0 {
        format!("{} {}", v.round() as u64, UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// `12,345,678` — Intl.NumberFormat("en-US") for the row-count line.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Collapse shard sets, then order by size so the tables that matter come first — port of
/// `collapseShards`. Ordering is by bytes descending: in a picker of hundreds of entries, the
/// million-row base table has to appear before a 1,000-row shard of it.
pub fn collapse_shards(tables: &[TableFact], min_members: usize) -> Vec<Family> {
    let mut groups: std::collections::HashMap<String, Vec<&TableFact>> =
        std::collections::HashMap::new();
    let mut key_order: Vec<String> = Vec::new();
    for t in tables {
        let key = family_key(&t.name);
        if !groups.contains_key(&key) {
            key_order.push(key.clone());
        }
        groups.entry(key).or_default().push(t);
    }

    let mut out: Vec<Family> = Vec::new();
    for key in key_order {
        let members = &groups[&key];
        if members.len() < min_members {
            // Not a shard set: every table stands on its own.
            for t in members {
                out.push(Family {
                    name: t.name.clone(),
                    representative: t.name.clone(),
                    members: vec![t.name.clone()],
                    rows: t.rows,
                    bytes: t.bytes,
                });
            }
            continue;
        }
        let mut names: Vec<&String> = members.iter().map(|t| &t.name).collect();
        names.sort();
        // The unsuffixed table is the one to read DDL from; without it, any shard will do.
        let base = names.iter().find(|n| ***n == key);
        let name = base
            .map(|n| (*n).clone())
            .unwrap_or_else(|| format!("{key}_*"));
        let representative = base
            .map(|n| (*n).clone())
            .unwrap_or_else(|| names[0].clone());
        out.push(Family {
            name,
            representative,
            members: names.into_iter().cloned().collect(),
            rows: members.iter().map(|t| t.rows).sum(),
            bytes: members.iter().map(|t| t.bytes).sum(),
        });
    }

    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

/// `~12,345,678 rows · 4.0 GB · 1015 shards (events_0 … events_1013)`
pub fn describe_family(f: &Family) -> String {
    let mut parts: Vec<String> = Vec::new();
    if f.rows > 0 {
        parts.push(format!("~{} rows", group_digits(f.rows)));
    }
    if f.bytes > 0 {
        parts.push(human_bytes(f.bytes));
    }
    if f.members.len() > 1 {
        let shards: Vec<&String> = f.members.iter().filter(|n| **n != f.name).collect();
        parts.push(format!(
            "{} shards ({} … {})",
            f.members.len(),
            shards.first().map(|s| s.as_str()).unwrap_or(""),
            shards.last().map(|s| s.as_str()).unwrap_or("")
        ));
    }
    parts.join(" · ")
}

/// Identifiers reach the catalog as bound parameters, but SHOW CREATE TABLE cannot bind one —
/// `[A-Za-z0-9_$]{1,64}` vetted by hand. The check itself lives in swiss_host::dbbrowser (the
/// browser model vets the same identifiers); this wrapper keeps the adapter fault type.
pub fn assert_ident(name: &str, what: &str) -> Result<(), ResourceFault> {
    swiss_host::dbbrowser::assert_ident(name, what).map_err(ResourceFault)
}

/// Split `scheme://<authority>/<rest>` without URL parsing — `new URL()` lowercases the
/// authority, and on Linux MySQL database names are case-sensitive: a resource URI must
/// round-trip exactly as listed.
pub fn split_uri(uri: &str, scheme: &str) -> Result<(String, Option<String>), ResourceFault> {
    let prefix = format!("{scheme}://");
    let rest = uri
        .strip_prefix(&prefix)
        .ok_or_else(|| ResourceFault(format!("not a {scheme} resource URI: {uri}")))?;
    // authority runs to / ? or #; only / introduces a rest segment in these URIs.
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    if authority.is_empty() {
        return Err(ResourceFault(format!("not a {scheme} resource URI: {uri}")));
    }
    if let Some(tail) = tail {
        let seg = tail
            .strip_prefix('/')
            .ok_or_else(|| ResourceFault(format!("not a {scheme} resource URI: {uri}")))?;
        let seg = seg.split(['?', '#']).next().unwrap_or("");
        let decoded = percent_decode(seg);
        let rest = if decoded.is_empty() {
            None
        } else {
            Some(decoded)
        };
        return Ok((percent_decode(authority), rest));
    }
    Ok((percent_decode(authority), None))
}

/// Percent-decode without a URL crate (`decodeURIComponent`).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One row→Value helper shared by the adapters' resource queries: NULLs become absent keys.
pub fn row_object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        if !v.is_null() {
            map.insert(k.to_string(), v);
        }
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(name: &str, rows: u64, bytes: u64) -> TableFact {
        TableFact {
            name: name.into(),
            rows,
            bytes,
        }
    }

    #[test]
    fn collapses_shard_sets_only() {
        let mut tables: Vec<TableFact> = (0..10)
            .map(|i| fact(&format!("events_{i}"), 1000, 2000))
            .collect();
        tables.push(fact("users", 5, 500));
        tables.push(fact("daily_1", 1, 1));
        tables.push(fact("daily_2", 1, 1));
        let fams = collapse_shards(&tables, SHARD_MIN);
        // 10 events_* collapse into one; users and the two dailies stand alone.
        assert_eq!(fams.len(), 4);
        let events = fams.iter().find(|f| f.name.starts_with("events")).unwrap();
        assert_eq!(events.members.len(), 10);
        assert_eq!(events.rows, 10_000);
        // The family with the most bytes sorts first.
        assert_eq!(fams[0].name, "events_*");
    }

    #[test]
    fn family_keys_strip_suffixes() {
        assert_eq!(family_key("events_1013"), "events");
        assert_eq!(family_key("stats_202601"), "stats");
        assert_eq!(family_key("users"), "users");
        assert_eq!(family_key("12345"), "12345"); // digits-only: no prefix to group by
    }

    #[test]
    fn human_and_grouped_numbers() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(812 * 1024), "812 KB");
        assert_eq!(human_bytes(4 * 1024 * 1024 * 1024), "4.0 GB");
        assert_eq!(group_digits(12_345_678), "12,345,678");
    }

    #[test]
    fn uris_split_exactly() {
        assert_eq!(
            split_uri("mysql://myDb/users", "mysql").unwrap(),
            ("myDb".into(), Some("users".into()))
        );
        assert_eq!(
            split_uri("mysql://myDb", "mysql").unwrap(),
            ("myDb".into(), None)
        );
        assert!(split_uri("mysql://", "mysql").is_err());
        assert!(split_uri("redis://x", "mysql").is_err());
    }

    #[test]
    fn paging_by_offset_cursor() {
        let items: Vec<u32> = (0..500).collect();
        let (slice, next) = page(&items, None).unwrap();
        assert_eq!(slice.len(), PAGE_SIZE);
        assert_eq!(next.as_deref(), Some("200"));
        let (slice2, next2) = page(&items, Some("200")).unwrap();
        assert_eq!(slice2.len(), 200);
        assert_eq!(next2.as_deref(), Some("400"));
        let (slice3, next3) = page(&items, Some("400")).unwrap();
        assert_eq!(slice3.len(), 100);
        assert!(next3.is_none());
        assert!(page(&items, Some("9999")).is_err());
        assert!(page(&items, Some("x")).is_err());
    }

    #[test]
    fn identifiers_vetted() {
        assert!(assert_ident("users", "table name").is_ok());
        assert!(assert_ident("a$b_1", "table name").is_ok());
        assert!(assert_ident("", "table name").is_err());
        assert!(assert_ident("drop table; --", "table name").is_err());
        assert!(assert_ident(&"x".repeat(65), "table name").is_err());
    }
}
