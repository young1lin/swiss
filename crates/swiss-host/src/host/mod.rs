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

//! The plugin host — P1 of docs/09: a small in-process host for statically linked built-in
//! plugins, with descriptors, page contributions, config rows in the ConfigStore, an explicit
//! lifecycle, scoped task cleanup, and one live route boundary.
//!
//! What the host IS (and the only things allowed in here — docs/09 §3):
//! - the plugin state machine ([`PluginState`]) with serialized start/stop per plugin and
//!   single-flight starts ([`PluginHost::reconcile`]);
//! - per-plugin scoped resources ([`PluginScope`]): every task a plugin spawns is tracked,
//!   cancelled and aborted on stop, so a stopped plugin provably leaves nothing running;
//! - the inventory ([`PluginHost::inventory`]) and the `/api/plugins` management surface
//!   ([`api`]);
//! - the route boundary ([`api::plugin_boundary`]): paths stay mounted, each request asks the
//!   host whether the owning plugin is serving — a disabled plugin's instance was dropped by
//!   its stop, not parked behind an `enabled` bool.
//!
//! What it is NOT: any business logic. The built-in plugins (MCP+Traffic, Tunnels, Data, Jobs)
//! and their wrappers around existing subsystems live in the composition crate's `builtin`
//! module — the table of this build, next to the wiring that constructs the subsystems. Adding
//! a plugin is one factory + one [`PluginHost::register`] line.

pub mod api;
pub mod descriptor;
pub mod engine;
pub mod factory;
pub mod scope;

pub use descriptor::{PageDescriptor, PluginDescriptor, PluginState};
pub use engine::PluginHost;
pub use factory::{PluginFactory, PluginInstance};
pub use scope::PluginScope;
