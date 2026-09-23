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
use serde_json::value::RawValue;
use serde_json::{json, Value};

use swiss_host::config::ServerDef;

use super::direct::{def_bool, BoxFut, Lazy};
use super::proxy::{assert_proxy_url, proxied_client, ProxyOpts, ProxyServer, RemoteMcp};
use super::{rmcp_endpoint, Adapter, McpEndpoint};
use crate::oauth::{self as oauth_flow, Discovered, NEEDS_AUTH};

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

/// The OAuth half of a proxied remote (docs/24 D4): where the bearer comes from, and what to
/// do when it stops working. Holds the registry name (followed across renames — credentials
/// are keyed by it), the MCP URL, the outbound client (the same proxy the MCP traffic rides),
/// a discovery cache, and the single-flight lock that makes concurrent 401s share one refresh.
struct OauthHalf {
    name: Arc<RwLock<String>>,
    url: String,
    http: reqwest::Client,
    discovery: RwLock<Option<Discovered>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl OauthHalf {
    fn new(name: Arc<RwLock<String>>, url: &str, http: reqwest::Client) -> Self {
        Self {
            name,
            url: url.to_string(),
            http,
            discovery: RwLock::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn current_name(&self) -> String {
        self.name.read().ok().map(|g| g.clone()).unwrap_or_default()
    }

    /// The endpoints, discovered once and cached — a failed attempt is never cached, so the
    /// next 401 gets a fresh discovery (docs/24 D4).
    async fn ensure_discovered(&self) -> Result<Discovered, String> {
        if let Ok(cell) = self.discovery.read() {
            if let Some(found) = cell.clone() {
                return Ok(found);
            }
        }
        let found = oauth_flow::discover(&self.http, &self.url).await?;
        if let Ok(mut cell) = self.discovery.write() {
            *cell = Some(found.clone());
        }
        Ok(found)
    }

    async fn refresh_from(
        &self,
        creds: &oauth_flow::StoredCredentials,
    ) -> Result<oauth_flow::TokenSet, String> {
        let refresh_token = creds.refresh_token.as_deref().ok_or_else(|| {
            format!("{NEEDS_AUTH} — no refresh token stored; open the panel and click Authorize")
        })?;
        let found = self.ensure_discovered().await?;
        let reg = oauth_flow::ClientReg {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.clone(),
            token_auth: creds.token_auth.clone(),
        };
        oauth_flow::refresh(
            &self.http,
            &found.token_endpoint,
            &reg,
            refresh_token,
            &found.resource,
        )
        .await
    }

    /// A token usable right now: the stored one when fresh, a refreshed one when not. No
    /// credentials at all is the plain needs-auth start error.
    async fn usable_token(&self) -> Result<String, String> {
        let name = self.current_name();
        match oauth_flow::credentials(&name) {
            Some(creds) if creds.access_fresh() => creds
                .access_token
                .clone()
                .ok_or_else(|| format!("{NEEDS_AUTH} — open the panel and click Authorize")),
            Some(creds) => {
                let tokens = match self.refresh_from(&creds).await {
                    Ok(tokens) => tokens,
                    Err(err) => {
                        self.mark_needs_auth();
                        return Err(err);
                    }
                };
                oauth_flow::update_tokens(&name, &tokens)?;
                Ok(tokens.access_token)
            }
            None => Err(format!("{NEEDS_AUTH} — open the panel and click Authorize")),
        }
    }

    /// The 401 path, single-flown: concurrent refusals share one refresh, and a waiter that
    /// finds the token already REPLACED (another request refreshed while this one queued)
    /// takes the new one without spending the grant again (docs/24 D4). The dedup key is the
    /// refused token, NOT freshness — a no-expiry token reads fresh right up to the 401 that
    /// just proved it dead, and shortcutting on "fresh" would hand the corpse back.
    async fn refresh_bearer(&self, refused: &str) -> Result<String, String> {
        let _single = self.refresh_lock.lock().await;
        let name = self.current_name();
        match oauth_flow::credentials(&name) {
            Some(creds)
                if creds.access_fresh()
                    && creds.access_token.as_deref() != Some(refused)
                    && !refused.is_empty() =>
            {
                creds
                    .access_token
                    .clone()
                    .ok_or_else(|| format!("{NEEDS_AUTH} — open the panel and click Authorize"))
            }
            Some(creds) => {
                let tokens = self.refresh_from(&creds).await?;
                oauth_flow::update_tokens(&name, &tokens)?;
                Ok(tokens.access_token)
            }
            None => Err(format!("{NEEDS_AUTH} — open the panel and click Authorize")),
        }
    }

    /// Drop the stored credentials — the needs-auth outcome (docs/24 D2.7): the old client_id
    /// is worthless anyway (each flow re-registers on a fresh loopback port).
    fn mark_needs_auth(&self) {
        if let Err(err) = oauth_flow::clear_credentials(&self.current_name()) {
            swiss_core::log::warn(
                "clearing oauth credentials failed",
                Some(json!({ "err": err })),
            );
        }
    }
}

/// The JSON-RPC request envelope, assembled without materializing a DOM: `params` rides
/// through as raw JSON whatever its size (AGENTS.md — no serde_json::Value on a forwarding
/// path).
#[derive(serde::Serialize)]
struct RequestEnvelope<'a> {
    jsonrpc: &'a str,
    id: i64,
    method: &'a str,
    params: &'a RawValue,
}

/// The response envelope — only the fields the transport ROUTES on: the id match that
/// demultiplexes SSE, and the error pointer. `result` stays raw; callers that need it typed
/// parse it once, directly from the borrowed bytes.
#[derive(Debug, serde::Deserialize)]
struct ResponseEnvelope {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    error: Option<RemoteErrorShape>,
    #[serde(default)]
    result: Option<Box<RawValue>>,
}

#[derive(Debug, serde::Deserialize)]
struct RemoteErrorShape {
    #[serde(default)]
    message: Option<String>,
}

/// The connected remote: one reqwest client, the negotiated protocol version and session id,
/// and the request-id counter that multiplexes concurrent POSTs.
pub struct RemoteMcpClient {
    client: reqwest::Client,
    url: String,
    headers: RemoteHeaders,
    /// The OAuth half, when this remote is an OAuth MCP — owns the bearer's source and its
    /// refresh (docs/24 D4). None on a plain http MCP.
    oauth: Option<Arc<OauthHalf>>,
    /// The bearer currently in force; swapped on refresh, read on every request.
    bearer: RwLock<Option<String>>,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<String>,
    next_id: AtomicI64,
}

impl RemoteMcpClient {
    /// Connect: run the initialize handshake — this IS the reachability check, which is why
    /// it happens eagerly in build() and not lazily on the first call — and capture what the
    /// remote negotiated. The client is built by the caller (proxied when the def names a
    /// proxy) so the OAuth half can share the exact same outbound path (docs/24 D4).
    async fn connect(
        client: reqwest::Client,
        url: &str,
        headers: RemoteHeaders,
        auth: Option<Arc<OauthHalf>>,
    ) -> Result<(Arc<RemoteMcpClient>, Value), String> {
        // OAuth MCPs gate every request behind the bearer; an unusable one is a start error
        // carrying the marker the panel lights up on (docs/24 D4).
        let mut bearer = None;
        if let Some(auth) = &auth {
            bearer = Some(auth.usable_token().await?);
        }
        let session = Arc::new(RemoteMcpClient {
            client,
            url: url.to_string(),
            headers,
            oauth: auth,
            bearer: RwLock::new(bearer),
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(OFFERED_PROTOCOL_VERSION.to_string()),
            next_id: AtomicI64::new(1),
        });
        let init_params = small_raw(&json!({
            "protocolVersion": OFFERED_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "swiss", "version": "1.0" },
        }))?;
        let result = session.request("initialize", &init_params, None).await?;
        // The negotiated version and capabilities are envelope fields the client routes on,
        // so this one small parse is the sanctioned exception; nothing else reads the answer.
        let negotiated: Value = serde_json::from_str(result.get()).unwrap_or_else(|_| json!({}));
        if let Some(version) = negotiated.get("protocolVersion").and_then(Value::as_str) {
            if !version.is_empty() {
                if let Ok(mut cell) = session.protocol_version.lock() {
                    *cell = version.to_string();
                }
            }
        }
        // The spec-mandated follow-up. A server that refuses it is not speaking streamable
        // HTTP, and refusing to connect here is the same start error the Node build produced.
        session.notify("notifications/initialized").await?;
        let caps = negotiated.get("capabilities").cloned().unwrap_or(json!({}));
        Ok((session, caps))
    }

    fn session_header(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|g| g.clone())
    }

    fn apply_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // Order mirrors the Node transport: defaults first, then the def's headers (a def may
        // override Accept or Content-Type), then OAuth's bearer, then the protocol-critical
        // headers last so a bad def value can never break session routing or version
        // negotiation. The bearer sits AFTER the def headers on purpose: on an OAuth MCP the
        // gateway owns Authorization (a def that hand-writes one is refused at construction),
        // and before the protocol headers, which always win.
        let mut builder = builder
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        for (name, value) in &self.headers.0 {
            builder = builder.header(name, value);
        }
        if let Ok(cell) = self.bearer.read() {
            if let Some(bearer) = cell.as_ref() {
                builder = builder.header("authorization", format!("Bearer {bearer}"));
            }
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
        params: &RawValue,
        timeout_ms: Option<u64>,
    ) -> Result<Box<RawValue>, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // Raw envelope assembly — no DOM on this path (AGENTS.md): the params blob rides
        // through verbatim whatever its size, and only the fields the transport routes on
        // (the id match, the error pointer) are ever parsed out of the answer.
        let body = serde_json::to_string(&RequestEnvelope {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|err| format!("building {method} request: {err}"))?;
        // OAuth 401s get ONE refresh-retry, no more (docs/24 D4): a second refusal after a
        // fresh token means the grant itself is dead, and the marker below sends the operator
        // to the panel's Authorize button instead of into a refresh loop.
        let mut refreshed = false;
        loop {
            let builder = self
                .apply_headers(self.client.request(reqwest::Method::POST, &self.url))
                .timeout(Duration::from_millis(
                    timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
                ))
                .body(body.clone());
            let response = builder
                .send()
                .await
                .map_err(|err| format!("Error POSTing to endpoint ({}): {err}", self.url))?;
            let status = response.status();
            if status.as_u16() == 401 && self.oauth.is_some() {
                let auth = self.oauth.clone().expect("checked just above");
                if !refreshed {
                    refreshed = true;
                    // What this request rode when it was refused — the dedup key below.
                    let refused = self
                        .bearer
                        .read()
                        .ok()
                        .and_then(|cell| cell.clone())
                        .unwrap_or_default();
                    let token = match auth.refresh_bearer(&refused).await {
                        Ok(token) => token,
                        Err(err) => {
                            auth.mark_needs_auth();
                            return Err(err);
                        }
                    };
                    if let Ok(mut cell) = self.bearer.write() {
                        *cell = Some(token);
                    }
                    continue;
                }
                auth.mark_needs_auth();
                return Err(format!(
                    "{NEEDS_AUTH} — the remote keeps refusing the token; \
                     open the panel and click Authorize"
                ));
            }
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(format!(
                    "Error POSTing to endpoint (HTTP {status}): {}",
                    preview(&text, 600)
                ));
            }
            // A session is issued on initialize (and may rotate); any answer may carry it.
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
                serde_json::from_str::<ResponseEnvelope>(&text)
                    .map_err(|err| format!("{method} response is not JSON: {err}"))?
            };
            if let Some(error) = message.error {
                let message_text = error
                    .message
                    .unwrap_or_else(|| "unknown remote error".to_string());
                return Err(message_text);
            }
            return Ok(message.result.unwrap_or_else(null_raw));
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
/// the stream — notifications, other ids — is not ours to read: the envelope carries only the
/// routed-on fields and the result payload stays raw (AGENTS.md), so a chatty stream costs
/// scans, not trees.
fn sse_message(text: &str, id: i64) -> Result<ResponseEnvelope, String> {
    let mut data: Vec<String> = Vec::new();
    let mut found: Option<ResponseEnvelope> = None;
    let flush = |data: &mut Vec<String>, found: &mut Option<ResponseEnvelope>| {
        if data.is_empty() {
            return;
        }
        // Per SSE, multiple data: lines join with a newline; a JSON payload never splits that
        // way, but honoring it costs nothing and keeps the parser honest.
        let joined = data.join("\n");
        data.clear();
        if let Ok(message) = serde_json::from_str::<ResponseEnvelope>(&joined) {
            if message.id == Some(id) {
                *found = Some(message);
            }
        }
    };
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            flush(&mut data, &mut found);
            continue;
        }
        if let Some(payload) = line.strip_prefix("data:") {
            data.push(payload.strip_prefix(' ').unwrap_or(payload).to_string());
        }
        // "event:", "id:", "retry:" and ":" comments carry nothing this reader needs.
    }
    flush(&mut data, &mut found);
    found.ok_or_else(|| format!("SSE stream carried no response for request {id}"))
}

/// Wrap a small JSON value this module itself constructed — handshake params, list cursors —
/// as raw JSON. Never for forwarded payloads: those arrive typed and serialize once, direct.
fn small_raw(value: &Value) -> Result<Box<RawValue>, String> {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| RawValue::from_string(text).ok())
        .ok_or_else(|| "envelope params failed to encode".to_string())
}

/// Serialize a typed request-params object straight to raw JSON — one typed encode, no DOM
/// in between (AGENTS.md).
fn typed_params<T: serde::Serialize>(params: &T, what: &str) -> Result<Box<RawValue>, String> {
    serde_json::to_string(params)
        .map_err(|err| format!("{what}: {err}"))
        .and_then(|text| RawValue::from_string(text).map_err(|err| format!("{what}: {err}")))
}

/// The JSON `null` as a raw value — the answer a spec-loose remote gives when it omits
/// `result` entirely.
fn null_raw() -> Box<RawValue> {
    // Static JSON: from_string cannot fail.
    RawValue::from_string("null".to_string()).expect("static null is valid JSON")
}

impl RemoteMcpClient {
    fn list_params(cursor: Option<String>) -> Box<RawValue> {
        let envelope = match cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        // A static shape this module wrote itself; encoding it cannot fail.
        small_raw(&envelope).expect("static envelope params are valid JSON")
    }
}

#[async_trait]
impl RemoteMcp for RemoteMcpClient {
    async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, String> {
        let result = self
            .request("tools/list", &Self::list_params(cursor), None)
            .await?;
        serde_json::from_str(result.get()).map_err(|err| format!("tools/list response: {err}"))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        timeout_ms: Option<u64>,
    ) -> Result<CallToolResult, String> {
        // One typed encode, one raw ride, one typed decode: no DOM and no clone in between —
        // the tool arguments and results this carries are exactly why the rule exists.
        let params = typed_params(&params, "tools/call params")?;
        let result = self.request("tools/call", &params, timeout_ms).await?;
        serde_json::from_str(result.get()).map_err(|err| format!("tools/call response: {err}"))
    }

    async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, String> {
        let result = self
            .request("resources/list", &Self::list_params(cursor), None)
            .await?;
        serde_json::from_str(result.get()).map_err(|err| format!("resources/list response: {err}"))
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, String> {
        let params = typed_params(&params, "resources/read params")?;
        let result = self.request("resources/read", &params, None).await?;
        serde_json::from_str(result.get()).map_err(|err| format!("resources/read response: {err}"))
    }

    async fn list_prompts(&self, cursor: Option<String>) -> Result<ListPromptsResult, String> {
        let result = self
            .request("prompts/list", &Self::list_params(cursor), None)
            .await?;
        serde_json::from_str(result.get()).map_err(|err| format!("prompts/list response: {err}"))
    }

    async fn get_prompt(&self, params: GetPromptRequestParams) -> Result<GetPromptResult, String> {
        let params = typed_params(&params, "prompts/get params")?;
        let result = self.request("prompts/get", &params, None).await?;
        serde_json::from_str(result.get()).map_err(|err| format!("prompts/get response: {err}"))
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
    /// `def` must be the resolve_def_checked() clone make_adapter hands over. A bad proxy URL or a
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
        // OAuth (docs/24 D1): "auth": "oauth" makes the gateway own the Authorization
        // header — a def that also hand-writes one is a config error, refused here rather
        // than silently stacked under the bearer.
        let want_oauth = def.get_str("auth") == Some("oauth");
        if want_oauth && headers.0.iter().any(|(n, _)| n.as_str() == "authorization") {
            return Err(
                "an http MCP with auth oauth manages Authorization itself — remove the header from the def"
                    .to_string(),
            );
        }
        let description = def.get_str("description").map(str::to_string);
        // "!== false" in the Node build; def_bool also folds the panel's "false" strings in.
        let expose_resources = !def_bool(def, "exposeResources");
        let expose_prompts = !def_bool(def, "exposePrompts");
        // One name cell, shared by the adapter (rename-following) and the OAuth half (the
        // credential store is keyed by registry name).
        let name_cell = Arc::new(RwLock::new(name.to_string()));
        let conn_name = name_cell.clone();
        let conn = Arc::new(Lazy::new(move || {
            let url = url.clone();
            let headers = headers.clone();
            let proxy = proxy.clone();
            let name_cell = conn_name.clone();
            Box::pin(async move {
                let client = match &proxy {
                    Some(proxy) => proxied_client(proxy)?,
                    None => reqwest::Client::builder()
                        .build()
                        .map_err(|err| format!("http client build failed: {err}"))?,
                };
                let auth = if want_oauth {
                    Some(Arc::new(OauthHalf::new(name_cell, &url, client.clone())))
                } else {
                    None
                };
                let (session, caps) = RemoteMcpClient::connect(client, &url, headers, auth).await?;
                Ok(RemoteSession {
                    client: session,
                    caps,
                })
            }) as BoxFut<Result<RemoteSession, String>>
        }));
        // Persisted toggles ride the def (register_one/store seed "disabledTools"); seeded
        // the same way DirectAdapter does it, so a toggle survives a restart.
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
            name: name_cell,
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
        let message = sse_message(stream, 7).expect("our id came back");
        assert_eq!(message.id, Some(7));
        assert!(message.error.is_none());
        assert_eq!(sse_result(stream, 7), json!({ "ok": true }));
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
        assert_eq!(sse_result(stream, 2), json!("ours"));
        assert_eq!(sse_result(stream, 1), json!("not ours"));
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
        assert_eq!(sse_result(proper, 3), json!(1));
    }

    /// Decode the raw result of the answer carrying `id` — the tests care what rode through,
    /// the transport itself only routes on the envelope.
    fn sse_result(text: &str, id: i64) -> Value {
        let message = sse_message(text, id).expect("response for our id");
        serde_json::from_str(message.result.expect("result payload").get())
            .expect("raw result parses")
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
        assert_eq!(sse_result(stream, 5), json!("last"));
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

    // ---- OAuth (docs/24 Phase 2) ----------------------------------------------------------------
    //
    // One loopback server plays all three roles a real OAuth MCP deployment spreads across
    // hosts: the protected resource (/mcp, bearer-gated JSON-RPC), the authorization server
    // (the two well-knowns + the refresh endpoint). "current" is the ONE bearer /mcp accepts
    // — rotating it simulates token expiry server-side without waiting for expires_in.

    use crate::oauth::{self, StoredCredentials};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[derive(Default)]
    struct FakeOauthState {
        /// The one bearer /mcp accepts right now.
        current: Option<String>,
        /// The refresh token /token accepts; None refuses every refresh (invalid_grant).
        refresh_ok: Option<String>,
        /// /token hands back a token /mcp still rejects — the double-401 path.
        wrong_refresh_reply: bool,
        /// Every Authorization header /mcp actually received, in order.
        seen: Vec<String>,
    }

    struct FakeOauthRemote {
        base: String,
        state: Arc<Mutex<FakeOauthState>>,
    }

    async fn fake_oauth_remote(state: FakeOauthState) -> FakeOauthRemote {
        let state = Arc::new(Mutex::new(state));
        let s_meta = state.clone();
        let s_register = state.clone();
        let s_token = state.clone();
        let s_mcp = state.clone();
        let counter = Arc::new(AtomicI64::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let meta_a = base.clone();
        let meta_b = base.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(move || {
                    let base = meta_a;
                    async move {
                        axum::Json(json!({
                            "resource": format!("{base}/mcp"),
                            "authorization_servers": [base],
                        }))
                    }
                }),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(move || {
                    let base = meta_b;
                    async move {
                        axum::Json(json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/oauth/authorize"),
                            "token_endpoint": format!("{base}/token"),
                        }))
                    }
                }),
            )
            .route(
                "/token",
                axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                    let s = s_token.clone();
                    let counter = counter.clone();
                    async move {
                        let form: HashMap<String, String> = body
                            .split('&')
                            .filter_map(|p| p.split_once('='))
                            .map(|(k, v)| {
                                (
                                    crate::adapters::rest::form_decode(k),
                                    crate::adapters::rest::form_decode(v),
                                )
                            })
                            .collect();
                        let get = |k: &str| form.get(k).cloned().unwrap_or_default();
                        let _ = headers; // the client posts its credentials in the body
                        if get("client_id") != "cid-1" || get("client_secret") != "sec-1" {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "invalid_client" })),
                            );
                        }
                        let mut s = s.lock().unwrap();
                        if get("grant_type") != "refresh_token"
                            || Some(get("refresh_token")) != s.refresh_ok
                        {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "invalid_grant" })),
                            );
                        }
                        if s.wrong_refresh_reply {
                            return (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({
                                    "access_token": "a-token-mcp-still-rejects",
                                    "refresh_token": "ref-next",
                                    "expires_in": 3600,
                                })),
                            );
                        }
                        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        let access = format!("acc-rot-{n}");
                        s.current = Some(access.clone());
                        drop(s);
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(json!({
                                "access_token": access,
                                "refresh_token": "ref-next",
                                "expires_in": 3600,
                            })),
                        )
                    }
                }),
            )
            .route(
                "/mcp",
                axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                    let s = s_mcp.clone();
                    async move {
                        let expected = s.lock().unwrap().current.clone().unwrap_or_default();
                        let got = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        if got != format!("Bearer {expected}") {
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                axum::Json(json!({ "error": "unauthorized" })),
                            );
                        }
                        s.lock().unwrap().seen.push(got);
                        let message: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                        if message.get("id").is_none() {
                            // A notification: nothing meaningful to answer.
                            return (axum::http::StatusCode::OK, axum::Json(Value::Null));
                        }
                        match message.get("method").and_then(Value::as_str) {
                            Some("initialize") => (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": message.get("id"),
                                    "result": {
                                        "protocolVersion": "2025-06-18",
                                        "capabilities": {},
                                    },
                                })),
                            ),
                            Some("tools/call") => (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": message.get("id"),
                                    "result": { "content": [ { "type": "text", "text": "ok" } ] },
                                })),
                            ),
                            _ => (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": message.get("id"),
                                    "result": { "tools": [] },
                                })),
                            ),
                        }
                    }
                }),
            );
        drop((s_meta, s_register)); // captured-and-released: keep the clone list explicit
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeOauthRemote { base, state }
    }

    /// Plant credentials under `name` — no expiry advertised, so they stay fresh until the
    /// remote answers 401 (exactly what a token that dies server-side looks like).
    fn plant(name: &str, access: &str, refresh: Option<&str>) {
        oauth::replace_credentials(
            name,
            StoredCredentials {
                client_id: "cid-1".into(),
                client_secret: Some("sec-1".into()),
                token_auth: "client_secret_post".into(),
                access_token: Some(access.into()),
                refresh_token: refresh.map(str::to_string),
                expires_at: None,
                scope: None,
                at: 0,
            },
        )
        .expect("plant");
    }

    /// The store preamble (scratch home, test key, data-dir lock) — same as oauth.rs's tests.
    async fn store_preamble() -> tokio::sync::MutexGuard<'static, ()> {
        swiss_core::paths::test_home();
        swiss_core::secure::key::use_test_master_key();
        swiss_core::paths::DATA_DIR_LOCK.lock().await
    }

    fn oauth_def(base: &str) -> ServerDef {
        def(json!({ "type": "http", "url": format!("{base}/mcp"), "auth": "oauth" }))
    }

    #[test]
    fn a_def_may_not_handwrite_authorization_on_an_oauth_mcp() {
        let refused = match HttpAdapter::new(
            &def(json!({
                "type": "http", "url": "https://x.test/mcp", "auth": "oauth",
                "headers": { "Authorization": "Bearer x" },
            })),
            "handwritten",
            crate::calls::test_log(),
        ) {
            Err(err) => err,
            Ok(_) => panic!("a hand-written Authorization must be refused on an oauth MCP"),
        };
        assert!(
            refused.contains("manages Authorization itself"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn an_oauth_mcp_without_credentials_fails_with_the_needs_auth_marker() {
        let _guard = store_preamble().await;
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("whatever".into()),
            ..FakeOauthState::default()
        })
        .await;
        let a = HttpAdapter::new(
            &oauth_def(&remote.base),
            "http-oauth-none",
            crate::calls::test_log(),
        )
        .expect("adapter builds");
        let err = match a.build().await {
            Err(err) => err,
            Ok(_) => panic!("no stored credentials must be a start error"),
        };
        assert!(err.contains(crate::oauth::NEEDS_AUTH), "{err}");
    }

    #[tokio::test]
    async fn an_oauth_mcp_connects_with_the_stored_bearer() {
        let _guard = store_preamble().await;
        let name = "http-oauth-fresh";
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("acc-good".into()),
            ..FakeOauthState::default()
        })
        .await;
        plant(name, "acc-good", Some("ref-1"));
        let a = HttpAdapter::new(&oauth_def(&remote.base), name, crate::calls::test_log())
            .expect("adapter");
        let endpoint = a.build().await.expect("the stored bearer connects");
        let (tools, _) = endpoint
            .probe
            .list("tools", None)
            .await
            .expect("tools/list");
        assert!(tools.is_empty());
        let seen = remote.state.lock().unwrap().seen.clone();
        assert!(
            seen.iter().all(|h| h == "Bearer acc-good"),
            "every request rode the stored bearer: {seen:?}"
        );
    }

    #[tokio::test]
    async fn a_stale_bearer_is_refreshed_once_before_connect() {
        let _guard = store_preamble().await;
        let name = "http-oauth-stale";
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("acc-rot-9".into()),
            refresh_ok: Some("ref-1".into()),
            ..FakeOauthState::default()
        })
        .await;
        plant(name, "acc-long-dead", Some("ref-1"));
        // Expired long ago: the 60s margin already counts it dead (docs/24 D4).
        {
            let mut creds = oauth::credentials(name).expect("planted");
            creds.expires_at = Some(1);
            oauth::replace_credentials(name, creds).expect("age it");
        }
        let a = HttpAdapter::new(&oauth_def(&remote.base), name, crate::calls::test_log())
            .expect("adapter");
        let endpoint = a
            .build()
            .await
            .expect("the refresh happens before initialize");
        endpoint
            .probe
            .list("tools", None)
            .await
            .expect("tools work");
        let stored = oauth::credentials(name).expect("credentials survive");
        // What /token minted is what /mcp now accepts AND what is stored — one truth, read
        // from the remote rather than hard-coded, so the fake's rotation stays free to vary.
        let accepted = remote
            .state
            .lock()
            .unwrap()
            .current
            .clone()
            .expect("rotated");
        assert_eq!(stored.access_token.as_deref(), Some(accepted.as_str()));
        assert_eq!(stored.refresh_token.as_deref(), Some("ref-next"));
    }

    #[tokio::test]
    async fn a_refused_refresh_clears_credentials_and_reports_needs_auth() {
        let _guard = store_preamble().await;
        let name = "http-oauth-refused";
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("acc-2".into()),
            refresh_ok: None, // every refresh is an invalid_grant
            ..FakeOauthState::default()
        })
        .await;
        plant(name, "acc-expired", Some("ref-dead"));
        let mut creds = oauth::credentials(name).expect("planted");
        creds.expires_at = Some(1);
        oauth::replace_credentials(name, creds).expect("age it");
        let a = HttpAdapter::new(&oauth_def(&remote.base), name, crate::calls::test_log())
            .expect("adapter");
        let err = match a.build().await {
            Err(err) => err,
            Ok(_) => panic!("a dead grant must be a start error"),
        };
        assert!(err.contains(crate::oauth::NEEDS_AUTH), "{err}");
        assert!(
            oauth::credentials(name).is_none(),
            "the dead credentials are cleared, not left to rot"
        );
    }

    #[tokio::test]
    async fn a_request_time_401_refreshes_and_retries_once() {
        let _guard = store_preamble().await;
        let name = "http-oauth-401";
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("acc-first".into()),
            refresh_ok: Some("ref-1".into()),
            ..FakeOauthState::default()
        })
        .await;
        plant(name, "acc-first", Some("ref-1"));
        let a = HttpAdapter::new(&oauth_def(&remote.base), name, crate::calls::test_log())
            .expect("adapter");
        let endpoint = a.build().await.expect("connects while fresh");
        endpoint
            .probe
            .list("tools", None)
            .await
            .expect("fresh works");

        // The remote rotates under us: no expiry on our side, the 401 is the first notice.
        // A tool CALL is the observable (a failed tools/list degrades to empty by design).
        remote.state.lock().unwrap().current = Some("acc-rot-5".into());
        endpoint
            .probe
            .call_tool("anything", json!({}))
            .await
            .expect("the 401 refreshed and retried");
        let stored = oauth::credentials(name).expect("still stored");
        let accepted = remote
            .state
            .lock()
            .unwrap()
            .current
            .clone()
            .expect("rotated");
        assert_eq!(stored.access_token.as_deref(), Some(accepted.as_str()));
    }

    #[tokio::test]
    async fn a_second_401_after_the_refresh_marks_needs_auth() {
        let _guard = store_preamble().await;
        let name = "http-oauth-double401";
        let remote = fake_oauth_remote(FakeOauthState {
            current: Some("acc-live".into()),
            refresh_ok: Some("ref-1".into()),
            wrong_refresh_reply: true,
            ..FakeOauthState::default()
        })
        .await;
        plant(name, "acc-live", Some("ref-1"));
        let a = HttpAdapter::new(&oauth_def(&remote.base), name, crate::calls::test_log())
            .expect("adapter");
        let endpoint = a.build().await.expect("connects");
        // Rotate: the old token 401s, the refresh "succeeds" with a token /mcp still rejects.
        remote.state.lock().unwrap().current = Some("acc-rotated".into());
        let err = match endpoint.probe.call_tool("anything", json!({})).await {
            Err(err) => err,
            Ok(_) => panic!("a rejected post-refresh token must surface needs-auth"),
        };
        assert!(err.contains(crate::oauth::NEEDS_AUTH), "{err}");
        assert!(oauth::credentials(name).is_none(), "credentials dropped");
    }
}
