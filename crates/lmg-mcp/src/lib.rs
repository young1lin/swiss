//! The MCP subsystem: the registry, every adapter (echo/proc/http/rest/mysql/pg/redis),
//! the call log, traffic metering, def import and introspection. Talks to the rest of the
//! gateway only through lmg-host's contracts.

pub mod adapters;
pub mod calls;
pub mod introspect;
pub mod mcp_import;
pub mod paging;
pub mod registry;
pub mod traffic;
