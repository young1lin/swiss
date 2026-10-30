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

//! The memory graph (SPEC §memory.links): an in-memory index over one scope's notes
//! and typed links, the markers it propagates (`contradicted`, `dependency_broken`),
//! and the graph read the panel draws. Scopes are small by design (hundreds to a few
//! thousand notes), so one load per call keeps every traversal exact and simple.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::fresh::{AnchorState, Checker};
use crate::model::{Kind, Link, LinkKind, Memory, Origin, Status};

/// Live notes older than this (since verified, else since updated) are `old`.
pub const OLD_AFTER_DAYS: i64 = 180;
pub const DEPTH_MAX: u32 = 3;
pub const GRAPH_LIMIT_MIN: usize = 10;
pub const GRAPH_LIMIT_MAX: usize = 300;
pub const GRAPH_LIMIT_DEFAULT: usize = 200;

/// What the graph needs to know about one note.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub kind: Kind,
    pub title: String,
    pub status: Status,
    pub confirmed: bool,
    pub origin: Origin,
    pub updated_ms: i64,
    /// The path of the domain the note is filed in.
    pub domain: Option<String>,
}

impl NodeInfo {
    pub fn of(memory: &Memory) -> NodeInfo {
        NodeInfo {
            kind: memory.kind,
            title: memory.title.clone(),
            status: memory.status,
            confirmed: memory.confirmed,
            origin: memory.origin,
            updated_ms: memory.updated_ms,
            domain: memory.domain_path.clone(),
        }
    }
}

/// One scope's notes and links, indexed both directions.
#[derive(Debug, Default)]
pub struct Net {
    pub nodes: HashMap<String, NodeInfo>,
    pub links: Vec<Link>,
    adj: HashMap<String, Vec<usize>>,
}

impl Net {
    pub fn new(nodes: HashMap<String, NodeInfo>, links: Vec<Link>) -> Net {
        let mut adj: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, link) in links.iter().enumerate() {
            adj.entry(link.from.clone()).or_default().push(i);
            if link.to != link.from {
                adj.entry(link.to.clone()).or_default().push(i);
            }
        }
        Net { nodes, links, adj }
    }

    pub fn from_memories(memories: &[Memory], links: Vec<Link>) -> Net {
        Net::new(memories.iter().map(|m| (m.id.clone(), NodeInfo::of(m))).collect(), links)
    }

    /// Links touching `id`, in stored order.
    pub fn touching<'a>(&'a self, id: &str) -> impl Iterator<Item = &'a Link> + 'a {
        self.adj.get(id).into_iter().flatten().map(move |i| &self.links[*i])
    }

    pub fn degree(&self, id: &str) -> usize {
        self.adj.get(id).map(Vec::len).unwrap_or(0)
    }

    pub fn status(&self, id: &str) -> Option<Status> {
        self.nodes.get(id).map(|n| n.status)
    }

    fn is_live(&self, id: &str) -> bool {
        self.status(id).map(|s| s.is_live()).unwrap_or(false)
    }
}

/// One reason a note deserves attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    Unconfirmed,
    NeedsReview,
    AnchorChanged(String),
    Old,
    Contradicted(String),
    DependencyBroken { id: String, status: Status },
}

impl Marker {
    /// The wire string.
    pub fn code(&self) -> &'static str {
        match self {
            Marker::Unconfirmed => "unconfirmed",
            Marker::NeedsReview => "needs_review",
            Marker::AnchorChanged(_) => "anchor_changed",
            Marker::Old => "old",
            Marker::Contradicted(_) => "contradicted",
            Marker::DependencyBroken { .. } => "dependency_broken",
        }
    }

    /// The brief's rendering.
    pub fn render(&self) -> String {
        match self {
            Marker::Unconfirmed => "unconfirmed".into(),
            Marker::NeedsReview => "needs review".into(),
            Marker::AnchorChanged(path) => format!("⚠ anchor changed: {path}"),
            Marker::Old => "old".into(),
            Marker::Contradicted(id) => format!("⚠ contradicted by {id}"),
            Marker::DependencyBroken { id, status } => format!("⚠ depends on {} {id}", status.as_str()),
        }
    }
}

/// Unique wire codes in a stable order.
pub fn codes(markers: &[Marker]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for marker in markers {
        if !out.contains(&marker.code()) {
            out.push(marker.code());
        }
    }
    out
}

/// Every marker of one note, plus the per-anchor states the checker computed.
pub fn markers(memory: &Memory, net: &Net, checker: &mut Checker, now: i64) -> (Vec<Marker>, Vec<AnchorState>) {
    let mut out = Vec::new();
    let states: Vec<AnchorState> = memory.anchors.iter().map(|a| checker.state(a)).collect();
    // Any origin: a CLI note waits for `confirm` like an MCP client's (Actor::confirms), and the
    // counts (store::counts, DomainCounts) say every unconfirmed note - the inbox must list them.
    if !memory.confirmed {
        out.push(Marker::Unconfirmed);
    }
    if memory.status == Status::NeedsReview {
        out.push(Marker::NeedsReview);
    }
    if !memory.status.is_live() {
        return (out, states);
    }
    for (anchor, state) in memory.anchors.iter().zip(states.iter()) {
        if state.is_alarm() {
            out.push(Marker::AnchorChanged(anchor.path.clone()));
        }
    }
    let since = memory.verified_ms.unwrap_or(memory.updated_ms);
    if (now - since) / 86_400_000 >= OLD_AFTER_DAYS {
        out.push(Marker::Old);
    }
    for link in net.touching(&memory.id) {
        let Some(other) = link.other(&memory.id) else { continue };
        match link.kind {
            LinkKind::Contradicts if net.is_live(other) => out.push(Marker::Contradicted(other.to_string())),
            LinkKind::DependsOn if link.from == memory.id => {
                if let Some(status @ (Status::Invalidated | Status::Superseded)) = net.status(other) {
                    out.push(Marker::DependencyBroken { id: other.to_string(), status });
                }
            }
            _ => {}
        }
    }
    (out, states)
}

/// One graph node on the wire.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub status: Status,
    pub confirmed: bool,
    pub origin: Origin,
    pub degree: usize,
    pub hop: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// One graph edge on the wire.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
    pub directed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl GraphEdge {
    pub fn of(link: &Link) -> GraphEdge {
        GraphEdge {
            from: link.from.clone(),
            to: link.to.clone(),
            kind: link.kind,
            directed: !link.kind.is_symmetric(),
            note: link.note.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
}

/// The graph read (SPEC §memory.graph). `center` must be a node of `net`.
pub fn read(net: &Net, center: Option<&str>, depth: u32, include_history: bool, limit: usize) -> GraphView {
    let limit = limit.clamp(GRAPH_LIMIT_MIN, GRAPH_LIMIT_MAX);
    let depth = depth.clamp(1, DEPTH_MAX);
    let visible = |id: &str| include_history || net.is_live(id) || Some(id) == center;
    let mut picked: Vec<(String, Option<u32>)> = Vec::new();
    let truncated = match center {
        Some(center) => {
            let mut hops: HashMap<String, u32> = HashMap::new();
            hops.insert(center.to_string(), 0);
            let mut frontier = vec![center.to_string()];
            for hop in 1..=depth {
                let mut next: BTreeSet<String> = BTreeSet::new();
                for node in &frontier {
                    for link in net.touching(node) {
                        let Some(other) = link.other(node) else { continue };
                        if hops.contains_key(other) || !net.nodes.contains_key(other) || !visible(other) {
                            continue;
                        }
                        next.insert(other.to_string());
                    }
                }
                for id in &next {
                    hops.insert(id.clone(), hop);
                }
                frontier = next.into_iter().collect();
            }
            let mut ordered: Vec<(String, u32)> = hops.into_iter().collect();
            ordered.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            let truncated = ordered.len() > limit;
            ordered.truncate(limit);
            picked.extend(ordered.into_iter().map(|(id, hop)| (id, Some(hop))));
            truncated
        }
        None => {
            let mut ids: Vec<&String> = net.nodes.keys().filter(|id| visible(id)).collect();
            let degree = |id: &str| net.touching(id).filter(|l| l.other(id).map(visible).unwrap_or(false)).count();
            ids.sort_by(|a, b| {
                degree(b)
                    .cmp(&degree(a))
                    .then_with(|| net.nodes[*b].updated_ms.cmp(&net.nodes[*a].updated_ms))
                    .then_with(|| a.cmp(b))
            });
            let truncated = ids.len() > limit;
            ids.truncate(limit);
            picked.extend(ids.into_iter().map(|id| (id.clone(), None)));
            truncated
        }
    };
    let chosen: HashSet<&str> = picked.iter().map(|(id, _)| id.as_str()).collect();
    let nodes = picked
        .iter()
        .filter_map(|(id, hop)| {
            let info = net.nodes.get(id)?;
            Some(GraphNode {
                id: id.clone(),
                kind: info.kind,
                title: info.title.clone(),
                status: info.status,
                confirmed: info.confirmed,
                origin: info.origin,
                degree: net.touching(id).filter(|l| l.other(id).map(visible).unwrap_or(false)).count(),
                hop: *hop,
                domain: info.domain.clone(),
            })
        })
        .collect();
    let edges = net
        .links
        .iter()
        .filter(|l| chosen.contains(l.from.as_str()) && chosen.contains(l.to.as_str()))
        .map(GraphEdge::of)
        .collect();
    GraphView { nodes, edges, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(status: Status) -> NodeInfo {
        NodeInfo {
            kind: Kind::Decision,
            title: "t".into(),
            status,
            confirmed: true,
            origin: Origin::User,
            updated_ms: 0,
            domain: None,
        }
    }

    fn link(from: &str, to: &str, kind: LinkKind) -> Link {
        Link { from: from.into(), to: to.into(), kind, note: None, actor: "cli".into(), created_ms: 0 }
    }

    fn net() -> Net {
        let mut nodes = HashMap::new();
        for id in ["m-a", "m-b", "m-c", "m-d"] {
            nodes.insert(id.to_string(), node(Status::Active));
        }
        nodes.insert("m-old".to_string(), node(Status::Superseded));
        Net::new(
            nodes,
            vec![
                link("m-a", "m-b", LinkKind::DependsOn),
                link("m-b", "m-c", LinkKind::Supports),
                link("m-c", "m-d", LinkKind::RelatedTo),
                link("m-a", "m-old", LinkKind::Supersedes),
            ],
        )
    }

    #[test]
    fn a_centered_read_walks_both_directions_to_depth() {
        let net = net();
        let one = read(&net, Some("m-b"), 1, false, 200);
        let ids: Vec<&str> = one.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["m-b", "m-a", "m-c"]);
        assert_eq!(one.edges.len(), 2);
        let two = read(&net, Some("m-b"), 2, false, 200);
        assert_eq!(two.nodes.len(), 4, "history stays hidden without includeHistory");
        let all = read(&net, Some("m-b"), 2, true, 200);
        assert_eq!(all.nodes.len(), 5);
    }

    #[test]
    fn the_project_read_puts_hubs_first_and_reports_truncation() {
        let net = net();
        let view = read(&net, None, 1, false, 10);
        assert_eq!(view.nodes.len(), 4);
        assert!(!view.truncated);
        assert!(view.nodes[0].degree >= view.nodes[3].degree);
        assert!(view.nodes.iter().all(|n| n.hop.is_none()));
    }
}
