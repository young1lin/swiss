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

//! The edit list into statements: parsing the panel's edits, binding them per dialect, and
//! reading the committed rows back.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::address::{
    address_where, binary_wire_bytes, is_binary_type, is_text_or_binary, md5_address, text_bytes,
    value_len, EDIT_ADDR_MD5_MIN,
};
use super::ident::{qualified, quote_ident};
use super::model::{BrowseColumn, BrowseEdit, BuiltStatement};
use super::values::js_to_string;
use super::DbDialect;

// --- edit statements -----------------------------------------------------------------------------

/// Read one posted edit ({op, pk?, changes?, values?}) into the typed shape the statement
/// builder guards. pk/changes/values default to empty objects the way JS's missing fields read
/// as undefined and tripped the builder's own checks.
pub fn browse_edit_of(v: &Value) -> Result<BrowseEdit, String> {
    let obj = |k: &str| -> Map<String, Value> {
        v.get(k)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    match js_to_string(v.get("op")).as_str() {
        "insert" => Ok(BrowseEdit::Insert {
            values: obj("values"),
        }),
        "update" => Ok(BrowseEdit::Update {
            pk: obj("pk"),
            changes: obj("changes"),
            source: v.get("source").and_then(Value::as_object).cloned(),
        }),
        "delete" => Ok(BrowseEdit::Delete { pk: obj("pk") }),
        // Garbage input: Node let an unknown op fall through to a TypeError deeper in; this port
        // refuses it at the parse with a plain message — same 400, different text.
        other => Err(format!(
            "unknown edit op: {other} (must be update, insert or delete)"
        )),
    }
}

/// A bound-parameter placeholder: "?" for MySQL, "$n" for Postgres (per statement, so n restarts).
pub(super) fn ph(dialect: DbDialect, params: &[Value]) -> String {
    // bind() has already pushed the value, so the array length IS this parameter's 1-based number.
    if dialect.binds_question() {
        "?".to_string()
    } else {
        format!("${}", params.len())
    }
}

/// information_schema data_types that are not directly usable as a CAST target: "ARRAY" needs
/// its element type (a separate column), and "USER-DEFINED" is an enum/domain NAME that only
/// the udt column carries. A bind on those keeps the bare placeholder — exactly as untyped as
/// node-pg's own binds were, never worse.
fn castable_data_type(data_type: Option<&str>) -> Option<&str> {
    data_type.filter(|t| {
        !t.is_empty() && !t.eq_ignore_ascii_case("ARRAY") && !t.eq_ignore_ascii_case("USER-DEFINED")
    })
}

/// A placeholder carrying the column's type on Postgres: node-pg bound parameters UNTYPED (OID
/// 0) and the server inferred them from the statement, but sqlx declares every string/null
/// bind as `text` — so `bigint = text` dies with 42883 (operator does not exist), and
/// `SET ts = $1` or a NULL against a non-text column with 42804. CASTing the PLACEHOLDER
/// restores the inference node-pg had, and types NULL binds, which a bare placeholder never
/// could. MySQL coerces strings against the column itself; its "?" stays bare.
pub(super) fn typed_ph(dialect: DbDialect, params: &[Value], data_type: Option<&str>) -> String {
    let base = ph(dialect, params);
    if dialect != DbDialect::Pg {
        // MySQL coerces string binds against the column itself - except JSON: a string
        // bound next to a JSON column compares as TEXT (server-verified on 8.4: zero
        // matches), so filters and keyless row addresses over a JSON column never hit.
        // CASTing the placeholder restores the comparison. Found live by the L1 suite
        // (SPEC §testing.it); only the comparison placeholder changes, never the SET clause.
        let json_compare = data_type.is_some_and(|t| t.eq_ignore_ascii_case("json"));
        return if json_compare {
            format!("CAST({base} AS JSON)")
        } else {
            base
        };
    }
    match castable_data_type(data_type) {
        Some(t) => format!("CAST({base} AS {t})"),
        None => base,
    }
}

/// How one edit's committed row comes home with the batch reply (SPEC §data.edits). The point is
/// showing what the SERVER kept — silent truncation, column DEFAULTs, trigger rewrites —
/// instead of the value the panel sent. Three shapes:
///
/// - an insert on Postgres rides its own INSERT (a RETURNING suffix carries the row out);
/// - an insert on MySQL reads back by LAST_INSERT_ID() in the same transaction;
/// - an update selects its row by primary key (the edit already carries those values).
///
/// Deletes read nothing back — the row is gone by design — and neither does a row whose
/// values cannot address itself (a NULL among them on a keyless table, W4.1).
#[derive(Clone, Debug)]
pub enum ReadBack {
    /// The suffix to append to the edit's own INSERT; its parameters are unchanged.
    Returning(String),
    /// A same-transaction SELECT. Its parameters are the edit's primary-key values — or empty
    /// when the SQL carries LAST_INSERT_ID() itself.
    Select(BuiltStatement),
    /// No row to read (a delete, or no key to address one by).
    None,
}

/// Read-back column list + addressing for one edit. `columns` is the full table column set (the
/// row should come back shaped like a grid row); `pk` the table's primary-key column names.
/// Identifier quoting goes through the same whitelist as every other statement here, so an
/// unpromising name downgrades to ReadBack::None rather than poisoning the commit.
pub fn readback_plan(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    columns: &[BrowseColumn],
    pk: &[String],
    edit: &BrowseEdit,
) -> ReadBack {
    let cols = || -> Option<String> {
        columns
            .iter()
            .map(|c| quote_ident(dialect, &c.name))
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .map(|v| v.join(", "))
    };
    let target = || -> Option<String> {
        let t = quote_ident(dialect, table).ok()?;
        Some(match schema {
            Some(s) => format!("{}.{t}", quote_ident(dialect, s).ok()?),
            None => t,
        })
    };
    match edit {
        BrowseEdit::Delete { .. } => ReadBack::None,
        BrowseEdit::Insert { .. } if dialect == DbDialect::Pg => match cols() {
            Some(list) => ReadBack::Returning(format!(" RETURNING {list}")),
            None => ReadBack::None,
        },
        BrowseEdit::Insert { .. } => {
            // MySQL has no RETURNING: the auto-generated key of the statement that just ran is
            // addressable as LAST_INSERT_ID() on the same connection, so the read-back SELECT
            // needs no parameters at all. A composite key with an auto column cannot be
            // addressed this way — those inserts read nothing back.
            let (Some(list), Some(target), true) = (cols(), target(), pk.len() == 1) else {
                return ReadBack::None;
            };
            let Some(k) = quote_ident(dialect, &pk[0]).ok() else {
                return ReadBack::None;
            };
            ReadBack::Select(BuiltStatement {
                sql: format!("SELECT {list} FROM {target} WHERE {k} = LAST_INSERT_ID()"),
                params: Vec::new(),
            })
        }
        BrowseEdit::Update {
            pk: values,
            changes,
            ..
        } if pk.is_empty() => {
            // SPEC §data.edits: a keyless table reads its row back by the same every-column
            // address the update itself used — the read-back is the same truth, and twins
            // (identical on every column) come back identical, which is exactly the one
            // case where a LIMIT-less SELECT still tells it. A row the values cannot
            // address (a NULL among them — the update refused it; a quote-hostile name)
            // reads nothing back rather than failing the commit it only decorates.
            //
            // The address is the row AS THE COMMIT LEFT IT (originals overlaid with the
            // changes): every column is addressing here, so probing by the untouched
            // originals would re-find nothing the moment any value moved. Caught live in
            // the W4.2 verify — the edit landed and the read-back said null.
            let (Some(list), Some(target)) = (cols(), target()) else {
                return ReadBack::None;
            };
            let mut after: Map<String, Value> = values.clone();
            for (c, v) in changes {
                after.insert(c.clone(), v.clone());
            }
            let mut params: Vec<Value> = Vec::new();
            let Ok(where_sql) = address_where(dialect, columns, &after, &mut params) else {
                return ReadBack::None;
            };
            ReadBack::Select(BuiltStatement {
                sql: format!("SELECT {list} FROM {target} WHERE {where_sql}"),
                params,
            })
        }
        BrowseEdit::Update { pk: values, .. } => {
            let (Some(list), Some(target)) = (cols(), target()) else {
                return ReadBack::None;
            };
            // Same typed placeholder the update's own WHERE got (W1.2): pk values arrive
            // from the panel as JSON strings, sqlx binds strings as text, and a bare $n
            // against a non-text key column dies with 42883 -- taking the whole commit down
            // with the read-back. Pre-existing, caught live in W2.2 verify on a real PG
            // table; reproducible on the pure W1 dblclick edit path (MySQL coerces, so it
            // hid there).
            let type_of: HashMap<&str, &str> = columns
                .iter()
                .map(|c| (c.name.as_str(), c.data_type.as_str()))
                .collect();
            let mut where_parts: Vec<String> = Vec::with_capacity(pk.len());
            let mut params: Vec<Value> = Vec::with_capacity(pk.len());
            for k in pk {
                let Some(v) = values.get(k) else {
                    return ReadBack::None; // the edit cannot address its own row
                };
                let Some(quoted) = quote_ident(dialect, k).ok() else {
                    return ReadBack::None;
                };
                params.push(v.clone());
                where_parts.push(format!(
                    "{quoted} = {}",
                    typed_ph(dialect, &params, type_of.get(k.as_str()).copied())
                ));
            }
            if where_parts.is_empty() {
                return ReadBack::None; // no key columns: nothing to select by
            }
            ReadBack::Select(BuiltStatement {
                sql: format!(
                    "SELECT {list} FROM {target} WHERE {}",
                    where_parts.join(" AND ")
                ),
                params,
            })
        }
    }
}

/// Turn a buffered edit list into executable statements: one statement per edit, in order, with
/// every value bound and every identifier pre-vetted. Column names the table does not have are
/// dropped from SET/INSERT (a stale page may reference a dropped column); an edit that ends up
/// with nothing to do is an error, because "commit 3 changes" that silently did 2 is a lie.
///
/// The INSERT for one map of values — the shared body of the edit grid's insert arm and both
/// import modes (SPEC §data.export): the same known-column filter, the same placeholder typing, the
/// same column order (the map's own), so a row commits as the same statement wherever it came
/// from. Returns the columns it mapped (an upsert tail addresses them), the SQL without any
/// suffix, and the bound parameters in SQL order.
pub(super) fn insert_half(
    dialect: DbDialect,
    table_ref: &str,
    values: &Map<String, Value>,
    known: &HashSet<&str>,
    type_of: &HashMap<&str, &str>,
) -> Result<(Vec<String>, String, Vec<Value>), String> {
    // Object.keys order — the panel's own insertion order.
    let cols: Vec<&String> = values
        .keys()
        .filter(|c| known.contains(c.as_str()))
        .collect();
    if cols.is_empty() {
        return Err("insert names no column of this table".into());
    }
    let mut params: Vec<Value> = Vec::new();
    let mut value_sql: Vec<String> = Vec::new();
    for c in &cols {
        params.push(values[*c].clone());
        value_sql.push(typed_ph(dialect, &params, type_of.get(c.as_str()).copied()));
    }
    let names = cols
        .iter()
        .map(|c| quote_ident(dialect, c))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    Ok((
        cols.into_iter().cloned().collect(),
        format!(
            "INSERT INTO {table_ref} ({names}) VALUES ({})",
            value_sql.join(", ")
        ),
        params,
    ))
}

/// Updates and deletes address rows by the FULL primary key when there is one — the DBeaver
/// default, so an edit can never fan out over more rows than the cell you changed — and by
/// EVERY column when there is not (SPEC §data.edits, adminer's unique_idf): the row map then
/// carries the whole original row, a NULL in any column makes the row unaddressable ("NULL
/// is ambiguous"), text/binary values past EDIT_ADDR_MD5_MIN bytes compare through md5(),
/// and MySQL clips the statement with LIMIT 1 while Postgres' affected-rows check in the
/// adapter refuses twins outright.
pub fn build_edit_statements(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    edits: &[BrowseEdit],
    columns: &[BrowseColumn],
    primary_key: &[String],
) -> Result<Vec<BuiltStatement>, String> {
    let table_ref = qualified(dialect, schema, table)?;
    let known: HashSet<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    let type_of: HashMap<&str, &str> = columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str()))
        .collect();
    let keyless = primary_key.is_empty();
    let mut out: Vec<BuiltStatement> = Vec::new();
    for edit in edits {
        let mut params: Vec<Value> = Vec::new();
        // The WHERE half update/delete share: one bound value per PK column, in key order.
        let pk_where =
            |pk: &Map<String, Value>, params: &mut Vec<Value>| -> Result<String, String> {
                let mut where_sql: Vec<String> = Vec::new();
                for c in primary_key {
                    params.push(pk[c.as_str()].clone());
                    where_sql.push(format!(
                        "{} = {}",
                        quote_ident(dialect, c)?,
                        typed_ph(dialect, params, type_of.get(c.as_str()).copied())
                    ));
                }
                Ok(where_sql.join(" AND "))
            };
        // The every-column twin of pk_where (SPEC §data.edits): the trailing LIMIT 1 MySQL alone
        // accepts — Postgres' ambiguity guard is the affected-rows check in the adapter.
        let row_where =
            |pk: &Map<String, Value>, params: &mut Vec<Value>| -> Result<String, String> {
                address_where(dialect, columns, pk, params).map_err(|gap| gap.message(edit.op()))
            };
        let mysql_limit = |stmt: String| -> String {
            if keyless && dialect == DbDialect::Mysql {
                format!("{stmt} LIMIT 1")
            } else {
                stmt
            }
        };
        match edit {
            BrowseEdit::Insert { values } => {
                let (_, sql, insert_params) =
                    insert_half(dialect, &table_ref, values, &known, &type_of)?;
                out.push(BuiltStatement {
                    sql,
                    params: insert_params,
                });
            }
            BrowseEdit::Delete { pk } => {
                // update / delete: the WHERE needs a value for every addressing column — the
                // PK columns on a keyed table, every column on a keyless one (W4.1).
                let where_sql = if keyless {
                    row_where(pk, &mut params)?
                } else {
                    for c in primary_key {
                        if !pk.contains_key(c) {
                            return Err(format!(
                                "{} needs a value for primary-key column {}",
                                edit.op(),
                                c
                            ));
                        }
                    }
                    pk_where(pk, &mut params)?
                };
                out.push(BuiltStatement {
                    sql: mysql_limit(format!("DELETE FROM {table_ref} WHERE {where_sql}")),
                    params,
                });
            }
            BrowseEdit::Update {
                pk,
                changes,
                source,
            } => {
                if !keyless {
                    for c in primary_key {
                        if !pk.contains_key(c) {
                            return Err(format!(
                                "{} needs a value for primary-key column {}",
                                edit.op(),
                                c
                            ));
                        }
                    }
                }
                // Bind in SQL order — SET placeholders appear before the WHERE's, and a positional
                // "?" driver (mysql2) wires params strictly by position.
                let sets: Vec<(&String, &Value)> = changes
                    .iter()
                    .filter(|(c, _)| known.contains(c.as_str()))
                    .collect();
                if sets.is_empty() {
                    return Err("update names no column of this table".into());
                }
                let mut set_sql: Vec<String> = Vec::new();
                for (c, v) in &sets {
                    params.push((*v).clone());
                    set_sql.push(format!(
                        "{} = {}",
                        quote_ident(dialect, c)?,
                        typed_ph(dialect, &params, type_of.get(c.as_str()).copied())
                    ));
                }
                let origin = source.as_ref().unwrap_or(pk);
                let where_sql = if keyless {
                    // The every-column address IS the optimistic lock here (W4.1 + W4.2
                    // merge on their own): the row the panel buffered is the row asked for.
                    row_where(pk, &mut params)?
                } else {
                    // SPEC §data.edits: the optimistic lock — every CHANGED column whose
                    // original the row map carries is compared in the WHERE, so a row that
                    // moved underneath matches zero rows instead of silently overwriting.
                    // Only columns the map actually carries are compared: a pk-only payload
                    // (an API caller that never saw the row) keeps today's SQL byte for
                    // byte. NULL originals need the NULL-safe forms — col = NULL matches
                    // nothing and would false-conflict every commit of a NULL cell; long
                    // text rides the md5 address like W4.1's keyless WHERE.
                    let mut w = pk_where(pk, &mut params)?;
                    for (c, _) in &sets {
                        if primary_key.contains(*c) {
                            continue; // the pk arm of the WHERE already pins the original key
                        }
                        let Some(orig) = origin.get(*c) else {
                            continue;
                        };
                        let quoted = quote_ident(dialect, c)?;
                        let dt = type_of.get(c.as_str()).copied();
                        let frag = if orig.is_null() {
                            params.push(Value::Null);
                            if dialect == DbDialect::Mysql {
                                format!("{quoted} <=> {}", ph(dialect, &params))
                            } else {
                                format!(
                                    "{quoted} IS NOT DISTINCT FROM {}",
                                    typed_ph(dialect, &params, dt)
                                )
                            }
                        } else if dt.map(is_binary_type).unwrap_or(false) {
                            // A binary original folds at any length, digesting the decoded
                            // wire bytes — same reasoning as the keyless address.
                            md5_address(dialect, &quoted, &binary_wire_bytes(orig), &mut params)
                        } else if is_text_or_binary(dt.unwrap_or(""))
                            && value_len(orig) > EDIT_ADDR_MD5_MIN
                        {
                            md5_address(dialect, &quoted, &text_bytes(orig), &mut params)
                        } else {
                            params.push(orig.clone());
                            format!("{quoted} = {}", typed_ph(dialect, &params, dt))
                        };
                        w.push_str(" AND ");
                        w.push_str(&frag);
                    }
                    w
                };
                out.push(BuiltStatement {
                    sql: mysql_limit(format!(
                        "UPDATE {table_ref} SET {} WHERE {}",
                        set_sql.join(", "),
                        where_sql
                    )),
                    params,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::address::md5_hex;
    use crate::dbbrowser::test_util::{col, edit, stub_columns};

    #[test]
    fn readback_plan_addresses_every_committed_row() {
        // SPEC §data.edits: the commit reply carries what the SERVER kept, not what was sent.
        let vmap = |pairs: &[(&str, i64)]| -> Map<String, Value> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect()
        };
        let col = |name: &str, data_type: &str| BrowseColumn {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            is_primary_key: name == "id",
            default_value: None,
            comment: None,
        };
        let columns = vec![col("id", "bigint"), col("name", "text")];
        let pk = vec!["id".to_string()];
        let upd = BrowseEdit::Update {
            pk: vmap(&[("id", 7)]),
            changes: vmap(&[("name", 0)]),
            source: None,
        };
        match readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &upd) {
            ReadBack::Select(s) => {
                assert_eq!(s.sql, "SELECT `id`, `name` FROM `users` WHERE `id` = ?");
                assert_eq!(s.params, vec![json!(7)]);
            }
            other => panic!("mysql update selects its row back: {other:?}"),
        }
        // The read-back SELECT addresses the row by the same TYPED placeholder the
        // update's WHERE got in W1.2: the pk value arrives from the panel as a JSON
        // string, and sqlx binds a string as text -- against a bigint column that dies
        // with 42883 (operator does not exist) and rolls the whole commit back. Caught
        // live in W2.2 verification on a real PG table; reproducible on the pure W1
        // dblclick edit path.
        match readback_plan(DbDialect::Pg, Some("app"), "users", &columns, &pk, &upd) {
            ReadBack::Select(s) => {
                assert_eq!(
                    s.sql,
                    "SELECT \"id\", \"name\" FROM \"app\".\"users\" WHERE \"id\" = CAST($1 AS bigint)"
                );
            }
            other => panic!("pg update selects its row back: {other:?}"),
        }
        let ins = BrowseEdit::Insert {
            values: vmap(&[("name", 0)]),
        };
        match readback_plan(DbDialect::Pg, None, "users", &columns, &pk, &ins) {
            ReadBack::Returning(suffix) => {
                assert_eq!(suffix, " RETURNING \"id\", \"name\"");
            }
            other => panic!("pg insert rides RETURNING: {other:?}"),
        }
        match readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &ins) {
            ReadBack::Select(s) => {
                assert_eq!(
                    s.sql,
                    "SELECT `id`, `name` FROM `users` WHERE `id` = LAST_INSERT_ID()"
                );
                assert!(s.params.is_empty());
            }
            other => panic!("mysql insert reads LAST_INSERT_ID back: {other:?}"),
        }
        let del = BrowseEdit::Delete {
            pk: vmap(&[("id", 7)]),
        };
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &del),
            ReadBack::None
        ));
        // W4.1 replaced the old "no key, nothing back": a keyless table reads back by
        // its every-column address (no_pk_readback_selects_by_the_same_all_column_address
        // pins it), and the address carries the changes the commit just wrote — the probe
        // re-finds the row the update moved.
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &columns, &[], &upd),
            ReadBack::Select(_)
        ));
    }

    #[test]
    fn edit_statements_bind_updates_by_full_primary_key() {
        let edits = vec![edit(
            json!({ "op": "update", "pk": { "id": 7 }, "changes": { "name": "alice" } }),
        )];
        let cols = stub_columns();
        let pk = vec!["id".to_string()];
        let out = build_edit_statements(DbDialect::Mysql, Some("app"), "users", &edits, &cols, &pk)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].sql,
            "UPDATE `app`.`users` SET `name` = ? WHERE `id` = ?"
        );
        assert_eq!(out[0].params, vec![json!("alice"), json!(7)]);

        let out = build_edit_statements(DbDialect::Pg, Some("public"), "users", &edits, &cols, &pk)
            .unwrap();
        // Postgres placeholders carry the column's type (see typed_ph): sqlx binds strings as
        // `text`, and node-pg's untyped binds let the server infer — the CAST restores that.
        assert_eq!(
            out[0].sql,
            "UPDATE \"public\".\"users\" SET \"name\" = CAST($1 AS text) WHERE \"id\" = CAST($2 AS int)"
        );
        assert_eq!(out[0].params, vec![json!("alice"), json!(7)]);
    }

    #[test]
    fn edit_statements_build_inserts_and_deletes() {
        let edits = vec![
            edit(json!({ "op": "insert", "values": { "id": 9, "name": "bob" } })),
            edit(json!({ "op": "delete", "pk": { "id": 3 } })),
        ];
        let out = build_edit_statements(
            DbDialect::Mysql,
            None,
            "users",
            &edits,
            &stub_columns(),
            &["id".into()],
        )
        .unwrap();
        assert_eq!(
            out[0].sql,
            "INSERT INTO `users` (`id`, `name`) VALUES (?, ?)"
        );
        assert_eq!(out[0].params, vec![json!(9), json!("bob")]);
        assert_eq!(out[1].sql, "DELETE FROM `users` WHERE `id` = ?");
        assert_eq!(out[1].params, vec![json!(3)]);
    }

    #[test]
    fn edit_statements_drop_unknown_columns() {
        let edits = vec![edit(
            json!({ "op": "update", "pk": { "id": 1 }, "changes": { "name": "x", "ghost": "y" } }),
        )];
        let out = build_edit_statements(
            DbDialect::Pg,
            None,
            "users",
            &edits,
            &stub_columns(),
            &["id".to_string()],
        )
        .unwrap();
        assert!(!out[0].sql.contains("ghost"));
        let err = build_edit_statements(
            DbDialect::Mysql,
            None,
            "u",
            &[edit(
                json!({ "op": "update", "pk": { "id": 1 }, "changes": { "ghost": "y" } }),
            )],
            &stub_columns(),
            &["id".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("no column"), "{err}");
    }

    #[test]
    fn edit_statements_refuse_unaddressable_rows() {
        let cols = stub_columns();
        // SPEC §data.edits: a table with no primary key is no longer refused outright — its rows
        // are addressed by every column — but the row map still has to carry those values.
        // An empty map names the first missing column instead of the old blanket refusal.
        let err = build_edit_statements(
            DbDialect::Mysql,
            None,
            "u",
            &[edit(json!({ "op": "delete", "pk": {} }))],
            &cols,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("every column"), "{err}");
        assert!(err.contains("id"), "{err}");
        let err = build_edit_statements(
            DbDialect::Mysql,
            None,
            "u",
            &[edit(json!({ "op": "delete", "pk": { "name": "x" } }))],
            &cols,
            &["id".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("primary-key column id"), "{err}");
    }

    /// The wire form of a binary column value: \x + lowercase hex, the shape both adapters
    /// emit and the panel echoes back (PG bytea text format).
    fn wire(bytes: &[u8]) -> String {
        format!("\\x{}", swiss_core::util::to_hex(bytes))
    }

    #[test]
    fn keyless_binary_address_digests_the_original_bytes() {
        // W4b audit: a non-UTF-8 binary past 64 bytes used to arrive as a from_utf8_lossy
        // STRING and the md5 fold digested that mangled text — md5(col) in the database
        // runs over the column's raw bytes, so the address matched nothing and the edit
        // read back as another writer's change (a false 409). The fold must digest the
        // DECODED bytes the \x wire form stands for.
        let payload: Vec<u8> = std::iter::once(0xFFu8)
            .chain(std::iter::repeat_n(b'x', 99))
            .collect();
        let cols = vec![
            col("name", "varchar(80)", false, false),
            col("payload", "varbinary(200)", false, false),
        ];
        let edits = vec![edit(json!({
            "op": "delete",
            "pk": { "name": "row-1", "payload": wire(&payload) },
        }))];
        let out = build_edit_statements(DbDialect::Mysql, None, "t", &edits, &cols, &[]).unwrap();
        assert!(out[0].sql.contains("md5(`payload`) = ?"), "{}", out[0].sql);
        assert_eq!(out[0].params.last().unwrap(), &json!(md5_hex(&payload)));
    }

    #[test]
    fn short_binary_columns_fold_too_hex_text_never_equals_bytes() {
        // `payload` = ? with the \\x STRING bound can never equal the column's bytes at ANY
        // length — a binary column addresses through md5() from its first byte, and an
        // ASCII-only binary is covered by the same arm (its bytes digest fine).
        let payload = vec![0xFFu8, 0x00, 0x41];
        let cols = vec![
            col("name", "varchar(80)", false, false),
            col("payload", "varbinary(16)", false, false),
        ];
        let edits = vec![edit(json!({
            "op": "delete",
            "pk": { "name": "row-1", "payload": wire(&payload) },
        }))];
        let out = build_edit_statements(DbDialect::Mysql, None, "t", &edits, &cols, &[]).unwrap();
        assert!(out[0].sql.contains("md5(`payload`) = ?"), "{}", out[0].sql);
        assert_eq!(out[0].params.last().unwrap(), &json!(md5_hex(&payload)));
    }

    #[test]
    fn optimistic_lock_digests_a_binary_original_over_raw_bytes() {
        // W4.2's lock compares every CHANGED column's original; a binary original rides
        // the \\x wire form, and its fold digests the decoded bytes — same reasoning as
        // the keyless address, same false-409 if it does not.
        let payload = vec![0xFFu8; 80];
        let cols = vec![
            col("id", "int", false, true),
            col("payload", "bytea", false, false),
        ];
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 5 },
            "source": { "id": 5, "payload": wire(&payload) },
            "changes": { "payload": wire(&[0x01, 0x02]) },
        }))];
        let out =
            build_edit_statements(DbDialect::Pg, None, "t", &edits, &cols, &["id".to_string()])
                .unwrap();
        assert!(
            out[0].sql.contains("md5(\"payload\") = $"),
            "{}",
            out[0].sql
        );
        assert_eq!(out[0].params.last().unwrap(), &json!(md5_hex(&payload)));
    }

    #[test]
    fn no_pk_edits_address_every_column() {
        // SPEC §data.edits (adminer select.inc.php:440-472): without a primary key a row is
        // addressed by ALL its columns — the panel carries the buffered row's original
        // values in the pk map, so the same map that holds key values on a keyed table
        // holds the whole row here. MySQL clips the statement with LIMIT 1; Postgres has
        // no such syntax and the adapter's affected-rows check guards twins instead.
        let cols = vec![
            col("id", "int", false, false),
            col("name", "varchar", true, false),
        ];
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "alice" },
            "changes": { "name": "ann" },
        }))];
        let out =
            build_edit_statements(DbDialect::Mysql, None, "users", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE `users` SET `name` = ? WHERE `id` = ? AND `name` = ? LIMIT 1"
        );
        assert_eq!(out[0].params, vec![json!("ann"), json!(7), json!("alice")]);
        let out = build_edit_statements(DbDialect::Pg, None, "users", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE \"users\" SET \"name\" = CAST($1 AS varchar) WHERE \"id\" = CAST($2 AS int) AND \"name\" = CAST($3 AS varchar)"
        );
        let edits = vec![edit(
            json!({ "op": "delete", "pk": { "id": 7, "name": "alice" } }),
        )];
        let out =
            build_edit_statements(DbDialect::Mysql, None, "users", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "DELETE FROM `users` WHERE `id` = ? AND `name` = ? LIMIT 1"
        );
        assert_eq!(out[0].params, vec![json!(7), json!("alice")]);
        let out = build_edit_statements(DbDialect::Pg, None, "users", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "DELETE FROM \"users\" WHERE \"id\" = CAST($1 AS int) AND \"name\" = CAST($2 AS varchar)"
        );
    }

    #[test]
    fn no_pk_address_hashes_long_text_with_md5() {
        // adminer select.inc.php:458: a text or binary value longer than 64 bytes addresses
        // the row as md5(col) = ? — the digest keeps the WHERE (and the W1.7 read-back
        // SELECT, which shares this addressing) from carrying whole paragraphs. Both
        // dialects have md5() built in, so the one spelling serves both.
        let long = "x".repeat(100);
        let cols = vec![
            col("id", "int", false, false),
            col("note", "text", true, false),
        ];
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7, "note": long },
            "changes": { "note": "short now" },
        }))];
        let out = build_edit_statements(DbDialect::Mysql, None, "t", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE `t` SET `note` = ? WHERE `id` = ? AND md5(`note`) = ? LIMIT 1"
        );
        assert_eq!(
            out[0].params,
            vec![
                json!("short now"),
                json!(7),
                json!("aed563ecafb4bcc5654c597a421547b2")
            ]
        );
        let out = build_edit_statements(DbDialect::Pg, None, "t", &edits, &cols, &[]).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE \"t\" SET \"note\" = CAST($1 AS text) WHERE \"id\" = CAST($2 AS int) AND md5(\"note\") = $3"
        );
        // Short values compare directly: 64 bytes is adminer's own line.
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7, "note": "x".repeat(64) },
            "changes": { "note": "short now" },
        }))];
        let out = build_edit_statements(DbDialect::Mysql, None, "t", &edits, &cols, &[]).unwrap();
        assert!(out[0].sql.contains("`note` = ? LIMIT 1"), "{}", out[0].sql);
    }

    #[test]
    fn no_pk_edits_refuse_null_and_missing_addressing_values() {
        // "NULL is ambiguous" — adminer's own unique_array comment (functions.inc.php:331):
        // col = NULL never matches, so a row with a NULL column cannot be addressed at all.
        let cols = vec![
            col("id", "int", false, false),
            col("name", "varchar", true, false),
        ];
        let err = build_edit_statements(
            DbDialect::Mysql,
            None,
            "u",
            &[edit(
                json!({ "op": "update", "pk": { "id": 7, "name": null },
                           "changes": { "name": "x" } }),
            )],
            &cols,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("name"), "{err}");
        assert!(err.contains("NULL"), "{err}");
        let err = build_edit_statements(
            DbDialect::Pg,
            None,
            "u",
            &[edit(json!({ "op": "delete", "pk": { "name": "x" } }))],
            &cols,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("id"), "{err}");
        assert!(err.contains("every column"), "{err}");
    }

    #[test]
    fn update_source_compares_each_changed_columns_original() {
        // SPEC §data.edits (cloudbeaver ResultSetEditAction.ts:112-127): an update that carries
        // its row's original values — the source field, or the pk map holding the whole row,
        // which is what the panel posts — compares every CHANGED column against that
        // original in the WHERE. Zero affected rows means another writer moved the row
        // underneath: the adapter refuses the whole batch (409) instead of overwriting.
        let cols = vec![
            col("id", "int", false, true),
            col("name", "varchar", true, false),
            col("city", "varchar", true, false),
        ];
        let pk = vec!["id".to_string()];
        // The full row in the pk map: id addresses, name's original guards the change, city
        // is untouched so it is not compared (only CHANGED columns ride the optimistic lock).
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "old", "city": "la" },
            "changes": { "name": "new" },
        }))];
        let out =
            build_edit_statements(DbDialect::Mysql, None, "users", &edits, &cols, &pk).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE `users` SET `name` = ? WHERE `id` = ? AND `name` = ?"
        );
        assert_eq!(out[0].params, vec![json!("new"), json!(7), json!("old")]);
        let out = build_edit_statements(DbDialect::Pg, None, "users", &edits, &cols, &pk).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE \"users\" SET \"name\" = CAST($1 AS varchar) WHERE \"id\" = CAST($2 AS int) AND \"name\" = CAST($3 AS varchar)"
        );
        // An explicit source field says the same thing (API callers' spelling); a NULL
        // original needs the NULL-safe form — col = NULL matches nothing, and the lock
        // would false-conflict on every commit of a cell that started NULL.
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7 },
            "source": { "id": 7, "note": null },
            "changes": { "note": "written" },
        }))];
        let cols2 = vec![
            col("id", "int", false, true),
            col("note", "text", true, false),
        ];
        let out = build_edit_statements(DbDialect::Mysql, None, "u", &edits, &cols2, &pk).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE `u` SET `note` = ? WHERE `id` = ? AND `note` <=> ?"
        );
        let out = build_edit_statements(DbDialect::Pg, None, "u", &edits, &cols2, &pk).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE \"u\" SET \"note\" = CAST($1 AS text) WHERE \"id\" = CAST($2 AS int) AND \"note\" IS NOT DISTINCT FROM CAST($3 AS text)"
        );
        // A key column change is not compared twice: the pk arm of the WHERE already holds
        // the original key value.
        let edits = vec![edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "x" },
            "changes": { "id": 8, "name": "y" },
        }))];
        let out = build_edit_statements(DbDialect::Mysql, None, "u", &edits, &cols, &pk).unwrap();
        assert_eq!(
            out[0].sql,
            "UPDATE `u` SET `id` = ?, `name` = ? WHERE `id` = ? AND `name` = ?"
        );
    }

    #[test]
    fn no_pk_readback_selects_by_the_same_all_column_address() {
        // SPEC §data.edits: the W1.7 read-back shares the update's addressing, so a no-PK row
        // reads back by the same every-column WHERE — twins come back identical, which is
        // the one case where LIMIT-less SELECT still tells the truth. A row whose values
        // cannot address anything (a NULL among them) reads nothing back rather than
        // failing the commit it is only decorating.
        let cols = vec![
            col("id", "int", false, false),
            col("name", "varchar", true, false),
        ];
        let upd = edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "alice" },
            "changes": { "name": "ann" },
        }));
        match readback_plan(DbDialect::Mysql, None, "users", &cols, &[], &upd) {
            ReadBack::Select(s) => {
                assert_eq!(
                    s.sql,
                    "SELECT `id`, `name` FROM `users` WHERE `id` = ? AND `name` = ?"
                );
                assert_eq!(s.params, vec![json!(7), json!("ann")]);
            }
            other => panic!("no-PK update selects its row back: {other:?}"),
        }
        // The overlay is the whole point: a NULL ORIGINAL whose commit sets a value reads
        // back fine (the post-commit row addresses), while a value changed TO NULL cannot
        // be addressed by any WHERE (col = NULL matches nothing) and reads nothing back.
        let was_null = edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": null },
            "changes": { "name": "ann" },
        }));
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &cols, &[], &was_null),
            ReadBack::Select(_)
        ));
        let to_null = edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "alice" },
            "changes": { "name": null },
        }));
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &cols, &[], &to_null),
            ReadBack::None
        ));
    }

    #[test]
    fn browse_edit_of_parses_and_refuses() {
        assert_eq!(
            edit(json!({ "op": "insert", "values": { "id": 1 } })).op(),
            "insert"
        );
        assert_eq!(
            edit(json!({ "op": "update", "pk": { "id": 1 }, "changes": {} })).op(),
            "update"
        );
        assert_eq!(
            edit(json!({ "op": "delete", "pk": { "id": 1 } })).op(),
            "delete"
        );
        assert!(browse_edit_of(&json!({ "op": "explode" })).is_err());
    }
}
