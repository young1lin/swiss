//! The adapter-SQL wiring assertions of dbbrowser.test.ts - they pin constants owned by
//! adapters/pg.rs and adapters/mysql.rs, checked from swiss-mcp (which owns them) so the
//! contract travels with the adapters; the browser consumes these statements verbatim.
//! Lived in dbbrowser.rs before the workspace split.

use swiss_mcp::adapters::mysql::{
    MYSQL_BROWSE_COLUMNS_SQL, MYSQL_BROWSE_FK_SQL, MYSQL_BROWSE_INDEXES_SQL,
};
use swiss_mcp::adapters::pg::{
    pg_browse_table_params, pg_list_tables_sql, COUNT_TABLES_SQL, DESCRIBE_SQL, LIST_TABLES_SQL,
};
use swiss_host::dbbrowser::{browse_table_sort, TableSort, TableSortKey};
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
    assert_eq!(pg_browse_table_params(None, None).len(), 2);
    assert_eq!(
        pg_browse_table_params(None, Some("us")),
        vec![json!(null), json!("%us%")]
    );
    // docs/22 W1.1: a schema pick fills the $1 slot the same statement always carried.
    assert_eq!(
        pg_browse_table_params(Some("app"), Some("us")),
        vec![json!("app"), json!("%us%")]
    );
    assert_eq!(
        max_placeholder(LIST_TABLES_SQL),
        pg_browse_table_params(None, Some("us")).len() + 2
    );
    assert_eq!(
        max_placeholder(COUNT_TABLES_SQL),
        pg_browse_table_params(None, Some("us")).len()
    );
}

#[test]
fn pg_table_list_sort_rewrites_only_the_order_by() {
    let name_asc = browse_table_sort(None, None).unwrap();
    assert_eq!(name_asc, TableSort { key: TableSortKey::Name, desc: false });
    // name/asc is the tool's standing statement with a case-insensitive name inside each
    // schema — the default sort must not drift from the constant the MCP tool still uses.
    assert_eq!(
        pg_list_tables_sql(name_asc).replace("n.nspname, lower(c.relname)", "1, 2"),
        LIST_TABLES_SQL
    );
    assert!(pg_list_tables_sql(TableSort { key: TableSortKey::Name, desc: true })
        .contains("n.nspname, lower(c.relname) DESC"));
    assert!(pg_list_tables_sql(TableSort { key: TableSortKey::Rows, desc: true })
        .contains("ORDER BY approx_rows DESC NULLS LAST, n.nspname, c.relname"));
    assert!(pg_list_tables_sql(TableSort { key: TableSortKey::Size, desc: false })
        .contains("ORDER BY pg_total_relation_size(c.oid), n.nspname, c.relname"));
}

#[test]
fn table_list_sort_refuses_unknown_keys_and_directions() {
    assert!(browse_table_sort(Some("evil; --"), Some("asc")).is_err());
    assert!(browse_table_sort(Some("name"), Some("sideways")).is_err());
    assert_eq!(
        browse_table_sort(Some("rows"), None).unwrap(),
        TableSort { key: TableSortKey::Rows, desc: false }
    );
    assert_eq!(
        browse_table_sort(None, Some("desc")).unwrap(),
        TableSort { key: TableSortKey::Name, desc: true }
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
