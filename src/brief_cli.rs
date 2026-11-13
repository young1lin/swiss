// Copyright 2026 young1lin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `swiss brief` - what this machine offers an agent, in one call (SPEC §lane.brief).
//!
//! An agent arriving on this machine does not know that a gateway serves MCP servers here, that
//! heavy work should go through the lane, which remote targets and tunnels exist, which apps are
//! managed, or whether this directory has a memory project. Each module answers one of those on
//! its own route; this reads them all at once and prints each section with the command that
//! uses it. The swiss-brief skill runs it as it loads.
//!
//! It is a briefing, not a gate: a gateway that is down, a plugin that is off, or a route that
//! fails still prints what is known and exits 0, so a skill that runs it never fails to load.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::daemon::{resolve_port, with_cli_key};

/// Every read gets this long; the brief is printed as a skill loads.
const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Rows past this in one section are counted, not listed.
const ROWS_MAX: usize = 20;
/// An MCP description is a hint, not the manual.
const DESCRIPTION_MAX: usize = 80;

/// What one route answered.
#[derive(Debug, Clone, PartialEq)]
enum Got {
    Up(Value),
    /// Answered, but not with the data: the plugin is off, or this build lacks the route.
    Off(String),
    /// No answer.
    Down,
}

#[derive(Debug, Clone, PartialEq)]
enum Memory {
    /// No memory store on this machine yet.
    NoStore,
    /// The store has no project for this directory.
    NoProject,
    Project { id: String, name: String },
    /// The store could not be read in time.
    Unread,
}

#[derive(Debug)]
struct Brief {
    port: u16,
    /// /health's build stamp; `None` when the gateway is not running.
    build: Option<Value>,
    lane: Got,
    mcps: Got,
    targets: Got,
    endpoints: Got,
    process: Got,
    tunnels: Got,
    memory: Memory,
    now: u64,
}

pub async fn main(argv: Vec<String>) -> i32 {
    let mut json = false;
    for arg in &argv {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" | "help" => {
                println!("usage: swiss brief [--json]\n\nwhat this machine offers an agent - the gateway's MCP servers, the lane for heavy work,\nremote targets, managed apps, tunnels and this directory's memory project - each with\nthe command that uses it. Always exits 0.");
                return 0;
            }
            other => {
                eprintln!("usage: swiss brief [--json]");
                eprintln!("unknown or misplaced: {other}");
                return 2;
            }
        }
    }
    let brief = gather().await;
    if json {
        match serde_json::to_string_pretty(&to_json(&brief)) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("cannot render the brief: {e}"),
        }
    } else {
        print!("{}", render(&brief));
    }
    0
}

async fn gather() -> Brief {
    let port = resolve_port();
    let base = format!("http://127.0.0.1:{port}");
    let client = crate::daemon::loopback_client();
    let cwd = std::env::current_dir().ok();
    let memory = memory_for(cwd.as_deref());
    let build = async {
        let res = client.get(format!("{base}/health")).timeout(READ_TIMEOUT).send().await.ok()?;
        let body: Value = res.json().await.ok()?;
        Some(body.get("build").cloned().unwrap_or(Value::Null))
    };
    let (build, memory) = tokio::join!(build, memory);
    let mut brief = Brief {
        port,
        build,
        lane: Got::Down,
        mcps: Got::Down,
        targets: Got::Down,
        endpoints: Got::Down,
        process: Got::Down,
        tunnels: Got::Down,
        memory,
        now: swiss_core::util::now_ms(),
    };
    if brief.build.is_none() {
        return brief;
    }
    let read = |path: &'static str| fetch(&client, &base, path);
    let (lane, mcps, targets, endpoints, process, tunnels) = tokio::join!(
        read("/api/lane"),
        read("/api/mcps"),
        read("/api/remote/targets"),
        read("/api/remote/endpoints"),
        read("/api/process"),
        read("/api/tunnels"),
    );
    brief.lane = lane;
    brief.mcps = mcps;
    brief.targets = targets;
    brief.endpoints = endpoints;
    brief.process = process;
    brief.tunnels = tunnels;
    brief
}

async fn fetch(client: &reqwest::Client, base: &str, path: &str) -> Got {
    let Ok(res) = with_cli_key(client.get(format!("{base}{path}"))).timeout(READ_TIMEOUT).send().await else {
        return Got::Down;
    };
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Got::Up(body);
    }
    Got::Off(match status.as_u16() {
        503 => "off".to_string(),
        404 => "not in this build".to_string(),
        code => format!("{code} {}", body["error"].as_str().unwrap_or("")).trim().to_string(),
    })
}

/// The memory project this directory belongs to, read offline from the store the gateway
/// shares. Never creates the store or a project.
async fn memory_for(cwd: Option<&Path>) -> Memory {
    use swiss_memory::service::{MemoryService, Target};
    let path = MemoryService::default_path();
    let Some(cwd) = cwd else { return Memory::Unread };
    if !path.exists() {
        return Memory::NoStore;
    }
    let service = MemoryService::at(&path);
    let target = Target::dir(cwd.display().to_string());
    let found = match tokio::time::timeout(READ_TIMEOUT, service.resolve(&target)).await {
        Ok(Ok(Some(project))) => Memory::Project { id: project.id, name: project.name },
        Ok(Ok(None)) => Memory::NoProject,
        _ => Memory::Unread,
    };
    service.close().await;
    found
}

fn rows<'a>(body: &'a Value, key: &str) -> Vec<&'a Value> {
    body[key].as_array().map(|list| list.iter().collect()).unwrap_or_default()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// A section's heading for a route that did not answer with data, or `None` when it did.
fn unavailable(got: &Got) -> Option<String> {
    match got {
        Got::Up(_) => None,
        Got::Off(why) => Some(why.clone()),
        Got::Down => Some("no answer".to_string()),
    }
}

fn render(b: &Brief) -> String {
    let mut out = String::new();
    let mut line = |s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    let Some(build) = &b.build else {
        line(&format!("# swiss: the gateway on port {} is not running", b.port));
        line("-> `swiss start` brings it up; until then MCP servers, the lane, remote targets and tunnels are unavailable.");
        line("");
        for l in memory_lines(&b.memory) {
            line(&l);
        }
        return out;
    };
    let mut stamp = format!("v{}", build["version"].as_str().unwrap_or("?"));
    if let Some(hash) = build["hash"].as_str().filter(|h| !h.is_empty()) {
        stamp.push(' ');
        stamp.extend(hash.chars().take(8));
    }
    line(&format!("# swiss gateway on 127.0.0.1:{} ({stamp})", b.port));
    line("");

    // The lane first: it is the one an agent must act on before its next build.
    match &b.lane {
        Got::Up(body) => {
            line("## Heavy work: the lane");
            for l in crate::lane_cli::status_lines(body, b.now, 3) {
                line(&l);
            }
            line("-> Run builds and test suites through it: `swiss lane run -- cargo test -j 2`. It waits for a free slot, then runs the command here with those caps; the last line names the full log. Long runs belong in a background shell.");
        }
        other => {
            line(&format!("## Heavy work: the lane - {}", unavailable(other).unwrap_or_default()));
            line("-> `swiss lane run -- <cmd>` still works: it runs the command at once and says the lane was skipped.");
        }
    }
    line("");

    match &b.mcps {
        Got::Up(body) => {
            let mcps = rows(body, "mcps");
            line(&format!("## MCP servers ({})", mcps.len()));
            for mcp in mcps.iter().take(ROWS_MAX) {
                let description = swiss_plugins::lane::queue::clip(text(mcp, "description"), DESCRIPTION_MAX);
                let state = text(mcp, "state");
                let mut row = format!("- {} [{state}]", text(mcp, "name"));
                if !description.is_empty() {
                    row.push_str(&format!(" {description}"));
                }
                line(&row);
            }
            more(&mut line, mcps.len());
            line(&format!("-> A connected client already has these as tools; `swiss connect claude|codex|cursor` points one at http://127.0.0.1:{}/mcp/<name>.", b.port));
        }
        other => line(&format!("## MCP servers - {}", unavailable(other).unwrap_or_default())),
    }
    line("");

    match &b.targets {
        Got::Up(body) => {
            let targets = rows(body, "targets");
            line(&format!("## Remote targets ({})", targets.len()));
            let endpoint_state = |id: &str| -> String {
                match &b.endpoints {
                    Got::Up(eps) => rows(eps, "endpoints")
                        .into_iter()
                        .find(|e| text(e, "id") == id)
                        .map(|e| text(e, "state").to_string())
                        .unwrap_or_else(|| "no endpoint".to_string()),
                    _ => "?".to_string(),
                }
            };
            for target in targets.iter().take(ROWS_MAX) {
                let label = text(target, "label");
                let label = if label.is_empty() || label == text(target, "id") { String::new() } else { format!(" ({label})") };
                line(&format!(
                    "- {}{label} [{}] root {}",
                    text(target, "id"),
                    endpoint_state(text(target, "endpoint")),
                    text(target, "workspaceRoot"),
                ));
            }
            more(&mut line, targets.len());
            if !targets.is_empty() {
                line("-> `swiss remote brief <target>` before the first command there, then `swiss remote exec <target> -- <cmd>` (skill: swiss-remote).");
            }
        }
        other => line(&format!("## Remote targets - {}", unavailable(other).unwrap_or_default())),
    }
    line("");

    match &b.process {
        Got::Up(body) => {
            let apps = rows(body, "apps");
            line(&format!("## Managed apps ({})", apps.len()));
            for app in apps.iter().take(ROWS_MAX) {
                line(&format!("- {}", crate::process_cli::status_line(app).trim_end()));
            }
            more(&mut line, apps.len());
            if apps.is_empty() {
                line("none; an app is added on the Process page");
            } else {
                line("-> `swiss process start|stop|restart <name>` acts and waits for the outcome; `swiss process logs <name> [-n N] [-f]` reads its output.");
            }
        }
        other => line(&format!("## Managed apps - {}", unavailable(other).unwrap_or_default())),
    }
    line("");

    match &b.tunnels {
        Got::Up(body) => {
            let live: Vec<&Value> = rows(body, "rules").into_iter().filter(|r| r["enabled"].as_bool() == Some(true)).collect();
            line(&format!("## Tunnels ({} enabled)", live.len()));
            for rule in live.iter().take(ROWS_MAX) {
                line(&format!(
                    "- {}: 127.0.0.1:{} -> {}:{} via {} [{}]",
                    text(rule, "name"),
                    rule["localPort"].as_u64().unwrap_or(0),
                    text(rule, "targetHost"),
                    rule["targetPort"].as_u64().unwrap_or(0),
                    text(rule, "connectionName"),
                    text(rule, "state"),
                ));
            }
            more(&mut line, live.len());
            if !live.is_empty() {
                line("-> Connect to the local port; the tunnel carries it to the far side.");
            }
        }
        other => line(&format!("## Tunnels - {}", unavailable(other).unwrap_or_default())),
    }
    line("");

    for l in memory_lines(&b.memory) {
        line(&l);
    }
    out
}

fn more(line: &mut impl FnMut(&str), total: usize) {
    if total > ROWS_MAX {
        line(&format!("... and {} more", total - ROWS_MAX));
    }
}

fn memory_lines(memory: &Memory) -> Vec<String> {
    let head = "## Memory".to_string();
    match memory {
        Memory::Project { id, name } => vec![
            head,
            format!("this directory is in project \"{name}\" ({id})"),
            "-> `swiss memory brief` for what the project knows (skill: swiss-memory).".to_string(),
        ],
        Memory::NoProject => vec![
            head,
            "this directory is in no memory project".to_string(),
            "-> `swiss memory init` starts one here (skill: swiss-memory).".to_string(),
        ],
        Memory::NoStore => vec![head, "no memory store on this machine yet".to_string()],
        Memory::Unread => vec![head, "the memory store could not be read just now".to_string()],
    }
}

fn got_json(got: &Got) -> Value {
    match got {
        Got::Up(body) => body.clone(),
        Got::Off(why) => json!({ "unavailable": why }),
        Got::Down => json!({ "unavailable": "no answer" }),
    }
}

fn to_json(b: &Brief) -> Value {
    let mut out = Map::new();
    out.insert("port".into(), json!(b.port));
    out.insert("running".into(), json!(b.build.is_some()));
    if let Some(build) = &b.build {
        out.insert("build".into(), build.clone());
        out.insert("lane".into(), got_json(&b.lane));
        // The MCP rows trimmed to what an agent acts on.
        out.insert(
            "mcps".into(),
            match &b.mcps {
                Got::Up(body) => Value::Array(
                    rows(body, "mcps")
                        .into_iter()
                        .map(|m| json!({ "name": m["name"], "state": m["state"], "description": m["description"] }))
                        .collect(),
                ),
                other => got_json(other),
            },
        );
        out.insert("targets".into(), got_json(&b.targets));
        out.insert("endpoints".into(), got_json(&b.endpoints));
        out.insert("process".into(), got_json(&b.process));
        out.insert(
            "tunnels".into(),
            match &b.tunnels {
                Got::Up(body) => Value::Array(
                    rows(body, "rules")
                        .into_iter()
                        .filter(|r| r["enabled"].as_bool() == Some(true))
                        .map(|r| {
                            json!({
                                "name": r["name"], "localPort": r["localPort"], "targetHost": r["targetHost"],
                                "targetPort": r["targetPort"], "connection": r["connectionName"], "state": r["state"],
                            })
                        })
                        .collect(),
                ),
                other => got_json(other),
            },
        );
    }
    out.insert(
        "memory".into(),
        match &b.memory {
            Memory::Project { id, name } => json!({ "project": { "id": id, "name": name } }),
            Memory::NoProject => json!({ "project": null }),
            Memory::NoStore => json!({ "store": false }),
            Memory::Unread => json!({ "unavailable": "the store could not be read" }),
        },
    );
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up() -> Brief {
        Brief {
            port: 19999,
            build: Some(json!({ "version": "0.2.2", "hash": "eb9e0208abcdef" })),
            lane: Got::Up(json!({
                "capacity": 1, "env": { "CARGO_BUILD_JOBS": "2" },
                "running": [{ "id": 4, "by": "codex", "cmd": "cargo test", "startedAt": 990_000 }],
                "waiting": [], "recent": [],
            })),
            mcps: Got::Up(json!({ "mcps": [
                { "name": "memory", "state": "running", "description": "Project memory" },
                { "name": "pg", "state": "stopped", "description": "" },
            ] })),
            targets: Got::Up(json!({ "targets": [
                { "id": "build", "label": "Build box", "endpoint": "ep-1", "workspaceRoot": "/srv/work" },
            ] })),
            endpoints: Got::Up(json!({ "endpoints": [{ "id": "ep-1", "label": "x", "state": "online" }] })),
            process: Got::Off("off".into()),
            tunnels: Got::Up(json!({ "rules": [
                { "name": "pg", "enabled": true, "localPort": 15432, "targetHost": "db", "targetPort": 5432, "connectionName": "jump", "state": "running" },
                { "name": "old", "enabled": false, "localPort": 1, "targetHost": "x", "targetPort": 1, "connectionName": "y", "state": "stopped" },
            ] })),
            memory: Memory::Project { id: "p-1".into(), name: "swiss".into() },
            now: 1_000_000,
        }
    }

    #[test]
    fn every_section_says_what_is_there_and_how_to_use_it() {
        let text = render(&up());
        assert!(text.starts_with("# swiss gateway on 127.0.0.1:19999 (v0.2.2 eb9e0208)\n"), "{text}");
        assert!(text.contains("## Heavy work: the lane\nlane: 1 slot; caps CARGO_BUILD_JOBS=2\nrunning  #4"), "{text}");
        assert!(text.contains("swiss lane run -- cargo test -j 2"), "{text}");
        assert!(text.contains("- memory [running] Project memory\n- pg [stopped]\n"), "{text}");
        assert!(text.contains("http://127.0.0.1:19999/mcp/<name>"), "{text}");
        assert!(text.contains("- build (Build box) [online] root /srv/work"), "{text}");
        assert!(text.contains("## Managed apps - off"), "{text}");
        assert!(text.contains("## Tunnels (1 enabled)\n- pg: 127.0.0.1:15432 -> db:5432 via jump [running]"), "{text}");
        assert!(!text.contains("old:"), "a disabled rule is not offered");
        assert!(text.contains("project \"swiss\" (p-1)"), "{text}");
    }

    #[test]
    fn a_gateway_that_is_down_still_briefs_memory() {
        let mut brief = up();
        brief.build = None;
        brief.memory = Memory::NoProject;
        let text = render(&brief);
        assert!(text.starts_with("# swiss: the gateway on port 19999 is not running\n-> `swiss start`"), "{text}");
        assert!(text.contains("swiss memory init"), "{text}");
        assert!(!text.contains("## MCP"), "{text}");
    }

    #[test]
    fn a_lane_that_is_off_says_the_command_still_runs() {
        let mut brief = up();
        brief.lane = Got::Off("off".into());
        let text = render(&brief);
        assert!(text.contains("## Heavy work: the lane - off\n-> `swiss lane run -- <cmd>` still works"), "{text}");
    }

    #[test]
    fn long_lists_are_counted_not_dumped() {
        let mut brief = up();
        let many: Vec<Value> = (0..ROWS_MAX + 3).map(|n| json!({ "name": format!("m{n}"), "state": "running" })).collect();
        brief.mcps = Got::Up(json!({ "mcps": many }));
        let text = render(&brief);
        assert!(text.contains(&format!("## MCP servers ({})", ROWS_MAX + 3)));
        assert!(text.contains("... and 3 more"), "{text}");
    }

    #[test]
    fn the_json_form_trims_rows_and_marks_what_is_unavailable() {
        let value = to_json(&up());
        assert_eq!(value["running"], true);
        assert_eq!(value["mcps"][0], json!({ "name": "memory", "state": "running", "description": "Project memory" }));
        assert_eq!(value["process"], json!({ "unavailable": "off" }));
        assert_eq!(value["tunnels"].as_array().unwrap().len(), 1);
        assert_eq!(value["memory"]["project"]["name"], "swiss");
        let mut down = up();
        down.build = None;
        let value = to_json(&down);
        assert_eq!(value["running"], false);
        assert!(value.get("lane").is_none());
    }
}
