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

//! The manifest's `tunnels` section (SPEC §host.setup, §tunnels). A manifest keys a connection or a
//! rule by name and the gateway by an id it mints, so a name is looked up on every read, and two
//! sharing one stop that item rather than being guessed between.
//!
//! A real connect pins whatever key answers, the first time. A manifest never takes that step: a
//! host key is written in it or confirmed by a human, so a rule on an unpinned connection is saved
//! and not started.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::stream::{self, StreamExt};
use reqwest::Method;
use serde_json::{json, Map, Value};

use super::apply::{blocked_by, fail};
use super::check::{CHECKS_AT_ONCE, CHECK_BOUND};
use super::manifest::{warn_of_env_refs, Manifest, Warning};
use super::plan::Options;
use super::plugins::{plugin_off_reason, Rows};
use super::refuse::{join, scan_definition, Problem, Scan};
use super::report::{Item, Status};
use crate::remote_cli::Gateway;
use swiss_core::secure::literal;
use swiss_core::secure::refs::references;
use swiss_host::config::is_env_ref;
use swiss_tunnels::tunnel::store::normalize_proxy;

const SECTION_KEYS: [&str; 2] = ["connections", "rules"];
const CONNECTION_KEYS: [&str; 13] = [
    "host", "port", "username", "authType", "keyPath", "passphrase", "password", "hostKey", "group", "proxy",
    "proxyUsername", "proxyPassword", "jump",
];
const RULE_KEYS: [&str; 10] = [
    "connection", "localPort", "targetHost", "targetPort", "remark", "autoReconnect", "reconnectInterval", "mcps",
    "group", "start",
];

/// Where the gateway expands a reference, when it connects. Each of these but the username must be
/// exactly one reference, because anything else reads back as the mask and could not be compared.
const CREDENTIALS: [&str; 3] = ["passphrase", "password", "proxyPassword"];
/// A connection's slots a reference would reach as plain text, and silently not work in.
const NOT_EXPANDED: [&str; 6] = ["host", "username", "keyPath", "group", "proxy", "jump"];
const RULE_NOT_EXPANDED: [&str; 4] = ["connection", "targetHost", "remark", "group"];

/// Compared with a connection's row, absent against absent included: a manifest holds the whole
/// definition, so a passphrase it leaves out is one it does not want.
const CONNECTION_COMPARED: [&str; 10] = [
    "host", "port", "username", "authType", "keyPath", "passphrase", "password", "proxy", "proxyUsername",
    "proxyPassword",
];
/// Compared only when named: a learned host key and a group set on the panel stay.
const CONNECTION_WHEN_NAMED: [&str; 2] = ["hostKey", "group"];
const RULE_COMPARED: [&str; 6] = ["localPort", "targetHost", "targetPort", "remark", "autoReconnect", "reconnectInterval"];
const RULE_WHEN_NAMED: [&str; 2] = ["mcps", "group"];

const NO_REFERENCE_HERE: &str = "the gateway does not expand a reference here, so it would stay this text";
const ONE_REFERENCE: &str = "a credential here is one whole reference, ${secret://name}";
const NAME_RULE: &str = "a name is not empty and has no space around it";
const PORT_RULE: &str = "a port is a whole number from 1 to 65535";
const HOST_KEY_RULE: &str = "a host key is SHA256: and 43 base64 characters, as ssh-keygen -lf prints it; there is no trust-on-first-use field, and a run with checks reports the fingerprint for a human to compare";
const PROXY_RULE: &str = "a proxy is http://host[:port] or socks5://host[:port], with its credentials in proxyUsername and proxyPassword";
const NOT_FOUND: &str = "names no connection in this manifest or on the gateway";
const SHARED: &str = "more than one connection on the gateway has this name, and nothing here guesses between them";
const UNPINNED_HOP: &str = "the connection this rule uses has no host key pinned, and starting it would pin whatever answers; name it in this manifest with hostKey, or start it on the panel";
const RESTART_UNPINNED: &str = "saving this edit would restart a running rule through an unconfirmed host key; pin hostKey first, or stop the affected rule before applying";

/// One connection: what its row should read, and what cannot be written until the gateway says.
#[derive(Debug, Clone)]
pub struct ConnectionEntry {
    pub def: Map<String, Value>,
    /// The connection it dials through, by name.
    pub jump: Option<String>,
    /// The vault names it references.
    pub secrets: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub def: Map<String, Value>,
    /// The connection it runs on, by name.
    pub connection: String,
    /// It should be running.
    pub start: bool,
}

pub fn schema() -> Value {
    json!({
        "identity": "a connection or rule's name; duplicate names on the gateway fail that item with their ids",
        "connections": { "keys": CONNECTION_KEYS, "required": ["host", "username"], "authType": "key (default) or password",
            "key": "keyPath is required; passphrase is optional", "password": "password is required",
            "credentials": ONE_REFERENCE, "port": "22 by default", "hostKey": HOST_KEY_RULE,
            "jump": "the name of another connection, in this manifest or on the gateway", "proxy": PROXY_RULE },
        "rules": { "keys": RULE_KEYS, "required": ["connection", "localPort", "targetPort"],
            "connection": "a connection's name", "targetHost": "127.0.0.1 by default", "start": "false by default; true starts only through pinned host keys, even under --no-check",
            "autoReconnect": "false by default", "reconnectInterval": "10 seconds by default",
            "mcps": "left out, existing links stay; [] clears them" },
        "preserved": "a stored hostKey and group stay when left out; connections and rules otherwise hold the whole definition",
    })
}

/// Read `tunnels`, every problem listed. `gateway_port` is the one port a rule may not listen on.
pub fn read_section(section: Option<&Value>, gateway_port: u16, manifest: &mut Manifest, problems: &mut Vec<Problem>) {
    let Some(section) = section.filter(|section| !section.is_null()) else {
        return;
    };
    let Some(fields) = section.as_object() else {
        problems.push(Problem::new("tunnels", "tunnels holds connections and rules, each a map of names to definitions"));
        return;
    };
    for key in fields.keys().filter(|key| !SECTION_KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join("tunnels", key), key, SECTION_KEYS));
    }
    for (name, entry) in entries("tunnels.connections", fields.get("connections"), problems) {
        read_connection(name, entry, manifest, problems);
    }
    refuse_jump_loops(manifest, problems);
    let mut ports = BTreeMap::new();
    for (name, entry) in entries("tunnels.rules", fields.get("rules"), problems) {
        read_rule(name, entry, gateway_port, &mut ports, manifest, problems);
    }
}

/// What a map of names holds, or nothing with the reason recorded.
pub(super) fn entries<'a>(
    path: &str,
    section: Option<&'a Value>,
    problems: &mut Vec<Problem>,
) -> impl Iterator<Item = (&'a String, &'a Value)> {
    let map = match section.filter(|section| !section.is_null()) {
        None => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => {
            problems.push(Problem::new(path, "a map of names to definitions"));
            None
        }
    };
    map.into_iter().flatten()
}

/// A trimmed string. Absent or empty is None, and something else is a problem unless the literal
/// scan has spoken for this path already.
pub(super) fn text(path: &str, fields: &Map<String, Value>, key: &str, problems: &mut Vec<Problem>) -> Option<String> {
    match fields.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Some(_) => {
            let path = join(path, key);
            if !problems.iter().any(|problem| problem.path == path) {
                problems.push(Problem::new(path, format!("{key} is a string")));
            }
            None
        }
    }
}

fn port(path: &str, fields: &Map<String, Value>, key: &str, problems: &mut Vec<Problem>) -> Option<u64> {
    let value = fields.get(key)?;
    let port = value.as_u64().filter(|port| (1..=65535).contains(port));
    if port.is_none() {
        problems.push(Problem::new(join(path, key), PORT_RULE));
    }
    port
}

fn boolean(path: &str, fields: &Map<String, Value>, key: &str, problems: &mut Vec<Problem>) -> Option<bool> {
    match fields.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => {
            problems.push(Problem::new(join(path, key), format!("{key} is true or false")));
            None
        }
    }
}

fn is_fingerprint(text: &str) -> bool {
    text.strip_prefix("SHA256:").is_some_and(|digest| {
        digest.len() == 43 && digest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    })
}

/// A name in a manifest, compared exactly with what the gateway stores trimmed.
pub(super) fn check_name(path: &str, name: &str, problems: &mut Vec<Problem>) {
    if name.is_empty() || name.trim() != name {
        problems.push(Problem::new(path, NAME_RULE));
    }
}

fn refuse_references_in(path: &str, fields: &Map<String, Value>, slots: &[&str], problems: &mut Vec<Problem>) {
    for slot in slots {
        let found = fields.get(*slot).and_then(Value::as_str).and_then(|value| references(value).ok());
        if found.is_some_and(|(named, _)| !named.is_empty()) {
            problems.push(Problem::new(join(path, slot), NO_REFERENCE_HERE));
        }
    }
}

fn read_connection(name: &str, entry: &Value, manifest: &mut Manifest, problems: &mut Vec<Problem>) {
    let path = join("tunnels.connections", name);
    check_name(&path, name, problems);
    let Some(fields) = entry.as_object() else {
        problems.push(Problem::new(path, "a connection is an object"));
        return;
    };
    for key in fields.keys().filter(|key| !CONNECTION_KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join(&path, key), key, CONNECTION_KEYS));
    }
    // The format below says all there is to say about a host key, and a run of 43 base64 characters
    // is what the shape scans look for.
    let mut body = fields.clone();
    body.remove("hostKey");
    let mut scan = Scan::default();
    scan_definition(&path, &Value::Object(body), &mut scan);
    problems.append(&mut scan.problems);
    warn_of_env_refs(std::mem::take(&mut scan.env_refs), manifest);
    refuse_references_in(&path, fields, &NOT_EXPANDED, problems);

    let mut def = Map::new();
    let mut required = |key: &str, problems: &mut Vec<Problem>| {
        let value = text(&path, fields, key, problems);
        let seen = problems.iter().any(|problem| problem.path == join(&path, key));
        if value.is_none() && !seen {
            problems.push(Problem::new(join(&path, key), format!("a connection needs a {key}")));
        }
        if let Some(value) = value {
            def.insert(key.to_string(), json!(value));
        }
    };
    required("host", problems);
    required("username", problems);
    let port_number = port(&path, fields, "port", problems).unwrap_or(22);
    def.insert("port".into(), json!(port_number));

    let auth = match fields.get("authType") {
        None | Some(Value::Null) => "key",
        Some(Value::String(auth)) if auth == "key" || auth == "password" => auth.as_str(),
        Some(_) => {
            problems.push(Problem::new(join(&path, "authType"), "authType is key or password"));
            "key"
        }
    };
    def.insert("authType".into(), json!(auth));
    read_credentials(&path, fields, auth, &mut def, problems);

    if let Some(host_key) = text(&path, fields, "hostKey", problems) {
        if is_fingerprint(&host_key) {
            def.insert("hostKey".into(), json!(host_key));
        } else {
            problems.push(Problem::new(join(&path, "hostKey"), HOST_KEY_RULE));
        }
    }
    if let Some(group) = text(&path, fields, "group", problems) {
        def.insert("group".into(), json!(group));
    }
    let proxy = text(&path, fields, "proxy", problems);
    if let Some(proxy) = &proxy {
        match normalize_proxy(proxy) {
            Ok(normal) => {
                def.insert("proxy".into(), json!(normal));
            }
            // Its own refusal may quote what was written.
            Err(_) => problems.push(Problem::new(join(&path, "proxy"), PROXY_RULE)),
        }
    }
    if let Some(username) = text(&path, fields, "proxyUsername", problems) {
        def.insert("proxyUsername".into(), json!(username));
    }
    let jump = text(&path, fields, "jump", problems);
    if let Some(jump) = &jump {
        if jump == name {
            problems.push(Problem::new(join(&path, "jump"), "a connection cannot jump through itself"));
        }
        if proxy.is_some() {
            problems.push(Problem::new(
                join(&path, "jump"),
                "a connection has a proxy or a jump, not both; put the proxy on the connection it jumps through",
            ));
        }
    }
    manifest.connections.insert(name.to_string(), ConnectionEntry { def, jump, secrets: scan.secrets });
}

/// One auth type's slots, and the other's refused: the store would drop them without a word.
fn read_credentials(
    path: &str,
    fields: &Map<String, Value>,
    auth: &str,
    def: &mut Map<String, Value>,
    problems: &mut Vec<Problem>,
) {
    let (needed, wrong): (&str, &[&str]) =
        if auth == "password" { ("password", &["keyPath", "passphrase"]) } else { ("keyPath", &["password"]) };
    for key in wrong {
        if fields.contains_key(*key) {
            problems.push(Problem::new(join(path, key), format!("{auth} auth does not read {key}")));
        }
    }
    let seen = |problems: &[Problem], key: &str| problems.iter().any(|problem| problem.path == join(path, key));
    if text(path, fields, needed, problems).is_none() && !seen(problems, needed) {
        problems.push(Problem::new(join(path, needed), format!("{auth} auth needs a {needed}")));
    }
    if let Some(key_path) = text(path, fields, "keyPath", problems).filter(|_| auth == "key") {
        def.insert("keyPath".into(), json!(key_path));
    }
    for key in CREDENTIALS {
        let Some(value) = text(path, fields, key, problems) else {
            continue;
        };
        if !seen(problems, key) && !is_env_ref(&json!(value)) {
            problems.push(Problem::new(join(path, key), ONE_REFERENCE));
        }
        let read_by_this_auth = key == "proxyPassword" || !wrong.contains(&key);
        if read_by_this_auth {
            def.insert(key.to_string(), json!(value));
        }
    }
}

/// A loop among what the manifest names. One through a connection only the gateway holds, the
/// gateway refuses when it is written.
fn refuse_jump_loops(manifest: &Manifest, problems: &mut Vec<Problem>) {
    let mut reported = BTreeSet::new();
    for name in manifest.connections.keys() {
        let mut chain = vec![name.as_str()];
        let mut next = manifest.connections[name].jump.as_deref();
        while let Some(hop) = next {
            if hop == name {
                if chain.len() > 1 && chain.iter().all(|link| !reported.contains(*link)) {
                    reported.insert(name.as_str());
                    chain.push(hop);
                    problems.push(Problem::new(
                        join(&join("tunnels.connections", name), "jump"),
                        format!("these connections jump through each other: {}", chain.join(" -> ")),
                    ));
                }
                break;
            }
            if chain.contains(&hop) {
                break;
            }
            chain.push(hop);
            next = manifest.connections.get(hop).and_then(|entry| entry.jump.as_deref());
        }
    }
}

fn read_rule(
    name: &str,
    entry: &Value,
    gateway_port: u16,
    ports: &mut BTreeMap<u64, String>,
    manifest: &mut Manifest,
    problems: &mut Vec<Problem>,
) {
    let path = join("tunnels.rules", name);
    check_name(&path, name, problems);
    let Some(fields) = entry.as_object() else {
        problems.push(Problem::new(path, "a rule is an object"));
        return;
    };
    for key in fields.keys().filter(|key| !RULE_KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join(&path, key), key, RULE_KEYS));
    }
    let mut scan = Scan::default();
    scan_definition(&path, entry, &mut scan);
    problems.append(&mut scan.problems);
    refuse_references_in(&path, fields, &RULE_NOT_EXPANDED, problems);

    let connection = text(&path, fields, "connection", problems);
    if connection.is_none() && !problems.iter().any(|problem| problem.path == join(&path, "connection")) {
        problems.push(Problem::new(join(&path, "connection"), "a rule names the connection it runs on"));
    }
    let mut def = Map::new();
    let local = port(&path, fields, "localPort", problems);
    let target = port(&path, fields, "targetPort", problems);
    for (key, value) in [("localPort", local), ("targetPort", target)] {
        match value {
            Some(port) => {
                def.insert(key.to_string(), json!(port));
            }
            None if !fields.contains_key(key) => {
                problems.push(Problem::new(join(&path, key), format!("a rule needs a {key}")));
            }
            None => {}
        }
    }
    if let Some(local) = local {
        if local == u64::from(gateway_port) {
            problems.push(Problem::new(join(&path, "localPort"), "this is the gateway's own port"));
        } else if let Some(other) = ports.insert(local, name.to_string()) {
            problems.push(Problem::new(join(&path, "localPort"), format!("rule {other} in this manifest listens on this port too")));
        }
    }
    let target_host = text(&path, fields, "targetHost", problems).unwrap_or_else(|| "127.0.0.1".to_string());
    def.insert("targetHost".into(), json!(target_host));
    let remark = text(&path, fields, "remark", problems).unwrap_or_default();
    def.insert("remark".into(), json!(remark));
    let reconnect = boolean(&path, fields, "autoReconnect", problems).unwrap_or(false);
    def.insert("autoReconnect".into(), json!(reconnect));
    let interval = match fields.get("reconnectInterval") {
        None | Some(Value::Null) => Some(10),
        Some(value) => value.as_u64().filter(|seconds| *seconds > 0),
    };
    match interval {
        Some(seconds) => {
            def.insert("reconnectInterval".into(), json!(seconds));
        }
        None => problems.push(Problem::new(join(&path, "reconnectInterval"), "reconnectInterval is a whole number of seconds above 0")),
    }
    if let Some(mcps) = fields.get("mcps").filter(|mcps| !mcps.is_null()) {
        let names = mcps.as_array().filter(|names| names.iter().all(|name| name.as_str().is_some_and(|name| !name.trim().is_empty())));
        match names {
            Some(names) => {
                def.insert("mcps".into(), json!(names));
            }
            None => problems.push(Problem::new(join(&path, "mcps"), "mcps lists MCPs by name; [] clears them, and left out they stay")),
        }
    }
    if let Some(group) = text(&path, fields, "group", problems) {
        def.insert("group".into(), json!(group));
    }
    let start = boolean(&path, fields, "start", problems).unwrap_or(false);
    if let Some(connection) = connection {
        manifest.rules.insert(name.to_string(), RuleEntry { def, connection, start });
    }
}

/// What the gateway holds of connections and rules. Read only when the manifest names any.
#[derive(Debug, Default, Clone)]
pub struct TunnelState {
    /// The plugin that holds them is not serving, for this reason.
    pub off: Option<String>,
    pub connections: Vec<Value>,
    pub rules: Vec<Value>,
}

impl TunnelState {
    pub(super) fn connections_named(&self, name: &str) -> Vec<&Value> {
        self.connections.iter().filter(|row| row["name"] == json!(name)).collect()
    }

    fn rules_named(&self, name: &str) -> Vec<&Value> {
        self.rules.iter().filter(|row| row["name"] == json!(name)).collect()
    }

    fn connection_by_id(&self, id: &str) -> Option<&Value> {
        self.connections.iter().find(|row| row["id"] == json!(id))
    }

    /// What a jump id on a row is called.
    fn jump_name_of(&self, row: &Value) -> Option<&str> {
        let id = row["jump"].as_str()?;
        self.connection_by_id(id).and_then(|hop| hop["name"].as_str())
    }

    /// A connection's name to its id, where one row holds that name.
    pub(super) fn connection_ids(&self) -> BTreeMap<String, String> {
        ids_of(&self.connections)
    }
}

fn ids_of(rows: &[Value]) -> BTreeMap<String, String> {
    let mut ids = BTreeMap::new();
    let mut shared = BTreeSet::new();
    for row in rows {
        if let (Some(name), Some(id)) = (row["name"].as_str(), row["id"].as_str()) {
            if ids.insert(name.to_string(), id.to_string()).is_some() {
                shared.insert(name.to_string());
            }
        }
    }
    ids.retain(|name, _| !shared.contains(name));
    ids
}

/// Whether the manifest needs the gateway's connections to decide anything.
pub fn names_any(manifest: &Manifest) -> bool {
    !manifest.connections.is_empty() || !manifest.rules.is_empty() || !manifest.targets.is_empty()
}

pub async fn read_state(gw: &Gateway, manifest: &Manifest, rows: &Rows) -> Result<TunnelState, String> {
    if !names_any(manifest) {
        return Ok(TunnelState::default());
    }
    if let Some(reason) = plugin_off_reason(rows, "tunnels") {
        return Ok(TunnelState { off: Some(reason), ..TunnelState::default() });
    }
    read_rows(gw).await
}

pub(super) async fn read_rows(gw: &Gateway) -> Result<TunnelState, String> {
    let answer = gw.get("/api/tunnels").await?;
    let list = |key: &str| answer[key].as_array().cloned().ok_or_else(|| "the gateway reported no tunnels".to_string());
    Ok(TunnelState { off: None, connections: list("connections")?, rules: list("rules")? })
}

fn ids_in(rows: &[&Value]) -> Vec<Value> {
    rows.iter().map(|row| row["id"].clone()).collect()
}

/// Two rows, one name: the gateway allows it, and nothing here can tell them apart.
fn shared_name(kind: &'static str, name: &str, rows: &[&Value]) -> Item {
    let what = if kind == "connection" { "connections" } else { "rules" };
    Item::new(kind, name, Status::Failed)
        .with("code", format!("{kind}.ambiguous"))
        .with("error", format!("{what} on the gateway share this name; rename one on the panel"))
        .with("ids", ids_in(rows))
}

fn item_status(items: &[Item], kind: &str, name: &str) -> Option<Status> {
    items.iter().find(|item| item.kind == kind && item.name == name).map(|item| item.status)
}

fn stops_what_uses_it(status: Option<Status>) -> bool {
    matches!(status, Some(Status::Failed | Status::Blocked))
}

/// Hop first, because a `jump` is written as the id of something that must already exist.
fn in_jump_order(manifest: &Manifest) -> Vec<&String> {
    fn place<'a>(name: &'a String, manifest: &'a Manifest, placed: &mut BTreeSet<&'a String>, order: &mut Vec<&'a String>) {
        if !placed.insert(name) {
            return;
        }
        let hop = manifest.connections[name].jump.as_ref();
        if let Some((hop, _)) = hop.and_then(|hop| manifest.connections.get_key_value(hop)) {
            place(hop, manifest, placed, order);
        }
        order.push(name);
    }
    let (mut placed, mut order) = (BTreeSet::new(), Vec::new());
    for name in manifest.connections.keys() {
        place(name, manifest, &mut placed, &mut order);
    }
    order
}

pub(super) fn plugin_warning(id: &str, warnings: &mut Vec<Warning>) {
    warnings.push(Warning::new(
        off_code(id),
        format!("plugins.{id}"),
        format!("the {id} plugin is off, so what it holds cannot be read here; apply turns it on first and decides each item again"),
    ));
}

pub(super) fn off_code(id: &str) -> &'static str {
    if id == "remote" {
        "remote.plugin-off"
    } else {
        "tunnels.plugin-off"
    }
}

/// Every connection, hop before what goes through it.
pub fn decide_connections(
    manifest: &Manifest,
    state: &TunnelState,
    unsettled: &BTreeSet<String>,
    warnings: &mut Vec<Warning>,
) -> Vec<Item> {
    let mut decided: Vec<Item> = Vec::new();
    let mut planned = state.clone();
    for name in in_jump_order(manifest) {
        let item = decide_connection(name, manifest, &planned, unsettled, &decided, warnings);
        if matches!(item.status, Status::Created | Status::Updated) {
            record_planned_connection(&mut planned, name, &manifest.connections[name]);
        }
        decided.push(item);
    }
    decided
}

/// Later decisions see pins and jumps as they would be after earlier successful writes.
pub(super) fn after_connections(manifest: &Manifest, state: &TunnelState, items: &[Item]) -> TunnelState {
    let mut planned = state.clone();
    for item in items.iter().filter(|item| item.kind == "connection" && matches!(item.status, Status::Created | Status::Updated)) {
        if let Some(entry) = manifest.connections.get(&item.name) {
            record_planned_connection(&mut planned, &item.name, entry);
        }
    }
    planned
}

fn record_planned_connection(state: &mut TunnelState, name: &str, entry: &ConnectionEntry) {
    let previous = state.connections_named(name).first().copied().cloned();
    let mut row = entry.def.clone();
    let id = previous.as_ref().map(|row| row["id"].clone()).unwrap_or_else(|| json!(format!("manifest:{name}")));
    row.insert("id".into(), id);
    row.insert("name".into(), json!(name));
    if let Some(old) = previous.as_ref() {
        let kept = entry.def.get("host") == old.get("host") && entry.def.get("port") == old.get("port");
        for key in ["hostKey", "group"] {
            if !row.contains_key(key) && (key == "group" || kept) {
                if let Some(value) = old.get(key) {
                    row.insert(key.into(), value.clone());
                }
            }
        }
    }
    if let Some(jump) = entry.jump.as_ref().and_then(|name| state.connection_ids().get(name).cloned()) {
        row.insert("jump".into(), json!(jump));
    }
    if let Some(old) = state.connections.iter_mut().find(|row| row["name"] == json!(name)) {
        *old = Value::Object(row);
    } else {
        state.connections.push(Value::Object(row));
    }
}

fn decide_connection(
    name: &str,
    manifest: &Manifest,
    state: &TunnelState,
    unsettled: &BTreeSet<String>,
    decided: &[Item],
    warnings: &mut Vec<Warning>,
) -> Item {
    let entry = &manifest.connections[name];
    if let Some(reason) = state.off.as_deref().filter(|_| !manifest.turns_on("tunnels")) {
        return Item::new("connection", name, Status::Created).failed(off_code("tunnels"), reason);
    }
    let rows = state.connections_named(name);
    if rows.len() > 1 {
        return shared_name("connection", name, &rows);
    }
    let mut by: Vec<Value> =
        entry.secrets.iter().filter(|secret| unsettled.contains(*secret)).map(|secret| json!(format!("secret:{secret}"))).collect();
    if let Some(hop) = &entry.jump {
        if manifest.connections.contains_key(hop) {
            if stops_what_uses_it(item_status(decided, "connection", hop)) {
                by.push(json!(format!("connection:{hop}")));
            }
        } else {
            let found = state.connections_named(hop);
            if found.is_empty() {
                return Item::new("connection", name, Status::Created).failed("connection.jump", NOT_FOUND);
            }
            if found.len() > 1 {
                return Item::new("connection", name, Status::Created).failed("connection.jump", SHARED).with("ids", ids_in(&found));
            }
        }
    }
    if !by.is_empty() {
        return Item::new("connection", name, Status::Blocked).with("by", by);
    }
    let row = rows.first().copied();
    if !entry.def.contains_key("hostKey") && row.is_none_or(|row| row.get("hostKey").is_none()) {
        warnings.push(Warning::new(
            "connection.unpinned",
            format!("tunnels.connections.{name}.hostKey"),
            "no host key is pinned, so a rule on this connection is saved and not started; a run with checks reports the fingerprint for a human to compare",
        ));
    }
    let status = match row {
        None => Status::Created,
        Some(row) if connection_matches(entry, row, state) => Status::Unchanged,
        Some(_) => Status::Updated,
    };
    let item = Item::new("connection", name, status);
    if status == Status::Updated && restart_unpinned(name, entry, state) {
        return item.failed("connection.host-key-required", RESTART_UNPINNED);
    }
    item
}

/// The edit route restarts running dependents itself. Check the state at that write, with just
/// this connection replaced; a pin another item will write later cannot protect that restart.
fn restart_unpinned(name: &str, entry: &ConnectionEntry, state: &TunnelState) -> bool {
    let Some(id) = state.connections_named(name).first().and_then(|row| row["id"].as_str()) else {
        return false;
    };
    state.rules.iter().filter(|rule| active_rule(rule)).any(|rule| {
        let mut seen = BTreeSet::new();
        let mut next = rule["connectionId"].as_str().and_then(|id| state.connection_by_id(id));
        let first = next.and_then(|row| row["name"].as_str());
        while let Some(row) = next {
            let row_id = row["id"].as_str().unwrap_or_default();
            if !seen.insert(row_id) {
                break;
            }
            if row_id == id {
                return first.is_some_and(|first| unpinned_after(first, state, Some((name, entry))));
            }
            next = row["jump"].as_str().and_then(|id| state.connection_by_id(id));
        }
        false
    })
}

/// Keep aligned with TunnelManager::is_active: all three states restart on an edit.
fn active_rule(row: &Value) -> bool {
    matches!(row["state"].as_str(), Some("up" | "starting" | "reconnecting"))
}

fn unpinned_after(name: &str, state: &TunnelState, changed: Option<(&str, &ConnectionEntry)>) -> bool {
    let mut seen = BTreeSet::new();
    let mut next = Some(name);
    while let Some(name) = next {
        if !seen.insert(name) {
            return true;
        }
        let rows = state.connections_named(name);
        let Some(row) = rows.first().filter(|_| rows.len() == 1) else {
            return true;
        };
        if let Some((_, entry)) = changed.filter(|(edited, _)| *edited == name) {
            let kept = entry.def.get("host") == row.get("host") && entry.def.get("port") == row.get("port");
            if !entry.def.contains_key("hostKey") && !(kept && row["hostKey"].is_string()) {
                return true;
            }
            next = entry.jump.as_deref();
        } else {
            if !row["hostKey"].is_string() {
                return true;
            }
            next = state.jump_name_of(row);
            if row["jump"].is_string() && next.is_none() {
                return true;
            }
        }
    }
    false
}

fn connection_matches(entry: &ConnectionEntry, row: &Value, state: &TunnelState) -> bool {
    let same = |key: &&str| entry.def.get(*key) == row.get(*key);
    CONNECTION_COMPARED.iter().all(same)
        && CONNECTION_WHEN_NAMED.iter().all(|key| !entry.def.contains_key(*key) || same(key))
        && state.jump_name_of(row) == entry.jump.as_deref()
}

/// Every rule. `decided` holds the connections.
pub fn decide_rules(manifest: &Manifest, state: &TunnelState, decided: &[Item]) -> Vec<Item> {
    manifest.rules.iter().map(|(name, entry)| decide_rule(name, entry, manifest, state, decided)).collect()
}

fn decide_rule(name: &str, entry: &RuleEntry, manifest: &Manifest, state: &TunnelState, decided: &[Item]) -> Item {
    if let Some(reason) = state.off.as_deref().filter(|_| !manifest.turns_on("tunnels")) {
        return Item::new("rule", name, Status::Created).failed(off_code("tunnels"), reason);
    }
    let rows = state.rules_named(name);
    if rows.len() > 1 {
        return shared_name("rule", name, &rows);
    }
    let found = state.connections_named(&entry.connection);
    if manifest.connections.contains_key(&entry.connection) {
        if stops_what_uses_it(item_status(decided, "connection", &entry.connection)) {
            return Item::new("rule", name, Status::Blocked).with("by", json!([format!("connection:{}", entry.connection)]));
        }
    } else if found.is_empty() {
        return Item::new("rule", name, Status::Created).failed("rule.connection", NOT_FOUND);
    } else if found.len() > 1 {
        return Item::new("rule", name, Status::Created).failed("rule.connection", SHARED).with("ids", ids_in(&found));
    }
    let connection_id = found.first().map(|row| &row["id"]);
    let row = rows.first().copied();
    let status = match row {
        None => Status::Created,
        Some(row) if rule_matches(entry, row, connection_id) => Status::Unchanged,
        Some(_) => Status::Updated,
    };
    let mut item = Item::new("rule", name, status);
    if status == Status::Updated && row.is_some_and(active_rule)
        && unpinned_after(&entry.connection, state, None) {
        return item.failed("rule.host-key-required", RESTART_UNPINNED);
    }
    if entry.start && row.is_none_or(|row| row["state"] != json!("up")) {
        item.note("starts", true);
    }
    item
}

fn rule_matches(entry: &RuleEntry, row: &Value, connection_id: Option<&Value>) -> bool {
    let same = |key: &&str| entry.def.get(*key) == row.get(*key);
    connection_id == row.get("connectionId")
        && RULE_COMPARED.iter().all(same)
        && RULE_WHEN_NAMED.iter().all(|key| !entry.def.contains_key(*key) || same(key))
}

/// Hop before what goes through it, which is the order the items already hold. A hop that failed
/// stops what uses it, and the rest are written.
pub async fn write_connections(gw: &Gateway, manifest: &Manifest, items: &mut [Item], failed_secrets: &BTreeSet<String>) {
    let indices: Vec<usize> = indices_of(items, "connection");
    if indices.is_empty() {
        return;
    }
    for index in indices {
        if matches!(items[index].status, Status::Failed | Status::Blocked) {
            continue;
        }
        let Some(entry) = manifest.connections.get(&items[index].name) else {
            continue;
        };
        let mut by = blocked_by(&entry.secrets, failed_secrets);
        if let Some(hop) = entry.jump.as_ref().filter(|hop| manifest.connections.contains_key(*hop)) {
            if stops_what_uses_it(item_status(items, "connection", hop)) {
                by.push(json!(format!("connection:{hop}")));
            }
        }
        if by.is_empty() {
            if !matches!(items[index].status, Status::Created | Status::Updated) {
                continue;
            }
            // A preceding write can remove a pin or change a jump. Read at each write so an
            // implicit restart never relies on the snapshot taken before that preceding edit.
            let state = match read_rows(gw).await {
                Ok(state) => state,
                Err(error) => {
                    fail(&mut items[index], "connection.write", literal::mask(&error));
                    continue;
                }
            };
            let fresh = decide_connection(&items[index].name, manifest, &state, failed_secrets, items, &mut Vec::new());
            items[index] = fresh;
            write_connection(gw, entry, &mut items[index], &mut state.connection_ids()).await;
        } else {
            items[index].status = Status::Blocked;
            items[index].note("by", by);
        }
    }
}

async fn write_connection(gw: &Gateway, entry: &ConnectionEntry, item: &mut Item, ids: &mut BTreeMap<String, String>) {
    if !matches!(item.status, Status::Created | Status::Updated) {
        return;
    }
    let mut body = entry.def.clone();
    body.insert("name".into(), json!(item.name));
    if let Some(hop) = &entry.jump {
        let Some(id) = ids.get(hop) else {
            return fail(item, "connection.write", "the connection it jumps through has no id on the gateway".to_string());
        };
        body.insert("jump".into(), json!(id));
    }
    let (method, path) = if item.status == Status::Created {
        (Method::POST, "/api/tunnels/connections".to_string())
    } else {
        let Some(id) = ids.get(&item.name) else {
            return fail(item, "connection.write", "this connection is no longer on the gateway".to_string());
        };
        (Method::PUT, format!("/api/tunnels/connections/{id}"))
    };
    match gw.send(method, &path, Some(Value::Object(body))).await {
        Ok(answer) => {
            if let Some(id) = answer["connection"]["id"].as_str() {
                ids.insert(item.name.clone(), id.to_string());
            }
        }
        Err(error) => fail(item, "connection.write", literal::mask(&error)),
    }
}

fn indices_of(items: &[Item], kind: &str) -> Vec<usize> {
    items.iter().enumerate().filter(|(_, item)| item.kind == kind).map(|(index, _)| index).collect()
}

/// Saved without starting: a start is `start: true`'s to ask for, after every check.
pub async fn write_rules(gw: &Gateway, manifest: &Manifest, items: &mut [Item]) {
    let indices = indices_of(items, "rule");
    if indices.is_empty() {
        return;
    }
    let state = match read_rows(gw).await {
        Ok(state) => state,
        Err(error) => {
            for index in indices {
                if matches!(items[index].status, Status::Created | Status::Updated) {
                    fail(&mut items[index], "rule.write", literal::mask(&error));
                }
            }
            return;
        }
    };
    let (connection_ids, rule_ids) = (state.connection_ids(), ids_of(&state.rules));
    for index in indices {
        if matches!(items[index].status, Status::Failed | Status::Blocked) {
            continue;
        }
        let Some(entry) = manifest.rules.get(&items[index].name) else {
            continue;
        };
        if items[index].status == Status::Updated
            && state.rules_named(&items[index].name).first().is_some_and(|row| active_rule(row))
            && unpinned_after(&entry.connection, &state, None) {
            fail(&mut items[index], "rule.host-key-required", RESTART_UNPINNED.to_string());
            continue;
        }
        if stops_what_uses_it(item_status(items, "connection", &entry.connection)) {
            items[index].status = Status::Blocked;
            items[index].extra.remove("starts");
            items[index].note("by", json!([format!("connection:{}", entry.connection)]));
            continue;
        }
        write_rule(gw, entry, &mut items[index], &connection_ids, &rule_ids).await;
    }
}

async fn write_rule(
    gw: &Gateway,
    entry: &RuleEntry,
    item: &mut Item,
    connection_ids: &BTreeMap<String, String>,
    rule_ids: &BTreeMap<String, String>,
) {
    if !matches!(item.status, Status::Created | Status::Updated) {
        return;
    }
    let Some(connection_id) = connection_ids.get(&entry.connection) else {
        return fail(item, "rule.write", "the connection it runs on has no id on the gateway".to_string());
    };
    let mut body = entry.def.clone();
    body.insert("name".into(), json!(item.name));
    body.insert("connectionId".into(), json!(connection_id));
    let (method, path) = if item.status == Status::Created {
        (Method::POST, "/api/tunnels/rules".to_string())
    } else {
        let Some(id) = rule_ids.get(&item.name) else {
            return fail(item, "rule.write", "this rule is no longer on the gateway".to_string());
        };
        (Method::PUT, format!("/api/tunnels/rules/{id}"))
    };
    if let Err(error) = gw.send(method, &path, Some(Value::Object(body))).await {
        fail(item, "rule.write", literal::mask(&error));
    }
}

fn failed_check(item: &mut Item, code: &str, error: String) {
    item.note("check", json!({ "ok": false, "code": code, "error": error }));
}

/// Each connection tested, then each rule that should be running started. `--no-check` skips the
/// tests and still starts what it can: `start: true` is what the manifest asked for, not a check.
pub async fn check(gw: &Gateway, manifest: &Manifest, options: Options, items: &mut [Item], warnings: &mut Vec<Warning>) {
    let (connections, rules) = (indices_of(items, "connection"), indices_of(items, "rule"));
    if connections.is_empty() && rules.is_empty() {
        return;
    }
    let state = match read_rows(gw).await {
        Ok(state) => state,
        Err(error) => {
            for index in connections.into_iter().chain(rules) {
                if !matches!(items[index].status, Status::Failed | Status::Blocked) {
                    failed_check(&mut items[index], "check.read", literal::mask(&error));
                }
            }
            return;
        }
    };
    if !options.no_check {
        test_connections(gw, &state, &connections, items, warnings).await;
    }
    for index in rules {
        let Some(entry) = manifest.rules.get(&items[index].name).filter(|entry| entry.start) else {
            continue;
        };
        if !matches!(items[index].status, Status::Failed | Status::Blocked) {
            start_rule(gw, manifest, entry, index, &state, items).await;
        }
        items[index].extra.remove("starts");
    }
}

async fn test_connections(
    gw: &Gateway,
    state: &TunnelState,
    connections: &[usize],
    items: &mut [Item],
    warnings: &mut Vec<Warning>,
) {
    let ids = state.connection_ids();
    let mut jobs = Vec::new();
    for index in connections.iter().copied() {
        let item = &mut items[index];
        if matches!(item.status, Status::Failed | Status::Blocked) {
            continue;
        }
        match ids.get(&item.name) {
            Some(id) => jobs.push((index, id.clone())),
            None => failed_check(item, "check.connection", "this connection is not on the gateway".to_string()),
        }
    }
    let outcomes: Vec<_> = stream::iter(jobs)
        .map(|(index, id)| async move {
            let path = format!("/api/tunnels/connections/{id}/test");
            let answer = tokio::time::timeout(CHECK_BOUND, gw.send(Method::POST, &path, None)).await;
            (index, answer)
        })
        .buffer_unordered(CHECKS_AT_ONCE)
        .collect()
        .await;
    for (index, answer) in outcomes {
        let name = items[index].name.clone();
        warnings.retain(|warning| warning.code != "connection.unpinned" || warning.path != format!("tunnels.connections.{name}.hostKey"));
        let pinned = state.connections_named(&name).first().is_some_and(|row| row.get("hostKey").is_some());
        match answer {
            Err(_) => failed_check(&mut items[index], "check.timeout", format!("no answer after {} s", CHECK_BOUND.as_secs())),
            Ok(Err(error)) => failed_check(&mut items[index], "check.connection", literal::mask(&error)),
            Ok(Ok(answer)) => read_test(&mut items[index], pinned, &answer),
        }
    }
}

/// The test pins nothing, whatever it answers. An unpinned connection that answered is saved and
/// waits for someone to compare the key it showed.
fn read_test(item: &mut Item, pinned: bool, answer: &Value) {
    let fingerprint = answer["fingerprint"].as_str().map(str::to_string);
    if answer["ok"] != json!(true) {
        let code = if answer["kind"] == json!("hostkey") { "check.host-key" } else { "check.connection" };
        failed_check(item, code, literal::mask(answer["error"].as_str().unwrap_or("no detail")));
        if let Some(fingerprint) = fingerprint {
            item.note("fingerprint", fingerprint);
        }
        return;
    }
    if pinned || answer["hostKey"].is_string() {
        let detail = format!("connected in {} ms", answer["ms"].as_u64().unwrap_or_default());
        item.note("check", json!({ "ok": true, "detail": detail }));
        return;
    }
    let saved = matches!(item.status, Status::Created | Status::Updated);
    item.status = Status::NeedsInput;
    item.note("code", "connection.host-key");
    if let Some(fingerprint) = fingerprint {
        item.note("fingerprint", fingerprint);
    }
    if saved {
        item.note("saved", true);
    }
}

async fn start_rule(gw: &Gateway, manifest: &Manifest, entry: &RuleEntry, index: usize, state: &TunnelState, items: &mut [Item]) {
    let name = items[index].name.clone();
    let row = state.rules_named(&name).first().copied();
    let Some(row) = row else {
        return failed_check(&mut items[index], "check.rule", "this rule is not on the gateway".to_string());
    };
    if row["state"] == json!("up") && items[index].status == Status::Unchanged {
        items[index].note("check", json!({ "ok": true, "detail": "up" }));
        return;
    }
    if let Some(holding) = withheld_by(entry, manifest, state, items) {
        let saved = matches!(items[index].status, Status::Created | Status::Updated);
        let item = &mut items[index];
        item.status = Status::Blocked;
        item.note("by", json!([format!("connection:{holding}")]));
        if saved {
            item.note("saved", true);
        }
        if !manifest.connections.contains_key(&holding) {
            item.note("why", UNPINNED_HOP);
        }
        return;
    }
    let Some(id) = row["id"].as_str() else {
        return failed_check(&mut items[index], "check.rule", "this rule has no id on the gateway".to_string());
    };
    let path = format!("/api/tunnels/rules/{id}/start");
    let answer = tokio::time::timeout(CHECK_BOUND, gw.send(Method::POST, &path, None)).await;
    let item = &mut items[index];
    match answer {
        Err(_) => failed_check(item, "check.timeout", format!("no answer after {} s", CHECK_BOUND.as_secs())),
        Ok(Err(error)) => failed_check(item, "check.rule", literal::mask(&error)),
        Ok(Ok(answer)) if answer["rule"]["state"] == json!("up") => {
            item.note("check", json!({ "ok": true, "detail": "up" }));
        }
        Ok(Ok(answer)) => {
            let said = answer["error"].as_str().or_else(|| answer["rule"]["reason"].as_str()).unwrap_or("it did not come up");
            failed_check(item, "check.rule", literal::mask(said));
        }
    }
}

/// The connection that keeps a rule from starting: its item did not pass, or it or a hop on the way
/// has no key pinned, and a real connect would pin whatever answers.
fn withheld_by(entry: &RuleEntry, manifest: &Manifest, state: &TunnelState, items: &[Item]) -> Option<String> {
    if manifest.connections.contains_key(&entry.connection) {
        let item = items.iter().find(|item| item.kind == "connection" && item.name == entry.connection)?;
        if matches!(item.status, Status::Failed | Status::Blocked | Status::NeedsInput) || item.check_failed() {
            return Some(entry.connection.clone());
        }
    }
    let mut seen = BTreeSet::new();
    let mut next = state.connections_named(&entry.connection).first().copied();
    while let Some(row) = next {
        let name = row["name"].as_str().unwrap_or_default();
        if !seen.insert(name.to_string()) {
            break;
        }
        if row.get("hostKey").is_none() {
            return Some(name.to_string());
        }
        next = row["jump"].as_str().and_then(|id| state.connection_by_id(id));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU";
    const VALUE: &str = "S3ntinelValue9";

    fn parsed(section: Value) -> Result<Manifest, Vec<(String, String)>> {
        let mut manifest = Manifest::default();
        let mut problems = Vec::new();
        read_section(Some(&section), 19999, &mut manifest, &mut problems);
        if problems.is_empty() {
            Ok(manifest)
        } else {
            Err(problems.into_iter().map(|p| (p.path, p.message)).collect())
        }
    }

    fn paths_of(section: Value) -> Vec<String> {
        parsed(section).expect_err("a refusal").into_iter().map(|(path, _)| path).collect()
    }

    fn connection(extra: Value) -> Value {
        let mut base = json!({ "host": "bastion.example.com", "username": "deploy", "keyPath": "~/.ssh/id_ed25519" });
        for (key, value) in extra.as_object().expect("an object") {
            base[key] = value.clone();
        }
        base
    }

    fn manifest_of(connection_def: Value) -> Manifest {
        parsed(json!({ "connections": { "a": connection_def } })).expect("it reads")
    }

    #[test]
    fn a_connection_is_read_with_its_defaults() {
        let manifest = manifest_of(connection(json!({ "passphrase": "${secret://deploy-pass}", "hostKey": FINGERPRINT, "proxy": "socks5://127.0.0.1" })));
        let entry = &manifest.connections["a"];
        assert_eq!(entry.def["port"], json!(22));
        assert_eq!(entry.def["authType"], json!("key"));
        assert_eq!(entry.def["hostKey"], json!(FINGERPRINT));
        assert_eq!(entry.def["proxy"], json!("socks5://127.0.0.1:1080"), "as the store would keep it");
        assert!(entry.secrets.contains("deploy-pass"));
    }

    // SPEC §host.setup: a host key is pinned or confirmed by a human, so a shape that is not a
    // fingerprint is refused, and the refusal says there is no other way.
    #[test]
    fn a_host_key_is_a_real_fingerprint() {
        let problems = parsed(json!({ "connections": { "a": connection(json!({ "hostKey": "tofu" })) } })).expect_err("refused");
        assert_eq!(problems[0].0, "tunnels.connections.a.hostKey");
        assert!(problems[0].1.contains("no trust-on-first-use field"), "{problems:?}");
        for bad in ["SHA256:short", "MD5:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU", "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOq="] {
            assert!(!is_fingerprint(bad), "{bad}");
        }
        assert!(is_fingerprint(FINGERPRINT));
    }

    // SPEC §host.setup: a reference only where the gateway expands one, and a literal nowhere.
    #[test]
    fn credentials_are_references_and_only_where_they_are_expanded() {
        let problems = parsed(json!({ "connections": {
            "literal": connection(json!({ "passphrase": VALUE })),
            "around": connection(json!({ "passphrase": "x${secret://a}" })),
            "digits": connection(json!({ "passphrase": 123456 })),
            "host-ref": connection(json!({ "host": "${secret://a}" })),
            "path-ref": connection(json!({ "keyPath": "${secret://a}" })),
            "proxy-pass": connection(json!({ "proxyPassword": VALUE })),
        } }))
        .expect_err("refused");
        assert!(!format!("{problems:?}").contains(VALUE), "{problems:?}");
        let paths: Vec<&str> = problems.iter().map(|(path, _)| path.as_str()).collect();
        for path in [
            "tunnels.connections.literal.passphrase",
            "tunnels.connections.around.passphrase",
            "tunnels.connections.digits.passphrase",
            "tunnels.connections.host-ref.host",
            "tunnels.connections.path-ref.keyPath",
            "tunnels.connections.proxy-pass.proxyPassword",
        ] {
            assert_eq!(paths.iter().filter(|seen| **seen == path).count(), 1, "{path} in {paths:?}");
        }
        assert!(parsed(json!({ "connections": { "a": connection(json!({ "proxyUsername": "${secret://u}", "proxyPassword": "${secret://p}", "proxy": "http://127.0.0.1:1" })) } })).is_ok());
    }

    #[test]
    fn each_auth_type_needs_its_own_slots_and_refuses_the_other() {
        let paths = paths_of(json!({ "connections": {
            "no-path": { "host": "h", "username": "u" },
            "key-with-password": connection(json!({ "password": "${secret://p}" })),
            "no-password": { "host": "h", "username": "u", "authType": "password" },
            "password-with-path": { "host": "h", "username": "u", "authType": "password", "password": "${secret://p}", "keyPath": "/k" },
            "other": connection(json!({ "authType": "agent" })),
            "bare": { "keyPath": "/k" },
        } }));
        for path in [
            "tunnels.connections.no-path.keyPath",
            "tunnels.connections.key-with-password.password",
            "tunnels.connections.no-password.password",
            "tunnels.connections.password-with-path.keyPath",
            "tunnels.connections.other.authType",
            "tunnels.connections.bare.host",
            "tunnels.connections.bare.username",
        ] {
            assert!(paths.iter().any(|seen| seen == path), "{path} in {paths:?}");
        }
    }

    #[test]
    fn a_jump_goes_somewhere_else_and_never_in_a_circle() {
        let paths = paths_of(json!({ "connections": {
            "self": connection(json!({ "jump": "self" })),
            "both": connection(json!({ "jump": "elsewhere", "proxy": "http://127.0.0.1:1" })),
            "bad-proxy": connection(json!({ "proxy": "ftp://user:pw@x" })),
            "a": connection(json!({ "jump": "b" })),
            "b": connection(json!({ "jump": "a" })),
        } }));
        for path in [
            "tunnels.connections.self.jump",
            "tunnels.connections.both.jump",
            "tunnels.connections.bad-proxy.proxy",
            "tunnels.connections.a.jump",
        ] {
            assert!(paths.iter().any(|seen| seen == path), "{path} in {paths:?}");
        }
        assert_eq!(paths.iter().filter(|seen| seen.ends_with(".a.jump") || seen.ends_with(".b.jump")).count(), 1, "one loop, one problem: {paths:?}");
        // A proxy refusal quotes nothing of what was written.
        let problems = parsed(json!({ "connections": { "p": connection(json!({ "proxy": "ftp://user:hunter2@x" })) } })).expect_err("refused");
        assert!(!format!("{problems:?}").contains("hunter2"), "{problems:?}");
    }

    #[test]
    fn a_rule_is_read_with_its_defaults_and_its_refusals() {
        let manifest = parsed(json!({ "rules": { "db": { "connection": "a", "localPort": 13306, "targetPort": 3306, "start": true } } })).expect("reads");
        let entry = &manifest.rules["db"];
        assert!(entry.start);
        assert_eq!(entry.def["targetHost"], json!("127.0.0.1"));
        assert_eq!(entry.def["reconnectInterval"], json!(10));
        assert_eq!(entry.def["autoReconnect"], json!(false));
        assert!(entry.def.get("mcps").is_none(), "left out, the stored links stay");

        let paths = paths_of(json!({ "rules": {
            "own-port": { "connection": "a", "localPort": 19999, "targetPort": 1 },
            "one": { "connection": "a", "localPort": 2000, "targetPort": 70000 },
            "two": { "connection": "a", "localPort": 2000, "targetPort": 1 },
            "typed": { "connection": "a", "localPort": 2001, "targetPort": 1, "start": "yes", "autoReconnect": 1, "reconnectInterval": 0, "mcps": [""] },
            "bare": {},
        } }));
        for path in [
            "tunnels.rules.own-port.localPort",
            "tunnels.rules.one.targetPort",
            "tunnels.rules.two.localPort",
            "tunnels.rules.typed.start",
            "tunnels.rules.typed.autoReconnect",
            "tunnels.rules.typed.reconnectInterval",
            "tunnels.rules.typed.mcps",
            "tunnels.rules.bare.connection",
            "tunnels.rules.bare.localPort",
            "tunnels.rules.bare.targetPort",
        ] {
            assert!(paths.iter().any(|seen| seen == path), "{path} in {paths:?}");
        }
    }

    #[test]
    fn hop_comes_before_what_goes_through_it() {
        let manifest = parsed(json!({ "connections": {
            "a": connection(json!({ "jump": "b" })),
            "b": connection(json!({ "jump": "c" })),
            "c": connection(json!({})),
        } }))
        .expect("reads");
        let order: Vec<&str> = in_jump_order(&manifest).into_iter().map(String::as_str).collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    fn row(name: &str, id: &str) -> Value {
        json!({ "id": id, "name": name, "host": "bastion.example.com", "port": 22, "username": "deploy",
                "authType": "key", "keyPath": "~/.ssh/id_ed25519", "state": "idle" })
    }

    fn state_with(connections: Vec<Value>, rules: Vec<Value>) -> TunnelState {
        TunnelState { off: None, connections, rules }
    }

    // SPEC §host.setup: "Two connections or rules sharing a name fail that item, naming the ids".
    #[test]
    fn a_shared_name_stops_that_item_and_blocks_what_uses_it() {
        let manifest = parsed(json!({
            "connections": { "a": connection(json!({})) },
            "rules": { "r": { "connection": "a", "localPort": 2000, "targetPort": 1 } },
        }))
        .expect("reads");
        let state = state_with(vec![row("a", "c1"), row("a", "c2")], vec![]);
        let mut warnings = Vec::new();
        let connections = decide_connections(&manifest, &state, &BTreeSet::new(), &mut warnings);
        assert_eq!(connections[0].status, Status::Failed);
        assert_eq!(connections[0].extra["code"], json!("connection.ambiguous"));
        assert_eq!(connections[0].extra["ids"], json!(["c1", "c2"]));
        let rules = decide_rules(&manifest, &state, &connections);
        assert_eq!(rules[0].status, Status::Blocked);
        assert_eq!(rules[0].extra["by"], json!(["connection:a"]));
    }

    // SPEC §host.setup: what the panel set and the manifest does not name stays, and a passphrase the
    // manifest left out is one it does not want.
    #[test]
    fn a_connection_compares_the_whole_definition_with_its_row() {
        let compare = |extra: Value, row: Value| {
            let manifest = manifest_of(connection(extra));
            let state = state_with(vec![row], vec![]);
            let items = decide_connections(&manifest, &state, &BTreeSet::new(), &mut Vec::new());
            items[0].status
        };
        assert_eq!(compare(json!({}), row("a", "c1")), Status::Unchanged);
        let mut learned = row("a", "c1");
        learned["hostKey"] = json!(FINGERPRINT);
        learned["group"] = json!("prod");
        assert_eq!(compare(json!({}), learned.clone()), Status::Unchanged, "a learned key and a group stay");
        assert_eq!(compare(json!({ "hostKey": FINGERPRINT }), learned.clone()), Status::Unchanged);
        assert_eq!(compare(json!({ "group": "dev" }), learned.clone()), Status::Updated);
        let mut with_pass = row("a", "c1");
        with_pass["passphrase"] = json!("${secret://p}");
        assert_eq!(compare(json!({}), with_pass.clone()), Status::Updated, "left out, it goes");
        assert_eq!(compare(json!({ "passphrase": "${secret://p}" }), with_pass), Status::Unchanged);
        let mut typed = row("a", "c1");
        typed["passphrase"] = json!(swiss_host::mask::MASK);
        assert_eq!(compare(json!({ "passphrase": "${secret://p}" }), typed), Status::Updated, "the reference replaces what the panel typed");
    }

    #[test]
    fn a_hop_on_the_gateway_only_must_be_one_connection() {
        let manifest = manifest_of(connection(json!({ "jump": "hop" })));
        let decide = |connections| {
            decide_connections(&manifest, &state_with(connections, vec![]), &BTreeSet::new(), &mut Vec::new()).remove(0)
        };
        let none = decide(vec![]);
        assert_eq!(none.extra["code"], json!("connection.jump"));
        let two = decide(vec![row("hop", "h1"), row("hop", "h2")]);
        assert_eq!(two.extra["ids"], json!(["h1", "h2"]));
        assert_eq!(decide(vec![row("hop", "h1")]).status, Status::Created);
        let mut stored = row("a", "c1");
        stored["jump"] = json!("h1");
        assert_eq!(decide(vec![row("hop", "h1"), stored]).status, Status::Unchanged);
    }

    #[test]
    fn an_unpinned_connection_warns_and_a_pinned_one_does_not() {
        let warned = |extra: Value, rows: Vec<Value>| {
            let manifest = manifest_of(connection(extra));
            let mut warnings = Vec::new();
            decide_connections(&manifest, &state_with(rows, vec![]), &BTreeSet::new(), &mut warnings);
            warnings.iter().map(|warning| warning.path.clone()).collect::<Vec<_>>()
        };
        assert_eq!(warned(json!({}), vec![]), ["tunnels.connections.a.hostKey"]);
        assert!(warned(json!({ "hostKey": FINGERPRINT }), vec![]).is_empty());
        let mut learned = row("a", "c1");
        learned["hostKey"] = json!(FINGERPRINT);
        assert!(warned(json!({}), vec![learned]).is_empty());
    }

    #[test]
    fn a_test_answer_says_what_the_item_becomes() {
        let run = |status: Status, pinned: bool, answer: Value| {
            let mut item = Item::new("connection", "a", status);
            read_test(&mut item, pinned, &answer);
            item
        };
        let good = run(Status::Unchanged, true, json!({ "ok": true, "ms": 12 }));
        assert_eq!(good.status, Status::Unchanged);
        assert_eq!(good.extra["check"], json!({ "ok": true, "detail": "connected in 12 ms" }));

        let shown = run(Status::Created, false, json!({ "ok": true, "ms": 12, "fingerprint": FINGERPRINT }));
        assert_eq!(shown.status, Status::NeedsInput);
        assert_eq!(shown.extra["code"], json!("connection.host-key"));
        assert_eq!(shown.extra["fingerprint"], json!(FINGERPRINT));
        assert_eq!(shown.extra["saved"], json!(true));
        assert!(run(Status::Unchanged, false, json!({ "ok": true, "fingerprint": FINGERPRINT })).extra.get("saved").is_none());

        let wrong = run(Status::Unchanged, true, json!({ "ok": false, "kind": "hostkey", "error": "it changed", "fingerprint": FINGERPRINT }));
        assert_eq!(wrong.status, Status::Unchanged, "the definition stays saved");
        assert_eq!(wrong.extra["check"]["code"], json!("check.host-key"));
        assert_eq!(wrong.extra["fingerprint"], json!(FINGERPRINT));

        let down = run(Status::Updated, false, json!({ "ok": false, "kind": "network", "error": "refused" }));
        assert_eq!(down.extra["check"]["code"], json!("check.connection"));
    }

    // SPEC §host.setup: no start pins by trust on first use, through a hop included.
    #[test]
    fn a_rule_is_held_while_a_key_on_the_way_is_unpinned() {
        let manifest = parsed(json!({ "rules": { "r": { "connection": "far", "localPort": 2000, "targetPort": 1, "start": true } } })).expect("reads");
        let entry = &manifest.rules["r"];
        let mut far = row("far", "c1");
        far["jump"] = json!("c2");
        far["hostKey"] = json!(FINGERPRINT);
        let mut hop = row("hop", "c2");
        assert_eq!(withheld_by(entry, &manifest, &state_with(vec![far.clone(), hop.clone()], vec![]), &[]), Some("hop".to_string()));
        hop["hostKey"] = json!(FINGERPRINT);
        assert_eq!(withheld_by(entry, &manifest, &state_with(vec![far.clone(), hop], vec![]), &[]), None);
        far.as_object_mut().expect("a row").remove("hostKey");
        assert_eq!(withheld_by(entry, &manifest, &state_with(vec![far], vec![]), &[]), Some("far".to_string()));
    }

    // Updates restart active rules inside the manager, before the explicit checks. The plan
    // must refuse an edit that would let that implicit restart learn an unconfirmed key.
    #[test]
    fn an_edit_cannot_implicitly_restart_through_an_unpinned_key() {
        let manifest = parsed(json!({
            "connections": { "a": connection(json!({ "host": "new.example.com" })) },
            "rules": { "r": { "connection": "a", "localPort": 2000, "targetPort": 1 } },
        })).expect("reads");
        for active in ["up", "starting", "reconnecting"] {
            let mut stored = row("a", "c1");
            stored["hostKey"] = json!(FINGERPRINT);
            let running = json!({ "id": "r1", "name": "r", "connectionId": "c1", "state": active });
            let state = state_with(vec![stored], vec![running]);
            let connections = decide_connections(&manifest, &state, &BTreeSet::new(), &mut Vec::new());
            assert_eq!(connections[0].status, Status::Failed, "{active}");
            assert_eq!(connections[0].extra["code"], json!("connection.host-key-required"));
            let mut unpinned = state.connections[0].clone();
            unpinned.as_object_mut().expect("a row").remove("hostKey");
            let state = state_with(vec![unpinned], state.rules);
            let rules = decide_rules(&manifest, &state, &[]);
            assert_eq!(rules[0].status, Status::Failed, "{active}");
            assert_eq!(rules[0].extra["code"], json!("rule.host-key-required"));
        }
    }

    #[test]
    fn a_later_edit_sees_a_pin_removed_by_an_earlier_edit() {
        let manifest = parsed(json!({ "connections": {
            "a": connection(json!({ "host": "new.example.com" })),
            "b": connection(json!({ "jump": "a" })),
        } })).expect("reads");
        let mut a = row("a", "c1");
        a["hostKey"] = json!(FINGERPRINT);
        let mut b = row("b", "c2");
        b["hostKey"] = json!(FINGERPRINT);
        let state = state_with(vec![a, b], vec![json!({ "id": "r1", "connectionId": "c2", "state": "up" })]);
        let items = decide_connections(&manifest, &state, &BTreeSet::new(), &mut Vec::new());
        assert_eq!(items[0].status, Status::Updated);
        assert_eq!(items[1].status, Status::Failed);
        assert_eq!(items[1].extra["code"], json!("connection.host-key-required"));
    }
}
