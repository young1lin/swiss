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

//! The evaluation's own arithmetic (SPEC §memory.evaluation) on a fixture small enough
//! to judge by hand: every context the four modes build, the rates of what a context
//! carries that it should not (another domain, no checkable evidence, a stale note),
//! the setup each report records, and semantic ON against OFF with a deterministic
//! fake embedder.

use std::path::PathBuf;
use std::sync::Arc;

use swiss_memory::eval::{self, Fixture, Mode};
use swiss_memory::{Embedder, MemResult, MemoryService, Purpose};

/// Four notes about deploying, filed in two domains and none; one verified by code, one
/// drifted. Every note says "deploy", so the word weighs almost nothing: the first query's
/// context is the two notes that also say "script" (§memory.search's word floor).
const FIXTURE: &str = r#"{
    "about": "the evaluation's arithmetic",
    "project": {"name": "eval-arith", "language": "en"},
    "domains": [
        {"path": "build", "title": "Build and ship", "owns": ["scripts"]},
        {"path": "panel", "title": "The panel", "owns": ["panel"]}
    ],
    "notes": [
        {"key": "deploy", "kind": "decision", "title": "Deploy only through the script",
         "content": "The deploy script builds, stops the gateway and swaps the binary.",
         "anchors": ["scripts/deploy.ps1"], "evidence": ["verified_by=code:scripts/deploy.ps1"]},
        {"key": "button", "kind": "knowledge", "title": "Panel deploy button",
         "content": "The panel shows a deploy button that calls the deploy script.",
         "anchors": ["panel/deploy.ts"], "drift": true},
        {"key": "lock", "kind": "pitfall", "title": "Deploy lock",
         "content": "Two deploys at once corrupt the binary; the lock file serializes them.",
         "anchors": ["scripts/deploy-lock.ps1"]},
        {"key": "notes", "kind": "knowledge", "title": "Release notes",
         "content": "Every deploy writes release notes."}
    ],
    "queries": [
        {"q": "deploy script", "expect": ["deploy"], "style": "keyword", "paths": ["scripts/deploy.ps1"], "avoid": ["lock"]},
        {"q": "Two deploys at once", "expect": ["lock"], "style": "natural"}
    ]
}"#;

const DIMS: usize = 64;

/// Word stems hashed into buckets: meaning a test can predict.
struct Fake;

fn vector_of(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIMS];
    for word in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let stem: String = word.chars().take(4).collect();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in stem.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        v[(hash % DIMS as u64) as usize] += 1.0;
    }
    v
}

#[async_trait::async_trait]
impl Embedder for Fake {
    fn id(&self) -> &str {
        "fake:stems:64:ep-0a1b2c3d"
    }

    async fn embed(&self, texts: &[String], _purpose: Purpose) -> MemResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector_of(t)).collect())
    }
}

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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[tokio::test]
async fn eval_judges_every_context_by_hand() {
    let fixture = Fixture::parse(FIXTURE).expect("fixture");
    let (root, service) = scratch("eval-arith");
    eval::load(&service, &root, &fixture).await.expect("load");

    // Semantic OFF: no embedder, and the setup says so.
    let lexical = eval::evaluate(&service, &root, &fixture, Mode::Lexical).await.expect("lexical");
    println!("{}", lexical.table());
    assert_eq!(lexical.mode, Mode::Lexical);
    assert_eq!(lexical.setup.state, "off");
    assert_eq!(lexical.setup.fingerprint, None);
    assert_eq!(lexical.semantic_queries, 0);
    let m = &lexical.overall;
    assert_eq!(m.queries, 2);
    // "deploy script": the decision and the button, the two holding "script" - the lock and
    // the release notes share only "deploy", under the word floor. "Two deploys at once":
    // the lock alone (no stemming: "deploys" is not "deploy").
    assert!(close(m.recall5, 1.0), "recall@5 {}", m.recall5);
    assert_eq!(m.items, 3, "contexts of 2 and 1");
    // The expected domain of the first query is build: the button (panel) is the one wrong
    // note; the second query's one note is in its own domain.
    assert_eq!((m.wrong_domain_items, m.domain_judged), (1, 3));
    assert!(close(m.wrong_domain, 1.0 / 3.0), "wrong-domain {}", m.wrong_domain);
    // Only the deploy decision is verified by code.
    assert!(close(m.unsupported, 2.0 / 3.0), "unsupported {}", m.unsupported);
    // The button's anchor drifted after it was saved.
    assert!(close(m.stale, 1.0 / 3.0), "stale {}", m.stale);
    // The first context holds a stale, wrong-domain note; the second is clean.
    assert!(close(m.fully_correct, 0.5), "fully correct {}", m.fully_correct);
    assert_eq!(lexical.misses.len(), 0);

    // The brief for the task alone, then with the path: the anchored note comes first.
    let task = eval::evaluate(&service, &root, &fixture, Mode::BriefTask).await.expect("brief task");
    println!("{}", task.table());
    assert!(close(task.overall.recall5, 1.0), "brief(task) recall@5 {}", task.overall.recall5);
    let paths = eval::evaluate(&service, &root, &fixture, Mode::BriefPaths).await.expect("brief paths");
    println!("{}", paths.table());
    let keyword = &paths.by_style["keyword"];
    assert!(close(keyword.recall1, 1.0), "brief(task + paths): the anchored note first, recall@1 {}", keyword.recall1);
    assert!(close(keyword.mrr5, 1.0));
    // A query without paths runs as brief(task): both modes score every query.
    assert_eq!(paths.overall.queries, 2);

    // Semantic ON: the hybrid mode fuses meaning, the lexical one never does, and both
    // record the setup they ran under.
    service.set_embedder(Some(Arc::new(Fake)));
    let report = service.index_pending(64).await.expect("index");
    assert_eq!(report.pending, 0);
    let on = eval::evaluate(&service, &root, &fixture, Mode::Hybrid).await.expect("hybrid");
    let off = eval::evaluate(&service, &root, &fixture, Mode::Lexical).await.expect("lexical again");
    println!("{}", eval::compare(&[off.clone(), on.clone()]));
    assert_eq!(on.semantic_queries, 2, "semantic ON");
    assert_eq!(off.semantic_queries, 0, "semantic OFF even with an embedder set");
    for report in [&on, &off] {
        let setup = &report.setup;
        assert_eq!(setup.fingerprint.as_deref(), Some("fake:stems:64:ep-0a1b2c3d"));
        assert_eq!(setup.provider.as_deref(), Some("fake"));
        assert_eq!(setup.model.as_deref(), Some("stems"));
        assert_eq!(setup.endpoint.as_deref(), Some("ep-0a1b2c3d"));
        assert_eq!(setup.dims, Some(DIMS));
        assert_eq!(setup.state, "ready");
        assert_eq!((setup.indexed, setup.pending, setup.incompatible), (4, 0, 0));
    }
    assert_eq!(off.overall, lexical.overall, "semantic OFF answers what no embedder does");
}

/// Ids derive from the fixture's keys and the clock from the load's start plus each note's
/// index, so equal scores break the same way every load: two loads report alike. A second
/// load into the same store (another project) finds the ids taken and mints fresh ones.
#[tokio::test]
async fn two_loads_evaluate_alike() {
    let fixture = Fixture::parse(FIXTURE).expect("fixture");
    let mut runs = Vec::new();
    for label in ["eval-twice-a", "eval-twice-b"] {
        let (root, service) = scratch(label);
        let loaded = eval::load(&service, &root, &fixture).await.expect("load");
        let mut reports = Vec::new();
        for mode in [Mode::Lexical, Mode::BriefTask, Mode::BriefPaths] {
            let report = eval::evaluate(&service, &root, &fixture, mode).await.expect("evaluate");
            let got: Vec<Vec<String>> = report.misses.iter().map(|m| m.got.clone()).collect();
            reports.push((report.overall, got));
        }
        if label.ends_with('b') {
            let other = root.parent().expect("scratch dir").join("repo-2");
            std::fs::create_dir_all(other.join(".git")).expect("second repo");
            let again = eval::load(&service, &other, &fixture).await.expect("second load, same store");
            assert!(again.ids.values().all(|id| !loaded.ids.values().any(|first| first == id)), "taken ids are minted afresh");
        }
        runs.push((loaded.ids, reports));
    }
    assert_eq!(runs[0].0, runs[1].0, "the same ids");
    assert_eq!(runs[0].1, runs[1].1, "the same reports");
}

#[test]
fn eval_fixtures_check_the_new_fields() {
    assert!(Fixture::parse(FIXTURE).is_ok());
    let bad = FIXTURE.replace(r#""avoid": ["lock"]"#, r#""avoid": ["nope"]"#);
    assert!(Fixture::parse(&bad).unwrap_err().contains("avoids unknown key nope"));
    let bad = FIXTURE.replace(r#""paths": ["scripts/deploy.ps1"]"#, r#""paths": ["../out.ps1"]"#);
    assert!(Fixture::parse(&bad).unwrap_err().contains("inside the root"));
    let bad = FIXTURE.replace("verified_by=code:scripts/deploy.ps1", "verified_by=session:abc");
    assert!(Fixture::parse(&bad).unwrap_err().contains("deploy: evidence"));
    // A drifted note needs a file to drift.
    let bad = FIXTURE.replace(r#""anchors": ["panel/deploy.ts"], "drift": true"#, r#""drift": true"#);
    assert!(Fixture::parse(&bad).unwrap_err().contains("button: drift needs"));
}
