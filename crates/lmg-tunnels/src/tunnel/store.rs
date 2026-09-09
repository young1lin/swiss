//! Persists SSH connections and forwarding rules to tunnels.json — port of `tunnels/store.ts`.
//!
//! Pure data: it validates and stores, and knows nothing about sockets or russh — which is what
//! makes every rule in here testable without a network. The file is a sealed envelope (the same
//! `write_secure_json` every state file uses), and `${ENV_VAR}` refs inside passwords/passphrases
//! stay refs on disk — they expand at connect time in `ssh.rs`, exactly like the Node build.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::types::{
    fmt_number, is_port_f, new_id, opt_str, value_number, AuthType, GroupKind, RuleDef, SshConnDef,
};
use lmg_core::log;
use lmg_core::secure::statefile::{read_secure_json, write_secure_json};

/// The group name every ungrouped row belongs to. Reserved: a stored group may not use it.
pub const DEFAULT_GROUP: &str = "default";

/// Input shape for a connection save: the API layer has already applied JS coercions (String /
/// Number), so numeric fields arrive as f64 (NaN where JS would have NaN) and the validation
/// below reproduces `str()`/`num()` on top.
#[derive(Debug, Clone, Default)]
pub struct ConnInput {
    pub id: Option<String>,
    pub name: String,
    pub host: String,
    pub port: f64,
    pub username: String,
    pub auth_type: AuthType,
    pub key_path: Option<String>,
    pub passphrase: Option<String>,
    pub password: Option<String>,
    pub host_key: Option<String>,
    pub group: Option<String>,
}

/// Input shape for a rule save. `mcps: None` means "not sent" (an update keeps the stored
/// links); `Some(vec![])` clears them.
#[derive(Debug, Clone, Default)]
pub struct RuleInput {
    pub id: Option<String>,
    pub name: String,
    pub connection_id: String,
    pub local_port: f64,
    pub target_host: String,
    pub target_port: f64,
    pub remark: String,
    pub auto_reconnect: bool,
    pub reconnect_interval: f64,
    pub enabled: Option<bool>,
    pub mcps: Option<Vec<String>>,
    pub group: Option<String>,
}

/// `num(v, fallback)` from the Node store: finite -> trunc, else the fallback.
fn num_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v.trunc()
    } else {
        fallback
    }
}

/// `Math.max(1, num(v, 10) || 10)`: NaN -> 10, 0 -> 10, fractions trunc, floor 1.
fn interval_or(v: f64) -> u64 {
    let n = num_or(v, 10.0);
    let n = if n == 0.0 { 10.0 } else { n };
    n.max(1.0) as u64
}

fn clean_mcps(v: Option<&Vec<String>>) -> Vec<String> {
    v.map(|list| {
        list.iter()
            .filter(|m| !m.is_empty())
            .cloned()
            .collect::<Vec<_>>()
    })
    .unwrap_or_default()
}

pub struct TunnelStore {
    conns: Vec<SshConnDef>,
    rules: Vec<RuleDef>,
    /// Custom group names per list, in panel order. The default group is implicit, never stored.
    rule_groups: Vec<String>,
    conn_groups: Vec<String>,
    path: PathBuf,
    /// Rejected as a local port, since binding it could never work.
    gateway_port: u16,
}

impl TunnelStore {
    /// Loads (best effort — a corrupt file logs and leaves the store empty, like the Node build).
    pub fn new(path: impl Into<PathBuf>, gateway_port: u16) -> Self {
        let mut store = TunnelStore {
            conns: Vec::new(),
            rules: Vec::new(),
            rule_groups: Vec::new(),
            conn_groups: Vec::new(),
            path: path.into(),
            gateway_port,
        };
        store.load();
        store
    }

    fn load(&mut self) {
        let raw = match read_secure_json(&self.path) {
            Ok(Some(raw)) => raw,
            Ok(None) => return,
            Err(err) => {
                log::warn(
                    "tunnels load failed",
                    Some(json!({ "err": err, "path": self.path.display().to_string() })),
                );
                return;
            }
        };
        for c in raw
            .get("connections")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let Some(id) = c.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = c.get("name").and_then(Value::as_str).unwrap_or("").trim();
            let host = c.get("host").and_then(Value::as_str).unwrap_or("").trim();
            if name.is_empty() || host.is_empty() {
                continue;
            }
            let port = value_number(c.get("port").unwrap_or(&Value::Null));
            let auth_type = if c.get("authType").and_then(Value::as_str) == Some("password") {
                AuthType::Password
            } else {
                AuthType::Key
            };
            self.conns.push(SshConnDef {
                id: id.to_string(),
                name: name.to_string(),
                host: host.to_string(),
                port: if is_port_f(port) { port as u16 } else { 22 },
                username: c
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                auth_type,
                key_path: c.get("keyPath").and_then(opt_str),
                passphrase: c.get("passphrase").and_then(opt_str),
                password: c.get("password").and_then(opt_str),
                host_key: nonempty(c.get("hostKey").and_then(Value::as_str).map(str::trim)),
                group: nonempty(c.get("group").and_then(Value::as_str).map(str::trim)),
            });
        }
        for r in raw
            .get("rules")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let Some(id) = r.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = r.get("name").and_then(Value::as_str).unwrap_or("").trim();
            let local_port = value_number(r.get("localPort").unwrap_or(&Value::Null));
            if name.is_empty() || !is_port_f(local_port) {
                continue;
            }
            // A rule whose connectionId is unknown is KEPT: it surfaces as
            // `error: unknown connection` rather than vanishing, so one bad hand-edit cannot
            // silently discard the other 17 rules.
            let target_port = value_number(r.get("targetPort").unwrap_or(&Value::Null));
            self.rules.push(RuleDef {
                id: id.to_string(),
                name: name.to_string(),
                connection_id: r
                    .get("connectionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                local_port: local_port as u16,
                target_host: nonempty(r.get("targetHost").and_then(Value::as_str).map(str::trim))
                    .unwrap_or_else(|| "127.0.0.1".into()),
                target_port: if is_port_f(target_port) {
                    target_port as u16
                } else {
                    local_port as u16
                },
                remark: r
                    .get("remark")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                auto_reconnect: r.get("autoReconnect") == Some(&Value::Bool(true)),
                reconnect_interval: interval_or(value_number(
                    r.get("reconnectInterval").unwrap_or(&json!(10)),
                )),
                enabled: r.get("enabled") == Some(&Value::Bool(true)),
                mcps: r
                    .get("mcps")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .filter(|m| !m.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                group: nonempty(r.get("group").and_then(Value::as_str).map(str::trim)),
            });
        }
        let group_list = |raw: &Value, key: &str| -> Vec<String> {
            raw.get(key)
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|n| !n.is_empty() && *n != DEFAULT_GROUP)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        self.rule_groups = group_list(&raw, "ruleGroups");
        self.conn_groups = group_list(&raw, "connGroups");
    }

    fn persist(&self) -> Result<(), String> {
        let body = json!({
            "connections": self.conns.iter().map(SshConnDef::to_json).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(RuleDef::to_json).collect::<Vec<_>>(),
            "ruleGroups": self.rule_groups,
            "connGroups": self.conn_groups,
        });
        write_secure_json(&self.path, &body)
    }

    pub fn is_empty(&self) -> bool {
        self.conns.is_empty() && self.rules.is_empty()
    }

    /// True when there is no file at all — the signal for a first-run import.
    pub fn is_fresh(&self) -> bool {
        !self.path.exists()
    }

    pub fn connections(&self) -> Vec<SshConnDef> {
        self.conns.clone()
    }

    pub fn rules(&self) -> Vec<RuleDef> {
        self.rules.clone()
    }

    pub fn connection(&self, id: &str) -> Option<SshConnDef> {
        self.conns.iter().find(|c| c.id == id).cloned()
    }

    pub fn rule(&self, id: &str) -> Option<RuleDef> {
        self.rules.iter().find(|r| r.id == id).cloned()
    }

    pub fn rules_for_connection(&self, id: &str) -> Vec<RuleDef> {
        self.rules
            .iter()
            .filter(|r| r.connection_id == id)
            .cloned()
            .collect()
    }

    // --- validation -------------------------------------------------------------------------------

    fn valid_conn(&self, input: &ConnInput) -> Result<SshConnDef, String> {
        if input.name.trim().is_empty() {
            return Err("name is required".into());
        }
        if input.host.is_empty() {
            return Err("host is required".into());
        }
        if input.username.is_empty() {
            return Err("username is required".into());
        }
        let port = num_or(input.port, 22.0);
        if !is_port_f(port) {
            return Err(format!("invalid SSH port: {}", fmt_number(input.port)));
        }
        let mut def = SshConnDef {
            id: input.id.clone().unwrap_or_else(new_id),
            name: input.name.trim().to_string(),
            host: input.host.trim().to_string(),
            port: port as u16,
            username: input.username.trim().to_string(),
            auth_type: input.auth_type,
            group: None,
            key_path: None,
            passphrase: None,
            password: None,
            host_key: None,
        };
        match input.auth_type {
            AuthType::Key => {
                let Some(key_path) = input.key_path.as_deref().map(str::trim) else {
                    return Err("a private key path is required for key authentication".into());
                };
                if key_path.is_empty() {
                    return Err("a private key path is required for key authentication".into());
                }
                def.key_path = Some(key_path.to_string());
                if let Some(pass) = &input.passphrase {
                    if !pass.is_empty() {
                        def.passphrase = Some(pass.clone());
                    }
                }
            }
            AuthType::Password => match &input.password {
                Some(p) if !p.is_empty() => def.password = Some(p.clone()),
                _ => return Err("a password is required for password authentication".into()),
            },
        }
        if let Some(hk) = &input.host_key {
            if !hk.trim().is_empty() {
                def.host_key = Some(hk.trim().to_string());
            }
        }
        if let Some(g) = &input.group {
            if !g.trim().is_empty() {
                def.group = Some(g.trim().to_string());
            }
        }
        Ok(def)
    }

    fn valid_rule(&self, input: &RuleInput, self_id: Option<&str>) -> Result<RuleDef, String> {
        if input.name.trim().is_empty() {
            return Err("name is required".into());
        }
        let connection_id = input.connection_id.trim();
        if !self.conns.iter().any(|c| c.id == connection_id) {
            return Err(format!(
                "unknown SSH connection: {}",
                if connection_id.is_empty() {
                    "(none selected)"
                } else {
                    connection_id
                }
            ));
        }
        let local_port = num_or(input.local_port, f64::NAN);
        if !is_port_f(local_port) {
            return Err(format!(
                "invalid local port: {}",
                fmt_number(input.local_port)
            ));
        }
        let local_port = local_port as u16;
        if self.gateway_port != 0 && local_port == self.gateway_port {
            return Err(format!("local port {local_port} is the gateway's own port"));
        }
        if let Some(clash) = self
            .rules
            .iter()
            .find(|r| r.local_port == local_port && Some(r.id.as_str()) != self_id)
        {
            return Err(format!(
                "local port {local_port} is already used by '{}'",
                clash.name
            ));
        }
        let target_port = num_or(input.target_port, f64::NAN);
        if !is_port_f(target_port) {
            return Err(format!(
                "invalid target port: {}",
                fmt_number(input.target_port)
            ));
        }
        let mut def = RuleDef {
            id: input
                .id
                .clone()
                .or_else(|| self_id.map(str::to_string))
                .unwrap_or_else(new_id),
            name: input.name.trim().to_string(),
            connection_id: connection_id.to_string(),
            local_port,
            target_host: {
                let h = input.target_host.trim();
                if h.is_empty() {
                    "127.0.0.1".into()
                } else {
                    h.to_string()
                }
            },
            target_port: target_port as u16,
            remark: input.remark.trim().to_string(),
            auto_reconnect: input.auto_reconnect,
            reconnect_interval: interval_or(input.reconnect_interval),
            enabled: input.enabled == Some(true),
            mcps: clean_mcps(input.mcps.as_ref()),
            group: None,
        };
        if let Some(g) = &input.group {
            if !g.trim().is_empty() {
                def.group = Some(g.trim().to_string());
            }
        }
        Ok(def)
    }

    // --- mutations --------------------------------------------------------------------------------

    pub fn add_connection(&mut self, input: &ConnInput) -> Result<SshConnDef, String> {
        let def = self.valid_conn(input)?;
        if self.conns.iter().any(|c| c.id == def.id) {
            return Err(format!("connection already exists: {}", def.id));
        }
        self.conns.push(def.clone());
        self.persist()?;
        Ok(def)
    }

    pub fn update_connection(&mut self, id: &str, input: &ConnInput) -> Result<SshConnDef, String> {
        let Some(i) = self.conns.iter().position(|c| c.id == id) else {
            return Err(format!("unknown SSH connection: {id}"));
        };
        let mut input = input.clone();
        input.id = Some(id.to_string());
        let mut def = self.valid_conn(&input)?;
        // A learned host key survives an edit unless the host itself moved — re-approving a
        // fingerprint because a username was corrected would train the user to click through the
        // one prompt that exists to stop a man-in-the-middle.
        if def.host_key.is_none()
            && def.host == self.conns[i].host
            && def.port == self.conns[i].port
        {
            def.host_key = self.conns[i].host_key.clone();
        }
        if def.group.is_none() {
            def.group = self.conns[i].group.clone();
        }
        self.conns[i] = def.clone();
        self.persist()?;
        Ok(def)
    }

    pub fn remove_connection(&mut self, id: &str) -> Result<(), String> {
        let used: Vec<&RuleDef> = self
            .rules
            .iter()
            .filter(|r| r.connection_id == id)
            .collect();
        if !used.is_empty() {
            return Err(format!(
                "connection is still used by: {}",
                used.iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let before = self.conns.len();
        self.conns.retain(|c| c.id != id);
        if self.conns.len() != before {
            self.persist()?;
        }
        Ok(())
    }

    pub fn add_rule(&mut self, input: &RuleInput) -> Result<RuleDef, String> {
        let def = self.valid_rule(input, None)?;
        if self.rules.iter().any(|r| r.id == def.id) {
            return Err(format!("rule already exists: {}", def.id));
        }
        self.rules.push(def.clone());
        self.persist()?;
        Ok(def)
    }

    pub fn update_rule(&mut self, id: &str, input: &RuleInput) -> Result<RuleDef, String> {
        let Some(i) = self.rules.iter().position(|r| r.id == id) else {
            return Err(format!("unknown rule: {id}"));
        };
        // `enabled` is runtime state owned by the manager, not something an edit may silently
        // flip; `mcps` omitted from the input keeps the stored links — the same courtesy `group`
        // gets below. (An explicit [] still clears them; a create with no mcps starts from [].)
        let mut input = input.clone();
        if input.mcps.is_none() {
            input.mcps = Some(self.rules[i].mcps.clone());
        }
        input.id = Some(id.to_string());
        input.enabled = Some(self.rules[i].enabled);
        let mut def = self.valid_rule(&input, Some(id))?;
        if def.group.is_none() {
            def.group = self.rules[i].group.clone();
        }
        self.rules[i] = def.clone();
        self.persist()?;
        Ok(def)
    }

    pub fn remove_rule(&mut self, id: &str) -> Result<(), String> {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        if self.rules.len() != before {
            self.persist()?;
        }
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let Some(r) = self.rules.iter_mut().find(|x| x.id == id) else {
            return Ok(());
        };
        if r.enabled == enabled {
            return Ok(());
        }
        r.enabled = enabled;
        self.persist()
    }

    /// Drop a connection's learned host key (a trust reset): the next connect learns it afresh.
    /// `update_connection` cannot express this — it deliberately re-adopts the old key when
    /// host/port are unchanged, which would silently undo the clear on the very next edit.
    pub fn clear_host_key(&mut self, id: &str) -> Result<(), String> {
        let Some(c) = self.conns.iter_mut().find(|x| x.id == id) else {
            return Ok(());
        };
        if c.host_key.is_none() {
            return Ok(());
        }
        c.host_key = None;
        self.persist()
    }

    pub fn set_host_key(&mut self, id: &str, fingerprint: &str) -> Result<(), String> {
        let Some(c) = self.conns.iter_mut().find(|x| x.id == id) else {
            return Ok(());
        };
        if c.host_key.as_deref() == Some(fingerprint) {
            return Ok(());
        }
        c.host_key = Some(fingerprint.to_string());
        self.persist()
    }

    /// Replace everything at once (the first-run import).
    pub fn replace_all(
        &mut self,
        conns: Vec<SshConnDef>,
        rules: Vec<RuleDef>,
    ) -> Result<(), String> {
        self.conns = conns;
        self.rules = rules;
        self.persist()
    }

    // --- groups and order -------------------------------------------------------------------------

    fn groups_of_kind(&self, kind: GroupKind) -> &Vec<String> {
        match kind {
            GroupKind::Rules => &self.rule_groups,
            GroupKind::Connections => &self.conn_groups,
        }
    }

    fn groups_of_kind_mut(&mut self, kind: GroupKind) -> &mut Vec<String> {
        match kind {
            GroupKind::Rules => &mut self.rule_groups,
            GroupKind::Connections => &mut self.conn_groups,
        }
    }

    pub fn groups_of(&self, kind: GroupKind) -> Vec<String> {
        self.groups_of_kind(kind).clone()
    }

    /// Replace the whole group-name list: create, reorder and delete are all "here is the new
    /// list". Deleting a group strands its members in the implicit default rather than pointing
    /// at nothing.
    pub fn set_groups(
        &mut self,
        kind: GroupKind,
        names: Option<&Vec<Value>>,
    ) -> Result<Vec<String>, String> {
        let Some(names) = names else {
            return Err("groups must be an array".into());
        };
        // [...new Set(...)]: dedupe preserving first occurrence, dropping empty and "default".
        let mut clean: Vec<String> = Vec::new();
        for n in names {
            let s = n.as_str().unwrap_or("").trim();
            if s.is_empty() || s == DEFAULT_GROUP || clean.iter().any(|c| c == s) {
                continue;
            }
            clean.push(s.to_string());
        }
        *self.groups_of_kind_mut(kind) = clean.clone();
        match kind {
            GroupKind::Rules => {
                for row in &mut self.rules {
                    if let Some(g) = &row.group {
                        if !clean.contains(g) {
                            row.group = None;
                        }
                    }
                }
            }
            GroupKind::Connections => {
                for row in &mut self.conns {
                    if let Some(g) = &row.group {
                        if !clean.contains(g) {
                            row.group = None;
                        }
                    }
                }
            }
        }
        self.persist()?;
        Ok(self.groups_of(kind))
    }

    pub fn rename_group(
        &mut self,
        kind: GroupKind,
        from: &str,
        to: &str,
    ) -> Result<(Vec<String>, usize), String> {
        let src = from.trim();
        let dst = to.trim();
        if src.is_empty() || dst.is_empty() {
            return Err("from and to are required".into());
        }
        if dst == DEFAULT_GROUP {
            return Err("the default group cannot be recreated under a new name".into());
        }
        if !self.groups_of_kind(kind).iter().any(|n| n == src) {
            return Err(format!("unknown group: {src}"));
        }
        if src != dst && self.groups_of_kind(kind).iter().any(|n| n == dst) {
            return Err(format!("group already exists: {dst}"));
        }
        let names = self.groups_of_kind_mut(kind);
        for n in names.iter_mut() {
            if n == src {
                *n = dst.to_string();
            }
        }
        let mut moved = 0usize;
        match kind {
            GroupKind::Rules => {
                for row in &mut self.rules {
                    if row.group.as_deref() == Some(src) {
                        row.group = Some(dst.to_string());
                        moved += 1;
                    }
                }
            }
            GroupKind::Connections => {
                for row in &mut self.conns {
                    if row.group.as_deref() == Some(src) {
                        row.group = Some(dst.to_string());
                        moved += 1;
                    }
                }
            }
        }
        self.persist()?;
        Ok((self.groups_of(kind), moved))
    }

    /// Put one row in a group. An empty name (or the reserved default) means the implicit
    /// default. `group: None` from the caller means "not sent", which is also the default.
    pub fn set_group(
        &mut self,
        kind: GroupKind,
        id: &str,
        group: Option<&Value>,
    ) -> Result<String, String> {
        let name = group.and_then(Value::as_str).map(str::trim).unwrap_or("");
        if !name.is_empty()
            && name != DEFAULT_GROUP
            && !self.groups_of_kind(kind).iter().any(|n| name == n.as_str())
        {
            return Err(format!("unknown group: {name}"));
        }
        let label = match kind {
            GroupKind::Rules => "rule",
            GroupKind::Connections => "SSH connection",
        };
        let new_group = if name.is_empty() || name == DEFAULT_GROUP {
            None
        } else {
            Some(name.to_string())
        };
        let changed = match kind {
            GroupKind::Rules => self
                .rules
                .iter_mut()
                .find(|x| x.id == id)
                .map(|row| {
                    row.group = new_group.clone();
                })
                .is_some(),
            GroupKind::Connections => self
                .conns
                .iter_mut()
                .find(|x| x.id == id)
                .map(|row| {
                    row.group = new_group.clone();
                })
                .is_some(),
        };
        if !changed {
            return Err(format!("unknown {label}: {id}"));
        };
        self.persist()?;
        Ok(new_group.unwrap_or_else(|| DEFAULT_GROUP.to_string()))
    }

    /// Impose a full order on one list. Rows the caller omitted keep their relative order after
    /// the mentioned ones, so a stale panel reorder cannot drop or duplicate anything.
    pub fn reorder(&mut self, kind: GroupKind, ids: Option<&Vec<Value>>) -> Result<(), String> {
        let Some(ids) = ids else {
            return Err("ids must be an array".into());
        };
        let order: Vec<String> = ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let rank = |id: &str| -> usize {
            match order.iter().position(|x| x == id) {
                Some(i) => i,
                // Unmentioned rows rank after every mentioned one (order.length in the Node
                // build); the sort below is stable, so they keep their relative order.
                None => order.len(),
            }
        };
        match kind {
            GroupKind::Rules => self.rules.sort_by_key(|r| rank(&r.id)),
            GroupKind::Connections => self.conns.sort_by_key(|c| rank(&c.id)),
        }
        self.persist()
    }

    // --- MCP link maintenance ---------------------------------------------------------------------

    pub fn rename_mcp(&mut self, from: &str, to: &str) -> Result<(), String> {
        let mut touched = false;
        for r in &mut self.rules {
            if let Some(i) = r.mcps.iter().position(|m| m == from) {
                r.mcps[i] = to.to_string();
                touched = true;
            }
        }
        if touched {
            self.persist()?;
        }
        Ok(())
    }

    pub fn forget_mcp(&mut self, name: &str) -> Result<(), String> {
        let mut touched = false;
        for r in &mut self.rules {
            if let Some(i) = r.mcps.iter().position(|m| m == name) {
                r.mcps.remove(i);
                touched = true;
            }
        }
        if touched {
            self.persist()?;
        }
        Ok(())
    }
}

fn nonempty(v: Option<&str>) -> Option<String> {
    v.filter(|s| !s.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    // Store rules are pure data — every case here runs with no network, no SSH, and no master
    // key beyond what the machine already provides (see the persist note in round_trips).
    use super::*;

    fn temp_store(name: &str) -> (PathBuf, TunnelStore) {
        let dir = std::env::temp_dir().join(format!(
            "lmg-tunnel-{}-{}",
            name,
            lmg_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tunnels.json");
        (dir, TunnelStore::new(&path, 19999))
    }

    fn conn_input(name: &str) -> ConnInput {
        ConnInput {
            id: None,
            name: name.into(),
            host: "bastion".into(),
            port: 22.0,
            username: "root".into(),
            auth_type: AuthType::Key,
            key_path: Some("~/.ssh/id_rsa".into()),
            ..Default::default()
        }
    }

    fn rule_input(name: &str, conn_id: &str, local: f64) -> RuleInput {
        RuleInput {
            name: name.into(),
            connection_id: conn_id.into(),
            local_port: local,
            target_host: "127.0.0.1".into(),
            target_port: 5432.0,
            ..Default::default()
        }
    }

    #[test]
    fn validates_connection_errors_exactly_like_node() {
        let (_dir, mut s) = temp_store("conn-err");
        assert_eq!(
            s.add_connection(&ConnInput {
                name: "  ".into(),
                ..conn_input("x")
            })
            .unwrap_err(),
            "name is required"
        );
        assert_eq!(
            s.add_connection(&ConnInput {
                host: "".into(),
                ..conn_input("x")
            })
            .unwrap_err(),
            "host is required"
        );
        let mut bad_port = conn_input("x");
        bad_port.port = 70000.0;
        assert_eq!(
            s.add_connection(&bad_port).unwrap_err(),
            "invalid SSH port: 70000"
        );
        let nan_port = ConnInput {
            port: f64::NAN,
            ..conn_input("x")
        };
        // num(NaN, 22) falls back to 22 in the Node store — a NaN port is NOT an error there.
        assert!(s.add_connection(&nan_port).is_ok());
    }

    #[test]
    fn key_auth_requires_key_path_password_auth_requires_password() {
        let (_dir, mut s) = temp_store("auth-req");
        assert_eq!(
            s.add_connection(&ConnInput {
                key_path: None,
                ..conn_input("k")
            })
            .unwrap_err(),
            "a private key path is required for key authentication"
        );
        let mut pw = conn_input("p");
        pw.auth_type = AuthType::Password;
        assert_eq!(
            s.add_connection(&pw).unwrap_err(),
            "a password is required for password authentication"
        );
        pw.password = Some("secret".into());
        assert!(s.add_connection(&pw).is_ok());
        assert_eq!(
            s.connection(&s.connections()[0].id)
                .unwrap()
                .password
                .as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn rule_validation_rejects_gateway_port_and_clashes() {
        let (_dir, mut s) = temp_store("rule-err");
        let c = s.add_connection(&conn_input("bastion")).unwrap();
        assert_eq!(
            s.add_rule(&rule_input("me", &c.id, 19999.0)).unwrap_err(),
            "local port 19999 is the gateway's own port"
        );
        assert!(s.add_rule(&rule_input("first", &c.id, 5433.0)).is_ok());
        assert_eq!(
            s.add_rule(&rule_input("second", &c.id, 5433.0))
                .unwrap_err(),
            "local port 5433 is already used by 'first'"
        );
        assert_eq!(
            s.add_rule(&rule_input("me", &c.id, f64::NAN)).unwrap_err(),
            "invalid local port: NaN"
        );
        assert_eq!(
            s.add_rule(&rule_input("me", "nope", 1234.0)).unwrap_err(),
            "unknown SSH connection: nope"
        );
    }

    #[test]
    fn update_keeps_enabled_and_mcp_links_and_adopts_host_key() {
        let (_dir, mut s) = temp_store("update");
        let c = s
            .add_connection(&ConnInput {
                host_key: Some("SHA256:aaa".into()),
                ..conn_input("bastion")
            })
            .unwrap();
        let mut r = rule_input("pg", &c.id, 5433.0);
        r.mcps = Some(vec!["pg-main".into()]);
        let id = s.add_rule(&r).unwrap().id;
        s.set_enabled(&id, true).unwrap();

        // An edit with mcps omitted keeps the links; enabled stays as stored.
        let mut edit = rule_input("pg2", &c.id, 5434.0);
        edit.mcps = None;
        let saved = s.update_rule(&id, &edit).unwrap();
        assert_eq!(saved.mcps, vec!["pg-main".to_string()]);
        assert!(saved.enabled);

        // The learned host key survives an edit that keeps host/port.
        let edited = s
            .update_connection(
                &c.id,
                &ConnInput {
                    username: "deploy".into(),
                    ..conn_input("bastion")
                },
            )
            .unwrap();
        assert_eq!(edited.host_key.as_deref(), Some("SHA256:aaa"));

        // A trust reset (clear_host_key) is the only path that drops it.
        s.clear_host_key(&c.id).unwrap();
        assert_eq!(s.connection(&c.id).unwrap().host_key, None);
    }

    #[test]
    fn remove_connection_blocked_while_rules_use_it() {
        let (_dir, mut s) = temp_store("remove-conn");
        let c = s.add_connection(&conn_input("b")).unwrap();
        s.add_rule(&rule_input("pg", &c.id, 5433.0)).unwrap();
        assert_eq!(
            s.remove_connection(&c.id).unwrap_err(),
            "connection is still used by: pg"
        );
    }

    #[test]
    fn groups_rename_reorder_and_default_group_rules() {
        let (_dir, mut s) = temp_store("groups");
        let c = s.add_connection(&conn_input("b")).unwrap();
        s.add_rule(&rule_input("a", &c.id, 5433.0)).unwrap();
        s.add_rule(&rule_input("z", &c.id, 5434.0)).unwrap();

        assert_eq!(
            s.set_groups(GroupKind::Rules, Some(&vec![json!("G1")]))
                .unwrap(),
            vec!["G1"]
        );
        assert_eq!(
            s.set_groups(
                GroupKind::Rules,
                Some(&vec![json!("G1"), json!("G1"), json!("default"), json!("")])
            )
            .unwrap(),
            vec!["G1"],
            "dedupe, and the reserved default never becomes a stored group"
        );
        let (groups, moved) = s.rename_group(GroupKind::Rules, "G1", "G2").unwrap();
        assert_eq!(groups, vec!["G2".to_string()]);
        assert_eq!(moved, 0);
        assert_eq!(
            s.rename_group(GroupKind::Rules, "G2", "default")
                .unwrap_err(),
            "the default group cannot be recreated under a new name"
        );
        assert_eq!(
            s.rename_group(GroupKind::Rules, "nope", "x").unwrap_err(),
            "unknown group: nope"
        );

        let rules = s.rules();
        let a_id = &rules[0].id;
        let z_id = &rules[1].id;
        s.set_group(GroupKind::Rules, a_id, Some(&json!("G2")))
            .unwrap();
        assert_eq!(
            s.set_group(GroupKind::Rules, a_id, None).unwrap(),
            DEFAULT_GROUP,
            "an absent group field means the implicit default"
        );
        s.reorder(GroupKind::Rules, Some(&vec![json!(z_id)]))
            .unwrap();
        assert_eq!(
            s.rules()[0].id,
            *z_id,
            "mentioned rows move up, the rest keep order"
        );
        assert!(s.reorder(GroupKind::Rules, None).is_err());
    }

    #[test]
    fn loads_a_plaintext_file_with_node_defaults() {
        let dir =
            std::env::temp_dir().join(format!("lmg-tunnel-load-{}", lmg_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tunnels.json");
        std::fs::write(
            &path,
            r#"{
  "connections": [
    { "id": "c1", "name": "b", "host": "bastion", "port": 2222, "username": "u", "authType": "key", "keyPath": "~/.ssh/id_rsa" },
    { "id": "c2", "name": "", "host": "x" },
    { "name": "no id", "host": "x", "username": "u" }
  ],
  "rules": [
    { "id": "r1", "name": "pg", "connectionId": "c1", "localPort": "5433", "targetPort": 5432, "reconnectInterval": 0 },
    { "id": "r2", "name": "orphan", "connectionId": "missing", "localPort": 6379 },
    { "id": "r3", "name": "badport", "connectionId": "c1", "localPort": 99999 }
  ],
  "ruleGroups": ["A", "default", ""],
  "connGroups": []
}"#,
        )
        .unwrap();
        let s = TunnelStore::new(&path, 19999);
        assert_eq!(s.connections().len(), 1);
        assert_eq!(s.rules().len(), 2);
        let r1 = &s.rules()[0];
        assert_eq!(
            r1.local_port, 5433,
            "a numeric string localPort loads (JS Number())"
        );
        assert_eq!(r1.target_port, 5432);
        assert_eq!(r1.reconnect_interval, 10, "0 falls back to 10");
        let r2 = &s.rules()[1];
        assert_eq!(
            r2.connection_id, "missing",
            "an unknown connectionId is kept, not dropped"
        );
        assert_eq!(r2.target_port, 6379, "targetPort defaults to localPort");
        assert_eq!(s.groups_of(GroupKind::Rules), vec!["A".to_string()]);
        assert!(!s.is_fresh());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_fresh_when_no_file() {
        let (dir, s) = temp_store("fresh");
        assert!(s.is_fresh());
        assert!(s.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trips_through_the_sealed_state_file() {
        let (dir, mut s) = temp_store("roundtrip");
        let c = s.add_connection(&conn_input("bastion")).unwrap();
        s.add_rule(&rule_input("pg", &c.id, 5433.0)).unwrap();
        // persist() seals; on a machine with no master key at all it fails — the in-memory state
        // is still correct, so only the reload assertion depends on the write having landed.
        let path = dir.join("tunnels.json");
        match TunnelStore::new(&path, 19999) {
            reloaded if !reloaded.is_empty() => {
                assert_eq!(reloaded.connections().len(), 1);
                assert_eq!(reloaded.rules().len(), 1);
                assert_eq!(reloaded.rules()[0].local_port, 5433);
            }
            _ => {}
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mcp_link_maintenance() {
        let (_dir, mut s) = temp_store("mcp-links");
        let c = s.add_connection(&conn_input("b")).unwrap();
        let mut r = rule_input("pg", &c.id, 5433.0);
        r.mcps = Some(vec!["old".into(), "keep".into()]);
        s.add_rule(&r).unwrap();
        s.rename_mcp("old", "new").unwrap();
        assert_eq!(
            s.rules()[0].mcps,
            vec!["new".to_string(), "keep".to_string()]
        );
        s.forget_mcp("keep").unwrap();
        assert_eq!(s.rules()[0].mcps, vec!["new".to_string()]);
    }
}
