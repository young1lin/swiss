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

//! The ONE test binary of the harness (docs/44 §2.1). Deliberately a single binary:
//! the engines' OnceCells live in the process, and one tests/*.rs file per suite would
//! mean five MySQL cold starts. Splits live in modules under this file, not in
//! additional binaries.
//!
//! Empty without the feature: `cargo test --workspace` on a machine without Docker
//! compiles this to nothing and stays green exactly as before.

#![cfg(feature = "it")]

mod mysql;
mod pg;
mod seed;
mod smoke;
