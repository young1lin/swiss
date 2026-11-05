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

//! The llm plugin's config row (SPEC §llm.config), checked whole:
//!
//! ```json
//! {
//!   "providers": { "glm": { "style": "anthropic", "baseUrl": "...", "apiKey": "${secret://glm}",
//!                           "models": [{ "id": "glm-5.3-flash", "kind": "chat" }] } },
//!   "embedding": { "provider": "ollama", "model": "bge-m3", "minSimilarity": 0.45 },
//!   "agents":    { "distiller": { "runtime": "builtin", "provider": "glm", "model": "glm-5.3-flash",
//!                                 "systemPrompt": "...", "tools": ["read", "bash", "sync"] } },
//!   "telemetry": { "protocol": "grpc", "endpoint": "http://127.0.0.1:4317" }
//! }
//! ```
//!
//! The blocks reference each other by name only - the embedding and every agent name a
//! provider and one of the models it lists - and every reference is checked here, so a row that names a provider it
//! lacks is refused at the PUT, never discovered by the first run. Every error names its
//! place (`agents.distiller.provider ...`), and none ever quotes a credential.

use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::{Map, Value};

use crate::agent::{self, AgentConfig};
use crate::embedding::{self, EmbeddingConfig};
use crate::provider::{self, ProviderEntry};
use crate::telemetry::{self, TelemetryConfig};

/// The keys a row may carry (`enabled` is the host's own switch).
const ROW_KEYS: [&str; 5] = ["enabled", "providers", "embedding", "agents", "telemetry"];

/// The most characters a provider or agent name runs to.
pub const NAME_MAX: usize = 32;

/// The checked row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmConfig {
    pub providers: BTreeMap<String, ProviderEntry>,
    /// The semantic tier's endpoint; None while the row has no embedding (or it is off).
    pub embedding: Option<EmbeddingConfig>,
    pub agents: BTreeMap<String, AgentConfig>,
    /// The task agent the row lists last: the one added most recently, since an edit keeps an
    /// agent where it stands and an add appends (the row keeps its key order). A consumer
    /// that names no agent of its own falls back to it (memory's sync, SPEC §memory.sync); a
    /// `db` agent, which serves the Data page alone, is never that fallback.
    pub newest_agent: Option<String>,
    /// The OTLP exporter; None while the row has no telemetry block.
    pub telemetry: Option<TelemetryConfig>,
}

impl LlmConfig {
    /// Check a whole plugin row.
    pub fn parse(row: &Value) -> Result<LlmConfig, String> {
        let obj = row.as_object().ok_or("the llm config must be an object")?;
        only(obj, "the llm config", &ROW_KEYS)?;
        if let Some(enabled) = obj.get("enabled") {
            if !enabled.is_boolean() {
                return Err("enabled must be a boolean".into());
            }
        }
        let providers = provider::parse_all(obj.get("providers"))?;
        let embedding = embedding::parse(obj.get("embedding"), &providers)?;
        let agents = agent::parse_all(obj.get("agents"), &providers)?;
        let newest_agent = obj
            .get("agents")
            .and_then(Value::as_object)
            .and_then(|named| named.keys().rev().find(|name| agents.get(*name).is_some_and(|a| a.runtime.serves_tasks())))
            .cloned();
        let telemetry = telemetry::parse(obj.get("telemetry"))?;
        Ok(LlmConfig { providers, embedding, agents, newest_agent, telemetry })
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderEntry> {
        self.providers.get(name)
    }

    pub fn agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name)
    }
}

// --- the field helpers every block shares ------------------------------------------------

/// The value as an object, or an error naming its place.
pub(crate) fn object<'a>(value: &'a Value, place: &str) -> Result<&'a Map<String, Value>, String> {
    value.as_object().ok_or_else(|| format!("{place} must be an object"))
}

/// Refuse a key the block does not define: accepted but not executed is a lie (SPEC §arch.rules).
pub(crate) fn only(obj: &Map<String, Value>, place: &str, keys: &[&str]) -> Result<(), String> {
    match obj.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => Err(format!("{place} has no {key:?} field")),
        None => Ok(()),
    }
}

/// A string field, trimmed; absent, null or blank reads as None.
pub(crate) fn text(obj: &Map<String, Value>, place: &str, key: &str) -> Result<Option<String>, String> {
    Ok(raw_text(obj, place, key)?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// A string field as typed (a prefix keeps its trailing space).
pub(crate) fn raw_text(obj: &Map<String, Value>, place: &str, key: &str) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{place}.{key} must be a string")),
    }
}

/// A whole number in `min..=max`.
pub(crate) fn whole(obj: &Map<String, Value>, place: &str, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{place}.{key} must be a whole number in {min}..={max}")),
        },
    }
}

pub(crate) fn flag(obj: &Map<String, Value>, place: &str, key: &str) -> Result<Option<bool>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{place}.{key} must be a boolean")),
    }
}

/// An agent's name, which the panel lists and a config references: lowercase letters, digits
/// and dashes (`[a-z0-9][a-z0-9-]{0,31}`), the rule memory's settings restate.
pub fn name_ok(name: &str) -> bool {
    let len = name.chars().count();
    (1..=NAME_MAX).contains(&len)
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A provider's name: an agent's rule with capitals too (`[A-Za-z0-9][A-Za-z0-9-]{0,31}`), so a
/// provider reads as its vendor writes itself (`GLM`, `DeepSeek`, `OpenRouter`).
pub fn provider_name_ok(name: &str) -> bool {
    let len = name.chars().count();
    (1..=NAME_MAX).contains(&len)
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// At most `max` characters and none of them a control character.
pub(crate) fn printable(value: &str, max: usize) -> bool {
    value.chars().count() <= max && !value.chars().any(char::is_control)
}

/// Like [printable], but a text of lines: newlines and tabs are its own (a prompt).
pub(crate) fn printable_lines(value: &str, max: usize) -> bool {
    value.chars().count() <= max && !value.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

/// A credential field: a `${secret://name}` or `${ENV_VAR}` reference, never the value.
pub(crate) fn reference(obj: &Map<String, Value>, place: &str, key: &str) -> Result<Option<String>, String> {
    let Some(value) = text(obj, place, key)? else { return Ok(None) };
    if !swiss_host::config::is_env_ref(&Value::String(value.clone())) {
        return Err(format!(
            "{place}.{key} must be a ${{secret://name}} or ${{ENV_VAR}} reference, never the credential itself"
        ));
    }
    Ok(Some(value))
}

/// An http(s) endpoint with a host and no credentials, query or fragment; the trailing
/// slash dropped (the wire's own path is appended).
pub(crate) fn http_url(raw: &str, place: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| format!("{place} must be an http:// or https:// URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(format!("{place} must be an http:// or https:// URL"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{place} must not carry credentials; a key goes in its own field as a reference"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{place} takes no query or fragment"));
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// An http, https or socks5 proxy with a host. Credentials only ever as `${...}`
/// references (SPEC §host.refs): they resolve HERE just to check the URL's shape (a
/// secret not in the vault yet is blanked, not fatal - the vault may gain the name
/// later), the block keeps them as typed, and the client resolves again at use. A literal
/// `user:password` is refused.
pub(crate) fn proxy_url(raw: &str, place: &str) -> Result<String, String> {
    let shape = proxy_shape(raw, place)?;
    let url = reqwest::Url::parse(&shape.text).map_err(|_| format!("{place} must be an http://, https:// or socks5:// URL"))?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") || url.host_str().is_none() {
        return Err(format!("{place} must be an http://, https:// or socks5:// URL"));
    }
    // The URL parser validates syntax; these offsets only locate the credential
    // bytes in its input. Keep their provenance through expansion: a reference can
    // supply ://, : or @, so deleting references before inspection loses the boundary.
    let special = matches!(url.scheme(), "http" | "https");
    let rest = shape.text.find(':').map(|at| at + 1).ok_or_else(|| format!("{place} has no URL scheme"))?;
    let tail = &shape.text[rest..];
    let tail = tail.trim_start_matches(|c| matches!(c, '/' | '\t' | '\r' | '\n') || special && c == '\\');
    let start = shape.text.len() - tail.len();
    let end = tail.find(|c| matches!(c, '/' | '?' | '#') || special && c == '\\').unwrap_or(tail.len());
    let authority = &tail[..end];
    let literal_in = |range: Range<usize>| !range.is_empty()
        && shape.literals.iter().any(|span| span.start < range.end && range.start < span.end);
    let literal_credentials = authority.rfind('@').is_some_and(|at| {
        let colon = authority[..at].find(':');
        literal_in(start..start + colon.unwrap_or(at))
            || colon.is_some_and(|colon| literal_in(start + colon + 1..start + at))
    });
    if literal_credentials {
        return Err(format!(
            "{place} must not carry credentials as literals; put them in a ${{secret://name}} or ${{ENV_VAR}} reference"
        ));
    }
    Ok(raw.trim().to_string())
}

/// The parsed input plus the byte ranges authored as literals, never reference values.
struct ProxyShape {
    text: String,
    literals: Vec<Range<usize>>,
}

/// Expand once while retaining origin. A missing vault entry is blank for validation:
/// the operator may add it later, and the live client still resolves strictly at use.
fn proxy_shape(raw: &str, place: &str) -> Result<ProxyShape, String> {
    let mut shape = ProxyShape { text: String::with_capacity(raw.len()), literals: Vec::new() };
    let mut rest = raw;
    loop {
        let at = rest.find("${").unwrap_or(rest.len());
        if at > 0 {
            let start = shape.text.len();
            shape.text.push_str(&rest[..at]);
            shape.literals.push(start..shape.text.len());
        }
        if at == rest.len() {
            return Ok(shape);
        }
        let reference = &rest[at..];
        let close = reference.find('}').ok_or_else(|| format!("{place} has an invalid reference"))?;
        if !swiss_host::config::is_env_ref(&Value::String(reference[..=close].to_string())) {
            return Err(format!("{place} has an invalid reference"));
        }
        let resolved = swiss_core::secure::refs::resolve_collect(&reference[..=close]).map(|(text, _)| text).unwrap_or_default();
        shape.text.push_str(&resolved);
        rest = &reference[close + 1..];
    }
}

/// Does the string carry a `${...}` reference at all?
pub(crate) fn has_refs(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while let Some(at) = bytes[i..].iter().position(|b| *b == b'$') {
        let at = i + at;
        if bytes.get(at + 1) == Some(&b'{') && bytes[at + 2..].contains(&b'}') {
            return true;
        }
        i = at + 1;
        if i >= bytes.len() {
            break;
        }
    }
    false
}

/// A reqwest client and the secrets its configured proxy actually captured.
/// References resolve once here; later masking never re-reads env or vault state.
pub(crate) fn client_with(proxy: Option<&str>, place: &str, builder: reqwest::ClientBuilder) -> Result<(reqwest::Client, Vec<String>), String> {
    let mut builder = builder;
    let mut credentials = None;
    let mut secrets = Vec::new();
    if let Some(proxy) = proxy {
        let resolved = swiss_core::secure::refs::resolve_collect(proxy).map_err(|err| format!("{place}: {err}"))?.0;
        let url = reqwest::Url::parse(&resolved).map_err(|_| format!("{place} is not a usable proxy URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            // reqwest's live proxy matcher decodes userinfo lossily, without form
            // encoding's plus substitution. Do not mask the username by itself.
            let user = percent_decode(url.username());
            let password = url.password().map(percent_decode);
            if let Some(password) = &password { if !password.is_empty() { secrets.push(password.clone()); } }
            if matches!(url.scheme(), "http" | "https") { credentials = Some((user, password)); }
        }
        let proxy = reqwest::Proxy::all(url).map_err(|_| format!("{place} is not a usable proxy URL"))?;
        builder = builder.proxy(proxy);
    }
    let client = builder.build().map_err(|err| format!("the HTTP client could not be built: {}", scrubbed_many(&err.to_string(), &secrets)))?;
    if let Some((user, password)) = credentials {
        // RequestBuilder owns the existing Basic encoder. Build only: this never
        // connects, and uses the SAME credential snapshot as this client's proxy.
        let request = client.get("http://127.0.0.1/").basic_auth(user, password).build()
            .map_err(|_| format!("{place}: the proxy credential mask could not be prepared"))?;
        let token = request.headers().get(reqwest::header::AUTHORIZATION).and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Basic ")).filter(|token| !token.is_empty())
            .ok_or_else(|| format!("{place}: the proxy credential mask could not be prepared"))?;
        secrets.push(token.to_string());
    }
    Ok((client, secrets))
}

/// Decode a URI component without treating a literal plus as a space.
/// Shared by proxy userinfo and the existing gRPC message contract.
pub(crate) fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The message with every occurrence of the key masked: some providers quote what they
/// were sent.
pub fn scrubbed(message: &str, key: &str) -> String {
    if key.is_empty() {
        return message.to_string();
    }
    message.replace(key, "***")
}

/// How much of a provider's error body an error message quotes.
const DETAIL_MAX: usize = 300;

/// A provider's error body, briefly: the OpenAI/Anthropic `error.message` when there is
/// one, otherwise the text - never the whole dump.
pub fn detail(bytes: &[u8]) -> String {
    clip(&detail_text(bytes), DETAIL_MAX)
}

/// Decode a provider's complete detail, mask the key, then take the bounded excerpt.
pub fn scrubbed_detail(bytes: &[u8], key: &str) -> String {
    clip(&scrubbed(&detail_text(bytes), key), DETAIL_MAX)
}

/// Mask every resolved header value before clipping a collector's decoded detail.
pub(crate) fn scrubbed_detail_many(bytes: &[u8], keys: &[String]) -> String {
    clip(&scrubbed_many(&detail_text(bytes), keys), DETAIL_MAX)
}

/// Mask the original text once: short keys cannot expand earlier mask markers.
pub(crate) fn scrubbed_many(message: &str, keys: &[String]) -> String {
    let mut out = String::with_capacity(message.len());
    let mut at = 0;
    while at < message.len() {
        let rest = &message[at..];
        let matched = keys.iter().filter(|key| !key.is_empty() && rest.starts_with(key.as_str())).map(String::len).max();
        if let Some(length) = matched {
            out.push_str("***");
            at += length;
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

fn detail_text(bytes: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(bytes).ok();
    let message = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error"))
            .or_else(|| v.get("message"))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    message.unwrap_or_else(|| String::from_utf8_lossy(bytes).trim().to_string())
}

/// An error and its causes as one line. reqwest's own text stops at "error sending
/// request", which names no reason; the reason (a refused connection, a TLS failure) sits
/// down the source chain. A cause its parent already quotes is said once.
pub(crate) fn causes(err: &(dyn std::error::Error + 'static)) -> String {
    let mut line = err.to_string();
    let mut next = err.source();
    while let Some(cause) = next {
        let text = cause.to_string();
        if !text.is_empty() && !line.contains(&text) {
            line.push_str(": ");
            line.push_str(&text);
        }
        next = cause.source();
    }
    line
}

/// The text's first `max` printable characters, an ellipsis when it went on.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let mut short: String = text.chars().filter(|c| !c.is_control()).take(max).collect();
    if text.chars().count() > max {
        short.push('…');
    }
    if short.is_empty() {
        "(no body)".into()
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({
            "providers": {
                "glm": { "style": "anthropic", "baseUrl": "https://open.bigmodel.cn/api/anthropic", "apiKey": "${secret://glm}",
                         "models": [{ "id": "glm-5.3-flash" }] },
                "ollama": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1",
                            "models": [{ "id": "qwen3.5:4b" }, { "id": "bge-m3", "kind": "embedding" }] }
            },
            "embedding": { "provider": "ollama", "model": "bge-m3", "minSimilarity": 0.45 },
            "agents": { "distiller": { "runtime": "builtin", "provider": "glm", "maxTurns": 12 } },
            "telemetry": { "protocol": "grpc", "endpoint": "http://127.0.0.1:4317" }
        })
    }

    #[test]
    fn a_row_parses_into_its_four_blocks() {
        let config = LlmConfig::parse(&row()).expect("row");
        assert_eq!(config.providers.keys().cloned().collect::<Vec<_>>(), vec!["glm", "ollama"]);
        let embedding = config.embedding.as_ref().expect("embedding");
        assert_eq!((embedding.provider.as_str(), embedding.model.as_str()), ("ollama", "bge-m3"));
        assert_eq!(config.agent("distiller").expect("agent").runtime, crate::agent::Runtime::Builtin);
        assert!(config.telemetry.as_ref().expect("telemetry").enabled);
        // An empty row is a valid row: nothing configured yet.
        assert_eq!(LlmConfig::parse(&json!({})).expect("empty"), LlmConfig::default());
        assert!(LlmConfig::parse(&json!({ "enabled": true })).is_ok());
    }

    #[test]
    fn the_row_refuses_what_it_does_not_define_and_dangling_names() {
        let refused = [
            (json!([]), "must be an object"),
            (json!({ "llm": {} }), "no \"llm\" field"),
            (json!({ "enabled": "yes" }), "enabled must be a boolean"),
            (json!({ "embedding": { "provider": "nope", "model": "m" } }), "embedding.provider"),
            (json!({ "agents": { "a": { "provider": "nope" } } }), "agents.a.provider"),
            // A model the provider does not list is refused where it is named.
            (json!({ "providers": { "o": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m" }] } },
                     "embedding": { "provider": "o", "model": "m" } }), "embedding.model"),
            (json!({ "providers": { "o": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m" }] } },
                     "agents": { "a": { "provider": "o", "model": "other" } } }), "agents.a.model"),
        ];
        for (value, needle) in refused {
            let err = LlmConfig::parse(&value).expect_err(&value.to_string());
            assert!(err.contains(needle), "{value}: {err}");
        }
    }

    #[test]
    fn names_urls_and_details_are_checked_once_for_every_block() {
        assert!(name_ok("glm") && name_ok("ollama-embed") && name_ok("0x"));
        assert!(!name_ok("") && !name_ok("GLM") && !name_ok("-x") && !name_ok("a b") && !name_ok(&"a".repeat(33)));
        assert!(provider_name_ok("GLM") && provider_name_ok("DeepSeek") && provider_name_ok("glm") && provider_name_ok("0x"));
        assert!(!provider_name_ok("") && !provider_name_ok("-GLM") && !provider_name_ok("Zhipu GLM") && !provider_name_ok("a_b") && !provider_name_ok(&"A".repeat(33)));
        assert_eq!(http_url("https://h/v1/", "x").expect("url"), "https://h/v1");
        assert!(http_url("ftp://h", "x").is_err());
        assert!(http_url("https://u:p@h", "x").expect_err("creds").contains("credentials"));
        assert!(http_url("https://h?a=1", "x").expect_err("query").contains("query"));
        assert!(proxy_url("socks5://alice:hunter2@127.0.0.1:1080", "p").expect_err("literal").contains("literals"));
        assert!(proxy_url("socks5://${secret://u}:${secret://p}@127.0.0.1:1080", "p").is_ok());
        assert_eq!(detail(br#"{"error":{"message":"bad model"}}"#), "bad model");
        assert_eq!(scrubbed("key sk-abcdef was wrong", "sk-abcdef"), "key *** was wrong");
        assert!(clip(&"x".repeat(400), 300).ends_with('…'));
    }

    #[test]
    fn proxy_percent_decoding_preserves_userinfo_semantics() {
        for (raw, decoded) in [
            ("literal+plus", "literal+plus"), ("encoded%2Bplus", "encoded+plus"),
            ("caf%C3%A9%E4%BD%A0", "café你"), ("lower%2bplus", "lower+plus"),
            ("bad%xy%2", "bad%xy%2"), ("lossy%fftail", "lossy�tail"), ("100%", "100%"),
        ] { assert_eq!(percent_decode(raw), decoded, "{raw}"); }
    }

    #[test]
    fn proxy_references_do_not_excuse_literal_credentials() {
        for raw in [
            "socks5://alice:hunter2@127.0.0.1:1080/${SWISS_PROXY_UNUSED}",
            "http://${SWISS_PROXY_USER}:hunter2@127.0.0.1:8080",
            "http://alice:${SWISS_PROXY_PASSWORD}@127.0.0.1:8080",
            "http://${SWISS_PROXY_USER}suffix:${SWISS_PROXY_PASSWORD}@127.0.0.1:8080",
            "http://alice:hunter2@127.0.0.1:8080?ref=${SWISS_PROXY_UNUSED}",
        ] {
            let err = proxy_url(raw, "providers.local.proxy").expect_err("literal credentials must be refused");
            assert!(err.contains("literals"), "{err}");
            assert!(!err.contains("hunter2"), "the error must not repeat credentials");
        }
    }

    #[test]
    fn proxy_references_are_validated_without_echoing_credentials() {
        for raw in ["http://${bad}:p@h", "http://${UNCLOSED", "http://${secret://}:p@h"] {
            assert!(proxy_url(raw, "p").is_err(), "malformed reference");
        }
        for raw in [
            "http://${SWISS_PROXY_USER}:${SWISS_PROXY_PASSWORD}@127.0.0.1:8080",
            "socks5h://${secret://proxy-user}:${secret://proxy-password}@127.0.0.1:1080",
            "http://127.0.0.1:8080/${SWISS_PROXY_PATH}",
        ] {
            assert!(proxy_url(raw, "p").is_ok(), "valid reference-only credentials");
        }
    }

    #[test]
    fn proxy_url_prefix_references_cannot_hide_literal_credentials() {
        let name = format!("SWISS_LLM_PROXY_PREFIX_{}", swiss_core::util::random_hex(8).to_ascii_uppercase());
        std::env::set_var(&name, "http://");
        let prefix = format!("{}{{{name}}}", '$');
        let results: Vec<_> = ["", "/http://decoy"].into_iter()
            .map(|tail| proxy_url(&format!("{prefix}alice:hunter2@127.0.0.1:8080{tail}"), "p")).collect();
        std::env::remove_var(&name);
        for result in results {
            let err = result.expect_err("a URL prefix reference cannot bless literal userinfo");
            assert!(err.contains("literals") && !err.contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn proxy_url_suffix_references_cannot_supply_credential_delimiters() {
        let name = format!("SWISS_LLM_PROXY_SUFFIX_{}", swiss_core::util::random_hex(8).to_ascii_uppercase());
        std::env::set_var(&name, "@127.0.0.1:8080");
        let raw = format!("http://alice:hunter2${{{name}}}");
        let result = proxy_url(&raw, "p");
        std::env::set_var(&name, "alice:hunter2@127.0.0.1:8080");
        let authority = proxy_url(&format!("http://${{{name}}}"), "p");
        std::env::set_var(&name, "http://alice:hunter2@127.0.0.1:8080");
        let whole = proxy_url(&format!("${{{name}}}"), "p");
        std::env::remove_var(&name);
        let err = result.expect_err("a referenced @ must not hide literal userinfo");
        assert!(err.contains("literals") && !err.contains("hunter2"), "{err}");
        assert!(authority.is_ok(), "a wholly referenced authority has no literal credentials");
        assert!(whole.is_ok(), "a wholly referenced proxy URL has no literal credentials");
    }

    #[test]
    fn proxy_credential_origin_survives_referenced_url_delimiters() {
        let tag = swiss_core::util::random_hex(8).to_ascii_uppercase();
        let values = ["http://", "alice", ":", "hun:ter2", "@", "127.0.0.1:8080", "/proxy"];
        let names: Vec<_> = (0..values.len()).map(|i| format!("SWISS_PROXY_ORIGIN_{tag}_{i}")).collect();
        for (name, value) in names.iter().zip(values) {
            std::env::set_var(name, value);
        }
        let results: Vec<_> = (0..1usize << values.len()).map(|bits| {
            let raw: String = values.iter().enumerate().map(|(i, value)| {
                if bits & (1 << i) == 0 { value.to_string() } else { format!("{}{{{}}}", '$', names[i]) }
            }).collect();
            (bits, proxy_url(&raw, "p"))
        }).collect();
        let colon_password = proxy_url(&format!("http://${{{}}}::${{{}}}${{{}}}", names[1], names[4], names[5]), "p");
        for name in names {
            std::env::remove_var(name);
        }
        for (bits, result) in results {
            let referenced_credentials = bits & (1 << 1) != 0 && bits & (1 << 3) != 0;
            assert_eq!(result.is_ok(), referenced_credentials, "credential ownership must survive URL composition, case {bits}");
        }
        assert!(colon_password.is_err(), "only the first colon is a credential delimiter");
    }

    #[test]
    fn masking_decodes_every_key_before_clipping_the_detail() {
        let escaped = r#"k-"quoted\credential"#;
        let keys = vec!["first-secret".to_string(), "xy".to_string(), escaped.to_string()];
        let body = json!({ "error": { "message": format!("first-secret xy {}{escaped}", "x".repeat(282)) } }).to_string();
        let shown = scrubbed_detail_many(body.as_bytes(), &keys);
        assert!(!shown.contains("first-secret") && !shown.contains("xy") && !shown.contains("k-\"q"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
        assert_eq!(scrubbed_detail(br#"{"message":"bad abc"}"#, "abc"), "bad ***");
        assert_eq!(scrubbed("no key", ""), "no key");
    }

    #[test]
    fn masking_many_never_remasks_its_own_markers() {
        let keys = vec!["*".to_string(); 64];
        assert_eq!(scrubbed_many("a*b", &keys), "a***b");
        assert_eq!(scrubbed_many("αλβ", &["".into(), "λ".into()]), "α***β");
        assert_eq!(scrubbed_many("abcd", &["ab".into(), "abc".into()]), "***d");
        assert_eq!(scrubbed_many("abc***", &["abc".into(), "***".into(), "*".into()]), "******");
    }

    #[test]
    fn an_error_line_carries_its_causes_once() {
        #[derive(Debug)]
        struct Wrap(&'static str, std::io::Error);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.1)
            }
        }
        let refused = || std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        assert_eq!(causes(&Wrap("error sending request", refused())), "error sending request: connection refused");
        // A parent that already quotes its cause is not followed by it again.
        assert_eq!(causes(&Wrap("tcp connect error: connection refused", refused())), "tcp connect error: connection refused");
    }
}
