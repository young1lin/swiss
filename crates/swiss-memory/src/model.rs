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

//! The record model (SPEC §memory.model): one note is a whole claim a human or a
//! model reads verbatim, and notes connect through typed links (the memory graph).
//! Every vocabulary here is closed - an unknown value is refused at the door, never
//! a silent string comparison somewhere downstream.

use serde::{Deserialize, Serialize};

/// Field limits (SPEC §memory.model). Over-limit input is refused, never truncated.
pub const TITLE_MAX: usize = 160;
pub const CONTENT_MAX: usize = 8 * 1024;
pub const ANCHORS_MAX: usize = 20;
pub const ANCHOR_PATH_MAX: usize = 400;
pub const REFS_MAX: usize = 5;
pub const REF_MAX: usize = 300;
pub const LINK_NOTE_MAX: usize = 200;
pub const LINKS_PER_RECORD_MAX: usize = 50;
pub const SAVE_LINKS_MAX: usize = 10;
/// Search keywords on a note or a domain (SPEC §memory.model): the terms a reader would
/// search for, in English and in the team's language, so a query in either finds it.
pub const KEYWORDS_MAX: usize = 16;
pub const KEYWORD_MAX: usize = 40;

/// Domain limits (SPEC §memory.domains).
pub const DOMAIN_TITLE_MAX: usize = 80;
pub const DOMAIN_SUMMARY_MAX: usize = 200;
pub const DOMAIN_BODY_MAX: usize = 48 * 1024;
pub const DOMAIN_OWNS_MAX: usize = 30;
pub const DOMAIN_DEPTH_MAX: usize = 5;
pub const DOMAIN_SEGMENT_MAX: usize = 40;
pub const DOMAINS_PER_PROJECT_MAX: usize = 500;
/// One `file` call moves at most this many notes.
pub const FILE_IDS_MAX: usize = 200;

/// A closed vocabulary with a stable wire spelling.
macro_rules! vocabulary {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// Parse strictly: an unknown value is a caller error, never a fallback.
            pub fn parse(value: &str) -> Option<$name> {
                match value {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }

            /// "a | b | c" for error messages.
            pub fn choices() -> String {
                [$($text),+].join(" | ")
            }
        }
    };
}

vocabulary!(
    /// What a note is.
    Kind {
        Decision => "decision",
        Constraint => "constraint",
        Pitfall => "pitfall",
        Knowledge => "knowledge",
        Preference => "preference",
    }
);

vocabulary!(
    /// Where a note stands. `active` and `needs_review` are "live".
    Status {
        Active => "active",
        NeedsReview => "needs_review",
        Superseded => "superseded",
        Invalidated => "invalidated",
    }
);

vocabulary!(
    /// Who wrote a note: set by the entry point, never by the caller.
    Origin {
        User => "user",
        Agent => "agent",
    }
);

vocabulary!(
    /// Who authored a note (ADR-032), apart from how it arrived. `unknown` is the CLI's own
    /// answer - a shell cannot tell a person from an agent - unless the caller declares one.
    Producer {
        Human => "human",
        Agent => "agent",
        Bootstrap => "bootstrap",
        Import => "import",
        Sync => "sync",
        Unknown => "unknown",
    }
);

vocabulary!(
    /// How a note arrived: the entry point that wrote it.
    Transport {
        Cli => "cli",
        Mcp => "mcp",
        Panel => "panel",
        Internal => "internal",
    }
);

vocabulary!(
    /// What a piece of evidence is (ADR-032).
    SourceType {
        Human => "human",
        Session => "session",
        Code => "code",
        Commit => "commit",
        Test => "test",
        Doc => "doc",
        Bootstrap => "bootstrap",
        Import => "import",
    }
);

vocabulary!(
    /// How a piece of evidence bears on a note.
    Relation {
        DerivedFrom => "derived_from",
        Supports => "supports",
        VerifiedBy => "verified_by",
    }
);

vocabulary!(
    /// How two notes relate (SPEC §memory.links).
    LinkKind {
        Supports => "supports",
        DependsOn => "depends_on",
        Contradicts => "contradicts",
        RelatedTo => "related_to",
        Supersedes => "supersedes",
    }
);

vocabulary!(
    /// Agent feedback on a note.
    Verdict {
        Verified => "verified",
        Outdated => "outdated",
        Wrong => "wrong",
    }
);

impl Producer {
    /// A producer a caller may declare (`swiss memory save --producer`): `sync` and `unknown`
    /// are the system's own answers, never input.
    pub fn declared(value: &str) -> Option<Producer> {
        match Producer::parse(value)? {
            Producer::Sync | Producer::Unknown => None,
            producer => Some(producer),
        }
    }

    /// "human | agent | bootstrap | import" for error messages.
    pub fn declarable() -> String {
        Producer::ALL
            .iter()
            .filter(|p| Producer::declared(p.as_str()).is_some())
            .map(Producer::as_str)
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// What a record line older than export format 4 is read as, before its actor decides.
    pub fn unknown() -> Producer {
        Producer::Unknown
    }
}

impl Transport {
    pub fn cli() -> Transport {
        Transport::Cli
    }
}

impl SourceType {
    /// A source a reader can check against the text: only these verify a note.
    pub fn verifies(&self) -> bool {
        matches!(self, SourceType::Code | SourceType::Test | SourceType::Commit | SourceType::Doc)
    }
}

/// The provenance of a note written before schema 6, or read from an export older than
/// format 4: derived from the actor label that wrote it. The store's migration and the
/// import both read it here, so the two never disagree.
pub fn legacy_provenance(origin: Origin, label: &str) -> (Producer, Transport) {
    match label {
        "panel" => (Producer::Human, Transport::Panel),
        "cli" => (Producer::Unknown, Transport::Cli),
        "sync" => (Producer::Sync, Transport::Internal),
        label if label == "mcp" || label.starts_with("mcp:") => (Producer::Agent, Transport::Mcp),
        _ => match origin {
            Origin::User => (Producer::Unknown, Transport::Cli),
            Origin::Agent => (Producer::Agent, Transport::Internal),
        },
    }
}

impl Status {
    /// Live notes are the ones briefs and default searches show.
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Active | Status::NeedsReview)
    }
}

impl LinkKind {
    /// Symmetric kinds are stored once, normalized `from < to`.
    pub fn is_symmetric(&self) -> bool {
        matches!(self, LinkKind::Contradicts | LinkKind::RelatedTo)
    }

    /// `supersedes` is written only by `save … replaces`.
    pub fn is_system(&self) -> bool {
        matches!(self, LinkKind::Supersedes)
    }

    /// The association weight of one hop over this kind (SPEC §memory.related).
    pub fn weight(&self) -> f64 {
        match self {
            LinkKind::Supersedes => 1.0,
            LinkKind::DependsOn | LinkKind::Contradicts => 0.9,
            LinkKind::Supports => 0.8,
            LinkKind::RelatedTo => 0.7,
        }
    }

    /// The phrase a brief or `get` prints, seen from `from`'s side (out) or `to`'s (in).
    pub fn phrase(&self, outgoing: bool) -> &'static str {
        match (self, outgoing) {
            (LinkKind::Supports, true) => "supports",
            (LinkKind::Supports, false) => "supported by",
            (LinkKind::DependsOn, true) => "depends on",
            (LinkKind::DependsOn, false) => "required by",
            (LinkKind::Contradicts, _) => "contradicts",
            (LinkKind::RelatedTo, _) => "related to",
            (LinkKind::Supersedes, true) => "supersedes",
            (LinkKind::Supersedes, false) => "superseded by",
        }
    }
}

/// A repo-relative path or glob a note is about; `hash` is the content baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub glob: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// One note, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub kind: Kind,
    pub title: String,
    pub content: String,
    pub status: Status,
    pub origin: Origin,
    pub actor: String,
    /// Who wrote the note and how it arrived (ADR-032): set when it is created, kept through
    /// every edit (the history says who edited it).
    #[serde(default = "Producer::unknown")]
    pub producer: Producer,
    #[serde(default = "Transport::cli")]
    pub transport: Transport,
    /// A person confirmed it (the panel, `swiss memory confirm`).
    pub confirmed: bool,
    /// Evidence from a checkable source verifies the current text: derived at read from the
    /// evidence rows, never stored on the note or exported.
    #[serde(skip)]
    pub source_verified: bool,
    pub anchors: Vec<Anchor>,
    pub refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The domain the note is filed in (SPEC §memory.domains): organisation, not content -
    /// outside the snapshot, and filing never bumps the revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// That domain's path as read (the export's key: domains are matched by path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    pub revision: u64,
    pub created_ms: i64,
    pub updated_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_ms: Option<i64>,
}

/// One piece of evidence for a note (ADR-032). It is bound to the text it was attached to:
/// `revision` is the note's revision then and `noteHash` its content hash then, so it bears on
/// the current text only while that hash is still the note's. A line number is never part of
/// it - a path, a symbol and the file's hash name the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub id: String,
    pub memory_id: String,
    pub revision: u64,
    pub note_hash: String,
    pub source_type: SourceType,
    pub relation: Relation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The cited file's hash when it was attached (code, test, doc under the project root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    /// The hash of the quoted excerpt; the excerpt itself is never kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_hash: Option<String>,
    pub actor: String,
    pub created_ms: i64,
}

/// One area of a project (SPEC §memory.domains): a node of the project's map. The body is
/// the hub - what the area is and where it lives, pointing at code - never a claim; `owns`
/// is the code it answers for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Domain {
    pub id: String,
    pub scope: String,
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    pub origin: Origin,
    pub actor: String,
    pub confirmed: bool,
    pub revision: u64,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// When the hub was last true to the code: a create, a body or `owns` change, a verify.
    pub verified_ms: i64,
}

impl Domain {
    pub fn depth(&self) -> usize {
        domain_depth(&self.path)
    }

    pub fn parent(&self) -> Option<&str> {
        domain_parent(&self.path)
    }
}

/// A domain's fields after one change (what its history diffs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainSnapshot {
    pub path: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    pub confirmed: bool,
}

impl DomainSnapshot {
    pub fn of(domain: &Domain) -> DomainSnapshot {
        DomainSnapshot {
            path: domain.path.clone(),
            title: domain.title.clone(),
            summary: domain.summary.clone(),
            body: domain.body.clone(),
            owns: domain.owns.clone(),
            keywords: domain.keywords.clone(),
            confirmed: domain.confirmed,
        }
    }
}

/// One append-only domain revision row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainRevision {
    pub revision: u64,
    pub op: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: i64,
    pub snapshot: DomainSnapshot,
}

/// A domain path's segments: 1..[DOMAIN_DEPTH_MAX], each `[a-z0-9][a-z0-9_-]{0,39}`. Err names what is wrong.
pub fn check_domain_path(raw: &str) -> Result<String, String> {
    let path = raw.trim().trim_matches('/');
    if path.is_empty() {
        return Err(format!(
            "path is required (one to {DOMAIN_DEPTH_MAX} segments joined by /, such as funds or funds/refunds)"
        ));
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > DOMAIN_DEPTH_MAX {
        return Err(format!("path {path:?} is {} levels deep; a map has at most {DOMAIN_DEPTH_MAX}", segments.len()));
    }
    for segment in &segments {
        let bytes = segment.as_bytes();
        let ok = !bytes.is_empty()
            && bytes.len() <= DOMAIN_SEGMENT_MAX
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-');
        if !ok {
            return Err(format!(
                "path {path:?}: each segment is 1..{DOMAIN_SEGMENT_MAX} of a-z, 0-9, _ and -, starting with a letter or \
                 a digit (the title carries any language)"
            ));
        }
    }
    Ok(path.to_string())
}

/// Keywords as a person or an agent gives them: each trimmed (inner whitespace folded),
/// one line, at most [KEYWORD_MAX] characters; empties dropped, repeats (case aside) once;
/// at most [KEYWORDS_MAX]. Err names what is wrong - over-limit input is refused.
pub fn check_keywords(raw: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw {
        let word = item.split_whitespace().collect::<Vec<_>>().join(" ");
        if word.is_empty() {
            continue;
        }
        let n = word.chars().count();
        if n > KEYWORD_MAX {
            return Err(format!("keyword {word:?} is {n} characters; the limit is {KEYWORD_MAX}"));
        }
        if !out.iter().any(|have| have.to_lowercase() == word.to_lowercase()) {
            out.push(word);
        }
    }
    if out.len() > KEYWORDS_MAX {
        return Err(format!("{} keywords; the limit is {KEYWORDS_MAX}", out.len()));
    }
    Ok(out)
}

/// How many segments a domain path has.
pub fn domain_depth(path: &str) -> usize {
    path.split('/').count()
}

/// The parent's path, None at the top level.
pub fn domain_parent(path: &str) -> Option<&str> {
    path.rfind('/').map(|at| &path[..at])
}

/// Whether `path` lies strictly under `ancestor`.
pub fn domain_is_under(path: &str, ancestor: &str) -> bool {
    path.len() > ancestor.len() && path.starts_with(ancestor) && path.as_bytes()[ancestor.len()] == b'/'
}

/// Tree order: parents before children, siblings by name (`a`, `a/x`, `a-b` - never a
/// byte order that slips `a-b` between `a` and `a/x`).
pub fn domain_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.split('/').cmp(b.split('/'))
}

/// One typed edge of the memory graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub actor: String,
    pub created_ms: i64,
}

impl Link {
    /// The other end, seen from `id`; None when the link does not touch it.
    pub fn other(&self, id: &str) -> Option<&str> {
        if self.from == id {
            Some(self.to.as_str())
        } else if self.to == id {
            Some(self.from.as_str())
        } else {
            None
        }
    }
}

/// The mutable fields after one change: what history diffs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub kind: Kind,
    pub title: String,
    pub content: String,
    pub status: Status,
    pub confirmed: bool,
    pub anchors: Vec<String>,
    pub refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

impl Snapshot {
    pub fn of(memory: &Memory) -> Snapshot {
        Snapshot {
            kind: memory.kind,
            title: memory.title.clone(),
            content: memory.content.clone(),
            status: memory.status,
            confirmed: memory.confirmed,
            anchors: memory.anchors.iter().map(|a| a.path.clone()).collect(),
            refs: memory.refs.clone(),
            keywords: memory.keywords.clone(),
        }
    }
}

/// One append-only revision row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub revision: u64,
    pub op: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: i64,
    pub snapshot: Snapshot,
}

/// How a directory was bound to a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Git,
    Path,
    Marker,
}

impl RootKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RootKind::Git => "git",
            RootKind::Path => "path",
            RootKind::Marker => "marker",
        }
    }

    pub fn parse(value: &str) -> Option<RootKind> {
        match value {
            "git" => Some(RootKind::Git),
            "path" => Some(RootKind::Path),
            "marker" => Some(RootKind::Marker),
            _ => None,
        }
    }
}

/// One bound directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub identity: String,
    pub kind: RootKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_url: Option<String>,
    pub added_ms: i64,
}

/// One project row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_ms: i64,
    pub roots: Vec<Root>,
}

/// Who performs a write: the origin and the declared actor label, and what a note it creates
/// records as its producer and transport (ADR-032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub origin: Origin,
    pub label: String,
    pub producer: Producer,
    pub transport: Transport,
}

impl Actor {
    /// The CLI: the producer is `unknown` until the caller declares one.
    pub fn cli() -> Actor {
        Actor::cli_as(Producer::Unknown)
    }

    /// The CLI with a declared producer (`--producer`: a declaration, not an authentication).
    pub fn cli_as(producer: Producer) -> Actor {
        Actor { origin: Origin::User, label: "cli".into(), producer, transport: Transport::Cli }
    }

    pub fn panel() -> Actor {
        Actor { origin: Origin::User, label: "panel".into(), producer: Producer::Human, transport: Transport::Panel }
    }

    /// The in-product sync (SPEC §memory.sync).
    pub fn sync() -> Actor {
        Actor {
            origin: Origin::Agent,
            label: crate::sync::ACTOR_LABEL.into(),
            producer: Producer::Sync,
            transport: Transport::Internal,
        }
    }

    /// Whether a note this actor creates starts confirmed. Only the panel's: a person typed it
    /// there. The CLI may be an agent with a shell, so its notes wait for `confirm` like an
    /// MCP client's (SPEC §memory.model - a workflow signal, not a security boundary).
    pub fn confirms(&self) -> bool {
        self.origin == Origin::User && self.label == "panel"
    }

    /// An MCP client; the label is the token label or the announced client name.
    pub fn mcp(client: Option<&str>) -> Actor {
        let label = match client {
            Some(name) if !name.trim().is_empty() => format!("mcp:{}", name.trim()),
            _ => "mcp".to_string(),
        };
        Actor { origin: Origin::Agent, label, producer: Producer::Agent, transport: Transport::Mcp }
    }
}

/// `m-` + 10 hex: opaque and stable.
pub fn new_record_id() -> String {
    format!("m-{}", swiss_core::util::random_hex(5))
}

/// `p-` + 8 hex.
pub fn new_project_id() -> String {
    format!("p-{}", swiss_core::util::random_hex(4))
}

/// `d-` + 8 hex.
pub fn new_domain_id() -> String {
    format!("d-{}", swiss_core::util::random_hex(4))
}

/// `e-` + 10 hex.
pub fn new_evidence_id() -> String {
    format!("e-{}", swiss_core::util::random_hex(5))
}

/// Whether a string has the evidence id shape.
pub fn is_evidence_id(value: &str) -> bool {
    value.len() == 12 && value.starts_with("e-") && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether a string has the domain id shape.
pub fn is_domain_id(value: &str) -> bool {
    value.len() == 10 && value.starts_with("d-") && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether a string has the record id shape.
pub fn is_record_id(value: &str) -> bool {
    value.len() == 12 && value.starts_with("m-") && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether a string has the project id shape (`global` included).
pub fn is_project_id(value: &str) -> bool {
    value == crate::scope::GLOBAL_PROJECT_ID
        || (value.len() == 10 && value.starts_with("p-") && value[2..].bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabularies_round_trip_and_refuse_unknowns() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(*kind));
        }
        for kind in LinkKind::ALL {
            assert_eq!(LinkKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(Kind::parse("note"), None);
        assert_eq!(Status::parse("candidate"), None);
        assert!(LinkKind::Contradicts.is_symmetric());
        assert!(!LinkKind::DependsOn.is_symmetric());
        assert!(LinkKind::Supersedes.is_system());
    }

    #[test]
    fn ids_have_their_shapes() {
        assert!(is_record_id(&new_record_id()));
        assert!(is_project_id(&new_project_id()));
        assert!(is_project_id("global"));
        assert!(!is_record_id("m-xyz"));
        assert!(is_domain_id(&new_domain_id()));
        assert!(!is_domain_id("funds"));
        assert!(is_evidence_id(&new_evidence_id()));
        assert!(!is_evidence_id(&new_record_id()));
    }

    #[test]
    fn legacy_provenance_reads_the_actor() {
        assert_eq!(legacy_provenance(Origin::User, "panel"), (Producer::Human, Transport::Panel));
        assert_eq!(legacy_provenance(Origin::User, "cli"), (Producer::Unknown, Transport::Cli));
        assert_eq!(legacy_provenance(Origin::Agent, "mcp:claude-code"), (Producer::Agent, Transport::Mcp));
        assert_eq!(legacy_provenance(Origin::Agent, "sync"), (Producer::Sync, Transport::Internal));
        assert_eq!(legacy_provenance(Origin::Agent, "mcpish"), (Producer::Agent, Transport::Internal));
        assert_eq!(legacy_provenance(Origin::User, "someone"), (Producer::Unknown, Transport::Cli));
        assert_eq!(Producer::declarable(), "human | agent | bootstrap | import");
        for actor in [Actor::cli(), Actor::panel(), Actor::mcp(Some("x")), Actor::sync()] {
            assert_eq!(legacy_provenance(actor.origin, &actor.label), (actor.producer, actor.transport), "{}", actor.label);
        }
    }

    #[test]
    fn domain_paths_have_their_shape_and_order() {
        assert_eq!(check_domain_path(" funds/refunds/ ").unwrap(), "funds/refunds");
        assert_eq!(check_domain_path("risk-engine/broadcast_v2").unwrap(), "risk-engine/broadcast_v2");
        assert_eq!(check_domain_path("a/b/c/d/e").unwrap(), "a/b/c/d/e", "five levels");
        for bad in ["", "Funds", "a/b/c/d/e/f", "a//b", "-a", "资金", "a b", &"x".repeat(41)] {
            assert!(check_domain_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!(domain_parent("a/b/c"), Some("a/b"));
        assert_eq!(domain_parent("a"), None);
        assert!(domain_is_under("a/b", "a"));
        assert!(!domain_is_under("ab", "a"));
        assert!(!domain_is_under("a", "a"));
        let mut paths = vec!["a-b", "a/x", "a", "b", "a/x/y"];
        paths.sort_by(|x, y| domain_order(x, y));
        assert_eq!(paths, ["a", "a/x", "a/x/y", "a-b", "b"]);
    }
}
