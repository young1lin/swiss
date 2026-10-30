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

//! The Adapter trait and the type-erasure seam that lets one HTTP router serve every adapter's
//! own `ServerHandler` type — port of `adapters/types.ts` plus the plumbing the Node SDK gave
//! that build for free.
//!
//! Two views come out of `build()`:
//! - `http` — the streamable-HTTP surface the MCP endpoint drives (fresh server per request,
//!   both protocol eras statelessly, exactly as `createMcpHandler(…, { legacy: "stateless" })`);
//! - `probe` — the panel's in-session view (tools/resources/prompts listing, tool running),
//!   which opens a throwaway in-memory session per call like `openSession` did.

pub mod direct;
pub mod echo;
pub mod http;
pub mod mongo;
pub mod mongo_browser;
pub mod mongo_resources;
pub mod mysql;
pub mod mysql_browser;
pub mod mysql_resources;
pub mod pg;
pub mod pg_browser;
pub mod pg_resources;
pub mod proc;
pub mod proxy;
pub mod redis;
pub mod redis_browser;
pub mod redis_pubsub;
pub mod redis_resources;
pub mod remote;
pub mod resources;
pub mod rest;
pub mod sql;
pub mod tool_server;
pub mod zai;
mod zai_prompts;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use serde_json::Value;

use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::BrowserFlavor;

/// Live, shared tool-toggle state: the disabled tool names, shared between the adapter (reads
/// it when a request arrives) and the admin API (mutates it on toggle).
pub type ToolToggle = Arc<std::sync::RwLock<std::collections::HashSet<String>>>;

/// Live, shared resources on/off — the master switch.
pub type ResourceToggle = Arc<std::sync::atomic::AtomicBool>;

/// The HTTP-facing MCP service, type-erased. Clone-cheap behind an Arc.
#[async_trait]
pub trait HttpMcp: Send + Sync {
    async fn handle(&self, req: Request<Body>) -> Response;
    /// Cancel the service: tears down any in-flight exchanges. The evictor calls this when an
    /// entry's name is abandoned, so a cached service cannot leak per rename/delete.
    fn cancel(&self);
}

/// The panel's in-session view of an MCP — one throwaway session per call, JSON-level results
/// (the Node build carried `unknown[]` through paging the same way).
#[async_trait]
pub trait McpProbe: Send + Sync {
    /// `(items, next_cursor)` for the paging endpoint.
    async fn list(
        &self,
        kind: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<Value>, Option<String>), String>;
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String>;
    async fn read_resource(&self, uri: &str) -> Result<Value, String>;
}

/// What `Adapter::build()` hands back: the two views over one adapter's live connection.
#[derive(Clone)]
pub struct McpEndpoint {
    pub http: Arc<dyn HttpMcp>,
    pub probe: Arc<dyn McpProbe>,
}

struct HttpHalf<S2>
where
    S2: rmcp::ServerHandler + Send + 'static,
{
    service: rmcp::transport::streamable_http_server::StreamableHttpService<
        S2,
        rmcp::transport::streamable_http_server::session::never::NeverSessionManager,
    >,
}

#[async_trait]
impl<S2> HttpMcp for HttpHalf<S2>
where
    S2: rmcp::ServerHandler + Send + 'static,
{
    async fn handle(&self, req: Request<Body>) -> Response {
        use tower::ServiceExt;
        let response = self
            .service
            .clone()
            .oneshot(req)
            .await
            .expect("StreamableHttpService is infallible");
        let (parts, body) = response.into_parts();
        Response::from_parts(parts, Body::new(body))
    }

    fn cancel(&self) {
        // The service's own config field is the handle to its lifecycle: cancelling tears down
        // every in-flight exchange this endpoint is serving.
        self.service.config.cancellation_token.cancel();
    }
}

struct ProbeHalf<F2> {
    factory: Arc<F2>,
}

#[async_trait]
impl<S2, F2> McpProbe for ProbeHalf<F2>
where
    S2: rmcp::ServerHandler + Send + 'static,
    F2: Fn(crate::calls::CallSource) -> S2 + Send + Sync + 'static,
{
    async fn list(
        &self,
        kind: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<Value>, Option<String>), String> {
        use rmcp::model::PaginatedRequestParams;
        let client =
            crate::introspect::open_session((self.factory)(crate::calls::current_source())).await?;
        let params = cursor.map(|c| {
            let mut p = PaginatedRequestParams::default();
            p.cursor = Some(c);
            p
        });
        let out = match kind {
            "tools" => {
                let r = client.list_tools(params).await.map_err(|e| e.to_string())?;
                let items = r
                    .tools
                    .iter()
                    .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                    .collect();
                (items, r.next_cursor)
            }
            "resources" => {
                let r = client
                    .list_resources(params)
                    .await
                    .map_err(|e| e.to_string())?;
                let items = r
                    .resources
                    .iter()
                    .map(|x| serde_json::to_value(x).unwrap_or(Value::Null))
                    .collect();
                (items, r.next_cursor)
            }
            "prompts" => {
                let r = client
                    .list_prompts(params)
                    .await
                    .map_err(|e| e.to_string())?;
                let items = r
                    .prompts
                    .iter()
                    .map(|x| serde_json::to_value(x).unwrap_or(Value::Null))
                    .collect();
                (items, r.next_cursor)
            }
            other => return Err(format!("unknown list kind: {other}")),
        };
        let _ = client.cancel().await;
        Ok(out)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        use rmcp::model::CallToolRequestParams;
        let client =
            crate::introspect::open_session((self.factory)(crate::calls::current_source())).await?;
        let mut params = CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = arguments.as_object().cloned();
        let out = client.call_tool(params).await.map_err(|e| e.to_string())?;
        let _ = client.cancel().await;
        serde_json::to_value(&out).map_err(|e| e.to_string())
    }

    async fn read_resource(&self, uri: &str) -> Result<Value, String> {
        use rmcp::model::ReadResourceRequestParams;
        let client =
            crate::introspect::open_session((self.factory)(crate::calls::current_source())).await?;
        let params = ReadResourceRequestParams::new(uri);
        let out = client
            .read_resource(params)
            .await
            .map_err(|e| e.to_string())?;
        let _ = client.cancel().await;
        serde_json::to_value(&out).map_err(|e| e.to_string())
    }
}

/// Build both views over a per-request server factory — the shared spine of every adapter.
///
/// `make_server` must return a FRESH handler per call (the Node build's `Adapter.makeServer`,
/// introduced because the SDK server holds a single connected transport and races under
/// concurrent requests); the handler itself wraps whatever shared driver state the adapter
/// holds behind an Arc.
pub fn rmcp_endpoint<S, F>(make_server: F) -> McpEndpoint
where
    S: rmcp::ServerHandler + Send + 'static,
    F: Fn(crate::calls::CallSource) -> S + Send + Sync + 'static,
{
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    // legacy_session_mode(false): both protocol eras served statelessly from one endpoint —
    // the port of `legacy: "stateless"`. The BODY cap mirrors the Node router's 2 MB limit.
    const BODY_LIMIT: usize = 2 * 1024 * 1024;
    // One shared factory behind an Arc: the HTTP service's per-request factory and the probe's
    // per-session factory are the same adapter state, never two copies.
    let factory = Arc::new(make_server);
    let http_factory = factory.clone();
    // The factory is invoked inside the request task (the HTTP service processes a request
    // inline, and the probe session is opened inside the panel scope), so reading the call
    // context HERE — at factory time — is the one point both paths carry it. It rides into the
    // server instance from there (see calls::CallSource).
    let service = StreamableHttpService::new(
        move || Ok(http_factory(crate::calls::current_source())),
        Arc::new(
            rmcp::transport::streamable_http_server::session::never::NeverSessionManager::default(),
        ),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_max_request_body_bytes(BODY_LIMIT),
    );
    McpEndpoint {
        http: Arc::new(HttpHalf { service }),
        probe: Arc::new(ProbeHalf { factory }),
    }
}

/// How much of one MCP's traffic the host may keep on disk. Honoured by the traffic log
/// (here, host-side) and by the adapter's own call logging. Memory declares
/// `MetadataOnly`: its arguments and results are the user's notes, and a log must never
/// become a second copy of them (SPEC §memory.mcp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogPolicy {
    /// Arguments and clipped results, as every adapter always had.
    #[default]
    Full,
    /// Method, tool name, timing, outcome and opaque ids - never arguments or results.
    MetadataOnly,
    /// Nothing at all.
    Disabled,
}

/// One hosted MCP's adapter — port of `adapters/types.ts`. Default methods stand in for
/// TypeScript's optional members, so a new adapter implements two methods and inherits the rest.
#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &str;

    /// Build the MCP endpoint once (holding its own connection). Not bound to a transport.
    async fn build(&self) -> Result<McpEndpoint, String>;

    /// Reachability probe. `None` means this kind deliberately has no ping — http/rest are
    /// metered third-party endpoints and must not be polled every 15 s. Do not "fix" this.
    async fn ping(&self) -> Option<Result<(), String>> {
        None
    }

    /// Close DB connections / children on shutdown. Best effort by contract.
    async fn close(&self) {}

    /// Captured stderr of a spawned child, for the panel's log view. proc only.
    fn logs(&self) -> Option<String> {
        None
    }

    /// Root PIDs of any spawned subtree, so the memory view knows what to measure. proc only.
    fn pids(&self) -> Vec<u32> {
        Vec::new()
    }

    /// Follow a registry rename, so calls keep being logged under the MCP's current name.
    fn rename(&self, _name: &str) {}

    fn browser(&self) -> Option<BrowserFlavor> {
        None
    }

    fn tool_toggle(&self) -> Option<ToolToggle> {
        None
    }
    fn resource_toggle(&self) -> Option<ResourceToggle> {
        None
    }

    /// What the host may log of this MCP's traffic. Full unless the adapter says otherwise.
    fn log_policy(&self) -> LogPolicy {
        LogPolicy::Full
    }
}

/// The child-row wiring a proc MCP carries (SPEC §process.children): the shared
/// registry its spawned child registers into, plus the stop hook that is EXACTLY what
/// `POST /api/mcps/{name}/stop` does — stop, then persist as disabled, so an MCP stopped
/// from the Process page does not come back at the next boot.
///
/// Built by the composition root, which owns the swiss-host child registry, this crate's
/// MCP registry and the managed store. `None` reaches the adapter in tests and in the
/// connection probe, whose children live for a handshake and nobody can click.
pub struct McpChildSeat {
    children: std::sync::Arc<swiss_host::services::children::ChildRegistry>,
    /// The MCP's CURRENT name, shared with the stop closure (SPEC §mcp.registry):
    /// a rename keeps the running adapter, so both the row's label and the stop
    /// path must answer to the name the entry carries now - never the one this
    /// seat was built with.
    name: std::sync::Arc<std::sync::RwLock<String>>,
    stop: std::sync::Arc<dyn swiss_host::services::children::ChildStop>,
}

impl McpChildSeat {
    pub fn new(
        children: std::sync::Arc<swiss_host::services::children::ChildRegistry>,
        name: &str,
        registry: &std::sync::Arc<crate::registry::Registry>,
        store: &std::sync::Arc<swiss_host::managed::ManagedStore>,
    ) -> Self {
        let mcp_registry = std::sync::Arc::clone(registry);
        let managed = std::sync::Arc::clone(store);
        let name = std::sync::Arc::new(std::sync::RwLock::new(name.to_string()));
        let stop_cell = std::sync::Arc::clone(&name);
        McpChildSeat {
            children,
            name,
            stop: swiss_host::services::children::child_stop_fn(move || {
                let mcp_registry = std::sync::Arc::clone(&mcp_registry);
                let managed = std::sync::Arc::clone(&managed);
                let cell = std::sync::Arc::clone(&stop_cell);
                async move {
                    let mcp = cell
                        .read()
                        .map(|n| n.clone())
                        .unwrap_or_default();
                    mcp_registry.stop(&mcp).await?;
                    managed
                        .set_enabled(&mcp, false)
                        .map_err(|err| err.to_string())
                }
            }),
        }
    }

    /// The row's stop path, the one thing the children route reaches - and the
    /// test's way in without a live child.
    pub async fn stop(&self) -> Result<(), String> {
        self.stop.stop().await
    }

    /// Follow a registry rename (SPEC §mcp.registry): the live name the stop
    /// closure resolves and new rows label with.
    pub fn rename(&self, new_name: &str) {
        if let Ok(mut name) = self.name.write() {
            *name = new_name.to_string();
        }
    }

    /// Relabel the already-registered row of a live child (no-op without one).
    pub fn relabel(&self, pid: u32, new_name: &str) {
        self.children.relabel(pid, new_name);
    }

    /// Register the just-spawned child at the point the adapter records the pid for the
    /// ledger (SPEC §process.children). The guard rides LiveChild, so the row vanishes
    /// exactly when the child does.
    pub fn register(&self, pid: u32) -> Option<swiss_host::services::children::ChildGuard> {
        let label = self
            .name
            .read()
            .map(|n| n.clone())
            .unwrap_or_default();
        self.children
            .register(swiss_host::services::children::ChildSpec {
                pid,
                owner: "mcp".to_string(),
                kind: swiss_host::services::children::ChildKind::Mcp,
                label,
                stop: Some(std::sync::Arc::clone(&self.stop)),
            })
    }
}

/// Map a config/managed ServerDef to its Adapter — port of `factory.ts` minus the
/// third-party module door (ADR-001: an external adapter becomes a proc or http MCP).
/// `${ENV_VAR}` and `${secret://name}` refs are expanded HERE, never at load, so persisted defs
/// keep the reference. A vault reference that names a secret this machine does not hold REFUSES
/// the build (SPEC §host.refs): an absent credential is a configuration error the operator can fix
/// in one panel visit, not an empty password to debug on someone else's server.
/// `log` is the call log every adapter records into - the caller's, so an app and its
/// adapters share exactly one view of what was called.
pub fn make_adapter(
    raw_def: &ServerDef,
    name: &str,
    log: &std::sync::Arc<crate::calls::CallLog>,
    child_seat: Option<McpChildSeat>,
) -> Result<Arc<dyn Adapter>, String> {
    let def =
        swiss_host::config::resolve_def_checked(raw_def).map_err(|e| format!("{name}: {e}"))?;
    match def.type_() {
        "echo" => Ok(Arc::new(echo::EchoAdapter::new(name, log.clone()))),
        "mysql" => {
            let engine = mysql::MysqlEngine::new(&def, name)?;
            Ok(Arc::new(direct::DirectAdapter::new(&def, name, engine, log.clone())))
        }
        // MariaDB speaks the MySQL wire protocol (SPEC §mcp.panel): the type stays "mariadb" in the
        // def — identity, panel tag, icon — while the engine is the mysql one. A def that says
        // what it points at is worth more than an alias that erases it.
        "mariadb" => {
            let engine = mysql::MysqlEngine::new(&def, name)?;
            Ok(Arc::new(direct::DirectAdapter::new(&def, name, engine, log.clone())))
        }
        "pg" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, pg::PgEngine::new(&def, name), log.clone()))),
        "redis" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, redis::RedisEngine::new(&def, name), log.clone()))),
        // MongoDB (SPEC §mcp.db, ADR-030): the driver's own pool, opened on the first call.
        "mongo" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, mongo::MongoEngine::new(&def, name)?, log.clone()))),
        "rest" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, rest::RestEngine::new(&def, name)?, log.clone()))),
        "proc" => Ok(Arc::new(proc::ProcAdapter::new(&def, name, log.clone(), child_seat))),
        // The zai-vision type: the @z_ai/mcp-server GLM vision tools compiled in natively
        // (replacing the Node child a proc def used to spawn) - see zai.rs for the port map.
        "zai-vision" => Ok(Arc::new(direct::DirectAdapter::new(
            &def,
            name,
            zai::ZaiEngine::new(&def, name)?,
            log.clone(),
        ))),
        // The figma type is an http+oauth def with every choice already made (SPEC §mcp.figma):
        // expand to the full http def, then build exactly the adapter a hand-written def
        // would get — refresh, badge and authorize flows need no case of their own.
        "figma" => {
            let mut expanded = raw_def.clone();
            expanded.set("type", serde_json::json!("http"));
            expanded.set("url", serde_json::json!(crate::oauth::FIGMA_MCP_URL));
            expanded.set("auth", serde_json::json!("oauth"));
            make_adapter(&expanded, name, log, child_seat)
        }
        "http" => Ok(Arc::new(http::HttpAdapter::new(&def, name, log.clone())?)),
        other => Err(format!(
            "Unknown adapter type: {other} (built-in: echo | mysql | mariadb | pg | redis | mongo | proc | http | rest | figma | zai-vision)"
        )),
    }
}

#[cfg(test)]
mod tests {
    //! Ported from the "makeAdapter routes by type" block of the Node build's
    //! `test/direct-adapters.test.ts`. Rust has no `instanceof`, so the routing is pinned through
    //! `kind()` — which is what the panel and the call log read anyway.
    use super::*;
    use serde_json::json;

    fn def(v: Value) -> swiss_host::config::ServerDef {
        match v {
            Value::Object(o) => swiss_host::config::ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    fn kind_of(v: Value) -> Result<String, String> {
        make_adapter(&def(v), "an-mcp", &crate::calls::test_log(), None).map(|a| a.kind().to_string())
    }

    fn rest_def() -> Value {
        json!({
            "type": "rest",
            "tools": [{ "name": "t", "request": { "method": "GET", "path": "/" } }],
        })
    }

    #[tokio::test]
    async fn the_seat_stops_and_persists_under_the_live_name() {
        // SPEC §process.children + §mcp.registry: a rename keeps the running adapter,
        // so the children row's stop must resolve under the name the entry carries
        // NOW. Before the live-name cell the closure captured the build-time name -
        // after a rename the Process page's Stop answered "unknown MCP: old" and
        // left the disabling unpersisted: a dead button.
        let dir = std::env::temp_dir().join(format!("swiss-seat-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let calls = crate::calls::test_log();
        let registry = crate::registry::Registry::new(3_600_000, calls.clone());
        let managed = std::sync::Arc::new(swiss_host::managed::ManagedStore::open_at(
            dir.join("managed.json"),
        ));
        let children = swiss_host::services::children::ChildRegistry::new();
        let def = def(json!({ "type": "proc", "command": "node" }));
        let adapter = make_adapter(&def, "old", &calls, None).expect("the adapter builds");
        registry
            .register("old", crate::registry::Source::Managed, def, adapter)
            .expect("registered");
        let seat = McpChildSeat::new(children.clone(), "old", &registry, &managed);
        // A live child's row registers under the build-time name, as at spawn.
        let _row = seat.register(4242).expect("the row registers");
        assert_eq!(children.rows()[0].label, "old");
        // The production flow: registry.rename relabels through adapter.rename,
        // whose seat follows (ProcAdapter::rename drives both calls below).
        registry.rename("old", "renamed").await.expect("the rename lands");
        seat.rename("renamed");
        seat.relabel(4242, "renamed");
        assert_eq!(children.rows()[0].label, "renamed", "the row answers to the current name");
        seat.stop().await.expect("stop resolves under the live name");
        assert_eq!(
            managed.enabled_for("renamed"),
            Some(false),
            "the disabling persisted under the new name"
        );
        assert_eq!(managed.enabled_for("old"), None, "no state under the vacated name");
    }

    #[test]
    fn routes_every_built_in_type_to_its_adapter() {
        assert_eq!(kind_of(json!({ "type": "echo" })), Ok("echo".into()));
        assert_eq!(
            kind_of(json!({ "type": "mysql", "host": "x" })),
            Ok("mysql".into())
        );
        assert_eq!(
            kind_of(json!({ "type": "pg", "url": "x" })),
            Ok("pg".into())
        );
        assert_eq!(
            kind_of(json!({ "type": "redis", "host": "x" })),
            Ok("redis".into())
        );
        assert_eq!(
            kind_of(json!({ "type": "proc", "command": "npx -y x" })),
            Ok("proc".into())
        );
        assert_eq!(
            kind_of(json!({ "type": "http", "url": "https://example.test/mcp" })),
            Ok("http".into())
        );
        assert_eq!(kind_of(rest_def()), Ok("rest".into()));
    }

    #[test]
    fn refuses_an_unknown_type_and_names_the_built_ins() {
        let err = kind_of(json!({ "type": "nope" })).unwrap_err();
        assert!(err.contains("Unknown adapter type: nope"), "{err}");
        // The message is the whole help a user gets in the panel, so it lists what IS available.
        for built_in in ["echo", "mysql", "pg", "redis", "proc", "http", "rest"] {
            assert!(err.contains(built_in), "{built_in} missing from: {err}");
        }
        // A def with no type at all lands in the same arm rather than panicking.
        assert!(kind_of(json!({ "host": "x" }))
            .unwrap_err()
            .contains("Unknown adapter type"));
    }

    #[test]
    fn a_def_that_cannot_be_configured_fails_at_make_time_not_at_the_first_request() {
        // The registry reports a start error the operator can see; a lazy failure would surface
        // as one broken tool call much later.
        assert!(kind_of(json!({ "type": "http" }))
            .unwrap_err()
            .contains("needs a url"));
        assert!(kind_of(json!({ "type": "rest" }))
            .unwrap_err()
            .contains("`tools` array"));
        assert!(
            kind_of(json!({ "type": "rest", "tools": [{ "name": "t" }] }))
                .unwrap_err()
                .contains("has no request")
        );
    }

    #[tokio::test]
    async fn http_and_rest_have_no_health_ping_on_purpose() {
        // They are metered third-party endpoints: polling them every 15 s would bill the user for
        // health checks. `None` means "this kind has no probe", not "the probe failed".
        let http = make_adapter(
            &def(json!({ "type": "http", "url": "https://example.test/mcp" })),
            "an-mcp",
            &crate::calls::test_log(),
            None,
        )
        .expect("http adapter");
        assert!(http.ping().await.is_none());

        let rest = make_adapter(&def(rest_def()), "an-mcp", &crate::calls::test_log(), None)
            .expect("rest adapter");
        assert!(rest.ping().await.is_none());
    }

    #[test]
    fn only_the_db_adapters_carry_toggles_and_a_browser() {
        // The toggles are the DirectAdapter shell's; echo has neither, which is what makes the
        // panel hide those controls for it.
        let echo = make_adapter(
            &def(json!({ "type": "echo" })),
            "an-mcp",
            &crate::calls::test_log(),
            None,
        )
        .expect("echo");
        assert!(echo.tool_toggle().is_none());
        assert!(echo.browser().is_none());

        let mysql = make_adapter(
            &def(json!({ "type": "mysql", "host": "x", "database": "shop" })),
            "an-mcp",
            &crate::calls::test_log(),
            None,
        )
        .expect("mysql");
        assert!(mysql.tool_toggle().is_some());
        assert!(mysql.resource_toggle().is_some());
        assert!(mysql.browser().is_some());

        // ...and the browser needs a database to scope itself to, so a server-wide mysql def has
        // the toggles but no browser tab.
        let unscoped = make_adapter(
            &def(json!({ "type": "mysql", "host": "x" })),
            "an-mcp",
            &crate::calls::test_log(),
            None,
        )
        .expect("mysql");
        assert!(unscoped.tool_toggle().is_some());
        assert!(unscoped.browser().is_none());
    }

    #[test]
    fn env_refs_are_expanded_at_make_time_so_the_persisted_def_keeps_the_reference() {
        // The def the registry holds keeps `${...}`; only the adapter's copy sees the value. A
        // literal that survived here would mean the secret had been written into managed.json.
        let raw = def(json!({ "type": "http", "url": "${SWISS_TEST_MISSING_URL}" }));
        // An unset ref resolves to nothing, which is exactly the "needs a url" error — proof the
        // expansion ran, since the unexpanded string is a perfectly good non-empty url.
        let err = match make_adapter(&raw, "an-mcp", &crate::calls::test_log(), None) {
            Err(err) => err,
            Ok(_) => panic!("an unset url ref must not build an http adapter"),
        };
        assert!(err.contains("needs a url"), "{err}");
        assert_eq!(raw.get_str("url"), Some("${SWISS_TEST_MISSING_URL}"));
    }

    #[test]
    fn a_figma_def_builds_the_oauth_http_adapter_and_keeps_its_own_shape() {
        // The figma type is sugar over http+oauth (SPEC §mcp.figma): the built adapter IS the http
        // one — kind is what the tag and the authorize guard read — while the def the registry
        // holds stays type figma with no url or auth key on it, exactly as the panel wrote it.
        let raw = def(json!({ "type": "figma", "description": "Figma design files" }));
        let adapter = make_adapter(&raw, "figma", &crate::calls::test_log(), None).expect("figma builds");
        assert_eq!(adapter.kind(), "http");
        assert_eq!(raw.type_(), "figma");
        assert_eq!(raw.get_str("url"), None);
        assert_eq!(raw.get_str("auth"), None);
        assert!(crate::oauth::is_oauth(&raw));
    }

    #[test]
    fn a_figma_def_hands_its_proxy_to_the_http_adapter() {
        // The proxy is the one network key a figma def takes (SPEC §mcp.figma): the expansion
        // keeps it, so the http adapter validates it like its own - a socks5 URL is a start
        // error, not a first-call mystery - and a usable one builds.
        let socks = def(json!({ "type": "figma", "proxy": "socks5://127.0.0.1:1080" }));
        let err = match make_adapter(&socks, "figma", &crate::calls::test_log(), None) {
            Err(err) => err,
            Ok(_) => panic!("a socks5 proxy must not build a figma adapter"),
        };
        assert!(err.contains("proxy must be an http"), "{err}");
        let http = def(json!({ "type": "figma", "proxy": "http://127.0.0.1:7890" }));
        assert!(make_adapter(&http, "figma", &crate::calls::test_log(), None).is_ok());
        assert_eq!(http.get_str("proxy"), Some("http://127.0.0.1:7890"));
    }
}
