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

//! Import: CSV rows into the edit grid's inserts, and upserts per dialect (SPEC §data.export).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::edit::insert_half;
use super::filter::numeric_bind_value;
use super::ident::{qualified, quote_ident};
use super::model::{BrowseColumn, BuiltStatement};
use super::values::js_to_string;
use super::DbDialect;

/// Cap on rows in one import batch — one transaction, one bounded payload.
pub const IMPORT_ROW_CAP: usize = 10_000;

/// The import modes (SPEC §data.export): insert keeps today's behavior; upsert turns every row's
/// INSERT into a conflict-aware statement so re-importing a file lands instead of key-clashing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Insert,
    Upsert,
}

/// Read the `mode` option: absent, null and empty stay the insert default; anything but the two
/// known spellings is the caller's whole mistake.
pub fn parse_import_mode(v: Option<&Value>) -> Result<ImportMode, String> {
    let empty = matches!(v, None | Some(Value::Null))
        || matches!(v, Some(Value::String(s)) if s.is_empty());
    if empty {
        return Ok(ImportMode::Insert);
    }
    match v {
        Some(Value::String(s)) if s == "insert" => Ok(ImportMode::Insert),
        Some(Value::String(s)) if s == "upsert" => Ok(ImportMode::Upsert),
        Some(other) => Err(format!(
            "mode must be insert or upsert, not {}",
            js_to_string(Some(other))
        )),
        None => Ok(ImportMode::Insert),
    }
}

/// The conflict tail of an upsert import (SPEC §data.export), adminer's insertUpdate per dialect
/// (`drivers/mysql.inc.php:292`, `drivers/pgsql.inc.php:340`): MySQL maps every column the row
/// carries (`col` = VALUES(col)) — a table without any unique key simply never conflicts;
/// Postgres targets the table's primary key and updates the non-key columns from EXCLUDED,
/// DO NOTHING when the row carries nothing but key columns. An Err is Postgres without a
/// primary key — ON CONFLICT has no target there, and the caller degrades to plain inserts
/// with a note instead of failing the whole file.
pub fn upsert_suffix(
    dialect: DbDialect,
    row_columns: &[String],
    primary_key: &[String],
) -> Result<String, String> {
    match dialect {
        DbDialect::Mysql => {
            let sets: Result<Vec<String>, String> = row_columns
                .iter()
                .map(|c| {
                    let q = quote_ident(dialect, c)?;
                    Ok(format!("{q} = VALUES({q})"))
                })
                .collect();
            Ok(format!(" ON DUPLICATE KEY UPDATE {}", sets?.join(", ")))
        }
        DbDialect::Pg => {
            if primary_key.is_empty() {
                return Err("table has no primary key — Postgres has no ON CONFLICT target".into());
            }
            let conflict = primary_key
                .iter()
                .map(|c| quote_ident(dialect, c))
                .collect::<Result<Vec<_>, _>>()?;
            let key: HashSet<&str> = primary_key.iter().map(String::as_str).collect();
            let updates: Result<Vec<String>, String> = row_columns
                .iter()
                .filter(|c| !key.contains(c.as_str()))
                .map(|c| {
                    let q = quote_ident(dialect, c)?;
                    Ok(format!("{q} = EXCLUDED.{q}"))
                })
                .collect();
            let updates = updates?;
            Ok(if updates.is_empty() {
                format!(" ON CONFLICT ({}) DO NOTHING", conflict.join(", "))
            } else {
                format!(
                    " ON CONFLICT ({}) DO UPDATE SET {}",
                    conflict.join(", "),
                    updates.join(", ")
                )
            })
        }
    }
}

/// Import rows → statements (SPEC §data.export). Insert mode produces exactly the statements the
/// edit grid's insert arm builds, one per row, by construction. Upsert appends the conflict
/// tail per row (the columns a row carries vary — empty CSV cells drop). A table that cannot
/// upsert (Postgres without a primary key) comes back in INSERT form plus the note that says
/// why, so the file still lands in one transaction instead of failing on statement one.
pub fn build_import_statements(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    rows: &[Map<String, Value>],
    columns: &[BrowseColumn],
    primary_key: &[String],
    mode: ImportMode,
) -> Result<(Vec<BuiltStatement>, Option<&'static str>), String> {
    let table_ref = qualified(dialect, schema, table)?;
    let known: HashSet<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    let type_of: HashMap<&str, &str> = columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str()))
        .collect();
    let degraded = if mode == ImportMode::Upsert
        && dialect == DbDialect::Pg
        && primary_key.is_empty()
    {
        Some("table has no primary key — upsert is not possible on Postgres, rows were inserted instead")
    } else {
        None
    };
    let upsert = mode == ImportMode::Upsert && degraded.is_none();
    let mut out: Vec<BuiltStatement> = Vec::with_capacity(rows.len());
    for row in rows {
        let (cols, sql, params) = insert_half(dialect, &table_ref, row, &known, &type_of)?;
        let suffix = if upsert {
            upsert_suffix(dialect, &cols, primary_key)?
        } else {
            String::new()
        };
        out.push(BuiltStatement {
            sql: format!("{sql}{suffix}"),
            params,
        });
    }
    Ok((out, degraded))
}

// --- CSV import ----------------------------------------------------------------------------------

/// Parse ONE line of CSV honoring RFC 4180 quoting (a quoted field may span lines is NOT
/// supported here — the panel splits on physical newlines, and quoted newlines inside a field
/// are the importer's job to have escaped or avoided; fields with them arrive via JSON upload).
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_q {
            if c == '"' && i + 1 < chars.len() && chars[i + 1] == '"' {
                cur.push('"');
                i += 1;
            } else if c == '"' {
                in_q = false;
            } else {
                cur.push(c);
            }
        } else if c == ',' {
            out.push(std::mem::take(&mut cur));
        } else if c == '"' && cur.is_empty() {
            // A quote only opens a quoted field at the field's very start.
            in_q = true;
        } else {
            cur.push(c);
        }
        i += 1;
    }
    out.push(cur);
    out
}

/// Turn CSV lines into insert values using the mapping (header index -> table column, None =
/// skip this CSV column). Coercion is deliberately thin: empty string becomes nothing (the key
/// is never set), numeric-looking strings become numbers ONLY when Number() is lossless
/// (numeric_bind_value — a 19-digit snowflake ID or a beyond-double-precision decimal stays a
/// string so the driver casts it against the column exactly), everything else stays a string and
/// the driver casts. A row whose mapped cells are ALL empty is dropped rather than inserted as
/// NULLs.
pub fn map_import_rows(
    header: &[String],
    lines: &[String],
    mapping: &[Option<String>],
) -> Result<Vec<Map<String, Value>>, String> {
    if mapping.len() != header.len() {
        return Err(format!(
            "mapping covers {} of {} CSV columns",
            mapping.len(),
            header.len()
        ));
    }
    let mut out: Vec<Map<String, Value>> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let cells = parse_csv_line(line);
        let mut row: Map<String, Value> = Map::new();
        let mut any = false;
        for i in 0..header.len() {
            // Node's `if (!col)` — None and "" both skip the column.
            let Some(col) = mapping
                .get(i)
                .and_then(|c| c.as_deref())
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let raw = cells.get(i).map(String::as_str).unwrap_or("");
            if raw.is_empty() {
                continue;
            }
            any = true;
            row.insert(col.to_string(), numeric_bind_value(raw));
        }
        if any {
            out.push(row);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::edit::build_edit_statements;
    use crate::dbbrowser::model::BrowseEdit;
    use crate::dbbrowser::test_util::{row, stub_columns};

    #[test]
    fn csv_helpers_round_trip() {
        // RFC 4180 line: doubled quotes, embedded commas.
        assert_eq!(
            parse_csv_line("a,\"b,c\",\"say \"\"hi\"\"\""),
            vec!["a", "b,c", "say \"hi\""]
        );
        assert_eq!(parse_csv_line("plain,cells"), vec!["plain", "cells"]);

        // mapImportRows: coerce numbers, drop empty rows, honor the mapping's nulls.
        let header = vec!["id".to_string(), "name".to_string(), "note".to_string()];
        let lines = vec![
            "1,alice,x".to_string(),
            "\"2\",\"bob\",".to_string(),
            ",,".to_string(),
        ];
        let mapping = vec![Some("id".to_string()), Some("name".to_string()), None];
        let rows = map_import_rows(&header, &lines, &mapping).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&json!(1)));
        assert_eq!(rows[0].get("name"), Some(&json!("alice")));
        assert_eq!(rows[1].get("id"), Some(&json!(2)));

        let err = map_import_rows(&["a".to_string()], &["1".to_string()], &[]).unwrap_err();
        assert!(err.contains("covers"), "{err}");
    }

    #[test]
    fn map_import_rows_keeps_snowflake_precision() {
        let header = vec!["id".to_string(), "qty".to_string(), "note".to_string()];
        let mapping = vec![
            Some("id".to_string()),
            Some("qty".to_string()),
            Some("note".to_string()),
        ];
        let rows = map_import_rows(
            &header,
            &["734023681584275456,3,hello".to_string()],
            &mapping,
        )
        .unwrap();
        assert_eq!(rows[0].get("id"), Some(&json!("734023681584275456")));
        assert_eq!(rows[0].get("qty"), Some(&json!(3)));
        assert_eq!(rows[0].get("note"), Some(&json!("hello")));
        let rows = map_import_rows(
            &header,
            &["734023681584275456,0.100000000000000009,x".to_string()],
            &mapping,
        )
        .unwrap();
        assert_eq!(rows[0].get("qty"), Some(&json!("0.100000000000000009")));
    }

    // --- import upsert (SPEC §data.export) ----------------------------------------------------------------

    #[test]
    fn upsert_tails_match_adminer_per_dialect() {
        let cols = vec!["id".to_string(), "name".to_string()];
        // MySQL maps every column the row carries — adminer's ON DUPLICATE KEY UPDATE.
        assert_eq!(
            upsert_suffix(DbDialect::Mysql, &cols, &["id".to_string()]).unwrap(),
            " ON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
        // MySQL needs no primary key: ON DUPLICATE KEY UPDATE rides any unique key.
        assert!(upsert_suffix(DbDialect::Mysql, &cols, &[]).is_ok());
        // Postgres targets the primary key and updates the rest from EXCLUDED.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &cols, &["id".to_string()]).unwrap(),
            " ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
        // A row that carries nothing but key columns has nothing to update — DO NOTHING.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &["id".to_string()], &["id".to_string()]).unwrap(),
            " ON CONFLICT (\"id\") DO NOTHING"
        );
        // A composite key targets every key column, in table order; mapped non-key columns
        // all update.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &cols, &["a".to_string(), "b".to_string()]).unwrap(),
            " ON CONFLICT (\"a\", \"b\") DO UPDATE SET \"id\" = EXCLUDED.\"id\", \"name\" = EXCLUDED.\"name\""
        );
        // Postgres without a primary key has no conflict target at all.
        assert!(upsert_suffix(DbDialect::Pg, &cols, &[]).is_err());
    }

    #[test]
    fn import_insert_mode_is_the_edit_grids_insert_arm_byte_for_byte() {
        let rows = vec![
            row(&[("id", json!(1)), ("name", json!("alice"))]),
            row(&[("id", json!(2)), ("name", json!("bob"))]),
        ];
        let edits = rows
            .iter()
            .cloned()
            .map(|values| BrowseEdit::Insert { values })
            .collect::<Vec<_>>();
        let cols = stub_columns();
        let pk = vec!["id".to_string()];
        for dialect in [DbDialect::Mysql, DbDialect::Pg] {
            let via_import = build_import_statements(
                dialect,
                Some("app"),
                "users",
                &rows,
                &cols,
                &pk,
                ImportMode::Insert,
            )
            .unwrap();
            let via_grid =
                build_edit_statements(dialect, Some("app"), "users", &edits, &cols, &pk).unwrap();
            assert_eq!(via_import.0, via_grid, "{dialect:?}");
            assert_eq!(via_import.1, None);
        }
    }

    #[test]
    fn upsert_imports_carry_the_conflict_tail() {
        let rows = vec![row(&[("id", json!(1)), ("name", json!("alice"))])];
        let cols = stub_columns();
        let pk = vec!["id".to_string()];
        let (out, degraded) = build_import_statements(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &rows,
            &cols,
            &pk,
            ImportMode::Upsert,
        )
        .unwrap();
        assert_eq!(degraded, None);
        assert_eq!(
            out[0].sql,
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
        assert_eq!(out[0].params, vec![json!(1), json!("alice")]);

        let (out, degraded) = build_import_statements(
            DbDialect::Pg,
            Some("public"),
            "users",
            &rows,
            &cols,
            &pk,
            ImportMode::Upsert,
        )
        .unwrap();
        assert_eq!(degraded, None);
        assert_eq!(
            out[0].sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES (CAST($1 AS int), CAST($2 AS text)) ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
    }

    #[test]
    fn a_postgres_upsert_without_a_pk_degrades_to_insert_with_a_note() {
        let rows = vec![row(&[("id", json!(1)), ("name", json!("alice"))])];
        let cols = stub_columns();
        let (out, degraded) = build_import_statements(
            DbDialect::Pg,
            Some("public"),
            "users",
            &rows,
            &cols,
            &[],
            ImportMode::Upsert,
        )
        .unwrap();
        let note = degraded.expect("Postgres without a pk must say why it degraded");
        assert!(note.contains("no primary key"), "{note}");
        assert!(note.contains("inserted instead"), "{note}");
        // The statements are plain inserts — the file still lands, in the same transaction.
        assert_eq!(
            out[0].sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES (CAST($1 AS int), CAST($2 AS text))"
        );
    }
}
