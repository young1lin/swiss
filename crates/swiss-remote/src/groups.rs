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

//! The targets GroupScope (docs/34 R8) - the seventh scope of the docs/20 family.
//!
//! Exactly the shape of tunnels' conns/rules pair: one scope over the one sealed
//! table, registered into the host's scope table by the composition point (swiss's
//! server.rs), never by a match arm inside the host. The route family owns the
//! protocol; this owns the semantics only this crate knows: a member is a target
//! id, and the scope's order is array order in the file.
//!
//! The scope holds the RemoteSystem the PLUGIN serves, but its registration outlives
//! start/stop on purpose - the family's own rule (adminapi.rs) is that a stopped
//! plugin must not take another scope's grouping off the air, and the sealed table
//! is safe to mutate at any time: it carries no credentials and no runtime state.

use std::sync::Arc;

use swiss_host::groups::{GroupScope, GroupScopes};

use crate::target::TargetStore;
use crate::RemoteSystem;

struct TargetGroups {
    system: Arc<RemoteSystem>,
}

impl TargetGroups {
    /// Run one mutation under the store lock. A poisoned lock is recovered by
    /// RemoteSystem::with_store itself, so there is no error branch here - the
    /// store's own Result is the whole story.
    fn with<T>(&self, f: impl FnOnce(&mut TargetStore) -> Result<T, String>) -> Result<T, String> {
        self.system.with_store(f)
    }
}

impl GroupScope for TargetGroups {
    fn names(&self) -> Vec<String> {
        self.system.with_store(|s| s.group_names())
    }

    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String> {
        self.with(|s| s.set_group_names(&next))
    }

    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        self.with(|s| s.rename_group(from, to))
    }

    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String> {
        self.with(|s| s.set_target_group(id, group))
    }

    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String> {
        // The persisted order is the answer: what the file now holds, not what was
        // asked (the same contract the tunnel scopes answer under).
        self.with(|s| s.reorder(&ids).map(|_| ids.clone()))
    }

    fn has_member(&self, id: &str) -> bool {
        self.system.with_store(|s| s.get(id).is_some())
    }
}

/// Register the `targets` scope over the remote system's sealed table. The
/// composition point calls this once, right after the system exists - the same
/// shape as every other subsystem joining the host table.
pub fn register_remote_scopes(scopes: &GroupScopes, system: &Arc<RemoteSystem>) {
    scopes.register(
        "targets",
        Arc::new(TargetGroups {
            system: system.clone(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_host::services::RuntimeServices;

    fn system_with_targets() -> (Arc<RemoteSystem>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "swiss-rgroup-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let system = RemoteSystem::open(RuntimeServices::new(), dir.join("remote.json"));
        system
            .with_store(|s| {
                s.set_group_names(&["default".into(), "prod".into()])
                    .map(|_| ())
                    .and_then(|_| {
                        s.add(crate::target::RemoteTarget {
                            id: "build".into(),
                            label: "Build".into(),
                            endpoint: "conn-1".into(),
                            workspace_root: "/data/ws".into(),
                            shell: "posix".into(),
                            capabilities: vec!["exec".into()],
                            default_timeout_ms: None,
                            group: Some("PROD".into()),
                        })
                    })
            })
            .expect("seed");
        (system, dir)
    }

    #[test]
    fn the_scope_serves_the_family_contract() {
        let (system, dir) = system_with_targets();
        let scopes = GroupScopes::new();
        register_remote_scopes(&scopes, &system);
        let scope = scopes.get("targets").expect("registered");

        assert_eq!(scope.names(), vec!["default".to_string(), "prod".to_string()]);
        // The row's explicit group was canonicalized at the door, and assign answers
        // the canonical casing back.
        assert_eq!(system.with_store(|s| s.group_of("build")), "prod");
        assert_eq!(scope.assign("build", Some("DEFAULT")).unwrap(), "default");
        // has_member gates the member PUT's 404; a stranger is not a member.
        assert!(scope.has_member("build"));
        assert!(!scope.has_member("ghost"));
        // set_order answers the order it persisted.
        assert_eq!(
            scope.set_order(vec!["ghost-first".into(), "build".into()]).unwrap(),
            vec!["ghost-first".to_string(), "build".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
