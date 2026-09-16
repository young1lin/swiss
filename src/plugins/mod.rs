/*
 * Copyright 2026 The swiss authors
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

//! Statically-linked plugins that are NOT part of the boot built-ins (docs/12 W4).
//!
//! The host and the built-ins live in src/host; everything here proves the other half
//! of the plugin story: a tool that reaches the panel, the action registry and Jobs
//! through the PUBLIC contracts only — descriptor, action, page — with no edits under
//! src/host and exactly one register line in server.rs per plugin.

pub mod terminal;
pub mod terminal_api;
