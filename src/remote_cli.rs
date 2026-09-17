/*
 * Copyright 2026 The swiss authors
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

//! The `swiss remote` and `swiss run` command lines (docs/32 SS26).
//!
//! A thin, honest client of the HTTP surface: every mutation goes through
//! /api/remote, every execution goes through /api/runs, and the command streams
//! the live output back through the cursor API exactly like a panel page would.
//! No second job system, no local exec engine - the CLI is just the agent-facing
//! face of the same gateway.
//!
//! THE HARD RULE (docs/32 SS26): everything after a bare `--` is ARGV for the far
//! side, never parsed as a flag. `swiss remote exec build -- make -j8 -- -k` must
//! send `["make","-j8","--","-k"]` untouched.

use serde_json::{json, Value};

use crate::daemon::{read_gateway_token, resolve_port};

/// How long one exec may run unless --timeout, the project binding, or the target
/// row says otherwise (docs/32 SS26: CLI > binding > target > this).
const DEFAULT_EXEC_TIMEOUT_MS: u64 = 2 * 60 * 60 * 1000;
/// The submit route ceiling (docs/10 SS7); refusing here is a better error than
/// the same refusal from the API after a round trip.
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
/// The parsed `swiss remote ...` command, after `--` splitting.
#[derive(Debug, PartialEq, Default, Clone)]
pub struct RemoteArgs {
    pub sub: String,
    /// Positionals before `--` (target/action names, paths).
    pub words: Vec<String>,
    /// Everything after the bare `--`: ARGV, passed through UNTOUCHED.
    pub passthrough: Vec<String>,
    pub port: Option<u16>,
    pub target: Option<String>,
    pub endpoint: Option<String>,
    pub root: Option<String>,
    pub label: Option<String>,
    pub caps: Option<String>,
    pub cwd: Option<String>,
    pub to: Option<String>,
    pub source: Option<String>,
    pub exclude: Vec<String>,
    pub env: Vec<(String, String)>,
    pub timeout: Option<String>,
    pub detach: bool,
    pub follow: bool,
    pub verbose: bool,
    pub json: bool,
    pub bad_port: bool,
    pub unknown: Vec<String>,
}

/// Split argv at the FIRST bare `--`. Before it: flags and words; after it: ARGV
/// for the far side, never interpreted here. This is the contract the whole
/// command hangs on, so it is tested from both sides.
pub fn split_passthrough(argv: &[String]) -> (&[String], &[String]) {
    match argv.iter().position(|a| a == "--") {
        Some(at) => (&argv[..at], &argv[at + 1..]),
        None => (argv, &[]),
    }
}
/// Parse the pre-`--` half. Mirrors cli.rs strictness: unknown flags are collected,
/// a bad port value is a refusal, a flag value never eats another flag.
pub fn parse(argv: &[String]) -> RemoteArgs {
    let mut a = RemoteArgs::default();
    let mut words = Vec::new();
    let mut i = 0usize;
    while i < argv.len() {
        let arg = &argv[i];
        if !arg.starts_with('-') {
            words.push(arg.clone());
            i += 1;
            continue;
        }
        let (flag, inline) = match arg.find('=') {
            Some(eq) => (&arg[..eq], Some(arg[eq + 1..].to_string())),
            None => (arg.as_str(), None),
        };
        let value = |argv: &[String], i: &mut usize, inline: &Option<String>| -> String {
            if let Some(v) = inline {
                return v.clone();
            }
            if *i + 1 < argv.len() && !argv[*i + 1].starts_with('-') {
                *i += 1;
                return argv[*i].clone();
            }
            String::new()
        };
        match flag {
            "-p" | "--port" => match crate::port::as_listen_port(&value(argv, &mut i, &inline)) {
                Some(port) => a.port = Some(port),
                None => a.bad_port = true,
            },
            "--target" => a.target = Some(value(argv, &mut i, &inline)),
            "--endpoint" => a.endpoint = Some(value(argv, &mut i, &inline)),
            "--root" => a.root = Some(value(argv, &mut i, &inline)),
            "--label" => a.label = Some(value(argv, &mut i, &inline)),
            "--caps" => a.caps = Some(value(argv, &mut i, &inline)),
            "--cwd" => a.cwd = Some(value(argv, &mut i, &inline)),
            "--to" => a.to = Some(value(argv, &mut i, &inline)),
            "--source" => a.source = Some(value(argv, &mut i, &inline)),
            "--exclude" => a.exclude.push(value(argv, &mut i, &inline)),
            "--env" => {
                let pair = value(argv, &mut i, &inline);
                match pair.split_once('=') {
                    Some((k, v)) if !k.is_empty() => a.env.push((k.to_string(), v.to_string())),
                    _ => a.unknown.push(format!("--env \"{pair}\" needs NAME=VALUE")),
                }
            }
            "--timeout" => a.timeout = Some(value(argv, &mut i, &inline)),
            "--detach" => a.detach = true,
            "--follow" | "-f" => a.follow = true,
            "--verbose" => a.verbose = true,
            "--json" => a.json = true,
            other => a.unknown.push(other.to_string()),
        }
        i += 1;
    }
    a.sub = words.first().cloned().unwrap_or_default();
    a.words = words.into_iter().skip(1).collect();
    a
}

/// Parse a duration the way a human writes one: "90s", "45m", "2h", or a bare
/// number of seconds. The only place a deadline is spelled, so it is tested.
pub fn parse_duration(raw: &str) -> Option<u64> {
    let (number, unit) = raw.split_at(raw.len().saturating_sub(1));
    let (digits, mul) = match unit {
        "s" => (number, 1000u64),
        "m" => (number, 60 * 1000),
        "h" => (number, 60 * 60 * 1000),
        _ => (raw, 1000), // no unit: seconds
    };
    let n: u64 = digits.parse().ok()?;
    Some(n.saturating_mul(mul))
}
/// One gateway client: base URL plus the bearer token the admin API wants.
struct Gateway {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Gateway {
    fn at(port: u16) -> Self {
        Gateway {
            client: reqwest::Client::new(),
            base: format!("http://127.0.0.1:{port}"),
            token: read_gateway_token(),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        self.send(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    async fn delete(&self, path: &str) -> Result<Value, String> {
        self.send(reqwest::Method::DELETE, path, None).await
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("cannot reach the gateway: {err}"))?;
        let status = response.status();
        let value: Value = match response.json().await {
            Ok(v) => v,
            Err(_) => Value::Null,
        };
        if !status.is_success() {
            let message = value["error"].as_str().unwrap_or("no detail");
            return Err(format!("{} {message}", status.as_u16()));
        }
        Ok(value)
    }
}
/// The entry from cli.rs: argv is everything AFTER `swiss remote` / `swiss run`.
pub async fn main(argv: Vec<String>) -> i32 {
    run(&argv).await
}

async fn run(argv: &[String]) -> i32 {
    let (head, passthrough) = split_passthrough(argv);
    let mut a = parse(head);
    a.passthrough = passthrough.to_vec();
    if a.bad_port {
        eprintln!("--port takes a number between 1 and 65535");
        return 1;
    }
    if !a.unknown.is_empty() {
        eprintln!("unknown option: {}", a.unknown.join(", "));
        eprintln!("{}", usage_text());
        return 1;
    }
    if a.sub.is_empty() || a.sub == "help" {
        println!("{}", usage_text());
        return if a.sub.is_empty() { 1 } else { 0 };
    }
    let gw = Gateway::at(a.port.unwrap_or_else(resolve_port));
    match a.sub.as_str() {
        "endpoints" => cmd_endpoints(gw, &a).await,
        "targets" => cmd_targets(gw, &a).await,
        "target" => cmd_target(gw, &a).await,
        "resolve" => cmd_resolve(gw, &a).await,
        "exec" => cmd_exec(gw, &a).await,
        "sync" => cmd_sync(gw, &a).await,
        "pull" => cmd_pull(gw, &a).await,
        other => {
            eprintln!("unknown remote subcommand: {other}");
            eprintln!("{}", usage_text());
            1
        }
    }
}

pub fn usage_text() -> String {
    let mut s = String::from("usage: swiss remote <command> [options]\n\n");
    for (cmd, text) in [
        ("endpoints", "list the endpoints the transport can serve"),
        ("targets", "list configured targets"),
        (
            "target add <id> --endpoint <id> --root <path> [--caps exec,sync]",
            "add a target",
        ),
        (
            "target set <id> [--endpoint] [--root] [--caps] [--label]",
            "edit a target",
        ),
        ("target remove <id>", "remove a target"),
        (
            "resolve [name]",
            "show what a name resolves to (target or project action)",
        ),
        (
            "exec [name] [--cwd DIR] [--env NAME=VALUE]... [--timeout 2h] [--detach] [--] ARGV...",
            "run a command on a target",
        ),
        (
            "sync [name] [--source DIR] [--exclude PATTERN]... [--verbose]",
            "upload the local tree",
        ),
        ("pull [name] <path> [--to LOCAL]", "download one file"),
    ] {
        s.push_str(&format!("  {cmd:<70} {text}\n"));
    }
    s.push_str(
        "  run status <id> | run logs <id> [-f] | run cancel <id>              the run surface\n",
    );
    s.push_str(
        "\nEverything after a bare -- is ARGV for the far side, passed through untouched.\n",
    );
    s.push_str(
        "Timeout precedence: --timeout > project binding > target default > 2h (max 24h).\n",
    );
    s
}
// --- the read-only listings ---------------------------------------------------------------

async fn cmd_endpoints(gw: Gateway, a: &RemoteArgs) -> i32 {
    match gw.get("/api/remote/endpoints").await {
        Ok(value) => {
            if a.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                let presence = value["presence"].as_str().unwrap_or("?");
                println!("transport: {presence}");
                for e in value["endpoints"].as_array().unwrap_or(&Vec::new()) {
                    println!(
                        "  {:<24} {:<12} {}",
                        e["id"].as_str().unwrap_or("?"),
                        e["state"].as_str().unwrap_or("?"),
                        e["label"].as_str().unwrap_or(""),
                    );
                }
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

async fn cmd_targets(gw: Gateway, a: &RemoteArgs) -> i32 {
    match gw.get("/api/remote/targets").await {
        Ok(value) => {
            if a.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                for t in value["targets"].as_array().unwrap_or(&Vec::new()) {
                    let caps = t["capabilities"]
                        .as_array()
                        .map(|c| {
                            c.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();
                    println!(
                        "  {:<16} {:<16} {:<10} {}",
                        t["id"].as_str().unwrap_or("?"),
                        t["endpoint"].as_str().unwrap_or("?"),
                        caps,
                        t["workspaceRoot"].as_str().unwrap_or("?"),
                    );
                }
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

// --- target CRUD --------------------------------------------------------------------------

fn caps_value(raw: Option<&str>) -> Vec<String> {
    let raw = raw.unwrap_or("exec");
    raw.split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

async fn cmd_target(gw: Gateway, a: &RemoteArgs) -> i32 {
    let Some(verb) = a.words.first().cloned() else {
        eprintln!("usage: swiss remote target add|set|remove ...");
        return 1;
    };
    let Some(id) = a.words.get(1).cloned() else {
        eprintln!("target {verb} needs an id");
        return 1;
    };
    match verb.as_str() {
        "add" => {
            let (Some(endpoint), Some(root)) = (&a.endpoint, &a.root) else {
                eprintln!("target add needs --endpoint <tunnels connection id> and --root <absolute POSIX path>");
                return 1;
            };
            let body = json!({
                "id": id,
                "label": a.label.clone().unwrap_or_else(|| id.clone()),
                "endpoint": endpoint,
                "workspaceRoot": root,
                "shell": "posix",
                "capabilities": caps_value(a.caps.as_deref()),
            });
            report(
                gw.post("/api/remote/targets", body).await,
                &format!("added {id}"),
                a.json,
            )
        }
        "set" => {
            // Merge onto the current row so a one-field edit is a one-flag command.
            let current = match gw.get(&format!("/api/remote/targets/{id}")).await {
                Ok(row) => row,
                Err(err) => {
                    eprintln!("{err}");
                    return 1;
                }
            };
            let mut body = current.clone();
            if let Some(endpoint) = &a.endpoint {
                body["endpoint"] = json!(endpoint);
            }
            if let Some(root) = &a.root {
                body["workspaceRoot"] = json!(root);
            }
            if let Some(label) = &a.label {
                body["label"] = json!(label);
            }
            if let Some(caps) = &a.caps {
                body["capabilities"] = json!(caps_value(Some(caps)));
            }
            report(
                gw.post(&format!("/api/remote/targets/{id}"), body).await,
                &format!("updated {id}"),
                a.json,
            )
        }
        "remove" | "rm" | "delete" => report(
            gw.delete(&format!("/api/remote/targets/{id}")).await,
            &format!("removed {id}"),
            a.json,
        ),
        other => {
            eprintln!("unknown target verb: {other} (add, set, remove)");
            1
        }
    }
}

fn report(result: Result<Value, String>, done: &str, as_json: bool) -> i32 {
    match result {
        Ok(value) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                println!("{done}");
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
// --- name resolution: explicit target > project action > binding default --------------------

/// What one exec/sync/pull name resolved to. The binding is discovered from the
/// working directory, exactly like git finds .git (docs/32 SS24).
pub struct Resolved {
    pub target: String,
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
    pub via: String,
}

/// The deadline chain (docs/32 SS26): --timeout > project action > target row >
/// 2h, capped at the submit route's 24h ceiling.
fn deadline(cli: Option<&str>, action: Option<u64>, target_default: Option<u64>) -> Option<u64> {
    let picked = parse_duration(cli.unwrap_or(""))
        .or(action)
        .or(target_default)
        .unwrap_or(DEFAULT_EXEC_TIMEOUT_MS);
    Some(picked.min(MAX_TIMEOUT_MS))
}

async fn resolve_name(gw: &Gateway, a: &RemoteArgs) -> Result<Resolved, String> {
    let binding =
        swiss_remote::project::ProjectBinding::discover(std::path::Path::new(".")).unwrap_or(None);
    let targets = gw.get("/api/remote/targets").await?;
    let rows: Vec<(String, Option<u64>)> = targets["targets"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|t| {
                    let id = t["id"].as_str().map(str::to_string)?;
                    Some((id, t["defaultTimeoutMs"].as_u64()))
                })
                .collect()
        })
        .unwrap_or_default();
    let target_default = |id: &str| rows.iter().find(|r| r.0 == id).and_then(|r| r.1);
    // 1. --target always wins: an explicit flag is a deliberate choice.
    if let Some(target) = &a.target {
        return Ok(Resolved {
            target: target.clone(),
            cwd: a.cwd.clone(),
            timeout_ms: deadline(a.timeout.as_deref(), None, target_default(target)),
            via: "--target".into(),
        });
    }
    let name = a
        .words
        .first()
        .cloned()
        .or_else(|| binding.as_ref().and_then(|b| b.default_target.clone()));
    let Some(name) = name else {
        return Err(
            "no target: pass --target, name one, or set defaultTarget in .swiss/remote.json".into(),
        );
    };
    // 2. A name that IS a target id.
    if let Some((_, default)) = rows.iter().find(|r| r.0 == name) {
        return Ok(Resolved {
            target: name.clone(),
            cwd: a.cwd.clone(),
            timeout_ms: deadline(a.timeout.as_deref(), None, *default),
            via: format!("target {name}"),
        });
    }
    // 3. A project action name: its target, workspace and deadline apply unless a
    //    flag overrode them here.
    if let Some(binding) = &binding {
        if let Some(action) = binding.action(&name) {
            return Ok(Resolved {
                target: action.target.clone(),
                cwd: a.cwd.clone().or_else(|| action.workspace.clone()),
                timeout_ms: deadline(
                    a.timeout.as_deref(),
                    action.timeout_ms,
                    target_default(&action.target),
                ),
                via: format!("project action {name}"),
            });
        }
    }
    let ids: Vec<String> = rows.into_iter().map(|r| r.0).collect();
    Err(format!(
        "no target or project action named {name:?} (known targets: {})",
        ids.join(", "),
    ))
}

async fn cmd_resolve(gw: Gateway, a: &RemoteArgs) -> i32 {
    match resolve_name(&gw, a).await {
        Ok(resolved) => {
            if a.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "target": resolved.target,
                        "cwd": resolved.cwd,
                        "timeoutMs": resolved.timeout_ms,
                        "via": resolved.via,
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("target: {}", resolved.target);
                println!("via:   {}", resolved.via);
                if let Some(cwd) = &resolved.cwd {
                    println!("cwd:   {cwd}");
                }
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
// --- exec: submit, stream, exit with the REMOTE exit code ------------------------------------

async fn cmd_exec(gw: Gateway, a: &RemoteArgs) -> i32 {
    // The name may be a target or a project action; consume it only when it WAS one.
    let named = a.words.first().cloned();
    let mut lookup = RemoteArgs {
        words: named.clone().into_iter().collect(),
        ..a.clone()
    };
    if lookup.target.is_some() {
        lookup.words.clear();
    }
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    // ARGV: everything after `--`, or - failing that - the words after the name.
    // The passthrough is the contract; the fallback is for the no-name spelling.
    let mut argv: Vec<String> = a.passthrough.clone();
    if argv.is_empty() {
        let skip_name = if named.is_some() && resolved.via != "--target" {
            1
        } else {
            0
        };
        argv = a.words.iter().skip(skip_name).cloned().collect();
    }
    if argv.is_empty() {
        eprintln!("no command: put it after a bare -- (swiss remote exec build -- make -j8)");
        return 1;
    }
    let mut input = json!({ "target": resolved.target, "argv": argv });
    if let Some(cwd) = &resolved.cwd {
        input["cwd"] = json!(cwd);
    }
    if !a.env.is_empty() {
        let env: serde_json::Map<String, Value> =
            a.env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        input["env"] = Value::Object(env);
    }
    submit_and_stream(
        &gw,
        "remote.exec",
        &format!("exec {}", resolved.target),
        input,
        resolved.timeout_ms,
        a,
    )
    .await
}
/// Submit one remote run and, unless --detach, stream its live output through the
/// cursor API until it is terminal - then exit with the REMOTE exit code (docs/32
/// SS26): `swiss remote exec build -- false` exits 1 because false did.
async fn submit_and_stream(
    gw: &Gateway,
    action: &str,
    label: &str,
    input: Value,
    timeout_ms: Option<u64>,
    a: &RemoteArgs,
) -> i32 {
    let mut body = json!({ "action": action, "input": input, "label": label });
    if let Some(ms) = timeout_ms {
        body["timeoutMs"] = json!(ms);
    }
    let submitted = match gw.post("/api/runs", body).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let run_id = submitted["runId"].as_u64().unwrap_or_default();
    if a.detach {
        if a.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&submitted).unwrap_or_default()
            );
        } else {
            println!("run {run_id} submitted (swiss run logs {run_id} -f)");
        }
        return 0;
    }
    stream_run(gw, run_id, true).await
}

/// The shared follow loop: poll the output cursor, print what arrived, and when
/// the run is terminal fetch the row and translate its outcome into an exit code.
async fn stream_run(gw: &Gateway, run_id: u64, print_from_start: bool) -> i32 {
    use std::io::Write as _;
    let mut cursor: u64 = 0;
    loop {
        let chunk = match gw
            .get(&format!(
                "/api/runs/{run_id}/output?after={cursor}&max=131072"
            ))
            .await
        {
            Ok(value) => value,
            Err(err) => {
                eprintln!("{err}");
                return 1;
            }
        };
        if print_from_start {
            let text = chunk["output"].as_str().unwrap_or("");
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        cursor = chunk["nextCursor"].as_u64().unwrap_or(cursor);
        if chunk["terminal"].as_bool().unwrap_or(false) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let run = match gw.get(&format!("/api/runs/{run_id}?output=0")).await {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let state = run["state"].as_str().unwrap_or("");
    if state == "succeeded" {
        return 0;
    }
    let exit = run["exitCode"].as_i64();
    if let Some(code) = exit {
        return code.clamp(1, 255) as i32;
    }
    if state == "canceled" {
        eprintln!("run {run_id} was canceled");
    } else if state == "timeout" {
        eprintln!("run {run_id} timed out");
    } else if let Some(err) = run["error"].as_str() {
        eprintln!("{err}");
    }
    1
}
async fn cmd_sync(gw: Gateway, a: &RemoteArgs) -> i32 {
    let named = a.words.first().cloned();
    let lookup = RemoteArgs {
        words: named.into_iter().collect(),
        ..a.clone()
    };
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let mut input = json!({ "target": resolved.target });
    if let Some(source) = &a.source {
        input["source"] = json!(source);
    }
    if !a.exclude.is_empty() {
        input["exclude"] = json!(a.exclude);
    }
    if a.verbose || a.json {
        input["verbose"] = json!(true);
    }
    submit_and_stream(
        &gw,
        "remote.sync",
        &format!("sync {}", resolved.target),
        input,
        resolved.timeout_ms,
        a,
    )
    .await
}

async fn cmd_pull(gw: Gateway, a: &RemoteArgs) -> i32 {
    let named = a.words.first().cloned();
    let path = a.words.get(1).cloned().or_else(|| {
        // --target spelling: the FIRST word is the path, not a name.
        if a.target.is_some() {
            named.clone()
        } else {
            None
        }
    });
    let lookup = RemoteArgs {
        words: vec![named.unwrap_or_default()],
        ..a.clone()
    };
    let lookup = if a.target.is_some() {
        RemoteArgs {
            words: Vec::new(),
            ..a.clone()
        }
    } else {
        lookup
    };
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let Some(path) = path else {
        eprintln!("pull needs a workspace-relative path");
        return 1;
    };
    let mut input = json!({ "target": resolved.target, "remote": path });
    if let Some(to) = &a.to {
        input["to"] = json!(to);
    }
    submit_and_stream(
        &gw,
        "remote.pull",
        &format!("pull {}", resolved.target),
        input,
        resolved.timeout_ms,
        a,
    )
    .await
}
// --- `swiss run`: the run surface, shared with every submit --------------------------------

/// The entry for `swiss run ...` (argv is everything after `swiss run`).
pub async fn run_main(argv: Vec<String>) -> i32 {
    let (head, _) = split_passthrough(&argv);
    let a = parse(head);
    let gw = Gateway::at(a.port.unwrap_or_else(resolve_port));
    match a.sub.as_str() {
        "status" => {
            let Some(id) = a.words.first() else {
                eprintln!("usage: swiss run status <id>");
                return 1;
            };
            match gw.get(&format!("/api/runs/{id}")).await {
                Ok(run) => {
                    if a.json {
                        println!("{}", serde_json::to_string_pretty(&run).unwrap_or_default());
                    } else {
                        println!("run {}     {}", id, run["state"].as_str().unwrap_or("?"));
                        if let Some(exit) = run["exitCode"].as_i64() {
                            println!("exit      {exit}");
                        }
                        if let Some(ms) = run["durationMs"].as_u64() {
                            println!("took      {ms}ms");
                        }
                    }
                    0
                }
                Err(err) => {
                    eprintln!("{err}");
                    1
                }
            }
        }
        "logs" => {
            let Some(id) = a.words.first().and_then(|w| w.parse::<u64>().ok()) else {
                eprintln!("usage: swiss run logs <id> [-f]");
                return 1;
            };
            stream_run(&gw, id, true).await
        }
        "cancel" => {
            let Some(id) = a.words.first() else {
                eprintln!("usage: swiss run cancel <id>");
                return 1;
            };
            report(
                gw.post(&format!("/api/runs/{id}/cancel"), Value::Null)
                    .await,
                &format!("run {id} canceled"),
                a.json,
            )
        }
        other => {
            eprintln!("unknown run subcommand: {other} (status, logs, cancel)");
            1
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_bare_dash_dash_splits_and_nothing_after_it_is_a_flag() {
        // THE contract (docs/32 SS26): everything after the first -- is ARGV.
        let whole = argv(&["exec", "build", "--", "make", "-j8", "--", "-k", "--json"]);
        let (head, tail) = split_passthrough(&whole);
        assert_eq!(head, &argv(&["exec", "build"]));
        assert_eq!(tail, &argv(&["make", "-j8", "--", "-k", "--json"]));
        let a = parse(head);
        assert_eq!(a.sub, "exec");
        assert_eq!(a.words, vec!["build"]);
        assert!(!a.json, "the --json AFTER -- belongs to the far side");

        // No -- at all: passthrough empty, flags still parse.
        let whole = argv(&["targets", "--json"]);
        let (head, tail) = split_passthrough(&whole);
        assert_eq!(tail.len(), 0);
        assert!(parse(head).json);
    }

    #[test]
    fn durations_parse_the_way_humans_write_them() {
        assert_eq!(parse_duration("90"), Some(90_000));
        assert_eq!(parse_duration("90s"), Some(90_000));
        assert_eq!(parse_duration("45m"), Some(2_700_000));
        assert_eq!(parse_duration("2h"), Some(7_200_000));
        assert_eq!(parse_duration("nope"), None);
    }

    #[test]
    fn flags_never_eat_each_other_and_unknowns_collect() {
        let a = parse(&argv(&["exec", "--target", "dev", "--json", "--detach"]));
        assert_eq!(a.target.as_deref(), Some("dev"));
        assert!(a.json && a.detach);
        let a = parse(&argv(&["exec", "--target", "--json"]));
        assert_eq!(a.target.as_deref(), Some(""), "empty value, not --json");
        assert!(a.json);
        let a = parse(&argv(&["exec", "--wat"]));
        assert_eq!(a.unknown, vec!["--wat"]);
    }

    #[test]
    fn env_pairs_split_once_and_bad_ones_are_named() {
        let a = parse(&argv(&[
            "exec",
            "--env",
            "BOARD=rpi",
            "--env",
            "JOB=1",
            "--env",
            "BAD",
        ]));
        assert_eq!(
            a.env,
            vec![("BOARD".into(), "rpi".into()), ("JOB".into(), "1".into())]
        );
        assert_eq!(a.unknown, vec!["--env \"BAD\" needs NAME=VALUE"]);
    }
}
