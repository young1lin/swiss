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

//! The one place this gateway keeps its state — port of `datadir.ts` + `port.ts`.
//!
//! Why a fixed home and not cwd: the binary can be invoked from any directory, so a cwd-relative
//! token would scatter across projects. `~/.mcp-gateway` is found regardless of cwd, which is what
//! makes the token "global" the way the panel and the skill expect.

use std::path::PathBuf;

/// The port the gateway listens on when nothing else names one.
pub const DEFAULT_PORT: u16 = 19999;

/// `~/.mcp-gateway`, or `$SWISS_HOME` / `$MCP_GATEWAY_HOME` when set — the new name wins
/// when both are, the legacy one keeps every existing shell and script working. Read fresh from
/// the env each call so a test that sets one is honored without a reload.
pub fn data_dir() -> PathBuf {
    std::env::var_os("SWISS_HOME")
        .or_else(|| std::env::var_os("MCP_GATEWAY_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".mcp-gateway"))
}

/// A scratch data directory for the whole test binary, installed once into the legacy
/// `MCP_GATEWAY_HOME` so that no unit test can read or write the real `~/.mcp-gateway`. The
/// legacy name on purpose: the whole test suite plants its scratch home through that variable
/// and nothing in a test binary ever sets `SWISS_HOME`, so the fallback keeps one shared
/// scratch home for them all.
///
/// `set_var` is unsafe in edition 2024 because it races other threads; this one runs once, under
/// a `OnceLock`, and sets a variable no test reads before calling this.
/// Also built under the `test-utils` feature so other crates' test binaries get the same
/// isolated scratch home (dev-dependencies only).
#[cfg(any(test, feature = "test-utils"))]
pub fn test_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("swiss-test-home-{}", crate::util::random_hex(8)));
        let _ = std::fs::create_dir_all(&dir);
        unsafe { std::env::set_var("MCP_GATEWAY_HOME", &dir) };
        dir
    })
    .clone()
}

/// Serializes the tests that write into the scratch data dir. One home serves the whole lib test
/// binary, so a test that plants a pid file or a sealed state file must not run beside one that
/// reads the directory whole. A tokio mutex because the daemon tests hold it across awaits; the
/// synchronous callers take it with `blocking_lock`, which is what a plain `#[test]` needs.
/// Also built under `test-utils` so the composition crate's own unit tests share the same
/// serialization (dev-dependencies only).
#[cfg(any(test, feature = "test-utils"))]
pub static DATA_DIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A path inside the data dir.
pub fn data_path(segments: &[&str]) -> PathBuf {
    let mut p = data_dir();
    for s in segments {
        p.push(s);
    }
    p
}

/// The user's home directory, honoring USERPROFILE on Windows (Rust's `home_dir` was removed from
/// std; this is the same two-step lookup Node's `os.homedir()` does).
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A usable TCP listen port from anything config or an env var might hold. Digit strings are
/// accepted (env vars); anything else is not a port.
pub fn as_listen_port(v: &str) -> Option<u16> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<u16>().ok().filter(|p| *p >= 1)
}

/// The first set-and-non-empty port variable, new name first, as (name, raw value) — the
/// name comes back so a caller can report an unusable value against the variable the user
/// actually set. `None` when both are unset or empty.
pub fn env_port_raw() -> Option<(&'static str, String)> {
    ["SWISS_PORT", "MCP_GATEWAY_PORT"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|raw| !raw.is_empty())
                .map(|raw| (name, raw))
        })
}

/// The port named by `SWISS_PORT` / `MCP_GATEWAY_PORT` (new name first) when it parses to a
/// usable port. Empty/unset is `None`, not an error.
pub fn env_listen_port() -> Option<u16> {
    env_port_raw().and_then(|(_, raw)| as_listen_port(raw.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_parsing_accepts_digit_strings_only() {
        assert_eq!(as_listen_port("19999"), Some(19999));
        assert_eq!(as_listen_port(""), None);
        assert_eq!(as_listen_port("abc"), None);
        assert_eq!(as_listen_port("-1"), None);
        assert_eq!(as_listen_port("0"), None);
        assert_eq!(as_listen_port("65536"), None);
        assert_eq!(as_listen_port("65535"), Some(65535));
    }
}
