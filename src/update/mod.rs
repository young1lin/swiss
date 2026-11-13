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

//! Self-update that runs only in a short-lived CLI process (ADR-041). The resident gateway
//! never downloads, never holds an exe and never patches.

pub mod apply;
pub mod check;
pub mod fetch;
pub mod manifest;
pub mod patch;
pub mod replace;
pub mod spawn;

#[cfg(test)]
mod tests;

pub use apply::{apply, ApplyEnv, Outcome};
pub use spawn::{real as real_spawner, InFlight, Spawned, UpdateSpawner};
pub use check::{channel, check, current_version, is_newer, GithubUpdateCheck, UpdateCheck, UpdateInfo};
