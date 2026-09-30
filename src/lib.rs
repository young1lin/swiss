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

//! The composition crate: the app skeleton, the MCP admin API, the server bootstrap, the CLI
//! and daemon, and the built-in plugin instances that glue the subsystem crates together. This
//! is the ONLY crate allowed to see every crate — which is what "plugin crates never depend on
//! each other" (SPEC §host.plugins) buys: composition is a file, not a graph.

pub mod adminapi;
pub mod app;
pub mod autostart;
pub mod bootstrap;
pub mod builtin;
pub mod cli;
pub mod daemon;
pub mod mcp_link;
pub mod pidfile;
pub mod plugins;
pub mod port;
pub mod remote_cli;
pub mod server;
pub mod session;
pub mod skill_install;
pub mod subsystems;
pub mod update_check;

#[doc(inline)]
pub use swiss_host::reply;
