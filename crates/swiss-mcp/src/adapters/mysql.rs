//! The in-process MySQL adapter — port of `adapters/mysql.ts`. A small sqlx pool behind one
//! query tool plus a table listing, schema-as-resources scoped to the configured database.

use std::pin::pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sqlx::mysql::{MySqlColumn, MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Either, Executor, Row};

use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{TableSort, TableSortKey};

use super::direct::{BoxFut, Lazy};
use super::mysql_browser::MysqlBrowser;
use super::mysql_resources::MysqlResources;
use super::resources::human_bytes;
use super::sql::{
    assert_single_statement, clamp_row_limit, drop_null_columns, like_contains,
    limit_report, table_page_args, with_row_limit, DEFAULT_ROW_LIMIT, DEFAULT_TABLE_LIMIT,
    MAX_ROW_LIMIT, MAX_TABLE_LIMIT,
};
use super::tool_server::{Engine, ServerMeta, ToolDef};

fn grep_arg() -> Value {
    json!({
        "type": "string",
        "description": concat!(
            "Keep only tables whose name contains this substring (case-insensitive): \"users\" lists p_users, ",
            "users_settings, … Optional — omit to list everything.",
        ),
    })
}
fn limit_arg() -> Value {
    json!({
        "type": "number",
        "description": format!("Max tables per page (default {DEFAULT_TABLE_LIMIT}, max {MAX_TABLE_LIMIT})."),
    })
}
fn page_arg() -> Value {
    json!({
        "type": "number",
        "description": "0-based page index through the filtered list (default 0) — the reply's total/more say what is left.",
    })
}

/// Two tools: the query tool (matching the single-tool MySQL MCP this replaced), and a table
/// listing with an optional name filter.
///
/// SHOW TABLES exists, but it answers names only — no sizes, no row estimates, no filtering — so
/// on an instance with thousands of tables the model shipped the whole list and grepped it
/// client-side. mysql_list_tables is that one call done properly: sizes and row estimates beside
/// the names, and a server-side grep. DESCRIBE t, SHOW INDEX FROM t and EXPLAIN stay un-wrapped,
/// unlike Postgres's describe tool: they are one-liners every model already knows, and each
/// wrapper schema is re-sent on every request.
///
/// Targets MySQL 8.0+.
fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "mysql_query".into(),
            description: format!(
                "Run a SQL statement (SELECT / INSERT / UPDATE / DELETE / DDL, plus SHOW / DESCRIBE / EXPLAIN for \
                 schema and plans). Returns {{ rowCount, rows }} for a SELECT, or {{ affectedRows, insertId, \
                 changedRows }} for DML/DDL — affectedRows counts MATCHED rows on UPDATE, and changedRows is \
                 always 0 in this build. A SELECT written without its own LIMIT is capped at \
                 {DEFAULT_ROW_LIMIT} rows and says so in the reply — raise `limit` or write your own \
                 LIMIT/OFFSET for more. NULL columns are omitted from each row (use DESCRIBE for the full \
                 column list), so on a wide table name the columns you need rather than SELECT *."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": { "type": "string", "description": "The SQL statement to execute." },
                    "limit": { "type": "number", "description": format!("Row cap for a LIMIT-less SELECT (default {DEFAULT_ROW_LIMIT}, max {MAX_ROW_LIMIT}).") },
                },
                "required": ["sql"],
            }),
        },
        ToolDef {
            name: "mysql_list_tables".into(),
            description: concat!(
                "List the connected database's tables and views with their approximate row count and on-disk ",
                "size — use this before querying, to know what exists and what is big enough to need a LIMIT. ",
                "Paged: limit per page (default 200, max 1000) and 0-based page; the reply carries total and ",
                "more. Pass `grep` to keep only names containing it: grep \"users\" lists p_users, users_settings, …"
            )
            .to_string(),
            input_schema: json!({ "type": "object", "properties": { "grep": grep_arg(), "limit": limit_arg(), "page": page_arg() } }),
        },
    ]
}

/// `col` (+ direction), with the name as an ascending tiebreaker so pages never shuffle equals.
fn keyed_order(col: &str, desc: bool) -> String {
    if desc { format!("{col} DESC, table_name") } else { format!("{col}, table_name") }
}

/// The `{ list, count }` pair behind mysql_list_tables, built as a pure function so the filtering
/// and paging logic is testable without a live MySQL. count shares the list's WHERE so a page can
/// report the filtered total beside the rows.
///
/// Row counts and sizes come from the statistics snapshot information_schema serves — cached up
/// to information_schema_stats_expiry (a day by default), the same caveat as mysql_resources.
/// The grep filter and the paging are applied SERVER-side: with thousands of tables on an
/// instance, shipping the whole list to drop most of it client-side is exactly what grep and
/// pages exist to avoid. LIKE under MySQL's default _ci collation is case-insensitive, matching
/// Postgres's ILIKE.
pub fn mysql_list_tables_sql(
    database: &str,
    grep: Option<&str>,
    paging: (i64, i64, i64), // (page, limit, offset)
    sort: Option<TableSort>,
) -> ((String, Vec<Value>), (String, Vec<Value>)) {
    let pattern = grep.map(like_contains);
    let (where_clause, head): (&str, Vec<Value>) = match &pattern {
        Some(p) => (
            "table_schema = ? AND table_name LIKE ? ESCAPE '!'",
            vec![json!(database), json!(p)],
        ),
        None => ("table_schema = ?", vec![json!(database)]),
    };
    mysql_tables_sql_pair(where_clause, &head, mysql_list_order(sort), paging)
}

/// The grammar-grep variant (docs/22 W1.6): `pred` is the multi-LIKE fragment grep_where built
/// (" AND (table_name LIKE ? ESCAPE '!' OR ...)") and `patterns` its bound values, slotted after
/// the database bind. A grep without the grammar characters never reaches here — it keeps
/// mysql_list_tables_sql's single-LIKE SQL byte-for-byte.
pub fn mysql_list_tables_grammar_sql(
    database: &str,
    pred: &str,
    patterns: &[Value],
    paging: (i64, i64, i64),
    sort: Option<TableSort>,
) -> ((String, Vec<Value>), (String, Vec<Value>)) {
    let where_clause = format!("table_schema = ?{pred}");
    let mut head = vec![json!(database)];
    head.extend(patterns.iter().cloned());
    mysql_tables_sql_pair(&where_clause, &head, mysql_list_order(sort), paging)
}

/// Node's mysqlListOrder: the name sort keeps information_schema's own (case-insensitive)
/// collation; rows/size sort by the statistics the SELECT already computes, with the name as
/// an ascending tiebreaker — DESC applies to the chosen key only, never the tiebreaker.
fn mysql_list_order(sort: Option<TableSort>) -> String {
    match sort {
        Some(TableSort { key: TableSortKey::Rows, desc }) => keyed_order("approx_rows", desc),
        Some(TableSort { key: TableSortKey::Size, desc }) => keyed_order("bytes", desc),
        Some(TableSort { key: TableSortKey::Name, desc: false }) | None => "table_name".into(),
        Some(TableSort { key: TableSortKey::Name, desc: true }) => "table_name DESC".into(),
    }
}

/// The list/count pair around one WHERE: count shares the list's WHERE so a page can report
/// the filtered total beside the rows; the paging pair binds last.
fn mysql_tables_sql_pair(
    where_clause: &str,
    head: &[Value],
    order: String,
    paging: (i64, i64, i64),
) -> ((String, Vec<Value>), (String, Vec<Value>)) {
    let list_sql = format!(
        "
      SELECT table_name AS name,
            CASE table_type WHEN 'BASE TABLE' THEN 'table' WHEN 'VIEW' THEN 'view'
                            ELSE LOWER(table_type) END AS type,
            table_rows AS approx_rows,
            COALESCE(data_length, 0) + COALESCE(index_length, 0) AS bytes
        FROM information_schema.tables
       WHERE {where_clause}
       ORDER BY {order}
       LIMIT ? OFFSET ?"
    );
    let count_sql = format!(
        "
      SELECT COUNT(*) AS total
        FROM information_schema.tables
       WHERE {where_clause}"
    );
    let (_, limit, offset) = paging;
    let mut list_params = head.to_vec();
    list_params.push(json!(limit));
    list_params.push(json!(offset));
    ((list_sql, list_params), (count_sql, head.to_vec()))
}

/// Columns of one table for the Data view — the shape toBrowseColumns reads. Every column is
/// aliased to its lowercase name: MySQL 8 reports information_schema columns back in UPPERCASE
/// when the select list is unaliased. column_comment rides along so the panel can say what a
/// field means, not just its type.
pub const MYSQL_BROWSE_COLUMNS_SQL: &str = "
  SELECT column_name AS column_name, data_type AS data_type,
         is_nullable AS is_nullable, column_default AS column_default,
         column_comment AS column_comment
    FROM information_schema.columns
   WHERE table_schema = ? AND table_name = ?
   ORDER BY ordinal_position";

/// Indexes, one row per indexed column (aliased: MySQL uppercases unaliased catalog columns).
/// PRIMARY is folded in from statistics rather than a second query.
pub const MYSQL_BROWSE_INDEXES_SQL: &str = "
  SELECT index_name AS name, CAST(non_unique AS SIGNED) AS unique0,
         0 AS is_primary, column_name AS col
    FROM information_schema.statistics
   WHERE table_schema = ? AND table_name = ?
   ORDER BY index_name, seq_in_index";

/// Foreign keys: one row per column of each referencing constraint.
pub const MYSQL_BROWSE_FK_SQL: &str = "
  SELECT kcu.constraint_name AS name, kcu.column_name AS col,
         kcu.referenced_table_schema AS ref_schema, kcu.referenced_table_name AS ref_table,
         kcu.referenced_column_name AS ref_column
    FROM information_schema.key_column_usage kcu
   WHERE kcu.table_schema = ? AND kcu.table_name = ?
     AND kcu.referenced_table_name IS NOT NULL
   ORDER BY kcu.constraint_name, kcu.ordinal_position";

/// The PRIMARY KEY's columns, in key order — the address an edit's WHERE clause speaks.
pub const MYSQL_BROWSE_PK_SQL: &str = "
  SELECT column_name AS column_name
    FROM information_schema.key_column_usage
   WHERE table_schema = ? AND table_name = ? AND constraint_name = 'PRIMARY'
   ORDER BY ordinal_position";

/// The connect options every MySQL connection shares — port of `mysqlPoolOptions` minus what a
/// pool is instead (connectionLimit/queueLimit/connectTimeout map onto the pool options below).
/// Pure, so the defaults have a regression test.
pub fn mysql_connect_options(def: &ServerDef) -> MySqlConnectOptions {
    let mut opts = MySqlConnectOptions::new();
    opts = opts.host(def.get_str("host").unwrap_or("localhost"));
    if let Some(port) = def.get_number("port") {
        if port > 0.0 {
            opts = opts.port(port as u16);
        }
    }
    if !def.get_str("user").unwrap_or("").is_empty() {
        opts = opts.username(def.get_str("user").unwrap_or(""));
    }
    if let Some(password) = def.get_str("password") {
        opts = opts.password(password);
    }
    if let Some(database) = def.get_str("database") {
        if !database.is_empty() {
            opts = opts.database(database);
        }
    }
    opts
}

/// The per-connection session statements the pool hands every new connection.
pub fn mysql_session_sql() -> Vec<&'static str> {
    // `max_execution_time` caps runaway SELECTs server-side. Nothing here makes the session
    // read-only: the console and the grid are the operator's own tools on the operator's own
    // databases.
    vec!["SET SESSION max_execution_time = 15000"]
}

// --- rows → JSON ----------------------------------------------------------------------------------

/// Bind a JSON value onto a dynamic query. MySQL coerces a numeric string back against the
/// column on bind, so edits and filters keep working with the exact-string ids above.
fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    value: &Value,
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    match value {
        Value::String(s) => query.bind(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else if let Some(u) = n.as_u64() {
                query.bind(u)
            } else {
                query.bind(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::Bool(b) => query.bind(*b),
        Value::Null => query.bind(Option::<String>::None),
        other => query.bind(other.to_string()),
    }
}

/// One column → one JSON value, dispatched on the type name sqlx prints (the public face of
/// MySqlTypeInfo). The mapping mirrors mysql2 with `dateStrings` + `supportBigNumbers` +
/// `bigNumberStrings`:
/// - DATE/DATETIME/TIMESTAMP/TIME stay strings (no TZ surprises);
/// - BIGINT and DECIMAL become exact strings — 18-19 digit ids (snowflake) are not representable
///   as a JS double, and Number() conversion silently rounds their low digits. Strings are exact
///   end to end, and MySQL coerces a numeric string back against the column on bind;
/// - BINARY/BLOB/GEOMETRY bytes decode as UTF-8 (renderResult normalized Buffers the same way).
fn column_to_value(row: &MySqlRow, col: &MySqlColumn, i: usize) -> Value {
    let type_name = col.type_info().to_string();
    match type_name.as_str() {
        "BIGINT" | "BIGINT UNSIGNED" => {
            // try_get::<i64> refuses an unsigned column (u64-only), so try both spellings.
            if let Ok(Some(v)) = row.try_get::<Option<i64>, _>(i) {
                return json!(v.to_string());
            }
            if let Ok(Some(v)) = row.try_get::<Option<u64>, _>(i) {
                return json!(v.to_string());
            }
            Value::Null
        }
        "DECIMAL" => row
            .try_get::<Option<bigdecimal::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.to_string()))
            .unwrap_or(Value::Null),
        "TINYINT" | "SMALLINT" | "INT" | "MEDIUMINT" | "TINYINT UNSIGNED" | "SMALLINT UNSIGNED"
        | "INT UNSIGNED" | "MEDIUMINT UNSIGNED" | "YEAR" | "BIT" | "BOOLEAN" => {
            if let Ok(Some(v)) = row.try_get::<Option<i64>, _>(i) {
                json!(v)
            } else if let Ok(Some(v)) = row.try_get::<Option<u64>, _>(i) {
                json!(v)
            } else if let Ok(Some(v)) = row.try_get::<Option<bool>, _>(i) {
                json!(v)
            } else {
                Value::Null
            }
        }
        "FLOAT" | "DOUBLE" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(|v| match v {
                f if f.is_finite() => json!(f),
                _ => Value::Null,
            })
            .unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.format("%Y-%m-%d").to_string()))
            .unwrap_or(Value::Null),
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.format("%Y-%m-%d %H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|t| json!(t.format("%H:%M:%S%.f").to_string()))
            // MySQL TIME is also a DURATION: values outside a clock face (`-00:30:00`,
            // `100:00:00`, down to `-838:59:59`) fall outside NaiveTime and decode fails —
            // mysql2's dateStrings handed back the exact wire string for those. Fall through to
            // the string form rather than dropping the cell.
            .or_else(|| {
                row.try_get::<Option<String>, _>(i)
                    .ok()
                    .flatten()
                    .map(Value::String)
            })
            .unwrap_or(Value::Null),
        "JSON" => row
            .try_get::<Option<Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(Value::Null),
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "GEOMETRY" => {
            row.try_get::<Option<Vec<u8>>, _>(i)
                .ok()
                .flatten()
                .map(|b| json!(String::from_utf8_lossy(&b).into_owned()))
                .unwrap_or(Value::Null)
        }
        // CHAR / VARCHAR / TEXT family / ENUM / SET / anything not modeled.
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null),
    }
}

pub fn mysql_row_to_value(row: &MySqlRow) -> Value {
    let mut map = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        // Nulls stay IN the row ("col": null): the grid, console and exports carried explicit
        // nulls in the Node build, and only the MCP query tools prune — drop_null_columns at the
        // tool layer removes a column that is null in EVERY row, exactly as mysql2 + Node did.
        map.insert(col.name().to_string(), column_to_value(row, col, i));
    }
    Value::Object(map)
}

/// What one statement produced: rows of a result set, or the OK packet a non-SELECT answers
/// with. Node's `pool.query` returned exactly one of these two shapes; `fetch_many` yields both.
pub struct QueryOutcome {
    pub rows: Vec<Map<String, Value>>,
    /// Some for an OK packet (DML/DDL — affected/insert id), None for a result set.
    pub affected: Option<(u64, u64)>,
}

/// Run one statement against the pool, capturing whichever of the two shapes comes back.
///
/// No-param statements go over the text protocol (`raw_sql` → COM_QUERY — the one path a
/// smuggled second statement could ride, which `assert_single_statement` already refuses
/// upstream); parameterized statements use the prepared protocol. Both run through the same
/// drain loop — see `collect_outcome` for how the two reply shapes are told apart.
pub async fn run_query(
    pool: &MySqlPool,
    sql: &str,
    params: &[Value],
) -> Result<QueryOutcome, String> {
    if params.is_empty() {
        collect_outcome(sqlx::raw_sql(sql).fetch_many(pool), sql).await
    } else {
        let mut query = sqlx::query(sql);
        for p in params {
            query = bind_value(query, p);
        }
        // fetch_many (deprecated for its multi-statement SQLite semantics) is the only fetch that
        // surfaces the OK-packet Left a DML statement answers with; fetch_all discards it. Single
        // statement only — assert_single_statement already refused anything else upstream.
        #[allow(deprecated)]
        let stream = query.fetch_many(pool);
        collect_outcome(stream, sql).await
    }
}

/// The drain loop over either protocol's `fetch_many` stream.
///
/// sqlx ends EVERY statement with `Either::Left` — an OK packet for DML, but also a
/// `rows_affected: 0` Left as the EOF terminator of a result set (the column-count packet that
/// would distinguish the two is swallowed inside the driver; mysql2 read it, we cannot). So the
/// shape is decided by what was OBSERVED before the first terminator: rows arrived → result
/// set; none arrived → the statement's own shape (`is_row_returning`) decides between an empty
/// result set (`SELECT … WHERE false` → `rows: []`) and a zero-effect OK packet (`UPDATE … WHERE
/// false` → `affectedRows: 0`).
///
/// Everything after the first terminator is drained but discarded: a stored procedure's later
/// result sets never ship (mysql2 returned the first set too), and the stream MUST reach its end
/// anyway — sqlx returns the connection to the pool only after the trailing ReadyForQuery, and
/// breaking early leaves it dirty (five health pings were enough to hang every pool slot).
/// The end-of-stream verdict for one MySQL statement. sqlx ends EVERY statement with the
/// Left terminator (an OK packet, or the rows_affected: 0 EOF of a result set — see the
/// drain-loop notes above). A stream that ends WITHOUT one was cut mid-statement: a dropped
/// tunnel forward, a killed server. mysql2 answered that with PROTOCOL_CONNECTION_LOST;
/// answering `rows: []` here instead would be a success-shaped lie — "the table is empty" —
/// for whatever subset of rows arrived before the cut. That exact lie was seen live against
/// a flapping tunnel: same minute, COUNT(*) on a live pooled connection, `SELECT *` empty
/// on one that had just died.
fn stream_end_verdict(terminated: bool, rows: usize) -> Result<(), String> {
    if terminated {
        Ok(())
    } else {
        Err(format!(
            "connection lost mid-query ({} row(s) arrived; the statement never finished) — \
             the server or the tunnel dropped the connection, retry the query",
            rows
        ))
    }
}

async fn collect_outcome<S>(stream: S, sql: &str) -> Result<QueryOutcome, String>
where
    S: futures_core::Stream<
        Item = Result<Either<sqlx::mysql::MySqlQueryResult, MySqlRow>, sqlx::Error>,
    >,
{
    use std::future::poll_fn;
    use std::task::Poll;

    let mut stream = pin!(stream);
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    let mut affected: Option<(u64, u64)> = None;
    let mut terminated = false; // the first statement's outcome is decided; later sets are dropped
    let outcome = poll_fn(|cx| loop {
        match stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(Either::Right(row)))) => {
                if !terminated {
                    if let Value::Object(map) = mysql_row_to_value(&row) {
                        rows.push(map);
                    }
                }
            }
            Poll::Ready(Some(Ok(Either::Left(done)))) => {
                if !terminated {
                    terminated = true;
                    affected = if rows.is_empty() && !super::sql::is_row_returning(sql) {
                        Some((done.rows_affected(), done.last_insert_id()))
                    } else {
                        None
                    };
                }
            }
            Poll::Ready(Some(Err(err))) => return Poll::Ready(Err(err.to_string())),
            Poll::Ready(None) => {
                // No terminator ever arrived: the connection was cut. Say so instead of
                // shipping whatever subset of the result made it through as the truth.
                return Poll::Ready(stream_end_verdict(terminated, rows.len()));
            }
            Poll::Pending => return Poll::Pending,
        }
    })
    .await;
    match outcome {
        Ok(()) => Ok(QueryOutcome { rows, affected }),
        Err(msg) => Err(msg),
    }
}

/// Run one built statement inside an open transaction, returning its affected-row count — the
/// per-edit reply of the Data view's buffered-edit commit (mysql2's `affectedRows`).
pub async fn run_query_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    sql: &str,
    params: &[Value],
) -> Result<u64, String> {
    let mut query = sqlx::query(sql);
    for p in params {
        query = bind_value(query, p);
    }
    let result = query.execute(&mut **tx).await.map_err(|e| e.to_string())?;
    Ok(result.rows_affected())
}

/// docs/22 W1.7: the rows-returning sibling of run_query_tx — the same-transaction read-back
/// SELECT (by primary key, or by LAST_INSERT_ID() right after an insert) needs the committed
/// row itself, not a count.
pub async fn run_query_tx_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    sql: &str,
    params: &[Value],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut query = sqlx::query(sql);
    for p in params {
        query = bind_value(query, p);
    }
    let rows = query.fetch_all(&mut **tx).await.map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(mysql_row_to_value)
        .filter_map(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect())
}

/// The health verdict on `SELECT 1 AS ok`. A bare integer literal is typed LONGLONG by the
/// server, so with the exact-string mapping the row comes back as `{ ok: "1" }`. Both spellings
/// mean healthy; anything else (or no row at all) is not.
pub fn ping_ok(rows: &[Map<String, Value>]) -> bool {
    match rows.first().and_then(|r| r.get("ok")) {
        Some(Value::String(s)) => s == "1",
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

/// A numeric column value back to a number for arithmetic (BIGINT arrives as an exact string).
pub fn num_or_zero(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().map(|i| i.max(0) as u64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

/// A numeric column as a JSON number, or null when absent (views report no row estimate).
pub fn numeric_or_null(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) => Value::Number(n.clone()),
        Some(Value::String(s)) => s
            .trim()
            .parse::<i64>()
            .map(|i| json!(i))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

// --- the engine -----------------------------------------------------------------------------------

pub struct MysqlEngine {
    def: ServerDef,
    /// The MCP's registry name, followed across renames — read at meta() time so the call log
    /// files under the current name.
    name: Arc<std::sync::RwLock<String>>,
    database: Option<String>,
    /// The one pool cell behind an Arc, shared with the resources provider so one connection
    /// pool serves tools, resources, pings and the panel's Data view.
    conn: Arc<Lazy<MySqlPool>>,
}

impl MysqlEngine {
    /// `def` must be the resolve_def() clone make_adapter hands over.
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let database = def
            .get_str("database")
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        // The database name is interpolated into SHOW CREATE TABLE by the resources provider;
        // vet it here so provider construction can never fail.
        if let Some(db) = &database {
            super::resources::assert_ident(db, "database name").map_err(|f| f.0)?;
        }
        let def = def.clone();
        let connect = mysql_connect_options(&def);
        let conn = Lazy::new(move || {
            let connect = connect.clone();
            Box::pin(async move {
                // connectionLimit 5 — a handful of local clients, not a web app. Node's
                // queueLimit 20 / connectTimeout 5000 map onto one acquire timeout that bounds
                // the backlog instead of growing it without limit.
                let pool = MySqlPoolOptions::new()
                    .max_connections(5)
                    .acquire_timeout(std::time::Duration::from_secs(5))
                    .after_connect(move |conn, _| {
                        let session = mysql_session_sql();
                        Box::pin(async move {
                            // The Node build swallowed a session statement an older server did
                            // not know; a failed SET must not fail the whole connection.
                            for sql in session {
                                let _ = conn.execute(sqlx::query(sql)).await;
                            }
                            Ok(())
                        })
                    })
                    .connect_with(connect)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(pool)
            }) as BoxFut<Result<MySqlPool, String>>
        });
        let conn = Arc::new(conn);
        Ok(Self {
            def,
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            database,
            conn,
        })
    }

    fn target(&self) -> String {
        let where_ = format!(
            "{}:{}",
            self.def.get_str("host").unwrap_or("localhost"),
            self.def
                .get_number("port")
                .map(|p| p as i64)
                .unwrap_or(3306)
        );
        match &self.database {
            Some(db) => format!("{db} @ {where_}"),
            None => where_,
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
        // sqlx negotiates MULTI_STATEMENTS where mysql2 did not — refuse the smuggled second
        // statement before it ever reaches the wire (sql.rs owns the why).
        let sql = assert_single_statement(&sql)?;
        let prepared = with_row_limit(&sql, clamp_row_limit(args.get("limit"), self.max_rows()));
        let pool = self.conn.get().await?;
        let outcome = run_query(&pool, &prepared.sql, &[]).await?;
        if let Some((affected, insert_id)) = outcome.affected {
            // OK-packet shape: DML/DDL. sqlx requests CLIENT_FOUND_ROWS, so affectedRows counts
            // MATCHED rows on UPDATE (mysql2 without the flag counted changed rows); changedRows
            // and info are not surfaced by the Rust driver and report their empty values.
            return Ok(json!({
                "affectedRows": affected,
                "insertId": insert_id,
                "changedRows": 0,
                "info": "",
            }));
        }
        let row_count = outcome.rows.len();
        let rows = drop_null_columns(outcome.rows.into_iter().map(Value::Object).collect());
        let mut out = Map::new();
        out.insert("rowCount".into(), json!(row_count));
        out.insert("rows".into(), Value::Array(rows));
        let report = limit_report(&prepared, row_count as i64, args.get("limit"));
        if let Value::Object(extra) = report {
            for (k, v) in extra {
                out.insert(k, v);
            }
        }
        Ok(Value::Object(out))
    }

    async fn call_list_tables(&self, args: &Value) -> Result<Value, String> {
        let Some(database) = self.database.clone() else {
            return Err(
                "no database configured on this MySQL MCP — set its database field, or query \
                 information_schema.tables through mysql_query"
                    .into(),
            );
        };
        let paging = table_page_args(args.get("limit"), args.get("page"));
        let grep = args.get("grep").and_then(Value::as_str);
        let ((list_sql, list_params), (count_sql, count_params)) =
            mysql_list_tables_sql(&database, grep, (paging.page, paging.limit, paging.offset), None);
        let pool = self.conn.get().await?;
        let (list, count) = tokio::join!(
            run_query(&pool, &list_sql, &list_params),
            run_query(&pool, &count_sql, &count_params)
        );
        let list = list?;
        let count = count?;
        let tables: Vec<Value> = list
            .rows
            .iter()
            .map(|r| {
                json!({
                    "schema": database,
                    "name": r.get("name").and_then(Value::as_str).unwrap_or(""),
                    "type": r.get("type").and_then(Value::as_str).unwrap_or(""),
                    // table_rows is an estimate (NULL for views) — keep NULL as unknown, not 0.
                    "approx_rows": numeric_or_null(r.get("approx_rows")),
                    "size": human_bytes(num_or_zero(r.get("bytes"))),
                })
            })
            .collect();
        let total = num_or_zero(count.rows.first().and_then(|r| r.get("total")));
        let listed = tables.len() as u64;
        Ok(json!({
            "tables": tables,
            "total": total,
            "page": paging.page,
            "limit": paging.limit,
            "more": (paging.offset as u64) + listed < total,
        }))
    }
}

#[async_trait]
impl Engine for MysqlEngine {
    fn kind(&self) -> &'static str {
        "mysql"
    }

    fn tools(&self) -> Vec<ToolDef> {
        tools()
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "mysql_query" => self.call_query(args).await,
            "mysql_list_tables" => self.call_list_tables(args).await,
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

    /// Schema as resources, scoped to the configured database. Without a `database` there is
    /// nothing to scope a listing to — the connection would see every table of the instance —
    /// so the capability is simply not announced in that case.
    fn resources(&self) -> Option<Arc<dyn super::resources::ResourceProvider>> {
        let database = self.database.clone()?;
        Some(Arc::new(MysqlResources::new(database, self.conn.clone())))
    }

    fn browser(&self) -> Option<swiss_host::dbbrowser::BrowserFlavor> {
        let database = self.database.clone()?;
        Some(swiss_host::dbbrowser::BrowserFlavor::Db(Arc::new(
            MysqlBrowser::new(database, self.target(), self.conn.clone()),
        )))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = async {
            let pool = self.conn.get().await?;
            let outcome = run_query(&pool, "SELECT 1 AS ok", &[]).await?;
            if !ping_ok(&outcome.rows) {
                return Err("mysql SELECT 1 returned no row".into());
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
    fn connect_options_defaults() {
        let def = ServerDef::default();
        let opts = mysql_connect_options(&def);
        assert_eq!(opts.get_host(), "localhost");
        assert_eq!(opts.get_port(), 3306);
    }

    #[test]
    fn session_sql_has_no_read_only_statement() {
        // The pool only caps runaway SELECTs; nothing makes the session read-only.
        assert_eq!(mysql_session_sql(), vec!["SET SESSION max_execution_time = 15000"]);
    }

    #[test]
    fn list_tables_sql_pairs_list_and_count() {
        let ((list_sql, list_params), (_count_sql, count_params)) =
            mysql_list_tables_sql("mydb", None, (0, 200, 0), None);
        assert!(list_sql.contains("table_schema = ?"));
        assert!(list_sql.contains("ORDER BY table_name"));
        assert!(!list_sql.contains("LIKE"));
        assert_eq!(list_params.len(), 3);
        assert_eq!(count_params.len(), 1);

        let ((list_sql, list_params), (_, count_params)) =
            mysql_list_tables_sql("mydb", Some("users"), (2, 50, 100), None);
        assert!(list_sql.contains("LIKE ? ESCAPE '!'"));
        assert_eq!(list_params.len(), 4);
        assert_eq!(count_params.len(), 2);
        assert_eq!(list_params[0], json!("mydb"));
        assert_eq!(list_params[1], json!("%users%"));
    }

    #[test]
    fn grammar_greps_expand_to_multi_like_sql() {
        // docs/22 W1.6: "user*|account" is one OR over two patterns; the database bind stays
        // first and the paging pair binds last, exactly like the single-LIKE statement.
        let w = swiss_host::dbbrowser::grep_where(
            swiss_host::dbbrowser::DbDialect::Mysql,
            "table_name",
            "user*|account",
            0,
        )
        .unwrap()
        .expect("grammar");
        let ((list_sql, list_params), (count_sql, count_params)) =
            mysql_list_tables_grammar_sql("mydb", &w.frag, &w.params, (0, 200, 0), None);
        assert!(
            list_sql.contains(
                "WHERE table_schema = ? AND (table_name LIKE ? ESCAPE '!' OR table_name LIKE ? ESCAPE '!')"
            ),
            "{list_sql}"
        );
        assert_eq!(list_params.len(), 5); // db, two patterns, limit, offset
        assert_eq!(count_params.len(), 3);
        assert_eq!(list_params[1], json!("user%"));
        assert_eq!(list_params[2], json!("%account%"));
        assert!(count_sql.contains("COUNT(*)"));
    }

    #[test]
    fn list_tables_sql_sorts_by_the_chosen_key_with_a_name_tiebreaker() {
        let rows_desc = TableSort { key: TableSortKey::Rows, desc: true };
        let size_asc = TableSort { key: TableSortKey::Size, desc: false };
        let name_desc = TableSort { key: TableSortKey::Name, desc: true };
        let ((list, _), _) = mysql_list_tables_sql("mydb", None, (0, 200, 0), Some(rows_desc));
        assert!(list.contains("ORDER BY approx_rows DESC, table_name"));
        let ((list, _), _) = mysql_list_tables_sql("mydb", None, (0, 200, 0), Some(size_asc));
        assert!(list.contains("ORDER BY bytes, table_name"));
        let ((list, _), _) = mysql_list_tables_sql("mydb", None, (0, 200, 0), Some(name_desc));
        assert!(list.contains("ORDER BY table_name DESC"));
    }

    #[test]
    fn ping_ok_accepts_both_spellings() {
        let mut row = Map::new();
        row.insert("ok".into(), json!("1"));
        assert!(ping_ok(&[row.clone()]));
        row.insert("ok".into(), json!(1));
        assert!(ping_ok(&[row]));
        assert!(!ping_ok(&[]));
    }
}
    #[test]
    fn a_stream_that_never_terminates_is_an_error_not_an_empty_success() {
        // The paid-for lesson, as a rule: every MySQL statement ends with the EOF/OK
        // terminator, so a stream that ends without one was cut mid-query. Answering
        // Ok(rows-so-far) there told a caller their table was EMPTY (or silently short)
        // while the tunnel was flapping — the worst failure shape there is.
        assert!(stream_end_verdict(true, 0).is_ok());
        assert!(stream_end_verdict(true, 4).is_ok());
        let cut = stream_end_verdict(false, 0).expect_err("cut before any row");
        assert!(cut.contains("connection lost mid-query"), "{cut}");
        let partial = stream_end_verdict(false, 3).expect_err("cut mid-result-set");
        assert!(partial.contains("3 row(s)"), "{partial}");
        assert!(partial.contains("retry"), "{partial}");
    }

