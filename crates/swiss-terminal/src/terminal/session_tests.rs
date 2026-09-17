/*
 * Copyright 2026 The swiss authors
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

//! The session machine's acceptance surface (docs/14 T4).
//!
//! Every rule in docs/14 §6 that can be asserted without a socket is asserted here,
//! against a fake shell provider and a fake local PTY — the test plays the part of the
//! shell, so a 30-minute idle timeout, a 60-second grace window and a jammed client are
//! all exact rather than approximate.
//!
//! Two clocks are in use on purpose. Tests about *time* run on the paused clock and step
//! it by hand. Tests about *behaviour* run on the real clock with their timers switched
//! off (`idle_timeout: 0`, `stall: 0`, an hour of grace), because the paused clock
//! auto-advances whenever every task is waiting — which would fire a timeout the test was
//! not asking about.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use swiss_host::services::shell::{
    PtyEvent, PtyIn, PtyInput, PtyOut, PtySession, PtySize, SessionLedger, ShellError,
    ShellProvider, ShellRegistry, ShellTarget,
};

use super::*;
use crate::terminal::config::LocalConfig;
use crate::terminal::local::LocalShell;
use swiss_core::platform::pty::ShellCandidate;

// --- the fakes -----------------------------------------------------------------------------

/// The far side of a fake shell: what the test writes output into and reads keystrokes
/// out of. Handing this to the test rather than scripting it in the fake is what makes
/// backpressure observable — a parked `send` here IS the pressure reaching the shell.
struct FarSide {
    out: PtyOut,
    input: PtyIn,
}

impl FarSide {
    async fn say(&self, text: &str) {
        self.out
            .send(PtyEvent::Data(text.as_bytes().to_vec()))
            .await
            .expect("the consumer is listening");
    }

    async fn exit(&self, code: i32) {
        let _ = self.out.send(PtyEvent::Exit { code: Some(code) }).await;
    }

    async fn heard(&mut self) -> Option<PtyInput> {
        tokio::time::timeout(Duration::from_secs(5), self.input.next())
            .await
            .ok()
            .flatten()
    }
}

struct FakeShells {
    ledger: Arc<SessionLedger>,
    targets: Vec<ShellTarget>,
    far: mpsc::UnboundedSender<FarSide>,
}

#[async_trait]
impl ShellProvider for FakeShells {
    fn list(&self) -> Vec<ShellTarget> {
        self.targets.clone()
    }

    async fn open(&self, id: &str, holder: &str, size: PtySize) -> Result<PtySession, ShellError> {
        if !self.targets.iter().any(|t| t.id == id) {
            return Err(ShellError::Unknown(format!("no such target: {id}")));
        }
        let lease = self.ledger.grant(id, holder);
        let (session, endpoint) = PtySession::duplex(format!("remote-{id}"), id, size, lease);
        let (out, input) = endpoint.split();
        let _ = self.far.send(FarSide { out, input });
        Ok(session)
    }
}

struct FakeLocal {
    ledger: Arc<SessionLedger>,
    far: mpsc::UnboundedSender<FarSide>,
    /// What each open was told to run (None = the default) — the tests' only window into
    /// the shell a session would actually spawn.
    opened: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

impl LocalShell for FakeLocal {
    fn program(&self) -> String {
        "fake-shell".to_string()
    }

    fn candidates(&self) -> Vec<ShellCandidate> {
        vec![
            ShellCandidate {
                program: "C:\\shells\\pwsh.exe".to_string(),
                label: "PowerShell 7".to_string(),
            },
            ShellCandidate {
                program: "C:\\Windows\\system32\\cmd.exe".to_string(),
                label: "cmd".to_string(),
            },
        ]
    }

    fn open(
        &self,
        session_id: &str,
        size: PtySize,
        shell: Option<&str>,
    ) -> Result<PtySession, ShellError> {
        self.opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(shell.map(str::to_string));
        let lease = self.ledger.grant(LOCAL_TARGET, session_id);
        let (session, endpoint) = PtySession::duplex(session_id, LOCAL_TARGET, size, lease);
        let (out, input) = endpoint.split();
        let _ = self.far.send(FarSide { out, input });
        Ok(session)
    }
}

// --- the harness ---------------------------------------------------------------------------

struct Harness {
    sessions: Arc<TerminalSessions>,
    shells: Arc<ShellRegistry>,
    far: mpsc::UnboundedReceiver<FarSide>,
    far_tx: mpsc::UnboundedSender<FarSide>,
    dir: PathBuf,
    /// The fake local shell's record of every open (see FakeLocal.opened).
    local_opens: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

/// Timers off. The default for anything not about time — see the module note.
fn quiet(local: bool) -> TerminalConfig {
    TerminalConfig {
        local: LocalConfig {
            enabled: local,
            shell: None,
        },
        idle_timeout: Duration::ZERO,
        stall: Duration::ZERO,
        grace: Duration::from_secs(3600),
        ..TerminalConfig::default()
    }
}

fn size() -> PtySize {
    PtySize::new(80, 24).expect("a legal geometry")
}

fn target(id: &str) -> ShellTarget {
    ShellTarget {
        id: id.to_string(),
        label: format!("the {id} box"),
        host: format!("{id}.example"),
        port: 22,
        username: "dev".to_string(),
        state: "connected".to_string(),
    }
}

impl Harness {
    /// A session machine with a registered provider offering two hosts.
    fn new(config: TerminalConfig) -> Harness {
        let harness = Harness::without_provider(config);
        harness.register("tunnels");
        harness
    }

    /// The same, with the shell seat empty — the "tunnels is off" world.
    fn without_provider(config: TerminalConfig) -> Harness {
        let dir = swiss_core::paths::test_home()
            .join("terminal-sessions")
            .join(swiss_core::util::random_hex(8));
        let (far_tx, far_rx) = mpsc::unbounded_channel();
        let shells = Arc::new(ShellRegistry::new());
        let local_opens = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions = TerminalSessions::new(
            config,
            Arc::clone(&shells),
            Arc::new(FakeLocal {
                ledger: SessionLedger::new(),
                far: far_tx.clone(),
                opened: Arc::clone(&local_opens),
            }),
            dir.clone(),
        );
        Harness {
            sessions,
            shells,
            far: far_rx,
            far_tx,
            dir,
            local_opens,
        }
    }

    /// Put a provider in the shell seat, offering two hosts.
    fn register(&self, who: &str) {
        self.shells
            .register(
                Arc::new(FakeShells {
                    ledger: SessionLedger::new(),
                    targets: vec![target("box-one"), target("box-two"), target("box-three")],
                    far: self.far_tx.clone(),
                }),
                who,
            )
            .expect("the seat was free");
    }

    /// The far side of the session that was just opened.
    fn far(&mut self) -> FarSide {
        self.far.try_recv().expect("the fake shell was opened")
    }

    async fn open(&mut self, target: &str) -> Result<Opened, TerminalError> {
        self.sessions.open(target, size(), None).await
    }

    /// Open and attach in one step — the normal path a panel takes.
    async fn started(&mut self, target: &str) -> (Opened, FarSide, Attachment) {
        let opened = self.open(target).await.expect("opens");
        let far = self.far();
        let attachment = self
            .sessions
            .attach(&opened.id, &opened.ticket)
            .await
            .expect("attaches");
        (opened, far, attachment)
    }
}

/// The next frame, or a panic naming what was being waited for. Real clock only.
async fn frame(attachment: &mut Attachment, what: &str) -> ClientFrame {
    tokio::time::timeout(Duration::from_secs(5), attachment.frames.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("the session closed while waiting for {what}"))
}

/// Everything the client can read right now, concatenated.
fn drain_now(attachment: &mut Attachment) -> Vec<u8> {
    let mut seen = Vec::new();
    while let Ok(ClientFrame::Data(bytes)) = attachment.frames.try_recv() {
        seen.extend_from_slice(&bytes);
    }
    seen
}

// --- opening, refusing, listing ---------------------------------------------------------------

#[tokio::test]
async fn a_session_streams_its_shells_output_to_the_attached_client() {
    let mut h = Harness::new(quiet(true));
    let (opened, far, mut attachment) = h.started(LOCAL_TARGET).await;
    far.say("hello from the shell").await;
    assert_eq!(
        frame(&mut attachment, "the shell's greeting").await,
        ClientFrame::Data(b"hello from the shell".to_vec())
    );

    let listed = h.sessions.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, opened.id);
    assert_eq!(listed[0].target, LOCAL_TARGET);
    assert_eq!(listed[0].label, "Local shell");
    assert!(listed[0].attached);
    assert_eq!(listed[0].bytes_out, 20);
    assert!(listed[0].recording.is_some());
}

#[tokio::test]
async fn keystrokes_and_resizes_reach_the_shell_in_the_order_they_were_sent() {
    let mut h = Harness::new(quiet(true));
    let (opened, mut far, _attachment) = h.started(LOCAL_TARGET).await;

    h.sessions
        .input(&opened.id, b"ls\r".to_vec())
        .await
        .expect("input");
    // The redundant resize path (docs/14 §8): POST /resize and the socket's resize frame
    // are the same call underneath, which is the only reason having both is safe.
    h.sessions
        .resize(&opened.id, PtySize::new(132, 43).unwrap())
        .await
        .expect("resize");

    assert_eq!(far.heard().await, Some(PtyInput::Data(b"ls\r".to_vec())));
    assert_eq!(
        far.heard().await,
        Some(PtyInput::Resize(PtySize::new(132, 43).unwrap()))
    );
}

#[tokio::test]
async fn the_local_shell_is_refused_while_it_is_switched_off() {
    // docs/14 §6.1: off by default, and the refusal has to say which switch to flip.
    let mut h = Harness::new(quiet(false));
    let err = h.open(LOCAL_TARGET).await.expect_err("refused");
    assert!(err.to_string().contains("local.enabled"), "{err}");
    assert!(h.sessions.list().is_empty());
}

#[tokio::test]
async fn the_total_cap_refuses_the_next_session_and_names_itself() {
    // Remote targets: both caps apply (an SSH channel costs the far host too).
    let mut h = Harness::new(TerminalConfig {
        max_sessions: 2,
        max_sessions_per_target: 2,
        ..quiet(true)
    });
    h.open("box-one").await.expect("one");
    h.open("box-two").await.expect("two");
    let err = h.open("box-three").await.expect_err("three is too many");
    assert!(err.to_string().contains("maxSessions (2)"), "{err}");
    assert_eq!(h.sessions.list().len(), 2);
}

#[tokio::test]
async fn local_sessions_are_uncapped_and_consume_no_remote_budget() {
    // The operator's call (2026-09-12): a local PTY costs a thread stack on a machine the
    // operator owns, and they asked to open as many local tabs as they like. A wall of
    // local sessions must neither be refused itself nor eat the budget a remote open needs.
    let mut h = Harness::new(TerminalConfig {
        max_sessions: 2,
        max_sessions_per_target: 1,
        ..quiet(true)
    });
    for i in 0..5 {
        h.open(LOCAL_TARGET)
            .await
            .unwrap_or_else(|e| panic!("local tab {i} should be allowed: {e}"));
    }
    h.open("box-one")
        .await
        .expect("a remote seat is still free");
    assert_eq!(h.sessions.list().len(), 6);
}

#[tokio::test]
async fn the_per_target_cap_binds_before_the_total_one() {
    let mut h = Harness::new(TerminalConfig {
        max_sessions: 4,
        max_sessions_per_target: 1,
        ..quiet(true)
    });
    h.open("box-one").await.expect("one on box-one");
    let err = h.open("box-one").await.expect_err("two on box-one");
    assert!(
        err.to_string()
            .contains("maxSessionsPerTarget (1) for box-one"),
        "{err}"
    );
    // ...and the other host is unaffected, which is the whole point of a per-target cap.
    h.open("box-two").await.expect("one on box-two");
}

#[tokio::test]
async fn allowed_targets_narrows_what_may_be_opened() {
    let mut h = Harness::new(TerminalConfig {
        allowed_targets: vec!["box-one".to_string()],
        ..quiet(true)
    });
    h.open("box-one").await.expect("allowed");
    let err = h.open("box-two").await.expect_err("not allowed");
    assert!(err.to_string().contains("allowedTargets"), "{err}");
    // The list the panel renders is narrowed the same way, so a user is never offered a
    // host the open would refuse.
    let ids: Vec<String> = h
        .sessions
        .targets()
        .remote
        .targets
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, vec!["box-one".to_string()]);
}

#[tokio::test]
async fn an_absent_provider_is_reported_by_name_not_as_an_empty_host_list() {
    // docs/14 §4, acceptance item 4: "you have no servers" and "the tunnels plugin is
    // off" are different statements and the panel has to be able to tell them apart.
    let h = Harness::without_provider(quiet(true));
    let view = h.sessions.targets();
    assert_eq!(view.remote.presence, "absent");
    assert!(view.remote.targets.is_empty());
    assert!(view
        .remote
        .reason
        .as_deref()
        .unwrap()
        .contains("no plugin currently provides"));

    let provider = Arc::new(FakeShells {
        ledger: SessionLedger::new(),
        targets: vec![target("box-one")],
        far: mpsc::unbounded_channel().0,
    });
    h.shells.register(provider, "tunnels").expect("registers");
    let view = h.sessions.targets();
    assert_eq!(view.remote.presence, "serving");
    assert_eq!(view.remote.targets.len(), 1);
    assert!(view.remote.reason.is_none());

    h.shells.begin_withdraw();
    let view = h.sessions.targets();
    assert_eq!(view.remote.presence, "stopping");
    assert!(view.remote.reason.as_deref().unwrap().contains("tunnels"));

    h.shells.clear();
    let view = h.sessions.targets();
    assert_eq!(view.remote.presence, "absent");
    let reason = view.remote.reason.unwrap();
    assert!(reason.contains("tunnels"), "{reason}");
    assert!(reason.contains("not running"), "{reason}");
}

#[tokio::test]
async fn opening_a_remote_target_while_its_provider_is_gone_names_the_plugin() {
    let mut h = Harness::new(quiet(true));
    h.shells.begin_withdraw();
    h.shells.clear();
    let err = h.open("box-one").await.expect_err("refused");
    assert!(err.to_string().contains("tunnels"), "{err}");
    // The local shell is untouched by the remote provider going away.
    h.open(LOCAL_TARGET).await.expect("local still opens");
}

#[tokio::test]
async fn the_local_view_reports_the_program_that_would_actually_run() {
    let h = Harness::new(quiet(false));
    let view = h.sessions.targets();
    assert!(!view.local.enabled);
    assert_eq!(view.local.shell, "fake-shell");
}

#[tokio::test]
async fn the_local_view_lists_the_candidates_the_sheet_offers() {
    // docs/15 §2.1: local.shells is the settings sheet's dropdown — the machine hands the
    // fake's fixed list through untouched, and the view adds nothing of its own.
    let h = Harness::new(quiet(false));
    let view = h.sessions.targets();
    assert_eq!(view.local.shells.len(), 2);
    assert_eq!(view.local.shells[0].label, "PowerShell 7");
    assert_eq!(view.local.shells[1].label, "cmd");
    assert!(view.local.shells[0].program.ends_with("pwsh.exe"));
}

#[tokio::test]
async fn the_configured_local_shell_is_what_a_session_runs_unless_overridden() {
    // docs/15 §2: the settings sheet writes local.shell; before this the config row only
    // ever changed the LABEL — a saved "Git Bash" still opened the default. The request's
    // one-shot override (the API's shell field) must keep winning.
    let mut config = quiet(true);
    config.local.shell = Some("C:\\Program Files\\Git\\bin\\bash.exe".to_string());
    let mut h = Harness::new(config);

    h.open(LOCAL_TARGET)
        .await
        .expect("opens with the config shell");
    // The recorder's mutex is scoped on purpose: FakeLocal::open locks it from INSIDE the
    // awaited open() call, so holding it across the second open would deadlock the
    // current_thread runtime — lock, assert, release, then open again.
    {
        let opens = h.local_opens.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            opens.last(),
            Some(&Some("C:\\Program Files\\Git\\bin\\bash.exe".to_string()))
        );
    }

    h.sessions
        .open(LOCAL_TARGET, size(), Some("pwsh.exe"))
        .await
        .expect("opens with the override");
    {
        let opens = h.local_opens.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            opens.last(),
            Some(&Some("pwsh.exe".to_string())),
            "the request override wins"
        );
    }

    // And the view reports the configured shell through the fake's resolver (identity).
    let view = h.sessions.targets();
    assert_eq!(
        view.local.shell, "C:\\Program Files\\Git\\bin\\bash.exe",
        "the label says what would run"
    );
}

// --- tickets -------------------------------------------------------------------------------

#[tokio::test]
async fn an_attach_spends_its_ticket_and_a_reconnect_needs_a_fresh_one() {
    let mut h = Harness::new(quiet(true));
    let opened = h.open(LOCAL_TARGET).await.expect("opens");
    let _far = h.far();
    let attachment = h
        .sessions
        .attach(&opened.id, &opened.ticket)
        .await
        .expect("attaches");
    drop(attachment);

    let err = h
        .sessions
        .attach(&opened.id, &opened.ticket)
        .await
        .expect_err("the ticket was single use");
    assert!(err.to_string().contains("already been used"), "{err}");

    let again = h.sessions.ticket(&opened.id).expect("a fresh ticket");
    h.sessions
        .attach(&opened.id, &again)
        .await
        .expect("reconnects");
}

#[tokio::test]
async fn a_ticket_for_a_session_that_has_closed_is_worth_nothing() {
    let mut h = Harness::new(quiet(true));
    let opened = h.open(LOCAL_TARGET).await.expect("opens");
    let _far = h.far();
    h.sessions.close(&opened.id).await.expect("closes");
    for _ in 0..50 {
        if h.sessions.list().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(h.sessions.list().is_empty(), "the session left the table");
    assert!(h.sessions.ticket(&opened.id).is_err());
    let err = h
        .sessions
        .attach(&opened.id, &opened.ticket)
        .await
        .expect_err("nothing to attach to");
    assert!(err.to_string().contains("already been used"), "{err}");
}

// --- backpressure ----------------------------------------------------------------------------

/// Fill the whole chain — session queue, driver, client queue — until the shell's own
/// send parks. Returns the chunks that made it in, in order.
async fn jam(far: &FarSide) -> Vec<String> {
    let mut sent = Vec::new();
    loop {
        let chunk = format!("{:04}|", sent.len());
        match tokio::time::timeout(
            Duration::from_millis(50),
            far.out.send(PtyEvent::Data(chunk.as_bytes().to_vec())),
        )
        .await
        {
            Ok(Ok(())) => sent.push(chunk),
            Ok(Err(err)) => panic!("the session closed under us: {err}"),
            // Parked: the pressure has reached the shell, which is the whole design.
            Err(_) => return sent,
        }
        assert!(
            sent.len() < 500,
            "500 chunks in and still no backpressure: something is buffering without a bound"
        );
    }
}

#[tokio::test]
async fn a_client_that_stops_reading_parks_the_shell_and_loses_no_bytes() {
    // docs/14 §6.8 and acceptance item 7. Dropping a fragment of an escape sequence
    // corrupts a terminal permanently, so "slow" must mean parked, never lossy.
    let mut h = Harness::new(quiet(true));
    let (_opened, far, mut attachment) = h.started(LOCAL_TARGET).await;

    let sent = jam(&far).await;
    assert!(
        sent.len() > 8,
        "the queues absorbed almost nothing: {}",
        sent.len()
    );

    // Now read. Everything that was accepted must come back, in order, complete.
    let mut seen = Vec::new();
    let wanted: String = sent.concat();
    while seen.len() < wanted.len() {
        match frame(&mut attachment, "the drained output").await {
            ClientFrame::Data(bytes) => seen.extend_from_slice(&bytes),
            other => panic!("unexpected frame while draining: {other:?}"),
        }
    }
    assert_eq!(String::from_utf8_lossy(&seen), wanted);
}

#[tokio::test]
async fn a_keystroke_gets_through_while_the_output_is_jammed() {
    // The reason the driver serves commands while it is parked: a flooding terminal that
    // cannot be interrupted is a terminal you have to kill from another window.
    let mut h = Harness::new(quiet(true));
    let (opened, mut far, _attachment) = h.started(LOCAL_TARGET).await;
    let sent = jam(&far).await;
    assert!(!sent.is_empty());

    h.sessions
        .input(&opened.id, vec![0x03])
        .await
        .expect("Ctrl-C is accepted");
    assert_eq!(far.heard().await, Some(PtyInput::Data(vec![0x03])));
}

// --- the reconnect grace window -----------------------------------------------------------

#[tokio::test]
async fn a_dropped_socket_does_not_kill_the_session_and_the_output_catches_up() {
    // docs/14 §6.7: a closed laptop lid must not kill a running compile.
    let mut h = Harness::new(quiet(true));
    let (opened, far, attachment) = h.started(LOCAL_TARGET).await;
    drop(attachment);
    // Let the driver notice the socket is gone before the shell says anything.
    for _ in 0..20 {
        tokio::task::yield_now().await;
        if !h.sessions.list()[0].attached {
            break;
        }
    }
    assert!(!h.sessions.list()[0].attached, "the driver saw the detach");

    far.say("compiling...").await;
    far.say("done").await;

    let ticket = h.sessions.ticket(&opened.id).expect("a fresh ticket");
    let mut back = h
        .sessions
        .attach(&opened.id, &ticket)
        .await
        .expect("reconnects");
    // The buffer may hand back one frame or two: the driver drains it as fast as the
    // shell fills it, and a test that demanded exactly one frame would be asserting on
    // scheduling rather than on the catch-up.
    let mut caught_up = Vec::new();
    while caught_up.len() < "compiling...done".len() {
        match frame(&mut back, "the catch-up buffer").await {
            ClientFrame::Data(bytes) => caught_up.extend_from_slice(&bytes),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(String::from_utf8_lossy(&caught_up), "compiling...done");
}

#[tokio::test]
async fn the_catch_up_buffer_is_bounded_and_admits_what_it_dropped() {
    // The one place in the design that is allowed to lose bytes, and it says so out loud
    // rather than showing the user a silent hole.
    let mut h = Harness::new(quiet(true));
    let (opened, far, attachment) = h.started(LOCAL_TARGET).await;
    drop(attachment);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    let chunk = "x".repeat(8 * 1024);
    for _ in 0..20 {
        far.say(&chunk).await;
    }
    far.say("THE-TAIL").await;
    // A `say` only queues an event. If the attach command were served first — and it
    // would be, the driver's select is biased towards commands — the output would go
    // straight to the client and never enter the buffer this test is about. `bytes_out`
    // counts what the driver has actually read, so it is the honest way to wait.
    let total = (20 * chunk.len() + "THE-TAIL".len()) as u64;
    for _ in 0..2000 {
        if h.sessions.list()[0].bytes_out >= total {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        h.sessions.list()[0].bytes_out,
        total,
        "the driver read every chunk while detached"
    );

    let ticket = h.sessions.ticket(&opened.id).expect("a ticket");
    let mut back = h.sessions.attach(&opened.id, &ticket).await.expect("back");

    let notice = match frame(&mut back, "the dropped-bytes notice").await {
        ClientFrame::Data(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        other => panic!("{other:?}"),
    };
    assert!(notice.contains("bytes of output were dropped"), "{notice}");

    let mut tail = Vec::new();
    while !tail.ends_with(b"THE-TAIL") {
        match frame(&mut back, "the surviving tail").await {
            ClientFrame::Data(bytes) => tail.extend_from_slice(&bytes),
            other => panic!("{other:?}"),
        }
    }
    tail.extend_from_slice(&drain_now(&mut back));
    // 160 KB went in while nobody was listening; at most one buffer's worth comes out.
    assert!(
        tail.len() <= CATCHUP_BYTES,
        "the buffer kept {} bytes, cap is {CATCHUP_BYTES}",
        tail.len()
    );
    assert!(
        tail.starts_with(b"x"),
        "the newest output is what survives, not the oldest"
    );
}

// --- the clocks ------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_idle_session_closes_and_says_which_setting_closed_it() {
    let mut h = Harness::new(TerminalConfig {
        idle_timeout: Duration::from_secs(120),
        stall: Duration::ZERO,
        ..quiet(true)
    });
    let (opened, far, mut attachment) = h.started(LOCAL_TARGET).await;
    far.say("prompt$ ").await;
    assert!(matches!(
        attachment.frames.recv().await,
        Some(ClientFrame::Data(_))
    ));

    tokio::time::advance(Duration::from_secs(119)).await;
    assert_eq!(h.sessions.list().len(), 1, "still inside the window");

    tokio::time::advance(Duration::from_secs(2)).await;
    let said = match attachment.frames.recv().await {
        Some(ClientFrame::Data(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        other => panic!("no visible reason was written: {other:?}"),
    };
    assert!(said.contains("2 minutes with no activity"), "{said}");
    assert!(said.contains("idleTimeoutMinutes"), "{said}");
    assert!(matches!(
        attachment.frames.recv().await,
        Some(ClientFrame::Error(_))
    ));
    assert!(h.sessions.list().is_empty());
    assert!(h.sessions.ticket(&opened.id).is_err());
}

#[tokio::test(start_paused = true)]
async fn output_keeps_a_session_alive_because_a_running_build_is_not_idle() {
    let mut h = Harness::new(TerminalConfig {
        idle_timeout: Duration::from_secs(60),
        stall: Duration::ZERO,
        ..quiet(true)
    });
    let (_opened, far, mut attachment) = h.started(LOCAL_TARGET).await;
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(45)).await;
        far.say(".").await;
        assert!(matches!(
            attachment.frames.recv().await,
            Some(ClientFrame::Data(_))
        ));
    }
    assert_eq!(h.sessions.list().len(), 1, "three minutes of output later");
}

#[tokio::test(start_paused = true)]
async fn a_session_nobody_ever_attached_to_is_collected_when_the_grace_expires() {
    // A POST that was never followed by a socket. Without this the caps would fill up
    // with sessions no one can see.
    let mut h = Harness::new(TerminalConfig {
        grace: Duration::from_secs(60),
        idle_timeout: Duration::ZERO,
        stall: Duration::ZERO,
        ..quiet(true)
    });
    let _opened = h.open(LOCAL_TARGET).await.expect("opens");
    let _far = h.far();
    tokio::time::advance(Duration::from_secs(59)).await;
    assert_eq!(h.sessions.list().len(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..50 {
        if h.sessions.list().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(h.sessions.list().is_empty(), "the grace window expired");
}

#[tokio::test(start_paused = true)]
async fn a_client_that_never_drains_is_warned_and_then_closed() {
    // docs/14 §6.8: a send queue that stays full is a client that is gone in every way
    // except the socket being open.
    let mut h = Harness::new(TerminalConfig {
        stall: Duration::from_secs(30),
        idle_timeout: Duration::ZERO,
        ..quiet(true)
    });
    let (_opened, far, mut attachment) = h.started(LOCAL_TARGET).await;
    let sent = jam(&far).await;
    assert!(!sent.is_empty());

    assert!(
        !*attachment.stalled.borrow(),
        "nothing is stalled until the notice fires"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    attachment
        .stalled
        .changed()
        .await
        .expect("the stall signal arrived");
    assert!(*attachment.stalled.borrow());
    assert_eq!(h.sessions.list().len(), 1, "warned, not yet closed");

    tokio::time::advance(Duration::from_secs(31)).await;
    // The client that would not take output cannot take the closing notice either, so
    // the driver waits out CLOSE_FLUSH before it lets go. On the paused clock that last
    // quarter second has to be stepped by hand, twice: once per frame it tries to hand
    // over.
    for _ in 0..10 {
        if h.sessions.list().is_empty() {
            break;
        }
        tokio::time::advance(CLOSE_FLUSH).await;
    }
    assert!(h.sessions.list().is_empty(), "the stalled session closed");
}

// --- closing ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_shell_that_exits_ends_the_session_with_its_code() {
    let mut h = Harness::new(quiet(true));
    let (_opened, far, mut attachment) = h.started(LOCAL_TARGET).await;
    far.say("bye").await;
    far.exit(3).await;

    let mut frames = VecDeque::new();
    while let Some(f) = attachment.frames.recv().await {
        frames.push_back(f);
    }
    // The shell said goodbye in its own words, so the gateway adds none of its own.
    assert_eq!(frames.pop_front(), Some(ClientFrame::Data(b"bye".to_vec())));
    assert_eq!(
        frames.pop_front(),
        Some(ClientFrame::Exit { code: Some(3) })
    );
    assert!(frames.is_empty(), "{frames:?}");
    assert!(h.sessions.list().is_empty());
}

#[tokio::test]
async fn shutdown_closes_every_session_saying_why() {
    let mut h = Harness::new(quiet(true));
    // Two sessions, one watched: a stopping plugin must tell the client it is
    // stopping, not that somebody requested a close.
    h.open(LOCAL_TARGET).await.expect("one");
    let _one = h.far();
    let two = h.open("box-one").await.expect("two");
    let _two = h.far();
    let mut attachment = h
        .sessions
        .attach(&two.id, &two.ticket)
        .await
        .expect("attaches");
    assert_eq!(h.sessions.shutdown().await, 2);
    let mut line = String::new();
    for _ in 0..2 {
        match frame(&mut attachment, "the shutdown frames").await {
            ClientFrame::Data(bytes) => line.push_str(&String::from_utf8_lossy(&bytes)),
            ClientFrame::Error(message) => assert!(
                message.contains("the terminal plugin is stopping"),
                "{message}"
            ),
            ClientFrame::Exit { .. } => panic!("a stop is not an exit"),
        }
    }
    assert!(line.contains("the terminal plugin is stopping"), "{line:?}");
    for _ in 0..50 {
        if h.sessions.list().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(h.sessions.list().is_empty());
}

#[tokio::test]
async fn the_recording_holds_the_output_and_ends_with_the_reason() {
    let mut h = Harness::new(quiet(true));
    let (opened, far, _attachment) = h.started(LOCAL_TARGET).await;
    far.say("recorded output").await;
    far.exit(0).await;
    for _ in 0..50 {
        if h.sessions.list().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let path = h.dir.join(format!("{}.cast", opened.id));
    assert_eq!(
        opened.recording.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
    let text = std::fs::read_to_string(&path).expect("the cast file exists");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("one JSON value per line"))
        .collect();
    assert_eq!(lines[0]["version"], 2);
    assert_eq!(lines[1][2], "recorded output");
    let last = lines.last().unwrap()[2].as_str().unwrap();
    assert!(
        last.contains("session ended: the shell exited with code 0"),
        "{last}"
    );
}

#[tokio::test]
async fn recording_off_means_no_file_and_a_null_path() {
    let mut h = Harness::new(TerminalConfig {
        recording: false,
        ..quiet(true)
    });
    let opened = h.open(LOCAL_TARGET).await.expect("opens");
    let _far = h.far();
    assert_eq!(opened.recording, None);
    assert_eq!(h.sessions.list()[0].recording, None);
    assert!(!h.dir.join(format!("{}.cast", opened.id)).exists());
}

#[tokio::test]
async fn a_second_attachment_joins_rather_than_replaces() {
    // Two panel tabs, one session: attaching the second must not blind the first
    // (the single-slot bug), dropping one must not detach the other, and output
    // produced while both hold on reaches BOTH.
    let mut h = Harness::new(quiet(true));
    let (opened, far, mut first) = h.started(LOCAL_TARGET).await;

    let ticket = h.sessions.ticket(&opened.id).expect("mints");
    let mut second = h.sessions.attach(&opened.id, &ticket).await.expect("joins");

    far.say("both-tabs").await;
    for (name, attachment) in [("first", &mut first), ("second", &mut second)] {
        match frame(attachment, name).await {
            ClientFrame::Data(bytes) => assert!(
                String::from_utf8_lossy(&bytes).contains("both-tabs"),
                "{name} got: {bytes:?}"
            ),
            other => panic!("{name} got {other:?} instead of the shared output"),
        }
    }

    // One tab closing its socket detaches ONE: the session stays attached, and the
    // survivor keeps receiving.
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;
    far.say("survivor").await;
    match frame(&mut second, "the surviving tab").await {
        ClientFrame::Data(bytes) => assert!(
            String::from_utf8_lossy(&bytes).contains("survivor"),
            "survivor got: {bytes:?}"
        ),
        other => panic!("the surviving tab got {other:?}"),
    }
    assert!(
        h.sessions.list()[0].attached,
        "one live tab still counts as attached"
    );
}
