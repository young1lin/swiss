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

//! The start/stop/restart engines (SPEC §process.apps): everything the
//! routes do to an app, over three injected seams - what listens ([Probe]),
//! how a program starts ([Spawner]) and how a tree dies ([Killer]) - so every
//! rule of the spec's tables is a unit test with no machine under it. The
//! real seams (Sys*) wrap the platform reads and the detached spawn of
//! swiss-host.
//!
//! Nothing between actions: no watchdog, no polling loop, no restart policy
//! (ADR-036). The window exists only while a start is deciding, and a stop
//! only while it is waiting for the app to honor it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use swiss_core::secure::refs::resolve_strict_collect;
use swiss_core::secure::statefile;
use swiss_host::services::BootInfo;
use tokio::sync::mpsc;

use crate::config::{expand_dir, expand_program, image_matches, AppConfig, ConfigRules};
use crate::facts::FactSnapshot;
use crate::history::{HistoryEntry, ProcessHistory};
use crate::logs;
use crate::status::{self, AppStatus, OwnerFact, PortFact, TrackedRecord, Verdict};

mod seams;

pub use self::seams::*;

/// The start window: 30 s of 250 ms polls (SPEC §process.apps).
pub const START_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// The poll step inside every window.
pub const POLL_STEP: std::time::Duration = std::time::Duration::from_millis(250);
/// After the ports open, they must still be open 2 s later (SPEC §process.apps).
pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
/// A stop command gets 30 s before the row offers force.
pub const STOP_COMMAND_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// A kill gets 15 s to take, then 5 s more per survivor to be gone.
pub const KILL_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);
pub const SURVIVOR_PROBE: std::time::Duration = std::time::Duration::from_secs(5);
/// Boot's one autostart retry waits this long (SPEC §process.apps): a start
/// at sign-in races the network and mapped drives, and the retry owes the
/// race a gap.
pub const AUTOSTART_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// What listens, and what a pid is. One trait so every verdict in this module
/// is testable against a table-driven fake.
#[async_trait::async_trait]
pub trait Probe: Send + Sync {
    /// The whole LISTEN table; None-able as an io error the caller turns into
    /// the spec's unreadable-table rows.
    async fn listeners(&self) -> std::io::Result<Vec<PortFact>>;
    /// Image and creation time for one pid, read once per distinct owner.
    async fn process(&self, pid: u32) -> Option<OwnerFact>;
}

/// How a program starts. Everything arrives RESOLVED (references never cross
/// this seam, so no seam implementation can log a value).
#[async_trait::async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn(&self, spec: &SpawnSpec) -> Result<Spawned, String>;
}

/// How a tree dies: returns the pids that SURVIVED the kill, reported never
/// swallowed (SPEC §process.apps).
#[async_trait::async_trait]
pub trait Killer: Send + Sync {
    async fn kill_tree(&self, pid: u32) -> Vec<u32>;
}

/// The clock, for history stamps and uptime. Injected so no test reads the
/// wall (the WINDOWS themselves run on tokio time, which start_paused owns).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// One resolved spawn: typed argv, a working directory, an env overlay, and
/// where the output is captured.
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub capture: PathBuf,
}

/// What a spawner hands back: the identity to track, and - when the spawner
/// can see exits - a one-shot that fires with the exit code while the start
/// window is still open. Dropped unread when the window ends first: the
/// system-level exit channel (the plugin's pump) is what reports late exits.
pub struct Spawned {
    pub pid: u32,
    pub created_at: Option<u64>,
    pub shares_job: bool,
    pub exits: Option<tokio::sync::oneshot::Receiver<i32>>,
}

/// A late exit of a started app, as the pump receives it: one history line
/// (action `exit`), never a copy of the output.
#[derive(Clone, Debug)]
pub struct ExitLine {
    pub app: String,
    pub pid: u32,
    pub code: i32,
    /// Unix milliseconds.
    pub at: u64,
}

/// What the routes and the history share: one action's final say.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub action: &'static str,
    pub ok: bool,
    pub message: String,
    pub pid: Option<u32>,
    pub image: Option<String>,
    pub exit_code: Option<i32>,
    pub forced: bool,
    pub can_force: bool,
    /// A conflict refusal (start-with-swiss leaves these alone, no retry).
    pub conflict: bool,
}

impl Outcome {
    fn failed(action: &'static str, message: String) -> Self {
        Outcome {
            action,
            ok: false,
            message,
            pid: None,
            image: None,
            exit_code: None,
            forced: false,
            can_force: false,
            conflict: false,
        }
    }
}

/// Why a wrapper refused before any engine ran. The routes map these to 404 /
/// 409 / 409-with-message.
#[derive(Debug)]
pub enum ControlError {
    UnknownApp,
    Busy(&'static str),
    Refused(String),
}

/// The last outcome the row shows, plus its exit code when there was one.
#[derive(Clone, Debug)]
pub struct LastOutcome {
    pub action: String,
    pub ok: bool,
    pub message: String,
    pub at: u64,
    pub exit_code: Option<i32>,
    pub can_force: bool,
}

/// The row's `lastAction` (SPEC §process.apps): the outcome the panel and
/// the CLI poll for, in the shape they read - the action, its verdict, its
/// sentence, when, and `canForce` only when there is something to force.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LastAction {
    pub action: String,
    pub ok: bool,
    pub message: String,
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_force: Option<bool>,
}

/// One row of GET /api/process (SPEC §process.apps): the verdict and its
/// facts, plus what the gateway last did to the app and nothing else.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub ports: Vec<u16>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub down_ports: Vec<u16>,
    /// Every port the row's pid actually holds - the listener table's fact,
    /// declared or not (SPEC §process.apps): the panel shows the declared
    /// chips and any extra held port beside them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub held_ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action: Option<LastAction>,
    pub autostart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
    pub stop_is_kill: bool,
    pub has_app_log: bool,
}

/// Everything [AppControl] needs, injected: the seams above plus where the
/// three files live (all under the home, but paths the tests own).
pub struct ControlDeps {
    pub rules: ConfigRules,
    pub boot: BootInfo,
    pub probe: Arc<dyn Probe>,
    pub spawner: Arc<dyn Spawner>,
    pub killer: Arc<dyn Killer>,
    pub clock: Arc<dyn Clock>,
    /// `.process-apps-<port>.json`, sealed.
    pub record_path: PathBuf,
    /// The process history store (SPEC §host.history): constructed closed by
    /// the composition, opened inside the plugin's enabled scope.
    pub history: Arc<ProcessHistory>,
    /// `logs/apps`: the capture files, `<name>.log`.
    pub capture_dir: PathBuf,
    /// This gateway's pid: a hard kill refuses it by pid AND by image.
    pub gateway_pid: u32,
}

/// The engines. One instance per plugin instance, behind an Arc.
pub struct AppControl {
    rules: ConfigRules,
    boot: BootInfo,
    apps: Mutex<Vec<AppConfig>>,
    record: Mutex<HashMap<String, TrackedRecord>>,
    last: Mutex<HashMap<String, LastOutcome>>,
    /// Arc-shared so a busy guard can OWN a handle instead of borrowing the
    /// control: a wrapper claims synchronously and moves the guard into the
    /// engine task it spawns (no ownerless window between 409 and engine).
    busy: Arc<Mutex<HashMap<String, &'static str>>>,
    /// Pids a stop/restart is killing right now: their exit lines are the
    /// action's own outcome, not a second history line.
    suppress: Mutex<HashSet<u32>>,
    history: Arc<ProcessHistory>,
    deps: Seams,
    capture_dir: PathBuf,
    record_path: PathBuf,
    gateway_pid: u32,
}

struct Seams {
    probe: Arc<dyn Probe>,
    spawner: Arc<dyn Spawner>,
    killer: Arc<dyn Killer>,
    clock: Arc<dyn Clock>,
}

/// A verified identity, not a bare pid. A kill re-reads and compares all of
/// these facts immediately before acting, including after a command wait.
struct KillTarget {
    pid: u32,
    created_at: u64,
    image: String,
}

/// Removes the busy mark when the engine ends, however it ends. The guard
/// owns its own handle to the mark table rather than borrowing the control,
/// so it moves into the spawned engine task - the mark is continuously owned
/// from the wrapper's synchronous claim to the engine's end.
struct BusyGuard {
    busy: Arc<Mutex<HashMap<String, &'static str>>>,
    name: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.name);
    }
}

impl AppControl {
    pub fn new(deps: ControlDeps) -> Arc<Self> {
        let ControlDeps {
            rules,
            boot,
            probe,
            spawner,
            killer,
            clock,
            record_path,
            history,
            capture_dir,
            gateway_pid,
        } = deps;
        Arc::new(AppControl {
            rules,
            boot,
            apps: Mutex::new(Vec::new()),
            record: Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
            busy: Arc::new(Mutex::new(HashMap::new())),
            suppress: Mutex::new(HashSet::new()),
            history,
            deps: Seams {
                probe,
                spawner,
                killer,
                clock,
            },
            capture_dir,
            record_path,
            gateway_pid,
        })
    }

    /// The boot's tracked record: read what survived the last gateway (a
    /// record whose pid is gone or reused drops here, SPEC §process.apps).
    /// A record for an app that left the config goes with the app; for an app
    /// WITHOUT ports the pin is the whole verdict, so an unpinned record drops
    /// - for an app WITH ports the ports decide and the record stays.
    pub async fn load_tracked(&self) {
        if let Some(map) = read_record(&self.record_path) {
            *self.record.lock().unwrap_or_else(|e| e.into_inner()) = map;
        }
        let apps = self.apps();
        let names: Vec<String> = self
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for name in names {
            let Some(app) = apps.iter().find(|a| a.name == name) else {
                self.record
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&name);
                continue;
            };
            if !app.ports.is_empty() {
                continue;
            }
            let record = self
                .record
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&name)
                .cloned();
            if let Some(record) = record {
                let fact = self.deps.probe.process(record.pid).await;
                if !status::pin_record_matches(Some(&record), fact.as_ref()) {
                    self.record
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&name);
                }
            }
        }
        self.save_tracked();
    }

    fn save_tracked(&self) {
        let map: serde_json::Map<String, serde_json::Value> = self
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, record)| {
                (
                    name.clone(),
                    serde_json::json!({
                        "pid": record.pid,
                        "createdAt": record.created_at,
                        "image": record.image,
                    }),
                )
            })
            .collect();
        if let Err(err) = statefile::write_secure_json(
            &self.record_path,
            &serde_json::Value::Object(map),
        ) {
            swiss_core::log::warn(
                "the process tracked record could not be saved",
                Some(serde_json::json!({ "error": err })),
            );
        }
    }

    pub fn set_apps(&self, apps: Vec<AppConfig>) {
        // Records for apps that left the config are not the config's business
        // to keep: they drop with the row. Only that is worth a save: a boot's
        // first call runs before load_tracked has read the file, so the map is
        // still empty here and saving it would erase what the last gateway left.
        let names: HashSet<String> = apps.iter().map(|a| a.name.clone()).collect();
        let dropped = {
            let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
            let before = record.len();
            record.retain(|name, _| names.contains(name));
            record.len() != before
        };
        *self.apps.lock().unwrap_or_else(|e| e.into_inner()) = apps;
        if dropped {
            self.save_tracked();
        }
    }

    pub fn apps(&self) -> Vec<AppConfig> {
        self.apps.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn app(&self, name: &str) -> Option<AppConfig> {
        self.apps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|a| a.name == name)
            .cloned()
    }

    pub fn history(&self) -> &ProcessHistory {
        &self.history
    }

    fn capture_path(&self, name: &str) -> PathBuf {
        self.capture_dir.join(format!("{name}.log"))
    }

    /// The capture file's path, for the log route (the app's OWN log file
    /// comes from the config row, never from a request).
    pub fn capture_file(&self, name: &str) -> PathBuf {
        self.capture_path(name)
    }

    /// The hourly tick's half (SPEC §process.apps): sweep every declared
    /// capture, metadata only. The plugin owns the clock; this owns the list.
    pub fn sweep_captures(&self) {
        for app in self.apps().iter() {
            let capture = self.capture_path(&app.name);
            if let Err(err) = logs::sweep_capture(&capture) {
                swiss_core::log::warn(
                    "the capture file could not be swept",
                    Some(serde_json::json!({
                        "file": capture.display().to_string(),
                        "error": err.to_string(),
                    })),
                );
            }
        }
    }

    fn busy_action(&self, name: &str) -> Option<&'static str> {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
    }


    /// Insert-only busy claim: None when the app already carries a mark. The
    /// wrappers claim here synchronously (a racing request sees 409 at once)
    /// and move the returned guard into the engine they spawn; the boot pass
    /// and the direct-call tests claim inside run_*. A mark that lands while
    /// a wrapper is between its check and its claim is HONORED, never
    /// overwritten: the loser refuses without spawning anything. This closes
    /// the window where a user action and the boot pass (or two racing
    /// wrappers) each overwrote the other's mark and both ran (round-4
    /// review).
    fn try_claim(&self, name: &str, action: &'static str) -> Option<BusyGuard> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if busy.contains_key(name) {
            return None;
        }
        busy.insert(name.to_string(), action);
        drop(busy);
        Some(BusyGuard {
            busy: self.busy.clone(),
            name: name.to_string(),
        })
    }

    /// Capture only the configured apps and their records for this read. No
    /// lock crosses a probe await, and no facts survive into another action.
    async fn facts(&self, apps: &[AppConfig]) -> FactSnapshot {
        let records = {
            let stored = self.record.lock().unwrap_or_else(|e| e.into_inner());
            apps.iter().filter_map(|app| {
                stored.get(&app.name).map(|record| (app.name.clone(), record.clone()))
            }).collect()
        };
        FactSnapshot::gather(self.deps.probe.as_ref(), apps, records).await
    }

    /// The action snapshot stays fresh on every call; row reads instead share
    /// one fact pass across the whole answer (SPEC §process.apps).
    async fn snapshot(&self, app: &AppConfig) -> Verdict {
        let facts = self.facts(std::slice::from_ref(app)).await;
        let verdict = facts.verdict(app);
        if verdict.drop_record {
            self.drop_record(&app.name);
        }
        verdict
    }

    /// Every app's row, from one listener table and one fact read per distinct
    /// owner (SPEC §process.apps), shared by verdicts and held-port columns.
    pub async fn rows(&self) -> Vec<Row> {
        let apps = self.apps();
        let now = self.deps.clock.now_ms();
        let facts = self.facts(&apps).await;
        let mut rows = Vec::with_capacity(apps.len());
        for app in &apps {
            let verdict = facts.verdict(app);
            if verdict.drop_record {
                self.drop_record(&app.name);
            }
            let last = self
                .last
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&app.name)
                .cloned();
            let (status_word, down_ports, reason) = row_words(&verdict, &app.ports);
            let uptime_sec = verdict
                .created_at
                .filter(|_| verdict.status == AppStatus::Running)
                .map(|created| now.saturating_sub(created) / 1000);
            // A conflict names its stranger in the row's own columns (SPEC
            // §process.apps): the panel links the pid to the children table.
            let (conflict_pid, conflict_image) = match &verdict.status {
                AppStatus::Conflict { pid, image, .. } => (Some(*pid), Some(image.clone())),
                _ => (None, None),
            };
            let held_ports = facts.held_ports(verdict.pid);
            rows.push(Row {
                name: app.name.clone(),
                status: status_word,
                reason: (!reason.is_empty()).then_some(reason),
                pid: verdict.pid,
                ports: app.ports.clone(),
                down_ports,
                held_ports,
                conflict_pid,
                conflict_image,
                uptime_sec,
                busy: self.busy_action(&app.name),
                last_action: last.map(|l| LastAction {
                    action: l.action,
                    ok: l.ok,
                    message: l.message,
                    at: l.at,
                    can_force: l.can_force.then_some(true),
                }),
                autostart: app.autostart,
                ui: app.ui.clone(),
                stop_is_kill: app.stop.is_none(),
                has_app_log: app.log.is_some(),
            });
        }
        rows
    }

    // --- the wrappers the routes call -------------------------------------

    /// The gateway's own uptime for the row table's foot (SPEC §process.apps):
    /// now minus the boot's creation time, over the injected clock so the
    /// paused tests see 0, not a lie.
    pub fn gateway_uptime_sec(&self, created_at_ms: u64) -> u64 {
        self.deps.clock.now_ms().saturating_sub(created_at_ms) / 1000
    }

    /// POST start: refuse synchronously (unknown / busy / already something),
    /// then run the window in the background - the answer is 202, the outcome
    /// lands in the row and the history (SPEC §process.apps).
    pub async fn start(self: &Arc<Self>, name: &str, by: &str) -> Result<(), ControlError> {
        let app = self.app(name).ok_or(ControlError::UnknownApp)?;
        if let Some(current) = self.busy_action(name) {
            return Err(ControlError::Busy(current));
        }
        let verdict = self.snapshot(&app).await;
        if let Some(refusal) = start_refusal(&verdict) {
            return Err(ControlError::Refused(refusal));
        }
        // Claim synchronously and HAND the guard to the spawned engine: the
        // mark exists before this call answers, and a claim that landed
        // during the snapshot await above is honored - 409, no engine, no
        // overwrite (the round-4 review's double-spawn window).
        let Some(claim) = self.try_claim(name, "start") else {
            return Err(ControlError::Busy(self.busy_action(name).unwrap_or("busy")));
        };
        let control = self.clone();
        let app = app.clone();
        let by = by.to_string();
        tokio::spawn(async move {
            control.start_under(claim, &app, &by).await;
        });
        Ok(())
    }

    /// POST stop: same shape; `force` skips the stop command and kills.
    pub async fn stop(
        self: &Arc<Self>,
        name: &str,
        by: &str,
        force: bool,
    ) -> Result<(), ControlError> {
        let app = self.app(name).ok_or(ControlError::UnknownApp)?;
        if let Some(current) = self.busy_action(name) {
            return Err(ControlError::Busy(current));
        }
        // The same synchronous claim as start: the mark exists before the
        // 202 answers, and the guard rides into the engine.
        let Some(claim) = self.try_claim(name, "stop") else {
            return Err(ControlError::Busy(self.busy_action(name).unwrap_or("busy")));
        };
        let control = self.clone();
        let app = app.clone();
        let by = by.to_string();
        tokio::spawn(async move {
            control.stop_under(claim, &app, &by, force).await;
        });
        Ok(())
    }

    /// POST restart: one busy mark for the whole thing, one history line.
    /// `force` skips the stop command in the stop half, exactly as a forced
    /// stop would (SPEC §process.apps: the stop runs without force unless the
    /// request asked for it).
    pub async fn restart(
        self: &Arc<Self>,
        name: &str,
        by: &str,
        force: bool,
    ) -> Result<(), ControlError> {
        let app = self.app(name).ok_or(ControlError::UnknownApp)?;
        if let Some(current) = self.busy_action(name) {
            return Err(ControlError::Busy(current));
        }
        // The same synchronous claim as start: one mark for the whole
        // restart, owned from here to the engine's end.
        let Some(claim) = self.try_claim(name, "restart") else {
            return Err(ControlError::Busy(self.busy_action(name).unwrap_or("busy")));
        };
        let control = self.clone();
        let app = app.clone();
        let by = by.to_string();
        tokio::spawn(async move {
            control.restart_under(claim, &app, &by, force).await;
        });
        Ok(())
    }

    // --- the engines ------------------------------------------------------

    /// The start, whole: resolve strictly, capture header, spawn, the window,
    /// the settle, the record, the history line. Public for the tests and for
    /// start-with-swiss, which runs it inline.
    pub async fn run_start(&self, app: &AppConfig, by: &str) -> Outcome {
        // Insert-only claim: an app already carrying a mark (a user action in
        // flight, or a racing engine) is left to it. The refusal returns
        // WITHOUT a history line - the action that holds the mark records its
        // own outcome, and a phantom "busy" line over a live start would lie.
        let Some(claim) = self.try_claim(&app.name, "start") else {
            let current = self.busy_action(&app.name).unwrap_or("busy");
            return Outcome::failed("start", format!("busy with {current}; nothing was run"));
        };
        self.start_under(claim, app, by).await
    }

    /// The start engine under a claim the caller already holds (the wrapper's
    /// synchronous claim, adopted here so the mark is owned without a gap).
    async fn start_under(&self, _claim: BusyGuard, app: &AppConfig, by: &str) -> Outcome {
        let outcome = self.start_engine(app, by).await;
        self.finish(&app.name, by, outcome).await
    }

    /// The stop, whole (SPEC §process.apps): the stop command when declared
    /// and not forced, else the hard kill with its re-resolve and refusals.
    pub async fn run_stop(&self, app: &AppConfig, by: &str, force: bool) -> Outcome {
        // Same insert-only claim as run_start: the mark holder records the
        // outcome, this call records nothing.
        let Some(claim) = self.try_claim(&app.name, "stop") else {
            let current = self.busy_action(&app.name).unwrap_or("busy");
            return Outcome::failed("stop", format!("busy with {current}; nothing was run"));
        };
        self.stop_under(claim, app, by, force).await
    }

    /// The stop engine under a claim the caller already holds.
    async fn stop_under(
        &self,
        _claim: BusyGuard,
        app: &AppConfig,
        by: &str,
        force: bool,
    ) -> Outcome {
        let outcome = self.stop_engine(app, by, force).await;
        self.finish(&app.name, by, outcome).await
    }

    /// Restart: stop then start under one mark, ONE line - its outcome is the
    /// last thing that happened, and a failed stop ends it (SPEC §process.apps).
    /// The stop half honors the request's `force`.
    pub async fn run_restart(&self, app: &AppConfig, by: &str, force: bool) -> Outcome {
        // Same insert-only claim as run_start: the mark holder records the
        // outcome, this call records nothing.
        let Some(claim) = self.try_claim(&app.name, "restart") else {
            let current = self.busy_action(&app.name).unwrap_or("busy");
            return Outcome::failed("restart", format!("busy with {current}; nothing was run"));
        };
        self.restart_under(claim, app, by, force).await
    }

    /// The restart engine under a claim the caller already holds.
    async fn restart_under(
        &self,
        _claim: BusyGuard,
        app: &AppConfig,
        by: &str,
        force: bool,
    ) -> Outcome {
        let mut stopped = self.stop_engine(app, by, force).await;
        if !stopped.ok {
            stopped.action = "restart";
            return self.finish(&app.name, by, stopped).await;
        }
        let mut started = self.start_engine(app, by).await;
        started.action = "restart";
        if started.ok {
            // Ports or not, the inner verb reads once: "listening on 14317
            // (pid 8552)" and "(pid 8552)" both land as "restarted <detail>",
            // never "restarted (started (pid 8552))".
            let detail = started
                .message
                .strip_prefix("started; ")
                .or_else(|| started.message.strip_prefix("started "))
                .unwrap_or(&started.message)
                .to_string();
            started.message = if detail.starts_with('(') {
                format!("restarted {detail}")
            } else {
                format!("restarted ({detail})")
            };
        } else {
            started.message = format!("restarted partway: {}", started.message);
        }
        self.finish(&app.name, by, started).await
    }

    async fn start_engine(&self, app: &AppConfig, by: &str) -> Outcome {
        // The verdict the wrapper checked can have moved: re-derive. Running,
        // partial and conflict are refusals; stopped and unknown proceed.
        let verdict = self.snapshot(app).await;
        if let Some(refusal) = start_refusal(&verdict) {
            let mut outcome = Outcome::failed("start", refusal);
            outcome.conflict = matches!(verdict.status, AppStatus::Conflict { .. });
            return outcome;
        }

        // Resolve everything the command stands for, strictly: a missing
        // reference fails the start naming the reference, never a value.
        let mut resolved_args = Vec::with_capacity(app.start.args.len());
        let mut values: Vec<String> = Vec::new();
        for arg in &app.start.args {
            match resolve_strict_collect(arg) {
                Ok((expanded, mut found)) => {
                    resolved_args.push(expanded);
                    values.append(&mut found);
                }
                Err(err) => return Outcome::failed("start", err),
            }
        }
        let mut env: Vec<(String, String)> = Vec::new();
        for (key, raw) in &app.env {
            match resolve_strict_collect(raw) {
                Ok((expanded, mut found)) => {
                    env.push((key.clone(), expanded));
                    values.append(&mut found);
                }
                Err(err) => return Outcome::failed("start", err),
            }
        }
        let cwd = app.dir.as_deref().and_then(expand_dir);

        // Capture hygiene, then the header, then the spawn (SPEC §process.apps).
        let capture = self.capture_path(&app.name);
        if let Err(err) = logs::sweep_capture(&capture) {
            swiss_core::log::warn(
                "the capture file could not be swept",
                Some(serde_json::json!({ "file": capture.display().to_string(), "error": err.to_string() })),
            );
        }
        if let Err(err) = logs::append_header(&capture, &swiss_core::log::iso_now(), "start", by) {
            return Outcome::failed(
                "start",
                format!("could not write {}: {err}", capture.display()),
            );
        }

        let spec = SpawnSpec {
            program: expand_program(&app.start.program),
            args: resolved_args,
            cwd,
            env,
            capture: capture.clone(),
        };
        let spawned = match self.deps.spawner.spawn(&spec).await {
            Ok(spawned) => spawned,
            Err(sentence) => return Outcome::failed("start", sentence),
        };
        let mut exits = spawned.exits;
        let pid = spawned.pid;

        // The window: 30 s of 250 ms polls (SPEC §process.apps). "Up" means
        // every declared port owned by the image - or, for an app without
        // ports, the pid still pinned in the probe.
        let deadline = tokio::time::Instant::now() + START_WINDOW;
        // A launcher that exits 0 does NOT end the window (SPEC §process.apps):
        // the child it started may still bind, so polling goes on to the end.
        let mut launcher_exited = false;
        // The last poll's stranger on a declared port: the window's failure
        // names it. The port IS bound, so "not listening" would be false - on
        // 2026-11-10 dsh's own node.exe held 3080 under the image `dsh` and the
        // start read "not listening on 3080 after 30s".
        let mut stranger: Option<(u32, String, u16)> = None;
        loop {
            if let Some(code) = poll_exit(&mut exits) {
                if code != 0 {
                    return self
                        .exit_while_starting(app, pid, code, &capture, &values)
                        .await;
                }
                if app.ports.is_empty() {
                    // Exit 0 without ports is an ok (SPEC §process.apps): the
                    // program ran to completion; ports are what would have
                    // let the record track what it left behind.
                    let mut outcome = Outcome::failed("start", String::new());
                    outcome.ok = true;
                    outcome.message = "exited 0; declare ports to track what it started".to_string();
                    outcome.pid = Some(pid);
                    outcome.exit_code = Some(0);
                    return outcome;
                }
                launcher_exited = true;
            }
            let up = if app.ports.is_empty() {
                self.deps.probe.process(pid).await.is_some()
            } else {
                let state = self.ports_state(app).await;
                stranger = match &state {
                    PortsState::Stranger(other, image, port) => Some((*other, image.clone(), *port)),
                    _ => None,
                };
                match state {
                    PortsState::AllUp => true,
                    // An unreadable table judges by the no-ports rows (SPEC
                    // §process.apps): the spawned pid decides. A dead
                    // launcher stays "not up", which keeps the polling honest.
                    PortsState::Unreadable => self.deps.probe.process(pid).await.is_some(),
                    _ => false,
                }
            };
            if up {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                // The window ends: the app stays exactly as it is - a live
                // process is NEVER killed by a failed start (SPEC §process.apps).
                if let Some((other, image, port)) = &stranger {
                    // A port that came up under another image is a conflict,
                    // not a slow app: start with swiss does not retry it.
                    let mut outcome = Outcome::failed(
                        "start",
                        window_stranger_sentence(app, pid, launcher_exited, *other, image, *port),
                    );
                    outcome.conflict = true;
                    if launcher_exited {
                        outcome.exit_code = Some(0);
                    }
                    return outcome;
                }
                let mut outcome = Outcome::failed(
                    "start",
                    if app.ports.is_empty() {
                        format!(
                            "pid {pid} did not stay up within {}s; left alone",
                            START_WINDOW.as_secs()
                        )
                    } else if launcher_exited {
                        format!(
                            "launcher exited 0 but nothing listens on {}",
                            first_port(app)
                        )
                    } else {
                        format!(
                            "started (pid {pid}) but not listening on {} after {}s; still running, not killed",
                            first_port(app),
                            START_WINDOW.as_secs()
                        )
                    },
                );
                if launcher_exited {
                    // The launcher's own code belongs in the failure it caused.
                    outcome.exit_code = Some(0);
                }
                return outcome;
            }
            tokio::time::sleep(POLL_STEP).await;
        }

        // The settle: what came up must STILL be up 2 s later (SPEC §process.apps).
        tokio::time::sleep(SETTLE).await;
        if let PortsState::Stranger(other, image, port) = self.ports_state(app).await {
            let mut outcome = Outcome::failed(
                "start",
                format!("port {port} was taken by pid {other} ({image}) during the settle"),
            );
            outcome.conflict = true;
            return outcome;
        }
        let settled = if app.ports.is_empty() {
            self.deps.probe.process(pid).await.is_some()
        } else {
            match self.ports_state(app).await {
                PortsState::AllUp => true,
                // The polling phase's own rule (SPEC §process.apps), one window later:
                // an unreadable table judges by the spawned pid. Demanding AllUp here
                // alone misjudged a healthy process as dead two seconds after polling
                // called it up - failed outcome, no tracking record saved.
                PortsState::Unreadable => self.deps.probe.process(pid).await.is_some(),
                _ => false,
            }
        };
        if !settled {
            let code = poll_exit(&mut exits);
            let mut message = if app.ports.is_empty() {
                format!(
                    "pid {pid} came up and died within the {}s settle",
                    SETTLE.as_secs()
                )
            } else {
                format!(
                    "listened on {} then stopped within the {}s settle",
                    first_port(app),
                    SETTLE.as_secs()
                )
            };
            if let Some(code) = code {
                message.push_str(&format!(" (exit code {code})"));
            }
            return Outcome::failed("start", message);
        }

        // Running: pin it. The record is what a later read trusts when the
        // ports are gone (SPEC §process.apps). A launcher-pattern start pins
        // the PORT OWNER: the launcher is dead, and whoever holds the port is
        // the app now - the exit watcher follows the record, so it must
        // follow the child, not the corpse.
        let (tracked_pid, fact) = if launcher_exited {
            match self.port_owner(app).await {
                Some((owner_pid, owner_fact)) => (owner_pid, Some(owner_fact)),
                None => {
                    return Outcome::failed(
                        "start",
                        format!(
                            "the launcher exited 0 and {} opened, but its owner is already gone",
                            first_port(app)
                        ),
                    )
                }
            }
        } else {
            (pid, self.deps.probe.process(pid).await)
        };
        let image = fact
            .as_ref()
            .map(|f| f.image.clone())
            .or_else(|| app.image.clone())
            .unwrap_or_default();
        let created_at = if launcher_exited {
            // The owner's creation time is the pin; the launcher's timestamp
            // belongs to a different, dead process.
            fact.as_ref().map(|f| f.created_at).unwrap_or(0)
        } else {
            spawned
                .created_at
                .or_else(|| fact.as_ref().map(|f| f.created_at))
                .unwrap_or(0)
        };
        let record = TrackedRecord {
            pid: tracked_pid,
            created_at,
            image: image.clone(),
        };
        self.record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(app.name.clone(), record);
        self.save_tracked();

        let mut outcome = Outcome {
            action: "start",
            ok: true,
            message: if app.ports.is_empty() {
                format!("started (pid {tracked_pid})")
            } else {
                format!("started; listening on {} (pid {tracked_pid})", port_list(app))
            },
            pid: Some(tracked_pid),
            image: Some(image),
            exit_code: None,
            forced: false,
            can_force: false,
            conflict: false,
        };
        if spawned.shares_job {
            outcome
                .message
                .push_str("; shares the gateway's job: it dies with the gateway");
        }
        outcome
    }

    /// A non-zero exit observed inside the window: the outcome table's exit
    /// row, with the masked tail of the capture, never a raw value. A code-0
    /// exit is the launcher row, handled by the window loop itself.
    async fn exit_while_starting(
        &self,
        _app: &AppConfig,
        pid: u32,
        code: i32,
        capture: &std::path::Path,
        values: &[String],
    ) -> Outcome {
        let mut outcome = Outcome::failed("start", format!("exited with code {code}"));
        if code != 0 {
            // The masked tail: the last few lines of the capture, header line
            // included (it is the newest line when a launcher dies instantly),
            // every resolved value bulleted out.
            if let Ok(Some(tail)) = logs::read_tail(capture, None, values) {
                let window: Vec<&str> = tail
                    .lines
                    .iter()
                    .rev()
                    .take(20)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|line| line.as_str())
                    .collect();
                if !window.is_empty() {
                    outcome.message.push_str(": ");
                    outcome.message.push_str(&window.join(" | "));
                }
            }
        }
        outcome.pid = Some(pid);
        outcome.exit_code = Some(code);
        outcome
    }

    async fn stop_engine(&self, app: &AppConfig, by: &str, force: bool) -> Outcome {
        // The gateway refusal is decided BEFORE anything else (SPEC §process.apps):
        // a stranger on the port is a refusal to stop, but a stranger whose
        // pid or image IS this gateway is a refusal to KILL, said that way.
        let early_targets = self.target_pids(app).await;
        if let Some(sentence) = self.gateway_refusal(&early_targets).await {
            return Outcome::failed("stop", sentence);
        }
        let verdict = self.snapshot(app).await;
        match &verdict.status {
            // Nothing to stop: an honest ok, and a stale record drops here.
            AppStatus::Stopped => {
                return Outcome {
                    action: "stop",
                    ok: true,
                    message: "already stopped".to_string(),
                    pid: None,
                    image: None,
                    exit_code: None,
                    forced: false,
                    can_force: false,
                    conflict: false,
                }
            }
            // A stranger is never ours to stop (SPEC §process.apps).
            AppStatus::Conflict { pid, image, port } => {
                return Outcome::failed(
                    "stop",
                    conflict_sentence(*pid, image, *port),
                )
            }
            // Unknown is a refusal, not an ok (SPEC §process.apps): nothing
            // is identified to stop - not the empty-target ok, and not a
            // stop command spawned into the void either.
            AppStatus::Unknown { .. } => {
                return Outcome::failed("stop", "nothing identified to stop".to_string())
            }
            _ => {}
        }

        // The stop command, when declared and not forced: 30 s to be honored
        // (SPEC §process.apps).
        if let Some(stop) = app.stop.as_ref() {
            if !force {
                let capture = self.capture_path(&app.name);
                if let Err(err) =
                    logs::append_header(&capture, &swiss_core::log::iso_now(), "stop", by)
                {
                    return Outcome::failed(
                        "stop",
                        format!("could not write {}: {err}", capture.display()),
                    );
                }
                let mut resolved_args = Vec::new();
                for arg in &stop.args {
                    match resolve_strict_collect(arg) {
                        Ok((expanded, _)) => resolved_args.push(expanded),
                        Err(err) => return Outcome::failed("stop", err),
                    }
                }
                let spec = SpawnSpec {
                    program: expand_program(&stop.program),
                    args: resolved_args,
                    cwd: app.dir.as_deref().and_then(expand_dir),
                    env: Vec::new(),
                    capture,
                };
                let spawned = match self.deps.spawner.spawn(&spec).await {
                    Ok(spawned) => spawned,
                    Err(sentence) => return Outcome::failed("stop", sentence),
                };
                // The spawn's creation time pins this command. Its image is
                // captured now, but no kill may trust it after the wait below.
                let command_target = self.read_target(spawned.pid).await.and_then(|target| {
                    if spawned.created_at == Some(target.created_at) {
                        Ok(target)
                    } else {
                        Err(format!("stop command pid {} has no matching spawn identity; not killed", spawned.pid))
                    }
                });
                // A completed command is never a kill target. Only an overrun
                // reaches the fresh identity check, and a failed check leaves
                // the pid alone while the app's own stop verdict still decides.
                let completed = if let Some(exits) = spawned.exits {
                    matches!(tokio::time::timeout(STOP_COMMAND_WINDOW, exits).await, Ok(Ok(_)))
                } else {
                    tokio::time::sleep(STOP_COMMAND_WINDOW).await;
                    false
                };
                let command_warning = if completed {
                    None
                } else {
                    match command_target {
                        Ok(target) => self.kill_identified(&target).await.err(),
                        Err(sentence) => Some(sentence),
                    }
                };
                let command_note = command_warning.map(|sentence| format!("; {sentence}")).unwrap_or_default();
                // Phase two: the app itself, up to 15 s of status polling.
                // Down is done; still up is a failure with force offered.
                let deadline = tokio::time::Instant::now() + KILL_WINDOW;
                let mut unreadable;
                loop {
                    match self.down_state(app).await {
                        DownState::Down => {
                            self.drop_record(&app.name);
                            return Outcome {
                                action: "stop",
                                ok: true,
                                message: format!("stopped by {}{command_note}", stop.program),
                                pid: verdict.pid,
                                image: None,
                                exit_code: None,
                                forced: false,
                                can_force: false,
                                conflict: false,
                            };
                        }
                        // Unknown never counts as stopped (SPEC §process.apps):
                        // keep polling, and name the unreadable table at the
                        // end instead of claiming the command won.
                        DownState::UnknownTable => unreadable = true,
                        DownState::Up => unreadable = false,
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Outcome {
                            action: "stop",
                            ok: false,
                            message: if unreadable {
                                format!("could not confirm the ports closed; the listener table cannot be read{command_note}; force is available")
                            } else {
                                format!("the stop command left it running; force is available{command_note}")
                            },
                            pid: verdict.pid,
                            image: None,
                            exit_code: None,
                            forced: false,
                            can_force: true,
                            conflict: false,
                        };
                    }
                    tokio::time::sleep(POLL_STEP).await;
                }
            }
        }

        // The hard kill (SPEC §process.apps): re-resolve the owners NOW -
        // the tracked pid is a hint, the port's owner is the fact - refuse the
        // gateway by pid and by image, kill the tree, report survivors. The
        // early scan above already refused the gateway case; the kill path
        // re-resolves anyway because the stop command had 30 s to change
        // everything.
        let targets = match self.kill_targets(app).await {
            Ok(targets) => targets,
            Err(sentence) => return Outcome::failed("stop", sentence),
        };
        if targets.is_empty() {
            // The ports closed between the verdict and here: done.
            self.drop_record(&app.name);
            return Outcome {
                action: "stop",
                ok: true,
                message: "already stopped".to_string(),
                pid: None,
                image: None,
                exit_code: None,
                forced: true,
                can_force: false,
                conflict: false,
            };
        }

        // The boundary the reader follows (SPEC §process.panel): a hard kill
        // is a stop too, and the capture must SHOW it as one. The stop-command
        // path writes its header above; a kill that wrote nothing left eleven
        // start headers and zero stops, as if nothing ever ended.
        let capture = self.capture_path(&app.name);
        if let Err(err) = logs::append_header(&capture, &swiss_core::log::iso_now(), "stop", by)
        {
            return Outcome::failed(
                "stop",
                format!("could not write {}: {err}", capture.display()),
            );
        }

        // The kill itself, with the exit lines suppressed for these pids: the
        // stop's outcome is the history line, not a second one per pid.
        {
            let mut suppress = self.suppress.lock().unwrap_or_else(|e| e.into_inner());
            for target in &targets {
                suppress.insert(target.pid);
            }
        }
        let mut survivors: Vec<u32> = Vec::new();
        for target in &targets {
            match self.kill_identified(target).await {
                Ok(pids) => survivors.extend(pids),
                Err(sentence) => {
                    self.clear_suppression(&targets);
                    let mut outcome = Outcome::failed("stop", sentence);
                    outcome.forced = true;
                    return outcome;
                }
            }
        }
        let deadline = tokio::time::Instant::now() + KILL_WINDOW;
        // Unknown keeps waiting - it proves nothing; the survivor probe below
        // decides the outcome from the processes themselves.
        while !matches!(self.down_state(app).await, DownState::Down)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(POLL_STEP).await;
        }
        // Survivors get their own short probe: gone counts as stopped.
        let probe_deadline = tokio::time::Instant::now() + SURVIVOR_PROBE;
        let mut still: Vec<u32> = Vec::new();
        loop {
            still.clear();
            for pid in &survivors {
                if self.deps.probe.process(*pid).await.is_some() {
                    still.push(*pid);
                }
            }
            if still.is_empty() || tokio::time::Instant::now() >= probe_deadline {
                break;
            }
            tokio::time::sleep(POLL_STEP).await;
        }
        self.clear_suppression(&targets);
        if !still.is_empty() {
            let list = still
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let mut outcome = Outcome::failed("stop", format!("pids {list} survived the tree kill"));
            outcome.pid = targets.first().map(|target| target.pid);
            outcome.image = targets.first().map(|target| target.image.clone());
            outcome.forced = true;
            return outcome;
        }
        self.drop_record(&app.name);
        Outcome {
            action: "stop",
            ok: true,
            message: format!("killed {}", targets.iter().map(|target| target.pid.to_string()).collect::<Vec<_>>().join(", ")),
            pid: targets.first().map(|target| target.pid),
            image: targets.first().map(|target| target.image.clone()),
            exit_code: None,
            forced: true,
            can_force: false,
            conflict: false,
        }
    }

    fn drop_record(&self, name: &str) {
        if self
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some()
        {
            self.save_tracked();
        }
    }

    /// One engine's end: the history line and the row's last-outcome say-so.
    async fn finish(&self, name: &str, by: &str, outcome: Outcome) -> Outcome {
        let at = self.deps.clock.now_ms();
        let _ = self.history.append(&HistoryEntry {
            ts: at,
            app: name.to_string(),
            action: outcome.action.to_string(),
            by: by.to_string(),
            ok: outcome.ok,
            message: outcome.message.clone(),
            pid: outcome.pid,
            image: outcome.image.clone(),
            exit_code: outcome.exit_code,
            forced: outcome.forced.then_some(true),
        });
        self.last.lock().unwrap_or_else(|e| e.into_inner()).insert(
            name.to_string(),
            LastOutcome {
                action: outcome.action.to_string(),
                ok: outcome.ok,
                message: outcome.message.clone(),
                at,
                exit_code: outcome.exit_code,
                can_force: outcome.can_force,
            },
        );
        outcome
    }

    /// A late exit of a started app: one history line, the row's exitCode.
    /// Called by the plugin's exit pump and by the tests; a pid a stop is
    /// killing is the stop's own line, not this one.
    pub fn handle_exit(&self, line: &ExitLine) {
        if self
            .suppress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&line.pid)
        {
            return;
        }
        // Only the process the record tracks as the app earns an exit line.
        // A stop command and a start-window launcher are DIFFERENT processes
        // whose deaths belong to their own actions, and their pids never
        // match the record; without this gate every stop command would add a
        // bogus ok:false "exit" line to the app it stopped (SPEC §process.apps
        // ties the watcher to the started app's process).
        {
            let record = self
                .record
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&line.app)
                .cloned();
            match record {
                Some(record) if record.pid == line.pid => {}
                _ => return,
            }
        }
        let at = self.deps.clock.now_ms();
        let _ = self.history.append(&HistoryEntry {
            ts: at,
            app: line.app.clone(),
            action: "exit".to_string(),
            by: "plugin".to_string(),
            ok: false,
            message: format!("exited with code {}", line.code),
            pid: Some(line.pid),
            image: None,
            exit_code: Some(line.code),
            forced: None,
        });
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = last.get_mut(&line.app) {
            entry.exit_code = Some(line.code);
            entry.message = format!("exited with code {}", line.code);
        } else {
            last.insert(
                line.app.clone(),
                LastOutcome {
                    action: "exit".to_string(),
                    ok: false,
                    message: format!("exited with code {}", line.code),
                    at,
                    exit_code: Some(line.code),
                    can_force: false,
                },
            );
        }
        // The watcher saw the tracked process end: the record goes with it
        // (SPEC §process.apps lists the watcher as a removal trigger; the
        // status-read sweep stays as the backup path).
        self.drop_record(&line.app);
    }

    /// The exit pump the plugin runs: one task draining the spawner's exit
    /// channel into history lines for as long as the plugin is up.
    pub async fn pump_exits(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<ExitLine>) {
        while let Some(line) = rx.recv().await {
            self.handle_exit(&line);
        }
    }

    /// Boot's autostart pass (SPEC §process.apps): daemons only, declared
    /// order, only stopped/unknown attempted, one retry 60 s later, and
    /// everything else recorded in the boot log alone - never as failed
    /// history lines on an app that was never started.
    pub async fn start_with_swiss(&self) -> Vec<(String, Outcome)> {
        self.start_with_swiss_until(None).await
    }

    /// The same pass with the plugin scope's stop signal (SPEC §process.apps):
    /// checked between apps and across the retry gap, never inside a start - a
    /// start abandoned mid-await could leave a spawned app with no tracked
    /// record. A plugin disabled mid-pass therefore stops STARTING apps while
    /// the ones already running keep the "disable is not a kill switch" rule.
    pub async fn start_with_swiss_until(
        &self,
        mut stop: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Vec<(String, Outcome)> {
        if !self.boot.is_daemon {
            return Vec::new();
        }
        let mut results = Vec::new();
        let mut retry: Vec<AppConfig> = Vec::new();
        for app in self.apps().iter().filter(|a| a.autostart) {
            if stop.as_ref().is_some_and(|rx| *rx.borrow()) {
                return results;
            }
            // The pre-filter: running, partial, conflict and an unreadable
            // listener table are recorded and left alone (SPEC §process.apps).
            // An Unknown whose reason is the unreadable table stays out too -
            // the reason sentence is what keeps the two unknowns apart.
            let verdict = self.snapshot(app).await;
            let attempt = match &verdict.status {
                AppStatus::Stopped => true,
                AppStatus::Unknown { reason } => !reason.contains("cannot be read"),
                _ => false,
            };
            if !attempt {
                let (word, _, _) = row_words(&verdict, &app.ports);
                let mut outcome = Outcome::failed(
                    "start",
                    if word == "unknown" {
                        "left alone at boot: the listener table cannot be read".to_string()
                    } else {
                        format!("left alone at boot: {word}")
                    },
                );
                outcome.conflict =
                    matches!(verdict.status, AppStatus::Conflict { .. });
                results.push((app.name.clone(), outcome));
                continue;
            }
            let first = self.run_start(app, "boot").await;
            if first.ok || first.conflict {
                results.push((app.name.clone(), first));
                continue;
            }
            retry.push(app.clone());
        }
        if retry.is_empty() {
            return results;
        }
        // One retry, 60 s later (SPEC §process.apps). start_paused tests
        // auto-advance the gap; the plugin runs this whole pass as a scoped
        // task, so the gateway never waits for it - and a disable landing in
        // the gap cancels the retry instead of starting apps under a plugin
        // whose routes already answer 503.
        if stop.as_ref().is_some_and(|rx| *rx.borrow()) {
            return results;
        }
        match stop.as_mut() {
            Some(rx) => {
                tokio::select! {
                    _ = tokio::time::sleep(AUTOSTART_RETRY) => {}
                    _ = rx.changed() => return results,
                }
            }
            None => tokio::time::sleep(AUTOSTART_RETRY).await,
        }
        for app in retry {
            if stop.as_ref().is_some_and(|rx| *rx.borrow()) {
                return results;
            }
            let second = self.run_start(&app, "boot").await;
            results.push((app.name.clone(), second));
        }
        results
    }

    async fn ports_state(&self, app: &AppConfig) -> PortsState {
        let listeners = match self.deps.probe.listeners().await {
            Ok(rows) => rows,
            // An unreadable table is unknown, never "all closed" (SPEC
            // §process.apps): the callers decide what unknown means to them.
            Err(_) => return PortsState::Unreadable,
        };
        let mut down = false;
        for port in &app.ports {
            match listeners.iter().find(|row| row.port == *port) {
                Some(row) => {
                    let fact = self.deps.probe.process(row.pid).await;
                    let mine = match &fact {
                        Some(fact) => app
                            .image
                            .as_deref()
                            .map(|declared| image_matches(declared, &fact.image))
                            .unwrap_or(false),
                        // pid 0 is another user's socket (§process.detached)
                        // and an unreadable owner is not provably ours: both
                        // are strangers, exactly as the verdict reads them -
                        // never "mine".
                        None => false,
                    };
                    if !mine {
                        let image = fact
                            .map(|f| f.image)
                            .unwrap_or_else(|| "another user".to_string());
                        return PortsState::Stranger(row.pid, image, *port);
                    }
                }
                None => down = true,
            }
        }
        if down {
            PortsState::Down
        } else {
            PortsState::AllUp
        }
    }

    /// The owner of the app's first declared port with its facts: the pin a
    /// launcher-pattern start leaves behind (the launcher is dead; whoever
    /// holds the port is the app).
    async fn port_owner(&self, app: &AppConfig) -> Option<(u32, OwnerFact)> {
        let listeners = self.deps.probe.listeners().await.ok()?;
        let port = *app.ports.first()?;
        let row = listeners.iter().find(|row| row.port == port)?;
        let fact = self.deps.probe.process(row.pid).await?;
        Some((row.pid, fact))
    }

    /// Candidate pids for the early gateway refusal only. These hints never
    /// authorize a kill; kill_targets validates fresh facts separately.
    async fn target_pids(&self, app: &AppConfig) -> Vec<u32> {
        let mut targets: Vec<u32> = Vec::new();
        if !app.ports.is_empty() {
            let listeners = self.deps.probe.listeners().await.unwrap_or_default();
            for port in &app.ports {
                // The same every-row rule as kill_targets: these are hints for the
                // early gateway refusal, and a second owner of a port is as much a
                // candidate as the first.
                for row in listeners.iter().filter(|row| row.port == *port) {
                    if !targets.contains(&row.pid) {
                        targets.push(row.pid);
                    }
                }
            }
        } else if let Some(record) = self
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&app.name)
            .cloned()
        {
            targets.push(record.pid);
        }
        targets
    }

    /// The pids a hard kill may act on, from fresh listener and identity reads.
    /// A missing fact or one foreign owner refuses the whole target set.
    async fn kill_targets(&self, app: &AppConfig) -> Result<Vec<KillTarget>, String> {
        let mut targets = Vec::new();
        if !app.ports.is_empty() {
            let listeners = self.deps.probe.listeners().await
                .map_err(|err| format!("the listener table cannot be read; not killed: {err}"))?;
            for port in &app.ports {
                // EVERY matching row, not the first: one numeric port can have two
                // owners (an IPv4 row and an IPv6 row), and killing half of them
                // reported success while the app kept serving on the survivor.
                for row in listeners.iter().filter(|row| row.port == *port) {
                    if targets.iter().any(|target: &KillTarget| target.pid == row.pid) {
                        continue;
                    }
                    let target = self.read_target(row.pid).await?;
                    if !app.image.as_deref().is_some_and(|image| image_matches(image, &target.image)) {
                        return Err(conflict_sentence(target.pid, &target.image, *port));
                    }
                    targets.push(target);
                }
            }
        } else {
            let record = self.record.lock().unwrap_or_else(|e| e.into_inner()).get(&app.name).cloned();
            if let Some(record) = record {
                let target = self.read_target(record.pid).await?;
                if target.created_at != record.created_at || !image_matches(&record.image, &target.image) {
                    return Err(format!("pid {} no longer matches its tracked creation time and image; not killed", record.pid));
                }
                targets.push(target);
            }
        }
        Ok(targets)
    }

    /// Read a complete identity and retain the gateway refusal at every kill
    /// boundary, not just at the earlier status snapshot.
    async fn read_target(&self, pid: u32) -> Result<KillTarget, String> {
        let fact = self.deps.probe.process(pid).await;
        if let Some(sentence) = self.gateway_identity_refusal(pid, fact.as_ref().map(|fact| fact.image.as_str())) {
            return Err(sentence);
        }
        let fact = fact.filter(|fact| fact.pid == pid && fact.created_at != 0 && !fact.image.is_empty())
            .ok_or_else(|| format!("pid {pid} identity cannot be read; not killed"))?;
        Ok(KillTarget { pid, created_at: fact.created_at, image: fact.image })
    }

    /// The only killer call: re-read the pin immediately before termination.
    /// Neither a row snapshot nor a command's pre-wait pin is kill authority.
    async fn kill_identified(&self, target: &KillTarget) -> Result<Vec<u32>, String> {
        let latest = self.read_target(target.pid).await?;
        if latest.created_at != target.created_at || !image_matches(&target.image, &latest.image) {
            return Err(format!("pid {} changed creation time or image; not killed", target.pid));
        }
        Ok(self.deps.killer.kill_tree(latest.pid).await)
    }

    fn clear_suppression(&self, targets: &[KillTarget]) {
        let mut suppress = self.suppress.lock().unwrap_or_else(|e| e.into_inner());
        for target in targets {
            suppress.remove(&target.pid);
        }
    }

    /// The early gateway refusal checks candidates, without authorizing a kill.
    async fn gateway_refusal(&self, targets: &[u32]) -> Option<String> {
        for pid in targets {
            if let Some(sentence) = self.gateway_identity_refusal(*pid, None) {
                return Some(sentence);
            }
            let fact = self.deps.probe.process(*pid).await;
            if let Some(sentence) = self.gateway_identity_refusal(*pid, fact.as_ref().map(|fact| fact.image.as_str())) {
                return Some(sentence);
            }
        }
        None
    }

    fn gateway_identity_refusal(&self, pid: u32, image: Option<&str>) -> Option<String> {
        if pid == self.gateway_pid {
            return Some(format!("refusing to kill pid {pid}: it is this gateway"));
        }
        if let Some(image) = image {
            if image_matches(&self.rules.gateway_image, image)
                || image.eq_ignore_ascii_case("swiss.exe")
                || image.eq_ignore_ascii_case("swiss")
            {
                return Some(format!("refusing to kill pid {pid}: its image {image} is the gateway's own"));
            }
        }
        None
    }

    /// The stop loops' confirm state. Down is proven down (every declared
    /// port closed, or the tracked pid gone); UnknownTable is a listener
    /// table that cannot be read, which proves nothing - unreadable is
    /// unknown, never stopped (SPEC §process.apps).
    async fn down_state(&self, app: &AppConfig) -> DownState {
        if !app.ports.is_empty() {
            let listeners = match self.deps.probe.listeners().await {
                Ok(rows) => rows,
                Err(_) => return DownState::UnknownTable,
            };
            let closed = app
                .ports
                .iter()
                .all(|port| !listeners.iter().any(|row| row.port == *port));
            return if closed { DownState::Down } else { DownState::Up };
        }
        let record = self
            .record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&app.name)
            .cloned();
        match record {
            Some(record) => {
                if self.deps.probe.process(record.pid).await.is_none() {
                    DownState::Down
                } else {
                    DownState::Up
                }
            }
            None => DownState::Down,
        }
    }
}

/// What a stop loop learns about "is it down": Down is proven, Up is proven
/// alive, UnknownTable proves nothing and never ends a stop early.
enum DownState {
    Down,
    Up,
    UnknownTable,
}

/// A read of the record file into its map shape.
fn read_record(path: &std::path::Path) -> Option<HashMap<String, TrackedRecord>> {
    let value = statefile::read_secure_json(path).ok()??;
    let obj = value.as_object()?;
    let mut map = HashMap::new();
    for (name, entry) in obj {
        let record = TrackedRecord {
            pid: entry.get("pid")?.as_u64()? as u32,
            created_at: entry.get("createdAt")?.as_u64()?,
            image: entry.get("image")?.as_str()?.to_string(),
        };
        map.insert(name.clone(), record);
    }
    Some(map)
}

enum PortsState {
    AllUp,
    Down,
    Stranger(u32, String, u16),
    /// The listener table could not be read: unknown, never "closed".
    Unreadable,
}

impl PartialEq for PortsState {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (PortsState::AllUp, PortsState::AllUp) | (PortsState::Down, PortsState::Down)
        )
    }
}

/// The synchronous refusal sentences for a start (SPEC §process.apps): what
/// is already there is named - pid, image, ports - never a value.
fn start_refusal(verdict: &Verdict) -> Option<String> {
    match &verdict.status {
        AppStatus::Running => Some(format!(
            "already running (pid {})",
            verdict.pid.unwrap_or(0)
        )),
        AppStatus::Partial { down_ports } => Some(format!(
            "only partly up; down: {}",
            down_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
        )),
        AppStatus::Conflict { pid, image, port } => Some(conflict_sentence(*pid, image, *port)),
        AppStatus::Stopped | AppStatus::Unknown { .. } => None,
    }
}

fn conflict_sentence(pid: u32, image: &str, port: u16) -> String {
    if pid == 0 {
        format!("port {port} is owned by another user")
    } else if image.is_empty() {
        format!("port {port} is held by pid {pid} (image unreadable)")
    } else {
        format!("port {port} is held by pid {pid} ({image})")
    }
}

/// The start window's failure when a declared port is bound by a process whose
/// image does not match (SPEC §process.apps): who holds it, and - when that
/// owner has a name - the image the config would need if it is this app (an
/// npm shim such as `dsh` runs as node.exe).
fn window_stranger_sentence(
    app: &AppConfig,
    pid: u32,
    launcher_exited: bool,
    other: u32,
    image: &str,
    port: u16,
) -> String {
    let lead = if launcher_exited {
        "launcher exited 0".to_string()
    } else {
        format!("started (pid {pid})")
    };
    let mut sentence = format!("{lead}, but {}", conflict_sentence(other, image, port));
    if other != 0 && !image.is_empty() {
        if let Some(declared) = app.image.as_deref() {
            sentence.push_str(&format!(
                ", not the image {declared} - if that is this app, set image to {image}"
            ));
        }
    }
    if !launcher_exited {
        sentence.push_str("; still running, not killed");
    }
    sentence
}

fn first_port(app: &AppConfig) -> u16 {
    app.ports.first().copied().unwrap_or(0)
}

fn port_list(app: &AppConfig) -> String {
    app.ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Non-blocking exit poll for the window's one-shot.
fn poll_exit(exits: &mut Option<tokio::sync::oneshot::Receiver<i32>>) -> Option<i32> {
    let rx = exits.as_mut()?;
    match rx.try_recv() {
        Ok(code) => {
            *exits = None;
            Some(code)
        }
        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
            *exits = None;
            None
        }
    }
}

fn row_words(verdict: &Verdict, ports: &[u16]) -> (String, Vec<u16>, String) {
    let _ = ports;
    match &verdict.status {
        AppStatus::Running => (
            "running".to_string(),
            Vec::new(),
            format!("running (pid {})", verdict.pid.unwrap_or(0)),
        ),
        AppStatus::Partial { down_ports } => (
            "partial".to_string(),
            down_ports.clone(),
            format!(
                "partly up; down: {}",
                down_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
            ),
        ),
        AppStatus::Stopped => ("stopped".to_string(), Vec::new(), "stopped".to_string()),
        AppStatus::Conflict { pid, image, port } => (
            "conflict".to_string(),
            Vec::new(),
            conflict_sentence(*pid, image, *port),
        ),
        AppStatus::Unknown { reason } => ("unknown".to_string(), Vec::new(), reason.clone()),
    }
}

#[cfg(test)]
mod tests;
