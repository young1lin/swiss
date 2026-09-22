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

//! MySQL Data-view browser backed by the adapter's shared sqlx pool.

use super::direct::Lazy;
use super::mysql::{
    run_query, MYSQL_BROWSE_COLUMNS_SQL, MYSQL_BROWSE_FK_SQL, MYSQL_BROWSE_INDEXES_SQL,
    MYSQL_BROWSE_PK_SQL,
};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::mysql::MySqlPool;
use std::sync::Arc;
use swiss_host::dbbrowser::{
    ambiguous_row_error, browse_count_sql, browse_offset, browse_order, browse_page_size,
    browse_rows_sql, build_ddl_create, build_ddl_op_sql, build_edit_statements,
    build_import_statements, conflict_of, ddl_script, export_row_limit, js_to_string,
    map_import_rows, optimistic_lock_columns, readback_plan, sql_dump_foot, sql_dump_head,
    sql_dump_literal, to_browse_columns, to_csv, to_json_lines, BrowseColumn, DbBrowser, DbDialect,
    DumpPiece, EditConflict, EditError, ReadBack, SqlDump, SqlInsertBatch, EXPORT_CHUNK,
    EXPORT_ROW_CAP, IMPORT_ROW_CAP,
};

/// The pure half of the docs/43 M3 schema-parameter decision: which database does this
/// READ target? Configured for absent/empty/same-as-configured (byte-identical path —
/// the SQL builders then receive the very string they always did); Foreign for anything
/// else, which the caller must whitelist against information_schema BEFORE any statement
/// is built. Making the decision a value is what lets the tests assert ordering: a
/// Foreign name never reaches a builder until db_of has cleared it.
#[derive(Debug)]
enum DbChoice {
    Configured,
    Foreign(String),
}

fn resolve_db(asked: Option<&str>, configured: &str) -> DbChoice {
    match asked {
        None => DbChoice::Configured,
        // Present-but-empty is the route's "not sent" (q_non_empty) — same as absent.
        Some(db) if db.is_empty() || db == configured => DbChoice::Configured,
        Some(db) => DbChoice::Foreign(db.to_string()),
    }
}

/// The read-only note a non-configured database earns on /data (docs/43 M3): the panel
/// already reads editNote, so the rule needs no extra client logic. Pure, so the exact
/// sentence is asserted without a pool.
fn foreign_db_note(db: &str, primary: &str) -> String {
    format!("Read-only: {db} is not this connection's configured database ({primary}).")
}

pub struct MysqlBrowser {
    database: String,
    label: String,
    conn: Arc<Lazy<MySqlPool>>,
    /// Completion candidates cache (docs/22 W3.1): table names + column lists, TTL-bound.
    /// Mutex (no await while held) — the browser is shared behind Arc for the lease's life.
    completion_cache: std::sync::Mutex<swiss_host::dbbrowser::CompletionCache>,
}
impl MysqlBrowser {
    pub fn new(database: String, label: String, conn: Arc<Lazy<MySqlPool>>) -> Self {
        Self {
            database,
            label,
            conn,
            completion_cache: std::sync::Mutex::new(swiss_host::dbbrowser::CompletionCache::new()),
        }
    }
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.conn.get().await?;
        Ok(run_query(&pool, sql, params).await?.rows)
    }
    /// Resolve the database a READ request targets (docs/43 M3). Absent or equal to the
    /// configured database keeps today's behavior byte-identical; any other name must FIRST
    /// appear in information_schema.schemata — the whitelist check runs BEFORE any SQL is
    /// built, so an unknown name is refused without ever being quoted into a statement.
    /// Quoting then applies as before: two gates, not either/or.
    async fn db_of(&self, o: &Value) -> Result<String, String> {
        match resolve_db(o.get("schema").and_then(Value::as_str), &self.database) {
            DbChoice::Configured => Ok(self.database.clone()),
            DbChoice::Foreign(db) => {
                let rows = self
                    .query(
                        "SELECT 1 AS ok FROM information_schema.schemata WHERE schema_name = ?",
                        &[json!(db)],
                    )
                    .await?;
                if rows.is_empty() {
                    return Err(format!("not a database on this connection: {db}"));
                }
                Ok(db)
            }
        }
    }
    /// The MySQL database catalog (docs/43 M3): every schema in information_schema with its
    /// table count, system schemas flagged and still listed (the panel sorts them last and
    /// dims them). primary == current == the configured database; switching costs nothing
    /// because every read statement is db.table-qualified already.
    async fn databases(&self) -> Result<Value, String> {
        let rows = self
            .query(
                "SELECT s.schema_name AS name, COALESCE(t.cnt, 0) AS tables \
                 FROM information_schema.schemata s \
                 LEFT JOIN (SELECT table_schema, COUNT(*) AS cnt FROM information_schema.tables \
                 GROUP BY table_schema) t ON t.table_schema = s.schema_name \
                 ORDER BY s.schema_name",
                &[],
            )
            .await?;
        const SYSTEM: [&str; 4] = [
            "information_schema",
            "mysql",
            "performance_schema",
            "sys",
        ];
        let databases: Vec<Value> = rows
            .iter()
            .map(|r| {
                let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                json!({
                    "name": name,
                    "primary": name == self.database,
                    "browsable": true,
                    "system": SYSTEM.contains(&name),
                    "tables": super::mysql::num_or_zero(r.get("tables")),
                })
            })
            .collect();
        Ok(json!({
            "primary": self.database,
            "current": self.database,
            "databases": databases,
        }))
    }
    async fn metadata(
        &self,
        db: &str,
        table: &str,
    ) -> Result<(Vec<BrowseColumn>, Vec<String>), String> {
        if table.is_empty() {
            return Err("table is required".into());
        }
        let params = vec![json!(db), json!(table)];
        let (columns, primary) = tokio::join!(
            self.query(MYSQL_BROWSE_COLUMNS_SQL, &params),
            self.query(MYSQL_BROWSE_PK_SQL, &params)
        );
        let columns = columns?;
        if columns.is_empty() {
            return Err(format!("no such table: {}.{table}", self.database));
        }
        let primary: Vec<String> = primary?
            .iter()
            .filter_map(|r| {
                r.get("column_name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        Ok((to_browse_columns(&columns, &primary), primary))
    }
    /// SHOW CREATE TABLE folded to its statement text — the DDL the Structure tab serves and
    /// the SQL dump's head opens with (docs/22 W4.4), read through one helper so the two
    /// cannot drift apart.
    async fn show_create(&self, db: &str, table: &str) -> Result<String, String> {
        let ddl_sql = format!(
            "SHOW CREATE TABLE {}.{}",
            swiss_host::dbbrowser::quote_ident(DbDialect::Mysql, db)?,
            swiss_host::dbbrowser::quote_ident(DbDialect::Mysql, table)?
        );
        let row = self
            .query(&ddl_sql, &[])
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(row
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase().starts_with("create "))
            .map(|(_, v)| js_to_string(Some(v)))
            .unwrap_or_default())
    }
}
#[async_trait]
impl DbBrowser for MysqlBrowser {
    fn dialect(&self) -> DbDialect {
        DbDialect::Mysql
    }
    fn label(&self) -> String {
        self.label.clone()
    }
    async fn list_tables(&self, o: &Value) -> Result<Value, String> {
        // Number()-style coercion: the route forwards query params as JSON strings.
        let page = o
            .get("page")
            .and_then(swiss_host::dbbrowser::js_number)
            .map(f64::floor)
            .unwrap_or(0.0)
            .max(0.0) as i64;
        // The sidebar tree fetches WHOLE catalogs (panel sends limit=2000, docs/43 addendum),
        // so the ceiling here bounds a hand-written query, not the panel: 5000 names + row
        // estimates is still one cheap metadata page, and anything larger belongs to grep.
        let limit = swiss_host::dbbrowser::clamp_browse_limit(o.get("limit"), 200, 5000);
        let grep = o.get("grep").and_then(Value::as_str);
        let sort = swiss_host::dbbrowser::browse_table_sort(
            o.get("sort").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
        )?;
        // docs/22 W1.6: a grammar grep (comma AND / | OR / * wildcard) expands to multi-LIKE
        // SQL; a plain substring keeps the single-LIKE statement byte-for-byte.
        let grammar = grep
            .map(|g| swiss_host::dbbrowser::grep_where(DbDialect::Mysql, "table_name", g, 0))
            .transpose()?
            .flatten();
        // docs/43 M3: the schema parameter names the DATABASE here — absent keeps the
        // configured one byte-identical; a foreign name must clear the whitelist first.
        let db = self.db_of(o).await?;
        let ((ls, lp), (cs, cp)) = match grammar {
            Some(w) => super::mysql::mysql_list_tables_grammar_sql(
                &db,
                &w.frag,
                &w.params,
                (page, limit, page.saturating_mul(limit)),
                Some(sort),
            ),
            None => super::mysql::mysql_list_tables_sql(
                &db,
                grep,
                (page, limit, page.saturating_mul(limit)),
                Some(sort),
            ),
        };
        let (list, count) = tokio::join!(self.query(&ls, &lp), self.query(&cs, &cp));
        let list = list?;
        let total = super::mysql::num_or_zero(count?.first().and_then(|r| r.get("total")));
        let tables: Vec<Value> = list.into_iter().map(|r| json!({"schema":db,"name":r.get("name").and_then(Value::as_str).unwrap_or(""),"type":r.get("type").and_then(Value::as_str).unwrap_or(""),"approxRows":r.get("approx_rows").cloned().unwrap_or(Value::Null),"size":super::resources::human_bytes(super::mysql::num_or_zero(r.get("bytes")))})).collect();
        let more = page.saturating_mul(limit) as u64 + (tables.len() as u64) < total;
        Ok(json!({"tables":tables,"total":total,"page":page,"limit":limit,"more":more}))
    }
    async fn read_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let db = self.db_of(o).await?;
        let (columns, primary) = self.metadata(&db, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let order = browse_order(
            &names,
            o.get("order").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
            DbDialect::Mysql,
        )?;
        let offset = browse_offset(o.get("offset"));
        let limit = browse_page_size(o.get("limit"), 50);
        // The grid's filters feed the page, the COUNT and exports through one WHERE
        // (browse_where) so the three can never drift (docs/22 W0.2).
        let where_ =
            swiss_host::dbbrowser::browse_where(DbDialect::Mysql, &columns, o.get("filters"))?;
        let exprs = swiss_host::dbbrowser::quoted_exprs(DbDialect::Mysql, &names)?;
        // docs/22 W1.9: fetch one row past the page — its presence answers "is there a next
        // page" without trusting COUNT arithmetic under concurrent writes.
        let rows_stmt = browse_rows_sql(
            DbDialect::Mysql,
            Some(&db),
            table,
            &exprs,
            order.as_deref(),
            offset,
            limit + 1,
            &where_.frag,
            &where_.params,
        )?;
        let count_stmt = browse_count_sql(
            DbDialect::Mysql,
            Some(&db),
            table,
            &where_.frag,
            &where_.params,
        )?;
        let (rows, count) = tokio::join!(
            self.query(&rows_stmt.sql, &rows_stmt.params),
            self.query(&count_stmt.sql, &count_stmt.params)
        );
        let (rows, next_page) = swiss_host::dbbrowser::page_and_next(rows?, limit);
        let total = super::mysql::num_or_zero(count?.first().and_then(|r| r.get("total")));
        // docs/22 W4.1: every table is editable — a keyless one addresses rows by every
        // column (NULL makes a row unaddressable, twins are refused), so the old pk-less
        // refusal becomes the note that says how the addressing works instead. docs/43 M3
        // adds the one exception: a table in a NON-configured database is read-only — the
        // write paths stay pinned to the configured database, and the panel already reads
        // editable/editNote, so no extra client rule is needed.
        let editable = db == self.database;
        let edit_note = if !editable {
            Some(foreign_db_note(&db, &self.database))
        } else if primary.is_empty() {
            Some("rows are addressed by all columns; ambiguous rows are refused".to_string())
        } else {
            None
        };
        Ok(
            json!({"schema":db,"table":table,"columns":columns,"rows":rows,"total":total,"offset":offset,"limit":limit,"nextPage":next_page,"primaryKey":primary,"editable":editable,"editNote":edit_note}),
        )
    }
    async fn describe_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let db = self.db_of(o).await?;
        let (columns, primary) = self.metadata(&db, table).await?;
        let params = vec![json!(db), json!(table)];
        let (indexes, fks, ddl) = tokio::join!(
            self.query(MYSQL_BROWSE_INDEXES_SQL, &params),
            self.query(MYSQL_BROWSE_FK_SQL, &params),
            self.show_create(&db, table)
        );
        // Map the SQL's aliases onto the fold's (Node's adapter did this inline): unique0 is
        // non_unique inverted by the fold's `!== 1`, PRIMARY is the PK index's actual name in
        // MySQL, col is the indexed column.
        let index_rows: Vec<Map<String, Value>> = indexes?
            .into_iter()
            .map(|r| {
                let mut m = Map::new();
                m.insert("name".into(), r.get("name").cloned().unwrap_or(Value::Null));
                m.insert(
                    "unique".into(),
                    r.get("unique0").cloned().unwrap_or(Value::Null),
                );
                let is_primary = r.get("name").and_then(Value::as_str) == Some("PRIMARY");
                m.insert("primary".into(), json!(if is_primary { 1 } else { 0 }));
                if let Some(col) = r.get("col") {
                    m.insert("column".into(), col.clone());
                }
                m
            })
            .collect();
        let indexes = swiss_host::dbbrowser::to_browse_indexes(&index_rows);
        let foreign_keys: Vec<Value> = fks?.into_iter().map(|r| json!({"name":r.get("name"),"column":r.get("col"),"refSchema":r.get("ref_schema"),"refTable":r.get("ref_table"),"refColumn":r.get("ref_column")})).collect();
        Ok(
            json!({"schema":db,"table":table,"columns":columns,"primaryKey":primary,"indexes":indexes,"foreignKeys":foreign_keys,"ddl":ddl?}),
        )
    }
    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        // A console DDL changes schema shape: the completion cache drops everything so the
        // next keystroke re-reads the catalog it now describes (docs/22 W3.1).
        if swiss_host::dbbrowser::sql_touches_schema(sql) {
            self.completion_cache
                .lock()
                .expect("completion cache")
                .invalidate();
        }
        // The console's one rule: a single statement per run. Reads and writes alike go through —
        // this console belongs to the panel on the operator's own machine.
        let sql = crate::adapters::sql::assert_single_statement(sql)?;
        let prepared = crate::adapters::sql::with_row_limit(
            &sql,
            crate::adapters::sql::clamp_row_limit(limit, 50),
        );
        let rows: Vec<Value> = {
            let pool = self.conn.get().await?;
            run_query(&pool, &prepared.sql, &[]).await?.rows
        }
        .into_iter()
        .map(Value::Object)
        .collect();
        let columns = rows
            .first()
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        // Driver limitation, accepted: `columns` derives from the first row, so a zero-row
        // SELECT answers columns: [] — mysql2 exposed field metadata for the empty header.
        let row_count = rows.len();
        // The LIMIT-less SELECT note rides the reply (Node spread limitReport in): a silent
        // truncation at 50 rows is a lie the console must not tell.
        let mut out = Map::new();
        out.insert("columns".into(), json!(columns));
        out.insert("rows".into(), Value::Array(rows));
        out.insert("rowCount".into(), json!(row_count));
        if let Value::Object(extra) =
            crate::adapters::sql::limit_report(&prepared, row_count as i64, limit)
        {
            for (k, v) in extra {
                out.insert(k, v);
            }
        }
        Ok(Value::Object(out))
    }
    async fn apply_edits(&self, o: &Value) -> Result<Value, EditError> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let edits = o.get("edits").and_then(Value::as_array);
        if table.is_empty() {
            return Err("table is required".into());
        }
        let Some(edits) = edits.filter(|e| !e.is_empty()) else {
            return Err("edits must be a non-empty array".into());
        };
        // docs/43 M3: edits are a write path — pinned to the CONFIGURED database, always.
        let (columns, primary) = self.metadata(&self.database, table).await?;
        let typed: Vec<swiss_host::dbbrowser::BrowseEdit> = edits
            .iter()
            .map(swiss_host::dbbrowser::browse_edit_of)
            .collect::<Result<_, _>>()?;
        let stmts = build_edit_statements(
            DbDialect::Mysql,
            Some(&self.database),
            table,
            &typed,
            &columns,
            &primary,
        )?;
        // docs/22 W1.7: same read-back as Postgres, shaped for MySQL — an insert's row comes
        // back by LAST_INSERT_ID() on the same connection, an update's by its primary key;
        // both inside the same transaction so the reply is what the commit really kept.
        let pool = self.conn.get().await?;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let mut results: Vec<Value> = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.iter().enumerate() {
            let plan = readback_plan(
                DbDialect::Mysql,
                Some(&self.database),
                table,
                &columns,
                &primary,
                &typed[i],
            );
            let affected = super::mysql::run_query_tx(&mut tx, &stmt.sql, &stmt.params).await?;
            // docs/22 W4.1: a keyless table addresses rows by every column, and the builder
            // clips MySQL's statements with LIMIT 1 — an UPDATE can never fan out. The guard
            // stays so the dialects carry the same verdict (Postgres has no LIMIT form and
            // genuinely needs it); an insert is single-row by construction and cannot trip.
            if primary.is_empty() && affected > 1 {
                return Err(EditError::Bad(ambiguous_row_error(typed[i].op(), affected)));
            }
            // The read-back runs before any verdict, so an optimistic update that matched
            // nothing probes with the same SELECT: the row the database shows RIGHT NOW
            // (inside this still-open transaction) is what names the conflicting columns.
            let row = match plan {
                ReadBack::Select(s) => super::mysql::run_query_tx_rows(&mut tx, &s.sql, &s.params)
                    .await?
                    .into_iter()
                    .next(),
                _ => None,
            };
            // docs/22 W4.2: an update whose optimistic lock matched zero rows lost the race
            // — another writer moved the row between the read and this commit. Refuse the
            // whole batch (the dropped transaction rolls back what already ran) instead of
            // overwriting. A bare pk-only update never claimed to know the row, so for it
            // zero affected stays the quiet idempotent result it always was.
            if affected == 0 {
                if let swiss_host::dbbrowser::BrowseEdit::Update { .. } = &typed[i] {
                    let lock = optimistic_lock_columns(&primary, &typed[i]);
                    if !lock.is_empty() {
                        let origin = match &typed[i] {
                            swiss_host::dbbrowser::BrowseEdit::Update { pk, source, .. } => {
                                source.clone().unwrap_or_else(|| pk.clone())
                            }
                            _ => Map::new(),
                        };
                        return Err(EditError::Conflict(EditConflict {
                            row: Some(origin.clone()),
                            ..conflict_of(&lock, &origin, row.as_ref())
                        }));
                    }
                }
            }
            results.push(json!({"op": typed[i].op(), "affected": affected, "row": row}));
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(json!({ "results": results }))
    }

    async fn export_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let db = self.db_of(o).await?;
        let (columns, _) = self.metadata(&db, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let cap = export_row_limit(o.get("limit"));
        // An export of a filtered grid exports the FILTERED set: the same browse_where the
        // page and its COUNT use (docs/22 W0.2), values bound, never inlined.
        let where_ =
            swiss_host::dbbrowser::browse_where(DbDialect::Mysql, &columns, o.get("filters"))?;
        let mut all: Vec<Map<String, Value>> = Vec::new();
        let mut capped = false;
        // Offset paging in chunks: simple, and the cap keeps the O(offset) tail-walk bounded.
        let mut offset = 0i64;
        let exprs = swiss_host::dbbrowser::quoted_exprs(DbDialect::Mysql, &names)?;
        while offset < cap {
            let chunk = EXPORT_CHUNK.min(cap - offset);
            let stmt = browse_rows_sql(
                DbDialect::Mysql,
                Some(&db),
                table,
                &exprs,
                None,
                offset,
                chunk,
                &where_.frag,
                &where_.params,
            )?;
            let rows = self.query(&stmt.sql, &stmt.params).await?;
            let fetched = rows.len() as i64;
            all.extend(rows);
            if fetched < chunk {
                break; // table exhausted before the cap
            }
            if offset + chunk >= cap {
                capped = true;
            }
            offset += chunk;
        }
        let capped = capped || all.len() as i64 >= EXPORT_ROW_CAP;
        let json_form = o.get("format").and_then(Value::as_str) == Some("json");
        Ok(json!({
            "format": if json_form { "json" } else { "csv" },
            "columns": names,
            "rows": all.len(),
            "capped": capped,
            "body": if json_form { to_json_lines(&all) } else { to_csv(&names, &all) },
        }))
    }

    async fn export_sql_dump(&self, o: &Value) -> Result<SqlDump, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let db = self.db_of(o).await?;
        let (columns, _) = self.metadata(&db, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let cap = export_row_limit(o.get("limit"));
        // The dump exports the FILTERED set: the same browse_where the page, the COUNT and the
        // folded exports use (docs/22 W0.2), values bound, never inlined.
        let where_ =
            swiss_host::dbbrowser::browse_where(DbDialect::Mysql, &columns, o.get("filters"))?;
        // Header facts come from that same WHERE's COUNT, so rows and capped are true before
        // the first byte ships (the folded formats learn their row count only after fetching;
        // a writer racing the stream can still move the table under a taken count — inherent).
        let count_stmt = browse_count_sql(
            DbDialect::Mysql,
            Some(&db),
            table,
            &where_.frag,
            &where_.params,
        )?;
        let count = super::mysql::num_or_zero(
            self.query(&count_stmt.sql, &count_stmt.params)
                .await?
                .first()
                .and_then(|r| r.get("total")),
        ) as i64;
        let rows = count.min(cap);
        let capped = count > rows;
        let head = sql_dump_head(
            DbDialect::Mysql,
            Some(&db),
            table,
            &self.show_create(&db, table).await?,
        )?;
        let exprs = swiss_host::dbbrowser::quoted_exprs(DbDialect::Mysql, &names)?;
        // The producer task holds one chunk of source rows and one statement under
        // construction — never the table. The bounded channel (2 pieces) hands each finished
        // statement to the route as the socket takes it, so a slow client throttles the dump
        // instead of growing it.
        let (tx, rx) = tokio::sync::mpsc::channel::<DumpPiece>(2);
        let dump_columns = names.clone();
        let conn = self.conn.clone();
        let database = db;
        let table = table.to_string();
        tokio::spawn(async move {
            if tx.send(Ok(head.into_bytes())).await.is_err() {
                return; // the consumer is gone; stop fetching
            }
            let mut batch =
                match SqlInsertBatch::new(DbDialect::Mysql, Some(&database), &table, &names) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
            // Offset paging in chunks, the folded export's loop shape: the cap keeps the
            // O(offset) tail-walk bounded and each page is released before the next is read.
            let mut offset = 0i64;
            while offset < rows {
                let chunk = EXPORT_CHUNK.min(rows - offset);
                let stmt = match browse_rows_sql(
                    DbDialect::Mysql,
                    Some(&database),
                    &table,
                    &exprs,
                    None,
                    offset,
                    chunk,
                    &where_.frag,
                    &where_.params,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                let page = match async {
                    let pool = conn.get().await?;
                    run_query(&pool, &stmt.sql, &stmt.params)
                        .await
                        .map(|r| r.rows)
                }
                .await
                {
                    Ok(rows) => rows,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                let fetched = page.len() as i64;
                for row in &page {
                    // The dump body is EXECUTED on replay — sql_dump_literal, never the
                    // clipboard's sql_literal (docs/22 W4.4 audit blocker).
                    let literals: Result<Vec<String>, String> = names
                        .iter()
                        .map(|c| sql_dump_literal(DbDialect::Mysql, row.get(c)))
                        .collect();
                    let literals = match literals {
                        Ok(l) => l,
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    };
                    if let Some(stmt) = batch.push(&format!("({})", literals.join(", "))) {
                        if tx.send(Ok(stmt.into_bytes())).await.is_err() {
                            return;
                        }
                    }
                }
                if fetched < chunk {
                    break; // the table ran out before the count (a delete raced the COUNT)
                }
                offset += chunk;
            }
            if let Some(tail) = batch.finish() {
                let _ = tx.send(Ok(tail.into_bytes())).await;
            }
            let _ = tx
                .send(Ok(sql_dump_foot(DbDialect::Mysql).as_bytes().to_vec()))
                .await;
        });
        Ok(SqlDump {
            columns: dump_columns,
            rows,
            capped,
            body: rx,
        })
    }

    async fn import_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let header: Vec<String> = o
            .get("header")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let lines: Vec<String> = o
            .get("lines")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if lines.len() > IMPORT_ROW_CAP {
            return Err(format!(
                "too many rows for one import (max {IMPORT_ROW_CAP})"
            ));
        }
        let mapping: Vec<Option<String>> = o
            .get("mapping")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let rows = map_import_rows(&header, &lines, &mapping)?;
        if rows.is_empty() {
            return Err("nothing to import after mapping — every row was empty or skipped".into());
        }
        // docs/22 W4.5: upsert appends ON DUPLICATE KEY UPDATE per row; insert builds the
        // exact statements the edit grid's insert arm builds. One transaction either way.
        let mode = swiss_host::dbbrowser::parse_import_mode(o.get("mode"))?;
        // docs/43 M3: the write paths stay pinned to the CONFIGURED database — an import
        // never targets a foreign one, schema parameter or not.
        let (columns, primary) = self.metadata(&self.database, table).await?;
        let (stmts, degraded) = build_import_statements(
            DbDialect::Mysql,
            Some(&self.database),
            table,
            &rows,
            &columns,
            &primary,
            mode,
        )?;
        let pool = self.conn.get().await?;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for stmt in &stmts {
            super::mysql::run_query_tx(&mut tx, &stmt.sql, &stmt.params).await?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        if let Some(note) = degraded {
            return Ok(json!({ "inserted": rows.len(), "mode": "insert", "note": note }));
        }
        Ok(json!({ "inserted": rows.len() }))
    }

    async fn ddl_op(&self, o: &Value) -> Result<Value, String> {
        // docs/22 W4.6: the create ops share ONE builder with /ddl-preview — the sheet showed
        // these exact statements before Commit posted. Each op is a single statement here
        // (MySQL embeds comments inline), so there is nothing to wrap: MySQL DDL commits
        // itself, atomically per statement.
        let op = o.get("op").and_then(Value::as_str).unwrap_or("");
        if matches!(op, "create_table" | "add_column" | "create_index") {
            let stmts = build_ddl_create(DbDialect::Mysql, op, o)?;
            let pool = self.conn.get().await?;
            for s in &stmts {
                run_query(&pool, s, &[]).await?;
            }
            self.completion_cache
                .lock()
                .expect("completion cache")
                .invalidate();
            return Ok(json!({ "ran": ddl_script(&stmts) }));
        }
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let to = o
            .get("to")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        let sql = build_ddl_op_sql(
            DbDialect::Mysql,
            o.get("op").and_then(Value::as_str).unwrap_or(""),
            Some(&self.database),
            table,
            to,
        )?;
        let pool = self.conn.get().await?;
        run_query(&pool, &sql, &[]).await?;
        self.completion_cache
            .lock()
            .expect("completion cache")
            .invalidate();
        Ok(json!({ "ran": sql }))
    }

    async fn activity(&self) -> Result<Value, String> {
        // The statement already aliases to the shared reply keys (dbbrowser.rs). The grid path
        // renders BIGINT cells as text, which is right for a grid and wrong for pid/seconds/own
        // here - activity_row types those three (docs/37 §11 D11).
        let rows: Vec<Map<String, Value>> = self
            .query(&swiss_host::dbbrowser::activity_sql(DbDialect::Mysql), &[])
            .await?
            .into_iter()
            .map(swiss_host::dbbrowser::activity_row)
            .collect();
        Ok(json!({ "rows": rows }))
    }

    async fn activity_kill(&self, pid: i64, terminate: bool) -> Result<Value, String> {
        let sql = swiss_host::dbbrowser::activity_kill_sql(DbDialect::Mysql, pid, terminate)?;
        self.query(&sql, &[]).await?;
        // KILL / KILL QUERY answer an OK packet — no result row to read a truth from.
        Ok(json!({ "ok": true }))
    }

    async fn completion(&self, sql: &str, caret: usize) -> Result<Value, String> {
        use swiss_host::dbbrowser::{completion_from_table, completion_items, sql_word_ending_at};
        let Some(prefix) = sql_word_ending_at(sql, caret) else {
            return Ok(json!({ "items": [] }));
        };
        let now = std::time::Instant::now();
        let plan = {
            // Lock only to read the plan — the loads below await.
            let cache = self.completion_cache.lock().expect("completion cache");
            (
                cache.needs_tables(now),
                completion_from_table(sql, caret).filter(|_| !cache.is_degraded()),
            )
        };
        let (need_tables, from) = plan;
        // Tables: the one database this connection owns, cached for the TTL. needs_tables is
        // TTL-only, so a degraded cache keeps serving its fresh list — the column budget does
        // not poison the table names into a catalog query on every keystroke.
        let tables = if need_tables {
            let rows = self
                .query(
                    "SELECT table_name AS name FROM information_schema.tables \
                     WHERE table_schema = ? ORDER BY name",
                    &[json!(self.database)],
                )
                .await?;
            let names: Vec<String> = rows
                .iter()
                .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
                .collect();
            self.completion_cache
                .lock()
                .expect("completion cache")
                .set_tables(names.clone(), now);
            names
        } else {
            self.completion_cache
                .lock()
                .expect("completion cache")
                .tables(now)
                .unwrap_or_default()
        };
        // Columns of the FROM-nearest table, cached per table.
        let mut columns: Option<(String, Vec<String>)> = None;
        if let Some(word) = from {
            let cached = {
                let cache = self.completion_cache.lock().expect("completion cache");
                cache.columns(&word, now)
            };
            if let Some(cols) = cached {
                columns = Some((word, cols));
            } else {
                let rows = self
                    .query(
                        "SELECT column_name FROM information_schema.columns \
                         WHERE table_schema = ? AND table_name = ? \
                         ORDER BY ordinal_position",
                        &[json!(self.database), json!(word)],
                    )
                    .await?;
                let cols: Vec<String> = rows
                    .iter()
                    .filter_map(|r| {
                        r.get("column_name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect();
                self.completion_cache
                    .lock()
                    .expect("completion cache")
                    .set_columns(word.clone(), cols.clone(), now);
                columns = Some((word, cols));
            }
        }
        let columns = columns.as_ref().map(|(t, c)| (t.as_str(), c.as_slice()));
        let items = completion_items(DbDialect::Mysql, prefix, &tables, columns);
        Ok(json!({ "items": items }))
    }
    /// docs/43 M3: the full information_schema catalog with table counts. Completion stays
    /// pinned to the CONFIGURED database (the console's write paths are) — a foreign db's
    /// tables are not completion candidates.
    async fn list_databases(&self) -> Result<Value, String> {
        self.databases().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs/43 M3: the whitelist decision precedes SQL construction BY CONSTRUCTION —
    /// resolve_db is the only path from a schema parameter to a database name, and its
    /// Foreign arm is the only one the caller may not hand straight to a builder.
    #[test]
    fn an_absent_or_matching_schema_takes_the_configured_path_byte_identically() {
        // Absent, empty, and the configured name itself all collapse to Configured — the
        // builders then receive the very string they always did (same SQL, same binds).
        assert!(matches!(
            resolve_db(None, "acme_app_dev"),
            DbChoice::Configured
        ));
        assert!(matches!(
            resolve_db(Some(""), "acme_app_dev"),
            DbChoice::Configured
        ));
        assert!(matches!(
            resolve_db(Some("acme_app_dev"), "acme_app_dev"),
            DbChoice::Configured
        ));
    }

    #[test]
    fn a_foreign_name_is_marked_foreign_before_any_builder_sees_it() {
        // The decision VALUE is Foreign — the whitelist query in db_of runs on this arm
        // alone, so an unknown name is refused without ever being quoted into a statement
        // (two gates: the whitelist, then quote_ident, never either/or).
        match resolve_db(Some("acme_app_uat"), "acme_app_dev") {
            DbChoice::Foreign(db) => assert_eq!(db, "acme_app_uat"),
            other => panic!("expected Foreign, got {other:?}-arm"),
        }
    }

    #[test]
    fn the_foreign_read_only_note_names_both_databases() {
        // /data on a non-configured database must say read-only AND name the configured
        // one — the panel shows this sentence verbatim (ApiDbDataPage.editNote).
        assert_eq!(
            foreign_db_note("acme_app_uat", "acme_app_dev"),
            "Read-only: acme_app_uat is not this connection's configured database (acme_app_dev)."
        );
    }
}
