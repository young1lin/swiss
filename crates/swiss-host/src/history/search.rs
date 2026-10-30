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

//! The literal scan engine over sealed bodies (plan docs/plans/module-log-storage.md D5,
//! SPEC `host.history). Literal means LITERAL: no FTS grammar, no ranking, no stemming -
//! ASCII case folds, everything else (non-ASCII, quotes, paths, spaces) matches byte for
//! byte. Bodies are decrypted in memory only; no plaintext derivative (column, WAL page,
//! FTS row, cursor token, log line) is ever written.

use sha2::{Digest, Sha256};

use crate::history::body::{self, BodyCodec};

/// One stored chunk, as an owner's database holds it. Ordered by (run_id, ofs).
#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub run_id: i64,
    pub ofs: i64,
    /// The owner's part ordinal for this chunk (one coded block per part today). The
    /// engine does not read it; owners need it to rebuild the seal binding at decode.
    pub part: i64,
    pub codec: i64,
    pub plain: i64,
    pub block: Vec<u8>,
}

/// Where a scan reads from. Implemented by each owner store; the engine never
/// knows a table name.
pub trait BodySource: Send + Sync {
    /// Chunks with (run_id, ofs) >= from, ascending, at most limit rows.
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>;

    /// The owner's generation: a clear bumps it, which expires every cursor.
    fn generation(&self) -> u64;

    /// Decode one stored chunk to its plaintext bytes. The engine never decodes a
    /// block itself: an owner whose rows carry sealed envelopes overrides this and
    /// unseals first, so the seal's binding (which the owner's rows know but the
    /// engine's SQL never sees) is checked where the row identity is real.
    fn decode(&self, chunk: &StoredChunk) -> Vec<u8> {
        BodyCodec::from_u8(chunk.codec as u8)
            .and_then(|c| body::decode_chunk(c, &chunk.block, chunk.plain as usize).ok())
            .unwrap_or_default()
    }
}

/// The literal query's bounds. The engine validates before touching a body.
pub const MIN_QUERY_BYTES: usize = 1;
pub const MAX_QUERY_BYTES: usize = 256;

/// Why a scan refused to run - wire-shaped strings, never SQL or table names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    QueryTooShort,
    QueryTooLong,
    CursorExpired,
    CursorInvalid,
    OwnerMismatch,
    QueryChanged,
    /// The owner's database read itself failed (corruption, a bad page). The reason
    /// is the engine's own sentence, carried verbatim: folding it into CursorInvalid
    /// once sent users chasing cursor state when the disk was the problem.
    ScanFailed(String),
}

impl ScanError {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanError::QueryTooShort => "query-too-short",
            ScanError::QueryTooLong => "query-too-long",
            ScanError::CursorExpired => "cursor-expired",
            ScanError::CursorInvalid => "cursor-invalid",
            ScanError::OwnerMismatch => "owner-mismatch",
            ScanError::QueryChanged => "query-changed",
            ScanError::ScanFailed(_) => "scan-failed",
        }
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::ScanFailed(reason) => write!(f, "scan-failed: {reason}"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// The resume token: WHERE the scan stopped and WHAT it was scanning - owner,
/// query digest, generation - signed with the host master key so a revoked,
/// forged or cross-owner cursor cannot borrow authority (S3). Bytes only: the
/// needle itself is never embedded (S2), and no plaintext ever rides along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCursor {
    pub run_id: i64,
    pub ofs: i64,
    pub generation: u64,
    owner: [u8; 2],
    q_digest: [u8; 32],
    sig: [u8; 32],
}

fn key_bytes() -> Vec<u8> {
    // Every candidate in order: signing accepts any historical key, like the
    // sealed-file reader, so a rotated key does not strand live cursors.
    let mut out = Vec::new();
    if let Ok(keys) = swiss_core::secure::key::master_key_candidates() {
        for material in keys {
            out.extend_from_slice(&material.key);
        }
    }
    out
}

/// The cursor signature. Keyed twice over fixed-width fields plus the FULL owner
/// string: the inner hash hides the key from length-extension, the outer binds it,
/// and the whole owner (not a prefix) rides inside so a future owner sharing the
/// first bytes of another's id can never borrow its authority. The two raw tag
/// bytes in the token stay as a cheap pre-check only.
fn cursor_sig(
    run_id: i64,
    ofs: i64,
    generation: u64,
    owner_full: &str,
    q_digest: &[u8; 32],
) -> [u8; 32] {
    let keys = key_bytes();
    let mut inner = Sha256::new();
    inner.update(&keys);
    inner.update(run_id.to_be_bytes());
    inner.update(ofs.to_be_bytes());
    inner.update(generation.to_be_bytes());
    inner.update(owner_full.as_bytes());
    inner.update(q_digest);
    let inner: [u8; 32] = inner.finalize().into();
    let mut outer = Sha256::new();
    outer.update(b"swiss.history.scan.v2");
    outer.update(&keys);
    outer.update(inner);
    outer.finalize().into()
}

/// The first two bytes of the owner id: a cheap visible pre-check that keeps a
/// wrong-owner cursor from even reaching the signature comparison. The binding
/// itself is in cursor_sig over the full owner string.
fn owner_tag(owner: &str) -> [u8; 2] {
    let b = owner.as_bytes();
    [b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)]
}

fn q_digest(q: &str) -> [u8; 32] {
    Sha256::digest(q.as_bytes()).into()
}

impl ScanCursor {
    pub fn encode(&self) -> String {
        let mut v = Vec::with_capacity(8 + 8 + 8 + 2 + 32 + 32);
        v.extend_from_slice(&self.run_id.to_be_bytes());
        v.extend_from_slice(&self.ofs.to_be_bytes());
        v.extend_from_slice(&self.generation.to_be_bytes());
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&self.q_digest);
        v.extend_from_slice(&self.sig);
        // Hex, not base64: no new dependency for a token nobody reads by eye.
        v.iter().map(|b| format!("{b:02x}")).collect::<String>()
    }

    pub fn decode(text: &str) -> Result<Self, ScanError> {
        let bytes = text.as_bytes();
        if bytes.len() != 2 * (8 + 8 + 8 + 2 + 32 + 32) {
            return Err(ScanError::CursorInvalid);
        }
        let mut raw = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let hi = (pair[0] as char).to_digit(16).ok_or(ScanError::CursorInvalid)?;
            let lo = (pair[1] as char).to_digit(16).ok_or(ScanError::CursorInvalid)?;
            raw.push((hi * 16 + lo) as u8);
        }
        let take = |i: usize, n: usize| raw[i..i + n].to_vec();
        let i64_of = |o: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&take(o, 8));
            i64::from_be_bytes(b)
        };
        let cursor = ScanCursor {
            run_id: i64_of(0),
            ofs: i64_of(8),
            generation: {
                let mut b = [0u8; 8];
                b.copy_from_slice(&take(16, 8));
                u64::from_be_bytes(b)
            },
            owner: [raw[24], raw[25]],
            q_digest: raw[26..58].try_into().map_err(|_| ScanError::CursorInvalid)?,
            sig: raw[58..90].try_into().map_err(|_| ScanError::CursorInvalid)?,
        };
        // No signature check here: the signature covers the FULL owner string and
        // only the caller of scan() knows it - the check lives there.
        Ok(cursor)
    }
}

/// One literal match: the run and the byte offset of the FIRST byte of the hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub run_id: i64,
    pub byte_ofs: u64,
}

/// How a scan answers: the page, whether more remains, and where to resume.
#[derive(Debug, Clone)]
pub struct SearchReply {
    pub hits: Vec<Hit>,
    pub complete: bool,
    pub next: Option<ScanCursor>,
    pub scanned_chunks: usize,
}

/// ASCII-fold to lowercase: the one folding literal search does. Non-ASCII bytes
/// pass through untouched, so UTF-8 text and paths match exactly.
fn fold(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| if b.is_ascii_uppercase() { b + 32 } else { b })
        .collect()
}

/// Find `needle` (already folded) in `hay` (already folded), returning the offsets of
/// non-overlapping matches, adjusted by `base` - the stream offset of haystack[0].
fn find_all(hay: &[u8], needle: &[u8], base: u64, out: &mut Vec<Hit>, run_id: i64) {
    if needle.is_empty() || hay.len() < needle.len() {
        return;
    }
    let mut from = 0;
    while let Some(at) = hay[from..].windows(needle.len()).position(|w| w == needle) {
        let at = from + at;
        out.push(Hit { run_id, byte_ofs: base + at as u64 });
        from = at + needle.len();
    }
}

/// The batch budget: how many chunks one call may decrypt before answering.
/// Exhausting it with zero hits still returns progress (Q3) - incomplete, never
/// a false "no matches".
pub const DEFAULT_BATCH_CHUNKS: usize = 64;

/// Scan one owner's sealed bodies for the literal needle.
///
/// Semantics the tests pin (plan Q1-Q5):
/// - pages are SNAPSHOT-ordered by (run_id, ofs): new rows with smaller keys are
///   picked up when they fall inside the window; the cursor never shifts under a
///   reader that has not moved (Q4).
/// - a generation bump (a clear) expires every cursor explicitly (Q4).
/// - matches crossing a chunk boundary are found exactly once via the carry -
///   the tail bytes of the previous chunk, RECONSTRUCTED on resume, never
///   persisted anywhere (Q2, Q3).
/// - a gap between chunks resets the carry: two halves never join into a hit
///   that exists in neither (Q2).
pub async fn scan(
    source: &dyn BodySource,
    owner: &str,
    q: &str,
    cursor: Option<&ScanCursor>,
    limit: usize,
) -> Result<SearchReply, ScanError> {
    if q.len() < MIN_QUERY_BYTES {
        return Err(ScanError::QueryTooShort);
    }
    if q.len() > MAX_QUERY_BYTES {
        return Err(ScanError::QueryTooLong);
    }
    // The page target is at least one hit: a literal 0 from any caller must never
    // degenerate into one empty page per stored chunk.
    let limit = limit.max(1);
    let needle = fold(q.as_bytes());
    let digest = q_digest(q);
    let tag = owner_tag(owner);
    let generation = source.generation();
    let mut from = (0i64, 0i64);
    let mut carry: Vec<u8> = Vec::new();
    let mut carry_run = 0i64;
    let mut carry_ofs = 0u64;
    if let Some(c) = cursor {
        if c.owner != tag {
            // A cursor minted for another owner cannot borrow this one's authority.
            return Err(ScanError::OwnerMismatch);
        }
        if c.q_digest != digest {
            return Err(ScanError::QueryChanged);
        }
        if c.generation != generation {
            // A clear (or any generation bump) expired the old snapshot.
            return Err(ScanError::CursorExpired);
        }
        let want = cursor_sig(c.run_id, c.ofs, c.generation, owner, &c.q_digest);
        if want != c.sig {
            // A v1 token from an older build, a forged layout, or a signature that
            // does not bind this owner: refused, never treated as a fresh start.
            return Err(ScanError::CursorInvalid);
        }
        from = (c.run_id, c.ofs);
        // Rebuild the carry from the last scanned chunk itself - the tail is
        // RE-read, never persisted in the cursor or anywhere else (Q3).
        if let Ok(prev) = source.chunks_from(from, 1).await {
            if let Some(chunk) = prev.into_iter().next() {
                let decoded: Vec<u8> = source.decode(&chunk);
                let plain = fold(&decoded);
                carry = tail_bytes(&plain, needle.len().saturating_sub(1));
                carry_ofs = chunk.ofs as u64 + plain.len() as u64;
                carry_run = chunk.run_id;
            }
        }
        from = (c.run_id, c.ofs + 1);
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut scanned = 0usize;
    let mut last_key = from;
    if !carry.is_empty() && carry_ofs == 0 {
        // A carry with no recorded end cannot be contiguous with anything.
        carry.clear();
    }
    // One bounded batch per call: every path out is an explicit answer.
    let batch = source
        .chunks_from(from, DEFAULT_BATCH_CHUNKS)
        .await
        .map_err(ScanError::ScanFailed)?;
    if batch.is_empty() {
        return Ok(SearchReply { hits, complete: true, next: None, scanned_chunks: scanned });
    }
    for chunk in &batch {
        scanned += 1;
        let decoded: Vec<u8> = source.decode(chunk);
        // Fold AFTER decoding: the comparison runs on folded bytes both sides.
        let plain = fold(&decoded);
        let boundary = chunk.ofs as u64;
        let contiguous = !carry.is_empty()
            && carry_run == chunk.run_id
            && carry_ofs == boundary;
        if contiguous {
            // Join the previous tail with this chunk's head: a match spanning
            // the boundary is found here, exactly once (Q2).
            let mut joined = carry.clone();
            joined.extend_from_slice(&plain);
            let before = hits.len();
            find_all(
                &joined,
                &needle,
                boundary - carry.len() as u64,
                &mut hits,
                chunk.run_id,
            );
            // Drop the matches wholly inside the carry: they were reported
            // when their chunk was scanned.
            let kept: Vec<Hit> = hits[..before].to_vec();
            let fresh: Vec<Hit> = hits[before..]
                .iter()
                .filter(|h| {
                    let wholly_in_carry = h.byte_ofs < boundary
                        && h.byte_ofs + needle.len() as u64 <= boundary;
                    !wholly_in_carry
                })
                .cloned()
                .collect();
            let mut all = kept;
            all.extend(fresh);
            hits = all;
        } else {
            find_all(&plain, &needle, boundary, &mut hits, chunk.run_id);
        }
        carry = tail_bytes(&plain, needle.len().saturating_sub(1));
        carry_run = chunk.run_id;
        carry_ofs = boundary + plain.len() as u64;
        last_key = (chunk.run_id, chunk.ofs);
        if hits.len() >= limit {
            // The cursor anchors at the chunk that FILLED the page (Q4): rows after it
            // stay unscanned for the next page, never skipped. The anchor chunk itself
            // is answered IN FULL - a page may exceed the limit when one chunk holds
            // more matches than were asked for, because truncating here would drop
            // matches no later page ever revisits (the resume is strictly after this
            // chunk). Nothing found is silently lost.
            let sig = cursor_sig(last_key.0, last_key.1, generation, owner, &digest);
            return Ok(SearchReply {
                hits,
                complete: false,
                next: Some(ScanCursor {
                    run_id: last_key.0,
                    ofs: last_key.1,
                    generation,
                    owner: tag,
                    q_digest: digest,
                    sig,
                }),
                scanned_chunks: scanned,
            });
        }
    }
    if batch.len() < DEFAULT_BATCH_CHUNKS {
        return Ok(SearchReply { hits, complete: true, next: None, scanned_chunks: scanned });
    }
    // The batch budget ran out (Q3): progress is explicit even with zero hits -
    // an incomplete page with a cursor, never a false "no matches".
    let sig = cursor_sig(last_key.0, last_key.1, generation, owner, &digest);
    Ok(SearchReply {
        hits,
        complete: false,
        next: Some(ScanCursor {
            run_id: last_key.0,
            ofs: last_key.1,
            generation,
            owner: tag,
            q_digest: digest,
            sig,
        }),
        scanned_chunks: scanned,
    })
}

/// The last n bytes of plain - the region a needle could straddle into the next
/// chunk. Empty when n is zero (a one-byte needle cannot straddle).
fn tail_bytes(plain: &[u8], n: usize) -> Vec<u8> {
    if n == 0 || plain.len() < n {
    return plain.to_vec();
    }
    plain[plain.len() - n..].to_vec()
}
