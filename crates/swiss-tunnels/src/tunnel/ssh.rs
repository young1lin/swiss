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

//! One SSH client per connection, refcounted by the manager — port of `tunnels/ssh.ts`, with
//! russh in ssh2's role.
//!
//! Refcounted rather than one client per rule: "Start all" over the 14 rules on one host must
//! dial SSH once, not fourteen times. `connect()` is single-flight — N rules starting together
//! share one handshake.
//!
//! `${ENV}` refs expand HERE, at connect time — the same build-time step the MCP adapters do.
//! tunnels.json keeps the reference on disk, and only the live connection ever sees the secret;
//! a literal password keeps working exactly as before. A ref whose env var is unset resolves to
//! "" and fails as `auth` on the first try — visible, never retried.
//!
//! A jump reference (SPEC §tunnels.jump) rides the jump's live session instead of a socket: the
//! transport is a direct-tcpip channel opened on it and the handshake runs over that stream
//! (SSH-over-SSH, OpenSSH -J's model). Each hop's client is an ordinary client — the chain
//! assembles recursively through the manager's connection table, one hold per hop.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use russh::client::{Handle, Handler, Msg};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::forward::ByteStream;
use super::types::{ConnState, FailureKind, SshConnDef, TunnelError};
use swiss_core::log;
use swiss_core::paths::home_dir;
use swiss_core::secure::refs;
use swiss_host::services::shell::{PtyEndpoint, PtyEvent, PtyIn, PtyInput, PtyOut, PtySize};

/// ssh2's readyTimeout: TCP + kex + auth inside one 15s budget.
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// A tunnel idle long enough for a NAT or firewall to drop it silently is the failure this
/// whole feature exists to handle: without keepalive the transport looks alive forever while
/// carrying nothing, and the local port stays bound. 15s x 3 detects it in ~45s.
const KEEPALIVE: Duration = Duration::from_secs(15);
const KEEPALIVE_MAX: usize = 3;
/// SPEC §tunnels.selfheal: a direct-tcpip open that no answer reaches within this budget
/// fails the caller's socket. A wedged session accepts TCP, answers keepalives and never
/// confirms a channel open — without this budget the socket hangs until the client's own
/// pool timeout, and nothing at the transport layer ever sees a failure. The data path
/// after a successful open keeps no timeout: an idle pooled socket is legitimate.
const OPEN_BUDGET: Duration = Duration::from_secs(10);
/// SPEC §tunnels.selfheal: this many consecutive unanswered opens on one connection judge
/// the session wedged and report it once. Any answered open — success or refusal — resets
/// the count, so an unreachable target (whose opens time out alone) never tears down its
/// healthy neighbours.
const WEDGE_OPENS: usize = 3;
/// A wedged transport must not block a shutdown (ssh.end()'s own cap).
const END_TIMEOUT: Duration = Duration::from_millis(1500);
/// What the far side is told it is talking to. xterm.js reports itself as an xterm with
/// 256 colours, and telling the remote anything else means a wrong terminfo entry and a
/// display that redraws incorrectly.
const TERM: &str = "xterm-256color";
/// How long to wait for a server's want_reply answer to the pty and shell requests. A
/// server that has accepted a session channel and then says nothing is wedged; hanging
/// the open forever would leave the panel spinning with no way to find out why.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Expand a leading `~` in a key path. The panel's own default is `~/.ssh/id_rsa`.
pub fn expand_home(p: &str) -> String {
    let t = p.trim();
    if t == "~" {
        return home_dir().to_string_lossy().to_string();
    }
    if let Some(rest) = t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\")) {
        return home_dir()
            .join(rest)
            .to_string_lossy()
            .replace('/', std::path::MAIN_SEPARATOR_STR);
    }
    t.to_string()
}

/// OpenSSH's fingerprint format: `SHA256:` + unpadded base64 of the key's SHA-256, over the
/// same public-key wire blob ssh2 hashed.
pub fn fingerprint(host_key: &[u8]) -> String {
    let digest = Sha256::digest(host_key);
    let mut b64 = base64::engine::general_purpose::STANDARD.encode(digest);
    while b64.ends_with('=') {
        b64.pop();
    }
    format!("SHA256:{b64}")
}

/// Map a russh failure message onto a FailureKind, because the kind decides whether a retry
/// loop is allowed to run at all: retrying bad credentials every 10 seconds earns a fail2ban
/// ban, so `auth` must never be retried. Hand-rolled case-insensitive scans (ADR-007) — the
/// Node build matched ssh2's message strings; russh's are ours to read directly.
fn classify_message(message: &str) -> FailureKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("auth") {
        return FailureKind::Auth;
    }
    // "host key" mismatches are normally intercepted by the fingerprint check before any russh
    // error is raised; if one leaks through, it still must not be retried.
    if lower.contains("host key") || lower.contains("hostkey") {
        return FailureKind::HostKey;
    }
    FailureKind::Network
}

/// Turn an unknown error into a TunnelError, preserving an already-classified one.
pub fn as_tunnel_error(message: impl std::fmt::Display, prefix: Option<&str>) -> TunnelError {
    let msg = message.to_string();
    let full = match prefix {
        Some(p) => format!("{p}: {msg}"),
        None => msg,
    };
    let kind = classify_message(&full);
    TunnelError::new(full, kind)
}

/// Shared callback invoked when a server presents a host key.
pub type HostKeyCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// SPEC §tunnels.jump: resolve one jump hop. Given the jump connection's id and this
/// connection's host:port, produce the byte stream the caller's SSH handshake rides on
/// (a direct-tcpip channel through the jump's live session), together with the hold that
/// keeps the jump's client alive for exactly as long as the caller's session lives.
pub type JumpDialer = Arc<
    dyn Fn(
            &str,
            &str,
            u16,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<(ByteStream, Box<dyn Send + 'static>), TunnelError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// Resolves a jump id to its stored definition — the read-only slice of the store the
/// throwaway Test chain needs (SPEC §tunnels.jump).
pub type JumpDefs = Arc<dyn Fn(&str) -> Option<SshConnDef> + Send + Sync>;

/// One jump dial's boxed future: the channel stream the caller's handshake rides on,
/// plus the hold keeping the hop alive for the session's lifetime.
type JumpResolved = Pin<
    Box<dyn Future<Output = Result<(ByteStream, Box<dyn Send + 'static>), TunnelError>> + Send>,
>;

/// Callbacks the manager registers on a live connection.
#[derive(Default)]
pub struct SshHooks {
    /// A fingerprint was learned (first connect) — persist it.
    pub on_host_key: Option<HostKeyCallback>,
    /// The transport died while connected. The manager releases ports and decides about retrying.
    pub on_lost: Option<Arc<dyn Fn(TunnelError) + Send + Sync>>,
    /// SPEC §tunnels.selfheal: channel opens stopped being answered while the transport
    /// still answers keepalives. The manager ends this client and runs the ordinary loss
    /// path — same fan-out as on_lost, reached from a judgment keepalive cannot make.
    pub on_wedged: Option<Arc<dyn Fn() + Send + Sync>>,
    /// SPEC §tunnels.jump: dial through a jump. Attached by the manager (the shared chain) or
    /// by the Test path (a throwaway chain); absent on direct and proxied connections.
    pub jump: Option<JumpDialer>,
    /// The connection's state moved (idle → connecting → connected, or error). The manager
    /// forwards it to the remote seat's changed() (SPEC §host.seats), so a page that shows
    /// endpoint dots re-reads without a poll.
    pub on_state: Option<Arc<dyn Fn(ConnState) + Send + Sync>>,
}

/// What the host-key check decided, shared between the russh Handler (where the key arrives)
/// and dial() (where the failure is reported).
#[derive(Default)]
struct HostKeyOutcome {
    /// The fingerprint the server presented when it failed verification.
    mismatch: Option<String>,
}

/// The russh client handler: trust-on-first-use and the banner live here.
struct ClientHandler {
    expected: Option<String>,
    on_host_key: Option<HostKeyCallback>,
    outcome: Arc<Mutex<HostKeyOutcome>>,
    banner: Arc<Mutex<Option<String>>>,
}

impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public = key.public_key();
        let Ok(blob) = public.to_bytes() else {
            return Ok(false);
        };
        let fp = fingerprint(&blob);
        match &self.expected {
            None => {
                // Trust on first use: learn and persist.
                if let Some(cb) = &self.on_host_key {
                    cb(&fp);
                }
                Ok(true)
            }
            Some(expected) if expected == &fp => Ok(true),
            Some(_expected) => {
                if let Ok(mut o) = self.outcome.lock() {
                    o.mismatch = Some(fp);
                }
                Ok(false) // russh fails the handshake; dial() reports the mismatch
            }
        }
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let trimmed = banner.trim();
        // Cap at 200 bytes but never mid-character: a multi-byte banner (a Chinese MOTD is the
        // common case) sliced at a byte boundary panics — and under `panic = "abort"` one banner
        // would take the whole gateway down.
        let cut: String = if trimmed.len() > 200 {
            trimmed
                .char_indices()
                .take_while(|(i, _)| *i <= 200)
                .map(|(_, c)| c)
                .collect()
        } else {
            trimmed.to_string()
        };
        *self.banner.lock().unwrap_or_else(|e| e.into_inner()) = Some(cut);
        Ok(())
    }
}

/// A live authenticated session: the shared russh handle plus the machinery to notice its death.
struct Live {
    handle: Arc<Handle<ClientHandler>>,
    /// Set by end() before a deliberate disconnect, so the watcher does not report a loss.
    intentional: Arc<AtomicBool>,
    /// Signalled when the transport future completes (any reason).
    closed: tokio::sync::watch::Receiver<bool>,
    /// SPEC §tunnels.jump: this session's hold on its jump hop, taken when the transport was
    /// dialed. Dropping it together with the Live — a deliberate end or the transport
    /// watcher — releases the hop. One hold per connection covers every intermediate hop
    /// of a chain, because each hop's own client holds the hop below it.
    jump_hold: Option<Box<dyn Send + 'static>>,
}

pub struct SshConnection {
    /// The channel-open budget of SPEC §tunnels.selfheal, held as milliseconds.
    /// OPEN_BUDGET everywhere in production; tests shrink it so the ten seconds cost
    /// milliseconds of wall clock (a paused clock cannot pay for real socket IO — the
    /// runtime jumps it past the handshake). Atomic only so a test can set it through
    /// the shared Arc.
    open_budget_ms: AtomicU64,
    /// Consecutive unanswered opens so far (SPEC §tunnels.selfheal). An answered open —
    /// success or refusal — resets it; WEDGE_OPENS in a row fires on_wedged.
    open_timeouts: AtomicUsize,
    def: Mutex<SshConnDef>,
    state: Mutex<ConnState>,
    reason: Mutex<Option<String>>,
    banner: Mutex<Option<String>>,
    /// Rules currently using this connection. The last one to stop ends the client.
    pub refs: AtomicI64,
    /// Single-flight dial: concurrent first starts share one handshake.
    connect_lock: tokio::sync::Mutex<()>,
    live: Mutex<Option<Live>>,
    generation: AtomicU64,
    hooks: SshHooks,
}

impl SshConnection {
    pub fn new(def: SshConnDef, hooks: SshHooks) -> Arc<Self> {
        Arc::new(SshConnection {
            open_budget_ms: AtomicU64::new(OPEN_BUDGET.as_millis() as u64),
            open_timeouts: AtomicUsize::new(0),
            def: Mutex::new(def),
            state: Mutex::new(ConnState::Idle),
            reason: Mutex::new(None),
            banner: Mutex::new(None),
            refs: AtomicI64::new(0),
            connect_lock: tokio::sync::Mutex::new(()),
            live: Mutex::new(None),
            generation: AtomicU64::new(0),
            hooks,
        })
    }

    pub fn state(&self) -> ConnState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The connection id from the live def — the map key this client is filed under.
    pub fn id(&self) -> String {
        self.def
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id
            .clone()
    }

    pub fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn banner(&self) -> Option<String> {
        self.banner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replace the definition (an edit); callers stop the rules and reconnect around this.
    pub fn set_def(&self, def: SshConnDef) {
        *self.def.lock().unwrap_or_else(|e| e.into_inner()) = def;
    }

    pub fn connected(&self) -> bool {
        self.state() == ConnState::Connected
            && self.live.lock().map(|l| l.is_some()).unwrap_or(false)
    }

    /// Dial and authenticate. Single-flight: N rules starting together share one handshake.
    pub async fn connect(self: &Arc<Self>) -> Result<(), TunnelError> {
        if self.connected() {
            return Ok(());
        }
        let _guard = self.connect_lock.lock().await;
        if self.connected() {
            return Ok(());
        }
        self.dial().await
    }

    fn set_state(&self, state: ConnState, reason: Option<String>) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = reason;
        if let Some(on_state) = &self.hooks.on_state {
            on_state(state);
        }
    }

    async fn dial(self: &Arc<Self>) -> Result<(), TunnelError> {
        let def = self.def.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.set_state(ConnState::Connecting, None);

        // Read the private key BEFORE any dial — an unreadable key is a config problem, and a
        // wrong passphrase is an auth problem; neither should ever be retried. (Node's ssh2 threw
        // both synchronously out of connect(); decoding here reproduces that classification.)
        let key: Option<Arc<PrivateKey>> = if def.auth_type == super::types::AuthType::Key {
            Some(Arc::new(read_key(&def)?))
        } else {
            None
        };

        let outcome = Arc::new(Mutex::new(HostKeyOutcome::default()));
        let banner = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            expected: def.host_key.clone(),
            on_host_key: self.hooks.on_host_key.clone(),
            outcome: outcome.clone(),
            banner: banner.clone(),
        };
        let config = Arc::new(russh::client::Config {
            keepalive_interval: Some(KEEPALIVE),
            keepalive_max: KEEPALIVE_MAX,
            ..Default::default()
        });

        let outcome_for_err = outcome.clone();
        let def_host = def.host.clone();
        let def_port = def.port;
        let result = tokio::time::timeout(READY_TIMEOUT, async {
            // SPEC §tunnels.proxy, §tunnels.jump: a jump rides the hop's live session — the transport IS
            // the direct-tcpip channel opened on it, and the SSH handshake runs on the
            // channel stream (SSH-over-SSH, OpenSSH -J's model; each hop recursively
            // resolves its own jump first, and the 15s budget here covers the whole
            // chain). A proxied connection dials its own transport (CONNECT or socks5);
            // the direct path stays exactly the russh sugar it always was. Save-time
            // validation keeps proxy and jump mutually exclusive — jump wins if a
            // hand-edited file ever carries both.
            let mut jump_hold: Option<Box<dyn Send + 'static>> = None;
            let mut handle = if let Some(jump_id) = def.jump.clone() {
                let Some(dial) = self.hooks.jump.clone() else {
                    return Err(TunnelError::new(
                        "a jump connection is configured but no jump dialer is available",
                        FailureKind::Config,
                    ));
                };
                let (stream, hold) = dial(&jump_id, &def_host, def_port).await?;
                jump_hold = Some(hold);
                russh::client::connect_stream(config, stream, handler)
                    .await
                    .map_err(|err| as_tunnel_error(err, None))?
            } else {
                match def.proxy.clone() {
                    None => russh::client::connect(config, (def_host.as_str(), def_port), handler)
                        .await
                        .map_err(|err| as_tunnel_error(err, None))?,
                    Some(url) => {
                        let mut proxy = super::proxy::parse(&url)
                            .map_err(|e| TunnelError::new(e, FailureKind::Config))?;
                        proxy.username = def.proxy_username.clone();
                        proxy.password = def.proxy_password.clone();
                        let stream = super::proxy::dial(&proxy, &def_host, def_port).await?;
                        russh::client::connect_stream(config, stream, handler)
                            .await
                            .map_err(|err| as_tunnel_error(err, None))?
                    }
                }
            };
            self.authenticate(&mut handle, &def, key).await?;
            Ok::<(Handle<ClientHandler>, Option<Box<dyn Send + 'static>>), TunnelError>((
                handle, jump_hold,
            ))
        })
        .await;

        let (handle, jump_hold) = match result {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                // A host-key mismatch carries the presented fingerprint as detail, so the panel
                // can offer "trust this key".
                if let Some(actual) = outcome_for_err
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .mismatch
                    .clone()
                {
                    let expected = def.host_key.clone().unwrap_or_default();
                    let mut detail = Map::new();
                    detail.insert("expected".into(), json!(expected.clone()));
                    detail.insert("actual".into(), json!(actual.clone()));
                    return Err(TunnelError::with_detail(
                        format!(
                            "host key mismatch for {}: expected {}, got {}",
                            def.host, expected, actual
                        ),
                        FailureKind::HostKey,
                        detail,
                    ));
                }
                return Err(err);
            }
            Err(_) => {
                return Err(TunnelError::new(
                    "timed out while waiting for handshake",
                    FailureKind::Network,
                ));
            }
        };

        // Registered as the live client only now, under a fresh generation: a watcher from an
        // earlier session checks the generation before reporting a loss.
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let intentional = Arc::new(AtomicBool::new(false));
        let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
        let handle = Arc::new(handle);
        let watcher = WatcherDeps {
            conn: Arc::downgrade(self),
            generation,
            intentional: intentional.clone(),
            host: def.host.clone(),
            on_lost: self.hooks.on_lost.clone(),
        };
        let watch_handle = handle.clone();
        tokio::spawn(async move {
            loop {
                if watch_handle.is_closed() {
                    let _ = closed_tx.send(true);
                    watcher.transport_ended(None);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        *self.banner.lock().unwrap_or_else(|e| e.into_inner()) =
            banner.lock().unwrap_or_else(|e| e.into_inner()).clone();
        *self.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(Live {
            handle,
            intentional,
            closed: closed_rx,
            jump_hold,
        });
        // A fresh session answers opens: whatever wedge count an earlier incarnation
        // of this client accumulated starts over (SPEC §tunnels.selfheal).
        self.open_timeouts.store(0, Ordering::Relaxed);
        self.set_state(ConnState::Connected, None);
        Ok(())
    }

    async fn authenticate(
        &self,
        handle: &mut Handle<ClientHandler>,
        def: &SshConnDef,
        key: Option<Arc<PrivateKey>>,
    ) -> Result<(), TunnelError> {
        let result = match def.auth_type {
            super::types::AuthType::Password => {
                // Vault refs are strict at the connect boundary (SPEC §host.refs): a missing
                // secret refuses the connection with a reason, never an empty password.
                let password = refs::resolve(def.password.as_deref().unwrap_or(""))
                    .map_err(|e| TunnelError::new(format!("password: {e}"), FailureKind::Config))?;
                handle
                    .authenticate_password(def.username.clone(), password)
                    .await
                    .map_err(|err| as_tunnel_error(err, None))?
            }
            super::types::AuthType::Key => {
                let key = key.expect("key auth loaded the key above");
                // RSA signature hash is negotiated with the server; anything else ignores it.
                let hash_alg = handle.best_supported_rsa_hash().await.ok().flatten();
                handle
                    .authenticate_publickey(
                        def.username.clone(),
                        PrivateKeyWithHashAlg::new(key, hash_alg.flatten()),
                    )
                    .await
                    .map_err(|err| as_tunnel_error(err, None))?
            }
        };
        if !result.success() {
            // ssh2's wording — the panel matches on it in places.
            return Err(TunnelError::new(
                "All configured authentication methods failed",
                FailureKind::Auth,
            ));
        }
        Ok(())
    }

    /// Open one channel to `host:port` through this connection.
    pub async fn open_channel(&self, host: &str, port: u16) -> Result<ByteStream, TunnelError> {
        let handle = {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            if self.state() != ConnState::Connected {
                return Err(TunnelError::new(
                    "ssh connection is not established",
                    FailureKind::Network,
                ));
            }
            let Some(live) = live.as_ref() else {
                return Err(TunnelError::new(
                    "ssh connection is not established",
                    FailureKind::Network,
                ));
            };
            live.handle.clone()
        };
        // SPEC §tunnels.selfheal: the open rides one budget, so a session that never
        // answers fails the socket instead of hanging it; an answered refusal keeps its
        // ordinary meaning — the session proved it is alive, the target is what said no.
        let open_budget = Duration::from_millis(self.open_budget_ms.load(Ordering::Relaxed));
        let opened = tokio::time::timeout(
            open_budget,
            handle.channel_open_direct_tcpip(
                host.to_string(),
                port as u32,
                "127.0.0.1".to_string(),
                0,
            ),
        )
        .await;
        let channel = match opened {
            // An answer arrived — success or refusal — so the session is alive: any
            // wedge count it had accumulated is history.
            Ok(result) => {
                self.open_timeouts.store(0, Ordering::Relaxed);
                result.map_err(|err| {
                    as_tunnel_error(err, Some(&format!("channel to {host}:{port} failed")))
                })?
            }
            Err(_budget_spent) => {
                // SPEC §tunnels.selfheal: consecutive unanswered opens judge the session
                // wedged — keepalive cannot see this shape, the transport still answers.
                // The hook fires once per episode, on the open that completes the count.
                let prev = self.open_timeouts.fetch_add(1, Ordering::Relaxed);
                if prev + 1 == WEDGE_OPENS {
                    if let Some(on_wedged) = &self.hooks.on_wedged {
                        on_wedged();
                    }
                }
                return Err(TunnelError::new(
                    format!(
                        "channel open to {host}:{port} timed out after {open_budget:?}"
                    ),
                    FailureKind::Network,
                ));
            }
        };
        Ok(Box::pin(channel.into_stream()))
    }

    /// Tests shrink the budget so §tunnels.selfheal's ten seconds cost milliseconds;
    /// production always opens with OPEN_BUDGET.
    #[cfg(test)]
    pub(crate) fn set_open_budget(&self, budget: Duration) {
        self.open_budget_ms.store(budget.as_millis() as u64, Ordering::Relaxed);
    }

    /// Open an interactive PTY on this connection and pump it against `endpoint`.
    ///
    /// Resolves once the shell has STARTED, not when it ends: the pump runs in its own
    /// task and `hold` — the manager's reference on this client — travels with it, so the
    /// connection is released exactly when the session is over and not a moment before.
    ///
    /// russh has had PTY channels all along; no new SSH dependency is involved (SPEC §terminal.remote).
    /// What is new is the shape of the plumbing on this side of it.
    pub async fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> Result<(), TunnelError> {
        let handle = {
            let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            if self.state() != ConnState::Connected {
                return Err(TunnelError::new(
                    "ssh connection is not established",
                    FailureKind::Network,
                ));
            }
            let Some(live) = live.as_ref() else {
                return Err(TunnelError::new(
                    "ssh connection is not established",
                    FailureKind::Network,
                ));
            };
            live.handle.clone()
        };
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|err| as_tunnel_error(err, Some("could not open a session channel")))?;
        let (mut read, write) = channel.split();

        // Both requests carry want_reply and are confirmed BEFORE the shell is reported
        // open. Fire-and-forget would mean a server that refuses a PTY hands the user a
        // working-looking terminal with no tty behind it - no echo, no job control, and
        // nothing saying why.
        write
            .request_pty(
                true,
                TERM,
                size.cols as u32,
                size.rows as u32,
                0,
                0,
                &[], // no modes: let the far side keep its defaults, as ssh(1) does
            )
            .await
            .map_err(|err| as_tunnel_error(err, Some("pty request failed")))?;
        await_request_reply(&mut read, "a pseudo-terminal").await?;
        write
            .request_shell(true)
            .await
            .map_err(|err| as_tunnel_error(err, Some("shell request failed")))?;
        await_request_reply(&mut read, "an interactive shell").await?;

        tokio::spawn(async move {
            let (out, input) = endpoint.split();
            // The input pump is a child of the output pump rather than a sibling: when the
            // far side goes away there is nothing left to type into, and an orphan parked
            // on next() would hold `hold` - and so the whole SSH client - open forever.
            let typing = tokio::spawn(pump_input(input, write));
            pump_output(&mut read, out).await;
            typing.abort();
            drop(hold);
        });
        Ok(())
    }

    // --- non-interactive exec and SFTP (SPEC §remote.transport, §remote.actions) ---------------------------------------

    /// The live handle clone every operation below starts with: connected-state check and
    /// Arc bump in one place, identical to open_channel/open_shell above.
    fn live_handle(&self) -> Result<Arc<Handle<ClientHandler>>, TunnelError> {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if self.state() != ConnState::Connected {
            return Err(TunnelError::new(
                "ssh connection is not established",
                FailureKind::Network,
            ));
        }
        let Some(live) = live.as_ref() else {
            return Err(TunnelError::new(
                "ssh connection is not established",
                FailureKind::Network,
            ));
        };
        Ok(live.handle.clone())
    }

    /// Run one non-interactive command over a session channel, streaming stdout/stderr
    /// into `events` AS THEY ARRIVE (never read-to-end), and return the exit status.
    ///
    /// Cancellation is cooperative and real (SPEC §remote.actions): on cancel the channel is
    /// closed from this side, a bounded drain collects whatever the far side flushes,
    /// and the operation resolves as canceled — no future-dropping pretence. The
    /// `hold` lease travels with this future, so the connection reference lives
    /// exactly as long as the exec.
    pub async fn exec(
        &self,
        mut request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: super::manager::ConnectionLease,
    ) -> Result<i32, TunnelError> {
        let handle = self.live_handle()?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|err| as_tunnel_error(err, Some("could not open a session channel")))?;
        let (mut read, write) = channel.split();
        // argv -> POSIX command string happens HERE, once, through the tested quoting
        // in tunnel/remote.rs — the provider contract carries argv precisely so this
        // translation cannot drift per caller.
        let command = super::remote::exec_command_string(&request);
        write
            .exec(true, command.as_bytes().to_vec())
            .await
            .map_err(|err| as_tunnel_error(err, Some("exec request failed")))?;
        await_request_reply(&mut read, "the exec request").await?;

        // Stdin rides alongside the output (SPEC §remote.transport), never ahead of it. While
        // the feed waits for window, this loop must keep draining the channel: russh parks
        // the whole session on a full channel buffer, and a parked session never reads the
        // window adjustment the feed is waiting for - a command that answers its input
        // (cat, sh -s) would wedge both. data_bytes cuts the text to the window itself.
        // EOF follows the text, or comes at once when there is none, so a command that
        // reads stdin ends instead of sitting out its deadline.
        let stdin = request.stdin.take().unwrap_or_default().into_bytes();
        let feeder = &write;
        let feed = async move {
            if !stdin.is_empty() {
                feeder.data_bytes(stdin).await?;
            }
            feeder.eof().await
        };
        tokio::pin!(feed);
        let mut fed = false;

        let mut exit_code: Option<i32> = None;
        let result = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    // Ask the far side to stop, then drain briefly: a well-behaved
                    // command flushes; a wedged one costs at most CANCEL_DRAIN and the
                    // channel close kills the session either way.
                    let _ = write.close().await;
                    let drained = tokio::time::timeout(CANCEL_DRAIN, async {
                        while let Some(msg) = read.wait().await {
                            match msg {
                                ChannelMsg::Data { data } => {
                                    if events
                                        .send(swiss_host::services::remote::RemoteExecEvent::Stdout(data.to_vec()))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                ChannelMsg::ExtendedData { data, .. } => {
                                    if events
                                        .send(swiss_host::services::remote::RemoteExecEvent::Stderr(data.to_vec()))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                ChannelMsg::ExitStatus { exit_status } => {
                                    exit_code = Some(exit_status as i32);
                                }
                                ChannelMsg::Close | ChannelMsg::Eof => return,
                                _ => {}
                            }
                        }
                    })
                    .await;
                    let _ = drained;
                    break Err(TunnelError::new(
                        "the remote command was canceled".to_string(),
                        FailureKind::Canceled,
                    ));
                }
                msg = read.wait() => {
                    let Some(msg) = msg else {
                        // The channel ended without an ExitStatus — ssh2's convention is
                        // 255 for "no exit status", and OpenSSH's ssh does the same.
                        break Ok(exit_code.unwrap_or(255));
                    };
                    match msg {
                        ChannelMsg::Data { data } => {
                            if events
                                .send(swiss_host::services::remote::RemoteExecEvent::Stdout(data.to_vec()))
                                .await
                                .is_err()
                            {
                                // The consumer is gone: no point reading more. Close the
                                // channel so the remote command sees EOF on its stdout
                                // and the lease below is released promptly.
                                let _ = write.close().await;
                                read.wait().await; // park until the channel truly ends
                                break Err(TunnelError::new(
                                    "the run reading this output went away".to_string(),
                                    FailureKind::Canceled,
                                ));
                            }
                        }
                        ChannelMsg::ExtendedData { data, .. } => {
                            if events
                                .send(swiss_host::services::remote::RemoteExecEvent::Stderr(data.to_vec()))
                                .await
                                .is_err()
                            {
                                let _ = write.close().await;
                                read.wait().await;
                                break Err(TunnelError::new(
                                    "the run reading this output went away".to_string(),
                                    FailureKind::Canceled,
                                ));
                            }
                        }
                        ChannelMsg::ExitStatus { exit_status } => {
                            exit_code = Some(exit_status as i32);
                        }
                        ChannelMsg::Eof => { /* status comes next, keep reading */ }
                        ChannelMsg::Close => break Ok(exit_code.unwrap_or(255)),
                        ChannelMsg::WindowAdjusted { .. } => {}
                        _ => {}
                    }
                }
                sent = &mut feed, if !fed => {
                    // A refused send means the channel is already gone (the command
                    // exited without reading it all); the read side reports how it ended.
                    let _ = sent;
                    fed = true;
                }
            }
        };
        // The lease drops here — after the channel ended, before the caller learns the
        // result — so the connection reference cannot outlive the operation.
        drop(hold);
        result
    }

    /// Open one SFTP session over this connection (SPEC §remote.actions): a session channel, the
    /// "sftp" subsystem request, then russh-sftp's handshake over the channel stream.
    /// One SFTP session per operation keeps lifetimes obvious; the connection itself is
    /// still shared and refcounted underneath.
    async fn sftp_session(&self) -> Result<Arc<russh_sftp::client::SftpSession>, TunnelError> {
        let handle = self.live_handle()?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|err| as_tunnel_error(err, Some("could not open an sftp channel")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|err| as_tunnel_error(err, Some("the sftp request failed")))?;
        let session = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|err| TunnelError::new(format!("sftp: {err}"), FailureKind::Network))?;
        Ok(Arc::new(session))
    }

    pub async fn stat_file(
        &self,
        path: String,
        hold: super::manager::ConnectionLease,
    ) -> Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError> {
        let session = self.sftp_session().await?;
        let result = match session.metadata(&path).await {
            Ok(meta) => Some(swiss_host::services::remote::RemoteFileStat {
                size: meta.len(),
                // SFTP's mtime is whole seconds; the ms field keeps room for transports
                // that can see finer.
                mtime_ms: meta.mtime.map(|secs| secs as u64 * 1000),
                is_dir: meta.is_dir(),
            }),
            Err(err) if is_no_such_file(&err) => None,
            Err(err) => {
                return Err(TunnelError::new(
                    format!("sftp stat {path}: {err}"),
                    FailureKind::Network,
                ))
            }
        };
        drop(hold);
        Ok(result)
    }

    /// Set one remote file's mtime over SFTP SETSTAT. atime rides along with
    /// the same value: the wire format sends both whenever either is present,
    /// and leaving atime unset would stamp it to the epoch.
    pub async fn set_mtime(
        &self,
        path: String,
        mtime_secs: u64,
        hold: super::manager::ConnectionLease,
    ) -> Result<(), TunnelError> {
        let session = self.sftp_session().await?;
        let mut attrs = russh_sftp::protocol::FileAttributes::default();
        let secs = mtime_secs.min(u32::MAX as u64) as u32;
        attrs.atime = Some(secs);
        attrs.mtime = Some(secs);
        session.set_metadata(&path, attrs).await.map_err(|err| {
            TunnelError::new(format!("sftp setstat {path}: {err}"), FailureKind::Network)
        })?;
        drop(hold);
        Ok(())
    }

    pub async fn open_read(
        &self,
        path: String,
        hold: super::manager::ConnectionLease,
    ) -> Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError> {
        let session = self.sftp_session().await?;
        let file = session.open(&path).await.map_err(|err| {
            TunnelError::new(format!("sftp open {path}: {err}"), FailureKind::Network)
        })?;
        Ok(Box::new(SftpRead {
            file,
            _session: session,
            _hold: hold,
        }))
    }

    pub async fn create_file(
        &self,
        path: String,
        hold: super::manager::ConnectionLease,
    ) -> Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError> {
        let session = self.sftp_session().await?;
        let file = session.create(&path).await.map_err(|err| {
            TunnelError::new(format!("sftp create {path}: {err}"), FailureKind::Network)
        })?;
        Ok(Box::new(SftpWrite {
            file,
            session: Some(session),
            hold: Some(hold),
        }))
    }

    pub async fn mkdir_p(
        &self,
        path: String,
        hold: super::manager::ConnectionLease,
    ) -> Result<(), TunnelError> {
        let session = self.sftp_session().await?;
        // Walk the prefix chain: SFTP's mkdir is single-level and fails on an existing
        // directory, so each component is attempted and existence is tolerated.
        let mut built = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            built.push('/');
            built.push_str(part);
            if let Err(err) = session.create_dir(&built).await {
                if !is_already_exists(&err) {
                    // Distinguish "a FILE blocks this path" from transport trouble.
                    if let Ok(meta) = session.metadata(&built).await {
                        if !meta.is_dir() {
                            return Err(TunnelError::new(
                                format!("sftp mkdir {built}: a non-directory is in the way"),
                                FailureKind::Network,
                            ));
                        }
                    } else {
                        return Err(TunnelError::new(
                            format!("sftp mkdir {built}: {err}"),
                            FailureKind::Network,
                        ));
                    }
                }
            }
        }
        drop(hold);
        Ok(())
    }

    /// List one remote directory's immediate children over SFTP readdir. Names are
    /// the entries' own names (read_dir already skips "." and ".."); a size the
    /// server did not report reads as 0, which the walkers tolerate by design.
    pub async fn list_dir(
        &self,
        path: String,
        hold: super::manager::ConnectionLease,
    ) -> Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError> {
        let session = self.sftp_session().await?;
        let entries = session.read_dir(&path).await.map_err(|err| {
            TunnelError::new(format!("sftp readdir {path}: {err}"), FailureKind::Network)
        })?;
        let mut out = Vec::new();
        for entry in entries {
            out.push(swiss_host::services::remote::RemoteListing {
                name: entry.file_name(),
                is_dir: entry.file_type().is_dir(),
                size: entry.metadata().len(),
            });
        }
        drop(hold);
        Ok(out)
    }

    /// End the session (best effort, capped): a wedged transport must not block a shutdown.
    pub async fn end(&self) {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.set_state(ConnState::Idle, None);
        *self.banner.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let Some(mut live) = live else { return };
        live.intentional.store(true, Ordering::SeqCst);
        let _ = live
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        let mut closed = live.closed;
        let _ = tokio::time::timeout(END_TIMEOUT, closed.changed()).await;
        // Release the jump hold AFTER our own transport is down — the hop served its
        // whole session, and releasing it first could yank the channel out from under
        // the disconnect above.
        drop(live.jump_hold.take());
    }

    /// Connect, authenticate and disconnect, on a THROWAWAY client.
    ///
    /// Never the live one: Test has to prove the credentials and the host key end to end, which
    /// is exactly what a probe on an already-authenticated session cannot do. Note the throwaway
    /// runs WITHOUT the persistence hook — a successful Test of a keyless connection stores
    /// nothing (the first real connect is what learns and persists the fingerprint). Its hook
    /// only remembers the key it was shown, and the result reports it, so a caller can ask a
    /// human before anything pins it (SPEC §host.setup).
    ///
    /// SPEC §tunnels.jump: a jump def rides a throwaway CHAIN — one one-off client per hop,
    /// resolved through the defs argument at dial time. Nothing is shared with the
    /// manager's connection table, and no hop learns a host key.
    pub async fn test(def: &SshConnDef, defs: Option<JumpDefs>) -> TestResult {
        let t0 = std::time::Instant::now();
        let shown: Arc<Mutex<Option<String>>> = Arc::default();
        let remember = shown.clone();
        let on_host_key: HostKeyCallback = Arc::new(move |fingerprint: &str| {
            *remember.lock().unwrap_or_else(|e| e.into_inner()) = Some(fingerprint.to_string());
        });
        let mut hooks = SshHooks {
            on_host_key: Some(on_host_key),
            ..SshHooks::default()
        };
        // Without a resolver (no store at hand) a jump def cannot look its hop up — a
        // config failure reported by the dial, not a silent direct connection.
        if let (Some(_), Some(defs)) = (&def.jump, defs) {
            hooks.jump = Some(throwaway_dialer(defs));
        }
        let probe = SshConnection::new(def.clone(), hooks);
        let result = probe.connect().await;
        let banner = probe.banner();
        let shown = shown.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Err(err) = result {
            let mismatch = err
                .detail
                .as_ref()
                .and_then(|detail| detail.get("actual"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let out = TestResult {
                ok: false,
                ms: t0.elapsed().as_millis() as u64,
                error: Some(err.message.clone()),
                kind: Some(err.kind.as_str().to_string()),
                fingerprint: mismatch.or(shown),
                banner,
            };
            let _ = probe.end().await;
            return out;
        }
        let _ = probe.end().await;
        TestResult {
            ok: true,
            ms: t0.elapsed().as_millis() as u64,
            banner,
            error: None,
            kind: None,
            fingerprint: shown,
        }
    }
}

/// The Test path's jump dialer (SPEC §tunnels.jump): every hop is a one-off client with no
/// persistence hook — a successful Test of a keyless hop stores nothing (the first real
/// connect is what learns the fingerprint), and nothing in the manager's shared table is
/// pinned by a probe.
fn throwaway_dialer(defs: JumpDefs) -> JumpDialer {
    Arc::new(move |jump_id: &str, host: &str, port: u16| {
        let defs = defs.clone();
        let jump_id = jump_id.to_string();
        let host = host.to_string();
        Box::pin(throwaway_jump(defs, jump_id, host, port))
    })
}

/// One hop of the throwaway chain. The probe's Arc travels as the hold: the channel
/// stream keeps the hop's session meaningful for exactly as long as the caller's
/// handshake needs it, and the hop tears down with the probe.
fn throwaway_jump(defs: JumpDefs, jump_id: String, host: String, port: u16) -> JumpResolved {
    Box::pin(async move {
        let Some(def) = defs(&jump_id) else {
            return Err(TunnelError::new(
                format!("jump connection not found: {jump_id}"),
                FailureKind::Config,
            ));
        };
        let name = def.name.clone();
        // Same discipline as the manager's live chain (§3.3): the hop's kind survives,
        // the message gains the hop's name, and the pair is constructed directly — the
        // prefix must never flow through classify_message, whose substring scan would
        // wash the kind away.
        let via =
            |err: TunnelError| TunnelError::new(format!("via {name}: {}", err.message), err.kind);
        let probe = SshConnection::new(
            def,
            SshHooks {
                jump: Some(throwaway_dialer(defs.clone())),
                ..SshHooks::default()
            },
        );
        probe.connect().await.map_err(via)?;
        let stream = probe.open_channel(&host, port).await.map_err(via)?;
        Ok((stream, Box::new(probe) as Box<dyn Send + 'static>))
    })
}

/// Everything the transport-watcher task needs once the session future resolves.
struct WatcherDeps {
    conn: std::sync::Weak<SshConnection>,
    generation: u64,
    intentional: Arc<AtomicBool>,
    host: String,
    on_lost: Option<Arc<dyn Fn(TunnelError) + Send + Sync>>,
}

impl WatcherDeps {
    fn transport_ended(self, err: Option<russh::Error>) {
        // Past the handshake, a transport failure is a runtime event: the manager releases the
        // ports of every rule on this connection, then decides whether a retry is allowed.
        if self.intentional.load(Ordering::SeqCst) {
            return; // end() asked for it
        }
        let Some(conn) = self.conn.upgrade() else {
            return;
        };
        // A later connection already replaced this one — its own watcher is current.
        if conn.generation.load(Ordering::SeqCst) != self.generation {
            return;
        }
        conn.live.lock().unwrap_or_else(|e| e.into_inner()).take();
        let te = match err {
            Some(err) => as_tunnel_error(err, Some("ssh connection lost")),
            None => TunnelError::new("ssh connection closed", FailureKind::Network),
        };
        conn.set_state(ConnState::Error, Some(te.message.clone()));
        log::warn(
            "ssh connection lost",
            Some(json!({ "host": self.host, "err": te.message })),
        );
        if let Some(cb) = &self.on_lost {
            cb(te);
        }
    }
}

/// Read the private key, failing as `config` before any dial when it cannot be read, and as
/// `auth` when it does not parse (Node's ssh2 threw both out of connect()).
fn read_key(def: &SshConnDef) -> Result<PrivateKey, TunnelError> {
    let raw_path = def.key_path.clone().unwrap_or_default();
    let path = expand_home(&raw_path);
    if path.is_empty() {
        return Err(TunnelError::new(
            "no private key path configured",
            FailureKind::Config,
        ));
    }
    if !std::path::Path::new(&path).is_absolute() {
        return Err(TunnelError::new(
            format!("private key path must be absolute: {path}"),
            FailureKind::Config,
        ));
    }
    let bytes = std::fs::read(&path).map_err(|err| {
        TunnelError::new(
            format!("cannot read private key {path}: {err}"),
            FailureKind::Config,
        )
    })?;
    let text = String::from_utf8_lossy(&bytes);
    // Same strict connect-time contract as the password (SPEC §host.refs).
    let passphrase = refs::resolve(def.passphrase.as_deref().unwrap_or(""))
        .map_err(|e| TunnelError::new(format!("passphrase: {e}"), FailureKind::Config))?;
    russh::keys::decode_secret_key(&text, Some(&passphrase)).map_err(|err| {
        TunnelError::new(
            format!("cannot parse private key: {err}"),
            FailureKind::Auth,
        )
    })
}

/// The API shape of POST /connections/:id/test.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub ok: bool,
    pub ms: u64,
    pub banner: Option<String>,
    pub error: Option<String>,
    pub kind: Option<String>,
    /// The key the server presented when a stored one did not match, so the panel can offer
    /// "trust this key", or the key an unpinned connection was shown, so a caller can ask
    /// someone before pinning it. Absent when a stored key matched.
    pub fingerprint: Option<String>,
}

impl TestResult {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("ok".into(), json!(self.ok));
        m.insert("ms".into(), json!(self.ms));
        if let Some(b) = &self.banner {
            m.insert("banner".into(), json!(b));
        }
        if let Some(e) = &self.error {
            m.insert("error".into(), json!(e));
        }
        if let Some(k) = &self.kind {
            m.insert("kind".into(), json!(k));
        }
        if let Some(f) = &self.fingerprint {
            m.insert("fingerprint".into(), json!(f));
        }
        Value::Object(m)
    }
}

// --- interactive shell plumbing (SPEC §terminal.remote) -------------------------------------------------

/// How long a canceled exec drains for before giving up on the far side's flush.
/// Bounded so a wedged remote command cannot hold a cancel hostage.
const CANCEL_DRAIN: Duration = Duration::from_secs(3);

/// russh-sftp surfaces server refusals as typed status codes; absence is a normal
/// answer to a sync's existence check, not a transport loss.
fn is_no_such_file(err: &russh_sftp::client::error::Error) -> bool {
    matches!(
        err,
        russh_sftp::client::error::Error::Status(s)
            if s.status_code == russh_sftp::protocol::StatusCode::NoSuchFile
    )
}

/// OpenSSH answers mkdir over an existing directory with the generic FAILURE code
/// (there is no dedicated "exists" code in SFTP v3); the caller only needs the parents
/// to exist, so that answer is tolerated.
fn is_already_exists(err: &russh_sftp::client::error::Error) -> bool {
    matches!(
        err,
        russh_sftp::client::error::Error::Status(s)
            if s.status_code == russh_sftp::protocol::StatusCode::Failure
                || s.status_code == russh_sftp::protocol::StatusCode::PermissionDenied
    )
}

/// One streaming remote read over SFTP. Owns the file handle, the SFTP session and the
/// connection lease: all three live exactly as long as the reader (SPEC §remote.transport).
struct SftpRead {
    file: russh_sftp::client::fs::File,
    _session: Arc<russh_sftp::client::SftpSession>,
    _hold: super::manager::ConnectionLease,
}

#[async_trait]
impl swiss_host::services::remote::RemoteRead for SftpRead {
    async fn read_chunk(
        &mut self,
    ) -> Result<Option<Vec<u8>>, swiss_host::services::remote::RemoteError> {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; swiss_host::services::remote::REMOTE_CHUNK_BYTES];
        match self.file.read(&mut buf).await {
            Ok(0) => Ok(None),
            Ok(n) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(err) => Err(swiss_host::services::remote::RemoteError::Failed(format!(
                "sftp read: {err}"
            ))),
        }
    }
}

/// One streaming remote write over SFTP. Same ownership story as SftpRead; `finish`
/// flushes, `drop` without finish is an abort.
///
/// A chunk is only queued: russh-sftp keeps up to `max_concurrent_writes` WRITE requests
/// in flight and checks the oldest ack whenever that queue is full, so a refused write
/// still surfaces on a later chunk or at `finish`. Flushing per chunk would wait out every
/// ack plus an fsync round trip each 64 KiB - two RTTs per chunk, which held a 170 MB
/// upload over a 70 ms link to ~420 KB/s.
struct SftpWrite {
    file: russh_sftp::client::fs::File,
    session: Option<Arc<russh_sftp::client::SftpSession>>,
    hold: Option<super::manager::ConnectionLease>,
}

#[async_trait]
impl swiss_host::services::remote::RemoteWrite for SftpWrite {
    async fn write_chunk(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), swiss_host::services::remote::RemoteError> {
        use tokio::io::AsyncWriteExt;
        self.file.write_all(bytes).await.map_err(|err| {
            swiss_host::services::remote::RemoteError::Failed(format!("sftp write: {err}"))
        })
    }

    async fn finish(&mut self) -> Result<(), swiss_host::services::remote::RemoteError> {
        use tokio::io::AsyncWriteExt;
        // The flush drains every queued ack (then fsyncs once): a refused write is
        // reported here, never dropped - a failed ack is consumed by the drain, so
        // ignoring this error would let the close below report success.
        self.file.flush().await.map_err(|err| {
            swiss_host::services::remote::RemoteError::Failed(format!("sftp write: {err}"))
        })?;
        self.file.shutdown().await.map_err(|err| {
            swiss_host::services::remote::RemoteError::Failed(format!("sftp close: {err}"))
        })?;
        // Release the session and the lease AFTER the close — the write is what keeps
        // the connection alive, and it is over now.
        let session = self.session.take();
        let hold = self.hold.take();
        drop(session);
        drop(hold);
        Ok(())
    }
}

/// Consume the server's answer to one `want_reply` channel request. Anything other than
/// Success is reported by name: "the server refused a pseudo-terminal" is something a user
/// can act on, where a terminal that silently behaves oddly is not.
async fn await_request_reply(read: &mut ChannelReadHalf, what: &str) -> Result<(), TunnelError> {
    // A server may widen the channel window at ANY moment - including between our
    // request and its reply - and russh surfaces those adjustments through wait()
    // like any other channel message. They are not answers; skip them and keep
    // waiting. (OpenSSH sends a 2 MiB WindowAdjusted exactly here, right as the shell
    // starts, which is what once made every remote open fail as "unexpected".)
    loop {
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, read.wait())
            .await
            .map_err(|_| {
                TunnelError::new(
                    format!("the server did not answer the request for {what}"),
                    FailureKind::Network,
                )
            })?;
        match reply {
            Some(ChannelMsg::WindowAdjusted { .. }) => continue,
            other => return answer_of(other, what),
        }
    }
}

/// The single-message decision left after window adjustments have been skipped.
fn answer_of(reply: Option<ChannelMsg>, what: &str) -> Result<(), TunnelError> {
    match reply {
        Some(ChannelMsg::Success) => Ok(()),
        Some(ChannelMsg::Failure) => Err(TunnelError::new(
            format!("the server refused {what}"),
            FailureKind::Network,
        )),
        Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => Err(TunnelError::new(
            format!("the session channel closed while requesting {what}"),
            FailureKind::Network,
        )),
        // Data before the reply would be a protocol violation on a channel that has not
        // been given a shell yet; treat anything unexpected as a refusal rather than
        // guessing, so an odd server cannot produce a half-open terminal.
        Some(other) => Err(TunnelError::new(
            format!("the server answered the request for {what} unexpectedly ({other:?})"),
            FailureKind::Network,
        )),
    }
}

/// Remote -> consumer, until the channel ends or the consumer drops its session.
///
/// The send is deliberately NOT inside the select: parking here is the backpressure that
/// stops us draining the SSH window, which is what makes the far side slow down instead of
/// this process buffering an unbounded flood (SPEC §terminal.sessions).
async fn pump_output(read: &mut ChannelReadHalf, out: PtyOut) {
    let mut code: Option<i32> = None;
    loop {
        let msg = tokio::select! {
            // Both arms are mpsc recv(), which is cancel-safe: whichever loses the race
            // has consumed nothing.
            msg = read.wait() => msg,
            _ = out.closed() => break,
        };
        match msg {
            Some(ChannelMsg::Data { data }) => {
                if out.send(PtyEvent::Data(data.to_vec())).await.is_err() {
                    return; // the consumer left; no exit event to deliver
                }
            }
            // stderr on a PTY session is unusual but legal. A terminal shows it inline,
            // exactly where the shell meant it to appear.
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                if out.send(PtyEvent::Data(data.to_vec())).await.is_err() {
                    return;
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status as i32),
            Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                let _ = out
                    .send(PtyEvent::Error(format!(
                        "the remote shell was killed by {signal_name:?}"
                    )))
                    .await;
            }
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    // Best effort: the consumer may already be gone, which is not an error here.
    let _ = out.send(PtyEvent::Exit { code }).await;
}

/// Consumer -> remote, until the consumer drops its session or the transport refuses.
///
/// Its own task so that a flooding terminal cannot starve it: an interrupt typed while the
/// output pump is parked on a full queue still reaches the far side (SPEC §terminal.verify item 7).
async fn pump_input(mut input: PtyIn, write: ChannelWriteHalf<Msg>) {
    while let Some(next) = input.next().await {
        let sent = match next {
            PtyInput::Data(bytes) => write.data_bytes(bytes).await,
            PtyInput::Resize(size) => {
                write
                    .window_change(size.cols as u32, size.rows as u32, 0, 0)
                    .await
            }
        };
        if sent.is_err() {
            return; // the transport is gone; the output pump reports the end
        }
    }
    // The consumer closed the tab: send EOF so the remote shell sees its stdin end and
    // exits on its own, rather than being cut off mid-command.
    let _ = write.eof().await;
    let _ = write.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_the_openssl_style_format() {
        // SHA256 of the empty input, unpadded.
        let fp = fingerprint(&[]);
        assert!(fp.starts_with("SHA256:"));
        assert!(!fp.ends_with('='), "padding is stripped");
        assert_eq!(fp.len(), "SHA256:".len() + 43);
    }

    #[test]
    fn expand_home_forms() {
        let home = home_dir().to_string_lossy().to_string();
        assert_eq!(expand_home("~"), home);
        assert_eq!(
            expand_home("~/.ssh/id_rsa"),
            format!(
                "{}/.ssh/id_rsa",
                home.trim_end_matches('\\').trim_end_matches('/')
            )
            .replace('/', std::path::MAIN_SEPARATOR_STR)
        );
        assert_eq!(expand_home("  C:/keys/x.pem  "), "C:/keys/x.pem");
        assert_eq!(expand_home("~/x"), {
            let mut p = home_dir();
            p.push("x");
            p.to_string_lossy().to_string()
        });
    }

    #[test]
    fn classify_sorts_auth_and_hostkey_from_network() {
        assert_eq!(
            classify_message("All configured authentication methods failed"),
            FailureKind::Auth
        );
        assert_eq!(classify_message("userauth error"), FailureKind::Auth);
        assert_eq!(classify_message("host key mismatch"), FailureKind::HostKey);
        assert_eq!(
            classify_message("connection reset by peer"),
            FailureKind::Network
        );
    }

    /// SPEC §tunnels.proxy: classify_message scans for the substrings "auth" and "host key",
    /// so a fixed proxy failure string that could ever flow through a classifier must
    /// avoid both — a proxy outage is a network event and the retry policy may run.
    /// (The spec's verbatim "...socks5 auth method" wording DOES contain "auth"; those
    /// two strings are built with an explicit Network kind and never re-classified,
    /// which the dialer tests' kind assertions pin — nothing routes them through here.)
    #[test]
    fn proxy_error_strings_stay_network_kind() {
        for msg in [
            "proxy 127.0.0.1:8080 refused CONNECT: HTTP 407",
            "proxy 127.0.0.1:8080 rejected socks5 credentials",
            "proxy 127.0.0.1:8080 socks5 reply 1",
            "cannot reach proxy 127.0.0.1:8080: connection refused",
            "proxy 127.0.0.1:8080 closed during CONNECT",
            "proxy 127.0.0.1:8080 sent a malformed CONNECT response",
        ] {
            assert_eq!(
                classify_message(msg),
                FailureKind::Network,
                "misclassified: {msg}"
            );
        }
    }

    #[test]
    fn as_tunnel_error_prefixes_and_classifies() {
        let te = as_tunnel_error(
            "auth failed for user root",
            Some("channel to db:5432 failed"),
        );
        assert_eq!(
            te.message,
            "channel to db:5432 failed: auth failed for user root"
        );
        assert_eq!(te.kind, FailureKind::Auth);
    }

    #[test]
    fn reading_a_missing_key_is_config_not_auth() {
        let def = SshConnDef {
            id: "c".into(),
            name: "c".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth_type: super::super::types::AuthType::Key,
            group: None,
            key_path: Some(format!(
                "{}/definitely-absent-{}",
                home_dir().to_string_lossy(),
                swiss_core::util::random_hex(4)
            )),
            passphrase: None,
            password: None,
            host_key: None,
            proxy: None,
            proxy_username: None,
            proxy_password: None,
            jump: None,
        };
        let err = read_key(&def).unwrap_err();
        assert_eq!(err.kind, FailureKind::Config);
        assert!(err.message.starts_with("cannot read private key"));
    }

    #[test]
    fn a_missing_vault_passphrase_reference_refuses_before_the_dial() {
        // SPEC §host.refs at the tunnel surface: a passphrase reference this machine does not hold
        // is a Config failure naming the reference — before any network attempt, and never an
        // empty passphrase sent to a key parser.
        let key =
            std::env::temp_dir().join(format!("swiss-key-{}.pem", swiss_core::util::random_hex(6)));
        std::fs::write(&key, b"not really a key, just bytes").expect("write the key file");
        let def = SshConnDef {
            id: "c".into(),
            name: "c".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth_type: super::super::types::AuthType::Key,
            group: None,
            key_path: Some(key.to_string_lossy().into_owned()),
            passphrase: Some("${secret://ssh-test-missing}".into()),
            password: None,
            host_key: None,
            proxy: None,
            proxy_username: None,
            proxy_password: None,
            jump: None,
        };
        let err = read_key(&def).unwrap_err();
        assert_eq!(err.kind, FailureKind::Config);
        assert!(
            err.message.contains("passphrase")
                && err.message.contains("secret://ssh-test-missing")
                && err.message.contains("not in the vault"),
            "names the field and the reference: {}",
            err.message
        );
        let _ = std::fs::remove_file(&key);
    }

    // --- the open budget (SPEC §tunnels.selfheal) ------------------------------------------------
    //
    // A real russh client against a real russh server on loopback; the server's open
    // policy is the only variable. The budget is shrunk per client so the ten seconds
    // cost milliseconds — a paused clock cannot stand in for real socket IO (the
    // runtime jumps it past the handshake).

    mod selfheal {
        use super::super::*;
        use crate::tunnel::sshtest::{spawn_opens_ssh_server, TestOpens};
        use std::sync::Arc;
        use std::time::Duration;

        fn def_to(port: u16) -> SshConnDef {
            SshConnDef {
                id: "selfheal".into(),
                name: "selfheal".into(),
                host: "127.0.0.1".into(),
                port,
                username: "u".into(),
                auth_type: crate::tunnel::types::AuthType::Password,
                group: None,
                key_path: None,
                passphrase: None,
                password: Some("pw".into()),
                host_key: None,
                proxy: None,
                proxy_username: None,
                proxy_password: None,
                jump: None,
            }
        }

        async fn connected(def: &SshConnDef) -> Arc<SshConnection> {
            connected_with(def, SshHooks::default()).await
        }

        async fn connected_with(def: &SshConnDef, hooks: SshHooks) -> Arc<SshConnection> {
            let c = SshConnection::new(def.clone(), hooks);
            c.connect().await.expect("the test server accepts");
            c
        }

        /// Hooks whose on_wedged increments a shared counter, so a test asserts exactly
        /// when the judgment fires.
        fn wedged_hooks() -> (Arc<AtomicUsize>, SshHooks) {
            let fired = Arc::new(AtomicUsize::new(0));
            let counted = fired.clone();
            (
                fired,
                SshHooks {
                    on_wedged: Some(Arc::new(move || {
                        counted.fetch_add(1, Ordering::SeqCst);
                    })),
                    ..Default::default()
                },
            )
        }

        /// SPEC §tunnels.selfheal: an open the session never answers fails after the
        /// budget instead of hanging — the incident shape, where sshd answers keepalives
        /// and never confirms a channel open.
        #[tokio::test]
        async fn an_unanswered_open_fails_after_the_budget() {
            let (port, _policy) = spawn_opens_ssh_server(TestOpens::Wedge).await;
            let c = connected(&def_to(port)).await;
            c.set_open_budget(Duration::from_millis(250));
            let t0 = std::time::Instant::now();
            let err = match c.open_channel("127.0.0.1", 9).await {
                Err(err) => err,
                // ByteStream is not Debug, so expect_err is out; the Ok arm names the
                // failure better than the trait error would.
                Ok(_channel) => panic!("the wedged policy never answers; the open must fail"),
            };
            assert!(
                err.message.contains("timed out"),
                "the budget must name itself: {}",
                err.message
            );
            assert_eq!(err.kind, FailureKind::Network);
            assert!(
                t0.elapsed() >= Duration::from_millis(250),
                "the full budget must be spent, not a polite guess"
            );
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "a shrunk budget must not turn into a wall-clock hang"
            );
            let _ = c.end().await;
        }

        /// SPEC §tunnels.selfheal: a refusal is an answered open — it fails fast, names
        /// the channel, and is never a timeout.
        #[tokio::test]
        async fn a_refused_open_answers_rather_than_times_out() {
            let (port, _policy) = spawn_opens_ssh_server(TestOpens::Refuse).await;
            let c = connected(&def_to(port)).await;
            let t0 = std::time::Instant::now();
            let err = match c.open_channel("127.0.0.1", 9).await {
                Err(err) => err,
                Ok(_channel) => panic!("the refusing policy answers no; the open must fail"),
            };
            assert!(
                !err.message.contains("timed out"),
                "a refusal is an answer: {}",
                err.message
            );
            assert!(
                err.message.contains("channel"),
                "names the channel: {}",
                err.message
            );
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "an answered refusal is immediate, not budgeted"
            );
            let _ = c.end().await;
        }

        /// SPEC §tunnels.selfheal: three consecutive unanswered opens judge the session
        /// wedged; the hook fires on the open that completes the count, once per episode
        /// — a fourth timeout does not fire it again.
        #[tokio::test]
        async fn three_unanswered_opens_judge_the_session_wedged() {
            let (port, _policy) = spawn_opens_ssh_server(TestOpens::Wedge).await;
            let (fired, hooks) = wedged_hooks();
            let c = connected_with(&def_to(port), hooks).await;
            c.set_open_budget(Duration::from_millis(120));
            for i in 1..=3 {
                assert!(
                    c.open_channel("127.0.0.1", 9).await.is_err(),
                    "open {i} must fail on its budget"
                );
                assert_eq!(
                    fired.load(Ordering::SeqCst),
                    if i == 3 { 1 } else { 0 },
                    "the judgment fires with the third unanswered open"
                );
            }
            assert!(c.open_channel("127.0.0.1", 9).await.is_err());
            assert_eq!(fired.load(Ordering::SeqCst), 1, "fires once per episode");
            let _ = c.end().await;
        }

        /// SPEC §tunnels.selfheal: the judgment is consecutive — any answered open
        /// resets the count, so a session that recovers mid-count is never torn down.
        #[tokio::test]
        async fn an_answered_open_resets_the_wedge_count() {
            let (port, policy) = spawn_opens_ssh_server(TestOpens::Wedge).await;
            let (fired, hooks) = wedged_hooks();
            let c = connected_with(&def_to(port), hooks).await;
            c.set_open_budget(Duration::from_millis(120));
            // Two unanswered opens: one short of the judgment.
            for _ in 0..2 {
                assert!(c.open_channel("127.0.0.1", 9).await.is_err());
            }
            assert_eq!(fired.load(Ordering::SeqCst), 0);
            // An answered open — even one whose target never dials — proves the session
            // is alive and clears the count.
            *policy.lock().unwrap() = TestOpens::Forward;
            assert!(
                c.open_channel("127.0.0.1", 9).await.is_ok(),
                "the Forward policy answers the open; the target need not exist"
            );
            *policy.lock().unwrap() = TestOpens::Wedge;
            // Two more unanswered opens still do not complete a fresh count of three.
            for _ in 0..2 {
                assert!(c.open_channel("127.0.0.1", 9).await.is_err());
                assert_eq!(fired.load(Ordering::SeqCst), 0);
            }
            assert!(c.open_channel("127.0.0.1", 9).await.is_err());
            assert_eq!(
                fired.load(Ordering::SeqCst),
                1,
                "a count that restarted from zero completes at three"
            );
            let _ = c.end().await;
        }

        /// SPEC §tunnels.selfheal: a refusal is an answered open — a target that says
        /// no, however often, never judges its session wedged.
        #[tokio::test]
        async fn refusals_never_judge_a_session_wedged() {
            let (port, _policy) = spawn_opens_ssh_server(TestOpens::Refuse).await;
            let (fired, hooks) = wedged_hooks();
            let c = connected_with(&def_to(port), hooks).await;
            c.set_open_budget(Duration::from_millis(120));
            for _ in 0..5 {
                assert!(c.open_channel("127.0.0.1", 9).await.is_err());
            }
            assert_eq!(
                fired.load(Ordering::SeqCst),
                0,
                "every refusal is an answer; only silence counts"
            );
            let _ = c.end().await;
        }
    }
}
