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

//! The listen port and its resolution order — port of `port.ts`.

/// The port the gateway listens on when nothing else names one.
pub const DEFAULT_PORT: u16 = 19999;

/// A usable TCP listen port out of a raw string, or None. Digit strings only (env vars arrive as
/// text): whitespace is trimmed first, then strict digits + the 1..=65535 range.
pub fn as_listen_port(raw: &str) -> Option<u16> {
    let s = raw.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u16>().ok().filter(|p| *p >= 1)
}

/// A listen port out of a config value: JSON numbers or digit strings.
pub fn as_listen_port_value(v: &serde_json::Value) -> Option<u16> {
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p >= 1),
        serde_json::Value::String(s) => as_listen_port(s),
        _ => None,
    }
}

/// The port named by `SWISS_PORT` / `MCP_GATEWAY_PORT` (new name first, first
/// set-and-non-empty wins) when it parses to a usable port. Empty/unset is None, not an error.
/// The scan itself is swiss-core's so the CLI here and the config loader below can never
/// disagree about which variable wins.
pub fn env_listen_port() -> Option<u16> {
    swiss_core::paths::env_port_raw().and_then(|(_, raw)| as_listen_port(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_parse_strictly() {
        assert_eq!(as_listen_port("19999"), Some(19999));
        assert_eq!(as_listen_port(" 8080 "), Some(8080));
        assert_eq!(as_listen_port("0"), None);
        assert_eq!(as_listen_port("65536"), None);
        assert_eq!(as_listen_port("0x10"), None);
        assert_eq!(as_listen_port("1e2"), None);
        assert_eq!(as_listen_port(""), None);
    }

    #[test]
    fn the_env_port_honours_the_new_name_first() {
        // Both variables are process-wide and other tests in this binary write them, so this
        // holds the same data-dir lock every other env-planting test takes.
        let _lock = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
        // SAFETY: planted only under that lock and taken back before it releases.
        unsafe {
            std::env::remove_var("SWISS_PORT");
            std::env::remove_var("MCP_GATEWAY_PORT");

            std::env::set_var("SWISS_PORT", "18095");
            assert_eq!(env_listen_port(), Some(18095));

            // Both set: the new name wins, so an operator pinning both gets one answer.
            std::env::set_var("MCP_GATEWAY_PORT", "18096");
            assert_eq!(env_listen_port(), Some(18095));

            // The legacy name alone still steers — pre-rename scripts keep working.
            std::env::remove_var("SWISS_PORT");
            assert_eq!(env_listen_port(), Some(18096));

            // An empty new-name value is not a pin: the scan falls through to the legacy one.
            std::env::set_var("SWISS_PORT", "");
            assert_eq!(env_listen_port(), Some(18096));

            // A set-but-unusable value is not silently skipped for the other name.
            std::env::remove_var("MCP_GATEWAY_PORT");
            std::env::set_var("SWISS_PORT", "not-a-port");
            assert_eq!(env_listen_port(), None);

            std::env::remove_var("SWISS_PORT");
        }
    }
}
