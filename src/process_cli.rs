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

//! `swiss process` - the managed apps from a terminal (SPEC §process.cli).
//!
//! There is no offline path: the gateway is the one control path, so every verb
//! speaks the admin API with the CLI key, against the instance `swiss api`
//! targets (`resolve_port`). The verbs mirror the page one to one - a Start is
//! the same 202 and the same history line, with `by: "cli"` so the audit
//! trail can tell an operator at a keyboard from a script.

use serde_json::Value;

use crate::daemon::{resolve_port, with_cli_key};

/// The action poll (SPEC §process.cli): fast enough to feel instant, slow enough
/// that a stop-and-wait never spins the listener table.
const POLL_MS: u64 = 250;
/// How long a start window may take before the CLI gives up saying so. The
/// action keeps running either way - the gateway owns it now.
const ACTION_TIMEOUT_MS: u64 = 120_000;
/// The log follow's cadence (the `swiss logs -f` rhythm).
const FOLLOW_MS: u64 = 1_000;

struct Args {
    cmd: String,
    name: Option<String>,
    json: bool,
    force: bool,
    app_log: bool,
    lines: u64,
    follow: bool,
    unknown: Vec<String>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            cmd: String::from("status"),
            name: None,
            json: false,
            force: false,
            app_log: false,
            lines: 200,
            follow: false,
            unknown: Vec::new(),
        }
    }
}

/// Parse strict, the way cli.rs does: unknown flags are collected and refused
/// whole, a flag value never eats another flag, `-n` takes its number inline or
/// as the next word.
fn parse(argv: &[String]) -> Args {
    let mut a = Args::default();
    let mut i = 0usize;
    while i < argv.len() {
        let arg = &argv[i];
        if !arg.starts_with('-') {
            if a.cmd == "status" && matches!(arg.as_str(), "status" | "start" | "stop" | "restart" | "logs") {
                a.cmd = arg.clone();
            } else if a.name.is_none() && a.cmd != "status" {
                a.name = Some(arg.clone());
            } else {
                a.unknown.push(arg.clone());
            }
            i += 1;
            continue;
        }
        let (flag, inline) = match arg.find('=') {
            Some(eq) => (&arg[..eq], Some(arg[eq + 1..].to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--json" => a.json = true,
            "--force" => a.force = true,
            "--app" => a.app_log = true,
            "-f" | "--follow" => a.follow = true,
            "-n" | "--lines" => {
                let value = inline.clone().or_else(|| {
                    argv.get(i + 1).filter(|v| !v.starts_with('-')).cloned()
                });
                match value.and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) => {
                        a.lines = n;
                        if inline.is_none() && i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                            i += 1;
                        }
                    }
                    None => a.unknown.push(arg.clone()),
                }
            }
            _ => a.unknown.push(arg.clone()),
        }
        i += 1;
    }
    a
}

fn usage() -> String {
    [
        "usage:",
        "  swiss process [status] [--json]     one line per app; --json prints the GET body",
        "  swiss process start|stop|restart <name> [--force]   act, then wait for the outcome",
        "  swiss process logs <name> [--app] [-n N] [-f]       the capture, or the app's own log",
    ]
    .join("\n\n")
}

/// `45s`, `3m`, `1h 5m`, `2d 4h` - coarse on purpose; a terminal is not a profiler.
fn fmt_uptime(sec: u64) -> String {
    if sec < 60 {
        return format!("{sec}s");
    }
    let min = sec / 60;
    if min < 60 {
        return format!("{min}m");
    }
    let hours = min / 60;
    if hours < 24 {
        return format!("{hours}h {}m", min % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

/// One status line: name, status, pid, ports, uptime, and the reason or the
/// stranger - the same facts the row shows (SPEC §process.cli).
fn status_line(app: &Value) -> String {
    let name = app["name"].as_str().unwrap_or("?");
    let status = app["status"].as_str().unwrap_or("?");
    let pid = app["pid"].as_u64().map(|p| p.to_string()).unwrap_or_else(|| "-".into());
    let ports = app["ports"].as_array().map(|list| {
        list.iter().filter_map(|p| p.as_u64().map(|n| n.to_string())).collect::<Vec<_>>().join(",")
    }).unwrap_or_default();
    let ports = if ports.is_empty() { "-".to_string() } else { ports };
    let uptime = app["uptimeSec"].as_u64().map(fmt_uptime).unwrap_or_else(|| "-".into());
    let mut line = format!("{name:<16} {status:<9} pid {pid:<7} ports {ports:<12} up {uptime}");
    if let Some(reason) = app["reason"].as_str() {
        if !reason.is_empty() {
            line.push_str(&format!("  {reason}"));
        }
    }
    line
}

/// Exit 0 when every autostart app is running, and when no app has autostart;
/// 1 otherwise (SPEC §process.cli) - a boot script's one check.
fn status_exit(apps: &[Value]) -> i32 {
    let autostart: Vec<&Value> = apps.iter().filter(|a| a["autostart"].as_bool() == Some(true)).collect();
    if autostart.is_empty() {
        return 0;
    }
    if autostart.iter().all(|a| a["status"].as_str() == Some("running")) {
        0
    } else {
        1
    }
}

/// The poll's stop condition: the row is no longer busy AND its last action is
/// not the one from before the request (`at` moves past the request's clock).
fn row_settled(app: &Value, request_ms: u64) -> bool {
    if app.get("busy").is_some_and(|b| !b.is_null()) {
        return false;
    }
    match app["lastAction"]["at"].as_u64() {
        Some(at) => at >= request_ms,
        None => false,
    }
}

async fn get_process(client: &reqwest::Client, base: &str) -> Result<Value, String> {
    let res = with_cli_key(client.get(format!("{base}/api/process")))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("cannot reach the gateway on port {}: {e}", resolve_port()))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    // 503 structured = the process plugin is off (SPEC §process.apps): same exit as
    // unreachable, different sentence.
    Err(format!(
        "the gateway answered {}: {}",
        status.as_u16(),
        body["error"].as_str().unwrap_or("the process plugin is off")
    ))
}

fn app_named<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    body["apps"].as_array()?.iter().find(|a| a["name"].as_str() == Some(name))
}


/// The entry `cli::main` dispatches to before the generic parser (SPEC §process.cli).
pub async fn main(argv: Vec<String>) -> i32 {
    let a = parse(&argv);
    if !a.unknown.is_empty() {
        eprintln!("{}", usage());
        eprintln!("unknown or misplaced: {}", a.unknown.join(", "));
        return 1;
    }
    if matches!(a.cmd.as_str(), "start" | "stop" | "restart" | "logs") && a.name.is_none() {
        eprintln!("{}", usage());
        eprintln!("{} needs an app name", a.cmd);
        return 1;
    }
    let base = format!("http://127.0.0.1:{}", resolve_port());
    let client = crate::daemon::loopback_client();
    match a.cmd.as_str() {
        "status" => status_cmd(&client, &base, &a).await,
        "start" | "stop" | "restart" => action_cmd(&client, &base, &a).await,
        "logs" => logs_cmd(&client, &base, &a).await,
        _ => {
            eprintln!("{}", usage());
            1
        }
    }
}

async fn status_cmd(client: &reqwest::Client, base: &str, a: &Args) -> i32 {
    let body = match get_process(client, base).await {
        Ok(body) => body,
        Err(sentence) => {
            eprintln!("{sentence}");
            return 2;
        }
    };
    if a.json {
        match serde_json::to_string_pretty(&body) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("cannot render the answer: {e}");
                return 1;
            }
        }
        return 0;
    }
    let apps = body["apps"].as_array().cloned().unwrap_or_default();
    if apps.is_empty() {
        println!("no managed apps; add one on the Process page");
        return 0;
    }
    for app in &apps {
        println!("{}", status_line(app));
    }
    status_exit(&apps)
}

async fn action_cmd(client: &reqwest::Client, base: &str, a: &Args) -> i32 {
    let Some(name) = a.name.clone() else { return 1; };
    // `by: "cli"` - the audit trail's only two answers (SPEC §process.apps).
    let mut payload = serde_json::json!({ "by": "cli" });
    if a.force && matches!(a.cmd.as_str(), "stop" | "restart") {
        payload["force"] = Value::Bool(true);
    }
    let request_ms = now_ms();
    let res = with_cli_key(
        client
            .post(format!("{base}/api/process/apps/{name}/{}", a.cmd))
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5)),
    )
    .send()
    .await;
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            eprintln!("cannot reach the gateway on port {}: {e}", resolve_port());
            return 2;
        }
    };
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        eprintln!("{}", body["error"].as_str().unwrap_or("the action was refused"));
        return 1;
    }
    // 202 accepted: the action runs in the gateway. Poll until the row settles
    // on an outcome newer than this request (SPEC §process.cli).
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ACTION_TIMEOUT_MS);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
        let body = match get_process(client, base).await {
            Ok(body) => body,
            Err(sentence) => {
                eprintln!("{sentence}");
                return 2;
            }
        };
        let Some(app) = app_named(&body, &name) else {
            eprintln!("no app named {name}");
            return 1;
        };
        if row_settled(app, request_ms) {
            let last = &app["lastAction"];
            let ok = last["ok"].as_bool().unwrap_or(false);
            let message = last["message"].as_str().unwrap_or("no outcome recorded");
            println!("{message}");
            if last["canForce"].as_bool() == Some(true) {
                // The refused-stop hint names the pid to kill (SPEC §process.cli).
                let pid = app["pid"].as_u64().or_else(|| app["conflictPid"].as_u64());
                match pid {
                    Some(pid) => println!("run again with --force to kill pid {pid}"),
                    None => println!("run again with --force"),
                }
                return 1;
            }
            return if ok { 0 } else { 1 };
        }
        if std::time::Instant::now() >= deadline {
            println!("still busy; see `swiss process status`");
            return 1;
        }
    }
}

async fn logs_cmd(client: &reqwest::Client, base: &str, a: &Args) -> i32 {
    let Some(name) = a.name.clone() else { return 1; };
    let source = if a.app_log { "app" } else { "capture" };
    let mut offset: Option<u64> = None;
    loop {
        let mut url = format!("{base}/api/process/apps/{name}/log?source={source}");
        match offset {
            Some(after) => url.push_str(&format!("&after={after}")),
            None => url.push_str(&format!("&tail={}", a.lines)),
        }
        let res = with_cli_key(client.get(url).timeout(std::time::Duration::from_secs(5)))
            .send()
            .await;
        let res = match res {
            Ok(res) => res,
            Err(e) => {
                eprintln!("cannot reach the gateway on port {}: {e}", resolve_port());
                return 2;
            }
        };
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            eprintln!("{}", body["error"].as_str().unwrap_or("no log answered"));
            return 1;
        }
        let size = body["size"].as_u64().unwrap_or(0);
        let reset = body["reset"].as_bool().unwrap_or(false);
        if reset && offset.is_some() {
            // The file rotated under the follow: restart from the top rather
            // than silently skipping what the rotation carried away.
            offset = None;
            continue;
        }
        for line in body["lines"].as_array().map(|l| l.iter().filter_map(|s| s.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default() {
            println!("{line}");
        }
        offset = Some(size);
        if !a.follow {
            return 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(FOLLOW_MS)).await;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_parse_strict_and_minus_n_takes_its_number() {
        let a = parse(&argv(&["logs", "demo", "--app", "-n", "5", "-f"]));
        assert_eq!(a.cmd, "logs");
        assert_eq!(a.name.as_deref(), Some("demo"));
        assert!(a.app_log && a.follow);
        assert_eq!(a.lines, 5);
        assert!(a.unknown.is_empty());

        let a = parse(&argv(&["stop", "demo", "--force"]));
        assert_eq!(a.cmd, "stop");
        assert_eq!(a.name.as_deref(), Some("demo"));
        assert!(a.force);

        let a = parse(&argv(&[]));
        assert_eq!(a.cmd, "status");
        assert!(!a.json);

        let a = parse(&argv(&["status", "--json"]));
        assert!(a.json);
    }

    #[test]
    fn an_unknown_or_misplaced_word_is_refused_not_guessed() {
        let a = parse(&argv(&["status", "--wat"]));
        assert_eq!(a.unknown, vec!["--wat".to_string()]);
        // A second word where no word belongs is unknown, not an app name.
        let a = parse(&argv(&["status", "extra"]));
        assert_eq!(a.unknown, vec!["extra".to_string()]);
        // A bad -n value is unknown, and the word it failed to eat is too - the
        // whole invocation is refused, not half-interpreted.
        let a = parse(&argv(&["logs", "demo", "-n", "many"]));
        assert_eq!(a.unknown, vec!["-n".to_string(), "many".to_string()]);
    }

    #[test]
    fn uptime_is_coarse_on_purpose() {
        assert_eq!(fmt_uptime(45), "45s");
        assert_eq!(fmt_uptime(180), "3m");
        assert_eq!(fmt_uptime(3_900), "1h 5m");
        assert_eq!(fmt_uptime(187_200), "2d 4h");
    }

    #[test]
    fn status_exit_is_the_boot_scripts_one_check() {
        let apps = serde_json::json!([
            { "name": "a", "autostart": true, "status": "running" },
            { "name": "b", "autostart": false, "status": "stopped" }
        ]);
        let list = apps.as_array().unwrap();
        assert_eq!(status_exit(list), 0);
        let apps = serde_json::json!([{ "name": "a", "autostart": true, "status": "partial" }]);
        assert_eq!(status_exit(apps.as_array().unwrap()), 1);
        // No autostart at all: nothing to demand, exit 0.
        let apps = serde_json::json!([{ "name": "a", "status": "stopped" }]);
        assert_eq!(status_exit(apps.as_array().unwrap()), 0);
    }

    #[test]
    fn a_row_settles_only_on_an_outcome_newer_than_the_request() {
        let row = serde_json::json!({ "status": "running", "lastAction": { "action": "start", "ok": true, "at": 1_000_u64 } });
        assert!(row_settled(&row, 1_000));
        assert!(!row_settled(&row, 1_500), "a stale outcome is not this action's");
        let busy = serde_json::json!({ "busy": "start", "lastAction": { "at": 2_000_u64 } });
        assert!(!row_settled(&busy, 1_000), "busy never settles");
        let no_history = serde_json::json!({ "status": "stopped" });
        assert!(!row_settled(&no_history, 1_000), "no outcome recorded yet");
    }

    #[test]
    fn a_status_line_carries_the_facts_and_the_reason() {
        let app = serde_json::json!({ "name": "cc-otel", "status": "conflict", "ports": [4317], "reason": "port 4317 is held by node.exe (pid 5)" });
        let line = status_line(&app);
        assert!(line.contains("cc-otel"));
        assert!(line.contains("conflict"));
        assert!(line.contains("4317"));
        assert!(line.contains("node.exe (pid 5)"));
    }
}
