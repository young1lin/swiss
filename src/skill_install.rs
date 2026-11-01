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

//! Copy the embedded AI skills into the user-level directories an AI tool actually scans.
//! The general `swiss` skill covers safe setup; `swiss-remote` holds the remote CLI
//! contract; `swiss-memory` holds the memory tool and CLI contract (SPEC §memory).
//! Each goes under ~/.agents/skills, ~/.claude/skills, and ~/.cursor/skills, and under
//! `$CLAUDE_CONFIG_DIR/skills` when that is set, since Claude Code then reads its user skills
//! there instead. A single-file release cannot read any skill from beside the binary.
//!
//! Idempotent: a re-run replaces each copy, so upgrading and re-installing drops stale files.
//! Returns all written targets for the CLI to print.

use std::path::{Path, PathBuf};

/// All shipped skills are embedded at compile time; Node is never needed to build the exe.
const SKILL_MD: &str = include_str!("skill_assets/SKILL.md");
const REMOTE_SKILL_MD: &str = include_str!("skill_assets/remote/SKILL.md");
const MEMORY_SKILL_MD: &str = include_str!("skill_assets/memory/SKILL.md");

const SKILL_NAME: &str = "swiss";
const REMOTE_SKILL_NAME: &str = "swiss-remote";
const MEMORY_SKILL_NAME: &str = "swiss-memory";
const SHIPPED_SKILLS: [(&str, &str); 3] = [
    (SKILL_NAME, SKILL_MD),
    (REMOTE_SKILL_NAME, REMOTE_SKILL_MD),
    (MEMORY_SKILL_NAME, MEMORY_SKILL_MD),
];

#[derive(serde::Serialize)]
pub struct SkillLocation {
    pub root: String,
    pub path: String,
    pub installed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(serde::Serialize)]
pub struct SkillStatus {
    pub name: String,
    pub locations: Vec<SkillLocation>,
}

/// Inspect only the shipped names; never enumerate or read unrelated user skills.
pub fn status() -> Result<Vec<SkillStatus>, String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "cannot find the user home directory".to_string())?;
    Ok(status_in(&PathBuf::from(home), claude_config().as_deref()))
}

/// `$CLAUDE_CONFIG_DIR`, when it names something. Claude Code reads its user skills and its
/// settings under it, so a machine that sets it is not served by `~/.claude` at all.
fn claude_config() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|c| !c.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

/// Where one of the three user skill roots actually lives. `.claude` is the only one that can
/// move: with `CLAUDE_CONFIG_DIR` set, that IS the directory Claude Code reads, and the batch
/// install (`install_skills_in`) has always honored it — the panel's per-skill path and status
/// did not, so a machine with the variable set saw the row say "installed" for a directory the
/// client never opens, and the install wrote there too (2026-11-01).
fn root_dir(home: &Path, root: &str, claude_config: Option<&Path>) -> PathBuf {
    match (root, claude_config) {
        (".claude", Some(config)) => config.join("skills"),
        _ => home.join(root).join("skills"),
    }
}

fn status_in(home: &Path, claude_config: Option<&Path>) -> Vec<SkillStatus> {
    SHIPPED_SKILLS.iter().map(|(name, _)| SkillStatus {
        name: (*name).into(),
        locations: [".claude", ".codex", ".agents"].iter().map(|root| {
            let path = root_dir(home, root, claude_config).join(name).join("SKILL.md");
            let (installed, error) = match std::fs::metadata(&path) {
                Ok(metadata) => (Some(metadata.is_file()), None),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Some(false), None),
                Err(err) => (None, Some(err.to_string())),
            };
            SkillLocation { root: (*root).into(), path: path.to_string_lossy().into_owned(), installed, error }
        }).collect(),
    }).collect()
}

pub fn valid_target(name: &str, root: &str) -> bool {
    SHIPPED_SKILLS.iter().any(|(shipped, _)| *shipped == name)
        && matches!(root, ".claude" | ".codex" | ".agents")
}

pub fn install_selected(name: &str, root: &str) -> Result<(), String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "cannot find the user home directory".to_string())?;
    install_selected_in(&PathBuf::from(home), name, root, claude_config().as_deref())
}

fn install_selected_in(home: &Path, name: &str, root: &str, claude_config: Option<&Path>) -> Result<(), String> {
    if !valid_target(name, root) { return Err("unknown skill or skill directory".into()); }
    let Some((_, body)) = SHIPPED_SKILLS.iter().find(|(shipped, _)| *shipped == name) else {
        return Err("unknown skill".into());
    };
    let directory = root_dir(home, root, claude_config).join(name);
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    // Replace only SKILL.md. User additions and other skills remain untouched, and a failed
    // staged write or rename leaves the previous installed document intact.
    let staged = directory.join(format!(".SKILL-{}.tmp", swiss_core::util::random_hex(12)));
    let result = std::fs::write(&staged, body).and_then(|_| retry_busy(|| {
        std::fs::rename(&staged, directory.join("SKILL.md"))
    })).map_err(|e| format!("could not install {name} in {root}: {e}"));
    if result.is_err() { let _ = std::fs::remove_file(&staged); }
    result
}

/// Virtual source directory for the general setup skill, kept for callers that report it.
/// The remote skill lives at the sibling `<embedded>/.agents/skills/swiss-remote` path.
pub fn skill_dir() -> PathBuf {
    PathBuf::from(format!("<embedded>/.agents/skills/{SKILL_NAME}"))
}

/// How often, and how far apart, a denied replace is tried again (about a second in all).
const BUSY_ATTEMPTS: u32 = 40;
const BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(25);

/// Run a filesystem step, trying again while it is denied. Windows refuses to rename or
/// remove a directory while another process holds a file in it open without delete
/// sharing, and a skill watcher (an AI client scanning its skills directory) opens a
/// freshly written SKILL.md for a few milliseconds: measured on ~/.agents/skills, the
/// staged rename was denied once and went through 25 ms later.
fn retry_busy<T>(mut step: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match step() {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempt < BUSY_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(BUSY_BACKOFF);
            }
            done => return done,
        }
    }
}

fn write_tree(src_files: &[(&str, &str)], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("could not create {}: {e}", dest.display()))?;
    for (name, body) in src_files {
        std::fs::write(dest.join(name), body)
            .map_err(|e| format!("could not write {}: {e}", dest.join(name).display()))?;
    }
    Ok(())
}

pub fn install_skill() -> Result<Vec<String>, String> {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Err("cannot find the user home directory".into());
    };
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|c| !c.to_string_lossy().trim().is_empty())
        .map(PathBuf::from);
    install_skills_in(&PathBuf::from(home), config.as_deref())
}

fn install_skills_in(home: &Path, claude_config: Option<&Path>) -> Result<Vec<String>, String> {
    let mut roots = vec![
        home.join(".agents").join("skills"),
        home.join(".claude").join("skills"),
        home.join(".cursor").join("skills"),
    ];
    if let Some(config) = claude_config {
        let skills = config.join("skills");
        if !roots.contains(&skills) {
            roots.push(skills);
        }
    }
    let mut written: Vec<String> = Vec::new();
    for root in &roots {
        for (name, body) in SHIPPED_SKILLS {
            let target = root.join(name);
            std::fs::create_dir_all(root)
                .map_err(|e| format!("could not create {}: {e}", root.display()))?;
            // Stage beside the target before replacing it, so a failed write leaves the old
            // skill intact. This also replaces the old combined swiss skill on upgrade.
            let staged = target.with_extension(format!("staged-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&staged);
            if let Err(err) = write_tree(&[("SKILL.md", body)], &staged).and_then(|_| {
                retry_busy(|| std::fs::remove_dir_all(&target))
                    .or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| e.to_string())
                    .and_then(|_| retry_busy(|| std::fs::rename(&staged, &target)).map_err(|e| e.to_string()))
            }) {
                let _ = std::fs::remove_dir_all(&staged);
                return Err(format!(
                    "could not install skill to {}: {err}",
                    target.display()
                ));
            }
            written.push(target.to_string_lossy().into_owned());
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_checks_each_shipped_skill_in_the_three_user_roots_without_writing() {
        let home = TempHome::new();
        let installed = home.0.join(".agents/skills/swiss/SKILL.md");
        std::fs::create_dir_all(installed.parent().expect("skill parent")).expect("mkdir");
        std::fs::write(&installed, "a locally installed skill").expect("write fixture");
        std::fs::create_dir_all(home.0.join(".codex/skills/swiss-memory")).expect("mkdir");
        let rows = status_in(&home.0, None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "swiss");
        assert_eq!(rows[0].locations.iter().map(|l| l.root.as_str()).collect::<Vec<_>>(), [".claude", ".codex", ".agents"]);
        assert_eq!(rows[0].locations.iter().map(|l| l.installed).collect::<Vec<_>>(), [Some(false), Some(false), Some(true)]);
        assert!(rows.iter().skip(1).flat_map(|s| &s.locations).all(|l| l.installed == Some(false)));
        assert!(!home.0.join(".claude").exists(), "checking never creates a directory");
        assert_eq!(std::fs::read_to_string(installed).expect("read fixture"), "a locally installed skill");
    }

    struct TempHome(PathBuf);

    #[test]
    fn a_moved_claude_config_dir_is_the_one_both_status_and_install_use() {
        // Claude Code reads `$CLAUDE_CONFIG_DIR` and nothing under `~/.claude` when it is set.
        // Status and the per-skill install have to follow it, or the panel reports "installed"
        // for a directory the client never opens (2026-11-01).
        let home = TempHome::new();
        let config = TempHome::new();
        let rows = status_in(&home.0, Some(&config.0));
        assert_eq!(rows[0].locations[0].path, config.0.join("skills").join("swiss").join("SKILL.md").to_string_lossy());
        install_selected_in(&home.0, "swiss", ".claude", Some(&config.0)).expect("install");
        assert!(config.0.join("skills/swiss/SKILL.md").is_file(), "the client's directory got the skill");
        assert!(!home.0.join(".claude").exists(), "and the one it does not read stayed untouched");
        let after = status_in(&home.0, Some(&config.0));
        assert_eq!(after[0].locations[0].installed, Some(true), "status follows the same directory");
    }

    #[test]
    fn selected_install_only_writes_one_skill_in_one_root_and_preserves_extra_files() {
        let home = TempHome::new();
        let folder = home.0.join(".codex/skills/swiss-memory");
        std::fs::create_dir_all(&folder).expect("mkdir");
        std::fs::write(folder.join("notes.txt"), "keep me").expect("extra file");
        install_selected_in(&home.0, "swiss-memory", ".codex", None).expect("install");
        assert_eq!(std::fs::read_to_string(folder.join("SKILL.md")).expect("skill"), MEMORY_SKILL_MD);
        assert_eq!(std::fs::read_to_string(folder.join("notes.txt")).expect("extra"), "keep me");
        assert!(!home.0.join(".agents").exists());
        assert!(!home.0.join(".claude").exists());
        assert!(!home.0.join(".codex/skills/swiss").exists());
        std::fs::write(folder.join("SKILL.md"), "old content").expect("old");
        install_selected_in(&home.0, "swiss-memory", ".codex", None).expect("update");
        assert_eq!(std::fs::read_to_string(folder.join("SKILL.md")).expect("updated"), MEMORY_SKILL_MD);
        for (name, root) in [("../other", ".codex"), ("swiss", "../other"), ("swiss", ".cursor")] {
            assert!(install_selected_in(&home.0, name, root, None).is_err());
        }
    }

    impl TempHome {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "swiss-skill-install-{}-{}",
                std::process::id(),
                swiss_core::util::random_hex(8)
            )))
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_denied_step_is_tried_again_and_another_error_is_not() {
        let mut calls = 0;
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let out = retry_busy(|| {
            calls += 1;
            if calls < 3 { Err(denied()) } else { Ok(calls) }
        });
        assert_eq!(out.expect("cleared"), 3);
        let mut calls = 0;
        let out: std::io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!((out.unwrap_err().kind(), calls), (std::io::ErrorKind::NotFound, 1));
        let mut calls = 0;
        let out: std::io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(denied())
        });
        assert_eq!((out.unwrap_err().kind(), calls), (std::io::ErrorKind::PermissionDenied, BUSY_ATTEMPTS));
    }

    /// The case met on ~/.agents/skills: a reader holds the staged SKILL.md open without
    /// delete sharing, so the directory's rename is denied until it lets go.
    #[cfg(windows)]
    #[test]
    fn a_staged_skill_held_open_for_a_moment_still_lands() {
        use std::os::windows::fs::OpenOptionsExt;
        let home = TempHome::new();
        let staged = home.0.join("swiss.staged");
        let target = home.0.join("swiss");
        std::fs::create_dir_all(&staged).expect("staged");
        std::fs::write(staged.join("SKILL.md"), SKILL_MD).expect("write");
        // FILE_SHARE_READ only: no delete sharing, as a scanner opens it.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0x1).open(staged.join("SKILL.md")).expect("hold");
        let denied = std::fs::rename(&staged, &target).expect_err("a held file blocks the rename");
        assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
        let reader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(held);
        });
        retry_busy(|| std::fs::rename(&staged, &target)).expect("lands once the reader lets go");
        reader.join().expect("reader");
        assert_eq!(std::fs::read_to_string(target.join("SKILL.md")).expect("installed"), SKILL_MD);
    }

    #[test]
    fn the_shipped_skills_have_distinct_names_and_bounded_roles() {
        assert!(SKILL_MD.starts_with("---\nname: swiss\n"));
        assert!(REMOTE_SKILL_MD.starts_with("---\nname: swiss-remote\n"));
        for body in [SKILL_MD, REMOTE_SKILL_MD] {
            assert!(body.contains("disable-model-invocation: true"));
        }
        assert!(SKILL_MD.contains("SHA256SUMS"));
        assert!(!SKILL_MD.contains("swiss remote exec"));
        assert!(REMOTE_SKILL_MD.contains("swiss remote exec"));
        assert!(!REMOTE_SKILL_MD.contains("SHA256SUMS"));
    }

    #[test]
    fn the_memory_skill_is_the_one_for_reading_and_writing_and_names_every_tool() {
        assert!(MEMORY_SKILL_MD.starts_with("---\nname: swiss-memory\n"));
        for tool in swiss_plugins::memory_mcp::TOOLS {
            assert!(
                MEMORY_SKILL_MD.contains(&format!("| `{tool}` |")),
                "the skill's tool table misses {tool}"
            );
        }
        for command in ["swiss memory ask", "swiss memory docs search", "swiss memory code find", "swiss memory save"] {
            assert!(MEMORY_SKILL_MD.contains(command), "the skill misses {command}");
        }
    }

    /// The installed skill carries what the save and the brief now say (SPEC §memory.capture,
    /// §memory.brief): a preference in the user's dated words, its `user_words` hint, the
    /// contradiction hint and the rejected precedent.
    #[test]
    fn the_memory_skill_asks_for_the_user_s_words_and_reads_the_history() {
        for wanted in [
            "quoted letter for letter and dated",
            "- stated by the user, 2026-10-21",
            "`user_words`",
            "**Contradictions:**",
            "**Rejected before:**",
        ] {
            assert!(MEMORY_SKILL_MD.contains(wanted), "the skill misses {wanted:?}");
        }
        assert!(!MEMORY_SKILL_MD.contains("The content says \"stated by the user\"."), "the old paraphrase rule is gone");
    }

    #[test]
    fn installs_both_skills_and_replaces_the_legacy_remote_copy() {
        let home = TempHome::new();
        let legacy = home.0.join(".agents/skills/swiss");
        std::fs::create_dir_all(&legacy).expect("create old skill directory");
        std::fs::write(legacy.join("SKILL.md"), "old combined remote skill")
            .expect("create old skill file");
        std::fs::write(legacy.join("stale.txt"), "obsolete").expect("create stale file");

        let roots = [".agents", ".claude", ".cursor"];
        let mut expected = Vec::new();
        for root in roots {
            for (name, _) in SHIPPED_SKILLS {
                expected.push(
                    home.0
                        .join(root)
                        .join("skills")
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        let installed = install_skills_in(&home.0, None).expect("install both skills");
        assert_eq!(installed, expected);
        assert!(!legacy.join("stale.txt").exists());
        for root in roots {
            for (name, body) in SHIPPED_SKILLS {
                let path = home.0.join(root).join("skills").join(name).join("SKILL.md");
                assert_eq!(
                    std::fs::read(&path).expect("read installed skill"),
                    body.as_bytes()
                );
            }
        }
        assert_eq!(
            install_skills_in(&home.0, None).expect("re-install skills"),
            expected
        );
    }

    #[test]
    fn a_claude_config_dir_gets_the_skills_too() {
        let home = TempHome::new();
        let config = home.0.join(".claude-account");
        let installed = install_skills_in(&home.0, Some(&config)).expect("install with a config dir");
        assert_eq!(installed.len(), 4 * SHIPPED_SKILLS.len());
        for (name, body) in SHIPPED_SKILLS {
            let path = config.join("skills").join(name).join("SKILL.md");
            assert_eq!(std::fs::read(&path).expect("read the config dir's skill"), body.as_bytes());
        }
        // A config dir that is ~/.claude itself is written once, not twice.
        let same = install_skills_in(&home.0, Some(&home.0.join(".claude"))).expect("install with ~/.claude");
        assert_eq!(same.len(), 3 * SHIPPED_SKILLS.len());
    }
}
