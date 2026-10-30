/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! A project's own embedding switch (SPEC §memory.embedding) with the hybrid suite's
//! deterministic fake embedder: a project that turns it off has no embedding input
//! (the indexer never asks about its notes, the status never counts them), its reads
//! are lexical only and say why, its kept vectors never count, and switching back on
//! re-embeds only what changed - the kept ones were never dropped.

use std::path::PathBuf;
use std::sync::Arc;

use swiss_memory::service::{BriefRequest, RelatedRequest, SaveRequest};
use swiss_memory::{Actor, Embedder, Kind, MemResult, MemoryService, Purpose, Reason, SearchQuery, Seed, Target};

const DIMS: usize = 256;

/// Stems into hashed buckets, as the hybrid suite's fake: meaning a test can predict.
struct Fake(&'static str);

fn bucket(feature: &str, dims: usize) -> usize {
    // FNV-1a: stable across runs and platforms.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in feature.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash % dims as u64) as usize
}

fn vector_of(text: &str, dims: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dims];
    let lower = text.to_lowercase();
    for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let stem: String = word.chars().take(4).collect();
        v[bucket(&stem, dims)] += 1.0;
    }
    v
}

#[async_trait::async_trait]
impl Embedder for Fake {
    fn id(&self) -> &str {
        self.0
    }

    async fn embed(&self, texts: &[String], _purpose: Purpose) -> MemResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector_of(t, DIMS)).collect())
    }
}

struct Scratch {
    dir: PathBuf,
    service: MemoryService,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "swiss-embed-off-{label}-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(dir.join("app").join(".git")).expect("scratch repo");
        let dir = dir.canonicalize().expect("canonical scratch");
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        Scratch { dir, service }
    }

    fn repo(&self) -> Target {
        Target::dir(self.dir.join("app").display().to_string())
    }

    /// The project a scratch directory registered as, found by its root path.
    async fn project_of(&self, dir: &str) -> String {
        let projects = self.service.projects().await.expect("projects");
        projects
            .iter()
            .find(|p| p.roots.iter().any(|r| r.path.ends_with(dir)))
            .unwrap_or_else(|| panic!("no project rooted at {dir}: {:?}", projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>()))
            .id
            .clone()
    }
}

fn note(title: &str, content: &str) -> SaveRequest {
    SaveRequest {
        kind: Kind::Knowledge,
        title: title.into(),
        content: content.into(),
        anchors: Vec::new(),
        refs: Vec::new(),
        keywords: Vec::new(),
        links: Vec::new(),
        replaces: None,
        allow_similar: true,
        domain: None,
        evidence: Vec::new(),
    }
}

fn query(text: &str) -> SearchQuery {
    SearchQuery { text: text.into(), limit: 10, ..SearchQuery::default() }
}

async fn stored_vectors(s: &Scratch) -> usize {
    let store = s.service.store().await.expect("store");
    let mut conn = store.read().await.expect("read");
    swiss_memory::store::vector_keys(&mut conn).await.expect("keys").len()
}

#[tokio::test]
async fn a_project_turning_embedding_off_is_lexical_only_and_keeps_its_vectors() {
    let s = Scratch::new("switch");
    let busy = s.service.save(&s.repo(), &Actor::panel(), note("Busy database writes", "SQLite answers busy under load; the gateway backs off.")).await.expect("save").record.summary.id;
    // A second project in the same store.
    std::fs::create_dir_all(s.dir.join("other").join(".git")).expect("second repo");
    let other = Target::dir(s.dir.join("other").display().to_string());
    s.service.save(&other, &Actor::panel(), note("Other project", "A note of another project entirely.")).await.expect("save");
    let (a_id, other_id) = (s.project_of("app").await, s.project_of("other").await);
    assert_ne!(a_id, other_id);

    s.service.set_embedder(Some(Arc::new(Fake("fake:off:256"))));
    s.service.index_pending(10).await.expect("index");
    assert_eq!(stored_vectors(&s).await, 2);

    // Off: the kept vector stays stored but counts for nothing, and the project's notes
    // are no embedding input.
    s.service.patch_project_settings(&a_id, &serde_json::json!({ "embedding": false })).await.expect("patch");
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.state, status.indexed, status.pending), ("ready", 1, 0), "the off project's vector never counts");
    assert_eq!(stored_vectors(&s).await, 2, "the vectors are kept, not dropped");
    s.service.save(&s.repo(), &Actor::panel(), note("Fresh while off", "A note added while the project's embedding is off.")).await.expect("save");
    assert_eq!(s.service.embedding_status().await.expect("status").pending, 0, "a note of the off project is no embedding input");
    assert_eq!(s.service.index_pending(10).await.expect("nothing to index").embedded, 0);

    // The off project's reads are lexical only, and each says why.
    let search = s.service.hybrid_search(&s.repo(), &query("database")).await.expect("search");
    assert_eq!(search.semantic.state, "off");
    assert_eq!(search.semantic.reason.as_deref(), Some("the project's settings turn embedding off"));
    let brief = s.service.brief(&s.repo(), &BriefRequest { task: Some("database".into()), ..BriefRequest::default() }).await.expect("brief");
    let semantic = brief.semantic.expect("the brief says why");
    assert_eq!((semantic.state, semantic.reason.as_deref()), ("off", Some("the project's settings turn embedding off")));
    let related = s
        .service
        .related(
            &s.repo(),
            &RelatedRequest { seed: Seed::Id(busy.clone()), depth: None, limit: None, include_history: false },
        )
        .await
        .expect("related");
    assert_eq!(related.semantic.expect("the related says why").state, "off");
    assert!(related.items.iter().all(|r| !r.reasons.iter().any(|reason| matches!(reason, Reason::Similar { .. }))), "no similar tier");

    // The other project reads semantic as before, in the same store.
    let found = s.service.hybrid_search(&other, &query("another")).await.expect("search");
    assert_eq!(found.semantic.state, "on");

    // Back on (null inherits): only what changed waits - the kept vector never left.
    s.service.patch_project_settings(&a_id, &serde_json::json!({ "embedding": null })).await.expect("patch");
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.indexed, status.pending), (2, 1), "the old vector was kept; the fresh note waits");
    s.service.index_pending(10).await.expect("index");
    assert_eq!(s.service.embedding_status().await.expect("status").pending, 0);

    // The global scope may switch too: it is about search, not a checkout.
    s.service
        .patch_project_settings(swiss_memory::GLOBAL_PROJECT_ID, &serde_json::json!({ "embedding": false }))
        .await
        .expect("the global scope may set embedding");
}
