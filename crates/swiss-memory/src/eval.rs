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

//! The retrieval evaluation (SPEC §memory.evaluation): a fixture is a project's worth of
//! notes plus the queries a person would type to find them, each tagged with a
//! style. [load] writes a fixture into a store through the service (the same rules
//! every entry point obeys); [evaluate] runs its queries in one [Mode] and judges each
//! query's context - the first [K] hits of a search, or every note a brief shows - for
//! recall@1/3/5 and MRR@5, and for what the context carries that it should not: notes
//! filed apart from the expected ones, notes no checkable evidence verifies, stale notes,
//! notes the query marks wrong. Every report records the embedding setup it ran under.
//! Pure library code: the tests and any future `swiss memory eval` decide what numbers
//! they demand. A project's recorded misses export as a fixture ([crate::misses]), so a
//! real failure becomes a query the evaluation scores.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::domain::DomainRequest;
use crate::error::{MemError, MemResult};
use crate::evidence::EvidenceSpec;
use crate::model::{Actor, Kind, LinkKind, SourceType, Status};
use crate::search::SearchQuery;
use crate::service::{BriefRequest, LinkSpec, ListRequest, MemoryService, SaveRequest, Target};

/// The query styles a fixture may use.
pub const STYLES: [&str; 6] = ["keyword", "natural", "path", "short", "cross", "symbol"];

/// A search's context: its first K hits; recall and MRR count no further, in any mode.
pub const K: usize = 5;

/// One fixture file. It serializes as it parses: an export of recorded misses
/// ([crate::misses]) is a fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub about: String,
    pub project: FixtureProject,
    /// The project's map, parents before their sub-domains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<FixtureDomain>,
    pub notes: Vec<FixtureNote>,
    pub queries: Vec<FixtureQuery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureProject {
    pub name: String,
    /// `en` | `zh`.
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureDomain {
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureNote {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<FixtureLink>,
    /// The key of an earlier note this one supersedes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// The domain path to file the note in; absent, its anchors decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// `agent` saves through an MCP-style actor (unconfirmed); anything else is a
    /// person's note, saved as the panel does (confirmed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Evidence in the CLI's compact form (`verified_by=code:src/retry.rs#retry`); a
    /// cited file is created under the root like an anchor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// The note's anchor files change after it is saved: it reads stale. Every note
    /// anchored at those files reads stale with it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drift: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureLink {
    pub to: String,
    /// `supports` | `depends_on` | `contradicts` | `related_to`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureQuery {
    pub q: String,
    pub expect: Vec<String>,
    /// One of [STYLES].
    pub style: String,
    /// The files the asker works on: the brief's paths in [Mode::BriefPaths].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Notes whose presence makes the context wrong (a superseded rule, a misleading
    /// neighbour): such a context is not fully correct.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<String>,
}

impl Fixture {
    /// Parse and check a fixture: unique keys, known kinds, styles and link kinds,
    /// and every key reference resolving.
    pub fn parse(text: &str) -> Result<Fixture, String> {
        let fixture: Fixture = serde_json::from_str(text).map_err(|e| format!("fixture: {e}"))?;
        fixture.check()?;
        Ok(fixture)
    }

    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.project.language.as_str(), "en" | "zh") {
            return Err(format!("project.language: en or zh, not {}", self.project.language));
        }
        let mut paths: Vec<&str> = Vec::new();
        for domain in &self.domains {
            let path = crate::model::check_domain_path(&domain.path).map_err(|e| format!("domains: {e}"))?;
            if path != domain.path || paths.contains(&domain.path.as_str()) {
                return Err(format!("domains: {} is not a unique, normalized path", domain.path));
            }
            if let Some(parent) = crate::model::domain_parent(&domain.path) {
                if !paths.contains(&parent) {
                    return Err(format!("domains: {} needs its parent {parent} before it", domain.path));
                }
            }
            paths.push(domain.path.as_str());
        }
        let mut keys: HashMap<&str, usize> = HashMap::new();
        for (i, note) in self.notes.iter().enumerate() {
            if let Some(domain) = &note.domain {
                if !paths.contains(&domain.as_str()) {
                    return Err(format!("{}: unknown domain {domain}", note.key));
                }
            }
            if keys.insert(note.key.as_str(), i).is_some() {
                return Err(format!("notes: the key {} repeats", note.key));
            }
            if Kind::parse(&note.kind).is_none() {
                return Err(format!("{}: unknown kind {}", note.key, note.kind));
            }
            for anchor in &note.anchors {
                if !relative_inside(anchor) {
                    return Err(format!("{}: anchor {anchor} must be a relative path inside the root", note.key));
                }
            }
            for raw in &note.evidence {
                EvidenceSpec::parse(raw).map_err(|e| format!("{}: {}", note.key, e.message))?;
            }
            if note.drift && note.anchors.iter().all(|a| is_glob(a)) {
                return Err(format!("{}: drift needs an anchor file to change", note.key));
            }
        }
        for note in &self.notes {
            for link in &note.links {
                if !keys.contains_key(link.to.as_str()) {
                    return Err(format!("{}: link to unknown key {}", note.key, link.to));
                }
                match LinkKind::parse(&link.kind) {
                    Some(LinkKind::Supersedes) | None => {
                        return Err(format!("{}: link kind {} (supports, depends_on, contradicts, related_to)", note.key, link.kind))
                    }
                    Some(_) => {}
                }
            }
            if let Some(old) = &note.replaces {
                if !keys.contains_key(old.as_str()) || old == &note.key {
                    return Err(format!("{}: replaces unknown key {old}", note.key));
                }
            }
        }
        for query in &self.queries {
            if !STYLES.contains(&query.style.as_str()) {
                return Err(format!("query {:?}: unknown style {}", query.q, query.style));
            }
            if query.expect.is_empty() {
                return Err(format!("query {:?}: expects nothing", query.q));
            }
            for key in &query.expect {
                if !keys.contains_key(key.as_str()) {
                    return Err(format!("query {:?}: expects unknown key {key}", query.q));
                }
            }
            for key in &query.avoid {
                if !keys.contains_key(key.as_str()) {
                    return Err(format!("query {:?}: avoids unknown key {key}", query.q));
                }
            }
            for path in &query.paths {
                if !relative_inside(path) {
                    return Err(format!("query {:?}: path {path} must be a relative path inside the root", query.q));
                }
            }
        }
        Ok(())
    }
}

/// A relative path with no `..`, root or prefix: it stays under the root.
pub(crate) fn relative_inside(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// What [load] wrote: the project and each key's note id.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub project_id: String,
    pub ids: HashMap<String, String>,
}

fn create(root: &Path, path: &str) -> MemResult<()> {
    let file = root.join(path);
    if file.exists() {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MemError::invalid(format!("{}: {e}", parent.display())))?;
    }
    std::fs::write(&file, format!("// fixture file: {path}\n")).map_err(|e| MemError::invalid(format!("{}: {e}", file.display())))
}

/// A fixture note's id: the same every load, so ties among equal scores break the same way
/// run after run.
fn fixture_id(fixture: &Fixture, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = Sha256::digest(format!("{}\0{}\0{key}", fixture.about, fixture.project.name).as_bytes()).into();
    format!("m-{}", digest.iter().take(5).map(|b| format!("{b:02x}")).collect::<String>())
}

/// Write a fixture into the store under `root` (a project root: a Git working tree
/// or a bound directory). Missing anchor and evidence files are created under the root
/// first, so anchors baseline as fresh. The domains are created first, as a person
/// would in the panel; then notes are saved in array order with `allowSimilar`,
/// those with `replaces` after the rest (still in array order, a replacement waiting
/// for the note it replaces); links are added in a second pass; last, the files of the
/// notes marked `drift` change. Each note's id derives from its key and its clock is the
/// load's start plus its array index in milliseconds, so two loads order and break ties
/// alike: an evaluation reproduces.
pub async fn load(service: &MemoryService, root: &Path, fixture: &Fixture) -> MemResult<Loaded> {
    fixture.check().map_err(MemError::invalid)?;
    let start = crate::store::now();
    for note in &fixture.notes {
        for anchor in note.anchors.iter().filter(|a| !is_glob(a)) {
            create(root, anchor)?;
        }
        for raw in &note.evidence {
            let spec = EvidenceSpec::parse(raw)?;
            if let (SourceType::Code | SourceType::Test | SourceType::Doc, Some(path)) = (spec.source_type, &spec.path) {
                create(root, path)?;
            }
        }
    }
    let target = Target::dir(root.display().to_string());
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut project_id = String::new();
    for domain in &fixture.domains {
        let request = DomainRequest {
            path: domain.path.clone(),
            title: domain.title.clone(),
            summary: domain.summary.clone(),
            body: domain.body.clone(),
            owns: domain.owns.clone(),
            keywords: domain.keywords.clone(),
        };
        let created = service
            .create_domain(&target, &Actor::panel(), request)
            .await
            .map_err(|e| MemError { message: format!("domain {}: {}", domain.path, e.message), ..e })?;
        project_id = created.scope;
    }
    let mut queue: Vec<&FixtureNote> = fixture.notes.iter().filter(|n| n.replaces.is_none()).collect();
    queue.extend(fixture.notes.iter().filter(|n| n.replaces.is_some()));
    while !queue.is_empty() {
        let before = queue.len();
        let mut waiting = Vec::new();
        for note in queue {
            let replaces = match &note.replaces {
                Some(old) => match ids.get(old) {
                    Some(id) => Some(id.clone()),
                    None => {
                        waiting.push(note);
                        continue;
                    }
                },
                None => None,
            };
            let actor = if note.origin.as_deref() == Some("agent") { Actor::mcp(Some("eval")) } else { Actor::panel() };
            let request = SaveRequest {
                kind: Kind::parse(&note.kind).unwrap_or(Kind::Knowledge),
                title: note.title.clone(),
                content: note.content.clone(),
                anchors: note.anchors.clone(),
                refs: note.refs.clone(),
                keywords: note.keywords.clone(),
                links: Vec::new(),
                replaces,
                allow_similar: true,
                domain: note.domain.clone(),
                evidence: note.evidence.iter().map(|raw| EvidenceSpec::parse(raw)).collect::<MemResult<_>>()?,
            };
            let index = fixture.notes.iter().position(|n| n.key == note.key).unwrap_or(0) as i64;
            let saved = service
                .save_minted(&target, &actor, request, Some((fixture_id(fixture, &note.key), start + index)))
                .await
                .map_err(|e| MemError { message: format!("{}: {}", note.key, e.message), ..e })?;
            if project_id.is_empty() {
                project_id = saved.project.id.clone();
            }
            ids.insert(note.key.clone(), saved.record.summary.id.clone());
        }
        if waiting.len() == before {
            let keys: Vec<&str> = waiting.iter().map(|n| n.key.as_str()).collect();
            return Err(MemError::invalid(format!("replaces: a cycle among {}", keys.join(", "))));
        }
        queue = waiting;
    }
    if !project_id.is_empty() && service.project(&project_id).await?.name != fixture.project.name {
        service.rename_project(&project_id, &fixture.project.name).await?;
    }
    let linker = Actor::panel();
    for note in &fixture.notes {
        for link in &note.links {
            let (Some(from), Some(to)) = (ids.get(&note.key), ids.get(&link.to)) else { continue };
            let spec = LinkSpec { to: to.clone(), kind: LinkKind::parse(&link.kind).unwrap_or(LinkKind::RelatedTo), note: link.note.clone() };
            service
                .link(&target, &linker, from, &spec.to, spec.kind, spec.note.as_deref())
                .await
                .map_err(|e| MemError { message: format!("{} -> {}: {}", note.key, link.to, e.message), ..e })?;
        }
    }
    for note in fixture.notes.iter().filter(|n| n.drift) {
        for anchor in note.anchors.iter().filter(|a| !is_glob(a)) {
            let file = root.join(anchor);
            let mut text = std::fs::read_to_string(&file).map_err(|e| MemError::invalid(format!("{}: {e}", file.display())))?;
            text.push_str("// changed after the note was saved\n");
            std::fs::write(&file, text).map_err(|e| MemError::invalid(format!("{}: {e}", file.display())))?;
        }
    }
    Ok(Loaded { project_id, ids })
}

/// How a query's context is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// The word tiers alone, even with an embedder set: semantic OFF. The first [K] hits.
    Lexical,
    /// The search every entry point runs - the words fused with meaning when an embedder
    /// is set and the scope is indexed: semantic ON. The first [K] hits.
    Hybrid,
    /// The brief with the query as its task: every note it shows, in reading order.
    BriefTask,
    /// The brief with the query as its task and its `paths` (a query without paths runs
    /// as [Mode::BriefTask], so both brief modes score every query).
    BriefPaths,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Lexical, Mode::Hybrid, Mode::BriefTask, Mode::BriefPaths];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Lexical => "lexical",
            Mode::Hybrid => "hybrid",
            Mode::BriefTask => "brief-task",
            Mode::BriefPaths => "brief-paths",
        }
    }
}

/// The embedding setup a report ran under (the store's index status).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    /// The fingerprint, `provider:model:dims:ep-HASH[:doc-HASH]`; None: no embedder.
    pub fingerprint: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// `ep-HASH`: the endpoint, named without its host.
    pub endpoint: Option<String>,
    /// The index's one width.
    pub dims: Option<usize>,
    /// `off` | `ready` | `indexing` | `stale` | `error`.
    pub state: String,
    pub indexed: usize,
    pub pending: usize,
    pub incompatible: usize,
}

impl Setup {
    /// The provider, model and endpoint a fingerprint names; a model name may hold `:`.
    fn of(fingerprint: Option<String>) -> Setup {
        let Some(fingerprint) = fingerprint else { return Setup::default() };
        let parts: Vec<&str> = fingerprint.split(':').collect();
        let ep = parts.iter().position(|p| p.starts_with("ep-"));
        // The width precedes the endpoint; without one (an older fingerprint), it is last.
        let model_end = ep.map(|i| i.saturating_sub(1)).unwrap_or(parts.len().saturating_sub(1)).max(1);
        Setup {
            provider: parts.first().map(|p| p.to_string()),
            model: (parts.len() > 1).then(|| parts[1..model_end.min(parts.len())].join(":")).filter(|m| !m.is_empty()),
            endpoint: ep.map(|i| parts[i].to_string()),
            fingerprint: Some(fingerprint),
            ..Setup::default()
        }
    }

    /// One line: what the semantic tier ran with.
    pub fn line(&self) -> String {
        match &self.fingerprint {
            None => format!("embedding {}", self.state),
            Some(fingerprint) => format!(
                "embedding {} · {fingerprint} · dims {} · {} indexed, {} pending, {} of another width",
                self.state,
                self.dims.map(|d| d.to_string()).unwrap_or_else(|| "?".into()),
                self.indexed,
                self.pending,
                self.incompatible
            ),
        }
    }
}

async fn setup(service: &MemoryService) -> MemResult<Setup> {
    let status = service.embedding_status().await?;
    Ok(Setup {
        dims: status.dims,
        state: status.state.to_string(),
        indexed: status.indexed,
        pending: status.pending,
        incompatible: status.incompatible,
        ..Setup::of(status.model)
    })
}

/// The scores over a set of queries.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub queries: usize,
    /// Mean over queries of |expected ∩ the first n| / |expected|, n = 1, 3, 5.
    pub recall1: f64,
    pub recall3: f64,
    pub recall5: f64,
    /// Mean over queries of 1 / rank of the first expected note in the first five (0 if none).
    pub mrr5: f64,
    /// The notes of every context.
    pub items: usize,
    /// Of the context notes filed where some expected note's domain can judge them, those
    /// filed apart from every expected note's domain (neither it, above it nor under it).
    /// An unfiled note claims no domain and is not judged.
    pub wrong_domain: f64,
    pub wrong_domain_items: usize,
    pub domain_judged: usize,
    /// Of the context notes, those no checkable evidence (code, a test, a commit, a doc)
    /// verifies as they read now.
    pub unsupported: f64,
    /// Of the context notes, the stale ones: an anchor changed since it was checked, a
    /// dependency broken, or a status other than active.
    pub stale: f64,
    /// The queries whose context holds an expected note and nothing stale, filed apart or
    /// avoided. Support is the corpus's property, reported apart: it is not part of it.
    pub fully_correct: f64,
}

/// One query that did not find everything it expected in the first [K].
#[derive(Debug, Clone, Serialize)]
pub struct Miss {
    pub query: String,
    pub style: String,
    pub expected: Vec<String>,
    /// The first [K] of the context as fixture keys (a note the fixture does not know
    /// shows as its id).
    pub got: Vec<String>,
}

/// One evaluation run: one mode over every query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalReport {
    pub mode: Mode,
    pub setup: Setup,
    pub overall: Metrics,
    pub by_style: BTreeMap<String, Metrics>,
    pub misses: Vec<Miss>,
    /// Queries the semantic tier took part in.
    pub semantic_queries: usize,
}

/// The columns: recall and MRR, then the context - its mean size, the notes filed apart (a
/// share and a count: a smaller context raises the share of what stays), unsupported, stale -
/// and the fully correct.
fn header(first: &str) -> String {
    format!(
        "{first:<12} {:>3} {:>6} {:>6} {:>6} {:>6} {:>6} {:>9} {:>5} {:>7} {:>6} {:>8}\n",
        "n", "R@1", "R@3", "R@5", "MRR@5", "notes", "wrong-dom", "apart", "unsupp", "stale", "correct"
    )
}

fn row(name: &str, m: &Metrics) -> String {
    format!(
        "{name:<12} {:>3} {:>6.3} {:>6.3} {:>6.3} {:>6.3} {:>6.1} {:>9.3} {:>5} {:>7.3} {:>6.3} {:>8.3}\n",
        m.queries,
        m.recall1,
        m.recall3,
        m.recall5,
        m.mrr5,
        m.items as f64 / m.queries.max(1) as f64,
        m.wrong_domain,
        m.wrong_domain_items,
        m.unsupported,
        m.stale,
        m.fully_correct
    )
}

impl EvalReport {
    /// The mode and setup, then one line per style and overall.
    pub fn table(&self) -> String {
        let mut out = format!(
            "mode {} · {} of {} queries semantic · {}\n",
            self.mode.as_str(),
            self.semantic_queries,
            self.overall.queries,
            self.setup.line()
        );
        out.push_str(&header("style"));
        for (style, metrics) in &self.by_style {
            out.push_str(&row(style, metrics));
        }
        out.push_str(&row("overall", &self.overall));
        out
    }
}

/// Reports side by side, one overall line each: the A/B of modes and setups.
pub fn compare(reports: &[EvalReport]) -> String {
    let mut out = header("mode");
    for report in reports {
        out.push_str(&row(report.mode.as_str(), &report.overall));
    }
    for report in reports {
        out.push_str(&format!("{}: {} of {} semantic · {}\n", report.mode.as_str(), report.semantic_queries, report.overall.queries, report.setup.line()));
    }
    out
}

/// What the judge knows of one context note.
#[derive(Debug, Clone, Default)]
struct Seen {
    key: Option<String>,
    domain: Option<String>,
    supported: bool,
    stale: bool,
}

/// One query's verdict.
#[derive(Debug, Clone, Default, PartialEq)]
struct Judged {
    recall: [f64; 3],
    rr: f64,
    items: usize,
    wrong: usize,
    judged: usize,
    unsupported: usize,
    stale: usize,
    correct: bool,
}

/// Whether two domain paths share a line: equal, or one above the other.
fn kin(a: &str, b: &str) -> bool {
    a == b || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/')) || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// Judge one context against what the query expects, avoids and the expected notes'
/// domains.
fn judge(context: &[&Seen], expect: &[String], avoid: &[String], domains: &[String]) -> Judged {
    let expected = |seen: &&Seen| seen.key.as_ref().is_some_and(|k| expect.contains(k));
    let mut recall = [0.0; 3];
    for (slot, n) in [1usize, 3, 5].into_iter().enumerate() {
        let found = expect.iter().filter(|key| context.iter().take(n).any(|s| s.key.as_ref() == Some(*key))).count();
        recall[slot] = found as f64 / expect.len().max(1) as f64;
    }
    let rr = context.iter().take(K).position(expected).map(|i| 1.0 / (i as f64 + 1.0)).unwrap_or(0.0);
    let mut judged = 0;
    let mut wrong = 0;
    for seen in context {
        if let (Some(domain), false) = (&seen.domain, domains.is_empty()) {
            judged += 1;
            if !domains.iter().any(|d| kin(d, domain)) {
                wrong += 1;
            }
        }
    }
    let stale = context.iter().filter(|s| s.stale).count();
    let avoided = context.iter().any(|s| s.key.as_ref().is_some_and(|k| avoid.contains(k)));
    Judged {
        recall,
        rr,
        items: context.len(),
        wrong,
        judged,
        unsupported: context.iter().filter(|s| !s.supported).count(),
        stale,
        correct: context.iter().any(expected) && stale == 0 && wrong == 0 && !avoided,
    }
}

/// Running sums of verdicts.
#[derive(Debug, Clone, Default)]
struct Sum {
    queries: usize,
    recall: [f64; 3],
    rr: f64,
    items: usize,
    wrong: usize,
    judged: usize,
    unsupported: usize,
    stale: usize,
    correct: usize,
}

impl Sum {
    fn add(&mut self, j: &Judged) {
        self.queries += 1;
        for (sum, r) in self.recall.iter_mut().zip(j.recall) {
            *sum += r;
        }
        self.rr += j.rr;
        self.items += j.items;
        self.wrong += j.wrong;
        self.judged += j.judged;
        self.unsupported += j.unsupported;
        self.stale += j.stale;
        self.correct += j.correct as usize;
    }

    fn metrics(&self) -> Metrics {
        let share = |n: f64, of: usize| if of == 0 { 0.0 } else { n / of as f64 };
        Metrics {
            queries: self.queries,
            recall1: share(self.recall[0], self.queries),
            recall3: share(self.recall[1], self.queries),
            recall5: share(self.recall[2], self.queries),
            mrr5: share(self.rr, self.queries),
            items: self.items,
            wrong_domain: share(self.wrong as f64, self.judged),
            wrong_domain_items: self.wrong,
            domain_judged: self.judged,
            unsupported: share(self.unsupported as f64, self.items),
            stale: share(self.stale as f64, self.items),
            fully_correct: share(self.correct as f64, self.queries),
        }
    }
}

/// One query's context in `mode`: note ids in order, and whether meaning took part.
async fn context(service: &MemoryService, target: &Target, query: &FixtureQuery, mode: Mode) -> MemResult<(Vec<String>, bool)> {
    match mode {
        Mode::Lexical | Mode::Hybrid => {
            let request = SearchQuery { text: query.q.clone(), limit: K, words_only: mode == Mode::Lexical, ..SearchQuery::default() };
            let found = service.hybrid_search(target, &request).await?;
            Ok((found.items.into_iter().map(|h| h.summary.id).collect(), found.semantic.state == "on"))
        }
        Mode::BriefTask | Mode::BriefPaths => {
            let request = BriefRequest {
                paths: if mode == Mode::BriefPaths { query.paths.clone() } else { Vec::new() },
                task: Some(query.q.clone()),
                max_chars: None,
                exclude_global: true,
            };
            let brief = service.brief(target, &request).await?;
            let semantic = brief.semantic.as_ref().is_some_and(|s| s.state == "on");
            Ok((brief.shown, semantic))
        }
    }
}

/// Run every query of `fixture` against the project at `root` (loaded by [load]) in
/// `mode` and judge each context. Notes are matched back to fixture keys by their exact
/// title and content, so the report needs nothing but the store.
pub async fn evaluate(service: &MemoryService, root: &Path, fixture: &Fixture, mode: Mode) -> MemResult<EvalReport> {
    let target = Target::dir(root.display().to_string());
    let setup = setup(service).await?;
    let (all, _) = service
        .list(&target, &ListRequest { history: true, limit: Some(crate::service::LIST_LIMIT_MAX), ..ListRequest::default() })
        .await?;
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut id_of: HashMap<String, String> = HashMap::new();
    for summary in &all {
        let (record, _) = service.get(&target, &summary.id, false).await?;
        let key = fixture.notes.iter().find(|n| n.title == record.summary.title && n.content == record.content).map(|n| n.key.clone());
        if let Some(key) = &key {
            id_of.insert(key.clone(), summary.id.clone());
        }
        let stale = record.summary.status != Status::Active
            || record.summary.markers.iter().any(|m| matches!(*m, "anchor_changed" | "dependency_broken"));
        seen.insert(
            summary.id.clone(),
            Seen { key, domain: record.summary.domain.clone(), supported: record.summary.source_verified, stale },
        );
    }
    let mut sums: BTreeMap<String, Sum> = BTreeMap::new();
    let mut total = Sum::default();
    let mut misses = Vec::new();
    let mut semantic_queries = 0;
    for query in &fixture.queries {
        let (ids, semantic) = context(service, &target, query, mode).await?;
        semantic_queries += semantic as usize;
        let unknown = Seen::default();
        let items: Vec<Seen> = ids
            .iter()
            .map(|id| seen.get(id).cloned().unwrap_or_else(|| Seen { key: Some(id.clone()), ..unknown.clone() }))
            .collect();
        let context: Vec<&Seen> = items.iter().collect();
        let mut domains: Vec<String> = query
            .expect
            .iter()
            .filter_map(|key| id_of.get(key).and_then(|id| seen.get(id)).and_then(|s| s.domain.clone()))
            .collect();
        domains.sort();
        domains.dedup();
        let verdict = judge(&context, &query.expect, &query.avoid, &domains);
        if verdict.recall[2] < 1.0 {
            let got = items.iter().take(K).filter_map(|s| s.key.clone()).collect();
            misses.push(Miss { query: query.q.clone(), style: query.style.clone(), expected: query.expect.clone(), got });
        }
        sums.entry(query.style.clone()).or_default().add(&verdict);
        total.add(&verdict);
    }
    Ok(EvalReport {
        mode,
        setup,
        overall: total.metrics(),
        by_style: sums.into_iter().map(|(style, sum)| (style, sum.metrics())).collect(),
        misses,
        semantic_queries,
    })
}

/// Whether every note of a loaded fixture that another replaced is superseded, and
/// by its replacement: the keys that are not.
pub async fn unsuperseded(service: &MemoryService, root: &Path, fixture: &Fixture, loaded: &Loaded) -> MemResult<Vec<String>> {
    let target = Target::dir(root.display().to_string());
    let mut out = Vec::new();
    for note in &fixture.notes {
        let Some(old) = &note.replaces else { continue };
        let (Some(old_id), Some(new_id)) = (loaded.ids.get(old), loaded.ids.get(&note.key)) else {
            out.push(old.clone());
            continue;
        };
        let (record, _) = service.get(&target, old_id, false).await?;
        if record.summary.status != Status::Superseded || record.superseded_by.as_deref() != Some(new_id.as_str()) {
            out.push(old.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY: &str = r#"{
        "about": "tiny",
        "project": {"name": "tiny", "language": "en"},
        "notes": [
            {"key": "a", "kind": "decision", "title": "Use SQLite", "content": "One file.", "anchors": ["src/store.rs"]},
            {"key": "b", "kind": "pitfall", "title": "Busy", "content": "Retry.", "links": [{"to": "a", "kind": "depends_on"}], "origin": "agent"},
            {"key": "c", "kind": "decision", "title": "Use SQLite WAL", "content": "One file, WAL.", "replaces": "a"}
        ],
        "queries": [{"q": "sqlite", "expect": ["c"], "style": "keyword"}]
    }"#;

    #[test]
    fn fixtures_check_their_references() {
        assert!(Fixture::parse(TINY).is_ok());
        let bad = TINY.replace(r#""to": "a""#, r#""to": "zz""#);
        assert!(Fixture::parse(&bad).unwrap_err().contains("unknown key zz"));
        let bad = TINY.replace(r#""style": "keyword""#, r#""style": "vibes""#);
        assert!(Fixture::parse(&bad).unwrap_err().contains("unknown style"));
        let bad = TINY.replace("src/store.rs", "../escape.rs");
        assert!(Fixture::parse(&bad).unwrap_err().contains("inside the root"));
        let bad = TINY.replace(r#""kind": "depends_on""#, r#""kind": "supersedes""#);
        assert!(Fixture::parse(&bad).is_err());
    }

    fn seen(key: &str, domain: Option<&str>, supported: bool, stale: bool) -> Seen {
        Seen { key: Some(key.into()), domain: domain.map(str::to_string), supported, stale }
    }

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_context_is_judged_note_by_note() {
        let (a, b, c, d) = (
            seen("a", Some("build"), true, false),
            seen("b", Some("build/instances"), false, false),
            seen("c", Some("panel"), false, true),
            seen("d", None, false, false),
        );
        // Expected b, filed in build/instances: build is its parent (kin), panel is not, d is
        // unfiled (not judged).
        let j = judge(&[&a, &b, &c, &d], &keys(&["b"]), &[], &keys(&["build/instances"]));
        assert_eq!(j.recall, [0.0, 1.0, 1.0]);
        assert_eq!(j.rr, 0.5);
        assert_eq!((j.items, j.judged, j.wrong, j.unsupported, j.stale), (4, 3, 1, 3, 1));
        assert!(!j.correct, "a stale, wrong-domain note rides along");
        // Without the panel note: correct - unless the query avoids a note it shows.
        assert!(judge(&[&a, &b, &d], &keys(&["b"]), &[], &keys(&["build"])).correct);
        assert!(!judge(&[&a, &b, &d], &keys(&["b"]), &keys(&["d"]), &keys(&["build"])).correct);
        // Two expected, one found in the first five; nothing expected found: no rank.
        let j = judge(&[&a, &d], &keys(&["a", "zz"]), &[], &[]);
        assert_eq!((j.recall, j.rr, j.judged), ([0.5, 0.5, 0.5], 1.0, 0));
        let j = judge(&[&c], &keys(&["a"]), &[], &keys(&["build"]));
        assert_eq!((j.rr, j.correct), (0.0, false));
        // Recall counts the first five only.
        let many: Vec<Seen> = (0..6).map(|i| seen(&format!("n{i}"), None, true, false)).collect();
        let refs: Vec<&Seen> = many.iter().collect();
        assert_eq!(judge(&refs, &keys(&["n5"]), &[], &[]).recall, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn domains_are_kin_along_one_line() {
        assert!(kin("build", "build/instances"));
        assert!(kin("build/instances", "build"));
        assert!(kin("panel", "panel"));
        assert!(!kin("build", "builder"));
        assert!(!kin("panel/i18n", "panel/theme"));
    }

    #[test]
    fn the_setup_reads_the_fingerprint() {
        let setup = Setup::of(Some("openai:nomic-embed-text:latest:native:ep-1a2b3c4d:doc-99aa88bb".into()));
        assert_eq!(setup.provider.as_deref(), Some("openai"));
        assert_eq!(setup.model.as_deref(), Some("nomic-embed-text:latest"));
        assert_eq!(setup.endpoint.as_deref(), Some("ep-1a2b3c4d"));
        let old = Setup::of(Some("fake:stems:256".into()));
        assert_eq!((old.provider.as_deref(), old.model.as_deref(), old.endpoint.as_deref()), (Some("fake"), Some("stems"), None));
        assert_eq!(Setup::of(None), Setup::default());
    }
}
