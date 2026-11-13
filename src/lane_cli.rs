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

//! `swiss lane` - heavy work takes turns (SPEC §lane.cli).
//!
//! `swiss lane run -- cargo test -j 2` waits until the machine-wide lane has a free slot, then
//! runs the command right here, as its caller would have: same directory, same environment, the
//! lane's caps (CARGO_BUILD_JOBS, VITEST_MAX_WORKERS) added only where the caller set nothing.
//! Its output goes to this terminal and to a log file, and this process exits with its code.
//!
//! The open request to the gateway is the lease. If this process dies the slot passes on, and
//! the command dies with it (on Windows it runs in a kill-on-close job), so the lane never
//! counts a command nobody is waiting for.
//!
//! The lane is a courtesy, never a gate: a gateway that is down, an older gateway without the
//! lane, or a disabled plugin means the command runs at once, after one warning line.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::daemon::{resolve_port, with_cli_key};

/// Logs kept under ~/.swiss/lane; the oldest go first.
const LOGS_KEPT: usize = 50;
/// The gateway writes at least every 5 s; this long without a byte means it is gone.
const SILENCE: Duration = Duration::from_secs(20);
/// After a lease breaks (the gateway restarted), how long to keep trying to get it back, and
/// how often.
const RECONNECT_FOR: Duration = Duration::from_secs(60);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
/// After the command exits, how long its output may still trickle in. A grandchild that kept
/// the pipes (a build daemon) would otherwise hold this process open forever.
const DRAIN: Duration = Duration::from_secs(2);
/// --wait ran out before the turn came (EX_TEMPFAIL: try again later).
const EXIT_WAIT_EXPIRED: i32 = 75;
/// The command could not be found or started, the shell's convention.
const EXIT_NOT_FOUND: i32 = 127;
const EXIT_INTERRUPTED: i32 = 130;
const EXIT_USAGE: i32 = 2;

#[derive(Debug, PartialEq)]
enum Cmd {
    Status { json: bool },
    Run(RunArgs),
    Release(u64),
    Help,
}

#[derive(Debug, PartialEq)]
struct RunArgs {
    /// Give up after waiting this long (ms); `None` waits as long as it takes.
    wait_ms: Option<u64>,
    /// The command: program first.
    argv: Vec<String>,
}

fn usage() -> String {
    [
        "usage:",
        "  swiss lane run [--wait DUR] [--] <program> [args...]",
        "      wait for a free slot, then run the command here with the lane's caps;",
        "      exits with the command's code (75 when --wait ran out, 127 not found)",
        "  swiss lane [status] [--json]      what runs, what waits, what just finished",
        "  swiss lane release <id>           stop counting a command (it keeps running)",
        "",
        "  A shell line runs through a shell: swiss lane run -- pwsh -c \"npm run check; npm run build\"",
    ]
    .join("\n")
}

/// Strict, the way cli.rs parses: a word this does not know is refused, never guessed.
fn parse(argv: &[String]) -> Result<Cmd, String> {
    let Some(first) = argv.first() else {
        return Ok(Cmd::Status { json: false });
    };
    match first.as_str() {
        "run" => parse_run(&argv[1..]),
        "status" | "--json" => {
            let rest = if first == "status" { &argv[1..] } else { argv };
            let mut json = false;
            for arg in rest {
                match arg.as_str() {
                    "--json" => json = true,
                    other => return Err(format!("unknown or misplaced: {other}")),
                }
            }
            Ok(Cmd::Status { json })
        }
        "release" => match &argv[1..] {
            [id] => id
                .trim_start_matches('#')
                .parse()
                .map(Cmd::Release)
                .map_err(|_| format!("release takes a lane id like 12, got {id}")),
            _ => Err("release takes one lane id".into()),
        },
        "help" | "-h" | "--help" => Ok(Cmd::Help),
        other => Err(format!("unknown lane command: {other}")),
    }
}

/// `run`'s own flags come first; the first word that is not one (or everything after `--`) is
/// the command, flags and all.
fn parse_run(argv: &[String]) -> Result<Cmd, String> {
    let mut wait_ms = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "--" {
            i += 1;
            break;
        }
        if !arg.starts_with('-') {
            break;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--wait" => {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        i += 1;
                        argv.get(i).cloned().ok_or_else(|| "--wait needs a duration like 10m".to_string())?
                    }
                };
                let ms = crate::remote_cli::parse_duration(&value)
                    .filter(|ms| *ms > 0)
                    .ok_or_else(|| format!("--wait takes a duration like 90s, 10m or 1h, got {value}"))?;
                wait_ms = Some(ms);
            }
            _ => return Err(format!("unknown flag before the command: {arg} (put the command after --)")),
        }
        i += 1;
    }
    let argv = argv[i..].to_vec();
    if argv.is_empty() {
        return Err("run needs a command: swiss lane run -- cargo test".into());
    }
    Ok(Cmd::Run(RunArgs { wait_ms, argv }))
}

/// The entry `cli::main` dispatches to before the generic parser (SPEC §lane.cli).
pub async fn main(argv: Vec<String>) -> i32 {
    match parse(&argv) {
        Err(err) => {
            eprintln!("{}", usage());
            eprintln!("{err}");
            EXIT_USAGE
        }
        Ok(Cmd::Help) => {
            println!("{}", usage());
            0
        }
        Ok(Cmd::Status { json }) => status_cmd(json).await,
        Ok(Cmd::Release(id)) => release_cmd(id).await,
        Ok(Cmd::Run(args)) => run_cmd(args).await,
    }
}

fn base() -> String {
    format!("http://127.0.0.1:{}", resolve_port())
}

/// GET /api/lane, or the sentence that says why not.
pub(crate) async fn get_lane(client: &reqwest::Client, base: &str) -> Result<Value, String> {
    let res = with_cli_key(client.get(format!("{base}/api/lane")))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| format!("cannot reach the gateway on port {}", resolve_port()))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    Err(refusal(status.as_u16(), &body))
}

fn refusal(status: u16, body: &Value) -> String {
    match status {
        503 => "the lane plugin is off".to_string(),
        404 => "this gateway has no lane (an older build)".to_string(),
        _ => format!("the gateway answered {status}: {}", body["error"].as_str().unwrap_or("no reason given")),
    }
}

async fn status_cmd(json: bool) -> i32 {
    let client = crate::daemon::loopback_client();
    let body = match get_lane(&client, &base()).await {
        Ok(body) => body,
        Err(sentence) => {
            eprintln!("{sentence}");
            return 2;
        }
    };
    if json {
        match serde_json::to_string_pretty(&body) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("cannot render the answer: {e}");
                return 1;
            }
        }
        return 0;
    }
    for line in status_lines(&body, now_ms(), 5) {
        println!("{line}");
    }
    0
}

/// The lane as a few lines: capacity and caps, then running, waiting, and the newest `recent`
/// finished commands.
pub(crate) fn status_lines(body: &Value, now: u64, recent: usize) -> Vec<String> {
    let capacity = body["capacity"].as_u64().unwrap_or(1);
    let slots = if capacity == 1 { "1 slot".to_string() } else { format!("{capacity} slots") };
    let mut out = vec![format!("lane: {slots}; caps {}", caps_text(&body["env"]))];
    let rows = |key: &str| body[key].as_array().cloned().unwrap_or_default();
    let (running, waiting) = (rows("running"), rows("waiting"));
    for entry in &running {
        let since = entry["startedAt"].as_u64().unwrap_or(now);
        out.push(entry_line("running", entry, &fmt_secs(now.saturating_sub(since) / 1000)));
    }
    for entry in &waiting {
        let since = entry["enteredAt"].as_u64().unwrap_or(now);
        out.push(entry_line("waiting", entry, &fmt_secs(now.saturating_sub(since) / 1000)));
    }
    if running.is_empty() && waiting.is_empty() {
        out.push("idle: a command run through the lane starts at once".to_string());
    }
    for entry in rows("recent").iter().take(recent) {
        let outcome = match entry["outcome"].as_str() {
            Some("exit") => match entry["exit"].as_i64() {
                Some(code) => format!("exit {code}"),
                None => "killed".to_string(),
            },
            Some(other) => other.to_string(),
            None => "?".to_string(),
        };
        let took = match (entry["startedAt"].as_u64(), entry["endedAt"].as_u64()) {
            (Some(start), Some(end)) => fmt_secs(end.saturating_sub(start) / 1000),
            _ => "-".to_string(),
        };
        out.push(entry_line("done", entry, &format!("{took} {outcome}")));
    }
    out
}

fn entry_line(state: &str, entry: &Value, when: &str) -> String {
    format!(
        "{state:<8} #{:<4} {:<8} {when:<14} {}",
        entry["id"].as_u64().unwrap_or(0),
        entry["by"].as_str().unwrap_or("?"),
        entry["cmd"].as_str().unwrap_or("?"),
    )
}

fn caps_text(env: &Value) -> String {
    let caps: Vec<String> = env
        .as_object()
        .map(|map| map.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect())
        .unwrap_or_default();
    if caps.is_empty() { "none".to_string() } else { caps.join(" ") }
}

/// 75 -> "1m15s"; under a minute "42s"; over an hour "2h05m".
pub(crate) fn fmt_secs(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

async fn release_cmd(id: u64) -> i32 {
    let client = crate::daemon::loopback_client();
    let res = with_cli_key(client.post(format!("{}/api/lane/{id}/release", base())))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    match res {
        Ok(res) if res.status().is_success() => {
            println!("released #{id}: the lane no longer counts it; the command itself runs on");
            0
        }
        Ok(res) => {
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            eprintln!("{}", body["error"].as_str().map(str::to_string).unwrap_or_else(|| refusal(status, &body)));
            1
        }
        Err(_) => {
            eprintln!("cannot reach the gateway on port {}", resolve_port());
            2
        }
    }
}

// --- the lease ------------------------------------------------------------------------------

/// What the lease said.
#[derive(Debug, PartialEq)]
enum Heard {
    Waiting { ahead: u64, running: Vec<Value> },
    Running(Vec<(String, String)>),
    Released,
    Closed,
    /// The stream broke, ended, or went silent: the gateway is gone or restarting.
    Lost,
}

struct Lease {
    client: reqwest::Client,
    base: String,
    ticket: Value,
    response: reqwest::Response,
    pending: Vec<u8>,
    /// The lane's id for this command, from the first line (0 until then).
    id: u64,
}

impl Lease {
    async fn open(client: &reqwest::Client, base: &str, ticket: &Value, resume: bool) -> Result<Lease, String> {
        let mut body = ticket.clone();
        body["resume"] = json!(resume);
        // No timeout on the request itself: the answer is the lease and stays open. Only the
        // wait for its head is bounded.
        let send = with_cli_key(client.post(format!("{base}/api/lane/enter")).json(&body)).send();
        let response = match tokio::time::timeout(Duration::from_secs(5), send).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(format!("cannot reach the gateway on port {}", resolve_port())),
            Err(_) => return Err("the gateway did not answer within 5 s".to_string()),
        };
        let status = response.status();
        if !status.is_success() {
            let body: Value = response.json().await.unwrap_or(Value::Null);
            return Err(refusal(status.as_u16(), &body));
        }
        Ok(Lease {
            client: client.clone(),
            base: base.to_string(),
            ticket: ticket.clone(),
            response,
            pending: Vec::new(),
            id: 0,
        })
    }

    async fn next(&mut self) -> Heard {
        loop {
            while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=at).collect();
                if let Some((id, heard)) = parse_line(&line) {
                    if let Some(id) = id {
                        self.id = id;
                    }
                    return heard;
                }
            }
            match tokio::time::timeout(SILENCE, self.response.chunk()).await {
                Ok(Ok(Some(bytes))) => self.pending.extend_from_slice(&bytes),
                _ => return Heard::Lost,
            }
        }
    }

    /// Enter again after the lease broke or the lane closed under a restarting gateway.
    /// `resume`: the command already runs, so it takes its slot back at once.
    async fn reopen(&mut self, resume: bool) -> bool {
        let until = Instant::now() + RECONNECT_FOR;
        loop {
            if let Ok(fresh) = Lease::open(&self.client, &self.base, &self.ticket, resume).await {
                *self = fresh;
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(RECONNECT_EVERY).await;
        }
    }

    async fn done(&self, exit: Option<i32>) {
        if self.id == 0 {
            return;
        }
        let url = format!("{}/api/lane/{}/done", self.base, self.id);
        let _ = with_cli_key(self.client.post(url).json(&json!({ "exit": exit })))
            .timeout(Duration::from_secs(5))
            .send()
            .await;
    }
}

/// One lease line: the id it names and what it says. `None` for a heartbeat (a bare newline)
/// or a line this build does not know.
fn parse_line(line: &[u8]) -> Option<(Option<u64>, Heard)> {
    let value: Value = serde_json::from_slice(line.trim_ascii()).ok()?;
    let heard = match value["state"].as_str()? {
        "waiting" => Heard::Waiting {
            ahead: value["ahead"].as_u64().unwrap_or(0),
            running: value["running"].as_array().cloned().unwrap_or_default(),
        },
        "running" => Heard::Running(
            value["env"]
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "released" => Heard::Released,
        "closed" => Heard::Closed,
        _ => return None,
    };
    Some((value["id"].as_u64(), heard))
}

enum Turn {
    Granted(Vec<(String, String)>),
    /// Run without the lane, for this reason.
    Without(String),
    Expired,
    Interrupted,
}

/// Wait for the slot, saying where the queue stands whenever that changes.
async fn wait_turn(lease: &mut Lease, wait: Option<Duration>) -> Turn {
    let deadline = wait.map(|wait| tokio::time::Instant::now() + wait);
    let mut said = String::new();
    loop {
        let heard = tokio::select! {
            heard = lease.next() => heard,
            _ = sleep_until(deadline) => return Turn::Expired,
            _ = tokio::signal::ctrl_c() => return Turn::Interrupted,
        };
        match heard {
            Heard::Waiting { ahead, running } => {
                let line = waiting_line(ahead, &running, now_ms());
                // The age in the line ticks; say it again only when the queue itself moved.
                let key = format!("{ahead}|{}", running.iter().map(|r| r["cmd"].to_string()).collect::<Vec<_>>().join("|"));
                if key != said {
                    eprintln!("{line}");
                    said = key;
                }
            }
            Heard::Running(env) => return Turn::Granted(env),
            Heard::Released => return Turn::Without("released from the Lane page".to_string()),
            Heard::Closed | Heard::Lost => {
                // Closed is a gateway restart or a disabled plugin; Lost is a dropped connection.
                eprintln!("lane: the lane went away; queueing again");
                said.clear();
                let back = tokio::select! {
                    back = lease.reopen(false) => back,
                    _ = sleep_until(deadline) => return Turn::Expired,
                    _ = tokio::signal::ctrl_c() => return Turn::Interrupted,
                };
                if !back {
                    return Turn::Without("the lane did not come back within a minute".to_string());
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn waiting_line(ahead: u64, running: &[Value], now: u64) -> String {
    let mut line = format!("lane: waiting - {ahead} ahead");
    let now_running: Vec<String> = running
        .iter()
        .map(|r| {
            let since = r["startedAt"].as_u64().unwrap_or(now);
            format!(
                "{} ({}, {})",
                r["cmd"].as_str().unwrap_or("?"),
                r["by"].as_str().unwrap_or("?"),
                fmt_secs(now.saturating_sub(since) / 1000)
            )
        })
        .collect();
    if !now_running.is_empty() {
        line.push_str("; running now: ");
        line.push_str(&now_running.join(", "));
    }
    line
}

/// While the command runs: answer heartbeats, take the slot back after a gateway restart.
/// Returns only when the lane lets go for good; the caller then stops following it.
async fn keep(lease: &mut Lease) -> &'static str {
    loop {
        match lease.next().await {
            Heard::Running(_) | Heard::Waiting { .. } => {}
            Heard::Released => return "released from the Lane page; the command runs on outside the lane",
            Heard::Closed | Heard::Lost => {
                if !lease.reopen(true).await {
                    return "the lane did not come back within a minute; the command runs on outside it";
                }
            }
        }
    }
}

// --- the run --------------------------------------------------------------------------------

async fn run_cmd(args: RunArgs) -> i32 {
    // Found before queueing: a typo should not wait its turn to fail.
    let path_var = std::env::var("PATH").unwrap_or_default();
    let Some(program) = swiss_host::pathenv::resolve_command(&args.argv[0], &path_var) else {
        eprintln!("lane: {}: command not found", args.argv[0]);
        return EXIT_NOT_FOUND;
    };
    let line = args.argv.iter().map(|w| crate::remote_cli::shell_word(w)).collect::<Vec<_>>().join(" ");
    let home = swiss_core::paths::home_dir();
    let cwd = std::env::current_dir().ok();
    let cwd_text = cwd.as_deref().map(|d| crate::remote_cli::origin::tilde(d, &home)).unwrap_or_default();
    let origin = crate::remote_cli::origin::detect();
    let by = origin.as_ref().map(|o| o.agent.clone()).unwrap_or_else(|| "cli".to_string());
    let mut ticket = json!({ "cmd": line, "cwd": cwd_text, "by": by, "pid": std::process::id() });
    if let Some(session) = origin.and_then(|o| o.session) {
        ticket["session"] = json!(session);
    }

    let client = crate::daemon::loopback_client();
    let entered = Instant::now();
    let mut lease = match Lease::open(&client, &base(), &ticket, false).await {
        Ok(lease) => Some(lease),
        Err(why) => {
            eprintln!("lane: {why}; running without the lane");
            None
        }
    };
    let mut caps = Vec::new();
    if let Some(held) = lease.as_mut() {
        match wait_turn(held, args.wait_ms.map(Duration::from_millis)).await {
            Turn::Granted(env) => caps = env,
            Turn::Without(why) => {
                eprintln!("lane: {why}; running without the lane");
                lease = None;
            }
            Turn::Expired => {
                eprintln!(
                    "lane: still waiting after {}; gave up without running it",
                    fmt_secs(entered.elapsed().as_secs())
                );
                return EXIT_WAIT_EXPIRED;
            }
            Turn::Interrupted => {
                eprintln!("lane: interrupted while waiting; nothing ran");
                return EXIT_INTERRUPTED;
            }
        }
    }
    let waited = entered.elapsed();

    let mut command = tokio::process::Command::new(&program);
    command.args(&args.argv[1..]);
    // The caller's environment as it was, launcher markers included (the CLI struck them from
    // itself at startup), then the caps the caller left unset.
    command.envs(swiss_core::env::struck_env());
    let added = caps_to_add(&caps, |name| std::env::var_os(name).is_some() || swiss_core::env::launch_var(name).is_some());
    command.envs(added.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    command.stdin(std::process::Stdio::inherit());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let log_path = open_log_path(&args.argv[0]);
    let log = log_path.as_ref().and_then(|path| {
        let mut file = std::fs::File::create(path).ok()?;
        use std::io::Write as _;
        let _ = writeln!(file, "# swiss lane run: {line}\n# cwd: {cwd_text}\n# by: {by}\n# waited: {}\n", fmt_secs(waited.as_secs()));
        Some(Arc::new(Mutex::new(file)))
    });

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("lane: could not start {}: {err}", program.display());
            if let Some(lease) = &lease {
                lease.done(None).await;
            }
            return EXIT_NOT_FOUND;
        }
    };
    // Dies with this process: a lease that ends must not leave its command running unseen.
    #[cfg(windows)]
    let _job = child.raw_handle().and_then(swiss_core::platform::KillOnCloseJob::assign);

    if lease.is_some() {
        let added_text = if added.is_empty() {
            String::new()
        } else {
            format!("; added {}", added.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))
        };
        eprintln!("lane: running (waited {}{added_text})", fmt_secs(waited.as_secs()));
    }

    let out = child.stdout.take().map(|pipe| tokio::spawn(pump(pipe, Sink::Out, log.clone())));
    let err = child.stderr.take().map(|pipe| tokio::spawn(pump(pipe, Sink::Err, log.clone())));

    let started = Instant::now();
    let mut interrupts = 0;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            note = keep_some(&mut lease) => {
                eprintln!("lane: {note}");
                lease = None;
            }
            _ = tokio::signal::ctrl_c() => {
                // The command got the same Ctrl+C from the console; wait for it to wind down.
                // A second one means it is not listening.
                interrupts += 1;
                if interrupts >= 2 {
                    let _ = child.start_kill();
                }
            }
        }
    };
    for pump in [out, err].into_iter().flatten() {
        let _ = tokio::time::timeout(DRAIN, pump).await;
    }

    let code = match &status {
        Ok(status) => exit_code(status),
        Err(_) => None,
    };
    if let Some(lease) = &lease {
        lease.done(code).await;
    }
    let ran = started.elapsed();
    let outcome = match code {
        Some(code) => format!("exit {code}"),
        None => "killed".to_string(),
    };
    if let Some(log) = &log {
        use std::io::Write as _;
        if let Ok(mut file) = log.lock() {
            let _ = writeln!(file, "\n# {outcome} after {}", fmt_secs(ran.as_secs()));
        }
    }
    let log_text = log_path
        .as_deref()
        .filter(|_| log.is_some())
        .map(|path| format!(" - log: {}", crate::remote_cli::origin::tilde(path, &home)))
        .unwrap_or_default();
    eprintln!(
        "lane: {outcome} after {} (waited {}){log_text}",
        fmt_secs(ran.as_secs()),
        fmt_secs(waited.as_secs())
    );
    code.unwrap_or(1)
}

/// [keep] when a lease is held; never resolves without one.
async fn keep_some(lease: &mut Option<Lease>) -> &'static str {
    match lease {
        Some(lease) => keep(lease).await,
        None => std::future::pending().await,
    }
}

/// The caps the lane offers minus the ones the caller already set: the caller's choice wins.
fn caps_to_add(caps: &[(String, String)], caller_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    caps.iter().filter(|(name, _)| !caller_set(name)).cloned().collect()
}

/// The command's code; a signal on unix reads as 128 + its number, the shell's convention.
fn exit_code(status: &std::process::ExitStatus) -> Option<i32> {
    if let Some(code) = status.code() {
        return Some(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return Some(128 + signal);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum Sink {
    Out,
    Err,
}

/// Copy one pipe to this terminal and to the log, as the bytes come.
async fn pump(mut from: impl tokio::io::AsyncRead + Unpin, to: Sink, log: Option<Arc<Mutex<std::fs::File>>>) {
    use std::io::Write as _;
    use tokio::io::AsyncReadExt as _;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = &buf[..n];
        match to {
            Sink::Out => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(chunk);
                let _ = out.flush();
            }
            Sink::Err => {
                let mut err = std::io::stderr().lock();
                let _ = err.write_all(chunk);
                let _ = err.flush();
            }
        }
        if let Some(log) = &log {
            if let Ok(mut file) = log.lock() {
                let _ = file.write_all(chunk);
            }
        }
    }
}

/// A fresh log path under ~/.swiss/lane, after pruning the oldest past [LOGS_KEPT]. `None` when
/// the directory cannot be made: the run goes on without a log.
fn open_log_path(program: &str) -> Option<PathBuf> {
    let dir = swiss_core::paths::data_path(&["lane"]);
    std::fs::create_dir_all(&dir).ok()?;
    prune_logs(&dir, LOGS_KEPT.saturating_sub(1));
    Some(dir.join(log_name(program, time::OffsetDateTime::now_utc(), std::process::id())))
}

/// `20261113-120102-4242-cargo.log`: the stamp first, so a name sort is a time sort.
fn log_name(program: &str, at: time::OffsetDateTime, pid: u32) -> String {
    let stem = Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(32)
        .collect();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{pid}-{}.log",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        if stem.is_empty() { "run" } else { &stem },
    )
}

/// Keep the newest `keep` logs in `dir`.
fn prune_logs(dir: &Path, keep: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    if logs.len() <= keep {
        return;
    }
    logs.sort();
    let excess = logs.len() - keep;
    for old in logs.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

fn now_ms() -> u64 {
    swiss_core::util::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_takes_its_flags_then_the_command_whole() {
        assert_eq!(
            parse(&argv(&["run", "--", "cargo", "test", "--", "--test-threads=1"])),
            Ok(Cmd::Run(RunArgs { wait_ms: None, argv: argv(&["cargo", "test", "--", "--test-threads=1"]) }))
        );
        assert_eq!(
            parse(&argv(&["run", "--wait", "10m", "npx", "vitest", "run", "--maxWorkers=4"])),
            Ok(Cmd::Run(RunArgs { wait_ms: Some(600_000), argv: argv(&["npx", "vitest", "run", "--maxWorkers=4"]) }))
        );
        assert_eq!(
            parse(&argv(&["run", "--wait=90s", "--", "-weird"])),
            Ok(Cmd::Run(RunArgs { wait_ms: Some(90_000), argv: argv(&["-weird"]) }))
        );
    }

    #[test]
    fn a_bad_run_is_refused_with_the_reason() {
        assert!(parse(&argv(&["run"])).unwrap_err().contains("needs a command"));
        assert!(parse(&argv(&["run", "--"])).unwrap_err().contains("needs a command"));
        assert!(parse(&argv(&["run", "--wait", "soon", "cargo"])).unwrap_err().contains("--wait"));
        assert!(parse(&argv(&["run", "--wait"])).unwrap_err().contains("--wait"));
        assert!(parse(&argv(&["run", "--wait", "0", "cargo"])).unwrap_err().contains("--wait"));
        assert!(parse(&argv(&["run", "--label", "x", "cargo"])).unwrap_err().contains("--label"));
    }

    #[test]
    fn status_release_and_help_parse_strict() {
        assert_eq!(parse(&[]), Ok(Cmd::Status { json: false }));
        assert_eq!(parse(&argv(&["status", "--json"])), Ok(Cmd::Status { json: true }));
        assert_eq!(parse(&argv(&["--json"])), Ok(Cmd::Status { json: true }));
        assert!(parse(&argv(&["status", "-v"])).is_err());
        assert_eq!(parse(&argv(&["release", "#12"])), Ok(Cmd::Release(12)));
        assert!(parse(&argv(&["release"])).is_err());
        assert!(parse(&argv(&["release", "x"])).is_err());
        assert_eq!(parse(&argv(&["--help"])), Ok(Cmd::Help));
        assert!(parse(&argv(&["start"])).unwrap_err().contains("start"));
    }

    #[test]
    fn the_callers_own_setting_wins_over_a_cap() {
        let caps = vec![
            ("CARGO_BUILD_JOBS".to_string(), "2".to_string()),
            ("VITEST_MAX_WORKERS".to_string(), "4".to_string()),
        ];
        let added = caps_to_add(&caps, |name| name == "CARGO_BUILD_JOBS");
        assert_eq!(added, vec![("VITEST_MAX_WORKERS".to_string(), "4".to_string())]);
        assert_eq!(caps_to_add(&caps, |_| false), caps);
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(fmt_secs(0), "0s");
        assert_eq!(fmt_secs(59), "59s");
        assert_eq!(fmt_secs(75), "1m15s");
        assert_eq!(fmt_secs(3600 + 5 * 60 + 9), "1h05m");
    }

    #[test]
    fn the_status_says_what_runs_waits_and_ended() {
        let now = 1_000_000;
        let body = json!({
            "capacity": 1,
            "env": { "CARGO_BUILD_JOBS": "2" },
            "running": [{ "id": 3, "by": "claude", "cmd": "cargo test", "startedAt": now - 75_000 }],
            "waiting": [{ "id": 4, "by": "codex", "cmd": "npx vitest run", "enteredAt": now - 5_000 }],
            "recent": [
                { "id": 2, "by": "cli", "cmd": "cargo build", "outcome": "exit", "exit": 0, "startedAt": now - 200_000, "endedAt": now - 80_000 },
                { "id": 1, "by": "cli", "cmd": "sleep", "outcome": "gone", "startedAt": now - 300_000, "endedAt": now - 290_000 },
            ],
        });
        let lines = status_lines(&body, now, 1);
        assert_eq!(lines[0], "lane: 1 slot; caps CARGO_BUILD_JOBS=2");
        assert!(lines[1].starts_with("running  #3") && lines[1].contains("claude") && lines[1].contains("1m15s") && lines[1].ends_with("cargo test"), "{}", lines[1]);
        assert!(lines[2].starts_with("waiting  #4") && lines[2].contains("5s"), "{}", lines[2]);
        assert!(lines[3].contains("2m00s exit 0") && lines[3].ends_with("cargo build"), "{}", lines[3]);
        assert_eq!(lines.len(), 4, "recent is capped");
        let idle = status_lines(&json!({ "capacity": 2, "env": {} }), now, 5);
        assert_eq!(idle, vec!["lane: 2 slots; caps none".to_string(), "idle: a command run through the lane starts at once".to_string()]);
    }

    #[test]
    fn a_waiting_line_names_what_holds_the_slot() {
        let now = 100_000;
        let running = vec![json!({ "cmd": "cargo build", "by": "claude", "startedAt": now - 42_000 })];
        assert_eq!(waiting_line(1, &running, now), "lane: waiting - 1 ahead; running now: cargo build (claude, 42s)");
        assert_eq!(waiting_line(2, &[], now), "lane: waiting - 2 ahead");
    }

    #[test]
    fn log_names_sort_by_time_and_keep_the_program() {
        let at = time::macros::datetime!(2026-11-13 09:05:07 UTC);
        assert_eq!(log_name("cargo", at, 42), "20261113-090507-42-cargo.log");
        assert_eq!(log_name("C:/tools/npm.cmd", at, 1), "20261113-090507-1-npm.log");
        assert_eq!(log_name("weird name!", at, 1), "20261113-090507-1-weird_name_.log");
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let dir = std::env::temp_dir().join(format!("swiss-lane-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..5 {
            std::fs::write(dir.join(format!("2026111{n}-000000-1-cargo.log")), "x").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "kept").unwrap();
        prune_logs(&dir, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["20261113-000000-1-cargo.log", "20261114-000000-1-cargo.log", "notes.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lease_line_reads_into_what_was_heard() {
        // The same bytes the route writes (SPEC §lane.api).
        let read = |line: &str| parse_line(line.as_bytes());
        assert_eq!(
            read("{\"id\":7,\"state\":\"running\",\"env\":{\"A\":\"1\"}}\n"),
            Some((Some(7), Heard::Running(vec![("A".into(), "1".into())])))
        );
        assert_eq!(
            read("{\"id\":7,\"state\":\"waiting\",\"ahead\":2,\"running\":[{\"cmd\":\"x\"}]}"),
            Some((Some(7), Heard::Waiting { ahead: 2, running: vec![json!({ "cmd": "x" })] }))
        );
        assert_eq!(read("{\"id\":7,\"state\":\"released\"}"), Some((Some(7), Heard::Released)));
        assert_eq!(read("{\"id\":7,\"state\":\"closed\"}"), Some((Some(7), Heard::Closed)));
        assert_eq!(read("\n"), None, "a heartbeat");
        assert_eq!(read("{\"id\":7,\"state\":\"paused\"}"), None, "a state from a newer gateway");
        assert_eq!(read("not json"), None);
    }
}
