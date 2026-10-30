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

//! The memory service (SPEC §memory): every business rule lives here, and the three
//! entry points - the offline CLI, `/mcp/memory` and `/api/knowledge` - are thin
//! adapters over it. One operation acquires the store's single connection once and,
//! when it writes, runs in exactly one BEGIN IMMEDIATE transaction: record,
//! revisions, anchors, refs, links and a supersede stamp land together or not at all.
//!
//! The store opens on first use, so a plugin start never waits on the disk and a
//! broken database surfaces as an error on the call that needs it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;
use serde_json::json;
use sqlx::sqlite::SqliteConnection;
use tokio::sync::{Notify, OnceCell};

use crate::brief::{self, BriefInput};
use crate::code::{CodeCheck, CodeState};
use crate::domain;
use crate::embed::{self, Embedder, EmbeddingStatus, IndexReport, Purpose, SemanticState};
use crate::error::{Code, MemError, MemResult};
use crate::evidence::{self, EvidenceSpec, EVIDENCE_PER_RECORD_MAX};
use crate::formation::{self, Lint};
use crate::fresh::{self, Checker};
use crate::glob;
use crate::graph::{self, GraphEdge, GraphView, Net, NodeInfo};
use crate::model::{
    is_project_id, is_record_id, new_project_id, new_record_id, Actor, Anchor, Kind, Link, LinkKind, Memory, Producer, Project,
    Relation, Revision, Root, RootKind, Snapshot, Status, Verdict, ANCHORS_MAX, ANCHOR_PATH_MAX, CONTENT_MAX,
    LINKS_PER_RECORD_MAX, LINK_NOTE_MAX, REFS_MAX, REF_MAX, SAVE_LINKS_MAX, TITLE_MAX,
};
use crate::opposed::{self, Opposed};
use crate::related::{self, Seed};
use crate::scope::{self, Found, Scope, GLOBAL_PROJECT_ID};
use crate::scopes::{Loaded, Scopes};
use crate::search::{self, SearchQuery};
use crate::secrets;
use crate::store::{self, Filter, Store};
use crate::view::{self, HitView, ProjectView, RecordView, RelatedView, Summary};

pub const STORE_FILE: &str = "store.sqlite3";
pub const PROJECT_NAME_MAX: usize = 80;
pub const REASON_MAX: usize = 500;
pub const LIST_LIMIT_DEFAULT: usize = 200;
pub const LIST_LIMIT_MAX: usize = 500;
const SIMILAR_HINTS: usize = 3;
/// The least CJK coverage a similar hint needs, the brief's task floor: a note
/// sharing one stray unit of the saved title is no hint.
const SIMILAR_COVERAGE_MIN: f64 = 0.2;
/// The brief's task section: at most this many notes.
const TASK_HITS: usize = 5;

/// What an operation is aimed at.
#[derive(Debug, Clone)]
pub enum Target {
    /// A directory: an agent's or the CLI's cwd. `global` selects the shared notes
    /// (the directory is still validated, never used for selection). `git_dir` is the
    /// CLI's explicit `GIT_DIR`, never read from the environment here.
    Dir { cwd: String, git_dir: Option<PathBuf>, global: bool },
    /// A project id (`global` included): the panel's selector.
    Project(String),
}

impl Target {
    pub fn dir(cwd: impl Into<String>) -> Target {
        Target::Dir { cwd: cwd.into(), git_dir: None, global: false }
    }

    pub fn global(cwd: impl Into<String>) -> Target {
        Target::Dir { cwd: cwd.into(), git_dir: None, global: true }
    }

    pub fn project(id: impl Into<String>) -> Target {
        Target::Project(id.into())
    }
}

/// Where an operation runs once its target is resolved.
#[derive(Debug, Clone, Default)]
pub struct Place {
    /// None when the directory is not (yet) a registered project.
    pub scope: Option<Scope>,
    pub project: Option<Project>,
    /// The checkout anchors are hashed and checked against: the working tree the
    /// directory is in (a linked worktree itself, though the project registers its
    /// main checkout), a project's first reachable root.
    pub root: Option<PathBuf>,
    pub found: Option<Found>,
    pub dir: Option<PathBuf>,
}

impl Place {
    pub(crate) fn project_id(&self) -> MemResult<String> {
        match &self.scope {
            Some(scope) => Ok(scope.project_id().to_string()),
            None => Err(MemError::not_found(match &self.found {
                Some(found) => format!("{} is not a registered project yet; the first save registers it", scope::display(&found.root)),
                None => "this directory is not inside a project".to_string(),
            })),
        }
    }
}

/// One link asked for at save time.
#[derive(Debug, Clone)]
pub struct LinkSpec {
    pub to: String,
    pub kind: LinkKind,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub kind: Kind,
    pub title: String,
    pub content: String,
    pub anchors: Vec<String>,
    pub refs: Vec<String>,
    /// The terms it is searched by, in English and the team's language.
    pub keywords: Vec<String>,
    pub links: Vec<LinkSpec>,
    pub replaces: Option<String>,
    pub allow_similar: bool,
    /// The domain to file the note in (a path or id; `none` leaves it unfiled). Absent:
    /// its anchors decide, then the replaced note's domain (SPEC §memory.domains).
    pub domain: Option<String>,
    /// What the note rests on (ADR-032), bound to its first revision; `derived_from` unless
    /// a piece says otherwise.
    pub evidence: Vec<EvidenceSpec>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
    /// True when this call registered the project.
    pub registered: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub record: RecordView,
    pub duplicate: bool,
    pub similar: Vec<Summary>,
    /// Live notes on the same files whose title asserts the opposite (crate::opposed):
    /// a hint, never a refusal; such a note is not repeated under `similar`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contradicts: Vec<Opposed>,
    pub project: ProjectRef,
    /// The formation lint on what was written (ADR-032): advisory, never a refusal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub formation: Vec<Lint>,
    /// What the note names, checked against the checkout (SPEC §memory.code); absent for a
    /// duplicate and for the global notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeCheck>,
}

/// What an update wrote, and what the code check found in the text it leaves.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateOutcome {
    pub record: RecordView,
    /// Absent when the text did not change, and for the global notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeCheck>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateRequest {
    pub expected_revision: u64,
    pub kind: Option<Kind>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub anchors: Option<Vec<String>>,
    pub refs: Option<Vec<String>>,
    /// Replaces the keywords; `[]` clears them. An edit, though the text stays.
    pub keywords: Option<Vec<String>>,
    pub reason: Option<String>,
    /// Re-file the note: a domain path or id, `""` or `none` to unfile. Alone it needs no
    /// `expectedRevision` and writes no revision: filing is organisation, not content.
    pub domain: Option<String>,
    /// Evidence for the text the update leaves (ADR-032); without a content change it binds
    /// to the current revision, like filing, without a compare-and-swap.
    pub evidence: Vec<EvidenceSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct ListRequest {
    pub kinds: Vec<Kind>,
    /// Explicit statuses; empty means live (or everything with `history`).
    pub statuses: Vec<Status>,
    pub unconfirmed: bool,
    pub history: bool,
    pub limit: Option<usize>,
    /// A domain's subtree (id or path), or `none` for the unfiled notes.
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BriefRequest {
    pub paths: Vec<String>,
    pub task: Option<String>,
    pub max_chars: Option<usize>,
    pub exclude_global: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefOutput {
    pub markdown: String,
    pub chars: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Whether the task section used vectors (present only when a task was given).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticState>,
    /// The notes the brief shows, in reading order: the evaluation's context (SPEC
    /// §memory.evaluation). Not on the wire - the markdown names them.
    #[serde(skip)]
    pub shown: Vec<String>,
}

/// A search's answer: the hits and whether the semantic tier ran.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    pub items: Vec<HitView>,
    pub semantic: SemanticState,
}

#[derive(Debug, Clone)]
pub struct RelatedRequest {
    pub seed: Seed,
    pub depth: Option<u32>,
    pub limit: Option<usize>,
    pub include_history: bool,
}

/// A related search's answer: the neighbours, and why the similar tier did not run when
/// it did not (the project's settings turn embedding off, §memory.embedding).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedOutcome {
    pub items: Vec<RelatedView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticState>,
}

#[derive(Debug, Clone, Default)]
pub struct GraphRequest {
    pub center: Option<String>,
    pub depth: Option<u32>,
    pub include_history: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    pub db_path: String,
    pub schema_version: i64,
    pub projects: Vec<ProjectView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    /// Register the enclosing Git repository.
    Auto,
    /// Bind this exact directory; nothing is written into it.
    Here,
    /// Write `.swiss/memory.json`, which survives moves and clones.
    Portable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitOutcome {
    pub project: ProjectView,
    pub identity: String,
    pub created: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker_file: Option<String>,
}

/// The one service every entry point shares.
pub struct MemoryService {
    path: PathBuf,
    store: OnceCell<Store>,
    /// Closing excludes an in-flight lazy open and prevents stale callers reopening it.
    closed: tokio::sync::RwLock<bool>,
    /// The embedding provider, swapped at run time by the plugin's config (none in the
    /// offline CLI, which stays lexical).
    embedder: RwLock<Option<Arc<dyn Embedder>>>,
    /// One indexing run at a time.
    index_lock: tokio::sync::Mutex<()>,
    index: Mutex<IndexState>,
    /// Fired after a write that changes what a note embeds as (the indexer's nudge).
    changed: Notify,
    /// The code index beside the store (SPEC §memory.code).
    pub(crate) code: CodeState,
    /// The scopes read lately, kept while nothing commits ([crate::scopes]).
    pub(crate) scopes: Arc<Scopes>,
}

/// What the last indexing runs left behind.
#[derive(Debug, Clone, Default)]
struct IndexState {
    running: bool,
    last_error: Option<String>,
    last_run_ms: Option<i64>,
    /// Notes whose embedding failed (id -> text hash): queued after the rest, so one
    /// note a provider always refuses cannot stall the index.
    failed: HashMap<String, String>,
    /// The width the provider last answered, with its fingerprint: the index's one
    /// width (SPEC §memory.embedding).
    width: Option<(String, usize)>,
}

/// A recheck verdict's two guards, checked where the write happens (SPEC §memory.init
/// The recheck): the note is still at the revision its question showed, and still one
/// the question's reach takes ([crate::recheck::Reach::holds]) - so a note a person
/// edited, confirmed, vouched for, invalidated or superseded while the answer waited
/// keeps every one of those, and the verdict is refused instead of landing on stale ground.
fn recheck_guard(memory: &Memory, expected: u64, reach: crate::recheck::Reach) -> MemResult<()> {
    if memory.revision != expected {
        return Err(MemError::new(
            Code::RevisionConflict,
            format!(
                "{} is at revision {}, not the {} its recheck question showed; it moved while the answer waited",
                memory.id, memory.revision, expected
            ),
        )
        .with("current", json!(memory.revision)));
    }
    if !reach.holds(memory) {
        return Err(MemError::invalid(format!(
            "{} left the recheck's scope while its answer waited; what a person did to it stands",
            memory.id
        )));
    }
    Ok(())
}

impl MemoryService {
    /// A service over the database at `path`; nothing is opened yet.
    pub fn at(path: &Path) -> MemoryService {
        MemoryService {
            path: path.to_path_buf(),
            store: OnceCell::new(),
            closed: tokio::sync::RwLock::new(false),
            embedder: RwLock::new(None),
            index_lock: tokio::sync::Mutex::new(()),
            index: Mutex::new(IndexState::default()),
            changed: Notify::new(),
            code: CodeState::beside(path),
            scopes: Arc::default(),
        }
    }

    /// The home's store: `<SWISS_HOME>/memory/store.sqlite3`.
    pub fn default_path() -> PathBuf {
        swiss_core::paths::data_path(&["memory", STORE_FILE])
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `project_id`'s notes and links through `conn`, or as kept since the last read when
    /// nothing has committed since ([crate::scopes]). Only on a connection outside a
    /// transaction: a scope read inside one could keep what a rollback takes back.
    pub(crate) async fn scope(&self, conn: &mut SqliteConnection, project_id: &str) -> MemResult<Arc<Loaded>> {
        self.scopes.get(&self.path, conn, project_id).await
    }

    /// The store, opened and migrated on first use (a failed open retries next call).
    pub async fn store(&self) -> MemResult<&Store> {
        let closed = self.closed.read().await;
        if *closed {
            return Err(MemError::store("the memory service is closed"));
        }
        self.store.get_or_try_init(|| Store::open(&self.path)).await
    }

    /// Release this service's databases without opening unused ones or deleting data.
    /// A restart creates a new service; outstanding callers cannot revive this one.
    pub async fn close(&self) {
        // Wait for lazy initialization before marking closed. Release the guard before
        // draining pools: a borrower may need to observe the closed flag to finish.
        *self.closed.write().await = true;
        self.set_embedder(None);
        let store = async {
            if let Some(store) = self.store.get() {
                store.close().await;
            }
        };
        tokio::join!(store, self.code.close(), self.scopes.clear());
    }

    // --- resolution -------------------------------------------------------------------

    /// Resolve a target without registering anything (every read).
    pub async fn locate(&self, conn: &mut SqliteConnection, target: &Target) -> MemResult<Place> {
        match target {
            Target::Project(id) => {
                let id = id.trim();
                if !is_project_id(id) {
                    return Err(MemError::invalid("project must be a project id (p-…) or \"global\""));
                }
                let project = store::project(conn, id).await?.ok_or_else(|| MemError::not_found(format!("no project {id}")))?;
                let root = first_root(&project);
                Ok(Place { scope: Some(Scope::of(id)), project: Some(project), root, found: None, dir: None })
            }
            Target::Dir { cwd, git_dir, global } => {
                let dir = scope::normalize_dir(cwd).map_err(MemError::invalid_cwd)?;
                if *global {
                    let project = store::project(conn, GLOBAL_PROJECT_ID).await?;
                    return Ok(Place { scope: Some(Scope::Global), project, root: None, found: None, dir: Some(dir) });
                }
                let found = match git_dir {
                    Some(git_dir) => Some(scope::from_git_dir(git_dir, &dir).map_err(MemError::invalid_cwd)?),
                    None => {
                        let exact = store::path_identities(conn).await?;
                        scope::resolve(&dir, &|identity: &str| exact.contains(identity)).map_err(MemError::invalid)?
                    }
                };
                let Some(found) = found else {
                    return Ok(Place { dir: Some(dir), ..Place::default() });
                };
                let owner = match store::root_owner(conn, &found.identity).await? {
                    Some(owner) => Some(owner),
                    None => match &found.marker {
                        Some(marker) if store::project(conn, &marker.project_id).await?.is_some() => {
                            Some(marker.project_id.clone())
                        }
                        _ => None,
                    },
                };
                let project = match &owner {
                    Some(id) => store::project(conn, id).await?,
                    None => None,
                };
                Ok(Place {
                    scope: project.as_ref().map(|p| Scope::of(&p.id)),
                    project,
                    root: Some(found.checkout.clone()),
                    found: Some(found),
                    dir: Some(dir),
                })
            }
        }
    }

    /// Resolve a target for a write, registering an unregistered boundary (SPEC
    /// §memory.scope). Runs inside the caller's transaction. Answers whether this
    /// call registered the project.
    pub(crate) async fn place_for_write(&self, conn: &mut SqliteConnection, target: &Target) -> MemResult<(Place, bool)> {
        let mut place = self.locate(conn, target).await?;
        if place.scope.is_some() {
            // A marker naming a known project whose directory is not bound yet.
            if let (Some(found), Some(project)) = (&place.found, &place.project) {
                if found.kind == RootKind::Marker && store::root_owner(conn, &found.identity).await?.is_none() {
                    claim_root(conn, &project.id, found).await?;
                } else if found.kind == RootKind::Git {
                    heal_root(conn, found).await?;
                }
            }
            return Ok((place, false));
        }
        let Some(found) = place.found.clone() else {
            let dir = place.dir.as_deref().map(scope::display).unwrap_or_default();
            return Err(MemError::not_found(format!(
                "{dir} is not inside a project: no Git repository, .swiss/memory.json or bound directory above it. \
                 Run `swiss memory init --here` there (or bind it in the panel), or pass scope \"global\""
            )));
        };
        let (project_id, name) = match &found.marker {
            Some(marker) => (
                marker.project_id.clone(),
                marker.name.clone().unwrap_or_else(|| scope::default_name(&found.root)),
            ),
            None => {
                refuse_reclone(conn, &found).await?;
                (new_project_id(), scope::default_name(&found.root))
            }
        };
        if store::project(conn, &project_id).await?.is_none() {
            store::insert_project(conn, &project_id, &view::clip(name.trim(), PROJECT_NAME_MAX), store::now()).await?;
        }
        claim_root(conn, &project_id, &found).await?;
        place.project = store::project(conn, &project_id).await?;
        place.scope = Some(Scope::of(&project_id));
        Ok((place, true))
    }

    // --- records ----------------------------------------------------------------------

    /// Save a new note (SPEC §memory.capture).
    pub async fn save(&self, target: &Target, actor: &Actor, request: SaveRequest) -> MemResult<SaveOutcome> {
        self.save_minted(target, actor, request, None).await
    }

    /// [Self::save] with the note's id and clock fixed: the evaluation loads a fixture the
    /// same way every run, so ties break the same way (SPEC §memory.evaluation). An id
    /// already taken falls back to a fresh one.
    pub(crate) async fn save_minted(
        &self,
        target: &Target,
        actor: &Actor,
        request: SaveRequest,
        minted: Option<(String, i64)>,
    ) -> MemResult<SaveOutcome> {
        let title = clean_title(&request.title)?;
        let content = clean_content(&request.content)?;
        let mut anchors = clean_anchors(&request.anchors)?;
        let refs = clean_refs(&request.refs)?;
        let keywords = domain::clean_keywords(&request.keywords)?;
        check_secret("title", &title)?;
        check_secret("content", &content)?;
        if request.links.len() > SAVE_LINKS_MAX {
            return Err(MemError::invalid(format!("links: at most {SAVE_LINKS_MAX} at save time")));
        }
        let mut specs = Vec::with_capacity(request.links.len());
        for spec in &request.links {
            if spec.kind.is_system() {
                return Err(MemError::invalid("links: supersedes is written by replaces, never linked directly"));
            }
            check_record_id("links.to", &spec.to)?;
            specs.push(LinkSpec { to: spec.to.clone(), kind: spec.kind, note: clean_note(spec.note.as_deref())? });
        }
        if let Some(old) = &request.replaces {
            check_record_id("replaces", old)?;
        }
        let proofs = evidence::check_all(&request.evidence, Relation::DerivedFrom)?;
        // Before the write: a check may refresh the code index, never under the store's lock.
        let (code, anchored) = self.check_code(target, actor, request.kind, &title, &content, &anchors, &proofs).await?;
        let held = code.as_ref().filter(|c| c.held).map(|c| c.reason(REASON_MAX));
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let (place, registered) = self.place_for_write(&mut conn, target).await?;
            let project = place.project.clone().ok_or_else(|| MemError::store("the resolved project vanished"))?;
            let hash = store::content_hash(&title, &content);
            if let Some(existing) = store::live_by_hash(&mut conn, &project.id, &hash).await? {
                let memory = in_scope(&mut conn, &project.id, &existing).await?;
                return Ok((memory, true, place, registered, project));
            }
            let replaced = match &request.replaces {
                Some(old) => {
                    let old = in_scope(&mut conn, &project.id, old).await?;
                    if !old.status.is_live() {
                        return Err(MemError::invalid(format!(
                            "replaces: {} is {}; only a live note can be replaced",
                            old.id,
                            old.status.as_str()
                        )));
                    }
                    Some(old)
                }
                None => None,
            };
            if replaced.is_none() && !request.allow_similar {
                if let Some((id, revision, existing)) =
                    store::live_by_title(&mut conn, &project.id, &store::title_norm(&title)).await?
                {
                    return Err(MemError::new(
                        Code::SimilarExists,
                        format!(
                            "a live note already has this title: {id} (revision {revision}). Update it, supersede it \
                             with replaces=\"{id}\", or pass allowSimilar"
                        ),
                    )
                    .with("similar", json!({"id": id, "revision": revision, "title": existing})));
                }
            }
            fresh::baseline(place.root.as_deref(), &mut anchors);
            let filed = match request.domain.as_deref().map(str::trim) {
                Some("" | "none") => None,
                Some(selector) => Some(domain::resolve(&mut conn, &project.id, selector).await?),
                None => match domain::auto_file(&mut conn, &project.id, &anchors).await? {
                    Some(owner) => Some(owner),
                    None => match replaced.as_ref().and_then(|old| old.domain.as_deref()) {
                        Some(id) => store::domain(&mut conn, id).await?,
                        None => None,
                    },
                },
            };
            let (id, now) = match &minted {
                Some((id, at)) if store::memory(&mut conn, id).await?.is_none() => (id.clone(), *at),
                Some((_, at)) => (new_record_id(), *at),
                None => (new_record_id(), store::now()),
            };
            let memory = Memory {
                id,
                scope: project.id.clone(),
                kind: request.kind,
                title: title.clone(),
                content: content.clone(),
                status: if held.is_some() { Status::NeedsReview } else { Status::Active },
                origin: actor.origin,
                actor: actor.label.clone(),
                producer: actor.producer,
                transport: actor.transport,
                confirmed: actor.confirms(),
                source_verified: false,
                anchors: anchors.clone(),
                refs: refs.clone(),
                keywords: keywords.clone(),
                domain: filed.as_ref().map(|d| d.id.clone()),
                domain_path: filed.as_ref().map(|d| d.path.clone()),
                superseded_by: None,
                revision: 1,
                created_ms: now,
                updated_ms: now,
                verified_ms: None,
            };
            store::insert_memory(&mut conn, &memory).await?;
            store::insert_revision(&mut conn, &memory.id, &revision_of(&memory, "create", actor, held.clone(), now)).await?;
            attach(&mut conn, &memory, &proofs, Relation::DerivedFrom, place.root.as_deref(), actor, now).await?;
            for spec in &specs {
                let to = in_scope(&mut conn, &project.id, &spec.to).await?;
                let link = normalized(&memory.id, &to.id, spec.kind, spec.note.clone(), &actor.label, now);
                if store::link(&mut conn, &link.from, &link.to, link.kind).await?.is_some() {
                    continue;
                }
                ensure_capacity(&mut conn, &to.id).await?;
                store::insert_link(&mut conn, &link).await?;
            }
            if let Some(mut old) = replaced {
                old.status = Status::Superseded;
                old.superseded_by = Some(memory.id.clone());
                old.revision += 1;
                old.updated_ms = now;
                store::update_memory(&mut conn, &old).await?;
                let reason = Some(format!("replaced by {}", memory.id));
                store::insert_revision(&mut conn, &old.id, &revision_of(&old, "supersede", actor, reason, now)).await?;
                ensure_capacity(&mut conn, &old.id).await?;
                store::insert_link(
                    &mut conn,
                    &Link {
                        from: memory.id.clone(),
                        to: old.id.clone(),
                        kind: LinkKind::Supersedes,
                        note: None,
                        actor: actor.label.clone(),
                        created_ms: now,
                    },
                )
                .await?;
            }
            Ok((memory, false, place, registered, project))
        }
        .await;
        let (memory, duplicate, place, registered, project) = store::finish(&mut conn, outcome).await?;
        self.touched();
        let record = view_one(&mut conn, &memory, place.root.clone()).await?;
        let contradicts = if duplicate { Vec::new() } else { opposed_hints(&mut conn, &project.id, &memory, &specs).await? };
        let mut similar = if duplicate { Vec::new() } else { similar_hints(&mut conn, &project.id, &memory).await? };
        similar.retain(|s| !contradicts.iter().any(|o| o.id == s.id));
        let mut formation = if duplicate { Vec::new() } else { formation::lint(memory.kind, &memory.title, &memory.content) };
        if code.is_some() {
            // The code check says it, re-counted where it can be.
            formation.retain(|l| l.rule != "quantifier");
        }
        if !duplicate {
            if let Some(lint) = formation::restates_code(&content, &anchored) {
                formation.push(lint);
            }
            // A person typing their own rule in the panel needs no quote of themselves.
            if !memory.confirmed {
                formation.extend(formation::user_words(memory.kind, &memory.content));
            }
        }
        Ok(SaveOutcome {
            record,
            duplicate,
            similar,
            contradicts,
            project: ProjectRef { id: project.id, name: project.name, registered },
            formation,
            code: if duplicate { None } else { code },
        })
    }

    /// One note in full, with its neighbourhood; revisions when asked.
    pub async fn get(&self, target: &Target, id: &str, history: bool) -> MemResult<(RecordView, Option<Vec<Revision>>)> {
        check_record_id("id", id)?;
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_id = place.project_id().map_err(|_| no_note(id))?;
        let memory = in_scope(&mut conn, &project_id, id).await?;
        let record = view_one(&mut conn, &memory, place.root.clone()).await?;
        let revisions = if history { Some(store::revisions(&mut conn, id).await?) } else { None };
        Ok((record, revisions))
    }

    /// A scope listing, newest first: (items, truncated).
    pub async fn list(&self, target: &Target, request: &ListRequest) -> MemResult<(Vec<Summary>, bool)> {
        let limit = request.limit.unwrap_or(LIST_LIMIT_DEFAULT).clamp(1, LIST_LIMIT_MAX);
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok((Vec::new(), false));
        };
        let in_domain = domain::filter(&mut conn, &project_id, request.domain.as_deref()).await?;
        let scope = self.scope(&mut conn, &project_id).await?;
        let (all, net) = (&scope.all, &scope.net);
        let statuses = if request.statuses.is_empty() { search::statuses(request.history) } else { request.statuses.clone() };
        let now = store::now();
        let mut checker = Checker::disabled();
        let matched: Vec<&Memory> = all
            .iter()
            .filter(|m| statuses.contains(&m.status))
            .filter(|m| request.kinds.is_empty() || request.kinds.contains(&m.kind))
            .filter(|m| !request.unconfirmed || !m.confirmed)
            .filter(|m| in_domain.admits(m))
            .collect();
        let truncated = matched.len() > limit;
        let items = matched
            .into_iter()
            .take(limit)
            .map(|m| {
                let (marks, _) = graph::markers(m, net, &mut checker, now);
                view::summary(m, &marks, net, now, true)
            })
            .collect();
        Ok((items, truncated))
    }

    /// Edit a note (CAS on `expectedRevision`).
    pub async fn update(&self, target: &Target, actor: &Actor, id: &str, request: UpdateRequest) -> MemResult<RecordView> {
        Ok(self.update_checked(target, actor, id, request).await?.record)
    }

    /// [Self::update], with the code check of the text it leaves (ADR-032 phase 7): an
    /// agent's edit that fails it holds the note as needs_review, and an edit that passes
    /// releases a note the check held - so an agent fixes its own held note by editing it,
    /// and no person need confirm it.
    pub async fn update_checked(&self, target: &Target, actor: &Actor, id: &str, request: UpdateRequest) -> MemResult<UpdateOutcome> {
        check_record_id("id", id)?;
        let content_change = request.kind.is_some()
            || request.title.is_some()
            || request.content.is_some()
            || request.anchors.is_some()
            || request.refs.is_some();
        // Keywords are an edit (a revision, a compare-and-swap) the code check has nothing to read in.
        let edits = content_change || request.keywords.is_some();
        if !edits && request.domain.is_none() && request.evidence.is_empty() {
            return Err(MemError::invalid(
                "nothing to update: pass kind, title, content, anchors, refs, keywords, domain or evidence",
            ));
        }
        let keywords = request.keywords.as_deref().map(domain::clean_keywords).transpose()?;
        let proofs = evidence::check_all(&request.evidence, Relation::DerivedFrom)?;
        let title = request.title.as_deref().map(clean_title).transpose()?;
        let content = request.content.as_deref().map(clean_content).transpose()?;
        let anchors = request.anchors.as_deref().map(clean_anchors).transpose()?;
        let refs = request.refs.as_deref().map(clean_refs).transpose()?;
        let reason = clean_reason(request.reason.as_deref(), false)?;
        if let Some(title) = &title {
            check_secret("title", title)?;
        }
        if let Some(content) = &content {
            check_secret("content", content)?;
        }
        // Before the write, as a save does: the text the edit leaves, against the checkout.
        let (code, _) = if content_change {
            let now = {
                let mut conn = self.store().await?.read().await?;
                let place = self.locate(&mut conn, target).await?;
                let project_id = place.project_id().map_err(|_| no_note(id))?;
                in_scope(&mut conn, &project_id, id).await?
            };
            let kind = request.kind.unwrap_or(now.kind);
            let (title, content) = (title.as_deref().unwrap_or(&now.title), content.as_deref().unwrap_or(&now.content));
            self.check_code(target, actor, kind, title, content, anchors.as_deref().unwrap_or(&now.anchors), &proofs).await?
        } else {
            (None, String::new())
        };
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut memory = in_scope(&mut conn, &project_id, id).await?;
            if !memory.status.is_live() {
                return Err(MemError::invalid(format!(
                    "{id} is {}; restore it before editing",
                    memory.status.as_str()
                )));
            }
            let filed = match request.domain.as_deref().map(str::trim) {
                None => None,
                Some("" | "none") => Some(None),
                Some(selector) => Some(Some(domain::resolve(&mut conn, &project_id, selector).await?)),
            };
            if !edits {
                // Filing or evidence alone: no compare-and-swap, no revision.
                if let Some(to) = filed {
                    store::file_memory(&mut conn, id, to.as_ref().map(|d| d.id.as_str())).await?;
                    memory.domain = to.as_ref().map(|d| d.id.clone());
                    memory.domain_path = to.map(|d| d.path);
                }
                attach(&mut conn, &memory, &proofs, Relation::DerivedFrom, place.root.as_deref(), actor, store::now()).await?;
                return Ok((memory, place));
            }
            if memory.revision != request.expected_revision {
                return Err(MemError::new(
                    Code::RevisionConflict,
                    format!(
                        "{id} is at revision {}, not {}; re-read it and retry",
                        memory.revision, request.expected_revision
                    ),
                )
                .with("current", json!(memory.revision)));
            }
            let text_was = (memory.title.clone(), memory.content.clone());
            if let Some(kind) = request.kind {
                memory.kind = kind;
            }
            if let Some(title) = &title {
                memory.title = title.clone();
            }
            if let Some(content) = &content {
                memory.content = content.clone();
            }
            if let Some(anchors) = &anchors {
                memory.anchors = anchors.clone();
            }
            if let Some(refs) = &refs {
                memory.refs = refs.clone();
            }
            if let Some(keywords) = &keywords {
                memory.keywords = keywords.clone();
            }
            fresh::baseline(place.root.as_deref(), &mut memory.anchors);
            let explicit = filed.is_some();
            let to = match filed {
                Some(to) => to,
                // New anchors file a note nobody filed yet; a filed one stays put.
                None if anchors.is_some() && memory.domain.is_none() => {
                    domain::auto_file(&mut conn, &project_id, &memory.anchors).await?
                }
                None => None,
            };
            if explicit || to.is_some() {
                memory.domain = to.as_ref().map(|d| d.id.clone());
                memory.domain_path = to.map(|d| d.path);
            }
            let mut reason = reason.clone();
            if let Some(check) = &code {
                if check.held {
                    memory.status = Status::NeedsReview;
                    reason = Some(with_reason(reason, &check.reason(REASON_MAX)));
                } else if memory.status == Status::NeedsReview {
                    // Evidence stays bound to the text it came with: a quote that failed backs
                    // the note until an edit to its text leaves it behind, or the edit cites a
                    // file the check just read.
                    if let Some(why) = held_by_check(&mut conn, &memory).await? {
                        let text_moved = text_was != (memory.title.clone(), memory.content.clone());
                        let cites = proofs.iter().any(|p| p.path.is_some());
                        if text_moved || cites || !EVIDENCE_RULES.iter().any(|rule| why.contains(rule)) {
                            memory.status = Status::Active;
                            reason = Some(with_reason(reason, RELEASED));
                        } else {
                            reason = Some(with_reason(reason, &why));
                        }
                    }
                }
            }
            let now = store::now();
            memory.revision += 1;
            memory.updated_ms = now;
            store::update_memory(&mut conn, &memory).await?;
            store::insert_revision(&mut conn, id, &revision_of(&memory, "edit", actor, reason, now)).await?;
            attach(&mut conn, &memory, &proofs, Relation::DerivedFrom, place.root.as_deref(), actor, now).await?;
            Ok((memory, place))
        }
        .await;
        let (memory, place) = store::finish(&mut conn, outcome).await?;
        self.touched();
        Ok(UpdateOutcome { record: view_one(&mut conn, &memory, place.root).await?, code })
    }

    /// Agent or human feedback: verified, outdated, wrong. No CAS: feedback must not
    /// fail on a race.
    pub async fn mark(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        verdict: Verdict,
        reason: Option<&str>,
    ) -> MemResult<RecordView> {
        self.mark_with(target, actor, id, verdict, reason, Vec::new()).await
    }

    /// [Self::mark] with the evidence a verified verdict rests on (ADR-032): `verified_by`
    /// unless a piece says otherwise, bound to the revision the mark writes. An outdated or
    /// wrong verdict carries its why in `reason`, never evidence.
    pub async fn mark_with(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        verdict: Verdict,
        reason: Option<&str>,
        proofs: Vec<EvidenceSpec>,
    ) -> MemResult<RecordView> {
        self.mark_guarded(target, actor, id, verdict, reason, proofs, None).await
    }

    /// A recheck's verdict on a note at the revision its question showed (SPEC
    /// §memory.init The recheck): [Self::mark]'s write, guarded inside the transaction -
    /// the note is re-read, and the verdict refused when it moved (any edit, confirm,
    /// verify or mark since the question bumps it) or left the recheck's scope
    /// ([crate::recheck::Reach::holds] for the question's `reach`). Feedback that arrives
    /// on its own never fails on a race; only a verdict captured before an answer waited does.
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_rechecked(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        expected_revision: u64,
        verdict: Verdict,
        reason: Option<&str>,
        reach: crate::recheck::Reach,
    ) -> MemResult<RecordView> {
        self.mark_guarded(target, actor, id, verdict, reason, Vec::new(), Some((expected_revision, reach))).await
    }

    /// [Self::mark_with]'s body; `expect` is the recheck's captured revision and reach,
    /// checked inside the write transaction (None: ordinary feedback, no CAS by design).
    #[allow(clippy::too_many_arguments)]
    async fn mark_guarded(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        verdict: Verdict,
        reason: Option<&str>,
        proofs: Vec<EvidenceSpec>,
        expect: Option<(u64, crate::recheck::Reach)>,
    ) -> MemResult<RecordView> {
        check_record_id("id", id)?;
        let reason = clean_reason(reason, verdict != Verdict::Verified)?;
        if !proofs.is_empty() && verdict != Verdict::Verified {
            return Err(MemError::invalid(
                "evidence backs a verified verdict; an outdated or wrong one says why in its reason",
            ));
        }
        let proofs = evidence::check_all(&proofs, Relation::VerifiedBy)?;
        if verdict == Verdict::Verified && matches!(actor.producer, Producer::Agent | Producer::Bootstrap | Producer::Sync) {
            // A model releases a note the check holds by fixing it, not by vouching for it; a
            // person (or a CLI caller who declares nothing) may still accept it.
            let now = {
                let mut conn = self.store().await?.read().await?;
                let place = self.locate(&mut conn, target).await?;
                let project_id = place.project_id().map_err(|_| no_note(id))?;
                let memory = in_scope(&mut conn, &project_id, id).await?;
                let held = if memory.status == Status::NeedsReview { held_by_check(&mut conn, &memory).await? } else { None };
                held.map(|why| (memory, why))
            };
            if let Some((memory, why)) = now {
                if EVIDENCE_RULES.iter().any(|rule| why.contains(rule)) {
                    // A mark keeps the evidence current: the quote that failed would back it still.
                    return Err(MemError::invalid(format!(
                        "{id} is {why}; fix its text and evidence with an update - a verified mark does not release it"
                    )));
                }
                let (check, _) = self
                    .check_code(target, actor, memory.kind, &memory.title, &memory.content, &memory.anchors, &proofs)
                    .await?;
                if let Some(check) = check.filter(|c| c.held) {
                    return Err(MemError::invalid(format!(
                        "{id} is still {}; fix it with an update - a verified mark does not release it",
                        check.reason(REASON_MAX)
                    ))
                    .with("check", json!(check)));
                }
            }
        }
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut memory = in_scope(&mut conn, &project_id, id).await?;
            if let Some((expected, reach)) = expect {
                recheck_guard(&memory, expected, reach)?;
            }
            if !memory.status.is_live() {
                return Err(MemError::invalid(format!(
                    "{id} is {}; restore it before marking it",
                    memory.status.as_str()
                )));
            }
            let now = store::now();
            let op = match verdict {
                Verdict::Verified => {
                    memory.status = Status::Active;
                    memory.verified_ms = Some(now);
                    fresh::baseline(place.root.as_deref(), &mut memory.anchors);
                    "mark_verified"
                }
                Verdict::Outdated => {
                    memory.status = Status::NeedsReview;
                    "mark_outdated"
                }
                Verdict::Wrong => {
                    memory.status = Status::Invalidated;
                    "invalidate"
                }
            };
            memory.revision += 1;
            memory.updated_ms = now;
            store::update_memory(&mut conn, &memory).await?;
            store::insert_revision(&mut conn, id, &revision_of(&memory, op, actor, reason.clone(), now)).await?;
            attach(&mut conn, &memory, &proofs, Relation::VerifiedBy, place.root.as_deref(), actor, now).await?;
            Ok((memory, place))
        }
        .await;
        let (memory, place) = store::finish(&mut conn, outcome).await?;
        view_one(&mut conn, &memory, place.root).await
    }

    /// A person confirms a note (CLI and panel only). Idempotent.
    pub async fn confirm(&self, target: &Target, actor: &Actor, id: &str) -> MemResult<RecordView> {
        check_record_id("id", id)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut memory = in_scope(&mut conn, &project_id, id).await?;
            if !memory.confirmed {
                let now = store::now();
                memory.confirmed = true;
                memory.revision += 1;
                memory.updated_ms = now;
                store::update_memory(&mut conn, &memory).await?;
                store::insert_revision(&mut conn, id, &revision_of(&memory, "confirm", actor, None, now)).await?;
            }
            Ok((memory, place))
        }
        .await;
        let (memory, place) = store::finish(&mut conn, outcome).await?;
        view_one(&mut conn, &memory, place.root).await
    }

    /// A review run's confirm (SPEC §memory.init The recheck): [Self::confirm]'s write on a
    /// note the run's answer just left verified, at the revision that mark wrote, with the
    /// run in its reason - refused when the note moved since or left the inbox's reach.
    /// Out: whether it was confirmed now (false: it already was).
    pub async fn confirm_rechecked(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        expected_revision: u64,
        reason: &str,
    ) -> MemResult<bool> {
        check_record_id("id", id)?;
        let reason = clean_reason(Some(reason), true)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut memory = in_scope(&mut conn, &project_id, id).await?;
            recheck_guard(&memory, expected_revision, crate::recheck::Reach::Inbox)?;
            if memory.confirmed {
                return Ok(false);
            }
            let now = store::now();
            memory.confirmed = true;
            memory.revision += 1;
            memory.updated_ms = now;
            store::update_memory(&mut conn, &memory).await?;
            store::insert_revision(&mut conn, id, &revision_of(&memory, "confirm", actor, reason.clone(), now)).await?;
            Ok(true)
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// Bring an invalidated or superseded note back (CLI and panel only).
    pub async fn restore(&self, target: &Target, actor: &Actor, id: &str, reason: &str) -> MemResult<RecordView> {
        check_record_id("id", id)?;
        let reason = clean_reason(Some(reason), true)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut memory = in_scope(&mut conn, &project_id, id).await?;
            if memory.status.is_live() {
                return Err(MemError::invalid(format!("{id} is {}; only history can be restored", memory.status.as_str())));
            }
            let now = store::now();
            memory.status = Status::Active;
            memory.superseded_by = None;
            memory.revision += 1;
            memory.updated_ms = now;
            store::update_memory(&mut conn, &memory).await?;
            store::insert_revision(&mut conn, id, &revision_of(&memory, "restore", actor, reason.clone(), now)).await?;
            Ok((memory, place))
        }
        .await;
        let (memory, place) = store::finish(&mut conn, outcome).await?;
        self.touched();
        view_one(&mut conn, &memory, place.root).await
    }

    /// Fold one note into another that says it (SPEC §memory.init The recheck): `id` is
    /// superseded by `by`, with the supersedes link a save that replaces writes. Both live,
    /// in the one project, never the same note; `restore` brings `id` back.
    pub async fn supersede(&self, target: &Target, actor: &Actor, id: &str, by: &str, reason: &str) -> MemResult<RecordView> {
        self.supersede_guarded(target, actor, id, by, reason, None).await
    }

    /// A recheck's merge at the revision its question showed: [Self::supersede]'s write,
    /// guarded on the superseded note exactly like [Self::mark_rechecked].
    #[allow(clippy::too_many_arguments)]
    pub async fn supersede_rechecked(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        by: &str,
        expected_revision: u64,
        reason: &str,
        reach: crate::recheck::Reach,
    ) -> MemResult<RecordView> {
        self.supersede_guarded(target, actor, id, by, reason, Some((expected_revision, reach))).await
    }

    /// The body both supersedes share; `expect` is the recheck's captured revision and
    /// reach for the note being superseded, checked inside the write transaction.
    async fn supersede_guarded(
        &self,
        target: &Target,
        actor: &Actor,
        id: &str,
        by: &str,
        reason: &str,
        expect: Option<(u64, crate::recheck::Reach)>,
    ) -> MemResult<RecordView> {
        check_record_id("id", id)?;
        check_record_id("by", by)?;
        if id == by {
            return Err(MemError::invalid(format!("{id} cannot be superseded by itself")));
        }
        let reason = clean_reason(Some(reason), true)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            let mut old = in_scope(&mut conn, &project_id, id).await?;
            if let Some((expected, reach)) = expect {
                recheck_guard(&old, expected, reach)?;
            }
            let keeper = in_scope(&mut conn, &project_id, by).await?;
            for memory in [&old, &keeper] {
                if !memory.status.is_live() {
                    return Err(MemError::invalid(format!(
                        "{} is {}; only a live note supersedes or is superseded",
                        memory.id,
                        memory.status.as_str()
                    )));
                }
            }
            let now = store::now();
            old.status = Status::Superseded;
            old.superseded_by = Some(keeper.id.clone());
            old.revision += 1;
            old.updated_ms = now;
            store::update_memory(&mut conn, &old).await?;
            store::insert_revision(&mut conn, id, &revision_of(&old, "supersede", actor, reason.clone(), now)).await?;
            if store::link(&mut conn, &keeper.id, &old.id, LinkKind::Supersedes).await?.is_none() {
                ensure_capacity(&mut conn, &keeper.id).await?;
                ensure_capacity(&mut conn, &old.id).await?;
                let link = Link {
                    from: keeper.id.clone(),
                    to: old.id.clone(),
                    kind: LinkKind::Supersedes,
                    note: None,
                    actor: actor.label.clone(),
                    created_ms: now,
                };
                store::insert_link(&mut conn, &link).await?;
            }
            Ok((old, place))
        }
        .await;
        let (memory, place) = store::finish(&mut conn, outcome).await?;
        self.touched();
        view_one(&mut conn, &memory, place.root).await
    }

    /// Physical delete (CLI and panel only): the caller echoes the id.
    pub async fn purge(&self, target: &Target, id: &str, confirm: &str) -> MemResult<()> {
        check_record_id("id", id)?;
        if confirm.trim() != id {
            return Err(MemError::invalid(format!("confirm: pass the note id ({id}) to delete it for good")));
        }
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(id))?;
            in_scope(&mut conn, &project_id, id).await?;
            store::delete_memory(&mut conn, id).await?;
            Ok(())
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    // --- links ------------------------------------------------------------------------

    /// Add (or re-note) one typed link: (edge, changed).
    pub async fn link(
        &self,
        target: &Target,
        actor: &Actor,
        from: &str,
        to: &str,
        kind: LinkKind,
        note: Option<&str>,
    ) -> MemResult<(GraphEdge, bool)> {
        check_link(from, to, kind)?;
        let note = clean_note(note)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(from))?;
            in_scope(&mut conn, &project_id, from).await?;
            in_scope(&mut conn, &project_id, to).await?;
            let link = normalized(from, to, kind, note.clone(), &actor.label, store::now());
            match store::link(&mut conn, &link.from, &link.to, kind).await? {
                Some(existing) => {
                    if note.is_some() && existing.note != note {
                        let updated = Link { note: note.clone(), actor: actor.label.clone(), ..existing };
                        store::set_link_note(&mut conn, &updated).await?;
                        Ok((updated, true))
                    } else {
                        Ok((existing, false))
                    }
                }
                None => {
                    ensure_capacity(&mut conn, from).await?;
                    ensure_capacity(&mut conn, to).await?;
                    store::insert_link(&mut conn, &link).await?;
                    Ok((link, true))
                }
            }
        }
        .await;
        let (link, changed) = store::finish(&mut conn, outcome).await?;
        Ok((GraphEdge::of(&link), changed))
    }

    /// Remove one typed link: changed.
    pub async fn unlink(&self, target: &Target, from: &str, to: &str, kind: LinkKind) -> MemResult<bool> {
        check_link(from, to, kind)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id().map_err(|_| no_note(from))?;
            in_scope(&mut conn, &project_id, from).await?;
            in_scope(&mut conn, &project_id, to).await?;
            let (a, b) = if kind.is_symmetric() && to < from { (to, from) } else { (from, to) };
            store::delete_link(&mut conn, a, b, kind).await
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    // --- reads ------------------------------------------------------------------------

    /// Search in one scope: the hits of [MemoryService::hybrid_search].
    pub async fn search(&self, target: &Target, query: &SearchQuery) -> MemResult<Vec<HitView>> {
        Ok(self.hybrid_search(target, query).await?.items)
    }

    /// Search in one scope (SPEC §memory.search): lexical, fused with the semantic
    /// tier when an embedder is set and the scope has current vectors (SPEC
    /// §memory.embedding). The embedder can only add: on failure or timeout the
    /// answer is the lexical one with `semantic.state = "unavailable"`.
    pub async fn hybrid_search(&self, target: &Target, query: &SearchQuery) -> MemResult<SearchOutcome> {
        let text = query.text.trim();
        if text.is_empty() || text.chars().count() > search::QUERY_MAX {
            return Err(MemError::invalid(format!("query: 1..{} characters", search::QUERY_MAX)));
        }
        let embedder = if query.words_only { None } else { self.embedder() };
        let off = || match &embedder {
            Some(e) => SemanticState::unavailable(e.id(), "no note of this scope is indexed yet"),
            None => SemanticState::off(),
        };
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok(SearchOutcome { items: Vec::new(), semantic: off() });
        };
        // A project whose settings turn embedding off reads lexical only (§memory.embedding):
        // no query vector, and the answer says why. An unreadable setting row reads as the
        // default (on): the search degrades, it does not die on the store's bug.
        let project_off = !crate::settings::embedding_on(&mut conn, &project_id).await.unwrap_or(true);
        let embedder = if project_off { None } else { embedder };
        let off = || {
            if project_off {
                return SemanticState::off_because("the project's settings turn embedding off");
            }
            match &embedder {
                Some(e) => SemanticState::unavailable(e.id(), "no note of this scope is indexed yet"),
                None => SemanticState::off(),
            }
        };
        let in_domain = domain::filter(&mut conn, &project_id, query.domain.as_deref()).await?;
        let mut rows = search::rows(&mut conn, &project_id, query).await?;
        rows.retain(|m| in_domain.admits(m));
        let hubbed = search::hubbed(&mut conn, &project_id, &rows).await?;
        let lexical = search::rank(&mut conn, &project_id, query, &rows, &hubbed).await?;
        let vectors = match &embedder {
            Some(e) => current_vectors(&mut conn, &project_id, e.id(), rows.iter()).await?,
            None => HashMap::new(),
        };
        let links = store::links_in(&mut conn, &project_id).await?;
        let statuses = all_statuses(&mut conn, &project_id).await?;
        // The query vector may take seconds: never hold the store's one connection
        // across it.
        drop(conn);
        let limit = query.limit.clamp(1, search::MAX_LIMIT);
        let now = store::now();
        let (mut hits, semantic) = match &embedder {
            Some(e) if !vectors.is_empty() => match self.meaning(e.as_ref(), text, &vectors).await {
                Ok(sims) => (search::fuse(lexical, &rows, &sims, e.floor(), now, &hubbed), SemanticState::on(e.id())),
                Err(reason) => (lexical, SemanticState::unavailable(e.id(), reason)),
            },
            _ => (lexical, off()),
        };
        hits.truncate(limit);
        let net = Net::new(statuses, links);
        let mut checker = Checker::disabled();
        let items = hits
            .into_iter()
            .map(|hit| {
                let (marks, _) = graph::markers(&hit.memory, &net, &mut checker, now);
                HitView {
                    summary: view::summary(&hit.memory, &marks, &net, now, true),
                    score: hit.score,
                    tier: hit.tier,
                    similarity: hit.similarity.map(round3),
                    word_share: hit.share.map(|s| (s * 1000.0).round() / 1000.0),
                }
            })
            .collect();
        Ok(SearchOutcome { items, semantic })
    }

    /// Association search (SPEC §memory.related). A project whose settings turn
    /// embedding off (§memory.embedding) gets no similar-meaning tier, and the answer
    /// says why.
    pub async fn related(&self, target: &Target, request: &RelatedRequest) -> MemResult<RelatedOutcome> {
        match &request.seed {
            Seed::Id(id) => check_record_id("id", id)?,
            Seed::Text(text) => {
                if text.trim().is_empty() {
                    return Err(MemError::invalid("text: pass a passage to associate"));
                }
            }
        }
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_id = match (place.project_id(), &request.seed) {
            (Ok(id), _) => id,
            (Err(_), Seed::Id(id)) => return Err(no_note(id)),
            (Err(_), Seed::Text(_)) => return Ok(RelatedOutcome { items: Vec::new(), semantic: None }),
        };
        let scope = self.scope(&mut conn, &project_id).await?;
        let (all, net) = (&scope.all, &scope.net);
        if let Seed::Id(id) = &request.seed {
            if !all.iter().any(|m| &m.id == id) {
                return Err(no_note(id));
            }
        }
        // "Similar meaning": an id seed compares its own stored vector (no provider
        // call); a text seed is embedded like a query, between two acquisitions. A project
        // whose settings turn embedding off never asks one.
        let project_off = !crate::settings::embedding_on(&mut conn, &project_id).await.unwrap_or(true);
        let mut similar: HashMap<String, f32> = HashMap::new();
        let mut floor = embed::SIMILARITY_FLOOR;
        if let Some(e) = self.embedder().filter(|_| !project_off) {
            floor = e.floor();
            let visible = all.iter().filter(|m| request.include_history || m.status.is_live());
            let vectors = current_vectors(&mut conn, &project_id, e.id(), visible).await?;
            let seed_vector = match &request.seed {
                Seed::Id(id) => vectors.get(id).cloned(),
                Seed::Text(text) if !vectors.is_empty() => {
                    drop(conn);
                    let vector = query_vector(e.as_ref(), related_head(text)).await.ok();
                    if let Some(vector) = &vector {
                        self.saw_width(e.id(), vector.len());
                    }
                    conn = self.store().await?.read().await?;
                    vector
                }
                Seed::Text(_) => None,
            };
            if let Some(seed) = seed_vector {
                similar = self
                    .scores(&seed, &vectors)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, sim)| *sim >= floor)
                    .collect();
            }
        }
        let found = related::run(
            &mut conn,
            &project_id,
            all,
            net,
            &related::Request {
                seed: request.seed.clone(),
                depth: request.depth.unwrap_or(related::DEPTH_DEFAULT),
                limit: request.limit.unwrap_or(related::LIMIT_DEFAULT),
                include_history: request.include_history,
                similar,
                floor,
            },
        )
        .await?;
        let by_id: HashMap<&str, &Memory> = all.iter().map(|m| (m.id.as_str(), m)).collect();
        let now = store::now();
        let mut checker = Checker::disabled();
        let items = found
            .into_iter()
            .filter_map(|hit| {
                let memory = by_id.get(hit.id.as_str())?;
                let (marks, _) = graph::markers(memory, net, &mut checker, now);
                Some(RelatedView { summary: view::summary(memory, &marks, net, now, true), score: hit.score, reasons: hit.reasons })
            })
            .collect();
        Ok(RelatedOutcome {
            items,
            semantic: project_off.then(|| SemanticState::off_because("the project's settings turn embedding off")),
        })
    }

    /// The graph read (SPEC §memory.graph).
    pub async fn graph(&self, target: &Target, request: &GraphRequest) -> MemResult<GraphView> {
        if let Some(center) = &request.center {
            check_record_id("center", center)?;
        }
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_id = match (place.project_id(), &request.center) {
            (Ok(id), _) => id,
            (Err(_), Some(center)) => return Err(no_note(center)),
            (Err(_), None) => return Ok(GraphView { nodes: Vec::new(), edges: Vec::new(), truncated: false }),
        };
        let scope = self.scope(&mut conn, &project_id).await?;
        let net = &scope.net;
        if let Some(center) = &request.center {
            if !net.nodes.contains_key(center) {
                return Err(no_note(center));
            }
        }
        Ok(graph::read(
            net,
            request.center.as_deref(),
            request.depth.unwrap_or(1),
            request.include_history,
            request.limit.unwrap_or(graph::GRAPH_LIMIT_DEFAULT),
        ))
    }

    /// The review inbox: live notes carrying any marker (SPEC §memory.freshness). Every
    /// anchored file is checked, not a read's first [crate::fresh::CHECK_CAP].
    pub async fn review(&self, target: &Target) -> MemResult<Vec<Summary>> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok(Vec::new());
        };
        let scope = self.scope(&mut conn, &project_id).await?;
        let (all, net) = (&scope.all, &scope.net);
        drop(conn);
        let now = store::now();
        let anchors = all.iter().filter(|m| m.status.is_live()).flat_map(|m| &m.anchors);
        let mut checker = Checker::whole(place.root.clone(), anchors).await;
        Ok(all
            .iter()
            .filter(|m| m.status.is_live())
            .filter_map(|m| {
                let (marks, _) = graph::markers(m, net, &mut checker, now);
                (!marks.is_empty()).then(|| view::summary(m, &marks, net, now, true))
            })
            .collect())
    }

    /// The brief (SPEC §memory.brief).
    pub async fn brief(&self, target: &Target, request: &BriefRequest) -> MemResult<BriefOutput> {
        if request.paths.len() > brief::PATHS_MAX {
            return Err(MemError::invalid(format!("paths: at most {}", brief::PATHS_MAX)));
        }
        let task = request.task.as_deref().map(str::trim).filter(|t| !t.is_empty());
        if task.map(|t| t.chars().count() > brief::TASK_MAX).unwrap_or(false) {
            return Err(MemError::invalid(format!("task: at most {} characters", brief::TASK_MAX)));
        }
        let max_chars = request.max_chars.unwrap_or(brief::MAX_CHARS_DEFAULT);
        if !(brief::MAX_CHARS_MIN..=brief::MAX_CHARS_MAX).contains(&max_chars) {
            return Err(MemError::invalid(format!(
                "maxChars: {}..{}",
                brief::MAX_CHARS_MIN,
                brief::MAX_CHARS_MAX
            )));
        }
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_scope: Arc<Loaded> = match place.project_id() {
            Ok(id) => self.scope(&mut conn, &id).await?,
            Err(_) => Arc::default(),
        };
        let (memories, net) = (&project_scope.all, &project_scope.net);
        let domains = match place.project_id() {
            Ok(id) if id != GLOBAL_PROJECT_ID => store::domains(&mut conn, &id).await?,
            _ => Vec::new(),
        };
        let is_global = place.scope == Some(Scope::Global);
        let global_scope: Arc<Loaded> =
            if request.exclude_global || is_global { Arc::default() } else { self.scope(&mut conn, GLOBAL_PROJECT_ID).await? };
        let global = global_scope
            .all
            .iter()
            .filter(|m| m.status.is_live() && matches!(m.kind, Kind::Preference | Kind::Constraint))
            .cloned()
            .collect::<Vec<_>>();
        let global_net = &global_scope.net;
        let (exact, mut task_hits, task_shares) = match (task, place.project_id()) {
            (Some(task), Ok(id)) => task_hits(&mut conn, &id, task, memories).await?,
            _ => (Vec::new(), Vec::new(), HashMap::new()),
        };
        let embedder = self.embedder();
        // A project whose settings turn embedding off reads lexical only (§memory.embedding):
        // no task vector, and the answer says why.
        let project_off = match place.project_id() {
            Ok(id) => !crate::settings::embedding_on(&mut conn, &id).await.unwrap_or(true),
            Err(_) => false,
        };
        let embedder = if project_off { None } else { embedder };
        let task_vectors = match (task, &embedder, place.project_id()) {
            (Some(_), Some(e), Ok(id)) => {
                current_vectors(&mut conn, &id, e.id(), memories.iter().filter(|m| m.status.is_live())).await?
            }
            _ => HashMap::new(),
        };
        let notice = match (&place.scope, &place.found) {
            (Some(_), _) => None,
            (None, Some(found)) => {
                let mut text =
                    format!("Not a registered project ({}); the first save registers it.", scope::display(&found.root));
                if let Some(hint) = reclone_hint(&mut conn, found).await? {
                    text.push(' ');
                    text.push_str(&hint);
                }
                Some(text)
            }
            (None, None) => Some(format!(
                "No project at {}: not inside a Git repository or a bound directory. Run `swiss memory init --here` to bind it.",
                place.dir.as_deref().map(scope::display).unwrap_or_default()
            )),
        };
        drop(conn);
        let semantic = match (task, &embedder, project_off) {
            (None, _, _) => None,
            (_, _, true) => Some(SemanticState::off_because("the project's settings turn embedding off")),
            (Some(_), None, false) => Some(SemanticState::off()),
            (Some(_), Some(e), false) if task_vectors.is_empty() => {
                Some(SemanticState::unavailable(e.id(), "no note of this scope is indexed yet"))
            }
            (Some(task), Some(e), false) => match self.meaning(e.as_ref(), task, &task_vectors).await {
                Ok(sims) => {
                    task_hits = fuse_ids(&task_hits, &sims, &exact, e.floor(), TASK_HITS);
                    Some(SemanticState::on(e.id()))
                }
                Err(reason) => Some(SemanticState::unavailable(e.id(), reason)),
            },
        };
        // The notes holding what the task names lead; meaning never reorders them.
        let exact_hits = exact.len().min(TASK_HITS);
        let task_hits: Vec<String> = exact.into_iter().chain(task_hits).take(TASK_HITS).collect();
        let project = place.project.as_ref().map(|p| (p.id.as_str(), p.name.as_str()));
        let rendered = brief::compose(BriefInput {
            project,
            notice,
            memories,
            net,
            domains: &domains,
            checker: Checker::new(place.root.clone()),
            paths: request.paths.clone(),
            task: task.map(str::to_string),
            task_hits,
            exact_hits,
            task_shares,
            global: &global,
            global_net,
            max_chars,
            now: store::now(),
        });
        Ok(BriefOutput {
            chars: rendered.markdown.chars().count(),
            markdown: rendered.markdown,
            root: place.root.as_deref().map(scope::display),
            semantic,
            shown: rendered.shown,
        })
    }

    // --- projects ---------------------------------------------------------------------

    /// Every project, global first, then by name.
    pub async fn projects(&self) -> MemResult<Vec<ProjectView>> {
        let mut conn = self.store().await?.read().await?;
        project_views(&mut conn).await
    }

    pub async fn project(&self, id: &str) -> MemResult<ProjectView> {
        let mut conn = self.store().await?.read().await?;
        project_view(&mut conn, id).await
    }

    /// Create a project; with `path`, bind it in the same transaction.
    pub async fn create_project(
        &self,
        name: Option<&str>,
        path: Option<&str>,
        exact: bool,
        git_dir: Option<&Path>,
    ) -> MemResult<ProjectView> {
        let dir = path.map(|p| scope::normalize_dir(p).map_err(MemError::invalid_cwd)).transpose()?;
        let name = match (name, &dir) {
            (Some(name), _) => clean_name(name)?,
            (None, Some(dir)) => view::clip(&scope::default_name(dir), PROJECT_NAME_MAX),
            (None, None) => return Err(MemError::invalid("name is required")),
        };
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let id = new_project_id();
            store::insert_project(&mut conn, &id, &name, store::now()).await?;
            if let Some(dir) = &dir {
                bind_in(&mut conn, &id, dir, exact, git_dir).await?;
            }
            Ok(id)
        }
        .await;
        let id = store::finish(&mut conn, outcome).await?;
        project_view(&mut conn, &id).await
    }

    pub async fn rename_project(&self, id: &str, name: &str) -> MemResult<ProjectView> {
        if id == GLOBAL_PROJECT_ID {
            return Err(MemError::invalid("the global project cannot be renamed"));
        }
        let name = clean_name(name)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if !store::rename_project(&mut conn, id, &name).await? {
                return Err(MemError::not_found(format!("no project {id}")));
            }
            Ok(())
        }
        .await;
        store::finish(&mut conn, outcome).await?;
        project_view(&mut conn, id).await
    }

    /// Delete a project with every note, link and root in it: (records, links).
    pub async fn delete_project(&self, id: &str, confirm: &str) -> MemResult<(u64, u64)> {
        if id == GLOBAL_PROJECT_ID {
            return Err(MemError::invalid("the global project cannot be deleted"));
        }
        if confirm.trim() != id {
            return Err(MemError::invalid(format!("confirm: pass the project id ({id}) to delete it")));
        }
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if store::project(&mut conn, id).await?.is_none() {
                return Err(MemError::not_found(format!("no project {id}")));
            }
            store::delete_project(&mut conn, id).await
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// Bind one more directory to a project (Git root by default, or exactly).
    pub async fn bind_root(&self, id: &str, path: &str, exact: bool, git_dir: Option<&Path>) -> MemResult<ProjectView> {
        let dir = scope::normalize_dir(path).map_err(MemError::invalid_cwd)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if id == GLOBAL_PROJECT_ID {
                return Err(MemError::invalid("the global project has no directories"));
            }
            if store::project(&mut conn, id).await?.is_none() {
                return Err(MemError::not_found(format!("no project {id}")));
            }
            bind_in(&mut conn, id, &dir, exact, git_dir).await
        }
        .await;
        store::finish(&mut conn, outcome).await?;
        project_view(&mut conn, id).await
    }

    /// Change one of a project's directories in one transaction: the notes belong to the
    /// project's id, never to a path, so a moved or renamed checkout keeps them.
    pub async fn rebind_root(
        &self,
        id: &str,
        identity: &str,
        path: &str,
        exact: bool,
        git_dir: Option<&Path>,
    ) -> MemResult<ProjectView> {
        let dir = scope::normalize_dir(path).map_err(MemError::invalid_cwd)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if !store::delete_root(&mut conn, id, identity).await? {
                return Err(MemError::not_found(format!("project {id} has no directory {identity}")));
            }
            bind_in(&mut conn, id, &dir, exact, git_dir).await
        }
        .await;
        store::finish(&mut conn, outcome).await?;
        project_view(&mut conn, id).await
    }

    pub async fn unbind_root(&self, id: &str, identity: &str) -> MemResult<ProjectView> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if !store::delete_root(&mut conn, id, identity).await? {
                return Err(MemError::not_found(format!("project {id} has no directory {identity}")));
            }
            Ok(())
        }
        .await;
        store::finish(&mut conn, outcome).await?;
        project_view(&mut conn, id).await
    }

    /// `swiss memory init` (SPEC §memory.cli).
    pub async fn init(&self, cwd: &str, git_dir: Option<&Path>, mode: InitMode, name: Option<&str>) -> MemResult<InitOutcome> {
        let dir = scope::normalize_dir(cwd).map_err(MemError::invalid_cwd)?;
        let name = name.map(clean_name).transpose()?;
        let mut marker_file = None;
        let found = match mode {
            InitMode::Here => scope::exact(&dir).map_err(MemError::invalid_cwd)?,
            InitMode::Portable => {
                let file = dir.join(scope::MARKER_FILE[0]).join(scope::MARKER_FILE[1]);
                if !file.is_file() {
                    let body = json!({
                        "schemaVersion": 1,
                        "projectId": new_project_id(),
                        "name": name.clone().unwrap_or_else(|| scope::default_name(&dir)),
                    });
                    let text = serde_json::to_string_pretty(&body).map_err(|e| MemError::store(e.to_string()))?;
                    if let Some(parent) = file.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| MemError::store(format!("{}: {e}", parent.display())))?;
                    }
                    std::fs::write(&file, format!("{text}\n")).map_err(|e| MemError::store(format!("{}: {e}", file.display())))?;
                }
                marker_file = Some(scope::display(&file));
                let found = scope::resolve(&dir, &|_: &str| false).map_err(MemError::invalid)?;
                found.filter(|f| f.kind == RootKind::Marker && f.root == dir).ok_or_else(|| {
                    MemError::store(format!("{} was written but does not resolve", scope::display(&file)))
                })?
            }
            InitMode::Auto => {
                let mut conn = self.store().await?.read().await?;
                let exact = store::path_identities(&mut conn).await?;
                drop(conn);
                let found = match git_dir {
                    Some(git_dir) => Some(scope::from_git_dir(git_dir, &dir).map_err(MemError::invalid_cwd)?),
                    None => scope::resolve(&dir, &|identity: &str| exact.contains(identity)).map_err(MemError::invalid)?,
                };
                found.ok_or_else(|| {
                    MemError::invalid(format!(
                        "{} is not inside a Git repository: use --here (bind this exact directory) or --portable \
                         (write .swiss/memory.json)",
                        scope::display(&dir)
                    ))
                })?
            }
        };
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if let Some(owner) = store::root_owner(&mut conn, &found.identity).await? {
                return Ok((owner, false));
            }
            let (id, default) = match &found.marker {
                Some(marker) => (marker.project_id.clone(), marker.name.clone()),
                None => {
                    if mode == InitMode::Auto {
                        refuse_reclone(&mut conn, &found).await?;
                    }
                    (new_project_id(), None)
                }
            };
            let created = store::project(&mut conn, &id).await?.is_none();
            if created {
                let label = name.clone().or(default).unwrap_or_else(|| scope::default_name(&found.root));
                store::insert_project(&mut conn, &id, &view::clip(&label, PROJECT_NAME_MAX), store::now()).await?;
            }
            claim_root(&mut conn, &id, &found).await?;
            Ok((id, true))
        }
        .await;
        let (id, created) = store::finish(&mut conn, outcome).await?;
        Ok(InitOutcome { project: project_view(&mut conn, &id).await?, identity: found.identity, created, marker_file })
    }

    /// The project a target selects, registering nothing (`None`: not registered yet).
    pub async fn resolve(&self, target: &Target) -> MemResult<Option<Project>> {
        let mut conn = self.store().await?.read().await?;
        Ok(self.locate(&mut conn, target).await?.project)
    }

    /// The checkout a target works in, registering nothing: the working tree its
    /// directory is in (SPEC §memory.scope) - the repository's top from a subdirectory,
    /// a linked worktree itself - or a project's first reachable root. `None`: no
    /// boundary above the directory, or a scope without one.
    pub async fn checkout(&self, target: &Target) -> MemResult<Option<PathBuf>> {
        let mut conn = self.store().await?.read().await?;
        Ok(self.locate(&mut conn, target).await?.root)
    }

    /// [Self::checkout], or the working tree a caller names instead (SPEC §memory.rag):
    /// the absolute path of one of the project's checkouts - a linked worktree or a
    /// directory in one - or a branch some working tree of its repository has checked
    /// out. A path outside the project, or a branch no tree has, is refused, never
    /// swapped for the main checkout.
    pub async fn checkout_for(&self, target: &Target, worktree: Option<&str>) -> MemResult<Option<PathBuf>> {
        let Some(wanted) = worktree.map(str::trim).filter(|w| !w.is_empty()) else {
            return self.checkout(target).await;
        };
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let mut identities: Vec<String> = place.found.iter().map(|f| f.identity.clone()).collect();
        if let Some(project) = &place.project {
            for (owner, identity, _) in store::root_paths(&mut conn).await? {
                if owner == project.id && !identities.contains(&identity) {
                    identities.push(identity);
                }
            }
        }
        // A branch name is never absolute; a WSL path starts at the root.
        if Path::new(wanted).is_absolute() || wanted.starts_with('/') {
            let dir = scope::normalize_dir(wanted).map_err(|why| MemError::invalid(format!("worktree: {why}")))?;
            let exact = store::path_identities(&mut conn).await?;
            let found = scope::resolve(&dir, &|identity: &str| exact.contains(identity)).map_err(MemError::invalid)?;
            return match found {
                Some(found) if identities.contains(&found.identity) => Ok(Some(found.checkout)),
                _ => Err(MemError::invalid(format!("worktree {wanted} is not a checkout of this project"))),
            };
        }
        identities
            .iter()
            .filter_map(|identity| scope::git_common(identity))
            .find_map(|common| scope::branch_checkout(&common, wanted))
            .map(Some)
            .ok_or_else(|| {
                MemError::invalid(format!(
                    "worktree {wanted:?}: no working tree of this project's repository has that branch checked out; \
                     check it out in one (git worktree add) or pass the worktree's absolute path"
                ))
            })
    }

    /// Database path, schema and per-project counts.
    pub async fn status(&self) -> MemResult<StatusView> {
        let mut conn = self.store().await?.read().await?;
        Ok(StatusView {
            db_path: scope::display(&self.path),
            schema_version: store::SCHEMA_VERSION,
            projects: project_views(&mut conn).await?,
        })
    }

    // --- embedding (SPEC §memory.embedding) -----------------------------------------------

    /// Plug in, swap or remove the embedding provider; the next call uses it. Stored
    /// vectors of another fingerprint simply become stale.
    pub fn set_embedder(&self, embedder: Option<Arc<dyn Embedder>>) {
        let changed = {
            let mut slot = self.embedder.write().unwrap_or_else(|p| p.into_inner());
            let before = slot.as_ref().map(|e| e.id().to_string());
            *slot = embedder;
            before != slot.as_ref().map(|e| e.id().to_string())
        };
        if changed {
            let mut state = self.index_state();
            state.last_error = None;
            state.failed.clear();
            state.width = None;
        }
    }

    /// The current provider, if any.
    pub fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        self.embedder.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The nudge fired after a write that changes what a note embeds as (save, edit,
    /// restore, import). A permit is stored when nobody waits, so a nudge is never lost.
    pub fn changes(&self) -> &Notify {
        &self.changed
    }

    pub(crate) fn touched(&self) {
        self.changed.notify_one();
    }

    fn index_state(&self) -> std::sync::MutexGuard<'_, IndexState> {
        self.index.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The provider answered `width` wide under `model`: the index's width from now on.
    fn saw_width(&self, model: &str, width: usize) {
        self.index_state().width = Some((model.to_string(), width));
    }

    /// The index's one width under `model` (SPEC §memory.embedding): what the provider
    /// answered last, else the width of the newest stored vector; None before either.
    async fn index_width(&self, conn: &mut SqliteConnection, model: &str) -> MemResult<Option<usize>> {
        let seen = self.index_state().width.clone().filter(|(m, _)| m == model).map(|(_, width)| width);
        match seen {
            Some(width) => Ok(Some(width)),
            None => Ok(store::vector_widths(conn, model).await?.first().map(|(dims, _)| *dims)),
        }
    }

    /// The cosines of `text` to `vectors`: the query embedded within the provider's
    /// timeout, its width recorded, then [MemoryService::scores]. Err is the reason a
    /// lexical answer gives.
    async fn meaning(
        &self,
        embedder: &dyn Embedder,
        text: &str,
        vectors: &HashMap<String, Vec<f32>>,
    ) -> Result<HashMap<String, f32>, String> {
        let query = query_vector(embedder, text).await?;
        self.saw_width(embedder.id(), query.len());
        self.scores(&query, vectors)
    }

    /// The cosine of `query` to each vector of its width. One fingerprint holds one
    /// width: a vector of another is left out - never scored 0, which would rank it as
    /// unrelated without a word - and the indexer is nudged to embed it again. Err,
    /// the reason the answer gives, when no vector is of the query's width.
    fn scores(&self, query: &[f32], vectors: &HashMap<String, Vec<f32>>) -> Result<HashMap<String, f32>, String> {
        let mut other = None;
        let sims: HashMap<String, f32> = vectors
            .iter()
            .filter_map(|(id, vector)| match embed::cosine(query, vector) {
                Some(sim) => Some((id.clone(), sim)),
                None => {
                    other = Some(vector.len());
                    None
                }
            })
            .collect();
        let Some(width) = other else {
            return Ok(sims);
        };
        self.touched();
        if sims.is_empty() {
            return Err(format!(
                "the index holds {width}-dimension vectors and the provider now answers {}: re-indexing",
                query.len()
            ));
        }
        Ok(sims)
    }

    /// Embed up to `batch` live notes (every project) that lack a current vector,
    /// newest first, notes that failed before last. One run at a time; a provider
    /// error ends the run and lands in the report (and the status), never as an Err.
    pub async fn index_pending(&self, batch: usize) -> MemResult<IndexReport> {
        let Some(embedder) = self.embedder() else {
            return Err(MemError::invalid("embedding is off: configure an embedding provider first"));
        };
        let _run = self.index_lock.lock().await;
        self.index_state().running = true;
        let outcome = self.index_run(embedder.as_ref(), batch.clamp(1, embed::INDEX_BATCH_MAX)).await;
        let mut state = self.index_state();
        state.running = false;
        state.last_run_ms = Some(store::now());
        state.last_error = match &outcome {
            Ok(report) => report.error.clone(),
            Err(err) => Some(err.message.clone()),
        };
        outcome
    }

    async fn index_run(&self, embedder: &dyn Embedder, batch: usize) -> MemResult<IndexReport> {
        let model = embedder.id().to_string();
        let mut report = IndexReport::default();
        // One fingerprint holds one width: the vectors of another than the index's go
        // first (their notes are pending already: a vector of another width is none).
        let (mut todo, mixed) = {
            let mut conn = self.store().await?.read().await?;
            let width = self.index_width(&mut conn, &model).await?;
            let widths = store::vector_widths(&mut conn, &model).await?;
            let mixed = width.filter(|w| widths.iter().any(|(dims, _)| dims != w));
            (pending_texts(&mut conn, &model, width).await?, mixed)
        };
        if let Some(width) = mixed {
            let store = self.store().await?;
            let mut conn = store.write().await?;
            let outcome = store::drop_other_widths(&mut conn, &model, width).await;
            report.dropped += store::finish(&mut conn, outcome).await? as usize;
        }
        {
            // Failed notes last (stable: newest first within each group).
            let state = self.index_state();
            todo.sort_by_key(|(id, title, content)| {
                state.failed.get(id).map(|h| *h == embed::text_hash(title, content)).unwrap_or(false)
            });
        }
        // One run, one width: the first vector sets it, and a later one of another is
        // unusable (a provider changing models mid-run).
        let mut run_width: Option<usize> = None;
        let take: Vec<&(String, String, String)> = todo.iter().take(batch).collect();
        for group in take.chunks(embedder.batch().clamp(1, embed::INDEX_BATCH_MAX)) {
            let texts: Vec<String> = group.iter().map(|(_, title, content)| embed::text_of(title, content)).collect();
            let vectors = match embedder.embed(&texts, Purpose::Document).await {
                Ok(vectors) if vectors.len() == texts.len() => Some(vectors),
                Ok(vectors) => {
                    report.error = Some(format!("the provider returned {} vectors for {} texts", vectors.len(), texts.len()));
                    None
                }
                Err(err) => {
                    report.error = Some(err.message);
                    None
                }
            };
            let Some(vectors) = vectors else {
                report.failed += group.len();
                let mut state = self.index_state();
                for (id, title, content) in group {
                    state.failed.insert(id.clone(), embed::text_hash(title, content));
                }
                break;
            };
            let mut rows: Vec<(&str, String, Vec<f32>)> = Vec::new();
            for ((id, title, content), mut vector) in group.iter().zip(vectors) {
                let width = vector.len();
                if width > embed::DIMS_MAX || run_width.is_some_and(|d| d != width) || !embed::normalize(&mut vector) {
                    report.failed += 1;
                    report.error.get_or_insert_with(|| format!("the provider returned an unusable vector ({width} dims)"));
                    self.index_state().failed.insert(id.clone(), embed::text_hash(title, content));
                    continue;
                }
                run_width = Some(width);
                rows.push((id.as_str(), embed::text_hash(title, content), vector));
            }
            if let Some(width) = run_width {
                self.saw_width(&model, width);
            }
            let store = self.store().await?;
            let mut conn = store.write().await?;
            let outcome = async {
                // The provider's width is the index's: older ones go in the same write.
                let dropped = match run_width {
                    Some(width) => store::drop_other_widths(&mut conn, &model, width).await?,
                    None => 0,
                };
                let now = store::now();
                let mut stored = 0;
                for (id, hash, vector) in &rows {
                    if store::put_vector(&mut conn, id, &model, hash, vector, now).await? {
                        stored += 1;
                    }
                }
                Ok((stored, dropped))
            }
            .await;
            let (stored, dropped) = store::finish(&mut conn, outcome).await?;
            report.embedded += stored;
            report.dropped += dropped as usize;
            let mut state = self.index_state();
            for (id, _, _) in &rows {
                state.failed.remove(*id);
            }
        }
        // Counted again: a dropped width made notes pending that the run began without.
        let mut conn = self.store().await?.read().await?;
        let width = self.index_width(&mut conn, &model).await?;
        report.pending = pending_texts(&mut conn, &model, width).await?.len();
        Ok(report)
    }

    /// The index at a glance: counts over live notes for the current provider.
    pub async fn embedding_status(&self) -> MemResult<EmbeddingStatus> {
        let state = self.index_state().clone();
        let Some(embedder) = self.embedder() else {
            return Ok(EmbeddingStatus {
                state: "off",
                model: None,
                dims: None,
                indexed: 0,
                pending: 0,
                incompatible: 0,
                last_error: None,
                last_run_ms: state.last_run_ms,
            });
        };
        let model = embedder.id().to_string();
        let mut conn = self.store().await?.read().await?;
        let dims = self.index_width(&mut conn, &model).await?;
        let incompatible = store::vector_widths(&mut conn, &model)
            .await?
            .iter()
            .filter(|(width, _)| Some(*width) != dims)
            .map(|(_, count)| count)
            .sum();
        // The eligible live notes - a project whose settings turn embedding off has none,
        // so its kept vectors (SPEC §memory.embedding) never count as indexed.
        let skip = crate::settings::embedding_off_projects(&mut conn).await?;
        let live = store::live_texts(&mut conn, &skip).await?.len();
        let pending = pending_texts(&mut conn, &model, dims).await?.len();
        Ok(EmbeddingStatus {
            state: if state.running {
                "indexing"
            } else if state.last_error.is_some() {
                "error"
            } else if incompatible > 0 {
                "stale"
            } else {
                "ready"
            },
            model: Some(model),
            dims,
            indexed: live.saturating_sub(pending),
            pending,
            incompatible,
            last_error: state.last_error,
            last_run_ms: state.last_run_ms,
        })
    }

    /// Drop the current provider's vectors (a re-index): the count dropped.
    pub async fn drop_vectors(&self) -> MemResult<u64> {
        let Some(embedder) = self.embedder() else {
            return Err(MemError::invalid("embedding is off: configure an embedding provider first"));
        };
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = store::drop_vectors(&mut conn, Some(embedder.id())).await;
        let dropped = store::finish(&mut conn, outcome).await?;
        self.index_state().failed.clear();
        self.touched();
        Ok(dropped)
    }
}

// --- helpers --------------------------------------------------------------------------------

fn no_note(id: &str) -> MemError {
    MemError::not_found(format!("no note {id} in this project"))
}

fn first_root(project: &Project) -> Option<PathBuf> {
    project.roots.iter().map(|r| PathBuf::from(&r.path)).find(|p| p.is_dir())
}

fn root_row(found: &Found) -> Root {
    Root {
        identity: found.identity.clone(),
        kind: found.kind,
        path: found.root.display().to_string(),
        origin_url: found.origin_url.clone(),
        added_ms: store::now(),
    }
}

/// A record of the scope, or `memory_not_found` - never a hint that it lives elsewhere.
pub(crate) async fn in_scope(conn: &mut SqliteConnection, project_id: &str, id: &str) -> MemResult<Memory> {
    match store::memory(conn, id).await? {
        Some(memory) if memory.scope == project_id => Ok(memory),
        _ => Err(no_note(id)),
    }
}

/// Every note of a scope (history included) and its link index.
pub(crate) async fn load_scope(conn: &mut SqliteConnection, project_id: &str) -> MemResult<(Vec<Memory>, Net)> {
    let all = store::memories(conn, project_id, &Filter::default()).await?;
    let links = store::links_in(conn, project_id).await?;
    let net = Net::from_memories(&all, links);
    Ok((all, net))
}

/// Node infos of a whole scope without its anchors (the search path's markers).
async fn all_statuses(conn: &mut SqliteConnection, project_id: &str) -> MemResult<HashMap<String, NodeInfo>> {
    let all = store::memories(conn, project_id, &Filter::default()).await?;
    Ok(all.iter().map(|m| (m.id.clone(), NodeInfo::of(m))).collect())
}

/// One record's view with its 1-hop neighbourhood and fresh anchor states.
pub(crate) async fn view_one(conn: &mut SqliteConnection, memory: &Memory, root: Option<PathBuf>) -> MemResult<RecordView> {
    let links = store::links_touching(conn, &memory.id).await?;
    let mut nodes: HashMap<String, NodeInfo> = HashMap::new();
    nodes.insert(memory.id.clone(), NodeInfo::of(memory));
    for link in &links {
        let Some(other) = link.other(&memory.id) else { continue };
        if nodes.contains_key(other) {
            continue;
        }
        if let Some(neighbour) = store::memory(conn, other).await? {
            nodes.insert(other.to_string(), NodeInfo::of(&neighbour));
        }
    }
    let net = Net::new(nodes, links);
    let now = store::now();
    let mut checker = Checker::new(root);
    let (marks, states) = graph::markers(memory, &net, &mut checker, now);
    let evidence = store::evidence(conn, &memory.id).await?;
    Ok(view::record(memory, &marks, &states, &net, now, &evidence))
}

/// Bind checked evidence to the note's text as it is now (ADR-032).
async fn attach(
    conn: &mut SqliteConnection,
    memory: &Memory,
    proofs: &[EvidenceSpec],
    default: Relation,
    root: Option<&Path>,
    actor: &Actor,
    now: i64,
) -> MemResult<()> {
    if proofs.is_empty() {
        return Ok(());
    }
    let held = store::evidence_count(conn, &memory.id).await?;
    if held + proofs.len() > EVIDENCE_PER_RECORD_MAX {
        return Err(MemError::invalid(format!(
            "evidence: {} holds {held} pieces; a note carries at most {EVIDENCE_PER_RECORD_MAX}",
            memory.id
        )));
    }
    for proof in proofs {
        store::insert_evidence(conn, &proof.bind(memory, default, root, &actor.label, now)).await?;
    }
    Ok(())
}

/// Up to three live notes whose text resembles a fresh save (hints only). A title with
/// CJK goes through the search's CJK tier - [search::coverage] over the live notes,
/// the saved note among them so its rare units set the scale, and a floor a stray
/// shared unit never clears: FTS cannot segment CJK, so there the title's only matches
/// are its Latin tokens, and `AI 客服预设问题` would hint every note saying `ai`. A
/// title without CJK keeps the word tier (FTS bm25 over the title's salient terms),
/// like the brief's task hits.
async fn similar_hints(conn: &mut SqliteConnection, project_id: &str, memory: &Memory) -> MemResult<Vec<Summary>> {
    // The CJK tier reads the whole scope; the word tier loads it only once FTS found a hint.
    let mut loaded = None;
    let ids: Vec<String> = if search::has_cjk(&memory.title) {
        let scope = load_scope(conn, project_id).await?;
        let units = search::units(&memory.title);
        let mut covered: Vec<search::Covered> =
            search::coverage(scope.0.iter().filter(|m| m.status.is_live()), &units)
                .into_iter()
                .filter(|c| c.id != memory.id && c.coverage >= SIMILAR_COVERAGE_MIN)
                .collect();
        covered.sort_by(|a, b| b.score(units.len()).cmp(&a.score(units.len())).then_with(|| a.id.cmp(&b.id)));
        loaded = Some(scope);
        covered.into_iter().take(SIMILAR_HINTS).map(|c| c.id).collect()
    } else {
        let terms = related::salient(&memory.title);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        store::fts(conn, project_id, &search::or_expr(&terms), &search::statuses(false), 10)
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| id != &memory.id && Some(id) != memory.superseded_by.as_ref())
            .take(SIMILAR_HINTS)
            .collect()
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let (all, net) = match loaded {
        Some(scope) => scope,
        None => load_scope(conn, project_id).await?,
    };
    let now = store::now();
    let mut checker = Checker::disabled();
    Ok(ids
        .iter()
        .filter_map(|id| all.iter().find(|m| &m.id == id))
        .filter(|m| m.id != memory.id && m.superseded_by.as_deref() != Some(memory.id.as_str()))
        .map(|m| {
            let (marks, _) = graph::markers(m, &net, &mut checker, now);
            view::summary(m, &marks, &net, now, true)
        })
        .collect())
}

/// Up to three live notes sharing an anchor with a fresh save whose title asserts the
/// opposite of its title (crate::opposed), oldest first; a note the save already links
/// as contradicting is no news.
async fn opposed_hints(conn: &mut SqliteConnection, project_id: &str, memory: &Memory, links: &[LinkSpec]) -> MemResult<Vec<Opposed>> {
    if memory.anchors.is_empty() {
        return Ok(Vec::new());
    }
    let mut live = store::memories(conn, project_id, &Filter::default()).await?;
    live.retain(|m| m.status.is_live() && m.id != memory.id);
    live.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));
    Ok(live
        .iter()
        .filter(|m| !links.iter().any(|l| l.to == m.id && l.kind == LinkKind::Contradicts))
        .filter_map(|m| {
            let shared = memory.anchors.iter().find(|a| m.anchors.iter().any(|b| glob::anchors_overlap(a, b)))?;
            opposed::opposed(&memory.title, &m.title).then(|| Opposed::new(&m.id, &m.title, &shared.path))
        })
        .take(SIMILAR_HINTS)
        .collect())
}

/// The brief's task section, lexically: (exact, words). Exact: the symbol tier's hits
/// (search::symbols - the notes holding an identifier, path or domain the task names),
/// best first, a tie to the better word rank. Words: the search's own word tier - FTS OR
/// over the task's salient terms (bm25) for a task without CJK, the CJK tier's
/// IDF-weighted coverage (search::coverage) for one with it, under the search's word floor
/// (search::WORD_SHARE_MIN) that keeps one stray common word or bigram out of the brief -
/// never repeating an exact hit. At most [TASK_HITS] each. With them, each word hit's share of
/// the task's word weight, for the retrieval readout (crate::readout).
async fn task_hits(
    conn: &mut SqliteConnection,
    project_id: &str,
    task: &str,
    memories: &[Memory],
) -> MemResult<(Vec<String>, Vec<String>, HashMap<String, f64>)> {
    let live = || memories.iter().filter(|m| m.status.is_live());
    let mut held: HashMap<String, f64> = HashMap::new();
    let words: Vec<String> = if search::has_cjk(task) {
        let units = search::units(task);
        let mut covered: Vec<search::Covered> =
            search::coverage(live(), &units).into_iter().filter(|c| c.coverage >= search::WORD_SHARE_MIN).collect();
        covered.sort_by(|a, b| b.score(units.len()).cmp(&a.score(units.len())).then_with(|| a.id.cmp(&b.id)));
        covered.truncate(TASK_HITS);
        held.extend(covered.iter().map(|c| (c.id.clone(), c.coverage)));
        covered.into_iter().map(|c| c.id).collect()
    } else {
        let mut terms = related::salient(task);
        if terms.is_empty() {
            terms = search::terms(task);
        }
        if terms.is_empty() {
            Vec::new()
        } else {
            let shares = search::word_shares(live(), task);
            let words: Vec<String> =
                store::fts(conn, project_id, &search::or_expr(&terms), &search::statuses(false), search::LEXICAL_CAP)
                    .await?
                    .into_iter()
                    .filter(|(id, _)| shares.as_ref().is_none_or(|s| s.get(id).is_some_and(|v| *v >= search::WORD_SHARE_MIN)))
                    .take(TASK_HITS)
                    .map(|(id, _)| id)
                    .collect();
            if let Some(shares) = &shares {
                held.extend(words.iter().filter_map(|id| shares.get(id).map(|s| (id.clone(), *s))));
            }
            words
        }
    };
    let place = |id: &str| words.iter().position(|w| w == id).unwrap_or(usize::MAX);
    let mut symbols = search::symbols(live(), task);
    symbols.sort_by(|a, b| b.score().cmp(&a.score()).then_with(|| place(&a.id).cmp(&place(&b.id))).then_with(|| a.id.cmp(&b.id)));
    let exact: Vec<String> = symbols.into_iter().take(TASK_HITS).map(|s| s.id).collect();
    let words = words.into_iter().filter(|id| !exact.contains(id)).collect();
    Ok((exact, words, held))
}

/// Three decimals: a similarity is a reading, not a measurement.
fn round3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

/// Live notes lacking a current vector under `model`: (id, title, content), newest
/// first. Current means this fingerprint, this text hash and the index's `width`. A
/// project whose settings turn embedding off (§memory.embedding) has no embedding input.
async fn pending_texts(
    conn: &mut SqliteConnection,
    model: &str,
    width: Option<usize>,
) -> MemResult<Vec<(String, String, String)>> {
    let keys = store::vector_keys(conn).await?;
    let skip = crate::settings::embedding_off_projects(conn).await?;
    Ok(store::live_texts(conn, &skip)
        .await?
        .into_iter()
        .filter(|(id, title, content)| match keys.get(id) {
            Some(key) => {
                key.model != model
                    || key.text_hash != embed::text_hash(title, content)
                    || width.is_some_and(|w| w != key.dims)
            }
            None => true,
        })
        .collect())
}

/// The current vectors of `rows` (of one scope): stale ones (another model, an
/// edited text) are left out; one of another width is left out where it is scored.
async fn current_vectors<'a>(
    conn: &mut SqliteConnection,
    project_id: &str,
    model: &str,
    rows: impl Iterator<Item = &'a Memory>,
) -> MemResult<HashMap<String, Vec<f32>>> {
    let mut stored = store::scope_vectors(conn, project_id, model).await?;
    let mut out = HashMap::new();
    for memory in rows {
        if let Some((hash, vector)) = stored.remove(&memory.id) {
            if hash == embed::text_hash(&memory.title, &memory.content) {
                out.insert(memory.id.clone(), vector);
            }
        }
    }
    Ok(out)
}

/// A query's vector within the provider's query timeout; Err carries the reason the
/// search reports (never a credential: the provider's message is already scrubbed).
async fn query_vector(embedder: &dyn Embedder, text: &str) -> Result<Vec<f32>, String> {
    let wait = embedder.query_timeout();
    let texts = vec![text.to_string()];
    match tokio::time::timeout(wait, embedder.embed(&texts, Purpose::Query)).await {
        Err(_) => Err(format!("the embedding provider did not answer within {} ms", wait.as_millis())),
        Ok(Err(err)) => Err(err.message),
        Ok(Ok(mut vectors)) => {
            let mut vector = match (vectors.len(), vectors.pop()) {
                (1, Some(vector)) => vector,
                (n, _) => return Err(format!("the provider returned {n} vectors for one query")),
            };
            if vector.len() > embed::DIMS_MAX || !embed::normalize(&mut vector) {
                return Err("the provider returned an unusable query vector".into());
            }
            Ok(vector)
        }
    }
}

/// Reciprocal-rank fusion of a ranked id list with the cosines (the brief's task
/// section): the search's rule ([search::rrf_sums]) without scores; `skip` (the exact
/// hits) takes no place. Top `max`; a tie goes to the better word rank, then the id.
fn fuse_ids(lexical: &[String], sims: &HashMap<String, f32>, skip: &[String], floor: f32, max: usize) -> Vec<String> {
    let mut ranked: Vec<(String, f64)> =
        search::rrf_sums(lexical, sims, skip, floor).into_iter().map(|(id, (sum, _))| (id, sum)).collect();
    let place = |id: &String| lexical.iter().position(|x| x == id).unwrap_or(usize::MAX);
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| place(&a.0).cmp(&place(&b.0))).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(max).map(|(id, _)| id).collect()
}

/// The head of a related text seed that gets embedded.
fn related_head(text: &str) -> &str {
    let max = related::SEED_TEXT_MAX;
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The project an unregistered checkout probably belongs to (same origin URL).
async fn possible_match(conn: &mut SqliteConnection, found: &Found) -> MemResult<Option<Project>> {
    let Some(origin) = &found.origin_url else {
        return Ok(None);
    };
    for (project_id, identity) in store::roots_by_origin(conn, origin).await? {
        if identity != found.identity {
            return store::project(conn, &project_id).await;
        }
    }
    Ok(None)
}

/// Refuse to auto-register a re-clone of a known project (SPEC §memory.scope).
async fn refuse_reclone(conn: &mut SqliteConnection, found: &Found) -> MemResult<()> {
    if let Some(project) = possible_match(conn, found).await? {
        return Err(MemError::new(
            Code::ProjectAmbiguous,
            format!(
                "{} is not registered, but project {} ({}) has the same origin. Bind this checkout to it with \
                 `swiss memory project bind --id {}` (or \"add directory\" in the panel), or start a new project with \
                 `swiss memory project bind --new-project`",
                scope::display(&found.root),
                project.name,
                project.id,
                project.id
            ),
        )
        .with("possibleMatch", json!({"projectId": project.id, "name": project.name})));
    }
    Ok(())
}

async fn reclone_hint(conn: &mut SqliteConnection, found: &Found) -> MemResult<Option<String>> {
    Ok(possible_match(conn, found).await?.map(|project| {
        format!(
            "This looks like a clone of project {} ({}): bind it with `swiss memory project bind --id {}` to share its notes.",
            project.name, project.id, project.id
        )
    }))
}

/// Bind `dir` to `project_id` inside the caller's transaction.
async fn bind_in(conn: &mut SqliteConnection, project_id: &str, dir: &Path, exact: bool, git_dir: Option<&Path>) -> MemResult<()> {
    let found = if exact {
        scope::exact(dir).map_err(MemError::invalid_cwd)?
    } else if let Some(git_dir) = git_dir {
        scope::from_git_dir(git_dir, dir).map_err(MemError::invalid_cwd)?
    } else {
        let exacts = store::path_identities(conn).await?;
        scope::resolve(dir, &|identity: &str| exacts.contains(identity))
            .map_err(MemError::invalid)?
            .ok_or_else(|| {
                MemError::invalid(format!(
                    "{} is not inside a Git repository or a .swiss/memory.json binding; bind the exact directory instead",
                    scope::display(dir)
                ))
            })?
    };
    if let Some(marker) = &found.marker {
        if marker.project_id != project_id {
            return Err(MemError::invalid(format!(
                "{} carries .swiss/memory.json naming {}; edit or remove that file to bind it elsewhere",
                scope::display(&found.root),
                marker.project_id
            )));
        }
    }
    match store::root_owner(conn, &found.identity).await? {
        Some(owner) if owner == project_id => Ok(()),
        Some(owner) => {
            let name = store::project(conn, &owner).await?.map(|p| p.name).unwrap_or_default();
            Err(MemError::new(
                Code::RootBound,
                format!(
                    "{} already belongs to project {name} ({owner}); unbind it there first",
                    scope::display(&found.root)
                ),
            )
            .with("projectId", json!(owner)))
        }
        None => claim_root(conn, project_id, &found).await,
    }
}

/// Whether two canonical directories are one (a Windows path ignores case).
fn same_dir(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// A bound Git identity whose directory moved on: the row follows the root resolved now
/// when the directory it names is gone (a bare repository's worktree, removed) or is a
/// working tree of the same repository whose root is the one resolved now (a project
/// first written from a linked worktree before roots followed the main checkout). Any
/// other difference - another worktree of a bare repository - is left alone, and a
/// directory another project holds is never taken.
async fn heal_root(conn: &mut SqliteConnection, found: &Found) -> MemResult<()> {
    let row = root_row(found);
    let rows = store::root_paths(conn).await?;
    let Some((owner, _, stored)) = rows.iter().find(|(_, identity, _)| identity == &found.identity) else {
        return Ok(());
    };
    if same_dir(stored, &row.path) || rows.iter().any(|(other, _, path)| other != owner && same_dir(path, &row.path)) {
        return Ok(());
    }
    let stored_dir = Path::new(stored);
    let moved = !stored_dir.is_dir()
        || stored_dir
            .canonicalize()
            .ok()
            .and_then(|dir| scope::resolve(&dir, &|_: &str| false).ok().flatten())
            .is_some_and(|there| there.identity == found.identity && there.root == found.root);
    if moved {
        store::move_root(conn, &found.identity, &row.path).await?;
    }
    Ok(())
}

/// Bind a resolved boundary to a project. A directory belongs to one project whatever
/// its identity's kind (SPEC §memory.scope): an exact binding over another project's
/// repository would silently take its notes' directory away.
async fn claim_root(conn: &mut SqliteConnection, project_id: &str, found: &Found) -> MemResult<()> {
    let row = root_row(found);
    for (owner, _, path) in store::root_paths(conn).await? {
        if owner != project_id && same_dir(&path, &row.path) {
            let name = store::project(conn, &owner).await?.map(|p| p.name).unwrap_or_default();
            return Err(MemError::new(
                Code::RootBound,
                format!("{} already belongs to project {name} ({owner}); unbind it there first", scope::display(&found.root)),
            )
            .with("projectId", json!(owner)));
        }
    }
    store::insert_root(conn, project_id, &row).await
}

async fn project_views(conn: &mut SqliteConnection) -> MemResult<Vec<ProjectView>> {
    let projects = store::projects(conn).await?;
    let counts = store::counts(conn).await?;
    let mut out: Vec<ProjectView> = projects.iter().map(|p| ProjectView::of(p, counts.get(&p.id))).collect();
    out.sort_by(|a, b| b.global.cmp(&a.global).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

async fn project_view(conn: &mut SqliteConnection, id: &str) -> MemResult<ProjectView> {
    let project = store::project(conn, id).await?.ok_or_else(|| MemError::not_found(format!("no project {id}")))?;
    let counts = store::counts(conn).await?;
    Ok(ProjectView::of(&project, counts.get(id)))
}

/// A link as stored: symmetric kinds normalized `from < to`.
pub(crate) fn normalized(from: &str, to: &str, kind: LinkKind, note: Option<String>, actor: &str, now: i64) -> Link {
    let (from, to) = if kind.is_symmetric() && to < from { (to, from) } else { (from, to) };
    Link { from: from.to_string(), to: to.to_string(), kind, note, actor: actor.to_string(), created_ms: now }
}

pub(crate) async fn ensure_capacity(conn: &mut SqliteConnection, id: &str) -> MemResult<()> {
    if store::link_count(conn, id).await? as usize >= LINKS_PER_RECORD_MAX {
        return Err(MemError::invalid(format!(
            "{id} already has {LINKS_PER_RECORD_MAX} links; remove one before adding another"
        )));
    }
    Ok(())
}

/// The reason an edit that releases a note the check held writes.
const RELEASED: &str = "released: the code check passes";
/// The check's rules about a note's evidence, which only an edit to its text leaves behind.
const EVIDENCE_RULES: [&str; 2] = ["excerpt_mismatch", "missing_evidence"];

/// Why the code check put a needs_review note where it is, if it did: its latest revision
/// says it held it (a save or an edit), not a person's or an agent's outdated mark.
async fn held_by_check(conn: &mut SqliteConnection, memory: &Memory) -> MemResult<Option<String>> {
    let revisions = store::revisions(conn, &memory.id).await?;
    Ok(revisions
        .into_iter()
        .rev()
        .find(|r| r.revision == memory.revision)
        .filter(|r| r.op != "mark_outdated")
        .and_then(|r| r.reason)
        .filter(|why| why.contains(crate::code::check::HELD)))
}

/// A caller's reason and what the check said, in one revision reason.
fn with_reason(reason: Option<String>, said: &str) -> String {
    match reason {
        Some(reason) => view::clip(&format!("{reason}; {said}"), REASON_MAX),
        None => view::clip(said, REASON_MAX),
    }
}

pub(crate) fn revision_of(memory: &Memory, op: &str, actor: &Actor, reason: Option<String>, now: i64) -> Revision {
    Revision {
        revision: memory.revision,
        op: op.to_string(),
        actor: actor.label.clone(),
        reason,
        at_ms: now,
        snapshot: Snapshot::of(memory),
    }
}

// --- validation -----------------------------------------------------------------------------

pub(crate) fn check_record_id(field: &str, id: &str) -> MemResult<()> {
    if is_record_id(id) {
        Ok(())
    } else {
        Err(MemError::invalid(format!("{field}: {id:?} is not a note id (m- followed by 10 hex digits)")))
    }
}

fn check_link(from: &str, to: &str, kind: LinkKind) -> MemResult<()> {
    check_record_id("from", from)?;
    check_record_id("to", to)?;
    if kind.is_system() {
        return Err(MemError::invalid("kind: supersedes is written by save … replaces, never linked directly"));
    }
    if from == to {
        return Err(MemError::invalid("a note cannot link to itself"));
    }
    Ok(())
}

pub(crate) fn check_secret(field: &str, text: &str) -> MemResult<()> {
    match secrets::scan(text) {
        Some(rule) => Err(MemError::new(
            Code::SensitiveContent,
            format!("{field} looks like it carries a credential ({rule}); remove it and try again"),
        )
        .with("rule", json!(rule))),
        None => Ok(()),
    }
}

pub(crate) fn clean_title(raw: &str) -> MemResult<String> {
    let title = raw.trim();
    if title.is_empty() {
        return Err(MemError::invalid("title is required"));
    }
    if title.contains(['\n', '\r']) {
        return Err(MemError::invalid("title must be a single line"));
    }
    let n = title.chars().count();
    if n > TITLE_MAX {
        return Err(MemError::invalid(format!("title is {n} characters; the limit is {TITLE_MAX}")));
    }
    Ok(title.to_string())
}

pub(crate) fn clean_content(raw: &str) -> MemResult<String> {
    let content = raw.trim();
    if content.is_empty() {
        return Err(MemError::invalid("content is required"));
    }
    if content.len() > CONTENT_MAX {
        return Err(MemError::invalid(format!(
            "content is {} bytes; the limit is {CONTENT_MAX} (keep notes short, link related ones)",
            content.len()
        )));
    }
    Ok(content.replace("\r\n", "\n"))
}

pub(crate) fn clean_anchor_path(raw: &str) -> MemResult<String> {
    let path = crate::glob::normalize(raw);
    if path.is_empty() {
        return Err(MemError::invalid("anchors: an anchor is empty"));
    }
    if path.chars().count() > ANCHOR_PATH_MAX {
        return Err(MemError::invalid(format!("anchors: a path is longer than {ANCHOR_PATH_MAX} characters")));
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    if path.starts_with('/') || drive {
        return Err(MemError::invalid(format!("anchors: {path} must be relative to the project root")));
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(MemError::invalid(format!("anchors: {path} must not leave the project root")));
    }
    if path.contains(['\n', '\r']) {
        return Err(MemError::invalid("anchors: a path must be a single line"));
    }
    Ok(path)
}

pub(crate) fn clean_anchors(raw: &[String]) -> MemResult<Vec<Anchor>> {
    if raw.len() > ANCHORS_MAX {
        return Err(MemError::invalid(format!("anchors: at most {ANCHORS_MAX}")));
    }
    let mut out: Vec<Anchor> = Vec::with_capacity(raw.len());
    for entry in raw {
        let path = clean_anchor_path(entry)?;
        if out.iter().any(|a| a.path == path) {
            continue;
        }
        let glob = path.contains(['*', '?']);
        out.push(Anchor { path, glob, hash: None });
    }
    Ok(out)
}

pub(crate) fn clean_refs(raw: &[String]) -> MemResult<Vec<String>> {
    if raw.len() > REFS_MAX {
        return Err(MemError::invalid(format!("refs: at most {REFS_MAX}")));
    }
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for entry in raw {
        let reference = entry.trim();
        if reference.is_empty() {
            return Err(MemError::invalid("refs: a ref is empty"));
        }
        if reference.contains(['\n', '\r']) {
            return Err(MemError::invalid("refs: a ref must be a single line"));
        }
        if reference.chars().count() > REF_MAX {
            return Err(MemError::invalid(format!("refs: a ref is longer than {REF_MAX} characters")));
        }
        check_secret("refs", reference)?;
        if !out.iter().any(|r| r == reference) {
            out.push(reference.to_string());
        }
    }
    Ok(out)
}

pub(crate) fn clean_note(raw: Option<&str>) -> MemResult<Option<String>> {
    let Some(note) = raw.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.contains(['\n', '\r']) {
        return Err(MemError::invalid("note must be a single line"));
    }
    if note.chars().count() > LINK_NOTE_MAX {
        return Err(MemError::invalid(format!("note is longer than {LINK_NOTE_MAX} characters")));
    }
    check_secret("note", note)?;
    Ok(Some(note.to_string()))
}

pub(crate) fn clean_reason(raw: Option<&str>, required: bool) -> MemResult<Option<String>> {
    let reason = raw.map(str::trim).filter(|r| !r.is_empty());
    match reason {
        None if required => Err(MemError::invalid("reason is required")),
        None => Ok(None),
        Some(reason) => {
            if reason.chars().count() > REASON_MAX {
                return Err(MemError::invalid(format!("reason is longer than {REASON_MAX} characters")));
            }
            check_secret("reason", reason)?;
            Ok(Some(reason.to_string()))
        }
    }
}

fn clean_name(raw: &str) -> MemResult<String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(MemError::invalid("name is required"));
    }
    if name.chars().count() > PROJECT_NAME_MAX {
        return Err(MemError::invalid(format!("name is longer than {PROJECT_NAME_MAX} characters")));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_memory_releases_both_pools_and_preserves_data() {
        let dir = std::env::temp_dir().join(format!("swiss-memory-close-{}", swiss_core::util::random_hex(8)));
        let path = dir.join("store.sqlite3");
        let service = MemoryService::at(&path);
        service.create_project(Some("Close fixture"), None, false, None).await.expect("create project");
        let store = service.store().await.expect("open store").clone();
        let code = service.code.db().await.expect("open code index").pool.clone();
        let independent = MemoryService::at(&path);
        independent.projects().await.expect("a CLI can share the same home");
        service.close().await;
        assert!(store.read().await.is_err(), "even a retained store must be closed");
        assert!(code.is_closed(), "the derived index must close too");
        assert!(service.store().await.is_err(), "a stale service cannot reopen");
        assert!(service.code.db().await.is_err(), "a stale index cannot reopen");
        assert!(independent.projects().await.expect("the other service stays live").iter().any(|p| p.name == "Close fixture"));
        independent.close().await;
        service.close().await;
        let next = MemoryService::at(&path);
        assert!(next.projects().await.expect("read persisted projects").iter().any(|p| p.name == "Close fixture"));
        next.close().await;
        std::fs::remove_dir_all(&dir).expect("release every database handle");
    }

    #[tokio::test]
    async fn closing_unused_memory_never_opens_either_database() {
        let dir = std::env::temp_dir().join(format!("swiss-memory-unused-{}", swiss_core::util::random_hex(8)));
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        service.close().await;
        assert!(service.store().await.is_err());
        assert!(service.code.db().await.is_err());
        assert!(!dir.exists(), "closing an unused service must perform no filesystem writes");
    }

    #[tokio::test]
    async fn closing_memory_waits_for_a_concurrent_first_open() {
        let dir = std::env::temp_dir().join(format!("swiss-memory-open-close-{}", swiss_core::util::random_hex(8)));
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        let (store, code, ()) = tokio::join!(service.store(), service.code.db(), service.close());
        if let Ok(store) = store {
            assert!(store.read().await.is_err(), "a first open cannot outlive close");
        }
        if let Ok(code) = code {
            assert!(code.pool.is_closed(), "an index first open cannot outlive close");
        }
        assert!(service.store().await.is_err());
        assert!(service.code.db().await.is_err());
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("no open handles after close");
        }
    }

    #[test]
    fn anchors_must_stay_inside_the_root() {
        assert!(clean_anchors(&["/etc/passwd".into()]).is_err());
        assert!(clean_anchors(&["C:\\x\\y".into()]).is_err());
        assert!(clean_anchors(&["../outside".into()]).is_err());
        let ok = clean_anchors(&[".\\src\\app.rs".into(), "src/app.rs".into(), "docs/**/*.md".into()]).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].path, "src/app.rs");
        assert!(ok[1].glob);
    }

    #[test]
    fn titles_are_one_bounded_line() {
        assert!(clean_title("  ").is_err());
        assert!(clean_title("a\nb").is_err());
        assert!(clean_title(&"x".repeat(TITLE_MAX + 1)).is_err());
        assert_eq!(clean_title("  claim  ").unwrap(), "claim");
    }

    #[test]
    fn symmetric_links_are_normalized() {
        let link = normalized("m-ffffffffff", "m-0000000000", LinkKind::RelatedTo, None, "cli", 0);
        assert_eq!(link.from, "m-0000000000");
        let link = normalized("m-ffffffffff", "m-0000000000", LinkKind::DependsOn, None, "cli", 0);
        assert_eq!(link.from, "m-ffffffffff");
    }
}
