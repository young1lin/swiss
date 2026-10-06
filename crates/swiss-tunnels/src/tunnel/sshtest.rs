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

//! Test-only SSH and proxy infrastructure (SPEC §tunnels.proxy).
//!
//! Three pieces, all on ephemeral loopback ports, none of them sleeping:
//!  - a REAL ssh server: russh's server side, auth per a chosen policy (accept-all, or
//!    reject-everything for the bad-credentials paths), direct-tcpip channels dialed
//!    back to the requested address — the far end a proxied or jumped client must reach.
//!    A killable/restartable flavor keeps one fixed port and host key for the
//!    loss-and-recovery lifecycle (SPEC §tunnels.jump);
//!  - a fake socks5 proxy speaking RFC 1928/1929, recording every byte of the handshake
//!    and then transparently pumping to the real target;
//!  - a fake http proxy answering CONNECT with a canned status line and pumping on 2xx.
//!
//! Shared by the proxy dialer tests (proxy.rs) and the manager-level lifecycle tests
//! (manager.rs), including the jump chains (SPEC §tunnels.jump).

#![cfg(test)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::mpsc;

/// Everything the fake socks5 proxy observed on one connection (SPEC §tunnels.proxy): the
/// methods the client offered, the RFC 1929 credential blob verbatim, and the connect
/// request's ATYP + address bytes + network-order port.
#[derive(Debug, Clone, PartialEq)]
pub struct Socks5Observations {
    pub methods: Vec<u8>,
    pub user_pass: Vec<u8>,
    pub atyp: u8,
    pub host: Vec<u8>,
    pub port: u16,
}

/// Which methods the fake socks5 proxy is willing to negotiate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SocksAuth {
    /// Only \x00 (no auth).
    None,
    /// \x02 only: a client offering nothing but \x00 is refused with \xFF, the way a
    /// hardened proxy behaves.
    UserPass,
}

/// What the test SSH server does with credentials (SPEC §tunnels.jump): accept anything,
/// or refuse every method — the bad-credentials paths need a server that says no.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestAuth {
    AcceptAll,
    RejectAll,
}

/// What the fake exec server answers one exec request with (SPEC §remote.transport):
/// channel success, stdout, stderr, then — when `exit` is set — the exit status
/// and channel close. A None `exit` is a WEDGED command: the handler returns, the
/// channel stays open, and the client must cancel or hit its deadline. (The stall
/// used to be a sleep inside the handler; that blocked the server's whole event
/// loop, so even the CHANNEL_SUCCESS sat unflushed. A real sshd always answers the
/// request before the program runs.)
#[derive(Clone, Default)]
pub struct ExecScript {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: Option<u32>,
    /// What the command does with its stdin; anything but Ignore runs until the client's
    /// EOF and then exits 0 (`exit` is not used).
    pub stdin: StdinMode,
}

/// What a fake exec command does with the stdin the client sends.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum StdinMode {
    /// Never reads it: the script's own exit ends the command.
    #[default]
    Ignore,
    /// A `cat`: every byte comes straight back as stdout.
    Echo,
    /// A `wc -c`: the byte count is the only output, printed at EOF. A large input needs
    /// this shape - russh's session loop does not read while it flushes, so a russh
    /// server echoing megabytes to a russh client that is still sending them wedges both
    /// on loopback. A real sshd reads and writes at once; that wedge is the test pair's,
    /// not the exec's.
    Count,
}

/// The russh server handler behind every test server: answer auth per the policy, and
/// forward direct-tcpip channels to the address the client asked for.
struct TestHandler {
    auth: TestAuth,
    /// When set, session-channel exec requests answer with this script (SPEC §remote.transport
    /// tests): channel success, stdout, stderr, an optional stall, then the exit
    /// status. A None handler ignores exec requests entirely.
    exec: Option<ExecScript>,
    /// Stdin bytes seen so far, for StdinMode::Count.
    stdin_bytes: usize,
}

impl TestHandler {
    fn stdin_mode(&self) -> StdinMode {
        self.exec.as_ref().map(|s| s.stdin).unwrap_or_default()
    }
}

impl russh::server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(match self.auth {
            TestAuth::AcceptAll => russh::server::Auth::Accept,
            TestAuth::RejectAll => russh::server::Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(match self.auth {
            TestAuth::AcceptAll => russh::server::Auth::Accept,
            TestAuth::RejectAll => russh::server::Auth::reject(),
        })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let target = (host_to_connect.to_string(), port_to_connect as u16);
        tokio::spawn(async move {
            // The far side of the channel: dial the requested address and pump
            // bytes both ways until either end closes.
            if let Ok(mut tcp) = TcpStream::connect(target).await {
                let mut chan = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut chan, &mut tcp).await;
            }
        });
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        _channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        // Dropping the handle rejects, so accept explicitly: session channels are how
        // exec (and later SFTP) reach this server.
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let Some(script) = self.exec.clone() else {
            return Ok(());
        };
        let _ = session.channel_success(channel);
        if !script.stdout.is_empty() {
            session.data(channel, script.stdout.clone())?;
        }
        if !script.stderr.is_empty() {
            session.extended_data(channel, 1, script.stderr.clone())?;
        }
        if script.stdin != StdinMode::Ignore {
            // The command runs until its stdin ends: data() reads, channel_eof() exits.
            return Ok(());
        }
        // Everything sent here flushes when this handler returns to the event loop.
        if let Some(exit) = script.exit {
            session.exit_status_request(channel, exit)?;
            session.close(channel)?;
        }
        // `data` (the command string) is unused on purpose: the echo of it is the
        // client's own assertion, not the server's.
        let _ = data;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        match self.stdin_mode() {
            StdinMode::Ignore => {}
            StdinMode::Echo => session.data(channel, data.to_vec())?,
            StdinMode::Count => self.stdin_bytes += data.len(),
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: russh::ChannelId,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let mode = self.stdin_mode();
        if mode == StdinMode::Count {
            session.data(channel, format!("{}\n", self.stdin_bytes).into_bytes())?;
        }
        if mode != StdinMode::Ignore {
            session.exit_status_request(channel, 0)?;
            session.close(channel)?;
        }
        Ok(())
    }
}

/// A fresh server config with a fresh random host key: the client under test runs TOFU
/// with no expected fingerprint, so any key verifies. Built from a random seed:
/// from_seed needs no rng handle threaded through ssh-key's API.
fn test_server_config() -> Arc<russh::server::Config> {
    let seed = rand::random::<[u8; 32]>();
    let keypair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::KeypairData::Ed25519(keypair),
        "",
    )
    .expect("a fresh host key");
    Arc::new(russh::server::Config {
        inactivity_timeout: None,
        auth_rejection_time: Duration::from_millis(200),
        auth_rejection_time_initial: Some(Duration::from_millis(200)),
        keys: vec![key],
        ..Default::default()
    })
}

/// The accept loop every server flavor shares. `track` collects each session's
/// russh handle so a killable server can disconnect established connections too.
fn serve_connections(
    listener: TcpListener,
    config: Arc<russh::server::Config>,
    auth: TestAuth,
    track: Option<Arc<Mutex<Vec<russh::server::Handle>>>>,
) -> tokio::task::JoinHandle<()> {
    serve_connections_exec(listener, config, auth, track, None)
}

/// The accept loop with an optional exec script handed to every session's handler.
fn serve_connections_exec(
    listener: TcpListener,
    config: Arc<russh::server::Config>,
    auth: TestAuth,
    track: Option<Arc<Mutex<Vec<russh::server::Handle>>>>,
    exec: Option<ExecScript>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = config.clone();
            let track = track.clone();
            let exec = exec.clone();
            tokio::spawn(async move {
                let handler = TestHandler { auth, exec, stdin_bytes: 0 };
                if let Ok(run) = russh::server::run_stream(config, socket, handler).await {
                    if let Some(track) = &track {
                        if let Ok(mut list) = track.lock() {
                            list.push(run.handle());
                        }
                    }
                    let _ = run.await;
                }
            });
        }
    })
}

/// A real SSH server on an ephemeral loopback port. Runs until the test binary exits;
/// accepts any password and any public key, and forwards direct-tcpip channels to the
/// address the client asked for.
pub async fn spawn_ssh_server() -> u16 {
    spawn_ssh_server_with(TestAuth::AcceptAll).await
}

/// The same server whose session channels answer exec requests per `script` (SPEC §remote.transport):
/// the full russh client path — connect, session channel, exec request, streaming
/// Data/ExtendedData, exit status — with no real shell anywhere.
pub async fn spawn_exec_ssh_server(script: ExecScript) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ssh");
    let port = listener.local_addr().expect("ssh addr").port();
    serve_connections_exec(
        listener,
        test_server_config(),
        TestAuth::AcceptAll,
        None,
        Some(script),
    );
    port
}

/// The same server with a chosen auth policy (SPEC §tunnels.jump).
pub async fn spawn_ssh_server_with(auth: TestAuth) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ssh");
    let port = listener.local_addr().expect("ssh addr").port();
    serve_connections(listener, test_server_config(), auth, None);
    port
}

/// Bind 127.0.0.1:<port> with SO_REUSEADDR, so a killed server's port can be taken back
/// by a restart. Port 0 picks an ephemeral port the usual way.
fn bind_reuse_port(port: u16) -> TcpListener {
    let socket = TcpSocket::new_v4().expect("a v4 socket");
    socket
        .set_reuseaddr(true)
        .expect("so a restart can rebind the same port");
    socket
        .bind(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("a loopback addr"),
        )
        .expect("bind ssh");
    socket.listen(64).expect("listen ssh")
}

/// A killable and restartable SSH server on one fixed port (SPEC §tunnels.jump). `kill`
/// stops the accepts and disconnects every established session — "the bastion went
/// away" — and `restart` rebinds the SAME port with the SAME host key, so the def
/// under test keeps pointing at it and TOFU still verifies when it comes back.
pub struct SshServerHandle {
    port: u16,
    config: Arc<russh::server::Config>,
    auth: TestAuth,
    accept: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The live sessions' russh handles. russh runs each accepted session in its OWN
    /// internally-spawned task, so aborting the wrapper task above cannot reach the
    /// socket — the handle's disconnect message is the one kill switch that can.
    sessions: Arc<Mutex<Vec<russh::server::Handle>>>,
}

impl SshServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Take the server down: no more accepts, and every established session is
    /// disconnected at the protocol level — clients see their transport die.
    pub async fn kill(&self) {
        if let Ok(mut slot) = self.accept.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
        // Take the list out before awaiting: a std guard must never sit across an
        // await point, and the disconnects below are sends into the sessions.
        let sessions = match self.sessions.lock() {
            Ok(mut list) => std::mem::take(&mut *list),
            Err(_) => Vec::new(),
        };
        for handle in sessions {
            // A session that already ended refuses the send; that is the normal
            // cleanup path, not an error.
            let _ = handle
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "killed by the test".into(),
                    String::new(),
                )
                .await;
        }
    }

    /// Bring it back on the same port with the same host key.
    pub async fn restart(&self) {
        self.kill().await;
        let task = serve_connections(
            bind_reuse_port(self.port),
            self.config.clone(),
            self.auth,
            Some(self.sessions.clone()),
        );
        if let Ok(mut slot) = self.accept.lock() {
            *slot = Some(task);
        }
    }
}

/// The loss-and-recovery fixture (SPEC §tunnels.jump): a server whose death and return are
/// both real and observable.
pub fn spawn_killable_ssh_server(auth: TestAuth) -> SshServerHandle {
    let listener = bind_reuse_port(0);
    let port = listener.local_addr().expect("ssh addr").port();
    let config = test_server_config();
    let sessions = Arc::new(Mutex::new(Vec::new()));
    let task = serve_connections(listener, config.clone(), auth, Some(sessions.clone()));
    SshServerHandle {
        port,
        config,
        auth,
        accept: Mutex::new(Some(task)),
        sessions,
    }
}

/// A fake socks5 proxy on an ephemeral loopback port. `rep` is the reply code it answers
/// the CONNECT with (0 for the happy paths, 1 to test the failure classification). Every
/// accepted connection reports one `Socks5Observations`; after a successful reply the
/// socket pumps transparently to the address the client asked for, so the SSH handshake
/// that follows rides this proxy like a real one.
pub async fn spawn_socks5_proxy(
    auth: SocksAuth,
    rep: u8,
) -> (u16, mpsc::Receiver<Socks5Observations>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind socks5");
    let port = listener.local_addr().expect("socks5 addr").port();
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                // 1. The greeting: VER NMETHODS METHODS...
                let mut head = [0u8; 2];
                if sock.read_exact(&mut head).await.is_err() {
                    return;
                }
                let mut methods = vec![0u8; head[1] as usize];
                if sock.read_exact(&mut methods).await.is_err() {
                    return;
                }
                // 2. Method selection: honor the intersection like a real proxy, then run
                // the RFC 1929 sub-negotiation when \x02 is chosen.
                let mut user_pass = Vec::new();
                let use_userpass = matches!(auth, SocksAuth::UserPass) && methods.contains(&0x02);
                if use_userpass {
                    let _ = sock.write_all(&[0x05, 0x02]).await;
                    let mut len = [0u8; 2]; // VER ULEN
                    if sock.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut user = vec![0u8; len[1] as usize];
                    if sock.read_exact(&mut user).await.is_err() {
                        return;
                    }
                    let mut plen = [0u8; 1];
                    if sock.read_exact(&mut plen).await.is_err() {
                        return;
                    }
                    let mut pass = vec![0u8; plen[0] as usize];
                    if sock.read_exact(&mut pass).await.is_err() {
                        return;
                    }
                    user_pass.extend_from_slice(&len);
                    user_pass.extend_from_slice(&user);
                    user_pass.extend_from_slice(&plen);
                    user_pass.extend_from_slice(&pass);
                    let _ = sock.write_all(&[0x01, 0x00]).await; // accepted
                } else if methods.contains(&0x00) && auth == SocksAuth::None {
                    let _ = sock.write_all(&[0x05, 0x00]).await;
                } else {
                    let _ = sock.write_all(&[0x05, 0xFF]).await;
                    return; // no acceptable method
                }
                // 3. The connect request: VER CMD RSV ATYP ADDR PORT.
                let mut req = [0u8; 4];
                if sock.read_exact(&mut req).await.is_err() {
                    return;
                }
                let host_bytes = match req[3] {
                    0x01 => read_exact_bytes(&mut sock, 4).await,
                    0x04 => read_exact_bytes(&mut sock, 16).await,
                    0x03 => {
                        let mut len = [0u8; 1];
                        if sock.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        read_exact_bytes(&mut sock, len[0] as usize).await
                    }
                    _ => return,
                };
                let mut port_bytes = [0u8; 2];
                if sock.read_exact(&mut port_bytes).await.is_err() {
                    return;
                }
                let target_port = u16::from_be_bytes(port_bytes);
                // 4. The reply: REP as configured, BND.ADDR 0.0.0.0 : 0.
                let _ = sock
                    .write_all(&[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await;
                if rep != 0x00 {
                    return; // a refused CONNECT ends the connection
                }
                let target = atyp_target(req[3], &host_bytes);
                let _ = tx
                    .send(Socks5Observations {
                        methods,
                        user_pass,
                        atyp: req[3],
                        host: host_bytes.clone(),
                        port: target_port,
                    })
                    .await;
                // 5. Transparent pump to the real target.
                if let Ok(mut up) = TcpStream::connect((target.as_str(), target_port)).await {
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
                }
            });
        }
    });
    (port, rx)
}

/// A fake http proxy answering CONNECT with the canned `status_line`. On 2xx it pumps
/// transparently to the CONNECT target; otherwise it closes. Each accepted connection
/// reports the full request head verbatim for the header assertions (SPEC §tunnels.proxy).
pub async fn spawn_http_proxy(status_line: &'static str) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind http proxy");
    let port = listener.local_addr().expect("http proxy addr").port();
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let tx = tx.clone();
            let status = status_line;
            tokio::spawn(async move {
                // Read the request head byte-wise until CRLFCRLF — same discipline as the
                // client side, so nothing past the head is swallowed.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match sock.read(&mut byte).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {
                            head.push(byte[0]);
                            if head.ends_with(b"\r\n\r\n") {
                                break;
                            }
                            if head.len() > 16 * 1024 {
                                return;
                            }
                        }
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let _ = tx.send(head.clone()).await;
                let _ = sock.write_all(format!("{status}\r\n\r\n").as_bytes()).await;
                let ok = status
                    .split_whitespace()
                    .nth(1)
                    .and_then(|c| c.parse::<u16>().ok())
                    .is_some_and(|c| (200..300).contains(&c));
                if !ok {
                    return;
                }
                // CONNECT host:port HTTP/1.1 — the authority is the middle token.
                let target = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_string));
                let Some(target) = target else { return };
                let Some((host, port)) = target.rsplit_once(':') else {
                    return;
                };
                let Ok(port) = port.parse::<u16>() else {
                    return;
                };
                if let Ok(mut up) = TcpStream::connect((host, port)).await {
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
                }
            });
        }
    });
    (port, rx)
}

async fn read_exact_bytes(sock: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    if sock.read_exact(&mut b).await.is_err() {
        return Vec::new();
    }
    b
}

/// The connectable spelling of the address an ATYP carried. For ATYP 03 the length
/// prefix rides in host_bytes[0]; anything malformed answers empty and the dial fails.
fn atyp_target(atyp: u8, host_bytes: &[u8]) -> String {
    match (atyp, host_bytes.len()) {
        (0x01, 4) => format!(
            "{}.{}.{}.{}",
            host_bytes[0], host_bytes[1], host_bytes[2], host_bytes[3]
        ),
        (0x04, 16) => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(host_bytes);
            std::net::Ipv6Addr::from(octets).to_string()
        }
        _ if !host_bytes.is_empty() => String::from_utf8_lossy(host_bytes).into_owned(),
        _ => String::new(),
    }
}
