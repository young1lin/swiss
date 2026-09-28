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

//! Start, stop and inspect the gateway as a background process — port of `daemon.ts`.
//!
//! The gateway is a long-lived local server, so the CLI's job is to detach one and then be able
//! to find it again from any directory later. Nothing here starts the server's module graph in
//! THIS process — a `status` call must stay cheap — so it talks to a running instance the same
//! way any other client does: over loopback HTTP.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::pidfile::{
    is_pid_alive, log_file_path, read_pid_file, remove_pid_file, write_pid_file, PidRecord,
};
use crate::port::{as_listen_port_value, env_listen_port, DEFAULT_PORT};
use swiss_core::paths::{data_dir, data_path};
use swiss_core::platform::tree_kill;
use swiss_core::secure::envstore::{read_env_store, write_env_store};
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

const POLL_MS: u64 = 150;
const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
const LOG_TAIL_BYTES: usize = 4000;

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub port: Option<u16>,
    /// Server executable to run. Defaults to this binary; injected by tests.
    pub entry: Option<std::path::PathBuf>,
    pub args: Vec<String>,
    /// How long to wait for the daemon to answer /health before calling the start failed.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct StopOptions {
    pub port: Option<u16>,
    /// How long to let a graceful shutdown finish before force-killing the tree.
    pub grace_period_ms: Option<u64>,
    /// Kill a live pid even when we cannot confirm it is still our gateway.
    pub force: bool,
}

pub enum StartResult {
    Started { pid: u32, port: u16, url: String },
    AlreadyRunning { pid: u32, port: u16, url: String },
    Failed { port: u16, log_tail: String },
}

pub enum StopResult {
    Stopped { pid: u32, port: u16 },
    Forced { pid: u32, port: u16 },
    NotRunning { port: u16 },
    Refused { pid: u32, port: u16, reason: String },
}

pub struct StatusResult {
    pub running: bool,
    pub port: u16,
    pub pid: Option<u32>,
    pub entry: Option<String>,
    pub started_at: Option<String>,
    pub uptime_ms: Option<u64>,
    pub url: Option<String>,
    pub log_file: String,
    pub health: Option<Value>,
    pub memory: Option<Value>,
    /// The RUNNING daemon's build, straight off /health (docs/16 H3).
    pub build: Option<Value>,
    /// The build of the entry ON DISK, from running the entry's own --version.
    pub disk_build: Option<String>,
    /// Set when the two disagree — "I rebuilt but never restarted", made visible.
    pub build_note: Option<String>,
}

/// Pull the git hash out of a version line ("swiss 0.1.0 (23047d8, 2026-09-11T13:16:40Z)") —
/// the exact pair shape version_line() prints; writer and reader in two files, one format,
/// so this parser is the contract's other half.
pub fn parse_version_hash(line: &str) -> Option<&str> {
    let start = line.find('(')? + 1;
    let rest = &line[start..];
    let end = rest.find(',')?;
    let hash = rest[..end].trim();
    (!hash.is_empty()).then_some(hash)
}

/// The build of the gateway binary ON DISK: run the entry's own --version and read its hash
/// (docs/16 H3). A missing entry or a non-gateway placeholder answers None — never a guess.
fn disk_build_of(entry: &str) -> Option<String> {
    let out = std::process::Command::new(entry)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    parse_version_hash(&line).map(str::to_string)
}

pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// The port to act on when none was given. Env (`SWISS_PORT`) wins, then the config file,
/// then 19999. Read straight out of the file rather than through load_config(), because every
/// command here must work even when the config is invalid — `swiss stop` on a gateway whose config
/// you just broke is exactly when you need it most.
pub fn resolve_port() -> u16 {
    env_listen_port()
        .or_else(|| read_config_raw().and_then(|c| c.get("port").and_then(as_listen_port_value)))
        .unwrap_or(DEFAULT_PORT)
}

/// Write `port` into gateway.config.json so the next `swiss start` (and the child about to spawn)
/// listens there. No-op when there is no config yet — first-run seed reads `SWISS_PORT`.
pub fn persist_listen_port(port: u16) {
    let path = data_path(&["gateway.config.json"]);
    if !path.exists() {
        return;
    }
    let Ok(Some(mut raw)) = read_secure_json(&path) else {
        return; // a file this machine cannot decrypt is not ours to rewrite
    };
    let Some(obj) = raw.as_object_mut() else {
        return;
    };
    if obj.get("port").and_then(Value::as_u64) == Some(port as u64) {
        return;
    }
    obj.insert("port".into(), json!(port));
    // Sealed + atomic like every other state write: a torn gateway.config.json is the one file
    // this gateway cannot boot through (loadConfig has no catch), and this rewrite runs on every
    // --port change.
    let _ = write_secure_json(&path, &raw);
}

fn read_config_raw() -> Option<Value> {
    read_secure_json(&data_path(&["gateway.config.json"]))
        .ok()
        .flatten()
}

/// The gateway executable to spawn: this very binary. The Node build had a .js entry beside the
/// CLI; a single-binary release points at itself.
pub fn server_entry() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("swiss"))
}

/// The attribution a detached serve child logs at boot: THIS process's pid and argv, as
/// JSON. The launcher exits right after the spawn by design, so the environment - captured
/// at spawn - is the only channel that outlives it; today's dual-deploy incident (two
/// sessions starting the same production four minutes apart) was diagnosed from session
/// transcripts only because no daemon could say who had launched it.
pub fn spawner_env_value() -> String {
    serde_json::json!({
        "pid": std::process::id(),
        "argv": std::env::args().collect::<Vec<_>>(),
    })
    .to_string()
}

/// True for a path inside npm's npx cache, which is version-keyed and cleared on update — a
/// daemon started from there stops being restartable the moment the cache turns over.
pub fn is_npx_cache_path(path: &Path) -> bool {
    path.to_string_lossy().contains("\\_npx\\") || path.to_string_lossy().contains("/_npx/")
}

/// The env store path handed to read_env_store/write_env_store (the sealed .env replacement).
fn env_store() -> std::collections::HashMap<String, String> {
    read_env_store(&swiss_core::secure::envstore::env_store_path())
}

/// The token a client needs for the authenticated endpoints. Precedence mirrors the server boot:
/// a rotation persisted in managed.json wins over the .env seed. Read as data, not through the
/// managed store, to keep the CLI free of the server's module graph.
fn read_env_key(key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    // An empty stored value counts as missing — token_lookup on the serve side treats it the
    // same way, so a deliberately-empty entry cannot make the CLI print an empty token.
    env_store().get(key).filter(|v| !v.is_empty()).cloned()
}

pub fn read_gateway_token() -> Option<String> {
    if let Ok(Some(managed)) = read_secure_json(&data_path(&["managed.json"])) {
        if let Some(token) = managed.get("token").and_then(Value::as_str) {
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    let name = read_config_raw()
        .as_ref()
        .and_then(|c| c.get("tokenEnv"))
        .and_then(Value::as_str)
        .unwrap_or(swiss_host::config::TOKEN_ENV)
        .to_string();
    read_env_key(&name)
}

/// Panel URL + bearer token, for `swiss creds`. Never dumps the rest of the env store (DB
/// passwords live there). The panel signs in with a one-time link (`swiss open`, docs/48), so
/// there is no username or password to print.
pub fn read_creds() -> (String, Option<String>) {
    (url_for(resolve_port()), read_gateway_token())
}

/// What a vault value exports as (the owner's call, 2026-09-28): a secret is known to the
/// person who typed it and to the machine that sealed it, and to nobody holding the file. The
/// bundle carries the NAMES so a restore says what to re-enter, never the values.
pub const SECRET_MASK: &str = "******";

/// One decrypted bundle of every state file — what 'swiss export' writes and 'swiss import' reads.
/// Machine binding cuts both ways, so moving to a new machine (or recovering from a lost OS
/// credential) needs an operator-initiated export on a machine that can still read the files.
/// The vault is the exception: its values leave as [SECRET_MASK] and are typed in again on the
/// far side. Everything else rides whole - config passwords, the gateway token, OAuth grants -
/// so the bundle is still a file to protect.
pub fn export_state() -> Value {
    json!({
        "version": 1,
        "exportedAt": swiss_core::log::iso_now(),
        "config": read_secure_json(&data_path(&["gateway.config.json"])).ok().flatten(),
        "managed": read_secure_json(&data_path(&["managed.json"])).ok().flatten(),
        "tunnels": read_secure_json(&data_path(&["tunnels.json"])).ok().flatten(),
        // OAuth grants ride too (docs/24 D6): values included for the same reason the vault
        // is — this bundle is already the one plaintext escape, and without this section a
        // machine move would silently drop every MCP grant.
        "oauth": read_secure_json(&swiss_mcp::oauth::store_path()).ok().flatten(),
        "env": Value::Object(env_store()
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect()),
        // The vault rides by NAME only (docs/19 D7, revised 2026-09-28): every value is
        // SECRET_MASK, so a bundle read by anyone - a backup, a terminal scrollback, a
        // support attachment - hands over no key. An import re-enters them by hand.
        "secrets": masked_vault(),
    })
}

/// The vault section of the bundle: the stored shape with every value replaced by the mask.
fn masked_vault() -> Value {
    let stored = read_secure_json(&swiss_core::secure::secretstore::secret_store_path())
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({ "rev": 0, "secrets": {} }));
    let names = stored
        .get("secrets")
        .and_then(Value::as_object)
        .map(|m| {
            m.keys()
                .map(|k| (k.clone(), Value::String(SECRET_MASK.to_string())))
                .collect::<serde_json::Map<String, Value>>()
        })
        .unwrap_or_default();
    json!({ "rev": stored.get("rev").cloned().unwrap_or_else(|| json!(0)), "secrets": names })
}

/// Restore a bundle: every section is re-sealed under THIS machine's key as it lands. Returns
/// the restored file names.
pub fn import_state(bundle: &Value) -> Result<Vec<String>, String> {
    let Some(obj) = bundle.as_object() else {
        return Err("not a state bundle (expected JSON written by 'swiss export')".into());
    };
    if obj.get("version").and_then(Value::as_i64) != Some(1) {
        return Err(format!(
            "unsupported bundle version: {}",
            obj.get("version")
                .map(Value::to_string)
                .unwrap_or_else(|| "null".into())
        ));
    }
    let mut restored: Vec<String> = Vec::new();
    let mut seal = |key: &str, file: &str| -> Result<(), String> {
        match obj.get(key) {
            Some(value) if !value.is_null() => {
                write_secure_json(&data_path(&[file]), value)?;
                restored.push(file.to_string());
                Ok(())
            }
            _ => Ok(()),
        }
    };
    seal("config", "gateway.config.json")?;
    seal("managed", "managed.json")?;
    seal("tunnels", "tunnels.json")?;
    // A running gateway reads OAuth credentials from its in-memory map, so an import while
    // it runs lands on restart — exactly the vault's behavior for the same shape.
    seal("oauth", "mcp-oauth.json")?;
    if let Some(env) = obj.get("env").filter(|v| v.is_object()) {
        // Imported values win over whatever is already stored.
        let mut merged = env_store();
        for (k, v) in env.as_object().expect("checked object") {
            if let Some(s) = v.as_str() {
                merged.insert(k.clone(), s.to_string());
            }
        }
        write_env_store(&merged, &swiss_core::secure::envstore::env_store_path())?;
        restored.push("env.json".into());
    }
    if let Some(vault) = obj.get("secrets") {
        // Accept the export shape ({rev, secrets: {name -> value}}) or a bare map; imported
        // values win, nothing is deleted (docs/19 D7 — a restore, not a mirror). The static
        // empty map is only for a section that is neither — a borrowed Map::new() would die
        // at the end of the expression.
        let entries: serde_json::Map<String, Value> = match vault.get("secrets") {
            Some(inner) => inner.as_object().cloned().unwrap_or_default(),
            None => vault.as_object().cloned().unwrap_or_default(),
        };
        // A masked entry carries no value, so it must not become one: writing SECRET_MASK
        // would replace a good local secret with six asterisks. Those names are skipped and
        // whatever this machine holds for them stays (2026-09-28).
        let entries: serde_json::Map<String, Value> = entries
            .into_iter()
            .filter(|(_, v)| v.as_str() != Some(SECRET_MASK))
            .collect();
        if !entries.is_empty() {
            swiss_core::secure::secretstore::import_secrets(
                &swiss_core::secure::secretstore::secret_store_path(),
                &entries,
            )
            .map_err(|e| e.message())?;
            restored.push("secrets.json".into());
        }
    }
    Ok(restored)
}

async fn health_ok(client: &reqwest::Client, port: u16) -> bool {
    client
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map(|res| res.status().is_success())
        .unwrap_or(false)
}

/// Poll /health until it answers. `stop` lets the caller abandon early — a daemon that has
/// already exited is never going to answer, and waiting out the full timeout for it wastes the
/// operator's time when what they need is the log.
pub async fn wait_for_health(
    client: &reqwest::Client,
    port: u16,
    timeout_ms: u64,
    mut stop: impl FnMut() -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if health_ok(client, port).await {
            return true;
        }
        if stop() {
            return false;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}

/// Poll until /health stops answering — i.e. the listener is really gone, not just asked to go.
async fn wait_for_port_closed(client: &reqwest::Client, port: u16, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if !health_ok(client, port).await {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}

async fn wait_until_dead(pid: u32, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if !is_pid_alive(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}

/// Keep one previous log rather than growing without bound; rotation at start is enough for a
/// file only this process appends to.
fn rotate_if_big(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= LOG_MAX_BYTES {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
        }
    }
}

fn tail_log(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    if text.len() > LOG_TAIL_BYTES {
        // Slide to a char boundary so a truncated tail is still valid UTF-8.
        let mut start = text.len() - LOG_TAIL_BYTES;
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_string()
    } else {
        text
    }
}

/// Detach a gateway and wait until it actually serves — port of `startDaemon`.
///
/// The spawn options are each load-bearing on Windows: CREATE_NEW_PROCESS_GROUP detaches the
/// child from our console's Ctrl-C group (closing the terminal must not signal it),
/// CREATE_NO_WINDOW keeps it invisible, and routing stdio to a file means no inherited terminal
/// handles (and gives `swiss logs` something to read). No job object is attached, so the child
/// keeps running when this process exits.
pub async fn start_daemon(opts: StartOptions) -> StartResult {
    if let Some(port) = opts.port {
        persist_listen_port(port);
    }
    let port = opts.port.unwrap_or_else(resolve_port);
    let entry = opts.entry.unwrap_or_else(server_entry);
    let timeout_ms = opts.timeout_ms.unwrap_or(30_000);
    let client = reqwest::Client::new();

    let existing = read_pid_file(port);
    if let Some(rec) = &existing {
        if is_pid_alive(rec.pid) && health_ok(&client, port).await {
            return StartResult::AlreadyRunning {
                pid: rec.pid,
                port,
                url: url_for(port),
            };
        }
    }
    // A pid file whose process is gone (or whose port answers nothing) is a leftover from an
    // unclean kill. Believing it would refuse every future start.
    if existing.is_some() {
        remove_pid_file(port);
    }

    let _ = std::fs::create_dir_all(data_dir());
    let log = log_file_path(port);
    rotate_if_big(&log);
    // Both streams land in the log file (append); if it cannot be opened the child runs silent
    // rather than inheriting this terminal's handles.
    let open_log = |path: &Path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(std::process::Stdio::from)
            .unwrap_or(std::process::Stdio::null())
    };
    let mut command = std::process::Command::new(&entry);
    command
        .arg("serve")
        .args(&opts.args)
        .stdin(std::process::Stdio::null())
        .stdout(open_log(&log))
        .stderr(open_log(&log));
    // The daemon's environment is this machine's, not this shell's (docs/16 §1): an agent or
    // CI launcher must not speak for every child the gateway will ever spawn. The serve path
    // repeats the scrub in its own process, so this covers the spawn even where main() grew a
    // regression.
    swiss_core::env::scrub_command(&mut command);
    // Launcher attribution for the boot log - set AFTER the scrub, like SWISS_PORT below,
    // so the blacklist cannot strike what we just wrote.
    command.env(swiss_core::env::SPAWNER_ENV, spawner_env_value());
    if let Some(port) = opts.port {
        // So the child listens where we asked, including a first run that has no config to
        // persist into. The serve child reads SWISS_PORT first.
        command.env("SWISS_PORT", port.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW — the detach plus the hidden console.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        // The log redirects above set the CHILD's stdio; they do not stop it inheriting OUR
        // std handles as well (bInheritHandles is TRUE for every configured spawn). Under a
        // wrapper reading this CLI through a pipe - deploy.ps1 in a shell that captures its
        // output, CI, `| tee` - the daemon then held the wrapper's pipe open for its whole life
        // and `swiss start` looked hung after it had finished (2026-09-20). Ours stay ours.
        swiss_core::platform::keep_std_handles_from_children();
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return StartResult::Failed {
                port,
                log_tail: format!("spawn failed: {err}\n{}", tail_log(&log)),
            }
        }
    };
    let pid = child.id();
    // The detached child is NOT waited on and no watcher thread is spawned: this CLI stays
    // alive polling health anyway, and "child already exited" is exactly what the pid probe in
    // the stop closure below reports.
    let _ = child;

    write_pid_file(&PidRecord {
        pid,
        port,
        entry: entry.to_string_lossy().into_owned(),
        node: entry.to_string_lossy().into_owned(),
        started_at: swiss_core::log::iso_now(),
    });

    if wait_for_health(&client, port, timeout_ms, || !is_pid_alive(pid)).await {
        // Two racing `swiss start`s both reach here: the loser's child died on EADDRINUSE while
        // the WINNER answered the health poll — the port being up proves nothing about whose
        // child it is. Only our own child still standing counts as "started"; otherwise this is
        // a failure with a pid file that must go (it points at the dead loser, not the daemon
        // that serves the port).
        if is_pid_alive(pid) {
            return StartResult::Started {
                pid,
                port,
                url: url_for(port),
            };
        }
        remove_pid_file(port);
        return StartResult::Failed {
            port,
            log_tail: tail_log(&log),
        };
    }
    // Never leave a pid file for something that never came up: the next start would report
    // already-running and the operator would have nothing to act on.
    remove_pid_file(port);
    StartResult::Failed {
        port,
        log_tail: tail_log(&log),
    }
}

/// The CLI key the running daemon accepts on /api/* (docs/48), unsealed from `session.json`
/// under this home. None before the first start, or when the file cannot be opened.
pub fn read_cli_key() -> Option<String> {
    crate::session::read_cli_key(&data_path(&[crate::session::SESSION_FILE]))
}

/// Attach the CLI key to an admin request - every /api/* call the CLI makes goes through here.
pub fn with_cli_key(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match read_cli_key() {
        Some(key) => req.header(crate::session::CLI_KEY_HEADER, key),
        None => req,
    }
}

/// A one-time sign-in link for the panel (docs/48): the daemon mints a login token, good for
/// one use within two minutes, behind the CLI key. What `swiss start` and `swiss open` open.
pub async fn login_url(port: u16) -> Result<String, String> {
    let client = reqwest::Client::new();
    let res = with_cli_key(
        client
            .post(format!("http://127.0.0.1:{port}/api/session/ticket"))
            .timeout(Duration::from_secs(5)),
    )
    .send()
    .await
    .map_err(|e| format!("cannot reach the gateway on port {port}: {e}"))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!(
            "{} {}",
            status.as_u16(),
            body["error"].as_str().unwrap_or("no detail")
        ));
    }
    body["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the gateway answered without a sign-in link".to_string())
}

/// Ask the gateway to shut itself down. Failure is fine — the caller falls back to a tree-kill.
async fn post_shutdown(client: &reqwest::Client, port: u16) {
    let token = read_gateway_token();
    let mut req = with_cli_key(
        client
            .post(format!("http://127.0.0.1:{port}/api/shutdown"))
            .timeout(Duration::from_secs(5)),
    );
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let _ = req.send().await; // it may well have closed the socket as it exited — that is success
}

/// Stop the daemon, preferring the graceful path — port of `stopDaemon`.
///
/// Windows has no deliverable SIGTERM (a TerminateProcess is unconditional), so asking the
/// gateway over HTTP is the only way it gets to run its own shutdown — closing adapters and
/// tree-killing proc-MCP children. A tree-kill is the floor, not the plan: skipping the
/// graceful step is how orphaned MCP subtrees happen.
pub async fn stop_daemon(opts: StopOptions) -> StopResult {
    let port = opts.port.unwrap_or_else(resolve_port);
    let client = reqwest::Client::new();
    let Some(rec) = read_pid_file(port) else {
        return StopResult::NotRunning { port };
    };

    if !health_ok(&client, port).await {
        if !is_pid_alive(rec.pid) {
            remove_pid_file(port); // stale record from a killed daemon
            return StopResult::NotRunning { port };
        }
        // The pid is alive but nothing serves on its port, so we cannot prove it is still our
        // gateway rather than a number the OS recycled. Killing on a guess is how a tool takes
        // down unrelated work; make the operator say so.
        if !opts.force {
            return StopResult::Refused {
                pid: rec.pid,
                port,
                reason: format!(
                    "pid {} is alive but nothing is answering on port {port}, so it cannot be confirmed \
                     as this gateway. Re-run with --force to kill it anyway.",
                    rec.pid
                ),
            };
        }
        tree_kill(rec.pid);
        wait_until_dead(rec.pid, 3000).await;
        remove_pid_file(port);
        return StopResult::Forced { pid: rec.pid, port };
    }

    post_shutdown(&client, port).await;
    if wait_for_port_closed(&client, port, opts.grace_period_ms.unwrap_or(10_000)).await {
        remove_pid_file(port);
        return StopResult::Stopped { pid: rec.pid, port };
    }
    tree_kill(rec.pid);
    wait_until_dead(rec.pid, 3000).await;
    remove_pid_file(port);
    StopResult::Forced { pid: rec.pid, port }
}

async fn get_json(
    client: &reqwest::Client,
    port: u16,
    path: &str,
    token: Option<&str>,
) -> Option<Value> {
    let mut req = with_cli_key(
        client
            .get(format!("http://127.0.0.1:{port}{path}"))
            .timeout(Duration::from_secs(5)),
    );
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let res = req.send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json::<Value>().await.ok()
}

/// What `swiss status` prints. "Running" means it answered — not that a pid file exists, which is
/// a claim about the past.
pub async fn daemon_status(port: u16) -> StatusResult {
    let client = reqwest::Client::new();
    let rec = read_pid_file(port);
    let mut result = StatusResult {
        running: false,
        port,
        pid: rec.as_ref().map(|r| r.pid),
        entry: rec.as_ref().map(|r| r.entry.clone()),
        started_at: rec.as_ref().map(|r| r.started_at.clone()),
        uptime_ms: None,
        url: None,
        log_file: log_file_path(port).to_string_lossy().into_owned(),
        memory: None,
        health: None,
        build: None,
        disk_build: None,
        build_note: None,
    };
    if let Some(rec) = &rec {
        if let Some(started) = swiss_core::util::parse_iso_ms(&rec.started_at) {
            let now = swiss_core::util::now_ms() as i64;
            result.uptime_ms = Some((now - started).max(0) as u64);
        }
    }

    let live = get_json(&client, port, "/health", None).await;
    if live
        .as_ref()
        .and_then(|v| v.get("ok"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return result;
    }

    let token = read_gateway_token();
    let listed = get_json(&client, port, "/api/mcps", token.as_deref()).await;
    let memory = get_json(&client, port, "/api/memory?tree=1", token.as_deref()).await;
    result.running = true;
    result.url = Some(url_for(port));
    result.health = Some(json!({ "ok": true, "health": listed
        .as_ref()
        .and_then(|v| v.get("mcps"))
        .cloned()
        .unwrap_or_else(|| json!([])) }));
    result.memory = memory;

    // The build pair (docs/16 H3): what the daemon REPORTS it is, versus what the entry on
    // disk would report. A disagreement is the deploy that never happened — exactly the
    // thing status exists to surface.
    result.build = live.as_ref().and_then(|v| v.get("build")).cloned();
    if let Some(entry) = result.entry.clone() {
        if let Some(disk) = disk_build_of(&entry) {
            let running = result
                .build
                .as_ref()
                .and_then(|b| b.get("hash"))
                .and_then(Value::as_str);
            if let Some(running) = running.filter(|running| *running != disk) {
                result.build_note = Some(format!(
                    "the binary on disk is {disk}; the running daemon is {running} — restart to pick it up"
                ));
            }
            result.disk_build = Some(disk);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    // Ported from test/daemon.test.ts.
    //
    // SAFETY: no test here may reach `tree_kill` with a live pid — it would kill the test runner.
    // Two rules keep that true: the pid planted in a record is either dead, or belongs to a path
    // that provably never signals (a refusal, an already-running report, a status read). The
    // `Forced` branch is therefore not exercised; nothing but a real daemon is safe to force-kill.
    use super::*;

    #[test]
    fn spawner_env_value_carries_this_process_pid_and_argv() {
        // The value is JSON the serve path parses straight back out at boot: the round trip
        // must yield exactly our pid and our argv, or the boot log would attribute the
        // daemon to the wrong launcher.
        let parsed: serde_json::Value = serde_json::from_str(&spawner_env_value()).unwrap();
        assert_eq!(parsed["pid"].as_u64(), Some(u64::from(std::process::id())));
        assert_eq!(
            parsed["argv"].as_array().map(|a| a.len()),
            Some(std::env::args().count())
        );
    }

    /// The state files and the pid files all live in the one scratch data dir the whole test
    /// binary shares, and the port/token env vars are process-wide, so every test that writes
    /// one takes this first and starts from a machine that has never run the gateway.
    async fn daemon_state() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = swiss_core::paths::DATA_DIR_LOCK.lock().await;
        swiss_core::paths::test_home();
        // Pin the key before anything seals, so no test here depends on DPAPI or on which other
        // test happened to install the deterministic key first.
        swiss_core::secure::key::use_test_master_key();
        clear_state();
        guard
    }

    /// Remove every file and variable this module reads.
    fn clear_state() {
        for file in ["gateway.config.json", "managed.json", "tunnels.json"] {
            let _ = std::fs::remove_file(data_path(&[file]));
        }
        let _ = std::fs::remove_file(swiss_core::secure::envstore::env_store_path());
        for key in [
            "SWISS_PORT",
            "SWISS_TOKEN",
            "SWISS_TEST_TOKEN",
        ] {
            unsafe { std::env::remove_var(key) };
        }
    }

    /// A port nothing is listening on: bound, read back, released.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("the loopback has a free port")
            .local_addr()
            .expect("a bound listener has an address")
            .port()
    }

    fn seal(file: &str, value: Value) {
        write_secure_json(&data_path(&[file]), &value).expect("seal a state file");
    }

    fn seal_env(pairs: &[(&str, &str)]) {
        let store = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        write_env_store(&store, &swiss_core::secure::envstore::env_store_path())
            .expect("seal the env");
    }

    fn plant(port: u16, pid: u32) -> u32 {
        write_pid_file(&PidRecord {
            pid,
            port,
            entry: server_entry().to_string_lossy().into_owned(),
            node: server_entry().to_string_lossy().into_owned(),
            started_at: swiss_core::log::iso_now(),
        });
        pid
    }

    /// A pid file whose pid cannot be alive: every stop path that signals checks liveness first,
    /// so a dead pid keeps a test off the kill branch entirely.
    fn plant_dead_pid(port: u16) -> u32 {
        plant(port, 2_147_483_647)
    }

    /// A pid file naming THIS process, for the paths that must report a live daemon without ever
    /// signalling it.
    fn plant_own_pid(port: u16) -> u32 {
        plant(port, std::process::id())
    }

    /// A loopback server standing in for a running gateway: the endpoints the CLI talks to, with
    /// a /api/shutdown that really does stop the listener, so the graceful stop is exercised end
    /// to end rather than mocked.
    struct FakeGateway {
        port: u16,
        serving: tokio::task::JoinHandle<()>,
    }

    impl FakeGateway {
        async fn start() -> Self {
            use axum::routing::{get, post};
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let signal = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
            let app = axum::Router::new()
                .route(
                    "/health",
                    // Carries a build stamp the way the real one does (docs/16 H3), so the
                    // status path has a running-build to compare the disk entry against.
                    get(|| async {
                        axum::Json(json!({
                            "ok": true,
                            "build": { "hash": "aaaaaaa", "time": "2026-01-01T00:00:00Z" }
                        }))
                    }),
                )
                .route(
                    "/api/mcps",
                    get(|| async { axum::Json(json!({ "mcps": [{ "name": "echo" }] })) }),
                )
                .route(
                    "/api/memory",
                    get(|| async { axum::Json(json!({ "gatewayMb": 12.5 })) }),
                )
                .route(
                    "/api/shutdown",
                    post(move || {
                        let signal = signal.clone();
                        async move {
                            if let Some(tx) = signal.lock().ok().and_then(|mut g| g.take()) {
                                let _ = tx.send(());
                            }
                            axum::Json(json!({ "ok": true }))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let port = listener.local_addr().expect("a bound address").port();
            let serving = tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self { port, serving }
        }

        fn stop(self) {
            self.serving.abort();
        }
    }

    // --- ports and paths ---------------------------------------------------------------------

    #[test]
    fn the_url_is_the_loopback_root() {
        assert_eq!(url_for(19999), "http://127.0.0.1:19999/");
    }

    #[test]
    fn spots_a_path_inside_the_npx_cache() {
        // A daemon started from there stops being restartable the moment the cache turns over.
        assert!(is_npx_cache_path(Path::new(
            "C:\\Users\\x\\AppData\\Local\\npm-cache\\_npx\\a1b2\\swiss.exe"
        )));
        assert!(is_npx_cache_path(Path::new("/home/x/.npm/_npx/a1b2/swiss")));
        assert!(!is_npx_cache_path(Path::new("C:\\tools\\swiss.exe")));
        assert!(!is_npx_cache_path(Path::new("/usr/local/bin/swiss")));
        // "_npx" inside a longer name is not the cache.
        assert!(!is_npx_cache_path(Path::new("/home/x/my_npx_tools/swiss")));
    }

    #[test]
    fn the_server_entry_is_this_very_binary() {
        // The Node build had a .js entry beside the CLI; a single-binary release points at itself.
        assert_eq!(
            server_entry(),
            std::env::current_exe().expect("a running test has an exe")
        );
    }

    #[tokio::test]
    async fn resolves_the_port_from_the_env_then_the_config_then_the_default() {
        let _lock = daemon_state().await;
        assert_eq!(resolve_port(), DEFAULT_PORT); // nothing names one anywhere

        seal(
            "gateway.config.json",
            json!({ "port": 18080, "tokenEnv": "SWISS_TOKEN" }),
        );
        assert_eq!(resolve_port(), 18080);

        unsafe { std::env::set_var("SWISS_PORT", "18081") };
        assert_eq!(resolve_port(), 18081); // the env wins over the file
        unsafe { std::env::set_var("SWISS_PORT", "not-a-port") };
        assert_eq!(resolve_port(), 18080); // an unusable env value is not an override
        clear_state();
    }

    #[tokio::test]
    async fn persists_a_listen_port_by_rewriting_only_that_field() {
        let _lock = daemon_state().await;
        let path = data_path(&["gateway.config.json"]);
        seal(
            "gateway.config.json",
            json!({
                "port": 19999,
                "tokenEnv": "SWISS_TOKEN",
                "servers": { "keeper": { "type": "echo", "password": "${KEEPER_PASS}" } },
            }),
        );

        persist_listen_port(18082);
        let after = read_secure_json(&path)
            .expect("the config is readable")
            .expect("the config is there");
        assert_eq!(after["port"], json!(18082));
        // Everything else survives verbatim — the ${ENV} reference above all, which is why this
        // rewrite is surgical rather than a re-serialize of the loaded config.
        assert_eq!(after["tokenEnv"], json!("SWISS_TOKEN"));
        assert_eq!(
            after["servers"]["keeper"],
            json!({ "type": "echo", "password": "${KEEPER_PASS}" })
        );
        clear_state();
    }

    #[tokio::test]
    async fn persisting_a_port_is_a_no_op_before_there_is_a_config() {
        // First run reads SWISS_PORT; seeding a half-built config here would hand the
        // bootstrap a file to adopt that the user never wrote.
        let _lock = daemon_state().await;
        persist_listen_port(18083);
        assert!(!data_path(&["gateway.config.json"]).exists());
    }

    // --- credentials -------------------------------------------------------------------------

    #[tokio::test]
    async fn reads_the_token_from_managed_json_before_the_env_store() {
        // Precedence mirrors the server boot: a rotation persisted in managed.json wins over the
        // seed the env store holds, or `swiss creds` would print a token that no longer works.
        let _lock = daemon_state().await;
        seal_env(&[("SWISS_TOKEN", "from-env-store")]);
        assert_eq!(read_gateway_token().as_deref(), Some("from-env-store"));

        seal("managed.json", json!({ "token": "rotated" }));
        assert_eq!(read_gateway_token().as_deref(), Some("rotated"));

        // An empty token is not a token: fall through rather than authenticate with "".
        seal("managed.json", json!({ "token": "" }));
        assert_eq!(read_gateway_token().as_deref(), Some("from-env-store"));

        // The config names which variable holds the seed, and that name is honoured.
        let _ = std::fs::remove_file(data_path(&["managed.json"]));
        seal(
            "gateway.config.json",
            json!({ "tokenEnv": "SWISS_TEST_TOKEN" }),
        );
        seal_env(&[("SWISS_TEST_TOKEN", "named-var")]);
        assert_eq!(read_gateway_token().as_deref(), Some("named-var"));
        clear_state();
    }

    #[tokio::test]
    async fn the_node_era_token_name_no_longer_pins() {
        // The pairing window closed: only the name the config carries resolves, on the CLI
        // exactly as on the serve side.
        let _lock = daemon_state().await;
        seal("gateway.config.json", json!({ "tokenEnv": "SWISS_TOKEN" }));
        seal_env(&[("MCP_GATEWAY_TOKEN", "legacy-pin")]);
        assert_eq!(read_gateway_token(), None);
        clear_state();
    }

    #[tokio::test]
    async fn an_empty_stored_value_is_missing_not_a_token() {
        // The store can hold an empty entry under the config's name; the serve side
        // (token_lookup) treats empty as missing, and the CLI must agree — otherwise creds
        // prints an empty token while serve refuses it.
        let _lock = daemon_state().await;
        seal(
            "gateway.config.json",
            json!({ "tokenEnv": "SWISS_TOKEN" }),
        );
        seal_env(&[("SWISS_TOKEN", "")]);
        assert_eq!(read_gateway_token(), None);
        clear_state();
    }

    #[tokio::test]
    async fn a_custom_token_env_name_gets_no_fallback() {
        // Only the name the config carries resolves: a config naming its own variable must
        // not quietly authenticate with a token pinned under a name its operator never wrote.
        let _lock = daemon_state().await;
        seal("gateway.config.json", json!({ "tokenEnv": "MY_OWN_TOKEN" }));
        seal_env(&[("SWISS_TOKEN", "new-pin")]);
        assert_eq!(read_gateway_token(), None);
        clear_state();
    }

    #[tokio::test]
    async fn creds_report_the_panel_url_and_the_token_and_nothing_else() {
        // The rest of the env store is DB passwords; `swiss creds` prints what a client needs to
        // connect and stops there. The panel signs in with a one-time link, so there is no
        // password to print.
        let _lock = daemon_state().await;
        seal_env(&[("SWISS_TOKEN", "tok"), ("DB_PASSWORD", "hunter2")]);

        let (url, token) = read_creds();
        assert_eq!(url, url_for(DEFAULT_PORT));
        assert_eq!(token.as_deref(), Some("tok"));
        clear_state();
    }

    // --- export / import ---------------------------------------------------------------------

    #[tokio::test]
    async fn exports_every_state_file_and_imports_it_back_sealed() {
        let _lock = daemon_state().await;
        seal("gateway.config.json", json!({ "port": 18084 }));
        seal("managed.json", json!({ "mcps": [] }));
        seal("tunnels.json", json!({ "connections": [] }));
        seal(
            "mcp-oauth.json",
            json!({ "figma": { "client_id": "cid", "access_token": "at" } }),
        );
        seal_env(&[("SWISS_TOKEN", "tok")]);

        let bundle = export_state();
        assert_eq!(bundle["version"], json!(1));
        assert_eq!(bundle["config"]["port"], json!(18084));
        assert_eq!(bundle["managed"], json!({ "mcps": [] }));
        assert_eq!(bundle["tunnels"], json!({ "connections": [] }));
        // OAuth grants are state like any other (docs/24 D6): without this section a machine
        // move would silently drop every grant the operator consented to.
        assert_eq!(bundle["oauth"]["figma"]["client_id"], json!("cid"));
        assert_eq!(bundle["env"]["SWISS_TOKEN"], json!("tok"));

        // A machine with nothing on it takes the bundle and ends up with the same state — the
        // point of the export being the one plaintext path out of the sealed files.
        clear_state();
        let restored = import_state(&bundle).expect("the bundle imports");
        assert_eq!(
            restored,
            vec![
                "gateway.config.json",
                "managed.json",
                "tunnels.json",
                "mcp-oauth.json",
                "env.json",
            ],
            "the vault exports as masks, so an import restores no secrets file"
        );
        assert_eq!(export_state()["config"]["port"], json!(18084));
        assert_eq!(read_gateway_token().as_deref(), Some("tok"));
        clear_state();
    }

    #[tokio::test]
    async fn the_vault_rides_the_bundle_and_an_import_merges_not_mirrors() {
        let _lock = daemon_state().await;
        // Two secrets on the source machine; the bundle carries one of them, changed.
        seal(
            "secrets.json",
            json!({ "rev": 4, "secrets": { "stripe-key": "sk_old", "keep-me": "v" } }),
        );
        let bundle = export_state();
        assert_eq!(
            bundle["secrets"]["secrets"]["stripe-key"],
            json!(SECRET_MASK),
            "the name rides, the key does not"
        );

        // The target machine already holds the other name: a restore keeps it, and the
        // imported value wins on the name both have (docs/19 D7).
        seal(
            "secrets.json",
            json!({ "rev": 1, "secrets": { "keep-me": "target-value", "local-only": "x" } }),
        );
        import_state(&json!({
            "version": 1,
            "secrets": { "secrets": { "stripe-key": "sk_new", "keep-me": "source-value" } }
        }))
        .expect("the bundle imports");

        let vault = stored_vault();
        assert_eq!(vault["stripe-key"], json!("sk_new")); // imported
        assert_eq!(vault["keep-me"], json!("source-value")); // imported wins
        assert_eq!(vault["local-only"], json!("x")); // nothing is deleted
                                                     // A tampered entry (bad name) is skipped without costing the rest.
        import_state(&json!({
            "version": 1,
            "secrets": { "secrets": { "not_a_name": "y", "fresh-one": "z" } }
        }))
        .expect("the bundle imports");
        let vault = stored_vault();
        assert_eq!(vault.get("not_a_name"), None);
        assert_eq!(vault["fresh-one"], json!("z"));
        clear_state();
    }

    /// The vault as it sits on disk - what export no longer shows.
    fn stored_vault() -> Value {
        read_secure_json(&swiss_core::secure::secretstore::secret_store_path())
            .ok()
            .flatten()
            .map(|v| v["secrets"].clone())
            .unwrap_or_else(|| json!({}))
    }

    #[tokio::test]
    async fn a_masked_value_never_lands_on_the_secret_it_names() {
        // The bundle a second machine reads says WHICH secrets existed and nothing about what
        // they were. Importing it must leave that machine's own vault alone - the failure this
        // guards is an import quietly setting every secret to "******".
        let _lock = daemon_state().await;
        seal(
            "secrets.json",
            json!({ "rev": 2, "secrets": { "zhipu-key": "real-value", "other": "v2" } }),
        );
        let bundle = export_state();
        let out = serde_json::to_string(&bundle).expect("serialises");
        assert!(!out.contains("real-value"), "no value in the bundle: {out}");
        assert_eq!(
            bundle["secrets"]["secrets"]["zhipu-key"],
            json!(SECRET_MASK)
        );
        assert_eq!(bundle["secrets"]["secrets"]["other"], json!(SECRET_MASK));

        let restored = import_state(&bundle).expect("the bundle imports");
        assert!(
            !restored.contains(&"secrets.json".to_string()),
            "nothing to restore from masks: {restored:?}"
        );
        assert_eq!(stored_vault()["zhipu-key"], json!("real-value"));
        assert_eq!(stored_vault()["other"], json!("v2"));

        // A bundle that mixes a real value in (an operator filling one back by hand) takes
        // that one and leaves the masked names alone.
        import_state(&json!({
            "version": 1,
            "secrets": { "secrets": { "zhipu-key": SECRET_MASK, "other": "typed-again" } }
        }))
        .expect("the bundle imports");
        assert_eq!(stored_vault()["zhipu-key"], json!("real-value"));
        assert_eq!(stored_vault()["other"], json!("typed-again"));
        clear_state();
    }

    #[tokio::test]
    async fn an_import_leaves_the_env_entries_the_bundle_says_nothing_about() {
        // The env store is shared with the DB adapters, so a bundle carrying only the gateway
        // token must not take a machine's database passwords with it.
        let _lock = daemon_state().await;
        seal_env(&[("DB_PASSWORD", "hunter2"), ("SWISS_TOKEN", "old")]);
        import_state(&json!({ "version": 1, "env": { "SWISS_TOKEN": "new" } }))
            .expect("the bundle imports");

        let env = export_state()["env"].clone();
        assert_eq!(env["SWISS_TOKEN"], json!("new")); // imported values win
        assert_eq!(env["DB_PASSWORD"], json!("hunter2")); // untouched
        clear_state();
    }

    #[test]
    fn refuses_anything_that_is_not_a_state_bundle() {
        // It re-seals four files under this machine's key; the version gate is what stops a
        // future format from being written back as if it were this one.
        for bad in [json!("nope"), json!([]), json!({}), json!({ "version": 2 })] {
            assert!(import_state(&bad).is_err(), "{bad}");
        }
        // An empty bundle is well-formed and restores nothing.
        assert_eq!(import_state(&json!({ "version": 1 })), Ok(Vec::new()));
    }

    // --- waiting for health ------------------------------------------------------------------

    #[tokio::test]
    async fn waits_only_until_health_answers() {
        let gw = FakeGateway::start().await;
        let client = reqwest::Client::new();
        assert!(wait_for_health(&client, gw.port, 5_000, || false).await);
        gw.stop();
    }

    #[tokio::test]
    async fn gives_up_on_health_after_the_timeout_instead_of_polling_forever() {
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();
        assert!(!wait_for_health(&client, free_port(), 300, || false).await);
        assert!(started.elapsed() < Duration::from_secs(15), "it returned");
    }

    #[tokio::test]
    async fn abandons_the_health_wait_early_when_the_child_is_already_gone() {
        // A daemon that has already exited is never going to answer, and waiting out the full
        // timeout for it wastes the operator's time when what they need is the log.
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();
        assert!(!wait_for_health(&client, free_port(), 600_000, || true).await);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "it did not wait out the ten minutes"
        );
    }

    // --- starting ----------------------------------------------------------------------------

    #[tokio::test]
    async fn reports_an_already_running_daemon_without_spawning_a_second_one() {
        let _lock = daemon_state().await;
        let gw = FakeGateway::start().await;
        // A live pid plus a port that answers: the daemon is up.
        let pid = plant_own_pid(gw.port);
        let result = start_daemon(StartOptions {
            port: Some(gw.port),
            // Unspawnable, so any attempt to start would surface as a failure rather than pass.
            entry: Some(std::path::PathBuf::from("swiss-no-such-binary")),
            ..Default::default()
        })
        .await;
        match result {
            StartResult::AlreadyRunning { pid: p, port, url } => {
                assert_eq!(p, pid);
                assert_eq!(port, gw.port);
                assert_eq!(url, url_for(gw.port));
            }
            _ => panic!("a live daemon must be reported, not restarted"),
        }
        assert!(read_pid_file(gw.port).is_some()); // the record it is still using stays
        remove_pid_file(gw.port);
        gw.stop();
    }

    #[tokio::test]
    async fn reports_the_log_tail_when_the_entry_cannot_be_spawned() {
        let _lock = daemon_state().await;
        let port = free_port();
        let result = start_daemon(StartOptions {
            port: Some(port),
            entry: Some(std::path::PathBuf::from("swiss-no-such-binary")),
            timeout_ms: Some(200),
            ..Default::default()
        })
        .await;
        match result {
            StartResult::Failed { port: p, log_tail } => {
                assert_eq!(p, port);
                // The operator gets the reason, not just "it did not start".
                assert!(log_tail.contains("spawn failed"), "{log_tail}");
            }
            _ => panic!("a start that never spawned is a failure"),
        }
        // Never leave a pid file for something that never came up: the next start would report
        // already-running and the operator would have nothing to act on.
        assert_eq!(read_pid_file(port), None);
        let _ = std::fs::remove_file(log_file_path(port));
    }

    #[tokio::test]
    async fn starts_over_a_stale_pid_file_instead_of_believing_it() {
        // The leftover of an unclean kill: a pid nothing owns, on a port nothing answers.
        // Believing it would refuse every future start.
        let _lock = daemon_state().await;
        let port = free_port();
        plant_dead_pid(port);
        let result = start_daemon(StartOptions {
            port: Some(port),
            entry: Some(std::path::PathBuf::from("swiss-no-such-binary")),
            timeout_ms: Some(200),
            ..Default::default()
        })
        .await;
        assert!(matches!(result, StartResult::Failed { .. }));
        assert_eq!(read_pid_file(port), None);
        let _ = std::fs::remove_file(log_file_path(port));
    }

    // --- the daemon's environment (docs/16 §1) ------------------------------------------------

    /// A fake entry that dumps the environment it was handed to a file and exits — the probe
    /// that makes the scrub observable from outside the process. A .cmd on Windows (Rust spawns
    /// .bat/.cmd through cmd.exe with argument escaping since the BatBadBut hardening, so
    /// Command::new works on the script path); a shell script elsewhere.
    fn env_probe_entry(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let dump = dir.join("env-dump.txt");
        // The dump sits beside the script and cmd finds it through %~dp0, never through a path
        // spelled in the script: cmd reads a .cmd in the ANSI code page, so a non-ASCII home
        // (a CJK user name under C:\Users) written into it as UTF-8 named a directory that
        // does not exist.
        #[cfg(windows)]
        let (script, text) = (
            dir.join("env-probe.cmd"),
            "@set > \"%~dp0env-dump.txt\"\r\n".to_string(),
        );
        #[cfg(not(windows))]
        let (script, text) = (
            dir.join("env-probe.sh"),
            format!("#!/bin/sh\nenv > '{}'\n", dump.display()),
        );
        std::fs::write(&script, text).expect("write the probe script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("exec bit for the probe");
        }
        (script, dump)
    }

    #[tokio::test]
    async fn the_spawned_daemon_does_not_inherit_the_launchers_noise() {
        // The 2026-09-11 incident as a test: a launcher carrying NO_COLOR=1 started the
        // gateway, and every child below it went colourless. The daemon's environment must be
        // this machine's, not the launching shell's (docs/16 §1) — the exact names and the
        // prefix family both go, while the machine's own variables survive.
        let _lock = daemon_state().await;
        let (entry, dump) = env_probe_entry(&data_dir());
        // SAFETY: planted under the same data-dir lock every other env-planting test in this
        // module holds, and no test in this binary reads these names.
        unsafe {
            std::env::set_var("NO_COLOR", "1");
            std::env::set_var("CLAUDE_CODE_PROBE", "1");
        }
        let port = free_port();
        let result = start_daemon(StartOptions {
            port: Some(port),
            entry: Some(entry),
            timeout_ms: Some(200),
            ..Default::default()
        })
        .await;
        // The probe is not a gateway — health never answers, so the start fails. The dump it
        // left behind is the point.
        assert!(matches!(result, StartResult::Failed { .. }));
        // Lossy: cmd's `set` writes in the ANSI code page, and a non-ASCII value (the user's
        // own home path) is not UTF-8. The names asserted on below are ASCII either way.
        let text = String::from_utf8_lossy(&std::fs::read(&dump).expect("the probe dumped its environment"))
            .into_owned();
        assert!(
            !text
                .lines()
                .any(|l| l.to_ascii_uppercase().starts_with("NO_COLOR=")),
            "the launcher's NO_COLOR must not reach the daemon: {text}"
        );
        assert!(
            !text
                .lines()
                .any(|l| l.to_ascii_uppercase().starts_with("CLAUDE_CODE_PROBE=")),
            "prefix-family noise must not reach the daemon: {text}"
        );
        assert!(
            text.lines()
                .any(|l| l.to_ascii_uppercase().starts_with("PATH=")),
            "the machine's own variables DO reach the daemon: {text}"
        );
        // SAFETY: restore the machine for the rest of the test binary.
        unsafe {
            std::env::remove_var("NO_COLOR");
            std::env::remove_var("CLAUDE_CODE_PROBE");
        }
        remove_pid_file(port);
        let _ = std::fs::remove_file(log_file_path(port));
        let _ = std::fs::remove_file(&dump);
    }

    // --- stopping ----------------------------------------------------------------------------

    #[tokio::test]
    async fn says_so_when_nothing_is_running() {
        let _lock = daemon_state().await;
        assert!(matches!(
            stop_daemon(StopOptions {
                port: Some(free_port()),
                ..Default::default()
            })
            .await,
            StopResult::NotRunning { .. }
        ));
    }

    #[tokio::test]
    async fn clears_a_stale_pid_file_rather_than_killing_whatever_owns_that_pid_now() {
        let _lock = daemon_state().await;
        let port = free_port();
        plant_dead_pid(port);
        assert!(matches!(
            stop_daemon(StopOptions {
                port: Some(port),
                ..Default::default()
            })
            .await,
            StopResult::NotRunning { .. }
        ));
        assert_eq!(read_pid_file(port), None);
    }

    #[tokio::test]
    async fn refuses_to_kill_a_live_pid_it_cannot_confirm_is_this_gateway() {
        // The pid is alive but nothing serves its port, so it cannot be told apart from a number
        // the OS recycled into unrelated work. Killing on a guess is how a tool takes down
        // someone else's process; make the operator say --force.
        let _lock = daemon_state().await;
        let port = free_port();
        let pid = plant_own_pid(port);
        match stop_daemon(StopOptions {
            port: Some(port),
            ..Default::default()
        })
        .await
        {
            StopResult::Refused { pid: p, reason, .. } => {
                assert_eq!(p, pid);
                assert!(reason.contains("--force"), "{reason}");
            }
            _ => panic!("an unconfirmable live pid must not be killed"),
        }
        // The record stays: the operator has to decide, and needs it to still be there.
        assert!(read_pid_file(port).is_some());
        remove_pid_file(port);
    }

    #[tokio::test]
    async fn asks_the_gateway_to_shut_itself_down_and_clears_the_record() {
        // Windows has no deliverable SIGTERM, so this HTTP round trip is the only way the gateway
        // gets to close its adapters and tree-kill its own proc-MCP children.
        let _lock = daemon_state().await;
        let gw = FakeGateway::start().await;
        let port = gw.port;
        let pid = plant_dead_pid(port);
        match stop_daemon(StopOptions {
            port: Some(port),
            grace_period_ms: Some(10_000),
            ..Default::default()
        })
        .await
        {
            StopResult::Stopped { pid: p, port: q } => {
                assert_eq!(p, pid);
                assert_eq!(q, port);
            }
            _ => panic!("a gateway that honours /api/shutdown stops gracefully"),
        }
        assert_eq!(read_pid_file(port), None);
        gw.stop();
    }

    // --- status ------------------------------------------------------------------------------

    #[tokio::test]
    async fn reports_not_running_when_there_is_no_daemon() {
        let _lock = daemon_state().await;
        let port = free_port();
        let status = daemon_status(port).await;
        assert!(!status.running);
        assert_eq!(status.pid, None);
        assert_eq!(status.url, None);
        assert_eq!(status.health, None);
        // The log path is reported either way — it is what the operator reads next.
        assert!(status.log_file.ends_with(&format!("gateway-{port}.log")));
    }

    #[tokio::test]
    async fn a_pid_file_alone_is_not_running() {
        // "Running" means it answered. A pid file is a claim about the past, and after an unclean
        // kill it is a false one.
        let _lock = daemon_state().await;
        let port = free_port();
        let pid = plant_dead_pid(port);
        let status = daemon_status(port).await;
        assert!(!status.running);
        assert_eq!(status.pid, Some(pid)); // still reported, so `swiss stop` has something to act on
        assert_eq!(status.url, None);
        remove_pid_file(port);
    }

    /// A fake entry whose --version reports the given hash — the "binary on disk" half of the
    /// build comparison. A .cmd on Windows (Rust spawns .bat/.cmd through cmd.exe), a shell
    /// script elsewhere. NOTE: never point this comparison at the test binary itself in a
    /// pid record — a test runner asked for --version just runs the whole suite again.
    fn version_probe_entry(dir: &Path, hash: &str) -> std::path::PathBuf {
        let line = format!("swiss 0.1.0 ({hash}, 2026-01-01T00:00:00Z)");
        #[cfg(windows)]
        let (script, text) = (dir.join("version-probe.cmd"), format!("@echo {line}\r\n"));
        #[cfg(not(windows))]
        let (script, text) = (
            dir.join("version-probe.sh"),
            format!("#!/bin/sh\necho '{line}'\n"),
        );
        std::fs::write(&script, text).expect("write the version probe");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("exec bit for the probe");
        }
        script
    }

    #[tokio::test]
    async fn reports_the_running_daemon_with_its_mcps_and_its_memory() {
        let _lock = daemon_state().await;
        let gw = FakeGateway::start().await;
        let pid = plant_own_pid(gw.port);
        // The entry the record names must answer --version (daemon_status runs it for the
        // disk build), and the test binary itself would misbehave asked that — point it at a
        // probe reporting the SAME hash the fake gateway serves: the matching-build path.
        let mut rec = read_pid_file(gw.port).expect("planted");
        rec.entry = version_probe_entry(&data_dir(), "aaaaaaa")
            .to_string_lossy()
            .into_owned();
        rec.node = rec.entry.clone();
        write_pid_file(&rec);

        let status = daemon_status(gw.port).await;
        assert!(status.running);
        assert_eq!(status.pid, Some(pid));
        assert_eq!(status.url.as_deref(), Some(url_for(gw.port).as_str()));
        assert_eq!(
            status.entry.as_deref(),
            Some(rec.entry.as_str()),
            "the entry the record names is what stop/status act on"
        );
        let health = status.health.expect("a running daemon reports health");
        assert_eq!(health["ok"], json!(true));
        assert_eq!(health["health"], json!([{ "name": "echo" }]));
        assert_eq!(
            status.memory.as_ref().map(|m| m["gatewayMb"].clone()),
            Some(json!(12.5))
        );
        // startedAt is an ISO stamp, so the uptime is a duration rather than a guess.
        assert!(status.uptime_ms.is_some());
        // The build pair agrees (disk probe says what /health says): no note (docs/16 H3).
        assert_eq!(
            status.build.as_ref().map(|b| b["hash"].clone()),
            Some(json!("aaaaaaa"))
        );
        assert_eq!(status.disk_build.as_deref(), Some("aaaaaaa"));
        assert_eq!(status.build_note, None);
        remove_pid_file(gw.port);
        gw.stop();
    }

    #[tokio::test]
    async fn status_flags_a_daemon_running_another_build_than_the_entry_on_disk() {
        // The 2026-09-11 confusion as a test: "19999 runs the old binary" was believed
        // deployed because nothing surfaced WHICH build was running. The fake gateway serves
        // build aaaaaaa; the probe standing in for the on-disk entry reports bbbbbbb — status
        // must carry both and say what to do (docs/16 H3).
        let _lock = daemon_state().await;
        let gw = FakeGateway::start().await;
        let entry = version_probe_entry(&data_dir(), "bbbbbbb");
        write_pid_file(&PidRecord {
            pid: std::process::id(),
            port: gw.port,
            entry: entry.to_string_lossy().into_owned(),
            node: entry.to_string_lossy().into_owned(),
            started_at: swiss_core::log::iso_now(),
        });

        let status = daemon_status(gw.port).await;
        assert!(status.running);
        assert_eq!(
            status.build.as_ref().map(|b| b["hash"].clone()),
            Some(json!("aaaaaaa"))
        );
        assert_eq!(status.disk_build.as_deref(), Some("bbbbbbb"));
        let note = status
            .build_note
            .as_deref()
            .expect("a disagreement is a note");
        assert!(
            note.contains("bbbbbbb") && note.contains("aaaaaaa"),
            "{note}"
        );
        assert!(note.contains("restart"), "{note}");

        let text = crate::cli::render_status(&status);
        assert!(text.contains("build"), "{text}");
        assert!(text.contains("note:"), "{text}");
        let json_text =
            serde_json::to_string(&crate::cli::status_json(&status)).unwrap_or_default();
        assert!(
            json_text.contains("\"diskBuild\":\"bbbbbbb\""),
            "{json_text}"
        );
        remove_pid_file(gw.port);
        gw.stop();
    }

    #[test]
    fn the_version_hash_is_parsed_from_the_parenthesised_pair() {
        assert_eq!(
            parse_version_hash("swiss 0.1.0 (23047d8, 2026-09-11T13:16:40Z)"),
            Some("23047d8")
        );
        assert_eq!(
            parse_version_hash("swiss 0.1.0 (23047d8-dirty, 2026-09-11T13:16:40Z)"),
            Some("23047d8-dirty")
        );
        // Anything not the version_line shape is a None, never a guess.
        assert_eq!(parse_version_hash("swiss 0.1.0"), None);
        assert_eq!(parse_version_hash(""), None);
        assert_eq!(parse_version_hash("swiss 0.1.0 (, x)"), None);
    }
}
