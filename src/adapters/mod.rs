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
#[cfg(feature = "mongo")]
pub mod mongo;
#[cfg(feature = "mongo")]
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
pub mod redis_resources;
pub mod resources;
pub mod rest;
pub mod sql;
pub mod tool_server;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use serde_json::Value;

use crate::config::ServerDef;
use crate::dbbrowser_api::BrowserFlavor;

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
}

/// Map a config/managed ServerDef to its Adapter — port of `factory.ts` minus the
/// third-party module door (ADR-001: an external adapter becomes a proc or http MCP).
/// `${ENV_VAR}` refs are expanded HERE, never at load, so persisted defs keep the reference.
pub fn make_adapter(raw_def: &ServerDef, name: &str) -> Result<Arc<dyn Adapter>, String> {
    let def = crate::config::resolve_def(raw_def);
    match def.type_() {
        "echo" => Ok(Arc::new(echo::EchoAdapter::new(name))),
        "mysql" => {
            let engine = mysql::MysqlEngine::new(&def, name)?;
            Ok(Arc::new(direct::DirectAdapter::new(&def, name, engine)))
        }
        "pg" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, pg::PgEngine::new(&def, name)))),
        "redis" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, redis::RedisEngine::new(&def, name)))),
        "rest" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, rest::RestEngine::new(&def, name)?))),
        "proc" => Ok(Arc::new(proc::ProcAdapter::new(&def, name))),
        "http" => Ok(Arc::new(http::HttpAdapter::new(&def, name)?)),
        #[cfg(feature = "mongo")]
        "mongo" => Ok(Arc::new(direct::DirectAdapter::new(&def, name, mongo::MongoEngine::new(&def, name)?))),
        #[cfg(not(feature = "mongo"))]
        "mongo" => Err("Mongo adapter is unavailable in this build; rebuild with --features mongo".into()),
        other => Err(format!(
            "Unknown adapter type: {other} (built-in: echo | mysql | pg | redis | mongo | proc | http | rest)"
        )),
    }
}
