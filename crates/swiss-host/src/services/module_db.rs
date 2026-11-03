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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One open module database. The plugin id is the owner's, the path is the file it
/// opened. The Data connection id and label are derived from these two, never stored
/// separately, so a rename of the rule cannot drift from the file.
#[derive(Clone)]
pub struct ModuleDatabase {
    pub plugin: String,
    pub path: PathBuf,
}

/// The seat. One per gateway, shared with the history-query seat so a history
/// registration and a direct publish land in the same list.
#[derive(Default)]
pub struct ModuleDbSeat {
    rows: Mutex<Vec<ModuleDatabase>>,
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

    /// The plugin stopped. Every file it published goes with it.
    pub fn withdraw(&self, plugin: &str) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.retain(|r| r.plugin != plugin);
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
}
