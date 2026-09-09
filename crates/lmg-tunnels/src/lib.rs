//! The tunnels subsystem: the SSH rule/connection manager and its /api/tunnels routes.

pub mod tunnel;
pub use tunnel::manager::TunnelLinks;
pub use tunnel::{McpDisplay, McpView, TunnelError, Tunnels};
