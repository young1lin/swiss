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

//! The plugin's config row -> the limits the session machine runs by (docs/14 §6).
//!
//! Every value here is a policy the user can see and change; the two that are NOT config
//! live next door as constants (the 64 KB catch-up buffer and the 8 MB recording cap),
//! because docs/14 §7 puts them in the memory budget rather than in the user's hands.
//!
//! Parsing is strict and the errors are user-facing: the plugin's `validate_config`
//! hands them back behind a 400. A mistyped `maxSessions` that silently fell back to the
//! default would be a limit the user believes in and does not have.

use std::time::Duration;

use serde_json::Value;

/// docs/14 §6.6. Four at once is a developer's desk, not a bastion host.
pub const DEFAULT_MAX_SESSIONS: usize = 4;
pub const DEFAULT_MAX_SESSIONS_PER_TARGET: usize = 2;
pub const DEFAULT_IDLE_TIMEOUT_MINUTES: u64 = 30;
/// docs/14 §6.7: a closed laptop lid must not kill a running compile.
pub const DEFAULT_GRACE_SECONDS: u64 = 60;
/// docs/14 §6.8: how long a full send queue is tolerated before the session is closed.
pub const DEFAULT_STALL_SECONDS: u64 = 30;

/// What the terminal plugin was configured with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalConfig {
    pub local: LocalConfig,
    /// Empty = every target the shell provider lists. Otherwise an exact allowlist of
    /// connection ids (docs/14 §6.2).
    pub allowed_targets: Vec<String>,
    pub max_sessions: usize,
    pub max_sessions_per_target: usize,
    pub idle_timeout: Duration,
    pub grace: Duration,
    pub stall: Duration,
    /// asciicast recording, on by default. Off is honest rather than silent: the session
    /// list and the open response then report `recording: null`.
    pub recording: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalConfig {
    /// docs/14 §6.1: OFF by default. The gateway already runs as the user, so a local
    /// shell gives a local attacker nothing new — but it turns a loopback HTTP port into
    /// arbitrary code execution, and that upgrade is worth one deliberate click.
    pub enabled: bool,
    /// Override for the program to run. None = the platform's own default shell.
    pub shell: Option<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig {
            local: LocalConfig::default(),
            allowed_targets: Vec::new(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            max_sessions_per_target: DEFAULT_MAX_SESSIONS_PER_TARGET,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_MINUTES * 60),
            grace: Duration::from_secs(DEFAULT_GRACE_SECONDS),
            stall: Duration::from_secs(DEFAULT_STALL_SECONDS),
            recording: true,
        }
    }
}

impl TerminalConfig {
    /// One config row -> a config. Absent keys take the default; a present key of the
    /// wrong shape is an error naming the key, never a silent fallback.
    pub fn parse(config: &Value) -> Result<Self, String> {
        let mut out = TerminalConfig::default();

        match config.get("local") {
            None | Some(Value::Null) => {}
            Some(Value::Object(local)) => {
                out.local.enabled = flag(local.get("enabled"), "local.enabled", false)?;
                out.local.shell = match local.get("shell") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if s.trim().is_empty() => None,
                    Some(Value::String(s)) => Some(s.trim().to_string()),
                    Some(_) => return Err("local.shell: must be a string".into()),
                };
            }
            Some(_) => return Err("local: must be an object".into()),
        }

        out.allowed_targets = match config.get("allowedTargets") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => {
                let mut ids = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let id = item
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            format!("allowedTargets[{i}]: must be a non-empty connection id")
                        })?;
                    ids.push(id.to_string());
                }
                ids
            }
            Some(_) => return Err("allowedTargets: must be an array of connection ids".into()),
        };

        out.max_sessions = count(config.get("maxSessions"), "maxSessions", out.max_sessions)?;
        out.max_sessions_per_target = count(
            config.get("maxSessionsPerTarget"),
            "maxSessionsPerTarget",
            out.max_sessions_per_target,
        )?;
        out.idle_timeout = Duration::from_secs(
            60 * span(
                config.get("idleTimeoutMinutes"),
                "idleTimeoutMinutes",
                DEFAULT_IDLE_TIMEOUT_MINUTES,
            )?,
        );
        out.grace = Duration::from_secs(span(
            config.get("graceSeconds"),
            "graceSeconds",
            DEFAULT_GRACE_SECONDS,
        )?);
        out.stall = Duration::from_secs(span(
            config.get("stallSeconds"),
            "stallSeconds",
            DEFAULT_STALL_SECONDS,
        )?);
        out.recording = flag(config.get("recording"), "recording", true)?;

        if out.max_sessions_per_target > out.max_sessions {
            return Err(format!(
                "maxSessionsPerTarget ({}) cannot exceed maxSessions ({})",
                out.max_sessions_per_target, out.max_sessions
            ));
        }
        Ok(out)
    }
}

fn flag(value: Option<&Value>, key: &str, default: bool) -> Result<bool, String> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key}: must be true or false")),
    }
}

/// A session count: a positive integer. Zero is refused rather than read as "none
/// allowed" — a plugin that can open no sessions at all is a disabled plugin, and saying
/// so is what `local.enabled` and the plugin switch are for, not a limit of 0 that nobody
/// would think to look at.
fn count(value: Option<&Value>, key: &str, default: usize) -> Result<usize, String> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if (1..=64).contains(&v) => Ok(v as usize),
            _ => Err(format!("{key}: must be a whole number between 1 and 64")),
        },
        Some(_) => Err(format!("{key}: must be a whole number between 1 and 64")),
    }
}

/// A timeout in whole units. Zero is allowed and means "no clock" — a user who wants a
/// terminal that never times out should not have to find the largest number that fits.
/// The upper bound keeps a typo'd year out of a timer nobody can see.
fn span(value: Option<&Value>, key: &str, default: u64) -> Result<u64, String> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v <= 86_400 => Ok(v),
            _ => Err(format!("{key}: must be a whole number of at most 86400")),
        },
        Some(_) => Err(format!("{key}: must be a whole number of at most 86400")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_config_is_the_documented_defaults() {
        let cfg = TerminalConfig::parse(&json!({})).expect("parses");
        assert_eq!(cfg, TerminalConfig::default());
        // The one default that is a security decision rather than a number (docs/14 §6.1).
        assert!(!cfg.local.enabled, "the local shell must default to OFF");
        assert_eq!(cfg.max_sessions, 4);
        assert_eq!(cfg.max_sessions_per_target, 2);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(1800));
        assert_eq!(cfg.grace, Duration::from_secs(60));
        assert_eq!(cfg.stall, Duration::from_secs(30));
        assert!(cfg.recording);
    }

    #[test]
    fn every_documented_key_is_read() {
        let cfg = TerminalConfig::parse(&json!({
            "local": { "enabled": true, "shell": "pwsh.exe" },
            "allowedTargets": ["box-one", " box-two "],
            "maxSessions": 8,
            "maxSessionsPerTarget": 3,
            "idleTimeoutMinutes": 5,
            "graceSeconds": 15,
            "stallSeconds": 7,
            "recording": false,
        }))
        .expect("parses");
        assert!(cfg.local.enabled);
        assert_eq!(cfg.local.shell.as_deref(), Some("pwsh.exe"));
        assert_eq!(cfg.allowed_targets, vec!["box-one", "box-two"]);
        assert_eq!(cfg.max_sessions, 8);
        assert_eq!(cfg.max_sessions_per_target, 3);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(300));
        assert_eq!(cfg.grace, Duration::from_secs(15));
        assert_eq!(cfg.stall, Duration::from_secs(7));
        assert!(!cfg.recording);
    }

    #[test]
    fn a_mistyped_limit_is_refused_not_defaulted() {
        // The whole point: a limit the user believes in must exist. Silently falling back
        // to 4 here would be worse than the 400.
        for (row, wanted) in [
            (json!({ "maxSessions": 0 }), "maxSessions"),
            (json!({ "maxSessions": "four" }), "maxSessions"),
            (json!({ "local": { "enabled": "yes" } }), "local.enabled"),
            (json!({ "local": true }), "local"),
            (json!({ "allowedTargets": "box-one" }), "allowedTargets"),
            (json!({ "allowedTargets": [""] }), "allowedTargets[0]"),
            (json!({ "idleTimeoutMinutes": -1 }), "idleTimeoutMinutes"),
            (json!({ "graceSeconds": 999999 }), "graceSeconds"),
            (json!({ "recording": 1 }), "recording"),
        ] {
            let err = TerminalConfig::parse(&row).expect_err("refused");
            assert!(err.contains(wanted), "{row} -> {err}");
        }
    }

    #[test]
    fn a_per_target_cap_above_the_total_cap_is_refused() {
        // Not pedantry: the pair reads as a promise ("at most 2 of your 4"), and inverted
        // it means the per-target cap can never bind. Say so at config time.
        let err = TerminalConfig::parse(&json!({ "maxSessions": 2, "maxSessionsPerTarget": 4 }))
            .expect_err("refused");
        assert!(err.contains("cannot exceed"), "{err}");
    }

    #[test]
    fn an_idle_timeout_of_zero_means_no_timeout_and_is_allowed() {
        let cfg = TerminalConfig::parse(&json!({ "idleTimeoutMinutes": 0 })).expect("parses");
        assert_eq!(cfg.idle_timeout, Duration::ZERO);
    }
}
