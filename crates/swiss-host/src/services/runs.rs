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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::services::action::{
    ActionContext, ActionError, ActionOutcome, ActionRegistry, CancelSource, RunOutputSink,
    RunOutputTee,
};
use serde_json::Map;
use swiss_core::util::now_ms;

/// Finished runs kept in memory for the API (bounded history: the ring holds at most this
/// many recent outcomes, each with its already-capped output).
const FINISHED_RING: usize = 32;

/// The live-output ceiling while a run is active (docs/34 §17). 256 KiB is a few hundred
/// build lines either side of "now" — enough for a follower to see progress, small
/// enough that a dozen concurrent runs cost single-digit megabytes worst case.
pub const MAX_LIVE_OUTPUT_BYTES: usize = 256 * 1024;

/// What a finished run's buffer keeps for later reads. Compacting on finish bounds the
/// finished history (32 runs x 64 KiB = 2 MiB worst case) instead of holding every
/// finished run's full live window forever.
pub const KEEP_FINISHED_OUTPUT_BYTES: usize = 64 * 1024;

/// The largest chunk one output read returns, so a single poll cannot materialise the
/// whole retained window into one response and one String.
pub const MAX_OUTPUT_READ_BYTES: usize = 128 * 1024;

/// A durable record of runs, provided by the plugin whose runs must outlive the process
/// (2026-09-18, the remote run log): the coordinator itself keeps the 32-view ring in
/// memory and nothing else. A sink sees a run START — and may attach a tee that receives
/// every output append — and then its terminal view, once. Where that goes, for how long
/// and under what budget is the sink's business, so the host never learns a directory.
/// Registered by a plugin's start and removed by its stop, like every other seat.
pub trait RunHistorySink: Send + Sync {
    /// A run is about to execute (queued runs are not announced). `Some(tee)` records
    /// it: the tee receives the output stream and `finish` follows with the terminal
    /// view. `None` leaves the run to the in-memory ring alone.
    fn begin(&self, run_id: u64, request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>>;
    /// The terminal view of a run `begin` accepted — called before the view enters the
    /// finished ring, so a client that sees the run terminal can already read the record.
    fn finish(&self, view: &RunView, request: &SubmitRequest);
    /// The highest run id this record holds, so a coordinator whose numbering started
    /// afresh (a first boot after the sequence file existed, a lost one) never hands out
    /// an id the record already keys on. None when the record is empty or keys on
    /// something else.
    fn last_run_id(&self) -> Option<u64> {
        None
    }
}

/// One run's bounded live output: a monotonic byte cursor over everything ever appended,
/// with the oldest data evicted when the cap is hit. A follower polls with the cursor its
/// previous read ended on; a cursor older than what is still retained reads the oldest
/// kept data with `truncated = true`, never a silent gap.
pub struct RunOutputBuffer {
    inner: Mutex<OutputInner>,
    terminal: AtomicBool,
}

#[derive(Default)]
struct OutputInner {
    data: VecDeque<u8>,
    /// The cursor value of `data.front()` — everything before it was evicted.
    start: u64,
    /// Bytes ever appended; also the cursor the NEXT append starts at.
    total: u64,
}

impl RunOutputBuffer {
    pub fn new() -> Arc<Self> {
        Arc::new(RunOutputBuffer {
            inner: Mutex::new(OutputInner::default()),
            terminal: AtomicBool::new(false),
        })
    }

    /// Append bytes, evicting the oldest past the live cap. Never blocks, never fails:
    /// dropping old bytes IS the policy, and the cursor makes the drop visible.
    pub fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.data.extend(bytes.iter().copied());
        inner.total = inner.total.saturating_add(bytes.len() as u64);
        let overflow = inner.data.len().saturating_sub(MAX_LIVE_OUTPUT_BYTES);
        if overflow > 0 {
            inner.data.drain(..overflow);
            inner.start = inner.start.saturating_add(overflow as u64);
        }
    }

    /// Everything appended so far — the cursor a follower passes on its next poll.
    pub fn total_bytes(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).total
    }

    /// The run reached its terminal state: freeze the retained window down to the
    /// finished-keep cap so history stays cheap. Reads with a cursor inside the evicted
    /// range keep working, flagged truncated.
    pub fn finish(&self) {
        self.terminal.store(true, Ordering::SeqCst);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let overflow = inner.data.len().saturating_sub(KEEP_FINISHED_OUTPUT_BYTES);
        if overflow > 0 {
            inner.data.drain(..overflow);
            inner.start = inner.start.saturating_add(overflow as u64);
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::SeqCst)
    }

    /// Read from `after` (a previous next-cursor). Returns the text, the cursor to poll
    /// with next, and whether the requested range had already been partially evicted.
    ///
    /// The window ends on a character boundary (docs/41 U1): a multi-byte character the
    /// `max` cut would split is held back and the next cursor points at its first byte,
    /// so the follower sees it whole on its next poll instead of two U+FFFD. A window
    /// that starts inside a character (an evicted ring, a cursor that was never ours)
    /// skips to the next boundary. Neither trim can stall a follower: when the run is
    /// terminal and the held-back bytes are the stream's last, or when `max` is smaller
    /// than one character, the bytes go out as they are and the decoder marks them.
    pub fn read_from(&self, after: u64, max_bytes: usize) -> RunOutputChunk {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let max = max_bytes.clamp(1, MAX_OUTPUT_READ_BYTES);
        // A caller ahead of the stream (or at its end) reads nothing, not an error.
        let from = after.min(inner.total);
        let truncated = from < inner.start;
        let begin = from.max(inner.start) as usize;
        let begin = begin.saturating_sub(inner.start as usize);
        let end = (begin + max).min(inner.data.len());
        let mut bytes = inner.data.range(begin..end).copied().collect::<Vec<u8>>();
        let mut begin = begin;
        if truncated {
            let skip = swiss_core::utf8::char_boundary_start(&bytes);
            bytes.drain(..skip);
            begin += skip;
        }
        let mut end = begin + bytes.len();
        let at_stream_end = end == inner.data.len();
        let keep = swiss_core::utf8::char_boundary_end(&bytes);
        // Hold the partial character back only when more bytes can still arrive (or
        // already sit past `max`) and the window keeps something: a terminal stream's
        // last bytes and a sub-character `max` both go out as they are.
        let more_coming = !at_stream_end || !self.terminal.load(Ordering::SeqCst);
        if keep < bytes.len() && more_coming && keep > 0 {
            bytes.truncate(keep);
            end = begin + keep;
        }
        let next = inner.start + end as u64;
        RunOutputChunk {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            cursor: from,
            next_cursor: next,
            truncated,
        }
    }
}

/// One bounded read out of a [RunOutputBuffer].
#[derive(Debug, Clone)]
pub struct RunOutputChunk {
    pub text: String,
    /// The cursor this read was asked to start from (echoed for the client's bookkeeping).
    pub cursor: u64,
    /// Poll with this next. Equal to `cursor` when there was nothing new.
    pub next_cursor: u64,
    /// True when `cursor` named data older than what is still retained — the read
    /// started at the oldest kept byte instead, and the client knows it missed some.
    pub truncated: bool,
}

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
    /// Who asked (docs/41 A1): `cli:<user>@<host>` (self-declared over loopback),
    /// `mcp:<token label>` (the authenticating token), `panel`, `jobs`, or `api` for any
    /// other /api/runs caller. Recorded with the run; the audit trail's "who" column.
    pub actor: String,
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

    /// Whether no further output can arrive — what an output follower polls for.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunState::Succeeded | RunState::Failed | RunState::TimedOut | RunState::Canceled
        )
    }
}

/// One run as the API sees it. Output TEXT is carried only where a caller asks for it
/// (single-run reads and the finished ring), so a listing cannot fan out every tail again.
#[derive(Debug, Clone)]
pub struct RunView {
    pub run_id: u64,
    pub owner: String,
    pub label: String,
    pub actor: String,
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
    /// The action's own extras (a remote run's target and endpoint, a sync's counts) —
    /// what the outcome carried, so a run row can say WHAT ran without re-reading input.
    pub meta: Map<String, Value>,
}

impl RunView {
    /// The wire shape (absent-not-null). With include_output=false the output TEXT is left
    /// out but its size evidence (chars / truncated) still is.
    pub fn to_json(&self, include_output: bool) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("runId".into(), json!(self.run_id));
        m.insert("owner".into(), json!(self.owner));
        m.insert("label".into(), json!(self.label));
        m.insert("actor".into(), json!(self.actor));
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
        if !self.meta.is_empty() {
            m.insert("meta".into(), Value::Object(self.meta.clone()));
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
    actor: String,
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
    /// Where `next_id` is kept across restarts (see [RunCoordinator::persist_sequence]);
    /// None in the bare services and in tests, where numbering starts at 0 each process.
    sequence: Option<PathBuf>,
    active: HashMap<u64, ActiveRun>,
    queued: VecDeque<QueuedRun>,
    finished: VecDeque<RunView>,
    /// Live output buffers for every run this coordinator knows (active and queued).
    outputs: HashMap<u64, Arc<RunOutputBuffer>>,
    /// The buffers of finished runs, retired alongside the finished ring (same bound;
    /// each already compacted to KEEP_FINISHED_OUTPUT_BYTES).
    finished_outputs: VecDeque<(u64, Arc<RunOutputBuffer>)>,
}

/// The coordinator. Shared as one Arc between RuntimeServices, the jobs scheduler and the
/// API handlers.
pub struct RunCoordinator {
    registry: Arc<ActionRegistry>,
    inner: Mutex<Inner>,
    /// The history sinks (usually none, at most one per plugin that keeps a record).
    /// Its own lock: a sink is consulted at run start, outside the bookkeeping lock.
    history: Mutex<Vec<Arc<dyn RunHistorySink>>>,
}

impl RunCoordinator {
    /// The action registry this coordinator resolves submissions against — shared with
    /// the composition root's RuntimeServices, exposed for route handlers and tests that
    /// register new capabilities into the same pool.
    pub fn actions(&self) -> &Arc<ActionRegistry> {
        &self.registry
    }

    pub fn new(registry: Arc<ActionRegistry>) -> Arc<Self> {
        Arc::new(RunCoordinator {
            registry,
            inner: Mutex::new(Inner {
                capacity: RunCapacity::default(),
                ..Inner::default()
            }),
            history: Mutex::new(Vec::new()),
        })
    }

    /// Attach a history sink (a plugin's start). Runs already executing are not
    /// announced to it — a record begins with the next run to start.
    pub fn add_history_sink(&self, sink: Arc<dyn RunHistorySink>) {
        // The record's highest id is a floor under the numbering: whatever this process
        // started counting from, the next run gets a number the record has never seen.
        if let Some(last) = sink.last_run_id() {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.next_id <= last {
                inner.next_id = last + 1;
                if let Some(path) = inner.sequence.clone() {
                    write_sequence(&path, inner.next_id);
                }
            }
        }
        let mut sinks = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if !sinks.iter().any(|s| Arc::ptr_eq(s, &sink)) {
            sinks.push(sink);
        }
    }

    /// Continue the run numbering from `path` across restarts: the file holds the next
    /// id, every allocation rewrites it (tmp + rename, so a crash mid-write leaves the
    /// previous value, never a torn one). Without it a run id is a per-process counter
    /// and a record keyed by run id - the remote log - carried two runs under one number
    /// after every restart (seen on 19998, 2026-09-21). A missing or unreadable file
    /// starts from wherever the counter is; the history sinks' floor covers the rest.
    pub fn persist_sequence(&self, path: PathBuf) {
        let stored = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.next_id = inner.next_id.max(stored);
        inner.sequence = Some(path);
    }

    /// The next id this coordinator will hand out (tests and the sequence's evidence).
    pub fn next_run_id(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).next_id
    }

    /// Detach a sink (a plugin's stop). A run it already accepted still gets its
    /// `finish`: the recorders were captured at start, so a record is never left open.
    pub fn remove_history_sink(&self, sink: &Arc<dyn RunHistorySink>) {
        let mut sinks = self.history.lock().unwrap_or_else(|e| e.into_inner());
        sinks.retain(|s| !Arc::ptr_eq(s, sink));
    }

    /// How many history sinks are attached — the lifecycle tests' evidence that a
    /// plugin's stop gave its seat back.
    pub fn history_sinks(&self) -> usize {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The sinks that want this run, each with the tee it handed back.
    fn history_begin(
        &self,
        run_id: u64,
        request: &SubmitRequest,
    ) -> Vec<(Arc<dyn RunHistorySink>, Arc<dyn RunOutputTee>)> {
        let sinks = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        sinks
            .into_iter()
            .filter_map(|sink| sink.begin(run_id, request).map(|tee| (sink, tee)))
            .collect()
    }

    /// The pool's current bounds.
    pub fn capacity(&self) -> RunCapacity {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capacity
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
            + inner
                .queued
                .iter()
                .filter(|q| q.request.label == label)
                .count()
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
        if let Some(path) = inner.sequence.clone() {
            write_sequence(&path, inner.next_id);
        }
        // The output buffer exists from the moment the run id is committed, so the
        // output route can follow a QUEUED run too (it simply has nothing yet).
        let output = RunOutputBuffer::new();
        inner.outputs.insert(run_id, output.clone());
        let queued_at = now_ms();
        if inner.active.len() < inner.capacity.max_concurrent {
            self.start_locked(
                &mut inner,
                run_id,
                request.clone(),
                queued_at,
                done_tx,
                output,
            );
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
        output: Arc<RunOutputBuffer>,
    ) {
        let cancel = Arc::new(CancelSource::new());
        let started_at = now_ms();
        let owner = request.owner.clone();
        let label = request.label.clone();
        let actor = request.actor.clone();
        let action_type = request.action_type.clone();
        let join = {
            let coordinator = self.clone();
            // The task owns one cancel handle half; the active-bookkeeping entry keeps the
            // other so a later cancel() can reach the run. One source, two observers.
            let cancel_for_task = cancel.clone();
            tokio::spawn(async move {
                coordinator
                    .execute_run(
                        run_id,
                        request,
                        queued_at_ms,
                        started_at,
                        cancel_for_task,
                        done,
                        output,
                    )
                    .await
            })
        };
        inner.active.insert(
            run_id,
            ActiveRun {
                owner,
                label,
                actor,
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
    // The run's whole life spans exactly these inputs; grouping them into a struct would
    // rename the same seven facts without hiding any of them.
    #[allow(clippy::too_many_arguments)]
    async fn execute_run(
        self: Arc<Self>,
        run_id: u64,
        request: SubmitRequest,
        queued_at_ms: u64,
        started_at_ms: u64,
        cancel: Arc<CancelSource>,
        done: oneshot::Sender<RunView>,
        output: Arc<RunOutputBuffer>,
    ) -> RunView {
        let recorders = self.history_begin(run_id, &request);
        let tees: Vec<Arc<dyn RunOutputTee>> = recorders.iter().map(|(_, t)| t.clone()).collect();
        let (result, timed_out) =
            drive_action(&self.registry, &request, &cancel, &output, tees).await;
        let ended_at = now_ms();
        let mut view = RunView {
            run_id,
            owner: request.owner.clone(),
            label: request.label.clone(),
            actor: request.actor.clone(),
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
            meta: Map::new(),
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
                view.meta = outcome.meta;
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
        // The durable record first, so the run is never terminal in the API before its
        // history row exists; the sinks were captured at start, so a plugin stop in
        // between cannot orphan a record.
        for (sink, _) in &recorders {
            sink.finish(&view, &request);
        }
        self.finish(run_id, view.clone());
        // The submitter may already be gone (fire-and-forget scheduled runs) — that is
        // fine; the finished ring is the durable record either way.
        let _ = done.send(view.clone());
        view
    }

    /// Bookkeeping when a run reaches its terminal state: leave the active set, enter the
    /// bounded finished ring, dispatch whatever was waiting for the freed slot.
    /// One bounded read of a run's live output (docs/34 §17): the text from `after`
    /// onward, the cursor to poll with next, and the run's state so a follower can stop
    /// polling the moment it goes terminal. None when the id is unknown — a run whose
    /// history has rotated out of the finished ring is gone, not empty.
    pub fn output(
        &self,
        run_id: u64,
        after: u64,
        max_bytes: usize,
    ) -> Option<(RunState, RunOutputChunk)> {
        let state = self.get(run_id)?.state;
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let buffer = inner.outputs.get(&run_id).cloned().or_else(|| {
            inner
                .finished_outputs
                .iter()
                .find(|(id, _)| *id == run_id)
                .map(|(_, b)| b.clone())
        })?;
        Some((state, buffer.read_from(after, max_bytes)))
    }

    fn finish(self: &Arc<Self>, run_id: u64, view: RunView) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.active.remove(&run_id);
            inner.finished.push_front(view);
            inner.finished.truncate(FINISHED_RING);
            retire_output_locked(&mut inner, run_id);
        }
        self.dispatch_queue();
    }

    /// Enter the finished ring without touching the active set (queued runs canceled in
    /// place).
    fn push_finished(&self, run_id: u64, view: RunView) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.finished.push_front(view);
        inner.finished.truncate(FINISHED_RING);
        retire_output_locked(&mut inner, run_id);
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
            // The queued run's buffer was created at submit; hand it over to the task.
            let output = inner
                .outputs
                .get(&run_id)
                .cloned()
                .unwrap_or_else(RunOutputBuffer::new);
            self.start_locked(&mut inner, run_id, request, queued_at_ms, done, output);
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
            self.push_finished(run_id, view.clone());
            return Some(view);
        }
        // Not active, not queued: finished (first-wins no-op) or unknown.
        inner.finished.iter().find(|v| v.run_id == run_id).cloned()
    }

    /// Cancel every run OWNED by `owner` and wait for them to finish; queued runs owned by
    /// it are dropped as canceled. Runs owned by others are untouched — stopping one
    /// producer must never take down another producer's work.
    pub async fn cancel_owner(self: &Arc<Self>, owner: &str) -> usize {
        self.cancel_some(|run_owner, _action| run_owner == owner)
            .await
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
        self.cancel_some(|_owner, action| types.contains(&action))
            .await
    }

    async fn cancel_some(self: &Arc<Self>, matches: impl Fn(&str, &str) -> bool) -> usize {
        let targets: Vec<(Arc<CancelSource>, JoinHandle<RunView>)> = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<u64> = inner
                .active
                .iter()
                .filter(|(_, a)| matches(&a.owner, &a.action_type))
                .map(|(id, _)| *id)
                .collect();
            ids.iter()
                .filter_map(|id| inner.active.remove(id).map(|a| (a.cancel, a.join)))
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
            self.push_finished(view.run_id, view);
        }
        count
    }

    /// Every run (queued, active, finished — newest first). Output text omitted; fetch a
    /// single run for its tail.
    pub fn list(&self) -> Vec<RunView> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut views: Vec<RunView> =
            Vec::with_capacity(inner.queued.len() + inner.active.len() + inner.finished.len());
        for q in &inner.queued {
            views.push(RunView {
                run_id: q.run_id,
                owner: q.request.owner.clone(),
                label: q.request.label.clone(),
                actor: q.request.actor.clone(),
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
                meta: Map::new(),
            });
        }
        for (run_id, a) in &inner.active {
            views.push(RunView {
                run_id: *run_id,
                owner: a.owner.clone(),
                label: a.label.clone(),
                actor: a.actor.clone(),
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
                meta: Map::new(),
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
                actor: q.request.actor.clone(),
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
                meta: Map::new(),
            });
        }
        if let Some(a) = inner.active.get(&run_id) {
            return Some(RunView {
                run_id,
                owner: a.owner.clone(),
                label: a.label.clone(),
                actor: a.actor.clone(),
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
                meta: Map::new(),
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
    output: &Arc<RunOutputBuffer>,
    tees: Vec<Arc<dyn RunOutputTee>>,
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
    let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
    // The single execution seam (docs/34 §16): every action runs through
    // execute_with_context; actions that predate it simply never touch the sink.
    let ctx = ActionContext {
        cancel: cancel.handle(),
        output: RunOutputSink::new(output.clone(), tees),
    };
    let exec = AssertUnwindSafe(action.execute_with_context(&request.input, ctx)).catch_unwind();
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

/// Move one run's output buffer from the active map into the retired ring: mark it
/// terminal (which also compacts it to the finished-keep cap), then park it next to
/// the finished views. Called under the coordinator lock by every path that puts a
/// view into the ring, so a buffer can never outlive its run's history slot.
/// The sequence file's one write: the next id, tmp + rename. A failure is silent - the
/// worst case is the pre-restart numbering this exists to replace, and the sinks' floor
/// still holds.
fn write_sequence(path: &std::path::Path, next: u64) {
    let tmp = path.with_extension("seq.tmp");
    if std::fs::write(&tmp, next.to_string()).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn retire_output_locked(inner: &mut Inner, run_id: u64) {
    let Some(buffer) = inner.outputs.remove(&run_id) else {
        return;
    };
    buffer.finish();
    inner.finished_outputs.push_front((run_id, buffer));
    inner.finished_outputs.truncate(FINISHED_RING);
}
/// The view for a queued run that never started (canceled / shutdown while waiting).
fn queued_canceled_view(run_id: u64, request: &SubmitRequest, queued_at_ms: u64) -> RunView {
    RunView {
        run_id,
        owner: request.owner.clone(),
        label: request.label.clone(),
        actor: request.actor.clone(),
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
        meta: Map::new(),
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
        async fn execute(
            &self,
            _i: &Value,
            _c: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
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
        async fn execute(
            &self,
            _i: &Value,
            _c: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
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
        registry
            .register(Arc::new(PanicAction))
            .expect("register panic");
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
            actor: "test".to_string(),
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
        let submitted = runs
            .submit(request("manual", "quick", 1))
            .expect("submitted");
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
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "only the first ever started"
        );
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
        assert_eq!(
            scheduled.done.await.expect("view").state,
            RunState::Canceled
        );
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
        let submitted = runs
            .submit(request("manual", "quick", 1))
            .expect("submitted");
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
        let view = runs
            .submit(req)
            .expect("submitted")
            .done
            .await
            .expect("view");
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
        let view = runs
            .submit(boom)
            .expect("submitted")
            .done
            .await
            .expect("view");
        assert_eq!(view.state, RunState::Failed);
        assert!(view.error.unwrap_or_default().contains("panicked"));

        // The slot was released and the pool still works.
        let next = runs
            .submit(request("manual", "after", 1))
            .expect("submitted");
        assert_eq!(next.done.await.expect("view").state, RunState::Succeeded);
    }

    // --- live output (docs/34 §17) -------------------------------------------------------

    /// An action that appends to the live sink in bursts and finishes: the
    /// streaming-aware seam without any process or network in the way.
    struct StreamingAction;

    #[async_trait]
    impl Action for StreamingAction {
        fn type_name(&self) -> &'static str {
            "test.streaming"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn cancelable(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _input: &Value,
            _cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            unreachable!("the coordinator must call execute_with_context")
        }
        async fn execute_with_context(
            &self,
            _input: &Value,
            ctx: crate::services::action::ActionContext,
        ) -> Result<ActionOutcome, ActionError> {
            for word in ["hello", " ", "world", "\n"] {
                ctx.output.append(word);
                // Yield so a concurrent reader can observe the intermediate cursor.
                tokio::task::yield_now().await;
            }
            Ok(ActionOutcome::ok())
        }
    }

    /// A streaming action that PARKS between its two lines: the running-state read is
    /// then deterministic, not a scheduling race.
    struct GatedStream {
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Action for GatedStream {
        fn type_name(&self) -> &'static str {
            "test.gated"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(
            &self,
            _input: &Value,
            _cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            unreachable!("the coordinator must call execute_with_context")
        }
        async fn execute_with_context(
            &self,
            _input: &Value,
            ctx: crate::services::action::ActionContext,
        ) -> Result<ActionOutcome, ActionError> {
            ctx.output.append("hello");
            self.release.notified().await;
            ctx.output.append(" world\n");
            Ok(ActionOutcome::ok())
        }
    }

    #[tokio::test]
    async fn live_output_is_readable_while_the_run_runs_and_after_it_finishes() {
        let (runs, _) = coordinator(RunCapacity::default());
        let release = Arc::new(tokio::sync::Notify::new());
        runs.actions()
            .register(Arc::new(GatedStream {
                release: release.clone(),
            }))
            .expect("register");
        let mut r = request("manual", "gated", 30_000);
        r.action_type = "test.gated".into();
        let submitted = runs.submit(r).expect("submitted");
        // Park the action between its lines: the running read must see "hello" alone.
        let mut running_text = None;
        for _ in 0..200 {
            if let Some((state, chunk)) = runs.output(submitted.run_id, 0, usize::MAX) {
                if state == RunState::Running && !chunk.text.is_empty() {
                    running_text = Some(chunk.text.clone());
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            running_text.as_deref(),
            Some("hello"),
            "partial text mid-run"
        );
        release.notify_waiters();
        let view = submitted.done.await.expect("view");
        assert_eq!(view.state, RunState::Succeeded);
        let (_, chunk) = runs
            .output(submitted.run_id, 0, usize::MAX)
            .expect("output readable after finish");
        assert_eq!(chunk.text, "hello world\n");
        assert_eq!(chunk.next_cursor, "hello world\n".len() as u64);
        assert!(!chunk.truncated);
    }

    #[tokio::test]
    async fn the_cursor_returns_only_new_bytes() {
        let (runs, _) = coordinator(RunCapacity::default());
        runs.actions()
            .register(Arc::new(StreamingAction))
            .expect("register");
        let submitted = runs
            .submit(request("manual", "cursor", 1))
            .expect("submitted");
        let _ = submitted.done.await.expect("view");
        let (_, first) = runs.output(submitted.run_id, 0, usize::MAX).expect("first");
        let (_, again) = runs
            .output(submitted.run_id, first.next_cursor, usize::MAX)
            .expect("second");
        assert!(again.text.is_empty());
        assert_eq!(again.next_cursor, first.next_cursor);
        // A cursor past the end reads nothing rather than erroring, and echoes the cap.
        let (_, ahead) = runs
            .output(submitted.run_id, 999_999, usize::MAX)
            .expect("ahead");
        assert!(ahead.text.is_empty());
        assert_eq!(ahead.cursor, first.next_cursor);
    }

    #[test]
    fn an_evicted_window_reads_oldest_kept_bytes_and_says_truncated() {
        let buffer = RunOutputBuffer::new();
        let big = "x".repeat(MAX_LIVE_OUTPUT_BYTES + 10);
        buffer.append(big.as_bytes());
        // One read is capped by MAX_OUTPUT_READ_BYTES: the whole window takes two.
        let first = buffer.read_from(0, usize::MAX);
        assert!(first.truncated, "the first 10 bytes were evicted");
        assert_eq!(first.text.len(), MAX_OUTPUT_READ_BYTES);
        let second = buffer.read_from(first.next_cursor, usize::MAX);
        assert!(!second.truncated);
        assert_eq!(first.text.len() + second.text.len(), MAX_LIVE_OUTPUT_BYTES);
        assert_eq!(second.next_cursor, big.len() as u64);
        let follow = buffer.read_from(second.next_cursor, usize::MAX);
        assert!(!follow.truncated);
        assert!(follow.text.is_empty());
        // Finishing compacts to the finished-keep cap: even more is evicted, flagged.
        buffer.finish();
        let compact = buffer.read_from(10, usize::MAX);
        assert!(compact.truncated);
        assert_eq!(compact.text.len(), KEEP_FINISHED_OUTPUT_BYTES);
        assert!(buffer.is_terminal());
    }

    #[test]
    fn a_window_never_cuts_a_character_in_half_while_the_run_is_live() {
        // docs/41 U2: "日志" is 6 bytes; a 4-byte max would cut 志 after its first byte.
        let buffer = RunOutputBuffer::new();
        buffer.append("日志\n".as_bytes());
        let first = buffer.read_from(0, 4);
        assert_eq!(first.text, "日", "the partial 志 is held back");
        assert_eq!(first.next_cursor, 3, "the next poll starts at 志's first byte");
        let second = buffer.read_from(first.next_cursor, 64);
        assert_eq!(second.text, "志\n");
        assert!(!first.text.contains('\u{FFFD}') && !second.text.contains('\u{FFFD}'));
    }

    #[test]
    fn a_max_smaller_than_one_character_still_makes_progress() {
        let buffer = RunOutputBuffer::new();
        buffer.append("中".as_bytes());
        // Two bytes of a three-byte character: nothing complete fits, so the bytes go
        // out lossy rather than the follower polling forever at cursor 0.
        let read = buffer.read_from(0, 2);
        assert_eq!(read.next_cursor, 2);
        assert!(read.text.contains('\u{FFFD}'));
    }

    #[test]
    fn a_terminal_streams_last_partial_bytes_are_not_held_back() {
        let buffer = RunOutputBuffer::new();
        buffer.append(b"ok \xe4\xb8"); // a truncated 中 - the process died mid-write
        buffer.finish();
        let read = buffer.read_from(0, 64);
        assert_eq!(read.next_cursor, 5, "everything is consumed");
        assert!(read.text.starts_with("ok "));
    }

    #[test]
    fn an_evicted_window_skips_the_continuation_bytes_it_starts_in() {
        let buffer = RunOutputBuffer::new();
        // Fill so that eviction lands one byte into a 汉字: 'x' * (cap - 1) then 中文.
        let mut big = "x".repeat(MAX_LIVE_OUTPUT_BYTES - 1).into_bytes();
        big.extend_from_slice("中文".as_bytes());
        buffer.append(&big);
        // The ring dropped 5 bytes: it now starts at 中's second byte, which is skipped.
        let read = buffer.read_from(0, usize::MAX);
        assert!(read.truncated);
        assert!(!read.text.contains('\u{FFFD}'));
        assert!(read.text.ends_with("文") || read.text.ends_with('x'));
    }

    #[test]
    fn a_read_cap_limits_one_reads_size() {
        let buffer = RunOutputBuffer::new();
        buffer.append(&vec![b'y'; MAX_OUTPUT_READ_BYTES * 2]);
        let chunk = buffer.read_from(0, 1000);
        assert_eq!(chunk.text.len(), 1000);
        assert!(!chunk.truncated, "nothing evicted, only read-capped");
        let next = buffer.read_from(chunk.next_cursor, usize::MAX);
        assert_eq!(next.text.len(), MAX_OUTPUT_READ_BYTES);
    }

    #[tokio::test]
    async fn output_for_an_unknown_run_is_none() {
        let (runs, _) = coordinator(RunCapacity::default());
        assert!(runs.output(424242, 0, usize::MAX).is_none());
    }

    #[tokio::test]
    async fn a_canceled_run_keeps_its_partial_output() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        // A streaming action that parks forever after emitting one line: cancel is the
        // only way out, and its already-streamed text must survive it.
        struct ParkingStream;
        #[async_trait]
        impl Action for ParkingStream {
            fn type_name(&self) -> &'static str {
                "test.parking"
            }
            fn schema(&self) -> Value {
                json!({ "type": "object" })
            }
            async fn execute(
                &self,
                _input: &Value,
                _cancel: CancelHandle,
            ) -> Result<ActionOutcome, ActionError> {
                unreachable!("the coordinator must call execute_with_context")
            }
            async fn execute_with_context(
                &self,
                _input: &Value,
                ctx: crate::services::action::ActionContext,
            ) -> Result<ActionOutcome, ActionError> {
                ctx.output.append("partial\n");
                ctx.cancel.cancelled().await;
                Ok(ActionOutcome {
                    ok: false,
                    canceled: true,
                    ..ActionOutcome::ok()
                })
            }
        }
        runs.actions()
            .register(Arc::new(ParkingStream))
            .expect("register");
        let mut r = request("manual", "park", 1);
        r.action_type = "test.parking".into();
        let submitted = runs.submit(r).expect("submitted");
        for _ in 0..200 {
            if runs
                .output(submitted.run_id, 0, usize::MAX)
                .is_some_and(|(_, c)| !c.text.is_empty())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let view = runs.cancel(submitted.run_id).await.expect("canceled");
        assert_eq!(view.state, RunState::Canceled);
        let (state, chunk) = runs
            .output(submitted.run_id, 0, usize::MAX)
            .expect("buffer retired, not dropped");
        assert!(state.is_terminal());
        assert_eq!(chunk.text, "partial\n");
    }

    /// A sink that keys on run ids and already holds some (a record from an earlier
    /// process).
    struct RecordWithIds(u64);

    impl RunHistorySink for RecordWithIds {
        fn begin(&self, _run_id: u64, _request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>> {
            None
        }
        fn finish(&self, _view: &RunView, _request: &SubmitRequest) {}
        fn last_run_id(&self) -> Option<u64> {
            Some(self.0)
        }
    }

    #[tokio::test]
    async fn run_numbering_continues_across_restarts_and_never_below_the_record() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-runseq-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let seq = dir.join("runs.seq");

        // Process one: no file yet, numbering starts at 0 and the file follows it.
        let (runs, _) = coordinator(RunCapacity::default());
        runs.persist_sequence(seq.clone());
        assert_eq!(runs.next_run_id(), 0);
        let a = runs.submit(request("manual", "a", 1)).unwrap().run_id;
        let b = runs.submit(request("manual", "b", 1)).unwrap().run_id;
        assert_eq!((a, b), (0, 1));
        assert_eq!(std::fs::read_to_string(&seq).unwrap().trim(), "2");
        assert!(!dir.join("runs.seq.tmp").exists(), "the rename left no tmp behind");

        // Process two over the same home: the count goes on where it stopped.
        let (runs, _) = coordinator(RunCapacity::default());
        runs.persist_sequence(seq.clone());
        assert_eq!(runs.next_run_id(), 2);
        assert_eq!(runs.submit(request("manual", "c", 1)).unwrap().run_id, 2);

        // A record that already holds a higher id (a home whose sequence file was lost
        // or never existed) is a floor: the next run is above it, and the file says so.
        let sink: Arc<dyn RunHistorySink> = Arc::new(RecordWithIds(40));
        runs.add_history_sink(sink);
        assert_eq!(runs.next_run_id(), 41);
        assert_eq!(std::fs::read_to_string(&seq).unwrap().trim(), "41");
        assert_eq!(runs.submit(request("manual", "d", 1)).unwrap().run_id, 41);
        // A lower floor changes nothing.
        let sink: Arc<dyn RunHistorySink> = Arc::new(RecordWithIds(3));
        runs.add_history_sink(sink);
        assert_eq!(runs.next_run_id(), 42);

        // A torn or foreign file is ignored, not fatal.
        std::fs::write(&seq, "not a number").unwrap();
        let (runs, _) = coordinator(RunCapacity::default());
        runs.persist_sequence(seq.clone());
        assert_eq!(runs.next_run_id(), 0);
        // Without persist_sequence nothing is written: the bare services and the tests
        // above this one keep their per-process numbering.
        let (bare, _) = coordinator(RunCapacity::default());
        bare.submit(request("manual", "e", 1)).unwrap();
        assert_eq!(std::fs::read_to_string(&seq).unwrap(), "not a number");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
