/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The memory plugin's factory (SPEC §memory): descriptor, and the instance that
//! owns the service's lifetime - publish it into the state slot the /api/knowledge
//! routes read, mount the nine memory tools as the builtin `memory` MCP under
//! /mcp/memory, and take both back on stop.
//!
//! The mount follows the remote plugin's pattern exactly (SPEC §remote.mcp): a user
//! MCP owning the name wins, ours stays unmounted and the rest of the plugin is
//! unaffected. The OFFLINE CLI does not ride this lifecycle at all - it opens the
//! same store itself, stopped gateway or not (ADR-031's measured case).
//!
//! The models are the llm plugin's (SPEC §llm): the semantic tier follows its embedding
//! block - a changed block swaps in without a restart (the MCP keeps serving), and the
//! indexer task on the plugin's scope embeds what lacks a vector, nudged by every write
//! and retried on a slow tick - and a sync run asks the agent the row's one block,
//! `sync`, names (SPEC §memory.sync).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_core::log;
use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::RuntimeServices;
use swiss_llm::embedding::EmbeddingConfig;
use swiss_mcp::calls::CallLog;
use swiss_mcp::registry::{Registry, Source};
use swiss_memory::embed::INDEX_BATCH_DEFAULT;
use swiss_memory::service::MemoryService;
use swiss_memory::Embedder;

use crate::memory_api::{McpSeat, MemoryState};
use crate::memory_embed::LlmEmbedder;
use crate::memory_mcp::MemoryMcpAdapter;
use crate::memory_sync::{self, SyncConfig};

/// The plugin id, also the config row key and the routes' owner.
pub const PLUGIN_ID: &str = "memory";

/// The name the instance registers its builtin MCP under: /mcp/memory.
const MCP_NAME: &str = "memory";

/// How often the indexer looks again without a nudge: a provider that was down, or
/// a note a provider refused, is retried at this pace and never faster.
const INDEX_TICK: Duration = Duration::from_secs(300);

pub struct MemoryPlugin {
    state: Arc<MemoryState>,
    registry: Arc<Registry>,
    call_log: Arc<CallLog>,
    /// The shared services (SPEC §host.plugins): the sync runner's spawns seat
    /// their children rows here (SPEC §process.children).
    services: Arc<RuntimeServices>,
}

impl MemoryPlugin {
    pub fn new(state: Arc<MemoryState>, registry: Arc<Registry>, call_log: Arc<CallLog>, services: Arc<RuntimeServices>) -> Self {
        MemoryPlugin {
            state,
            registry,
            call_log,
            services,
        }
    }
}

/// The row checked whole: `enabled` and the `sync` block. The blocks that moved to the
/// llm plugin are refused with where they went, never silently dropped.
fn check(config: &Value) -> Result<Option<SyncConfig>, String> {
    let obj = config
        .as_object()
        .ok_or_else(|| "memory config must be an object".to_string())?;
    if let Some(key) = obj.keys().find(|k| matches!(k.as_str(), "embedding" | "llm")) {
        return Err(format!(
            "memory.{key} moved to the llm plugin: configure it on the LLM pages (Providers, Embedding) and remove it here"
        ));
    }
    if let Some(key) = obj.keys().find(|k| !matches!(k.as_str(), "enabled" | "sync")) {
        return Err(format!("memory config has no {key:?} field"));
    }
    SyncConfig::from_plugin(config)
}

#[async_trait]
impl PluginFactory for MemoryPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "memory".into(),
            label: "Memory".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({
                "type": "object",
                "properties": {
                    "sync": {
                        "type": "object",
                        "description": "The sync runner (SPEC §memory.sync): what moves a project's map and notes forward - an agent the llm plugin defines (SPEC §llm.agents), named here.",
                        "properties": {
                            "agent": { "type": "string", "description": "An llm agents entry, or \"off\"." },
                            "lead": { "type": "string", "description": "An llm agents entry a map or full run's judgment goes to - the review of the top level and each domain's wrap-up (SPEC §memory.init Lead) - or \"off\"." },
                            "transcripts": { "type": "string", "enum": ["claude-code", "off"] },
                            "transcriptDirs": { "type": "array", "items": { "type": "string" } },
                            "auto": { "type": "boolean" },
                            "minIntervalSec": { "type": "integer", "minimum": 0, "maximum": 86400 }
                        },
                        "additionalProperties": false
                    }
                }
            }),
            pages: vec![PageDescriptor {
                id: "memory".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "Memory".into(),
                order: 80,
                path: "#memory".into(),
                entry: "/admin/js/views/memory.js".into(),
                sidebar: false,
                layout: "workspace",
            }],
            routes: vec!["/api/knowledge".into()],
            restart_on_config_change: false,
            // Notes, search and the MCP work without it; the semantic tier and sync runs
            // need its embedding block and agents.
            requires: vec![crate::llm::PLUGIN_ID.into()],
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        check(config).map(|_| ())
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        self.create_at(config, MemoryService::default_path()).await
    }
}

impl MemoryPlugin {
    /// Keep the store path injectable so lifecycle tests own their state.
    async fn create_at(&self, config: &Value, path: PathBuf) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(MemoryInstance {
            state: self.state.clone(),
            registry: self.registry.clone(),
            call_log: self.call_log.clone(),
            services: self.services.clone(),
            sync: check(config)?,
            path,
        }))
    }
}

struct MemoryInstance {
    state: Arc<MemoryState>,
    registry: Arc<Registry>,
    call_log: Arc<CallLog>,
    /// The shared services: the sync runner's children rows seat here for the
    /// life of every spawn (SPEC §process.children).
    services: Arc<RuntimeServices>,
    /// The `sync` block the instance was created with (a later row arrives through
    /// apply_config).
    sync: Option<SyncConfig>,
    path: PathBuf,
}

/// Put the embedder a block names into the service (None takes it out) and say so in
/// the state the admin API reads. The indexer is nudged: a new endpoint has every note
/// to embed. Err only when the client cannot be built or the width is too wide.
fn plug(state: &MemoryState, service: &MemoryService, config: Option<EmbeddingConfig>) -> Result<(), String> {
    // The llm handle is always there (this plugin requires llm); the STATE goes in,
    // not the store of the moment - memory plugs before llm boots and re-plugs on a
    // row change, and the embedder resolves the open store per call either way.
    let llm = state.llm();
    let embedder = match &config {
        Some(config) => Some(LlmEmbedder::build(config, Some(llm.clone()))?),
        None => None,
    };
    service.set_embedder(embedder.map(|e| Arc::new(e) as Arc<dyn Embedder>));
    state.set_embedding(config, None);
    service.changes().notify_one();
    Ok(())
}

/// [plug], and a block that cannot be plugged leaves search lexical and says why; it
/// never fails anything - notes, the MCP and the brief do not need it.
fn plug_or_say(state: &MemoryState, service: &MemoryService, config: Option<EmbeddingConfig>) -> Result<(), String> {
    plug(state, service, config.clone()).inspect_err(|err| {
        log::warn("memory embedding is off", Some(json!({ "err": err })));
        service.set_embedder(None);
        state.set_embedding(config, Some(err.clone()));
    })
}

/// Follow the llm row: every apply, start and stop of the llm plugin moves its
/// generation, and memory re-plugs only when the embedding block itself changed (or
/// the last one failed).
async fn follow_embedding(
    state: Arc<MemoryState>,
    service: Arc<MemoryService>,
    mut generation: tokio::sync::watch::Receiver<u64>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = cancel.changed() => return,
            moved = generation.changed() => {
                if moved.is_err() {
                    return;
                }
                let next = state.llm().embedding();
                let now = state.embedding();
                if now.config != next || now.error.is_some() {
                    let _ = plug_or_say(&state, &service, next);
                }
            }
        }
    }
}

/// Embed what lacks a vector, a batch at a time, until nothing is left or a run
/// fails; then wait for a write's nudge, the slow tick, or the stop. A failing
/// provider costs one request per tick, never a hot loop (its error is in the
/// status the panel shows).
async fn indexer(service: Arc<MemoryService>, mut cancel: tokio::sync::watch::Receiver<bool>) {
    loop {
        while service.embedder().is_some() && !*cancel.borrow() {
            match service.index_pending(INDEX_BATCH_DEFAULT).await {
                Ok(report) if report.error.is_none() && report.embedded > 0 && report.pending > 0 => continue,
                Ok(_) => break,
                Err(err) => {
                    log::warn("memory indexing stopped", Some(json!({ "err": err.message })));
                    break;
                }
            }
        }
        tokio::select! {
            _ = cancel.changed() => return,
            _ = service.changes().notified() => {}
            _ = tokio::time::sleep(INDEX_TICK) => {}
        }
    }
}

/// Whether the registry's "memory" entry is OURS (def type "memory"): a user MCP
/// that happens to carry the name must not be hijacked by a restart.
fn mcp_is_ours(registry: &Arc<Registry>) -> bool {
    registry
        .get(MCP_NAME)
        .and_then(|entry| entry.data.read().ok().map(|d| d.def.type_() == "memory"))
        .unwrap_or(false)
}

#[async_trait]
impl PluginInstance for MemoryInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        // One service per active lifetime. Construction opens nothing; re-enabling
        // gets fresh pools over the same persisted data after stop closed the old ones.
        let service = Arc::new(MemoryService::at(&self.path));
        service.set_lister(Some(Arc::new(crate::memory_preview::CheckoutLister)));
        self.state.install(service.clone());

        // Subscribe before reading, so a row the llm plugin applies in between is
        // followed, not missed; the llm plugin may start before or after this one.
        let generation = self.state.llm().watch();
        let _ = plug_or_say(&self.state, &service, self.state.llm().embedding());
        scope.spawn(follow_embedding(self.state.clone(), service.clone(), generation, scope.cancel_signal()));
        self.state.set_sync(self.sync.clone(), None);
        // The runner's spawns carry children rows while they live (SPEC
        // §process.children): seat the registry before the worker that spawns.
        memory_sync::set_children_seat(self.services.children.clone());
        scope.spawn(indexer(service.clone(), scope.cancel_signal()));
        // The daily review of the changed notes, for the projects whose settings name a
        // time of day: one settings read a minute, a queued run when the time comes.
        scope.spawn(memory_sync::review_clock(service.clone(), self.state.clone(), scope.cancel_signal()));
        // The sync queue's driver: one run at a time, woken by an enqueue. It costs
        // nothing while no block and no queue entry exist (it sleeps on the notify).
        scope.spawn(memory_sync::sync_worker(service, self.state.clone(), scope.cancel_signal()));

        // /mcp/memory: nine tools over the same service, every one taking the
        // caller's cwd (SPEC §memory.mcp). The adapter declares MetadataOnly, so
        // neither log keeps arguments or results.
        let adapter = Arc::new(MemoryMcpAdapter::new(
            self.state.clone(),
            self.call_log.clone(),
        ));
        let def = swiss_host::config::ServerDef(
            json!({
                "type": "memory",
                "description": "Builtin: project memory for code agents as nine tools (memory_brief, memory_search, memory_get, memory_domain, memory_save, memory_update, memory_mark, memory_link, memory_domain_save); every tool takes the caller's absolute cwd."
            })
            .as_object()
            .cloned()
            .expect("the literal above is an object"),
        );
        if self.registry.has(MCP_NAME) {
            if mcp_is_ours(&self.registry) {
                self.registry
                    .update_def(MCP_NAME, def, adapter, true)
                    .await
                    .map_err(|err| format!("memory mcp restart: {err}"))?;
                self.state.set_seat(McpSeat::Serving);
            } else {
                // The panel shows this state with the fix: rename the user MCP.
                self.state.set_seat(McpSeat::Conflicted);
                log::warn(
                    "the /mcp/memory path is taken by a user MCP; the builtin memory tools stay unmounted",
                    None,
                );
            }
        } else {
            self.registry
                .register(MCP_NAME, Source::Config, def, adapter)
                .map_err(|err| format!("memory mcp registration: {err}"))?;
            self.registry
                .start(MCP_NAME)
                .await
                .map_err(|err| format!("memory mcp start: {err}"))?;
            self.state.set_seat(McpSeat::Serving);
        }
        // This plugin's own files (SPEC §data.sqlite). The store is always the
        // one this instance was given; the code index sits beside it and is
        // listed only when that file already exists.
        self.services.module_dbs.publish(PLUGIN_ID, self.path.clone());
        let code = self.path.with_file_name(swiss_memory::code::CODE_FILE);
        if code.is_file() {
            self.services.module_dbs.publish(PLUGIN_ID, code);
        }
        Ok(())
    }

    async fn stop(&self) {
        self.services.module_dbs.withdraw(PLUGIN_ID);
        // Withdraw first so no request finds a service that is going away (the
        // seat goes to stopped with it). The
        // store itself is NOT deleted or vacuumed: the offline CLI and the next
        // start expect every record where it was.
        if let Some(service) = self.state.withdraw() {
            service.close().await;
        }
        // The MCP entry follows the plugin down - stopped, not deleted, exactly
        // like the remote plugin's. A user MCP holding the name is not ours to stop.
        if !mcp_is_ours(&self.registry) {
            return;
        }
        if let Err(err) = self.registry.stop(MCP_NAME).await {
            log::warn(
                "the memory mcp entry did not stop cleanly",
                Some(json!({ "err": err })),
            );
        }
    }

    /// Every config revision lands here (an edit to any plugin's row moves it): only a
    /// changed `sync` block does anything, and it never bounces the instance. The
    /// embedding block is the llm row's, followed by [follow_embedding].
    async fn apply_config(&self, config: &Value) -> ApplyOutcome {
        let sync = match check(config) {
            Ok(sync) => sync,
            Err(err) => return ApplyOutcome::Failed(err),
        };
        if self.state.current().is_none() {
            return ApplyOutcome::NotApplicable;
        }
        // A change lands at once and wakes the worker (a run that just became due, or
        // a config that just fixed a failing one).
        if self.state.sync_seat().config != sync {
            self.state.set_sync(sync, None);
            self.state.wake_sync();
        }
        ApplyOutcome::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry + call log pair that writes nowhere a test would mourn.
    fn test_registry() -> (Arc<Registry>, Arc<CallLog>) {
        let calls = Arc::new(CallLog::at(std::env::temp_dir().join(format!(
            "swiss-memoryplug-calls-{}",
            swiss_core::util::random_hex(8)
        ))));
        (Registry::new(15_000, calls.clone()), calls)
    }

    fn plugin() -> MemoryPlugin {
        let (registry, calls) = test_registry();
        MemoryPlugin::new(
            Arc::new(MemoryState::new()),
            registry,
            calls,
            RuntimeServices::new(swiss_host::services::BootInfo::default()),
        )
    }

    #[test]
    fn the_descriptor_carries_one_page_and_the_knowledge_routes() {
        let d = plugin().descriptor();
        assert_eq!(d.id, "memory");
        assert_eq!(d.pages.len(), 1);
        assert_eq!(d.pages[0].id, "memory");
        assert_eq!(d.pages[0].entry, "/admin/js/views/memory.js");
        assert_eq!(d.routes, vec!["/api/knowledge".to_string()]);
        assert_eq!(d.requires, vec!["llm".to_string()]);
    }

    #[test]
    fn the_row_holds_the_sync_block_and_says_where_the_rest_went() {
        let plugin = plugin();
        assert!(plugin.validate_config(&json!({ "enabled": true })).is_ok());
        assert!(plugin.validate_config(&json!({ "enabled": true, "path": "x" })).is_err());
        assert!(plugin.validate_config(&json!({ "sync": { "agent": "distiller", "transcripts": "off" } })).is_ok());
        assert!(plugin.validate_config(&json!({ "sync": { "agent": "distiller", "lead": "planner" } })).is_ok());
        for moved in ["embedding", "llm"] {
            let err = plugin.validate_config(&json!({ moved: {} })).expect_err("moved");
            assert!(err.contains("llm plugin"), "{err}");
        }
        let err = plugin.validate_config(&json!({ "sync": { "runner": "claude" } })).expect_err("runner moved");
        assert!(err.contains("sync.agent"), "{err}");
    }

    #[tokio::test]
    async fn stopping_memory_closes_its_store_before_the_scope_ends() {
        let dir = std::env::temp_dir().join(format!("swiss-memory-stop-{}", swiss_core::util::random_hex(8)));
        let plugin = plugin();
        let instance = plugin.create_at(&json!({}), dir.join("store.sqlite3")).await.expect("create");
        let mut scope = PluginScope::new(PLUGIN_ID);
        instance.clone().start(&mut scope).await.expect("start");
        let service = plugin.state.current().expect("serving");
        let store = service.store().await.expect("open").clone();
        let project = service.create_project(Some("Stop fixture"), None, false, None).await.expect("create");
        instance.stop().await;
        let released = store.read().await.is_err();
        scope.shutdown().await;
        assert!(plugin.state.current().is_none(), "the routes must be withdrawn");
        assert!(released, "stop must close the pool even while a caller retains the service");
        assert!(service.store().await.is_err(), "old callers cannot revive a disabled service");

        let instance = plugin.create_at(&json!({}), dir.join("store.sqlite3")).await.expect("recreate");
        let mut scope = PluginScope::new(PLUGIN_ID);
        instance.clone().start(&mut scope).await.expect("restart");
        let next = plugin.state.current().expect("serving again");
        assert!(!Arc::ptr_eq(&service, &next), "restart must own a fresh service");
        assert!(next.projects().await.expect("read").iter().any(|p| p.id == project.id), "notes and projects survive stop");
        instance.stop().await;
        scope.shutdown().await;
        std::fs::remove_dir_all(&dir).expect("release store handles");
    }

    #[tokio::test]
    async fn the_embedder_follows_the_llm_row() {
        let llm = swiss_llm::LlmState::new();
        let state = Arc::new(MemoryState::with_llm(llm.clone()));
        let service = Arc::new(MemoryService::at(
            &std::env::temp_dir().join(format!("swiss-memoryplug-{}", swiss_core::util::random_hex(8))).join("store.sqlite3"),
        ));
        let (stop, cancel) = tokio::sync::watch::channel(false);
        let follower = tokio::spawn(follow_embedding(state.clone(), service.clone(), llm.watch(), cancel));
        let row = |model: &str| {
            swiss_llm::LlmConfig::parse(&json!({
                "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": model, "kind": "embedding" }] } },
                "embedding": { "provider": "local", "model": model }
            }))
            .expect("row")
        };
        let settled = || async {
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
        };
        llm.apply(row("bge-m3"));
        settled().await;
        assert!(service.embedder().is_some_and(|e| e.id().starts_with("openai:bge-m3:")));
        llm.apply(row("nomic"));
        settled().await;
        assert_eq!(state.embedding().config.map(|c| c.model), Some("nomic".to_string()));
        llm.stop();
        settled().await;
        assert!(service.embedder().is_none(), "the llm plugin off takes the tier off");
        stop.send(true).expect("stop");
        follower.await.expect("the follower ends on the stop");
    }
}
