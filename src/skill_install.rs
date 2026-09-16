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

//! Copy the shipped AI skill into the user-level directories an AI tool actually scans — port
//! of `skill-install.ts` + `skilldir.ts`, with the skill EMBEDDED in the binary (a single-file
//! release cannot read it from beside the package).
//!
//!   ~/.agents/skills/swiss/   the tool-agnostic convention
//!   ~/.claude/skills/swiss/   Claude Code's user-level path
//!   ~/.cursor/skills/swiss/   Cursor's user-level path
//!
//! Idempotent: a re-run replaces the copies, so upgrading the gateway and re-installing drops
//! exactly the newer package's files — never a merge with stale ones. Returns the targets
//! written, for the CLI to print.

use std::path::{Path, PathBuf};

/// The shipped skill, embedded at compile time. The swiss product's own copy — deliberately
/// diverged from the Node build's skill, which documents the Node-era CLI.
const SKILL_MD: &str = include_str!("skill_assets/SKILL.md");

const SKILL_NAME: &str = "swiss";

/// Pre-rename installs left directories named local-mcp-gateway in the same roots; removing
/// them keeps a tool from discovering the stale lmg-era skill beside the new one.
const SKILL_NAME_LEGACY: &str = "local-mcp-gateway";

/// Where the embedded skill would be staged from — reported in errors the way the Node build
/// reported its package dir.
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
    let home = PathBuf::from(home);
    let files: Vec<(&str, &str)> = vec![("SKILL.md", SKILL_MD)];
    let targets = [
        home.join(".agents").join("skills").join(SKILL_NAME),
        home.join(".claude").join("skills").join(SKILL_NAME),
        home.join(".cursor").join("skills").join(SKILL_NAME),
    ];
    let mut written: Vec<String> = Vec::new();
    for target in &targets {
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Stage the whole copy BESIDE the target, then swap — a failed stage leaves the previous
        // install untouched; only a complete copy is swapped in (the Node build's lesson after a
        // half install with no rollback).
        let staged = target.with_extension(format!("staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staged);
        if let Err(err) = write_tree(&files, &staged).and_then(|_| {
            std::fs::remove_dir_all(target)
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| e.to_string())
                .and_then(|_| std::fs::rename(&staged, target).map_err(|e| e.to_string()))
        }) {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(format!(
                "could not install skill to {}: {err}",
                target.display()
            ));
        }
        written.push(target.to_string_lossy().into_owned());
    }
    // Pre-rename installs left a skills/local-mcp-gateway directory in each root; remove it so
    // no tool keeps discovering the stale lmg-era skill beside the new one. Best-effort only:
    // an unreadable legacy directory must not fail the install.
    for target in &targets {
        let _ = std::fs::remove_dir_all(target.with_file_name(SKILL_NAME_LEGACY));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installing_removes_the_pre_rename_skill_directory() {
        // A pre-rename install left skills/local-mcp-gateway in each root; the install must
        // remove it so no tool discovers the stale lmg-era skill beside the new one.
        let _lock = swiss_core::paths::DATA_DIR_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("swiss-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join(".agents").join("skills").join(SKILL_NAME_LEGACY);
        std::fs::create_dir_all(&legacy).expect("legacy dir");
        std::fs::write(legacy.join("SKILL.md"), "stale lmg-era skill").expect("legacy file");
        let prev = std::env::var_os("USERPROFILE");
        std::env::set_var("USERPROFILE", &dir);
        let written = install_skill().expect("install");
        match prev {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        assert!(!legacy.exists(), "the pre-rename skill dir is removed");
        assert!(dir.join(".agents").join("skills").join("swiss").join("SKILL.md").exists());
        assert!(written.iter().any(|w| w.contains("swiss")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shipped_skill_carries_valid_frontmatter() {
        // The embedded file carries the skill frontmatter the tools scan for. The body is the
        // swiss product's own (swiss commands, SWISS_HOME) — it deliberately no longer mirrors
        // the Node build's skill, which documents the Node-era CLI.
        assert!(SKILL_MD.contains("name: swiss"));
        assert!(SKILL_MD.contains("disable-model-invocation: true"));
    }
}
