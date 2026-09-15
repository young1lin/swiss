//! Test-only SSH and proxy infrastructure (docs/27 §2.6.1).
//!
//! Three pieces, all on ephemeral loopback ports, none of them sleeping:
//!  - a REAL ssh server: russh's server side, accept-all auth, direct-tcpip channels
//!    dialed back to the requested address — the far end a proxied client must reach;
//!  - a fake socks5 proxy speaking RFC 1928/1929, recording every byte of the handshake
//!    and then transparently pumping to the real target;
//!  - a fake http proxy answering CONNECT with a canned status line and pumping on 2xx.
//!
//! Shared by the proxy dialer tests (proxy.rs) and the manager-level lifecycle test
//! (manager.rs); Item 3 (jump) reuses the ssh server unchanged.

#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// Everything the fake socks5 proxy observed on one connection (docs/27 §2.6.2): the
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

/// A real SSH server on an ephemeral loopback port. Runs until the test binary exits;
/// accepts any password and any public key, and forwards direct-tcpip channels to the
/// address the client asked for.
pub async fn spawn_ssh_server() -> u16 {
    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
    use russh::Channel;

    struct TestHandler;

    impl server::Handler for TestHandler {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            _user: &str,
            _password: &str,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn auth_publickey(
            &mut self,
            _user: &str,
            _key: &russh::keys::ssh_key::PublicKey,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn channel_open_direct_tcpip(
            &mut self,
            channel: Channel<Msg>,
            host_to_connect: &str,
            port_to_connect: u32,
            _originator_address: &str,
            _originator_port: u32,
            reply: ChannelOpenHandle,
            _session: &mut Session,
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
    }

    // A fresh random host key per server: the client under test runs TOFU with no
    // expected fingerprint, so any key verifies. Built from a random seed rather than
    // PrivateKey::random because russh's rng traits live on rand 0.10 and the workspace
    // carries rand 0.9 — from_seed needs no rng crate at all.
    let seed = rand::random::<[u8; 32]>();
    let keypair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::KeypairData::Ed25519(keypair),
        "",
    )
    .expect("a fresh host key");
    let config = Arc::new(server::Config {
        inactivity_timeout: None,
        auth_rejection_time: Duration::from_millis(200),
        auth_rejection_time_initial: Some(Duration::from_millis(200)),
        keys: vec![key],
        ..Default::default()
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ssh");
    let port = listener.local_addr().expect("ssh addr").port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, TestHandler).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

/// A fake socks5 proxy on an ephemeral loopback port. `rep` is the reply code it answers
/// the CONNECT with (0 for the happy paths, 1 to test the failure classification). Every
/// accepted connection reports one `Socks5Observations`; after a successful reply the
/// socket pumps transparently to the address the client asked for, so the SSH handshake
/// that follows rides this proxy like a real one.
pub async fn spawn_socks5_proxy(auth: SocksAuth, rep: u8) -> (u16, mpsc::Receiver<Socks5Observations>) {
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
/// reports the full request head verbatim for the header assertions (docs/27 §2.6.3).
pub async fn spawn_http_proxy(status_line: &'static str) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http proxy");
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
                let _ = sock
                    .write_all(format!("{status}\r\n\r\n").as_bytes())
                    .await;
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
                let Ok(port) = port.parse::<u16>() else { return };
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
