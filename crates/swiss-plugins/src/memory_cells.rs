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

//! The memory store's embedding vectors, decoded for the Data page (SPEC §data.sqlite
//! "Decoded cells"). `memory_vectors.vec` is little-endian f32, L2-normalized, `dims`
//! wide (SPEC §memory.embedding); the grid shows it as bytes, this reads it as numbers.
//! The row is read again from the serving store by its `memory_id`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use swiss_host::services::module_db::{CellRef, DecodedCell, ModuleCellDecoder, VectorFacts};

use crate::memory_api::MemoryState;

const TABLE: &str = "memory_vectors";
const COLUMN: &str = "vec";

pub(crate) struct MemoryVectorDecoder {
    state: Arc<MemoryState>,
}

impl MemoryVectorDecoder {
    pub(crate) fn new(state: Arc<MemoryState>) -> Self {
        MemoryVectorDecoder { state }
    }
}

/// One stored vector as the panel reads it. A blob that is not whole f32 values, or
/// whose width is not the row's `dims`, is said, never shown as a shorter vector.
pub(crate) fn vector_cell(id: String, model: String, dims: i64, bytes: &[u8]) -> Result<DecodedCell, String> {
    let vector = swiss_memory::embed::from_bytes(bytes)
        .ok_or_else(|| format!("the vector's {} bytes are not whole f32 values", bytes.len()))?;
    if i64::try_from(vector.len()).ok() != Some(dims) {
        return Err(format!("the vector holds {} values but the row says dims {dims}", vector.len()));
    }
    let norm = vector.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    Ok(DecodedCell {
        kind: "vector",
        text: None,
        record: "memory",
        record_id: id,
        chunk: None,
        vector_facts: Some(VectorFacts { model, dims: vector.len(), norm }),
        vector: Some(vector),
    })
}

#[async_trait]
impl ModuleCellDecoder for MemoryVectorDecoder {
    async fn decode(&self, cell: &CellRef<'_>) -> Result<Option<DecodedCell>, String> {
        if cell.table != TABLE || cell.column != COLUMN {
            return Ok(None);
        }
        let Some(Value::String(id)) = cell.key.get("memory_id") else {
            return Err("the row key must name memory_id for memory_vectors".into());
        };
        let service = self.state.current().ok_or("the memory service is not running")?;
        let store = service.store().await.map_err(|e| e.to_string())?;
        let mut conn = store.read().await.map_err(|e| e.to_string())?;
        let (model, dims, bytes) = swiss_memory::store::stored_vector(&mut conn, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("no memory_vectors row has that key any more (re-embedded or deleted since the page was read)")?;
        vector_cell(id.clone(), model, dims, &bytes).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn a_stored_vector_reads_back_as_its_numbers_with_width_and_norm() {
        let cell = vector_cell("m-1".into(), "glm/embedding-3".into(), 2, &le(&[0.6, 0.8])).expect("decodes");
        assert_eq!(cell.kind, "vector");
        assert_eq!(cell.vector.as_deref(), Some(&[0.6f32, 0.8][..]));
        assert_eq!((cell.record, cell.record_id.as_str()), ("memory", "m-1"));
        let facts = cell.vector_facts.expect("facts");
        assert_eq!((facts.model.as_str(), facts.dims), ("glm/embedding-3", 2));
        assert!((facts.norm - 1.0).abs() < 1e-6, "an index vector is normalized: {}", facts.norm);
    }

    #[test]
    fn a_torn_blob_or_a_wrong_width_is_said_not_shown_short() {
        assert!(vector_cell("m".into(), "x".into(), 1, &[0, 0, 128]).unwrap_err().contains("whole f32"));
        assert!(vector_cell("m".into(), "x".into(), 3, &le(&[1.0, 0.0])).unwrap_err().contains("dims 3"));
    }

    #[tokio::test]
    async fn only_the_vector_column_is_this_decoders_and_a_stopped_service_is_said() {
        let decoder = MemoryVectorDecoder::new(Arc::new(MemoryState::new()));
        let mut key = serde_json::Map::new();
        key.insert("memory_id".into(), Value::String("m-1".into()));
        let other = CellRef { table: "memories", column: "content", key: &key };
        assert!(decoder.decode(&other).await.expect("not an error").is_none());
        let vec = CellRef { table: TABLE, column: COLUMN, key: &key };
        assert!(decoder.decode(&vec).await.unwrap_err().contains("not running"));
    }
}
