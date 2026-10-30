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

    struct TempHome(PathBuf);

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
