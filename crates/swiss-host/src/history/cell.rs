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

//! One stored body part, decoded for the Data page (SPEC §data.sqlite "Decoded cells").
//!
//! Every history owner stores a body the same way: chunked, codec-picked, sealed with
//! the row's identity inside the envelope (§host.history). The grid can only show such
//! a `block` as bytes. The owner hands this decoder its own store handle, its keys and
//! the tables it wrote; the decoder reads the row again by its primary key, opens the
//! envelope against the binding the writer used, and decompresses the part. A table or
//! column the owner did not list is not decoded, so no name from the request reaches SQL.

use std::sync::Arc;

use serde_json::{Map, Value};
use sqlx::Row;

use crate::history::body::{self, BodyCodec};
use crate::history::seal::{ChunkBinding, ChunkKeys, OpenBlock};
use crate::history::store::ModuleHistoryStore;
use crate::services::module_db::{CellRef, ChunkFacts, DecodedCell, ModuleCellDecoder};

/// How a table's seal binding names the record its parts belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordBinding {
    /// Resource `""`, event = the integer record id (a call, turn, session or run).
    Event,
    /// Resource = the record's text id, event 0 (an llm piece's hash).
    Resource,
}

/// One table of body parts, described the way its owner writes it.
#[derive(Debug, Clone, Copy)]
pub struct BodyPartTable {
    pub table: &'static str,
    /// The column naming the record (`call_id`, `hash`, `session`, `run_id`).
    pub record_col: &'static str,
    /// The record's noun the panel shows (`call`, `piece`, `session`, `run`).
    pub record: &'static str,
    pub binding: RecordBinding,
    /// Whether the row stores its plaintext offset (`ofs`). Without it the offset is
    /// the sum of the earlier parts' plain lengths, which is what the writer counted.
    pub has_ofs: bool,
    /// The primary key the grid addresses a row by.
    pub key: &'static [&'static str],
}

/// The column every body-part table keeps its stored bytes in.
pub const BLOCK_COLUMN: &str = "block";

/// A history owner's cell decoder: the owner's module id, store, keys and tables.
pub struct BodyPartDecoder {
    module: &'static str,
    store: Arc<ModuleHistoryStore>,
    keys: ChunkKeys,
    tables: &'static [BodyPartTable],
}

impl BodyPartDecoder {
    pub fn new(
        module: &'static str,
        store: Arc<ModuleHistoryStore>,
        keys: ChunkKeys,
        tables: &'static [BodyPartTable],
    ) -> Self {
        BodyPartDecoder { module, store, keys, tables }
    }
}

/// A key value the grid sent: integers bind as integers, text as text. SQLite's column
/// affinity turns a text `"42"` (a snowflake beyond 2^53 travels as text) into the
/// integer it names when compared against an INTEGER column.
#[derive(Debug, Clone)]
enum KeyValue {
    Int(i64),
    Text(String),
}

fn key_values(spec: &BodyPartTable, key: &Map<String, Value>) -> Result<Vec<KeyValue>, String> {
    spec.key
        .iter()
        .map(|col| match key.get(*col) {
            Some(Value::Number(n)) => n.as_i64().map(KeyValue::Int),
            Some(Value::String(s)) => Some(KeyValue::Text(s.clone())),
            _ => None,
        }
        .ok_or_else(|| format!("the row key must name {} for {}", spec.key.join(", "), spec.table)))
        .collect()
}

/// The stored row and the two facts read beside it.
#[derive(Debug, Clone)]
pub struct FetchedPart {
    pub record_int: i64,
    pub record_text: String,
    pub part: i64,
    pub codec: i64,
    pub plain: i64,
    pub block: Vec<u8>,
    pub ofs: i64,
    pub parts: i64,
}

async fn fetch(
    pool: sqlx::SqlitePool,
    spec: BodyPartTable,
    key: Vec<KeyValue>,
) -> Result<Option<FetchedPart>, sqlx::Error> {
    // Every identifier below is one of the owner's literals (BodyPartTable is static);
    // the request supplies only bound values.
    let ofs = if spec.has_ofs { ", ofs" } else { "" };
    let filter = spec.key.iter().map(|c| format!("{c} = ?")).collect::<Vec<_>>().join(" AND ");
    let sql = format!(
        "SELECT {rec}, part, codec, plain, block{ofs} FROM {table} WHERE {filter}",
        rec = spec.record_col,
        table = spec.table,
    );
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    for v in key {
        q = match v {
            KeyValue::Int(n) => q.bind(n),
            KeyValue::Text(s) => q.bind(s),
        };
    }
    let Some(row) = q.fetch_optional(&pool).await? else {
        return Ok(None);
    };
    let (record_int, record_text) = match spec.binding {
        RecordBinding::Event => {
            let n: i64 = row.try_get(0)?;
            (n, n.to_string())
        }
        RecordBinding::Resource => (0, row.try_get::<String, _>(0)?),
    };
    let part: i64 = row.try_get(1)?;
    let bind_record = |q: sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>| match spec.binding {
        RecordBinding::Event => q.bind(record_int),
        RecordBinding::Resource => q.bind(record_text.clone()),
    };
    let parts: i64 = bind_record(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {} WHERE {} = ?",
        spec.table, spec.record_col
    ))))
    .fetch_one(&pool)
    .await?
    .try_get(0)?;
    let ofs: i64 = if spec.has_ofs {
        row.try_get(5)?
    } else {
        bind_record(sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT COALESCE(SUM(plain), 0) FROM {} WHERE {} = ? AND part < ?",
            spec.table, spec.record_col
        ))))
        .bind(part)
        .fetch_one(&pool)
        .await?
        .try_get(0)?
    };
    Ok(Some(FetchedPart {
        record_int,
        record_text,
        part,
        codec: row.try_get(2)?,
        plain: row.try_get(3)?,
        block: row.try_get(4)?,
        ofs,
        parts,
    }))
}

/// Open and decompress one fetched part against the binding its writer sealed it with.
/// Pure, so the seal and codec rules are testable without a store.
pub fn open_part(
    module: &str,
    keys: &ChunkKeys,
    spec: &BodyPartTable,
    row: FetchedPart,
) -> Result<DecodedCell, String> {
    let codec = u8::try_from(row.codec)
        .ok()
        .and_then(BodyCodec::from_u8)
        .ok_or_else(|| format!("codec {} is not one this build wrote", row.codec))?;
    let plain = usize::try_from(row.plain).map_err(|_| format!("the stored plain length {} is corrupt", row.plain))?;
    let ofs = u64::try_from(row.ofs).map_err(|_| format!("the stored offset {} is corrupt", row.ofs))?;
    let binding = match spec.binding {
        RecordBinding::Event => ChunkBinding::row(module, "", row.record_int, row.part, ofs, plain, codec.as_u8()),
        RecordBinding::Resource => ChunkBinding::row(module, &row.record_text, 0, row.part, ofs, plain, codec.as_u8()),
    };
    let stored = row.block.len() as u64;
    let (coded, sealed) = match keys.open_block(&row.block, &binding) {
        OpenBlock::Sealed(coded) => (coded, true),
        OpenBlock::Plain => (row.block, false),
        OpenBlock::Refused => {
            return Err(
                "the sealed part did not open: no key on this machine opens it, or it is bound to another row".into(),
            )
        }
    };
    let bytes = body::decode_chunk(codec, &coded, plain).map_err(|e| e.to_string())?;
    // A whole body (one part) that is a JSON object or array reads as JSON; a part cut
    // from a longer body is text, since its JSON does not close.
    let looks_json = row.parts == 1
        && matches!(bytes.iter().find(|b| !b.is_ascii_whitespace()), Some(b'{' | b'['))
        && serde_json::from_slice::<serde::de::IgnoredAny>(&bytes).is_ok();
    Ok(DecodedCell {
        kind: if looks_json { "json" } else { "text" },
        text: Some(String::from_utf8_lossy(&bytes).into_owned()),
        vector: None,
        record: spec.record,
        record_id: row.record_text,
        chunk: Some(ChunkFacts {
            part: row.part,
            parts: row.parts,
            codec: match codec {
                BodyCodec::Raw => "raw",
                BodyCodec::Lz4 => "lz4",
            },
            sealed,
            plain: plain as u64,
            stored,
            ofs,
        }),
        vector_facts: None,
    })
}

#[async_trait::async_trait]
impl ModuleCellDecoder for BodyPartDecoder {
    async fn decode(&self, cell: &CellRef<'_>) -> Result<Option<DecodedCell>, String> {
        let Some(spec) = self.tables.iter().find(|t| t.table == cell.table).copied() else {
            return Ok(None);
        };
        if cell.column != BLOCK_COLUMN {
            return Ok(None);
        }
        let key = key_values(&spec, cell.key)?;
        let fetched = self
            .store
            .query(move |pool| Box::pin(fetch(pool, spec, key)))
            .await
            .map_err(|e| e.to_string())?;
        let row = fetched.ok_or_else(|| format!("no {} row has that key any more (pruned since the page was read)", spec.table))?;
        open_part(self.module, &self.keys, &spec, row).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL_BODY: BodyPartTable = BodyPartTable {
        table: "call_body",
        record_col: "call_id",
        record: "call",
        binding: RecordBinding::Event,
        has_ofs: false,
        key: &["id"],
    };

    fn plain_row(text: &[u8], part: i64, parts: i64) -> FetchedPart {
        let (codec, coded) = body::encode_chunk(text);
        FetchedPart {
            record_int: 42,
            record_text: "42".into(),
            part,
            codec: codec.as_u8() as i64,
            plain: text.len() as i64,
            block: coded,
            ofs: 0,
            parts,
        }
    }

    #[test]
    fn a_plain_lz4_part_decodes_to_its_text_and_says_where_it_came_from() {
        let text = format!("{{\"model\":\"glm\",\"messages\":[{}]}}", "{\"role\":\"user\"},".repeat(40).trim_end_matches(','));
        let cell = open_part("llm", &ChunkKeys::default(), &CALL_BODY, plain_row(text.as_bytes(), 0, 1)).expect("decodes");
        assert_eq!(cell.kind, "json", "a whole body that parses reads as JSON");
        assert_eq!(cell.text.as_deref(), Some(text.as_str()));
        assert_eq!((cell.record, cell.record_id.as_str()), ("call", "42"));
        let chunk = cell.chunk.expect("chunk facts");
        assert_eq!(chunk.codec, "lz4", "repetitive text compresses");
        assert!(!chunk.sealed, "a degraded store writes plain blocks");
        assert_eq!((chunk.part, chunk.parts, chunk.plain), (0, 1, text.len() as u64));
        assert!(chunk.stored < chunk.plain);
    }

    #[test]
    fn a_sealed_part_opens_against_its_own_row_and_refuses_another() {
        let keys = ChunkKeys::from_master([7u8; 32]);
        let text = b"second part of a turn body";
        let (codec, coded) = body::encode_chunk(text);
        // The writer's binding: llm call 42, part 1, starting at byte 65536.
        let binding = ChunkBinding::row("llm", "", 42, 1, 65_536, text.len(), codec.as_u8());
        let block = keys.seal_block("history:llm:body", &binding, &coded);
        let row = FetchedPart { part: 1, ofs: 65_536, block, parts: 2, ..plain_row(text, 1, 2) };
        let cell = open_part("llm", &keys, &CALL_BODY, row.clone()).expect("opens");
        assert_eq!(cell.text.as_deref(), Some("second part of a turn body"));
        let chunk = cell.chunk.expect("facts");
        assert!(chunk.sealed);
        assert_eq!((chunk.part, chunk.parts, chunk.ofs), (1, 2, 65_536));
        let moved = FetchedPart { record_int: 43, record_text: "43".into(), ..row.clone() };
        assert!(open_part("llm", &keys, &CALL_BODY, moved).unwrap_err().contains("did not open"), "another row's envelope is refused");
        assert!(open_part("terminal", &keys, &CALL_BODY, row).is_err(), "another module's binding is refused");
    }

    #[test]
    fn a_part_of_a_longer_body_is_text_even_when_it_starts_like_json() {
        let cell = open_part("llm", &ChunkKeys::default(), &CALL_BODY, plain_row(b"{\"a\": 1}", 0, 3)).expect("decodes");
        assert_eq!(cell.kind, "text");
        assert_eq!(cell.chunk.expect("facts").parts, 3);
    }

    #[test]
    fn a_corrupt_codec_or_size_is_an_error_not_bytes_from_elsewhere() {
        let mut row = plain_row(b"hello", 0, 1);
        row.codec = 9;
        assert!(open_part("llm", &ChunkKeys::default(), &CALL_BODY, row).unwrap_err().contains("codec 9"));
        let mut row = plain_row(b"hello", 0, 1);
        row.plain = 4;
        assert!(open_part("llm", &ChunkKeys::default(), &CALL_BODY, row).is_err());
    }

    #[test]
    fn the_key_must_name_every_primary_key_column() {
        let mut key = Map::new();
        assert!(key_values(&CALL_BODY, &key).unwrap_err().contains("id"));
        key.insert("id".into(), Value::Bool(true));
        assert!(key_values(&CALL_BODY, &key).is_err(), "a boolean names no row");
        key.insert("id".into(), Value::String("9007199254740993".into()));
        assert!(matches!(key_values(&CALL_BODY, &key).unwrap()[0], KeyValue::Text(_)));
    }
}
