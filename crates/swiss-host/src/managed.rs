//! managed.json — port of `managed.ts`. User-added MCPs, overrides, toggles, enabled flags,
//! named tokens, sidebar order and groups — all in one sealed state file.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::config::ServerDef;
use crate::groups::Groups;
use crate::token::TokenRec;
use swiss_core::log;
use swiss_core::paths::data_path;
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

/// A user-added MCP: its definition + whether it should auto-start.
#[derive(Debug, Clone)]
pub struct ManagedEntry {
    pub name: String,
    pub def: ServerDef,
    pub enabled: bool,
    /// True when this overrides a config-file MCP's def.
    pub override_: bool,
}

/// The group every MCP belongs to until it is put somewhere else. Deliberately NOT stored: an
/// The name a group list starts from. It is an ordinary group the user can rename, delete and
/// reorder; its only privilege is being the initial FIRST entry — that slot (never the name) is
/// where unassigned MCPs and the members of a deleted group land. An MCP in the first group with
/// no explicit assignment has no entry in `mcpGroups`.
pub const DEFAULT_GROUP: &str = "default";

fn managed_path() -> PathBuf {
    data_path(&["managed.json"])
}

fn read_managed_raw(path: &Path) -> Option<Map<String, Value>> {
    match read_secure_json(path) {
        Ok(Some(Value::Object(o))) => Some(o),
        Ok(_) => Some(Map::new()),
        Err(err) => {
            log::warn(
                "managed load failed",
                Some(json!({ "err": err, "path": path.display().to_string() })),
            );
            None
        }
    }
}

/// Read the managed MCP list (missing or invalid entries are dropped).
pub fn load_managed(path: &Path) -> Vec<ManagedEntry> {
    let Some(raw) = read_managed_raw(path) else {
        return Vec::new();
    };
    let arr = match raw.get("mcps") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    arr.iter()
        .filter_map(|e| {
            let obj = e.as_object()?;
            let name = obj.get("name")?.as_str()?.to_string();
            let def = obj.get("def")?.as_object()?;
            def.get("type")?.as_str()?;
            Some(ManagedEntry {
                name,
                def: ServerDef(def.clone()),
                enabled: !matches!(obj.get("enabled"), Some(Value::Bool(false))),
                override_: matches!(obj.get("override"), Some(Value::Bool(true))),
            })
        })
        .collect()
}

fn load_string_map(path: &Path, key: &str) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let Some(raw) = read_managed_raw(path) else {
        return out;
    };
    if let Some(Value::Object(dt)) = raw.get(key) {
        for (k, v) in dt {
            if let Value::Array(list) = v {
                let names: Vec<String> = list
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                out.insert(k.clone(), names);
            }
        }
    }
    out
}

/// Per-MCP run/stop state for CONFIG-file MCPs. A managed MCP carries its own `enabled`; a
/// config MCP has no entry to carry one, and gateway.config.json is the user's committed file.
/// Without this side-map, Stop on a config MCP held only until the next boot.
fn load_bool_map(path: &Path, key: &str) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    let Some(raw) = read_managed_raw(path) else {
        return out;
    };
    if let Some(Value::Object(m)) = raw.get(key) {
        for (k, v) in m {
            if let Value::Bool(b) = v {
                out.insert(k.clone(), *b);
            }
        }
    }
    out
}

/// A rotated bearer token persisted here; absent means "use the env token".
pub fn load_managed_token(path: &Path) -> Option<String> {
    let raw = read_managed_raw(path)?;
    match raw.get("token") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The panel-defined sidebar order; absent means the user never arranged the list.
pub fn load_order(path: &Path) -> Vec<String> {
    let Some(raw) = read_managed_raw(path) else {
        return Vec::new();
    };
    match raw.get("order") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Every sidebar group in display order — `default` included when it exists. The list is never
/// empty: a missing or invalid file starts from `[default]`, and a pre-v2 file (which never stored
/// `default`, because the old model rendered it implicitly) gets it materialized at the front so
/// the first load after the upgrade looks exactly like the last load before it. A v2 file is the
/// list verbatim — an absent `default` there means the user deleted it, and resurrecting it
/// would undo the edit.
pub fn load_groups(path: &Path) -> Vec<String> {
    let start = || vec![DEFAULT_GROUP.to_string()];
    let Some(raw) = read_managed_raw(path) else {
        return start();
    };
    let list = match raw.get("groups") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if raw.get("groupsV2") == Some(&Value::Bool(true)) {
        return if list.is_empty() { start() } else { list };
    }
    if list.iter().any(|n| same_name(n, DEFAULT_GROUP)) {
        list
    } else {
        let mut out = start();
        out.extend(list);
        out
    }
}

/// The tokens' group list (docs/20 G7). A file from before groups names no `tokenGroups` and
/// reads as the single default group; a list someone emptied by hand recovers the same way —
/// something must catch unassigned tokens.
pub fn load_token_groups(path: &Path) -> Vec<String> {
    let list = match read_managed_raw(path).and_then(|raw| {
        raw.get("tokenGroups").cloned()
    }) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if list.is_empty() {
        vec![DEFAULT_GROUP.to_string()]
    } else {
        list
    }
}

/// Which group each token is in — sparse, keyed by token id (docs/20 G7). An absent id
/// renders in the first group; rotate keeps the id, so an assignment survives a rotate.
pub fn load_token_members(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = read_managed_raw(path) else {
        return out;
    };
    if let Some(Value::Object(m)) = raw.get("tokenMembers") {
        for (k, v) in m {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    out
}

/// Which group each MCP is in — sparse: an absent name is in `default`.
pub fn load_mcp_groups(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = read_managed_raw(path) else {
        return out;
    };
    if let Some(Value::Object(m)) = raw.get("mcpGroups") {
        for (k, v) in m {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    out
}

/// The named-token set, persisted so created / rotated / revoked tokens survive a restart.
pub fn load_tokens(path: &Path) -> Vec<TokenRec> {
    let Some(raw) = read_managed_raw(path) else {
        return Vec::new();
    };
    let Some(Value::Array(arr)) = raw.get("tokens") else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let o = t.as_object()?;
            Some(TokenRec {
                id: o.get("id")?.as_str()?.to_string(),
                label: o.get("label")?.as_str()?.to_string(),
                secret: o.get("secret")?.as_str()?.to_string(),
                created_at: o
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// Case-insensitive, so "Docs" and "docs" cannot both exist and read as two groups.
fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Persists user-added MCPs so they survive a gateway restart. One instance, shared behind an
/// Arc; the Mutex is uncontended on this single-threaded runtime.
pub struct ManagedStore {
    path: PathBuf,
    state: Mutex<StoreState>,
}

#[derive(Default)]
struct StoreState {
    entries: Vec<ManagedEntry>,
    tool_toggles: HashMap<String, Vec<String>>,
    resource_toggles: HashMap<String, bool>,
    tokens: Vec<TokenRec>,
    order: Vec<String>,
    /// The one grouping model (docs/20): this store owns only the managed.json key names it
    /// serializes under (`groups` + `mcpGroups`); every rule lives in swiss_host::groups.
    groups: Groups,
    /// The second grouping model (docs/20 G7): the same rules, keyed by token id under
    /// `tokenGroups` + `tokenMembers`.
    token_groups: Groups,
    mcp_enabled: HashMap<String, bool>,
}

impl ManagedStore {
    pub fn open() -> Self {
        Self::open_at(managed_path())
    }

    pub fn open_at(path: PathBuf) -> Self {
        let state = StoreState {
            entries: load_managed(&path),
            tool_toggles: load_string_map(&path, "disabledTools")
                .into_iter()
                .collect(),
            resource_toggles: load_bool_map(&path, "resourceToggles"),
            tokens: load_tokens(&path),
            order: load_order(&path),
            groups: Groups::from_parts(
                load_groups(&path),
                load_mcp_groups(&path).into_iter().collect(),
            ),
            token_groups: Groups::from_parts(
                load_token_groups(&path),
                load_token_members(&path).into_iter().collect(),
            ),
            mcp_enabled: load_bool_map(&path, "mcpEnabled"),
        };
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    pub fn all(&self) -> Vec<ManagedEntry> {
        self.state
            .lock()
            .map(|s| s.entries.clone())
            .unwrap_or_default()
    }

    pub fn has(&self, name: &str) -> bool {
        self.state
            .lock()
            .map(|s| s.entries.iter().any(|e| e.name == name))
            .unwrap_or(false)
    }

    pub fn add(&self, e: ManagedEntry) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if s.entries.iter().any(|x| x.name == e.name) {
            return Err(format!("managed MCP already exists: {}", e.name));
        }
        s.entries.push(e);
        self.persist(&s)
    }

    /// Upsert a config-file MCP override (persisted here so the committed gateway.config.json
    /// keeps its ${ENV} refs).
    pub fn upsert_override(&self, name: &str, def: ServerDef) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if let Some(existing) = s.entries.iter_mut().find(|x| x.name == name) {
            existing.def = def;
            existing.override_ = true;
        } else {
            // The override inherits the run/stop state the user already chose for this name,
            // rather than resetting it — an edit that flipped a stopped MCP back on read as the
            // panel ignoring the Stop.
            let enabled = s.mcp_enabled.remove(name).unwrap_or(true);
            s.entries.push(ManagedEntry {
                name: name.into(),
                def,
                enabled,
                override_: true,
            });
        }
        self.persist(&s)
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.entries.retain(|e| e.name != name);
        s.order.retain(|n| n != name); // a deleted MCP holds no sidebar slot
        s.groups.forget_member(name); // ...nor a group membership
        s.mcp_enabled.remove(name); // ...nor a run/stop state
        self.persist(&s)
    }

    pub fn rename(&self, old_name: &str, new_name: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if let Some(e) = s.entries.iter_mut().find(|x| x.name == old_name) {
            e.name = new_name.to_string();
        }
        // Keep order/group/run-stop pointing at the live name, so a rename neither shuffles the
        // sidebar nor drops the MCP back into default / back to running.
        if s.order.iter().any(|n| n == old_name) {
            let new_name = new_name.to_string();
            s.order = s
                .order
                .iter()
                .map(|n| {
                    if n == old_name {
                        new_name.clone()
                    } else {
                        n.clone()
                    }
                })
                .collect();
        }
        s.groups.rename_member(old_name, new_name);
        if let Some(v) = s.mcp_enabled.remove(old_name) {
            s.mcp_enabled.insert(new_name.into(), v);
        }
        self.persist(&s)
    }

    /// Replace a managed MCP's def (used by config edit).
    pub fn update_def(&self, name: &str, def: ServerDef) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if let Some(e) = s.entries.iter_mut().find(|x| x.name == name) {
            e.def = def;
            return self.persist(&s);
        }
        Ok(())
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if let Some(e) = s.entries.iter_mut().find(|x| x.name == name) {
            e.enabled = enabled;
        } else {
            // A config MCP has no entry — the side-map is its state.
            s.mcp_enabled.insert(name.into(), enabled);
        }
        self.persist(&s)
    }

    /// Run/stop state for any MCP: the managed entry's own flag, else the config-MCP side-map.
    pub fn enabled_for(&self, name: &str) -> Option<bool> {
        self.state.lock().ok().and_then(|s| {
            s.entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.enabled)
                .or_else(|| s.mcp_enabled.get(name).copied())
        })
    }

    /// The tools a user has turned off for this MCP (empty when none / unknown).
    pub fn disabled_tools(&self, name: &str) -> Vec<String> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.tool_toggles.get(name).cloned())
            .unwrap_or_default()
    }

    pub fn set_disabled_tools(&self, name: &str, tools: &[String]) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        if tools.is_empty() {
            s.tool_toggles.remove(name);
        } else {
            s.tool_toggles.insert(name.into(), tools.to_vec());
        }
        self.persist(&s)
    }

    /// Whether this MCP's resources are exposed. None when never toggled (use the default).
    pub fn resource_enabled(&self, name: &str) -> Option<bool> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.resource_toggles.get(name).copied())
    }

    pub fn set_resource_enabled(&self, name: &str, on: bool) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.resource_toggles.insert(name.into(), on);
        self.persist(&s)
    }

    pub fn get_tokens(&self) -> Vec<TokenRec> {
        self.state
            .lock()
            .map(|s| s.tokens.clone())
            .unwrap_or_default()
    }

    /// Replace the whole token set (create / rotate / revoke all persist through here). A
    /// token that left the set forgets its group entry: a revoked id must not keep a ghost
    /// assignment (docs/20 G7) — the group model never names a token that is not there.
    pub fn save_tokens(&self, next: Vec<TokenRec>) {
        if let Ok(mut s) = self.state.lock() {
            for gone in s
                .tokens
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>()
            {
                if !next.iter().any(|t| t.id == gone) {
                    s.token_groups.forget_member(&gone);
                }
            }
            s.tokens = next;
            let _ = self.persist(&s);
        }
    }

    pub fn get_order(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.order.clone())
            .unwrap_or_default()
    }

    /// Persist the sidebar order. Duplicates collapse to their first slot; an empty list gives
    /// up the arrangement and restores the default name order.
    pub fn set_order(&self, names: Vec<String>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        let mut seen = std::collections::HashSet::new();
        s.order = names
            .into_iter()
            .filter(|n| !n.is_empty() && seen.insert(n.clone()))
            .collect();
        self.persist(&s)
    }

    pub fn get_groups(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.groups.names())
            .unwrap_or_default()
    }

    /// The group this MCP is in. Unassigned — or assigned to a group since deleted — lands in
    /// the FIRST group, whatever it is called: that slot is the sink, not the name `default`.
    pub fn group_of(&self, name: &str) -> String {
        self.state
            .lock()
            .ok()
            .map(|s| s.groups.group_of(name))
            .unwrap_or_else(|| DEFAULT_GROUP.into())
    }

    pub fn get_mcp_groups(&self) -> BTreeMap<String, String> {
        self.state
            .lock()
            .map(|s| s.groups.members().clone())
            .unwrap_or_default()
    }

    /// Replace the whole group list: create, delete and reorder in one call — `default` rides in
    /// it like any other name. A group dropped by omission is deleted and its members fall into
    /// the FIRST group of the new list. The list may never be empty: something must catch
    /// unassigned MCPs, and deleting the last group is the one move this refuses.
    pub fn set_groups(&self, names: Vec<String>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.groups.set_names(names)?;
        self.persist(&s)
    }

    /// Put an MCP in a group. `None` means "no explicit group" — it renders in the first group,
    /// whatever that is called, and follows it if the first group is renamed (rename keeps the
    /// slot).
    pub fn set_mcp_group(&self, name: &str, group: Option<&str>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.groups.assign(name, group)?;
        self.persist(&s)
    }

    /// Rename a group in place — any group, `default` included — carrying its members with it and
    /// keeping its slot. Members with no explicit group need no rewrite: they render in the first
    /// group, and a rename keeps the slot, so they follow the new name on their own.
    pub fn rename_group(&self, from: &str, to: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.groups.rename(from, to)?;
        self.persist(&s)
    }

    /// Delete a group. Its MCPs are not touched — they fall into the first remaining group.
    /// Refused when it is the last one: something must catch unassigned MCPs.
    pub fn remove_group(&self, name: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        // An unknown name is a no-op, not an error: deleting something that is already gone
        // must not fail a whole-list save the panel built from a stale read.
        let Some(match_) = s.groups.canonical(name) else {
            return Ok(());
        };
        let next: Vec<String> = s
            .groups
            .names()
            .into_iter()
            .filter(|n| *n != match_)
            .collect();
        s.groups.set_names(next)?;
        self.persist(&s)
    }

    // --- the tokens' groups (docs/20 G7): the same five verbs the MCPs have, over the second
    // model — `token_groups`. Rotation never passes through here (same id, same entry); a
    // revoke drops the member so no ghost entry outlives its token.

    pub fn has_token(&self, id: &str) -> bool {
        self.state
            .lock()
            .map(|s| s.tokens.iter().any(|t| t.id == id))
            .unwrap_or(false)
    }

    pub fn get_token_groups(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.token_groups.names())
            .unwrap_or_default()
    }

    /// The group this token is in — unassigned or orphaned sinks into the first group.
    pub fn token_group_of(&self, id: &str) -> String {
        self.state
            .lock()
            .ok()
            .map(|s| s.token_groups.group_of(id))
            .unwrap_or_else(|| DEFAULT_GROUP.into())
    }

    pub fn get_token_members(&self) -> BTreeMap<String, String> {
        self.state
            .lock()
            .map(|s| s.token_groups.members().clone())
            .unwrap_or_default()
    }

    /// Replace the whole token group list (create / delete / reorder in one call); deleting a
    /// group by omission sinks its tokens into the first remaining one, untouched and usable.
    pub fn set_token_groups(&self, names: Vec<String>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.token_groups.set_names(names)?;
        self.persist(&s)
    }

    pub fn rename_token_group(&self, from: &str, to: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.token_groups.rename(from, to)?;
        self.persist(&s)
    }

    /// Put a token in a group; `None` drops the explicit entry (renders in the first group).
    pub fn set_token_group(&self, id: &str, group: Option<&str>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        s.token_groups.assign(id, group)?;
        self.persist(&s)
    }

    /// Write the file atomically, and tell the caller when it could not be written. Both halves
    /// matter: a torn managed.json makes loadManaged answer empty, silently discarding every
    /// managed MCP and every config override, and a save that only logged let the admin API
    /// report 201 Created for an MCP whose definition never reached the disk.
    fn persist(&self, s: &StoreState) -> Result<(), String> {
        let mut root = Map::new();
        root.insert(
            "mcps".into(),
            Value::Array(
                s.entries
                    .iter()
                    .map(|e| {
                        let mut o = Map::new();
                        o.insert("name".into(), json!(e.name));
                        o.insert("def".into(), Value::Object(e.def.0.clone()));
                        o.insert("enabled".into(), json!(e.enabled));
                        if e.override_ {
                            o.insert("override".into(), json!(true));
                        }
                        Value::Object(o)
                    })
                    .collect(),
            ),
        );
        root.insert(
            "disabledTools".into(),
            Value::Object(
                s.tool_toggles
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
        root.insert(
            "resourceToggles".into(),
            Value::Object(
                s.resource_toggles
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
        // `undefined` is dropped by JSON.stringify in the Node build — mirror the same absence.
        root.insert(
            "tokens".into(),
            Value::Array(s.tokens.iter().map(token_json).collect()),
        );
        if !s.order.is_empty() {
            root.insert("order".into(), json!(s.order));
        }
        if !s.groups.names().is_empty() {
            // The v2 marker rides with any group write: it says "this list is complete, `default`
            // included or deleted on purpose" — the loader stops prepending `default` to marked
            // files.
            root.insert("groups".into(), json!(s.groups.names()));
            root.insert("groupsV2".into(), json!(true));
        }
        if !s.groups.members().is_empty() {
            root.insert(
                "mcpGroups".into(),
                Value::Object(
                    s.groups
                        .members()
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
        }
        if !s.token_groups.names().is_empty() {
            root.insert("tokenGroups".into(), json!(s.token_groups.names()));
        }
        if !s.token_groups.members().is_empty() {
            root.insert(
                "tokenMembers".into(),
                Value::Object(
                    s.token_groups
                        .members()
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
        }
        if !s.mcp_enabled.is_empty() {
            root.insert(
                "mcpEnabled".into(),
                Value::Object(
                    s.mcp_enabled
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
        }
        write_secure_json(&self.path, &Value::Object(root))
    }
}

fn token_json(t: &TokenRec) -> Value {
    json!({ "id": t.id, "label": t.label, "secret": t.secret, "createdAt": t.created_at })
}

impl crate::token::TokenPersistence for ManagedStore {
    fn get_tokens(&self) -> Vec<TokenRec> {
        ManagedStore::get_tokens(self)
    }
    fn save_tokens(&self, tokens: &[TokenRec]) {
        ManagedStore::save_tokens(self, tokens.to_vec())
    }
}

#[cfg(test)]
mod tests {
    //! Ported from the Node build's `test/managed.test.ts`, plus the invariants that are only
    //! visible from this side of the port: the sealed round trip, a torn file, and the loaders
    //! that Node exported separately and Rust keeps private to this module.
    use super::*;

    /// A fresh managed.json in its own temp directory, sealed under the deterministic test key so
    /// no OS credential store is touched and no test can collide with another's file.
    fn scratch() -> PathBuf {
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-managed-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("managed.json")
    }

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    fn entry(name: &str, d: Value, enabled: bool) -> ManagedEntry {
        ManagedEntry {
            name: name.into(),
            def: def(d),
            enabled,
            override_: false,
        }
    }

    fn names(list: &[ManagedEntry]) -> Vec<String> {
        list.iter().map(|e| e.name.clone()).collect()
    }

    /// Hand-authored plaintext JSON — a pre-encryption install, or a file a user dropped in.
    /// `read_secure_json` accepts it (and re-seals it on the first read), which is what lets
    /// these tests plant a malformed section without reaching for the envelope format.
    fn write_plain(path: &Path, v: Value) {
        std::fs::write(path, v.to_string()).expect("plant a plaintext managed.json");
    }

    fn raw_of(path: &Path) -> Map<String, Value> {
        match read_secure_json(path) {
            Ok(Some(Value::Object(o))) => o,
            other => panic!("expected an object on disk, got {other:?}"),
        }
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    // ---- persistence ------------------------------------------------------------------

    #[test]
    fn leaves_no_temp_file_behind_after_a_successful_save() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();

        let dir = path.parent().expect("scratch parent");
        let leftovers: Vec<String> = std::fs::read_dir(dir)
            .expect("read scratch dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        assert_eq!(raw_of(&path)["mcps"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn reports_a_failed_save_instead_of_pretending_it_worked() {
        // A path under a directory that does not exist: the write cannot succeed, and a store
        // that only logged let the admin API answer 201 Created for a def that never landed.
        let path = scratch();
        let doomed = path.parent().unwrap().join("missing").join("managed.json");
        let store = ManagedStore::open_at(doomed);
        let err = store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap_err();
        assert!(err.to_lowercase().contains("could not save"), "{err}");
    }

    #[test]
    fn replaces_the_file_only_with_complete_content() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store
            .add(entry("b", json!({ "type": "echo" }), false))
            .unwrap();
        // Whatever is on disk at any moment must parse and hold whole entries.
        assert_eq!(names(&load_managed(&path)), vec!["a", "b"]);
    }

    #[test]
    fn keeps_every_mutation_durable() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store.rename("a", "b").unwrap();
        store.set_enabled("b", false).unwrap();
        store
            .update_def("b", def(json!({ "type": "proc", "command": "node x.js" })))
            .unwrap();

        let reloaded = load_managed(&path);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].name, "b");
        assert!(!reloaded[0].enabled);
        assert!(!reloaded[0].override_);
        assert_eq!(reloaded[0].def.type_(), "proc");
        assert_eq!(reloaded[0].def.get_str("command"), Some("node x.js"));
    }

    #[test]
    fn refuses_a_duplicate_name_rather_than_shadowing_the_first() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        let err = store
            .add(entry("a", json!({ "type": "proc", "command": "x" }), true))
            .unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(store.all().len(), 1);
        assert_eq!(store.all()[0].def.type_(), "echo");
        assert!(store.has("a"));
        assert!(!store.has("nope"));
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_store_rather_than_an_error() {
        let path = scratch();
        assert!(!path.exists());
        let store = ManagedStore::open_at(path.clone());
        assert!(store.all().is_empty());
        // The group list is never empty anymore — a missing file starts from the default group.
        assert_eq!(store.get_groups(), vec![DEFAULT_GROUP]);
        assert!(store.get_order().is_empty());
        assert!(store.get_tokens().is_empty());
        assert!(load_managed(&path).is_empty());
        assert_eq!(load_managed_token(&path), None);
    }

    #[test]
    fn a_torn_file_reads_as_empty_and_the_next_save_repairs_it() {
        // The documented failure mode: an unparseable managed.json makes every loader answer
        // empty rather than throwing the boot. The first successful persist writes it whole.
        let path = scratch();
        std::fs::write(&path, "{not json at all").unwrap();
        let store = ManagedStore::open_at(path.clone());
        assert!(store.all().is_empty());
        assert!(store.get_tokens().is_empty());

        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        assert_eq!(names(&load_managed(&path)), vec!["a"]);
    }

    #[test]
    fn drops_entries_that_are_not_a_named_typed_def() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "mcps": [
                { "name": "a", "def": { "type": "echo" } },
                { "name": "b" },                                 // no def
                { "def": { "type": "echo" } },                   // no name
                { "name": "c", "def": {} },                      // def has no type
                { "name": "d", "def": { "type": 5 } },           // type is not a string
                "nope",                                          // not an object at all
            ] }),
        );
        let list = load_managed(&path);
        assert_eq!(names(&list), vec!["a"]);
        // `enabled` is absent above, and absent means on — only an explicit false stops an MCP.
        assert!(list[0].enabled);
        assert!(!list[0].override_);
    }

    #[test]
    fn an_override_round_trips_through_the_file() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store
            .upsert_override("a", def(json!({ "type": "proc", "command": "x" })))
            .unwrap();

        let reloaded = load_managed(&path);
        assert_eq!(
            reloaded.len(),
            1,
            "an override replaces the entry, it does not add one"
        );
        assert!(reloaded[0].override_);
        assert_eq!(reloaded[0].def.type_(), "proc");
    }

    // ---- tool toggles -----------------------------------------------------------------

    #[test]
    fn persists_and_reloads_disabled_tools_for_any_mcp_name() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_disabled_tools("mysql", &["mysql_query".into()])
            .unwrap();
        store
            .set_disabled_tools("redis-a-6379", &["redis_scan".into(), "redis_read".into()])
            .unwrap();

        let reloaded = ManagedStore::open_at(path.clone());
        assert_eq!(reloaded.disabled_tools("mysql"), vec!["mysql_query"]);
        assert_eq!(
            reloaded.disabled_tools("redis-a-6379"),
            vec!["redis_scan", "redis_read"]
        );

        let on_disk = load_string_map(&path, "disabledTools");
        assert_eq!(on_disk.len(), 2);
        assert_eq!(on_disk["mysql"], vec!["mysql_query"]);
    }

    #[test]
    fn answers_empty_for_an_unknown_mcp_and_drops_the_key_when_the_list_empties() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        assert!(store.disabled_tools("nope").is_empty());
        store
            .set_disabled_tools("mysql", &["mysql_query".into()])
            .unwrap();
        store.set_disabled_tools("mysql", &[]).unwrap();
        assert!(store.disabled_tools("mysql").is_empty());
        // An empty list is not persisted as a key — it is "nothing is turned off".
        assert!(load_string_map(&path, "disabledTools").is_empty());
    }

    #[test]
    fn ignores_a_malformed_disabled_tools_section_rather_than_failing_to_load() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "mcps": [], "disabledTools": { "mysql": "not-an-array", "ok": [1, 2] } }),
        );
        let reloaded = ManagedStore::open_at(path);
        assert!(reloaded.disabled_tools("mysql").is_empty()); // non-array dropped
        assert!(reloaded.disabled_tools("ok").is_empty()); // non-string entries dropped
    }

    #[test]
    fn keeps_toggles_when_an_mcp_is_added_or_removed() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_disabled_tools("mysql", &["mysql_query".into()])
            .unwrap();
        store
            .add(entry("redis", json!({ "type": "redis" }), true))
            .unwrap();
        store.remove("redis").unwrap();
        assert_eq!(
            ManagedStore::open_at(path).disabled_tools("mysql"),
            vec!["mysql_query"]
        );
    }

    // ---- resource toggles -------------------------------------------------------------

    #[test]
    fn persists_and_reloads_an_explicit_resource_on_and_off() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_resource_enabled("mysql", false).unwrap();
        store.set_resource_enabled("redis", true).unwrap();

        let reloaded = ManagedStore::open_at(path.clone());
        assert_eq!(reloaded.resource_enabled("mysql"), Some(false));
        assert_eq!(reloaded.resource_enabled("redis"), Some(true));
        // Never toggled is None, not false: the caller applies the per-type default.
        assert_eq!(reloaded.resource_enabled("never-toggled"), None);

        let on_disk = load_bool_map(&path, "resourceToggles");
        assert_eq!(on_disk.get("mysql"), Some(&false));
        assert_eq!(on_disk.get("redis"), Some(&true));
    }

    #[test]
    fn resource_toggles_survive_alongside_tool_toggles_in_the_same_file() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_disabled_tools("mysql", &["mysql_query".into()])
            .unwrap();
        store.set_resource_enabled("mysql", false).unwrap();

        let reloaded = ManagedStore::open_at(path);
        assert_eq!(reloaded.disabled_tools("mysql"), vec!["mysql_query"]);
        assert_eq!(reloaded.resource_enabled("mysql"), Some(false));
    }

    // ---- groups -----------------------------------------------------------------------

    #[test]
    fn starts_every_list_with_the_default_group_storing_nothing_until_a_save() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        // `default` is an ordinary group the loader materializes at the front of an empty list.
        assert_eq!(store.get_groups(), vec![DEFAULT_GROUP]);
        assert_eq!(store.group_of("anything"), DEFAULT_GROUP);
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();

        // Any persist carries the in-memory list — it is never empty now — but no membership
        // was made. The v2 marker rode along, so a reload of this file will not re-prepend.
        let raw = raw_of(&path);
        assert_eq!(raw.get("groups"), Some(&json!([DEFAULT_GROUP])));
        assert_eq!(raw.get("groupsV2"), Some(&json!(true)));
        assert!(raw.get("mcpGroups").is_none());
    }

    #[test]
    fn persists_custom_groups_in_order_and_reloads_them() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Search".into(), "Docs".into()])
            .unwrap();
        assert_eq!(
            ManagedStore::open_at(path.clone()).get_groups(),
            vec!["default", "Search", "Docs"]
        );
        assert_eq!(load_groups(&path), vec!["default", "Search", "Docs"]);
        // The v2 marker rode along: a reload of a DELETED default must not resurrect it.
        store.set_groups(vec!["Search".into(), "Docs".into()]).unwrap();
        assert_eq!(
            ManagedStore::open_at(path).get_groups(),
            vec!["Search", "Docs"]
        );
    }

    #[test]
    fn materializes_default_at_the_front_of_an_unmarked_pre_v2_file() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "mcps": [], "groups": ["learn", "ForTest"] }),
        );
        assert_eq!(load_groups(&path), vec!["default", "learn", "ForTest"]);
    }

    #[test]
    fn assigns_an_mcp_to_a_group_and_back_to_default_keeping_the_map_sparse() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("Docs")).unwrap();
        assert_eq!(store.group_of("context7"), "Docs");
        assert_eq!(
            load_mcp_groups(&path).get("context7").map(String::as_str),
            Some("Docs")
        );

        store.set_mcp_group("context7", None).unwrap();
        // None = "no explicit group": it renders under the FIRST group — default, still the front.
        assert_eq!(store.group_of("context7"), DEFAULT_GROUP);
        // "No explicit group" means "no entry", never an entry naming the first group.
        assert!(load_mcp_groups(&path).is_empty());
    }

    #[test]
    fn assigning_the_default_group_explicitly_stores_the_canonical_name() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into()])
            .unwrap();
        store.set_mcp_group("a", Some("DeFaUlT")).unwrap();
        // An explicit assignment to default is a real entry now — the canonical casing is stored.
        assert_eq!(store.group_of("a"), DEFAULT_GROUP);
        assert_eq!(
            load_mcp_groups(&path).get("a").map(String::as_str),
            Some("default")
        );
    }

    #[test]
    fn stores_the_canonical_casing_of_a_group_not_what_the_caller_typed() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_groups(vec!["Docs".into()]).unwrap();
        store.set_mcp_group("a", Some("DOCS")).unwrap();
        assert_eq!(store.group_of("a"), "Docs");
        assert_eq!(
            load_mcp_groups(&path).get("a").map(String::as_str),
            Some("Docs")
        );
    }

    #[test]
    fn groups_a_config_sourced_mcp_that_has_no_managed_entry_at_all() {
        // The whole reason membership is a name-keyed map and not a field on ServerDef: config
        // MCPs never get a ManagedEntry, and they are the ones this user actually has.
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_groups(vec!["Docs".into()]).unwrap();
        store.set_mcp_group("context7", Some("Docs")).unwrap();
        assert!(store.all().is_empty(), "no managed entry may be created");
        assert_eq!(ManagedStore::open_at(path).group_of("context7"), "Docs");
    }

    #[test]
    fn refuses_an_unknown_group_so_a_typo_cannot_strand_an_mcp() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        let err = store.set_mcp_group("a", Some("Nope")).unwrap_err();
        assert!(err.to_lowercase().contains("unknown group"), "{err}");
    }

    #[test]
    fn treats_default_as_an_ordinary_name_rejecting_only_real_conflicts() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_groups(vec!["default".into(), "Docs".into()]).unwrap();
        assert!(store
            .set_groups(vec!["Docs".into(), "docs".into()])
            .unwrap_err()
            .contains("duplicate"));
        assert!(store
            .set_groups(vec![" ".into()])
            .unwrap_err()
            .contains("empty"));
        // The list may never be empty: something must catch unassigned MCPs.
        assert!(store
            .set_groups(vec![])
            .unwrap_err()
            .contains("at least one group"));
        // A refused call leaves the previous list alone.
        assert_eq!(store.get_groups(), vec!["default", "Docs"]);
    }

    #[test]
    fn trims_group_names_before_storing_them() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_groups(vec!["  Docs  ".into()]).unwrap();
        assert_eq!(store.get_groups(), vec!["Docs"]);
    }

    #[test]
    fn renames_a_group_and_carries_its_members_across() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["Docs".into(), "Search".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("Docs")).unwrap();
        store.set_mcp_group("deepwiki", Some("Docs")).unwrap();
        store.rename_group("Docs", "Reference").unwrap();

        let reloaded = ManagedStore::open_at(path);
        assert_eq!(
            reloaded.get_groups(),
            vec!["Reference", "Search"],
            "the slot is kept"
        );
        assert_eq!(reloaded.group_of("context7"), "Reference");
        assert_eq!(reloaded.group_of("deepwiki"), "Reference");
    }

    #[test]
    fn refuses_to_rename_onto_an_existing_group_or_an_unknown_one() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store
            .set_groups(vec!["default".into(), "Docs".into(), "Search".into()])
            .unwrap();
        assert!(store
            .rename_group("Docs", "Search")
            .unwrap_err()
            .contains("duplicate"));
        assert!(store
            .rename_group("Missing", "X")
            .unwrap_err()
            .to_lowercase()
            .contains("unknown group"));
        assert!(store
            .rename_group("Docs", "  ")
            .unwrap_err()
            .contains("empty"));
        assert_eq!(store.get_groups(), vec!["default", "Docs", "Search"]);
    }

    #[test]
    fn renames_default_like_any_other_group_carrying_the_first_slot_with_it() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("default")).unwrap();
        store.rename_group("default", "主力").unwrap();

        let reloaded = ManagedStore::open_at(path);
        assert_eq!(reloaded.get_groups(), vec!["主力", "Docs"]);
        // An explicit member is rewritten; an unassigned one follows the slot on its own.
        assert_eq!(reloaded.group_of("context7"), "主力");
        assert_eq!(reloaded.group_of("deepwiki"), "主力");
    }

    #[test]
    fn renaming_a_group_to_its_own_other_casing_is_allowed() {
        // The duplicate check must skip the row being renamed, or fixing a group's capitalisation
        // would report a collision with itself.
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_groups(vec!["docs".into()]).unwrap();
        store.set_mcp_group("a", Some("docs")).unwrap();
        store.rename_group("docs", "Docs").unwrap();
        assert_eq!(store.get_groups(), vec!["Docs"]);
        assert_eq!(store.group_of("a"), "Docs");
    }

    #[test]
    fn drops_a_group_without_deleting_its_mcps() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("Docs")).unwrap();
        store.remove_group("Docs").unwrap();

        let reloaded = ManagedStore::open_at(path.clone());
        assert_eq!(reloaded.get_groups(), vec![DEFAULT_GROUP]);
        assert_eq!(reloaded.group_of("context7"), DEFAULT_GROUP);
        assert!(load_mcp_groups(&path).is_empty());
    }

    #[test]
    fn deleting_the_group_that_owns_the_first_slot_moves_the_sink() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into(), "Extra".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("default")).unwrap();
        store.remove_group("default").unwrap();

        // The first remaining group catches both the explicit member and the unassigned ones.
        assert_eq!(store.get_groups(), vec!["Docs", "Extra"]);
        assert_eq!(store.group_of("context7"), "Docs");
        assert_eq!(store.group_of("deepwiki"), "Docs");
    }

    #[test]
    fn refuses_to_delete_the_last_remaining_group() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_groups(vec!["Docs".into()]).unwrap();
        let err = store.remove_group("Docs").unwrap_err();
        assert!(err.contains("at least one group"), "{err}");
        assert_eq!(store.get_groups(), vec!["Docs"]);
    }

    #[test]
    fn removing_an_unknown_group_is_a_no_op_not_an_error() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_groups(vec!["Docs".into()]).unwrap();
        store.remove_group("Nope").unwrap();
        assert_eq!(store.get_groups(), vec!["Docs"]);
    }

    #[test]
    fn prunes_members_of_groups_dropped_by_a_whole_list_set_groups() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_groups(vec!["default".into(), "Docs".into(), "Search".into()])
            .unwrap();
        store.set_mcp_group("context7", Some("Docs")).unwrap();
        store.set_mcp_group("github", Some("Search")).unwrap();
        store
            .set_groups(vec!["default".into(), "Search".into()])
            .unwrap(); // Docs deleted by omission

        assert_eq!(store.group_of("context7"), DEFAULT_GROUP);
        assert_eq!(store.group_of("github"), "Search");
        let on_disk = load_mcp_groups(&path);
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk.get("github").map(String::as_str), Some("Search"));
    }

    #[test]
    fn follows_an_mcp_through_rename_and_forgets_it_on_delete_like_order_does() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_groups(vec!["Docs".into()]).unwrap();
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store.set_mcp_group("a", Some("Docs")).unwrap();
        store.set_order(vec!["a".into()]).unwrap();

        store.rename("a", "b").unwrap();
        assert_eq!(store.group_of("b"), "Docs");
        // The old name answers via the first group: the only group here is Docs.
        assert_eq!(store.group_of("a"), "Docs");
        assert_eq!(
            store.get_order(),
            vec!["b"],
            "a rename must not shuffle the sidebar"
        );

        store.remove("b").unwrap();
        assert!(load_mcp_groups(&path).is_empty());
        assert!(load_order(&path).is_empty());
    }

    #[test]
    fn treats_a_member_of_a_group_that_vanished_from_the_file_as_default() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "mcps": [], "groups": ["Docs"], "mcpGroups": { "a": "Docs", "b": "Ghost" } }),
        );
        let store = ManagedStore::open_at(path);
        // Unmarked file: default is prepended and owns the first slot.
        assert_eq!(store.get_groups(), vec!["default", "Docs"]);
        assert_eq!(store.group_of("a"), "Docs");
        assert_eq!(store.group_of("b"), DEFAULT_GROUP);
    }

    #[test]
    fn ignores_a_malformed_groups_section_rather_than_failing_to_load() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "mcps": [], "groups": "nope", "mcpGroups": { "a": 5, "b": "Docs" } }),
        );
        let store = ManagedStore::open_at(path);
        assert_eq!(store.get_groups(), vec![DEFAULT_GROUP]);
        assert_eq!(store.group_of("a"), DEFAULT_GROUP); // non-string value dropped
        assert_eq!(store.group_of("b"), DEFAULT_GROUP); // the list is default-only, so Docs is dead
    }

    #[test]
    fn groups_survive_alongside_order_toggles_and_tokens_in_one_file() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_order(vec!["b".into(), "a".into()]).unwrap();
        store.set_disabled_tools("a", &["t".into()]).unwrap();
        store.set_groups(vec!["Docs".into()]).unwrap();
        store.set_mcp_group("a", Some("Docs")).unwrap();
        store.save_tokens(vec![TokenRec {
            id: "t1".into(),
            label: "default".into(),
            secret: "s".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        }]);

        let reloaded = ManagedStore::open_at(path);
        assert_eq!(reloaded.get_order(), vec!["b", "a"]);
        assert_eq!(reloaded.disabled_tools("a"), vec!["t"]);
        assert_eq!(reloaded.get_groups(), vec!["Docs"]);
        assert_eq!(reloaded.group_of("a"), "Docs");
        assert_eq!(reloaded.get_tokens().len(), 1);
    }

    // ---- order ------------------------------------------------------------------------

    #[test]
    fn the_sidebar_order_collapses_duplicates_and_drops_blanks() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .set_order(vec!["b".into(), "a".into(), "b".into(), String::new()])
            .unwrap();
        assert_eq!(store.get_order(), vec!["b", "a"]);
        assert_eq!(load_order(&path), vec!["b", "a"]);
    }

    #[test]
    fn an_empty_order_gives_up_the_arrangement_and_stores_no_key() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_order(vec!["b".into(), "a".into()]).unwrap();
        store.set_order(Vec::new()).unwrap();
        assert!(store.get_order().is_empty());
        assert!(raw_of(&path).get("order").is_none());
        assert!(load_order(&path).is_empty());
    }

    // ---- tokens -----------------------------------------------------------------------

    #[test]
    fn the_token_set_round_trips_through_the_file() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        let rec = TokenRec {
            id: "id-1".into(),
            label: "claude-code".into(),
            secret: "sec-1".into(),
            created_at: "2026-02-03T04:05:06.000Z".into(),
        };
        store.save_tokens(vec![rec.clone()]);
        assert_eq!(load_tokens(&path), vec![rec.clone()]);
        assert_eq!(ManagedStore::open_at(path.clone()).get_tokens(), vec![rec]);

        // Revoking the last token persists the empty set, it does not leave the old one behind.
        store.save_tokens(Vec::new());
        assert!(ManagedStore::open_at(path).get_tokens().is_empty());
    }

    #[test]
    fn a_token_row_missing_a_required_field_is_dropped_not_defaulted() {
        let path = scratch();
        write_plain(
            &path,
            json!({ "tokens": [
                { "id": "a", "label": "l", "secret": "s", "createdAt": "t" },
                { "id": "b", "label": "l" },              // no secret
                { "label": "l", "secret": "s" },          // no id
                { "id": "d", "label": "l", "secret": "s" }, // no createdAt: allowed, empty
            ] }),
        );
        let list = load_tokens(&path);
        assert_eq!(
            list.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec!["a", "d"]
        );
        assert_eq!(list[1].created_at, "");
    }

    #[test]
    fn a_rotated_bearer_token_is_read_back_and_an_empty_one_is_not() {
        let path = scratch();
        write_plain(&path, json!({ "token": "rotated-secret" }));
        assert_eq!(load_managed_token(&path).as_deref(), Some("rotated-secret"));

        let blank = scratch();
        write_plain(&blank, json!({ "token": "" }));
        assert_eq!(load_managed_token(&blank), None);
    }

    // ---- mcpEnabled: run/stop state for config-sourced MCPs ----------------------------

    #[test]
    fn persists_a_stop_on_a_name_with_no_managed_entry() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_enabled("from-config", false).unwrap();
        assert_eq!(store.enabled_for("from-config"), Some(false));

        let reloaded = ManagedStore::open_at(path.clone());
        assert_eq!(reloaded.enabled_for("from-config"), Some(false));
        reloaded.set_enabled("from-config", true).unwrap();
        assert_eq!(
            ManagedStore::open_at(path).enabled_for("from-config"),
            Some(true)
        );
    }

    #[test]
    fn run_stop_state_defaults_to_none_when_nothing_was_recorded() {
        let path = scratch();
        // None, not Some(true): the caller decides what an unrecorded MCP does.
        assert_eq!(
            ManagedStore::open_at(path).enabled_for("never-touched"),
            None
        );
    }

    #[test]
    fn moves_the_run_stop_flag_on_rename_and_clears_it_on_remove() {
        let path = scratch();
        let store = ManagedStore::open_at(path);
        store.set_enabled("cfg-a", false).unwrap();
        store.rename("cfg-a", "cfg-b").unwrap();
        assert_eq!(store.enabled_for("cfg-a"), None);
        assert_eq!(store.enabled_for("cfg-b"), Some(false));
        store.remove("cfg-b").unwrap();
        assert_eq!(store.enabled_for("cfg-b"), None);
    }

    #[test]
    fn consumes_the_run_stop_flag_when_the_mcp_becomes_a_managed_override() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store.set_enabled("cfg-a", false).unwrap();
        store
            .upsert_override("cfg-a", def(json!({ "type": "echo" })))
            .unwrap();
        // The entry inherits the Stop instead of resetting it: an edit that flipped a stopped
        // MCP back on read as the panel ignoring the Stop.
        assert_eq!(store.enabled_for("cfg-a"), Some(false));
        assert!(!store.all()[0].enabled);
        // ...and the side-map entry is consumed, not left to shadow the entry later.
        assert!(raw_of(&path).get("mcpEnabled").is_none());
    }

    #[test]
    fn a_managed_entrys_own_flag_wins_over_the_config_side_map() {
        let path = scratch();
        write_plain(
            &path,
            json!({
                "mcps": [{ "name": "a", "def": { "type": "echo" }, "enabled": true }],
                "mcpEnabled": { "a": false },
            }),
        );
        let store = ManagedStore::open_at(path);
        assert_eq!(store.enabled_for("a"), Some(true));
    }

    #[test]
    fn set_enabled_on_a_managed_entry_does_not_grow_the_config_side_map() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store.set_enabled("a", false).unwrap();
        assert_eq!(store.enabled_for("a"), Some(false));
        assert!(raw_of(&path).get("mcpEnabled").is_none());
    }

    #[test]
    fn update_def_on_an_unknown_name_is_a_no_op_not_an_insert() {
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .update_def("ghost", def(json!({ "type": "echo" })))
            .unwrap();
        assert!(store.all().is_empty());
        assert!(!path.exists(), "a no-op must not write the file at all");
    }

    #[test]
    fn every_section_survives_one_reload_together() {
        // The regression this catches: a persist that writes one section from stale state.
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        store.set_disabled_tools("a", &["t1".into()]).unwrap();
        store.set_resource_enabled("a", false).unwrap();
        store.set_groups(vec!["Docs".into()]).unwrap();
        store.set_mcp_group("a", Some("Docs")).unwrap();
        store.set_order(vec!["a".into()]).unwrap();
        store.set_enabled("cfg-only", false).unwrap();
        store.save_tokens(vec![TokenRec {
            id: "t".into(),
            label: "default".into(),
            secret: "s".into(),
            created_at: "now".into(),
        }]);

        let r = ManagedStore::open_at(path);
        assert_eq!(names(&r.all()), vec!["a"]);
        assert_eq!(r.disabled_tools("a"), vec!["t1"]);
        assert_eq!(r.resource_enabled("a"), Some(false));
        assert_eq!(r.get_groups(), vec!["Docs"]);
        assert_eq!(
            sorted(r.get_mcp_groups().keys().cloned().collect()),
            vec!["a"]
        );
        assert_eq!(r.get_order(), vec!["a"]);
        assert_eq!(r.enabled_for("cfg-only"), Some(false));
        assert_eq!(r.get_tokens().len(), 1);
    }

    #[test]
    fn the_file_on_disk_is_sealed_not_plaintext() {
        // managed.json holds ${ENV} refs, tokens and the bearer secret; the envelope is the only
        // shape it may ever reach the disk in.
        let path = scratch();
        let store = ManagedStore::open_at(path.clone());
        store
            .add(entry("a", json!({ "type": "echo" }), true))
            .unwrap();
        let text = std::fs::read_to_string(&path).expect("read the sealed file");
        assert!(
            !text.contains("\"echo\""),
            "the def leaked in plaintext: {text}"
        );
        let raw: Value = serde_json::from_str(&text).expect("an envelope is still JSON");
        assert!(swiss_core::secure::envelope::is_sealed(&raw), "{text}");
    }
}
