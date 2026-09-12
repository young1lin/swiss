//! Postgres Data-view browser backed by the adapter's shared sqlx pool — the port of the
//! `dbBrowser()` object Node's pg adapter returned. Same capability set as the MySQL browser in
//! this build: list/read/describe plus the console; the buffered-edit grid, CSV export/import
//! and DDL ops answer "not available in this build" (they are ported per dialect from
//! dbbrowser.ts in one go, not piecemeal).

use super::direct::Lazy;
use super::pg::{
    pg_browse_table_params, pg_grammar_params, pg_list_tables_grammar_sql, pg_list_tables_sql,
    pg_query_rows, COUNT_TABLES_SQL, DESCRIBE_SQL, PG_BROWSE_FK_SQL, PG_BROWSE_INDEXES_SQL,
    PK_SQL,
};
use super::sql::{clamp_row_limit, limit_report, with_row_limit};
use async_trait::async_trait;
use swiss_host::dbbrowser::{
    browse_count_sql, browse_offset, browse_order, browse_page_size, browse_rows_sql,
    browse_table_sort, build_ddl_op_sql, build_edit_statements, build_import_statements,
    build_pg_ddl, export_row_limit,
    js_to_string, map_import_rows, readback_plan, sql_dump_foot, sql_dump_head, sql_literal,
    to_browse_columns, to_browse_indexes, to_csv, to_json_lines, BrowseColumn,
    BrowseForeignKey, DbBrowser, DbDialect, DumpPiece, ReadBack, SqlDump, SqlInsertBatch,
    EXPORT_CHUNK, EXPORT_ROW_CAP, IMPORT_ROW_CAP,
};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use std::sync::Arc;

pub struct PgBrowser {
    label: String,
    conn: Arc<Lazy<PgPool>>,
}

impl PgBrowser {
    pub fn new(label: String, conn: Arc<Lazy<PgPool>>) -> Self {
        Self { label, conn }
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Map<String, Value>>, String> {
        let pool = self.conn.get().await?;
        pg_query_rows(&pool, sql, params).await
    }

    /// Columns + primary key of one table — the shared head of read_table and describe_table.
    /// A PK-query failure is swallowed (permission on pg_index): the columns still describe the
    /// table, exactly where Node's metaOf did the same.
    async fn metadata(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<(Vec<BrowseColumn>, Vec<String>), String> {
        if table.is_empty() {
            return Err("table is required".into());
        }
        let params = vec![json!(schema), json!(table)];
        let (columns, primary) = tokio::join!(
            self.query(DESCRIBE_SQL, &params),
            self.query(PK_SQL, &params)
        );
        let columns = columns?;
        if columns.is_empty() {
            return Err(format!("no such table: {schema}.{table}"));
        }
        let primary: Vec<String> = primary
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("column").and_then(Value::as_str).map(str::to_string))
            .collect();
        Ok((to_browse_columns(&columns, &primary), primary))
    }

    /// The catalog-sketch CREATE TABLE and the FK rows it folds in — the shared tail of
    /// describe_table and the SQL dump's head (docs/22 W4.4), built through one helper so the
    /// dump's DDL is by construction the DDL the Structure tab shows.
    async fn ddl_of(
        &self,
        schema: &str,
        table: &str,
        columns: &[BrowseColumn],
        primary: &[String],
    ) -> Result<(String, Vec<BrowseForeignKey>), String> {
        // The FK query needs permission on pg_constraint — a refusal answers [] (Node caught it).
        let fks = self
            .query(PG_BROWSE_FK_SQL, &[json!(schema), json!(table)])
            .await
            .unwrap_or_default();
        let foreign_keys: Vec<BrowseForeignKey> = fks
            .into_iter()
            .map(|r| BrowseForeignKey {
                name: r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                column: r
                    .get("column")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ref_schema: r
                    .get("ref_schema")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ref_table: r
                    .get("ref_table")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ref_column: r
                    .get("ref_column")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        // Postgres has no SHOW CREATE TABLE — a faithful sketch from the catalog (dbbrowser.rs).
        Ok((
            build_pg_ddl(schema, table, columns, primary, &foreign_keys)?,
            foreign_keys,
        ))
    }
}

/// `total` out of a COUNT row — node-pg answers int4 as a number, but the exact-string path
/// (config-driven) can make it a string; both spellings count.
fn total_of(rows: &[Map<String, Value>]) -> i64 {
    match rows.first().and_then(|r| r.get("total")) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

#[async_trait]
impl DbBrowser for PgBrowser {
    fn dialect(&self) -> DbDialect {
        DbDialect::Pg
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
        // docs/22 W1.1: the panel's schema picker narrows the catalog walk server-side; MySQL
        // has no such parameter (one database per connection) and ignores it.
        let schema = o.get("schema").and_then(Value::as_str).map(str::to_string);
        let sort = browse_table_sort(
            o.get("sort").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
        )?;
        // docs/22 W1.6: a grammar grep (comma AND / | OR / * wildcard) expands to multi-LIKE
        // SQL with the LIMIT/OFFSET binds renumbered past the patterns; a plain substring keeps
        // the const statements byte-for-byte.
        let grammar = grep
            .map(|g| swiss_host::dbbrowser::grep_where(DbDialect::Pg, "c.relname", g, 1))
            .transpose()?
            .flatten();
        let (list_sql, count_sql, list_params, count_params) = match grammar {
            Some(w) => {
                let (ls, cs) = pg_list_tables_grammar_sql(sort, &w.frag, w.params.len());
                let (lp, cp) = pg_grammar_params(
                    schema.as_deref(),
                    &w.params,
                    limit,
                    page.saturating_mul(limit),
                );
                (ls, cs, lp, cp)
            }
            None => {
                let filters = pg_browse_table_params(schema.as_deref(), grep);
                let lp = vec![
                    filters[0].clone(),
                    filters[1].clone(),
                    json!(limit),
                    json!(page.saturating_mul(limit)),
                ];
                (
                    pg_list_tables_sql(sort),
                    COUNT_TABLES_SQL.to_string(),
                    lp,
                    filters,
                )
            }
        };
        let (list, count) = tokio::join!(
            self.query(&list_sql, &list_params),
            self.query(&count_sql, &count_params)
        );
        let list = list?;
        let total = total_of(&count?);
        // Node mapped the SQL rows to camelCase (pg.ts:320-326): schema/name/type stringify,
        // approx_rows numbers or null (reltuples is -1 when never analyzed), size is
        // pg_size_pretty's own text — the panel reads t.approxRows / t.size.
        let tables: Vec<Value> = list
            .into_iter()
            .map(|r| {
                json!({
                    "schema": js_to_string(r.get("schema")),
                    "name": js_to_string(r.get("name")),
                    "type": js_to_string(r.get("type")),
                    "approxRows": match r.get("approx_rows") {
                        None | Some(Value::Null) => Value::Null,
                        Some(v) => json!(swiss_host::dbbrowser::js_number(v).map(f64::floor).unwrap_or(0.0)),
                    },
                    "size": js_to_string(r.get("size")),
                })
            })
            .collect();
        let more = page.saturating_mul(limit) + (tables.len() as i64) < total;
        Ok(json!({"tables":tables,"total":total,"page":page,"limit":limit,"more":more}))
    }

    async fn read_table(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.metadata(&schema, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let order = browse_order(
            &names,
            o.get("order").and_then(Value::as_str),
            o.get("dir").and_then(Value::as_str),
            DbDialect::Pg,
        )?;
        let offset = browse_offset(o.get("offset"));
        let limit = browse_page_size(o.get("limit"), 50);
        // The grid's filters feed the page, the COUNT and exports through one WHERE
        // (browse_where) so the three can never drift (docs/22 W0.2).
        let where_ = swiss_host::dbbrowser::browse_where(DbDialect::Pg, &columns, o.get("filters"))?;
        let exprs = swiss_host::dbbrowser::pg_typed_exprs(&columns)?;
        // docs/22 W1.9: fetch one row past the page — its presence answers "is there a next
        // page" without trusting COUNT arithmetic under concurrent writes.
        let rows_stmt = browse_rows_sql(
            DbDialect::Pg,
            Some(&schema),
            table,
            &exprs,
            order.as_deref(),
            offset,
            limit + 1,
            &where_.frag,
            &where_.params,
        )?;
        let count_stmt = browse_count_sql(
            DbDialect::Pg,
            Some(&schema),
            table,
            &where_.frag,
            &where_.params,
        )?;
        let (rows, count) = tokio::join!(
            self.query(&rows_stmt.sql, &rows_stmt.params),
            self.query(&count_stmt.sql, &count_stmt.params)
        );
        let (rows, next_page) = swiss_host::dbbrowser::page_and_next(rows?, limit);
        let total = total_of(&count?);
        let editable = !primary.is_empty();
        // Node's editNote: the pk-less explanation is the only one left (pg.ts).
        let edit_note = if primary.is_empty() {
            Some("table has no primary key, so a row cannot be addressed for edits")
        } else {
            None
        };
        Ok(json!({
            "schema": schema, "table": table, "columns": columns, "rows": rows,
            "total": total, "offset": offset, "limit": limit, "nextPage": next_page,
            "primaryKey": primary, "editable": editable, "editNote": edit_note,
        }))
    }

    async fn describe_table(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let (columns, primary) = self.metadata(&schema, table).await?;
        let params = vec![json!(schema), json!(table)];
        let (indexes, ddl) = tokio::join!(
            self.query(PG_BROWSE_INDEXES_SQL, &params),
            self.ddl_of(&schema, table, &columns, &primary)
        );
        let (ddl, foreign_keys) = ddl?;
        // Fold rows into toBrowseIndexes shape first, exactly where Node's adapter mapped them:
        // a CREATE UNIQUE INDEX says so in its definition; the PK index is flagged by
        // indisprimary itself, not by a <table>_pkey name guess.
        let index_rows: Vec<Map<String, Value>> = indexes?
            .into_iter()
            .map(|r| {
                let definition = r.get("definition").and_then(Value::as_str).unwrap_or("");
                let mut m = Map::new();
                m.insert("name".into(), r.get("name").cloned().unwrap_or(Value::Null));
                m.insert(
                    "unique".into(),
                    json!(if definition.starts_with("CREATE UNIQUE") {
                        0
                    } else {
                        1
                    }),
                );
                m.insert(
                    "primary".into(),
                    r.get("is_primary").cloned().unwrap_or(json!(0)),
                );
                m.insert("column".into(), Value::Null); // the definition already lists the columns
                m.insert("definition".into(), json!(definition));
                m
            })
            .collect();
        let indexes = to_browse_indexes(&index_rows);
        Ok(json!({
            "schema": schema, "table": table, "columns": columns, "primaryKey": primary,
            "indexes": indexes, "foreignKeys": foreign_keys, "ddl": ddl,
        }))
    }

    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        let s = sql.trim();
        if s.is_empty() {
            return Err("sql is required".into());
        }
        // The console's one rule: a single statement per run. Reads and writes alike go through —
        // this console belongs to the panel on the operator's own machine.
        let sql = super::sql::assert_single_statement(s)?;
        let prepared = with_row_limit(&sql, clamp_row_limit(limit, 50));
        // Object form on purpose (Node sent { text, values: [] }): the extended protocol refuses
        // a stacked second statement server-side even if a masked-literal trick ever slips one
        // past the shape check. pg_query (the MCP tool) keeps the string form — multi-statement
        // there is a feature summarize() already reports per result.
        let rows: Vec<Value> = self
            .query(&prepared.sql, &[])
            .await?
            .into_iter()
            .map(Value::Object)
            .collect();
        let columns = rows
            .first()
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        // Driver limitation, accepted: `columns` derives from the first row, so a zero-row
        // SELECT answers columns: [] — node-pg exposed RowDescription for the empty header.
        let row_count = rows.len();
        let mut out = Map::new();
        out.insert("columns".into(), json!(columns));
        out.insert("rows".into(), Value::Array(rows));
        out.insert("rowCount".into(), json!(row_count));
        if let Value::Object(extra) = limit_report(&prepared, row_count as i64, limit) {
            for (k, v) in extra {
                out.insert(k, v);
            }
        }
        Ok(Value::Object(out))
    }

    async fn apply_edits(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        let edits = o.get("edits").and_then(Value::as_array);
        if table.is_empty() {
            return Err("table is required".into());
        }
        let Some(edits) = edits.filter(|e| !e.is_empty()) else {
            return Err("edits must be a non-empty array".into());
        };
        let (columns, primary) = self.metadata(&schema, table).await?;
        let typed: Vec<swiss_host::dbbrowser::BrowseEdit> = edits
            .iter()
            .map(swiss_host::dbbrowser::browse_edit_of)
            .collect::<Result<_, _>>()?;
        let stmts = build_edit_statements(
            DbDialect::Pg,
            Some(&schema),
            table,
            &typed,
            &columns,
            &primary,
        )?;
        // docs/22 W1.7: every update/insert also reads its committed row back inside the same
        // transaction — silent truncation, DEFAULTs and trigger rewrites land on screen instead
        // of the value that was typed. Deletes read nothing back; a plan that cannot address a
        // row comes home null and the commit itself is unaffected.
        let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let pool = self.conn.get().await?;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        let mut results: Vec<Value> = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.iter().enumerate() {
            let plan = readback_plan(
                DbDialect::Pg,
                Some(&schema),
                table,
                &col_names,
                &primary,
                &typed[i],
            );
            let (affected, row) = match &plan {
                ReadBack::Returning(suffix) => {
                    let rows = super::pg::run_pg_tx_rows(
                        &mut tx,
                        &format!("{}{}", stmt.sql, suffix),
                        &stmt.params,
                    )
                    .await?;
                    (rows.len() as u64, rows.into_iter().next())
                }
                _ => {
                    let affected =
                        super::pg::run_pg_tx(&mut tx, &stmt.sql, &stmt.params).await?;
                    let row = match plan {
                        ReadBack::Select(s) => super::pg::run_pg_tx_rows(&mut tx, &s.sql, &s.params)
                            .await?
                            .into_iter()
                            .next(),
                        ReadBack::None => None,
                        ReadBack::Returning(_) => unreachable!("matched above"),
                    };
                    (affected, row)
                }
            };
            results.push(json!({"op": typed[i].op(), "affected": affected, "row": row}));
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(json!({ "results": results }))
    }

    async fn export_table(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let (columns, _) = self.metadata(&schema, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let cap = export_row_limit(o.get("limit"));
        // An export of a filtered grid exports the FILTERED set: the same browse_where the
        // page and its COUNT use (docs/22 W0.2), values bound, never inlined.
        let where_ = swiss_host::dbbrowser::browse_where(DbDialect::Pg, &columns, o.get("filters"))?;
        let mut all: Vec<Map<String, Value>> = Vec::new();
        let mut capped = false;
        // Offset paging in chunks: simple, and the cap keeps the O(offset) tail-walk bounded.
        let mut offset = 0i64;
        let exprs = swiss_host::dbbrowser::pg_typed_exprs(&columns)?;
        while offset < cap {
            let chunk = EXPORT_CHUNK.min(cap - offset);
            let stmt = browse_rows_sql(
                DbDialect::Pg,
                Some(&schema),
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
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let (columns, primary) = self.metadata(&schema, table).await?;
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let cap = export_row_limit(o.get("limit"));
        // The dump exports the FILTERED set: the same browse_where the page, the COUNT and the
        // folded exports use (docs/22 W0.2), values bound, never inlined.
        let where_ = swiss_host::dbbrowser::browse_where(DbDialect::Pg, &columns, o.get("filters"))?;
        // Header facts come from that same WHERE's COUNT, so rows and capped are true before
        // the first byte ships (the folded formats learn their row count only after fetching;
        // a writer racing the stream can still move the table under a taken count — inherent).
        let count_stmt = browse_count_sql(
            DbDialect::Pg,
            Some(&schema),
            table,
            &where_.frag,
            &where_.params,
        )?;
        let count = total_of(&self.query(&count_stmt.sql, &count_stmt.params).await?);
        let rows = count.min(cap);
        let capped = count > rows;
        let (ddl, _) = self.ddl_of(&schema, table, &columns, &primary).await?;
        let head = sql_dump_head(DbDialect::Pg, Some(&schema), table, &ddl)?;
        let exprs = swiss_host::dbbrowser::pg_typed_exprs(&columns)?;
        // The producer task holds one chunk of source rows and one statement under
        // construction — never the table. The bounded channel (2 pieces) hands each finished
        // statement to the route as the socket takes it, so a slow client throttles the dump
        // instead of growing it.
        let (tx, rx) = tokio::sync::mpsc::channel::<DumpPiece>(2);
        let dump_columns = names.clone();
        let conn = self.conn.clone();
        let table = table.to_string();
        tokio::spawn(async move {
            if tx.send(Ok(head.into_bytes())).await.is_err() {
                return; // the consumer is gone; stop fetching
            }
            let mut batch = match SqlInsertBatch::new(DbDialect::Pg, Some(&schema), &table, &names) {
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
                    DbDialect::Pg,
                    Some(&schema),
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
                    super::pg::pg_query_rows(&pool, &stmt.sql, &stmt.params).await
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
            let _ = tx.send(Ok(sql_dump_foot(DbDialect::Pg).as_bytes().to_vec())).await;
        });
        Ok(SqlDump {
            columns: dump_columns,
            rows,
            capped,
            body: rx,
        })
    }

    async fn import_table(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
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
        // docs/22 W4.5: upsert appends ON CONFLICT (pk) DO UPDATE per row — a table without
        // a primary key cannot target a conflict on Postgres, so the builder degrades to the
        // plain insert statements and says so in the reply; insert mode stays byte-identical
        // to the edit grid's insert arm. One transaction either way.
        let mode = swiss_host::dbbrowser::parse_import_mode(o.get("mode"))?;
        let (columns, primary) = self.metadata(&schema, table).await?;
        let (stmts, degraded) = build_import_statements(
            DbDialect::Pg,
            Some(&schema),
            table,
            &rows,
            &columns,
            &primary,
            mode,
        )?;
        let pool = self.conn.get().await?;
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for stmt in &stmts {
            super::pg::run_pg_tx(&mut tx, &stmt.sql, &stmt.params).await?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        if let Some(note) = degraded {
            return Ok(json!({ "inserted": rows.len(), "mode": "insert", "note": note }));
        }
        Ok(json!({ "inserted": rows.len() }))
    }

    async fn ddl_op(&self, o: &Value) -> Result<Value, String> {
        let schema = o
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table = o.get("table").and_then(Value::as_str).unwrap_or("");
        if table.is_empty() {
            return Err("table is required".into());
        }
        let to = o
            .get("to")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        let sql = build_ddl_op_sql(
            DbDialect::Pg,
            o.get("op").and_then(Value::as_str).unwrap_or(""),
            Some(&schema),
            table,
            to,
        )?;
        let pool = self.conn.get().await?;
        super::pg::pg_query_rows(&pool, &sql, &[]).await?;
        Ok(json!({ "ran": sql }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn a_count_reads_the_same_whichever_spelling_the_driver_used() {
        // node-pg answers int4 as a number, but the exact-string path — which is config-driven,
        // so it is a per-install difference, not a per-query one — makes it a string. The row
        // count drives the panel's paging, so reading one spelling and not the other silently
        // reports every table as empty on half the installs.
        assert_eq!(total_of(&[row(&[("total", json!(42))])]), 42);
        assert_eq!(total_of(&[row(&[("total", json!("42"))])]), 42);
        assert_eq!(total_of(&[row(&[("total", json!(" 42 "))])]), 42);
        // Past 2^31: a table with more rows than an i32 holds is exactly when this matters.
        assert_eq!(
            total_of(&[row(&[("total", json!("4294967296"))])]),
            4_294_967_296
        );
    }

    #[test]
    fn a_missing_or_unreadable_count_is_zero_rather_than_a_panic() {
        // COUNT always returns a row, so these are the shapes of a query that went wrong — and a
        // browser that panics on a malformed reply takes the whole gateway task with it.
        assert_eq!(total_of(&[]), 0);
        assert_eq!(total_of(&[row(&[])]), 0);
        assert_eq!(
            total_of(&[row(&[("count", json!(7))])]),
            0,
            "the column is named total"
        );
        assert_eq!(total_of(&[row(&[("total", json!(null))])]), 0);
        assert_eq!(total_of(&[row(&[("total", json!("not a number"))])]), 0);
        assert_eq!(total_of(&[row(&[("total", json!(true))])]), 0);
    }

    #[test]
    fn only_the_first_row_counts() {
        // COUNT(*) answers exactly one row; a second one would mean the query was not the count.
        assert_eq!(
            total_of(&[row(&[("total", json!(1))]), row(&[("total", json!(999))])]),
            1
        );
    }
}
