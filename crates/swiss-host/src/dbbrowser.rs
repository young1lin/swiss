//! The data-browser model behind the admin panel's "Data" view — port of `dbbrowser.ts`. A small
//! DBeaver-style table browser layered on the SAME driver pools the mysql/pg MCP adapters already
//! own.
//!
//! Pure logic + types + traits only: no HTTP here (`dbbrowser_api` owns the routes), no driver
//! imports (the adapters own the pools). Errors are Strings carrying the Node build's exact
//! messages; option bags are loose-typed `serde_json::Value` objects, mirroring Node's
//! `o: {...}` parameters (values arrive as strings from the query string, and the clamps below
//! coerce them the way `Number()` did).
//!
//! Two contracts matter here:
//!
//! - PAGING IS SERVER-SIDE, ALWAYS. The table list and the row grid both page at the database
//!   (LIMIT/OFFSET with a counted total), so an instance with thousands of tables or a
//!   million-row table costs one bounded page per click, never a full dump.
//!
//! - EDITS ARE TRANSACTIONAL. Cell edits, inserts and deletes arrive as a buffered edit list and
//!   touch the database only when the panel posts them to applyEdits, which runs every statement
//!   on ONE connection inside BEGIN ... COMMIT and rolls the whole batch back on the first
//!   failure. "Discard" never reaches the database at all — it only drops the client-side buffer.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// The browser flavor one registered MCP exposes. Carried by the connection catalog so a
/// consumer can browse a leased connection without knowing the adapter that built it — the
/// reason this type lives HERE (the host's vocabulary) and not in the data routes.
pub enum BrowserFlavor {
    Db(Arc<dyn DbBrowser>),
    Redis(Arc<dyn RedisBrowser>),
    /// Registered but nothing to browse (echo / proc / http / rest). Present so the lookup
    /// errors can name the adapter type exactly as Node's did; GET /api/db skips these rows.
    None,
}

/// Identifiers reach the catalog as bound parameters, but SHOW CREATE TABLE cannot bind one —
/// `[A-Za-z0-9_$]{1,64}` vetted by hand. Returns the Node build's message as a plain String;
/// the adapters wrap it into their own fault type (same text, one allocation).
pub fn assert_ident(name: &str, what: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
    if ok {
        Ok(())
    } else {
        Err(format!("not a valid {what}: {name}"))
    }
}

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

/// The SQL dialects the browser speaks (Node's `DbDialect`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DbDialect {
    Mysql,
    Pg,
}

impl DbDialect {
    pub fn as_str(&self) -> &'static str {
        match self {
            DbDialect::Mysql => "mysql",
            DbDialect::Pg => "pg",
        }
    }
}

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

/// Cap on rows in one import batch — one transaction, one bounded payload.
pub const IMPORT_ROW_CAP: usize = 10_000;

/// Hard ceiling on one export. A table bigger than this needs a filtered query, not a download —
/// the browser would happily try to hold a 4 GB string.
pub const EXPORT_ROW_CAP: i64 = 100_000;
/// Rows fetched per chunk while paging through an export.
pub const EXPORT_CHUNK: i64 = 5_000;

// --- JS value coercion ---------------------------------------------------------------------------

/// `Number(x)` for the loose-typed option values the browser methods accept: numbers pass
/// through, strings parse after a JS-style trim ("" → 0), booleans → 0/1, null → 0. None is the
/// NaN branch — the caller's fallback takes over, exactly where the Node build's
/// `Number.isFinite` check fell through. (A couple of JS exotics — hex strings, "Infinity" —
/// coerce differently; none of them is a page/limit argument any caller sends.)
pub fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(0.0)
            } else {
                t.parse::<f64>().ok()
            }
        }
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

/// `String(x)` for values the panel posted: absent → "undefined", null → "null", numbers render
/// the way JS prints them (integers plainly, floats in plain notation below 1e21). Objects and
/// arrays fall back to their compact JSON text — JS would say "[object Object]", but no code path
/// here stringifies an object for anything but an error message.
pub fn js_to_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => number_to_js_string(n),
        Some(other) => other.to_string(),
    }
}

/// A JSON number the way `String(number)` renders it in JS: integers plainly (i64 is always
/// below JS's 1e21 exponent threshold), floats plainly inside [1e-6, 1e21) and in exponential
/// notation outside it (JS spells positive exponents "1e+21"; Rust's `{:e}` says "1e21" — one
/// character apart, on values that only ever reach error text).
fn number_to_js_string(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) => float_to_js_string(f),
        None => n.to_string(),
    }
}

fn float_to_js_string(f: f64) -> String {
    if !f.is_finite() {
        // serde_json numbers are always finite; unreachable, but never panic over a value.
        return String::new();
    }
    let a = f.abs();
    if a != 0.0 && (a < 1e-6 || a >= 1e21) {
        format!("{f:e}")
    } else {
        format!("{f}")
    }
}

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

// --- shapes --------------------------------------------------------------------------------------

/// One column of the Structure view. `defaultValue` and `comment` are always serialized (null
/// when absent) — toBrowseColumns sets both unconditionally, the way the Node object literal did.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseColumn {
    pub name: String,
    #[serde(rename = "dataType")]
    pub data_type: String,
    pub nullable: bool,
    #[serde(rename = "isPrimaryKey")]
    pub is_primary_key: bool,
    #[serde(rename = "defaultValue")]
    pub default_value: Option<String>,
    /// The column's COMMENT text — MySQL information_schema or pg col_description. Null when the
    /// column carries none; the panel surfaces it in the grid header tooltip and Columns tab.
    pub comment: Option<String>,
}

/// One row of the lazy table list.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseTableInfo {
    pub schema: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "approxRows")]
    pub approx_rows: Option<i64>,
    pub size: String,
}

/// One index of the Structure tabs, folded from per-column rows by toBrowseIndexes.
/// MySQL leaves `definition` absent; PG fills it with the CREATE INDEX statement from pg_indexes.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseIndex {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowseForeignKey {
    pub name: String,
    pub column: String,
    #[serde(rename = "refSchema")]
    pub ref_schema: String,
    #[serde(rename = "refTable")]
    pub ref_table: String,
    #[serde(rename = "refColumn")]
    pub ref_column: String,
}

/// Columns, indexes, foreign keys and DDL for one table — the Structure tabs.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseTableDetail {
    pub schema: String,
    pub table: String,
    pub columns: Vec<BrowseColumn>,
    #[serde(rename = "primaryKey")]
    pub primary_key: Vec<String>,
    pub indexes: Vec<BrowseIndex>,
    #[serde(rename = "foreignKeys")]
    pub foreign_keys: Vec<BrowseForeignKey>,
    /// The CREATE TABLE text — verbatim from MySQL SHOW CREATE TABLE, synthesized from the
    /// catalog for Postgres (which has no equivalent command).
    pub ddl: String,
}

/// One applied edit: what it was, and how many rows it touched.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseEditResult {
    pub op: &'static str,
    pub affected: i64,
}

/// A whole-table export folded to text.
#[derive(Clone, Debug, Serialize)]
pub struct ExportResult {
    pub format: String,
    pub columns: Vec<String>,
    pub rows: i64,
    pub capped: bool,
    pub body: String,
}

/// One redis key as the key list shows it. ttl is seconds; -1 = no expiry.
#[derive(Clone, Debug, Serialize)]
pub struct RedisKeyInfo {
    pub key: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub ttl: i64,
}

/// One buffered change, as the panel posts it. pk maps primary-key column -> value; a null or
/// string value is passed through to the driver as a bound parameter (both drivers cast).
#[derive(Clone, Debug)]
pub enum BrowseEdit {
    Update {
        pk: Map<String, Value>,
        changes: Map<String, Value>,
    },
    Insert {
        values: Map<String, Value>,
    },
    Delete {
        pk: Map<String, Value>,
    },
}

impl BrowseEdit {
    pub fn op(&self) -> &'static str {
        match self {
            BrowseEdit::Update { .. } => "update",
            BrowseEdit::Insert { .. } => "insert",
            BrowseEdit::Delete { .. } => "delete",
        }
    }
}

/// One grid filter as the panel posts it ({column, op, value}). Structural validation happened
/// at the route; unknown columns/operators are refused by build_filter_where, which keeps the
/// rule next to the SQL it guards.
#[derive(Clone, Debug)]
pub struct BrowseFilter {
    pub column: String,
    pub op: String,
    pub value: Option<Value>,
}

/// A statement plus its bound parameters.
#[derive(Clone, Debug)]
pub struct BuiltStatement {
    pub sql: String,
    pub params: Vec<Value>,
}

/// A WHERE fragment plus the parameters it binds.
#[derive(Clone, Debug)]
pub struct BuiltWhere {
    pub frag: String,
    pub params: Vec<Value>,
}

/// The column list build_filter_where guards: a bare name (the mysql callers pass no type info,
/// so a pg-style CAST can never be built) or a full BrowseColumn — Node's `(string | BrowseColumn)[]`.
#[derive(Clone, Debug)]
pub enum FilterColumn {
    Name(String),
    Column(BrowseColumn),
}

impl FilterColumn {
    pub fn name(&self) -> &str {
        match self {
            FilterColumn::Name(n) => n,
            FilterColumn::Column(c) => &c.name,
        }
    }

    /// The column's information_schema data_type, when the caller knew it.
    pub fn data_type(&self) -> Option<&str> {
        match self {
            FilterColumn::Name(_) => None,
            FilterColumn::Column(c) => Some(&c.data_type),
        }
    }
}

// --- the browser traits --------------------------------------------------------------------------

/// What an adapter exposes for the browser — Node's `DbBrowser` interface. Constructed on
/// demand; the heavy thing (the driver pool) is the adapter's own Lazy, shared with the MCP tools.
///
/// Methods take the same loose option objects the Node build received (`o: { table, schema?, ... }`)
/// and return the reply JSON whole, so the routes forward shapes byte-identically and the
/// contract stays in one place. Implementors must keep Node's error message strings.
#[async_trait]
pub trait DbBrowser: Send + Sync {
    fn dialect(&self) -> DbDialect;
    /// Where this connection points ("app @ localhost:3306"). Never a password.
    fn label(&self) -> String;
    async fn list_tables(&self, o: &Value) -> Result<Value, String>;
    async fn read_table(&self, o: &Value) -> Result<Value, String>;
    /// Columns, indexes, foreign keys and DDL for one table — the Structure tabs.
    async fn describe_table(&self, o: &Value) -> Result<Value, String>;
    /// Apply a buffered edit list in ONE transaction: all of it, or none of it.
    async fn apply_edits(&self, o: &Value) -> Result<Value, String>;
    /// The SQL console: one statement per run, reads and writes alike — this console belongs
    /// to the panel on the operator's own machine.
    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String>;
    /// Stream a whole table (capped at EXPORT_ROW_CAP) out as CSV or newline JSON.
    async fn export_table(&self, o: &Value) -> Result<Value, String>;
    /// Insert mapped CSV rows in ONE transaction (all-or-nothing), via the same statement
    /// builders the edit grid uses. Returns rows written.
    async fn import_table(&self, o: &Value) -> Result<Value, String>;
    /// Rename / truncate / drop a table.
    async fn ddl_op(&self, o: &Value) -> Result<Value, String>;
}

/// The redis flavour of the Data view: page keys by SCAN, read one key type-aware.
#[async_trait]
pub trait RedisBrowser: Send + Sync {
    fn label(&self) -> String;
    async fn list_keys(&self, o: &Value) -> Result<Value, String>;
    async fn read_key(&self, key: &str) -> Result<Value, String>;
    /// Run ONE command from the console: reads AND writes (SET, DEL, EXPIRE…). The adapter's
    /// own command guard still refuses what would break the shared connection or the server.
    async fn run_command(&self, line: &str) -> Result<Value, String>;
}

// --- identifier quoting --------------------------------------------------------------------------

/// Quote one identifier for the dialect, after assert_ident has vetted it.
///
/// The browser interpolates table/column names into SQL (LIMIT/OFFSET paging cannot bind them),
/// so every identifier passes the same SAFE_IDENT gate the resource URIs use — then it is quoted
/// anyway. Schema-qualified names are quoted per part by qualified().
pub fn quote_ident(dialect: DbDialect, name: &str) -> Result<String, String> {
    let what = if dialect == DbDialect::Mysql {
        "MySQL identifier"
    } else {
        "Postgres identifier"
    };
    assert_ident(name, what)?;
    Ok(if dialect == DbDialect::Mysql {
        format!("`{name}`")
    } else {
        format!("\"{name}\"")
    })
}

/// A quoted, schema-qualified table reference (backticks for MySQL, double quotes for PG).
/// An empty schema is dropped the way JS's truthiness check did (`ref.schema ? s.t : t`).
pub fn qualified(dialect: DbDialect, schema: Option<&str>, table: &str) -> Result<String, String> {
    match schema.filter(|s| !s.is_empty()) {
        Some(s) => Ok(format!(
            "{}.{}",
            quote_ident(dialect, s)?,
            quote_ident(dialect, table)?
        )),
        None => quote_ident(dialect, table),
    }
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
            return Err(format!("cannot sort the table list by unknown key: {other}"))
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
                let ph = bind(&mut params, Value::String(like_contains(&text)));
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
            // docs/22 W1.2: IN/NOT IN bind a comma-separated list, BETWEEN the closed interval
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
/// (docs/22 W0.2), so the filter applied to a page is by construction the filter applied to
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
fn ph(dialect: DbDialect, params: &[Value]) -> String {
    // bind() has already pushed the value, so the array length IS this parameter's 1-based number.
    if dialect == DbDialect::Mysql {
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
fn typed_ph(dialect: DbDialect, params: &[Value], data_type: Option<&str>) -> String {
    let base = ph(dialect, params);
    if dialect != DbDialect::Pg {
        return base;
    }
    match castable_data_type(data_type) {
        Some(t) => format!("CAST({base} AS {t})"),
        None => base,
    }
}

/// Turn a buffered edit list into executable statements: one statement per edit, in order, with
/// every value bound and every identifier pre-vetted. Column names the table does not have are
/// dropped from SET/INSERT (a stale page may reference a dropped column); an edit that ends up
/// with nothing to do is an error, because "commit 3 changes" that silently did 2 is a lie.
///
/// Updates and deletes address rows by the FULL primary key only — the DBeaver default — so an
/// edit can never fan out over more rows than the cell you changed.
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
        match edit {
            BrowseEdit::Insert { values } => {
                // Object.keys order — the panel's own insertion order.
                let cols: Vec<&String> = values
                    .keys()
                    .filter(|c| known.contains(c.as_str()))
                    .collect();
                if cols.is_empty() {
                    return Err("insert names no column of this table".into());
                }
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
                out.push(BuiltStatement {
                    sql: format!(
                        "INSERT INTO {table_ref} ({names}) VALUES ({})",
                        value_sql.join(", ")
                    ),
                    params,
                });
            }
            BrowseEdit::Delete { pk } => {
                // update / delete: the WHERE needs a value for every PK column, or the edit is
                // unaddressable.
                if primary_key.is_empty() {
                    return Err(format!(
                        "table has no primary key — {} is not possible",
                        edit.op()
                    ));
                }
                for c in primary_key {
                    if !pk.contains_key(c) {
                        return Err(format!(
                            "{} needs a value for primary-key column {}",
                            edit.op(),
                            c
                        ));
                    }
                }
                let where_sql = pk_where(pk, &mut params)?;
                out.push(BuiltStatement {
                    sql: format!("DELETE FROM {table_ref} WHERE {where_sql}"),
                    params,
                });
            }
            BrowseEdit::Update { pk, changes } => {
                if primary_key.is_empty() {
                    return Err(format!(
                        "table has no primary key — {} is not possible",
                        edit.op()
                    ));
                }
                for c in primary_key {
                    if !pk.contains_key(c) {
                        return Err(format!(
                            "{} needs a value for primary-key column {}",
                            edit.op(),
                            c
                        ));
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
                let where_sql = pk_where(pk, &mut params)?;
                out.push(BuiltStatement {
                    sql: format!(
                        "UPDATE {table_ref} SET {} WHERE {}",
                        set_sql.join(", "),
                        where_sql
                    ),
                    params,
                });
            }
        }
    }
    Ok(out)
}

// --- table structure -----------------------------------------------------------------------------

/// Shape raw information_schema column rows into BrowseColumn, marking PK membership.
pub fn to_browse_columns(rows: &[Map<String, Value>], pk: &[String]) -> Vec<BrowseColumn> {
    let pk_set: HashSet<&str> = pk.iter().map(String::as_str).collect();
    rows.iter()
        .map(|r| {
            let name = js_to_string(r.get("column_name"));
            BrowseColumn {
                is_primary_key: pk_set.contains(name.as_str()),
                data_type: js_to_string(r.get("data_type")),
                nullable: js_to_string(r.get("is_nullable")).to_uppercase() == "YES",
                default_value: r
                    .get("column_default")
                    .filter(|v| !v.is_null())
                    .map(|v| js_to_string(Some(v))),
                // A blank comment is no comment — the panel's tooltip stays empty, not " ".
                comment: r
                    .get("column_comment")
                    .filter(|v| !v.is_null())
                    .map(|v| js_to_string(Some(v)))
                    .filter(|s| !s.is_empty()),
                name,
            }
        })
        .collect()
}

/// Fold raw index rows (one per indexed COLUMN, already aliased: name/unique(0|1)/primary/column,
/// in index order) into one BrowseIndex per index, columns in arrival order.
pub fn to_browse_indexes(rows: &[Map<String, Value>]) -> Vec<BrowseIndex> {
    // Insertion-ordered like the JS Map whose values() the Node build spread out.
    let mut by_name: Vec<(String, BrowseIndex)> = Vec::new();
    for r in rows {
        let name = js_to_string(r.get("name"));
        if !by_name.iter().any(|(n, _)| n == &name) {
            // unique: Number(r.unique) !== 1 — NaN counts as unique, same as the JS comparison.
            let unique = js_number(r.get("unique").unwrap_or(&Value::Null)) != Some(1.0);
            // primary: strict === 1 or === true.
            let primary = match r.get("primary") {
                Some(Value::Number(n)) => n.as_f64() == Some(1.0),
                Some(Value::Bool(b)) => *b,
                _ => false,
            };
            let definition = r
                .get("definition")
                .filter(|v| !v.is_null())
                .map(|v| js_to_string(Some(v)));
            by_name.push((
                name.clone(),
                BrowseIndex {
                    name: name.clone(),
                    unique,
                    primary,
                    columns: Vec::new(),
                    definition,
                },
            ));
        }
        if let Some(col) = r.get("column").filter(|v| !v.is_null()) {
            if let Some((_, idx)) = by_name.iter_mut().find(|(n, _)| n == &name) {
                idx.columns.push(js_to_string(Some(col)));
            }
        }
    }
    by_name.into_iter().map(|(_, idx)| idx).collect()
}

/// Postgres has no SHOW CREATE TABLE. Rather than shell out to pg_dump (not installed, needs a
/// password dance, version-matched), assemble a faithful sketch from the catalog pieces the
/// detail view already fetched. Column types come out as information_schema spells them.
pub fn build_pg_ddl(
    schema: &str,
    table: &str,
    columns: &[BrowseColumn],
    primary_key: &[String],
    foreign_keys: &[BrowseForeignKey],
) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    for c in columns {
        let mut line = format!(
            "    {} {}",
            quote_ident(DbDialect::Pg, &c.name)?,
            c.data_type
        );
        if !c.nullable {
            line.push_str(" NOT NULL");
        }
        if let Some(d) = &c.default_value {
            if !d.is_empty() {
                line.push_str(&format!(" DEFAULT {d}"));
            }
        }
        lines.push(line + ",");
    }
    if !primary_key.is_empty() {
        let cols = primary_key
            .iter()
            .map(|c| quote_ident(DbDialect::Pg, c))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!("    PRIMARY KEY ({}),", cols.join(", ")));
    }
    for fk in foreign_keys {
        lines.push(format!(
            "    FOREIGN KEY ({}) REFERENCES {}.{} ({}),",
            quote_ident(DbDialect::Pg, &fk.column)?,
            quote_ident(DbDialect::Pg, &fk.ref_schema)?,
            quote_ident(DbDialect::Pg, &fk.ref_table)?,
            quote_ident(DbDialect::Pg, &fk.ref_column)?
        ));
    }
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", lines.join("\n"))
    };
    Ok(format!(
        "CREATE TABLE {}.{} ({body});",
        quote_ident(DbDialect::Pg, schema)?,
        quote_ident(DbDialect::Pg, table)?
    ))
}

/// The statement behind a structure operation. Every identifier passes assert_ident + dialect
/// quoting on the way in (these statements bind nothing — the names ARE the payload).
pub fn build_ddl_op_sql(
    dialect: DbDialect,
    op: &str,
    schema: Option<&str>,
    table: &str,
    to: Option<&str>,
) -> Result<String, String> {
    match op {
        "rename" => {
            let Some(to) = to.filter(|t| !t.is_empty()) else {
                return Err("rename needs the new table name".into());
            };
            if dialect == DbDialect::Mysql {
                // Same-schema rename only — the panel never offered a cross-schema one.
                let target = match schema.filter(|s| !s.is_empty()) {
                    Some(s) => qualified(dialect, Some(s), to)?,
                    None => quote_ident(dialect, to)?,
                };
                Ok(format!(
                    "RENAME TABLE {} TO {}",
                    qualified(dialect, schema, table)?,
                    target
                ))
            } else {
                Ok(format!(
                    "ALTER TABLE {} RENAME TO {}",
                    qualified(dialect, schema, table)?,
                    quote_ident(dialect, to)?
                ))
            }
        }
        "truncate" => Ok(format!(
            "TRUNCATE TABLE {}",
            qualified(dialect, schema, table)?
        )),
        "drop" => Ok(format!("DROP TABLE {}", qualified(dialect, schema, table)?)),
        other => Err(format!("unknown structure operation: {other}")),
    }
}

// --- the read-only console -----------------------------------------------------------------------

/// Prefix a statement with EXPLAIN for the console's plan view — idempotent, so a query that
/// already explains itself is not double-prefixed. The statement keeps its single trailing
/// terminator stripped (EXPLAIN accepts one statement, not one plus a dangling ";").
///
/// Both dialects spell it the same way. Read-only-ness still gates the WHOLE statement inside
/// run_query: EXPLAIN of an INSERT is refused there by the write-keyword scan, and EXPLAIN ANALYZE
/// of a SELECT is allowed (it executes the select — reads only).
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

// --- CSV import ----------------------------------------------------------------------------------

/// Parse ONE line of CSV honoring RFC 4180 quoting (a quoted field may span lines is NOT
/// supported here — the panel splits on physical newlines, and quoted newlines inside a field
/// are the importer's job to have escaped or avoided; fields with them arrive via JSON upload).
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_q {
            if c == '"' && i + 1 < chars.len() && chars[i + 1] == '"' {
                cur.push('"');
                i += 1;
            } else if c == '"' {
                in_q = false;
            } else {
                cur.push(c);
            }
        } else if c == ',' {
            out.push(std::mem::take(&mut cur));
        } else if c == '"' && cur.is_empty() {
            // A quote only opens a quoted field at the field's very start.
            in_q = true;
        } else {
            cur.push(c);
        }
        i += 1;
    }
    out.push(cur);
    out
}

/// Turn CSV lines into insert values using the mapping (header index -> table column, None =
/// skip this CSV column). Coercion is deliberately thin: empty string becomes nothing (the key
/// is never set), numeric-looking strings become numbers ONLY when Number() is lossless
/// (numeric_bind_value — a 19-digit snowflake ID or a beyond-double-precision decimal stays a
/// string so the driver casts it against the column exactly), everything else stays a string and
/// the driver casts. A row whose mapped cells are ALL empty is dropped rather than inserted as
/// NULLs.
pub fn map_import_rows(
    header: &[String],
    lines: &[String],
    mapping: &[Option<String>],
) -> Result<Vec<Map<String, Value>>, String> {
    if mapping.len() != header.len() {
        return Err(format!(
            "mapping covers {} of {} CSV columns",
            mapping.len(),
            header.len()
        ));
    }
    let mut out: Vec<Map<String, Value>> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let cells = parse_csv_line(line);
        let mut row: Map<String, Value> = Map::new();
        let mut any = false;
        for i in 0..header.len() {
            // Node's `if (!col)` — None and "" both skip the column.
            let Some(col) = mapping
                .get(i)
                .and_then(|c| c.as_deref())
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let raw = cells.get(i).map(String::as_str).unwrap_or("");
            if raw.is_empty() {
                continue;
            }
            any = true;
            row.insert(col.to_string(), numeric_bind_value(raw));
        }
        if any {
            out.push(row);
        }
    }
    Ok(out)
}

// --- full-table export ---------------------------------------------------------------------------

/// Fold fetched rows into the final CSV text (RFC 4180, header row included).
pub fn to_csv(columns: &[String], rows: &[Map<String, Value>]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(rows.len() + 1);
    out.push(
        columns
            .iter()
            .map(|c| csv_escape(Some(&Value::String(c.clone()))))
            .collect::<Vec<_>>()
            .join(","),
    );
    for r in rows {
        out.push(
            columns
                .iter()
                .map(|c| csv_escape(r.get(c)))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    out.join("\r\n")
}

/// Fold fetched rows into newline-delimited JSON (one object per line — splittable, streamable).
pub fn to_json_lines(rows: &[Map<String, Value>]) -> String {
    rows.iter()
        .map(|r| Value::Object(r.clone()).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

// --- copy-out helpers ----------------------------------------------------------------------------

/// One CSV cell, quoted per RFC 4180: quotes doubled, embedded separators/newlines survive.
/// Null and absent are the empty (unquoted) cell; an object becomes compact JSON, whose quotes
/// double like any other quote inside the cell.
pub fn csv_escape(v: Option<&Value>) -> String {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return String::new();
    };
    let s = match v {
        Value::Object(_) | Value::Array(_) => v.to_string(),
        other => js_to_string(Some(other)),
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// One SQL literal for the clipboard — never executed, so inlining is safe here.
pub fn sql_literal(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "NULL".into();
    };
    match v {
        Value::Null => "NULL".into(),
        Value::Number(n) => number_to_js_string(n),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        other => {
            let s = js_to_string(Some(other));
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            for c in s.chars() {
                if c == '\'' {
                    out.push('\'');
                }
                out.push(c);
            }
            out.push('\'');
            out
        }
    }
}

/// INSERT INTO t (a, b) VALUES (...); — mirrors build_edit_statements identifier rules
/// (each column vetted + dialect-quoted) so what you copy matches what Commit would run.
pub fn to_insert_statement(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    column_names: &[String],
    row: &Map<String, Value>,
) -> Result<String, String> {
    // row[c] !== undefined — a present-but-null value copies as NULL; only absent keys drop.
    let cols: Vec<&String> = column_names
        .iter()
        .filter(|c| row.contains_key(c.as_str()))
        .collect();
    if cols.is_empty() {
        return Err("the row has no values to copy".into());
    }
    let names = cols
        .iter()
        .map(|c| quote_ident(dialect, c))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let values = cols
        .iter()
        .map(|c| sql_literal(row.get(c.as_str())))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "INSERT INTO {} ({names}) VALUES ({values});",
        qualified(dialect, schema, table)?
    ))
}

#[cfg(test)]
#[allow(clippy::approx_constant)]
mod tests {
    // Ported from the Node build's dbbrowser.test.ts (the pure-helper half; the HTTP half lives
    // in dbbrowser_api's tests).
    use super::*;

    fn names(cols: &[&str]) -> Vec<FilterColumn> {
        cols.iter()
            .map(|c| FilterColumn::Name((*c).to_string()))
            .collect()
    }

    fn col(name: &str, data_type: &str, nullable: bool, pk: bool) -> BrowseColumn {
        BrowseColumn {
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            is_primary_key: pk,
            default_value: None,
            comment: None,
        }
    }

    fn filter(json: Value) -> Vec<BrowseFilter> {
        browse_filters_of(Some(&json))
    }

    #[test]
    fn browse_paging_clamps() {
        assert_eq!(browse_page_size(None, BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(
            browse_page_size(Some(&json!("nonsense")), BROWSE_DEFAULT_PAGE),
            50
        );
        assert_eq!(browse_page_size(Some(&json!(0)), BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(browse_page_size(Some(&json!(-5)), BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(
            browse_page_size(Some(&json!(100_000)), BROWSE_DEFAULT_PAGE),
            500
        );
        assert_eq!(browse_page_size(Some(&json!(10)), BROWSE_DEFAULT_PAGE), 10);
        assert_eq!(BROWSE_PAGE_SIZES, [10, 20, 50, 100, 200, 500]);
        assert_eq!(browse_offset(None), 0);
        assert_eq!(browse_offset(Some(&json!(-3))), 0);
        assert_eq!(browse_offset(Some(&json!("40"))), 40);
    }

    #[test]
    fn identifier_quoting() {
        assert_eq!(quote_ident(DbDialect::Mysql, "users").unwrap(), "`users`");
        assert_eq!(quote_ident(DbDialect::Pg, "users").unwrap(), "\"users\"");
        for bad in ["users; DROP TABLE x", "a`b", "c\"d", "with space", ""] {
            assert!(
                quote_ident(DbDialect::Mysql, bad).is_err(),
                "mysql accepted {bad:?}"
            );
            assert!(
                quote_ident(DbDialect::Pg, bad).is_err(),
                "pg accepted {bad:?}"
            );
        }
    }

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
    fn in_not_in_and_between_bind_every_value() {
        // docs/22 W1.2: IN/NOT IN split the value on commas and bind each item; BETWEEN takes
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
    fn browse_where_is_the_one_assembly_rows_and_export_share() {
        // docs/22 W0.2: rows, COUNT and export all build their WHERE through browse_where, so
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
        let count = browse_count_sql(DbDialect::Mysql, Some("app"), "users", &w.frag, &w.params)
            .unwrap();
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

    fn stub_columns() -> Vec<BrowseColumn> {
        vec![
            col("id", "int", false, true),
            col("name", "text", true, false),
        ]
    }

    fn edit(v: Value) -> BrowseEdit {
        browse_edit_of(&v).unwrap()
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
        let err = build_edit_statements(
            DbDialect::Mysql,
            None,
            "u",
            &[edit(json!({ "op": "delete", "pk": {} }))],
            &cols,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("no primary key"), "{err}");
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

    fn row(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn to_browse_columns_marks_pk_and_nullability() {
        let rows = vec![
            row(&[
                ("column_name", json!("id")),
                ("data_type", json!("int")),
                ("is_nullable", json!("NO")),
                ("column_default", Value::Null),
            ]),
            row(&[
                ("column_name", json!("name")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", json!("''")),
            ]),
        ];
        let cols = to_browse_columns(&rows, &["id".to_string()]);
        assert_eq!(cols[0].name, "id");
        assert!(!cols[0].nullable);
        assert!(cols[0].is_primary_key);
        assert_eq!(cols[1].name, "name");
        assert!(cols[1].nullable);
        assert!(!cols[1].is_primary_key);
        assert_eq!(cols[1].default_value.as_deref(), Some("''"));
    }

    #[test]
    fn to_browse_columns_carries_comments() {
        let rows = vec![
            row(&[
                ("column_name", json!("id")),
                ("data_type", json!("bigint")),
                ("is_nullable", json!("NO")),
                ("column_default", Value::Null),
                ("column_comment", json!("Snowflake-style row ID")),
            ]),
            row(&[
                ("column_name", json!("name")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", Value::Null),
                ("column_comment", json!("")),
            ]),
            row(&[
                ("column_name", json!("note")),
                ("data_type", json!("text")),
                ("is_nullable", json!("YES")),
                ("column_default", Value::Null),
            ]),
        ];
        let cols = to_browse_columns(&rows, &["id".to_string()]);
        assert_eq!(cols[0].comment.as_deref(), Some("Snowflake-style row ID"));
        assert!(cols[1].comment.is_none());
        assert!(cols[2].comment.is_none());
    }

    #[test]
    fn to_browse_indexes_folds_per_column_rows() {
        let rows = vec![
            row(&[
                ("name", json!("PRIMARY")),
                ("unique", json!(0)),
                ("primary", json!(1)),
                ("column", json!("id")),
            ]),
            row(&[
                ("name", json!("name_idx")),
                ("unique", json!(1)),
                ("primary", json!(0)),
                ("column", json!("name")),
            ]),
            row(&[
                ("name", json!("name_idx")),
                ("unique", json!(1)),
                ("primary", json!(0)),
                ("column", json!("email")),
            ]),
        ];
        let idx = to_browse_indexes(&rows);
        assert_eq!(idx.len(), 2);
        let pk = idx.iter().find(|i| i.name == "PRIMARY").unwrap();
        assert!(pk.unique);
        assert!(pk.primary);
        assert_eq!(pk.columns, vec!["id"]);
        let other = idx.iter().find(|i| i.name == "name_idx").unwrap();
        assert!(!other.unique);
        assert!(!other.primary);
        assert_eq!(other.columns, vec!["name", "email"]);
        assert!(other.definition.is_none());
    }

    #[test]
    fn build_pg_ddl_sketches_the_catalog() {
        let cols = vec![
            col("id", "bigint", false, true),
            BrowseColumn {
                default_value: Some("''::text".into()),
                ..col("email", "text", false, false)
            },
            col("note", "text", true, false),
        ];
        let fks = vec![BrowseForeignKey {
            name: "owner_fk".into(),
            column: "id".into(),
            ref_schema: "public".into(),
            ref_table: "users".into(),
            ref_column: "id".into(),
        }];
        let ddl = build_pg_ddl("public", "things", &cols, &["id".to_string()], &fks).unwrap();
        assert!(ddl.contains("CREATE TABLE \"public\".\"things\""));
        assert!(ddl.contains("\"id\" bigint NOT NULL"));
        assert!(ddl.contains("\"email\" text NOT NULL DEFAULT ''::text"));
        assert!(ddl.contains("\"note\" text"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        assert!(ddl.contains("FOREIGN KEY (\"id\") REFERENCES \"public\".\"users\" (\"id\")"));
    }

    #[test]
    fn build_pg_ddl_omits_empty_pk_and_fk_clauses() {
        let ddl = build_pg_ddl("s", "t", &[col("x", "int", true, false)], &[], &[]).unwrap();
        assert!(!ddl.contains("PRIMARY KEY"));
        assert!(!ddl.contains("FOREIGN KEY"));
    }

    #[test]
    fn with_explain_prefixes_once_and_strips_the_terminator() {
        assert_eq!(with_explain("SELECT * FROM t"), "EXPLAIN SELECT * FROM t");
        assert_eq!(with_explain("select 1"), "EXPLAIN select 1");
        assert_eq!(with_explain("EXPLAIN SELECT 1"), "EXPLAIN SELECT 1");
        assert_eq!(with_explain("explain select 1;"), "explain select 1");
        assert_eq!(with_explain("SELECT 1;\n"), "EXPLAIN SELECT 1");
    }

    #[test]
    fn ddl_ops_build_dialect_correct_statements() {
        assert_eq!(
            build_ddl_op_sql(
                DbDialect::Mysql,
                "rename",
                Some("app"),
                "users",
                Some("members")
            )
            .unwrap(),
            "RENAME TABLE `app`.`users` TO `app`.`members`"
        );
        assert_eq!(
            build_ddl_op_sql(
                DbDialect::Pg,
                "rename",
                Some("public"),
                "users",
                Some("members")
            )
            .unwrap(),
            "ALTER TABLE \"public\".\"users\" RENAME TO \"members\""
        );
        assert_eq!(
            build_ddl_op_sql(DbDialect::Mysql, "truncate", None, "users", None).unwrap(),
            "TRUNCATE TABLE `users`"
        );
        assert_eq!(
            build_ddl_op_sql(DbDialect::Pg, "drop", Some("s"), "t", None).unwrap(),
            "DROP TABLE \"s\".\"t\""
        );
        let err = build_ddl_op_sql(DbDialect::Mysql, "rename", None, "a", None).unwrap_err();
        assert!(err.contains("new table name"), "{err}");
        let err = build_ddl_op_sql(DbDialect::Mysql, "explode", None, "a", None).unwrap_err();
        assert!(err.contains("unknown structure operation"), "{err}");
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

    #[test]
    fn clamp_browse_limit_falls_back_and_caps() {
        assert_eq!(clamp_browse_limit(None, 200, 1000), 200);
        assert_eq!(clamp_browse_limit(Some(&json!(50)), 200, 1000), 50);
        assert_eq!(clamp_browse_limit(Some(&json!(99_999)), 200, 1000), 1000);
        assert_eq!(clamp_browse_limit(Some(&json!("12")), 200, 1000), 12);
        assert_eq!(clamp_browse_limit(Some(&json!(0)), 200, 1000), 200);
        assert_eq!(clamp_browse_limit(Some(&json!("x")), 200, 1000), 200);
    }

    #[test]
    fn csv_helpers_round_trip() {
        // RFC 4180 line: doubled quotes, embedded commas.
        assert_eq!(
            parse_csv_line("a,\"b,c\",\"say \"\"hi\"\"\""),
            vec!["a", "b,c", "say \"hi\""]
        );
        assert_eq!(parse_csv_line("plain,cells"), vec!["plain", "cells"]);

        // mapImportRows: coerce numbers, drop empty rows, honor the mapping's nulls.
        let header = vec!["id".to_string(), "name".to_string(), "note".to_string()];
        let lines = vec![
            "1,alice,x".to_string(),
            "\"2\",\"bob\",".to_string(),
            ",,".to_string(),
        ];
        let mapping = vec![Some("id".to_string()), Some("name".to_string()), None];
        let rows = map_import_rows(&header, &lines, &mapping).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&json!(1)));
        assert_eq!(rows[0].get("name"), Some(&json!("alice")));
        assert_eq!(rows[1].get("id"), Some(&json!(2)));

        let err = map_import_rows(&["a".to_string()], &["1".to_string()], &[]).unwrap_err();
        assert!(err.contains("covers"), "{err}");
    }

    #[test]
    fn map_import_rows_keeps_snowflake_precision() {
        let header = vec!["id".to_string(), "qty".to_string(), "note".to_string()];
        let mapping = vec![
            Some("id".to_string()),
            Some("qty".to_string()),
            Some("note".to_string()),
        ];
        let rows = map_import_rows(
            &header,
            &["734023681584275456,3,hello".to_string()],
            &mapping,
        )
        .unwrap();
        assert_eq!(rows[0].get("id"), Some(&json!("734023681584275456")));
        assert_eq!(rows[0].get("qty"), Some(&json!(3)));
        assert_eq!(rows[0].get("note"), Some(&json!("hello")));
        let rows = map_import_rows(
            &header,
            &["734023681584275456,0.100000000000000009,x".to_string()],
            &mapping,
        )
        .unwrap();
        assert_eq!(rows[0].get("qty"), Some(&json!("0.100000000000000009")));
    }

    #[test]
    fn export_helpers() {
        assert_eq!(EXPORT_ROW_CAP, 100_000);
        assert_eq!(export_row_limit(None), EXPORT_ROW_CAP);
        assert_eq!(export_row_limit(Some(&json!(500))), 500);
        assert_eq!(export_row_limit(Some(&json!(999_999_999))), EXPORT_ROW_CAP);

        // Every cell is quoted uniformly (numbers included) — simplest RFC-legal shape.
        let rows = vec![
            row(&[("id", json!(1)), ("name", json!("a,b\""))]),
            row(&[("id", Value::Null), ("name", Value::Null)]),
        ];
        let csv = to_csv(&["id".to_string(), "name".to_string()], &rows);
        assert_eq!(csv, "\"id\",\"name\"\r\n\"1\",\"a,b\"\"\"\r\n,");

        let rows = vec![row(&[("a", json!(1))]), row(&[("b", Value::Null)])];
        assert_eq!(to_json_lines(&rows), "{\"a\":1}\n{\"b\":null}");
    }

    #[test]
    fn copy_out_helpers() {
        assert_eq!(csv_escape(None), "");
        assert_eq!(csv_escape(Some(&Value::Null)), "");
        assert_eq!(csv_escape(Some(&json!("plain"))), "\"plain\"");
        assert_eq!(csv_escape(Some(&json!("say \"hi\""))), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape(Some(&json!("a,b\nline2"))), "\"a,b\nline2\"");
        // An object becomes compact JSON, whose quotes double like any other quote inside the cell.
        assert_eq!(csv_escape(Some(&json!({ "x": 1 }))), "\"{\"\"x\"\":1}\"");

        assert_eq!(sql_literal(None), "NULL");
        assert_eq!(sql_literal(Some(&Value::Null)), "NULL");
        assert_eq!(sql_literal(Some(&json!(42))), "42");
        assert_eq!(sql_literal(Some(&json!(true))), "true");
        assert_eq!(sql_literal(Some(&json!("it's"))), "'it''s'");

        let out = to_insert_statement(
            DbDialect::Mysql,
            Some("app"),
            "users",
            &["id".to_string(), "name".to_string(), "ghost".to_string()],
            &row(&[("id", json!(1)), ("name", json!("alice"))]),
        )
        .unwrap();
        assert_eq!(
            out,
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES (1, 'alice');"
        );
        let pg = to_insert_statement(
            DbDialect::Pg,
            None,
            "t",
            &["a".to_string()],
            &row(&[("a", Value::Null)]),
        )
        .unwrap();
        assert_eq!(pg, "INSERT INTO \"t\" (\"a\") VALUES (NULL);");
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
