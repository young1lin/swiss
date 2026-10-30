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

//! History body primitives (SPEC §host.history): 64 KiB plaintext chunks coded as
//! independently decompressible blocks. Original UTF-8/output bytes define every public
//! offset; compression and ciphertext never do. Raw blocks are kept when LZ4 would expand
//! them, and decompression honours a declared size cap before it allocates.

/// The target plaintext bytes per chunk. Smaller final chunks are valid.
pub const CHUNK_TARGET_BYTES: usize = 64 * 1024;

/// The largest single body read (SPEC §host.history): one read never materialises
/// more than two chunks of plaintext.
pub const MAX_READ_BYTES: usize = 128 * 1024;

/// The codec one stored block carries. Values are persistence literals — append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyCodec {
    /// Uncompressed plaintext bytes.
    Raw,
    /// LZ4 block (lz4_flex, safe + checked decode), preceded by the uncompressed size as
    /// a little-endian u32 so a block is independently decompressible.
    Lz4,
}

impl BodyCodec {
    /// The byte persisted beside the block. Stable literals — never renumber.
    pub fn as_u8(self) -> u8 {
        match self {
            BodyCodec::Raw => 0,
            BodyCodec::Lz4 => 1,
        }
    }

    pub fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(BodyCodec::Raw),
            1 => Some(BodyCodec::Lz4),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error("the codec byte {0} is not one this build wrote")]
    UnknownCodec(u8),
    #[error("declared plaintext size {declared} exceeds the cap {cap}")]
    DeclaredTooLarge { declared: usize, cap: usize },
    #[error("the stored block is malformed: {0}")]
    Malformed(&'static str),
    #[error("decompression failed: {0}")]
    Decompress(String),
}

/// Split plaintext into chunks of at most [CHUNK_TARGET_BYTES]. The final chunk may be
/// smaller; an empty body yields no chunks (an empty read is complete at offset 0).
pub fn chunk(plain: &[u8]) -> impl Iterator<Item = &[u8]> {
    plain.chunks(CHUNK_TARGET_BYTES)
}

/// Encode one chunk: LZ4 when it shrinks, raw when it does not. The returned pair is what
/// a body_parts row stores (codec literal, block bytes).
pub fn encode_chunk(plain: &[u8]) -> (BodyCodec, Vec<u8>) {
    let compressed = lz4_flex::compress_prepend_size(plain);
    if compressed.len() < plain.len() {
        (BodyCodec::Lz4, compressed)
    } else {
        // Incompressible or expanding input stays raw — a stored block is never larger
        // than its plaintext because of the codec.
        (BodyCodec::Raw, plain.to_vec())
    }
}

/// Decode one stored block back to plaintext. `declared` is the row's plaintext size for
/// this chunk; it is enforced BEFORE decompression or allocation, so a corrupt row cannot
/// ask for an unbounded buffer.
pub fn decode_chunk(codec: BodyCodec, block: &[u8], declared: usize) -> Result<Vec<u8>, BodyError> {
    let cap = declared.max(1);
    if cap > MAX_READ_BYTES {
        return Err(BodyError::DeclaredTooLarge { declared, cap: MAX_READ_BYTES });
    }
    match codec {
        BodyCodec::Raw => {
            if block.len() != declared {
                return Err(BodyError::Malformed("raw block length differs from the declared size"));
            }
            Ok(block.to_vec())
        }
        BodyCodec::Lz4 => {
            if block.len() < 4 {
                return Err(BodyError::Malformed("lz4 block shorter than its size header"));
            }
            let (head, rest) = block.split_at(4);
            let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            if size != declared {
                return Err(BodyError::Malformed(
                    "lz4 block header differs from the declared plaintext size",
                ));
            }
            lz4_flex::decompress(rest, size).map_err(|e| BodyError::Decompress(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bodies_have_no_chunks_and_never_a_fake_one() {
        assert_eq!(chunk(b"").count(), 0);
    }

    #[test]
    fn chunks_respect_the_target_and_the_tail_may_be_small() {
        let plain = vec![7u8; CHUNK_TARGET_BYTES * 2 + 10];
        let sizes: Vec<usize> = chunk(&plain).map(|c| c.len()).collect();
        assert_eq!(sizes, vec![CHUNK_TARGET_BYTES, CHUNK_TARGET_BYTES, 10]);
    }

    #[test]
    fn raw_wins_when_lz4_would_expand() {
        // splitmix64 bytes: no 4-byte match LZ4 could code cheaper than the literals, so
        // the lz4 block (which also carries its size header) is strictly larger.
        let mut state = 0x9E3779B97F4A7C15u64;
        let plain: Vec<u8> = (0..4096)
            .map(|_| {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^ (z >> 31)
            })
            .flat_map(|z| z.to_le_bytes())
            .collect();
        let (codec, block) = encode_chunk(&plain);
        assert_eq!(codec, BodyCodec::Raw);
        assert_eq!(block, plain);
    }

    #[test]
    fn round_trip_compressed_and_raw() {
        let repetitive = vec![b'a'; 100_000];
        let (codec, block) = encode_chunk(&repetitive);
        assert_eq!(codec, BodyCodec::Lz4);
        assert!(block.len() < repetitive.len() / 4, "repetitive input must shrink");
        let back = decode_chunk(codec, &block, repetitive.len()).expect("decode");
        assert_eq!(back, repetitive);
    }

    #[test]
    fn a_declared_size_over_the_cap_is_refused_before_any_allocation() {
        let (codec, block) = encode_chunk(b"small");
        let err = decode_chunk(codec, &block, MAX_READ_BYTES + 1).unwrap_err();
        assert!(matches!(err, BodyError::DeclaredTooLarge { .. }), "{err}");
    }

    #[test]
    fn a_corrupt_block_is_an_error_never_empty_success() {
        let (codec, mut block) = encode_chunk(&vec![b'x'; 2048]);
        assert_eq!(codec, BodyCodec::Lz4);
        // Flip payload bytes: the checksum-free lz4 stream decodes to wrong bytes or fails,
        // but a RAW codec with a lying length must be caught by the length check.
        let len = block.len();
        block[len - 1] ^= 0xff;
        let result = decode_chunk(codec, &block, 2048);
        // Either outcome is honest; empty success is not.
        if let Ok(bytes) = result {
            assert_eq!(bytes.len(), 2048);
        }
        let raw_err = decode_chunk(BodyCodec::Raw, b"short", 2048).unwrap_err();
        assert!(matches!(raw_err, BodyError::Malformed(_)), "{raw_err}");
    }

    #[test]
    fn unknown_codec_literals_are_refused() {
        assert!(BodyCodec::from_u8(2).is_none());
        assert!(BodyCodec::from_u8(255).is_none());
    }
}
