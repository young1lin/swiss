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

//! Association search (SPEC §memory.related): "what is this connected to?". Three
//! signals, each 0..1 - graph proximity over typed links, text similarity (bm25 over
//! the seed's salient terms; the search's IDF-weighted CJK coverage for CJK text; the
//! cosine of stored vectors when an embedder indexed the scope), and anchor overlap -
//! combined as `round(100 · (0.5g + 0.35t + 0.15a))`. Every result carries the
//! evidence behind its score, so a human can see why two notes were put side by side.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;
use sqlx::sqlite::SqliteConnection;

use crate::error::MemResult;
use crate::glob;
use crate::graph::{Net, DEPTH_MAX};
use crate::model::{Anchor, LinkKind, Memory, Status};
use crate::search;
use crate::store;

pub const SEED_TEXT_MAX: usize = 4 * 1024;
pub const LIMIT_DEFAULT: usize = 20;
pub const LIMIT_MAX: usize = 50;
pub const DEPTH_DEFAULT: u32 = 2;
const TERMS_MAX: usize = 24;
/// CJK units a seed contributes (a seed is a passage, not a query: more than a query's).
const CJK_UNITS_MAX: usize = 128;
const TEXT_SEEDS: usize = 5;
const W_GRAPH: f64 = 0.5;
const W_TEXT: f64 = 0.35;
const W_ANCHOR: f64 = 0.15;

pub(crate) const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "do", "does", "for", "from", "has", "have",
    "how", "if", "in", "into", "is", "it", "its", "may", "must", "no", "not", "of", "on", "or", "should", "so", "than",
    "that", "the", "their", "then", "there", "these", "this", "to", "use", "was", "we", "were", "what", "when", "which",
    "while", "will", "with", "you", "your",
];

/// What the association starts from.
#[derive(Debug, Clone)]
pub enum Seed {
    Id(String),
    Text(String),
}

/// One piece of evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reason {
    Link { kind: LinkKind, hops: u32, via: String },
    Text { terms: Vec<String> },
    Anchor { paths: Vec<String> },
    /// Close in meaning: the cosine of the two notes' vectors (SPEC §memory.embedding).
    Similar { similarity: f32 },
}

/// One association request.
#[derive(Debug, Clone)]
pub struct Request {
    pub seed: Seed,
    pub depth: u32,
    pub limit: usize,
    pub include_history: bool,
    /// Cosine of the seed to each note at or above `floor`, when the scope has
    /// vectors (empty otherwise).
    pub similar: HashMap<String, f32>,
    /// The provider's similarity floor ([crate::embed::Embedder::floor]).
    pub floor: f32,
}

/// One associated note.
#[derive(Debug, Clone)]
pub struct Associated {
    pub id: String,
    pub score: u32,
    pub reasons: Vec<Reason>,
}

/// The salient terms of a seed: stop-words and 1-char Latin tokens dropped, ≤ 24.
pub fn salient(text: &str) -> Vec<String> {
    search::terms(text)
        .into_iter()
        .filter(|t| !STOP_WORDS.contains(&t.as_str()))
        .filter(|t| !(t.chars().count() == 1 && t.is_ascii()))
        .take(TERMS_MAX)
        .collect()
}

/// The CJK passages `text` shares with `hay`, longest first, at most 5: chains of
/// matched bigrams merged back into the phrase they spell when the note holds it whole
/// (`缓存失效`, not `缓存, 存失, 失效`), the bigrams themselves when it does not.
fn shared_phrases(text: &str, matched: &[String], hay: &str) -> Vec<String> {
    let pairs: HashSet<(char, char)> = matched
        .iter()
        .filter_map(|u| {
            let mut chars = u.chars();
            Some((chars.next()?, chars.next()?))
        })
        .collect();
    let hit = |a: char, b: char| search::is_cjk(a) && search::is_cjk(b) && pairs.contains(&(a, b));
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let push = |phrase: String, out: &mut Vec<String>| {
        if !out.contains(&phrase) {
            out.push(phrase);
        }
    };
    let mut i = 0;
    while i + 1 < chars.len() {
        if !hit(chars[i], chars[i + 1]) {
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < chars.len() && hit(chars[end - 1], chars[end]) {
            end += 1;
        }
        let phrase: String = chars[i..end].iter().collect();
        if end - i == 2 || hay.contains(&phrase) {
            push(phrase, &mut out);
        } else {
            for k in i..end - 1 {
                push(chars[k..k + 2].iter().collect(), &mut out);
            }
        }
        i = end;
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.chars().count()));
    out.truncate(5);
    out
}

/// File-like tokens a passage mentions, normalized.
fn mentioned_paths(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || "`'\"()[]{}<>,;".contains(c)) {
        let token = raw.trim_end_matches(['.', ':', '!', '?']);
        if glob::looks_like_path(token) {
            let path = glob::normalize(token);
            if !path.is_empty() && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

fn path_hits_anchor(path: &str, anchor: &Anchor) -> bool {
    glob::anchor_matches(anchor, path) || anchor.path.ends_with(&format!("/{path}"))
}

/// The first `max` bytes of `text` at a char boundary.
fn head(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One note's text match before its words are spelled out: its share `t` in 0..1,
/// whether the word tier found it, and the CJK units it holds when the CJK tier did.
struct TextHit {
    t: f64,
    words: bool,
    units: Option<Vec<String>>,
}

/// Text similarity of every note to `text`: id -> its [TextHit], and the salient terms
/// the word tier asked for. The seed note is left out before normalizing: it always
/// matches its own text best and would flatten every neighbour towards zero. The words
/// a match shares ([hit_terms]) wait for the notes an association keeps.
async fn text_signal(
    conn: &mut SqliteConnection,
    project_id: &str,
    text: &str,
    pool: &HashMap<&str, &Memory>,
    statuses: &[Status],
    exclude: Option<&str>,
) -> MemResult<(Vec<String>, HashMap<String, TextHit>)> {
    let mut out: HashMap<String, TextHit> = HashMap::new();
    let terms = salient(text);
    if !terms.is_empty() {
        let ranked: Vec<(String, f64)> = store::fts(conn, project_id, &search::or_expr(&terms), statuses, 200)
            .await?
            .into_iter()
            .filter(|(id, _)| Some(id.as_str()) != exclude && pool.contains_key(id.as_str()))
            .collect();
        let best = ranked.first().map(|(_, r)| *r).unwrap_or(0.0);
        for (id, rank) in ranked {
            let t = if best < 0.0 { (rank / best).clamp(0.0, 1.0) } else { 1.0 };
            out.insert(id, TextHit { t, words: true, units: None });
        }
    }
    if search::has_cjk(text) {
        // The search's CJK tier, normalized to the best neighbour like bm25 above.
        let units = search::cjk_units(text, CJK_UNITS_MAX);
        let candidates = pool.iter().filter(|(id, _)| Some(**id) != exclude).map(|(_, m)| *m);
        let covered = search::coverage(candidates, &units);
        let best = covered.iter().map(|c| c.coverage).fold(0.0, f64::max);
        for c in covered {
            let t = if best > 0.0 { c.coverage / best } else { 0.0 };
            let entry = out.entry(c.id).or_insert(TextHit { t: 0.0, words: false, units: None });
            if t > entry.t {
                entry.t = t;
            }
            entry.units = Some(c.matched);
        }
    }
    Ok((terms, out))
}

/// What a text match shares with `memory`, at most five: the salient `terms` it holds
/// when the word tier found it, then the whole phrases of `text` its CJK units make.
fn hit_terms(text: &str, terms: &[String], hit: &TextHit, memory: &Memory) -> Vec<String> {
    let hay = format!("{}\n{}", memory.title, memory.content).to_lowercase();
    let mut out: Vec<String> =
        if hit.words { terms.iter().filter(|t| hay.contains(t.as_str())).take(5).cloned().collect() } else { Vec::new() };
    if let Some(units) = &hit.units {
        for phrase in shared_phrases(text, units, &hay) {
            if out.len() < 5 && !out.contains(&phrase) {
                out.push(phrase);
            }
        }
    }
    out
}

/// The anchors `memory` shares with the seed note (or, for a passage, the paths the
/// passage mentions that its anchors cover): the share in 0..1, and up to five of them.
fn anchor_share(seed: Option<&Memory>, mentioned: &[String], memory: &Memory) -> (f64, Vec<String>) {
    match seed {
        Some(seed) if !seed.anchors.is_empty() && !memory.anchors.is_empty() => {
            let shared: Vec<String> = seed
                .anchors
                .iter()
                .filter(|sa| memory.anchors.iter().any(|ma| glob::anchors_overlap(sa, ma)))
                .map(|sa| sa.path.clone())
                .collect();
            if shared.is_empty() {
                return (0.0, Vec::new());
            }
            let union = seed.anchors.len() + memory.anchors.len() - shared.len();
            (shared.len() as f64 / union.max(1) as f64, shared.into_iter().take(5).collect())
        }
        None if !mentioned.is_empty() => {
            let hit: Vec<String> =
                mentioned.iter().filter(|p| memory.anchors.iter().any(|a| path_hits_anchor(p, a))).cloned().collect();
            if hit.is_empty() {
                return (0.0, Vec::new());
            }
            (hit.len() as f64 / mentioned.len() as f64, hit.into_iter().take(5).collect())
        }
        _ => (0.0, Vec::new()),
    }
}

/// The meaning signal: each similar note's cosine above the floor, normalized to the
/// best like the lexical signal.
fn meaning_signal(similar: &HashMap<String, f32>, floor: f32, visible: &dyn Fn(&str) -> bool, exclude: Option<&str>) -> HashMap<String, f64> {
    let floor = floor as f64;
    let kept: Vec<(&String, f64)> = similar
        .iter()
        .filter(|(id, _)| Some(id.as_str()) != exclude && visible(id))
        .map(|(id, s)| (id, (*s as f64 - floor).max(0.0)))
        .collect();
    let best = kept.iter().map(|(_, s)| *s).fold(0.0, f64::max);
    kept.into_iter().map(|(id, s)| (id.clone(), if best > 0.0 { s / best } else { 0.0 })).collect()
}

/// Graph proximity from weighted seeds: the best `weight · factor · w(k) · 0.5^(h−1)`,
/// shortest hop first (a farther hop can never beat a nearer one).
fn spread(
    net: &Net,
    seeds: &[(String, f64)],
    depth: u32,
    factor: f64,
    visible: &dyn Fn(&str) -> bool,
) -> HashMap<String, (f64, LinkKind, u32, String)> {
    let mut best: HashMap<String, (f64, LinkKind, u32, String)> = HashMap::new();
    for (seed, weight) in seeds {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(seed.clone());
        let mut frontier = vec![seed.clone()];
        for hop in 1..=depth {
            let mut layer: BTreeSet<String> = BTreeSet::new();
            for node in &frontier {
                for link in net.touching(node) {
                    let Some(other) = link.other(node) else { continue };
                    if seen.contains(other) || !net.nodes.contains_key(other) || !visible(other) {
                        continue;
                    }
                    let score = weight * factor * link.kind.weight() * 0.5f64.powi(hop as i32 - 1);
                    let replace = match best.get(other) {
                        Some((have, _, have_hop, have_via)) => {
                            score > *have + 1e-12 || ((score - *have).abs() <= 1e-12 && (hop, node) < (*have_hop, have_via))
                        }
                        None => true,
                    };
                    if replace {
                        best.insert(other.to_string(), (score, link.kind, hop, node.clone()));
                    }
                    layer.insert(other.to_string());
                }
            }
            seen.extend(layer.iter().cloned());
            frontier = layer.into_iter().collect();
        }
    }
    best
}

/// Run one association over one scope. `memories` holds every note of the scope
/// (history included); `net` indexes them with their links.
pub async fn run(
    conn: &mut SqliteConnection,
    project_id: &str,
    memories: &[Memory],
    net: &Net,
    request: &Request,
) -> MemResult<Vec<Associated>> {
    let depth = request.depth.clamp(1, DEPTH_MAX);
    let limit = request.limit.clamp(1, LIMIT_MAX);
    let include_history = request.include_history;
    let seed = &request.seed;
    let statuses = search::statuses(include_history);
    let visible_set: HashMap<&str, &Memory> = memories
        .iter()
        .filter(|m| include_history || m.status.is_live())
        .map(|m| (m.id.as_str(), m))
        .collect();
    let visible = |id: &str| visible_set.contains_key(id);
    let seed_id = match seed {
        Seed::Id(id) => Some(id.as_str()),
        Seed::Text(_) => None,
    };
    let seed_memory = seed_id.and_then(|id| memories.iter().find(|m| m.id == id));
    let text = match (seed, seed_memory) {
        (Seed::Text(text), _) => head(text, SEED_TEXT_MAX).to_string(),
        (Seed::Id(_), Some(m)) => format!("{}\n{}", m.title, head(&m.content, 1024)),
        (Seed::Id(_), None) => String::new(),
    };

    let (terms, texts) = text_signal(conn, project_id, &text, &visible_set, &statuses, seed_id).await?;
    let meaning = meaning_signal(&request.similar, request.floor, &visible, seed_id);
    let graph = match seed_memory {
        Some(m) => spread(net, &[(m.id.clone(), 1.0)], depth, 1.0, &visible),
        None => {
            let mut ranked: Vec<(&String, f64)> = texts.iter().map(|(id, hit)| (id, hit.t)).collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let seeds: Vec<(String, f64)> = ranked.into_iter().take(TEXT_SEEDS).map(|(id, t)| (id.clone(), t)).collect();
            spread(net, &seeds, depth, 0.5, &visible)
        }
    };
    let mentioned = match seed {
        Seed::Text(text) => mentioned_paths(head(text, SEED_TEXT_MAX)),
        Seed::Id(_) => Vec::new(),
    };

    let mut scored: Vec<(&str, &Memory, u32, Vec<String>)> = Vec::new();
    for (id, memory) in &visible_set {
        if Some(*id) == seed_id {
            continue;
        }
        let g = graph.get(*id).map(|(score, ..)| *score).unwrap_or(0.0);
        let mut t = texts.get(*id).map(|hit| hit.t).unwrap_or(0.0);
        if request.similar.contains_key(*id) {
            // Meaning is text similarity too: the stronger of the two counts.
            t = t.max(meaning.get(*id).copied().unwrap_or(0.0));
        }
        let (a, shared) = anchor_share(seed_memory, &mentioned, memory);
        let score = (100.0 * (W_GRAPH * g.min(1.0) + W_TEXT * t.min(1.0) + W_ANCHOR * a.min(1.0))).round() as u32;
        if score > 0 {
            scored.push((id, memory, score, shared));
        }
    }
    scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
    scored.truncate(limit);
    // Only the notes kept say why: the words a text match shares cost a lowercased copy
    // of the note, and a passage in CJK matches nearly every note of a project.
    Ok(scored
        .into_iter()
        .map(|(id, memory, score, shared)| {
            let mut reasons = Vec::new();
            if let Some((_, kind, hops, via)) = graph.get(id) {
                reasons.push(Reason::Link { kind: *kind, hops: *hops, via: via.clone() });
            }
            if let Some(hit) = texts.get(id) {
                let terms = hit_terms(&text, &terms, hit, memory);
                if !terms.is_empty() {
                    reasons.push(Reason::Text { terms });
                }
            }
            if let Some(similarity) = request.similar.get(id) {
                reasons.push(Reason::Similar { similarity: (similarity * 1000.0).round() / 1000.0 });
            }
            if !shared.is_empty() {
                reasons.push(Reason::Anchor { paths: shared });
            }
            Associated { id: id.to_string(), score, reasons }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salient_terms_drop_noise() {
        assert_eq!(salient("Use the BEGIN IMMEDIATE for a write"), vec!["begin", "immediate", "write"]);
        assert!(salient("a b c").is_empty());
    }

    #[test]
    fn passages_mention_paths() {
        let paths = mentioned_paths("see `crates/swiss-memory/src/store.rs`, and app.rs. Also v1.2 and words.");
        assert_eq!(paths, vec!["crates/swiss-memory/src/store.rs", "app.rs"]);
    }

    #[test]
    fn cjk_reasons_name_whole_phrases() {
        let matched: Vec<String> = ["缓存", "存失", "失效", "版本"].iter().map(|s| s.to_string()).collect();
        assert_eq!(shared_phrases("修复缓存失效，看版本号", &matched, "缓存失效靠版本号"), vec!["缓存失效", "版本"]);
        // Not held whole: the bigrams themselves, never a phrase the note lacks.
        assert_eq!(shared_phrases("缓存失效", &matched, "缓存 存失 失效"), vec!["缓存", "存失", "失效"]);
    }
}
