//! The Rust port of `local-mcp-gateway` (the Node original at `../local-mcp-gateway` is the
//! reference implementation). The module layout mirrors docs/02.

pub mod adapters;
pub mod admin;
pub mod adminapi;
pub mod app;
pub mod atomic_json;
pub mod auth;
pub mod bootstrap;
pub mod calls;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod dbbrowser;
pub mod dbbrowser_api;
pub mod introspect;
pub mod local_only;
pub mod log;
pub mod managed;
pub mod mask;
pub mod mcp_import;
pub mod mem;
pub mod paging;
pub mod pathenv;
pub mod paths;
pub mod pidfile;
pub mod platform;
pub mod port;
pub mod proc_pids;
pub mod registry;
pub mod secure;
pub mod server;
pub mod skill_install;
pub mod token;
pub mod traffic;
pub mod tunnel;
pub mod util;
