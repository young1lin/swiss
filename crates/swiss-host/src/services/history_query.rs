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

//! The cross-module history query seat (plan docs/plans/module-log-storage.md D5,
//! SPEC `host.history): each module whose database is open registers an adapter on
//! start and withdraws it on stop; the host aggregates whatever is registered. A
//! stopped module is REPORTED SKIPPED, never silently absent, and never auto-opened
//! by a query (SPEC `host.history lifecycle).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::history::search::BodySource;

/// One bounded body slice (plan D5): the plaintext after a byte cursor, the next
/// frontier when more remains, and the honest gone flag for pruned records.
#[derive(serde::Serialize)]
pub struct BodySlice {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<u64>,
    pub gone: bool,
}

/// Reading one stored record's body by the module's internal record id. The module
/// owns the semantics (MCP: the stored call reply; Remote: the run's output) — the
/// host route never learns table names.
#[async_trait::async_trait]
pub trait HistoryBodyReader: Send + Sync {
    async fn read_body(&self, id: i64, after: u64, max: usize) -> Result<BodySlice, String>;
}

/// One module's contribution: its owner id, its database file (for the physical
/// usage report), the scan source over its stored bodies, and — when the module
/// exposes record bodies — the reader for the body endpoint.
pub struct HistoryQueryAdapter {
    pub owner: String,
    pub db_file: PathBuf,
    pub source: Arc<dyn BodySource>,
    pub bodies: Option<Arc<dyn HistoryBodyReader>>,
}

/// The seat. Constructed with the services, so it outlives every plugin instance —
/// a module stopping and starting again finds the same seat. Registration also
/// enrolls the owner's file on [`super::module_db::ModuleDbSeat`]: the Data page
/// reads that seat, and the owner is the only one who named the path.
#[derive(Default)]
pub struct HistoryQuerySeat {
    adapters: Mutex<Vec<HistoryQueryAdapter>>,
    modules: Arc<super::module_db::ModuleDbSeat>,
}

impl HistoryQuerySeat {
    pub fn new() -> Self {
        Self::sharing(Arc::new(super::module_db::ModuleDbSeat::new()))
    }

    /// Share the gateway's module-database seat, so a registration is visible to Data.
    pub fn sharing(modules: Arc<super::module_db::ModuleDbSeat>) -> Self {
        HistoryQuerySeat {
            adapters: Mutex::new(Vec::new()),
            modules,
        }
    }

    /// Register an adapter. A duplicate owner id is a composition bug: refused,
    /// not last-write-wins. The owner's database file is enrolled for browsing
    /// at the same time, and withdrawn by [`unregister`](Self::unregister).
    pub fn register(&self, adapter: HistoryQueryAdapter) -> Result<(), String> {
        let mut all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        if all.iter().any(|a| a.owner == adapter.owner) {
            return Err(format!(
                "history query adapter for {} is already registered",
                adapter.owner
            ));
        }
        self.modules.publish(&adapter.owner, adapter.db_file.clone());
        all.push(adapter);
        Ok(())
    }

    /// Withdraw an owner's adapter (its plugin stopped: the database closed).
    /// Every file that owner published goes with the adapter.
    pub fn unregister(&self, owner: &str) {
        let mut all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|a| a.owner != owner);
        self.modules.withdraw(owner);
    }

    /// The registered body reader for one owner, if it exposes record bodies.
    pub fn body_reader(&self, owner: &str) -> Option<Arc<dyn HistoryBodyReader>> {
        let all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        all.iter().find(|a| a.owner == owner).and_then(|a| a.bodies.clone())
    }

    /// Every registered owner with its database file, for the usage report.
    pub fn owners(&self) -> Vec<(String, PathBuf)> {
        let all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        all.iter().map(|a| (a.owner.clone(), a.db_file.clone())).collect()
    }

    /// The owners a query should fan out to: the requested filter, or all
    /// registered when none was sent.
    pub fn scan_targets(&self, requested: Option<&[String]>) -> (Vec<Arc<dyn BodySource>>, Vec<String>, Vec<String>) {
        let all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        let mut sources = Vec::new();
        let mut owners = Vec::new();
        let mut skipped = Vec::new();
        if let Some(want) = requested {
            for owner in want {
                match all.iter().find(|a| &a.owner == owner) {
                    Some(a) => {
                        sources.push(a.source.clone());
                        owners.push(a.owner.clone());
                    }
                    None => skipped.push(owner.clone()),
                }
            }
        } else {
            for a in all.iter() {
                sources.push(a.source.clone());
                owners.push(a.owner.clone());
            }
        }
        (sources, owners, skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::search::{BodySource, StoredChunk};

    struct Silent;

    impl BodySource for Silent {
        fn chunks_from(
            &self,
            _from: (i64, i64),
            _limit: usize,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<StoredChunk>, String>> + Send + '_>>
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn generation(&self) -> u64 {
            0
        }
    }

    #[test]
    fn registering_an_owner_enrolls_its_own_file_and_unregister_takes_it_back() {
        let modules = Arc::new(super::super::module_db::ModuleDbSeat::new());
        let seat = HistoryQuerySeat::sharing(modules.clone());
        let path = PathBuf::from("logs").join("terminal.db");
        seat.register(HistoryQueryAdapter {
            owner: "terminal".into(),
            db_file: path.clone(),
            source: Arc::new(Silent),
            bodies: None,
        })
        .expect("first registration");
        let rows = modules.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].plugin, "terminal");
        assert_eq!(rows[0].path, path);
        seat.unregister("terminal");
        assert!(modules.list().is_empty(), "the terminal file leaves with the plugin");
    }
}
