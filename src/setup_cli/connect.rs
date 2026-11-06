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

//! `swiss connect` (SPEC §host.setup). A bearer token is written into the client's own config and
//! nowhere else: not the document, not a log line, not an error.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Map, Value};

use super::manifest::{ClientMcps, Manifest, Warning};
use super::refuse::nearest;
use super::report::{Item, Mode, Report, Status};
use crate::adminapi::is_valid_name;
use crate::remote_cli::Gateway;

const CLIENTS: [&str; 3] = ["claude", "codex", "cursor"];

pub fn usage() -> &'static str {
    USAGE
}

const USAGE: &str = "usage: swiss connect <claude|codex|cursor> [--mcp <name>]... [--replace]\n\
    Points that client at this gateway. A token labelled client-<client> is reused or minted \
    and never printed. An existing entry is left unchanged unless --replace. \
    Restart the client afterwards.";

pub struct ConnectArgs {
    pub client: String,
    pub mcps: Vec<String>,
    pub replace: bool,
}

/// Where the three clients keep their config, and the `claude` program to run.
pub struct ConnectEnv {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub claude: PathBuf,
}

impl ConnectEnv {
    pub fn of_this_user() -> Result<Self, String> {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .ok_or_else(|| "cannot find the user home directory".to_string())?;
        let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        Ok(ConnectEnv { home, codex_home, claude: PathBuf::from("claude") })
    }
}

pub fn parse(argv: &[String]) -> Result<ConnectArgs, String> {
    let mut client = None;
    let mut mcps = Vec::new();
    let mut replace = false;
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        if word == "--replace" {
            replace = true;
            continue;
        }
        if word == "--mcp" {
            let Some(name) = words.next() else {
                return Err(format!("--mcp needs an MCP name\n{USAGE}"));
            };
            if !is_valid_name(name) {
                return Err(format!("an MCP's name starts with a letter or digit, then letters, digits, _ or -, 63 at most\n{USAGE}"));
            }
            if !mcps.iter().any(|have| have == name) {
                mcps.push(name.clone());
            }
            continue;
        }
        if word.starts_with('-') {
            return Err(format!("unknown option\n{USAGE}"));
        }
        if client.is_some() {
            return Err(format!("connect takes one client\n{USAGE}"));
        }
        if !CLIENTS.contains(&word.as_str()) {
            let message = match nearest(word, &CLIENTS) {
                Some(near) => format!("unknown client; did you mean {near}?"),
                None => format!("a client is {}", CLIENTS.join(", ")),
            };
            return Err(format!("{message}\n{USAGE}"));
        }
        client = Some(word.clone());
    }
    let Some(client) = client else {
        return Err(USAGE.into());
    };
    Ok(ConnectArgs { client, mcps, replace })
}

/// Point one client at `mcps`. An empty list means every MCP the gateway has.
pub async fn connect_on(port: u16, started: bool, client: &str, mcps: &[String], replace: bool, env: &ConnectEnv) -> (Value, i32) {
    let gw = Gateway::at(port);
    let which = if mcps.is_empty() { ClientMcps::All } else { ClientMcps::Names(mcps.to_vec()) };
    let known = match gateway_names(&gw).await {
        Ok(names) => names,
        Err(error) => {
            return Report {
                mode: Mode::Connect,
                gateway: json!({ "port": port, "started": started }),
                items: vec![Item::new("client", client, Status::Failed).with("client", client).failed("client.gateway", &error)],
                warnings: Vec::new(),
                next: Vec::new(),
            }
            .finish();
        }
    };
    let mut warnings = Vec::new();
    let names = match resolve(&which, &[], &known, &mut warnings, client) {
        Ok(names) => names,
        Err(error) => {
            return Report {
                mode: Mode::Connect,
                gateway: json!({ "port": port, "started": started }),
                items: vec![Item::new("client", client, Status::Failed).with("client", client).failed("client.mcp", &error)],
                warnings,
                next: Vec::new(),
            }
            .finish();
        }
    };
    let items = write_client(
        &Write { gw: &gw, port, client, mode: Mode::Connect, replace, env },
        &names,
        &mut warnings,
    )
    .await;
    let mut report = Report {
        mode: Mode::Connect,
        gateway: json!({ "port": port, "started": started }),
        items,
        warnings,
        next: Vec::new(),
    };
    note_restart(&mut report);
    report.finish()
}

/// Manifest clients, after the other writes. Plan reads and does not write.
pub async fn sync(
    gw: &Gateway,
    port: u16,
    manifest: &Manifest,
    mode: Mode,
    env: &ConnectEnv,
    items: &mut Vec<Item>,
    warnings: &mut Vec<Warning>,
) {
    if manifest.clients.is_empty() {
        return;
    }
    let gateway = match gateway_names(gw).await {
        Ok(names) => names,
        Err(error) => {
            for name in manifest.clients.keys() {
                items.push(Item::new("client", name, Status::Failed).with("client", name.clone()).failed("client.gateway", &error));
            }
            return;
        }
    };
    let manifest_names: Vec<String> = manifest.mcps.keys().cloned().collect();
    for (client, entry) in &manifest.clients {
        let names = match resolve(&entry.mcps, &manifest_names, &gateway, warnings, client) {
            Ok(names) => names,
            Err(error) => {
                items.push(Item::new("client", client, Status::Failed).with("client", client.clone()).failed("client.mcp", &error));
                continue;
            }
        };
        items.extend(write_client(&Write { gw, port, client, mode, replace: false, env }, &names, warnings).await);
    }
}

pub fn note_restart(report: &mut Report) {
    let mut clients = std::collections::BTreeSet::new();
    for item in &report.items {
        if item.kind == "client" {
            if let Some(name) = item.extra.get("client").and_then(Value::as_str) {
                clients.insert(name.to_string());
            }
        }
    }
    for client in clients {
        report.next_step("human", format!("restart {client}"), "a client reads its MCP list at startup");
    }
}

fn resolve(which: &ClientMcps, manifest: &[String], gateway: &[String], warnings: &mut Vec<Warning>, client: &str) -> Result<Vec<String>, String> {
    let names = match which {
        ClientMcps::Manifest => manifest.to_vec(),
        ClientMcps::All => gateway.to_vec(),
        ClientMcps::Names(names) => {
            for name in names {
                if !manifest.iter().any(|have| have == name) && !gateway.iter().any(|have| have == name) {
                    return Err(format!("no such MCP: {name}"));
                }
            }
            names.clone()
        }
    };
    if names.is_empty() {
        warnings.push(Warning::new("clients.none", format!("clients.{client}"), "this client names no MCP"));
    }
    Ok(names)
}

#[derive(Copy, Clone)]
struct Write<'a> {
    gw: &'a Gateway,
    port: u16,
    client: &'a str,
    mode: Mode,
    replace: bool,
    env: &'a ConnectEnv,
}

async fn write_client(job: &Write<'_>, names: &[String], warnings: &mut Vec<Warning>) -> Vec<Item> {
    let Write { gw, port, client, mode, replace, env } = *job;
    if names.is_empty() {
        return Vec::new();
    }
    let writing = mode != Mode::Plan;
    let mut pending = Vec::new();
    let mut items = Vec::new();
    let listed = if client == "claude" { claude_list(&env.claude) } else { Ok(String::new()) };
    if client == "claude" {
        if let Err(error) = &listed {
            return names
                .iter()
                .map(|name| Item::new("client", name, Status::Failed).with("client", client).failed("client.program", error))
                .collect();
        }
    }
    for name in names {
        let exists = match client {
            "claude" => listed.as_ref().ok().is_some_and(|text| listed_name(text, name)),
            "codex" => file_has_line(&env.codex_home.join("config.toml"), &format!("[mcp_servers.{name}]")),
            "cursor" => cursor_has(&env.home.join(".cursor").join("mcp.json"), name),
            _ => false,
        };
        if exists && !replace {
            items.push(Item::new("client", name, Status::Unchanged).with("client", client));
        } else if !writing {
            items.push(Item::new("client", name, Status::Created).with("client", client));
        } else {
            pending.push((name.clone(), exists));
        }
    }
    if pending.is_empty() {
        return items;
    }
    let secret = match client_token(gw, client).await {
        Ok(secret) => secret,
        Err(error) => {
            for (name, _) in &pending {
                items.push(Item::new("client", name, Status::Failed).with("client", client).failed("client.token", &error));
            }
            return items;
        }
    };
    for (name, exists) in pending {
        let url = format!("http://127.0.0.1:{port}/mcp/{name}");
        let result = match client {
            "claude" => claude_write(&env.claude, &name, &url, &secret, replace && exists),
            "codex" => codex_write(&env.codex_home.join("config.toml"), &name, &url, &secret, replace && exists),
            "cursor" => cursor_write(&env.home.join(".cursor").join("mcp.json"), &name, &url, &secret, replace && exists),
            _ => Err("unknown client".into()),
        };
        items.push(match result {
            Ok(status) => Item::new("client", &name, status).with("client", client),
            Err(error) => {
                warnings.push(Warning::new("client.write", format!("clients.{client}.{name}"), error.clone()));
                Item::new("client", &name, Status::Failed).with("client", client).failed("client.write", &error)
            }
        });
    }
    items
}

async fn client_token(gw: &Gateway, client: &str) -> Result<String, String> {
    let label = format!("client-{client}");
    let listed = gw.get("/api/tokens").await?;
    let found = listed["tokens"].as_array().into_iter().flatten().find(|row| row["label"].as_str() == Some(label.as_str()));
    if let Some(row) = found {
        let id = row["id"].as_str().filter(|id| id.chars().all(|ch| ch.is_ascii_hexdigit())).ok_or("the client token has no id")?;
        let secret = gw.get(&format!("/api/tokens/{id}/secret")).await?;
        return secret["secret"].as_str().filter(|value| !value.is_empty()).map(str::to_string).ok_or_else(|| "the client token has no secret".into());
    }
    let created = gw.send(reqwest::Method::POST, "/api/tokens", Some(json!({ "label": label }))).await?;
    created["secret"].as_str().filter(|value| !value.is_empty()).map(str::to_string).ok_or_else(|| "the gateway minted a token without a secret".into())
}

async fn gateway_names(gw: &Gateway) -> Result<Vec<String>, String> {
    let listed = gw.get("/api/mcps").await?;
    Ok(listed["mcps"].as_array().into_iter().flatten().filter_map(|row| row["name"].as_str().map(str::to_string)).collect())
}

fn listed_name(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line == name || line.starts_with(&format!("{name}:")) || line.starts_with(&format!("{name} "))
    })
}

fn file_has_line(path: &Path, header: &str) -> bool {
    std::fs::read_to_string(path).ok().is_some_and(|text| text.lines().any(|line| line.trim() == header))
}

fn cursor_has(path: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&text).ok().is_some_and(|value| value["mcpServers"][name].is_object())
}

/// `Command` on Windows looks only for `.exe`. A shell finds `.cmd` and `.bat` in the same
/// directory first, which is what `claude` on PATH means, including the suite's stand-in.
fn resolve_program(program: &Path) -> PathBuf {
    if program.parent().is_some_and(|parent| !parent.as_os_str().is_empty()) {
        return program.to_path_buf();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return program.to_path_buf();
    };
    let name = program.to_string_lossy();
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    program.to_path_buf()
}

fn claude_list(program: &Path) -> Result<String, String> {
    let program = resolve_program(program);
    // Unlike add/remove, list does not accept --scope.
    let output = Command::new(&program)
        .args(["mcp", "list"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "claude is not on PATH".to_string())?;
    if !output.status.success() {
        return Err("claude mcp list failed".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "claude mcp list was not text".into())
}

fn claude_write(program: &Path, name: &str, url: &str, secret: &str, replace: bool) -> Result<Status, String> {
    let program = resolve_program(program);
    if replace {
        let status = Command::new(&program)
            .args(["mcp", "remove", name, "-s", "user"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "claude is not on PATH".to_string())?;
        if !status.success() {
            return Err("claude mcp remove failed".into());
        }
    }
    let header = format!("Authorization: Bearer {secret}");
    let status = Command::new(&program)
        .args(["mcp", "add", "--transport", "http", "-s", "user", name, url, "--header", &header])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "claude is not on PATH".to_string())?;
    if status.success() {
        Ok(if replace { Status::Updated } else { Status::Created })
    } else {
        Err("claude mcp add failed".into())
    }
}

fn codex_write(path: &Path, name: &str, url: &str, secret: &str, replace: bool) -> Result<Status, String> {
    let block = format!("[mcp_servers.{name}]\ntype = \"http\"\nurl = \"{url}\"\nhttp_headers = {{ \"Authorization\" = \"Bearer {secret}\" }}\n");
    let header = format!("[mcp_servers.{name}]");
    match rewrite_section(path, &header, &block, replace)? {
        None => Ok(Status::Unchanged),
        Some(()) => Ok(if replace { Status::Updated } else { Status::Created }),
    }
}

fn cursor_write(path: &Path, name: &str, url: &str, secret: &str, replace: bool) -> Result<Status, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    let mut root: Map<String, Value> = if text.trim().is_empty() {
        Map::new()
    } else {
        serde_json::from_str(&text).map_err(|_| "cursor mcp.json is not a JSON object".to_string())?
    };
    if !root.contains_key("mcpServers") {
        root.insert("mcpServers".into(), json!({}));
    }
    let present = {
        let servers = root.get_mut("mcpServers").and_then(Value::as_object_mut).ok_or_else(|| "cursor mcp.json mcpServers is not an object".to_string())?;
        let present = servers.contains_key(name);
        if !present || replace {
            servers.insert(
                name.to_string(),
                json!({ "type": "http", "url": url, "headers": { "Authorization": format!("Bearer {secret}") } }),
            );
        }
        present
    };
    if present && !replace {
        return Ok(Status::Unchanged);
    }
    let mut body = serde_json::to_string_pretty(&Value::Object(root)).map_err(|err| err.to_string())?;
    body.push('\n');
    write_atomic(path, &body)?;
    Ok(if present { Status::Updated } else { Status::Created })
}

/// None when the section is already there and this call does not replace it.
fn rewrite_section(path: &Path, header: &str, block: &str, replace: bool) -> Result<Option<()>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    let present = text.lines().any(|line| line.trim() == header);
    if present && !replace {
        return Ok(None);
    }
    let mut out = String::new();
    if present {
        let mut skipping = false;
        let mut wrote = false;
        for line in text.lines() {
            if line.trim() == header {
                skipping = true;
                if !wrote {
                    out.push_str(block);
                    wrote = true;
                }
                continue;
            }
            if skipping {
                if line.starts_with('[') {
                    skipping = false;
                } else {
                    continue;
                }
            }
            out.push_str(line.trim_end_matches('\r'));
            out.push('\n');
        }
    } else {
        out.push_str(&text);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
    }
    write_atomic(path, &out)?;
    Ok(Some(()))
}

fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    if let Err(err) = std::fs::write(&tmp, body) {
        return Err(format!("could not write {}: {err}", tmp.display()));
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("could not replace {}: {err}", path.display()))
        }
    }
}
