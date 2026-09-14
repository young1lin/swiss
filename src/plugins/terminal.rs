//! The terminal plugin's factory (docs/14 §9 T5): descriptor, config validation, and
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
use swiss_terminal::terminal::{LocalShells, TerminalConfig, TerminalSessions};

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
                                "description": "OFF by default (docs/14 §6.1): the gateway already runs as you, but this turns a loopback port into a shell entry — flip it on deliberately."
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
                        "description": "asciicast v2, output only, ~/.mcp-gateway/terminal/<sessionId>.cast, 8 MB cap."
                    }
                },
                "additionalProperties": false
            }),
            pages: vec![PageDescriptor {
                id: "terminal".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "Terminal".into(),
                // docs/14 §8: its own first-level group under the docs/13 rules,
                // before the plugins page's 1000.
                order: 70,
                path: "#terminal".into(),
                entry: "/admin/js/views/terminal.js".into(),
                // sidebar:true would park the shell's MCP list beside the terminal: the flag
                // means "this page renders INTO that list's layout", and only the mcps page
                // does (page-registry hides .sidebar for every page without it). docs/14's
                // sketch said true; the browser said otherwise.
                sidebar: false,
                // workspace (docs/13 D5): the terminal supplies ALL of its own chrome -
                // session tabs, target picker, open/expand controls, status footer - so the
                // shell draws no context bar over it and immersive folds straight to it.
                layout: "workspace",
            }],
            routes: vec!["/api/terminal".into()],
            // The allowlist and the local switch are read at start; a config PUT
            // restarting the instance is honest AND cheap — sessions close with a
            // visible reason and the panel reopens them.
            restart_on_config_change: true,
            // NOT ["ssh-shell"] (docs/14 §4): a local session needs no SSH, and an
            // unmet requirement would park the whole plugin in waitingDependency.
            // The targets route says honestly whether remote hosts are reachable.
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
            config,
        }))
    }
}

struct TerminalInstance {
    state: Arc<TerminalState>,
    /// The shell capability seat (docs/14 §4): provided by the tunnels plugin, read
    /// live per open — never captured, so a provider restart needs no terminal restart.
    shells: Arc<ShellRegistry>,
    config: TerminalConfig,
}

#[async_trait]
impl PluginInstance for TerminalInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // Recordings land in ~/.mcp-gateway/terminal/<sessionId>.cast. Created eagerly
        // so a session open never races a mkdir, and so a read-only data dir fails
        // HERE — on the inventory row — instead of on the first open.
        let dir = swiss_core::paths::data_path(&["terminal"]);
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        let sessions = TerminalSessions::new(
            self.config.clone(),
            Arc::clone(&self.shells),
            Arc::new(LocalShells::new()),
            dir,
        );
        self.state.install(sessions);
        Ok(())
    }

    async fn stop(&self) {
        // Withdraw first so no request finds a machine that is about to die, then close
        // every session — each client gets its close reason as a final frame, and the
        // drivers, PTYs and recordings follow the sessions down (T4's shutdown path).
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
        let schema =
            TerminalPlugin::new(RuntimeServices::new(), TerminalState::new())
                .descriptor()
                .config_schema;
        let defaults = TerminalConfig::default();
        let props = &schema["properties"];
        assert_eq!(props["local"]["properties"]["enabled"]["default"], json!(false));
        // docs/15 §2.1: the schema is the sheet's hint, so the default order it states must
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
        assert_eq!(props["stallSeconds"]["default"], json!(defaults.stall.as_secs()));
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
            TerminalPlugin::new(RuntimeServices::new(), TerminalState::new()).descriptor();
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
        let plugin = TerminalPlugin::new(RuntimeServices::new(), TerminalState::new());
        assert!(plugin.validate_config(&json!({})).is_ok());
        let err = plugin
            .validate_config(&json!({ "maxSessions": 0 }))
            .expect_err("refused");
        assert!(err.contains("maxSessions"), "{err}");
    }
}
