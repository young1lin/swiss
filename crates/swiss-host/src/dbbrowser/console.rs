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

//! The SQL console: the EXPLAIN prefix and server-side completion (SPEC §data.completion).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::DbDialect;

// --- the SQL console ----------------------------------------------------------------------------

/// Prefix a statement with EXPLAIN for the console's plan view — idempotent, so a query that
/// already explains itself is not double-prefixed. The statement keeps its single trailing
/// terminator stripped (EXPLAIN accepts one statement, not one plus a dangling ";").
///
/// Both dialects spell it the same way.
///
/// Not on any production path: the console's plan view gets its prefix from the panel twin
/// (panel/src/data-sql.ts, dbWithExplain); this copy anchors the semantics and the tests
/// (with_explain_prefixes_once_and_strips_the_terminator) — kept deliberately (SPEC §arch.exceptions).
pub fn with_explain(sql: &str) -> String {
    // .replace(/\s+$/, "").replace(/;\s*$/, "").replace(/\s+$/, "") — one trailing terminator,
    // then whitespace gone again.
    let s = sql.trim_end();
    let s = s.strip_suffix(';').map(str::trim_end).unwrap_or(s);
    // /^explain\b/i — "explain" then a word boundary (or the end of the statement).
    let b = s.as_bytes();
    if b.len() >= 7 && b[..7].eq_ignore_ascii_case(b"explain") {
        let boundary = b.len() == 7 || !(b[7].is_ascii_alphanumeric() || b[7] == b'_');
        if boundary {
            return s.to_string();
        }
    }
    format!("EXPLAIN {s}")
}

// --- SQL completion (SPEC §data.completion) ------------------------------------------------------------------

/// How long a completion cache entry stays fresh. Ten minutes: long enough that typing a
/// query does not re-walk the catalog per keystroke, short enough that a table created in
/// another tab shows up without a reconnect.
pub const COMPLETION_TTL: Duration = Duration::from_secs(600);
/// Ceiling on the cached column NAMES (bytes) per connection. A schema wide enough to blow
/// this is one nobody wants completed from memory anyway: the cache degrades to keywords +
/// table names instead of growing without bound.
pub const COMPLETION_BUDGET_BYTES: usize = 64 * 1024;

/// The keyword half of the completion candidates, ~150 per dialect: the statements and types
/// an operator actually types, uppercase (SQL folds case; the list carries one spelling per
/// word). Static on purpose — a keyword needs no round trip and never expires. Built once
/// per dialect into a sorted, deduped table; CORE carries the shared vocabulary.
pub fn sql_keywords(dialect: DbDialect) -> &'static [&'static str] {
    const CORE: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "AND",
        "OR",
        "NOT",
        "NULL",
        "IS",
        "IN",
        "LIKE",
        "BETWEEN",
        "AS",
        "ON",
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "OUTER",
        "CROSS",
        "UNION",
        "ALL",
        "ANY",
        "SOME",
        "EXISTS",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "CAST",
        "COALESCE",
        "NULLIF",
        "DISTINCT",
        "GROUP",
        "BY",
        "HAVING",
        "ORDER",
        "ASC",
        "DESC",
        "LIMIT",
        "OFFSET",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "CREATE",
        "DROP",
        "ALTER",
        "TABLE",
        "VIEW",
        "INDEX",
        "SEQUENCE",
        "TRIGGER",
        "FUNCTION",
        "PROCEDURE",
        "PRIMARY",
        "FOREIGN",
        "KEY",
        "REFERENCES",
        "CONSTRAINT",
        "UNIQUE",
        "DEFAULT",
        "CHECK",
        "CASCADE",
        "RESTRICT",
        "IF",
        "TEMP",
        "TEMPORARY",
        "WITH",
        "RETURNING",
        "TRUE",
        "FALSE",
        "UNKNOWN",
        "COMMENT",
        "GRANT",
        "REVOKE",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "TRANSACTION",
        "ISOLATION",
        "LEVEL",
        "READ",
        "WRITE",
        "ONLY",
        "EXPLAIN",
        "ANALYZE",
        "VERBOSE",
        "TRUNCATE",
        "RENAME",
        "ADD",
        "COLUMN",
        "USING",
        "INTERVAL",
        "DAY",
        "HOUR",
        "MINUTE",
        "SECOND",
        "YEAR",
        "MONTH",
        "QUARTER",
        "WEEK",
        "CURRENT_DATE",
        "CURRENT_TIME",
        "CURRENT_TIMESTAMP",
        "LOCALTIME",
        "LOCALTIMESTAMP",
        "EXTRACT",
        "SUBSTRING",
        "TRIM",
        "UPPER",
        "LOWER",
        "LENGTH",
        "REPLACE",
        "CONCAT",
        "ABS",
        "ROUND",
        "CEILING",
        "FLOOR",
        "MOD",
        "POWER",
        "SQRT",
        "COUNT",
        "SUM",
        "AVG",
        "MIN",
        "MAX",
        "CHAR",
        "VARCHAR",
        "DATE",
        "TIME",
        "TIMESTAMP",
        "INTEGER",
        "INT",
        "SMALLINT",
        "BIGINT",
        "FLOAT",
        "DOUBLE",
        "DECIMAL",
        "NUMERIC",
        "BOOLEAN",
        "TEXT",
        "ESCAPE",
        "ACTION",
        "NO",
    ];
    const PG_EXTRA: &[&str] = &[
        "ILIKE",
        "SIMILAR",
        "REGEXP",
        "LATERAL",
        "RECURSIVE",
        "WINDOW",
        "PARTITION",
        "ROWS",
        "RANGE",
        "GROUPS",
        "FILTER",
        "FETCH",
        "SHARE",
        "NOWAIT",
        "CONFLICT",
        "DO",
        "NOTHING",
        "SERIALIZABLE",
        "REPEATABLE",
        "DEFERRABLE",
        "INITIALLY",
        "VACUUM",
        "ANALYSE",
        "LISTEN",
        "NOTIFY",
        "LOAD",
        "SAVEPOINT",
        "PREPARE",
        "EXECUTE",
        "DEALLOCATE",
        "DISCARD",
        "RESET",
        "REASSIGN",
        "OWNED",
        "OBJECT",
        "PRIVILEGES",
        "TABLESPACE",
        "DATABASE",
        "ROLE",
        "PASSWORD",
        "VALID",
        "UNTIL",
        "CONNECTION",
        "EXTENSION",
        "TYPE",
        "DOMAIN",
        "ENUM",
        "ARRAY",
        "JSONB",
        "JSON",
        "UUID",
        "XML",
        "MONEY",
        "BYTEA",
        "TIMESTAMPTZ",
        "TIMETZ",
        "INET",
        "CIDR",
        "MACADDR",
        "TSVECTOR",
        "GENERATED",
        "IDENTITY",
        "ALWAYS",
        "STORED",
        "IMMUTABLE",
        "STABLE",
        "VOLATILE",
        "LANGUAGE",
        "SQL",
        "RETURNS",
        "SETOF",
        "ORDINALITY",
        "SERIAL",
        "BIGSERIAL",
        "FOR",
        "SKIP",
        "LOCKED",
        "SERVER",
    ];
    const MYSQL_EXTRA: &[&str] = &[
        "ENGINE",
        "CHARSET",
        "COLLATE",
        "AUTO_INCREMENT",
        "UNSIGNED",
        "ZEROFILL",
        "TINYINT",
        "MEDIUMINT",
        "BIT",
        "TINYBLOB",
        "BLOB",
        "MEDIUMBLOB",
        "LONGBLOB",
        "TINYTEXT",
        "MEDIUMTEXT",
        "LONGTEXT",
        "NATIONAL",
        "VARYING",
        "STRAIGHT_JOIN",
        "SQL_NO_CACHE",
        "SQL_CALC_FOUND_ROWS",
        "LOCK",
        "MODE",
        "OUTFILE",
        "DUMPFILE",
        "DUPLICATE",
        "LAST_INSERT_ID",
        "BINLOG",
        "PURGE",
        "MASTER",
        "SLAVE",
        "START",
        "STOP",
        "REPAIR",
        "OPTIMIZE",
        "CHECKSUM",
        "SHOW",
        "DESCRIBE",
        "EXTENDED",
        "PARTITION",
        "LESS",
        "THAN",
        "MAXVALUE",
        "LINEAR",
        "SUBPARTITION",
        "SIGNED",
        "BINARY",
        "VARBINARY",
        "DELIMITER",
        "DELAYED",
        "HIGH_PRIORITY",
        "LOW_PRIORITY",
        "QUICK",
        "ENFORCED",
        "CLUSTER",
        "GTID",
        "REPLICA",
        "SOURCE",
    ];
    static PG_KEYWORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    static MYSQL_KEYWORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    fn build(core: &'static [&'static str], extra: &'static [&'static str]) -> Vec<&'static str> {
        let mut all: Vec<&'static str> = core.to_vec();
        all.extend_from_slice(extra);
        all.sort_unstable();
        all.dedup();
        all
    }
    match dialect {
        // SQLite's completion set is Postgres's: the shared core plus the words a
        // SELECT actually uses. A MySQL-only word (STRAIGHT_JOIN) would be a lie here.
        DbDialect::Pg | DbDialect::Sqlite => PG_KEYWORDS.get_or_init(|| build(CORE, PG_EXTRA)),
        DbDialect::Mysql => MYSQL_KEYWORDS.get_or_init(|| build(CORE, MYSQL_EXTRA)),
    }
}
use std::sync::OnceLock;

/// The completion prefix: the [A-Za-z0-9_.$] run that ends at the caret. The panel fires the
/// request only when this is non-empty; dots let "public.us" complete as one word. `caret`
/// is a byte offset (the panel converts its UTF-16 selection index) and clamps to the text.
pub fn sql_word_ending_at(sql: &str, caret: usize) -> Option<&str> {
    let end = caret.min(sql.len());
    if !sql.is_char_boundary(end) {
        return None; // caret inside a multibyte character: no word to complete
    }
    let bytes = sql.as_bytes();
    let mut start = end;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        None
    } else {
        Some(&sql[start..end])
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'$'
}

/// The table the caret's statement reads FROM: the identifier after the NEAREST `FROM` before
/// the caret (dbgate's codeCompletion resolves the same way). Schema-qualified names come
/// back whole. No `FROM` before the caret — or one only after it — means keyword/table
/// candidates only.
pub fn completion_from_table(sql: &str, caret: usize) -> Option<String> {
    let end = caret.min(sql.len());
    if !sql.is_char_boundary(end) {
        return None;
    }
    let head = &sql[..end];
    let lower = head.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut pos = lower.len();
    while let Some(hit) = lower[..pos].rfind("from") {
        let before_ok = hit == 0 || !is_word_byte(bytes[hit - 1]);
        let after = hit + 4;
        let after_ok = after >= lower.len() || !is_word_byte(bytes[after]);
        if before_ok && after_ok {
            // The identifier that follows the FROM, if the query has grown one yet.
            let rest = &head[after..];
            let trimmed = rest.trim_start();
            let mut name_end = 0;
            for (i, b) in trimmed.bytes().enumerate() {
                if is_word_byte(b) {
                    name_end = i + 1;
                } else {
                    break;
                }
            }
            if name_end > 0 {
                return Some(trimmed[..name_end].to_string());
            }
            // FROM with nothing after it yet — keep looking further back.
        }
        if hit == 0 {
            break;
        }
        pos = hit;
    }
    None
}

/// The reply items for one prefix, ordered most-specific first: the FROM table's columns,
/// then matching table names, then keywords — each {label, kind, detail}. Matching is
/// case-insensitive prefix (SQL folds); the panel caps what it shows.
pub fn completion_items(
    dialect: DbDialect,
    prefix: &str,
    tables: &[String],
    columns: Option<(&str, &[String])>,
) -> Vec<Value> {
    let mut out = Vec::new();
    let upper = prefix.to_ascii_uppercase();
    let lower = prefix.to_ascii_lowercase();
    if let Some((table, cols)) = columns {
        for c in cols {
            if c.to_ascii_lowercase().starts_with(&lower) {
                out.push(json!({
                    "label": c,
                    "kind": "column",
                    "detail": format!("column of {table}"),
                }));
            }
        }
    }
    for t in tables {
        if t.to_ascii_lowercase().starts_with(&lower) {
            out.push(json!({ "label": t, "kind": "table", "detail": "table" }));
        }
    }
    for kw in sql_keywords(dialect) {
        if kw.starts_with(upper.as_str()) {
            out.push(json!({ "label": kw, "kind": "keyword", "detail": "keyword" }));
        }
    }
    out
}

/// Does this console statement change schema shape? The completion cache is invalidated
/// when it does — a stale column list after CREATE TABLE is worse than none.
pub fn sql_touches_schema(sql: &str) -> bool {
    let first = sql
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    matches!(
        first.to_ascii_uppercase().as_str(),
        "CREATE"
            | "ALTER"
            | "DROP"
            | "TRUNCATE"
            | "RENAME"
            | "COMMENT"
            | "GRANT"
            | "REVOKE"
            | "VACUUM"
            | "ANALYSE"
            | "ANALYZE"
            | "REFRESH"
            | "REINDEX"
    )
}

/// Per-connection completion cache (SPEC §data.completion): table names and column lists fetched
/// lazily on first use, served for COMPLETION_TTL, dropped whole the moment DDL runs. The
/// column half is byte-budgeted; past the budget the cache serves keywords + tables only
/// rather than memorising a schema nobody navigates by typing.
pub struct CompletionCache {
    tables: Option<(Vec<String>, Instant)>,
    columns: HashMap<String, (Vec<String>, Instant)>,
    bytes: usize,
    degraded: bool,
}

impl Default for CompletionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionCache {
    pub fn new() -> Self {
        Self {
            tables: None,
            columns: HashMap::new(),
            bytes: 0,
            degraded: false,
        }
    }

    /// Fresh table names, if a cached fetch is still worth serving.
    pub fn tables(&self, now: Instant) -> Option<Vec<String>> {
        self.tables
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) <= COMPLETION_TTL)
            .map(|(names, _)| names.clone())
    }

    pub fn set_tables(&mut self, names: Vec<String>, now: Instant) {
        self.tables = Some((names, now));
    }

    /// Fresh column names for one table, if cached and unexpired.
    pub fn columns(&self, table: &str, now: Instant) -> Option<Vec<String>> {
        self.columns
            .get(table)
            .filter(|(_, at)| now.duration_since(*at) <= COMPLETION_TTL)
            .map(|(names, _)| names.clone())
    }

    /// Cache one table's column names, refusing (and degrading) once the byte budget is
    /// blown. Table names keep serving: that list is one bounded query.
    pub fn set_columns(&mut self, table: String, names: Vec<String>, now: Instant) {
        if self.degraded {
            return;
        }
        let cost: usize = names.iter().map(|n| n.len() + 1).sum();
        // A refresh of the same table RETIRES its predecessor's bytes first — the W3 audit's
        // double-count let a TTL-refreshed schema blow the budget all by itself and degrade
        // a cache whose real footprint never moved.
        let retired: usize = self
            .columns
            .get(&table)
            .map(|(old, _)| old.iter().map(|n| n.len() + 1).sum())
            .unwrap_or(0);
        if self.bytes - retired + cost > COMPLETION_BUDGET_BYTES {
            self.degraded = true;
            return;
        }
        self.bytes = self.bytes - retired + cost;
        self.columns.insert(table, (names, now));
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Whether the table-name query must run before serving this completion — TTL-only: a
    /// degraded cache still serves its fresh table list (the budget caps column names, and
    /// re-querying the catalog on every keystroke is exactly what the cache exists to avoid).
    pub fn needs_tables(&self, now: Instant) -> bool {
        self.tables(now).is_none()
    }

    /// DDL ran: forget everything, including the degradation (the schema changed shape).
    pub fn invalidate(&mut self) {
        self.tables = None;
        self.columns.clear();
        self.bytes = 0;
        self.degraded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_explain_prefixes_once_and_strips_the_terminator() {
        assert_eq!(with_explain("SELECT * FROM t"), "EXPLAIN SELECT * FROM t");
        assert_eq!(with_explain("select 1"), "EXPLAIN select 1");
        assert_eq!(with_explain("EXPLAIN SELECT 1"), "EXPLAIN SELECT 1");
        assert_eq!(with_explain("explain select 1;"), "explain select 1");
        assert_eq!(with_explain("SELECT 1;\n"), "EXPLAIN SELECT 1");
    }

    // --- SPEC §data.completion: server-side completion -------------------------------------------------

    fn items_of(
        dialect: DbDialect,
        sql: &str,
        caret: usize,
        tables: &[&str],
        columns: &[&str],
    ) -> Vec<Value> {
        let tables: Vec<String> = tables.iter().map(|s| s.to_string()).collect();
        let cols: Vec<String> = columns.iter().map(|s| s.to_string()).collect();
        let prefix = sql_word_ending_at(sql, caret).unwrap_or("");
        let from = completion_from_table(sql, caret);
        let columns = from.map(|t| (t, cols));
        let columns = columns.as_ref().map(|(t, c)| (t.as_str(), c.as_slice()));
        completion_items(dialect, prefix, &tables, columns)
    }

    #[test]
    fn the_word_ending_at_the_caret_is_the_completion_prefix() {
        // Only [A-Za-z0-9_.$] count as word characters — the panel triggers on the same class.
        assert_eq!(sql_word_ending_at("SELECT * FROM us", 16), Some("us"));
        assert_eq!(sql_word_ending_at("SELECT na", 9), Some("na"));
        assert_eq!(
            sql_word_ending_at("SELECT public.us", 16),
            Some("public.us")
        );
        // A caret over whitespace or after ')' has no word — nothing to complete.
        assert_eq!(sql_word_ending_at("SELECT * FROM users ", 20), None);
        assert_eq!(sql_word_ending_at("SELECT 1)", 9), None);
        // Caret beyond the text clamps; an empty text has no word.
        assert_eq!(sql_word_ending_at("us", 99), Some("us"));
        assert_eq!(sql_word_ending_at("", 0), None);
    }

    #[test]
    fn the_nearest_from_before_the_caret_names_the_column_source() {
        assert_eq!(
            completion_from_table("SELECT * FROM users WHERE ", 25),
            Some("users".to_string())
        );
        // The NEAREST FROM wins — a subquery's FROM shadows the outer one. JOIN targets are
        // not resolved: the spec promises the FROM-nearest match, nothing fancier.
        assert_eq!(
            completion_from_table("SELECT * FROM a WHERE x IN (SELECT * FROM b ", 43),
            Some("b".to_string())
        );
        assert_eq!(
            completion_from_table("SELECT * FROM a JOIN b ON a.x = b.y WHERE ", 39),
            Some("a".to_string())
        );
        // Schema-qualified and case-folded.
        assert_eq!(
            completion_from_table("SELECT * FROM public.users WHERE ", 31),
            Some("public.users".to_string())
        );
        assert_eq!(
            completion_from_table("select * from users where ", 24),
            Some("users".to_string())
        );
        // No FROM before the caret: keyword/table candidates only.
        assert_eq!(completion_from_table("SELECT ", 7), None);
        // A FROM AFTER the caret does not count.
        assert_eq!(completion_from_table("SELECT x FROM users", 8), None);
    }

    #[test]
    fn completion_items_order_columns_tables_then_keywords() {
        let items = items_of(
            DbDialect::Pg,
            "SELECT * FROM users WHERE u",
            28,
            &["users", "user_events"],
            &["id", "user_name"],
        );
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        // The from-table's columns come first, then matching tables, then keywords.
        assert_eq!(labels[0], "user_name");
        assert!(labels.contains(&"users"));
        assert!(labels.contains(&"user_events"));
        // Every item speaks the panel's shape: label + kind + detail.
        for i in &items {
            assert!(i["label"].is_string());
            let kind = i["kind"].as_str().unwrap();
            assert!(kind == "column" || kind == "table" || kind == "keyword");
            assert!(i["detail"].is_string());
        }
        let col = items.iter().find(|i| i["label"] == "user_name").unwrap();
        assert_eq!(col["kind"], "column");
        assert_eq!(col["detail"], "column of users");
        let kw = items.iter().find(|i| i["label"] == "UNION").unwrap();
        assert_eq!(kw["kind"], "keyword");
    }

    #[test]
    fn each_dialect_carries_a_keyword_table_worth_offering() {
        for dialect in [DbDialect::Pg, DbDialect::Mysql] {
            let kws = sql_keywords(dialect);
            assert!(
                kws.len() >= 120,
                "{} has {} keywords",
                dialect.as_str(),
                kws.len()
            );
            for core in [
                "SELECT", "FROM", "WHERE", "JOIN", "INSERT", "UPDATE", "DELETE", "WITH",
            ] {
                assert!(
                    kws.contains(&core),
                    "{core} missing from {}",
                    dialect.as_str()
                );
            }
        }
        // Dialect-specific spellings exist on their dialect only.
        assert!(sql_keywords(DbDialect::Pg).contains(&"ILIKE"));
        assert!(!sql_keywords(DbDialect::Mysql).contains(&"ILIKE"));
        assert!(sql_keywords(DbDialect::Mysql).contains(&"AUTO_INCREMENT"));
        assert!(!sql_keywords(DbDialect::Pg).contains(&"AUTO_INCREMENT"));
    }

    #[test]
    fn a_completion_cache_expires_refreshes_and_degrades_over_budget() {
        let mut cache = CompletionCache::new();
        let t0 = Instant::now();
        assert!(cache.tables(t0).is_none());
        cache.set_tables(vec!["users".into()], t0);
        assert_eq!(cache.tables(t0).unwrap(), vec!["users".to_string()]);
        // Fresh for the whole TTL, expired one tick past it.
        assert!(cache.tables(t0 + COMPLETION_TTL).is_some());
        assert!(cache
            .tables(t0 + COMPLETION_TTL + Duration::from_millis(1))
            .is_none());

        cache.set_columns("users".into(), vec!["id".into()], t0);
        assert_eq!(cache.columns("users", t0).unwrap(), vec!["id".to_string()]);
        assert!(cache
            .columns("users", t0 + COMPLETION_TTL + Duration::from_millis(1))
            .is_none());

        // The byte budget caps cached column NAMES; past it the cache degrades to keywords
        // + tables instead of growing without bound on a wide schema.
        let wide: Vec<String> = (0..4000)
            .map(|i| format!("column_name_number_{i:05}"))
            .collect();
        cache.set_columns("wide".into(), wide, t0);
        assert!(cache.is_degraded());
        assert!(cache.columns("wide", t0).is_none());
        // Tables keep serving even degraded — the list is one bounded query.
        assert!(cache.tables(t0).is_some());

        // A DDL invalidates everything, degraded flag included.
        cache.invalidate();
        assert!(cache.tables(t0).is_none());
        assert!(!cache.is_degraded());
    }

    #[test]
    fn a_ttl_refresh_of_the_same_table_replaces_its_own_bytes() {
        // W3 audit: a TTL refresh used to add the new list's cost on top of the old entry's
        // without retiring it, so bytes grew on every refresh and a schema far under the
        // budget degraded the cache after a few minutes of typing.
        let mut cache = CompletionCache::new();
        let t0 = Instant::now();
        let cols: Vec<String> = (0..200)
            .map(|i| format!("column_with_a_long_name_{i:03}"))
            .collect();
        for round in 0..20u32 {
            let at = t0 + COMPLETION_TTL * round + Duration::from_secs(1);
            cache.set_columns("big".into(), cols.clone(), at);
        }
        assert!(
            !cache.is_degraded(),
            "20 refreshes of one ~5KB list must not blow the 64KB budget"
        );
        let served = t0 + COMPLETION_TTL * 19 + Duration::from_secs(2);
        assert_eq!(cache.columns("big", served).unwrap().len(), 200);
    }

    #[test]
    fn a_degraded_cache_does_not_requery_tables_within_the_ttl() {
        // The adapters' comment says tables are "still served when degraded", but the plan
        // re-queried them on every keystroke anyway. needs_tables is TTL-only: a fresh list
        // serves, expired or missing re-queries, degraded alone changes nothing.
        let mut cache = CompletionCache::new();
        let t0 = Instant::now();
        cache.set_tables(vec!["t1".into()], t0);
        let huge: Vec<String> = (0..10_000).map(|i| format!("c{i:05}")).collect();
        cache.set_columns("x".into(), huge, t0);
        assert!(cache.is_degraded());
        assert!(!cache.needs_tables(t0 + Duration::from_secs(1)));
        assert!(cache.needs_tables(t0 + COMPLETION_TTL + Duration::from_millis(1)));
    }
}
