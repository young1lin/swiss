//! The remote-MCP-over-HTTP adapter — port of `adapters/http.ts`.
//!
//! Hosts a REMOTE MCP server reached over streamable HTTP, by connecting to it once and
//! proxying every request to it: the third kind of MCP this gateway serves, after the
//! in-process DB adapters and the stdio children of `proc` — and the one that needs the least:
//! no process to spawn, nothing to orphan, no driver to import.
//!
//! Only streamable HTTP. Some providers still publish an SSE endpoint beside the streamable
//! one, but it is legacy everywhere it appears and untestable here — this gateway serves no
//! GET SSE stream of its own, so there is nothing local to test a client against.
//!
//! The Node build borrowed the TS SDK's `StreamableHTTPClientTransport`; the Rust build's rmcp
//! does not ship one in the lib's feature set, so the half of that transport this proxy needs
//! is hand-rolled over reqwest below: initialize, notifications, and one JSON-RPC POST per
//! request, answered by `application/json` or a single-shot SSE stream. What is deliberately
//! NOT ported is the SDK's listening GET stream — the proxy only forwards client-initiated
//! requests (see `proxy.rs`), so server-initiated messages have no reader to reach.

use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult,
    ListPromptsResult, ListResourcesResult, ListToolsResult, ReadResourceRequestParams,
    ReadResourceResult,
};
use serde_json::{json, Value};

use lmg_host::config::ServerDef;

use super::direct::{def_bool, BoxFut, Lazy};
use super::proxy::{assert_proxy_url, proxied_client, ProxyOpts, ProxyServer, RemoteMcp};
use super::{rmcp_endpoint, Adapter, McpEndpoint};

/// The protocol revision this client offers at initialize, exactly what the Node build's SDK
/// sent as its latest. Any server worth proxying accepts it or negotiates down.
const OFFERED_PROTOCOL_VERSION: &str = "2025-06-18";

/// Per-request deadline when the caller did not name one — the TS SDK's own
/// `DEFAULT_REQUEST_TIMEOUT`, inherited by every proxied call in the Node build.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;

/// Header names validated once at construction, so a malformed one is a start error rather
/// than a first-call mystery.
#[derive(Clone)]
struct RemoteHeaders(Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>);

impl RemoteHeaders {
    fn from_def(def: &ServerDef) -> Result<Self, String> {
        let mut out = Vec::new();
        if let Some(Value::Object(headers)) = def.get("headers") {
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|err| format!("invalid header name {name:?}: {err}"))?;
                let text = value_as_string(value);
                let value = reqwest::header::HeaderValue::from_str(&text)
                    .map_err(|err| format!("invalid header value for {name:?}: {err}"))?;
                out.push((name, value));
            }
        }
        Ok(RemoteHeaders(out))
    }
}

/// `String(v)` for a def-sourced header value: JSON strings pass through, numbers and booleans
/// render as JSON does (a def like `{"X-Count": 5}` sends `5`, not `"5"`).
fn value_as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The connected remote: one reqwest client, the negotiated protocol version and session id,
/// and the request-id counter that multiplexes concurrent POSTs.
pub struct RemoteMcpClient {
    client: reqwest::Client,
    url: String,
    headers: RemoteHeaders,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<String>,
    next_id: AtomicI64,
}

impl RemoteMcpClient {
    /// Connect: build the client (proxied when the def names a proxy), run the initialize
    /// handshake — this IS the reachability check, which is why it happens eagerly in build()
    /// and not lazily on the first call — and capture what the remote negotiated.
    async fn connect(
        url: &str,
        headers: RemoteHeaders,
        proxy: Option<&str>,
    ) -> Result<(Arc<RemoteMcpClient>, Value), String> {
        let client = match proxy {
            Some(proxy) => proxied_client(proxy)?,
            None => reqwest::Client::builder()
                .build()
                .map_err(|err| format!("http client build failed: {err}"))?,
        };
        let session = Arc::new(RemoteMcpClient {
            client,
            url: url.to_string(),
            headers,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(OFFERED_PROTOCOL_VERSION.to_string()),
            next_id: AtomicI64::new(1),
        });
        let result = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": OFFERED_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "mcp-gateway", "version": "1.0" },
                }),
                None,
            )
            .await?;
        if let Some(negotiated) = result.get("protocolVersion").and_then(Value::as_str) {
            if !negotiated.is_empty() {
                if let Ok(mut version) = session.protocol_version.lock() {
                    *version = negotiated.to_string();
                }
            }
        }
        // The spec-mandated follow-up. A server that refuses it is not speaking streamable
        // HTTP, and refusing to connect here is the same start error the Node build produced.
        session.notify("notifications/initialized").await?;
        let caps = result.get("capabilities").cloned().unwrap_or(json!({}));
        Ok((session, caps))
    }

    fn session_header(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|g| g.clone())
    }

    fn apply_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // Order mirrors the Node transport: defaults first, then the def's headers (a def may
        // override Accept or Content-Type), then the protocol-critical headers last so a bad
        // def value can never break session routing or version negotiation.
        let mut builder = builder
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        for (name, value) in &self.headers.0 {
            builder = builder.header(name, value);
        }
        if let Some(session) = self.session_header() {
            builder = builder.header("mcp-session-id", session);
        }
        if let Ok(version) = self.protocol_version.lock() {
            builder = builder.header("mcp-protocol-version", version.as_str());
        }
        builder
    }

    /// One JSON-RPC request over one POST. The answer may come back as plain JSON or as a
    /// single-shot SSE stream; both are parsed, and the response carrying our id is the result.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let builder = self
            .apply_headers(self.client.request(reqwest::Method::POST, &self.url))
            .timeout(Duration::from_millis(
                timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
            ))
            .body(serde_json::to_string(&body).unwrap_or_default());
        let response = builder
            .send()
            .await
            .map_err(|err| format!("Error POSTing to endpoint ({}): {err}", self.url))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Error POSTing to endpoint (HTTP {status}): {}",
                preview(&text, 600)
            ));
        }
        // A session is issued on initialize (and may rotate); any answer may carry the header.
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
        {
            if let Ok(mut cell) = self.session_id.lock() {
                *cell = Some(session.to_string());
            }
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let text = response
            .text()
            .await
            .map_err(|err| format!("reading {method} response: {err}"))?;
        let message = if content_type.contains("text/event-stream") {
            sse_message(&text, id)?
        } else {
            serde_json::from_str::<Value>(&text)
                .map_err(|err| format!("{method} response is not JSON: {err}"))?
        };
        match message.get("error") {
            Some(Value::Object(_)) => {
                let message_text = message
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown remote error");
                Err(message_text.to_string())
            }
            _ => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    /// One JSON-RPC notification over one POST — no id, no body expected back.
    async fn notify(&self, method: &str) -> Result<(), String> {
        let body = json!({ "jsonrpc": "2.0", "method": method });
        let builder = self
            .apply_headers(self.client.request(reqwest::Method::POST, &self.url))
            .timeout(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS))
            .body(serde_json::to_string(&body).unwrap_or_default());
        let response = builder
            .send()
            .await
            .map_err(|err| format!("Error POSTing to endpoint ({}): {err}", self.url))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "Error POSTing {method} to endpoint (HTTP {status}): {}",
                preview(&response.text().await.unwrap_or_default(), 600)
            ));
        }
        Ok(())
    }

    /// Terminate the remote session, if any — the spec's DELETE. Best effort by contract: a
    /// remote that ignores it is left to time its own session out.
    async fn delete_session(&self) {
        let Some(session) = self.session_header() else {
            return;
        };
        let mut builder = self
            .client
            .request(reqwest::Method::DELETE, &self.url)
            .header("accept", "application/json")
            .header("mcp-session-id", session)
            .timeout(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS));
        if let Ok(version) = self.protocol_version.lock() {
            builder = builder.header("mcp-protocol-version", version.as_str());
        }
        let _ = builder.send().await;
    }
}

/// The first `max` chars of a response body, for error messages — enough to carry the remote's
/// own error code, short enough not to become the error.
fn preview(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Parse one SSE payload and pick out the JSON-RPC response carrying `id`. Everything else in
/// the stream — notifications, other ids — is not ours to read.
fn sse_message(text: &str, id: i64) -> Result<Value, String> {
    let mut data: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let flush = |data: &mut Vec<String>, out: &mut Vec<Value>| {
        if data.is_empty() {
            return;
        }
        // Per SSE, multiple data: lines join with a newline; a JSON payload never splits that
        // way, but honoring it costs nothing and keeps the parser honest.
        let joined = data.join("\n");
        data.clear();
        if let Ok(value) = serde_json::from_str::<Value>(&joined) {
            out.push(value);
        }
    };
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            flush(&mut data, &mut messages);
            continue;
        }
        if let Some(payload) = line.strip_prefix("data:") {
            data.push(payload.strip_prefix(' ').unwrap_or(payload).to_string());
        }
        // "event:", "id:", "retry:" and ":" comments carry nothing this reader needs.
    }
    flush(&mut data, &mut messages);
    messages
        .into_iter()
        .find(|message| message.get("id") == Some(&json!(id)))
        .ok_or_else(|| format!("SSE stream carried no response for request {id}"))
}

impl RemoteMcpClient {
    fn list_params(cursor: Option<String>) -> Value {
        match cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        }
    }
}

#[async_trait]
impl RemoteMcp for RemoteMcpClient {
    async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, String> {
        let result = self
            .request("tools/list", Self::list_params(cursor), None)
            .await?;
        serde_json::from_value(result).map_err(|err| format!("tools/list response: {err}"))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        timeout_ms: Option<u64>,
    ) -> Result<CallToolResult, String> {
        let params =
            serde_json::to_value(&params).map_err(|err| format!("tools/call params: {err}"))?;
        let result = self.request("tools/call", params, timeout_ms).await?;
        serde_json::from_value(result).map_err(|err| format!("tools/call response: {err}"))
    }

    async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, String> {
        let result = self
            .request("resources/list", Self::list_params(cursor), None)
            .await?;
        serde_json::from_value(result).map_err(|err| format!("resources/list response: {err}"))
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, String> {
        let params =
            serde_json::to_value(&params).map_err(|err| format!("resources/read params: {err}"))?;
        let result = self.request("resources/read", params, None).await?;
        serde_json::from_value(result).map_err(|err| format!("resources/read response: {err}"))
    }

    async fn list_prompts(&self, cursor: Option<String>) -> Result<ListPromptsResult, String> {
        let result = self
            .request("prompts/list", Self::list_params(cursor), None)
            .await?;
        serde_json::from_value(result).map_err(|err| format!("prompts/list response: {err}"))
    }

    async fn get_prompt(&self, params: GetPromptRequestParams) -> Result<GetPromptResult, String> {
        let params =
            serde_json::to_value(&params).map_err(|err| format!("prompts/get params: {err}"))?;
        let result = self.request("prompts/get", params, None).await?;
        serde_json::from_value(result).map_err(|err| format!("prompts/get response: {err}"))
    }
}

/// The connected state one adapter shares across every per-request server.
struct RemoteSession {
    client: Arc<RemoteMcpClient>,
    /// What the remote negotiated — read once at connect, so the proxy announces its
    /// capabilities and not a fixed guess.
    caps: Value,
}

/// Hosts a REMOTE MCP server reached over streamable HTTP — port of `HttpAdapter`.
pub struct HttpAdapter {
    description: Option<String>,
    expose_resources: bool,
    /// Disabled tool names, shared live with every per-request ProxyServer (see
    /// ProxyOpts::disabled — the deliberate divergence note lives there).
    disabled: super::ToolToggle,
    expose_prompts: bool,
    /// The MCP's registry name, followed across renames — the call log is filed under the
    /// engine's view of this cell, read fresh at call time.
    name: Arc<RwLock<String>>,
    /// One connection, opened once in build() and shared by every per-request server.
    conn: Arc<Lazy<RemoteSession>>,
    /// Where calls through this adapter are recorded.
    log: std::sync::Arc<crate::calls::CallLog>,
}

impl HttpAdapter {
    /// `def` must be the resolve_def() clone make_adapter hands over. A bad proxy URL or a
    /// malformed header name is a start error, not a first-call mystery.
    pub fn new(
        def: &ServerDef,
        name: &str,
        log: std::sync::Arc<crate::calls::CallLog>,
    ) -> Result<Self, String> {
        let url = def
            .get_str("url")
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "an http MCP needs a url".to_string())?
            .to_string();
        // A truthy (non-empty) proxy is validated; empty stays "no proxy" exactly as Node's
        // `opts.proxy ?` check treats "" as absent.
        let proxy = def
            .get_str("proxy")
            .filter(|p| !p.is_empty())
            .map(assert_proxy_url)
            .transpose()?;
        let headers = RemoteHeaders::from_def(def)?;
        let description = def.get_str("description").map(str::to_string);
        // `!== false` in the Node build; def_bool also folds the panel's "false" strings in.
        let expose_resources = !def_bool(def, "exposeResources");
        let expose_prompts = !def_bool(def, "exposePrompts");
        let conn = Arc::new(Lazy::new(move || {
            let url = url.clone();
            let headers = headers.clone();
            let proxy = proxy.clone();
            Box::pin(async move {
                let (client, caps) =
                    RemoteMcpClient::connect(&url, headers, proxy.as_deref()).await?;
                Ok(RemoteSession { client, caps })
            }) as BoxFut<Result<RemoteSession, String>>
        }));
        // Persisted toggles ride the def (register_one/store seed `disabledTools`); seeded the
        // same way DirectAdapter does it, so a toggle survives a restart.
        let disabled: std::collections::HashSet<String> = def
            .get("disabledTools")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            description,
            expose_resources,
            expose_prompts,
            disabled: Arc::new(std::sync::RwLock::new(disabled)),
            name: Arc::new(RwLock::new(name.to_string())),
            conn,
            log,
        })
    }

    /// Test-only: the live session's negotiated capabilities, so a test that moved out of
    /// this crate (tests/http_adapter.rs) can still assert the handshake stored exactly what
    /// the remote announced. Built under `test-utils`, which only dev-dependencies enable.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn session_caps(&self) -> Option<Value> {
        self.conn.get().await.ok().map(|s| s.caps.clone())
    }

    /// The per-request server factory — the one construction site for the proxy opts. Returned
    /// as an owned closure (capturing clones, not `&self`) because `rmcp_endpoint` keeps it for
    /// the endpoint's lifetime; the shared `RemoteSession` rides inside.
    fn endpoint_factory(
        &self,
        session: Arc<RemoteSession>,
    ) -> impl Fn(crate::calls::CallSource) -> ProxyServer + Send + Sync + 'static {
        let name = self.name.clone();
        let description = self.description.clone();
        let expose_resources = self.expose_resources;
        let expose_prompts = self.expose_prompts;
        let disabled = self.disabled.clone();
        let log = self.log.clone();
        move |source| {
            ProxyServer::new(
                session.client.clone(),
                ProxyOpts {
                    name: name.clone(),
                    description: description.clone(),
                    expose_resources,
                    expose_prompts,
                    call_timeout_ms: None,
                    remote_caps: Some(session.caps.clone()),
                    disabled: disabled.clone(),
                    log: log.clone(),
                },
                source,
            )
        }
    }
}

#[async_trait]
impl Adapter for HttpAdapter {
    fn kind(&self) -> &str {
        "http"
    }

    fn tool_toggle(&self) -> Option<super::ToolToggle> {
        Some(self.disabled.clone())
    }

    async fn build(&self) -> Result<McpEndpoint, String> {
        // Connect eagerly: the initialize handshake is the reachability check. A metered remote
        // is contacted exactly once here and never probed again (no ping — see below).
        let session = self.conn.get().await?;
        Ok(rmcp_endpoint(self.endpoint_factory(session)))
    }

    // Deliberately NO ping. The registry reports an adapter without one as "unknown" — the
    // honest reading for a metered third-party endpoint, and exactly what AGENTS.md requires of
    // http/rest: probing every 15s would be thousands of unpaid requests a day. Reachability
    // was proven once by the initialize handshake in build(); a real failure surfaces on a real
    // call, where the traffic log records it. (An always-succeeding ping() used to live in the
    // Node build and lit the dot green with a fake ~0ms latency — rest never had one, so the
    // two drifted.)

    async fn close(&self) {
        // Mirror of Node's client.close(): drop the shared connection and terminate the remote
        // session. Taking the cell first means a request queued behind this close re-connects
        // only AFTER the DELETE completes.
        self.conn
            .dispose(move |session| {
                let client = session.client.clone();
                Box::pin(async move {
                    client.delete_session().await;
                })
            })
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
    //! Ported from the Node build's `test/http-adapter.test.ts`, plus the hand-rolled SSE and
    //! header parsing this port owns.
    //!
    //! Like the Node suite, the remote MCP under proxy is the gateway itself serving `echo` on an
    //! ephemeral loopback port. Two reasons: it needs no network, and its endpoint is
    //! bearer-gated — so a proxy that fails to send the configured headers cannot connect at all,
    //! which is what makes the "headers reach the remote" assertion honest rather than incidental.
    use super::*;

    // TOKEN moved with the live-gateway tests to tests/http_adapter.rs.

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    // ---- header and body helpers -------------------------------------------------------

    #[test]
    fn a_def_header_value_renders_the_way_json_does() {
        assert_eq!(value_as_string(&json!("Bearer x")), "Bearer x");
        assert_eq!(value_as_string(&json!(5)), "5");
        assert_eq!(value_as_string(&json!(true)), "true");
        assert_eq!(value_as_string(&json!(null)), "");
    }

    #[test]
    fn remote_headers_are_validated_once_at_construction() {
        let headers = RemoteHeaders::from_def(&def(
            json!({ "headers": { "Authorization": "Bearer x", "X-Count": 5 } }),
        ))
        .expect("valid headers");
        let pairs: Vec<(String, String)> = headers
            .0
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        assert!(pairs.contains(&("authorization".into(), "Bearer x".into())));
        assert!(pairs.contains(&("x-count".into(), "5".into())));

        // A malformed one is a start error, not a first-call mystery. `RemoteHeaders` is not
        // `Debug`, so the Ok arm is named rather than unwrapped.
        let refused = |d: Value| match RemoteHeaders::from_def(&def(d)) {
            Err(err) => err,
            Ok(_) => panic!("this header set must not be accepted"),
        };
        assert!(refused(json!({ "headers": { "bad name": "v" } })).contains("invalid header name"));
        assert!(refused(json!({ "headers": { "X-Bad": "line\nbreak" } }))
            .contains("invalid header value"));
        // No headers at all is not an error.
        assert!(RemoteHeaders::from_def(&def(json!({})))
            .expect("no headers")
            .0
            .is_empty());
    }

    #[test]
    fn an_error_preview_counts_characters_not_bytes() {
        assert_eq!(preview("abcdef", 3), "abc");
        assert_eq!(preview("ab", 10), "ab");
        // Truncating by byte would cut a multi-byte character in half.
        assert_eq!(preview("héllo", 3), "hél");
    }

    // ---- the hand-rolled SSE reader ----------------------------------------------------

    #[test]
    fn reads_the_response_carrying_our_id_out_of_an_sse_stream() {
        let stream =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(
            sse_message(stream, 7).unwrap(),
            json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } })
        );
    }

    #[test]
    fn skips_notifications_and_other_ids_in_the_same_stream() {
        // Everything else in the stream is not ours to read: a proxied call multiplexes ids over
        // one connection, and a notification has no id at all.
        let stream = concat!(
            ": a comment\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n",
            "\n",
            "id: 42\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"not ours\"}\r\n",
            "\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":\"ours\"}\n",
            "\n",
        );
        assert_eq!(sse_message(stream, 2).unwrap()["result"], json!("ours"));
        assert_eq!(sse_message(stream, 1).unwrap()["result"], json!("not ours"));
    }

    #[test]
    fn joins_a_payload_split_across_several_data_lines() {
        // Per SSE, multiple data: lines join with a newline before parsing.
        let stream = "data: {\"jsonrpc\":\"2.0\",\n data: \"id\":3,\n data: \"result\":1}\n\n";
        // The leading space after `data:` is the optional one SSE strips; anything past it is
        // payload, so this stream's continuation lines carry their own `data: ` prefix as text
        // and must NOT parse. The honest shape is the one below.
        assert!(sse_message(stream, 3).is_err());

        let proper = "data: {\"jsonrpc\":\"2.0\",\ndata:\"id\":3,\ndata:\"result\":1}\n\n";
        assert_eq!(sse_message(proper, 3).unwrap()["result"], json!(1));
    }

    #[test]
    fn a_stream_with_no_answer_for_our_request_is_an_error_not_an_empty_result() {
        let err =
            sse_message("data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":1}\n\n", 4).unwrap_err();
        assert!(err.contains("no response for request 4"), "{err}");
        assert!(sse_message("", 1).is_err());
        // Unparseable payloads are skipped rather than failing the whole read.
        assert!(sse_message("data: not json\n\n", 1).is_err());
    }

    #[test]
    fn a_final_event_without_a_trailing_blank_line_is_still_read() {
        let stream = "data: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":\"last\"}";
        assert_eq!(sse_message(stream, 5).unwrap()["result"], json!("last"));
    }

    // ---- construction ------------------------------------------------------------------

    fn adapter_err(d: Value) -> String {
        match HttpAdapter::new(&def(d), "r", crate::calls::test_log()) {
            Err(err) => err,
            Ok(_) => panic!("this def must not build an http adapter"),
        }
    }

    #[test]
    fn an_http_mcp_needs_a_url_and_a_usable_proxy() {
        assert!(adapter_err(json!({ "type": "http" })).contains("needs a url"));
        assert!(adapter_err(json!({ "type": "http", "url": "" })).contains("needs a url"));
        assert!(!adapter_err(json!({
            "type": "http",
            "url": "https://x.test/mcp",
            "proxy": "not a url",
        }))
        .is_empty());
        // An empty proxy is "no proxy", exactly as Node's truthiness check read it.
        assert!(HttpAdapter::new(
            &def(json!({ "type": "http", "url": "https://x.test/mcp", "proxy": "" })),
            "r",
            crate::calls::test_log(),
        )
        .is_ok());
    }

    #[test]
    fn seeds_its_tool_toggle_from_the_def_so_a_toggle_survives_a_restart() {
        let a = HttpAdapter::new(
            &def(json!({
                "type": "http",
                "url": "https://x.test/mcp",
                "disabledTools": ["hidden", 5],
            })),
            "r",
            crate::calls::test_log(),
        )
        .expect("adapter");
        assert_eq!(a.kind(), "http");
        let disabled = a.tool_toggle().expect("http adapters carry a tool toggle");
        let names: Vec<String> = disabled.read().unwrap().iter().cloned().collect();
        assert_eq!(names, vec!["hidden"], "non-string entries are dropped");
    }

    #[test]
    fn a_rename_reaches_the_name_the_call_log_is_filed_under() {
        let a = HttpAdapter::new(
            &def(json!({ "type": "http", "url": "https://x.test/mcp" })),
            "r",
            crate::calls::test_log(),
        )
        .expect("adapter");
        a.rename("renamed");
        assert_eq!(a.name.read().unwrap().as_str(), "renamed");
    }
}
