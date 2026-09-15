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

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// Callbacks the manager registers on a live connection.
#[derive(Default)]
pub struct SshHooks {
    /// A fingerprint was learned (first connect) — persist it.
    pub on_host_key: Option<HostKeyCallback>,
    /// The transport died while connected. The manager releases ports and decides about retrying.
    pub on_lost: Option<Arc<dyn Fn(TunnelError) + Send + Sync>>,
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
}

pub struct SshConnection {
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
            let mut handle = russh::client::connect(config, (def_host.as_str(), def_port), handler)
                .await
                .map_err(|err| as_tunnel_error(err, None))?;
            self.authenticate(&mut handle, &def, key).await?;
            Ok::<Handle<ClientHandler>, TunnelError>(handle)
        })
        .await;

        let handle = match result {
            Ok(Ok(handle)) => handle,
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
        });
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
                // Vault refs are strict at the connect boundary (docs/19 D4): a missing
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
        let channel = handle
            .channel_open_direct_tcpip(host.to_string(), port as u32, "127.0.0.1".to_string(), 0)
            .await
            .map_err(|err| {
                as_tunnel_error(err, Some(&format!("channel to {host}:{port} failed")))
            })?;
        Ok(Box::pin(channel.into_stream()))
    }

    /// Open an interactive PTY on this connection and pump it against `endpoint`.
    ///
    /// Resolves once the shell has STARTED, not when it ends: the pump runs in its own
    /// task and `hold` — the manager's reference on this client — travels with it, so the
    /// connection is released exactly when the session is over and not a moment before.
    ///
    /// russh has had PTY channels all along; no new SSH dependency is involved (docs/14
    /// §4). What is new is the shape of the plumbing on this side of it.
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

    /// End the session (best effort, capped): a wedged transport must not block a shutdown.
    pub async fn end(&self) {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.set_state(ConnState::Idle, None);
        *self.banner.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let Some(live) = live else { return };
        live.intentional.store(true, Ordering::SeqCst);
        let _ = live
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        let mut closed = live.closed;
        let _ = tokio::time::timeout(END_TIMEOUT, closed.changed()).await;
    }

    /// Connect, authenticate and disconnect, on a THROWAWAY client.
    ///
    /// Never the live one: Test has to prove the credentials and the host key end to end, which
    /// is exactly what a probe on an already-authenticated session cannot do. Note the throwaway
    /// runs WITHOUT the persistence hook — a successful Test of a keyless connection stores
    /// nothing (the first real connect is what learns and persists the fingerprint).
    pub async fn test(def: &SshConnDef) -> TestResult {
        let t0 = std::time::Instant::now();
        let probe = SshConnection::new(def.clone(), SshHooks::default());
        let result = probe.connect().await;
        let banner = probe.banner();
        if let Err(err) = result {
            let mut out = TestResult {
                ok: false,
                ms: t0.elapsed().as_millis() as u64,
                error: Some(err.message.clone()),
                kind: Some(err.kind.as_str().to_string()),
                fingerprint: None,
                banner,
            };
            if let Some(detail) = &err.detail {
                if let Some(actual) = detail.get("actual").and_then(Value::as_str) {
                    out.fingerprint = Some(actual.to_string());
                }
            }
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
            fingerprint: None,
        }
    }
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
    // Same strict connect-time contract as the password (docs/19 D4).
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
    /// Set on a host-key mismatch, so the panel can offer "trust this key".
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

// --- interactive shell plumbing (docs/14 T2) -------------------------------------------------

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
/// this process buffering an unbounded flood (docs/14 §6.8).
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
/// output pump is parked on a full queue still reaches the far side (docs/14 §10 item 7).
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
        };
        let err = read_key(&def).unwrap_err();
        assert_eq!(err.kind, FailureKind::Config);
        assert!(err.message.starts_with("cannot read private key"));
    }

    #[test]
    fn a_missing_vault_passphrase_reference_refuses_before_the_dial() {
        // docs/19 D4 at the tunnel surface: a passphrase reference this machine does not hold
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
}
