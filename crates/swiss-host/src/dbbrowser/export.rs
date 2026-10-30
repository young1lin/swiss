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

//! Export: CSV and JSON lines, SQL literals, and the streaming SQL dump (SPEC §data.export).

use serde_json::{Map, Value};

use super::ident::{qualified, quote_ident};
use super::values::{js_to_string, number_to_js_string};
use super::DbDialect;

/// Hard ceiling on one export. A table bigger than this needs a filtered query, not a download —
/// the browser would happily try to hold a 4 GB string.
pub const EXPORT_ROW_CAP: i64 = 100_000;
/// Rows fetched per chunk while paging through an export.
pub const EXPORT_CHUNK: i64 = 5_000;
/// The flush threshold of a streaming SQL dump (SPEC §data.export) — adminer's max_packet number
/// (`adminer.inc.php:982`): rows accumulate into one multi-value INSERT until the statement
/// would cross this size, then the statement ships whole and a fresh one opens. A single row
/// larger than the threshold is never split — it ships alone, exactly as adminer let it.
pub const EXPORT_SQL_MAX_PACKET: usize = 1_048_576;

// --- full-table export ---------------------------------------------------------------------------

/// Fold fetched rows into the final CSV text (RFC 4180, header row included).
pub fn to_csv(columns: &[String], rows: &[Map<String, Value>]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(rows.len() + 1);
    out.push(
        columns
            .iter()
            .map(|c| csv_escape(Some(&Value::String(c.clone()))))
            .collect::<Vec<_>>()
            .join(","),
    );
    for r in rows {
        out.push(
            columns
                .iter()
                .map(|c| csv_escape(r.get(c)))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    out.join("\r\n")
}

/// Fold fetched rows into newline-delimited JSON (one object per line — splittable, streamable).
pub fn to_json_lines(rows: &[Map<String, Value>]) -> String {
    rows.iter()
        .map(|r| Value::Object(r.clone()).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

// --- copy-out helpers ----------------------------------------------------------------------------

/// One CSV cell, quoted per RFC 4180: quotes doubled, embedded separators/newlines survive.
/// Null and absent are the empty (unquoted) cell; an object becomes compact JSON, whose quotes
/// double like any other quote inside the cell.
pub fn csv_escape(v: Option<&Value>) -> String {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return String::new();
    };
    let s = match v {
        Value::Object(_) | Value::Array(_) => v.to_string(),
        other => js_to_string(Some(other)),
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// One SQL literal for a DUMP BODY — the dump is EXECUTED on replay (SPEC §data.export), so the
/// escaping has to hold as real SQL, not just as display. The clipboard's sql_literal only
/// doubles quotes and must never be used here: a trailing backslash in a MySQL value swallowed
/// the closing quote and let an ordinary string break (or inject into) the replay — an audit
/// blocker. MySQL escapes exactly what adminer's real_escape_string escapes
/// ("drivers/mysql.inc.php" q(): backslash, single quote, double quote, LF, CR, NUL, Ctrl-Z);
/// Postgres doubles single quotes (its text type holds every other byte raw) and REFUSES a
/// NUL — emitting the raw byte would corrupt the dump, and a clear error lets the caller stop.
pub fn sql_dump_literal(dialect: DbDialect, v: Option<&Value>) -> Result<String, String> {
    let Some(v) = v else {
        return Ok("NULL".into());
    };
    match v {
        Value::Null => Ok("NULL".into()),
        Value::Number(n) => Ok(number_to_js_string(n)),
        Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        other => {
            let s = js_to_string(Some(other));
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            match dialect {
                DbDialect::Mysql => {
                    for c in s.chars() {
                        match c {
                            '\\' => out.push_str("\\\\"),
                            '\'' => out.push_str("\\'"),
                            '"' => out.push_str("\\\""),
                            '\n' => out.push_str("\\n"),
                            '\r' => out.push_str("\\r"),
                            '\0' => out.push_str("\\0"),
                            '\u{1a}' => out.push_str("\\Z"),
                            _ => out.push(c),
                        }
                    }
                }
                DbDialect::Pg => {
                    if s.contains('\0') {
                        return Err(
                            "a Postgres text value holds a NUL byte — the dump cannot express it"
                                .into(),
                        );
                    }
                    for c in s.chars() {
                        if c == '\'' {
                            out.push('\'');
                        }
                        out.push(c);
                    }
                }
            }
            out.push('\'');
            Ok(out)
        }
    }
}

/// One SQL literal for the clipboard — never executed, so inlining is safe here. Text that
/// WILL be executed (a dump body, a replayed statement) belongs to sql_dump_literal instead.
pub fn sql_literal(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "NULL".into();
    };
    match v {
        Value::Null => "NULL".into(),
        Value::Number(n) => number_to_js_string(n),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        other => {
            let s = js_to_string(Some(other));
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            for c in s.chars() {
                if c == '\'' {
                    out.push('\'');
                }
                out.push(c);
            }
            out.push('\'');
            out
        }
    }
}

/// INSERT INTO t (a, b) VALUES (...); — mirrors build_edit_statements identifier rules
/// (each column vetted + dialect-quoted) so what you copy matches what Commit would run.
pub fn to_insert_statement(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    column_names: &[String],
    row: &Map<String, Value>,
) -> Result<String, String> {
    // row[c] !== undefined — a present-but-null value copies as NULL; only absent keys drop.
    let cols: Vec<&String> = column_names
        .iter()
        .filter(|c| row.contains_key(c.as_str()))
        .collect();
    if cols.is_empty() {
        return Err("the row has no values to copy".into());
    }
    let names = cols
        .iter()
        .map(|c| quote_ident(dialect, c))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let values = cols
        .iter()
        .map(|c| sql_literal(row.get(c.as_str())))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "INSERT INTO {} ({names}) VALUES ({values});",
        qualified(dialect, schema, table)?
    ))
}

// --- streaming SQL dump (SPEC §data.export) ------------------------------------------------------------

/// Accumulates multi-value INSERT rows for a streaming SQL dump, adminer's dumpData shape
/// (`adminer.inc.php:1007-1070`): one `INSERT INTO … VALUES` prefix, each row appended behind
/// a newline, the statement flushed whole the moment one more row would cross
/// [`EXPORT_SQL_MAX_PACKET`]. Nothing but the statement under construction is held, so a dump
/// of any capped size costs O(chunk), never O(table).
pub struct SqlInsertBatch {
    prefix: String,
    buffer: String,
    /// prefix + buffer — adminer's strlen($buffer), which always carried its own prefix.
    used: usize,
}

impl SqlInsertBatch {
    /// The prefix vets and quotes every identifier the way the edit grid's statements do, so
    /// what the dump replays is what Commit would have run.
    pub fn new(
        dialect: DbDialect,
        schema: Option<&str>,
        table: &str,
        columns: &[String],
    ) -> Result<Self, String> {
        let names = columns
            .iter()
            .map(|c| quote_ident(dialect, c))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        Ok(Self {
            prefix: format!(
                "INSERT INTO {} ({names}) VALUES",
                qualified(dialect, schema, table)?
            ),
            buffer: String::new(),
            used: 0,
        })
    }

    /// Add one row's value literals ("(1, 'a')", no leading newline). Some(statement) when
    /// the row did not fit and the statement so far must ship before this row opens the next
    /// one; a row larger than the threshold alone still joins — rows are never split.
    pub fn push(&mut self, row_sql: &str) -> Option<String> {
        // The fit check is adminer's (`adminer.inc.php:1058`): used + 4 (length-spec
        // headroom) + the row with its leading newline + the ";\n" every statement ends with.
        let piece = row_sql.len() + 1 + ";\n".len();
        if self.used != 0 && self.used + 4 + piece >= EXPORT_SQL_MAX_PACKET {
            let out = format!("{}{};\n", self.prefix, self.buffer);
            self.buffer = format!("\n{row_sql}");
            self.used = self.prefix.len() + self.buffer.len();
            return Some(out);
        }
        if self.buffer.is_empty() {
            self.buffer.push('\n');
        } else {
            self.buffer.push_str(",\n");
        }
        self.buffer.push_str(row_sql);
        self.used = self.prefix.len() + self.buffer.len();
        None
    }

    /// The last, not-yet-full statement — the dump's closing INSERT when any rows remain.
    pub fn finish(self) -> Option<String> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(format!("{}{};\n", self.prefix, self.buffer))
        }
    }
}

/// The preamble of a streaming SQL dump (SPEC §data.export): a provenance comment, the dialect's
/// foreign-key stance, and the CREATE TABLE from the same DDL path the Structure tab uses.
/// MySQL's `SET FOREIGN_KEY_CHECKS=0` lets the replay session load rows before any referenced
/// table exists (the footer restores it); Postgres has no session switch, so a comment states
/// the ordering contract instead. `ddl` arrives exactly as describe_table served it.
pub fn sql_dump_head(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    ddl: &str,
) -> Result<String, String> {
    let target = qualified(dialect, schema, table)?;
    // SHOW CREATE TABLE carries no terminator; build_pg_ddl already ends with ';'.
    let mut ddl = ddl.trim_end().to_string();
    if !ddl.is_empty() && !ddl.ends_with(';') {
        ddl.push(';');
    }
    let stance = if dialect == DbDialect::Mysql {
        format!(
            "-- {target}: foreign-key checks are off for the replay session\n\
             SET FOREIGN_KEY_CHECKS=0;\n"
        )
    } else {
        format!("-- {target}: Postgres has no session FK switch - replay in dependency order\n")
    };
    Ok(format!("-- swiss SQL dump\n{stance}\n{ddl}\n"))
}

/// The closing statement of a dump: MySQL restores the FK checks the head disabled; Postgres
/// needs nothing.
pub fn sql_dump_foot(dialect: DbDialect) -> &'static str {
    if dialect == DbDialect::Mysql {
        "SET FOREIGN_KEY_CHECKS=1;\n"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::paging::export_row_limit;
    use crate::dbbrowser::test_util::row;
    use crate::dbbrowser::values::exact_int64;

    #[test]
    fn export_carries_the_exact_digits_verbatim() {
        // SPEC §data.browse: rows/query/export all receive the SAME stringified cell, so the
        // digits a bigint column holds are what every surface shows — CSV quotes it (it is
        // a string cell), NDJSON re-quotes it as a JSON string, and neither re-parses it
        // through a number.
        let row: Map<String, Value> = [
            ("id".to_string(), exact_int64(i64::MAX)),
            ("n".to_string(), json!(1.5)),
        ]
        .into_iter()
        .collect();
        let cols = ["id".to_string(), "n".to_string()];
        let csv = to_csv(&cols, std::slice::from_ref(&row));
        assert_eq!(csv, "\"id\",\"n\"\r\n\"9223372036854775807\",\"1.5\"");
        let ndjson = to_json_lines(&[row]);
        assert_eq!(ndjson.trim(), "{\"id\":\"9223372036854775807\",\"n\":1.5}");
    }

    #[test]
    fn export_helpers() {
        assert_eq!(EXPORT_ROW_CAP, 100_000);
        assert_eq!(export_row_limit(None), EXPORT_ROW_CAP);
        assert_eq!(export_row_limit(Some(&json!(500))), 500);
        assert_eq!(export_row_limit(Some(&json!(999_999_999))), EXPORT_ROW_CAP);

        // Every cell is quoted uniformly (numbers included) — simplest RFC-legal shape.
        let rows = vec![
            row(&[("id", json!(1)), ("name", json!("a,b\""))]),
            row(&[("id", Value::Null), ("name", Value::Null)]),
        ];
        let csv = to_csv(&["id".to_string(), "name".to_string()], &rows);
        assert_eq!(csv, "\"id\",\"name\"\r\n\"1\",\"a,b\"\"\"\r\n,");

        let rows = vec![row(&[("a", json!(1))]), row(&[("b", Value::Null)])];
        assert_eq!(to_json_lines(&rows), "{\"a\":1}\n{\"b\":null}");
    }

    #[test]
    fn copy_out_helpers() {
        assert_eq!(csv_escape(None), "");
        assert_eq!(csv_escape(Some(&Value::Null)), "");
        assert_eq!(csv_escape(Some(&json!("plain"))), "\"plain\"");
        assert_eq!(csv_escape(Some(&json!("say \"hi\""))), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape(Some(&json!("a,b\nline2"))), "\"a,b\nline2\"");
        // An object becomes compact JSON, whose quotes double like any other quote inside the cell.
        assert_eq!(csv_escape(Some(&json!({ "x": 1 }))), "\"{\"\"x\"\":1}\"");

        assert_eq!(sql_literal(None), "NULL");
        assert_eq!(sql_literal(Some(&Value::Null)), "NULL");
        assert_eq!(sql_literal(Some(&json!(42))), "42");
        assert_eq!(sql_literal(Some(&json!(true))), "true");
        assert_eq!(sql_literal(Some(&json!("it's"))), "'it''s'");

        let out = to_insert_statement(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &["id".to_string(), "name".to_string(), "ghost".to_string()],
            &row(&[("id", json!(1)), ("name", json!("alice"))]),
        )
        .unwrap();
        assert_eq!(
            out,
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES (1, 'alice');"
        );
        let pg = to_insert_statement(
            DbDialect::Pg,
            None,
            "t",
            &["a".to_string()],
            &row(&[("a", Value::Null)]),
        )
        .unwrap();
        assert_eq!(pg, "INSERT INTO \"t\" (\"a\") VALUES (NULL);");
    }

    // --- streaming SQL dump (SPEC §data.export) ------------------------------------------------------------

    #[test]
    fn sql_dump_head_and_foot_snapshots() {
        // MySQL: the FK switch brackets the dump so a replay session can load rows before
        // referenced tables exist; the DDL arrives from SHOW CREATE TABLE with no ';'.
        let head = sql_dump_head(
            DbDialect::Mysql,
            Some("app"),
            "users",
            "CREATE TABLE `users` (\n  `id` int\n)",
        )
        .unwrap();
        assert_eq!(
            head,
            "-- swiss SQL dump\n\
             -- `app`.`users`: foreign-key checks are off for the replay session\n\
             SET FOREIGN_KEY_CHECKS=0;\n\
             \n\
             CREATE TABLE `users` (\n  `id` int\n);\n"
        );
        // Postgres: no session switch exists, so the comment says the ordering contract; the
        // catalog-sketch DDL already ends with ';' and must not gain a second one.
        let pg = sql_dump_head(
            DbDialect::Pg,
            Some("public"),
            "t",
            "CREATE TABLE \"public\".\"t\" (\n    \"id\" integer NOT NULL\n);",
        )
        .unwrap();
        assert_eq!(
            pg,
            "-- swiss SQL dump\n\
             -- \"public\".\"t\": Postgres has no session FK switch - replay in dependency order\n\
             \n\
             CREATE TABLE \"public\".\"t\" (\n    \"id\" integer NOT NULL\n);\n"
        );
        assert_eq!(
            sql_dump_foot(DbDialect::Mysql),
            "SET FOREIGN_KEY_CHECKS=1;\n"
        );
        assert_eq!(sql_dump_foot(DbDialect::Pg), "");
    }

    #[test]
    fn sql_insert_batches_rows_into_multi_value_statements() {
        let mut batch = SqlInsertBatch::new(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &["id".into(), "name".into()],
        )
        .unwrap();
        assert_eq!(batch.push("(1, 'a')"), None);
        assert_eq!(batch.push("(2, NULL)"), None);
        assert_eq!(
            batch.finish().unwrap(),
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES\n(1, 'a'),\n(2, NULL);\n"
        );
    }

    #[test]
    fn a_sql_insert_batch_flushes_when_the_next_row_would_cross_the_cap() {
        // adminer's threshold at work: two ~700 KB rows cannot share one statement under a
        // 1 MB cap — the first ships whole, the second opens the next statement, and neither
        // statement is ever split mid-row.
        let big = format!("('{}')", "x".repeat(700_000));
        let mut batch = SqlInsertBatch::new(DbDialect::Pg, None, "t", &["pad".into()]).unwrap();
        assert_eq!(batch.push(&big), None);
        let shipped = batch
            .push(&big)
            .expect("second ~700 KB row must flush the first");
        assert!(shipped.starts_with("INSERT INTO \"t\" (\"pad\") VALUES\n('"));
        assert!(shipped.ends_with(";\n"));
        assert!(shipped.len() < EXPORT_SQL_MAX_PACKET + big.len());
        let tail = batch.finish().unwrap();
        assert!(tail.starts_with("INSERT INTO \"t\" (\"pad\") VALUES\n('"));
        // Neither statement crossed the cap by more than the one row that always fits.
        assert!(tail.len() < EXPORT_SQL_MAX_PACKET + big.len());
    }

    #[test]
    fn an_empty_sql_insert_batch_finishes_into_nothing() {
        let batch = SqlInsertBatch::new(DbDialect::Mysql, None, "t", &["a".to_string()]).unwrap();
        assert_eq!(batch.finish(), None);
    }

    // --- dump literal escaping (SPEC §data.export) ---------------------------------------

    #[test]
    fn mysql_dump_literals_escape_everything_adminer_does() {
        // THE audit blocker case: a trailing backslash swallowed the closing quote under the
        // clipboard escaping, so an ordinary Windows path broke (or injected into) the replay.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("C:\\path\\"))).unwrap(),
            "'C:\\\\path\\\\'"
        );
        // Quotes, newlines: every character real_escape_string would not let through raw.
        assert_eq!(
            sql_dump_literal(
                DbDialect::Mysql,
                Some(&json!("it's \"quoted\"\nline\r end"))
            )
            .unwrap(),
            "'it\\'s \\\"quoted\\\"\\nline\\r end'"
        );
        // NUL and Ctrl-Z (0x1A) — the two control bytes the Windows MySQL client chokes on.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("a\0b\u{1a}c"))).unwrap(),
            "'a\\0b\\Zc'"
        );
        // Unicode passes through untouched — the bytes replay as themselves.
        assert_eq!(
            sql_dump_literal(
                DbDialect::Mysql,
                Some(&json!("\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}"))
            )
            .unwrap(),
            "'\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}'"
        );
        // Numbers, bools and nulls keep the plain shapes the dump always used.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!(7))).unwrap(),
            "7"
        );
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!(1.5))).unwrap(),
            "1.5"
        );
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!(true))).unwrap(),
            "true"
        );
        assert_eq!(sql_dump_literal(DbDialect::Mysql, None).unwrap(), "NULL");
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&Value::Null)).unwrap(),
            "NULL"
        );
    }

    #[test]
    fn pg_dump_literals_double_quotes_and_refuse_nul() {
        // Postgres text holds quotes, double quotes, newlines and backslashes raw — only the
        // single quote doubles (the dump's head does not opt into escape-string syntax).
        assert_eq!(
            sql_dump_literal(DbDialect::Pg, Some(&json!("it's \"x\"\n\\ line"))).unwrap(),
            "'it''s \"x\"\n\\ line'"
        );
        assert_eq!(
            sql_dump_literal(
                DbDialect::Pg,
                Some(&json!("\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}"))
            )
            .unwrap(),
            "'\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}'"
        );
        // A NUL cannot live in a Postgres text value; refusing it beats emitting the raw byte
        // into a dump that would then be corrupt from that row on.
        let err = sql_dump_literal(DbDialect::Pg, Some(&json!("a\0b"))).unwrap_err();
        assert!(err.contains("NUL"), "{err}");
        assert_eq!(
            sql_dump_literal(DbDialect::Pg, Some(&json!(9))).unwrap(),
            "9"
        );
        assert_eq!(sql_dump_literal(DbDialect::Pg, None).unwrap(), "NULL");
    }
}
