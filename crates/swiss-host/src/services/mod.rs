//! Shared runtime services — the capabilities the plugins execute THROUGH, owned by no
//! plugin (docs/09 §3 "lazy shared capabilities", docs/10 §2).
//!
//! Three layers, deliberately stacked so that each one is usable without the one above it:
//!
//! - [`process`]: the shared supervisor. One place owns how this gateway spawns a child,
//!   captures its output under a hard byte cap, and tears its whole tree down.
//! - [`action`]: the Action contract and its registry — a name -> capability map with
//!   per-action input validation and a watch-backed cancel handle. Adding a capability is
//!   registering an impl; no scheduler grows a match arm.
//! - [`actions`]: this build's process capabilities (`process.exec`, and the tokenizer-
//!   compatible `process.legacy-command`), registered over the supervisor.
//! - [`runs`]: the shared run registry and coordinator — bounded, owner-scoped and
//!   first-wins, so scheduled runs and manual runs share one accounting without either
//!   owning the other.
//! - [`catalog`] and [`shell`]: the two typed capability seats. One provider registers,
//!   consumers take leases, and a provider stopping withdraws before it closes — the
//!   pattern that lets two plugins cooperate without a crate edge between them.
//!
//! Nothing here knows about Jobs, MCP or the panel: the consumers are plugins.

pub mod action;
pub mod actions;
pub mod api;
pub mod catalog;
pub mod process;
pub mod runs;
pub mod shell;

use std::sync::Arc;

use crate::services::action::ActionRegistry;
use crate::services::catalog::CatalogRegistry;
use crate::services::process::Supervisor;
use crate::services::runs::RunCoordinator;
use crate::services::shell::ShellRegistry;

/// The shared services, constructed ONCE by the composition root and handed to the
/// plugins that contribute to (or execute through) them. Not an AppContext under another
/// name: it holds no business state, only the registries a capability provider registers
/// into and the pool every producer's runs are accounted in.
pub struct RuntimeServices {
    /// Capability id -> impl. Providers register on start and withdraw on stop.
    pub actions: Arc<ActionRegistry>,
    /// The bounded, owner-scoped run pool shared by the scheduler and manual runs.
    pub runs: Arc<RunCoordinator>,
    /// The one owner of child-process spawning, capture and subtree teardown.
    pub supervisor: Arc<Supervisor>,
    /// The typed connection catalog (docs/12 W3): the provider (MCP) registers on start;
    /// consumers (Data) take request-scoped leases. Constructed here so it OUTLIVES every
    /// plugin instance — a provider stopping and starting again finds the same seat.
    pub catalog: Arc<CatalogRegistry>,
    /// The interactive shell capability (docs/14 §4): the provider (Tunnels) registers on
    /// start; the consumer (Terminal) takes a session-scoped lease per open PTY. Same
    /// reason for living here as the catalog — the seat outlives both plugins.
    pub shells: Arc<ShellRegistry>,
}

impl RuntimeServices {
    pub fn new() -> Arc<Self> {
        let actions = Arc::new(ActionRegistry::new());
        Arc::new(RuntimeServices {
            runs: RunCoordinator::new(actions.clone()),
            actions,
            supervisor: Supervisor::new(),
            catalog: Arc::new(CatalogRegistry::new()),
            shells: Arc::new(ShellRegistry::new()),
        })
    }

    /// Cancel and await every run, whoever produced it — the gateway's own teardown, after
    /// the plugins have stopped. Returns how many runs were still in flight.
    pub async fn shutdown(&self) -> usize {
        self.runs.shutdown_all().await
    }
}
