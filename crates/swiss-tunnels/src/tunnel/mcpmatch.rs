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

//! Mapping MCP paths to local forwarded ports — port of `tunnels/mcpmatch.ts`.
//!
//! THE ROUTER SEAM: `mcp_loopback_port` is what connects the MCP world to the tunnel world. An
//! MCP (mysql/redis/pg) whose connection definition points at a loopback host:port is presumed
//! to be served by the forwarding rule that binds that local port. It powers the rule editor's
//! pre-checked "Serves MCPs" suggestion (`GET /api/tunnels/suggest/:port`) and nothing else —
//! display and guard rail only, never automation. The registry-backed window itself lives in
//! the composition layer (mcp_link.rs): this module stays pure def analysis, so the tunnel
//! subsystem never names the registry's type.

#[cfg(test)]
use serde_json::Value;

use super::types::value_number;
use swiss_host::config::{resolve_def_checked, ServerDef};

fn is_loopback(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0.0.0.0"
    )
}

/// host/port out of a Postgres connection string, without a URL parser's strictness — the
/// authority sits between the last '@' and the next '/' or '?'. Hand-rolled (ADR-007).
fn pg_host_port(url: &str) -> Option<(String, u16)> {
    // postgresql://user:pass@host:port/db?opts
    let scheme = url.find("://")?;
    let rest = &url[scheme + 3..];
    // The authority ends at the first '/' OR '?' — postgresql://u:p@h:5433?sslmode=require has
    // no path, and cutting only at '/' once glued "5433?sslmode=require" into the host.
    let slash = rest.find('/');
    let q = rest.find('?');
    let cut = match (slash, q) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => rest.len(),
    };
    let authority = &rest[..cut];
    let host_part = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    if host_part.is_empty() {
        return None;
    }
    // A bare IPv6 literal has colons but no port; only treat the tail as a port when it is
    // entirely digits.
    if let Some(colon) = host_part.rfind(':') {
        let tail = &host_part[colon + 1..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            let port: u32 = tail.parse().ok()?;
            if (1..=65535).contains(&port) {
                return Some((host_part[..colon].to_string(), port as u16));
            }
        }
    }
    let host = host_part
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host_part);
    Some((host.to_string(), 5432))
}

/// The local port an MCP connects to, when it connects to loopback — otherwise None.
///
/// This is the whole of the "Serves MCPs" suggestion rule: an MCP pointing at 127.0.0.1:5433 is
/// very probably served by the rule that binds 5433. `proc` and `echo` are never matched,
/// because guessing a port out of a command line would be a guess.
///
/// `resolve_def_checked` runs first so a `${PG_WEK_URL}` reference is compared by value; the
/// resolution stays server-side and only the match result is ever sent to the browser. A
/// definition that cannot resolve (a vault reference this machine does not hold, SPEC §host.refs)
/// is simply unmatchable — None — never compared half-resolved.
pub fn mcp_loopback_port(def: &ServerDef) -> Option<u16> {
    let d = resolve_def_checked(def).ok()?;
    let type_ = d.type_();
    if type_ == "mysql" || type_ == "redis" {
        let host = d.get_str("host").unwrap_or("localhost");
        if !is_loopback(host) {
            return None;
        }
        let fallback = if type_ == "mysql" { 3306 } else { 6379 };
        let port = match d.get("port") {
            Some(v) => value_number(v),
            None => fallback as f64,
        };
        if port.fract() == 0.0 && (1.0..=65535.0).contains(&port) {
            return Some(port as u16);
        }
        return Some(fallback);
    }
    if type_ == "pg" {
        let url = d.get_str("url")?;
        if url.is_empty() {
            return None;
        }
        let (host, port) = pg_host_port(url)?;
        if !is_loopback(&host) {
            return None;
        }
        return Some(port);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(type_: &str, fields: Value) -> ServerDef {
        let mut v = json!({ "type": type_ });
        if let (Some(dst), Some(src)) = (v.as_object_mut(), fields.as_object()) {
            for (k, val) in src {
                dst.insert(k.clone(), val.clone());
            }
        }
        ServerDef(v.as_object().cloned().unwrap())
    }

    #[test]
    fn an_unresolvable_definition_is_unmatchable() {
        // SPEC §host.refs: a definition holding a vault reference this machine does not hold can
        // never be compared half-resolved — it answers None, same as "no loopback port".
        assert_eq!(
            mcp_loopback_port(&def(
                "mysql",
                json!({ "host": "127.0.0.1", "port": 3307, "password": "${secret://gone}" })
            )),
            None
        );
    }

    #[test]
    fn loopback_hosts_only() {
        assert!(
            mcp_loopback_port(&def("mysql", json!({ "host": "127.0.0.1", "port": 3307 })))
                .is_some()
        );
        assert_eq!(
            mcp_loopback_port(&def("mysql", json!({ "host": "127.0.0.1", "port": 3307 }))),
            Some(3307)
        );
        assert_eq!(
            mcp_loopback_port(&def("mysql", json!({ "host": "localhost" }))),
            Some(3306),
            "the mysql default port"
        );
        assert_eq!(
            mcp_loopback_port(&def("redis", json!({ "host": "[::1]" }))),
            Some(6379),
            "bracketed IPv6 loopback and the redis default"
        );
        assert_eq!(
            mcp_loopback_port(&def("redis", json!({ "host": "db.lan" }))),
            None
        );
        // `0.0.0.0` counts as loopback-matching, exactly as in the Node build.
        assert!(mcp_loopback_port(&def("mysql", json!({ "host": "0.0.0.0" }))).is_some());
        // proc/echo never match — no port to read off a command line.
        assert_eq!(
            mcp_loopback_port(&def("proc", json!({ "command": "x" }))),
            None
        );
    }

    #[test]
    fn pg_urls_parse_by_hand() {
        assert_eq!(
            mcp_loopback_port(&def(
                "pg",
                json!({ "url": "postgresql://u:p@127.0.0.1:5433/mydb" })
            )),
            Some(5433)
        );
        assert_eq!(
            mcp_loopback_port(&def(
                "pg",
                json!({ "url": "postgresql://u:p@localhost/db" })
            )),
            Some(5432),
            "no port -> 5432"
        );
        assert_eq!(
            mcp_loopback_port(&def(
                "pg",
                json!({ "url": "postgresql://u@127.0.0.1:5433?sslmode=require" })
            )),
            Some(5433),
            "query string ends the authority, not just a path"
        );
        assert_eq!(
            mcp_loopback_port(&def(
                "pg",
                json!({ "url": "postgresql://db.example.com/x" })
            )),
            None
        );
        assert_eq!(
            mcp_loopback_port(&def("pg", json!({}))),
            None,
            "no url at all"
        );
        // A bare IPv6 literal has colons but no numeric tail port.
        assert_eq!(
            pg_host_port("postgresql://u@[::1]/db").map(|(h, _)| h),
            Some("::1".into())
        );
    }

    #[test]
    fn ports_read_like_js_number() {
        assert_eq!(
            mcp_loopback_port(&def(
                "mysql",
                json!({ "host": "localhost", "port": "3307" })
            )),
            Some(3307),
            "a numeric-string port works, as Number(d.port) did"
        );
        assert_eq!(
            mcp_loopback_port(&def("mysql", json!({ "host": "localhost", "port": 0 }))),
            Some(3306),
            "an unusable port falls back to the default"
        );
    }
}
