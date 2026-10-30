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

//! Read-only previews of a project's checkout (SPEC §memory.preview): the file or
//! directory an anchor names, the files a glob anchor matches, the commits that touched
//! a path, one commit, and a path's uncommitted change. The panel's only; the CLI and
//! /mcp/memory never reach them - except the file listing, which the code index's lister
//! ([CheckoutLister], SPEC §memory.code) shares with the CLI.
//!
//! Everything stays inside the project's first reachable root - the checkout freshness
//! hashes anchors against. A path is repo-relative, never absolute and never climbing,
//! and is canonicalized and rechecked against the root so a link cannot lead out. Names
//! that hold credentials are refused before a byte is read, by the name asked and the
//! name it resolves to. Git runs as a child with no prompt, no optional locks, no
//! external diff or text conversion, literal pathspecs, a time limit and capped output;
//! a commit is named by hex digits only, so nothing a caller sends reaches git as an
//! option or a revision expression.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use swiss_memory::code::{Lister, Listing};
use swiss_memory::{glob, scope, Checkout, Code, CommitFiles, GitIssue, MemError, MemResult, ProjectView};
use tokio::io::AsyncReadExt;

/// A file is shown up to this many bytes, then cut at a line end.
pub const FILE_BYTES_MAX: u64 = 512 * 1024;
pub const FILE_LINES_MAX: usize = 5_000;
pub const DIR_ENTRIES_MAX: usize = 500;
pub const GLOB_FILES_MAX: usize = 200;
pub const LOG_LIMIT_DEFAULT: u64 = 30;
pub const LOG_LIMIT_MAX: u64 = 50;
pub const PATCH_BYTES_MAX: usize = 256 * 1024;
/// `git ls-files` of a large checkout.
const LIST_BYTES_MAX: usize = 8 * 1024 * 1024;
/// Commit metadata, file lists and stderr.
const META_BYTES_MAX: usize = 256 * 1024;
const BINARY_SNIFF: usize = 8 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// The no-git glob fallback walks at most this many entries.
const WALK_MAX: usize = 20_000;
/// Directories the no-git walk never enters.
const WALK_SKIP: &[&str] = &[".git", "node_modules", "target", ".venv", "venv", "__pycache__"];

/// %H %h %an %at %s, unit- and record-separated: no subject can split a field.
const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e";
/// Coverage's log: a record mark first, so `--name-only`'s paths close each record.
const NAME_LOG_FORMAT: &str = "--format=%x1e%H%x1f%at%x1f%s";
/// The commits coverage reads for drift (SPEC §memory.domains).
const COVERAGE_COMMITS: &str = "-n500";
const SHOW_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%P%x1f%s%x1f%b";

// --- shapes --------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// The checkout, as the user would type it.
    pub root: String,
    pub path: String,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Body {
    #[serde(rename_all = "camelCase")]
    File {
        size: u64,
        modified_ms: Option<i64>,
        binary: bool,
        /// Cut at FILE_BYTES_MAX or FILE_LINES_MAX.
        truncated: bool,
        lines: usize,
        /// None for a binary file.
        text: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Dir { entries: Vec<Entry>, truncated: bool },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// A name `sensitive` refuses: listed, never opened.
    pub sensitive: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Matching {
    pub root: String,
    pub glob: String,
    pub files: Vec<String>,
    pub truncated: bool,
    /// Listed by git (tracked and untracked, ignores honoured) or, outside a git
    /// checkout, by a bounded walk.
    pub git: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub short: String,
    /// The author's name; never the address.
    pub author: String,
    pub date_ms: i64,
    pub subject: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub root: String,
    pub path: String,
    /// Newest first, following renames for a file.
    pub commits: Vec<Commit>,
    /// More commits exist past the limit.
    pub more: bool,
    /// The path differs from HEAD in the working tree or the index.
    pub dirty: bool,
    /// The path is not known to git (and not ignored).
    pub untracked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub path: String,
    /// git's letter: A, M, D, T.
    pub status: String,
    /// None for a binary file.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    pub root: String,
    #[serde(flatten)]
    pub commit: Commit,
    pub body: String,
    pub parents: Vec<String>,
    /// Every file the commit changed (against its first parent).
    pub files: Vec<Changed>,
    /// The patch: the whole commit, or only `path` when one was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub patch: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingDiff {
    pub root: String,
    pub path: String,
    /// The working tree and the index against HEAD.
    pub patch: String,
    pub truncated: bool,
    pub untracked: bool,
}

// --- the checkout and its paths -------------------------------------------------------------

pub(crate) fn unavailable(issue: GitIssue, message: impl Into<String>) -> MemError {
    MemError::new(Code::GitUnavailable, message).with("issue", json!(issue.as_str()))
}

/// The issue a git error carries (`unavailable`); any other error is a failure.
pub fn issue_of(err: &MemError) -> GitIssue {
    err.extra.get("issue").and_then(|v| v.as_str()).and_then(GitIssue::parse).unwrap_or(GitIssue::GitFailed)
}

/// The checkout a project's previews read: its first root that is a directory now (the
/// one freshness checks against), canonical. Global has none.
pub fn root_of(project: &ProjectView) -> MemResult<PathBuf> {
    project
        .roots
        .iter()
        .map(|r| Path::new(&r.path))
        .find(|p| p.is_dir())
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| {
            if project.global {
                MemError::not_found("global notes have no checkout to preview")
            } else {
                MemError::not_found("no directory of this project is reachable on this machine")
                    .with("issue", json!(GitIssue::NoDirectory.as_str()))
            }
        })
}

/// A caller's repo-relative path, normalized the way anchors are stored. Absolute,
/// drive-qualified and climbing paths are refused; "" names the root itself.
pub fn relative(raw: &str) -> MemResult<String> {
    let path = glob::normalize(raw);
    if path.chars().any(char::is_control) {
        return Err(MemError::invalid("path has a control character"));
    }
    if path.starts_with('/') || Path::new(&path).components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(MemError::invalid("path must be relative to the project's directory, without .."));
    }
    Ok(path)
}

/// Whether a repo-relative path names something that holds credentials or git's own
/// state: refused before it is read (the code index never reads one either).
pub fn sensitive(path: &str) -> bool {
    swiss_memory::secrets::sensitive_path(path)
}

fn refused(path: &str) -> MemError {
    MemError::new(Code::SensitiveContent, format!("{path} may hold credentials; it is not previewed"))
        .with("rule", serde_json::json!("sensitive_name"))
}

/// `rel` under `root`, canonical, inside, and not sensitive by either name.
fn inside(root: &Path, rel: &str) -> MemResult<PathBuf> {
    if sensitive(rel) {
        return Err(refused(rel));
    }
    let target = root
        .join(rel)
        .canonicalize()
        .map_err(|_| MemError::not_found(format!("{} does not exist in the checkout", shown(rel))))?;
    let Ok(resolved) = target.strip_prefix(root) else {
        return Err(MemError::invalid(format!("{rel} leads outside the project's directory")));
    };
    let resolved = resolved.to_string_lossy().replace('\\', "/");
    if sensitive(&resolved) {
        return Err(refused(rel));
    }
    Ok(target)
}

fn shown(rel: &str) -> &str {
    if rel.is_empty() {
        "the project's directory"
    } else {
        rel
    }
}

// --- files ---------------------------------------------------------------------------------

/// The file or directory `rel` names.
pub fn read(root: &Path, rel: &str) -> MemResult<Preview> {
    let target = inside(root, rel)?;
    let meta = std::fs::metadata(&target)
        .map_err(|err| MemError::invalid(format!("{} could not be read: {err}", shown(rel))))?;
    let body = if meta.is_dir() { listing(&target, rel)? } else { file(&target, rel, &meta)? };
    Ok(Preview { root: scope::display(root), path: rel.to_string(), body })
}

fn file(target: &Path, rel: &str, meta: &std::fs::Metadata) -> MemResult<Body> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(target)
        .and_then(|f| f.take(FILE_BYTES_MAX + 1).read_to_end(&mut bytes))
        .map_err(|err| MemError::invalid(format!("{rel} could not be read: {err}")))?;
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        return Ok(Body::File { size: meta.len(), modified_ms, binary: true, truncated: false, lines: 0, text: None });
    }
    let (text, truncated) = clip_text(&bytes, FILE_BYTES_MAX as usize, FILE_LINES_MAX);
    let lines = if text.is_empty() { 0 } else { text.trim_end_matches('\n').split('\n').count() };
    Ok(Body::File { size: meta.len(), modified_ms, binary: false, truncated, lines, text: Some(text) })
}

/// UTF-8 (lossy) without a BOM, cut at `max_bytes` or `max_lines`, always at a line end.
fn clip_text(bytes: &[u8], max_bytes: usize, max_lines: usize) -> (String, bool) {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut truncated = false;
    let mut end = bytes.len();
    if end > max_bytes {
        truncated = true;
        end = bytes[..max_bytes].iter().rposition(|b| *b == b'\n').map(|at| at + 1).unwrap_or(max_bytes);
    }
    if let Some((at, _)) = bytes[..end].iter().enumerate().filter(|(_, b)| **b == b'\n').nth(max_lines - 1) {
        if at + 1 < end {
            truncated = true;
            end = at + 1;
        }
    }
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), truncated)
}

fn listing(dir: &Path, rel: &str) -> MemResult<Body> {
    let read = std::fs::read_dir(dir).map_err(|err| MemError::invalid(format!("{} could not be listed: {err}", shown(rel))))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        if entries.len() == DIR_ENTRIES_MAX {
            truncated = true;
            break;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let size = if is_dir { None } else { entry.metadata().ok().map(|m| m.len()) };
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        entries.push(Entry { sensitive: sensitive(&child), name, dir: is_dir, size });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Body::Dir { entries, truncated })
}

/// The files a glob anchor matches now.
pub async fn matching(root: &Path, pattern: &str) -> MemResult<Matching> {
    let pattern = glob::normalize(pattern);
    if pattern.is_empty() {
        return Err(MemError::invalid("glob is required"));
    }
    let (names, mut truncated, by_git) = list_files(root).await?;
    let mut files: Vec<String> = names.into_iter().filter(|name| glob::matches(&pattern, name)).collect();
    files.sort();
    files.dedup();
    if files.len() > GLOB_FILES_MAX {
        files.truncate(GLOB_FILES_MAX);
        truncated = true;
    }
    Ok(Matching { root: scope::display(root), glob: pattern, files, truncated, git: by_git })
}

/// A checkout's files: git's tracked and unignored ones, or the bounded walk outside Git -
/// (files, truncated, listed by git). A listing cut at its cap loses its last, partial name.
pub async fn list_files(root: &Path) -> MemResult<(Vec<String>, bool, bool)> {
    match git(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"], LIST_BYTES_MAX).await {
        Ok(out) => {
            let text = out.text();
            let mut files: Vec<String> = text.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
            if out.truncated {
                files.pop();
            }
            Ok((files, out.truncated, true))
        }
        Err(err) if issue_of(&err) == GitIssue::NotARepository => {
            let base = root.to_path_buf();
            let (files, truncated) = tokio::task::spawn_blocking(move || walk(&base))
                .await
                .map_err(|err| MemError::store(format!("the directory walk failed: {err}")))?;
            Ok((files, truncated, false))
        }
        Err(err) => Err(err),
    }
}

/// The code index's lister: [list_files], for the gateway's service and the CLI's.
pub struct CheckoutLister;

#[async_trait::async_trait]
impl Lister for CheckoutLister {
    async fn list(&self, root: &Path) -> MemResult<Listing> {
        let (files, truncated, git) = list_files(root).await?;
        Ok(Listing { files, truncated, git })
    }
}

/// Every file under `root`, repo-relative, skipping WALK_SKIP; capped at WALK_MAX entries.
fn walk(root: &Path) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    let mut seen = 0usize;
    while let Some((dir, rel)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            seen += 1;
            if seen > WALK_MAX {
                return (out, true);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    if !WALK_SKIP.contains(&name.as_str()) {
                        stack.push((entry.path(), child));
                    }
                }
                Ok(t) if t.is_file() => out.push(child),
                _ => {}
            }
        }
    }
    (out, false)
}

/// What coverage needs from a checkout (SPEC §memory.domains): its files (git's tracked
/// and unignored ones, or the bounded walk outside Git) and the last 500 commits with the
/// paths each touched. Without Git the files stay and `reason` says why.
pub async fn coverage_read(root: &Path) -> Checkout {
    // The listing and the history are independent: read side by side.
    let (listed, log) = tokio::join!(
        git(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"], LIST_BYTES_MAX),
        git(root, &["log", COVERAGE_COMMITS, "--name-only", "--no-renames", NAME_LOG_FORMAT], LIST_BYTES_MAX),
    );
    let listed = match listed {
        Ok(out) => out,
        Err(err) => {
            let base = root.to_path_buf();
            let (files, truncated) = tokio::task::spawn_blocking(move || walk(&base)).await.unwrap_or_default();
            let issue = Some(issue_of(&err));
            return Checkout { files: Some(files), files_truncated: truncated, commits: None, issue, reason: Some(err.message) };
        }
    };
    let mut files: Vec<String> = listed.text().split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
    files.sort();
    files.dedup();
    let (commits, issue, reason) = match log {
        Ok(out) => (Some(parse_name_log(&out.text(), out.truncated)), None, None),
        // A repository without a commit yet has no drift to report.
        Err(err) if err.message.contains("does not have any commits") => (Some(Vec::new()), None, None),
        Err(err) => (None, Some(issue_of(&err)), Some(err.message)),
    };
    Checkout { files: Some(files), files_truncated: listed.truncated, commits, issue, reason }
}

/// `--name-only` under NAME_LOG_FORMAT: `\x1eHASH\x1fAT\x1fSUBJECT` then the paths. A log cut
/// at its cap loses its last, partial record.
fn parse_name_log(text: &str, truncated: bool) -> Vec<CommitFiles> {
    let mut commits: Vec<CommitFiles> = text
        .split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let mut head = lines.next()?.split('\x1f');
            let hash = head.next()?.trim().to_string();
            if hash.is_empty() {
                return None;
            }
            let at_ms = head.next()?.trim().parse::<i64>().ok()? * 1000;
            let subject = head.next().unwrap_or_default().to_string();
            let files = lines.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
            Some(CommitFiles { hash, at_ms, subject, files })
        })
        .collect();
    if truncated {
        commits.pop();
    }
    commits
}

// --- git -----------------------------------------------------------------------------------

/// A commit is named by 4 to 64 hex digits (SHA-1 or SHA-256), nothing else.
pub fn revision(raw: &str) -> MemResult<String> {
    let rev = raw.trim().to_ascii_lowercase();
    if (4..=64).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(rev)
    } else {
        Err(MemError::invalid("rev must be a commit hash (4 to 64 hex digits)"))
    }
}

/// The commits that touched `rel`, newest first (renames followed for a file), and
/// whether the working tree differs from HEAD there.
pub async fn history(root: &Path, rel: &str, limit: u64) -> MemResult<History> {
    let limit = limit.clamp(1, LOG_LIMIT_MAX);
    if !rel.is_empty() {
        inside(root, rel).or_else(|err| if err.code == Code::NotFound { Ok(PathBuf::new()) } else { Err(err) })?;
    }
    let count = format!("-n{}", limit + 1);
    let mut args = vec!["log", count.as_str(), LOG_FORMAT];
    if root.join(rel).is_file() {
        args.push("--follow");
    }
    args.extend(["--", pathspec(rel)]);
    let out = git(root, &args, META_BYTES_MAX).await?;
    let mut commits = parse_log(&out.text());
    let more = commits.len() as u64 > limit;
    commits.truncate(limit as usize);
    let status = git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", pathspec(rel)], META_BYTES_MAX)
        .await?
        .text();
    let untracked = !rel.is_empty() && status.split('\0').any(|line| line.starts_with("??"));
    let dirty = status.split('\0').any(|line| !line.is_empty());
    Ok(History { root: scope::display(root), path: rel.to_string(), commits, more, dirty, untracked })
}

/// One commit: its message, the files it changed and the patch (of `path` alone when
/// one is given), against its first parent for a merge.
pub async fn commit(root: &Path, rev: &str, path: Option<&str>) -> MemResult<CommitDetail> {
    let rev = revision(rev)?;
    if let Some(rel) = path {
        if sensitive(rel) {
            return Err(refused(rel));
        }
    }
    let object = format!("{rev}^{{commit}}");
    let meta = git(root, &["show", "-s", SHOW_FORMAT, &object], META_BYTES_MAX).await?.text();
    let mut fields = meta.splitn(7, '\x1f');
    let mut next = || fields.next().unwrap_or_default().trim_end_matches('\n').to_string();
    let hash = next();
    let short = next();
    let author = next();
    let date_ms = next().trim().parse::<i64>().map(|s| s * 1000).unwrap_or(0);
    let parents: Vec<String> = next().split_whitespace().map(str::to_string).collect();
    let subject = next();
    let body = next().trim().to_string();
    if hash.is_empty() {
        return Err(MemError::not_found(format!("no commit {rev} in this checkout")));
    }
    let diff = ["show", "--format=", "--no-renames", "--diff-merges=first-parent", "--no-ext-diff", "--no-textconv"];
    let mut status_args = diff.to_vec();
    status_args.extend(["--name-status", "-z", hash.as_str()]);
    let mut numstat_args = diff.to_vec();
    numstat_args.extend(["--numstat", "-z", hash.as_str()]);
    let mut patch_args = diff.to_vec();
    patch_args.extend(["--patch", hash.as_str()]);
    if let Some(rel) = path {
        patch_args.extend(["--", pathspec(rel)]);
    }
    let (statuses, numstat, patch) = tokio::try_join!(
        git(root, &status_args, META_BYTES_MAX),
        git(root, &numstat_args, META_BYTES_MAX),
        git(root, &patch_args, PATCH_BYTES_MAX),
    )?;
    let files = changed(&statuses.text(), &numstat.text());
    let (patch, truncated) = clip_patch(patch);
    Ok(CommitDetail {
        root: scope::display(root),
        commit: Commit { hash, short, author, date_ms, subject },
        body,
        parents,
        files,
        path: path.map(str::to_string),
        patch,
        truncated,
    })
}

/// What the working tree and the index change at `rel` against HEAD.
pub async fn working(root: &Path, rel: &str) -> MemResult<WorkingDiff> {
    if !rel.is_empty() {
        inside(root, rel).or_else(|err| if err.code == Code::NotFound { Ok(PathBuf::new()) } else { Err(err) })?;
    }
    let status = git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", pathspec(rel)], META_BYTES_MAX)
        .await?
        .text();
    let untracked = !rel.is_empty() && status.split('\0').any(|line| line.starts_with("??"));
    let out = git(root, &["diff", "HEAD", "--no-ext-diff", "--no-textconv", "--", pathspec(rel)], PATCH_BYTES_MAX).await?;
    let (patch, truncated) = clip_patch(out);
    Ok(WorkingDiff { root: scope::display(root), path: rel.to_string(), patch, truncated, untracked })
}

/// The whole checkout for "", the path otherwise (pathspecs are literal: see `git`).
fn pathspec(rel: &str) -> &str {
    if rel.is_empty() {
        "."
    } else {
        rel
    }
}

fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let mut f = record.split('\x1f');
            let hash = f.next()?.to_string();
            if hash.is_empty() {
                return None;
            }
            Some(Commit {
                hash,
                short: f.next()?.to_string(),
                author: f.next()?.to_string(),
                date_ms: f.next()?.trim().parse::<i64>().ok()? * 1000,
                subject: f.next()?.to_string(),
            })
        })
        .collect()
}

/// `--name-status -z` (`M\0path\0`) joined with `--numstat -z` (`add\tdel\tpath\0`;
/// `-` for a binary file).
fn changed(statuses: &str, numstat: &str) -> Vec<Changed> {
    let mut counts = std::collections::HashMap::new();
    for record in numstat.split('\0').filter(|r| !r.is_empty()) {
        let mut parts = record.trim_start_matches('\n').splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        counts.insert(path.to_string(), (add.parse::<u64>().ok(), del.parse::<u64>().ok()));
    }
    let mut out = Vec::new();
    let mut fields = statuses.split('\0').map(|s| s.trim_start_matches('\n'));
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if status.is_empty() {
            break;
        }
        let (additions, deletions) = counts.get(path).copied().unwrap_or((None, None));
        out.push(Changed { path: path.to_string(), status: status.chars().take(1).collect(), additions, deletions });
    }
    out
}

/// A patch cut at its cap ends at a whole line.
fn clip_patch(out: GitOut) -> (String, bool) {
    if !out.truncated {
        return (out.text(), false);
    }
    let end = out.bytes.iter().rposition(|b| *b == b'\n').map(|at| at + 1).unwrap_or(out.bytes.len());
    (String::from_utf8_lossy(&out.bytes[..end]).into_owned(), true)
}

pub(crate) struct GitOut {
    pub(crate) bytes: Vec<u8>,
    /// Stopped at the cap (the child was cut off).
    pub(crate) truncated: bool,
}

impl GitOut {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Run git in `root`, reading at most `cap` bytes of its output.
pub(crate) async fn git(root: &Path, args: &[&str], cap: usize) -> MemResult<GitOut> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["--no-pager", "-c", "core.quotepath=off", "-c", "color.ui=false", "-c", "core.fsmonitor=false"])
        .args(["-c", "i18n.logOutputEncoding=UTF-8"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: the daemon has no console, and without it every git call
        // flashes one (tokio's Command exposes creation_flags directly on Windows).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => unavailable(GitIssue::GitMissing, "git is not installed or not on PATH"),
        _ => unavailable(GitIssue::GitFailed, format!("git could not start: {err}")),
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read_out = async {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout {
            // Dropping the pipe at the cap ends a child still writing.
            pipe.take(cap as u64 + 1).read_to_end(&mut bytes).await?;
        }
        Ok::<_, std::io::Error>(bytes)
    };
    let read_err = async {
        let mut bytes = Vec::new();
        if let Some(pipe) = stderr {
            pipe.take(META_BYTES_MAX as u64).read_to_end(&mut bytes).await?;
        }
        Ok::<_, std::io::Error>(bytes)
    };
    let run = async {
        let (out, err) = tokio::join!(read_out, read_err);
        (out, err, child.wait().await)
    };
    let Ok((out, err, status)) = tokio::time::timeout(GIT_TIMEOUT, run).await else {
        return Err(unavailable(GitIssue::GitFailed, format!("git took longer than {} s", GIT_TIMEOUT.as_secs())));
    };
    let mut bytes = out.map_err(|err| unavailable(GitIssue::GitFailed, format!("reading git's output failed: {err}")))?;
    if bytes.len() > cap {
        bytes.truncate(cap);
        return Ok(GitOut { bytes, truncated: true });
    }
    let status = status.map_err(|err| unavailable(GitIssue::GitFailed, format!("git did not finish: {err}")))?;
    if !status.success() {
        return Err(git_failure(&String::from_utf8_lossy(&err.unwrap_or_default())));
    }
    Ok(GitOut { bytes, truncated: false })
}

/// git's stderr as one of our errors (LC_ALL=C keeps it English).
fn git_failure(stderr: &str) -> MemError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("not a git repository") {
        return unavailable(GitIssue::NotARepository, "the project's directory is not a git checkout");
    }
    if lower.contains("dubious ownership") {
        return unavailable(GitIssue::Refused, "git refuses the project's directory: another user owns it (see git's safe.directory)");
    }
    if ["unknown revision", "bad object", "bad revision", "not a valid object name", "invalid object name"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return MemError::not_found("no such commit in this checkout");
    }
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git failed");
    unavailable(GitIssue::GitFailed, line.chars().take(200).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-preview-{label}-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir.canonicalize().expect("canonical scratch")
    }

    fn sh(root: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn relative_paths_stay_inside() {
        assert_eq!(relative("./src\\main.rs").expect("ok"), "src/main.rs");
        assert_eq!(relative("").expect("root"), "");
        for bad in ["/etc/passwd", "../x", "src/../../x", "C:/Windows/win.ini", "a\u{7}b"] {
            assert!(relative(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn credential_names_are_refused() {
        for name in [".env", "app/.env.local", "id_rsa", "keys/id_ed25519.pub", "tls/server.key", "c.pem", ".git/config", "x/.ssh/known_hosts", "gateway.config.json", ".npmrc"] {
            assert!(sensitive(name), "{name}");
        }
        for name in [".env.example", "src/env.rs", "keyboard.rs", ".gitignore", ".github/workflows/ci.yml", "docs/SPEC.md"] {
            assert!(!sensitive(name), "{name}");
        }
    }

    #[test]
    fn a_file_is_read_inside_the_root_and_capped() {
        let root = scratch("file");
        std::fs::create_dir_all(root.join("src")).expect("dir");
        std::fs::write(root.join("src/a.rs"), "\u{feff}fn main() {}\n// two\n").expect("write");
        std::fs::write(root.join(".env"), "TOKEN=x").expect("write");
        std::fs::write(root.join("blob.bin"), [1u8, 0, 2]).expect("write");
        let preview = read(&root, "src/a.rs").expect("read");
        let Body::File { text, lines, binary, truncated, .. } = preview.body else { panic!("a file") };
        assert_eq!((text.as_deref(), lines, binary, truncated), (Some("fn main() {}\n// two\n"), 2, false, false));
        assert!(matches!(read(&root, "blob.bin").expect("bin").body, Body::File { binary: true, text: None, .. }));
        assert_eq!(read(&root, ".env").expect_err("refused").code, Code::SensitiveContent);
        assert_eq!(read(&root, "nope.rs").expect_err("missing").code, Code::NotFound);
        let Body::Dir { entries, .. } = read(&root, "").expect("root").body else { panic!("a dir") };
        let names: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.sensitive)).collect();
        assert_eq!(names, [("src", false), (".env", true), ("blob.bin", false)]);

        let (text, cut) = clip_text(b"a\nb\nc\n", 1024, 2);
        assert_eq!((text.as_str(), cut), ("a\nb\n", true));
        let (text, cut) = clip_text(b"aaaa\nbbbb\n", 7, 100);
        assert_eq!((text.as_str(), cut), ("aaaa\n", true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn revisions_are_hex_only() {
        assert_eq!(revision(" ABCdef12 ").expect("hex"), "abcdef12");
        for bad in ["abc", "HEAD", "--output=x", "abcd~1", "main"] {
            assert!(revision(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn name_status_and_numstat_join() {
        let files = changed("M\0src/a.rs\0A\0img.png\0", "3\t1\tsrc/a.rs\0-\t-\timg.png\0");
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].status.as_str(), files[0].additions, files[0].deletions), ("M", Some(3), Some(1)));
        assert_eq!((files[1].path.as_str(), files[1].additions), ("img.png", None));
        let log = parse_log("abc\x1fab\x1fAda\x1f1700000000\x1ffix: a | b\x1e\ndef\x1fde\x1fBo\x1f1600000000\x1finit\x1e\n");
        assert_eq!(log.len(), 2);
        assert_eq!((log[0].subject.as_str(), log[0].date_ms), ("fix: a | b", 1_700_000_000_000));
        let named = "\x1eabc\x1f1700000000\x1fsecond\n\nsrc/a.rs\ndocs/b.md\n\x1emrg\x1f1650000000\x1fmerge\n\x1edef\x1f1600000000\x1finit\n\nsrc/a.rs\n";
        let commits = parse_name_log(named, false);
        assert_eq!(commits.len(), 3);
        assert_eq!((commits[0].files.as_slice(), commits[0].at_ms), (["src/a.rs".to_string(), "docs/b.md".to_string()].as_slice(), 1_700_000_000_000));
        assert!(commits[1].files.is_empty(), "a merge lists no paths");
        assert_eq!(parse_name_log(named, true).len(), 2, "a cut log drops its partial record");
    }

    #[tokio::test]
    async fn git_history_commit_and_working_diff() {
        let root = scratch("git");
        if !sh(&root, &["init", "-q"]) {
            eprintln!("git is not available; skipped");
            return;
        }
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        assert!(sh(&root, &["add", "a.txt"]) && sh(&root, &["commit", "-q", "-m", "first\n\nthe body"]));
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        std::fs::write(root.join("b.rs"), "fn b() {}\n").expect("write");
        assert!(sh(&root, &["add", "-A"]) && sh(&root, &["commit", "-q", "-m", "second"]));
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").expect("write");

        let log = history(&root, "a.txt", 1).await.expect("log");
        assert_eq!(log.commits.len(), 1);
        assert_eq!(log.commits[0].subject, "second");
        assert_eq!(log.commits[0].author, "Ada");
        assert!(log.more && log.dirty && !log.untracked);

        let first = history(&root, "a.txt", 10).await.expect("log").commits[1].clone();
        let detail = commit(&root, &first.short, None).await.expect("commit");
        assert_eq!((detail.commit.subject.as_str(), detail.body.as_str()), ("first", "the body"));
        assert_eq!(detail.files.len(), 1);
        assert!(detail.patch.contains("+one"), "{}", detail.patch);

        let second = commit(&root, &log.commits[0].hash, Some("b.rs")).await.expect("commit");
        assert_eq!(second.files.len(), 2);
        assert!(second.patch.contains("+fn b()") && !second.patch.contains("+two"), "{}", second.patch);

        let diff = working(&root, "a.txt").await.expect("diff");
        assert!(diff.patch.contains("+three") && !diff.truncated);

        let globbed = matching(&root, "*.rs").await.expect("glob");
        assert_eq!((globbed.files, globbed.git), (vec!["b.rs".to_string()], true));

        assert_eq!(commit(&root, "deadbeef", None).await.expect_err("unknown").code, Code::NotFound);
        let plain = scratch("plain");
        assert_eq!(history(&plain, "", 5).await.expect_err("no git").code, Code::GitUnavailable);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&plain);
    }
}
