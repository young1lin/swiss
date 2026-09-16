/*
 * Copyright 2026 The swiss authors
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

//! SQL guards shared by the mysql/pg adapters — port of `adapters/sql.ts`. Every matcher is a
//! hand-written scanner (ADR-007 keeps the regex engine out of the binary).

/// Word-boundary, case-insensitive contains: does `haystack` contain `word` as a whole ASCII word?
/// A word starts at a non-alphanumeric-underscore boundary on each side — the `\b` of the regexes
/// this replaces.
fn has_word(haystack: &str, word: &str) -> bool {
    let h = haystack.as_bytes();
    let w = word.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + w.len() <= h.len() {
        if i + w.len() <= h.len() && h[i..i + w.len()].eq_ignore_ascii_case(w) {
            let before_ok = i == 0 || !is_word(h[i - 1]);
            let after = i + w.len();
            let after_ok = after >= h.len() || !is_word(h[after]);
            if before_ok && after_ok {
                return true;
            }
        }
        // Advance one byte; ASCII-vs-UTF8 safety: word chars are matched case-insensitively only
        // against ASCII, and multi-byte sequences never partially match a word.
        i += 1;
    }
    false
}

fn has_any_word(haystack: &str, words: &[&str]) -> bool {
    words.iter().any(|w| has_word(haystack, w))
}

/// The first run of ASCII letters, lowercased — `body.match(/^[a-z]+/i)`.
fn first_word(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Reject a statement that smuggles a second one past the driver, returning the single trimmed
/// statement. mysql2 parity: the Node build never sent two (multipleStatements stays false
/// there), while sqlx negotiates MULTI_STATEMENTS unconditionally — so this port draws the same
/// line itself. Decided on MASKED text (a `;` inside a literal or comment is not a separator),
/// and one trailing terminator is still one statement.
pub fn assert_single_statement(sql: &str) -> Result<String, String> {
    let trimmed = strip_trailing_semicolon(trim_trailing(sql))
        .trim()
        .to_string();
    if mask_literals(&trimmed).contains(';') {
        return Err("one statement per run — send each further statement on its own.".into());
    }
    Ok(trimmed)
}

/// Blank out string literals, quoted identifiers and comments (length-preserving) — the shared
/// scanner every statement-shape decision runs on. Public for the pg adapter, which labels the
/// statements of a multi-statement call by splitting on the masked separators.
pub fn mask_statement(sql: &str) -> String {
    mask_literals(sql)
}

fn trim_trailing(s: &str) -> &str {
    s.trim_end()
}

/// `.replace(/;\s*$/, "")` — one trailing semicolon plus whatever whitespace follows it.
fn strip_trailing_semicolon(s: &str) -> &str {
    let mut end = s.len();
    let bytes = s.as_bytes();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b';' {
        end -= 1;
    }
    &s[..end]
}

/// Drop the columns that are NULL in each row — port of `dropNullColumns`.
///
/// A `SELECT *` on a wide table spends most of its reply saying nothing: one row of a 65-column
/// table is ~2 KB, and a third of the columns can be `"col": null`. An absent key carries the
/// same information for free. Both query tools state this in their description, and
/// `pg_describe_table` / `DESCRIBE` remain the way to see the full column list.
pub fn drop_null_columns(rows: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    rows.into_iter()
        .map(|row| match row {
            serde_json::Value::Object(map) => {
                serde_json::Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).collect())
            }
            other => other,
        })
        .collect()
}

// --- table-name filtering -----------------------------------------------------------------------

// `like_contains` lives in swiss_host::dbbrowser (the browser model vets the same filters the
// adapters do); re-exported so this module keeps being its SQL home for callers.
pub use swiss_host::dbbrowser::like_contains;

/// LIKE prefix for a table name: `_` and `%` escaped (table names are full of underscores).
pub fn like_prefix(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '!' | '%' | '_') {
            out.push('!');
        }
        out.push(c);
    }
    out
}

// --- table-list paging --------------------------------------------------------------------------

/// Tables per page for pg_list_tables / mysql_list_tables when the caller passes no limit.
pub const DEFAULT_TABLE_LIMIT: i64 = 200;
/// Ceiling on a requested page, so one listing cannot flood a context by asking for a million.
pub const MAX_TABLE_LIMIT: i64 = 1000;

/// A clamped page request: 0-based `page`, the clamped `limit`, and the SQL `offset`.
pub struct TablePage {
    pub page: i64,
    pub limit: i64,
    pub offset: i64,
}

/// Clamp a list_tables call's `limit`/`page` arguments: limit defaults to DEFAULT_TABLE_LIMIT
/// and never exceeds MAX_TABLE_LIMIT; page is 0-based and never negative (a nonsense page reads
/// as 0, which pages from the start rather than erroring mid-conversation).
pub fn table_page_args(
    limit: Option<&serde_json::Value>,
    page: Option<&serde_json::Value>,
) -> TablePage {
    let raw_limit = limit.and_then(as_i64).unwrap_or(0);
    let limit = if raw_limit > 0 {
        raw_limit.min(MAX_TABLE_LIMIT)
    } else {
        DEFAULT_TABLE_LIMIT
    };
    let raw_page = page.and_then(as_i64).unwrap_or(0);
    let page = if raw_page > 0 { raw_page } else { 0 };
    TablePage {
        page,
        limit,
        offset: page * limit,
    }
}

/// JSON number → i64 the way Math.floor(Number(x)) behaved: only true integers survive.
pub fn as_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

// --- automatic row limits -----------------------------------------------------------------------

/// Default rows returned when a SELECT arrives without a LIMIT of its own.
pub const DEFAULT_ROW_LIMIT: i64 = 200;
/// Ceiling on an explicitly requested limit, so the output budget stays meaningful.
pub const MAX_ROW_LIMIT: i64 = 10_000;

pub fn clamp_row_limit(requested: Option<&serde_json::Value>, fallback: i64) -> i64 {
    let n = requested.and_then(as_i64).unwrap_or(0);
    if n <= 0 {
        return fallback;
    }
    n.min(MAX_ROW_LIMIT)
}

/// Blank out string literals, quoted identifiers and comments, preserving length so offsets
/// into the original stay valid. Keyword scanning must not be fooled by
/// `WHERE note = 'limit 5'`.
///
/// STANDARD-SQL masking only: the sole in-literal escape is the doubled quote. There is
/// deliberately NO backslash-escape branch. MySQL treats \' as an escaped quote, but Postgres
/// (standard_conforming_strings=on, the default for 15 years) treats the backslash as an
/// ordinary character — and under MySQL semantics, `SELECT 'a\'; DROP TABLE t; --'` masks the
/// entire tail as one string literal, hiding a real second statement. Masking by the stricter
/// dialect can only ever OVER-reject a MySQL query; the opposite choice executes hidden writes
/// on Postgres.
fn mask_literals(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\'' || c == '"' || c == '`' {
            // Doubled quote = escaped quote; everything else blanks, preserving length.
            out.push(' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] as char == c {
                    if i + 1 < bytes.len() && bytes[i + 1] as char == c {
                        out.push_str("  ");
                        i += 2;
                        continue;
                    }
                    out.push(' ');
                    i += 1;
                    break;
                }
                let ch_len = sql[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                out.push_str(&" ".repeat(ch_len));
                i += ch_len;
            }
            continue;
        }
        if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let end = sql[i + 2..]
                .find("*/")
                .map(|p| i + 2 + p + 2)
                .unwrap_or(sql.len());
            for _ in i..end {
                out.push(' ');
            }
            i = end;
            continue;
        }
        let ch_len = sql[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&sql[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Blank anything nested in parentheses, so only top-level clauses remain. Length is preserved.
fn blank_parens(masked: &str) -> String {
    let mut out = String::with_capacity(masked.len());
    let mut depth = 0usize;
    for ch in masked.chars() {
        if ch == '(' {
            depth += 1;
            out.push(' ');
        } else if ch == ')' {
            depth = depth.saturating_sub(1);
            out.push(' ');
        } else if depth > 0 {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Row-returning statement shapes that accept a trailing LIMIT in both MySQL and Postgres.
const LIMITABLE: [&str; 4] = ["select", "with", "table", "values"];

/// Statement shapes that answer a RESULT SET rather than an OK packet. mysql2 knew from the
/// protocol — a result set arrives headed by a column-count packet, an OK packet does not — but
/// sqlx swallows the column metadata and ends every statement with the same `Either::Left`, so
/// the zero-rows edge (`SELECT … WHERE false` vs `UPDATE … WHERE false`, both wire-identical
/// through the driver) is decided here on the statement's own shape. Only consulted when NO rows
/// arrived; a statement whose rows did arrive is a result set by observation.
///
/// Known blind spot (accepted): a zero-row result set the shape cannot see — `CALL p()` whose
/// first SELECT returns nothing answers the OK-packet shape, and `SELECT … INTO @var` answers
/// the rows shape, where mysql2 answered the opposite. Telling them apart needs the wire
/// protocol's column-count/EOS distinction, which sqlx does not surface.
pub fn is_row_returning(sql: &str) -> bool {
    let masked = mask_literals(sql);
    let top = blank_parens(&masked);
    let first = first_word(masked.trim());
    match first.as_str() {
        "select" | "table" | "values" | "show" | "describe" | "desc" | "explain" | "analyze" => {
            true
        }
        // A data-modifying CTE answers an OK packet no matter what the outer SELECT reads.
        "with" => {
            has_word(&top, "select")
                && !has_any_word(&masked, &["insert", "update", "delete", "merge"])
        }
        _ => false,
    }
}

pub struct LimitedSql {
    pub sql: String,
    /// Set when this function added a limit that the caller did not write.
    pub limit_applied: Option<i64>,
    /// Short explanation to hand back with the rows, so partial results are never mistaken for
    /// all of them.
    pub note: Option<String>,
}

/// The `limitApplied` / `note` fields to return alongside a row set, if any — port of
/// `limitReport`. Only when the cap actually bit, and prose only when the caller did not choose
/// the limit (someone who passed `limit: 1` does not need the reply to explain their own
/// boundary back to them).
pub fn limit_report(
    prepared: &LimitedSql,
    row_count: i64,
    requested_limit: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(note) = &prepared.note {
        if prepared.limit_applied.is_none() {
            out.insert("note".into(), serde_json::json!(note));
            return serde_json::Value::Object(out);
        }
    }
    let Some(applied) = prepared.limit_applied else {
        return serde_json::Value::Object(out);
    };
    if row_count < applied {
        return serde_json::Value::Object(out);
    }
    out.insert("limitApplied".into(), serde_json::json!(applied));
    if requested_limit.is_none() {
        if let Some(note) = &prepared.note {
            out.insert("note".into(), serde_json::json!(note));
        }
    }
    serde_json::Value::Object(out)
}

/// Add `LIMIT n` to a row-returning statement that has none — port of `withRowLimit`.
///
/// The point is to stop the database from doing the work at all: `SELECT * FROM big_table` spent
/// 24 seconds building a result set before a server-side execution cap killed it in the Node
/// build. A statement that already limits itself, writes, or contains several statements is left
/// untouched.
pub fn with_row_limit(sql: &str, limit: i64) -> LimitedSql {
    let trimmed = strip_trailing_semicolon(trim_trailing(sql))
        .trim_end()
        .to_string();
    let masked = mask_literals(&trimmed);
    let top = blank_parens(&masked);

    let first = first_word(masked.trim());
    if !LIMITABLE.contains(&first.as_str()) {
        return LimitedSql {
            sql: trimmed,
            limit_applied: None,
            note: None,
        };
    }
    if first == "with"
        && (!has_word(&top, "select")
            || has_any_word(&masked, &["insert", "update", "delete", "merge"]))
    {
        // A data-modifying CTE writes every row no matter what the outer LIMIT says.
        return LimitedSql {
            sql: trimmed,
            limit_applied: None,
            note: None,
        };
    }
    if top.contains(';') {
        return LimitedSql {
            sql: trimmed,
            limit_applied: None,
            note: Some(
                "several statements were sent, so no row limit was added — add LIMIT yourself."
                    .into(),
            ),
        };
    }
    if has_word(&top, "limit") || has_two_words(&top, "fetch", &["first", "next"]) {
        return LimitedSql {
            sql: trimmed,
            limit_applied: None,
            note: None,
        };
    }
    if into_file(&top) {
        return LimitedSql {
            sql: trimmed,
            limit_applied: None,
            note: None,
        };
    }

    let clause = format!("LIMIT {limit}");
    // A locking clause must stay last: `... LIMIT n FOR UPDATE`, never `... FOR UPDATE LIMIT n`.
    match lock_tail_index(&top) {
        Some(idx) => {
            let head = trim_trailing(&trimmed[..idx]).to_string();
            LimitedSql {
                sql: format!("{head} {clause} {}", &trimmed[idx..]),
                limit_applied: Some(limit),
                note: Some(format!(
                    "no LIMIT in the statement, so {clause} was applied — there may be more rows."
                )),
            }
        }
        None => LimitedSql {
            sql: format!("{trimmed} {clause}"),
            limit_applied: Some(limit),
            note: Some(format!(
                "no LIMIT in the statement, so {clause} was applied — there may be more rows."
            )),
        },
    }
}

/// `fetch first|next` — the standard cousin of LIMIT.
fn has_two_words(top: &str, first: &str, seconds: &[&str]) -> bool {
    has_word(top, first) && seconds.iter().any(|s| has_word(top, s))
}

/// `INTO OUTFILE|DUMPFILE` (top level, masked) — the TWO-WORD phrase, Node's
/// `/\binto\s+(outfile|dumpfile)\b/`: a column literally named `outfile` (`SELECT outfile FROM
/// t`) must not suppress the auto-LIMIT.
fn into_file(top: &str) -> bool {
    phrase_index(top, &["into", "outfile"]).is_some()
        || phrase_index(top, &["into", "dumpfile"]).is_some()
}

/// Byte index in `top` where a trailing locking clause begins (the START of `FOR UPDATE`,
/// `FOR SHARE` or `LOCK IN SHARE MODE`), mirroring the regex alternation's `exec` index. The
/// clause must keep its trailing position after a LIMIT is inserted, so the insertion point is
/// the phrase head, not the keyword.
fn lock_tail_index(top: &str) -> Option<usize> {
    const PHRASES: [&[&str]; 5] = [
        &["lock", "in", "share", "mode"],
        &["for", "no", "key", "update"],
        &["for", "key", "share"],
        &["for", "update"],
        &["for", "share"],
    ];
    let mut best: Option<usize> = None;
    for phrase in PHRASES {
        if let Some(idx) = phrase_index(top, phrase) {
            best = Some(best.map_or(idx, |b: usize| b.min(idx)));
        }
    }
    best
}

/// Byte index of the first whole-word occurrence of a consecutive word sequence (words may be
/// separated by any whitespace), or None.
fn phrase_index(haystack: &str, words: &[&str]) -> Option<usize> {
    let first = words.first()?;
    let mut search_from = 0usize;
    while let Some(start) = word_index_from(haystack, first, search_from) {
        let mut cursor = start + first.len();
        let mut matched = true;
        for word in &words[1..] {
            // skip whitespace between words
            let rest = &haystack[cursor..];
            let skipped = rest.len() - rest.trim_start().len();
            if skipped == 0 {
                matched = false;
                break;
            }
            cursor += skipped;
            if let Some(idx) = word_index_from(haystack, word, cursor) {
                if idx != cursor {
                    matched = false;
                    break;
                }
                cursor += word.len();
            } else {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(start);
        }
        search_from = start + 1;
    }
    None
}

/// `word_index` starting the scan at `from`.
fn word_index_from(haystack: &str, word: &str, from: usize) -> Option<usize> {
    let h = haystack.as_bytes();
    let w = word.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = from;
    while i + w.len() <= h.len() {
        if h[i..i + w.len()].eq_ignore_ascii_case(w) {
            let before_ok = i == 0 || !is_word(h[i - 1]);
            let after = i + w.len();
            let after_ok = after >= h.len() || !is_word(h[after]);
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    // Ported from the guards the Node build's sql.test.ts pinned.
    use super::*;

    #[test]
    fn any_single_statement_passes_reads_and_writes_alike() {
        for sql in [
            "SELECT 1",
            "select * from t where note = 'delete me'",
            "  ((select 1) union (select 2))  ",
            "UPDATE t SET x = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "SELECT * INTO newtable FROM t",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "-- select\nUPDATE t SET x = 1",
        ] {
            assert!(assert_single_statement(sql).is_ok(), "{sql}");
        }
    }

    #[test]
    fn stacked_statements_fail() {
        for sql in [
            "SELECT 1; DROP TABLE t",
            "UPDATE t SET x = 1; SELECT 1",
            "select 'a'; drop table t'",
        ] {
            assert!(assert_single_statement(sql).is_err(), "{sql}");
        }
        // One trailing terminator is one statement.
        assert!(assert_single_statement("select 1;").is_ok());
        // A semicolon inside a literal or a comment is not a separator.
        assert!(assert_single_statement("SELECT * FROM t WHERE note = 'a;b'").is_ok());
        assert!(assert_single_statement("SELECT 1 -- ; DROP TABLE t\n").is_ok());
    }

    #[test]
    fn row_limits() {
        // Plain read gets a limit appended.
        let out = with_row_limit("SELECT * FROM big", 200);
        assert_eq!(out.sql, "SELECT * FROM big LIMIT 200");
        assert_eq!(out.limit_applied, Some(200));

        // Already limited: untouched.
        let out = with_row_limit("SELECT * FROM t LIMIT 5", 200);
        assert_eq!(out.sql, "SELECT * FROM t LIMIT 5");
        assert!(out.limit_applied.is_none());

        // Writes untouched; trailing semicolon dropped.
        let out = with_row_limit("UPDATE t SET x = 1;", 200);
        assert_eq!(out.sql, "UPDATE t SET x = 1");
        assert!(out.limit_applied.is_none());

        // Locking clause stays last.
        let out = with_row_limit("SELECT * FROM t FOR UPDATE", 200);
        assert_eq!(out.sql, "SELECT * FROM t LIMIT 200 FOR UPDATE");

        // Multi-statement: warned, not limited.
        let out = with_row_limit("SELECT 1; SELECT 2", 200);
        assert!(out.note.is_some());
        assert!(out.limit_applied.is_none());
    }

    #[test]
    fn like_patterns() {
        assert_eq!(like_contains("users"), "%users%");
        assert_eq!(like_contains("a_b%c"), "%a!_b!%c%");
        assert_eq!(like_prefix("events_2026"), "events!_2026");
    }

    #[test]
    fn row_returning_shapes() {
        // Result-set answers.
        for sql in [
            "SELECT 1",
            "select * from t where x = 0",
            "SHOW TABLES",
            "DESCRIBE t",
            "desc t",
            "EXPLAIN SELECT 1",
            "ANALYZE TABLE t",
            "TABLE t",
            "VALUES ROW(1)",
            "WITH c AS (SELECT 1) SELECT * FROM c",
        ] {
            assert!(is_row_returning(sql), "expected row-returning: {sql}");
        }
        // OK-packet answers.
        for sql in [
            "UPDATE t SET x = 1 WHERE 0",
            "INSERT INTO t VALUES (1)",
            "DELETE FROM t",
            "SET @x = 1",
            "CALL p()",
            // A data-modifying CTE writes rows; the outer SELECT does not make it a result set.
            "WITH c AS (INSERT INTO t SELECT 1 RETURNING *) SELECT * FROM c",
        ] {
            assert!(!is_row_returning(sql), "expected OK-packet: {sql}");
        }
    }
}
