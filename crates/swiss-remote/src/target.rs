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

//! The remote TARGET table (docs/32): what an agent may aim at.
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
        let target = RemoteTarget {
            id,
            label,
            endpoint: text("endpoint")?,
            workspace_root: text("workspaceRoot")?,
            shell: text("shell")?,
            capabilities,
            default_timeout_ms,
        };
        validate(&target)?;
        Ok(target)
    }
}

/// The sealed target table. `open` reads (a missing file is an empty table, not an
/// error - a fresh install has no targets), every mutation rewrites the file.
pub struct TargetStore {
    path: PathBuf,
    targets: Vec<RemoteTarget>,
}

impl TargetStore {
    pub fn open(path: PathBuf) -> Self {
        let targets = match read_secure_json(&path) {
            Ok(Some(Value::Array(rows))) => rows
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
                .collect(),
            _ => Vec::new(),
        };
        TargetStore { path, targets }
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

    pub fn add(&mut self, target: RemoteTarget) -> Result<(), String> {
        validate(&target)?;
        if self.get(&target.id).is_some() {
            return Err(format!("target {} already exists", target.id));
        }
        self.targets.push(target);
        self.save()
    }

    /// Replace a row wholesale; the id itself cannot change (it IS the key).
    pub fn update(&mut self, id: &str, target: RemoteTarget) -> Result<(), String> {
        if target.id != id {
            return Err(format!(
                "cannot rename target {id} to {} - remove and re-add instead",
                target.id
            ));
        }
        validate(&target)?;
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

    fn save(&self) -> Result<(), String> {
        let rows: Vec<Value> = self.targets.iter().map(RemoteTarget::to_json).collect();
        write_secure_json(&self.path, &Value::Array(rows))
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
    let mut path = root.trim_end_matches('/').to_string();
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
}
