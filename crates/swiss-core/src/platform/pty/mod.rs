//! Local pseudo-terminals (docs/14 §5): ConPTY on Windows, `openpty` on unix.
//!
//! The whole point of this module is that `unsafe` stops here. The terminal plugin above it
//! opens a shell, reads bytes, writes bytes and resizes a grid; it never sees a HANDLE, an fd or
//! an attribute list. That is AGENTS.md's rule ("unsafe belongs at the Windows FFI boundary")
//! applied to a feature that would otherwise sprinkle it across a whole crate — and it is why
//! `portable-pty` is not here: this is ~300 lines of FFI against a `windows` crate the gateway
//! already links, versus a dependency tree with its own winpty compatibility path, in a process
//! whose whole budget is 15 MB (docs/14 §5).
//!
//! ## The shape, and why it is split in two
//!
//! [`open_pty`] hands back a pair:
//!
//! - [`PtyHandle`] is the async side: cheap to clone, `Send + Sync`, and every method on it
//!   returns promptly. Writing, resizing, asking whether the child is gone, killing it.
//! - [`PtyPump`] is the blocking side: exactly one exists, it is moved into a `spawn_blocking`
//!   thread, and [`PtyPump::read`] blocks. **Dropping it is what tears the session down** — the
//!   pty, the child, and the child's whole subtree.
//!
//! The split is not decoration. A ConPTY's output is an anonymous pipe, which tokio cannot poll,
//! so a local session costs one blocking thread and that thread is the direct reason local
//! sessions are capped (docs/14 §7). Putting teardown in the pump rather than in the handle is
//! what keeps that thread from leaking: on Windows conhost owns the write end of the output pipe
//! and holds it open after the child exits, so a reader blocked in `ReadFile` would never return
//! and the thread would be lost for the life of the process. The Windows reader therefore polls
//! (`PeekNamedPipe`) instead of blocking, and closes the read handle before closing the
//! pseudoconsole — closing a pseudoconsole whose output nobody is draining is the documented way
//! to hang. Unix has neither problem: killing the process group closes the last slave fd and the
//! master read ends by itself.
//!
//! ## What it deliberately does not do
//!
//! No echo policy, no line discipline, no scrollback, no recording, no idle timeout. Those are
//! session-machine concerns and live one layer up, where they can be tested without a real
//! process. This module opens a pty and gets out of the way.

#[cfg(windows)]
mod conpty;
#[cfg(windows)]
pub use conpty::{
    default_shell, default_shell_in, find_on_path, find_on_path_with, open_pty,
    resolve_program, resolve_program_in, shell_candidates, shell_candidates_in, PtyHandle,
    PtyPump,
};

#[cfg(not(windows))]
mod openpty;
#[cfg(not(windows))]
pub use openpty::{default_shell, open_pty, resolve_program, shell_candidates, PtyHandle, PtyPump};

/// One shell this host can offer a local terminal, as the settings sheet lists it: the
/// program (an absolute path — what would actually run) and a human label. Plain data in
/// swiss-core so the pty seam, the session machine and the /targets JSON share one type
/// instead of mapping between three (docs/15 §2.1).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ShellCandidate {
    pub program: String,
    pub label: String,
}

/// Terminal geometry in character cells.
///
/// Unvalidated on purpose: the bounds that matter (1..=1000) belong to the session contract in
/// `swiss-host`, which rejects a bad resize frame with a message the user sees. Repeating them here
/// would mean two answers to "how big may a terminal be", and the platform's own answer — what
/// the kernel or conhost accepts — is the one this struct passes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtyGeometry {
    pub cols: u16,
    pub rows: u16,
}

impl PtyGeometry {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// What to run on the far side of the pty.
#[derive(Clone, Debug, Default)]
pub struct PtyCommand {
    /// The program, resolved through PATH exactly as a shell would resolve it.
    pub program: String,
    pub args: Vec<String>,
    /// Added to the gateway's own environment, not replacing it: a shell with no PATH, no HOME
    /// and no SystemRoot is not a shell anyone can use.
    pub env: Vec<(String, String)>,
    /// Dropped from the inherited environment before `env` is applied. A daemon's
    /// environment is an accident of whoever launched it — an agent harness that sets
    /// NO_COLOR for its own tool shells, say — and a shell started from a browser tab
    /// must not inherit that launcher's opinion of what a terminal can draw.
    pub env_remove: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
}

impl PtyCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Default::default()
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    pub fn cwd(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// `echo hi` in the platform's own shell, run non-interactively so it exits on its own.
    fn echo_hi() -> PtyCommand {
        #[cfg(windows)]
        {
            PtyCommand::new("cmd.exe").arg("/c").arg("echo hi")
        }
        #[cfg(not(windows))]
        {
            PtyCommand::new("/bin/sh").arg("-c").arg("echo hi")
        }
    }

    /// A child that stays up until it is killed — the teardown tests need something to kill.
    fn a_long_wait() -> PtyCommand {
        #[cfg(windows)]
        {
            PtyCommand::new("cmd.exe")
                .arg("/c")
                .arg("ping -n 60 127.0.0.1 > nul")
        }
        #[cfg(not(windows))]
        {
            PtyCommand::new("/bin/sh").arg("-c").arg("sleep 60")
        }
    }

    /// Wait for the child to be gone. Killing a process is a request, not an event: the pump
    /// can be back from its read before the kernel has marked the process object signalled, so
    /// every "it died" assertion polls rather than reading the status once.
    fn wait_for_exit(handle: &PtyHandle, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if handle.exit_code().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.exit_code().is_some()
    }

    /// Read until the far side is gone or the deadline passes. Returns everything it saw, lossily
    /// decoded: a pty carries escape sequences, and no test here cares which ones.
    fn drain(pump: &mut PtyPump, deadline: Duration) -> String {
        let started = Instant::now();
        let mut seen: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        while started.elapsed() < deadline {
            match pump.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&seen).into_owned()
    }

    #[test]
    fn a_command_run_in_a_local_pty_comes_back_through_the_pump() {
        // The floor of the whole feature: a real child, a real pty, real bytes. If this passes,
        // the FFI is wired the right way round — pipes, pseudoconsole, attribute list and all.
        let (handle, mut pump) = open_pty(&echo_hi(), PtyGeometry::new(80, 24)).expect("opens");
        let output = drain(&mut pump, Duration::from_secs(15));
        assert!(
            output.contains("hi"),
            "no echo in the pty output: {output:?}"
        );
        // EOF, not a timeout: the pump has to notice the child left of its own accord, or a
        // finished session would sit there holding a blocking thread for ever.
        assert_eq!(pump.read(&mut [0u8; 16]).ok(), Some(0), "the pump ended");
        assert_eq!(
            handle.exit_code(),
            Some(0),
            "and reported how the child left"
        );
    }

    #[test]
    fn a_resize_on_a_live_pty_is_accepted() {
        let (handle, mut pump) = open_pty(&a_long_wait(), PtyGeometry::new(80, 24)).expect("opens");
        handle
            .resize(PtyGeometry::new(132, 43))
            .expect("the pty takes a new size");
        // A window change is not an error even when nothing is listening for SIGWINCH yet.
        handle
            .resize(PtyGeometry::new(80, 24))
            .expect("and takes it back");
        handle.kill();
        drain(&mut pump, Duration::from_secs(15));
    }

    #[test]
    fn dropping_the_pump_ends_the_child_it_was_reading() {
        // ADR-008 at this layer: the session's lifetime is the pump's lifetime. Nothing else in
        // the gateway has to remember to clean up after a terminal tab.
        let (handle, pump) = open_pty(&a_long_wait(), PtyGeometry::new(80, 24)).expect("opens");
        assert_eq!(handle.exit_code(), None, "still running");
        let pid = handle.pid();
        assert!(pid > 0);
        drop(pump);
        assert!(
            wait_for_exit(&handle, Duration::from_secs(10)),
            "the child outlived the pump that owned it"
        );
        assert!(
            !crate::platform::pid_alive(pid),
            "pid {pid} is still on the machine"
        );
    }

    #[test]
    fn killing_from_the_handle_unblocks_the_pump() {
        // The forced-close path (idle timeout, a stalled send queue): the async side calls kill,
        // and the blocking thread must come back rather than sit in a read for ever.
        let (handle, mut pump) = open_pty(&a_long_wait(), PtyGeometry::new(80, 24)).expect("opens");
        let reader = std::thread::spawn(move || {
            let text = drain(&mut pump, Duration::from_secs(20));
            (text, pump)
        });
        std::thread::sleep(Duration::from_millis(200));
        handle.kill();
        let (_, _pump) = reader.join().expect("the reading thread came back");
        assert!(
            wait_for_exit(&handle, Duration::from_secs(10)),
            "the child is gone"
        );
    }

    #[test]
    fn the_command_builder_keeps_what_it_was_given() {
        let cmd = PtyCommand::new("bash")
            .arg("-l")
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .cwd("/tmp");
        assert_eq!(cmd.program, "bash");
        assert_eq!(cmd.args, vec!["-l".to_string()]);
        assert_eq!(cmd.env, vec![("TERM".to_string(), "xterm-256color".into())]);
        assert_eq!(cmd.env_remove, vec!["NO_COLOR".to_string()]);
        assert_eq!(cmd.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
    }

    #[test]
    fn the_default_shell_is_something_that_exists_on_this_host() {
        let shell = default_shell();
        assert!(!shell.program.is_empty());
        let (handle, mut pump) =
            open_pty(&shell, PtyGeometry::new(80, 24)).expect("the shell opens");
        // An interactive shell prints a prompt and then waits; it must not have exited already.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(handle.exit_code(), None, "the shell exited immediately");
        handle.kill();
        drain(&mut pump, Duration::from_secs(15));
    }
}
