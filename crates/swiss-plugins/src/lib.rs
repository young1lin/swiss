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

//! Statically-linked plugins that are NOT part of the boot built-ins (SPEC §host.plugins).
//!
//! The host and the boot built-ins live in the swiss-host crate and src/builtin.rs
//! (register_all); everything here proves the other half of the plugin story: a tool
//! that reaches the panel, the action registry and Jobs through the PUBLIC contracts
//! only — descriptor, action, page — with no edits inside the host crate and exactly
//! one register line in server.rs per plugin.

pub mod analysis;
pub mod analysis_api;
mod db_consoles;
mod llm_db_analysis;
pub mod llm;
pub mod llm_api;
pub mod llm_tools;
mod llm_db;
mod llm_db_memory;
mod llm_search;
mod llm_test_tools;
mod llm_secrets;
mod llm_shell;
pub mod memory;
pub mod memory_agent;
pub mod memory_api;
pub mod memory_ask;
pub mod memory_embed;
#[cfg(test)]
mod memory_live;
pub mod memory_mcp;
pub mod memory_pool;
pub mod memory_preview;
pub mod memory_sync;
pub mod process;
pub mod remote;
pub mod terminal;
pub mod terminal_api;
