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

//! The applied row, shared: the llm plugin writes it on start and on every config apply,
//! a consumer reads it at the moment it needs a model. The composition root builds one
//! [LlmState] and hands it to both, so a consumer crate never depends on this plugin's
//! wrapper - only on this crate.
//!
//! A consumer that caches something derived from the row (memory's embedder, keyed on the
//! endpoint's fingerprint) watches [LlmState::watch]: the generation bumps on every apply
//! and every start or stop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::watch;

use crate::agent::{AgentConfig, Runtime};
use crate::config::LlmConfig;
use crate::embedding::EmbeddingConfig;
use crate::history::LlmHistory;
use crate::telemetry::{Reporter, Session};

pub struct LlmState {
    applied: RwLock<Arc<LlmConfig>>,
    serving: AtomicBool,
    generation: watch::Sender<u64>,
    reporter: Arc<Reporter>,
    /// The module history store (SPEC llm.history), installed by the plugin start and
    /// withdrawn before its bounded close. None means calls and runs record nothing -
    /// a disabled plugin keeps no database seat open.
    history: RwLock<Option<Arc<LlmHistory>>>,
    /// The LIVE conversations (SPEC llm.agents Test): one seat per agent whose
    /// stream is in flight. A seat exists only while its stream drives the loop -
    /// the cancel route reads it, the loop observes it, every terminal path
    /// releases it.
    seats: RwLock<HashMap<String, Seat>>,
}

/// One in-flight conversation.
struct Seat {
    run_id: i64,
    cancel: Arc<crate::history::CancelFlag>,
    /// What the reader sends while it runs (the steer route posts, the loop takes).
    steer: Arc<crate::history::Steer>,
}

impl LlmState {
    pub fn new() -> Arc<LlmState> {
        Arc::new(LlmState {
            applied: RwLock::new(Arc::new(LlmConfig::default())),
            serving: AtomicBool::new(false),
            generation: watch::channel(0).0,
            reporter: Reporter::new(),
            history: RwLock::new(None),
            seats: RwLock::new(HashMap::new()),
        })
    }

    /// The plugin opened its history store: every chat and embedding built from now on
    /// can attach it.
    pub fn install_history(&self, history: Arc<LlmHistory>) {
        match self.history.write() {
            Ok(mut slot) => *slot = Some(history),
            // A poisoned lock means a panic held it mid-write; silent no-op would
            // make the whole session look recorded while nothing is.
            Err(_) => swiss_core::log::warn("the llm history lock is poisoned; calls and runs will not record", None),
        }
    }

    /// Withdraw the store before the bounded close: calls built after this record
    /// nothing (the writer queue is gone by the time they would).
    pub fn withdraw_history(&self) {
        match self.history.write() {
            Ok(mut slot) => *slot = None,
            Err(_) => swiss_core::log::warn("the llm history lock is poisoned on withdraw; the store may outlive its close", None),
        }
    }

    /// The store while the plugin serves one, for consumers building a chat or an
    /// embedder.
    pub fn history(&self) -> Option<Arc<LlmHistory>> {
        match self.history.read() {
            Ok(slot) => slot.clone(),
            Err(_) => {
                swiss_core::log::warn("the llm history lock is poisoned; this consumer records nothing", None);
                None
            }
        }
    }

    /// Take the seat of a live conversation - ONE in-flight stream per agent
    /// (a second while one runs is a busy 409). Ok(false) = the agent's seat is
    /// taken; the caller refuses before touching the provider.
    pub fn seat(
        &self,
        agent: &str,
        run_id: i64,
        cancel: Arc<crate::history::CancelFlag>,
        steer: Arc<crate::history::Steer>,
    ) -> bool {
        match self.seats.write() {
            Ok(mut seats) => {
                if seats.contains_key(agent) {
                    return false;
                }
                seats.insert(agent.to_string(), Seat { run_id, cancel, steer });
                true
            }
            // A poisoned lock means a panic held it mid-write; refusing is the
            // honest answer (the run could never be cancelled).
            Err(_) => {
                swiss_core::log::warn("the llm seat lock is poisoned; this run cannot be cancelled", None);
                false
            }
        }
    }

    /// Release the agent's seat on a terminal path (done, failed, cancelled,
    /// dropped) - a released run is idle: cancel answers known-but-idle.
    pub fn unseat(&self, agent: &str) {
        if let Ok(mut seats) = self.seats.write() {
            seats.remove(agent);
        }
    }

    /// The flag of a run in flight, when it is.
    pub fn seat_of(&self, run_id: i64) -> Option<Arc<crate::history::CancelFlag>> {
        self.seats
            .read()
            .ok()
            .and_then(|seats| seats.values().find(|seat| seat.run_id == run_id).map(|seat| seat.cancel.clone()))
    }

    /// The inbox of a run in flight, when it is.
    pub fn steer_of(&self, run_id: i64) -> Option<Arc<crate::history::Steer>> {
        self.seats
            .read()
            .ok()
            .and_then(|seats| seats.values().find(|seat| seat.run_id == run_id).map(|seat| seat.steer.clone()))
    }

    /// The run id currently in flight for an agent (the busy guard's answer).
    pub fn seat_run_of(&self, agent: &str) -> Option<i64> {
        self.seats.read().ok().and_then(|seats| seats.get(agent).map(|seat| seat.run_id))
    }

    /// The plugin is stopping: request every live conversation's cancel and drop
    /// the seats. The loops file their honest tails on their own paths.
    pub fn cancel_all_conversations(&self) {
        if let Ok(mut seats) = self.seats.write() {
            for seat in seats.values() {
                seat.cancel.request();
            }
            seats.clear();
        }
    }

    /// Request every live conversation's cancel WITHOUT dropping the seats:
    /// the stop path asks, then watches the seats empty as each drive files
    /// its honest tail and unseats itself - the observation point a clear
    /// would destroy.
    pub fn request_all_cancels(&self) {
        if let Ok(seats) = self.seats.read() {
            for seat in seats.values() {
                seat.cancel.request();
            }
        }
    }

    /// Whether every live conversation has reached a terminal path (no seat
    /// held) - the stop path's bounded settle.
    pub fn seats_idle(&self) -> bool {
        self.seats.read().map(|seats| seats.is_empty()).unwrap_or(true)
    }

    /// The plugin started or took a new row.
    pub fn apply(&self, config: LlmConfig) {
        self.reporter.configure(config.telemetry.as_ref());
        if let Ok(mut applied) = self.applied.write() {
            *applied = Arc::new(config);
        }
        self.serving.store(true, Ordering::SeqCst);
        self.bump();
    }

    /// The plugin stopped: consumers see it off. The row stays readable (the panel's
    /// status still shows what was configured); what is queued for export waits for the
    /// next start.
    pub fn stop(&self) {
        self.serving.store(false, Ordering::SeqCst);
        self.bump();
    }

    fn bump(&self) {
        self.generation.send_modify(|g| *g += 1);
    }

    pub fn serving(&self) -> bool {
        self.serving.load(Ordering::SeqCst)
    }

    /// The applied row (the last one, even while stopped).
    pub fn config(&self) -> Arc<LlmConfig> {
        self.applied.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Bumps on every apply, start and stop.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    pub fn reporter(&self) -> &Arc<Reporter> {
        &self.reporter
    }

    /// The embedding endpoint while the plugin serves one.
    pub fn embedding(&self) -> Option<EmbeddingConfig> {
        if !self.serving() {
            return None;
        }
        self.config().embedding.clone()
    }

    /// A named agent, or the sentence a consumer shows when it cannot have one.
    pub fn agent(&self, name: &str) -> Result<AgentConfig, String> {
        if !self.serving() {
            return Err("the llm plugin is off: enable it to run agents".into());
        }
        self.config()
            .agent(name)
            .cloned()
            .ok_or_else(|| format!("the llm plugin has no agent named {name:?}; add it on the Agents page"))
    }

    /// The agent added most recently ([LlmConfig::newest_agent]) while the plugin serves:
    /// what a consumer that names none falls back to.
    pub fn newest_agent(&self) -> Option<String> {
        if !self.serving() {
            return None;
        }
        self.config().newest_agent.clone()
    }

    /// The reporting session for one run of `agent` on behalf of `consumer` (say
    /// "memory.sync"). Disabled unless telemetry is on and the agent reports - and always
    /// for a spawned runtime: a Claude Code child reports itself (see
    /// [AgentConfig::child_env]), and pi cannot.
    pub fn session(&self, agent: &AgentConfig, consumer: &str) -> Session {
        if !self.serving() || !agent.report || matches!(agent.runtime, Runtime::Claude | Runtime::Pi) {
            return Session::disabled();
        }
        self.reporter.session(&agent.name, consumer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> LlmConfig {
        LlmConfig::parse(&json!({
            "providers": { "ollama": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1",
                                       "models": [{ "id": "qwen3.5:4b" }, { "id": "bge-m3", "kind": "embedding" }] } },
            "embedding": { "provider": "ollama", "model": "bge-m3" },
            "agents": {
                "local": { "provider": "ollama" },
                "quiet": { "provider": "ollama", "report": false },
                "cc": { "runtime": "claude" },
                "data": { "runtime": "db", "provider": "ollama" }
            },
            "telemetry": { "protocol": "grpc", "endpoint": "http://127.0.0.1:4317" }
        }))
        .expect("row")
    }

    #[test]
    fn consumers_see_the_row_only_while_it_serves() {
        let state = LlmState::new();
        let watch = state.watch();
        assert!(state.embedding().is_none());
        assert!(state.agent("local").expect_err("off").contains("off"));
        assert_eq!(state.newest_agent(), None);
        state.apply(row());
        assert_eq!(*watch.borrow(), 1);
        // The row's last task agent, not the name sorted last: an add appends, and the Data
        // page's agent listed after it is no task's fallback.
        assert_eq!(state.newest_agent().as_deref(), Some("cc"));
        assert_eq!(state.agent("data").expect("data").runtime, crate::agent::Runtime::Db);
        assert_eq!(state.embedding().expect("embedding").model, "bge-m3");
        assert!(state.agent("nope").expect_err("missing").contains("no agent named"));
        let local = state.agent("local").expect("local");
        assert!(state.session(&local, "memory.sync").reporting());
        assert!(!state.session(&state.agent("quiet").expect("quiet"), "memory.sync").reporting());
        assert!(!state.session(&state.agent("cc").expect("cc"), "memory.sync").reporting(), "a child reports itself");
        state.stop();
        assert_eq!(*watch.borrow(), 2);
        assert!(state.embedding().is_none() && !state.session(&local, "memory.sync").reporting());
        assert_eq!(state.newest_agent(), None, "a stopped plugin has no agent to fall back to");
        assert_eq!(state.config().agents.len(), 4, "the row stays readable while stopped");
    }
}
