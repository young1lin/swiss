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

//! Freshness (SPEC §memory.freshness): anchored files are hashed when a note is
//! written or verified, and re-hashed when a note is about to be shown. The verdict
//! is computed, never persisted - reads do not mutate. Only regular files under the
//! resolved root are read (canonicalize, then a prefix check), so a symlink that
//! leaves the root is skipped rather than followed. A hash already taken is reused while
//! the file's size and mtime stand ([remembered]), so a review over a thousand
//! unchanged files stats them instead of reading them all again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::model::Anchor;

/// Files above this size are fingerprinted by size and mtime instead of content.
pub const HASH_SIZE_LIMIT: u64 = 1024 * 1024;
/// Files one read call may hash. A whole-project scan (the review inbox, a review run)
/// has no cap: [Checker::whole].
pub const CHECK_CAP: usize = 50;
/// Hashes [remembered] at most; past it the memory starts over.
pub const REMEMBERED_MAX: usize = 16_384;
/// How old a file's mtime must be before its hash is remembered: a file written this
/// recently may be written again within the same mtime tick, its size unchanged.
pub const SETTLED: Duration = Duration::from_secs(2);

/// A content hash and the size and mtime the file had when it was taken.
struct Taken {
    len: u64,
    mtime: SystemTime,
    hash: String,
}

/// The hashes this process took, by canonical path: a file whose size and mtime are
/// still the ones it had is not read again. Nothing is persisted - the verdict is still
/// computed at read time, from the file's metadata.
static REMEMBERED: LazyLock<Mutex<HashMap<PathBuf, Taken>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn remembered(path: &Path, len: u64, mtime: SystemTime) -> Option<String> {
    let seen = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    seen.get(path).filter(|t| t.len == len && t.mtime == mtime).map(|t| t.hash.clone())
}

fn remember(path: PathBuf, len: u64, mtime: SystemTime, hash: &str) {
    if SystemTime::now().duration_since(mtime).map(|age| age < SETTLED).unwrap_or(true) {
        return;
    }
    let mut seen = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() >= REMEMBERED_MAX && !seen.contains_key(&path) {
        seen.clear();
    }
    seen.insert(path, Taken { len, mtime, hash: hash.to_string() });
}

/// Drop every remembered hash (the service's warm state going idle).
pub fn forget() {
    let mut seen = REMEMBERED.lock().unwrap_or_else(|e| e.into_inner());
    *seen = HashMap::new();
}

/// What one probe of an anchored file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Hash(String),
    Missing,
    /// Outside the root, not a regular file, or unreadable: never guessed.
    Skipped,
}

/// The state of one anchor at read time (the wire spelling is `as_str`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorState {
    Ok,
    Changed,
    Missing,
    Unchecked,
    None,
}

impl AnchorState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnchorState::Ok => "ok",
            AnchorState::Changed => "changed",
            AnchorState::Missing => "missing",
            AnchorState::Unchecked => "unchecked",
            AnchorState::None => "none",
        }
    }

    /// Whether the state raises the `anchor_changed` marker.
    pub fn is_alarm(&self) -> bool {
        matches!(self, AnchorState::Changed | AnchorState::Missing)
    }
}

/// Fingerprint `rel` under `root` (both trusted to be canonical for `root`).
pub fn probe(root: &Path, rel: &str) -> Probe {
    let joined = root.join(rel);
    let canonical = match joined.canonicalize() {
        Ok(path) => path,
        Err(_) => return Probe::Missing,
    };
    if !canonical.starts_with(root) {
        return Probe::Skipped;
    }
    let meta = match std::fs::metadata(&canonical) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Probe::Skipped,
        Err(_) => return Probe::Missing,
    };
    if meta.len() > HASH_SIZE_LIMIT {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        return Probe::Hash(format!("size:{}:{mtime}", meta.len()));
    }
    let mtime = meta.modified().ok();
    if let Some(hash) = mtime.and_then(|mtime| remembered(&canonical, meta.len(), mtime)) {
        return Probe::Hash(hash);
    }
    match std::fs::read(&canonical) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let digest: [u8; 32] = hasher.finalize().into();
            let hash = format!("sha256:{}", swiss_core::util::to_hex(&digest));
            // Under the metadata read before the bytes: a write in between leaves an
            // entry the file no longer matches, never a wrong hit.
            if let Some(mtime) = mtime {
                remember(canonical, meta.len(), mtime, &hash);
            }
            Probe::Hash(hash)
        }
        Err(_) => Probe::Skipped,
    }
}

/// Set the baseline hash of every non-glob anchor naming an existing file under
/// `root`; others lose any stale hash (they only trigger).
pub fn baseline(root: Option<&Path>, anchors: &mut [Anchor]) {
    for anchor in anchors.iter_mut() {
        anchor.hash = match (root, anchor.glob) {
            (Some(root), false) => match probe(root, &anchor.path) {
                Probe::Hash(hash) => Some(hash),
                _ => None,
            },
            _ => None,
        };
    }
}

/// One read call's checker: a shared budget of CHECK_CAP files and a per-path cache.
pub struct Checker {
    root: Option<PathBuf>,
    budget: usize,
    cache: HashMap<String, Probe>,
}

impl Checker {
    pub fn new(root: Option<PathBuf>) -> Checker {
        Checker { root, budget: CHECK_CAP, cache: HashMap::new() }
    }

    /// A whole-project scan's checker: no budget, every file the `anchors` name read once up
    /// front - on a blocking thread, so a project of a thousand anchored files does not
    /// stall the runtime. A file read later (none, for these anchors) is read then.
    pub async fn whole<'a>(root: Option<PathBuf>, anchors: impl IntoIterator<Item = &'a Anchor>) -> Checker {
        let mut paths: Vec<String> =
            anchors.into_iter().filter(|a| !a.glob && a.hash.is_some()).map(|a| a.path.clone()).collect();
        paths.sort_unstable();
        paths.dedup();
        let Some(base) = root.clone() else {
            return Checker { root, budget: usize::MAX, cache: HashMap::new() };
        };
        let read = tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .map(|path| {
                    let probed = probe(&base, &path);
                    (path, probed)
                })
                .collect()
        });
        // A probe thread that died leaves the cache empty: the reads happen inline instead.
        let cache = read.await.unwrap_or_default();
        Checker { root, budget: usize::MAX, cache }
    }

    /// A checker that never reads (list and search rows).
    pub fn disabled() -> Checker {
        Checker { root: None, budget: 0, cache: HashMap::new() }
    }

    pub fn state(&mut self, anchor: &Anchor) -> AnchorState {
        let Some(baseline) = anchor.hash.as_deref() else {
            return AnchorState::None;
        };
        if anchor.glob {
            return AnchorState::None;
        }
        let Some(root) = self.root.as_deref() else {
            return AnchorState::Unchecked;
        };
        if !self.cache.contains_key(&anchor.path) {
            if self.budget == 0 {
                return AnchorState::Unchecked;
            }
            self.budget -= 1;
            let probed = probe(root, &anchor.path);
            self.cache.insert(anchor.path.clone(), probed);
        }
        match self.cache.get(&anchor.path) {
            Some(Probe::Hash(now)) if now == baseline => AnchorState::Ok,
            Some(Probe::Hash(_)) => AnchorState::Changed,
            Some(Probe::Missing) => AnchorState::Missing,
            _ => AnchorState::Unchecked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_or_deleted_file_is_flagged_and_a_new_baseline_clears_it() {
        let root = std::env::temp_dir()
            .join(format!("swiss-mem-fresh-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("src").join("a.rs"), "one").unwrap();
        let mut anchors = vec![
            Anchor { path: "src/a.rs".into(), glob: false, hash: None },
            Anchor { path: "src/*.rs".into(), glob: true, hash: None },
        ];
        baseline(Some(&root), &mut anchors);
        assert!(anchors[0].hash.as_deref().unwrap_or("").starts_with("sha256:"));
        assert_eq!(anchors[1].hash, None);
        assert_eq!(Checker::new(Some(root.clone())).state(&anchors[0]), AnchorState::Ok);
        std::fs::write(root.join("src").join("a.rs"), "two").unwrap();
        assert_eq!(Checker::new(Some(root.clone())).state(&anchors[0]), AnchorState::Changed);
        baseline(Some(&root), &mut anchors);
        assert_eq!(Checker::new(Some(root.clone())).state(&anchors[0]), AnchorState::Ok);
        std::fs::remove_file(root.join("src").join("a.rs")).unwrap();
        assert_eq!(Checker::new(Some(root.clone())).state(&anchors[0]), AnchorState::Missing);
        assert_eq!(Checker::new(None).state(&anchors[0]), AnchorState::Unchecked);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_whole_scan_checks_past_a_reads_cap() {
        let dir = std::env::temp_dir().join(format!("swiss-mem-fresh-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let root = dir.canonicalize().unwrap();
        let files = CHECK_CAP + 10;
        let mut anchors: Vec<Anchor> = Vec::new();
        for i in 0..files {
            let path = format!("src/f{i}.rs");
            std::fs::write(root.join(&path), "one").unwrap();
            anchors.push(Anchor { path, glob: false, hash: None });
        }
        baseline(Some(&root), &mut anchors);
        for anchor in &anchors {
            std::fs::write(root.join(&anchor.path), "two").unwrap();
        }
        let states = |checker: &mut Checker| -> Vec<AnchorState> { anchors.iter().map(|a| checker.state(a)).collect() };
        let capped = states(&mut Checker::new(Some(root.clone())));
        assert_eq!(capped.iter().filter(|s| **s == AnchorState::Changed).count(), CHECK_CAP, "a read stops at its cap");
        assert_eq!(capped.iter().filter(|s| **s == AnchorState::Unchecked).count(), 10);
        let whole = states(&mut Checker::whole(Some(root.clone()), &anchors).await);
        assert!(whole.iter().all(|s| *s == AnchorState::Changed), "{whole:?}");
        let rootless = states(&mut Checker::whole(None, &anchors).await);
        assert!(rootless.iter().all(|s| *s == AnchorState::Unchecked), "no root reads nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_settled_files_hash_is_reused_until_its_size_or_mtime_moves() {
        let dir = std::env::temp_dir().join(format!("swiss-mem-fresh-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("a.rs");
        let write = |text: &str, mtime: SystemTime| {
            std::fs::write(&file, text).unwrap();
            std::fs::File::options().write(true).open(&file).unwrap().set_modified(mtime).unwrap();
        };
        let old = SystemTime::now() - Duration::from_secs(60);
        write("one", old);
        let first = probe(&root, "a.rs");
        // The same size and mtime are trusted: the file is not read again.
        write("two", old);
        assert_eq!(probe(&root, "a.rs"), first, "a settled file's hash is remembered");
        // Another mtime is another file.
        write("two", old + Duration::from_secs(1));
        let second = probe(&root, "a.rs");
        assert_ne!(second, first);
        // Another size too, whatever the mtime.
        write("three", old + Duration::from_secs(1));
        assert_ne!(probe(&root, "a.rs"), second);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_written_just_now_is_read_every_time() {
        let dir = std::env::temp_dir().join(format!("swiss-mem-fresh-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("a.rs");
        let now = SystemTime::now();
        let write = |text: &str| {
            std::fs::write(&file, text).unwrap();
            std::fs::File::options().write(true).open(&file).unwrap().set_modified(now).unwrap();
        };
        write("one");
        let first = probe(&root, "a.rs");
        // Rewritten within the same mtime tick, the size unchanged: still told apart.
        write("two");
        assert_ne!(probe(&root, "a.rs"), first, "an unsettled file's hash is never reused");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn paths_that_leave_the_root_are_skipped() {
        let root = std::env::temp_dir()
            .join(format!("swiss-mem-fresh-{}", swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(root.join("inner")).unwrap();
        std::fs::write(root.join("outside.txt"), "x").unwrap();
        let inner = root.join("inner").canonicalize().unwrap();
        assert_eq!(probe(&inner, "../outside.txt"), Probe::Skipped);
        std::fs::remove_dir_all(&root).ok();
    }
}
