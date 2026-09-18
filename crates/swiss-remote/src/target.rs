/*
 * Copyright 2026 The swiss authors
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

//! The remote TARGET table (docs/34): what an agent may aim at.
//!
//! A target is the agent-facing half of a connection: an endpoint id (a tunnels
//! connection, by that connection's own id), a workspaceRoot on the far side, a shell
//! family, and capability flags. What it deliberately is NOT: credentials. No host, no
//! user, no password, no key - those live in tunnels.json and never cross this seam, so
//! a target row can be printed, committed, or read aloud without leaking anything.
//!
//! The store is one sealed remote.json like every other state file, loaded at open and
//! written through on every mutation; the file is small (a handful of machines) and the
//! writes are rare (a human setting up).

use std::path::PathBuf;

use serde_json::{json, Value};

use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

/// The capabilities a target may declare.
pub const KNOWN_CAPABILITIES: [&str; 3] = ["exec", "files", "sync"];

/// Field names a target row must never carry: the credential denylist. Rejected
/// loudly rather than silently ignored - a row that carries one of these was written
/// by something that did not understand the contract, and pretending otherwise would
/// store a secret in a file whose whole design is being secret-free.
const FORBIDDEN_FIELDS: [&str; 10] = [
    "host",
    "port",
    "username",
    "user",
    "password",
    "passphrase",
    "key",
    "keyPath",
    "privateKey",
    "identityFile",
];

/// One agent-facing target, exactly as persisted (camelCase on the wire).
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTarget {
    /// A slug the agent types: lowercase letters, digits and inner dashes.
    pub id: String,
    /// Human label; defaults to the id when absent.
    pub label: String,
    /// The endpoint id - for the tunnels transport, a tunnels connection id.
    pub endpoint: String,
    /// Absolute POSIX path of the project root on the far machine. The exec cwd and
    /// every sync/pull path resolve under it; it is a guardrail (paths are checked
    /// against it) not a sandbox (the command itself can cd anywhere).
    pub workspace_root: String,
    /// Shell family of the far side. posix only for now - the quoting layer is POSIX
    /// single-quote based and will not silently mis-quote a cmd.exe.
    pub shell: String,
    /// Subset of exec/files/sync. exec is always required (a target you cannot run
    /// anything on is not a target).
    pub capabilities: Vec<String>,
    /// Per-target default deadline for runs, when the caller gives none.
    pub default_timeout_ms: Option<u64>,
    /// The docs/20 rendering group this target lists under (R8). None means "the
    /// first group, whatever it is called" - the sink rule every scope shares; the
    /// canonical spelling is stored, matched case-insensitively like every scope.
    pub group: Option<String>,
}

/// A slug for target ids: lowercase, digits, inner dashes.
fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.ends_with('-')
        && !id.contains("--")
}

/// An absolute POSIX path with no dot segments: starts with '/', non-trivial, and
/// neither '.' nor '..' appears as a segment.
fn valid_abs_posix(path: &str) -> bool {
    if !path.starts_with('/') || path.len() < 2 || path.contains('\\') || path.contains('\0') {
        return false;
    }
    // skip(1) drops the empty segment before the leading '/'; a trailing slash
    // still produces a trailing empty segment and is refused, on purpose.
    path.split('/')
        .skip(1)
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}
/// Validate one target. Called at every entry point (store load, API write, action
/// resolve) so nothing downstream has to defend against a half-formed row.
pub fn validate(target: &RemoteTarget) -> Result<(), String> {
    if !valid_id(&target.id) {
        return Err(format!(
            "target id {:?} is not usable: 1-64 chars of a-z 0-9 -, starting with a letter or digit",
            target.id
        ));
    }
    if target.label.trim().is_empty() {
        return Err("target label must not be empty".into());
    }
    if target.endpoint.trim().is_empty() {
        return Err(format!("target {} needs an endpoint id", target.id));
    }
    if !valid_abs_posix(&target.workspace_root) {
        return Err(format!(
            "target {} workspaceRoot {:?} must be an absolute POSIX path without . or .. segments",
            target.id, target.workspace_root
        ));
    }
    if target.shell != "posix" {
        return Err(format!(
            "target {} shell must be 'posix' (got {:?}); other shells are not quoted safely yet",
            target.id, target.shell
        ));
    }
    if target.capabilities.is_empty() || !target.capabilities.contains(&"exec".to_string()) {
        return Err(format!(
            "target {} must declare at least the 'exec' capability",
            target.id
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for cap in &target.capabilities {
        if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
            return Err(format!(
                "target {} capability {:?} is not one of exec/files/sync",
                target.id, cap
            ));
        }
        if seen.contains(&cap.as_str()) {
            return Err(format!(
                "target {} lists capability {:?} twice",
                target.id, cap
            ));
        }
        seen.push(cap);
    }
    if let Some(ms) = target.default_timeout_ms {
        if ms == 0 {
            return Err(format!(
                "target {} defaultTimeoutMs must be positive",
                target.id
            ));
        }
    }
    Ok(())
}

impl RemoteTarget {
    /// The wire row (camelCase, like every state file).
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "endpoint": self.endpoint,
            "workspaceRoot": self.workspace_root,
            "shell": self.shell,
            "capabilities": self.capabilities,
            "defaultTimeoutMs": self.default_timeout_ms,
            "group": self.group,
        })
    }

    /// Parse one row STRICTLY: unknown fields are refused naming the path (the
    /// convention of every strict parser in this codebase), so a typo'd key fails at
    /// the door instead of silently dropping half a target.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "target: expected an object".to_string())?;
        for key in obj.keys() {
            if FORBIDDEN_FIELDS.contains(&key.as_str()) {
                return Err(format!(
                    "target.{} must not carry credentials; connections (and their secrets) are managed on the Tunnels page",
                    key
                ));
            }
        }
        let known = [
            "id",
            "label",
            "endpoint",
            "workspaceRoot",
            "shell",
            "capabilities",
            "defaultTimeoutMs",
            "group",
        ];
        for key in obj.keys() {
            if !known.contains(&key.as_str()) {
                return Err(format!("target.{} is not a known field", key));
            }
        }
        let text = |key: &str| -> Result<String, String> {
            obj.get(key)
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| format!("target.{} must be a non-empty string", key))
        };
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "target.id is required".to_string())?;
        let label = match obj.get("label") {
            None => id.clone(),
            Some(v) => v
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "target.label must be a string".to_string())?,
        };
        let capabilities = match obj.get("capabilities") {
            None => vec!["exec".to_string()],
            Some(Value::Array(items)) => {
                let mut caps = Vec::new();
                for item in items {
                    caps.push(
                        item.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "target.capabilities must be strings".to_string())?,
                    );
                }
                caps
            }
            Some(_) => return Err("target.capabilities must be an array".into()),
        };
        let default_timeout_ms = match obj.get("defaultTimeoutMs") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => {
                let ms = n.as_u64().ok_or_else(|| {
                    "target.defaultTimeoutMs must be a positive integer".to_string()
                })?;
                if ms == 0 {
                    return Err("target.defaultTimeoutMs must be a positive integer".into());
                }
                Some(ms)
            }
            Some(_) => return Err("target.defaultTimeoutMs must be a positive integer".into()),
        };
        let group = match obj.get("group") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Some(_) => return Err("target.group must be a group name or null".into()),
        };
        let target = RemoteTarget {
            id,
            label,
            endpoint: text("endpoint")?,
            workspace_root: text("workspaceRoot")?,
            shell: text("shell")?,
            capabilities,
            default_timeout_ms,
            group,
        };
        validate(&target)?;
        Ok(target)
    }
}

/// Store a row's explicit group under its canonical spelling, refusing a name the
/// group list does not carry - the same word-for-word rule the family's member PUT
/// enforces, applied at the row door (Add/Edit) so a typo fails at save, not at render.
fn canonicalize_group(
    target: &mut RemoteTarget,
    groups: &swiss_host::groups::Groups,
) -> Result<(), String> {
    if let Some(name) = target.group.clone() {
        let canonical = groups
            .canonical(&name)
            .ok_or_else(|| format!("unknown group: {name}"))?
            .to_string();
        target.group = Some(canonical);
    }
    Ok(())
}

/// The sealed target table. `open` reads (a missing file is an empty table, not an
/// error - a fresh install has no targets), every mutation rewrites the file.
pub struct TargetStore {
    path: PathBuf,
    targets: Vec<RemoteTarget>,
    /// The ordered group names (docs/34 R8). Membership lives on the rows themselves -
    /// the tunnels.json shape - so this model holds names only and the members map
    /// stays empty by construction.
    groups: swiss_host::groups::Groups,
}

impl TargetStore {
    pub fn open(path: PathBuf) -> Self {
        // Two shapes on disk: the object R8 writes ({ groups, targets }) and the bare
        // row array every earlier build sealed. The array reads as "no groups", which
        // renders exactly as the pre-groups page did - one undivided list.
        let raw = read_secure_json(&path).ok().flatten();
        let (names, rows) = match &raw {
            Some(Value::Array(rows)) => (Vec::new(), rows.clone()),
            Some(Value::Object(obj)) => (
                obj.get("groups")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                obj.get("targets")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            ),
            _ => (Vec::new(), Vec::new()),
        };
        let targets = rows
            .iter()
            .filter_map(|row| match RemoteTarget::from_json(row) {
                Ok(t) => Some(t),
                Err(err) => {
                    // One bad row must not take the table down (the same rule as
                    // every load path here): drop it, say so, keep the rest.
                    swiss_core::log::warn(
                        "dropping an invalid remote target",
                        Some(json!({ "err": err })),
                    );
                    None
                }
            })
            .collect();
        let names: Vec<String> = names
            .iter()
            .filter_map(|n| n.as_str().map(str::to_string))
            .collect();
        TargetStore {
            path,
            targets,
            groups: swiss_host::groups::Groups::from_parts(names, std::collections::BTreeMap::new()),
        }
    }

    pub fn list(&self) -> &[RemoteTarget] {
        &self.targets
    }

    pub fn get(&self, id: &str) -> Option<&RemoteTarget> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn endpoint_used(&self, endpoint: &str) -> bool {
        self.targets.iter().any(|t| t.endpoint == endpoint)
    }

    pub fn add(&mut self, mut target: RemoteTarget) -> Result<(), String> {
        validate(&target)?;
        if self.get(&target.id).is_some() {
            return Err(format!("target {} already exists", target.id));
        }
        canonicalize_group(&mut target, &self.groups)?;
        self.targets.push(target);
        self.save()
    }

    /// Replace a row wholesale; the id itself cannot change (it IS the key).
    pub fn update(&mut self, id: &str, mut target: RemoteTarget) -> Result<(), String> {
        if target.id != id {
            return Err(format!(
                "cannot rename target {id} to {} - remove and re-add instead",
                target.id
            ));
        }
        validate(&target)?;
        canonicalize_group(&mut target, &self.groups)?;
        let slot = self
            .targets
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("target {id} does not exist"))?;
        *slot = target;
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let before = self.targets.len();
        self.targets.retain(|t| t.id != id);
        if self.targets.len() == before {
            return Err(format!("target {id} does not exist"));
        }
        self.save()
    }

    // --- groups (docs/34 R8: the targets scope of the docs/20 family) ---------------------------

    /// The ordered group names, exactly as the family's PUT answers them.
    pub fn group_names(&self) -> Vec<String> {
        self.groups.names()
    }

    /// The rendering group of one target: its explicit row group while that name lives,
    /// else the first group - the sink rule every scope shares.
    pub fn group_of(&self, id: &str) -> String {
        let explicit = self
            .targets
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.group.as_deref())
            .and_then(|g| self.groups.canonical(g));
        explicit.unwrap_or_else(|| {
            self.group_names()
                .first()
                .cloned()
                .unwrap_or_else(|| swiss_host::groups::DEFAULT_GROUP.to_string())
        })
    }

    /// Replace the whole group-name list: create, reorder and delete are all "here is
    /// the new list". Row-side bookkeeping mirrors tunnels.json exactly (docs/20 §2.2):
    /// a demoted first group pins its default members to the name so nobody silently
    /// re-homes, and a group dropped by omission loses its rows' explicit entries.
    pub fn set_group_names(&mut self, next: &[String]) -> Result<Vec<String>, String> {
        let before = self.groups.names();
        let clean = self.groups.set_names(next.to_vec())?;
        if let Some(first) = swiss_host::groups::demoted_first(&before, &clean) {
            // The NEW list's spelling, not the old one: a replace may respell the
            // group while demoting it, and the panel buckets rows by exact string.
            let pinned = clean
                .iter()
                .find(|n| n.eq_ignore_ascii_case(&first))
                .cloned()
                .unwrap_or(first);
            for row in &mut self.targets {
                if row.group.is_none() {
                    row.group = Some(pinned.clone());
                }
            }
        }
        for row in &mut self.targets {
            if let Some(g) = &row.group {
                if !clean.iter().any(|n| n.eq_ignore_ascii_case(g)) {
                    row.group = None;
                }
            }
        }
        self.save()?;
        Ok(self.groups.names())
    }

    /// Rename in place: the slot is kept and the rows' explicit entries ride along
    /// (the count is what a rename toast and a delete-confirm report).
    pub fn rename_group(&mut self, from: &str, to: &str) -> Result<(Vec<String>, usize), String> {
        let (names, _) = self.groups.rename(from, to)?;
        let old_name = from.trim();
        let new_name = to.trim();
        let mut moved = 0usize;
        for row in &mut self.targets {
            if row
                .group
                .as_deref()
                .is_some_and(|g| g.eq_ignore_ascii_case(old_name))
            {
                row.group = Some(new_name.to_string());
                moved += 1;
            }
        }
        self.save()?;
        Ok((names, moved))
    }

    /// Put one target in a group. None means "no explicit group" - it renders in the
    /// first group, whatever that is called. The canonical casing is stored, and
    /// answered (the shape every scope's member PUT returns).
    pub fn set_target_group(&mut self, id: &str, group: Option<&str>) -> Result<String, String> {
        let new_group = match group.map(str::trim) {
            None | Some("") => None,
            Some(name) => Some(
                self.groups
                    .canonical(name)
                    .ok_or_else(|| format!("unknown group: {name}"))?
                    .to_string(),
            ),
        };
        let row = self
            .targets
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown target: {id}"))?;
        row.group = new_group.clone();
        self.save()?;
        Ok(new_group.unwrap_or_else(|| {
            self.group_names()
                .first()
                .cloned()
                .unwrap_or_else(|| swiss_host::groups::DEFAULT_GROUP.to_string())
        }))
    }

    /// Impose a full order on the target list (array order in the file is the order,
    /// exactly the tunnels.json contract). Rows the caller omitted keep their relative
    /// order after the mentioned ones, so a stale panel reorder drops nothing.
    pub fn reorder(&mut self, ids: &[String]) -> Result<(), String> {
        let rank = |id: &str| -> usize {
            ids.iter().position(|x| x == id).unwrap_or(ids.len())
        };
        self.targets.sort_by_key(|t| rank(&t.id));
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let rows: Vec<Value> = self.targets.iter().map(RemoteTarget::to_json).collect();
        let doc = json!({ "groups": self.groups.names(), "targets": rows });
        write_secure_json(&self.path, &doc)
            .map_err(|err| format!("could not write {}: {err}", self.path.display()))
    }
}

/// Join a workspace root and a relative path into the absolute remote path, refusing
/// anything that would escape the root. `rel` may be empty (the root itself), may use
/// '/' separators, and is normalized; a '..' that climbs past the root is refused
/// rather than clamped.
pub fn safe_join(root: &str, rel: &str) -> Result<String, String> {
    if rel.is_empty() {
        return Ok(root.trim_end_matches('/').to_string());
    }
    // An absolute rel is a path the caller typed in full — the same trust an ssh
    // command line gets. It is normalized on its own and never joined onto the
    // root: the root anchors *relative* paths, it does not cage absolute ones.
    let base = if rel.starts_with('/') { "" } else { root };
    let mut parts: Vec<&str> = Vec::new();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("path {rel:?} escapes the workspace root"));
                }
            }
            other => parts.push(other),
        }
    }
    let mut path = base.trim_end_matches('/').to_string();
    for seg in parts {
        path.push('/');
        path.push_str(seg);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    #[test]
    fn safe_join_passes_an_absolute_rel_through_as_the_path_itself() {
        // An absolute path is what the caller typed in full - ssh-level trust, never
        // joined onto the root (docs/34: the root anchors relative paths, not a cage).
        assert_eq!(
            safe_join("/tmp/ws", "/home/dev/app").unwrap(),
            "/home/dev/app"
        );
        assert_eq!(
            safe_join("/tmp/ws", "//var//log/./x").unwrap(),
            "/var/log/x"
        );
        assert!(safe_join("/tmp/ws", "/a/../..").is_err());
    }
    use std::path::Path;

    fn target(id: &str) -> RemoteTarget {
        RemoteTarget {
            id: id.into(),
            label: id.into(),
            endpoint: "conn-1".into(),
            workspace_root: "/data/ws/proj".into(),
            shell: "posix".into(),
            capabilities: vec!["exec".into(), "sync".into()],
            default_timeout_ms: None,
            group: None,
        }
    }

    fn scratch() -> PathBuf {
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-rmt-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn ids_must_be_slugs() {
        for bad in ["", "A", "-a", "a--b", "a-", "a b", "x".repeat(65).as_str()] {
            let mut t = target("ok");
            t.id = bad.into();
            assert!(validate(&t).is_err(), "{bad:?} must not validate");
        }
        for good in ["a", "build-1", "rpi-5-64bit"] {
            let mut t = target(good);
            t.label = good.into();
            validate(&t).expect("a slug validates");
        }
    }

    #[test]
    fn workspace_roots_must_be_absolute_posix_without_dots() {
        for bad in ["data/ws", "C:/ws", "/data/../etc", "/data/./ws", "/"] {
            let mut t = target("ok");
            t.workspace_root = bad.into();
            assert!(validate(&t).is_err(), "{bad:?} must not validate");
        }
        let mut t = target("ok");
        t.workspace_root = "/data/ws/proj".into();
        validate(&t).expect("a clean absolute path validates");
    }

    #[test]
    fn exec_is_the_minimum_capability_and_others_must_be_known() {
        let mut t = target("ok");
        t.capabilities = vec!["files".into()];
        assert!(validate(&t).is_err(), "a target without exec is unusable");
        t.capabilities = vec!["exec".into(), "teleport".into()];
        assert!(validate(&t).is_err(), "an unknown capability is refused");
        t.capabilities = vec!["exec".into(), "exec".into()];
        assert!(validate(&t).is_err(), "a repeated capability is refused");
    }

    #[test]
    fn only_the_posix_shell_is_safe_so_far() {
        let mut t = target("ok");
        t.shell = "cmd".into();
        let err = validate(&t).unwrap_err();
        assert!(err.contains("posix"), "{err}");
    }

    #[test]
    fn credential_fields_are_refused_loudly() {
        let mut row = target("ok").to_json();
        if let Some(obj) = row.as_object_mut() {
            obj.insert("password".into(), Value::String("hunter2".into()));
        }
        let err = RemoteTarget::from_json(&row).unwrap_err();
        assert!(err.contains("target.password"), "{err}");
        assert!(
            err.contains("Tunnels"),
            "the message says where secrets live: {err}"
        );
    }

    #[test]
    fn unknown_fields_are_refused_naming_the_path() {
        let mut row = target("ok").to_json();
        if let Some(obj) = row.as_object_mut() {
            obj.insert("workspceRoot".into(), Value::String("/x".into()));
        }
        let err = RemoteTarget::from_json(&row).unwrap_err();
        assert!(err.contains("target.workspceRoot"), "{err}");
    }

    #[test]
    fn a_row_round_trips_and_defaults_apply() {
        let mut row = Map::new();
        row.insert("id".into(), json!("build"));
        row.insert("endpoint".into(), json!("conn-1"));
        row.insert("workspaceRoot".into(), json!("/data/ws/proj"));
        row.insert("shell".into(), json!("posix"));
        let t = RemoteTarget::from_json(&Value::Object(row)).expect("minimal row parses");
        assert_eq!(t.label, "build", "label defaults to the id");
        assert_eq!(t.capabilities, vec!["exec"], "capabilities default to exec");
        assert_eq!(RemoteTarget::from_json(&t.to_json()).unwrap(), t);
    }

    #[test]
    fn the_store_persists_through_the_sealed_file() {
        let dir = scratch();
        let path = dir.join("remote.json");
        {
            let mut store = TargetStore::open(path.clone());
            assert!(store.list().is_empty(), "a missing file is an empty table");
            store.add(target("build")).expect("add");
            store
                .add(RemoteTarget {
                    id: "flash".into(),
                    label: "flash".into(),
                    endpoint: "conn-2".into(),
                    workspace_root: "/opt/flash".into(),
                    shell: "posix".into(),
                    capabilities: vec!["exec".into(), "files".into(), "sync".into()],
                    default_timeout_ms: Some(3_600_000),
                    group: None,
                })
                .expect("add");
            assert!(
                store.add(target("build")).is_err(),
                "duplicate ids are refused"
            );
        }
        let reopened = TargetStore::open(path.clone());
        assert_eq!(reopened.list().len(), 2);
        assert_eq!(
            reopened.get("flash").and_then(|t| t.default_timeout_ms),
            Some(3_600_000)
        );
        assert!(Path::new(&path).exists(), "the sealed file exists");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn updates_cannot_rename_and_removal_of_unknown_is_named() {
        let dir = scratch();
        let mut store = TargetStore::open(dir.join("remote.json"));
        store.add(target("build")).expect("add");
        let mut renamed = target("other");
        renamed.endpoint = "conn-9".into();
        assert!(store.update("build", renamed).is_err());
        let mut edited = target("build");
        edited.workspace_root = "/srv/new".into();
        store.update("build", edited).expect("update");
        assert_eq!(store.get("build").unwrap().workspace_root, "/srv/new");
        assert!(store.remove("ghost").is_err());
        store.remove("build").expect("remove");
        assert!(store.get("build").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_join_keeps_paths_inside_the_root() {
        assert_eq!(safe_join("/data/ws", "").unwrap(), "/data/ws");
        assert_eq!(
            safe_join("/data/ws/", "src/main.c").unwrap(),
            "/data/ws/src/main.c"
        );
        assert_eq!(
            safe_join("/data/ws", "./a//b/../c").unwrap(),
            "/data/ws/a/c"
        );
        assert!(safe_join("/data/ws", "../etc/passwd").is_err());
        assert!(safe_join("/data/ws", "a/../../..").is_err());
    }

    /// The docs/34 R8 group contract on the one sealed table: names, membership,
    /// pinning and order - the same word-for-word semantics tunnels.json carries
    /// (docs/20 §2.2), asserted through a reopen so persistence is part of the proof.
    #[test]
    fn groups_follow_the_family_contract_and_survive_a_reopen() {
        let dir = scratch();
        let path = dir.join("remote.json");
        let mut store = TargetStore::open(path.clone());
        store.add(target("build")).expect("add");
        store.add(target("flash")).expect("add");

        // Two groups appear; the pre-groups rows had none, so they stay unpinned.
        assert_eq!(
            store.set_group_names(&["default".into(), "prod".into()]).unwrap(),
            ["default".to_string(), "prod".to_string()]
        );
        assert_eq!(store.group_names().len(), 2);

        // Assignment canonicalizes the casing, refuses unknown names and ids.
        assert_eq!(
            store.set_target_group("build", Some("PROD")).unwrap(),
            "prod"
        );
        assert!(store.set_target_group("build", Some("ghost")).is_err());
        assert!(store.set_target_group("ghost", None).is_err());
        assert_eq!(store.group_of("build"), "prod");
        assert_eq!(store.group_of("flash"), "default", "the sink group");

        // A demoted first group pins its default members to the name (docs/20 §2.2).
        store
            .set_group_names(&["prod".into(), "default".into()])
            .expect("demote");
        assert_eq!(
            store.get("flash").unwrap().group.as_deref(),
            Some("default"),
            "the unpinned row was pinned by the demote"
        );
        assert_eq!(store.group_of("flash"), "default", "and still renders there");

        // Rename carries the explicit members and counts them.
        assert_eq!(store.rename_group("prod", "Production").unwrap().1, 1);
        assert_eq!(store.group_of("build"), "Production");

        // Order: the array order the panel saved, omitted rows keep relative order.
        store.reorder(&["flash".into()]).expect("reorder");
        assert_eq!(
            store.list().iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["flash", "build"]
        );

        // A group dropped by omission loses its rows' explicit entries.
        store
            .set_group_names(&["default".into()])
            .expect("collapse");
        assert_eq!(store.get("build").unwrap().group, None);

        let reopened = TargetStore::open(path.clone());
        assert_eq!(reopened.group_names(), vec!["default".to_string()]);
        assert_eq!(reopened.get("build").unwrap().group, None);
        assert_eq!(
            reopened.list().iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["flash", "build"],
            "the reorder survived the reopen"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row written with an unknown group name is refused at the door, not at render.
    #[test]
    fn an_unknown_group_on_a_row_write_is_refused() {
        let dir = scratch();
        let mut store = TargetStore::open(dir.join("remote.json"));
        let mut row = target("build");
        row.group = Some("nowhere".into());
        assert!(
            store.add(row).is_err(),
            "an add carrying a group the list does not carry is a named error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pre-R8 file shape - a bare array of rows - still opens: it reads as "no
    /// groups", which renders as one undivided list, exactly the page it always was.
    #[test]
    fn a_legacy_bare_array_opens_as_an_ungrouped_table() {
        let dir = scratch();
        let path = dir.join("remote.json");
        swiss_core::secure::statefile::write_secure_json(
            &path,
            &json!([{ "id": "legacy", "label": "legacy", "endpoint": "conn-1",
               "workspaceRoot": "/data/ws", "shell": "posix", "capabilities": ["exec"] }]),
        )
        .expect("legacy write");
        let reopened = TargetStore::open(path.clone());
        assert_eq!(reopened.list().len(), 1);
        // The model never carries zero groups: a legacy file answers the single
        // default group, which the page draws as no divider at all.
        assert_eq!(reopened.group_names(), vec!["default".to_string()]);
        assert_eq!(reopened.group_of("legacy"), "default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
