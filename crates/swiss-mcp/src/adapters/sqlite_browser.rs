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

//! One module's own SQLite file as a Data connection (SPEC §data.sqlite).
//!
//! The file belongs to whichever plugin opened it. This browser never writes it:
//! the file is opened `SQLITE_OPEN_READONLY`, every connection sets `query_only`,
//! and a statement that is not a read is refused before it reaches the file.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{AssertSqlSafe, Column, Executor, Row};

use swiss_host::dbbrowser::{
    browse_count_sql, browse_offset, browse_order, browse_page_size, browse_rows_sql,
    browse_table_sort, bytea_hex, completion_items, exact_int64, finite_f64, grep_where,
    like_contains, page_and_next, quote_ident, sql_word_ending_at, to_browse_indexes,
    BrowseColumn, DbBrowser, DbDialect, EditError, SqlDump, TableSortKey, WriteRefusal,
};

/// The one database SQLite has here. Schema parameters other than this are refused.
const SCHEMA: &str = "main";

pub struct SqliteBrowser {
    path: PathBuf,
    /// The picker label (`logs/data.db`, `memory/store.sqlite3`). The home stays out.
    label: String,
    pool: tokio::sync::Mutex<Option<SqlitePool>>,
}

impl SqliteBrowser {
    pub fn new(path: PathBuf, label: impl Into<String>) -> Self {
        Self {
            path,
            label: label.into(),
            pool: tokio::sync::Mutex::new(None),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn refusal(&self) -> String {
        format!(
            "this connection is query-only ({}): a write would change this module's own database",
            self.label
        )
    }

    async fn pool(&self) -> Result<SqlitePool, String> {
        let mut slot = self.pool.lock().await;
        if let Some(pool) = slot.clone() {
            return Ok(pool);
        }
        if !self.path.is_file() {
            return Err(format!("this module database is not there yet ({})", self.label));
        }
        // Read-only at open, and query_only on top: a statement the gate misread
        // still cannot write. No journal_mode pragma — that would try to change
        // the memory plugin's file. busy_timeout waits out a short writer lock.
        let opts = SqliteConnectOptions::new()
            .filename(&self.path)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .min_connections(0)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    conn.execute(sqlx::query("PRAGMA query_only = ON")).await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await
            .map_err(|e| format!("could not open {}: {e}", self.label))?;
        *slot = Some(pool.clone());
        Ok(pool)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.pool().await?;
        let rows = if params.is_empty() {
            sqlx::query(AssertSqlSafe(sql)).fetch_all(&pool).await
        } else {
            let mut q = sqlx::query(AssertSqlSafe(sql));
            for p in params {
                q = bind_value(q, p);
            }
            q.fetch_all(&pool).await
        }
        .map_err(|e| e.to_string())?;
        Ok(rows.iter().map(row_map).collect())
    }

    fn schema_of(o: &Value) -> Result<(), String> {
        match o.get("schema").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            None | Some(SCHEMA) => Ok(()),
            Some(other) => Err(format!("sqlite has one database ({SCHEMA}), not {other}")),
        }
    }

    async fn columns(&self, table: &str) -> Result<(Vec<BrowseColumn>, Vec<String>), String> {
        quote_ident(DbDialect::Sqlite, table)?;
        let lit = sql_lit(table);
        // SELECT *: naming `notnull` in the statement is parsed as NOT NULL.
        let rows = self
            .query(
                &format!("SELECT * FROM pragma_table_info({lit}) ORDER BY cid"),
                &[],
            )
            .await?;
        let mut pk: Vec<(i64, String)> = Vec::new();
        let mut columns = Vec::with_capacity(rows.len());
        for r in rows {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let pk_ord = r.get("pk").and_then(Value::as_i64).unwrap_or(0);
            if pk_ord > 0 {
                pk.push((pk_ord, name.clone()));
            }
            let data_type = r
                .get("type")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .unwrap_or("ANY")
                .to_string();
            let notnull = r.get("notnull").and_then(Value::as_i64).unwrap_or(0) != 0;
            let default_value = r.get("dflt_value").and_then(Value::as_str).map(str::to_string);
            columns.push(BrowseColumn {
                name,
                data_type,
                nullable: !notnull,
                is_primary_key: pk_ord > 0,
                default_value,
                comment: None,
            });
        }
        pk.sort_by_key(|p| p.0);
        let primary = pk.into_iter().map(|p| p.1).collect();
        Ok((columns, primary))
    }
}

fn sql_lit(name: &str) -> String {
    format!("'{}'", name.replace('\'', "''"))
}

/// The shared read gate, plus the one word it treats as a read that writes here.
fn assert_sqlite_read(sql: &str, refusal: &str) -> Result<String, String> {
    let sql = crate::adapters::sql::assert_read_statement(sql)
        .map_err(|reason| format!("{refusal} ({reason})"))?;
    let word = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('(')
        .to_ascii_lowercase();
    // ANALYZE rewrites sqlite_stat1. The shared gate allows it because ANALYZE
    // is a read on MySQL; on this file it is a write.
    if word == "analyze" || word == "analyse" {
        return Err(format!("{refusal} (ANALYZE writes sqlite_stat1)"));
    }
    Ok(sql)
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    value: &Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    match value {
        Value::String(s) => query.bind(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else if let Some(u) = n.as_u64() {
                query.bind(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                query.bind(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::Bool(b) => query.bind(*b),
        Value::Null => query.bind(Option::<String>::None),
        other => query.bind(other.to_string()),
    }
}

fn row_map(row: &SqliteRow) -> Map<String, Value> {
    let mut out = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        out.insert(col.name().to_string(), cell(row, i));
    }
    out
}

fn cell(row: &SqliteRow, i: usize) -> Value {
    // sqlx reports the declared type. A table column has one. `pragma_*` and
    // expressions such as `COUNT(*)` or `SELECT 1` declare none, so this string
    // is "NULL" while the value still has a storage class.
    match row.column(i).type_info().to_string().as_str() {
        "INTEGER" => integer_cell(row, i),
        "REAL" => real_cell(row, i),
        "BLOB" => blob_cell(row, i),
        "TEXT" => text_cell(row, i),
        _ => untyped_cell(row, i),
    }
}

fn js_int(v: i64) -> Value {
    // SPEC §data.browse: past the JS safe integer the wire is a string.
    if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&v) {
        exact_int64(v)
    } else {
        json!(v)
    }
}

fn integer_cell(row: &SqliteRow, i: usize) -> Value {
    match row.try_get::<Option<i64>, _>(i) {
        Ok(Some(v)) => js_int(v),
        _ => Value::Null,
    }
}

fn real_cell(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<f64>, _>(i)
        .ok()
        .flatten()
        .map(finite_f64)
        .unwrap_or(Value::Null)
}

fn blob_cell(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<Vec<u8>>, _>(i)
        .ok()
        .flatten()
        .map(|b| Value::String(bytea_hex(&b)))
        .unwrap_or(Value::Null)
}

fn text_cell(row: &SqliteRow, i: usize) -> Value {
    row.try_get::<Option<String>, _>(i)
        .ok()
        .flatten()
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn untyped_cell(row: &SqliteRow, i: usize) -> Value {
    // A SQL NULL fails each of these. A wrong storage class fails that one and
    // the next is tried. `try_get` checks the value's storage class, not the
    // declared type that was missing above.
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return js_int(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return finite_f64(v);
    }
    if let Ok(v) = row.try_get::<String, _>(i) {
        return Value::String(v);
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return Value::String(bytea_hex(&v));
    }
    Value::Null
}

fn i64_cell(row: &Map<String, Value>, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[async_trait]
impl DbBrowser for SqliteBrowser {
    fn dialect(&self) -> DbDialect {
        DbDialect::Sqlite
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    async fn list_tables(&self, o: &Value) -> Result<Value, String> {
        Self::schema_of(o)?;
        let page = o
            .get("page")
            .and_then(swiss_host::dbbrowser::js_number)
            .map(f64::floor)
            .unwrap_or(0.0)
            .max(0.0) as i64;
        let limit = swiss_host::dbbrowser::clamp_browse_limit(o.get("limit"), 200, 5000);
        let grep = o.get("grep").and_then(Value::as_str);
        let sort = browse_table_sort(
            o.get("sort").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
        )?;
        let grammar = grep
            .map(|g| grep_where(DbDialect::Sqlite, "name", g, 0))
            .transpose()?
            .flatten();
        let (extra, params) = if let Some(w) = grammar {
            (w.frag, w.params)
        } else if let Some(g) = grep.filter(|g| !g.is_empty()) {
            (
                " AND name LIKE ? ESCAPE '!'".to_string(),
                vec![json!(like_contains(g))],
            )
        } else {
            (String::new(), Vec::new())
        };
        let order = if sort.desc { "DESC" } else { "ASC" };
        // The catalog of one file. Name order is the statement's; a rows sort
        // counts each of those names and reorders, because the file has no
        // statistics table to ORDER BY. Size is unknown, so it ties and the
        // name breaks the tie.
        let listed = self
            .query(
                &format!(
                    "SELECT name, type FROM sqlite_master \
                     WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                     {extra} ORDER BY name COLLATE NOCASE {order} LIMIT 5000"
                ),
                &params,
            )
            .await?;
        let mut tables: Vec<(String, String, Option<i64>)> = listed
            .into_iter()
            .filter_map(|r| {
                let name = r.get("name").and_then(Value::as_str)?.to_string();
                if quote_ident(DbDialect::Sqlite, &name).is_err() {
                    return None;
                }
                let type_ = r.get("type").and_then(Value::as_str).unwrap_or("table").to_string();
                Some((name, type_, None))
            })
            .collect();
        if sort.key == TableSortKey::Rows {
            for row in &mut tables {
                let q = quote_ident(DbDialect::Sqlite, &row.0)?;
                let counted = self
                    .query(&format!("SELECT COUNT(*) AS total FROM {q}"), &[])
                    .await?;
                row.2 = counted.first().map(|r| i64_cell(r, "total"));
            }
            tables.sort_by(|a, b| {
                let ord = a.2.unwrap_or(0).cmp(&b.2.unwrap_or(0)).then_with(|| a.0.cmp(&b.0));
                if sort.desc { ord.reverse() } else { ord }
            });
        }
        let total = tables.len() as u64;
        // A page past the catalog is empty. Clamping only `end` leaves `start` past
        // `len`, and the slice panics — release aborts the whole process.
        let start = (page.max(0) as usize).saturating_mul(limit as usize).min(tables.len());
        let end = (start + limit as usize).min(tables.len());
        let page_rows: Vec<Value> = tables[start..end]
            .iter()
            .map(|(name, type_, rows)| {
                json!({
                    "schema": SCHEMA,
                    "name": name,
                    "type": type_,
                    "approxRows": rows,
                    "size": "",
                })
            })
            .collect();
        let more = (end as u64) < total;
        Ok(json!({
            "tables": page_rows,
            "total": total,
            "page": page,
            "limit": limit,
            "more": more,
        }))
    }

    async fn read_table(&self, o: &Value) -> Result<Value, String> {
        Self::schema_of(o)?;
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.columns(table).await?;
        if columns.is_empty() {
            return Err(format!("not a table on this connection: {table}"));
        }
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let order = browse_order(
            &names,
            o.get("order").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
            DbDialect::Sqlite,
        )?;
        let offset = browse_offset(o.get("offset"));
        let limit = browse_page_size(o.get("limit"), 50);
        let where_ = swiss_host::dbbrowser::browse_where(DbDialect::Sqlite, &columns, o.get("filters"))?;
        let exprs = swiss_host::dbbrowser::quoted_exprs(DbDialect::Sqlite, &names)?;
        let rows_stmt = browse_rows_sql(
            DbDialect::Sqlite,
            None,
            table,
            &exprs,
            order.as_deref(),
            offset,
            limit + 1,
            &where_.frag,
            &where_.params,
        )?;
        let count_stmt = browse_count_sql(DbDialect::Sqlite, None, table, &where_.frag, &where_.params)?;
        let (got, total_rows) = tokio::join!(
            self.query(&rows_stmt.sql, &rows_stmt.params),
            self.query(&count_stmt.sql, &count_stmt.params),
        );
        let (rows, next_page) = page_and_next(got?, limit);
        let total = total_rows?.first().map(|r| i64_cell(r, "total")).unwrap_or(0);
        let columns_json = serde_json::to_value(&columns).map_err(|e| e.to_string())?;
        Ok(json!({
            "schema": SCHEMA,
            "table": table,
            "columns": columns_json,
            "rows": rows,
            "total": total,
            "offset": offset,
            "limit": limit,
            "nextPage": next_page,
            "primaryKey": primary,
            "editable": false,
            "editNote": self.refusal(),
        }))
    }

    async fn describe_table(&self, o: &Value) -> Result<Value, String> {
        Self::schema_of(o)?;
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.columns(table).await?;
        if columns.is_empty() {
            return Err(format!("not a table on this connection: {table}"));
        }
        let lit = sql_lit(table);
        let index_list = self
            .query(&format!("SELECT * FROM pragma_index_list({lit})"), &[])
            .await?;
        let mut index_rows: Vec<Map<String, Value>> = Vec::new();
        for idx in index_list {
            let name = idx.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if quote_ident(DbDialect::Sqlite, &name).is_err() {
                continue;
            }
            // to_browse_indexes treats unique == 1 as NOT unique (MySQL's Non_unique).
            let unique_flag = if i64_cell(&idx, "unique") == 1 { 0 } else { 1 };
            let primary_flag = if idx.get("origin").and_then(Value::as_str) == Some("pk") { 1 } else { 0 };
            let cols = self
                .query(
                    &format!("SELECT * FROM pragma_index_info({}) ORDER BY seqno", sql_lit(&name)),
                    &[],
                )
                .await?;
            if cols.is_empty() {
                let mut row = Map::new();
                row.insert("name".into(), json!(name));
                row.insert("unique".into(), json!(unique_flag));
                row.insert("primary".into(), json!(primary_flag));
                index_rows.push(row);
            }
            for c in cols {
                let mut row = Map::new();
                row.insert("name".into(), json!(name.clone()));
                row.insert("unique".into(), json!(unique_flag));
                row.insert("primary".into(), json!(primary_flag));
                row.insert("column".into(), c.get("name").cloned().unwrap_or(Value::Null));
                index_rows.push(row);
            }
        }
        let fk_rows = self
            .query(&format!("SELECT * FROM pragma_foreign_key_list({lit})"), &[])
            .await?;
        let foreign_keys: Vec<Value> = fk_rows
            .into_iter()
            .map(|r| {
                json!({
                    "name": Value::Null,
                    "column": r.get("from"),
                    "refSchema": SCHEMA,
                    "refTable": r.get("table"),
                    "refColumn": r.get("to"),
                })
            })
            .collect();
        let ddl = self
            .query(
                "SELECT sql FROM sqlite_master WHERE name = ? AND type IN ('table', 'view')",
                &[json!(table)],
            )
            .await?;
        let ddl = ddl
            .first()
            .and_then(|r| r.get("sql"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let columns_json = serde_json::to_value(&columns).map_err(|e| e.to_string())?;
        let indexes = serde_json::to_value(to_browse_indexes(&index_rows)).map_err(|e| e.to_string())?;
        Ok(json!({
            "schema": SCHEMA,
            "table": table,
            "columns": columns_json,
            "primaryKey": primary,
            "indexes": indexes,
            "foreignKeys": foreign_keys,
            "ddl": ddl,
        }))
    }

    async fn apply_edits(&self, _o: &Value) -> Result<Value, EditError> {
        Err(self.refusal().into())
    }

    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        self.exec_read(sql, limit).await
    }

    async fn run_read_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        self.exec_read(sql, limit).await
    }

    async fn run_agent_write(&self, _sql: &str, _cap: u64) -> Result<Value, WriteRefusal> {
        Err(WriteRefusal::Held(self.refusal()))
    }

    async fn export_table(&self, _o: &Value) -> Result<Value, String> {
        Err(self.refusal())
    }

    async fn export_sql_dump(&self, _o: &Value) -> Result<SqlDump, String> {
        Err(self.refusal())
    }

    async fn import_table(&self, _o: &Value) -> Result<Value, String> {
        Err(self.refusal())
    }

    async fn ddl_op(&self, _o: &Value) -> Result<Value, String> {
        Err(self.refusal())
    }

    async fn activity(&self) -> Result<Value, String> {
        Ok(json!({ "rows": [] }))
    }

    async fn activity_kill(&self, _pid: i64, _terminate: bool) -> Result<Value, String> {
        Err("this connection is query-only and has no sessions to cancel".into())
    }

    async fn completion(&self, sql: &str, caret: usize) -> Result<Value, String> {
        let Some(prefix) = sql_word_ending_at(sql, caret) else {
            return Ok(json!({ "items": [] }));
        };
        let listed = self
            .query(
                "SELECT name FROM sqlite_master \
                 WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                 ORDER BY name COLLATE NOCASE LIMIT 5000",
                &[],
            )
            .await?;
        let tables: Vec<String> = listed
            .iter()
            .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        Ok(json!({ "items": completion_items(DbDialect::Sqlite, prefix, &tables, None) }))
    }
}

impl SqliteBrowser {
    async fn exec_read(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        let sql = assert_sqlite_read(sql, &self.refusal())?;
        let prepared = crate::adapters::sql::with_row_limit(
            &sql,
            crate::adapters::sql::clamp_row_limit(limit, crate::adapters::sql::CONSOLE_ROW_LIMIT),
        );
        swiss_host::dbbrowser::execution::submitted(&prepared.sql);
        let rows = self.query(&prepared.sql, &[]).await?;
        Ok(crate::adapters::sql::console_reply(rows, &prepared, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteJournalMode;

    fn scratch() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "swiss-sqlite-browser-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn sample(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("store.sqlite3");
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO notes (body) VALUES ('kept')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        path
    }

    #[tokio::test]
    async fn a_query_only_store_lists_and_reads_and_refuses_a_write() {
        let dir = scratch();
        let path = sample(&dir).await;
        let browser = SqliteBrowser::new(path.clone(), "memory/store.sqlite3");
        let listed = browser.list_tables(&json!({})).await.unwrap();
        let names: Vec<&str> = listed["tables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["notes"]);
        assert!(listed["tables"][0]["approxRows"].is_null(), "name sort does not count");

        let page = browser.read_table(&json!({ "table": "notes" })).await.unwrap();
        assert_eq!(page["editable"], json!(false));
        assert!(page["editNote"].as_str().unwrap().contains("query-only"));
        assert_eq!(page["rows"][0]["body"], json!("kept"));
        assert_eq!(page["rows"][0]["id"], json!(1));
        assert_eq!(page["total"], json!(1));
        assert_eq!(page["primaryKey"], json!(["id"]));

        let described = browser.describe_table(&json!({ "table": "notes" })).await.unwrap();
        assert_eq!(described["primaryKey"], json!(["id"]));
        assert!(described["ddl"].as_str().unwrap().contains("CREATE TABLE notes"));

        let read = browser.run_query("SELECT body FROM notes", None).await.unwrap();
        assert_eq!(read["rows"][0]["body"], json!("kept"));
        // No declared type: a literal and an aggregate still have to come back as numbers.
        let shaped = browser
            .run_query("SELECT 1 AS n, COUNT(*) AS c FROM notes", None)
            .await
            .unwrap();
        assert_eq!(shaped["rows"][0]["n"], json!(1));
        assert_eq!(shaped["rows"][0]["c"], json!(1));

        let refused = browser.run_query("INSERT INTO notes (body) VALUES ('no')", None).await.unwrap_err();
        assert!(refused.contains("query-only"), "{refused}");
        let analyzed = browser.run_query("ANALYZE", None).await;
        assert!(analyzed.unwrap_err().contains("query-only"));
        let off = browser.run_query("PRAGMA query_only = OFF", None).await;
        assert!(off.is_err());

        // The memory plugin's writer still works. The browser sees the new row
        // and its own refusal did not land.
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false),
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO notes (body) VALUES ('from-the-writer')")
            .execute(&writer)
            .await
            .unwrap();
        writer.close().await;
        let again = browser.run_query("SELECT body FROM notes ORDER BY id", None).await.unwrap();
        let bodies: Vec<&str> = again["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r.get("body").and_then(Value::as_str))
            .collect();
        assert_eq!(bodies, vec!["kept", "from-the-writer"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_table_page_past_the_end_is_empty() {
        let dir = scratch();
        let path = sample(&dir).await;
        let browser = SqliteBrowser::new(path, "memory/store.sqlite3");
        let listed = browser.list_tables(&json!({ "page": 1 })).await.unwrap();
        assert_eq!(listed["tables"].as_array().unwrap().len(), 0);
        assert_eq!(listed["total"], json!(1));
        assert_eq!(listed["more"], json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_store_is_named_and_not_created() {
        let dir = scratch();
        let path = dir.join("store.sqlite3");
        let browser = SqliteBrowser::new(path.clone(), "memory/store.sqlite3");
        assert!(!path.is_file());
        let err = browser.run_query("SELECT 1", None).await.unwrap_err();
        assert!(err.contains("memory/store.sqlite3"), "{err}");
        assert!(!path.exists(), "a query must not create the memory store");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
