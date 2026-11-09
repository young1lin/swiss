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

//! The shared run registry — bounded, owner-scoped, first-wins (SPEC §host.plugins: "Jobs only
//! owns scheduling, it does not monopolize background execution"; SPEC §jobs: the
//! lightweight RunRegistry / RunCoordinator is the shared run service, and the jobs
//! scheduler is one producer among several).
//!
//! Every execution — a scheduled job (producer: "jobs"), a manual API run (producer:
//! "manual"), a future plugin's work — is registered HERE with its owner, its provider
//! action and its cancellation source. Three properties are load-bearing:
//!
//! - BOUNDED: at most max_concurrent LOCAL runs execute, at most remote_per_target runs
//!   drive each remote target ([RunLane]), and at most max_queued runs wait; a submission
//!   past a bound is a VISIBLE capacity error, never a silently growing task set.
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
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use prost::Message as _;
use serde_json::{json, Value};
use swiss_bus::pb::v1;
use std::panic::AssertUnwindSafe;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::services::action::{
    ActionContext, ActionError, ActionOutcome, ActionRegistry, CancelSource, RunLane,
    RunOutputSink, RunOutputTee,
};
use crate::services::children::{ChildStop, ChildTag};
use serde_json::Map;
use swiss_core::util::now_ms;

/// Finished runs kept in memory for the API (bounded history: the ring holds at most this
/// many recent outcomes, each with its already-capped output).
const FINISHED_RING: usize = 32;

/// The live-output ceiling while a run is active (SPEC §host.actions). 256 KiB is a few hundred
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
    /// The terminal view of a run `begin` accepted, ACKNOWLEDGED (SPEC §host.history,
    /// planned): the coordinator awaits this future — bounded — before the view enters
    /// the finished ring, so a client that sees the run terminal can already read the
    /// record. `Recorded` claims durable visibility; `Unavailable` is the honest
    /// nothing-persisted answer, which the coordinator publishes on the view instead of
    /// failing the run it already ran.
    fn finish<'a>(
        &'a self,
        view: RunView,
        request: SubmitRequest,
    ) -> futures_core::future::BoxFuture<'a, HistoryAck>;
    /// The highest run id this record holds, so a coordinator whose numbering started
    /// afresh (a first boot after the sequence file existed, a lost one) never hands out
    /// an id the record already keys on. None when the record is empty or keys on
    /// something else.
    fn last_run_id(&self) -> Option<u64> {
        None
    }
}

/// The acknowledged outcome of a sink's terminal record (SPEC §host.history, planned):
/// "recorded completion" is a fact the sink attests, not a hope the enqueue implied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryAck {
    /// The record is durably visible; recorded completion may be claimed.
    Recorded,
    /// The record is NOT complete — enqueue-only, failed, or unavailable within the
    /// bound. The run's own outcome is unaffected; the terminal view carries the honest
    /// status (meta `historyAck`) instead of false success.
    Unavailable { reason: String },
}

/// How long the coordinator waits for one sink's finish acknowledgment before publishing
/// the run with an explicit unacknowledged status: a stop-the-world record must never
/// stop the world, but a lie about durability is worse than a late view.
const HISTORY_ACK_BOUND: Duration = Duration::from_secs(5);

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
    /// The window ends on a character boundary (SPEC §remote.utf8): a multi-byte character the
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
/// Runs one remote target may have going at once ([RunLane::Remote]). Each is a session
/// channel on the target's one SSH connection, and OpenSSH's MaxSessions allows 10 per
/// connection by default: 8 leaves room for a terminal tab and a file transfer beside them.
pub const DEFAULT_MAX_REMOTE_RUNS_PER_TARGET: usize = 8;

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
    /// Who asked (SPEC §remote.history): `cli:<user>@<host>` (self-declared over loopback),
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
    lane: RunLane,
    actor: String,
    action_type: String,
    queued_at_ms: u64,
    started_at_ms: u64,
    cancel: Arc<CancelSource>,
    /// Taken by the first cancel that waits on it. The entry itself stays in `active` until the
    /// run's own finish() moves it to the ring: a stopping run is still a run, visible to get()
    /// and output(), and still holds its slot while its child is alive.
    join: Option<JoinHandle<RunView>>,
}

struct QueuedRun {
    run_id: u64,
    request: SubmitRequest,
    lane: RunLane,
    queued_at_ms: u64,
    done: oneshot::Sender<RunView>,
}

#[derive(Default)]
struct Inner {
    capacity: RunCapacity,
    /// The per-target bound of the remote lanes. Beside [RunCapacity], not in it: the jobs
    /// reconciler re-aims that struct on every config apply.
    remote_per_target: usize,
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

impl Inner {
    /// Why `lane` has no room for one more run, as the refusal the caller shows - or None.
    fn lane_full(&self, lane: &RunLane) -> Option<String> {
        let running = self.active.values().filter(|a| &a.lane == lane).count();
        match lane {
            RunLane::Local if running >= self.capacity.max_concurrent => Some(format!(
                "run capacity is full ({running}/{} running); retry later or raise maxConcurrentRuns",
                self.capacity.max_concurrent
            )),
            RunLane::Remote(target) if running >= self.remote_per_target => Some(format!(
                "remote target {target} already has {running}/{} runs going; retry later",
                self.remote_per_target
            )),
            _ => None,
        }
    }
}

/// The lane as HOST.RUNS spells it: the remote target's name, "" for the local pool.
fn lane_name(lane: &RunLane) -> &str {
    match lane {
        RunLane::Local => "",
        RunLane::Remote(target) => target,
    }
}

fn proto_state(state: RunState) -> v1::RunState {
    match state {
        RunState::Queued => v1::RunState::Queued,
        RunState::Running => v1::RunState::Running,
        RunState::Succeeded => v1::RunState::Succeeded,
        RunState::Failed => v1::RunState::Failed,
        RunState::TimedOut => v1::RunState::TimedOut,
        RunState::Canceled => v1::RunState::Canceled,
    }
}

/// What one HOST.RUNS event carries (SPEC §host.actions: Run events): the run's
/// state — id, action, owner, actor, lane, times, exit code — and never its input,
/// label, error or output, which can hold a credential literal only the owning
/// module knows how to mask.
struct RunEvent {
    run_id: u64,
    state: RunState,
    action: String,
    owner: String,
    actor: String,
    lane: String,
    queued_at_ms: u64,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    exit_code: Option<i32>,
}

impl RunEvent {
    /// A submission that waits: revision 1, no start.
    fn queued(run_id: u64, request: &SubmitRequest, lane: &RunLane, queued_at_ms: u64) -> Self {
        Self {
            run_id,
            state: RunState::Queued,
            action: request.action_type.clone(),
            owner: request.owner.clone(),
            actor: request.actor.clone(),
            lane: lane_name(lane).to_owned(),
            queued_at_ms,
            started_at_ms: None,
            ended_at_ms: None,
            exit_code: None,
        }
    }

    /// A start, at submit or from the queue: revision 2.
    fn running(
        run_id: u64,
        owner: &str,
        actor: &str,
        action: &str,
        lane: &RunLane,
        queued_at_ms: u64,
        started_at_ms: u64,
    ) -> Self {
        Self {
            run_id,
            state: RunState::Running,
            action: action.to_owned(),
            owner: owner.to_owned(),
            actor: actor.to_owned(),
            lane: lane_name(lane).to_owned(),
            queued_at_ms,
            started_at_ms: Some(started_at_ms),
            ended_at_ms: None,
            exit_code: None,
        }
    }

    /// The terminal state, or canceled for a queued run a cancel dropped: revision 3.
    fn terminal(view: &RunView, lane: &RunLane) -> Self {
        Self {
            run_id: view.run_id,
            state: view.state,
            action: view.action_type.clone(),
            owner: view.owner.clone(),
            actor: view.actor.clone(),
            lane: lane_name(lane).to_owned(),
            queued_at_ms: view.queued_at_ms,
            started_at_ms: view.started_at_ms,
            ended_at_ms: view.ended_at_ms,
            exit_code: view.exit_code,
        }
    }
}

/// The coordinator. Shared as one Arc between RuntimeServices, the jobs scheduler and the
/// API handlers.
pub struct RunCoordinator {
    registry: Arc<ActionRegistry>,
    /// HOST.RUNS publishes here (SPEC §host.actions: Run events). The bus never
    /// calls back into the coordinator, so the lock order below holds one way.
    bus: Arc<swiss_bus::Bus>,
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

    pub fn new(registry: Arc<ActionRegistry>, bus: Arc<swiss_bus::Bus>) -> Arc<Self> {
        Arc::new(RunCoordinator {
            registry,
            bus,
            inner: Mutex::new(Inner {
                capacity: RunCapacity::default(),
                remote_per_target: DEFAULT_MAX_REMOTE_RUNS_PER_TARGET,
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
                    // The floor raise goes through the same cross-process claim as every
                    // allocation: two gateways raising against one file serialize, and
                    // neither can flatten the other's commit. A raise takes no id — the
                    // committed value IS the next id to hand out.
                    inner.next_id = claim_sequence(&path, inner.next_id, CLAIM_STALE, false);
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

    /// The per-target bound of the remote lanes.
    pub fn remote_per_target(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remote_per_target
    }

    /// Re-aim the remote lanes' per-target bound; like [RunCoordinator::set_capacity], it
    /// gates new submissions only and dispatches anything a raised bound now admits.
    pub fn set_remote_per_target(self: &Arc<Self>, bound: usize) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.remote_per_target = bound;
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

    /// Publish HOST.RUNS for one state a run just entered (SPEC §host.actions: Run
    /// events). The caller still holds the inner lock — commit order is event order
    /// (SPEC §bus.topics) — and the lock order is this lock, then the bus's own; the
    /// bus never calls back. A refused submission never reaches here: it never
    /// became a run.
    fn announce(&self, event: RunEvent) {
        #[cfg(test)]
        assert!(
            self.inner.try_lock().is_err(),
            "run events publish under the bookkeeping lock"
        );
        let revision = if event.state.is_terminal() {
            3
        } else if event.state == RunState::Running {
            2
        } else {
            1
        };
        // The key is the action's namespace — the type up to its first dot — so a
        // page hears only the runs it shows ("remote.exec" -> "remote").
        let key = event.action.split('.').next().unwrap_or_default().to_owned();
        let body = v1::RunChanged {
            run_id: event.run_id,
            revision,
            state: proto_state(event.state) as i32,
            action: event.action,
            owner: event.owner,
            actor: event.actor,
            lane: event.lane,
            queued_at_ms: event.queued_at_ms,
            started_at_ms: event.started_at_ms,
            ended_at_ms: event.ended_at_ms,
            exit_code: event.exit_code,
        }
        .encode_to_vec();
        self.bus.publish(
            v1::Module::Host as u32,
            v1::HostSub::Runs as u32,
            v1::RunsId::RunsChanged as u32,
            &key,
            "",
            body,
        );
    }

    /// Register + start (or queue) one run. Returns immediately with the run id — an HTTP
    /// request must never own a background task's lifetime (SPEC §host.actions).
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
        let lane = action.lane(&request.input);
        let (done_tx, done_rx) = oneshot::channel();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = match inner.sequence.clone() {
            // The id comes from the FILE under a cross-process claim, never from the
            // in-process counter alone: a second same-home gateway allocates between
            // this process's reads, and two runs under one number is the corruption the
            // sequence exists to prevent.
            Some(path) => {
                let id = claim_sequence(&path, inner.next_id, CLAIM_STALE, true);
                inner.next_id = id + 1;
                id
            }
            None => {
                let id = inner.next_id;
                inner.next_id += 1;
                id
            }
        };
        let output = RunOutputBuffer::new();
        let queued_at = now_ms();
        let full = inner.lane_full(&lane);
        if full.is_none() {
            // The output buffer exists from the moment the run id is committed, so the
            // output route can follow a QUEUED run too (it simply has nothing yet). It is
            // registered only on the paths that commit the run: nothing ever retires a buffer
            // for a run that was refused, so registering before the capacity checks leaked one
            // map entry per refusal, forever.
            inner.outputs.insert(run_id, output.clone());
            let run = QueuedRun {
                run_id,
                request: request.clone(),
                lane,
                queued_at_ms: queued_at,
                done: done_tx,
            };
            self.start_locked(&mut inner, run, output);
        } else if request.queue_if_busy {
            if inner.queued.len() >= inner.capacity.max_queued {
                return Err(SubmitError::Capacity(format!(
                    "run queue is full ({}/{}); overlap policy queue-one cannot enqueue another successor",
                    inner.queued.len(),
                    inner.capacity.max_queued
                )));
            }
            inner.outputs.insert(run_id, output.clone());
            let event = RunEvent::queued(run_id, &request, &lane, queued_at);
            inner.queued.push_back(QueuedRun {
                run_id,
                request,
                lane,
                queued_at_ms: queued_at,
                done: done_tx,
            });
            self.announce(event);
        } else {
            return Err(SubmitError::Capacity(full.unwrap_or_default()));
        }
        Ok(SubmittedRun {
            run_id,
            done: done_rx,
        })
    }

    /// Spawn the tracked execution task. Caller holds the inner lock. The receiver is
    /// &Arc<Self> because the task needs an owned handle back to self for its bookkeeping.
    /// `run` is what a queued run holds - a submission that starts at once is shaped the same.
    fn start_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        run: QueuedRun,
        output: Arc<RunOutputBuffer>,
    ) {
        let QueuedRun {
            run_id,
            request,
            lane,
            queued_at_ms,
            done,
        } = run;
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
        let event = RunEvent::running(
            run_id,
            &owner,
            &actor,
            &action_type,
            &lane,
            queued_at_ms,
            started_at,
        );
        inner.active.insert(
            run_id,
            ActiveRun {
                owner,
                label,
                lane,
                actor,
                action_type,
                queued_at_ms,
                started_at_ms: started_at,
                cancel,
                join: Some(join),
            },
        );
        self.announce(event);
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
        // The registry row a spawning action will carry (SPEC §process.children): this
        // run's owner and label, and a hook that cancels through the coordinator — the
        // run's OWN teardown path, so a stop from the Process page lands the run in
        // `canceled` instead of killing a pid underneath the coordinator. The hook holds
        // a WEAK handle: the registry row must not be the thing keeping the pool alive.
        let child = ChildTag {
            owner: request.owner.clone(),
            label: if request.label.is_empty() {
                request.action_type.clone()
            } else {
                format!("{} \u{b7} {}", request.action_type, request.label)
            },
            stop: Some(Arc::new(RunChildStop {
                runs: Arc::downgrade(&self),
                run_id,
            })),
        };
        let (result, timed_out) =
            drive_action(&self.registry, &request, &cancel, &output, tees, child).await;
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
        // The durable record first, and ACKNOWLEDGED: the coordinator awaits each sink's
        // completion acknowledgment (bounded) before the view enters the finished ring,
        // so the run is never terminal in the API before its history row exists — and a
        // record that could not persist publishes its honest status on the view instead
        // of false success. The sinks were captured at start, so a plugin stop in
        // between cannot orphan a record.
        let mut ack_failures: Vec<String> = Vec::new();
        for (sink, _) in &recorders {
            let ack = tokio::time::timeout(
                HISTORY_ACK_BOUND,
                sink.finish(view.clone(), request.clone()),
            )
            .await;
            let failure = match ack {
                Ok(HistoryAck::Recorded) => None,
                Ok(HistoryAck::Unavailable { reason }) => Some(reason),
                // The bound broke: the view must still publish, but never as recorded.
                Err(_) => Some("the record did not acknowledge in time".into()),
            };
            if let Some(reason) = failure {
                ack_failures.push(reason);
            }
        }
        if !ack_failures.is_empty() {
            view.meta
                .insert("historyAck".into(), Value::String("unavailable".into()));
            view.meta.insert(
                "historyAckError".into(),
                Value::String(ack_failures.join("; ")),
            );
        }
        self.finish(run_id, view.clone());
        // The submitter may already be gone (fire-and-forget scheduled runs) — that is
        // fine; the finished ring is the durable record either way.
        let _ = done.send(view.clone());
        view
    }

    /// Bookkeeping when a run reaches its terminal state: leave the active set, enter the
    /// bounded finished ring, dispatch whatever was waiting for the freed slot.
    /// One bounded read of a run's live output (SPEC §host.actions): the text from `after`
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
            let lane = inner
                .active
                .remove(&run_id)
                .map(|a| a.lane)
                .unwrap_or(RunLane::Local);
            inner.finished.push_front(view.clone());
            inner.finished.truncate(FINISHED_RING);
            retire_output_locked(&mut inner, run_id);
            self.announce(RunEvent::terminal(&view, &lane));
        }
        self.dispatch_queue();
    }

    /// Waits until a run another caller is already stopping has left the active set.
    async fn wait_finished(&self, run_id: u64) {
        while self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .contains_key(&run_id)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn finished_view(&self, run_id: u64) -> Option<RunView> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished
            .iter()
            .find(|v| v.run_id == run_id)
            .cloned()
    }

    /// Enter the finished ring without touching the active set (queued runs canceled in
    /// place). `lane` is the run's lane: the event names it even though the run never
    /// started.
    fn push_finished(&self, run_id: u64, view: RunView, lane: &RunLane) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.finished.push_front(view.clone());
        inner.finished.truncate(FINISHED_RING);
        retire_output_locked(&mut inner, run_id);
        self.announce(RunEvent::terminal(&view, lane));
    }

    /// Start queued runs while the pool has room, FIFO.
    fn dispatch_queue(self: &Arc<Self>) {
        loop {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            // The oldest waiting run whose lane has room: a run queued for a busy remote
            // target does not hold back a local one behind it, and within one lane the order
            // stays first-in, first-out (a lane with no room has none for any of its runs).
            let Some(next) = inner
                .queued
                .iter()
                .position(|q| inner.lane_full(&q.lane).is_none())
            else {
                return;
            };
            let Some(run) = inner.queued.remove(next) else {
                return;
            };
            // The queued run's buffer was created at submit; hand it over to the task.
            let output = inner
                .outputs
                .get(&run.run_id)
                .cloned()
                .unwrap_or_else(RunOutputBuffer::new);
            self.start_locked(&mut inner, run, output);
        }
    }

    /// Cancel one run and WAIT for it to actually finish (child reaped, readers joined).
    /// Idempotent under first-wins: a finished run returns its finished view unchanged.
    pub async fn cancel(&self, run_id: u64) -> Option<RunView> {
        // The entry is NOT removed here: the run's own finish() does that once it has
        // stopped. Removing it first left a window - as long as the action's clean stop, a
        // remote channel's close - in which get() and output() found the run nowhere (the
        // CLI's follower printed `404 no run N`, 2026-09-28) and its slot was free for the
        // next run while its child still ran.
        let active = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .active
                .get_mut(&run_id)
                .map(|a| (a.cancel.clone(), a.join.take()))
        };
        if let Some((source, join)) = active {
            source.cancel();
            match join {
                Some(join) => {
                    if let Ok(view) = join.await {
                        return Some(view);
                    }
                    // The task itself died abnormally (runtime shutdown); the ring may
                    // still hold what it recorded before that.
                }
                // Another cancel holds the join: wait for the same finish.
                None => self.wait_finished(run_id).await,
            }
            return self.finished_view(run_id);
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = inner.queued.iter().position(|q| q.run_id == run_id) {
            let QueuedRun {
                run_id,
                request,
                lane,
                queued_at_ms,
                done,
                ..
            } = inner.queued.remove(pos).expect("position just checked");
            let view = queued_canceled_view(run_id, &request, queued_at_ms);
            drop(inner);
            let _ = done.send(view.clone());
            self.push_finished(run_id, view.clone(), &lane);
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
        // As cancel(): the entries stay in `active` until each run's finish() moves it.
        let targets: Vec<(u64, Arc<CancelSource>, Option<JoinHandle<RunView>>)> = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .active
                .iter_mut()
                .filter(|(_, a)| matches(&a.owner, &a.action_type))
                .map(|(id, a)| (*id, a.cancel.clone(), a.join.take()))
                .collect()
        };
        let mut count = targets.len();
        for (_, cancel, _) in &targets {
            cancel.cancel();
        }
        // Await each: when this returns, every child is reaped and every reader joined.
        for (run_id, _, join) in targets {
            match join {
                Some(join) => {
                    let _ = join.await;
                }
                None => self.wait_finished(run_id).await,
            }
        }
        // Drop matching queued runs as canceled.
        let mut canceled_queued: Vec<(RunView, RunLane)> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut kept = VecDeque::new();
            while let Some(q) = inner.queued.pop_front() {
                if matches(&q.request.owner, &q.request.action_type) {
                    count += 1;
                    let view = queued_canceled_view(q.run_id, &q.request, q.queued_at_ms);
                    let _ = q.done.send(view.clone());
                    canceled_queued.push((view, q.lane));
                } else {
                    kept.push_back(q);
                }
            }
            inner.queued = kept;
        }
        for (view, lane) in canceled_queued {
            self.push_finished(view.run_id, view, &lane);
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

/// The run owner's stop (SPEC §process.children): the coordinator's own cancel(run_id),
/// so a stopped run reaches `canceled` through its lifecycle instead of dying underneath
/// the coordinator and never reaching a terminal state. Weak by design — the row must not
/// be what keeps the pool alive.
struct RunChildStop {
    runs: Weak<RunCoordinator>,
    run_id: u64,
}

impl ChildStop for RunChildStop {
    fn stop(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        let Some(runs) = self.runs.upgrade() else {
            return Box::pin(async {
                Err("the run coordinator is gone; the run already ended".to_string())
            });
        };
        let run_id = self.run_id;
        Box::pin(async move {
            match runs.cancel(run_id).await {
                // cancel() answers the view even when the run had already finished — a
                // natural stop is as stopped as a canceled one.
                Some(_) => Ok(()),
                None => Err(format!("run {run_id} is not active")),
            }
        })
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
    child: ChildTag,
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
    // The single execution seam (SPEC §host.actions): every action runs through
    // execute_with_context; actions that predate it simply never touch the sink.
    let ctx = ActionContext {
        cancel: cancel.handle(),
        output: RunOutputSink::new(output.clone(), tees),
        child,
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

/// How long a run-id claim may sit unreleased before another same-home gateway presumes
/// its holder died and steals it: a live claimant holds it for a handful of file
/// operations, so anything near seconds means the process crashed between create and
/// release (SPEC §host.history, planned).
const CLAIM_STALE: Duration = Duration::from_secs(5);
/// The claim retry cadence while the claim is fresh, then the coarser nap for the
/// long tail of a stale wait: bounded wakeups without missing the stale moment.
const CLAIM_RETRY: Duration = Duration::from_millis(2);
const CLAIM_COARSE: Duration = Duration::from_millis(25);

/// One claim over the shared-home sequence file, safe against OTHER PROCESSES
/// (SPEC §host.history, planned): same-home multiport gateways allocate concurrently,
/// and an in-process mutex solves nothing there. Every claim takes a short-lived lock
/// file (`create_new` is atomic on the filesystem), re-reads the committed value under
/// the claim, commits and releases. With `consume` the folded next id is TAKEN (a
/// submission's run id) and the file holds the id after it; without it the claim only
/// raises the committed next-to-hand-out (a sink's floor) and takes nothing. The
/// claim's payload is the value its holder is about to commit, so a crash mid-claim
/// leaves a floor behind — the id a dead holder may have used is never reused, worst
/// case skipped. Returns the folded next id.
fn claim_sequence(path: &std::path::Path, floor: u64, stale_after: Duration, consume: bool) -> u64 {
    use std::io::Write as _;
    let claim = path.with_extension("claim");
    let started = std::time::Instant::now();
    let mut floor = floor;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim)
        {
            Ok(mut file) => {
                let committed = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                let next = committed.max(floor);
                let commit = if consume { next + 1 } else { next };
                // The payload BEFORE the commit: a claimant that dies between the two
                // leaves a floor behind, never a reusable id.
                let _ = file.write_all(commit.to_string().as_bytes());
                let _ = file.flush();
                drop(file);
                write_sequence(path, commit);
                let _ = std::fs::remove_file(&claim);
                return next;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another process holds the claim. Its payload is the value AFTER the id
                // it may use — floor on it whether the holder is live or dead.
                let payload = std::fs::read_to_string(&claim)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or_else(|| {
                        // Unreadable or torn: assume the worst — its holder may have used
                        // exactly the committed value.
                        std::fs::read_to_string(path)
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .unwrap_or(0)
                            + 1
                    });
                floor = floor.max(payload);
                if started.elapsed() >= stale_after {
                    // Stale: the holder is presumed dead past the bound. The payload
                    // floor already covers whatever it may have handed out. The steal
                    // must stay REACHABLE: any liveness cap below this bound would
                    // orphan the claim file forever and wedge every later submission
                    // into the capped path.
                    let _ = std::fs::remove_file(&claim);
                    continue;
                }
                if started.elapsed() >= stale_after.saturating_mul(3) {
                    // Bounded liveness ceiling for a pathologically wedged filesystem:
                    // three stale windows went by without the claim clearing. COMMIT
                    // the floor - an uncommitted floor hands one id to two processes -
                    // then proceed past the claimant.
                    write_sequence(path, floor);
                    return floor;
                }
                let nap = if started.elapsed() < Duration::from_millis(100) {
                    CLAIM_RETRY
                } else {
                    CLAIM_COARSE
                };
                std::thread::sleep(nap);
            }
            Err(_) => {
                // The claim cannot even be attempted (read-only home, removed dir): the
                // in-process counter with its floors is the most that can be claimed.
                return floor;
            }
        }
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

    /// The sleep, run against a remote target: its lane is the input's `target`.
    struct RemoteSleepAction {
        inner: SleepAction,
    }

    #[async_trait]
    impl Action for RemoteSleepAction {
        fn type_name(&self) -> &'static str {
            "test.remote-sleep"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn lane(&self, input: &Value) -> RunLane {
            RunLane::Remote(input["target"].as_str().unwrap_or_default().to_string())
        }
        async fn execute(
            &self,
            input: &Value,
            cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            self.inner.execute(input, cancel).await
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
        let (runs, _bus, starts) = coordinator_with_bus(capacity);
        (runs, starts)
    }

    /// The coordinator and its bus, so HOST.RUNS tests can subscribe a peer to the
    /// events the coordinator publishes.
    fn coordinator_with_bus(
        capacity: RunCapacity,
    ) -> (Arc<RunCoordinator>, Arc<swiss_bus::Bus>, Arc<AtomicUsize>) {
        let starts = Arc::new(AtomicUsize::new(0));
        let registry = ActionRegistry::new();
        registry
            .register(Arc::new(SleepAction {
                starts: starts.clone(),
            }))
            .expect("register sleep");
        registry
            .register(Arc::new(NamespacedRemoteSleepAction {
                inner: SleepAction {
                    starts: starts.clone(),
                },
            }))
            .expect("register namespaced remote sleep");
        registry
            .register(Arc::new(RemoteSleepAction {
                inner: SleepAction {
                    starts: starts.clone(),
                },
            }))
            .expect("register remote sleep");
        registry
            .register(Arc::new(StubbornAction))
            .expect("register stubborn");
        registry
            .register(Arc::new(PanicAction))
            .expect("register panic");
        let bus = swiss_bus::Bus::new(Arc::new(|| "test".to_string()));
        let coordinator = RunCoordinator::new(Arc::new(registry), bus.clone());
        coordinator.set_capacity(capacity);
        (coordinator, bus, starts)
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

    fn remote(target: &str, ms: u64) -> SubmitRequest {
        SubmitRequest {
            action_type: "test.remote-sleep".into(),
            input: json!({ "ms": ms, "target": target }),
            ..request("manual", target, ms)
        }
    }

    // The owner's report (2026-09-28): several terminals running `swiss remote exec` against
    // one server - the third was refused with "run capacity is full (2/2 running)", though
    // the SSH connection multiplexes channels. A remote run counts against its target's lane.
    #[tokio::test]
    async fn remote_runs_take_their_targets_lane_not_the_local_pool() {
        let (runs, starts) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        runs.set_remote_per_target(2);
        runs.submit(request("manual", "build", 30_000))
            .expect("the local slot");
        assert!(
            runs.submit(request("manual", "other", 30_000)).is_err(),
            "the local pool is full"
        );
        runs.submit(remote("web", 30_000)).expect("web 1");
        runs.submit(remote("web", 30_000))
            .expect("web 2, beside a full local pool");
        let err = runs.submit(remote("web", 30_000)).expect_err("web is full");
        assert!(matches!(err, SubmitError::Capacity(_)), "{err}");
        assert!(err.to_string().contains("web already has 2/2"), "{err}");
        runs.submit(remote("db", 30_000))
            .expect("another target has its own lane");
        settle().await;
        assert_eq!(starts.load(Ordering::SeqCst), 4);
        runs.shutdown_all().await;
    }

    #[tokio::test]
    async fn a_queued_run_waits_for_its_own_lane_only() {
        let (runs, _starts) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 4,
        });
        runs.set_remote_per_target(1);
        let first = runs.submit(remote("web", 30_000)).expect("web runs");
        let mut waiting = remote("web", 1);
        waiting.queue_if_busy = true;
        let waiting = runs.submit(waiting).expect("queued behind web");
        settle().await;
        assert_eq!(
            runs.get(waiting.run_id).map(|v| v.state),
            Some(RunState::Queued)
        );
        // A local run is not held back by the remote one queued ahead of it.
        let local = runs
            .submit(request("manual", "quick", 1))
            .expect("local has room");
        assert_eq!(local.done.await.expect("view").state, RunState::Succeeded);
        // Freeing web's slot starts the queued web run.
        runs.cancel(first.run_id).await;
        let view = waiting.done.await.expect("the queued run ran");
        assert_eq!(view.state, RunState::Succeeded);
    }

    // A refused submission must not park an output buffer for a run id that will never run:
    // retire paths only run for started or queued runs, so each refusal used to leak one
    // `outputs` entry forever - invisible to list()/get(), unbounded under a schedule that
    // keeps hammering a full pool.
    #[tokio::test]
    async fn refused_submissions_leave_no_output_buffer_behind() {
        let (runs, _starts) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        let _first = runs
            .submit(request("manual", "long", 30_000))
            .expect("first runs");
        settle().await;
        for _ in 0..3 {
            // Both Capacity refusals: the queue-full arm (queue_if_busy against a zero-length
            // queue) and the pool-full arm.
            let mut queued = request("manual", "other", 30_000);
            queued.queue_if_busy = true;
            assert!(runs.submit(queued).is_err(), "queue is full");
            assert!(
                runs.submit(request("manual", "other", 30_000)).is_err(),
                "pool is full"
            );
        }
        let buffered = {
            let inner = runs.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.outputs.len()
        };
        assert_eq!(buffered, 1, "only the buffer of the running run remains");
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

    // --- live output (SPEC §host.actions) -------------------------------------------------------

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
        // SPEC §remote.utf8: "日志" is 6 bytes; a 4-byte max would cut 志 after its first byte.
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

    #[tokio::test]
    async fn a_run_that_is_stopping_stays_visible_and_keeps_its_slot() {
        // A remote command's clean stop waits for its SSH channel to close. While it did,
        // cancel() had already taken the run out of the active set, so get() and output()
        // found it nowhere: the CLI's follower printed `404 no run 4` and exited 1 mid-stop
        // (2026-09-28), and the freed slot let the next run start beside a child still alive.
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 1,
            max_queued: 0,
        });
        struct SlowStop(Arc<tokio::sync::Notify>);
        #[async_trait]
        impl Action for SlowStop {
            fn type_name(&self) -> &'static str {
                "test.slowstop"
            }
            fn schema(&self) -> Value {
                json!({ "type": "object" })
            }
            async fn execute(
                &self,
                _input: &Value,
                cancel: CancelHandle,
            ) -> Result<ActionOutcome, ActionError> {
                cancel.cancelled().await;
                // Canceled: the stop itself takes as long as the test says.
                self.0.notified().await;
                Ok(ActionOutcome {
                    ok: false,
                    canceled: true,
                    ..ActionOutcome::ok()
                })
            }
        }
        let gate = Arc::new(tokio::sync::Notify::new());
        runs.actions()
            .register(Arc::new(SlowStop(gate.clone())))
            .expect("register");
        let mut r = request("manual", "slow", 1);
        r.action_type = "test.slowstop".into();
        let id = runs.submit(r).expect("submitted").run_id;
        settle().await;

        let canceling = {
            let runs = runs.clone();
            tokio::spawn(async move { runs.cancel(id).await })
        };
        settle().await;
        assert!(!canceling.is_finished(), "the stop is still in progress");
        let view = runs.get(id).expect("a stopping run is still a run");
        assert!(!view.state.is_terminal(), "not terminal until it has stopped: {:?}", view.state);
        let (state, _) = runs.output(id, 0, usize::MAX).expect("its output still answers");
        assert!(!state.is_terminal());
        assert!(
            matches!(runs.submit(request("manual", "next", 1)), Err(SubmitError::Capacity(_))),
            "the slot is held until the child is gone"
        );

        gate.notify_one();
        let view = canceling.await.expect("joined").expect("canceled view");
        assert_eq!(view.state, RunState::Canceled);
        assert_eq!(runs.get(id).expect("in the ring").state, RunState::Canceled);
        assert!(runs.output(id, 0, usize::MAX).expect("retired, not dropped").0.is_terminal());
    }

    #[tokio::test]
    async fn a_second_cancel_of_a_stopping_run_waits_for_the_same_finish() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 2,
            max_queued: 0,
        });
        let id = runs.submit(request("manual", "long", 30_000)).expect("submitted").run_id;
        settle().await;
        let (a, b) = tokio::join!(runs.cancel(id), runs.cancel(id));
        assert_eq!(a.expect("first").state, RunState::Canceled);
        assert_eq!(b.expect("second").state, RunState::Canceled);
    }

    /// A sink that keys on run ids and already holds some (a record from an earlier
    /// process).
    struct RecordWithIds(u64);

    impl RunHistorySink for RecordWithIds {
        fn begin(&self, _run_id: u64, _request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>> {
            None
        }
        fn finish<'a>(
            &'a self,
            _view: RunView,
            _request: SubmitRequest,
        ) -> futures_core::future::BoxFuture<'a, HistoryAck> {
            Box::pin(async { HistoryAck::Recorded })
        }
        fn last_run_id(&self) -> Option<u64> {
            Some(self.0)
        }
    }

    /// A tee that takes the stream and keeps nothing — the record this test's sinks owe
    /// is the acknowledgment, not the bytes.
    struct NullTee;

    impl RunOutputTee for NullTee {
        fn write(&self, _bytes: &[u8]) {}
    }

    /// A sink whose finish waits for a gate before acknowledging — the shape that must
    /// hold a run's terminal publication open until its record is real. begin ACCEPTS the
    /// run: a sink that declines never sees finish at all.
    struct GatedAck {
        started: Arc<AtomicBool>,
        acked: Arc<AtomicBool>,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl RunHistorySink for GatedAck {
        fn begin(&self, _run_id: u64, _request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>> {
            Some(Arc::new(NullTee))
        }
        fn finish<'a>(
            &'a self,
            _view: RunView,
            _request: SubmitRequest,
        ) -> futures_core::future::BoxFuture<'a, HistoryAck> {
            Box::pin(async move {
                self.started.store(true, Ordering::SeqCst);
                let gate = self
                    .gate
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                self.acked.store(true, Ordering::SeqCst);
                HistoryAck::Recorded
            })
        }
    }

    /// A sink that cannot persist anything: the failure must be VISIBLE on the terminal
    /// view without failing the execution that already succeeded.
    struct FailingAck;

    impl RunHistorySink for FailingAck {
        fn begin(&self, _run_id: u64, _request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>> {
            Some(Arc::new(NullTee))
        }
        fn finish<'a>(
            &'a self,
            _view: RunView,
            _request: SubmitRequest,
        ) -> futures_core::future::BoxFuture<'a, HistoryAck> {
            Box::pin(async {
                HistoryAck::Unavailable {
                    reason: "the record store is gone".into(),
                }
            })
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

    #[tokio::test]
    async fn terminal_view_waits_for_history_finish_ack() {
        let (runs, _) = coordinator(RunCapacity {
            max_concurrent: 2,
            max_queued: 2,
        });
        let (release, gate) = oneshot::channel::<()>();
        let sink = Arc::new(GatedAck {
            started: Arc::new(AtomicBool::new(false)),
            acked: Arc::new(AtomicBool::new(false)),
            gate: Mutex::new(Some(gate)),
        });
        runs.add_history_sink(sink.clone());
        let submitted = runs
            .submit(request("manual", "quick", 1))
            .expect("submitted");
        // Wait until the sink was asked to finish (the run itself lasted 1 ms).
        for _ in 0..100 {
            if sink.started.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(sink.started.load(Ordering::SeqCst), "the sink was asked");
        // The execution is over; the unacknowledged record is the ONLY thing holding the
        // terminal publication open — enqueue-and-return could not promise that.
        assert!(
            !runs
                .get(submitted.run_id)
                .expect("still a run")
                .state
                .is_terminal(),
            "not terminal before the acknowledgment"
        );
        release.send(()).expect("release the record");
        let view = submitted.done.await.expect("the final view");
        assert_eq!(view.state, RunState::Succeeded);
        assert!(sink.acked.load(Ordering::SeqCst));
        assert_eq!(
            runs.get(view.run_id).expect("in the ring").state,
            RunState::Succeeded
        );
    }

    #[tokio::test]
    async fn history_ack_failure_is_visible_without_failing_execution() {
        let (runs, starts) = coordinator(RunCapacity {
            max_concurrent: 2,
            max_queued: 2,
        });
        runs.add_history_sink(Arc::new(FailingAck));
        let submitted = runs
            .submit(request("manual", "quick", 1))
            .expect("submitted");
        let view = submitted.done.await.expect("the final view");
        // The run itself succeeded; the record's failure is a status, not an outcome.
        assert_eq!(view.state, RunState::Succeeded);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            view.meta.get("historyAck").and_then(Value::as_str),
            Some("unavailable")
        );
        let reason = view
            .meta
            .get("historyAckError")
            .and_then(Value::as_str)
            .expect("the reason is carried");
        assert!(reason.contains("the record store is gone"), "{reason}");
        // The ring copy carries the same honest status, not just the one-shot view.
        let stored = runs.get(view.run_id).expect("in the ring");
        assert_eq!(
            stored.meta.get("historyAck").and_then(Value::as_str),
            Some("unavailable")
        );
    }

    #[tokio::test]
    async fn two_process_run_allocator_never_reuses_ids() {
        // Two coordinators over one sequence file are two gateway processes on the ports
        // of one Swiss home. The claim protocol coordinates through files only — what a
        // real second process would take and release — so unit interleaving is honest
        // evidence for it; the true two-exe walk stays with swiss-it.
        let dir = std::env::temp_dir().join(format!(
            "swiss-runclaim-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let seq = dir.join("runs.seq");
        let claim = dir.join("runs.claim");

        // Eight 1 ms submissions land nearly at once: room for all of them.
        let capacity = RunCapacity {
            max_concurrent: 8,
            max_queued: 8,
        };
        let (a, _) = coordinator(capacity);
        let (b, _) = coordinator(capacity);
        a.persist_sequence(seq.clone());
        b.persist_sequence(seq.clone());

        // Interleaved allocation across both "processes": every id distinct, every claim
        // released, the file always the next unused id.
        let mut seen = std::collections::HashSet::new();
        for round in 0..4 {
            let x = a.submit(request("manual", "a", 1)).unwrap().run_id;
            let y = b.submit(request("manual", "b", 1)).unwrap().run_id;
            assert_ne!(x, y, "round {round} handed the same id to both processes");
            seen.insert(x);
            seen.insert(y);
            assert!(!claim.exists(), "the claim was released every time");
        }
        assert_eq!(seen.len(), 8, "eight submissions, eight distinct ids");
        let committed: u64 = std::fs::read_to_string(&seq).unwrap().trim().parse().unwrap();
        assert!(committed >= 8, "the file holds the next unused id: {committed}");

        // A stale claim (its holder died mid-claim) is stolen, with its payload — the
        // value its dead holder was about to commit — as the floor: that id is skipped,
        // never reused.
        std::fs::write(&claim, "100").unwrap();
        let next = claim_sequence(&seq, 0, Duration::from_millis(20), true);
        assert!(next >= 100, "the stale claim's payload floors the steal: {next}");
        assert!(!claim.exists(), "the steal released the claim");
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn a_large_stale_bound_still_steals_and_commits_its_floor() {
        // The bounded-liveness cap must sit ABOVE the stale bound, never below it:
        // with the cap first, the steal is unreachable — an orphaned claim wedges
        // every later submission into the bounded path, the claim file is never
        // removed, and the returned floor is never committed, so two processes can
        // be handed the same id. Production uses CLAIM_STALE (5 s): the claim
        // protocol must honor whatever bound it is given.
        let dir = std::env::temp_dir().join(format!(
            "swiss-runclaim2-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let seq = dir.join("runs.seq");
        let claim = dir.join("runs.claim");
        std::fs::write(&seq, "50").unwrap();
        std::fs::write(&claim, "100").unwrap();
        let started = std::time::Instant::now();
        let next = claim_sequence(&seq, 0, Duration::from_millis(300), true);
        let took = started.elapsed();
        assert!(next >= 100, "the foreign claim's payload floors the result: {next}");
        assert!(
            !claim.exists(),
            "the stale steal REMOVED the foreign claim - an orphaned claim must not wedge every later submission"
        );
        assert!(
            took >= Duration::from_millis(280),
            "the steal waited out the stale bound, not an earlier arbitrary cap: {took:?}"
        );
        let committed: u64 = std::fs::read_to_string(&seq).unwrap().trim().parse().unwrap();
        assert!(
            committed >= 100,
            "the floor is committed to the sequence file - an uncommitted floor hands one id to two processes: {committed}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sleep under a `remote.` type name: its events key to the "remote"
    /// namespace, whatever lane it actually runs in.
    struct NamespacedRemoteSleepAction {
        inner: SleepAction,
    }

    #[async_trait]
    impl Action for NamespacedRemoteSleepAction {
        fn type_name(&self) -> &'static str {
            "remote.sleep"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn lane(&self, input: &Value) -> RunLane {
            RunLane::Remote(input["target"].as_str().unwrap_or_default().to_string())
        }
        async fn execute(
            &self,
            input: &Value,
            cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            self.inner.execute(input, cancel).await
        }
    }

    /// HOST.RUNS over the real pump, without sockets (SPEC §host.actions: Run events,
    /// SPEC §bus.topics): a subscribed link, its frames decoded as RunChanged.
    mod run_events {
        use super::*;
        use tokio::sync::mpsc;

        struct Channel {
            rx: mpsc::Receiver<swiss_bus::IoMsg>,
            tx: mpsc::Sender<swiss_bus::IoMsg>,
        }

        impl swiss_bus::Io for Channel {
            fn recv(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<swiss_bus::IoMsg>> + Send + '_>> {
                Box::pin(async move { self.rx.recv().await })
            }
            fn send(&mut self, msg: swiss_bus::IoMsg) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
                Box::pin(async move { self.tx.send(msg).await.is_ok() })
            }
        }

        /// One subscribed link. `open` subscribes BEFORE the first submission: events
        /// that left while nobody listened are gone, and the tests must see that.
        struct Peer {
            tx: mpsc::Sender<swiss_bus::IoMsg>,
            rx: mpsc::Receiver<swiss_bus::IoMsg>,
            seq: u32,
            events: std::collections::VecDeque<v1::Frame>,
        }

        impl Peer {
            async fn open(bus: &Arc<swiss_bus::Bus>, key: &str) -> Self {
                let (tx, rx) = mpsc::channel(32);
                let (out, incoming) = mpsc::channel(32);
                tokio::spawn(swiss_bus::run(bus.clone(), Channel { rx, tx: out }));
                let mut peer = Self { tx, rx: incoming, seq: 1, events: Default::default() };
                let welcome = peer
                    .request_at(
                        0,
                        v1::BusSub::Link as u32,
                        v1::LinkId::Hello as u32,
                        v1::Hello { protocol: 1, page_id: "page-a".into(), build: "test".into() }
                            .encode_to_vec(),
                    )
                    .await;
                assert!(welcome.fault.is_none());
                let answer = peer
                    .request_at(
                        0,
                        v1::BusSub::Topic as u32,
                        v1::TopicId::Subscribe as u32,
                        v1::Topics {
                            topics: vec![v1::Topic {
                                module: v1::Module::Host as u32,
                                sub: v1::HostSub::Runs as u32,
                                key: key.to_owned(),
                            }],
                        }
                        .encode_to_vec(),
                    )
                    .await;
                assert!(answer.fault.is_none());
                peer
            }

            async fn next(&mut self) -> v1::Frame {
                loop {
                    match self.rx.recv().await.expect("link frame") {
                        swiss_bus::IoMsg::Binary(bytes) => {
                            return v1::Frame::decode(bytes.as_slice()).expect("frame");
                        }
                        // The pump's keepalive ticks land here between frames; they
                        // are not link protocol for this harness.
                        swiss_bus::IoMsg::Ping(_) | swiss_bus::IoMsg::Pong(_) => continue,
                        other => panic!("expected binary, got {other:?}"),
                    }
                }
            }

            async fn request_at(&mut self, module: u32, sub: u32, id: u32, body: Vec<u8>) -> v1::Frame {
                let seq = self.seq;
                self.seq += 1;
                self.tx
                    .send(swiss_bus::IoMsg::Binary(
                        v1::Frame {
                            module,
                            sub,
                            id,
                            kind: v1::Kind::Request as i32,
                            seq,
                            body,
                            ..Default::default()
                        }
                        .encode_to_vec(),
                    ))
                    .await
                    .expect("send");
                loop {
                    let frame = self.next().await;
                    if frame.kind == v1::Kind::Event as i32 {
                        self.events.push_back(frame);
                        continue;
                    }
                    assert_eq!(frame.reply_to, seq);
                    return frame;
                }
            }

            async fn event(&mut self) -> v1::Frame {
                if let Some(event) = self.events.pop_front() {
                    return event;
                }
                let event = self.next().await;
                assert_eq!(event.kind, v1::Kind::Event as i32);
                event
            }

            /// True when nothing more is waiting — an event that "never left" is proven
            /// here, not by a timeout. Anything already arrived is parked, never eaten:
            /// try_recv alone would consume a frame and drop it.
            fn drained(&mut self) -> bool {
                loop {
                    match self.rx.try_recv() {
                        Ok(swiss_bus::IoMsg::Binary(bytes)) => {
                            self.events
                                .push_back(v1::Frame::decode(bytes.as_slice()).expect("frame"));
                        }
                        Ok(swiss_bus::IoMsg::Ping(_) | swiss_bus::IoMsg::Pong(_)) => continue,
                        Ok(other) => panic!("expected binary, got {other:?}"),
                        Err(_) => return self.events.is_empty(),
                    }
                }
            }
        }

        fn changed(frame: &v1::Frame) -> v1::RunChanged {
            assert_eq!(frame.module, v1::Module::Host as u32);
            assert_eq!(frame.sub, v1::HostSub::Runs as u32);
            assert_eq!(frame.id, v1::RunsId::RunsChanged as u32);
            assert_eq!(frame.kind, v1::Kind::Event as i32);
            v1::RunChanged::decode(frame.body.as_slice()).expect("RunChanged body")
        }

        fn contains(haystack: &[u8], needle: &[u8]) -> bool {
            haystack.windows(needle.len()).any(|window| window == needle)
        }

        // SPEC §host.actions: one run's events follow its states — queued, running,
        // terminal — in the order they were committed; a run that starts at once skips
        // queued. revision: 1 queued, 2 running, 3 terminal.
        #[tokio::test]
        async fn run_events_follow_queued_running_terminal() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 1,
                max_queued: 4,
            });
            let mut peer = Peer::open(&bus, "").await;
            let first = runs.submit(request("manual", "first", 120)).expect("starts at once");
            let second = runs
                .submit({
                    let mut r = request("manual", "second", 120);
                    r.queue_if_busy = true;
                    r
                })
                .expect("queues behind");

            let event = changed(&peer.event().await);
            assert_eq!(
                (event.run_id, event.revision, event.state),
                (first.run_id, 2, v1::RunState::Running as i32),
                "a run that starts at once begins at RUNNING"
            );
            assert!(event.started_at_ms.is_some());
            assert!(event.queued_at_ms > 0);
            let event = changed(&peer.event().await);
            assert_eq!(
                (event.run_id, event.revision, event.state),
                (second.run_id, 1, v1::RunState::Queued as i32),
                "a submission that waits enters at QUEUED"
            );
            assert!(event.started_at_ms.is_none());

            let _ = first.done.await;
            let _ = second.done.await;
            settle().await;
            let event = changed(&peer.event().await);
            assert_eq!((event.run_id, event.revision, event.state),
                (first.run_id, 3, v1::RunState::Succeeded as i32));
            let event = changed(&peer.event().await);
            assert_eq!((event.run_id, event.revision, event.state),
                (second.run_id, 2, v1::RunState::Running as i32));
            let event = changed(&peer.event().await);
            assert_eq!((event.run_id, event.revision, event.state),
                (second.run_id, 3, v1::RunState::Succeeded as i32));
            assert!(peer.drained(), "the lifecycle is complete");
        }

        // The terminal event leaves only after the record is acknowledged and the ring
        // holds the run: a reader acting on the event finds the run everywhere it should
        // be (SPEC §host.actions: Run events).
        #[tokio::test]
        async fn the_terminal_event_leaves_after_the_ack_and_the_ring() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 2,
                max_queued: 2,
            });
            let mut peer = Peer::open(&bus, "").await;
            let (release, gate) = oneshot::channel::<()>();
            let sink = Arc::new(GatedAck {
                started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                acked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                gate: Mutex::new(Some(gate)),
            });
            runs.add_history_sink(sink.clone());
            let submitted = runs.submit(request("manual", "held", 1)).expect("submitted");
            let running = changed(&peer.event().await);
            assert_eq!(running.run_id, submitted.run_id);
            for _ in 0..500 {
                if sink.started.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(sink.started.load(Ordering::SeqCst), "the run reached its sink");
            settle().await;
            assert!(
                peer.drained(),
                "the terminal event waits for the record's acknowledgment"
            );
            assert!(!runs.get(submitted.run_id).expect("run").state.is_terminal());

            release.send(()).expect("release the record");
            let event = changed(&peer.event().await);
            assert_eq!(
                (event.run_id, event.revision, event.state),
                (submitted.run_id, 3, v1::RunState::Succeeded as i32)
            );
            assert!(sink.acked.load(Ordering::SeqCst), "the sink finished first");
            assert!(runs.get(submitted.run_id).expect("run").state.is_terminal());
        }

        // A queued run a cancel drops — by id and by owner — publishes its terminal
        // CANCELED event too (SPEC §host.actions: Run events).
        #[tokio::test]
        async fn a_queued_run_dropped_by_cancel_publishes_canceled() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 1,
                max_queued: 4,
            });
            let mut peer = Peer::open(&bus, "").await;
            let hold = runs.submit(request("manual", "hold", 5_000)).expect("holds the pool");
            let via_cancel = runs.submit({
                let mut r = request("alice", "via-cancel", 100);
                r.queue_if_busy = true;
                r
            })
            .expect("queued");
            let via_owner = runs.submit({
                let mut r = request("bob", "via-owner", 100);
                r.queue_if_busy = true;
                r
            })
            .expect("queued");
            for (id, revision) in [(hold.run_id, 2), (via_cancel.run_id, 1), (via_owner.run_id, 1)] {
                let event = changed(&peer.event().await);
                assert_eq!((event.run_id, event.revision), (id, revision));
            }

            let view = runs.cancel(via_cancel.run_id).await.expect("canceled by id");
            assert_eq!(view.state, RunState::Canceled);
            assert_eq!(runs.cancel_owner("bob").await, 1, "canceled by owner");
            for id in [via_cancel.run_id, via_owner.run_id] {
                let event = changed(&peer.event().await);
                assert_eq!(
                    (event.run_id, event.revision, event.state),
                    (id, 3, v1::RunState::Canceled as i32),
                    "a dropped queued run reports its own terminal state"
                );
                assert!(event.started_at_ms.is_none());
            }

            runs.cancel(hold.run_id).await.expect("cancel the hold");
            let event = changed(&peer.event().await);
            assert_eq!(
                (event.run_id, event.revision, event.state),
                (hold.run_id, 3, v1::RunState::Canceled as i32)
            );
            settle().await;
            assert!(peer.drained());
        }

        // A submission that was refused never became a run: nothing is published for it
        // (SPEC §host.actions: Run events).
        #[tokio::test]
        async fn a_refused_submission_publishes_nothing() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 1,
                max_queued: 0,
            });
            let mut peer = Peer::open(&bus, "").await;
            let unknown = runs.submit(SubmitRequest {
                action_type: "no.such".into(),
                ..request("manual", "unknown", 10)
            });
            assert!(unknown.is_err());
            let hold = runs.submit(request("manual", "hold", 60)).expect("holds the pool");
            let full = runs.submit(request("manual", "full", 10));
            assert!(full.is_err(), "the pool is full and the queue holds nothing");
            let _ = hold.done.await;
            settle().await;

            // Exactly the one accepted run's own lifecycle; the refusals never appear.
            let event = changed(&peer.event().await);
            assert_eq!(event.run_id, hold.run_id);
            assert_eq!((event.revision, event.state), (2, v1::RunState::Running as i32));
            let event = changed(&peer.event().await);
            assert_eq!(event.run_id, hold.run_id);
            assert_eq!((event.revision, event.state), (3, v1::RunState::Succeeded as i32));
            assert!(peer.drained());
        }

        // The topic key is the action's namespace — the type up to its first dot — so a
        // page hears only the runs it shows (SPEC §bus.registry: HOST.RUNS).
        #[tokio::test]
        async fn run_events_are_keyed_by_action_namespace() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 4,
                max_queued: 4,
            });
            let mut remote = Peer::open(&bus, "remote").await;
            let mut all = Peer::open(&bus, "").await;
            let far = runs
                .submit(SubmitRequest {
                    action_type: "remote.sleep".into(),
                    input: json!({ "target": "dev", "ms": 1 }),
                    ..request("manual", "far", 1)
                })
                .expect("remote-namespace run");
            let near = runs.submit(request("manual", "near", 1)).expect("local run");
            let _ = far.done.await;
            let _ = near.done.await;
            settle().await;

            for revision in [2, 3] {
                let frame = remote.event().await;
                assert_eq!(frame.key, "remote", "the key is the namespace");
                let event = changed(&frame);
                assert_eq!((event.run_id, event.revision), (far.run_id, revision));
                assert_eq!(event.lane, "dev", "the lane names the target");
            }
            assert!(remote.drained(), "the remote key never carries the local pool");

            let mut seen = Vec::new();
            while !all.drained() {
                let event = changed(&all.event().await);
                seen.push((event.run_id, event.revision));
            }
            seen.sort_unstable();
            assert_eq!(
                seen,
                vec![
                    (far.run_id, 2),
                    (far.run_id, 3),
                    (near.run_id, 2),
                    (near.run_id, 3),
                ],
                "the empty key hears every namespace"
            );
        }

        // The body carries no free text: the label and the panic message stay behind
        // the owning module's route, where they can be masked (SPEC §remote.security,
        // §host.actions: Run events).
        #[tokio::test]
        async fn run_events_carry_no_free_text() {
            let (runs, bus, _) = coordinator_with_bus(RunCapacity {
                max_concurrent: 2,
                max_queued: 2,
            });
            let mut peer = Peer::open(&bus, "").await;
            let label = "L-sentinel-label-4f2c31";
            let submitted = runs
                .submit(SubmitRequest {
                    action_type: "test.panic".into(),
                    label: label.to_owned(),
                    input: json!({}),
                    ..request("manual", "boom", 1)
                })
                .expect("submitted");
            let view = submitted.done.await.expect("the final view");
            // The free text was real — it reached the view — so its absence below is
            // the event's doing, not the action's.
            assert_eq!(view.label, label);
            assert_eq!(view.error.as_deref(), Some("action panicked; the run was terminated"));
            settle().await;

            let mut frames = Vec::new();
            while !peer.drained() {
                frames.push(peer.event().await);
            }
            assert!(frames.len() >= 2, "the panicking run still reported");
            for frame in &frames {
                assert!(
                    !contains(frame.body.as_slice(), label.as_bytes()),
                    "a label never rides the bus"
                );
                assert!(
                    !contains(frame.body.as_slice(), b"action panicked"),
                    "an error message never rides the bus"
                );
            }
        }
    }
}
