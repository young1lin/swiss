//! managed.json — port of `managed.ts`. User-added MCPs, overrides, toggles, enabled flags,
//! named tokens, sidebar order and groups — all in one sealed state file.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::config::ServerDef;
use crate::log;
use crate::paths::data_path;
use crate::secure::statefile::{read_secure_json, write_secure_json};
use crate::token::TokenRec;

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
/// MCP in the default group has no entry in `mcpGroups`, so a file written before groups existed
/// already means "everything is in default". The name is reserved — a custom group may not take
/// it, because it is the sink a deleted group's members fall back into.
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

/// The user's custom sidebar groups, in display order. `default` is never in here.
pub fn load_groups(path: &Path) -> Vec<String> {
    let Some(raw) = read_managed_raw(path) else {
        return Vec::new();
    };
    match raw.get("groups") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
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
    groups: Vec<String>,
    mcp_groups: HashMap<String, String>,
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
            groups: load_groups(&path),
            mcp_groups: load_mcp_groups(&path),
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
        s.mcp_groups.remove(name); // ...nor a group membership
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
        if let Some(g) = s.mcp_groups.remove(old_name) {
            s.mcp_groups.insert(new_name.into(), g);
        }
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

    /// Replace the whole token set (create / rotate / revoke all persist through here).
    pub fn save_tokens(&self, next: Vec<TokenRec>) {
        if let Ok(mut s) = self.state.lock() {
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
            .map(|s| s.groups.clone())
            .unwrap_or_default()
    }

    /// The group this MCP is in. Unassigned — or assigned to a group since deleted — means
    /// default.
    pub fn group_of(&self, name: &str) -> String {
        self.state
            .lock()
            .ok()
            .and_then(|s| {
                s.mcp_groups
                    .get(name)
                    .filter(|g| s.groups.iter().any(|x| x == *g))
                    .cloned()
            })
            .unwrap_or_else(|| DEFAULT_GROUP.into())
    }

    pub fn get_mcp_groups(&self) -> BTreeMap<String, String> {
        self.state
            .lock()
            .map(|s| s.mcp_groups.clone().into_iter().collect())
            .unwrap_or_default()
    }

    /// Replace the whole custom-group list: create, delete and reorder in one call. A group
    /// dropped by omission is deleted, and its members fall back to the default group.
    pub fn set_groups(&self, names: Vec<String>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        let mut next = Vec::new();
        for raw in names {
            let name = raw.trim();
            if name.is_empty() {
                return Err("a group name cannot be empty".into());
            }
            if same_name(name, DEFAULT_GROUP) {
                return Err(format!("\"{DEFAULT_GROUP}\" is a reserved group name"));
            }
            if next.iter().any(|n: &String| same_name(n, name)) {
                return Err(format!("duplicate group name: {name}"));
            }
            next.push(name.to_string());
        }
        s.groups = next;
        let live = s.groups.clone();
        s.mcp_groups.retain(|_, g| live.iter().any(|n| n == g));
        self.persist(&s)
    }

    /// Put an MCP in a group, or back into the default group with None.
    pub fn set_mcp_group(&self, name: &str, group: Option<&str>) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        match group {
            None => {
                s.mcp_groups.remove(name);
            }
            Some(g) if same_name(g, DEFAULT_GROUP) => {
                s.mcp_groups.remove(name);
            }
            Some(g) => {
                let match_ = s
                    .groups
                    .iter()
                    .find(|n| same_name(n, g))
                    .cloned()
                    .ok_or_else(|| format!("unknown group: {g}"))?;
                // Store the canonical casing, not what the caller typed.
                s.mcp_groups.insert(name.into(), match_);
            }
        }
        self.persist(&s)
    }

    /// Rename a group in place, carrying its members with it and keeping its slot.
    pub fn rename_group(&self, from: &str, to: &str) -> Result<(), String> {
        let next = to.trim();
        if next.is_empty() {
            return Err("a group name cannot be empty".into());
        }
        if same_name(next, DEFAULT_GROUP) {
            return Err(format!("\"{DEFAULT_GROUP}\" is a reserved group name"));
        }
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        let i = s
            .groups
            .iter()
            .position(|n| same_name(n, from))
            .ok_or_else(|| format!("unknown group: {from}"))?;
        if s.groups
            .iter()
            .enumerate()
            .any(|(j, n)| j != i && same_name(n, next))
        {
            return Err(format!("duplicate group name: {next}"));
        }
        let old = s.groups[i].clone();
        s.groups[i] = next.to_string();
        for g in s.mcp_groups.values_mut() {
            if *g == old {
                *g = next.to_string();
            }
        }
        self.persist(&s)
    }

    /// Delete a group. Its MCPs are not touched — they fall back into the default group.
    pub fn remove_group(&self, name: &str) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "store poisoned")?;
        let Some(match_) = s.groups.iter().find(|n| same_name(n, name)).cloned() else {
            return Ok(());
        };
        s.groups.retain(|n| *n != match_);
        s.mcp_groups.retain(|_, g| *g != match_);
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
        if !s.groups.is_empty() {
            root.insert("groups".into(), json!(s.groups));
        }
        if !s.mcp_groups.is_empty() {
            root.insert(
                "mcpGroups".into(),
                Value::Object(
                    s.mcp_groups
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
