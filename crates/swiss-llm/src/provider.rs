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

//! Named providers (SPEC §llm.providers): an endpoint, the wire it speaks, a key
//! reference and the models it serves - one Ollama, one GLM, one OpenAI entry each named
//! once. The embedding tier and every agent name a provider and pick one of its models;
//! nothing restates an endpoint, and a model nobody listed is never asked.
//!
//! Every chat goes through [Chat] over the tree's one reqwest. genai supplies the
//! canonical messages and the resolver; both wires - OpenAI's chat completions and
//! Anthropic's messages - are swiss-owned bodies read over maintained SSE grammar with
//! bounded input and complete tool accumulation. The key resolves when a [Chat] is
//! built and lives only there; errors never quote it or a request.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatResponse, ContentPart, MessageContent, StopReason, StreamEnd, Usage,
};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};
use serde_json::{json, Value};

use crate::config::{self, clip, flag, http_url, object, only, printable, provider_name_ok, proxy_url, reference, scrubbed_detail_many, scrubbed_many, text, whole};
use crate::telemetry::Session;

mod anthropic;
mod stream;
#[cfg(test)]
mod proxy_tests;

/// At most this many named providers: a panel list stays a list.
pub const PROVIDERS_MAX: usize = 16;
const MODEL_MAX: usize = 200;
/// At most this many models per provider.
pub const MODELS_MAX: usize = 64;
/// The keys a provider entry may carry.
const ENTRY_KEYS: [&str; 5] = ["style", "baseUrl", "apiKey", "models", "proxy"];
/// The keys one `models` item may carry.
const MODEL_KEYS: [&str; 7] = ["id", "kind", "name", "contextWindow", "maxOutput", "image", "thinking"];
/// The keys only a chat model may carry: what it holds, answers, reads and thinks.
const CHAT_MODEL_KEYS: [&str; 4] = ["contextWindow", "maxOutput", "image", "thinking"];
/// How long a model's display name runs.
const MODEL_NAME_MAX: usize = 80;
/// A model's context window, in tokens: a 1K toy to a 100M frontier.
const WINDOW_MIN: u64 = 1024;
const WINDOW_MAX: u64 = 100_000_000;
/// The most tokens a model answers in one request, the floor an agent's `maxTokens` keeps.
const OUTPUT_MIN: u64 = 256;
/// How long an error message runs before it is clipped.
const ERROR_MAX: usize = 600;

/// How the provider is spoken to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// OpenAI-compatible `POST {baseUrl}/chat/completions` (OpenAI, Ollama, vLLM, GLM's
    /// `/api/paas/v4`, DeepSeek): the base URL carries its own version segment.
    OpenAi,
    /// Anthropic-compatible `POST {baseUrl}/v1/messages` (Anthropic, GLM's `/api/anthropic`,
    /// Kimi's and DeepSeek's `/anthropic`).
    Anthropic,
}

impl Style {
    pub fn as_str(&self) -> &'static str {
        match self {
            Style::OpenAi => "openai",
            Style::Anthropic => "anthropic",
        }
    }
}

/// What a listed model is for: an agent asks a chat model, the embedding tier an
/// embedding model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Chat,
    Embedding,
}

impl ModelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelKind::Chat => "chat",
            ModelKind::Embedding => "embedding",
        }
    }

    /// The kind with its article, for a sentence.
    fn spoken(&self) -> &'static str {
        match self {
            ModelKind::Chat => "a chat",
            ModelKind::Embedding => "an embedding",
        }
    }
}

/// One model a provider serves, by its name on the wire, and what the vendor says of it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    pub id: String,
    pub kind: ModelKind,
    /// What a menu shows for it ("DeepSeek V4.1 Flash"); None shows the id.
    pub name: Option<String>,
    /// A chat model's context window in tokens: what one request and its answer hold.
    pub context_window: Option<u32>,
    /// A chat model's most tokens in one answer: a request asks no more than this.
    pub max_output: Option<u32>,
    /// A chat model that reads images beside text.
    pub image: bool,
    /// A chat model that takes a thinking effort; false sends none, whatever the agent asks.
    pub thinking: bool,
}

impl ModelEntry {
    /// A model as a bare listing names it: its id and kind, nothing declared.
    pub fn new(id: impl Into<String>, kind: ModelKind) -> ModelEntry {
        ModelEntry { id: id.into(), kind, name: None, context_window: None, max_output: None, image: false, thinking: true }
    }
}

/// One named provider entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEntry {
    pub name: String,
    pub style: Style,
    /// Without a trailing slash; never carries credentials.
    pub base_url: String,
    /// The `${...}` reference, never a value; None for a keyless endpoint (a local Ollama).
    pub api_key: Option<String>,
    /// The models it serves, in the order listed: the first chat model is what an agent
    /// asks when it names none, the first embedding model the embedding tier's default.
    pub models: Vec<ModelEntry>,
    /// An http/https/socks5 proxy for this provider only; references resolve at use.
    pub proxy: Option<String>,
}

/// The `providers` block: name -> entry.
pub(crate) fn parse_all(block: Option<&Value>) -> Result<BTreeMap<String, ProviderEntry>, String> {
    let Some(block) = block.filter(|b| !b.is_null()) else { return Ok(BTreeMap::new()) };
    let named = object(block, "providers")?;
    if named.len() > PROVIDERS_MAX {
        return Err(format!("providers holds {} entries; at most {PROVIDERS_MAX}", named.len()));
    }
    let mut out: BTreeMap<String, ProviderEntry> = BTreeMap::new();
    for (name, entry) in named {
        if !provider_name_ok(name) {
            return Err(format!(
                "provider names are 1..={} chars of [A-Za-z0-9-] starting with a letter or digit, got {name:?}",
                config::NAME_MAX
            ));
        }
        // GLM beside glm would be two rows a reader cannot tell apart in a model menu.
        if let Some(other) = out.keys().find(|other| other.eq_ignore_ascii_case(name)) {
            return Err(format!("providers {other:?} and {name:?} differ only in case; name one of them otherwise"));
        }
        out.insert(name.clone(), parse_entry(name, entry)?);
    }
    Ok(out)
}

fn parse_entry(name: &str, entry: &Value) -> Result<ProviderEntry, String> {
    let place = format!("providers.{name}");
    let obj = object(entry, &place)?;
    only(obj, &place, &ENTRY_KEYS)?;
    let style = match text(obj, &place, "style")?.as_deref() {
        Some("openai") => Style::OpenAi,
        Some("anthropic") => Style::Anthropic,
        Some(other) => return Err(format!("{place}.style must be \"openai\" or \"anthropic\", got {other:?}")),
        None => return Err(format!("{place}.style is required: \"openai\" or \"anthropic\"")),
    };
    let base_url = match text(obj, &place, "baseUrl")? {
        Some(url) => http_url(&url, &format!("{place}.baseUrl"))?,
        None => return Err(format!("{place}.baseUrl is required (e.g. http://127.0.0.1:11434/v1)")),
    };
    let api_key = reference(obj, &place, "apiKey")?;
    let models = parse_models(obj.get("models"), &place, style)?;
    let proxy = match text(obj, &place, "proxy")? {
        Some(proxy) => Some(proxy_url(&proxy, &format!("{place}.proxy"))?),
        None => None,
    };
    Ok(ProviderEntry { name: name.to_string(), style, base_url, api_key, models, proxy })
}

/// The `models` list: `{"id", "kind"}` items, `kind` "chat" (the default) or
/// "embedding", each id once, each with an optional display `name`. A chat model may say
/// what the vendor states of it: `contextWindow` and `maxOutput` in tokens (the answer
/// within the window), `image` (reads images) and `thinking` (takes an effort, true when
/// unset). An anthropic-style provider serves no embeddings - its wire has no /embeddings
/// - so it lists chat models only.
fn parse_models(value: Option<&Value>, place: &str, style: Style) -> Result<Vec<ModelEntry>, String> {
    let list = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(format!("{place}.models must be a list of {{\"id\", \"kind\"}} items")),
    };
    if list.len() > MODELS_MAX {
        return Err(format!("{place}.models lists {} models; at most {MODELS_MAX}", list.len()));
    }
    let mut out: Vec<ModelEntry> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let at = format!("{place}.models[{i}]");
        let obj = object(item, &at)?;
        only(obj, &at, &MODEL_KEYS)?;
        let id = text(obj, &at, "id")?.ok_or_else(|| format!("{at}.id is required: the model's name on the wire"))?;
        if !printable(&id, MODEL_MAX) {
            return Err(format!("{at}.id: at most {MODEL_MAX} printable characters"));
        }
        let kind = match text(obj, &at, "kind")?.as_deref() {
            None | Some("chat") => ModelKind::Chat,
            Some("embedding") => ModelKind::Embedding,
            Some(other) => return Err(format!("{at}.kind must be \"chat\" or \"embedding\", got {other:?}")),
        };
        if kind == ModelKind::Embedding && style == Style::Anthropic {
            return Err(format!(
                "{at}: an anthropic-style provider serves no embeddings; list {id:?} on an openai-style entry for that endpoint"
            ));
        }
        if out.iter().any(|m| m.id == id) {
            return Err(format!("{at}.id {id:?} is listed twice"));
        }
        let name = text(obj, &at, "name")?;
        if name.as_deref().is_some_and(|name| !printable(name, MODEL_NAME_MAX)) {
            return Err(format!("{at}.name: at most {MODEL_NAME_MAX} printable characters"));
        }
        if kind == ModelKind::Embedding {
            if let Some(key) = CHAT_MODEL_KEYS.iter().find(|key| obj.get(**key).is_some_and(|v| !v.is_null())) {
                return Err(format!("{at}.{key} is a chat model's; an embedding model takes id, kind and name"));
            }
        }
        let context_window = whole(obj, &at, "contextWindow", WINDOW_MIN, WINDOW_MAX)?.map(|n| n as u32);
        let max_output = whole(obj, &at, "maxOutput", OUTPUT_MIN, WINDOW_MAX)?.map(|n| n as u32);
        if let (Some(window), Some(output)) = (context_window, max_output) {
            if output > window {
                return Err(format!("{at}.maxOutput {output} is past its contextWindow {window}: the answer is part of the window"));
            }
        }
        let image = flag(obj, &at, "image")?.unwrap_or(false);
        let thinking = flag(obj, &at, "thinking")?.unwrap_or(true);
        out.push(ModelEntry { id, kind, name, context_window, max_output, image, thinking });
    }
    Ok(out)
}

/// The entry's key, resolved now (empty for a keyless endpoint). A configured reference
/// that resolves to nothing is a config error, not a keyless request: the error names the
/// reference, never a value.
pub fn resolve_key(entry: &ProviderEntry) -> Result<String, String> {
    match &entry.api_key {
        None => Ok(String::new()),
        Some(reference) => {
            let resolved = swiss_core::secure::refs::resolve(reference)
                .map_err(|err| format!("providers.{}.apiKey: {err}", entry.name))?;
            if resolved.is_empty() {
                Err(format!("providers.{}.apiKey: the reference {reference} resolved to an empty value", entry.name))
            } else {
                Ok(resolved)
            }
        }
    }
}

/// The endpoint a request path joins. The OpenAI wire appends `chat/completions`, so an
/// OpenAI-style base URL is taken as written (it names its own version: `/v1`, GLM's
/// `/v4`); the Anthropic wire appends `messages`, so an Anthropic-style root
/// ("https://open.bigmodel.cn/api/anthropic") gains its `/v1`.
fn endpoint_of(entry: &ProviderEntry) -> String {
    let mut url = entry.base_url.trim_end_matches('/').to_string();
    if entry.style == Style::Anthropic && !url.ends_with("/v1") {
        url.push_str("/v1");
    }
    url.push('/');
    url
}

impl ProviderEntry {
    /// The listed model with this id, when the entry lists it.
    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The listed ids of one kind, in order.
    pub fn models_of(&self, kind: ModelKind) -> impl Iterator<Item = &str> {
        self.models.iter().filter(move |m| m.kind == kind).map(|m| m.id.as_str())
    }

    /// The model a consumer asks: the one it named, which must be listed under that kind,
    /// or else the first of the kind. `place` is the consumer's field (`agents.x.model`).
    pub fn pick(&self, named: Option<&str>, kind: ModelKind, place: &str) -> Result<String, String> {
        let listed: Vec<&str> = self.models_of(kind).collect();
        let Some(id) = named else {
            return listed.first().map(|id| id.to_string()).ok_or_else(|| {
                format!("{place} is required: providers.{} lists no {} model to default to", self.name, kind.as_str())
            });
        };
        if listed.contains(&id) {
            return Ok(id.to_string());
        }
        if let Some(other) = self.models.iter().find(|m| m.id == id) {
            return Err(format!("{place}: {id:?} is {} model on providers.{}, not {} model", other.kind.spoken(), self.name, kind.spoken()));
        }
        let have = if listed.is_empty() {
            format!("it lists no {} model", kind.as_str())
        } else {
            format!("its {} models: {}", kind.as_str(), listed.join(", "))
        };
        Err(format!("{place}: providers.{} does not list {id:?} ({have}); add it to the provider's models first", self.name))
    }

    /// The URL the provider lists its models at, as [discover] asks it.
    pub fn models_url(&self) -> String {
        format!("{}models", endpoint_of(self))
    }

    /// The URL a chat request is posted to, as the panel shows it beside the base URL.
    pub fn chat_url(&self) -> String {
        let path = match self.style {
            Style::OpenAi => "chat/completions",
            Style::Anthropic => "messages",
        };
        format!("{}{path}", endpoint_of(self))
    }
}

/// Token counts in Claude Code's semantics (what its `api_request` event reports and
/// cc-otel's columns mean): `input` excludes the cache, cache reads and writes apart.
/// genai normalizes every adapter to OpenAI's sense, where the prompt count includes
/// both, so the cache is taken back out here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Tokens {
    pub fn of(usage: &Usage) -> Tokens {
        let n = |v: Option<i32>| v.map(|v| v.max(0) as u64).unwrap_or(0);
        let details = usage.prompt_tokens_details.as_ref();
        let cache_read = n(details.and_then(|d| d.cached_tokens));
        let cache_creation = n(details.and_then(|d| d.cache_creation_tokens));
        let prompt = n(usage.prompt_tokens);
        Tokens {
            input: prompt.saturating_sub(cache_read + cache_creation),
            output: n(usage.completion_tokens),
            cache_read,
            cache_creation,
        }
    }
}

impl std::ops::AddAssign for Tokens {
    fn add_assign(&mut self, other: Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

/// One failed call: a short message (status and the provider's own words, never a
/// payload dump), the HTTP status when there was one.
#[derive(Debug, Clone, PartialEq)]
pub struct CallError {
    pub message: String,
    pub status: Option<u16>,
    /// "http", "network", "timeout" or "protocol".
    pub kind: &'static str,
    pub retryable: bool,
    pub duration_ms: u64,
}

/// One answered call.
pub struct Reply {
    pub response: ChatResponse,
    pub tokens: Tokens,
    pub duration_ms: u64,
    /// From the request's start to the model's first output on the wire (thinking, text
    /// or a tool call): the time to first token. None when the reply carried no output.
    pub ttft_ms: Option<u64>,
    /// The provider's response id when it gives one, else one minted here: cc-otel
    /// deduplicates on it, so a batch re-sent after a failed export never counts twice.
    pub request_id: String,
}

/// One delta off the wire (a streamed call's reasoning or answer text, the piece that
/// just arrived), for a live surface to paint as a typewriter. Thinking and answer
/// ride as different kinds so the surface never mixes them.
#[derive(Debug, Clone)]
pub enum Delta {
    Thinking(String),
    Text(String),
}

/// A chat client for one provider and model, the key resolved into it. When a
/// [history](Chat::recording) is attached, every round trip through [answer] and the
/// builtin loop files itself into logs/llm.db (SPEC llm.history) - the local record a
/// disabled telemetry collector never silences. A clone shares the HTTP clients and the
/// history: a consumer asks one task at another effort over a copy (a lead's worker).
#[derive(Clone)]
pub struct Chat {
    client: Client,
    http: reqwest::Client,
    model_id: String,
    options: ChatOptions,
    pub provider: String,
    pub model: String,
    /// The endpoint every request of this chat goes to, and the wire style - the
    /// history's body records both beside each call (SPEC llm.history).
    endpoint: String,
    style: Style,
    key: String,
    // Provider key and credentials captured by this exact HTTP client.
    error_secrets: Vec<String>,
    history: Option<Arc<crate::history::LlmHistory>>,
    history_run_id: Option<i64>,
    agent: Option<String>,
    /// The most one request of the builtin loop carries, serialized, in bytes.
    context_cap: usize,
    /// The model takes an effort (its entry's `thinking`): false drops every effort asked.
    thinks: bool,
    /// The model's context window in tokens, when its entry states one.
    context_window: Option<u32>,
}

impl Chat {
    /// `model` is asked as given (a consumer checked it against the list at parse); without
    /// one, the entry's first chat model. A listed model's `maxOutput` caps `max_tokens`: an
    /// agent's 128K ceiling asks a 64K model for 64K, never a request the vendor refuses.
    pub fn new(entry: &ProviderEntry, model: Option<&str>, max_tokens: u32) -> Result<Chat, String> {
        let model = model
            .or_else(|| entry.models_of(ModelKind::Chat).next())
            .ok_or_else(|| format!("providers.{} lists no chat model, and the caller named none", entry.name))?
            .to_string();
        let listed = entry.model(&model);
        let max_tokens = listed.and_then(|m| m.max_output).map_or(max_tokens, |cap| max_tokens.min(cap));
        let thinks = listed.is_none_or(|m| m.thinking);
        let context_window = listed.and_then(|m| m.context_window);
        let key = resolve_key(entry)?;
        let kind = match entry.style {
            Style::Anthropic => AdapterKind::Anthropic,
            Style::OpenAi => AdapterKind::OpenAI,
        };
        let endpoint = endpoint_of(entry);
        // The resolver closure captures the endpoint by move; the Chat keeps its own
        // copy for the history's wire facts.
        let wire_endpoint = endpoint.clone();
        let auth_key = key.clone();
        let resolver = ServiceTargetResolver::from_resolver_fn(move |target: ServiceTarget| {
            // genai's OpenAI adapter refuses AuthData::None outright, so a keyless endpoint
            // (a local Ollama) gets an empty key: "Authorization: Bearer ", which such an
            // endpoint ignores.
            Ok(ServiceTarget {
                endpoint: Endpoint::from_owned(endpoint.clone()),
                auth: AuthData::from_single(auth_key.clone()),
                // The "anthropic::model" form routes genai to the adapter; the wire wants
                // the bare name, so the namespace is stripped here (GLM 400s "model not
                // found" on any prefixed form, and "/" is not genai's separator).
                model: ModelIden::new(kind, target.model.model_name.namespace_and_name().1),
            })
        });
        let (http, mut error_secrets) = config::client_with(entry.proxy.as_deref(), &format!("providers.{}.proxy", entry.name), reqwest::Client::builder())?;
        if !key.is_empty() { error_secrets.push(key.clone()); }
        let client = Client::builder().with_reqwest(http.clone()).with_service_target_resolver(resolver).build();
        let options = ChatOptions {
            max_tokens: Some(max_tokens),
            capture_usage: Some(true),
            ..ChatOptions::default()
        };
        // genai's namespace separator is "::" - "anthropic/model" would reach the adapter
        // but arrive on the wire with the prefix whole, which GLM refuses.
        let model_id = format!("{}::{}", entry.style.as_str(), model);
        Ok(Chat {
            client,
            http,
            model_id,
            options,
            provider: entry.name.clone(),
            model,
            endpoint: wire_endpoint,
            style: entry.style,
            key,
            error_secrets,
            history: None,
            history_run_id: None,
            agent: None,
            context_cap: crate::agent::CONTEXT_KIB_DEFAULT as usize * 1024,
            thinks,
            context_window,
        })
    }

    /// The model's context window in tokens, as its provider entry states it.
    pub fn context_window(&self) -> Option<u32> {
        self.context_window
    }

    /// The most one request carries (SPEC llm.agents `maxContextKiB`): the builtin loop
    /// refuses a round over it, and a conversation whose record has passed it takes no
    /// further send.
    pub fn with_context_kib(mut self, kib: u32) -> Chat {
        self.context_cap = kib as usize * 1024;
        self
    }

    /// [Chat::with_context_kib] in bytes.
    pub fn context_cap(&self) -> usize {
        self.context_cap
    }

    /// How hard the model thinks before it answers (SPEC llm.agents `effort`): None
    /// leaves the model's default - or the effort its name's suffix says
    /// ("glm-5.3-flash-low"). Each wire spells it its own way: OpenAI's
    /// `reasoning_effort`, Anthropic's adaptive thinking with `output_config.effort`. A
    /// model its entry says takes none (`thinking: false`) is sent none: the vendor would
    /// refuse the field or ignore it.
    pub fn with_effort(mut self, effort: Option<genai::chat::ReasoningEffort>) -> Chat {
        self.options.reasoning_effort = effort.filter(|_| self.thinks);
        self
    }

    /// Attach the module history and the agent name calls file under. A None history
    /// (tests, a panel probe before the plugin opened its store) records nothing.
    pub fn recording(mut self, history: Option<Arc<crate::history::LlmHistory>>, agent: Option<&str>) -> Chat {
        self.history = history;
        self.agent = agent.map(str::to_string);
        self
    }

    /// Scope subsequent call rows to one reserved or open conversation run.
    pub fn recording_run(mut self, run_id: i64) -> Chat {
        self.history_run_id = Some(run_id);
        self
    }

    /// The explicit run scope, absent for legacy callers that do not create a run.
    pub fn history_run_id(&self) -> Option<i64> {
        self.history_run_id
    }

    /// File a round trip under its explicit run, or preserve the unscoped legacy API.
    pub(crate) fn record_chat(&self, call: crate::history::ChatCall<'_>) {
        if let Some(history) = &self.history {
            match self.history_run_id {
                Some(run_id) => history.record_chat_in_run(run_id, call),
                None => history.record_chat(call),
            }
        }
    }

    /// File a round trip a consumer-side deadline dropped (SPEC llm.history: every
    /// round trip, including the ones that never answered): under `run_id` when the
    /// caller's run reserved one, else under this chat's own run scope, else the
    /// unscoped legacy record. A chat without history files nothing.
    pub fn record_timeout(&self, run_id: Option<i64>, system: Option<&str>, prompt: &str, waited_ms: u64) {
        let Some(history) = &self.history else { return };
        let call = crate::history::ChatTimeout {
            provider: &self.provider,
            model: &self.model,
            agent: self.agent_name(),
            system,
            prompt,
            turn: None,
            waited_ms,
        };
        match run_id.or(self.history_run_id) {
            Some(id) => history.record_chat_timeout_in_run(id, call),
            None => history.record_chat_timeout(call),
        }
    }

    /// The verified OpenAI-format DeepSeek tool contract requires all prior reasoning.
    /// Provider labels, compatible proxies and model substrings are not capabilities.
    pub fn requires_reasoning_replay(&self, request: &ChatRequest) -> bool {
        let configured = self.wire_model_name();
        let (_, wire_model) = stream::wire_model(&configured, &self.options);
        self.style == Style::OpenAi
            && request.tools.as_ref().is_some_and(|tools| !tools.is_empty())
            && matches!(wire_model, "deepseek-flash" | "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp")
            && reqwest::Url::parse(&self.endpoint).ok().is_some_and(|url| url.host_str() == Some("api.deepseek.com"))
    }

    /// The attached history, when the plugin opened one.
    pub fn history(&self) -> Option<&Arc<crate::history::LlmHistory>> {
        self.history.as_ref()
    }

    /// The agent name this chat files its calls and runs under, when it has one.
    pub fn agent_name(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// A conversation recorder for this chat: named after its agent, or a no-op
    /// recorder when no history is attached.
    pub fn run_recorder(&self, runtime: &str) -> crate::history::RunRecorder {
        match &self.history {
            Some(history) => history.run_recorder(
                // Unreachable today (every caller names its agent); the word says
                // what the row is rather than filing an empty string the panel would
                // render as a bare "run #N".
                self.agent.as_deref().unwrap_or("anonymous"),
                runtime,
                Some(&self.provider),
                Some(&self.model),
            ),
            None => crate::history::RunRecorder::none(),
        }
    }

    /// The wire facts a history body keeps beside each call (SPEC llm.history): the
    /// endpoint, the per-request options, and every header each request carries -
    /// credential headers are NAMED so the record says they were sent, but their
    /// values never leave this process. The openai style sends the key as
    /// Authorization: Bearer; the anthropic style sends it twice, as x-api-key
    /// (Anthropic reads it) and as Bearer (GLM's compatible endpoint reads only that).
    pub fn wire(&self) -> Value {
        let hidden = if self.key.is_empty() { "" } else { "(credential, not recorded)" };
        let mut headers = json!({ "content-type": "application/json" });
        match self.style {
            Style::Anthropic => {
                headers["anthropic-version"] = json!("2023-06-01");
                headers["x-api-key"] = json!(hidden);
                if !self.key.is_empty() {
                    headers["authorization"] = json!("(credential, not recorded)");
                }
            }
            Style::OpenAi => {
                headers["authorization"] = json!(hidden);
            }
        }
        // An extra header in ChatOptions could carry a live key (one did, until
        // 2026-10-11): its name is information, its value never leaves this process.
        let mut options = serde_json::to_value(&self.options).unwrap_or(Value::Null);
        scrub_extra_headers(&mut options);
        json!({
            "endpoint": self.endpoint,
            "headers": headers,
            "options": options,
        })
    }

    // Mirror the existing resolver's synthetic namespace removal, then the
    // configured namespace removal. Neither step sends a request.
    fn wire_model_name(&self) -> String {
        let configured = genai::ModelName::from(self.model_id.as_str());
        let resolved = genai::ModelName::from(configured.namespace_and_name().1);
        resolved.namespace_and_name().1.to_string()
    }

    /// Exact JSON bytes of the outbound body, either wire; no auth or network I/O.
    pub(crate) fn request_body(&self, request: &ChatRequest) -> Result<Vec<u8>, CallError> {
        let payload = match self.style {
            Style::OpenAi => stream::openai_payload(&self.wire_model_name(), request.clone(), &self.options),
            Style::Anthropic => anthropic::payload(&self.wire_model_name(), request.clone(), &self.options),
        }
        .map_err(|fault| stream_call_error(fault, &self.error_secrets, 0))?;
        serde_json::to_vec(&payload).map_err(|_| protocol_call_error("the request could not be serialized", 0))
    }

    /// Legacy synchronous delivery adapter. Live consumers use the awaited facade.
    pub(crate) async fn exec_streamed(
        &self,
        request: ChatRequest,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Reply, CallError> {
        let mut sink = |delta| { on_delta(delta); std::future::ready(true) };
        self.exec_streamed_async(request, &mut sink).await?
            .ok_or_else(|| protocol_call_error("the provider's stream delivery stopped unexpectedly", 0))
    }

    /// Await each delta before driving the source again. false drops the source and
    /// returns no completed reply, even when a terminal is cached in the same chunk.
    pub(crate) async fn exec_streamed_async<F, Fut>(
        &self,
        request: ChatRequest,
        sink: &mut F,
    ) -> Result<Option<Reply>, CallError>
    where
        F: FnMut(Delta) -> Fut + Send,
        Fut: std::future::Future<Output = bool> + Send,
    {
        let started = Instant::now();
        let ms = || started.elapsed().as_millis() as u64;
        let body = self.request_body(&request)?;
        if self.style == Style::Anthropic {
            let base = reqwest::Url::parse(&self.endpoint).map_err(|_| protocol_call_error("the provider endpoint could not be parsed", ms()))?;
            let mut url = base.join("messages").map_err(|_| protocol_call_error("the provider endpoint could not be joined", ms()))?;
            url.set_query(base.query());
            let mut post = self.http.post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header("anthropic-version", anthropic::VERSION);
            // Anthropic reads x-api-key and ignores the rest; GLM's compatible endpoint
            // reads only Bearer. The key rides on both; each side takes the one it knows.
            if !self.key.is_empty() {
                post = post.header("x-api-key", &self.key).bearer_auth(&self.key);
            }
            let iden = ModelIden::new(AdapterKind::Anthropic, self.model.as_str());
            return self.read_stream(post.body(body), anthropic::Reading::default(), iden, sink, started).await;
        }
        let target = self.client.resolve_service_target(self.model_id.as_str()).await
            .map_err(|err| call_error(&err, &self.error_secrets, self.key.is_empty(), ms()))?;
        let base = reqwest::Url::parse(target.endpoint.base_url())
            .map_err(|_| protocol_call_error("the provider endpoint could not be parsed", ms()))?;
        let mut url = base.join("chat/completions")
            .map_err(|_| protocol_call_error("the provider endpoint could not be joined", ms()))?;
        url.set_query(base.query());
        let key = target.auth.single_key_value()
            .map_err(|resolver_error| {
                let err = genai::Error::Resolver { model_iden: target.model.clone(), resolver_error };
                call_error(&err, &self.error_secrets, self.key.is_empty(), ms())
            })?;
        // The same configured reqwest pool owns proxy/TLS behavior. Empty Bearer
        // preserves the existing keyless OpenAI-compatible endpoint contract.
        let post = self.http.post(url).bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json").body(body);
        self.read_stream(post, stream::openai_assembly(), target.model, sink, started).await
    }

    /// Send one streamed request and read its answer through the wire's assembly: a
    /// refusal is its status and the provider's words, a reply its deltas awaited out.
    async fn read_stream<A, F, Fut>(
        &self,
        post: reqwest::RequestBuilder,
        assembly: A,
        iden: ModelIden,
        sink: &mut F,
        started: Instant,
    ) -> Result<Option<Reply>, CallError>
    where
        A: stream::Assembly,
        F: FnMut(Delta) -> Fut + Send,
        Fut: std::future::Future<Output = bool> + Send,
    {
        let ms = || started.elapsed().as_millis() as u64;
        let response = post.send().await.map_err(|err| network_call_error(&err, &self.error_secrets, ms()))?;
        let status = response.status();
        if !status.is_success() {
            let bytes = match stream::bounded_body(response, stream::ERROR_BODY_MAX).await {
                Ok(bytes) => Some(bytes),
                Err(stream::BodyFault::TooLarge) => None,
                Err(stream::BodyFault::Source(err)) => return Err(network_call_error(&err, &self.error_secrets, ms())),
            };
            return Err(http_call_error(status.as_u16(), bytes.as_deref(), &self.error_secrets, self.key.is_empty(), ms()));
        }
        let mut began = None;
        let end = stream::drive(response.bytes_stream(), sink, assembly, &mut began).await
            .map_err(|fault| stream_call_error(fault, &self.error_secrets, ms()))?;
        let ttft_ms = began.map(|at: Instant| at.duration_since(started).as_millis() as u64);
        Ok(end.map(|end| reply_from_end(iden, end, ms(), ttft_ms)))
    }
}

fn reply_from_end(iden: ModelIden, mut end: StreamEnd, duration_ms: u64, ttft_ms: Option<u64>) -> Reply {
    // A token-cut turn is useful text, never an executable tool batch.
    if matches!(end.captured_stop_reason, Some(StopReason::MaxTokens(_))) {
        end.captured_content = end.captured_content.take().map(|content| {
            content.into_iter().filter(|part| !matches!(part, ContentPart::ToolCall(_))).collect()
        });
    }
    let response = ChatResponse {
        content: end.captured_content.unwrap_or_else(|| MessageContent::from(Vec::<ContentPart>::new())),
        reasoning_content: end.captured_reasoning_content,
        model_iden: iden.clone(), provider_model_iden: iden,
        stop_reason: end.captured_stop_reason, usage: end.captured_usage.unwrap_or_default(),
        captured_raw_body: None, response_id: end.captured_response_id,
    };
    let tokens = Tokens::of(&response.usage);
    let request_id = response.response_id.clone().filter(|id| !id.is_empty())
        .unwrap_or_else(|| format!("swiss-{}", swiss_core::util::random_hex(12)));
    Reply { response, tokens, duration_ms, ttft_ms, request_id }
}

fn protocol_call_error(message: &str, duration_ms: u64) -> CallError {
    CallError { message: message.to_string(), status: None, kind: "protocol", retryable: false, duration_ms }
}

fn network_call_error(err: &reqwest::Error, secrets: &[String], duration_ms: u64) -> CallError {
    CallError {
        message: format!("the request failed: {}", clip(&scrubbed_many(&config::causes(err), secrets), ERROR_MAX)),
        status: None, kind: if err.is_timeout() { "timeout" } else { "network" }, retryable: true, duration_ms,
    }
}

fn http_call_error(status: u16, bytes: Option<&[u8]>, secrets: &[String], keyless: bool, duration_ms: u64) -> CallError {
    let detail = bytes.map(|bytes| scrubbed_detail_many(bytes, secrets))
        .unwrap_or_else(|| format!("the error body exceeded {} bytes", stream::ERROR_BODY_MAX));
    let mut message = format!("the provider answered {status}: {detail}");
    if matches!(status, 401 | 403) && keyless { message.push_str(" (no apiKey is configured)"); }
    CallError { message, status: Some(status), kind: "http", retryable: status == 429 || status >= 500, duration_ms }
}

fn stream_call_error(fault: stream::StreamFault, secrets: &[String], duration_ms: u64) -> CallError {
    match fault {
        stream::StreamFault::Protocol(message) => protocol_call_error(message, duration_ms),
        stream::StreamFault::Source(err) => {
            if let Some(err) = err.downcast_ref::<reqwest::Error>() { network_call_error(err, secrets, duration_ms) }
            else { protocol_call_error("the provider's stream body could not be read", duration_ms) }
        }
        stream::StreamFault::Provider(body) => protocol_call_error(
            &format!("the provider returned an error: {}", scrubbed_detail_many(body.as_bytes(), secrets)), duration_ms),
    }
}

/// genai's error, said briefly. Its display of a failed generation carries the whole
/// request payload (one runner's error once stretched a runs list to tens of thousands of
/// pixels), so every arm here builds its own sentence.
fn call_error(err: &genai::Error, secrets: &[String], keyless: bool, duration_ms: u64) -> CallError {
    use genai::webc::Error as Web;
    let brief = |message: &str| clip(&scrubbed_many(message, secrets), ERROR_MAX);
    let status_error = |status: u16, body: &str| http_call_error(status, Some(body.as_bytes()), secrets, keyless, duration_ms);
    let unreadable = |message: &str| CallError {
        message: format!("the provider's answer could not be read: {}", brief(message)),
        status: None, kind: "protocol", retryable: false, duration_ms,
    };
    match err {
        genai::Error::WebModelCall { webc_error, .. } | genai::Error::WebAdapterCall { webc_error, .. } => match webc_error {
            Web::ResponseFailedStatus { status, body, .. } => status_error(status.as_u16(), body),
            Web::Reqwest(err) => network_call_error(err, secrets, duration_ms),
            other => unreadable(&other.to_string()),
        },
        genai::Error::WebStream { cause, error, .. } => {
            // The streaming adapter boxes HTTP and transport errors. Keep their type,
            // not the fallback protocol classification of the wrapper's display text.
            if let Some(inner) = error.downcast_ref::<genai::Error>() {
                return call_error(inner, secrets, keyless, duration_ms);
            }
            if let Some(inner) = error.downcast_ref::<reqwest::Error>() {
                network_call_error(inner, secrets, duration_ms)
            } else if let Some(Web::Reqwest(inner)) = error.downcast_ref::<Web>() {
                network_call_error(inner, secrets, duration_ms)
            } else {
                unreadable(cause)
            }
        }
        genai::Error::HttpError { status, body, .. } => status_error(status.as_u16(), body),
        genai::Error::ChatResponseGeneration { cause, .. } => unreadable(cause),
        genai::Error::ChatResponse { body, .. } => CallError {
            message: format!("the provider returned an error: {}", scrubbed_detail_many(body.to_string().as_bytes(), secrets)),
            status: None, kind: "protocol", retryable: false, duration_ms,
        },
        other => CallError {
            message: brief(&other.to_string()), status: None, kind: "protocol", retryable: false, duration_ms,
        },
    }
}

/// One prompt, one answer: the panel's Test. `system` is the system message (an agent's
/// prompt; None sends none). The session reports the prompt and the call (a disabled
/// session reports nothing); the answer is every text block of the reply, joined, and a
/// reply with none is an error. One round trip, never re-sent: a Test reports the request
/// as it went, so a failure in transit shows as one instead of hiding behind a second try.
pub async fn ask(chat: &Chat, system: Option<&str>, prompt: &str, session: &Session) -> Result<String, String> {
    let answered = answer_round(chat, system, prompt, session, &mut |_| {}).await.map_err(|err| err.message)?;
    if answered.text.trim().is_empty() {
        Err("the provider's answer carries no text".into())
    } else {
        Ok(answered.text)
    }
}

/// What one request came back with: every text block of the reply, joined, and whether
/// the reply stopped at the request's `max_tokens` - a cut answer is a partial one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub text: String,
    pub cut: bool,
}

/// Blank every value under `options.extra_headers` to the credential placeholder.
/// The header NAMES stay ("an Authorization was sent" is information); a value there
/// is a live key, and the 2026-10-11 leak it prevented was real, not theoretical.
fn scrub_extra_headers(options: &mut Value) {
    if let Some(extra) = options.get_mut("extra_headers").and_then(|h| h.as_object_mut()) {
        for (_name, value) in extra.iter_mut() {
            *value = json!("(credential, not recorded)");
        }
    }
}

/// The full request as composed for one round trip, serialized as genai holds it:
/// every message (roles, tool calls, tool answers) and every offered tool with its
/// schema. The history body keeps this beside its curated keys, so a call's record
/// is the complete ask, not a digest.
pub fn request_json(request: &ChatRequest) -> Value {
    serde_json::to_value(request).unwrap_or_else(|_| json!({}))
}

/// The stop reason as one plain word (with the provider's own detail when it sent
/// one), for the history body: `tool_call`, `max_tokens`, `completed`, ... - the
/// genai enum's payloads carry the raw provider text, which is fine to show.
pub fn stop_text(stop: &Option<StopReason>) -> Option<String> {
    use genai::chat::StopReason::*;
    let (why, detail) = match stop {
        None => return None,
        Some(Completed(s)) => ("completed", s),
        Some(MaxTokens(s)) => ("max_tokens", s),
        Some(ToolCall(s)) => ("tool_call", s),
        Some(ContentFilter(s)) => ("content_filter", s),
        Some(StopSequence(s)) => ("stop_sequence", s),
        Some(Other(s)) => ("other", s),
    };
    Some(if detail.is_empty() { why.to_string() } else { format!("{why}: {detail}") })
}

/// How long [answer] waits before it asks again after a failure in transit.
const RETRY_PAUSE: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(2) };

/// [ask] with the answer handed back as it came, empty or cut, for a caller that judges
/// answers itself: the memory runtimes ask once more on an invalid one, and a thinking
/// model now and then spends its whole budget thinking. A request that failed in transit
/// (the network, the request's own timeout, 429, 5xx) is asked once more after a short
/// pause: a batch of questions on one connection dies together when the connection does,
/// and nothing was shown yet, so the second ask repeats nothing. Each attempt files its
/// own call row; a refusal (4xx) or a bad answer is never re-sent here, and neither is the
/// panel's Test ([ask]).
pub async fn answer(chat: &Chat, system: Option<&str>, prompt: &str, session: &Session) -> Result<Answered, String> {
    match answer_round(chat, system, prompt, session, &mut |_| {}).await {
        Err(err) if err.retryable => {
            tokio::time::sleep(RETRY_PAUSE).await;
            answer_round(chat, system, prompt, session, &mut |_| {})
                .await
                .map_err(|again| format!("{} (asked twice: {})", again.message, err.kind))
        }
        other => other.map_err(|err| err.message),
    }
}

/// [answer] streamed: every delta of the one ask is handed to `on_delta` as it lands
/// (a live surface paints the typewriter); the record files exactly as the blocking
/// [answer] files it, thinking included. Never re-sent: a retry would paint the deltas
/// twice.
pub async fn answer_with(
    chat: &Chat,
    system: Option<&str>,
    prompt: &str,
    session: &Session,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Answered, String> {
    answer_round(chat, system, prompt, session, on_delta).await.map_err(|err| err.message)
}

/// One round trip of [ask]/[answer]/[answer_with], filed in the history and telemetry, the
/// failure kept typed so [answer] can tell a failure in transit from a refusal.
async fn answer_round(
    chat: &Chat,
    system: Option<&str>,
    prompt: &str,
    session: &Session,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Answered, CallError> {
    let prompt_id = session.prompt(prompt);
    let mut request = ChatRequest::default();
    if let Some(system) = system {
        request = request.with_system(system);
    }
    let request = request.append_message(ChatMessage::user(prompt.to_string()));
    // The complete ask, kept before exec takes the request by value.
    let wire = chat.wire();
    let asked = request_json(&request);
    match chat.exec_streamed(request, on_delta).await {
        Ok(reply) => {
            session.api_request(&prompt_id, chat, &reply);
            let cut = matches!(reply.response.stop_reason, Some(StopReason::MaxTokens(_)));
            // The local record rides the same seam telemetry does (SPEC llm.history):
            // a call row per round trip, agent-named when the chat carries a name.
            let texts: Vec<String> = reply.response.texts().into_iter().map(str::to_string).collect();
            // The reasoning the model did on the way (DeepSeek, kimi, Claude thinking)
            // is part of the round trip's record: the answer's texts stay the answer.
            let thinking: Vec<String> = reply
                .response
                .reasoning_content
                .clone()
                .map(|one| vec![one])
                .unwrap_or_default();
            let stop = stop_text(&reply.response.stop_reason);
            let contents = serde_json::to_value(&reply.response.content).unwrap_or_else(|_| json!({}));
            chat.record_chat(crate::history::ChatCall {
                provider: &chat.provider,
                model: &chat.model,
                agent: chat.agent_name(),
                system,
                prompt,
                turn: None,
                wire: Some(&wire),
                request: Some(&asked),
                replay: None,
                attempt: None,
                outcome: crate::history::ChatOutcome::Ok {
                    texts: &texts,
                    thinking: &thinking,
                    tool_calls: &[],
                    cut,
                    tokens: reply.tokens,
                    ms: reply.duration_ms,
                    ttft_ms: reply.ttft_ms,
                    request_id: &reply.request_id,
                    stop_reason: stop.as_deref(),
                    contents: &contents,
                },
            });
            Ok(Answered { text: texts.join(""), cut })
        }
        Err(err) => {
            session.api_error(&prompt_id, chat, &err);
            chat.record_chat(crate::history::ChatCall {
                provider: &chat.provider,
                model: &chat.model,
                agent: chat.agent_name(),
                system,
                prompt,
                turn: None,
                // A failed round trip keeps the complete ask too: the answer's
                // absence is the only difference from a successful one.
                wire: Some(&wire),
                request: Some(&asked),
                replay: None,
                attempt: None,
                outcome: crate::history::ChatOutcome::Err(&err),
            });
            Err(err)
        }
    }
}

/// How long a model listing may take.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(20);
/// At most this many ids come back from one listing.
const DISCOVER_MAX: usize = 500;

/// The provider's own model listing: `GET {endpoint}models`, which OpenAI, Ollama's
/// OpenAI wire and Anthropic all answer as `{"data": [{"id": ...}]}`. Each id comes with
/// the kind its name suggests - a guess the panel prefills and the operator confirms,
/// never a config fact. An endpoint without the route (some compatible gateways) answers
/// an error that says so.
pub async fn discover(entry: &ProviderEntry) -> Result<Vec<ModelEntry>, String> {
    let key = resolve_key(entry)?;
    let builder = reqwest::Client::builder().timeout(DISCOVER_TIMEOUT);
    let (http, mut secrets) = config::client_with(entry.proxy.as_deref(), &format!("providers.{}.proxy", entry.name), builder)?;
    if !key.is_empty() { secrets.push(key.clone()); }
    let mut request = http.get(entry.models_url());
    if !key.is_empty() {
        request = request.bearer_auth(&key);
        if entry.style == Style::Anthropic {
            request = request.header("x-api-key", &key);
        }
    }
    if entry.style == Style::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("the request failed: {}", clip(&scrubbed_many(&config::causes(&err), &secrets), ERROR_MAX)))?;
    let status = response.status();
    let limit = if status.is_success() { stream::MODEL_BODY_MAX } else { stream::ERROR_BODY_MAX };
    let bytes = match stream::bounded_body(response, limit).await {
        Ok(bytes) => bytes,
        Err(stream::BodyFault::TooLarge) if status.is_success() => {
            return Err(format!("the provider's model list exceeded {} bytes", stream::MODEL_BODY_MAX));
        }
        Err(stream::BodyFault::TooLarge) => {
            return Err(http_call_error(status.as_u16(), None, &secrets, key.is_empty(), 0).message);
        }
        Err(stream::BodyFault::Source(err)) => {
            return Err(format!("the listing could not be read: {}", clip(&scrubbed_many(&config::causes(&err), &secrets), ERROR_MAX)));
        }
    };
    if !status.is_success() {
        return Err(http_call_error(status.as_u16(), Some(&bytes), &secrets, key.is_empty(), 0).message);
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| "the provider's model list is not JSON".to_string())?;
    let ids = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or("the provider's model list has no \"data\" array")?
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty() && printable(id, MODEL_MAX));
    let mut out: Vec<ModelEntry> = Vec::new();
    for id in ids {
        if out.len() >= DISCOVER_MAX {
            break;
        }
        if out.iter().any(|m| m.id == id) {
            continue;
        }
        let kind = if entry.style == Style::OpenAi && looks_like_embedding(id) { ModelKind::Embedding } else { ModelKind::Chat };
        out.push(ModelEntry::new(id, kind));
    }
    Ok(out)
}

/// Whether a model's name says it embeds: "text-embedding-3-small", "qwen3-embedding:0.6b",
/// "nomic-embed-text", "bge-m3", "all-minilm".
fn looks_like_embedding(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let name = id.rsplit('/').next().unwrap_or(&id);
    name.contains("embed") || name.starts_with("bge") || name.contains("minilm") || name.starts_with("gte-") || name.starts_with("e5-")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn parse(value: Value) -> Result<BTreeMap<String, ProviderEntry>, String> {
        parse_all(Some(&value))
    }

    #[test]
    fn scrubbed_options_name_the_extra_header_but_never_value_it() {
        // The 2026-10-11 leak this pins: ChatOptions.extra_headers carried swiss's own
        // Authorization with the live key, and the wire body serialized it whole.
        let mut options = json!({
            "max_tokens": 8192,
            "extra_headers": { "Authorization": "Bearer sk-live-do-not-record" }
        });
        scrub_extra_headers(&mut options);
        assert_eq!(options["extra_headers"]["Authorization"], "(credential, not recorded)");
        let text = options.to_string();
        assert!(!text.contains("sk-live-do-not-record"), "the value never rides");
        // A missing or null extra_headers is a no-op, not a panic.
        let mut bare = json!({ "max_tokens": 8 });
        scrub_extra_headers(&mut bare);
        let mut null = json!({ "extra_headers": null });
        scrub_extra_headers(&mut null);
        assert_eq!(bare["max_tokens"], 8);
    }

    #[test]
    fn providers_parse_named_entries() {
        let parsed = parse(json!({
            "ollama": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1/", "models": [
                { "id": "qwen3.5:4b" }, { "id": "qwen3-embedding:0.6b", "kind": "embedding" }, { "id": "qwen3:8b", "kind": "chat" }
            ] },
            "glm": { "style": "anthropic", "baseUrl": "https://open.bigmodel.cn/api/anthropic", "apiKey": "${secret://glm}" },
            "DeepSeek": { "style": "openai", "baseUrl": "https://api.deepseek.com", "models": [{ "id": "deepseek-flash" }] }
        }))
        .expect("providers");
        // A provider is named as its vendor writes itself, capitals and all.
        assert_eq!(parsed["DeepSeek"].models_of(ModelKind::Chat).collect::<Vec<_>>(), ["deepseek-flash"]);
        let ollama = &parsed["ollama"];
        assert_eq!((ollama.style, ollama.base_url.as_str(), ollama.api_key.as_deref()), (Style::OpenAi, "http://127.0.0.1:11434/v1", None));
        assert_eq!(ollama.models_of(ModelKind::Chat).collect::<Vec<_>>(), ["qwen3.5:4b", "qwen3:8b"], "kind defaults to chat, order kept");
        assert_eq!(ollama.models_of(ModelKind::Embedding).collect::<Vec<_>>(), ["qwen3-embedding:0.6b"]);
        let glm = &parsed["glm"];
        assert_eq!((glm.style, glm.models.len()), (Style::Anthropic, 0));
        assert_eq!(glm.models_url(), "https://open.bigmodel.cn/api/anthropic/v1/models");
        assert_eq!(endpoint_of(glm), "https://open.bigmodel.cn/api/anthropic/v1/");
        assert_eq!(endpoint_of(ollama), "http://127.0.0.1:11434/v1/");
        // GLM's OpenAI-compatible endpoint names its own version: nothing is appended.
        let v4 = parse(json!({ "glm-oai": { "style": "openai", "baseUrl": "https://open.bigmodel.cn/api/paas/v4" } })).expect("v4");
        assert_eq!(endpoint_of(&v4["glm-oai"]), "https://open.bigmodel.cn/api/paas/v4/");
    }

    #[test]
    fn a_model_states_its_window_output_image_and_thinking() {
        let parsed = parse(json!({ "DeepSeek": { "style": "openai", "baseUrl": "https://api.deepseek.com", "models": [
            { "id": "deepseek-flash", "name": "DeepSeek V4.1 Flash", "contextWindow": 1048576, "maxOutput": 393216, "image": true },
            { "id": "deepseek-chat", "thinking": false },
            { "id": "bge-m3", "kind": "embedding", "name": "BGE M3" }
        ] } }))
        .expect("providers");
        let flash = parsed["DeepSeek"].model("deepseek-flash").expect("listed");
        assert_eq!(flash.name.as_deref(), Some("DeepSeek V4.1 Flash"));
        assert_eq!((flash.context_window, flash.max_output, flash.image, flash.thinking), (Some(1048576), Some(393216), true, true));
        // A bare model states nothing: no figure, text only, and it takes an effort.
        let chat = parsed["DeepSeek"].model("deepseek-chat").expect("listed");
        assert_eq!((chat.name.as_deref(), chat.context_window, chat.max_output, chat.image, chat.thinking), (None, None, None, false, false));
        assert_eq!(parsed["DeepSeek"].model("bge-m3").map(|m| m.thinking), Some(true));
        assert!(parsed["DeepSeek"].model("nope").is_none());
    }

    #[test]
    fn providers_refuse_the_shape_mistakes() {
        let refused = [
            (json!({ "Zhipu GLM": {} }), "provider names"),
            (json!({ "-GLM": {} }), "provider names"),
            (json!({ "glm": { "style": "openai", "baseUrl": "http://h/v1" }, "GLM": { "style": "openai", "baseUrl": "http://h/v1" } }), "differ only in case"),
            (json!({ "x": { "style": "openai" } }), "baseUrl is required"),
            (json!({ "x": { "baseUrl": "http://h/v1" } }), "style is required"),
            (json!({ "x": { "style": "gemini", "baseUrl": "http://h/v1" } }), "style must be"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "apiKey": "sk-live-123" } }), "reference"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://user:pw@h/v1" } }), "credentials"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "maxTokens": 1 } }), "no \"maxTokens\" field"),
            // The provider's default model is the first one listed, never a field of its own.
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "model": "m" } }), "no \"model\" field"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": "m" } }), "models must be a list"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{}] } }), "models[0].id is required"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "kind": "image" }] } }), "kind must be"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "size": 1 }] } }), "no \"size\" field"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m" }, { "id": "m", "kind": "embedding" }] } }), "listed twice"),
            (json!({ "x": { "style": "anthropic", "baseUrl": "http://h", "models": [{ "id": "e", "kind": "embedding" }] } }), "serves no embeddings"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "name": "M".repeat(81) }] } }), "name: at most 80"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "contextWindow": 512 }] } }), "contextWindow must be a whole number in 1024..="),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "maxOutput": "64K" }] } }), "maxOutput must be a whole number"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "contextWindow": 8192, "maxOutput": 16384 }] } }), "past its contextWindow"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "m", "image": "yes" }] } }), "image must be a boolean"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "e", "kind": "embedding", "contextWindow": 8192 }] } }), "contextWindow is a chat model's"),
            (json!({ "x": { "style": "openai", "baseUrl": "http://h/v1", "models": [{ "id": "e", "kind": "embedding", "thinking": false }] } }), "thinking is a chat model's"),
            (json!([]), "providers must be an object"),
        ];
        for (value, needle) in refused {
            let err = parse(value.clone()).expect_err(&value.to_string());
            assert!(err.contains(needle), "{value}: {err}");
            assert!(!err.contains("sk-live-123"), "a key is never echoed: {err}");
        }
    }

    #[test]
    fn tokens_take_the_cache_back_out_of_the_prompt() {
        let usage: Usage = serde_json::from_value(json!({
            "prompt_tokens": 1000, "completion_tokens": 50,
            "prompt_tokens_details": { "cached_tokens": 600, "cache_creation_tokens": 100 }
        }))
        .expect("usage");
        assert_eq!(Tokens::of(&usage), Tokens { input: 300, output: 50, cache_read: 600, cache_creation: 100 });
        assert_eq!(Tokens::of(&Usage::default()), Tokens::default());
    }

    /// A one-shot HTTP server: answers the first request with `status` and `body`, handing
    /// the request's head and body back.
    pub(crate) async fn fake_server(status: u16, body: String) -> (String, tokio::task::JoinHandle<(String, String)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    return (String::new(), String::new());
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    let received = String::from_utf8_lossy(&buf[head_end + 4..]).into_owned();
                    let answer = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write");
                    return (head, received);
                }
            }
        });
        (url, task)
    }

    /// A one-shot SSE wire: answers the first request with `status` and the given
    /// event frames as one body - the shape every chat now speaks (stream: true).
    pub(crate) async fn fake_stream_server(status: u16, frames: String) -> (String, tokio::task::JoinHandle<(String, String)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    return (String::new(), String::new());
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    let received = String::from_utf8_lossy(&buf[head_end + 4..]).into_owned();
                    let answer = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        frames.len(),
                        frames
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write");
                    return (head, received);
                }
            }
        });
        (url, task)
    }

    pub(crate) fn entry(style: Style, base_url: String) -> ProviderEntry {
        ProviderEntry {
            name: "x".into(),
            style,
            base_url,
            api_key: None,
            models: vec![ModelEntry::new("m-1", ModelKind::Chat)],
            proxy: None,
        }
    }

    #[tokio::test]
    async fn ask_speaks_the_openai_wire() {
        // Every ask speaks the stream wire now: the text as one delta, the stop
        // with usage, then DONE.
        let (url, server) = fake_stream_server(
            200,
            r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","model":"m-1","choices":[{"index":0,"delta":{"role":"assistant","content":"the answer"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","model":"m-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}

data: [DONE]

"#
            .to_string(),
        )
        .await;
        let chat = Chat::new(&entry(Style::OpenAi, format!("{url}/v1")), None, 1024).expect("chat");
        let answer = ask(&chat, Some("be brief"), "say it", &Session::disabled()).await.expect("answer");
        assert_eq!(answer, "the answer");
        let (head, body) = server.await.expect("server");
        assert!(head.starts_with("POST /v1/chat/completions"), "{head}");
        let sent: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(sent["model"], "m-1", "no model named: the first chat model listed");
        // The agent's system prompt rides as the system message, ahead of the prompt.
        assert_eq!((sent["messages"][0]["role"].as_str(), sent["messages"][0]["content"].as_str()), (Some("system"), Some("be brief")));
        assert_eq!(sent["messages"][1]["content"], "say it");
    }

    #[tokio::test]
    async fn ask_speaks_the_anthropic_wire_with_both_key_headers() {
        std::env::set_var("SWISS_LLM_TEST_KEY_A", "k-anthropic-1234");
        // The anthropic wire streams named events: two text deltas of one block,
        // the stop with its usage.
        let (url, server) = fake_stream_server(
            200,
            r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":5,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"one "}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"two"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}

event: message_stop
data: {"type":"message_stop"}

"#
            .to_string(),
        )
        .await;
        let mut e = entry(Style::Anthropic, url);
        e.api_key = Some("${SWISS_LLM_TEST_KEY_A}".into());
        // An effort reaches the wire the way GLM's Anthropic endpoint reads it.
        let chat = Chat::new(&e, Some("glm-5.3-flash"), 2048).expect("chat").with_effort(Some(genai::chat::ReasoningEffort::Low));
        let answer = ask(&chat, None, "say it", &Session::disabled()).await.expect("answer");
        assert_eq!(answer, "one two");
        let (head, body) = server.await.expect("server");
        assert!(head.starts_with("POST /v1/messages"), "{head}");
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: k-anthropic-1234"), "{head}");
        assert!(lower.contains("authorization: bearer k-anthropic-1234"), "{head}");
        let sent: Value = serde_json::from_str(&body).expect("json");
        assert_eq!((sent["model"].as_str(), sent["max_tokens"].as_u64()), (Some("glm-5.3-flash"), Some(2048)));
        assert!(sent.get("system").is_none(), "no system prompt, no system field: {body}");
        assert_eq!((sent["thinking"]["type"].as_str(), sent["output_config"]["effort"].as_str()), (Some("adaptive"), Some("low")), "{body}");
        assert_eq!(sent["stream"], true, "{body}");
    }

    #[tokio::test]
    async fn a_reply_of_thinking_alone_is_empty_for_answer_and_an_error_for_ask() {
        // A thinking-only reply, streamed: one thinking block, no text, cut at
        // max_tokens - empty for answer, an error for ask.
        let thinking = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":5,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me weigh it..."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":2048}}

event: message_stop
data: {"type":"message_stop"}

"#
        .to_string();
        let (url, _server) = fake_stream_server(200, thinking.clone()).await;
        let chat = Chat::new(&entry(Style::Anthropic, url), None, 2048).expect("chat");
        let answered = answer(&chat, None, "say it", &Session::disabled()).await;
        assert_eq!(answered, Ok(Answered { text: String::new(), cut: true }), "it stopped at max_tokens");
        let (url, _server) = fake_stream_server(200, thinking).await;
        let chat = Chat::new(&entry(Style::Anthropic, url), None, 2048).expect("chat");
        let err = ask(&chat, None, "say it", &Session::disabled()).await.expect_err("no text");
        assert_eq!(err, "the provider's answer carries no text");
    }

    /// A server that answers its connections in turn, one `(status, content type, body)`
    /// each, counting the requests it was sent.
    async fn fake_turns(turns: Vec<(u16, &'static str, String)>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            for (status, kind, body) in turns {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(n) = socket.read(&mut chunk).await else { return };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find(|l| l.starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + length {
                        break;
                    }
                }
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let answer = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(answer.as_bytes()).await;
            }
        });
        (url, count)
    }

    #[tokio::test]
    async fn a_failure_in_transit_is_asked_once_more_and_a_refusal_never() {
        let ok = r#"data: {"id":"c-1","object":"chat.completion.chunk","model":"m-1","choices":[{"index":0,"delta":{"role":"assistant","content":"second time"},"finish_reason":"stop"}]}

data: [DONE]

"#
        .to_string();
        let busy = json!({ "error": { "message": "overloaded" } }).to_string();
        let sent = |count: &Arc<std::sync::atomic::AtomicUsize>| count.load(std::sync::atomic::Ordering::SeqCst);
        // 503, then the answer: the caller sees the answer.
        let (url, count) = fake_turns(vec![(503, "application/json", busy.clone()), (200, "text/event-stream", ok.clone())]).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let answered = answer(&chat, None, "say it", &Session::disabled()).await.expect("asked again");
        assert_eq!((answered.text.as_str(), sent(&count)), ("second time", 2));
        // 429 twice: one retry only, and the error says it was asked twice.
        let (url, count) = fake_turns(vec![(429, "application/json", busy.clone()), (429, "application/json", busy.clone())]).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let err = answer(&chat, None, "say it", &Session::disabled()).await.expect_err("busy twice");
        assert!(err.contains("429") && err.contains("asked twice"), "{err}");
        assert_eq!(sent(&count), 2);
        // A refusal is the caller's to fix: one request, never re-sent.
        let (url, count) = fake_turns(vec![(400, "application/json", busy.clone()), (200, "text/event-stream", ok.clone())]).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let err = answer(&chat, None, "say it", &Session::disabled()).await.expect_err("refused");
        assert!(err.contains("400") && !err.contains("asked twice"), "{err}");
        assert_eq!(sent(&count), 1);
        // The streamed ask paints as it goes, so it is never re-sent either.
        let (url, count) = fake_turns(vec![(503, "application/json", busy.clone()), (200, "text/event-stream", ok.clone())]).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        answer_with(&chat, None, "say it", &Session::disabled(), &mut |_| {}).await.expect_err("one try");
        assert_eq!(sent(&count), 1);
        // The panel's Test reports the request as it went: a 503 shows as a 503, one request.
        let (url, count) = fake_turns(vec![(503, "application/json", busy), (200, "text/event-stream", ok)]).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let err = ask(&chat, None, "say it", &Session::disabled()).await.expect_err("the Test shows the failure");
        assert!(err.contains("503") && !err.contains("asked twice"), "{err}");
        assert_eq!(sent(&count), 1);
    }

    #[tokio::test]
    async fn a_refusal_names_the_status_and_never_the_key() {
        std::env::set_var("SWISS_LLM_TEST_KEY_B", "k-secret-5678");
        let (url, _server) = fake_server(401, r#"{"error":{"message":"bad key k-secret-5678"}}"#.into()).await;
        let mut e = entry(Style::OpenAi, url);
        e.api_key = Some("${SWISS_LLM_TEST_KEY_B}".into());
        let chat = Chat::new(&e, None, 256).expect("chat");
        let err = ask(&chat, None, "x", &Session::disabled()).await.expect_err("refused");
        assert!(err.contains("401"), "{err}");
        assert!(!err.contains("k-secret-5678"), "{err}");
        // A reference that cannot resolve fails the build, naming the reference.
        let mut missing = entry(Style::OpenAi, "http://127.0.0.1:9".into());
        missing.api_key = Some("${SWISS_LLM_NO_SUCH_KEY}".into());
        let err = Chat::new(&missing, None, 256).err().expect("unresolved");
        assert!(err.contains("SWISS_LLM_NO_SUCH_KEY"), "{err}");
        // No model anywhere: said before any request.
        let mut bare = entry(Style::OpenAi, "http://127.0.0.1:9".into());
        bare.models[0].kind = ModelKind::Embedding;
        assert!(Chat::new(&bare, None, 256).err().expect("no model").contains("lists no chat model"));
    }

    fn openai_chunk(delta: Value, finish_reason: Value) -> Value {
        json!({
            "id": "chatcmpl-protocol-test", "object": "chat.completion.chunk", "model": "m-1",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        })
    }

    fn openai_frames(chunks: &[Value], done: bool) -> String {
        let mut frames: String = chunks.iter().map(|chunk| format!("data: {chunk}\n\n")).collect();
        if done {
            frames.push_str("data: [DONE]\n\n");
        }
        frames
    }

    #[tokio::test]
    async fn streamed_eof_requires_a_terminal_event() {
        let frames = openai_frames(&[openai_chunk(json!({ "content": "unfinished" }), Value::Null)], false);
        let (url, server) = fake_stream_server(200, frames).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let request = ChatRequest::default().append_message(ChatMessage::user("say it"));
        let mut observed = Vec::new();
        let reply = chat.exec_streamed(request, &mut |delta| observed.push(delta)).await;
        server.await.expect("server");
        assert!(observed.iter().any(|delta| matches!(delta, Delta::Text(text) if text == "unfinished")));
        let err = reply.err().expect("an incomplete stream is not a complete empty reply");
        assert_eq!(err.kind, "protocol", "{err:?}");
    }

    #[tokio::test]
    async fn streamed_http_errors_keep_status_and_retryability() {
        let mut observed = Vec::new();
        for status in [401, 403, 429, 503] {
            let (url, server) = fake_server(status, json!({ "error": { "message": "test refusal" } }).to_string()).await;
            let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
            let request = ChatRequest::default().append_message(ChatMessage::user("say it"));
            let err = chat.exec_streamed(request, &mut |_| {}).await.err().expect("HTTP refusal");
            server.await.expect("server");
            observed.push((err.status, err.kind, err.retryable));
        }
        assert_eq!(observed, [
            (Some(401), "http", false), (Some(403), "http", false),
            (Some(429), "http", true), (Some(503), "http", true)
        ]);
    }

    #[test]
    fn streamed_error_masks_short_nonempty_credentials() {
        let source = genai::Error::HttpError {
            status: reqwest::StatusCode::UNAUTHORIZED,
            canonical_reason: "Unauthorized".into(),
            body: json!({ "error": { "message": "bad credential abc" } }).to_string(),
        };
        let err = call_error(&source, &["abc".into()], false, 7);
        assert!(!err.message.contains("abc"), "{err:?}");
        assert!(err.message.contains("***"), "{err:?}");
        assert_eq!(err.duration_ms, 7);
    }

    #[test]
    fn streamed_error_masks_credentials_before_clipping() {
        let secret = "k-secret-crosses-the-clip-boundary";
        let detail = format!("{}{secret}", "x".repeat(295));
        let source = genai::Error::HttpError {
            status: reqwest::StatusCode::BAD_REQUEST,
            canonical_reason: "Bad Request".into(),
            body: json!({ "error": { "message": detail } }).to_string(),
        };
        let err = call_error(&source, &[secret.into()], false, 9);
        assert!(!err.message.contains("k-sec"), "even a clipped prefix must be masked: {err:?}");
        assert!(err.message.contains("***"), "{err:?}");
    }

    #[test]
    fn streamed_error_masks_decoded_escaped_credentials_before_clipping() {
        let secret = r#"k-"quoted\credential"#;
        let source = genai::Error::HttpError {
            status: reqwest::StatusCode::BAD_REQUEST,
            canonical_reason: "Bad Request".into(),
            body: json!({ "error": { "message": format!("{}{secret}", "x".repeat(295)) } }).to_string(),
        };
        let err = call_error(&source, &[secret.into()], false, 11);
        assert!(!err.message.contains("k-\"quo"), "a decoded secret prefix must be masked: {err:?}");
        assert!(err.message.contains("***"), "{err:?}");
    }

    #[test]
    fn recording_run_is_optional_and_survives_history_attachment() {
        let chat = Chat::new(&entry(Style::OpenAi, "http://127.0.0.1:9".into()), None, 256).expect("chat");
        assert_eq!(chat.history_run_id(), None);
        let chat = chat.recording_run(42).recording(None, Some("probe"));
        assert_eq!(chat.history_run_id(), Some(42));
        assert!(chat.history().is_none());
    }

    #[test]
    fn reasoning_replay_is_verified_target_and_tool_scoped() {
        let mut chat = Chat::new(&entry(Style::OpenAi, "http://127.0.0.1:9".into()), Some("deepseek-flash"), 256).expect("chat");
        let bare = ChatRequest::from_user("continue");
        let tools = bare.clone().with_tools([genai::chat::Tool::new("read")]);
        chat.model = "deepseek-flash".into();
        chat.provider = "deepseek".into();
        assert!(!chat.requires_reasoning_replay(&tools), "a label or local alias is not a capability");
        chat.endpoint = "https://api.deepseek.com/v1/".into();
        chat.provider = "arbitrary-label".into();
        for model in ["deepseek-flash", "deepseek-v4-pro", "deepseek-v4-flash", "deepseek-v4-flash-vision-exp"] {
            chat.model = model.into();
            chat.model_id = format!("openai::{model}");
            assert!(chat.requires_reasoning_replay(&tools), "{model}");
        }
        assert!(!chat.requires_reasoning_replay(&bare));
        assert!(!chat.requires_reasoning_replay(&bare.clone().with_tools(Vec::<genai::chat::Tool>::new())));
        chat.model = "deepseek-looking-unknown".into();
        chat.model_id = "openai::deepseek-looking-unknown".into();
        assert!(!chat.requires_reasoning_replay(&tools));
        chat.model = "deepseek-flash".into();
        chat.model_id = "openai::deepseek-flash".into();
        chat.endpoint = "https://api.deepseek.com.foreign.invalid/v1/".into();
        assert!(!chat.requires_reasoning_replay(&tools));
        chat.endpoint = "https://api.deepseek.com/anthropic/".into();
        chat.style = Style::Anthropic;
        assert!(!chat.requires_reasoning_replay(&tools), "native signed thinking is a different contract");
    }

    #[tokio::test]
    async fn openai_preflight_bytes_are_the_exact_replayed_wire() {
        use genai::chat::{Tool, ToolCall, ToolResponse};
        let frames = openai_frames(&[openai_chunk(json!({ "content": "complete" }), json!("stop"))], true);
        let (url, server) = fake_stream_server(200, frames).await;
        let chat = Chat::new(&entry(Style::OpenAi, format!("{url}/v4")), Some("openai::glm-5.3-flash-high"), 8192).expect("chat");
        let call = ToolCall { call_id: "call-exact-id".into(), fn_name: "read".into(),
            fn_arguments: json!({ "path": "你好\\file.txt", "count": 2 }), thought_signatures: None };
        let request = ChatRequest::from_system("system")
            .append_message(ChatMessage::user("first"))
            .append_message(ChatMessage::assistant(MessageContent::from_parts(vec![
                ContentPart::Text("one".into()), ContentPart::Text("two".into()),
                ContentPart::ReasoningContent("think one".into()), ContentPart::ReasoningContent("think two".into()),
                ContentPart::ToolCall(call.clone()),
            ])))
            .append_message(ChatMessage::from(ToolResponse::new("call-exact-id", "tool result")))
            .append_message(ChatMessage::user("continue"))
            .with_tools([Tool::new("read").with_description("Read the file")
                .with_schema(json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })).with_strict(true)]);
        let prepared = chat.request_body(&request).expect("preflight");
        assert!(!chat.requires_reasoning_replay(&request), "a loopback capture cannot promise official replay capability");
        let reply = tokio::time::timeout(Duration::from_secs(5), chat.exec_streamed(request, &mut |_| {})).await.expect("deadline").expect("reply");
        assert_eq!(reply.request_id, "chatcmpl-protocol-test");
        assert_eq!(reply.response.content.first_text(), Some("complete"));
        let (head, body) = tokio::time::timeout(Duration::from_secs(5), server).await.expect("server deadline").expect("server");
        assert!(head.starts_with("POST /v4/chat/completions"), "{head}");
        assert!(head.to_ascii_lowercase().contains("authorization: bearer"), "keyless compatibility header");
        assert_eq!(body.as_bytes(), prepared.as_slice(), "preflight counts the actual serialized bytes, not the canonical request");
        let sent: Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(sent["model"], "glm-5.3-flash");
        assert_eq!(sent["reasoning_effort"], "high");
        assert_eq!(sent["max_tokens"], 8192);
        assert_eq!(sent["stream_options"]["include_usage"], true);
        assert_eq!(sent["messages"][2]["content"], "one\n\ntwo");
        assert_eq!(sent["messages"][2]["reasoning_content"], "think one\nthink two");
        assert_eq!(sent["messages"][2]["tool_calls"][0]["id"], call.call_id);
        assert_eq!(sent["messages"][2]["tool_calls"][0]["function"]["arguments"], call.fn_arguments.to_string());
        assert_eq!(sent["messages"][3]["tool_call_id"], "call-exact-id");
        assert_eq!(sent["messages"][3]["content"], "tool result");
        assert_eq!(sent["tools"][0]["function"]["parameters"]["additionalProperties"], false);
        assert!(sent.get("authorization").is_none());
        assert!(sent.get("apiKey").is_none());
        assert!(sent.get("extra_headers").is_none());
    }

    #[test]
    fn openai_preflight_normalizes_model_options_without_network() {
        for (configured, wire, effort, token_key) in [
            ("openai::gpt-5-high", "gpt-5", Some("high"), "max_completion_tokens"),
            ("o3-mini", "o3-mini", None, "max_completion_tokens"),
            ("gpt-6.1-terra", "gpt-6.1-terra", None, "max_completion_tokens"),
            ("gpt-6-astra-low", "gpt-6-astra", Some("low"), "max_completion_tokens"),
            ("gpt-4o-mini", "gpt-4o-mini", None, "max_tokens"),
            ("gpt-oss-120b", "gpt-oss-120b", None, "max_tokens"),
            // A trailing -max is the model's tier, never an effort: the id goes out whole.
            ("qwen3.8-max", "qwen3.8-max", None, "max_tokens"),
            ("gpt-5.1-codex-max", "gpt-5.1-codex-max", None, "max_completion_tokens"),
            ("deepseek-flash-low", "deepseek-flash", Some("low"), "max_tokens"),
            ("glm-5.3-flash", "glm-5.3-flash", None, "max_tokens"),
        ] {
            let chat = Chat::new(&entry(Style::OpenAi, "http://127.0.0.1:9/v1".into()), Some(configured), 4096).expect("chat");
            let body = chat.request_body(&ChatRequest::from_user("test")).expect("body");
            let sent: Value = serde_json::from_slice(&body).expect("JSON");
            assert_eq!(sent["model"], wire, "{configured}");
            assert_eq!(sent["reasoning_effort"].as_str(), effort, "{configured}");
            assert_eq!(sent[token_key], 4096, "{configured}");
            assert_eq!(sent["stream"], true);
            assert_eq!(sent["stream_options"]["include_usage"], true);
        }
        // A model's stated ceiling caps the agent's, and a model that takes no effort is sent
        // none, on either wire; a model listing nothing keeps both as asked.
        for style in [Style::OpenAi, Style::Anthropic] {
            let mut declared = entry(style, "http://127.0.0.1:9/v1".into());
            declared.models = vec![
                ModelEntry { max_output: Some(8192), thinking: false, ..ModelEntry::new("small", ModelKind::Chat) },
                ModelEntry { max_output: Some(65536), context_window: Some(131072), ..ModelEntry::new("roomy", ModelKind::Chat) },
            ];
            let ask = |model: &str, tokens: u32| {
                let chat = Chat::new(&declared, Some(model), tokens).expect("chat").with_effort(Some(genai::chat::ReasoningEffort::High));
                let sent: Value = serde_json::from_slice(&chat.request_body(&ChatRequest::from_user("test")).expect("body")).expect("JSON");
                (sent, chat.context_window())
            };
            let (small, window) = ask("small", 131072);
            assert_eq!((small["max_tokens"].as_u64(), window), (Some(8192), None), "{style:?}: {small}");
            assert!(small.get("reasoning_effort").is_none() && small.get("thinking").is_none() && small.get("output_config").is_none(), "{style:?}: {small}");
            let (roomy, window) = ask("roomy", 4096);
            assert_eq!((roomy["max_tokens"].as_u64(), window), (Some(4096), Some(131072)), "{style:?}: under the ceiling the agent's own stands");
            let thought = roomy.get("reasoning_effort").or_else(|| roomy.get("output_config")).is_some();
            assert!(thought, "{style:?}: a thinking model keeps the effort: {roomy}");
        }
        let native = Chat::new(&entry(Style::Anthropic, "http://127.0.0.1:9".into()), None, 256).expect("native");
        let body: Value = serde_json::from_slice(&native.request_body(&ChatRequest::from_user("test")).expect("anthropic")).expect("JSON");
        assert_eq!((body["model"].as_str(), body["max_tokens"].as_u64()), (Some("m-1"), Some(256)));
        assert!(body.get("stream_options").is_none() && body.get("reasoning_effort").is_none(), "{body}");
    }

    #[test]
    fn reasoning_replay_uses_exact_normalized_wire_model() {
        let bare = ChatRequest::from_user("continue");
        let tools = bare.clone().with_tools([genai::chat::Tool::new("read")]);
        for (style, host, model) in [
            (Style::OpenAi, "http://127.0.0.1:9/v1", "deepseek-flash-high"),
            (Style::OpenAi, "https://api.deepseek.com.foreign.invalid/v1", "openai::deepseek-flash"),
            (Style::Anthropic, "https://api.deepseek.com", "deepseek-flash-high"),
            (Style::OpenAi, "https://api.deepseek.com", "openai::deepseek-looking-unknown-high"),
        ] {
            let chat = Chat::new(&entry(style, host.into()), Some(model), 256).expect("pure policy");
            assert!(!chat.requires_reasoning_replay(&tools), "negative control {host} {model}");
        }
        let mut observed = Vec::new();
        for configured in ["openai::deepseek-flash", "deepseek-flash-high", "openai::deepseek-v4-pro-low"] {
            // Pure preflight only. No official endpoint is requested by this test.
            let chat = Chat::new(&entry(Style::OpenAi, "https://api.deepseek.com/v1".into()), Some(configured), 256).expect("pure policy");
            let body = chat.request_body(&tools).expect("preflight");
            let payload: Value = serde_json::from_slice(&body).expect("JSON");
            assert!(matches!(payload["model"].as_str(), Some("deepseek-flash" | "deepseek-v4-pro")));
            assert!(!chat.requires_reasoning_replay(&bare));
            assert!(!chat.requires_reasoning_replay(&bare.clone().with_tools(Vec::<genai::chat::Tool>::new())));
            observed.push(chat.requires_reasoning_replay(&tools));
        }
        assert_eq!(observed, [true, true, true], "capability must compare exact wire ids after public normalization");
    }

    #[test]
    fn reasoning_replay_follows_wire_identity_not_public_labels() {
        let tools = ChatRequest::from_user("continue").with_tools([genai::chat::Tool::new("read")]);
        let official = entry(Style::OpenAi, "https://api.deepseek.com/v1".into());
        let mut unknown = Chat::new(&official, Some("unsupported-wire-model"), 256).expect("pure policy");
        unknown.model = "deepseek-flash".into();
        unknown.provider = "deepseek".into();
        assert!(!unknown.requires_reasoning_replay(&tools), "public labels do not replace the configured wire id");
        let mut configured = Chat::new(&official, Some("deepseek-flash-high"), 256).expect("pure policy");
        assert!(configured.requires_reasoning_replay(&tools));
        configured.options.reasoning_effort = Some(genai::chat::ReasoningEffort::Low);
        let body = configured.request_body(&tools).expect("preflight");
        assert_eq!(serde_json::from_slice::<Value>(&body).expect("JSON")["model"], "deepseek-flash-high");
        assert!(!configured.requires_reasoning_replay(&tools), "an explicit option preserves the unsupported whole wire id");
    }

    #[tokio::test]
    async fn streamed_provider_errors_mask_decoded_messages_not_raw_frames() {
        let key = "k-escaped-stream-credential";
        std::env::set_var("SWISS_LLM_TEST_STREAM_MASK_KEY", key);
        let detail = format!("{}{}", "x".repeat(295), key);
        let escaped = json!({ "error": { "message": detail } }).to_string().replace(key, "k-escaped-stream-cred\\u0065ntial");
        for frames in [format!("data: {escaped}\n\n"), format!("data: malformed {key}\n\n")] {
            let (url, server) = fake_stream_server(200, frames).await;
            let mut e = entry(Style::OpenAi, url);
            e.api_key = Some("${SWISS_LLM_TEST_STREAM_MASK_KEY}".into());
            let chat = Chat::new(&e, None, 256).expect("chat");
            let err = tokio::time::timeout(Duration::from_secs(5), chat.exec_streamed(ChatRequest::from_user("test"), &mut |_| {}))
                .await.expect("deadline").err().expect("protocol error");
            tokio::time::timeout(Duration::from_secs(5), server).await.expect("server deadline").expect("server");
            assert_eq!((err.status, err.kind, err.retryable), (None, "protocol", false));
            assert!(!err.message.contains("k-escaped-stream-cred"), "{err:?}");
            assert!(!err.message.contains("data:"), "offending SSE lines are never included");
        }
    }

    #[tokio::test]
    async fn streamed_async_sink_false_discards_completed_reply() {
        let openai = openai_frames(&[openai_chunk(json!({ "content": "one" }), Value::Null),
            openai_chunk(json!({ "content": "two" }), json!("stop"))], true);
        let native = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"one\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"two\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        ).to_string();
        for (style, frames) in [(Style::OpenAi, openai), (Style::Anthropic, native)] {
            let (url, server) = fake_stream_server(200, frames).await;
            let chat = Chat::new(&entry(style, url), None, 256).expect("chat");
            let mut observed = Vec::new();
            let mut sink = |delta| { observed.push(delta); std::future::ready(false) };
            let reply = tokio::time::timeout(Duration::from_secs(5), chat.exec_streamed_async(ChatRequest::from_user("test"), &mut sink))
                .await.expect("deadline").expect("cancelled delivery");
            tokio::time::timeout(Duration::from_secs(5), server).await.expect("server deadline").expect("server");
            assert!(reply.is_none());
            assert_eq!(observed.len(), 1, "no later callback or complete reply after false");
            assert!(matches!(&observed[0], Delta::Text(text) if text == "one"));
        }
    }

    #[tokio::test]
    async fn streamed_parallel_tool_calls_survive_one_delta() {
        let calls = json!([
            { "index": 0, "id": "call-a", "type": "function", "function": { "name": "read", "arguments": "{\"p\":\"a\"}" } },
            { "index": 1, "id": "call-b", "type": "function", "function": { "name": "sync", "arguments": "{\"p\":\"b\"}" } }
        ]);
        let frames = openai_frames(&[
            openai_chunk(json!({ "tool_calls": calls }), Value::Null),
            openai_chunk(json!({}), json!("tool_calls"))
        ], true);
        let (url, server) = fake_stream_server(200, frames).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let schema = json!({ "type": "object", "properties": { "p": { "type": "string" } }, "required": ["p"] });
        let request = ChatRequest::default().append_message(ChatMessage::user("read and sync")).with_tools([
            genai::chat::Tool::new("read").with_schema(schema.clone()),
            genai::chat::Tool::new("sync").with_schema(schema)
        ]);
        let reply = chat.exec_streamed(request, &mut |_| {}).await.expect("reply");
        server.await.expect("server");
        let calls = reply.response.tool_calls();
        assert_eq!(calls.len(), 2, "both calls from one SSE delta survive");
        assert_eq!((calls[0].call_id.as_str(), calls[0].fn_name.as_str()), ("call-a", "read"));
        assert_eq!((calls[1].call_id.as_str(), calls[1].fn_name.as_str()), ("call-b", "sync"));
        assert_eq!(calls[0].fn_arguments, json!({ "p": "a" }));
        assert_eq!(calls[1].fn_arguments, json!({ "p": "b" }));
    }

    #[tokio::test]
    async fn streamed_cut_finish_never_returns_executable_calls() {
        let frames = openai_frames(&[
            openai_chunk(json!({ "content": "partial answer" }), Value::Null),
            openai_chunk(json!({ "tool_calls": [{ "index": 0, "id": "call-a", "type": "function", "function": { "name": "read", "arguments": "{}" } }] }), Value::Null),
            openai_chunk(json!({}), json!("length"))
        ], true);
        let (url, server) = fake_stream_server(200, frames).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let request = ChatRequest::default().append_message(ChatMessage::user("read")).with_tools([
            genai::chat::Tool::new("read").with_schema(json!({ "type": "object", "properties": {} }))
        ]);
        let reply = chat.exec_streamed(request, &mut |_| {}).await.expect("cut reply");
        server.await.expect("server");
        assert!(matches!(reply.response.stop_reason, Some(StopReason::MaxTokens(_))));
        assert_eq!(reply.response.texts().join(""), "partial answer");
        assert!(reply.response.tool_calls().is_empty(), "a token-cut turn cannot dispatch tools");
    }

    async fn protocol_exec(frames: String, request: ChatRequest) -> Result<Reply, CallError> {
        let (url, server) = fake_stream_server(200, frames).await;
        let chat = Chat::new(&entry(Style::OpenAi, url), None, 256).expect("chat");
        let result = tokio::time::timeout(Duration::from_secs(5), chat.exec_streamed(request, &mut |_| {}))
            .await.expect("a finite protocol fixture terminates");
        tokio::time::timeout(Duration::from_secs(5), server).await.expect("server terminates").expect("server");
        result
    }

    #[tokio::test]
    async fn protocol_rejects_unframed_done() {
        let mut observed = Vec::new();
        for tail in ["data: [DONE]", "data: [DONE]\n", "data: [DONE]\r\n"] {
            let mut frames = openai_frames(&[openai_chunk(json!({ "content": "answer" }), json!("stop"))], false);
            frames.push_str(tail);
            let result = protocol_exec(frames, ChatRequest::from_user("say it")).await;
            observed.push(result.err().map(|err| err.kind));
        }
        assert_eq!(observed, vec![Some("protocol"); 3], "an EOF tail is not a framed terminal event");
    }

    #[tokio::test]
    async fn protocol_requires_valid_finish_reason() {
        let mut cases: Vec<Value> = [Value::Null, json!(17), json!(true), json!([]), json!(""), json!("unknown")]
            .into_iter().map(|finish| openai_chunk(json!({ "content": "answer" }), finish)).collect();
        let mut missing = openai_chunk(json!({ "content": "answer" }), Value::Null);
        missing["choices"][0].as_object_mut().expect("choice").remove("finish_reason");
        cases.push(missing);
        let mut observed = Vec::new();
        for chunk in cases {
            let result = protocol_exec(openai_frames(&[chunk], true), ChatRequest::from_user("say it")).await;
            observed.push(result.err().map(|err| err.kind));
        }
        assert_eq!(observed, vec![Some("protocol"); 7], "DONE does not validate a missing, mistyped or unknown finish");
    }

    #[tokio::test]
    async fn protocol_rejects_interrupted_finish_reason() {
        let mut observed = Vec::new();
        for finish in ["aborted", "insufficient_system_resource", "unknown"] {
            let frames = openai_frames(&[
                openai_chunk(json!({ "tool_calls": [{ "index": 0, "id": "call-a", "type": "function", "function": { "name": "read", "arguments": "{}" } }] }), Value::Null),
                openai_chunk(json!({}), json!(finish))
            ], true);
            let request = ChatRequest::from_user("read").with_tools([
                genai::chat::Tool::new("read").with_schema(json!({ "type": "object", "properties": {} }))
            ]);
            let result = protocol_exec(frames, request).await;
            observed.push(match result {
                Ok(reply) => (None, reply.response.tool_calls().len()),
                Err(err) => (Some(err.kind), 0),
            });
        }
        assert_eq!(observed, vec![(Some("protocol"), 0); 3], "an interrupted answer cannot dispatch captured calls");
    }

    // Large refusal fixtures permit the client to drop the socket at the header budget.
    // Their finite body and join deadlines do not rely on TCP write/chunk correspondence.
    async fn fake_budget_server(status: u16, body: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 { return; }
                received.extend_from_slice(&chunk[..n]);
                let Some(head_end) = received.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&received[..head_end]);
                let length = head.lines().find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|line| line.split(':').nth(1)).and_then(|value| value.trim().parse::<usize>().ok()).unwrap_or(0);
                if received.len() < head_end + 4 + length { continue; }
                let header = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                if socket.write_all(header.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(body.as_bytes()).await;
                }
                return;
            }
        });
        (url, task)
    }

    fn padded_json(mut body: String, size: usize) -> String {
        assert!(body.len() <= size);
        body.push_str(&" ".repeat(size - body.len()));
        body
    }

    #[tokio::test]
    async fn protocol_bounds_stream_error_body() {
        const LIMIT: usize = 64 * 1024;
        let secret = "k-stream-body-limit-secret";
        std::env::set_var("SWISS_LLM_TEST_STREAM_BODY_LIMIT_KEY", secret);
        let mut observed = Vec::new();
        for status in [401, 503] {
            for size in [LIMIT, LIMIT + 1] {
                let body = padded_json(json!({ "error": { "message": format!("{}{secret}", "x".repeat(295)) } }).to_string(), size);
                let (url, server) = fake_budget_server(status, body).await;
                let mut e = entry(Style::OpenAi, url);
                e.api_key = Some("${SWISS_LLM_TEST_STREAM_BODY_LIMIT_KEY}".into());
                let chat = Chat::new(&e, None, 256).expect("chat");
                let result = tokio::time::timeout(Duration::from_secs(5), chat.exec_streamed(ChatRequest::from_user("say it"), &mut |_| {}))
                    .await.expect("a finite refusal terminates");
                tokio::time::timeout(Duration::from_secs(5), server).await.expect("server terminates").expect("server");
                let err = result.err().expect("HTTP refusal");
                let safe_detail = if size == LIMIT {
                    err.message.contains("***") && !err.message.contains("k-str")
                } else {
                    err.message == format!("the provider answered {status}: the error body exceeded {LIMIT} bytes")
                };
                observed.push((err.status, err.kind, err.retryable, safe_detail));
            }
        }
        assert_eq!(observed, [
            (Some(401), "http", false, true), (Some(401), "http", false, true),
            (Some(503), "http", true, true), (Some(503), "http", true, true)
        ], "L is decoded and masked; L+1 is a fixed refusal, not an untrusted prefix");
    }

    #[tokio::test]
    async fn protocol_bounds_discovery_body() {
        const LIMIT: usize = 1024 * 1024;
        let mut observed = Vec::new();
        for size in [LIMIT, LIMIT + 1] {
            let (url, server) = fake_budget_server(200, padded_json(json!({ "data": [] }).to_string(), size)).await;
            let result = tokio::time::timeout(Duration::from_secs(5), discover(&entry(Style::OpenAi, url)))
                .await.expect("a finite listing terminates");
            tokio::time::timeout(Duration::from_secs(5), server).await.expect("server terminates").expect("server");
            observed.push(match result {
                Ok(models) => (models.is_empty(), None),
                Err(message) => (false, Some(message)),
            });
        }
        assert_eq!(observed, [
            (true, None), (false, Some(format!("the provider's model list exceeded {LIMIT} bytes")))
        ], "valid JSON at L succeeds; valid JSON at L+1 exceeds the raw body budget");
    }

    #[tokio::test]
    async fn protocol_bounds_discovery_error_body() {
        const LIMIT: usize = 64 * 1024;
        let secret = r#"k-"discovery\body-limit"#;
        std::env::set_var("SWISS_LLM_TEST_DISCOVERY_BODY_LIMIT_KEY", secret);
        let mut observed = Vec::new();
        for status in [401, 503] {
            for size in [LIMIT, LIMIT + 1] {
                let body = padded_json(json!({ "error": { "message": format!("{}{secret}", "x".repeat(295)) } }).to_string(), size);
                let (url, server) = fake_budget_server(status, body).await;
                let mut e = entry(Style::OpenAi, url);
                e.api_key = Some("${SWISS_LLM_TEST_DISCOVERY_BODY_LIMIT_KEY}".into());
                let result = tokio::time::timeout(Duration::from_secs(5), discover(&e))
                    .await.expect("a finite listing refusal terminates");
                tokio::time::timeout(Duration::from_secs(5), server).await.expect("server terminates").expect("server");
                let message = result.expect_err("HTTP refusal");
                observed.push(if size == LIMIT {
                    message.contains(&status.to_string()) && message.contains("***") && !message.contains("k-\"dis")
                } else {
                    message == format!("the provider answered {status}: the error body exceeded {LIMIT} bytes")
                });
            }
        }
        assert_eq!(observed, vec![true; 4], "discovery shares the decoded error-body limit and masking order");
    }

    #[test]
    fn a_consumer_picks_a_listed_model_of_its_kind() {
        let parsed = parse(json!({ "o": { "style": "openai", "baseUrl": "http://h/v1", "models": [
            { "id": "chat-a" }, { "id": "chat-b" }, { "id": "emb", "kind": "embedding" }
        ] } }))
        .expect("providers");
        let o = &parsed["o"];
        assert_eq!(o.pick(None, ModelKind::Chat, "agents.a.model").expect("default"), "chat-a");
        assert_eq!(o.pick(Some("chat-b"), ModelKind::Chat, "agents.a.model").expect("named"), "chat-b");
        assert_eq!(o.pick(None, ModelKind::Embedding, "embedding.model").expect("default"), "emb");
        let err = o.pick(Some("emb"), ModelKind::Chat, "agents.a.model").expect_err("wrong kind");
        assert!(err.contains("\"emb\" is an embedding model on providers.o, not a chat model"), "{err}");
        let err = o.pick(Some("gpt-9"), ModelKind::Chat, "agents.a.model").expect_err("unlisted");
        assert!(err.starts_with("agents.a.model: providers.o does not list \"gpt-9\"") && err.contains("chat-a, chat-b"), "{err}");
        let mut bare = o.clone();
        bare.models.clear();
        assert!(bare.pick(None, ModelKind::Chat, "agents.a.model").expect_err("none").contains("lists no chat model to default to"));
    }

    #[tokio::test]
    async fn discover_reads_the_listing_and_guesses_the_kind() {
        std::env::set_var("SWISS_LLM_TEST_KEY_D", "k-discover-9012");
        let (url, server) = fake_server(
            200,
            json!({ "object": "list", "data": [
                { "id": "qwen3.5:4b" }, { "id": "qwen3-embedding:0.6b" }, { "id": "bge-m3" }, { "id": "qwen3.5:4b" }, { "x": 1 }
            ] })
            .to_string(),
        )
        .await;
        let mut e = entry(Style::OpenAi, format!("{url}/v1"));
        e.api_key = Some("${SWISS_LLM_TEST_KEY_D}".into());
        let found = discover(&e).await.expect("listing");
        let got: Vec<(&str, ModelKind)> = found.iter().map(|m| (m.id.as_str(), m.kind)).collect();
        assert_eq!(got, [("qwen3.5:4b", ModelKind::Chat), ("qwen3-embedding:0.6b", ModelKind::Embedding), ("bge-m3", ModelKind::Embedding)]);
        let (head, _) = server.await.expect("server");
        assert!(head.starts_with("GET /v1/models"), "{head}");
        assert!(head.to_ascii_lowercase().contains("authorization: bearer k-discover-9012"), "{head}");
        // A refusal says the status and never the key.
        let (url, _server) = fake_server(404, r#"{"error":{"message":"no route for k-discover-9012"}}"#.into()).await;
        let mut e = entry(Style::Anthropic, url);
        e.api_key = Some("${SWISS_LLM_TEST_KEY_D}".into());
        let err = discover(&e).await.expect_err("404");
        assert!(err.contains("404") && !err.contains("k-discover-9012"), "{err}");
    }
}
