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

//! The history-storage factory seat (SPEC §host.history).
//!
//! The host hands every module owner its OWN store through this factory; it never shares a
//! pool, a writer queue or a lifecycle between two owners. The identity is opaque — there
//! is no MCP-versus-Remote match arm here — and construction performs no I/O: a disabled
//! module's store opens, migrates and restores nothing until its owner explicitly opens it.

use std::sync::Arc;

use crate::history::store::{ModuleHistoryStore, ModuleId, OwnerPolicy};

/// The seat the composition root constructs once and every enabled owner creates through.
pub struct HistoryStoreFactory;

impl HistoryStoreFactory {
    pub fn new() -> Arc<Self> {
        Arc::new(HistoryStoreFactory)
    }

    /// Create one owner's store. Pure construction: no file is touched, no engine probed,
    /// no pool opened — the store starts [disabled][crate::history::store::OwnerState]
    /// and only its owner's `open` makes it live.
    pub fn create(&self, id: &str, policy: OwnerPolicy) -> Arc<ModuleHistoryStore> {
        ModuleHistoryStore::new(ModuleId::new(id), policy)
    }
}

impl Default for HistoryStoreFactory {
    fn default() -> Self {
        HistoryStoreFactory
    }
}
