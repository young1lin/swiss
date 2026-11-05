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

use std::io::IsTerminal;

use serde_json::Value;

use crate::daemon::{
    self, read_creds, read_gateway_token, resolve_port, url_for, StartOptions, StartResult,
    StatusResult, StopOptions, StopResult,
};
use crate::pidfile::log_file_path;
use crate::port::as_listen_port;
use crate::skill_install::{CopyState, Places, SkillStatus};

pub const COMMANDS: &[&str] = &[
    "start",
    "stop",
    "restart",
    "status",
    "logs",
    "process",
    "token",
    "creds",
    "open",
    "gui",
    "export",
    "import",
    "skill",
    "autostart",
    "path",
    "update",
];

#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub cmd: String,
    /// The subcommand slot `skill` (`list`, `install`), `autostart` and `path` (`on`/`off`, or
    /// nothing for the status read) share.
    pub sub: Option<String>,
    /// The positional file argument for 'import'.
    pub file: Option<String>,
    /// The skills named after `skill install`.
    pub names: Vec<String>,
    /// `skill install --all`: every shipped skill, said out loud.
    pub all: bool,
    /// `skill install --installed`: refresh what is there, add nothing.
    pub installed: bool,
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
    /// A one-time sign-in link for the panel on `port` (SPEC §host.session).
    async fn login_url(&self, port: u16) -> Result<String, String>;
    fn token(&self) -> Option<String>;
    fn creds(&self) -> (String, Option<String>);
    fn export_state(&self) -> Result<Value, String>;
    fn import_state(&self, bundle: &Value) -> Result<Vec<String>, String>;
    async fn foreground(&self, port: Option<u16>);

    // The autostart and path commands carry no daemon state, so the trait ships them as
    // defaults over the library fns: RealOps needs nothing hand-written. A test fake that
    // drives these commands must override them - the defaults write the real machine. (swiss
    // update is NOT here: an async default would force Sync onto dyn Ops for every implementor,
    // and a network call has no fake worth trait plumbing.)
    fn autostart_status(&self) -> crate::autostart::AutoStartState {
        crate::autostart::status()
    }
    fn autostart_set(&self, enabled: bool) -> Result<crate::autostart::AutoStartState, String> {
        crate::autostart::set(enabled)
    }
    fn user_path_status(&self) -> crate::userpath::UserPathState {
        crate::userpath::status()
    }
    fn user_path_set(&self, enabled: bool) -> Result<crate::userpath::UserPathState, String> {
        crate::userpath::set(enabled)
    }

}

pub const USAGE: &str = "swiss — one local endpoint in front of your databases and remote MCPs

usage: swiss <command> [options]

On Windows or macOS, double-click the app or run swiss without arguments to open the GUI.

  start            start the gateway in the background and open the panel
  stop             ask it to shut down, then force it if it will not
  restart          stop, then start
  status           whether it is running, its MCPs, and what it costs in memory
  logs             show what the background gateway has been printing
  token            print the token clients authenticate with
  creds            print the panel url and the gateway token (for asking an AI)
  open             open the panel in a browser, signed in with a one-time link (SPEC §host.session)
  gui              open the panel in its own window and return (Windows and macOS;
                   on Linux run start and open the panel in a browser)
  export           dump every state file as JSON to stdout (vault values masked) — the recovery /
                   move-to-another-machine path; redirect to a file and protect it
  import <file>    restore an export on THIS machine (every file re-sealed to this machine)
  skill list [--json]
                   each AI skill this binary ships: what it does, whether it loads by itself,
                   where it is installed, and whether that copy still matches this binary
  skill install [<name>... | --all | --installed]
                   with no names, asks per skill at a terminal (default no) and refuses without
                   one. --installed refreshes the copies already there and adds none: run it
                   after replacing the executable
  autostart [on|off]
                   show, enable or disable start-at-sign-in — a registry Run value on
                   Windows, a LaunchAgent on macOS, a systemd user unit on Linux
  path [on|off]    show, add or remove swiss on your own PATH — this exe's folder in the
                   user Path on Windows, a ~/.local/bin/swiss symlink elsewhere
  update           check GitHub for a newer release; updating stays a manual exe swap
  serve            run the gateway in this process (what the daemon spawns)
  remote …         run commands and move files on SSH targets (swiss remote help)
  run …            a run's status / logs / cancel; `run audit` lists the last 7 days of
                   remote runs - who ran what, where, with what result
  api <METHOD> <path> [json | --body-file <path|->]
                   one admin API call, signed with this machine's CLI key (SPEC §host.session) -
                   what a script uses instead of curl; prints the JSON answer. --body-file reads
                   the body from a file or stdin (-), so a value stays off the command line
  secret set <name> [--from <env:VAR|file:PATH|dotenv:PATH#KEY>]
                   store a vault secret from that source or from piped stdin, never from the
                   command line; starts the gateway if it is down; prints one JSON document
  setup schema [section | plugins.<id>]
                   what a manifest may say (SPEC §host.setup)
  setup plan <file|-> [--rotate-secrets]
                   what a manifest would change on this gateway; writes nothing and starts no
                   MCP; starts the gateway if it is down; prints one JSON document
  setup apply <file|-> [--no-check] [--rotate-secrets]
                   make this gateway hold what a manifest names, then check that it works; adds
                   and updates, never removes; prints one JSON document

options
  -p, --port <n>   listen on this port (saved as the new default)
                   other commands: which instance to act on
  -f               start/gui: run in the foreground instead of detaching
                   logs:      follow the log as it grows
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
            } else if matches!(p.cmd.as_str(), "skill" | "autostart" | "path") && p.sub.is_none() {
                p.sub = Some(arg.clone());
            } else if p.cmd == "skill" {
                p.names.push(arg.clone());
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
            "--all" if p.cmd == "skill" => p.all = true,
            "--installed" if p.cmd == "skill" => p.installed = true,
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

/// One indented "label  value" line. The label column is 8 wide plus a space that is always
/// there, so a longer label pushes its value right rather than running into it.
fn row(label: &str, value: &str) -> String {
    format!("  {label:<8} {value}")
}

/// The one line the version flag prints — the build's name tag (SPEC §host.daemon): version, the
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
        // pid 0 is a gateway discovered by health alone — alive, but started outside
        // `swiss start`, so there is no record to name it by.
        StartResult::AlreadyRunning { pid: 0, url, .. } => {
            io.out("gateway already running (started outside `swiss start`)");
            io.out(&row("url", &url));
            0
        }
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
/// Open the panel signed in (SPEC §host.session): with a one-time link when the daemon mints one - the
/// token "injected" into the start - and on the bare URL, whose page says how to sign in, when
/// it cannot.
async fn open_signed_in(ops: &dyn Ops, port: u16, fallback: &str, io: &dyn Io) {
    match ops.login_url(port).await {
        Ok(url) => ops.open(&url),
        Err(err) => {
            io.err(&format!("could not mint a sign-in link ({err}); opening the sign-in page"));
            ops.open(fallback);
        }
    }
}

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
                open_signed_in(ops, port, &url, io).await;
            } else if started_ok {
                io.out(&row("sign in", "swiss open (a one-time link)"));
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
                open_signed_in(ops, port, &url, io).await;
            } else if started_ok {
                io.out(&row("sign in", "swiss open (a one-time link)"));
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
            io.out(&row("login", "swiss open (a one-time sign-in link, SPEC §host.session)"));
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
            open_signed_in(ops, port, &url_for(port), io).await;
            0
        }
        "export" => match ops.export_state() {
            Ok(bundle) => {
                io.err(
                    "warning: config passwords, the gateway token and OAuth grants are in the clear below (vault secrets are masked) — redirect to a file, protect it, delete it when done",
                );
                io.out(&serde_json::to_string_pretty(&bundle).unwrap_or_default());
                0
            }
            Err(err) => {
                io.err(&format!("nothing was exported: {err}"));
                1
            }
        },
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
                io.out(&row("where", &st.detail));
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
        "path" => match p.sub.as_deref() {
            None | Some("status") => {
                let st = ops.user_path_status();
                io.out(&format!("on your PATH: {}", if st.enabled { "yes" } else { "no" }));
                io.out(&row("where", &st.detail));
                io.out(&row("swiss", &st.dir));
                0
            }
            Some(verb @ ("on" | "off")) => match ops.user_path_set(verb == "on") {
                Ok(_) => {
                    io.out(if verb == "on" {
                        "swiss is on your PATH - open a new terminal to use it"
                    } else {
                        "swiss is off your PATH - terminals opened from now on will not find it"
                    });
                    0
                }
                Err(err) => {
                    io.err(&err);
                    1
                }
            },
            Some(other) => {
                io.err(&format!("unknown path subcommand: {other}"));
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
            let places = match Places::of_this_user() {
                Ok(places) => places,
                Err(err) => {
                    io.err(&err);
                    return 1;
                }
            };
            let mut ask = ask_at_terminal;
            let terminal = std::io::stdin().is_terminal();
            skill_command(&p, io, &places, terminal.then_some(&mut ask as &mut dyn FnMut(&str) -> String))
        }
        _ => 1,
    }
}

/// Exit code of a skill command that refused and wrote nothing, as `swiss secret set` does.
const SKILL_REFUSED: i32 = 2;

/// One question to the person at this terminal. EOF reads as an empty answer, which is a no.
fn ask_at_terminal(question: &str) -> String {
    use std::io::Write;
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    answer
}

fn how_a_skill_runs(skill: &SkillStatus) -> &'static str {
    if skill.loads_by_itself {
        "loads by itself when a task matches"
    } else {
        "runs only when you ask for it"
    }
}

/// What `skill list` prints, and what a refusal shows so the choice can be made.
fn skill_lines(rows: &[SkillStatus]) -> Vec<String> {
    let mut lines = Vec::new();
    for skill in rows {
        lines.push(format!("{} ({})", skill.name, how_a_skill_runs(skill)));
        lines.push(format!("  {}", skill.description));
        for place in &skill.locations {
            let state = match (place.state, &place.error) {
                (CopyState::Current, _) => "current".to_string(),
                (CopyState::Outdated, _) => "outdated".to_string(),
                (CopyState::Absent, _) => "not installed".to_string(),
                (CopyState::Unreadable, Some(err)) => format!("cannot check ({err})"),
                (CopyState::Unreadable, None) => "cannot check".to_string(),
            };
            lines.push(format!("  {:<8} {state:<13} {}", place.root, place.path));
        }
    }
    lines
}

/// `swiss skill …` (SPEC §host.cli). `ask` reaches the person at a terminal, and is None without
/// one: which skills land is the user's answer, so nothing here picks a default for them.
fn skill_command(
    p: &Parsed,
    io: &dyn Io,
    places: &Places,
    ask: Option<&mut dyn FnMut(&str) -> String>,
) -> i32 {
    match p.sub.as_deref() {
        Some("list") => skill_list(p, io, places),
        Some("install") => skill_install(p, io, places, ask),
        Some(other) => {
            io.err(&format!("unknown skill subcommand: {other}"));
            1
        }
        None => {
            io.err("usage: swiss skill list [--json] | swiss skill install [<name>... | --all | --installed]");
            1
        }
    }
}

fn skill_list(p: &Parsed, io: &dyn Io, places: &Places) -> i32 {
    if !p.names.is_empty() || p.all || p.installed {
        io.err("swiss skill list takes only --json");
        return SKILL_REFUSED;
    }
    let rows = places.status();
    if p.json {
        let document = serde_json::json!({ "skills": rows });
        io.out(&serde_json::to_string_pretty(&document).unwrap_or_default());
        return 0;
    }
    for line in skill_lines(&rows) {
        io.out(&line);
    }
    if rows.iter().flat_map(|s| &s.locations).any(|l| l.state == CopyState::Outdated) {
        io.out("");
        io.out("an outdated copy differs from this binary: swiss skill install --installed");
    }
    0
}

fn skill_install(
    p: &Parsed,
    io: &dyn Io,
    places: &Places,
    ask: Option<&mut dyn FnMut(&str) -> String>,
) -> i32 {
    let selectors = usize::from(!p.names.is_empty()) + usize::from(p.all) + usize::from(p.installed);
    if selectors > 1 {
        io.err("swiss skill install takes one of: names, --all, --installed");
        return SKILL_REFUSED;
    }
    if p.installed {
        return skill_refresh(io, places);
    }
    let chosen = if p.all {
        crate::skill_install::shipped_names()
    } else if !p.names.is_empty() {
        p.names.clone()
    } else if let Some(ask) = ask {
        let chosen = skill_prompt(io, places, ask);
        if chosen.is_empty() {
            io.out("no skills installed");
            return 0;
        }
        chosen
    } else {
        io.err("nothing installed: which skills land is the user's choice, and no terminal is here to ask.");
        for line in skill_lines(&places.status()) {
            io.err(&line);
        }
        io.err("Ask the user, then: swiss skill install <name>...  (or --all)");
        return SKILL_REFUSED;
    };
    match places.install_named(&chosen) {
        Ok(files) => {
            io.out("skills installed:");
            for file in &files {
                io.out(&format!("  {file}"));
            }
            0
        }
        Err(err) if err.starts_with("unknown skill") => {
            io.err(&err);
            SKILL_REFUSED
        }
        Err(err) => {
            io.err(&err);
            1
        }
    }
}

/// One question per skill, default no.
fn skill_prompt(io: &dyn Io, places: &Places, ask: &mut dyn FnMut(&str) -> String) -> Vec<String> {
    let mut chosen = Vec::new();
    for skill in places.status() {
        io.out(&format!("{} ({})", skill.name, how_a_skill_runs(&skill)));
        io.out(&format!("  {}", skill.description));
        let answer = ask(&format!("Install {}? [y/N] ", skill.name));
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            chosen.push(skill.name);
        }
    }
    chosen
}

fn skill_refresh(io: &dyn Io, places: &Places) -> i32 {
    let refresh = places.refresh_installed();
    if refresh.replaced.is_empty() && refresh.failed.is_empty() {
        io.out("nothing to refresh: no installed copy differs from this binary");
        return 0;
    }
    if !refresh.replaced.is_empty() {
        io.out("skills refreshed:");
        for file in &refresh.replaced {
            io.out(&format!("  {file}"));
        }
    }
    for failure in &refresh.failed {
        io.err(failure);
    }
    i32::from(!refresh.failed.is_empty())
}

/// The `--json` shape of status — camelCase, absent-not-null, the Node build's StatusResult,
/// plus the SPEC §host.daemon build pair (build, diskBuild and, on disagreement, buildNote).
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

/// Build the Windows "open this URL with the default handler" command.
///
/// The URL must reach the shell as one CreateProcess argument. `cmd /c start` cannot give
/// that: cmd re-parses its command line, so a URL containing `&` — every OAuth authorize
/// URL carries several — splits at the first one, the browser opens a truncated link, and
/// the remainder runs as its own command. `rundll32 url.dll,FileProtocolHandler` resolves
/// the same default handler with no shell in between, leaving `&`, `%` and friends
/// untouched. Spaces are encoded first: std quotes any argument containing one, and
/// rundll32 hands those quotes to the handler verbatim.
#[cfg(windows)]
fn windows_open_command(url: &str) -> std::process::Command {
    let encoded = if url.contains(' ') || url.contains('\t') {
        url.replace(' ', "%20").replace('\t', "%09")
    } else {
        url.to_string()
    };
    let mut c = std::process::Command::new("rundll32");
    c.args(["url.dll,FileProtocolHandler", encoded.as_str()]);
    c
}

pub(crate) fn open_in_browser(url: &str) {
    // The platform default opener, detached and hidden.
    #[cfg(windows)]
    let mut command = windows_open_command(url);
    // macOS has no xdg-open; `open` is the system opener there, and the desktop shell's
    // new-window hand-off needs it too (2026-11-01: `swiss open` on a Mac did nothing).
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(not(windows), not(target_os = "macos")))]
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
    async fn login_url(&self, port: u16) -> Result<String, String> {
        daemon::login_url(port).await
    }
    fn token(&self) -> Option<String> {
        read_gateway_token()
    }
    fn creds(&self) -> (String, Option<String>) {
        read_creds()
    }
    fn export_state(&self) -> Result<Value, String> {
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

fn gui_requested(argv: &[String]) -> bool {
    (cfg!(any(windows, target_os = "macos")) && argv.is_empty()) || matches!(argv.first().map(String::as_str), Some("gui"))
}

/// Wire the real implementations and run — the CLI entry point main.rs calls.
pub async fn main(argv: Vec<String>) -> i32 {
    // An empty desktop command line is Explorer/Finder's double-click launch. Only that exact
    // case defaults to the window; explicit flags still reach the ordinary CLI parser.
    if gui_requested(&argv) {
        let p = parse_argv(&argv);
        if p.help || p.version { return run(&argv, &PrintIo, &RealOps).await; }
        if !p.unknown.is_empty() {
            eprintln!("unknown option: {}", p.unknown.join(", "));
            return 1;
        }
        if p.bad_port {
            eprintln!("--port needs a number between 1 and 65535");
            return 1;
        }
        // Finder/Explorer owns a double-clicked app's lifetime, so that path stays in this
        // process. An explicit terminal command returns like `start`; `gui -f` opts back in.
        if argv.is_empty() || p.foreground {
            return crate::gui::run(p.port);
        }
        return crate::gui::launch(p.port);
    }
    // `remote` and `run` carry their own argv contract (SPEC §remote.cli): a bare `--` means
    // everything after it is the far side's ARGV, untouched. The generic parser would eat
    // flags out of that passthrough, so these two dispatch before it.
    match argv.first().map(String::as_str) {
        Some("remote") => return crate::remote_cli::main(argv[1..].to_vec()).await,
        Some("run") => return crate::remote_cli::run_main(argv[1..].to_vec()).await,
        // A body is free-form JSON the generic parser must not touch.
        Some("api") => return crate::remote_cli::api_main(argv[1..].to_vec()).await,
        // Refuses what the generic parser would quietly keep: a value in the wrong place.
        Some("secret") => return crate::setup_cli::secret_main(argv[1..].to_vec()).await,
        // A manifest command prints one document, and refuses a flag rather than ignoring it.
        Some("setup") => return crate::setup_cli::setup_main(argv[1..].to_vec()).await,
        // `memory` carries its own argv contract too (SPEC §memory.cli): the offline
        // CLI over the SAME store the gateway holds, opened directly - and `--stdin`
        // bodies must survive the generic parser untouched.
        Some("memory") => return crate::memory_cli::main(argv[1..].to_vec()).await,
        // `process` carries its own subcommand grammar (SPEC §process.cli).
        Some("process") => return crate::process_cli::main(argv[1..].to_vec()).await,
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

    #[cfg(windows)]
    #[test]
    fn windows_opener_hands_ampersand_urls_over_whole() {
        // OAuth authorize URLs carry several query pairs. The opener must pass the URL as
        // one argument of a non-shell program: `cmd /c start` re-parses its command line,
        // splits the URL at the first `&` and executes the remainder as a command.
        let url = "https://auth.example/oauth?client_id=1&redirect_uri=%2Fcb&state=abc";
        let command = windows_open_command(url);
        assert_eq!(command.get_program().to_str(), Some("rundll32"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0].to_str(), Some("url.dll,FileProtocolHandler"));
        assert_eq!(args[1].to_str(), Some(url));
    }

    #[cfg(windows)]
    #[test]
    fn windows_opener_encodes_spaces_so_std_never_quotes() {
        // std quotes an argument containing a space, and rundll32 hands those quotes to the
        // handler verbatim — the URL would not resolve. Spaces must leave as %20.
        let command = windows_open_command("http://127.0.0.1:1/a b?x=1");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[1].to_str(), Some("http://127.0.0.1:1/a%20b?x=1"));
        assert!(!args.iter().any(|a| a.to_str().unwrap_or("").contains(' ')));
    }

    #[test]
    fn desktop_entry_accepts_double_click_and_preserves_cli_commands() {
        assert_eq!(gui_requested(&[]), cfg!(any(windows, target_os = "macos")));
        assert!(gui_requested(&argv(&["gui"])));
        assert!(gui_requested(&argv(&["gui", "--port", "19998"])));
        for args in [&["--help"][..], &["--version"], &["status"], &["serve"], &["api", "GET", "/api/info"]] {
            assert!(!gui_requested(&argv(args)), "{args:?}");
        }
    }

    #[tokio::test]
    async fn gui_flags_finish_without_opening_a_window() {
        assert_eq!(main(argv(&["gui", "--help"])).await, 0);
        assert_eq!(main(argv(&["gui", "--version"])).await, 0);
        assert_eq!(main(argv(&["gui", "--port", "0"])).await, 1);
        assert_eq!(main(argv(&["gui", "--unknown"])).await, 1);
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
        let p = parse_argv(&argv(&["gui", "--foreground"]));
        assert!(p.foreground);

        let p = parse_argv(&argv(&["import", "bundle.json", "--port", "8080"]));
        assert_eq!(p.file.as_deref(), Some("bundle.json"));
        assert_eq!(p.port, Some(8080));

        let p = parse_argv(&argv(&["status", "--detach"]));
        assert_eq!(p.unknown, vec!["--detach"]);

        let p = parse_argv(&argv(&["start", "--port", "abc"]));
        assert!(p.bad_port);

        // gui parses like any command: a port may be named, and a bad one is flagged, not guessed.
        let p = parse_argv(&argv(&["gui", "--port", "19998"]));
        assert_eq!(p.cmd, "gui");
        assert_eq!(p.port, Some(19998));
        let p = parse_argv(&argv(&["gui", "--port", "abc"]));
        assert_eq!(p.cmd, "gui");
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

        // path takes the same on/off slot.
        let p = parse_argv(&argv(&["path", "off"]));
        assert_eq!(p.cmd, "path");
        assert_eq!(p.sub.as_deref(), Some("off"));
        assert_eq!(p.file, None);

        let p = parse_argv(&argv(&["update"]));
        assert_eq!(p.cmd, "update");

        // skill shares the same subcommand slot, and only skill takes names and these two flags.
        let p = parse_argv(&argv(&["skill", "install"]));
        assert_eq!(p.sub.as_deref(), Some("install"));
        let p = parse_argv(&argv(&["skill", "install", "swiss", "swiss-memory", "--all", "--installed"]));
        assert_eq!(p.names, ["swiss", "swiss-memory"]);
        assert!(p.all && p.installed && p.unknown.is_empty());
        let p = parse_argv(&argv(&["status", "--all", "--installed"]));
        assert_eq!(p.unknown, ["--all", "--installed"]);
    }

    /// A home of the test's own, and what the command printed.
    struct SkillBench {
        home: std::path::PathBuf,
        out: std::sync::Mutex<Vec<String>>,
        err: std::sync::Mutex<Vec<String>>,
    }

    impl Io for SkillBench {
        fn out(&self, s: &str) {
            self.out.lock().expect("out").push(s.to_string());
        }
        fn err(&self, s: &str) {
            self.err.lock().expect("err").push(s.to_string());
        }
    }

    impl Drop for SkillBench {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    impl SkillBench {
        fn new() -> Self {
            let home = std::env::temp_dir()
                .join(format!("swiss-skill-cli-{}", swiss_core::util::random_hex(8)));
            std::fs::create_dir_all(&home).expect("a home");
            Self { home, out: Default::default(), err: Default::default() }
        }

        fn places(&self) -> Places {
            Places { home: self.home.clone(), claude_config: None }
        }

        fn run(&self, args: &[&str], ask: Option<&mut dyn FnMut(&str) -> String>) -> i32 {
            let mut full = vec!["skill"];
            full.extend_from_slice(args);
            skill_command(&parse_argv(&argv(&full)), self, &self.places(), ask)
        }

        fn nothing_was_written(&self) -> bool {
            std::fs::read_dir(&self.home).expect("list the home").next().is_none()
        }

        fn out_text(&self) -> String {
            self.out.lock().expect("out").join("\n")
        }

        fn err_text(&self) -> String {
            self.err.lock().expect("err").join("\n")
        }
    }

    // SPEC §host.cli: which skills land is the user's answer, so a script gets the list, not a default.
    #[test]
    fn no_terminal_and_no_names_refuses_and_installs_nothing() {
        let bench = SkillBench::new();
        assert_eq!(bench.run(&["install"], None), SKILL_REFUSED);
        assert!(bench.nothing_was_written());
        let said = bench.err_text();
        for name in ["swiss", "swiss-remote", "swiss-memory", "swiss skill install <name>"] {
            assert!(said.contains(name), "{name}: {said}");
        }
    }

    // SPEC §host.cli: Enter on every question is a no.
    #[test]
    fn a_terminal_prompt_defaults_to_no() {
        let bench = SkillBench::new();
        let mut asked = Vec::new();
        let mut enter = |question: &str| {
            asked.push(question.to_string());
            "\n".to_string()
        };
        assert_eq!(bench.run(&["install"], Some(&mut enter)), 0);
        assert_eq!(asked.len(), 3, "one question per skill: {asked:?}");
        assert!(asked.iter().all(|q| q.contains("[y/N]")), "{asked:?}");
        assert!(bench.nothing_was_written());

        let mut only_memory = |question: &str| {
            if question.contains("swiss-memory") { "Yes\r\n".to_string() } else { "n\n".to_string() }
        };
        assert_eq!(bench.run(&["install"], Some(&mut only_memory)), 0);
        for root in [".claude", ".agents"] {
            let skills = bench.home.join(root).join("skills");
            assert!(skills.join("swiss-memory/SKILL.md").is_file(), "{root}");
            assert!(!skills.join("swiss").exists(), "{root}");
            assert!(!skills.join("swiss-remote").exists(), "{root}");
        }
    }

    #[test]
    fn a_named_install_never_asks_and_an_unknown_name_refuses() {
        let bench = SkillBench::new();
        let mut never = |question: &str| -> String { panic!("asked {question}") };
        assert_eq!(bench.run(&["install", "nope"], Some(&mut never)), SKILL_REFUSED);
        assert!(bench.nothing_was_written());
        assert_eq!(bench.run(&["install", "swiss"], Some(&mut never)), 0);
        assert!(bench.home.join(".agents/skills/swiss/SKILL.md").is_file());
        assert!(!bench.home.join(".agents/skills/swiss-memory").exists());
        assert_eq!(bench.run(&["install", "--all"], None), 0);
        assert!(bench.home.join(".claude/skills/swiss-memory/SKILL.md").is_file());
    }

    #[test]
    fn two_selectors_at_once_are_refused() {
        let bench = SkillBench::new();
        for args in [
            &["install", "swiss", "--all"][..],
            &["install", "swiss", "--installed"],
            &["install", "--all", "--installed"],
            &["list", "swiss"],
            &["list", "--all"],
        ] {
            assert_eq!(bench.run(args, None), SKILL_REFUSED, "{args:?}");
        }
        assert!(bench.nothing_was_written());
    }

    // SPEC §host.cli: the list says what a skill does, how it loads, where it is and whether that
    // copy still matches, and it points at the command that brings an old copy up to date.
    #[test]
    fn the_list_names_an_outdated_copy_and_its_remedy() {
        let bench = SkillBench::new();
        assert_eq!(bench.run(&["list"], None), 0);
        assert!(bench.nothing_was_written(), "listing creates nothing");
        assert!(bench.out_text().contains("swiss-memory (loads by itself"), "{}", bench.out_text());
        assert!(bench.out_text().contains("swiss (runs only when you ask"), "{}", bench.out_text());
        assert!(!bench.out_text().contains("--installed"), "{}", bench.out_text());

        let file = bench.home.join(".agents/skills/swiss/SKILL.md");
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("mkdir");
        std::fs::write(&file, "an older copy").expect("an old copy");
        bench.out.lock().expect("out").clear();
        assert_eq!(bench.run(&["list"], None), 0);
        assert!(bench.out_text().contains("outdated"), "{}", bench.out_text());
        assert!(bench.out_text().contains("swiss skill install --installed"), "{}", bench.out_text());

        assert_eq!(bench.run(&["install", "--installed"], None), 0);
        bench.out.lock().expect("out").clear();
        assert_eq!(bench.run(&["list", "--json"], None), 0);
        let document: Value = serde_json::from_str(&bench.out_text()).expect("one JSON document");
        let swiss = &document["skills"][0];
        assert_eq!(swiss["loadsByItself"], serde_json::json!(false));
        assert_eq!(swiss["locations"][1]["state"], serde_json::json!("current"));
        assert_eq!(swiss["locations"][0]["state"], serde_json::json!("absent"));
    }

    #[test]
    fn the_version_line_names_the_build() {
        // SPEC §host.daemon: "which build is this" is answerable from the binary itself, and the
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

    #[test]
    fn a_row_keeps_its_column_and_never_glues_a_long_label_to_its_value() {
        assert_eq!(row("pid", "42"), "  pid      42");
        assert_eq!(row("sign in", "x"), "  sign in  x");
        assert_eq!(row("12345678", "x"), "  12345678 x");
        assert_eq!(row("registered", "x"), "  registered x");
    }
}
