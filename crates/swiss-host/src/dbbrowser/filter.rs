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

//! Row filters: the panel's filter list and grep line into one parameterised WHERE.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::edit::typed_ph;
use super::ident::quote_ident;
use super::model::{BrowseColumn, BrowseFilter, BuiltWhere, FilterColumn};
use super::values::{float_to_js_string, js_to_string};
use super::DbDialect;

/// Turn a `grep` argument into a LIKE/ILIKE pattern that matches it as a literal substring.
///
/// `users` becomes `%users%`, so `p_users` and `users_settings` both match. `%` and `_` (and the
/// `!` that escapes them — every LIKE here runs with `ESCAPE '!'`) are escaped first. Case
/// insensitivity comes from the database side: ILIKE in Postgres, and MySQL's default
/// case-insensitive collation for LIKE.
pub fn like_contains(filter: &str) -> String {
    let mut out = String::with_capacity(filter.len() + 2);
    out.push('%');
    for c in filter.chars() {
        if matches!(c, '!' | '%' | '_') {
            out.push('!');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Total LIKE patterns one table-list grep may expand to (AND terms x OR alternatives) — the
/// grep-side twin of the route's filter-count cap, so a hand-written URL cannot ask for a
/// hundred-term WHERE either.
pub const BROWSE_GREP_MAX: usize = 32;

/// One side of the sidebar grep grammar (SPEC §data.browse): comma-separated terms AND together,
/// "|" inside a term is OR, "*" is a wildcard, everything case-insensitive (ILIKE on Postgres,
/// MySQL's default _ci collation). The panel's pure `dbFilterMatches` speaks the same language
/// client-side. Returns one list of LIKE patterns per AND-term — each escaped and %wrapped as
/// `like_contains` would wrap it, except a term that carries its own "*", which anchors the
/// pattern the wildcards shape ("user*" is `user%`, not `%user%%`). None when the grep uses none
/// of the three grammar characters: that is the plain substring the list always matched, and the
/// caller keeps its single-LIKE SQL byte-for-byte. Empty terms drop; empty alternatives never
/// match (they are simply not bound).
pub fn grep_patterns(grep: &str) -> Result<Option<Vec<Vec<String>>>, String> {
    let trimmed = grep.trim();
    if !trimmed.contains(['*', '|', ',']) {
        return Ok(None);
    }
    let mut terms: Vec<Vec<String>> = Vec::new();
    let mut total = 0usize;
    for term in trimmed.split(',') {
        let mut alts: Vec<String> = Vec::new();
        for alt in term.split('|') {
            let alt = alt.trim();
            if alt.is_empty() {
                continue;
            }
            let pattern = if alt.contains('*') {
                // The user supplied the wildcards: escape the literals, translate the * to %,
                // and do NOT add the substring wrap — "user*" means "starts with user".
                let mut out = String::with_capacity(alt.len() + 2);
                for c in alt.chars() {
                    if matches!(c, '!' | '%' | '_') {
                        out.push('!');
                        out.push(c);
                    } else if c == '*' {
                        out.push('%');
                    } else {
                        out.push(c);
                    }
                }
                out
            } else {
                like_contains(alt)
            };
            alts.push(pattern);
            total += 1;
            if total > BROWSE_GREP_MAX {
                return Err(format!(
                    "table filter accepts at most {BROWSE_GREP_MAX} patterns"
                ));
            }
        }
        if !alts.is_empty() {
            terms.push(alts);
        }
    }
    Ok(Some(terms))
}

/// The grep predicate as a SQL fragment: " AND (col ILIKE $n ESCAPE '!' OR ...)" per AND-term,
/// ready to append to a table-list WHERE. Placeholders are "?" on MySQL and "$n" on Postgres,
/// numbered to follow `first_param` existing bound parameters (Postgres binds the schema first).
/// None for a plain substring grep — the caller keeps its own single-LIKE SQL.
pub fn grep_where(
    dialect: DbDialect,
    subject: &str,
    grep: &str,
    first_param: usize,
) -> Result<Option<BuiltWhere>, String> {
    let Some(terms) = grep_patterns(grep)? else {
        return Ok(None);
    };
    let like = if dialect == DbDialect::Pg {
        "ILIKE"
    } else {
        "LIKE"
    };
    let mut params: Vec<Value> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for alts in terms {
        let mut phs: Vec<String> = Vec::with_capacity(alts.len());
        for pattern in alts {
            params.push(Value::String(pattern));
            let n = first_param + params.len();
            phs.push(if dialect == DbDialect::Mysql {
                "?".to_string()
            } else {
                format!("${n}")
            });
        }
        let one: Vec<String> = phs
            .iter()
            .map(|p| format!("{subject} {like} {p} ESCAPE '!'"))
            .collect();
        parts.push(format!("({})", one.join(" OR ")));
    }
    Ok(Some(BuiltWhere {
        frag: format!(" AND {}", parts.join(" AND ")),
        params,
    }))
}

/// The operators a grid filter may use. Deliberately a whitelist: the panel offers exactly these
/// and anything else is refused, because the fragment is interpolated into SQL by identifier —
/// every VALUE stays a bound parameter, but the operator slot must never be free text.
pub const BROWSE_FILTER_OPS: [&str; 13] = [
    "eq",
    "ne",
    "gt",
    "gte",
    "lt",
    "lte",
    "in",
    "notIn",
    "between",
    "like",
    "notLike",
    "isNull",
    "isNotNull",
];

/// Values one in/not-in filter may list. The route caps FILTER COUNT; this caps LIST LENGTH,
/// so a hand-written URL cannot ask for a thousand-term IN either.
pub const BROWSE_IN_MAX: usize = 100;

// --- row filters ---------------------------------------------------------------------------------

/// Bind a numeric-looking string as a number only while Number() is lossless; otherwise bind the
/// original string. Integers must fit a JS safe integer — a snowflake ID (19 digits) loses its low
/// bits through Number() (734023681584275456 would arrive as …500 in every row) — and decimals
/// must round-trip exactly (String(Number(s)) === s, so "3.14" binds as a number while "1.10" and
/// beyond-double-precision decimals stay strings). String-bound values stay exact: MySQL coerces
/// them against the column, and Postgres infers the parameter type from the comparison or the
/// column, so neither dialect silently rounds.
///
/// Shared by the grid filters (build_filter_where) and CSV import (map_import_rows), because the
/// two once drifted: filters already refused lossy integers while import rounded them into the
/// table.
pub fn numeric_bind_value(raw: &str) -> Value {
    let s = raw.trim();
    if !is_plain_number(s) {
        return Value::String(raw.to_string());
    }
    if !s.contains('.') {
        // Number.isSafeInteger: within ±(2^53-1) bind the number, else keep the exact string.
        return match s.parse::<i64>() {
            Ok(n) if n.unsigned_abs() <= 9_007_199_254_740_991 => json!(n),
            _ => Value::String(s.to_string()),
        };
    }
    match s.parse::<f64>() {
        // String(n) === s — JS prints plain notation only inside [1e-6, 1e21), and only a
        // plain-digit string can equal s, so outside that window the value always stays a string.
        Ok(n) => {
            let a = n.abs();
            let js_plain = a == 0.0 || (1e-6..1e21).contains(&a);
            if js_plain && float_to_js_string(n) == s {
                json!(n)
            } else {
                Value::String(s.to_string())
            }
        }
        Err(_) => Value::String(s.to_string()),
    }
}

/// `/^-?\d+(\.\d+)?$/` by hand (ADR-007): an optional minus, digits, then optionally a dot with
/// more digits — no sign-plus, no exponent, no bare dot.
fn is_plain_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && b[i] == b'-' {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return false;
        }
    }
    i == b.len()
}

/// information_schema data_types that are textual, so `col ILIKE $n` is valid on Postgres as-is.
/// Anything else (numbers, dates, booleans, json, arrays, …) must be CAST to text first —
/// `integer ILIKE text` is Postgres error 42883, operator does not exist.
const PG_TEXTUAL_TYPES: [&str; 5] = ["text", "character varying", "character", "name", "citext"];

/// The left side of a contains-filter comparison. Postgres casts non-text columns to text
/// explicitly; MySQL stays the bare column because its string context converts implicitly.
fn like_subject(dialect: DbDialect, col: &str, data_type: Option<&str>) -> Result<String, String> {
    let quoted = quote_ident(dialect, col)?;
    let textual = data_type
        .map(|t| PG_TEXTUAL_TYPES.contains(&t.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    if dialect != DbDialect::Pg || data_type.is_none() || textual {
        return Ok(quoted);
    }
    Ok(format!("CAST({quoted} AS text)"))
}

/// Build the WHERE fragment for a grid page: every identifier vetted + quoted, every value a
/// bound parameter. `columns` may be bare names or full BrowseColumns — the type info is optional
/// and only feeds the contains-filter cast above (callers without it get the plain column).
/// "contains" matches as a substring — like_contains does the wildcard escaping, and the
/// ESCAPE '!' clause keeps a user's own % and _ literal — with ILIKE on Postgres (CAST to text on
/// non-text columns) so both dialects filter case-insensitively. Multiple filters stack with AND.
pub fn build_filter_where(
    dialect: DbDialect,
    columns: &[FilterColumn],
    filters: &[BrowseFilter],
) -> Result<BuiltWhere, String> {
    let mut params: Vec<Value> = Vec::new();
    // bind() has already pushed the value when this runs, so the array length IS the
    // parameter's 1-based number.
    let bind = |params: &mut Vec<Value>, v: Value| -> String {
        params.push(v);
        if dialect == DbDialect::Mysql {
            "?".to_string()
        } else {
            format!("${}", params.len())
        }
    };
    let type_of: HashMap<&str, Option<&str>> =
        columns.iter().map(|c| (c.name(), c.data_type())).collect();
    let known: HashSet<&str> = columns.iter().map(|c| c.name()).collect();
    let mut parts: Vec<String> = Vec::new();
    for f in filters {
        if !known.contains(f.column.as_str()) {
            return Err(format!("cannot filter by unknown column: {}", f.column));
        }
        let col = quote_ident(dialect, &f.column)?;
        match f.op.as_str() {
            "isNull" => parts.push(format!("{col} IS NULL")),
            "isNotNull" => parts.push(format!("{col} IS NOT NULL")),
            "like" | "notLike" => {
                let v = f.value.as_ref().filter(|v| !v.is_null());
                let text = v.map(|x| js_to_string(Some(x))).unwrap_or_default();
                if v.is_none() || text.is_empty() {
                    return Err(format!("a '{}' filter needs a value", f.op));
                }
                let neg = if f.op == "notLike" { "NOT " } else { "" };
                let like = if dialect == DbDialect::Pg {
                    "ILIKE"
                } else {
                    "LIKE"
                };
                let dtype = type_of.get(f.column.as_str()).copied().flatten();
                let subject = like_subject(dialect, &f.column, dtype)?;
                // A "*" in the value is the user's own wildcard (SPEC §data.browse): same escaping
                // as like_contains, * translated to %, still wrapped as a substring — a value
                // without * keeps the exact pattern the operator always built.
                let pattern = if text.contains('*') {
                    let mut out = String::with_capacity(text.len() + 2);
                    out.push('%');
                    for c in text.chars() {
                        if matches!(c, '!' | '%' | '_') {
                            out.push('!');
                            out.push(c);
                        } else if c == '*' {
                            out.push('%');
                        } else {
                            out.push(c);
                        }
                    }
                    out.push('%');
                    out
                } else {
                    like_contains(&text)
                };
                let ph = bind(&mut params, Value::String(pattern));
                parts.push(format!("{subject} {neg}{like} {ph} ESCAPE '!'"));
            }
            "eq" | "ne" | "gt" | "gte" | "lt" | "lte" => {
                let v = f.value.as_ref().filter(|v| !v.is_null());
                let text = v.map(|x| js_to_string(Some(x))).unwrap_or_default();
                if v.is_none() || text.is_empty() {
                    return Err("a comparison filter needs a value".into());
                }
                let sql_op = match f.op.as_str() {
                    "eq" => "=",
                    "ne" => "<>",
                    "gt" => ">",
                    "gte" => ">=",
                    "lt" => "<",
                    _ => "<=",
                };
                // Same typed placeholder as the edit statements: sqlx binds a String as `text`,
                // and `"bigint" = $1::text` is a 42883 on Postgres.
                bind(&mut params, numeric_bind_value(&text));
                let typed = typed_ph(
                    dialect,
                    &params,
                    type_of.get(f.column.as_str()).copied().flatten(),
                );
                parts.push(format!("{col} {sql_op} {typed}"));
            }
            // SPEC §data.browse: IN/NOT IN bind a comma-separated list, BETWEEN the closed interval
            // lo,hi. Every item is its own bound parameter carrying the same typed placeholder a
            // comparison gets, so a list over a bigint column on Postgres does not hit the 42883
            // a bare text bind would.
            "in" | "notIn" | "between" => {
                let v = f.value.as_ref().filter(|v| !v.is_null());
                let text = v.map(|x| js_to_string(Some(x))).unwrap_or_default();
                let items: Vec<&str> = text.split(',').map(str::trim).collect();
                if f.op == "between" {
                    if items.len() != 2 || items.iter().any(|s| s.is_empty()) {
                        return Err("a 'between' filter needs exactly two values: lo,hi".into());
                    }
                } else if items.iter().any(|s| s.is_empty()) {
                    // Covers the empty value too: "".split(',') is [""].
                    return Err(format!(
                        "an '{}' filter needs a comma-separated list of values",
                        f.op
                    ));
                } else if items.len() > BROWSE_IN_MAX {
                    return Err(format!(
                        "an '{}' filter accepts at most {BROWSE_IN_MAX} values",
                        f.op
                    ));
                }
                let dtype = type_of.get(f.column.as_str()).copied().flatten();
                let mut phs: Vec<String> = Vec::with_capacity(items.len());
                for item in items {
                    bind(&mut params, numeric_bind_value(item));
                    // params.len() IS this item's 1-based number — bind() just pushed it.
                    phs.push(typed_ph(dialect, &params, dtype));
                }
                if f.op == "between" {
                    parts.push(format!("{col} BETWEEN {} AND {}", phs[0], phs[1]));
                } else {
                    let neg = if f.op == "notIn" { "NOT " } else { "" };
                    parts.push(format!("{col} {neg}IN ({})", phs.join(", ")));
                }
            }
            other => return Err(format!("unknown filter operator: {other}")),
        }
    }
    let frag = if parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", parts.join(" AND "))
    };
    Ok(BuiltWhere { frag, params })
}

/// Read the `filters` option the panel posted (a JSON array of {column, op, value}) into the
/// typed shape build_filter_where guards. Structural validation happened at the route; column and
/// operator vetting stays in build_filter_where, next to the SQL it guards.
pub fn browse_filters_of(v: Option<&Value>) -> Vec<BrowseFilter> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .map(|f| BrowseFilter {
            // A missing column stringifies the way String(undefined) did, so the unknown-column
            // refusal reads exactly like the Node build's.
            column: js_to_string(f.get("column")),
            op: js_to_string(f.get("op")),
            value: f.get("value").filter(|v| !v.is_null()).cloned(),
        })
        .collect()
}

/// The grid-filter WHERE behind rows, export and COUNT — ONE assembly point for the three
/// (SPEC §data.export), so the filter applied to a page is by construction the filter applied to
/// the download that claims to export it. Values stay bound parameters and identifier vetting
/// stays inside build_filter_where, next to the SQL it guards.
pub fn browse_where(
    dialect: DbDialect,
    columns: &[BrowseColumn],
    filters: Option<&Value>,
) -> Result<BuiltWhere, String> {
    let typed: Vec<FilterColumn> = columns
        .iter()
        .map(|c| FilterColumn::Column(c.clone()))
        .collect();
    build_filter_where(dialect, &typed, &browse_filters_of(filters))
}

#[cfg(test)]
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    use crate::dbbrowser::paging::{browse_count_sql, browse_rows_sql};
    use crate::dbbrowser::test_util::{col, filter, names};

    #[test]
    fn filters_bind_comparisons_as_parameters() {
        let out = build_filter_where(
            DbDialect::Mysql,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "id", "op": "gte", "value": "42" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE `id` >= ?");
        assert_eq!(out.params, vec![json!(42)]);

        let pg = build_filter_where(
            DbDialect::Pg,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "name", "op": "eq", "value": "alice' --" }])),
        )
        .unwrap();
        assert_eq!(pg.frag, " WHERE \"name\" = $1");
        assert_eq!(pg.params, vec![json!("alice' --")]);
    }

    #[test]
    fn filters_keep_snowflake_ids_exact() {
        let n = names(&["id", "name"]);
        let out = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "id", "op": "eq", "value": "1120230230635368448" }])),
        )
        .unwrap();
        assert_eq!(out.params, vec![json!("1120230230635368448")]);
        let pg = build_filter_where(
            DbDialect::Pg,
            &n,
            &filter(json!([{ "column": "id", "op": "eq", "value": "9007199254740993" }])),
        )
        .unwrap();
        assert_eq!(pg.params, vec![json!("9007199254740993")]);
        // The boundary itself stays a number, and decimals still bind as numbers.
        let out = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "id", "op": "eq", "value": "9007199254740991" }])),
        )
        .unwrap();
        assert_eq!(out.params, vec![json!(9_007_199_254_740_991i64)]);
        let out = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "id", "op": "eq", "value": "3.14" }])),
        )
        .unwrap();
        assert_eq!(out.params, vec![json!(3.14)]);
    }

    #[test]
    fn filters_stack_with_and() {
        let out = build_filter_where(
            DbDialect::Pg,
            &names(&["id", "name"]),
            &filter(json!([
                { "column": "id", "op": "lt", "value": "10" },
                { "column": "name", "op": "ne", "value": "bob" },
            ])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE \"id\" < $1 AND \"name\" <> $2");
        assert_eq!(out.params, vec![json!(10), json!("bob")]);
    }

    #[test]
    fn contains_filters_escape_wildcards() {
        let my = build_filter_where(
            DbDialect::Mysql,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "name", "op": "like", "value": "50%_off" }])),
        )
        .unwrap();
        assert_eq!(my.frag, " WHERE `name` LIKE ? ESCAPE '!'");
        assert_eq!(my.params, vec![json!("%50!%!_off%")]);
        let pg = build_filter_where(
            DbDialect::Pg,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "name", "op": "notLike", "value": "x" }])),
        )
        .unwrap();
        assert_eq!(pg.frag, " WHERE \"name\" NOT ILIKE $1 ESCAPE '!'");
    }

    #[test]
    fn is_null_filters_take_no_value() {
        let out = build_filter_where(
            DbDialect::Mysql,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "name", "op": "isNull" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE `name` IS NULL");
        assert_eq!(out.params, Vec::<Value>::new());
        let out = build_filter_where(
            DbDialect::Pg,
            &names(&["id", "name"]),
            &filter(json!([{ "column": "id", "op": "isNotNull" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE \"id\" IS NOT NULL");
    }

    #[test]
    fn filters_refuse_unknown_input() {
        let n = names(&["id", "name"]);
        let err = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "evil; --", "op": "eq", "value": "1" }])),
        )
        .unwrap_err();
        assert!(err.contains("unknown column"), "{err}");
        let err = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "id", "op": "DROP", "value": "1" }])),
        )
        .unwrap_err();
        assert!(err.contains("unknown filter operator"), "{err}");
        let err = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "id", "op": "eq" }])),
        )
        .unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
        let err = build_filter_where(
            DbDialect::Mysql,
            &n,
            &filter(json!([{ "column": "name", "op": "like", "value": "" }])),
        )
        .unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
        // An empty filter set is no WHERE at all.
        let out = build_filter_where(DbDialect::Mysql, &n, &[]).unwrap();
        assert_eq!(out.frag, "");
        assert_eq!(out.params, Vec::<Value>::new());
    }

    #[test]
    fn grep_patterns_parse_the_grammar_and_keep_plain_substrings_alone() {
        // SPEC §data.browse: comma terms AND, | OR, * wildcard. A grep without any of the three
        // grammar characters is the substring the list always matched — None, so the caller's
        // single-LIKE SQL stays byte-for-byte what it was.
        assert!(grep_patterns("").unwrap().is_none());
        assert!(grep_patterns("users").unwrap().is_none());
        assert!(grep_patterns("a%b").unwrap().is_none()); // % and _ are literals, not grammar
        let g = grep_patterns("user*|account").unwrap().expect("grammar");
        assert_eq!(g, vec![vec!["user%".to_string(), "%account%".to_string()]]);
        let g = grep_patterns("a, b*|c").unwrap().expect("grammar");
        assert_eq!(
            g,
            vec![
                vec!["%a%".to_string()],
                vec!["b%".to_string(), "%c%".to_string()]
            ]
        );
        // Empty terms drop, empty alternatives never match, LIKE metacharacters stay literal.
        let g = grep_patterns(" , a | , ").unwrap().expect("grammar");
        assert_eq!(g, vec![vec!["%a%".to_string()]]);
        let g = grep_patterns("a!b").unwrap();
        assert!(g.is_none(), "! alone is not grammar");
        // A hand-written URL cannot ask for an unbounded WHERE either.
        let big = vec!["a|b"; 17].join(",");
        let err = grep_patterns(&big).unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn grep_where_numbers_placeholders_per_dialect() {
        // Postgres numbers after the schema bind ($1): first pattern is $2.
        let w = grep_where(DbDialect::Pg, "c.relname", "user*|account", 1)
            .unwrap()
            .expect("grammar");
        assert_eq!(
            w.frag,
            " AND (c.relname ILIKE $2 ESCAPE '!' OR c.relname ILIKE $3 ESCAPE '!')"
        );
        assert_eq!(w.params, vec![json!("user%"), json!("%account%")]);
        // MySQL binds positionally and LIKE is case-insensitive under the default collation.
        let w = grep_where(DbDialect::Mysql, "table_name", "user*|account", 0)
            .unwrap()
            .expect("grammar");
        assert_eq!(
            w.frag,
            " AND (table_name LIKE ? ESCAPE '!' OR table_name LIKE ? ESCAPE '!')"
        );
        // A plain substring grep produces no fragment at all — the caller keeps its own SQL.
        assert!(grep_where(DbDialect::Pg, "c.relname", "users", 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_star_in_a_contains_value_is_a_like_wildcard() {
        // SPEC §data.browse: inside a contains-filter value, * means "any run" — translated to LIKE's
        // own % after the metacharacters are escaped, so the operator still matches as a
        // substring but a value's own wildcards shape it.
        let out = build_filter_where(
            DbDialect::Mysql,
            &names(&["name"]),
            &filter(json!([{ "column": "name", "op": "like", "value": "a*b" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE `name` LIKE ? ESCAPE '!'");
        assert_eq!(out.params, vec![json!("%a%b%")]);
        let out = build_filter_where(
            DbDialect::Mysql,
            &names(&["name"]),
            &filter(json!([{ "column": "name", "op": "like", "value": "100%" }])),
        )
        .unwrap();
        // A literal % stays literal (escaped); only * translates.
        assert_eq!(out.params, vec![json!("%100!%%")]);
    }

    #[test]
    fn in_not_in_and_between_bind_every_value() {
        // SPEC §data.browse: IN/NOT IN split the value on commas and bind each item; BETWEEN takes
        // exactly lo,hi (the closed interval). Values stay bound parameters with the same typed
        // placeholder a comparison gets — never inlined into the SQL.
        let cols = vec![FilterColumn::Column(col("id", "bigint", false, true))];
        let out = build_filter_where(
            DbDialect::Mysql,
            &cols,
            &filter(json!([{ "column": "id", "op": "in", "value": "1, 2, 3" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE \x60id\x60 IN (?, ?, ?)");
        assert_eq!(out.params, vec![json!(1), json!(2), json!(3)]);
        let out = build_filter_where(
            DbDialect::Mysql,
            &cols,
            &filter(json!([{ "column": "id", "op": "notIn", "value": "4,5" }])),
        )
        .unwrap();
        assert_eq!(out.frag, " WHERE \x60id\x60 NOT IN (?, ?)");
        assert_eq!(out.params, vec![json!(4), json!(5)]);
        // Postgres types every placeholder against the column, same as a comparison.
        let out = build_filter_where(
            DbDialect::Pg,
            &cols,
            &filter(json!([{ "column": "id", "op": "between", "value": "10,20" }])),
        )
        .unwrap();
        assert_eq!(
            out.frag,
            " WHERE \"id\" BETWEEN CAST($1 AS bigint) AND CAST($2 AS bigint)"
        );
        assert_eq!(out.params, vec![json!(10), json!(20)]);
        // Text items stay strings — MySQL coerces them against the column itself.
        let out = build_filter_where(
            DbDialect::Mysql,
            &cols,
            &filter(json!([{ "column": "id", "op": "in", "value": "a,b" }])),
        )
        .unwrap();
        assert_eq!(out.params, vec![json!("a"), json!("b")]);
        // Empty or malformed lists are the caller's whole mistake, not a silent default.
        for bad in ["", "1,,2", "1,  "] {
            let err = build_filter_where(
                DbDialect::Mysql,
                &cols,
                &filter(json!([{ "column": "id", "op": "in", "value": bad }])),
            )
            .unwrap_err();
            assert!(err.contains("comma-separated"), "{bad}: {err}");
        }
        for bad in ["", "1", "1,2,3", "1,"] {
            let err = build_filter_where(
                DbDialect::Mysql,
                &cols,
                &filter(json!([{ "column": "id", "op": "between", "value": bad }])),
            )
            .unwrap_err();
            assert!(err.contains("two values"), "{bad}: {err}");
        }
        // A hand-written URL cannot ask for an unbounded IN list.
        let long = vec!["7"; BROWSE_IN_MAX + 1].join(",");
        let err = build_filter_where(
            DbDialect::Mysql,
            &cols,
            &filter(json!([{ "column": "id", "op": "in", "value": long }])),
        )
        .unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn browse_where_is_the_one_assembly_rows_and_export_share() {
        // SPEC §data.export: rows, COUNT and export all build their WHERE through browse_where, so
        // the download and the grid can never disagree — and an export with filters carries
        // the same bound values the page did (never inlined into the SQL).
        let columns = vec![
            col("id", "bigint", false, true),
            col("name", "text", true, false),
        ];
        let w = browse_where(
            DbDialect::Mysql,
            &columns,
            Some(&json!([{ "column": "name", "op": "like", "value": "al" }])),
        )
        .unwrap();
        assert_eq!(w.frag, " WHERE `name` LIKE ? ESCAPE '!'");
        assert_eq!(w.params, vec![json!("%al%")]);
        // The export statement (chunked page, no ORDER) carries the same WHERE.
        let stmt = browse_rows_sql(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &["`id`".to_string(), "`name`".to_string()],
            None,
            0,
            5000,
            &w.frag,
            &w.params,
        )
        .unwrap();
        assert_eq!(
            stmt.sql,
            "SELECT `id`, `name` FROM `app`.`users` WHERE `name` LIKE ? ESCAPE '!' LIMIT 5000 OFFSET 0"
        );
        assert_eq!(stmt.params, vec![json!("%al%")]);
        let count =
            browse_count_sql(DbDialect::Mysql, Some("app"), "users", &w.frag, &w.params).unwrap();
        assert_eq!(
            count.sql,
            "SELECT COUNT(*) AS total FROM `app`.`users` WHERE `name` LIKE ? ESCAPE '!'"
        );
        // No filters is still no WHERE at all — an unfiltered export is the whole table.
        let bare = browse_where(DbDialect::Mysql, &columns, None).unwrap();
        assert_eq!(bare.frag, "");
        assert!(bare.params.is_empty());
    }

    #[test]
    fn contains_on_typed_columns_casts_on_pg() {
        let cols = vec![
            FilterColumn::Column(col("name", "character varying", true, false)),
            FilterColumn::Column(col("id", "bigint", false, true)),
            FilterColumn::Column(col("created", "timestamp without time zone", true, false)),
        ];
        let pg = build_filter_where(
            DbDialect::Pg,
            &cols,
            &filter(json!([{ "column": "id", "op": "like", "value": "45" }])),
        )
        .unwrap();
        assert!(pg.frag.contains("CAST(\"id\" AS text) ILIKE"));
        let ts = build_filter_where(
            DbDialect::Pg,
            &cols,
            &filter(json!([{ "column": "created", "op": "like", "value": "2024" }])),
        )
        .unwrap();
        assert!(ts.frag.contains("CAST(\"created\" AS text) ILIKE"));
        // Text columns stay bare on Postgres, everything stays bare on MySQL.
        let bare = build_filter_where(
            DbDialect::Pg,
            &cols,
            &filter(json!([{ "column": "name", "op": "like", "value": "al" }])),
        )
        .unwrap();
        assert!(bare.frag.contains("\"name\" ILIKE"));
        let my = build_filter_where(
            DbDialect::Mysql,
            &cols,
            &filter(json!([{ "column": "id", "op": "like", "value": "45" }])),
        )
        .unwrap();
        assert!(my.frag.contains("`id` LIKE"));
        assert!(!my.frag.contains("CAST"));
        // Bare column names carry no type info, so no cast is ever built.
        let bare = build_filter_where(
            DbDialect::Pg,
            &names(&["name", "id"]),
            &filter(json!([{ "column": "id", "op": "like", "value": "4" }])),
        )
        .unwrap();
        assert!(bare.frag.contains("\"id\" ILIKE"));
    }

    #[test]
    fn numeric_bind_value_is_lossless_only() {
        assert_eq!(numeric_bind_value("42"), json!(42));
        assert_eq!(numeric_bind_value("-7"), json!(-7));
        // 734023681584275456 does not fit a JS double: Number() would round it to …500.
        assert_eq!(
            numeric_bind_value("734023681584275456"),
            json!("734023681584275456")
        );
        assert_eq!(numeric_bind_value("3.14"), json!(3.14));
        // String(1.1) !== "1.10" — the trailing zero matters.
        assert_eq!(numeric_bind_value("1.10"), json!("1.10"));
        assert_eq!(
            numeric_bind_value("0.100000000000000009"),
            json!("0.100000000000000009")
        );
        // Non-numeric text passes through untouched.
        assert_eq!(numeric_bind_value("abc"), json!("abc"));
        assert_eq!(numeric_bind_value(""), json!(""));
        assert_eq!(numeric_bind_value("12px"), json!("12px"));
    }
}
