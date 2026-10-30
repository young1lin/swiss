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

use super::*;

#[tokio::test]
async fn a_landed_tail_never_carries_plaintext_beyond_the_preview_in_the_raw_database() {
    let dir = swiss_core::paths::test_home().join("llm-hist-tail-privacy");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let id = run.id().expect("reserved");
    // The TOKEN-MATCHED path that actually lands: the mark is committed
    // and unsettled (no call row cleared it), so the tail write below is
    // the live one - never a pending-already-cleared no-op that would
    // fake the confidentiality green.
    run.begin_round().await.expect("the activity mark commits");
    // A REAL tail past every preview boundary: 2.8 KiB of tool content
    // whose unique sentinel sits well beyond the 2 KiB preview window, so
    // its presence in a raw file can only mean the FULL plaintext landed
    // there.
    let sentinel = "PRIVACY-SENTINEL-4b1e-never-in-a-preview";
    let text = format!("{}{sentinel}", "t".repeat(2800));
    assert!(text.len() > 2600, "the tail is a real oversized one");
    let filed = json!({ "tools": [{ "id": "call_big", "name": "big", "text": text }] });
    run.file_tail(filed.clone());
    history.flush().await;
    // The canonical value reads back EXACT through the DECODE SEAM - the
    // one read path production has: the run column's reference opens
    // authenticated, piece by piece, and the exact filed value rebuilds.
    // The write provably landed through the production path, on the
    // admitted store, visible to the very next read.
    let read_back = history
        .store
        .query(move |pool| {
            Box::pin(async move {
                sqlx::query("SELECT tail FROM run WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&pool)
                    .await?
                    .map(|row| row.try_get::<String, _>("tail"))
                    .transpose()
            })
        })
        .await
        .expect("the tail read must succeed");
    let landed = read_back.expect("the token-matched tail write must LAND");
    // The RAW column carries a NONEMPTY sealed reference - hashes, never
    // plaintext: the landing guard and the privacy boundary in one.
    assert!(!landed.trim().is_empty(), "the raw tail column carries a nonempty sealed reference");
    assert!(!landed.contains(sentinel), "the raw tail reference is hashes, not plaintext: {landed}");
    let decoded = history
        .decode_tail(Some(&landed))
        .await
        .expect("the sealed tail decodes")
        .expect("a landed tail is present");
    assert_eq!(decoded, filed, "the canonical value reads back exact through the decode seam");
    // ENGAGEMENT GUARD (authenticated encryption, not just absence): the
    // referenced pieces have NONEMPTY chunk rows and EVERY stored block
    // is a genuine sealed envelope - the frozen `lmg`/`alg`/`ct` shape a
    // compressed-but-unsealed fallback row would not carry. A plain
    // block would decode without a key and could fake the sentinel
    // absence; under this key-pinned run the rows must be sealed ones.
    // Read through the store's own rows; no new machinery.
    let referenced: Vec<String> = serde_json::from_str::<Value>(&landed)
        .expect("the reference decodes")
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().filter_map(|hash| hash.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(!referenced.is_empty(), "the raw reference names its pieces: {landed}");
    for hash in referenced {
        // The query job takes an OWNED clone; `hash` itself stays here
        // for the diagnostics below (the move must not eat them).
        let owned = hash.clone();
        let rows = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT part, block FROM piece_body WHERE hash = ? ORDER BY part")
                        .bind(&owned)
                        .fetch_all(&pool)
                        .await?
                        .into_iter()
                        .map(|row| Ok::<(i64, Vec<u8>), sqlx::Error>((row.try_get(0)?, row.try_get(1)?)))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .await
            .expect("the piece chunk read must succeed");
        assert!(!rows.is_empty(), "the referenced piece {hash} has chunk rows");
        for (part, block) in rows {
            let parsed: Value = serde_json::from_slice(&block).unwrap_or(Value::Null);
            assert!(
                swiss_core::secure::envelope::is_sealed(&parsed),
                "part {part} of piece {hash} must be a genuine sealed envelope, not an unsealed fallback block",
            );
        }
    }
    // The privacy boundary itself: bodies ride sealed chunks - the tail
    // must not leak its full plaintext into the raw main database or its
    // WAL/SHM sidecars. The landing is already proven above, so absence
    // here can never be a no-op fake green. A sidecar that does not exist
    // skips on NotFound ONLY - any other read error propagates, never a
    // swallowed failure that could fake the absence.
    let db = history.store.policy().db_file.clone();
    let side = |suffix: &str| std::path::PathBuf::from(format!("{}{suffix}", db.display()));
    for path in [db.clone(), side("-wal"), side("-shm")] {
        // MAIN llm.db must read or the whole guard fails: a home whose
        // main database cannot be opened has nothing proven. Only the
        // optional sidecars skip on NotFound - exactly them, never the
        // main file (SPEC llm.history privacy seam).
        let sidecar = path != db;
        match std::fs::read(&path) {
            Ok(bytes) => assert!(
                !bytes.windows(sentinel.len()).any(|window| window == sentinel.as_bytes()),
                "the tail sentinel must not appear in the raw file {}: the tail column carries full plaintext",
                path.display(),
            ),
            Err(e) if sidecar && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("the raw file {} could not be read: {e}", path.display()),
        }
    }
    history.close().await.expect("close");
}
