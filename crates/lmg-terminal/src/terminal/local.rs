//! The local shell source (docs/14 §5) — the other half of what the session machine can
//! open, and the mirror image of the remote one.
//!
//! Remote sessions arrive through the shell capability seat in `lmg-host`, provided by
//! the tunnels plugin. Local sessions have no provider to ask: the PTY is right here, on
//! the other side of `lmg_core::platform::pty`. [`LocalShell`] is the seam that makes the
//! two look the same to the driver — both hand back a
//! [`lmg_host::services::shell::PtySession`] — and it is a trait rather than a function
//! so the session machine can be tested against a scripted shell on any host, with no
//! process, no ConPTY and no timing.
//!
//! ## The blocking thread, and where the backpressure goes
//!
//! A ConPTY's output is an anonymous pipe (docs/14 §5), so reading it costs one
//! `spawn_blocking` thread for the life of the session — the direct reason local sessions
//! are capped. That thread cannot `await`, so it hands chunks to an async forwarder over
//! a channel of **depth one**. The depth is the whole design: with one slot, a forwarder
//! parked on a full consumer queue leaves the reader parked on `blocking_send`, which
//! leaves the pipe unread, which is exactly how a real tty pushes back on a program that
//! is printing faster than anyone is reading. Nothing is dropped anywhere along it
//! (docs/14 §6.8).
//!
//! Teardown is by ownership, not by protocol. The reader thread owns the [`PtyPump`], and
//! dropping the pump kills the child's whole subtree (ADR-008). So when the consumer
//! drops its session: the forwarder's send fails, the forwarder exits, the channel
//! closes, the reader's next `blocking_send` fails, the thread returns, the pump drops,
//! the shell dies. The one thing that needs saying out loud is the wake-up — a quiet
//! shell leaves the reader parked inside `PtyPump::read`, so both async halves call
//! [`PtyHandle::kill`] on their way out to make that read return.

use std::sync::Arc;

use lmg_core::platform::pty::{self, PtyCommand, PtyGeometry, PtyHandle, ShellCandidate, PtyPump};
use lmg_host::services::shell::{
    PtyEvent, PtyIn, PtyInput, PtyOut, PtySession, PtySize, SessionLedger, ShellError,
};
use tokio::sync::mpsc;

/// The target id a local session carries, in the session list and on the wire.
pub const LOCAL_TARGET: &str = "local";

/// What the panel shows next to the local switch, and what a session is labelled.
pub const LOCAL_LABEL: &str = "Local shell";

/// How much of the pipe is moved per read. One screenful of a fast `cat` is a few KB;
/// 32 KB keeps the syscall count low without making a single chunk something the
/// consumer queue notices.
const READ_CHUNK: usize = 32 * 1024;

/// Where a local session comes from. Implemented once for real PTYs and again by the
/// tests for a shell that does what the test says.
pub trait LocalShell: Send + Sync {
    /// The program a session would run with no override — the panel shows it beside the
    /// local switch, so it must be the truth about this host, not a guess.
    fn program(&self) -> String;

    /// The shells this host offers a local terminal, probed once at plugin start
    /// (docs/15 §2.1): the settings sheet's candidate list. Never a per-request probe —
    /// detection walks PATH, which is fine once at start and wrong on every GET.
    fn candidates(&self) -> Vec<ShellCandidate>;

    /// The program string the panel should label a configured shell with: resolved to an
    /// absolute path where the platform can (docs/15 §2.1). The default is the identity,
    /// which is exactly right for the tests' fake and for unix.
    fn resolve(&self, program: &str) -> String {
        program.to_string()
    }

    /// Open one local PTY. `shell` overrides the program; the session id is only used for
    /// the lease's forensics ("closed over N sessions").
    fn open(
        &self,
        session_id: &str,
        size: PtySize,
        shell: Option<&str>,
    ) -> Result<PtySession, ShellError>;
}

/// The real one: ConPTY on Windows, `openpty` on unix.
pub struct LocalShells {
    /// Local sessions have no provider to drain, but they do have a count, and the
    /// ledger is what the lease inside each session decrements on drop. Keeping the same
    /// mechanism as the remote side means the driver holds one kind of session.
    ledger: Arc<SessionLedger>,
    /// The default shell and the candidate list, probed ONCE here (construction is the
    /// plugin's start) and reused by every open — a session must not re-walk PATH either.
    default: PtyCommand,
    candidates: Vec<ShellCandidate>,
}

impl Default for LocalShells {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalShells {
    pub fn new() -> Self {
        LocalShells {
            ledger: SessionLedger::new(),
            default: pty::default_shell(),
            candidates: pty::shell_candidates(),
        }
    }

    /// How many local PTYs are open. The session table caps them; this is the count that
    /// says whether it worked.
    pub fn outstanding(&self) -> usize {
        self.ledger.outstanding()
    }
}

impl LocalShell for LocalShells {
    fn program(&self) -> String {
        self.default.program.clone()
    }

    fn candidates(&self) -> Vec<ShellCandidate> {
        self.candidates.clone()
    }

    fn resolve(&self, program: &str) -> String {
        pty::resolve_program(program)
    }

    fn open(
        &self,
        session_id: &str,
        size: PtySize,
        shell: Option<&str>,
    ) -> Result<PtySession, ShellError> {
        let mut command = match shell.map(str::trim).filter(|s| !s.is_empty()) {
            Some(program) => PtyCommand::new(program),
            None => self.default.clone(),
        };
        // Every terminal program reads TERM before it decides what it may draw. Without
        // it a shell assumes "dumb" and the panel gets a terminal with no colour and no
        // cursor addressing — which looks like a broken terminal, not a missing variable.
        command = command.env("TERM", super::recording::RECORDING_TERM);

        let (handle, pump) = pty::open_pty(&command, PtyGeometry::new(size.cols, size.rows))
            .map_err(|err| {
                ShellError::Failed(format!("could not open a local shell ({}): {err}", {
                    command.program.clone()
                }))
            })?;

        let lease = self.ledger.grant(LOCAL_TARGET, session_id);
        let (session, endpoint) = PtySession::duplex(session_id, LOCAL_TARGET, size, lease);
        let (out, input) = endpoint.split();

        // Depth one: see the module note. This is the backpressure, not a buffer.
        let (raw_tx, raw_rx) = mpsc::channel::<Vec<u8>>(1);
        tokio::task::spawn_blocking(move || read_pty(pump, raw_tx));
        tokio::spawn(forward_output(out, raw_rx, handle.clone()));
        tokio::spawn(forward_input(input, handle));
        Ok(session)
    }
}

/// The blocking half: read the pipe until it ends or nobody wants it any more.
fn read_pty(mut pump: PtyPump, chunks: mpsc::Sender<Vec<u8>>) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        match pump.read(&mut buf) {
            // EOF. Dropping the pump here is what reaps the child's subtree.
            Ok(0) => break,
            Ok(n) => {
                // Parks while the forwarder is behind, which parks the pipe. Err means
                // the consumer is gone, and there is nothing left to read for.
                if chunks.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// The async half of the read path: chunks -> the session's event queue, then one Exit.
async fn forward_output(out: PtyOut, mut chunks: mpsc::Receiver<Vec<u8>>, handle: PtyHandle) {
    loop {
        tokio::select! {
            chunk = chunks.recv() => match chunk {
                Some(bytes) => {
                    // Parking here IS the backpressure (docs/14 §6.8); an Err means the
                    // consumer dropped the session while we were parked.
                    if out.send(PtyEvent::Data(bytes)).await.is_err() {
                        break;
                    }
                }
                None => {
                    // The reader thread is done: the shell exited or the pipe broke.
                    let _ = out.send(PtyEvent::Exit { code: handle.exit_code() }).await;
                    return;
                }
            },
            // A consumer that goes away while the shell is quiet: without this the reader
            // thread would sit in its poll loop until the shell happened to print.
            _ = out.closed() => break,
        }
    }
    handle.kill();
}

/// Keystrokes and window changes, in the order the consumer sent them.
async fn forward_input(mut input: PtyIn, handle: PtyHandle) {
    while let Some(message) = input.next().await {
        match message {
            // A synchronous write, deliberately: these are keystrokes and the input pipe
            // holds 64 KB, so it cannot plausibly block the runtime thread. A paste large
            // enough to fill it is already bounded by the session's input queue.
            PtyInput::Data(bytes) => {
                if handle.write(&bytes).is_err() {
                    break;
                }
            }
            PtyInput::Resize(size) => {
                let _ = handle.resize(PtyGeometry::new(size.cols, size.rows));
            }
        }
    }
    handle.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Read events until the shell exits or the deadline passes; returns what it saw and
    /// the exit code if one arrived.
    async fn drain(session: &mut PtySession, deadline: Duration) -> (String, Option<Option<i32>>) {
        let mut text = Vec::new();
        let mut exit = None;
        let _ = tokio::time::timeout(deadline, async {
            while let Some(event) = session.next_event().await {
                match event {
                    PtyEvent::Data(bytes) => text.extend_from_slice(&bytes),
                    PtyEvent::Exit { code } => {
                        exit = Some(code);
                        break;
                    }
                    PtyEvent::Error(_) => break,
                }
            }
        })
        .await;
        (String::from_utf8_lossy(&text).into_owned(), exit)
    }

    #[tokio::test]
    async fn a_local_shell_produces_bytes_and_ends_with_an_exit() {
        // The whole local path in one test: PTY, blocking reader, forwarder, session.
        let shells = LocalShells::new();
        let mut session = shells
            .open("sess-1", PtySize::new(80, 24).unwrap(), None)
            .expect("the default shell opens");
        assert_eq!(shells.outstanding(), 1, "the lease is counted");

        // An interactive shell prints a prompt and waits, so ask it to leave.
        session.write(b"exit\r\n".to_vec()).await.expect("writes");
        let (text, exit) = drain(&mut session, Duration::from_secs(20)).await;
        assert!(!text.is_empty(), "the shell printed nothing at all");
        assert!(exit.is_some(), "no Exit event: {text:?}");

        drop(session);
        // The lease is released by the drop, which is what lets a provider close clean.
        tokio::task::yield_now().await;
        assert_eq!(shells.outstanding(), 0);
    }

    #[tokio::test]
    async fn dropping_the_session_kills_the_shell_it_opened() {
        // ADR-008 at the session layer: nothing has to remember to clean up a tab.
        let shells = LocalShells::new();
        let session = shells
            .open("sess-1", PtySize::new(80, 24).unwrap(), None)
            .expect("opens");
        drop(session);
        // The forwarder notices the closed queue, kills, and the reader thread returns
        // with the pump — which is the subtree teardown.
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if shells.outstanding() == 0 {
                return;
            }
        }
        panic!("the lease was never released");
    }

    #[tokio::test]
    async fn an_unopenable_program_is_a_named_error_not_a_panic() {
        let shells = LocalShells::new();
        let Err(err) = shells.open(
            "sess-1",
            PtySize::new(80, 24).unwrap(),
            Some("no-such-program-9d2f1a.exe"),
        ) else {
            panic!("a program that does not exist must not open a shell");
        };
        let text = err.to_string();
        assert!(text.contains("no-such-program-9d2f1a"), "{text}");
    }

    #[test]
    fn the_reported_program_is_the_one_that_would_run() {
        // The panel prints this beside the local switch, so it has to be the truth about
        // this host rather than a hardcoded "bash".
        let shells = LocalShells::new();
        assert_eq!(shells.program(), pty::default_shell().program);
        assert!(!shells.program().is_empty());
    }

    #[test]
    fn the_candidate_list_is_probed_once_and_is_never_empty() {
        // docs/15 §2.1: the settings sheet's dropdown comes from this list, and every
        // host offers at least one shell (cmd via COMSPEC on Windows, $SHELL on unix) —
        // an empty list means the probe broke, not that the host has no shells.
        let shells = LocalShells::new();
        let candidates = shells.candidates();
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|c| !c.program.is_empty() && !c.label.is_empty()),
            "{candidates:?}"
        );
        // The cached answer is stable across calls — the probe ran once, at construction.
        assert_eq!(shells.candidates(), candidates);
    }
}
