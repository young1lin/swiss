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

//! `swiss setup show` (SPEC §host.setup): this gateway's state as a manifest, without values.
//!
//! The vault cannot be read back, so a secret's name is `ask` and nothing else. A slot that held
//! a credential holds the mask sentinel the panel already prints; a reference is kept, because it
//! is not the value. Feeding the manifest back to `plan` is refused wherever that sentinel sits.
//! Reads only: no seat is written, and a seat this process cannot see stays out of the document.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use super::manifest::{Warning, VERSION};
use super::plan;
use super::plugins::{self, Rows};
use super::report::EXIT_NO_GATEWAY;
use crate::adminapi::DEF_KEYS;
use crate::remote_cli::Gateway;
use crate::skill_install::{CopyState, Places};
use swiss_core::secure::literal::{self, credential_name};
use swiss_core::secure::refs::secret_ref_parts;
use swiss_host::config::is_env_ref;
use swiss_host::mask::{
    is_scheme_and_reference, is_secret_env_key, is_secret_header_key, mask_command, mask_url, MASK,
    SECRET_KEYS,
};

const SEATS: [(&str, &str); 2] = [("path", "/api/user-path"), ("autostart", "/api/autostart")];
const CONNECTION_KEYS: [&str; 12] = [
    "host", "port", "username", "authType", "keyPath", "passphrase", "password", "hostKey", "group", "proxy",
    "proxyUsername", "proxyPassword",
];
const RULE_KEYS: [&str; 8] = [
    "localPort", "targetHost", "targetPort", "remark", "autoReconnect", "reconnectInterval", "mcps", "group",
];

/// What `swiss setup show` prints for the gateway already on `port`.
pub async fn show_at(port: u16) -> (Value, i32) {
    show_on(port, false).await
}

pub(crate) async fn show_on(port: u16, started: bool) -> (Value, i32) {
    let gw = Gateway::at(port);
    let own_build = env!("SWISS_GIT_HASH");
    let info = match gw.get("/api/info").await {
        Ok(info) => info,
        Err(error) => return unavailable(&literal::mask(&error)),
    };
    let rows = match plugins::wait_until_started(&gw).await {
        Ok(rows) => rows,
        Err(error) => return unavailable(&literal::mask(&error)),
    };
    let build = info["build"]["hash"].as_str();
    let mut warnings = Vec::new();
    if let Some(warning) = plan::stale_build(build, own_build) {
        warnings.push(warning);
    }
    let manifest = match assemble(&gw, &rows, &mut warnings).await {
        Ok(manifest) => manifest,
        Err(error) => return unavailable(&literal::mask(&error)),
    };
    let document = json!({
        "ok": true,
        "mode": "show",
        "gateway": { "port": port, "started": started, "build": build },
        "manifest": manifest,
        "warnings": warnings,
    });
    (document, 0)
}

fn unavailable(error: &str) -> (Value, i32) {
    (json!({ "ok": false, "mode": "show", "error": error }), EXIT_NO_GATEWAY)
}

async fn assemble(gw: &Gateway, rows: &Rows, warnings: &mut Vec<Warning>) -> Result<Value, String> {
    let mut manifest = Map::new();
    manifest.insert("version".into(), json!(VERSION));
    if let Some(machine) = machine_section(gw, warnings).await {
        manifest.insert("machine".into(), machine);
    }
    if let Some(secrets) = secrets_section(gw).await? {
        manifest.insert("secrets".into(), secrets);
    }
    manifest.insert("plugins".into(), plugins_section(gw, rows, warnings).await);
    if let Some(tunnels) = tunnels_section(gw, rows, warnings).await {
        manifest.insert("tunnels".into(), tunnels);
    }
    if let Some(mcps) = mcps_section(gw, rows, warnings).await {
        manifest.insert("mcps".into(), mcps);
    }
    if let Some(remote) = remote_section(gw, rows, warnings).await {
        manifest.insert("remote".into(), remote);
    }
    Ok(Value::Object(manifest))
}

async fn machine_section(gw: &Gateway, warnings: &mut Vec<Warning>) -> Option<Value> {
    let mut machine = Map::new();
    for (key, route) in SEATS {
        match gw.get(route).await {
            Ok(answer) => {
                if let Some(enabled) = answer["enabled"].as_bool() {
                    machine.insert(key.into(), json!(enabled));
                }
            }
            // A composition without the seat answers 503 and has not touched the host.
            Err(error) if error.starts_with("503 ") => {}
            Err(error) => warnings.push(Warning::new(
                "machine.unavailable",
                format!("machine.{key}"),
                literal::mask(&error),
            )),
        }
    }
    match installed_skills() {
        Ok(skills) if !skills.is_empty() => {
            machine.insert("skills".into(), json!(skills));
        }
        Ok(_) => {}
        Err(error) => warnings.push(Warning::new("skill.read", "machine.skills", literal::mask(&error))),
    }
    (!machine.is_empty()).then_some(Value::Object(machine))
}

/// Shipped skills with a copy in either directory. One that cannot be read stays out: absence
/// leaves it untouched, which is the safe guess.
fn installed_skills() -> Result<Vec<String>, String> {
    let places = Places::of_this_user()?;
    let mut names = Vec::new();
    for skill in places.status() {
        if skill.locations.iter().any(|place| place.state == CopyState::Unreadable) {
            continue;
        }
        if skill.locations.iter().any(|place| place.state != CopyState::Absent) {
            names.push(skill.name);
        }
    }
    Ok(names)
}

async fn secrets_section(gw: &Gateway) -> Result<Option<Value>, String> {
    let vault = gw.get("/api/secrets").await?;
    let names = vault["secrets"].as_array().ok_or("the gateway reported no vault")?;
    let mut secrets = Map::new();
    for name in names.iter().filter_map(|name| name.as_str()) {
        secrets.insert(name.to_string(), json!("ask"));
    }
    Ok((!secrets.is_empty()).then_some(Value::Object(secrets)))
}

async fn plugins_section(gw: &Gateway, rows: &Rows, warnings: &mut Vec<Warning>) -> Value {
    let mut plugins = Map::new();
    for (id, row) in rows {
        let Some(enabled) = row["enabled"].as_bool() else {
            continue;
        };
        let mut entry = Map::new();
        entry.insert("enabled".into(), json!(enabled));
        match gw.get(&format!("/api/plugins/{id}/config")).await {
            Ok(answer) => {
                if let Some(config) = answer.get("config").cloned().and_then(|config| mask_node("", "", config)) {
                    entry.insert("config".into(), config);
                }
            }
            Err(error) => warnings.push(Warning::new(
                "plugin.unreadable",
                format!("plugins.{id}"),
                literal::mask(&error),
            )),
        }
        plugins.insert(id.clone(), Value::Object(entry));
    }
    Value::Object(plugins)
}

fn serving(rows: &Rows, id: &str) -> bool {
    rows.get(id).and_then(|row| row["state"].as_str()) == Some("active")
}

/// Enabled, but not serving: the section cannot be read, and saying so is the whole of it.
fn left_out(rows: &Rows, id: &str, warnings: &mut Vec<Warning>) -> bool {
    let Some(row) = rows.get(id) else {
        return true;
    };
    if serving(rows, id) {
        return false;
    }
    if row["enabled"] == json!(true) {
        warnings.push(Warning::new(
            "plugin.not-serving",
            format!("plugins.{id}"),
            format!("the {id} plugin is not serving, so what it holds is left out of the manifest"),
        ));
    }
    true
}

async fn tunnels_section(gw: &Gateway, rows: &Rows, warnings: &mut Vec<Warning>) -> Option<Value> {
    if left_out(rows, "tunnels", warnings) {
        return None;
    }
    let answer = match gw.get("/api/tunnels").await {
        Ok(answer) => answer,
        Err(error) => {
            warnings.push(Warning::new("tunnels.unreadable", "tunnels", literal::mask(&error)));
            return None;
        }
    };
    let connections = answer["connections"].as_array();
    let rules = answer["rules"].as_array();
    let (connections, id_to_name) = named_connections(connections.unwrap_or(&Vec::new()), warnings);
    let rules = named_rules(rules.unwrap_or(&Vec::new()), &id_to_name, warnings);
    if connections.is_empty() && rules.is_empty() {
        return None;
    }
    let mut tunnels = Map::new();
    if !connections.is_empty() {
        tunnels.insert("connections".into(), object_of(connections));
    }
    if !rules.is_empty() {
        tunnels.insert("rules".into(), object_of(rules));
    }
    Some(Value::Object(tunnels))
}

/// Names that belong to one row, and that row's id. A shared name is in neither: nothing here
/// guesses which row a manifest's name means.
fn index_connections(rows: &[Value]) -> (BTreeSet<String>, BTreeMap<String, String>) {
    let shared = shared_names(rows);
    let mut id_to_name = BTreeMap::new();
    for row in rows {
        let (Some(id), Some(name)) = (row["id"].as_str(), row["name"].as_str()) else {
            continue;
        };
        if !shared.contains(name) {
            id_to_name.insert(id.to_string(), name.to_string());
        }
    }
    (shared, id_to_name)
}

fn named_connections(rows: &[Value], warnings: &mut Vec<Warning>) -> (BTreeMap<String, Value>, BTreeMap<String, String>) {
    let (shared, id_to_name) = index_connections(rows);
    let mut connections = BTreeMap::new();
    for row in rows {
        let Some(name) = row["name"].as_str().filter(|name| !shared.contains(*name)) else {
            continue;
        };
        let mut fields = copied(row, &CONNECTION_KEYS);
        if let Some(jump) = row["jump"].as_str() {
            match id_to_name.get(jump) {
                Some(hop) => {
                    fields.insert("jump".into(), json!(hop));
                }
                None => warnings.push(Warning::new(
                    "connection.jump",
                    format!("tunnels.connections.{name}.jump"),
                    "the jump this connection dials through has no single name, so the field is left out",
                )),
            }
        }
        if let Some(masked) = mask_node("", "", Value::Object(fields)) {
            connections.insert(name.to_string(), masked);
        }
    }
    for name in shared {
        let ids = ids_named(rows, &name);
        warnings.push(Warning::new(
            "connection.ambiguous",
            format!("tunnels.connections.{name}"),
            format!("connections on the gateway share this name, so they are left out; their ids are {ids}"),
        ));
    }
    (connections, id_to_name)
}

fn named_rules(
    rows: &[Value],
    id_to_name: &BTreeMap<String, String>,
    warnings: &mut Vec<Warning>,
) -> BTreeMap<String, Value> {
    let shared = shared_names(rows);
    let mut rules = BTreeMap::new();
    for row in rows {
        let Some(name) = row["name"].as_str() else {
            continue;
        };
        if shared.contains(name) {
            continue;
        }
        let Some(connection) = row["connectionName"].as_str().filter(|name| *name != "(unknown)") else {
            warnings.push(Warning::new(
                "rule.connection",
                format!("tunnels.rules.{name}"),
                "the connection this rule uses has no name on the gateway, so the rule is left out",
            ));
            continue;
        };
        if row["connectionId"].as_str().is_some_and(|id| !id_to_name.contains_key(id)) {
            warnings.push(Warning::new(
                "rule.connection",
                format!("tunnels.rules.{name}.connection"),
                "the connection this rule uses has no single name, so the rule is left out",
            ));
            continue;
        }
        let mut fields = copied(row, &RULE_KEYS);
        if fields.get("mcps").and_then(Value::as_array).is_some_and(Vec::is_empty) {
            fields.remove("mcps");
        }
        fields.insert("connection".into(), json!(connection));
        if let Some(masked) = mask_node("", "", Value::Object(fields)) {
            rules.insert(name.to_string(), masked);
        }
    }
    for name in shared {
        let ids = ids_named(rows, &name);
        warnings.push(Warning::new(
            "rule.ambiguous",
            format!("tunnels.rules.{name}"),
            format!("rules on the gateway share this name, so they are left out; their ids are {ids}"),
        ));
    }
    rules
}

fn shared_names(rows: &[Value]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut shared = BTreeSet::new();
    for name in rows.iter().filter_map(|row| row["name"].as_str()) {
        if !seen.insert(name.to_string()) {
            shared.insert(name.to_string());
        }
    }
    shared
}

fn ids_named(rows: &[Value], name: &str) -> String {
    rows.iter()
        .filter(|row| row["name"].as_str() == Some(name))
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn copied(row: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut fields = Map::new();
    for key in keys {
        if let Some(value) = row.get(*key).filter(|value| !value.is_null()) {
            fields.insert((*key).to_string(), value.clone());
        }
    }
    fields
}

async fn mcps_section(gw: &Gateway, rows: &Rows, warnings: &mut Vec<Warning>) -> Option<Value> {
    if left_out(rows, "mcp", warnings) {
        return None;
    }
    let listing = match gw.get("/api/mcps").await {
        Ok(listing) => listing,
        Err(error) => {
            warnings.push(Warning::new("mcp.unreadable", "mcps", literal::mask(&error)));
            return None;
        }
    };
    let names: Vec<String> = listing["mcps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["name"].as_str().map(str::to_string))
        .collect();
    let mut mcps = BTreeMap::new();
    for name in names {
        let details = match gw.get(&format!("/api/mcps/{name}/details")).await {
            Ok(details) => details,
            Err(error) => {
                warnings.push(Warning::new("mcp.unreadable", format!("mcps.{name}"), literal::mask(&error)));
                continue;
            }
        };
        match details.get("config").cloned().and_then(mcp_entry) {
            Some(entry) => {
                mcps.insert(name, entry);
            }
            None => warnings.push(Warning::new(
                "mcp.unreadable",
                format!("mcps.{name}"),
                "the gateway's definition is not one this manifest can describe, so it is left out",
            )),
        }
    }
    (!mcps.is_empty()).then_some(object_of(mcps))
}

fn mcp_entry(config: Value) -> Option<Value> {
    let config = mask_node("", "", config)?;
    let fields = config.as_object()?;
    let type_name = fields.get("type").and_then(Value::as_str).unwrap_or("proc");
    let known = known_mcp_keys(type_name)?;
    let mut kept = Map::new();
    for (key, value) in fields {
        if known.contains(key.as_str()) {
            kept.insert(key.clone(), value.clone());
        }
    }
    (!kept.is_empty()).then_some(Value::Object(kept))
}

fn known_mcp_keys(type_name: &str) -> Option<BTreeSet<&str>> {
    let row = DEF_KEYS.iter().find(|row| row.type_ == type_name)?;
    let mut keys = BTreeSet::from(["type", "lazy", "idleMs"]);
    keys.extend(row.required.iter().copied());
    keys.extend(row.other.iter().copied());
    if type_name == "figma" {
        keys.extend(["url", "auth", "oauthClientName", "headers"]);
    }
    Some(keys)
}

async fn remote_section(gw: &Gateway, rows: &Rows, warnings: &mut Vec<Warning>) -> Option<Value> {
    if left_out(rows, "remote", warnings) {
        return None;
    }
    let answer = match gw.get("/api/remote/targets").await {
        Ok(answer) => answer,
        Err(error) => {
            warnings.push(Warning::new("remote.unreadable", "remote", literal::mask(&error)));
            return None;
        }
    };
    let id_to_name = connection_names(gw, warnings).await;
    let mut targets = BTreeMap::new();
    for row in answer["targets"].as_array().into_iter().flatten() {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        let Some(endpoint) = row["endpoint"].as_str() else {
            continue;
        };
        let Some(root) = row["workspaceRoot"].as_str() else {
            continue;
        };
        let Some(name) = id_to_name.get(endpoint) else {
            warnings.push(Warning::new(
                "target.endpoint",
                format!("remote.targets.{id}"),
                "the connection this target uses has no single name, so the target is left out",
            ));
            continue;
        };
        let mut fields = Map::new();
        fields.insert("endpoint".into(), json!(name));
        fields.insert("root".into(), json!(root));
        if let Some(caps) = row.get("capabilities").filter(|caps| caps.is_array()) {
            fields.insert("caps".into(), caps.clone());
        }
        if let Some(masked) = mask_node("", "", Value::Object(fields)) {
            targets.insert(id.to_string(), masked);
        }
    }
    (!targets.is_empty()).then_some(json!({ "targets": object_of(targets) }))
}

async fn connection_names(gw: &Gateway, warnings: &mut Vec<Warning>) -> BTreeMap<String, String> {
    let Ok(answer) = gw.get("/api/tunnels").await else {
        warnings.push(Warning::new(
            "target.endpoint",
            "remote.targets",
            "the connections a target can name could not be read, so the targets are left out",
        ));
        return BTreeMap::new();
    };
    let rows = answer["connections"].as_array().cloned().unwrap_or_default();
    let (_, id_to_name) = index_connections(&rows);
    id_to_name
}

fn object_of(fields: BTreeMap<String, Value>) -> Value {
    Value::Object(fields.into_iter().collect())
}

/// A value safe to put in a manifest: null is dropped, a credential slot holds the mask, and a
/// reference with no default is kept. A default is typed text, so the whole slot becomes the mask
/// rather than print it.
fn mask_node(parent: &str, key: &str, value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(Value::String(mask_text(parent, key, &text))),
        Value::Number(_) if credential_slot(parent, key) => Some(Value::String(MASK.into())),
        Value::Array(items) if key == "args" => Some(mask_args(items)),
        Value::Array(items) => {
            let items = items.into_iter().filter_map(|item| mask_node("", "", item)).collect();
            Some(Value::Array(items))
        }
        Value::Object(fields) => {
            let mut out = Map::new();
            for (child, item) in fields {
                if let Some(masked) = mask_node(key, &child, item) {
                    out.insert(child, masked);
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        other => Some(other),
    }
}

fn mask_text(parent: &str, key: &str, text: &str) -> String {
    if leaks_a_default(text) {
        return MASK.to_string();
    }
    let value = Value::String(text.to_string());
    if is_env_ref(&value) || is_scheme_and_reference(&value) {
        return text.to_string();
    }
    if key.eq_ignore_ascii_case("auth") && text == "oauth" {
        return text.to_string();
    }
    if credential_slot(parent, key) && !text.is_empty() {
        return MASK.to_string();
    }
    if is_url_key(key) {
        return literal::mask(&mask_url(text));
    }
    if key == "command" {
        return literal::mask(&mask_command(text));
    }
    literal::mask(text)
}

/// An argv is one command line, so `--token` and the next word are read together.
fn mask_args(items: Vec<Value>) -> Value {
    if items.iter().any(|item| !item.is_string()) {
        let walked = items.into_iter().filter_map(|item| mask_node("", "", item)).collect();
        return Value::Array(walked);
    }
    let mut words: Vec<String> = items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect();
    while let Some((index, hit)) = literal::find_argv(&words) {
        let word = words[index].clone();
        if hit.start > word.len() || hit.end > word.len() || hit.start > hit.end {
            break;
        }
        let mut next = String::new();
        next.push_str(&word[..hit.start]);
        next.push_str(MASK);
        next.push_str(&word[hit.end..]);
        if next == word {
            break;
        }
        words[index] = next;
    }
    Value::Array(words.into_iter().map(Value::String).collect())
}

fn credential_slot(parent: &str, key: &str) -> bool {
    match parent {
        "env" => is_secret_env_key(key),
        "headers" => is_secret_header_key(key),
        _ => SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str()) || credential_name(key),
    }
}

fn is_url_key(key: &str) -> bool {
    matches!(key.to_ascii_lowercase().as_str(), "url" | "connectionstring" | "dsn" | "proxy")
}

/// `${secret://name:default}` carries the default in the text. The name is not worth keeping if
/// keeping it prints the default.
fn leaks_a_default(text: &str) -> bool {
    text.split("${secret://").skip(1).any(|rest| {
        let body = rest.split('}').next().unwrap_or(rest);
        secret_ref_parts(body).is_some_and(|(_, default)| default.is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUE: &str = "show-literal-9f3a";

    fn masked(value: Value) -> Value {
        mask_node("", "", value).unwrap_or(Value::Null)
    }

    // SPEC §host.setup: show prints the mask where a credential was, and keeps a reference.
    #[test]
    fn a_shown_value_keeps_a_reference_and_masks_a_literal() {
        let shown = masked(json!({
            "password": VALUE,
            "token": "${secret://db-pass}",
            "apiKey": format!("${{secret://db-pass:{VALUE}}}"),
            "auth": "oauth",
            "host": "db.internal",
            "headers": { "Authorization": format!("Bearer {VALUE}"), "Accept": "application/json" },
            "env": { "API_KEY": VALUE, "PORT": "1" },
            "url": format!("redis://u:{VALUE}@10.0.0.1:6379"),
            "command": format!("server --password={VALUE}"),
            "args": ["--token", VALUE],
            "gone": null,
        }));
        let text = shown.to_string();
        assert!(!text.contains(VALUE), "{text}");
        assert_eq!(shown["password"], json!(MASK));
        assert_eq!(shown["token"], json!("${secret://db-pass}"));
        assert_eq!(shown["apiKey"], json!(MASK));
        assert_eq!(shown["auth"], json!("oauth"));
        assert_eq!(shown["host"], json!("db.internal"));
        assert_eq!(shown["headers"]["Authorization"], json!(MASK));
        assert_eq!(shown["headers"]["Accept"], json!("application/json"));
        assert_eq!(shown["env"]["API_KEY"], json!(MASK));
        assert!(shown["url"].as_str().expect("a url").contains(MASK), "{shown}");
        assert!(shown["command"].as_str().expect("a command").contains(MASK), "{shown}");
        assert!(shown.get("gone").is_none(), "{shown}");
        assert_eq!(masked(json!({ "password": 12345 })), json!({ "password": MASK }));
    }
}
