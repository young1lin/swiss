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
/// a module stopping and starting again finds the same seat.
#[derive(Default)]
pub struct HistoryQuerySeat {
    adapters: Mutex<Vec<HistoryQueryAdapter>>,
}

impl HistoryQuerySeat {
    pub fn new() -> Self {
        HistoryQuerySeat::default()
    }

    /// Register an adapter. A duplicate owner id is a composition bug: refused,
    /// not last-write-wins.
    pub fn register(&self, adapter: HistoryQueryAdapter) -> Result<(), String> {
        let mut all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        if all.iter().any(|a| a.owner == adapter.owner) {
            return Err(format!(
                "history query adapter for {} is already registered",
                adapter.owner
            ));
        }
        all.push(adapter);
        Ok(())
    }

    /// Withdraw an owner's adapter (its plugin stopped: the database closed).
    pub fn unregister(&self, owner: &str) {
        let mut all = self.adapters.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|a| a.owner != owner);
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
