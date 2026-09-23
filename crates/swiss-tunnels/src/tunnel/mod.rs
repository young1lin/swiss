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

//! The SSH-tunnel subsystem — port of the Node build's `src/tunnels/` (the LIVING SPEC).
//!
//! What lives here:
//!
//!  - `types`   — the domain model and the exact JSON shapes tunnels.json and the API speak
//!    (camelCase, absent-not-null, Node's field order).
//!  - `store`   — validation + persistence to tunnels.json, a sealed envelope like every state
//!    file. `${ENV_VAR}` refs in credentials stay refs on disk and expand at
//!    connect time (`ssh.rs`).
//!  - `port`    — local-port occupancy (probe / wait-for-release), the direct-Win32 owner
//!    lookup, and Force free.
//!  - `proxy`  — the HTTP CONNECT / SOCKS5 dialer a proxied connection dials through
//!    before its SSH handshake (docs/27 §2): credentials reach the handshake as
//!    components, never as a URL string.
//!  - `ssh`     — one refcounted russh client per SSH connection: TOFU host keys, the banner,
//!    single-flight dial, channel opens, the transport-death watcher, and the jump
//!    chains that dial through another connection's live session (docs/27 §3).
//!  - `forward` — one rule's local listener: accept, cap, pipe socket <-> SSH channel, and the
//!    close-destroys-everything-then-verify-release contract.
//!  - `manager` — the live state machine: start/stop with per-rule serialization, reconnect
//!    backoff (network-only, capped, jittered), edits that restart what was running,
//!    and the MCP guard rail (Dependents -> 409 + confirm).
//!  - `mcpmatch`— THE ROUTER SEAM: maps an MCP definition's loopback target to a local port,
//!    which is how the panel suggests "this rule serves that MCP" and how the
//!    manager knows a rule has live dependents. Read-only on the registry.
//!  - `shell`  — the interactive-shell PROVIDER (docs/14 T2): the tunnels connections seen as
//!    hosts a PTY can be opened on, handed to the host's shell capability so the
//!    terminal plugin never links an SSH client.
//!  - `import` — the one-shot forward-port config adoption on first run.
//!  - `api`    — the /api/tunnels admin routes, shape-identical to the Node build's.
//!
//! Boot order (index.ts): the store loads (adopting forward-port's config on the very first
//! run), the manager starts every rule marked `enabled`, and only then do MCPs register —
//! several MCPs connect through tunnels, but a failed tunnel never blocks an MCP. Shutdown is
//! `manager.close_all()` BEFORE the registry closes: it releases every local port and leaves
//! each rule's `enabled` flag alone so the next boot restores the same set.

pub mod api;
pub mod forward;
pub mod groups;
pub mod import;
pub mod manager;
pub mod mcpmatch;
pub mod port;
/// HTTP CONNECT / SOCKS5 dialing for proxied SSH connections (docs/27 §2).
pub mod proxy;
pub mod remote;
pub mod shell;
pub mod ssh;
#[cfg(test)]
pub(crate) mod sshtest;
pub mod store;
pub mod types;

pub use api::{McpDisplay, Tunnels};
pub use groups::register_tunnel_scopes;
pub use manager::{ConnectionLease, McpView, OpError, OpResult, ShellSessionGuard, TunnelManager};
pub use remote::TunnelRemote;
pub use shell::TunnelShells;
pub use store::TunnelStore;
pub use types::{FailureKind, RuleDef, RuleState, SshConnDef, TunnelError};
