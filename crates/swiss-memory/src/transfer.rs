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

//! Export, import and backup (SPEC §memory.store). The JSONL format is version 4: a
//! header line saying what the file is - the build that wrote it, what it covers (every
//! project, one, or one domain's subtree), the project, who reads its notes and how many
//! of them a person confirmed - then one line per project, per domain (parents first,
//! revisions inline), per record (anchors, refs, revisions, evidence, provenance and its
//! domain's path inline) and per link. A version 3 file still imports, its provenance
//! derived from its actors as the store's own migration derives it; a version 2 file
//! imports unfiled as well. Import is one transaction per file - a
//! dry run executes it and rolls back - and never imports a directory binding: projects
//! arrive without roots and are bound afterwards. A record names its domain by path, so
//! a map that already exists in the store takes the file's notes in. An import can land
//! a file's one project in a project of this store (`into`: the same work under another
//! store's id), and it reports every note that disagrees with the store - the same id at
//! another revision, a live note's title with other text - keeping the store's instead of
//! skipping the file's unsaid: a knowledge PR, read before it merges.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::SqliteConnection;

use crate::error::{Code, MemError, MemResult};
use crate::evidence::EVIDENCE_PER_RECORD_MAX;
use crate::domain::{self, clean_body, clean_domain_title, clean_keywords, clean_owns, clean_summary, Map};
use crate::model::{
    check_domain_path, domain_parent, is_domain_id, is_project_id, is_record_id, legacy_provenance, new_domain_id,
    Domain, DomainRevision, DomainSnapshot, Evidence, Link, Memory, Project, Revision, Status, DOMAINS_PER_PROJECT_MAX,
};
use crate::scope::{self, GLOBAL_PROJECT_ID};
use crate::service::{
    check_secret, clean_anchor_path, clean_content, clean_refs, clean_title, ensure_capacity, normalized, MemoryService,
    PROJECT_NAME_MAX,
};
use crate::store::{self, Filter};
use crate::view;

pub const FORMAT_VERSION: u64 = 4;
/// The oldest version an import still reads (no domains: its notes arrive unfiled).
pub const FORMAT_VERSION_MIN: u64 = 2;
/// One import file's ceiling: far above any real store, small enough to parse in memory.
pub const IMPORT_MAX_BYTES: usize = 64 * 1024 * 1024;
/// The header's word for the build that wrote a file.
const EXPORTER: &str = concat!("swiss-memory ", env!("CARGO_PKG_VERSION"));
/// A conflict side's excerpt of the note's text.
const CONFLICT_EXCERPT_MAX: usize = 240;

/// What an export covers.
#[derive(Debug, Clone)]
pub enum ExportScope {
    Project(String),
    /// One domain of a project (its id or path) and every domain under it: their notes,
    /// history included, the domains above it (hub only) so an import can place it, and
    /// the links whose two ends it holds.
    Domain { project: String, domain: String },
    All,
}

impl ExportScope {
    fn word(&self) -> &'static str {
        match self {
            ExportScope::Project(_) => "project",
            ExportScope::Domain { .. } => "domain",
            ExportScope::All => "all",
        }
    }
}

/// The header line: the format, and what the file says of itself. An import reads it back
/// into its report; a file without the descriptive fields (an older build's) still imports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub format_version: u64,
    #[serde(default)]
    pub exported_ms: i64,
    /// The build that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exporter: Option<String>,
    /// `all`, `project` or `domain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The project a project or domain export holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<HeaderProject>,
    /// The subtree a domain export holds, by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Who reads its notes: `project` (that project's agents), `global` (every project's),
    /// `mixed` (an export of everything).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// The notes it carries, history included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<u64>,
    /// How many of them a person confirmed; the rest an agent or a tool stands behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<u64>,
    /// A domain export's links left out because one end lies outside the subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links_left_out: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderProject {
    pub id: String,
    pub name: String,
}

/// What a record collision does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStrategy {
    /// Keep the stored record, count the line as skipped.
    Skip,
    /// Refuse the whole file.
    Fail,
}

impl ImportStrategy {
    pub fn parse(raw: &str) -> Option<ImportStrategy> {
        match raw {
            "skip" => Some(ImportStrategy::Skip),
            "fail" => Some(ImportStrategy::Fail),
            _ => None,
        }
    }
}

/// How an import runs.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub dry_run: bool,
    pub strategy: ImportStrategy,
    /// The store's project the file's one project lands in - the same work under another
    /// store's id. `None`: the projects the file names, made when missing.
    pub into: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub dry_run: bool,
    /// What the file says of itself on its header line.
    pub source: Header,
    /// The project the file's notes landed in, when the import named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    pub projects_created: Vec<String>,
    pub domains_created: u64,
    /// The paths of domain lines the store already had.
    pub domains_skipped: Vec<String>,
    pub records_created: u64,
    /// The file's notes the store already holds: the same id at the same revision and
    /// text, or - under another id - a live note of the project with the same title and text.
    pub records_skipped: Vec<String>,
    /// The file's notes kept out because the store's disagree; the store's stand.
    pub conflicts: Vec<ImportConflict>,
    pub links_created: u64,
    /// Links the store already had, and links to a note kept out as a conflict.
    pub links_skipped: u64,
}

/// One note of the file the store disagrees with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportConflict {
    pub kind: ConflictKind,
    /// The file's line.
    pub line: usize,
    pub incoming: ConflictSide,
    pub stored: ConflictSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictKind {
    /// The store holds this note at another revision, or with other text at the same one.
    Revision,
    /// A live note of the project already has this title, with other text.
    Title,
}

/// A conflict's note as each side holds it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSide {
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub status: Status,
    pub confirmed: bool,
    pub updated_ms: i64,
    pub excerpt: String,
}

impl ConflictSide {
    fn of(memory: &Memory) -> ConflictSide {
        ConflictSide {
            id: memory.id.clone(),
            revision: memory.revision,
            title: memory.title.clone(),
            status: memory.status,
            confirmed: memory.confirmed,
            updated_ms: memory.updated_ms,
            excerpt: view::excerpt(&memory.content, CONFLICT_EXCERPT_MAX),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectLine {
    id: String,
    name: String,
    created_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DomainLine {
    #[serde(flatten)]
    domain: Domain,
    #[serde(default)]
    revisions: Vec<DomainRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordLine {
    #[serde(flatten)]
    memory: Memory,
    #[serde(default)]
    revisions: Vec<Revision>,
    /// Version 4 on; each piece bound to one of the note's revisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<Evidence>,
}

fn line(kind: &str, body: impl Serialize) -> MemResult<String> {
    let mut value = serde_json::to_value(body).map_err(|e| MemError::store(e.to_string()))?;
    let mut out = serde_json::Map::new();
    out.insert("type".into(), json!(kind));
    if let Value::Object(map) = &mut value {
        out.append(map);
    }
    serde_json::to_string(&Value::Object(out)).map_err(|e| MemError::store(e.to_string()))
}

async fn named_project(conn: &mut SqliteConnection, id: &str) -> MemResult<Project> {
    store::project(conn, id).await?.ok_or_else(|| MemError::not_found(format!("no project {id}")))
}

impl MemoryService {
    /// The JSONL export of one domain's subtree, one project or everything. Plaintext:
    /// callers warn.
    pub async fn export(&self, scope: &ExportScope) -> MemResult<String> {
        let mut conn = self.store().await?.read().await?;
        let (projects, root) = match scope {
            ExportScope::All => (store::projects(&mut conn).await?, None),
            ExportScope::Project(id) => (vec![named_project(&mut conn, id).await?], None),
            ExportScope::Domain { project, domain } => {
                let project = named_project(&mut conn, project).await?;
                let root = domain::resolve(&mut conn, &project.id, domain).await?;
                (vec![project], Some(root))
            }
        };
        let mut body = String::new();
        for project in &projects {
            let line_body = ProjectLine { id: project.id.clone(), name: project.name.clone(), created_ms: project.created_ms };
            body.push_str(&line("project", line_body)?);
            body.push('\n');
        }
        // A domain export: the subtree's domain ids; the domains above it travel hub only.
        let mut inside: Option<HashSet<String>> = None;
        for project in &projects {
            let domains = store::domains(&mut conn, &project.id).await?;
            let above: HashSet<String> = match &root {
                Some(root) => {
                    let map = Map::new(&domains);
                    inside = Some(map.subtree(root));
                    map.ancestors(root).into_iter().map(|d| d.id.clone()).collect()
                }
                None => HashSet::new(),
            };
            for domain in domains {
                let revisions = if above.contains(&domain.id) {
                    Vec::new()
                } else if inside.as_ref().is_none_or(|ids| ids.contains(&domain.id)) {
                    store::domain_revisions(&mut conn, &domain.id).await?
                } else {
                    continue;
                };
                body.push_str(&line("domain", DomainLine { domain, revisions })?);
                body.push('\n');
            }
        }
        let (mut notes, mut confirmed) = (0u64, 0u64);
        let mut exported: HashSet<String> = HashSet::new();
        let mut links: Vec<Link> = Vec::new();
        for project in &projects {
            let mut memories = store::memories(&mut conn, &project.id, &Filter::default()).await?;
            if let Some(ids) = &inside {
                memories.retain(|m| m.domain.as_ref().is_some_and(|d| ids.contains(d)));
            }
            memories.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
            for memory in memories {
                notes += 1;
                confirmed += memory.confirmed as u64;
                exported.insert(memory.id.clone());
                let revisions = store::revisions(&mut conn, &memory.id).await?;
                let evidence = store::evidence(&mut conn, &memory.id).await?;
                body.push_str(&line("record", RecordLine { memory, revisions, evidence })?);
                body.push('\n');
            }
            links.extend(store::links_in(&mut conn, &project.id).await?);
        }
        let mut left_out = 0u64;
        if inside.is_some() {
            links.retain(|link| match (exported.contains(&link.from), exported.contains(&link.to)) {
                (true, true) => true,
                (false, false) => false,
                _ => {
                    left_out += 1;
                    false
                }
            });
        }
        links.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)).then_with(|| a.kind.cmp(&b.kind)));
        for link in links {
            body.push_str(&line("link", link)?);
            body.push('\n');
        }
        let single = match scope {
            ExportScope::All => None,
            _ => projects.first(),
        };
        let header = Header {
            format_version: FORMAT_VERSION,
            exported_ms: store::now(),
            exporter: Some(EXPORTER.to_string()),
            scope: Some(scope.word().to_string()),
            project: single.map(|p| HeaderProject { id: p.id.clone(), name: p.name.clone() }),
            domain: root.as_ref().map(|d| d.path.clone()),
            visibility: Some(
                match single {
                    None => "mixed",
                    Some(p) if p.id == GLOBAL_PROJECT_ID => "global",
                    Some(_) => "project",
                }
                .to_string(),
            ),
            notes: Some(notes),
            confirmed: Some(confirmed),
            links_left_out: root.is_some().then_some(left_out),
        };
        let mut out = line("header", &header)?;
        out.push('\n');
        out.push_str(&body);
        Ok(out)
    }

    /// Import a JSONL export in one transaction; `dry_run` reports and rolls back.
    pub async fn import(&self, text: &str, dry_run: bool, strategy: ImportStrategy) -> MemResult<ImportReport> {
        self.import_with(text, &ImportOptions { dry_run, strategy, into: None }).await
    }

    /// [MemoryService::import] with every option, `into` among them.
    pub async fn import_with(&self, text: &str, options: &ImportOptions) -> MemResult<ImportReport> {
        if text.len() > IMPORT_MAX_BYTES {
            return Err(MemError::invalid(format!("the file is larger than {} MiB", IMPORT_MAX_BYTES / (1024 * 1024))));
        }
        let mut parsed = parse(text)?;
        let dry_run = options.dry_run;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if let Some(into) = &options.into {
                land_into(&mut conn, &mut parsed, into).await?;
            }
            apply(&mut conn, &parsed, options).await
        }
        .await;
        if dry_run {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return outcome;
        }
        let report = store::finish(&mut conn, outcome).await?;
        self.changes().notify_one();
        Ok(report)
    }

    /// A consistent copy of the store. Without `out`, the server picks
    /// `<store dir>/backups/store-<stamp>.sqlite3` (the panel names no path).
    pub async fn backup(&self, out: Option<&Path>) -> MemResult<PathBuf> {
        let target = match out {
            Some(path) => {
                if path.exists() {
                    return Err(MemError::invalid(format!("{} already exists", scope::display(path))));
                }
                path.to_path_buf()
            }
            None => {
                let dir = self.path().parent().map(Path::to_path_buf).unwrap_or_default().join("backups");
                let stamp: String = swiss_core::log::iso_now()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                dir.join(format!("store-{stamp}-{}.sqlite3", swiss_core::util::random_hex(2)))
            }
        };
        self.store().await?.backup_into(&target).await?;
        Ok(target)
    }
}

struct Parsed {
    header: Header,
    projects: Vec<ProjectLine>,
    domains: Vec<(usize, DomainLine)>,
    records: Vec<(usize, RecordLine)>,
    links: Vec<(usize, Link)>,
    /// Landed in another project than the file's: its domain ids were the other project's,
    /// so a domain is matched by path alone.
    remapped: bool,
}

fn bad(line: usize, message: impl std::fmt::Display) -> MemError {
    MemError::invalid(format!("line {line}: {message}")).with("line", json!(line))
}

/// The header's descriptive fields as far as they read; a field of the wrong shape is left
/// out rather than refusing the file (they describe it, nothing is written from them).
fn read_header(value: &Value, version: u64) -> Header {
    let text = |v: Option<&Value>, max: usize| {
        v.and_then(Value::as_str).map(|s| view::clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), max)).filter(|s| !s.is_empty())
    };
    let count = |key: &str| value.get(key).and_then(Value::as_u64);
    let project = value.get("project").and_then(|p| {
        let id = p.get("id").and_then(Value::as_str).filter(|id| is_project_id(id))?;
        Some(HeaderProject { id: id.to_string(), name: text(p.get("name"), PROJECT_NAME_MAX).unwrap_or_else(|| id.to_string()) })
    });
    Header {
        format_version: version,
        exported_ms: value.get("exportedMs").and_then(Value::as_i64).unwrap_or(0),
        exporter: text(value.get("exporter"), 80),
        scope: text(value.get("scope"), 16),
        project,
        domain: text(value.get("domain"), 240),
        visibility: text(value.get("visibility"), 16),
        notes: count("notes"),
        confirmed: count("confirmed"),
        links_left_out: count("linksLeftOut"),
    }
}

/// Parse and validate every line before anything is written.
fn parse(text: &str) -> MemResult<Parsed> {
    let mut parsed = Parsed {
        header: Header::default(),
        projects: Vec::new(),
        domains: Vec::new(),
        records: Vec::new(),
        links: Vec::new(),
        remapped: false,
    };
    let mut header = false;
    let mut version = 0;
    for (index, raw) in text.lines().enumerate() {
        let n = index + 1;
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(raw).map_err(|e| bad(n, format!("not JSON: {e}")))?;
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Value::Object(map) = &mut value {
            map.remove("type");
        }
        if !header {
            if kind != "header" {
                return Err(bad(n, "the first line must be the export header ({\"type\":\"header\",\"formatVersion\":4})"));
            }
            version = value.get("formatVersion").and_then(Value::as_u64).unwrap_or(0);
            if !(FORMAT_VERSION_MIN..=FORMAT_VERSION).contains(&version) {
                return Err(MemError::new(
                    Code::UnsupportedSchema,
                    format!(
                        "line {n}: formatVersion {version}; this build imports versions {FORMAT_VERSION_MIN}..{FORMAT_VERSION}"
                    ),
                ));
            }
            parsed.header = read_header(&value, version);
            header = true;
            continue;
        }
        match kind.as_str() {
            "project" => {
                let mut project: ProjectLine = serde_json::from_value(value).map_err(|e| bad(n, e))?;
                if !is_project_id(&project.id) {
                    return Err(bad(n, format!("{:?} is not a project id", project.id)));
                }
                project.name = project.name.split_whitespace().collect::<Vec<_>>().join(" ");
                if project.name.is_empty() {
                    return Err(bad(n, "a project needs a name"));
                }
                project.name = view::clip(&project.name, PROJECT_NAME_MAX);
                parsed.projects.push(project);
            }
            "domain" if version >= 3 => {
                let mut domain: DomainLine = serde_json::from_value(value).map_err(|e| bad(n, e))?;
                check_domain(&mut domain).map_err(|e| bad(n, e.message))?;
                parsed.domains.push((n, domain));
            }
            "record" => {
                let mut record: RecordLine = serde_json::from_value(value).map_err(|e| bad(n, e))?;
                if version < 3 {
                    record.memory.domain_path = None;
                }
                if version < 4 {
                    // Before ADR-032: the actor is all there is to read provenance from.
                    (record.memory.producer, record.memory.transport) =
                        legacy_provenance(record.memory.origin, &record.memory.actor);
                    record.evidence.clear();
                }
                // The path decides; an id is the exporting store's.
                record.memory.domain = None;
                check_record(&mut record).map_err(|e| bad(n, e.message))?;
                parsed.records.push((n, record));
            }
            "link" => {
                let link: Link = serde_json::from_value(value).map_err(|e| bad(n, e))?;
                if !is_record_id(&link.from) || !is_record_id(&link.to) || link.from == link.to {
                    return Err(bad(n, "a link needs two distinct note ids"));
                }
                if let Some(note) = &link.note {
                    check_secret("note", note).map_err(|e| bad(n, e.message))?;
                }
                parsed.links.push((n, link));
            }
            "header" => return Err(bad(n, "a second header")),
            other => return Err(bad(n, format!("unknown line type {other:?}"))),
        }
    }
    if !header {
        return Err(MemError::invalid("the file is empty"));
    }
    Ok(parsed)
}

/// A domain line obeys every rule a create does, secrets check included.
fn check_domain(line: &mut DomainLine) -> MemResult<()> {
    let domain = &mut line.domain;
    if !is_domain_id(&domain.id) {
        return Err(MemError::invalid(format!("{:?} is not a domain id", domain.id)));
    }
    if !is_project_id(&domain.scope) || domain.scope == GLOBAL_PROJECT_ID {
        return Err(MemError::invalid(format!("{:?} is not a project id with a map", domain.scope)));
    }
    if domain.revision == 0 {
        return Err(MemError::invalid("revision starts at 1"));
    }
    domain.path = check_domain_path(&domain.path).map_err(MemError::invalid)?;
    domain.title = clean_domain_title(&domain.title)?;
    domain.summary = clean_summary(&domain.summary)?;
    domain.body = clean_body(&domain.body)?;
    domain.owns = clean_owns(&domain.owns)?;
    domain.keywords = clean_keywords(&domain.keywords)?;
    let mut numbers = HashSet::new();
    for revision in &line.revisions {
        if !numbers.insert(revision.revision) || revision.revision == 0 || revision.revision > domain.revision {
            return Err(MemError::invalid(format!("revision {} is out of sequence", revision.revision)));
        }
        check_secret("revision title", &revision.snapshot.title)?;
        check_secret("revision summary", &revision.snapshot.summary)?;
        check_secret("revision body", &revision.snapshot.body)?;
    }
    Ok(())
}

/// A record line obeys every rule a save does, secrets check included.
fn check_record(record: &mut RecordLine) -> MemResult<()> {
    let memory = &mut record.memory;
    if !is_record_id(&memory.id) {
        return Err(MemError::invalid(format!("{:?} is not a note id", memory.id)));
    }
    if !is_project_id(&memory.scope) {
        return Err(MemError::invalid(format!("{:?} is not a project id", memory.scope)));
    }
    if memory.revision == 0 {
        return Err(MemError::invalid("revision starts at 1"));
    }
    if let Some(next) = &memory.superseded_by {
        if !is_record_id(next) {
            return Err(MemError::invalid("supersededBy is not a note id"));
        }
    }
    if let Some(path) = &memory.domain_path {
        memory.domain_path = Some(check_domain_path(path).map_err(|e| MemError::invalid(format!("domainPath: {e}")))?);
    }
    memory.title = clean_title(&memory.title)?;
    memory.content = clean_content(&memory.content)?;
    check_secret("title", &memory.title)?;
    check_secret("content", &memory.content)?;
    let mut seen = HashSet::new();
    for anchor in &mut memory.anchors {
        anchor.path = clean_anchor_path(&anchor.path)?;
        anchor.glob = anchor.path.contains(['*', '?']);
        if !seen.insert(anchor.path.clone()) {
            return Err(MemError::invalid(format!("anchors: {} twice", anchor.path)));
        }
    }
    if memory.anchors.len() > crate::model::ANCHORS_MAX {
        return Err(MemError::invalid(format!("anchors: at most {}", crate::model::ANCHORS_MAX)));
    }
    memory.refs = clean_refs(&memory.refs)?;
    memory.keywords = clean_keywords(&memory.keywords)?;
    let mut numbers = HashSet::new();
    for revision in &record.revisions {
        if !numbers.insert(revision.revision) || revision.revision == 0 || revision.revision > memory.revision {
            return Err(MemError::invalid(format!("revision {} is out of sequence", revision.revision)));
        }
        check_secret("revision title", &revision.snapshot.title)?;
        check_secret("revision content", &revision.snapshot.content)?;
    }
    if record.evidence.len() > EVIDENCE_PER_RECORD_MAX {
        return Err(MemError::invalid(format!("evidence: a note carries at most {EVIDENCE_PER_RECORD_MAX}")));
    }
    let mut ids = HashSet::new();
    for evidence in &mut record.evidence {
        crate::evidence::check_imported(evidence, &record.memory)?;
        if !ids.insert(evidence.id.clone()) {
            return Err(MemError::invalid(format!("evidence: {} twice", evidence.id)));
        }
    }
    Ok(())
}

/// Create a project the file names (or a record refers to) when the store lacks it:
/// without roots - identities are never imported implicitly.
async fn ensure_project(
    conn: &mut SqliteConnection,
    id: &str,
    names: &HashMap<&str, &ProjectLine>,
    known: &mut HashSet<String>,
    report: &mut ImportReport,
) -> MemResult<()> {
    if known.contains(id) {
        return Ok(());
    }
    if store::project(conn, id).await?.is_none() {
        if id == GLOBAL_PROJECT_ID {
            return Err(MemError::store("the global project is missing"));
        }
        let (name, created) = match names.get(id) {
            Some(line) => (line.name.clone(), line.created_ms),
            None => (id.to_string(), store::now()),
        };
        store::insert_project(conn, id, &name, created).await?;
        report.projects_created.push(id.to_string());
    }
    known.insert(id.to_string());
    Ok(())
}

/// Point a file of one project at the store's project `into`: its domains and notes take
/// that scope, its domains fresh ids (the file's are another project's - a domain is its
/// path), its notes their own ids, so importing the same file again meets the first. A
/// note keeps who reads it: a global file lands only in the global scope, a project's only
/// in a project.
async fn land_into(conn: &mut SqliteConnection, parsed: &mut Parsed, into: &str) -> MemResult<()> {
    let into = into.trim();
    if !is_project_id(into) {
        return Err(MemError::invalid(format!("into: {into:?} is not a project id")));
    }
    named_project(conn, into).await?;
    let named: BTreeSet<&str> = parsed
        .projects
        .iter()
        .map(|p| p.id.as_str())
        .chain(parsed.domains.iter().map(|(_, d)| d.domain.scope.as_str()))
        .chain(parsed.records.iter().map(|(_, r)| r.memory.scope.as_str()))
        .collect();
    let from = match named.iter().collect::<Vec<_>>().as_slice() {
        [] => return Ok(()),
        [one] => one.to_string(),
        many => {
            return Err(MemError::invalid(format!(
                "into takes the file of one project; this one holds {} ({})",
                many.len(),
                named.iter().copied().collect::<Vec<_>>().join(", ")
            )))
        }
    };
    if (from == GLOBAL_PROJECT_ID) != (into == GLOBAL_PROJECT_ID) {
        return Err(MemError::invalid(format!(
            "into: a note keeps who reads it - a global file lands in the global scope, a project's in a project \
             ({from} into {into})"
        )));
    }
    if from == into {
        return Ok(());
    }
    parsed.projects.clear();
    for (_, line) in &mut parsed.domains {
        line.domain.scope = into.to_string();
        line.domain.id = new_domain_id();
    }
    for (_, record) in &mut parsed.records {
        record.memory.scope = into.to_string();
    }
    parsed.remapped = true;
    Ok(())
}

/// The first of `ids` this import did not write itself.
fn first_stored(ids: Vec<String>, written: &HashSet<String>) -> Option<String> {
    ids.into_iter().find(|id| !written.contains(id))
}

async fn apply(conn: &mut SqliteConnection, parsed: &Parsed, options: &ImportOptions) -> MemResult<ImportReport> {
    let strategy = options.strategy;
    let mut report = ImportReport {
        dry_run: options.dry_run,
        source: parsed.header.clone(),
        into: options.into.as_ref().map(|p| p.trim().to_string()),
        ..ImportReport::default()
    };
    let mut names: HashMap<&str, &ProjectLine> = HashMap::new();
    for project in &parsed.projects {
        names.insert(project.id.as_str(), project);
    }
    let mut known: HashSet<String> = HashSet::new();
    for project in &parsed.projects {
        ensure_project(conn, &project.id, &names, &mut known, &mut report).await?;
    }
    // (project, path) of every domain line -> the stored domain it is.
    let mut filed: HashMap<(String, String), String> = HashMap::new();
    for (n, line) in &parsed.domains {
        let domain = &line.domain;
        ensure_project(conn, &domain.scope, &names, &mut known, &mut report).await?;
        let stored = store::domains(conn, &domain.scope).await?;
        let key = (domain.scope.clone(), domain.path.clone());
        if let Some(existing) = stored.iter().find(|d| d.path == domain.path) {
            if existing.id != domain.id && strategy == ImportStrategy::Fail && !parsed.remapped {
                return Err(MemError::new(
                    Code::DomainExists,
                    format!("line {n}: project {} already has a domain {} ({})", domain.scope, domain.path, existing.id),
                )
                .with("line", json!(n))
                .with("path", json!(domain.path)));
            }
            report.domains_skipped.push(domain.path.clone());
            filed.insert(key, existing.id.clone());
            continue;
        }
        if let Some(existing) = store::domain(conn, &domain.id).await? {
            if existing.scope == domain.scope && strategy == ImportStrategy::Skip {
                // Moved since the export: the stored one is it.
                report.domains_skipped.push(domain.path.clone());
                filed.insert(key, existing.id.clone());
                continue;
            }
            return Err(MemError::new(
                Code::DomainExists,
                format!("line {n}: domain {} already exists as {} (project {})", domain.id, existing.path, existing.scope),
            )
            .with("line", json!(n))
            .with("id", json!(domain.id)));
        }
        if let Some(parent) = domain_parent(&domain.path) {
            if !stored.iter().any(|d| d.path == parent) {
                return Err(bad(*n, format!("{} needs its parent {parent} before it", domain.path)));
            }
        }
        if stored.len() >= DOMAINS_PER_PROJECT_MAX {
            return Err(bad(*n, format!("project {} would hold more than {DOMAINS_PER_PROJECT_MAX} domains", domain.scope)));
        }
        store::insert_domain(conn, domain).await?;
        let mut revisions = line.revisions.clone();
        revisions.sort_by_key(|r| r.revision);
        if !revisions.iter().any(|r| r.revision == domain.revision) {
            revisions.push(DomainRevision {
                revision: domain.revision,
                op: "import".into(),
                actor: "import".into(),
                reason: None,
                at_ms: store::now(),
                snapshot: DomainSnapshot::of(domain),
            });
        }
        for revision in &revisions {
            store::insert_domain_revision(conn, &domain.id, revision).await?;
        }
        filed.insert(key, domain.id.clone());
        report.domains_created += 1;
    }
    // The notes this import wrote, the file's notes that are a stored one under another id,
    // and the ones kept out as a conflict (their links go with them).
    let mut written: HashSet<String> = HashSet::new();
    let mut same_as: HashMap<String, String> = HashMap::new();
    let mut kept_out: HashSet<String> = HashSet::new();
    for (n, record) in &parsed.records {
        let mut memory = record.memory.clone();
        if let Some(path) = &memory.domain_path {
            let Some(id) = filed.get(&(memory.scope.clone(), path.clone())) else {
                return Err(bad(*n, format!("note {} is filed in {path}, a domain this file does not carry", memory.id)));
            };
            memory.domain = Some(id.clone());
        }
        let memory = &memory;
        if let Some(existing) = store::memory(conn, &memory.id).await? {
            if existing.scope != memory.scope || strategy == ImportStrategy::Fail {
                return Err(MemError::new(
                    Code::SimilarExists,
                    format!("line {n}: note {} already exists (project {})", memory.id, existing.scope),
                )
                .with("line", json!(n))
                .with("id", json!(memory.id)));
            }
            let same_text = store::content_hash(&existing.title, &existing.content)
                == store::content_hash(&memory.title, &memory.content);
            if existing.revision == memory.revision && same_text {
                report.records_skipped.push(memory.id.clone());
            } else {
                report.conflicts.push(ImportConflict {
                    kind: ConflictKind::Revision,
                    line: *n,
                    incoming: ConflictSide::of(memory),
                    stored: ConflictSide::of(&existing),
                });
            }
            continue;
        }
        if memory.status.is_live() {
            let hash = store::content_hash(&memory.title, &memory.content);
            let twin = first_stored(store::live_ids_by(conn, &memory.scope, &hash, true).await?, &written);
            let titled = match &twin {
                Some(_) => None,
                None => first_stored(
                    store::live_ids_by(conn, &memory.scope, &store::title_norm(&memory.title), false).await?,
                    &written,
                ),
            };
            if let Some(stored) = twin.as_ref().or(titled.as_ref()) {
                if strategy == ImportStrategy::Fail {
                    let why = if twin.is_some() { "repeats" } else { "has the title, with other text, of" };
                    return Err(MemError::new(
                        Code::SimilarExists,
                        format!("line {n}: note {} {why} the project's live note {stored}", memory.id),
                    )
                    .with("line", json!(n))
                    .with("id", json!(memory.id))
                    .with("similar", json!(stored)));
                }
                if twin.is_some() {
                    report.records_skipped.push(memory.id.clone());
                    same_as.insert(memory.id.clone(), stored.clone());
                } else {
                    let existing =
                        store::memory(conn, stored).await?.ok_or_else(|| MemError::store("a live note vanished mid-import"))?;
                    report.conflicts.push(ImportConflict {
                        kind: ConflictKind::Title,
                        line: *n,
                        incoming: ConflictSide::of(memory),
                        stored: ConflictSide::of(&existing),
                    });
                    kept_out.insert(memory.id.clone());
                }
                continue;
            }
        }
        written.insert(memory.id.clone());
        ensure_project(conn, &memory.scope, &names, &mut known, &mut report).await?;
        store::insert_memory(conn, memory).await?;
        let mut revisions = record.revisions.clone();
        revisions.sort_by_key(|r| r.revision);
        if !revisions.iter().any(|r| r.revision == memory.revision) {
            revisions.push(Revision {
                revision: memory.revision,
                op: "import".into(),
                actor: "import".into(),
                reason: None,
                at_ms: store::now(),
                snapshot: crate::model::Snapshot::of(memory),
            });
        }
        for revision in &revisions {
            store::insert_revision(conn, &memory.id, revision).await?;
        }
        for evidence in &record.evidence {
            if !store::insert_evidence(conn, evidence).await? {
                return Err(bad(*n, format!("evidence {} already exists in this store", evidence.id)));
            }
        }
        report.records_created += 1;
    }
    for (n, link) in &parsed.links {
        if kept_out.contains(&link.from) || kept_out.contains(&link.to) {
            report.links_skipped += 1;
            continue;
        }
        // A note the store holds under another id takes the file's links to it.
        let from_id = same_as.get(&link.from).unwrap_or(&link.from);
        let to_id = same_as.get(&link.to).unwrap_or(&link.to);
        if from_id == to_id {
            report.links_skipped += 1;
            continue;
        }
        let from = store::memory(conn, from_id).await?;
        let to = store::memory(conn, to_id).await?;
        let (Some(from), Some(to)) = (from, to) else {
            return Err(bad(*n, format!("link {} → {}: an end is missing", link.from, link.to)));
        };
        if from.scope != to.scope {
            return Err(bad(*n, format!("link {} → {} crosses projects", link.from, link.to)));
        }
        let link = normalized(from_id, to_id, link.kind, link.note.clone(), &link.actor, link.created_ms);
        if store::link(conn, &link.from, &link.to, link.kind).await?.is_some() {
            report.links_skipped += 1;
            continue;
        }
        ensure_capacity(conn, &link.from).await.map_err(|e| bad(*n, e.message))?;
        ensure_capacity(conn, &link.to).await.map_err(|e| bad(*n, e.message))?;
        store::insert_link(conn, &link).await?;
        report.links_created += 1;
    }
    Ok(report)
}
