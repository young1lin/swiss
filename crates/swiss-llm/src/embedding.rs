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

//! The embedding tier's endpoint (SPEC §llm.embedding): a provider by name plus one of the
//! embedding models it lists, spoken to as OpenAI's `POST {baseUrl}/embeddings` - a hosted
//! API, a relay, or a local server holding BGE-M3 or a MiniLM.
//!
//! The block names its provider instead of restating an endpoint, so a local Ollama that
//! chats and embeds is configured once. The resolved [EmbeddingConfig] carries everything
//! the client needs; [EmbedClient] is the HTTP half - vectors in the provider's order,
//! errors scrubbed of the key. What a vector MEANS (normalization, the store's width check,
//! the similarity floor) is the consumer's: memory wraps the client in its own Embedder.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{causes, object, only, raw_text, scrubbed_detail_many, scrubbed_many, text, whole};
use crate::provider::{ModelKind, ProviderEntry, Style};

/// Texts per request when the block names none, and the ceiling.
pub const BATCH_DEFAULT: usize = 16;
pub const BATCH_MAX: usize = 256;
/// The per-request timeout when the block names none, and its bounds.
pub const TIMEOUT_DEFAULT_MS: u64 = 30_000;
pub const TIMEOUT_MIN_MS: u64 = 1_000;
pub const TIMEOUT_MAX_MS: u64 = 120_000;
/// The widest vector the tier accepts.
pub const DIMS_MAX: usize = 8_192;
const PREFIX_MAX: usize = 200;
/// The keys an `embedding` block may carry.
const KEYS: [&str; 8] = [
    "provider", "model", "dimensions", "batch", "timeoutMs", "queryPrefix", "documentPrefix", "minSimilarity",
];

/// The checked `embedding` block, resolved against its provider.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingConfig {
    /// The providers entry this block embeds through.
    pub provider: String,
    /// The provider's base URL, without a trailing slash.
    pub base_url: String,
    /// The provider's `${...}` reference, never a value.
    pub api_key: Option<String>,
    pub proxy: Option<String>,
    pub model: String,
    pub dimensions: Option<usize>,
    pub batch: usize,
    pub timeout_ms: u64,
    pub query_prefix: String,
    pub document_prefix: String,
    /// The least cosine that counts as close in meaning for this model; None keeps the
    /// consumer's default. Query-time only: not in the fingerprint.
    pub min_similarity: Option<f64>,
}

/// The `embedding` block: None when absent or `provider` is "off"/blank.
pub(crate) fn parse(block: Option<&Value>, providers: &BTreeMap<String, ProviderEntry>) -> Result<Option<EmbeddingConfig>, String> {
    let Some(block) = block.filter(|b| !b.is_null()) else { return Ok(None) };
    let place = "embedding";
    let obj = object(block, place)?;
    only(obj, place, &KEYS)?;
    let Some(name) = text(obj, place, "provider")?.filter(|n| n != "off") else { return Ok(None) };
    let entry = providers.get(&name).ok_or_else(|| {
        format!("embedding.provider names {name:?}, which the providers block does not define")
    })?;
    if entry.style == Style::Anthropic {
        return Err(format!(
            "embedding.provider names {name:?}, an anthropic-style provider; embeddings speak OpenAI's /embeddings wire - add an openai-style entry for that endpoint"
        ));
    }
    // One of the provider's embedding models; without a name, its first.
    let model = entry.pick(text(obj, place, "model")?.as_deref(), ModelKind::Embedding, "embedding.model")?;
    let dimensions = whole(obj, place, "dimensions", 1, DIMS_MAX as u64)?.map(|n| n as usize);
    let batch = whole(obj, place, "batch", 1, BATCH_MAX as u64)?.map(|n| n as usize).unwrap_or(BATCH_DEFAULT);
    let timeout_ms = whole(obj, place, "timeoutMs", TIMEOUT_MIN_MS, TIMEOUT_MAX_MS)?.unwrap_or(TIMEOUT_DEFAULT_MS);
    let prefix = |key: &str| -> Result<String, String> {
        let value = raw_text(obj, place, key)?.unwrap_or_default();
        if value.chars().count() > PREFIX_MAX {
            return Err(format!("embedding.{key}: at most {PREFIX_MAX} characters"));
        }
        Ok(value)
    };
    let min_similarity = match obj.get("minSimilarity") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_f64() {
            Some(floor) if (0.0..=1.0).contains(&floor) => Some(floor),
            _ => return Err("embedding.minSimilarity must be a number in 0..=1 (the least cosine that counts as close in meaning)".into()),
        },
    };
    Ok(Some(EmbeddingConfig {
        provider: name,
        base_url: entry.base_url.clone(),
        api_key: entry.api_key.clone(),
        proxy: entry.proxy.clone(),
        model,
        dimensions,
        batch,
        timeout_ms,
        query_prefix: prefix("queryPrefix")?,
        document_prefix: prefix("documentPrefix")?,
        min_similarity,
    }))
}

impl EmbeddingConfig {
    /// Whether text leaves this machine: the endpoint is not a loopback host.
    pub fn remote(&self) -> bool {
        match reqwest::Url::parse(&self.base_url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) {
            Some(host) => !(host == "localhost" || host == "[::1]" || host.starts_with("127.")),
            None => false,
        }
    }

    /// The stored vectors' fingerprint: the wire, model, requested width and endpoint,
    /// plus the document prefix when there is one (it changes every stored vector). A model
    /// name is no identity (two endpoints may serve other weights or widths under one
    /// name), so a moved endpoint re-embeds once (ADR-032). The endpoint enters as a hash of
    /// its normalized URL, never as the host; the provider's NAME, the proxy and the query
    /// prefix do not enter: none of them changes a stored vector.
    pub fn fingerprint(&self) -> String {
        let dims = self.dimensions.map(|d| d.to_string()).unwrap_or_else(|| "native".into());
        let mut id = format!("openai:{}:{dims}", self.model);
        if let Ok(url) = reqwest::Url::parse(&self.base_url) {
            id.push_str(&format!(":ep-{}", short_hash(url.as_str().trim_end_matches('/'))));
        }
        if !self.document_prefix.is_empty() {
            id.push_str(&format!(":doc-{}", short_hash(&self.document_prefix)));
        }
        id
    }

    /// The block in force as the admin API shows it (the key never appears: it lives on
    /// the provider, as a reference).
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "provider": self.provider,
            "model": self.model,
            "batch": self.batch,
            "timeoutMs": self.timeout_ms,
            "queryPrefix": self.query_prefix,
            "documentPrefix": self.document_prefix,
            "remote": self.remote(),
        });
        if let Some(dimensions) = self.dimensions {
            out["dimensions"] = json!(dimensions);
        }
        if let Some(floor) = self.min_similarity {
            out["minSimilarity"] = json!(floor);
        }
        out
    }
}

/// The first 8 hex characters of a text's SHA-256: enough to tell configurations apart
/// without spelling them out.
fn short_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
    swiss_core::util::to_hex(&digest)[..8].to_string()
}

/// The OpenAI-compatible embedding client for one resolved block. With a
/// [history](EmbedClient::recording) attached, every round trip files itself into
/// logs/llm.db (SPEC llm.history).
pub struct EmbedClient {
    client: reqwest::Client,
    endpoint: String,
    provider: String,
    model: String,
    api_key: Option<String>,
    // Proxy credentials belong to this client; the provider key resolves per call.
    proxy_secrets: Vec<String>,
    dimensions: Option<usize>,
    timeout: Duration,
    /// The llm state the history store is resolved through per call (see
    /// [recording]); None when this client records nothing.
    state: Option<std::sync::Arc<crate::state::LlmState>>,
    /// The consumer name embedding rows file under (None = a panel test).
    agent: Option<String>,
}

impl EmbedClient {
    /// Err only when the HTTP client cannot be built (the proxy, the TLS backend).
    pub fn build(config: &EmbeddingConfig) -> Result<EmbedClient, String> {
        let place = format!("providers.{}.proxy", config.provider);
        let (client, proxy_secrets) = crate::config::client_with(config.proxy.as_deref(), &place, reqwest::Client::builder())?;
        Ok(EmbedClient {
            client,
            endpoint: format!("{}/embeddings", config.base_url),
            provider: config.provider.clone(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            proxy_secrets,
            dimensions: config.dimensions,
            timeout: Duration::from_millis(config.timeout_ms),
            state: None,
            agent: None,
        })
    }

    /// Attach the state the history is read through, and the consumer name the
    /// rows file under: every embed round trip records itself. The store is
    /// resolved PER CALL, not captured here - the llm plugin opens its database
    /// on start, which can land after this client was built (memory plugs before
    /// llm boots), so a captured store would stay None forever.
    pub fn recording(mut self, state: Option<std::sync::Arc<crate::state::LlmState>>, agent: Option<&str>) -> EmbedClient {
        self.state = state;
        self.agent = agent.map(str::to_string);
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// One request: the texts as given (any prefix is already on them), the vectors in the
    /// texts' order. Errors are scrubbed of the resolved key and never quote the URL. With
    /// a history attached the round trip records itself (SPEC llm.history): the texts as
    /// asked, the answer's width and every vector rounded to six decimals - the whole
    /// answer stays inspectable (drift, dead dimensions, sign flips).
    pub async fn embed(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        if input.is_empty() {
            return Ok(Vec::new());
        }
        let started = std::time::Instant::now();
        let outcome = self.embed_inner(input.clone()).await;
        let history = self.state.as_ref().and_then(|state| state.history());
        if let Some(history) = history {
            let ms = started.elapsed().as_millis() as u64;
            let recorded = match &outcome {
                Ok(vectors) => Ok(vectors.as_slice()),
                Err(err) => Err(err.as_str()),
            };
            history.record_embedding(crate::history::EmbedCall {
                provider: &self.provider,
                model: &self.model,
                agent: self.agent.as_deref(),
                texts: &input,
                ms,
                outcome: recorded,
            });
        }
        outcome
    }

    async fn embed_inner(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        let count = input.len();
        let mut body = json!({ "model": self.model, "input": input });
        if let Some(dimensions) = self.dimensions {
            body["dimensions"] = json!(dimensions);
        }
        // The key resolves per request, so a vault edit takes effect on the next call.
        let key = match &self.api_key {
            None => String::new(),
            Some(reference) => swiss_core::secure::refs::resolve(reference).map_err(|err| format!("the embedding key: {err}"))?,
        };
        let mut request = self.client.post(&self.endpoint).timeout(self.timeout).json(&body);
        if !key.is_empty() {
            request = request.bearer_auth(&key);
        }
        let mut secrets = self.proxy_secrets.clone();
        if !key.is_empty() { secrets.push(key.clone()); }
        let scrub = |message: String| scrubbed_many(&message, &secrets);
        let response = request
            .send()
            .await
            .map_err(|err| scrub(format!("the embedding request failed: {}", causes(&err.without_url()))))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| scrub(format!("the embedding answer could not be read: {}", causes(&err.without_url()))))?;
        if !status.is_success() {
            let mut message = format!("the embedding provider answered {status}: {}", scrubbed_detail_many(&bytes, &secrets));
            if matches!(status.as_u16(), 401 | 403) && key.is_empty() {
                message.push_str(" (no apiKey is configured, or its reference resolved to an empty value)");
            }
            return Err(message);
        }
        vectors_of(&bytes, count).map_err(scrub)
    }
}

/// `data[].embedding` in `index` order (array order when a server omits `index`).
fn vectors_of(bytes: &[u8], count: usize) -> Result<Vec<Vec<f32>>, String> {
    let body: Value = serde_json::from_slice(bytes).map_err(|_| "the embedding provider's answer is not JSON".to_string())?;
    let data = body.get("data").and_then(Value::as_array).ok_or("the embedding provider's answer has no data array")?;
    if data.len() != count {
        return Err(format!("the provider returned {} vectors for {count} texts", data.len()));
    }
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; count];
    for (position, item) in data.iter().enumerate() {
        let index = match item.get("index") {
            None | Some(Value::Null) => position,
            Some(v) => v
                .as_u64()
                .map(|n| n as usize)
                .filter(|n| *n < count)
                .ok_or_else(|| format!("the provider returned index {v} for {count} texts"))?,
        };
        let numbers = item.get("embedding").and_then(Value::as_array).ok_or("a data item has no embedding array")?;
        if numbers.is_empty() || numbers.len() > DIMS_MAX {
            return Err(format!("the provider returned a vector of {} dimensions (1..={DIMS_MAX})", numbers.len()));
        }
        let vector = numbers
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or("an embedding holds something that is not a number")?;
        if slots[index].replace(vector).is_some() {
            return Err(format!("the provider returned index {index} twice"));
        }
    }
    // Every slot is filled: count items, none twice, all in range.
    Ok(slots.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn providers() -> BTreeMap<String, ProviderEntry> {
        crate::provider::parse_all(Some(&json!({
            "ollama": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1",
                        "models": [{ "id": "qwen3.5:4b" }, { "id": "bge-m3", "kind": "embedding" }, { "id": "m", "kind": "embedding" }] },
            "hosted": { "style": "openai", "baseUrl": "https://api.example.com/v1/", "apiKey": "${secret://embed-key}", "proxy": "socks5://127.0.0.1:1080",
                        "models": [{ "id": "text-embedding-3-small", "kind": "embedding" }] },
            "chatty": { "style": "openai", "baseUrl": "http://127.0.0.1:8000/v1", "models": [{ "id": "c" }] },
            "glm": { "style": "anthropic", "baseUrl": "https://open.bigmodel.cn/api/anthropic" }
        })))
        .expect("providers")
    }

    fn block(value: Value) -> Result<Option<EmbeddingConfig>, String> {
        parse(Some(&value), &providers())
    }

    #[test]
    fn a_block_resolves_its_provider_and_fills_defaults() {
        let config = block(json!({ "provider": "hosted", "model": "text-embedding-3-small", "dimensions": 512 }))
            .expect("valid")
            .expect("on");
        assert_eq!(config.base_url, "https://api.example.com/v1");
        assert_eq!(config.api_key.as_deref(), Some("${secret://embed-key}"));
        assert_eq!(config.proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
        assert_eq!((config.batch, config.timeout_ms), (BATCH_DEFAULT, TIMEOUT_DEFAULT_MS));
        assert!(config.remote());
        let fingerprint = config.fingerprint();
        let endpoint = fingerprint.strip_prefix("openai:text-embedding-3-small:512:ep-").expect(&fingerprint);
        assert!(endpoint.len() == 8 && endpoint.bytes().all(|b| b.is_ascii_hexdigit()), "{fingerprint}");
        assert!(config.to_json().get("apiKey").is_none(), "the key lives on the provider");
        // Off, absent or blank: no tier.
        assert_eq!(block(json!({ "provider": "off" })).expect("off"), None);
        assert_eq!(parse(None, &providers()).expect("absent"), None);
        assert_eq!(block(json!({})).expect("blank"), None);
        // No model named: the provider's first embedding model.
        assert_eq!(block(json!({ "provider": "ollama" })).expect("valid").expect("on").model, "bge-m3");
    }

    #[test]
    fn a_block_refuses_what_it_cannot_embed_with() {
        let refused = [
            (json!({ "provider": "nope", "model": "m" }), "does not define"),
            (json!({ "provider": "glm", "model": "m" }), "anthropic-style"),
            (json!({ "provider": "chatty" }), "lists no embedding model to default to"),
            (json!({ "provider": "ollama", "model": "qwen3.5:4b" }), "is a chat model on providers.ollama, not an embedding model"),
            (json!({ "provider": "ollama", "model": "nomic-embed-text" }), "does not list \"nomic-embed-text\""),
            (json!({ "provider": "ollama", "model": "m", "baseUrl": "http://x" }), "no \"baseUrl\" field"),
            (json!({ "provider": "ollama", "model": "m", "batch": 0 }), "batch"),
            (json!({ "provider": "ollama", "model": "m", "batch": 999 }), "batch"),
            (json!({ "provider": "ollama", "model": "m", "timeoutMs": 5 }), "timeoutMs"),
            (json!({ "provider": "ollama", "model": "m", "dimensions": 0 }), "dimensions"),
            (json!({ "provider": "ollama", "model": "m", "minSimilarity": 1.5 }), "minSimilarity"),
            (json!({ "provider": "ollama", "model": "m", "minSimilarity": "0.5" }), "minSimilarity"),
        ];
        for (value, needle) in refused {
            let err = block(value.clone()).expect_err(&value.to_string());
            assert!(err.contains(needle), "{value}: {err}");
        }
    }

    #[test]
    fn the_fingerprint_names_the_endpoint_not_the_provider_name() {
        let local = block(json!({ "provider": "ollama", "model": "bge-m3" })).expect("ok").expect("on");
        assert!(!local.remote());
        assert!(local.fingerprint().starts_with("openai:bge-m3:native:ep-"), "{}", local.fingerprint());
        let renamed = EmbeddingConfig { provider: "other-name".into(), ..local.clone() };
        assert_eq!(renamed.fingerprint(), local.fingerprint(), "a renamed provider re-embeds nothing");
        let prefixed = EmbeddingConfig { document_prefix: "passage: ".into(), ..local.clone() };
        assert_ne!(prefixed.fingerprint(), local.fingerprint(), "the document prefix changes every stored vector");
        let queried = EmbeddingConfig { query_prefix: "query: ".into(), ..local.clone() };
        assert_eq!(queried.fingerprint(), local.fingerprint());
        let moved = EmbeddingConfig { base_url: "http://127.0.0.1:11435/v1".into(), ..local.clone() };
        assert_ne!(moved.fingerprint(), local.fingerprint(), "a moved endpoint re-embeds once");
    }

    #[test]
    fn answers_are_read_in_index_order() {
        let body = json!({ "data": [
            { "index": 1, "embedding": [0.0, 2.0] },
            { "index": 0, "embedding": [1.0, 0.0] },
        ] });
        let vectors = vectors_of(body.to_string().as_bytes(), 2).expect("vectors");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 2.0]]);
        let twice = json!({ "data": [{ "index": 0, "embedding": [1.0] }, { "index": 0, "embedding": [1.0] }] });
        assert!(vectors_of(twice.to_string().as_bytes(), 2).expect_err("twice").contains("twice"));
        assert!(vectors_of(b"{}", 1).is_err());
        assert!(vectors_of(json!({ "data": [] }).to_string().as_bytes(), 1).expect_err("count").contains("0 vectors"));
    }

    #[tokio::test]
    async fn masking_embedding_refusals_hides_clipped_key_prefixes() {
        let secret = "k-secret-crosses-the-clip-boundary";
        std::env::set_var("SWISS_LLM_TEST_EMBED_MASKED_KEY", secret);
        let body = json!({ "error": { "message": format!("{}{secret}", "x".repeat(295)) } }).to_string();
        let (url, server) = crate::provider::tests::fake_server(401, body).await;
        let mut config = block(json!({ "provider": "ollama", "model": "bge-m3" })).expect("config").expect("on");
        config.base_url = url;
        config.api_key = Some("$".to_string() + "{SWISS_LLM_TEST_EMBED_MASKED_KEY}");
        let err = EmbedClient::build(&config).expect("client").embed(vec!["test".into()]).await.expect_err("refused");
        server.await.expect("server");
        assert!(!err.contains("k-sec"), "{err}");
        assert!(err.contains("***"), "{err}");
    }

    #[tokio::test]
    async fn the_client_posts_to_the_providers_embeddings_path() {
        let (url, server) = crate::provider::tests::fake_server(
            200,
            json!({ "data": [{ "index": 0, "embedding": [0.6, 0.8] }] }).to_string(),
        )
        .await;
        let config = EmbeddingConfig {
            provider: "local".into(),
            base_url: format!("{url}/v1"),
            api_key: None,
            proxy: None,
            model: "bge-m3".into(),
            dimensions: Some(2),
            batch: 4,
            timeout_ms: 5_000,
            query_prefix: String::new(),
            document_prefix: String::new(),
            min_similarity: None,
        };
        let client = EmbedClient::build(&config).expect("client");
        let vectors = client.embed(vec!["query: hello".into()]).await.expect("vectors");
        assert_eq!(vectors, vec![vec![0.6, 0.8]]);
        let (head, body) = server.await.expect("server");
        assert!(head.starts_with("POST /v1/embeddings"), "{head}");
        let sent: Value = serde_json::from_str(&body).expect("json");
        assert_eq!((sent["model"].as_str(), sent["dimensions"].as_u64()), (Some("bge-m3"), Some(2)));
        assert_eq!(sent["input"][0], "query: hello");
    }
}
