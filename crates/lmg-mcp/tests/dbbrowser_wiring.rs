//! The adapter-SQL wiring assertions of dbbrowser.test.ts - they pin constants owned by
//! adapters/pg.rs and adapters/mysql.rs, checked from lmg-mcp (which owns them) so the
//! contract travels with the adapters; the browser consumes these statements verbatim.
//! Lived in dbbrowser.rs before the workspace split.

use lmg_mcp::adapters::mysql::{
    MYSQL_BROWSE_COLUMNS_SQL, MYSQL_BROWSE_FK_SQL, MYSQL_BROWSE_INDEXES_SQL,
};
use lmg_mcp::adapters::pg::{
    pg_browse_table_params, COUNT_TABLES_SQL, DESCRIBE_SQL, LIST_TABLES_SQL,
};
use serde_json::json;

/// The Node test's `placeholders()` — the highest $n a statement carries, scanned by hand
/// (ADR-007: no regex engine).
fn max_placeholder(sql: &str) -> usize {
    let b = sql.as_bytes();
    let mut max = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'$' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                if let Ok(n) = std::str::from_utf8(&b[i + 1..j])
                    .unwrap_or("0")
                    .parse::<usize>()
                {
                    max = max.max(n);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    max
}

#[test]
fn pg_table_list_supplies_every_placeholder() {
    // Regression: the Data view's listTables once passed [grep] alone against a statement
    // with $1..$4, and Postgres refused every page with "bind message supplies N parameters,
    // but prepared statement requires N+1".
    assert_eq!(pg_browse_table_params(None).len(), 2);
    assert_eq!(
        pg_browse_table_params(Some("us")),
        vec![json!(null), json!("%us%")]
    );
    assert_eq!(
        max_placeholder(LIST_TABLES_SQL),
        pg_browse_table_params(Some("us")).len() + 2
    );
    assert_eq!(
        max_placeholder(COUNT_TABLES_SQL),
        pg_browse_table_params(Some("us")).len()
    );
}

#[test]
fn mysql_aliases_never_use_the_reserved_word_column() {
    // Regression: COLUMN is a reserved word in MySQL 8 — an alias spelled "column" once made
    // every schema-tab query die with a syntax error near 'column'.
    for sql in [MYSQL_BROWSE_INDEXES_SQL, MYSQL_BROWSE_FK_SQL] {
        let lower = sql.to_ascii_lowercase();
        assert!(!lower.contains("as column"), "reserved alias in: {sql}");
        assert!(lower.contains("as col"), "missing col alias in: {sql}");
    }
}

#[test]
fn column_comment_wiring() {
    // The header tooltip shows the column COMMENT, so the column queries must fetch it —
    // and the pg one must do so without growing the bind list its callers supply.
    assert!(MYSQL_BROWSE_COLUMNS_SQL
        .to_ascii_lowercase()
        .contains("column_comment as column_comment"));
    let describe = DESCRIBE_SQL.to_ascii_lowercase();
    assert!(describe.contains("col_description("));
    assert!(describe.contains("as column_comment"));
    assert_eq!(max_placeholder(DESCRIBE_SQL), 2); // callers still bind exactly [schema, table]
}
