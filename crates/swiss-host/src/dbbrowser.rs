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

/// Total LIKE patterns one table-list grep may expand to (AND terms x OR alternatives) — the
/// grep-side twin of the route's filter-count cap, so a hand-written URL cannot ask for a
/// hundred-term WHERE either.
pub const BROWSE_GREP_MAX: usize = 32;

/// One side of the sidebar grep grammar (docs/22 W1.6): comma-separated terms AND together,
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
/// The flush threshold of a streaming SQL dump (docs/22 W4.4) — adminer's max_packet number
/// (`adminer.inc.php:982`): rows accumulate into one multi-value INSERT until the statement
/// would cross this size, then the statement ships whole and a fresh one opens. A single row
/// larger than the threshold is never split — it ships alone, exactly as adminer let it.
pub const EXPORT_SQL_MAX_PACKET: usize = 1_048_576;

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

/// One piece of a streaming export body: UTF-8 bytes ready for the socket, or the error that
/// ended the dump early. Metadata failures happen before any piece exists and answer as a
/// plain Err from export_sql_dump; a failure mid-stream aborts the download, which is the
/// honest behavior of a body whose headers were already sent.
pub type DumpPiece = Result<Vec<u8>, String>;

/// The streaming form of format=sql (docs/22 W4.4): response-header facts up front — rows and
/// capped come from a COUNT over the same WHERE the grid, the CSV and the NDJSON exports use —
/// and the body arriving as pieces on a bounded channel. The producer holds one chunk of
/// source rows and one statement under construction, never the table: a dump of any capped
/// size costs O(chunk), where the folded formats cost O(table) for their body string.
pub struct SqlDump {
    pub columns: Vec<String>,
    pub rows: i64,
    pub capped: bool,
    pub body: tokio::sync::mpsc::Receiver<DumpPiece>,
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
#[derive(Clone, Debug, PartialEq)]
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
    /// Stream the same table out as a SQL dump — the format=sql arm of the export (docs/22
    /// W4.4): the CREATE TABLE head (the same DDL path describe_table uses), then the rows as
    /// ~1 MB multi-value INSERTs. The head facts come back before the first byte ships; the
    /// body arrives as channel pieces so the dump never materializes whole.
    async fn export_sql_dump(&self, o: &Value) -> Result<SqlDump, String>;
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
                // A "*" in the value is the user's own wildcard (docs/22 W1.6): same escaping
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
/// docs/22 W1.9: the fetch-one-more paging probe. The adapter asks for `limit + 1` rows; a
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

/// How one edit's committed row comes home with the batch reply (docs/22 W1.7). The point is
/// showing what the SERVER kept — silent truncation, column DEFAULTs, trigger rewrites —
/// instead of the value the panel sent. Three shapes:
///
/// - an insert on Postgres rides its own INSERT (a RETURNING suffix carries the row out);
/// - an insert on MySQL reads back by LAST_INSERT_ID() in the same transaction;
/// - an update selects its row by primary key (the edit already carries those values).
///
/// Deletes read nothing back — the row is gone by design — and a table without a usable key
/// reads nothing back either (W4.1 makes those editable later).
#[derive(Clone, Debug)]
pub enum ReadBack {
    /// The suffix to append to the edit's own INSERT; its parameters are unchanged.
    Returning(String),
    /// A same-transaction SELECT. Its parameters are the edit's primary-key values — or empty
    /// when the SQL carries LAST_INSERT_ID() itself.
    Select(BuiltStatement),
    /// No row to read (a delete, or no key to address one by).
    None,
}

/// Read-back column list + addressing for one edit. `columns` is the full table column set (the
/// row should come back shaped like a grid row); `pk` the table's primary-key column names.
/// Identifier quoting goes through the same whitelist as every other statement here, so an
/// unpromising name downgrades to ReadBack::None rather than poisoning the commit.
pub fn readback_plan(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    columns: &[String],
    pk: &[String],
    edit: &BrowseEdit,
) -> ReadBack {
    let cols = || -> Option<String> {
        columns
            .iter()
            .map(|c| quote_ident(dialect, c))
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .map(|v| v.join(", "))
    };
    let target = || -> Option<String> {
        let t = quote_ident(dialect, table).ok()?;
        Some(match schema {
            Some(s) => format!("{}.{t}", quote_ident(dialect, s).ok()?),
            None => t,
        })
    };
    match edit {
        BrowseEdit::Delete { .. } => ReadBack::None,
        BrowseEdit::Insert { .. } if dialect == DbDialect::Pg => match cols() {
            Some(list) => ReadBack::Returning(format!(" RETURNING {list}")),
            None => ReadBack::None,
        },
        BrowseEdit::Insert { .. } => {
            // MySQL has no RETURNING: the auto-generated key of the statement that just ran is
            // addressable as LAST_INSERT_ID() on the same connection, so the read-back SELECT
            // needs no parameters at all. A composite key with an auto column cannot be
            // addressed this way — those inserts read nothing back.
            let (Some(list), Some(target), true) = (cols(), target(), pk.len() == 1) else {
                return ReadBack::None;
            };
            let Some(k) = quote_ident(dialect, &pk[0]).ok() else {
                return ReadBack::None;
            };
            ReadBack::Select(BuiltStatement {
                sql: format!("SELECT {list} FROM {target} WHERE {k} = LAST_INSERT_ID()"),
                params: Vec::new(),
            })
        }
        BrowseEdit::Update { pk: values, .. } => {
            let (Some(list), Some(target)) = (cols(), target()) else {
                return ReadBack::None;
            };
            let mut where_parts: Vec<String> = Vec::with_capacity(pk.len());
            let mut params: Vec<Value> = Vec::with_capacity(pk.len());
            for k in pk {
                let Some(v) = values.get(k) else {
                    return ReadBack::None; // the edit cannot address its own row
                };
                let Some(quoted) = quote_ident(dialect, k).ok() else {
                    return ReadBack::None;
                };
                params.push(v.clone());
                where_parts.push(if dialect == DbDialect::Mysql {
                    format!("{quoted} = ?")
                } else {
                    format!("{quoted} = ${}", params.len())
                });
            }
            if where_parts.is_empty() {
                return ReadBack::None; // no key columns: nothing to select by
            }
            ReadBack::Select(BuiltStatement {
                sql: format!("SELECT {list} FROM {target} WHERE {}", where_parts.join(" AND ")),
                params,
            })
        }
    }
}

/// Turn a buffered edit list into executable statements: one statement per edit, in order, with
/// every value bound and every identifier pre-vetted. Column names the table does not have are
/// dropped from SET/INSERT (a stale page may reference a dropped column); an edit that ends up
/// with nothing to do is an error, because "commit 3 changes" that silently did 2 is a lie.
///
/// The INSERT for one map of values — the shared body of the edit grid's insert arm and both
/// import modes (docs/22 W4.5): the same known-column filter, the same placeholder typing, the
/// same column order (the map's own), so a row commits as the same statement wherever it came
/// from. Returns the columns it mapped (an upsert tail addresses them), the SQL without any
/// suffix, and the bound parameters in SQL order.
fn insert_half(
    dialect: DbDialect,
    table_ref: &str,
    values: &Map<String, Value>,
    known: &HashSet<&str>,
    type_of: &HashMap<&str, &str>,
) -> Result<(Vec<String>, String, Vec<Value>), String> {
    // Object.keys order — the panel's own insertion order.
    let cols: Vec<&String> = values
        .keys()
        .filter(|c| known.contains(c.as_str()))
        .collect();
    if cols.is_empty() {
        return Err("insert names no column of this table".into());
    }
    let mut params: Vec<Value> = Vec::new();
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
    Ok((
        cols.into_iter().cloned().collect(),
        format!(
            "INSERT INTO {table_ref} ({names}) VALUES ({})",
            value_sql.join(", ")
        ),
        params,
    ))
}

/// The import modes (docs/22 W4.5): insert keeps today's behavior; upsert turns every row's
/// INSERT into a conflict-aware statement so re-importing a file lands instead of key-clashing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Insert,
    Upsert,
}

/// Read the `mode` option: absent, null and empty stay the insert default; anything but the two
/// known spellings is the caller's whole mistake.
pub fn parse_import_mode(v: Option<&Value>) -> Result<ImportMode, String> {
    let empty = matches!(v, None | Some(Value::Null))
        || matches!(v, Some(Value::String(s)) if s.is_empty());
    if empty {
        return Ok(ImportMode::Insert);
    }
    match v {
        Some(Value::String(s)) if s == "insert" => Ok(ImportMode::Insert),
        Some(Value::String(s)) if s == "upsert" => Ok(ImportMode::Upsert),
        Some(other) => Err(format!(
            "mode must be insert or upsert, not {}",
            js_to_string(Some(other))
        )),
        None => Ok(ImportMode::Insert),
    }
}

/// The conflict tail of an upsert import (docs/22 W4.5), adminer's insertUpdate per dialect
/// (`drivers/mysql.inc.php:292`, `drivers/pgsql.inc.php:340`): MySQL maps every column the row
/// carries (`col` = VALUES(col)) — a table without any unique key simply never conflicts;
/// Postgres targets the table's primary key and updates the non-key columns from EXCLUDED,
/// DO NOTHING when the row carries nothing but key columns. An Err is Postgres without a
/// primary key — ON CONFLICT has no target there, and the caller degrades to plain inserts
/// with a note instead of failing the whole file.
pub fn upsert_suffix(
    dialect: DbDialect,
    row_columns: &[String],
    primary_key: &[String],
) -> Result<String, String> {
    match dialect {
        DbDialect::Mysql => {
            let sets: Result<Vec<String>, String> = row_columns
                .iter()
                .map(|c| {
                    let q = quote_ident(dialect, c)?;
                    Ok(format!("{q} = VALUES({q})"))
                })
                .collect();
            Ok(format!(" ON DUPLICATE KEY UPDATE {}", sets?.join(", ")))
        }
        DbDialect::Pg => {
            if primary_key.is_empty() {
                return Err(
                    "table has no primary key — Postgres has no ON CONFLICT target".into(),
                );
            }
            let conflict = primary_key
                .iter()
                .map(|c| quote_ident(dialect, c))
                .collect::<Result<Vec<_>, _>>()?;
            let key: HashSet<&str> = primary_key.iter().map(String::as_str).collect();
            let updates: Result<Vec<String>, String> = row_columns
                .iter()
                .filter(|c| !key.contains(c.as_str()))
                .map(|c| {
                    let q = quote_ident(dialect, c)?;
                    Ok(format!("{q} = EXCLUDED.{q}"))
                })
                .collect();
            let updates = updates?;
            Ok(if updates.is_empty() {
                format!(" ON CONFLICT ({}) DO NOTHING", conflict.join(", "))
            } else {
                format!(
                    " ON CONFLICT ({}) DO UPDATE SET {}",
                    conflict.join(", "),
                    updates.join(", ")
                )
            })
        }
    }
}

/// Import rows → statements (docs/22 W4.5). Insert mode produces exactly the statements the
/// edit grid's insert arm builds, one per row, by construction. Upsert appends the conflict
/// tail per row (the columns a row carries vary — empty CSV cells drop). A table that cannot
/// upsert (Postgres without a primary key) comes back in INSERT form plus the note that says
/// why, so the file still lands in one transaction instead of failing on statement one.
pub fn build_import_statements(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    rows: &[Map<String, Value>],
    columns: &[BrowseColumn],
    primary_key: &[String],
    mode: ImportMode,
) -> Result<(Vec<BuiltStatement>, Option<&'static str>), String> {
    let table_ref = qualified(dialect, schema, table)?;
    let known: HashSet<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    let type_of: HashMap<&str, &str> = columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str()))
        .collect();
    let degraded =
        if mode == ImportMode::Upsert && dialect == DbDialect::Pg && primary_key.is_empty() {
            Some("table has no primary key — upsert is not possible on Postgres, rows were inserted instead")
        } else {
            None
        };
    let upsert = mode == ImportMode::Upsert && degraded.is_none();
    let mut out: Vec<BuiltStatement> = Vec::with_capacity(rows.len());
    for row in rows {
        let (cols, sql, params) = insert_half(dialect, &table_ref, row, &known, &type_of)?;
        let suffix = if upsert {
            upsert_suffix(dialect, &cols, primary_key)?
        } else {
            String::new()
        };
        out.push(BuiltStatement {
            sql: format!("{sql}{suffix}"),
            params,
        });
    }
    Ok((out, degraded))
}

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
                let (_, sql, insert_params) =
                    insert_half(dialect, &table_ref, values, &known, &type_of)?;
                out.push(BuiltStatement {
                    sql,
                    params: insert_params,
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
        lines.push(line);
    }
    if !primary_key.is_empty() {
        let cols = primary_key
            .iter()
            .map(|c| quote_ident(DbDialect::Pg, c))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!("    PRIMARY KEY ({})", cols.join(", ")));
    }
    for fk in foreign_keys {
        lines.push(format!(
            "    FOREIGN KEY ({}) REFERENCES {}.{} ({})",
            quote_ident(DbDialect::Pg, &fk.column)?,
            quote_ident(DbDialect::Pg, &fk.ref_schema)?,
            quote_ident(DbDialect::Pg, &fk.ref_table)?,
            quote_ident(DbDialect::Pg, &fk.ref_column)?
        ));
    }
    // Lines join with their own commas — the last one must not carry one into the closing
    // paren, or the sketch (and any SQL dump that replays it, docs/22 W4.4) is invalid SQL.
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", lines.join(",\n"))
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

/// One SQL literal for a DUMP BODY — the dump is EXECUTED on replay (docs/22 W4.4), so the
/// escaping has to hold as real SQL, not just as display. The clipboard's sql_literal only
/// doubles quotes and must never be used here: a trailing backslash in a MySQL value swallowed
/// the closing quote and let an ordinary string break (or inject into) the replay — an audit
/// blocker. MySQL escapes exactly what adminer's real_escape_string escapes
/// ("drivers/mysql.inc.php" q(): backslash, single quote, double quote, LF, CR, NUL, Ctrl-Z);
/// Postgres doubles single quotes (its text type holds every other byte raw) and REFUSES a
/// NUL — emitting the raw byte would corrupt the dump, and a clear error lets the caller stop.
pub fn sql_dump_literal(dialect: DbDialect, v: Option<&Value>) -> Result<String, String> {
    let Some(v) = v else {
        return Ok("NULL".into());
    };
    match v {
        Value::Null => Ok("NULL".into()),
        Value::Number(n) => Ok(number_to_js_string(n)),
        Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
        other => {
            let s = js_to_string(Some(other));
            let mut out = String::with_capacity(s.len() + 2);
            out.push('\'');
            match dialect {
                DbDialect::Mysql => {
                    for c in s.chars() {
                        match c {
                            '\\' => out.push_str("\\\\"),
                            '\'' => out.push_str("\\'"),
                            '"' => out.push_str("\\\""),
                            '\n' => out.push_str("\\n"),
                            '\r' => out.push_str("\\r"),
                            '\0' => out.push_str("\\0"),
                            '\u{1a}' => out.push_str("\\Z"),
                            _ => out.push(c),
                        }
                    }
                }
                DbDialect::Pg => {
                    if s.contains('\0') {
                        return Err(
                            "a Postgres text value holds a NUL byte — the dump cannot express it"
                                .into(),
                        );
                    }
                    for c in s.chars() {
                        if c == '\'' {
                            out.push('\'');
                        }
                        out.push(c);
                    }
                }
            }
            out.push('\'');
            Ok(out)
        }
    }
}

/// One SQL literal for the clipboard — never executed, so inlining is safe here. Text that
/// WILL be executed (a dump body, a replayed statement) belongs to sql_dump_literal instead.
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

// --- streaming SQL dump (docs/22 W4.4) ------------------------------------------------------------

/// Accumulates multi-value INSERT rows for a streaming SQL dump, adminer's dumpData shape
/// (`adminer.inc.php:1007-1070`): one `INSERT INTO … VALUES` prefix, each row appended behind
/// a newline, the statement flushed whole the moment one more row would cross
/// [`EXPORT_SQL_MAX_PACKET`]. Nothing but the statement under construction is held, so a dump
/// of any capped size costs O(chunk), never O(table).
pub struct SqlInsertBatch {
    prefix: String,
    buffer: String,
    /// prefix + buffer — adminer's strlen($buffer), which always carried its own prefix.
    used: usize,
}

impl SqlInsertBatch {
    /// The prefix vets and quotes every identifier the way the edit grid's statements do, so
    /// what the dump replays is what Commit would have run.
    pub fn new(
        dialect: DbDialect,
        schema: Option<&str>,
        table: &str,
        columns: &[String],
    ) -> Result<Self, String> {
        let names = columns
            .iter()
            .map(|c| quote_ident(dialect, c))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        Ok(Self {
            prefix: format!(
                "INSERT INTO {} ({names}) VALUES",
                qualified(dialect, schema, table)?
            ),
            buffer: String::new(),
            used: 0,
        })
    }

    /// Add one row's value literals ("(1, 'a')", no leading newline). Some(statement) when
    /// the row did not fit and the statement so far must ship before this row opens the next
    /// one; a row larger than the threshold alone still joins — rows are never split.
    pub fn push(&mut self, row_sql: &str) -> Option<String> {
        // The fit check is adminer's (`adminer.inc.php:1058`): used + 4 (length-spec
        // headroom) + the row with its leading newline + the ";\n" every statement ends with.
        let piece = row_sql.len() + 1 + ";\n".len();
        if self.used != 0 && self.used + 4 + piece >= EXPORT_SQL_MAX_PACKET {
            let out = format!("{}{};\n", self.prefix, self.buffer);
            self.buffer = format!("\n{row_sql}");
            self.used = self.prefix.len() + self.buffer.len();
            return Some(out);
        }
        if self.buffer.is_empty() {
            self.buffer.push('\n');
        } else {
            self.buffer.push_str(",\n");
        }
        self.buffer.push_str(row_sql);
        self.used = self.prefix.len() + self.buffer.len();
        None
    }

    /// The last, not-yet-full statement — the dump's closing INSERT when any rows remain.
    pub fn finish(self) -> Option<String> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(format!("{}{};\n", self.prefix, self.buffer))
        }
    }
}

/// The preamble of a streaming SQL dump (docs/22 W4.4): a provenance comment, the dialect's
/// foreign-key stance, and the CREATE TABLE from the same DDL path the Structure tab uses.
/// MySQL's `SET FOREIGN_KEY_CHECKS=0` lets the replay session load rows before any referenced
/// table exists (the footer restores it); Postgres has no session switch, so a comment states
/// the ordering contract instead. `ddl` arrives exactly as describe_table served it.
pub fn sql_dump_head(
    dialect: DbDialect,
    schema: Option<&str>,
    table: &str,
    ddl: &str,
) -> Result<String, String> {
    let target = qualified(dialect, schema, table)?;
    // SHOW CREATE TABLE carries no terminator; build_pg_ddl already ends with ';'.
    let mut ddl = ddl.trim_end().to_string();
    if !ddl.is_empty() && !ddl.ends_with(';') {
        ddl.push(';');
    }
    let stance = if dialect == DbDialect::Mysql {
        format!(
            "-- {target}: foreign-key checks are off for the replay session\n\
             SET FOREIGN_KEY_CHECKS=0;\n"
        )
    } else {
        format!("-- {target}: Postgres has no session FK switch - replay in dependency order\n")
    };
    Ok(format!("-- swiss SQL dump\n{stance}\n{ddl}\n"))
}

/// The closing statement of a dump: MySQL restores the FK checks the head disabled; Postgres
/// needs nothing.
pub fn sql_dump_foot(dialect: DbDialect) -> &'static str {
    if dialect == DbDialect::Mysql {
        "SET FOREIGN_KEY_CHECKS=1;\n"
    } else {
        ""
    }
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
    fn grep_patterns_parse_the_grammar_and_keep_plain_substrings_alone() {
        // docs/22 W1.6: comma terms AND, | OR, * wildcard. A grep without any of the three
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
        let w = grep_where(
            DbDialect::Pg,
            "c.relname",
            "user*|account",
            1,
        )
        .unwrap()
        .expect("grammar");
        assert_eq!(
            w.frag,
            " AND (c.relname ILIKE $2 ESCAPE '!' OR c.relname ILIKE $3 ESCAPE '!')"
        );
        assert_eq!(w.params, vec![json!("user%"), json!("%account%")]);
        // MySQL binds positionally and LIKE is case-insensitive under the default collation.
        let w = grep_where(
            DbDialect::Mysql,
            "table_name",
            "user*|account",
            0,
        )
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
        // docs/22 W1.6: inside a contains-filter value, * means "any run" — translated to LIKE's
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
    fn page_and_next_truncates_the_probe_row() {
        // docs/22 W1.9: limit+1 fetched, the spare row reported as nextPage and dropped.
        let row = || -> Map<String, Value> {
            [("id", 1)].iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
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
    fn readback_plan_addresses_every_committed_row() {
        // docs/22 W1.7: the commit reply carries what the SERVER kept, not what was sent.
        let vmap = |pairs: &[(&str, i64)]| -> Map<String, Value> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect()
        };
        let columns = vec!["id".to_string(), "name".to_string()];
        let pk = vec!["id".to_string()];
        let upd = BrowseEdit::Update {
            pk: vmap(&[("id", 7)]),
            changes: vmap(&[("name", 0)]),
        };
        match readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &upd) {
            ReadBack::Select(s) => {
                assert_eq!(s.sql, "SELECT `id`, `name` FROM `users` WHERE `id` = ?");
                assert_eq!(s.params, vec![json!(7)]);
            }
            other => panic!("mysql update selects its row back: {other:?}"),
        }
        match readback_plan(DbDialect::Pg, Some("app"), "users", &columns, &pk, &upd) {
            ReadBack::Select(s) => {
                assert_eq!(
                    s.sql,
                    "SELECT \"id\", \"name\" FROM \"app\".\"users\" WHERE \"id\" = $1"
                );
            }
            other => panic!("pg update selects its row back: {other:?}"),
        }
        let ins = BrowseEdit::Insert {
            values: vmap(&[("name", 0)]),
        };
        match readback_plan(DbDialect::Pg, None, "users", &columns, &pk, &ins) {
            ReadBack::Returning(suffix) => {
                assert_eq!(suffix, " RETURNING \"id\", \"name\"");
            }
            other => panic!("pg insert rides RETURNING: {other:?}"),
        }
        match readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &ins) {
            ReadBack::Select(s) => {
                assert_eq!(
                    s.sql,
                    "SELECT `id`, `name` FROM `users` WHERE `id` = LAST_INSERT_ID()"
                );
                assert!(s.params.is_empty());
            }
            other => panic!("mysql insert reads LAST_INSERT_ID back: {other:?}"),
        }
        let del = BrowseEdit::Delete {
            pk: vmap(&[("id", 7)]),
        };
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &columns, &pk, &del),
            ReadBack::None
        ));
        // No key to address the row by: nothing comes back, the commit itself is unaffected.
        assert!(matches!(
            readback_plan(DbDialect::Mysql, None, "users", &columns, &[], &upd),
            ReadBack::None
        ));
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
        // docs/22 W4.4 live-verify caught this once: a trailing comma after the last clause
        // made the sketch (and any SQL dump that replays it) invalid SQL. The tail must be
        // the clause, then the paren — never a comma between them.
        assert!(ddl.ends_with("    FOREIGN KEY (\"id\") REFERENCES \"public\".\"users\" (\"id\")\n);"));
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

    // --- streaming SQL dump (docs/22 W4.4) ------------------------------------------------------------

    #[test]
    fn sql_dump_head_and_foot_snapshots() {
        // MySQL: the FK switch brackets the dump so a replay session can load rows before
        // referenced tables exist; the DDL arrives from SHOW CREATE TABLE with no ';'.
        let head = sql_dump_head(
            DbDialect::Mysql,
            Some("app"),
            "users",
            "CREATE TABLE `users` (\n  `id` int\n)",
        )
        .unwrap();
        assert_eq!(
            head,
            "-- swiss SQL dump\n\
             -- `app`.`users`: foreign-key checks are off for the replay session\n\
             SET FOREIGN_KEY_CHECKS=0;\n\
             \n\
             CREATE TABLE `users` (\n  `id` int\n);\n"
        );
        // Postgres: no session switch exists, so the comment says the ordering contract; the
        // catalog-sketch DDL already ends with ';' and must not gain a second one.
        let pg = sql_dump_head(
            DbDialect::Pg,
            Some("public"),
            "t",
            "CREATE TABLE \"public\".\"t\" (\n    \"id\" integer NOT NULL\n);",
        )
        .unwrap();
        assert_eq!(
            pg,
            "-- swiss SQL dump\n\
             -- \"public\".\"t\": Postgres has no session FK switch - replay in dependency order\n\
             \n\
             CREATE TABLE \"public\".\"t\" (\n    \"id\" integer NOT NULL\n);\n"
        );
        assert_eq!(sql_dump_foot(DbDialect::Mysql), "SET FOREIGN_KEY_CHECKS=1;\n");
        assert_eq!(sql_dump_foot(DbDialect::Pg), "");
    }

    #[test]
    fn sql_insert_batches_rows_into_multi_value_statements() {
        let mut batch =
            SqlInsertBatch::new(DbDialect::Mysql, Some("app"), "users", &["id".into(), "name".into()])
                .unwrap();
        assert_eq!(batch.push("(1, 'a')"), None);
        assert_eq!(batch.push("(2, NULL)"), None);
        assert_eq!(
            batch.finish().unwrap(),
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES\n(1, 'a'),\n(2, NULL);\n"
        );
    }

    #[test]
    fn a_sql_insert_batch_flushes_when_the_next_row_would_cross_the_cap() {
        // adminer's threshold at work: two ~700 KB rows cannot share one statement under a
        // 1 MB cap — the first ships whole, the second opens the next statement, and neither
        // statement is ever split mid-row.
        let big = format!("('{}')", "x".repeat(700_000));
        let mut batch = SqlInsertBatch::new(DbDialect::Pg, None, "t", &["pad".into()]).unwrap();
        assert_eq!(batch.push(&big), None);
        let shipped = batch.push(&big).expect("second ~700 KB row must flush the first");
        assert!(shipped.starts_with("INSERT INTO \"t\" (\"pad\") VALUES\n('"));
        assert!(shipped.ends_with(";\n"));
        assert!(shipped.len() < EXPORT_SQL_MAX_PACKET + big.len());
        let tail = batch.finish().unwrap();
        assert!(tail.starts_with("INSERT INTO \"t\" (\"pad\") VALUES\n('"));
        // Neither statement crossed the cap by more than the one row that always fits.
        assert!(tail.len() < EXPORT_SQL_MAX_PACKET + big.len());
    }

    #[test]
    fn an_empty_sql_insert_batch_finishes_into_nothing() {
        let batch = SqlInsertBatch::new(DbDialect::Mysql, None, "t", &["a".to_string()]).unwrap();
        assert_eq!(batch.finish(), None);
    }

    // --- import upsert (docs/22 W4.5) ----------------------------------------------------------------

    #[test]
    fn upsert_tails_match_adminer_per_dialect() {
        let cols = vec!["id".to_string(), "name".to_string()];
        // MySQL maps every column the row carries — adminer's ON DUPLICATE KEY UPDATE.
        assert_eq!(
            upsert_suffix(DbDialect::Mysql, &cols, &["id".to_string()]).unwrap(),
            " ON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
        // MySQL needs no primary key: ON DUPLICATE KEY UPDATE rides any unique key.
        assert!(upsert_suffix(DbDialect::Mysql, &cols, &[]).is_ok());
        // Postgres targets the primary key and updates the rest from EXCLUDED.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &cols, &["id".to_string()]).unwrap(),
            " ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
        // A row that carries nothing but key columns has nothing to update — DO NOTHING.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &["id".to_string()], &["id".to_string()]).unwrap(),
            " ON CONFLICT (\"id\") DO NOTHING"
        );
        // A composite key targets every key column, in table order; mapped non-key columns
        // all update.
        assert_eq!(
            upsert_suffix(DbDialect::Pg, &cols, &["a".to_string(), "b".to_string()]).unwrap(),
            " ON CONFLICT (\"a\", \"b\") DO UPDATE SET \"id\" = EXCLUDED.\"id\", \"name\" = EXCLUDED.\"name\""
        );
        // Postgres without a primary key has no conflict target at all.
        assert!(upsert_suffix(DbDialect::Pg, &cols, &[]).is_err());
    }

    #[test]
    fn import_insert_mode_is_the_edit_grids_insert_arm_byte_for_byte() {
        let rows = vec![
            row(&[("id", json!(1)), ("name", json!("alice"))]),
            row(&[("id", json!(2)), ("name", json!("bob"))]),
        ];
        let edits = rows
            .iter()
            .cloned()
            .map(|values| BrowseEdit::Insert { values })
            .collect::<Vec<_>>();
        let cols = stub_columns();
        let pk = vec!["id".to_string()];
        for dialect in [DbDialect::Mysql, DbDialect::Pg] {
            let via_import =
                build_import_statements(dialect, Some("app"), "users", &rows, &cols, &pk, ImportMode::Insert)
                    .unwrap();
            let via_grid = build_edit_statements(dialect, Some("app"), "users", &edits, &cols, &pk)
                .unwrap();
            assert_eq!(via_import.0, via_grid, "{dialect:?}");
            assert_eq!(via_import.1, None);
        }
    }

    #[test]
    fn upsert_imports_carry_the_conflict_tail() {
        let rows = vec![row(&[("id", json!(1)), ("name", json!("alice"))])];
        let cols = stub_columns();
        let pk = vec!["id".to_string()];
        let (out, degraded) =
            build_import_statements(DbDialect::Mysql, Some("app"), "users", &rows, &cols, &pk, ImportMode::Upsert)
                .unwrap();
        assert_eq!(degraded, None);
        assert_eq!(
            out[0].sql,
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `id` = VALUES(`id`), `name` = VALUES(`name`)"
        );
        assert_eq!(out[0].params, vec![json!(1), json!("alice")]);

        let (out, degraded) =
            build_import_statements(DbDialect::Pg, Some("public"), "users", &rows, &cols, &pk, ImportMode::Upsert)
                .unwrap();
        assert_eq!(degraded, None);
        assert_eq!(
            out[0].sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES (CAST($1 AS int), CAST($2 AS text)) ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
    }

    // --- dump literal escaping (docs/22 W4.4 audit blocker) ---------------------------------------

    #[test]
    fn mysql_dump_literals_escape_everything_adminer_does() {
        // THE audit blocker case: a trailing backslash swallowed the closing quote under the
        // clipboard escaping, so an ordinary Windows path broke (or injected into) the replay.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("C:\\path\\"))).unwrap(),
            "'C:\\\\path\\\\'"
        );
        // Quotes, newlines: every character real_escape_string would not let through raw.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("it's \"quoted\"\nline\r end"))).unwrap(),
            "'it\\'s \\\"quoted\\\"\\nline\\r end'"
        );
        // NUL and Ctrl-Z (0x1A) — the two control bytes the Windows MySQL client chokes on.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("a\0b\u{1a}c"))).unwrap(),
            "'a\\0b\\Zc'"
        );
        // Unicode passes through untouched — the bytes replay as themselves.
        assert_eq!(
            sql_dump_literal(DbDialect::Mysql, Some(&json!("\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}"))).unwrap(),
            "'\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}'"
        );
        // Numbers, bools and nulls keep the plain shapes the dump always used.
        assert_eq!(sql_dump_literal(DbDialect::Mysql, Some(&json!(7))).unwrap(), "7");
        assert_eq!(sql_dump_literal(DbDialect::Mysql, Some(&json!(1.5))).unwrap(), "1.5");
        assert_eq!(sql_dump_literal(DbDialect::Mysql, Some(&json!(true))).unwrap(), "true");
        assert_eq!(sql_dump_literal(DbDialect::Mysql, None).unwrap(), "NULL");
        assert_eq!(sql_dump_literal(DbDialect::Mysql, Some(&Value::Null)).unwrap(), "NULL");
    }

    #[test]
    fn pg_dump_literals_double_quotes_and_refuse_nul() {
        // Postgres text holds quotes, double quotes, newlines and backslashes raw — only the
        // single quote doubles (the dump's head does not opt into escape-string syntax).
        assert_eq!(
            sql_dump_literal(DbDialect::Pg, Some(&json!("it's \"x\"\n\\ line"))).unwrap(),
            "'it''s \"x\"\n\\ line'"
        );
        assert_eq!(
            sql_dump_literal(DbDialect::Pg, Some(&json!("\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}"))).unwrap(),
            "'\u{4e2d}\u{6587} \u{3b1}\u{3b2}\u{3b3}'"
        );
        // A NUL cannot live in a Postgres text value; refusing it beats emitting the raw byte
        // into a dump that would then be corrupt from that row on.
        let err = sql_dump_literal(DbDialect::Pg, Some(&json!("a\0b"))).unwrap_err();
        assert!(err.contains("NUL"), "{err}");
        assert_eq!(sql_dump_literal(DbDialect::Pg, Some(&json!(9))).unwrap(), "9");
        assert_eq!(sql_dump_literal(DbDialect::Pg, None).unwrap(), "NULL");
    }

    #[test]
    fn a_postgres_upsert_without_a_pk_degrades_to_insert_with_a_note() {
        let rows = vec![row(&[("id", json!(1)), ("name", json!("alice"))])];
        let cols = stub_columns();
        let (out, degraded) = build_import_statements(
            DbDialect::Pg,
            Some("public"),
            "users",
            &rows,
            &cols,
            &[],
            ImportMode::Upsert,
        )
        .unwrap();
        let note = degraded.expect("Postgres without a pk must say why it degraded");
        assert!(note.contains("no primary key"), "{note}");
        assert!(note.contains("inserted instead"), "{note}");
        // The statements are plain inserts — the file still lands, in the same transaction.
        assert_eq!(
            out[0].sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES (CAST($1 AS int), CAST($2 AS text))"
        );
    }
}
