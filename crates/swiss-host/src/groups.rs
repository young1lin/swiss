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

//! One grouping model for every list the panel shows (SPEC §host.groups).
//!
//! "An ordered list of group names + one group name per member + the first group as the sink"
//! used to exist twice (managed.json, tunnels.json) and was about to be written four more times
//! (jobs, secrets, tokens, the Data picker). The details had already forked between the two
//! copies - whether `default` is renameable, what a rename body looks like, which name a
//! delete-confirm names as the landing group. This module is the one implementation: pure data,
//! no I/O, embedded by each scope's store, which decides only the JSON key names it serializes
//! under (SPEC §formats.groups).
//!
//! Order is deliberately NOT here. Every scope already owns a flat order (the MCP `order` side
//! map, tunnels.json array order, jobs definition order); `Groups` only slices it. That is why
//! moving a member between groups never has to rewrite the ordering - the panel's
//! "one flat order sliced by group" contract.

use std::collections::BTreeMap;

/// The name a group list starts from. An ordinary group the user can rename, delete and
/// reorder; its only privilege is being the initial FIRST entry - that slot (never the name)
/// is where unassigned members and the members of a deleted group land.
pub const DEFAULT_GROUP: &str = "default";

/// How long a group name may be. Names render as list headers; longer than this they stop
/// reading as names and start reading as sentences.
const MAX_NAME: usize = 64;

/// Case-insensitive, so "Docs" and "docs" cannot both exist and read as two groups.
fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The group a whole-list replace DEMOTES: it survives, but no longer first. That is the one
/// case the sink slot must not carry its default members along - they render in the first
/// group only because nothing ever pinned them to a name, so the slot moving on means they
/// silently move with it. None when the first group keeps the front (nothing moves) or is
/// dropped by omission (the delete contract sinks its members into the new front, which is
/// what the delete-confirm promises). Shared by both member shapes: the sparse map the
/// managed/jobs scopes hold, and the row field tunnels denormalize.
pub fn demoted_first(before: &[String], after: &[String]) -> Option<String> {
    let first = before.first()?;
    let still_there = after.iter().any(|n| same_name(n, first));
    let still_first = after.first().is_some_and(|n| same_name(n, first));
    (still_there && !still_first).then(|| first.clone())
}

/// The one grouping model (SPEC §host.groups): ordered names, sparse member assignments.
///
/// Embedders hold this behind their own lock and give it their persistence; it never touches
/// a file itself, which is what keeps every rule here testable in one place.
#[derive(Debug, Clone, PartialEq)]
pub struct Groups {
    /// Display order. Never empty: the FIRST entry is the sink for unassigned members and for
    /// the members of a deleted group - the slot, never the name `default`, carries that role.
    names: Vec<String>,
    /// Sparse: an absent id renders in the first group. Stores the canonical casing of the name.
    members: BTreeMap<String, String>,
}

impl Groups {
    /// The list every scope starts from: one ordinary `default` group, nothing assigned.
    pub fn new() -> Self {
        Groups {
            names: vec![DEFAULT_GROUP.to_string()],
            members: BTreeMap::new(),
        }
    }

    /// Build from loaded state. Trusted only as far as the invariants reach: names that fail
    /// validation are dropped rather than refused, because a load must never fail the boot over
    /// a hand-edited file - the same stance the managed.json loaders take.
    pub fn from_parts(names: Vec<String>, members: BTreeMap<String, String>) -> Self {
        let mut g = Groups::new();
        let mut clean = Vec::new();
        for raw in names {
            let name = raw.trim();
            if name.is_empty() || name.len() > MAX_NAME {
                continue;
            }
            if clean.iter().any(|n: &String| same_name(n, name)) {
                continue;
            }
            clean.push(name.to_string());
        }
        if !clean.is_empty() {
            g.names = clean;
        }
        let live = g.names.clone();
        g.members = members
            .into_iter()
            .filter(|(_, v)| live.iter().any(|n| n == v))
            .collect();
        g
    }

    pub fn names(&self) -> Vec<String> {
        self.names.clone()
    }

    /// The explicit assignments, canonical casings, keyed by member id.
    pub fn members(&self) -> &BTreeMap<String, String> {
        &self.members
    }

    /// The canonical spelling of a name in the list, matched case-insensitively.
    pub fn canonical(&self, name: &str) -> Option<String> {
        self.names.iter().find(|n| same_name(n, name)).cloned()
    }

    /// The group a member renders under: its stored group when that group still exists, else
    /// the FIRST group - that slot is the sink for unassigned members, whatever it is called.
    pub fn group_of(&self, id: &str) -> String {
        self.members
            .get(id)
            .filter(|g| self.names.iter().any(|n| n == *g))
            .cloned()
            .unwrap_or_else(|| self.names[0].clone())
    }

    /// Validate one name the way every mutation does, so the error text is identical from
    /// `set_names` and `rename`.
    fn checked(raw: &str) -> Result<String, String> {
        let name = raw.trim();
        if name.is_empty() {
            return Err("a group name cannot be empty".into());
        }
        if name.len() > MAX_NAME {
            return Err(format!("a group name is at most {MAX_NAME} characters"));
        }
        Ok(name.to_string())
    }

    /// Replace the whole list: create, delete and reorder are all "here is the new list".
    /// A group dropped by omission is deleted and its members lose their explicit entry, so
    /// they render in the new first group. Atomic: a rejected list leaves `self` untouched.
    ///
    /// The gap: a member with no explicit entry renders in the first group by default, so a
    /// replace that demotes the first group carries it to the new front. Scopes that can
    /// name their members close it with [Groups::set_names_pinning] - a reorder must not
    /// re-home anyone.
    pub fn set_names(&mut self, next: Vec<String>) -> Result<Vec<String>, String> {
        self.set_names_pinning(next, std::iter::empty())
    }

    /// [Groups::set_names] with the reorder gap closed: when the replace demotes the first
    /// group without deleting it, every id that renders there only by DEFAULT (no explicit
    /// entry) is pinned to the name first, so the slot can move to the front without
    /// carrying them along. Ids with an explicit entry are untouched; a first group that
    /// stays first pins nothing (nothing would move), and a first group dropped by omission
    /// sinks its default members to the new front - that is the delete contract, not a
    /// reorder.
    pub fn set_names_pinning(
        &mut self,
        next: Vec<String>,
        ids: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, String> {
        let clean = Self::validated(next)?;
        if let Some(name) = demoted_first(&self.names, &clean) {
            // Pin under the NEW list's spelling: a replace may respell the group while
            // demoting it, and the retain below (and every loader's member filter) matches
            // spellings exactly - a pin in the old spelling would be orphaned by the very
            // call that inserted it.
            let canonical = clean
                .iter()
                .find(|n| same_name(n, &name))
                .cloned()
                .unwrap_or(name);
            for id in ids {
                self.members.entry(id).or_insert_with(|| canonical.clone());
            }
        }
        self.names = clean.clone();
        self.members
            .retain(|_, g| self.names.iter().any(|n| n == g));
        Ok(clean)
    }

    /// Validate a whole-list replace the way every mutation reports it, so a rejected list
    /// pins nothing and leaves `self` untouched.
    fn validated(next: Vec<String>) -> Result<Vec<String>, String> {
        let mut clean = Vec::new();
        for raw in next {
            let name = Self::checked(&raw)?;
            if clean.iter().any(|n: &String| same_name(n, &name)) {
                return Err(format!("duplicate group name: {name}"));
            }
            clean.push(name);
        }
        if clean.is_empty() {
            return Err("at least one group must remain".into());
        }
        Ok(clean)
    }

    /// Rename in place, keeping the slot and carrying the members with it. Members with no
    /// explicit group need no rewrite: they render in the first group, and a rename keeps the
    /// slot, so they follow the new name on their own. Returns the new names and how many
    /// explicit members moved.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        let to = Self::checked(to)?;
        let i = self
            .names
            .iter()
            .position(|n| same_name(n, from))
            .ok_or_else(|| format!("unknown group: {from}"))?;
        if self
            .names
            .iter()
            .enumerate()
            .any(|(j, n)| j != i && same_name(n, &to))
        {
            return Err(format!("duplicate group name: {to}"));
        }
        let old = self.names[i].clone();
        self.names[i] = to;
        let mut moved = 0;
        for g in self.members.values_mut() {
            if *g == old {
                *g = self.names[i].clone();
                moved += 1;
            }
        }
        Ok((self.names.clone(), moved))
    }

    /// Put a member in a group. `None` means "no explicit group" - it renders in the first
    /// group, whatever that is called, and follows it if the first group is renamed. `Some`
    /// must name an existing group (case-insensitively); the canonical casing is stored, not
    /// what the caller typed. Returns the group the member now renders under.
    pub fn assign(&mut self, id: &str, group: Option<&str>) -> Result<String, String> {
        match group {
            None => {
                self.members.remove(id);
            }
            Some(g) => {
                let canonical = self
                    .canonical(g)
                    .ok_or_else(|| format!("unknown group: {g}"))?;
                self.members.insert(id.to_string(), canonical);
            }
        }
        Ok(self.group_of(id))
    }

    /// Move a member's explicit entry across an id rename, so renaming an item neither
    /// reshuffles the list nor drops it back into the first group.
    pub fn rename_member(&mut self, old_id: &str, new_id: &str) {
        if let Some(g) = self.members.remove(old_id) {
            self.members.insert(new_id.to_string(), g);
        }
    }

    /// Drop a member's explicit entry (the member itself was deleted; it holds no slot).
    pub fn forget_member(&mut self, id: &str) {
        self.members.remove(id);
    }
}

impl Default for Groups {
    fn default() -> Self {
        Self::new()
    }
}

/// What the route family needs from a scope (SPEC §host.groups). The host holds only this
/// mechanism - each scope's members are its own business, which is why `has_member` is here
/// instead of a host-side registry lookup: only the scope knows what a valid member id is.
pub trait GroupScope: Send + Sync {
    fn names(&self) -> Vec<String>;
    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String>;
    /// Returns the new names and how many explicit members the rename carried.
    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String>;
    /// Returns the canonical group name the member now renders under.
    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String>;
    /// The scope's own flat order; the route family validates shape, the scope owns semantics.
    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String>;
    /// 404 for an unknown MCP/job/token, not a silent write.
    fn has_member(&self, id: &str) -> bool;
}

/// The scope table the route family dispatches through (`mcps`, `conns`, `rules`, `jobs`,
/// `secrets`, `tokens`). The host carries no match arm: a scope joins by registering, the
/// same way a plugin joins the host - SPEC §host.plugins's "mechanism in the host, business in the
/// plugin", applied to groups.
#[derive(Default)]
pub struct GroupScopes(
    std::sync::RwLock<std::collections::HashMap<&'static str, std::sync::Arc<dyn GroupScope>>>,
);

impl GroupScopes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, scope: &'static str, impl_: std::sync::Arc<dyn GroupScope>) {
        if let Ok(mut map) = self.0.write() {
            map.insert(scope, impl_);
        }
    }

    pub fn get(&self, scope: &str) -> Option<std::sync::Arc<dyn GroupScope>> {
        self.0.read().ok().and_then(|map| map.get(scope).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(names: &[&str]) -> Groups {
        Groups::from_parts(
            names.iter().map(|s| s.to_string()).collect(),
            BTreeMap::new(),
        )
    }

    /// Invariant 1: the list is never empty - something must catch unassigned members.
    #[test]
    fn a_group_list_is_never_empty() {
        let mut g = groups(&["default", "Docs"]);
        assert_eq!(
            g.set_names(vec![]).unwrap_err(),
            "at least one group must remain"
        );
        assert_eq!(
            g.names(),
            ["default", "Docs"],
            "a rejected list leaves state untouched"
        );
        // A fresh model starts from one ordinary group, not from nothing.
        assert_eq!(Groups::new().names(), ["default"]);
    }

    /// Invariant 2: names are trimmed, bounded and case-insensitively unique.
    #[test]
    fn names_are_trimmed_bounded_and_unique_ignoring_case() {
        let mut g = groups(&["default"]);
        assert!(g.set_names(vec!["  Docs  ".into()]).is_ok());
        assert_eq!(g.names(), ["Docs"]);
        assert_eq!(
            g.set_names(vec!["a".into(), "A".into()]).unwrap_err(),
            "duplicate group name: A"
        );
        assert_eq!(
            g.set_names(vec![" ".into()]).unwrap_err(),
            "a group name cannot be empty"
        );
        assert_eq!(
            g.set_names(vec!["x".repeat(65)]).unwrap_err(),
            "a group name is at most 64 characters"
        );
        assert_eq!(g.names(), ["Docs"], "and none of that half-applied");
    }

    /// Invariant 3: a member renders under its stored group while that group exists, else in
    /// the first group - the slot is the sink, not the name.
    #[test]
    fn group_of_falls_back_to_the_first_group_not_the_default_name() {
        let mut g = groups(&["default", "Docs"]);
        g.assign("a", Some("docs")).expect("case-insensitive match");
        assert_eq!(g.group_of("a"), "Docs"); // canonical casing comes back
        assert_eq!(g.group_of("b"), "default"); // unassigned
                                                // Delete Docs: "a" is unassigned again and lands in the first group.
        g.set_names(vec!["Work".into()]).expect("replaces the list");
        assert_eq!(g.group_of("a"), "Work");
    }

    /// Invariant 4: set_names is a whole-list replace; omitted groups are deleted and their
    /// members fall into the first remaining group; `default` is omissible like any name.
    #[test]
    fn set_names_deletes_by_omission_and_default_is_omittable() {
        let mut g = groups(&["default", "Docs", "Search"]);
        g.assign("a", Some("Docs")).unwrap();
        g.assign("b", Some("Search")).unwrap();
        g.set_names(vec!["Search".into()]).unwrap();
        assert_eq!(g.names(), ["Search"]);
        assert_eq!(g.group_of("a"), "Search"); // fell out of the deleted Docs
        assert_eq!(g.group_of("b"), "Search");
    }

    /// Invariant 5: rename keeps the slot and carries the members; a collision is refused.
    #[test]
    fn rename_keeps_the_slot_and_carries_members() {
        let mut g = groups(&["default", "Docs", "Search"]);
        g.assign("a", Some("Docs")).unwrap();
        let (names, moved) = g.rename("docs", "Reference").unwrap();
        assert_eq!(names, ["default", "Reference", "Search"]);
        assert_eq!(moved, 1);
        assert_eq!(g.group_of("a"), "Reference");
        assert_eq!(
            g.rename("Reference", "search").unwrap_err(),
            "duplicate group name: search"
        );
        assert!(g.rename("Ghost", "X").is_err());
        // An unassigned member follows the renamed FIRST slot on its own.
        let _ = g.rename("default", "Home").unwrap();
        assert_eq!(g.group_of("zzz"), "Home");
    }

    /// Invariant 6: assign stores the canonical casing and demands an existing group;
    /// None removes the explicit entry.
    #[test]
    fn assign_stores_canonical_casing_and_rejects_unknown_groups() {
        let mut g = groups(&["default", "Docs"]);
        assert_eq!(g.assign("a", Some("DOCS")).unwrap(), "Docs");
        assert_eq!(
            g.assign("a", Some("Nope")).unwrap_err(),
            "unknown group: Nope"
        );
        assert_eq!(g.assign("a", None).unwrap(), "default");
        assert!(g.members().is_empty());
    }

    /// Invariant 7 (by construction, pinned here so it stays true): flat order never passes
    /// through Groups - members keep their entry verbatim when the LIST is reordered, so the
    /// embedding scope's own order field stays the only order.
    #[test]
    fn reordering_the_names_never_touches_membership() {
        let mut g = groups(&["default", "Docs"]);
        g.assign("a", Some("Docs")).unwrap();
        g.set_names(vec!["Docs".into(), "default".into()]).unwrap();
        assert_eq!(g.group_of("a"), "Docs");
        assert_eq!(g.group_of("b"), "Docs"); // the sink is the slot, now Docs
    }

    /// Invariant 9: a reorder must not re-home anyone - not even the members that render in
    /// the first group by DEFAULT. set_names_pinning pins those to the name when the replace
    /// demotes it; dropping the first group is still a delete, and its members still sink.
    #[test]
    fn a_demoted_first_group_leaves_its_default_members_behind() {
        let mut g = groups(&["g1", "g2"]);
        g.assign("b", Some("g2")).unwrap();
        // "a" and "c" have no entry: they render in g1 because g1 is first - the exact shape
        // the panel's Move up broke (three under g1, g2 empty, one Move up and all three
        // answered g2).
        g.set_names_pinning(
            vec!["g2".into(), "g1".into()],
            ["a", "b", "c"].map(String::from),
        )
        .unwrap();
        assert_eq!(g.names(), ["g2", "g1"]);
        assert_eq!(g.group_of("a"), "g1", "default members stay with the name");
        assert_eq!(g.group_of("c"), "g1");
        assert_eq!(g.group_of("b"), "g2", "explicit members never move");
        // Dropping the first group is a delete, not a reorder: its members sink.
        g.set_names_pinning(vec!["g2".into()], ["a", "b", "c"].map(String::from))
            .unwrap();
        assert_eq!(g.group_of("a"), "g2");
        // A rejected list pins nothing and leaves state untouched.
        let mut h = groups(&["g1", "g2"]);
        assert!(h
            .set_names_pinning(vec!["g2".into(), "g2".into()], ["a"].map(String::from))
            .is_err());
        assert_eq!(h.names(), ["g1", "g2"]);
        assert!(h.members().is_empty(), "a rejected list pinned nothing");
        // A replace that RESPELLS the first group while demoting it pins under the new
        // spelling - the same call's retain (and every loader's member filter) matches
        // spellings exactly, so an old-spelling pin would be orphaned on insert.
        let mut r = groups(&["g1", "g2"]);
        r.set_names_pinning(vec!["g2".into(), "G1".into()], ["x"].map(String::from))
            .unwrap();
        assert_eq!(r.names(), ["g2", "G1"]);
        assert_eq!(r.group_of("x"), "G1", "the pin rides the respell");
        assert_eq!(r.members().get("x").map(String::as_str), Some("G1"));
    }

    /// Invariant 8: the parts serialize under whatever keys the embedding store chooses; what
    /// Groups guarantees is that the parts round-trip - the names verbatim, the explicit
    /// members sparse, so "in the first group" never needs storing.
    #[test]
    fn parts_round_trip_through_from_parts() {
        let mut g = groups(&["default", "Docs"]);
        g.assign("a", Some("Docs")).unwrap();
        let again = Groups::from_parts(g.names(), g.members().clone());
        assert_eq!(again, g);
        // A malformed loaded list degrades to the start list rather than panicking or
        // refusing the boot: empty after cleaning means "nothing usable was stored".
        let degraded = Groups::from_parts(vec![" ".into(), "".into()], BTreeMap::new());
        assert_eq!(degraded.names(), ["default"]);
        // and a member pointing at a group the file no longer lists is dropped on load.
        let stale = Groups::from_parts(
            vec!["default".into()],
            BTreeMap::from([("a".into(), "Gone".into())]),
        );
        assert_eq!(stale.group_of("a"), "default");
    }

    #[test]
    fn rename_and_forget_member_keep_assignments_pointed_at_the_live_id() {
        let mut g = groups(&["default", "Docs"]);
        g.assign("old", Some("Docs")).unwrap();
        g.rename_member("old", "new");
        assert_eq!(g.group_of("new"), "Docs");
        assert_eq!(g.group_of("old"), "default");
        g.forget_member("new");
        assert!(g.members().is_empty());
    }

    #[test]
    fn the_scope_table_answers_by_name_and_unknown_scopes_are_absent() {
        struct Fixed;
        impl GroupScope for Fixed {
            fn names(&self) -> Vec<String> {
                vec!["default".into()]
            }
            fn set_names(&self, _next: Vec<String>) -> Result<Vec<String>, String> {
                Ok(self.names())
            }
            fn rename(&self, _from: &str, _to: &str) -> Result<(Vec<String>, usize), String> {
                Ok((self.names(), 0))
            }
            fn assign(&self, _id: &str, _group: Option<&str>) -> Result<String, String> {
                Ok("default".into())
            }
            fn set_order(&self, _ids: Vec<String>) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
            fn has_member(&self, _id: &str) -> bool {
                true
            }
        }
        let scopes = GroupScopes::new();
        assert!(scopes.get("mcps").is_none());
        scopes.register("mcps", std::sync::Arc::new(Fixed));
        assert!(scopes.get("mcps").is_some());
        assert!(scopes.get("nope").is_none());
    }
}
