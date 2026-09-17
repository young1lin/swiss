/*
 * Copyright 2026 The swiss authors
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

//! The remote-execution transport capability — who can run a non-interactive command
//! (or move a file) on a remote machine, and under what identity (docs/32).
//!
//! Same shape as [crate::services::catalog] and [crate::services::shell]: ONE provider
//! registers on start, consumers take operation-scoped leases through the provider, and
//! a provider stopping withdraws (no new operations) before it closes. The remote plugin
//! never links an SSH client; the tunnels plugin never learns that remote execution
//! exists. ADR-010 forbids an edge between two subsystem crates, so a need like this one
//! is read as a MISSING HOST CONTRACT, not as a reason to add the edge.
//!
//! What the consumer deliberately does NOT get:
//!
//! - **Credentials, ever.** No password, no passphrase, no private key, no host and no
//!   username crosses this seam. [RemoteEndpoint] carries an opaque id, a display label
//!   and a connection state — that is the whole identity an agent-facing surface needs.
//!   The endpoint id RESOLVES to a real connection only inside the provider.
//! - **A russh handle.** [RemoteExecRequest] is argv-shaped (never a raw shell string):
//!   how the argv becomes a command line is the provider's translation problem, which is
//!   what keeps a future Docker or WinRM provider honest about argument boundaries.
//! - **Unbounded output.** Events stream through a bounded channel the CALLER owns; when
//!   the caller falls behind, the provider's sends park and the transport's own window
//!   applies backpressure. There is no read-to-end anywhere in this contract.
//!
//! What differs from the shell capability, and why: an exec is a RUN, not a session. It
//! has a beginning (the request), an end (the exit status) and a deadline; its lifetime
//! is owned by the run coordinator, not by an attached tab. So the contract takes a
//! [CancelHandle] and a bounded event sender instead of returning a duplex, and there is
//! no session ledger to drain — a provider stopping withdraws, and the in-flight
//! operations fail with a transport error when the underlying connections close, which
//! the runs surface reports honestly.

use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::services::action::CancelHandle;

/// One remote machine an operation can be sent to. Identity is the provider's connection
/// id (for tunnels: the SshConnDef id) — two servers that merely share host and port are
/// NOT the same endpoint, exactly as [crate::services::shell::ShellTarget] insists.
#[derive(Clone, Debug)]
pub struct RemoteEndpoint {
    pub id: String,
    pub label: String,
    /// The provider's live connection state ("connected", "idle", ...), so callers can
    /// grey a target out honestly instead of discovering the dial failure later.
    pub state: String,
}

/// One non-interactive command, argv-shaped on purpose. SSH eventually needs a command
/// STRING, and translating argv -> string is the provider's job (quoting included); a
/// contract that accepted a raw string would let that translation drift per caller and
/// quietly reintroduce shell-injection into "just run this build".
#[derive(Clone, Debug)]
pub struct RemoteExecRequest {
    /// The argv to run; argv[0] is the program. Length >= 1, boundaries preserved.
    pub argv: Vec<String>,
    /// Extra environment for the command, applied by the provider's translation.
    pub env: Vec<(String, String)>,
    /// Absolute working directory on the remote machine, when the caller resolved one.
    /// The CALLER owns workspace guardrails; the provider just cds.
    pub cwd: Option<String>,
}

/// One piece of streaming output from a running exec. The exit status is the exec's
/// return value, not an event, so a consumer that stops reading early can still learn
/// how the command ended from the Result.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteExecEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// How an exec ended when it ended at all.
#[derive(Clone, Copy, Debug)]
pub struct RemoteExecResult {
    /// The remote command's exit status.
    pub exit_code: i32,
}

/// How much output may sit between the provider and the consumer before the provider's
/// sends park. Bounded on purpose (docs/32 §memory): a build that floods must slow the
/// transport down, not buffer here.
pub const EXEC_EVENT_QUEUE: usize = 64;

/// Why a remote operation refused or failed. User-facing messages, the catalog/shell
/// error shape.
#[derive(Debug)]
pub enum RemoteError {
    /// No such endpoint id.
    Unknown(String),
    /// No provider is registered — "the tunnels plugin is not running", not "no hosts".
    Unavailable(String),
    /// The provider is shutting down and will not start new operations.
    Withdrawing(String),
    /// The provider cannot do this kind of operation (e.g. no file transport).
    Unsupported(String),
    /// The operation was cancelled through its handle; partial output was delivered.
    Canceled(String),
    /// The operation started and then failed (transport loss, remote refusal, ...).
    Failed(String),
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteError::Unknown(m)
            | RemoteError::Unavailable(m)
            | RemoteError::Withdrawing(m)
            | RemoteError::Unsupported(m)
            | RemoteError::Canceled(m)
            | RemoteError::Failed(m) => f.write_str(m),
        }
    }
}

/// The stat a sync needs to decide "changed?": size plus a coarse modify time. Nothing
/// here is a credential or even a name — the caller owns the paths it asks about.
#[derive(Clone, Copy, Debug)]
pub struct RemoteFileStat {
    pub size: u64,
    /// Milliseconds since the epoch, when the provider can see it.
    pub mtime_ms: Option<u64>,
    pub is_dir: bool,
}

/// How big one file chunk may be on this seam. Chunked (not stream-object) on purpose:
/// a Vec-per-chunk with a fixed ceiling is the smallest interface both a real SFTP
/// transport and a test fake can implement, and it caps the per-operation memory by
/// construction — a 2 GB firmware file never appears whole anywhere.
pub const REMOTE_CHUNK_BYTES: usize = 64 * 1024;

/// The reading half of one remote file. `None` ends the stream. The implementation
/// holds whatever keeps the transport alive for as long as the read does.
#[async_trait]
pub trait RemoteRead: Send {
    async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, RemoteError>;
}

/// The writing half of one remote file. `finish` flushes and closes; dropping without
/// finishing is an abort, not a commit.
#[async_trait]
pub trait RemoteWrite: Send {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), RemoteError>;
    async fn finish(&mut self) -> Result<(), RemoteError>;
}

/// What a provider registers: the machines it can reach, and how to run on them.
#[async_trait]
pub trait RemoteTransportProvider: Send + Sync {
    fn list(&self) -> Vec<RemoteEndpoint>;

    /// Whether the file operations are usable. An exec-only provider answers false and
    /// the file routes answer [RemoteError::Unsupported] — capabilities are per target,
    /// but the mechanism is per provider.
    fn supports_files(&self) -> bool;

    /// Run one command to completion, streaming its output into `events` as it is
    /// produced. `cancel` must be honoured by stopping the read, closing the transport
    /// channel, waiting a bounded period, and returning [RemoteError::Canceled] — never
    /// by simply hoping the caller drops the future.
    async fn exec(
        &self,
        endpoint: &str,
        holder: &str,
        request: RemoteExecRequest,
        events: mpsc::Sender<RemoteExecEvent>,
        cancel: CancelHandle,
    ) -> Result<RemoteExecResult, RemoteError>;

    /// Stat one absolute remote path; None when it does not exist.
    async fn stat(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Option<RemoteFileStat>, RemoteError>;

    /// Open one absolute remote path for streaming read.
    async fn open_read(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteRead>, RemoteError>;

    /// Create/truncate one absolute remote path for streaming write. Parent directories
    /// are the caller's business ([RemoteTransportProvider::mkdir_p]).
    async fn create(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteWrite>, RemoteError>;

    /// Create one absolute remote path and any missing parents (idempotent).
    async fn mkdir_p(&self, endpoint: &str, holder: &str, path: &str) -> Result<(), RemoteError>;
}

/// Whether the transport capability can serve right now, and if not, who is missing —
/// the same three states as the catalog and shell presences, kept a separate type for
/// the same reason those two are separate (docs/14 §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemotePresence {
    Serving(String),
    Stopping(String),
    Absent(Option<String>),
}

impl RemotePresence {
    pub fn as_str(&self) -> &'static str {
        match self {
            RemotePresence::Serving(_) => "serving",
            RemotePresence::Stopping(_) => "stopping",
            RemotePresence::Absent(_) => "absent",
        }
    }
}

/// The single-provider registry, the catalog/shell shape once more. One remote
/// transport per process: the day a second one exists, this grows a per-capability map
/// — not a silent overwrite.
#[derive(Default)]
pub struct RemoteTransportRegistry {
    state: Mutex<RemoteState>,
}

#[derive(Default)]
struct RemoteState {
    provider: Option<Arc<dyn RemoteTransportProvider>>,
    provider_id: Option<String>,
    last_provider_id: Option<String>,
    withdrawing: bool,
}

impl RemoteTransportRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the one provider. Err names the incumbent when the seat is taken — even
    /// a withdrawing one, because the withdraw still owns the seat until
    /// [RemoteTransportRegistry::clear].
    pub fn register(
        &self,
        provider: Arc<dyn RemoteTransportProvider>,
        provider_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = &state.provider_id {
            return Err(format!(
                "the remote transport capability is already provided by plugin {current}"
            ));
        }
        state.provider = Some(provider);
        state.provider_id = Some(provider_id.to_string());
        state.last_provider_id = Some(provider_id.to_string());
        state.withdrawing = false;
        Ok(())
    }

    /// A provider's stop, first half: no new operations go out. In-flight operations
    /// are NOT drained here: an exec is a run with a deadline, and the provider's
    /// connections closing turns any survivor into an honestly-failed run.
    pub fn begin_withdraw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.provider.is_some() {
            state.withdrawing = true;
        }
    }

    /// A provider's stop, last half: the seat is free for the next start.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.provider = None;
        state.provider_id = None;
        state.withdrawing = false;
    }

    /// The live provider snapshot for an operation call, with the withdraw gate applied.
    fn provider(&self) -> Result<Arc<dyn RemoteTransportProvider>, RemoteError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match (&state.provider, state.withdrawing) {
            (None, _) => Err(RemoteError::Unavailable(format!(
                "no plugin currently provides remote execution{}",
                state
                    .last_provider_id
                    .as_deref()
                    .map(|id| format!(" (the {id} plugin provided it before)"))
                    .unwrap_or_default()
            ))),
            (Some(_), true) => Err(RemoteError::Withdrawing(format!(
                "plugin {} is stopping; no new remote operations",
                state.provider_id.as_deref().unwrap_or("?")
            ))),
            (Some(provider), false) => Ok(provider.clone()),
        }
    }

    pub fn list(&self) -> Vec<RemoteEndpoint> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .provider
            .as_ref()
            .map(|p| p.list())
            .unwrap_or_default()
    }

    /// Serving state + who is missing, for 503 messages and capability probes.
    pub fn presence(&self) -> RemotePresence {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match (&state.provider_id, state.withdrawing) {
            (Some(id), false) => RemotePresence::Serving(id.clone()),
            (Some(id), true) => RemotePresence::Stopping(id.clone()),
            (None, _) => RemotePresence::Absent(state.last_provider_id.clone()),
        }
    }

    /// Whether a provider is registered and serving — what a capability probe answers.
    pub fn has_provider(&self) -> bool {
        matches!(self.presence(), RemotePresence::Serving(_))
    }

    pub async fn exec(
        &self,
        endpoint: &str,
        holder: &str,
        request: RemoteExecRequest,
        events: mpsc::Sender<RemoteExecEvent>,
        cancel: CancelHandle,
    ) -> Result<RemoteExecResult, RemoteError> {
        let provider = self.provider()?;
        // Called OUTSIDE the registry lock: the provider locks its own state, and a
        // lock-order inversion between the two is what the borrow checker will not catch.
        provider
            .exec(endpoint, holder, request, events, cancel)
            .await
    }

    pub async fn stat(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Option<RemoteFileStat>, RemoteError> {
        let provider = self.files_provider()?;
        provider.stat(endpoint, holder, path).await
    }

    pub async fn open_read(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteRead>, RemoteError> {
        let provider = self.files_provider()?;
        provider.open_read(endpoint, holder, path).await
    }

    pub async fn create(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
        let provider = self.files_provider()?;
        provider.create(endpoint, holder, path).await
    }

    pub async fn mkdir_p(
        &self,
        endpoint: &str,
        holder: &str,
        path: &str,
    ) -> Result<(), RemoteError> {
        let provider = self.files_provider()?;
        provider.mkdir_p(endpoint, holder, path).await
    }

    fn files_provider(&self) -> Result<Arc<dyn RemoteTransportProvider>, RemoteError> {
        let provider = self.provider()?;
        if !provider.supports_files() {
            return Err(RemoteError::Unsupported(
                "this remote transport has no file channel (no SFTP)".to_string(),
            ));
        }
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::action::CancelSource;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The one-connection stub every registry test uses: an endpoint, an exec that
    /// streams two chunks then exits, and an in-memory file (Vec<u8>) standing in for
    /// the whole SFTP side.
    struct StubTransport {
        execs: AtomicUsize,
        files: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    }

    impl StubTransport {
        fn new() -> Arc<Self> {
            Arc::new(StubTransport {
                execs: AtomicUsize::new(0),
                files: Arc::new(Mutex::new(std::collections::HashMap::new())),
            })
        }
    }

    #[async_trait]
    impl RemoteTransportProvider for StubTransport {
        fn list(&self) -> Vec<RemoteEndpoint> {
            vec![RemoteEndpoint {
                id: "build-01".into(),
                label: "build server".into(),
                state: "connected".into(),
            }]
        }

        fn supports_files(&self) -> bool {
            true
        }

        async fn exec(
            &self,
            endpoint: &str,
            _holder: &str,
            request: RemoteExecRequest,
            events: mpsc::Sender<RemoteExecEvent>,
            _cancel: CancelHandle,
        ) -> Result<RemoteExecResult, RemoteError> {
            match endpoint {
                "build-01" => {
                    self.execs.fetch_add(1, Ordering::SeqCst);
                    events
                        .send(RemoteExecEvent::Stdout(
                            format!("argv={}", request.argv.join("\u{1f}")).into_bytes(),
                        ))
                        .await
                        .map_err(|_| RemoteError::Failed("consumer gone".into()))?;
                    events
                        .send(RemoteExecEvent::Stderr(b"warn".to_vec()))
                        .await
                        .map_err(|_| RemoteError::Failed("consumer gone".into()))?;
                    Ok(RemoteExecResult { exit_code: 0 })
                }
                other => Err(RemoteError::Unknown(format!(
                    "unknown remote endpoint: {other}"
                ))),
            }
        }

        async fn stat(
            &self,
            endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Option<RemoteFileStat>, RemoteError> {
            if endpoint != "build-01" {
                return Err(RemoteError::Unknown(format!(
                    "unknown remote endpoint: {endpoint}"
                )));
            }
            Ok(self
                .files
                .lock()
                .unwrap()
                .get(path)
                .map(|bytes| RemoteFileStat {
                    size: bytes.len() as u64,
                    mtime_ms: Some(1_000),
                    is_dir: false,
                }))
        }

        async fn open_read(
            &self,
            endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Box<dyn RemoteRead>, RemoteError> {
            if endpoint != "build-01" {
                return Err(RemoteError::Unknown(format!(
                    "unknown remote endpoint: {endpoint}"
                )));
            }
            let bytes = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| RemoteError::Failed(format!("no such file: {path}")))?;
            Ok(Box::new(ChunkReader { bytes, at: 0 }))
        }

        async fn create(
            &self,
            endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
            if endpoint != "build-01" {
                return Err(RemoteError::Unknown(format!(
                    "unknown remote endpoint: {endpoint}"
                )));
            }
            Ok(Box::new(MapWriter {
                files: self.files.clone(),
                path: path.to_string(),
                buf: Vec::new(),
            }))
        }

        async fn mkdir_p(
            &self,
            endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<(), RemoteError> {
            if endpoint != "build-01" {
                return Err(RemoteError::Unknown(format!(
                    "unknown remote endpoint: {endpoint}"
                )));
            }
            self.files
                .lock()
                .unwrap()
                .insert(format!("{path}/"), Vec::new());
            Ok(())
        }
    }

    /// An exec-only transport: the file routes must answer Unsupported, not pretend.
    struct ExecOnlyTransport;

    #[async_trait]
    impl RemoteTransportProvider for ExecOnlyTransport {
        fn list(&self) -> Vec<RemoteEndpoint> {
            Vec::new()
        }
        fn supports_files(&self) -> bool {
            false
        }
        async fn exec(
            &self,
            _endpoint: &str,
            _holder: &str,
            _request: RemoteExecRequest,
            _events: mpsc::Sender<RemoteExecEvent>,
            _cancel: CancelHandle,
        ) -> Result<RemoteExecResult, RemoteError> {
            Ok(RemoteExecResult { exit_code: 0 })
        }
        async fn stat(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<RemoteFileStat>, RemoteError> {
            Err(RemoteError::Unsupported("no files".into()))
        }
        async fn open_read(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Box<dyn RemoteRead>, RemoteError> {
            Err(RemoteError::Unsupported("no files".into()))
        }
        async fn create(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
            Err(RemoteError::Unsupported("no files".into()))
        }
        async fn mkdir_p(&self, _: &str, _: &str, _: &str) -> Result<(), RemoteError> {
            Err(RemoteError::Unsupported("no files".into()))
        }
    }

    struct ChunkReader {
        bytes: Vec<u8>,
        at: usize,
    }

    #[async_trait]
    impl RemoteRead for ChunkReader {
        async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, RemoteError> {
            if self.at >= self.bytes.len() {
                return Ok(None);
            }
            let end = (self.at + REMOTE_CHUNK_BYTES).min(self.bytes.len());
            let chunk = self.bytes[self.at..end].to_vec();
            self.at = end;
            Ok(Some(chunk))
        }
    }

    struct MapWriter {
        files: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
        path: String,
        buf: Vec<u8>,
    }

    #[async_trait]
    impl RemoteWrite for MapWriter {
        async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), RemoteError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
        async fn finish(&mut self) -> Result<(), RemoteError> {
            self.files
                .lock()
                .unwrap()
                .insert(self.path.clone(), std::mem::take(&mut self.buf));
            Ok(())
        }
    }

    #[test]
    fn a_duplicate_registration_is_refused_and_the_incumbent_stays() {
        let reg = RemoteTransportRegistry::new();
        reg.register(StubTransport::new(), "tunnels")
            .expect("first registers");
        let err = reg
            .register(StubTransport::new(), "other")
            .expect_err("the seat is taken");
        assert!(err.contains("already provided by plugin tunnels"), "{err}");
        assert_eq!(reg.list().len(), 1);
        assert!(matches!(reg.presence(), RemotePresence::Serving(id) if id == "tunnels"));
    }

    #[tokio::test]
    async fn withdrawal_refuses_new_operations_and_clear_releases_the_seat() {
        let reg = RemoteTransportRegistry::new();
        reg.register(StubTransport::new(), "tunnels")
            .expect("registers");
        reg.begin_withdraw();
        let (tx, _rx) = mpsc::channel(EXEC_EVENT_QUEUE);
        match reg
            .exec(
                "build-01",
                "test",
                RemoteExecRequest {
                    argv: vec!["make".into()],
                    env: Vec::new(),
                    cwd: None,
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
        {
            Err(RemoteError::Withdrawing(m)) => assert!(m.contains("tunnels"), "{m}"),
            Ok(_) => panic!("a withdrawing provider must not start operations"),
            Err(other) => panic!("withdrawing refuses, wrong error: {other}"),
        }
        assert!(matches!(
            reg.stat("build-01", "t", "/x").await,
            Err(RemoteError::Withdrawing(_))
        ));
        // A duplicate while withdrawing is STILL a duplicate: the withdraw owns the seat.
        assert!(reg.register(StubTransport::new(), "other").is_err());
        reg.clear();
        assert!(matches!(
            reg.presence(),
            RemotePresence::Absent(Some(id)) if id == "tunnels"
        ));
        reg.register(StubTransport::new(), "tunnels")
            .expect("re-registers after clear");
    }

    #[tokio::test]
    async fn an_absent_provider_is_named_rather_than_reported_as_no_hosts() {
        let reg = RemoteTransportRegistry::new();
        assert!(matches!(reg.presence(), RemotePresence::Absent(None)));
        assert!(reg.list().is_empty());
        let (tx, _rx) = mpsc::channel(EXEC_EVENT_QUEUE);
        match reg
            .exec(
                "build-01",
                "test",
                RemoteExecRequest {
                    argv: vec!["make".into()],
                    env: Vec::new(),
                    cwd: None,
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
        {
            Err(RemoteError::Unavailable(m)) => {
                assert!(m.contains("no plugin currently provides"), "{m}")
            }
            Ok(_) => panic!("no provider, got a result"),
            Err(other) => panic!("no provider, wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn exec_streams_events_and_reports_the_exit_status() {
        let reg = RemoteTransportRegistry::new();
        let stub = StubTransport::new();
        reg.register(stub.clone(), "tunnels").expect("registers");
        let (tx, mut rx) = mpsc::channel(EXEC_EVENT_QUEUE);
        let result = reg
            .exec(
                "build-01",
                "remote.exec",
                RemoteExecRequest {
                    argv: vec!["cmake".into(), "--build".into(), "build".into()],
                    env: vec![("BOARD".into(), "foo".into())],
                    cwd: Some("/data/workspaces/proj".into()),
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
            .expect("execs");
        assert_eq!(result.exit_code, 0);
        assert_eq!(
            rx.recv().await,
            Some(RemoteExecEvent::Stdout(
                "argv=cmake\u{1f}--build\u{1f}build".bytes().collect()
            ))
        );
        assert_eq!(
            rx.recv().await,
            Some(RemoteExecEvent::Stderr(b"warn".to_vec()))
        );
        assert_eq!(stub.execs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_endpoints_are_named() {
        let reg = RemoteTransportRegistry::new();
        reg.register(StubTransport::new(), "tunnels")
            .expect("registers");
        let (tx, _rx) = mpsc::channel(EXEC_EVENT_QUEUE);
        match reg
            .exec(
                "ghost",
                "test",
                RemoteExecRequest {
                    argv: vec!["make".into()],
                    env: Vec::new(),
                    cwd: None,
                },
                tx,
                CancelSource::new().handle(),
            )
            .await
        {
            Err(RemoteError::Unknown(m)) => assert!(m.contains("ghost"), "{m}"),
            Ok(_) => panic!("unknown endpoint, got a result"),
            Err(other) => panic!("unknown endpoint, wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn file_operations_round_trip_through_the_seam_in_chunks() {
        let reg = RemoteTransportRegistry::new();
        let stub = StubTransport::new();
        reg.register(stub.clone(), "tunnels").expect("registers");
        reg.mkdir_p("build-01", "sync", "/data/ws/proj/src")
            .await
            .expect("mkdir");
        let mut writer = reg
            .create("build-01", "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("create");
        let payload: Vec<u8> = (0..(REMOTE_CHUNK_BYTES * 2 + 10) as u32)
            .map(|i| (i % 251) as u8)
            .collect();
        for chunk in payload.chunks(REMOTE_CHUNK_BYTES) {
            writer.write_chunk(chunk).await.expect("write chunk");
        }
        writer.finish().await.expect("finish");
        let stat = reg
            .stat("build-01", "sync", "/data/ws/proj/src/main.c")
            .await
            .expect("stat")
            .expect("present");
        assert_eq!(stat.size, payload.len() as u64);
        // And back: the read must arrive in chunk-size pieces, never whole.
        let mut reader = reg
            .open_read("build-01", "pull", "/data/ws/proj/src/main.c")
            .await
            .expect("open");
        let mut back = Vec::new();
        while let Some(chunk) = reader.read_chunk().await.expect("chunk") {
            assert!(
                chunk.len() <= REMOTE_CHUNK_BYTES,
                "a chunk exceeded the ceiling"
            );
            back.extend_from_slice(&chunk);
        }
        assert_eq!(back, payload);
    }

    #[tokio::test]
    async fn an_exec_only_provider_refuses_file_operations_as_unsupported() {
        let reg = RemoteTransportRegistry::new();
        reg.register(Arc::new(ExecOnlyTransport), "tunnels")
            .expect("registers");
        match reg.stat("any", "test", "/x").await {
            Err(RemoteError::Unsupported(m)) => assert!(m.contains("no file channel"), "{m}"),
            Ok(_) => panic!("no file transport, got a stat"),
            Err(other) => panic!("wrong error: {other}"),
        }
    }

    #[tokio::test]
    async fn a_hundred_lifecycles_leave_the_registry_exactly_empty() {
        // The same churn discipline the catalog and shell registries are held to:
        // enable/disable must not leak providers or rows.
        let reg = RemoteTransportRegistry::new();
        for _ in 0..100 {
            reg.register(StubTransport::new(), "tunnels")
                .expect("registers");
            reg.begin_withdraw();
            reg.clear();
        }
        assert!(matches!(reg.presence(), RemotePresence::Absent(Some(_))));
        assert!(reg.list().is_empty());
        assert!(!reg.has_provider());
        reg.register(StubTransport::new(), "tunnels")
            .expect("registers after the churn");
    }
}
