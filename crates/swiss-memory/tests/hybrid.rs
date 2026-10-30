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

//! The semantic tier end to end (SPEC §memory.embedding) with a deterministic fake
//! embedder: word stems (the first four letters) and CJK bigrams hashed into a
//! normalized vector - "meaning" a test can predict. Indexing, staleness on edit and
//! model change, the purge cascade, fusion of semantic-only hits, the brief's task
//! section, the related reason, the rule that a failing or slow provider only ever
//! costs the semantic tier, and the rule that one fingerprint holds one width: a
//! vector of another width is never scored, only re-embedded.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use swiss_memory::service::{BriefRequest, ListRequest, RelatedRequest, SaveRequest, UpdateRequest};
use swiss_memory::transfer::ExportScope;
use swiss_memory::{Actor, Embedder, Kind, MemError, MemResult, MemoryService, Purpose, Reason, SearchQuery, Seed, Target};

const DIMS: usize = 256;
const OK: u8 = 0;
const FAIL: u8 = 1;
const SLOW: u8 = 2;

/// Stems and bigrams into hashed buckets; `mode` makes it fail or stall on demand,
/// and `width` answers another width under the same fingerprint (the model behind a
/// name changed).
struct Fake {
    id: String,
    mode: Arc<AtomicU8>,
    width: Arc<AtomicUsize>,
}

impl Fake {
    fn new(id: &str) -> (Arc<Fake>, Arc<AtomicU8>) {
        let (fake, mode, _) = Fake::resizable(id);
        (fake, mode)
    }

    fn resizable(id: &str) -> (Arc<Fake>, Arc<AtomicU8>, Arc<AtomicUsize>) {
        let mode = Arc::new(AtomicU8::new(OK));
        let width = Arc::new(AtomicUsize::new(DIMS));
        (Arc::new(Fake { id: id.into(), mode: mode.clone(), width: width.clone() }), mode, width)
    }
}

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
        if word.chars().any(swiss_memory::search::is_cjk) {
            for gram in swiss_memory::search::cjk_units(word, 64) {
                v[bucket(&gram, dims)] += 1.0;
            }
        } else {
            let stem: String = word.chars().take(4).collect();
            v[bucket(&stem, dims)] += 1.0;
        }
    }
    v
}

#[async_trait::async_trait]
impl Embedder for Fake {
    fn id(&self) -> &str {
        &self.id
    }

    fn query_timeout(&self) -> Duration {
        Duration::from_millis(100)
    }

    fn batch(&self) -> usize {
        2
    }

    async fn embed(&self, texts: &[String], _purpose: Purpose) -> MemResult<Vec<Vec<f32>>> {
        let dims = self.width.load(Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            FAIL => Err(MemError::invalid("provider said 503")),
            SLOW => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(texts.iter().map(|t| vector_of(t, dims)).collect())
            }
            _ => Ok(texts.iter().map(|t| vector_of(t, dims)).collect()),
        }
    }
}

struct Scratch {
    dir: PathBuf,
    service: MemoryService,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "swiss-knowledge-{label}-{}-{}",
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

async fn save(s: &Scratch, title: &str, content: &str) -> String {
    s.service.save(&s.repo(), &Actor::panel(), note(title, content)).await.expect("save").record.summary.id
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
async fn hybrid_indexing_staleness_and_purge() {
    let s = Scratch::new("hybrid-index");
    let busy = save(&s, "Busy database writes", "SQLite answers busy under load; the gateway backs off.").await;
    let retry = save(&s, "Retry policy", "Retries back off exponentially.").await;
    save(&s, "缓存设计", "项目级缓存，按写入失效。").await;

    assert_eq!(s.service.embedding_status().await.expect("status").state, "off");
    assert!(s.service.index_pending(10).await.is_err(), "no embedder, nothing to index with");

    let (fake, _) = Fake::new("fake:stems:256");
    s.service.set_embedder(Some(fake));
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.state, status.indexed, status.pending), ("ready", 0, 3));

    // Batch 2 of 3: one run leaves one pending; the next finishes.
    let first = s.service.index_pending(2).await.expect("index");
    assert_eq!((first.embedded, first.pending, first.failed), (2, 1, 0));
    let second = s.service.index_pending(10).await.expect("index");
    assert_eq!((second.embedded, second.pending), (1, 0));
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.indexed, status.pending, status.dims), (3, 0, Some(DIMS)));
    assert!(status.last_run_ms.is_some());

    // An edit makes exactly that vector stale.
    let edit = UpdateRequest { expected_revision: 1, content: Some("Retries back off exponentially, at most five.".into()), ..UpdateRequest::default() };
    s.service.update(&s.repo(), &Actor::panel(), &retry, edit).await.expect("edit");
    assert_eq!(s.service.embedding_status().await.expect("status").pending, 1);
    assert_eq!(s.service.index_pending(10).await.expect("index").embedded, 1);

    // A purge takes its vector with it.
    assert_eq!(stored_vectors(&s).await, 3);
    s.service.purge(&s.repo(), &busy, &busy).await.expect("purge");
    assert_eq!(stored_vectors(&s).await, 2);

    // Another model: every vector is stale; a re-index drops the current model's.
    let (other, _) = Fake::new("fake:stems:other");
    s.service.set_embedder(Some(other));
    assert_eq!(s.service.embedding_status().await.expect("status").pending, 2);
    s.service.index_pending(10).await.expect("index");
    assert_eq!(stored_vectors(&s).await, 2, "one vector per note: the new model's replaced the old");
    assert_eq!(s.service.drop_vectors().await.expect("drop"), 2);
    assert_eq!(stored_vectors(&s).await, 0);
    assert_eq!(s.service.embedding_status().await.expect("status").indexed, 0);
}

#[tokio::test]
async fn hybrid_search_fuses_meaning_and_degrades_to_lexical() {
    let s = Scratch::new("hybrid-search");
    let busy = save(&s, "Busy database writes", "SQLite answers busy under load; the gateway backs off.").await;
    let retry = save(&s, "Retry policy", "Retries back off exponentially.").await;
    save(&s, "Release checklist", "Tag, build, publish the notes.").await;

    // Lexical alone: "retrying" is no token of any note.
    let lexical = s.service.hybrid_search(&s.repo(), &query("retrying")).await.expect("search");
    assert!(lexical.items.is_empty());
    assert_eq!(lexical.semantic.state, "off");

    let (fake, mode) = Fake::new("fake:stems:256");
    s.service.set_embedder(Some(fake));
    let pending = s.service.hybrid_search(&s.repo(), &query("busy")).await.expect("search");
    assert_eq!(pending.semantic.state, "unavailable", "nothing indexed yet");
    assert_eq!(pending.items.first().map(|h| h.summary.id.clone()), Some(busy.clone()));
    s.service.index_pending(10).await.expect("index");

    // Meaning only: the retry note, tier semantic, with its cosine.
    let found = s.service.hybrid_search(&s.repo(), &query("retrying")).await.expect("search");
    assert_eq!(found.semantic.state, "on");
    let first = found.items.first().expect("a semantic hit");
    assert_eq!((first.summary.id.as_str(), first.tier), (retry.as_str(), "semantic"));
    assert!(first.similarity.is_some_and(|s| s >= swiss_memory::embed::SIMILARITY_FLOOR));
    assert!(found.items.iter().all(|h| h.summary.title != "Release checklist"), "below the floor is no hit");

    // Both lists: the lexical hit keeps its tier, the semantic one joins it.
    let both = s.service.hybrid_search(&s.repo(), &query("busy retrying")).await.expect("search");
    let tiers: Vec<(&str, &str)> = both.items.iter().map(|h| (h.summary.id.as_str(), h.tier)).collect();
    assert!(tiers.contains(&(busy.as_str(), "lexical")), "{tiers:?}");
    assert!(tiers.contains(&(retry.as_str(), "semantic")), "{tiers:?}");
    // Exact tiers stay on top whatever the vectors say.
    let exact = s.service.hybrid_search(&s.repo(), &query("Retry policy")).await.expect("search");
    assert_eq!((exact.items[0].summary.id.as_str(), exact.items[0].tier), (retry.as_str(), "title"));

    // A failing provider costs the semantic tier only, and says why.
    mode.store(FAIL, Ordering::SeqCst);
    let failed = s.service.hybrid_search(&s.repo(), &query("gateway")).await.expect("search never fails on the embedder");
    assert_eq!(failed.semantic.state, "unavailable");
    assert!(failed.semantic.reason.as_deref().unwrap_or("").contains("503"), "{:?}", failed.semantic);
    assert_eq!(failed.items.first().map(|h| (h.summary.id.clone(), h.tier)), Some((busy.clone(), "lexical")));
    assert!(failed.items.iter().all(|h| h.similarity.is_none()));

    // So does a slow one, within its query timeout.
    mode.store(SLOW, Ordering::SeqCst);
    let slow = s.service.hybrid_search(&s.repo(), &query("gateway")).await.expect("search");
    assert_eq!(slow.semantic.state, "unavailable");
    assert!(slow.semantic.reason.as_deref().unwrap_or("").contains("did not answer"), "{:?}", slow.semantic);
    assert_eq!(slow.items.len(), 1);

    // An indexing run against a failing provider reports, never panics or errs.
    mode.store(FAIL, Ordering::SeqCst);
    save(&s, "Fresh note", "Written while the provider is down.").await;
    let report = s.service.index_pending(10).await.expect("a report, not an error");
    assert_eq!((report.embedded, report.failed, report.pending), (0, 1, 1));
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!(status.state, "error");
    assert!(status.last_error.as_deref().unwrap_or("").contains("503"));
    mode.store(OK, Ordering::SeqCst);
    s.service.index_pending(10).await.expect("index");
    assert_eq!(s.service.embedding_status().await.expect("status").state, "ready");
}

#[tokio::test]
async fn hybrid_brief_and_related_use_meaning() {
    let s = Scratch::new("hybrid-brief");
    let retry = save(&s, "Retry policy", "Retries back off exponentially.").await;
    let budget = save(&s, "Retry budget", "Retries stop after five attempts.").await;
    save(&s, "Release checklist", "Tag, build, publish the notes.").await;
    let (fake, _) = Fake::new("fake:stems:256");
    s.service.set_embedder(Some(fake));
    s.service.index_pending(10).await.expect("index");

    let brief = s
        .service
        .brief(&s.repo(), &BriefRequest { task: Some("retrying flaky uploads".into()), ..BriefRequest::default() })
        .await
        .expect("brief");
    assert_eq!(brief.semantic.as_ref().map(|st| st.state), Some("on"));
    assert!(brief.markdown.contains("Retry policy"), "{}", brief.markdown);

    let related = s
        .service
        .related(&s.repo(), &RelatedRequest { seed: Seed::Id(retry.clone()), depth: None, limit: None, include_history: false })
        .await
        .expect("related");
    let neighbour = related.items.iter().find(|r| r.summary.id == budget).expect("the budget note is related");
    assert!(neighbour.reasons.iter().any(|r| matches!(r, Reason::Similar { .. })), "{:?}", neighbour.reasons);
}

#[tokio::test]
async fn hybrid_one_fingerprint_holds_one_width() {
    let s = Scratch::new("hybrid-width");
    let busy = save(&s, "Busy database writes", "SQLite answers busy under load; the gateway backs off.").await;
    let retry = save(&s, "Retry policy", "Retries back off exponentially.").await;
    save(&s, "Release checklist", "Tag, build, publish the notes.").await;
    let (fake, _, width) = Fake::resizable("fake:stems:native");
    s.service.set_embedder(Some(fake));
    s.service.index_pending(10).await.expect("index");
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.state, status.dims, status.indexed, status.incompatible), ("ready", Some(DIMS), 3, 0));

    // The model behind the name changed: the provider answers 128 wide now. Nothing is
    // scored against the 256-wide index - not as 0, not at all - and the answer says why.
    width.store(128, Ordering::SeqCst);
    let found = s.service.hybrid_search(&s.repo(), &query("retrying")).await.expect("search");
    assert_eq!(found.semantic.state, "unavailable", "{:?}", found.semantic);
    let reason = found.semantic.reason.clone().unwrap_or_default();
    assert!(reason.contains("256") && reason.contains("128"), "{reason}");
    assert!(found.items.is_empty(), "no word hit, and no vector of the old width counts");
    let lexical = s.service.hybrid_search(&s.repo(), &query("busy")).await.expect("search");
    assert_eq!(lexical.items.first().map(|h| h.summary.id.clone()), Some(busy.clone()));
    assert!(lexical.items.iter().all(|h| h.similarity.is_none()), "a word hit gets no cosine across widths");
    let brief = s
        .service
        .brief(&s.repo(), &BriefRequest { task: Some("retrying flaky uploads".into()), ..BriefRequest::default() })
        .await
        .expect("brief");
    assert_eq!(brief.semantic.as_ref().map(|st| st.state), Some("unavailable"));

    // The index is stale at the width the provider answers now, and says so.
    let stale = s.service.embedding_status().await.expect("status");
    assert_eq!((stale.state, stale.dims, stale.incompatible), ("stale", Some(128), 3));
    assert_eq!((stale.indexed, stale.pending), (0, 3), "a vector of another width is no vector");

    // The next run drops them and embeds every note again, at one width.
    let report = s.service.index_pending(10).await.expect("index");
    assert_eq!((report.embedded, report.dropped, report.pending), (3, 3, 0));
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.state, status.dims, status.indexed, status.incompatible), ("ready", Some(128), 3, 0));
    let again = s.service.hybrid_search(&s.repo(), &query("retrying")).await.expect("search");
    assert_eq!(again.semantic.state, "on");
    assert_eq!(again.items.first().map(|h| h.summary.id.clone()), Some(retry.clone()));

    // A change met while indexing, before any query saw it: the run that first gets
    // the new width drops the old ones in the same write, and the rest follow.
    width.store(64, Ordering::SeqCst);
    save(&s, "Fresh note", "Written after the model changed.").await;
    let report = s.service.index_pending(1).await.expect("index");
    assert_eq!((report.embedded, report.dropped, report.pending), (1, 3, 3));
    assert_eq!(s.service.embedding_status().await.expect("status").incompatible, 0);
    let rest = s.service.index_pending(10).await.expect("index");
    assert_eq!((rest.embedded, rest.pending), (3, 0));
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.state, status.dims, status.indexed), ("ready", Some(64), 4));
}

#[tokio::test]
async fn hybrid_vectors_are_derived_dropping_every_one_loses_no_note() {
    let s = Scratch::new("hybrid-derived");
    save(&s, "Busy database writes", "SQLite answers busy under load; the gateway backs off.").await;
    save(&s, "Retry policy", "Retries back off exponentially.").await;
    save(&s, "缓存设计", "项目级缓存，按写入失效。").await;
    let (fake, _) = Fake::new("fake:stems:256");
    s.service.set_embedder(Some(fake));
    s.service.index_pending(10).await.expect("index");
    // The export after its header line (which carries the time it was taken).
    let body = |text: String| text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
    let export = body(s.service.export(&ExportScope::All).await.expect("export"));
    let listed = s.service.list(&s.repo(), &ListRequest::default()).await.expect("list").0.len();
    assert_eq!(stored_vectors(&s).await, 3);

    // Every vector of every model gone: no note, revision or link goes with them.
    {
        let store = s.service.store().await.expect("store");
        let mut conn = store.write().await.expect("write");
        let outcome = swiss_memory::store::drop_vectors(&mut conn, None).await;
        swiss_memory::store::finish(&mut conn, outcome).await.expect("drop");
    }
    assert_eq!(stored_vectors(&s).await, 0);
    assert!(export.contains("缓存设计"), "{export}");
    assert_eq!(body(s.service.export(&ExportScope::All).await.expect("export")), export, "the export holds no vector");
    assert_eq!(s.service.list(&s.repo(), &ListRequest::default()).await.expect("list").0.len(), listed);
    let words = s.service.hybrid_search(&s.repo(), &query("busy")).await.expect("search");
    assert_eq!(words.items.len(), 1);
    assert_eq!(words.semantic.state, "unavailable", "nothing indexed: the words answer alone");
    let status = s.service.embedding_status().await.expect("status");
    assert_eq!((status.indexed, status.pending, status.incompatible), (0, 3, 0));

    // Rebuilt from the notes alone.
    assert_eq!(s.service.index_pending(10).await.expect("index").embedded, 3);
    assert_eq!(s.service.hybrid_search(&s.repo(), &query("retrying")).await.expect("search").semantic.state, "on");
}

#[tokio::test]
async fn hybrid_an_exact_identifier_beats_words_and_meaning() {
    let s = Scratch::new("hybrid-symbol");
    let exact = save(&s, "Request budget", "Each request spends retry_budget before it fails over.").await;
    let loud = save(&s, "Under load", "Why the retry loop is exhausted under load: every retry waits.").await;
    save(&s, "Exhausted pools", "Under load the pool is exhausted, and why matters.").await;
    let (fake, _) = Fake::new("fake:stems:256");
    s.service.set_embedder(Some(fake));
    s.service.index_pending(10).await.expect("index");
    let asked = "why is retry_budget exhausted under load";

    // Without the identifier, the words and the meaning put the loud note first...
    let plain = s.service.hybrid_search(&s.repo(), &query("why is the retry exhausted under load")).await.expect("search");
    assert_eq!(plain.semantic.state, "on");
    assert_eq!(plain.items[0].summary.id, loud, "{:?}", plain.items);
    assert!(plain.items.iter().all(|h| h.tier != "symbol"));

    // ...naming it puts the note that holds it first: in hybrid search, by words alone, and
    // in the brief's task section.
    let named = s.service.hybrid_search(&s.repo(), &query(asked)).await.expect("search");
    assert_eq!(named.semantic.state, "on");
    assert_eq!(named.items[0].summary.id, exact, "{:?}", named.items);
    assert_eq!(named.items[0].tier, "symbol");
    // The rest are word hits under it; the two left hold the same words and tie, so their
    // order is the id tie-break's.
    let rest = &named.items[1..];
    assert!(rest.iter().any(|h| h.summary.id == loud) && rest.iter().all(|h| h.tier != "symbol"), "{:?}", named.items);
    let words = SearchQuery { words_only: true, ..query(asked) };
    let lexical = s.service.hybrid_search(&s.repo(), &words).await.expect("search");
    assert_eq!(lexical.semantic.state, "off", "words only: the semantic tier stays out");
    assert_eq!(lexical.items[0].summary.id, exact);
    let brief = s
        .service
        .brief(&s.repo(), &BriefRequest { task: Some(asked.into()), exclude_global: true, ..BriefRequest::default() })
        .await
        .expect("brief");
    assert_eq!(brief.semantic.as_ref().map(|m| m.state), Some("on"));
    assert_eq!(brief.shown.first(), Some(&exact), "{}", brief.markdown);
    assert!(brief.shown.contains(&loud));
}
