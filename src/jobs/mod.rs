//! Scheduled command jobs: run a configured command on a schedule, record every run, never
//! let one job's failure disturb anything else.
//!
//! A Rust-side subsystem with NO counterpart in the Node reference build - the deliberate,
//! documented kind of divergence (the panel is untouched, no existing /api shape changed). If
//! this ever grows a panel page, the right order is: build it in the Node repo first, port the
//! panel over, and keep this API shape as the contract.
//!
//! Pieces, each small enough to test alone:
//! - [JobDef]/[JobStore]: the job table, sealed into jobs.json like every other state file.
//!   ${ENV_VAR} refs inside a command stay refs on disk and expand at run time (runner.rs).
//! - [JobSystem]: the scheduler. ONE tokio task ticking once a second - the current_thread
//!   runtime must not grow a task per job for a feature that usually has two jobs. A run is
//!   claimed atomically (one run of a job at a time; an overlapping tick is skipped, not
//!   queued), SUBMITTED to the shared run coordinator with its deadline, appended to the job's
//!   run log, and summarised into the gateway log so 'lmg logs' shows it.
//! - schedule.rs: when a job is due (interval or 5-field cron, local time).
//! - runner.rs: what a run PRODUCED (the record type). How it runs is no longer here: the
//!   scheduler is one producer of the shared run coordinator (docs/10 §2), which executes the
//!   `process.legacy-command` capability over the shared process supervisor. That is what
//!   gives a job bounded output capture, a real cancel, and one global concurrency bound
//!   shared with manual runs - instead of a second execution path of its own.
//! - runlog.rs: what is remembered (logs/jobs/<name>.jsonl, byte- and age-capped).
//! - api.rs: the /api/jobs management surface (the panel does not know it exists).

pub mod api;
pub mod runlog;
pub mod runner;
pub mod schedule;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use chrono::{Datelike, Timelike};

use crate::log;
use crate::secure::statefile::{read_secure_json, write_secure_json};
use crate::services::runs::{SubmitError, SubmitRequest};
use crate::services::RuntimeServices;
use crate::util::now_ms;

/// The owner every scheduled run is registered under. Cancellation is scoped by this exact
/// string: stopping Jobs takes back the runs Jobs started and nothing else.
pub const JOBS_OWNER: &str = "jobs";
/// The capability a v1 job's `command` string executes as - the compatibility action that
/// keeps the old tokenizer and the old lenient `${VAR}` expansion (docs/10 §9 step 2).
pub const JOBS_ACTION: &str = "process.legacy-command";

/// One scheduled command, exactly as it is persisted (camelCase on the wire, like every state
/// file). Exactly one of every_sec / cron is set - enforced by [JobDef::validate].
#[derive(Debug, Clone)]
pub struct JobDef {
    pub name: String,
    pub command: String,
    pub every_sec: Option<u64>,
    pub cron: Option<String>,
    pub enabled: bool,
    pub timeout_ms: u64,
    pub cwd: Option<String>,
    /// When the job last STARTED (the interval anchor), if ever. Gateway-maintained.
    pub last_run_ms: Option<i64>,
    /// Whether that last run exited 0. Gateway-maintained.
    pub last_ok: Option<bool>,
}

impl JobDef {
    /// Validate a definition the same way at every entry point (store load drops, API refuses),
    /// so the scheduler never has to defend against half-formed jobs.
    pub fn validate(&self) -> Result<(), String> {
        if !runlog::valid_name(&self.name) {
            return Err(format!(
                "job name {:?} is not usable: 1-64 chars of letters, digits, . _ - only",
                self.name
            ));
        }
        if self.command.trim().is_empty() {
            return Err("command must not be empty".into());
        }
        if self.every_sec.is_some() == self.cron.is_some() {
            return Err("give exactly one of everySec or cron".into());
        }
        if let Some(secs) = self.every_sec {
            if secs == 0 {
                return Err("everySec must be at least 1".into());
            }
        }
        if let Some(expr) = &self.cron {
            schedule::CronExpr::parse(expr)?;
        }
        if self.timeout_ms == 0 {
            return Err("timeoutMs must be at least 1".into());
        }
        Ok(())
    }

    fn from_json(v: &Value) -> Result<Self, String> {
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "missing name".to_string())?;
        let command = v
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "missing command".to_string())?;
        let def = JobDef {
            name,
            command,
            every_sec: v.get("everySec").and_then(Value::as_u64),
            cron: v
                .get("cron")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            // Absent - or anything but literal false - means enabled: a new feature defaults ON,
            // and a hand-edited file with no flag must not silently disable every job.
            enabled: v.get("enabled") != Some(&Value::Bool(false)),
            timeout_ms: v
                .get("timeoutMs")
                .and_then(Value::as_u64)
                .unwrap_or(runner::DEFAULT_TIMEOUT_MS),
            cwd: v
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            last_run_ms: v
                .get("lastRunAt")
                .and_then(Value::as_str)
                .and_then(crate::util::parse_iso_ms),
            last_ok: v.get("lastOk").and_then(Value::as_bool),
        };
        def.validate()?;
        Ok(def)
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("command".into(), json!(self.command));
        if let Some(secs) = self.every_sec {
            m.insert("everySec".into(), json!(secs));
        }
        if let Some(expr) = &self.cron {
            m.insert("cron".into(), json!(expr));
        }
        m.insert("enabled".into(), json!(self.enabled));
        m.insert("timeoutMs".into(), json!(self.timeout_ms));
        if let Some(cwd) = &self.cwd {
            m.insert("cwd".into(), json!(cwd));
        }
        if let Some(ms) = self.last_run_ms {
            m.insert("lastRunAt".into(), json!(iso_of_ms(ms)));
        }
        if let Some(ok) = self.last_ok {
            m.insert("lastOk".into(), json!(ok));
        }
        Value::Object(m)
    }
}

/// RFC3339 UTC (millisecond precision, Z-suffixed) from a unix millisecond stamp - the same
/// shape log::iso_now produces, so a reader parses lastRunAt with the same util it parses every
/// other timestamp here. chrono rather than the time crate: from_timestamp_millis is right here
/// and chrono is already linked for the local-time cron work.
fn iso_of_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// The job table, sealed into jobs.json. Invalid entries are dropped with a warning at load -
/// one hand-mangled job must not cost the whole table (the managed-store rule).
pub struct JobStore {
    pub jobs: Vec<JobDef>,
    path: PathBuf,
}

impl JobStore {
    pub fn open(path: PathBuf) -> Self {
        let mut jobs = Vec::new();
        match read_secure_json(&path) {
            Ok(Some(Value::Object(root))) => {
                if let Some(Value::Array(arr)) = root.get("jobs") {
                    for entry in arr {
                        match JobDef::from_json(entry) {
                            Ok(def) => jobs.push(def),
                            Err(err) => log::warn(
                                "dropping an invalid job entry",
                                Some(json!({ "err": err, "entry": entry.to_string() })),
                            ),
                        }
                    }
                }
            }
            Ok(_) => {} // no file yet, or not an object: an empty table, not an error
            Err(err) => {
                log::warn(
                    "jobs.json load failed - starting with no jobs",
                    Some(json!({ "err": err, "path": path.display().to_string() })),
                );
            }
        }
        JobStore { jobs, path }
    }

    /// Seal the table to disk. The error is RETURNED, not just logged: a config write that
    /// could not persist must not answer the caller with a success (docs/10 §5). Callers on a
    /// RUN path deliberately degrade it to a warning instead — see their own comments.
    fn save(&self) -> Result<(), String> {
        write_secure_json(
            &self.path,
            &json!({ "jobs": self.jobs.iter().map(JobDef::to_json).collect::<Vec<_>>() }),
        )
        .map_err(|err| {
            log::warn(
                "jobs.json persist failed",
                Some(json!({ "err": err, "path": self.path.display().to_string() })),
            );
            format!("could not persist jobs.json: {err}")
        })
    }
}

/// Why a run could not start. Every variant is a REFUSAL the caller is told about: a manual
/// request never gets a success reply for work that did not happen (docs/10 §4, "队列满").
#[derive(Debug)]
pub enum RunError {
    Unknown(String),
    /// This job already has a run in flight - the overlap=skip gate.
    Busy(String),
    /// The shared pool is full. The bound is global: manual runs and other producers count.
    Capacity(String),
    /// The capability is not registered - the process plugin is disabled. A dependency that
    /// is not there is reported as missing, not silently skipped (docs/09 §9).
    Unavailable(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Unknown(m)
            | RunError::Busy(m)
            | RunError::Capacity(m)
            | RunError::Unavailable(m) => write!(f, "{m}"),
        }
    }
}

/// The scheduler plus its mutable state. Shared as one Arc between the boot sequence, the tick
/// task and the /api/jobs handlers - the same shape as Tunnels.
pub struct JobSystem {
    store: Mutex<JobStore>,
    /// The shared run services. The scheduler owns WHEN a job runs; the coordinator owns the
    /// run itself (bounds, cancellation, the terminal state) and the supervisor owns the
    /// child process. This subsystem spawns nothing of its own any more.
    services: Arc<RuntimeServices>,
    /// Per-job "a run is in flight" flag; absent means false. The claim gate. This is the
    /// per-job overlap policy (skip); the pool's global bound is the coordinator's.
    runtime: Mutex<HashMap<String, bool>>,
    /// Gateway boot time: the interval anchor for a job that has never run, so a fresh job
    /// fires one interval after boot instead of instantly.
    anchor_ms: i64,
    timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl JobSystem {
    pub fn open(path: PathBuf, services: Arc<RuntimeServices>) -> Arc<Self> {
        Arc::new(JobSystem {
            store: Mutex::new(JobStore::open(path)),
            services,
            runtime: Mutex::new(HashMap::new()),
            anchor_ms: now_ms() as i64,
            timer: Mutex::new(None),
        })
    }

    /// Start the one-second tick task. stop() aborts it.
    pub fn start(self: &Arc<Self>) {
        let sys = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                for name in sys.due_jobs_now() {
                    let sys = sys.clone();
                    tokio::spawn(async move {
                        // A Busy result here is the normal overlap skip; Unknown is a delete
                        // that won the race. Neither is worth a log line.
                        let _ = sys.execute(&name, "timer").await;
                    });
                }
            }
        });
        if let Ok(mut slot) = self.timer.lock() {
            *slot = Some(handle);
        }
    }

    /// Stop SCHEDULING: the tick task is aborted, so nothing new fires. Runs already in
    /// flight are the coordinator's, not the timer's - see [JobSystem::shutdown] for the
    /// teardown that actually takes them back.
    pub fn stop(&self) {
        if let Ok(mut slot) = self.timer.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }

    /// Stop scheduling AND take back what is running: every run this scheduler owns is
    /// cancelled and AWAITED through the shared coordinator, so when this returns no job
    /// command - and no child of one - is still alive. Returns how many were in flight.
    ///
    /// Runs of OTHER producers are untouched: the bound is by owner, so a manual run someone
    /// started by hand survives the jobs plugin being disabled.
    pub async fn shutdown(self: &Arc<Self>) -> usize {
        self.stop();
        let canceled = self.services.runs.cancel_owner(JOBS_OWNER).await;
        // Every run the coordinator held is terminal now, so the claim flags left behind
        // belong to execute() continuations that can no longer start anything. Clearing them
        // means a later enable finds no job wedged as "already running".
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        canceled
    }

    /// The jobs whose schedule says RUN right now. Snapshot under both locks; no awaits inside.
    pub fn due_jobs_now(&self) -> Vec<String> {
        let now = now_ms() as i64;
        let local = chrono::Local::now().naive_local();
        self.due_jobs(now, &local)
    }

    /// The pure half of the tick: given a clock, which jobs fire? Split from due_jobs_now so
    /// the decision is testable against synthetic times.
    pub fn due_jobs(&self, now: i64, now_local: &chrono::NaiveDateTime) -> Vec<String> {
        let running = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store
            .jobs
            .iter()
            .filter(|j| j.enabled && !running.get(&j.name).copied().unwrap_or(false))
            .filter(|j| job_due(j, now, now_local, self.anchor_ms))
            .map(|j| j.name.clone())
            .collect()
    }

    /// Atomically claim one job for a run: marks it running and stamps last_run_ms (the
    /// interval anchor moves to the run's START, so a long run does not queue an immediate
    /// re-run when it finishes). The anchor moves for the OCCURRENCE, not for a success: a
    /// refused occurrence still counts, or a job would retry every tick while the pool is
    /// full. The second claimant of a busy job gets Busy, not a queue.
    fn claim(&self, name: &str, now: i64) -> Result<JobDef, RunError> {
        let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let Some(def) = store.jobs.iter().find(|j| j.name == name).cloned() else {
            return Err(RunError::Unknown(format!("unknown job: {name}")));
        };
        if rt.get(name).copied().unwrap_or(false) {
            return Err(RunError::Busy(format!("job {name} is already running")));
        }
        rt.insert(name.to_string(), true);
        if let Some(j) = store.jobs.iter_mut().find(|j| j.name == name) {
            j.last_run_ms = Some(now);
        }
        // Best-effort on purpose: the run is about to happen either way, and refusing to run
        // because the ANCHOR could not be written would turn a disk problem into a missed job.
        // The warning is in save(); the cost of the failure is one possible re-fire after a
        // restart, which the cron same-minute rule already guards for cron jobs.
        let _ = store.save();
        Ok(def)
    }

    /// Run one job to completion (timer or manual): claim it, submit it to the shared
    /// coordinator with the job's own deadline, append the run record, persist lastOk, and
    /// leave one line in the gateway log. Every step that can fail degrades to a recorded
    /// field - a job's failure is data, never a panic or an abort of the scheduler. Returns
    /// the run log's sequence number with the outcome.
    ///
    /// A REFUSAL (no capability, full pool) is recorded as an occurrence too and then
    /// returned as an error: the history shows that the job was due and did not run, and the
    /// caller is never told a run happened that did not.
    pub async fn execute(
        self: Arc<Self>,
        name: &str,
        trigger: &str,
    ) -> Result<(u64, runner::RunOutcome), RunError> {
        let def = self.claim(name, now_ms() as i64)?;
        let submitted = self.services.runs.submit(SubmitRequest {
            owner: JOBS_OWNER.to_string(),
            label: def.name.clone(),
            action_type: JOBS_ACTION.to_string(),
            input: legacy_input(&def),
            timeout_ms: def.timeout_ms,
            // overlap=skip, this build's only policy: the per-job claim above already refused
            // a second run of THIS job, so a full pool is a refusal as well, not a queue.
            queue_if_busy: false,
        });
        let submitted = match submitted {
            Ok(submitted) => submitted,
            Err(err) => {
                let refusal = runner::RunOutcome::refused(err.to_string());
                let _ = self.record_run(name, trigger, &refusal);
                self.release(name);
                return Err(match err {
                    SubmitError::Capacity(m) => RunError::Capacity(m),
                    SubmitError::Action(m) => RunError::Unavailable(m),
                });
            }
        };
        let out = match submitted.done.await {
            Ok(view) => runner::RunOutcome::from_run_view(&view),
            // The coordinator dropped the sender: the run task was aborted under us (process
            // teardown). Record what is actually known - that it never reported.
            Err(_) => runner::RunOutcome::refused(
                "the run ended without reporting (gateway shutting down)".into(),
            ),
        };

        let seq = self.record_run(name, trigger, &out);
        self.persist_last_ok(name, out.ok);
        self.release(name);
        Ok((seq, out))
    }

    /// Append one run to the job's log and summarise it into the gateway log. Returns the
    /// sequence number, or 0 when the record could not be written (a warning, not a failure:
    /// the run itself already happened).
    fn record_run(&self, name: &str, trigger: &str, out: &runner::RunOutcome) -> u64 {
        let seq = match runlog::append_run(name, run_record(trigger, out)) {
            Ok(seq) => seq,
            Err(err) => {
                log::warn(
                    "job run could not be recorded",
                    Some(json!({ "name": name, "err": err })),
                );
                0
            }
        };
        let level = if out.ok { "info" } else { "warn" };
        let mut extra = json!({ "name": name, "trigger": trigger, "ok": out.ok, "ms": out.ms });
        if let Some(code) = out.exit_code {
            extra["exitCode"] = json!(code);
        }
        if out.timed_out {
            extra["timedOut"] = json!(true);
        }
        if out.canceled {
            extra["canceled"] = json!(true);
        }
        if let Some(err) = &out.error {
            extra["error"] = json!(err);
        }
        log::log(level, "job ran", Some(extra));
        seq
    }

    /// Persist the outcome of the last actual run. Skipped for a refusal: a job that could
    /// not start did not exit, and lastOk must not claim otherwise.
    fn persist_last_ok(&self, name: &str, ok: bool) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        // The job may have been deleted mid-run; the run record is already on disk, so there
        // is nothing to persist back for it.
        if let Some(j) = store.jobs.iter_mut().find(|j| j.name == name) {
            j.last_ok = Some(ok);
        }
        // Best-effort: the run already happened and its record is already in the run log. A
        // failed write here loses one lastOk flag, not the history.
        let _ = store.save();
    }

    /// Drop the per-job claim, whatever the run's outcome was.
    fn release(&self, name: &str) {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), false);
    }

    /// Insert or update a job (validated first). An update keeps the accumulated lastRunAt /
    /// lastOk - editing a schedule must not fabricate a "never ran" history.
    ///
    /// A persist failure is RETURNED. The in-memory table keeps the edit (the scheduler is
    /// already running it, and silently reverting would be its own lie), but the caller is
    /// told the save did not reach disk instead of being shown a success.
    pub fn upsert(&self, def: JobDef) -> Result<(), String> {
        def.validate()?;
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        match store.jobs.iter_mut().find(|j| j.name == def.name) {
            Some(existing) => {
                let last_run_ms = existing.last_run_ms;
                let last_ok = existing.last_ok;
                *existing = def;
                existing.last_run_ms = last_run_ms;
                existing.last_ok = last_ok;
            }
            None => store.jobs.push(def),
        }
        store.save()
    }

    /// Delete a job. Its run history stays on disk (the log outlives the job that wrote it,
    /// exactly like an MCP's call log).
    ///
    /// `Ok(false)` means there was no such job; `Err` means it was removed from the table but
    /// the new table did not reach disk, so the deletion will not survive a restart. Those are
    /// different answers and the API gives different ones.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let before = store.jobs.len();
        store.jobs.retain(|j| j.name != name);
        let removed = store.jobs.len() != before;
        if removed {
            store.save()?;
        }
        Ok(removed)
    }

    /// One job's API view: the definition plus the live facts (running, next due). Built for
    /// GET /api/jobs and the PUT reply, so both show the same shape.
    pub fn job_view(&self, def: &JobDef) -> Value {
        let running = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&def.name)
            .copied()
            .unwrap_or(false);
        let mut v = def.to_json();
        v["running"] = json!(running);
        if def.enabled {
            if let Some(next) = next_due_iso(def, now_ms() as i64, self.anchor_ms) {
                v["nextDueAt"] = json!(next);
            }
        }
        v
    }

    /// The listing behind GET /api/jobs.
    pub fn all_views(&self) -> Vec<Value> {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store.jobs.iter().map(|j| self.job_view(j)).collect()
    }
}

/// Test-only wiring: the shared services with the legacy command capability registered —
/// exactly what the process plugin's start does in a real boot, without a plugin host.
#[cfg(test)]
pub(crate) fn test_services() -> Arc<RuntimeServices> {
    let services = RuntimeServices::new();
    services
        .actions
        .register(Arc::new(crate::services::actions::LegacyCommandAction::new(
            services.supervisor.clone(),
        )))
        .expect("the legacy command capability registers once");
    services
}

/// A v1 job's action input: the command string exactly as saved (refs stay refs - they
/// resolve inside the capability, at run time) plus the optional working directory.
fn legacy_input(def: &JobDef) -> Value {
    let mut input = Map::new();
    input.insert("command".into(), json!(def.command));
    if let Some(cwd) = &def.cwd {
        input.insert("cwd".into(), json!(cwd));
    }
    Value::Object(input)
}

/// One run's JSONL record and API-reply shape - built in ONE place so the file line and the
/// POST /run response cannot drift apart. The seq is assigned (and inserted first) by
/// runlog::append_run; the API reply inserts it the same way.
pub fn run_record(trigger: &str, out: &runner::RunOutcome) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("at".into(), json!(log::iso_now()));
    rec.insert("trigger".into(), json!(trigger));
    rec.insert("ok".into(), json!(out.ok));
    rec.insert("ms".into(), json!(out.ms));
    if let Some(pid) = out.pid {
        rec.insert("pid".into(), json!(pid));
    }
    if let Some(code) = out.exit_code {
        rec.insert("exitCode".into(), json!(code));
    }
    if out.timed_out {
        rec.insert("timedOut".into(), json!(true));
    }
    if out.canceled {
        rec.insert("canceled".into(), json!(true));
    }
    if let Some(err) = &out.error {
        rec.insert("error".into(), json!(err));
    }
    if out.output_truncated {
        rec.insert("preview".into(), json!(true));
    }
    rec.insert("output".into(), json!(out.output));
    rec.insert("chars".into(), json!(out.chars));
    rec
}

/// Whether a job fires at "now". Interval: last run (or the boot anchor for a never-run job) plus
/// the interval has passed. Cron: this local minute matches AND the last run is not inside this
/// very minute - the second half is what makes a restart within the firing minute (03:30:05 ran,
/// gateway restarted, 03:30:59 ticks again) skip instead of double-firing.
pub fn job_due(def: &JobDef, now: i64, now_local: &chrono::NaiveDateTime, anchor_ms: i64) -> bool {
    if let Some(secs) = def.every_sec {
        let base = def.last_run_ms.unwrap_or(anchor_ms);
        return now.saturating_sub(base) >= (secs as i64) * 1000;
    }
    let Some(expr) = &def.cron else {
        return false;
    };
    let Ok(expr) = schedule::CronExpr::parse(expr) else {
        return false; // validation rejects this at every entry point; unreachable in practice
    };
    if !expr.matches(&schedule::fields_of(now_local)) {
        return false;
    }
    match def
        .last_run_ms
        .and_then(local_minute_of_ms)
        .map(|t| (t.year(), t.ordinal(), t.hour(), t.minute()))
    {
        Some(last) => {
            let cur = (
                now_local.year(),
                now_local.ordinal(),
                now_local.hour(),
                now_local.minute(),
            );
            last != cur
        }
        None => true,
    }
}

/// A unix-ms stamp as a local naive datetime (None when out of range).
fn local_minute_of_ms(ms: i64) -> Option<chrono::NaiveDateTime> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.naive_local())
}

/// The ISO stamp of a job's next firing - what a human scheduling "3 a.m. nightly" wants to
/// read back. Intervals anchor on lastRunAt (or boot); cron scans forward from now. Cron results
/// carry the LOCAL offset on purpose (chrono's to_rfc3339) - the field names a wall-clock time.
fn next_due_iso(def: &JobDef, now: i64, anchor_ms: i64) -> Option<String> {
    if let Some(secs) = def.every_sec {
        let base = def.last_run_ms.unwrap_or(anchor_ms);
        return Some(iso_of_ms(base + (secs as i64) * 1000));
    }
    let expr = schedule::CronExpr::parse(def.cron.as_deref()?).ok()?;
    let now_local = local_minute_of_ms(now)?;
    let next = expr.next_after(&now_local)?;
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&next)
        .single()
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn scratch_path(name: &str) -> PathBuf {
        crate::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!("lmg-jobs-{}-{}", name, crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("jobs.json")
    }

    /// Point the PROCESS-WIDE run log at a private directory, and hand back the lock that
    /// makes that safe. The guard is returned rather than dropped here on purpose: the log
    /// directory is global state, so a test that forgets to hold it would race every other
    /// test that writes a run (docs/10 §8 calls this debt out by name).
    async fn runlog_scratch() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = runlog::RUNLOG_TEST_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("lmg-jobs-log-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("runlog dir");
        runlog::set_run_log_dir(dir);
        guard
    }

    fn interval_job(name: &str, every_sec: u64) -> JobDef {
        JobDef {
            name: name.into(),
            command: if cfg!(windows) {
                "cmd /c echo hi".into()
            } else {
                "sh -c 'echo hi'".into()
            },
            every_sec: Some(every_sec),
            cron: None,
            enabled: true,
            timeout_ms: 30_000,
            cwd: None,
            last_run_ms: None,
            last_ok: None,
        }
    }

    fn cron_job(name: &str, expr: &str) -> JobDef {
        JobDef {
            every_sec: None,
            cron: Some(expr.into()),
            ..interval_job(name, 60)
        }
    }

    #[test]
    fn definitions_validate_at_every_entry_point() {
        assert!(interval_job("ok", 60).validate().is_ok());
        assert!(cron_job("ok2", "30 3 * * *").validate().is_ok());
        // bad names
        assert!(interval_job("not/ok", 60).validate().is_err());
        assert!(interval_job("", 60).validate().is_err());
        // empty command
        let mut j = interval_job("ok", 60);
        j.command = "   ".into();
        assert!(j.validate().is_err());
        // both schedules at once, or neither
        let mut j = interval_job("ok", 60);
        j.cron = Some("* * * * *".into());
        assert!(j.validate().is_err());
        let mut j = interval_job("ok", 60);
        j.every_sec = None;
        assert!(j.validate().is_err());
        // zero interval / timeout
        assert!(interval_job("ok", 0).validate().is_err());
        let mut j = interval_job("ok", 60);
        j.timeout_ms = 0;
        assert!(j.validate().is_err());
        // unparsable cron
        assert!(cron_job("ok", "99 * * * *").validate().is_err());
    }

    #[test]
    fn the_store_seals_round_trip_and_drops_invalid_entries() {
        let path = scratch_path("store");
        {
            let mut store = JobStore::open(path.clone());
            store.jobs.push(interval_job("keep-me", 300));
            store.jobs.push({
                let mut bad = interval_job("bad-cron", 0); // zero interval: invalid
                bad.cron = Some("* * * * *".into()); // and doubly-scheduled
                bad
            });
            store.save().expect("the scratch file is writable");
        }
        let reloaded = JobStore::open(path);
        assert_eq!(reloaded.jobs.len(), 1, "only the valid entry survives");
        assert_eq!(reloaded.jobs[0].name, "keep-me");
        // Defaults ride through: enabled by default, the runner's default timeout preserved.
        assert_eq!(reloaded.jobs[0].timeout_ms, 30_000);
    }

    #[test]
    fn absent_enabled_means_true() {
        let v = json!({
            "name": "handwritten",
            "command": "echo hi",
            "everySec": 60,
        });
        let def = JobDef::from_json(&v).expect("a valid def");
        assert!(def.enabled, "a hand-edited file with no flag must not disable the job");
        assert_eq!(def.timeout_ms, runner::DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn interval_due_anchors_on_last_run_or_boot() {
        let now = 1_000_000_000_i64 * 1000;
        let local = chrono::Local.timestamp_millis_opt(now).unwrap().naive_local();
        let anchor = now - 5000;
        // Never ran: due once the interval since boot has passed.
        let j = interval_job("j", 10); // 10s
        assert!(!job_due(&j, anchor + 9000, &local, anchor));
        assert!(job_due(&j, anchor + 10_000, &local, anchor));
        // Ran 4s ago on a 10s interval: not yet; ran 11s ago: due.
        let j = JobDef { last_run_ms: Some(now - 4000), ..j };
        assert!(!job_due(&j, now, &local, anchor));
        let j = JobDef { last_run_ms: Some(now - 11_000), ..j };
        assert!(job_due(&j, now, &local, anchor));
    }

    #[test]
    fn cron_due_requires_a_matching_minute_and_not_the_same_one_twice() {
        // 03:30 local, some date.
        let now_local: chrono::NaiveDateTime =
            chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap().and_hms_opt(3, 30, 5).unwrap();
        let j = cron_job("j", "30 3 * * *");
        assert!(job_due(&j, 0, &now_local, 0), "matching minute, never ran");
        // last run inside THIS minute (a restart at :05 after a run at :02): skip.
        let same_minute_ms = chrono::Local
            .from_local_datetime(&now_local)
            .unwrap()
            .timestamp_millis();
        let j = JobDef { last_run_ms: Some(same_minute_ms), ..j };
        assert!(!job_due(&j, 0, &now_local, 0));
        // last run was yesterday: due again.
        let yesterday = same_minute_ms - 24 * 60 * 60 * 1000;
        let j = JobDef { last_run_ms: Some(yesterday), ..j };
        assert!(job_due(&j, 0, &now_local, 0));
        // A non-matching minute is never due.
        let off = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap().and_hms_opt(3, 31, 0).unwrap();
        assert!(!job_due(&j, 0, &off, 0));
    }

    #[tokio::test]
    async fn a_manual_run_claims_executes_records_and_persists() {
        let _log = runlog_scratch().await;
        let path = scratch_path("manual");
        let sys = JobSystem::open(path.clone(), test_services());
        sys.upsert(interval_job("echo-job", 3600)).expect("valid");

        let (_seq, out) = sys.clone().execute("echo-job", "manual").await.expect("runs");
        assert!(out.ok, "{out:?}");

        // The claim is released: a second run goes through.
        assert!(sys.clone().execute("echo-job", "manual").await.is_ok());
        // The run log holds both runs, newest first.
        let (page, _) = runlog::read_page("echo-job", None, 10);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["trigger"], json!("manual"));
        // The outcome survived to disk.
        let reloaded = JobStore::open(path);
        let saved = reloaded.jobs.iter().find(|j| j.name == "echo-job").unwrap();
        assert_eq!(saved.last_ok, Some(true));
        assert!(saved.last_run_ms.is_some(), "the claim stamped lastRunAt");
    }

    #[tokio::test]
    async fn an_unknown_job_is_unknown_and_a_claimed_job_is_busy() {
        let _log = runlog_scratch().await;
        let path = scratch_path("claims");
        let sys = JobSystem::open(path, test_services());
        match sys.clone().execute("ghost", "manual").await {
            Err(RunError::Unknown(msg)) => assert!(msg.contains("ghost"), "{msg}"),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Claim directly, then confirm execute refuses to double-start.
        sys.upsert(interval_job("busy-job", 3600)).expect("valid");
        let _def = sys.claim("busy-job", now_ms() as i64).expect("first claim wins");
        match sys.clone().execute("busy-job", "manual").await {
            Err(RunError::Busy(msg)) => assert!(msg.contains("already running"), "{msg}"),
            other => panic!("expected Busy, got {other:?}"),
        }
        // And a claimed job is not reported due by the tick.
        assert!(!sys.due_jobs_now().contains(&"busy-job".to_string()));
    }

    /// A command that outlives any test, spelled per platform.
    fn long_command() -> String {
        if cfg!(windows) {
            "ping -n 60 127.0.0.1".into()
        } else {
            "sleep 60".into()
        }
    }

    /// Poll until the scheduler reports the job as running, so a test races the child rather
    /// than the submit. Returns false if it never got there.
    async fn wait_running(sys: &Arc<JobSystem>, name: &str) -> bool {
        for _ in 0..300 {
            let running = sys
                .all_views()
                .into_iter()
                .any(|v| v["name"] == json!(name) && v["running"] == json!(true));
            if running {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    /// The process plugin is disabled: the capability is simply not registered. A job that
    /// comes due must SAY so — a missing dependency is reported, never silently skipped.
    #[tokio::test]
    async fn a_missing_capability_is_reported_and_recorded() {
        let _log = runlog_scratch().await;
        let path = scratch_path("nocap");
        let sys = JobSystem::open(path.clone(), RuntimeServices::new());
        sys.upsert(interval_job("orphan", 3600)).expect("valid");

        match sys.clone().execute("orphan", "timer").await {
            Err(RunError::Unavailable(msg)) => assert!(msg.contains(JOBS_ACTION), "{msg}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }

        // The occurrence is in the history: the job was due and did not run.
        let (page, _) = runlog::read_page("orphan", None, 10);
        assert_eq!(page.len(), 1, "{page:?}");
        assert_eq!(page[0]["ok"], json!(false));
        assert!(page[0]["error"].as_str().unwrap_or_default().contains(JOBS_ACTION));

        // The claim was released, and lastOk was NOT written: nothing exited.
        let view = sys.all_views().remove(0);
        assert_eq!(view["running"], json!(false));
        let reloaded = JobStore::open(path);
        assert_eq!(reloaded.jobs[0].last_ok, None, "a refusal is not an outcome");
        assert!(
            reloaded.jobs[0].last_run_ms.is_some(),
            "the occurrence anchor still moved, so a full pool cannot make a job retry every tick"
        );
    }

    /// The pool's bound is GLOBAL: a manual run of another producer can fill it, and the
    /// scheduled occurrence is refused visibly rather than queued out of sight.
    #[tokio::test]
    async fn a_full_pool_refuses_the_occurrence_visibly() {
        let _log = runlog_scratch().await;
        let path = scratch_path("capacity");
        let services = test_services();
        services.runs.set_capacity(crate::services::runs::RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let sys = JobSystem::open(path, services.clone());
        sys.upsert(interval_job("waiting", 3600)).expect("valid");

        let hog = services
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "hog".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": long_command() }),
                timeout_ms: 30_000,
                queue_if_busy: false,
            })
            .expect("the only slot");

        match sys.clone().execute("waiting", "timer").await {
            Err(RunError::Capacity(msg)) => assert!(msg.contains("maxConcurrentRuns"), "{msg}"),
            other => panic!("expected Capacity, got {other:?}"),
        }
        let (page, _) = runlog::read_page("waiting", None, 10);
        assert_eq!(page.len(), 1, "the refused occurrence is recorded: {page:?}");

        // The other producer's run is untouched by the refusal, and reaped here.
        assert_eq!(services.runs.shutdown_all().await, 1);
        assert!(hog.done.await.is_ok(), "the hog reported its own terminal view");
    }

    /// The job's own timeoutMs is the deadline, enforced by cancellation: the child is killed
    /// and the record says WHICH kind of not-ok it was.
    #[tokio::test]
    async fn a_wedged_command_is_killed_at_the_job_deadline() {
        let _log = runlog_scratch().await;
        let path = scratch_path("deadline");
        let sys = JobSystem::open(path, test_services());
        let mut def = interval_job("wedged", 3600);
        def.command = long_command();
        def.timeout_ms = 400;
        sys.upsert(def).expect("valid");

        let started = std::time::Instant::now();
        let (_seq, out) = sys.clone().execute("wedged", "timer").await.expect("reports");
        assert!(out.timed_out, "{out:?}");
        assert!(!out.ok);
        assert!(!out.canceled, "the deadline owns this outcome, not a cancel");
        assert!(started.elapsed().as_secs() < 20, "it did not wait out the command");

        let (page, _) = runlog::read_page("wedged", None, 5);
        assert_eq!(page[0]["timedOut"], json!(true), "{page:?}");
        assert_eq!(page[0]["ok"], json!(false));
    }

    /// Disabling the jobs plugin means what it says: the runs this scheduler owns are
    /// cancelled AND awaited, and the per-job claims they held are gone.
    #[tokio::test]
    async fn shutdown_cancels_the_runs_this_scheduler_owns() {
        let _log = runlog_scratch().await;
        let path = scratch_path("shutdown");
        let services = test_services();
        let sys = JobSystem::open(path, services.clone());
        let mut def = interval_job("slow", 3600);
        def.command = long_command();
        sys.upsert(def).expect("valid");

        let running = {
            let sys = sys.clone();
            tokio::spawn(async move { sys.execute("slow", "timer").await })
        };
        assert!(wait_running(&sys, "slow").await, "the run reached the pool");

        assert_eq!(sys.shutdown().await, 1, "one in-flight run was taken back");
        let (_seq, out) = running.await.expect("the run task").expect("reports");
        assert!(out.canceled, "{out:?}");
        assert!(!out.ok);
        assert!(!out.timed_out, "nobody waited too long; it was cancelled");

        // No claim survives a shutdown, so enabling again does not find a wedged job.
        assert_eq!(sys.all_views().remove(0)["running"], json!(false));
    }

    /// docs/10 §5 asks for this by name: a persist that failed must not be reported as a
    /// save. The store keeps the edit in memory (the scheduler is already running it, and
    /// silently reverting would be a second lie), but the caller hears the truth.
    #[test]
    fn a_config_write_that_cannot_persist_is_an_error_not_a_warning() {
        crate::secure::key::use_test_master_key();
        // A path under a directory that does not exist: every write to it fails, on every
        // platform, without needing permissions a test cannot portably set.
        let path = std::env::temp_dir()
            .join(format!("lmg-jobs-gone-{}", crate::util::random_hex(8)))
            .join("nested")
            .join("jobs.json");
        let sys = JobSystem::open(path, test_services());

        let err = sys
            .upsert(interval_job("doomed", 60))
            .expect_err("the write cannot succeed");
        assert!(err.contains("could not persist"), "{err}");
        assert_eq!(sys.all_views().len(), 1, "the edit is live even though the save failed");

        let err = sys
            .delete("doomed")
            .expect_err("the removal cannot reach disk either");
        assert!(err.contains("could not persist"), "{err}");
        // Deleting something that was never there is not a failed write; it is a miss.
        assert_eq!(sys.delete("never-existed"), Ok(false));
    }

    #[test]
    fn upsert_preserves_history_and_delete_removes_only_the_job() {
        let path = scratch_path("upsert");
        let sys = JobSystem::open(path, test_services());
        sys.upsert(interval_job("edit-me", 60)).expect("valid");
        {
            let mut store = sys.store.lock().unwrap();
            store.jobs[0].last_ok = Some(false);
            store.jobs[0].last_run_ms = Some(123);
        }
        sys.upsert(interval_job("edit-me", 120)).expect("valid edit");
        {
            let store = sys.store.lock().unwrap();
            assert_eq!(store.jobs[0].every_sec, Some(120));
            assert_eq!(store.jobs[0].last_ok, Some(false), "history is kept");
            assert_eq!(store.jobs[0].last_run_ms, Some(123));
        }
        assert_eq!(sys.delete("edit-me"), Ok(true));
        assert_eq!(sys.delete("edit-me"), Ok(false), "already gone");
        assert!(sys.all_views().is_empty());
    }

    #[test]
    fn a_job_view_reports_running_and_a_next_due() {
        let path = scratch_path("view");
        let sys = JobSystem::open(path, test_services());
        sys.upsert(interval_job("viewed", 60)).expect("valid");
        sys.upsert(cron_job("nightly", "30 3 * * *")).expect("valid");
        let views: Vec<Value> = sys.all_views();
        assert_eq!(views.len(), 2);
        for v in &views {
            assert_eq!(v["running"], json!(false));
            assert!(
                v.get("nextDueAt").and_then(Value::as_str).is_some(),
                "every enabled job shows its next firing: {v}"
            );
        }
        // A disabled job shows no nextDueAt - nothing is scheduled.
        let mut disabled = interval_job("off", 60);
        disabled.enabled = false;
        sys.upsert(disabled).expect("valid");
        let v = sys
            .all_views()
            .into_iter()
            .find(|v| v["name"] == json!("off"))
            .unwrap();
        assert!(v.get("nextDueAt").is_none());
    }
}
