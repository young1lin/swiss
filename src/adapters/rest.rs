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

use crate::config::ServerDef;

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
fn encode_uri_component(text: &str) -> String {
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
fn form_encode(text: &str) -> String {
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
fn form_decode(text: &str) -> String {
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
            // Keys are normalized through the same decode/encode round trip the values take.
            let decoded_key = form_decode(key);
            pairs.retain(|(existing_key, _)| *existing_key != decoded_key);
            pairs.push((decoded_key, form_encode(&js_string(value))));
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
