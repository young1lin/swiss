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
/// `pg_describe_table` / `mysql_describe_table` remain the way to see the full column list.
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
/// The Data console's safety net (SPEC §data.console): a LIMIT-less SELECT run from the panel
/// reads at most this many rows unless the request names its own cap (the panel's row-limit
/// setting, clamped to [MAX_ROW_LIMIT]). A LIMIT the statement writes itself always wins.
pub const CONSOLE_ROW_LIMIT: i64 = 500;

pub fn clamp_row_limit(requested: Option<&serde_json::Value>, fallback: i64) -> i64 {
    let n = requested.and_then(as_i64).unwrap_or(0);
    if n <= 0 {
        return fallback;
    }
    n.min(MAX_ROW_LIMIT)
}

// --- tool argument schemas ----------------------------------------------------------------------

/// The query tool's `limit`. `default` is the instance's own cap (its `maxRows`), so the schema
/// states the number a LIMIT-less SELECT will actually stop at.
pub fn row_limit_arg(default: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_ROW_LIMIT,
        "description": format!("Row cap for a LIMIT-less SELECT (default {default}, max {MAX_ROW_LIMIT})."),
    })
}

/// The table listings' `grep`, `limit` and `page`, shared by both engines.
pub fn grep_arg() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": concat!(
            "Keep only tables whose name contains this substring (case-insensitive): \"users\" lists p_users, ",
            "users_settings, … Optional — omit to list everything.",
        ),
    })
}

pub fn table_limit_arg() -> serde_json::Value {
    serde_json::json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_TABLE_LIMIT,
        "description": format!("Max tables per page (default {DEFAULT_TABLE_LIMIT}, max {MAX_TABLE_LIMIT})."),
    })
}

pub fn page_arg() -> serde_json::Value {
    serde_json::json!({
        "type": "integer",
        "minimum": 0,
        "description": "0-based page index through the filtered list (default 0) — the reply's total/more say what is left.",
    })
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

/// Blank anything nested in parentheses, so only top-level clauses remain. Length is preserved
/// in bytes, so an index found here is an index into the statement too.
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
            // A multi-byte name nested in parentheses blanks to as many bytes as it took.
            out.push_str(&" ".repeat(ch.len_utf8()));
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

/// The Data agent's read gate (SPEC §data.agent): ONE statement whose first word reads -
/// SELECT, WITH without a data-modifying CTE, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN - and
/// no SELECT ... INTO OUTFILE or DUMPFILE, a server file no transaction undoes. The read-only
/// transaction the browsers run it in is the second gate; this one also stops MySQL DDL,
/// which commits implicitly before a transaction's mode could refuse it.
pub fn assert_read_statement(sql: &str) -> Result<String, String> {
    let sql = assert_single_statement(sql)?;
    let masked = mask_literals(&sql);
    // A parenthesised read ((SELECT ...) UNION (SELECT ...)) opens with its first word.
    let first = first_word(masked.trim_start_matches(|c: char| c == '(' || c.is_whitespace()));
    let reads = match first.as_str() {
        "select" | "table" | "values" | "show" | "describe" | "desc" | "explain" => true,
        "with" => !has_any_word(&masked, &["insert", "update", "delete", "merge"]),
        _ => false,
    };
    if !reads {
        return Err(format!(
            "only a read runs here (SELECT, WITH, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN), not {}",
            if first.is_empty() { "an empty statement".to_string() } else { first.to_uppercase() }
        ));
    }
    if has_any_word(&masked, &["outfile", "dumpfile"]) {
        return Err("only a read runs here: INTO OUTFILE and INTO DUMPFILE write a server file".into());
    }
    Ok(sql)
}

/// The console's reply to a row set: columns, rows, rowCount and, when the cap bit, the
/// limit report. `columns` derives from the first row (a driver limitation, accepted: a
/// zero-row SELECT answers columns: []).
pub fn console_reply(
    rows: Vec<serde_json::Map<String, serde_json::Value>>,
    prepared: &LimitedSql,
    limit: Option<&serde_json::Value>,
) -> serde_json::Value {
    let columns = rows.first().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    let row_count = rows.len();
    let mut out = serde_json::Map::new();
    out.insert("columns".into(), serde_json::json!(columns));
    out.insert("rows".into(), serde_json::Value::Array(rows.into_iter().map(serde_json::Value::Object).collect()));
    out.insert("rowCount".into(), serde_json::json!(row_count));
    if let serde_json::Value::Object(extra) = limit_report(prepared, row_count as i64, limit) {
        out.extend(extra);
    }
    serde_json::Value::Object(out)
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

// --- the Data agent's writes --------------------------------------------------------------------

/// The server's own schemas: a write of the agent's never touches one on its own.
const SYSTEM_SCHEMAS: [&str; 6] = ["mysql", "sys", "information_schema", "performance_schema", "pg_catalog", "pg_toast"];

/// The words of a condition that name no column: its connectives and its constants.
const CONDITION_WORDS: [&str; 13] = [
    "and", "or", "xor", "not", "is", "null", "true", "false", "unknown", "in", "like", "ilike", "between",
];

/// The Data agent's auto-write gate (SPEC §data.agent Writes): ONE INSERT, REPLACE, UPDATE or
/// DELETE - an UPDATE or DELETE only with a top-level WHERE whose every OR branch names a
/// column - naming no system schema. Everything else is the user's to run: DDL (MySQL commits
/// it implicitly, so no rollback undoes it), a data-modifying CTE, CALL, COPY, MERGE, LOCK, SET
/// and the rest. The Err says why, for the agent to pass on beside the staged statement. How
/// many rows a WHERE matches is the runner's cap to decide: this gate cannot know.
pub fn assert_auto_write(sql: &str) -> Result<String, String> {
    let sql = assert_single_statement(sql)?;
    let masked = mask_literals(&sql);
    let first = first_word(masked.trim_start());
    match first.as_str() {
        "insert" | "replace" => {}
        "update" | "delete" => where_narrows(&sql, &blank_parens(&masked), &first)?,
        "" => return Err("an empty statement".into()),
        other => {
            return Err(format!(
                "only an INSERT, REPLACE, UPDATE or DELETE runs on its own, not {}",
                other.to_uppercase()
            ))
        }
    }
    // Quotes dropped, so `mysql`.user and "pg_catalog".x read as the names they are; a literal
    // or a comment spelling one only holds a statement that could have run.
    let bare: String = sql.chars().filter(|c| *c != '"' && *c != '`').collect();
    if let Some(schema) = SYSTEM_SCHEMAS.iter().find(|schema| names_schema(&bare, schema)) {
        return Err(format!("it writes to the server's own schema {schema}"));
    }
    Ok(sql)
}

/// Does `text` qualify a name by `schema` (`schema.table`, spaces allowed before the dot)?
fn names_schema(text: &str, schema: &str) -> bool {
    let mut from = 0;
    while let Some(at) = word_index_from(text, schema, from) {
        if text[at + schema.len()..].trim_start().starts_with('.') {
            return true;
        }
        from = at + 1;
    }
    false
}

/// An UPDATE's or DELETE's WHERE at the top level, every OR branch of it naming a column: none
/// at all, or `1=1`, `true`, `'a'='a'` - alone or OR'd beside a real test - changes every row.
/// `top` is the statement masked and paren-blanked, byte for byte the statement's length.
fn where_narrows(sql: &str, top: &str, verb: &str) -> Result<(), String> {
    let verb = verb.to_uppercase();
    let Some(at) = word_index_from(top, "where", 0) else {
        return Err(format!("{verb} without a WHERE changes every row"));
    };
    let from = at + "where".len();
    // The condition runs to the clause after it: RETURNING (PostgreSQL), ORDER BY or LIMIT (MySQL).
    let end = ["returning", "order", "limit"]
        .iter()
        .filter_map(|word| word_index_from(top, word, from))
        .min()
        .unwrap_or(top.len());
    // The OR branches: OR, and MySQL's || (PostgreSQL's concatenation splits too, holding at worst).
    let mut cuts = vec![(from, from)];
    let mut i = from;
    while let Some(or) = word_index_from(top, "or", i).filter(|or| *or < end) {
        cuts.push((or, or + 2));
        i = or + 2;
    }
    let mut j = from;
    while let Some(pipes) = top[j..end].find("||").map(|p| j + p) {
        cuts.push((pipes, pipes + 2));
        j = pipes + 2;
    }
    cuts.sort_unstable();
    cuts.push((end, end));
    for pair in cuts.windows(2) {
        let branch = &sql[pair[0].1..pair[1].0];
        if !names_a_column(branch) {
            let shown = branch.split_whitespace().collect::<Vec<_>>().join(" ");
            return Err(if pair[0].1 == from && pair[1].0 == end {
                format!("{verb} WHERE {shown} names no column, so it changes every row")
            } else {
                format!("{verb} WHERE ... OR {shown} names no column, so it changes every row")
            });
        }
    }
    Ok(())
}

/// Does a condition name a column - an unquoted name that is not a connective, a constant or
/// a cast's type, or a quoted one? Literals, numbers and comments name none.
fn names_a_column(cond: &str) -> bool {
    let b = cond.as_bytes();
    let named = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == b'"' || c == b'`' {
            return true;
        }
        // -- and # (MySQL) run to the line's end; read as comments on PostgreSQL too, where #
        // is an operator, they can only hide a column, never invent one.
        if (c == b'-' && b.get(i + 1) == Some(&b'-')) || c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = cond[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
            continue;
        }
        if c.is_ascii_digit() {
            while i < b.len() && (named(b[i]) || b[i] == b'.') {
                i += 1;
            }
            continue;
        }
        if named(c) {
            let start = i;
            while i < b.len() && named(b[i]) {
                i += 1;
            }
            let word = cond[start..i].to_ascii_lowercase();
            // A cast's type (PostgreSQL's '1'::int) is no column either.
            let cast = cond[..start].trim_end().ends_with("::");
            if !cast && !CONDITION_WORDS.contains(&word.as_str()) {
                return true;
            }
            continue;
        }
        i += 1;
    }
    false
}

/// A single-table UPDATE or DELETE with no LIMIT of its own, bounded to `rows` (MySQL, SPEC
/// §data.agent Writes): the server stops at that many matched rows, so a WHERE matching
/// millions neither holds its locks for minutes nor leaves minutes of undo before the cap
/// rolls it back. A multi-table one takes no LIMIT and is left as it is, as is every other
/// statement.
pub fn bounded_write(sql: &str, rows: u64) -> String {
    let masked = mask_literals(sql);
    let top = blank_parens(&masked);
    let single = match first_word(masked.trim_start()).as_str() {
        "update" => {
            let tables = &top[..word_index_from(&top, "set", 0).unwrap_or(top.len())];
            !tables.contains(',') && !has_word(tables, "join")
        }
        // DELETE [LOW_PRIORITY] [QUICK] [IGNORE] FROM t WHERE ...: no table list before FROM,
        // none after it, no USING.
        "delete" => match word_index_from(&top, "from", 0) {
            Some(at) => {
                let tables = &top[at..word_index_from(&top, "where", at).unwrap_or(top.len())];
                top[..at]
                    .split_whitespace()
                    .skip(1)
                    .all(|word| ["low_priority", "quick", "ignore"].contains(&word.to_ascii_lowercase().as_str()))
                    && !tables.contains(',')
                    && !has_any_word(&top, &["join", "using"])
            }
            None => false,
        },
        _ => false,
    };
    if !single || has_word(&top, "limit") {
        return sql.to_string();
    }
    // On a line of its own: a trailing -- or # comment would swallow it otherwise.
    format!("{sql}\nLIMIT {rows}")
}

/// Does a write answer rows - a top-level RETURNING (PostgreSQL), one row for each it changed?
pub fn has_returning(sql: &str) -> bool {
    has_word(&blank_parens(&mask_literals(sql)), "returning")
}

/// What a capped write of the agent's came to, as a dialect's runner reports it.
pub enum CappedWrite {
    /// Committed: the rows it changed, the rows it answered (PostgreSQL's RETURNING) and the
    /// id an insert took (MySQL's LAST_INSERT_ID, when it took one).
    Kept { changed: u64, rows: Vec<serde_json::Map<String, serde_json::Value>>, insert_id: Option<u64> },
    /// Rolled back: it changed more rows than the cap, `changed` as counted - cap + 1 when
    /// the statement was bounded there.
    Past { changed: u64 },
}

/// A capped write's outcome as run_agent_write answers it: a kept one in the console's reply
/// shape with affectedRows (and insertId), one past the cap held for the user, saying why.
pub fn capped_reply(outcome: CappedWrite, cap: u64) -> Result<serde_json::Value, swiss_host::dbbrowser::WriteRefusal> {
    match outcome {
        CappedWrite::Kept { changed, rows, insert_id } => {
            let plain = LimitedSql { sql: String::new(), limit_applied: None, note: None };
            let mut reply = console_reply(rows, &plain, None);
            reply["affectedRows"] = serde_json::json!(changed);
            if let Some(id) = insert_id {
                reply["insertId"] = serde_json::json!(id);
            }
            Ok(reply)
        }
        CappedWrite::Past { changed } => {
            let counted = if changed > cap + 1 { changed.to_string() } else { format!("more than {cap}") };
            Err(swiss_host::dbbrowser::WriteRefusal::Held(format!(
                "it changes {counted} rows and a write changes at most {cap} on its own, so it was rolled back"
            )))
        }
    }
}

// --- diagnostics --------------------------------------------------------------------------------

/// Rows one pg_inspect / mysql_inspect check returns when the caller passes no limit.
pub const DEFAULT_INSPECT_LIMIT: i64 = 20;
/// Ceiling on a check's rows: a diagnosis reads the worst offenders, not the whole catalog.
pub const MAX_INSPECT_LIMIT: i64 = 100;

/// One built-in check of pg_inspect / mysql_inspect, in the engine's own dialect. The row
/// query's last placeholder is the row limit.
#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    /// What it reports — one clause of the tool description.
    pub about: &'static str,
    pub rows: Option<&'static str>,
    /// A one-row query whose columns become the reply's `summary`.
    pub summary: Option<&'static str>,
    /// What the numbers do not say on their own, carried in the reply.
    pub note: Option<&'static str>,
}

/// The inspect tool's description: `intro`, every check as `name — about`, then `outro`.
pub fn inspect_description(intro: &str, checks: &[Check], outro: &str) -> String {
    let list: Vec<String> = checks
        .iter()
        .map(|c| format!("{} — {}", c.name, c.about))
        .collect();
    format!("{intro} Checks: {}. {outro}", list.join("; "))
}

pub fn inspect_schema(checks: &[Check]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "check": {
                "type": "string",
                "enum": checks.iter().map(|c| c.name).collect::<Vec<_>>(),
                "description": "Which diagnostic to run (the tool description says what each reports).",
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_INSPECT_LIMIT,
                "description": format!(
                    "Max rows (default {DEFAULT_INSPECT_LIMIT}, max {MAX_INSPECT_LIMIT})."
                ),
            },
        },
        "required": ["check"],
        "additionalProperties": false,
    })
}

/// The check a call names, and its clamped row limit. The enum is checked here: the tool layer
/// refuses unknown argument names, not values outside an enum.
pub fn inspect_args<'a>(
    args: &serde_json::Value,
    checks: &'a [Check],
) -> Result<(&'a Check, i64), String> {
    let name = args
        .get("check")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let names = || checks.iter().map(|c| c.name).collect::<Vec<_>>().join(", ");
    if name.is_empty() {
        return Err(format!("check is required — one of {}", names()));
    }
    let check = checks
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown check \"{name}\" — one of {}", names()))?;
    let limit = match args.get("limit").and_then(as_i64) {
        Some(n) if n > 0 => n.min(MAX_INSPECT_LIMIT),
        _ => DEFAULT_INSPECT_LIMIT,
    };
    Ok((check, limit))
}

/// `{ check, rows?, summary?, note? }`, with NULL fields dropped from each row and the summary.
pub fn inspect_reply(
    check: &Check,
    rows: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    summary: Option<serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Value {
    use serde_json::Value;
    let mut out = serde_json::Map::new();
    out.insert("check".into(), Value::from(check.name));
    if let Some(rows) = rows {
        let rows = drop_null_columns(rows.into_iter().map(Value::Object).collect());
        out.insert("rows".into(), Value::Array(rows));
    }
    if let Some(summary) = summary {
        let summary: serde_json::Map<_, _> =
            summary.into_iter().filter(|(_, v)| !v.is_null()).collect();
        if !summary.is_empty() {
            out.insert("summary".into(), Value::Object(summary));
        }
    }
    if let Some(note) = check.note {
        out.insert("note".into(), Value::from(note));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    // Ported from the guards the Node build's sql.test.ts pinned.
    use super::*;

    #[test]
    fn the_read_gate_passes_reads_and_refuses_every_write() {
        for sql in [
            "SELECT 1;",
            "select * from posts where type = 'delete me'",
            "WITH t AS (SELECT 1) SELECT * FROM t",
            "  (select 1) union (select 2)",
            "SHOW TABLES",
            "EXPLAIN SELECT 1",
            "DESC posts",
            "TABLE posts",
            "-- update\nSELECT 1",
            "SELECT `outfile` FROM t",
        ] {
            assert!(assert_read_statement(sql).is_ok(), "{sql}");
        }
        for sql in [
            "UPDATE t SET x = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "INSERT INTO t VALUES (1)",
            "TRUNCATE t",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "SELECT * FROM t INTO OUTFILE '/tmp/x'",
            "SELECT 1; DROP TABLE t",
            "",
        ] {
            assert!(assert_read_statement(sql).is_err(), "{sql}");
        }
        assert!(assert_read_statement("DROP TABLE t").unwrap_err().ends_with("not DROP"));
    }

    #[test]
    fn the_auto_write_gate_runs_narrow_writes_and_holds_the_rest() {
        for sql in [
            "INSERT INTO t (a) VALUES (1);",
            "insert into t select * from u where note = 'x'",
            "REPLACE INTO t VALUES (1)",
            "INSERT INTO t VALUES (1) ON DUPLICATE KEY UPDATE a = 2",
            "UPDATE t SET x = 1 WHERE id = 5",
            "update t set x = (select max(y) from u) where id in (select id from u where ok)",
            "UPDATE t SET note = 'where' WHERE \"Id\" = 5",
            "UPDATE `t` SET x = 1 WHERE `id` = 5 ORDER BY id LIMIT 1",
            "DELETE FROM t WHERE id = 5 OR id = 6",
            "DELETE FROM t WHERE created < now() - interval '1 day' RETURNING id",
            "DELETE FROM 订单 WHERE (编号 = 1)",
            "UPDATE t SET x = 1 WHERE 1 = 1 AND id = 5",
        ] {
            assert!(assert_auto_write(sql).is_ok(), "{sql}: {:?}", assert_auto_write(sql));
        }
        for (sql, why) in [
            ("DELETE FROM t", "DELETE without a WHERE"),
            ("update t set x = 1", "UPDATE without a WHERE"),
            ("UPDATE t SET x = (SELECT 1 FROM u WHERE u.id = 1)", "without a WHERE"),
            ("DELETE FROM t WHERE 1=1", "names no column"),
            ("DELETE FROM t WHERE true", "names no column"),
            ("DELETE FROM t WHERE '1' = '1'::text", "names no column"),
            ("DELETE FROM t WHERE (1)", "names no column"),
            ("DELETE FROM t WHERE id = 5 OR 1 = 1", "OR 1 = 1 names no column"),
            ("DELETE FROM t WHERE id = 5 || 2 > 1", "names no column"),
            ("DELETE FROM t WHERE 1 = 1 -- id = 5", "names no column"),
            ("DELETE FROM t WHERE 1 = 1 # id = 5", "names no column"),
            ("DROP TABLE t", "not DROP"),
            ("TRUNCATE t", "not TRUNCATE"),
            ("ALTER TABLE t ADD c int", "not ALTER"),
            ("WITH x AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM x", "not WITH"),
            ("CALL p()", "not CALL"),
            ("SET x = 1", "not SET"),
            ("SELECT 1", "not SELECT"),
            ("INSERT INTO mysql.user VALUES (1)", "schema mysql"),
            ("DELETE FROM `pg_catalog` . pg_class WHERE oid = 1", "schema pg_catalog"),
            ("INSERT INTO t VALUES (1); DELETE FROM t", "one statement per run"),
            ("", "an empty statement"),
        ] {
            let held = assert_auto_write(sql).unwrap_err();
            assert!(held.contains(why), "{sql}: {held}");
        }
    }

    #[test]
    fn a_single_table_write_is_bounded_and_a_multi_table_one_left_alone() {
        assert_eq!(bounded_write("DELETE FROM t WHERE id > 1", 1001), "DELETE FROM t WHERE id > 1\nLIMIT 1001");
        assert_eq!(bounded_write("delete low_priority quick from t where a = 1 -- old", 3), "delete low_priority quick from t where a = 1 -- old\nLIMIT 3");
        assert_eq!(bounded_write("UPDATE t SET a = 1 WHERE b = 2 ORDER BY id", 9), "UPDATE t SET a = 1 WHERE b = 2 ORDER BY id\nLIMIT 9");
        assert_eq!(bounded_write("UPDATE t SET a = (SELECT 1 FROM u, v LIMIT 1) WHERE b = 2", 9), "UPDATE t SET a = (SELECT 1 FROM u, v LIMIT 1) WHERE b = 2\nLIMIT 9");
        for sql in [
            "UPDATE t SET a = 1 WHERE b = 2 LIMIT 5",
            "UPDATE t JOIN u ON t.id = u.id SET t.a = 1 WHERE u.b = 2",
            "UPDATE t, u SET t.a = u.a WHERE t.id = u.id",
            "DELETE t FROM t JOIN u ON t.id = u.id WHERE u.b = 2",
            "DELETE FROM t USING t, u WHERE t.id = u.id",
            "DELETE FROM t, u WHERE t.id = 1",
            "INSERT INTO t SELECT * FROM u",
        ] {
            assert_eq!(bounded_write(sql, 9), sql);
        }
    }

    #[test]
    fn a_capped_write_answers_its_count_or_is_held_past_the_cap() {
        let mut row = serde_json::Map::new();
        row.insert("id".into(), serde_json::json!(7));
        let kept = capped_reply(CappedWrite::Kept { changed: 1, rows: vec![row], insert_id: Some(7) }, 1000).unwrap();
        assert_eq!(kept["affectedRows"], 1);
        assert_eq!(kept["insertId"], 7);
        assert_eq!(kept["rowCount"], 1);
        assert_eq!(kept["columns"], serde_json::json!(["id"]));
        let plain = capped_reply(CappedWrite::Kept { changed: 0, rows: Vec::new(), insert_id: None }, 1000).unwrap();
        assert_eq!(plain["affectedRows"], 0);
        assert!(plain.get("insertId").is_none());
        let held = |changed| match capped_reply(CappedWrite::Past { changed }, 1000) {
            Err(swiss_host::dbbrowser::WriteRefusal::Held(why)) => why,
            other => panic!("{:?}", other.map(|_| ())),
        };
        assert_eq!(held(1001), "it changes more than 1000 rows and a write changes at most 1000 on its own, so it was rolled back");
        assert!(held(25_000).starts_with("it changes 25000 rows"));
        assert!(has_returning("DELETE FROM t WHERE id = 1 RETURNING *"));
        assert!(!has_returning("DELETE FROM t WHERE note = 'returning' AND id IN (SELECT id FROM u)"));
    }

    #[test]
    fn blanked_parentheses_keep_the_statement_s_byte_length() {
        let sql = "SELECT (名字) FROM t";
        assert_eq!(blank_parens(&mask_literals(sql)).len(), sql.len());
    }

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

    const CHECKS: &[Check] = &[
        Check {
            name: "activity",
            about: "running statements",
            rows: Some("SELECT 1 LIMIT ?"),
            summary: None,
            note: None,
        },
        Check {
            name: "cache",
            about: "hit ratios",
            rows: None,
            summary: Some("SELECT 1"),
            note: Some("since start"),
        },
    ];

    #[test]
    fn inspect_args_name_a_known_check_and_clamp_the_limit() {
        let args = serde_json::json!({ "check": "cache" });
        let (check, limit) = inspect_args(&args, CHECKS).unwrap();
        assert_eq!((check.name, limit), ("cache", DEFAULT_INSPECT_LIMIT));
        let args = serde_json::json!({ "check": "activity", "limit": 5000 });
        assert_eq!(inspect_args(&args, CHECKS).unwrap().1, MAX_INSPECT_LIMIT);
        let args = serde_json::json!({ "check": "activity", "limit": 0 });
        assert_eq!(inspect_args(&args, CHECKS).unwrap().1, DEFAULT_INSPECT_LIMIT);
        // The tool layer does not enforce the enum; a typo must say what exists.
        let err = inspect_args(&serde_json::json!({ "check": "lock" }), CHECKS).unwrap_err();
        assert!(err.contains("activity, cache"), "{err}");
        let err = inspect_args(&serde_json::json!({}), CHECKS).unwrap_err();
        assert!(err.starts_with("check is required"), "{err}");
    }

    #[test]
    fn inspect_schema_and_description_list_every_check() {
        let schema = inspect_schema(CHECKS);
        assert_eq!(
            schema["properties"]["check"]["enum"],
            serde_json::json!(["activity", "cache"])
        );
        assert_eq!(schema["required"], serde_json::json!(["check"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["limit"]["maximum"], MAX_INSPECT_LIMIT);
        let text = inspect_description("Diagnose.", CHECKS, "Read-only.");
        assert_eq!(
            text,
            "Diagnose. Checks: activity — running statements; cache — hit ratios. Read-only."
        );
    }

    #[test]
    fn inspect_reply_drops_nulls_and_carries_the_note() {
        let row = serde_json::json!({ "pid": 7, "query": null });
        let summary = serde_json::json!({ "hit_pct": 99.5, "size": null });
        let reply = inspect_reply(
            &CHECKS[1],
            Some(vec![row.as_object().unwrap().clone()]),
            Some(summary.as_object().unwrap().clone()),
        );
        assert_eq!(
            reply,
            serde_json::json!({
                "check": "cache",
                "rows": [{ "pid": 7 }],
                "summary": { "hit_pct": 99.5 },
                "note": "since start",
            })
        );
        // No row query, an all-NULL summary and no note leave only the check's name.
        let empty = serde_json::json!({ "x": null });
        let reply = inspect_reply(&CHECKS[0], None, Some(empty.as_object().unwrap().clone()));
        assert_eq!(reply, serde_json::json!({ "check": "activity" }));
    }
}
