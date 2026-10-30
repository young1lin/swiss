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

//! The managed-apps half of the Process plugin (SPEC §process.apps).
//!
//! An app is an outside program the operator declares. The gateway reads its
//! status, starts, stops and restarts it, starts it when a daemon boots, shows
//! its logs and keeps a history of what was done - and NOTHING watches an app
//! between those moments (ADR-036). The children that die WITH the gateway are
//! the other half (SPEC §process.children) and live in swiss-host.
//!
//! Layering, all pure where it can be: [config] (schema and validation),
//! [status] (the verdict, a pure function over facts), [control] (the actions
//! over injected probe/spawner/killer seams), [history] and [logs] (the two
//! files an app produces), [api] (the /api/process routes behind a state slot
//! the plugin installs into).

pub mod api;
pub mod config;
pub mod control;
mod facts;
pub mod history;
pub mod logs;
pub mod status;
