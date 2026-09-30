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

//! The conns/rules GroupScope pair (SPEC §host.groups).
//!
//! Two scopes over the one tunnels.json - the way `mcps` is a scope over managed.json -
//! registered into the host's scope table by the composition point (swiss's server.rs),
//! never by a match arm inside the host. The route family owns the protocol; this owns the
//! semantics that only this crate knows: a member is a tunnel id, and a scope's order is
//! array order in the file.

use std::sync::{Arc, Mutex};

use swiss_host::groups::{GroupScope, GroupScopes};

use super::store::TunnelStore;
use super::types::GroupKind;

struct TunnelGroups {
    store: Arc<Mutex<TunnelStore>>,
    kind: GroupKind,
}

impl TunnelGroups {
    fn scope(store: Arc<Mutex<TunnelStore>>, kind: GroupKind) -> Arc<dyn GroupScope> {
        Arc::new(TunnelGroups { store, kind })
    }

    /// Lock the store and run one mutation; a poisoned lock is an error, never a panic -
    /// one wedged scope must not take the admin API down with it.
    fn with<T>(&self, f: impl FnOnce(&mut TunnelStore) -> Result<T, String>) -> Result<T, String> {
        self.store
            .lock()
            .map_err(|_| "tunnels.json is busy".to_string())
            .and_then(|mut s| f(&mut s))
    }
}

impl GroupScope for TunnelGroups {
    fn names(&self) -> Vec<String> {
        self.store
            .lock()
            .map(|s| s.groups_of(self.kind))
            .unwrap_or_default()
    }

    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String> {
        self.with(|s| s.set_groups(self.kind, &next))
    }

    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        self.with(|s| s.rename_group(self.kind, from, to))
    }

    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String> {
        self.with(|s| s.set_group(self.kind, id, group))
    }

    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String> {
        // The persisted order is the answer: what the file now holds, not what was asked.
        self.with(|s| s.reorder(self.kind, &ids).map(|_| ids.clone()))
    }

    fn has_member(&self, id: &str) -> bool {
        self.store
            .lock()
            .map(|s| match self.kind {
                GroupKind::Rules => s.rule(id).is_some(),
                GroupKind::Connections => s.connection(id).is_some(),
            })
            .unwrap_or(false)
    }
}

/// Register the two tunnel scopes (`conns`, `rules`) over one store. The composition point
/// calls this once, right after the store exists - the same shape as every other subsystem
/// joining a host table.
pub fn register_tunnel_scopes(scopes: &GroupScopes, store: &Arc<Mutex<TunnelStore>>) {
    scopes.register(
        "conns",
        TunnelGroups::scope(store.clone(), GroupKind::Connections),
    );
    scopes.register(
        "rules",
        TunnelGroups::scope(store.clone(), GroupKind::Rules),
    );
}
