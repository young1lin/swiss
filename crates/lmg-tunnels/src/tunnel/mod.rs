//! The SSH-tunnel subsystem — port of the Node build's `src/tunnels/` (the LIVING SPEC).
//!
//! What lives here:
//!
//!  - `types`   — the domain model and the exact JSON shapes tunnels.json and the API speak
//!    (camelCase, absent-not-null, Node's field order).
//!  - `store`   — validation + persistence to tunnels.json, a sealed envelope like every state
//!    file. `${ENV_VAR}` refs in credentials stay refs on disk and expand at
//!    connect time (`ssh.rs`).
//!  - `port`    — local-port occupancy (probe / wait-for-release), the netstat+tasklist owner
//!    lookup, and Force free.
//!  - `ssh`     — one refcounted russh client per SSH connection: TOFU host keys, the banner,
//!    single-flight dial, channel opens, and the transport-death watcher.
//!  - `forward` — one rule's local listener: accept, cap, pipe socket <-> SSH channel, and the
//!    close-destroys-everything-then-verify-release contract.
//!  - `manager` — the live state machine: start/stop with per-rule serialization, reconnect
//!    backoff (network-only, capped, jittered), edits that restart what was running,
//!    and the MCP guard rail (Dependents -> 409 + confirm).
//!  - `mcpmatch`— THE ROUTER SEAM: maps an MCP definition's loopback target to a local port,
//!    which is how the panel suggests "this rule serves that MCP" and how the
//!    manager knows a rule has live dependents. Read-only on the registry.
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
pub mod import;
pub mod manager;
pub mod mcpmatch;
pub mod port;
pub mod ssh;
pub mod store;
pub mod types;

pub use api::{McpDisplay, Tunnels};
pub use manager::{McpView, OpError, OpResult, TunnelManager};
pub use store::TunnelStore;
pub use types::{FailureKind, RuleDef, RuleState, SshConnDef, TunnelError};
