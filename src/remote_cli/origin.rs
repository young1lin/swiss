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

//! Where a `swiss remote` call ran (SPEC §remote.history "Provenance"): the coding agent that
//! started this process, read from the environment it gave its shell, and where that agent
//! keeps the conversation the gateway reads the run's trail from. Paths under the home are
//! sent as `~/...`, so a record never carries the user name.
//!
//! - Claude Code: `CLAUDE_CODE_SESSION_ID`, the log `<CLAUDE_CONFIG_DIR|~/.claude>/projects/*/<id>.jsonl`;
//! - DSH: `DSH_SESSION_ID`, the log folder `<DSH_HOME|~/.dsh>/sessions/*/<id>`;
//! - Codex: `CODEX_*` markers (a session id when one is set), the folder `<CODEX_HOME|~/.codex>/sessions`;
//! - opencode: `OPENCODE` / `OPENCODE_PID`, the database `<OPENCODE_DB|~/.local/share/opencode/opencode.db>`.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The agent a call came from, as the run's `input.origin` carries it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Origin {
    pub agent: String,
    pub version: Option<String>,
    pub session: Option<String>,
    pub cwd: Option<String>,
    pub log: Option<String>,
}

impl Origin {
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("agent".into(), json!(self.agent));
        for (key, value) in [
            ("version", &self.version),
            ("session", &self.session),
            ("cwd", &self.cwd),
            ("log", &self.log),
        ] {
            // The gateway refuses a field past 1 KiB; an odd path is dropped, not the run.
            if let Some(value) = value.as_ref().filter(|value| value.len() <= 1024) {
                out.insert(key.into(), json!(value));
            }
        }
        Value::Object(out)
    }
}

/// The calling agent from this process's environment, `None` for a person at a terminal.
/// `launch_var`, not `std::env::var`: main strikes the agent markers (CLAUDECODE,
/// CLAUDE_CODE_*, DSH_SESSION_ID) before any command runs, so only the launch snapshot has them.
pub fn detect() -> Option<Origin> {
    let env = |name: &str| swiss_core::env::launch_var(name).filter(|v| !v.trim().is_empty());
    let home = swiss_core::paths::home_dir();
    let cwd = std::env::current_dir().ok();
    detect_from(&env, &home, cwd.as_deref())
}

/// [detect] over a given environment, home and working directory.
pub fn detect_from(env: &dyn Fn(&str) -> Option<String>, home: &Path, cwd: Option<&Path>) -> Option<Origin> {
    let mut origin = agent_of(env, home)?;
    origin.cwd = cwd.map(|cwd| tilde(cwd, home));
    Some(origin)
}

fn agent_of(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> Option<Origin> {
    // The agents that hand their shell a session id come first: the innermost agent that
    // set one is the caller even when an outer one's markers were inherited.
    if let Some(session) = env("CLAUDE_CODE_SESSION_ID") {
        let config = env("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        let log = find_child(&config.join("projects"), &format!("{session}.jsonl"));
        return Some(Origin {
            agent: "claude-code".into(),
            version: env("AI_AGENT").and_then(|tag| claude_version(&tag)),
            session: Some(session),
            log: log.map(|log| tilde(&log, home)),
            ..Origin::default()
        });
    }
    if let Some(session) = env("DSH_SESSION_ID") {
        let dsh = env("DSH_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".dsh"));
        let log = find_child(&dsh.join("sessions"), &session);
        return Some(Origin {
            agent: "dsh".into(),
            session: Some(session),
            log: log.map(|log| tilde(&log, home)),
            ..Origin::default()
        });
    }
    let codex_session = env("CODEX_SESSION_ID").or_else(|| env("CODEX_THREAD_ID"));
    let codex_marked = ["CODEX_MANAGED_BY_NPM", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "CODEX_CI"]
        .iter()
        .any(|name| env(name).is_some());
    if codex_session.is_some() || codex_marked {
        let codex = env("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        return Some(Origin {
            agent: "codex".into(),
            // Only an id the rollout file names can narrow the search.
            session: codex_session.filter(|id| uuid_like(id)),
            log: Some(tilde(&codex.join("sessions"), home)),
            ..Origin::default()
        });
    }
    if env("OPENCODE").is_some() || env("OPENCODE_PID").is_some() {
        let data = env("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local").join("share"));
        let db = match env("OPENCODE_DB").map(PathBuf::from) {
            Some(db) if db.is_absolute() => db,
            Some(db) => data.join("opencode").join(db),
            None => data.join("opencode").join("opencode.db"),
        };
        return Some(Origin { agent: "opencode".into(), log: Some(tilde(&db, home)), ..Origin::default() });
    }
    // An older Claude Code marks its shell without a session id: the agent alone, no trail.
    env("CLAUDECODE").map(|_| Origin { agent: "claude-code".into(), ..Origin::default() })
}

/// `claude-code_2-1-322_agent` -> `2.1.322`.
fn claude_version(tag: &str) -> Option<String> {
    let rest = tag.strip_prefix("claude-code_")?;
    let version = rest.split('_').next()?.replace('-', ".");
    (!version.is_empty()).then_some(version)
}

fn uuid_like(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `<dir>/*/<name>`: the first sub-directory of `dir` holding `name`.
fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(name))
        .find(|path| path.exists())
}

/// A path as a record carries it: `~/...` under the home, forward slashes throughout.
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.to_string_lossy().replace('\\', "/")),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

/// The actions that change the far side: an agent says why before it runs one.
pub fn needs_why(action: &str) -> bool {
    matches!(action, "remote.exec" | "remote.write" | "remote.sync")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn each_agent_is_told_by_its_own_environment() {
        let home = std::env::temp_dir().join(format!("swiss-origin-{}", std::process::id()));
        let session = "00000000-1111-4222-8333-444444444444";
        let project = home.join(".claude").join("projects").join("C--src-app");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{session}.jsonl")), "").unwrap();
        let cwd = home.join("src").join("app");

        let claude = detect_from(
            &env_of(&[("CLAUDE_CODE_SESSION_ID", "00000000-1111-4222-8333-444444444444"), ("AI_AGENT", "claude-code_2-1-322_agent"), ("OPENCODE", "1")]),
            &home,
            Some(&cwd),
        )
        .unwrap();
        assert_eq!(claude.agent, "claude-code", "the agent with a session id wins over inherited markers");
        assert_eq!(claude.version.as_deref(), Some("2.1.322"));
        assert_eq!(claude.log.as_deref(), Some(format!("~/.claude/projects/C--src-app/{session}.jsonl").as_str()));
        assert_eq!(claude.cwd.as_deref(), Some("~/src/app"));

        let codex = detect_from(&env_of(&[("CODEX_MANAGED_BY_NPM", "1")]), &home, None).unwrap();
        assert_eq!((codex.agent.as_str(), codex.session.as_deref()), ("codex", None));
        assert_eq!(codex.log.as_deref(), Some("~/.codex/sessions"));

        let opencode = detect_from(&env_of(&[("OPENCODE", "1"), ("OPENCODE_PID", "42")]), &home, None).unwrap();
        assert_eq!(opencode.log.as_deref(), Some("~/.local/share/opencode/opencode.db"));

        let dsh = detect_from(&env_of(&[("DSH_SESSION_ID", "session-1")]), &home, None).unwrap();
        assert_eq!((dsh.agent.as_str(), dsh.log.as_deref()), ("dsh", None), "no log folder found, none sent");

        assert!(detect_from(&env_of(&[("TERM", "xterm")]), &home, Some(&cwd)).is_none(), "a person at a terminal");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_record_never_carries_the_home_path() {
        let home = Path::new("/home/someone");
        assert_eq!(tilde(Path::new("/home/someone/.dsh/sessions/x"), home), "~/.dsh/sessions/x");
        assert_eq!(tilde(Path::new("/srv/data"), home), "/srv/data");
        assert!(needs_why("remote.exec") && needs_why("remote.sync") && !needs_why("remote.cat"));
    }
}
