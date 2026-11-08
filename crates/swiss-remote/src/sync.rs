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

//! The file-movement half of remote execution (SPEC §remote.actions): one-way sync (a local
//! directory, or a single file -> workspace on the target) and pull (one remote
//! file, or a whole directory tree -> local).
//!
//! Sync is UPLOAD-ONLY and NEVER DELETES (SPEC §remote.actions): a sync that removes files
//! remotely is a footgun no agent should hold. What it does: walk the local tree,
//! skip the default excludes (.git, .swiss, target, node_modules - the build
//! artifacts nobody wants on the far side), compare size AND whole-second mtime against
//! the remote stat (rsync's quick check; a stat without an mtime falls back to size
//! only), and stream changed files through the transport's chunked write. An upload
//! stamps the remote file with the local mtime so the next quick check passes; a
//! same-size file whose mtime differs is content-checked in batches (one sha256sum
//! exec per 200 paths) before it is sent, so files uploaded before this rule carry
//! their bytes over untouched and only get their mtime fixed.
//! A changed file of 1 MiB or more whose previous version is already on the target goes up as
//! a delta against it ([crate::delta]); the whole file goes up whenever that is not possible.
//!
//! Both operations report COMPACTLY: one line per transferred file and one summary
//! line at the end - an agent watching a run wants the shape, not a wall of text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use swiss_host::services::remote::{RemoteFileStat, RemoteTransportRegistry};

use crate::delta::{self, DeltaError, DELTA_MIN_BYTES};
use crate::target::{safe_join, RemoteTarget};

/// The excludes every sync starts from (SPEC §remote.actions), applied by path prefix.
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
    /// Send big changed files as a delta when the target holds a previous version.
    pub delta: bool,
    /// Single-file only: the remote file (relative to the workspace, or absolute) to
    /// take the delta against when the destination does not exist yet. None guesses a
    /// sibling with the same extension and a similar size, newest first.
    pub base: Option<String>,
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
    /// Files that went up as a delta.
    pub delta: usize,
    /// Bytes that crossed the link: whole files plus delta signatures and patches.
    pub sent: u64,
    pub failures: Vec<String>,
}

/// The local mtime floored to whole seconds - the resolution SFTP carries.
fn mtime_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// One file's quick-check verdict: current when the size AND the whole-second
/// mtime both match (SPEC §remote.actions). Equality only, never newer or older,
/// so two machines whose clocks disagree cannot loop re-uploads; a stat that
/// reports no mtime (or a local one that cannot be read) keeps the size-only
/// rule these comparisons used to be.
enum Freshness {
    /// Missing, or a different size: send it.
    Upload,
    /// Size and mtime both match: nothing to do.
    UpToDate,
    /// Same size, different mtime: content-check it before sending anything -
    /// the migration case, because a file uploaded before this rule carries the
    /// upload time as its remote mtime.
    VerifyContent,
}

fn freshness(existing: RemoteFileStat, len: u64, local_secs: Option<u64>) -> Freshness {
    if existing.size != len {
        return Freshness::Upload;
    }
    match (existing.mtime_ms, local_secs) {
        (Some(remote_ms), Some(local)) if remote_ms / 1000 != local => Freshness::VerifyContent,
        _ => Freshness::UpToDate,
    }
}

/// One same-size file whose mtime differs, held back for the batch content check.
struct PendingVerify {
    local: PathBuf,
    rel: String,
    remote_path: String,
    stat: RemoteFileStat,
    len: u64,
    local_secs: u64,
}

/// How many paths one hashing exec carries: a relayed link pays ~100 ms per
/// round trip, so same-size files are settled in batches, never one by one.
const HASH_BATCH: usize = 200;
/// The output ceiling for one hashing batch: 64 hex digits, a separator and a
/// path per line, a few hundred paths per batch.
const HASH_OUT_CAP: usize = 8 * 1024 * 1024;

/// Everything one uploaded file needs, shared by the fresh path and the
/// content-verified path so a file worth sending goes up exactly one way.
struct Uploader<'a> {
    registry: &'a Arc<RemoteTransportRegistry>,
    target: &'a RemoteTarget,
    holder: String,
    verbose: bool,
    single: bool,
    base_hint: Option<String>,
    emit: &'a mut (dyn FnMut(&str) + Send),
    cancel: &'a swiss_host::services::action::CancelHandle,
    made_dirs: Vec<String>,
    /// Cleared once the target turns out to have no python3: no retry per file.
    delta_ok: bool,
    /// The set-mtime failure note is printed once per run, not once per file.
    mtime_note: bool,
    report: &'a mut SyncReport,
}

impl Uploader<'_> {
    /// The remote parent of one path, created once per run.
    async fn mkdir_for(&mut self, remote_path: &str) -> Result<(), String> {
        if let Some(parent) = Path::new(remote_path).parent() {
            let parent = parent.to_string_lossy().into_owned();
            if !self.made_dirs.contains(&parent) && !parent.is_empty() {
                self.registry
                    .mkdir_p(&self.target.endpoint, &self.holder, &parent)
                    .await
                    .map_err(|err| format!("mkdir {parent}: {err}"))?;
                self.made_dirs.push(parent);
            }
        }
        Ok(())
    }

    /// Send one file that is worth sending - as a delta when that is possible
    /// and worth it, else whole - and stamp the remote mtime. `existing` is the
    /// stat the quick check saw; a delta bases on the destination itself.
    async fn push(
        &mut self,
        local: &Path,
        rel: &str,
        remote_path: &str,
        existing: Option<RemoteFileStat>,
        local_secs: Option<u64>,
        len: u64,
    ) -> Result<(), String> {
        self.mkdir_for(remote_path).await?;
        if self.delta_ok && len >= DELTA_MIN_BYTES {
            let base = match existing {
                Some(stat) if !stat.is_dir => Some(remote_path.to_string()),
                Some(_) => None,
                None if self.single => match &self.base_hint {
                    Some(base) => Some(safe_join(&self.target.workspace_root, base)?),
                    None => {
                        guess_base(self.registry, self.target, &self.holder, remote_path, len)
                            .await
                    }
                },
                None => None,
            };
            if let Some(base) = base {
                match delta::upload_delta(
                    self.registry,
                    self.target,
                    &self.holder,
                    local,
                    remote_path,
                    &base,
                    self.cancel,
                )
                .await
                {
                    Ok(done) => {
                        self.report.uploaded += 1;
                        self.report.delta += 1;
                        self.report.bytes += done.stats.len;
                        self.report.sent += done.wire_bytes;
                        if self.verbose {
                            (self.emit)(&format!(
                                "{rel} ({} bytes, delta against {}: {} bytes sent)\n",
                                done.stats.len,
                                shown(&self.target.workspace_root, &base),
                                done.wire_bytes
                            ));
                        }
                        return Ok(()); // the script stamped the mtime itself
                    }
                    Err(DeltaError::Canceled) => return Err("the sync was canceled".into()),
                    Err(DeltaError::NoPython) => {
                        self.delta_ok = false;
                        (self.emit)(
                            "python3 is not on the target, so no delta: whole files go up\n",
                        );
                    }
                    Err(DeltaError::Fallback(reason)) => {
                        (self.emit)(&format!(
                            "{rel}: no delta ({reason}); the whole file goes up\n"
                        ));
                    }
                }
            }
        }
        let sent = upload_one(
            self.registry,
            self.target,
            &self.holder,
            local,
            rel,
            remote_path,
            self.cancel,
        )
        .await?;
        self.report.uploaded += 1;
        self.report.bytes += sent;
        self.report.sent += sent;
        if self.verbose {
            (self.emit)(&format!("{rel} ({sent} bytes)\n"));
        }
        self.stamp_mtime(remote_path, local_secs).await;
        Ok(())
    }

    /// Best-effort remote mtime stamp so the next quick check passes. A refusal
    /// or failure is noted once per run and the file still counts as uploaded -
    /// the bytes are there; only the bookkeeping suffers (the next run
    /// content-checks the file instead of skipping it).
    async fn stamp_mtime(&mut self, remote_path: &str, local_secs: Option<u64>) {
        let Some(secs) = local_secs else {
            return;
        };
        if let Err(err) = self
            .registry
            .set_mtime(&self.target.endpoint, &self.holder, remote_path, secs)
            .await
        {
            self.note_mtime_failure(&err);
        }
    }

    /// A file whose content already matches: only its mtime is fixed, and it
    /// counts as up to date. Even a failed stamp keeps that verdict - the bytes
    /// are current; the next run just re-checks them.
    async fn set_mtime_only(&mut self, remote_path: &str, rel: &str, local_secs: u64) {
        match self
            .registry
            .set_mtime(&self.target.endpoint, &self.holder, remote_path, local_secs)
            .await
        {
            Ok(()) => {
                if self.verbose {
                    (self.emit)(&format!(
                        "{rel} (already current; the remote mtime now matches)\n"
                    ));
                }
            }
            Err(err) => self.note_mtime_failure(&err),
        }
        self.report.skipped += 1;
    }

    fn note_mtime_failure(&mut self, err: &swiss_host::services::remote::RemoteError) {
        if !self.mtime_note {
            self.mtime_note = true;
            (self.emit)(&format!(
                "the remote mtime could not be set ({err}); the next sync re-checks these files\n"
            ));
        }
    }
}

/// The migration half: files uploaded before the mtime rule carry the upload
/// time as their remote mtime, so a mismatching mtime alone must not re-send
/// them. One sha256sum exec covers a batch of paths (argv, never a shell
/// string); matching content keeps its bytes and only gets the mtime fixed, and
/// a target that cannot hash sends the whole batch up with one note.
async fn verify_batches(
    uploader: &mut Uploader<'_>,
    pending: Vec<PendingVerify>,
) -> Result<(), String> {
    let mut no_hash = false;
    for batch in pending.chunks(HASH_BATCH) {
        if uploader.cancel.is_cancelled() {
            return Err("the sync was canceled".into());
        }
        let paths: Vec<String> = batch.iter().map(|p| p.remote_path.clone()).collect();
        let remote = match remote_hashes(
            uploader.registry,
            uploader.target,
            &uploader.holder,
            &paths,
            uploader.cancel,
        )
        .await
        {
            Some(hashes) => hashes,
            None => {
                if !no_hash {
                    no_hash = true;
                    (uploader.emit)(
                        "the target has no sha256sum, so same-size files go up whole\n",
                    );
                }
                HashMap::new()
            }
        };
        // The local digests are file reads: keep them off the async worker.
        let jobs: Vec<PathBuf> = batch.iter().map(|p| p.local.clone()).collect();
        let want = batch.len();
        let local = tokio::task::spawn_blocking(move || {
            jobs.iter().map(|path| local_sha256(path)).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|_| vec![None; want]);
        for (pending, digest) in batch.iter().zip(local) {
            let same = digest.is_some_and(|want| remote.get(&pending.remote_path) == Some(&want));
            if same {
                uploader
                    .set_mtime_only(&pending.remote_path, &pending.rel, pending.local_secs)
                    .await;
            } else {
                uploader
                    .push(
                        &pending.local,
                        &pending.rel,
                        &pending.remote_path,
                        Some(pending.stat),
                        Some(pending.local_secs),
                        pending.len,
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// sha256 of every path in one batch, computed on the target by ONE exec.
/// None when the target cannot hash at all; a path the command could not read
/// is simply missing from the map, which the caller reads as "changed".
async fn remote_hashes(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    paths: &[String],
    cancel: &swiss_host::services::action::CancelHandle,
) -> Option<HashMap<String, [u8; 32]>> {
    let mut argv = vec!["sha256sum".to_string(), "--".to_string()];
    argv.extend(paths.iter().cloned());
    match crate::delta::capture_exec(registry, target, holder, argv, None, HASH_OUT_CAP, cancel)
        .await
    {
        // 127 is the shell's "command not found": no hashing on that target.
        Ok((127, _, _)) | Err(_) => None,
        Ok((_, stdout, _)) => Some(parse_sha256sum(&stdout)),
    }
}

/// Parse sha256sum's output into path -> digest. Each line is the hex digest,
/// a space, a marker (a space for text mode, `*` for binary) and the name; a
/// name containing a backslash or newline makes the whole name escaped with a
/// leading backslash (coreutils' rule). A line that does not parse is dropped:
/// its path then has no digest, which reads as "changed" - never as a skip.
fn parse_sha256sum(stdout: &[u8]) -> HashMap<String, [u8; 32]> {
    let mut out = HashMap::new();
    for line in String::from_utf8_lossy(stdout).split('\n') {
        let bytes = line.as_bytes();
        // 64 hex digits + space + marker + at least one character of name.
        if bytes.len() < 67
            || bytes[64] != b' '
            || (bytes[65] != b' ' && bytes[65] != b'*')
            || !bytes[..64].iter().all(u8::is_ascii_hexdigit)
        {
            continue;
        }
        let name = if let Some(escaped) = line[66..].strip_prefix('\\') {
            unescape_sha_name(escaped)
        } else {
            line[66..].to_string()
        };
        let mut digest = [0u8; 32];
        let mut parsed = true;
        for (i, byte) in digest.iter_mut().enumerate() {
            match (hex_val(bytes[i * 2]), hex_val(bytes[i * 2 + 1])) {
                (Some(hi), Some(lo)) => *byte = hi << 4 | lo,
                _ => {
                    parsed = false;
                    break;
                }
            }
        }
        if parsed {
            out.insert(name, digest);
        }
    }
    out
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// coreutils' backslash escaping, undone: `\\` is a backslash, `\\n` a newline,
/// anything else keeps both characters.
fn unescape_sha_name(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The local file's sha256, streamed; None when it cannot be read (the caller
/// then sends it, and the plain upload reports the real error).
fn local_sha256(path: &Path) -> Option<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().into())
}

/// Run one sync. `emit` receives each progress line (already newline-terminated
/// where wanted by the caller) so both the action's tap and tests can observe.
pub async fn sync_tree(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    opts: &SyncOptions,
    mut emit: impl FnMut(&str) + Send,
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
    let single = std::fs::metadata(&opts.source)
        .map(|m| m.is_file())
        .unwrap_or(false);
    if opts.base.is_some() && !single {
        return Err("base applies to a single-file upload only".into());
    }
    let pairs: Vec<(PathBuf, String)> = if single {
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
    {
        let mut uploader = Uploader {
            registry,
            target,
            holder: holder.clone(),
            verbose: opts.verbose,
            single,
            base_hint: opts.base.clone(),
            emit: &mut emit,
            cancel,
            made_dirs: Vec::new(),
            delta_ok: opts.delta,
            mtime_note: false,
            report: &mut report,
        };
        // Same-size files whose mtime differs are held back and content-checked
        // in batches after the walk, so one exec settles up to 200 of them.
        let mut verify: Vec<PendingVerify> = Vec::new();
        for (local, rel) in &pairs {
            if cancel.is_cancelled() {
                return Err("the sync was canceled".into());
            }
            let meta = match std::fs::metadata(local) {
                Ok(m) => m,
                Err(err) => {
                    uploader.report.failures.push(format!("{rel}: {err}"));
                    continue;
                }
            };
            let remote_path = safe_join(&target.workspace_root, rel)?;
            let existing = match registry.stat(&target.endpoint, &holder, &remote_path).await {
                Ok(stat) => stat,
                Err(err) => return Err(format!("stat {remote_path}: {err}")),
            };
            // A missing file is the normal first-sync case, not an error.
            let local_secs = mtime_secs(&meta);
            match existing {
                None => {
                    uploader
                        .push(local, rel, &remote_path, None, local_secs, meta.len())
                        .await?
                }
                Some(stat) => match freshness(stat, meta.len(), local_secs) {
                    Freshness::UpToDate => uploader.report.skipped += 1,
                    Freshness::Upload => {
                        uploader
                            .push(local, rel, &remote_path, Some(stat), local_secs, meta.len())
                            .await?
                    }
                    Freshness::VerifyContent => verify.push(PendingVerify {
                        local: local.clone(),
                        rel: rel.clone(),
                        remote_path,
                        stat,
                        len: meta.len(),
                        local_secs: local_secs.unwrap_or_default(),
                    }),
                },
            }
        }
        verify_batches(&mut uploader, verify).await?;
    }
    emit(&format!(
        "sync: {} scanned, {} uploaded ({} bytes{}), {} up to date{}\n",
        report.scanned,
        report.uploaded,
        report.bytes,
        delta_note(&report),
        report.skipped,
        if report.failures.is_empty() {
            String::new()
        } else {
            format!(", {} failed", report.failures.len())
        },
    ));
    Ok(report)
}

/// The summary's delta clause: empty when every file went up whole, so the line
/// keeps its old shape.
pub fn delta_note(report: &SyncReport) -> String {
    if report.delta == 0 {
        return String::new();
    }
    format!("; {} by delta, {} bytes sent", report.delta, report.sent)
}

/// A remote path as the run output shows it: relative to the workspace when under it.
fn shown(root: &str, path: &str) -> String {
    let root = root.trim_end_matches('/');
    path.strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
        .to_string()
}

/// How many same-extension siblings [guess_base] stats for their modify time.
const GUESS_CANDIDATES: usize = 8;

/// A base for a destination that does not exist yet (rsync's --fuzzy): a sibling
/// file with the same extension and a size within a factor of two, the newest of the
/// closest few. `app-1.4.jar` pushed as `app-1.5.jar` diffs against the 1.4 build. A
/// wrong guess costs the signature round trip and a patch about the file's size -
/// never correctness: the rebuild is checked by sha256.
async fn guess_base(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    remote_path: &str,
    local_len: u64,
) -> Option<String> {
    let (dir, name) = remote_path.rsplit_once('/')?;
    let dir = if dir.is_empty() { "/" } else { dir };
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty())?;
    let listing = registry.list_dir(&target.endpoint, holder, dir).await.ok()?;
    let mut candidates: Vec<_> = listing
        .into_iter()
        .filter(|entry| {
            !entry.is_dir
                && entry.name != name
                // Dot files include the delta's own temporaries.
                && !entry.name.starts_with('.')
                && !entry.name.contains('/')
                && !entry.name.contains('\\')
                && entry
                    .name
                    .rsplit_once('.')
                    .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
                && entry.size >= local_len / 2
                && entry.size <= local_len.saturating_mul(2)
        })
        .collect();
    candidates.sort_by_key(|entry| entry.size.abs_diff(local_len));
    candidates.truncate(GUESS_CANDIDATES);
    let mut best: Option<(u64, String)> = None;
    for entry in candidates {
        let path = format!("{}/{}", dir.trim_end_matches('/'), entry.name);
        let mtime = match registry.stat(&target.endpoint, holder, &path).await {
            Ok(Some(stat)) if !stat.is_dir => stat.mtime_ms.unwrap_or(0),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(newest, _)| mtime > *newest) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Stream one local file up to `remote_path`, one chunk at a time: the file never
/// sits whole in the gateway's memory (a 200 MB jar used to), and a cancel stops it
/// between chunks. Dropping the writer before `finish` aborts the upload.
async fn upload_one(
    registry: &Arc<RemoteTransportRegistry>,
    target: &RemoteTarget,
    holder: &str,
    local: &Path,
    rel: &str,
    remote_path: &str,
    cancel: &swiss_host::services::action::CancelHandle,
) -> Result<u64, String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|err| format!("read {rel}: {err}"))?;
    let mut writer = registry
        .create(&target.endpoint, holder, remote_path)
        .await
        .map_err(|err| format!("create {remote_path}: {err}"))?;
    let mut buf = vec![0u8; swiss_host::services::remote::REMOTE_CHUNK_BYTES];
    let mut sent: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err("the sync was canceled".into());
        }
        // Fill the whole chunk before sending: a short read is not the end of a file.
        let mut filled = 0;
        while filled < buf.len() {
            let n = file
                .read(&mut buf[filled..])
                .await
                .map_err(|err| format!("read {rel}: {err}"))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        writer
            .write_chunk(&buf[..filled])
            .await
            .map_err(|err| format!("write {rel}: {err}"))?;
        sent += filled as u64;
        if filled < buf.len() {
            break;
        }
    }
    writer
        .finish()
        .await
        .map_err(|err| format!("commit {rel}: {err}"))?;
    Ok(sent)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_target_without_sha256sum_says_so_once_and_sends_the_files() {
        let (system, fake) = crate::testing::system_with_fake();
        fake.no_sha256sum
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fake.files
            .lock()
            .unwrap()
            .insert("/data/ws/proj/same.txt".into(), b"same".to_vec());
        let dir =
            std::env::temp_dir().join(format!("swiss-ntn-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join("same.txt"), b"same").expect("w");
        let target = system.resolve("dev").expect("the seeded target");
        let opts = SyncOptions {
            source: dir.clone(),
            extra_excludes: Vec::new(),
            to: None,
            verbose: false,
            delta: true,
            base: None,
        };
        let mut lines = String::new();
        let report = sync_tree(
            &system.services().remote.clone(),
            &target,
            &opts,
            |line: &str| lines.push_str(line),
            &swiss_host::services::action::CancelSource::new().handle(),
        )
        .await
        .expect("the sync runs");
        assert_eq!(report.uploaded, 1, "no hashing: the file goes up whole");
        assert_eq!(
            lines.matches("sha256sum").count(),
            1,
            "the missing tool is explained exactly once: {lines}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256sum_output_parses_by_name_not_position() {
        let digest = [0xabu8; 32];
        let hexed: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let out = format!("{hexed}  /a/b.txt\n{hexed} *binary.bin\n{hexed}  plain\n");
        let map = parse_sha256sum(out.as_bytes());
        assert_eq!(map.len(), 3, "{map:?}");
        assert_eq!(map.get("/a/b.txt"), Some(&digest));
        assert_eq!(map.get("binary.bin"), Some(&digest));
        assert_eq!(map.get("plain"), Some(&digest));
    }

    #[test]
    fn escaped_names_come_back_and_junk_lines_are_ignored() {
        let digest = [7u8; 32];
        let hexed: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        // The true name holds two backslashes and a newline; coreutils prints
        // it with a leading marker backslash and every such character doubled.
        let name = "\u{5c}one\u{5c}two\u{a}three.txt".to_string();
        let escaped = "\u{5c}\u{5c}one\u{5c}\u{5c}two\u{5c}nthree.txt";
        let out = format!("{hexed}  {escaped}\u{a}not a sha line at all\u{a}");
        let map = parse_sha256sum(out.as_bytes());
        assert_eq!(map.len(), 1, "{map:?}");
        assert_eq!(map.get(name.as_str()), Some(&digest), "{map:?}");
    }
}