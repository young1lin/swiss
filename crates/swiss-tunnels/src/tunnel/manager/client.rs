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

//! The slice of an SSH client the manager drives (`SshLike`), the real russh client's side of it,
//! and the factory that builds one client per connection definition.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use crate::tunnel::forward::ByteStream;
use crate::tunnel::ssh::{SshConnection, SshHooks};
use crate::tunnel::types::{ConnState, SshConnDef, TunnelError};
use super::lease::ConnectionLease;
use swiss_host::services::shell::{PtyEndpoint, PtySize};

/// One connection operation's future. The crate carries no `async-trait`, and boxing a future
/// per dial or channel open costs nothing beside the network round trip inside it.
pub(super) type ConnFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The slice of an SSH client this manager drives. `SshConnection` is the only implementor that
/// ships — the indirection exists so the tests can plug in a client whose channels are plain TCP
/// sockets, and run the whole rule lifecycle (sharing, transport loss, backoff) with no sshd
/// anywhere. It is the Node build's `SshLike` interface, which the manager took the same way.
pub trait SshLike: Send + Sync {
    fn id(&self) -> String;
    fn state(&self) -> ConnState;
    fn reason(&self) -> Option<String>;
    /// Adopt an edited definition without dropping the live session.
    fn set_def(&self, def: SshConnDef);
    /// How many rules hold this client. A plain field on `SshConnection`; a method here because
    /// a trait cannot have one.
    fn refs(&self) -> &AtomicI64;
    /// Dial, or return at once when already connected. Takes `Arc<Self>` because a real connect
    /// hands the client to a watcher task that outlives the call.
    fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>>;
    fn open_channel(
        &self,
        host: String,
        port: u16,
    ) -> ConnFuture<'_, Result<ByteStream, TunnelError>>;
    /// Open an interactive PTY and pump it against `endpoint` (SPEC §terminal.remote). Resolves when
    /// the shell has started, not when it ends.
    ///
    /// `hold` is this session's reference on the client, minted by
    /// [TunnelManager::open_shell]. Passing it in rather than letting the implementation
    /// mint its own is the point: reference counting has exactly one owner (`put_ref`
    /// below), and a second implementation of it would eventually disagree with the first
    /// — the way that disagreement shows up in the product is a terminal closing somebody
    /// else's tunnel.
    fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> ConnFuture<'_, Result<(), TunnelError>>;
    /// Run one non-interactive command to completion over a session channel
    /// (SPEC §remote.transport), streaming stdout/stderr into `events` as they arrive and
    /// honouring `cancel` by closing the channel. `hold` is the operation's
    /// reference on this client, owned by the returned future.
    fn exec(
        &self,
        request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<i32, TunnelError>>;
    /// Stat one remote path over SFTP; None when it does not exist.
    fn stat_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>;
    /// Open one remote path for streaming read. The returned reader owns `hold`.
    fn open_read(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>;
    /// Create/truncate one remote path for streaming write. The writer owns `hold`.
    fn create_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>;
    /// Create one remote path (and missing parents) over SFTP.
    fn mkdir_p(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>>;
    /// List one remote directory's immediate children over SFTP readdir.
    fn list_dir(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>>;
    /// Set one remote file's mtime (whole seconds since the epoch) over SFTP
    /// SETSTAT - sync's quick-check bookkeeping (SPEC §remote.actions).
    fn set_mtime(
        &self,
        path: String,
        mtime_secs: u64,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>>;
    fn end(&self) -> ConnFuture<'_, ()>;
}

impl SshLike for SshConnection {
    fn id(&self) -> String {
        SshConnection::id(self)
    }
    fn state(&self) -> ConnState {
        SshConnection::state(self)
    }
    fn reason(&self) -> Option<String> {
        SshConnection::reason(self)
    }
    fn set_def(&self, def: SshConnDef) {
        SshConnection::set_def(self, def);
    }
    fn refs(&self) -> &AtomicI64 {
        &self.refs
    }
    fn connect(self: Arc<Self>) -> ConnFuture<'static, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::connect(&self).await })
    }
    fn open_channel(
        &self,
        host: String,
        port: u16,
    ) -> ConnFuture<'_, Result<ByteStream, TunnelError>> {
        Box::pin(async move { SshConnection::open_channel(self, &host, port).await })
    }
    fn open_shell(
        &self,
        size: PtySize,
        endpoint: PtyEndpoint,
        hold: Box<dyn Send + 'static>,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::open_shell(self, size, endpoint, hold).await })
    }
    /// Run one non-interactive command to completion over a session channel
    /// (SPEC §remote.transport), streaming stdout/stderr into `events` as they arrive and
    /// honouring `cancel` by closing the channel. `hold` is the operation's
    /// reference on this client, owned by the returned future.
    fn exec(
        &self,
        request: swiss_host::services::remote::RemoteExecRequest,
        events: tokio::sync::mpsc::Sender<swiss_host::services::remote::RemoteExecEvent>,
        cancel: swiss_host::services::action::CancelHandle,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<i32, TunnelError>> {
        Box::pin(async move { SshConnection::exec(self, request, events, cancel, hold).await })
    }
    /// Stat one remote path over SFTP; None when it does not exist.
    fn stat_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Option<swiss_host::services::remote::RemoteFileStat>, TunnelError>>
    {
        Box::pin(async move { SshConnection::stat_file(self, path, hold).await })
    }
    /// Open one remote path for streaming read. The returned reader owns `hold`.
    fn open_read(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteRead>, TunnelError>>
    {
        Box::pin(async move { SshConnection::open_read(self, path, hold).await })
    }
    /// Create/truncate one remote path for streaming write. The writer owns `hold`.
    fn create_file(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Box<dyn swiss_host::services::remote::RemoteWrite>, TunnelError>>
    {
        Box::pin(async move { SshConnection::create_file(self, path, hold).await })
    }
    /// Create one remote path (and missing parents) over SFTP.
    fn mkdir_p(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::mkdir_p(self, path, hold).await })
    }
    /// List one remote directory's immediate children over SFTP readdir.
    fn list_dir(
        &self,
        path: String,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<Vec<swiss_host::services::remote::RemoteListing>, TunnelError>> {
        Box::pin(async move { SshConnection::list_dir(self, path, hold).await })
    }
    /// Set one remote file's mtime over SFTP SETSTAT.
    fn set_mtime(
        &self,
        path: String,
        mtime_secs: u64,
        hold: ConnectionLease,
    ) -> ConnFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move { SshConnection::set_mtime(self, path, mtime_secs, hold).await })
    }
    fn end(&self) -> ConnFuture<'_, ()> {
        Box::pin(SshConnection::end(self))
    }
}

/// Builds the client for one connection definition, with the manager's hooks already attached.
pub(super) type ConnFactory = Arc<dyn Fn(&SshConnDef, SshHooks) -> Arc<dyn SshLike> + Send + Sync>;

/// The factory every non-test caller gets: a real russh client.
pub(super) fn real_connections() -> ConnFactory {
    Arc::new(|def: &SshConnDef, hooks: SshHooks| {
        SshConnection::new(def.clone(), hooks) as Arc<dyn SshLike>
    })
}
