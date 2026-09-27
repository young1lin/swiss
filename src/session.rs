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

//! The admin session (docs/48): who may use `/api/*` and the panel.
//!
//! The loopback checks keep other machines and rebinding pages out; they cannot tell one local
//! process from another, so until this module any process on the machine could read every
//! connection and run remote commands with one `curl`. Two credentials now open the admin
//! surface, on top of those checks, never instead of them:
//!
//! - a **browser session**: a cookie `swiss_session_<port>` signed with HMAC-SHA256 under a
//!   signing key that survives restarts, so a signed-in browser stays signed in. A browser gets
//!   one only by redeeming a **login token** on `GET /?token=` - single-use and good for
//!   [`TICKET_TTL_MS`], minted for `swiss start` / `swiss open` (docs/48 §3: RH's per-process
//!   startup token, tightened to one use);
//! - the **CLI key**, sent as `X-Swiss-Key`: rotated on every daemon start and readable only by
//!   unsealing `session.json`, i.e. by the same OS user on the same machine.
//!
//! Both secrets live in `SWISS_HOME/session.json`, sealed like every other state file.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use swiss_core::util::{now_ms, random_hex};

/// The header the CLI sends its key in. Not `Authorization`: that one carries MCP bearer
/// tokens, and the two credentials must never be mistaken for each other.
pub const CLI_KEY_HEADER: &str = "x-swiss-key";
/// The state file both secrets live in, under SWISS_HOME.
pub const SESSION_FILE: &str = "session.json";
/// A login token is good for this long after it is minted, and for one use.
pub const TICKET_TTL_MS: u64 = 120_000;
/// A browser session lasts this long; signing in again starts a new one.
pub const SESSION_TTL_MS: u64 = 30 * 24 * 3600 * 1000;
/// Unredeemed login tokens kept at most - a loop minting them cannot grow the map.
const MAX_TICKETS: usize = 16;
/// Clock skew tolerated on a cookie's issue time.
const SKEW_MS: u64 = 5 * 60 * 1000;

type HmacSha256 = Hmac<Sha256>;

pub struct AdminSession {
    port: u16,
    signing: Vec<u8>,
    cli_key: String,
    /// Unredeemed login tokens with their expiry, oldest first: every token lives the same
    /// TTL, so insertion order is expiry order.
    tickets: Mutex<VecDeque<(String, u64)>>,
}

/// What `session.json` holds after a daemon start: the signing key carried over (or made the
/// first time), and a fresh CLI key. Pure, so the rotation rule is tested without a master key.
pub fn next_state(existing: Option<&Value>) -> Value {
    let signing = existing
        .and_then(|v| v.get("signingKey"))
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
        .unwrap_or_else(|| random_hex(32));
    json!({ "signingKey": signing, "cliKey": random_hex(32) })
}

/// The CLI key the running daemon accepts, read from the sealed `session.json`. None when the
/// gateway never started under this home, or the file cannot be unsealed.
pub fn read_cli_key(path: &Path) -> Option<String> {
    let state = swiss_core::secure::statefile::read_secure_json(path).ok()??;
    state
        .get("cliKey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl AdminSession {
    /// The daemon's boot step: keep the signing key, rotate the CLI key, seal the file back.
    pub fn load_or_create(path: &Path, port: u16) -> Result<Arc<Self>, String> {
        let existing = swiss_core::secure::statefile::read_secure_json(path)
            .map_err(|e| format!("session: {e}"))?;
        let state = next_state(existing.as_ref());
        swiss_core::secure::statefile::write_secure_json(path, &state)
            .map_err(|e| format!("session: {e}"))?;
        Self::from_state(&state, port)
    }

    /// A session over a given state (`next_state`'s shape) - what tests build directly.
    pub fn from_state(state: &Value, port: u16) -> Result<Arc<Self>, String> {
        let signing = state
            .get("signingKey")
            .and_then(Value::as_str)
            .and_then(decode_hex)
            .filter(|k| k.len() == 32)
            .ok_or("session: signingKey is not 32 bytes of hex")?;
        let cli_key = state
            .get("cliKey")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("session: cliKey is missing")?
            .to_string();
        Ok(Arc::new(AdminSession {
            port,
            signing,
            cli_key,
            tickets: Mutex::new(VecDeque::new()),
        }))
    }

    pub fn cookie_name(&self) -> String {
        format!("swiss_session_{}", self.port)
    }

    /// A fresh single-use login token.
    pub fn mint_ticket(&self) -> String {
        let now = now_ms();
        let token = random_hex(32);
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|(_, expires)| *expires > now);
        while tickets.len() >= MAX_TICKETS {
            // The oldest goes: the newest request is the one a person is waiting on.
            tickets.pop_front();
        }
        tickets.push_back((token.clone(), now + TICKET_TTL_MS));
        token
    }

    /// Spend a login token: true once, for a token minted here and not yet expired.
    pub fn redeem_ticket(&self, token: &str) -> bool {
        let now = now_ms();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|(_, expires)| *expires > now);
        match tickets.iter().position(|(t, _)| constant_time_eq(t.as_bytes(), token.as_bytes())) {
            Some(at) => tickets.remove(at).is_some(),
            None => false,
        }
    }

    /// The `Set-Cookie` value that signs a browser in for [`SESSION_TTL_MS`].
    pub fn issue_cookie(&self) -> String {
        let issued = now_ms();
        let nonce = random_hex(16);
        let mac = self.mac(issued, &nonce);
        format!(
            "{}=v1.{issued}.{nonce}.{mac}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
            self.cookie_name(),
            SESSION_TTL_MS / 1000
        )
    }

    /// Whether the request carries this gateway's CLI key or a valid session cookie.
    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        self.cli_key_ok(headers) || self.cookie_ok(headers)
    }

    pub fn cli_key_ok(&self, headers: &HeaderMap) -> bool {
        headers
            .get(CLI_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|k| constant_time_eq(k.as_bytes(), self.cli_key.as_bytes()))
    }

    pub fn cookie_ok(&self, headers: &HeaderMap) -> bool {
        let name = self.cookie_name();
        headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .filter(|(k, _)| *k == name)
            .any(|(_, value)| self.cookie_value_ok(value))
    }

    fn cookie_value_ok(&self, value: &str) -> bool {
        let mut parts = value.split('.');
        let (Some("v1"), Some(issued), Some(nonce), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let Ok(issued) = issued.parse::<u64>() else {
            return false;
        };
        let now = now_ms();
        if issued > now + SKEW_MS || now.saturating_sub(issued) > SESSION_TTL_MS {
            return false;
        }
        constant_time_eq(self.mac(issued, nonce).as_bytes(), mac.as_bytes())
    }

    /// The cookie's signature: the port is inside it, so a cookie minted for one instance is
    /// never accepted by another that happens to share a signing key (a copied test home).
    fn mac(&self, issued: u64, nonce: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.signing).expect("HMAC takes a key of any length");
        mac.update(format!("swiss-session|v1|{}|{issued}|{nonce}", self.port).as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Equal-length inputs compared without an early exit, so a guess's timing says nothing about
/// how many leading bytes were right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn session(port: u16) -> Arc<AdminSession> {
        AdminSession::from_state(&next_state(None), port).expect("a session")
    }

    fn with_cookie(set_cookie: &str) -> HeaderMap {
        let pair = set_cookie.split(';').next().expect("name=value");
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::COOKIE, HeaderValue::from_str(&format!("theme=dark; {pair}")).unwrap());
        h
    }

    #[test]
    fn a_restart_keeps_the_signing_key_and_rotates_the_cli_key() {
        let first = next_state(None);
        let second = next_state(Some(&first));
        assert_eq!(first["signingKey"], second["signingKey"]);
        assert_ne!(first["cliKey"], second["cliKey"]);
        // A damaged signing key is replaced, not trusted.
        let broken = json!({ "signingKey": "zz", "cliKey": "x" });
        assert_eq!(next_state(Some(&broken))["signingKey"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn a_login_token_works_once_and_only_if_minted_here() {
        let s = session(19999);
        let t = s.mint_ticket();
        assert!(!s.redeem_ticket("not-a-ticket"));
        assert!(s.redeem_ticket(&t));
        assert!(!s.redeem_ticket(&t), "single use");
        assert!(!session(19999).redeem_ticket(&s.mint_ticket()), "another process's token");
    }

    #[test]
    fn unredeemed_tokens_are_bounded() {
        let s = session(19999);
        let first = s.mint_ticket();
        for _ in 0..MAX_TICKETS {
            s.mint_ticket();
        }
        assert!(s.tickets.lock().unwrap().len() <= MAX_TICKETS);
        assert!(!s.redeem_ticket(&first), "the oldest made room");
    }

    #[test]
    fn a_session_cookie_opens_the_gate_and_a_forged_one_does_not() {
        let s = session(19999);
        let cookie = s.issue_cookie();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        assert!(s.authorized(&with_cookie(&cookie)));
        // Another key, the same shape: refused.
        assert!(!session(19999).authorized(&with_cookie(&cookie)));
        // The same key on another port: refused (the port is signed in).
        let state = json!({ "signingKey": "ab".repeat(32), "cliKey": "k" });
        let a = AdminSession::from_state(&state, 19999).unwrap();
        let b = AdminSession::from_state(&state, 19998).unwrap();
        let on_a = a.issue_cookie().replace("swiss_session_19999", "swiss_session_19998");
        assert!(!b.authorized(&with_cookie(&on_a)));
        // A tampered issue time breaks the signature.
        let pair = cookie.split(';').next().unwrap();
        let (name, value) = pair.split_once('=').unwrap();
        let mut parts: Vec<&str> = value.split('.').collect();
        let bumped = (parts[1].parse::<u64>().unwrap() + 1).to_string();
        parts[1] = &bumped;
        assert!(!s.authorized(&with_cookie(&format!("{name}={}", parts.join(".")))));
        assert!(!s.authorized(&HeaderMap::new()));
    }

    #[test]
    fn the_cli_key_opens_the_gate_only_when_exact() {
        let state = next_state(None);
        let s = AdminSession::from_state(&state, 19999).unwrap();
        let key = state["cliKey"].as_str().unwrap();
        let mut h = HeaderMap::new();
        h.insert(CLI_KEY_HEADER, HeaderValue::from_str(key).unwrap());
        assert!(s.authorized(&h));
        h.insert(CLI_KEY_HEADER, HeaderValue::from_str(&key[..key.len() - 1]).unwrap());
        assert!(!s.authorized(&h));
        // The MCP bearer slot is not the CLI key's.
        let mut bearer = HeaderMap::new();
        bearer.insert(axum::http::header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {key}")).unwrap());
        assert!(!s.authorized(&bearer));
    }
}
