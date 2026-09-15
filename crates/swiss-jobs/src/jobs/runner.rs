//! What one job run PRODUCED — the record shape, and nothing else.
//!
//! This module used to own the execution too: its own spawn, its own tokenizer call, its own
//! process-tree teardown. It no longer does. Running a command is a shared capability now
//! (`process.legacy-command` over `services::process::Supervisor`), submitted through the
//! shared [`swiss_host::services::runs::RunCoordinator`] like every other run in the gateway
//! (docs/10 §8: "进程能力上移到共享服务", §9 step 2). Keeping a second spawner here would mean
//! two execution paths with two sets of teardown bugs — exactly what the shared service
//! exists to prevent.
//!
//! What DID move is only the machinery. The legacy behaviour a saved job depends on is
//! unchanged and still tested at its new home: the old argv tokenizer (quoting rules and all),
//! PATH+PATHEXT resolution, lenient `${ENV_VAR}` expansion at RUN time so the file keeps the
//! reference rather than the secret (vault `${secret://...}` references resolve at the same point,
//! strictly — a missing one fails the run, docs/19 D4), GBK/lossy decoding, CREATE_NO_WINDOW,
//! and subtree teardown on both platforms.
//!
//! What stays here is the outcome type the run log and the /api/jobs reply are written
//! against, so those shapes cannot drift with the executor underneath them.

/// Default per-run deadline: generous (backup/export scripts run long), but finite, so a
/// wedged command cannot pin a slot forever. The coordinator enforces it by cancellation.
pub const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// Everything one execution produced. "ok" is exactly "exit code 0 within the deadline"; every
/// other path keeps its own flag so the record can say WHICH kind of not-ok it was.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub ok: bool,
    pub ms: u64,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The run was cancelled — the jobs plugin stopped, or the gateway shut down. Distinct
    /// from a deadline: nobody waited too long, someone asked it to end.
    pub canceled: bool,
    /// A failure to run at all (empty command, spawn error), or a refusal to start it
    /// (missing capability, full pool). A recorded field, never an exception.
    pub error: Option<String>,
    /// The combined output (stdout, then stderr), decoded, masked and tail-capped.
    pub output: String,
    /// Size of the full output before capping, so a reader knows there was more.
    pub chars: usize,
    /// True when the tail cap dropped bytes.
    pub output_truncated: bool,
}

impl RunOutcome {
    /// A run that never started: the capability was missing, or the pool refused it. Recorded
    /// exactly like a failed run so the history shows the occurrence instead of hiding it.
    pub fn refused(error: String) -> Self {
        RunOutcome {
            ok: false,
            ms: 0,
            pid: None,
            exit_code: None,
            timed_out: false,
            canceled: false,
            error: Some(error),
            output: String::new(),
            chars: 0,
            output_truncated: false,
        }
    }

    /// The coordinator's terminal view of a run, as this subsystem records it. The states map
    /// one-to-one: only `succeeded` is ok, and timeout/cancel keep their own flag so the
    /// record says which kind of not-ok happened.
    pub fn from_run_view(view: &swiss_host::services::runs::RunView) -> Self {
        use swiss_host::services::runs::RunState;
        RunOutcome {
            ok: view.state == RunState::Succeeded,
            ms: view.ms.unwrap_or(0),
            pid: view.pid,
            exit_code: view.exit_code,
            timed_out: view.timed_out,
            canceled: view.canceled,
            error: view.error.clone(),
            output: view.output.clone().unwrap_or_default(),
            chars: view.chars.unwrap_or(0),
            output_truncated: view.output_truncated,
        }
    }
}
