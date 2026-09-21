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

//! The `swiss` command line — port of `cli.ts`.
//!
//! Deliberately free of the server's module graph: `swiss status` does not build a registry or a
//! driver pool just to print a line. The server is reached as a subprocess (`start`) or over
//! loopback HTTP (everything else).

use serde_json::Value;

use crate::daemon::{
    self, read_creds, read_gateway_token, resolve_port, url_for, StartOptions, StartResult,
    StatusResult, StopOptions, StopResult,
};
use crate::pidfile::log_file_path;
use crate::port::as_listen_port;
use crate::skill_install::install_skill;

pub const COMMANDS: &[&str] = &[
    "start",
    "stop",
    "restart",
    "status",
    "logs",
    "token",
    "creds",
    "open",
    "export",
    "import",
    "skill",
    "autostart",
    "update",
];

#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub cmd: String,
    /// The subcommand slot `skill` (only `install`) and `autostart` (`on`/`off`, or
    /// nothing for the status read) share.
    pub sub: Option<String>,
    /// The positional file argument for 'import'.
    pub file: Option<String>,
    pub port: Option<u16>,
    /// A --port that was given but is not a usable port. Refused before anything is acted on.
    pub bad_port: bool,
    /// A --lines that was given but is not a positive integer — same refusal as bad_port.
    pub bad_lines: bool,
    pub foreground: bool,
    pub follow: bool,
    pub force: bool,
    pub json: bool,
    pub no_open: bool,
    pub lines: Option<u64>,
    pub help: bool,
    pub version: bool,
    pub unknown: Vec<String>,
}

/// Everything `run` says to the world, so the dispatch logic is testable on its own.
pub trait Io: Sync {
    fn out(&self, s: &str);
    fn err(&self, s: &str);
}

/// Everything `run` does to the outside world, same contract again.
#[async_trait::async_trait]
pub trait Ops {
    fn port(&self) -> u16;
    async fn start(&self, opts: StartOptions) -> StartResult;
    async fn stop(&self, opts: StopOptions) -> StopResult;
    async fn status(&self, port: u16) -> StatusResult;
    async fn logs(&self, port: u16, lines: u64, follow: bool, io: &dyn Io);
    fn open(&self, url: &str);
    fn token(&self) -> Option<String>;
    fn creds(&self) -> (String, Option<String>);
    fn export_state(&self) -> Value;
    fn import_state(&self, bundle: &Value) -> Result<Vec<String>, String>;
    async fn foreground(&self, port: Option<u16>);
    fn skill_install(&self) -> Result<Vec<String>, String>;

    // The autostart commands carry no daemon state, so the trait ships them as defaults over
    // the library fns: RealOps needs nothing hand-written, and a test fake inherits real
    // behaviour. (swiss update is NOT here: an async default would force Sync onto dyn Ops
    // for every implementor, and a network call has no fake worth trait plumbing.)
    fn autostart_status(&self) -> crate::autostart::AutoStartState {
        crate::autostart::status()
    }
    fn autostart_set(&self, enabled: bool) -> Result<crate::autostart::AutoStartState, String> {
        crate::autostart::set(enabled)
    }

    // The one-time `~/.mcp-gateway` -> `~/.swiss` move (swiss_core::paths). Only RealOps
    // does it: the default is a no-op so a test fake can never reach the operator's home.
    fn migrate_home(&self) -> swiss_core::paths::LegacyHomeMigration {
        swiss_core::paths::LegacyHomeMigration::NotNeeded
    }
}

/// One human line for what the home migration did — nothing for the common no-op.
fn report_home_migration(m: swiss_core::paths::LegacyHomeMigration, io: &dyn Io) {
    use swiss_core::paths::LegacyHomeMigration as M;
    match m {
        M::NotNeeded => {}
        M::Moved { from, to } => io.out(&format!(
            "moved the state home {} -> {}",
            from.display(),
            to.display()
        )),
        M::Blocked { pid, port } => io.err(&format!(
            "state home not moved to ~/{}: a gateway (pid {pid}, port {port}) is still running out of the old one; stop it and start again",
            swiss_core::paths::HOME_DIR_NAME
        )),
        M::Failed { from, to, err } => io.err(&format!(
            "could not move the state home {} -> {}: {err}; still serving from the old one",
            from.display(),
            to.display()
        )),
    }
}

pub const USAGE: &str = "swiss — one local endpoint in front of your databases and remote MCPs

usage: swiss <command> [options]

  start            start the gateway in the background and open the panel
  stop             ask it to shut down, then force it if it will not
  restart          stop, then start
  status           whether it is running, its MCPs, and what it costs in memory
  logs             show what the background gateway has been printing
  token            print the token clients authenticate with
  creds            print the panel url and the gateway token (for asking an AI)
  open             open the panel in a browser
  export           dump every state file as plaintext JSON to stdout — the recovery /
                   move-to-another-machine path; redirect to a file and protect it
  import <file>    restore an export on THIS machine (every file re-sealed to this machine)
  skill install    copy the shipped AI skill to ~/.agents/skills, ~/.claude/skills, ~/.cursor/skills
  autostart [on|off]
                   show, enable or disable start-at-sign-in — a registry Run value on
                   Windows, a LaunchAgent on macOS, a systemd user unit on Linux
  update           check GitHub for a newer release; updating stays a manual exe swap
  serve            run the gateway in this process (what the daemon spawns)

options
  -p, --port <n>   listen on this port (saved as the new default)
                   other commands: which instance to act on
  -f               start: run in the foreground instead of detaching
                   logs:  follow the log as it grows
      --no-open    start: do not open the panel in a browser
  -n, --lines <n>  logs: how many lines to show first (default 200)
      --force      stop: kill a live pid even if it cannot be confirmed as this gateway
      --json       status: emit JSON
  -h, --help       this text
  -v, --version    print the version
";

/// Parse argv without a dependency. Unknown flags are collected rather than ignored: silently
/// dropping `--detach` on a command whose whole point is detaching would be the worst answer.
pub fn parse_argv(argv: &[String]) -> Parsed {
    let mut p = Parsed::default();
    let mut i = 0usize;
    while i < argv.len() {
        let arg = &argv[i];
        if !arg.starts_with('-') {
            if p.cmd.is_empty() {
                p.cmd = arg.clone();
            } else if (p.cmd == "skill" || p.cmd == "autostart") && p.sub.is_none() {
                p.sub = Some(arg.clone());
            } else if p.file.is_none() {
                p.file = Some(arg.clone());
            }
            i += 1;
            continue;
        }
        // --key=value and "--key value" are the same thing.
        let (flag, inline_value) = match arg.find('=') {
            Some(eq) => (&arg[..eq], Some(arg[eq + 1..].to_string())),
            None => (arg.as_str(), None),
        };
        // Consume the next argv as this flag's value — but never a following FLAG: `logs -n
        // --json` used to eat --json as -n's value and silently drop it.
        let value = |argv: &[String], i: &mut usize| -> String {
            if let Some(v) = &inline_value {
                return v.clone();
            }
            if *i + 1 < argv.len() && !argv[*i + 1].starts_with('-') {
                *i += 1;
                return argv[*i].clone();
            }
            String::new()
        };
        match flag {
            "-p" | "--port" => match as_listen_port(&value(argv, &mut i)) {
                Some(port) => p.port = Some(port),
                None => p.bad_port = true,
            },
            "-n" | "--lines" => {
                // Strict digits, like the port: 0x10 and 1e2 are refusals, not 16 and 100.
                let raw = value(argv, &mut i);
                let ok = !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit());
                match if ok {
                    raw.parse::<u64>().ok().filter(|n| *n > 0)
                } else {
                    None
                } {
                    Some(n) => p.lines = Some(n),
                    None => p.bad_lines = true,
                }
            }
            // One letter, two conventional meanings — each the expected one for its command.
            "-f" => {
                p.foreground = true;
                p.follow = true;
            }
            "--foreground" => p.foreground = true,
            "--follow" => p.follow = true,
            "--force" => p.force = true,
            "--json" => p.json = true,
            "--no-open" => p.no_open = true,
            "-h" | "--help" => p.help = true,
            "-v" | "--version" => p.version = true,
            other => p.unknown.push(other.to_string()),
        }
        i += 1;
    }
    p
}

fn fmt_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 24 {
        return format!("{h}h {}m", m % 60);
    }
    format!("{}d {}h", h / 24, h % 24)
}

fn row(label: &str, value: &str) -> String {
    format!("  {label:<9}{value}")
}

/// The one line the version flag prints — the build's name tag (docs/16 H3): version, the
/// git hash build.rs stamped in, and the build time. The parenthesised pair is what the
/// status command and scripts/deploy.ps1 parse, so this shape is a small public contract.
pub fn version_line() -> String {
    format!(
        "swiss {} ({}, {})",
        env!("CARGO_PKG_VERSION"),
        env!("SWISS_GIT_HASH"),
        env!("SWISS_BUILD_TIME")
    )
}

pub fn render_status(st: &StatusResult) -> String {
    let mut lines = vec![
        "gateway running".to_string(),
        row("url", st.url.as_deref().unwrap_or(&url_for(st.port))),
    ];
    if let Some(pid) = st.pid {
        let up = st
            .uptime_ms
            .map(|ms| format!("   (up {})", fmt_duration(ms)))
            .unwrap_or_default();
        lines.push(row("pid", &format!("{pid}{up}")));
    }
    if let Some(Value::Object(memory)) = &st.memory {
        if let Some(gateway_mb) = memory.get("gatewayMb").and_then(Value::as_f64) {
            let kids = memory
                .get("childrenMb")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let procs = memory
                .get("processCount")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            lines.push(row(
                "memory",
                &format!("{gateway_mb} MB gateway + {kids} MB children ({procs} proc)"),
            ));
        }
    }
    if let Some(build) = &st.build {
        let hash = build.get("hash").and_then(Value::as_str).unwrap_or("?");
        let time = build.get("time").and_then(Value::as_str).unwrap_or("?");
        lines.push(row("build", &format!("{hash} ({time})")));
    }
    if let Some(note) = &st.build_note {
        lines.push(format!("note: {note}"));
    }
    lines.push(row("log", &st.log_file));

    let entries = st
        .health
        .as_ref()
        .and_then(|h| h.get("health"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !entries.is_empty() {
        let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for e in &entries {
            let state = e
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            *counts.entry(state).or_insert(0) += 1;
        }
        let summary = counts
            .iter()
            .map(|(state, n)| format!("{n} {state}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(row("mcps", &summary));
        let width = entries
            .iter()
            .map(|e| {
                e.get("name")
                    .and_then(Value::as_str)
                    .map(str::len)
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        for e in &entries {
            let name = e.get("name").and_then(Value::as_str).unwrap_or("?");
            let state = e.get("state").and_then(Value::as_str).unwrap_or("?");
            let latency = e
                .get("latencyMs")
                .and_then(Value::as_i64)
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_default();
            lines.push(format!(
                "    {name:<width$}  {state:<8}{latency}",
                width = width
            ));
        }
    }
    lines.join("\n")
}

/// A daemon started out of the npx cache stops being restartable as soon as that cache turns
/// over, which is exactly the failure you do not want from something you expect to be running.
/// (Node's cli.ts printed the same three lines before startDaemon; both start and restart
/// spawn one, so both warn.)
fn warn_if_npx_cache(io: &dyn Io) {
    let entry = crate::daemon::server_entry();
    if crate::daemon::is_npx_cache_path(&entry) {
        io.err(&format!(
            "warning: running from npm's npx cache ({}).",
            entry.display()
        ));
        io.err(
            "         That directory is version-keyed and cleared on update, so this daemon will",
        );
        io.err("         not survive it. Install it properly instead: npm i -g <package>");
        io.err("");
    }
}

fn report_start(r: StartResult, io: &dyn Io) -> i32 {
    match r {
        StartResult::AlreadyRunning { pid, url, .. } => {
            io.out(&format!("gateway already running (pid {pid})"));
            io.out(&row("url", &url));
            0
        }
        StartResult::Started { pid, port, url } => {
            io.out("gateway started");
            io.out(&row("url", &url));
            io.out(&row("pid", &pid.to_string()));
            io.out(&row("log", &log_file_path(port).to_string_lossy()));
            0
        }
        StartResult::Failed { port, log_tail } => {
            io.err(&format!("gateway failed to start on port {port}"));
            let trimmed = log_tail.trim();
            if !trimmed.is_empty() {
                io.err("");
                io.err(trimmed.trim_end());
            }
            io.err("");
            io.err(&format!("full log: {}", log_file_path(port).display()));
            1
        }
    }
}

fn report_stop(r: StopResult, io: &dyn Io) -> i32 {
    match r {
        StopResult::Stopped { pid, .. } => {
            io.out(&format!("gateway stopped (pid {pid})"));
            0
        }
        StopResult::Forced { pid, .. } => {
            io.out(&format!(
                "gateway did not shut down in time — forced (pid {pid} and its child processes)"
            ));
            0
        }
        StopResult::NotRunning { port } => {
            io.out(&format!("gateway not running on port {port}"));
            3
        }
        StopResult::Refused { reason, .. } => {
            io.err(&reason);
            1
        }
    }
}

/// Decide and report. Returns the process exit code: 0 done, 1 refused or failed, 3 nothing
/// running — the third one so `swiss status` is usable in a script without parsing text.
pub async fn run(argv: &[String], io: &dyn Io, ops: &dyn Ops) -> i32 {
    let p = parse_argv(argv);

    if p.help {
        io.out(USAGE);
        return 0;
    }
    if p.version {
        io.out(&version_line());
        return 0;
    }
    if !p.unknown.is_empty() {
        io.err(&format!("unknown option: {}", p.unknown.join(", ")));
        io.err(USAGE);
        return 1;
    }
    if p.bad_port {
        io.err("--port takes a number between 1 and 65535");
        return 1;
    }
    if p.bad_lines {
        io.err("--lines takes a positive whole number of lines");
        return 1;
    }
    if p.cmd.is_empty() {
        io.out(USAGE);
        return 1;
    }
    if !COMMANDS.contains(&p.cmd.as_str()) {
        io.err(&format!("unknown command: {}", p.cmd));
        io.err(USAGE);
        return 1;
    }

    // start lets the daemon resolve its own port from the config it is about to serve; every
    // other command needs a concrete port up front, because it has to find the pid file to act on.
    let port = p.port.unwrap_or_else(|| ops.port());

    match p.cmd.as_str() {
        "start" => {
            if p.foreground {
                ops.foreground(p.port).await;
                return 0;
            }
            warn_if_npx_cache(io);
            report_home_migration(ops.migrate_home(), io);
            let r = ops
                .start(StartOptions {
                    port: p.port,
                    ..Default::default()
                })
                .await;
            let started_ok = matches!(
                r,
                StartResult::Started { .. } | StartResult::AlreadyRunning { .. }
            );
            let url = match &r {
                StartResult::Started { url, .. } | StartResult::AlreadyRunning { url, .. } => {
                    url.clone()
                }
                StartResult::Failed { .. } => String::new(),
            };
            let code = report_start(r, io);
            if started_ok && !p.no_open {
                ops.open(&url);
            }
            code
        }
        "stop" => report_stop(
            ops.stop(StopOptions {
                port: Some(port),
                force: p.force,
                ..Default::default()
            })
            .await,
            io,
        ),
        "restart" => {
            // A restart must work whether or not anything was running, so "not-running" is not a
            // failure here — only a refusal is, and that means we would be leaving a process
            // behind.
            let stopped = ops
                .stop(StopOptions {
                    port: Some(port),
                    force: p.force,
                    ..Default::default()
                })
                .await;
            if let StopResult::Refused { .. } = &stopped {
                return report_stop(stopped, io);
            }
            warn_if_npx_cache(io);
            report_home_migration(ops.migrate_home(), io);
            let r = ops
                .start(StartOptions {
                    port: p.port,
                    ..Default::default()
                })
                .await;
            let started_ok = matches!(
                r,
                StartResult::Started { .. } | StartResult::AlreadyRunning { .. }
            );
            let url = match &r {
                StartResult::Started { url, .. } | StartResult::AlreadyRunning { url, .. } => {
                    url.clone()
                }
                StartResult::Failed { .. } => String::new(),
            };
            let code = report_start(r, io);
            if started_ok && !p.no_open {
                ops.open(&url);
            }
            code
        }
        "status" => {
            let st = ops.status(port).await;
            if p.json {
                io.out(&serde_json::to_string_pretty(&status_json(&st)).unwrap_or_default());
                return if st.running { 0 } else { 3 };
            }
            if !st.running {
                io.out(&format!(
                    "gateway not running (nothing answering on port {})",
                    st.port
                ));
                io.out(&row("log", &st.log_file));
                return 3;
            }
            io.out(&render_status(&st));
            0
        }
        "logs" => {
            ops.logs(port, p.lines.unwrap_or(200), p.follow, io).await;
            0
        }
        "token" => {
            let Some(token) = ops.token() else {
                io.err("no token found — start the gateway once and it will generate one");
                return 1;
            };
            io.out(&token);
            0
        }
        "creds" => {
            let (url, token) = ops.creds();
            io.out(&row("url", &url));
            io.out(&row("login", "(none — the panel is loopback-only)"));
            match token {
                Some(token) => {
                    io.out(&row("token", &token));
                    0
                }
                None => {
                    io.out(&row("token", "(none — start the gateway once)"));
                    1
                }
            }
        }
        "open" => {
            ops.open(&url_for(port));
            0
        }
        "export" => {
            io.err(
                "warning: everything below is plaintext secrets — redirect to a file, protect it, delete it when done",
            );
            io.out(&serde_json::to_string_pretty(&ops.export_state()).unwrap_or_default());
            0
        }
        "import" => {
            let Some(file) = &p.file else {
                io.err("usage: swiss import <file written by swiss export>");
                return 1;
            };
            let text = match std::fs::read_to_string(file) {
                Ok(text) => text,
                Err(err) => {
                    io.err(&format!("cannot read {file}: {err}"));
                    return 1;
                }
            };
            let bundle: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(err) => {
                    io.err(&format!("cannot read {file}: {err}"));
                    return 1;
                }
            };
            match ops.import_state(&bundle) {
                Ok(restored) => {
                    let joined = restored.join(", ");
                    io.out(&format!(
                        "restored (sealed to this machine): {}",
                        if joined.is_empty() {
                            "nothing".into()
                        } else {
                            joined
                        }
                    ));
                    0
                }
                Err(err) => {
                    io.err(&err);
                    1
                }
            }
        }
        "autostart" => match p.sub.as_deref() {
            None | Some("status") => {
                let st = ops.autostart_status();
                io.out(&format!(
                    "start at sign-in: {}",
                    if st.enabled { "on" } else { "off" }
                ));
                io.out(&row("registered", &st.detail));
                io.out(&row("command", &st.command));
                0
            }
            Some("on") => match ops.autostart_set(true) {
                Ok(_) => {
                    io.out("start at sign-in enabled");
                    0
                }
                Err(err) => {
                    io.err(&err);
                    1
                }
            },
            Some("off") => match ops.autostart_set(false) {
                Ok(_) => {
                    io.out("start at sign-in disabled");
                    0
                }
                Err(err) => {
                    io.err(&err);
                    1
                }
            },
            Some(other) => {
                io.err(&format!("unknown autostart subcommand: {other}"));
                1
            }
        },
        "update" => match crate::update_check::check().await {
            Ok(info) => {
                for line in info.render().lines() {
                    io.out(line);
                }
                0
            }
            Err(err) => {
                io.err(&err);
                1
            }
        },
        "skill" => {
            if p.sub.as_deref() != Some("install") {
                match p.sub {
                    Some(sub) => io.err(&format!("unknown skill subcommand: {sub}")),
                    None => io.err("usage: swiss skill install"),
                }
                return 1;
            }
            match ops.skill_install() {
                Ok(targets) => {
                    io.out("skill installed:");
                    for t in &targets {
                        io.out(&format!("  {t}"));
                    }
                    0
                }
                Err(err) => {
                    io.err(&err);
                    1
                }
            }
        }
        _ => 1,
    }
}

/// The `--json` shape of status — camelCase, absent-not-null, the Node build's StatusResult,
/// plus the docs/16 H3 build pair (build, diskBuild and, on disagreement, buildNote).
pub(crate) fn status_json(st: &StatusResult) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("running".into(), Value::Bool(st.running));
    out.insert("port".into(), serde_json::json!(st.port));
    if let Some(pid) = st.pid {
        out.insert("pid".into(), serde_json::json!(pid));
    }
    if let Some(entry) = &st.entry {
        out.insert("entry".into(), serde_json::json!(entry));
    }
    if let Some(started) = &st.started_at {
        out.insert("startedAt".into(), serde_json::json!(started));
    }
    if let Some(uptime) = st.uptime_ms {
        out.insert("uptimeMs".into(), serde_json::json!(uptime));
    }
    if let Some(url) = &st.url {
        out.insert("url".into(), serde_json::json!(url));
    }
    out.insert("logFile".into(), serde_json::json!(st.log_file));
    if let Some(build) = &st.build {
        out.insert("build".into(), build.clone());
    }
    if let Some(disk) = &st.disk_build {
        out.insert("diskBuild".into(), serde_json::json!(disk));
    }
    if let Some(note) = &st.build_note {
        out.insert("buildNote".into(), serde_json::json!(note));
    }
    if let Some(health) = &st.health {
        out.insert("health".into(), health.clone());
    }
    if let Some(memory) = &st.memory {
        out.insert("memory".into(), memory.clone());
    }
    Value::Object(out)
}

/// Show the background gateway's output. Polls rather than watching: file-change notifications
/// do not reliably fire for appends on Windows, and a poll is what `--follow` costs anyway.
pub async fn tail_log(port: u16, lines: u64, follow: bool, io: &dyn Io) {
    let file = log_file_path(port);
    let mut offset = 0u64;
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let all: Vec<&str> = text.split(['\r', '\n']).collect();
            let take = all.len().saturating_sub(lines as usize);
            io.out(all[take..].join("\n").trim_end());
            offset = text.len() as u64;
        }
        Err(_) => {
            // No file yet. Without --follow that is the whole answer; WITH it, the common case
            // is "swiss start just fired and the child has not written its first line" — wait for
            // the file instead of quitting.
            if !follow {
                io.err(&format!("no log yet at {}", file.display()));
                return;
            }
            io.err(&format!("waiting for {} …", file.display()));
        }
    }
    if follow {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let Ok(meta) = std::fs::metadata(&file) else {
                continue; // mid-rotation
            };
            let size = meta.len();
            if size < offset {
                offset = 0; // rotated out from under us
            }
            if size == offset {
                continue;
            }
            // Read just the new bytes.
            use std::io::{Read, Seek, SeekFrom};
            if let Ok(mut f) = std::fs::File::open(&file) {
                if f.seek(SeekFrom::Start(offset)).is_ok() {
                    let mut buf = Vec::with_capacity((size - offset) as usize);
                    if f.read_to_end(&mut buf).is_ok() {
                        io.out(String::from_utf8_lossy(&buf).trim_end());
                    }
                }
            }
            offset = size;
        }
    }
}

fn open_in_browser(url: &str) {
    // The platform default opener, detached and hidden.
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/c", "start", "", url]);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The real-world Ops: daemon.rs functions + this-binary foreground.
pub struct RealOps;

#[async_trait::async_trait]
impl Ops for RealOps {
    fn port(&self) -> u16 {
        resolve_port()
    }
    fn migrate_home(&self) -> swiss_core::paths::LegacyHomeMigration {
        swiss_core::paths::migrate_legacy_home()
    }
    async fn start(&self, opts: StartOptions) -> StartResult {
        daemon::start_daemon(opts).await
    }
    async fn stop(&self, opts: StopOptions) -> StopResult {
        daemon::stop_daemon(opts).await
    }
    async fn status(&self, port: u16) -> StatusResult {
        daemon::daemon_status(port).await
    }
    async fn logs(&self, port: u16, lines: u64, follow: bool, io: &dyn Io) {
        tail_log(port, lines, follow, io).await;
    }
    fn open(&self, url: &str) {
        open_in_browser(url);
    }
    fn token(&self) -> Option<String> {
        read_gateway_token()
    }
    fn creds(&self) -> (String, Option<String>) {
        read_creds()
    }
    fn export_state(&self) -> Value {
        daemon::export_state()
    }
    fn import_state(&self, bundle: &Value) -> Result<Vec<String>, String> {
        daemon::import_state(bundle)
    }
    async fn foreground(&self, port: Option<u16>) {
        // Run the server in this process: no detach, output on this terminal. What a Scheduled
        // Task or a systemd unit should invoke, since those supply their own supervision. Those
        // also must not pop a browser, so the CLI never auto-opens in the foreground.
        if let Some(port) = port {
            daemon::persist_listen_port(port);
            // SAFETY: this runs before any thread exists (the runtime boots the server below),
            // so no other thread can observe the write concurrently.
            unsafe { std::env::set_var("SWISS_PORT", port.to_string()) };
        }
        if let Err(err) = crate::server::run_gateway().await {
            swiss_core::log::error("fatal", Some(serde_json::json!({ "err": err })));
            std::process::exit(1);
        }
    }
    fn skill_install(&self) -> Result<Vec<String>, String> {
        install_skill()
    }
}

struct PrintIo;
impl Io for PrintIo {
    fn out(&self, s: &str) {
        println!("{s}");
    }
    fn err(&self, s: &str) {
        eprintln!("{s}");
    }
}

/// Wire the real implementations and run — the CLI entry point main.rs calls.
pub async fn main(argv: Vec<String>) -> i32 {
    // `remote` and `run` carry their own argv contract (docs/34 SS26): a bare `--` means
    // everything after it is the far side's ARGV, untouched. The generic parser would eat
    // flags out of that passthrough, so these two dispatch before it.
    match argv.first().map(String::as_str) {
        Some("remote") => return crate::remote_cli::main(argv[1..].to_vec()).await,
        Some("run") => return crate::remote_cli::run_main(argv[1..].to_vec()).await,
        _ => {}
    }
    run(&argv, &PrintIo, &RealOps).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_parse_in_both_spellings() {
        let p = parse_argv(&argv(&["start", "--port=19998", "--no-open"]));
        assert_eq!(p.cmd, "start");
        assert_eq!(p.port, Some(19998));
        assert!(p.no_open);
        assert!(!p.bad_port);

        let p = parse_argv(&argv(&["logs", "-n", "50", "--json"]));
        assert_eq!(p.lines, Some(50));
        assert!(p.json);

        // A flag value never eats the next flag.
        let p = parse_argv(&argv(&["logs", "-n", "--json"]));
        assert!(p.bad_lines);
        assert!(p.json);

        // -f means both foreground and follow.
        let p = parse_argv(&argv(&["logs", "-f"]));
        assert!(p.follow);
        let p = parse_argv(&argv(&["start", "-f"]));
        assert!(p.foreground);

        let p = parse_argv(&argv(&["import", "bundle.json", "--port", "8080"]));
        assert_eq!(p.file.as_deref(), Some("bundle.json"));
        assert_eq!(p.port, Some(8080));

        let p = parse_argv(&argv(&["status", "--detach"]));
        assert_eq!(p.unknown, vec!["--detach"]);

        let p = parse_argv(&argv(&["start", "--port", "abc"]));
        assert!(p.bad_port);
    }

    #[test]
    fn autostart_and_update_parse() {
        let p = parse_argv(&argv(&["autostart", "on"]));
        assert_eq!(p.cmd, "autostart");
        assert_eq!(p.sub.as_deref(), Some("on"));

        // Bare autostart is the status read.
        let p = parse_argv(&argv(&["autostart"]));
        assert_eq!(p.sub, None);

        let p = parse_argv(&argv(&["update"]));
        assert_eq!(p.cmd, "update");

        // skill shares the same subcommand slot.
        let p = parse_argv(&argv(&["skill", "install"]));
        assert_eq!(p.sub.as_deref(), Some("install"));
    }

    #[test]
    fn the_version_line_names_the_build() {
        // docs/16 H3: "which build is this" is answerable from the binary itself, and the
        // parenthesised pair is exactly what the status command and deploy.ps1 parse.
        let line = version_line();
        let rest = line
            .strip_prefix(&format!("swiss {} (", env!("CARGO_PKG_VERSION")))
            .unwrap_or_else(|| panic!("not the version shape: {line}"));
        let (hash, time) = rest
            .split_once(", ")
            .unwrap_or_else(|| panic!("no hash/time pair: {line}"));
        let time = time
            .strip_suffix(')')
            .unwrap_or_else(|| panic!("unterminated: {line}"));
        // A git-less box builds "unknown"; a repo builds a short hex id, optionally -dirty.
        let bare = hash.strip_suffix("-dirty").unwrap_or(hash);
        assert!(
            hash == "unknown" || (bare.len() >= 7 && bare.bytes().all(|b| b.is_ascii_hexdigit())),
            "hash not a build stamp: {line}"
        );
        assert_eq!(time.len(), 20, "RFC 3339 UTC: {line}");
        assert!(time.ends_with('Z'), "{line}");
    }

    #[test]
    fn durations_render() {
        assert_eq!(fmt_duration(500), "0s");
        assert_eq!(fmt_duration(90_000), "1m");
        assert_eq!(fmt_duration(3_600_000), "1h 0m");
        assert_eq!(fmt_duration(90_000_000), "1d 1h");
    }
}
