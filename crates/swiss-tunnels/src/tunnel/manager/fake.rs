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

//! The fake SSH client the manager's tests drive. Ported from the Node build's
//! `test/tunnel-manager.test.ts`, one case per `it(...)`.
//!
//! The whole rule lifecycle runs with no sshd anywhere: the manager builds `FakeConn`
//! instead of `SshConnection` (see `SshLike`), and a fake channel is a plain TCP socket to a
//! local echo server. So "the tunnel carries traffic" is asserted by sending bytes through
//! the bound local port and reading them back, exactly as a user would.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::net::TcpStream;

use crate::tunnel::forward::ByteStream;
use crate::tunnel::ssh::SshHooks;
use crate::tunnel::types::{ConnState, FailureKind, SshConnDef, TunnelError};
use super::client::{ConnFuture, SshLike};
use super::lease::ConnectionLease;
use swiss_host::services::shell::{PtyEndpoint, PtyEvent, PtyInput, PtySize};

/// Every client one manager built, newest last — the Node suite's `built` array, but scoped
/// to a single manager so tests never see each other's connections.
pub(super) type Built = Arc<Mutex<Vec<Arc<FakeConn>>>>;

/// A stand-in for `SshConnection`: `open_channel` dials the echo server over loopback.
pub(super) struct FakeConn {
    def: Mutex<SshConnDef>,
    hooks: SshHooks,
    refs: AtomicI64,
    state: Mutex<ConnState>,
    reason: Mutex<Option<String>>,
    /// Dials that actually reached the "server" — a connect on a live client is not one.
    dials: AtomicU32,
    ended: AtomicU32,
    /// Interactive shells opened on this client (SPEC §terminal.remote).
    shells: AtomicU32,
    /// Non-interactive execs run on this client (SPEC §remote).
    pub(super) execs: AtomicU32,
    /// The exit status the next fake exec reports.
    pub(super) exec_exit: AtomicI32,
    /// Bytes the next fake exec streams to stdout before exiting.
    pub(super) exec_output: Mutex<Vec<u8>>,
    /// The in-memory remote filesystem the fake file ops serve.
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Per-file mtimes (whole seconds) the set-mtime op records and stat serves.
    mtimes: Arc<Mutex<HashMap<String, u64>>>,
    /// Set to make every shell request fail - a server with PTYs disabled.
    pub(super) refuse_shell: AtomicBool,
    /// Set to make every connect fail with this error.
    pub(super) fail_with: Mutex<Option<TunnelError>>,
}

impl FakeConn {
    pub(super) fn new(def: &SshConnDef, hooks: SshHooks) -> Arc<FakeConn> {
        Arc::new(FakeConn {
            def: Mutex::new(def.clone()),
            hooks,
            refs: AtomicI64::new(0),
            state: Mutex::new(ConnState::Idle),
            reason: Mutex::new(None),
            dials: AtomicU32::new(0),
            ended: AtomicU32::new(0),
            shells: AtomicU32::new(0),
            execs: AtomicU32::new(0),
            exec_exit: AtomicI32::new(0),
            exec_output: Mutex::new(Vec::new()),
            files: Arc::new(Mutex::new(HashMap::new())),
            mtimes: Arc::new(Mutex::new(HashMap::new())),
            refuse_shell: AtomicBool::new(false),
            fail_with: Mutex::new(None),
        })
    }

    pub(super) fn dials(&self) -> u32 {
        self.dials.load(Ordering::SeqCst)
    }

    pub(super) fn ended(&self) -> u32 {
        self.ended.load(Ordering::SeqCst)
    }

    pub(super) fn ref_count(&self) -> i64 {
        self.refs.load(Ordering::SeqCst)
    }

    pub(super) fn shells(&self) -> u32 {
        self.shells.load(Ordering::SeqCst)
    }

    pub(super) fn username(&self) -> String {
        self.def.lock().unwrap().username.clone()
    }

    pub(super) fn is_connected(&self) -> bool {
        *self.state.lock().unwrap() == ConnState::Connected
    }

    /// The same contract as the real client's: store, then fire on_state (SPEC §host.seats).
    fn set_state(&self, state: ConnState, reason: Option<String>) {
        *self.state.lock().unwrap() = state;
        *self.reason.lock().unwrap() = reason;
        if let Some(on_state) = &self.hooks.on_state {
            on_state(state);
        }
    }

    /// The transport dies under whatever rules are riding it.
    pub(super) fn die(&self, kind: FailureKind) {
        self.set_state(ConnState::Error, None);
        if let Some(on_lost) = &self.hooks.on_lost {
            on_lost(TunnelError::new(
                format!("ssh connection lost: {} failure", kind.as_str()),
                kind,
            ));
        }
    }

    /// SPEC §tunnels.selfheal: channel opens stopped being answered while the transport
    /// stays up — unlike die(), the state is still Connected, which is the whole shape.
    /// The manager must end this client and run the loss path for every rule riding it.
    pub(super) fn wedge(&self) {
        if let Some(on_wedged) = &self.hooks.on_wedged {
            on_wedged();
        }
    }
}

impl SshLike for FakeConn {
    fn id(&self) -> String {
        self.def.lock().unwrap().id.clone()
    }
    fn state(&self) -> ConnState {
        *self.state.lock().unwrap()
    }
    fn reason(&self) -> Option<String> {
        self.reason.lock().unwrap().clone()
    }
    fn set_def(&self, def: SshConnDef) {
        *self.def.lock().unwrap() = def;
    }
    fn refs(&self) -> &AtomicI64 {
        &self.refs
    }
    fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>> {
        Box::pin(async move {
            if self.is_connected() {
                return Ok(());
            }
            self.dials.fetch_add(1, Ordering::SeqCst);
            let failure = self.fail_with.lock().unwrap().clone();
            if let Some(err) = failure {
                self.set_state(ConnState::Error, Some(err.message.clone()));
                return Err(err);
            }
            self.set_state(ConnState::Connected, None);
            Ok(())
        })
    }
    fn open_channel(
        &self,
        host: String,
        port: u16,
    ) -> ConnFuture<'_, Result<ByteStream, TunnelError>> {
        Box::pin(async move {
            if !self.is_connected() {
                return Err(TunnelError::new("not established", FailureKind::Network));
            }
            let stream = TcpStream::connect((host.as_str(), port))
                .await
                .map_err(|err| TunnelError::new(err.to_string(), FailureKind::Network))?;
            Ok(Box::pin(stream) as ByteStream)
        })
    }
    fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move {
            if !self.is_connected() {
                return Err(TunnelError::new("not established", FailureKind::Network));
            }
            if self.refuse_shell.load(Ordering::SeqCst) {
                return Err(TunnelError::new(
                    "the server refused a pseudo-terminal",
                    FailureKind::Network,
                ));
            }
            self.shells.fetch_add(1, Ordering::SeqCst);
            // The same shape as the real pump: two directions, the reference travelling
            // with the task, and the task ending when either end goes away.
            tokio::spawn(async move {
                let _hold = hold;
                let (out, mut input) = endpoint.split();
                let greeting = format!("{}x{}$ ", size.cols, size.rows).into_bytes();
                if out.send(PtyEvent::Data(greeting)).await.is_err() {
                    return;
                }
                loop {
                    let echo = tokio::select! {
                        next = input.next() => match next {
                            Some(PtyInput::Data(bytes)) => bytes,
                            Some(PtyInput::Resize(s)) => {
                                format!("{}x{}", s.cols, s.rows).into_bytes()
                            }
                            None => break,
                        },
                        _ = out.closed() => break,
                    };
                    if out.send(PtyEvent::Data(echo)).await.is_err() {
                        return;
                    }
                }
                let _ = out.send(PtyEvent::Exit { code: Some(0) }).await;
            });
            Ok(())
        })
    }
    fn end(&self) -> ConnFuture<'_, ()> {
        Box::pin(async move {
            self.ended.fetch_add(1, Ordering::SeqCst);
            self.set_state(ConnState::Idle, None);
        })
    }
    fn exec(
        &self,
        _request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<i32, TunnelError>> {
        Box::pin(async move {
            if !self.is_connected() {
                return Err(TunnelError::new("not established", FailureKind::Network));
            }
            self.execs.fetch_add(1, Ordering::SeqCst);
            // Stream the canned output in small pieces, parking between them, so
            // a test can exercise mid-run observation and cancel.
            let payload = self.exec_output.lock().unwrap().clone();
            for chunk in payload.chunks(7) {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        drop(hold);
                        return Err(TunnelError::new(
                            "the remote command was canceled".to_string(),
                            FailureKind::Canceled,
                        ));
                    }
                    sent = events.send(
                        swiss_host::services::remote::RemoteExecEvent::Stdout(chunk.to_vec()),
                    ) => {
                        if sent.is_err() {
                            drop(hold);
                            return Err(TunnelError::new(
                                "the run reading this output went away".to_string(),
                                FailureKind::Canceled,
                            ));
                        }
                    }
                }
            }
            drop(hold);
            Ok(self.exec_exit.load(Ordering::SeqCst))
        })
    }
    fn stat_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>
    {
        Box::pin(async move {
            drop(hold);
            Ok(self.files.lock().unwrap().get(&path).map(|b| {
                // A recorded mtime reads back; without one the file keeps the
                // fake's traditional fixed timestamp.
                let secs = self.mtimes.lock().unwrap().get(&path).copied();
                swiss_host::services::remote::RemoteFileStat {
                    size: b.len() as u64,
                    mtime_ms: Some(secs.unwrap_or(1).saturating_mul(1000)),
                    is_dir: false,
                }
            }))
        })
    }
    fn open_read(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>
    {
        Box::pin(async move {
            let bytes = self
                .files
                .lock()
                .unwrap()
                .get(&path)
                .cloned()
                .ok_or_else(|| {
                    TunnelError::new(format!("no such file: {path}"), FailureKind::Network)
                })?;
            Ok(Box::new(FakeFileRead {
                bytes,
                at: 0,
                _hold: Some(hold),
            })
                as Box<dyn swiss_host::services::remote::RemoteRead>)
        })
    }
    fn create_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>
    {
        Box::pin(async move {
            Ok(Box::new(FakeFileWrite {
                files: self.files.clone(),
                path,
                buf: Vec::new(),
                hold: Some(hold),
            })
                as Box<dyn swiss_host::services::remote::RemoteWrite>)
        })
    }
    fn mkdir_p(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move {
            drop(hold);
            self.files
                .lock()
                .unwrap()
                .insert(format!("{path}/"), Vec::new());
            Ok(())
        })
    }
    fn set_mtime(
        &self,
        path: String,
        mtime_secs: u64,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move {
            drop(hold);
            self.mtimes.lock().unwrap().insert(path, mtime_secs);
            Ok(())
        })
    }
    fn list_dir(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>>
    {
        Box::pin(async move {
            drop(hold);
            // The map holds files (and "path/" markers from mkdir); the
            // immediate children of the listed path are the first segments
            // of deeper keys.
            let files = self.files.lock().unwrap();
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let mut names: Vec<String> = Vec::new();
            for key in files.keys() {
                let Some(rest) = key.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                if rest.is_empty() {
                    continue; // the directory's own marker, not a child
                }
                let name = rest.split('/').next().unwrap_or_default().to_string();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
            names.sort();
            Ok(names
                .into_iter()
                .map(|name| {
                    let child = format!("{prefix}{name}");
                    swiss_host::services::remote::RemoteListing {
                        is_dir: files.keys().any(|k| k.starts_with(&format!("{child}/"))),
                        size: files.get(&child).map(|b| b.len() as u64).unwrap_or(0),
                        name,
                    }
                })
                .collect())
        })
    }
}

/// The fake reader/writer halves behind the fake SFTP - chunked, lease-owning,
/// shaped exactly like the real ones in ssh.rs.
struct FakeFileRead {
    bytes: Vec<u8>,
    at: usize,
    _hold: Option<ConnectionLease>,
}

#[async_trait]
impl swiss_host::services::remote::RemoteRead for FakeFileRead {
    async fn read_chunk(
        &mut self,
    ) -> Result<Option<Vec<u8>>, swiss_host::services::remote::RemoteError> {
        if self.at >= self.bytes.len() {
            self._hold.take(); // release once the stream truly ends
            return Ok(None);
        }
        let end =
            (self.at + swiss_host::services::remote::REMOTE_CHUNK_BYTES).min(self.bytes.len());
        let chunk = self.bytes[self.at..end].to_vec();
        self.at = end;
        Ok(Some(chunk))
    }
}

struct FakeFileWrite {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    path: String,
    buf: Vec<u8>,
    hold: Option<ConnectionLease>,
}

#[async_trait]
impl swiss_host::services::remote::RemoteWrite for FakeFileWrite {
    async fn write_chunk(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), swiss_host::services::remote::RemoteError> {
        self.buf.extend_from_slice(bytes);
        Ok(())
    }
    async fn finish(&mut self) -> Result<(), swiss_host::services::remote::RemoteError> {
        self.files
            .lock()
            .unwrap()
            .insert(self.path.clone(), std::mem::take(&mut self.buf));
        self.hold.take(); // commit done, connection reference back
        Ok(())
    }
}
