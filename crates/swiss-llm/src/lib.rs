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

//! The model hub (SPEC §llm): one home for every model the toolbox asks.
//!
//! - [provider] - named endpoints (a wire, a base URL, a key reference, a default
//!   model) and the chat client every ask goes through (genai's adapters over the one
//!   reqwest the tree links).
//! - [embedding] - the semantic tier's endpoint: a provider by name plus the
//!   embedding model, and the OpenAI-compatible `/embeddings` client.
//! - [agent] - named agents: who executes a tool-using task (swiss's own loop, a
//!   one-shot chat, a spawned Claude Code or pi), with its limits and whether it
//!   reports. The loop itself lives here; the tools belong to the consumer.
//! - [history] - the module-owned record of every round trip and run (`logs/llm.db`).
//! - [conversation] - the live Test's conversation lane: which run a send joins, and
//!   every guard that refuses it before a provider request.
//! - [telemetry] - the OTLP exporter: every agent run reported as Claude Code's own
//!   event vocabulary (`user_prompt`, `api_request`, `tool_result`, ...) over gRPC or
//!   HTTP, so a collector built for Claude Code (cc-otel) reads swiss's agents unchanged.
//! - [state] - the applied row the plugin owns and every consumer reads.
//!
//! Nothing here starts anything on its own or touches axum: the plugin wrapper and the
//! `/api/llm` routes live in the root crate, and a consumer (memory) reads the state.

pub mod agent;
pub mod config;
pub mod conversation;
pub mod embedding;
pub mod history;
pub mod provider;
pub mod state;
pub mod telemetry;

pub use config::LlmConfig;
pub use state::LlmState;
