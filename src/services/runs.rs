//! The shared run registry — bounded, owner-scoped, first-wins (docs/09 §3: "Jobs only
//! owns scheduling, it does not monopolize background execution"; docs/10 §2: the
//! lightweight RunRegistry / RunCoordinator is the shared run service, and the jobs
//! scheduler is one producer among several).
//!
//! Every execution — a scheduled job (producer: "jobs"), a manual API run (producer:
//! "manual"), a future plugin's work — is registered HERE with its owner, its provider
//! action and its cancellation source. Three properties are load-bearing:
//!
//! - BOUNDED: at most max_concurrent runs execute and at most max_queued runs wait; a
//!   submission past either bound is a VISIBLE capacity error, never a silently growing
//!   task set.
//! - OWNER-SCOPED: cancellation and shutdown name an owner. Stopping Jobs cancels the runs
//!   Jobs started; a manual run someone else submitted keeps running. Detached work nobody
//!   owns does not exist here — every run's task is tracked until it completes.
//! - FIRST-WINS: a run reaches exactly ONE terminal state. A cancel landing after a natural
//!   finish is an idempotent no-op that reports the finished view; racers (deadline,
//!   cancel, exit) cannot both write the outcome.
//!
//! Cancellation is cooperative-by-contract and verified: the coordinator only accepts
//! actions whose cancelable() is true (a capability that cannot stop would hold a
//! concurrency slot forever — exactly the wedge the deadline exists to prevent), and a
//! cancel AWAITS the run task, so when it returns the child is reaped and the pipe readers
//! are joined. Dropping a JoinHandle and hoping is not cancellation here.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::services::action::{ActionError, ActionOutcome, ActionRegistry, CancelSource};
use crate::util::now_ms;

/// Finished runs kept in memory for the API (bounded history: the ring holds at most this
/// many recent outcomes, each with its already-capped output).
const FINISHED_RING: usize = 32;

/// The docs' safe defaults for the shared pool; the jobs config reconciler overrides these
/// with the plugin's maxConcurrentRuns / maxQueuedRuns when Jobs is configured.
pub const DEFAULT_MAX_CONCURRENT_RUNS: usize = 2;
pub const DEFAULT_MAX_QUEUED_RUNS: usize = 32;

/// The shared pool's shape. Bounds are strict: a run past either bound is refused.
#[derive(Debug, Clone, Copy)]
pub struct RunCapacity {
    pub max_concurrent: usize,
    pub max_queued: usize,
}

impl Default for RunCapacity {
    fn default() -> Self {
        RunCapacity {
            max_concurrent: DEFAULT_MAX_CONCURRENT_RUNS,
            max_queued: DEFAULT_MAX_QUEUED_RUNS,
        }
    }
}

/// One execution request. Small by design: the queue holds these while waiting, so the
/// input must be a definition-sized value, not a payload dump.
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    /// Who owns this run ("jobs" for the scheduler, "manual" for API runs). Cancellation
    /// and shutdown are scoped by this exact string.
    pub owner: String,
    /// Human-facing name (job id). Also the overlap key the scheduler checks.
    pub label: String,
    /// Capability id, resolved through the registry at start time.
    pub action_type: String,
    pub input: Value,
    /// Deadline policy in milliseconds, enforced through cancellation (the action stops
    /// cleanly and reports; the outcome is marked timed-out).
    pub timeout_ms: u64,
    /// overlap=queue-one: when the pool is busy, hold ONE successor instead of refusing.
    pub queue_if_busy: bool,
}

/// Why a submission did not start. A capacity refusal is data the caller must show the
/// user: manual requests get a visible capacity error, they never pretend to have run.
#[derive(Debug, Clone)]
pub enum SubmitError {
    /// The pool (or the queue, for queue-one) is full.
    Capacity(String),
    /// The action type is unknown, or it declared itself non-cancelable.
    Action(String),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::Capacity(m) | SubmitError::Action(m) => write!(f, "{m}"),
        }
    }
}

/// A run's lifecycle state. Terminal: succeeded / failed / timeout / canceled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Canceled,
}

impl RunState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Running => "running",
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
            RunState::TimedOut => "timeout",
            RunState::Canceled => "canceled",
        }
    }
}

/// One run as the API sees it. Output TEXT is carried only where a caller asks for it
/// (single-run reads and the finished ring), so a listing cannot fan out every tail again.
#[derive(Debug, Clone)]
pub struct RunView {
    pub run_id: u64,
    pub owner: String,
    pub label: String,
    pub action_type: String,
    pub state: RunState,
    pub queued_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub ms: Option<u64>,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    pub error: Option<String>,
    pub output: Option<String>,
    pub chars: Option<usize>,
    pub output_truncated: bool,
}

impl RunView {
    /// The wire shape (absent-not-null). With include_output=false the output TEXT is left
    /// out but its size evidence (chars / truncated) still is.
    pub fn to_json(&self, include_output: bool) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("runId".into(), json!(self.run_id));
        m.insert("owner".into(), json!(self.owner));
        m.insert("label".into(), json!(self.label));
        m.insert("action".into(), json!(self.action_type));
        m.insert("state".into(), json!(self.state.as_str()));
        m.insert("queuedAt".into(), json!(iso_of_ms(self.queued_at_ms)));
        if let Some(ms) = self.started_at_ms {
            m.insert("startedAt".into(), json!(iso_of_ms(ms)));
        }
        if let Some(ms) = self.ended_at_ms {
            m.insert("endedAt".into(), json!(iso_of_ms(ms)));
        }
        if let Some(ms) = self.ms {
            m.insert("ms".into(), json!(ms));
        }
        if let Some(pid) = self.pid {
            m.insert("pid".into(), json!(pid));
        }
        if let Some(code) = self.exit_code {
            m.insert("exitCode".into(), json!(code));
        }
        if self.timed_out {
            m.insert("timedOut".into(), json!(true));
        }
        if self.canceled {
            m.insert("canceled".into(), json!(true));
        }
        if let Some(err) = &self.error {
            m.insert("error".into(), json!(err));
        }
        if let Some(n) = self.chars {
            m.insert("chars".into(), json!(n));
        }
        if self.output_truncated {
            m.insert("outputTruncated".into(), json!(true));
        }
        if include_output {
            if let Some(out) = &self.output {
                m.insert("output".into(), json!(out));
            }
        }
        Value::Object(m)
    }
}

/// Unix-ms stamp as an RFC3339 UTC string — the same shape every other timestamp here
/// uses, so API clients parse run times with the util they already parse log times with.
fn iso_of_ms(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// What submit() hands back: the run id plus the one-shot that resolves with the FINAL
/// view — first-wins, whatever the terminal state turned out to be.
#[derive(Debug)]
pub struct SubmittedRun {
    pub run_id: u64,
    pub done: oneshot::Receiver<RunView>,
}

struct ActiveRun {
    owner: String,
    label: String,
    action_type: String,
    queued_at_ms: u64,
    started_at_ms: u64,
    cancel: Arc<CancelSource>,
    join: JoinHandle<RunView>,
}

struct QueuedRun {
    run_id: u64,
    request: SubmitRequest,
    queued_at_ms: u64,
    done: oneshot::Sender<RunView>,
}

#[derive(Default)]
struct Inner {
    capacity: RunCapacity,
    next_id: u64,
    active: HashMap<u64, ActiveRun>,
    queued: VecDeque<QueuedRun>,
    finished: VecDeque<RunView>,
}

/// The coordinator. Shared as one Arc between RuntimeServices, the jobs scheduler and the
/// API handlers.
pub struct RunCoordinator {
    registry: Arc<ActionRegistry>,
    inner: Mutex<Inner>,
}

impl RunCoordinator {
    pub fn new(registry: Arc<ActionRegistry>) -> Arc<Self> {
        Arc::new(RunCoordinator {
            registry,
            inner: Mutex::new(Inner {
                capacity: RunCapacity::default(),
                ..Inner::default()
            }),
        })
    }

    /// The pool's current bounds.
    pub fn capacity(&self) -> RunCapacity {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).capacity
    }

    /// Re-aim the pool's bounds (the jobs reconciler does this on config apply). Lowering
    /// bounds never kills in-flight work — it only gates NEW submissions; a raised bound
    /// dispatches anything that was waiting.
    pub fn set_capacity(self: &Arc<Self>, capacity: RunCapacity) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.capacity = capacity;
        }
        self.dispatch_queue();
    }

    /// How many runs are active or queued under one label (any owner) — the overlap gate
    /// the scheduler checks for skip / queue-one.
    pub fn count_for_label(&self, label: &str) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.active.values().filter(|a| a.label == label).count()
            + inner.queued.iter().filter(|q| q.request.label == label).count()
    }

    /// Register + start (or queue) one run. Returns immediately with the run id — an HTTP
    /// request must never own a background task's lifetime (docs/10 §7).
    pub fn submit(self: &Arc<Self>, request: SubmitRequest) -> Result<SubmittedRun, SubmitError> {
        let action = self.registry.get(&request.action_type).ok_or_else(|| {
            SubmitError::Action(format!(
                "unknown action type {:?} (see GET /api/actions)",
                request.action_type
            ))
        })?;
        // A capability that cannot stop would pin a concurrency slot past its deadline —
        // exactly the wedge the timeout policy exists to prevent. Refuse honestly.
        if !action.cancelable() {
            return Err(SubmitError::Action(format!(
                "action {:?} is not cancelable and cannot run under a deadline policy",
                request.action_type
            )));
        }
        let (done_tx, done_rx) = oneshot::channel();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = inner.next_id;
        inner.next_id += 1;
        let queued_at = now_ms();
        if inner.active.len() < inner.capacity.max_concurrent {
            self.start_locked(&mut inner, run_id, request.clone(), queued_at, done_tx);
        } else if request.queue_if_busy {
            if inner.queued.len() >= inner.capacity.max_queued {
                return Err(SubmitError::Capacity(format!(
                    "run queue is full ({}/{}); overlap policy queue-one cannot enqueue another successor",
                    inner.queued.len(),
                    inner.capacity.max_queued
                )));
            }
            inner.queued.push_back(QueuedRun {
                run_id,
                request,
                queued_at_ms: queued_at,
                done: done_tx,
            });
        } else {
            return Err(SubmitError::Capacity(format!(
                "run capacity is full ({}/{} running); retry later or raise maxConcurrentRuns",
                inner.active.len(),
                inner.capacity.max_concurrent
            )));
        }
        Ok(SubmittedRun {
            run_id,
            done: done_rx,
        })
    }

    /// Spawn the tracked execution task. Caller holds the inner lock. The receiver is
    /// &Arc<Self> because the task needs an owned handle back to self for its bookkeeping.
    fn start_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        run_id: u64,
        request: SubmitRequest,
        queued_at_ms: u64,
        done: oneshot::Sender<RunView>,
    ) {
        let cancel = Arc::new(CancelSource::new());
        let started_at = now_ms();
        let owner = request.owner.clone();
        let label = request.label.clone();
        let action_type = request.action_type.clone();
        let join = {
            let coordinator = self.clone();
            // The task owns one cancel handle half; the active-bookkeeping entry keeps the
            // other so a later cancel() can reach the run. One source, two observers.
            let cancel_for_task = cancel.clone();
            tokio::spawn(async move {
                coordinator
                    .execute_run(run_id, request, queued_at_ms, started_at, cancel_for_task, done)
                    .await
            })
        };
        inner.active.insert(
            run_id,
            ActiveRun {
                owner,
                label,
                action_type,
                queued_at_ms,
                started_at_ms: started_at,
                cancel,
                join,
            },
        );
    }

    /// One run's whole life: resolve the action, race execution against the deadline
    /// (through CANCELLATION, never by dropping the future), record the terminal view
    /// exactly once, and dispatch the queue. A tracked task on the shared runtime.
    async fn execute_run(
        self: Arc<Self>,
        run_id: u64,
        request: SubmitRequest,
        queued_at_ms: u64,
        started_at_ms: u64,
        cancel: Arc<CancelSource>,
        done: oneshot::Sender<RunView>,
    ) -> RunView {
        let (result, timed_out) = drive_action(&self.registry, &request, &cancel).await;
        let ended_at = now_ms();
        let mut view = RunView {
            run_id,
            owner: request.owner.clone(),
            label: request.label.clone(),
            action_type: request.action_type.clone(),
            state: RunState::Failed,
            queued_at_ms,
            started_at_ms: Some(started_at_ms),
            ended_at_ms: Some(ended_at),
            ms: Some(ended_at.saturating_sub(started_at_ms)),
            pid: None,
            exit_code: None,
            timed_out,
            canceled: false,
            error: None,
            output: None,
            chars: None,
            output_truncated: false,
        };
        match result {
            Ok(outcome) => {
                view.pid = outcome.pid;
                view.exit_code = outcome.exit_code;
                view.error = outcome.error;
                view.chars = Some(outcome.chars);
                view.output = Some(outcome.output);
                view.output_truncated = outcome
                    .meta
                    .get("outputTruncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // The deadline layer owns the timeout truth; the action's own canceled
                // flag only counts as a cancel when the deadline did not cause it.
                view.canceled = outcome.canceled && !timed_out;
                view.state = if timed_out {
                    RunState::TimedOut
                } else if outcome.canceled {
                    RunState::Canceled
                } else if outcome.ok {
                    RunState::Succeeded
                } else {
                    RunState::Failed
                };
            }
            Err(err) => {
                view.error = Some(err.to_string());
                view.state = if timed_out {
                    RunState::TimedOut
                } else {
                    RunState::Failed
                };
            }
        }
        self.finish(run_id, view.clone());
        // The submitter may already be gone (fire-and-forget scheduled runs) — that is
        // fine; the finished ring is the durable record either way.
        let _ = done.send(view.clone());
        view
    }

    /// Bookkeeping when a run reaches its terminal state: leave the active set, enter the
    /// bounded finished ring, dispatch whatever was waiting for the freed slot.
    fn finish(self: &Arc<Self>, run_id: u64, view: RunView) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.active.remove(&run_id);
            inner.finished.push_front(view);
            inner.finished.truncate(FINISHED_RING);
        }
        self.dispatch_queue();
    }

    /// Enter the finished ring without touching the active set (queued runs canceled in
    /// place).
    fn push_finished(&self, view: RunView) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.finished.push_front(view);
        inner.finished.truncate(FINISHED_RING);
    }

    /// Start queued runs while the pool has room, FIFO.
    fn dispatch_queue(self: &Arc<Self>) {
        loop {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.active.len() >= inner.capacity.max_concurrent {
                return;
            }
            let Some(QueuedRun {
                run_id,
                request,
                queued_at_ms,
                done,
            }) = inner.queued.pop_front()
            else {
                return;
            };
            self.start_locked(&mut inner, run_id, request, queued_at_ms, done);
        }
    }

    /// Cancel one run and WAIT for it to actually finish (child reaped, readers joined).
    /// Idempotent under first-wins: a finished run returns its finished view unchanged.
    pub async fn cancel(&self, run_id: u64) -> Option<RunView> {
        let active = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.active.remove(&run_id)
        };
        if let Some(active) = active {
            active.cancel.cancel();
            return match active.join.await {
                Ok(view) => Some(view),
                Err(_) => {
                    // The task itself died abnormally (runtime shutdown); the ring may
                    // still hold what it recorded before that.
                    self.inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .finished
                        .iter()
                        .find(|v| v.run_id == run_id)
                        .cloned()
                }
            };
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = inner.queued.iter().position(|q| q.run_id == run_id) {
            let QueuedRun {
                run_id,
                request,
                queued_at_ms,
                done,
            } = inner.queued.remove(pos).expect("position just checked");
            let view = queued_canceled_view(run_id, &request, queued_at_ms);
            drop(inner);
            let _ = done.send(view.clone());
            self.push_finished(view.clone());
            return Some(view);
        }
        // Not active, not queued: finished (first-wins no-op) or unknown.
        inner
            .finished
            .iter()
            .find(|v| v.run_id == run_id)
            .cloned()
    }

    /// Cancel every run OWNED by `owner` and wait for them to finish; queued runs owned by
    /// it are dropped as canceled. Runs owned by others are untouched — stopping one
    /// producer must never take down another producer's work.
    pub async fn cancel_owner(self: &Arc<Self>, owner: &str) -> usize {
        self.cancel_some(|run_owner, _action| run_owner == owner).await
    }

    /// Cancel EVERYTHING (RuntimeServices::shutdown): all active runs are canceled and
    /// awaited, all queued runs are dropped as canceled. Returns how many were affected.
    pub async fn shutdown_all(self: &Arc<Self>) -> usize {
        self.cancel_some(|_, _| true).await
    }

    /// Cancel every run EXECUTING one of these capabilities — what a capability provider's
    /// stop does to work already in flight, after the registry withdrew the action so no new
    /// run can pick it up. Scoped by action, not by owner: the producers (jobs, manual) keep
    /// their other runs.
    pub async fn cancel_action_types(self: &Arc<Self>, types: &[&str]) -> usize {
        self.cancel_some(|_owner, action| types.contains(&action)).await
    }

    async fn cancel_some(
        self: &Arc<Self>,
        matches: impl Fn(&str, &str) -> bool,
    ) -> usize {
        let targets: Vec<(Arc<CancelSource>, JoinHandle<RunView>)> = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<u64> = inner
                .active
                .iter()
                .filter(|(_, a)| matches(&a.owner, &a.action_type))
                .map(|(id, _)| *id)
                .collect();
            ids.iter()
                .filter_map(|id| {
                    inner.active.remove(id).map(|a| (a.cancel, a.join))
                })
                .collect()
        };
        let mut count = targets.len();
        // Await each: when this returns, every child is reaped and every reader joined.
        for (cancel, join) in targets {
            cancel.cancel();
            let _ = join.await;
        }
        // Drop matching queued runs as canceled.
        let mut canceled_queued: Vec<RunView> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut kept = VecDeque::new();
            while let Some(q) = inner.queued.pop_front() {
                if matches(&q.request.owner, &q.request.action_type) {
                    count += 1;
                    let view = queued_canceled_view(q.run_id, &q.request, q.queued_at_ms);
                    let _ = q.done.send(view.clone());
                    canceled_queued.push(view);
                } else {
                    kept.push_back(q);
                }
            }
            inner.queued = kept;
        }
        for view in canceled_queued {
            self.push_finished(view);
        }
        count
    }

    /// Every run (queued, active, finished — newest first). Output text omitted; fetch a
    /// single run for its tail.
    pub fn list(&self) -> Vec<RunView> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut views: Vec<RunView> = Vec::with_capacity(
            inner.queued.len() + inner.active.len() + inner.finished.len(),
        );
        for q in &inner.queued {
            views.push(RunView {
                run_id: q.run_id,
                owner: q.request.owner.clone(),
                label: q.request.label.clone(),
                action_type: q.request.action_type.clone(),
                state: RunState::Queued,
                queued_at_ms: q.queued_at_ms,
                started_at_ms: None,
                ended_at_ms: None,
                ms: None,
                pid: None,
                exit_code: None,
                timed_out: false,
                canceled: false,
                error: None,
                output: None,
                chars: None,
                output_truncated: false,
            });
        }
        for (run_id, a) in &inner.active {
            views.push(RunView {
                run_id: *run_id,
                owner: a.owner.clone(),
                label: a.label.clone(),
                action_type: a.action_type.clone(),
                state: RunState::Running,
                queued_at_ms: a.queued_at_ms,
                started_at_ms: Some(a.started_at_ms),
                ended_at_ms: None,
                ms: None,
                pid: None,
                exit_code: None,
                timed_out: false,
                canceled: false,
                error: None,
                output: None,
                chars: None,
                output_truncated: false,
            });
        }
        views.extend(inner.finished.iter().cloned());
        views
    }

    /// One run by id, WITH its output tail when it has one.
    pub fn get(&self, run_id: u64) -> Option<RunView> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = inner.queued.iter().find(|q| q.run_id == run_id) {
            return Some(RunView {
                run_id,
                owner: q.request.owner.clone(),
                label: q.request.label.clone(),
                action_type: q.request.action_type.clone(),
                state: RunState::Queued,
                queued_at_ms: q.queued_at_ms,
                started_at_ms: None,
                ended_at_ms: None,
                ms: None,
                pid: None,
                exit_code: None,
                timed_out: false,
                canceled: false,
                error: None,
                output: None,
                chars: None,
                output_truncated: false,
            });
        }
        if let Some(a) = inner.active.get(&run_id) {
            return Some(RunView {
                run_id,
                owner: a.owner.clone(),
                label: a.label.clone(),
                action_type: a.action_type.clone(),
                state: RunState::Running,
                queued_at_ms: a.queued_at_ms,
                started_at_ms: Some(a.started_at_ms),
                ended_at_ms: None,
                ms: None,
                pid: None,
                exit_code: None,
                timed_out: false,
                canceled: false,
                error: None,
                output: None,
                chars: None,
                output_truncated: false,
            });
        }
        inner.finished.iter().find(|v| v.run_id == run_id).cloned()
    }
}

/// Race one action against the deadline WITHOUT ever dropping the mid-flight future: the
/// deadline cancels, then the loop WAITS for the action's clean stop (for process actions
/// that is the tree kill plus reader joins, bounded by their drain grace). A panicking
/// action becomes a failed run, never a poisoned scheduler.
async fn drive_action(
    registry: &ActionRegistry,
    request: &SubmitRequest,
    cancel: &Arc<CancelSource>,
) -> (Result<ActionOutcome, ActionError>, bool) {
    let Some(action) = registry.get(&request.action_type) else {
        return (
            Err(ActionError::Failed(format!(
                "action {:?} disappeared from the registry",
                request.action_type
            ))),
            false,
        );
    };
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
    let exec = AssertUnwindSafe(action.execute(&request.input, cancel.handle())).catch_unwind();
    tokio::pin!(exec);
    let mut timed_out = false;
    let res = tokio::select! {
        biased;
        res = &mut exec => res,
        _ = tokio::time::sleep_until(deadline) => {
            timed_out = true;
            cancel.cancel();
            (&mut exec).await
        }
    };
    match res {
        Ok(outcome) => (outcome, timed_out),
        Err(_) => (
            Err(ActionError::Failed(
                "action panicked; the run was terminated".into(),
            )),
            timed_out,
        ),
    }
}

/// The view for a queued run that never started (canceled / shutdown while waiting).
fn queued_canceled_view(run_id: u64, request: &SubmitRequest, queued_at_ms: u64) -> RunView {
    RunView {
        run_id,
        owner: request.owner.clone(),
        label: request.label.clone(),
        action_type: request.action_type.clone(),
        state: RunState::Canceled,
        queued_at_ms,
        started_at_ms: None,
        ended_at_ms: Some(now_ms()),
        ms: None,
        pid: None,
        exit_code: None,
        timed_out: false,
        canceled: true,
        error: Some("canceled while queued".into()),
        output: None,
        chars: None,
        output_truncated: false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::action::{Action, CancelHandle};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A run that lasts until its `ms` elapse or the handle is cancelled — the shape of
    /// every long capability, without a child process in the way.
    struct SleepAction {
        starts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Action for SleepAction {
        fn type_name(&self) -> &'static str {
            "test.sleep"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(
            &self,
            input: &Value,
            cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let ms = input.get("ms").and_then(Value::as_u64).unwrap_or(30_000);
            let mut out = ActionOutcome::ok();
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    out.ok = false;
                    out.canceled = true;
                }
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {
                    out.output = "slept".into();
                    out.chars = out.output.len();
                }
            }
            Ok(out)
        }
    }

    /// A capability that admits it cannot stop early.
    struct StubbornAction;

    #[async_trait]
    impl Action for StubbornAction {
        fn type_name(&self) -> &'static str {
            "test.stubborn"
        }
        fn cancelable(&self) -> bool {
            false
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(&self, _i: &Value, _c: CancelHandle) -> Result<ActionOutcome, ActionError> {
            Ok(ActionOutcome::ok())
        }
    }

    /// A capability that panics mid-run: one broken action must not poison the pool.
    struct PanicAction;

    #[async_trait]
    impl Action for PanicAction {
        fn type_name(&self) -> &'static str {
            "test.panic"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(&self, _i: &Value, _c: CancelHandle) -> Result<ActionOutcome, ActionError> {
            panic!("the capability blew up");
        }
    }

    fn coordinator(capacity: RunCapacity) -> (Arc<RunCoordinator>, Arc<AtomicUsize>) {
        let starts = Arc::new(AtomicUsize::new(0));
        let registry = ActionRegistry::new();
        registry
            .register(Arc::new(SleepAction {
                starts: starts.clone(),
            }))
            .expect("register sleep");
        registry
            .register(Arc::new(StubbornAction))
            .expect("register stubborn");
        registry.register(Arc::new(PanicAction)).expect("register panic");
        let coordinator = RunCoordinator::new(Arc::new(registry));
        coordinator.set_capacity(capacity);
        (coordinator, starts)
    }

    fn request(owner: &str, label: &str, ms: u64) -> SubmitRequest {
        SubmitRequest {
            owner: owner.into(),
            label: label.into(),
            action_type: "test.sleep".into(),
            input: json!({ "ms": ms }),
            timeout_ms: 30_000,
            queue_if_busy: false,
        }
    }

    /// Give the spawned run task a turn: submit() returns before its task has run at all.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_run_reaches_one_terminal_view_and_stays_readable() {
        let (runs, starts) = coordinator(RunCapacity {
            max_concurrent: 2,
            max_queued: 4,
        });
        let submitted = runs.submit(request("manual", "quick", 1)).expect("submitted");
        let view = submitted.done.await.expect("the final view");
        assert_eq!(view.state, RunState::Succeeded);
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        // Readable afterwards, with its output tail, and listed exactly once.
        let stored = runs.get(view.run_id).expect("in the finished ring");
        assert_eq!(stored.state, RunState::Succeeded);
        assert_eq!(stored.output.as_deref(), Some("slept"));
        let listed: Vec<u64> = runs.list().iter().map(|v| v.run_id).collect();
        assert_eq!(listed, vec![view.run_id]);
    }

    #[tokio::test]
    async fn capacity_is_a_visible_refusal_not_a_growing_task_set() {
        let (runs, starts) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let _first = runs
            .submit(request("manual", "long", 30_000))
            .expect("first runs");
        settle().await;

        // Second submission, no queue-one: refused, and NOTHING was started for it.
        let err = runs
            .submit(request("manual", "other", 30_000))
            .expect_err("refused");
        assert!(matches!(err, SubmitError::Capacity(_)), "{err}");
        assert!(err.to_string().contains("maxConcurrentRuns"), "{err}");

        // queue-one against a zero-length queue is refused just as visibly.
        let mut queued = request("manual", "other", 30_000);
        queued.queue_if_busy = true;
        let err = runs.submit(queued).expect_err("refused");
        assert!(err.to_string().contains("queue is full"), "{err}");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "only the first ever started");
        runs.shutdown_all().await;
    }

    #[tokio::test]
    async fn a_queued_successor_starts_when_the_slot_frees() {
        let (runs, starts) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 1,
        });
        let first = runs
            .submit(request("jobs", "nightly", 30_000))
            .expect("first runs");
        settle().await;
        let mut successor = request("jobs", "nightly", 1);
        successor.queue_if_busy = true;
        let second = runs.submit(successor).expect("queued");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "the successor waits");
        assert_eq!(
            runs.get(second.run_id).map(|v| v.state),
            Some(RunState::Queued)
        );
        // Both count against the label: that is the overlap gate the scheduler reads.
        assert_eq!(runs.count_for_label("nightly"), 2);

        // Freeing the slot dispatches the queue in FIFO order.
        let canceled = runs.cancel(first.run_id).await.expect("the first view");
        assert_eq!(canceled.state, RunState::Canceled);
        let view = second.done.await.expect("the successor ran");
        assert_eq!(view.state, RunState::Succeeded);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(runs.count_for_label("nightly"), 0);
    }

    #[tokio::test]
    async fn cancellation_is_scoped_to_the_owner_that_asked() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 4,
            max_queued: 4,
        });
        let scheduled = runs
            .submit(request("jobs", "nightly", 30_000))
            .expect("submitted");
        let manual = runs
            .submit(request("manual", "by-hand", 30_000))
            .expect("submitted");
        settle().await;

        // Stopping the Jobs plugin cancels what Jobs started — and only that.
        assert_eq!(runs.cancel_owner("jobs").await, 1);
        assert_eq!(scheduled.done.await.expect("view").state, RunState::Canceled);
        assert_eq!(
            runs.get(manual.run_id).map(|v| v.state),
            Some(RunState::Running)
        );

        // Shutdown takes the rest.
        assert_eq!(runs.shutdown_all().await, 1);
        assert_eq!(manual.done.await.expect("view").state, RunState::Canceled);
    }

    #[tokio::test]
    async fn a_cancel_after_the_finish_reports_the_finished_view() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let submitted = runs.submit(request("manual", "quick", 1)).expect("submitted");
        let run_id = submitted.run_id;
        let view = submitted.done.await.expect("finished");
        assert_eq!(view.state, RunState::Succeeded);

        // First-wins: the late cancel does not rewrite the outcome, and it does not fail.
        let late = runs.cancel(run_id).await.expect("still known");
        assert_eq!(
            late.state,
            RunState::Succeeded,
            "the terminal state is written once"
        );
        assert!(
            runs.cancel(9_999).await.is_none(),
            "an unknown run is simply unknown"
        );
    }

    #[tokio::test]
    async fn a_deadline_ends_the_run_as_timed_out_not_as_canceled() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let mut req = request("jobs", "slow", 30_000);
        req.timeout_ms = 30;
        let view = runs.submit(req).expect("submitted").done.await.expect("view");
        assert_eq!(view.state, RunState::TimedOut);
        assert!(view.timed_out);
        assert!(
            !view.canceled,
            "the deadline owns the timeout truth, not the action's own flag"
        );
    }

    #[tokio::test]
    async fn unknown_and_uncancelable_actions_are_refused_at_submit() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let mut unknown = request("manual", "x", 1);
        unknown.action_type = "test.nope".into();
        let err = runs.submit(unknown).expect_err("no such capability");
        assert!(matches!(err, SubmitError::Action(_)), "{err}");
        assert!(err.to_string().contains("/api/actions"), "{err}");

        let mut stubborn = request("manual", "x", 1);
        stubborn.action_type = "test.stubborn".into();
        let err = runs.submit(stubborn).expect_err("cannot honour a deadline");
        assert!(err.to_string().contains("not cancelable"), "{err}");
        assert!(
            runs.list().is_empty(),
            "a refused submission leaves no run behind"
        );
    }

    #[tokio::test]
    async fn a_panicking_action_is_one_failed_run_not_a_poisoned_pool() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let mut boom = request("manual", "boom", 1);
        boom.action_type = "test.panic".into();
        let view = runs.submit(boom).expect("submitted").done.await.expect("view");
        assert_eq!(view.state, RunState::Failed);
        assert!(view.error.unwrap_or_default().contains("panicked"));

        // The slot was released and the pool still works.
        let next = runs.submit(request("manual", "after", 1)).expect("submitted");
        assert_eq!(next.done.await.expect("view").state, RunState::Succeeded);
    }
}
