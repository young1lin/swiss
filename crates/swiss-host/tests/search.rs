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

//! The literal scan engine's acceptance set (plan Q1-Q4 plus the S2/S3 engine
//! halves): literal semantics, boundary carry, continuation progress, snapshot
//! cursors, and authority-bound tokens.

use std::sync::{Arc, Mutex};

use swiss_host::history::body;
use swiss_host::history::search::{
    self, BodySource, ScanCursor, ScanError, SearchReply, StoredChunk,
};

/// An owner whose database read fails: the corrupted-disk shape seen on a live
/// instance (a garbage main file beside a healthy -wal opens, then fails
/// mid-scan). The scan must say why, never blame the cursor.
struct FailOwner;

impl BodySource for FailOwner {
    fn chunks_from(
        &self,
        _from: (i64, i64),
        _limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>
    {
        Box::pin(async { Err("database disk image is malformed".to_string()) })
    }

    fn generation(&self) -> u64 {
        0
    }

    fn decode(&self, _chunk: &StoredChunk) -> Vec<u8> {
        Vec::new()
    }
}

/// An in-memory owner: chunks in key order, a clearable generation.
#[derive(Clone, Default)]
struct MemOwner {
    chunks: Arc<Mutex<Vec<StoredChunk>>>,
    generation: Arc<Mutex<u64>>,
}

impl MemOwner {
    fn add(&self, run_id: i64, ofs: i64, plain: &[u8]) {
        let (codec, block) = body::encode_chunk(plain);
        self.chunks.lock().unwrap().push(StoredChunk {
            run_id,
            ofs,
            part: 0,
            codec: codec.as_u8() as i64,
            plain: plain.len() as i64,
            block,
        });
        self.chunks
            .lock()
            .unwrap()
            .sort_by_key(|c| (c.run_id, c.ofs));
    }

    fn clear(&self) {
        self.chunks.lock().unwrap().clear();
        *self.generation.lock().unwrap() += 1;
    }
}

impl BodySource for MemOwner {
    fn chunks_from(
        &self,
        from: (i64, i64),
        limit: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>
    {
        let chunks = self.chunks.clone();
        Box::pin(async move {
            let all = chunks.lock().unwrap();
            Ok(all
                .iter()
                .filter(|c| (c.run_id, c.ofs) >= from)
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }
}

async fn first(owner: &MemOwner, q: &str, cursor: Option<&ScanCursor>, limit: usize) -> SearchReply {
    search::scan(owner, "mcp", q, cursor, limit).await.unwrap()
}

fn hit_ofs(reply: &SearchReply) -> Vec<u64> {
    reply.hits.iter().map(|h| h.byte_ofs).collect()
}

// --- Q1 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn literal_query_preserves_ascii_fold_nonascii_quotes_paths_and_short_words() {
    let owner = MemOwner::default();
    owner.add(1, 0, b"Deploy FAILED on \"C:\\Program Files\\x y\" \xE8\xB7\xAF\xE5\xBE\x84");
    // ASCII folds, case-insensitively.
    let r = first(&owner, "failed", None, 10).await;
    assert_eq!(hit_ofs(&r), vec![7], "{r:?}");
    // Non-ASCII matches byte-exactly, no folding of multibyte text.
    let r = first(&owner, "\u{8DEF}\u{5F84}", None, 10).await;
    assert_eq!(r.hits.len(), 1);
    let r = first(&owner, "\u{8DEF}\u{5F84}X", None, 10).await;
    assert_eq!(r.hits.len(), 0, "no fuzzy matching of non-ASCII");
    // Quotes and spaces are literal: no phrase splitting, no grammar.
    let r = first(&owner, "\"C:\\Program Files\\x y\"", None, 10).await;
    assert_eq!(r.hits.len(), 1, "a quoted path with spaces is one literal");
    let r = first(&owner, "\"C:\\Program  Files\"", None, 10).await;
    assert_eq!(r.hits.len(), 0, "two spaces are two spaces");
    // A one-byte needle works: there is no trigram minimum here.
    let r = first(&owner, "F", None, 10).await;
    assert_eq!(r.hits.len(), 2, "short words are found by bounded scan");
    // Length bounds are explicit, not silent.
    assert_eq!(
        search::scan(&owner, "mcp", "", None, 10).await.unwrap_err(),
        ScanError::QueryTooShort
    );
    assert_eq!(
        search::scan(&owner, "mcp", &"x".repeat(257), None, 10).await.unwrap_err(),
        ScanError::QueryTooLong
    );
}

#[tokio::test]
async fn fts_candidate_verification_removes_false_hits() {
    // Literal means literal: no tokenization, prefixes or edit distance. A hit is
    // a byte-for-byte (ASCII-folded) substring, nothing else.
    let owner = MemOwner::default();
    owner.add(1, 0, b"running run runner NOT-A-MATCH runx");
    let r = first(&owner, "run", None, 10).await;
    assert_eq!(r.hits.len(), 4, "{r:?}");
    assert_eq!(hit_ofs(&r), vec![0, 8, 12, 31]);
    let r = first(&owner, "not a match", None, 10).await;
    assert_eq!(r.hits.len(), 0, "punctuation is not a word boundary illusion");
}

// --- Q2 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn fts_carry_finds_every_chunk_boundary_once() {
    let owner = MemOwner::default();
    // SECRET spans the 5/6-byte boundary of chunks "...xxSEC" | "RETyy...".
    owner.add(1, 0, b"xxSEC");
    owner.add(1, 5, b"RETyy SECRET");
    let r = first(&owner, "SECRET", None, 10).await;
    assert_eq!(hit_ofs(&r), vec![2, 11], "both hits, each exactly once: {r:?}");
    // The spanning hit at 2 is found ONCE even though the carry re-reads the tail.
    assert_eq!(r.hits.len(), 2);
}

#[tokio::test]
async fn search_never_joins_fields_or_gaps() {
    let owner = MemOwner::default();
    // A byte gap between chunks: ofs jumps 5 -> 40.
    owner.add(1, 0, b"xxSEC");
    owner.add(1, 40, b"RETyy");
    let r = first(&owner, "SECRET", None, 10).await;
    assert_eq!(r.hits.len(), 0, "two halves across a gap never join");
    // Different runs never join either.
    let owner2 = MemOwner::default();
    owner2.add(1, 0, b"xxSEC");
    owner2.add(2, 5, b"RETyy");
    let r = first(&owner2, "SECRET", None, 10).await;
    assert_eq!(r.hits.len(), 0, "two runs are two streams");
}

// --- Q3 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn encrypted_zero_hit_batch_advances_with_continuation() {
    let owner = MemOwner::default();
    // More chunks than two batches: no hit anywhere.
    for i in 0..130i64 {
        owner.add(1, i * 8, b"no-match");
    }
    let r = first(&owner, "SECRET", None, 10).await;
    assert!(!r.complete, "the batch budget ran out before the stream did");
    assert_eq!(r.hits.len(), 0);
    assert_eq!(r.scanned_chunks, search::DEFAULT_BATCH_CHUNKS);
    let next = r.next.clone().unwrap();
    // The continuation makes progress and eventually completes.
    let r2 = first(&owner, "SECRET", Some(&next), 10).await;
    assert_eq!(r2.scanned_chunks, search::DEFAULT_BATCH_CHUNKS);
    let r3 = first(&owner, "SECRET", r2.next.as_ref(), 10).await;
    assert!(r3.complete, "the third page finishes the stream");
    assert_eq!(r3.scanned_chunks, 130 - 2 * search::DEFAULT_BATCH_CHUNKS);
}

#[tokio::test]
async fn resume_reconstructs_carry_without_persisting_it() {
    let owner = MemOwner::default();
    // SECRET spans chunks 63|64 - exactly the first batch's edge.
    for i in 0..63i64 {
        owner.add(1, i * 8, b"no-match");
    }
    owner.add(1, 63 * 8, b"xxSEC");
    owner.add(1, 63 * 8 + 5, b"RETyy");
    owner.add(2, 0, b"tail");
    let r = first(&owner, "SECRET", None, 10).await;
    assert!(!r.complete);
    let token = r.next.clone().unwrap();
    assert_eq!(token.run_id, 1);
    // The carry lives only inside a scan: the token carries positions and a
    // signature, never body bytes (S2).
    let hex = token.encode();
    assert!(!hex.contains("SEC") && !hex.contains("RET"));
    let r2 = first(&owner, "SECRET", Some(&token), 10).await;
    assert_eq!(hit_ofs(&r2), vec![63 * 8 + 2], "the resume rebuilt the carry and found the boundary hit exactly once");
    assert!(r2.complete);
}

// --- Q4 ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn new_rows_do_not_shift_snapshot_pages() {
    let owner = MemOwner::default();
    for i in 0..10i64 {
        owner.add(1, i * 8, b"alpha SECRET beta");
    }
    // Page one with a tight limit: its hits are stable facts.
    let r1 = first(&owner, "SECRET", None, 3).await;
    assert_eq!(r1.hits.len(), 3);
    let token = r1.next.clone().unwrap();
    // New rows land AFTER the cursor's key: they extend the stream, they never
    // renumber what page one already returned.
    owner.add(5, 0, b"SECRET SECRET");
    let r2 = first(&owner, "SECRET", Some(&token), 10).await;
    let mut all = hit_ofs(&r1);
    all.extend(hit_ofs(&r2));
    // Page one's three, the seven remaining old hits, and the two new ones -
    // every hit exactly once, none skipped, none renumbered.
    assert_eq!(
        all,
        vec![6, 14, 22, 30, 38, 46, 54, 62, 70, 78, 0, 7],
        "{all:?}"
    );
}

#[tokio::test]
async fn out_of_order_run_finishes_page_completely() {
    let owner = MemOwner::default();
    // Runs finish out of order: run 2's chunk exists, run 1's lands late with a
    // key INSIDE the not-yet-scanned window.
    owner.add(2, 0, b"SECRET late");
    let r1 = first(&owner, "SECRET", None, 10).await;
    assert_eq!(r1.hits.len(), 1, "page one saw run 2");
    assert!(r1.complete);
    // Run 1 finishes now, with a SMALLER key than run 2's - a fresh scan sees it;
    // an old cursor stays where it was, honestly expired only by a generation.
    owner.add(1, 0, b"SECRET early");
    let fresh = first(&owner, "SECRET", None, 10).await;
    assert_eq!(fresh.hits.len(), 2, "a fresh scan is ordered by key, not arrival");
}

#[tokio::test]
async fn cleared_generation_expires_cursor() {
    let owner = MemOwner::default();
    for i in 0..80i64 {
        owner.add(1, i * 8, b"filler");
    }
    let r1 = first(&owner, "SECRET", None, 10).await;
    let token = r1.next.clone().unwrap();
    owner.clear();
    assert_eq!(
        search::scan(&owner, "mcp", "SECRET", Some(&token), 10).await.unwrap_err(),
        ScanError::CursorExpired,
        "a clear is an explicit expiration, never a silent empty page"
    );
}


#[tokio::test]
async fn a_failed_database_read_is_reported_not_blamed_on_the_cursor() {
    // Found live: a corrupted store made every scan answer "cursor-invalid",
    // sending the user chasing cursor state while the disk was the problem.
    let err = search::scan(&FailOwner, "terminal", "needle", None, 10).await.unwrap_err();
    match &err {
        ScanError::ScanFailed(reason) => assert_eq!(reason, "database disk image is malformed"),
        other => panic!("expected ScanFailed, got {other:?}"),
    }
    assert_eq!(err.as_str(), "scan-failed");
    // Display is what /api/history/query puts in errors[].reason (services/api.rs):
    // the wire carries the engine's sentence, not just the wire word.
    assert_eq!(err.to_string(), "scan-failed: database disk image is malformed");
}

#[tokio::test]
async fn denied_query_leaks_no_hit_ids_or_counts() {
    // A forged cursor carries no authority: flipping one hex digit breaks the
    // signature, and the refusal shape carries no ids or counts.
    let owner = MemOwner::default();
    owner.add(1, 0, b"SECRET");
    let r1 = first(&owner, "SECRET", None, 1).await;
    let token = r1.next.clone().unwrap();
    let hex = token.encode();
    let tampered = {
        let flip = if hex.starts_with('0') { '1' } else { '0' };
        format!("{flip}{}", &hex[1..])
    };
    // The signature check lives in scan() - it needs the full owner string, which
    // decode() does not know. A forged cursor carries no authority there either,
    // and the refusal shape carries no ids or counts.
    let forged = ScanCursor::decode(&tampered).unwrap();
    let bad = search::scan(&owner, "mcp", "SECRET", Some(&forged), 10)
        .await
        .unwrap_err();
    assert_eq!(bad, ScanError::CursorInvalid);
    assert_eq!(bad.as_str(), "cursor-invalid");
    // Cross-owner and changed-query reuse are refused by name, before any read.
    let token = ScanCursor::decode(&hex).unwrap();
    assert_eq!(
        search::scan(&owner, "remote", "SECRET", Some(&token), 10).await.unwrap_err(),
        ScanError::OwnerMismatch
    );
    assert_eq!(
        search::scan(&owner, "mcp", "OTHER", Some(&token), 10).await.unwrap_err(),
        ScanError::QueryChanged
    );
}


// --- code-review C1: a dense anchor chunk never loses its excess hits ------------------------------

#[tokio::test]
async fn a_dense_anchor_chunk_never_loses_its_excess_hits() {
    // One chunk holding more matches than the limit: the page may exceed the limit
    // (the anchor chunk is answered IN FULL), but no match may silently vanish.
    let owner = MemOwner::default();
    let mut plain = Vec::new();
    for _ in 0..30 {
        plain.extend_from_slice(b"SECRET ");
    }
    owner.add(1, 0, &plain);
    let r = first(&owner, "SECRET", None, 20).await;
    let mut total = r.hits.len();
    let mut next = r.next.clone();
    let mut guard = 0;
    while let Some(cursor) = next {
        guard += 1;
        assert!(guard < 10, "paging terminates");
        let page = first(&owner, "SECRET", Some(&cursor), 20).await;
        total += page.hits.len();
        next = page.next.clone();
    }
    assert_eq!(total, 30, "every match in the dense chunk is reported: {total}");
}
