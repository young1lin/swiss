//! The host layer: the plugin host's mechanism, the run/action services, the connection
//! catalog, the browser model, config + config-store, auth and token management. Knows no
//! subsystem by type — that is the whole point of the contracts.

pub mod auth;
pub mod config;
pub mod config_store;
pub mod dbbrowser;
pub mod host;
pub mod local_only;
pub mod managed;
pub mod mask;
pub mod mem;
pub mod pathenv;
pub mod proc_pids;
pub mod reply;
pub mod services;
pub mod token;
