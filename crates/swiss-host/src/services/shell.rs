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

//! The interactive shell capability — who can open a PTY on a remote host, and for how
//! long (docs/14 §4).
//!
//! Same shape as [crate::services::catalog]: ONE provider registers on start, consumers
//! take a session-scoped lease, and a provider stopping withdraws (no new leases), drains,
//! then closes. The terminal plugin never links an SSH client; the tunnels plugin never
//! learns that a terminal exists. ADR-010 forbids an edge between two subsystem crates, so
//! a need like this one is read as a MISSING HOST CONTRACT, not as a reason to add the
//! edge — the connection catalog (docs/12 W3) arrived the same way.
//!
//! Two things here deliberately differ from the catalog, and both follow from what a shell
//! session IS:
//!
//! - **The lease is the session, not the request.** A browse lease lives for one /api/db
//!   call and comes back in milliseconds; a terminal lease lives as long as someone has
//!   the tab open. So the drain is async ([SessionLedger::wait_idle] over a watch channel
//!   rather than the catalog's condvar) and its window is short: a live terminal will not
//!   hand itself back because tunnels is stopping, and blocking the current_thread runtime
//!   (ADR-003) for seconds waiting for something that cannot happen would stall the whole
//!   gateway on every stop.
//! - **The payload is bytes.** [PtySession] is a transport-neutral duplex, NOT a russh
//!   channel and not a ConPTY handle. That is the single line keeping the consumer crate
//!   free of an SSH client, and it is why the same type serves the local PTY too.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

/// PTY geometry bounds. A 0-column PTY is undefined behaviour on the far side and a
/// 100k-column one is a memory bug wearing a resize frame, so both ends are refused
/// rather than clamped to a default — docs/14 §8: out of range is a rejection.
pub const MIN_PTY_AXIS: u16 = 1;
pub const MAX_PTY_AXIS: u16 = 1000;

/// How many output chunks may sit between the PTY and whoever is reading it. Bounded on
/// purpose: when it fills, the provider's send parks, the provider stops reading its PTY,
/// and the pressure travels back through the SSH window or the pipe exactly as it would
/// on a real tty. Nothing is ever dropped — losing a fragment of an escape sequence
/// corrupts the display permanently (docs/14 §6.8).
pub const OUTPUT_QUEUE_CHUNKS: usize = 32;

/// The same, for keystrokes and control frames heading in. Small: a human types slower
/// than any of this, and a burst larger than the queue is a paste that can wait.
pub const INPUT_QUEUE_CHUNKS: usize = 32;

/// How long a provider waits for its sessions before closing over them. Deliberately
/// short — see the module note: an attached terminal does not release on request, so this
/// window is for opens still settling, not for a negotiated handover. The warning names
/// how many sessions were closed under.
pub const DRAIN_TIMEOUT_MS: u64 = 3_000;

/// A PTY's geometry, validated at construction so no unchecked pair reaches a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    /// Refuse anything outside [MIN_PTY_AXIS]..=[MAX_PTY_AXIS]. The Err is user-facing:
    /// the HTTP layer hands it back verbatim behind a 400.
    pub fn new(cols: u16, rows: u16) -> Result<Self, String> {
        for (axis, value) in [("cols", cols), ("rows", rows)] {
            if !(MIN_PTY_AXIS..=MAX_PTY_AXIS).contains(&value) {
                return Err(format!(
                    "{axis} must be between {MIN_PTY_AXIS} and {MAX_PTY_AXIS}, got {value}"
                ));
            }
        }
        Ok(PtySize { cols, rows })
    }
}

/// What comes out of a PTY, in order. Output and termination share one channel so the
/// last bytes a shell wrote can never be reported after its exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    /// Raw PTY bytes. Not decoded, not framed — the consumer forwards them untouched.
    Data(Vec<u8>),
    /// The far side ended. `code` is None when the platform or transport reported none.
    Exit { code: Option<i32> },
    /// The session died of something other than the shell exiting (transport dropped,
    /// host went away). User-facing text.
    Error(String),
}

/// What goes into a PTY. Data and resize share one channel so a resize cannot overtake
/// the keystrokes that preceded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyInput {
    Data(Vec<u8>),
    Resize(PtySize),
}

/// Why a shell could not be opened. The strings are user-facing: the /api/terminal routes
/// hand them to the panel verbatim behind the matching status code.
#[derive(Debug)]
pub enum ShellError {
    /// No such target id.
    Unknown(String),
    /// The provider is shutting down and will not open new sessions.
    Withdrawing(String),
    /// The target exists but cannot carry a shell right now (its connection is down).
    Unavailable(String),
    /// The open itself failed — authentication, a refused PTY request, a spawn error.
    Failed(String),
    /// The session existed and is gone; the far side hung up.
    Closed(String),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellError::Unknown(m)
            | ShellError::Withdrawing(m)
            | ShellError::Unavailable(m)
            | ShellError::Failed(m)
            | ShellError::Closed(m) => write!(f, "{m}"),
        }
    }
}

/// One host an interactive shell can be opened on. Identity is the provider's own
/// connection id: two servers that merely share host and port are NOT the same target
/// (docs/09 §3) — the id encodes the whole definition, credentials included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellTarget {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// The provider's live connection state, so the panel can grey a target out honestly.
    pub state: String,
}

/// The session ledger one provider shares with its leases: a count, a withdrawing flag,
/// and a watch channel the drain waits on. Deliberately primitive — policy lives in the
/// [ShellProvider] impls and the stop choreography, not here.
pub struct SessionLedger {
    count: watch::Sender<usize>,
    withdrawing: AtomicBool,
}

impl SessionLedger {
    pub fn new() -> Arc<Self> {
        Arc::new(SessionLedger {
            count: watch::channel(0usize).0,
            withdrawing: AtomicBool::new(false),
        })
    }

    /// Count a session in and mint its lease. The count exists so a draining provider can
    /// say honestly whether it closed clean or over live terminals.
    pub fn grant(self: &Arc<Self>, target: &str, holder: &str) -> ShellLease {
        self.count.send_modify(|n| *n += 1);
        ShellLease {
            ledger: Arc::clone(self),
            target: target.to_string(),
            holder: holder.to_string(),
        }
    }

    pub fn outstanding(&self) -> usize {
        *self.count.borrow()
    }

    /// Stop handing out sessions — the first half of a stop. Idempotent.
    pub fn begin_withdraw(&self) {
        self.withdrawing.store(true, Ordering::SeqCst);
    }

    pub fn is_withdrawing(&self) -> bool {
        self.withdrawing.load(Ordering::SeqCst)
    }

    /// Wait until every session is back, or the timeout expires. Returns how many were
    /// still out when it gave up — 0 means the close was clean. Async, and never holds a
    /// lock across the await: a provider stopping must not stall the runtime.
    pub async fn wait_idle(&self, timeout: Duration) -> usize {
        let mut rx = self.count.subscribe();
        // Bound the borrow to this statement: wait_for hands back a Ref into the channel,
        // and holding it into the tail expression would outlive `rx` itself.
        // Err = the window closed; Ok(Err) = the ledger went away, which can only happen
        // once nobody can hold a lease either. Either way, report what is actually out.
        let settled = matches!(
            tokio::time::timeout(timeout, rx.wait_for(|n| *n == 0)).await,
            Ok(Ok(_))
        );
        if settled {
            0
        } else {
            self.outstanding()
        }
    }
}

/// The provider's stop choreography, in order and out loud: wait (logged when there is
/// something to wait for), then warn with the count when the wait was not clean. Returns
/// the unreleased count so tests can assert on it without scraping logs.
pub async fn drain_sessions(ledger: &SessionLedger, timeout: Duration, who: &str) -> usize {
    let waiting = ledger.outstanding();
    if waiting > 0 {
        swiss_core::log::log(
            "info",
            "waiting for shell sessions before closing",
            Some(serde_json::json!({ "provider": who, "sessions": waiting })),
        );
    }
    let remaining = ledger.wait_idle(timeout).await;
    if remaining > 0 {
        swiss_core::log::warn(
            "closing with live shell sessions",
            Some(serde_json::json!({ "provider": who, "sessions": remaining })),
        );
    }
    remaining
}

/// A lease on one open shell, held for the life of its session. Dropping it is the
/// release — there is no "remember to give it back" API to forget.
pub struct ShellLease {
    ledger: Arc<SessionLedger>,
    target: String,
    holder: String,
}

impl ShellLease {
    /// Which target this leases.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Who holds it — the name that lands in "closed over N sessions" forensics.
    pub fn holder(&self) -> &str {
        &self.holder
    }
}

impl Drop for ShellLease {
    fn drop(&mut self) {
        self.ledger.count.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// A transport-neutral duplex PTY: bytes out, bytes and resizes in, and a lease that
/// keeps the provider's connection alive for exactly as long as this value exists.
/// Deliberately NOT a russh type — see the module note.
///
/// Closing is dropping. The provider's half then sees both halves hang up, which is what
/// tells it to kill the child or close the channel.
pub struct PtySession {
    id: String,
    target: String,
    size: Mutex<PtySize>,
    output: mpsc::Receiver<PtyEvent>,
    input: mpsc::Sender<PtyInput>,
    /// Held, never read: its Drop is the release.
    _lease: ShellLease,
}

/// The provider's half of a [PtySession] — what the SSH channel or the local PTY reader
/// writes into and reads from. Built together with the session so neither side can invent
/// a queue depth of its own.
pub struct PtyEndpoint {
    output: mpsc::Sender<PtyEvent>,
    input: mpsc::Receiver<PtyInput>,
}

impl PtySession {
    /// Build both halves of one session. The provider keeps the [PtyEndpoint], hands the
    /// [PtySession] back through [ShellProvider::open], and holds nothing else.
    pub fn duplex(
        id: impl Into<String>,
        target: impl Into<String>,
        size: PtySize,
        lease: ShellLease,
    ) -> (PtySession, PtyEndpoint) {
        let (out_tx, out_rx) = mpsc::channel(OUTPUT_QUEUE_CHUNKS);
        let (in_tx, in_rx) = mpsc::channel(INPUT_QUEUE_CHUNKS);
        (
            PtySession {
                id: id.into(),
                target: target.into(),
                size: Mutex::new(size),
                output: out_rx,
                input: in_tx,
                _lease: lease,
            },
            PtyEndpoint {
                output: out_tx,
                input: in_rx,
            },
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The target id this session was opened on.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The last size the far side was told about.
    pub fn size(&self) -> PtySize {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The next thing the shell produced, in order; None once the provider has hung up
    /// and the queue is drained.
    pub async fn next_event(&mut self) -> Option<PtyEvent> {
        self.output.recv().await
    }

    /// Send keystrokes. Parks while the input queue is full rather than dropping them.
    pub async fn write(&self, data: Vec<u8>) -> Result<(), ShellError> {
        self.send(PtyInput::Data(data)).await
    }

    /// Tell the far side the window changed. Recorded locally only once the provider has
    /// accepted it, so [PtySession::size] never claims a size that was never sent.
    pub async fn resize(&self, size: PtySize) -> Result<(), ShellError> {
        self.send(PtyInput::Resize(size)).await?;
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = size;
        Ok(())
    }

    async fn send(&self, input: PtyInput) -> Result<(), ShellError> {
        self.input
            .send(input)
            .await
            .map_err(|_| ShellError::Closed(format!("shell session {} has closed", self.id)))
    }
}

impl PtyEndpoint {
    /// Publish one event. Parks while the consumer is behind — that parking IS the
    /// backpressure, and it is why the provider must call this from the same task that
    /// reads its PTY (docs/14 §6.8).
    pub async fn send(&self, event: PtyEvent) -> Result<(), ShellError> {
        self.output
            .send(event)
            .await
            .map_err(|_| ShellError::Closed("the shell consumer has gone away".to_string()))
    }

    /// The next thing the consumer sent; None once the session has been dropped, which is
    /// the provider's signal to tear the far side down.
    pub async fn next_input(&mut self) -> Option<PtyInput> {
        self.input.recv().await
    }

    /// Whether the consumer is already gone, for a provider that wants to give up before
    /// producing more output.
    pub fn is_closed(&self) -> bool {
        self.output.is_closed()
    }

    /// Split into the two directions so a provider can pump each in its OWN task.
    ///
    /// Necessary rather than stylistic. One `select!` loop cannot do this correctly: it
    /// must park on a full output queue (that parking is the backpressure), and while it
    /// is parked nothing is reading input — so a Ctrl-C typed into a terminal that is
    /// flooding would never reach the far side, which is acceptance item 7 in docs/14
    /// §10. Moving the send inside the select instead makes it cancellable, and a
    /// cancelled send drops the bytes it was carrying. Two tasks have neither problem.
    pub fn split(self) -> (PtyOut, PtyIn) {
        (
            PtyOut {
                output: self.output,
            },
            PtyIn { input: self.input },
        )
    }
}

/// The outbound half of a [PtyEndpoint]: whatever the far side produced.
#[derive(Clone)]
pub struct PtyOut {
    output: mpsc::Sender<PtyEvent>,
}

impl PtyOut {
    /// Publish one event, parking while the consumer is behind. See [PtyEndpoint::send].
    pub async fn send(&self, event: PtyEvent) -> Result<(), ShellError> {
        self.output
            .send(event)
            .await
            .map_err(|_| ShellError::Closed("the shell consumer has gone away".to_string()))
    }

    /// Resolves when the consumer drops its session. A provider that is merely waiting
    /// for output needs this: without it, a quiet remote shell would hold its connection
    /// open long after the tab that asked for it went away.
    pub async fn closed(&self) {
        self.output.closed().await
    }

    pub fn is_closed(&self) -> bool {
        self.output.is_closed()
    }
}

/// The inbound half of a [PtyEndpoint]: keystrokes and resizes.
pub struct PtyIn {
    input: mpsc::Receiver<PtyInput>,
}

impl PtyIn {
    /// The next thing the consumer sent; None once the session has been dropped.
    pub async fn next(&mut self) -> Option<PtyInput> {
        self.input.recv().await
    }
}

/// What a provider registers: the hosts it can open a shell on, and how to open one.
#[async_trait]
pub trait ShellProvider: Send + Sync {
    fn list(&self) -> Vec<ShellTarget>;

    /// Open one interactive PTY. The provider owns the connection, its credentials and
    /// its host-key policy; the caller sees bytes and nothing else. While the returned
    /// session is alive the provider MUST keep the underlying client alive (the session
    /// holds a lease), and dropping it is what allows the client to be released.
    async fn open(&self, id: &str, holder: &str, size: PtySize) -> Result<PtySession, ShellError>;
}

/// Whether the shell capability can serve right now, and if not, who is missing — the
/// input to GET /api/terminal/targets, so its message can NAME the absent plugin instead
/// of reporting an empty host list (which reads as "you have no servers").
///
/// Not shared with [crate::services::catalog::CatalogPresence] on purpose: two capability
/// contracts that happen to have the same three states today should not become one type
/// that both must agree to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellPresence {
    /// A provider is registered and serving.
    Serving(String),
    /// The provider is draining; no new sessions.
    Stopping(String),
    /// No provider. The id is the LAST one, when there was one.
    Absent(Option<String>),
}

impl ShellPresence {
    /// The wire word, as docs/14 §8 spells it.
    pub fn as_str(&self) -> &'static str {
        match self {
            ShellPresence::Serving(_) => "serving",
            ShellPresence::Stopping(_) => "stopping",
            ShellPresence::Absent(_) => "absent",
        }
    }
}

/// The single-provider registry, [crate::services::catalog::CatalogRegistry]'s shape.
/// One shell provider per process: the day a second one exists, this grows a
/// per-capability map — not a silent overwrite.
#[derive(Default)]
pub struct ShellRegistry {
    state: Mutex<ShellState>,
}

#[derive(Default)]
struct ShellState {
    provider: Option<Arc<dyn ShellProvider>>,
    provider_id: Option<String>,
    last_provider_id: Option<String>,
    withdrawing: bool,
}

impl ShellRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the one provider. Err names the incumbent when the seat is taken — even a
    /// withdrawing one, because the drain still owns it until [ShellRegistry::clear].
    pub fn register(
        &self,
        provider: Arc<dyn ShellProvider>,
        provider_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = &state.provider_id {
            return Err(format!(
                "the interactive shell capability is already provided by plugin {current}"
            ));
        }
        state.provider = Some(provider);
        state.provider_id = Some(provider_id.to_string());
        state.last_provider_id = Some(provider_id.to_string());
        state.withdrawing = false;
        Ok(())
    }

    /// A provider's stop, first half: no new sessions go out, the live ones are expected
    /// back (see [drain_sessions]). The seat stays occupied until [ShellRegistry::clear].
    pub fn begin_withdraw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.provider.is_some() {
            state.withdrawing = true;
        }
    }

    /// A provider's stop, last half: the seat is free for the next start. The last
    /// provider id is kept for the honest "who is missing" answer.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.provider = None;
        state.provider_id = None;
        state.withdrawing = false;
    }

    pub fn list(&self) -> Vec<ShellTarget> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .provider
            .as_ref()
            .map(|p| p.list())
            .unwrap_or_default()
    }

    pub async fn open(
        &self,
        id: &str,
        holder: &str,
        size: PtySize,
    ) -> Result<PtySession, ShellError> {
        let provider = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(provider) = state.provider.clone() else {
                return Err(ShellError::Unavailable(
                    "no plugin currently provides interactive shells".to_string(),
                ));
            };
            if state.withdrawing {
                return Err(ShellError::Withdrawing(format!(
                    "plugin {} is stopping; no new shell sessions",
                    state.provider_id.as_deref().unwrap_or("?")
                )));
            }
            provider
        };
        // Open OUTSIDE the registry lock, and never hold a std Mutex across an await: the
        // provider locks its own state, and a lock-order inversion between the two is
        // exactly what the borrow checker will not catch for us.
        provider.open(id, holder, size).await
    }

    /// Serving state + who is missing, for the targets endpoint and capability probes.
    pub fn presence(&self) -> ShellPresence {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match (&state.provider_id, state.withdrawing) {
            (Some(id), false) => ShellPresence::Serving(id.clone()),
            (Some(id), true) => ShellPresence::Stopping(id.clone()),
            (None, _) => ShellPresence::Absent(state.last_provider_id.clone()),
        }
    }

    /// Whether a provider is registered and serving — what a capability probe answers.
    pub fn has_provider(&self) -> bool {
        matches!(self.presence(), ShellPresence::Serving(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A provider with one reachable target and one whose connection is down, plus an
    /// open counter and the ledger its leases are drawn from.
    struct StubShells {
        ledger: Arc<SessionLedger>,
        opens: AtomicUsize,
    }

    impl StubShells {
        fn new() -> Arc<Self> {
            Arc::new(StubShells {
                ledger: SessionLedger::new(),
                opens: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ShellProvider for StubShells {
        fn list(&self) -> Vec<ShellTarget> {
            vec![
                ShellTarget {
                    id: "box-one".into(),
                    label: "box one".into(),
                    host: "10.0.0.1".into(),
                    port: 22,
                    username: "dev".into(),
                    state: "connected".into(),
                },
                ShellTarget {
                    id: "box-down".into(),
                    label: "box down".into(),
                    host: "10.0.0.2".into(),
                    port: 22,
                    username: "dev".into(),
                    state: "error".into(),
                },
            ]
        }

        async fn open(
            &self,
            id: &str,
            holder: &str,
            size: PtySize,
        ) -> Result<PtySession, ShellError> {
            match id {
                "box-one" => {
                    self.opens.fetch_add(1, Ordering::SeqCst);
                    let lease = self.ledger.grant(id, holder);
                    let (session, endpoint) = PtySession::duplex("s1", id, size, lease);
                    // Echo the far side: one greeting, then whatever is typed, so the
                    // duplex is exercised end to end without a real shell.
                    tokio::spawn(async move {
                        let mut endpoint = endpoint;
                        if endpoint.send(PtyEvent::Data(b"$ ".to_vec())).await.is_err() {
                            return;
                        }
                        while let Some(input) = endpoint.next_input().await {
                            let event = match input {
                                PtyInput::Data(bytes) => PtyEvent::Data(bytes),
                                PtyInput::Resize(size) => {
                                    PtyEvent::Data(format!("{}x{}", size.cols, size.rows).into())
                                }
                            };
                            if endpoint.send(event).await.is_err() {
                                return;
                            }
                        }
                        let _ = endpoint.send(PtyEvent::Exit { code: Some(0) }).await;
                    });
                    Ok(session)
                }
                "box-down" => Err(ShellError::Unavailable(format!(
                    "tunnel connection '{id}' is not connected"
                ))),
                other => Err(ShellError::Unknown(format!(
                    "unknown shell target: {other}"
                ))),
            }
        }
    }

    fn size() -> PtySize {
        PtySize::new(80, 24).expect("a sane default is in range")
    }

    #[test]
    fn a_pty_size_outside_the_bounds_is_refused_not_clamped() {
        assert!(PtySize::new(0, 24)
            .expect_err("zero columns")
            .contains("cols"));
        assert!(PtySize::new(80, 0).expect_err("zero rows").contains("rows"));
        let err = PtySize::new(80, MAX_PTY_AXIS + 1).expect_err("too tall");
        assert!(err.contains("rows") && err.contains("1000"), "{err}");
        // Both ends of the range are legal - the bound is inclusive.
        assert!(PtySize::new(MIN_PTY_AXIS, MIN_PTY_AXIS).is_ok());
        assert!(PtySize::new(MAX_PTY_AXIS, MAX_PTY_AXIS).is_ok());
    }

    #[tokio::test]
    async fn a_duplicate_registration_is_refused_and_the_incumbent_stays() {
        let reg = ShellRegistry::new();
        reg.register(StubShells::new(), "tunnels")
            .expect("first registers");
        let err = reg
            .register(StubShells::new(), "other")
            .expect_err("the seat is taken");
        assert!(
            err.contains("already provided by plugin tunnels"),
            "the refusal must name the incumbent: {err}"
        );
        assert_eq!(reg.list().len(), 2, "the incumbent still answers");
        assert!(matches!(reg.presence(), ShellPresence::Serving(id) if id == "tunnels"));
    }

    #[tokio::test]
    async fn withdrawal_refuses_new_sessions_and_clear_releases_the_seat() {
        let reg = ShellRegistry::new();
        reg.register(StubShells::new(), "tunnels")
            .expect("registers");
        reg.begin_withdraw();
        match reg.open("box-one", "terminal", size()).await {
            Err(ShellError::Withdrawing(m)) => assert!(m.contains("tunnels"), "{m}"),
            Ok(_) => panic!("a withdrawing provider must not open a session"),
            Err(other) => panic!("withdrawing refuses, wrong error: {other}"),
        }
        assert_eq!(reg.presence().as_str(), "stopping");
        // A duplicate while withdrawing is STILL a duplicate: the drain owns the seat.
        assert!(reg.register(StubShells::new(), "other").is_err());
        reg.clear();
        assert!(matches!(
            reg.presence(),
            ShellPresence::Absent(Some(id)) if id == "tunnels"
        ));
        assert_eq!(reg.presence().as_str(), "absent");
        reg.register(StubShells::new(), "tunnels")
            .expect("re-registers after clear");
    }

    #[tokio::test]
    async fn an_absent_provider_is_named_rather_than_reported_as_no_hosts() {
        let reg = ShellRegistry::new();
        // Before anyone has ever registered there is nobody to name, and the list is
        // empty - but the error says "no provider", not "no such host".
        assert!(matches!(reg.presence(), ShellPresence::Absent(None)));
        assert!(reg.list().is_empty());
        match reg.open("box-one", "terminal", size()).await {
            Err(ShellError::Unavailable(m)) => assert!(m.contains("provides"), "{m}"),
            Ok(_) => panic!("no provider, got a session"),
            Err(other) => panic!("no provider, wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn unknown_and_unreachable_targets_are_told_apart() {
        let reg = ShellRegistry::new();
        reg.register(StubShells::new(), "tunnels")
            .expect("registers");
        match reg.open("ghost", "terminal", size()).await {
            Err(ShellError::Unknown(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown, got a session"),
            Err(other) => panic!("unknown, wrong error: {other}"),
        }
        match reg.open("box-down", "terminal", size()).await {
            Err(ShellError::Unavailable(m)) => assert!(m.contains("not connected"), "{m}"),
            Ok(_) => panic!("unreachable, got a session"),
            Err(other) => panic!("unreachable, wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn a_session_holds_its_lease_until_it_drops() {
        let reg = ShellRegistry::new();
        let provider = StubShells::new();
        reg.register(provider.clone(), "tunnels")
            .expect("registers");
        let session = reg
            .open("box-one", "terminal", size())
            .await
            .expect("opened");
        assert_eq!(session.target(), "box-one");
        assert_eq!(session.size(), size());
        assert_eq!(provider.ledger.outstanding(), 1, "the session is counted");
        drop(session); // the release must not depend on the holder remembering anything
        assert_eq!(
            provider.ledger.wait_idle(Duration::from_secs(5)).await,
            0,
            "dropping the session hands the lease back"
        );
    }

    #[tokio::test]
    async fn the_duplex_carries_bytes_both_ways_and_ends_with_the_exit() {
        let reg = ShellRegistry::new();
        reg.register(StubShells::new(), "tunnels")
            .expect("registers");
        let mut session = reg
            .open("box-one", "terminal", size())
            .await
            .expect("opened");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"$ ".to_vec()))
        );
        session.write(b"whoami\n".to_vec()).await.expect("typed");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"whoami\n".to_vec()))
        );
        let bigger = PtySize::new(120, 30).expect("in range");
        session.resize(bigger).await.expect("resized");
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(b"120x30".to_vec())),
            "a resize reaches the far side in order, behind the keystrokes"
        );
        assert_eq!(session.size(), bigger, "and is recorded once accepted");
    }

    #[tokio::test]
    async fn a_full_output_queue_parks_the_provider_instead_of_dropping_bytes() {
        // docs/14 §6.8: never drop a byte - a lost escape sequence corrupts the display
        // permanently. The proof is that the provider's send is still pending while the
        // consumer is behind, and that every chunk arrives once it catches up.
        let ledger = SessionLedger::new();
        let lease = ledger.grant("box-one", "terminal");
        let (mut session, endpoint) = PtySession::duplex("s1", "box-one", size(), lease);
        for i in 0..OUTPUT_QUEUE_CHUNKS {
            endpoint
                .send(PtyEvent::Data(vec![i as u8]))
                .await
                .expect("the queue has room");
        }
        let blocked = endpoint.send(PtyEvent::Data(vec![255]));
        tokio::pin!(blocked);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut blocked)
                .await
                .is_err(),
            "a full queue must park the producer, not accept and discard"
        );
        // Read one chunk: the parked send now completes, and nothing was lost.
        assert_eq!(
            session.next_event().await,
            Some(PtyEvent::Data(vec![0])),
            "the queue is FIFO"
        );
        blocked.await.expect("the parked send resumes");
        for i in 1..OUTPUT_QUEUE_CHUNKS {
            assert_eq!(
                session.next_event().await,
                Some(PtyEvent::Data(vec![i as u8]))
            );
        }
        assert_eq!(session.next_event().await, Some(PtyEvent::Data(vec![255])));
    }

    #[tokio::test]
    async fn a_split_endpoint_still_takes_input_while_its_output_is_parked() {
        // docs/14 §10 item 7: Ctrl-C must reach a terminal that is flooding. With the two
        // directions in one select! loop it could not - the loop would be parked on the
        // full output queue. Split, the input side keeps moving.
        let ledger = SessionLedger::new();
        let lease = ledger.grant("box-one", "terminal");
        let (session, endpoint) = PtySession::duplex("s1", "box-one", size(), lease);
        let (out, mut input) = endpoint.split();
        for i in 0..OUTPUT_QUEUE_CHUNKS {
            out.send(PtyEvent::Data(vec![i as u8]))
                .await
                .expect("the queue has room");
        }
        let flooding = out.send(PtyEvent::Data(b"more".to_vec()));
        tokio::pin!(flooding);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut flooding)
                .await
                .is_err(),
            "the output side is parked, as it should be"
        );
        // ...and the interrupt still gets through, which is the whole point.
        session.write(vec![0x03]).await.expect("Ctrl-C is accepted");
        assert_eq!(input.next().await, Some(PtyInput::Data(vec![0x03])));
    }

    #[tokio::test]
    async fn a_split_output_half_learns_that_the_consumer_left() {
        let ledger = SessionLedger::new();
        let lease = ledger.grant("box-one", "terminal");
        let (session, endpoint) = PtySession::duplex("s1", "box-one", size(), lease);
        let (out, mut input) = endpoint.split();
        assert!(!out.is_closed());
        drop(session);
        // A quiet remote produces nothing to fail on, so "the consumer left" has to be
        // observable on its own - otherwise the connection outlives the tab.
        tokio::time::timeout(Duration::from_secs(5), out.closed())
            .await
            .expect("closed() resolves when the session drops");
        assert!(out.is_closed());
        assert!(input.next().await.is_none());
    }

    #[tokio::test]
    async fn dropping_the_session_is_what_tells_the_provider_to_tear_down() {
        let ledger = SessionLedger::new();
        let lease = ledger.grant("box-one", "terminal");
        let (session, mut endpoint) = PtySession::duplex("s1", "box-one", size(), lease);
        assert!(!endpoint.is_closed());
        drop(session);
        assert!(
            endpoint.next_input().await.is_none(),
            "the input side hangs up"
        );
        assert!(endpoint.is_closed(), "and so does the output side");
        assert!(
            endpoint
                .send(PtyEvent::Data(b"late".to_vec()))
                .await
                .is_err(),
            "writing to a gone consumer fails instead of buffering forever"
        );
    }

    #[tokio::test]
    async fn writing_to_a_provider_that_has_gone_away_reports_closed() {
        let ledger = SessionLedger::new();
        let lease = ledger.grant("box-one", "terminal");
        let (session, endpoint) = PtySession::duplex("s1", "box-one", size(), lease);
        drop(endpoint);
        match session.write(b"x".to_vec()).await {
            Err(ShellError::Closed(m)) => assert!(m.contains("s1"), "{m}"),
            Ok(()) => panic!("the far side is gone; the write must fail"),
            Err(other) => panic!("closed expected, got: {other}"),
        }
    }

    #[tokio::test]
    async fn the_drain_reports_the_sessions_it_had_to_close_over() {
        let ledger = SessionLedger::new();
        let held = ledger.grant("box-one", "terminal");
        assert_eq!(held.holder(), "terminal");
        assert_eq!(held.target(), "box-one");
        let remaining = drain_sessions(&ledger, Duration::from_millis(50), "tunnels").await;
        assert_eq!(
            remaining, 1,
            "the session never came back within the window"
        );
        drop(held);
        assert_eq!(
            drain_sessions(&ledger, Duration::from_millis(50), "tunnels").await,
            0,
            "once back, the close is clean"
        );
    }

    #[tokio::test]
    async fn the_drain_is_woken_by_a_release_not_just_the_timeout() {
        let ledger = SessionLedger::new();
        let held = ledger.grant("box-one", "terminal");
        let waiter = {
            let ledger = ledger.clone();
            tokio::spawn(async move { ledger.wait_idle(Duration::from_secs(30)).await })
        };
        tokio::task::yield_now().await; // let the waiter subscribe
        drop(held);
        assert_eq!(waiter.await.expect("waiter"), 0, "woken by the release");
    }

    #[tokio::test]
    async fn withdrawing_is_visible_on_the_ledger_too() {
        // The registry refuses new opens, but a provider with its own open path needs to
        // see the same flag - otherwise "withdrawing" means two different things.
        let ledger = SessionLedger::new();
        assert!(!ledger.is_withdrawing());
        ledger.begin_withdraw();
        assert!(ledger.is_withdrawing());
        ledger.begin_withdraw();
        assert!(ledger.is_withdrawing(), "idempotent");
    }

    #[tokio::test]
    async fn a_hundred_lifecycles_leave_the_registry_exactly_empty() {
        // docs/10 §9's last acceptance item, at the capability level: enable/disable churn
        // must not leak providers, sessions or rows.
        let reg = ShellRegistry::new();
        let provider = StubShells::new();
        for _ in 0..100 {
            reg.register(provider.clone(), "tunnels")
                .expect("registers");
            let session = reg
                .open("box-one", "terminal", size())
                .await
                .expect("opened");
            drop(session);
            reg.begin_withdraw();
            assert_eq!(
                drain_sessions(&provider.ledger, Duration::from_secs(5), "tunnels").await,
                0,
                "every cycle drains clean"
            );
            reg.clear();
        }
        assert!(matches!(reg.presence(), ShellPresence::Absent(Some(_))));
        assert!(reg.list().is_empty());
        assert!(!reg.has_provider());
        assert_eq!(provider.ledger.outstanding(), 0, "no lease leaked");
    }
}
