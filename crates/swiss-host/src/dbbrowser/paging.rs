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

//! Server-side paging: page-size and offset clamps, ordering, and the row and count statements.

use serde_json::{Map, Value};

use super::export::EXPORT_ROW_CAP;
use super::ident::{qualified, quote_ident};
use super::model::{BrowseColumn, BuiltStatement};
use super::values::js_number;
use super::DbDialect;

/// Row-count options the panel offers. 50 is the default: enough to scan, small enough that a
/// wide table paints instantly; anything bigger is an explicit choice.
pub const BROWSE_PAGE_SIZES: [i64; 6] = [10, 20, 50, 100, 200, 500];
/// Rows fetched per page when the caller sends no (or a nonsense) limit.
pub const BROWSE_DEFAULT_PAGE: i64 = 50;
/// Ceiling on a requested page — one grid may never ask for the whole table.
pub const BROWSE_MAX_PAGE: i64 = 500;
/// Tables per page in the lazy table list (mirrors DEFAULT_TABLE_LIMIT of the MCP tools).
pub const BROWSE_TABLES_PAGE: i64 = 200;
pub const BROWSE_TABLES_MAX: i64 = 1000;

// --- paging clamps -------------------------------------------------------------------------------

/// Clamp a requested page size to a sane positive int bounded by BROWSE_MAX_PAGE —
/// `Math.floor(Number(requested))`, fallback when NaN/<=0.
pub fn browse_page_size(requested: Option<&Value>, fallback: i64) -> i64 {
    let n = requested
        .and_then(js_number)
        .map(f64::floor)
        .unwrap_or(f64::NAN);
    if !n.is_finite() || n <= 0.0 {
        return fallback;
    }
    n.min(BROWSE_MAX_PAGE as f64) as i64
}

/// Clamp a non-negative offset.
pub fn browse_offset(requested: Option<&Value>) -> i64 {
    let n = requested
        .and_then(js_number)
        .map(f64::floor)
        .unwrap_or(f64::NAN);
    if n.is_finite() && n > 0.0 {
        n as i64
    } else {
        0
    }
}

/// Clamp a table-list page-size request: a positive int when the caller sent one, capped at
/// `max`, `fallback` otherwise. Shared by the mysql/pg listTables browsers so the Data view clamps
/// one shape only (sql.rs keeps its own table_page_args for the MCP tools — same rule, args
/// object).
pub fn clamp_browse_limit(raw: Option<&Value>, fallback: i64, max: i64) -> i64 {
    let n = raw.and_then(js_number).map(f64::floor).unwrap_or(f64::NAN);
    if n.is_finite() && n > 0.0 {
        n.min(max as f64) as i64
    } else {
        fallback
    }
}

/// Clamp an export row cap request.
pub fn export_row_limit(requested: Option<&Value>) -> i64 {
    let n = requested
        .and_then(js_number)
        .map(f64::floor)
        .unwrap_or(f64::NAN);
    if !n.is_finite() || n <= 0.0 {
        return EXPORT_ROW_CAP;
    }
    n.min(EXPORT_ROW_CAP as f64) as i64
}

// --- read paging ---------------------------------------------------------------------------------

/// A vetted ORDER BY clause, or None when the caller asked for none. Unknown columns and unknown
/// directions are refused rather than ignored — a silently dropped sort changes what the grid
/// shows page to page, which looks like a bug in the paging instead of the sort.
pub fn browse_order(
    columns: &[String],
    order: Option<&str>,
    dir: Option<&str>,
    dialect: DbDialect,
) -> Result<Option<String>, String> {
    // Node's `if (!order)` — absent and empty both mean "no sort".
    let Some(order) = order.filter(|o| !o.is_empty()) else {
        return Ok(None);
    };
    if !columns.iter().any(|c| c == order) {
        return Err(format!("cannot sort by unknown column: {order}"));
    }
    let d = match dir {
        Some("desc") => "DESC",
        // Node: dir === "asc" || dir == null → ASC (undefined and null both land here).
        Some("asc") | None => "ASC",
        Some(other) => return Err(format!("sort direction must be asc or desc, not '{other}'")),
    };
    Ok(Some(format!("{} {}", quote_ident(dialect, order)?, d)))
}

/// Sort keys the Data view's table list accepts, mirroring Node's BrowseTableSort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableSortKey {
    Name,
    Rows,
    Size,
}

/// A validated table-list sort: key + direction. Name is the default (a missing or empty value,
/// never a guess).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableSort {
    pub key: TableSortKey,
    pub desc: bool,
}

/// Vet the panel's `sort`/`dir` query pair. Unknown keys and directions are refused rather than
/// ignored — the same rule as [`browse_order`], for the same reason: a silently dropped sort
/// changes what the pages show.
pub fn browse_table_sort(sort: Option<&str>, dir: Option<&str>) -> Result<TableSort, String> {
    let key = match sort.filter(|s| !s.is_empty()) {
        None | Some("name") => TableSortKey::Name,
        Some("rows") => TableSortKey::Rows,
        Some("size") => TableSortKey::Size,
        Some(other) => {
            return Err(format!(
                "cannot sort the table list by unknown key: {other}"
            ))
        }
    };
    let desc = match dir.filter(|d| !d.is_empty()) {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(other) => {
            return Err(format!(
                "table list sort direction must be asc or desc, not '{other}'"
            ))
        }
    };
    Ok(TableSort { key, desc })
}

#[allow(clippy::too_many_arguments)]
/// The bounded SELECT behind one grid page. LIMIT/OFFSET are clamped integers, so they are
/// inlined rather than bound (both dialects accept that only for literal ints); the WHERE
/// fragment's parameters are the statement's only bound values, so their numbering starts at 1.
/// `select_exprs` are the final column expressions (see [`quoted_exprs`] / [`pg_typed_exprs`]).
pub fn browse_rows_sql(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    select_exprs: &[String],
    order: Option<&str>,
    offset: i64,
    limit: i64,
    where_frag: &str,
    where_params: &[Value],
) -> Result<BuiltStatement, String> {
    let cols = if select_exprs.is_empty() {
        "*".to_string()
    } else {
        select_exprs.join(", ")
    };
    let order_by = order.map(|o| format!(" ORDER BY {o}")).unwrap_or_default();
    Ok(BuiltStatement {
        sql: format!(
            "SELECT {cols} FROM {}{where_frag}{order_by} LIMIT {limit} OFFSET {offset}",
            qualified(dialect, schema, table)?
        ),
        params: where_params.to_vec(),
    })
}

/// The plain SELECT expressions for bare column names — the MySQL form (its wire protocol is
/// all text, so every type arrives as a string the catch-all decodes).
pub fn quoted_exprs(dialect: DbDialect, names: &[String]) -> Result<Vec<String>, String> {
    names.iter().map(|c| quote_ident(dialect, c)).collect()
}

/// information_schema data_types whose BINARY wire form the pg row decoder models (see
/// pg.rs `column_to_value`); anything else — uuid, inet/cidr/macaddr, money, tsvector,
/// interval, custom types, ARRAY — would reach the decoder's String catch-all, which cannot
/// read binary and silently drops the value.
const PG_DECODED_TYPES: [&str; 17] = [
    "smallint",
    "integer",
    "bigint",
    "real",
    "double precision",
    "boolean",
    "text",
    "character varying",
    "character",
    "name",
    "numeric",
    "json",
    "jsonb",
    "bytea",
    "date",
    "timestamp without time zone",
    "timestamp with time zone",
];

/// The SELECT expressions for one pg grid page: modeled types stay the bare quoted column;
/// every other type is CAST to text in the SELECT itself. Two birds: the extended protocol
/// answers those columns in a binary form the decoder cannot read (a uuid column would come
/// back missing from every row), and the text it becomes is exactly the string node-pg handed
/// the Node build's grid. "time with/without time zone" stays modeled in spirit: TIME decodes,
/// but TIMETZ does not — it is absent from the list, so it casts (node-pg passed it through as
/// text too).
pub fn pg_typed_exprs(columns: &[BrowseColumn]) -> Result<Vec<String>, String> {
    columns
        .iter()
        .map(|c| {
            let quoted = quote_ident(DbDialect::Pg, &c.name)?;
            if PG_DECODED_TYPES.contains(&c.data_type.to_ascii_lowercase().as_str()) {
                Ok(quoted)
            } else {
                Ok(format!("{quoted}::text AS {quoted}"))
            }
        })
        .collect()
}

pub fn browse_count_sql(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    where_frag: &str,
    where_params: &[Value],
) -> Result<BuiltStatement, String> {
    Ok(BuiltStatement {
        sql: format!(
            "SELECT COUNT(*) AS total FROM {}{where_frag}",
            qualified(dialect, schema, table)?
        ),
        params: where_params.to_vec(),
    })
}

/// Turn a buffered edit list into executable statements: one statement per edit, in order, with
/// every value bound and every identifier pre-vetted. Column names the table does not have are
/// dropped from SET/INSERT (a stale page may reference a dropped column); an edit that ends up
/// with nothing to do is an error, because "commit 3 changes" that silently did 2 is a lie.
///
/// SPEC §data.browse: the fetch-one-more paging probe. The adapter asks for `limit + 1` rows; a
/// full page plus a spare row means another page exists, and the spare is dropped before the
/// reply. Cheaper and more truthful than offset arithmetic against a COUNT that concurrent
/// writes may have already moved — the count stays in the reply for the "x–y of z" line only.
pub fn page_and_next(
    mut rows: Vec<Map<String, Value>>,
    limit: i64,
) -> (Vec<Map<String, Value>>, bool) {
    let next = rows.len() as i64 > limit;
    if next {
        rows.truncate(limit.max(0) as usize);
    }
    (rows, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::filter::build_filter_where;
    use crate::dbbrowser::test_util::{filter, names};

    #[test]
    fn read_paging_sql() {
        let stmt = browse_rows_sql(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &["`id`".to_string(), "`name`".to_string()],
            Some("`id` DESC"),
            40,
            20,
            "",
            &[],
        )
        .unwrap();
        assert_eq!(
            stmt.sql,
            "SELECT `id`, `name` FROM `app`.`users` ORDER BY `id` DESC LIMIT 20 OFFSET 40"
        );
        assert_eq!(stmt.params, Vec::<Value>::new());

        let stmt = browse_rows_sql(
            DbDialect::Pg,
            Some("public"),
            "users",
            &["\"id\"".to_string()],
            None,
            0,
            500,
            "",
            &[],
        )
        .unwrap();
        assert_eq!(
            stmt.sql,
            "SELECT \"id\" FROM \"public\".\"users\" LIMIT 500 OFFSET 0"
        );

        let stmt = browse_count_sql(DbDialect::Pg, None, "t", "", &[]).unwrap();
        assert_eq!(stmt.sql, "SELECT COUNT(*) AS total FROM \"t\"");
    }

    #[test]
    fn browse_order_rules() {
        let cols = vec!["id".to_string()];
        let err = browse_order(&cols, Some("evil; --"), Some("asc"), DbDialect::Mysql).unwrap_err();
        assert!(err.contains("unknown column"), "{err}");
        let err = browse_order(&cols, Some("id"), Some("sideways"), DbDialect::Pg).unwrap_err();
        assert!(err.contains("asc or desc"), "{err}");
        assert_eq!(
            browse_order(&cols, Some("id"), Some("desc"), DbDialect::Mysql).unwrap(),
            Some("`id` DESC".to_string())
        );
        assert_eq!(
            browse_order(&cols, None, None, DbDialect::Mysql).unwrap(),
            None
        );
        // Empty strings read as "no sort" the way JS falsiness did.
        assert_eq!(
            browse_order(&cols, Some(""), None, DbDialect::Mysql).unwrap(),
            None
        );
    }

    #[test]
    fn page_and_next_truncates_the_probe_row() {
        // SPEC §data.browse: limit+1 fetched, the spare row reported as nextPage and dropped.
        let row = || -> Map<String, Value> {
            [("id", 1)]
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect()
        };
        let (page, next) = page_and_next(vec![row(), row(), row()], 2);
        assert!(next);
        assert_eq!(page.len(), 2);
        let (page, next) = page_and_next(vec![row(), row()], 2);
        assert!(!next);
        assert_eq!(page.len(), 2);
        let (page, next) = page_and_next(vec![row()], 2);
        assert!(!next);
        assert_eq!(page.len(), 1);
    }

    #[test]
    fn filters_ride_along_in_rows_and_count() {
        let w = build_filter_where(
            DbDialect::Mysql,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "id", "op": "eq", "value": "7" }])),
        )
        .unwrap();
        let rows = browse_rows_sql(
            DbDialect::Mysql,
            None,
            "users",
            &["`id`".to_string(), "`name`".to_string()],
            None,
            0,
            50,
            &w.frag,
            &w.params,
        )
        .unwrap();
        assert_eq!(
            rows.sql,
            "SELECT `id`, `name` FROM `users` WHERE `id` = ? LIMIT 50 OFFSET 0"
        );
        assert_eq!(rows.params, vec![json!(7)]);
        let count = browse_count_sql(DbDialect::Mysql, None, "users", &w.frag, &w.params).unwrap();
        assert_eq!(
            count.sql,
            "SELECT COUNT(*) AS total FROM `users` WHERE `id` = ?"
        );
        assert_eq!(count.params, vec![json!(7)]);
    }

    #[test]
    fn clamp_browse_limit_falls_back_and_caps() {
        assert_eq!(clamp_browse_limit(None, 200, 1000), 200);
        assert_eq!(clamp_browse_limit(Some(&json!(50)), 200, 1000), 50);
        assert_eq!(clamp_browse_limit(Some(&json!(99_999)), 200, 1000), 1000);
        assert_eq!(clamp_browse_limit(Some(&json!("12")), 200, 1000), 12);
        assert_eq!(clamp_browse_limit(Some(&json!(0)), 200, 1000), 200);
        assert_eq!(clamp_browse_limit(Some(&json!("x")), 200, 1000), 200);
    }
}
