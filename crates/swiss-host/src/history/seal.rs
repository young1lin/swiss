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

//! Authenticated chunk sealing for sensitive history bodies (SPEC §core.sealed, SPEC
//! §host.history).
//!
//! The sealed-envelope format itself is FROZEN and untouched: this module is a caller of
//! [swiss_core::secure::envelope] that puts the chunk's row identity INSIDE the encrypted
//! plaintext, so a valid envelope moved to the wrong row, part, chunk or byte range fails
//! the binding check after the tag check — ciphertext swaps cannot smuggle a real seal
//! into someone else's row. Strictness is the whole point for a SEALED row: a missing
//! key, a malformed envelope or a bad tag refuses that row — never a plaintext fallback
//! for a sealed row and never empty successful content. Bytes that were never sealed
//! (pre-wiring rows, the keyless degraded mode below) are plain by construction and
//! open as plain; that is a different row class, not a fallback.

use base64::Engine;
use serde::{Deserialize, Serialize};
use swiss_core::secure::envelope::{self, EnvelopeError, Sealed};
use swiss_core::secure::key::master_key_candidates;

/// The master keys a store seals and opens chunk envelopes with, resolved once per
/// open from the machine's key sources (strongest first — the same order every other
/// sealed file in the home opens with). Empty means the machine yielded no key at all:
/// the store runs DEGRADED, writing plain coded blocks exactly as before sealing
/// existed and saying so once, because refusing to record history over a missing key
/// would trade availability for a promise nothing else in the home makes either.
#[derive(Debug, Clone, Default)]
pub struct ChunkKeys {
    candidates: Vec<[u8; 32]>,
    /// Warned-once flags SHARED by every clone: the keys are cloned per write (the
    /// queued job owns its copy), so a per-instance flag would log on every record.
    warned_degraded: std::sync::Arc<std::sync::atomic::AtomicBool>,
    warned_refused: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ChunkKeys {
    /// Resolve from the machine's key sources; never errors — an empty candidate list
    /// IS the degraded state.
    pub fn resolve() -> Self {
        let candidates = master_key_candidates()
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.key)
            .collect();
        Self {
            candidates,
            warned_degraded: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            warned_refused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Keys over one fixed master, for tests in this crate: the sealed path without
    /// the machine's key sources.
    #[cfg(test)]
    pub(crate) fn from_master(master: [u8; 32]) -> Self {
        Self { candidates: vec![master], ..Self::default() }
    }

    /// True when no key source answered: writes stay plain; reads still open PLAIN
/// (legacy/degraded) rows - a sealed row from a key this machine lacks serves as
/// gone, exactly as it always does.
    pub fn degraded(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Seal one coded block into its stored bytes. Degraded: the coded bytes return
    /// unchanged (a plain block, the pre-sealing row shape) after one log line.
    pub fn seal_block(&self, key_source: &str, binding: &ChunkBinding, coded: &[u8]) -> Vec<u8> {
        if self.candidates.is_empty() {
            if !self
                .warned_degraded
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                swiss_core::log::error(
                    "history chunks stored unsealed: no master key",
                    Some(serde_json::json!({ "keySource": key_source })),
                );
            }
            return coded.to_vec();
        }
        serde_json::to_vec(&seal_chunk(&self.candidates[0], key_source, binding, coded))
            .unwrap_or_else(|_| coded.to_vec())
    }

    /// What a stored block opened as. [OpenBlock::Plain] is a legacy or degraded row —
    /// the caller decodes it with the plain codec as always. [OpenBlock::Sealed] is the
    /// unsealed coded payload. [OpenBlock::Refused] is a genuine envelope this build
    /// could not open or whose binding belongs elsewhere: strictness means the row is
    /// GONE, never served from a fallback, and the reason is logged once per store.
    pub fn open_block(&self, block: &[u8], expected: &ChunkBinding) -> OpenBlock {
        let Ok(sealed) = serde_json::from_slice::<Sealed>(block) else {
            return OpenBlock::Plain;
        };
        for master in &self.candidates {
            match open_chunk(master, &sealed, expected) {
                Ok(coded) => return OpenBlock::Sealed(coded),
                // AuthFailed means THIS key did not open it; other candidates may.
                Err(ChunkSealError::Envelope(EnvelopeError::AuthFailed)) => continue,
                Err(e) => {
                    // Once per store (the flag is shared across clones), not once
                    // per refused chunk: a bulk read of gone bodies must not spam.
                    if !self
                        .warned_refused
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        swiss_core::log::error(
                            "a sealed history chunk refused to open: the row serves as gone",
                            Some(serde_json::json!({ "reason": e.to_string() })),
                        );
                    }
                    return OpenBlock::Refused;
                }
            }
        }
        if !self
            .warned_refused
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            swiss_core::log::error(
                "a sealed history chunk refused to open: the row serves as gone",
                Some(serde_json::json!({ "reason": "no key opens this envelope" })),
            );
        }
        OpenBlock::Refused
    }
}

/// The outcome of opening one stored block (see [ChunkKeys::open_block]).
pub enum OpenBlock {
    Sealed(Vec<u8>),
    Plain,
    Refused,
}

/// Every fact a body row carries that the sealed plaintext must repeat. Field order in the
/// JSON is the binding's, so two rows with different values never share a valid envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkBinding {
    /// The owning module id ("mcp", "remote") — one module's seal never opens as another's.
    pub module: String,
    /// The stable resource id inside the owner (an MCP id, a target id).
    pub resource: String,
    /// The event row this body belongs to.
    pub event: i64,
    /// The part this chunk stores (multi-part bodies), 0 for single-part.
    pub part: i64,
    /// The owner's body generation at write time. The wired owners pin 0 (the row
/// carries no worker epoch); the field is reserved for a future epoch binding.
    pub generation: u64,
    /// The chunk's ordinal within the part.
    pub chunk: i64,
    /// The chunk's plaintext byte range within the ORIGINAL body bytes — offsets users see.
    pub range_start: u64,
    pub range_end: u64,
    /// The codec literal of the payload inside (see crate::history::body::BodyCodec).
    pub codec: u8,
}

impl ChunkBinding {
    /// The row-identity binding for one stored body part. The generation and chunk
    /// fields stay fixed at 0: pinning the live worker generation would break every
    /// read after a reopen (the row does not store it), and one coded block per part
    /// means the chunk ordinal carries no information - the other eight fields pin
    /// the row's identity on their own.
    pub fn row(
        module: &str,
        resource: &str,
        event: i64,
        part: i64,
        range_start: u64,
        plain: usize,
        codec: u8,
    ) -> Self {
        ChunkBinding {
            module: module.into(),
            resource: resource.into(),
            event,
            part,
            generation: 0,
            chunk: 0,
            range_start,
            range_end: range_start + plain as u64,
            codec,
        }
    }
}

/// The sealed plaintext wrapper: binding first, payload second, both authenticated by the
/// envelope's GCM tag. `v1` leaves room for a future in-format revision to be refused
/// explicitly rather than misparsed.
#[derive(Serialize, Deserialize)]
struct ChunkPlaintext {
    v: u8,
    b: ChunkBinding,
    d: String,
}

const PLAINTEXT_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ChunkSealError {
    #[error("the envelope's tag verified but its binding belongs to another row")]
    BindingMismatch,
    #[error("the sealed plaintext is not a chunk envelope this build wrote")]
    Malformed(#[source] serde_json::Error),
    #[error("the sealed plaintext carries v{0}, newer than this build's v{PLAINTEXT_VERSION}")]
    NewerPlaintext(u8),
    #[error("the payload is not valid base64: {0}")]
    PayloadBase64(#[source] base64::DecodeError),
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
}

/// Seal one coded chunk. Compression happens BEFORE this call (codec bytes are what `d`
/// carries); sealing is the last step, so nothing about the row can change afterwards.
pub fn seal_chunk(master: &[u8], key_source: &str, binding: &ChunkBinding, coded: &[u8]) -> Sealed {
    let plaintext = ChunkPlaintext {
        v: PLAINTEXT_VERSION,
        b: binding.clone(),
        d: base64::engine::general_purpose::STANDARD.encode(coded),
    };
    let text = serde_json::to_string(&plaintext).expect("a binding and base64 always serialize");
    envelope::seal(master, key_source, &text)
}

/// Open one sealed chunk and require it to belong to `expected`. The GCM tag is verified
/// FIRST (by [envelope::unseal]); only then is the inner binding compared to the row's, so
/// a swap of two genuine envelopes is caught here and tampering is caught there.
pub fn open_chunk(
    master: &[u8],
    sealed: &Sealed,
    expected: &ChunkBinding,
) -> Result<Vec<u8>, ChunkSealError> {
    let text = envelope::unseal(master, sealed)?;
    let plaintext: ChunkPlaintext =
        serde_json::from_str(&text).map_err(ChunkSealError::Malformed)?;
    if plaintext.v != PLAINTEXT_VERSION {
        return Err(ChunkSealError::NewerPlaintext(plaintext.v));
    }
    if plaintext.b != *expected {
        return Err(ChunkSealError::BindingMismatch);
    }
    base64::engine::general_purpose::STANDARD
        .decode(plaintext.d)
        .map_err(ChunkSealError::PayloadBase64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(chunk: i64, range: (u64, u64)) -> ChunkBinding {
        ChunkBinding {
            module: "mcp".into(),
            resource: "echo".into(),
            event: 42,
            part: 0,
            generation: 1,
            chunk,
            range_start: range.0,
            range_end: range.1,
            codec: 0,
        }
    }

    fn master() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = i as u8;
        }
        key
    }

    #[test]
    fn round_trip_returns_the_coded_bytes() {
        let m = master();
        let b = binding(0, (0, 16));
        let sealed = seal_chunk(&m, "env", &b, b"coded payload bytes");
        let opened = open_chunk(&m, &sealed, &b).expect("open");
        assert_eq!(opened, b"coded payload bytes");
    }

    #[test]
    fn envelope_chunk_swap_or_reorder_is_rejected() {
        let m = master();
        let first = binding(0, (0, 64));
        let second = binding(1, (64, 128));
        let sealed_first = seal_chunk(&m, "env", &first, b"first block");
        let sealed_second = seal_chunk(&m, "env", &second, b"second block");

        // Swap: each genuine envelope presented against the other row's binding.
        let err = open_chunk(&m, &sealed_second, &first).unwrap_err();
        assert!(matches!(err, ChunkSealError::BindingMismatch), "{err}");
        let err = open_chunk(&m, &sealed_first, &second).unwrap_err();
        assert!(matches!(err, ChunkSealError::BindingMismatch), "{err}");

        // Reorder inside one event: a later chunk's seal on the earlier row.
        let reordered = binding(0, (64, 128));
        let err = open_chunk(&m, &sealed_second, &reordered).unwrap_err();
        assert!(matches!(err, ChunkSealError::BindingMismatch), "{err}");

        // A different module's row, an otherwise identical binding.
        let mut other_module = binding(0, (0, 64));
        other_module.module = "remote".into();
        let err = open_chunk(&m, &sealed_first, &other_module).unwrap_err();
        assert!(matches!(err, ChunkSealError::BindingMismatch), "{err}");
    }

    #[test]
    fn a_wrong_key_is_an_auth_failure_not_a_forged_match() {
        let sealed = seal_chunk(&master(), "env", &binding(0, (0, 8)), b"x");
        let mut wrong = master();
        wrong[0] ^= 0xff;
        let err = open_chunk(&wrong, &sealed, &binding(0, (0, 8))).unwrap_err();
        assert!(matches!(err, ChunkSealError::Envelope(EnvelopeError::AuthFailed)), "{err}");
    }

    #[test]
    fn the_same_plaintext_never_seals_to_the_same_bytes() {
        let m = master();
        let b = binding(0, (0, 8));
        let one = seal_chunk(&m, "env", &b, b"same");
        let two = seal_chunk(&m, "env", &b, b"same");
        assert_ne!(one.ct, two.ct, "per-call salt and iv must re-randomise the seal");
    }
}
