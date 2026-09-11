//! One forwarding rule's local listener — port of `tunnels/forward.ts`.
//!
//! The contract that matters, and the reason this type owns the socket set: a local port stays
//! bound only while the tunnel can actually carry traffic. `close()` therefore closes the
//! listener AND destroys every accepted socket AND verifies the port is rebindable — a lingering
//! accepted socket is exactly what keeps a "dead" port occupied, which is the failure the old
//! tool shipped with (an app connects successfully and then hangs, and the port cannot be reused
//! until the tool restarts).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use super::port::wait_for_release;
use super::types::{FailureKind, ForwardStats, TunnelError};
use swiss_core::log;

/// A combined alias: a trait object cannot name two non-auto traits, so `AsyncRead + AsyncWrite
/// + Send` gets one supertrait. Every readable+writable+sendable type implements it via the
///   blanket impl below.
pub trait StreamPair: AsyncRead + AsyncWrite + Send {}
impl<T: AsyncRead + AsyncWrite + Send> StreamPair for T {}

/// A byte stream through the SSH connection to the remote target. `Channel::into_stream()`
/// boxed — the Forward never knows russh is behind it, which is what keeps it testable.
pub type ByteStream = Pin<Box<dyn StreamPair>>;

/// Opens one channel to the remote target. Injected, so Forward runs with no SSH at all.
pub type ChannelOpener = Arc<
    dyn Fn(&str, u16) -> Pin<Box<dyn Future<Output = Result<ByteStream, TunnelError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct ForwardTarget {
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// Cap on concurrent local sockets, so one runaway client cannot exhaust memory or SSH channels.
const DEFAULT_MAX_SOCKETS: usize = 200;

/// The counters shared by the listener and every accepted connection (the panel's live columns).
#[derive(Default)]
struct Shared {
    sockets: AtomicUsize,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    channel_failures: AtomicU64,
    refused: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Shared {
    fn set_last_error(&self, msg: String) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(msg);
        }
    }
}

/// Decrement the socket count exactly once, even when the pipe task is aborted mid-copy —
/// an aborted task never runs code after its await point, so only Drop is reliable.
struct SocketGuard(Arc<Shared>);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.0.sockets.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A TcpStream that counts bytes as they flow, in the socket direction Node counted
/// (`bytesRead` = in from the client, `bytesWritten` = out to the client).
struct CountedStream {
    stream: TcpStream,
    shared: Arc<Shared>,
}

impl AsyncRead for CountedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let n = buf.filled().len().saturating_sub(before);
                self.shared.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncWrite for CountedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.shared.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

pub struct Forward {
    target: ForwardTarget,
    open: ChannelOpener,
    max_sockets: usize,
    host: String,
    /// The accept loop's task, plus every live connection — close() aborts them all, which is
    /// what "destroy every accepted socket" means under an async runtime.
    tasks: Mutex<JoinSet<()>>,
    accept_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    listening: std::sync::atomic::AtomicBool,
    shared: Arc<Shared>,
}

impl Forward {
    pub fn new(target: ForwardTarget, open: ChannelOpener) -> Arc<Self> {
        Arc::new(Forward {
            target,
            open,
            max_sockets: DEFAULT_MAX_SOCKETS,
            host: "127.0.0.1".into(),
            tasks: Mutex::new(JoinSet::new()),
            accept_task: Mutex::new(None),
            listening: std::sync::atomic::AtomicBool::new(false),
            shared: Arc::new(Shared::default()),
        })
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    /// Bind the local port. Errors with TunnelError(kind=port) when something already holds it.
    pub async fn listen(self: &Arc<Self>) -> Result<(), TunnelError> {
        if self.listening() {
            return Ok(());
        }
        let listener = tokio::net::TcpListener::bind((self.host.as_str(), self.target.local_port))
            .await
            .map_err(|err| {
                let code = match err.kind() {
                    std::io::ErrorKind::AddrInUse => "EADDRINUSE",
                    std::io::ErrorKind::PermissionDenied => "EACCES",
                    _ => "",
                };
                if !code.is_empty() {
                    let mut detail = serde_json::Map::new();
                    detail.insert("port".into(), json!(self.target.local_port));
                    detail.insert("code".into(), json!(code));
                    return TunnelError::with_detail(
                        format!(
                            "local port {} is not available ({})",
                            self.target.local_port, code
                        ),
                        FailureKind::Port,
                        detail,
                    );
                }
                TunnelError::new(
                    format!(
                        "listen on {}:{} failed: {}",
                        self.host, self.target.local_port, err
                    ),
                    FailureKind::Port,
                )
            })?;
        self.listening.store(true, Ordering::SeqCst);
        let this = self.clone();
        let task = tokio::spawn(async move {
            this.accept_loop(listener).await;
        });
        if let Ok(mut slot) = self.accept_task.lock() {
            *slot = Some(task);
        }
        Ok(())
    }

    async fn accept_loop(self: Arc<Self>, listener: tokio::net::TcpListener) {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => self.accept(socket),
                // Past startup, a listener error must not take the process down — record it and
                // keep accepting, exactly as the Node build's post-listen error handler did.
                Err(err) => {
                    let msg = err.to_string();
                    self.shared.set_last_error(msg.clone());
                    log::warn(
                        "tunnel listener error",
                        Some(json!({ "port": self.target.local_port, "err": msg })),
                    );
                    // A persistently failing accept (fd exhaustion, a dying listener) fails
                    // INSTANTLY, and without a beat here this loop would spin a full core; a
                    // healthy listener never pays this sleep.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn accept(self: &Arc<Self>, socket: TcpStream) {
        if self.shared.sockets.load(Ordering::SeqCst) >= self.max_sockets {
            self.shared.refused.fetch_add(1, Ordering::SeqCst);
            self.shared.set_last_error(format!(
                "refused: {} concurrent connections already open",
                self.max_sockets
            ));
            return; // dropping the socket destroys it, as Node's socket.destroy() did
        }
        self.shared.sockets.fetch_add(1, Ordering::SeqCst);

        // A client that vanishes without a FIN would otherwise hold a socket — and its SSH
        // channel — forever. No idle timeout: an idle DB pool socket is legitimate and must not
        // be reaped.
        set_keepalive_30s(&socket);
        let _ = socket.set_nodelay(true);

        let opener = self.open.clone();
        let target = self.target.clone();
        let shared = self.shared.clone();
        let fut = async move {
            let _guard = SocketGuard(shared.clone()); // decrements even on abort, via Drop
            let channel = match opener(&target.target_host, target.target_port).await {
                Ok(channel) => channel,
                // The remote target refused the channel. That is this one connection's failure,
                // not the tunnel's: SSH is fine, so the listener stays up and the rule stays
                // reported as up.
                Err(err) => {
                    shared.channel_failures.fetch_add(1, Ordering::SeqCst);
                    shared.set_last_error(err.message.clone());
                    return; // dropping the socket destroys it
                }
            };
            // Both halves of a proxied pair routinely die mid-transfer (ECONNRESET on either
            // side); that is normal traffic, not a fault worth surfacing.
            let mut counted = CountedStream {
                stream: socket,
                shared: shared.clone(),
            };
            let mut channel = channel;
            let _ = tokio::io::copy_bidirectional(&mut counted, &mut channel).await;
        };
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.spawn(fut);
            // Reap finished entries so a long-lived forward does not accumulate them.
            while tasks.try_join_next().is_some() {}
        }
    }

    /// Stop accepting, destroy every accepted socket, and wait until the port can be bound
    /// again. All three steps are the point: closing the listener alone leaves established
    /// sockets holding the port, and returning before the OS has released it makes an immediate
    /// restart fail with EADDRINUSE against nothing.
    pub async fn close(&self) {
        self.listening.store(false, Ordering::SeqCst);
        if let Ok(mut slot) = self.accept_task.lock() {
            if let Some(task) = slot.take() {
                task.abort(); // drops the listener
            }
        }
        // Aborting each task drops its TcpStream and its channel — the destroy-everything step.
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.abort_all();
            tasks.detach_all();
        }
        if !wait_for_release(self.target.local_port, &self.host).await {
            log::warn(
                "tunnel port still held after close",
                Some(json!({ "port": self.target.local_port })),
            );
        }
    }

    pub fn stats(&self) -> ForwardStats {
        ForwardStats {
            sockets: self.shared.sockets.load(Ordering::SeqCst),
            bytes_in: self.shared.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.shared.bytes_out.load(Ordering::Relaxed),
            channel_failures: self.shared.channel_failures.load(Ordering::SeqCst),
            refused: self.shared.refused.load(Ordering::SeqCst),
            last_error: self.shared.last_error.lock().ok().and_then(|s| s.clone()),
        }
    }
}

/// Node's `socket.setKeepAlive(true, 30_000)`, via socket2 (tokio does not expose it).
fn set_keepalive_30s(socket: &TcpStream) {
    use socket2::SockRef;
    let keepalive = socket2::TcpKeepalive::new().with_time(std::time::Duration::from_secs(30));
    let _ = SockRef::from(socket).set_tcp_keepalive(&keepalive);
}

#[cfg(test)]
mod tests {
    // Forward is pure plumbing: a fake ChannelOpener plus a loopback listener exercises the
    // whole accept/pipe/close lifecycle with no SSH at all.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn opener_echo() -> ChannelOpener {
        // Echo server as the "channel": whatever the client sends comes back.
        Arc::new(|_host: &str, _port: u16| {
            Box::pin(async {
                let (client, server) = tokio::io::duplex(4096);
                tokio::spawn(async move {
                    let (mut r, mut w) = tokio::io::split(server);
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
                Ok::<_, TunnelError>(Box::pin(client) as ByteStream)
            }) as Pin<Box<dyn Future<Output = Result<ByteStream, TunnelError>> + Send>>
        })
    }

    #[tokio::test]
    async fn binds_forwards_and_releases_the_port() {
        let target = ForwardTarget {
            local_port: 0, // placeholder; real port chosen below
            target_host: "db".into(),
            target_port: 5432,
        };
        // Pick a free port by binding once, so parallel tests never collide.
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let target = ForwardTarget {
            local_port: port,
            ..target
        };

        let fwd = Forward::new(target, opener_echo());
        fwd.listen().await.expect("binds");
        assert!(fwd.listening());

        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        sock.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        sock.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let stats = fwd.stats();
        assert_eq!(stats.sockets, 1);
        assert!(stats.bytes_in >= 4);

        fwd.close().await;
        assert!(!fwd.listening());
        assert!(
            probe_port_now(port).await,
            "the port is bindable again right after close"
        );
    }

    async fn probe_port_now(port: u16) -> bool {
        super::super::port::probe_port(port, "127.0.0.1").await
    }

    #[tokio::test]
    async fn a_second_binder_gets_the_port_error() {
        let holder = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = holder.local_addr().unwrap().port();
        let fwd = Forward::new(
            ForwardTarget {
                local_port: port,
                target_host: "db".into(),
                target_port: 5432,
            },
            opener_echo(),
        );
        let err = fwd.listen().await.unwrap_err();
        assert_eq!(err.kind, FailureKind::Port);
        assert_eq!(
            err.message,
            format!("local port {port} is not available (EADDRINUSE)")
        );
    }

    #[tokio::test]
    async fn a_failing_opener_counts_channel_failures_not_tunnel_failures() {
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let fwd = Forward::new(
            ForwardTarget {
                local_port: port,
                target_host: "db".into(),
                target_port: 5432,
            },
            Arc::new(|_h: &str, _p: u16| {
                Box::pin(async {
                    Err::<ByteStream, TunnelError>(TunnelError::new(
                        "connect failed",
                        FailureKind::Network,
                    ))
                })
                    as Pin<Box<dyn Future<Output = Result<ByteStream, TunnelError>> + Send>>
            }),
        );
        fwd.listen().await.unwrap();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // The connection is accepted, the channel fails, the socket is destroyed. The rule
        // itself stays up — that is the contract this test pins.
        sock.write_all(b"x").await.unwrap_or(());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let stats = fwd.stats();
        assert_eq!(stats.channel_failures, 1);
        assert_eq!(stats.last_error.as_deref(), Some("connect failed"));
        fwd.close().await;
    }
}
