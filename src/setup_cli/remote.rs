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

//! The manifest's `remote` section (SPEC §host.setup, §remote.targets). A target is keyed by the id
//! the gateway uses too, and its endpoint is the id of a connection, which a manifest names.

use std::collections::{BTreeMap, BTreeSet};

use reqwest::Method;
use serde_json::{json, Map, Value};

use super::apply::fail;
use super::manifest::Manifest;
use super::plugins::{plugin_off_reason, Rows};
use super::refuse::{join, nearest, scan_definition, Problem, Scan};
use super::report::{Item, Status};
use super::tunnels::{entries, off_code, read_rows, text, TunnelState};
use crate::remote_cli::Gateway;
use swiss_core::secure::literal;
use swiss_core::secure::refs::references;
use swiss_remote::target::{valid_abs_posix, valid_id, KNOWN_CAPABILITIES};

const SECTION_KEYS: [&str; 1] = ["targets"];
const TARGET_KEYS: [&str; 3] = ["endpoint", "root", "caps"];

/// What a target needs from the transport before anything else it can do.
const EXEC: &str = "exec";

const ID_RULE: &str = "a target's id is a lowercase letter or digit, then lowercase letters, digits or -, 64 at most";
const ROOT_RULE: &str = "root is an absolute POSIX path, with no trailing slash and no .. in it";
const NO_REFERENCE_HERE: &str = "the gateway does not expand a reference here, so it would stay this text";
const NOT_FOUND: &str = "names no connection in this manifest or on the gateway";
const SHARED: &str = "more than one connection on the gateway has this name, and nothing here guesses between them";

/// One target: the connection it runs through, by name, and the folder it works in.
#[derive(Debug, Clone)]
pub struct TargetEntry {
    pub endpoint: String,
    pub root: String,
    /// Left out on an existing target, its stored capabilities stay.
    pub caps: Option<Vec<String>>,
}

pub fn schema() -> Value {
    json!({ "targets": { "identity": "the target's id", "id": ID_RULE, "keys": TARGET_KEYS,
        "required": ["endpoint", "root"], "endpoint": "a connection's name, in this manifest or on the gateway",
        "root": ROOT_RULE, "caps": KNOWN_CAPABILITIES, "defaultCaps": [EXEC],
        "preserved": "left out, existing capabilities stay; the label, group and timeout stay too" } })
}

pub fn read_section(section: Option<&Value>, manifest: &mut Manifest, problems: &mut Vec<Problem>) {
    let Some(section) = section.filter(|section| !section.is_null()) else {
        return;
    };
    let Some(fields) = section.as_object() else {
        problems.push(Problem::new("remote", "remote holds targets, a map of ids to definitions"));
        return;
    };
    for key in fields.keys().filter(|key| !SECTION_KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join("remote", key), key, SECTION_KEYS));
    }
    for (id, entry) in entries("remote.targets", fields.get("targets"), problems) {
        read_target(id, entry, manifest, problems);
    }
}

fn read_target(id: &str, entry: &Value, manifest: &mut Manifest, problems: &mut Vec<Problem>) {
    let path = join("remote.targets", id);
    if !valid_id(id) {
        problems.push(Problem::new(path.clone(), ID_RULE));
    }
    let Some(fields) = entry.as_object() else {
        problems.push(Problem::new(path, "a target is an object"));
        return;
    };
    for key in fields.keys().filter(|key| !TARGET_KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join(&path, key), key, TARGET_KEYS));
    }
    let mut scan = Scan::default();
    scan_definition(&path, entry, &mut scan);
    problems.append(&mut scan.problems);
    for slot in ["endpoint", "root"] {
        let found = fields.get(slot).and_then(Value::as_str).and_then(|value| references(value).ok());
        if found.is_some_and(|(named, _)| !named.is_empty()) {
            problems.push(Problem::new(join(&path, slot), NO_REFERENCE_HERE));
        }
    }
    let endpoint = text(&path, fields, "endpoint", problems);
    if endpoint.is_none() && !problems.iter().any(|problem| problem.path == join(&path, "endpoint")) {
        problems.push(Problem::new(join(&path, "endpoint"), "a target names the connection it runs through"));
    }
    let root = text(&path, fields, "root", problems);
    match &root {
        Some(root) if !valid_abs_posix(root) && !problems.iter().any(|problem| problem.path == join(&path, "root")) => {
            problems.push(Problem::new(join(&path, "root"), ROOT_RULE));
        }
        None if !problems.iter().any(|problem| problem.path == join(&path, "root")) => {
            problems.push(Problem::new(join(&path, "root"), "a target needs a root"));
        }
        _ => {}
    }
    let caps = fields.get("caps").filter(|caps| !caps.is_null()).and_then(|caps| read_caps(&join(&path, "caps"), caps, problems));
    if let (Some(endpoint), Some(root)) = (endpoint, root) {
        manifest.targets.insert(id.to_string(), TargetEntry { endpoint, root, caps });
    }
}

fn read_caps(path: &str, caps: &Value, problems: &mut Vec<Problem>) -> Option<Vec<String>> {
    let the_rule = || format!("caps lists what a target may do, {} at most, and exec is one of them", KNOWN_CAPABILITIES.join(", "));
    let Some(words) = caps.as_array() else {
        problems.push(Problem::new(path, the_rule()));
        return None;
    };
    let mut seen = BTreeSet::new();
    let mut clean = true;
    for (index, word) in words.iter().enumerate() {
        let slot = format!("{path}[{index}]");
        match word.as_str() {
            Some(word) if KNOWN_CAPABILITIES.contains(&word) => {
                if !seen.insert(word.to_string()) {
                    problems.push(Problem::new(slot, "this capability is listed twice"));
                    clean = false;
                }
            }
            Some(word) => {
                let message = match nearest(word, &KNOWN_CAPABILITIES) {
                    Some(near) => format!("no such capability; did you mean {near}?"),
                    None => the_rule(),
                };
                problems.push(Problem::new(slot, message));
                clean = false;
            }
            None => {
                problems.push(Problem::new(slot, the_rule()));
                clean = false;
            }
        }
    }
    if clean && !seen.contains(EXEC) {
        problems.push(Problem::new(path, the_rule()));
        clean = false;
    }
    clean.then(|| words.iter().filter_map(|word| word.as_str().map(str::to_string)).collect())
}

/// What the gateway holds of what the section names.
#[derive(Debug, Default)]
pub struct RemoteState {
    /// The plugin that holds them is not serving, for this reason.
    pub off: Option<String>,
    pub targets: BTreeMap<String, Value>,
}

pub async fn read_state(gw: &Gateway, manifest: &Manifest, rows: &Rows) -> Result<RemoteState, String> {
    if manifest.targets.is_empty() {
        return Ok(RemoteState::default());
    }
    if let Some(reason) = plugin_off_reason(rows, "remote") {
        return Ok(RemoteState { off: Some(reason), ..RemoteState::default() });
    }
    read_targets(gw).await
}

async fn read_targets(gw: &Gateway) -> Result<RemoteState, String> {
    let answer = gw.get("/api/remote/targets").await?;
    let rows = answer["targets"].as_array().ok_or("the gateway reported no targets")?;
    let targets = rows
        .iter()
        .filter_map(|row| row["id"].as_str().map(|id| (id.to_string(), row.clone())))
        .collect();
    Ok(RemoteState { off: None, targets })
}

/// Every target. `decided` holds the connections, which is where an endpoint comes from.
pub fn decide(manifest: &Manifest, state: &RemoteState, tunnels: &TunnelState, decided: &[Item]) -> Vec<Item> {
    manifest
        .targets
        .iter()
        .map(|(id, entry)| decide_target(id, entry, manifest, state, tunnels, decided))
        .collect()
}

fn decide_target(
    id: &str,
    entry: &TargetEntry,
    manifest: &Manifest,
    state: &RemoteState,
    tunnels: &TunnelState,
    decided: &[Item],
) -> Item {
    if let Some(reason) = state.off.as_deref().filter(|_| !manifest.turns_on("remote")) {
        return Item::new("target", id, Status::Created).failed(off_code("remote"), reason);
    }
    if let Some(reason) = tunnels.off.as_deref().filter(|_| !manifest.turns_on("tunnels")) {
        return Item::new("target", id, Status::Created).failed(off_code("tunnels"), reason);
    }
    let found = tunnels.connections_named(&entry.endpoint);
    if manifest.connections.contains_key(&entry.endpoint) {
        let hop = decided.iter().find(|item| item.kind == "connection" && item.name == entry.endpoint);
        if hop.is_some_and(|item| matches!(item.status, Status::Failed | Status::Blocked)) {
            return Item::new("target", id, Status::Blocked).with("by", json!([format!("connection:{}", entry.endpoint)]));
        }
    } else if found.is_empty() {
        return Item::new("target", id, Status::Created).failed("target.endpoint", NOT_FOUND);
    } else if found.len() > 1 {
        let ids: Vec<Value> = found.iter().map(|row| row["id"].clone()).collect();
        return Item::new("target", id, Status::Created).failed("target.endpoint", SHARED).with("ids", ids);
    }
    let endpoint = found.first().map(|row| &row["id"]);
    let status = match state.targets.get(id) {
        None => Status::Created,
        Some(row) if target_matches(entry, row, endpoint) => Status::Unchanged,
        Some(_) => Status::Updated,
    };
    Item::new("target", id, status)
}

fn target_matches(entry: &TargetEntry, row: &Value, endpoint: Option<&Value>) -> bool {
    endpoint == row.get("endpoint")
        && row["workspaceRoot"] == json!(entry.root)
        && entry.caps.as_ref().is_none_or(|caps| same_capabilities(caps, &row["capabilities"]))
}

/// The order of a capability list means nothing.
fn same_capabilities(caps: &[String], stored: &Value) -> bool {
    let wanted: BTreeSet<&str> = caps.iter().map(String::as_str).collect();
    let held: BTreeSet<&str> = stored.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    wanted == held
}

/// Connections were written before this, so their ids are read fresh.
pub async fn write(gw: &Gateway, manifest: &Manifest, items: &mut [Item]) {
    let indices: Vec<usize> = items.iter().enumerate().filter(|(_, item)| item.kind == "target").map(|(index, _)| index).collect();
    if indices.is_empty() {
        return;
    }
    let read = async {
        let tunnels = read_rows(gw).await?;
        let targets = read_targets(gw).await?;
        Ok::<_, String>((tunnels.connection_ids(), targets.targets))
    };
    let (connection_ids, stored) = match read.await {
        Ok(found) => found,
        Err(error) => {
            for index in indices {
                if matches!(items[index].status, Status::Created | Status::Updated) {
                    fail(&mut items[index], "target.write", literal::mask(&error));
                }
            }
            return;
        }
    };
    for index in indices {
        if matches!(items[index].status, Status::Failed | Status::Blocked) {
            continue;
        }
        let Some(entry) = manifest.targets.get(&items[index].name) else {
            continue;
        };
        let hop = items.iter().find(|item| item.kind == "connection" && item.name == entry.endpoint);
        if hop.is_some_and(|item| matches!(item.status, Status::Failed | Status::Blocked)) {
            items[index].status = Status::Blocked;
            items[index].note("by", json!([format!("connection:{}", entry.endpoint)]));
            continue;
        }
        write_target(gw, entry, &mut items[index], &connection_ids, &stored).await;
    }
}

async fn write_target(
    gw: &Gateway,
    entry: &TargetEntry,
    item: &mut Item,
    connection_ids: &BTreeMap<String, String>,
    stored: &BTreeMap<String, Value>,
) {
    if !matches!(item.status, Status::Created | Status::Updated) {
        return;
    }
    let Some(endpoint) = connection_ids.get(&entry.endpoint) else {
        return fail(item, "target.write", "the connection it runs through has no id on the gateway".to_string());
    };
    let (path, body) = match (item.status, stored.get(&item.name)) {
        (Status::Created, _) | (_, None) => ("/api/remote/targets".to_string(), new_target(&item.name, entry, endpoint)),
        (_, Some(row)) => (format!("/api/remote/targets/{}", item.name), overlaid(row, entry, endpoint)),
    };
    if let Err(error) = gw.send(Method::POST, &path, Some(body)).await {
        fail(item, "target.write", literal::mask(&error));
    }
}

fn new_target(id: &str, entry: &TargetEntry, endpoint: &str) -> Value {
    let caps = entry.caps.clone().unwrap_or_else(|| vec![EXEC.to_string()]);
    json!({ "id": id, "endpoint": endpoint, "workspaceRoot": entry.root, "shell": "posix", "capabilities": caps })
}

/// An update is the whole target, so what the manifest cannot say (its label, its group, its
/// timeout) rides over from what is stored.
fn overlaid(row: &Value, entry: &TargetEntry, endpoint: &str) -> Value {
    let mut body: Map<String, Value> = row.as_object().cloned().unwrap_or_default();
    body.insert("endpoint".into(), json!(endpoint));
    body.insert("workspaceRoot".into(), json!(entry.root));
    if let Some(caps) = &entry.caps {
        body.insert("capabilities".into(), json!(caps));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(section: Value) -> Result<Manifest, Vec<(String, String)>> {
        let mut manifest = Manifest::default();
        let mut problems = Vec::new();
        read_section(Some(&section), &mut manifest, &mut problems);
        if problems.is_empty() {
            Ok(manifest)
        } else {
            Err(problems.into_iter().map(|p| (p.path, p.message)).collect())
        }
    }

    #[test]
    fn a_target_takes_an_endpoint_a_root_and_its_capabilities() {
        let manifest = parsed(json!({ "targets": { "gpu": { "endpoint": "bastion", "root": "/srv/gpu", "caps": ["exec", "files"] } } })).expect("reads");
        assert_eq!(manifest.targets["gpu"].caps.as_deref(), Some(&["exec".to_string(), "files".to_string()][..]));

        let problems = parsed(json!({ "targes": {}, "targets": {
            "Bad_Id": { "endpoint": "b", "root": "/x" },
            "relative": { "endpoint": "b", "root": "srv/x" },
            "slashed": { "endpoint": "b", "root": "/srv/x/" },
            "no-exec": { "endpoint": "b", "root": "/x", "caps": ["files"] },
            "typo": { "endpoint": "b", "root": "/x", "caps": ["exec", "file"] },
            "twice": { "endpoint": "b", "root": "/x", "caps": ["exec", "exec"] },
            "reference": { "endpoint": "${secret://a}", "root": "/x" },
            "bare": {},
        } }))
        .expect_err("refused");
        let paths: Vec<&str> = problems.iter().map(|(path, _)| path.as_str()).collect();
        for path in [
            "remote.targes",
            "remote.targets.Bad_Id",
            "remote.targets.relative.root",
            "remote.targets.slashed.root",
            "remote.targets.no-exec.caps",
            "remote.targets.typo.caps[1]",
            "remote.targets.twice.caps[1]",
            "remote.targets.reference.endpoint",
            "remote.targets.bare.endpoint",
            "remote.targets.bare.root",
        ] {
            assert!(paths.contains(&path), "{path} in {paths:?}");
        }
    }

    // SPEC §host.setup: an update keeps what a manifest cannot say.
    #[test]
    fn an_update_keeps_the_label_the_group_and_the_timeout() {
        let stored = json!({ "id": "gpu", "label": "GPU box", "endpoint": "old", "workspaceRoot": "/old", "shell": "posix",
                             "capabilities": ["exec", "sync"], "defaultTimeoutMs": 9000, "group": "lab" });
        let entry = TargetEntry { endpoint: "bastion".into(), root: "/srv/gpu".into(), caps: None };
        let body = overlaid(&stored, &entry, "c1");
        assert_eq!(body["label"], json!("GPU box"));
        assert_eq!(body["group"], json!("lab"));
        assert_eq!(body["defaultTimeoutMs"], json!(9000));
        assert_eq!(body["capabilities"], json!(["exec", "sync"]), "left out, they stay");
        assert_eq!(body["endpoint"], json!("c1"));
        assert_eq!(body["workspaceRoot"], json!("/srv/gpu"));
    }

    #[test]
    fn capabilities_compare_as_a_set_and_only_when_named() {
        let row = json!({ "endpoint": "c1", "workspaceRoot": "/x", "capabilities": ["files", "exec"] });
        let endpoint = json!("c1");
        let entry = |caps: Option<&[&str]>| TargetEntry {
            endpoint: "b".into(),
            root: "/x".into(),
            caps: caps.map(|caps| caps.iter().map(|cap| cap.to_string()).collect()),
        };
        assert!(target_matches(&entry(None), &row, Some(&endpoint)));
        assert!(target_matches(&entry(Some(&["exec", "files"])), &row, Some(&endpoint)));
        assert!(!target_matches(&entry(Some(&["exec"])), &row, Some(&endpoint)));
        assert!(!target_matches(&entry(None), &row, Some(&json!("c2"))));
    }
}
