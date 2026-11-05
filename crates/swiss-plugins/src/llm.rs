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

//! The llm plugin's factory (SPEC §llm): the models every other plugin asks, configured
//! once. Three pages - Providers, Embedding, Agents - over one config row the swiss-llm
//! crate checks whole; the instance publishes the applied row into the shared
//! [LlmState] the composition root hands to each consumer (memory today), and runs the
//! telemetry exporter on its scope.
//!
//! Every block applies in place: a consumer reads the row at the moment it asks, and one
//! that caches something derived from it (memory's embedder) watches the generation.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_llm::{LlmConfig, LlmState};

/// The plugin id, also the config row key, the routes' owner and the capability other
/// plugins require.
pub const PLUGIN_ID: &str = "llm";

pub struct LlmPlugin {
    consoles: Arc<crate::db_consoles::ConsoleStore>,
    /// The runtime services, for the maintenance seat the opened history needs
    /// (SPEC host.history): the retention timer only reaches stores that
    /// registered a seat, so the llm share's snapshot would never refresh
    /// without this handle.
    services: Arc<swiss_host::services::RuntimeServices>,
    state: Arc<LlmState>,
}

impl LlmPlugin {
    pub fn new(services: Arc<swiss_host::services::RuntimeServices>, state: Arc<LlmState>, consoles: Arc<crate::db_consoles::ConsoleStore>) -> Self {
        LlmPlugin { services, state, consoles }
    }
}

/// The swiss-llm check, plus memory's secrets scan over every value a credential name
/// does not already force into a reference: an agent's env and the collector's headers
/// reach a child or a wire, so a pasted token under an innocent name is refused too.
fn check(config: &Value) -> Result<LlmConfig, String> {
    let parsed = LlmConfig::parse(config)?;
    for agent in parsed.agents.values() {
        for (name, value) in &agent.env {
            if let Some(rule) = swiss_memory::secrets::scan(value) {
                return Err(format!(
                    "agents.{}.env {name}'s value looks like a credential ({rule}); put it in the vault and use a reference",
                    agent.name
                ));
            }
        }
    }
    if let Some(telemetry) = &parsed.telemetry {
        for (name, value) in &telemetry.headers {
            if let Some(rule) = swiss_memory::secrets::scan(value) {
                return Err(format!(
                    "telemetry.headers {name}'s value looks like a credential ({rule}); put it in the vault and use a reference"
                ));
            }
        }
    }
    Ok(parsed)
}

fn page(id: &str, label: &str, order: i64) -> PageDescriptor {
    PageDescriptor {
        id: id.into(),
        plugin_id: PLUGIN_ID.into(),
        label: label.into(),
        order,
        path: format!("#{id}"),
        entry: format!("/admin/js/views/{id}.js"),
        sidebar: false,
        layout: "page",
    }
}

#[async_trait]
impl PluginFactory for LlmPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        let reference = "A ${secret://name} or ${ENV_VAR} reference, never the key itself.";
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "llm".into(),
            label: "LLM".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({
                "type": "object",
                "properties": {
                    "providers": {
                        "type": "object",
                        "description": "Named endpoints (SPEC §llm.providers): each is spoken to once by name - by the embedding block and by agents.",
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "style": { "type": "string", "enum": ["openai", "anthropic"] },
                                "baseUrl": { "type": "string", "description": "e.g. http://127.0.0.1:11434/v1 (Ollama), https://open.bigmodel.cn/api/anthropic (GLM)" },
                                "apiKey": { "type": "string", "description": reference },
                                "models": {
                                    "type": "array",
                                    "description": "The models it serves; consumers pick from these. The first chat model is an agent's default, the first embedding model the embedding tier's. An anthropic-style provider lists chat models only.",
                                    "maxItems": 64,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": { "type": "string", "description": "The model's name on the wire, e.g. glm-5.3-flash, bge-m3." },
                                            "kind": { "type": "string", "enum": ["chat", "embedding"], "default": "chat" }
                                        },
                                        "required": ["id"],
                                        "additionalProperties": false
                                    }
                                },
                                "proxy": { "type": "string", "description": "An http://, https:// or socks5:// proxy for this provider only." }
                            },
                            "required": ["style", "baseUrl"],
                            "additionalProperties": false
                        }
                    },
                    "embedding": {
                        "type": "object",
                        "description": "The semantic tier (SPEC §llm.embedding): an openai-style provider's POST {baseUrl}/embeddings. Text is sent to that endpoint.",
                        "properties": {
                            "provider": { "type": "string", "description": "A providers entry, or \"off\"." },
                            "model": { "type": "string", "description": "One of the provider's embedding models; its first when unset." },
                            "dimensions": { "type": "integer", "minimum": 1 },
                            "batch": { "type": "integer", "minimum": 1 },
                            "timeoutMs": { "type": "integer", "minimum": 1000 },
                            "queryPrefix": { "type": "string" },
                            "documentPrefix": { "type": "string", "description": "Changing it re-embeds every stored vector." },
                            "minSimilarity": { "type": "number", "minimum": 0, "maximum": 1 }
                        },
                        "additionalProperties": false
                    },
                    "agents": {
                        "type": "object",
                        "description": "Named agents (SPEC §llm.agents): who executes a tool-using task. A consumer names one (memory's sync.agent).",
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "runtime": { "type": "string", "enum": ["builtin", "chat", "claude", "pi"] },
                                "provider": { "type": "string", "description": "builtin/chat: the providers entry it asks." },
                                "model": { "type": "string", "description": "builtin/chat: one of the provider's chat models, its first when unset. claude: --model. pi: provider/model, required." },
                                "maxTokens": { "type": "integer", "minimum": 256, "maximum": 131072 },
                                "maxContextKiB": { "type": "integer", "minimum": 64, "maximum": 8192, "description": "builtin/chat: the most one request carries, in KiB (1024 when unset); a conversation past it takes no further send." },
                                "effort": { "type": "string", "enum": ["low", "medium", "high", "max"], "description": "builtin/chat: how hard the model thinks before it answers; unset leaves the model's own default." },
                                "concurrency": { "type": "integer", "minimum": 1, "maximum": 16, "description": "How many of this agent's tasks one consumer runs at once (4 for builtin/chat, 1 for claude/pi when unset)." },
                                "command": { "type": "string", "description": "claude/pi: a bare name found on PATH, or an absolute path." },
                                "env": { "type": "object", "description": "claude/pi: extra env for the child; a credential-named value must be a reference." },
                                "report": { "type": "boolean", "description": "Whether this agent's runs are reported while telemetry is on." },
                                "systemPrompt": { "type": "string", "maxLength": 20000, "description": "builtin/chat: appended to the built-in default system message, which is always sent. claude/pi: appended to the child's own system prompt." },
                                "tools": {
                                    "type": "array",
                                    "description": "builtin: read, write, edit, glob, grep, bash, job_output, job_kill, sync, delegate (write and edit only in the panel's Test). claude: Read, Grep, Glob. pi: read, grep, find, ls. Of the tools a task offers, the ones this agent takes; unset takes them all, [] none. chat takes no tools.",
                                    "items": { "type": "string" },
                                    "uniqueItems": true
                                }
                            },
                            "additionalProperties": false
                        }
                    },
                    "telemetry": {
                        "type": "object",
                        "description": "Agent runs as OTLP logs in Claude Code's event vocabulary (SPEC §llm.telemetry), so a collector built for Claude Code (cc-otel) reads them unchanged.",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "protocol": { "type": "string", "enum": ["grpc", "http/protobuf", "http/json"] },
                            "endpoint": { "type": "string", "description": "The collector's base URL: cc-otel listens on http://127.0.0.1:4317 (grpc) and :4318 (http)." },
                            "headers": { "type": "object", "description": "Extra request headers; a credential-named one must be a reference." },
                            "serviceName": { "type": "string" },
                            "logPrompts": { "type": "boolean", "description": "Send prompt text, not only its length." }
                        },
                        "additionalProperties": false
                    }
                }
            }),
            // Four pages, the MCP plugin's shape, ordered after Memory's 80. The default
            // rail seats every served group up to its cap (SPEC §panel.nav), so this order
            // only decides LLM's place in the row, not whether it needs a pin.
            pages: vec![
                page("llm-providers", "Providers", 85),
                page("llm-embedding", "Embedding", 86),
                page("llm-agents", "Agents", 87),
                page("llm-history", "History", 88),
            ],
            // /api/analysis is the Data agent's ledger (SPEC §data.analysis): it is this
            // plugin's because every analysis is an agent's work, so disabling llm disables it.
            routes: vec!["/api/llm".into(), "/api/analysis".into()],
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        check(config).map(|_| ())
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        self.create_at(config, swiss_core::paths::data_path(&["logs"])).await
    }
}

impl LlmPlugin {
    /// The same construction with the logs root named - the remote plugin's L1
    /// pattern: create() names the real home; the L1 test hands a private one so
    /// its assert cannot be raced by the start test in the shared home.
    async fn create_at(
        &self,
        config: &Value,
        logs_dir: std::path::PathBuf,
    ) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(LlmInstance {
            services: self.services.clone(),
            state: self.state.clone(),
            config: check(config)?,
            logs_dir,
            history: tokio::sync::Mutex::new(None),
            consoles: self.consoles.clone(),
            console_handler: tokio::sync::Mutex::new(None),
        }))
    }
}

struct LlmInstance {
    consoles: Arc<crate::db_consoles::ConsoleStore>,
    // The serving instance owns the bus seat (SPEC §bus.requests).
    console_handler: tokio::sync::Mutex<Option<swiss_bus::Registration>>,
    services: Arc<swiss_host::services::RuntimeServices>,
    state: Arc<LlmState>,
    /// The row the instance was created with (a later row arrives through apply_config).
    config: LlmConfig,
    /// The home's logs root, where logs/llm.db lives. The database opens on start
    /// (never at construction, SPEC §host.history L1) and closes bounded on stop.
    logs_dir: std::path::PathBuf,
    /// The store this instance opened, held so stop can close it.
    history: tokio::sync::Mutex<Option<Arc<swiss_llm::history::LlmHistory>>>,
}

#[async_trait]
impl PluginInstance for LlmInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        crate::llm_shell::register(&self.services)?;
        *self.console_handler.lock().await = Some(self.consoles.register(self.services.clone()));
        self.state.apply(self.config.clone());
        // The module history (SPEC §llm.history): opens before anything can record, so
        // the first call of the session files too. A failed open is a warned, degraded
        // start - asking keeps working, nothing records (the terminal owner's rule).
        match swiss_llm::history::LlmHistory::open(&self.logs_dir).await {
            Ok(history) => {
                // The hourly maintenance seat (SPEC host.history): checkpoint,
                // conditional VACUUM, ledger reconciliation, usage-snapshot
                // refresh and the 180-day sweep (SPEC llm.history), run by the
                // server's retention timer while this database is open. The
                // refreshed snapshot is what keeps the share bound at runtime -
                // the one-shot measure at open goes stale with every write.
                {
                    let history = history.clone();
                    self.services.maintenance.register(
                        "llm",
                        Arc::new(move || {
                            let history = history.clone();
                            Box::pin(async move {
                                history.maintain(180 * 24 * 60 * 60 * 1000).await;
                            })
                        }),
                    );
                }
                self.state.install_history(history.clone());
                self.services
                    .module_dbs
                    .publish(PLUGIN_ID, self.logs_dir.join("llm.db"));
                // The Data page reads its sealed body parts through this store's own
                // keys (SPEC §data.sqlite "Decoded cells"); the withdraw in stop takes it back.
                self.services.module_dbs.set_decoder(
                    PLUGIN_ID,
                    self.logs_dir.join("llm.db"),
                    history.cell_decoder(),
                );
                *self.history.lock().await = Some(history);
            }
            Err(err) => {
                swiss_core::log::warn(
                    &format!("the llm history database did not open; calls and runs are not recorded: {err}"),
                    None,
                );
            }
        }
        // The exporter: idle (a sleep on its notify) while telemetry is off, one final
        // flush when the scope cancels.
        scope.spawn(self.state.reporter().clone().run(scope.cancel_signal()));
        Ok(())
    }

    async fn stop(&self) {
        self.console_handler.lock().await.take();
        self.state.stop();
        // Live conversations first (SPEC llm.agents Test): request every
        // cancel, then let the drives settle BOUNDED - each one files its
        // honest tail and releases its own seat while the store still admits
        // writes. Only then does the history withdraw and drain.
        self.state.request_all_cancels();
        // Reap queued/running jobs while their action still exists. Withdrawing it first
        // turns a just-submitted job into a spurious missing-action failure.
        self.services.runs.cancel_action_types(&[crate::llm_shell::ACTION]).await;
        self.services.actions.unregister(crate::llm_shell::ACTION);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !self.state.seats_idle() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        // Withdraw first: chats and embedders built after this record nothing, and the
        // writer queue drains before the pool releases (the bounded close, 10s).
        self.services.module_dbs.withdraw(PLUGIN_ID);
        self.state.withdraw_history();
        // The maintenance seat goes with the store (SPEC host.history): a stopped
        // module is reported skipped by name, never run against a closed pool.
        self.services.maintenance.unregister("llm");
        let history = self.history.lock().await.take();
        if let Some(history) = history {
            history.flush().await;
            // The closing count: what the record holds as it goes away (the ops
            // signal remote's stop logs for its own history).
            let (calls, runs, bytes) = history.usage().await;
            swiss_core::log::info(&format!(
                "llm.db closed: {calls} calls, {runs} runs, {bytes} bytes"
            ));
            if let Err(err) = history.close().await {
                swiss_core::log::warn(
                    &format!("the llm history database did not close within its bound: {err}"),
                    None,
                );
            }
        }
    }

    /// Every block applies in place: the state swaps the row and bumps its generation, and
    /// the exporter rebuilds its client on its next pass.
    async fn apply_config(&self, config: &Value) -> ApplyOutcome {
        match check(config) {
            Ok(parsed) => {
                if *self.state.config() != parsed || !self.state.serving() {
                    self.state.apply(parsed);
                }
                ApplyOutcome::Applied
            }
            Err(err) => ApplyOutcome::Failed(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A default runtime services for factory-level tests: the seats exist but
    /// nothing is started against them.
    fn test_services() -> Arc<swiss_host::services::RuntimeServices> {
        swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default())
    }

    fn test_plugin(services: Arc<swiss_host::services::RuntimeServices>, state: Arc<LlmState>) -> LlmPlugin {
        let root = std::env::temp_dir().join(format!("swiss-llm-consoles-{}", swiss_core::util::random_hex(8)));
        let consoles = crate::db_consoles::ConsoleStore::at(root, services.bus.clone());
        LlmPlugin::new(services, state, consoles)
    }

    #[test]
    fn the_descriptor_carries_four_pages_and_the_llm_routes() {
        let d = test_plugin(test_services(), LlmState::new()).descriptor();
        assert_eq!(d.id, "llm");
        let pages: Vec<&str> = d.pages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(pages, ["llm-providers", "llm-embedding", "llm-agents", "llm-history"]);
        assert!(d.pages.iter().all(|p| p.layout == "page" && p.entry == format!("/admin/js/views/{}.js", p.id)));
        assert_eq!(d.routes, vec!["/api/llm".to_string(), "/api/analysis".to_string()]);
    }

    #[test]
    fn a_pasted_credential_is_refused_under_any_name() {
        let plugin = test_plugin(test_services(), LlmState::new());
        assert!(plugin.validate_config(&json!({ "enabled": true })).is_ok());
        assert!(plugin.validate_config(&json!({ "oops": 1 })).is_err());
        let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let env = json!({ "agents": { "cc": { "runtime": "claude", "env": { "PLAIN": token } } } });
        let err = plugin.validate_config(&env).expect_err("a token under a plain name");
        assert!(err.contains("looks like a credential") && !err.contains(token), "{err}");
        let headers = json!({ "telemetry": { "endpoint": "http://127.0.0.1:4317", "headers": { "x-team": token } } });
        let err = plugin.validate_config(&headers).expect_err("a token in a header");
        assert!(err.contains("looks like a credential") && !err.contains(token), "{err}");
    }

    #[tokio::test]
    async fn a_constructed_instance_opens_no_history_database() {
        // SPEC §host.history L1, the module half: creating the instance (composition, a
        // disabled module) opens no database - the file appears only on start. The
        // logs root is injected so the assert cannot be raced by the start test that
        // opens the real shared-home path.
        let dir = std::env::temp_dir().join(format!(
            "swiss-llmplug-l1-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plugin = test_plugin(test_services(), LlmState::new());
        let instance = plugin
            .create_at(&json!({ "enabled": true }), dir.join("logs"))
            .await
            .expect("create");
        drop(instance);
        assert!(!dir.join("logs").exists(), "no logs tree before start");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stopping_the_real_plugin_unregisters_its_console_bus_handler() {
        use swiss_bus::pb::v1;
        let services = test_services();
        services.catalog.register(Arc::new(crate::db_consoles::bus_tests::Catalog), "test").expect("catalog");
        let plugin = test_plugin(services.clone(), LlmState::new());
        let root = std::env::temp_dir().join(format!("swiss-bus-stop-{}", swiss_core::util::random_hex(8)));
        let instance = plugin.create_at(&json!({}), root.join("logs")).await.expect("create");
        let mut scope = PluginScope::new(PLUGIN_ID);
        instance.clone().start(&mut scope).await.expect("start");
        let mut peer = crate::db_consoles::bus_tests::Peer::open(services.bus.clone()).await;
        assert!(peer.list("mm").await.consoles.is_empty());
        instance.stop().await;
        scope.shutdown().await;
        let after = peer.request(v1::ConsolesId::ConsolesList,
            v1::ConsolesPage { conn: "mm".into(), after: String::new() }).await;
        assert_eq!(after.fault.expect("stopped plugin").code, swiss_bus::UNAVAILABLE);
        std::fs::remove_dir_all(root).expect("scratch cleanup");
    }

    #[tokio::test]
    async fn a_row_applies_in_place_and_stop_takes_it_off_the_air() {
        // create() opens the history under the data dir's logs/, so the scratch home goes
        // in first: without it the start writes into the real ~/.swiss.
        swiss_core::paths::test_home();
        let state = LlmState::new();
        let services = test_services();
        let plugin = test_plugin(services.clone(), state.clone());
        let row = json!({
            "providers": { "ollama": { "style": "openai", "baseUrl": "http://127.0.0.1:11434/v1",
                                       "models": [{ "id": "qwen3.5:4b" }, { "id": "bge-m3", "kind": "embedding" }] } },
            "embedding": { "provider": "ollama", "model": "bge-m3" }
        });
        let instance = plugin.create(&row).await.expect("create");
        let mut scope = PluginScope::new(PLUGIN_ID);
        instance.clone().start(&mut scope).await.expect("start");
        assert!(services.actions.get(crate::llm_shell::ACTION).is_some());
        assert_eq!(state.embedding().expect("serving").model, "bge-m3");
        let next = json!({ "providers": row["providers"].clone() });
        assert!(matches!(instance.apply_config(&next).await, ApplyOutcome::Applied));
        assert!(state.embedding().is_none(), "the embedding block left the row");
        assert!(matches!(instance.apply_config(&json!({ "providers": 3 })).await, ApplyOutcome::Failed(_)));
        let shell = crate::llm_shell::ShellTools::new(&swiss_core::paths::test_home(), &["bash", "job_output"], &services,
            crate::llm_shell::scope("stop-proof", Some(42)), None);
        // echo and sleep mean the same in Bash and PowerShell.
        let started = shell.call(crate::llm_shell::TOOL, &json!({"command":"echo lifecycle; sleep 60","run_in_background":true})).await;
        assert!(started.ok, "{}", started.text);
        let started: Value = serde_json::from_str(&started.text).expect("job result");
        let id = started["job_id"].as_u64().expect("job id");
        instance.stop().await;
        scope.shutdown().await;
        assert!(!state.serving());
        assert!(services.actions.get(crate::llm_shell::ACTION).is_none());
        assert_eq!(services.runs.get(id).expect("retained job").state, swiss_host::services::runs::RunState::Canceled);
    }
}
