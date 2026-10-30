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

//! The retrieval fixtures (SPEC §memory.evaluation): each fixture loads into a scratch
//! repository through the service, the load is checked (every note, the agent notes
//! unconfirmed, replacements superseded, the links in the graph, anchored notes in
//! their briefs) and the queries are scored in every mode - lexical and hybrid search
//! (no embedder here: the two agree), the brief for the task, the brief for the task and
//! its paths - at recall@1/3/5 and MRR@5, with the rates of what the contexts carry that
//! they should not. The keyword, path, short and symbol styles carry floors on lexical
//! search, and the path style one on the brief for the task alone - a path is an
//! identifier, so the symbol tier brings its notes in without the path being handed over;
//! natural and cross are printed, not asserted - they are what the semantic tier is for.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use swiss_memory::eval::{self, EvalReport, Fixture, Mode};
use swiss_memory::readout::{self, Verdict};
use swiss_memory::service::{BriefRequest, GraphRequest};
use swiss_memory::{LinkKind, MemoryService, SearchQuery, Target};

fn fixture(name: &str) -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Fixture::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A scratch store and one Git working tree to load a fixture into.
fn scratch(label: &str) -> (PathBuf, MemoryService) {
    let dir = std::env::temp_dir().join(format!(
        "swiss-knowledge-{label}-{}-{}",
        std::process::id(),
        swiss_core::util::random_hex(6)
    ));
    std::fs::create_dir_all(dir.join("repo").join(".git")).expect("scratch repo");
    let dir = dir.canonicalize().expect("canonical scratch");
    let service = MemoryService::at(&dir.join("store.sqlite3"));
    (dir.join("repo"), service)
}

fn print(name: &str, report: &EvalReport) {
    println!("--- {name}\n{}", report.table());
    for miss in &report.misses {
        println!("miss [{}] {:?}: expected {:?}, got {:?}", miss.style, miss.query, miss.expected, miss.got);
    }
}

fn style(report: &EvalReport, style: &str) -> f64 {
    report.by_style.get(style).map(|m| m.recall5).unwrap_or_else(|| panic!("no {style} queries"))
}

/// Load, check the load, score in every mode: the shared body of both fixtures. The
/// lexical report first.
async fn load_and_score(name: &str) -> Vec<EvalReport> {
    let fixture = fixture(name);
    let (root, service) = scratch(name.trim_end_matches(".json"));
    let target = Target::dir(root.display().to_string());
    let loaded = eval::load(&service, &root, &fixture).await.unwrap_or_else(|e| panic!("{name}: {e}"));

    // Every note saved, each its own record.
    let distinct: HashSet<&String> = loaded.ids.values().collect();
    assert_eq!(loaded.ids.len(), fixture.notes.len(), "{name}: every note is saved");
    assert_eq!(distinct.len(), fixture.notes.len(), "{name}: no note collapsed into another (duplicate)");

    // Agent notes wait for a person; the others were typed by one.
    for note in &fixture.notes {
        let (record, _) = service.get(&target, &loaded.ids[&note.key], false).await.expect("get");
        let agent = note.origin.as_deref() == Some("agent");
        assert_eq!(record.summary.confirmed, !agent, "{name}: {} confirmed", note.key);
        // The map holds every note it can: an explicit domain, else its anchors' owner; only
        // a note with neither is unfiled.
        if let Some(domain) = &note.domain {
            assert_eq!(record.summary.domain.as_deref(), Some(domain.as_str()), "{name}: {} filed by hand", note.key);
        } else if !fixture.domains.is_empty() {
            assert_eq!(record.summary.domain.is_some(), !note.anchors.is_empty(), "{name}: {} filed by its anchors", note.key);
        }
    }

    // Replaced notes are superseded by their replacement.
    let left = eval::unsuperseded(&service, &root, &fixture, &loaded).await.expect("supersede check");
    assert!(left.is_empty(), "{name}: not superseded: {left:?}");

    // The project graph holds every link.
    let graph = service
        .graph(&target, &GraphRequest { include_history: true, limit: Some(swiss_memory::graph::GRAPH_LIMIT_MAX), ..GraphRequest::default() })
        .await
        .expect("graph");
    assert!(!graph.truncated, "{name}: the whole graph fits");
    for note in &fixture.notes {
        for link in &note.links {
            let (from, to) = (&loaded.ids[&note.key], &loaded.ids[&link.to]);
            let kind = LinkKind::parse(&link.kind).expect("checked kind");
            let held = graph.edges.iter().any(|e| {
                e.kind == kind && ((&e.from == from && &e.to == to) || (kind.is_symmetric() && &e.from == to && &e.to == from))
            });
            assert!(held, "{name}: link {} -{}-> {} missing", note.key, link.kind, link.to);
        }
    }

    // Briefs for a few anchors carry their notes (live ones: a replaced note is history).
    let replaced: HashSet<&str> = fixture.notes.iter().filter_map(|n| n.replaces.as_deref()).collect();
    let anchored: Vec<_> = fixture
        .notes
        .iter()
        .filter(|n| !replaced.contains(n.key.as_str()))
        .filter_map(|n| n.anchors.iter().find(|a| !a.contains(['*', '?', '['])).map(|a| (n, a)))
        .step_by(7)
        .take(4)
        .collect();
    assert!(!anchored.is_empty(), "{name}: some notes are anchored");
    for (note, anchor) in anchored {
        let brief = service
            .brief(&target, &BriefRequest { paths: vec![anchor.clone()], max_chars: Some(20_000), ..BriefRequest::default() })
            .await
            .expect("brief");
        assert!(brief.markdown.contains(&note.title), "{name}: brief for {anchor} lacks {}:\n{}", note.key, brief.markdown);
    }

    let mut reports = Vec::new();
    for mode in Mode::ALL {
        let report = eval::evaluate(&service, &root, &fixture, mode).await.expect("evaluate");
        print(name, &report);
        assert_eq!(report.semantic_queries, 0, "no embedder: no mode runs the semantic tier");
        assert_eq!(report.setup.state, "off");
        assert_eq!(report.overall.queries, fixture.queries.len(), "every mode scores every query");
        // The notes whose text cites one of their files carry it as evidence, so support
        // measures something: some context notes are verified, not all.
        assert!(report.overall.unsupported < 0.8, "{name} {}: unsupported {}", mode.as_str(), report.overall.unsupported);
        reports.push(report);
    }
    println!("--- {name}: the modes side by side\n{}", eval::compare(&reports));
    assert_eq!(reports[0].mode, Mode::Lexical);
    assert_eq!(reports[0].overall, reports[1].overall, "no embedder: hybrid answers what lexical does");

    // The retrieval readout is honest: a search it calls strong has an expected note first.
    let mut verdicts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for query in &fixture.queries {
        let wanted: Vec<&String> = query.expect.iter().map(|key| &loaded.ids[key]).collect();
        let request = SearchQuery { text: query.q.clone(), limit: eval::K, ..SearchQuery::default() };
        let hits = service.search(&target, &request).await.expect("search");
        let Some(said) = readout::judge(hits.iter().map(|h| (h.hold(), h.summary.domain.as_deref()))) else { continue };
        let first = wanted.contains(&&hits[0].summary.id);
        let tally = verdicts.entry(said.verdict.as_str()).or_default();
        tally.0 += 1;
        tally.1 += usize::from(first);
        assert!(first || said.verdict != Verdict::Strong, "{name}: {:?} read strong, its first hit unexpected", query.q);
    }
    println!("--- {name}: the readout (queries, first hit expected) {verdicts:?}");
    reports
}

#[tokio::test]
async fn fixture_swiss_history_english() {
    let reports = load_and_score("swiss-history.json").await;
    let report = &reports[0];
    assert!(style(report, "keyword") >= 0.8, "keyword recall@5 {}", style(report, "keyword"));
    assert!(style(report, "path") >= 0.9, "path recall@5 {}", style(report, "path"));
    assert!(style(report, "symbol") >= 0.9, "symbol recall@5 {}", style(report, "symbol"));
    let task = &reports[2];
    assert_eq!(task.mode, Mode::BriefTask);
    assert!(style(task, "path") >= 0.9, "brief-task path recall@5 {}", style(task, "path"));
}

#[tokio::test]
async fn fixture_cc_tutorial_chinese() {
    let reports = load_and_score("cc-tutorial.json").await;
    let report = &reports[0];
    assert!(style(report, "keyword") >= 0.8, "keyword recall@5 {}", style(report, "keyword"));
    assert!(style(report, "short") >= 0.75, "short recall@5 {}", style(report, "short"));
}
