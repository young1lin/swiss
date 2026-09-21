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

//! The tunnels plugin as a provider of interactive shells (docs/14 T2).
//!
//! This is the whole of the SSH side of the terminal feature. Everything above it — the
//! session machine, the WebSocket, the panel page — reaches a remote host through
//! `swiss_host::services::shell` and never links an SSH client, which is what keeps ADR-010
//! true while the two plugins cooperate.
//!
//! What the terminal deliberately does NOT get:
//!
//! - **A second host list.** Targets are the tunnels connections the user already
//!   configured, by their own ids. No second place to type a hostname, no second copy of
//!   a credential, no second host-key decision (docs/14 §1).
//! - **The credentials.** The provider opens the channel and hands back bytes. A password
//!   or a key passphrase never crosses into the consumer, and `${ENV_VAR}` refs expand at
//!   connect time in `ssh.rs` exactly as they do for a forwarding rule.
//! - **Its own reference counting.** Sessions take a reference through
//!   [`TunnelManager::open_shell`], so a terminal on a host that already has tunnels up
//!   shares their client — and closing the terminal does not close their tunnels.

use std::sync::Arc;

use async_trait::async_trait;

use super::manager::TunnelManager;
use swiss_host::services::shell::{
    PtySession, PtySize, SessionLedger, ShellError, ShellProvider, ShellTarget,
};

/// The plugin id this provider registers under, and the name that appears in a "who is
/// missing" message when it is not registered.
pub const PROVIDER_ID: &str = "tunnels";

/// The provider the Tunnels plugin registers on start and withdraws on stop.
pub struct TunnelShells {
    manager: Arc<TunnelManager>,
    /// One ledger per plugin INSTANCE: a stopped instance drains its own sessions, never
    /// the next one's. Same rule as the MCP plugin's catalog tracker.
    ledger: Arc<SessionLedger>,
}

impl TunnelShells {
    pub fn new(manager: Arc<TunnelManager>) -> Arc<Self> {
        Arc::new(TunnelShells {
            manager,
            ledger: SessionLedger::new(),
        })
    }

    /// The ledger the plugin's stop drains before it closes its connections.
    pub fn ledger(&self) -> &Arc<SessionLedger> {
        &self.ledger
    }
}

#[async_trait]
impl ShellProvider for TunnelShells {
    fn list(&self) -> Vec<ShellTarget> {
        self.manager
            .connections()
            .into_iter()
            .map(|def| ShellTarget {
                // The tunnels connection id IS the identity: two servers that merely share
                // host and port are two definitions, and merging them would silently open
                // a shell as the wrong user (docs/09 §3).
                id: def.id.clone(),
                label: def.name.clone(),
                host: def.host.clone(),
                port: def.port,
                username: def.username.clone(),
                state: self.manager.conn_state(&def.id).as_str().to_string(),
            })
            .collect()
    }

    async fn open(&self, id: &str, holder: &str, size: PtySize) -> Result<PtySession, ShellError> {
        if self.ledger.is_withdrawing() {
            return Err(ShellError::Withdrawing(format!(
                "the {PROVIDER_ID} plugin is stopping; no new shell sessions"
            )));
        }
        // Refuse an unknown id BEFORE minting a lease, so a typo never shows up in the
        // "closing over N sessions" count.
        if !self.list().iter().any(|t| t.id == id) {
            return Err(ShellError::Unknown(format!(
                "no ssh connection named '{id}' — the {PROVIDER_ID} plugin knows nothing about it"
            )));
        }
        let lease = self.ledger.grant(id, holder);
        let session_id = swiss_core::util::random_hex(8);
        let (session, endpoint) = PtySession::duplex(session_id, id, size, lease);
        self.manager
            .open_shell(id, size, endpoint)
            .await
            .map_err(|err| match err.kind {
                // A host key that no longer matches, or credentials that do not work, is
                // the user's to fix on the Tunnels page — say which page, since the
                // terminal has no control of its own for it.
                super::types::FailureKind::Auth | super::types::FailureKind::HostKey => {
                    ShellError::Failed(format!("{} (fix it on the Tunnels page)", err.message))
                }
                super::types::FailureKind::Config => ShellError::Unknown(err.message.clone()),
                _ => ShellError::Unavailable(err.message.clone()),
            })?;
        Ok(session)
    }
}
