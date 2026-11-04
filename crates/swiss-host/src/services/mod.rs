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

//! Shared runtime services — the capabilities the plugins execute THROUGH, owned by no
//! plugin (SPEC §host.plugins "lazy shared capabilities", SPEC §jobs).
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
//! - [`catalog`], [`shell`] and [`remote`]: the three typed capability seats. One
//!   provider registers, consumers take leases, and a provider stopping withdraws before
//!   it closes — the pattern that lets two plugins cooperate without a crate edge between
//!   them.
//!
//! Nothing here knows about Jobs, MCP or the panel: the consumers are plugins.

pub mod action;
pub mod actions;
pub mod api;
pub mod catalog;
pub mod children;
pub mod detached;
pub mod history_query;
pub mod maintenance;
pub mod module_db;
pub mod process;
pub mod remote;
pub mod runs;
pub mod shell;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::action::ActionRegistry;
use crate::services::catalog::CatalogRegistry;
use crate::services::children::ChildRegistry;
use crate::services::process::Supervisor;
use crate::services::remote::RemoteTransportRegistry;
use crate::services::runs::RunCoordinator;
use crate::services::shell::ShellRegistry;

/// What the boot knows about this instance (SPEC §process.apps): the port it serves, and
/// whether a detaching `swiss start` spawned it — read from the SWISS_SPAWNER attribution
/// before the gateway scrubs that variable, because nothing after boot may need to know
/// how the gateway was launched. "Start with swiss" (autostart) runs on a daemon only.
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    pub port: u16,
    pub is_daemon: bool,
}

impl Default for BootInfo {
    /// The honest default for an embedded caller that never read the spawner attribution:
    /// a foreground instance on an unknown port, exactly `swiss serve`'s shape.
    fn default() -> Self {
        BootInfo {
            port: 0,
            is_daemon: false,
        }
    }
}

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
    /// The typed connection catalog (SPEC §host.seats): the provider (MCP) registers on start;
    /// consumers (Data) take request-scoped leases. Constructed here so it OUTLIVES every
    /// plugin instance — a provider stopping and starting again finds the same seat.
    pub catalog: Arc<CatalogRegistry>,
    /// The interactive shell capability (SPEC §terminal.remote): the provider (Tunnels) registers on
    /// start; the consumer (Terminal) takes a session-scoped lease per open PTY. Same
    /// reason for living here as the catalog — the seat outlives both plugins.
    pub shells: Arc<ShellRegistry>,
    /// The remote-execution transport capability (SPEC §remote): the provider (Tunnels)
    /// registers on start; the consumer (Remote) takes run-scoped leases per exec or
    /// file operation. Same seat-outlives-plugins reason as the two above.
    pub remote: Arc<RemoteTransportRegistry>,
    /// The one list of the gateway's own children (SPEC §process.children): every run,
    /// proc MCP and local terminal registers here, and the memory reading takes its roots
    /// from here. Replaces the supervisor's private pid set.
    pub children: Arc<ChildRegistry>,
    /// What the boot knew about this instance (SPEC §process.apps): the port, and whether
    /// a detaching start spawned it.
    pub boot: BootInfo,
    /// The cross-module history query seat (SPEC §host.history): modules with an
    /// open history database register on start, withdraw on stop. The seat
    /// outlives plugin instances like the catalog and shell seats.
    pub history_query: Arc<history_query::HistoryQuerySeat>,
    /// Module databases the Data page browses (SPEC §data.sqlite). The same seat
    /// the history-query registration enrolls into, so a plugin publishes its own
    /// file and no other plugin is handed the path.
    pub module_dbs: Arc<module_db::ModuleDbSeat>,
    /// The hourly maintenance seat (SPEC §host.history): a module with an open
    /// history database registers its maintenance hook on start and withdraws on
    /// stop; the server's retention timer runs them each hour. Same
    /// seat-outlives-plugins reason as the two above.
    pub maintenance: Arc<maintenance::MaintenanceSeat>,
    /// The boot autostart pass (SPEC §process.apps) runs once per GATEWAY
    /// process. The process plugin's first start takes it; every later start —
    /// a disable/enable cycle is not a reboot — finds it spent. It lives here,
    /// not on the plugin instance, because enable rebuilds the instance on the
    /// same services set.
    boot_pass_taken: AtomicBool,
    /// The event bus (SPEC §bus). One hub for the process; plugins register handlers
    /// on start and drop them on stop. The stamp is injected so a test bus carries
    /// an empty one and the gateway carries the panel's.
    pub bus: Arc<swiss_bus::Bus>,
}

impl RuntimeServices {
    pub fn new(boot: BootInfo) -> Arc<Self> {
        Self::with_stamp(boot, Arc::new(String::new))
    }

    /// `stamp` is what WELCOME and BUILD carry (SPEC §bus.link). The composition passes
    /// the panel stamp; tests keep [`new`](Self::new)'s empty one.
    pub fn with_stamp(boot: BootInfo, stamp: swiss_bus::Stamp) -> Arc<Self> {
        let actions = Arc::new(ActionRegistry::new());
        let children = ChildRegistry::new();
        let module_dbs = Arc::new(module_db::ModuleDbSeat::new());
        Arc::new(RuntimeServices {
            runs: RunCoordinator::new(actions.clone()),
            actions,
            supervisor: Supervisor::new(Arc::clone(&children)),
            catalog: Arc::new(CatalogRegistry::new()),
            shells: Arc::new(ShellRegistry::new()),
            remote: Arc::new(RemoteTransportRegistry::new()),
            children,
            boot,
            history_query: Arc::new(history_query::HistoryQuerySeat::sharing(module_dbs.clone())),
            module_dbs,
            maintenance: Arc::new(maintenance::MaintenanceSeat::new()),
            boot_pass_taken: AtomicBool::new(false),
            bus: swiss_bus::Bus::new(stamp),
        })
    }

    /// Tell every bus link the process is stopping, then cancel and await every run.
    /// Returns how many runs were still in flight. The bus wait returns as soon as
    /// the links have gone (SPEC §bus.link).
    pub async fn shutdown(&self) -> usize {
        self.bus.shutdown().await;
        self.runs.shutdown_all().await
    }

    /// Take the once-per-gateway-process boot autostart pass (SPEC §process.apps):
    /// true when this call is the one that wins it, false when an earlier start
    /// already ran it — "Enabling the plugin later or applying config never
    /// starts one either".
    pub fn take_boot_pass(&self) -> bool {
        !self.boot_pass_taken.swap(true, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_pass_is_won_once_per_services_set() {
        let services = RuntimeServices::new(BootInfo::default());
        assert!(services.take_boot_pass(), "the first start wins the pass");
        assert!(!services.take_boot_pass(), "a disable/enable cycle is not a reboot");
        assert!(!services.take_boot_pass(), "and neither is another one");
    }
}
