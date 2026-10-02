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

//! The in-process MongoDB adapter (SPEC §mcp.db, ADR-030): one pooled driver client behind six
//! tools, the command guard that keeps that client shared, and the operations the Data view's
//! browser (`mongo_browser.rs`) and the resources (`mongo_resources.rs`) run on the same client.
//!
//! Values cross every seam here as Extended JSON (SPEC §data.mongo): canonical towards the
//! panel, which must hand an Int64 or a Decimal128 back exactly as it read it, and relaxed
//! towards a model, which reads `{"$date": "2026-01-01T00:00:00Z"}` far better than a
//! `$numberLong` of milliseconds. Input may be either form; bson parses both.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::{ClientOptions, ConnectionString, HostInfo};
use mongodb::Client;
use serde_json::{json, Map, Value};

use swiss_host::config::ServerDef;
use swiss_host::mongobrowser::{
    infer_schema, mongo_activity_noise, mongo_activity_row, mongo_aggregate_of, mongo_find_of, pipeline_writes,
    stage_op, MongoAggregate, MongoCount, MongoExplain, MongoExplainTarget, MongoFind, MongoNs,
    MONGO_PAGE_MAX, MONGO_STATS_MAX,
};

use super::direct::{def_bool, BoxFut, Lazy};
use super::mongo_resources::MongoResources;
use super::resources::human_bytes;
use super::sql::{as_i64, inspect_args, inspect_description, inspect_reply, Check};
use super::tool_server::{Engine, ServerMeta, ToolDef};

// --- limits --------------------------------------------------------------------------------------

/// Documents mongo_find and mongo_aggregate return when the call names no limit and the MCP sets
/// no `maxRows` — a document is often several rows' worth of text, so the page is small.
pub const FIND_LIMIT_DEFAULT: i64 = 20;
/// Documents mongo_describe_collection samples for the shape by default, and at most.
pub const DESCRIBE_SAMPLE_DEFAULT: i64 = 100;
pub const DESCRIBE_SAMPLE_MAX: i64 = 1_000;
/// Paths the brief schema of mongo_describe_collection lists before it says it stopped.
const BRIEF_PATHS_MAX: usize = 200;
/// The client-side bound on one command: the server's `maxTimeMS` bounds the reads that take
/// it, this bounds everything else (a `{sleep: …}` is refused, but a slow `dbHash` is not), so
/// no call can hold a request — and the MCP behind it — forever.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the first connect may take before the open gives up: server selection (4 s) plus a
/// handshake and a SCRAM round trip.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

// --- Extended JSON -------------------------------------------------------------------------------

/// Which Extended JSON a reply speaks (SPEC §data.mongo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ejson {
    /// Every value typed — the panel's wire form, lossless both ways.
    Canonical,
    /// Numbers as numbers, dates as ISO strings — the model's and the export's form.
    Relaxed,
}

pub fn ejson(b: Bson, mode: Ejson) -> Value {
    match mode {
        Ejson::Canonical => b.into_canonical_extjson(),
        Ejson::Relaxed => b.into_relaxed_extjson(),
    }
}

pub fn doc_json(d: Document, mode: Ejson) -> Value {
    ejson(Bson::Document(d), mode)
}

/// A document out of Extended JSON (either form), the failure naming what it was.
pub fn to_doc(m: &Map<String, Value>, what: &str) -> Result<Document, String> {
    Document::try_from(m.clone()).map_err(|e| format!("{what} is not valid Extended JSON: {e}"))
}

pub fn to_bson(v: &Value, what: &str) -> Result<Bson, String> {
    Bson::try_from(v.clone()).map_err(|e| format!("{what} is not valid Extended JSON: {e}"))
}

/// One canonical value re-spoken relaxed — the schema sample values a model reads.
fn relaxed_of(v: &Value) -> Value {
    match Bson::try_from(v.clone()) {
        Ok(b) => b.into_relaxed_extjson(),
        Err(_) => v.clone(),
    }
}

/// A whole number out of whichever numeric type the server chose for it: counts arrive as Int32
/// on one version and Int64 or Double on the next.
pub fn num(b: Option<&Bson>) -> Option<i64> {
    match b? {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(f) if f.is_finite() => Some(*f as i64),
        _ => None,
    }
}

pub fn numf(b: Option<&Bson>) -> Option<f64> {
    match b? {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(f) => Some(*f),
        _ => None,
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() <= max {
        return line.to_string();
    }
    let mut end = max;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// The driver's error, said the way a person reads it: a server refusal as its own message and
/// code name, a write error with the validator's details, a dead network as the reason the
/// topology gave. Never the connection string — the driver does not echo it, and nothing here
/// adds it.
pub fn err_text(e: &mongodb::error::Error) -> String {
    use mongodb::error::{ErrorKind, WriteFailure};
    match e.kind.as_ref() {
        ErrorKind::Command(c) => format!("{} ({}, code {})", c.message, c.code_name, c.code),
        ErrorKind::Write(WriteFailure::WriteError(w)) => {
            let mut s = match &w.code_name {
                Some(name) => format!("{} ({name}, code {})", w.message, w.code),
                None => format!("{} (code {})", w.message, w.code),
            };
            // A validation failure explains itself in errInfo — which rule, which field.
            if let Some(details) = &w.details {
                s.push_str(&format!(" — {}", doc_json(details.clone(), Ejson::Relaxed)));
            }
            s
        }
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => {
            format!("write concern failed: {} ({}, code {})", w.message, w.code_name, w.code)
        }
        ErrorKind::InsertMany(m) => match m.write_errors.as_ref().and_then(|w| w.first()) {
            Some(w) => format!("document {}: {} (code {})", w.index + 1, w.message, w.code),
            None => match &m.write_concern_error {
                Some(w) => format!("write concern failed: {} (code {})", w.message, w.code),
                None => first_line(&e.to_string(), 500),
            },
        },
        ErrorKind::ServerSelection { message, .. } => match selection_reason(message) {
            Some((addr, why)) => format!("no reachable MongoDB server: {addr}: {why}"),
            None => format!("no reachable MongoDB server: {}", first_line(message, 600)),
        },
        ErrorKind::Authentication { message, .. } => format!("authentication failed: {message}"),
        ErrorKind::InvalidArgument { message, .. } => message.clone(),
        _ => first_line(&e.to_string(), 600),
    }
}

/// The first server and its error out of a server-selection timeout, whose message is the
/// driver's whole topology dump (`… Servers: [ { Address: h:1, Type: Unknown, Error: Kind: I/O
/// error: Connection refused (os error 111), labels: {…} … } ] }`): the address and the reason
/// are the two facts a person acts on.
fn selection_reason(message: &str) -> Option<(String, String)> {
    let addr_at = message.find("Address: ")? + "Address: ".len();
    let addr = message[addr_at..].split(',').next()?.trim().to_string();
    let kind_at = message.find("Error: Kind: ")? + "Error: Kind: ".len();
    let rest = &message[kind_at..];
    let why = rest.split(", labels:").next()?.trim().to_string();
    (!addr.is_empty() && !why.is_empty()).then_some((addr, why))
}

/// The server's error code, when the failure was the server's answer.
pub fn err_code(e: &mongodb::error::Error) -> Option<i32> {
    use mongodb::error::{ErrorKind, WriteFailure};
    match e.kind.as_ref() {
        ErrorKind::Command(c) => Some(c.code),
        ErrorKind::Write(WriteFailure::WriteError(w)) => Some(w.code),
        _ => None,
    }
}

/// MaxTimeMSExpired — a read that ran out of its own time budget, which a count reports as
/// "unknown" rather than as a failure.
pub const CODE_MAX_TIME_EXPIRED: i32 = 50;
/// Unauthorized — the login may not run this; several listings fall back to a narrower form.
pub const CODE_UNAUTHORIZED: i32 = 13;

// --- the command guard ---------------------------------------------------------------------------

/// Commands refused outright, each with the reason the model sees — a refusal that says why ends
/// the attempt; a bare "rejected" invites the next bad idea. Names compare case-insensitively.
const REJECTED: &[(&[&str], &str)] = &[
    (&["shutdown"], "it stops the MongoDB server."),
    (
        &["fsync", "fsyncunlock"],
        "fsync with a lock blocks every write on the server until someone unlocks it.",
    ),
    (
        &[
            "setparameter",
            "setclusterparameter",
            "setfeaturecompatibilityversion",
            "setdefaultrwconcern",
            "configurefailpoint",
            "logrotate",
            "rotatecertificates",
            "sleep",
            "resync",
            "compact",
            "compactstructuredencryptiondata",
        ],
        "it changes how the server itself runs, or holds it busy; that is an operator's decision, \
         not a debugging step.",
    ),
    (
        &[
            "replsetinitiate",
            "replsetreconfig",
            "replsetstepdown",
            "replsetstepup",
            "replsetfreeze",
            "replsetmaintenance",
            "replsetsyncfrom",
            "replsetresizeoplog",
            "replsetabortprimarycatchup",
        ],
        "it reconfigures the replica set. replSetGetStatus and replSetGetConfig read it, and \
         mongo_inspect check \"replication\" summarises it.",
    ),
    (
        &[
            "addshard",
            "removeshard",
            "enablesharding",
            "shardcollection",
            "reshardcollection",
            "abortreshardcollection",
            "commitreshardcollection",
            "unshardcollection",
            "movecollection",
            "refinecollectionshardkey",
            "movechunk",
            "moverange",
            "moveprimary",
            "split",
            "mergechunks",
            "balancerstart",
            "balancerstop",
            "configurecollectionbalancing",
            "cleanuporphaned",
            "clearjumboflag",
            "addshardtozone",
            "removeshardfromzone",
            "updatezonekeyrange",
            "transitiontodedicatedconfigserver",
            "transitionfromdedicatedconfigserver",
        ],
        "it reshapes the sharded cluster; that is an operator's decision, not a debugging step.",
    ),
    (
        &[
            "createuser",
            "updateuser",
            "dropuser",
            "dropallusersfromdatabase",
            "grantrolestouser",
            "revokerolesfromuser",
            "createrole",
            "updaterole",
            "droprole",
            "dropallrolesfromdatabase",
            "grantprivilegestorole",
            "revokeprivilegesfromrole",
            "grantrolestorole",
            "revokerolesfromrole",
            "invalidateusercache",
        ],
        "it rewrites this server's users and roles — one updateUser can lock this gateway out. \
         usersInfo and rolesInfo read them.",
    ),
    (
        &[
            "authenticate",
            "logout",
            "saslstart",
            "saslcontinue",
            "startsession",
            "endsessions",
            "refreshsessions",
            "committransaction",
            "aborttransaction",
            "getmore",
            "killcursors",
        ],
        "this MCP shares one pooled client, so a login, session or cursor opened by one call \
         belongs to a connection the next call may not get. mongo_find and mongo_aggregate page \
         for you.",
    ),
    (
        &["killallsessions", "killallsessionsbypattern", "dropconnections"],
        "it kills sessions or connections across the whole server, this gateway's own included. \
         killOp ends one operation.",
    ),
    (
        &["applyops"],
        "it writes oplog entries directly, below every check the server would otherwise apply.",
    ),
];

/// Top-level fields that belong to the driver's session machinery. A command carrying its own
/// `lsid` + `startTransaction` would open a transaction on the shared pool that no later call
/// could commit, holding its locks until the server's transaction lifetime runs out.
const SESSION_FIELDS: &[&str] = &["lsid", "txnnumber", "autocommit", "starttransaction", "$db"];

/// Commands that destroy data, permitted only with `allowDestructive: true` — the twin of
/// redis's FLUSHALL/FLUSHDB line. Dropping an index is not here: it loses no data and can be
/// rebuilt.
const DESTRUCTIVE: &[&str] = &["dropdatabase", "drop", "emptycapped", "converttocapped"];

/// Whether one command document destroys data: the named commands, a rename over an existing
/// collection (`dropTarget`), an aggregation that `$out`s over a collection, and a map-reduce
/// that replaces its output.
pub fn command_destroys(cmd: &Map<String, Value>) -> bool {
    let Some(name) = cmd.keys().next() else { return false };
    let lower = name.to_ascii_lowercase();
    if DESTRUCTIVE.contains(&lower.as_str()) {
        return true;
    }
    match lower.as_str() {
        "renamecollection" => cmd.get("dropTarget") == Some(&Value::Bool(true)),
        "aggregate" => cmd
            .get("pipeline")
            .and_then(Value::as_array)
            .is_some_and(|p| p.iter().any(|s| s.get("$out").is_some())),
        "mapreduce" => match cmd.get("out") {
            Some(Value::String(_)) => true,
            Some(Value::Object(o)) => o.contains_key("replace"),
            _ => false,
        },
        _ => false,
    }
}

/// The one policy mongo_command and the Data view's console share. Decided before a socket is
/// touched: a refused call never connects.
pub fn assert_command_allowed(cmd: &Map<String, Value>, allow_destructive: bool) -> Result<(), String> {
    let Some(name) = cmd.keys().next() else {
        return Err("command is empty — pass one command document, e.g. {\"ping\": 1}".into());
    };
    let lower = name.to_ascii_lowercase();
    for (names, why) in REJECTED {
        if names.contains(&lower.as_str()) {
            return Err(format!("{name} is rejected: {why}"));
        }
    }
    if let Some(k) = cmd.keys().find(|k| SESSION_FIELDS.contains(&k.to_ascii_lowercase().as_str())) {
        return Err(format!(
            "{k} is rejected: sessions and transactions belong to the driver here — this MCP shares \
             one client, so a transaction opened by one call would hold its locks with no later call \
             able to commit it."
        ));
    }
    if command_destroys(cmd) && !allow_destructive {
        return Err(format!(
            "{name} is rejected here: it destroys data. Set \"allowDestructive\": true on this MCP to \
             permit it."
        ));
    }
    Ok(())
}

// --- where it points -----------------------------------------------------------------------------

/// `shop @ db1:27017 +2` — the target line of a mongo MCP and its browser label. The driver's
/// own parser reads the URL (no network), so what is shown is what will be dialled; the
/// credentials it parsed are never part of the line. `database` is the def's override.
pub fn mongo_where(url: &str, database: Option<&str>) -> String {
    let Ok(cs) = ConnectionString::parse(url) else {
        return "mongodb (unreadable url)".into();
    };
    let hosts: Vec<String> = match &cs.host_info {
        HostInfo::HostIdentifiers(list) => list.iter().map(|a| a.to_string()).collect(),
        HostInfo::DnsRecord(name) => vec![name.clone()],
        _ => Vec::new(),
    };
    let first = hosts.first().cloned().unwrap_or_else(|| "localhost:27017".into());
    let place = if hosts.len() > 1 { format!("{first} +{}", hosts.len() - 1) } else { first };
    match database.filter(|d| !d.is_empty()).or(cs.default_database.as_deref()) {
        Some(db) => format!("{db} @ {place}"),
        None => place,
    }
}

/// Validate a def's URL at construction, without dialling: the scheme, the hosts, the options.
/// A `mongodb+srv://` URL fails here in the driver's own words — this build carries no DNS
/// resolver (ADR-030).
pub fn check_url(url: &str) -> Result<(), String> {
    ConnectionString::parse(url).map(|_| ()).map_err(|e| format!("url: {}", err_text(&e)))
}

// --- the shared client ---------------------------------------------------------------------------

/// One driver client — its own pool and monitors — opened on first use and shared by the tools,
/// the resources, the browser and the health ping.
pub struct MongoHandle {
    client: Client,
    default_db: Option<String>,
}

/// What the server says about itself, read once per call that needs it.
#[derive(Clone, Debug)]
pub struct ServerFacts {
    pub version: String,
    /// `standalone`, `replicaSet` or `sharded`.
    pub topology: &'static str,
    pub set_name: Option<String>,
    pub max_wire: i64,
}

impl ServerFacts {
    /// Multi-document transactions: a replica set from wire 7 (4.0), a sharded cluster from wire
    /// 8 (4.2). A standalone never has them.
    pub fn transactions(&self) -> bool {
        match self.topology {
            "replicaSet" => self.max_wire >= 7,
            "sharded" => self.max_wire >= 8,
            _ => false,
        }
    }
}

impl MongoHandle {
    /// Parse, build, and prove the connection with one `ping`. The driver builds a client
    /// without dialling, so without the probe a wrong password or a dead host would surface
    /// only on the first real call, as a server-selection timeout that hides what the server
    /// actually said.
    pub async fn open(url: &str, database: Option<String>) -> Result<Self, String> {
        let mut opts = ClientOptions::parse(url).await.map_err(|e| format!("url: {}", err_text(&e)))?;
        // Defaults only where the URL said nothing: a URL option always wins.
        if opts.app_name.is_none() {
            opts.app_name = Some("swiss".into());
        }
        opts.server_selection_timeout.get_or_insert(Duration::from_secs(4));
        opts.connect_timeout.get_or_insert(Duration::from_secs(4));
        // A developer tool's pool: a handful of sockets, closed when idle a minute, never kept
        // warm (SPEC §product.memory).
        opts.max_pool_size.get_or_insert(4);
        opts.max_idle_time.get_or_insert(Duration::from_secs(60));
        let default_db = database.filter(|d| !d.is_empty()).or_else(|| opts.default_database.clone());
        let client = Client::with_options(opts).map_err(|e| err_text(&e))?;
        let probe = tokio::time::timeout(OPEN_TIMEOUT, client.database("admin").run_command(doc! { "ping": 1 })).await;
        let failure = match probe {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(err_text(&e)),
            Err(_) => Some(format!("mongo connect timed out after {}s", OPEN_TIMEOUT.as_secs())),
        };
        if let Some(message) = failure {
            let _ = tokio::time::timeout(Duration::from_secs(1), client.shutdown().immediate(true)).await;
            return Err(message);
        }
        Ok(Self { client, default_db })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn default_db(&self) -> Option<&str> {
        self.default_db.as_deref()
    }

    /// Shut the client down without waiting on handles still alive elsewhere (a cursor an
    /// export is streaming): stopping an MCP must not hang on one.
    pub async fn close(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), self.client.clone().shutdown().immediate(true)).await;
    }

    /// One command, bounded by [`COMMAND_TIMEOUT`].
    pub async fn command(&self, db: &str, cmd: Document) -> Result<Document, String> {
        self.command_raw(db, cmd).await.map_err(fail)
    }

    /// [`command`](Self::command) keeping the driver's error, so a caller can branch on the code.
    /// `Err(None)` is the client-side timeout.
    pub async fn command_raw(&self, db: &str, cmd: Document) -> Result<Document, Option<mongodb::error::Error>> {
        match tokio::time::timeout(COMMAND_TIMEOUT, self.client.database(db).run_command(cmd)).await {
            Ok(Ok(d)) => Ok(d),
            Ok(Err(e)) => Err(Some(e)),
            Err(_) => Err(None),
        }
    }

    /// Run a cursor-returning command and collect up to `max` documents; `more` says whether the
    /// cursor held another. The cursor is dropped after, which kills it server-side.
    pub async fn cursor_docs(&self, db: &str, cmd: Document, max: usize) -> Result<(Vec<Document>, bool), Option<mongodb::error::Error>> {
        let run = async {
            let mut cursor = self.client.database(db).run_cursor_command(cmd).await?;
            let mut out = Vec::new();
            let mut more = false;
            while cursor.advance().await? {
                if out.len() >= max {
                    more = true;
                    break;
                }
                out.push(cursor.deserialize_current()?);
            }
            Ok::<_, mongodb::error::Error>((out, more))
        };
        match tokio::time::timeout(COMMAND_TIMEOUT, run).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(Some(e)),
            Err(_) => Err(None),
        }
    }

    pub async fn cursor(&self, db: &str, cmd: Document, max: usize) -> Result<(Vec<Document>, bool), String> {
        self.cursor_docs(db, cmd, max).await.map_err(fail)
    }

    /// One page of a find: `limit` documents and whether there are more.
    pub async fn find_page(&self, q: &MongoFind) -> Result<(Vec<Document>, bool), String> {
        let cmd = find_command(q, true)?;
        self.cursor(&q.ns.db, cmd, q.limit.max(0) as usize).await
    }

    /// An aggregation: `(docs, more, wrote)`. A pipeline ending in `$out`/`$merge` runs whole and
    /// answers the namespace it wrote instead of documents.
    pub async fn aggregate_docs(&self, q: &MongoAggregate) -> Result<(Vec<Document>, bool, Option<String>), String> {
        let (cmd, writes) = aggregate_command(q)?;
        let (docs, more) = self.cursor(&q.ns.db, cmd, q.limit.max(0) as usize).await?;
        if writes {
            return Ok((Vec::new(), false, Some(written_ns(&q.ns.db, &q.pipeline))));
        }
        Ok((docs, more, None))
    }

    /// `{total, estimated}`: from the collection's metadata when nothing filters it (instant on
    /// any size), else by counting the matches under the request's own time budget — a count
    /// that runs out of time answers `total: null, timedOut: true`, never a wrong number.
    pub async fn count(&self, q: &MongoCount) -> Result<Value, String> {
        if q.filter.is_empty() && !q.exact {
            let reply = self.command(&q.ns.db, doc! { "count": &q.ns.coll, "maxTimeMS": q.max_time_ms as i64 }).await?;
            return Ok(json!({ "total": num(reply.get("n")), "estimated": true }));
        }
        let mut cmd = doc! {
            "aggregate": &q.ns.coll,
            "pipeline": [ { "$match": to_doc(&q.filter, "filter")? }, { "$count": "n" } ],
            "cursor": {},
            "maxTimeMS": q.max_time_ms as i64,
        };
        if let Some(h) = &q.hint {
            cmd.insert("hint", to_bson(h, "hint")?);
        }
        match self.cursor_docs(&q.ns.db, cmd, 1).await {
            Ok((docs, _)) => {
                let total = docs.first().and_then(|d| num(d.get("n"))).unwrap_or(0);
                Ok(json!({ "total": total, "estimated": false }))
            }
            Err(Some(e)) if err_code(&e) == Some(CODE_MAX_TIME_EXPIRED) => {
                Ok(json!({ "total": null, "estimated": false, "timedOut": true }))
            }
            Err(e) => Err(fail(e)),
        }
    }

    /// The server's explain reply for a find or an aggregation.
    pub async fn explain(&self, q: &MongoExplain) -> Result<Document, String> {
        let (db, inner) = match &q.target {
            MongoExplainTarget::Find(f) => (&f.ns.db, find_command(f, true)?),
            MongoExplainTarget::Aggregate(a) => (&a.ns.db, aggregate_command(a)?.0),
        };
        self.command(db, doc! { "explain": inner, "verbosity": q.verbosity }).await
    }

    /// Up to `size` documents by `$sample`, after `filter` when one narrows the population.
    pub async fn sample(&self, ns: &MongoNs, filter: &Map<String, Value>, size: i64) -> Result<Vec<Document>, String> {
        let mut pipeline: Vec<Bson> = Vec::new();
        if !filter.is_empty() {
            pipeline.push(Bson::Document(doc! { "$match": to_doc(filter, "filter")? }));
        }
        pipeline.push(Bson::Document(doc! { "$sample": { "size": size } }));
        let cmd = doc! {
            "aggregate": &ns.coll,
            "pipeline": pipeline,
            "cursor": {},
            "allowDiskUse": true,
            "maxTimeMS": 30_000_i64,
        };
        Ok(self.cursor(&ns.db, cmd, size.max(0) as usize).await?.0)
    }

    /// `hello` + `buildInfo`, condensed.
    pub async fn server_facts(&self) -> Result<ServerFacts, String> {
        let hello = self.command("admin", doc! { "hello": 1 }).await?;
        let build = self.command("admin", doc! { "buildInfo": 1 }).await?;
        let set_name = hello.get_str("setName").ok().map(str::to_string);
        let topology = if hello.get_str("msg").ok() == Some("isdbgrid") {
            "sharded"
        } else if set_name.is_some() {
            "replicaSet"
        } else {
            "standalone"
        };
        Ok(ServerFacts {
            version: build.get_str("version").unwrap_or("").to_string(),
            topology,
            set_name,
            max_wire: num(hello.get("maxWireVersion")).unwrap_or(0),
        })
    }

    /// Every database the login may name: `[(name, sizeOnDisk, empty)]`. A login without the
    /// listDatabases privilege still sees the databases it holds a role on (authorizedDatabases),
    /// and when even that is refused, the URL's own database.
    pub async fn databases(&self) -> Result<Vec<(String, Option<i64>, bool)>, String> {
        let reply = self
            .command_raw("admin", doc! { "listDatabases": 1, "nameOnly": false, "authorizedDatabases": true })
            .await;
        let reply = match reply {
            Ok(r) => r,
            Err(Some(e)) if err_code(&e) == Some(CODE_UNAUTHORIZED) => match self
                .command("admin", doc! { "listDatabases": 1, "nameOnly": true, "authorizedDatabases": true })
                .await
            {
                Ok(r) => r,
                Err(message) => match &self.default_db {
                    Some(db) => return Ok(vec![(db.clone(), None, false)]),
                    None => return Err(message),
                },
            },
            Err(e) => return Err(fail(e)),
        };
        let mut out = Vec::new();
        for d in reply.get_array("databases").map(|a| a.as_slice()).unwrap_or(&[]) {
            let Some(d) = d.as_document() else { continue };
            let Ok(name) = d.get_str("name") else { continue };
            out.push((name.to_string(), num(d.get("sizeOnDisk")), d.get_bool("empty").unwrap_or(false)));
        }
        if let Some(db) = &self.default_db {
            if !out.iter().any(|(n, _, _)| n == db) {
                out.push((db.clone(), None, true));
            }
        }
        Ok(out)
    }

    /// The raw `listCollections` entries of one database, with a fallback to the name-only,
    /// authorized-only form for a login that may not list the whole catalog.
    pub async fn collection_infos(&self, db: &str, filter: Option<Document>) -> Result<Vec<Document>, String> {
        let mut cmd = doc! { "listCollections": 1, "nameOnly": false };
        if let Some(f) = &filter {
            cmd.insert("filter", f.clone());
        }
        match self.cursor_docs(db, cmd, usize::MAX).await {
            Ok((docs, _)) => Ok(docs),
            Err(Some(e)) if err_code(&e) == Some(CODE_UNAUTHORIZED) => {
                let mut cmd = doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true };
                if let Some(f) = filter {
                    cmd.insert("filter", f);
                }
                Ok(self.cursor(db, cmd, usize::MAX).await?.0)
            }
            Err(e) => Err(fail(e)),
        }
    }

    /// `$collStats` storage statistics of one collection, summed across shards.
    pub async fn coll_stats(&self, db: &str, coll: &str) -> Result<Document, String> {
        let cmd = doc! {
            "aggregate": coll,
            "pipeline": [ { "$collStats": { "storageStats": {} } } ],
            "cursor": {},
        };
        let (docs, _) = self.cursor(db, cmd, 64).await?;
        let mut sum = Document::new();
        for d in docs {
            let Ok(s) = d.get_document("storageStats") else { continue };
            for key in ["count", "size", "storageSize", "totalIndexSize", "nindexes", "freeStorageSize"] {
                if let Some(n) = num(s.get(key)) {
                    let prev = num(sum.get(key)).unwrap_or(0);
                    sum.insert(key, prev + n);
                }
            }
            if let Ok(capped) = s.get_bool("capped") {
                sum.insert("capped", capped);
            }
            if let Ok(sizes) = s.get_document("indexSizes") {
                let mut merged = sum.get_document("indexSizes").cloned().unwrap_or_default();
                for (k, v) in sizes {
                    let prev = num(merged.get(k)).unwrap_or(0);
                    merged.insert(k, prev + num(Some(v)).unwrap_or(0));
                }
                sum.insert("indexSizes", merged);
            }
        }
        if let (Some(size), Some(count)) = (num(sum.get("size")), num(sum.get("count"))) {
            if count > 0 {
                sum.insert("avgObjSize", size / count);
            }
        }
        Ok(sum)
    }

    /// The collections of one database as listing rows: name, type, the storage statistics of
    /// the first [`MONGO_STATS_MAX`] (gathered eight at a time), and what the catalog says of
    /// views, validators, capped and time-series collections. `(rows, statsOmitted)`.
    pub async fn collections(&self, db: &str, stats: bool, mode: Ejson) -> Result<(Vec<Value>, usize), String> {
        use futures_util::StreamExt;
        let infos = self.collection_infos(db, None).await?;
        let mut rows: Vec<(String, Map<String, Value>)> = infos.into_iter().map(|i| collection_row(i, mode)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut omitted = 0usize;
        if stats {
            let wanted: Vec<String> = rows
                .iter()
                .filter(|(_, r)| r.get("type").and_then(Value::as_str) != Some("view"))
                .map(|(n, _)| n.clone())
                .collect();
            omitted = wanted.len().saturating_sub(MONGO_STATS_MAX);
            let gathered: Vec<(String, Result<Document, String>)> = futures_util::stream::iter(wanted.into_iter().take(MONGO_STATS_MAX))
                .map(|name| async move {
                    let s = self.coll_stats(db, &name).await;
                    (name, s)
                })
                .buffer_unordered(8)
                .collect()
                .await;
            for (name, s) in gathered {
                let Ok(s) = s else { continue };
                if let Some((_, row)) = rows.iter_mut().find(|(n, _)| *n == name) {
                    for (key, out) in [
                        ("count", "count"),
                        ("size", "size"),
                        ("storageSize", "storageSize"),
                        ("avgObjSize", "avgObjSize"),
                        ("nindexes", "indexes"),
                        ("totalIndexSize", "indexSize"),
                    ] {
                        if let Some(n) = num(s.get(key)) {
                            row.insert(out.into(), json!(n));
                        }
                    }
                    if s.get_bool("capped").unwrap_or(false) {
                        row.insert("capped".into(), json!(true));
                    }
                }
            }
        }
        Ok((rows.into_iter().map(|(_, r)| Value::Object(r)).collect(), omitted))
    }

    /// The indexes of one collection with their usage (`$indexStats`) and size (`$collStats`).
    /// The two statistics are best-effort — a view, a login without `indexStats`, an old server —
    /// and their failure rides along as `statsError` instead of hiding the definitions.
    pub async fn indexes(&self, ns: &MongoNs, mode: Ejson) -> Result<(Vec<Value>, Option<String>), String> {
        let (specs, _) = self.cursor(&ns.db, doc! { "listIndexes": &ns.coll }, usize::MAX).await?;
        let usage = self
            .cursor(&ns.db, doc! { "aggregate": &ns.coll, "pipeline": [ { "$indexStats": {} } ], "cursor": {} }, 1_000)
            .await;
        let sizes = self.coll_stats(&ns.db, &ns.coll).await;
        let mut stats_error: Option<String> = None;
        let usage = match usage {
            Ok((docs, _)) => docs,
            Err(e) => {
                stats_error = Some(e);
                Vec::new()
            }
        };
        let sizes = match sizes {
            Ok(s) => s.get_document("indexSizes").cloned().unwrap_or_default(),
            Err(e) => {
                stats_error.get_or_insert(e);
                Document::new()
            }
        };
        let mut out = Vec::with_capacity(specs.len());
        for spec in specs {
            let name = spec.get_str("name").unwrap_or("").to_string();
            let mut row = Map::new();
            row.insert("name".into(), json!(name));
            for (k, v) in spec {
                // `v` is the index format version and `ns` a pre-4.4 echo — noise to a reader.
                if matches!(k.as_str(), "name" | "v" | "ns") {
                    continue;
                }
                let value = match (&v, k.as_str()) {
                    (_, "key" | "partialFilterExpression" | "collation" | "weights" | "wildcardProjection") => ejson(v, mode),
                    _ => ejson(v, Ejson::Relaxed),
                };
                row.insert(k, value);
            }
            if let Some(n) = num(sizes.get(&name)) {
                row.insert("size".into(), json!(n));
            }
            // Usage is per mongod; on a sharded cluster the shards' counters are summed.
            let mut ops: Option<i64> = None;
            let mut since: Option<mongodb::bson::DateTime> = None;
            for u in usage.iter().filter(|u| u.get_str("name").ok() == Some(name.as_str())) {
                if let Ok(acc) = u.get_document("accesses") {
                    ops = Some(ops.unwrap_or(0) + num(acc.get("ops")).unwrap_or(0));
                    if let Ok(s) = acc.get_datetime("since") {
                        since = Some(since.map_or(*s, |cur| cur.min(*s)));
                    }
                }
            }
            if let Some(n) = ops {
                row.insert("ops".into(), json!(n));
            }
            if let Some(s) = since {
                if let Ok(text) = s.try_to_rfc3339_string() {
                    row.insert("since".into(), json!(text));
                }
            }
            out.push(Value::Object(row));
        }
        Ok((out, stats_error))
    }

    /// `$currentOp`: every user's operations when the login may see them, else its own.
    pub async fn current_ops(&self, active_only: bool) -> Result<Vec<Document>, String> {
        let run = |all: bool| {
            let mut pipeline = vec![Bson::Document(doc! { "$currentOp": { "allUsers": all, "idleConnections": false } })];
            if active_only {
                pipeline.push(Bson::Document(doc! { "$match": { "active": true } }));
            }
            doc! { "aggregate": 1, "pipeline": pipeline, "cursor": {} }
        };
        match self.cursor_docs("admin", run(true), 1_000).await {
            Ok((docs, _)) => Ok(docs),
            Err(Some(e)) if err_code(&e) == Some(CODE_UNAUTHORIZED) => Ok(self.cursor("admin", run(false), 1_000).await?.0),
            Err(e) => Err(fail(e)),
        }
    }
}

/// A command reply without the cluster-time gossip every replica-set reply carries
/// (`$clusterTime` with its signature, `operationTime`) — driver bookkeeping, not an answer.
pub fn without_gossip(mut reply: Document) -> Document {
    reply.remove("$clusterTime");
    reply.remove("operationTime");
    reply
}

/// The text of a failed call: the driver's error, or the client-side timeout (`None`).
fn fail(e: Option<mongodb::error::Error>) -> String {
    match e {
        Some(e) => err_text(&e),
        None => format!("the command did not answer within {}s", COMMAND_TIMEOUT.as_secs()),
    }
}

/// One `listCollections` entry as a listing row (name kept beside it for sorting).
fn collection_row(info: Document, mode: Ejson) -> (String, Map<String, Value>) {
    let name = info.get_str("name").unwrap_or("").to_string();
    let kind = info.get_str("type").unwrap_or("collection").to_string();
    let mut row = Map::new();
    row.insert("name".into(), json!(name));
    let options = info.get_document("options").cloned().unwrap_or_default();
    let kind = if options.contains_key("timeseries") { "timeseries".to_string() } else { kind };
    row.insert("type".into(), json!(kind));
    if let Ok(info) = info.get_document("info") {
        if info.get_bool("readOnly").unwrap_or(false) {
            row.insert("readOnly".into(), json!(true));
        }
    }
    if let Ok(on) = options.get_str("viewOn") {
        row.insert("viewOn".into(), json!(on));
    }
    for key in ["pipeline", "validator", "timeseries", "collation", "clusteredIndex"] {
        if let Some(v) = options.get(key) {
            row.insert(key.into(), ejson(v.clone(), mode));
        }
    }
    for key in ["validationLevel", "validationAction"] {
        if let Ok(v) = options.get_str(key) {
            row.insert(key.into(), json!(v));
        }
    }
    if options.get_bool("capped").unwrap_or(false) {
        row.insert("capped".into(), json!(true));
        for key in ["size", "max"] {
            if let Some(n) = num(options.get(key)) {
                row.insert(if key == "size" { "cappedSize".into() } else { "cappedMax".into() }, json!(n));
            }
        }
    }
    if let Some(n) = num(options.get("expireAfterSeconds")) {
        row.insert("expireAfterSeconds".into(), json!(n));
    }
    (name, row)
}

/// The namespace a writing pipeline writes: `$out: "coll"`, `$out: {db, coll}`, `$merge: "coll"`
/// or `$merge: {into: "coll" | {db, coll}}`.
pub fn written_ns(db: &str, pipeline: &[Map<String, Value>]) -> String {
    let Some(last) = pipeline.last() else { return String::new() };
    let target = last.get("$out").or_else(|| last.get("$merge").map(|m| m.get("into").unwrap_or(m)));
    match target {
        Some(Value::String(c)) => format!("{db}.{c}"),
        Some(Value::Object(o)) => format!(
            "{}.{}",
            o.get("db").and_then(Value::as_str).unwrap_or(db),
            o.get("coll").and_then(Value::as_str).unwrap_or("")
        ),
        _ => String::new(),
    }
}

/// The `find` command of a [`MongoFind`]. `page` asks one past the limit — the page's `more` —
/// in a single batch; an export streams instead and leaves the batching to the server.
pub fn find_command(q: &MongoFind, page: bool) -> Result<Document, String> {
    let mut c = doc! { "find": &q.ns.coll, "filter": to_doc(&q.filter, "filter")? };
    if let Some(p) = &q.projection {
        c.insert("projection", to_doc(p, "projection")?);
    }
    if let Some(s) = &q.sort {
        c.insert("sort", to_doc(s, "sort")?);
    }
    if let Some(col) = &q.collation {
        c.insert("collation", to_doc(col, "collation")?);
    }
    if let Some(h) = &q.hint {
        c.insert("hint", to_bson(h, "hint")?);
    }
    if q.skip > 0 {
        c.insert("skip", q.skip as i64);
    }
    if page {
        let fetch = q.limit.saturating_add(1);
        c.insert("limit", fetch);
        c.insert("batchSize", fetch);
    } else if q.limit > 0 {
        c.insert("limit", q.limit);
    }
    c.insert("maxTimeMS", q.max_time_ms as i64);
    Ok(c)
}

/// The `aggregate` command of a [`MongoAggregate`], and whether it writes. A reading pipeline
/// gains a trailing `$limit` of limit+1 (the page's `more`); a writing one runs whole. A change
/// stream never ends, so it has no place in a request that must.
pub fn aggregate_command(q: &MongoAggregate) -> Result<(Document, bool), String> {
    let writes = pipeline_writes(&q.pipeline);
    let mut stages: Vec<Bson> = Vec::with_capacity(q.pipeline.len() + 1);
    for (i, s) in q.pipeline.iter().enumerate() {
        if stage_op(s) == "$changeStream" {
            return Err(format!(
                "stage {}: $changeStream never ends, so it cannot answer a request — watch the collection from a client instead",
                i + 1
            ));
        }
        stages.push(Bson::Document(to_doc(s, &format!("stage {} ({})", i + 1, stage_op(s)))?));
    }
    if !writes {
        stages.push(Bson::Document(doc! { "$limit": q.limit.saturating_add(1) }));
    }
    let cursor = if writes { doc! {} } else { doc! { "batchSize": q.limit.saturating_add(1) } };
    let mut c = doc! {
        "aggregate": &q.ns.coll,
        "pipeline": stages,
        "cursor": cursor,
        "maxTimeMS": q.max_time_ms as i64,
    };
    if q.allow_disk_use {
        c.insert("allowDiskUse", true);
    }
    if let Some(col) = &q.collation {
        c.insert("collation", to_doc(col, "collation")?);
    }
    Ok((c, writes))
}

// --- the brief schema ----------------------------------------------------------------------------

/// [`infer_schema`]'s full report boiled down to what a model needs to write a query: every
/// path with how often it is present, the types it holds and their shares, and up to three
/// example values (relaxed). Array element types ride on the path as `elementTypes`.
pub fn brief_schema(schema: &Value) -> Value {
    fn walk(fields: &Value, out: &mut Vec<Value>, capped: &mut bool) {
        let Some(list) = fields.as_array() else { return };
        for f in list {
            if out.len() >= BRIEF_PATHS_MAX {
                *capped = true;
                return;
            }
            let mut row = Map::new();
            row.insert("path".into(), f.get("path").cloned().unwrap_or(Value::Null));
            row.insert("presence".into(), f.get("probability").cloned().unwrap_or(json!(0)));
            let mut types = Map::new();
            let mut examples: Vec<Value> = Vec::new();
            let mut nested: Vec<&Value> = Vec::new();
            let mut element_types = Map::new();
            for t in f.get("types").and_then(Value::as_array).into_iter().flatten() {
                let name = t.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                types.insert(name, t.get("probability").cloned().unwrap_or(json!(0)));
                for v in t.get("values").and_then(Value::as_array).into_iter().flatten() {
                    if examples.len() < 3 {
                        examples.push(clip_example(relaxed_of(v)));
                    }
                }
                if let Some(sub) = t.get("fields") {
                    nested.push(sub);
                }
                if let Some(el) = t.get("elements").and_then(|e| e.get("types")).and_then(Value::as_array) {
                    for et in el {
                        let name = et.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
                        element_types.insert(name, et.get("probability").cloned().unwrap_or(json!(0)));
                        if let Some(sub) = et.get("fields") {
                            nested.push(sub);
                        }
                    }
                }
            }
            row.insert("types".into(), Value::Object(types));
            if !element_types.is_empty() {
                row.insert("elementTypes".into(), Value::Object(element_types));
            }
            if !examples.is_empty() {
                row.insert("examples".into(), Value::Array(examples));
            }
            out.push(Value::Object(row));
            for sub in nested {
                walk(sub, out, capped);
            }
        }
    }
    let mut out = Vec::new();
    let mut capped = schema.get("fieldsCapped") == Some(&Value::Bool(true));
    if let Some(f) = schema.get("fields") {
        walk(f, &mut out, &mut capped);
    }
    let mut reply = json!({ "sampled": schema.get("sampled").cloned().unwrap_or(json!(0)), "fields": out });
    if capped {
        reply["fieldsCapped"] = json!(true);
    }
    reply
}

/// An example value short enough to sit in a schema line: long strings are cut, and a document
/// or array is replaced by its size (its paths are listed on their own lines anyway).
fn clip_example(v: Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > 80 => Value::String(format!("{}…", s.chars().take(80).collect::<String>())),
        Value::Array(a) => json!(format!("[{} items]", a.len())),
        Value::Object(o) if !o.keys().next().is_some_and(|k| k.starts_with('$')) => json!(format!("{{{} fields}}", o.len())),
        other => other,
    }
}

// --- tools ---------------------------------------------------------------------------------------

fn db_arg() -> Value {
    json!({ "type": "string", "description": "Database name. Omit to use the database the connection URL names." })
}
fn coll_arg() -> Value {
    json!({ "type": "string", "description": "Collection (or view) name." })
}
fn doc_arg(what: &str) -> Value {
    json!({ "type": "object", "description": what })
}

/// mongo_inspect's checks. Unlike the SQL engines' none is a single query, so `rows`/`summary`
/// stay empty and each check is code below.
pub const MONGO_CHECKS: &[Check] = &[
    Check {
        name: "activity",
        about: "operations running right now, longest first",
        rows: None,
        summary: None,
        note: Some("Other users' operations are visible only to a login with the inprog privilege."),
    },
    Check {
        name: "slow_queries",
        about: "the slowest recent operations the database profiler recorded (system.profile)",
        rows: None,
        summary: None,
        note: None,
    },
    Check {
        name: "top",
        about: "time spent per collection (reads, writes, totals) since the server started",
        rows: None,
        summary: None,
        note: Some("Times are milliseconds accumulated since the server started; mongos does not run top."),
    },
    Check {
        name: "unused_indexes",
        about: "indexes no query has used since the counters last reset, with their size",
        rows: None,
        summary: None,
        note: Some("Usage counters reset when the server restarts or the index is rebuilt; check every replica before dropping one."),
    },
    Check {
        name: "storage",
        about: "the database's size and its largest collections",
        rows: None,
        summary: None,
        note: None,
    },
    Check {
        name: "server",
        about: "version, uptime, connections, operation counters, memory and cache",
        rows: None,
        summary: None,
        note: None,
    },
    Check {
        name: "replication",
        about: "replica set members, their state and replication lag",
        rows: None,
        summary: None,
        note: None,
    },
];

/// Six tools. A model already knows MongoDB's commands; what it cannot know is which databases
/// and collections exist, what the documents in them look like, and which call would stall the
/// server or the shared client — so those get tools of their own, and everything else goes
/// through the guarded mongo_command, whose description says what this instance refuses.
fn tools(default_limit: i64, allow_destructive: bool) -> Vec<ToolDef> {
    let limit = json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MONGO_PAGE_MAX,
        "description": format!("Documents to return (default {default_limit}, max {MONGO_PAGE_MAX}). The reply's `more` says whether there were more."),
    });
    vec![
        ToolDef {
            name: "mongo_list_collections".into(),
            description: concat!(
                "List the collections and views of a database with their document count, data size and index ",
                "count — use this before querying, to know what exists and what is big. Without `db` (and when ",
                "the connection names no database) it lists the databases instead, with their size. Pass `grep` ",
                "to keep only names containing it.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": db_arg(),
                    "grep": { "type": "string", "description": "Keep only names containing this text (case-insensitive)." },
                },
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "mongo_find".into(),
            description: format!(
                "Query a collection. filter/projection/sort are MongoDB documents in Extended JSON: \
                 {{\"_id\": {{\"$oid\": \"…\"}}}}, {{\"at\": {{\"$gte\": {{\"$date\": \"2026-01-01T00:00:00Z\"}}}}}}, \
                 {{\"n\": {{\"$numberLong\": \"9007199254740993\"}}}}. Returns {{ docs, count, more }} with documents \
                 in relaxed Extended JSON. At most {MONGO_PAGE_MAX} per call — page with skip, or better, with a \
                 range filter on a sorted indexed field. Each call runs under a 15 s server-side time limit."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": db_arg(),
                    "collection": coll_arg(),
                    "filter": doc_arg("Query filter. Omit to match every document."),
                    "projection": doc_arg("Fields to include ({\"name\": 1}) or exclude ({\"blob\": 0})."),
                    "sort": doc_arg("Sort order, e.g. {\"createdAt\": -1}."),
                    "skip": { "type": "integer", "minimum": 0, "description": "Documents to skip first." },
                    "limit": limit,
                },
                "required": ["collection"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "mongo_aggregate".into(),
            description: format!(
                "Run an aggregation pipeline on a collection — grouping, joins ($lookup), counting, reshaping. \
                 Stages are documents in Extended JSON. Returns {{ docs, count, more }}; the output is capped \
                 at `limit` documents. A pipeline ending in $merge writes and returns what it wrote{}. \
                 $changeStream is not supported (it never ends).",
                if allow_destructive {
                    "; so does $out, which replaces its target collection"
                } else {
                    "; $out, which replaces its target collection, is refused on this MCP"
                }
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": db_arg(),
                    "collection": coll_arg(),
                    "pipeline": { "type": "array", "items": { "type": "object" }, "description": "The stages, in order." },
                    "limit": limit,
                    "allowDiskUse": { "type": "boolean", "description": "Let large $group/$sort stages spill to disk." },
                },
                "required": ["collection", "pipeline"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "mongo_describe_collection".into(),
            description: concat!(
                "Everything about one collection in one call: its inferred document shape (every field path ",
                "with how often it is present, its BSON types and their shares, and example values — sampled ",
                "with $sample), its indexes with their usage counts and sizes, the document count and sizes, ",
                "and the validator, view pipeline, capped or time-series options when it has them. Use this ",
                "before writing a query against an unfamiliar collection.",
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": db_arg(),
                    "collection": coll_arg(),
                    "sample": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": DESCRIBE_SAMPLE_MAX,
                        "description": format!("Documents to sample for the shape (default {DESCRIBE_SAMPLE_DEFAULT}, max {DESCRIBE_SAMPLE_MAX})."),
                    },
                },
                "required": ["collection"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "mongo_command".into(),
            description: command_description(allow_destructive),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db": db_arg(),
                    "command": doc_arg("One command document; its first key is the command, e.g. {\"insert\": \"users\", \"documents\": [{…}]}."),
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        },
        ToolDef {
            name: "mongo_inspect".into(),
            description: inspect_description(
                "Diagnose the server with one built-in check per call. Read-only.",
                MONGO_CHECKS,
                "Returns { check, rows?, summary?, note? }.",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "check": {
                        "type": "string",
                        "enum": MONGO_CHECKS.iter().map(|c| c.name).collect::<Vec<_>>(),
                        "description": "Which diagnostic to run (the tool description says what each reports).",
                    },
                    "db": db_arg(),
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": super::sql::MAX_INSPECT_LIMIT,
                        "description": format!(
                            "Max rows (default {}, max {}).",
                            super::sql::DEFAULT_INSPECT_LIMIT,
                            super::sql::MAX_INSPECT_LIMIT
                        ),
                    },
                },
                "required": ["check"],
                "additionalProperties": false,
            }),
        },
    ]
}

/// mongo_command's description, closing with what THIS instance refuses.
fn command_description(allow_destructive: bool) -> String {
    let mut text = String::from(
        "Run any other MongoDB database command: { command: {\"insert\": \"users\", \"documents\": [{\"name\": \"Ada\"}]} }, \
         {\"update\": \"users\", \"updates\": [{\"q\": {…}, \"u\": {\"$set\": {…}}}]}, {\"delete\": …}, {\"createIndexes\": …}, \
         {\"distinct\": …}, {\"collStats\"…}, {\"serverStatus\": 1}, {\"profile\": 1, \"slowms\": 100}, {\"killOp\": 1, \"op\": 123}. \
         The reply comes back in relaxed Extended JSON; a find or aggregate here returns only its first batch \
         — use mongo_find / mongo_aggregate to read documents. Rejected: server and cluster administration \
         (shutdown, fsync, setParameter, replSet*/sharding reconfiguration), user and role management, \
         session and cursor commands (startSession, getMore, …) and session fields (lsid, txnNumber), which \
         would break this shared client",
    );
    if allow_destructive {
        text.push_str(". This MCP allows dropDatabase, drop and the other data-destroying commands.");
    } else {
        text.push_str(", and commands that destroy data (dropDatabase, drop, emptycapped, renameCollection with dropTarget, an aggregate with $out).");
    }
    text
}

/// A document argument that a client sent as a JSON string — models do that — read as the
/// document it spells. Anything else stays as it came, for the parser to name.
fn doc_value(v: &Value) -> Value {
    match v {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(parsed @ (Value::Object(_) | Value::Array(_))) => parsed,
            _ => v.clone(),
        },
        other => other.clone(),
    }
}

// --- the engine ----------------------------------------------------------------------------------

pub struct MongoEngine {
    def: ServerDef,
    name: Arc<std::sync::RwLock<String>>,
    conn: Arc<Lazy<MongoHandle>>,
    where_: String,
}

impl MongoEngine {
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let def = def.clone();
        let url = def
            .get_str("url")
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "mongo needs a url, e.g. mongodb://user:pass@localhost:27017/app".to_string())?
            .to_string();
        check_url(&url)?;
        let database = def.get_str("database").map(str::trim).filter(|d| !d.is_empty()).map(str::to_string);
        let where_ = mongo_where(&url, database.as_deref());
        let conn = Lazy::new(move || {
            let url = url.clone();
            let database = database.clone();
            Box::pin(async move { MongoHandle::open(&url, database).await }) as BoxFut<Result<MongoHandle, String>>
        });
        Ok(Self {
            def,
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            conn: Arc::new(conn),
            where_,
        })
    }

    fn allow_destructive(&self) -> bool {
        def_bool(&self.def, "allowDestructive")
    }

    /// The `maxRows` of the def as the default page, kept inside the page cap.
    fn default_limit(&self) -> i64 {
        self.def
            .get_number("maxRows")
            .map(|n| n as i64)
            .filter(|n| *n > 0)
            .unwrap_or(FIND_LIMIT_DEFAULT)
            .min(MONGO_PAGE_MAX)
    }

    /// The database a call addresses: its own `db`, else the connection's.
    fn db_of(handle: &MongoHandle, args: &Value) -> Result<String, String> {
        match args.get("db").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty()) {
            Some(db) => Ok(db.to_string()),
            None => handle.default_db().map(str::to_string).ok_or_else(|| {
                "db is required: the connection URL names no database (mongo_list_collections without db lists them)".to_string()
            }),
        }
    }

    /// The tool's arguments as the browser parsers read them: `db` filled in, document
    /// arguments un-stringified, the page default applied.
    fn browse_args(&self, handle: &MongoHandle, args: &Value) -> Result<Value, String> {
        let mut o = args.as_object().cloned().unwrap_or_default();
        o.insert("db".into(), json!(Self::db_of(handle, args)?));
        for key in ["filter", "projection", "sort", "pipeline"] {
            if let Some(v) = o.get(key) {
                let v = doc_value(v);
                o.insert(key.into(), v);
            }
        }
        if !o.contains_key("limit") {
            o.insert("limit".into(), json!(self.default_limit()));
        }
        Ok(Value::Object(o))
    }

    async fn call_list(&self, args: &Value) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let grep = args.get("grep").and_then(Value::as_str).map(|g| g.trim().to_lowercase()).filter(|g| !g.is_empty());
        let keep = |name: &str| grep.as_ref().is_none_or(|g| name.to_lowercase().contains(g));
        let db = args.get("db").and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty()).map(str::to_string);
        let Some(db) = db.or_else(|| handle.default_db().map(str::to_string)) else {
            let dbs = handle.databases().await?;
            let rows: Vec<Value> = dbs
                .into_iter()
                .filter(|(n, _, _)| keep(n))
                .map(|(name, size, empty)| {
                    let mut row = json!({ "name": name });
                    if let Some(s) = size {
                        row["size"] = json!(human_bytes(s.max(0) as u64));
                    }
                    if empty {
                        row["empty"] = json!(true);
                    }
                    row
                })
                .collect();
            return Ok(json!({ "databases": rows, "note": "Pass db to list one database's collections." }));
        };
        let (rows, omitted) = handle.collections(&db, true, Ejson::Relaxed).await?;
        let rows: Vec<Value> = rows
            .into_iter()
            .filter(|r| keep(r.get("name").and_then(Value::as_str).unwrap_or("")))
            .map(|mut r| {
                // Byte counts read as sizes to a model; the panel gets the raw numbers.
                for key in ["size", "storageSize", "indexSize"] {
                    if let Some(n) = r.get(key).and_then(Value::as_i64) {
                        r[key] = json!(human_bytes(n.max(0) as u64));
                    }
                }
                if let Some(o) = r.as_object_mut() {
                    // The full validator and view pipeline belong to mongo_describe_collection.
                    for key in ["pipeline", "validator", "collation", "clusteredIndex", "storageSize", "avgObjSize"] {
                        o.remove(key);
                    }
                }
                r
            })
            .collect();
        let mut out = json!({ "db": db, "collections": rows });
        if omitted > 0 {
            out["statsOmitted"] = json!(omitted);
        }
        Ok(out)
    }

    async fn call_find(&self, args: &Value) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let q = mongo_find_of(&self.browse_args(&handle, args)?, MONGO_PAGE_MAX)?;
        let (docs, more) = handle.find_page(&q).await?;
        let count = docs.len();
        let docs: Vec<Value> = docs.into_iter().map(|d| doc_json(d, Ejson::Relaxed)).collect();
        Ok(json!({ "docs": docs, "count": count, "more": more }))
    }

    async fn call_aggregate(&self, args: &Value) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let q = mongo_aggregate_of(&self.browse_args(&handle, args)?)?;
        if q.pipeline.is_empty() {
            return Err("pipeline is empty — pass at least one stage, or use mongo_find".into());
        }
        if !self.allow_destructive() && q.pipeline.iter().any(|s| stage_op(s) == "$out") {
            return Err(
                "$out is rejected here: it replaces its target collection. Use $merge, or set \"allowDestructive\": true on this MCP."
                    .into(),
            );
        }
        let (docs, more, wrote) = handle.aggregate_docs(&q).await?;
        if let Some(ns) = wrote {
            return Ok(json!({ "wrote": ns }));
        }
        let count = docs.len();
        let docs: Vec<Value> = docs.into_iter().map(|d| doc_json(d, Ejson::Relaxed)).collect();
        Ok(json!({ "docs": docs, "count": count, "more": more }))
    }

    async fn call_describe(&self, args: &Value) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let db = Self::db_of(&handle, args)?;
        let coll = args.get("collection").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let size = args
            .get("sample")
            .and_then(as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(DESCRIBE_SAMPLE_DEFAULT)
            .min(DESCRIBE_SAMPLE_MAX);
        describe_collection(&handle, &db, &coll, size).await
    }

    async fn call_command(&self, args: &Value) -> Result<Value, String> {
        let command = match args.get("command").map(doc_value) {
            Some(Value::Object(m)) => m,
            _ => return Err("command must be a document, e.g. {\"ping\": 1}".into()),
        };
        // Refused before connecting: a refused call never opens a socket.
        assert_command_allowed(&command, self.allow_destructive())?;
        let cmd = to_doc(&command, "command")?;
        let handle = self.conn.get().await?;
        let db = Self::db_of(&handle, args)?;
        let reply = handle.command(&db, cmd).await?;
        Ok(doc_json(without_gossip(reply), Ejson::Relaxed))
    }

    async fn call_inspect(&self, args: &Value) -> Result<Value, String> {
        let (check, limit) = inspect_args(args, MONGO_CHECKS)?;
        let handle = self.conn.get().await?;
        let limit = limit.max(1) as usize;
        let (rows, summary, note) = match check.name {
            "activity" => (Some(inspect_activity(&handle, limit).await?), None, None),
            "slow_queries" => {
                let db = Self::db_of(&handle, args)?;
                inspect_slow(&handle, &db, limit).await?
            }
            "top" => (Some(inspect_top(&handle, limit).await?), None, None),
            "unused_indexes" => {
                let db = Self::db_of(&handle, args)?;
                (Some(inspect_unused(&handle, &db, limit).await?), None, None)
            }
            "storage" => {
                let db = Self::db_of(&handle, args)?;
                let (rows, summary) = inspect_storage(&handle, &db, limit).await?;
                (Some(rows), Some(summary), None)
            }
            "server" => (None, Some(inspect_server(&handle).await?), None),
            "replication" => inspect_replication(&handle).await?,
            other => return Err(format!("unknown check \"{other}\"")),
        };
        let mut reply = inspect_reply(check, rows, summary);
        if let Some(n) = note {
            reply["note"] = json!(n);
        }
        Ok(reply)
    }
}

/// Everything about one collection, for a model: the catalog entry (type, validator, view
/// pipeline, capped and time-series options), storage numbers, indexes with usage, and the
/// brief schema of a `$sample` of `size` documents. mongo_describe_collection and the
/// collection resource both answer with it.
pub async fn describe_collection(handle: &MongoHandle, db: &str, coll: &str, size: i64) -> Result<Value, String> {
    swiss_host::mongobrowser::assert_coll_name(db, coll)?;
    let ns = MongoNs { db: db.to_string(), coll: coll.to_string() };
    let infos = handle.collection_infos(db, Some(doc! { "name": coll })).await?;
    let Some(info) = infos.into_iter().next() else {
        return Err(format!("no collection or view named \"{coll}\" in {db} (mongo_list_collections lists them)"));
    };
    let (_, row) = collection_row(info, Ejson::Relaxed);
    let mut out = Map::new();
    out.insert("db".into(), json!(db));
    out.insert("collection".into(), json!(coll));
    for (k, v) in row {
        if k != "name" {
            out.insert(k, v);
        }
    }
    let is_view = out.get("type").and_then(Value::as_str) == Some("view");
    if !is_view {
        if let Ok(s) = handle.coll_stats(db, coll).await {
            if let Some(n) = num(s.get("count")) {
                out.insert("count".into(), json!(n));
            }
            for (key, label) in [("size", "size"), ("storageSize", "storageSize"), ("totalIndexSize", "indexSize"), ("avgObjSize", "avgObjSize")] {
                if let Some(n) = num(s.get(key)) {
                    out.insert(label.into(), json!(human_bytes(n.max(0) as u64)));
                }
            }
        }
        match handle.indexes(&ns, Ejson::Relaxed).await {
            Ok((mut list, stats_error)) => {
                for ix in &mut list {
                    if let Some(n) = ix.get("size").and_then(Value::as_i64) {
                        ix["size"] = json!(human_bytes(n.max(0) as u64));
                    }
                }
                out.insert("indexes".into(), Value::Array(list));
                if let Some(e) = stats_error {
                    out.insert("indexStatsError".into(), json!(e));
                }
            }
            Err(e) => {
                out.insert("indexesError".into(), json!(e));
            }
        }
    }
    let sample = handle.sample(&ns, &Map::new(), size).await?;
    let docs: Vec<Value> = sample.into_iter().map(|d| doc_json(d, Ejson::Canonical)).collect();
    out.insert("schema".into(), brief_schema(&infer_schema(&docs)));
    Ok(Value::Object(out))
}

type Rows = Vec<Map<String, Value>>;

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

async fn inspect_activity(handle: &MongoHandle, limit: usize) -> Result<Rows, String> {
    let ops = handle.current_ops(true).await?;
    let mut rows: Vec<Value> = ops
        .into_iter()
        .map(|d| doc_json(d, Ejson::Relaxed))
        .filter(|op| !mongo_activity_noise(op))
        .map(|op| mongo_activity_row(&op))
        .filter(|r| r.get("own") != Some(&Value::Bool(true)))
        .collect();
    rows.sort_by(|a, b| b.get("seconds").and_then(Value::as_i64).cmp(&a.get("seconds").and_then(Value::as_i64)));
    Ok(rows
        .into_iter()
        .take(limit)
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                o.remove("own");
                o.remove("killable");
                if let Some(pid) = o.remove("pid") {
                    o.insert("opid".into(), pid);
                }
            }
            obj(r)
        })
        .collect())
}

async fn inspect_slow(handle: &MongoHandle, db: &str, limit: usize) -> Result<(Option<Rows>, Option<Map<String, Value>>, Option<String>), String> {
    let level = handle.command(db, doc! { "profile": -1 }).await?;
    let was = num(level.get("was")).unwrap_or(0);
    let slowms = num(level.get("slowms")).unwrap_or(100);
    let cmd = doc! {
        "find": "system.profile",
        "filter": {},
        "sort": { "millis": -1 },
        "limit": limit as i64,
        "projection": {
            "ts": 1, "op": 1, "ns": 1, "millis": 1, "docsExamined": 1, "keysExamined": 1,
            "nreturned": 1, "planSummary": 1, "command": 1, "user": 1,
        },
        "maxTimeMS": 15_000_i64,
    };
    let (docs, _) = handle.cursor(db, cmd, limit).await?;
    let rows: Rows = docs
        .into_iter()
        .map(|d| {
            let mut r = obj(doc_json(d, Ejson::Relaxed));
            if let Some(c) = r.get("command") {
                let text = c.to_string();
                r.insert("command".into(), json!(first_line(&text, 1_000)));
            }
            r
        })
        .collect();
    let mut summary = Map::new();
    summary.insert("profilingLevel".into(), json!(was));
    summary.insert("slowms".into(), json!(slowms));
    let note = (was == 0).then(|| {
        format!(
            "The profiler is off on {db} (level 0), so only what it recorded earlier is listed. Turn it on with \
             mongo_command {{\"profile\": 1, \"slowms\": {slowms}}}; it records operations slower than slowms into \
             {db}.system.profile."
        )
    });
    Ok((Some(rows), Some(summary), note))
}

async fn inspect_top(handle: &MongoHandle, limit: usize) -> Result<Rows, String> {
    let reply = handle.command("admin", doc! { "top": 1 }).await?;
    let totals = reply.get_document("totals").cloned().unwrap_or_default();
    let ms = |d: &Document, key: &str| -> f64 {
        d.get_document(key).ok().and_then(|t| numf(t.get("time"))).unwrap_or(0.0) / 1_000.0
    };
    let count = |d: &Document, key: &str| -> i64 { d.get_document(key).ok().and_then(|t| num(t.get("count"))).unwrap_or(0) };
    let mut rows: Vec<(f64, Map<String, Value>)> = Vec::new();
    for (ns, v) in totals {
        let Some(d) = v.as_document() else { continue };
        if ns == "note" || ns.is_empty() {
            continue;
        }
        let total = ms(d, "total");
        let mut r = Map::new();
        r.insert("ns".into(), json!(ns));
        r.insert("totalMs".into(), json!((total * 10.0).round() / 10.0));
        r.insert("readMs".into(), json!((ms(d, "readLock") * 10.0).round() / 10.0));
        r.insert("writeMs".into(), json!((ms(d, "writeLock") * 10.0).round() / 10.0));
        r.insert("operations".into(), json!(count(d, "total")));
        for (key, label) in [("queries", "queries"), ("insert", "inserts"), ("update", "updates"), ("remove", "removes"), ("getmore", "getMores")] {
            let n = count(d, key);
            if n > 0 {
                r.insert(label.into(), json!(n));
            }
        }
        rows.push((total, r));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(rows.into_iter().take(limit).map(|(_, r)| r).collect())
}

async fn inspect_unused(handle: &MongoHandle, db: &str, limit: usize) -> Result<Rows, String> {
    let infos = handle.collection_infos(db, None).await?;
    let mut rows: Vec<(i64, Map<String, Value>)> = Vec::new();
    let names: Vec<String> = infos
        .into_iter()
        .filter(|i| i.get_str("type").unwrap_or("collection") == "collection")
        .filter_map(|i| i.get_str("name").ok().map(str::to_string))
        .filter(|n| !n.starts_with("system."))
        .take(MONGO_STATS_MAX)
        .collect();
    for coll in names {
        let ns = MongoNs { db: db.to_string(), coll: coll.clone() };
        let Ok((list, None)) = handle.indexes(&ns, Ejson::Relaxed).await else { continue };
        for ix in list {
            let name = ix.get("name").and_then(Value::as_str).unwrap_or("");
            if name == "_id_" || ix.get("ops").and_then(Value::as_i64) != Some(0) {
                continue;
            }
            let size = ix.get("size").and_then(Value::as_i64).unwrap_or(0);
            let mut r = Map::new();
            r.insert("collection".into(), json!(coll));
            r.insert("index".into(), json!(name));
            r.insert("key".into(), ix.get("key").cloned().unwrap_or(Value::Null));
            r.insert("size".into(), json!(human_bytes(size.max(0) as u64)));
            if let Some(s) = ix.get("since") {
                r.insert("unusedSince".into(), s.clone());
            }
            if ix.get("unique") == Some(&Value::Bool(true)) {
                // A unique index enforces a rule even when no query reads it.
                r.insert("unique".into(), json!(true));
            }
            rows.push((size, r));
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    Ok(rows.into_iter().take(limit).map(|(_, r)| r).collect())
}

async fn inspect_storage(handle: &MongoHandle, db: &str, limit: usize) -> Result<(Rows, Map<String, Value>), String> {
    let stats = handle.command(db, doc! { "dbStats": 1, "scale": 1 }).await?;
    let mut summary = Map::new();
    for (key, label, bytes) in [
        ("collections", "collections", false),
        ("views", "views", false),
        ("objects", "documents", false),
        ("indexes", "indexes", false),
        ("dataSize", "dataSize", true),
        ("storageSize", "storageSize", true),
        ("indexSize", "indexSize", true),
        ("fsUsedSize", "filesystemUsed", true),
        ("fsTotalSize", "filesystemTotal", true),
    ] {
        if let Some(n) = num(stats.get(key)) {
            summary.insert(label.into(), if bytes { json!(human_bytes(n.max(0) as u64)) } else { json!(n) });
        }
    }
    let (list, _) = handle.collections(db, true, Ejson::Relaxed).await?;
    let mut rows: Vec<(i64, Map<String, Value>)> = list
        .into_iter()
        .filter(|r| r.get("type").and_then(Value::as_str) != Some("view"))
        .map(|r| {
            let storage = r.get("storageSize").and_then(Value::as_i64).unwrap_or(0);
            let index = r.get("indexSize").and_then(Value::as_i64).unwrap_or(0);
            let mut out = Map::new();
            out.insert("collection".into(), r.get("name").cloned().unwrap_or(Value::Null));
            if let Some(n) = r.get("count") {
                out.insert("documents".into(), n.clone());
            }
            for (key, label) in [("size", "dataSize"), ("storageSize", "storageSize"), ("indexSize", "indexSize"), ("avgObjSize", "avgDocument")] {
                if let Some(n) = r.get(key).and_then(Value::as_i64) {
                    out.insert(label.into(), json!(human_bytes(n.max(0) as u64)));
                }
            }
            (storage + index, out)
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    Ok((rows.into_iter().take(limit).map(|(_, r)| r).collect(), summary))
}

async fn inspect_server(handle: &MongoHandle) -> Result<Map<String, Value>, String> {
    let s = handle.command("admin", doc! { "serverStatus": 1, "repl": 0, "metrics": 0, "locks": 0 }).await?;
    let mut out = Map::new();
    if let Ok(v) = s.get_str("version") {
        out.insert("version".into(), json!(v));
    }
    if let Ok(v) = s.get_str("host") {
        out.insert("host".into(), json!(v));
    }
    if let Some(n) = num(s.get("uptime")) {
        out.insert("uptimeHours".into(), json!(((n as f64 / 3600.0) * 10.0).round() / 10.0));
    }
    if let Ok(c) = s.get_document("connections") {
        out.insert(
            "connections".into(),
            json!({ "current": num(c.get("current")), "available": num(c.get("available")), "active": num(c.get("active")) }),
        );
    }
    if let Ok(o) = s.get_document("opcounters") {
        let mut m = Map::new();
        for (k, v) in o {
            if let Some(n) = num(Some(v)) {
                m.insert(k.clone(), json!(n));
            }
        }
        out.insert("opcounters".into(), Value::Object(m));
    }
    if let Ok(m) = s.get_document("mem") {
        out.insert("memoryMB".into(), json!({ "resident": num(m.get("resident")), "virtual": num(m.get("virtual")) }));
    }
    if let Ok(wt) = s.get_document("wiredTiger") {
        if let Ok(cache) = wt.get_document("cache") {
            let used = num(cache.get("bytes currently in the cache"));
            let max = num(cache.get("maximum bytes configured"));
            if let (Some(u), Some(m)) = (used, max) {
                out.insert(
                    "cache".into(),
                    json!({
                        "used": human_bytes(u.max(0) as u64),
                        "configured": human_bytes(m.max(0) as u64),
                        "dirty": num(cache.get("tracked dirty bytes in the cache")).map(|d| human_bytes(d.max(0) as u64)),
                    }),
                );
            }
        }
    }
    if let Ok(n) = s.get_document("network") {
        out.insert(
            "network".into(),
            json!({
                "bytesIn": num(n.get("bytesIn")).map(|b| human_bytes(b.max(0) as u64)),
                "bytesOut": num(n.get("bytesOut")).map(|b| human_bytes(b.max(0) as u64)),
                "requests": num(n.get("numRequests")),
            }),
        );
    }
    if let Ok(a) = s.get_document("asserts") {
        let total: i64 = a.iter().filter_map(|(_, v)| num(Some(v))).sum();
        if total > 0 {
            out.insert("asserts".into(), doc_json(a.clone(), Ejson::Relaxed));
        }
    }
    Ok(out)
}

async fn inspect_replication(handle: &MongoHandle) -> Result<(Option<Rows>, Option<Map<String, Value>>, Option<String>), String> {
    let status = match handle.command_raw("admin", doc! { "replSetGetStatus": 1 }).await {
        Ok(s) => s,
        // NoReplicationEnabled (76) / NotYetInitialized (94): a standalone answers in words.
        Err(Some(e)) if matches!(err_code(&e), Some(76) | Some(94)) => {
            return Ok((None, None, Some("This server is not part of a replica set (a standalone, or a mongos — ask a shard).".into())))
        }
        Err(e) => return Err(fail(e)),
    };
    let members = status.get_array("members").cloned().unwrap_or_default();
    let primary_optime = members
        .iter()
        .filter_map(Bson::as_document)
        .find(|m| m.get_str("stateStr").ok() == Some("PRIMARY"))
        .and_then(|m| m.get_datetime("optimeDate").ok().copied());
    let rows: Rows = members
        .iter()
        .filter_map(Bson::as_document)
        .map(|m| {
            let mut r = Map::new();
            r.insert("name".into(), json!(m.get_str("name").unwrap_or("")));
            r.insert("state".into(), json!(m.get_str("stateStr").unwrap_or("")));
            r.insert("health".into(), json!(num(m.get("health")).unwrap_or(0) == 1));
            if let (Some(p), Ok(o)) = (primary_optime, m.get_datetime("optimeDate")) {
                let lag = (p.timestamp_millis() - o.timestamp_millis()) as f64 / 1_000.0;
                if m.get_str("stateStr").ok() != Some("PRIMARY") {
                    r.insert("lagSeconds".into(), json!(lag.max(0.0)));
                }
            }
            if let Some(n) = num(m.get("uptime")) {
                r.insert("uptimeHours".into(), json!(((n as f64 / 3600.0) * 10.0).round() / 10.0));
            }
            if let Ok(s) = m.get_str("syncSourceHost") {
                if !s.is_empty() {
                    r.insert("syncSource".into(), json!(s));
                }
            }
            if let Ok(msg) = m.get_str("lastHeartbeatMessage") {
                if !msg.is_empty() {
                    r.insert("lastHeartbeatMessage".into(), json!(msg));
                }
            }
            r
        })
        .collect();
    let mut summary = Map::new();
    if let Ok(set) = status.get_str("set") {
        summary.insert("set".into(), json!(set));
    }
    summary.insert("members".into(), json!(rows.len()));
    Ok((Some(rows), Some(summary), None))
}

#[async_trait]
impl Engine for MongoEngine {
    fn kind(&self) -> &'static str {
        "mongo"
    }

    fn tools(&self) -> Vec<ToolDef> {
        tools(self.default_limit(), self.allow_destructive())
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "mongo_list_collections" => self.call_list(args).await,
            "mongo_find" => self.call_find(args).await,
            "mongo_aggregate" => self.call_aggregate(args).await,
            "mongo_describe_collection" => self.call_describe(args).await,
            "mongo_command" => self.call_command(args).await,
            "mongo_inspect" => self.call_inspect(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn meta(&self) -> ServerMeta {
        ServerMeta {
            name: self.name.read().ok().map(|g| g.clone()),
            description: self.def.get_str("description").map(str::to_string),
            target: Some(self.where_.clone()),
            limits: None,
        }
    }

    /// The databases and collections as resources, labelled with the MCP name (what a client
    /// shows beside the URI, and what stays put when a tunnel's port moves).
    fn resources(&self) -> Option<Arc<dyn super::resources::ResourceProvider>> {
        let label = self
            .name
            .read()
            .ok()
            .and_then(|g| if g.is_empty() { None } else { Some(g.clone()) })
            .unwrap_or_else(|| self.where_.clone());
        Some(Arc::new(MongoResources::new(label, self.conn.clone())))
    }

    /// The Data view (SPEC §data.mongo), carrying the adapter's own `allowDestructive` so the
    /// console and the drop buttons refuse exactly what mongo_command refuses.
    fn browser(&self) -> Option<swiss_host::dbbrowser::BrowserFlavor> {
        Some(swiss_host::dbbrowser::BrowserFlavor::Mongo(Arc::new(super::mongo_browser::MongoDataBrowser::new(
            self.where_.clone(),
            self.allow_destructive(),
            self.conn.clone(),
        ))))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = async {
            let handle = self.conn.get().await?;
            handle.command("admin", doc! { "ping": 1 }).await.map(|_| ())
        }
        .await;
        Some(result)
    }

    async fn close(&self) {
        self.conn
            .dispose(|handle| Box::pin(async move { handle.close().await }) as BoxFut<()>)
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

    fn cmd(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("a command is a document, got {other}"),
        }
    }

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    #[test]
    fn a_selection_timeout_says_which_server_and_why() {
        let dump = "Server selection timeout: No available servers. Topology: { Type: Unknown, Servers: [ { Address: 127.0.0.1:1, Type: Unknown, Error: Kind: I/O error: Connection refused (os error 111), labels: {\"RetryableError\"}, source: None } ] }";
        assert_eq!(
            selection_reason(dump),
            Some(("127.0.0.1:1".to_string(), "I/O error: Connection refused (os error 111)".to_string()))
        );
        assert_eq!(selection_reason("Server selection timeout: nothing else"), None);
    }

    #[test]
    fn replies_drop_the_cluster_time_gossip() {
        let reply = without_gossip(doc! { "n": 1, "ok": 1.0, "$clusterTime": { "x": 1 }, "operationTime": 5 });
        assert_eq!(reply, doc! { "n": 1, "ok": 1.0 });
    }

    // ---- the guard ----------------------------------------------------------------------------

    #[test]
    fn ordinary_reads_and_writes_pass_the_guard() {
        for c in [
            json!({ "ping": 1 }),
            json!({ "insert": "users", "documents": [{ "name": "Ada" }] }),
            json!({ "update": "users", "updates": [{ "q": {}, "u": { "$set": { "x": 1 } }, "multi": true }] }),
            json!({ "delete": "users", "deletes": [{ "q": { "x": 1 }, "limit": 0 }] }),
            json!({ "createIndexes": "users", "indexes": [{ "key": { "email": 1 }, "name": "email_1" }] }),
            json!({ "dropIndexes": "users", "index": "email_1" }),
            json!({ "replSetGetStatus": 1 }),
            json!({ "usersInfo": 1 }),
            json!({ "killOp": 1, "op": 12 }),
            json!({ "renameCollection": "app.a", "to": "app.b" }),
            json!({ "aggregate": "users", "pipeline": [{ "$merge": "copy" }], "cursor": {} }),
        ] {
            assert!(assert_command_allowed(&cmd(c.clone()), false).is_ok(), "{c} must pass");
        }
    }

    #[test]
    fn server_breaking_commands_are_refused_whatever_the_flags_say() {
        for c in [
            json!({ "shutdown": 1 }),
            json!({ "fsync": 1, "lock": true }),
            json!({ "setParameter": 1, "logLevel": 5 }),
            json!({ "replSetStepDown": 60 }),
            json!({ "shardCollection": "app.users", "key": { "_id": "hashed" } }),
            json!({ "createUser": "x", "pwd": "y", "roles": [] }),
            json!({ "getMore": { "$numberLong": "1" }, "collection": "users" }),
            json!({ "startSession": 1 }),
            json!({ "killAllSessions": [] }),
            json!({ "applyOps": [] }),
        ] {
            let err = assert_command_allowed(&cmd(c.clone()), true).expect_err(&c.to_string());
            assert!(err.contains("is rejected"), "{err}");
        }
    }

    #[test]
    fn command_names_compare_without_case() {
        // The server matches a few aliases case-insensitively (ismaster/isMaster); the guard
        // refuses every spelling rather than the one it was written with.
        assert!(assert_command_allowed(&cmd(json!({ "SHUTDOWN": 1 })), true).is_err());
        assert!(assert_command_allowed(&cmd(json!({ "DropDatabase": 1 })), false).is_err());
    }

    #[test]
    fn session_fields_cannot_open_a_transaction_on_the_shared_client() {
        let err = assert_command_allowed(
            &cmd(json!({ "insert": "users", "documents": [{}], "lsid": { "id": "x" }, "startTransaction": true, "autocommit": false })),
            true,
        )
        .unwrap_err();
        assert!(err.starts_with("lsid is rejected"), "{err}");
    }

    #[test]
    fn data_destroying_commands_need_allow_destructive() {
        for c in [
            json!({ "dropDatabase": 1 }),
            json!({ "drop": "users" }),
            json!({ "emptycapped": "log" }),
            json!({ "convertToCapped": "log", "size": 1024 }),
            json!({ "renameCollection": "app.a", "to": "app.b", "dropTarget": true }),
            json!({ "aggregate": "users", "pipeline": [{ "$match": {} }, { "$out": "users_copy" }], "cursor": {} }),
            json!({ "mapReduce": "users", "map": "", "reduce": "", "out": "totals" }),
            json!({ "mapReduce": "users", "map": "", "reduce": "", "out": { "replace": "totals" } }),
        ] {
            let err = assert_command_allowed(&cmd(c.clone()), false).expect_err(&c.to_string());
            assert!(err.contains("allowDestructive"), "{err}");
            assert!(assert_command_allowed(&cmd(c.clone()), true).is_ok(), "{c} passes with the flag");
        }
        // An inline map-reduce writes nothing.
        assert!(assert_command_allowed(&cmd(json!({ "mapReduce": "u", "map": "", "reduce": "", "out": { "inline": 1 } })), false).is_ok());
    }

    #[test]
    fn an_empty_command_says_what_a_command_looks_like() {
        assert!(assert_command_allowed(&Map::new(), true).unwrap_err().contains("{\"ping\": 1}"));
    }

    // ---- where it points ----------------------------------------------------------------------

    #[test]
    fn the_target_line_names_database_and_hosts_never_credentials() {
        assert_eq!(mongo_where("mongodb://u:secret@db1:27017/shop?authSource=admin", None), "shop @ db1:27017");
        assert_eq!(mongo_where("mongodb://localhost", None), "localhost:27017");
        assert_eq!(mongo_where("mongodb://a:1,b:2,c:3/app?replicaSet=rs0", None), "app @ a:1 +2");
        // The def's database wins over the URL path.
        assert_eq!(mongo_where("mongodb://localhost/app", Some("other")), "other @ localhost:27017");
        assert!(!mongo_where("mongodb://u:secret@db1/shop", None).contains("secret"));
        assert_eq!(mongo_where("not a url", None), "mongodb (unreadable url)");
    }

    #[test]
    fn an_srv_url_is_refused_at_construction_in_the_drivers_words() {
        let err = check_url("mongodb+srv://cluster0.example.net/app").unwrap_err();
        assert!(err.contains("dns-resolver"), "{err}");
        assert!(check_url("mongodb://localhost:27017/app").is_ok());
        assert!(check_url("http://localhost").is_err());
    }

    #[test]
    fn the_engine_needs_a_url_and_reads_its_flags() {
        assert!(MongoEngine::new(&def(json!({ "type": "mongo" })), "m").is_err());
        let e = MongoEngine::new(&def(json!({ "type": "mongo", "url": "mongodb://localhost/app", "maxRows": 50 })), "m").unwrap();
        assert_eq!(e.kind(), "mongo");
        assert_eq!(e.default_limit(), 50);
        assert!(!e.allow_destructive());
        let names: Vec<String> = e.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            ["mongo_list_collections", "mongo_find", "mongo_aggregate", "mongo_describe_collection", "mongo_command", "mongo_inspect"]
        );
        assert_eq!(e.meta().target.as_deref(), Some("app @ localhost:27017"));
        // maxRows above the page cap is clamped, not honoured into a megabyte reply.
        let big = MongoEngine::new(&def(json!({ "type": "mongo", "url": "mongodb://localhost", "maxRows": 100000 })), "m").unwrap();
        assert_eq!(big.default_limit(), MONGO_PAGE_MAX);
    }

    #[test]
    fn every_tool_schema_refuses_stray_arguments() {
        for t in tools(20, false) {
            assert_eq!(t.input_schema.get("additionalProperties"), Some(&Value::Bool(false)), "{}", t.name);
        }
    }

    #[test]
    fn the_command_description_states_this_instances_line() {
        assert!(command_description(false).contains("destroy data"));
        assert!(command_description(true).contains("allows dropDatabase"));
    }

    // ---- commands -----------------------------------------------------------------------------

    fn find(v: Value) -> MongoFind {
        mongo_find_of(&v, MONGO_PAGE_MAX).expect("a valid find")
    }

    #[test]
    fn a_page_asks_one_past_its_limit_in_one_batch() {
        let q = find(json!({
            "db": "app", "collection": "users", "limit": 20, "skip": 40,
            "filter": { "_id": { "$oid": "65f0c0ffee00000000000001" } },
            "sort": { "at": -1 }, "projection": { "name": 1 }, "hint": "at_-1",
        }));
        let c = find_command(&q, true).unwrap();
        assert_eq!(c.get_str("find").unwrap(), "users");
        assert_eq!(num(c.get("limit")), Some(21));
        assert_eq!(num(c.get("batchSize")), Some(21));
        assert_eq!(num(c.get("skip")), Some(40));
        assert_eq!(c.get_str("hint").unwrap(), "at_-1");
        // Extended JSON became real BSON: the filter's _id is an ObjectId, not a sub-document.
        assert!(matches!(c.get_document("filter").unwrap().get("_id"), Some(Bson::ObjectId(_))));
        let export = find_command(&q, false).unwrap();
        assert_eq!(num(export.get("limit")), Some(20));
        assert!(export.get("batchSize").is_none());
    }

    #[test]
    fn invalid_extended_json_is_named_by_where_it_sat() {
        let q = find(json!({ "db": "app", "collection": "users", "filter": { "_id": { "$oid": "nope" } } }));
        let err = find_command(&q, true).unwrap_err();
        assert!(err.starts_with("filter is not valid Extended JSON"), "{err}");
    }

    #[test]
    fn a_reading_pipeline_gains_a_limit_and_a_writing_one_runs_whole() {
        let read = mongo_aggregate_of(&json!({
            "db": "app", "collection": "orders", "limit": 10,
            "pipeline": [{ "$group": { "_id": "$status", "n": { "$sum": 1 } } }],
        }))
        .unwrap();
        let (c, writes) = aggregate_command(&read).unwrap();
        assert!(!writes);
        let stages = c.get_array("pipeline").unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(num(stages[1].as_document().unwrap().get("$limit")), Some(11));

        let write = mongo_aggregate_of(&json!({
            "db": "app", "collection": "orders", "pipeline": [{ "$match": {} }, { "$merge": { "into": { "db": "bi", "coll": "o" } } }],
        }))
        .unwrap();
        let (c, writes) = aggregate_command(&write).unwrap();
        assert!(writes);
        assert_eq!(c.get_array("pipeline").unwrap().len(), 2);
        assert_eq!(written_ns("app", &write.pipeline), "bi.o");
        assert_eq!(written_ns("app", &[cmd(json!({ "$out": "copy" }))]), "app.copy");
    }

    #[test]
    fn a_change_stream_is_refused_because_it_never_ends() {
        let q = mongo_aggregate_of(&json!({ "db": "app", "collection": "c", "pipeline": [{ "$changeStream": {} }] })).unwrap();
        assert!(aggregate_command(&q).unwrap_err().contains("never ends"));
    }

    #[test]
    fn string_documents_from_a_model_are_read_as_documents() {
        assert_eq!(doc_value(&json!("{\"a\": 1}")), json!({ "a": 1 }));
        assert_eq!(doc_value(&json!("[{\"$match\": {}}]")), json!([{ "$match": {} }]));
        assert_eq!(doc_value(&json!("plain")), json!("plain"));
        assert_eq!(doc_value(&json!({ "a": 1 })), json!({ "a": 1 }));
    }

    // ---- the brief schema ---------------------------------------------------------------------

    #[test]
    fn the_brief_schema_lists_every_path_with_types_and_examples() {
        let docs = vec![
            json!({ "_id": { "$oid": "65f0c0ffee00000000000001" }, "name": "Ada", "tags": ["a"], "addr": { "city": "Paris" }, "n": { "$numberLong": "5" } }),
            json!({ "_id": { "$oid": "65f0c0ffee00000000000002" }, "name": "Bob", "tags": [{ "k": 1 }], "n": { "$numberInt": "2" } }),
        ];
        let brief = brief_schema(&infer_schema(&docs));
        assert_eq!(brief["sampled"], json!(2));
        let paths: Vec<&str> = brief["fields"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).collect();
        for p in ["_id", "name", "tags", "tags.k", "addr", "addr.city", "n"] {
            assert!(paths.contains(&p), "{p} missing from {paths:?}");
        }
        let addr = brief["fields"].as_array().unwrap().iter().find(|f| f["path"] == "addr").unwrap();
        assert_eq!(addr["presence"], json!(0.5));
        let n = brief["fields"].as_array().unwrap().iter().find(|f| f["path"] == "n").unwrap();
        assert_eq!(n["types"]["Int64"], json!(0.5));
        assert_eq!(n["types"]["Int32"], json!(0.5));
        // Examples are relaxed: a model reads 5, not {"$numberLong": "5"}.
        assert!(n["examples"].as_array().unwrap().contains(&json!(5)));
        let tags = brief["fields"].as_array().unwrap().iter().find(|f| f["path"] == "tags").unwrap();
        assert_eq!(tags["elementTypes"]["String"], json!(0.5));
        assert_eq!(tags["elementTypes"]["Document"], json!(0.5));
    }

    #[test]
    fn examples_stay_short() {
        let long = "x".repeat(200);
        let clipped = clip_example(json!(long));
        assert_eq!(clipped.as_str().unwrap().chars().count(), 81);
        assert_eq!(clip_example(json!([1, 2, 3])), json!("[3 items]"));
        assert_eq!(clip_example(json!({ "a": 1, "b": 2 })), json!("{2 fields}"));
        // A wrapper is a value, not a sub-document.
        assert_eq!(clip_example(json!({ "$oid": "65f0c0ffee00000000000001" })), json!({ "$oid": "65f0c0ffee00000000000001" }));
    }

    #[test]
    fn transactions_follow_the_topology_and_wire_version() {
        let facts = |topology: &'static str, max_wire: i64| ServerFacts { version: String::new(), topology, set_name: None, max_wire };
        assert!(!facts("standalone", 25).transactions());
        assert!(facts("replicaSet", 7).transactions());
        assert!(!facts("replicaSet", 6).transactions());
        assert!(!facts("sharded", 7).transactions());
        assert!(facts("sharded", 8).transactions());
    }
}
