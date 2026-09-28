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

//! The extensible Action contract — the shared execution vocabulary the whole toolbox
//! speaks (docs/09 §3, docs/10 §2).
//!
//! An Action is ONE callable capability with a stable string id ("process.exec",
//! "mcp.call", ...). The registry is a plain name -> impl map, NOT an
//! enum: adding a capability means registering an impl, and Jobs never learns any of
//! their names — a configured definition carries the id as data and the registry resolves
//! it at run time. That is the whole point of the layer: the scheduler must not grow a
//! match arm (or a recompile) per capability.
//!
//! Deliberately small on purpose:
//! - The input arrives as JSON and is validated by the action itself (it owns its schema;
//!   the registry only guarantees the action exists).
//! - Cancellation is a watch-backed handle, not an abort: an action that cannot stop early
//!   simply never looks at it, and reports what it did — "cannot cancel" stays an honest
//!   outcome, not a dropped thread.
//! - Every failure mode is a VALUE ([ActionError]/err fields of [ActionOutcome]), never a
//!   panic — one broken capability must not take the host or another plugin down.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::services::runs::RunOutputBuffer;

/// The observing half of a cancellation flag, shared by every reader of one run. Watch
/// (not a broadcast channel): O(1) per reader, and a late subscriber still sees the flag.
#[derive(Clone)]
pub struct CancelHandle {
    rx: watch::Receiver<bool>,
}

impl CancelHandle {
    /// A handle that can never be cancelled — the shape used by direct (uncoordinated)
    /// callers such as the legacy one-shot runner. The sender lives in a process-wide
    /// static and is never dropped ON PURPOSE: `cancelled()` treats a dropped sender as
    /// "no cancellation can ever arrive" and returns, so a per-call source dropped on the
    /// spot would make this handle read as INSTANTLY cancelled — every reader would stop
    /// at its first poll and the output would come back empty.
    pub fn never() -> Self {
        static NEVER: OnceLock<watch::Sender<bool>> = OnceLock::new();
        let tx = NEVER.get_or_init(|| watch::channel(false).0);
        CancelHandle { rx: tx.subscribe() }
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves when cancellation is requested (immediately, if it already was). A dropped
    /// sender resolves too — it means nobody can cancel anymore, so the caller finishes
    /// normally instead of waiting forever.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return; // sender gone: no cancellation can ever arrive
            }
        }
    }
}

/// The write half of a [CancelHandle]: exactly one per run, held by whoever owns the
/// run's lifecycle (the run coordinator, or a test).
pub struct CancelSource {
    tx: watch::Sender<bool>,
}

impl CancelSource {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(false);
        CancelSource { tx }
    }

    /// The observing half. Any number of readers (run task, pipe readers).
    pub fn handle(&self) -> CancelHandle {
        CancelHandle {
            rx: self.tx.subscribe(),
        }
    }

    /// Request cancellation. Idempotent; never blocks.
    ///
    /// `send_replace`, not `send`: `send` REFUSES (and leaves the value untouched) when no
    /// receiver is subscribed at that instant, which would silently lose a cancel that
    /// arrives before the run took its handle — the handle would then subscribe to a
    /// channel still reading `false` and the run would never stop.
    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }
}

impl Default for CancelSource {
    fn default() -> Self {
        Self::new()
    }
}

/// What one action execution produced. "ok" means the capability reports success; every
/// other field is evidence about WHICH kind of run happened, so callers (runlog, API,
/// scheduler) can record it without knowing the action.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub ok: bool,
    pub ms: u64,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    pub error: Option<String>,
    /// Captured output, already bounded by the action's own limit. KEPT SMALL: this value
    /// may be cached in run history, so actions must not return unbounded text.
    pub output: String,
    /// Length of the full output before any capping, so a reader knows there was more.
    pub chars: usize,
    /// Action-specific extras (e.g. "outputTruncated": true). Absent-not-null convention.
    pub meta: Map<String, Value>,
}

impl ActionOutcome {
    /// A minimal success outcome (actions that produce nothing else start here).
    pub fn ok() -> Self {
        ActionOutcome {
            ok: true,
            ms: 0,
            pid: None,
            exit_code: None,
            timed_out: false,
            canceled: false,
            error: None,
            output: String::new(),
            chars: 0,
            meta: Map::new(),
        }
    }
}

/// Why an action refused to run. Distinct from a FAILED run: this is "did not start".
#[derive(Debug, Clone)]
pub enum ActionError {
    /// The input does not match the action's schema.
    InvalidInput(String),
    /// The run could not be started at all (missing provider, spawn failure, ...).
    Failed(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::InvalidInput(m) | ActionError::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// One registered capability, as GET /api/actions shows it.
#[derive(Debug, Clone)]
pub struct ActionInfo {
    pub type_name: String,
    pub title: String,
    pub provider: String,
    pub cancelable: bool,
    pub schema: Value,
}

/// A copy of one run's output stream as it is appended — the durable half of a
/// [crate::services::runs::RunHistorySink]. Writes ride on the synchronous buffer append,
/// so an implementation bounds its own cost (a capped file, never a growing Vec) and
/// never blocks on anything but the write itself.
pub trait RunOutputTee: Send + Sync {
    fn write(&self, bytes: &[u8]);
}

/// The write half of one run's live output, handed to an action through
/// [ActionContext]. Appends are synchronous and bounded: they land in the run's
/// [RunOutputBuffer] (cap enforced there) and never block, so an action may append from
/// inside a select arm without stalling anything. Every append is also copied to the
/// tees a history sink attached at run start (none for most runs).
///
/// An action that never looks at it loses nothing — the buffer simply stays empty and
/// the run's outcome text is the only output, exactly as before this seam existed.
#[derive(Clone)]
pub struct RunOutputSink {
    buffer: Arc<RunOutputBuffer>,
    tees: Arc<[Arc<dyn RunOutputTee>]>,
}

impl RunOutputSink {
    pub(crate) fn new(buffer: Arc<RunOutputBuffer>, tees: Vec<Arc<dyn RunOutputTee>>) -> Self {
        RunOutputSink {
            buffer,
            tees: tees.into(),
        }
    }

    /// Append output text to the run's live buffer. Bounded by the buffer's own cap
    /// (oldest data evicted, cursor keeps advancing) — an hours-long Yocto build may
    /// stream gigabytes through here while the gateway holds kilobytes.
    pub fn append(&self, text: &str) {
        self.append_bytes(text.as_bytes());
    }

    /// Append raw bytes (lossy when they split a character; cursors stay byte-based).
    pub fn append_bytes(&self, bytes: &[u8]) {
        self.buffer.append(bytes);
        for tee in self.tees.iter() {
            tee.write(bytes);
        }
    }

    /// The cursor after everything appended so far — the "caught up to" mark a follower
    /// would poll with next.
    pub fn cursor(&self) -> u64 {
        self.buffer.total_bytes()
    }
}

/// What [Action::execute_with_context] receives: the cancel handle every action already
/// knows, plus the run's live output sink. One struct so the contract can grow another
/// capability later without a second blanket-rename of every action.
pub struct ActionContext {
    pub cancel: CancelHandle,
    pub output: RunOutputSink,
}

/// Where a run's work happens - which of the coordinator's bounds it counts against
/// (2026-09-28). The shared pool (maxConcurrentRuns, 2 by default) exists to protect THIS
/// machine: a local process costs its CPU. A remote action's work runs on another machine
/// and costs one channel on that target's SSH connection here, so it takes a slot in the
/// target's own lane instead - several terminals driving one server in parallel must not
/// queue behind two local slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLane {
    Local,
    /// The remote target's name, as the run's input names it.
    Remote(String),
}

/// One callable capability. Implementations are shared as Arc<dyn Action> through the
/// registry; they must therefore be stateless or internally synchronized.
#[async_trait]
pub trait Action: Send + Sync {
    /// The stable capability id jobs config and API clients refer to.
    fn type_name(&self) -> &'static str;

    /// Human-facing name (defaults to the id).
    fn title(&self) -> String {
        self.type_name().to_string()
    }

    /// Which plugin provided the capability (display/diagnosis only).
    fn provider(&self) -> &'static str {
        "builtin"
    }

    /// Whether the run coordinator should offer cancel for runs of this action. An action
    /// that cannot honour the handle must say false here — cancelling must not lie.
    fn cancelable(&self) -> bool {
        true
    }

    /// Which bound a run of this input counts against ([RunLane]). Default: local.
    fn lane(&self, _input: &Value) -> RunLane {
        RunLane::Local
    }

    /// The input schema, as a JSON-schema-ish object the config editor and GET /api/actions
    /// render. Metadata for humans; validation lives in [Action::execute].
    fn schema(&self) -> Value;

    /// Validate an input WITHOUT running it - the config-save path's door to the same
    /// check execute() applies (docs/11 §3.4: action.input is validated by the resolved
    /// capability's own schema). Default: any object-shaped input passes, which stays
    /// correct for capabilities with no pre-run parse of their own.
    fn validate_input(&self, _input: &Value) -> Result<(), String> {
        Ok(())
    }

    /// Run the capability to completion. Implementations must return within the caller's
    /// timeout policy and must honour "cancel" when [Action::cancelable] is true.
    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError>;

    /// The streaming-aware entry point (docs/34 §16): same contract as [Action::execute]
    /// plus a live output sink. The DEFAULT delegates to `execute`, so every existing
    /// action keeps working unchanged and migrates at its own pace; the run coordinator
    /// calls only this one. An action that produces output incrementally overrides this,
    /// appends as output arrives, and still returns the bounded tail in its outcome.
    async fn execute_with_context(
        &self,
        input: &Value,
        ctx: ActionContext,
    ) -> Result<ActionOutcome, ActionError> {
        self.execute(input, ctx.cancel).await
    }
}

/// Name -> impl map. Registration refuses duplicates: two providers claiming one id is a
/// composition bug to surface at boot, not a silent last-write-wins.
#[derive(Default)]
pub struct ActionRegistry {
    actions: Mutex<HashMap<&'static str, Arc<dyn Action>>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a capability. Err names the duplicate when the id is already taken.
    pub fn register(&self, action: Arc<dyn Action>) -> Result<(), String> {
        let name = action.type_name();
        let mut actions = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        if actions.contains_key(name) {
            return Err(format!("action {name:?} is already registered"));
        }
        actions.insert(name, action);
        Ok(())
    }

    /// Withdraw a capability — what a plugin's stop does to its contributions (docs/09 §4:
    /// stopping deregisters contributions). New submissions then fail with "unknown action";
    /// runs already in flight are the coordinator's business, not the registry's.
    pub fn unregister(&self, type_name: &str) {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(type_name);
    }

    /// Resolve a capability id from config/API input.
    pub fn get(&self, type_name: &str) -> Option<Arc<dyn Action>> {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(type_name)
            .cloned()
    }

    /// Every registered capability, sorted by id so listings are stable across polls.
    pub fn list(&self) -> Vec<ActionInfo> {
        let actions = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        let mut infos: Vec<ActionInfo> = actions
            .values()
            .map(|a| ActionInfo {
                type_name: a.type_name().to_string(),
                title: a.title(),
                provider: a.provider().to_string(),
                cancelable: a.cancelable(),
                schema: a.schema(),
            })
            .collect();
        infos.sort_by(|a, b| a.type_name.cmp(&b.type_name));
        infos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Echo;

    #[async_trait]
    impl Action for Echo {
        fn type_name(&self) -> &'static str {
            "test.echo"
        }
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(
            &self,
            input: &Value,
            _cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            let mut out = ActionOutcome::ok();
            out.output = input.to_string();
            out.chars = out.output.chars().count();
            Ok(out)
        }
    }

    #[test]
    fn the_registry_resolves_by_name_and_refuses_duplicates() {
        let reg = ActionRegistry::new();
        assert!(reg.get("test.echo").is_none(), "empty registry");
        reg.register(Arc::new(Echo)).expect("first insert");
        assert!(
            reg.register(Arc::new(Echo)).is_err(),
            "a duplicate id is a composition bug, not an overwrite"
        );
        let a = reg.get("test.echo").expect("resolved");
        assert_eq!(a.type_name(), "test.echo");
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].cancelable, "default is cancelable");
    }

    #[tokio::test]
    async fn a_cancel_source_signals_every_handle() {
        let src = CancelSource::new();
        let h1 = src.handle();
        let h2 = src.handle();
        assert!(!h1.is_cancelled());
        src.cancel();
        src.cancel(); // idempotent
        assert!(h2.is_cancelled());
        h1.cancelled().await; // resolves even for a handle taken before the cancel
        assert!(!CancelHandle::never().is_cancelled());
    }
}
