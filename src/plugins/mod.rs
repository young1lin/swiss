//! Statically-linked plugins that are NOT part of the boot built-ins (docs/12 W4).
//!
//! The host and the built-ins live in src/host; everything here proves the other half
//! of the plugin story: a tool that reaches the panel, the action registry and Jobs
//! through the PUBLIC contracts only — descriptor, action, page — with no edits under
//! src/host and exactly one register line in server.rs per plugin.

pub mod terminal;
pub mod terminal_api;
