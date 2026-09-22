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

//! swiss-it - the docs/44 integration-test harness: real MySQL, PostgreSQL and Redis
//! for the suites that need them, at zero compiled cost for the machines that do not
//! have Docker.
//!
//! Everything in this crate hides behind the `it` feature (default off). Without it the
//! lib, the test binary and (from docs/44 I6) the stdio server are empty files,
//! `cargo test --workspace` stays green anywhere, and nothing this crate would have
//! pulled enters the build. With it the second gate opens -
//! `cargo test -p swiss-it --features it` - which needs a Docker endpoint
//! (`DOCKER_HOST`) or one of the three `SWISS_IT_*_URL` escapes, and treats their
//! absence as a failure, never a skip.
//!
//! Nothing in the workspace depends on this crate; it is a leaf and dev-only by
//! construction (docs/44 §2.1, ADR-028). The shipping dependency graph is therefore
//! byte-for-byte what it was before - `cargo tree -e normal,build -p swiss` is the
//! proof, recorded in the I0 commit.

#![cfg(feature = "it")]

pub mod engine;
mod exit;
pub mod seed;
