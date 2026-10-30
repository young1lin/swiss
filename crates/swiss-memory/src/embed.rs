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

//! The embedding provider contract (SPEC §memory.embedding). This crate holds no
//! HTTP code: a provider is anything implementing [Embedder], plugged into the
//! service at run time (the gateway's OpenAI-compatible client lives in the root
//! crate). Vectors are stored L2-normalized as little-endian f32, keyed by the
//! provider's fingerprint and the hash of the text they were made from, so a model
//! change or an edit makes them stale without any bookkeeping. One fingerprint holds
//! one width: a vector of another width than the provider answers is never compared
//! ([cosine] has no answer for it), only dropped and embedded again.
//!
//! The trait is `#[async_trait]` rather than a hand-written boxed future: it is the
//! tree's convention for dyn async traits (the host's service traits), it is
//! already linked, and its futures are `Send`, which the plugin's indexer task needs
//! to run under `PluginScope::spawn`.

use std::time::Duration;

use serde::Serialize;

use crate::error::MemResult;

/// The least cosine a note needs to count as close in meaning when the provider
/// names none ([Embedder::floor]). Below it a match is noise for the common
/// general-purpose models (unrelated text sits near 0.0–0.25); a model whose
/// unrelated pairs sit higher (BGE-M3, near 0.3–0.5) wants a higher one.
pub const SIMILARITY_FLOOR: f32 = 0.3;
/// The semantic list a search fuses: at most this many notes, best first.
pub const SEMANTIC_CAP: usize = 50;
/// The default time a search waits for its query vector.
pub const QUERY_TIMEOUT_MS: u64 = 4_000;
/// The widest vector the store accepts.
pub const DIMS_MAX: usize = 8_192;
/// Notes one indexing run embeds by default, and at most.
pub const INDEX_BATCH_DEFAULT: usize = 64;
pub const INDEX_BATCH_MAX: usize = 256;

/// What a text is embedded as: some models embed a question and a passage
/// differently (instruction prefixes, asymmetric heads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Query,
    Document,
}

/// One embedding provider.
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// The fingerprint of what makes the vectors: provider, model, requested width and
    /// endpoint (e.g. `openai:text-embedding-3-small:1536:ep-1a2b3c4d`). A stored
    /// vector under any other fingerprint is stale.
    fn id(&self) -> &str;

    /// How long a search waits for its query vector before answering lexically.
    fn query_timeout(&self) -> Duration {
        Duration::from_millis(QUERY_TIMEOUT_MS)
    }

    /// Texts per [Embedder::embed] call during indexing.
    fn batch(&self) -> usize {
        16
    }

    /// The least cosine that counts as close in meaning for this model: a note the
    /// words missed enters a search, a brief or an association only at or above it.
    fn floor(&self) -> f32 {
        SIMILARITY_FLOOR
    }

    /// One vector per text, in order. The service normalizes and checks them; an
    /// error message must never carry a credential.
    async fn embed(&self, texts: &[String], purpose: Purpose) -> MemResult<Vec<Vec<f32>>>;
}

/// The text a note is embedded from.
pub fn text_of(title: &str, content: &str) -> String {
    format!("{title}\n{content}")
}

/// The staleness key: SHA-256 of exactly `title + "\n" + content`, hex.
pub fn text_hash(title: &str, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = Sha256::digest(text_of(title, content).as_bytes()).into();
    swiss_core::util::to_hex(&digest)
}

/// L2-normalize in place. False for an empty, zero or non-finite vector.
pub fn normalize(vector: &mut [f32]) -> bool {
    if vector.is_empty() || vector.iter().any(|x| !x.is_finite()) {
        return false;
    }
    let norm = vector.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt();
    if norm <= f64::EPSILON {
        return false;
    }
    for x in vector.iter_mut() {
        *x = (*x as f64 / norm) as f32;
    }
    true
}

/// Little-endian f32 bytes.
pub fn to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The inverse of [to_bytes]; None for a length that is not a whole number of f32s.
pub fn from_bytes(bytes: &[u8]) -> Option<Vec<f32>> {
    let (words, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        return None;
    }
    Some(words.iter().map(|w| f32::from_le_bytes(*w)).collect())
}

/// Cosine of two normalized vectors (their dot product); None when the widths differ:
/// such a pair is incomparable, and a 0 would rank it as unrelated without a word.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().clamp(-1.0, 1.0))
}

/// Whether the search's semantic tier ran (SPEC §memory.embedding): `off` (no
/// embedder), `on` (fused), `unavailable` (the embedder failed or timed out, or the
/// scope has no current vectors yet - `reason` says which; the results are lexical).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticState {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl SemanticState {
    pub fn off() -> SemanticState {
        SemanticState { state: "off", reason: None, model: None }
    }

    /// Off for a reason someone chose (a project's settings turn embedding off,
    /// §memory.embedding), not for lack of a model.
    pub fn off_because(reason: impl Into<String>) -> SemanticState {
        SemanticState { state: "off", reason: Some(reason.into()), model: None }
    }

    pub fn on(model: &str) -> SemanticState {
        SemanticState { state: "on", reason: None, model: Some(model.to_string()) }
    }

    pub fn unavailable(model: &str, reason: impl Into<String>) -> SemanticState {
        SemanticState { state: "unavailable", reason: Some(reason.into()), model: Some(model.to_string()) }
    }
}

/// One indexing run's outcome.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexReport {
    /// Notes embedded and stored by this run.
    pub embedded: usize,
    /// Live notes still lacking a current vector afterwards.
    pub pending: usize,
    /// Notes this run tried and could not embed.
    pub failed: usize,
    /// Vectors of another width than the provider answers now, dropped by this run:
    /// their notes are pending again.
    pub dropped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The embedding index at a glance (the admin API's `status`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    /// `off` | `ready` | `indexing` | `stale` (vectors of another width wait to be
    /// embedded again) | `error`.
    pub state: &'static str,
    pub model: Option<String>,
    /// The index's one width: what the provider answered last, else the width of the
    /// newest stored vector.
    pub dims: Option<usize>,
    /// Live notes with a current vector: this fingerprint, this text, this width.
    pub indexed: usize,
    /// Live notes without one.
    pub pending: usize,
    /// Vectors under this fingerprint of another width: never scored, dropped by the
    /// next indexing run.
    pub incompatible: usize,
    pub last_error: Option<String>,
    pub last_run_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_round_trip_normalized() {
        let mut v = vec![3.0f32, 4.0];
        assert!(normalize(&mut v));
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(from_bytes(&to_bytes(&v)), Some(v.clone()));
        assert!(cosine(&v, &v).is_some_and(|c| (c - 1.0).abs() < 1e-6));
        assert_eq!(cosine(&v, &[1.0]), None, "another width is incomparable, never 0");
        assert!(from_bytes(&[0, 1, 2]).is_none());
        assert!(!normalize(&mut [0.0, 0.0]));
        assert!(!normalize(&mut [f32::NAN, 1.0]));
        assert!(!normalize(&mut []));
    }

    #[test]
    fn the_staleness_key_is_the_exact_text() {
        assert_eq!(text_hash("a", "b"), text_hash("a", "b"));
        assert_ne!(text_hash("a", "b"), text_hash("a", "b "));
    }
}
