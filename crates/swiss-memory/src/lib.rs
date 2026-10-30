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

//! The long-term memory subsystem (SPEC §memory): short, typed, linked notes -
//! decisions, constraints, pitfalls, knowledge and preferences - scoped to a project
//! and shared by every entry point (the offline CLI, the /mcp/memory endpoint and the
//! panel) over one storage engine and one rule set. Notes form a graph: typed links,
//! association search over links, text and anchors, and a brief that hands an agent
//! what matters for the paths and task it is about to touch. A project's notes are
//! filed under a map of its domains, so the brief leads with where the caller is.
//! Beside the notes a project keeps its documents whole - design docs, plans, pitfall
//! logs - each cut into searchable sections and carded with the code it names
//! ([crate::docs]).
//!
//! Storage is one SQLite database under the Swiss home (ADR-031): WAL, a 5 s busy
//! timeout and BEGIN IMMEDIATE mutations, so independent processes (the gateway and
//! a CLI run) share one home without losing writes - the property that decided the
//! engine. The store is plain SQLite, not a sealed envelope; the home's OS-private
//! placement is the privacy boundary.
//!
//! This crate knows nothing about gateways: no axum, no rmcp, no host services. The
//! routes ([crate::service::MemoryService] consumers) and the MCP handler live in
//! the root crate's src/, like terminal's and remote's.

pub mod brief;
pub mod code;
pub mod docs;
pub mod domain;
pub mod embed;
pub mod error;
pub mod eval;
pub mod evidence;
pub mod formation;
pub mod fresh;
pub mod glob;
pub mod graph;
pub mod init;
pub mod misses;
pub mod model;
pub mod opposed;
pub mod readout;
pub mod recheck;
pub mod related;
pub mod render;
pub mod scope;
pub mod scopes;
pub mod search;
pub mod secrets;
pub mod service;
pub mod settings;
pub mod store;
pub mod survey;
pub mod sync;
pub mod transcript;
pub mod transfer;
pub mod view;

pub use domain::{
    Checkout, CommitFiles, CoverageView, DeleteOutcome, DomainCounts, DomainDetail, DomainList, DomainRequest, DomainUpdate,
    DomainView, FileOutcome, GitIssue, TreeConfirmed,
};
pub use embed::{Embedder, EmbeddingStatus, IndexReport, Purpose, SemanticState};
pub use error::{Code, MemError, MemResult};
pub use graph::{GraphEdge, GraphNode, GraphView};
pub use evidence::EvidenceSpec;
pub use misses::{MissExport, MissRequest, RecordedMiss};
pub use model::{
    Actor, Domain, DomainRevision, Evidence, Kind, LinkKind, Memory, Origin, Producer, Relation, Revision, SourceType, Status, Transport,
    Verdict,
};
pub use related::{Reason, Seed};
pub use scope::{Scope, GLOBAL_PROJECT_ID};
pub use search::SearchQuery;
pub use service::{
    BriefOutput, BriefRequest, GraphRequest, InitMode, InitOutcome, LinkSpec, ListRequest, MemoryService, RelatedRequest,
    SaveOutcome, SaveRequest, SearchOutcome, StatusView, Target, UpdateOutcome, UpdateRequest,
};
pub use transfer::{ConflictKind, ExportScope, ImportConflict, ImportOptions, ImportReport, ImportStrategy};
pub use view::{HitView, ProjectView, RecordView, RelatedView, Summary};
