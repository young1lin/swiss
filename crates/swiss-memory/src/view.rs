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

//! Wire shapes (SPEC §memory.api): what the API returns, the CLI's `--json` prints
//! and the MCP adapter renders. camelCase JSON; ISO strings never - epoch ms only.

use serde::Serialize;

use crate::fresh::AnchorState;
use crate::graph::{codes, Marker, Net};
use crate::model::{Evidence, Kind, LinkKind, Memory, Origin, Producer, Project, Relation, SourceType, Status, Transport};
use crate::related::Reason;

pub const EXCERPT_MAX: usize = 300;
const DAY_MS: i64 = 86_400_000;

/// One note in a list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub scope: String,
    pub kind: Kind,
    pub title: String,
    pub status: Status,
    pub origin: Origin,
    pub actor: String,
    /// Who wrote it and how it arrived (ADR-032).
    pub producer: Producer,
    pub transport: Transport,
    /// A person confirmed it.
    pub confirmed: bool,
    /// Evidence from a checkable source (code, a test, a commit, a doc) verifies the current
    /// text - apart from `confirmed`: who checked a note is not who wrote it.
    pub source_verified: bool,
    pub revision: u64,
    pub updated_ms: i64,
    pub age_days: i64,
    pub markers: Vec<&'static str>,
    pub link_count: usize,
    /// The domain the note is filed in (its path); None: unfiled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorView {
    pub path: String,
    pub glob: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub state: &'static str,
}

/// One neighbour of a note.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkView {
    /// `out` when the note is the link's `from` (symmetric kinds are always `out`).
    pub dir: &'static str,
    pub kind: LinkKind,
    pub id: String,
    pub title: String,
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One piece of a note's evidence; `current` while it was attached to the text the note has now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceView {
    pub id: String,
    pub revision: u64,
    pub source_type: SourceType,
    pub relation: Relation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_hash: Option<String>,
    pub actor: String,
    pub created_ms: i64,
    pub current: bool,
}

impl EvidenceView {
    pub fn of(evidence: &Evidence, note_hash: &str) -> EvidenceView {
        EvidenceView {
            id: evidence.id.clone(),
            revision: evidence.revision,
            source_type: evidence.source_type,
            relation: evidence.relation,
            path: evidence.path.clone(),
            symbol: evidence.symbol.clone(),
            commit_sha: evidence.commit_sha.clone(),
            session_id: evidence.session_id.clone(),
            run_id: evidence.run_id.clone(),
            source_hash: evidence.source_hash.clone(),
            excerpt_hash: evidence.excerpt_hash.clone(),
            actor: evidence.actor.clone(),
            created_ms: evidence.created_ms,
            current: evidence.note_hash == note_hash,
        }
    }

    /// Whether this piece verifies the current text.
    pub fn verifies(&self) -> bool {
        self.current && self.relation == Relation::VerifiedBy && self.source_type.verifies()
    }

    /// One line: `verified_by code src/retry.rs#retry (rev 2)`, `(earlier text)` once it is not
    /// current.
    pub fn render(&self) -> String {
        let mut out = format!("{} {}", self.relation.as_str(), self.source_type.as_str());
        let locator = [&self.path, &self.commit_sha, &self.session_id, &self.run_id].into_iter().flatten().next();
        if let Some(locator) = locator {
            out.push(' ');
            out.push_str(locator);
        }
        if let Some(symbol) = &self.symbol {
            out.push('#');
            out.push_str(symbol);
        }
        out.push_str(&format!(" (rev {}", self.revision));
        if !self.current {
            out.push_str(", earlier text");
        }
        out.push(')');
        out
    }
}

/// One note in full.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    #[serde(flatten)]
    pub summary: Summary,
    pub content: String,
    pub anchors: Vec<AnchorView>,
    pub refs: Vec<String>,
    /// The terms it is searched by, in English and the team's language.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    pub created_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_ms: Option<i64>,
    pub links: Vec<LinkView>,
    /// What the note was derived from, what supports it and what verifies it, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceView>,
    /// The rendered markers (brief wording), for the CLI and MCP.
    #[serde(skip)]
    pub marker_text: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitView {
    #[serde(flatten)]
    pub summary: Summary,
    pub score: i64,
    pub tier: &'static str,
    /// Cosine to the query, three decimals, when the semantic tier ran and the note has
    /// a current vector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// The share of the query's word weight the note holds, three decimals, when a word
    /// tier measured it: what the retrieval readout judges (crate::readout).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_share: Option<f64>,
}

impl HitView {
    /// How it holds the query, for the retrieval readout.
    pub fn hold(&self) -> crate::readout::Hold {
        crate::readout::Hold::of(self.tier, self.word_share)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedView {
    #[serde(flatten)]
    pub summary: Summary,
    pub score: u32,
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootView {
    pub identity: String,
    pub kind: &'static str,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_url: Option<String>,
    pub added_ms: i64,
    pub reachable: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub live: u64,
    pub needs_review: u64,
    pub unconfirmed: u64,
    pub history: u64,
    pub links: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub global: bool,
    pub created_ms: i64,
    pub roots: Vec<RootView>,
    pub counts: Counts,
}

impl ProjectView {
    pub fn of(project: &Project, counts: Option<&[u64; 5]>) -> ProjectView {
        let c = counts.copied().unwrap_or([0; 5]);
        ProjectView {
            id: project.id.clone(),
            name: project.name.clone(),
            global: project.id == crate::scope::GLOBAL_PROJECT_ID,
            created_ms: project.created_ms,
            roots: project
                .roots
                .iter()
                .map(|r| RootView {
                    identity: r.identity.clone(),
                    kind: r.kind.as_str(),
                    path: crate::scope::display(std::path::Path::new(&r.path)),
                    origin_url: r.origin_url.clone(),
                    added_ms: r.added_ms,
                    reachable: std::path::Path::new(&r.path).is_dir(),
                })
                .collect(),
            counts: Counts { live: c[0], needs_review: c[1], unconfirmed: c[2], history: c[3], links: c[4] },
        }
    }
}

/// Whole days since the note last changed.
pub fn age_days(memory: &Memory, now: i64) -> i64 {
    ((now - memory.updated_ms) / DAY_MS).max(0)
}

/// A one-line excerpt of at most `max` characters.
pub fn excerpt(content: &str, max: usize) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&flat, max)
}

/// The first `max` characters, with an ellipsis when cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn summary(memory: &Memory, markers: &[Marker], net: &Net, now: i64, with_excerpt: bool) -> Summary {
    Summary {
        id: memory.id.clone(),
        scope: memory.scope.clone(),
        kind: memory.kind,
        title: memory.title.clone(),
        status: memory.status,
        origin: memory.origin,
        actor: memory.actor.clone(),
        producer: memory.producer,
        transport: memory.transport,
        confirmed: memory.confirmed,
        source_verified: memory.source_verified,
        revision: memory.revision,
        updated_ms: memory.updated_ms,
        age_days: age_days(memory, now),
        markers: codes(markers),
        link_count: net.degree(&memory.id),
        domain: memory.domain_path.clone(),
        excerpt: with_excerpt.then(|| excerpt(&memory.content, EXCERPT_MAX)),
    }
}

/// The neighbours of a note, grouped by kind, then direction, then id.
pub fn link_views(memory: &Memory, net: &Net) -> Vec<LinkView> {
    let mut out: Vec<LinkView> = net
        .touching(&memory.id)
        .filter_map(|link| {
            let other = link.other(&memory.id)?;
            let info = net.nodes.get(other);
            let outgoing = link.from == memory.id || link.kind.is_symmetric();
            Some(LinkView {
                dir: if outgoing { "out" } else { "in" },
                kind: link.kind,
                id: other.to_string(),
                title: info.map(|i| i.title.clone()).unwrap_or_else(|| "(purged)".into()),
                status: info.map(|i| i.status),
                note: link.note.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.dir.cmp(b.dir)).then_with(|| a.id.cmp(&b.id)));
    out
}

/// The note in full; `evidence` decides `sourceVerified` against the text the note has here.
pub fn record(
    memory: &Memory,
    markers: &[Marker],
    states: &[AnchorState],
    net: &Net,
    now: i64,
    evidence: &[Evidence],
) -> RecordView {
    let note_hash = crate::store::content_hash(&memory.title, &memory.content);
    let evidence: Vec<EvidenceView> = evidence.iter().map(|e| EvidenceView::of(e, &note_hash)).collect();
    let mut summary = summary(memory, markers, net, now, false);
    summary.source_verified = evidence.iter().any(EvidenceView::verifies);
    RecordView {
        summary,
        content: memory.content.clone(),
        anchors: memory
            .anchors
            .iter()
            .zip(states.iter().copied().chain(std::iter::repeat(AnchorState::None)))
            .map(|(a, state)| AnchorView { path: a.path.clone(), glob: a.glob, hash: a.hash.clone(), state: state.as_str() })
            .collect(),
        refs: memory.refs.clone(),
        keywords: memory.keywords.clone(),
        superseded_by: memory.superseded_by.clone(),
        created_ms: memory.created_ms,
        verified_ms: memory.verified_ms,
        links: link_views(memory, net),
        evidence,
        marker_text: markers.iter().map(Marker::render).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_flatten_and_clip() {
        assert_eq!(excerpt("a\n  b\tc", 10), "a b c");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("缓存失效问题", 3), "缓存…");
    }
}
