/*
 * Copyright 2026 The swiss authors
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

//! The shared tool-server skeleton behind the direct DB adapters — port of `tool-server.ts`.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ResourcesCapability, ServerCapabilities, ServerInfo, Tool,
    ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use crate::adapters::resources::{ResourceBody, ResourceEntry, ResourceFault, ResourceProvider};
use crate::calls::CallSource;
use swiss_host::dbbrowser::BrowserFlavor;

pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments (an `object` schema).
    pub input_schema: Value,
}

/// Output budget for a single tool result. Without one, `KEYS *` or `SELECT * FROM big_table`
/// builds the entire reply in memory and floods the client's context, which is the more
/// expensive of the two problems.
#[derive(Clone, Copy)]
pub struct RenderLimits {
    /// Max array elements rendered.
    pub max_items: usize,
    /// Max serialized bytes.
    pub max_bytes: usize,
}

pub const DEFAULT_LIMITS: RenderLimits = RenderLimits {
    max_items: 1000,
    max_bytes: 256 * 1024,
};

/// Results this small stay pretty-printed; anything larger is rendered compact.
///
/// Indentation is ~25% of a wide SQL row (one newline plus six spaces per column), which buys
/// nothing on a reply that is already too long to read at a glance. Small answers — SELECT 1, a
/// describe, a handful of keys — still arrive formatted.
const PRETTY_MAX: usize = 1024;

/// Cut a string down to a BYTE budget, dropping any trailing partial character whole rather than
/// replacing it with U+FFFD (which would cost three bytes of the room we just made).
fn clip_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn byte_len(text: &str) -> usize {
    text.len()
}

/// The array a result is mostly made of, so items can be dropped from it — `(items, host)` where
/// `host` is the object the list lives in (or a marker map meaning "bare array").
///
/// A bare array is its own list, but the SQL adapters return `{ rowCount, rows: [...] }` and
/// redis_scan returns `{ cursor, keys: [...] }` — the list is one level down. Without this,
/// shedding never applied to the shapes that actually get large, and a 200-row `SELECT *` was
/// hard-cut mid-string: the model received JSON it could not parse, which is the one outcome
/// this function exists to prevent.
fn list_of(value: &Value) -> Option<(Vec<Value>, Map<String, Value>)> {
    match value {
        Value::Array(items) => {
            let mut marker = Map::new();
            marker.insert(String::new(), Value::Array(Vec::new()));
            Some((items.clone(), marker))
        }
        Value::Object(obj) => {
            // The largest array field is the list; every other key is metadata to keep.
            let mut key: Option<String> = None;
            let mut best = 0usize;
            for (k, v) in obj {
                if let Value::Array(items) = v {
                    if items.len() > best {
                        key = Some(k.clone());
                        best = items.len();
                    }
                }
            }
            let key = key?;
            let items = obj.get(&key).and_then(Value::as_array)?.clone();
            Some((items, obj.clone()))
        }
        _ => None,
    }
}

/// The array field of the host map (largest one wins — `list_of` guaranteed it exists).
fn array_key_of(host: &Map<String, Value>) -> Option<String> {
    host.iter()
        .filter(|(_, v)| matches!(v, Value::Array(_)))
        .max_by_key(|(_, v)| v.as_array().map(|a| a.len()).unwrap_or(0))
        .map(|(k, _)| k.clone())
}

/// Rewrap a shed list into the object it came from (or a bare array), reporting the count
/// actually returned, so `rowCount` never contradicts the rows beside it.
fn rewrap(host: &Map<String, Value>, items: Vec<Value>) -> Value {
    if host.len() == 1 && host.contains_key("") {
        return Value::Array(items);
    }
    let mut out = host.clone();
    if let Some(key) = array_key_of(host) {
        let count = items.len();
        out.insert(key, Value::Array(items));
        if out.contains_key("rowCount") {
            out.insert("rowCount".into(), json!(count));
        }
    }
    Value::Object(out)
}

/// Serialize a tool result within the given budget, telling the model explicitly when output was
/// dropped and how to narrow it — a silently truncated list reads as a complete one.
pub fn render_result(result: &Value, limits: RenderLimits) -> String {
    let mut value = result.clone();
    let mut notes: Vec<String> = Vec::new();

    if let Value::String(text) = &value {
        if byte_len(text) <= limits.max_bytes {
            return text.clone();
        }
        return format!(
            "{}\n\n[truncated at {} bytes]",
            clip_bytes(text, limits.max_bytes),
            limits.max_bytes
        );
    }

    let list = list_of(&value);
    let total_items = list.as_ref().map(|(items, _)| items.len()).unwrap_or(0);

    if let Some((items, host)) = &list {
        if total_items > limits.max_items {
            notes.push(format!(
                "showing the first {} of {} items",
                limits.max_items, total_items
            ));
            value = rewrap(host, items[..limits.max_items].to_vec());
        }
    }

    let mut text = serde_json::to_string(&value).unwrap_or_default();

    // Over budget: drop items until it fits, so the output stays valid JSON rather than being cut
    // mid-token. A result with no list to shed has nothing to give and gets a hard cut.
    if byte_len(&text) > limits.max_bytes {
        if let Some((items, host)) = &list {
            let mut kept = items[..items.len().min(limits.max_items)].to_vec();
            while kept.len() > 1 && byte_len(&text) > limits.max_bytes {
                kept.truncate(kept.len() / 2);
                value = rewrap(host, kept.clone());
                text = serde_json::to_string(&value).unwrap_or_else(|_| text.clone());
            }
            notes.clear(); // the item count changed — restate it below
            notes.push(format!(
                "showing {} of {} items ({}-byte output budget)",
                kept.len(),
                total_items,
                limits.max_bytes
            ));
        }
    }

    if byte_len(&text) <= PRETTY_MAX {
        if let Ok(pretty) = serde_json::to_string_pretty(&value) {
            text = pretty;
        }
    }

    if byte_len(&text) > limits.max_bytes {
        text = clip_bytes(&text, limits.max_bytes);
        notes.push(format!(
            "truncated at {} bytes — this is no longer valid JSON",
            limits.max_bytes
        ));
    }

    if notes.is_empty() {
        return text;
    }
    format!(
        "{text}\n\n[{}. Narrow the request (SQL LIMIT, a key pattern, SCAN COUNT) to see the rest.]",
        notes.join("; ")
    )
}

/// Identity of one hosted MCP, so a client can tell same-engine endpoints apart.
///
/// A gateway can host several instances of the same engine — two Redis caches, two Postgres
/// databases. Without this, every one of them advertises an identical tool set and the only
/// distinguishing information is the path.
pub struct ServerMeta {
    /// Registry name of this MCP — the key its call log is filed under.
    pub name: Option<String>,
    /// What this MCP is for and which application it serves, from the config's `description`.
    pub description: Option<String>,
    /// Where it connects — "app @ localhost:3306", "localhost:6380 db 0". Never a password.
    pub target: Option<String>,
    pub limits: Option<RenderLimits>,
}

impl ServerMeta {
    pub fn limits(&self) -> RenderLimits {
        self.limits.unwrap_or(DEFAULT_LIMITS)
    }
}

/// Build the MCP `instructions` string returned by initialize — the server-level description of
/// what this endpoint is and what it is connected to, stated once per server rather than
/// repeated into every tool's description.
fn build_instructions(meta: &ServerMeta) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(description) = &meta.description {
        parts.push(description.clone());
    }
    if let Some(target) = &meta.target {
        parts.push(format!(
            "Connected to {target}; every tool here acts on that instance."
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The engine-specific half of a direct adapter — what `DirectAdapter`'s subclasses supplied in
/// the Node build: the tools, where the endpoint points, and what a tool call does.
#[async_trait]
pub trait Engine: Send + Sync {
    fn kind(&self) -> &'static str;
    fn tools(&self) -> Vec<ToolDef>;
    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String>;
    fn meta(&self) -> ServerMeta;
    fn resources(&self) -> Option<Arc<dyn ResourceProvider>> {
        None
    }
    /// Reachability probe; `None` means this engine has none. Mirrors [`Adapter::ping`].
    async fn ping(&self) -> Option<Result<(), String>> {
        None
    }
    /// Close the connection(s) on shutdown. Best effort by contract.
    async fn close(&self) {}
    /// Follow a registry rename, so `meta()` keeps reporting the MCP's current name.
    fn rename(&self, _name: &str) {}

    fn browser(&self) -> Option<BrowserFlavor> {
        None
    }
}

/// An in-process MCP server exposing an [`Engine`]'s tools — port of `makeToolServer`. Built
/// fresh per request by the adapter's factory, so concurrent requests never share one
/// transport-bound server, while the engine (and its connection) underneath is shared.
///
/// Note: tools deliberately carry no `annotations` (readOnlyHint & co). Claude Code negotiates
/// protocol 2024-11-05 over HTTP, where that key is unknown and strict parsing rejects the whole
/// tools/list. Read-only vs destructive is encoded in the tool name and description instead.
pub struct ToolServer {
    pub engine: Arc<dyn Engine>,
    /// Tool names the operator has turned off — filtered at factory time, so a toggle is live
    /// on the very next request.
    pub disabled_tools: std::collections::HashSet<String>,
    /// The resources master switch (off = resources/list answers empty, capability stays).
    pub resources_on: bool,
    /// Who is calling (see `calls::CallSource`).
    pub source: CallSource,
    /// Where the call is recorded: the app's CallLog, threaded in at construction.
    pub log: std::sync::Arc<crate::calls::CallLog>,
}

fn tool_to_rmcp(def: &ToolDef) -> Tool {
    let schema: JsonObject =
        serde_json::from_value(def.input_schema.clone()).unwrap_or_else(|_| {
            serde_json::from_value(json!({ "type": "object" })).expect("literal object")
        });
    let mut tool = Tool::default();
    tool.name = def.name.clone().into();
    tool.description = Some(def.description.clone().into());
    tool.input_schema = Arc::new(schema);
    tool
}

impl ToolServer {
    fn advertised(&self) -> Vec<ToolDef> {
        self.engine
            .tools()
            .into_iter()
            .filter(|t| !self.disabled_tools.contains(&t.name))
            .collect()
    }

    fn resource_to_rmcp(e: &ResourceEntry) -> Resource {
        let mut r = Resource::new(e.uri.clone(), e.name.clone());
        if let Some(d) = &e.description {
            r = r.with_description(d.clone());
        }
        if let Some(m) = &e.mime_type {
            r = r.with_mime_type(m.clone());
        }
        r
    }

    fn body_to_contents(b: &ResourceBody, max_bytes: usize) -> ResourceContents {
        // The same output budget tool results get: an `@` mention drops this straight into a
        // context window.
        let text = if b.text.len() > max_bytes {
            format!(
                "{}\n\n[truncated at {max_bytes} bytes]",
                clip_bytes(&b.text, max_bytes)
            )
        } else {
            b.text.clone()
        };
        let mut c = ResourceContents::text(text, b.uri.clone());
        if let Some(m) = &b.mime_type {
            c = c.with_mime_type(m.clone());
        }
        c
    }
}

impl ServerHandler for ToolServer {
    fn get_info(&self) -> ServerInfo {
        let meta = self.engine.meta();
        let mut capabilities = ServerCapabilities::default();
        // `tools.listChanged` and `resources.listChanged` are both announced because either can
        // be toggled at runtime. `resources` carries neither `subscribe` nor a poll: clients
        // re-list when the user opens the picker anyway.
        let mut tools_cap = ToolsCapability::default();
        tools_cap.list_changed = Some(true);
        capabilities.tools = Some(tools_cap);
        if self.engine.resources().is_some() {
            let mut res_cap = ResourcesCapability::default();
            res_cap.list_changed = Some(true);
            capabilities.resources = Some(res_cap);
        }
        let mut server_info = Implementation::default();
        server_info.name = "mcp-gateway-direct".into();
        server_info.version = "1.0".into();
        let mut info = ServerInfo::new(capabilities).with_server_info(server_info);
        if let Some(instructions) = build_instructions(&meta) {
            info = info.with_instructions(instructions);
        }
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(
            self.advertised().iter().map(tool_to_rmcp).collect(),
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        // Feeds the transport's SEP-2243 header validation. A disabled tool is NOT gettable:
        // the advertised list is the contract.
        self.advertised()
            .iter()
            .find(|t| t.name == name)
            .map(tool_to_rmcp)
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        // Logged here rather than in each adapter: this is where the rendered text exists, so
        // the log holds exactly what the caller was sent.
        let meta = self.engine.meta();
        let limits = meta.limits();
        let mcp = meta.name.clone();
        let tool = request.name.to_string();
        let args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Null);
        let engine = self.engine.clone();
        let advertised = self.advertised();
        let source = self.source.clone();
        let log = self.log.clone();
        // logged() holds `&tool` for as long as its future lives, while the run block below
        // needs the name by value — two bindings, one borrow each.
        let tool_for_log = tool.clone();
        async move {
            log.logged(
                mcp.as_deref(),
                &source,
                &tool_for_log,
                if args.is_null() {
                    None
                } else {
                    Some(args.clone())
                },
                async move {
                    // The advertised list is the contract. A tool hidden from tools/list — a
                    // readonly instance's write tools, or anything the operator disabled — must
                    // not be summonable back by naming it directly.
                    if !advertised.iter().any(|t| t.name == tool) {
                        return Err(format!("unknown tool: {tool}"));
                    }
                    let result = engine.call(&tool, &args).await?;
                    Ok::<_, String>(CallToolResult::success(vec![ContentBlock::text(
                        render_result(&result, limits),
                    )]))
                },
                |result: &CallToolResult| {
                    // Every error path here is the Err arm; a Complete result always logs ok.
                    let text = result
                        .content
                        .iter()
                        .map(|c| match c {
                            ContentBlock::Text(t) => t.text.clone(),
                            other => format!("[{other:?} content]"),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (true, text)
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
        let cursor = request.and_then(|p| p.cursor);
        let provider = self.engine.resources();
        let on = self.resources_on;
        async move {
            match (provider, on) {
                (Some(provider), true) => match provider.list(cursor.as_deref()).await {
                    Ok(page) => {
                        let mut out = ListResourcesResult::with_all_items(
                            page.resources.iter().map(Self::resource_to_rmcp).collect(),
                        );
                        out.next_cursor = page.next_cursor;
                        Ok(out)
                    }
                    // A ResourceFault becomes -32602, which the spec requires for a cursor that
                    // means nothing; anything else would keep the default -32603.
                    Err(fault) => Err(ErrorData::invalid_params(fault.0, None)),
                },
                // Toggled off: the client sees no resources, while the capability stays announced.
                _ => Ok(ListResourcesResult::default()),
            }
        }
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + MaybeSendFuture + '_
    {
        let provider = self.engine.resources();
        let on = self.resources_on;
        let templates = match (provider, on) {
            (Some(provider), true) => provider.templates(),
            _ => Vec::new(),
        };
        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(
            templates
                .into_iter()
                .map(|t| {
                    let mut rt = ResourceTemplate::new(t.uri_template, t.name);
                    if let Some(d) = t.description {
                        rt = rt.with_description(d);
                    }
                    if let Some(m) = t.mime_type {
                        rt = rt.with_mime_type(m);
                    }
                    rt
                })
                .collect(),
        )))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ReadResourceResponse, ErrorData>> + MaybeSendFuture + '_
    {
        let provider = self.engine.resources();
        let limits = self.engine.meta().limits();
        let mcp = self.engine.meta().name.clone();
        let source = self.source.clone();
        let log = self.log.clone();
        let uri = request.uri;
        async move {
            let Some(provider) = provider else {
                return Err(ErrorData::internal_error(
                    "this MCP exposes no resources",
                    None,
                ));
            };
            // Reads go through the call log, so the panel's Logs tab shows which schema a
            // client attached. The resources toggle does NOT gate reads in the Node build — a
            // URI the client already holds keeps working.
            log.logged(
                mcp.as_deref(),
                &source,
                "resources/read",
                Some(json!({ "uri": uri })),
                async {
                    let bodies = provider
                        .read(&uri)
                        .await
                        .map_err(|fault: ResourceFault| fault.0)?;
                    let joined = bodies
                        .iter()
                        .map(|b| b.text.clone())
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok::<_, String>((bodies, joined))
                },
                |(_, joined): &(Vec<ResourceBody>, String)| (true, joined.clone()),
            )
            .await
            .map(|(bodies, _)| {
                rmcp::model::ReadResourceResponse::Complete(ReadResourceResult::new(
                    bodies
                        .iter()
                        .map(|b| Self::body_to_contents(b, limits.max_bytes))
                        .collect(),
                ))
            })
            .map_err(|message| ErrorData::internal_error(message, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_results_stay_pretty_and_complete() {
        let out = render_result(
            &json!({ "rowCount": 1, "rows": [{ "id": 1 }] }),
            DEFAULT_LIMITS,
        );
        assert!(out.contains("\n"), "small result is pretty-printed");
        assert!(!out.contains('[') || !out.starts_with('[') || out.contains("rowCount"));
    }

    #[test]
    fn string_results_pass_through_under_budget() {
        assert_eq!(render_result(&json!("hello"), DEFAULT_LIMITS), "hello");
        let long = "x".repeat(DEFAULT_LIMITS.max_bytes + 100);
        let out = render_result(&json!(long), DEFAULT_LIMITS);
        assert!(out.contains("[truncated at 262144 bytes]"));
    }

    #[test]
    fn item_budget_drops_rows_and_says_so() {
        let rows: Vec<Value> = (0..1500).map(|i| json!({ "n": i })).collect();
        let out = render_result(&json!({ "rowCount": 1500, "rows": rows }), DEFAULT_LIMITS);
        assert!(
            out.contains("showing"),
            "must state what was dropped: {out}"
        );
        assert!(out.contains("Narrow the request"));
    }

    #[test]
    fn byte_budget_halves_until_it_fits() {
        // 1000 items is under max_items but far over max_bytes.
        let rows: Vec<Value> = (0..1000)
            .map(|i| json!({ "n": i, "pad": "0123456789abcdefghij".repeat(20) }))
            .collect();
        let mut limits = DEFAULT_LIMITS;
        limits.max_bytes = 8 * 1024;
        let out = render_result(&json!({ "rowCount": 1000, "rows": rows }), limits);
        assert!(out.len() <= limits.max_bytes + 512, "output {}", out.len());
        assert!(out.contains("output budget"));
    }

    #[test]
    fn bare_arrays_shed_too() {
        let items: Vec<Value> = (0..1200).map(|i| json!(i)).collect();
        let out = render_result(&json!(items), DEFAULT_LIMITS);
        assert!(out.contains("showing the first 1000 of 1200 items"));
    }
}
