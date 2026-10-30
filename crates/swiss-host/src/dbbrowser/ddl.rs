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

//! Table structure from the catalogs, and the minimal DDL set (SPEC §data.ddl).

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use super::export::sql_dump_literal;
use super::ident::{qualified, quote_ident};
use super::model::{BrowseColumn, BrowseForeignKey, BrowseIndex};
use super::values::{js_number, js_to_string};
use super::DbDialect;

// --- table structure -----------------------------------------------------------------------------

/// Shape raw information_schema column rows into BrowseColumn, marking PK membership.
pub fn to_browse_columns(rows: &[Map<String, Value>], pk: &[String]) -> Vec<BrowseColumn> {
    let pk_set: HashSet<&str> = pk.iter().map(String::as_str).collect();
    rows.iter()
        .map(|r| {
            let name = js_to_string(r.get("column_name"));
            BrowseColumn {
                is_primary_key: pk_set.contains(name.as_str()),
                data_type: js_to_string(r.get("data_type")),
                nullable: js_to_string(r.get("is_nullable")).to_uppercase() == "YES",
                default_value: r
                    .get("column_default")
                    .filter(|v| !v.is_null())
                    .map(|v| js_to_string(Some(v))),
                // A blank comment is no comment — the panel's tooltip stays empty, not " ".
                comment: r
                    .get("column_comment")
                    .filter(|v| !v.is_null())
                    .map(|v| js_to_string(Some(v)))
                    .filter(|s| !s.is_empty()),
                name,
            }
        })
        .collect()
}

/// Fold raw index rows (one per indexed COLUMN, already aliased: name/unique(0|1)/primary/column,
/// in index order) into one BrowseIndex per index, columns in arrival order.
pub fn to_browse_indexes(rows: &[Map<String, Value>]) -> Vec<BrowseIndex> {
    // Insertion-ordered like the JS Map whose values() the Node build spread out.
    let mut by_name: Vec<(String, BrowseIndex)> = Vec::new();
    for r in rows {
        let name = js_to_string(r.get("name"));
        if !by_name.iter().any(|(n, _)| n == &name) {
            // unique: Number(r.unique) !== 1 — NaN counts as unique, same as the JS comparison.
            let unique = js_number(r.get("unique").unwrap_or(&Value::Null)) != Some(1.0);
            // primary: strict === 1 or === true.
            let primary = match r.get("primary") {
                Some(Value::Number(n)) => n.as_f64() == Some(1.0),
                Some(Value::Bool(b)) => *b,
                _ => false,
            };
            let definition = r
                .get("definition")
                .filter(|v| !v.is_null())
                .map(|v| js_to_string(Some(v)));
            by_name.push((
                name.clone(),
                BrowseIndex {
                    name: name.clone(),
                    unique,
                    primary,
                    columns: Vec::new(),
                    definition,
                },
            ));
        }
        if let Some(col) = r.get("column").filter(|v| !v.is_null()) {
            if let Some((_, idx)) = by_name.iter_mut().find(|(n, _)| n == &name) {
                idx.columns.push(js_to_string(Some(col)));
            }
        }
    }
    by_name.into_iter().map(|(_, idx)| idx).collect()
}

/// Postgres has no SHOW CREATE TABLE. Rather than shell out to pg_dump (not installed, needs a
/// password dance, version-matched), assemble a faithful sketch from the catalog pieces the
/// detail view already fetched. Column types come out as information_schema spells them.
pub fn build_pg_ddl(
    schema: &str,
    table: &str,
    columns: &[BrowseColumn],
    primary_key: &[String],
    foreign_keys: &[BrowseForeignKey],
) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    for c in columns {
        let mut line = format!(
            "    {} {}",
            quote_ident(DbDialect::Pg, &c.name)?,
            c.data_type
        );
        if !c.nullable {
            line.push_str(" NOT NULL");
        }
        if let Some(d) = &c.default_value {
            if !d.is_empty() {
                line.push_str(&format!(" DEFAULT {d}"));
            }
        }
        lines.push(line);
    }
    if !primary_key.is_empty() {
        let cols = primary_key
            .iter()
            .map(|c| quote_ident(DbDialect::Pg, c))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!("    PRIMARY KEY ({})", cols.join(", ")));
    }
    for fk in foreign_keys {
        lines.push(format!(
            "    FOREIGN KEY ({}) REFERENCES {}.{} ({})",
            quote_ident(DbDialect::Pg, &fk.column)?,
            quote_ident(DbDialect::Pg, &fk.ref_schema)?,
            quote_ident(DbDialect::Pg, &fk.ref_table)?,
            quote_ident(DbDialect::Pg, &fk.ref_column)?
        ));
    }
    // Lines join with their own commas — the last one must not carry one into the closing
    // paren, or the sketch (and any SQL dump that replays it, SPEC §data.export) is invalid SQL.
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", lines.join(",\n"))
    };
    let qualified = format!(
        "{}.{}",
        quote_ident(DbDialect::Pg, schema)?,
        quote_ident(DbDialect::Pg, table)?
    );
    let mut out = format!("CREATE TABLE {qualified} ({body});");
    // Postgres has no COMMENT clause inside CREATE TABLE the way MySQL does - a comment is its
    // own statement afterwards. A sketch that stopped at the paren silently dropped every
    // comment the catalog holds (the owner, 2026-09-28), and a dump replaying it lost them.
    for c in columns {
        let Some(text) = c.comment.as_ref().filter(|t| !t.is_empty()) else {
            continue;
        };
        out.push_str(&format!(
            "\nCOMMENT ON COLUMN {qualified}.{} IS {};",
            quote_ident(DbDialect::Pg, &c.name)?,
            sql_dump_literal(DbDialect::Pg, Some(&Value::String(text.clone())))?
        ));
    }
    Ok(out)
}

/// The statement behind a structure operation. Every identifier passes assert_ident + dialect
/// quoting on the way in (these statements bind nothing — the names ARE the payload).
pub fn build_ddl_op_sql(
    dialect: DbDialect,
    op: &str,
    schema: Option<&str>,
    table: &str,
    to: Option<&str>,
) -> Result<String, String> {
    match op {
        "rename" => {
            let Some(to) = to.filter(|t| !t.is_empty()) else {
                return Err("rename needs the new table name".into());
            };
            if dialect == DbDialect::Mysql {
                // Same-schema rename only — the panel never offered a cross-schema one.
                let target = match schema.filter(|s| !s.is_empty()) {
                    Some(s) => qualified(dialect, Some(s), to)?,
                    None => quote_ident(dialect, to)?,
                };
                Ok(format!(
                    "RENAME TABLE {} TO {}",
                    qualified(dialect, schema, table)?,
                    target
                ))
            } else {
                Ok(format!(
                    "ALTER TABLE {} RENAME TO {}",
                    qualified(dialect, schema, table)?,
                    quote_ident(dialect, to)?
                ))
            }
        }
        "truncate" => Ok(format!(
            "TRUNCATE TABLE {}",
            qualified(dialect, schema, table)?
        )),
        "drop" => Ok(format!("DROP TABLE {}", qualified(dialect, schema, table)?)),
        other => Err(format!("unknown structure operation: {other}")),
    }
}

// --- the W4.6 minimal DDL set -----------------------------------------------------------------------
//
// Exactly three operations: CREATE TABLE, ADD COLUMN, CREATE INDEX (SPEC §data.ddl; the visual
// designer stays out on purpose, §9). One builder serves BOTH the panel's live preview
// (POST /api/db/:name/ddl-preview) and the commit (POST /api/db/:name/ddl) — pgAdmin's msql
// flow: preview and save share one SQL producer, so what the sheet showed is byte-for-byte
// what Commit runs. Identifiers ride the same assert_ident + quote_ident gate every other
// interpolated name uses. Types and defaults are the operator's own SQL text: the preview
// shows them verbatim, and vet_ddl_type / vet_ddl_default only refuse what could break the
// statement out of its one line — the console exists for the exotic DDL.

/// One column of a W4.6 form. The five facts the Columns tab already shows; `default` is raw
/// SQL text (`0`, `''`, `now()`) and `comment` free text, quoted as a literal here.
pub struct DdlColumn {
    pub name: String,
    pub type_: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
}

/// Columns one form may carry — a bound on a hand-written request, in MAX_EDITS' spirit.
const MAX_DDL_COLUMNS: usize = 128;

/// One comment's ceiling, in BYTES — MySQL's own COMMENT cap is 1024 bytes and it rejects a
/// longer one server-side, halfway through a CREATE. Refusing at the form's parse layer
/// keeps the error in the form's language; Postgres's COMMENT ON is unbounded but one form
/// serves both dialects, so the stricter cap wins.
const MAX_DDL_COMMENT: usize = 1024;

/// Key parts an index (or primary key) may name — MySQL's own ceiling, so one builder fits
/// both dialects.
const MAX_DDL_KEY_PARTS: usize = 16;

/// A type spelling the forms accept: letters, digits, spaces and the punctuation real types
/// need (`varchar(100)`, `numeric(10,2)`, `enum('a','b')`, `int[]`, `double precision`).
/// Anything that could break out of the column definition is refused.
fn vet_ddl_type(t: &str) -> Result<&str, String> {
    let t = t.trim();
    let ok = !t.is_empty()
        && t.len() <= 128
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, ' ' | '_' | '(' | ')' | ',' | '\'' | '"' | '[' | ']')
        });
    if ok {
        Ok(t)
    } else {
        Err(format!("not a valid column type: {t}"))
    }
}

/// A DEFAULT expression the forms accept: any printable one-line SQL expression. Statement
/// breaks (`;`), comment starters (`--`, `#`, `/*`) and MySQL's identifier quote are
/// refused so every generated statement stays exactly one line — what the preview shows is
/// what runs.
fn vet_ddl_comment(c: &str) -> Result<&str, String> {
    if c.len() > MAX_DDL_COMMENT {
        return Err(format!(
            "a comment may hold at most {MAX_DDL_COMMENT} bytes (this one is {}) - trim it",
            c.len()
        ));
    }
    Ok(c)
}

fn vet_ddl_default(d: &str) -> Result<&str, String> {
    let d = d.trim();
    let ok = d.len() <= 256
        && !d.contains(';')
        && !d.contains('\x60')
        && !d.contains("--")
        && !d.contains('#')
        && !d.contains("/*")
        && !d.chars().any(|c| c.is_ascii_control());
    if ok {
        Ok(d)
    } else {
        Err(format!("not a valid default: {d}"))
    }
}

/// `name type [NOT NULL] [DEFAULT x] [COMMENT 'y']` — MySQL carries the comment inline, so
/// this is the whole column clause there; Postgres gets its COMMENT ON statements separately.
fn ddl_column_clause(dialect: DbDialect, c: &DdlColumn) -> Result<String, String> {
    let mut s = format!("{} {}", quote_ident(dialect, &c.name)?, c.type_);
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = &c.default {
        s.push_str(&format!(" DEFAULT {d}"));
    }
    if dialect == DbDialect::Mysql {
        if let Some(cm) = c.comment.as_deref().filter(|s| !s.is_empty()) {
            s.push_str(&format!(
                " COMMENT {}",
                sql_dump_literal(dialect, Some(&json!(cm)))?
            ));
        }
    }
    Ok(s)
}

/// Postgres keeps comments outside the definition (COMMENT ON), so the create gains follow-up
/// statements; MySQL already embedded them and gets none.
fn ddl_comment_statements(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    columns: &[DdlColumn],
    table_comment: Option<&str>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    if dialect != DbDialect::Pg {
        return Ok(());
    }
    let target = qualified(dialect, schema, table)?;
    if let Some(cm) = table_comment.filter(|s| !s.is_empty()) {
        out.push(format!(
            "COMMENT ON TABLE {target} IS {}",
            sql_dump_literal(dialect, Some(&json!(cm)))?
        ));
    }
    for c in columns {
        if let Some(cm) = c.comment.as_deref().filter(|s| !s.is_empty()) {
            out.push(format!(
                "COMMENT ON COLUMN {target}.{} IS {}",
                quote_ident(dialect, &c.name)?,
                sql_dump_literal(dialect, Some(&json!(cm)))?
            ));
        }
    }
    Ok(())
}

/// CREATE TABLE (plus Postgres COMMENT ON statements). The body follows build_pg_ddl's shape:
/// one clause per line, four-space indented, so the DDL tab's alignment sees what it knows.
pub fn table_ddl(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    columns: &[DdlColumn],
    primary: &[String],
    comment: Option<&str>,
) -> Result<Vec<String>, String> {
    if columns.is_empty() {
        return Err("a table needs at least one column".into());
    }
    let schema = schema.filter(|s| !s.is_empty());
    let target = qualified(dialect, schema, table)?;
    let mut lines: Vec<String> = Vec::with_capacity(columns.len() + 1);
    for c in columns {
        lines.push(format!("    {}", ddl_column_clause(dialect, c)?));
    }
    if !primary.is_empty() {
        if primary.len() > MAX_DDL_KEY_PARTS {
            return Err(format!(
                "a primary key may name at most {MAX_DDL_KEY_PARTS} columns"
            ));
        }
        let cols = primary
            .iter()
            .map(|c| quote_ident(dialect, c))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!("    PRIMARY KEY ({})", cols.join(", ")));
    }
    let mut stmt = format!("CREATE TABLE {target} (\n{}\n)", lines.join(",\n"));
    if dialect == DbDialect::Mysql {
        if let Some(cm) = comment.filter(|s| !s.is_empty()) {
            stmt.push_str(&format!(
                " COMMENT={}",
                sql_dump_literal(dialect, Some(&json!(cm)))?
            ));
        }
    }
    let mut out = vec![stmt];
    ddl_comment_statements(dialect, schema, table, columns, comment, &mut out)?;
    Ok(out)
}

/// ALTER TABLE ... ADD COLUMN — one statement however many columns the form added (both
/// dialects accept a comma-joined action list, and one ALTER is atomic on MySQL).
pub fn column_ddl(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    columns: &[DdlColumn],
) -> Result<Vec<String>, String> {
    if columns.is_empty() {
        return Err("no columns to add".into());
    }
    let schema = schema.filter(|s| !s.is_empty());
    let target = qualified(dialect, schema, table)?;
    let adds = columns
        .iter()
        .map(|c| Ok(format!("ADD COLUMN {}", ddl_column_clause(dialect, c)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let mut out = vec![format!("ALTER TABLE {target} {}", adds.join(", "))];
    ddl_comment_statements(dialect, schema, table, columns, None, &mut out)?;
    Ok(out)
}

/// CREATE [UNIQUE] INDEX over the table's existing columns.
pub fn index_ddl(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    index: &str,
    columns: &[String],
    unique: bool,
) -> Result<Vec<String>, String> {
    if columns.is_empty() {
        return Err("an index needs at least one column".into());
    }
    if columns.len() > MAX_DDL_KEY_PARTS {
        return Err(format!(
            "an index may name at most {MAX_DDL_KEY_PARTS} columns"
        ));
    }
    let schema = schema.filter(|s| !s.is_empty());
    let target = qualified(dialect, schema, table)?;
    let cols = columns
        .iter()
        .map(|c| quote_ident(dialect, c))
        .collect::<Result<Vec<_>, _>>()?;
    let unique = if unique { "UNIQUE " } else { "" };
    Ok(vec![format!(
        "CREATE {unique}INDEX {} ON {target} ({})",
        quote_ident(dialect, index)?,
        cols.join(", ")
    )])
}

/// The mini-grid's rows: name/type/nullable/default/comment, duplicates refused.
fn parse_ddl_columns(o: &Value) -> Result<Vec<DdlColumn>, String> {
    let arr = o
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| "columns must be an array".to_string())?;
    if arr.is_empty() {
        return Err("a form needs at least one column".into());
    }
    if arr.len() > MAX_DDL_COLUMNS {
        return Err(format!("at most {MAX_DDL_COLUMNS} columns per form"));
    }
    let mut out = Vec::with_capacity(arr.len());
    let mut seen: Vec<&str> = Vec::with_capacity(arr.len());
    for c in arr {
        let name = c
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "each column needs a name".to_string())?;
        if seen.contains(&name) {
            return Err(format!("duplicate column name: {name}"));
        }
        seen.push(name);
        let type_ = vet_ddl_type(c.get("type").and_then(Value::as_str).unwrap_or(""))?.to_string();
        let nullable = c.get("nullable").and_then(Value::as_bool).unwrap_or(true);
        let default = match c.get("default").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() => Some(vet_ddl_default(d)?.to_string()),
            _ => None,
        };
        let comment = c
            .get("comment")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(vet_ddl_comment)
            .transpose()?
            .map(str::to_string);
        out.push(DdlColumn {
            name: name.to_string(),
            type_,
            nullable,
            default,
            comment,
        });
    }
    Ok(out)
}

fn parse_str_list(o: &Value, k: &str) -> Result<Vec<String>, String> {
    let arr = o
        .get(k)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{k} must be an array"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{k} must hold strings"))
        })
        .collect()
}

/// Parse one W4.6 form payload into its statements. `o` is the flattened browser object the
/// /ddl route forwards ({op, table, schema?, ...}); /ddl-preview calls this same function,
/// which is what makes preview and commit one code path.
pub fn build_ddl_create(dialect: DbDialect, op: &str, o: &Value) -> Result<Vec<String>, String> {
    let table = o
        .get("table")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "table is required".to_string())?;
    let schema = o
        .get("schema")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match op {
        "create_table" => table_ddl(
            dialect,
            schema,
            table,
            &parse_ddl_columns(o)?,
            &if o.get("primary").is_some() {
                parse_str_list(o, "primary")?
            } else {
                Vec::new()
            },
            o.get("comment")
                .and_then(Value::as_str)
                .map(vet_ddl_comment)
                .transpose()?,
        ),
        "add_column" => column_ddl(dialect, schema, table, &parse_ddl_columns(o)?),
        "create_index" => {
            let index = o
                .get("index")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "index is required".to_string())?;
            index_ddl(
                dialect,
                schema,
                table,
                index,
                &parse_str_list(o, "columns")?,
                o.get("unique").and_then(Value::as_bool).unwrap_or(false),
            )
        }
        other => Err(format!("unknown ddl operation: {other}")),
    }
}

/// The script form the preview shows and /ddl echoes back: every statement on its own line,
/// terminated — the display IS the payload, like the edit bar's SQL preview.
pub fn ddl_script(statements: &[String]) -> String {
    statements
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dbbrowser::test_util::{col, row};

    #[test]
    fn to_browse_columns_marks_pk_and_nullability() {
        let rows = vec![
            row(&[
                ("column_name", json!("id")),
                ("data_type", json!("int")),
                ("is_nullable", json!("NO")),
                ("column_default", Value::Null),
            ]),
            row(&[
                ("column_name", json!("name")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", json!("''")),
            ]),
        ];
        let cols = to_browse_columns(&rows, &["id".to_string()]);
        assert_eq!(cols[0].name, "id");
        assert!(!cols[0].nullable);
        assert!(cols[0].is_primary_key);
        assert_eq!(cols[1].name, "name");
        assert!(cols[1].nullable);
        assert!(!cols[1].is_primary_key);
        assert_eq!(cols[1].default_value.as_deref(), Some("''"));
    }

    #[test]
    fn to_browse_columns_carries_comments() {
        let rows = vec![
            row(&[
                ("column_name", json!("id")),
                ("data_type", json!("bigint")),
                ("is_nullable", json!("NO")),
                ("column_default", Value::Null),
                ("column_comment", json!("Snowflake-style row ID")),
            ]),
            row(&[
                ("column_name", json!("name")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", Value::Null),
                ("column_comment", json!("")),
            ]),
            row(&[
                ("column_name", json!("note")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", Value::Null),
            ]),
        ];
        let cols = to_browse_columns(&rows, &["id".to_string()]);
        assert_eq!(cols[0].comment.as_deref(), Some("Snowflake-style row ID"));
        assert!(cols[1].comment.is_none());
        assert!(cols[2].comment.is_none());
    }

    #[test]
    fn to_browse_indexes_folds_per_column_rows() {
        let rows = vec![
            row(&[
                ("name", json!("PRIMARY")),
                ("unique", json!(0)),
                ("primary", json!(1)),
                ("column", json!("id")),
            ]),
            row(&[
                ("name", json!("name_idx")),
                ("unique", json!(1)),
                ("primary", json!(0)),
                ("column", json!("name")),
            ]),
            row(&[
                ("name", json!("name_idx")),
                ("unique", json!(1)),
                ("primary", json!(0)),
                ("column", json!("email")),
            ]),
        ];
        let idx = to_browse_indexes(&rows);
        assert_eq!(idx.len(), 2);
        let pk = idx.iter().find(|i| i.name == "PRIMARY").unwrap();
        assert!(pk.unique);
        assert!(pk.primary);
        assert_eq!(pk.columns, vec!["id"]);
        let other = idx.iter().find(|i| i.name == "name_idx").unwrap();
        assert!(!other.unique);
        assert!(!other.primary);
        assert_eq!(other.columns, vec!["name", "email"]);
        assert!(other.definition.is_none());
    }

    #[test]
    fn build_pg_ddl_sketches_the_catalog() {
        let cols = vec![
            col("id", "bigint", false, true),
            BrowseColumn {
                default_value: Some("''::text".into()),
                ..col("email", "text", false, false)
            },
            col("note", "text", true, false),
        ];
        let fks = vec![BrowseForeignKey {
            name: "owner_fk".into(),
            column: "id".into(),
            ref_schema: "public".into(),
            ref_table: "users".into(),
            ref_column: "id".into(),
        }];
        let ddl = build_pg_ddl("public", "things", &cols, &["id".to_string()], &fks).unwrap();
        assert!(ddl.contains("CREATE TABLE \"public\".\"things\""));
        assert!(ddl.contains("\"id\" bigint NOT NULL"));
        assert!(ddl.contains("\"email\" text NOT NULL DEFAULT ''::text"));
        assert!(ddl.contains("\"note\" text"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        assert!(ddl.contains("FOREIGN KEY (\"id\") REFERENCES \"public\".\"users\" (\"id\")"));
        // SPEC §data.export live-verify caught this once: a trailing comma after the last clause
        // made the sketch (and any SQL dump that replays it) invalid SQL. The tail must be
        // the clause, then the paren — never a comma between them.
        assert!(
            ddl.ends_with("    FOREIGN KEY (\"id\") REFERENCES \"public\".\"users\" (\"id\")\n);")
        );
    }

    #[test]
    fn build_pg_ddl_states_the_comments_postgres_keeps_apart() {
        // The owner (2026-09-28): "the PG DDL has no COMMENT in it - postgres adds them
        // afterwards, so the PG DDL needs its own handling." MySQL's SHOW CREATE TABLE carries
        // a column's COMMENT inline; postgres has no such clause, so a sketch without the
        // COMMENT ON statements silently drops what the catalog holds.
        let cols = vec![
            BrowseColumn {
                comment: Some("Who placed it".into()),
                ..col("id", "bigint", false, true)
            },
            BrowseColumn {
                comment: Some("today's price, not the order's".into()),
                ..col("price", "numeric", false, false)
            },
            col("plain", "text", true, false),
        ];
        let ddl = build_pg_ddl("shop", "orders", &cols, &["id".to_string()], &[]).unwrap();
        let (create, comments) = ddl
            .split_once(";\n")
            .expect("the create, then the comments");
        assert!(create.starts_with("CREATE TABLE \"shop\".\"orders\""));
        assert!(!create.contains("COMMENT"), "never inline: {create}");
        assert_eq!(
            comments.lines().collect::<Vec<_>>(),
            vec![
                "COMMENT ON COLUMN \"shop\".\"orders\".\"id\" IS 'Who placed it';",
                "COMMENT ON COLUMN \"shop\".\"orders\".\"price\" IS 'today''s price, not the order''s';",
            ],
            "one statement per commented column, quotes doubled, the bare column skipped"
        );
    }

    #[test]
    fn build_pg_ddl_omits_empty_pk_and_fk_clauses() {
        let ddl = build_pg_ddl("s", "t", &[col("x", "int", true, false)], &[], &[]).unwrap();
        assert!(!ddl.contains("PRIMARY KEY"));
        assert!(!ddl.contains("FOREIGN KEY"));
        assert!(
            !ddl.contains("COMMENT"),
            "no comments, no trailing statements: {ddl}"
        );
    }

    #[test]
    fn ddl_ops_build_dialect_correct_statements() {
        assert_eq!(
            build_ddl_op_sql(
                DbDialect::Mysql,
                "rename",
                Some("app"),
                "users",
                Some("members")
            )
            .unwrap(),
            "RENAME TABLE `app`.`users` TO `app`.`members`"
        );
        assert_eq!(
            build_ddl_op_sql(
                DbDialect::Pg,
                "rename",
                Some("public"),
                "users",
                Some("members")
            )
            .unwrap(),
            "ALTER TABLE \"public\".\"users\" RENAME TO \"members\""
        );
        assert_eq!(
            build_ddl_op_sql(DbDialect::Mysql, "truncate", None, "users", None).unwrap(),
            "TRUNCATE TABLE `users`"
        );
        assert_eq!(
            build_ddl_op_sql(DbDialect::Pg, "drop", Some("s"), "t", None).unwrap(),
            "DROP TABLE \"s\".\"t\""
        );
        let err = build_ddl_op_sql(DbDialect::Mysql, "rename", None, "a", None).unwrap_err();
        assert!(err.contains("new table name"), "{err}");
        let err = build_ddl_op_sql(DbDialect::Mysql, "explode", None, "a", None).unwrap_err();
        assert!(err.contains("unknown structure operation"), "{err}");
    }

    // --- SPEC §data.ddl: the minimal DDL set (form -> one builder -> SQL; preview = commit) -------------

    /// The W4.6 tests' plain column: name/type/nullability only.
    fn ddl_col(name: &str, type_: &str, nullable: bool) -> DdlColumn {
        DdlColumn {
            name: name.into(),
            type_: type_.into(),
            nullable,
            default: None,
            comment: None,
        }
    }

    #[test]
    fn w46_table_ddl_snapshots_both_dialects() {
        let columns = vec![
            DdlColumn {
                name: "id".into(),
                type_: "int".into(),
                nullable: false,
                default: Some("0".into()),
                comment: None,
            },
            DdlColumn {
                name: "name".into(),
                type_: "varchar(100)".into(),
                nullable: true,
                default: None,
                comment: Some("名称".into()),
            },
            // A reserved word rides the same identifier gate as any other name — quoted, not
            // refused, because quoting is what makes it legal.
            DdlColumn {
                name: "order".into(),
                type_: "int".into(),
                nullable: false,
                default: None,
                comment: None,
            },
        ];
        // MySQL: comments ride inline, one statement total.
        assert_eq!(
            table_ddl(
                DbDialect::Mysql,
                Some("app"),
                "cfg",
                &columns,
                &["id".to_string()],
                Some("配置表")
            )
            .unwrap(),
            vec![
                "CREATE TABLE `app`.`cfg` (\n    `id` int NOT NULL DEFAULT 0,\n    `name` varchar(100) COMMENT '名称',\n    `order` int NOT NULL,\n    PRIMARY KEY (`id`)\n) COMMENT='配置表'".to_string()
            ]
        );
        // Postgres: comments are their own statements after the CREATE.
        assert_eq!(
            table_ddl(
                DbDialect::Pg,
                Some("public"),
                "cfg",
                &columns,
                &["id".to_string()],
                Some("配置表")
            )
            .unwrap(),
            vec![
                "CREATE TABLE \"public\".\"cfg\" (\n    \"id\" int NOT NULL DEFAULT 0,\n    \"name\" varchar(100),\n    \"order\" int NOT NULL,\n    PRIMARY KEY (\"id\")\n)".to_string(),
                "COMMENT ON TABLE \"public\".\"cfg\" IS '配置表'".to_string(),
                "COMMENT ON COLUMN \"public\".\"cfg\".\"name\" IS '名称'".to_string()
            ]
        );
        // No schema, no pk, no comments: the bare create, one column.
        assert_eq!(
            table_ddl(
                DbDialect::Mysql,
                None,
                "t",
                &[ddl_col("a", "int", true)],
                &[],
                None
            )
            .unwrap(),
            vec!["CREATE TABLE `t` (\n    `a` int\n)".to_string()]
        );
    }

    #[test]
    fn w46_column_ddl_snapshots_both_dialects() {
        let adds = vec![
            DdlColumn {
                name: "email".into(),
                type_: "varchar(255)".into(),
                nullable: false,
                default: Some("''".into()),
                comment: Some("联系".into()),
            },
            ddl_col("created_at", "timestamp", true),
        ];
        assert_eq!(
            column_ddl(DbDialect::Mysql, Some("app"), "users", &adds).unwrap(),
            vec![
                "ALTER TABLE `app`.`users` ADD COLUMN `email` varchar(255) NOT NULL DEFAULT '' COMMENT '联系', ADD COLUMN `created_at` timestamp".to_string()
            ]
        );
        assert_eq!(
            column_ddl(DbDialect::Pg, None, "users", &adds).unwrap(),
            vec![
                "ALTER TABLE \"users\" ADD COLUMN \"email\" varchar(255) NOT NULL DEFAULT '', ADD COLUMN \"created_at\" timestamp".to_string(),
                "COMMENT ON COLUMN \"users\".\"email\" IS '联系'".to_string()
            ]
        );
    }

    #[test]
    fn w46_index_ddl_snapshots_both_dialects() {
        assert_eq!(
            index_ddl(
                DbDialect::Mysql,
                Some("app"),
                "users",
                "users_name_idx",
                &["name".to_string(), "order".to_string()],
                false
            )
            .unwrap(),
            vec!["CREATE INDEX `users_name_idx` ON `app`.`users` (`name`, `order`)".to_string()]
        );
        assert_eq!(
            index_ddl(
                DbDialect::Pg,
                None,
                "users",
                "users_email_key",
                &["email".to_string()],
                true
            )
            .unwrap(),
            vec!["CREATE UNIQUE INDEX \"users_email_key\" ON \"users\" (\"email\")".to_string()]
        );
    }

    #[test]
    fn w46_build_ddl_create_dispatches_the_panel_payload() {
        let o = json!({
            "table": "cfg",
            "schema": "public",
            "comment": "配置",
            "columns": [
                { "name": "id", "type": "bigint", "nullable": false },
                { "name": "label", "type": "varchar(40)", "default": "''", "comment": "标签" }
            ],
            "primary": ["id"]
        });
        let stmts = build_ddl_create(DbDialect::Pg, "create_table", &o).unwrap();
        assert_eq!(
            stmts[0],
            "CREATE TABLE \"public\".\"cfg\" (\n    \"id\" bigint NOT NULL,\n    \"label\" varchar(40) DEFAULT '',\n    PRIMARY KEY (\"id\")\n)"
        );
        assert!(
            stmts.contains(&"COMMENT ON COLUMN \"public\".\"cfg\".\"label\" IS '标签'".to_string())
        );

        let o = json!({ "table": "t", "columns": [{ "name": "a", "type": "int" }] });
        assert_eq!(
            build_ddl_create(DbDialect::Mysql, "add_column", &o).unwrap(),
            vec!["ALTER TABLE `t` ADD COLUMN `a` int".to_string()]
        );

        let o = json!({ "table": "t", "index": "t_a_idx", "columns": ["a"], "unique": true });
        assert_eq!(
            build_ddl_create(DbDialect::Mysql, "create_index", &o).unwrap(),
            vec!["CREATE UNIQUE INDEX `t_a_idx` ON `t` (`a`)".to_string()]
        );
    }

    #[test]
    fn ddl_comments_cannot_exceed_1024_bytes() {
        // W4.6 audit: nothing capped comment length — MySQL rejects >1024 bytes server-side,
        // halfway through a CREATE. The cap belongs in the form's own vetting, counted in
        // BYTES (that is what the server counts), with room for exactly 1024.
        let long = "\u{e4}".repeat(513); // 513 chars, 1026 bytes
        let form = json!({
            "op": "create_table",
            "table": "t",
            "columns": [{ "name": "id", "type": "int", "comment": long }],
            "comment": "fine",
        });
        let err = build_ddl_create(DbDialect::Mysql, "create_table", &form).unwrap_err();
        assert!(err.contains("1024"), "{err}");
        assert!(err.contains("1026"), "{err}");

        // The table comment rides the same cap.
        let form = json!({
            "op": "create_table",
            "table": "t",
            "columns": [{ "name": "id", "type": "int" }],
            "comment": "x".repeat(1025),
        });
        let err = build_ddl_create(DbDialect::Mysql, "create_table", &form).unwrap_err();
        assert!(err.contains("1024"), "{err}");

        // Exactly 1024 bytes passes on both spots — the boundary is inclusive.
        let form = json!({
            "op": "create_table",
            "table": "t",
            "columns": [{ "name": "id", "type": "int", "comment": "y".repeat(1024) }],
            "comment": "z".repeat(1024),
        });
        assert!(build_ddl_create(DbDialect::Mysql, "create_table", &form).is_ok());
        assert!(build_ddl_create(DbDialect::Pg, "create_table", &form).is_ok());
    }

    #[test]
    fn w46_ddl_refuses_illegal_identifiers_and_payloads() {
        // The identifier whitelist: table, column and index names that are not bare words.
        let bad_col =
            json!({ "table": "t", "columns": [{ "name": "a; DROP TABLE users", "type": "int" }] });
        let err = build_ddl_create(DbDialect::Mysql, "create_table", &bad_col).unwrap_err();
        assert!(err.contains("not a valid MySQL identifier"), "{err}");
        let bad_table =
            json!({ "table": "drop table; --", "columns": [{ "name": "a", "type": "int" }] });
        assert!(build_ddl_create(DbDialect::Pg, "create_table", &bad_table).is_err());
        let bad_index = json!({ "table": "t", "index": "x y", "columns": ["a"] });
        assert!(build_ddl_create(DbDialect::Mysql, "create_index", &bad_index).is_err());
        // A type or default that could break the statement out of its one line.
        let bad_type =
            json!({ "table": "t", "columns": [{ "name": "a", "type": "int; DROP TABLE x" }] });
        let err = build_ddl_create(DbDialect::Pg, "create_table", &bad_type).unwrap_err();
        assert!(err.contains("not a valid column type"), "{err}");
        let bad_default = json!({ "table": "t", "columns": [{ "name": "a", "type": "int", "default": "0; -- done" }] });
        let err = build_ddl_create(DbDialect::Mysql, "create_table", &bad_default).unwrap_err();
        assert!(err.contains("not a valid default"), "{err}");
        // Structural refusals.
        let no_cols = json!({ "table": "t", "columns": [] });
        let err = build_ddl_create(DbDialect::Mysql, "create_table", &no_cols).unwrap_err();
        assert!(err.contains("at least one column"), "{err}");
        let dupes = json!({ "table": "t", "columns": [{ "name": "a", "type": "int" }, { "name": "a", "type": "int" }] });
        let err = build_ddl_create(DbDialect::Pg, "create_table", &dupes).unwrap_err();
        assert!(err.contains("duplicate column"), "{err}");
        let no_index_cols = json!({ "table": "t", "index": "i", "columns": [] });
        let err = build_ddl_create(DbDialect::Mysql, "create_index", &no_index_cols).unwrap_err();
        assert!(err.contains("at least one column"), "{err}");
        let missing = json!({});
        let err = build_ddl_create(DbDialect::Mysql, "add_column", &missing).unwrap_err();
        assert!(err.contains("table is required"), "{err}");
        let unknown = build_ddl_create(DbDialect::Mysql, "drop_database", &json!({ "table": "t" }))
            .unwrap_err();
        assert!(unknown.contains("unknown ddl operation"), "{unknown}");
    }

    #[test]
    fn w46_ddl_script_and_comment_escaping() {
        // The script is what /ddl-preview shows and /ddl echoes back: one statement per
        // line, terminated — the preview IS the payload.
        assert_eq!(ddl_script(&["A".to_string(), "B".to_string()]), "A;\nB;");
        // Comment literals escape the way the dump literal already does (W4.4): MySQL
        // backslash-escapes, Postgres doubles quotes.
        let c = vec![DdlColumn {
            name: "a".into(),
            type_: "int".into(),
            nullable: true,
            default: None,
            comment: Some("it's \\ kept".into()),
        }];
        let my = table_ddl(DbDialect::Mysql, None, "t", &c, &[], None).unwrap();
        assert_eq!(
            my[0],
            "CREATE TABLE `t` (\n    `a` int COMMENT 'it\\'s \\\\ kept'\n)"
        );
        let pg = table_ddl(DbDialect::Pg, None, "t", &c, &[], None).unwrap();
        assert_eq!(pg[1], "COMMENT ON COLUMN \"t\".\"a\" IS 'it''s \\ kept'");
    }
}
