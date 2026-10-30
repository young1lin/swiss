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

//! The project map (SPEC §memory.domains): a project's knowledge has a shape before it has
//! details. Domains form a tree of at most five levels; each has a hub (what the area is
//! and where it lives, pointing at code) and `owns` (the code it answers for). Notes are
//! filed in one domain or none - by hand, or by their anchors through `owns`.
//!
//! The pure part - ownership, the filing rule, counts, coverage - is up here, testable
//! without a store; the service operations follow, each one transaction like a note's.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::json;
use sqlx::sqlite::SqliteConnection;

use crate::error::{Code, MemError, MemResult};
use crate::glob;
use crate::graph;
use crate::fresh::Checker;
use crate::model::{
    check_domain_path, domain_depth, domain_is_under, domain_parent, is_domain_id, new_domain_id, Actor, Anchor, Domain,
    DomainRevision, DomainSnapshot, Memory, Status, DOMAINS_PER_PROJECT_MAX, DOMAIN_BODY_MAX, DOMAIN_DEPTH_MAX,
    DOMAIN_OWNS_MAX, DOMAIN_SUMMARY_MAX, DOMAIN_TITLE_MAX, FILE_IDS_MAX,
};
use crate::scope::GLOBAL_PROJECT_ID;
use crate::service::{check_secret, clean_anchor_path, clean_reason, in_scope, load_scope, revision_of, MemoryService, Target};
use crate::store;
use crate::view::{self, Summary};

/// Uncovered directories listed at most.
pub const UNCOVERED_MAX: usize = 20;
/// Paths a not-found error names at most.
const NAMED_PATHS_MAX: usize = 30;

// --- ownership and filing -------------------------------------------------------------------

/// Which notes a read admits by domain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DomainFilter {
    #[default]
    Any,
    /// The notes filed nowhere.
    Unfiled,
    /// The notes filed in these domains (a domain and its subtree).
    In(HashSet<String>),
}

impl DomainFilter {
    pub fn admits(&self, memory: &Memory) -> bool {
        match self {
            DomainFilter::Any => true,
            DomainFilter::Unfiled => memory.domain.is_none(),
            DomainFilter::In(ids) => memory.domain.as_ref().is_some_and(|d| ids.contains(d)),
        }
    }
}

/// Whether one `owns` entry owns a repo-relative path: what it matches as a glob, and - as
/// a directory - everything under it.
pub fn owns_path(entry: &str, path: &str) -> bool {
    if entry.contains(['*', '?']) {
        // A glob naming directories owns what lies under them.
        return glob::matches_or_under(entry, path);
    }
    under_dir(entry.trim_end_matches('/'), path)
}

fn under_dir(base: &str, path: &str) -> bool {
    path == base || (path.len() > base.len() && path.starts_with(base) && path.as_bytes()[base.len()] == b'/')
}

/// Of the entries owning a path, the one whose domain is deepest; between two of one depth,
/// the longer entry, then tree order. An entry that cannot win is never tested.
fn best<'a, E>(entries: impl IntoIterator<Item = (&'a Domain, (usize, usize), E)>, owns: impl Fn(&E) -> bool) -> Option<&'a Domain> {
    let mut best: Option<(&'a Domain, (usize, usize))> = None;
    for (domain, key, entry) in entries {
        if best.is_none_or(|(_, have)| key > have) && owns(&entry) {
            best = Some((domain, key));
        }
    }
    best.map(|(domain, _)| domain)
}

/// One `owns` entry ready to test paths: a glob split once, or a directory.
enum Owned<'a> {
    Glob(glob::Glob<'a>),
    Dir(&'a str),
}

impl<'a> Owned<'a> {
    fn new(entry: &'a str) -> Owned<'a> {
        if entry.contains(['*', '?']) {
            Owned::Glob(glob::Glob::new(entry))
        } else {
            Owned::Dir(entry.trim_end_matches('/'))
        }
    }

    fn owns(&self, path: &str) -> bool {
        match self {
            // A glob naming directories owns what lies under them.
            Owned::Glob(g) => g.matches_or_under(path),
            Owned::Dir(base) => under_dir(base, path),
        }
    }
}

/// A map's `owns` entries made ready once, for paths by the thousand (coverage reads every
/// file of a checkout).
pub struct Owners<'a> {
    entries: Vec<(&'a Domain, (usize, usize), Owned<'a>)>,
}

impl<'a> Owners<'a> {
    /// [Map::owner] for this map.
    pub fn owner(&self, path: &str) -> Option<&'a Domain> {
        best(self.entries.iter().map(|(domain, key, owned)| (*domain, *key, owned)), |owned| owned.owns(path))
    }
}

/// The literal directory in front of a glob's first wildcard: `crates/*/src/*.rs` → `crates`.
pub fn literal_prefix(pattern: &str) -> &str {
    let cut = pattern.find(['*', '?']).unwrap_or(pattern.len());
    let head = &pattern[..cut];
    match head.rfind('/') {
        Some(at) => &head[..at],
        None => "",
    }
}

/// A project's map over its domains (tree order, as the store reads them).
pub struct Map<'a> {
    pub domains: &'a [Domain],
}

impl<'a> Map<'a> {
    pub fn new(domains: &'a [Domain]) -> Map<'a> {
        Map { domains }
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }

    pub fn by_id(&self, id: &str) -> Option<&'a Domain> {
        self.domains.iter().find(|d| d.id == id)
    }

    pub fn by_path(&self, path: &str) -> Option<&'a Domain> {
        self.domains.iter().find(|d| d.path == path)
    }

    /// The deepest domain owning `path`; between two of one depth, the longer entry, then
    /// tree order. [Map::owners] for many paths.
    pub fn owner(&self, path: &str) -> Option<&'a Domain> {
        let entries = self.domains.iter().flat_map(|domain| domain.owns.iter().map(move |entry| (domain, (domain.depth(), entry.len()), entry)));
        best(entries, |entry| owns_path(entry, path))
    }

    /// The owns entries made ready once, for many paths.
    pub fn owners(&self) -> Owners<'a> {
        let entries = self
            .domains
            .iter()
            .flat_map(|domain| domain.owns.iter().map(move |entry| (domain, (domain.depth(), entry.len()), Owned::new(entry))))
            .collect();
        Owners { entries }
    }

    /// The direct sub-domains of `path`, in tree order.
    pub fn children(&self, path: &'a str) -> impl Iterator<Item = &'a Domain> + 'a {
        self.domains.iter().filter(move |d| d.parent() == Some(path))
    }

    /// A domain and every domain under it.
    pub fn subtree(&self, domain: &Domain) -> HashSet<String> {
        self.domains
            .iter()
            .filter(|d| d.id == domain.id || domain_is_under(&d.path, &domain.path))
            .map(|d| d.id.clone())
            .collect()
    }

    /// A domain's ancestors, the top level first.
    pub fn ancestors(&self, domain: &Domain) -> Vec<&'a Domain> {
        let mut out = Vec::new();
        let mut at = domain.parent();
        while let Some(path) = at {
            if let Some(parent) = self.by_path(path) {
                out.push(parent);
            }
            at = domain_parent(path);
        }
        out.reverse();
        out
    }

    /// The filing rule (SPEC §memory.domains): each anchor - a glob by its literal prefix -
    /// goes to its deepest owner; the note to the domain that took most of them, a tie to
    /// the tied domains' deepest common ancestor. No anchors or no owner: None.
    pub fn file_by_anchors(&self, anchors: &[Anchor]) -> Option<&'a Domain> {
        let mut votes: Vec<(&'a Domain, usize)> = Vec::new();
        for anchor in anchors {
            let probe = if anchor.glob { literal_prefix(&anchor.path) } else { anchor.path.as_str() };
            if probe.is_empty() {
                continue;
            }
            let Some(owner) = self.owner(probe) else { continue };
            match votes.iter_mut().find(|(d, _)| d.id == owner.id) {
                Some((_, n)) => *n += 1,
                None => votes.push((owner, 1)),
            }
        }
        let top = votes.iter().map(|(_, n)| *n).max()?;
        let tied: Vec<&'a Domain> = votes.iter().filter(|(_, n)| *n == top).map(|(d, _)| *d).collect();
        if let [only] = tied.as_slice() {
            return Some(only);
        }
        let first: Vec<&str> = tied[0].path.split('/').collect();
        let mut common = first.len();
        for domain in &tied[1..] {
            let shared = first.iter().zip(domain.path.split('/')).take_while(|(a, b)| **a == *b).count();
            common = common.min(shared);
        }
        if common == 0 {
            return None;
        }
        self.by_path(&first[..common].join("/"))
    }
}

/// A domain's notes at a glance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainCounts {
    pub live: u64,
    pub needs_review: u64,
    pub unconfirmed: u64,
}

impl DomainCounts {
    fn add(&mut self, memory: &Memory) {
        if !memory.status.is_live() {
            return;
        }
        self.live += 1;
        if memory.status == Status::NeedsReview {
            self.needs_review += 1;
        }
        if !memory.confirmed {
            self.unconfirmed += 1;
        }
    }
}

/// Live notes per domain id (filed directly there), and the unfiled ones.
pub fn counts(memories: &[Memory]) -> (HashMap<String, DomainCounts>, DomainCounts) {
    let mut by_domain: HashMap<String, DomainCounts> = HashMap::new();
    let mut unfiled = DomainCounts::default();
    for memory in memories {
        match &memory.domain {
            Some(id) => by_domain.entry(id.clone()).or_default().add(memory),
            None => unfiled.add(memory),
        }
    }
    (by_domain, unfiled)
}

/// One domain's line on the map: `` `path` Title — summary · 3 notes · 1 need review · unconfirmed ``.
pub fn map_line(domain: &Domain, counts: DomainCounts) -> String {
    line_of(&domain.path, &domain.title, &domain.summary, domain.confirmed, counts)
}

fn line_of(path: &str, title: &str, summary: &str, confirmed: bool, counts: DomainCounts) -> String {
    let mut out = format!("`{path}` {title}");
    if !summary.is_empty() {
        out.push_str(" — ");
        out.push_str(summary);
    }
    let mut tail: Vec<String> = Vec::new();
    match counts.live {
        0 => {}
        1 => tail.push("1 note".into()),
        n => tail.push(format!("{n} notes")),
    }
    if counts.needs_review > 0 {
        tail.push(format!("{} need review", counts.needs_review));
    }
    if !confirmed {
        tail.push("unconfirmed".into());
    }
    for part in tail {
        out.push_str(" · ");
        out.push_str(&part);
    }
    out
}

// --- coverage -------------------------------------------------------------------------------

/// One commit as coverage reads it: `git log --name-only`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFiles {
    pub hash: String,
    pub at_ms: i64,
    pub subject: String,
    #[serde(skip)]
    pub files: Vec<String>,
}

/// Why a checkout gave no history, as a stable code: the panel says it in the reader's
/// language, and `reason` keeps git's own line for `GitFailed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitIssue {
    /// No directory of the project is reachable on this machine.
    NoDirectory,
    /// The directory is not inside a Git repository.
    NotARepository,
    /// No git on this machine's PATH.
    GitMissing,
    /// Git refuses the directory: another user owns it (git's safe.directory).
    Refused,
    /// Anything else: git failed, was slow, or could not start.
    GitFailed,
}

impl GitIssue {
    pub fn as_str(self) -> &'static str {
        match self {
            GitIssue::NoDirectory => "no_directory",
            GitIssue::NotARepository => "not_a_repository",
            GitIssue::GitMissing => "git_missing",
            GitIssue::Refused => "refused",
            GitIssue::GitFailed => "git_failed",
        }
    }

    pub fn parse(raw: &str) -> Option<GitIssue> {
        [GitIssue::NoDirectory, GitIssue::NotARepository, GitIssue::GitMissing, GitIssue::Refused, GitIssue::GitFailed]
            .into_iter()
            .find(|issue| issue.as_str() == raw)
    }
}

/// What the caller learned from the checkout (the root crate runs Git; this crate never
/// spawns a process).
#[derive(Debug, Clone, Default)]
pub struct Checkout {
    /// Tracked files (or a bounded walk's files outside Git); None: no readable root.
    pub files: Option<Vec<String>>,
    /// Whether `files` was cut at its cap.
    pub files_truncated: bool,
    /// Recent commits, newest first; None: no Git (`issue` says why, `reason` in git's words).
    pub commits: Option<Vec<CommitFiles>>,
    pub issue: Option<GitIssue>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainCoverage {
    pub id: String,
    pub path: String,
    pub counts: DomainCounts,
    /// Owned files; None without a readable root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    /// Commits since `verifiedMs` that touched an owned path; None without Git.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<CommitFiles>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UncoveredDir {
    pub path: String,
    pub files: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageView {
    /// Whether drift and the uncovered directories could be computed.
    pub git: bool,
    /// Why not, when `git` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<GitIssue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub domains: Vec<DomainCoverage>,
    pub unfiled: DomainCounts,
    pub uncovered: Vec<UncoveredDir>,
    pub files_truncated: bool,
}

/// Coverage is computed, never written (SPEC §memory.domains).
pub fn coverage(domains: &[Domain], memories: &[Memory], checkout: &Checkout) -> CoverageView {
    let owners = Map::new(domains).owners();
    let (by_domain, unfiled) = counts(memories);
    let mut seen: HashMap<&str, Option<&str>> = HashMap::new();
    let mut owned_files: HashMap<&str, usize> = HashMap::new();
    let mut uncovered: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(files) = &checkout.files {
        for file in files {
            match owner_id(&owners, &mut seen, file) {
                Some(owner) => *owned_files.entry(owner).or_default() += 1,
                None if checkout.commits.is_some() => {
                    let dir = match file.rfind('/') {
                        Some(at) => &file[..at],
                        None => continue, // a root file is no directory to own
                    };
                    let two: Vec<&str> = dir.split('/').take(2).collect();
                    *uncovered.entry(two.join("/")).or_default() += 1;
                }
                None => {}
            }
        }
    }
    // The domains each commit touched, read once per commit rather than once per domain.
    let touched: Vec<HashSet<&str>> = checkout
        .commits
        .iter()
        .flatten()
        .map(|c| c.files.iter().filter_map(|f| owner_id(&owners, &mut seen, f)).collect())
        .collect();
    let mut out_domains = Vec::with_capacity(domains.len());
    for domain in domains {
        let (drift, latest) = match &checkout.commits {
            None => (None, None),
            Some(commits) => {
                let touched: Vec<&CommitFiles> = commits
                    .iter()
                    .zip(&touched)
                    .filter(|(c, ids)| c.at_ms > domain.verified_ms && ids.contains(domain.id.as_str()))
                    .map(|(c, _)| c)
                    .collect();
                (Some(touched.len()), touched.first().map(|c| (*c).clone()))
            }
        };
        out_domains.push(DomainCoverage {
            id: domain.id.clone(),
            path: domain.path.clone(),
            counts: by_domain.get(&domain.id).copied().unwrap_or_default(),
            files: checkout.files.as_ref().map(|_| owned_files.get(domain.id.as_str()).copied().unwrap_or(0)),
            drift,
            latest,
        });
    }
    let mut dirs: Vec<UncoveredDir> = uncovered.into_iter().map(|(path, files)| UncoveredDir { path, files }).collect();
    dirs.sort_by(|a, b| b.files.cmp(&a.files).then_with(|| a.path.cmp(&b.path)));
    dirs.truncate(UNCOVERED_MAX);
    CoverageView {
        git: checkout.commits.is_some(),
        // A history read leaves no issue; one missing without a cause is a failure.
        issue: match checkout.commits {
            Some(_) => None,
            None => Some(checkout.issue.unwrap_or(GitIssue::GitFailed)),
        },
        reason: checkout.reason.clone(),
        domains: out_domains,
        unfiled,
        uncovered: dirs,
        files_truncated: checkout.files_truncated,
    }
}

/// `path`'s owner by id, each path looked up once: a lookup is a pass over every
/// domain's owns, and a commit's paths repeat the checkout's files.
fn owner_id<'p, 'd>(owners: &Owners<'d>, seen: &mut HashMap<&'p str, Option<&'d str>>, path: &'p str) -> Option<&'d str> {
    *seen.entry(path).or_insert_with(|| owners.owner(path).map(|d| d.id.as_str()))
}

// --- views ----------------------------------------------------------------------------------

/// One domain on the wire. `body` only where the hub is read (a domain's own read).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainView {
    pub id: String,
    pub scope: String,
    pub path: String,
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub owns: Vec<String>,
    /// The terms it is searched by, in English and the team's language.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    pub origin: crate::model::Origin,
    pub actor: String,
    pub confirmed: bool,
    pub revision: u64,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub verified_ms: i64,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub sub_domains: usize,
    pub counts: DomainCounts,
}

impl DomainView {
    /// Its line on the map.
    pub fn line(&self) -> String {
        line_of(&self.path, &self.title, &self.summary, self.confirmed, self.counts)
    }

    pub fn of(domain: &Domain, map: &Map<'_>, counts: DomainCounts, with_body: bool) -> DomainView {
        DomainView {
            id: domain.id.clone(),
            scope: domain.scope.clone(),
            path: domain.path.clone(),
            title: domain.title.clone(),
            summary: domain.summary.clone(),
            body: with_body.then(|| domain.body.clone()),
            owns: domain.owns.clone(),
            keywords: domain.keywords.clone(),
            origin: domain.origin,
            actor: domain.actor.clone(),
            confirmed: domain.confirmed,
            revision: domain.revision,
            created_ms: domain.created_ms,
            updated_ms: domain.updated_ms,
            verified_ms: domain.verified_ms,
            depth: domain.depth(),
            parent: domain.parent().map(str::to_string),
            sub_domains: map.children(&domain.path).count(),
            counts,
        }
    }
}

/// The map: every domain in tree order and the unfiled notes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainList {
    pub project: String,
    pub items: Vec<DomainView>,
    pub unfiled: DomainCounts,
}

/// A domain step on a path (ancestors).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainRef {
    pub id: String,
    pub path: String,
    pub title: String,
}

/// One domain in full: its hub, where it sits, its sub-domains and the notes filed there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainDetail {
    #[serde(flatten)]
    pub domain: DomainView,
    pub ancestors: Vec<DomainRef>,
    pub children: Vec<DomainView>,
    /// The live notes filed in this domain itself.
    pub notes: Vec<Summary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<DomainRevision>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutcome {
    /// Notes that moved (one already there does not count).
    pub moved: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<DomainRef>,
}

/// A domain confirmed whole: what a person took on in one go, and what still waits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeConfirmed {
    pub domain: DomainView,
    /// Domains confirmed now: this one and the ones under it that were not yet.
    pub domains: u64,
    /// Live notes confirmed now.
    pub notes: u64,
    /// Unconfirmed notes that need review, left for a verdict: the code check or a reader
    /// doubted them, so a click on the domain does not vouch for them.
    pub held: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub path: String,
    /// Notes re-filed in the parent (or left unfiled at the top level).
    pub refiled: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Which protection a domain delete runs under.
pub(crate) enum DeleteGuard {
    /// CLI and panel: the caller echoes the domain's path.
    Confirm(String),
    /// The redraw: the protection re-derived from the store as it is now.
    Redraw,
}

// --- requests -------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DomainRequest {
    pub path: String,
    pub title: String,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub owns: Vec<String>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DomainUpdate {
    pub expected_revision: u64,
    /// A new path moves the domain and its sub-domains.
    pub path: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub owns: Option<Vec<String>>,
    /// Replaces the keywords; `[]` clears them.
    pub keywords: Option<Vec<String>>,
    pub reason: Option<String>,
}

/// Keywords checked like the model says and for secrets.
pub(crate) fn clean_keywords(raw: &[String]) -> MemResult<Vec<String>> {
    let keywords = crate::model::check_keywords(raw).map_err(|err| MemError::invalid(format!("keywords: {err}")))?;
    for keyword in &keywords {
        check_secret("keywords", keyword)?;
    }
    Ok(keywords)
}

pub(crate) fn clean_domain_title(raw: &str) -> MemResult<String> {
    let title = raw.trim();
    if title.is_empty() {
        return Err(MemError::invalid("title is required"));
    }
    if title.contains(['\n', '\r']) {
        return Err(MemError::invalid("title must be a single line"));
    }
    let n = title.chars().count();
    if n > DOMAIN_TITLE_MAX {
        return Err(MemError::invalid(format!("title is {n} characters; the limit is {DOMAIN_TITLE_MAX}")));
    }
    check_secret("title", title)?;
    Ok(title.to_string())
}

pub(crate) fn clean_summary(raw: &str) -> MemResult<String> {
    let summary = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = summary.chars().count();
    if n > DOMAIN_SUMMARY_MAX {
        return Err(MemError::invalid(format!("summary is {n} characters; the limit is {DOMAIN_SUMMARY_MAX}")));
    }
    check_secret("summary", &summary)?;
    Ok(summary)
}

pub(crate) fn clean_body(raw: &str) -> MemResult<String> {
    let body = raw.replace("\r\n", "\n").trim().to_string();
    if body.len() > DOMAIN_BODY_MAX {
        return Err(MemError::invalid(format!(
            "body is {} bytes; the limit is {DOMAIN_BODY_MAX} (split a larger domain into sub-domains)",
            body.len()
        )));
    }
    check_secret("body", &body)?;
    Ok(body)
}

pub(crate) fn clean_owns(raw: &[String]) -> MemResult<Vec<String>> {
    if raw.len() > DOMAIN_OWNS_MAX {
        return Err(MemError::invalid(format!("owns: at most {DOMAIN_OWNS_MAX}")));
    }
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for entry in raw {
        let path = clean_anchor_path(entry).map_err(|e| MemError::invalid(e.message.replacen("anchors", "owns", 1)))?;
        if !out.contains(&path) {
            out.push(path);
        }
    }
    Ok(out)
}

fn clean_path(raw: &str) -> MemResult<String> {
    check_domain_path(raw).map_err(MemError::invalid)
}

fn snapshot_revision(domain: &Domain, op: &str, actor: &str, reason: Option<String>, now: i64) -> DomainRevision {
    DomainRevision {
        revision: domain.revision,
        op: op.to_string(),
        actor: actor.to_string(),
        reason,
        at_ms: now,
        snapshot: DomainSnapshot::of(domain),
    }
}

/// `memory_not_found` naming the map's paths, so the caller can pick one.
fn unknown_domain(selector: &str, domains: &[Domain]) -> MemError {
    if domains.is_empty() {
        return MemError::not_found(format!(
            "no domain {selector}: this project has no map yet (create a domain with memory_domain_save, \
             `swiss memory domain add` or the panel)"
        ));
    }
    let paths: Vec<&str> = domains.iter().map(|d| d.path.as_str()).take(NAMED_PATHS_MAX).collect();
    let more = domains.len().saturating_sub(paths.len());
    let mut list = paths.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    MemError::not_found(format!("no domain {selector} in this project; the map has {list}")).with("paths", json!(paths))
}

/// A domain of the scope by id or path.
pub(crate) async fn resolve(conn: &mut SqliteConnection, project_id: &str, selector: &str) -> MemResult<Domain> {
    if project_id == GLOBAL_PROJECT_ID {
        return Err(MemError::invalid("the global scope has no domains: its notes hold across projects"));
    }
    let selector = selector.trim().trim_matches('/');
    let domains = store::domains(conn, project_id).await?;
    let found = if is_domain_id(selector) {
        domains.iter().find(|d| d.id == selector)
    } else {
        domains.iter().find(|d| d.path == selector)
    };
    found.cloned().ok_or_else(|| unknown_domain(selector, &domains))
}

/// A read's domain selector - an id, a path, or `none` - as a filter over a subtree.
pub(crate) async fn filter(conn: &mut SqliteConnection, project_id: &str, selector: Option<&str>) -> MemResult<DomainFilter> {
    let Some(selector) = selector.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DomainFilter::Any);
    };
    if selector == "none" {
        return Ok(DomainFilter::Unfiled);
    }
    let domain = resolve(conn, project_id, selector).await?;
    let domains = store::domains(conn, project_id).await?;
    Ok(DomainFilter::In(Map::new(&domains).subtree(&domain)))
}

/// Where a new or re-anchored note goes when no domain was named: its anchors decide.
pub(crate) async fn auto_file(conn: &mut SqliteConnection, project_id: &str, anchors: &[Anchor]) -> MemResult<Option<Domain>> {
    if project_id == GLOBAL_PROJECT_ID || anchors.is_empty() {
        return Ok(None);
    }
    let domains = store::domains(conn, project_id).await?;
    Ok(Map::new(&domains).file_by_anchors(anchors).cloned())
}

fn conflict(domain: &Domain, expected: u64) -> MemError {
    MemError::new(
        Code::RevisionConflict,
        format!("{} is at revision {}, not {expected}; re-read it and retry", domain.path, domain.revision),
    )
    .with("current", json!(domain.revision))
}

fn taken(path: &str) -> MemError {
    MemError::new(Code::DomainExists, format!("this project already has a domain {path}")).with("path", json!(path))
}

// --- service --------------------------------------------------------------------------------

impl MemoryService {
    /// The map of one project (SPEC §memory.domains): every domain with its counts, and the
    /// unfiled notes.
    pub async fn domains(&self, target: &Target) -> MemResult<DomainList> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok(DomainList { project: String::new(), items: Vec::new(), unfiled: DomainCounts::default() });
        };
        let domains = store::domains(&mut conn, &project_id).await?;
        let scope = self.scope(&mut conn, &project_id).await?;
        let (by_domain, unfiled) = counts(&scope.all);
        let map = Map::new(&domains);
        let items = domains
            .iter()
            .map(|d| DomainView::of(d, &map, by_domain.get(&d.id).copied().unwrap_or_default(), false))
            .collect();
        Ok(DomainList { project: project_id, items, unfiled })
    }

    /// One domain in full, by id or path; its revisions when asked.
    pub async fn domain(&self, target: &Target, selector: &str, history: bool) -> MemResult<DomainDetail> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_id = place.project_id().map_err(|_| unknown_domain(selector.trim(), &[]))?;
        let domain = resolve(&mut conn, &project_id, selector).await?;
        let domains = store::domains(&mut conn, &project_id).await?;
        let scope = self.scope(&mut conn, &project_id).await?;
        let (memories, net) = (&scope.all, &scope.net);
        let (by_domain, _) = counts(memories);
        let count = |d: &Domain| by_domain.get(&d.id).copied().unwrap_or_default();
        let map = Map::new(&domains);
        let now = store::now();
        let mut checker = Checker::disabled();
        let mut filed: Vec<&Memory> =
            memories.iter().filter(|m| m.status.is_live() && m.domain.as_deref() == Some(domain.id.as_str())).collect();
        filed.sort_by(|a, b| b.confirmed.cmp(&a.confirmed).then_with(|| b.updated_ms.cmp(&a.updated_ms)).then_with(|| a.id.cmp(&b.id)));
        let notes = filed
            .into_iter()
            .map(|m| {
                let (marks, _) = graph::markers(m, net, &mut checker, now);
                view::summary(m, &marks, net, now, false)
            })
            .collect();
        let history = if history { Some(store::domain_revisions(&mut conn, &domain.id).await?) } else { None };
        Ok(DomainDetail {
            domain: DomainView::of(&domain, &map, count(&domain), true),
            ancestors: map
                .ancestors(&domain)
                .into_iter()
                .map(|d| DomainRef { id: d.id.clone(), path: d.path.clone(), title: d.title.clone() })
                .collect(),
            children: map.children(&domain.path).map(|d| DomainView::of(d, &map, count(d), false)).collect(),
            notes,
            history,
        })
    }

    /// Create a domain. An unregistered checkout registers like a first save.
    pub async fn create_domain(&self, target: &Target, actor: &Actor, request: DomainRequest) -> MemResult<DomainView> {
        let path = clean_path(&request.path)?;
        let title = clean_domain_title(&request.title)?;
        let summary = clean_summary(request.summary.as_deref().unwrap_or_default())?;
        let body = clean_body(request.body.as_deref().unwrap_or_default())?;
        let owns = clean_owns(&request.owns)?;
        let keywords = clean_keywords(&request.keywords)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let (place, _) = self.place_for_write(&mut conn, target).await?;
            let project_id = place.project_id()?;
            if project_id == GLOBAL_PROJECT_ID {
                return Err(MemError::invalid("the global scope has no domains: its notes hold across projects"));
            }
            let domains = store::domains(&mut conn, &project_id).await?;
            if domains.iter().any(|d| d.path == path) {
                return Err(taken(&path));
            }
            if let Some(parent) = domain_parent(&path) {
                if !domains.iter().any(|d| d.path == parent) {
                    return Err(MemError::invalid(format!("{path} needs its parent {parent}; create {parent} first")));
                }
            }
            if domains.len() >= DOMAINS_PER_PROJECT_MAX {
                return Err(MemError::invalid(format!("a project holds at most {DOMAINS_PER_PROJECT_MAX} domains")));
            }
            let now = store::now();
            let domain = Domain {
                id: new_domain_id(),
                scope: project_id.clone(),
                path: path.clone(),
                title: title.clone(),
                summary: summary.clone(),
                body: body.clone(),
                owns: owns.clone(),
                keywords: keywords.clone(),
                origin: actor.origin,
                actor: actor.label.clone(),
                confirmed: actor.confirms(),
                revision: 1,
                created_ms: now,
                updated_ms: now,
                verified_ms: now,
            };
            store::insert_domain(&mut conn, &domain).await?;
            store::insert_domain_revision(&mut conn, &domain.id, &snapshot_revision(&domain, "create", &actor.label, None, now))
                .await?;
            Ok(domain)
        }
        .await;
        let domain = store::finish(&mut conn, outcome).await?;
        self.domain_view(&mut conn, &domain).await
    }

    /// Edit a domain (CAS on `expectedRevision`). A new path moves it with its sub-domains.
    pub async fn update_domain(
        &self,
        target: &Target,
        actor: &Actor,
        selector: &str,
        request: DomainUpdate,
    ) -> MemResult<DomainView> {
        if request.path.is_none()
            && request.title.is_none()
            && request.summary.is_none()
            && request.body.is_none()
            && request.owns.is_none()
            && request.keywords.is_none()
        {
            return Err(MemError::invalid("nothing to update: pass path, title, summary, body, owns or keywords"));
        }
        let new_path = request.path.as_deref().map(clean_path).transpose()?;
        let title = request.title.as_deref().map(clean_domain_title).transpose()?;
        let summary = request.summary.as_deref().map(clean_summary).transpose()?;
        let body = request.body.as_deref().map(clean_body).transpose()?;
        let owns = request.owns.as_deref().map(clean_owns).transpose()?;
        let keywords = request.keywords.as_deref().map(clean_keywords).transpose()?;
        let reason = clean_reason(request.reason.as_deref(), false)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| unknown_domain(selector.trim(), &[]))?;
            let mut domain = resolve(&mut conn, &project_id, selector).await?;
            if domain.revision != request.expected_revision {
                return Err(conflict(&domain, request.expected_revision));
            }
            let now = store::now();
            let old_path = domain.path.clone();
            let mut moved: Vec<Domain> = Vec::new();
            if let Some(to) = new_path.as_ref().filter(|to| **to != old_path) {
                let domains = store::domains(&mut conn, &project_id).await?;
                if domain_is_under(to, &old_path) {
                    return Err(MemError::invalid(format!("{old_path} cannot move under itself ({to})")));
                }
                if domains.iter().any(|d| d.path == *to) {
                    return Err(taken(to));
                }
                if let Some(parent) = domain_parent(to) {
                    if !domains.iter().any(|d| d.path == parent) {
                        return Err(MemError::invalid(format!("{to} needs its parent {parent}; create {parent} first")));
                    }
                }
                for sub in domains.iter().filter(|d| domain_is_under(&d.path, &old_path)) {
                    let mut sub = sub.clone();
                    sub.path = format!("{to}{}", &sub.path[old_path.len()..]);
                    if domain_depth(&sub.path) > DOMAIN_DEPTH_MAX {
                        return Err(MemError::invalid(format!(
                            "moving {old_path} to {to} would put {} {} levels deep; a map has at most {DOMAIN_DEPTH_MAX}",
                            sub.path,
                            domain_depth(&sub.path)
                        )));
                    }
                    if domains.iter().any(|d| d.path == sub.path) {
                        return Err(taken(&sub.path));
                    }
                    moved.push(sub);
                }
                domain.path = to.clone();
            }
            let mut verified = false;
            if let Some(title) = title {
                domain.title = title;
            }
            if let Some(summary) = summary {
                domain.summary = summary;
            }
            if let Some(body) = body {
                verified |= body != domain.body;
                domain.body = body;
            }
            if let Some(owns) = owns {
                verified |= owns != domain.owns;
                domain.owns = owns;
            }
            if let Some(keywords) = keywords {
                domain.keywords = keywords;
            }
            if verified {
                domain.verified_ms = now;
            }
            domain.revision += 1;
            domain.updated_ms = now;
            // Parents first: a sub-domain's new path never collides with one not yet moved.
            store::update_domain(&mut conn, &domain).await?;
            let op = if domain.path != old_path { "move" } else { "edit" };
            store::insert_domain_revision(&mut conn, &domain.id, &snapshot_revision(&domain, op, &actor.label, reason.clone(), now))
                .await?;
            for mut sub in moved {
                sub.revision += 1;
                sub.updated_ms = now;
                store::update_domain(&mut conn, &sub).await?;
                let why = Some(format!("moved with {old_path}"));
                store::insert_domain_revision(&mut conn, &sub.id, &snapshot_revision(&sub, "move", &actor.label, why, now))
                    .await?;
            }
            Ok(domain)
        }
        .await;
        let domain = store::finish(&mut conn, outcome).await?;
        self.domain_view(&mut conn, &domain).await
    }

    /// A person confirms a domain (CLI and panel only). Idempotent.
    pub async fn confirm_domain(&self, target: &Target, actor: &Actor, selector: &str) -> MemResult<DomainView> {
        self.stamp_domain(target, actor, selector, true).await
    }

    /// A person confirms a domain whole (CLI and panel only): it, every domain under it and
    /// their unconfirmed active notes, in one transaction. A note that needs review is left
    /// unconfirmed and counted - it waits for a verdict. Idempotent.
    pub async fn confirm_tree(&self, target: &Target, actor: &Actor, selector: &str) -> MemResult<TreeConfirmed> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| unknown_domain(selector.trim(), &[]))?;
            let mut root = resolve(&mut conn, &project_id, selector).await?;
            let domains = store::domains(&mut conn, &project_id).await?;
            let tree = Map::new(&domains).subtree(&root);
            let now = store::now();
            let reason = Some(format!("confirmed with {}", root.path));
            let (mut confirmed, mut notes, mut held) = (0, 0, 0);
            for domain in domains.iter().filter(|d| tree.contains(&d.id) && !d.confirmed) {
                let mut domain = domain.clone();
                domain.confirmed = true;
                domain.revision += 1;
                domain.updated_ms = now;
                store::update_domain(&mut conn, &domain).await?;
                let why = if domain.id == root.id { None } else { reason.clone() };
                store::insert_domain_revision(&mut conn, &domain.id, &snapshot_revision(&domain, "confirm", &actor.label, why, now))
                    .await?;
                if domain.id == root.id {
                    root = domain;
                }
                confirmed += 1;
            }
            let waiting = store::Filter { unconfirmed_only: true, ..store::Filter::default() };
            for mut memory in store::memories(&mut conn, &project_id, &waiting).await? {
                if !memory.domain.as_ref().is_some_and(|id| tree.contains(id)) || !memory.status.is_live() {
                    continue;
                }
                if memory.status == Status::NeedsReview {
                    held += 1;
                    continue;
                }
                memory.confirmed = true;
                memory.revision += 1;
                memory.updated_ms = now;
                store::update_memory(&mut conn, &memory).await?;
                store::insert_revision(&mut conn, &memory.id, &revision_of(&memory, "confirm", actor, reason.clone(), now)).await?;
                notes += 1;
            }
            Ok((root, confirmed, notes, held))
        }
        .await;
        let (root, domains, notes, held) = store::finish(&mut conn, outcome).await?;
        Ok(TreeConfirmed { domain: self.domain_view(&mut conn, &root).await?, domains, notes, held })
    }

    /// The hub is true to the code again (anyone): `verifiedMs` is now.
    pub async fn verify_domain(&self, target: &Target, actor: &Actor, selector: &str) -> MemResult<DomainView> {
        self.stamp_domain(target, actor, selector, false).await
    }

    async fn stamp_domain(&self, target: &Target, actor: &Actor, selector: &str, confirm: bool) -> MemResult<DomainView> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| unknown_domain(selector.trim(), &[]))?;
            let mut domain = resolve(&mut conn, &project_id, selector).await?;
            if confirm && domain.confirmed {
                return Ok(domain);
            }
            let now = store::now();
            if confirm {
                domain.confirmed = true;
            } else {
                domain.verified_ms = now;
            }
            domain.revision += 1;
            domain.updated_ms = now;
            store::update_domain(&mut conn, &domain).await?;
            let op = if confirm { "confirm" } else { "verify" };
            store::insert_domain_revision(&mut conn, &domain.id, &snapshot_revision(&domain, op, &actor.label, None, now))
                .await?;
            Ok(domain)
        }
        .await;
        let domain = store::finish(&mut conn, outcome).await?;
        self.domain_view(&mut conn, &domain).await
    }

    /// Delete a domain (CLI and panel only, the caller types its path). Refused while
    /// sub-domains remain; its notes go to the parent, or unfiled at the top level.
    pub async fn delete_domain(&self, target: &Target, selector: &str, confirm: &str) -> MemResult<DeleteOutcome> {
        self.delete_domain_guarded(target, selector, &DeleteGuard::Confirm(confirm.to_string())).await
    }

    /// Delete one domain the redraw owns (SPEC §memory.map Redraw): the ordinary delete's
    /// refiling, under the redraw's protection re-derived inside this write transaction
    /// ([crate::init::redraw_candidates_now]). A plan is a question asked before a model
    /// spends minutes answering; the answer never outlives the protection it was promised
    /// under - a domain a person confirmed, edited or drew since the plan was cut is kept
    /// and reported, and so is a parent whose sub-domain stayed.
    pub async fn delete_domain_redraw(&self, target: &Target, selector: &str) -> MemResult<DeleteOutcome> {
        self.delete_domain_guarded(target, selector, &DeleteGuard::Redraw).await
    }

    /// The body both deletes share: the guard first, then the ordinary sub-domain
    /// protection, the refile and the delete, in the one transaction.
    async fn delete_domain_guarded(&self, target: &Target, selector: &str, guard: &DeleteGuard) -> MemResult<DeleteOutcome> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| unknown_domain(selector.trim(), &[]))?;
            let domain = resolve(&mut conn, &project_id, selector).await?;
            match guard {
                DeleteGuard::Confirm(confirm) => {
                    if confirm.trim() != domain.path {
                        return Err(MemError::invalid(format!("confirm: type the path ({}) to delete the domain", domain.path)));
                    }
                }
                DeleteGuard::Redraw => {
                    // Re-derived here, from the rows this transaction sees - never from
                    // the plan, which the world may have moved under while it waited.
                    let candidates = crate::init::redraw_candidates_now(&mut conn, &project_id).await?;
                    if !candidates.contains(&domain.path) {
                        return Err(MemError::invalid(format!(
                            "{} kept: someone confirmed, edited or drew it since the redraw planned it, or a sub-domain stayed;                              only a domain sync drew and nobody touched is redrawn",
                            domain.path
                        )));
                    }
                }
            }
            let domains = store::domains(&mut conn, &project_id).await?;
            let subs: Vec<&str> =
                domains.iter().filter(|d| domain_is_under(&d.path, &domain.path)).map(|d| d.path.as_str()).collect();
            if !subs.is_empty() {
                return Err(MemError::invalid(format!(
                    "{} has sub-domains ({}); move or delete them first",
                    domain.path,
                    subs.join(", ")
                )));
            }
            let parent = domain.parent().and_then(|p| domains.iter().find(|d| d.path == p));
            let refiled = store::refile(&mut conn, &domain.id, parent.map(|p| p.id.as_str())).await?;
            store::delete_domain(&mut conn, &domain.id).await?;
            Ok(DeleteOutcome { path: domain.path.clone(), refiled, to: parent.map(|p| p.path.clone()) })
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// File notes by hand (`domain` None: unfile them). Any status: history keeps its place.
    pub async fn file(&self, target: &Target, ids: &[String], domain: Option<&str>) -> MemResult<FileOutcome> {
        if ids.is_empty() {
            return Err(MemError::invalid("ids: name at least one note"));
        }
        if ids.len() > FILE_IDS_MAX {
            return Err(MemError::invalid(format!("ids: at most {FILE_IDS_MAX} per call")));
        }
        for id in ids {
            crate::service::check_record_id("ids", id)?;
        }
        let domain = domain.map(str::trim).filter(|d| !d.is_empty() && *d != "none");
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| MemError::not_found(format!("no note {} in this project", ids[0])))?;
            let to = match domain {
                Some(selector) => Some(resolve(&mut conn, &project_id, selector).await?),
                None => None,
            };
            let mut moved = 0;
            for id in ids {
                in_scope(&mut conn, &project_id, id).await?;
                if store::file_memory(&mut conn, id, to.as_ref().map(|d| d.id.as_str())).await? {
                    moved += 1;
                }
            }
            Ok(FileOutcome {
                moved,
                domain: to.map(|d| DomainRef { id: d.id.clone(), path: d.path.clone(), title: d.title.clone() }),
            })
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// File every unfiled live note by its anchors: the count filed. A note filed by hand
    /// is never moved.
    pub async fn autofile(&self, target: &Target) -> MemResult<u64> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let Ok(project_id) = place.project_id() else {
                return Ok(0);
            };
            let domains = store::domains(&mut conn, &project_id).await?;
            if domains.is_empty() {
                return Ok(0);
            }
            let map = Map::new(&domains);
            let (memories, _) = load_scope(&mut conn, &project_id).await?;
            let mut filed = 0;
            for memory in memories.iter().filter(|m| m.status.is_live() && m.domain.is_none()) {
                if let Some(domain) = map.file_by_anchors(&memory.anchors) {
                    if store::file_memory(&mut conn, &memory.id, Some(&domain.id)).await? {
                        filed += 1;
                    }
                }
            }
            Ok(filed)
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// Coverage (SPEC §memory.domains) over what the caller read from the checkout.
    /// Coverage on the blocking pool: it reads every file of the checkout against every
    /// owns entry, which on a large repository would hold the runtime thread.
    pub async fn coverage(&self, target: &Target, checkout: Checkout) -> MemResult<CoverageView> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok(coverage(&[], &[], &checkout));
        };
        let domains = store::domains(&mut conn, &project_id).await?;
        let scope = self.scope(&mut conn, &project_id).await?;
        drop(conn);
        tokio::task::spawn_blocking(move || coverage(&domains, &scope.all, &checkout))
            .await
            .map_err(|err| MemError::store(format!("coverage failed: {err}")))
    }

    async fn domain_view(&self, conn: &mut SqliteConnection, domain: &Domain) -> MemResult<DomainView> {
        let domains = store::domains(conn, &domain.scope).await?;
        let (memories, _) = load_scope(conn, &domain.scope).await?;
        let (by_domain, _) = counts(&memories);
        Ok(DomainView::of(domain, &Map::new(&domains), by_domain.get(&domain.id).copied().unwrap_or_default(), true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;

    fn domain(id: &str, path: &str, owns: &[&str]) -> Domain {
        Domain {
            id: id.into(),
            scope: "p-00000001".into(),
            path: path.into(),
            title: path.to_uppercase(),
            summary: String::new(),
            body: String::new(),
            owns: owns.iter().map(|o| o.to_string()).collect(),
            keywords: Vec::new(),
            origin: Origin::User,
            actor: "panel".into(),
            confirmed: true,
            revision: 1,
            created_ms: 0,
            updated_ms: 0,
            verified_ms: 100,
        }
    }

    fn anchors(paths: &[&str]) -> Vec<Anchor> {
        paths.iter().map(|p| Anchor { path: p.to_string(), glob: p.contains('*'), hash: None }).collect()
    }

    #[test]
    fn owns_matches_globs_and_directories() {
        assert!(owns_path("crates/swiss-memory", "crates/swiss-memory/src/store.rs"));
        assert!(owns_path("crates/swiss-memory/", "crates/swiss-memory"));
        assert!(!owns_path("crates/swiss-memory", "crates/swiss-memory-x/lib.rs"));
        assert!(owns_path("src/plugins/memory*.rs", "src/plugins/memory_api.rs"));
        assert!(owns_path("crates/*", "crates/swiss-panel/panel/src/app.ts"));
        assert!(!owns_path("crates/*.rs", "crates/x/y.rs"));
        assert_eq!(literal_prefix("crates/*/src/*.rs"), "crates");
        assert_eq!(literal_prefix("src/plugins/memory*.rs"), "src/plugins");
        assert_eq!(literal_prefix("*.md"), "");
    }

    #[test]
    fn the_deepest_owner_takes_a_path() {
        let domains = vec![
            domain("d-00000001", "memory", &["crates/swiss-memory", "src/plugins/memory*.rs"]),
            domain("d-00000002", "memory/store", &["crates/swiss-memory/src/store.rs"]),
            domain("d-00000003", "panel", &["crates/swiss-panel"]),
        ];
        let map = Map::new(&domains);
        assert_eq!(map.owner("crates/swiss-memory/src/store.rs").map(|d| d.path.as_str()), Some("memory/store"));
        assert_eq!(map.owner("crates/swiss-memory/src/brief.rs").map(|d| d.path.as_str()), Some("memory"));
        assert_eq!(map.owner("README.md"), None);
        assert_eq!(map.subtree(&domains[0]).len(), 2);
        assert_eq!(map.children("memory").count(), 1);
        assert_eq!(map.ancestors(&domains[1]).len(), 1);
    }

    #[test]
    fn owners_made_ready_once_answer_as_the_map_does() {
        let domains = vec![
            domain("d-00000001", "app", &["src/main/java/com/x/**/Order*.java", "src/main/java/com/x/**/*Pay*.java", "src/main"]),
            domain("d-00000002", "app/pay", &["src/main/java/com/x/**/*Pay*.java", "src/main/resources/mybatis/Pay*.xml"]),
            domain("d-00000003", "docs", &["docs/", "docs/*/api"]),
            domain("d-00000004", "web", &["web/**", "web/src/?/*.ts"]),
        ];
        let map = Map::new(&domains);
        let owners = map.owners();
        let paths = [
            "src/main/java/com/x/order/OrderService.java", "src/main/java/com/x/PayGate.java", "src/main/java/com/x/a/b/CardPayCallback.java",
            "src/main/java/com/x/user/UserService.java", "src/main/resources/mybatis/PayMapper.xml", "src/test/X.java", "docs",
            "docs/v1/api/index.md", "docs/v1/guide.md", "web/src/a/b.ts", "web", "README.md", "src/main/java/com/x/Pay.java/inner",
        ];
        for path in paths {
            assert_eq!(owners.owner(path).map(|d| &d.id), map.owner(path).map(|d| &d.id), "{path}");
        }
        assert_eq!(owners.owner("src/main/java/com/x/a/b/CardPayCallback.java").map(|d| d.path.as_str()), Some("app/pay"));
        assert_eq!(owners.owner("src/main/java/com/x/user/UserService.java").map(|d| d.path.as_str()), Some("app"));
        assert_eq!(owners.owner("docs/v1/api/index.md").map(|d| d.path.as_str()), Some("docs"));
    }

    #[test]
    fn a_note_goes_where_most_of_its_anchors_go() {
        let domains = vec![
            domain("d-00000001", "memory", &["crates/swiss-memory"]),
            domain("d-00000002", "memory/store", &["crates/swiss-memory/src/store.rs"]),
            domain("d-00000003", "memory/brief", &["crates/swiss-memory/src/brief.rs"]),
            domain("d-00000004", "panel", &["crates/swiss-panel"]),
        ];
        let map = Map::new(&domains);
        let at = |paths: &[&str]| map.file_by_anchors(&anchors(paths)).map(|d| d.path.clone());
        assert_eq!(at(&["crates/swiss-memory/src/store.rs"]), Some("memory/store".into()));
        // Two to one.
        assert_eq!(
            at(&["crates/swiss-memory/src/store.rs", "crates/swiss-memory/src/store.rs/x", "crates/swiss-panel/a.ts"]),
            Some("memory/store".into())
        );
        // A tie goes to the deepest common ancestor; none in common leaves it unfiled.
        assert_eq!(at(&["crates/swiss-memory/src/store.rs", "crates/swiss-memory/src/brief.rs"]), Some("memory".into()));
        assert_eq!(at(&["crates/swiss-memory/src/store.rs", "crates/swiss-panel/a.ts"]), None);
        // A glob by its literal prefix; no anchors or no owner: unfiled.
        assert_eq!(at(&["crates/swiss-memory/src/*.rs"]), Some("memory".into()));
        assert_eq!(at(&[]), None);
        assert_eq!(at(&["docs/SPEC.md"]), None);
    }

    #[test]
    fn coverage_counts_files_drift_and_what_no_domain_owns() {
        let domains = vec![
            domain("d-00000001", "memory", &["crates/swiss-memory"]),
            domain("d-00000002", "panel", &["crates/swiss-panel"]),
        ];
        let commit = |hash: &str, at: i64, files: &[&str]| CommitFiles {
            hash: hash.into(),
            at_ms: at,
            subject: format!("commit {hash}"),
            files: files.iter().map(|f| f.to_string()).collect(),
        };
        let checkout = Checkout {
            files: Some(
                ["crates/swiss-memory/src/a.rs", "crates/swiss-memory/src/b.rs", "src/app.rs", "src/plugins/x.rs", "README.md"]
                    .iter()
                    .map(|f| f.to_string())
                    .collect(),
            ),
            files_truncated: false,
            commits: Some(vec![
                commit("c3", 300, &["crates/swiss-memory/src/a.rs"]),
                commit("c2", 200, &["crates/swiss-memory/src/b.rs", "src/app.rs"]),
                commit("c1", 50, &["crates/swiss-memory/src/a.rs"]),
            ]),
            issue: None,
            reason: None,
        };
        let view = coverage(&domains, &[], &checkout);
        assert!(view.git && view.issue.is_none());
        assert_eq!(view.domains[0].files, Some(2));
        assert_eq!(view.domains[0].drift, Some(2), "c1 predates verifiedMs");
        assert_eq!(view.domains[0].latest.as_ref().map(|c| c.hash.as_str()), Some("c3"));
        assert_eq!(view.domains[1].drift, Some(0));
        let uncovered: Vec<(&str, usize)> = view.uncovered.iter().map(|u| (u.path.as_str(), u.files)).collect();
        assert_eq!(uncovered, [("src", 1), ("src/plugins", 1)]);
        let without_git = Checkout { commits: None, issue: Some(GitIssue::NotARepository), reason: Some("no git".into()), ..checkout };
        let without_git = coverage(&domains, &[], &without_git);
        assert!(!without_git.git);
        assert_eq!(without_git.issue, Some(GitIssue::NotARepository));
        let wire = serde_json::to_value(&without_git).expect("json");
        assert_eq!(wire["issue"], "not_a_repository", "{wire}");
        assert_eq!(GitIssue::parse("refused"), Some(GitIssue::Refused));
        assert_eq!(GitIssue::parse("nope"), None);
        assert_eq!(without_git.domains[0].files, Some(2));
        assert_eq!(without_git.domains[0].drift, None);
        assert!(without_git.uncovered.is_empty());
    }
}
