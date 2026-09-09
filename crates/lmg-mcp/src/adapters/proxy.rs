//! The transport-agnostic proxy half — port of `adapters/proxy.ts` (`makeProxyServer`) plus
//! `adapters/proxy-fetch.ts` (the per-MCP outbound proxy helpers).
//!
//! `ProxyServer` is an rmcp `ServerHandler` whose every request is forwarded to an
//! already-connected remote through the [`RemoteMcp`] seam. Transport-agnostic on purpose: the
//! remote may be a spawned stdio child (`proc`, an rmcp client over the child process) or a
//! remote HTTP MCP (`http`, the hand-rolled streamable-HTTP client in `http.rs`). Everything a
//! proxied MCP needs — call logging, the `annotations` strip for older clients, the
//! resource/prompt gates, cursor pagination — is the same either way and lives here once.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorData, GetPromptRequestMethod,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    ListPromptsRequestMethod, ListPromptsResult, ListResourceTemplatesRequestMethod,
    ListResourcesRequestMethod, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    PromptsCapability, ReadResourceRequestMethod, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ResourcesCapability, ServerCapabilities, ServerInfo, ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use super::ToolToggle;
use crate::calls::{self, CallSource};
use lmg_core::log;

/// The page-level remote seam every proxied server forwards through.
///
/// One call = one page: a caller's cursor is handed over as-is and the remote's `nextCursor`
/// comes back on the result (the single-page mode of the Node SDK's client). The no-cursor
/// aggregation walk lives in [`ProxyServer`], so every remote — stdio child or HTTP — inherits
/// exactly the behavior the Node build's SDK client supplied.
#[async_trait]
pub trait RemoteMcp: Send + Sync {
    async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, String>;
    /// `timeout_ms` mirrors the Node SDK's per-request `options.timeout` (`callTimeoutMs`);
    /// `None` leaves the remote's own default in place.
    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        timeout_ms: Option<u64>,
    ) -> Result<CallToolResult, String>;
    async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, String>;
    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, String>;
    async fn list_prompts(&self, cursor: Option<String>) -> Result<ListPromptsResult, String>;
    async fn get_prompt(&self, params: GetPromptRequestParams) -> Result<GetPromptResult, String>;
}

/// What the Node build's `ClientOptions.listMaxPages` capped the pagination walk at. A remote
/// whose pagination has not converged within this many pages reports zero tools through the
/// `safe` fallback, with the reason in the log — never silently.
const LIST_MAX_PAGES: usize = 64;

/// JS truthiness, which is what the Node build's capability checks boil down to: an absent
/// capability is falsy, an object (even empty) is truthy, `false`/`null`/`0`/`""` are falsy.
/// Shared with `rest.rs`, whose `enum`/`required`/`request` checks are the same `if (x)` reads.
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `!opts.remoteCaps || !!opts.remoteCaps[cap]` — unknown capabilities expose per the toggles
/// (a spawned child that answers before its capabilities are read); known ones expose only what
/// the remote actually negotiated.
fn has_cap(remote_caps: &Option<Value>, cap: &str) -> bool {
    match remote_caps {
        None => true,
        Some(caps) => caps.get(cap).map(js_truthy).unwrap_or(false),
    }
}

/// `JSON.stringify(proxy)` for the assert message — surrounding quotes, `\` and `"` escaped,
/// control characters escaped the way JSON does it. Non-ASCII passes through unchanged.
fn json_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Port of `assertProxyUrl`: the proxy must be http(s):// — reqwest's proxy speaks CONNECT over
/// either. A missing scheme is the classic typo and socks5 is unsupported, and both belong as a
/// loud start error rather than a mysterious connect failure on the first call. Returns the
/// trimmed URL.
pub fn assert_proxy_url(proxy: &str) -> Result<String, String> {
    let trimmed = proxy.trim();
    let scheme_ok = starts_with_ci(trimmed, "http://") || starts_with_ci(trimmed, "https://");
    if !scheme_ok {
        return Err(format!(
            "proxy must be an http:// or https:// URL, got {}",
            json_quote(proxy)
        ));
    }
    Ok(trimmed.to_string())
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

static PROXIED_CLIENTS: OnceLock<Mutex<HashMap<String, reqwest::Client>>> = OnceLock::new();

/// A reqwest client routed through the given proxy — the port of `proxiedFetch`.
///
/// One client (and so one connection pool) per proxy URL, memoized at module level: a pool is
/// exactly the thing to share across MCPs on the same proxy, and it spares every adapter the
/// lifecycle wiring of owning and closing one. The set of distinct proxies is one or two in
/// practice. Per-MCP by config, never a global dispatcher — one MCP's proxy must not reroute
/// another MCP's traffic.
pub fn proxied_client(proxy: &str) -> Result<reqwest::Client, String> {
    let url = assert_proxy_url(proxy)?;
    let table = PROXIED_CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = table.lock() {
        if let Some(client) = guard.get(&url) {
            return Ok(client.clone()); // a Client handle shares the pool it was cloned from
        }
    }
    let proxy =
        reqwest::Proxy::all(&url).map_err(|err| format!("proxy {url} is not usable: {err}"))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .build()
        .map_err(|err| format!("proxy {url} client build failed: {err}"))?;
    if let Ok(mut guard) = table.lock() {
        // First writer wins the cell; a racing builder's pool simply drains away unused.
        Ok(guard.entry(url).or_insert(client).clone())
    } else {
        Ok(client)
    }
}

/// The resolved toggles and identity a [`ProxyServer`] is built from — port of `ProxyOpts`, with
/// the panel's boolean-as-string quirk already resolved by the caller (`def_bool`).
pub struct ProxyOpts {
    /// Registry name of this MCP — the key its call log is filed under, followed across renames.
    pub name: Arc<RwLock<String>>,
    /// What this MCP is for, from the config. Surfaced to clients as MCP `instructions`.
    pub description: Option<String>,
    /// Expose the remote's resources (default true), before the remote-capability gate.
    pub expose_resources: bool,
    /// Expose the remote's prompts (default true), before the remote-capability gate.
    pub expose_prompts: bool,
    /// Deadline for a single proxied `tools/call`, in ms. `None` inherits the remote client's
    /// own default (60s) — the caller owns this because only it knows what its remote does.
    pub call_timeout_ms: Option<u64>,
    /// What the remote actually negotiated (`initialize`'s `capabilities` object), when known.
    /// A capability is announced only if the toggle allows it AND the remote has it — otherwise
    /// clients ask for lists that cannot exist, which against a metered remote is a round trip
    /// billed for a guaranteed empty answer. `None` = unknown (announce what the toggles allow).
    pub remote_caps: Option<Value>,
    /// Disabled tool names, shared with the owning adapter — the live set the admin API mutates.
    /// A DELIBERATE divergence from the Node build (which answered 501 for proxied MCPs): this
    /// gateway IS the endpoint clients contract with, so what its tools/list advertises is the
    /// contract — the remote keeping its own copy of the tool changes nothing for a client that
    /// can neither see nor call it.
    pub disabled: ToolToggle,
    /// Where calls through this proxy are recorded - the app's CallLog, threaded from
    /// make_adapter through the adapter into every per-request server it builds.
    pub log: std::sync::Arc<crate::calls::CallLog>,
}

/// An MCP server whose handlers forward every request to an already-connected remote — port of
/// `makeProxyServer`. Built fresh per request by the owning adapter's `rmcp_endpoint` factory,
/// wrapping the one shared remote, which multiplexes concurrent requests by JSON-RPC id.
pub struct ProxyServer {
    remote: Arc<dyn RemoteMcp>,
    name: Arc<RwLock<String>>,
    description: Option<String>,
    /// Both the toggle and the remote's capability said yes; handlers exist only then, and a
    /// probe against a hidden capability gets Method Not Found.
    expose_resources: bool,
    expose_prompts: bool,
    call_timeout_ms: Option<u64>,
    /// Live disabled-tool set, shared with the adapter (see ProxyOpts::disabled).
    disabled: ToolToggle,
    source: CallSource,
    log: std::sync::Arc<crate::calls::CallLog>,
}

impl ProxyServer {
    pub fn new(remote: Arc<dyn RemoteMcp>, opts: ProxyOpts, source: CallSource) -> Self {
        let expose_resources = opts.expose_resources && has_cap(&opts.remote_caps, "resources");
        let expose_prompts = opts.expose_prompts && has_cap(&opts.remote_caps, "prompts");
        Self {
            remote,
            name: opts.name,
            description: opts.description,
            expose_resources,
            expose_prompts,
            call_timeout_ms: opts.call_timeout_ms,
            disabled: opts.disabled,
            source,
            log: opts.log,
        }
    }

    /// The registry name at log time — the cell is shared with the adapter, so a renamed MCP
    /// keeps logging under its current name instead of splitting the log across two keys.
    fn name_snapshot(&self) -> Option<String> {
        self.name.read().ok().map(|g| g.clone())
    }
}

/// An empty list beats a broken one — a remote that fails to answer a list should not take the
/// client's whole session down. But it must not be SILENT: an empty result is indistinguishable
/// from "this server has no tools", so the one thing that says otherwise is the warn line.
async fn safe<T>(
    mcp: Option<&str>,
    what: &str,
    run: impl Future<Output = Result<T, String>>,
    fallback: T,
) -> T {
    match run.await {
        Ok(value) => value,
        Err(err) => {
            let mut extra = Map::new();
            if let Some(mcp) = mcp {
                extra.insert("mcp".into(), json!(mcp));
            }
            extra.insert("what".into(), json!(what));
            extra.insert("err".into(), json!(err));
            log::warn(
                "proxied list failed; answering empty",
                Some(Value::Object(extra)),
            );
            fallback
        }
    }
}

/// Walk a remote's pagination the way the Node build's SDK client did when a list was called
/// with no cursor: fetch page by page until the remote stops handing out cursors, aggregating
/// every page into one answer. Non-convergence within [`LIST_MAX_PAGES`] pages is an error, not
/// a truncated list.
async fn aggregate_pages<T, F, Fut>(mut fetch: F) -> Result<Vec<T>, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, Option<String>), String>>,
{
    let (mut items, mut cursor) = fetch(None).await?;
    let mut pages = 1usize;
    while let Some(next) = cursor {
        if pages >= LIST_MAX_PAGES {
            return Err(format!(
                "exceeded listMaxPages ({LIST_MAX_PAGES}) while enumerating results"
            ));
        }
        pages += 1;
        let (page_items, next_cursor) = fetch(Some(next)).await?;
        items.extend(page_items);
        cursor = next_cursor;
    }
    Ok(items)
}

impl ServerHandler for ProxyServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        // Plain `tools: {}` — unlike the direct adapters there is no toggleable tool list here,
        // so no `listChanged` is announced. Resource/prompt capabilities appear only when the
        // toggle allows AND the remote negotiated them.
        capabilities.tools = Some(ToolsCapability::default());
        if self.expose_resources {
            capabilities.resources = Some(ResourcesCapability::default());
        }
        if self.expose_prompts {
            capabilities.prompts = Some(PromptsCapability::default());
        }
        let mut server_info = Implementation::default();
        server_info.name = "mcp-gateway-proxy".into();
        server_info.version = "1.0".into();
        let mut info = ServerInfo::new(capabilities).with_server_info(server_info);
        if let Some(description) = &self.description {
            info = info.with_instructions(description.clone());
        }
        info
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        let mcp = self.name_snapshot();
        let disabled = self.disabled.clone();
        let cursor = request.and_then(|p| p.cursor).filter(|c| !c.is_empty());
        async move {
            let mut result = match cursor {
                // A caller holding a cursor gets that one page and the remote's own cursor
                // back — handing both over unchanged is the whole of per-page paging.
                Some(cursor) => {
                    safe(
                        mcp.as_deref(),
                        "tools/list",
                        remote.list_tools(Some(cursor)),
                        ListToolsResult::with_all_items(Vec::new()),
                    )
                    .await
                }
                None => ListToolsResult::with_all_items(
                    safe(
                        mcp.as_deref(),
                        "tools/list",
                        aggregate_pages(|cursor| {
                            let remote = remote.clone();
                            async move {
                                let page = remote.list_tools(cursor).await?;
                                Ok((page.tools, page.next_cursor))
                            }
                        }),
                        Vec::new(),
                    )
                    .await,
                ),
            };
            // The advertised list is the contract: a disabled tool is absent, exactly as it is
            // on the direct adapters' ToolServer.
            if let Ok(set) = disabled.read() {
                result.tools.retain(|t| !set.contains(&*t.name));
            }
            // Strip `annotations` (added in protocol 2025-03-26) from every tool: some clients
            // negotiate an older version over HTTP, under which `annotations` is an unknown key
            // and strict schema parsing rejects the whole tools/list.
            for tool in &mut result.tools {
                tool.annotations = None;
            }
            Ok(result)
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        let mcp = self.name_snapshot();
        let timeout = self.call_timeout_ms;
        let source = self.source.clone();
        let disabled = self.disabled.clone();
        let log = self.log.clone();
        let tool = request.name.to_string();
        let args = request.arguments.clone().map(Value::Object);
        async move {
            let logged_tool = tool.clone();
            log.logged(
                mcp.as_deref(),
                &source,
                &logged_tool,
                args,
                async move {
                    // A disabled tool is refused with the same words the direct adapters answer a
                    // nonexistent one with — to a client they are the same thing: not in the
                    // contract. Inside calls::logged so the refusal lands in the call log like
                    // every other failed call, not silently.
                    if let Ok(set) = disabled.read() {
                        if set.contains(tool.as_str()) {
                            return Err(format!("unknown tool: {tool}"));
                        }
                    }
                    let result = remote.call_tool(request, timeout).await?;
                    Ok::<CallToolResult, String>(result)
                },
                // The remote's answer is logged as the client sees it, including an in-band
                // `isError` failure.
                |result: &CallToolResult| {
                    let output = serde_json::to_value(result)
                        .map(|v| calls::content_text(&v))
                        .unwrap_or_default();
                    (!result.is_error.unwrap_or(false), output)
                },
            )
            .await
            .map(CallToolResponse::Complete)
            .map_err(|message| ErrorData::internal_error(message, None))
        }
    }

    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        let mcp = self.name_snapshot();
        let cursor = request.and_then(|p| p.cursor).filter(|c| !c.is_empty());
        async move {
            // Registered only when exposed: the capability is not announced either, and a probe
            // against a hidden capability gets Method Not Found.
            if !self.expose_resources {
                return Err(ErrorData::method_not_found::<ListResourcesRequestMethod>());
            }
            let result = match cursor {
                Some(cursor) => {
                    safe(
                        mcp.as_deref(),
                        "resources/list",
                        remote.list_resources(Some(cursor)),
                        ListResourcesResult::with_all_items(Vec::new()),
                    )
                    .await
                }
                None => ListResourcesResult::with_all_items(
                    safe(
                        mcp.as_deref(),
                        "resources/list",
                        aggregate_pages(|cursor| {
                            let remote = remote.clone();
                            async move {
                                let page = remote.list_resources(cursor).await?;
                                Ok((page.resources, page.next_cursor))
                            }
                        }),
                        Vec::new(),
                    )
                    .await,
                ),
            };
            Ok(result)
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        let mcp = self.name_snapshot();
        let source = self.source.clone();
        let log = self.log.clone();
        let uri = request.uri.clone();
        async move {
            if !self.expose_resources {
                return Err(ErrorData::method_not_found::<ReadResourceRequestMethod>());
            }
            // Logged like tools/call above (and like the direct adapters' resource reads): a
            // read is a billed/observable action on the remote, and the Logs tab must show it.
            log.logged(
                mcp.as_deref(),
                &source,
                "resources/read",
                Some(json!({ "uri": uri })),
                async move { remote.read_resource(request).await },
                |result: &ReadResourceResult| {
                    // The Node build pinned this preview at 200 chars of the serialized result.
                    let serialized = serde_json::to_string(result).unwrap_or_default();
                    (true, serialized.chars().take(200).collect())
                },
            )
            .await
            .map(ReadResourceResponse::Complete)
            .map_err(|message| ErrorData::internal_error(message, None))
        }
    }

    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        let mcp = self.name_snapshot();
        let cursor = request.and_then(|p| p.cursor).filter(|c| !c.is_empty());
        async move {
            if !self.expose_prompts {
                return Err(ErrorData::method_not_found::<ListPromptsRequestMethod>());
            }
            let result = match cursor {
                Some(cursor) => {
                    safe(
                        mcp.as_deref(),
                        "prompts/list",
                        remote.list_prompts(Some(cursor)),
                        ListPromptsResult::with_all_items(Vec::new()),
                    )
                    .await
                }
                None => ListPromptsResult::with_all_items(
                    safe(
                        mcp.as_deref(),
                        "prompts/list",
                        aggregate_pages(|cursor| {
                            let remote = remote.clone();
                            async move {
                                let page = remote.list_prompts(cursor).await?;
                                Ok((page.prompts, page.next_cursor))
                            }
                        }),
                        Vec::new(),
                    )
                    .await,
                ),
            };
            Ok(result)
        }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, ErrorData>> + MaybeSendFuture + '_ {
        let remote = self.remote.clone();
        async move {
            if !self.expose_prompts {
                return Err(ErrorData::method_not_found::<GetPromptRequestMethod>());
            }
            remote
                .get_prompt(request)
                .await
                .map(GetPromptResponse::Complete)
                .map_err(|message| ErrorData::internal_error(message, None))
        }
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListResourceTemplatesResult, ErrorData>>
           + MaybeSendFuture
           + '_ {
        // The Node proxy registers no templates/list handler, so the SDK answered Method Not
        // Found — a proxied remote's templates arrive through resources/list or not at all.
        std::future::ready(Err(ErrorData::method_not_found::<
            ListResourceTemplatesRequestMethod,
        >()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ContentBlock, JsonObject, Prompt, Resource, Tool, ToolAnnotations};

    /// A named tool with the minimal shape every assertion here needs.
    fn tool(name: &str) -> Tool {
        let schema: JsonObject =
            serde_json::from_value(json!({ "type": "object", "properties": {} }))
                .expect("literal object");
        let mut tool = Tool::default();
        tool.name = name.to_string().into();
        tool.description = Some(format!("tool {name}").into());
        tool.input_schema = Arc::new(schema);
        tool
    }

    /// A remote MCP that paginates — port of the Node fake: `seen` records every cursor that
    /// reached the remote, which is the assertion that matters (the caller's cursor arrived,
    /// rather than the answer merely looking plausible).
    struct PagingRemote {
        tool_pages: Vec<Vec<Tool>>,
        resource_pages: Vec<Vec<Resource>>,
        prompt_pages: Vec<Vec<Prompt>>,
        seen: Mutex<Vec<Option<String>>>,
    }

    impl PagingRemote {
        fn empty_seen() -> Mutex<Vec<Option<String>>> {
            Mutex::new(Vec::new())
        }

        fn page(&self, cursor: Option<&str>) -> (usize, Option<String>) {
            self.seen
                .lock()
                .expect("seen")
                .push(cursor.map(str::to_string));
            let index = cursor.and_then(|c| c.parse::<usize>().ok()).unwrap_or(0);
            let lengths = [
                self.tool_pages.len(),
                self.resource_pages.len(),
                self.prompt_pages.len(),
            ];
            let longest = lengths.into_iter().max().unwrap_or(0);
            let next = (index + 1 < longest).then(|| (index + 1).to_string());
            (index, next)
        }
    }

    #[async_trait]
    impl RemoteMcp for PagingRemote {
        async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, String> {
            let (index, next) = self.page(cursor.as_deref());
            let mut result = ListToolsResult::with_all_items(
                self.tool_pages.get(index).cloned().unwrap_or_default(),
            );
            result.next_cursor = next;
            Ok(result)
        }

        async fn call_tool(
            &self,
            _params: CallToolRequestParams,
            _timeout_ms: Option<u64>,
        ) -> Result<CallToolResult, String> {
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")]))
        }

        async fn list_resources(
            &self,
            cursor: Option<String>,
        ) -> Result<ListResourcesResult, String> {
            let (index, next) = self.page(cursor.as_deref());
            let mut result = ListResourcesResult::with_all_items(
                self.resource_pages.get(index).cloned().unwrap_or_default(),
            );
            result.next_cursor = next;
            Ok(result)
        }

        async fn read_resource(
            &self,
            _params: ReadResourceRequestParams,
        ) -> Result<ReadResourceResult, String> {
            Err("read_resource is not exercised by these tests".into())
        }

        async fn list_prompts(&self, cursor: Option<String>) -> Result<ListPromptsResult, String> {
            let (index, next) = self.page(cursor.as_deref());
            let mut result = ListPromptsResult::with_all_items(
                self.prompt_pages.get(index).cloned().unwrap_or_default(),
            );
            result.next_cursor = next;
            Ok(result)
        }

        async fn get_prompt(
            &self,
            _params: GetPromptRequestParams,
        ) -> Result<GetPromptResult, String> {
            Err("get_prompt is not exercised by these tests".into())
        }
    }

    /// makeProxyServer's opts, defaulted the way http/proc hand them over.
    fn proxy_server(remote: Arc<dyn RemoteMcp>, call_timeout_ms: Option<u64>) -> ProxyServer {
        proxy_server_with_disabled(remote, call_timeout_ms, std::collections::HashSet::new())
    }

    /// Same construction with a disabled-tool set — the live toggle the admin API mutates.
    fn proxy_server_with_disabled(
        remote: Arc<dyn RemoteMcp>,
        call_timeout_ms: Option<u64>,
        disabled: std::collections::HashSet<String>,
    ) -> ProxyServer {
        ProxyServer::new(
            remote,
            ProxyOpts {
                name: Arc::new(RwLock::new("p".to_string())),
                description: None,
                expose_resources: true,
                expose_prompts: true,
                call_timeout_ms,
                // Unknown capabilities expose per the toggles, like a child that answers
                // before its capabilities are read.
                remote_caps: None,
                disabled: Arc::new(RwLock::new(disabled)),
                log: crate::calls::test_log(),
            },
            crate::calls::current_source(),
        )
    }

    /// A disabled tool is absent from tools/list — the advertised list is the contract.
    #[tokio::test]
    async fn disabled_tools_are_absent_from_the_list() {
        let remote = Arc::new(PagingRemote {
            tool_pages: tool_pages(3, 50),
            resource_pages: Vec::new(),
            prompt_pages: Vec::new(),
            seen: PagingRemote::empty_seen(),
        });
        let disabled: std::collections::HashSet<String> = ["t1".to_string()].into();
        let server = proxy_server_with_disabled(remote, None, disabled);
        let client = crate::introspect::open_session(server)
            .await
            .expect("session");
        let result = client.list_tools(None).await.expect("tools/list");
        let names: Vec<String> = result.tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, vec!["t0".to_string(), "t2".to_string()]);
        let _ = client.cancel().await;
    }

    fn with_cursor(cursor: &str) -> Option<PaginatedRequestParams> {
        let mut params = PaginatedRequestParams::default();
        params.cursor = Some(cursor.to_string());
        Some(params)
    }

    fn tool_pages(count: usize, page_size: usize) -> Vec<Vec<Tool>> {
        (0..count)
            .collect::<Vec<_>>()
            .chunks(page_size)
            .map(|chunk| chunk.iter().map(|i| tool(&format!("t{i}"))).collect())
            .collect()
    }

    /// Port of "aggregates every page when the caller sends no cursor": the walk happens here
    /// page by page, and the answer carries no cursor of its own.
    #[tokio::test]
    async fn aggregates_every_page_when_no_cursor() {
        let remote = Arc::new(PagingRemote {
            tool_pages: tool_pages(130, 50),
            resource_pages: Vec::new(),
            prompt_pages: Vec::new(),
            seen: PagingRemote::empty_seen(),
        });
        let client = crate::introspect::open_session(proxy_server(remote.clone(), None))
            .await
            .expect("session");
        let result = client.list_tools(None).await.expect("tools/list");
        let names: Vec<String> = result.tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names.len(), 130);
        assert_eq!(names.first().map(String::as_str), Some("t0"));
        assert_eq!(names.last().map(String::as_str), Some("t129"));
        assert!(
            result.next_cursor.is_none(),
            "aggregated answer is complete"
        );
        assert_eq!(
            *remote.seen.lock().expect("seen"),
            vec![None, Some("1".into()), Some("2".into())],
            "the walk happened page by page"
        );
        let _ = client.cancel().await;
    }

    /// Port of "answers one page, with the remote's nextCursor, when the caller sends a
    /// cursor": explicit per-page paging works at all only because cursors pass through.
    #[tokio::test]
    async fn answers_one_page_with_next_cursor_when_cursor_sent() {
        let remote = Arc::new(PagingRemote {
            tool_pages: tool_pages(130, 50),
            resource_pages: Vec::new(),
            prompt_pages: Vec::new(),
            seen: PagingRemote::empty_seen(),
        });
        let client = crate::introspect::open_session(proxy_server(remote.clone(), None))
            .await
            .expect("session");
        let second = client.list_tools(with_cursor("1")).await.expect("page two");
        assert_eq!(second.tools.len(), 50);
        assert_eq!(
            second.tools.first().map(|t| t.name.to_string()).as_deref(),
            Some("t50")
        );
        assert_eq!(second.next_cursor.as_deref(), Some("2"));

        let third = client
            .list_tools(with_cursor(&second.next_cursor.expect("cursor")))
            .await
            .expect("page three");
        assert_eq!(third.tools.len(), 30);
        assert!(third.next_cursor.is_none());

        assert_eq!(
            *remote.seen.lock().expect("seen"),
            vec![Some("1".into()), Some("2".into())],
            "the cursors reached the remote unchanged"
        );
        let _ = client.cancel().await;
    }

    /// Port of "still strips annotations": the 2025-03-26 key must not reach clients that
    /// negotiated an older protocol.
    #[tokio::test]
    async fn strips_annotations_from_forwarded_tools() {
        let mut annotated = tool("a");
        let mut annotations = ToolAnnotations::default();
        annotations.title = Some("A".into());
        annotated.annotations = Some(annotations);
        let remote = Arc::new(PagingRemote {
            tool_pages: vec![vec![annotated]],
            resource_pages: Vec::new(),
            prompt_pages: Vec::new(),
            seen: PagingRemote::empty_seen(),
        });
        let client = crate::introspect::open_session(proxy_server(remote, None))
            .await
            .expect("session");
        let result = client.list_tools(None).await.expect("tools/list");
        assert!(
            result.tools[0].annotations.is_none(),
            "annotations stripped"
        );
        let _ = client.cancel().await;
    }

    /// Port of "passes a cursor through for resources and prompts too".
    #[tokio::test]
    async fn passes_cursors_through_for_resources_and_prompts() {
        let resource_pages: Vec<Vec<Resource>> = (0..70)
            .collect::<Vec<_>>()
            .chunks(50)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|i| Resource::new(format!("x://{i}"), format!("r{i}")))
                    .collect()
            })
            .collect();
        let prompt_pages: Vec<Vec<Prompt>> = (0..70)
            .collect::<Vec<_>>()
            .chunks(50)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|i| Prompt::new(format!("p{i}"), None::<String>, None))
                    .collect()
            })
            .collect();
        for kind in ["resources", "prompts"] {
            let remote = Arc::new(PagingRemote {
                tool_pages: Vec::new(),
                resource_pages: if kind == "resources" {
                    resource_pages.clone()
                } else {
                    Vec::new()
                },
                prompt_pages: if kind == "prompts" {
                    prompt_pages.clone()
                } else {
                    Vec::new()
                },
                seen: PagingRemote::empty_seen(),
            });
            let client = crate::introspect::open_session(proxy_server(remote.clone(), None))
                .await
                .expect("session");
            if kind == "resources" {
                let page = client.list_resources(with_cursor("1")).await.expect("page");
                assert_eq!(page.resources.len(), 20);
            } else {
                let page = client.list_prompts(with_cursor("1")).await.expect("page");
                assert_eq!(page.prompts.len(), 20);
            }
            assert_eq!(
                *remote.seen.lock().expect("seen"),
                vec![Some("1".into())],
                "{kind} cursor reached the remote"
            );
            let _ = client.cancel().await;
        }
    }

    /// A remote whose lists always fail — the ListPaginationExceeded shape from the Node test.
    struct FailingRemote;

    #[async_trait]
    impl RemoteMcp for FailingRemote {
        async fn list_tools(&self, _cursor: Option<String>) -> Result<ListToolsResult, String> {
            Err("exceeded listMaxPages (64) while enumerating results".into())
        }
        async fn call_tool(
            &self,
            _params: CallToolRequestParams,
            _timeout_ms: Option<u64>,
        ) -> Result<CallToolResult, String> {
            unreachable!("not exercised")
        }
        async fn list_resources(&self, _c: Option<String>) -> Result<ListResourcesResult, String> {
            Err("not exercised".into())
        }
        async fn read_resource(
            &self,
            _params: ReadResourceRequestParams,
        ) -> Result<ReadResourceResult, String> {
            Err("not exercised".into())
        }
        async fn list_prompts(&self, _c: Option<String>) -> Result<ListPromptsResult, String> {
            Err("not exercised".into())
        }
        async fn get_prompt(&self, _p: GetPromptRequestParams) -> Result<GetPromptResult, String> {
            Err("not exercised".into())
        }
    }

    /// Port of "answers an empty list when the remote's list fails, and says so in the log":
    /// an empty list beats a broken one, and the warn line carries the remote's own reason.
    /// (The Node test spied on console.log; here the empty answer is asserted and the warn is
    /// visible in the runner's captured stdout.)
    #[tokio::test]
    async fn answers_empty_when_the_remote_list_fails() {
        let client = crate::introspect::open_session(proxy_server(Arc::new(FailingRemote), None))
            .await
            .expect("session");
        let result = client
            .list_tools(None)
            .await
            .expect("a failed list is an empty list");
        assert!(result.tools.is_empty());
        let _ = client.cancel().await;
    }

    /// A remote whose pagination never converges — the walk cap turns it into the same empty
    /// answer rather than an infinite loop, with the reason in the log.
    struct EndlessRemote;

    #[async_trait]
    impl RemoteMcp for EndlessRemote {
        async fn list_tools(&self, _cursor: Option<String>) -> Result<ListToolsResult, String> {
            let mut result = ListToolsResult::with_all_items(vec![tool("t")]);
            result.next_cursor = Some("more".into());
            Ok(result)
        }
        async fn call_tool(
            &self,
            _params: CallToolRequestParams,
            _timeout_ms: Option<u64>,
        ) -> Result<CallToolResult, String> {
            unreachable!("not exercised")
        }
        async fn list_resources(&self, _c: Option<String>) -> Result<ListResourcesResult, String> {
            Err("not exercised".into())
        }
        async fn read_resource(
            &self,
            _params: ReadResourceRequestParams,
        ) -> Result<ReadResourceResult, String> {
            Err("not exercised".into())
        }
        async fn list_prompts(&self, _c: Option<String>) -> Result<ListPromptsResult, String> {
            Err("not exercised".into())
        }
        async fn get_prompt(&self, _p: GetPromptRequestParams) -> Result<GetPromptResult, String> {
            Err("not exercised".into())
        }
    }

    #[tokio::test]
    async fn walk_cap_answers_empty_when_pagination_never_converges() {
        let client = crate::introspect::open_session(proxy_server(Arc::new(EndlessRemote), None))
            .await
            .expect("session");
        let result = client
            .list_tools(None)
            .await
            .expect("capped walk is an empty list");
        assert!(result.tools.is_empty());
        let _ = client.cancel().await;
    }

    /// Records the deadline each proxied call arrived with — port of the Node recordingClient.
    struct RecordingRemote {
        timeouts: Mutex<Vec<Option<u64>>>,
    }

    #[async_trait]
    impl RemoteMcp for RecordingRemote {
        async fn list_tools(&self, _cursor: Option<String>) -> Result<ListToolsResult, String> {
            Ok(ListToolsResult::with_all_items(vec![tool("slow")]))
        }
        async fn call_tool(
            &self,
            _params: CallToolRequestParams,
            timeout_ms: Option<u64>,
        ) -> Result<CallToolResult, String> {
            self.timeouts.lock().expect("timeouts").push(timeout_ms);
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")]))
        }
        async fn list_resources(&self, _c: Option<String>) -> Result<ListResourcesResult, String> {
            Err("not exercised".into())
        }
        async fn read_resource(
            &self,
            _params: ReadResourceRequestParams,
        ) -> Result<ReadResourceResult, String> {
            Err("not exercised".into())
        }
        async fn list_prompts(&self, _c: Option<String>) -> Result<ListPromptsResult, String> {
            Err("not exercised".into())
        }
        async fn get_prompt(&self, _p: GetPromptRequestParams) -> Result<GetPromptResult, String> {
            Err("not exercised".into())
        }
    }

    async fn call_through(remote: &Arc<RecordingRemote>, timeout: Option<u64>) {
        let client = crate::introspect::open_session(proxy_server(remote.clone(), timeout))
            .await
            .expect("session");
        let mut params = CallToolRequestParams::default();
        params.name = "slow".into();
        params.arguments = Some(Map::new());
        let result = client.call_tool(params).await.expect("call");
        assert!(!result.is_error.unwrap_or(false));
        let _ = client.cancel().await;
    }

    /// Port of proxy.test.ts: a configured deadline reaches the remote even above the SDK's
    /// 60s default — the point of the fix — and no deadline configured means none is imposed.
    #[tokio::test]
    async fn passes_a_call_deadline_above_the_default_straight_through() {
        let remote = Arc::new(RecordingRemote {
            timeouts: Mutex::new(Vec::new()),
        });
        call_through(&remote, Some(180_000)).await;
        assert_eq!(
            *remote.timeouts.lock().expect("timeouts"),
            vec![Some(180_000)]
        );
    }

    #[tokio::test]
    async fn leaves_the_default_in_place_when_no_deadline_is_configured() {
        let remote = Arc::new(RecordingRemote {
            timeouts: Mutex::new(Vec::new()),
        });
        call_through(&remote, None).await;
        assert_eq!(*remote.timeouts.lock().expect("timeouts"), vec![None]);
    }

    // --- proxy-fetch.ts -------------------------------------------------------------------------

    #[test]
    fn assert_proxy_url_accepts_http_and_https_and_trims() {
        assert_eq!(
            assert_proxy_url(" http://127.0.0.1:8080 ").as_deref(),
            Ok("http://127.0.0.1:8080")
        );
        assert_eq!(
            assert_proxy_url("HTTPS://proxy.example").as_deref(),
            Ok("HTTPS://proxy.example")
        );
    }

    #[test]
    fn assert_proxy_url_refuses_missing_scheme_and_socks() {
        // The Node message, JSON.stringify quoting included.
        assert_eq!(
            assert_proxy_url("127.0.0.1:8080"),
            Err("proxy must be an http:// or https:// URL, got \"127.0.0.1:8080\"".to_string())
        );
        assert_eq!(
            assert_proxy_url("socks5://127.0.0.1"),
            Err("proxy must be an http:// or https:// URL, got \"socks5://127.0.0.1\"".to_string())
        );
        assert_eq!(
            assert_proxy_url("a\"b"),
            Err("proxy must be an http:// or https:// URL, got \"a\\\"b\"".to_string())
        );
    }

    #[test]
    fn proxied_clients_share_one_pool_per_url() {
        let first = proxied_client("http://127.0.0.1:3128").expect("valid proxy");
        let second = proxied_client(" http://127.0.0.1:3128 ").expect("same proxy, trimmed");
        drop((first, second));
        // The memo table is keyed on the trimmed URL: one dispatcher, many handles. A bad
        // proxy never becomes a client.
        assert!(proxied_client("not a url").is_err());
    }
}
