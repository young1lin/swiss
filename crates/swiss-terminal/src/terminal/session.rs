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

//! The session table and the per-session driver (SPEC §terminal.config).
//!
//! One driver task per session owns the [`PtySession`] and is the only thing that touches
//! it. Everything else — opening, listing, attaching a socket, a resize that arrived over
//! HTTP instead of over the socket, closing — goes through a command channel to that
//! task. That is not ceremony: it is what makes "a Ctrl-C typed into a flooding terminal
//! still gets through" true, because the driver's send loop keeps serving commands while
//! it is parked on a full output queue.
//!
//! ## The four clocks, and what each protects
//!
//! - **idle** (`idleTimeoutMinutes`, 30) — nothing in either direction. Output counts as
//!   activity on purpose: a compile that prints for an hour is not idle, and killing it
//!   would be the exact failure the grace window below exists to prevent.
//! - **grace** (`graceSeconds`, 60) — the socket is gone but the session is not. Output
//!   keeps flowing into a bounded catch-up buffer; a reconnect inside the window picks it
//!   up. A session that is opened and never attached is on this clock from birth, which
//!   is also how a POST nobody followed up on gets collected.
//! - **stall** (`stallSeconds`, 30) — attached, but the socket has not accepted a byte.
//!   The session is closed and says why; `{"t":"stalled"}` goes out first, after one
//!   second, so a user watching a frozen terminal learns which side is stuck.
//! - the recorder's own cap, next door in [`super::recording`].
//!
//! ## Bytes are never dropped, except in one place that says so
//!
//! While a client is attached the driver parks rather than drops (SPEC §terminal.sessions): the
//! output queue fills, the driver stops reading the session, the provider stops reading
//! its PTY or SSH channel, and the pressure reaches the program that is printing. Exactly
//! one place is allowed to lose bytes — the catch-up buffer of a **disconnected** session,
//! which is 64 KB and drops from the front. It has to: the alternative is stopping the
//! read, which would freeze the compile the grace window is there to protect. When it has
//! dropped anything, the reconnecting terminal is told so in a visible line rather than
//! silently shown a hole.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use swiss_host::services::shell::{
    PtyEvent, PtySession, PtySize, ShellError, ShellPresence, ShellRegistry,
};

use super::config::TerminalConfig;
use super::local::{LocalShell, LOCAL_LABEL, LOCAL_TARGET};
use super::recording::Recorder;
use super::tickets::{TicketBook, TicketError};

use swiss_core::platform::pty::ShellCandidate;

/// SPEC §terminal.budget: 64 KB per session, a constant and not a config item, because it is a line
/// in the memory budget rather than a preference.
pub const CATCHUP_BYTES: usize = 64 * 1024;

/// How many frames may sit between the driver and one attached socket. Small: this queue
/// existing at all is what lets the driver notice a stalled client, and a deep one would
/// only move the stall somewhere it cannot be seen.
pub const CLIENT_QUEUE_FRAMES: usize = 16;

/// Commands waiting for a driver. A human types slower than this; a paste larger than it
/// is a paste that can wait.
const COMMAND_QUEUE: usize = 32;

/// How long a full client queue is tolerated before `{"t":"stalled"}` goes out. The hard
/// close is `stallSeconds` later; this is the warning, and one second is about when a
/// person starts wondering whether their terminal is broken.
const STALL_NOTICE: Duration = Duration::from_secs(1);

/// How long the close path waits to hand a client its last frames. Bounded because the
/// most likely reason a session is closing is that nobody is reading it.
const CLOSE_FLUSH: Duration = Duration::from_millis(250);

/// What the panel is told about one session, in the order SPEC §terminal.api spells it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub target: String,
    pub label: String,
    pub opened: String,
    pub bytes_out: u64,
    pub attached: bool,
    pub recording: Option<String>,
}

/// `GET /api/terminal/targets`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TargetsView {
    pub local: LocalView,
    pub remote: RemoteView,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LocalView {
    pub enabled: bool,
    /// The program a session would actually run, resolved to an absolute path where the
    /// platform can (SPEC §terminal.local) — the panel's `local · …` label must be the truth.
    pub shell: String,
    /// Every shell this host offers, probed once at plugin start: the settings sheet's
    /// candidate list. Additive on purpose — the panel's older readers only look at
    /// `enabled` and `shell` (SPEC §terminal.api).
    pub shells: Vec<ShellCandidate>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RemoteView {
    pub presence: String,
    /// Present only when there is nothing to serve, and it NAMES the plugin that is
    /// missing (SPEC §terminal.remote). An empty target list on its own reads as "you have no
    /// servers", which is a different and wrong statement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub targets: Vec<TargetView>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TargetView {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub state: String,
}

/// The answer to `POST /api/terminal/sessions`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Opened {
    pub id: String,
    pub ticket: String,
    pub recording: Option<String>,
}

/// What one attached client receives. Binary data and the two terminal events share one
/// ordered channel so an exit can never be reported before the last bytes the shell
/// wrote; `stalled` is out of band precisely because the data channel is what is stuck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientFrame {
    Data(Vec<u8>),
    Exit { code: Option<i32> },
    Error(String),
}

/// One attached socket's end of a session. Dropping it detaches — which starts the grace
/// window rather than closing anything.
#[derive(Debug)]
pub struct Attachment {
    pub frames: mpsc::Receiver<ClientFrame>,
    /// `true` while the driver has output it cannot hand over. T5 turns each edge into a
    /// `{"t":"stalled"}` text frame.
    pub stalled: watch::Receiver<bool>,
}

/// Why a session ended. Every variant carries what the user is told, because a terminal
/// that closes without saying why is indistinguishable from one that crashed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The shell exited on its own — the only ending that needs no explanation.
    Exited(Option<i32>),
    /// The transport or the provider failed under us.
    Failed(String),
    Idle(Duration),
    Stalled(Duration),
    /// Nobody reconnected inside the grace window.
    Abandoned(Duration),
    /// DELETE, or the panel closing the tab.
    Requested,
    /// The plugin is stopping.
    Shutdown,
}

impl CloseReason {
    /// The user-facing sentence. Written into the terminal, into the recording's end
    /// marker, and into the `error` frame.
    pub fn text(&self) -> String {
        match self {
            CloseReason::Exited(Some(code)) => format!("the shell exited with code {code}"),
            CloseReason::Exited(None) => "the shell exited".to_string(),
            CloseReason::Failed(why) => why.clone(),
            CloseReason::Idle(after) => format!(
                "closed after {} minutes with no activity (idleTimeoutMinutes)",
                after.as_secs() / 60
            ),
            CloseReason::Stalled(after) => format!(
                "closed: this terminal accepted no output for {} seconds (stallSeconds)",
                after.as_secs()
            ),
            CloseReason::Abandoned(after) => format!(
                "closed: nothing reconnected within {} seconds of the socket dropping (graceSeconds)",
                after.as_secs()
            ),
            CloseReason::Requested => "closed on request".to_string(),
            CloseReason::Shutdown => "closed: the terminal plugin is stopping".to_string(),
        }
    }

    /// Whether the terminal gets a visible line before it goes. Everything except the
    /// shell exiting: that one already said goodbye in its own words.
    fn is_worth_saying(&self) -> bool {
        !matches!(self, CloseReason::Exited(_))
    }
}

/// Why a request against the session machine failed. The HTTP layer maps these to status
/// codes; the strings are what the user reads.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("no such terminal session")]
    NoSession,
    #[error("{0}")]
    Ticket(TicketError),
    #[error("{0}")]
    Refused(String),
    /// Straight from the provider or the local PTY. Wrapped by hand rather than with
    /// `#[from]`: [ShellError] is a user-facing message type in the host contract, not a
    /// `std::error::Error`, and making it one there to satisfy a derive here would be the
    /// consumer dictating the contract's shape.
    #[error("{0}")]
    Shell(ShellError),
}

impl From<ShellError> for TerminalError {
    fn from(err: ShellError) -> Self {
        TerminalError::Shell(err)
    }
}

/// Live counters for one session, shared with the listing so a `GET` never has to ask the
/// driver anything.
#[derive(Default)]
struct SessionStats {
    bytes_out: AtomicU64,
    attached: AtomicBool,
}

enum DriverCommand {
    Attach(AttachedSink),
    Input(Vec<u8>),
    Resize(PtySize),
    Close,
}

/// One attached socket's server-side end, as the driver holds it. A Vec of these, not a
/// single slot: two panel tabs may watch one session, so output fans out to every
/// attachment, input from any attachment reaches the PTY, and one socket going away
/// removes only its own entry — the grace clock starts when the LAST one drops.
struct AttachedSink {
    frames: mpsc::Sender<ClientFrame>,
    stalled: watch::Sender<bool>,
}

struct Entry {
    target: String,
    label: String,
    opened: String,
    recording: Option<String>,
    commands: mpsc::Sender<DriverCommand>,
    stats: Arc<SessionStats>,
}

#[derive(Default)]
struct TableState {
    live: HashMap<String, Entry>,
    /// Targets with an open in flight. An open awaits (an SSH channel request is a round
    /// trip), so without this two simultaneous opens would both see room and both take
    /// it — the caps would be advisory rather than caps.
    opening: Vec<String>,
}

struct Table {
    config: TerminalConfig,
    shells: Arc<ShellRegistry>,
    local: Arc<dyn LocalShell>,
    dir: PathBuf,
    tickets: TicketBook,
    state: Mutex<TableState>,
}

/// The public face: everything /api/terminal does, minus the HTTP.
pub struct TerminalSessions {
    table: Arc<Table>,
}

impl TerminalSessions {
    /// `dir` is where recordings go — `~/.swiss/terminal` in the gateway, a scratch
    /// directory in tests.
    pub fn new(
        config: TerminalConfig,
        shells: Arc<ShellRegistry>,
        local: Arc<dyn LocalShell>,
        dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new(TerminalSessions {
            table: Arc::new(Table {
                config,
                shells,
                local,
                dir,
                tickets: TicketBook::new(),
                state: Mutex::new(TableState::default()),
            }),
        })
    }

    pub fn config(&self) -> &TerminalConfig {
        &self.table.config
    }

    /// What may be opened right now, and — when nothing remote may be — who is missing.
    pub fn targets(&self) -> TargetsView {
        let table = &self.table;
        let presence = table.shells.presence();
        let allowed = |id: &str| {
            table.config.allowed_targets.is_empty()
                || table.config.allowed_targets.iter().any(|a| a == id)
        };
        let targets = match presence {
            ShellPresence::Serving(_) => table
                .shells
                .list()
                .into_iter()
                .filter(|t| allowed(&t.id))
                .map(|t| TargetView {
                    id: t.id,
                    label: t.label,
                    host: t.host,
                    port: t.port,
                    username: t.username,
                    state: t.state,
                })
                .collect(),
            _ => Vec::new(),
        };
        let reason = match &presence {
            ShellPresence::Serving(_) => None,
            ShellPresence::Stopping(who) => Some(format!(
                "the {who} plugin is stopping; no new remote sessions"
            )),
            ShellPresence::Absent(Some(who)) => Some(format!(
                "the {who} plugin is not running, so no remote hosts can be reached"
            )),
            ShellPresence::Absent(None) => {
                Some("no plugin currently provides interactive shells".to_string())
            }
        };
        TargetsView {
            local: LocalView {
                enabled: table.config.local.enabled,
                shell: match table.config.local.shell.as_deref() {
                    // A configured name is resolved so the label shows the path that
                    // would actually spawn, not what the user typed.
                    Some(configured) => table.local.resolve(configured),
                    None => table.local.program(),
                },
                shells: table.local.candidates(),
            },
            remote: RemoteView {
                presence: presence.as_str().to_string(),
                reason,
                targets,
            },
        }
    }

    pub fn list(&self) -> Vec<SessionView> {
        let state = self.table.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<SessionView> = state
            .live
            .iter()
            .map(|(id, entry)| SessionView {
                id: id.clone(),
                target: entry.target.clone(),
                label: entry.label.clone(),
                opened: entry.opened.clone(),
                bytes_out: entry.stats.bytes_out.load(Ordering::Relaxed),
                attached: entry.stats.attached.load(Ordering::Relaxed),
                recording: entry.recording.clone(),
            })
            .collect();
        // Oldest first: a terminal list that reorders itself as bytes arrive is unusable.
        out.sort_by(|a, b| a.opened.cmp(&b.opened).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Open a session on `target` — `"local"` or a connection id from [Self::targets].
    pub async fn open(
        &self,
        target: &str,
        size: PtySize,
        shell: Option<&str>,
    ) -> Result<Opened, TerminalError> {
        let table = Arc::clone(&self.table);
        let label = table.resolve(target)?;
        let reservation = table.reserve(target)?;

        let id = swiss_core::util::random_hex(16);
        let session = if target == LOCAL_TARGET {
            // The request's one-shot override beats the plugin's configured shell; with
            // neither, the default the probe cached at start runs. Without this the
            // config row only ever changed the label, never the program (SPEC §terminal.local).
            let shell = shell.or_else(|| table.config.local.shell.as_deref());
            table.local.open(&id, size, shell)?
        } else {
            table.shells.open(target, "terminal", size).await?
        };
        drop(reservation);

        let recorder = table
            .config
            .recording
            .then(
                || match Recorder::create(&table.dir, &id, size.cols, size.rows) {
                    Ok(rec) => Some(rec),
                    Err(err) => {
                        // A recording that cannot be written must not cost the user a
                        // terminal; it costs them the recording, and says so once.
                        swiss_core::log::warn(
                            "could not start a terminal recording",
                            Some(serde_json::json!({ "session": id, "error": err.to_string() })),
                        );
                        None
                    }
                },
            )
            .flatten();
        let recording = recorder
            .as_ref()
            .map(|r| r.path().to_string_lossy().into_owned());

        let stats = Arc::new(SessionStats::default());
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_QUEUE);
        let driver = Driver {
            id: id.clone(),
            session,
            commands: commands_rx,
            attached: Vec::new(),
            place_at: 0,
            pending: None,
            backlog: Backlog::default(),
            recorder,
            stats: Arc::clone(&stats),
            config: table.config.clone(),
            last_activity: Instant::now(),
            // Detached from birth: the client has yet to open its socket, and the same
            // clock collects a session nobody ever came back for.
            detached_since: Some(Instant::now()),
            table: Arc::clone(&table),
        };

        {
            let mut state = table.state.lock().unwrap_or_else(|e| e.into_inner());
            state.live.insert(
                id.clone(),
                Entry {
                    target: target.to_string(),
                    label,
                    opened: swiss_core::log::iso_now(),
                    recording: recording.clone(),
                    commands: commands_tx,
                    stats,
                },
            );
        }
        tokio::spawn(driver.run());

        Ok(Opened {
            ticket: table.tickets.mint(&id),
            id,
            recording,
        })
    }

    /// A fresh ticket for an existing session — what a reconnect inside the grace window
    /// spends. Deliberately separate from [Self::open]: a ticket lives ten seconds and a
    /// grace window lives sixty, so the one minted at open cannot serve a reconnect.
    pub fn ticket(&self, id: &str) -> Result<String, TerminalError> {
        let state = self.table.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.live.contains_key(id) {
            return Err(TerminalError::NoSession);
        }
        Ok(self.table.tickets.mint(id))
    }

    /// Spend a ticket and JOIN the session's output. Attachments fan out — a second
    /// panel tab attaching must not blind the first (that was the single-slot bug):
    /// every attached socket receives every frame from here on, and the grace window
    /// only starts once the last one is gone. A reconnect is just another attach, so
    /// the old guarantee survives: a reconnect is never refused because a dead socket
    /// is still on the books — the dead one is pruned the moment output tries it.
    pub async fn attach(&self, id: &str, ticket: &str) -> Result<Attachment, TerminalError> {
        self.table
            .tickets
            .redeem(ticket, id)
            .map_err(TerminalError::Ticket)?;
        let commands = self.table.commands(id)?;
        let (frames_tx, frames_rx) = mpsc::channel(CLIENT_QUEUE_FRAMES);
        let (stalled_tx, stalled_rx) = watch::channel(false);
        commands
            .send(DriverCommand::Attach(AttachedSink {
                frames: frames_tx,
                stalled: stalled_tx,
            }))
            .await
            .map_err(|_| TerminalError::NoSession)?;
        Ok(Attachment {
            frames: frames_rx,
            stalled: stalled_rx,
        })
    }

    /// Keystrokes from an attached socket.
    pub async fn input(&self, id: &str, bytes: Vec<u8>) -> Result<(), TerminalError> {
        self.send(id, DriverCommand::Input(bytes)).await
    }

    /// A window change, from either the socket's resize frame or `POST /resize`. Both
    /// paths land here, which is the point of having both (SPEC §terminal.api).
    pub async fn resize(&self, id: &str, size: PtySize) -> Result<(), TerminalError> {
        self.send(id, DriverCommand::Resize(size)).await
    }

    pub async fn close(&self, id: &str) -> Result<(), TerminalError> {
        self.send(id, DriverCommand::Close).await
    }

    /// Close every session — the plugin stopping. Returns how many were closed.
    ///
    /// Closing OVER the sessions, not asking each one to close: dropping the table's
    /// command senders is the signal every driver already selects on
    /// (Wake::Command(None) => Shutdown), and "the terminal plugin is stopping" is the
    /// reason a stopping plugin owes its clients — "closed on request" is the DELETE
    /// path's story, not this one's.
    pub async fn shutdown(&self) -> usize {
        let mut state = self.table.state.lock().unwrap_or_else(|e| e.into_inner());
        let closed = state.live.len();
        state.live.clear();
        closed
    }

    async fn send(&self, id: &str, command: DriverCommand) -> Result<(), TerminalError> {
        self.table
            .commands(id)?
            .send(command)
            .await
            .map_err(|_| TerminalError::NoSession)
    }
}

impl Table {
    /// Is this target openable at all, and what is it called? Refusals are specific: a
    /// disabled local shell and an unknown connection id are different mistakes.
    fn resolve(&self, target: &str) -> Result<String, TerminalError> {
        if target == LOCAL_TARGET {
            if !self.config.local.enabled {
                return Err(TerminalError::Refused(
                    "the local shell is disabled; turn on local.enabled in the terminal \
                     plugin's configuration to allow it"
                        .to_string(),
                ));
            }
            return Ok(self
                .config
                .local
                .shell
                .clone()
                .unwrap_or_else(|| LOCAL_LABEL.to_string()));
        }
        if !self.config.allowed_targets.is_empty()
            && !self.config.allowed_targets.iter().any(|a| a == target)
        {
            return Err(TerminalError::Refused(format!(
                "target {target} is not in the terminal plugin's allowedTargets"
            )));
        }
        match self.shells.presence() {
            ShellPresence::Serving(_) => {}
            ShellPresence::Stopping(who) => {
                return Err(TerminalError::Refused(format!(
                    "the {who} plugin is stopping; no new remote sessions"
                )))
            }
            ShellPresence::Absent(Some(who)) => {
                return Err(TerminalError::Refused(format!(
                    "the {who} plugin is not running, so {target} cannot be reached"
                )))
            }
            ShellPresence::Absent(None) => {
                return Err(TerminalError::Refused(
                    "no plugin currently provides interactive shells".to_string(),
                ))
            }
        }
        self.shells
            .list()
            .into_iter()
            .find(|t| t.id == target)
            .map(|t| t.label)
            .ok_or_else(|| TerminalError::Refused(format!("no such shell target: {target}")))
    }

    /// Take a slot under the caps, or say which one is full. The guard holds the slot
    /// across the open's awaits.
    ///
    /// Local sessions are UNCAPPED (the operator's call, 2026-09-12): a local PTY costs a
    /// thread stack on a machine the operator owns, and they asked for as many tabs as they
    /// like — so a local open takes no checks and consumes no budget. Remote targets keep
    /// both caps, and only remote sessions count toward them: an SSH channel costs the far
    /// host's memory too, and a runaway loop of opens must not march through a host one
    /// after another while ten harmless local tabs sit between them.
    fn reserve(self: &Arc<Self>, target: &str) -> Result<Reservation, TerminalError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if target != LOCAL_TARGET {
            let remote_opening = state.opening.iter().filter(|t| *t != LOCAL_TARGET).count();
            let total = state
                .live
                .values()
                .filter(|e| e.target != LOCAL_TARGET)
                .count()
                + remote_opening;
            if total >= self.config.max_sessions {
                return Err(TerminalError::Refused(format!(
                    "already at maxSessions ({}); close a terminal before opening another",
                    self.config.max_sessions
                )));
            }
            let on_target = state.live.values().filter(|e| e.target == target).count()
                + state.opening.iter().filter(|t| *t == target).count();
            if on_target >= self.config.max_sessions_per_target {
                return Err(TerminalError::Refused(format!(
                    "already at maxSessionsPerTarget ({}) for {target}",
                    self.config.max_sessions_per_target
                )));
            }
        }
        state.opening.push(target.to_string());
        Ok(Reservation {
            table: Arc::clone(self),
            target: target.to_string(),
        })
    }

    fn commands(&self, id: &str) -> Result<mpsc::Sender<DriverCommand>, TerminalError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .live
            .get(id)
            .map(|e| e.commands.clone())
            .ok_or(TerminalError::NoSession)
    }

    /// A driver deregistering itself. The only place a session leaves the table, so the
    /// listing and the tickets can never disagree about what exists.
    fn remove(&self, id: &str, reason: &CloseReason) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.live.remove(id);
        }
        self.tickets.forget(id);
        swiss_core::log::log(
            "info",
            "terminal session closed",
            Some(serde_json::json!({ "session": id, "reason": reason.text() })),
        );
    }
}

/// A held slot in the session caps. Dropping it is the release, so an open that fails
/// halfway cannot leak the slot it took.
struct Reservation {
    table: Arc<Table>,
    target: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.table.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = state.opening.iter().position(|t| *t == self.target) {
            state.opening.remove(at);
        }
    }
}

/// The bounded catch-up buffer of a disconnected session (SPEC §terminal.sessions).
#[derive(Default)]
struct Backlog {
    bytes: VecDeque<u8>,
    dropped: u64,
    /// Set when something was dropped and the next attach has not been told yet.
    owed_notice: bool,
}

impl Backlog {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes.iter().copied());
        if self.bytes.len() > CATCHUP_BYTES {
            let over = self.bytes.len() - CATCHUP_BYTES;
            self.bytes.drain(..over);
            self.dropped += over as u64;
            self.owed_notice = true;
        }
    }

    /// The next thing a freshly attached client should be handed: the honesty notice
    /// first, then everything that survived.
    fn pop(&mut self) -> Option<ClientFrame> {
        if self.owed_notice {
            self.owed_notice = false;
            let dropped = self.dropped;
            return Some(ClientFrame::Data(
                format!(
                    "\r\n[gateway] {dropped} bytes of output were dropped while this terminal \
                     was disconnected\r\n"
                )
                .into_bytes(),
            ));
        }
        if self.bytes.is_empty() {
            return None;
        }
        Some(ClientFrame::Data(self.bytes.drain(..).collect()))
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.owed_notice
    }
}

/// One session's owner. Everything about a live terminal that is not in the table is here,
/// and it is single-threaded, so none of it needs a lock.
struct Driver {
    id: String,
    session: PtySession,
    commands: mpsc::Receiver<DriverCommand>,
    attached: Vec<AttachedSink>,
    /// Where [Driver::place] is inside [Driver::attached]: the index whose permit the
    /// frame in flight is currently waiting on. A fresh attachment pushes at the end,
    /// so a join mid-frame is simply served when the cursor reaches it — no duplicate,
    /// no hole.
    place_at: usize,
    /// The one frame in flight. Holding it in a field rather than in a local is what lets
    /// the send be interrupted by a command and resumed, instead of being cancelled —
    /// a cancelled `send` drops the bytes it was carrying.
    pending: Option<ClientFrame>,
    backlog: Backlog,
    recorder: Option<Recorder>,
    stats: Arc<SessionStats>,
    config: TerminalConfig,
    last_activity: Instant,
    detached_since: Option<Instant>,
    table: Arc<Table>,
}

/// What woke the driver's main select.
enum Wake {
    Command(Option<DriverCommand>),
    Event(Option<PtyEvent>),
    Detached,
    Idle,
    Abandoned,
}

/// What woke the driver while it was trying to hand a frame over.
enum Placed<'a> {
    Permit(mpsc::Permit<'a, ClientFrame>),
    Command(Option<DriverCommand>),
    Detached,
    Notice,
    Stalled,
}

impl Driver {
    async fn run(mut self) {
        let reason = self.pump().await;
        self.finish(reason).await;
    }

    async fn pump(&mut self) -> CloseReason {
        loop {
            // A reconnected client is owed the catch-up buffer before anything new.
            if self.pending.is_none() && !self.attached.is_empty() && !self.backlog.is_empty() {
                self.pending = self.backlog.pop();
            }
            if self.pending.is_some() {
                if let Some(reason) = self.place().await {
                    return reason;
                }
                continue;
            }

            let idle_at = (!self.config.idle_timeout.is_zero())
                .then(|| self.last_activity + self.config.idle_timeout);
            let grace_at = self.detached_since.map(|at| at + self.config.grace);

            let wake = {
                let Driver {
                    session,
                    commands,
                    attached,
                    ..
                } = &mut *self;
                tokio::select! {
                    biased;
                    command = commands.recv() => Wake::Command(command),
                    event = session.next_event() => Wake::Event(event),
                    _ = all_sinks_closed(attached) => Wake::Detached,
                    _ = deadline(idle_at) => Wake::Idle,
                    _ = deadline(grace_at) => Wake::Abandoned,
                }
            };

            match wake {
                Wake::Command(Some(command)) => {
                    if let Some(reason) = self.command(command).await {
                        return reason;
                    }
                }
                // The table dropped the session's sender without a Close — only possible
                // if the whole plugin went away underneath us.
                Wake::Command(None) => return CloseReason::Shutdown,
                Wake::Event(Some(PtyEvent::Data(bytes))) => self.produced(bytes),
                Wake::Event(Some(PtyEvent::Exit { code })) => return CloseReason::Exited(code),
                Wake::Event(Some(PtyEvent::Error(why))) => return CloseReason::Failed(why),
                Wake::Event(None) => return CloseReason::Exited(None),
                Wake::Detached => self.detach(),
                Wake::Idle => return CloseReason::Idle(self.config.idle_timeout),
                Wake::Abandoned => return CloseReason::Abandoned(self.config.grace),
            }
        }
    }

    /// Output from the shell: recorded, counted, and queued for whoever is listening.
    fn produced(&mut self, bytes: Vec<u8>) {
        self.last_activity = Instant::now();
        self.stats
            .bytes_out
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if let Some(recorder) = &mut self.recorder {
            recorder.output(&bytes);
        }
        self.pending = Some(ClientFrame::Data(bytes));
    }

    /// Hand [Driver::pending] to EVERY attached client, parking until it fits on each in
    /// turn. Commands are still served while parked — that is what keeps a Ctrl-C alive
    /// during a flood. The cursor ([Driver::place_at]) says which attachment the frame in
    /// flight is waiting on; it only resets once the last one has its copy, so an attach
    /// that arrives mid-frame is served when the cursor reaches it — no duplicate, no
    /// hole — and a detach that arrives mid-frame only removes its own unserved slot.
    async fn place(&mut self) -> Option<CloseReason> {
        let mut notice_at = Some(Instant::now() + STALL_NOTICE);
        let stall_at = (!self.config.stall.is_zero()).then(|| Instant::now() + self.config.stall);
        loop {
            if self.attached.is_empty() {
                // Nobody to hand it to: it goes in the catch-up buffer, and the grace
                // clock (started by the detach) decides how long that is worth doing.
                if let Some(ClientFrame::Data(bytes)) = self.pending.take() {
                    self.backlog.push(&bytes);
                }
                return None;
            }
            if self.place_at >= self.attached.len() {
                self.pending = None;
                self.place_at = 0;
                self.set_stalled(false);
                return None;
            }

            let sink = self.attached[self.place_at].frames.clone();
            let placed = {
                let commands = &mut self.commands;
                tokio::select! {
                    biased;
                    command = commands.recv() => Placed::Command(command),
                    permit = sink.reserve() => match permit {
                        Ok(permit) => Placed::Permit(permit),
                        Err(_) => Placed::Detached,
                    },
                    _ = deadline(notice_at) => Placed::Notice,
                    _ = deadline(stall_at) => Placed::Stalled,
                }
            };

            match placed {
                Placed::Permit(permit) => {
                    // Clone, not take: the SAME frame goes to every attachment. The
                    // completion branch above is what clears it, once all have a copy.
                    if let Some(frame) = self.pending.clone() {
                        permit.send(frame);
                    }
                    self.place_at += 1;
                }
                Placed::Command(Some(command)) => {
                    if let Some(reason) = self.command(command).await {
                        return Some(reason);
                    }
                }
                Placed::Command(None) => return Some(CloseReason::Shutdown),
                Placed::Detached => {
                    // This attachment's receiver is gone. It was never served this
                    // frame and never will be — drop the slot, not the session.
                    if self.place_at < self.attached.len() {
                        self.attached.remove(self.place_at);
                    }
                    if self.attached.is_empty() {
                        self.detach();
                    }
                }
                Placed::Notice => {
                    self.set_stalled(true);
                    notice_at = None;
                }
                Placed::Stalled => return Some(CloseReason::Stalled(self.config.stall)),
            }
        }
    }

    async fn command(&mut self, command: DriverCommand) -> Option<CloseReason> {
        match command {
            DriverCommand::Attach(sink) => {
                self.attached.push(sink);
                self.detached_since = None;
                self.stats.attached.store(true, Ordering::Relaxed);
                None
            }
            DriverCommand::Input(bytes) => {
                self.last_activity = Instant::now();
                match self.session.write(bytes).await {
                    Ok(()) => None,
                    Err(err) => Some(CloseReason::Failed(err.to_string())),
                }
            }
            DriverCommand::Resize(size) => match self.session.resize(size).await {
                Ok(()) => None,
                Err(err) => Some(CloseReason::Failed(err.to_string())),
            },
            DriverCommand::Close => Some(CloseReason::Requested),
        }
    }

    /// Every socket is gone. Not a close: the grace window starts here — for ALL of
    /// them, since one live tab keeps the session honestly attached.
    fn detach(&mut self) {
        if !self.attached.is_empty() {
            self.attached.clear();
            self.place_at = 0;
            self.stats.attached.store(false, Ordering::Relaxed);
            self.detached_since = Some(Instant::now());
        }
    }

    /// Only an actual edge is published. A plain `send` marks the channel changed even
    /// when the value is the same, so a client waiting on `changed()` would be woken by
    /// every successful write and could not tell a stall from ordinary progress.
    fn set_stalled(&mut self, stalled: bool) {
        for sink in &self.attached {
            sink.stalled.send_if_modified(|current| {
                let changed = *current != stalled;
                *current = stalled;
                changed
            });
        }
    }

    /// Say why, in the terminal and in the recording, then leave the table.
    async fn finish(mut self, reason: CloseReason) {
        let text = reason.text();
        if reason.is_worth_saying() {
            let line = format!("\r\n[gateway] {text}\r\n").into_bytes();
            if let Some(recorder) = &mut self.recorder {
                recorder.output(&line);
            }
            self.hand_over(ClientFrame::Data(line)).await;
        }
        if let Some(recorder) = &mut self.recorder {
            recorder.finish(&text);
        }
        let last = match &reason {
            CloseReason::Exited(code) => ClientFrame::Exit { code: *code },
            _ => ClientFrame::Error(text),
        };
        self.hand_over(last).await;
        self.table.remove(&self.id, &reason);
        // Dropping self drops the PtySession, which releases the provider's lease and,
        // for a local shell, reaps the child's whole subtree (ADR-008).
    }

    /// Best-effort delivery on the way out, to every socket still holding one. Bounded:
    /// the most likely reason a session is closing is that nobody is reading it.
    async fn hand_over(&mut self, frame: ClientFrame) {
        for sink in &self.attached {
            let _ = tokio::time::timeout(CLOSE_FLUSH, sink.frames.send(frame.clone())).await;
        }
    }
}

/// `sleep_until`, or never.
async fn deadline(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Resolves when EVERY attached socket has dropped its receiver — the earliest moment
/// the grace clock may start. Awaited one at a time: each completion lets the loop
/// re-prune the ones that went meanwhile, which is exact for the all-closed question
/// (a socket that drops while another still lives is pruned lazily on its next send —
/// and it does not matter, because the session is still honestly attached).
async fn all_sinks_closed(attached: &[AttachedSink]) {
    if attached.is_empty() {
        // Nobody to wait for — and resolving instantly here would spin the pump.
        std::future::pending::<()>().await;
    }
    for sink in attached {
        sink.frames.closed().await;
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
