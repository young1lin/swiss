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

//! The L2 gateway group (SPEC §testing.it): the real router on a REAL listener, driven
//! by a real rmcp client over HTTP. Gateway::boot is the copy of tests/adminapi.rs's
//! sandbox()+setup() the spec asked for - a copy, not a share, because the root
//! crate's test binary must stay untouched; drift between the two is what the
//! parity test at the bottom guards.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::Connection;

use swiss::app::{build_app, AppContext};
use swiss_host::config::ServerDef;
use swiss_host::managed::ManagedStore;
use swiss_host::token::TokenManager;
use swiss_mcp::adapters::make_adapter;
use swiss_mcp::registry::{Registry, Source};
use swiss_it::engine::{engine, Kind};
use swiss_it::seed::{fresh, fresh_redis_with_neighbor};

const TOKEN: &str = "it-gw-tok-0123456789abcdef";

/// One distinguishable scratch name without reaching for swiss-core's random_hex:
/// the pid plus a per-process counter is exactly as unique for temp paths.
fn scratch_id() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// The scratch home + master key, once per process - the same two lines
/// tests/adminapi.rs sets, copied per SPEC §testing.it rather than shared.
fn sandbox() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("swiss-it-gateway-home-{}", scratch_id()));
        std::fs::create_dir_all(&home).expect("create the scratch home");
        // Safety: this runs once, before any test has read either variable, and both
        // are read fresh on every use.
        unsafe {
            std::env::set_var("SWISS_HOME", &home);
            std::env::set_var("SWISS_MASTER_KEY", "cd".repeat(32));
        }
    });
}

/// One gateway on a real ephemeral port: registry + managed store + router, served
/// by a spawned axum task on the test's own runtime. The task dies with the runtime
/// when the test ends - no shutdown path needed.
pub(crate) struct Gateway {
    port: u16,
    token: String,
    #[allow(dead_code)]
    registry: Arc<Registry>,
    #[allow(dead_code)]
    store: Arc<ManagedStore>,
}

pub(crate) async fn boot(defs: Vec<(&str, Value)>) -> Gateway {
    sandbox();
    let scratch = std::env::temp_dir().join(format!("swiss-it-gw-{}", scratch_id()));
    std::fs::create_dir_all(&scratch).expect("create the scratch dir");
    let calls = Arc::new(swiss_mcp::calls::CallLog::at(scratch.join("calls")));
    let registry = Registry::new(60_000, calls.clone());
    let store = Arc::new(ManagedStore::open_at(scratch.join("managed.json")));
    let tokens = Arc::new(TokenManager::new(store.clone(), Some(TOKEN)));
    for (name, d) in defs {
        let d = ServerDef(d.as_object().cloned().expect("a def is an object"));
        // What the daemon does at boot: start the non-lazy MCPs (an idle proc child
        // is money nobody asked to spend - its first request wakes it).
        let lazy = swiss_mcp::registry::is_lazy(&d);
        let adapter = make_adapter(&d, name, &calls).expect("adapter");
        registry
            .register(name, Source::Config, d, adapter)
            .expect("register");
        if !lazy {
            registry.start(name).await.expect("start");
        }
    }
    // The daemon's boot starts the health-probe interval AND the 1 s idle-reap
    // sweeper - without this line a lazy proc's child would never be reaped, and
    // the L3 suite caught exactly that gap in the first cut of this boot.
    registry.start_timer();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port on loopback");
    let port = listener.local_addr().expect("bound").port();
    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        calls.clone(),
        Arc::new(swiss_mcp::traffic::TrafficLog::memory()),
        "SWISS_TOKEN",
        port,
    );
    // The connection catalog the daemon's MCP plugin registers on start (SPEC §host.seats):
    // without it every /api/db route answers the honest 503, and SPEC §data.streams's L2
    // needs the stream routes over the real gateway. Registered exactly the way the
    // plugin does, so the harness cannot drift from the product's wiring.
    let catalog = swiss_host::services::catalog::CatalogRegistry::new();
    let provider = swiss_mcp::registry::RegistryCatalog::new(
        registry.clone(),
        swiss_host::services::catalog::LeaseTracker::new(),
    );
    catalog
        .register(std::sync::Arc::new(provider), "swiss-it-boot")
        .expect("register the catalog provider");
    let _ = ctx.catalog.set(std::sync::Arc::new(catalog));
    let app = build_app(ctx, None);
    tokio::spawn(async move {
        // Serves until the runtime drops; a bind-level failure here is a bug worth
        // hearing about, so it lands on stderr rather than vanishing.
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("swiss-it gateway: serve loop ended: {e}");
        }
    });
    Gateway {
        port,
        token: TOKEN.to_string(),
        registry,
        store,
    }
}

impl Gateway {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// One admin-API call over real HTTP. The harness serves without ConnectInfo, so the
    /// admin session gate (SPEC §host.session) treats these calls as in-process, as the loopback guard
    /// does; the Host header reqwest sends is the loopback one the guard requires.
    pub(crate) async fn api(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = reqwest::Client::new().request(method, format!("{}{}", self.base(), path));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.expect("the admin API answers");
        let status = res.status().as_u16();
        let text = res.text().await.expect("a body");
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, value)
    }
}

/// A no-op client handler: the tests drive the session; the client side needs none.
pub(crate) struct NoopClient;
impl rmcp::ClientHandler for NoopClient {}

/// One rmcp client session against the gateway's /mcp/<name>, token-authenticated
/// the way a hosted client is. serve_client completes the initialize handshake.
pub(crate) async fn mcp(
    g: &Gateway,
    name: &str,
) -> rmcp::service::RunningService<rmcp::RoleClient, NoopClient> {
    let mut config =
        rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::default();
    config.uri = format!("{}/mcp/{name}", g.base()).into();
    config.auth_header = Some(g.token.clone());
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
    rmcp::service::serve_client(NoopClient, transport)
        .await
        .expect("the rmcp client connects and initializes")
}

/// The sorted tool names a session lists - the exact-set assertions read better.
pub(crate) async fn tool_names(
    client: &rmcp::service::RunningService<rmcp::RoleClient, NoopClient>,
) -> Vec<String> {
    let mut names: Vec<String> = client
        .list_tools(None)
        .await
        .expect("tools/list")
        .tools
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

/// One tool call through the session, the RAW text of the first content block -
/// byte-identity assertions (the L3 blob) need the wire bytes, not a re-parse.
pub(crate) async fn call_text(
    client: &rmcp::service::RunningService<rmcp::RoleClient, NoopClient>,
    tool: &str,
    args: Value,
) -> String {
    let mut params = rmcp::model::CallToolRequestParams::default();
    params.name = tool.to_string().into();
    params.arguments = args.as_object().cloned();
    let res = client.call_tool(params).await.expect("the tool answers");
    res.content
        .first()
        .and_then(|c| match c {
            rmcp::model::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// One tool call through the session, answer parsed back from its text content.
pub(crate) async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, NoopClient>,
    tool: &str,
    args: Value,
) -> Value {
    let text = call_text(client, tool, args).await;
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

/// Store one secret the way the panel does: GET the rev, PUT the value.
async fn store_secret(g: &Gateway, name: &str, value: &str) {
    let (s, listing) = g.api(reqwest::Method::GET, "/api/secrets", None).await;
    assert_eq!(s, 200, "{listing}");
    let rev = listing["rev"].as_u64().expect("a rev");
    let (s, body) = g
        .api(
            reqwest::Method::PUT,
            &format!("/api/secrets/{name}"),
            Some(json!({ "value": value, "rev": rev })),
        )
        .await;
    assert_eq!(s, 200, "{body}");
}

// --- one group per engine -------------------------------------------------------------------

#[tokio::test]
async fn mysql_group_round_trips_through_the_real_gateway() {
    let f = fresh(Kind::Mysql, "l2g_m").await;
    let g = boot(vec![("l2m", f.def.clone())]).await;
    let c = mcp(&g, "l2m").await;

    // The four-tool contract, exactly - no extras smuggled in, none missing.
    assert_eq!(
        tool_names(&c).await,
        ["mysql_describe_table", "mysql_inspect", "mysql_list_tables", "mysql_query"]
    );

    // A real query over the whole path: rmcp client -> HTTP -> adapter -> pool -> seed.
    let rows = call(
        &c,
        "mysql_query",
        json!({ "sql": "SELECT id, name FROM users ORDER BY id LIMIT 3" }),
    )
    .await;
    assert_eq!(rows["rowCount"], 3, "{rows}");
    let first = rows["rows"][0].as_object().expect("a row");
    assert_eq!(first["id"], "1"); // BIGINT rides as an exact string
    assert_eq!(first["name"], "张三 🙂");

    // The listing tool sees the seeded world: tables plus the view.
    let tables = call(&c, "mysql_list_tables", json!({ "grep": "users" })).await;
    let text = tables.to_string();
    assert!(text.contains("users"), "{tables}");
    assert!(text.contains("active_users"), "the view too: {tables}");
    // The database is named once, at the top - not repeated on every row.
    assert_eq!(tables["database"], f.def["database"], "{tables}");
    assert!(tables["tables"][0].get("schema").is_none(), "{tables}");

    // Describe hands back the server's own DDL, plus the foreign keys pointing IN,
    // which no DDL of this table shows.
    let db = f.def["database"].as_str().expect("the seeded database");
    let users = call(&c, "mysql_describe_table", json!({ "table": "users" })).await;
    assert_eq!(users["type"], "table", "{users}");
    let ddl = users["ddl"].as_str().unwrap_or_default();
    assert!(ddl.starts_with("CREATE TABLE `users`"), "{users}");
    assert!(ddl.contains("PRIMARY KEY (`id`)"), "{users}");
    assert_eq!(
        users["referencedBy"],
        json!([{
            "table": format!("{db}.orders"),
            "name": "fk_orders_user",
            "definition": format!("FOREIGN KEY (user_id) REFERENCES {db}.users (id)"),
        }]),
        "{users}"
    );
    let view = call(&c, "mysql_describe_table", json!({ "table": "active_users" })).await;
    assert_eq!(view["type"], "view", "{view}");
    assert!(view["ddl"].as_str().unwrap_or_default().contains("VIEW"), "{view}");
    assert!(view.get("size").is_none(), "a view has no storage: {view}");
    let miss = call(&c, "mysql_describe_table", json!({ "table": "nope" })).await;
    assert!(miss.as_str().is_some_and(|e| e.contains("mysql_list_tables")), "{miss}");

    // Diagnostics read the server's own views; this login has only its database, and
    // the refusal names the grants that fix it.
    let denied = call(&c, "mysql_inspect", json!({ "check": "activity" })).await;
    assert!(
        denied.as_str().is_some_and(|e| e.contains("PROCESS and SELECT on performance_schema")),
        "{denied}"
    );

    // The write contract as the code ships it (SPEC §testing.it: record the present):
    // mysql_query is NOT read-only - UPDATE runs, reports matched rows, and lands.
    let upd = call(
        &c,
        "mysql_query",
        json!({ "sql": "UPDATE users SET name = 'l2-edited' WHERE id = 8" }),
    )
    .await;
    assert_eq!(upd["affectedRows"], 1, "{upd}");
    let back = call(
        &c,
        "mysql_query",
        json!({ "sql": "SELECT name FROM users WHERE id = 8" }),
    )
    .await;
    assert_eq!(back["rows"][0]["name"], "l2-edited", "{back}");

    // Resources: one overview plus one entry per table, readable through rmcp.
    let res = c.list_resources(None).await.expect("resources/list");
    let db = f.def["database"].as_str().expect("the seeded database");
    let uri = |suffix: &str| format!("mysql://{db}/{suffix}");
    assert!(
        res.resources.iter().any(|r| *r.uri == uri("users")),
        "a users resource: {:?}",
        res.resources
    );
    assert!(
        res.resources.iter().any(|r| *r.uri == uri("active_users")),
        "the view too: {:?}",
        res.resources
    );
    let read = c
        .read_resource(rmcp::model::ReadResourceRequestParams::new(uri("users")))
        .await
        .expect("resources/read");
    assert!(!read.contents.is_empty());
}

#[tokio::test]
async fn pg_group_round_trips_through_the_real_gateway() {
    let f = fresh(Kind::Postgres, "l2g_p").await;
    let g = boot(vec![("l2p", f.def.clone())]).await;
    let c = mcp(&g, "l2p").await;

    assert_eq!(
        tool_names(&c).await,
        ["pg_describe_table", "pg_inspect", "pg_list_tables", "pg_query"]
    );

    let rows = call(
        &c,
        "pg_query",
        json!({ "sql": "SELECT id, name FROM users ORDER BY id LIMIT 3" }),
    )
    .await;
    assert_eq!(rows["rowCount"], 3, "{rows}");
    assert_eq!(rows["rows"][0]["name"], "张三 🙂", "{rows}");

    // The listing spans schemas the seed planted: public tables AND audit.log_entries.
    let tables = call(&c, "pg_list_tables", json!({})).await;
    let listed = tables["tables"].as_array().cloned().unwrap_or_default();
    let pair = |schema: &str, name: &str| {
        listed.iter().any(|t| {
            t["schema"] == json!(schema) && t["name"] == json!(name)
        })
    };
    assert!(pair("public", "users"), "{tables}");
    assert!(pair("audit", "log_entries"), "{tables}");
    assert!(pair("public", "active_users"), "the view lists too: {tables}");

    // Describe answers the column facts with pg's own types (the enum surfaces as
    // its udt name since the I3 product fix).
    let described = call(&c, "pg_describe_table", json!({ "table": "users" })).await;
    let text = described.to_string();
    assert!(text.contains("user_status"), "{described}");
    // One call carries the keys both ways: the primary key, and every table whose
    // foreign key points here - across schemas.
    assert_eq!(described["schema"], "public", "found on the search_path: {described}");
    assert_eq!(described["primaryKey"], json!(["id"]), "{described}");
    let referencing: Vec<&str> = described["referencedBy"]
        .as_array()
        .map(|r| r.iter().filter_map(|e| e["table"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        referencing.contains(&"public.orders") && referencing.contains(&"app.documents"),
        "{described}"
    );
    // A schema outside the search_path is named apart; the partial index keeps its
    // predicate, and the outgoing key names its target.
    let log = call(
        &c,
        "pg_describe_table",
        json!({ "schema": "audit", "table": "log_entries" }),
    )
    .await;
    let index = log["indexes"]
        .as_array()
        .and_then(|i| i.iter().find(|i| i["name"] == "log_entries_noted_at"))
        .unwrap_or_else(|| panic!("the partial index: {log}"));
    assert!(
        index["definition"].as_str().is_some_and(|d| d.contains("WHERE (note IS NOT NULL)")),
        "{log}"
    );
    assert!(log["foreignKeys"].to_string().contains("app.documents"), "{log}");
    let dotted = call(&c, "pg_describe_table", json!({ "table": "audit.log_entries" })).await;
    assert!(dotted.as_str().is_some_and(|e| e.contains("pass the parts apart")), "{dotted}");

    // The write contract as the code ships it: like mysql, pg_query runs DML.
    let upd = call(
        &c,
        "pg_query",
        json!({ "sql": "UPDATE users SET name = 'l2-edited' WHERE id = 8" }),
    )
    .await;
    // pg's driver reports the command tag's count as rowCount; mysql's answers
    // affectedRows - each tool stays honest to its own wire.
    assert_eq!(upd["rowCount"], 1, "{upd}");

    // Resources: schema-qualified uris, readable through rmcp.
    let res = c.list_resources(None).await.expect("resources/list");
    let db = url_db(&f.def);
    let uri = format!("pg://{db}/public.users");
    assert!(
        res.resources.iter().any(|r| *r.uri == uri),
        "a public.users resource: {:?}",
        res.resources
    );
    let read = c
        .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
        .await;
    assert!(read.is_ok(), "{read:?}");
}

#[tokio::test]
async fn redis_group_round_trips_through_the_real_gateway() {
    let f = fresh(Kind::Redis, "l2g_r").await;
    let g = boot(vec![("l2r", f.def.clone())]).await;
    let c = mcp(&g, "l2r").await;

    assert_eq!(tool_names(&c).await, ["redis_command", "redis_read", "redis_scan"]);

    // UTF-8 through the whole path, twice over: the command tool and the reader.
    let got = call(
        &c,
        "redis_command",
        json!({ "command": "GET", "args": ["users:1"] }),
    )
    .await;
    assert!(got.to_string().contains("张三"), "{got}");
    let read = call(&c, "redis_read", json!({ "key": "ns:h:profile" })).await;
    assert!(read.to_string().contains("f01"), "{read}");

    // The scan tool narrows by pattern server-side and keeps scanning until it holds
    // `limit` keys or the keyspace ends: a raw SCAN round under this sparse pattern
    // mostly answers `keys: []`, but one call here walks all ~3,000 keys.
    let scan = call(&c, "redis_scan", json!({ "pattern": "tree:l1:l2:*" })).await;
    assert_eq!(scan["done"], json!(true), "one call finishes a sparse walk: {scan}");
    let names: Vec<&str> = scan["keys"]
        .as_array()
        .map(|k| k.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        names.contains(&"tree:l1:l2:leaf1") && names.contains(&"tree:l1:l2:leaf2"),
        "the l2 subtree walks out: {scan}"
    );

    // `limit` pages a dense walk: every key, each page about `limit` keys.
    let size = call(&c, "redis_command", json!({ "command": "DBSIZE" })).await;
    let size = size.as_u64().unwrap_or_else(|| panic!("DBSIZE: {size}")) as usize;
    let mut cursor = Value::Null;
    let mut walked = std::collections::HashSet::new();
    let mut calls = 0;
    loop {
        let mut args = json!({ "limit": 500 });
        if !cursor.is_null() {
            args["cursor"] = cursor.clone();
        }
        let page = call(&c, "redis_scan", args).await;
        let keys = page["keys"].as_array().cloned().unwrap_or_default();
        calls += 1;
        let done = page["done"] == json!(true);
        assert!(done || keys.len() >= 500, "a page short of its limit: {}", keys.len());
        walked.extend(keys.into_iter().filter_map(|k| k.as_str().map(str::to_string)));
        if done {
            break;
        }
        cursor = page["cursor"].clone();
    }
    assert_eq!(walked.len(), size, "the walk reaches every key");
    assert!(calls <= size.div_ceil(500) + 1, "{calls} calls for {size} keys");

    // One resource: the keyspace overview, readable.
    let res = c.list_resources(None).await.expect("resources/list");
    assert_eq!(res.resources.len(), 1, "the overview: {:?}", res.resources);
    let read = c
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "redis://l2r/overview".to_string(),
        ))
        .await
        .expect("resources/read");
    assert!(!read.contents.is_empty());
}

// --- diagnostics: every check against a real server -----------------------------------------

/// The rows of an inspect reply.
fn inspect_rows(reply: &Value) -> Vec<Value> {
    reply["rows"].as_array().cloned().unwrap_or_default()
}

/// Ask `check` until `ready` accepts its rows - lock waits and table statistics land a
/// moment after the statement that causes them.
async fn inspect_until(
    c: &rmcp::service::RunningService<rmcp::RoleClient, NoopClient>,
    tool: &str,
    check: &str,
    ready: impl Fn(&[Value]) -> bool,
) -> Value {
    let mut reply = Value::Null;
    for _ in 0..100 {
        reply = call(c, tool, json!({ "check": check, "limit": 100 })).await;
        if ready(&inspect_rows(&reply)) {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{check} never showed what the test set up: {reply}");
}

/// The first row whose `field` (a string) starts with `prefix`.
fn row_with<'a>(rows: &'a [Value], field: &str, prefix: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|r| r[field].as_str().is_some_and(|v| v.starts_with(prefix)))
}

#[tokio::test]
async fn mysql_inspect_answers_every_check_for_a_monitoring_login() {
    let f = fresh(Kind::Mysql, "l2g_mi").await;
    let db = f.def["database"].as_str().expect("the seeded database").to_string();
    let root_url = engine(Kind::Mysql).await.root_url.clone();
    let db_url = format!("{root_url}/{db}");
    // The grants mysql_inspect's description asks for, on a login of its own.
    let mut root = sqlx::MySqlConnection::connect(&root_url).await.expect("root connects");
    for sql in [
        "CREATE USER IF NOT EXISTS 'it_ops'@'%' IDENTIFIED BY 'it_ops'".to_string(),
        "GRANT PROCESS ON *.* TO 'it_ops'@'%'".to_string(),
        "GRANT SELECT ON performance_schema.* TO 'it_ops'@'%'".to_string(),
        format!("GRANT ALL PRIVILEGES ON `{db}`.* TO 'it_ops'@'%'"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut root).await.expect("grant");
    }
    let mut def = f.def.clone();
    def["user"] = json!("it_ops");
    def["password"] = json!("it_ops");
    let g = boot(vec![("l2mi", def)]).await;
    let c = mcp(&g, "l2mi").await;

    // One open transaction blocks two sessions: a row lock and a metadata lock.
    let mut blocker = sqlx::MySqlConnection::connect(&db_url).await.expect("blocker");
    let blocker_id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut blocker)
        .await
        .expect("connection id");
    let mut tx = blocker.begin().await.expect("begin");
    sqlx::query("SELECT id FROM users WHERE id = 1 FOR UPDATE")
        .fetch_all(&mut *tx)
        .await
        .expect("row lock");
    sqlx::query("SELECT COUNT(*) FROM events").fetch_all(&mut *tx).await.expect("events read");
    let waiter = |setting: &'static str, sql: &'static str| {
        let url = db_url.clone();
        tokio::spawn(async move {
            let mut w = sqlx::MySqlConnection::connect(&url).await.expect("waiter connects");
            sqlx::query(setting).execute(&mut w).await.expect("bound the wait");
            sqlx::query(sql).execute(&mut w).await.map(|_| ())
        })
    };
    let row_wait = waiter(
        "SET SESSION innodb_lock_wait_timeout = 30",
        "UPDATE users SET name = name WHERE id = 1",
    );
    let mdl_wait = waiter(
        "SET SESSION lock_wait_timeout = 30",
        "ALTER TABLE events ADD COLUMN probe INT NULL",
    );
    let locks = inspect_until(&c, "mysql_inspect", "locks", |rows| {
        rows.iter().any(|r| r["kind"] == "row") && rows.iter().any(|r| r["kind"] == "metadata")
    })
    .await;
    let rows = inspect_rows(&locks);
    let row = row_with(&rows, "waiting_query", "UPDATE users").expect("the row wait");
    assert_eq!(row["kind"], "row", "{locks}");
    assert_eq!(row["locked"], json!(format!("{db}.users")), "{locks}");
    assert_eq!(row["blocking_pid"], json!(blocker_id), "{locks}");
    let mdl = row_with(&rows, "waiting_query", "ALTER TABLE events").expect("the metadata wait");
    assert_eq!(mdl["kind"], "metadata", "{locks}");
    assert_eq!(mdl["locked"], json!(format!("{db}.events")), "{locks}");
    assert_eq!(mdl["blocking_pid"], json!(blocker_id), "{locks}");
    // The blocker sleeps between statements, and activity still lists it: it holds a
    // transaction open.
    let activity = call(&c, "mysql_inspect", json!({ "check": "activity" })).await;
    assert!(
        inspect_rows(&activity).iter().any(|r| r["id"] == json!(blocker_id)),
        "{activity}"
    );
    tx.rollback().await.expect("release");
    row_wait.await.expect("joined").expect("the row wait completes");
    mdl_wait.await.expect("joined").expect("the metadata wait completes");

    // Work for the counters: a full scan of the PK-less events table.
    call(
        &c,
        "mysql_query",
        json!({ "sql": "SELECT COUNT(*) AS n FROM events WHERE kind = 'probe'" }),
    )
    .await;
    let scans = inspect_until(&c, "mysql_inspect", "table_scans", |rows| {
        rows.iter().any(|r| r["table"] == "events")
    })
    .await;
    let events = inspect_rows(&scans).into_iter().find(|r| r["table"] == "events");
    assert!(events.is_some_and(|r| r["rows_read_without_index"].is_u64()), "{scans}");

    let mut replies = std::collections::HashMap::new();
    for check in [
        "activity",
        "locks",
        "top_queries",
        "table_scans",
        "indexes",
        "fragmentation",
        "cache",
        "connections",
    ] {
        let reply = call(&c, "mysql_inspect", json!({ "check": check })).await;
        assert_eq!(reply["check"], check, "{reply}");
        replies.insert(check, reply);
    }
    let top = &replies["top_queries"];
    assert!(inspect_rows(top).first().is_some_and(|r| r["calls"].is_u64()), "{top}");
    let indexes = &replies["indexes"];
    assert!(
        inspect_rows(indexes)
            .iter()
            .any(|r| r["problem"] == "no primary key" && r["table"] == "events"),
        "{indexes}"
    );
    let cache = &replies["cache"]["summary"];
    assert!(cache["hit_pct"].is_number() && cache["size"].is_string(), "{cache}");
    let conns = &replies["connections"]["summary"];
    assert!(conns["connected"].as_u64().is_some_and(|n| n >= 1), "{conns}");
    assert!(conns["max_connections"].is_u64(), "{conns}");
}

#[tokio::test]
async fn pg_inspect_answers_every_check() {
    let f = fresh(Kind::Postgres, "l2g_pi").await;
    let url = f.def["url"].as_str().expect("the def url").to_string();
    let g = boot(vec![("l2pi", f.def.clone())]).await;
    let c = mcp(&g, "l2pi").await;

    // top_queries names what it is missing, then reads the extension once it exists.
    let missing = call(&c, "pg_inspect", json!({ "check": "top_queries" })).await;
    assert!(
        missing.as_str().is_some_and(|e| e.contains("CREATE EXTENSION pg_stat_statements")),
        "{missing}"
    );
    let root_url = engine(Kind::Postgres).await.root_url.clone();
    let (server, _) = root_url.rsplit_once('/').expect("a database in the root url");
    let mut root = sqlx::PgConnection::connect(&format!("{server}/{}", url_db(&f.def)))
        .await
        .expect("root connects to the test database");
    sqlx::query("CREATE EXTENSION pg_stat_statements")
        .execute(&mut root)
        .await
        .expect("the extension installs");

    // One open transaction blocks two sessions: a row lock and a table lock. All three
    // are this login's own, so pg_stat_activity shows their statements.
    let mut blocker = sqlx::PgConnection::connect(&url).await.expect("blocker");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("backend pid");
    let mut tx = blocker.begin().await.expect("begin");
    sqlx::query("UPDATE users SET name = name WHERE id = 1")
        .execute(&mut *tx)
        .await
        .expect("row lock");
    sqlx::query("SELECT count(*) FROM events").fetch_all(&mut *tx).await.expect("events read");
    let waiter = |sql: &'static str| {
        let url = url.clone();
        tokio::spawn(async move {
            let mut w = sqlx::PgConnection::connect(&url).await.expect("waiter connects");
            sqlx::query("SET lock_timeout = '30s'").execute(&mut w).await.expect("bound");
            sqlx::query(sql).execute(&mut w).await.map(|_| ())
        })
    };
    let row_wait = waiter("UPDATE users SET name = name WHERE id = 1");
    let table_wait = waiter("ALTER TABLE events ADD COLUMN probe int");
    let locks = inspect_until(&c, "pg_inspect", "locks", |rows| {
        row_with(rows, "waiting_query", "UPDATE users").is_some()
            && row_with(rows, "waiting_query", "ALTER TABLE events").is_some()
    })
    .await;
    let rows = inspect_rows(&locks);
    let row = row_with(&rows, "waiting_query", "UPDATE users").expect("the row wait");
    assert_eq!(row["waiting_for"], "ShareLock on transactionid", "{locks}");
    assert_eq!(row["blocking_pid"], json!(blocker_pid), "{locks}");
    let table = row_with(&rows, "waiting_query", "ALTER TABLE events").expect("the table wait");
    assert_eq!(table["waiting_for"], "AccessExclusiveLock on events", "{locks}");
    assert_eq!(table["blocking_pid"], json!(blocker_pid), "{locks}");
    let activity = call(&c, "pg_inspect", json!({ "check": "activity" })).await;
    let held = inspect_rows(&activity).into_iter().find(|r| r["pid"] == json!(blocker_pid));
    assert!(held.is_some_and(|r| r["state"] == "idle in transaction"), "{activity}");
    tx.rollback().await.expect("release");
    row_wait.await.expect("joined").expect("the row wait completes");
    table_wait.await.expect("joined").expect("the table wait completes");

    // Work for the counters: a sequential scan of events, on a session that then ends -
    // a backend flushes its statistics on exit.
    let mut scanner = sqlx::PgConnection::connect(&url).await.expect("scanner");
    sqlx::query("SELECT count(*) FROM events WHERE kind = 'probe'")
        .fetch_all(&mut scanner)
        .await
        .expect("scan");
    scanner.close().await.expect("scanner closes");
    inspect_until(&c, "pg_inspect", "table_scans", |rows| {
        rows.iter().any(|r| r["table"] == "events" && r["seq_scan"].is_u64())
    })
    .await;

    let mut replies = std::collections::HashMap::new();
    for check in [
        "activity",
        "locks",
        "top_queries",
        "table_scans",
        "vacuum",
        "indexes",
        "cache",
        "connections",
    ] {
        let reply = call(&c, "pg_inspect", json!({ "check": check })).await;
        assert_eq!(reply["check"], check, "{reply}");
        replies.insert(check, reply);
    }
    let top = &replies["top_queries"];
    assert!(inspect_rows(top).first().is_some_and(|r| r["calls"].is_u64()), "{top}");
    let indexes = inspect_rows(&replies["indexes"]);
    assert!(
        indexes.iter().any(|r| r["problem"] == "no primary key" && r["table"] == "events"),
        "{indexes:?}"
    );
    assert!(
        indexes.iter().any(|r| r["problem"] == "unused" && r["index"] == "log_entries_noted_at"),
        "{indexes:?}"
    );
    let vacuum = &replies["vacuum"];
    assert!(vacuum["summary"]["database_xid_age"].is_u64(), "{vacuum}");
    assert!(!inspect_rows(vacuum).is_empty(), "{vacuum}");
    let cache = &replies["cache"]["summary"];
    assert!(cache["shared_buffers"].is_string(), "{cache}");
    let conns = &replies["connections"]["summary"];
    assert!(conns["client_connections"].as_u64().is_some_and(|n| n >= 1), "{conns}");
}

#[tokio::test]
async fn stream_route_round_trips_three_directions_and_groups() {
    // SPEC §data.streams: the admin route over the real gateway — newest window,
    // before-page, after-page, the groups table, and the two 400s, exactly the
    // calls the panel's stream view makes.
    let f = fresh(Kind::Redis, "l2_stream").await;
    let g = boot(vec![("str", f.def.clone())]).await;

    let (status, latest) = g
        .api(reqwest::Method::GET, "/api/db/str/stream?key=stream:ticks", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(latest["type"], "stream");
    assert_eq!(latest["length"], json!(10_000));
    let entries = latest["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 100);
    assert_eq!(entries[0]["id"], json!("1700000999900-0"), "newest first");
    assert_eq!(latest["more"], json!(true));

    let (status, older) = g
        .api(
            reqwest::Method::GET,
            "/api/db/str/stream?key=stream:ticks&before=1700000990000-0&count=100",
            None,
        )
        .await;
    assert_eq!(status, 200);
    let older = older["entries"].as_array().expect("entries").clone();
    assert_eq!(
        older[0]["id"],
        json!("1700000989900-0"),
        "strictly older than the cursor, still newest-first"
    );

    let (status, after) = g
        .api(
            reqwest::Method::GET,
            "/api/db/str/stream?key=stream:ticks&after=1700000000000-0&count=5",
            None,
        )
        .await;
    assert_eq!(status, 200);
    let after: Vec<&str> = after["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        after,
        vec![
            "1700000999900-0",
            "1700000999800-0",
            "1700000999700-0",
            "1700000999600-0",
            "1700000999500-0"
        ],
        "the newest five above the cursor"
    );

    let (status, groups) = g
        .api(
            reqwest::Method::GET,
            "/api/db/str/stream/groups?key=stream:ticks",
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(groups["groups"][0]["pending"], json!(7));
    assert_eq!(groups["groups"][0]["lag"], json!(9_993));

    let (status, body) = g.api(reqwest::Method::GET, "/api/db/str/stream", None).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "key is required");
    let (status, body) = g
        .api(
            reqwest::Method::GET,
            "/api/db/str/stream?key=s&before=1-0&after=2-0",
            None,
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "pass only one of before / after — they page in opposite directions");
}


/// The database name out of a pg def's URL, for resource URIs.
fn url_db(def: &Value) -> String {
    let url = def["url"].as_str().expect("pg def carries a url");
    url.rsplit('/').next().unwrap_or_default().to_string()
}

// --- credentials as references --------------------------------------------------------------

#[tokio::test]
async fn credential_refs_close_the_loop_over_a_real_database() {
    // SPEC §host.vault/25 on real engines for the first time: the def holds ONLY the
    // reference, the vault holds the value (stored through the panel's own API), and
    // the engine still connects. Redis has no credential in this harness (no ACL on
    // the engine), so the loop runs on mysql's password and pg's URL.
    let fm = fresh(Kind::Mysql, "l2sec_m").await;
    let mut mysql_def = fm.def.clone();
    mysql_def["password"] = json!("${secret://it-pass}");
    let fp = fresh(Kind::Postgres, "l2sec_p").await;
    let mut pg_url = fp.def["url"].as_str().expect("url").to_string();
    let plain = ":it@";
    assert!(pg_url.contains(plain), "seed url carries it:it: {pg_url}");
    pg_url = pg_url.replace(plain, ":${secret://it-pass}@");
    let mut pg_def = fp.def.clone();
    pg_def["url"] = json!(pg_url);

    // The secret must sit in the vault BEFORE a def referencing it registers (the
    // adapter resolves refs at build time) - and it enters through the panel's own
    // API, on a first bare gateway. The vault is process-global, so the real
    // gateway below reads what this one stored.
    let bare = boot(vec![]).await;
    store_secret(&bare, "it-pass", "it").await;

    let g = boot(vec![("sec-m", mysql_def), ("sec-p", pg_def)]).await;

    // Both engines answer through the reference - the expansion happened at connect
    // time, inside the product, with no plaintext anywhere in between.
    let cm = mcp(&g, "sec-m").await;
    let rows = call(
        &cm,
        "mysql_query",
        json!({ "sql": "SELECT COUNT(*) AS n FROM users" }),
    )
    .await;
    assert!(rows.to_string().contains("8"), "{rows}");
    let cp = mcp(&g, "sec-p").await;
    let rows = call(
        &cp,
        "pg_query",
        json!({ "sql": "SELECT COUNT(*) AS n FROM users" }),
    )
    .await;
    assert!(rows.to_string().contains("8"), "{rows}");

    // The write-only rule (SPEC §host.vault): a listing answers names and rev, never values.
    let (s, listed) = g.api(reqwest::Method::GET, "/api/secrets", None).await;
    assert_eq!(s, 200);
    assert_eq!(listed["secrets"], json!(["it-pass"]), "{listed}");
    assert!(!listed.to_string().contains("it-pass-value"), "{listed}");

    // And the def the panel would re-edit shows the reference, not the secret.
    for name in ["sec-m", "sec-p"] {
        let (s, detail) = g
            .api(reqwest::Method::GET, &format!("/api/mcps/{name}/details"), None)
            .await;
        assert_eq!(s, 200, "{detail}");
        let text = detail.to_string();
        assert!(
            text.contains("${secret://it-pass}"),
            "the reference rides the def echo: {text}"
        );
        assert!(
            !text.contains(":it@") && !text.contains("\"password\":\"it\""),
            "no plaintext credential in {name}: {text}"
        );
    }
}

// --- hot-pluggable: stop really releases ----------------------------------------------------

#[tokio::test]
async fn stopping_an_mcp_releases_its_server_connections() {
    // The property SPEC §testing.it says this file can prove: disabled means RELEASED.
    // One gateway, three engines; each takes a tool call (so each pool holds a live
    // 'it' connection), then stop - and the server side must see zero of our
    // connections within 2 s. Start again, and the tool answers once more.
    let fm = fresh(Kind::Mysql, "l2hp_m").await;
    let fp = fresh(Kind::Postgres, "l2hp_p").await;
    let (fr, _neighbor) = fresh_redis_with_neighbor("l2hp_r").await;
    let g = boot(vec![
        ("hp-m", fm.def.clone()),
        ("hp-p", fp.def.clone()),
        ("hp-r", fr.def.clone()),
    ])
    .await;

    // One call each - the pools now hold open connections as user 'it'.
    let cm = mcp(&g, "hp-m").await;
    assert!(call(&cm, "mysql_query", json!({ "sql": "SELECT 1" })).await.to_string().contains("1"));
    let cp = mcp(&g, "hp-p").await;
    assert!(call(&cp, "pg_query", json!({ "sql": "SELECT 1" })).await.to_string().contains("1"));
    let cr = mcp(&g, "hp-r").await;
    assert!(call(&cr, "redis_command", json!({ "command": "PING" }))
        .await
        .to_string()
        .contains("PONG"));

    let mysql_root = Arc::new(tokio::sync::Mutex::new(
        sqlx::MySqlConnection::connect(&engine(Kind::Mysql).await.root_url)
            .await
            .expect("root connects"),
    ));
    // The count is scoped to THIS test's database, not the whole server: parallel
    // suites hold their own 'it' connections in their own databases, and they are
    // none of this stop's business.
    let my_db = fm.def["database"].as_str().expect("db name").to_string();
    let count_mysql = || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.processlist WHERE user = 'it' AND db = ?",
        )
        .bind(&my_db)
        .fetch_one(&mut *mysql_root.lock().await)
        .await
        .expect("processlist");
        n
    };
    let pg_root = Arc::new(tokio::sync::Mutex::new(
        sqlx::PgConnection::connect(&engine(Kind::Postgres).await.root_url)
            .await
            .expect("root connects"),
    ));
    let my_pg_db = url_db(&fp.def);
    let count_pg = || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE usename = 'it' AND datname = $1",
        )
        .bind(&my_pg_db)
        .fetch_one(&mut *pg_root.lock().await)
        .await
        .expect("pg_stat_activity");
        n
    };
    // The counting client must NOT sit on the leased db: CLIENT LIST is server-global
    // and a db-0 counting connection self-matches whenever the lease happens to be 0.
    // The neighbor lease is a different index by construction, so counting from there
    // can only ever see the MCP's own clients.
    let neighbor_db = _neighbor.def["db"].as_u64().expect("neighbor db");
    let host = engine(Kind::Redis).await.root_url.clone();
    let counting_url = format!("{}/{}", host.trim_end_matches('/'), neighbor_db);
    let redis_root = redis::Client::open(counting_url.as_str()).expect("counting url");
    let leased = fr.def["db"].as_u64().expect("the leased index") as usize;
    let count_redis = || async {
        let mut conn = redis_root
            .get_multiplexed_async_connection()
            .await
            .expect("redis");
        let list: String = redis::cmd("CLIENT")
            .arg("LIST")
            .query_async(&mut conn)
            .await
            .expect("client list");
        list.lines()
            .filter(|l| l.split_whitespace().any(|f| f == format!("db={leased}")))
            .count() as i64
    };

    // Connections are open before the stop - the assertions below would be vacuous
    // otherwise, so prove the baseline is non-zero first.
    assert!(count_mysql().await > 0, "mysql 'it' connections exist");
    assert!(count_pg().await > 0, "pg 'it' connections exist");
    assert!(count_redis().await > 0, "redis clients on db {leased} exist");

    // Stop all three through the panel's own routes.
    for name in ["hp-m", "hp-p", "hp-r"] {
        let (s, body) = g
            .api(reqwest::Method::POST, &format!("/api/mcps/{name}/stop"), None)
            .await;
        assert_eq!(s, 200, "{body}");
    }

    // Zero within the spec's 2 s window (polled - close is async on the far side).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let (m, p, r) = (count_mysql().await, count_pg().await, count_redis().await);
        if m == 0 && p == 0 && r == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "connections survived the stop: mysql={m} pg={p} redis={r}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // And back: start, and the tool answers again over a fresh pool.
    for name in ["hp-m", "hp-p", "hp-r"] {
        let (s, body) = g
            .api(reqwest::Method::POST, &format!("/api/mcps/{name}/start"), None)
            .await;
        assert_eq!(s, 200, "{body}");
    }
    let again = call(&cm, "mysql_query", json!({ "sql": "SELECT 1" })).await;
    assert!(again.to_string().contains("1"), "{again}");
}

// --- the parity guard -----------------------------------------------------------------------

#[tokio::test]
async fn gateway_rows_match_the_adminapi_contract() {
    // tests/adminapi.rs pins the same row shape through its oneshot harness
    // (tags_each_mcp_row_with_how_it_is_launched and friends). This copy of
    // sandbox/setup must drift nowhere: same def in, same row out - asserted field
    // for field against what that suite asserts, with startedAt (a clock) exempt.
    let g = boot(vec![(
        "t-mysql",
        json!({ "type": "mysql", "host": "127.0.0.1" }),
    )])
    .await;

    let (s, body) = g.api(reqwest::Method::GET, "/api/mcps", None).await;
    assert_eq!(s, 200, "{body}");
    let row = body["mcps"]
        .as_array()
        .expect("mcps array")
        .iter()
        .find(|m| m["name"] == json!("t-mysql"))
        .cloned()
        .unwrap_or_else(|| panic!("no row: {body}"));
    assert_eq!(row["source"], json!("config"));
    assert_eq!(row["type"], json!("mysql"));
    assert_eq!(row["tag"], json!("mysql"));
    assert_eq!(row["description"], json!(""));
    assert_eq!(row["group"], json!("default"));
    assert_eq!(row["lifecycle"], json!("started"));
    assert!(row.get("startedAt").is_some(), "a started row carries a time");
}
