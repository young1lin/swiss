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

//! SPEC §data.sqlite "Decoded cells" over llm.db: a body part and a piece part, written
//! by the production writers, read back one cell at a time through [LlmHistory::cell_decoder].
//! The two tables bind differently (a call id as the event, a piece hash as the
//! resource), so each must open against its own writer's binding.

use super::*;
use serde_json::{Map, Value};
use sqlx::Row;
use swiss_host::services::module_db::CellRef;

fn id_key(id: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("id".into(), Value::from(id));
    m
}

#[tokio::test]
async fn a_call_body_part_and_a_piece_decode_against_their_own_bindings() {
    let dir = swiss_core::paths::test_home().join("llm-hist-cell-decode");
    let history = LlmHistory::open(&dir).await.expect("open");
    // A body past one 64 KiB part, so the second part's offset is the first's length.
    let body = format!("{{\"messages\":[\"{}\"]}}", "m".repeat(70_000));
    let piece = r#"{"role":"user","content":"which table holds the vectors?"}"#;
    let keys = history.keys.clone();
    let (body_w, piece_w) = (body.clone(), piece.to_string());
    let ids: (i64, i64) = history
        .store
        .query(move |pool| {
            Box::pin(async move {
                let mut tx = pool.begin().await?;
                super::rows::write_chunks_tx(&mut tx, "call_body", "call_id", 7, &body_w, &keys).await?;
                super::rows::write_piece_chunks(&mut tx, "feedbeef", &piece_w, &keys).await?;
                tx.commit().await?;
                let second: i64 = sqlx::query("SELECT id FROM call_body WHERE call_id = 7 AND part = 1")
                    .fetch_one(&pool)
                    .await?
                    .try_get(0)?;
                let piece: i64 = sqlx::query("SELECT id FROM piece_body WHERE hash = 'feedbeef'")
                    .fetch_one(&pool)
                    .await?
                    .try_get(0)?;
                Ok((second, piece))
            })
        })
        .await
        .expect("the writers land");
    let decoder = history.cell_decoder();

    let key = id_key(ids.0);
    let part = decoder
        .decode(&CellRef { table: "call_body", column: "block", key: &key })
        .await
        .expect("decodes")
        .expect("a call body block is decoded");
    assert_eq!(part.kind, "text", "the tail of a longer body is not whole JSON");
    assert_eq!(part.text.as_deref(), Some(&body[65_536..]));
    assert_eq!((part.record, part.record_id.as_str()), ("call", "7"));
    let facts = part.chunk.expect("facts");
    assert_eq!((facts.part, facts.parts, facts.ofs), (1, 2, 65_536), "the offset is the earlier parts summed");

    let key = id_key(ids.1);
    let piece_cell = decoder
        .decode(&CellRef { table: "piece_body", column: "block", key: &key })
        .await
        .expect("decodes")
        .expect("a piece block is decoded");
    assert_eq!(piece_cell.kind, "json", "a whole piece that parses reads as JSON");
    assert_eq!(piece_cell.text.as_deref(), Some(piece));
    assert_eq!((piece_cell.record, piece_cell.record_id.as_str()), ("piece", "feedbeef"));

    // The piece's id under the call table is another row's binding (or no row): never its text.
    let crossed = decoder.decode(&CellRef { table: "turn_body", column: "block", key: &key }).await;
    assert!(crossed.is_err(), "a key from another table names no turn_body row");
    let other = decoder.decode(&CellRef { table: "call", column: "preview_text", key: &key }).await;
    assert!(other.expect("not an error").is_none(), "metadata columns are the grid's own");
    history.close().await.expect("close");
}
