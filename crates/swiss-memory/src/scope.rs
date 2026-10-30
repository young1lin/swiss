/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Project scope (SPEC §memory.scope). A walk-up from a directory finds the nearest
//! boundary; at one directory an explicit `.swiss/memory.json` binding wins, then a
//! registered exact-directory (`path:`) binding, then Git. Resolution is pure: it
//! never writes, never spawns git, and never reads the process environment - the
//! CLI passes `GIT_DIR` in explicitly, so the gateway's environment cannot steer an
//! MCP call.
//!
//! Git identity is the canonical COMMON dir: a linked worktree's `.git` file is
//! followed to its gitdir and then to `commondir`, so every worktree of one
//! repository is one project while independent clones stay apart. Its root is the
//! main checkout too: a worktree is a branch's scratch copy, deleted when the branch
//! lands, and a project first written from one was named after it and kept a root
//! that vanished with it. A bare repository has no main checkout; its worktree stays
//! the root. The checkout is the other half: the working tree the directory is in -
//! the worktree itself - where a call reads the code its caller sees, the branch's
//! new files included.

use std::path::{Path, PathBuf};

use crate::model::RootKind;

/// The sentinel project id shared notes live under: explicit, never a fallback.
pub const GLOBAL_PROJECT_ID: &str = "global";

/// The portable binding file, relative to a directory.
pub const MARKER_FILE: [&str; 2] = [".swiss", "memory.json"];

/// The scope one operation runs in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// One project's notes.
    Project(String),
    /// The shared notes (project id `global`).
    Global,
}

impl Scope {
    /// The project id every store predicate for this scope uses - one equality is
    /// what makes cross-project leakage impossible by construction.
    pub fn project_id(&self) -> &str {
        match self {
            Scope::Project(id) => id.as_str(),
            Scope::Global => GLOBAL_PROJECT_ID,
        }
    }

    /// `global` or a project id, as the wire spells it.
    pub fn of(id: &str) -> Scope {
        if id == GLOBAL_PROJECT_ID {
            Scope::Global
        } else {
            Scope::Project(id.to_string())
        }
    }
}

/// The project id a `.swiss/memory.json` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerBinding {
    pub project_id: String,
    pub name: Option<String>,
}

/// What a walk-up found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The canonical project root (the directory carrying the boundary).
    pub root: PathBuf,
    /// The working tree the directory is in, canonical: a linked worktree's own top
    /// while `root` is its main checkout, else the root itself.
    pub checkout: PathBuf,
    /// `git:<common dir>`, `path:<dir>` or `marker:<dir>`, all canonical.
    pub identity: String,
    pub kind: RootKind,
    /// The normalized `origin` remote (Git only), userinfo stripped.
    pub origin_url: Option<String>,
    /// The explicit binding (marker only).
    pub marker: Option<MarkerBinding>,
}

/// A path for humans: the canonical form minus Windows' verbatim prefix.
pub fn display(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(text)
}

/// `/mnt/<drive>/<rest>` to `<DRIVE>:\<rest>` (the WSL leg of a Windows gateway).
pub fn map_wsl(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("/mnt/")?;
    let mut parts = rest.splitn(2, '/');
    let drive = parts.next()?;
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let tail = parts.next().unwrap_or("").replace('/', "\\");
    Some(format!("{}:\\{}", drive.to_ascii_uppercase(), tail))
}

/// Validate a caller-supplied directory (an MCP `cwd`, a panel path): absolute,
/// existing, mapped from WSL form on Windows, canonical. A file names its parent.
pub fn normalize_dir(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("cwd is required: pass your own absolute working directory".into());
    }
    let mapped = if cfg!(windows) && raw.starts_with('/') {
        match map_wsl(raw) {
            Some(mapped) => mapped,
            None => {
                return Err(format!(
                    "{raw} is a POSIX path this Windows gateway cannot map; pass a Windows path (C:\\...) or /mnt/<drive>/..."
                ))
            }
        }
    } else {
        raw.to_string()
    };
    let path = PathBuf::from(&mapped);
    if !path.is_absolute() {
        return Err(format!("{raw} is not absolute; pass your own absolute working directory"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("{raw} does not exist on the gateway's machine"))?;
    if canonical.is_file() {
        return canonical
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{raw} has no parent directory"));
    }
    Ok(canonical)
}

/// The Swiss home, canonical when it exists: found roots come back canonical
/// (`\\?\C:\...` on Windows) and `Path::starts_with` compares components textually,
/// so both sides must have the same form.
fn home_canonical() -> PathBuf {
    let home = swiss_core::paths::data_dir();
    home.canonicalize().unwrap_or(home)
}

/// Whether a canonical path lies inside the Swiss home (never a project).
pub fn inside_home(path: &Path) -> bool {
    path.starts_with(home_canonical())
}

/// The identity of an exact-directory binding.
pub fn path_identity(dir: &Path) -> String {
    format!("path:{}", dir.display())
}

/// An exact-directory binding for `dir` (canonical).
pub fn exact(dir: &Path) -> Result<Found, String> {
    if inside_home(dir) {
        return Err(format!("{} is inside the Swiss home, which is never a project", display(dir)));
    }
    Ok(Found {
        root: dir.to_path_buf(),
        checkout: dir.to_path_buf(),
        identity: path_identity(dir),
        kind: RootKind::Path,
        origin_url: None,
        marker: None,
    })
}

/// Walk up from a canonical `cwd` to the nearest boundary. `is_exact` answers
/// whether a `path:` identity is registered. Ok(None) means no boundary; Err means
/// a boundary exists but cannot be read (a malformed binding file) - never guessed.
pub fn resolve(cwd: &Path, is_exact: &dyn Fn(&str) -> bool) -> Result<Option<Found>, String> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if let Some(found) = boundary_at(dir, is_exact)? {
            if inside_home(&found.root) {
                return Ok(None);
            }
            return Ok(Some(found));
        }
        current = dir.parent();
    }
    Ok(None)
}

/// One directory's claim, in precedence order.
fn boundary_at(dir: &Path, is_exact: &dyn Fn(&str) -> bool) -> Result<Option<Found>, String> {
    let marker = dir.join(MARKER_FILE[0]).join(MARKER_FILE[1]);
    if marker.is_file() {
        let binding = read_marker(&marker)?;
        let checkout = dir.canonicalize().map_err(|e| format!("{}: {e}", display(dir)))?;
        // A committed marker is checked out into every worktree too: the main
        // checkout's own, naming the same project, is the root, as Git's would be.
        let root = marked_main(&checkout, &binding).unwrap_or_else(|| checkout.clone());
        return Ok(Some(Found {
            identity: format!("marker:{}", root.display()),
            root,
            checkout,
            kind: RootKind::Marker,
            origin_url: None,
            marker: Some(binding),
        }));
    }
    let identity = path_identity(dir);
    if is_exact(&identity) {
        return Ok(Some(Found {
            root: dir.to_path_buf(),
            checkout: dir.to_path_buf(),
            identity,
            kind: RootKind::Path,
            origin_url: None,
            marker: None,
        }));
    }
    let dot_git = dir.join(".git");
    if dot_git.exists() {
        return git_at(dir, &dot_git).map(Some);
    }
    Ok(None)
}

/// Parse `.swiss/memory.json`: `{schemaVersion: 1, projectId, name?}`.
fn read_marker(file: &Path) -> Result<MarkerBinding, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", display(file)))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", display(file)))?;
    if value.get("schemaVersion").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(format!("{} must carry \"schemaVersion\": 1", display(file)));
    }
    let project_id = value
        .get("projectId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| crate::model::is_project_id(id) && *id != GLOBAL_PROJECT_ID)
        .ok_or_else(|| format!("{} must name a projectId like p-0123abcd", display(file)))?;
    Ok(MarkerBinding {
        project_id: project_id.to_string(),
        name: value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|n| !n.trim().is_empty()),
    })
}

/// The Git identity of a working tree whose `.git` entry is `dot_git`.
fn git_at(dir: &Path, dot_git: &Path) -> Result<Found, String> {
    let root = dir.canonicalize().map_err(|e| format!("{}: {e}", display(dir)))?;
    let git_dir = if dot_git.is_dir() {
        dot_git.to_path_buf()
    } else {
        gitdir_of_file(dot_git).ok_or_else(|| {
            format!(
                "{} is a .git file without a readable gitdir; bind the directory by hand (swiss memory init --here)",
                display(dot_git)
            )
        })?
    };
    let common = common_dir(&git_dir)?;
    let checkout = root.clone();
    let root = main_checkout(&git_dir, &common).unwrap_or(root);
    Ok(Found {
        root,
        checkout,
        identity: format!("git:{}", common.display()),
        kind: RootKind::Git,
        origin_url: origin_url(&common),
        marker: None,
    })
}

/// The Git identity for an explicit `GIT_DIR` (the CLI's git-hook case).
pub fn from_git_dir(git_dir: &Path, cwd: &Path) -> Result<Found, String> {
    let git_dir = git_dir
        .canonicalize()
        .map_err(|e| format!("GIT_DIR {}: {e}", git_dir.display()))?;
    let common = common_dir(&git_dir)?;
    let (root, checkout) = if let Some(main) = main_checkout(&git_dir, &common) {
        (main, worktree_top(&git_dir).unwrap_or_else(|| cwd.to_path_buf()))
    } else if git_dir.file_name().map(|n| n == ".git").unwrap_or(false) {
        let top = git_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.to_path_buf());
        (top.clone(), top)
    } else {
        (cwd.to_path_buf(), cwd.to_path_buf())
    };
    if inside_home(&root) {
        return Err(format!("{} is inside the Swiss home, which is never a project", display(&root)));
    }
    Ok(Found {
        root,
        checkout,
        identity: format!("git:{}", common.display()),
        kind: RootKind::Git,
        origin_url: origin_url(&common),
        marker: None,
    })
}

/// The `gitdir:` target of a linked worktree's `.git` file.
fn gitdir_of_file(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let line = text.lines().find(|l| l.trim_start().starts_with("gitdir:"))?;
    let target = line.trim_start().trim_start_matches("gitdir:").trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        file.parent()?.join(target)
    };
    resolved.canonicalize().ok()
}

/// Follow `commondir` when present (linked worktrees), canonical either way.
fn common_dir(git_dir: &Path) -> Result<PathBuf, String> {
    let pointer = git_dir.join("commondir");
    let common = match std::fs::read_to_string(&pointer) {
        Ok(text) => {
            let target = Path::new(text.trim());
            if target.is_absolute() {
                target.to_path_buf()
            } else {
                git_dir.join(target)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    };
    common
        .canonicalize()
        .map_err(|e| format!("git common dir {}: {e}", display(&common)))
}

/// The main checkout of a linked worktree (`git_dir` is its admin dir under `common`):
/// the directory holding a non-bare common dir named `.git`. None for a main checkout
/// or a submodule (no `commondir`, so `git_dir` is the common dir) and for a bare
/// repository, which has no main checkout - even a bare one cloned into `.git`.
fn main_checkout(git_dir: &Path, common: &Path) -> Option<PathBuf> {
    let linked = git_dir.canonicalize().ok()? != common;
    if !linked || common.file_name()? != ".git" || is_bare(common) {
        return None;
    }
    common.parent().filter(|main| main.is_dir()).map(Path::to_path_buf)
}

/// A linked worktree's own top, from its admin dir: `gitdir` there names the
/// worktree's `.git` file. None when it is unreadable or the worktree is gone.
fn worktree_top(admin: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(admin.join("gitdir")).ok()?;
    let dot_git = Path::new(text.trim());
    let dot_git = if dot_git.is_absolute() { dot_git.to_path_buf() } else { admin.join(dot_git) };
    dot_git.parent()?.canonicalize().ok().filter(|top| top.is_dir())
}

/// The main checkout whose own marker names the same project as the one a linked
/// worktree `dir` carries; None for any other directory or a marker only the
/// worktree's branch has.
fn marked_main(dir: &Path, binding: &MarkerBinding) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if !dot_git.is_file() {
        return None;
    }
    let git_dir = gitdir_of_file(&dot_git)?;
    let common = common_dir(&git_dir).ok()?;
    let main = main_checkout(&git_dir, &common)?;
    let theirs = read_marker(&main.join(MARKER_FILE[0]).join(MARKER_FILE[1])).ok()?;
    (theirs.project_id == binding.project_id).then_some(main)
}

/// The common dir a `git:` identity names.
pub fn git_common(identity: &str) -> Option<PathBuf> {
    identity.strip_prefix("git:").map(PathBuf::from)
}

/// The working tree `branch` is checked out in among a repository's: the main
/// checkout when its HEAD is on it, else the linked worktree whose HEAD is. Read from
/// the common dir's own files (`HEAD`, `worktrees/<name>/HEAD` and `gitdir`), no git
/// spawned; None when no working tree has the branch checked out.
pub fn branch_checkout(common: &Path, branch: &str) -> Option<PathBuf> {
    let wanted = format!("ref: refs/heads/{}", branch.trim().trim_start_matches("refs/heads/"));
    let on = |head: PathBuf| std::fs::read_to_string(head).map(|text| text.trim() == wanted).unwrap_or(false);
    let named_git = common.file_name().map(|n| n == ".git").unwrap_or(false);
    if named_git && !is_bare(common) && on(common.join("HEAD")) {
        if let Some(main) = common.parent().filter(|main| main.is_dir()) {
            return Some(main.to_path_buf());
        }
    }
    let mut admins: Vec<PathBuf> = std::fs::read_dir(common.join("worktrees")).ok()?.flatten().map(|e| e.path()).collect();
    admins.sort();
    admins.into_iter().filter(|admin| on(admin.join("HEAD"))).find_map(|admin| worktree_top(&admin))
}

/// Whether `<common>/config` says `[core] bare = true`.
fn is_bare(common: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(common.join("config")) else { return false };
    let mut in_core = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_core = line.replace(' ', "").eq_ignore_ascii_case("[core]");
            continue;
        }
        if in_core {
            // A bare key with no value is git's "true".
            let (key, value) = line.split_once('=').unwrap_or((line, "true"));
            if key.trim().eq_ignore_ascii_case("bare") {
                let value = value.trim().to_ascii_lowercase();
                return matches!(value.as_str(), "true" | "yes" | "on" | "1");
            }
        }
    }
    false
}

/// The `origin` remote URL from `<common>/config`, normalized.
fn origin_url(common: &Path) -> Option<String> {
    let text = std::fs::read_to_string(common.join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(' ', "") == "[remote\"origin\"]";
            continue;
        }
        if in_origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "url" {
                    return normalize_origin(value.trim());
                }
            }
        }
    }
    None
}

/// `host/path` form, lowercase, no userinfo, no port, no `.git` suffix: the shape
/// two clones of one repository share whatever transport they were cloned over.
pub fn normalize_origin(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        match rest.split_once('/') {
            Some((host, path)) => (host.to_string(), path.to_string()),
            None => (rest.to_string(), String::new()),
        }
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| l.len() > 1 && !l.contains('/')) {
        // scp-like `user@host:owner/repo`; a one-letter left side is a drive.
        let host = left.rsplit_once('@').map(|(_, h)| h).unwrap_or(left);
        (host.to_string(), path.to_string())
    } else {
        return Some(url.trim_end_matches('/').trim_end_matches(".git").to_ascii_lowercase());
    };
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    let path = path.trim_matches('/').trim_end_matches(".git").to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    Some(if path.is_empty() { host } else { format!("{host}/{path}") })
}

/// The default name for a root: its final path component.
pub fn default_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-mem-scope-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.canonicalize().expect("canonical temp dir")
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn a_nested_directory_resolves_to_its_repository() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src").join("deep")).unwrap();
        let found = resolve(&root.join("src").join("deep"), &none).unwrap().expect("resolves");
        assert_eq!(found.kind, RootKind::Git);
        assert_eq!(found.root, root);
        let again = resolve(&root, &none).unwrap().expect("resolves from the root too");
        assert_eq!(found.identity, again.identity);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn linked_worktrees_share_the_common_dir_identity() {
        let root = temp_dir();
        let main = root.join("main");
        let wt_git = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let work = root.join("wt");
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        std::fs::write(wt_git.join("gitdir"), format!("{}\n", work.join(".git").display())).unwrap();
        let from_main = resolve(&main, &none).unwrap().expect("main resolves");
        let from_wt = resolve(&work.join("src"), &none).unwrap().expect("worktree resolves");
        assert_eq!(from_main.identity, from_wt.identity, "one repository, one project");
        assert_eq!(from_wt.root, main, "a worktree's root is the main checkout, never the worktree");
        assert_eq!(from_wt.checkout, work, "while its code is read where its branch is checked out");
        assert_eq!((&from_main.root, &from_main.checkout), (&main, &main), "a main checkout is its own");
        let explicit = from_git_dir(&wt_git, &root).unwrap();
        assert_eq!((explicit.identity, explicit.root, explicit.checkout), (from_main.identity, main, work));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_branch_names_the_working_tree_it_is_checked_out_in() {
        let root = temp_dir();
        let main = root.join("main");
        let common = main.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/develop\n").unwrap();
        let mut trees = Vec::new();
        for (name, branch) in [("a", "feature/a"), ("b", "feature/b")] {
            let admin = common.join("worktrees").join(name);
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(admin.join("commondir"), "../..\n").unwrap();
            std::fs::write(admin.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
            let work = root.join("trees").join(name);
            std::fs::create_dir_all(&work).unwrap();
            std::fs::write(work.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
            std::fs::write(admin.join("gitdir"), format!("{}\n", work.join(".git").display())).unwrap();
            trees.push(work);
        }
        let common = common.canonicalize().unwrap();
        assert_eq!(branch_checkout(&common, "develop"), Some(main.clone()), "the main checkout's own branch");
        assert_eq!(branch_checkout(&common, "feature/b"), Some(trees[1].clone()));
        assert_eq!(branch_checkout(&common, "refs/heads/feature/a"), Some(trees[0].clone()), "a full ref too");
        assert_eq!(branch_checkout(&common, "feature"), None, "a prefix is not a branch");
        assert_eq!(branch_checkout(&common, "gone"), None, "a branch no tree has checked out");
        std::fs::remove_dir_all(&trees[0]).unwrap();
        assert_eq!(branch_checkout(&common, "feature/a"), None, "a removed worktree is no checkout");
        let identity = resolve(&main, &none).unwrap().expect("resolves").identity;
        assert_eq!(git_common(&identity), Some(common));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_worktrees_committed_marker_is_the_main_checkouts() {
        let root = temp_dir();
        let marker = r#"{"schemaVersion":1,"projectId":"p-0123abcd"}"#;
        let main = root.join("main");
        let admin = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let work = main.join(".agents").join("worktrees").join("wt");
        for dir in [&main, &work] {
            std::fs::create_dir_all(dir.join(".swiss")).unwrap();
            std::fs::write(dir.join(".swiss").join("memory.json"), marker).unwrap();
        }
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        let from_main = resolve(&main, &none).unwrap().expect("main resolves");
        let from_wt = resolve(&work, &none).unwrap().expect("worktree resolves");
        assert_eq!(from_wt.identity, from_main.identity, "one binding, never one per worktree");
        assert_eq!((from_wt.root, from_wt.checkout), (main.clone(), work.clone()));
        // A marker only the worktree's branch has binds the worktree itself.
        std::fs::write(main.join(".swiss").join("memory.json"), r#"{"schemaVersion":1,"projectId":"p-89abcdef"}"#).unwrap();
        let other = resolve(&work, &none).unwrap().expect("resolves");
        assert_eq!((other.root, other.checkout), (work.clone(), work));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bare_repositorys_worktree_stays_the_root() {
        let root = temp_dir();
        // The bare-in-.git layout: `git clone --bare url repo/.git`, worktrees beside it.
        let common = root.join("repo").join(".git");
        let admin = common.join("worktrees").join("main");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(common.join("config"), "[core]\n\trepositoryformatversion = 0\n\tbare = true\n").unwrap();
        let work = root.join("repo").join("main");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        let found = resolve(&work, &none).unwrap().expect("resolves");
        assert_eq!(found.root, work, "a bare repository has no main checkout to move to");
        assert_eq!(found.checkout, work);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_submodule_is_its_own_root() {
        let root = temp_dir();
        let modules = root.join("super").join(".git").join("modules").join("lib");
        std::fs::create_dir_all(&modules).unwrap();
        let sub = root.join("super").join("lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", modules.display())).unwrap();
        let found = resolve(&sub, &none).unwrap().expect("resolves");
        assert_eq!(found.root, sub, "a submodule has no commondir: it is not a worktree of the superproject");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_marker_file_wins_and_must_be_well_formed() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".swiss")).unwrap();
        std::fs::write(
            root.join(".swiss").join("memory.json"),
            r#"{"schemaVersion":1,"projectId":"p-0123abcd","name":"portable"}"#,
        )
        .unwrap();
        let found = resolve(&root, &none).unwrap().expect("resolves");
        assert_eq!(found.kind, RootKind::Marker);
        assert_eq!(found.marker.as_ref().map(|m| m.project_id.as_str()), Some("p-0123abcd"));
        std::fs::write(root.join(".swiss").join("memory.json"), "{}").unwrap();
        assert!(resolve(&root, &none).is_err(), "a malformed binding is an error, never a guess");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_exact_binding_below_a_repository_is_the_nearer_boundary() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("packages").join("api");
        std::fs::create_dir_all(sub.join("src")).unwrap();
        let identity = path_identity(&sub);
        let found = resolve(&sub.join("src"), &|id: &str| id == identity).unwrap().expect("resolves");
        assert_eq!(found.kind, RootKind::Path);
        assert_eq!(found.root, sub);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_ancestor_means_no_project() {
        let orphan = temp_dir();
        assert!(resolve(&orphan, &none).unwrap().is_none());
        std::fs::remove_dir_all(&orphan).ok();
    }

    #[test]
    fn wsl_paths_map_to_drive_paths() {
        assert_eq!(map_wsl("/mnt/c/Users/me/repo").as_deref(), Some("C:\\Users\\me\\repo"));
        assert_eq!(map_wsl("/mnt/d").as_deref(), Some("D:\\"));
        assert_eq!(map_wsl("/home/me"), None);
        assert_eq!(map_wsl("/mnt/wsl/x"), None);
    }

    #[test]
    fn origins_normalize_across_transports_and_drop_userinfo() {
        let https = normalize_origin("https://user:secret@GitHub.com/Owner/Repo.git");
        let ssh = normalize_origin("git@github.com:owner/repo.git");
        let ssh_url = normalize_origin("ssh://git@github.com:22/owner/repo");
        assert_eq!(https.as_deref(), Some("github.com/owner/repo"));
        assert_eq!(https, ssh);
        assert_eq!(https, ssh_url);
        assert!(!https.unwrap_or_default().contains("secret"));
    }

    #[test]
    fn relative_and_missing_directories_are_refused() {
        assert!(normalize_dir("").is_err());
        assert!(normalize_dir("relative/dir").is_err());
        let missing = std::env::temp_dir().join("swiss-no-such-dir-xyz-0");
        assert!(normalize_dir(&missing.display().to_string()).is_err());
    }
}
