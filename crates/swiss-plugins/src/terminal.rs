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

//! The terminal plugin's factory (SPEC §terminal): descriptor, config validation, and
//! the instance that owns the session machine's lifetime.
//!
//! A plugin built purely on the public contracts: everything the host needs is said
//! here in data — descriptor, schema, routes — and the host never learns the id
//! "terminal". The instance's whole job is to build the [TerminalSessions] the routes
//! drive (T4's machine, over the shell capability seat and the local PTY seam) and to
//! tear it down on stop; every session-level rule already lives in swiss-terminal, tested
//! there without a server.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::shell::ShellRegistry;
use swiss_host::services::RuntimeServices;
use swiss_terminal::terminal::{LocalChildWiring, LocalShells, TerminalConfig, TerminalSessions};

use super::terminal_api::TerminalState;

/// The plugin id, also the config row key and the routes' owner.
pub const PLUGIN_ID: &str = "terminal";

pub struct TerminalPlugin {
    services: Arc<RuntimeServices>,
    /// The seat the instance publishes itself into; the routes read the same one.
    state: Arc<TerminalState>,
}

impl TerminalPlugin {
    pub fn new(services: Arc<RuntimeServices>, state: Arc<TerminalState>) -> Self {
        TerminalPlugin { services, state }
    }
}

#[async_trait]
impl PluginFactory for TerminalPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "terminal".into(),
            label: "Terminal".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            // Mirrors TerminalConfig::parse key for key — the panel's form is generated
            // from this, and the parser is the authority the numbers must match.
            config_schema: json!({
                "type": "object",
                "properties": {
                    "local": {
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "default": false,
                                "description": "OFF by default (SPEC §terminal.config): the gateway already runs as you, but this turns a loopback port into a shell entry — flip it on deliberately."
                            },
                            "shell": {
                                "type": "string",
                                "description": "Program to run instead of the platform's default shell (local sessions only). Empty = the default: pwsh.exe, then powershell.exe, then COMSPEC on Windows; $SHELL on unix."
                            }
                        },
                        "additionalProperties": false
                    },
                    "allowedTargets": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Empty = every tunnels connection. Otherwise an exact list of connection ids a terminal may be opened on."
                    },
                    "maxSessions": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 64,
                        "default": 4,
                        "description": "Remote sessions only; local sessions are uncapped."
                    },
                    "maxSessionsPerTarget": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 64,
                        "default": 2,
                        "description": "Remote sessions only; local sessions are uncapped."
                    },
                    "idleTimeoutMinutes": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 86400,
                        "default": 30,
                        "description": "0 = never idle out."
                    },
                    "graceSeconds": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 86400,
                        "default": 60,
                        "description": "How long a dropped socket may reconnect and catch up."
                    },
                    "stallSeconds": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 86400,
                        "default": 30,
                        "description": "How long a client that accepts no output is tolerated before the session closes."
                    },
                    "recording": {
                        "type": "boolean",
                        "default": true,
                        "description": "asciicast v2, output only, stored in the terminal history with a byte-faithful .cast export, 8 MB cap."
                    }
                },
                "additionalProperties": false
            }),
            pages: vec![PageDescriptor {
                id: "terminal".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "Terminal".into(),
                // SPEC §terminal.api: its own first-level group under the SPEC §panel.nav rules,
                // before the plugins page's 1000.
                order: 70,
                path: "#terminal".into(),
                entry: "/admin/js/views/terminal.js".into(),
                // sidebar:true would park the shell's MCP list beside the terminal: the flag
                // means "this page renders INTO that list's layout", and only the mcps page
                // does (page-registry hides .sidebar for every page without it). SPEC §terminal's
                // sketch said true; the browser said otherwise.
                sidebar: false,
                // workspace (SPEC §panel.nav, as revised): a full-bleed, dense body. The
                // terminal's session tabs, target picker and status footer are L3 page-
                // local chrome INSIDE that body; the shell still draws the context bar
                // above it, and immersive folds straight to the body.
                layout: "workspace",
            }],
            routes: vec!["/api/terminal".into()],
            // The allowlist and the local switch are read at start; a config PUT
            // restarting the instance is honest AND cheap — sessions close with a
            // visible reason and the panel reopens them.
            restart_on_config_change: true,
            // NOT ["ssh-shell"] (SPEC §terminal.remote): a local session needs no SSH — `requires`
            // expresses a functional dependency, not a hard gate. WaitingDependency was
            // never implemented (SPEC §host.plugins): an unmet require is a plain refusal
            // naming the dependency, and the targets route already says honestly whether
            // remote hosts are reachable.
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        TerminalConfig::parse(config).map(|_| ())
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        let config = TerminalConfig::parse(config)?;
        Ok(Arc::new(TerminalInstance {
            state: self.state.clone(),
            shells: Arc::clone(&self.services.shells),
            children: Arc::clone(&self.services.children),
            config,
            services: self.services.clone(),
            history: tokio::sync::Mutex::new(None),
        }))
    }
}

struct TerminalInstance {
    state: Arc<TerminalState>,
    /// The shell capability seat (SPEC §terminal.remote): provided by the tunnels plugin, read
    /// live per open — never captured, so a provider restart needs no terminal restart.
    shells: Arc<ShellRegistry>,
    /// The Process page's rows (SPEC §process.children): every local PTY registers here.
    children: Arc<swiss_host::services::children::ChildRegistry>,
    config: TerminalConfig,
    services: Arc<RuntimeServices>,
    /// The history database this instance opened on start — held so stop can close the
    /// very pool it opened, not whatever another instance later opened.
    history: tokio::sync::Mutex<Option<Arc<swiss_terminal::terminal::history::TerminalHistory>>>,
}

#[async_trait]
impl PluginInstance for TerminalInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // The sessions directory is created eagerly so a session open never races a
        // mkdir; it is also the legacy .cast tree the store imports from (ADR-035).
        let dir = swiss_core::paths::data_path(&[swiss_terminal::terminal::history::SESSIONS_DIR]);
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        let logs = swiss_core::paths::data_path(&["logs"]);
        // The history database opens FIRST (SPEC §host.history): boot-gated, migrated,
        // legacy-imported before any session can record. A failed open degrades to
        // recording-off with one warning — a storage decision is a log line, never a
        // failed start (the module-history rule, verbatim).
        let history = match swiss_terminal::terminal::history::TerminalHistory::open(&dir, &logs).await {
            Ok(history) => {
                let _ = self.services.history_query.register(
                    swiss_host::services::history_query::HistoryQueryAdapter {
                        owner: "terminal".into(),
                        db_file: logs.join("terminal.db"),
                        source: history.clone(),
                        bodies: Some(Arc::new(
                            swiss_terminal::terminal::history::TerminalBodyReader::new(history.clone()),
                        )),
                    },
                );
                // The Data page reads the sealed recording parts through this store
                // (SPEC §data.sqlite "Decoded cells"); unregister takes it back.
                self.services.module_dbs.set_decoder(
                    "terminal",
                    logs.join("terminal.db"),
                    history.cell_decoder(),
                );
                {
                    // The hourly maintenance seat (SPEC §host.history): checkpoint,
                    // conditional VACUUM, ledger reconciliation and the 90-day sweep
                    // (§terminal.recording), run by the server's retention timer while
                    // this database is open.
                    let history = history.clone();
                    self.services.maintenance.register(
                        "terminal",
                        Arc::new(move || {
                            let history = history.clone();
                            Box::pin(async move {
                                history.maintain(90 * 24 * 60 * 60 * 1000).await;
                            })
                        }),
                    );
                }
                Some(history)
            }
            Err(err) => {
                swiss_core::log::warn(
                    "terminal history database unavailable; recording is off for new sessions",
                    Some(json!({ "err": err })),
                );
                None
            }
        };
        // The child-row wiring (SPEC §process.children): the provider registers each PTY,
        // and a row's stop closes that session through the state's live machine — the
        // session's own teardown path, never a kill underneath the tab.
        let state = Arc::clone(&self.state);
        let wiring = LocalChildWiring::new(
            Arc::clone(&self.children),
            Arc::new(move |session_id| {
                let state = Arc::clone(&state);
                let id = session_id.to_string();
                swiss_host::services::children::child_stop_fn(move || {
                    let state = Arc::clone(&state);
                    let id = id.clone();
                    async move {
                        let sessions = state.current().ok_or_else(|| {
                            "the terminal plugin is not serving; the tab is already gone"
                                .to_string()
                        })?;
                        sessions.close(&id).await.map_err(|err| err.to_string())
                    }
                })
            }),
        );
        let sessions = TerminalSessions::new(
            self.config.clone(),
            Arc::clone(&self.shells),
            Arc::new(LocalShells::with_wiring(wiring)),
            history.clone(),
        );
        self.state.install(sessions);
        if let Some(history) = &history {
            self.state.install_history(history.clone());
        }
        *self.history.lock().await = history;
        Ok(())
    }

    async fn stop(&self) {
        // Withdraw first so no request finds a machine that is about to die, then close
        // every session — each client gets its close reason as a final frame, and the
        // drivers, PTYs and recordings follow the sessions down (T4's shutdown path).
        // Sessions close BEFORE the store: every recorder's final part and finish stamp
        // lands before the pool that takes them drains.
        if let Some(sessions) = self.state.withdraw() {
            let closed = sessions.shutdown().await;
            if closed > 0 {
                swiss_core::log::log(
                    "info",
                    "the terminal plugin stopped with live sessions",
                    Some(json!({ "sessions": closed })),
                );
            }
        }
        // The export route's store goes first: no request may reach a pool that is
        // about to close (the same withdraw-before-close rule the sessions follow).
        let route_history = self.state.withdraw_history();
        // The query and maintenance seats go with the store (SPEC §host.history): a
        // stopped module is reported skipped by name, never auto-opened by a query.
        self.services.history_query.unregister("terminal");
        self.services.maintenance.unregister("terminal");
        drop(route_history);
        let history = self.history.lock().await.take();
        if let Some(history) = history {
            // A barrier BEFORE the close: the drain inside close() counts failed
            // jobs as completed and returns Ok, so a tail write that failed at
            // EXECUTION would otherwise die unlogged with the process - flush()
            // is the one caller that turns that sticky failure into a warning.
            history.flush().await;
            if let Err(err) = history.close().await {
                swiss_core::log::warn(
                    "terminal history database close incomplete",
                    Some(json!({ "err": err.to_string() })),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_numbers_match_the_parser_defaults() {
        // The form is generated from the schema; the limits are enforced by the parser.
        // If these drift apart the panel offers a default the backend rejects (or
        // silently substitutes), which is exactly the bug this test exists to catch.
        let schema = TerminalPlugin::new(RuntimeServices::new(swiss_host::services::BootInfo::default()), TerminalState::new())
            .descriptor()
            .config_schema;
        let defaults = TerminalConfig::default();
        let props = &schema["properties"];
        assert_eq!(
            props["local"]["properties"]["enabled"]["default"],
            json!(false)
        );
        // SPEC §terminal.local: the schema is the sheet's hint, so the default order it states must
        // name the same shells conpty's probe prefers.
        let shell_desc = props["local"]["properties"]["shell"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(shell_desc.contains("pwsh.exe"), "{shell_desc}");
        assert!(shell_desc.contains("COMSPEC"), "{shell_desc}");
        assert_eq!(
            props["maxSessions"]["default"],
            json!(defaults.max_sessions)
        );
        assert_eq!(
            props["maxSessionsPerTarget"]["default"],
            json!(defaults.max_sessions_per_target)
        );
        assert_eq!(
            props["idleTimeoutMinutes"]["default"],
            json!(defaults.idle_timeout.as_secs() / 60)
        );
        assert_eq!(
            props["graceSeconds"]["default"],
            json!(defaults.grace.as_secs())
        );
        assert_eq!(
            props["stallSeconds"]["default"],
            json!(defaults.stall.as_secs())
        );
        assert_eq!(props["recording"]["default"], json!(defaults.recording));
        // Every key the parser accepts is in the schema, and nothing else.
        let mut schema_keys: Vec<&str> = props
            .as_object()
            .expect("properties is an object")
            .keys()
            .map(String::as_str)
            .collect();
        schema_keys.sort_unstable();
        assert_eq!(
            schema_keys,
            [
                "allowedTargets",
                "graceSeconds",
                "idleTimeoutMinutes",
                "local",
                "maxSessions",
                "maxSessionsPerTarget",
                "recording",
                "stallSeconds"
            ]
        );
    }

    #[test]
    fn the_descriptor_claims_exactly_the_terminal_routes() {
        let descriptor =
            TerminalPlugin::new(RuntimeServices::new(swiss_host::services::BootInfo::default()), TerminalState::new()).descriptor();
        assert_eq!(descriptor.id, "terminal");
        assert_eq!(descriptor.routes, vec!["/api/terminal".to_string()]);
        assert!(descriptor.requires.is_empty(), "not requires: [ssh-shell]");
        assert!(descriptor.restart_on_config_change);
        assert_eq!(descriptor.pages.len(), 1);
        assert_eq!(descriptor.pages[0].order, 70);
        assert_eq!(descriptor.pages[0].entry, "/admin/js/views/terminal.js");
        // The MCP sidebar is the mcps page's chrome; leaving this true renders the hosted
        // server list beside a terminal (the bug the first browser run caught).
        assert!(!descriptor.pages[0].sidebar);
    }

    #[tokio::test]
    async fn validate_rejects_what_the_parser_rejects() {
        let plugin = TerminalPlugin::new(RuntimeServices::new(swiss_host::services::BootInfo::default()), TerminalState::new());
        assert!(plugin.validate_config(&json!({})).is_ok());
        let err = plugin
            .validate_config(&json!({ "maxSessions": 0 }))
            .expect_err("refused");
        assert!(err.contains("maxSessions"), "{err}");
    }
}
