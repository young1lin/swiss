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

//! `swiss update --apply`. Every step that can fail runs before the gateway is stopped.
//! The executable path is the one the caller computed; this function never calls
//! `current_exe` (ADR-041).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::fetch::{self, Source};
use super::manifest::{self, Choice, Plan, Via};
use super::patch::{self, sha256_file};
use super::replace::{self, RealFs, SwapIo};

pub struct ApplyEnv {
    pub exe: PathBuf,
    pub channel: String,
    pub version: String,
    pub hash: String,
    pub target: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub force: bool,
    pub by: String,
}

/// A refusal the panel can translate. `status` is 400 or 409.
pub struct Refusal {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

fn refuse(status: u16, code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal { status, code, message: message.into() }
}

/// The checks before a download. `running_exe` is the pid file's entry when that
/// pid is alive. The panel route calls this so a refusal is an HTTP error;
/// `apply` repeats the same checks inside the short-lived process.
pub fn preflight(env: &ApplyEnv, running_exe: Option<&Path>) -> Result<(), Refusal> {
    if env.channel == "dev" && !env.force {
        return Err(refuse(400, "update.devBuild", "this build was made from source (channel dev); pass --force to replace it"));
    }
    if replace::in_app_bundle(&env.exe) {
        return Err(refuse(400, "update.bundle", "a macOS app bundle is updated by replacing the .app"));
    }
    let Some(dir) = env.exe.parent() else {
        return Err(refuse(400, "update.readOnly", "exe has no directory"));
    };
    if let Err(err) = replace::dir_writable(dir) {
        return Err(refuse(400, "update.readOnly", err));
    }
    if let Some(pid) = replace::held_lock_pid(&env.data_dir) {
        return Err(refuse(409, "update.running", format!("an update is already running (pid {pid})")));
    }
    if let Some(entry) = running_exe {
        if !same_exe(entry, &env.exe) {
            return Err(refuse(409, "update.otherExe", format!("gateway: runs another exe ({}), left alone", entry.display())));
        }
    }
    Ok(())
}

/// Why the panel must not offer Apply. `None` means the click is allowed.
/// Channel `dev` wins, so a test binary reports `update.devBuild` without
/// looking at another gateway's pid file.
pub fn refusal_for(exe: &Path, channel: &str, running_exe: Option<&Path>) -> Option<&'static str> {
    if channel == "dev" {
        return Some("update.devBuild");
    }
    if replace::in_app_bundle(exe) {
        return Some("update.bundle");
    }
    let Some(dir) = exe.parent() else {
        return Some("update.readOnly");
    };
    if replace::dir_writable(dir).is_err() {
        return Some("update.readOnly");
    }
    if let Some(entry) = running_exe {
        if !same_exe(entry, exe) {
            return Some("update.otherExe");
        }
    }
    None
}

/// `GET /api/update/status`. `child_running` is an updater this process spawned
/// and has not yet seen exit. A missing or unreadable `last.json` is null.
pub fn status_body(home: &Path, channel: &str, refusal: Option<&str>, child_running: bool) -> Value {
    let last = std::fs::read_to_string(home.join("update").join("last.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    json!({
        "channel": channel,
        "canApply": refusal.is_none(),
        "refusal": refusal,
        "running": child_running || replace::held_lock_pid(home).is_some(),
        "last": last,
    })
}

pub struct Outcome {
    pub code: i32,
    pub lines: Vec<String>,
    pub rolled_back: bool,
}

#[async_trait]
pub trait Fetch: Send + Sync {
    async fn manifest(&self) -> Result<Vec<u8>, String>;
    async fn asset(&self, tag: &str, name: &str, sha: &str, dest: &Path) -> Result<u64, String>;
}

#[async_trait]
pub trait Gateway: Send + Sync {
    fn record_entry(&self, port: u16) -> Option<String>;
    async fn stop(&self, port: u16) -> Result<(), String>;
    async fn start(&self, exe: &Path, port: u16) -> Result<(), String>;
    async fn health_hash(&self, port: u16) -> Result<String, String>;
    fn probe(&self, exe: &Path) -> Result<String, String>;
    fn refresh_skills(&self, exe: &Path) -> Result<String, String>;
}

pub struct HttpFetch {
    source: Source,
}

impl HttpFetch {
    pub fn real() -> Result<Self, String> {
        Ok(Self { source: fetch::source()? })
    }
}

#[async_trait]
impl Fetch for HttpFetch {
    async fn manifest(&self) -> Result<Vec<u8>, String> {
        let client = fetch::http_client(&self.source)?;
        fetch::get_bytes(&client, &fetch::manifest_url(&self.source), fetch::MANIFEST_LIMIT).await
    }

    async fn asset(&self, tag: &str, name: &str, sha: &str, dest: &Path) -> Result<u64, String> {
        let client = fetch::http_client(&self.source)?;
        let url = fetch::asset_url(&self.source, tag, name);
        fetch::get_to_file(&client, &url, dest, fetch::ASSET_LIMIT, sha).await
    }
}

pub async fn apply(env: &ApplyEnv, fetch: &dyn Fetch, gate: &dyn Gateway) -> Outcome {
    let mut lines = Vec::new();
    let mut phase = "refused";
    let mut via = String::new();
    let mut bytes = 0u64;
    let mut to = String::new();
    let mut rolled_back = false;
    let started = now_rfc3339();
    let from = format!("{} ({})", env.version, env.hash);

    let fail = |code: i32, phase: &str, error: String, lines: Vec<String>, rolled_back: bool, via: &str, bytes: u64, to: &str| {
        write_last(env, &from, to, via, bytes, false, phase, Some(error), rolled_back, &started);
        Outcome { code, lines, rolled_back }
    };

    if env.channel == "dev" && !env.force {
        lines.push("this build was made from source (channel dev); --apply refuses it".to_string());
        return fail(2, phase, lines[0].clone(), lines, false, &via, bytes, &to);
    }
    if replace::in_app_bundle(&env.exe) {
        lines.push("this exe is inside a macOS app bundle; update it by replacing the .app".to_string());
        lines.push(manifest::MANUAL.to_string());
        return fail(2, phase, "app bundle".to_string(), lines, false, &via, bytes, &to);
    }
    let Some(dir) = env.exe.parent() else {
        return fail(2, phase, "exe has no directory".to_string(), lines, false, &via, bytes, &to);
    };
    if let Err(err) = replace::dir_writable(dir) {
        lines.push(err.clone());
        return fail(2, phase, err, lines, false, &via, bytes, &to);
    }
    let lock = match take_lock(&env.data_dir.join("update")) {
        Ok(lock) => lock,
        Err(err) => {
            lines.push(err.clone());
            return fail(2, phase, err, lines, false, &via, bytes, &to);
        }
    };
    if let Some(entry) = gate.record_entry(env.port) {
        if !same_exe(Path::new(&entry), &env.exe) {
            let message = format!("gateway: runs another exe ({entry}), left alone");
            lines.push(message.clone());
            lines.push(format!("this exe is {}", env.exe.display()));
            drop(lock);
            return fail(2, phase, message, lines, false, &via, bytes, &to);
        }
    }

    phase = "manifest";
    let body = match fetch.manifest().await {
        Ok(body) => body,
        Err(err) => {
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, false, &via, bytes, &to);
        }
    };
    let choice = match manifest::select(&body, &env.target, &sha_of(&env.exe), &env.version, env.force) {
        Ok(choice) => choice,
        Err(err) => {
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, false, &via, bytes, &to);
        }
    };
    let plan = match choice {
        Choice::Install(plan) => plan,
        Choice::AlreadyNewest => {
            lines.push(format!("current: {} ({})", env.version, env.hash));
            lines.push("already the newest".to_string());
            drop(lock);
            write_last(env, &from, &from, "", 0, true, "done", None, false, &started);
            return Outcome { code: 0, lines, rolled_back: false };
        }
        Choice::Older => {
            lines.push("refusing to downgrade".to_string());
            drop(lock);
            return fail(2, phase, "downgrade".to_string(), lines, false, &via, bytes, &to);
        }
        Choice::UnknownTarget | Choice::SchemaTooNew => {
            lines.push(manifest::MANUAL.to_string());
            drop(lock);
            return fail(1, phase, "manual update".to_string(), lines, false, &via, bytes, &to);
        }
    };

    lines.push(format!("current: {} ({})", env.version, env.hash));
    lines.push(format!("latest: {}", plan.tag));
    lines.push(plan.apply_line());
    via = plan.via_name().to_string();
    to = plan.version.clone();

    phase = "download";
    let download = env.data_dir.join("update").join(format!("{}.part", plan.asset()));
    bytes = match fetch.asset(&plan.tag, plan.asset(), plan.asset_sha(), &download).await {
        Ok(n) => n,
        Err(err) => {
            let _ = std::fs::remove_file(&download);
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, false, &via, 0, &to);
        }
    };

    phase = "verify";
    let new_path = replace::sibling(&env.exe, ".new");
    let built = build_new(&env.exe, &download, &plan, &new_path);
    let _ = std::fs::remove_file(&download);
    if let Err(err) = built {
        lines.push(err.clone());
        drop(lock);
        return fail(1, phase, err, lines, false, &via, bytes, &to);
    }

    phase = "probe";
    let probe = match gate.probe(&new_path) {
        Ok(line) => line,
        Err(err) => {
            let _ = std::fs::remove_file(&new_path);
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, false, &via, bytes, &to);
        }
    };
    let probe_line = probe.lines().find(|line| line.starts_with("swiss ")).unwrap_or(probe.trim()).to_string();
    let probed = crate::daemon::parse_version_number(&probe_line).unwrap_or("");
    if probed != plan.version {
        let err = format!("probe says {probe_line}, manifest says {}", plan.version);
        let _ = std::fs::remove_file(&new_path);
        lines.push(err.clone());
        drop(lock);
        return fail(1, phase, err, lines, false, &via, bytes, &to);
    }
    let expected_hash = crate::daemon::parse_version_hash(&probe_line).unwrap_or("").to_string();
    lines.push(format!("verified: sha256 ok, probe says {probe_line}"));

    let was_running = gate.record_entry(env.port).is_some();
    phase = "stop";
    if was_running {
        if let Err(err) = gate.stop(env.port).await {
            let _ = std::fs::remove_file(&new_path);
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, false, &via, bytes, &to);
        }
    }

    phase = "swap";
    let mut fs = RealFs;
    if let Err(err) = replace::swap(&env.exe, &mut fs) {
        let _ = std::fs::remove_file(&new_path);
        if was_running {
            let _ = gate.start(&env.exe, env.port).await;
        }
        lines.push(err.clone());
        drop(lock);
        return fail(1, phase, err, lines, false, &via, bytes, &to);
    }
    let old_name = replace::sibling(&env.exe, ".old");
    lines.push(format!(
        "replaced: {} (previous kept as {})",
        env.exe.display(),
        old_name.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "swiss.exe.old".to_string())
    ));

    if !was_running {
        lines.push("gateway: not running, left stopped".to_string());
        drop(lock);
        write_last(env, &from, &format!("{} ({expected_hash})", plan.version), &via, bytes, true, "done", None, false, &started);
        return Outcome { code: 0, lines, rolled_back: false };
    }

    phase = "start";
    if let Err(err) = gate.start(&env.exe, env.port).await {
        // A start that timed out can leave its child alive, holding the new exe open, and
        // the swap back cannot rename an exe that is still running on Windows.
        let _ = gate.stop(env.port).await;
        rolled_back = undo(env, gate, &mut fs, &mut lines).await;
        lines.push(err.clone());
        drop(lock);
        return fail(1, phase, err, lines, rolled_back, &via, bytes, &to);
    }
    phase = "health";
    match gate.health_hash(env.port).await {
        Ok(hash) if hash == expected_hash => {}
        other => {
            let err = match other {
                Ok(hash) => format!("health hash {hash} is not {expected_hash}"),
                Err(err) => err,
            };
            let _ = gate.stop(env.port).await;
            rolled_back = undo(env, gate, &mut fs, &mut lines).await;
            lines.push(err.clone());
            drop(lock);
            return fail(1, phase, err, lines, rolled_back, &via, bytes, &to);
        }
    }
    lines.push(format!("gateway: restarted on {}, serving {} ({expected_hash})", env.port, plan.version));

    match gate.refresh_skills(&env.exe) {
        Ok(line) => lines.push(line),
        Err(warning) => lines.push(warning),
    }
    drop(lock);
    write_last(
        env,
        &from,
        &format!("{} ({expected_hash})", plan.version),
        &via,
        bytes,
        true,
        "done",
        None,
        false,
        &started,
    );
    Outcome { code: 0, lines, rolled_back }
}

async fn undo(env: &ApplyEnv, gate: &dyn Gateway, fs: &mut dyn SwapIo, lines: &mut Vec<String>) -> bool {
    if let Err(err) = replace::rollback(&env.exe, fs) {
        lines.push(format!("rollback failed: {err}"));
        return false;
    }
    if let Err(err) = gate.start(&env.exe, env.port).await {
        lines.push(format!("could not start the previous gateway: {err}"));
    }
    true
}

/// Decode the download into `<exe>.new` and verify it against the target's hash. A patch
/// reads the old exe once, as its reference, and drops it before the gateway is stopped.
fn build_new(exe: &Path, download: &Path, plan: &Plan, new_path: &Path) -> Result<(), String> {
    match &plan.via {
        Via::Full { .. } => patch::decode_to_file(download, patch::Packed::Full, new_path, &plan.exe_sha256),
        Via::Patch { window_log, .. } => {
            let old = std::fs::read(exe).map_err(|err| format!("cannot read {}: {err}", exe.display()))?;
            let how = patch::Packed::Patch { old: &old, window_log: *window_log };
            patch::decode_to_file(download, how, new_path, &plan.exe_sha256)
        }
    }
}

fn sha_of(exe: &Path) -> String {
    sha256_file(exe).unwrap_or_default()
}

fn same_exe(record: &Path, exe: &Path) -> bool {
    let left = std::fs::canonicalize(record).unwrap_or_else(|_| record.to_path_buf());
    let right = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    #[cfg(windows)]
    {
        left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn take_lock(dir: &Path) -> Result<Lock, String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let path = dir.join("update.lock");
    for _ in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let _ = write!(file, "{}", std::process::id());
                return Ok(Lock(path));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                // A pid that cannot be read is stale, same as a dead one.
                let held = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                    .is_some_and(|pid| pid != std::process::id() && swiss_core::platform::pid_alive(pid));
                if held {
                    return Err("another update holds update.lock".to_string());
                }
                let _ = std::fs::remove_file(&path);
            }
            Err(err) => return Err(format!("cannot take update.lock: {err}")),
        }
    }
    Err("cannot take update.lock".to_string())
}

fn write_last(env: &ApplyEnv, from: &str, to: &str, via: &str, bytes: u64, ok: bool, phase: &str, error: Option<String>, rolled_back: bool, started: &str) {
    let path = env.data_dir.join("update").join("last.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = json!({
        "from": from,
        "to": to,
        "via": if via.is_empty() { Value::Null } else { Value::String(via.to_string()) },
        "bytes": bytes,
        "ok": ok,
        "phase": phase,
        "error": error,
        "rolledBack": rolled_back,
        "startedAt": started,
        "finishedAt": now_rfc3339(),
        "by": env.by,
    });
    let _ = std::fs::write(path, serde_json::to_vec_pretty(&body).unwrap_or_default());
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

pub struct RealGateway;

#[async_trait]
impl Gateway for RealGateway {
    /// A pid file whose process is gone is a leftover from an unclean exit: it names no
    /// running gateway, so it neither refuses another exe nor makes a stopped gateway start.
    fn record_entry(&self, port: u16) -> Option<String> {
        crate::pidfile::read_pid_file(port)
            .filter(|rec| swiss_core::platform::pid_alive(rec.pid))
            .map(|rec| rec.entry)
    }

    async fn stop(&self, port: u16) -> Result<(), String> {
        match crate::daemon::stop_daemon(crate::daemon::StopOptions { port: Some(port), ..Default::default() }).await {
            crate::daemon::StopResult::Stopped { .. } | crate::daemon::StopResult::Forced { .. } | crate::daemon::StopResult::NotRunning { .. } => Ok(()),
            crate::daemon::StopResult::Refused { reason, .. } => Err(reason),
        }
    }

    async fn start(&self, exe: &Path, port: u16) -> Result<(), String> {
        match crate::daemon::start_daemon(crate::daemon::StartOptions {
            port: Some(port),
            entry: Some(exe.to_path_buf()),
            port_env_only: true,
            timeout_ms: Some(30_000),
            ..Default::default()
        })
        .await
        {
            crate::daemon::StartResult::Started { .. } | crate::daemon::StartResult::AlreadyRunning { .. } => Ok(()),
            crate::daemon::StartResult::Failed { log_tail, .. } => Err(format!("gateway did not start: {log_tail}")),
        }
    }

    async fn health_hash(&self, port: u16) -> Result<String, String> {
        health_hash(port, Duration::from_secs(30)).await
    }

    fn probe(&self, exe: &Path) -> Result<String, String> {
        probe_version(exe)
    }

    fn refresh_skills(&self, exe: &Path) -> Result<String, String> {
        refresh_skills(exe)
    }
}

pub async fn health_hash(port: u16, budget: Duration) -> Result<String, String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().map_err(|err| err.to_string())?;
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + budget;
    loop {
        if Instant::now() > deadline {
            return Err("health timed out".to_string());
        }
        if let Ok(response) = client.get(&url).send().await {
            if let Ok(body) = response.json::<Value>().await {
                if let Some(hash) = body.get("build").and_then(|b| b.get("hash")).and_then(Value::as_str) {
                    return Ok(hash.to_string());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn probe_version(exe: &Path) -> Result<String, String> {
    let mut child = std::process::Command::new(exe)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("cannot probe {}: {err}", exe.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(stdout) = child.stdout.take() {
                    use std::io::Read;
                    let _ = std::io::BufReader::new(stdout).read_to_string(&mut out);
                }
                if !status.success() {
                    return Err(format!("probe exited {status}"));
                }
                return Ok(out);
            }
            Ok(None) if start.elapsed() > Duration::from_secs(10) => {
                let _ = child.kill();
                return Err("probe timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => return Err(format!("probe: {err}")),
        }
    }
}

fn refresh_skills(exe: &Path) -> Result<String, String> {
    let out = std::process::Command::new(exe)
        .args(["skill", "install", "--installed"])
        .output()
        .map_err(|err| format!("warning: skill refresh could not start: {err}"))?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(1);
        return Err(format!("warning: skill refresh exited {code} - the update itself succeeded"));
    }
    Ok(skills_line(&String::from_utf8_lossy(&out.stdout)))
}

fn skills_line(stdout: &str) -> String {
    if stdout.contains("nothing to refresh") {
        return "skills: 0 copies refreshed in 0 seats".to_string();
    }
    let files: Vec<&str> = stdout.lines().filter(|line| line.starts_with("  ")).collect();
    let mut seats = std::collections::BTreeSet::new();
    for file in &files {
        let path = Path::new(file.trim());
        if let Some(seat) = path.parent().and_then(|skill| skill.parent()) {
            seats.insert(seat.to_string_lossy().into_owned());
        }
    }
    format!("skills: {} copies refreshed in {} seats", files.len(), seats.len())
}
