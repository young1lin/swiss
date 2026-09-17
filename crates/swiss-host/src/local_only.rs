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

//! This gateway serves one machine: the one it runs on — port of `local-only.ts`.
//!
//! It holds live database credentials, SSH keys and third-party API keys, and hands whoever
//! reaches it a tool set that reads and writes production-adjacent data. There is no multi-user
//! model, no per-user authorization, and no intent to grow one — the token identifies a client,
//! not a person. So "local" is not a default here, it is the boundary, and it is enforced in
//! three places because binding to loopback alone does not achieve it:
//!
//! - the bind address, so the socket is never offered to another machine;
//! - the peer address, so the guarantee holds even if something else changed the bind;
//! - the `Host` header, which is the only thing that reveals a DNS-rebinding page — its domain
//!   resolves to 127.0.0.1, so the connection really is local, and every check above it passes.
//!
//! `Origin` is checked too: a page on another site cannot read our answers (no CORS headers are
//! sent), but a request with side effects still lands, and a refusal costs nothing.
//!
//! There is deliberately no opt-out. Reaching this gateway from another machine is what SSH port
//! forwarding is for.

/// `localhost`, `::1`, or anything in 127.0.0.0/8 — matched whole, so `localhost.evil.test` does
/// not. Hand-rolled (ADR-007): a lowercase compare plus an IPv4-dotted-quad check.
pub fn is_loopback_name(name: &str) -> bool {
    let s = name.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    if s == "localhost" || s == "::1" || s == "[::1]" {
        return true;
    }
    // 127.x.x.x with each octet 1-3 digits. Digits-only check (the Node regex also allowed
    // non-canonical forms like 127.999.1.1 — `all(digits)` accepts them equally).
    let mut parts = s.split('.');
    let mut first = true;
    for _ in 0..4 {
        match parts.next() {
            Some(p) => {
                if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
                    return false;
                }
                if first {
                    if p != "127" {
                        return false;
                    }
                    first = false;
                }
            }
            None => return false,
        }
    }
    parts.next().is_none()
}

/// Whether `host` from the config names only this machine.
///
/// `0.0.0.0` and `::` are the ones worth naming: they read like "the local one" and mean "every
/// interface", which is exactly how a dev gateway ends up answering the office LAN.
pub fn is_loopback_bind_host(host: &str) -> bool {
    let h = host.trim();
    if h == "::" || h == "[::]" {
        return false;
    }
    is_loopback_name(h)
}

/// A connected peer's address string. A loopback client on a dual-stack socket reports
/// `::ffff:127.0.0.1`, which is the same machine and has to be accepted.
pub fn is_loopback_peer(addr: &str) -> bool {
    let s = addr.trim().to_ascii_lowercase();
    let stripped = s.strip_prefix("::ffff:").unwrap_or(&s);
    is_loopback_name(stripped)
}

/// The host part of a `Host` header, without its port. Bracketed IPv6 keeps its brackets; an
/// unbracketed IPv6 literal (malformed, but a client may send one) is returned whole rather than
/// chopped at its first colon.
pub fn hostname_of(host_header: &str) -> &str {
    let s = host_header.trim();
    if let Some(rest) = s.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => &s[..end + 2], // keeps the brackets, like the Node build
            None => s,
        };
    }
    if s.matches(':').count() > 1 {
        return s; // unbracketed IPv6 — nothing to strip
    }
    match s.find(':') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `Origin` is loopback when its URL's hostname is. `null` (an opaque origin — sandboxed iframe,
/// file://) is NOT this machine.
pub fn is_loopback_origin(origin: &str) -> bool {
    if origin == "null" {
        return false;
    }
    // Extract `://authority` then cut the authority at /, ?, # — the pieces new URL().hostname
    // computed; everything else is not parseable and therefore not ours.
    let Some(rest) = origin.split_once("://") else {
        return false;
    };
    let authority = rest.1.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once('@') {
        Some((_, h)) => h,
        None => authority,
    };
    let host = host
        .strip_prefix('[')
        .map(|h| match h.find(']') {
            Some(end) => &h[..end + 1],
            None => h,
        })
        .unwrap_or(match host.rsplit_once(':') {
            // Only strip a trailing :port when what precedes it is not itself IPv6-shaped.
            Some((h, port))
                if port.is_empty()
                    || port.bytes().all(|b| b.is_ascii_digit()) && h.matches(':').count() <= 1 =>
            {
                h
            }
            _ => host,
        });
    !host.is_empty() && is_loopback_name(host)
}

/// Why this request is not local, or None when it is. The reason is returned rather than thrown
/// so the caller decides the status code, and so it can be logged verbatim.
pub fn remote_request_reason(
    peer_addr: Option<&str>,
    host: Option<&str>,
    origin: Option<&str>,
) -> Option<String> {
    let peer = peer_addr.unwrap_or("unknown");
    if !is_loopback_peer(peer) {
        return Some(format!(
            "this gateway serves only its own machine (peer address {peer})"
        ));
    }
    let host = host.unwrap_or("");
    if host.is_empty() || !is_loopback_name(hostname_of(host)) {
        let shown = if host.is_empty() {
            "no Host header".to_string()
        } else {
            format!("\"{host}\"")
        };
        return Some(format!("Host must name this machine, got {shown}"));
    }
    if let Some(origin) = origin {
        if !origin.is_empty() && !is_loopback_origin(origin) {
            return Some(format!("Origin \"{origin}\" is not this machine"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    // Ported from test/local-only.test.ts — the security boundary's own spec.
    use super::*;

    #[test]
    fn loopback_names() {
        for name in [
            "localhost",
            "127.0.0.1",
            "127.0.0.2",
            "127.1.2.3",
            "::1",
            "[::1]",
            "LOCALHOST",
            "  localhost  ",
        ] {
            assert!(is_loopback_name(name), "{name} should be loopback");
        }
        for name in [
            "localhost.evil.test",
            "evil.test",
            "10.0.0.1",
            "0.0.0.0",
            "192.168.1.1",
            "::",
            "127",
            "127.1",
            "127.0.0",
            "127.0.0.0.1",
            "",
        ] {
            assert!(!is_loopback_name(name), "{name} should NOT be loopback");
        }
    }

    #[test]
    fn bind_hosts() {
        assert!(is_loopback_bind_host("127.0.0.1"));
        assert!(is_loopback_bind_host("localhost"));
        assert!(!is_loopback_bind_host("0.0.0.0"));
        assert!(!is_loopback_bind_host("::"));
        assert!(!is_loopback_bind_host("192.168.0.10"));
    }

    #[test]
    fn hostnames_lose_their_port() {
        assert_eq!(hostname_of("127.0.0.1:19999"), "127.0.0.1");
        assert_eq!(hostname_of("localhost:19999"), "localhost");
        assert_eq!(hostname_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(hostname_of("[::1]:19999"), "[::1]");
    }

    #[test]
    fn peer_addresses() {
        assert!(is_loopback_peer("127.0.0.1"));
        assert!(is_loopback_peer("::ffff:127.0.0.1"));
        assert!(is_loopback_peer("::1"));
        assert!(!is_loopback_peer("192.168.1.5"));
        assert!(!is_loopback_peer("::ffff:192.168.1.5"));
    }

    #[test]
    fn origins() {
        assert!(is_loopback_origin("http://127.0.0.1:19999"));
        assert!(is_loopback_origin("http://localhost:19999"));
        assert!(!is_loopback_origin("http://evil.test"));
        assert!(!is_loopback_origin("null"));
        assert!(!is_loopback_origin("not a url"));
    }

    #[test]
    fn remote_reasons() {
        // Local through and through.
        assert_eq!(
            remote_request_reason(Some("127.0.0.1"), Some("127.0.0.1:19999"), None),
            None
        );
        assert_eq!(
            remote_request_reason(
                Some("::ffff:127.0.0.1"),
                Some("localhost:19999"),
                Some("http://localhost:19999")
            ),
            None
        );
        // Remote peer.
        assert!(
            remote_request_reason(Some("192.168.1.5"), Some("127.0.0.1:19999"), None).is_some()
        );
        // Rebind: local peer but a foreign Host.
        let reason =
            remote_request_reason(Some("127.0.0.1"), Some("evil.test:19999"), None).unwrap();
        assert!(reason.contains("Host must name this machine"));
        // Foreign Origin.
        let reason = remote_request_reason(
            Some("127.0.0.1"),
            Some("127.0.0.1:19999"),
            Some("http://evil.test"),
        )
        .unwrap();
        assert!(reason.contains("Origin"));
    }
}
