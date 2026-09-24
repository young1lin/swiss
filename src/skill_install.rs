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

//! Copy both embedded AI skills into the user-level directories an AI tool actually scans.
//! The general `swiss` skill covers safe setup; `swiss-remote` holds the remote CLI contract.
//! Each goes under ~/.agents/skills, ~/.claude/skills, and ~/.cursor/skills. A single-file
//! release cannot read either skill from beside the binary.
//!
//! Idempotent: a re-run replaces each copy, so upgrading and re-installing drops stale files.
//! Returns all written targets for the CLI to print.

use std::path::{Path, PathBuf};

/// Both shipped skills are embedded at compile time; Node is never needed to build the exe.
const SKILL_MD: &str = include_str!("skill_assets/SKILL.md");
const REMOTE_SKILL_MD: &str = include_str!("skill_assets/remote/SKILL.md");

const SKILL_NAME: &str = "swiss";
const REMOTE_SKILL_NAME: &str = "swiss-remote";
const SHIPPED_SKILLS: [(&str, &str); 2] =
    [(SKILL_NAME, SKILL_MD), (REMOTE_SKILL_NAME, REMOTE_SKILL_MD)];

/// Virtual source directory for the general setup skill, kept for callers that report it.
/// The remote skill lives at the sibling `<embedded>/.agents/skills/swiss-remote` path.
pub fn skill_dir() -> PathBuf {
    PathBuf::from(format!("<embedded>/.agents/skills/{SKILL_NAME}"))
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
    install_skills_in(&PathBuf::from(home))
}

fn install_skills_in(home: &Path) -> Result<Vec<String>, String> {
    let roots = [
        home.join(".agents").join("skills"),
        home.join(".claude").join("skills"),
        home.join(".cursor").join("skills"),
    ];
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
                std::fs::remove_dir_all(&target)
                    .or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| e.to_string())
                    .and_then(|_| std::fs::rename(&staged, &target).map_err(|e| e.to_string()))
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
        let installed = install_skills_in(&home.0).expect("install both skills");
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
            install_skills_in(&home.0).expect("re-install skills"),
            expected
        );
    }
}
