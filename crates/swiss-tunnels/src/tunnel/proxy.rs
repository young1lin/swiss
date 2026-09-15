//! HTTP CONNECT / SOCKS5 proxy dialing for SSH connections (docs/27 §2).
//!
//! The proxy is a TRANSPORT for the SSH connection, not a URL feature: credentials never
//! ride inside the proxy URL (save-time validation already refused userinfo, §1.3) and
//! never materialize as a URL string here — the username and password travel as handshake
//! components only (§2.4). Both clients are hand-written against their RFCs (CONNECT via
//! RFC 9110 §9.3.6, socks5 via RFC 1928/1929): the crate already carries tokio streams
//! and a base64 coder, so a URL or socks crate would be the second of something the
//! workspace already has.

use std::net::{Ipv4Addr, Ipv6Addr};

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::types::{FailureKind, TunnelError};
use swiss_core::secure::refs;

/// Cap on the CONNECT response head: a proxy that never terminates its headers is wedged,
/// not slow, and must not hold the handshake budget hostage byte by byte forever.
const CONNECT_HEAD_CAP: usize = 8 * 1024;
/// RFC 1929 carries username and password as one length-prefixed byte each.
const RFC1929_MAX: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Socks5,
}

/// A parsed, ready-to-dial proxy (docs/27 §2.1): the stored URL's three components plus
/// the two credential fields, kept apart until they reach the handshake (§2.4).
#[derive(Debug, Clone)]
pub struct ResolvedProxy {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    /// Whole-field `${...}` reference or literal, unresolved — dial() resolves.
    pub username: Option<String>,
    /// Same contract as username.
    pub password: Option<String>,
}

/// Parse the stored proxy URL. The grammar was already tightened at save time (§1.3), so
/// this is the defensive second gate on the dial path — same family of messages, never
/// expected to fire on a value that came through the store.
pub fn parse(url: &str) -> Result<ResolvedProxy, String> {
    let url = url.trim();
    let bad_scheme = |input: &str| {
        format!(
            "proxy must be an http:// or socks5:// URL, got \"{}\"",
            input
        )
    };
    let Some(sep) = url.find("://") else {
        return Err(bad_scheme(url));
    };
    let scheme = match url[..sep].to_ascii_lowercase().as_str() {
        "http" => ProxyScheme::Http,
        "socks5" => ProxyScheme::Socks5,
        _ => return Err(bad_scheme(url)),
    };
    let rest = &url[sep + 3..];
    if rest.contains('@') {
        return Err(
            "proxy URL must not carry credentials; use the proxy username and password fields"
                .into(),
        );
    }
    if rest.contains(['/', '?', '#']) {
        return Err("proxy URL must not carry a path or query".into());
    }
    let default_port = match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Socks5 => 1080,
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let Some((h, after)) = inner.split_once(']') else {
            return Err(format!("proxy URL has an invalid IPv6 address: {rest}"));
        };
        let port = if after.is_empty() {
            default_port
        } else if let Some(p) = after.strip_prefix(':') {
            parse_port(p)?
        } else {
            return Err("proxy URL must not carry a path or query".into());
        };
        (format!("[{h}]"), port)
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains(':') => (h.to_string(), parse_port(p)?),
            None if !rest.is_empty() => (rest.to_string(), default_port),
            None => return Err("proxy URL has no host".into()),
            Some(("", _)) => return Err("proxy URL has no host".into()),
            Some(_) => return Err(format!("proxy URL has an invalid address: {rest}")),
        }
    };
    Ok(ResolvedProxy {
        scheme,
        host,
        port,
        username: None,
        password: None,
    })
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .ok()
        .filter(|p| *p >= 1)
        .ok_or_else(|| format!("invalid proxy port: {raw}"))
}

/// Dial `host:port` THROUGH the proxy and hand back the raw stream for the SSH handshake
/// (docs/27 §2.1). Credential references resolve HERE, before any socket: a missing
/// reference is a config failure (docs/19 D4) — visible, never retried, and the proxy
/// never sees a connection. The two values then travel only as handshake components.
pub async fn dial(p: &ResolvedProxy, host: &str, port: u16) -> Result<TcpStream, TunnelError> {
    let username = match &p.username {
        Some(raw) => {
            let v = refs::resolve(raw).map_err(|e| {
                TunnelError::new(format!("proxy username: {e}"), FailureKind::Config)
            })?;
            if v.len() > RFC1929_MAX {
                return Err(TunnelError::new(
                    format!(
                        "proxy username is longer than {RFC1929_MAX} bytes, which socks5 cannot carry"
                    ),
                    FailureKind::Config,
                ));
            }
            Some(v)
        }
        None => None,
    };
    let password = match &p.password {
        Some(raw) => {
            let v = refs::resolve(raw).map_err(|e| {
                TunnelError::new(format!("proxy password: {e}"), FailureKind::Config)
            })?;
            if v.len() > RFC1929_MAX {
                return Err(TunnelError::new(
                    format!(
                        "proxy password is longer than {RFC1929_MAX} bytes, which socks5 cannot carry"
                    ),
                    FailureKind::Config,
                ));
            }
            Some(v)
        }
        None => None,
    };

    let stream = TcpStream::connect(format!("{}:{}", p.host, p.port).as_str())
        .await
        .map_err(|e| {
            TunnelError::new(
                format!("cannot reach proxy {}:{}: {e}", p.host, p.port),
                FailureKind::Network,
            )
        })?;
    // Nagle off, matching what russh::client::connect does for its own TCP dial: an
    // interactive SSH session should not queue small packets on the proxied socket
    // either (docs/27 §2.1).
    if let Err(e) = stream.set_nodelay(true) {
        swiss_core::log::warn(
            "proxy set_nodelay failed",
            Some(serde_json::json!({ "err": e.to_string() })),
        );
    }
    match p.scheme {
        ProxyScheme::Http => connect_http(stream, p, &username, &password, host, port).await,
        ProxyScheme::Socks5 => connect_socks5(stream, p, &username, &password, host, port).await,
    }
}

/// HTTP CONNECT (docs/27 §2.2). The request carries the target authority, Host, and —
/// only when a credential is configured — Basic authorization assembled from the two
/// component fields (§2.4: components in a header, never a URL string). The response
/// head is read BYTE-WISE so nothing past CRLFCRLF is swallowed: the stream is handed
/// straight to the SSH handshake afterwards and must not lose its first bytes.
async fn connect_http(
    mut stream: TcpStream,
    p: &ResolvedProxy,
    username: &Option<String>,
    password: &Option<String>,
    host: &str,
    port: u16,
) -> Result<TcpStream, TunnelError> {
    let target = format!("{host}:{port}");
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if username.is_some() || password.is_some() {
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", username.as_deref().unwrap_or(""), password.as_deref().unwrap_or("")));
        req.push_str(&format!("Proxy-Authorization: Basic {basic}\r\n"));
    }
    req.push_str("\r\n");
    let io_err = |e: std::io::Error| {
        TunnelError::new(
            format!("proxy {}:{} closed during CONNECT: {e}", p.host, p.port),
            FailureKind::Network,
        )
    };
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(io_err)?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte).await {
            Ok(0) => return Err(io_err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof",
            ))),
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
                if head.len() > CONNECT_HEAD_CAP {
                    return Err(TunnelError::new(
                        format!(
                            "proxy {}:{} sent an oversized CONNECT response",
                            p.host, p.port
                        ),
                        FailureKind::Network,
                    ));
                }
            }
            Err(e) => return Err(io_err(e)),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let code = head
        .lines()
        .next()
        .and_then(|status| status.split_whitespace().nth(1))
        .and_then(|token| token.parse::<u16>().ok());
    match code {
        Some(c) if (200..300).contains(&c) => Ok(stream),
        Some(c) => Err(TunnelError::new(
            format!("proxy {}:{} refused CONNECT: HTTP {c}", p.host, p.port),
            FailureKind::Network,
        )),
        None => Err(TunnelError::new(
            format!(
                "proxy {}:{} sent a malformed CONNECT response",
                p.host, p.port
            ),
            FailureKind::Network,
        )),
    }
}

/// SOCKS5 (docs/27 §2.3, RFC 1928 + RFC 1929). Method negotiation, optional username/
/// password sub-negotiation, then CONNECT with the target as-is: a name goes through as
/// ATYP 03 for the PROXY to resolve (socks5h semantics, §1.3), IP literals as 01/04.
async fn connect_socks5(
    mut stream: TcpStream,
    p: &ResolvedProxy,
    username: &Option<String>,
    password: &Option<String>,
    host: &str,
    port: u16,
) -> Result<TcpStream, TunnelError> {
    let creds = username.is_some() || password.is_some();
    let io_err = |e: std::io::Error| {
        TunnelError::new(
            format!("proxy {}:{} closed during socks5: {e}", p.host, p.port),
            FailureKind::Network,
        )
    };
    let greeting: &[u8] = if creds { &[0x05, 0x02, 0x00, 0x02] } else { &[0x05, 0x01, 0x00] };
    stream.write_all(greeting).await.map_err(io_err)?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await.map_err(io_err)?;
    if reply[0] != 0x05 {
        return Err(TunnelError::new(
            format!(
                "proxy {}:{} sent a malformed socks5 reply",
                p.host, p.port
            ),
            FailureKind::Network,
        ));
    }
    match reply[1] {
        0x00 => {} // no auth needed
        0x02 => {
            // RFC 1929: VER ULEN USER PLEN PASS — the lengths were capped in dial().
            let user = username.clone().unwrap_or_default();
            let pass = password.clone().unwrap_or_default();
            let mut blob = vec![0x01, user.len() as u8];
            blob.extend_from_slice(user.as_bytes());
            blob.push(pass.len() as u8);
            blob.extend_from_slice(pass.as_bytes());
            stream.write_all(&blob).await.map_err(io_err)?;
            let mut sub = [0u8; 2];
            stream.read_exact(&mut sub).await.map_err(io_err)?;
            if sub != [0x01, 0x00] {
                return Err(TunnelError::new(
                    format!(
                        "proxy {}:{} rejected socks5 credentials",
                        p.host, p.port
                    ),
                    FailureKind::Network,
                ));
            }
        }
        0xFF => {
            return Err(TunnelError::new(
                format!(
                    "proxy {}:{} offers no supported socks5 auth method",
                    p.host, p.port
                ),
                FailureKind::Network,
            ));
        }
        other => {
            return Err(TunnelError::new(
                format!(
                    "proxy {}:{} chose an unknown socks5 auth method {other}",
                    p.host, p.port
                ),
                FailureKind::Network,
            ));
        }
    }

    let mut req = vec![0x05, 0x01, 0x00];
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        req.push(0x01);
        req.extend_from_slice(&v4.octets());
    } else if let Ok(v6) = host.parse::<Ipv6Addr>() {
        req.push(0x04);
        req.extend_from_slice(&v6.octets());
    } else {
        let bytes = host.as_bytes();
        if bytes.len() > RFC1929_MAX {
            return Err(TunnelError::new(
                format!(
                    "target host is longer than {RFC1929_MAX} bytes, which socks5 cannot carry"
                ),
                FailureKind::Config,
            ));
        }
        req.push(0x03);
        req.push(bytes.len() as u8);
        req.extend_from_slice(bytes);
    }
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await.map_err(io_err)?;

    let mut head = [0u8; 4]; // VER REP RSV ATYP
    stream.read_exact(&mut head).await.map_err(io_err)?;
    if head[0] != 0x05 {
        return Err(TunnelError::new(
            format!(
                "proxy {}:{} sent a malformed socks5 reply",
                p.host, p.port
            ),
            FailureKind::Network,
        ));
    }
    if head[1] != 0x00 {
        return Err(TunnelError::new(
            format!("proxy {}:{} socks5 reply {}", p.host, p.port, head[1]),
            FailureKind::Network,
        ));
    }
    // Skip BND.ADDR + BND.PORT: the bound address is informative, never dialed.
    let skip = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let len = stream.read_u8().await.map_err(io_err)? as usize;
            len + 2
        }
        _ => {
            return Err(TunnelError::new(
                format!(
                    "proxy {}:{} sent a malformed socks5 reply",
                    p.host, p.port
                ),
                FailureKind::Network,
            ));
        }
    };
    let mut rest = vec![0u8; skip];
    stream.read_exact(&mut rest).await.map_err(io_err)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use base64::Engine;
    use tokio::net::TcpListener;

    use super::super::ssh::{SshConnection, SshHooks};
    use super::super::types::{AuthType, FailureKind, SshConnDef};
    use super::*;
    use crate::tunnel::sshtest::{self, SocksAuth};

    fn proxied_def(proxy: &str, ssh_port: u16) -> SshConnDef {
        SshConnDef {
            id: "c1".into(),
            name: "via proxy".into(),
            host: "localhost".into(), // a NAME, so socks5 must carry it as ATYP 03
            port: ssh_port,
            username: "u".into(),
            auth_type: AuthType::Password,
            group: None,
            key_path: None,
            passphrase: None,
            password: Some("pw".into()),
            host_key: None,
            proxy: Some(proxy.into()),
            proxy_username: None,
            proxy_password: None,
            jump: None,
        }
    }

    async fn connect_expect(def: SshConnDef) -> Arc<SshConnection> {
        let conn = SshConnection::new(def, SshHooks::default()); // new() hands back an Arc
        conn.connect().await.expect("the proxied handshake");
        assert!(conn.connected());
        conn
    }

    /// docs/27 §2.6.2, no credentials: the greeting offers exactly \x00, the connect
    /// request carries the hostname as ATYP 03 plus the network-order port, and the SSH
    /// handshake completes through the pump.
    #[tokio::test]
    async fn socks5_dial_hands_the_ssh_session_through_the_proxy() {
        let ssh_port = sshtest::spawn_ssh_server().await;
        let (proxy_port, mut obs) = sshtest::spawn_socks5_proxy(SocksAuth::None, 0).await;
        let conn = connect_expect(proxied_def(
            &format!("socks5://127.0.0.1:{proxy_port}"),
            ssh_port,
        ))
        .await;
        let o = obs.recv().await.expect("the observations");
        assert_eq!(o.methods, vec![0x00], "one method offered: no auth");
        assert!(o.user_pass.is_empty());
        assert_eq!(o.atyp, 0x03, "a hostname target goes to the proxy verbatim");
        assert_eq!(o.host, b"localhost".to_vec());
        assert_eq!(o.port, ssh_port, "the port is network-order at the proxy");
        let _ = conn.end().await;
    }

    /// docs/27 §2.6.2, with credentials: the greeting offers \x00+\x02 and the RFC 1929
    /// blob is byte-exact — VER, the lengths, and the two values as sent.
    #[tokio::test]
    async fn socks5_dial_offers_rfc1929_credentials_when_configured() {
        let ssh_port = sshtest::spawn_ssh_server().await;
        let (proxy_port, mut obs) = sshtest::spawn_socks5_proxy(SocksAuth::UserPass, 0).await;
        let mut def = proxied_def(&format!("socks5://127.0.0.1:{proxy_port}"), ssh_port);
        def.proxy_username = Some("pxuser".into());
        def.proxy_password = Some("pxpass".into());
        let conn = connect_expect(def).await;
        let o = obs.recv().await.expect("the observations");
        assert_eq!(o.methods, vec![0x00, 0x02]);
        let mut blob = vec![0x01, 6]; // VER ULEN ("pxuser" is 6 bytes)
        blob.extend_from_slice(b"pxuser");
        blob.push(6); // PLEN
        blob.extend_from_slice(b"pxpass");
        assert_eq!(o.user_pass, blob, "the RFC 1929 blob is byte-exact");
        assert_eq!(o.atyp, 0x03);
        assert_eq!(o.host, b"localhost".to_vec());
        assert_eq!(o.port, ssh_port);
        let _ = conn.end().await;
    }

    /// docs/27 §2.6.3: the CONNECT request line, the Host header, and the Basic
    /// authorization built from the two component fields — then the session completes.
    #[tokio::test]
    async fn http_connect_dials_through_the_proxy_and_carries_basic_auth() {
        let ssh_port = sshtest::spawn_ssh_server().await;
        let (proxy_port, mut requests) =
            sshtest::spawn_http_proxy("HTTP/1.1 200 Connection established").await;
        let mut def = proxied_def(&format!("http://127.0.0.1:{proxy_port}"), ssh_port);
        def.proxy_username = Some("pxuser".into());
        def.proxy_password = Some("pxpass".into());
        let conn = connect_expect(def).await;
        let req = requests.recv().await.expect("the CONNECT request");
        let target = format!("localhost:{ssh_port}");
        assert!(
            req.starts_with(&format!("CONNECT {target} HTTP/1.1\r\n")),
            "request line first, got: {req}"
        );
        assert!(req.contains(&format!("Host: {target}\r\n")), "{req}");
        let basic = base64::engine::general_purpose::STANDARD.encode("pxuser:pxpass");
        assert!(
            req.contains(&format!("Proxy-Authorization: Basic {basic}\r\n")),
            "{req}"
        );
        let _ = conn.end().await;
    }

    /// docs/27 §2.6.3: a non-2xx answer is a network failure naming the proxy and the code.
    #[tokio::test]
    async fn http_connect_refusal_is_a_network_failure() {
        let (proxy_port, _requests) =
            sshtest::spawn_http_proxy("HTTP/1.1 407 Proxy Authentication Required").await;
        let def = proxied_def(&format!("http://127.0.0.1:{proxy_port}"), 22);
        let conn = Arc::new(SshConnection::new(def, SshHooks::default()));
        let err = conn.connect().await.expect_err("the 407");
        assert_eq!(err.kind, FailureKind::Network);
        assert!(
            err.message.contains("proxy") && err.message.contains("HTTP 407"),
            "{}",
            err.message
        );
    }

    /// docs/27 §2.6.4: REP != 0 is a network failure; the retry policy may run.
    #[tokio::test]
    async fn a_socks5_connect_refusal_is_a_network_failure() {
        let (proxy_port, _obs) = sshtest::spawn_socks5_proxy(SocksAuth::None, 1).await;
        let def = proxied_def(&format!("socks5://127.0.0.1:{proxy_port}"), 22);
        let conn = Arc::new(SshConnection::new(def, SshHooks::default()));
        let err = conn.connect().await.expect_err("the refusal");
        assert_eq!(err.kind, FailureKind::Network);
        assert!(
            err.message.contains("socks5 reply"),
            "{}",
            err.message
        );
    }

    /// docs/27 §2.6.5: a credential reference that does not resolve fails as Config,
    /// naming the field and the reference, BEFORE any socket to the proxy is opened —
    /// docs/19 D4's strict contract, same shape as the passphrase one.
    #[tokio::test]
    async fn a_broken_proxy_credential_reference_fails_as_config_before_any_dial() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let proxy_port = listener.local_addr().expect("proxy addr").port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = accepts.clone();
        tokio::spawn(async move {
            while let Ok((_s, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut def = proxied_def(&format!("socks5://127.0.0.1:{proxy_port}"), 22);
        def.proxy_password = Some("${secret://no-such-proxy-secret}".into());
        let conn = Arc::new(SshConnection::new(def, SshHooks::default()));
        let err = conn.connect().await.expect_err("the missing reference");
        assert_eq!(err.kind, FailureKind::Config);
        assert!(
            err.message.contains("proxy password")
                && err.message.contains("secret://no-such-proxy-secret")
                && err.message.contains("not in the vault"),
            "{}",
            err.message
        );
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            0,
            "nothing may reach the proxy before the reference resolves"
        );
    }

    /// docs/27 §2.3: a proxy that offers no acceptable method (\xFF) is a network
    /// failure — the verbatim message says "auth method", so its kind is pinned here
    /// rather than through the classifier (see the note in ssh.rs's pin test).
    #[tokio::test]
    async fn a_socks5_method_refusal_is_a_network_failure() {
        // The proxy wants RFC 1929 only; the client offered just \x00, so the
        // intersection is empty.
        let (proxy_port, _obs) = sshtest::spawn_socks5_proxy(SocksAuth::UserPass, 0).await;
        let def = proxied_def(&format!("socks5://127.0.0.1:{proxy_port}"), 22);
        let conn = SshConnection::new(def, SshHooks::default());
        let err = conn.connect().await.expect_err("the method refusal");
        assert_eq!(err.kind, FailureKind::Network);
        assert!(
            err.message.contains("offers no supported socks5 auth method"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parse_accepts_the_two_schemes_and_fills_default_ports() {
        let p = parse("http://proxy.lan").expect("http");
        assert_eq!(p.scheme, ProxyScheme::Http);
        assert_eq!(p.host, "proxy.lan");
        assert_eq!(p.port, 80);
        let p = parse("socks5://proxy.lan").expect("socks5");
        assert_eq!(p.scheme, ProxyScheme::Socks5);
        assert_eq!(p.port, 1080);
        let p = parse("socks5://[::1]:1080").expect("bracketed v6");
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, 1080);
        assert!(parse("socks5://u:p@h:1080").is_err(), "no userinfo");
        assert!(parse("ftp://h").is_err(), "scheme whitelist");
    }
}
