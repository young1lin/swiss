//! One command execution - the "run" half of a scheduled job. Everything about a run that can
//! fail is captured as data in the returned [RunOutcome] (the "try" of the feature): a spawn
//! failure, a timeout, a non-zero exit - each becomes a recorded field, never a panic, never
//! something that takes the scheduler or another job down with it.
//!
//! It reuses the proc adapter's process machinery on purpose: argv tokenization (quoting
//! rules), PATH+PATHEXT program resolution, GBK/lossy output decoding for children that speak
//! a Chinese Windows code page, CREATE_NO_WINDOW so a console-less daemon never flashes a
//! window, and subtree teardown on BOTH platforms - a Windows kill-on-close job object, and on
//! Unix a private process group whose kill(-pgid) reaches every descendant - so a scheduled
//! command never leaks memory-holding descendants past its timeout. (The GBK branch is
//! Windows-typical but harmless anywhere: valid UTF-8 short-circuits it first.)
//!
//! ${ENV_VAR} references in the command and the working directory are expanded HERE, at run
//! time - the same build-time-only rule every other adapter follows, so a saved job holds the
//! reference, not the secret.

use std::time::Instant;

use crate::adapters::proc::{decode_child_output, tokenize_command};
use crate::config::resolve_env_refs;

/// Default per-run deadline: generous (backup/export scripts run long), but finite, so a wedged
/// command cannot pin a slot forever.
pub const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Kept of the combined output, tail-first: the end of a log is where the error lives.
const OUTPUT_MAX: usize = 16 * 1024;
/// Grace period for draining output pipes after the process is gone: a descendant that
/// inherited the pipe can hold it open briefly, and it must not turn into a second hang.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Everything one execution produced. "ok" is exactly "exit code 0 within the deadline"; every
/// other path keeps its own flag so the record can say WHICH kind of not-ok it was.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub ok: bool,
    pub ms: u64,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// A failure to run at all (empty command, spawn error, unreadable status).
    pub error: Option<String>,
    /// The combined output (stdout, then stderr), decoded, tail-capped to OUTPUT_MAX chars.
    pub output: String,
    /// Length of the full output before capping, so a reader knows there was more.
    pub chars: usize,
}

/// Run one command to completion (or its deadline) and report. Never panics, never returns
/// Err: every failure mode is a field of the outcome, because a scheduled job's failure is a
/// RECORD, not an exception for a caller to unwind.
pub async fn run_command(command: &str, cwd: Option<&str>, timeout_ms: u64) -> RunOutcome {
    let started = Instant::now();
    let fail = |error: String| RunOutcome {
        ok: false,
        ms: started.elapsed().as_millis() as u64,
        pid: None,
        exit_code: None,
        timed_out: false,
        error: Some(error),
        output: String::new(),
        chars: 0,
    };

    let expanded = resolve_env_refs(command);
    let argv = tokenize_command(&expanded);
    let Some((program, args)) = argv.split_first() else {
        return fail("empty command".into());
    };
    // Resolve through PATH+PATATEXT like the proc adapter does, so "lmg" finds lmg.exe and the
    // failure mode of an unresolvable name is the OS's own spawn error naming the command.
    let path = std::env::var("PATH").unwrap_or_default();
    let resolved = crate::pathenv::resolve_command(program, &path)
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| program.into());

    let mut cmd = tokio::process::Command::new(&resolved);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(resolve_env_refs(cwd));
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW - without it a console-less daemon flashes a console window for
        // every scheduled child (what windowsHide: true maps to in Node's spawn).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        // Private process group (pgid == the child's pid): one kill(-pgid) at the deadline
        // then reaches sh -c 'a & b &' descendants at any depth, with no PID walk - the Unix
        // counterpart of the Windows job object. The group also insulates the job from
        // terminal signals aimed at the gateway's own group.
        use std::os::unix::process::CommandExt as _;
        cmd.as_std_mut().process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => return fail(format!("spawn {program:?} failed: {err}")),
    };
    let pid = child.id();

    // The subtree guard, assigned at spawn: everything the command goes on to create inherits
    // the job, so one handle owns the whole tree - for the timeout kill AND for the moment the
    // run ends while a backgrounded descendant is still alive.
    #[cfg(windows)]
    let job = child
        .raw_handle()
        .and_then(crate::adapters::proc::win::KillOnCloseJob::assign);

    // Readers run as their own tasks so a timeout cancel cannot discard what was already read:
    // after the kill the pipes close, the tasks finish, and the partial output is still there.
    let out_task = child.stdout.take().map(|mut s| {
        tokio::spawn(async move {
            let mut b = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut b).await;
            b
        })
    });
    let err_task = child.stderr.take().map(|mut s| {
        tokio::spawn(async move {
            let mut b = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut b).await;
            b
        })
    });

    let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
    let (timed_out, status) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (false, Some(status)),
        Ok(Err(err)) => {
            kill_subtree(&mut child).await;
            let output = drain(out_task, err_task).await;
            return RunOutcome {
                ok: false,
                ms: started.elapsed().as_millis() as u64,
                pid,
                exit_code: None,
                timed_out: false,
                error: Some(format!("wait failed: {err}")),
                chars: output.chars().count(),
                output,
            };
        }
        Err(_elapsed) => {
            kill_subtree(&mut child).await;
            (true, None)
        }
    };

    // The root is gone; drop the job so descendants still holding the pipes die and the reader
    // tasks can finish. Belt-and-braces with kill_on_drop above.
    #[cfg(windows)]
    drop(job);

    let exit_code = status.and_then(|s| s.code());
    let output = drain(out_task, err_task).await;
    let chars = output.chars().count();
    RunOutcome {
        ok: !timed_out && exit_code == Some(0),
        ms: started.elapsed().as_millis() as u64,
        pid,
        exit_code,
        timed_out,
        error: None,
        chars,
        output: if chars > OUTPUT_MAX {
            output.chars().skip(chars - OUTPUT_MAX).collect()
        } else {
            output
        },
    }
}

/// Kill the whole subtree of a still-live child, then reap it so the OS does not carry a
/// zombie. Windows: the kill-on-close job handle the caller still holds covers any depth,
/// PID-reuse safe. Unix: kill(-pgid) takes the private group set at spawn - descendants at any
/// depth; the group dies with its last member, so the pid-reuse window (root gone, group gone,
/// pid reassigned as a NEW group leader) is vanishingly narrow, and a stale signal simply
/// returns ESRCH. start_kill then takes the root itself either way.
async fn kill_subtree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) on a process group this run created at spawn; the return value is
        // ignored because the wait below is the source of truth for the root's death.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(DRAIN_GRACE, child.wait()).await;
}

/// Collect the reader tasks' bytes into one decoded, section-labelled string. Bounded by a
/// grace period: the process is dead by the time this runs, so pipes only stay open briefly.
async fn drain(
    out_task: Option<tokio::task::JoinHandle<Vec<u8>>>,
    err_task: Option<tokio::task::JoinHandle<Vec<u8>>>,
) -> String {
    let take = |t: Option<tokio::task::JoinHandle<Vec<u8>>>| async move {
        match t {
            Some(t) => tokio::time::timeout(DRAIN_GRACE, t)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default(),
            None => Vec::new(),
        }
    };
    let (out_bytes, err_bytes) = tokio::join!(take(out_task), take(err_task));
    let stdout = decode_child_output(&out_bytes);
    let stderr = decode_child_output(&err_bytes);
    let mut combined = String::with_capacity(stdout.len() + stderr.len() + 10);
    combined.push_str(&stdout);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(&stderr);
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that prints, portably per platform: the repo develops on Windows, but the
    /// suite must keep passing anywhere the toolchain runs.
    fn echo(text: &str) -> String {
        if cfg!(windows) {
            format!("cmd /c echo {text}")
        } else {
            format!("sh -c 'echo {text}'")
        }
    }

    #[tokio::test]
    async fn a_successful_command_reports_output_and_exit_zero() {
        let out = run_command(&echo("hi-from-job"), None, 30_000).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.output.contains("hi-from-job"), "{out:?}");
        assert!(out.error.is_none());
        assert!(!out.timed_out);
        assert!(out.pid.is_some());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_recorded_failure_not_an_exception() {
        let cmd = if cfg!(windows) {
            "cmd /c exit 3"
        } else {
            "sh -c 'exit 3'"
        };
        let out = run_command(cmd, None, 30_000).await;
        assert!(!out.ok);
        assert_eq!(out.exit_code, Some(3));
        assert!(out.error.is_none(), "the command ran; it just failed");
    }

    #[tokio::test]
    async fn a_wedged_command_is_killed_at_its_deadline() {
        let cmd = if cfg!(windows) {
            // ping is the classic Windows sleep: ~1s per echo.
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let started = Instant::now();
        let out = run_command(cmd, None, 400).await;
        assert!(out.timed_out, "{out:?}");
        assert!(!out.ok);
        assert!(out.ms >= 350 && out.ms < 10_000, "{out:?}");
        assert!(started.elapsed().as_secs() < 10, "must not wait the full command");
    }

    #[tokio::test]
    async fn an_empty_or_unspawnable_command_is_reported_not_panicked() {
        let out = run_command("   ", None, 1_000).await;
        assert!(!out.ok);
        assert_eq!(out.error.as_deref(), Some("empty command"));

        let out = run_command("definitely-not-a-real-program-xyz --flag", None, 1_000).await;
        assert!(!out.ok, "{out:?}");
        assert!(out.error.is_some(), "{out:?}");
        // The error names the program, so the job log says what could not be found.
        assert!(out.error.unwrap().contains("definitely-not-a-real-program-xyz"));
    }

    #[tokio::test]
    async fn env_refs_in_the_command_expand_at_run_time() {
        // A ref that resolves is used; one that does not resolves to empty (the credential
        // model's behaviour everywhere else in the gateway).
        unsafe { std::env::set_var("LMG_JOB_TEST_WORD", "expanded-word") };
        let cmd = if cfg!(windows) {
            "cmd /c echo ${LMG_JOB_TEST_WORD}"
        } else {
            "sh -c 'echo ${LMG_JOB_TEST_WORD}'"
        };
        let out = run_command(cmd, None, 30_000).await;
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("expanded-word"), "{out:?}");
    }
}
