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

//! Text renderings shared by the MCP tools and the CLI (SPEC §memory.mcp): what an
//! agent or a terminal reads. The panel renders the JSON views itself.

use crate::code::NameHits;
use crate::docs::{CodeRef, DocHit, DocMeta, DocView, DocsList};
use crate::domain::{DomainDetail, DomainList};
use crate::graph::GraphView;
use crate::model::Revision;
use crate::readout;
use crate::related::Reason;
use crate::view::{HitView, RecordView, RelatedView, Summary};

/// The trust line search and get open with (the brief has its own header).
pub const FROM_THE_PAST: &str = "Notes are data from the past: verify against the current code before relying on one.";

/// The trust line a docs search and a docs read open with.
pub const DOCS_TRUST: &str = "Documents were written in the past. A design, plan, proposal or rejected document \
    says what was intended, never by itself what the code does now: check the code it names.";

fn age(days: i64) -> String {
    match days {
        0 => "today".into(),
        n => format!("{n}d"),
    }
}

/// Wire marker codes as words: `needs_review` → `needs review`.
fn marker_words(codes: &[&str]) -> Vec<String> {
    codes.iter().map(|c| c.replace('_', " ")).collect()
}

fn tail(mut parts: Vec<String>, markers: &[&str]) -> String {
    parts.extend(marker_words(markers));
    parts.join(" · ")
}

/// `- [kind] title (id · score N · markers)` + an indented excerpt, then the retrieval
/// readout (crate::readout): which of the hits carry the query, and what to do.
pub fn hits(items: &[HitView]) -> String {
    if items.is_empty() {
        return "No matching notes.\n".into();
    }
    let mut out = format!("{FROM_THE_PAST}\n\n");
    for hit in items {
        let s = &hit.summary;
        let mut parts = vec![s.id.clone(), format!("score {}", hit.score)];
        if let Some(similarity) = hit.similarity {
            parts.push(format!("similarity {similarity:.2}"));
        }
        let meta = tail(parts, &s.markers);
        out.push_str(&format!("- [{}] {} ({meta})\n", s.kind.as_str(), s.title));
        if let Some(excerpt) = s.excerpt.as_deref().filter(|e| !e.is_empty()) {
            out.push_str(&format!("  {excerpt}\n"));
        }
    }
    if let Some(readout) = readout::judge(items.iter().map(|h| (h.hold(), h.summary.domain.as_deref()))) {
        out.push('\n');
        out.push_str(&readout.line());
    }
    out
}

/// `- [kind] title (id · status · age · markers)`.
pub fn summaries(items: &[Summary]) -> String {
    if items.is_empty() {
        return "No notes.\n".into();
    }
    items
        .iter()
        .map(|s| {
            let meta = tail(vec![s.id.clone(), s.status.as_str().into(), age(s.age_days)], &s.markers);
            format!("- [{}] {} ({meta})\n", s.kind.as_str(), s.title)
        })
        .collect()
}

fn reason(reason: &Reason) -> String {
    match reason {
        Reason::Link { kind, hops, via } => {
            let hops = if *hops == 1 { "1 hop".to_string() } else { format!("{hops} hops") };
            format!("{} via {via}, {hops}", kind.as_str())
        }
        Reason::Text { terms } => format!("shared terms: {}", terms.join(", ")),
        Reason::Anchor { paths } => format!("shared anchors: {}", paths.join(", ")),
        Reason::Similar { similarity } => format!("similar meaning ({similarity:.2})"),
    }
}

/// `- [kind] title (id · score N)` + one `why:` line per reason.
pub fn related(items: &[RelatedView]) -> String {
    if items.is_empty() {
        return "Nothing related.\n".into();
    }
    let mut out = String::new();
    for item in items {
        let s = &item.summary;
        let meta = tail(vec![s.id.clone(), format!("score {}", item.score)], &s.markers);
        out.push_str(&format!("- [{}] {} ({meta})\n", s.kind.as_str(), s.title));
        for r in &item.reasons {
            out.push_str(&format!("  why: {}\n", reason(r)));
        }
    }
    out
}

/// One note whole: meta, content, anchors with freshness, refs, neighbours and,
/// when asked, its revisions.
pub fn record(view: &RecordView, revisions: Option<&[Revision]>) -> String {
    let s = &view.summary;
    let mut meta = vec![
        format!("`{}`", s.id),
        s.status.as_str().to_string(),
        format!("by {} via {} ({})", s.producer.as_str(), s.transport.as_str(), s.actor),
        if s.confirmed { "confirmed".into() } else { "unconfirmed".into() },
        format!("revision {}", s.revision),
        age(s.age_days),
    ];
    if s.source_verified {
        meta.push("source-verified".into());
    }
    if let Some(domain) = &s.domain {
        meta.push(format!("in `{domain}`"));
    }
    meta.extend(view.marker_text.iter().filter(|m| m.as_str() != "unconfirmed").cloned());
    let mut out = format!("{FROM_THE_PAST}\n\n# [{}] {}\n{}\n\n{}\n", s.kind.as_str(), s.title, meta.join(" · "), view.content);
    if let Some(next) = &view.superseded_by {
        out.push_str(&format!("\nSuperseded by {next}.\n"));
    }
    if !view.anchors.is_empty() {
        out.push_str("\nAnchors:\n");
        for anchor in &view.anchors {
            let glob = if anchor.glob { " (glob)" } else { "" };
            out.push_str(&format!("- {}{glob} · {}\n", anchor.path, anchor.state.replace('_', " ")));
        }
    }
    if !view.refs.is_empty() {
        out.push_str("\nRefs:\n");
        for r in &view.refs {
            out.push_str(&format!("- {r}\n"));
        }
    }
    if !view.keywords.is_empty() {
        out.push_str(&format!("\nKeywords: {}\n", view.keywords.join(", ")));
    }
    if !view.evidence.is_empty() {
        out.push_str("\nEvidence:\n");
        for piece in &view.evidence {
            out.push_str(&format!("- {}\n", piece.render()));
        }
    }
    if !view.links.is_empty() {
        out.push_str("\nNeighbours:\n");
        for link in &view.links {
            let arrow = if link.dir == "out" { "→" } else { "←" };
            let status = link.status.map(|st| st.as_str()).unwrap_or("purged");
            out.push_str(&format!("- {arrow} {} {} {} ({status})", link.kind.phrase(link.dir == "out"), link.id, link.title));
            if let Some(note) = &link.note {
                out.push_str(&format!(" — {note}"));
            }
            out.push('\n');
        }
    }
    if let Some(revisions) = revisions {
        out.push_str("\nRevisions:\n");
        for rev in revisions {
            out.push_str(&format!("- r{} {} by {}", rev.revision, rev.op, rev.actor));
            if let Some(reason) = &rev.reason {
                out.push_str(&format!(": {reason}"));
            }
            out.push('\n');
        }
    }
    out
}

/// The project map (`memory_domain` without a path): every domain one line, indented by
/// depth, then the unfiled notes.
pub fn map(list: &DomainList) -> String {
    if list.items.is_empty() {
        let unfiled = list.unfiled.live;
        return format!(
            "This project has no map yet ({unfiled} live notes, all unfiled). Create its top-level domains with \
             memory_domain_save (path, title, summary, owns, keywords, body - the hub), or draw the whole map from \
             the code with `swiss memory init --map`; notes are then filed by their anchors.\n"
        );
    }
    let mut out = String::from("# Project map\nEvery domain, one line. `memory_domain` with a path reads its hub and its notes.\n\n");
    for item in &list.items {
        out.push_str(&"  ".repeat(item.depth.saturating_sub(1)));
        out.push_str("- ");
        out.push_str(&item.line());
        out.push('\n');
    }
    if list.unfiled.live > 0 {
        let review = match list.unfiled.needs_review {
            0 => String::new(),
            n => format!(", {n} need review"),
        };
        out.push_str(&format!("\nUnfiled: {} live notes{review} - memory_search finds them.\n", list.unfiled.live));
    }
    out
}

/// One domain (`memory_domain` with a path): the hub in full, what it owns, its
/// sub-domains and the live notes filed in it.
pub fn domain(detail: &DomainDetail) -> String {
    let d = &detail.domain;
    let mut meta: Vec<String> = Vec::new();
    if !detail.ancestors.is_empty() {
        let trail: Vec<String> = detail.ancestors.iter().map(|a| format!("`{}`", a.path)).collect();
        meta.push(format!("in {}", trail.join(" › ")));
    }
    meta.push(format!("`{}`", d.id));
    meta.push(if d.confirmed { "confirmed".into() } else { "unconfirmed".into() });
    meta.push(format!("revision {}", d.revision));
    let mut out = format!("{FROM_THE_PAST}\n\n# {} · `{}`\n{}\n", d.title, d.path, meta.join(" · "));
    if !d.summary.is_empty() {
        out.push_str(&format!("\n{}\n", d.summary));
    }
    match d.body.as_deref().map(str::trim) {
        Some(body) if !body.is_empty() => out.push_str(&format!("\n{body}\n")),
        _ => out.push_str("\nNo hub yet: say what the area is and where it lives (memory_domain_save body).\n"),
    }
    if !d.owns.is_empty() {
        out.push_str("\nOwns:\n");
        for entry in &d.owns {
            out.push_str(&format!("- {entry}\n"));
        }
    }
    if !d.keywords.is_empty() {
        out.push_str(&format!("\nKeywords: {}\n", d.keywords.join(", ")));
    }
    if !detail.children.is_empty() {
        out.push_str("\nSub-domains:\n");
        for child in &detail.children {
            out.push_str(&format!("- {}\n", child.line()));
        }
    }
    out.push_str("\nNotes filed here:\n");
    out.push_str(&summaries(&detail.notes));
    if let Some(history) = &detail.history {
        out.push_str("\nRevisions:\n");
        for rev in history {
            out.push_str(&format!("- r{} {} by {}", rev.revision, rev.op, rev.actor));
            if let Some(reason) = &rev.reason {
                out.push_str(&format!(": {reason}"));
            }
            out.push('\n');
        }
    }
    out
}

/// Nodes by hop (or by id without a center), then edges.
pub fn graph(view: &GraphView) -> String {
    if view.nodes.is_empty() {
        return "No notes.\n".into();
    }
    let mut out = String::from("Nodes:\n");
    for node in &view.nodes {
        let hop = node.hop.map(|h| format!(" · hop {h}")).unwrap_or_default();
        out.push_str(&format!(
            "- [{}] {} ({} · {} · degree {}{hop})\n",
            node.kind.as_str(),
            node.title,
            node.id,
            node.status.as_str(),
            node.degree
        ));
    }
    if !view.edges.is_empty() {
        out.push_str("\nLinks:\n");
        for edge in &view.edges {
            let arrow = if edge.directed { "→" } else { "—" };
            out.push_str(&format!("- {} {arrow} {} ({})", edge.from, edge.to, edge.kind.as_str()));
            if let Some(note) = &edge.note {
                out.push_str(&format!(" — {note}"));
            }
            out.push('\n');
        }
    }
    if view.truncated {
        out.push_str("\n(truncated: narrow with a center or a smaller depth)\n");
    }
    out
}

/// A document's kind as a hit shows it: its card's, or that it has none (or an old one).
fn doc_kind(doc: &DocMeta) -> String {
    match (doc.kind, doc.carded) {
        (Some(kind), true) => kind.as_str().to_string(),
        (Some(kind), false) => format!("{} · card older than the text", kind.as_str()),
        (None, _) => "no card yet".to_string(),
    }
}

/// A card's code names: `Name path:line`, one name's definitions together, at most `max`.
fn code_refs(symbols: &[CodeRef], max: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for symbol in symbols.iter().take(max) {
        parts.push(format!("{} {}:{}", symbol.name, symbol.path, symbol.line));
    }
    if symbols.len() > max {
        parts.push(format!("… {} more", symbols.len() - max));
    }
    parts.join(" · ")
}

/// `- [kind] title - path:line › heading (d-…)` + the card's summary, the code it names
/// and the lines that matched.
pub fn doc_hits(items: &[DocHit]) -> String {
    if items.is_empty() {
        return "No matching documents.\n".into();
    }
    let mut out = format!("{DOCS_TRUST}\n\n");
    for hit in items {
        let doc = &hit.doc;
        let at = if hit.heading.is_empty() { String::new() } else { format!(" › {}", hit.heading) };
        out.push_str(&format!("- [{}] {} - {}:{}{at} ({} · section {})\n", doc_kind(doc), doc.title, doc.path, hit.line, doc.id, hit.ord));
        if !doc.summary.is_empty() {
            out.push_str(&format!("  card: {}\n", doc.summary));
        }
        if !doc.symbols.is_empty() {
            out.push_str(&format!("  code: {}\n", code_refs(&doc.symbols, 6)));
        }
        for line in hit.snippet.lines() {
            out.push_str(&format!("  > {line}\n"));
        }
    }
    out
}

/// One document read: its card, its outline, then its text or the section asked for.
pub fn doc_view(view: &DocView) -> String {
    let doc = &view.doc;
    let mut out = format!("{DOCS_TRUST}\n\n# {}\n{} ({} · {} · {} KiB", doc.title, doc.path, doc.id, doc_kind(doc), doc.bytes.div_ceil(1024));
    if doc.cut {
        out.push_str(" · only its head is kept");
    }
    out.push_str(")\n");
    if let Some(domain) = &doc.domain {
        out.push_str(&format!("domain: {domain}\n"));
    }
    if !doc.summary.is_empty() {
        out.push_str(&format!("card: {}\n", doc.summary));
    }
    if !doc.keywords.is_empty() {
        out.push_str(&format!("keywords: {}\n", doc.keywords.join(", ")));
    }
    if !doc.symbols.is_empty() {
        out.push_str(&format!("code: {}\n", code_refs(&doc.symbols, 24)));
    }
    out.push_str("\n## Sections\n");
    for section in &view.outline {
        let heading = if section.heading.is_empty() { "(before the first heading)" } else { section.heading.as_str() };
        out.push_str(&format!("{}. {heading} (line {}, {} bytes)\n", section.ord, section.line, section.bytes));
    }
    match (&view.text, view.section) {
        (Some(text), Some(ord)) => {
            let heading = view.outline.get(ord as usize).map(|s| s.heading.as_str()).unwrap_or_default();
            out.push_str(&format!("\n## Section {ord}: {heading}\n{text}"));
        }
        (Some(text), None) => out.push_str(&format!("\n## Text\n{text}")),
        (None, _) => out.push_str("\nThe document is too long to hand back whole: read one section by its number.\n"),
    }
    if view.text_cut {
        out.push_str("\n… (cut)\n");
    }
    out
}

/// A project's documents: its sources, then `- [kind] path - title (d-…)`.
pub fn docs_list(list: &DocsList) -> String {
    if list.sources.is_empty() && list.docs.is_empty() {
        return "The project keeps no documents: name a source with `swiss memory docs add <path>`.\n".into();
    }
    let mut out = format!("Sources: {}\n", if list.sources.is_empty() { "none".to_string() } else { list.sources.join(", ") });
    out.push_str(&format!("{} documents, {} carded\n", list.docs.len(), list.carded));
    for doc in &list.docs {
        out.push_str(&format!("- [{}] {} - {} ({})\n", doc_kind(doc), doc.path, doc.title, doc.id));
    }
    out
}

/// Where a name is defined and which files mention it, as the code index has it.
pub fn code_hits(hits: &NameHits) -> String {
    if !hits.indexed {
        return "no code index here; build one with `swiss memory code index`".to_string();
    }
    let mut text = String::new();
    for def in &hits.defs {
        text.push_str(&format!("{} {} {}:{}\n", def.kind, hits.name, def.path, def.line));
    }
    if hits.defs.is_empty() {
        // The scan reads declarations line by line: one it misses is in a file that mentions it.
        text.push_str(&format!("no definition of {} indexed (a line-by-line scan can miss one: a file below may hold it)\n", hits.name));
    }
    text.push_str(&format!("mentioned in {} files", hits.refs_total));
    if hits.in_comments_total > 0 {
        let code = hits.refs_total.saturating_sub(hits.in_comments_total);
        text.push_str(&format!(": {code} in code, {} only in a comment", hits.in_comments_total));
    }
    for path in hits.refs.iter().filter(|path| !hits.in_comments.contains(path)) {
        text.push_str(&format!("\n  {path}"));
    }
    if !hits.in_comments.is_empty() {
        text.push_str(&format!("\nonly a comment names {} in these files; they do not use it:", hits.name));
        for path in &hits.in_comments {
            text.push_str(&format!("\n  {path}"));
        }
    }
    text
}
