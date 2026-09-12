//! MySQL Data-view browser backed by the adapter's shared sqlx pool.

use super::direct::Lazy;
use super::mysql::{
    run_query, MYSQL_BROWSE_COLUMNS_SQL, MYSQL_BROWSE_FK_SQL, MYSQL_BROWSE_INDEXES_SQL,
    MYSQL_BROWSE_PK_SQL,
};
use async_trait::async_trait;
use swiss_host::dbbrowser::{
    browse_count_sql, browse_offset, browse_order, browse_page_size, browse_rows_sql,
    build_ddl_op_sql, build_edit_statements, build_import_statements, export_row_limit,
    js_to_string, map_import_rows,
    readback_plan, sql_dump_foot, sql_dump_head, sql_literal, to_browse_columns, to_csv,
    to_json_lines, BrowseColumn, DbBrowser, DbDialect, DumpPiece, ReadBack, SqlDump,
    SqlInsertBatch, EXPORT_CHUNK, EXPORT_ROW_CAP, IMPORT_ROW_CAP,
};
use serde_json::{json, Map, Value};
use sqlx::mysql::MySqlPool;
use std::sync::Arc;

pub struct MysqlBrowser {
    database: String,
    label: String,
    conn: Arc<Lazy<MySqlPool>>,
}
impl MysqlBrowser {
    pub fn new(database: String, label: String, conn: Arc<Lazy<MySqlPool>>) -> Self {
        Self {
            database,
            label,
            conn,
        }
    }
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.conn.get().await?;
        Ok(run_query(&pool, sql, params).await?.rows)
    }
    async fn metadata(&self, table: &str) -> Result<(Vec<BrowseColumn>, Vec<String>), String> {
        if table.is_empty() {
            return Err("table is required".into());
        }
        let params = vec![json!(self.database), json!(table)];
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
    async fn show_create(&self, table: &str) -> Result<String, String> {
        let ddl_sql = format!(
            "SHOW CREATE TABLE {}.{}",
            swiss_host::dbbrowser::quote_ident(DbDialect::Mysql, &self.database)?,
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
        let limit = swiss_host::dbbrowser::clamp_browse_limit(o.get("limit"), 200, 1000);
        let grep = o.get("grep").and_then(Value::as_str);
        let sort = swiss_host::dbbrowser::browse_table_sort(
            o.get("sort").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
        )?;
        // docs/22 W1.6: a grammar grep (comma AND / | OR / * wildcard) expands to multi-LIKE
        // SQL; a plain substring keeps the single-LIKE statement byte-for-byte.
        let grammar = grep
            .map(|g| {
                swiss_host::dbbrowser::grep_where(DbDialect::Mysql, "table_name", g, 0)
            })
            .transpose()?
            .flatten();
        let ((ls, lp), (cs, cp)) = match grammar {
            Some(w) => super::mysql::mysql_list_tables_grammar_sql(
                &self.database,
                &w.frag,
                &w.params,
                (page, limit, page.saturating_mul(limit)),
                Some(sort),
            ),
            None => super::mysql::mysql_list_tables_sql(
                &self.database,
                grep,
                (page, limit, page.saturating_mul(limit)),
                Some(sort),
            ),
        };
        let (list, count) = tokio::join!(self.query(&ls, &lp), self.query(&cs, &cp));
        let list = list?;
        let total = super::mysql::num_or_zero(count?.first().and_then(|r| r.get("total")));
        let tables: Vec<Value> = list.into_iter().map(|r| json!({"schema":self.database,"name":r.get("name").and_then(Value::as_str).unwrap_or(""),"type":r.get("type").and_then(Value::as_str).unwrap_or(""),"approxRows":r.get("approx_rows").cloned().unwrap_or(Value::Null),"size":super::resources::human_bytes(super::mysql::num_or_zero(r.get("bytes")))})).collect();
        let more = page.saturating_mul(limit) as u64 + (tables.len() as u64) < total;
        Ok(json!({"tables":tables,"total":total,"page":page,"limit":limit,"more":more}))
    }
    async fn read_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.metadata(table).await?;
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
            Some(&self.database),
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
            Some(&self.database),
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
        let editable = !primary.is_empty();
        // Node's editNote: the pk-less explanation is the only one left (mysql.ts).
        let edit_note = if primary.is_empty() {
            Some("table has no primary key, so a row cannot be addressed for edits")
        } else {
            None
        };
        Ok(
            json!({"schema":self.database,"table":table,"columns":columns,"rows":rows,"total":total,"offset":offset,"limit":limit,"nextPage":next_page,"primaryKey":primary,"editable":editable,"editNote":edit_note}),
        )
    }
    async fn describe_table(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.metadata(table).await?;
        let params = vec![json!(self.database), json!(table)];
        let (indexes, fks, ddl) = tokio::join!(
            self.query(MYSQL_BROWSE_INDEXES_SQL, &params),
            self.query(MYSQL_BROWSE_FK_SQL, &params),
            self.show_create(table)
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
            json!({"schema":self.database,"table":table,"columns":columns,"primaryKey":primary,"indexes":indexes,"foreignKeys":foreign_keys,"ddl":ddl?}),
        )
    }
    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
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
    async fn apply_edits(&self, o: &Value) -> Result<Value, String> {
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let edits = o.get("edits").and_then(Value::as_array);
        if table.is_empty() {
            return Err("table is required".into());
        }
        let Some(edits) = edits.filter(|e| !e.is_empty()) else {
            return Err("edits must be a non-empty array".into());
        };
        let (columns, primary) = self.metadata(table).await?;
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
        let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let pool = self.conn.get().await?;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let mut results: Vec<Value> = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.iter().enumerate() {
            let plan = readback_plan(
                DbDialect::Mysql,
                Some(&self.database),
                table,
                &col_names,
                &primary,
                &typed[i],
            );
            let affected = super::mysql::run_query_tx(&mut tx, &stmt.sql, &stmt.params).await?;
            let row = match plan {
                ReadBack::Select(s) => super::mysql::run_query_tx_rows(&mut tx, &s.sql, &s.params)
                    .await?
                    .into_iter()
                    .next(),
                _ => None,
            };
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
        let (columns, _) = self.metadata(table).await?;
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
                Some(&self.database),
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
        let (columns, _) = self.metadata(table).await?;
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
            Some(&self.database),
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
            Some(&self.database),
            table,
            &self.show_create(table).await?,
        )?;
        let exprs = swiss_host::dbbrowser::quoted_exprs(DbDialect::Mysql, &names)?;
        // The producer task holds one chunk of source rows and one statement under
        // construction — never the table. The bounded channel (2 pieces) hands each finished
        // statement to the route as the socket takes it, so a slow client throttles the dump
        // instead of growing it.
        let (tx, rx) = tokio::sync::mpsc::channel::<DumpPiece>(2);
        let dump_columns = names.clone();
        let conn = self.conn.clone();
        let database = self.database.clone();
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
                    let literals: Vec<String> =
                        names.iter().map(|c| sql_literal(row.get(c))).collect();
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
            let _ = tx.send(Ok(sql_dump_foot(DbDialect::Mysql).as_bytes().to_vec())).await;
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
        let (columns, primary) = self.metadata(table).await?;
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
        Ok(json!({ "ran": sql }))
    }
}
