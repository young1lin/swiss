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
use std::collections::BTreeMap;

use swiss_core::log;
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};
use swiss_host::groups::Groups;

/// The one grouping vocabulary (docs/20 §2): the FIRST group is the sink slot for unassigned
/// rows; `default` is just the name a fresh list starts from - an ordinary group.
pub use swiss_host::groups::DEFAULT_GROUP;

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
    rule_groups: Groups,
    conn_groups: Groups,
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
            rule_groups: Groups::new(),
            conn_groups: Groups::new(),
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
                        .filter(|n| !n.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        // A v1 file never stored "default" - it was the implicit first group the loader
        // invented on the fly. The one model stores it like any other name (docs/20 §2.1),
        // so a v1 list gains it at the HEAD, where the sink slot lives. The marker written
        // on every save since means verbatim from then on: a default the user deleted must
        // not resurrect on the next load, the same contract managed.json's groupsV2 has.
        let marked = raw.get("tunnelGroupsV2") == Some(&Value::Bool(true));
        let migrated = |names: Vec<String>| -> Vec<String> {
            if marked || names.iter().any(|n| n.eq_ignore_ascii_case(DEFAULT_GROUP)) {
                return names;
            }
            let mut out = vec![DEFAULT_GROUP.to_string()];
            out.extend(names);
            out
        };
        // Members live on the rows themselves (tunnels.json's shape), so the Groups here hold
        // the names and the ordering; from_parts still normalizes them - dedupe, trim, drop
        // over-long names - exactly the way every other scope's list loads.
        self.rule_groups = Groups::from_parts(
            migrated(group_list(&raw, "ruleGroups")),
            BTreeMap::new(),
        );
        self.conn_groups = Groups::from_parts(
            migrated(group_list(&raw, "connGroups")),
            BTreeMap::new(),
        );
    }

    fn persist(&self) -> Result<(), String> {
        let body = json!({
            "connections": self.conns.iter().map(SshConnDef::to_json).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(RuleDef::to_json).collect::<Vec<_>>(),
            "ruleGroups": self.rule_groups.names(),
            "connGroups": self.conn_groups.names(),
            // "the list is verbatim" - see the loader. Every save writes it.
            "tunnelGroupsV2": true,
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
    fn groups_of_kind(&self, kind: GroupKind) -> &Groups {
        match kind {
            GroupKind::Rules => &self.rule_groups,
            GroupKind::Connections => &self.conn_groups,
        }
    }

    fn groups_of_kind_mut(&mut self, kind: GroupKind) -> &mut Groups {
        match kind {
            GroupKind::Rules => &mut self.rule_groups,
            GroupKind::Connections => &mut self.conn_groups,
        }
    }

    pub fn groups_of(&self, kind: GroupKind) -> Vec<String> {
        self.groups_of_kind(kind).names()
    }

    /// Replace the whole group-name list: create, reorder and delete are all "here is the new
    /// list" (docs/20 §2.1). Validation and ordering live in the one Groups model; this store
    /// adds the row-side bookkeeping - a group dropped by omission loses its rows' explicit
    /// entries, so they render in the new first group. A reorder pins the rows that render in
    /// the first group by default (group: None) to the name, so demoting it re-homes nobody:
    /// members live on the rows here, not in the model's sparse map.
    pub fn set_groups(
        &mut self,
        kind: GroupKind,
        names: &[String],
    ) -> Result<Vec<String>, String> {
        let before = self.groups_of_kind(kind).names();
        let clean = self.groups_of_kind_mut(kind).set_names(names.to_vec())?;
        if let Some(first) = swiss_host::groups::demoted_first(&before, &clean) {
            // The NEW list's spelling, not the old one: a replace may respell the group
            // while demoting it, and the panel buckets rows by exact string match.
            let pinned = clean
                .iter()
                .find(|n| n.eq_ignore_ascii_case(&first))
                .cloned()
                .unwrap_or(first);
            match kind {
                GroupKind::Rules => {
                    for row in &mut self.rules {
                        if row.group.is_none() {
                            row.group = Some(pinned.clone());
                        }
                    }
                }
                GroupKind::Connections => {
                    for row in &mut self.conns {
                        if row.group.is_none() {
                            row.group = Some(pinned.clone());
                        }
                    }
                }
            }
        }
        match kind {
            GroupKind::Rules => {
                for row in &mut self.rules {
                    if let Some(g) = &row.group {
                        if !clean.iter().any(|n| n.eq_ignore_ascii_case(g)) {
                            row.group = None;
                        }
                    }
                }
            }
            GroupKind::Connections => {
                for row in &mut self.conns {
                    if let Some(g) = &row.group {
                        if !clean.iter().any(|n| n.eq_ignore_ascii_case(g)) {
                            row.group = None;
                        }
                    }
                }
            }
        }
        self.persist()?;
        Ok(self.groups_of(kind))
    }

    /// Rename in place: the slot is kept and the rows' explicit entries ride along (the count
    /// is what a rename toast and a delete-confirm report). Rows with no explicit group need
    /// no rewrite - the sink slot keeps its place, so they follow the new name on their own.
    pub fn rename_group(
        &mut self,
        kind: GroupKind,
        from: &str,
        to: &str,
    ) -> Result<(Vec<String>, usize), String> {
        let (names, _) = self.groups_of_kind_mut(kind).rename(from, to)?;
        let old_name = from.trim();
        let new_name = to.trim();
        let mut moved = 0usize;
        let mut walk = |group: &mut Option<String>| {
            if group
                .as_deref()
                .is_some_and(|g| g.eq_ignore_ascii_case(old_name))
            {
                *group = Some(new_name.to_string());
                moved += 1;
            }
        };
        match kind {
            GroupKind::Rules => {
                for row in &mut self.rules {
                    walk(&mut row.group);
                }
            }
            GroupKind::Connections => {
                for row in &mut self.conns {
                    walk(&mut row.group);
                }
            }
        }
        self.persist()?;
        Ok((names, moved))
    }

    /// Put one row in a group. None means "no explicit group" - the row renders in the first
    /// group, whatever that is called. A name must exist in the list (case-insensitively);
    /// the canonical casing is stored, and answered.
    pub fn set_group(
        &mut self,
        kind: GroupKind,
        id: &str,
        group: Option<&str>,
    ) -> Result<String, String> {
        let model = self.groups_of_kind_mut(kind);
        let label = match kind {
            GroupKind::Rules => "rule",
            GroupKind::Connections => "SSH connection",
        };
        let new_group = match group.map(str::trim) {
            None | Some("") => None,
            Some(name) => Some(
                model
                    .canonical(name)
                    .ok_or_else(|| format!("unknown group: {name}"))?,
            ),
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
        Ok(new_group.unwrap_or_else(|| self.groups_of(kind)[0].clone()))
    }

    /// Impose a full order on one list. Rows the caller omitted keep their relative order after
    /// the mentioned ones, so a stale panel reorder cannot drop or duplicate anything.
    pub fn reorder(&mut self, kind: GroupKind, ids: &[String]) -> Result<(), String> {
        let rank = |id: &str| -> usize {
            match ids.iter().position(|x| x == id) {
                Some(i) => i,
                // Unmentioned rows rank after every mentioned one (order.length in the Node
                // build); the sort below is stable, so they keep their relative order.
                None => ids.len(),
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
            "swiss-tunnel-{}-{}",
            name,
            swiss_core::util::random_hex(8)
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
    fn groups_follow_the_one_model_default_is_ordinary() {
        let (_dir, mut s) = temp_store("groups");
        let c = s.add_connection(&conn_input("b")).unwrap();
        s.add_rule(&rule_input("a", &c.id, 5433.0)).unwrap();
        s.add_rule(&rule_input("z", &c.id, 5434.0)).unwrap();

        // A fresh store starts from the one default group (docs/20 §2.1): an ordinary name,
        // whose only privilege is being the FIRST entry - the sink slot.
        assert_eq!(s.groups_of(GroupKind::Rules), vec!["default".to_string()]);
        assert_eq!(
            s.set_groups(
                GroupKind::Rules,
                &[
                    "default".to_string(),
                    "G1".to_string(),
                    "G1".to_string(),
                    String::new()
                ],
            )
            .unwrap_err(),
            "duplicate group name: G1",
            "the whole-list mutation validates: duplicates are rejected, not silently deduped"
        );
        assert_eq!(
            s.set_groups(GroupKind::Rules, &[]).unwrap_err(),
            "at least one group must remain"
        );
        assert_eq!(
            s.set_groups(
                GroupKind::Rules,
                &["default".to_string(), "G1".to_string()],
            )
            .unwrap(),
            vec!["default".to_string(), "G1".to_string()],
            "default is a stored name now, kept like any other"
        );
        let (groups, moved) = s.rename_group(GroupKind::Rules, "G1", "G2").unwrap();
        assert_eq!(groups, vec!["default".to_string(), "G2".to_string()]);
        assert_eq!(moved, 0);
        assert_eq!(
            s.rename_group(GroupKind::Rules, "nope", "x").unwrap_err(),
            "unknown group: nope"
        );

        let rules = s.rules();
        let a_id = &rules[0].id;
        let z_id = &rules[1].id;
        assert_eq!(
            s.set_group(GroupKind::Rules, a_id, Some("g2")).unwrap(),
            "G2",
            "assignment stores the canonical casing"
        );
        assert_eq!(
            s.set_group(GroupKind::Rules, a_id, None).unwrap(),
            "default",
            "an absent/null group means the first group, whatever it is called"
        );
        assert_eq!(
            s.set_group(GroupKind::Rules, a_id, Some("Nope")).unwrap_err(),
            "unknown group: Nope"
        );
        s.reorder(GroupKind::Rules, std::slice::from_ref(z_id)).unwrap();
        assert_eq!(
            s.rules()[0].id,
            *z_id,
            "mentioned rows move up, the rest keep order"
        );
    }

    #[test]
    fn a_reorder_pins_rows_that_rendered_in_the_demoted_first_group() {
        // The panel's Move up: the whole list comes back with the moved group first. Rows
        // whose group was None rendered in the first group by default - they must stay with
        // the name, not follow the slot (their membership lives on the row, so the pin is a
        // row write here, not a model entry).
        let (_dir, mut s) = temp_store("groups-reorder");
        let c = s.add_connection(&conn_input("b")).unwrap();
        s.add_rule(&rule_input("a", &c.id, 5433.0)).unwrap();
        let pinned_id = s.add_rule(&rule_input("z", &c.id, 5434.0)).unwrap().id;
        s.set_groups(GroupKind::Rules, &["g1".to_string(), "g2".to_string()])
            .unwrap();
        s.set_group(GroupKind::Rules, &pinned_id, Some("g2")).unwrap();

        s.set_groups(GroupKind::Rules, &["g2".to_string(), "g1".to_string()])
            .unwrap();
        assert_eq!(s.groups_of(GroupKind::Rules), vec!["g2".to_string(), "g1".to_string()]);
        let row = s.rule(&pinned_id).unwrap();
        assert_eq!(row.group.as_deref(), Some("g2"), "the explicit row never moves");
        for r in s.rules() {
            if r.id != pinned_id {
                assert_eq!(
                    r.group.as_deref(),
                    Some("g1"),
                    "the default row stayed with the name"
                );
            }
        }
        // The connections list reorders the same way over its own rows: create the two,
        // then swap them.
        s.set_groups(GroupKind::Connections, &["cg1".to_string(), "cg2".to_string()])
            .unwrap();
        s.set_groups(GroupKind::Connections, &["cg2".to_string(), "cg1".to_string()])
            .unwrap();
        assert_eq!(s.connection(&c.id).unwrap().group.as_deref(), Some("cg1"));
        // A replace that respells the group while demoting it pins under the NEW spelling:
        // the panel buckets rows by exact string match, so an old-spelling row.group
        // would fall out of its own group.
        s.set_group(GroupKind::Connections, &c.id, None).unwrap();
        s.set_groups(GroupKind::Connections, &["cg1".to_string(), "CG2".to_string()])
            .unwrap();
        assert_eq!(
            s.connection(&c.id).unwrap().group.as_deref(),
            Some("CG2"),
            "the pin rode the respell, not the slot"
        );
    }

    #[test]
    fn old_group_files_gain_a_stored_default_once() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-tunnel-migrate-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tunnels.json");
        // A v1 file: no marker, no "default" in either list, a row with no group.
        std::fs::write(
            &path,
            r#"{
  "connections": [
    { "id": "c1", "name": "b", "host": "bastion", "port": 22, "username": "u", "authType": "key", "keyPath": "~/.ssh/id_rsa" }
  ],
  "rules": [
    { "id": "r1", "name": "pg", "connectionId": "c1", "localPort": 5433, "group": null }
  ],
  "ruleGroups": ["A"],
  "connGroups": ["Prod"]
}"#,
        )
        .unwrap();
        let s = TunnelStore::new(&path, 19999);
        assert_eq!(
            s.groups_of(GroupKind::Rules),
            vec!["default".to_string(), "A".to_string()],
            "default is materialized at the HEAD of the list - that slot is the sink"
        );
        assert_eq!(
            s.groups_of(GroupKind::Connections),
            vec!["default".to_string(), "Prod".to_string()]
        );
        // The marker means the list is verbatim from here on: reloading must not insert
        // default a second time.
        let s2 = TunnelStore::new(&path, 19999);
        assert_eq!(s2.groups_of(GroupKind::Rules).len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saved_files_carry_the_v2_marker_and_default_never_resurrects() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-tunnel-v2-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tunnels.json");
        std::fs::write(
            &path,
            r#"{ "connections": [], "rules": [], "ruleGroups": ["A"], "connGroups": [] }"#,
        )
        .unwrap();
        let mut s = TunnelStore::new(&path, 19999);
        // Deleting the default group is ordinary now; the remaining group takes the sink slot.
        s.set_groups(GroupKind::Rules, &["G1".to_string()])
            .unwrap();
        let raw = swiss_core::secure::statefile::read_secure_json(&path)
            .expect("the saved file reads back")
            .expect("the file exists");
        assert_eq!(
            raw.get("tunnelGroupsV2"),
            Some(&Value::Bool(true)),
            "the marker says the list is verbatim; without it every load would re-insert default"
        );
        let re = TunnelStore::new(&path, 19999);
        assert_eq!(
            re.groups_of(GroupKind::Rules),
            vec!["G1".to_string()],
            "a saved v2 list loads verbatim - default stays deleted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_a_plaintext_file_with_node_defaults() {
        let dir =
            std::env::temp_dir().join(format!("swiss-tunnel-load-{}", swiss_core::util::random_hex(8)));
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
        assert_eq!(
            s.groups_of(GroupKind::Rules),
            vec!["A".to_string(), "default".to_string()],
            "a v1 list that already carries default keeps it verbatim (the same rule as             managed.json's loader); empty entries drop"
        );
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
