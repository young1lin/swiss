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

//! The `swiss remote` and `swiss run` command lines (docs/34 SS26).
//!
//! A thin, honest client of the HTTP surface: every mutation goes through
//! /api/remote, every execution goes through /api/runs, and the command streams
//! the live output back through the cursor API exactly like a panel page would.
//! No second job system, no local exec engine - the CLI is just the agent-facing
//! face of the same gateway.
//!
//! THE HARD RULE (docs/34 SS26): everything after a bare `--` is ARGV for the far
//! side, never parsed as a flag. `swiss remote exec build -- make -j8 -- -k` must
//! send `["make","-j8","--","-k"]` untouched. The same cut happens at exec's first
//! command word: `exec t ls -a` needs no `--`, and local flags end there.

use serde_json::{json, Value};

use crate::daemon::{read_gateway_token, resolve_port};

/// How long one exec may run unless --timeout, the project binding, or the target
/// row says otherwise (docs/34 SS26: CLI > binding > target > this).
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
    /// Exec only: the tokens from the first command word on, cut verbatim before
    /// flag parsing could eat them (docs/34 SS26). [parse_command] splices this
    /// ahead of the post-`--` tail so `passthrough` is the whole remote ARGV.
    pub operand: Vec<String>,
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
    /// `swiss run audit` (docs/41 A3): the window, the actor, and where to copy it.
    pub since: Option<String>,
    pub until: Option<String>,
    pub actor: Option<String>,
    pub export: Option<String>,
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
            // Exec's ARGV begins at the first command word (docs/34 SS26): the
            // second positional after the subcommand, or the first when --target
            // already named the target. From that word on nothing is a local
            // flag; the synopsis's bare "--" is the explicit spelling of the
            // same cut, so "exec t ls -a" and "exec t -- ls -a" agree.
            if !words.is_empty() && words[0] == "exec"
                && words.len() == if a.target.is_some() { 2 } else { 3 }
            {
                words.pop();
                a.operand = argv[i..].to_vec();
                break;
            }
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
            "--since" => a.since = Some(value(argv, &mut i, &inline)),
            "--until" => a.until = Some(value(argv, &mut i, &inline)),
            "--actor" => a.actor = Some(value(argv, &mut i, &inline)),
            "--export" => a.export = Some(value(argv, &mut i, &inline)),
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

/// Split, parse, and splice in one step: the returned `passthrough` is exactly
/// the remote ARGV whatever spelling produced it - after a bare `--`, at exec's
/// first command word, or both at once (the command word leads, the post-`--`
/// tail follows).
pub fn parse_command(argv: &[String]) -> RemoteArgs {
    let (head, tail) = split_passthrough(argv);
    let mut a = parse(head);
    let mut passthrough = std::mem::take(&mut a.operand);
    passthrough.extend_from_slice(tail);
    a.passthrough = passthrough;
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
/// An instant the way `--since` / `--until` are written: a span back from now
/// ("7d", "36h", "90m", "30s"), an ISO-8601 instant, or epoch milliseconds. Answers
/// epoch ms, which is what the API takes.
pub fn parse_instant(raw: &str, now_ms: u64) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(days) = raw.strip_suffix('d') {
        let n: u64 = days.parse().ok()?;
        return Some(now_ms.saturating_sub(n.saturating_mul(24 * 60 * 60 * 1000)));
    }
    if raw.ends_with(['h', 'm', 's']) {
        return Some(now_ms.saturating_sub(parse_duration(raw)?));
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw.parse().ok();
    }
    swiss_core::util::parse_iso_ms(raw).and_then(|ms| u64::try_from(ms).ok())
}

/// One argv word as a POSIX shell would need it typed: bare when it is plain, in
/// single quotes otherwise (a quote inside becomes '\''). The audit line shows the
/// command the way it could be run again, not the way JSON spells it.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// A query-string value: everything but the unreserved set percent-encoded, so an
/// actor like `cli:jdoe@box` or a target with a space survives the trip.
pub fn query_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
    let a = parse_command(argv);
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
        "push" => cmd_push(gw, &a).await,
        "pull" => cmd_pull(gw, &a).await,
        "cat" => cmd_cat(gw, &a).await,
        "write" => cmd_write(gw, &a).await,
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
            "target add <id> --endpoint <id|unique-name> --root <path> [--caps exec,sync,files]",
            "add a target",
        ),
        (
            "target set <id> [--endpoint <id|unique-name>] [--root <path>] [--caps <list>] [--label <text>]",
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
            "sync [name] [--source PATH] [--exclude PATTERN]... [--verbose]",
            "upload the local tree or one file",
        ),
        (
            "push [name] <file> [--to NAME]",
            "upload one file under a remote name",
        ),
        ("cat [name] <path>", "print one remote file to stdout"),
        (
            "write [name] <path>",
            "write stdin to one remote file (create/overwrite)",
        ),
        (
            "pull [name] <path> [--to LOCAL]",
            "download a file or folder",
        ),
    ] {
        s.push_str(&format!("  {cmd:<70} {text}\n"));
    }
    s.push_str(
        "  run status <id> | run logs <id> [-f] | run cancel <id>              the run surface\n",
    );
    s.push_str(
        "  run audit [--since 7d|ISO] [--until ISO] [--target t] [--actor a] [--json] [--export DIR]\n",
    );
    s.push_str(
        "                                                                         who ran what, where, with what result - the last 7 days by default\n",
    );
    s.push_str(
        "\nEverything after a bare -- is ARGV for the far side, passed through untouched.\n",
    );
    s.push_str(
        "For exec, ARGV also begins at the first command word: exec t ls -a == exec t -- ls -a.\n",
    );
    s.push_str(
        "Remote commands run under LANG=C.UTF-8 / LC_ALL=C.UTF-8 unless --env sets them; argv and output are UTF-8\n",
    );
    s.push_str(
        "end to end (Windows PowerShell 5: [Console]::OutputEncoding = [Text.Encoding]::UTF8 before reading it).\n",
    );
    s.push_str(
        "Every remote run is recorded - who, what, where, exit, output - for seven days at least: swiss run audit.\n",
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
                println!("  {:<24} {:<12} ID", "NAME", "STATE");
                for e in value["endpoints"].as_array().unwrap_or(&Vec::new()) {
                    println!(
                        "  {:<24} {:<12} {}",
                        e["label"].as_str().unwrap_or(""),
                        e["state"].as_str().unwrap_or("?"),
                        e["id"].as_str().unwrap_or("?"),
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
                let endpoint_inventory = gw.get("/api/remote/endpoints").await.ok();
                println!(
                    "  {:<16} {:<24} {:<16} ROOT",
                    "TARGET", "ENDPOINT", "CAPABILITIES"
                );
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
                    let endpoint_id = t["endpoint"].as_str().unwrap_or("?");
                    let endpoint = endpoint_inventory
                        .as_ref()
                        .map(|inventory| endpoint_display_name(inventory, endpoint_id))
                        .unwrap_or_else(|| endpoint_id.to_string());
                    println!(
                        "  {:<16} {:<24} {:<16} {}",
                        t["id"].as_str().unwrap_or("?"),
                        endpoint,
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

fn endpoint_display_name(response: &Value, id: &str) -> String {
    response["endpoints"]
        .as_array()
        .and_then(|endpoints| endpoints.iter().find(|e| e["id"].as_str() == Some(id)))
        .and_then(|endpoint| endpoint["label"].as_str())
        .filter(|label| !label.is_empty())
        .unwrap_or(id)
        .to_string()
}

/// Resolve the operator's endpoint selector to the provider-owned stable id. An exact id
/// always wins; otherwise one exact display-label match is accepted. Labels are not identity
/// and need not be unique, so an ambiguous label is refused instead of choosing silently.
fn canonical_endpoint_id(response: &Value, selector: &str) -> Result<String, String> {
    let endpoints = response["endpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if endpoints.is_empty() {
        // The API deliberately permits offline target configuration while no transport is
        // serving. With no inventory there is nothing to resolve, so preserve the id-shaped
        // value exactly as the caller supplied it.
        return Ok(selector.to_string());
    }
    if endpoints.iter().any(|e| e["id"].as_str() == Some(selector)) {
        return Ok(selector.to_string());
    }
    let matches: Vec<&Value> = endpoints
        .iter()
        .filter(|e| e["label"].as_str() == Some(selector))
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0]["id"].as_str().unwrap_or(selector).to_string());
    }
    if matches.len() > 1 {
        let ids = matches
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "endpoint name {selector:?} is ambiguous; use one of these ids: {ids}"
        ));
    }
    let known = endpoints
        .iter()
        .filter_map(|e| Some(format!("{} ({})", e["label"].as_str()?, e["id"].as_str()?)))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "unknown endpoint {selector:?}; use an endpoint id or one unique name (known: {known})"
    ))
}

async fn resolve_endpoint_selector(gw: &Gateway, selector: &str) -> Result<String, String> {
    let response = gw.get("/api/remote/endpoints").await?;
    canonical_endpoint_id(&response, selector)
}

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
                eprintln!("target add needs --endpoint <connection id or unique name> and --root <absolute POSIX path>");
                return 1;
            };
            let endpoint = match resolve_endpoint_selector(&gw, endpoint).await {
                Ok(endpoint) => endpoint,
                Err(err) => {
                    eprintln!("{err}");
                    return 1;
                }
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
                let endpoint = match resolve_endpoint_selector(&gw, endpoint).await {
                    Ok(endpoint) => endpoint,
                    Err(err) => {
                        eprintln!("{err}");
                        return 1;
                    }
                };
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
/// working directory, exactly like git finds .git (docs/34 SS24).
pub struct Resolved {
    pub target: String,
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
    pub via: String,
}

/// The deadline chain (docs/34 SS26): --timeout > project action > target row >
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
/// Who this CLI is, for the run record (docs/41 A1): `cli:<os user>@<hostname>`. The
/// admin API has no credential - loopback is its boundary - so this is self-declared,
/// which on a single-user machine is the truth and in the audit trail is the difference
/// between "the operator ran make" and "an agent's MCP token ran make".
fn cli_actor() -> String {
    let user = whoami::username();
    let host = whoami::fallible::hostname().unwrap_or_else(|_| "?".to_string());
    format!("cli:{user}@{host}")
}

/// Submit one remote run and, unless --detach, stream its live output through the
/// cursor API until it is terminal - then exit with the REMOTE exit code (docs/34
/// SS26): `swiss remote exec build -- false` exits 1 because false did.
async fn submit_and_stream(
    gw: &Gateway,
    action: &str,
    label: &str,
    input: Value,
    timeout_ms: Option<u64>,
    a: &RemoteArgs,
) -> i32 {
    let mut body =
        json!({ "action": action, "input": input, "label": label, "actor": cli_actor() });
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

/// The whole recorded stream of a finished remote run, from the record
/// (logs/remote/out/<id>.txt) rather than the live window. Answers false without
/// printing anything when there is no record to read (not a remote run, the plugin
/// off), so the caller falls back to the live window.
async fn print_recorded_output(gw: &Gateway, run_id: u64) -> bool {
    use std::io::Write as _;
    let mut cursor: u64 = 0;
    loop {
        let Ok(chunk) = gw
            .get(&format!(
                "/api/remote/runs/{run_id}/output?after={cursor}&max=131072"
            ))
            .await
        else {
            return cursor > 0;
        };
        print!("{}", chunk["output"].as_str().unwrap_or(""));
        let _ = std::io::stdout().flush();
        let next = chunk["nextCursor"].as_u64().unwrap_or(cursor);
        let total = chunk["total"].as_u64().unwrap_or(0);
        if next <= cursor || next >= total {
            return true;
        }
        cursor = next;
    }
}

/// The shared follow loop: poll the output cursor, print what arrived, and when
/// the run is terminal fetch the row and translate its outcome into an exit code.
///
/// A run that finished before the first poll has already been compacted to its last
/// 64 KiB (KEEP_FINISHED_OUTPUT_BYTES): the first read comes back `truncated`, and
/// printing it as if it were the stream would drop the head of a `cat` or a fast
/// build silently (found live 2026-09-21 with a 121 KB file). The record is written
/// before the run turns terminal, so the follower prints from there instead.
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
        let terminal = chunk["terminal"].as_bool().unwrap_or(false);
        if print_from_start {
            let evicted_head = cursor == 0 && chunk["truncated"].as_bool().unwrap_or(false);
            if evicted_head && terminal && print_recorded_output(gw, run_id).await {
                break;
            }
            let text = chunk["output"].as_str().unwrap_or("");
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        cursor = chunk["nextCursor"].as_u64().unwrap_or(cursor);
        if terminal {
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

/// `swiss remote push <name> <localfile> [--to remotename]`: one file up,
/// through the same remote.sync action a tree sync uses - the source is simply
/// a file and the optional `to` names it on the far side.
async fn cmd_push(gw: Gateway, a: &RemoteArgs) -> i32 {
    // Resolve exactly like sync: the first word (when present) names the target.
    let named = a.words.first().cloned();
    let lookup = RemoteArgs {
        words: named.clone().into_iter().collect(),
        ..a.clone()
    };
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    // The file: the word after the name, or the first word with --target.
    let file = a
        .words
        .get(1)
        .cloned()
        .or_else(|| if a.target.is_some() { named } else { None });
    let Some(file) = file else {
        eprintln!("push needs a local file (swiss remote push <name> <file> [--to NAME])");
        return 1;
    };
    let mut input = json!({ "target": resolved.target, "source": file });
    if let Some(to) = &a.to {
        input["to"] = json!(to);
    }
    if a.verbose || a.json {
        input["verbose"] = json!(true);
    }
    submit_and_stream(
        &gw,
        "remote.sync",
        &format!("push {}", resolved.target),
        input,
        resolved.timeout_ms,
        a,
    )
    .await
}

async fn cmd_cat(gw: Gateway, a: &RemoteArgs) -> i32 {
    // cat <name> <path>: print one remote file to stdout, no local file.
    let named = a.words.first().cloned();
    let lookup = RemoteArgs {
        words: named.clone().into_iter().collect(),
        ..a.clone()
    };
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let path = a
        .words
        .get(1)
        .cloned()
        .or_else(|| if a.target.is_some() { named } else { None });
    let Some(path) = path else {
        eprintln!("cat needs a remote path (swiss remote cat <name> <path>)");
        return 1;
    };
    let input = json!({ "target": resolved.target, "remote": path });
    submit_and_stream(
        &gw,
        "remote.cat",
        &format!("cat {}", resolved.target),
        input,
        resolved.timeout_ms,
        a,
    )
    .await
}

async fn cmd_write(gw: Gateway, a: &RemoteArgs) -> i32 {
    // write <name> <path>: stdin becomes the whole remote file (create/overwrite).
    let named = a.words.first().cloned();
    let lookup = RemoteArgs {
        words: named.clone().into_iter().collect(),
        ..a.clone()
    };
    let resolved = match resolve_name(&gw, &lookup).await {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let path = a
        .words
        .get(1)
        .cloned()
        .or_else(|| if a.target.is_some() { named } else { None });
    let Some(path) = path else {
        eprintln!("write needs a remote path (swiss remote write <name> <path> < local-file)");
        return 1;
    };
    let mut content = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut content).is_err() {
        eprintln!("could not read stdin");
        return 1;
    }
    let input = json!({ "target": resolved.target, "remote": path, "content": content });
    submit_and_stream(
        &gw,
        "remote.write",
        &format!("write {}", resolved.target),
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
        "audit" => cmd_audit(&gw, &a).await,
        other => {
            eprintln!("unknown run subcommand: {other} (status, logs, cancel, audit)");
            1
        }
    }
}

// --- `swiss run audit`: the last seven days, one line a run (docs/41 A3) ------------------

/// The record's rows inside the window, newest first, every page of them - the API
/// stops walking at `since`, so this ends where the window does.
async fn audit_rows(
    gw: &Gateway,
    a: &RemoteArgs,
    since: u64,
    until: Option<u64>,
) -> Result<Vec<Value>, String> {
    let mut rows = Vec::new();
    let mut before: Option<u64> = None;
    loop {
        let mut path = format!("/api/remote/runs?limit=100&since={since}");
        if let Some(until) = until {
            path.push_str(&format!("&until={until}"));
        }
        if let Some(target) = a.target.as_deref().filter(|t| !t.is_empty()) {
            path.push_str(&format!("&target={}", query_encode(target)));
        }
        if let Some(actor) = a.actor.as_deref().filter(|x| !x.is_empty()) {
            path.push_str(&format!("&actor={}", query_encode(actor)));
        }
        if let Some(b) = before {
            path.push_str(&format!("&before={b}"));
        }
        let page = gw.get(&path).await?;
        if let Some(runs) = page["runs"].as_array() {
            rows.extend(runs.iter().cloned());
        }
        match page["nextBefore"].as_u64() {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    Ok(rows)
}

/// What ran, the way it could be typed again: the argv shell-quoted for an exec, the
/// shape for the file actions.
pub fn audit_command(row: &Value) -> String {
    let input = &row["input"];
    let kind = row["action"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("remote.");
    let field = |k: &str| input[k].as_str().unwrap_or("");
    match kind {
        "exec" => input["argv"]
            .as_array()
            .map(|argv| {
                argv.iter()
                    .filter_map(Value::as_str)
                    .map(shell_word)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        "sync" => {
            let mut s = format!(
                "sync {}",
                shell_word(if field("source").is_empty() {
                    "."
                } else {
                    field("source")
                })
            );
            if !field("to").is_empty() {
                s.push_str(&format!(" -> {}", shell_word(field("to"))));
            }
            s
        }
        "pull" => {
            let mut s = format!("pull {}", shell_word(field("remote")));
            if !field("to").is_empty() {
                s.push_str(&format!(" -> {}", shell_word(field("to"))));
            }
            s
        }
        "cat" | "write" => format!("{kind} {}", shell_word(field("remote"))),
        other => row["label"].as_str().unwrap_or(other).to_string(),
    }
}

/// How the run ended, in a word or two.
pub fn audit_outcome(row: &Value) -> String {
    match row["state"].as_str().unwrap_or("") {
        "succeeded" => "exit 0".to_string(),
        "canceled" => "canceled".to_string(),
        "timeout" => "timeout".to_string(),
        _ => match row["exitCode"].as_i64() {
            Some(code) => format!("exit {code}"),
            None => match row["error"].as_str() {
                Some(err) => format!("error: {}", err.lines().next().unwrap_or("")),
                None => row["state"].as_str().unwrap_or("?").to_string(),
            },
        },
    }
}

fn audit_duration(row: &Value) -> String {
    match row["ms"].as_u64() {
        Some(ms) if ms >= 60_000 => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
        Some(ms) if ms >= 10_000 => format!("{}s", ms / 1000),
        Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
        None => "-".to_string(),
    }
}

fn audit_bytes(row: &Value) -> String {
    let n = row["outputBytes"].as_u64().unwrap_or(0);
    let s = if n >= 1024 * 1024 {
        format!("{:.1}M", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    };
    if row["outputEvicted"].as_bool().unwrap_or(false) {
        format!("{s}*")
    } else {
        s
    }
}

/// One row as the audit prints it: when, who, where, what, how it ended, how long,
/// how much, and the id to `swiss run status` / the panel.
pub fn audit_line(row: &Value) -> String {
    let when = row["endedAt"]
        .as_str()
        .or(row["startedAt"].as_str())
        .unwrap_or("?");
    let when = when.get(..19).unwrap_or(when); // to the second; the record keeps the ms
    let target = row["meta"]["target"]
        .as_str()
        .or(row["input"]["target"].as_str())
        .unwrap_or("-");
    format!(
        "{when:<19}  {:<24} {:<14} {:<7} {:<40} {:<10} {:>7} {:>8}  #{}",
        row["actor"].as_str().unwrap_or("-"),
        target,
        row["action"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("remote."),
        audit_command(row),
        audit_outcome(row),
        audit_duration(row),
        audit_bytes(row),
        row["runId"].as_u64().unwrap_or(0),
    )
}

/// Copy the window out: `<dir>/runs.jsonl` (the rows, one a line, as the record has
/// them) and `<dir>/out/<id>.txt` for every run whose output is still on disk. The
/// material a person hands to whoever asked "what happened on Tuesday".
async fn audit_export(
    gw: &Gateway,
    rows: &[Value],
    dir: &std::path::Path,
) -> Result<(usize, usize), String> {
    use std::io::Write as _;
    let out_dir = dir.join("out");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let index = dir.join("runs.jsonl");
    let mut file = std::fs::File::create(&index)
        .map_err(|e| format!("cannot write {}: {e}", index.display()))?;
    let mut files = 0usize;
    for row in rows {
        let mut line = serde_json::to_string(row).unwrap_or_default();
        line.push('\n');
        file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        let id = row["runId"].as_u64().unwrap_or(0);
        let evicted = row["outputEvicted"].as_bool().unwrap_or(false);
        if row["outputBytes"].as_u64().unwrap_or(0) == 0 || evicted {
            continue;
        }
        let path = out_dir.join(format!("{id}.txt"));
        let mut out = std::fs::File::create(&path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut cursor: u64 = 0;
        loop {
            let chunk = gw
                .get(&format!(
                    "/api/remote/runs/{id}/output?after={cursor}&max=131072"
                ))
                .await?;
            out.write_all(chunk["output"].as_str().unwrap_or("").as_bytes())
                .map_err(|e| e.to_string())?;
            let next = chunk["nextCursor"].as_u64().unwrap_or(cursor);
            let total = chunk["total"].as_u64().unwrap_or(0);
            if next <= cursor || next >= total {
                break;
            }
            cursor = next;
        }
        files += 1;
    }
    Ok((rows.len(), files))
}

async fn cmd_audit(gw: &Gateway, a: &RemoteArgs) -> i32 {
    let now = swiss_core::util::now_ms();
    let since = match a.since.as_deref() {
        None => now.saturating_sub(7 * 24 * 60 * 60 * 1000),
        Some(raw) => match parse_instant(raw, now) {
            Some(ms) => ms,
            None => {
                eprintln!("--since takes 7d / 36h / 90m, an ISO-8601 instant, or epoch milliseconds - not {raw:?}");
                return 1;
            }
        },
    };
    let until = match a.until.as_deref() {
        None => None,
        Some(raw) => match parse_instant(raw, now) {
            Some(ms) => Some(ms),
            None => {
                eprintln!("--until takes an ISO-8601 instant, epoch milliseconds, or 2d / 12h back from now - not {raw:?}");
                return 1;
            }
        },
    };
    let rows = match audit_rows(gw, a, since, until).await {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Array(rows.clone())).unwrap_or_default()
        );
    } else if rows.is_empty() {
        println!("no remote runs recorded in the window");
    } else {
        for row in &rows {
            println!("{}", audit_line(row));
        }
        if rows
            .iter()
            .any(|r| r["outputEvicted"].as_bool().unwrap_or(false))
        {
            println!("* output file evicted by the size budget; the line is what remains");
        }
    }
    if let Some(dir) = a.export.as_deref().filter(|d| !d.is_empty()) {
        let dir = std::path::PathBuf::from(dir);
        match audit_export(gw, &rows, &dir).await {
            Ok((runs, files)) => {
                eprintln!(
                    "exported {runs} runs, {files} output files to {}",
                    dir.display()
                );
            }
            Err(err) => {
                eprintln!("{err}");
                return 1;
            }
        }
    }
    0
}
#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_bare_dash_dash_splits_and_nothing_after_it_is_a_flag() {
        // THE contract (docs/34 SS26): everything after the first -- is ARGV.
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
    fn the_cli_declares_itself_as_user_at_host() {
        // docs/41 A1: the shape the audit trail keys on; the parts are whatever the OS
        // says, never empty.
        let actor = cli_actor();
        let rest = actor.strip_prefix("cli:").expect("the cli: prefix");
        let (user, host) = rest.split_once('@').expect("user@host");
        assert!(!user.is_empty() && !host.is_empty(), "{actor}");
    }

    #[test]
    fn audit_instants_are_a_span_back_an_iso_instant_or_epoch_ms() {
        let now = 1_700_000_000_000;
        assert_eq!(parse_instant("7d", now), Some(now - 7 * 86_400_000));
        assert_eq!(parse_instant("36h", now), Some(now - 36 * 3_600_000));
        assert_eq!(parse_instant("90m", now), Some(now - 90 * 60_000));
        assert_eq!(parse_instant("30s", now), Some(now - 30_000));
        assert_eq!(parse_instant("2023-11-14T22:13:20Z", now), Some(now));
        assert_eq!(parse_instant("1700000000000", now), Some(now));
        assert_eq!(parse_instant("", now), None);
        assert_eq!(parse_instant("last tuesday", now), None);
        assert_eq!(parse_instant("xd", now), None);
    }

    #[test]
    fn an_audit_line_shows_the_command_as_it_could_be_typed_again() {
        // The argv is quoted for a POSIX shell, never JSON-spelled; the file actions
        // show their shape; the outcome is a word; an evicted output is starred.
        let row = json!({
            "runId": 17, "actor": "cli:jdoe@box", "action": "remote.exec", "state": "failed",
            "endedAt": "2026-09-21T14:03:11.250Z", "ms": 12345, "exitCode": 2, "outputBytes": 3072,
            "meta": { "target": "build" },
            "input": { "target": "build", "argv": ["bash", "-c", "echo 中文 && make -j8", "it's"] },
        });
        let line = audit_line(&row);
        assert!(
            line.starts_with("2026-09-21T14:03:11  cli:jdoe@box"),
            "{line}"
        );
        assert!(line.contains(" build "), "{line}");
        assert!(
            line.contains("bash -c 'echo 中文 && make -j8' 'it'\\''s'"),
            "{line}"
        );
        assert!(line.contains(" exit 2 "), "{line}");
        assert!(line.contains(" 12s "), "{line}");
        assert!(line.contains(" 3.0K "), "{line}");
        assert!(line.ends_with("#17"), "{line}");

        let sync = json!({ "action": "remote.sync", "state": "succeeded", "input": { "source": "src", "to": "app/src" } });
        assert_eq!(audit_command(&sync), "sync src -> app/src");
        assert_eq!(audit_outcome(&sync), "exit 0");
        let cat = json!({ "action": "remote.cat", "state": "canceled", "input": { "remote": "logs/app.log" } });
        assert_eq!(audit_command(&cat), "cat logs/app.log");
        assert_eq!(audit_outcome(&cat), "canceled");
        let broken = json!({ "action": "remote.exec", "state": "failed", "error": "ssh: connect refused\nmore" });
        assert_eq!(audit_outcome(&broken), "error: ssh: connect refused");
        let evicted = json!({ "action": "remote.exec", "state": "succeeded", "outputBytes": 2048, "outputEvicted": true, "input": { "argv": ["true"] } });
        assert!(
            audit_line(&evicted).contains(" 2.0K* "),
            "{}",
            audit_line(&evicted)
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(query_encode("build"), "build");
        assert_eq!(query_encode("cli:jdoe@box"), "cli%3Ajdoe%40box");
        assert_eq!(query_encode("a b&c=中"), "a%20b%26c%3D%E4%B8%AD");
    }

    #[test]
    fn shell_words_quote_only_what_needs_it() {
        assert_eq!(shell_word("make"), "make");
        assert_eq!(shell_word("-j8"), "-j8");
        assert_eq!(shell_word("a=b:c@d/e.f"), "a=b:c@d/e.f");
        assert_eq!(shell_word("two words"), "'two words'");
        assert_eq!(shell_word("中文"), "'中文'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
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
    fn push_parses_name_file_and_to() {
        let a = parse(&argv(&["push", "dev", "app.exe", "--to", "bin/app.exe"]));
        assert_eq!(a.sub, "push");
        assert_eq!(a.words, vec!["dev", "app.exe"]);
        assert_eq!(a.to.as_deref(), Some("bin/app.exe"));
        // --target spelling: the only word is the file.
        let a = parse(&argv(&["push", "--target", "dev", "app.exe"]));
        assert_eq!(a.words, vec!["app.exe"]);
        assert_eq!(a.target.as_deref(), Some("dev"));
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

    #[test]
    fn exec_argv_also_begins_at_the_first_command_word() {
        // docs/34 SS26: "exec test ls -a" and "exec test -- ls -a" are the same
        // call; the local-flag zone ends at the first command word after the name.
        let a = parse_command(&argv(&["exec", "test", "ls", "-a"]));
        assert_eq!(a.sub, "exec");
        assert_eq!(a.words, vec!["test"]);
        assert_eq!(a.passthrough, vec!["ls", "-a"]);
        assert!(a.unknown.is_empty());

        // With --target naming the target, the first word IS the command.
        let a = parse_command(&argv(&["exec", "--target", "t", "ls", "-a"]));
        assert_eq!(a.passthrough, vec!["ls", "-a"]);

        // Flags between the name and the command word stay local.
        let a = parse_command(&argv(&[
            "exec",
            "test",
            "--env",
            "A=B",
            "--timeout",
            "30m",
            "make",
            "-j8",
        ]));
        assert_eq!(a.env, vec![("A".into(), "B".into())]);
        assert_eq!(a.timeout.as_deref(), Some("30m"));
        assert_eq!(a.passthrough, vec!["make", "-j8"]);
    }

    #[test]
    fn an_exec_operand_rejoins_what_follows_the_bare_dash_dash() {
        // "exec test make -- -k" used to drop the words before -- and send only
        // ["-k"]; with the operand rule the command word leads the argv.
        let a = parse_command(&argv(&["exec", "test", "make", "--", "-k"]));
        assert_eq!(a.passthrough, vec!["make", "-k"]);

        // The bare -- itself still vanishes into the split, as always.
        let a = parse_command(&argv(&["exec", "test", "ls", "--", "-a"]));
        assert_eq!(a.passthrough, vec!["ls", "-a"]);
    }

    #[test]
    fn unknown_flags_before_the_exec_command_word_still_refuse() {
        // Typo protection: a flag-shaped token before the command word is a
        // local error (the whole argv, not usage, decides); non-exec
        // subcommands keep the strict parse.
        let a = parse_command(&argv(&["exec", "--timeuot", "5m", "make"]));
        assert_eq!(a.unknown, vec!["--timeuot"]);
        let a = parse_command(&argv(&["pull", "test", "out/x", "-a"]));
        assert_eq!(a.unknown, vec!["-a"]);
    }

    #[test]
    fn endpoint_selector_accepts_an_id_or_one_unique_display_name() {
        let response = json!({
            "endpoints": [
                { "id": "id-one", "label": "开发机", "state": "connected" },
                { "id": "id-two", "label": "构建机", "state": "idle" }
            ]
        });
        assert_eq!(
            canonical_endpoint_id(&response, "id-one").as_deref(),
            Ok("id-one")
        );
        assert_eq!(
            canonical_endpoint_id(&response, "开发机").as_deref(),
            Ok("id-one")
        );
        assert_eq!(endpoint_display_name(&response, "id-one"), "开发机");
        assert_eq!(endpoint_display_name(&response, "missing"), "missing");
        assert_eq!(
            canonical_endpoint_id(&json!({ "endpoints": [] }), "offline-id").as_deref(),
            Ok("offline-id"),
            "offline configuration preserves the provider-owned id"
        );
        let id_beats_label = json!({
            "endpoints": [
                { "id": "build", "label": "primary", "state": "connected" },
                { "id": "id-two", "label": "build", "state": "connected" }
            ]
        });
        assert_eq!(
            canonical_endpoint_id(&id_beats_label, "build").as_deref(),
            Ok("build"),
            "stable identity wins over a colliding display name"
        );
    }

    #[test]
    fn endpoint_selector_refuses_ambiguous_or_unknown_display_names() {
        let response = json!({
            "endpoints": [
                { "id": "id-one", "label": "build", "state": "connected" },
                { "id": "id-two", "label": "build", "state": "idle" }
            ]
        });
        let duplicate = canonical_endpoint_id(&response, "build").expect_err("ambiguous");
        assert!(duplicate.contains("ambiguous"), "{duplicate}");
        assert!(
            duplicate.contains("id-one") && duplicate.contains("id-two"),
            "{duplicate}"
        );

        let unknown = canonical_endpoint_id(&response, "missing").expect_err("unknown");
        assert!(unknown.contains("unknown endpoint"), "{unknown}");
        assert!(unknown.contains("build (id-one)"), "{unknown}");
    }
}
