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

//! The /api/process routes (SPEC §process.apps and §process.children),
//! mounted the way the terminal plugin mounts its tree: the boot builds a
//! [ProcessState] slot, hands it BOTH to the plugin factory (which installs on
//! start, withdraws on stop) and to [mount] (merged into the extra tree under
//! the loopback guard and the admin session). While the plugin is off every
//! route answers a structured 503 naming it - a disabled plugin is a missing
//! plugin, not a half-open one.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::json;

use swiss_host::reply::{admin_error, admin_json};
use swiss_host::services::children::{ChildRegistry, StopLookup};

use crate::control::{AppControl, ControlError};
use crate::logs;

/// What the routes need, installed by the plugin's start: the engines and the
/// children registry (SPEC §process.children: the children routes belong to
/// this prefix, and the registry comes from the host's RuntimeServices).
pub struct ProcessSystem {
    pub control: Arc<AppControl>,
    pub children: Arc<ChildRegistry>,
}

/// The state slot: constructed once by the boot, filled by the plugin's start,
/// emptied by its stop. Routes read it per request - no request can reach a
/// plugin that is stopping.
pub struct ProcessState {
    slot: RwLock<Option<Arc<ProcessSystem>>>,
}

impl ProcessState {
    pub fn new() -> Arc<Self> {
        Arc::new(ProcessState {
            slot: RwLock::new(None),
        })
    }

    pub fn install(&self, system: Arc<ProcessSystem>) {
        *self.slot.write().unwrap_or_else(|e| e.into_inner()) = Some(system);
    }

    pub fn withdraw(&self) -> Option<Arc<ProcessSystem>> {
        self.slot.write().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn current(&self) -> Option<Arc<ProcessSystem>> {
        self.slot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Mount the /api/process tree. The caller merges this into the extra tree
/// BEFORE build_app folds it under the loopback guard and the session gate.
pub fn mount(state: Arc<ProcessState>) -> Router<()> {
    Router::new()
        .route("/api/process", get(rows))
        .route("/api/process/apps/{name}/start", post(start_app))
        .route("/api/process/apps/{name}/stop", post(stop_app))
        .route("/api/process/apps/{name}/restart", post(restart_app))
        .route("/api/process/apps/{name}/log", get(app_log))
        .route("/api/process/history", get(history))
        .route("/api/process/children", get(children))
        .route("/api/process/children/{pid}/stop", post(stop_child))
        // The Program picker's read-only listing (SPEC §process.panel): names
        // and shapes only - never contents. The admin session and the loopback
        // guard sit in front of it like every /api route, and the Process page
        // already lets that same admin run any program at all.
        .route("/api/process/fs", get(fs_list))
        .with_state(state)
}

/// The defensive branch: the plugin boundary should have answered this, but a
/// mounted router never assumes its mounter (SPEC §host.plugins).
fn not_running() -> Response {
    admin_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "the process plugin is not running",
    )
}

fn control_error(err: ControlError) -> Response {
    match err {
        ControlError::UnknownApp => {
            admin_error(StatusCode::NOT_FOUND, "no such app in the process config")
        }
        // Busy and refused are both conflicts: one names the action in flight,
        // the other names what is on the ports - never a value.
        ControlError::Busy(action) => admin_error(
            StatusCode::CONFLICT,
            &format!("busy: {action}"),
        ),
        ControlError::Refused(sentence) => admin_error(StatusCode::CONFLICT, &sentence),
    }
}

async fn rows(State(state): State<Arc<ProcessState>>) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    let apps = system.control.rows().await;
    // The gateway's own facts ride with the table (SPEC §process.apps): the
    // pid the kill refusal names, and the two numbers the panel's foot shows.
    let pid = std::process::id();
    let uptime_sec = swiss_core::platform::process_created_at(pid)
        .map(|created| system.control.gateway_uptime_sec(created))
        .unwrap_or(0);
    let mem_mb = swiss_core::platform::self_working_set() / (1024 * 1024);
    match serde_json::to_value(json!({
        "apps": apps,
        "gateway": { "pid": pid, "uptimeSec": uptime_sec, "memMb": mem_mb }
    })) {
        Ok(value) => admin_json(StatusCode::OK, value),
        Err(err) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

/// The action bodies (SPEC §process.apps): `by` is ATTRIBUTION, not
/// authorization - the CLI sends {"by": "cli"}, the panel sends nothing
/// (panel), and boot/plugin are the gateway's own words, never accepted from
/// a request.
#[derive(Deserialize, Default, Clone)]
struct ActionBody {
    by: Option<String>,
    force: Option<bool>,
}

impl ActionBody {
    /// The by the history will carry: "panel" unless the request said "cli".
    /// Anything else is a 400 - a word the row never writes. The sentence
    /// comes back, not a response, so the closure stays small (clippy's
    /// large-err rule is right about Response).
    fn by(&self) -> Result<&str, String> {
        match self.by.as_deref() {
            None => Ok("panel"),
            Some("cli") => Ok("cli"),
            Some("panel") => Ok("panel"),
            Some(other) => Err(format!("by must be \"panel\" or \"cli\", not {other:?}")),
        }
    }
}

async fn start_app(
    State(state): State<Arc<ProcessState>>,
    Path(name): Path<String>,
    body: Option<axum::Json<ActionBody>>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    let by = match body.as_ref().map(|axum::Json(b)| b.by()) {
        None => Ok("panel"),
        Some(result) => result,
    };
    let by = match by {
        Ok(by) => by,
        Err(sentence) => return admin_error(StatusCode::BAD_REQUEST, &sentence),
    };
    match system.control.start(&name, by).await {
        Ok(()) => admin_json(StatusCode::ACCEPTED, json!({ "accepted": true })),
        Err(err) => control_error(err),
    }
}

async fn stop_app(
    State(state): State<Arc<ProcessState>>,
    Path(name): Path<String>,
    body: Option<axum::Json<ActionBody>>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    // `force` skips the stop command and kills (the row's canForce is the
    // offer, the panel's confirm is the contract).
    let force = body
        .as_ref()
        .map(|axum::Json(body)| body.force.unwrap_or(false))
        .unwrap_or(false);
    let by = match body.as_ref().map(|axum::Json(b)| b.by()) {
        None => Ok("panel"),
        Some(result) => result,
    };
    let by = match by {
        Ok(by) => by,
        Err(sentence) => return admin_error(StatusCode::BAD_REQUEST, &sentence),
    };
    match system.control.stop(&name, by, force).await {
        Ok(()) => admin_json(StatusCode::ACCEPTED, json!({ "accepted": true })),
        Err(err) => control_error(err),
    }
}

async fn restart_app(
    State(state): State<Arc<ProcessState>>,
    Path(name): Path<String>,
    body: Option<axum::Json<ActionBody>>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    // The restart's stop half honors `force` too (SPEC §process.apps): the
    // CLI sends it for `swiss process restart --force`, the panel does not.
    let force = body
        .as_ref()
        .map(|axum::Json(body)| body.force.unwrap_or(false))
        .unwrap_or(false);
    let by = match body.as_ref().map(|axum::Json(b)| b.by()) {
        None => Ok("panel"),
        Some(result) => result,
    };
    let by = match by {
        Ok(by) => by,
        Err(sentence) => return admin_error(StatusCode::BAD_REQUEST, &sentence),
    };
    match system.control.restart(&name, by, force).await {
        Ok(()) => admin_json(StatusCode::ACCEPTED, json!({ "accepted": true })),
        Err(err) => control_error(err),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    /// capture (default) or app.
    source: Option<String>,
    tail: Option<usize>,
    after: Option<u64>,
}

async fn app_log(
    State(state): State<Arc<ProcessState>>,
    Path(name): Path<String>,
    Query(query): Query<LogQuery>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    // The app must be declared; the file is NEVER taken from the request.
    let Some(app) = system.control.app(&name) else {
        return admin_error(StatusCode::NOT_FOUND, "no such app in the process config");
    };
    let path = match query.source.as_deref() {
        None | Some("capture") => Some(system.control.capture_file(&name)),
        Some("app") => app.log.as_deref().and_then(logs::resolve_app_log),
        Some(other) => {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown log source {other:?}; capture or app"),
            )
        }
    };
    let Some(path) = path else {
        return admin_error(StatusCode::NOT_FOUND, "this app declares no log file");
    };
    let values = logs::maskable_values(&app.env, &app.start.program, &app.start.args);
    let tail = match logs::read_tail(&path, query.after, &values) {
        Ok(Some(tail)) => tail,
        Ok(None) => {
            return admin_error(StatusCode::NOT_FOUND, "the log file does not exist yet")
        }
        Err(err) => return admin_error(StatusCode::BAD_GATEWAY, &err.to_string()),
    };
    let mut lines = tail.lines;
    if let Some(limit) = query.tail {
        if lines.len() > limit {
            lines.drain(..lines.len() - limit);
        }
    }
    // The answer names what was read (SPEC §process.apps): the source word,
    // the server's own path for it (never one taken from the request), the
    // byte offset the next `after` follows from.
    let source = query.source.clone().unwrap_or_else(|| "capture".to_string());
    admin_json(
        StatusCode::OK,
        json!({
            "source": source,
            "path": path.to_string_lossy(),
            "size": tail.size,
            "lines": lines,
            "truncated": tail.truncated,
            "reset": logs::was_reset(tail.size, query.after),
        }),
    )
}

#[derive(Deserialize)]
struct HistoryQuery {
    app: Option<String>,
    limit: Option<usize>,
}

async fn history(
    State(state): State<Arc<ProcessState>>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    let entries: Vec<serde_json::Value> = system
        .control
        .history()
        .read(query.app.as_deref(), query.limit.unwrap_or(100).min(1000))
        .await
        .iter()
        .map(|entry| entry.to_json())
        .collect();
    admin_json(StatusCode::OK, json!({ "entries": entries }))
}

async fn children(State(state): State<Arc<ProcessState>>) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    let rows: Vec<serde_json::Value> = system
        .children
        .rows()
        .iter()
        .map(|row| {
            // The memory column ChildRow deliberately leaves to the reader:
            // the child's own working set, one platform call per row - absent
            // when unreadable, so a row without a reading shows none, never 0
            // (SPEC §process.children).
            let mut value = json!({
                "pid": row.pid,
                "owner": row.owner,
                "kind": row.kind.as_str(),
                "label": row.label,
                "uptimeSec": row.uptime_sec,
                "stoppable": row.stoppable,
            });
            if let Some(bytes) = swiss_core::platform::process_working_set(row.pid) {
                value["memMb"] = json!(((bytes as f64) / 1_048_576.0 * 10.0).round() / 10.0);
            }
            value
        })
        .collect();
    admin_json(StatusCode::OK, json!({ "children": rows }))
}

async fn stop_child(
    State(state): State<Arc<ProcessState>>,
    Path(pid): Path<u32>,
) -> Response {
    let Some(system) = state.current() else {
        return not_running();
    };
    // The registry decides the whole shape (SPEC §process.children): 404 for
    // an unknown pid, 409 for a row nobody can stop, the owner's own hook for
    // the rest - awaited here, bounded, because the hook is the owner's
    // teardown, not ours to improvise.
    match system.children.stop(pid) {
        StopLookup::Unknown => {
            admin_error(StatusCode::NOT_FOUND, "no such child of this gateway")
        }
        StopLookup::NoHook => admin_error(
            StatusCode::CONFLICT,
            "this child has no stop path; it is not stoppable",
        ),
        StopLookup::Hook(hook) => match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            hook.stop(),
        )
        .await
        {
            // The panel toasts this body: {ok, message}, the spec's shape,
            // so a successful stop never reads as an error.
            Ok(Ok(())) => admin_json(StatusCode::OK, json!({ "ok": true, "message": "stopped" })),
            Ok(Err(sentence)) => admin_error(StatusCode::BAD_GATEWAY, &sentence),
            Err(_) => admin_error(
                StatusCode::GATEWAY_TIMEOUT,
                "the stop did not finish within 15s",
            ),
        },
    }
}

// --- the Program picker's listing (SPEC §process.panel) --------------

#[derive(Deserialize)]
struct FsQuery {
    dir: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FsEntry {
    name: String,
    dir: bool,
    exe: bool,
}

/// Whether a `program` field can honestly hold this file. Windows answers
/// by extension; Unix answers by the executable bit - a `+.x` shell script
/// is a program, a `.exe` without it is not (SPEC §process.panel: the
/// picker is cross-platform, the test of a program is the platform's own).
#[cfg(windows)]
fn is_executable(_meta: &std::fs::Metadata, name: &str) -> bool {
    let Some(ext) = name.rsplit('.').next() else {
        return false;
    };
    matches!(ext.to_ascii_lowercase().as_str(), "exe" | "cmd" | "bat" | "com" | "ps1")
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata, _name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// One directory, read-only: entries sorted folders-first then name,
/// capped so a System32 cannot flood the sheet. `dir` absent starts at the
/// user's home; a path that is not a readable directory is a 400 naming it.
async fn fs_list(State(state): State<Arc<ProcessState>>, Query(q): Query<FsQuery>) -> Response {
    if state.current().is_none() {
        return not_running();
    }
    let asked = q.dir.unwrap_or_else(home_root);
    let dir = std::path::PathBuf::from(&asked);
    let readable = std::fs::metadata(&dir).ok().filter(|m| m.is_dir());
    if readable.is_none() {
        return admin_error(
            StatusCode::BAD_REQUEST,
            &format!("could not read {asked}: not a directory"),
        );
    }
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) => {
            return admin_error(
                StatusCode::BAD_REQUEST,
                &format!("could not read {asked}: {err}"),
            )
        }
    };
    let mut dirs: Vec<FsEntry> = Vec::new();
    let mut files: Vec<FsEntry> = Vec::new();
    let mut truncated = false;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if dirs.len() + files.len() >= 2000 {
            truncated = true;
            break;
        }
        let e = FsEntry {
            name: name.clone(),
            dir: is_dir,
            exe: !is_dir
                && entry
                    .metadata()
                    .map(|m| is_executable(&m, &name))
                    .unwrap_or(false),
        };
        if is_dir {
            dirs.push(e);
        } else {
            files.push(e);
        }
    }
    let by_name = |a: &FsEntry, b: &FsEntry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    let mut entries = dirs;
    entries.extend(files);
    let parent = dir
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty());
    // The platform's separator rides along: the panel joins a walked path with
    // what the server said, never with a hard-coded backslash (SPEC
    // §process.panel - the same panel serves a Windows box and a Linux box).
    let sep = if cfg!(windows) { "\\" } else { "/" };
    admin_json(StatusCode::OK, json!({
        "dir": asked,
        "sep": sep,
        "parent": parent,
        "entries": entries,
        "truncated": truncated,
    }))
}

/// The picker's start: the account's home, the way the gateway itself resolves
/// it (USERPROFILE on Windows, HOME elsewhere). The route is read-only, so a
/// machine without either lists the process's current directory.
fn home_root() -> String {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

