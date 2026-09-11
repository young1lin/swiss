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
//!   run log, and summarised into the gateway log so 'swiss logs' shows it.
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
pub mod clock;
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

use swiss_core::log;
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};
use swiss_core::util::now_ms;
use swiss_host::config_store::ConfigStore;
use swiss_host::services::runs::{SubmitError, SubmitRequest};
use swiss_host::services::RuntimeServices;

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
    /// Per-job environment variables, added on top of the inherited environment at run
    /// time (the panel's "Environment variables" box). Sorted keys: a state file must
    /// serialize deterministically no matter which map the edit came through.
    pub env: Option<std::collections::BTreeMap<String, String>>,
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
            env: match v.get("env") {
                None | Some(Value::Null) => None,
                Some(Value::Object(map)) => {
                    let mut out = std::collections::BTreeMap::new();
                    for (k, val) in map {
                        let Some(s) = val.as_str() else {
                            return Err(format!("env.{k}: must be a string"));
                        };
                        if k.is_empty() || k.contains('=') || k.contains('\0') {
                            return Err(format!("env.{k}: not a usable variable name"));
                        }
                        out.insert(k.clone(), s.to_string());
                    }
                    if out.is_empty() { None } else { Some(out) }
                }
                Some(_) => return Err("env: must be an object of string values".into()),
            },
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
        if let Some(env) = &self.env {
            m.insert("env".into(), json!(env));
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
    /// The clock the scheduler reads (docs/11 §6.6): the real one is chrono::Local; the
    /// DST tests inject a fake with a synthetic rule so the same assertions pass on any
    /// machine. Swappable before start through [JobSystem::set_clock] - the test seam.
    clock: Mutex<Arc<dyn clock::Clock>>,
    /// The next-due table (docs/11 §6.5): one unix-ms instant per job id. Rebuilt on
    /// every apply, recomputed for ONE job when its occurrence is claimed - so the
    /// per-second tick is a map scan, never a calendar walk.
    next_due: Mutex<HashMap<String, i64>>,
    /// Serializes (overlap check -> submit) pairs across the tick and the manual API, so
    /// two of them cannot both see \"not running\" and double-submit one job. No await is
    /// ever held through this lock.
    submit_gate: Mutex<()>,
    /// Cancellation for occurrence tasks (docs/11 §6.4): a retry sitting out its delayMs
    /// must abort the moment the plugin stops, not after the delay. A fresh channel per
    /// start() so a stopped-and-re-enabled scheduler works again.
    cancel: Mutex<tokio::sync::watch::Sender<bool>>,
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
            clock: Mutex::new(Arc::new(clock::LocalClock)),
            next_due: Mutex::new(HashMap::new()),
            submit_gate: Mutex::new(()),
            cancel: Mutex::new(tokio::sync::watch::channel(false).0),
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
            .set_capacity(swiss_host::services::runs::RunCapacity {
                max_concurrent,
                max_queued,
            });
        self.runlog
            .set_limits(runlog::limits_of_retention(&retention));
        // The applied table changed, so every next-due entry is recomputed once here
        // (docs/11 §6.5) - the per-second tick never walks the calendar.
        self.rebuild_next_due();
        Ok(())
    }

    /// The registry-backed action lookup behind `actionAvailable` (docs/11 §7.1).
    pub fn actions(&self) -> &Arc<swiss_host::services::action::ActionRegistry> {
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

    /// The test seam of docs/11 §6.6: swap the clock (before start) and rebuild the
    /// next-due table against it. Production never calls this - the system opens on
    /// [clock::LocalClock].
    pub fn set_clock(&self, clock: Arc<dyn clock::Clock>) {
        *self.clock.lock().unwrap_or_else(|e| e.into_inner()) = clock;
        self.rebuild_next_due();
    }

    fn clock_now_ms(&self) -> i64 {
        self.clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .now_ms()
    }

    /// Recompute the next-due table for the whole applied table (docs/11 §6.5): the one
    /// place every definition is walked. Called on apply and on clock swap - a per-job
    /// recompute happens when an occurrence is claimed, never in the tick.
    fn rebuild_next_due(&self) {
        let clock = self.clock.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let now = clock.now_ms();
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut table: HashMap<String, i64> = HashMap::new();
        for d in &cfg.definitions {
            if d.disabled {
                continue;
            }
            let last = self.state.get(&d.id).last_run_at;
            if let Some(ms) = def::next_due_ms(d, last, now, self.anchor_ms, clock.as_ref()) {
                table.insert(d.id.clone(), ms);
            }
        }
        *self.next_due.lock().unwrap_or_else(|e| e.into_inner()) = table;
    }

    /// Recompute ONE job's next-due entry from its claim state. The post-claim path: the
    /// calendar walk happens exactly once per occurrence, for exactly that job.
    fn refresh_next_due(&self, id: &str) {
        let clock = self.clock.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let now = clock.now_ms();
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut table = self.next_due.lock().unwrap_or_else(|e| e.into_inner());
        match cfg.definitions.iter().find(|d| d.id == id) {
            Some(d) if !d.disabled => {
                let last = self.state.get(id).last_run_at;
                match def::next_due_ms(d, last, now, self.anchor_ms, clock.as_ref()) {
                    Some(ms) => {
                        table.insert(id.to_string(), ms);
                    }
                    None => {
                        table.remove(id);
                    }
                }
            }
            _ => {
                table.remove(id);
            }
        }
    }

    /// Start the one-second tick task (docs/11 §6.5): each tick is one map scan - the
    /// entries at or before \"now\" - and one spawned occurrence task per due job. No
    /// calendar math ever runs here. A fresh cancel channel per start, so a
    /// stopped-and-re-enabled scheduler works again.
    pub fn start(self: &Arc<Self>) {
        self.rebuild_next_due();
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) =
            tokio::sync::watch::channel(false).0;
        let sys = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                for name in sys.due_jobs_now() {
                    let sys = sys.clone();
                    tokio::spawn(async move { sys.run_occurrence(&name).await });
                }
            }
        });
        if let Ok(mut slot) = self.timer.lock() {
            *slot = Some(handle);
        }
    }

    /// Stop SCHEDULING: the tick task is aborted and every occurrence task sitting out a
    /// retry delay hears the cancel signal, so nothing new fires and no delayed retry
    /// comes back to life later. Runs already executing are the coordinator's - see
    /// [JobSystem::shutdown] for the teardown that takes them back.
    pub fn stop(&self) {
        if let Ok(mut slot) = self.timer.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
        if let Ok(tx) = self.cancel.lock() {
            let _ = tx.send(true);
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
        self.services.runs.cancel_owner(JOBS_OWNER).await
    }

    /// The jobs whose next-due entry sits at or before now. The tick's whole read side:
    /// a clock read and a map scan (docs/11 §6.5).
    pub fn due_jobs_now(&self) -> Vec<String> {
        let now = self.clock_now_ms();
        self.due_jobs(now)
    }

    /// The testable half of the tick: the jobs whose next-due entry is at or before
    /// `now`. The table is maintained by apply/claim (docs/11 §6.5), so this reads the
    /// map, not the calendar.
    pub fn due_jobs(&self, now: i64) -> Vec<String> {
        self.next_due
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The definition snapshot for `name`, or Unknown. The clone IS the snapshot
    /// (docs/11 §8, 修改中任务): an apply_config that rewrites the table mid-flight
    /// cannot reach into a run that already claimed its definition - the run finishes
    /// under the one it started with.
    fn definition_of(&self, name: &str) -> Result<JobDefinition, RunError> {
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.definitions
            .iter()
            .find(|d| d.id == name)
            .cloned()
            .ok_or_else(|| RunError::Unknown(format!("unknown job: {name}")))
    }

    /// Await a submitted run to its terminal view.
    async fn await_outcome(
        submitted: swiss_host::services::runs::SubmittedRun,
    ) -> runner::RunOutcome {
        match submitted.done.await {
            Ok(view) => runner::RunOutcome::from_run_view(&view),
            // The coordinator dropped the sender: the run task was aborted under us
            // (process teardown). Record what is actually known - that it never reported.
            Err(_) => runner::RunOutcome::refused(
                "the run ended without reporting (gateway shutting down)".into(),
            ),
        }
    }

    /// The manual submission half, shared by both manual doors (docs/11 §7.3): the
    /// check-then-submit under the gate, and on a submission refusal the record plus the
    /// typed error. There is exactly ONE execution path into the coordinator - the sync
    /// and async doors differ only in who waits for the outcome.
    async fn submit_manual(
        self: &Arc<Self>,
        name: &str,
        trigger: &str,
    ) -> Result<swiss_host::services::runs::SubmittedRun, RunError> {
        let def = self.definition_of(name)?;
        let label = format!("job:{}", def.id);
        let submitted = {
            let _gate = self.submit_gate.lock().unwrap_or_else(|e| e.into_inner());
            if self.services.runs.count_for_label(&label) > 0 {
                return Err(RunError::Busy(format!("job {} is already running", def.id)));
            }
            self.services.runs.submit(SubmitRequest {
                owner: JOBS_OWNER.to_string(),
                label,
                // The definition's OWN action ref (docs/11 §3.4): any registered capability,
                // input verbatim - env refs stay refs and resolve inside the action.
                action_type: def.action.type_.clone(),
                input: def.action.input.clone(),
                timeout_ms: def.timeout_ms,
                queue_if_busy: false,
            })
        };
        match submitted {
            Ok(s) => Ok(s),
            Err(err) => {
                let refusal = runner::RunOutcome::refused(err.to_string());
                self.record_ran(name, trigger, None, (1, 1), None, &refusal);
                Err(match err {
                    SubmitError::Capacity(m) => RunError::Capacity(m),
                    SubmitError::Action(m) => RunError::Unavailable(m),
                })
            }
        }
    }

    /// The manual-run door (POST /run and tests): ONE attempt, no occurrence key, no
    /// retry - a manual run is a human's explicit act, not an occurrence the scheduler
    /// owns (docs/11 §6.1), so it never moves a scheduling anchor either. Busy when
    /// this job already has a run in flight, whatever started it (the label gate).
    pub async fn execute(
        self: Arc<Self>,
        name: &str,
        trigger: &str,
    ) -> Result<(u64, runner::RunOutcome), RunError> {
        let submitted = self.submit_manual(name, trigger).await?;
        let run_id = submitted.run_id;
        let out = Self::await_outcome(submitted).await;
        let seq = self.record_ran(name, trigger, None, (1, 1), Some(run_id), &out);
        self.persist_last_ok(name, out.ok, Some(run_id));
        Ok((seq, out))
    }

    /// The async manual door (POST /run with {"async": true}, docs/11 §7.3): submit,
    /// hand the runId back immediately, and finish the bookkeeping in a detached task.
    /// The run outlives the request by design - closing the page does not stop the job;
    /// the record and lastOk settle exactly as the sync door would have written them.
    pub async fn execute_async(self: Arc<Self>, name: &str) -> Result<u64, RunError> {
        let submitted = self.submit_manual(name, "manual").await?;
        let run_id = submitted.run_id;
        let sys = Arc::clone(&self);
        let job = name.to_string();
        tokio::spawn(async move {
            let out = Self::await_outcome(submitted).await;
            sys.record_ran(&job, "manual", None, (1, 1), Some(run_id), &out);
            sys.persist_last_ok(&job, out.ok, Some(run_id));
        });
        Ok(run_id)
    }

    /// One scheduled occurrence, start to finish (docs/11 §6): claim + occurrence
    /// identity, misfire accounting, the overlap gate, the retry loop with cancellable
    /// delays, and a record for everything that happened on the way.
    async fn run_occurrence(self: &Arc<Self>, name: &str) {
        let clock = self.clock.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let now = clock.now_ms();
        let Ok(def) = self.definition_of(name) else {
            self.refresh_next_due(name);
            return;
        };
        if def.disabled {
            self.refresh_next_due(name);
            return;
        }
        let st = self.state.get(name);
        let Some((count, latest_due)) =
            def::missed_occurrences(&def, now, self.anchor_ms, st.last_run_at, clock.as_ref())
        else {
            // Nothing is actually due - the clock moved, or a concurrent occurrence
            // claimed first. Re-arm and go.
            self.refresh_next_due(name);
            return;
        };
        let key = def::occurrence_key_of(&def.trigger, latest_due, clock.as_ref());
        // Occurrence identity (docs/11 §6.1): a restart inside the same cron minute - or
        // the fall-back's repeated local minute - already ran this occurrence.
        if st.last_occurrence_key.as_deref() == Some(key.as_str()) {
            self.refresh_next_due(name);
            return;
        }
        // CLAIM first, at the OCCURRENCE's instant: the anchor moves whatever happens
        // next - skip, refusal, run - or a full pool would make the job re-fire every
        // tick (the §6.2 invariant, kept from the v1 scheduler).
        self.state.claim(name, latest_due, Some(&key));
        self.refresh_next_due(name);

        // Misfire accounting (docs/11 §6.3): everything older than the newest
        // occurrence is one summary line, never a catch-up storm.
        if count > 1 {
            self.record_missed(name, &key, count - 1);
            if matches!(def.misfire, def::Misfire::Skip) {
                // skip: not even the newest runs - the anchor already sits at it, the
                // next firing is one period away, and the summary is the whole story.
                return;
            }
        }

        // The retry loop (docs/11 §6.4). Attempt 1..=max_attempts; each attempt is its
        // own submit (so the global bounds apply every time); the wait between attempts
        // is cancellable - a plugin stop aborts the occurrence mid-delay.
        let label = format!("job:{}", def.id);
        let queue = matches!(def.overlap, def::Overlap::QueueOne);
        let mut cancel_rx = self
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .subscribe();
        let mut attempt = 1u32;
        let attempts = def.retry.max_attempts;
        loop {
            let submitted = {
                let _gate = self.submit_gate.lock().unwrap_or_else(|e| e.into_inner());
                if !queue && self.services.runs.count_for_label(&label) > 0 {
                    // overlap=skip: the occurrence is skipped, visibly, anchor already
                    // moved - this is the steady-state rule, not an error.
                    self.record_skipped(name, &key, "overlap");
                    return;
                }
                self.services.runs.submit(SubmitRequest {
                    owner: JOBS_OWNER.to_string(),
                    label: label.clone(),
                    action_type: def.action.type_.clone(),
                    input: def.action.input.clone(),
                    timeout_ms: def.timeout_ms,
                    // queue-one: a busy pool queues this occurrence (docs/11 §6.2);
                    // the queue's own bound turns into a visible capacity skip below.
                    queue_if_busy: queue,
                })
            };
            let (run_id, out) = match submitted {
                Ok(s) => {
                    let id = s.run_id;
                    (Some(id), Self::await_outcome(s).await)
                }
                Err(SubmitError::Capacity(reason)) => {
                    // A full pool (skip) or a full queue (queue-one): recorded, anchor
                    // already moved, reported - never silently dropped.
                    self.record_skipped(name, &key, "capacity");
                    log::warn(
                        "job occurrence refused by a full pool",
                        Some(json!({ "name": name, "reason": reason })),
                    );
                    return;
                }
                Err(SubmitError::Action(reason)) => {
                    // A missing capability is a REFUSAL, not a failed run: lastOk must
                    // not claim an exit that never happened. Retrying it would only
                    // keep hitting the same missing wall (docs/11 §6.4).
                    let refusal = runner::RunOutcome::refused(reason.clone());
                    self.record_ran(
                        name,
                        "timer",
                        Some(&key),
                        (attempt, attempts),
                        None,
                        &refusal,
                    );
                    log::warn(
                        "job occurrence refused: the action is not registered",
                        Some(json!({ "name": name, "reason": reason })),
                    );
                    return;
                }
            };

            self.record_ran(name, "timer", Some(&key), (attempt, attempts), run_id, &out);

            // Retry only the outcomes the policy names (docs/11 §6.4): failure = a
            // non-zero exit or a spawn error; timeout = the deadline. canceled never
            // retries - someone asked for it to stop - and neither do refusals.
            let retryable = !out.ok
                && !out.canceled
                && ((out.timed_out && def.retry.retry_on.contains(&def::RetryOn::Timeout))
                    || (!out.timed_out && def.retry.retry_on.contains(&def::RetryOn::Failure)));
            if out.ok || out.canceled || !retryable || attempt >= attempts {
                self.persist_last_ok(name, out.ok, run_id);
                return;
            }

            // The cancellable delay before the next attempt.
            let delay = def::retry_delay_ms(&def.retry, attempt + 1);
            if delay > 0 {
                let waited = tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => true,
                    _ = cancel_rx.changed() => !*cancel_rx.borrow(),
                };
                if !waited {
                    // The plugin stopped mid-delay: the occurrence is over. The last
                    // attempt's record already says what happened; no further attempts.
                    return;
                }
            }
            attempt += 1;
        }
    }

    /// Append one entry to the job's run log; returns the sequence number (0 when the
    /// record could not be written - a warning, never a failure: the run already
    /// happened). `entry` is fully shaped by the caller; this adds the gateway-log line.
    fn record_entry(&self, name: &str, entry: Map<String, Value>, level: &str, msg: &str) -> u64 {
        let seq = match self.runlog.append_run(name, entry) {
            Ok(seq) => seq,
            Err(err) => {
                log::warn(
                    "job run could not be recorded",
                    Some(json!({ "name": name, "err": err })),
                );
                0
            }
        };
        log::log(level, msg, Some(json!({ "name": name, "seq": seq })));
        seq
    }
    /// Test seam: append one bare history record for `job` without running anything -
    /// pagination tests need a known number of entries, not real processes.
    #[cfg(test)]
    pub(crate) fn append_test_run(&self, job: &str, at_seq: u64) -> u64 {
        let entry = json!({
            "at": state::iso_of_ms(swiss_core::util::now_ms() as i64),
            "trigger": "timer",
            "ok": true,
            "ms": 5,
            "outcome": "ran",
            "attempt": 1,
            "attempts": 1,
            "seq": at_seq,
        });
        let Value::Object(map) = entry else {
            unreachable!()
        };
        self.runlog.append_run(job, map).unwrap_or(0)
    }

    /// One ATTEMPT's record (docs/11 §7.3): outcome "ran" (or "refused" for a
    /// submission that never started), with the occurrence key, the attempt numbers and
    /// the coordinator's run id riding along.
    fn record_ran(
        &self,
        name: &str,
        trigger: &str,
        occurrence: Option<&str>,
        try_no: (u32, u32),
        run_id: Option<u64>,
        out: &runner::RunOutcome,
    ) -> u64 {
        let (attempt, attempts) = try_no;
        let mut entry = ran_record(trigger, occurrence, attempt, attempts, run_id, out);
        // output.capture "none" (docs/11 §3.2): the record keeps NO output - the run
        // itself still captured; only the history is cut.
        let none_capture = self
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .definitions
            .iter()
            .find(|d| d.id == name)
            .is_some_and(|d| matches!(d.output.capture, def::OutputCapture::None));
        if none_capture {
            entry.remove("output");
            entry.remove("chars");
            entry.remove("preview");
        }
        let level = if out.ok { "info" } else { "warn" };
        self.record_entry(name, entry, level, "job ran")
    }

    /// A skipped occurrence (docs/11 §6.2/§7.3): no exit code, no output, the reason
    /// and the occurrence key are the story.
    fn record_skipped(&self, name: &str, occurrence: &str, reason: &str) -> u64 {
        let entry = json!({
            "at": log::iso_now(),
            "trigger": "timer",
            "ok": false,
            "ms": 0,
            "outcome": "skipped",
            "reason": reason,
            "occurrenceKey": occurrence,
        });
        let Value::Object(map) = entry else {
            unreachable!("an object literal")
        };
        self.record_entry(name, map, "warn", "job occurrence skipped")
    }

    /// The misfire summary (docs/11 §6.3): ONE line for every occurrence that will
    /// never be caught up, with the newest occurrence's key for context.
    fn record_missed(&self, name: &str, occurrence: &str, missed: usize) -> u64 {
        let entry = json!({
            "at": log::iso_now(),
            "trigger": "timer",
            "ok": false,
            "ms": 0,
            "outcome": "missed",
            "missedCount": missed,
            "occurrenceKey": occurrence,
        });
        let Value::Object(map) = entry else {
            unreachable!("an object literal")
        };
        self.record_entry(name, map, "warn", "job occurrences missed while down")
    }

    /// Persist the outcome of the last actual attempt. Skipped for a refusal that never
    /// started: lastOk must not claim an exit that did not happen. The run id rides
    /// along now that execute carries it back (the S5 promise in state.rs).
    fn persist_last_ok(&self, name: &str, ok: bool, run_id: Option<u64>) {
        let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        // The job may have been deleted mid-run; the run record is already on disk, and a
        // deleted definition owns no state line (delete forgets it) - settle would only
        // resurrect a line for a job that no longer exists.
        if !cfg.definitions.iter().any(|d| d.id == name) {
            return;
        }
        // Best-effort: the run already happened and its record is already in the run log.
        self.state.settle(name, ok, run_id);
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
        // "Running" is the coordinator's truth now (docs/11 §6.2): the label count is
        // owner-blind, so a manual submission of the same job shows as running too -
        // which is exactly what the reader wants to know.
        let running = self
            .services
            .runs
            .count_for_label(&format!("job:{}", def.id))
            > 0;
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
            // The maintained table, not a calendar walk per view (docs/11 §6.5).
            if let Some(next) = self
                .next_due
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&def.id)
                .copied()
            {
                v["nextDueAt"] = json!(state::iso_of_ms(next));
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
    if let Some(env) = &def.env {
        input.insert("env".into(), json!(env));
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

/// One ATTEMPT's JSONL record and API-reply shape (docs/11 §7.3) - built in ONE place
/// so the file line and the POST /run response cannot drift apart. The seq is assigned
/// (and inserted first) by runlog::append_run; the API reply inserts it the same way.
/// A submission that never started (outcome "refused") keeps the classic fields it can
/// honestly carry and drops the rest, exactly like the skipped and missed shapes.
pub fn ran_record(
    trigger: &str,
    occurrence: Option<&str>,
    attempt: u32,
    attempts: u32,
    run_id: Option<u64>,
    out: &runner::RunOutcome,
) -> Map<String, Value> {
    let refused = out.error.is_some() && out.exit_code.is_none() && out.ms == 0 && !out.timed_out;
    let mut rec = Map::new();
    rec.insert("at".into(), json!(log::iso_now()));
    rec.insert("trigger".into(), json!(trigger));
    rec.insert("ok".into(), json!(out.ok));
    rec.insert("ms".into(), json!(out.ms));
    rec.insert(
        "outcome".into(),
        json!(if refused { "refused" } else { "ran" }),
    );
    if let Some(key) = occurrence {
        rec.insert("occurrenceKey".into(), json!(key));
    }
    rec.insert("attempt".into(), json!(attempt));
    rec.insert("attempts".into(), json!(attempts));
    if let Some(id) = run_id {
        rec.insert("runId".into(), json!(id));
    }
    if let Some(pid) = out.pid {
        rec.insert("pid".into(), json!(pid));
    }
    if !refused {
        // outcome != "ran" carries no exit code or output (docs/11 §7.3); a refused
        // submission never had either.
        if let Some(code) = out.exit_code {
            rec.insert("exitCode".into(), json!(code));
        }
        if out.output_truncated {
            rec.insert("preview".into(), json!(true));
        }
        rec.insert("output".into(), json!(out.output));
        rec.insert("chars".into(), json!(out.chars));
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
    rec
}

/// Test-only wiring: the shared services with the legacy command capability registered —
/// exactly what the process plugin's start does in a real boot, without a plugin host.
#[cfg(test)]
pub(crate) fn test_services() -> Arc<RuntimeServices> {
    let services = RuntimeServices::new();
    services
        .actions
        .register(Arc::new(
            swiss_host::services::actions::LegacyCommandAction::new(services.supervisor.clone()),
        ))
        .expect("the legacy command capability registers once");
    services
}
#[cfg(test)]
mod tests {
    use super::*;
    use clock::Clock as _;

    fn scratch_path(name: &str) -> PathBuf {
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-jobs-{}-{}",
            name,
            swiss_core::util::random_hex(8)
        ));
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
            env: None,
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
    fn interval_occurrences_anchor_on_last_run_or_boot() {
        let clock = clock::testing::FakeClock::at(0);
        let now = 1_000_000_000_i64 * 1000;
        let anchor = now - 5000;
        let d = def::JobDefinition::from_v1(&interval_job("j", 10));
        // Nothing due before the first full period past the anchor...
        assert_eq!(
            def::missed_occurrences(&d, anchor + 9000, anchor, None, clock.as_ref()),
            None
        );
        // ...due exactly at it (the newest occurrence is the anchor + every).
        assert_eq!(
            def::missed_occurrences(&d, anchor + 10_000, anchor, None, clock.as_ref()),
            Some((1, anchor + 10_000))
        );
        // A last run inside the period pushes the next occurrence a full period out.
        assert_eq!(
            def::missed_occurrences(&d, now, anchor, Some(now - 4000), clock.as_ref()),
            None,
            "4000ms into a 10s period: not due"
        );
        assert_eq!(
            def::missed_occurrences(&d, now, anchor, Some(now - 11_000), clock.as_ref()),
            Some((1, now - 1_000)),
            "11s since the last run: one occurrence, at the 10s mark past the anchor"
        );
        // Three shut-down periods are THREE due occurrences, newest last - the misfire
        // half of the story (docs/11 §6.3) starts from this count.
        assert_eq!(
            def::missed_occurrences(&d, now, anchor, Some(now - 31_000), clock.as_ref()),
            Some((3, now - 1_000))
        );
        // firstRun: "immediate" on a never-run job anchors at 0: due the first tick.
        let mut fast = def::JobDefinition::from_v1(&interval_job("fast", 3_600));
        if let def::Trigger::Interval { first_run, .. } = &mut fast.trigger {
            *first_run = def::FirstRun::Immediate;
        }
        assert_eq!(
            def::missed_occurrences(&fast, now, anchor, None, clock.as_ref()),
            Some((1, now)),
            "immediate: due NOW, one occurrence, next firing a full period out"
        );
    }

    #[test]
    fn cron_occurrences_count_matching_minutes_and_never_the_same_one_twice() {
        let clock = clock::testing::FakeClock::at(0);
        // 03:30 local on 2026-09-08; the fake zone is UTC+60, so real = 02:30Z.
        let now_local = chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
            .unwrap()
            .and_hms_opt(3, 30, 5)
            .unwrap();
        let now = clock.ms_from_local(&now_local).expect("a real minute");
        let d = def::JobDefinition::from_v1(&cron_job("j", "30 3 * * *"));
        // A run claimed this very minute: one occurrence due, and the occurrence KEY
        // is the local wall minute - the restart-in-the-same-minute guard (§6.1).
        let boot = now - 5_000;
        let (count, latest) = def::missed_occurrences(&d, now, boot, None, clock.as_ref())
            .expect("due at its minute");
        assert_eq!((count, latest), (1, now - (now % 60_000)));
        assert_eq!(
            def::occurrence_key_of(&d.trigger, latest, clock.as_ref()),
            format!("cron:{}", now_local.format("%Y-%m-%dT%H:%M"))
        );
        // Same local minute, one day back: exactly one occurrence since.
        let yesterday = now - 24 * 60 * 60 * 1000;
        assert_eq!(
            def::missed_occurrences(&d, now, 0, Some(yesterday), clock.as_ref()),
            Some((1, now - (now % 60_000)))
        );
        // One minute PAST the firing, still unclaimed since yesterday: the occurrence
        // is due (an unclaimed firing is due - whether to catch up is the misfire
        // policy's call, not the calendar's).
        let off = chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
            .unwrap()
            .and_hms_opt(3, 31, 0)
            .unwrap();
        let off_ms = clock.ms_from_local(&off).expect("a real minute");
        assert_eq!(
            def::missed_occurrences(&d, off_ms, 0, Some(yesterday), clock.as_ref()),
            Some((1, now - (now % 60_000))),
            "the just-missed 03:30 occurrence is due once"
        );
        // The same minute CLAIMED: nothing due until tomorrow.
        assert_eq!(
            def::missed_occurrences(&d, off_ms, 0, Some(now), clock.as_ref()),
            None,
            "this minute's firing already ran"
        );
        let next = def::next_due_ms(&d, Some(yesterday), off_ms, 0, clock.as_ref())
            .expect("tomorrow exists");
        assert_eq!(
            clock
                .local_from_ms(next)
                .format("%Y-%m-%dT%H:%M")
                .to_string(),
            "2026-09-09T03:30",
            "the next firing is the next matching LOCAL minute"
        );
    }

    #[test]
    fn a_manual_definition_never_auto_fires_and_has_no_next_due() {
        let clock = clock::testing::FakeClock::at(0);
        let d = def::JobDefinition {
            trigger: def::Trigger::Manual,
            ..def::JobDefinition::from_v1(&interval_job("by-hand", 60))
        };
        assert_eq!(
            def::missed_occurrences(&d, i64::MAX, 0, None, clock.as_ref()),
            None
        );
        assert_eq!(def::next_due_ms(&d, None, 0, 0, clock.as_ref()), None);
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

        // Inject the clock (§6.6) and walk time: 1.5s past boot, the 1s interval is
        // due, the 1h one is not, and neither the disabled nor the manual row ever fires.
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 1500);
        assert_eq!(
            sys.due_jobs(fake.now_ms()),
            vec!["due-now".to_string()],
            "only the elapsed interval sits in the table's past"
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
            sys.due_jobs(fake.now_ms()).len(),
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
            swiss_host::services::runs::DEFAULT_MAX_CONCURRENT_RUNS
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
        assert_eq!(page[0]["attempt"], json!(1), "manual is one attempt");
        assert_eq!(page[0]["attempts"], json!(1));
        assert!(
            page[0].get("occurrenceKey").is_none(),
            "no occurrence key on a manual run"
        );

        // A fresh boot over the same tree reads the same facts: the config row is
        // re-applied the way plugin create does, and the state file carries the run
        // facts across the boundary (docs/11 §4).
        let reopened = JobSystem::open(path, test_services(), store.clone());
        reopened
            .apply_config(&store.plugin_config("jobs"))
            .expect("the persisted row re-applies");
        let st = reopened.run_state("echo-job");
        assert_eq!(st.last_ok, Some(true));
        assert_eq!(
            st.last_run_at, None,
            "a manual run never moves the scheduling anchor (docs/11 §6.1) - only an
            occurrence does, so Run now cannot delay the next timer firing"
        );
        assert_eq!(
            st.last_occurrence_key, None,
            "and it never writes an occurrence key"
        );
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
        // "Busy" is the coordinator's label count now (docs/11 §6.2): occupy the
        // job:<id> label with a manual submission of our own, the way any producer
        // would, and the manual door refuses with Busy instead of double-running.
        let hog = sys
            .services
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "job:busy-job".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": long_command() }),
                timeout_ms: 30_000,
                queue_if_busy: false,
            })
            .expect("the label is free");
        match sys.clone().execute("busy-job", "manual").await {
            Err(RunError::Busy(msg)) => assert!(msg.contains("already running"), "{msg}"),
            other => panic!("expected Busy, got {other:?}"),
        }
        assert_eq!(sys.services.runs.shutdown_all().await, 1);
        assert!(hog.done.await.is_ok());
    }

    /// The process plugin is disabled: the capability is simply not registered. A job that
    /// comes due must SAY so - a missing dependency is reported, never silently skipped -
    /// and the listing reports it as not action-available while remaining saved.
    #[tokio::test]
    async fn a_missing_capability_is_reported_recorded_and_listed() {
        let (sys, store) = system("nocap", RuntimeServices::new());
        install(&sys, &store, row_of(&[interval_job("orphan", 3_600)]));
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100); // the interval has elapsed

        // The timer path is an OCCURRENCE now (docs/11 §6): it claims (stamping the
        // anchor and the occurrence key) and then hits the missing capability - the
        // refusal is a record, not a silent skip, and the anchor moved so it will not
        // re-fire every tick against the same wall.
        sys.clone().run_occurrence("orphan").await;

        let (page, _) = sys.read_runs("orphan", None, 10);
        assert_eq!(page.len(), 1, "{page:?}");
        assert_eq!(page[0]["ok"], json!(false));
        assert_eq!(page[0]["outcome"], json!("refused"), "{page:?}");
        assert!(page[0]["occurrenceKey"]
            .as_str()
            .is_some_and(|k| k.starts_with("interval:")));
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
        swiss_core::secure::key::use_test_master_key();
        let dir =
            std::env::temp_dir().join(format!("swiss-jobs-gone-{}", swiss_core::util::random_hex(8)));
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

    // ---- S5: scheduling semantics (docs/11 §6, all injected-clock) ----

    /// docs/11 §6.6: a cron minute inside the spring-forward gap never happens on the
    /// wall clock. The next-due scan skips it to the next REAL matching minute, and the
    /// missed walk does not count it.
    #[test]
    fn a_spring_forward_gap_minute_is_skipped_not_waited_for() {
        let naive = chrono::NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let r = naive.and_utc().timestamp_millis();
        // Shift the zone so the [spring+1h, spring+2h) gap covers exactly that minute.
        let spring = r - 90 * 60 * 1000;
        let clock = Arc::new(clock::testing::FakeClock::with_rule(
            0,
            spring,
            spring + 30 * 86_400_000,
        ));
        let d = def::JobDefinition::from_v1(&cron_job("early", "30 2 * * *"));

        assert_eq!(
            clock.ms_from_local(&naive),
            None,
            "02:30 does not exist that day"
        );

        // next_due from the day before lands on the NEXT day's 02:30, not the gap.
        let day_before = naive - chrono::TimeDelta::try_days(1).unwrap();
        let now = clock.ms_from_local(&day_before).unwrap();
        let next = def::next_due_ms(&d, None, now, now, clock.as_ref()).expect("a next firing");
        assert_eq!(
            clock
                .local_from_ms(next)
                .format("%Y-%m-%dT%H:%M")
                .to_string(),
            "2026-03-09T02:30",
            "the nonexistent minute is skipped"
        );

        // The missed walk at 03:00 the gap day finds NOTHING due: yesterday's ran, and
        // today's minute never existed.
        let after = chrono::NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        let now = clock.ms_from_local(&after).unwrap();
        let prev_firing = clock
            .ms_from_local(&(naive - chrono::TimeDelta::try_days(1).unwrap()))
            .expect("yesterday's 02:30 exists");
        assert_eq!(
            def::missed_occurrences(&d, now, now, Some(prev_firing), clock.as_ref()),
            None,
            "the gap minute is not a missed occurrence: yesterday's ran, today's never existed"
        );
    }

    /// docs/11 §6.1/§6.6: the fall-back's repeated local minute has ONE occurrence key,
    /// so the second wall-clock pass of that minute is the same occurrence - already
    /// claimed, never run twice.
    #[test]
    fn a_fall_back_repeated_minute_runs_once_by_occurrence_key() {
        let naive = chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        let n = naive.and_utc().timestamp_millis();
        // The repeated locals are [fall+1h, fall+2h); put 01:30 inside: fall = n-90m.
        let fall = n - 90 * 60 * 1000;
        let clock = Arc::new(clock::testing::FakeClock::with_rule(
            0,
            fall - 86_400_000,
            fall,
        ));
        let d = def::JobDefinition::from_v1(&cron_job("late", "30 1 * * *"));

        // The same wall minute happens at two real instants: once in the +120 segment
        // (real = local - 2h) and once after the fall (real = local - 1h).
        let first_pass = n - 2 * 60 * 60 * 1000;
        let second_pass = n - 60 * 60 * 1000;
        assert_eq!(
            clock::minute_floor(clock.local_from_ms(first_pass)),
            clock::minute_floor(clock.local_from_ms(second_pass)),
            "one wall minute, two real instants"
        );
        // ...and therefore has ONE occurrence key: claiming the first pass wins, and
        // the second pass has nothing due - the next firing is a day away.
        assert_eq!(
            def::occurrence_key_of(&d.trigger, first_pass, clock.as_ref()),
            def::occurrence_key_of(&d.trigger, second_pass, clock.as_ref())
        );
        assert_eq!(
            def::missed_occurrences(&d, second_pass, 0, Some(first_pass), clock.as_ref()),
            None,
            "already claimed this occurrence: no refire on the second pass"
        );
    }

    /// The shared shape of the misfire tests: a job that ran once at the anchor, then the
    /// gateway was down for three days.
    async fn misfire_system(
        name: &str,
        misfire: &str,
    ) -> (Arc<JobSystem>, Arc<clock::testing::FakeClock>) {
        let (sys, store) = system(name, test_services());
        install(
            &sys,
            &store,
            json!({ "definitions": {
                "hourly": {
                    "trigger": { "kind": "interval", "everyMs": 3600000 },
                    "action": { "type": "process.legacy-command", "input": { "command": "cmd /c echo hi" } },
                    "timeoutMs": 30000,
                    "misfire": misfire
                }
            } }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        // It ran once, at boot, before the downtime.
        sys.state
            .claim("hourly", sys.anchor_ms, Some("interval:boot"));
        // Three days of downtime, plus a partial period.
        fake.set(sys.anchor_ms + 72 * 3_600_000 + 1_800_000);
        (sys, fake)
    }

    /// docs/11 §6.3: skip - ONE summary line, NO catch-up run, and the anchor pushed to
    /// the newest occurrence so the next firing is a full period out.
    #[tokio::test]
    async fn misfire_skip_summarizes_and_never_catches_up() {
        let (sys, fake) = misfire_system("misfire-skip", "skip").await;
        sys.clone().run_occurrence("hourly").await;

        let (page, _) = sys.read_runs("hourly", None, 10);
        assert_eq!(page.len(), 1, "exactly the summary line: {page:?}");
        assert_eq!(page[0]["outcome"], json!("missed"));
        assert_eq!(
            page[0]["missedCount"],
            json!(71),
            "72 due, 1 summarized as the newest"
        );
        assert!(page[0].get("exitCode").is_none() && page[0].get("output").is_none());

        let st = sys.run_state("hourly");
        assert_eq!(st.last_run_at, Some(sys.anchor_ms + 72 * 3_600_000));
        // The next firing is one full period past the NEWEST occurrence, and the tick's
        // map already reflects it: nothing re-fires while the fake clock stands still.
        assert!(sys.due_jobs(fake.now_ms()).is_empty());
    }

    /// docs/11 §6.3: run-once - the SAME one summary line, plus exactly one catch-up run
    /// carrying the newest occurrence's key.
    #[tokio::test]
    async fn misfire_run_once_catches_up_exactly_one() {
        let (sys, _fake) = misfire_system("misfire-runonce", "run-once").await;
        sys.clone().run_occurrence("hourly").await;

        let (page, _) = sys.read_runs("hourly", None, 10);
        assert_eq!(page.len(), 2, "summary + one run: {page:?}");
        let missed = &page[1];
        assert_eq!(missed["outcome"], json!("missed"));
        assert_eq!(missed["missedCount"], json!(71));
        let ran = &page[0];
        assert_eq!(ran["outcome"], json!("ran"));
        assert_eq!(ran["ok"], json!(true));
        assert_eq!(ran["attempt"], json!(1), "a catch-up run is not a retry");
        assert!(
            ran["occurrenceKey"]
                .as_str()
                .is_some_and(|k| k == missed["occurrenceKey"].as_str().unwrap()),
            "the run carries the summarized occurrence's own key"
        );
    }

    /// docs/11 §6.2: overlap=skip records a skipped occurrence and still moves the anchor
    /// - the invariant that keeps a busy job from re-firing every tick.
    #[tokio::test]
    async fn overlap_skip_records_and_moves_the_anchor() {
        let (sys, store) = system("overlap-skip", test_services());
        install(&sys, &store, row_of(&[interval_job("solo", 3_600)]));
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        // Occupy the job:<id> label the way any producer would.
        let hog = sys
            .services
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "job:solo".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": long_command() }),
                timeout_ms: 30_000,
                queue_if_busy: false,
            })
            .expect("the label is free");

        sys.clone().run_occurrence("solo").await;
        let (page, _) = sys.read_runs("solo", None, 10);
        assert_eq!(page.len(), 1, "one skipped line, no run: {page:?}");
        assert_eq!(page[0]["outcome"], json!("skipped"));
        assert_eq!(page[0]["reason"], json!("overlap"));
        // The anchor moved: the occurrence counted even though it did not run.
        assert!(
            sys.due_jobs(fake.now_ms()).is_empty(),
            "a skipped occurrence is still consumed - no per-tick refire"
        );
        assert_eq!(sys.services.runs.shutdown_all().await, 1);
        assert!(hog.done.await.is_ok());
    }

    /// docs/11 §6.2: overlap=queue-one queues the successor behind the busy pool, and a
    /// FULL queue is a visible capacity skip, never a silent drop.
    #[tokio::test]
    async fn overlap_queue_one_queues_and_a_full_queue_skips_visibly() {
        let (sys, store) = system("queue-one", test_services());
        install(
            &sys,
            &store,
            json!({
                "maxConcurrentRuns": 1,
                "maxQueuedRuns": 1,
                "definitions": {
                    "queued": {
                        "trigger": { "kind": "interval", "everyMs": 3600000 },
                        "action": { "type": "process.legacy-command", "input": { "command": "cmd /c echo hi" } },
                        "timeoutMs": 30000,
                        "overlap": "queue-one"
                    }
                }
            }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        // Fill the single slot AND the single queue slot with another producer.
        let busy = sys.services.clone();
        let hog = busy
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "hog".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": long_command() }),
                timeout_ms: 30_000,
                queue_if_busy: true,
            })
            .expect("the slot");
        let second = busy
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "hog2".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": long_command() }),
                timeout_ms: 30_000,
                queue_if_busy: true,
            })
            .expect("the queue slot");

        sys.clone().run_occurrence("queued").await;
        let (page, _) = sys.read_runs("queued", None, 10);
        assert_eq!(
            page.len(),
            1,
            "the full queue is one visible skip: {page:?}"
        );
        assert_eq!(page[0]["outcome"], json!("skipped"));
        assert_eq!(page[0]["reason"], json!("capacity"));

        // Free everything, then a fresh occurrence queues BEHIND a short-lived hog:
        // the successor must RUN when the slot frees, not merely be parked. (Only the
        // ACTIVE run is cancellable; the queued manual hog is dropped from the queue
        // instead, so that cleanup reports one cancellation.)
        assert_eq!(sys.services.runs.shutdown_all().await, 1);
        let _ = hog.done.await;
        let _ = second.done.await;
        let short_hog = busy
            .runs
            .submit(SubmitRequest {
                owner: "manual".into(),
                label: "hog3".into(),
                action_type: JOBS_ACTION.into(),
                input: json!({ "command": "cmd /c echo hog" }),
                timeout_ms: 30_000,
                queue_if_busy: false,
            })
            .expect("the slot again");
        fake.set(sys.anchor_ms + 2 * 3_600_000 + 100);
        let runner = {
            let sys = sys.clone();
            tokio::spawn(async move { sys.run_occurrence("queued").await })
        };
        // The successor runs as soon as the short hog exits; wait for its record.
        let mut ran = None;
        for _ in 0..300 {
            let (page, _) = sys.read_runs("queued", None, 10);
            if let Some(rec) = page.iter().find(|r| r["outcome"] == json!("ran")) {
                ran = Some(rec.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _ = short_hog.done.await;
        let _ = runner.await;
        let ran = ran.expect("the queued successor ran when the slot freed");
        assert_eq!(ran["ok"], json!(true), "{ran}");
        assert_eq!(ran["attempt"], json!(1));
    }

    /// docs/11 §6.4: retry to maxAttempts inside one occurrence - each attempt its own
    /// record - and lastOk settles ONCE, on the final attempt.
    #[tokio::test]
    async fn retry_runs_to_max_attempts_inside_one_occurrence() {
        let (sys, store) = system("retry", test_services());
        install(
            &sys,
            &store,
            json!({ "definitions": {
                "flaky": {
                    "trigger": { "kind": "interval", "everyMs": 3600000 },
                    "action": { "type": "process.legacy-command", "input": { "command": "cmd /c exit 3" } },
                    "timeoutMs": 30000,
                    "retry": { "maxAttempts": 3, "delayMs": 20 }
                }
            } }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        sys.clone().run_occurrence("flaky").await;
        let (page, _) = sys.read_runs("flaky", None, 10);
        assert_eq!(page.len(), 3, "three attempts, three records: {page:?}");
        for (i, rec) in page.iter().enumerate() {
            let expect_attempt = (3 - i) as u32; // newest first
            assert_eq!(rec["attempt"], json!(expect_attempt), "{rec:?}");
            assert_eq!(rec["attempts"], json!(3));
            assert_eq!(rec["ok"], json!(false));
            assert_eq!(rec["exitCode"], json!(3));
            assert_eq!(
                rec["occurrenceKey"], page[0]["occurrenceKey"],
                "every attempt belongs to the same occurrence"
            );
        }
        let st = sys.run_state("flaky");
        assert_eq!(st.last_ok, Some(false));
        assert_eq!(
            st.consecutive_failures, 1,
            "one occurrence failed, not three runs"
        );
    }

    /// docs/11 §6.4: a timeout the policy does not name is final on attempt 1.
    #[tokio::test]
    async fn a_timeout_the_policy_does_not_name_is_final() {
        let (sys, store) = system("retry-timeout", test_services());
        let command = long_command();
        install(
            &sys,
            &store,
            json!({ "definitions": {
                "wedged": {
                    "trigger": { "kind": "interval", "everyMs": 3600000 },
                    "action": { "type": "process.legacy-command", "input": { "command": command } },
                    "timeoutMs": 1500,
                    "retry": { "maxAttempts": 3, "delayMs": 20, "retryOn": ["failure"] }
                }
            } }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        sys.clone().run_occurrence("wedged").await;
        let (page, _) = sys.read_runs("wedged", None, 10);
        assert_eq!(
            page.len(),
            1,
            "timedOut is not on the retryOn list: {page:?}"
        );
        assert_eq!(page[0]["timedOut"], json!(true));
    }

    /// docs/11 §6.4: canceled never retries - someone asked for it to stop.
    #[tokio::test]
    async fn a_canceled_attempt_never_retries() {
        let (sys, store) = system("retry-cancel", test_services());
        let command = long_command();
        install(
            &sys,
            &store,
            json!({ "definitions": {
                "stopping": {
                    "trigger": { "kind": "interval", "everyMs": 3600000 },
                    "action": { "type": "process.legacy-command", "input": { "command": command } },
                    "timeoutMs": 30000,
                    "retry": { "maxAttempts": 3, "delayMs": 20 }
                }
            } }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        let running = {
            let sys = sys.clone();
            tokio::spawn(async move { sys.run_occurrence("stopping").await })
        };
        assert!(
            wait_running(&sys, "stopping").await,
            "the attempt reached the pool"
        );
        assert_eq!(sys.shutdown().await, 1, "the attempt was cancelled");
        running.await.expect("the occurrence task");
        let (page, _) = sys.read_runs("stopping", None, 10);
        assert_eq!(page.len(), 1, "canceled ends the occurrence: {page:?}");
        assert_eq!(page[0]["canceled"], json!(true));
    }

    /// docs/11 §6.4: the delay between attempts is cancellable - stopping the scheduler
    /// mid-delay ends the occurrence instead of coming back to life later.
    #[tokio::test]
    async fn stopping_mid_delay_aborts_the_occurrence() {
        let (sys, store) = system("retry-delay", test_services());
        install(
            &sys,
            &store,
            json!({ "definitions": {
                "flaky": {
                    "trigger": { "kind": "interval", "everyMs": 3600000 },
                    "action": { "type": "process.legacy-command", "input": { "command": "cmd /c exit 3" } },
                    "timeoutMs": 30000,
                    "retry": { "maxAttempts": 2, "delayMs": 120_000 }
                }
            } }),
        );
        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 3_600_000 + 100);

        let started = std::time::Instant::now();
        let running = {
            let sys = sys.clone();
            tokio::spawn(async move { sys.run_occurrence("flaky").await })
        };
        // Wait for attempt 1's record, then stop while the occurrence sits in its delay.
        for _ in 0..300 {
            let (page, _) = sys.read_runs("flaky", None, 10);
            if page.len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        sys.stop(); // the cancel signal, without cancelling any run
        running
            .await
            .expect("the occurrence task ends with the delay");
        assert!(
            started.elapsed().as_secs() < 30,
            "it did not wait the 120s delay out"
        );
        let (page, _) = sys.read_runs("flaky", None, 10);
        assert_eq!(page.len(), 1, "attempt 2 never happened: {page:?}");
    }

    /// docs/11 §6.5: the per-second tick is a map scan. A table of a thousand jobs,
    /// every one of them overdue, costs ZERO calendar calls to enumerate; only a claim
    /// walks the calendar again, for exactly that job.
    #[test]
    fn the_tick_is_a_map_scan_not_a_calendar_walk() {
        let (sys, store) = system("budget", test_services());
        let mut defs = Map::new();
        for i in 0..500 {
            let d = def::JobDefinition::from_v1(&interval_job(&format!("job-{i}"), 3_600));
            defs.insert(format!("job-{i}"), d.to_config_json());
        }
        install(&sys, &store, json!({ "definitions": defs }));

        let fake = clock::testing::FakeClock::at(sys.anchor_ms);
        sys.set_clock(fake.clone());
        fake.set(sys.anchor_ms + 7_200_000 + 100); // every job overdue

        let before = fake.local_calls.load(std::sync::atomic::Ordering::SeqCst);
        let due = sys.due_jobs(fake.now_ms());
        let during = fake.local_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(due.len(), 500, "the whole overdue table is due");
        assert_eq!(before, during, "enumerating it cost zero calendar calls");

        // One occurrence task touches the calendar a bounded number of times for ITS
        // job only - never for the other 999.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            sys.clone().run_occurrence("job-0").await;
        });
        let after = fake.local_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            during <= after && after - during <= 16,
            "one claim is a handful of calls, not a walk: {after} - {during}"
        );
    }
}
