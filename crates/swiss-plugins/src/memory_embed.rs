/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Memory's semantic tier over the llm plugin's embedding endpoint (SPEC §memory.embedding):
//! swiss-memory's [Embedder] trait in front of swiss-llm's [EmbedClient]. The endpoint, the
//! model and the key live on the llm row (SPEC §llm.embedding); what a vector MEANS for the
//! store - the prefixes, the width check, normalization, the similarity floor - is decided
//! here.
//!
//! The embedder's id is the endpoint's fingerprint, unchanged from the in-row block it
//! replaces: a store embedded before the move keeps its vectors.

use std::time::Duration;

use async_trait::async_trait;
use swiss_llm::embedding::{EmbedClient, EmbeddingConfig};
use swiss_memory::embed::{self, Embedder, Purpose, DIMS_MAX, INDEX_BATCH_MAX, QUERY_TIMEOUT_MS};
use swiss_memory::{MemError, MemResult};

pub struct LlmEmbedder {
    id: String,
    client: EmbedClient,
    dimensions: Option<usize>,
    batch: usize,
    query_prefix: String,
    document_prefix: String,
    floor: f32,
}

impl LlmEmbedder {
    /// Err only when the HTTP client cannot be built (the proxy, the TLS backend) or the
    /// endpoint asks for a width the store cannot hold. The llm STATE attaches (not a
    /// captured store): the history is resolved per embed call, so the embedder records
    /// even when it was built before the llm plugin opened its database (SPEC llm.history).
    pub fn build(
        config: &EmbeddingConfig,
        llm: Option<std::sync::Arc<swiss_llm::state::LlmState>>,
    ) -> Result<LlmEmbedder, String> {
        if config.dimensions.is_some_and(|d| d > DIMS_MAX) {
            return Err(format!("embedding.dimensions: the memory store holds at most {DIMS_MAX}"));
        }
        Ok(LlmEmbedder {
            id: config.fingerprint(),
            client: EmbedClient::build(config)?.recording(llm, Some("memory")),
            dimensions: config.dimensions,
            batch: config.batch.clamp(1, INDEX_BATCH_MAX),
            query_prefix: config.query_prefix.clone(),
            document_prefix: config.document_prefix.clone(),
            floor: config.min_similarity.map(|f| f as f32).unwrap_or(embed::SIMILARITY_FLOOR),
        })
    }
}

#[async_trait]
impl Embedder for LlmEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    /// A search waits for its query vector no longer than the endpoint's request timeout,
    /// and never longer than the service's default.
    fn query_timeout(&self) -> Duration {
        self.client.timeout().min(Duration::from_millis(QUERY_TIMEOUT_MS))
    }

    fn batch(&self) -> usize {
        self.batch
    }

    fn floor(&self) -> f32 {
        self.floor
    }

    async fn embed(&self, texts: &[String], purpose: Purpose) -> MemResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = match purpose {
            Purpose::Query => &self.query_prefix,
            Purpose::Document => &self.document_prefix,
        };
        let input: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        let mut vectors = self.client.embed(input).await.map_err(MemError::invalid)?;
        check(&mut vectors, self.dimensions)?;
        Ok(vectors)
    }
}

/// Every vector one width (the asked width when there is one), at most the store's widest,
/// and normalized; a zero or non-finite vector is refused.
fn check(vectors: &mut [Vec<f32>], dimensions: Option<usize>) -> MemResult<()> {
    let width = vectors.first().map(Vec::len).unwrap_or(0);
    for vector in vectors.iter_mut() {
        if vector.len() != width || dimensions.is_some_and(|d| d != width) {
            return Err(MemError::invalid(format!(
                "the provider returned {} dimensions where {} were expected",
                vector.len(),
                dimensions.unwrap_or(width)
            )));
        }
        if width > DIMS_MAX {
            return Err(MemError::invalid(format!("the provider returned {width} dimensions; the store holds at most {DIMS_MAX}")));
        }
        if !embed::normalize(vector) {
            return Err(MemError::invalid("the provider returned a zero or non-finite vector"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(extra: serde_json::Value) -> EmbeddingConfig {
        let mut embedding = json!({ "provider": "local", "model": "bge-m3" });
        if let (Some(target), Some(more)) = (embedding.as_object_mut(), extra.as_object()) {
            target.extend(more.clone());
        }
        swiss_llm::LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1", "models": [{ "id": "bge-m3", "kind": "embedding" }] } },
            "embedding": embedding,
        }))
        .expect("row")
        .embedding
        .expect("embedding")
    }

    #[test]
    fn the_adapter_keeps_the_fingerprint_and_the_floor() {
        let plain = LlmEmbedder::build(&config(json!({})), None).expect("build");
        assert!(plain.id().starts_with("openai:bge-m3:native:ep-"), "{}", plain.id());
        assert_eq!(plain.floor(), embed::SIMILARITY_FLOOR);
        let strict = LlmEmbedder::build(&config(json!({ "minSimilarity": 0.5, "batch": 8 })), None).expect("build");
        assert_eq!((strict.floor(), strict.batch()), (0.5, 8));
        assert_eq!(strict.id(), plain.id(), "the floor is query-time only");
        let prefixed = LlmEmbedder::build(&config(json!({ "documentPrefix": "passage: " })), None).expect("build");
        assert_ne!(prefixed.id(), plain.id(), "the document prefix changes every stored vector");
    }

    #[test]
    fn vectors_are_checked_and_normalized() {
        let mut ok = vec![vec![3.0, 4.0], vec![0.0, 2.0]];
        check(&mut ok, None).expect("ok");
        assert_eq!(ok[0], vec![0.6, 0.8]);
        let mut ragged = vec![vec![1.0, 0.0], vec![1.0]];
        assert!(check(&mut ragged, None).is_err());
        let mut narrow = vec![vec![1.0, 0.0]];
        assert!(check(&mut narrow, Some(3)).expect_err("width").message.contains("3 were expected"));
        let mut zero = vec![vec![0.0, 0.0]];
        assert!(check(&mut zero, None).is_err());
    }
}
