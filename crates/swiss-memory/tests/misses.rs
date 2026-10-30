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

//! Recorded misses (SPEC §memory.evaluation Misses): a retrieval that failed is recorded
//! against the notes it named, exported as a fixture, and that fixture - loaded into a
//! fresh store - scores the same failure.

use std::path::PathBuf;

use swiss_memory::eval::{self, Fixture, Mode};
use swiss_memory::misses::{MissRequest, MISSES_MAX};
use swiss_memory::{Actor, MemoryService, Target};

/// A charge retried after a timeout reuses its idempotency key, but no word of the question
/// an agent asks ("customer billed twice after timeout") is in that note: the support note
/// holding "customer", "timeout" and "twice" comes back instead. The refund rule was replaced.
const FIXTURE: &str = r#"{
    "about": "recorded misses, the source project",
    "project": {"name": "misses-source", "language": "en"},
    "domains": [
        {"path": "pay", "title": "Payments", "owns": ["src/pay"]},
        {"path": "support", "title": "Support", "owns": ["docs/support"]}
    ],
    "notes": [
        {"key": "key", "kind": "decision", "title": "Retries reuse the idempotency key",
         "content": "A retried charge sends the same idempotency key, so the processor answers with the first result.",
         "anchors": ["src/pay/retry.rs"], "domain": "pay", "evidence": ["verified_by=code:src/pay/retry.rs#retry"]},
        {"key": "support", "kind": "knowledge", "title": "Support handles timeout complaints",
         "content": "When a customer reports a timeout, support checks the status page twice a day.",
         "domain": "support", "links": [{"to": "key", "kind": "related_to"}]},
        {"key": "old-refund", "kind": "decision", "title": "Refunds go through the legacy gateway",
         "content": "Refund requests call the legacy gateway.", "domain": "pay"},
        {"key": "refund", "kind": "decision", "title": "Refunds go through the v2 gateway",
         "content": "Refund requests call the v2 gateway since the migration.", "domain": "pay", "replaces": "old-refund"},
        {"key": "webhook", "kind": "pitfall", "title": "Webhook events arrive out of order",
         "content": "Sort webhook events by their sequence number before applying them.", "origin": "agent"}
    ],
    "queries": [
        {"q": "idempotency key", "expect": ["key"], "style": "keyword"}
    ]
}"#;

fn scratch(label: &str) -> (PathBuf, MemoryService) {
    let dir = std::env::temp_dir().join(format!("swiss-knowledge-{label}-{}-{}", std::process::id(), swiss_core::util::random_hex(6)));
    std::fs::create_dir_all(dir.join("repo").join(".git")).expect("scratch repo");
    let dir = dir.canonicalize().expect("canonical scratch");
    let service = MemoryService::at(&dir.join("store.sqlite3"));
    (dir.join("repo"), service)
}

fn target(root: &std::path::Path) -> Target {
    Target::dir(root.display().to_string())
}

#[tokio::test]
async fn a_recorded_miss_exports_as_a_fixture_that_fails_alike() {
    let (root, service) = scratch("misses-source");
    let source = Fixture::parse(FIXTURE).expect("fixture");
    let loaded = eval::load(&service, &root, &source).await.expect("load");
    let id = |key: &str| loaded.ids[key].clone();
    let here = target(&root);
    let agent = Actor::mcp(Some("test"));

    // Another project in the same store, with a note of its own.
    let other_root = root.parent().expect("scratch dir").join("repo-2");
    std::fs::create_dir_all(other_root.join(".git")).expect("second repo");
    let other = eval::load(&service, &other_root, &Fixture::parse(&FIXTURE.replace("misses-source", "misses-other")).unwrap())
        .await
        .expect("second project");

    let billed = service
        .record_miss(
            &here,
            &agent,
            MissRequest {
                query: "customer billed twice after timeout".into(),
                via: Some("search".into()),
                shown: vec![id("support")],
                wrong: vec![id("support")],
                wanted: vec![id("key")],
                ..MissRequest::default()
            },
        )
        .await
        .expect("record");
    assert_eq!((billed.project.as_str(), billed.actor.as_str()), (loaded.project_id.as_str(), "mcp:test"));
    service
        .record_miss(
            &here,
            &agent,
            MissRequest {
                query: "refund gateway".into(),
                via: Some("brief".into()),
                style: Some("keyword".into()),
                wrong: vec![id("old-refund")],
                wanted: vec![id("refund")],
                paths: vec!["src/pay/refund.rs".into()],
                ..MissRequest::default()
            },
        )
        .await
        .expect("record a superseded note as wrong");
    service
        .record_miss(
            &here,
            &agent,
            MissRequest { query: "key rotation".into(), want: Some("how the signing key rotates".into()), ..MissRequest::default() },
        )
        .await
        .expect("record what no note holds");

    // A miss names this project's notes and says what was wanted, never both ways at once.
    let refused = [
        MissRequest { query: "x".into(), wanted: vec![other.ids["key"].clone()], ..MissRequest::default() },
        MissRequest { query: "x".into(), shown: vec![id("key")], ..MissRequest::default() },
        MissRequest { query: "x".into(), wanted: vec![id("key")], wrong: vec![id("key")], ..MissRequest::default() },
    ];
    for request in refused {
        assert!(service.record_miss(&here, &agent, request).await.is_err());
    }

    let listed = service.misses(&here).await.expect("list");
    let queries: Vec<&str> = listed.iter().map(|m| m.query.as_str()).collect();
    assert_eq!(queries, ["key rotation", "refund gateway", "customer billed twice after timeout"], "newest first");
    assert!(service.misses(&target(&other_root)).await.expect("other list").is_empty(), "misses stay in their project");

    let export = service.export_misses(&here).await.expect("export");
    assert_eq!(export.queries, 2);
    assert_eq!(export.gaps.len(), 1, "a miss no note holds is a note to write");
    assert_eq!(export.gaps[0].want.as_deref(), Some("how the signing key rotates"));
    let text = serde_json::to_string_pretty(&export.fixture).expect("serialize");
    let fixture = Fixture::parse(&text).expect("the export parses as a fixture");
    assert_eq!(fixture.project.name, "misses-source");
    assert_eq!(fixture.project.language, "en");
    let domains: Vec<&str> = fixture.domains.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(domains, ["pay", "support"]);
    let note = |key: &str| fixture.notes.iter().find(|n| n.key == id(key)).unwrap_or_else(|| panic!("{key} exported"));
    assert_eq!(fixture.notes.len(), 5, "the live notes and the superseded one a miss names");
    assert_eq!(note("refund").replaces, Some(id("old-refund")));
    assert_eq!(note("key").domain.as_deref(), Some("pay"));
    assert_eq!(note("key").anchors, ["src/pay/retry.rs"]);
    assert_eq!(note("key").evidence, ["verified_by=code:src/pay/retry.rs#retry"]);
    assert_eq!(note("webhook").origin.as_deref(), Some("agent"));
    assert_eq!(note("key").origin, None);
    let linked = |a: &str, b: &str| note(a).links.iter().any(|l| l.to == id(b) && l.kind == "related_to");
    assert!(linked("support", "key") || linked("key", "support"), "the link travels");

    let billed_query = &fixture.queries[0];
    assert_eq!(billed_query.q, "customer billed twice after timeout", "oldest first");
    assert_eq!((billed_query.expect.clone(), billed_query.avoid.clone()), (vec![id("key")], vec![id("support")]));
    assert_eq!(billed_query.style, "natural", "inferred");
    let refund_query = &fixture.queries[1];
    assert_eq!((refund_query.style.as_str(), refund_query.paths.clone()), ("keyword", vec!["src/pay/refund.rs".to_string()]));
    assert_eq!(refund_query.avoid, [id("old-refund")]);

    // Loaded into a fresh store, the fixture scores the same failure.
    let (fresh_root, fresh) = scratch("misses-replay");
    eval::load(&fresh, &fresh_root, &fixture).await.expect("load the export");
    let report = eval::evaluate(&fresh, &fresh_root, &fixture, Mode::Lexical).await.expect("evaluate");
    println!("{}", report.table());
    assert_eq!(report.overall.queries, 2);
    assert!(report.misses.iter().any(|m| m.query == "customer billed twice after timeout"), "the miss reproduces: {:?}", report.misses);

    // Dropping: one by id, an id the project lacks refused, then the rest.
    assert_eq!(service.drop_misses(&here, std::slice::from_ref(&billed.id)).await.expect("drop one"), 1);
    assert!(service.drop_misses(&here, std::slice::from_ref(&billed.id)).await.is_err(), "gone already");
    assert!(service.drop_misses(&here, &["m-0123456789".into()]).await.is_err(), "not a miss id");
    assert_eq!(service.drop_misses(&here, &[]).await.expect("drop the rest"), 2);
    assert!(service.misses(&here).await.expect("list").is_empty());
}

#[tokio::test]
async fn a_project_keeps_its_newest_misses() {
    let (root, service) = scratch("misses-cap");
    let source = Fixture::parse(FIXTURE).expect("fixture");
    let loaded = eval::load(&service, &root, &source).await.expect("load");
    let here = target(&root);
    for n in 0..=MISSES_MAX {
        let request = MissRequest { query: format!("query {n}"), wanted: vec![loaded.ids["key"].clone()], ..MissRequest::default() };
        service.record_miss(&here, &Actor::cli(), request).await.expect("record");
    }
    let listed = service.misses(&here).await.expect("list");
    assert_eq!(listed.len(), MISSES_MAX);
    assert_eq!(listed[0].query, format!("query {MISSES_MAX}"));
    assert_eq!(listed.last().map(|m| m.query.as_str()), Some("query 1"), "the oldest went");
}
