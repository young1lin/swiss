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

//! The code index (SPEC §memory.code, ADR-032 phase 5): what a checkout's files hold -
//! each file and its hash, the names it defines (and so contains), the modules it imports
//! and the code-like names it references - kept apart from the memory graph in its own
//! `code.sqlite3`. It is derived: built only when asked (`swiss memory code index`),
//! refreshed by size, mtime and then hash, and deleted without a note noticing. A save
//! checks what a note names against it ([check]).
//!
//! No daemon and no watcher: an index is as fresh as its last refresh, and a save that
//! misses a name refreshes one older than its lister's gap before it judges.
//!
//! The index lives under the home like the store, and holds no credential: a file whose
//! name says it holds one ([crate::secrets::sensitive_path]) is listed and never opened,
//! and every other file's text passes [crate::secrets::redact] before a name is taken.

pub mod check;
pub mod db;
pub mod extract;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use crate::error::{MemError, MemResult};
use crate::evidence::EvidenceSpec;
use crate::formation::{self, SetClaim};
use crate::fresh::HASH_SIZE_LIMIT;
use crate::glob;
use crate::model::{Actor, Anchor, Kind, Producer};
use crate::secrets;
use crate::service::{MemoryService, Target};
use crate::store;
use db::{Db, FileRow, Names};
use extract::{Facts, Lang};

pub use check::{CodeCheck, CodeFinding};

pub const CODE_FILE: &str = "code.sqlite3";
/// How old an index may be before a save's miss refreshes it, unless the lister says.
pub const REFRESH_GAP: Duration = Duration::from_secs(30);
/// Files examined per blocking batch and per write transaction.
const BATCH: usize = 256;
/// A file whose first bytes hold a NUL is binary: listed, never read for names.
const BINARY_SNIFF: usize = 8 * 1024;
/// Extensions listed but never read: data and generated text, not code or prose about it.
const NOT_READ: [&str; 12] = ["jsonl", "ndjson", "map", "svg", "csv", "tsv", "lock", "log", "snap", "min.js", "min.css", "pem"];
/// Lock files under names of their own.
const LOCK_FILES: [&str; 3] = ["package-lock.json", "pnpm-lock.yaml", "npm-shrinkwrap.json"];
/// A find answers at most this many referencing files.
pub const FIND_REFS_MAX: usize = 50;

/// The files of a checkout, listed by the host (git's tracked and unignored files, or a
/// bounded walk): this crate never runs git.
#[async_trait::async_trait]
pub trait Lister: Send + Sync {
    /// Repo-relative paths with `/`.
    async fn list(&self, root: &Path) -> MemResult<Listing>;

    /// How old an index may be before a save's miss refreshes it.
    fn refresh_gap(&self) -> Duration {
        REFRESH_GAP
    }
}

#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub files: Vec<String>,
    /// The listing stopped at a cap: what is absent may still exist.
    pub truncated: bool,
    /// Listed by git, not walked.
    pub git: bool,
}

/// What one refresh did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshReport {
    pub root: String,
    pub listed: u64,
    /// Read and extracted: new or changed bytes.
    pub read: u64,
    /// A new stat over the same bytes.
    pub touched: u64,
    pub unchanged: u64,
    pub removed: u64,
    /// Listed, not read: binary, too large or data.
    pub skipped: u64,
    pub defs: u64,
    pub refs: u64,
    pub imports: u64,
    pub truncated: bool,
    pub git: bool,
    pub took_ms: u64,
}

/// A root's index at a glance.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeStatus {
    pub root: String,
    pub indexed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_ms: Option<i64>,
    pub files: u64,
    pub skipped: u64,
    pub defs: u64,
    pub refs: u64,
    pub imports: u64,
    /// Distinct names over every indexed root.
    pub names: u64,
    pub truncated: bool,
    pub git: bool,
    /// The index file's size (every root's).
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefHit {
    pub path: String,
    pub kind: &'static str,
    pub line: u32,
}

/// Where a name is defined and which files mention it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameHits {
    pub name: String,
    pub indexed: bool,
    pub defs: Vec<DefHit>,
    /// The first [FIND_REFS_MAX] files that mention it: those whose code names it, then
    /// those only a comment names it in, each part by path.
    pub refs: Vec<String>,
    pub refs_total: u64,
    /// Those of `refs` only a comment names it in: a mention, not where it is used.
    pub in_comments: Vec<String>,
    /// How many of the `refs_total` files name it only in a comment.
    pub in_comments_total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDef {
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportView {
    pub spec: String,
    pub line: u32,
}

/// One file as the index holds it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    pub size: u64,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    pub defs: Vec<FileDef>,
    pub imports: Vec<ImportView>,
    /// Distinct code-like names it mentions.
    pub refs: u64,
}

/// The service's code index: the file, opened on first use, the host's lister, and one
/// refresh at a time.
pub(crate) struct CodeState {
    path: PathBuf,
    db: OnceCell<Db>,
    closed: tokio::sync::RwLock<bool>,
    lister: RwLock<Option<Arc<dyn Lister>>>,
    lock: tokio::sync::Mutex<()>,
}

impl CodeState {
    /// The index beside the store at `store_path`.
    pub(crate) fn beside(store_path: &Path) -> CodeState {
        CodeState {
            path: store_path.with_file_name(CODE_FILE),
            db: OnceCell::new(),
            closed: tokio::sync::RwLock::new(false),
            lister: RwLock::new(None),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) async fn db(&self) -> MemResult<&Db> {
        let closed = self.closed.read().await;
        if *closed {
            return Err(MemError::store("the memory code index is closed"));
        }
        self.db.get_or_try_init(|| Db::open(&self.path)).await
    }

    pub(crate) async fn close(&self) {
        *self.closed.write().await = true;
        if let Some(db) = self.db.get() {
            db.pool.close().await;
        }
    }

    pub(crate) fn lister(&self) -> Option<Arc<dyn Lister>> {
        self.lister.read().ok().and_then(|slot| slot.clone())
    }

    /// The index of `root`, if one was ever built; never creates the file.
    pub(crate) async fn root_row(&self, root: &str) -> MemResult<Option<db::RootRow>> {
        if self.db.get().is_none() && !self.path.exists() {
            return Ok(None);
        }
        let mut conn = self.db().await?.pool.acquire().await?;
        db::root(&mut conn, root).await
    }

    /// Refresh `root` when its index is at least `gap` old; whether it ran.
    pub(crate) async fn refresh_if_older(&self, root: &Path, gap: Duration) -> MemResult<bool> {
        let Some(lister) = self.lister() else { return Ok(false) };
        let key = root_key(root);
        let _one = self.lock.lock().await;
        match self.root_row(&key).await? {
            Some(row) if row.stale || store::now() - row.indexed_ms >= gap.as_millis() as i64 => {}
            _ => return Ok(false),
        }
        self.refresh_locked(root, lister.as_ref()).await?;
        Ok(true)
    }

    pub(crate) async fn refresh(&self, root: &Path) -> MemResult<RefreshReport> {
        let lister = self.lister().ok_or_else(|| MemError::invalid("no file lister is set: the code index needs the host's"))?;
        let _one = self.lock.lock().await;
        self.refresh_locked(root, lister.as_ref()).await
    }

    /// List, stat, read what changed, write per batch; remove what is no longer listed.
    async fn refresh_locked(&self, root: &Path, lister: &dyn Lister) -> MemResult<RefreshReport> {
        let started = Instant::now();
        let listing = lister.list(root).await?;
        let key = root_key(root);
        let db = self.db().await?;
        let mut conn = db.pool.acquire().await?;
        let root_id = db::ensure_root(&mut conn, &key).await?;
        let mut known = db::files(&mut conn, root_id).await?;
        let mut report = RefreshReport {
            root: key,
            listed: listing.files.len() as u64,
            truncated: listing.truncated,
            git: listing.git,
            ..Default::default()
        };
        let mut names = Names::default();
        let mut seen: HashSet<&str> = HashSet::with_capacity(listing.files.len());
        for chunk in listing.files.chunks(BATCH) {
            let work: Vec<(String, Option<FileRow>)> = chunk
                .iter()
                .filter(|path| seen.insert(path.as_str()))
                .map(|path| (path.clone(), known.remove(path)))
                .collect();
            let base = root.to_path_buf();
            let examined = tokio::task::spawn_blocking(move || {
                let look = |(path, prev): (String, Option<FileRow>)| {
                    let seen = examine(&base, &path, prev.as_ref());
                    (path, prev, seen)
                };
                work.into_iter().map(look).collect::<Vec<_>>()
            })
            .await
            .map_err(|err| MemError::store(format!("the code index's file read failed: {err}")))?;
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
            let written = async {
                for (path, prev, seen) in &examined {
                    match seen {
                        Seen::Unchanged => report.unchanged += 1,
                        Seen::Touched(stat) => {
                            if let Some(prev) = prev {
                                db::touch_file(&mut conn, prev.id, *stat).await?;
                            }
                            report.touched += 1;
                        }
                        Seen::Skipped { stat, hash, why } => {
                            let lang = Lang::of(path).map(|l| l.as_str());
                            db::put_file(&mut conn, root_id, path, *stat, hash, lang, Some(why)).await?;
                            report.skipped += 1;
                        }
                        Seen::Read { stat, hash, facts } => {
                            let lang = Lang::of(path).map(|l| l.as_str());
                            let id = db::put_file(&mut conn, root_id, path, *stat, hash, lang, None).await?;
                            db::put_facts(&mut conn, &mut names, id, facts).await?;
                            report.read += 1;
                            report.defs += facts.defs.len() as u64;
                            report.refs += facts.refs.len() as u64;
                            report.imports += facts.imports.len() as u64;
                        }
                        Seen::Gone => {
                            if let Some(prev) = prev {
                                db::delete_file(&mut conn, prev.id).await?;
                                report.removed += 1;
                            }
                        }
                    }
                }
                Ok::<(), MemError>(())
            }
            .await;
            store::finish(&mut conn, written).await?;
        }
        // What the listing no longer names.
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let tail = async {
            for prev in known.values() {
                db::delete_file(&mut conn, prev.id).await?;
                report.removed += 1;
            }
            if report.removed > 0 || report.read > 0 || report.skipped > 0 {
                db::prune_names(&mut conn).await?;
            }
            db::stamp_root(&mut conn, root_id, store::now(), listing.files.len(), listing.truncated, listing.git).await
        }
        .await;
        store::finish(&mut conn, tail).await?;
        report.took_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }
}

/// What a look at one listed file found.
enum Seen {
    Unchanged,
    Touched((i64, i64)),
    Skipped { stat: (i64, i64), hash: String, why: &'static str },
    Read { stat: (i64, i64), hash: String, facts: Facts },
    /// Listed but not a regular file now (deleted in the work tree, a link, a directory).
    Gone,
}

/// Stat `rel`; read it only when its size or mtime moved, extract only when its bytes did.
fn examine(root: &Path, rel: &str, prev: Option<&FileRow>) -> Seen {
    let path = root.join(rel);
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return Seen::Gone };
    if !meta.is_file() {
        return Seen::Gone;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let stat = (meta.len() as i64, mtime);
    if prev.is_some_and(|p| p.size == stat.0 && p.mtime_ms == stat.1) {
        return Seen::Unchanged;
    }
    // A file that holds credentials is listed (its path is no secret), never opened.
    if secrets::sensitive_path(rel) {
        return Seen::Skipped { stat, hash: format!("size:{}:{mtime}", meta.len()), why: "sensitive" };
    }
    if meta.len() > HASH_SIZE_LIMIT {
        return Seen::Skipped { stat, hash: format!("size:{}:{mtime}", meta.len()), why: "large" };
    }
    let Ok(bytes) = std::fs::read(&path) else { return Seen::Gone };
    let hash = sha256(&bytes);
    if prev.is_some_and(|p| p.hash == hash) {
        return Seen::Touched(stat);
    }
    if not_read(rel) {
        return Seen::Skipped { stat, hash, why: "data" };
    }
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        return Seen::Skipped { stat, hash, why: "binary" };
    }
    // A credential-shaped span (a key in a test, a token in a config) never becomes a name.
    let facts = extract::extract(rel, &secrets::redact_lines(&String::from_utf8_lossy(&bytes)));
    Seen::Read { stat, hash, facts }
}

fn not_read(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    LOCK_FILES.contains(&name.as_str()) || NOT_READ.iter().any(|ext| name.strip_suffix(ext).is_some_and(|stem| stem.ends_with('.')))
}

fn sha256(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    format!("sha256:{}", swiss_core::util::to_hex(&digest))
}

/// The key a root is filed under: its canonical path as a person reads it.
pub(crate) fn root_key(root: &Path) -> String {
    crate::scope::display(root)
}

/// What one note says, as the index judges it.
struct Named<'a> {
    claims: &'a [check::Claim],
    sets: &'a [SetClaim],
    anchors: &'a [Anchor],
    /// The paths the note names in its text and cites as evidence.
    named: &'a [String],
    /// The anchored files' text.
    text: &'a str,
}

impl MemoryService {
    /// What a note about to be saved names, quotes and counts, checked against its checkout
    /// (SPEC §memory.code); None for the global notes and outside a checkout. Holds the note
    /// unless a person wrote it. `evidence` is what this call cites: a piece bound earlier
    /// keeps only its excerpt's hash, so only a new one is quoted against its file.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn check_code(
        &self,
        target: &Target,
        actor: &Actor,
        kind: Kind,
        title: &str,
        content: &str,
        anchors: &[Anchor],
        evidence: &[EvidenceSpec],
    ) -> MemResult<(Option<CodeCheck>, String)> {
        let root = {
            let mut conn = self.store().await?.read().await?;
            self.locate(&mut conn, target).await?.root
        };
        let Some(root) = root else { return Ok((None, String::new())) };
        let claims = check::claims(title, content);
        // A stated preference is absolute by design: it counts nothing.
        let sets = if kind == Kind::Preference { Vec::new() } else { formation::set_claims(title, content) };
        let excerpts: Vec<&str> = evidence.iter().filter_map(|e| e.excerpt.as_deref()).collect();
        let unbacked = check::unbacked_sets(&sets, &excerpts);
        let mut named: Vec<String> = claims
            .iter()
            .filter_map(|c| match c {
                check::Claim::Path(path) => Some(path.clone()),
                _ => None,
            })
            .collect();
        named.extend(evidence.iter().filter_map(|e| e.path.clone()));
        let (base, owned, cited) = (root.clone(), anchors.to_vec(), evidence.to_vec());
        let (mut findings, text) = tokio::task::spawn_blocking(move || {
            let mut found = check::missing_anchors(&base, &owned);
            found.extend(check::evidence_findings(&base, &cited));
            (found, check::anchored_text(&base, &owned))
        })
        .await
        .map_err(|err| MemError::store(format!("the anchor read failed: {err}")))?;
        findings.extend(unbacked);
        let mut out = CodeCheck { index: "none", indexed_ms: None, names: claims.len(), findings: Vec::new(), held: false, error: None };
        let key = root_key(&root);
        let note = Named { claims: &claims, sets: &sets, anchors, named: &named, text: &text };
        let judged = async {
            let Some(mut row) = self.code.root_row(&key).await? else { return Ok(None) };
            let mut found = self.judge(&row, &root, &note).await?;
            let mut index = "checked";
            if let Some(lister) = self.code.lister().filter(|_| found.iter().any(|f| f.holds())) {
                if self.code.refresh_if_older(&root, lister.refresh_gap()).await? {
                    index = "refreshed";
                    if let Some(fresh) = self.code.root_row(&key).await? {
                        row = fresh;
                    }
                    found = self.judge(&row, &root, &note).await?;
                }
            }
            Ok::<_, MemError>(Some((index, row.indexed_ms, found)))
        }
        .await;
        match judged {
            Ok(Some((index, indexed_ms, found))) => {
                out.index = index;
                out.indexed_ms = Some(indexed_ms);
                findings.extend(found);
            }
            Ok(None) => {}
            Err(err) => out.error = Some(err.message),
        }
        out.held = actor.producer != Producer::Human && findings.iter().any(|f| f.holds());
        out.findings = findings;
        Ok((Some(out), text))
    }

    /// Every claim, every claim over a set and every glob anchor against one root's index.
    async fn judge(&self, row: &db::RootRow, root: &Path, note: &Named<'_>) -> MemResult<Vec<CodeFinding>> {
        let mut conn = self.code.db().await?.pool.acquire().await?;
        let mut out = Vec::new();
        for claim in note.claims {
            if let Some(finding) = check::judge(&mut conn, row.id, root, claim, note.anchors, note.text, !row.truncated).await? {
                out.push(finding);
            }
        }
        if row.truncated {
            return Ok(out);
        }
        let globs = note.anchors.iter().any(|a| a.glob);
        let paths = if globs || note.sets.iter().any(|c| !c.sets.is_empty()) { db::paths(&mut conn, row.id).await? } else { Vec::new() };
        let named = |path: &str| note.anchors.iter().any(|a| glob::anchor_matches(a, path)) || note.named.iter().any(|n| n == path);
        for claim in note.sets {
            if let Some(finding) = check::recount(&mut conn, row.id, root, claim, &paths, &named).await? {
                if !out.iter().any(|f: &CodeFinding| f.rule == finding.rule && f.name == finding.name) {
                    out.push(finding);
                }
            }
        }
        if globs {
            out.extend(check::empty_globs(&paths, note.anchors));
        }
        Ok(out)
    }

    /// Set (or clear) the host's file lister; without one an index cannot be built.
    pub fn set_lister(&self, lister: Option<Arc<dyn Lister>>) {
        if let Ok(mut slot) = self.code.lister.write() {
            *slot = lister;
        }
    }

    /// The checkout a target resolves to.
    pub async fn code_root(&self, target: &Target) -> MemResult<PathBuf> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        place
            .root
            .ok_or_else(|| MemError::invalid("no checkout here: the code index is per project root (the global notes have none)"))
    }

    /// Build or refresh the target's index.
    pub async fn code_index(&self, target: &Target) -> MemResult<RefreshReport> {
        let root = self.code_root(target).await?;
        self.code.refresh(&root).await
    }

    pub async fn code_status(&self, target: &Target) -> MemResult<CodeStatus> {
        let root = self.code_root(target).await?;
        let key = root_key(&root);
        let Some(row) = self.code.root_row(&key).await? else {
            return Ok(CodeStatus { root: key, ..Default::default() });
        };
        let mut conn = self.code.db().await?.pool.acquire().await?;
        let [files, skipped, defs, refs, imports, names] = db::counts(&mut conn, row.id).await?;
        Ok(CodeStatus {
            root: key,
            indexed: true,
            indexed_ms: Some(row.indexed_ms),
            files,
            skipped,
            defs,
            refs,
            imports,
            names,
            truncated: row.truncated,
            git: row.git,
            bytes: db::bytes(&mut conn).await?,
        })
    }

    /// Where `name` is defined and which files mention it.
    pub async fn code_find(&self, target: &Target, name: &str) -> MemResult<NameHits> {
        let name = name.trim();
        if name.is_empty() || name.len() > extract::NAME_MAX {
            return Err(MemError::invalid(format!("name: 1 to {} characters", extract::NAME_MAX)));
        }
        let root = self.code_root(target).await?;
        // An index past the lister's gap is refreshed first, so a definition written since -
        // or one a newer scan reads out of an unchanged file - is where the answer says. One
        // that fails to refresh answers as it is.
        if let Some(lister) = self.code.lister() {
            self.code.refresh_if_older(&root, lister.refresh_gap()).await.ok();
        }
        let Some(row) = self.code.root_row(&root_key(&root)).await? else {
            return Ok(NameHits { name: name.to_string(), ..Default::default() });
        };
        let mut conn = self.code.db().await?.pool.acquire().await?;
        let defs = db::defs_of(&mut conn, row.id, name).await?;
        let mentions = db::refs_of(&mut conn, row.id, name, FIND_REFS_MAX).await?;
        let in_comments = mentions.files.iter().filter(|(_, comment)| *comment).map(|(path, _)| path.clone()).collect();
        Ok(NameHits {
            name: name.to_string(),
            indexed: true,
            defs: defs.into_iter().map(|(path, kind, line)| DefHit { path, kind, line }).collect(),
            refs_total: mentions.total,
            in_comments_total: mentions.in_comments,
            in_comments,
            refs: mentions.paths(),
        })
    }

    /// One file of the target's index (a repo-relative path, or the one file ending in it).
    pub async fn code_file(&self, target: &Target, path: &str) -> MemResult<FileView> {
        let rel = crate::glob::normalize(path);
        if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|part| part == "..") {
            return Err(MemError::invalid("path: a repo-relative file path"));
        }
        let root = self.code_root(target).await?;
        let none = || MemError::not_found(format!("{rel} is not in this checkout's code index"));
        let row = self.code.root_row(&root_key(&root)).await?.ok_or_else(none)?;
        let mut conn = self.code.db().await?.pool.acquire().await?;
        let path = db::file_named(&mut conn, row.id, &rel).await?.ok_or_else(none)?;
        let facts = db::file(&mut conn, row.id, &path).await?.ok_or_else(none)?;
        Ok(FileView {
            path,
            lang: facts.lang,
            size: facts.size.max(0) as u64,
            hash: facts.hash,
            skipped: facts.skipped,
            defs: facts.defs.into_iter().map(|(name, kind, line)| FileDef { name, kind, line }).collect(),
            imports: facts.imports.into_iter().map(|(spec, line)| ImportView { spec, line }).collect(),
            refs: facts.refs,
        })
    }

    /// Delete the target's index: the files it held. No note changes.
    pub async fn code_drop(&self, target: &Target) -> MemResult<u64> {
        let root = self.code_root(target).await?;
        let _one = self.code.lock.lock().await;
        let Some(row) = self.code.root_row(&root_key(&root)).await? else { return Ok(0) };
        let mut conn = self.code.db().await?.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let dropped = async {
            let files = db::drop_root(&mut conn, row.id).await?;
            db::prune_names(&mut conn).await?;
            Ok(files)
        }
        .await;
        store::finish(&mut conn, dropped).await
    }

    /// Delete every root's index and give the space back: the files they held.
    pub async fn code_drop_all(&self) -> MemResult<u64> {
        let _one = self.code.lock.lock().await;
        if self.code.db.get().is_none() && !self.code.path.exists() {
            return Ok(0);
        }
        let mut conn = self.code.db().await?.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let dropped = db::drop_all(&mut conn).await;
        let files = store::finish(&mut conn, dropped).await?;
        sqlx::query("VACUUM").execute(&mut *conn).await?;
        Ok(files)
    }
}
