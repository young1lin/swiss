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
//! - runlog.rs: what is remembered (logs/jobs/<name>.jsonl, byte- and age-capped) -
//!   an INSTANCE the system owns, never a process-global directory.
//! - state.rs: run FACTS (docs/11 §4) - the interval anchor, lastOk, the failure
//!   streak - sealed into jobs-state.json, one line per job, best-effort writes.
//! - api.rs: the /api/jobs management surface (the panel does not know it exists).
//! - def.rs: the v2 definition model (docs/11) - the scheduler's source of definitions
//!   since S3, applied from the plugin config row.
//! - migrate.rs: the one-time v1 jobs.json → config row migration (docs/11 §5, S4).

pub mod api;
pub mod def;
pub mod migrate;
pub mod runlog;
pub mod runner;
pub mod schedule;
pub mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::config_store::ConfigStore;
use crate::log;
use crate::secure::statefile::{read_secure_json, write_secure_json};
use crate::services::runs::{SubmitError, SubmitRequest};
use crate::services::RuntimeServices;
use crate::util::now_ms;

use def::JobDefinition;

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
            // lastRunAt / lastOk in an old jobs.json are READ PAST here on purpose: run
            // state moved to jobs-state.json (docs/11 §4), and the S4 migration reads
            // those v1 keys deliberately when it moves them. For loading they are simply
            // not definition fields any more.
        };
        def.validate()?;
        Ok(def)
    }

    #[allow(dead_code)] // pairs with JobStore::save: read back by S4's migration tests
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
        // Run facts (lastRunAt / lastOk) are NOT written here any more: a definition file
        // holds intent, the state file holds facts (docs/11 §4). Every run used to
        // rewrite jobs.json just to move the anchor; now jobs.json only changes when a
        // definition changes.
        Value::Object(m)
    }
}

/// The v1 job table, sealed into jobs.json. From S3 on the scheduler never consults it -
/// definitions live in the plugin config row (docs/11 §9 S3) - and the whole type exists
/// as the READ-ONLY migration input S4 consumes. Its write side is kept (unused until
/// then) because the migration's tests round-trip it.
#[allow(dead_code)]
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
    /// could not persist must not answer the caller with a success (docs/10 §5).
    #[allow(dead_code)] // the S4 migration and its tests; the S3 scheduler never writes
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
    /// The APPLIED definitions row (docs/11 §9 S3): parsed from plugins.jobs.config by
    /// [JobSystem::apply_config], which is the one writer - boot, plugin reconcile and the
    /// v1 API edit path all go through it, so capacity, retention and the table itself
    /// can never fall out of step.
    config: Mutex<def::JobsConfig>,
    /// The config revision the applied row came from. `/api/jobs` reports it so a client
    /// can tell a stale listing from a fresh one (docs/11 §7.1).
    applied_revision: AtomicU64,
    /// The gateway's config store: the v1 API's edit path writes the SAME row the plugin
    /// host reconciles, revision-checked, instead of growing a second definition file.
    config_store: Arc<ConfigStore>,
    /// The v1 jobs.json path. From S3 on the scheduler never consults the file; the
    /// path is kept for the S4 migration (docs/11 §5), which reads it once and
    /// rewrites it with its completion marker.
    jobs_path: PathBuf,
    /// Run FACTS, sealed into jobs-state.json (docs/11 §4): the interval anchor, lastOk,
    /// the failure streak. Definitions and facts stopped sharing a file in S2.
    state: state::JobsState,
    /// The per-job run history (logs/jobs), owned - no process-global directory any more.
    runlog: runlog::RunLog,
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
    pub fn open(
        path: PathBuf,
        services: Arc<RuntimeServices>,
        config_store: Arc<ConfigStore>,
    ) -> Arc<Self> {
        // The siblings of jobs.json place the other two artifacts (docs/11 §4): run state
        // sits NEXT TO the definitions file, the per-job logs under logs/jobs beside it.
        // One argument keeps every caller colocating the family - and gives each
        // JobSystem its own private tree, which is the point of this stage.
        let dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        Arc::new(JobSystem {
            config: Mutex::new(def::JobsConfig {
                max_concurrent_runs: def::DEFAULT_MAX_CONCURRENT_RUNS,
                max_queued_runs: def::DEFAULT_MAX_QUEUED_RUNS,
                retention: def::Retention::default(),
                definitions: Vec::new(),
            }),
            applied_revision: AtomicU64::new(0),
            config_store,
            jobs_path: path,
            state: state::JobsState::open(dir.join("jobs-state.json")),
            runlog: runlog::RunLog::at(dir.join("logs").join("jobs")),
            services,
            runtime: Mutex::new(HashMap::new()),
            anchor_ms: now_ms() as i64,
            timer: Mutex::new(None),
        })
    }

    /// Apply a `plugins.jobs.config` row IN PLACE (docs/11 §8): swap the definitions,
    /// push the new capacity to the shared coordinator (a lower bound does not touch runs
    /// already executing) and re-budget the run log. Called at boot (plugin create) and
    /// on every config apply - plugin reconcile or the v1 edit path, both of which land
    /// here so there is exactly one interpretation of the row.
    ///
    /// The boot leniency applies here too: a leftover entry an older validator let in is
    /// dropped with a warn, not allowed to fail the whole table (docs/11 §9 S1). An
    /// unregistered action type only warns - the provider may be disabled, and its jobs
    /// report `actionAvailable: false` until it returns.
    pub fn apply_config(&self, config: &Value) -> Result<(), String> {
        let (parsed, _warnings) =
            def::JobsConfig::parse_boot_with_actions(config, &self.services.actions)
                .map_err(|err| err.to_string())?;
        for (id, reason) in &parsed.dropped {
            log::warn(
                "ignoring a config job definition saved by an older validator",
                Some(json!({ "id": id, "reason": reason })),
            );
        }
        let revision = self.config_store.snapshot().revision;
        let (max_concurrent, max_queued, retention) = {
            let mut cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
            *cfg = parsed.config;
            (
                cfg.max_concurrent_runs,
                cfg.max_queued_runs,
                cfg.retention.clone(),
            )
        };
        self.applied_revision.store(revision, Ordering::SeqCst);
        self.services
            .runs
            .set_capacity(crate::services::runs::RunCapacity {
                max_concurrent,
                max_queued,
            });
        self.runlog
            .set_limits(runlog::limits_of_retention(&retention));
        Ok(())
    }

    /// The registry-backed action lookup behind `actionAvailable` (docs/11 §7.1).
    pub fn actions(&self) -> &Arc<crate::services::action::ActionRegistry> {
        &self.services.actions
    }

    /// The config revision currently applied - what /api/jobs reports as configRevision.
    pub fn applied_revision(&self) -> u64 {
        self.applied_revision.load(Ordering::SeqCst)
    }

    /// The jobs config row as the STORE holds it right now - the row apply_config
    /// reads at boot after the S4 migration may have merged v1 definitions into it
    /// (docs/11 §5.1). `{}` when no row exists yet.
    pub fn current_config(&self) -> Value {
        self.config_store.plugin_config("jobs")
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
    /// the decision is testable against synthetic times. Reads the APPLIED config table
    /// (docs/11 §9 S3) - jobs.json no longer feeds the scheduler.
    pub fn due_jobs(&self, now: i64, now_local: &chrono::NaiveDateTime) -> Vec<String> {
        let running = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.definitions
            .iter()
            .filter(|d| !d.disabled && !running.get(d.id.as_str()).copied().unwrap_or(false))
            .filter(|d| {
                def::definition_due(
                    d,
                    now,
                    now_local,
                    self.anchor_ms,
                    self.state.get(&d.id).last_run_at,
                )
            })
            .map(|d| d.id.clone())
            .collect()
    }

    /// Atomically claim one job for a run: marks it running and stamps the anchor (the
    /// interval anchor moves to the run's START, so a long run does not queue an immediate
    /// re-run when it finishes). The anchor moves for the OCCURRENCE, not for a success: a
    /// refused occurrence still counts, or a job would retry every tick while the pool is
    /// full. The second claimant of a busy job gets Busy, not a queue.
    ///
    /// The definition is CLONED here on purpose: an apply_config that rewrites the table
    /// mid-flight cannot reach into a run that already claimed its snapshot (docs/11 §8,
    /// "修改中任务") - the run finishes under the definition it started with.
    fn claim(&self, name: &str, now: i64) -> Result<JobDefinition, RunError> {
        let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let Some(def) = cfg.definitions.iter().find(|d| d.id == name).cloned() else {
            return Err(RunError::Unknown(format!("unknown job: {name}")));
        };
        if rt.get(name).copied().unwrap_or(false) {
            return Err(RunError::Busy(format!("job {name} is already running")));
        }
        rt.insert(name.to_string(), true);
        // Best-effort on purpose: the run is about to happen either way, and refusing to run
        // because the ANCHOR could not be written would turn a disk problem into a missed job.
        // The cost of a failed write is one possible re-fire after a restart, which the cron
        // same-minute rule already guards for cron jobs - see state.rs for the full trade.
        self.state.claim(name, now, None);
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
            label: def.id.clone(),
            // The definition's OWN action ref (docs/11 §3.4): any registered capability,
            // input verbatim - env refs stay refs and resolve inside the action.
            action_type: def.action.type_.clone(),
            input: def.action.input.clone(),
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
        let seq = match self.runlog.append_run(name, run_record(trigger, out)) {
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
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        // The job may have been deleted mid-run; the run record is already on disk, and a
        // deleted definition owns no state line (delete forgets it) - settle would only
        // resurrect a line for a job that no longer exists.
        if !cfg.definitions.iter().any(|d| d.id == name) {
            return;
        }
        // Best-effort: the run already happened and its record is already in the run log. A
        // failed write here loses one lastOk flag, not the history. The coordinator's run id
        // rides along from S5, when execute() can carry it back.
        self.state.settle(name, ok, None);
    }

    /// Drop the per-job claim, whatever the run's outcome was.
    fn release(&self, name: &str) {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), false);
    }

    /// The v1 edit path (docs/11 §7.1): fold a v1-shaped body into the definitions row,
    /// through the SAME revision-checked config write the plugin host reconciles from. A
    /// definition the v1 shape cannot spell without loss (`editableInV1: false`) is
    /// refused with [WriteError::NotEditableV1] - overwriting it would silently drop
    /// fields, and the honest door is PUT /api/plugins/jobs/config.
    ///
    /// `def` is the v1-shaped JobDef the API layer shaped from the body; the id is its
    /// name. A NEW id is created through the v1->v2 migration mapping (legacy command
    /// action, exactly what the v1 row meant); an EXISTING editable definition is edited
    /// in place, keeping every v2 field the v1 shape does not know.
    pub fn upsert_v1(&self, def: JobDef) -> Result<(), WriteError> {
        def.validate().map_err(WriteError::Invalid)?;
        let mut row = self.config_store.plugin_config("jobs");
        let defs = definitions_map_of(&mut row)?;
        let edited = match defs.get(def.name.as_str()) {
            Some(existing) => {
                // Parse under the definition's OWN id: title == id is one of the
                // editable-in-v1 conditions, and a placeholder key would break it.
                let one =
                    def::JobsConfig::parse(&json!({ "definitions": { &def.name: existing } }))
                        .map_err(|e| WriteError::Invalid(e.to_string()))?;
                let current = one
                    .definitions
                    .into_iter()
                    .next()
                    .expect("one definition went in, one comes out");
                if !current.editable_in_v1() {
                    return Err(WriteError::NotEditableV1(def.name.clone()));
                }
                apply_v1_edit(current, &def)
            }
            None => def::JobDefinition::from_v1(&def),
        };
        defs.insert(def.name.clone(), definition_to_config(&edited));
        self.commit_row(row).map_err(WriteError::Persist)
    }

    /// Delete a definition (docs/11 §7.1/§8): the row loses the id, the run history
    /// stays on disk, and a run already in flight keeps its claimed snapshot to the end.
    /// `Ok(false)` means there was no such id.
    pub fn delete(&self, name: &str) -> Result<bool, WriteError> {
        let mut row = self.config_store.plugin_config("jobs");
        let defs = definitions_map_of(&mut row)?;
        if defs.shift_remove(name).is_none() {
            return Ok(false);
        }
        self.commit_row(row).map_err(WriteError::Persist)?;
        // The deleted definition's scheduling facts go with it (docs/11 §4); the run
        // HISTORY stays in the log file, which outlives the job that wrote it. Best-effort
        // like every state write on a non-config path.
        self.state.forget(name);
        Ok(true)
    }

    /// Validate a full row strictly (the v1 path shares the plugin row's rules) and land
    /// it: revision-checked store write, then [JobSystem::apply_config] so the running
    /// table, capacity and retention move in one step.
    fn commit_row(&self, row: Value) -> Result<(), String> {
        def::JobsConfig::parse_with_actions(&row, &self.services.actions)
            .map_err(|e| e.to_string())?;
        let expected = self.config_store.snapshot().revision;
        self.config_store
            .update_plugin("jobs", expected, row.clone())
            .map_err(|e| e.to_string())?;
        self.apply_config(&row)
    }

    /// One job's API row (docs/11 §7.1): every v1 field the panel reads, exactly the
    /// shape they had when definitions lived in jobs.json, plus the v2 fields. Built for
    /// GET /api/jobs and the PUT reply, so both show the same shape.
    pub fn job_view(&self, def: &JobDefinition) -> Value {
        let running = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(def.id.as_str())
            .copied()
            .unwrap_or(false);
        let st = self.state.get(&def.id);
        let mut v = def.to_v1_view();
        for (key, value) in def.v2_view_fields() {
            v[key] = value;
        }
        v["source"] = json!("config");
        v["actionAvailable"] = json!(self.services.actions.get(&def.action.type_).is_some());
        v["configRevision"] = json!(self.applied_revision());
        // The live facts the panel reads: lastRunAt / lastOk come from the state file,
        // exactly the shape they had when they rode inside the definition (docs/11 §7.1
        // keeps the v1 /api/jobs fields frozen).
        if let Some(ms) = st.last_run_at {
            v["lastRunAt"] = json!(state::iso_of_ms(ms));
        }
        if let Some(ok) = st.last_ok {
            v["lastOk"] = json!(ok);
        }
        v["running"] = json!(running);
        if !def.disabled {
            if let Some(next) =
                def::next_due_of(def, st.last_run_at, now_ms() as i64, self.anchor_ms)
            {
                v["nextDueAt"] = json!(next);
            }
        }
        v
    }

    /// The listing behind GET /api/jobs.
    pub fn all_views(&self) -> Vec<Value> {
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.definitions.iter().map(|d| self.job_view(d)).collect()
    }

    /// One job's run facts (docs/11 §4) - what lastRunAt / lastOk used to be inside a
    /// JobDef, now read from jobs-state.json.
    pub fn run_state(&self, name: &str) -> state::JobRunState {
        self.state.get(name)
    }

    /// One page of a job's run history (docs/11 §7.3), read from THIS system's log.
    pub fn read_runs(
        &self,
        job: &str,
        before: Option<u64>,
        limit: usize,
    ) -> (Vec<Value>, Option<u64>) {
        self.runlog.read_page(job, before, limit)
    }

    /// Age-and-trim this system's run logs - the hourly sweep task's jobs half.
    pub fn sweep_run_logs(&self) {
        self.runlog.sweep_all();
    }
}

/// Why a definition write did not land. The three answers the v1 API maps to 400 / 409
/// / 500: a bad definition is the caller's, a not-editable-in-v1 row points at the config
/// editor, and a failed persist is this gateway's.
#[derive(Debug)]
pub enum WriteError {
    Invalid(String),
    NotEditableV1(String),
    Persist(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Invalid(m) | WriteError::Persist(m) => write!(f, "{m}"),
            WriteError::NotEditableV1(name) => write!(
                f,
                "job {name} cannot be edited through the v1 shape without losing fields; use PUT /api/plugins/jobs/config"
            ),
        }
    }
}

/// The definitions object of a config row, created empty when absent.
fn definitions_map_of(row: &mut Value) -> Result<&mut Map<String, Value>, WriteError> {
    if row.get("definitions").is_none() {
        row["definitions"] = json!({});
    }
    row.get_mut("definitions")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            WriteError::Invalid("definitions must be an object keyed by job id".to_string())
        })
}

/// Fold a v1-shaped edit onto an editable definition (docs/11 §7.1): command/cwd move
/// into the legacy action input, the schedule becomes the matching trigger, and every
/// v2 field the shape does not know keeps its value - the caller checked
/// [def::JobDefinition::editable_in_v1], so there are none to lose today, and the check
/// plus this fold are what keep that promise true.
fn apply_v1_edit(mut current: JobDefinition, def: &JobDef) -> JobDefinition {
    let mut input = Map::new();
    input.insert("command".into(), json!(def.command));
    if let Some(cwd) = &def.cwd {
        input.insert("cwd".into(), json!(cwd));
    }
    current.action.input = Value::Object(input);
    let first_run = match current.trigger {
        def::Trigger::Interval { first_run, .. } => first_run,
        _ => def::FirstRun::AfterInterval,
    };
    current.trigger = match (def.every_sec, def.cron.as_deref()) {
        (Some(secs), _) => def::Trigger::Interval {
            every_ms: secs.saturating_mul(1000),
            first_run,
        },
        (None, Some(expr)) => match schedule::CronExpr::parse(expr) {
            Ok(compiled) => def::Trigger::Cron {
                expression: expr.to_string(),
                compiled,
            },
            // def.validate() parsed this cron already; unreachable in practice.
            Err(_) => current.trigger.clone(),
        },
        (None, None) => def::Trigger::Manual,
    };
    current.disabled = !def.enabled;
    current.timeout_ms = def.timeout_ms;
    current
}

/// A definition as the config row spells it - the exact grammar
/// [def::JobsConfig::parse] reads back, so a v1 edit can never smuggle in a shape the
/// strict validator would not have accepted.
fn definition_to_config(def: &JobDefinition) -> Value {
    def.to_config_json()
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

/// A unix-ms stamp as a local naive datetime (None when out of range).
fn local_minute_of_ms(ms: i64) -> Option<chrono::NaiveDateTime> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.naive_local())
}

/// Test-only wiring: the shared services with the legacy command capability registered —
/// exactly what the process plugin's start does in a real boot, without a plugin host.
#[cfg(test)]
pub(crate) fn test_services() -> Arc<RuntimeServices> {
    let services = RuntimeServices::new();
    services
        .actions
        .register(Arc::new(
            crate::services::actions::LegacyCommandAction::new(services.supervisor.clone()),
        ))
        .expect("the legacy command capability registers once");
    services
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn scratch_path(name: &str) -> PathBuf {
        crate::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("lmg-jobs-{}-{}", name, crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("jobs.json")
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
        }
    }

    fn cron_job(name: &str, expr: &str) -> JobDef {
        JobDef {
            every_sec: None,
            cron: Some(expr.into()),
            ..interval_job(name, 60)
        }
    }

    /// A system plus a file-backed config store in the same scratch tree - the S3 shape:
    /// definitions live in plugins.jobs.config, jobs.json is the migration input.
    fn system_at(
        path: PathBuf,
        services: Arc<RuntimeServices>,
    ) -> (Arc<JobSystem>, Arc<ConfigStore>) {
        let store = ConfigStore::from_loaded(
            path.parent()
                .expect("jobs.json has a parent")
                .join("gateway.config.json"),
            json!({}),
        );
        let sys = JobSystem::open(path, services, store.clone());
        (sys, store)
    }

    fn system(name: &str, services: Arc<RuntimeServices>) -> (Arc<JobSystem>, Arc<ConfigStore>) {
        system_at(scratch_path(name), services)
    }

    /// The definitions row these tests spell: v1 JobDefs through the migration mapping,
    /// the way the S4 migration and the v1 API both build rows.
    fn row_of(defs: &[JobDef]) -> Value {
        let mut definitions = Map::new();
        for def in defs {
            let v2 = def::JobDefinition::from_v1(def);
            definitions.insert(v2.id.clone(), v2.to_config_json());
        }
        json!({ "definitions": definitions })
    }

    /// Land a row the way the plugin host does: revision-checked store write, then the
    /// in-place apply.
    fn install(sys: &JobSystem, store: &ConfigStore, row: Value) {
        let rev = store.snapshot().revision;
        store
            .update_plugin("jobs", rev, row.clone())
            .expect("the scratch config store accepts the row");
        sys.apply_config(&row).expect("the row applies");
    }

    #[test]
    fn definitions_validate_at_every_entry_point() {
        assert!(interval_job("ok", 60).validate().is_ok());
        assert!(cron_job("ok2", "30 3 * * *").validate().is_ok());
        assert!(interval_job("not/ok", 60).validate().is_err());
        assert!(interval_job("", 60).validate().is_err());
        let mut j = interval_job("ok", 60);
        j.command = "   ".into();
        assert!(j.validate().is_err());
        let mut j = interval_job("ok", 60);
        j.cron = Some("* * * * *".into());
        assert!(j.validate().is_err());
        let mut j = interval_job("ok", 60);
        j.every_sec = None;
        assert!(j.validate().is_err());
        assert!(interval_job("ok", 0).validate().is_err());
        let mut j = interval_job("ok", 60);
        j.timeout_ms = 0;
        assert!(j.validate().is_err());
        assert!(cron_job("ok", "99 * * * *").validate().is_err());
    }

    #[test]
    fn the_store_seals_round_trip_and_drops_invalid_entries() {
        let path = scratch_path("store");
        {
            let mut store = JobStore::open(path.clone());
            store.jobs.push(interval_job("keep-me", 300));
            store.jobs.push({
                let mut bad = interval_job("bad-cron", 0);
                bad.cron = Some("* * * * *".into());
                bad
            });
            store.save().expect("the scratch file is writable");
        }
        let reloaded = JobStore::open(path);
        assert_eq!(reloaded.jobs.len(), 1, "only the valid entry survives");
        assert_eq!(reloaded.jobs[0].name, "keep-me");
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
        assert!(
            def.enabled,
            "a hand-edited file with no flag must not disable the job"
        );
        assert_eq!(def.timeout_ms, runner::DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn interval_due_anchors_on_last_run_or_boot() {
        let now = 1_000_000_000_i64 * 1000;
        let local = chrono::Local
            .timestamp_millis_opt(now)
            .unwrap()
            .naive_local();
        let anchor = now - 5000;
        let d = def::JobDefinition::from_v1(&interval_job("j", 10));
        assert!(!def::definition_due(
            &d,
            anchor + 9000,
            &local,
            anchor,
            None
        ));
        assert!(def::definition_due(
            &d,
            anchor + 10_000,
            &local,
            anchor,
            None
        ));
        assert!(!def::definition_due(
            &d,
            now,
            &local,
            anchor,
            Some(now - 4000)
        ));
        assert!(def::definition_due(
            &d,
            now,
            &local,
            anchor,
            Some(now - 11_000)
        ));
    }

    #[test]
    fn cron_due_requires_a_matching_minute_and_not_the_same_one_twice() {
        let now_local: chrono::NaiveDateTime = chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
            .unwrap()
            .and_hms_opt(3, 30, 5)
            .unwrap();
        let d = def::JobDefinition::from_v1(&cron_job("j", "30 3 * * *"));
        assert!(
            def::definition_due(&d, 0, &now_local, 0, None),
            "matching minute, never ran"
        );
        let same_minute_ms = chrono::Local
            .from_local_datetime(&now_local)
            .unwrap()
            .timestamp_millis();
        assert!(!def::definition_due(
            &d,
            0,
            &now_local,
            0,
            Some(same_minute_ms)
        ));
        let yesterday = same_minute_ms - 24 * 60 * 60 * 1000;
        assert!(def::definition_due(&d, 0, &now_local, 0, Some(yesterday)));
        let off = chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
            .unwrap()
            .and_hms_opt(3, 31, 0)
            .unwrap();
        assert!(!def::definition_due(&d, 0, &off, 0, Some(yesterday)));
    }

    #[test]
    fn a_manual_definition_never_auto_fires_and_has_no_next_due() {
        let d = def::JobDefinition {
            trigger: def::Trigger::Manual,
            ..def::JobDefinition::from_v1(&interval_job("by-hand", 60))
        };
        let now_local = chrono::Local::now().naive_local();
        assert!(!def::definition_due(&d, i64::MAX, &now_local, 0, None));
        assert!(def::next_due_of(&d, None, 0, 0).is_none());
        // The v1 shape cannot spell a schedule-less job: its validator still refuses one.
        assert!(JobDef {
            every_sec: None,
            cron: None,
            ..interval_job("x", 60)
        }
        .validate()
        .is_err());
    }

    /// The S3 promise in one assertion set (docs/11 §9 S3): the row IS the table. A due
    /// interval job fires, a future one does not, a disabled one does not, a manual one
    /// never auto-fires - and a jobs.json row schedules nothing any more.
    #[test]
    fn config_definitions_drive_the_due_set() {
        let path = scratch_path("due-set");
        let (sys, store) = system_at(path.clone(), test_services());
        let mut disabled = interval_job("off", 1);
        disabled.enabled = false;
        let manual = def::JobDefinition {
            trigger: def::Trigger::Manual,
            ..def::JobDefinition::from_v1(&interval_job("by-hand", 1))
        };
        let mut row = row_of(&[interval_job("due-now", 1), interval_job("later", 3_600)]);
        row["definitions"]["off"] = def::JobDefinition::from_v1(&disabled).to_config_json();
        row["definitions"]["by-hand"] = manual.to_config_json();
        install(&sys, &store, row);

        let now = sys.anchor_ms + 1500;
        let local = local_minute_of_ms(now).expect("a local time");
        assert_eq!(
            sys.due_jobs(now, &local),
            vec!["due-now".to_string()],
            "only the elapsed interval fires"
        );
        // jobs.json no longer schedules anything, even with a v1 row sitting on disk
        // next to the config store - which is exactly what an S3 boot of an old tree
        // looks like until the S4 migration moves it.
        {
            let mut legacy = JobStore::open(path.clone());
            legacy.jobs.push(interval_job("legacy-row", 1));
            legacy.save().expect("the scratch file is writable");
        }
        assert_eq!(
            sys.due_jobs(now, &local).len(),
            1,
            "jobs.json is not consulted"
        );
    }

    /// docs/11 §8: capacity changes reach the shared coordinator on apply.
    #[test]
    fn apply_config_moves_capacity() {
        let (sys, store) = system("capacity-apply", test_services());
        assert_eq!(
            sys.services.runs.capacity().max_concurrent,
            crate::services::runs::DEFAULT_MAX_CONCURRENT_RUNS
        );
        install(
            &sys,
            &store,
            json!({ "maxConcurrentRuns": 5, "maxQueuedRuns": 7, "definitions": {} }),
        );
        let cap = sys.services.runs.capacity();
        assert_eq!(cap.max_concurrent, 5);
        assert_eq!(cap.max_queued, 7);
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

    #[tokio::test]
    async fn a_manual_run_claims_executes_records_and_persists() {
        let path = scratch_path("manual");
        let (sys, store) = system_at(path.clone(), test_services());
        install(&sys, &store, row_of(&[interval_job("echo-job", 3_600)]));

        let (_seq, out) = sys
            .clone()
            .execute("echo-job", "manual")
            .await
            .expect("runs");
        assert!(out.ok, "{out:?}");
        assert!(sys.clone().execute("echo-job", "manual").await.is_ok());
        let (page, _) = sys.read_runs("echo-job", None, 10);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["trigger"], json!("manual"));

        // A fresh boot over the same tree reads the same facts: the config row is
        // re-applied the way plugin create does, and the state file carries the run
        // facts across the boundary (docs/11 §4).
        let reopened = JobSystem::open(path, test_services(), store.clone());
        reopened
            .apply_config(&store.plugin_config("jobs"))
            .expect("the persisted row re-applies");
        let st = reopened.run_state("echo-job");
        assert_eq!(st.last_ok, Some(true));
        assert!(st.last_run_at.is_some(), "the claim stamped lastRunAt");
        assert_eq!(
            reopened.all_views().len(),
            1,
            "the reopened system lists the config's definition"
        );
    }

    #[tokio::test]
    async fn an_unknown_job_is_unknown_and_a_claimed_job_is_busy() {
        let (sys, store) = system("claims", test_services());
        match sys.clone().execute("ghost", "manual").await {
            Err(RunError::Unknown(msg)) => assert!(msg.contains("ghost"), "{msg}"),
            other => panic!("expected Unknown, got {other:?}"),
        }
        install(&sys, &store, row_of(&[interval_job("busy-job", 3_600)]));
        let _def = sys
            .claim("busy-job", now_ms() as i64)
            .expect("first claim wins");
        match sys.clone().execute("busy-job", "manual").await {
            Err(RunError::Busy(msg)) => assert!(msg.contains("already running"), "{msg}"),
            other => panic!("expected Busy, got {other:?}"),
        }
        assert!(!sys.due_jobs_now().contains(&"busy-job".to_string()));
    }

    /// The process plugin is disabled: the capability is simply not registered. A job that
    /// comes due must SAY so - a missing dependency is reported, never silently skipped -
    /// and the listing reports it as not action-available while remaining saved.
    #[tokio::test]
    async fn a_missing_capability_is_reported_recorded_and_listed() {
        let (sys, store) = system("nocap", RuntimeServices::new());
        install(&sys, &store, row_of(&[interval_job("orphan", 3_600)]));

        match sys.clone().execute("orphan", "timer").await {
            Err(RunError::Unavailable(msg)) => assert!(msg.contains(JOBS_ACTION), "{msg}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }

        let (page, _) = sys.read_runs("orphan", None, 10);
        assert_eq!(page.len(), 1, "{page:?}");
        assert_eq!(page[0]["ok"], json!(false));
        assert!(page[0]["error"]
            .as_str()
            .unwrap_or_default()
            .contains(JOBS_ACTION));

        let view = sys.all_views().remove(0);
        assert_eq!(view["running"], json!(false));
        assert_eq!(
            view["actionAvailable"],
            json!(false),
            "the listing says the provider is gone"
        );
        assert_eq!(
            view["editableInV1"],
            json!(true),
            "the definition itself stays a plain legacy row"
        );
        let st = sys.run_state("orphan");
        assert_eq!(st.last_ok, None, "a refusal is not an outcome");
        assert!(st.last_run_at.is_some());
    }

    /// The pool's bound is GLOBAL and now config-driven: a row with one slot and no
    /// queue is the visible-refusal setup, and a manual run of another producer filling
    /// that slot refuses the scheduled occurrence instead of queueing it out of sight.
    #[tokio::test]
    async fn a_full_pool_refuses_the_occurrence_visibly() {
        let (sys, store) = system("capacity", test_services());
        install(
            &sys,
            &store,
            json!({
                "maxConcurrentRuns": 1,
                "maxQueuedRuns": 0,
                "definitions": row_of(&[interval_job("waiting", 3_600)])["definitions"],
            }),
        );
        assert_eq!(sys.services.runs.capacity().max_concurrent, 1);
        let services = sys.services.clone();

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
        let (page, _) = sys.read_runs("waiting", None, 10);
        assert_eq!(
            page.len(),
            1,
            "the refused occurrence is recorded: {page:?}"
        );
        assert_eq!(services.runs.shutdown_all().await, 1);
        assert!(
            hog.done.await.is_ok(),
            "the hog reported its own terminal view"
        );
    }

    /// The job's own timeoutMs is the deadline, enforced by cancellation: the child is killed
    /// and the record says WHICH kind of not-ok it was.
    #[tokio::test]
    async fn a_wedged_command_is_killed_at_the_job_deadline() {
        let (sys, store) = system("deadline", test_services());
        let mut def = interval_job("wedged", 3_600);
        def.command = long_command();
        def.timeout_ms = 1_500; // v2 floors timeoutMs at 1000 (docs/11 §3.2)
        install(&sys, &store, row_of(&[def]));

        let started = std::time::Instant::now();
        let (_seq, out) = match sys.clone().execute("wedged", "timer").await {
            Ok(r) => r,
            Err(err) => panic!("the run must report, not refuse: {err}"),
        };
        assert!(out.timed_out, "{out:?}");
        assert!(!out.ok);
        assert!(
            !out.canceled,
            "the deadline owns this outcome, not a cancel"
        );
        assert!(
            started.elapsed().as_secs() < 20,
            "it did not wait out the command"
        );

        let (page, _) = sys.read_runs("wedged", None, 5);
        assert_eq!(page[0]["timedOut"], json!(true), "{page:?}");
        assert_eq!(page[0]["ok"], json!(false));
    }

    /// Disabling the jobs plugin means what it says: the runs this scheduler owns are
    /// cancelled AND awaited, and the per-job claims they held are gone.
    #[tokio::test]
    async fn shutdown_cancels_the_runs_this_scheduler_owns() {
        let (sys, store) = system("shutdown", test_services());
        let mut def = interval_job("slow", 3_600);
        def.command = long_command();
        install(&sys, &store, row_of(&[def]));

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
        assert!(!sys.all_views().iter().any(|v| v["running"] == json!(true)));
    }
    /// docs/10 §5 asks for this by name: a persist that failed must not be reported as a
    /// save. The config row is the definition store now, so a store whose file cannot be
    /// written refuses the v1 edit - and nothing half-applies anywhere.
    #[test]
    fn a_config_write_that_cannot_persist_is_an_error_not_a_warning() {
        crate::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("lmg-jobs-gone-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // jobs.json and the runlog tree stay real; only the config file sits under a
        // directory that does not exist, so every store persist fails on every platform.
        let store =
            ConfigStore::from_loaded(dir.join("gone").join("gateway.config.json"), json!({}));
        let sys = JobSystem::open(dir.join("jobs.json"), test_services(), store);

        match sys.upsert_v1(interval_job("doomed", 60)) {
            Err(WriteError::Persist(err)) => assert!(!err.is_empty()),
            other => panic!("the write cannot succeed: {other:?}"),
        }
        assert!(
            sys.all_views().is_empty(),
            "nothing half-applied: the row never landed"
        );
        // Deleting something that was never there is not a failed write; it is a miss.
        assert!(matches!(sys.delete("never-existed"), Ok(false)));
    }

    #[test]
    fn v1_edits_never_touch_run_state_and_delete_forgets_it() {
        let store = ConfigStore::memory(json!({}));
        let sys = JobSystem::open(scratch_path("upsert"), test_services(), store);
        sys.upsert_v1(interval_job("edit-me", 60)).expect("valid");
        // Run facts arrive through the state file, not the definition row.
        sys.state.claim("edit-me", 123, None);
        sys.state.settle("edit-me", false, None);
        sys.upsert_v1(interval_job("edit-me", 120))
            .expect("valid edit");
        let view = sys.all_views().remove(0);
        assert_eq!(view["everySec"], json!(120), "the edit landed in the row");
        assert_eq!(view["source"], json!("config"));
        let st = sys.run_state("edit-me");
        assert_eq!(
            st.last_run_at,
            Some(123),
            "an edit cannot fabricate a never-ran"
        );
        assert_eq!(st.last_ok, Some(false));
        assert_eq!(st.consecutive_failures, 1);
        // The v1 /api/jobs shape still SHOWS the facts next to the definition.
        assert_eq!(view["lastOk"], json!(false));
        assert!(view["lastRunAt"].as_str().is_some());
        assert!(matches!(sys.delete("edit-me"), Ok(true)));
        assert!(matches!(sys.delete("edit-me"), Ok(false)), "already gone");
        assert!(sys.all_views().is_empty());
        // The deleted definition's state line is gone too (docs/11 §4); the run history
        // file is deliberately NOT touched - it outlives the job.
        assert_eq!(sys.run_state("edit-me"), state::JobRunState::default());
    }

    #[test]
    fn a_job_view_reports_running_a_next_due_and_the_v2_fields() {
        let (sys, store) = system("view", test_services());
        install(
            &sys,
            &store,
            row_of(&[
                interval_job("viewed", 60),
                cron_job("nightly", "30 3 * * *"),
            ]),
        );
        let views: Vec<Value> = sys.all_views();
        assert_eq!(views.len(), 2);
        for v in &views {
            assert_eq!(v["running"], json!(false));
            assert!(
                v.get("nextDueAt").and_then(Value::as_str).is_some(),
                "every enabled job shows its next firing: {v}"
            );
            // The v2 fields ride along (docs/11 §7.1) so a v2 client needs no second
            // endpoint and the panel's future form can round-trip through the row.
            assert_eq!(v["source"], json!("config"));
            assert_eq!(v["actionAvailable"], json!(true));
            assert_eq!(v["editableInV1"], json!(true));
            assert!(v["configRevision"].as_u64().is_some(), "{v}");
            assert!(
                v["trigger"]
                    .as_object()
                    .is_some_and(|t| t.contains_key("kind")),
                "{v}"
            );
            assert!(
                v["action"]
                    .as_object()
                    .is_some_and(|a| a.contains_key("type")),
                "{v}"
            );
        }
        // A disabled definition shows no nextDueAt - nothing is scheduled.
        let mut disabled = interval_job("off", 60);
        disabled.enabled = false;
        let mut row = row_of(&[disabled]);
        row["definitions"].as_object_mut().expect("map").insert(
            "viewed".into(),
            def::JobDefinition::from_v1(&interval_job("viewed", 60)).to_config_json(),
        );
        install(&sys, &store, row);
        let v = sys
            .all_views()
            .into_iter()
            .find(|v| v["name"] == json!("off"))
            .unwrap();
        assert!(v.get("nextDueAt").is_none());
    }

    /// docs/11 §8: deleting a definition cancels its FUTURE schedule only. The run in
    /// flight holds the snapshot it claimed and finishes normally, and the history file
    /// outlives the job that wrote it.
    #[tokio::test]
    async fn deleting_a_definition_keeps_its_in_flight_run_and_history() {
        let (sys, store) = system("delete-inflight", test_services());
        let mut def = interval_job("keeper", 3_600);
        def.command = if cfg!(windows) {
            "ping -n 3 127.0.0.1".into() // ~2s: long enough to delete mid-run
        } else {
            "sleep 2".into()
        };
        install(&sys, &store, row_of(&[def]));

        let running = {
            let sys = sys.clone();
            tokio::spawn(async move { sys.execute("keeper", "timer").await })
        };
        assert!(
            wait_running(&sys, "keeper").await,
            "the run reached the pool"
        );
        assert!(matches!(sys.delete("keeper"), Ok(true)));
        let (_seq, out) = running.await.expect("the run task").expect("reports");
        assert!(
            out.ok,
            "the in-flight run was NOT canceled by the delete: {out:?}"
        );
        assert!(sys.all_views().is_empty(), "the schedule is gone");
        let (page, _) = sys.read_runs("keeper", None, 5);
        assert_eq!(page.len(), 1, "the history outlives the definition");
    }

    /// docs/11 §7.1: a definition the v1 shape cannot spell without loss refuses the v1
    /// edit outright - overwriting it would silently drop fields.
    #[test]
    fn v1_edit_of_a_v2_only_definition_is_refused() {
        let (sys, store) = system("v2-only", test_services());
        install(
            &sys,
            &store,
            json!({
                "definitions": {
                    "proud": {
                        "title": "A titled job",
                        "trigger": { "kind": "interval", "everyMs": 60000 },
                        "action": {
                            "type": "process.legacy-command",
                            "input": { "command": "echo hi" }
                        }
                    }
                }
            }),
        );
        let view = sys.all_views().remove(0);
        assert_eq!(view["editableInV1"], json!(false), "{view}");
        match sys.upsert_v1(interval_job("proud", 60)) {
            Err(err @ WriteError::NotEditableV1(_)) => assert!(
                err.to_string().contains("/api/plugins/jobs/config"),
                "{err}"
            ),
            other => panic!("expected NotEditableV1, got {other:?}"),
        }
        // The refused edit changed nothing.
        assert_eq!(sys.all_views().remove(0)["title"], json!("A titled job"));
    }
}
