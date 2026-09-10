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
//! ([`lmg_host::services::shell::ShellRegistry`], provided by the tunnels plugin) and a
//! local one from [`local::LocalShell`] over the ConPTY/openpty seam in `lmg-core`; both
//! hand back a [`lmg_host::services::shell::PtySession`], so the driver below has one code
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
