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

//! The git hooks that nudge a sync (SPEC §memory.sync Triggers): one marked block,
//! installed and removed in place.

use super::*;

/// The marked block's first line (a hook's copy is compared by this whole line).
pub(super) const HOOK_BEGIN: &str = "# begin swiss memory sync -- managed; edits between the marks are replaced";
pub(super) const HOOK_END: &str = "# end swiss memory sync";

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn hook_block(exe: &Path, home: &str, port: u16) -> String {
    format!(
        "{HOOK_BEGIN}\nSWISS_HOME={} SWISS_PORT={} {} memory sync --nudge >/dev/null 2>&1 &\n{HOOK_END}\n",
        shell_quote(home),
        port,
        shell_quote(&exe.display().to_string())
    )
}

pub(super) fn hooks_dir(root: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .map_err(|err| format!("git could not start: {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(root.join(path))
    }
}

pub(super) fn edit_hook(path: &Path, install: bool, block: &str) -> Result<bool, String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    // Strip a previous block, wherever it sits.
    let mut kept: Vec<String> = Vec::new();
    let mut inside = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == HOOK_BEGIN {
            inside = true;
            continue;
        }
        if trimmed == HOOK_END {
            inside = false;
            continue;
        }
        if !inside {
            kept.push(line.to_string());
        }
    }
    if !install {
        let rest: Vec<&String> = kept.iter().filter(|l| !l.trim().is_empty()).collect();
        if rest.is_empty() || (rest.len() == 1 && rest[0].starts_with("#!")) {
            // Nothing of the operator's own is left: the file goes.
            let _ = std::fs::remove_file(path);
            return Ok(true);
        }
        let body = format!("{}\n", kept.join("\n"));
        std::fs::write(path, body).map_err(|err| format!("{} could not be written: {err}", path.display()))?;
        return Ok(false);
    }
    // Install: after the shebang when there is one, else at the top.
    let mut out: Vec<String> = Vec::new();
    let mut placed = false;
    for (index, line) in kept.iter().enumerate() {
        out.push(line.clone());
        if index == 0 && line.starts_with("#!") {
            out.push(String::new());
            out.extend(block.lines().map(str::to_string));
            placed = true;
        }
    }
    if !placed {
        let mut top: Vec<String> = block.lines().map(str::to_string).collect();
        top.extend(out);
        out = top;
    }
    if !out.last().is_some_and(|l| l.is_empty()) {
        out.push(String::new());
    }
    let body = out.join("\n");
    std::fs::write(path, body).map_err(|err| format!("{} could not be written: {err}", path.display()))?;
    Ok(false)
}

/// Install the marked block into `post-merge` and `pre-push` (SPEC §memory.sync
/// Triggers): files are created when missing, other tools' hooks preserved.
pub fn hooks_install(root: &Path, exe: &Path, home: &str, port: u16) -> Result<Vec<String>, String> {
    let dir = hooks_dir(root)?;
    std::fs::create_dir_all(&dir).map_err(|err| format!("{} could not be created: {err}", dir.display()))?;
    let block = hook_block(exe, home, port);
    let mut touched = Vec::new();
    for name in ["post-merge", "pre-push"] {
        let path = dir.join(name);
        edit_hook(&path, true, &block)?;
        touched.push(path.display().to_string());
    }
    Ok(touched)
}

/// Remove the marked block from both hooks; a file left with only a shebang is deleted.
pub fn hooks_uninstall(root: &Path) -> Result<Vec<String>, String> {
    let dir = hooks_dir(root)?;
    let mut touched = Vec::new();
    for name in ["post-merge", "pre-push"] {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let removed = edit_hook(&path, false, "")?;
        if !removed {
            touched.push(path.display().to_string());
        }
    }
    Ok(touched)
}
