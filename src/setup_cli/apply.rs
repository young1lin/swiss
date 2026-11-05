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

//! Making the gateway hold what the manifest names (SPEC §host.setup): every write `plan::decide`
//! called for, in order, through the routes the panel uses. A failed item does not stop the rest;
//! what uses it is `blocked`.

use std::collections::BTreeSet;
use std::path::Path;

use reqwest::Method;
use serde_json::{json, Value};

use super::manifest::{Manifest, McpEntry, SecretSpec, Warning};
use super::plan::{decide_mcp, read_mcps, read_source};
use super::{machine, plugins, remote, tunnels};
use super::report::{Item, Status};
use super::sources::Source;
use super::store_secret;
use crate::remote_cli::Gateway;
use swiss_host::config::ServerDef;

/// Secrets first, because a definition resolves what it references when it is built, then plugins,
/// because the MCPs live in one.
pub async fn write_all(
    gw: &Gateway,
    manifest: &Manifest,
    base: &Path,
    items: &mut [Item],
    warnings: &mut Vec<Warning>,
) {
    for item in items.iter_mut().filter(|item| item.kind == "secret") {
        if let Some(SecretSpec::From(source)) = manifest.secrets.get(&item.name) {
            write_secret(gw, source, base, item).await;
        }
    }
    let failed: BTreeSet<String> = items
        .iter()
        .filter(|item| item.kind == "secret" && item.status == Status::Failed)
        .map(|item| item.name.clone())
        .collect();
    for item in items.iter_mut().filter(|item| item.kind == "plugin" && item.status != Status::Failed) {
        let Some(entry) = manifest.plugins.get(&item.name) else {
            continue;
        };
        let by = blocked_by(&entry.secrets, &failed);
        if by.is_empty() {
            plugins::write(gw, entry, item).await;
        } else {
            item.status = Status::Blocked;
            item.note("by", by);
        }
    }
    if warnings.iter().any(|warning| warning.code == "mcp.plugin-off") {
        decide_mcps_again(gw, manifest, items, warnings).await;
    }
    machine::write(gw, &manifest.machine, items).await;
    if warnings.iter().any(|warning| matches!(warning.code, "tunnels.plugin-off" | "remote.plugin-off")) {
        decide_tunnels_again(gw, manifest, items, warnings).await;
    }
    tunnels::write_connections(gw, manifest, items, &failed).await;
    tunnels::write_rules(gw, manifest, items).await;
    remote::write(gw, manifest, items).await;
    for item in items.iter_mut().filter(|item| item.kind == "mcp" && item.status != Status::Failed) {
        let Some(entry) = manifest.mcps.get(&item.name) else {
            continue;
        };
        let by = blocked_by(&entry.secrets, &failed);
        if by.is_empty() {
            write_mcp(gw, entry, item).await;
        } else {
            item.status = Status::Blocked;
            item.note("by", by);
        }
    }
}

pub(super) fn blocked_by(secrets: &BTreeSet<String>, failed: &BTreeSet<String>) -> Vec<Value> {
    secrets.intersection(failed).map(|secret| json!(format!("secret:{secret}"))).collect()
}

/// A plugin turned on after the plan can now say what it already holds.
async fn decide_tunnels_again(gw: &Gateway, manifest: &Manifest, items: &mut [Item], warnings: &mut Vec<Warning>) {
    let read = async {
        let rows = plugins::read_rows(gw).await?;
        let tunnels = tunnels::read_state(gw, manifest, &rows).await?;
        let remote = remote::read_state(gw, manifest, &rows).await?;
        Ok::<_, String>((tunnels, remote))
    };
    match read.await {
        Ok((tunnels, remote)) => {
            warnings.retain(|warning| !matches!(warning.code, "tunnels.plugin-off" | "remote.plugin-off" | "connection.unpinned"));
            let unsettled = items.iter().filter(|item| item.kind == "secret" && matches!(item.status, Status::Failed | Status::NeedsInput))
                .map(|item| item.name.clone()).collect();
            // Failed starts must be reported as off, rather than optimistically planned again.
            let actual = Manifest { connections: manifest.connections.clone(), rules: manifest.rules.clone(),
                                    targets: manifest.targets.clone(), ..Manifest::default() };
            let decided = super::plan::decide_tunnels_and_remote(&actual, &tunnels, &remote, &unsettled, warnings);
            for fresh in decided {
                if let Some(item) = items.iter_mut().find(|item| item.kind == fresh.kind && item.name == fresh.name) {
                    *item = fresh;
                }
            }
        }
        Err(error) => {
            for item in items.iter_mut().filter(|item| matches!(item.kind, "connection" | "rule" | "target")) {
                fail(item, "setup.read", swiss_core::secure::literal::mask(&error));
            }
        }
    }
}

/// The plan could not read what a plugin that was off holds, and the manifest has just turned it on.
/// Now it can, and the rest of the run writes from what it finds. A plugin that did not come up
/// fails every MCP it would have held.
async fn decide_mcps_again(gw: &Gateway, manifest: &Manifest, items: &mut [Item], warnings: &mut Vec<Warning>) {
    let off = match plugins::read_rows(gw).await {
        Ok(rows) => plugins::mcp_off_reason(&rows),
        Err(error) => Some(error),
    };
    let stored = match off {
        Some(reason) => Err(reason),
        None => read_mcps(gw, manifest).await,
    };
    warnings.retain(|warning| warning.code != "mcp.plugin-off");
    let unsettled = BTreeSet::new();
    for item in items.iter_mut().filter(|item| item.kind == "mcp" && item.status != Status::Blocked) {
        let Some(entry) = manifest.mcps.get(&item.name) else {
            continue;
        };
        match &stored {
            Ok(stored) => {
                item.status = decide_mcp(&item.name, entry, stored.get(&item.name), &unsettled, warnings).status;
            }
            Err(reason) => fail(item, "mcp.plugin-off", reason.clone()),
        }
    }
}

/// The value is read here and lives only for this call. The vault cannot be read back, so what the
/// document can say about it is whether the name existed.
async fn write_secret(gw: &Gateway, source: &Source, base: &Path, item: &mut Item) {
    if !matches!(item.status, Status::Created | Status::Updated) {
        return;
    }
    let stored = match read_source(source, base) {
        Ok(value) => store_secret(gw, &item.name, &value).await,
        Err(error) => Err(error),
    };
    let stored = match stored {
        Ok(stored) => stored,
        Err(error) => return fail(item, "secret.write", error),
    };
    item.status = if stored.status == "updated" { Status::Updated } else { Status::Created };
    if stored.refreshed.as_array().is_some_and(|names| !names.is_empty()) {
        item.note("refreshed", stored.refreshed);
    }
    // Stored, but an MCP using it did not come back: the value is there and the machine is not right.
    if stored.failed.as_array().is_some_and(|names| !names.is_empty()) {
        let error = "an MCP using this secret did not rebuild on the new value; `failed` names it";
        item.note("check", json!({ "ok": false, "code": "check.rebuild", "error": error }));
        item.note("failed", stored.failed);
    }
}

/// Added with `start: false`, so nothing holds this answer for a handshake and every check can run
/// after every write. What is sent is the definition `build_def` left, which builds to itself.
async fn write_mcp(gw: &Gateway, entry: &McpEntry, item: &mut Item) {
    let (method, path, body) = match item.status {
        Status::Created => (Method::POST, "/api/mcps".to_string(), create_body(&item.name, &entry.def)),
        Status::Updated => (Method::PUT, format!("/api/mcps/{}", item.name), Value::Object(entry.def.0.clone())),
        _ => return,
    };
    if let Err(error) = gw.send(method, &path, Some(body)).await {
        fail(item, "mcp.write", error);
    }
}

fn create_body(name: &str, def: &ServerDef) -> Value {
    let mut body = def.0.clone();
    body.insert("name".into(), json!(name));
    body.insert("start".into(), json!(false));
    Value::Object(body)
}

pub(super) fn fail(item: &mut Item, code: &str, error: String) {
    item.status = Status::Failed;
    item.note("code", code);
    item.note("error", error);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adminapi::{build_def, DEF_KEYS};

    // SPEC §host.setup: what `apply` sends is the built definition, so the route must build the same
    // one from it, for every type a manifest can name.
    #[test]
    fn a_built_definition_builds_to_itself() {
        let tools = json!([{ "name": "x", "request": { "method": "GET", "path": "/x" } }]);
        let bodies = [
            json!({ "command": "tool", "args": ["-y", "pkg"], "env": { "K": "${secret://x}" }, "cwd": "/srv",
                    "timeoutMs": 1000, "exposePrompts": false, "description": "d", "lazy": false, "idleMs": 5 }),
            json!({ "type": "http", "url": "https://x.test/mcp", "headers": { "Authorization": "Bearer ${secret://x}" },
                    "proxy": "http://127.0.0.1:1", "auth": "oauth", "oauthClientName": "c" }),
            json!({ "type": "rest", "baseUrl": "https://x.test", "tools": tools, "headers": { "A": "${secret://x}" },
                    "timeoutMs": 1000, "lazy": true }),
            json!({ "type": "mysql", "host": "h", "port": 1, "user": "u", "password": "${secret://x}", "database": "d" }),
            json!({ "type": "mariadb", "host": "h", "port": 1, "user": "u", "password": "${secret://x}", "database": "d" }),
            json!({ "type": "pg", "url": "postgres://u:${secret://x}@h/d", "maxRows": 5 }),
            json!({ "type": "redis", "host": "h", "port": 1, "password": "${secret://x}", "db": 1 }),
            json!({ "type": "mongo", "url": "mongodb://u:${secret://x}@h/d" }),
            json!({ "type": "zai-vision", "apiKey": "${secret://x}", "mode": "zai", "model": "m", "baseUrl": "https://x.test" }),
            json!({ "type": "figma", "proxy": "http://127.0.0.1:1", "description": "d" }),
            json!({ "type": "echo" }),
        ];
        let types: BTreeSet<&str> = bodies.iter().map(|body| body["type"].as_str().unwrap_or("proc")).collect();
        assert_eq!(types.len(), DEF_KEYS.len(), "every type has a body here");

        for body in &bodies {
            let built = build_def(body, &[]).expect("a definition");
            let again = build_def(&Value::Object(built.0.clone()), &[]).expect("what PUT receives");
            assert_eq!(again.0, built.0, "{body}");
            let added = build_def(&create_body("m", &built), &["name", "enabled", "start"]).expect("what POST receives");
            assert_eq!(added.0, built.0, "{body}");
        }
    }
}
