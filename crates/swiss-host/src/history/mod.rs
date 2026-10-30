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

//! The storage machinery module history owners share (SPEC §arch.deps): one SQLite engine,
//! gated before any of them may open a database in WAL mode, and the owner-scoped store
//! primitives — a factory that builds one store per module without touching a file, a
//! bounded writer queue with generations and barriers, transactional versioned schema
//! steps, body chunk codecs and the authenticated chunk binding. The MCP and Remote
//! stores that consume these land with their own delivery slices.

pub mod body;
pub mod engine;
pub mod factory;
pub mod quota;
pub mod search;
pub mod schema;
pub mod seal;
pub mod store;
