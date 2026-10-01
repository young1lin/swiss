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

//! The in-process PostgreSQL adapter — port of `adapters/pg.ts`. A small sqlx pool behind one
//! query tool, two discovery tools and a diagnostics tool; multi-statement pg_query keeps its
//! Node semantics (the simple protocol runs them, one summary per statement).

use std::pin::pin;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::postgres::{PgColumn, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
// sqlx 0.9 accepts only `&'static str` as a statement unless the caller asserts otherwise.
// Every statement this module runs is either the operator's own SQL in their own client (the
// console, the grid's query) or one the Data view built with quoted identifiers and bound
// values, so each call site says `AssertSqlSafe`; the guards live upstream, not in sqlx.
use sqlx::{AssertSqlSafe, Column, Either, Row};

use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{
    bytea_hex, exact_int64, exact_int64_list, finite_f64, quote_ident, DbDialect, TableSort,
    TableSortKey,
};

use super::direct::{BoxFut, Lazy};
use super::pg_resources::PgResources;
use super::sql::{
    clamp_row_limit, drop_null_columns, grep_arg, inspect_args, inspect_description,
    inspect_reply, inspect_schema, like_contains, limit_report, page_arg, row_limit_arg,
    table_limit_arg, table_page_args, with_row_limit, Check, DEFAULT_ROW_LIMIT,
};
use super::tool_server::{Engine, ServerMeta, ToolDef};

fn schema_arg() -> Value {
    json!({ "type": "string", "description": "Schema name. Defaults to every non-system schema." })
}
fn table_arg() -> Value {
    json!({ "type": "string", "description": "Table name (unqualified)." })
}

/// Four tools: run a statement; the two things a model cannot guess — what tables exist (with
/// sizes, so it knows what needs a LIMIT) and what one of them looks like, keys, indexes and
/// incoming references included; and the health checks a DBA would otherwise hand-write against
/// the statistics views (the inspect tools of Neon's and crystaldba's Postgres MCPs).
///
/// Deliberately no pg_list_indexes / pg_list_schemas / pg_explain: pg_describe_table already
/// carries the indexes, pg_list_tables the schema column, and EXPLAIN is a pg_query — while
/// every tool schema is re-sent on every request. `max_rows` is this instance's cap for a
/// LIMIT-less SELECT (its `maxRows`), stated as the number it is.
fn tools(max_rows: i64) -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "pg_query".into(),
            description: format!(
                "Run a SQL statement. Returns {{ command, rowCount, rows }} — so an UPDATE/DELETE reports how \
                 many rows it actually touched. A SELECT written without its own LIMIT is capped at \
                 {max_rows} rows and says so in the reply — raise `limit` or write your own \
                 LIMIT/OFFSET to page through more. NULL columns are omitted from each row (pg_describe_table \
                 gives the full column list), so on a wide table name the columns you need rather than SELECT *."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string", "description": "The SQL statement to execute." },
                    "limit": row_limit_arg(max_rows),
                },
                "required": ["sql"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "pg_list_tables".into(),
            description: concat!(
                "List tables and views with their approximate row count and on-disk size — use this before ",
                "querying, to know what exists and what is big enough to need a LIMIT. Paged: limit per page ",
                "(default 200, max 1000) and 0-based page; the reply carries total and more. Pass `grep` to ",
                "keep only names containing it: grep \"users\" lists p_users, users_settings, …",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "schema": schema_arg(), "grep": grep_arg(), "limit": table_limit_arg(), "page": page_arg() },
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "pg_describe_table".into(),
            description: concat!(
                "Everything about one table or view in one call: columns (type, nullability, default, ",
                "identity/generated, comment), the primary key in key order, indexes and constraints as ",
                "their DDL, foreign keys, the tables whose foreign keys point here (referencedBy), and the ",
                "row estimate, size and comment; a view also returns its definition. Fields with nothing to ",
                "say are omitted. For a plan, run EXPLAIN through pg_query.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "table": table_arg(),
                    "schema": {
                        "type": "string",
                        "description": "Schema name. Omit to resolve the table through the search_path (normally public).",
                    },
                },
                "required": ["table"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "pg_inspect".into(),
            description: inspect_description(
                "Diagnose the server with one built-in check per call — the statistics-view queries a \
                 DBA would otherwise write by hand. Read-only.",
                PG_CHECKS,
                "Counters accumulate since the last statistics reset. Returns { check, rows, \
                 summary?, note? }.",
            ),
            input_schema: inspect_schema(PG_CHECKS),
        },
    ]
}

pub const LIST_TABLES_SQL: &str = "
  SELECT n.nspname AS schema,
         c.relname  AS name,
         CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview'
                        WHEN 'p' THEN 'partitioned table' WHEN 'f' THEN 'foreign table' END AS type,
         -- reltuples is -1 when the table has never been analyzed; report that as unknown rather
         -- than as a row count of minus one.
         CASE WHEN c.reltuples < 0 THEN NULL ELSE c.reltuples::bigint END AS approx_rows,
         pg_size_pretty(pg_total_relation_size(c.oid)) AS size
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','v','m','p','f')
     AND n.nspname NOT IN ('pg_catalog','information_schema')
     AND ($1::text IS NULL OR n.nspname = $1)
     -- $2 is a ready-built %substring% pattern (likeContains); ESCAPE '!' keeps its % and _ literal.
     AND ($2::text IS NULL OR c.relname ILIKE $2 ESCAPE '!')
   ORDER BY 1, 2
   LIMIT $3 OFFSET $4";

/// Same filter, counted — so a page can say how much of the list is behind it.
pub const COUNT_TABLES_SQL: &str = "
  SELECT count(*)::int AS total
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','v','m','p','f')
     AND n.nspname NOT IN ('pg_catalog','information_schema')
     AND ($1::text IS NULL OR n.nspname = $1)
     AND ($2::text IS NULL OR c.relname ILIKE $2 ESCAPE '!')";

/// The Data view's table list with a chosen sort — Node's pgListTablesSql. The MCP tool keeps
/// LIST_TABLES_SQL as-is (tools have no sort argument); the browser swaps the ORDER BY: name
/// sorts SCHEMA-major and case-insensitively (the tool's ORDER BY 1, 2, made lower(relname)
/// inside a schema, so a C-collation database does not rank 'Zebra' above 'apple'), rows/size
/// sort by the catalog estimate / on-disk bytes with NULLS LAST (never-analyzed tables stay at
/// the bottom) and abandon the schema grouping on purpose, with (schema, name) as the
/// tiebreaker. Node replaces the literal ORDER BY; so does this, one string swap.
pub fn pg_list_tables_sql(sort: TableSort) -> String {
    with_pg_list_order(LIST_TABLES_SQL.to_string(), sort)
}

/// Node replaces the literal ORDER BY; so does this, one string swap.
fn with_pg_list_order(sql: String, sort: TableSort) -> String {
    let desc = if sort.desc { " DESC" } else { "" };
    let clause = match sort.key {
        TableSortKey::Rows => {
            format!("approx_rows{desc} NULLS LAST, n.nspname, c.relname")
        }
        TableSortKey::Size => {
            format!("pg_total_relation_size(c.oid){desc}, n.nspname, c.relname")
        }
        TableSortKey::Name => format!("n.nspname, lower(c.relname){desc}"),
    };
    sql.replacen("ORDER BY 1, 2", &format!("ORDER BY {clause}"), 1)
}

/// The grammar-grep twin (SPEC §data.browse): the const SQL has ONE grep placeholder, but a grammar
/// grep expands to any number of patterns, so the same query text is assembled here with the
/// predicate grep_where built inline and the LIMIT/OFFSET placeholders renumbered past it.
/// `pred` arrives as " AND (...)" from grep_where — the leading AND is the one the replaced
/// line carried. A plain substring grep never reaches here; pg_list_tables_sql keeps its SQL
/// byte-for-byte.
pub fn pg_list_tables_grammar_sql(
    sort: TableSort,
    pred: &str,
    patterns: usize,
) -> (String, String) {
    // The single $2 grep bind becomes N patterns, so LIMIT/OFFSET shift by N-1: $3/$4 →
    // $(N+2)/$(N+3).
    let limit_n = patterns + 2;
    let offset_n = patterns + 3;
    let inline = pred.trim_start();
    let list = LIST_TABLES_SQL
        .replacen(
            "AND ($2::text IS NULL OR c.relname ILIKE $2 ESCAPE '!')",
            inline,
            1,
        )
        .replacen(
            "LIMIT $3 OFFSET $4",
            &format!("LIMIT ${limit_n} OFFSET ${offset_n}"),
            1,
        );
    let count = COUNT_TABLES_SQL.replacen(
        "AND ($2::text IS NULL OR c.relname ILIKE $2 ESCAPE '!')",
        inline,
        1,
    );
    (with_pg_list_order(list, sort), count)
}
pub const DESCRIBE_SQL: &str = "
  SELECT column_name,
         CASE WHEN data_type = 'USER-DEFINED' THEN udt_name ELSE data_type END AS data_type,
         is_nullable, column_default,
         character_maximum_length, numeric_precision, numeric_scale,
         col_description(format('%I.%I', table_schema, table_name)::regclass, ordinal_position) AS column_comment
    FROM information_schema.columns
   WHERE table_schema = $1 AND table_name = $2
   ORDER BY ordinal_position";

pub const PK_SQL: &str = "
  SELECT a.attname AS column
    FROM pg_index i
    JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
   WHERE i.indrelid = format('%I.%I', $1::text, $2::text)::regclass
     AND i.indisprimary";

/// Indexes with their full CREATE INDEX statement, one row per index. pg_index is joined (not
/// just pg_indexes) because only it carries indisprimary — pg_indexes lists the PRIMARY KEY
/// index like any other unique index, and its name is only CONVENTIONALLY <table>_pkey.
pub const PG_BROWSE_INDEXES_SQL: &str = "
  SELECT ic.relname AS name, pg_get_indexdef(i.indexrelid) AS definition,
         CASE WHEN i.indisprimary THEN 1 ELSE 0 END AS is_primary
    FROM pg_index i
    JOIN pg_class ic ON ic.oid = i.indexrelid
    JOIN pg_namespace n ON n.oid = ic.relnamespace
   WHERE n.nspname = $1
     AND i.indrelid = format('%I.%I', $1::text, $2::text)::regclass
   ORDER BY ic.relname";

/// Foreign keys: one row per column of each referencing constraint (LATERAL UNNEST walks the
/// conkey/confkey column-number pairs in lockstep).
pub const PG_BROWSE_FK_SQL: &str = "
  SELECT con.conname AS name, a.attname AS column,
         fn.nspname AS ref_schema, cf.relname AS ref_table, af.attname AS ref_column
    FROM pg_constraint con
    JOIN pg_class c ON c.oid = con.conrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    JOIN pg_class cf ON cf.oid = con.confrelid
    JOIN pg_namespace fn ON fn.oid = cf.relnamespace
    CROSS JOIN LATERAL UNNEST(con.conkey, con.confkey) AS k(attnum, ref_attnum)
    JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum
    JOIN pg_attribute af ON af.attrelid = con.confrelid AND af.attnum = k.ref_attnum
   WHERE con.contype = 'f' AND n.nspname = $1 AND c.relname = $2
   ORDER BY con.conname, a.attnum";

/// pg_describe_table in one round trip, as one JSON document with its NULL fields stripped.
/// `$1` is the schema or NULL — then to_regclass resolves the name through the search_path the
/// way an unqualified name in a query would. Both names are quoted with %I, so a mixed-case or
/// dotted name means exactly itself. The primary key comes in key order (unnest WITH
/// ORDINALITY over indkey), not table order. A foreign key into a partitioned table is cloned
/// once per partition (conparentid): foreignKeys drops the clones on the same table, keeping a
/// partition's inherited key, and referencedBy lists each constraint once, at its root.
pub const DESCRIBE_TABLE_SQL: &str = "
  SELECT json_strip_nulls(row_to_json(r)) AS doc FROM (
  WITH t AS (
    SELECT c.oid, n.nspname, c.relname, c.relkind, c.reltuples
      FROM pg_class c
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE c.relkind IN ('r','v','m','p','f')
       AND c.oid = to_regclass(CASE WHEN $1::text IS NULL THEN format('%I', $2::text)
                                    ELSE format('%I.%I', $1::text, $2::text) END)
  )
  SELECT t.nspname AS schema, t.relname AS \"table\",
         CASE t.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'matview'
                        WHEN 'p' THEN 'partitioned table' WHEN 'f' THEN 'foreign table' END AS type,
         obj_description(t.oid, 'pg_class') AS comment,
         CASE WHEN t.relkind IN ('r','m','p') AND t.reltuples >= 0
              THEN t.reltuples::bigint END AS \"approxRows\",
         CASE WHEN t.relkind IN ('r','m','p')
              THEN pg_size_pretty(pg_total_relation_size(t.oid)) END AS size,
         CASE WHEN t.relkind IN ('v','m') THEN pg_get_viewdef(t.oid, true) END AS definition,
         (SELECT json_agg(json_strip_nulls(json_build_object(
                   'name', a.attname,
                   'type', format_type(a.atttypid, a.atttypmod),
                   'nullable', NOT a.attnotnull,
                   'default', CASE WHEN a.attgenerated = ''
                                   THEN pg_get_expr(d.adbin, d.adrelid) END,
                   'generated', CASE WHEN a.attgenerated <> ''
                                     THEN pg_get_expr(d.adbin, d.adrelid) END,
                   'identity', CASE a.attidentity WHEN 'a' THEN 'always'
                                                  WHEN 'd' THEN 'by default' END,
                   'comment', col_description(t.oid, a.attnum))) ORDER BY a.attnum)
            FROM pg_attribute a
            LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
           WHERE a.attrelid = t.oid AND a.attnum > 0 AND NOT a.attisdropped) AS columns,
         CASE WHEN t.relkind IN ('r','p') THEN coalesce((
           SELECT json_agg(a.attname ORDER BY k.ord)
             FROM pg_index i
             CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
            WHERE i.indrelid = t.oid AND i.indisprimary), '[]') END AS \"primaryKey\",
         (SELECT json_agg(json_strip_nulls(json_build_object(
                   'name', ic.relname,
                   'definition', pg_get_indexdef(i.indexrelid),
                   'invalid', CASE WHEN NOT i.indisvalid THEN true END)) ORDER BY ic.relname)
            FROM pg_index i
            JOIN pg_class ic ON ic.oid = i.indexrelid
           WHERE i.indrelid = t.oid AND NOT i.indisprimary) AS indexes,
         (SELECT json_agg(json_build_object(
                   'name', con.conname,
                   'definition', pg_get_constraintdef(con.oid, true)) ORDER BY con.conname)
            FROM pg_constraint con
           WHERE con.conrelid = t.oid AND con.contype = 'f'
             AND NOT EXISTS (SELECT 1 FROM pg_constraint p
                              WHERE p.oid = con.conparentid AND p.conrelid = con.conrelid))
           AS \"foreignKeys\",
         (SELECT json_agg(json_build_object(
                   'name', con.conname,
                   'definition', pg_get_constraintdef(con.oid, true)) ORDER BY con.conname)
            FROM pg_constraint con
           WHERE con.conrelid = t.oid AND con.contype IN ('c','x')) AS constraints,
         (SELECT json_agg(json_build_object(
                   'table', format('%I.%I', rn.nspname, rc.relname),
                   'name', con.conname,
                   'definition', pg_get_constraintdef(con.oid, true))
                 ORDER BY rn.nspname, rc.relname, con.conname)
            FROM pg_constraint con
            JOIN pg_class rc ON rc.oid = con.conrelid
            JOIN pg_namespace rn ON rn.oid = rc.relnamespace
           WHERE con.confrelid = t.oid AND con.contype = 'f'
             AND con.conparentid = 0) AS \"referencedBy\"
    FROM t
  ) r";

/// The miss message of pg_describe_table, steering the next call: a dotted name with no schema
/// is almost always `schema.table` written as one string.
fn describe_miss(schema: Option<&str>, table: &str) -> String {
    match (schema, table.split_once('.')) {
        (Some(schema), _) => {
            format!("no table or view {schema}.{table} — pg_list_tables lists what exists")
        }
        (None, Some((s, t))) => format!(
            "no table or view named \"{table}\" on the search_path — for a qualified name pass \
             the parts apart: schema \"{s}\", table \"{t}\""
        ),
        (None, None) => format!(
            "no table or view named \"{table}\" on the search_path — pass its schema, or find it \
             with pg_list_tables"
        ),
    }
}

/// Where pg_stat_statements is installed in this database, if it is.
const PGSS_SCHEMA_SQL: &str = "
  SELECT n.nspname AS schema
    FROM pg_extension e
    JOIN pg_namespace n ON n.oid = e.extnamespace
   WHERE e.extname = 'pg_stat_statements'";

const PGSS_MISSING: &str = "top_queries reads the pg_stat_statements extension, which this \
     database does not have: add pg_stat_statements to shared_preload_libraries (a restart), \
     then run CREATE EXTENSION pg_stat_statements in this database";

/// pg_inspect's checks. Every row query takes the row limit as `$1`; `{pgss}` in top_queries is
/// the extension's schema, quoted, found at call time. Ratios and seconds are cast to float8 so
/// they arrive as numbers, not NUMERIC strings.
pub const PG_CHECKS: &[Check] = &[
    Check {
        name: "activity",
        about: "running statements and open transactions, longest first",
        rows: Some(
            "
  SELECT pid, datname AS database, usename AS \"user\", application_name AS application,
         client_addr::text AS client, state,
         wait_event_type || ': ' || wait_event AS waiting_on,
         round(extract(epoch FROM now() - query_start)::numeric, 1)::float8 AS query_seconds,
         round(extract(epoch FROM now() - xact_start)::numeric, 1)::float8 AS xact_seconds,
         left(query, 1000) AS query
    FROM pg_stat_activity
   WHERE backend_type = 'client backend' AND state IS DISTINCT FROM 'idle'
     AND pid <> pg_backend_pid()
   ORDER BY coalesce(xact_start, query_start) NULLS LAST
   LIMIT $1",
        ),
        summary: None,
        note: Some(
            "Other roles' sessions show their query and timings only to superusers and members \
             of pg_read_all_stats.",
        ),
    },
    Check {
        name: "locks",
        about: "sessions waiting on a lock, with the session blocking each",
        rows: Some(
            "
  SELECT w.pid AS waiting_pid, w.usename AS waiting_user,
         (SELECT l.mode || ' on ' || coalesce(l.relation::regclass::text, l.locktype)
            FROM pg_locks l WHERE l.pid = w.pid AND NOT l.granted LIMIT 1) AS waiting_for,
         round(extract(epoch FROM now() - w.query_start)::numeric, 1)::float8 AS waiting_seconds,
         left(w.query, 500) AS waiting_query,
         b.pid AS blocking_pid, b.usename AS blocking_user, b.state AS blocking_state,
         round(extract(epoch FROM now() - b.xact_start)::numeric, 1)::float8
           AS blocking_xact_seconds,
         left(b.query, 500) AS blocking_query
    FROM pg_stat_activity w
    CROSS JOIN LATERAL unnest(pg_blocking_pids(w.pid)) AS bp(pid)
    JOIN pg_stat_activity b ON b.pid = bp.pid
   WHERE w.wait_event_type = 'Lock'
   ORDER BY w.query_start
   LIMIT $1",
        ),
        summary: None,
        note: Some(
            "blocking_query is the blocker's latest statement, not necessarily the one that took \
             the lock — an idle-in-transaction blocker is holding it until it commits.",
        ),
    },
    Check {
        name: "top_queries",
        about: "statements by total execution time (needs the pg_stat_statements extension)",
        rows: Some(
            "
  SELECT left(s.query, 1000) AS query, s.calls,
         round(s.total_exec_time::numeric, 1)::float8 AS total_ms,
         round(s.mean_exec_time::numeric, 2)::float8 AS mean_ms,
         round(s.max_exec_time::numeric, 2)::float8 AS max_ms,
         round((100 * s.total_exec_time
                / nullif(sum(s.total_exec_time) OVER (), 0))::numeric, 1)::float8 AS pct_of_total,
         s.rows,
         round((100.0 * s.shared_blks_hit
                / nullif(s.shared_blks_hit + s.shared_blks_read, 0))::numeric, 1)::float8
           AS cache_hit_pct
    FROM {pgss}.pg_stat_statements s
   WHERE s.dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
   ORDER BY s.total_exec_time DESC
   LIMIT $1",
        ),
        summary: None,
        note: Some(
            "Query text is normalized (constants read as $1, $2, …); counters run since \
             pg_stat_statements_reset().",
        ),
    },
    Check {
        name: "table_scans",
        about: "tables read by sequential scans, most rows read first",
        rows: Some(
            "
  SELECT schemaname AS schema, relname AS \"table\", seq_scan,
         seq_tup_read AS seq_rows_read, seq_tup_read / seq_scan AS rows_per_seq_scan,
         idx_scan, n_live_tup AS live_rows, pg_size_pretty(pg_relation_size(relid)) AS size
    FROM pg_stat_user_tables
   WHERE seq_scan > 0
   ORDER BY seq_tup_read DESC
   LIMIT $1",
        ),
        summary: None,
        note: Some(
            "Sequential scans of a small table are normal; a large table with a high \
             rows_per_seq_scan is a WHERE clause no index serves.",
        ),
    },
    Check {
        name: "vacuum",
        about: "dead rows, last vacuum and analyze, and transaction-ID age per table",
        rows: Some(
            "
  SELECT s.schemaname AS schema, s.relname AS \"table\",
         s.n_live_tup AS live_rows, s.n_dead_tup AS dead_rows,
         round((100.0 * s.n_dead_tup
                / nullif(s.n_live_tup + s.n_dead_tup, 0))::numeric, 1)::float8 AS dead_pct,
         greatest(s.last_vacuum, s.last_autovacuum) AS last_vacuum,
         greatest(s.last_analyze, s.last_autoanalyze) AS last_analyze,
         s.n_mod_since_analyze AS modified_since_analyze,
         CASE WHEN c.relkind IN ('r','m','t') THEN age(c.relfrozenxid) END AS xid_age
    FROM pg_stat_user_tables s
    JOIN pg_class c ON c.oid = s.relid
   ORDER BY s.n_dead_tup DESC, xid_age DESC NULLS LAST
   LIMIT $1",
        ),
        summary: Some(
            "
  SELECT age(datfrozenxid) AS database_xid_age,
         current_setting('autovacuum_freeze_max_age')::int AS autovacuum_freeze_max_age,
         (SELECT count(*)::int FROM pg_stat_activity
           WHERE backend_type = 'autovacuum worker') AS autovacuum_workers_running
    FROM pg_database WHERE datname = current_database()",
        ),
        note: Some(
            "Autovacuum forces a freeze once an xid_age passes autovacuum_freeze_max_age; at \
             about 2 billion the server stops accepting writes.",
        ),
    },
    Check {
        name: "indexes",
        about: "invalid and unused indexes, and tables without a primary key",
        rows: Some(
            "
  SELECT problem, schema, \"table\", \"index\", pg_size_pretty(bytes) AS size, definition
    FROM (
    SELECT 0 AS ord, 'invalid' AS problem, n.nspname AS schema, c.relname AS \"table\",
           ic.relname AS \"index\", pg_relation_size(i.indexrelid) AS bytes,
           pg_get_indexdef(i.indexrelid) AS definition
      FROM pg_index i
      JOIN pg_class ic ON ic.oid = i.indexrelid
      JOIN pg_class c ON c.oid = i.indrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE NOT i.indisvalid
    UNION ALL
    SELECT 1, 'unused', s.schemaname, s.relname, s.indexrelname,
           pg_relation_size(s.indexrelid), pg_get_indexdef(s.indexrelid)
      FROM pg_stat_user_indexes s
      JOIN pg_index i ON i.indexrelid = s.indexrelid
     WHERE s.idx_scan = 0 AND NOT i.indisunique AND NOT i.indisprimary
    UNION ALL
    SELECT 2, 'no primary key', n.nspname, c.relname, NULL, pg_table_size(c.oid), NULL
      FROM pg_class c
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE c.relkind IN ('r','p') AND NOT c.relispartition
       AND n.nspname <> 'information_schema' AND n.nspname !~ '^pg_'
       AND NOT EXISTS (SELECT 1 FROM pg_index pk WHERE pk.indrelid = c.oid AND pk.indisprimary)
  ) p
   ORDER BY ord, bytes DESC, schema, \"table\"
   LIMIT $1",
        ),
        summary: None,
        note: Some(
            "idx_scan counts since the last statistics reset and on this server only — an index \
             only a replica uses reads as unused here. Unique indexes enforce a constraint and \
             are never listed as unused.",
        ),
    },
    Check {
        name: "cache",
        about: "buffer cache hit ratios, overall and for the tables read most from disk",
        rows: Some(
            "
  SELECT schemaname AS schema, relname AS \"table\", heap_blks_read AS disk_reads,
         round((100.0 * heap_blks_hit
                / nullif(heap_blks_hit + heap_blks_read, 0))::numeric, 2)::float8 AS hit_pct,
         idx_blks_read AS index_disk_reads,
         round((100.0 * idx_blks_hit
                / nullif(idx_blks_hit + idx_blks_read, 0))::numeric, 2)::float8 AS index_hit_pct
    FROM pg_statio_user_tables
   WHERE heap_blks_read + coalesce(idx_blks_read, 0) > 0
   ORDER BY heap_blks_read + coalesce(idx_blks_read, 0) DESC
   LIMIT $1",
        ),
        summary: Some(
            "
  SELECT round((100.0 * sum(heap_blks_hit)
                / nullif(sum(heap_blks_hit) + sum(heap_blks_read), 0))::numeric, 2)::float8
           AS table_hit_pct,
         round((100.0 * sum(idx_blks_hit)
                / nullif(sum(idx_blks_hit) + sum(idx_blks_read), 0))::numeric, 2)::float8
           AS index_hit_pct,
         current_setting('shared_buffers') AS shared_buffers
    FROM pg_statio_user_tables",
        ),
        note: Some(
            "A read the OS page cache served still counts as a disk read here. A steady OLTP \
             workload usually sits above 99%.",
        ),
    },
    Check {
        name: "connections",
        about: "connections by database, user and state, against max_connections",
        rows: Some(
            "
  SELECT datname AS database, usename AS \"user\", state, count(*)::int AS connections,
         round(max(extract(epoch FROM now() - state_change))::numeric, 1)::float8
           AS longest_in_state_seconds
    FROM pg_stat_activity
   WHERE backend_type = 'client backend'
   GROUP BY 1, 2, 3
   ORDER BY 4 DESC, 1, 2, 3
   LIMIT $1",
        ),
        summary: Some(
            "
  SELECT (count(*) FILTER (WHERE backend_type = 'client backend'))::int AS client_connections,
         current_setting('max_connections')::int AS max_connections,
         current_setting('superuser_reserved_connections')::int AS reserved_for_superusers
    FROM pg_stat_activity",
        ),
        note: Some(
            "Other roles' sessions show their state only to superusers and members of \
             pg_read_all_stats.",
        ),
    },
];

/// The filter params LIST_TABLES_SQL / COUNT_TABLES_SQL expect: `[schema-or-null,
/// grep-or-null]` (the LIMIT/OFFSET pair is appended by the caller). The Data view lists every
/// non-system schema by default (null), but the panel's schema picker (SPEC §data.browse) and the
/// pg_list_tables tool both narrow the walk to one schema — and the slot must still BE there
/// either way, or the bind message supplies one parameter fewer than the statement's
/// placeholders and Postgres refuses the query.
pub fn pg_browse_table_params(schema: Option<&str>, grep: Option<&str>) -> Vec<Value> {
    vec![
        schema
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
        grep.map(like_contains)
            .map(Value::String)
            .unwrap_or(Value::Null),
    ]
}

/// The bind pair for the grammar list/count statements (SPEC §data.browse): schema first (the $1
/// slot every one of these statements carries), then the grammar patterns; the list appends the
/// paging pair, the count shares exactly the head. Extracted because the inline version once
/// sent the count without its schema bind and Postgres answered "bind message supplies 2
/// parameters, but prepared statement requires 3".
pub fn pg_grammar_params(
    schema: Option<&str>,
    patterns: &[Value],
    limit: i64,
    offset: i64,
) -> (Vec<Value>, Vec<Value>) {
    let mut head: Vec<Value> = vec![schema
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .unwrap_or(Value::Null)];
    head.extend(patterns.iter().cloned());
    let mut list = head.clone();
    list.push(json!(limit));
    list.push(json!(offset));
    (list, head)
}

/// `(host, port, database)` out of a postgres:// URL — `new URL()` + pathname in the Node build.
/// None when the URL is unusable, exactly where Node's try/catch returned "".
pub fn parse_pg_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))?;
    // Drop the query/fragment before anything else.
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let rest = &rest[..end];
    // Authority runs from the last '@' (userinfo may contain '@'-free but keep it simple —
    // passwords with '@' must be percent-encoded by the same rule in new URL()).
    let after_auth = match rest.rfind('@') {
        Some(i) => &rest[i + 1..],
        None => rest,
    };
    let (authority, path) = match after_auth.find('/') {
        Some(i) => (&after_auth[..i], Some(&after_auth[i + 1..])),
        None => (after_auth, None),
    };
    // host[:port], with IPv6 brackets.
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let tail = &rest[close + 1..];
        let port = tail.strip_prefix(':').map(|p| p.parse::<u16>().ok())?;
        (host.to_string(), port)
    } else {
        match authority.rfind(':') {
            Some(i) => (
                authority[..i].to_string(),
                authority[i + 1..].parse::<u16>().ok(),
            ),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let database = path
        .filter(|p| !p.is_empty())
        .map(super::resources::percent_decode)
        .unwrap_or_default();
    Some((host, port.unwrap_or(5432), database))
}

/// The connect options — port of `createPool`'s knobs. `max: 4`, `connectionTimeoutMillis:
/// 5000` and `idleTimeoutMillis: 60000` map onto pool options; `statement_timeout: 15000` rides
/// the startup `options` parameter. Nothing here makes the session read-only: the console and
/// the grid are the operator's own tools on the operator's own databases.
pub fn pg_connect_options(url: &str) -> Result<PgConnectOptions, String> {
    let opts = PgConnectOptions::from_str(url).map_err(|e| e.to_string())?;
    let opts = opts.options([("statement_timeout", "15000")]);
    Ok(opts)
}

// --- rows → JSON ----------------------------------------------------------------------------------

/// One column → one JSON value, dispatched on the type name sqlx prints. The mapping mirrors
/// node-postgres's defaults: INT8 and NUMERIC arrive as exact strings (a JS double silently
/// rounds 18-digit ids), JSON/JSONB parse, BYTEA decodes as UTF-8 (renderResult normalized
/// Buffers the same way).
///
/// Temporal columns deliberately do NOT reproduce node-pg's Date behavior: pg builds a JS Date
/// in the client's local zone and JSON.stringify re-prints it in UTC, so the Node reply was
/// zone-dependent. This port prints stable strings (DATE `2026-09-07`, TIMESTAMP
/// `2026-09-07 10:00:00`, TIMESTAMPTZ as UTC ISO-8601).
fn column_to_value(row: &PgRow, col: &PgColumn, i: usize) -> Value {
    let type_name = col.type_info().to_string();
    match type_name.as_str() {
        "BOOL" => row
            .try_get::<Option<bool>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "INT2" | "INT4" | "OID" => row
            .try_get::<Option<i32>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "INT8" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(exact_int64) // exact digits past the JS double boundary (SPEC §data.browse)
            .unwrap_or(Value::Null),
        "FLOAT4" | "FLOAT8" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(finite_f64) // NaN/±Inf have no JSON spelling; they read as NULL (W2.4)
            .unwrap_or(Value::Null),
        "NUMERIC" => row
            .try_get::<Option<bigdecimal::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.to_string()))
            .unwrap_or(Value::Null),
        "JSON" | "JSONB" => row
            .try_get::<Option<Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(Value::Null),
        // BYTEA rides the API as \x hex (bytea text format): non-UTF-8 bytes survive the
        // round trip, and the keyless md5 address digests the decoded bytes. The old lossy
        // string mangled every non-UTF-8 value and made such rows unaddressable.
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(i)
            .ok()
            .flatten()
            .map(|b| json!(bytea_hex(&b)))
            .unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.format("%Y-%m-%d").to_string()))
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|t| json!(t.format("%H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.format("%Y-%m-%d %H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()))
            .unwrap_or(Value::Null),
        "TEXT[]" | "VARCHAR[]" | "CHAR[]" | "BPCHAR[]" | "NAME[]" => row
            .try_get::<Option<Vec<String>>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "INT2[]" | "INT4[]" | "OID[]" => row
            .try_get::<Option<Vec<i32>>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "INT8[]" => row
            .try_get::<Option<Vec<i64>>, _>(i)
            .ok()
            .flatten()
            .map(exact_int64_list) // same exact-digits rule, per element (SPEC §data.browse)
            .unwrap_or(Value::Null),
        "FLOAT4[]" | "FLOAT8[]" => row
            .try_get::<Option<Vec<f64>>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "BOOL[]" => row
            .try_get::<Option<Vec<bool>>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        "JSON[]" => row
            .try_get::<Option<Vec<Value>>, _>(i)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        // UUID/INET/custom types and anything not modeled. Unchecked so a TEXT-format value
        // (the simple protocol pg_query runs on) still reads as its literal text, the way
        // node-pg's passthrough did.
        _ => row
            .try_get_unchecked::<Option<String>, usize>(i)
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null),
    }
}

pub fn pg_row_to_value(row: &PgRow) -> Value {
    let mut map = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        // Nulls stay IN the row ("col": null): the grid, console and exports carried explicit
        // nulls in the Node build, and only the MCP query tools prune — drop_null_columns at the
        // tool layer removes a column that is null in EVERY row, exactly as node-pg + Node did.
        map.insert(col.name().to_string(), column_to_value(row, col, i));
    }
    Value::Object(map)
}

/// One statement's outcome, grouped the way `summarize` reported each pg result.
pub struct PgGroup {
    pub command: String,
    pub rows: Vec<Map<String, Value>>,
    /// From the CommandComplete tag; for a SELECT this equals the row count.
    pub rows_affected: Option<u64>,
}

/// Split a possibly multi-statement string into its statements, on MASKED semicolons only, and
/// report each statement's leading word — node-pg's `command` was the first word of the
/// completion tag, which matches the statement's own first word in every shape this labels,
/// except a CTE header: `WITH x AS (…) SELECT` tags SELECT (the MAIN statement's verb).
fn split_statement_labels(sql: &str) -> Vec<String> {
    let masked = super::sql::mask_statement(sql);
    let labels = masked
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let label: String = s
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .to_ascii_uppercase();
            if label == "WITH" {
                if let Some(verb) = cte_main_verb(s) {
                    return verb.to_ascii_uppercase();
                }
            }
            label
        })
        .collect::<Vec<_>>();
    labels
}

/// The main statement's verb in a CTE header: the first data verb at parenthesis depth 0 after
/// the CTE list's closing parenthesis. (The "last verb in the text" heuristic mislabels
/// `WITH … INSERT INTO u SELECT *` — INSERT is the head there; the trailing SELECT only feeds
/// it. The statement arrives MASKED, so parens inside literals cannot confuse the depth count.)
fn cte_main_verb(stmt: &str) -> Option<&'static str> {
    const VERBS: [&str; 5] = ["select", "insert", "update", "delete", "merge"];
    let bytes = stmt.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut depth = 0usize;
    let mut cte_closed = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'(' {
            depth += 1;
            continue;
        }
        if b == b')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                cte_closed = true;
            }
            continue;
        }
        if depth != 0 || !cte_closed {
            continue;
        }
        if i > 0 && is_word(bytes[i - 1]) {
            continue; // mid-word, not a verb head
        }
        for verb in VERBS {
            let end = i + verb.len();
            if bytes.len() >= end
                && bytes[i..end].eq_ignore_ascii_case(verb.as_bytes())
                && (end >= bytes.len() || !is_word(bytes[end]))
            {
                return Some(verb);
            }
        }
    }
    None
}

/// Run one (possibly multi-statement) string over the simple protocol — the port of
/// `pool.query(prepared.sql)`. Every statement completion yields its own query result, so
/// groups map one-to-one onto statements and the Node build's array-of-results shape survives.
///
/// The end-of-stream verdict for the trailing open group: the simple protocol closes EVERY
/// statement with a completion (the Left in the loop). Rows sitting in an open group at stream
/// end therefore mean the connection was cut mid-statement — a dropped tunnel forward, a killed
/// server. Shipping those rows as a completed group would be a success-shaped truncation
/// ("here is your data" minus the tail that never arrived); Node's pg driver errored here, and
/// so does this. An open group with NOTHING is the normal end of a healthy stream.
fn open_group_verdict(open_rows: usize) -> Result<(), String> {
    if open_rows == 0 {
        Ok(())
    } else {
        Err(format!(
            "connection lost mid-query ({} row(s) of the last statement arrived without \
             its completion) — the server or the tunnel dropped the connection, retry the query",
            open_rows
        ))
    }
}

pub async fn run_pg_statements(pool: &PgPool, sql: &str) -> Result<Vec<PgGroup>, String> {
    use futures_core::Stream;
    use std::future::poll_fn;
    use std::task::Poll;

    let labels = split_statement_labels(sql);
    let mut stream = pin!(sqlx::raw_sql(AssertSqlSafe(sql)).fetch_many(pool));
    let mut groups: Vec<PgGroup> = vec![PgGroup {
        command: String::new(),
        rows: Vec::new(),
        rows_affected: None,
    }];
    let outcome = poll_fn(|cx| loop {
        match stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(Either::Right(row)))) => {
                if let Value::Object(map) = pg_row_to_value(&row) {
                    groups
                        .last_mut()
                        .expect("always one open group")
                        .rows
                        .push(map);
                }
            }
            Poll::Ready(Some(Ok(Either::Left(done)))) => {
                let group = groups.last_mut().expect("always one open group");
                group.rows_affected = Some(done.rows_affected());
                groups.push(PgGroup {
                    command: String::new(),
                    rows: Vec::new(),
                    rows_affected: None,
                });
            }
            Poll::Ready(Some(Err(err))) => return Poll::Ready(Err(err.to_string())),
            Poll::Ready(None) => {
                return Poll::Ready(open_group_verdict(
                    groups.last().expect("always one open group").rows.len(),
                ));
            }
            Poll::Pending => return Poll::Pending,
        }
    })
    .await;
    outcome?;

    // The loop above opens one trailing group that never completed. The Ready(None) arm
    // already refuses an open group that collected rows (a cut connection), so what is left
    // here is the normal empty tail of a healthy stream — drop it.
    if groups
        .last()
        .map(|g| g.rows.is_empty() && g.rows_affected.is_none())
        .unwrap_or(false)
    {
        groups.pop();
    }
    // Label each completed group with its statement's first word.
    for (group, label) in groups.iter_mut().zip(labels) {
        group.command = label;
    }
    Ok(groups)
}

/// The single-result reply shape: what ran, how many rows it touched, and the rows themselves.
fn summarize(group: &PgGroup) -> Value {
    let row_count = if group.rows.is_empty() {
        group.rows_affected.unwrap_or(0)
    } else {
        group.rows.len() as u64
    };
    let rows = drop_null_columns(group.rows.iter().cloned().map(Value::Object).collect());
    json!({ "command": group.command, "rowCount": row_count, "rows": rows })
}

/// Bind a JSON value onto a parameterized pg query.
fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        Value::String(s) => query.bind(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else if let Some(u) = n.as_u64() {
                query.bind(u as i64)
            } else {
                query.bind(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::Bool(b) => query.bind(*b),
        Value::Null => query.bind(Option::<String>::None),
        other => query.bind(other.to_string()),
    }
}

/// Run one built statement inside an open transaction, returning its affected-row count — the
/// per-edit reply of the Data view's buffered-edit commit.
pub async fn run_pg_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    params: &[Value],
) -> Result<u64, String> {
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for p in params {
        query = bind_value(query, p);
    }
    let result = query.execute(&mut **tx).await.map_err(|e| e.to_string())?;
    Ok(result.rows_affected())
}

/// SPEC §data.edits: the rows-returning sibling of run_pg_tx — an INSERT..RETURNING or the
/// same-transaction read-back SELECT needs the committed row itself, not a count.
pub async fn run_pg_tx_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for p in params {
        query = bind_value(query, p);
    }
    let rows = query
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(pg_row_to_value)
        .filter_map(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect())
}

/// Run one PARAMETERIZED statement (extended protocol — Postgres itself refuses a second
/// statement at Parse) and return its rows.
pub async fn pg_query_rows(
    pool: &PgPool,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for p in params {
        query = bind_value(query, p);
    }
    let rows = query.fetch_all(pool).await.map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(pg_row_to_value)
        .filter_map(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect())
}

// --- the engine -----------------------------------------------------------------------------------

pub struct PgEngine {
    def: ServerDef,
    name: Arc<std::sync::RwLock<String>>,
    url: String,
    database: Option<String>,
    conn: Arc<Lazy<PgPool>>,
}

impl PgEngine {
    pub fn new(def: &ServerDef, name: &str) -> Self {
        let url = def.get_str("url").unwrap_or("").to_string();
        let database = parse_pg_url(&url)
            .map(|(_, _, db)| db)
            .filter(|db| !db.is_empty());
        let def = def.clone();
        let connect_url = url.clone();
        let conn = Lazy::new(move || {
            let opts = pg_connect_options(&connect_url);
            Box::pin(async move {
                let opts = opts?;
                // max 4, connectionTimeoutMillis 5000, idleTimeoutMillis 60000 — idle longer
                // than the 15s health-probe interval, or every probe forks a backend that
                // times out again. query_timeout (20s in Node) has no sqlx knob; the
                // server-side statement_timeout covers the runaway case.
                let pool = PgPoolOptions::new()
                    .max_connections(4)
                    .acquire_timeout(std::time::Duration::from_secs(5))
                    .idle_timeout(std::time::Duration::from_secs(60))
                    .connect_with(opts)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pool)
            }) as BoxFut<Result<PgPool, String>>
        });
        Self {
            def,
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            url,
            database,
            conn: Arc::new(conn),
        }
    }

    /// The database this endpoint talks to, with the password stripped.
    fn target(&self) -> String {
        match parse_pg_url(&self.url) {
            Some((host, port, _)) => format!(
                "{} @ {}:{}",
                self.database.as_deref().unwrap_or("postgres"),
                host,
                port
            ),
            None => "postgres".into(),
        }
    }

    fn max_rows(&self) -> i64 {
        clamp_row_limit(self.def.get("maxRows"), DEFAULT_ROW_LIMIT)
    }

    async fn call_query(&self, args: &Value) -> Result<Value, String> {
        let sql = args
            .get("sql")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if sql.is_empty() {
            return Err("sql is required".into());
        }
        let prepared = with_row_limit(&sql, clamp_row_limit(args.get("limit"), self.max_rows()));
        let pool = self.conn.get().await?;
        let groups = run_pg_statements(&pool, &prepared.sql).await?;
        if groups.len() != 1 {
            // Several results: nothing to annotate (the Node build returned the array as-is).
            return Ok(Value::Array(groups.iter().map(summarize).collect()));
        }
        let group = &groups[0];
        let row_count = if group.rows.is_empty() {
            group.rows_affected.unwrap_or(0)
        } else {
            group.rows.len() as u64
        };
        let mut out = match summarize(group) {
            Value::Object(map) => map,
            other => return Ok(other),
        };
        let report = limit_report(&prepared, row_count as i64, args.get("limit"));
        if let Value::Object(extra) = report {
            for (k, v) in extra {
                out.insert(k, v);
            }
        }
        Ok(Value::Object(out))
    }

    async fn call_list_tables(&self, args: &Value) -> Result<Value, String> {
        let paging = table_page_args(args.get("limit"), args.get("page"));
        // The tool's schema argument narrows the walk (Node's pg.ts did the same — the port had
        // silently dropped it); null lists every non-system schema.
        let filters = pg_browse_table_params(
            args.get("schema").and_then(Value::as_str),
            args.get("grep").and_then(Value::as_str),
        );
        let list_params = vec![
            filters[0].clone(),
            filters[1].clone(),
            json!(paging.limit),
            json!(paging.offset),
        ];
        let pool = self.conn.get().await?;
        let (list, count) = tokio::join!(
            pg_query_rows(&pool, LIST_TABLES_SQL, &list_params),
            pg_query_rows(&pool, COUNT_TABLES_SQL, &filters)
        );
        let list = list?;
        let count = count?;
        let total = count
            .first()
            .and_then(|r| r.get("total"))
            .and_then(|v| match v {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            })
            .unwrap_or(0);
        let listed = list.len() as i64;
        Ok(json!({
            // rows pass through with node-pg types intact: approx_rows is an exact string or
            // null (never analyzed), size is pg_size_pretty's own text.
            "tables": list.into_iter().map(Value::Object).collect::<Vec<_>>(),
            "total": total,
            "page": paging.page,
            "limit": paging.limit,
            "more": paging.offset + listed < total,
        }))
    }

    async fn call_describe_table(&self, args: &Value) -> Result<Value, String> {
        let table = args
            .get("table")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if table.is_empty() {
            return Err("table is required".into());
        }
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let pool = self.conn.get().await?;
        let params = [schema.map_or(Value::Null, |s| json!(s)), json!(table)];
        let rows = pg_query_rows(&pool, DESCRIBE_TABLE_SQL, &params).await?;
        match rows.into_iter().next().and_then(|mut r| r.remove("doc")) {
            Some(doc @ Value::Object(_)) => Ok(doc),
            _ => Err(describe_miss(schema, &table)),
        }
    }

    async fn call_inspect(&self, args: &Value) -> Result<Value, String> {
        let (check, limit) = inspect_args(args, PG_CHECKS)?;
        let failed = |e: String| inspect_failed(check.name, e);
        let pool = self.conn.get().await?;
        let rows = match check.rows {
            Some(sql) => {
                let mut sql = sql.to_string();
                if sql.contains("{pgss}") {
                    let found = pg_query_rows(&pool, PGSS_SCHEMA_SQL, &[])
                        .await
                        .map_err(failed)?;
                    let schema = found
                        .first()
                        .and_then(|r| r.get("schema"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| PGSS_MISSING.to_string())?;
                    sql = sql.replace("{pgss}", &quote_ident(DbDialect::Pg, schema)?);
                }
                Some(pg_diagnostic_rows(&pool, &sql, &[json!(limit)]).await.map_err(failed)?)
            }
            None => None,
        };
        let summary = match check.summary {
            Some(sql) => pg_diagnostic_rows(&pool, sql, &[])
                .await
                .map_err(failed)?
                .into_iter()
                .next(),
            None => None,
        };
        Ok(inspect_reply(check, rows, summary))
    }
}

/// A check that failed, named — with the grant that fixes the usual cause.
fn inspect_failed(check: &str, e: String) -> String {
    if e.contains("permission denied") {
        format!(
            "{check} failed: {e} — grant this login pg_monitor (or pg_read_all_stats) to read \
             the statistics views"
        )
    } else {
        format!("{check} failed: {e}")
    }
}

/// pg_inspect's rows: pg_query_rows, except that an INT8 or NUMERIC column comes back as a JSON
/// number — there it is a counter or a sum, not an id, and a model compares numbers, not
/// strings.
async fn pg_diagnostic_rows(
    pool: &PgPool,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for p in params {
        query = bind_value(query, p);
    }
    let rows = query.fetch_all(pool).await.map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|row| {
            row.columns()
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    let value = match col.type_info().to_string().as_str() {
                        "INT8" => row
                            .try_get::<Option<i64>, _>(i)
                            .ok()
                            .flatten()
                            .map_or(Value::Null, Value::from),
                        "NUMERIC" => row
                            .try_get::<Option<bigdecimal::BigDecimal>, _>(i)
                            .ok()
                            .flatten()
                            .and_then(|d| d.to_string().parse::<f64>().ok())
                            .map_or(Value::Null, finite_f64),
                        _ => column_to_value(row, col, i),
                    };
                    (col.name().to_string(), value)
                })
                .collect()
        })
        .collect())
}

#[async_trait]
impl Engine for PgEngine {
    fn kind(&self) -> &'static str {
        "pg"
    }

    fn tools(&self) -> Vec<ToolDef> {
        tools(self.max_rows())
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "pg_query" => self.call_query(args).await,
            "pg_list_tables" => self.call_list_tables(args).await,
            "pg_describe_table" => self.call_describe_table(args).await,
            "pg_inspect" => self.call_inspect(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn meta(&self) -> ServerMeta {
        let name = self.name.read().ok().map(|g| g.clone());
        ServerMeta {
            name,
            description: self.def.get_str("description").map(str::to_string),
            target: Some(self.target()),
            limits: None,
        }
    }

    fn resources(&self) -> Option<Arc<dyn super::resources::ResourceProvider>> {
        let database = self.database.clone()?;
        Some(Arc::new(PgResources::new(database, self.conn.clone())))
    }

    fn browser(&self) -> Option<swiss_host::dbbrowser::BrowserFlavor> {
        Some(swiss_host::dbbrowser::BrowserFlavor::Db(Arc::new(
            super::pg_browser::PgBrowser::new(self.target(), self.conn.clone()),
        )))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = async {
            let pool = self.conn.get().await?;
            let rows = pg_query_rows(&pool, "SELECT 1 AS ok", &[]).await?;
            let ok = rows
                .first()
                .and_then(|r| r.get("ok"))
                .and_then(Value::as_i64);
            if ok != Some(1) {
                return Err("pg SELECT 1 returned no row".into());
            }
            Ok(())
        }
        .await;
        Some(result)
    }

    async fn close(&self) {
        self.conn
            .dispose(|pool| Box::pin(async move { pool.close().await }) as BoxFut<()>)
            .await;
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_set_is_query_list_describe_inspect() {
        let names: Vec<String> = tools(500).into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            ["pg_query", "pg_list_tables", "pg_describe_table", "pg_inspect"]
        );
        let inspect = tools(500).pop().expect("pg_inspect");
        assert_eq!(
            inspect.input_schema["properties"]["check"]["enum"],
            json!([
                "activity",
                "locks",
                "top_queries",
                "table_scans",
                "vacuum",
                "indexes",
                "cache",
                "connections"
            ])
        );
    }

    #[test]
    fn every_check_binds_only_its_row_limit() {
        // call_inspect binds exactly [limit] to a row query and nothing to a summary.
        for check in PG_CHECKS {
            let rows = check.rows.expect("every pg check lists rows");
            assert!(rows.trim_end().ends_with("LIMIT $1"), "{}", check.name);
            assert!(!rows.contains("$2"), "{}", check.name);
            if let Some(summary) = check.summary {
                assert!(!summary.contains('$'), "{}", check.name);
            }
        }
        let pgss: Vec<&str> = PG_CHECKS
            .iter()
            .filter(|c| c.rows.is_some_and(|r| r.contains("{pgss}")))
            .map(|c| c.name)
            .collect();
        assert_eq!(pgss, ["top_queries"]);
    }

    #[test]
    fn a_describe_miss_points_at_the_next_call() {
        assert_eq!(
            describe_miss(Some("app"), "users"),
            "no table or view app.users — pg_list_tables lists what exists"
        );
        let dotted = describe_miss(None, "app.users");
        assert!(dotted.contains("schema \"app\", table \"users\""), "{dotted}");
        let bare = describe_miss(None, "users");
        assert!(bare.contains("on the search_path"), "{bare}");
        assert!(bare.contains("pg_list_tables"), "{bare}");
    }

    #[test]
    fn describe_resolves_through_to_regclass_with_both_names_quoted() {
        assert!(DESCRIBE_TABLE_SQL.contains("format('%I.%I', $1::text, $2::text)"));
        assert!(DESCRIBE_TABLE_SQL.contains("format('%I', $2::text)"));
        assert!(!DESCRIBE_TABLE_SQL.contains("$3"));
    }

    #[test]
    fn a_permission_error_names_the_grant() {
        let e = inspect_failed("activity", "permission denied for view x".into());
        assert!(e.starts_with("activity failed: permission denied"), "{e}");
        assert!(e.contains("pg_monitor"), "{e}");
        assert_eq!(inspect_failed("locks", "boom".into()), "locks failed: boom");
    }

    #[test]
    fn browse_table_params_carry_the_schema_pick() {
        // SPEC §data.browse: the schema picker narrows the walk to one schema; the slot stays bound
        // (null) when the panel asks for every schema, and an empty string means the same as
        // absent — the panel never sends one, a hand-written URL might.
        assert_eq!(
            pg_browse_table_params(None, None),
            vec![Value::Null, Value::Null]
        );
        assert_eq!(
            pg_browse_table_params(Some("app"), Some("us")),
            vec![json!("app"), json!("%us%")]
        );
        assert_eq!(
            pg_browse_table_params(Some(""), None),
            vec![Value::Null, Value::Null]
        );
    }

    #[test]
    fn grammar_params_give_the_count_its_schema_bind() {
        // Regression: the inline assembly sent the count with the patterns alone, and Postgres
        // refused it with "bind message supplies 2 parameters, but prepared statement
        // requires 3" — the schema slot is a placeholder whether or not a schema was picked.
        let (list, count) = pg_grammar_params(None, &[json!("user%"), json!("%account%")], 200, 0);
        assert_eq!(list.len(), 5); // schema, two patterns, limit, offset
        assert_eq!(count.len(), 3);
        assert_eq!(count[0], Value::Null);
        let (list, count) = pg_grammar_params(Some("app"), &[json!("user%")], 200, 0);
        assert_eq!(
            list,
            vec![json!("app"), json!("user%"), json!(200), json!(0)]
        );
        assert_eq!(count, vec![json!("app"), json!("user%")]);
    }

    #[test]
    fn grammar_grep_sql_renumbers_the_paging_binds() {
        // SPEC §data.browse: two patterns push LIMIT/OFFSET from $3/$4 to $4/$5, the schema keeps $1,
        // and the count shares the predicate without the paging pair.
        let w = swiss_host::dbbrowser::grep_where(
            swiss_host::dbbrowser::DbDialect::Pg,
            "c.relname",
            "user*|account",
            1,
        )
        .unwrap()
        .expect("grammar");
        let (list, count) = pg_list_tables_grammar_sql(
            TableSort {
                key: TableSortKey::Name,
                desc: false,
            },
            &w.frag,
            w.params.len(),
        );
        assert!(
            list.contains("AND (c.relname ILIKE $2 ESCAPE '!' OR c.relname ILIKE $3 ESCAPE '!')"),
            "{list}"
        );
        assert!(list.contains("LIMIT $4 OFFSET $5"), "{list}");
        assert!(
            list.contains("ORDER BY n.nspname, lower(c.relname)"),
            "{list}"
        );
        assert!(
            count.contains("AND (c.relname ILIKE $2 ESCAPE '!' OR c.relname ILIKE $3 ESCAPE '!')"),
            "{count}"
        );
        assert!(!count.contains("LIMIT"), "{count}");
    }

    #[test]
    fn url_parsing_matches_new_url() {
        let (host, port, db) =
            parse_pg_url("postgres://u:p@localhost:5433/mydb?sslmode=disable").unwrap();
        assert_eq!(
            (host.as_str(), port, db.as_str()),
            ("localhost", 5433, "mydb")
        );
        let (host, port, db) = parse_pg_url("postgresql://app@db.example.com/prod").unwrap();
        assert_eq!(
            (host.as_str(), port, db.as_str()),
            ("db.example.com", 5432, "prod")
        );
        // No path → empty database (resources capability stays off, as in Node).
        let (host, _, db) = parse_pg_url("postgres://localhost").unwrap();
        assert_eq!((host.as_str(), db.as_str()), ("localhost", ""));
        assert!(parse_pg_url("not a url").is_none());
        assert!(parse_pg_url("mysql://localhost/db").is_none());
    }

    #[test]
    fn statement_labels() {
        assert_eq!(split_statement_labels("select 1"), vec!["SELECT"]);
        assert_eq!(
            split_statement_labels("UPDATE t SET x=1; select 'a;b'"),
            vec!["UPDATE", "SELECT"]
        );
        // node-pg's command was the completion tag's word — the MAIN statement's verb, so a
        // CTE header labels as its reader, not as WITH.
        assert_eq!(
            split_statement_labels("with x as (select 1) select * from x"),
            vec!["SELECT"]
        );
        assert_eq!(
            split_statement_labels(
                "WITH moved AS (DELETE FROM t RETURNING *) INSERT INTO u SELECT * FROM moved"
            ),
            vec!["INSERT"]
        );
    }
}
#[test]
fn rows_in_an_open_group_at_stream_end_are_a_cut_not_a_result() {
    // Every statement closes with a completion; rows without one are the tail of a
    // connection that died mid-query. They must surface as an error, never as a
    // completed (silently short) group.
    assert!(open_group_verdict(0).is_ok());
    let cut = open_group_verdict(7).expect_err("cut mid-statement");
    assert!(cut.contains("connection lost mid-query"), "{cut}");
    assert!(cut.contains("7 row(s)"), "{cut}");
    assert!(cut.contains("retry"), "{cut}");
}
