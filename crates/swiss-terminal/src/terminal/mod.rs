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

//! The terminal session machine (docs/14 T4).
//!
//! What lives here is everything that is true about a terminal session no matter how the
//! bytes reach a browser: the session table, its limits, the one-shot tickets, the idle
//! and stall clocks, the reconnect grace window with its catch-up buffer, and the
//! asciicast recorder. **No HTTP** — that is T5, and keeping the split means every rule in
//! docs/14 §6 is asserted against a fake shell provider and a fake local PTY rather than
//! against a live WebSocket.
//!
//! Two sources, one type. A remote session comes from the shell capability seat
//! ([`swiss_host::services::shell::ShellRegistry`], provided by the tunnels plugin) and a
//! local one from [`local::LocalShell`] over the ConPTY/openpty seam in `swiss-core`; both
//! hand back a [`swiss_host::services::shell::PtySession`], so the driver below has one code
//! path and no idea which kind of shell it is pumping.

pub mod config;
pub mod local;
pub mod recording;
pub mod session;
pub mod tickets;

pub use config::TerminalConfig;
pub use local::{LocalShell, LocalShells};
pub use session::{
    Attachment, ClientFrame, CloseReason, LocalView, Opened, RemoteView, SessionView, TargetsView,
    TerminalError, TerminalSessions,
};
pub use tickets::{TicketBook, TicketError, TICKET_TTL};
