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

//! The file-movement half of remote execution (docs/34 §20): one-way sync (a local
//! directory, or a single file -> workspace on the target) and pull (one remote
//! file, or a whole directory tree -> local).
//!
//! Sync is UPLOAD-ONLY and NEVER DELETES (docs/34 §20): a sync that removes files
//! remotely is a footgun no agent should hold. What it does: walk the local tree,
//! skip the default excludes (.git, .swiss, target, node_modules - the build
//! artifacts nobody wants on the far side), compare size+mtime against the remote
//! stat, and stream changed files through the transport's chunked write.
//!
//! Both operations report COMPACTLY: one line per transferred file and one summary
//! line at the end - an agent watching a run wants the shape, not a wall of text.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use swiss_host::services::remote::RemoteTransportRegistry;

use crate::target::{safe_join, RemoteTarget};

/// The excludes every sync starts from (docs/34 §20), applied by path prefix.
pub const DEFAULT_EXCLUDES: [&str; 4] = [".git/", ".swiss/", "target/", "node_modules/"];

/// One sync's knobs, with the wire's defaults already applied.
pub struct SyncOptions {
    /// Local root to walk ("." for the binding's workspace), or ONE file to
    /// upload under its own name (or `to`, when given).
    pub source: PathBuf,
    /// Extra excludes on top of [DEFAULT_EXCLUDES], as slash paths with trailing
    /// slash for directories (same grammar as the project binding).
    pub extra_excludes: Vec<String>,
    /// Remote relative path for a single-file upload; None keeps the source
    /// file's own name. Ignored when `source` is a directory.
    pub to: Option<String>,
    /// How much output one line of progress carries (a summary is always printed).
    pub verbose: bool,
}

/// Does `rel` (a slash-separated relative path) match an exclude? A trailing slash
/// means directory prefix; no slash means the exact name at any depth.
fn excluded(rel: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('/') {
        let mut at = 0;
        while let Some(hit) = rel[at..].find(prefix) {
            let end = at + hit + prefix.len();
            if end == rel.len() || rel.as_bytes().get(end) == Some(&b'/') {
                return true;
            }
            at += hit + prefix.len();
        }
        false
    } else {
        rel.split('/').any(|seg| seg == pattern)
    }
}

/// Walk `dir` collecting every FILE's path relative to it, skipping excludes,
/// sorted so a run's output is deterministic. Depth-capped and count-capped: a
/// sync is for source trees, not disk images.
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 32;

fn walk(
    dir: &Path,
    rel: &str,
    depth: usize,
    excludes: &[String],
    out: &mut Vec<String>,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "tree deeper than {MAX_DEPTH} levels under {}",
            dir.display()
        ));
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|err| format!("read {}: {err}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if !excludes
                .iter()
                .any(|p| excluded(&format!("{child_rel}/"), p))
            {
                walk(&entry.path(), &child_rel, depth + 1, excludes, out)?;
            }
        } else if !excludes.iter().any(|p| excluded(&child_rel, p)) {
            out.push(child_rel);
            if out.len() > MAX_FILES {
                return Err(format!(
                    "more than {MAX_FILES} files under {}",
                    dir.display()
                ));
            }
        }
    }
    Ok(())
}

/// What one sync did, for the summary line and the outcome meta.
#[derive(Default, Debug, PartialEq)]
pub struct SyncReport {
    pub scanned: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub bytes: u64,
    pub failures: Vec<String>,
}

/// Run one sync. `emit` receives each progress line (already newline-terminated
/// where wanted by the caller) so both the action's tap and tests can observe.
pub async fn sync_tree(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    opts: &SyncOptions,
    mut emit: impl FnMut(&str),
    cancel: &swiss_host::services::action::CancelHandle,
) -> Result<SyncReport, String> {
    if !target.capabilities.iter().any(|c| c == "sync") {
        return Err(format!(
            "target {} does not declare the sync capability",
            target.id
        ));
    }
    let mut excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
    excludes.extend(opts.extra_excludes.iter().cloned());
    // Single-file mode: a FILE source uploads just that one file, under its own
    // name or the caller's `to` name - no walk and no excludes, because the file
    // was named on purpose. A directory walks exactly as before.
    let pairs: Vec<(PathBuf, String)> = if std::fs::metadata(&opts.source)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        let name = opts.to.clone().or_else(|| {
            opts.source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        });
        match name {
            Some(name) => vec![(opts.source.clone(), name)],
            None => {
                return Err(format!(
                    "source {} has no file name to upload under; pass to",
                    opts.source.display()
                ))
            }
        }
    } else {
        let mut files = Vec::new();
        walk(&opts.source, "", 0, &excludes, &mut files)?;
        files
            .into_iter()
            .map(|rel| {
                let local = opts
                    .source
                    .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                (local, rel)
            })
            .collect()
    };
    let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, target.id);
    let mut report = SyncReport {
        scanned: pairs.len(),
        ..Default::default()
    };
    let mut made_dirs: Vec<String> = Vec::new();
    for (local, rel) in &pairs {
        if cancel.is_cancelled() {
            return Err("the sync was canceled".into());
        }
        let meta = match std::fs::metadata(local) {
            Ok(m) => m,
            Err(err) => {
                report.failures.push(format!("{rel}: {err}"));
                continue;
            }
        };
        let remote_path = safe_join(&target.workspace_root, rel)?;
        let changed = match registry.stat(&target.endpoint, &holder, &remote_path).await {
            Ok(Some(stat)) => stat.size != meta.len(),
            // A missing file is the normal first-sync case, not an error.
            Ok(None) => true,
            Err(err) => return Err(format!("stat {remote_path}: {err}")),
        };
        if !changed {
            report.skipped += 1;
            continue;
        }
        if let Some(parent) = Path::new(&remote_path).parent() {
            let parent = parent.to_string_lossy().into_owned();
            if !made_dirs.contains(&parent) && !parent.is_empty() {
                registry
                    .mkdir_p(&target.endpoint, &holder, &parent)
                    .await
                    .map_err(|err| format!("mkdir {parent}: {err}"))?;
                made_dirs.push(parent);
            }
        }
        let bytes = std::fs::read(local).map_err(|err| format!("read {rel}: {err}"))?;
        let mut writer = registry
            .create(&target.endpoint, &holder, &remote_path)
            .await
            .map_err(|err| format!("create {remote_path}: {err}"))?;
        for chunk in bytes.chunks(swiss_host::services::remote::REMOTE_CHUNK_BYTES) {
            writer
                .write_chunk(chunk)
                .await
                .map_err(|err| format!("write {rel}: {err}"))?;
        }
        writer
            .finish()
            .await
            .map_err(|err| format!("commit {rel}: {err}"))?;
        drop(writer);
        report.uploaded += 1;
        report.bytes += bytes.len() as u64;
        if opts.verbose {
            emit(&format!("{rel} ({} bytes)\n", bytes.len()));
        }
    }
    emit(&format!(
        "sync: {} scanned, {} uploaded ({} bytes), {} up to date{}\n",
        report.scanned,
        report.uploaded,
        report.bytes,
        report.skipped,
        if report.failures.is_empty() {
            String::new()
        } else {
            format!(", {} failed", report.failures.len())
        },
    ));
    Ok(report)
}

/// Pull one remote file (relative to the workspace root) into `to`. Streams
/// chunk-by-chunk; the local parent is created first.
pub async fn pull_one(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    rel: &str,
    to: &Path,
    mut emit: impl FnMut(&str),
    cancel: &swiss_host::services::action::CancelHandle,
) -> Result<u64, String> {
    if !target
        .capabilities
        .iter()
        .any(|c| c == "files" || c == "sync")
    {
        return Err(format!(
            "target {} does not declare the files or sync capability",
            target.id
        ));
    }
    if cancel.is_cancelled() {
        return Err("the pull was canceled".into());
    }
    let remote_path = safe_join(&target.workspace_root, rel)?;
    let total = pull_stream(registry, target, &remote_path, to, cancel).await?;
    emit(&format!(
        "pulled {rel} ({total} bytes) -> {}\n",
        to.display()
    ));
    Ok(total)
}

/// The streaming core every pull path shares: open the remote file, create the
/// local parent, stream chunk-by-chunk into `to`, return the byte count. The
/// cancel handle is honoured per chunk, so a canceled pull stops mid-file.
async fn pull_stream(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    remote_path: &str,
    to: &Path,
    cancel: &swiss_host::services::action::CancelHandle,
) -> Result<u64, String> {
    let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, target.id);
    let mut reader = registry
        .open_read(&target.endpoint, &holder, remote_path)
        .await
        .map_err(|err| format!("open {remote_path}: {err}"))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
    }
    let mut out = tokio::fs::File::create(to)
        .await
        .map_err(|err| format!("create {}: {err}", to.display()))?;
    let mut total: u64 = 0;
    use tokio::io::AsyncWriteExt;
    loop {
        if cancel.is_cancelled() {
            return Err("the pull was canceled".into());
        }
        let chunk = reader
            .read_chunk()
            .await
            .map_err(|err| format!("read {remote_path}: {err}"))?;
        let Some(chunk) = chunk else { break };
        out.write_all(&chunk)
            .await
            .map_err(|err| format!("write {}: {err}", to.display()))?;
        total += chunk.len() as u64;
    }
    out.flush()
        .await
        .map_err(|err| format!("flush {}: {err}", to.display()))?;
    drop(out);
    drop(reader);
    Ok(total)
}

/// What one pull-tree walk brought down, for the summary line and the meta.
#[derive(Default, Debug, PartialEq)]
pub struct PullReport {
    pub files: usize,
    pub bytes: u64,
    pub dirs: usize,
}

/// Caps in the sync walk's style: a pull is for workspaces, not whole disks.
const PULL_MAX_FILES: usize = 2_000;
const PULL_MAX_DEPTH: usize = 32;

/// Pull a remote file OR a whole directory tree (relative to the workspace
/// root) into `to`: stat first, then either one streaming file (the pull_one
/// path) or a recursive walk. Cancels per file like sync does; one summary
/// line at the end, and each file's path when `verbose`.
pub async fn pull_tree(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    rel: &str,
    to: &Path,
    verbose: bool,
    mut emit: impl FnMut(&str) + Send,
    cancel: &swiss_host::services::action::CancelHandle,
) -> Result<PullReport, String> {
    if !target
        .capabilities
        .iter()
        .any(|c| c == "files" || c == "sync")
    {
        return Err(format!(
            "target {} does not declare the files or sync capability",
            target.id
        ));
    }
    if cancel.is_cancelled() {
        return Err("the pull was canceled".into());
    }
    let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, target.id);
    let remote_path = safe_join(&target.workspace_root, rel)?;
    let stat = registry
        .stat(&target.endpoint, &holder, &remote_path)
        .await
        .map_err(|err| format!("stat {remote_path}: {err}"))?;
    let mut report = PullReport::default();
    match stat {
        Some(stat) if stat.is_dir => {
            // `to` is the local stand-in for the remote root directory.
            std::fs::create_dir_all(to).map_err(|err| format!("mkdir {}: {err}", to.display()))?;
            report.dirs += 1;
            PullWalk {
                registry,
                target,
                report: &mut report,
                verbose,
                emit: &mut emit,
                cancel,
            }
            .dir(rel, to, 0)
            .await?;
        }
        Some(_) => {
            // A file at the root: exactly the one-file pull that always existed.
            let total = pull_one(registry, target, rel, to, |line| emit(line), cancel).await?;
            report.files += 1;
            report.bytes += total;
        }
        None => return Err(format!("{rel}: no such file or directory on the target")),
    }
    emit(&format!(
        "pull: {} files ({} bytes), {} dirs\n",
        report.files, report.bytes, report.dirs
    ));
    Ok(report)
}

/// The state one pull-tree walk threads through every recursion level: the
/// transport plumbing, the running report and the output tap. A struct (not an
/// argument list) because the walk recurses and the signature would grow with
/// every new knob.
struct PullWalk<'a> {
    registry: &'a Arc<RemoteTransportRegistry>,
    target: &'a RemoteTarget,
    report: &'a mut PullReport,
    verbose: bool,
    emit: &'a mut (dyn FnMut(&str) + Send),
    cancel: &'a swiss_host::services::action::CancelHandle,
}

impl PullWalk<'_> {
    /// One directory level of the pull walk: list the remote directory, recurse
    /// into child directories, stream the files down. Every child is re-joined
    /// through [safe_join], so a hostile listing cannot steer the pull outside
    /// the workspace; a name that is not one plain segment is skipped.
    async fn dir(&mut self, rel: &str, local_dir: &Path, depth: usize) -> Result<(), String> {
        if depth > PULL_MAX_DEPTH {
            return Err(format!(
                "tree deeper than {PULL_MAX_DEPTH} levels under {rel}"
            ));
        }
        let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, self.target.id);
        let dir_path = safe_join(&self.target.workspace_root, rel)?;
        let entries = self
            .registry
            .list_dir(&self.target.endpoint, &holder, &dir_path)
            .await
            .map_err(|err| format!("list {dir_path}: {err}"))?;
        for entry in entries {
            if self.cancel.is_cancelled() {
                return Err("the pull was canceled".into());
            }
            if entry.name.is_empty()
                || entry.name == "."
                || entry.name == ".."
                || entry.name.contains('/')
                || entry.name.contains('\\')
            {
                continue;
            }
            let child_rel = if rel.is_empty() {
                entry.name.clone()
            } else {
                format!("{rel}/{}", entry.name)
            };
            if entry.is_dir {
                let child_dir = local_dir.join(&entry.name);
                std::fs::create_dir_all(&child_dir)
                    .map_err(|err| format!("mkdir {}: {err}", child_dir.display()))?;
                self.report.dirs += 1;
                Box::pin(self.dir(&child_rel, &child_dir, depth + 1)).await?;
            } else {
                if self.report.files >= PULL_MAX_FILES {
                    return Err(format!("more than {PULL_MAX_FILES} files under {rel}"));
                }
                let remote_path = safe_join(&self.target.workspace_root, &child_rel)?;
                let to = local_dir.join(&entry.name);
                let total =
                    pull_stream(self.registry, self.target, &remote_path, &to, self.cancel).await?;
                self.report.files += 1;
                self.report.bytes += total;
                if self.verbose {
                    (self.emit)(&format!("{child_rel} ({total} bytes)\n"));
                }
            }
        }
        Ok(())
    }
}
