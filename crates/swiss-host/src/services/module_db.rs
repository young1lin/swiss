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

//! The module databases the Data page can browse (SPEC §data.sqlite, §host.history).
//!
//! Each plugin publishes its OWN file when it opens that file, and withdraws it when
//! it stops. Nothing else names the path: a terminal test does not learn where the
//! memory store lives, and the catalog does not keep a list of module names. A history
//! owner is enrolled by the same `history_query.register` it already performs; any
//! other owner calls [`ModuleDbSeat::publish`] beside its own open.
//!
//! A cell the grid can only show as bytes (a sealed, LZ4-coded body part; an f32
//! vector) is decoded by the module that wrote it, never by the browser: the owner
//! sets a [`ModuleCellDecoder`] on its file beside the publish, the decoder reads
//! the row from its own database by the primary key the grid shows, and a stop takes
//! the decoder back with the file (SPEC §data.sqlite "Decoded cells").

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value};

/// One open module database. The plugin id is the owner's, the path is the file it
/// opened. The Data connection id and label are derived from these two, never stored
/// separately, so a rename of the rule cannot drift from the file.
#[derive(Clone)]
pub struct ModuleDatabase {
    pub plugin: String,
    pub path: PathBuf,
}

/// One cell to decode: the table and column the grid shows, and the row's primary
/// key as the grid read it (`{"id": 42}`, `{"session": 7, "part": 0}`). The decoder
/// never trusts bytes from the browser; it reads the row again by this key.
pub struct CellRef<'a> {
    pub table: &'a str,
    pub column: &'a str,
    pub key: &'a Map<String, Value>,
}

/// What one sealed or coded body part is: its place in the record and how it was
/// stored. `ofs` is the part's first plaintext byte within the whole record body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkFacts {
    pub part: i64,
    pub parts: i64,
    /// `raw` or `lz4`.
    pub codec: &'static str,
    /// True when the block was a sealed envelope this machine opened; false for a
    /// plain (pre-sealing or degraded) block.
    pub sealed: bool,
    pub plain: u64,
    pub stored: u64,
    pub ofs: u64,
}

/// What one stored vector is.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorFacts {
    pub model: String,
    pub dims: usize,
    /// The L2 norm; an index vector is normalized, so this reads 1.0.
    pub norm: f64,
}

/// A decoded cell. `kind` is `text`, `json` (the whole text parses as JSON) or
/// `vector`; `record` names what the row belongs to (`call`, `turn`, `piece`,
/// `session`, `run`, `memory`) and `record_id` which one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedCell {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
    pub record: &'static str,
    pub record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<ChunkFacts>,
    #[serde(rename = "vectorFacts", skip_serializing_if = "Option::is_none")]
    pub vector_facts: Option<VectorFacts>,
}

/// The owner's reading of its own cells. `Ok(None)` is "not a cell this module
/// decodes" (the grid keeps its hex view); `Err` is a cell it does decode that did
/// not open (the row is gone, the seal refused), said in English for the panel.
#[async_trait::async_trait]
pub trait ModuleCellDecoder: Send + Sync {
    async fn decode(&self, cell: &CellRef<'_>) -> Result<Option<DecodedCell>, String>;
}

/// One file's decoder, keyed like the row it decodes.
struct DecoderSeat {
    plugin: String,
    path: PathBuf,
    decoder: Arc<dyn ModuleCellDecoder>,
}

/// The seat. One per gateway, shared with the history-query seat so a history
/// registration and a direct publish land in the same list.
#[derive(Default)]
pub struct ModuleDbSeat {
    rows: Mutex<Vec<ModuleDatabase>>,
    decoders: Mutex<Vec<DecoderSeat>>,
}

impl ModuleDbSeat {
    pub fn new() -> Self {
        ModuleDbSeat::default()
    }

    /// The plugin opened this file. A second publish of the same plugin and path
    /// is the same row (a restart that opens again).
    pub fn publish(&self, plugin: &str, path: PathBuf) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if rows.iter().any(|r| r.plugin == plugin && r.path == path) {
            return;
        }
        rows.push(ModuleDatabase { plugin: plugin.to_string(), path });
    }

    /// The plugin stopped. Every file it published goes with it, and so does every
    /// decoder it set: the store handle a decoder reads through closes with the plugin.
    pub fn withdraw(&self, plugin: &str) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.retain(|r| r.plugin != plugin);
        drop(rows);
        let mut decoders = self.decoders.lock().unwrap_or_else(|e| e.into_inner());
        decoders.retain(|d| d.plugin != plugin);
    }

    /// The plugin decodes cells of a file it published. A second call for the same
    /// file replaces the first (a restart hands over its new store handle).
    pub fn set_decoder(&self, plugin: &str, path: PathBuf, decoder: Arc<dyn ModuleCellDecoder>) {
        let mut decoders = self.decoders.lock().unwrap_or_else(|e| e.into_inner());
        decoders.retain(|d| !(d.plugin == plugin && d.path == path));
        decoders.push(DecoderSeat { plugin: plugin.to_string(), path, decoder });
    }

    /// The decoder for one published file, looked up per request: the handle it
    /// holds is the one the running plugin set, never a stopped instance's.
    pub fn decoder(&self, plugin: &str, path: &Path) -> Option<Arc<dyn ModuleCellDecoder>> {
        let decoders = self.decoders.lock().unwrap_or_else(|e| e.into_inner());
        decoders
            .iter()
            .find(|d| d.plugin == plugin && d.path == path)
            .map(|d| d.decoder.clone())
    }

    pub fn list(&self) -> Vec<ModuleDatabase> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The Data connection id for one published file. The stem `store` is the memory
/// plugin's primary file (`store.sqlite3` → `swiss-memory`); any other stem that
/// is not the plugin id is a second file of that plugin (`code.sqlite3` →
/// `swiss-memory-code`).
pub fn connection_id(plugin: &str, path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(plugin);
    if stem == plugin || stem == "store" {
        format!("swiss-{plugin}")
    } else {
        format!("swiss-{plugin}-{stem}")
    }
}

/// What the picker shows. The home directory stays out of it: `logs/data.db`,
/// `memory/store.sqlite3`.
pub fn connection_label(path: &Path) -> String {
    let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("db");
    match path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) {
        Some(parent @ ("logs" | "memory")) => format!("{parent}/{file}"),
        _ => file.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_publishes_its_own_file_and_a_stop_takes_it_back() {
        let seat = ModuleDbSeat::new();
        let store = PathBuf::from("memory").join("store.sqlite3");
        let code = PathBuf::from("memory").join("code.sqlite3");
        seat.publish("memory", store.clone());
        seat.publish("memory", code.clone());
        seat.publish("memory", store.clone());
        let rows = seat.list();
        assert_eq!(rows.len(), 2, "a repeat publish is the same row");
        assert_eq!(connection_id("memory", &store), "swiss-memory");
        assert_eq!(connection_id("memory", &code), "swiss-memory-code");
        assert_eq!(connection_label(&store), "memory/store.sqlite3");
        assert_eq!(connection_id("data", Path::new("logs/data.db")), "swiss-data");
        assert_eq!(connection_label(Path::new("logs/data.db")), "logs/data.db");
        seat.withdraw("memory");
        assert!(seat.list().is_empty());
    }

    struct Fixed;

    #[async_trait::async_trait]
    impl ModuleCellDecoder for Fixed {
        async fn decode(&self, _cell: &CellRef<'_>) -> Result<Option<DecodedCell>, String> {
            Ok(None)
        }
    }

    #[test]
    fn a_decoder_belongs_to_its_file_and_leaves_with_the_plugin() {
        let seat = ModuleDbSeat::new();
        let llm = PathBuf::from("logs").join("llm.db");
        seat.publish("llm", llm.clone());
        seat.set_decoder("llm", llm.clone(), Arc::new(Fixed));
        assert!(seat.decoder("llm", &llm).is_some());
        assert!(seat.decoder("llm", Path::new("logs/other.db")).is_none(), "another file has none");
        assert!(seat.decoder("terminal", &llm).is_none(), "another plugin has none");
        seat.withdraw("llm");
        assert!(seat.decoder("llm", &llm).is_none(), "a stop takes the decoder back");
    }
}
