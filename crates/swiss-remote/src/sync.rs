//! The file-movement half of remote execution (docs/32 §20): one-way sync (local
//! directory -> workspace on the target) and pull (one remote file -> local).
//!
//! Sync is UPLOAD-ONLY and NEVER DELETES (docs/32 §20): a sync that removes files
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

/// The excludes every sync starts from (docs/32 §20), applied by path prefix.
pub const DEFAULT_EXCLUDES: [&str; 4] = [".git/", ".swiss/", "target/", "node_modules/"];

/// One sync's knobs, with the wire's defaults already applied.
pub struct SyncOptions {
    /// Local root to walk ("." for the binding's workspace).
    pub source: PathBuf,
    /// Extra excludes on top of [DEFAULT_EXCLUDES], as slash paths with trailing
    /// slash for directories (same grammar as the project binding).
    pub extra_excludes: Vec<String>,
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
    let mut files = Vec::new();
    walk(&opts.source, "", 0, &excludes, &mut files)?;
    let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, target.id);
    let mut report = SyncReport {
        scanned: files.len(),
        ..Default::default()
    };
    let mut made_dirs: Vec<String> = Vec::new();
    for rel in &files {
        if cancel.is_cancelled() {
            return Err("the sync was canceled".into());
        }
        let local = opts
            .source
            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let meta = match std::fs::metadata(&local) {
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
        let bytes = std::fs::read(&local).map_err(|err| format!("read {rel}: {err}"))?;
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
    let holder = format!("{}:{}", crate::actions::REMOTE_OWNER, target.id);
    let mut reader = registry
        .open_read(&target.endpoint, &holder, &remote_path)
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
    emit(&format!(
        "pulled {rel} ({total} bytes) -> {}\n",
        to.display()
    ));
    Ok(total)
}
