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
///
/// Where the agent keeps its state (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `DSH_HOME`, the opencode
/// database) is also read from the agent's own processes up the tree: the Codex app runs on the
/// CODEX_HOME it was launched with - another account's, on another drive - and hands its tool
/// shell none (2026-11-11).
pub fn detect() -> Option<Origin> {
    let env = |name: &str| swiss_core::env::launch_var(name).filter(|v| !v.trim().is_empty());
    let up_the_tree = |name: &str| -> Vec<String> {
        swiss_core::platform::ancestor_env(&[name])
            .into_iter()
            .map(|(_, value)| value)
            .filter(|value| !value.trim().is_empty())
            .collect()
    };
    let home = swiss_core::paths::home_dir();
    let cwd = std::env::current_dir().ok();
    detect_from(&env, &up_the_tree, &home, cwd.as_deref())
}

/// [detect] over a given environment, the ancestors' values of a variable (nearest first), a
/// home and a working directory.
pub fn detect_from(
    env: &dyn Fn(&str) -> Option<String>,
    up_the_tree: &dyn Fn(&str) -> Vec<String>,
    home: &Path,
    cwd: Option<&Path>,
) -> Option<Origin> {
    let mut origin = agent_of(env, up_the_tree, home)?;
    origin.cwd = cwd.map(|cwd| tilde(cwd, home));
    Some(origin)
}

fn agent_of(env: &dyn Fn(&str) -> Option<String>, up_the_tree: &dyn Fn(&str) -> Vec<String>, home: &Path) -> Option<Origin> {
    // A place variable as this shell has it, then as each agent process above it has it.
    let named = |name: &str| -> Vec<String> {
        let mut values: Vec<String> = env(name).into_iter().collect();
        for value in up_the_tree(name) {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        values
    };
    // The agents that hand their shell a session id come first: the innermost agent that
    // set one is the caller even when an outer one's markers were inherited.
    if let Some(session) = env("CLAUDE_CODE_SESSION_ID") {
        let log = agent_homes(named("CLAUDE_CONFIG_DIR"), home, ".claude")
            .iter()
            .find_map(|config| find_child(&config.join("projects"), &format!("{session}.jsonl")));
        return Some(Origin {
            agent: "claude-code".into(),
            version: env("AI_AGENT").and_then(|tag| claude_version(&tag)),
            session: Some(session),
            log: log.map(|log| tilde(&log, home)),
            ..Origin::default()
        });
    }
    if let Some(session) = env("DSH_SESSION_ID") {
        let log = agent_homes(named("DSH_HOME"), home, ".dsh")
            .iter()
            .find_map(|dsh| find_child(&dsh.join("sessions"), &session));
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
        // Only an id the rollout file names can narrow the search.
        let session = codex_session.filter(|id| uuid_like(id));
        // The likeliest first: the named homes, then the default - the fallback when no
        // session id can say which one holds the conversation.
        let homes = agent_homes(named("CODEX_HOME"), home, ".codex");
        let sessions = session
            .as_deref()
            .and_then(|id| homes.iter().map(|codex| codex.join("sessions")).find(|dir| holds_rollout(dir, id)))
            .unwrap_or_else(|| homes[0].join("sessions"));
        return Some(Origin {
            agent: "codex".into(),
            session,
            log: Some(tilde(&sessions, home)),
            ..Origin::default()
        });
    }
    if env("OPENCODE").is_some() || env("OPENCODE_PID").is_some() {
        let first = |name: &str| named(name).into_iter().next().map(PathBuf::from);
        let data = first("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
        let db = match first("OPENCODE_DB") {
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

/// Every folder an agent may keep its state in, the likeliest first: the ones its variable
/// names (this shell's value, then each ancestor's - wherever it is, another drive included),
/// the default, then each folder in the home named like the default (`.codex-work` beside
/// `.codex`, `.claude-account-x` beside `.claude`). One machine, several accounts, each app
/// launched with its own `*_HOME` - and an app that does not hand that variable on to its tool
/// shell, so the shell never learns which one (2026-11-11). The caller picks the folder that
/// holds the session.
fn agent_homes(named: Vec<String>, home: &Path, default: &str) -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    for path in named.into_iter().map(PathBuf::from).chain([home.join(default)]) {
        if !homes.contains(&path) {
            homes.push(path);
        }
    }
    let bare = default.trim_start_matches('.');
    let mut siblings: Vec<PathBuf> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            name.starts_with(default) || name.starts_with(bare)
        })
        .map(|entry| entry.path())
        .collect();
    siblings.sort();
    for sibling in siblings {
        if !homes.contains(&sibling) {
            homes.push(sibling);
        }
    }
    homes
}

/// Whether a Codex `sessions` folder holds the conversation `id`: its rollout file lies under
/// the YYYY/MM/DD of the day it began, named `rollout-<time>-<id>.jsonl`. The newest two
/// months, newest day first - a conversation kept open for weeks is still the one asked.
fn holds_rollout(sessions: &Path, id: &str) -> bool {
    let newest = |dir: &Path, n: usize| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> =
            std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        dirs.into_iter().rev().take(n).collect()
    };
    let suffix = format!("{id}.jsonl");
    let mut months: Vec<PathBuf> = newest(sessions, 2).iter().flat_map(|year| newest(year, 2)).collect();
    months.sort();
    months.into_iter().rev().take(2).flat_map(|month| newest(&month, 31)).any(|day| {
        std::fs::read_dir(day)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(&suffix))
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

    fn nothing_above(_: &str) -> Vec<String> {
        Vec::new()
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
            &nothing_above,
            &home,
            Some(&cwd),
        )
        .unwrap();
        assert_eq!(claude.agent, "claude-code", "the agent with a session id wins over inherited markers");
        assert_eq!(claude.version.as_deref(), Some("2.1.322"));
        assert_eq!(claude.log.as_deref(), Some(format!("~/.claude/projects/C--src-app/{session}.jsonl").as_str()));
        assert_eq!(claude.cwd.as_deref(), Some("~/src/app"));

        let codex = detect_from(&env_of(&[("CODEX_MANAGED_BY_NPM", "1")]), &nothing_above, &home, None).unwrap();
        assert_eq!((codex.agent.as_str(), codex.session.as_deref()), ("codex", None));
        assert_eq!(codex.log.as_deref(), Some("~/.codex/sessions"));

        let opencode = detect_from(&env_of(&[("OPENCODE", "1"), ("OPENCODE_PID", "42")]), &nothing_above, &home, None).unwrap();
        assert_eq!(opencode.log.as_deref(), Some("~/.local/share/opencode/opencode.db"));

        let dsh = detect_from(&env_of(&[("DSH_SESSION_ID", "session-1")]), &nothing_above, &home, None).unwrap();
        assert_eq!((dsh.agent.as_str(), dsh.log.as_deref()), ("dsh", None), "no log folder found, none sent");

        assert!(detect_from(&env_of(&[("TERM", "xterm")]), &nothing_above, &home, Some(&cwd)).is_none(), "a person at a terminal");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_codex_home_only_the_app_knows_is_found_by_its_session() {
        // The 2026-11-11 report: the Codex app was launched with CODEX_HOME set to another
        // account's folder, its tool shell got no CODEX_HOME, and the origin named the default
        // ~/.codex - so the trail read the wrong account's rollouts and found nothing.
        let root = std::env::temp_dir().join(format!("swiss-origin-homes-{}", std::process::id()));
        let home = root.join("home");
        let elsewhere = root.join("d-drive").join("codex-work");
        let session = "00000000-0000-7111-8222-333333333333";
        let rollout = |codex: &Path, id: &str| {
            let day = codex.join("sessions").join("2026").join("11").join("11");
            std::fs::create_dir_all(&day).unwrap();
            std::fs::write(day.join(format!("rollout-2026-11-11T10-00-00-{id}.jsonl")), "").unwrap();
        };
        // Every candidate holds a rollout; only one holds THIS session.
        rollout(&home.join(".codex"), "00000000-0000-0000-0000-000000000001");
        rollout(&elsewhere, session);
        let marked: &'static [(&'static str, &'static str)] =
            &[("CODEX_THREAD_ID", "00000000-0000-7111-8222-333333333333"), ("CODEX_SANDBOX", "1")];

        // The app's own process holds the folder, outside the home (another drive).
        let app_has = |name: &str| -> Vec<String> {
            if name == "CODEX_HOME" {
                vec![elsewhere.to_string_lossy().into_owned()]
            } else {
                Vec::new()
            }
        };
        let codex = detect_from(&env_of(marked), &app_has, &home, None).unwrap();
        assert_eq!(codex.session.as_deref(), Some(session));
        let expected = elsewhere.join("sessions").to_string_lossy().replace('\\', "/");
        assert_eq!(codex.log.as_deref(), Some(expected.as_str()), "an absolute path outside the home");

        // Nobody names it, but it sits beside the default: the session file picks it.
        rollout(&home.join(".codex-work"), session);
        let codex = detect_from(&env_of(marked), &nothing_above, &home, None).unwrap();
        assert_eq!(codex.log.as_deref(), Some("~/.codex-work/sessions"));

        // A named folder that does not hold the session gives way to the one that does.
        let stale = |name: &str| -> Vec<String> {
            if name == "CODEX_HOME" {
                vec![home.join(".codex").to_string_lossy().into_owned()]
            } else {
                Vec::new()
            }
        };
        let codex = detect_from(&env_of(marked), &stale, &home, None).unwrap();
        assert_eq!(codex.log.as_deref(), Some("~/.codex-work/sessions"));

        // No session id to check against: the nearest named folder is the best guess.
        let codex = detect_from(&env_of(&[("CODEX_SANDBOX", "1")]), &app_has, &home, None).unwrap();
        assert_eq!(codex.log.as_deref(), Some(expected.as_str()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_claude_config_only_an_ancestor_names_is_searched() {
        let root = std::env::temp_dir().join(format!("swiss-origin-claude-{}", std::process::id()));
        let home = root.join("home");
        let config = root.join("d-drive").join("claude-work");
        let session = "00000000-1111-4222-8333-444444444444";
        let project = config.join("projects").join("C--src-app");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{session}.jsonl")), "").unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let above = |name: &str| -> Vec<String> {
            if name == "CLAUDE_CONFIG_DIR" {
                vec![config.to_string_lossy().into_owned()]
            } else {
                Vec::new()
            }
        };
        let claude =
            detect_from(&env_of(&[("CLAUDE_CODE_SESSION_ID", "00000000-1111-4222-8333-444444444444")]), &above, &home, None)
                .unwrap();
        let expected = project.join(format!("{session}.jsonl")).to_string_lossy().replace('\\', "/");
        assert_eq!(claude.log.as_deref(), Some(expected.as_str()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_record_never_carries_the_home_path() {
        let home = Path::new("/home/someone");
        assert_eq!(tilde(Path::new("/home/someone/.dsh/sessions/x"), home), "~/.dsh/sessions/x");
        assert_eq!(tilde(Path::new("/srv/data"), home), "/srv/data");
        assert!(needs_why("remote.exec") && needs_why("remote.sync") && !needs_why("remote.cat"));
    }
}
