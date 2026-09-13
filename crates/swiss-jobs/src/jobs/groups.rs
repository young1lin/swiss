//! The jobs GroupScope (docs/20 G4).
//!
//! The thinnest adapter in the family: the system's own methods own every rule (the one
//! model lives in the config row, mutations land through commit_row - strict parse,
//! revision-checked write, apply in place, no restart), and this file only hands them to
//! the host's route family. Registered from the composition point like every scope.

use std::sync::Arc;

use swiss_host::groups::{GroupScope, GroupScopes};

use super::JobSystem;

struct JobGroups {
    system: Arc<JobSystem>,
}

impl GroupScope for JobGroups {
    fn names(&self) -> Vec<String> {
        self.system.groups()
    }

    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String> {
        self.system.set_groups(&next)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        self.system.rename_group(from, to)
    }

    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String> {
        self.system.set_job_group(id, group)
    }

    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String> {
        self.system.reorder(&ids)
    }

    fn has_member(&self, id: &str) -> bool {
        self.system.has_job(id)
    }
}

/// Register the jobs scope over one JobSystem. Called from swiss's server.rs, right after
/// the system opens - the same composition point tunnels use.
pub fn register_job_scopes(scopes: &GroupScopes, system: &Arc<JobSystem>) {
    scopes.register("jobs", Arc::new(JobGroups { system: system.clone() }));
}
