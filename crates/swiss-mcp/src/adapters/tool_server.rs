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

//! The shared tool-server skeleton behind the direct DB adapters — port of `tool-server.ts`.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ResourcesCapability, ServerCapabilities, ServerConfig, Tool,
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
///
/// The cut is deliberate and stays: a reply lands whole in the model's context window, and past
/// 1000 items or 256 KB one call stops being an answer and starts crowding out the conversation
/// that asked for it. What the cut must never do is change what the reply is (render_result).
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

/// What one render cut away, for the account the reply carries (budget_account).
#[derive(Default)]
struct Cut {
    /// `(shown, total)` when the result's list was shed.
    items: Option<(usize, usize)>,
    /// How many strings were clipped, and the byte length each was cut to.
    clipped: Option<(usize, usize)>,
    /// The value had nothing left to give: `partial` holds the start of its JSON as text.
    partial: bool,
}

/// Shortest a clipped string is cut to: below this the text says nothing anyway.
const CLIP_FLOOR: usize = 64;

/// The `outputBudget` object a cut reply carries: what was shown of how much, and how to narrow.
fn budget_account(limits: RenderLimits, cut: &Cut) -> Value {
    let mut out = Map::new();
    let mut said: Vec<String> = Vec::new();
    if let Some((shown, total)) = cut.items {
        out.insert("shown".into(), json!(shown));
        out.insert("total".into(), json!(total));
        said.push(format!("showing the first {shown} of {total} items"));
    }
    if let Some((count, keep)) = cut.clipped {
        out.insert("clippedStrings".into(), json!(count));
        said.push(format!(
            "{count} long string(s) cut to {keep} bytes, each ending in …[+N bytes]"
        ));
    }
    if cut.partial {
        said.push("the value had no list or long string to drop, so `partial` holds the start of its JSON as text".into());
    }
    out.insert(
        "note".into(),
        json!(format!(
            "Cut to the output budget ({} items, {} bytes a reply), which keeps one call from \
             flooding the model's context: {}. Narrow the request (SQL LIMIT, a key pattern, \
             SCAN COUNT) to see the rest.",
            limits.max_items,
            limits.max_bytes,
            said.join("; ")
        )),
    );
    Value::Object(out)
}

/// The cut value with its account: `truncated: true` and `outputBudget` beside the fields of an
/// object (an engine's own `truncated` means the same thing — not everything is here), or a bare
/// array wrapped as `{ items, truncated, outputBudget }`, since an array has nowhere to say it.
fn annotate(value: Value, limits: RenderLimits, cut: &Cut) -> Value {
    let mut out = match value {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("items".into(), other);
            map
        }
    };
    out.insert("truncated".into(), json!(true));
    out.insert("outputBudget".into(), budget_account(limits, cut));
    Value::Object(out)
}

/// Every string in `value` longer than `keep` bytes cut to `keep` (on a char boundary) and
/// marked with how much went; returns how many were cut.
fn clip_strings(value: &mut Value, keep: usize) -> usize {
    match value {
        Value::String(text) if text.len() > keep => {
            let mut end = keep;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            let dropped = text.len() - end;
            text.truncate(end);
            text.push_str(&format!("…[+{dropped} bytes]"));
            1
        }
        Value::Array(items) => items.iter_mut().map(|v| clip_strings(v, keep)).sum(),
        Value::Object(map) => map.values_mut().map(|v| clip_strings(v, keep)).sum(),
        _ => 0,
    }
}

fn longest_string(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(longest_string).max().unwrap_or(0),
        Value::Object(map) => map.values().map(longest_string).max().unwrap_or(0),
        _ => 0,
    }
}

/// The largest `n` in `lo..=hi` for which `fits(n)` holds, given that it holds for `lo` and
/// grows false as `n` grows.
fn largest_fitting(mut lo: usize, mut hi: usize, fits: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Serialize a tool result within the output budget (RenderLimits).
///
/// A structured result stays ONE valid JSON value however hard it is cut, so a caller that
/// parses `content[0].text` gets what fit and is told the rest was dropped. The cut used to
/// trail the JSON as a prose note, which made that parse throw - and callers do write a parse
/// whose fallback is an empty list (one read a present mysql row as missing on 2026-10-21, over
/// a different slip), so the largest answers would read as "no rows", indistinguishable from a
/// real empty result. Now the account rides inside the value (annotate): `truncated: true` and
/// an `outputBudget` saying what was shown of how much and how to narrow the request.
/// `rowCount` still counts the rows beside it; `outputBudget.total` is the count before the cut.
///
/// In order, each step only when the one before was not enough: the list is cut to
/// `max_items`; then to the most items that fit `max_bytes` (a binary search, so a reply is
/// never halved below what fits); then every string over a length is clipped - one wide row (a
/// TEXT column, a big redis value) keeps its other fields; and a value with nothing left to give
/// is carried as a `partial` string of its first bytes. A plain-text result (`Value::String`:
/// vision prose, a redis string reply) is text, not JSON, and is cut as text.
pub fn render_result(result: &Value, limits: RenderLimits) -> String {
    if let Value::String(text) = result {
        if byte_len(text) <= limits.max_bytes {
            return text.clone();
        }
        return format!(
            "{}\n\n[truncated at {} bytes]",
            clip_bytes(text, limits.max_bytes),
            limits.max_bytes
        );
    }

    let compact = |value: &Value| serde_json::to_string(value).unwrap_or_default();
    let fits = |text: &str| byte_len(text) <= limits.max_bytes;
    let list = list_of(result);
    let total = list.as_ref().map_or(0, |(items, _)| items.len());
    let whole = compact(result);
    if fits(&whole) && total <= limits.max_items {
        return whole;
    }

    let mut cut = Cut::default();
    let mut value = result.clone();
    if let Some((items, host)) = &list {
        let cap = total.min(limits.max_items);
        let shed = |n: usize| rewrap(host, items[..n].to_vec());
        let render = |n: usize| {
            let cut = Cut { items: Some((n, total)), ..Cut::default() };
            compact(&annotate(shed(n), limits, &cut))
        };
        // One item always stays: a row too wide to fit is clipped below, not dropped.
        let shown = if cap == 0 || fits(&render(cap)) {
            cap
        } else {
            largest_fitting(1, cap, |n| fits(&render(n)))
        };
        if shown < total {
            cut.items = Some((shown, total));
        }
        value = shed(shown);
    }

    let mut text = compact(&annotate(value.clone(), limits, &cut));
    if fits(&text) {
        return text;
    }

    let longest = longest_string(&value);
    if longest > CLIP_FLOOR {
        let clipped = |keep: usize| {
            let mut v = value.clone();
            let count = clip_strings(&mut v, keep);
            (v, count)
        };
        let render = |keep: usize| {
            let (v, count) = clipped(keep);
            let cut = Cut { clipped: Some((count, keep)), items: cut.items, ..Cut::default() };
            compact(&annotate(v, limits, &cut))
        };
        if fits(&render(CLIP_FLOOR)) {
            let keep = largest_fitting(CLIP_FLOOR, longest - 1, |keep| fits(&render(keep)));
            let (v, count) = clipped(keep);
            cut.clipped = Some((count, keep));
            return compact(&annotate(v, limits, &cut));
        }
    }

    // Nothing left to give (a deep or wide value with no top-level list): its first bytes, as a
    // string, so the reply still parses. Escaping can grow the text, hence the shrinking loop.
    cut.partial = true;
    let whole = compact(&value);
    let mut room = limits.max_bytes;
    loop {
        let mut out = Map::new();
        out.insert("truncated".into(), json!(true));
        out.insert("outputBudget".into(), budget_account(limits, &cut));
        out.insert("partial".into(), json!(clip_bytes(&whole, room)));
        text = compact(&Value::Object(out));
        if fits(&text) || room == 0 {
            return text;
        }
        room = room.saturating_sub(byte_len(&text) - limits.max_bytes).min(room / 2 + room / 4);
    }
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
    /// Drop pooled sockets after the tunnel they rode came back, so the next call dials
    /// the new session. False for an engine that has no pool to drop (a child process
    /// stays up). The default is false.
    async fn release_pool(&self) -> bool {
        false
    }
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

/// The strict door, for a schema that declares `additionalProperties: false`: an argument the
/// tool does not take is refused naming it, so a misspelt optional (`limt` for `limit`) fails
/// here instead of being dropped while the call quietly runs with the default. A schema without
/// that declaration (a REST def's, where a template may read any argument) stays permissive.
fn stray_argument(tool: &str, schema: &Value, args: &Value) -> Option<String> {
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return None;
    }
    let declared = schema.get("properties").and_then(Value::as_object);
    let takes = |key: &str| declared.is_some_and(|d| d.contains_key(key));
    let stray = args.as_object()?.keys().find(|k| !takes(k))?;
    let known: Vec<&str> = declared
        .map(|d| d.keys().map(String::as_str).collect())
        .unwrap_or_default();
    Some(if known.is_empty() {
        format!("{tool} has no {stray:?} argument; it takes none")
    } else {
        format!("{tool} has no {stray:?} argument; it takes: {}", known.join(", "))
    })
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
    fn get_info(&self) -> ServerConfig {
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
        server_info.name = "swiss-direct".into();
        server_info.version = "1.0".into();
        let mut info = ServerConfig::new(capabilities).with_server_info(server_info);
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
                    // The advertised list is the contract. A tool hidden from tools/list —
                    // anything the operator disabled — must not be summonable back by naming it
                    // directly. No tool ran, so this is the one protocol error (-32602, below).
                    let Some(def) = advertised.iter().find(|t| t.name == tool) else {
                        return Err(format!("unknown tool: {tool}"));
                    };
                    if let Some(refusal) = stray_argument(&tool, &def.input_schema, &args) {
                        return Ok(CallToolResult::error(vec![ContentBlock::text(refusal)]));
                    }
                    // A tool that ran and failed answers in-band (isError: true), as the spec
                    // asks: the model reads the reason as the tool's output and corrects its
                    // next call, where a JSON-RPC error reads as a broken server.
                    Ok::<_, String>(match engine.call(&tool, &args).await {
                        Ok(result) => CallToolResult::success(vec![ContentBlock::text(
                            render_result(&result, limits),
                        )]),
                        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
                    })
                },
                |result: &CallToolResult| {
                    let text = result
                        .content
                        .iter()
                        .map(|c| match c {
                            ContentBlock::Text(t) => t.text.clone(),
                            other => format!("[{other:?} content]"),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (result.is_error != Some(true), text)
                },
            )
            .await
            .map(CallToolResponse::Complete)
            .map_err(|message| ErrorData::invalid_params(message, None))
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
    fn small_results_stay_compact_and_complete() {
        let out = render_result(
            &json!({ "rowCount": 1, "rows": [{ "id": 1 }] }),
            DEFAULT_LIMITS,
        );
        assert_eq!(out, r#"{"rowCount":1,"rows":[{"id":1}]}"#);
    }

    #[test]
    fn string_results_pass_through_under_budget() {
        assert_eq!(render_result(&json!("hello"), DEFAULT_LIMITS), "hello");
        let long = "x".repeat(DEFAULT_LIMITS.max_bytes + 100);
        let out = render_result(&json!(long), DEFAULT_LIMITS);
        assert!(out.contains("[truncated at 262144 bytes]"));
    }

    #[test]
    fn an_empty_result_says_so_explicitly() {
        let out = render_result(&json!({ "rowCount": 0, "rows": [] }), DEFAULT_LIMITS);
        assert_eq!(out, r#"{"rowCount":0,"rows":[]}"#);
    }

    /// The reply a caller parses: past the budget it is still one JSON value, so
    /// `JSON.parse(content[0].text)` yields the rows that fit and says the rest went - never a
    /// throw that a fallback turns into "no rows" (the trailing prose note did exactly that).
    #[test]
    fn a_cut_reply_still_parses_and_says_what_went() {
        let rows: Vec<Value> = (0..1500).map(|i| json!({ "n": i })).collect();
        let out = render_result(&json!({ "rowCount": 1500, "rows": rows }), DEFAULT_LIMITS);
        let reply: Value = serde_json::from_str(&out).expect("a cut reply is valid JSON");
        assert_eq!(reply["rows"].as_array().map(Vec::len), Some(1000));
        assert_eq!(reply["rowCount"], json!(1000), "rowCount counts the rows beside it");
        assert_eq!(reply["truncated"], json!(true));
        assert_eq!(reply["outputBudget"]["shown"], json!(1000));
        assert_eq!(reply["outputBudget"]["total"], json!(1500));
        let note = reply["outputBudget"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("showing the first 1000 of 1500 items"), "{note}");
        assert!(note.contains("Narrow the request"), "{note}");
    }

    #[test]
    fn byte_budget_keeps_the_most_rows_that_fit() {
        // 1000 items is under max_items but far over max_bytes.
        let rows: Vec<Value> = (0..1000)
            .map(|i| json!({ "n": i, "pad": "0123456789abcdefghij".repeat(20) }))
            .collect();
        let row_len = serde_json::to_string(&rows[999]).unwrap_or_default().len();
        let mut limits = DEFAULT_LIMITS;
        limits.max_bytes = 8 * 1024;
        let out = render_result(&json!({ "rowCount": 1000, "rows": rows }), limits);
        assert!(out.len() <= limits.max_bytes, "output {}", out.len());
        let reply: Value = serde_json::from_str(&out).expect("valid JSON");
        let shown = reply["rows"].as_array().map_or(0, Vec::len);
        assert_eq!(reply["outputBudget"]["shown"], json!(shown));
        assert_eq!(reply["outputBudget"]["total"], json!(1000));
        // Halving stopped at 31 rows here; one more row than shown must not fit.
        assert!(out.len() + row_len + 1 > limits.max_bytes, "{shown} rows, {} bytes", out.len());
    }

    #[test]
    fn bare_arrays_shed_too() {
        let items: Vec<Value> = (0..1200).map(|i| json!(i)).collect();
        let out = render_result(&json!(items), DEFAULT_LIMITS);
        let reply: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(reply["items"].as_array().map(Vec::len), Some(1000));
        assert_eq!(reply["truncated"], json!(true));
        let note = reply["outputBudget"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("showing the first 1000 of 1200 items"), "{note}");
    }

    /// One row wider than the whole budget (a TEXT column, a big redis string) keeps its row and
    /// its other fields; only the long string is cut, and says how much went.
    #[test]
    fn one_wide_row_is_clipped_not_dropped() {
        let body = "x".repeat(400 * 1024);
        let result = json!({ "rowCount": 1, "rows": [{ "id": 7, "body": body, "tag": "kept" }] });
        let out = render_result(&result, DEFAULT_LIMITS);
        assert!(out.len() <= DEFAULT_LIMITS.max_bytes, "output {}", out.len());
        let reply: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(reply["rows"][0]["id"], json!(7));
        assert_eq!(reply["rows"][0]["tag"], json!("kept"));
        let cut = reply["rows"][0]["body"].as_str().unwrap_or_default();
        assert!(cut.ends_with(" bytes]") && cut.contains("…[+"), "marked: …{}", &cut[cut.len().saturating_sub(24)..]);
        assert_eq!(reply["outputBudget"]["clippedStrings"], json!(1));
        assert!(reply["outputBudget"].get("total").is_none(), "no row was dropped");
    }

    /// No list to shed and no long string to clip: the start of the JSON, carried as a string so
    /// the reply still parses.
    #[test]
    fn a_value_with_nothing_to_drop_is_carried_as_partial_text() {
        let wide: Map<String, Value> = (0..40_000).map(|i| (format!("f{i}"), json!(i))).collect();
        let out = render_result(&json!({ "value": wide }), DEFAULT_LIMITS);
        assert!(out.len() <= DEFAULT_LIMITS.max_bytes, "output {}", out.len());
        let reply: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(reply["truncated"], json!(true));
        let partial = reply["partial"].as_str().unwrap_or_default();
        assert!(partial.starts_with(r#"{"value":{"f0":0,"#), "{}", &partial[..40.min(partial.len())]);
        let note = reply["outputBudget"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("`partial`"), "{note}");
    }

    /// Four tools: one that answers behind a strict schema, one behind a permissive schema, one
    /// whose call fails, one the operator turned off.
    struct Probe {
        name: String,
    }

    #[async_trait]
    impl Engine for Probe {
        fn kind(&self) -> &'static str {
            "probe"
        }
        fn tools(&self) -> Vec<ToolDef> {
            vec![
                ToolDef {
                    name: "probe_ok".into(),
                    description: "answers".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "n": { "type": "integer" }, "tag": { "type": "string" } },
                        "additionalProperties": false,
                    }),
                },
                ToolDef {
                    name: "probe_open".into(),
                    description: "answers, any arguments".into(),
                    input_schema: json!({ "type": "object", "properties": { "n": { "type": "integer" } } }),
                },
                ToolDef {
                    name: "probe_fail".into(),
                    description: "fails".into(),
                    input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                },
                ToolDef {
                    name: "probe_off".into(),
                    description: "disabled".into(),
                    input_schema: json!({ "type": "object" }),
                },
            ]
        }
        async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
            match tool {
                "probe_fail" => Err("connection refused".into()),
                _ => Ok(json!({ "tool": tool, "args": args })),
            }
        }
        fn meta(&self) -> ServerMeta {
            ServerMeta {
                name: Some(self.name.clone()),
                description: None,
                target: None,
                limits: None,
            }
        }
    }

    async fn session(
        mcp: &str,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, crate::introspect::GatewayIntrospectClient>
    {
        let server = ToolServer {
            engine: Arc::new(Probe { name: mcp.into() }),
            disabled_tools: ["probe_off".to_string()].into_iter().collect(),
            resources_on: false,
            source: CallSource::default(),
            log: crate::calls::test_log(),
        };
        crate::introspect::open_session(server).await.expect("a session opens")
    }

    async fn call(
        client: &rmcp::service::RunningService<rmcp::RoleClient, crate::introspect::GatewayIntrospectClient>,
        tool: &str,
        args: Value,
    ) -> Result<CallToolResult, rmcp::ServiceError> {
        let mut params = CallToolRequestParams::default();
        params.name = tool.to_string().into();
        params.arguments = args.as_object().cloned();
        client.call_tool(params).await
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_failed_call_answers_in_band_and_logs_as_a_failure() {
        let client = session("ts-fail").await;
        let out = call(&client, "probe_fail", json!({})).await.expect("an answer, not a fault");
        assert_eq!(out.is_error, Some(true));
        assert_eq!(text(&out), "connection refused");

        let ok = call(&client, "probe_ok", json!({ "n": 1 })).await.expect("an answer");
        assert_ne!(ok.is_error, Some(true));

        let page = crate::calls::test_log().read_calls("ts-fail", 0, 10, None).await;
        let oks: Vec<(String, bool)> = page["calls"]
            .as_array()
            .expect("a page")
            .iter()
            .map(|c| (c["tool"].as_str().unwrap_or("").to_string(), c["ok"] == json!(true)))
            .collect();
        assert!(oks.contains(&("probe_fail".into(), false)), "{oks:?}");
        assert!(oks.contains(&("probe_ok".into(), true)), "{oks:?}");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn an_unknown_or_disabled_tool_is_an_invalid_params_error() {
        let client = session("ts-unknown").await;
        for tool in ["probe_nope", "probe_off"] {
            match call(&client, tool, json!({})).await {
                Err(rmcp::ServiceError::McpError(e)) => {
                    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{tool}");
                    assert!(e.message.contains(tool), "{tool}: {}", e.message);
                }
                other => panic!("{tool}: expected -32602, got {other:?}"),
            }
        }
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn a_strict_schema_refuses_an_argument_it_does_not_declare() {
        let client = session("ts-strict").await;
        let out = call(&client, "probe_ok", json!({ "n": 1, "limt": 5 })).await.expect("an answer");
        assert_eq!(out.is_error, Some(true));
        assert_eq!(text(&out), r#"probe_ok has no "limt" argument; it takes: n, tag"#);

        let out = call(&client, "probe_fail", json!({ "x": 1 })).await.expect("an answer");
        assert_eq!(text(&out), r#"probe_fail has no "x" argument; it takes none"#);

        // No declaration, no door: the permissive schema passes what it is given.
        let out = call(&client, "probe_open", json!({ "n": 1, "extra": true })).await.expect("an answer");
        assert_ne!(out.is_error, Some(true), "{}", text(&out));
        assert!(text(&out).contains("extra"));
        let _ = client.cancel().await;
    }
}
