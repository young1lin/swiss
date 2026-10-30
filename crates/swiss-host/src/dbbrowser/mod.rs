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
//!
//! The model is split by concern, one file each; every public item is re-exported here, so a
//! caller names `dbbrowser::X` whichever file X lives in:
//!
//! - `model` - the shapes: columns, tables, indexes, edits and their conflicts, filters, built statements.
//! - `values` - JSON values the way the Node build coerced them, and exact 64-bit integers on the wire.
//! - `ident` - identifier checks and per-dialect quoting.
//! - `paging` - page and offset clamps, ordering, the row and count statements.
//! - `filter` - the filter list and the grep line into one parameterised WHERE.
//! - `redis` - stream windows and filters, console word splitting, pipeline bodies, the pub/sub watch set.
//! - `address` - addressing a row that has no primary key.
//! - `edit` - the edit list into statements, and reading committed rows back.
//! - `import` - CSV import and upserts.
//! - `ddl` - table structure from the catalogs and the minimal DDL set.
//! - `export` - CSV and JSON-lines export, SQL literals, the streaming SQL dump.
//! - `console` - the SQL console's EXPLAIN prefix and server-side completion.
//! - `activity` - the session list and the kill statements.

mod model;
mod values;
mod ident;
mod paging;
mod filter;
mod redis;
mod address;
mod edit;
mod import;
mod ddl;
mod export;
mod console;
mod activity;

#[cfg(test)]
mod test_util;

pub use self::model::*;
pub use self::values::*;
pub use self::ident::*;
pub use self::paging::*;
pub use self::filter::*;
pub use self::redis::*;
pub use self::address::*;
pub use self::edit::*;
pub use self::import::*;
pub use self::ddl::*;
pub use self::export::*;
pub use self::console::*;
pub use self::activity::*;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

/// The browser flavor one registered MCP exposes. Carried by the connection catalog so a
/// consumer can browse a leased connection without knowing the adapter that built it — the
/// reason this type lives HERE (the host's vocabulary) and not in the data routes.
pub enum BrowserFlavor {
    Db(Arc<dyn DbBrowser>),
    Redis(Arc<dyn RedisBrowser>),
    /// The document flavour (SPEC §data.mongo): collections and documents, not tables and rows.
    Mongo(Arc<dyn crate::mongobrowser::MongoBrowser>),
    /// Registered but nothing to browse (echo / proc / http / rest). Present so the lookup
    /// errors can name the adapter type exactly as Node's did; GET /api/db skips these rows.
    None,
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
    /// Apply a buffered edit list in ONE transaction: all of it, or none of it. The
    /// error says whose fault it is (SPEC §data.edits): Bad is the caller's request, Conflict
    /// is another writer having moved a row the buffer claimed to still see.
    async fn apply_edits(&self, o: &Value) -> Result<Value, EditError>;
    /// The SQL console: one statement per run, reads and writes alike — this console belongs
    /// to the panel on the operator's own machine.
    async fn run_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String>;
    /// The Data agent's read (SPEC §data.agent): ONE read statement (the dialect's read gate
    /// refuses anything else) inside a read-only transaction that is always rolled back, so
    /// the server itself refuses a write the gate misread. The reply is run_query's shape.
    /// The default refuses: a dialect without it gives the agent no reads.
    async fn run_read_query(&self, sql: &str, limit: Option<&Value>) -> Result<Value, String> {
        let _ = (sql, limit);
        Err("read-only queries are not supported by this connection".into())
    }
    /// The Data agent's write in auto mode (SPEC §data.agent Writes): ONE statement the auto
    /// gate passes (swiss-mcp sql::assert_auto_write), in a read-write transaction committed
    /// only when it changed at most `cap` rows - past that it is rolled back and held. The
    /// reply is run_query's shape with affectedRows. The default holds every write: a dialect
    /// without it leaves them all to the user.
    async fn run_agent_write(&self, sql: &str, cap: u64) -> Result<Value, WriteRefusal> {
        let _ = (sql, cap);
        Err(WriteRefusal::Held("writes do not run on their own on this connection".into()))
    }
    /// Stream a whole table (capped at EXPORT_ROW_CAP) out as CSV or newline JSON.
    async fn export_table(&self, o: &Value) -> Result<Value, String>;
    /// Stream the same table out as a SQL dump — the format=sql arm of the export (SPEC §data.export):
    /// the CREATE TABLE head (the same DDL path describe_table uses), then the rows as
    /// ~1 MB multi-value INSERTs. The head facts come back before the first byte ships; the
    /// body arrives as channel pieces so the dump never materializes whole.
    async fn export_sql_dump(&self, o: &Value) -> Result<SqlDump, String>;
    /// Insert mapped CSV rows in ONE transaction (all-or-nothing), via the same statement
    /// builders the edit grid uses. Returns rows written.
    async fn import_table(&self, o: &Value) -> Result<Value, String>;
    /// Rename / truncate / drop a table.
    async fn ddl_op(&self, o: &Value) -> Result<Value, String>;
    /// Live sessions on this connection's server (SPEC §data.activity) — the Activity page, polled
    /// while open. Rows speak the shared shape {pid, user, state, wait, seconds, query, own,
    /// blockedBy?} so one panel table fits both dialects.
    async fn activity(&self) -> Result<Value, String>;
    /// Cancel (mode "cancel") or terminate one session by pid. The route validated the mode;
    /// the browser still refuses a pid that cannot name a session.
    async fn activity_kill(&self, pid: i64, terminate: bool) -> Result<Value, String>;
    /// Server-side SQL completion (SPEC §data.completion): candidates for the word ending at the
    /// caret — dialect keywords + table names + the FROM-nearest table's columns. `caret` is
    /// a byte offset into `sql`.
    async fn completion(&self, sql: &str, caret: usize) -> Result<Value, String>;
    /// The database axis of this connection (SPEC §data.databases): the configured primary, the
    /// database the pool actually sits on, and every other database the instance will name,
    /// each entry honestly marking whether it is browsable on THIS connection and why not.
    /// The DEFAULT is the empty catalog — a dialect with no database axis (and every test
    /// stub) returns it unchanged, so adding the route forced no edits anywhere else.
    async fn list_databases(&self) -> Result<Value, String> {
        Ok(json!({ "primary": null, "current": null, "databases": [] }))
    }
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
    /// The Data agent's read (SPEC §data.agent): ONE command line the adapter's read gate
    /// passes - an allowlist of reads - then the console's own guard; the reply in
    /// run_command's shape. The default refuses: a flavour without it gives the agent no reads.
    async fn run_read_command(&self, line: &str) -> Result<Value, String> {
        let _ = line;
        Err("read-only commands are not supported by this connection".into())
    }
    /// Run a buffered structured-edit batch as ONE pipelined round trip (SPEC §data.redis). Every
    /// command passes the SAME guard the one-command console applies — checked per command,
    /// before the socket is touched — and the first refusal rejects the whole batch, so a
    /// buffered edit never half-applies. No MULTI: the panel's edits are field-addressed and
    /// safe to re-run, and a plain pipeline keeps every reply individual and honest.
    async fn run_pipeline(&self, commands: &[Vec<String>]) -> Result<Value, String>;
    /// The redis twin of DbBrowser::list_databases (SPEC §data.databases): INFO keyspace names every
    /// dbN with its key count, CLIENT INFO says which one this connection sits on. The
    /// default (empty catalog) serves every stub and any future flavor that has no axis.
    async fn list_databases(&self) -> Result<Value, String> {
        Ok(json!({ "primary": null, "current": null, "databases": [] }))
    }
    /// SPEC §data.streams: a newest-first window of one stream key — the opening page,
    /// the page strictly older than a cursor (“load earlier”), or the page strictly
    /// newer than one (the Follow tick). `o` is { before?: id, after?: id, count?: n,
    /// match?: line } — the filter is SPEC §data.streams's, a bounded backward walk because redis
    /// indexes nothing inside an entry — validated by [`redis_stream_opts`]. Defaulted exactly like list_databases above
    /// (SPEC §data.databases): the capability is redis-only, and a defaulted method means every
    /// stub and any future flavor keeps compiling and answering an honest “not
    /// supported” instead of the host growing a match arm per capability.
    async fn read_stream(&self, key: &str, o: &Value) -> Result<Value, String> {
        let _ = (key, o);
        Err("stream windows are not supported by this connection".into())
    }
    /// SPEC §data.streams: XINFO GROUPS for one stream key, read-only — name /
    /// consumers / pending / lag / last-delivered-id. Same defaulting rationale:
    /// the only implementation is redis, and no other flavor should be forced to
    /// say so.
    async fn stream_groups(&self, key: &str) -> Result<Value, String> {
        let _ = key;
        Err("stream groups are not supported by this connection".into())
    }
    /// SPEC §data.redis-console: the command catalog of the server this connection speaks to — COMMAND DOCS
    /// plus COMMAND INFO, merged. The console's completion is built from it rather than from
    /// a table in the panel, so it knows exactly the commands (and modules, and versions)
    /// that are actually there. Defaulted like the two above.
    async fn command_catalog(&self) -> Result<Value, String> {
        Err("command catalogs are not supported by this connection".into())
    }
    /// SPEC §data.pubsub: the channels with a subscriber right now - PUBSUB CHANNELS, sorted, at most
    /// PUBSUB_LIST_MAX - each with its PUBSUB NUMSUB count, and the server's pattern
    /// subscriptions (PUBSUB NUMPAT). `o` is { pattern? }. Defaulted like the stream reads:
    /// pub/sub is redis-only.
    async fn pubsub_channels(&self, o: &Value) -> Result<Value, String> {
        let _ = o;
        Err("pub/sub is not supported by this connection".into())
    }
    /// SPEC §data.pubsub: subscribe to `watch` on a connection of its own - never the shared one, which
    /// a SUBSCRIBE would leave in a mode it cannot leave - and hand back the feed. A failure
    /// before the first event (connect, AUTH, a refused SUBSCRIBE) is the Err; the feed's own
    /// end is its closed event.
    async fn subscribe(&self, watch: &PubSubWatch) -> Result<PubSubFeed, String> {
        let _ = watch;
        Err("pub/sub is not supported by this connection".into())
    }
}
