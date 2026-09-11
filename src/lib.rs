//! The composition crate: the app skeleton, the MCP admin API, the server bootstrap, the CLI
//! and daemon, and the built-in plugin instances that glue the subsystem crates together. This
//! is the ONLY crate allowed to see every crate — which is what "plugin crates never depend on
//! each other" (docs/09) buys: composition is a file, not a graph.

pub mod adminapi;
pub mod app;
pub mod bootstrap;
pub mod builtin;
pub mod cli;
pub mod daemon;
pub mod mcp_link;
pub mod pidfile;
pub mod plugins;
pub mod port;
pub mod server;
pub mod skill_install;
pub mod subsystems;

#[doc(inline)]
pub use swiss_host::reply;
