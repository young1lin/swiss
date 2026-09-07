//! Copy the shipped AI skill into the user-level directories an AI tool actually scans — port
//! of `skill-install.ts` + `skilldir.ts`, with the skill EMBEDDED in the binary (a single-file
//! release cannot read it from beside the package).
//!
//!   ~/.agents/skills/local-mcp-gateway/   the tool-agnostic convention
//!   ~/.claude/skills/local-mcp-gateway/   Claude Code's user-level path
//!   ~/.cursor/skills/local-mcp-gateway/   Cursor's user-level path
//!
//! Idempotent: a re-run replaces the copies, so upgrading the gateway and re-installing drops
//! exactly the newer package's files — never a merge with stale ones. Returns the targets
//! written, for the CLI to print.

use std::path::{Path, PathBuf};

/// The shipped skill, embedded at compile time (byte-for-byte the Node build's SKILL.md).
const SKILL_MD: &str = include_str!("skill_assets/SKILL.md");

const SKILL_NAME: &str = "local-mcp-gateway";

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
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_skill_is_the_node_builds() {
        // The embedded file carries the skill frontmatter the tools scan for.
        assert!(SKILL_MD.contains("name: local-mcp-gateway"));
        assert!(SKILL_MD.contains("disable-model-invocation: true"));
    }
}
