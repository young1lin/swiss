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

//! OS-level start-at-sign-in — the one host setting that lives outside the process: a registry
//! Run value on Windows, a LaunchAgent plist on macOS, a systemd user unit on Linux. Nothing is
//! ever spawned to register it (AGENTS.md: no subprocess where a syscall exists) — all three
//! providers are plain registry or file writes the OS itself honours at login.
//!
//! The registered command is always the current exe plus "start --no-open": start already
//! detaches the gateway into a hidden child whose output lands in the log file, and a sign-in
//! must not pop a browser. State never moves — the sealed files under the home directory are
//! untouched by an exe swap, which is the same property manual updates rely on.

use serde_json::{json, Value};

/// What the panel and the CLI both read: whether the OS will start swiss at sign-in, where the
/// registration lives, and the exact command the entry runs.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoStartState {
    pub enabled: bool,
    /// Where the registration is recorded — a registry path, a plist path, a unit path. Shown
    /// to the operator so "is it on?" has a checkable answer outside the panel.
    pub detail: String,
    /// The command the OS entry would run. Display only; the stored entry is the authority.
    pub command: String,
}

impl AutoStartState {
    /// The /api/autostart shape: camelCase keys, same as every admin answer.
    pub fn to_json(&self) -> Value {
        json!({ "enabled": self.enabled, "detail": self.detail, "command": self.command })
    }
}

/// This binary's path as a plain string ("swiss" when the OS will not say — enable() then
/// honestly records a PATH lookup, which fails visibly at the next sign-in if it does not).
fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "swiss".to_string())
}

/// The command every provider registers: this binary, detached, without the browser. The path
/// is quoted because "Program Files" and %LOCALAPPDATA% both contain spaces.
pub fn command_line() -> String {
    format!("\"{}\" start --no-open", exe_path())
}

// --- the three providers -------------------------------------------------------------------------

#[cfg(windows)]
mod provider {
    use super::AutoStartState;

    const DETAIL: &str =
        "registry: HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run (value: swiss)";

    pub fn status() -> AutoStartState {
        AutoStartState {
            enabled: swiss_core::platform::run_entry_read().is_some(),
            detail: DETAIL.to_string(),
            command: super::command_line(),
        }
    }

    pub fn enable() -> Result<AutoStartState, String> {
        let cmd = super::command_line();
        swiss_core::platform::run_entry_write(&cmd)
            .map_err(|err| format!("registry write failed: {err}"))?;
        Ok(status())
    }

    pub fn disable() -> Result<AutoStartState, String> {
        swiss_core::platform::run_entry_remove()
            .map_err(|err| format!("registry delete failed: {err}"))?;
        Ok(status())
    }
}

#[cfg(target_os = "macos")]
mod provider {
    use super::AutoStartState;
    use std::path::PathBuf;

    const LABEL: &str = "dev.swiss.swiss";

    fn plist_path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LABEL}.plist"))
        })
    }

    /// XML-escape a path for the plist body — an unescaped ampersand is rare in paths but
    /// legal, and one would make launchd refuse the whole file.
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// The LaunchAgent body, pure so the suite can pin it: RunAtLoad starts the gateway at
    /// sign-in; no KeepAlive — the gateway is not a service that must be resurrected, and
    /// "swiss start" remains the honest way to bring it back after a stop or crash.
    pub fn plist_body(exe: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>start</string>\n    <string>--no-open</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
            xml_escape(exe)
        )
    }

    pub fn status() -> AutoStartState {
        let path = plist_path();
        AutoStartState {
            enabled: path.as_ref().is_some_and(|p| p.exists()),
            detail: format!(
                "launch agent: {}",
                path.as_ref()
                    .map_or("(no HOME)".to_string(), |p| p.display().to_string())
            ),
            command: super::command_line(),
        }
    }

    pub fn enable() -> Result<AutoStartState, String> {
        let Some(path) = plist_path() else {
            return Err("cannot find the home directory (HOME)".to_string());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        std::fs::write(&path, plist_body(&super::exe_path()))
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
        Ok(status())
    }

    pub fn disable() -> Result<AutoStartState, String> {
        let Some(path) = plist_path() else {
            return Err("cannot find the home directory (HOME)".to_string());
        };
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(status()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(status()),
            Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
        }
    }
}

#[cfg(target_os = "linux")]
mod provider {
    use super::AutoStartState;
    use std::path::{Path, PathBuf};

    fn user_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("systemd")
                .join("user")
        })
    }

    fn unit_path() -> Option<PathBuf> {
        user_dir().map(|d| d.join("swiss.service"))
    }

    fn wants_link() -> Option<PathBuf> {
        user_dir().map(|d| d.join("default.target.wants").join("swiss.service"))
    }

    /// The unit body, pure so the suite can pin it. Enabling is the unit PLUS a symlink under
    /// default.target.wants — written directly instead of shelling out to "systemctl --user
    /// enable" (AGENTS.md: no subprocess where a file write exists). systemd picks both up at
    /// the next session; a daemon-reload would need the subprocess we just refused.
    pub fn unit_body(exe: &str) -> String {
        format!(
            "[Unit]\nDescription=swiss gateway (developer toolbox)\n\n[Service]\nExecStart=\"{exe}\" start --no-open\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
        )
    }

    fn remove_quiet(path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
        }
    }

    pub fn status() -> AutoStartState {
        let unit = unit_path();
        let wants = wants_link();
        let enabled =
            unit.as_ref().is_some_and(|p| p.exists()) && wants.as_ref().is_some_and(|p| p.exists());
        AutoStartState {
            enabled,
            detail: format!(
                "systemd user unit: {}",
                unit.as_ref()
                    .map_or("(no HOME)".to_string(), |p| p.display().to_string())
            ),
            command: super::command_line(),
        }
    }

    pub fn enable() -> Result<AutoStartState, String> {
        let (Some(unit), Some(wants)) = (unit_path(), wants_link()) else {
            return Err("cannot find the home directory (HOME)".to_string());
        };
        if let Some(dir) = unit.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        std::fs::write(&unit, unit_body(&super::exe_path()))
            .map_err(|err| format!("cannot write {}: {err}", unit.display()))?;
        if let Some(dir) = wants.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        remove_quiet(&wants)?;
        std::os::unix::fs::symlink("../swiss.service", &wants)
            .map_err(|err| format!("cannot link {}: {err}", wants.display()))?;
        Ok(status())
    }

    pub fn disable() -> Result<AutoStartState, String> {
        let (Some(unit), Some(wants)) = (unit_path(), wants_link()) else {
            return Err("cannot find the home directory (HOME)".to_string());
        };
        remove_quiet(&wants)?;
        remove_quiet(&unit)?;
        Ok(status())
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod provider {
    use super::AutoStartState;

    pub fn status() -> AutoStartState {
        AutoStartState {
            enabled: false,
            detail: "not supported on this platform".to_string(),
            command: super::command_line(),
        }
    }

    pub fn enable() -> Result<AutoStartState, String> {
        Err("start-at-sign-in is not supported on this platform".to_string())
    }

    pub fn disable() -> Result<AutoStartState, String> {
        Ok(status())
    }
}

/// Read the OS registration. Never fails — an unreadable registration reads as "off", with the
/// provider's detail saying where it looked.
pub fn status() -> AutoStartState {
    provider::status()
}

/// Register (true) or unregister (false) start-at-sign-in for the CURRENT user. The state in
/// the answer is read back from the OS, never echoed from what we meant to write.
pub fn set(enabled: bool) -> Result<AutoStartState, String> {
    if enabled {
        provider::enable()
    } else {
        provider::disable()
    }
}

/// The seam /api/autostart reads through (AppContext::autostart). The gateway mounts
/// [`OsAutoStart`]; a test composition mounts a fake or nothing, so a green suite never touches
/// the machine's login items. The suite once wrote the real Run value on every run and left it
/// removed, which read to the operator as "the switch does nothing".
pub trait AutoStart: Send + Sync {
    fn status(&self) -> AutoStartState;
    fn set(&self, enabled: bool) -> Result<AutoStartState, String>;
}

/// The real registration: this OS's provider, for the current user.
pub struct OsAutoStart;

impl AutoStart for OsAutoStart {
    fn status(&self) -> AutoStartState {
        status()
    }

    fn set(&self, enabled: bool) -> Result<AutoStartState, String> {
        set(enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_is_this_exe_detached_and_quiet() {
        let cmd = command_line();
        assert!(cmd.ends_with("start --no-open"));
        assert!(cmd.starts_with('"'), "the exe path must be quoted: {cmd}");
    }

    #[test]
    fn state_json_shape_is_three_keys() {
        let state = AutoStartState {
            enabled: false,
            detail: "d".into(),
            command: "c".into(),
        };
        let v = state.to_json();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["detail"], "d");
        assert_eq!(v["command"], "c");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_body_carries_the_exe_and_run_at_load() {
        let body = provider::plist_body("/Users/a b/swiss");
        assert!(body.contains("<string>/Users/a b/swiss</string>"));
        assert!(body.contains("<string>start</string>"));
        assert!(body.contains("<string>--no-open</string>"));
        assert!(body.contains("<key>RunAtLoad</key>"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unit_body_carries_the_exe_and_wanted_by() {
        let body = provider::unit_body("/home/ab/swiss");
        assert!(body.contains("ExecStart=\"/home/ab/swiss\" start --no-open"));
        assert!(body.contains("WantedBy=default.target"));
    }
}
