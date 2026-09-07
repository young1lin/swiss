//! Start, stop and inspect the gateway as a background process — port of `daemon.ts`.
//!
//! The gateway is a long-lived local server, so the CLI's job is to detach one and then be able
//! to find it again from any directory later. Nothing here starts the server's module graph in
//! THIS process — a `status` call must stay cheap — so it talks to a running instance the same
//! way any other client does: over loopback HTTP.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::paths::{data_dir, data_path};
use crate::pidfile::{
    is_pid_alive, log_file_path, read_pid_file, remove_pid_file, write_pid_file, PidRecord,
};
use crate::platform::tree_kill;
use crate::port::{as_listen_port_value, env_listen_port, DEFAULT_PORT};
use crate::secure::envstore::{read_env_store, write_env_store};
use crate::secure::statefile::{read_secure_json, write_secure_json};

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
}

pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// The port to act on when none was given. Env (`MCP_GATEWAY_PORT`) wins, then the config file,
/// then 19999. Read straight out of the file rather than through load_config(), because every
/// command here must work even when the config is invalid — `lmg stop` on a gateway whose config
/// you just broke is exactly when you need it most.
pub fn resolve_port() -> u16 {
    env_listen_port()
        .or_else(|| read_config_raw().and_then(|c| c.get("port").and_then(as_listen_port_value)))
        .unwrap_or(DEFAULT_PORT)
}

/// Write `port` into gateway.config.json so the next `lmg start` (and the child about to spawn)
/// listens there. No-op when there is no config yet — first-run seed reads `MCP_GATEWAY_PORT`.
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
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("lmg"))
}

/// True for a path inside npm's npx cache, which is version-keyed and cleared on update — a
/// daemon started from there stops being restartable the moment the cache turns over.
pub fn is_npx_cache_path(path: &Path) -> bool {
    path.to_string_lossy().contains("\\_npx\\") || path.to_string_lossy().contains("/_npx/")
}

/// The env store path handed to read_env_store/write_env_store (the sealed .env replacement).
fn env_store() -> std::collections::HashMap<String, String> {
    read_env_store(&crate::secure::envstore::env_store_path())
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
    env_store().get(key).cloned()
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
        .unwrap_or("MCP_GATEWAY_TOKEN")
        .to_string();
    read_env_key(&name)
}

/// Panel URL + bearer token, for `lmg creds`. Never dumps the rest of the env store (DB
/// passwords live there). The panel itself has no login — the loopback guard is its boundary —
/// so there is no username or password to print.
pub fn read_creds() -> (String, Option<String>) {
    (url_for(resolve_port()), read_gateway_token())
}

/// One decrypted bundle of every state file — what 'lmg export' writes and 'lmg import' reads.
/// The ONLY plaintext export path: machine binding cuts both ways, so moving to a new machine
/// (or recovering from a lost OS credential) needs an operator-initiated export on a machine
/// that can still read the files.
pub fn export_state() -> Value {
    json!({
        "version": 1,
        "exportedAt": crate::log::iso_now(),
        "config": read_secure_json(&data_path(&["gateway.config.json"])).ok().flatten(),
        "managed": read_secure_json(&data_path(&["managed.json"])).ok().flatten(),
        "tunnels": read_secure_json(&data_path(&["tunnels.json"])).ok().flatten(),
        "env": Value::Object(env_store()
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect()),
    })
}

/// Restore a bundle: every section is re-sealed under THIS machine's key as it lands. Returns
/// the restored file names.
pub fn import_state(bundle: &Value) -> Result<Vec<String>, String> {
    let Some(obj) = bundle.as_object() else {
        return Err("not a state bundle (expected JSON written by 'lmg export')".into());
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
    if let Some(env) = obj.get("env").filter(|v| v.is_object()) {
        // Imported values win over whatever is already stored.
        let mut merged = env_store();
        for (k, v) in env.as_object().expect("checked object") {
            if let Some(s) = v.as_str() {
                merged.insert(k.clone(), s.to_string());
            }
        }
        write_env_store(&merged, &crate::secure::envstore::env_store_path())?;
        restored.push("env.json".into());
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
/// handles (and gives `lmg logs` something to read). No job object is attached, so the child
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
    if let Some(port) = opts.port {
        // So the child listens where we asked, including a first run that has no config to
        // persist into.
        command.env("MCP_GATEWAY_PORT", port.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW — the detach plus the hidden console.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
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
        started_at: crate::log::iso_now(),
    });

    if wait_for_health(&client, port, timeout_ms, || !is_pid_alive(pid)).await {
        // Two racing `lmg start`s both reach here: the loser's child died on EADDRINUSE while
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

/// Ask the gateway to shut itself down. Failure is fine — the caller falls back to a tree-kill.
async fn post_shutdown(client: &reqwest::Client, port: u16) {
    let token = read_gateway_token();
    let mut req = client
        .post(format!("http://127.0.0.1:{port}/api/shutdown"))
        .timeout(Duration::from_secs(5));
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
    let mut req = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .timeout(Duration::from_secs(5));
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let res = req.send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json::<Value>().await.ok()
}

/// What `lmg status` prints. "Running" means it answered — not that a pid file exists, which is
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
    };
    if let Some(rec) = &rec {
        if let Some(started) = crate::util::parse_iso_ms(&rec.started_at) {
            let now = crate::util::now_ms() as i64;
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
    result
}
