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

//! Postgres schema as MCP resources — port of `adapters/pg-resources.ts`: a flat listing (a
//! typical database is small), one JSON body per table with columns, primary key and indexes in
//! a single round trip.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::postgres::PgPool;

use super::direct::Lazy;
use super::pg::pg_query_rows;
use super::resources::{
    collapse_shards, describe_family, human_bytes, page, split_uri, Family, ResourceBody,
    ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate, TableFact,
};

const TABLES_SQL: &str = "
  SELECT n.nspname AS schema, c.relname AS name,
         -- reltuples is -1 before the first ANALYZE; that is \"unknown\", not \"minus one row\".
         CASE WHEN c.reltuples < 0 THEN NULL ELSE c.reltuples::bigint END AS rows_est,
         pg_total_relation_size(c.oid) AS bytes
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','v','m','p','f')
     AND n.nspname NOT IN ('pg_catalog','information_schema')";

/// Everything one table's resource says, in a single round trip.
///
/// Not `pg_describe_table`'s query: a resource is the compact card an AI client attaches as
/// context (byte sizes, no constraint DDL or incoming references), keyed by the schema in its
/// URI. Primary-key columns come out in table order; `indexes` carries the definitions, which is
/// where a composite key's real column order is visible.
const ONE_TABLE_SQL: &str = "
  SELECT n.nspname AS schema, c.relname AS name,
         CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview'
                        WHEN 'p' THEN 'partitioned table' WHEN 'f' THEN 'foreign table' END AS type,
         CASE WHEN c.reltuples < 0 THEN NULL ELSE c.reltuples::bigint END AS rows_est,
         pg_total_relation_size(c.oid) AS bytes,
         obj_description(c.oid, 'pg_class') AS comment,
         (SELECT json_agg(json_build_object(
                   'column', a.attname,
                   'type', format_type(a.atttypid, a.atttypmod),
                   'nullable', NOT a.attnotnull,
                   'default', pg_get_expr(d.adbin, d.adrelid)) ORDER BY a.attnum)
            FROM pg_attribute a
            LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
           WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped) AS columns,
         (SELECT json_agg(a.attname ORDER BY a.attnum)
            FROM pg_index i
            JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
           WHERE i.indrelid = c.oid AND i.indisprimary) AS primary_key,
         (SELECT json_agg(indexdef ORDER BY indexname)
            FROM pg_indexes WHERE schemaname = n.nspname AND tablename = c.relname) AS indexes
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','v','m','p','f')";

const SCHEMAS_SQL: &str = "
  SELECT n.nspname AS schema, count(*) AS tables, sum(pg_total_relation_size(c.oid)) AS bytes
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','v','m','p','f')
     AND n.nspname NOT IN ('pg_catalog','information_schema')
   GROUP BY 1 ORDER BY 3 DESC";

fn num(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().map(|i| i.max(0) as u64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

/// `public.channels` → `("public", "channels")`; a bare name defaults to `public`.
fn split_qualified(rest: &str) -> Result<(String, String), ResourceFault> {
    match rest.find('.') {
        None => Ok(("public".into(), rest.to_string())),
        Some(dot) => {
            let schema = &rest[..dot];
            let table = &rest[dot + 1..];
            if schema.is_empty() || table.is_empty() {
                return Err(ResourceFault(format!(
                    "not a schema-qualified table: {rest}"
                )));
            }
            Ok((schema.to_string(), table.to_string()))
        }
    }
}

pub struct PgResources {
    database: String,
    conn: Arc<Lazy<PgPool>>,
}

impl PgResources {
    pub fn new(database: String, conn: Arc<Lazy<PgPool>>) -> Self {
        Self { database, conn }
    }

    fn overview_uri(&self) -> String {
        format!("pg://{}", self.database)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.conn.get().await?;
        pg_query_rows(&pool, sql, params).await
    }

    async fn families(&self) -> Result<Vec<Family>, ResourceFault> {
        let rows = self.query(TABLES_SQL, &[]).await.map_err(ResourceFault)?;
        let facts: Vec<TableFact> = rows
            .iter()
            .map(|r| TableFact {
                name: format!(
                    "{}.{}",
                    r.get("schema").and_then(Value::as_str).unwrap_or(""),
                    r.get("name").and_then(Value::as_str).unwrap_or("")
                ),
                rows: num(r.get("rows_est")),
                bytes: num(r.get("bytes")),
            })
            .collect();
        Ok(collapse_shards(&facts, super::resources::SHARD_MIN))
    }

    async fn overview(&self) -> Result<String, ResourceFault> {
        let (fams, schemas) = tokio::join!(self.families(), self.query(SCHEMAS_SQL, &[]));
        let fams = fams?;
        let schemas = schemas.map_err(ResourceFault)?;
        let tables: u64 = fams.iter().map(|f| f.members.len() as u64).sum();
        let schemas_out: Vec<Value> = schemas
            .iter()
            .map(|s| {
                json!({
                    "schema": s.get("schema").and_then(Value::as_str).unwrap_or(""),
                    "tables": num(s.get("tables")),
                    "size": human_bytes(num(s.get("bytes"))),
                })
            })
            .collect();
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
            largest.push(Value::Object(m));
        }
        let overview = json!({
            "database": self.database,
            "tables": tables,
            "schemas": schemas_out,
            "largest": largest,
            "notes": [
                format!("Read pg://{}/<schema>.<table> for columns, primary key and indexes.", self.database),
                "Row estimates come from pg_class.reltuples and move with ANALYZE; the table list is live.",
            ],
        });
        serde_json::to_string_pretty(&overview).map_err(|e| ResourceFault(e.to_string()))
    }

    async fn table_body(&self, rest: &str) -> Result<String, ResourceFault> {
        let (schema, table) = split_qualified(rest)?;
        let rows = self
            .query(ONE_TABLE_SQL, &[json!(schema), json!(table)])
            .await
            .map_err(ResourceFault)?;
        let Some(r) = rows.first() else {
            return Err(ResourceFault(format!("no such table: {schema}.{table}")));
        };
        let mut out = Map::new();
        out.insert("database".into(), json!(self.database));
        out.insert(
            "schema".into(),
            json!(r.get("schema").cloned().unwrap_or(Value::Null)),
        );
        out.insert(
            "table".into(),
            json!(r.get("name").cloned().unwrap_or(Value::Null)),
        );
        out.insert(
            "type".into(),
            r.get("type").cloned().unwrap_or(json!("table")),
        );
        let approx = num(r.get("rows_est"));
        if approx > 0 {
            out.insert("approxRows".into(), json!(approx));
        }
        out.insert("size".into(), json!(human_bytes(num(r.get("bytes")))));
        if let Some(comment) = r.get("comment") {
            if !comment.is_null() {
                out.insert("comment".into(), comment.clone());
            }
        }
        out.insert(
            "primaryKey".into(),
            r.get("primary_key").cloned().unwrap_or_else(|| json!([])),
        );
        out.insert(
            "columns".into(),
            r.get("columns").cloned().unwrap_or_else(|| json!([])),
        );
        out.insert(
            "indexes".into(),
            r.get("indexes").cloned().unwrap_or_else(|| json!([])),
        );
        serde_json::to_string_pretty(&Value::Object(out)).map_err(|e| ResourceFault(e.to_string()))
    }
}

#[async_trait]
impl ResourceProvider for PgResources {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        let fams = self.families().await?;
        let (slice, next_cursor) = page(&fams, cursor)?;
        let mut resources: Vec<ResourceEntry> = Vec::new();
        if cursor.is_none() {
            resources.push(ResourceEntry {
                uri: self.overview_uri(),
                name: self.database.clone(),
                description: Some(format!(
                    "Schema overview: {} tables, live from the catalog",
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
                mime_type: Some("application/json".into()),
            });
        }
        Ok(ResourcePage {
            resources,
            next_cursor,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate {
            uri_template: format!("pg://{}/{{schema}}.{{table}}", self.database),
            name: format!("{} table schema", self.database),
            description: Some(
                "Columns, primary key, indexes and size of any table in this database.".into(),
            ),
            mime_type: Some("application/json".into()),
        }]
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let (authority, rest) = split_uri(uri, "pg")?;
        if authority != self.database {
            return Err(ResourceFault(format!(
                "this MCP is connected to {}, not {authority}",
                self.database
            )));
        }
        let text = match rest {
            None => self.overview().await?,
            Some(table) => self.table_body(&table).await?,
        };
        Ok(vec![ResourceBody {
            uri: uri.to_string(),
            mime_type: Some("application/json".into()),
            text,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_split() {
        assert_eq!(
            split_qualified("public.channels").unwrap(),
            ("public".into(), "channels".into())
        );
        assert_eq!(
            split_qualified("channels").unwrap(),
            ("public".into(), "channels".into())
        );
        assert!(split_qualified("public.").is_err());
        assert!(split_qualified(".channels").is_err());
    }
}
