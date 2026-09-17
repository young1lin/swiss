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

//! MySQL schema as MCP resources — port of `adapters/mysql-resources.ts`: one overview of the
//! database, one entry per logical table.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::mysql::MySqlPool;

use super::direct::Lazy;
use super::mysql::{num_or_zero, run_query};
use super::resources::{
    assert_ident, collapse_shards, describe_family, human_bytes, page, split_uri, Family,
    ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate,
    TableFact,
};

/// Every table in the current database with the two numbers that decide ordering.
///
/// `data_length + index_length` rather than `data_length`: a table whose bulk is its indexes is
/// still a big table. Both, plus `table_rows`, come from the statistics snapshot that
/// `information_schema_stats_expiry` governs (86400s by default), so they can lag a day — which
/// affects only the ordering, never whether a table is listed or what its DDL says.
const TABLES_SQL: &str = "
  SELECT table_name AS name, table_type AS type, table_rows AS rows_est,
         COALESCE(data_length, 0) + COALESCE(index_length, 0) AS bytes
    FROM information_schema.tables
   WHERE table_schema = ?";

const ONE_TABLE_SQL: &str = "
  SELECT table_type AS type, table_rows AS rows_est,
         COALESCE(data_length, 0) + COALESCE(index_length, 0) AS bytes
    FROM information_schema.tables
   WHERE table_schema = ? AND table_name = ?";

/// Siblings of a possible shard set, matched by prefix and filtered exactly in Rust.
const SIBLINGS_SQL: &str = "
  SELECT table_name AS name
    FROM information_schema.tables
   WHERE table_schema = ? AND table_name LIKE CONCAT(?, '%') ESCAPE '!'";

/// LIKE treats `_` and `%` as wildcards, and table names are full of underscores.
fn like_prefix(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '!' | '%' | '_') {
            out.push('!');
        }
        out.push(c);
    }
    out
}

/// Is `candidate` `name` or `name` + optional `_` + 1-8 digits — a shard of this table's set?
/// The Node build matched a regex built from the escaped name; this is the same test hand-rolled
/// (ADR-007 keeps the regex engine out).
fn is_shard_of(name: &str, candidate: &str) -> bool {
    let Some(rest) = candidate.strip_prefix(name) else {
        return false;
    };
    let rest = rest.strip_prefix('_').unwrap_or(rest);
    !rest.is_empty() && rest.len() <= 8 && rest.bytes().all(|b| b.is_ascii_digit())
}

pub struct MysqlResources {
    database: String,
    conn: Arc<Lazy<MySqlPool>>,
}

impl MysqlResources {
    /// The database was vetted by `assert_ident` when the engine was built (it is interpolated
    /// into SHOW CREATE TABLE below, which cannot bind an identifier).
    pub fn new(database: String, conn: Arc<Lazy<MySqlPool>>) -> Self {
        Self { database, conn }
    }

    fn overview_uri(&self) -> String {
        format!("mysql://{}", self.database)
    }
}

impl MysqlResources {
    async fn families(&self) -> Result<Vec<Family>, ResourceFault> {
        let rows = self
            .query(TABLES_SQL, vec![json!(self.database)])
            .await
            .map_err(ResourceFault)?;
        let facts: Vec<TableFact> = rows
            .iter()
            .map(|r| TableFact {
                name: r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                rows: num_or_zero(r.get("rows_est")),
                bytes: num_or_zero(r.get("bytes")),
            })
            .collect();
        Ok(collapse_shards(&facts, super::resources::SHARD_MIN))
    }

    async fn query(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.conn.get().await?;
        Ok(run_query(&pool, sql, &params).await?.rows)
    }

    async fn overview(&self) -> Result<String, ResourceFault> {
        let fams = self.families().await?;
        let shard_sets: Vec<&Family> = fams.iter().filter(|f| f.members.len() > 1).collect();
        let physical: u64 = fams.iter().map(|f| f.members.len() as u64).sum();

        let mut largest: Vec<Value> = Vec::new();
        for f in fams.iter().take(30) {
            let mut m = Map::new();
            m.insert("table".into(), json!(f.name));
            if f.rows > 0 {
                m.insert("rows".into(), json!(f.rows));
            }
            if f.bytes > 0 {
                m.insert("size".into(), json!(human_bytes(f.bytes)));
            }
            if f.members.len() > 1 {
                m.insert("shards".into(), json!(f.members.len()));
            }
            largest.push(Value::Object(m));
        }
        let mut collapsed: Vec<Value> = Vec::new();
        for f in &shard_sets {
            collapsed.push(json!({
                "table": f.name,
                "shards": f.members.len(),
                "range": format!("{} … {}", f.members.first().map(String::as_str).unwrap_or(""), f.members.last().map(String::as_str).unwrap_or("")),
            }));
        }
        let overview = json!({
            "database": self.database,
            "physicalTables": physical,
            "logicalTables": fams.len(),
            "shardSets": shard_sets.len(),
            "largest": largest,
            "collapsedShardSets": collapsed,
            "notes": [
                format!("Read mysql://{}/<table> for a table's CREATE statement.", self.database),
                "Row counts and sizes come from MySQL's statistics snapshot and can lag up to a day; \
                 the table list itself is read live from the data dictionary.",
                "Shard sets are collapsed into one entry — query any individual shard by its real name.",
            ],
        });
        serde_json::to_string_pretty(&overview).map_err(|e| ResourceFault(e.to_string()))
    }

    async fn table_body(&self, name: &str) -> Result<String, ResourceFault> {
        assert_ident(name, "table name")?;
        let database = &self.database;
        // SHOW CREATE TABLE names its column after the object kind (`Create Table` / `Create
        // View`); an older server or exotic permission can refuse it — the header still ships.
        let ddl = self
            .query(
                &format!("SHOW CREATE TABLE `{database}`.`{name}`"),
                Vec::new(),
            )
            .await
            .ok()
            .and_then(|rows| {
                rows.first().and_then(|r| {
                    r.iter()
                        .find(|(k, _)| k.to_ascii_lowercase().starts_with("create "))
                        .map(|(_, v)| v.as_str().unwrap_or("").to_string())
                })
            })
            .unwrap_or_default();

        let (meta, siblings) = tokio::join!(
            self.query(ONE_TABLE_SQL, vec![json!(database), json!(name)]),
            self.query(
                SIBLINGS_SQL,
                vec![json!(database), json!(like_prefix(name))]
            )
        );
        let meta = meta.map_err(ResourceFault)?;
        let Some(info) = meta.first() else {
            return Err(ResourceFault(format!("no such table: {database}.{name}")));
        };
        let siblings = siblings.map_err(ResourceFault)?;

        let rows_est = num_or_zero(info.get("rows_est"));
        let bytes = num_or_zero(info.get("bytes"));
        let mut shards_sorted: Vec<&str> = siblings
            .iter()
            .filter_map(|r| r.get("name").and_then(Value::as_str))
            .filter(|n| is_shard_of(name, n))
            .collect();
        shards_sorted.sort_unstable();

        let mut header: Vec<String> = vec![format!(
            "-- {database}.{name} ({})",
            info.get("type")
                .and_then(Value::as_str)
                .unwrap_or("TABLE")
                .to_ascii_lowercase()
        )];
        if rows_est > 0 || bytes > 0 {
            header.push(format!(
                "-- ~{rows_est} rows · {} (statistics snapshot, may lag a day)",
                human_bytes(bytes)
            ));
        }
        if !shards_sorted.is_empty() {
            header.push(format!(
                "-- sharded: {} sibling tables {} … {}, same columns; query one directly by name",
                shards_sorted.len(),
                shards_sorted.first().copied().unwrap_or(""),
                shards_sorted.last().copied().unwrap_or("")
            ));
        }

        if ddl.is_empty() {
            return Err(ResourceFault(format!(
                "no DDL available for {database}.{name}"
            )));
        }
        Ok(format!("{}\n{}", header.join("\n"), ddl))
    }
}

#[async_trait]
impl ResourceProvider for MysqlResources {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        let fams = self.families().await?;
        let (slice, next_cursor) = page(&fams, cursor)?;
        let mut resources: Vec<ResourceEntry> = Vec::new();
        // The overview belongs on the first page: it is the map for everything after it.
        if cursor.is_none() {
            resources.push(ResourceEntry {
                uri: self.overview_uri(),
                name: self.database.clone(),
                description: Some(format!(
                    "Schema overview: {} logical tables, live from the data dictionary",
                    fams.len()
                )),
                mime_type: Some("application/json".into()),
            });
        }
        for f in slice {
            let described = describe_family(f);
            resources.push(ResourceEntry {
                uri: format!("{}/{}", self.overview_uri(), f.name),
                name: f.name.clone(),
                description: (!described.is_empty()).then_some(described),
                mime_type: Some("text/plain".into()),
            });
        }
        Ok(ResourcePage {
            resources,
            next_cursor,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate {
            uri_template: format!("mysql://{}/{{table}}", self.database),
            name: format!("{} table DDL", self.database),
            description: Some(
                "CREATE statement of any table in this database, including an individual shard of a \
                 collapsed shard set."
                    .into(),
            ),
            mime_type: Some("text/plain".into()),
        }]
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let (authority, rest) = split_uri(uri, "mysql")?;
        if authority != self.database {
            return Err(ResourceFault(format!(
                "this MCP is connected to {}, not {authority}",
                self.database
            )));
        }
        match rest {
            None => Ok(vec![ResourceBody {
                uri: uri.to_string(),
                mime_type: Some("application/json".into()),
                text: self.overview().await?,
            }]),
            Some(table) => Ok(vec![ResourceBody {
                uri: uri.to_string(),
                mime_type: Some("text/plain".into()),
                text: self.table_body(&table).await?,
            }]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_matching() {
        assert!(is_shard_of("events", "events_1013"));
        assert!(is_shard_of("events", "events1013"));
        // The base table is NOT its own shard — the Node regex required 1-8 digits.
        assert!(!is_shard_of("events", "events"));
        assert!(!is_shard_of("events", "events_x"));
        assert!(!is_shard_of("events", "other_events_1"));
        assert!(!is_shard_of("events", "events_123456789")); // more than 8 digits
    }

    #[test]
    fn like_prefixes_escape_wildcards() {
        assert_eq!(like_prefix("events_2026"), "events!_2026");
        assert_eq!(like_prefix("a!b"), "a!!b");
    }
}
