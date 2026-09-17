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

//! The config-declared REST adapter — port of `adapters/rest.ts` (the adapter) and
//! `adapters/rest-template.ts` (its declaration language).
//!
//! Turns a plain HTTP API into MCP tools from config alone — the companion to `http`, which
//! needs the far end to already speak MCP. It exists because a plain HTTP API is the common
//! case: most do not speak MCP at all, and even the ones that do often expose more over REST
//! than their MCP tool surfaces. Declaring the REST endpoint here trades "somebody else
//! maintains the schema" for control over the request, without writing an adapter.
//!
//! The design rule behind the template language is that **the request you write is the request
//! that gets sent**: a tool's `request` block is the vendor's own example, copied out of their
//! docs, with the values you want the model to control swapped for `{{arg}}`. Everything else
//! stays a literal. `{{arg}}` and NOT `${arg}`: `${VAR}` already means "environment variable"
//! everywhere in this config, resolved before an adapter ever sees the definition — two
//! syntaxes that look identical and resolve from different places is a trap.
//!
//! Scanners are hand-rolled (ADR-007: no regex) — see `src/adapters/sql.rs` for the house style.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use swiss_host::config::ServerDef;

use super::direct::{BoxFut, Lazy};
use super::proxy::{assert_proxy_url, js_truthy, proxied_client};
use super::tool_server::{Engine, ServerMeta, ToolDef};

/// How much of a failed response body is quoted back in the error. Enough to carry the API's
/// own error code and message, short enough not to become the tool result.
const ERROR_BODY_MAX: usize = 600;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

// --- the declaration language (rest-template.ts) ----------------------------------------------

/// One `{{name}}` occurrence: `(byte start, byte end, argument name)`.
///
/// The equivalent of Node's ANY_REF regex, scanned by hand: `{{`, optional whitespace, an
/// identifier, optional whitespace, `}}`. Braces that do not complete the pattern stay literal
/// text, exactly as a regex that fails to match at a position moves on — and on failure the
/// scan advances ONE byte, so `{{{a}}}` still finds the inner reference.
fn scan_refs(text: &str) -> Vec<(usize, usize, String)> {
    fn js_space(b: u8) -> bool {
        // JS `\s` also covers exotic Unicode spaces; none of them can appear inside the ASCII
        // delimiters of a reference anybody wrote on purpose.
        matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
    }
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'{' || bytes[i + 1] != b'{' {
            i += 1;
            continue;
        }
        let mut p = i + 2;
        while p < bytes.len() && js_space(bytes[p]) {
            p += 1;
        }
        if p >= bytes.len() || !(bytes[p].is_ascii_alphabetic() || bytes[p] == b'_') {
            i += 1;
            continue;
        }
        let name_start = p;
        while p < bytes.len() && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_') {
            p += 1;
        }
        let name_end = p;
        while p < bytes.len() && js_space(bytes[p]) {
            p += 1;
        }
        if p + 1 < bytes.len() && bytes[p] == b'}' && bytes[p + 1] == b'}' {
            out.push((i, p + 2, text[name_start..name_end].to_string()));
            i = p + 2;
        } else {
            i += 1;
        }
    }
    out
}

/// `{{name}}` occupying the whole string — the case that substitutes a typed value rather than
/// text (Node's WHOLE_REF, anchored at both ends).
fn whole_ref(text: &str) -> Option<String> {
    match scan_refs(text).as_slice() {
        [(0, end, name)] if *end == text.len() => Some(name.clone()),
        _ => None,
    }
}

/// `String(v)` for a JSON value, the way JavaScript coerces: numbers and booleans render as
/// themselves, `null` as the literal `null`, arrays comma-join, objects are `[object Object]`.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `encodeURIComponent` — the exact unreserved set JS leaves alone; everything else is
/// percent-encoded as UTF-8 with uppercase hex, so a `/` in an argument stays inside its own
/// path segment instead of inventing a new one.
///
/// `pub(crate)`: oauth.rs builds authorization URLs and form bodies with the same two encoders
/// — one definition of "how this gateway encodes" beats two that can drift.
pub(crate) fn encode_uri_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Compile the compact `input` declaration into the JSON Schema a client sees in tools/list.
///
/// A default is stated in prose rather than as JSON Schema's `default`, which clients render
/// inconsistently and models routinely ignore.
pub fn compile_input(input: Option<&Value>) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<String> = Vec::new();
    if let Some(Value::Object(decls)) = input {
        for (name, decl) in decls {
            let mut prop = Map::new();
            if let Some(t) = decl.get("type") {
                prop.insert("type".into(), t.clone());
            }
            let description: Option<String> = match decl.get("default") {
                Some(default) => {
                    let suffix = format!(
                        "Defaults to {}.",
                        serde_json::to_string(default).unwrap_or_default()
                    );
                    Some(
                        match decl
                            .get("description")
                            .and_then(Value::as_str)
                            .filter(|d| !d.is_empty())
                        {
                            Some(text) => format!("{text} {suffix}"),
                            None => suffix,
                        },
                    )
                }
                None => decl
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
            };
            if let Some(description) = description {
                prop.insert("description".into(), Value::String(description));
            }
            if decl.get("enum").map(js_truthy).unwrap_or(false) {
                prop.insert(
                    "enum".into(),
                    decl.get("enum").expect("just checked").clone(),
                );
            }
            properties.insert(name.clone(), Value::Object(prop));
            if decl.get("required").map(js_truthy).unwrap_or(false) {
                required.push(name.clone());
            }
        }
    }
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), json!(required));
    }
    Value::Object(schema)
}

/// Fill in declared defaults and refuse a call missing a required argument.
///
/// The check lives here rather than in the schema alone: a schema is advice to the model, and a
/// missing required value would otherwise reach the API as a dropped key and come back as
/// somebody else's confusing 400. Only an ABSENT key is missing — a caller-sent `null` is a
/// value, exactly as `=== undefined` reads in the Node build.
pub fn apply_defaults(
    input: Option<&Value>,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut out = args.clone();
    let mut missing: Vec<String> = Vec::new();
    if let Some(Value::Object(decls)) = input {
        for (name, decl) in decls {
            if !out.contains_key(name) {
                if let Some(default) = decl.get("default") {
                    out.insert(name.clone(), default.clone());
                }
            }
            if !out.contains_key(name) && decl.get("required").map(js_truthy).unwrap_or(false) {
                missing.push(name.clone());
            }
        }
    }
    if !missing.is_empty() {
        let plural = if missing.len() > 1 { "s" } else { "" };
        return Err(format!(
            "missing required argument{plural}: {}",
            missing.join(", ")
        ));
    }
    Ok(out)
}

/// Render a request template against the call's arguments.
///
/// Returns `None` for a value that has nothing behind it — the caller drops that key (or that
/// array element) rather than sending a null. Sending `null` for an omitted optional is how
/// you get a validation error out of an API that would have been perfectly happy with the
/// field absent.
pub fn render_template(node: &Value, args: &Map<String, Value>) -> Option<Value> {
    match node {
        Value::String(text) => render_string(text, args),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if let Some(rendered) = render_template(item, args) {
                    out.push(rendered);
                }
            }
            Some(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, value) in map {
                if let Some(rendered) = render_template(value, args) {
                    out.insert(key.clone(), rendered);
                }
            }
            Some(Value::Object(out))
        }
        // number, boolean, null — a literal.
        other => Some(other.clone()),
    }
}

/// A string template: a whole-string reference substitutes the argument's own typed value;
/// references embedded in text interpolate; one missing reference anywhere makes the whole
/// string dropped.
fn render_string(text: &str, args: &Map<String, Value>) -> Option<Value> {
    if let Some(name) = whole_ref(text) {
        return args.get(&name).cloned(); // absent -> None, the drop signal
    }
    let refs = scan_refs(text);
    if refs.is_empty() {
        return Some(Value::String(text.to_string()));
    }
    let mut out = String::with_capacity(text.len());
    let mut missing = false;
    let mut last = 0;
    for (start, end, name) in refs {
        out.push_str(&text[last..start]);
        match args.get(&name) {
            Some(value) => out.push_str(&js_string(value)),
            None => missing = true, // Node replaced with "" and discarded the result either way
        }
        last = end;
    }
    out.push_str(&text[last..]);
    if missing {
        None
    } else {
        Some(Value::String(out))
    }
}

/// Interpolate a request path. Unlike a body key, a path segment cannot be dropped —
/// `/repos//x` is a different endpoint — so an unfilled reference is an error.
pub fn render_path(path: &str, args: &Map<String, Value>) -> Result<String, String> {
    let refs = scan_refs(path);
    if refs.is_empty() {
        return Ok(path.to_string());
    }
    let mut out = String::with_capacity(path.len());
    let mut missing: Vec<&str> = Vec::new();
    let mut last = 0;
    for (start, end, name) in &refs {
        out.push_str(&path[last..*start]);
        match args.get(name.as_str()) {
            Some(value) => out.push_str(&encode_uri_component(&js_string(value))),
            None => missing.push(name.as_str()),
        }
        last = *end;
    }
    out.push_str(&path[last..]);
    if !missing.is_empty() {
        return Err(format!("path {path} needs {}", missing.join(", ")));
    }
    Ok(out)
}

// --- URL and query building --------------------------------------------------------------------

/// The application/x-www-form-urlencoded serializer set (what `URLSearchParams` emits): the URL
/// code points plus `*` and `~`, with space as `+`, everything else percent-encoded uppercase.
pub(crate) fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The matching decode, for folding a query string that came in on the config's path back
/// into pairs before re-serializing — a round trip `searchParams` performs on every set().
pub(crate) fn form_decode(text: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if bytes.get(i + 1).and_then(|b| hex(*b)).is_some()
                && bytes.get(i + 2).and_then(|b| hex(*b)).is_some() =>
            {
                let high = hex(bytes[i + 1]).expect("guarded");
                let low = hex(bytes[i + 2]).expect("guarded");
                out.push(high * 16 + low);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the request URL the way the Node build did: `new URL(target + path)` with each
/// rendered query value set through `searchParams.set` — a parameter with nothing behind it is
/// never added, and an existing pair of the same name is replaced rather than duplicated.
fn build_url(target: &str, path: &str, query: Option<&Value>) -> String {
    let had_query = target.contains('?') || path.contains('?');
    let full = format!("{target}{path}");
    let (base, existing) = match full.split_once('?') {
        Some((base, query)) => (base.to_string(), query.to_string()),
        None => (full, String::new()),
    };
    let mut pairs: Vec<(String, String)> = existing
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (form_decode(key), form_decode(value)),
            None => (form_decode(pair), String::new()),
        })
        .collect();
    if let Some(Value::Object(query)) = query {
        for (key, value) in query {
            // Both halves go in DECODED, like the pairs parsed out of the URL above — the single
            // encode happens on serialization below. Encoding here as well sent the API `a%2Bb`
            // where the tool was told `a b`.
            let decoded_key = form_decode(key);
            pairs.retain(|(existing_key, _)| *existing_key != decoded_key);
            pairs.push((decoded_key, js_string(value)));
        }
    }
    if pairs.is_empty() && !had_query {
        return base;
    }
    let serialized = pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{serialized}")
}

/// The path portion of a built URL — what the Node build's `url.pathname` reported in errors.
fn url_path_name(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

// --- the engine (rest.ts) ----------------------------------------------------------------------

/// One tool, declared in config. `request` is the vendor's own example with `{{arg}}` in the
/// slots.
struct ToolDecl {
    name: String,
    description: String,
    /// The compact `input` declaration, as written (rendered into a schema at tools() time).
    input: Option<Value>,
    method: Option<String>,
    path: Option<String>,
    query: Option<Value>,
    /// Sent as JSON when declared. Omit it for GET.
    body: Option<Value>,
    /// Top-level response keys worth keeping. Empty = return the whole response.
    pick: Vec<String>,
}

fn parse_tools(def: &ServerDef) -> Result<Vec<ToolDecl>, String> {
    let bad = || "a rest MCP needs a `tools` array".to_string();
    let Value::Array(items) = def.get("tools").ok_or_else(bad)? else {
        return Err(bad());
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, tool) in items.iter().enumerate() {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("rest tool #{i} has no name"))?;
        let request = tool
            .get("request")
            .filter(|request| js_truthy(request))
            .ok_or_else(|| format!("rest tool {name} has no request"))?;
        let string_field = |key: &str| request.get(key).and_then(Value::as_str).map(str::to_string);
        out.push(ToolDecl {
            name: name.to_string(),
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // `input` and `pick` sit beside `request` on the tool, not inside it.
            input: tool.get("input").filter(|input| input.is_object()).cloned(),
            method: string_field("method"),
            path: string_field("path"),
            query: request.get("query").cloned(),
            body: request.get("body").cloned(),
            pick: tool
                .get("pick")
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

/// The REST engine a [`super::direct::DirectAdapter`] wraps: nothing to connect — every call
/// is its own request — a fixed tool set compiled from the def, and one shared HTTP client
/// behind a `Lazy` (proxied when the def names a proxy).
pub struct RestEngine {
    def: ServerDef,
    /// The MCP's registry name, followed across renames — the call log is filed under the
    /// engine's view of this cell, read fresh at call time.
    name: Arc<RwLock<String>>,
    decls: Vec<ToolDecl>,
    by_name: HashMap<String, usize>,
    /// `String(def.baseUrl ?? "")` — shown in `instructions` when non-empty.
    target: String,
    /// Def-sourced headers, sent on every request in declaration order.
    headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>,
    timeout_ms: u64,
    client: Arc<Lazy<reqwest::Client>>,
}

impl RestEngine {
    /// `def` must be the resolve_def() clone make_adapter hands over. Tool declarations and
    /// the proxy URL are validated at construction (a bad one is a start error).
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let decls = parse_tools(def)?;
        let by_name = decls
            .iter()
            .enumerate()
            .map(|(i, decl)| (decl.name.clone(), i))
            .collect();
        let mut headers = Vec::new();
        if let Some(Value::Object(map)) = def.get("headers") {
            for (key, value) in map {
                let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|err| format!("invalid header name {key:?}: {err}"))?;
                let value = reqwest::header::HeaderValue::from_str(&header_value_string(value))
                    .map_err(|err| format!("invalid header value for {key:?}: {err}"))?;
                headers.push((name, value));
            }
        }
        // Validated here, used per request in call() — a bad proxy is a start error.
        let proxy_url = def
            .get_str("proxy")
            .filter(|p| !p.is_empty())
            .map(assert_proxy_url)
            .transpose()?;
        // `Number(def.timeoutMs)` finite-and-positive, else the 30s default.
        let timeout_ms = match def.get("timeoutMs") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.parse::<f64>().ok(),
            Some(Value::Bool(true)) => Some(1.0),
            _ => None,
        }
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v as u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
        let client = Arc::new(Lazy::new(move || {
            let proxy_url = proxy_url.clone();
            Box::pin(async move {
                match &proxy_url {
                    // One shared pool per proxy URL across the whole gateway (proxy-fetch.ts);
                    // a plain client per adapter otherwise.
                    Some(url) => proxied_client(url),
                    None => reqwest::Client::builder()
                        .build()
                        .map_err(|err| err.to_string()),
                }
            }) as BoxFut<Result<reqwest::Client, String>>
        }));
        Ok(Self {
            target: def.get_str("baseUrl").unwrap_or("").to_string(),
            def: def.clone(),
            name: Arc::new(RwLock::new(name.to_string())),
            decls,
            by_name,
            headers,
            timeout_ms,
            client,
        })
    }
}

/// Header values arrive as JSON: strings pass through, numbers and booleans render as JSON
/// does (`{"X-Count": 5}` sends `5`).
fn header_value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[async_trait]
impl Engine for RestEngine {
    fn kind(&self) -> &'static str {
        "rest"
    }

    fn tools(&self) -> Vec<ToolDef> {
        self.decls
            .iter()
            .map(|decl| ToolDef {
                name: decl.name.clone(),
                description: decl.description.clone(),
                input_schema: compile_input(decl.input.as_ref()),
            })
            .collect()
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let index = *self
            .by_name
            .get(tool)
            .ok_or_else(|| format!("unknown tool: {tool}"))?;
        let decl = &self.decls[index];
        // Defaults and required-argument checks first: a call that cannot be rendered must not
        // become a request, least of all a billed one.
        let empty = Map::new();
        let args_map = args.as_object().unwrap_or(&empty);
        let filled = apply_defaults(decl.input.as_ref(), args_map)?;
        let method = decl
            .method
            .clone()
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let path = render_path(decl.path.as_deref().unwrap_or(""), &filled)?;
        // `renderTemplate(query ?? {}) ?? {}`: absent query is an empty object, a query that
        // renders to nothing (a bare missing reference) is the same as no query.
        let query = match &decl.query {
            Some(query) => {
                render_template(query, &filled).or_else(|| Some(Value::Object(Map::new())))
            }
            None => Some(Value::Object(Map::new())),
        };
        let url = build_url(&self.target, &path, query.as_ref());
        // A declared body that renders to nothing is dropped entirely — no body, and no
        // content-type either, exactly as Node's `undefined` body behaved.
        let body: Option<Value> = match &decl.body {
            Some(body) => render_template(body, &filled),
            None => None,
        };

        let mut headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> = vec![(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        )];
        for (name, value) in &self.headers {
            // A def header overrides a default of the same name; on the wire only one survives.
            match headers
                .iter_mut()
                .find(|(existing, _)| existing.as_str() == name.as_str())
            {
                Some(slot) => slot.1 = value.clone(),
                None => headers.push((name.clone(), value.clone())),
            }
        }
        if body.is_some() {
            let json_type = reqwest::header::HeaderValue::from_static("application/json");
            match headers
                .iter_mut()
                .find(|(existing, _)| *existing == reqwest::header::CONTENT_TYPE)
            {
                Some(slot) => slot.1 = json_type,
                None => headers.push((reqwest::header::CONTENT_TYPE, json_type)),
            }
        }

        let client = self.client.get().await?;
        let http_method = method
            .parse::<reqwest::Method>()
            .map_err(|_| format!("invalid HTTP method: {method}"))?;
        let mut request = client
            .request(http_method, &url)
            .timeout(Duration::from_millis(self.timeout_ms));
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if let Some(body) = &body {
            request = request.body(serde_json::to_string(body).map_err(|err| err.to_string())?);
        }
        let response = request.send().await.map_err(|err| err.to_string())?;
        let status = response.status();
        let text = response.text().await.map_err(|err| err.to_string())?;
        if !status.is_success() {
            // The vendor's own error code is the useful part (a 429 with a rate-limit body
            // means "slow down"), so it is quoted rather than replaced with a generic message.
            let excerpt = if text.chars().count() > ERROR_BODY_MAX {
                let head: String = text.chars().take(ERROR_BODY_MAX).collect();
                format!("{head}\u{2026}")
            } else {
                text.clone()
            };
            return Err(format!(
                "{method} {} failed: HTTP {}\n{excerpt}",
                url_path_name(&url),
                status.as_u16()
            ));
        }
        let parsed: Value = match serde_json::from_str(&text) {
            // Not JSON — hand it over as it came, bounded by the output budget upstream.
            Err(_) => return Ok(Value::String(text)),
            Ok(parsed) => parsed,
        };
        if decl.pick.is_empty() || !matches!(parsed, Value::Object(_) | Value::Array(_)) {
            return Ok(parsed);
        }
        let mut picked = Map::new();
        for key in &decl.pick {
            // Present keys are kept as they came (a JSON answer has no undefined to filter);
            // keys absent from the response stay absent from the pick.
            if let Some(value) = parsed.get(key.as_str()) {
                picked.insert(key.clone(), value.clone());
            }
        }
        Ok(Value::Object(picked))
    }

    fn meta(&self) -> ServerMeta {
        let name = self.name.read().ok().map(|g| g.clone());
        ServerMeta {
            name,
            description: self.def.get_str("description").map(str::to_string),
            // An empty baseUrl stays out of the instructions — Node's `if (meta.target)`
            // skipped falsy targets rather than saying "Connected to ;"
            target: (!self.target.is_empty()).then(|| self.target.clone()),
            limits: None,
        }
    }

    // Deliberately NO ping (the Engine default stays): a declared REST API is as metered as a
    // remote MCP, and the registry's 15s health probe would spend real money to colour a dot.
    // The absent ping makes the registry report "unknown", which is the truth.

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from the Node build's `test/rest-template.test.ts` (the declaration language) and
    //! `test/rest-adapter.test.ts` (the engine, driven against a real local HTTP server exactly
    //! as the Node suite drives it).
    use super::*;

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    fn args(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(o) => o,
            other => panic!("arguments are an object, got {other}"),
        }
    }

    // ---- compile_input ----------------------------------------------------------------

    #[test]
    fn compiles_a_compact_declaration_into_an_object_json_schema() {
        let schema = compile_input(Some(&json!({
            "query": { "type": "string", "required": true, "description": "What to search for" },
            "count": { "type": "number", "default": 10 },
            "recency": { "type": "string", "enum": ["oneDay", "noLimit"] },
        })));
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to search for" },
                    // The default is stated in prose because the model reads descriptions, not
                    // JSON Schema defaults.
                    "count": { "type": "number", "description": "Defaults to 10." },
                    "recency": { "type": "string", "enum": ["oneDay", "noLimit"] },
                },
                "required": ["query"],
            })
        );
    }

    #[test]
    fn a_declared_default_is_appended_to_an_existing_description() {
        let schema = compile_input(Some(&json!({
            "count": { "type": "number", "default": 10, "description": "How many." },
        })));
        assert_eq!(
            schema["properties"]["count"]["description"],
            json!("How many. Defaults to 10.")
        );
    }

    #[test]
    fn omits_required_entirely_when_nothing_is_required() {
        assert_eq!(
            compile_input(Some(&json!({ "q": { "type": "string" } }))),
            json!({ "type": "object", "properties": { "q": { "type": "string" } } })
        );
    }

    #[test]
    fn compiles_an_absent_declaration_into_a_no_argument_schema() {
        assert_eq!(
            compile_input(None),
            json!({ "type": "object", "properties": {} })
        );
        // A declaration that is not an object is the same as none — a schema is what a client
        // validates against, so a broken one must not reach it.
        assert_eq!(
            compile_input(Some(&json!("nope"))),
            json!({ "type": "object", "properties": {} })
        );
    }

    // ---- apply_defaults ---------------------------------------------------------------

    #[test]
    fn fills_in_a_declared_default_the_caller_omitted() {
        let out = apply_defaults(
            Some(&json!({ "count": { "type": "number", "default": 10 } })),
            &Map::new(),
        )
        .expect("no required argument");
        assert_eq!(out["count"], json!(10));
    }

    #[test]
    fn never_overrides_a_value_the_caller_passed_including_a_falsy_one() {
        let decl = json!({
            "count": { "type": "number", "default": 10 },
            "deep": { "type": "boolean", "default": true },
        });
        let out = apply_defaults(Some(&decl), &args(json!({ "count": 0, "deep": false })))
            .expect("nothing required");
        assert_eq!(out["count"], json!(0));
        assert_eq!(out["deep"], json!(false));
    }

    #[test]
    fn refuses_a_call_missing_a_required_argument_and_names_it() {
        let err = apply_defaults(
            Some(&json!({ "query": { "type": "string", "required": true } })),
            &Map::new(),
        )
        .unwrap_err();
        assert!(err.contains("query"), "{err}");

        // Several missing arguments are reported together, so a caller fixes them in one turn.
        let err = apply_defaults(
            Some(&json!({
                "a": { "type": "string", "required": true },
                "b": { "type": "string", "required": true },
            })),
            &Map::new(),
        )
        .unwrap_err();
        assert!(err.contains("arguments"), "{err}");
        assert!(err.contains("a, b"), "{err}");
    }

    #[test]
    fn an_explicit_null_is_a_value_not_a_missing_argument() {
        // Only an ABSENT key is missing, exactly as `=== undefined` reads in the Node build.
        let out = apply_defaults(
            Some(&json!({ "q": { "type": "string", "required": true, "default": "x" } })),
            &args(json!({ "q": null })),
        )
        .expect("null is a value");
        assert_eq!(out["q"], json!(null));
    }

    // ---- render_template --------------------------------------------------------------

    #[test]
    fn substitutes_a_whole_string_reference_with_the_arguments_own_type() {
        let out = render_template(
            &json!({ "count": "{{count}}", "flag": "{{flag}}" }),
            &args(json!({ "count": 20, "flag": false })),
        );
        // 20, not "20": the API is being sent the vendor's own JSON shape.
        assert_eq!(out, Some(json!({ "count": 20, "flag": false })));
    }

    #[test]
    fn interpolates_a_reference_embedded_in_surrounding_text() {
        assert_eq!(
            render_template(
                &json!({ "q": "site:{{domain}} {{term}}" }),
                &args(json!({ "domain": "a.test", "term": "x" })),
            ),
            Some(json!({ "q": "site:a.test x" }))
        );
    }

    #[test]
    fn keeps_literals_exactly_as_written() {
        let literals = json!({ "engine": "default", "intent": false, "n": 3, "nothing": null });
        assert_eq!(render_template(&literals, &Map::new()), Some(literals));
    }

    #[test]
    fn drops_a_key_whose_reference_has_no_argument_behind_it() {
        // Sending `null` for an omitted optional is how you get a validation error out of an API
        // that would have been perfectly happy with the field absent.
        let out = render_template(
            &json!({ "q": "{{term}}", "recency": "{{recency}}" }),
            &args(json!({ "term": "x" })),
        );
        assert_eq!(out, Some(json!({ "q": "x" })));
    }

    #[test]
    fn drops_a_key_when_any_reference_inside_a_longer_string_is_missing() {
        assert_eq!(
            render_template(
                &json!({ "q": "{{a}} and {{b}}" }),
                &args(json!({ "a": "x" }))
            ),
            Some(json!({}))
        );
    }

    #[test]
    fn renders_nested_objects_and_arrays_dropping_what_has_nothing_behind_it() {
        let out = render_template(
            &json!({
                "filter": { "domain": "{{domain}}", "size": "high" },
                "tags": ["fixed", "{{tag}}", "{{missing}}"],
            }),
            &args(json!({ "domain": "a.test", "tag": "t" })),
        );
        assert_eq!(
            out,
            Some(json!({
                "filter": { "domain": "a.test", "size": "high" },
                "tags": ["fixed", "t"],
            }))
        );
    }

    #[test]
    fn a_template_that_is_nothing_but_a_missing_reference_renders_to_nothing() {
        assert_eq!(render_template(&json!("{{gone}}"), &Map::new()), None);
    }

    // ---- render_path ------------------------------------------------------------------

    #[test]
    fn interpolates_path_segments_and_percent_encodes_them() {
        // A `/` in an argument stays inside its own segment instead of inventing a new one.
        assert_eq!(
            render_path(
                "/repos/{{owner}}/{{repo}}",
                &args(json!({ "owner": "a b", "repo": "c/d" }))
            ),
            Ok("/repos/a%20b/c%2Fd".to_string())
        );
    }

    #[test]
    fn refuses_a_path_with_an_unfilled_segment_rather_than_sending_it() {
        // `/repos//x` is a different endpoint, so a path reference cannot be dropped the way a
        // body key can.
        let err = render_path("/repos/{{owner}}", &Map::new()).unwrap_err();
        assert!(err.contains("owner"), "{err}");
    }

    #[test]
    fn leaves_a_path_with_no_references_alone() {
        assert_eq!(
            render_path("/web_search", &args(json!({ "a": 1 }))),
            Ok("/web_search".to_string())
        );
    }

    // ---- the hand-rolled scanners (ADR-007: no regex) ----------------------------------

    #[test]
    fn the_reference_scanner_matches_what_the_node_regex_matched() {
        let names = |t: &str| {
            scan_refs(t)
                .into_iter()
                .map(|(_, _, name)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("{{a}}"), vec!["a"]);
        assert_eq!(names("{{  spaced  }}"), vec!["spaced"]);
        assert_eq!(names("{{_under1}}"), vec!["_under1"]);
        // On a failed match the scan advances one byte, so the inner reference is still found.
        assert_eq!(names("{{{a}}}"), vec!["a"]);
        // Braces that do not complete the pattern stay literal text.
        assert_eq!(names("{{1bad}}"), Vec::<String>::new());
        assert_eq!(names("{{unclosed"), Vec::<String>::new());
        assert_eq!(names("{ {a} }"), Vec::<String>::new());
        assert_eq!(names("{{a}} and {{b}}"), vec!["a", "b"]);
    }

    #[test]
    fn only_a_reference_filling_the_whole_string_substitutes_a_typed_value() {
        assert_eq!(whole_ref("{{a}}").as_deref(), Some("a"));
        assert_eq!(whole_ref("{{ a }}").as_deref(), Some("a"));
        assert_eq!(whole_ref(" {{a}}"), None);
        assert_eq!(whole_ref("{{a}}x"), None);
        assert_eq!(whole_ref("{{a}}{{b}}"), None);
    }

    #[test]
    fn values_coerce_to_text_the_way_javascript_does() {
        assert_eq!(js_string(&json!(null)), "null");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(3)), "3");
        assert_eq!(js_string(&json!("x")), "x");
        // Array.prototype.join renders null and undefined elements as empty strings.
        assert_eq!(js_string(&json!(["a", null, 2])), "a,,2");
        assert_eq!(js_string(&json!({ "a": 1 })), "[object Object]");
    }

    #[test]
    fn percent_encoding_leaves_exactly_the_unreserved_set_alone() {
        assert_eq!(encode_uri_component("aZ09-_.!~*'()"), "aZ09-_.!~*'()");
        assert_eq!(encode_uri_component("a b/c?d&e=f"), "a%20b%2Fc%3Fd%26e%3Df");
        // UTF-8, uppercase hex, one escape per byte.
        assert_eq!(encode_uri_component("é"), "%C3%A9");
    }

    // ---- build_url --------------------------------------------------------------------

    #[test]
    fn builds_a_url_with_the_rendered_query_appended() {
        assert_eq!(
            build_url(
                "http://api.test",
                "/v1/search",
                Some(&json!({ "q": "x", "n": 2 }))
            ),
            "http://api.test/v1/search?q=x&n=2"
        );
        assert_eq!(
            build_url("http://api.test", "/ping", None),
            "http://api.test/ping"
        );
    }

    #[test]
    fn a_query_value_is_form_encoded_exactly_once() {
        // searchParams.set stores the raw value and encodes on serialize; encoding it twice sends
        // the API `a%2Bb` where it was told `a b`.
        assert_eq!(
            build_url("http://api.test", "/s", Some(&json!({ "q": "a b" }))),
            "http://api.test/s?q=a+b"
        );
        assert_eq!(
            build_url("http://api.test", "/s", Some(&json!({ "q": "a/b&c" }))),
            "http://api.test/s?q=a%2Fb%26c"
        );
    }

    #[test]
    fn a_declared_query_replaces_a_pair_of_the_same_name_rather_than_duplicating_it() {
        assert_eq!(
            build_url(
                "http://api.test",
                "/s?fixed=1&q=old",
                Some(&json!({ "q": "new" }))
            ),
            "http://api.test/s?fixed=1&q=new"
        );
    }

    #[test]
    fn a_query_parameter_with_nothing_behind_it_never_reaches_the_url() {
        // render_template dropped it; build_url must not resurrect it as an empty pair.
        let rendered = render_template(&json!({ "ref": "{{ref}}", "raw": "1" }), &args(json!({})));
        assert_eq!(
            build_url("http://api.test", "/f", rendered.as_ref()),
            "http://api.test/f?raw=1"
        );
    }

    // ---- the engine, against a real local API ------------------------------------------

    /// One request the stand-in API received.
    #[derive(Clone)]
    struct Seen {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// A stand-in for the third-party API: records what arrived and answers what the test told
    /// it to. The Node suite runs the same shape through node:http.
    struct Api {
        base_url: String,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        answer: Arc<std::sync::Mutex<(u16, String)>>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Api {
        fn answer(&self, status: u16, body: Value) {
            *self.answer.lock().unwrap() = (status, body.to_string());
        }
        fn answer_text(&self, status: u16, body: &str) {
            *self.answer.lock().unwrap() = (status, body.to_string());
        }
        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
        fn stop(self) {
            self.server.abort();
        }
    }

    async fn api() -> Api {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Seen>::new()));
        let answer = Arc::new(std::sync::Mutex::new((
            200u16,
            json!({ "ok": true }).to_string(),
        )));
        let state = (seen.clone(), answer.clone());
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (seen, answer) = state.clone();
                async move {
                    let method = req.method().to_string();
                    let target = req
                        .uri()
                        .path_and_query()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    let headers = req
                        .headers()
                        .iter()
                        .map(|(k, v)| {
                            (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())
                        })
                        .collect();
                    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
                        .await
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default();
                    seen.lock().unwrap().push(Seen {
                        method,
                        target,
                        headers,
                        body,
                    });
                    let (status, payload) = answer.lock().unwrap().clone();
                    axum::response::Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(payload))
                        .expect("a literal response")
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Api {
            base_url: format!("http://127.0.0.1:{port}"),
            seen,
            answer,
            server,
        }
    }

    /// The declared REST API this feature exists for — the vendor's own request, with the values
    /// the model controls swapped for `{{arg}}`.
    fn search_def(base_url: &str) -> Value {
        json!({
            "type": "rest",
            "description": "Example search API (REST).",
            "baseUrl": base_url,
            "headers": { "Authorization": "Bearer sk-test" },
            "tools": [{
                "name": "search",
                "description": "Search the index.",
                "input": {
                    "query": { "type": "string", "required": true, "description": "What to search for" },
                    "count": { "type": "number", "default": 10 },
                    "recency": { "type": "string", "enum": ["oneDay", "noLimit"] },
                },
                "request": {
                    "method": "POST",
                    "path": "/v1/search",
                    "body": {
                        "q": "{{query}}",
                        "engine": "default",
                        "safe": false,
                        "count": "{{count}}",
                        "recency": "{{recency}}",
                        "detail": "high",
                    },
                },
                "pick": ["results"],
            }],
        })
    }

    fn engine(d: Value) -> RestEngine {
        RestEngine::new(&def(d), "example").expect("a valid rest declaration")
    }

    #[tokio::test]
    async fn exposes_each_declared_tool_with_its_compiled_schema() {
        let remote = api().await;
        let e = engine(search_def(&remote.base_url));
        let tools = e.tools();
        assert_eq!(
            tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            vec!["search"]
        );
        assert_eq!(tools[0].description, "Search the index.");
        assert_eq!(tools[0].input_schema["required"], json!(["query"]));
        assert_eq!(
            tools[0].input_schema["properties"]["recency"]["enum"],
            json!(["oneDay", "noLimit"])
        );
        remote.stop();
    }

    #[tokio::test]
    async fn sends_the_rendered_request_with_the_declared_headers_and_the_default_filled_in() {
        let remote = api().await;
        let e = engine(search_def(&remote.base_url));
        e.call("search", &json!({ "query": "mcp gateway" }))
            .await
            .expect("the call succeeds");

        let seen = remote.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].target, "/v1/search");
        assert_eq!(seen[0].header("authorization"), Some("Bearer sk-test"));
        assert_eq!(seen[0].header("accept"), Some("application/json"));
        assert!(seen[0]
            .header("content-type")
            .unwrap_or("")
            .contains("application/json"));
        assert_eq!(
            serde_json::from_str::<Value>(&seen[0].body).expect("a JSON body"),
            json!({
                "q": "mcp gateway",
                "engine": "default",
                "safe": false,
                // the declared default, and a number rather than "10"
                "count": 10,
                "detail": "high",
                // recency is absent, not null — the caller passed no recency
            })
        );
        remote.stop();
    }

    #[tokio::test]
    async fn narrows_the_answer_to_the_picked_keys() {
        let remote = api().await;
        remote.answer(
            200,
            json!({ "id": "x", "created": 1, "request_id": "y", "results": [{ "title": "t" }] }),
        );
        let e = engine(search_def(&remote.base_url));
        assert_eq!(
            e.call("search", &json!({ "query": "q" })).await.unwrap(),
            json!({ "results": [{ "title": "t" }] })
        );
        remote.stop();
    }

    #[tokio::test]
    async fn a_picked_key_the_api_did_not_send_stays_absent() {
        let remote = api().await;
        remote.answer(200, json!({ "id": "x" }));
        let e = engine(search_def(&remote.base_url));
        assert_eq!(
            e.call("search", &json!({ "query": "q" })).await.unwrap(),
            json!({})
        );
        remote.stop();
    }

    #[tokio::test]
    async fn reports_the_apis_own_status_and_body_when_a_call_fails() {
        // The vendor's error code is the useful part: a 429 with a rate-limit body means "slow
        // down", which a generic message would throw away.
        let remote = api().await;
        remote.answer(
            429,
            json!({ "error": { "code": "rate_limited", "message": "concurrency limit" } }),
        );
        let e = engine(search_def(&remote.base_url));
        let err = e
            .call("search", &json!({ "query": "q" }))
            .await
            .unwrap_err();
        assert!(err.contains("429"), "{err}");
        assert!(err.contains("rate_limited"), "{err}");
        assert!(err.contains("/v1/search"), "{err}");
        remote.stop();
    }

    #[tokio::test]
    async fn refuses_a_call_with_no_required_argument_before_any_request_is_made() {
        // A call that cannot be rendered must not become a request, least of all a billed one.
        let remote = api().await;
        let e = engine(search_def(&remote.base_url));
        let err = e.call("search", &json!({})).await.unwrap_err();
        assert!(err.contains("query"), "{err}");
        assert!(remote.seen().is_empty(), "a request was sent anyway");
        remote.stop();
    }

    #[tokio::test]
    async fn rejects_a_tool_it_does_not_declare() {
        let remote = api().await;
        let e = engine(search_def(&remote.base_url));
        let err = e.call("nope", &json!({})).await.unwrap_err();
        assert!(err.contains("unknown tool: nope"), "{err}");
        assert!(remote.seen().is_empty());
        remote.stop();
    }

    #[tokio::test]
    async fn renders_query_parameters_and_path_segments() {
        let remote = api().await;
        let e = engine(json!({
            "type": "rest",
            "baseUrl": remote.base_url,
            "tools": [{
                "name": "read_file",
                "description": "Read a file from a repo.",
                "input": {
                    "owner": { "type": "string", "required": true },
                    "repo": { "type": "string", "required": true },
                    "ref": { "type": "string" },
                },
                "request": {
                    "method": "GET",
                    "path": "/repos/{{owner}}/{{repo}}/file",
                    "query": { "ref": "{{ref}}", "raw": "1" },
                },
            }],
        }));

        e.call(
            "read_file",
            &json!({ "owner": "an org", "repo": "a/b", "ref": "main" }),
        )
        .await
        .expect("call");
        assert_eq!(
            remote.seen()[0].target,
            "/repos/an%20org/a%2Fb/file?ref=main&raw=1"
        );

        // No ref passed, so no empty ref sent.
        e.call("read_file", &json!({ "owner": "o", "repo": "r" }))
            .await
            .expect("call");
        assert_eq!(remote.seen()[1].target, "/repos/o/r/file?raw=1");
        remote.stop();
    }

    #[tokio::test]
    async fn sends_no_body_on_a_get() {
        let remote = api().await;
        let e = engine(json!({
            "type": "rest",
            "baseUrl": remote.base_url,
            "tools": [{ "name": "ping_it", "description": "p", "request": { "method": "GET", "path": "/ping" } }],
        }));
        e.call("ping_it", &json!({})).await.expect("call");
        let seen = remote.seen();
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].body, "");
        // ...and no content-type either: there is no content.
        assert_eq!(seen[0].header("content-type"), None);
        remote.stop();
    }

    #[tokio::test]
    async fn a_response_that_is_not_json_is_handed_over_as_it_came() {
        let remote = api().await;
        remote.answer_text(200, "plain words, no braces");
        let e = engine(json!({
            "type": "rest",
            "baseUrl": remote.base_url,
            "tools": [{ "name": "t", "description": "d", "request": { "method": "GET", "path": "/x" } }],
        }));
        assert_eq!(
            e.call("t", &json!({})).await.unwrap(),
            json!("plain words, no braces")
        );
        remote.stop();
    }

    #[tokio::test]
    async fn a_def_header_overrides_the_default_of_the_same_name() {
        let remote = api().await;
        let e = engine(json!({
            "type": "rest",
            "baseUrl": remote.base_url,
            // Numbers and booleans render the way JSON does; Accept replaces the built-in default.
            "headers": { "Accept": "text/plain", "X-Count": 5 },
            "tools": [{ "name": "t", "description": "d", "request": { "method": "GET", "path": "/x" } }],
        }));
        e.call("t", &json!({})).await.expect("call");
        let seen = remote.seen();
        assert_eq!(seen[0].header("accept"), Some("text/plain"));
        assert_eq!(seen[0].header("x-count"), Some("5"));
        remote.stop();
    }

    /// The construction error for a declaration that cannot be configured. `RestEngine` is not
    /// `Debug` (it holds a live client), so the Ok arm is named rather than unwrapped.
    fn engine_err(d: Value) -> String {
        match RestEngine::new(&def(d), "x") {
            Err(err) => err,
            Ok(_) => panic!("this declaration must not build an engine"),
        }
    }

    #[test]
    fn a_declaration_that_cannot_be_configured_is_a_start_error() {
        assert!(engine_err(json!({ "type": "rest" })).contains("`tools` array"));
        assert!(engine_err(json!({ "type": "rest", "tools": {} })).contains("`tools` array"));
        assert!(
            engine_err(json!({ "type": "rest", "tools": [{ "request": {} }] })).contains("no name")
        );
        assert!(
            engine_err(json!({ "type": "rest", "tools": [{ "name": "t" }] }))
                .contains("has no request")
        );
        // A header name or value the HTTP layer cannot carry fails here, not at the first call.
        assert!(engine_err(
            json!({ "type": "rest", "tools": [], "headers": { "bad header": "v" } })
        )
        .contains("invalid header name"));
    }

    #[test]
    fn the_meta_reports_the_live_name_and_skips_an_empty_target() {
        let e = engine(json!({
            "type": "rest",
            "description": "A described API.",
            "tools": [],
        }));
        let meta = e.meta();
        assert_eq!(meta.name.as_deref(), Some("example"));
        assert_eq!(meta.description.as_deref(), Some("A described API."));
        // An empty baseUrl stays out of the instructions rather than saying "Connected to ;".
        assert_eq!(meta.target, None);

        e.rename("renamed");
        assert_eq!(e.meta().name.as_deref(), Some("renamed"));
    }

    #[test]
    fn a_rest_engine_has_no_health_probe_at_all() {
        // A declared REST API is as metered as a remote MCP: the registry's 15 s probe would
        // spend real money to colour a dot, so the absent ping makes it report "unknown".
        let e = engine(json!({ "type": "rest", "tools": [] }));
        assert!(futures_ping(&e).is_none());
    }

    /// `Engine::ping` is async; this test only cares that the default (no probe) is in force, so
    /// it drives the future to completion on a fresh runtime rather than making the test async.
    fn futures_ping(e: &RestEngine) -> Option<Result<(), String>> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a test runtime")
            .block_on(e.ping())
    }
}
