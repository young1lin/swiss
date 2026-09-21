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

//! The one place this gateway keeps its state — port of `datadir.ts` + `port.ts`.
//!
//! Why a fixed home and not cwd: the binary can be invoked from any directory, so a cwd-relative
//! token would scatter across projects. `~/.swiss` is found regardless of cwd, which is what
//! makes the token "global" the way the panel and the skill expect.
//!
//! The home was `~/.mcp-gateway` until 2026-09-18 (the product's old name). A home written
//! under that name is moved to `~/.swiss` once, whole, by [migrate_legacy_home] — the sealed
//! files inside are path-independent (DPAPI binds `master.key` to machine+user, not to a
//! directory; the test-instance script has always copied it elsewhere and opened it). Until the
//! move happens, [data_dir] keeps answering with the old directory, so `swiss stop` / `status`
//! / `token` on a not-yet-migrated home still find the daemon's pid file and the token.

use std::path::{Path, PathBuf};

/// The port the gateway listens on when nothing else names one.
pub const DEFAULT_PORT: u16 = 19999;

/// The home's name under `~`.
pub const HOME_DIR_NAME: &str = ".swiss";

/// The home's name under `~` before the rename — read, and moved, never written to.
pub const LEGACY_HOME_DIR_NAME: &str = ".mcp-gateway";

/// `$SWISS_HOME` / `$MCP_GATEWAY_HOME` when set (the new name wins when both are; the legacy
/// one keeps every existing shell and script working), else [default_data_dir_in] under the
/// user's home. Read fresh from the env each call so a test that sets one is honored without
/// a reload.
pub fn data_dir() -> PathBuf {
    home_env_override().unwrap_or_else(|| default_data_dir_in(&home_dir()))
}

fn home_env_override() -> Option<PathBuf> {
    std::env::var_os("SWISS_HOME")
        .or_else(|| std::env::var_os("MCP_GATEWAY_HOME"))
        .map(PathBuf::from)
}

/// `<home>/.swiss` — unless nothing has been written there yet and `<home>/.mcp-gateway`
/// still holds a home, in which case the legacy directory keeps serving until
/// [migrate_legacy_home] moves it. An empty `.swiss` counts as "nothing written": a stray
/// `mkdir` must never hide the user's configuration.
pub fn default_data_dir_in(home: &Path) -> PathBuf {
    let new = home.join(HOME_DIR_NAME);
    if !has_entries(&new) {
        let legacy = home.join(LEGACY_HOME_DIR_NAME);
        if has_entries(&legacy) {
            return legacy;
        }
    }
    new
}

/// Whether `dir` is a directory with at least one entry.
fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// What [migrate_legacy_home] did.
#[derive(Debug, PartialEq, Eq)]
pub enum LegacyHomeMigration {
    /// No legacy home, or the new one is already in use, or an env override names the home.
    NotNeeded,
    /// The whole directory was renamed.
    Moved { from: PathBuf, to: PathBuf },
    /// A gateway is still running out of the legacy home: its open log and its next save
    /// belong to the old path, so the move waits for it to stop.
    Blocked { pid: u32, port: u16 },
    /// The rename itself failed; the legacy home is left where it was and keeps serving.
    Failed { from: PathBuf, to: PathBuf, err: String },
}

/// Move `~/.mcp-gateway` to `~/.swiss`, once. A no-op when an env override names the home
/// (the caller chose a directory; nothing to migrate), when there is no legacy home, or when
/// the new home already holds files. Safe to call from every entry point — it is idempotent.
pub fn migrate_legacy_home() -> LegacyHomeMigration {
    if home_env_override().is_some() {
        return LegacyHomeMigration::NotNeeded;
    }
    migrate_legacy_home_in(&home_dir(), &crate::platform::pid_alive)
}

/// [migrate_legacy_home] against an explicit home, with the liveness probe injected so a
/// test can plant a "live" daemon without one.
pub fn migrate_legacy_home_in(
    home: &Path,
    pid_alive: &dyn Fn(u32) -> bool,
) -> LegacyHomeMigration {
    let from = home.join(LEGACY_HOME_DIR_NAME);
    let to = home.join(HOME_DIR_NAME);
    if !has_entries(&from) || has_entries(&to) {
        return LegacyHomeMigration::NotNeeded;
    }
    if let Some((port, pid)) = live_daemon_in(&from, pid_alive) {
        return LegacyHomeMigration::Blocked { pid, port };
    }
    // An empty `.swiss` (a stray mkdir) would make the rename fail: clear it first.
    if to.is_dir() {
        if let Err(err) = std::fs::remove_dir(&to) {
            return LegacyHomeMigration::Failed { from, to, err: err.to_string() };
        }
    }
    // Windows refuses to rename a directory while any handle is open beneath it, and a
    // just-stopped daemon keeps its log and ledger open for a few seconds past the moment
    // its port closed (measured 2026-09-18: `swiss stop` back at 0.2 s, the move possible
    // at 2.9 s — the teardown plus Defender's post-write scan). `swiss restart` is exactly
    // that sequence, so the move waits it out, bounded: 50 tries, 100 ms apart.
    let mut last_err = None;
    for _ in 0..RENAME_TRIES {
        match std::fs::rename(&from, &to) {
            Ok(()) => return LegacyHomeMigration::Moved { from, to },
            Err(err) if is_transient_rename_error(&err) => {
                last_err = Some(err);
                std::thread::sleep(RENAME_RETRY_EVERY);
            }
            Err(err) => return LegacyHomeMigration::Failed { from, to, err: err.to_string() },
        }
    }
    LegacyHomeMigration::Failed {
        from,
        to,
        err: last_err.map(|e| e.to_string()).unwrap_or_default(),
    }
}

const RENAME_TRIES: usize = 50;
const RENAME_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// The errors a rename gives while a handle is still open beneath the directory: access
/// denied (os error 5) and the sharing violation (os error 32) on Windows, EBUSY on unix.
fn is_transient_rename_error(err: &std::io::Error) -> bool {
    matches!(err.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(err.raw_os_error(), Some(5) | Some(32) | Some(16))
}

/// The first `gateway-<port>.pid` in `dir` whose pid is alive, as (port, pid). The pid file
/// is the daemon's own JSON record (`src/pidfile.rs`); only its `pid` field matters here.
fn live_daemon_in(dir: &Path, pid_alive: &dyn Fn(u32) -> bool) -> Option<(u16, u32)> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(port) = name
            .strip_prefix("gateway-")
            .and_then(|s| s.strip_suffix(".pid"))
            .and_then(|s| s.parse::<u16>().ok())
        else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let pid = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.get("pid")?.as_u64())
            .and_then(|p| u32::try_from(p).ok());
        if let Some(pid) = pid.filter(|p| *p > 0 && pid_alive(*p)) {
            return Some((port, pid));
        }
    }
    None
}

/// A scratch data directory for the whole test binary, installed once into the legacy
/// `MCP_GATEWAY_HOME` so that no unit test can read or write the real `~/.swiss`. The
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

    fn scratch_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swiss-home-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch home");
        dir
    }

    fn plant_legacy(home: &Path) -> PathBuf {
        let legacy = home.join(LEGACY_HOME_DIR_NAME);
        std::fs::create_dir_all(legacy.join("logs")).expect("legacy home");
        std::fs::write(legacy.join("gateway.config.json"), "{}").expect("config");
        std::fs::write(legacy.join("logs").join("traffic.jsonl"), "").expect("traffic");
        legacy
    }

    #[test]
    fn a_fresh_machine_gets_the_new_home_name() {
        let home = scratch_home();
        assert_eq!(default_data_dir_in(&home), home.join(HOME_DIR_NAME));
        assert_eq!(
            migrate_legacy_home_in(&home, &|_| false),
            LegacyHomeMigration::NotNeeded
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_unmigrated_legacy_home_keeps_serving_until_it_is_moved() {
        let home = scratch_home();
        let legacy = plant_legacy(&home);
        // Before the move: the old directory answers, so stop/status/token still work.
        assert_eq!(default_data_dir_in(&home), legacy);
        // A stray empty `.swiss` does not hide the configuration either.
        std::fs::create_dir_all(home.join(HOME_DIR_NAME)).expect("stray mkdir");
        assert_eq!(default_data_dir_in(&home), legacy);

        let moved = migrate_legacy_home_in(&home, &|_| false);
        assert_eq!(
            moved,
            LegacyHomeMigration::Moved { from: legacy.clone(), to: home.join(HOME_DIR_NAME) }
        );
        assert!(!legacy.exists(), "the legacy directory is gone, not copied");
        let new = home.join(HOME_DIR_NAME);
        assert!(new.join("gateway.config.json").is_file());
        assert!(new.join("logs").join("traffic.jsonl").is_file());
        assert_eq!(default_data_dir_in(&home), new);
        // Idempotent: a second call finds nothing to do.
        assert_eq!(
            migrate_legacy_home_in(&home, &|_| false),
            LegacyHomeMigration::NotNeeded
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_live_daemon_blocks_the_move_and_a_dead_pid_file_does_not() {
        let home = scratch_home();
        let legacy = plant_legacy(&home);
        std::fs::write(
            legacy.join("gateway-19999.pid"),
            r#"{"pid": 4242, "port": 19999}"#,
        )
        .expect("pid file");
        assert_eq!(
            migrate_legacy_home_in(&home, &|pid| pid == 4242),
            LegacyHomeMigration::Blocked { pid: 4242, port: 19999 }
        );
        assert!(legacy.is_dir(), "blocked means untouched");
        assert_eq!(default_data_dir_in(&home), legacy);
        // The daemon stopped (its pid file is stale): the move proceeds.
        assert!(matches!(
            migrate_legacy_home_in(&home, &|_| false),
            LegacyHomeMigration::Moved { .. }
        ));
        assert!(home.join(HOME_DIR_NAME).join("gateway-19999.pid").is_file());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_populated_new_home_wins_over_a_leftover_legacy_one() {
        let home = scratch_home();
        let legacy = plant_legacy(&home);
        let new = home.join(HOME_DIR_NAME);
        std::fs::create_dir_all(&new).expect("new home");
        std::fs::write(new.join("gateway.config.json"), "{}").expect("config");
        assert_eq!(default_data_dir_in(&home), new);
        assert_eq!(
            migrate_legacy_home_in(&home, &|_| false),
            LegacyHomeMigration::NotNeeded
        );
        assert!(legacy.is_dir(), "never merged, never deleted");
        let _ = std::fs::remove_dir_all(&home);
    }
}
