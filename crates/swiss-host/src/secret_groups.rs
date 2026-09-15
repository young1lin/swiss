//! The secrets GroupScope (docs/20 G6).
//!
//! The vault is a global in swiss-core, so this adapter is a pure translation: it reads
//! the one model's two lists out of the vault, runs every mutation through the shared
//! [Groups] (all validation, canonical casing and error wording), and lands the result as
//! ONE rev-checked vault write - the same discipline a value write has. Values never
//! cross this boundary: a group label names a folder, not a credential.
//!
//! The order is the model's third list (docs/26): `set_order` lands it in the same
//! single rev-checked vault write, and an empty order keeps the name order every pre-26
//! file had.

use std::collections::BTreeMap;

use swiss_core::secure::secretstore as vault;

use crate::groups::{GroupScope, Groups};

struct SecretGroups;

/// The vault's current one-model view: names + the member map, as [Groups].
fn model() -> Groups {
    let members = vault::list_secrets()
        .into_iter()
        .map(|name| {
            let g = vault::vault_group_of(&name);
            (name, g)
        })
        .collect::<BTreeMap<_, _>>();
    Groups::from_parts(vault::vault_groups(), members)
}

/// Land a mutated model as one vault write, mapping the vault's errors to the family's
/// strings. A regroup is one rev bump (docs/20 G6), so a concurrent value write fails
/// here honestly - the message says reload, which is all a single panel ever needs.
fn commit(groups: Groups, order: Vec<String>) -> Result<(), String> {
    vault::set_vault_groups(
        &vault::secret_store_path(),
        vault::vault_rev(),
        groups.names(),
        groups.members().clone().into_iter().collect(),
        order,
    )
    .map(|_| ())
    .map_err(|e| match e {
        vault::MutateError::RevMismatch { have, saw } => {
            format!("the vault changed since rev {saw} (now at {have}) - reload and retry")
        }
        other => other.message(),
    })
}

impl GroupScope for SecretGroups {
    fn names(&self) -> Vec<String> {
        vault::vault_groups()
    }

    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String> {
        let mut g = model();
        g.set_names(next)?;
        commit(g, vault::vault_order())?;
        Ok(vault::vault_groups())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        let mut g = model();
        let answer = g.rename(from, to)?;
        commit(g, vault::vault_order())?;
        Ok(answer)
    }

    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String> {
        let mut g = model();
        let answer = g.assign(id, group)?;
        commit(g, vault::vault_order())?;
        Ok(answer)
    }

    // docs/26: the order is the model's third list. Unknown names drop out and duplicates
    // keep their first slot - the managed scope's discipline - so a stale panel cannot
    // plant a ghost row.
    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String> {
        let g = model();
        let mut seen = std::collections::HashSet::new();
        let order: Vec<String> = ids
            .into_iter()
            .filter(|id| vault::vault_has(id) && seen.insert(id.clone()))
            .collect();
        commit(g, order)?;
        Ok(vault::vault_order())
    }

    fn has_member(&self, id: &str) -> bool {
        vault::vault_has(id)
    }
}

/// Register the secrets scope over the global vault. Called from the admin API's mount -
/// the same place the vault's own routes live, so the scope and the write-only API it
/// wraps can never drift apart.
pub fn register_secret_scopes(scopes: &crate::groups::GroupScopes) {
    scopes.register("secrets", std::sync::Arc::new(SecretGroups));
}
