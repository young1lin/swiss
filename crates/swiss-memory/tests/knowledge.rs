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

//! The memory core's acceptance tests (SPEC §memory.evaluation): the capture, graph,
//! association, freshness, scope, brief, search, project, transfer and concurrency
//! suites. The MCP and API surfaces are tested in the root crate beside their
//! adapters.
//!
//! Each test owns a scratch directory holding its repositories and its store.
//! Nothing touches the user's home.

use std::path::{Path, PathBuf};

use swiss_memory::service::{BriefRequest, GraphRequest, InitMode, LinkSpec, ListRequest, RelatedRequest, SaveRequest, UpdateRequest};
use swiss_memory::{
    Actor, Code, ConflictKind, DomainRequest, DomainUpdate, EvidenceSpec, ExportScope, ImportOptions, ImportStrategy, Kind, LinkKind, MemError,
    MemoryService, Producer,
    Relation, SearchQuery, Seed, SourceType, Status, Target, Transport, Verdict,
};
use swiss_memory::recheck::{Judged, Judgement, Linked, Reach, Rechecked};
use swiss_memory::render;
use swiss_memory::sync::RecheckScope;

/// One scratch: a directory with a store and any number of repositories.
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
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let dir = dir.canonicalize().expect("canonical scratch");
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        Scratch { dir, service }
    }

    /// A second service over the same store: another process sharing the home.
    fn twin(&self) -> MemoryService {
        MemoryService::at(&self.dir.join("store.sqlite3"))
    }

    /// A Git working tree (a `.git` directory) named `name`, optionally with an origin.
    fn repo(&self, name: &str, origin: Option<&str>) -> PathBuf {
        let root = self.dir.join(name);
        std::fs::create_dir_all(root.join(".git")).expect("repo");
        if let Some(url) = origin {
            std::fs::write(root.join(".git").join("config"), format!("[remote \"origin\"]\n\turl = {url}\n")).expect("config");
        }
        root
    }

    fn plain(&self, name: &str) -> PathBuf {
        let root = self.dir.join(name);
        std::fs::create_dir_all(&root).expect("plain dir");
        root
    }
}

fn at(dir: &Path) -> Target {
    Target::dir(dir.display().to_string())
}

fn note(kind: Kind, title: &str, content: &str) -> SaveRequest {
    SaveRequest {
        kind,
        title: title.into(),
        content: content.into(),
        anchors: Vec::new(),
        refs: Vec::new(),
        keywords: Vec::new(),
        links: Vec::new(),
        replaces: None,
        allow_similar: false,
        domain: None,
        evidence: Vec::new(),
    }
}

async fn save(service: &MemoryService, target: &Target, title: &str, content: &str) -> String {
    service
        .save(target, &Actor::cli(), note(Kind::Decision, title, content))
        .await
        .expect("save")
        .record
        .summary
        .id
}

fn code(err: MemError) -> Code {
    err.code
}

// --- capture --------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_capture_same_save_twice_is_one_record() {
    let s = Scratch::new("dup");
    let repo = at(&s.repo("app", None));
    let first = s.service.save(&repo, &Actor::cli(), note(Kind::Decision, "Use WAL", "WAL keeps readers apart.")).await.unwrap();
    assert!(first.project.registered);
    assert!(!first.duplicate);
    let second = s.service.save(&repo, &Actor::cli(), note(Kind::Decision, "use  wal", "WAL keeps readers apart.\n")).await.unwrap();
    assert!(second.duplicate);
    assert!(!second.project.registered);
    assert_eq!(first.record.summary.id, second.record.summary.id);
    let (items, _) = s.service.list(&repo, &ListRequest::default()).await.unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn knowledge_capture_same_title_is_similar_exists() {
    let s = Scratch::new("similar");
    let repo = at(&s.repo("app", None));
    let id = save(&s.service, &repo, "Cache TTL", "Five minutes.").await;
    let err = s.service.save(&repo, &Actor::cli(), note(Kind::Decision, "cache ttl", "Ten minutes.")).await.unwrap_err();
    assert_eq!(err.code, Code::SimilarExists);
    assert_eq!(err.extra["similar"]["id"], id.as_str());
    let mut forced = note(Kind::Decision, "cache ttl", "Ten minutes.");
    forced.allow_similar = true;
    s.service.save(&repo, &Actor::cli(), forced).await.unwrap();
}

/// A save's similar hints share the saved title's subject, never a stray token. A CJK
/// title cannot fall back to its Latin tokens: FTS cannot segment CJK, so
/// `AI 客服预设问题` would otherwise hint every note saying `ai` - the note goes through
/// the CJK tier instead, with a floor a stray shared unit never clears.
#[tokio::test]
async fn knowledge_capture_similar_hints_share_the_subject() {
    let s = Scratch::new("hints");
    let repo = at(&s.repo("app", None));
    // A diverse corpus: nothing about 客服预设, two notes saying AI.
    save(&s.service, &repo, "H2 内存库", "The AI keeps the H2 heap off-heap.").await;
    save(&s.service, &repo, "BuyerPositionMergeRes", "The AI joins follower batches.").await;
    save(&s.service, &repo, "Retry budget", "Retries spend retry_budget before failing over.").await;
    let stray = s
        .service
        .save(&repo, &Actor::cli(), note(Kind::Knowledge, "AI 客服预设问题", "客服上线前先配好预设问题。"))
        .await
        .unwrap();
    let titles: Vec<&str> = stray.similar.iter().map(|s| s.title.as_str()).collect();
    assert!(titles.is_empty(), "a stray shared token is no hint: {titles:?}");
    // A note on the same subject is the hint, CJK title to CJK title.
    save(&s.service, &repo, "客服预设问题带版本", "预设问题带版本号，回滚靠版本。").await;
    let on_subject = s
        .service
        .save(&repo, &Actor::cli(), note(Kind::Knowledge, "客服预设的回滚", "回滚预设问题依赖版本号。"))
        .await
        .unwrap();
    let titles: Vec<&str> = on_subject.similar.iter().map(|s| s.title.as_str()).collect();
    assert!(
        titles.contains(&"AI 客服预设问题") || titles.contains(&"客服预设问题带版本"),
        "the same subject is hinted: {titles:?}"
    );
    assert!(!titles.iter().any(|t| *t == "H2 内存库" || *t == "BuyerPositionMergeRes"), "{titles:?}");
    // A title without CJK keeps the word tier: its subject is still hinted.
    save(&s.service, &repo, "Cache TTL rounds down", "A partial window keeps the old value.").await;
    save(&s.service, &repo, "Cache TTL is five minutes", "Five minutes, measured.").await;
    let latin = s
        .service
        .save(&repo, &Actor::cli(), note(Kind::Knowledge, "Cache TTL of the buyer list", "Same TTL for follower positions."))
        .await
        .unwrap();
    let titles: Vec<&str> = latin.similar.iter().map(|s| s.title.as_str()).collect();
    assert!(titles.iter().any(|t| t.starts_with("Cache TTL")), "the word tier still hints: {titles:?}");
}

/// A save whose title asserts the opposite of a live note on the same file answers with
/// that note (SPEC §memory.capture): a hint to supersede the stale one or link the two,
/// never a refusal, and not repeated among the similar hints. The same assertion, the
/// opposite one on another file and a note the save already links as contradicting are
/// no hint; a glob anchor shares the files it covers.
#[tokio::test]
async fn knowledge_capture_an_opposite_title_on_the_same_file_is_a_contradiction_hint() {
    let s = Scratch::new("opposed");
    let root = s.repo("app", None);
    for file in ["src/pay/webhook.rs", "src/pay/mod.rs", "src/shop/callback.rs"] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, "fn handle() {}\n").expect("write");
    }
    let repo = at(&root);
    let anchored = |title: &str, content: &str, anchors: &[&str]| {
        let mut request = note(Kind::Decision, title, content);
        request.anchors = anchors.iter().map(|a| a.to_string()).collect();
        request
    };
    let cli = Actor::cli();
    let save_anchored = |request: SaveRequest| s.service.save(&repo, &cli, request);

    let always = save_anchored(anchored(
        "Always retry the refund webhook",
        "The provider drops about one delivery in a hundred.",
        &["src/pay/webhook.rs"],
    ))
    .await
    .unwrap();
    assert!(always.contradicts.is_empty());
    let always = always.record.summary.id;
    let alike = save_anchored(anchored(
        "Retry the refund webhook on a timeout",
        "A timeout is the provider's, not ours.",
        &["src/pay/webhook.rs"],
    ))
    .await
    .unwrap();
    assert!(alike.contradicts.is_empty(), "the same assertion is no contradiction: {:?}", alike.contradicts);
    let alike = alike.record.summary.id;
    let elsewhere = save_anchored(anchored(
        "Never retry the refund webhook",
        "The shop's copy of the handler is fire-and-forget.",
        &["src/shop/callback.rs"],
    ))
    .await
    .unwrap();
    assert!(elsewhere.contradicts.is_empty(), "another file: {:?}", elsewhere.contradicts);

    let flipped = save_anchored(anchored(
        "Do not retry the refund webhook",
        "A retry double-refunds when the first delivery was only slow.",
        &["src/pay/webhook.rs", "src/pay/mod.rs"],
    ))
    .await
    .unwrap();
    assert_eq!(flipped.record.summary.status, Status::Active, "a hint never holds a note");
    let ids: Vec<&str> = flipped.contradicts.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, [always.as_str(), alike.as_str()], "oldest first");
    assert_eq!(flipped.contradicts[0].anchor, "src/pay/webhook.rs");
    assert_eq!(flipped.contradicts[0].title, "Always retry the refund webhook");
    let message = &flipped.contradicts[0].message;
    assert!(message.contains("contradicts?") && message.contains("outdated") && message.contains("replaces"), "{message}");
    assert!(!flipped.similar.iter().any(|h| ids.contains(&h.id.as_str())), "not repeated as similar");
    let wire = serde_json::to_value(&flipped).expect("serialize");
    assert_eq!(wire["contradicts"][0]["id"], always.as_str());
    assert!(serde_json::to_value(&elsewhere).expect("serialize").get("contradicts").is_none(), "absent when none");

    let mut linked = anchored(
        "Never retry refund webhooks",
        "Settled with the provider: they redeliver themselves.",
        &["src/pay/*.rs"],
    );
    linked.links = vec![LinkSpec { to: always.clone(), kind: LinkKind::Contradicts, note: None }];
    let linked = save_anchored(linked).await.unwrap();
    let ids: Vec<&str> = linked.contradicts.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, [alike.as_str()], "a glob shares its files; a linked note is no news");
    assert_eq!(linked.contradicts[0].anchor, "src/pay/*.rs");
}

/// A note that retells its anchored files is a formation hint, never a refusal: the
/// save already read the anchored text for the code check, so the lint says when most
/// of the note's characters are the code's own words back - what the code already says
/// is not worth a note (SPEC §memory.capture).
#[tokio::test]
async fn knowledge_capture_restated_code_is_advisory() {
    let s = Scratch::new("restates");
    let root = s.repo("app", None);
    std::fs::create_dir_all(root.join("src")).expect("src");
    let code = "pub fn retry_key(order: u64) -> String {\n    format!(\"retry-{order}\")\n}\n\npub fn place(order: u64) -> bool {\n    let key = retry_key(order);\n    key.len() == 8\n}\n";
    std::fs::write(root.join("src").join("retry.rs"), code).expect("write");
    let repo = at(&root);
    let mut retold = note(Kind::Knowledge, "Retry code", code);
    retold.anchors = vec!["src/retry.rs".into()];
    let saved = s.service.save(&repo, &Actor::cli(), retold).await.unwrap();
    assert_eq!(saved.record.summary.status, Status::Active, "a hint never holds a note");
    assert!(
        saved.formation.iter().any(|l| l.rule == "restates_code"),
        "{:?}",
        saved.formation
    );
    let mut why = note(
        Kind::Decision,
        "Retries reuse the key on purpose",
        "The key is derived from the order id so a retried charge is not charged twice; a fresh key per attempt would let a replay slip through unseen.",
    );
    why.anchors = vec!["src/retry.rs".into()];
    let clean = s.service.save(&repo, &Actor::cli(), why).await.unwrap();
    assert!(
        !clean.formation.iter().any(|l| l.rule == "restates_code"),
        "{:?}",
        clean.formation
    );
}

/// The formation lint (ADR-032) answers a save; it never refuses one.
#[tokio::test]
async fn knowledge_capture_formation_lint_is_advisory() {
    let s = Scratch::new("lint");
    let repo = at(&s.repo("app", None));
    let chain = "### What happens\nEvery push blocks.\n\n### Code path\n```text\na() -- src/a.rs -- x\nb() -- src/b.rs -- y\n\
                 c() -- src/c.rs -- z\n```\n";
    let saved = s.service.save(&repo, &Actor::cli(), note(Kind::Knowledge, "Push blocks", chain)).await.expect("saved anyway");
    let rules: Vec<&str> = saved.formation.iter().map(|l| l.rule).collect();
    // "Every push blocks" is one place's behaviour, not a count; a count in a checkout is the
    // code check's to say (tests/code.rs).
    assert_eq!(rules, ["sections", "call_chain"]);
    let atomic = "A full queue blocks the producer rather than drop an item: the replay trusts sequence numbers \
                  (observed in `src/replay.rs`), so a gap would go unseen.";
    let clean = s.service.save(&repo, &Actor::cli(), note(Kind::Decision, "A full queue blocks", atomic)).await.expect("save");
    assert!(clean.formation.is_empty(), "{:?}", clean.formation);
    // A duplicate wrote nothing, so it has nothing to say about shape.
    let again = s.service.save(&repo, &Actor::cli(), note(Kind::Knowledge, "Push blocks", chain)).await.expect("dup");
    assert!(again.duplicate && again.formation.is_empty());
}

/// An agent's preference opens with what the user said, quoted and dated (SPEC
/// §memory.capture): without it the save hints `user_words`; a person typing their own
/// rule in the panel is not asked to quote themselves.
#[tokio::test]
async fn knowledge_capture_a_preference_quotes_the_user() {
    let s = Scratch::new("user-words");
    let repo = at(&s.repo("app", None));
    let rules = |saved: &swiss_memory::service::SaveOutcome| saved.formation.iter().map(|l| l.rule).collect::<Vec<_>>();
    let mcp = Actor::mcp(Some("claude-code"));
    let paraphrase = note(Kind::Preference, "Answers stay short", "The user reads answers on a phone.");
    let saved = s.service.save(&repo, &mcp, paraphrase).await.expect("saved anyway");
    assert_eq!(rules(&saved), ["user_words"]);
    assert!(saved.formation[0].message.contains("quoted letter for letter and dated"), "{}", saved.formation[0].message);
    let quoted = note(
        Kind::Preference,
        "Commit messages stay one line",
        "> \"One line per commit message, please.\" - stated by the user, 2026-10-21\n\nThe log is read with --oneline.",
    );
    assert_eq!(rules(&s.service.save(&repo, &mcp, quoted).await.expect("save")), Vec::<&str>::new());
    let typed = note(Kind::Preference, "Tabs are four columns", "Every editor here renders a tab as four columns.");
    assert_eq!(rules(&s.service.save(&repo, &Actor::panel(), typed).await.expect("save")), Vec::<&str>::new());
}

#[tokio::test]
async fn knowledge_capture_secret_is_refused() {
    let s = Scratch::new("secret");
    let repo = at(&s.repo("app", None));
    let err = s
        .service
        .save(&repo, &Actor::cli(), note(Kind::Knowledge, "Deploy key", "use AKIAABCDEFGHIJKLMNOP for staging"))
        .await
        .unwrap_err();
    assert_eq!(err.code, Code::SensitiveContent);
    assert!(!err.message.contains("AKIA"), "the error never echoes the secret");
    let projects = s.service.projects().await.unwrap();
    assert_eq!(projects.len(), 1, "a refused save registers nothing");
}

#[tokio::test]
async fn knowledge_capture_replaces_supersedes_in_one_step() {
    let s = Scratch::new("replace");
    let repo = at(&s.repo("app", None));
    let old = save(&s.service, &repo, "Retry three times", "Three retries, fixed delay.").await;
    let mut next = note(Kind::Decision, "Retry with backoff", "Exponential backoff, five tries.");
    next.replaces = Some(old.clone());
    let new = s.service.save(&repo, &Actor::cli(), next).await.unwrap().record;
    let (old_view, history) = s.service.get(&repo, &old, true).await.unwrap();
    assert_eq!(old_view.summary.status, Status::Superseded);
    assert_eq!(old_view.superseded_by.as_deref(), Some(new.summary.id.as_str()));
    assert_eq!(history.unwrap().last().unwrap().op, "supersede");
    assert!(new.links.iter().any(|l| l.kind == LinkKind::Supersedes && l.id == old && l.dir == "out"));
    let (live, _) = s.service.list(&repo, &ListRequest::default()).await.unwrap();
    assert_eq!(live.len(), 1);
}

#[tokio::test]
async fn knowledge_capture_update_is_compare_and_swap() {
    let s = Scratch::new("cas");
    let repo = at(&s.repo("app", None));
    let id = save(&s.service, &repo, "Port", "19998 is the test instance.").await;
    let stale = UpdateRequest { expected_revision: 7, content: Some("x".into()), ..UpdateRequest::default() };
    let err = s.service.update(&repo, &Actor::cli(), &id, stale).await.unwrap_err();
    assert_eq!(err.code, Code::RevisionConflict);
    assert_eq!(err.extra["current"], 1);
    let fresh = UpdateRequest {
        expected_revision: 1,
        content: Some("19998 is the test instance; 19999 is production.".into()),
        reason: Some("clarify".into()),
        ..UpdateRequest::default()
    };
    let view = s.service.update(&repo, &Actor::cli(), &id, fresh).await.unwrap();
    assert_eq!(view.summary.revision, 2);
}

#[tokio::test]
async fn knowledge_capture_mark_confirm_restore_purge() {
    let s = Scratch::new("mark");
    let repo = at(&s.repo("app", None));
    let agent = Actor::mcp(Some("codex"));
    let id = s.service.save(&repo, &agent, note(Kind::Pitfall, "Flaky test", "Retry on CI.")).await.unwrap().record.summary.id;
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert!(view.summary.markers.contains(&"unconfirmed"));
    let view = s.service.confirm(&repo, &Actor::cli(), &id).await.unwrap();
    assert!(view.summary.confirmed && !view.summary.markers.contains(&"unconfirmed"));
    let view = s.service.mark(&repo, &agent, &id, Verdict::Outdated, Some("CI moved")).await.unwrap();
    assert_eq!(view.summary.status, Status::NeedsReview);
    let view = s.service.mark(&repo, &agent, &id, Verdict::Verified, None).await.unwrap();
    assert_eq!(view.summary.status, Status::Active);
    assert!(view.verified_ms.is_some());
    assert_eq!(code(s.service.mark(&repo, &agent, &id, Verdict::Wrong, None).await.unwrap_err()), Code::Invalid);
    let view = s.service.mark(&repo, &agent, &id, Verdict::Wrong, Some("fixed upstream")).await.unwrap();
    assert_eq!(view.summary.status, Status::Invalidated);
    assert!(s.service.list(&repo, &ListRequest::default()).await.unwrap().0.is_empty());
    let view = s.service.restore(&repo, &Actor::cli(), &id, "it came back").await.unwrap();
    assert_eq!(view.summary.status, Status::Active);
    assert_eq!(code(s.service.purge(&repo, &id, "m-0000000000").await.unwrap_err()), Code::Invalid);
    s.service.purge(&repo, &id, &id).await.unwrap();
    assert_eq!(code(s.service.get(&repo, &id, false).await.unwrap_err()), Code::NotFound);
    // A person typing in the panel needs no second step; the CLI may be an agent with a shell.
    let typed = s.service.save(&repo, &Actor::panel(), note(Kind::Preference, "Tabs", "Hard tabs.")).await.unwrap();
    assert!(typed.record.summary.confirmed);
    let shelled = s.service.save(&repo, &Actor::cli(), note(Kind::Preference, "Spaces", "Two.")).await.unwrap();
    assert!(!shelled.record.summary.confirmed);
    // The inbox lists what the counts say waits: the shell's note, not the person's.
    let inbox: Vec<String> = s.service.review(&repo).await.unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(inbox, vec![shelled.record.summary.id.clone()]);
    s.service.confirm(&repo, &Actor::cli(), &shelled.record.summary.id).await.unwrap();
    assert!(s.service.review(&repo).await.unwrap().is_empty());
}

// --- provenance and evidence (ADR-032) ------------------------------------------------------

/// Who wrote a note (producer) and how it arrived (transport) are two facts, and neither is
/// whether a person confirmed it or whether a source verifies it.
#[tokio::test]
async fn knowledge_provenance_producer_transport_and_confirmation_are_separate() {
    let s = Scratch::new("provenance");
    let repo = at(&s.repo("app", None));
    let by = |actor: Actor, title: &'static str| {
        let service = &s.service;
        let repo = repo.clone();
        async move { service.save(&repo, &actor, note(Kind::Decision, title, "Written for the test.")).await.unwrap().record }
    };
    let panel = by(Actor::panel(), "Typed in the panel").await;
    assert_eq!((panel.summary.producer, panel.summary.transport, panel.summary.confirmed), (Producer::Human, Transport::Panel, true));
    let mcp = by(Actor::mcp(Some("claude")), "Written over MCP").await;
    assert_eq!((mcp.summary.producer, mcp.summary.transport, mcp.summary.confirmed), (Producer::Agent, Transport::Mcp, false));
    let cli = by(Actor::cli(), "Written in a shell").await;
    assert_eq!((cli.summary.producer, cli.summary.transport, cli.summary.confirmed), (Producer::Unknown, Transport::Cli, false));
    let boot = by(Actor::cli_as(Producer::Bootstrap), "Written by a bootstrap").await;
    assert_eq!((boot.summary.producer, boot.summary.transport, boot.summary.confirmed), (Producer::Bootstrap, Transport::Cli, false));
    let sync = by(Actor::sync(), "Written by sync").await;
    assert_eq!((sync.summary.producer, sync.summary.transport, sync.summary.confirmed), (Producer::Sync, Transport::Internal, false));
    for view in [&panel, &mcp, &cli, &boot, &sync] {
        assert!(!view.summary.source_verified, "{}: no evidence, nothing verified", view.summary.title);
    }

    // A person confirming a note neither verifies its source nor changes who wrote it.
    let confirmed = s.service.confirm(&repo, &Actor::panel(), &boot.summary.id).await.unwrap();
    assert!(confirmed.summary.confirmed);
    assert!(!confirmed.summary.source_verified);
    assert_eq!((confirmed.summary.producer, confirmed.summary.transport), (Producer::Bootstrap, Transport::Cli));

    // A caller may declare human, agent, bootstrap or import; sync and unknown are the system's.
    assert_eq!(Producer::declared("bootstrap"), Some(Producer::Bootstrap));
    assert_eq!(Producer::declared("human"), Some(Producer::Human));
    assert_eq!(Producer::declared("sync"), None);
    assert_eq!(Producer::declared("unknown"), None);
}

/// Evidence binds to the text it checked: confirming leaves it current, an edit leaves it
/// behind (kept, no longer current), and a verified mark with fresh evidence binds anew.
#[tokio::test]
async fn knowledge_evidence_binds_to_the_text_it_checked() {
    let s = Scratch::new("evidence");
    let root = s.repo("app", None);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("retry.rs"), "pub fn retry() {}\n").unwrap();
    let repo = at(&root);
    let mut request = note(
        Kind::Pitfall,
        "Retries reuse the idempotency key",
        "Observed in retry(): the key is reused, so a retried charge is not charged twice.",
    );
    request.evidence = vec![
        EvidenceSpec::parse("bootstrap:run-7").unwrap(),
        EvidenceSpec::parse("verified_by=code:src/retry.rs#retry").unwrap(),
    ];
    let saved = s.service.save(&repo, &Actor::cli_as(Producer::Bootstrap), request).await.unwrap().record;
    let id = saved.summary.id.clone();
    assert!(saved.summary.source_verified);
    assert!(!saved.summary.confirmed, "a verified source is not a person's confirmation");
    assert_eq!(saved.evidence.len(), 2);
    let code_ev = saved.evidence.iter().find(|e| e.source_type == SourceType::Code).unwrap();
    assert_eq!((code_ev.relation, code_ev.revision, code_ev.current), (Relation::VerifiedBy, 1, true));
    assert_eq!((code_ev.path.as_deref(), code_ev.symbol.as_deref()), (Some("src/retry.rs"), Some("retry")));
    assert!(code_ev.source_hash.as_deref().is_some_and(|h| h.starts_with("sha256:")), "{:?}", code_ev.source_hash);
    assert!(code_ev.id.starts_with("e-"));
    let run = saved.evidence.iter().find(|e| e.source_type == SourceType::Bootstrap).unwrap();
    assert_eq!((run.relation, run.run_id.as_deref()), (Relation::DerivedFrom, Some("run-7")));

    let confirmed = s.service.confirm(&repo, &Actor::panel(), &id).await.unwrap();
    assert!(confirmed.summary.source_verified && confirmed.summary.confirmed);
    assert!(confirmed.evidence.iter().all(|e| e.current), "the text did not change");

    let edit = UpdateRequest {
        expected_revision: confirmed.summary.revision,
        content: Some("Observed in retry(): the key is reused.".into()),
        ..UpdateRequest::default()
    };
    let edited = s.service.update(&repo, &Actor::cli(), &id, edit).await.unwrap();
    assert!(!edited.summary.source_verified);
    assert_eq!(edited.evidence.len(), 2, "evidence is history: kept");
    assert!(edited.evidence.iter().all(|e| !e.current));
    let (items, _) = s.service.list(&repo, &ListRequest::default()).await.unwrap();
    assert!(!items.iter().find(|i| i.id == id).unwrap().source_verified, "a list says the same");

    let test = vec![EvidenceSpec::parse("test:tests/retry.rs#retry_reuses_the_key").unwrap()];
    let marked = s.service.mark_with(&repo, &Actor::mcp(None), &id, Verdict::Verified, None, test).await.unwrap();
    assert!(marked.summary.source_verified);
    let test_ev = marked.evidence.iter().find(|e| e.source_type == SourceType::Test).unwrap();
    assert_eq!((test_ev.relation, test_ev.revision, test_ev.current), (Relation::VerifiedBy, marked.summary.revision, true));
    assert_eq!(test_ev.source_hash, None, "the file is not there: no hash is invented");

    // An update may carry evidence too: it binds to the revision the update writes.
    let edit = UpdateRequest {
        expected_revision: marked.summary.revision,
        title: Some("Retries reuse their idempotency key".into()),
        evidence: vec![EvidenceSpec::parse("supports=commit:ABC1234").unwrap()],
        ..UpdateRequest::default()
    };
    let retitled = s.service.update(&repo, &Actor::cli(), &id, edit).await.unwrap();
    let commit = retitled.evidence.iter().find(|e| e.source_type == SourceType::Commit).unwrap();
    assert_eq!((commit.commit_sha.as_deref(), commit.revision, commit.current), (Some("abc1234"), retitled.summary.revision, true));
    assert!(!retitled.summary.source_verified, "a commit that supports a note does not verify it");
}

#[tokio::test]
async fn knowledge_evidence_is_checked_at_the_door() {
    for bad in [
        "code",
        "code:",
        "nonsense:x",
        "refutes=code:src/a.rs",
        "code:/etc/passwd",
        "code:../outside.rs",
        "code:src/*.rs",
        "code:src/a.rs:42",
        "code:src/a.rs#L42",
        "commit:not-a-sha",
        "verified_by=session:abc",
        "verified_by=bootstrap:run-1",
        "session:",
    ] {
        assert!(EvidenceSpec::parse(bad).is_err(), "{bad:?} parsed");
    }
    let json = serde_json::json!([
        {"sourceType": "code", "relation": "verified_by", "path": "src/a.rs", "symbol": "Retry::run", "excerpt": "fn run()"},
        {"sourceType": "session", "sessionId": "s-1"}
    ]);
    let specs = EvidenceSpec::list_from_json(&json).unwrap();
    assert_eq!(specs.len(), 2);
    for bad in [
        serde_json::json!({"sourceType": "code"}),
        serde_json::json!([{"sourceType": "code", "path": "src/a.rs", "line": 4}]),
        serde_json::json!([{"sourceType": "doc"}]),
    ] {
        assert!(EvidenceSpec::list_from_json(&bad).is_err(), "{bad}");
    }

    let s = Scratch::new("evidence-door");
    let repo = at(&s.repo("app", None));
    let mut request = note(Kind::Decision, "Too much evidence", "One claim.");
    request.evidence = (0..11).map(|n| EvidenceSpec::parse(&format!("session:s-{n}")).unwrap()).collect();
    assert_eq!(code(s.service.save(&repo, &Actor::cli(), request).await.unwrap_err()), Code::Invalid);
    let id = save(&s.service, &repo, "A claim", "Because.").await;
    let ev = vec![EvidenceSpec::parse("verified_by=code:src/a.rs").unwrap()];
    let err = s.service.mark_with(&repo, &Actor::cli(), &id, Verdict::Outdated, Some("moved"), ev).await.unwrap_err();
    assert_eq!(code(err), Code::Invalid, "evidence backs a verified verdict only");
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!((view.summary.revision, view.evidence.len()), (1, 0), "a refused mark wrote nothing");
}

// --- graph ----------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_graph_link_rules() {
    let s = Scratch::new("graph");
    let a_repo = at(&s.repo("a", None));
    let b_repo = at(&s.repo("b", None));
    let a = save(&s.service, &a_repo, "A one", "first").await;
    let a2 = save(&s.service, &a_repo, "A two", "second").await;
    let b = save(&s.service, &b_repo, "B one", "other project").await;
    let cli = Actor::cli();

    // Cross-scope: the other end is simply not found here.
    let err = s.service.link(&a_repo, &cli, &a, &b, LinkKind::Supports, None).await.unwrap_err();
    assert_eq!(err.code, Code::NotFound);
    // Self link and the system kind.
    assert_eq!(code(s.service.link(&a_repo, &cli, &a, &a, LinkKind::Supports, None).await.unwrap_err()), Code::Invalid);
    assert_eq!(code(s.service.link(&a_repo, &cli, &a, &a2, LinkKind::Supersedes, None).await.unwrap_err()), Code::Invalid);

    // Duplicate is idempotent; a note change is an update.
    let (_, changed) = s.service.link(&a_repo, &cli, &a, &a2, LinkKind::DependsOn, None).await.unwrap();
    assert!(changed);
    let (_, changed) = s.service.link(&a_repo, &cli, &a, &a2, LinkKind::DependsOn, None).await.unwrap();
    assert!(!changed);
    let (edge, changed) = s.service.link(&a_repo, &cli, &a, &a2, LinkKind::DependsOn, Some("needs the schema")).await.unwrap();
    assert!(changed && edge.directed && edge.note.as_deref() == Some("needs the schema"));

    // Symmetric kinds are stored from < to, whichever way they were asked.
    let (hi, lo) = if a > a2 { (&a, &a2) } else { (&a2, &a) };
    let (edge, _) = s.service.link(&a_repo, &cli, hi, lo, LinkKind::RelatedTo, None).await.unwrap();
    assert_eq!(&edge.from, lo);
    assert!(!edge.directed);
    assert!(s.service.unlink(&a_repo, hi, lo, LinkKind::RelatedTo).await.unwrap());
    assert!(!s.service.unlink(&a_repo, lo, hi, LinkKind::RelatedTo).await.unwrap());

    // The graph read: the center at hop 0, its neighbour at hop 1.
    let view = s
        .service
        .graph(&a_repo, &GraphRequest { center: Some(a.clone()), ..GraphRequest::default() })
        .await
        .unwrap();
    assert_eq!(view.nodes.len(), 2);
    assert_eq!(view.nodes[0].id, a);
    assert_eq!(view.nodes[0].hop, Some(0));
    assert_eq!(view.nodes[1].hop, Some(1));
    assert_eq!(view.edges.len(), 1);

    // Purge removes edges.
    s.service.purge(&a_repo, &a2, &a2).await.unwrap();
    let (view, _) = s.service.get(&a_repo, &a, false).await.unwrap();
    assert!(view.links.is_empty());
}

#[tokio::test]
async fn knowledge_graph_project_delete_cascades() {
    let s = Scratch::new("cascade");
    let repo_dir = s.repo("app", None);
    let repo = at(&repo_dir);
    let a = save(&s.service, &repo, "One", "first").await;
    let b = save(&s.service, &repo, "Two", "second").await;
    s.service.link(&repo, &Actor::cli(), &a, &b, LinkKind::Supports, None).await.unwrap();
    let project = s.service.projects().await.unwrap().into_iter().find(|p| !p.global).unwrap();
    assert_eq!(project.counts.live, 2);
    assert_eq!(project.counts.links, 1);
    assert_eq!(code(s.service.delete_project(&project.id, "nope").await.unwrap_err()), Code::Invalid);
    assert_eq!(s.service.delete_project(&project.id, &project.id).await.unwrap(), (2, 1));
    assert_eq!(s.service.projects().await.unwrap().len(), 1);
    // The directory is unbound with it: the next save registers a fresh project.
    let again = s.service.save(&repo, &Actor::cli(), note(Kind::Decision, "One", "first")).await.unwrap();
    assert!(again.project.registered);
    assert_ne!(again.project.id, project.id);
    assert_eq!(code(s.service.delete_project("global", "global").await.unwrap_err()), Code::Invalid);
}

// --- association ----------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_association_ranks_linked_and_similar_first() {
    let s = Scratch::new("related");
    let repo = at(&s.repo("app", None));
    let seed = save(&s.service, &repo, "Order service retries payment calls", "Retry the payment gateway with backoff.").await;
    let linked = save(&s.service, &repo, "Payment gateway timeout is 30s", "The gateway answers within thirty seconds.").await;
    let similar = save(&s.service, &repo, "Payment retries must be idempotent", "Retries reuse the idempotency key.").await;
    let unrelated = save(&s.service, &repo, "Logo colour", "The logo is teal.").await;
    s.service.link(&repo, &Actor::cli(), &seed, &linked, LinkKind::DependsOn, None).await.unwrap();

    let request = RelatedRequest { seed: Seed::Id(seed.clone()), depth: None, limit: None, include_history: false };
    let first = s.service.related(&repo, &request).await.unwrap();
    let ids: Vec<&str> = first.items.iter().map(|r| r.summary.id.as_str()).collect();
    assert!(!ids.contains(&seed.as_str()), "the seed is never its own association");
    assert_eq!(ids.first(), Some(&linked.as_str()), "a direct link outranks text");
    assert!(ids.contains(&similar.as_str()));
    assert!(!ids.contains(&unrelated.as_str()));
    assert!(first.items[0].reasons.iter().any(|r| matches!(r, swiss_memory::Reason::Link { .. })));
    let again = s.service.related(&repo, &request).await.unwrap();
    assert_eq!(ids, again.items.iter().map(|r| r.summary.id.as_str()).collect::<Vec<_>>(), "deterministic");

    let text = RelatedRequest {
        seed: Seed::Text("why do payment retries need an idempotency key?".into()),
        depth: None,
        limit: None,
        include_history: false,
    };
    let found = s.service.related(&repo, &text).await.unwrap();
    assert_eq!(found.items.first().map(|r| r.summary.id.as_str()), Some(similar.as_str()));
    assert!(!found.items.iter().any(|r| r.summary.id == unrelated));
}

// --- freshness ------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_freshness_anchor_change_and_dependency() {
    let s = Scratch::new("fresh");
    let root = s.repo("app", None);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("lib.rs"), "fn a() {}\n").unwrap();
    let repo = at(&root);
    let mut request = note(Kind::Constraint, "lib.rs stays dependency-free", "No external crates in lib.rs.");
    request.anchors = vec!["src/lib.rs".into()];
    let id = s.service.save(&repo, &Actor::cli(), request).await.unwrap().record.summary.id;
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!(view.anchors[0].state, "ok");
    assert!(view.anchors[0].hash.as_deref().unwrap_or("").starts_with("sha256:"));

    std::fs::write(root.join("src").join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!(view.anchors[0].state, "changed");
    assert!(view.summary.markers.contains(&"anchor_changed"));
    assert_eq!(s.service.review(&repo).await.unwrap().len(), 1);

    s.service.mark(&repo, &Actor::cli(), &id, Verdict::Verified, None).await.unwrap();
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert!(!view.summary.markers.contains(&"anchor_changed"), "verified re-baselines");

    std::fs::remove_file(root.join("src").join("lib.rs")).unwrap();
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!(view.anchors[0].state, "missing");
    assert!(view.summary.markers.contains(&"anchor_changed"));

    let base = save(&s.service, &repo, "Schema v2", "The store is at schema 2.").await;
    let dependent = save(&s.service, &repo, "Import needs schema v2", "Import reads format 2.").await;
    s.service.link(&repo, &Actor::cli(), &dependent, &base, LinkKind::DependsOn, None).await.unwrap();
    s.service.mark(&repo, &Actor::cli(), &base, Verdict::Wrong, Some("schema 3 now")).await.unwrap();
    let (view, _) = s.service.get(&repo, &dependent, false).await.unwrap();
    assert!(view.summary.markers.contains(&"dependency_broken"));
}

// --- scope ----------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_scope_nested_dir_and_nested_repo() {
    let s = Scratch::new("nested");
    let root = s.repo("app", None);
    let deep = root.join("src").join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    let id = save(&s.service, &at(&root), "Top", "top level").await;
    s.service.get(&at(&deep), &id, false).await.expect("a nested directory is the same project");
    let inner = root.join("vendor").join("lib");
    std::fs::create_dir_all(inner.join(".git")).unwrap();
    assert_eq!(code(s.service.get(&at(&inner), &id, false).await.unwrap_err()), Code::NotFound, "a nested repo is its own project");
}

#[tokio::test]
async fn knowledge_scope_linked_worktree_shares_the_project() {
    let s = Scratch::new("worktree");
    let main = s.repo("main", None);
    let admin = main.join(".git").join("worktrees").join("feature");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let tree = s.plain("feature");
    std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    let id = save(&s.service, &at(&main), "Shared", "seen from every worktree").await;
    s.service.get(&at(&tree), &id, false).await.expect("a linked worktree resolves to its main repository");
}

/// `main` with a linked worktree `tree` under `.agents/worktrees/<name>`.
fn worktree_of(main: &Path, name: &str) -> PathBuf {
    let admin = main.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let tree = main.join(".agents").join("worktrees").join(name);
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    std::fs::write(admin.join("gitdir"), format!("{}\n", tree.join(".git").display())).unwrap();
    tree
}

/// The first write of a repository coming from a linked worktree (an agent's branch
/// checkout under the main one) registers the main checkout: the project is named after
/// the repository and its root is the main checkout, never the worktree, which is gone
/// once its branch lands. The worktree's own calls still read the worktree: a note an
/// agent anchors to a file only its branch has is hashed there, not held as missing.
#[tokio::test]
async fn knowledge_scope_a_worktree_registers_the_main_checkout() {
    let s = Scratch::new("worktree-first");
    let main = s.repo("java-biz", None);
    let tree = worktree_of(&main, "assistant-notification");

    let outcome = s.service.save(&at(&tree.join("src")), &Actor::cli(), note(Kind::Decision, "First", "from a worktree")).await.unwrap();
    let view = s.service.project(&outcome.project.id).await.unwrap();
    assert_eq!(view.name, "java-biz", "named after the repository, not the worktree");
    let paths: Vec<&str> = view.roots.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, [swiss_memory::scope::display(&main)], "the root is the main checkout");
    let brief = s.service.brief(&at(&tree), &BriefRequest::default()).await.unwrap();
    assert_eq!(brief.root, Some(swiss_memory::scope::display(&tree)), "a read from the worktree checks the worktree");
    s.service.get(&at(&main), &outcome.record.summary.id, false).await.expect("the main checkout reads it");

    std::fs::write(tree.join("src").join("Notify.java"), "class Notify {}\n").unwrap();
    let mut request = note(Kind::Knowledge, "Notify sends the assistant's messages", "Notify is the branch's sender.");
    request.anchors = vec!["src/Notify.java".into()];
    let branch = s.service.save(&at(&tree.join("src")), &Actor::mcp(Some("codex")), request).await.unwrap();
    assert_eq!(branch.record.summary.status, Status::Active, "the branch's file is there: {:?}", branch.code);
    let (view, _) = s.service.get(&at(&tree), &branch.record.summary.id, false).await.unwrap();
    assert_eq!(view.anchors[0].state, "ok", "hashed in the worktree");
}

/// A root row a linked worktree wrote before roots followed the main checkout heals on
/// the repository's next write: it names the main checkout again, and the project keeps
/// its id and notes.
#[tokio::test]
async fn knowledge_scope_a_worktree_root_row_heals_to_the_main_checkout() {
    let s = Scratch::new("worktree-heal");
    let main = s.repo("app", None);
    let tree = worktree_of(&main, "feature");
    let first = s.service.save(&at(&main), &Actor::cli(), note(Kind::Decision, "First", "from the main checkout")).await.unwrap();
    let identity = s.service.project(&first.project.id).await.unwrap().roots[0].identity.clone();
    {
        // What a worktree's first write stored before: the worktree itself.
        let mut conn = s.service.store().await.unwrap().read().await.unwrap();
        swiss_memory::store::move_root(&mut conn, &identity, &tree.display().to_string()).await.unwrap();
    }
    let stale = s.service.project(&first.project.id).await.unwrap();
    assert_eq!(stale.roots[0].path, swiss_memory::scope::display(&tree), "the fixture's stale row");
    s.service.save(&at(&tree), &Actor::cli(), note(Kind::Decision, "Second", "from the worktree")).await.unwrap();
    let view = s.service.project(&first.project.id).await.unwrap();
    let paths: Vec<&str> = view.roots.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, [swiss_memory::scope::display(&main)], "the row follows the main checkout");
    assert_eq!(view.id, first.project.id);
}

/// A bare repository has no main checkout, so its first worktree's write registers that
/// worktree; another worktree's writes leave it alone while it stands, and once it is
/// removed the row follows the worktree writing now instead of naming a dead directory.
#[tokio::test]
async fn knowledge_scope_a_bare_repositorys_dead_worktree_root_heals() {
    let s = Scratch::new("bare-heal");
    let bare = s.plain("app.git");
    std::fs::write(bare.join("config"), "[core]\n\tbare = true\n").unwrap();
    let worktree = |name: &str| {
        let admin = bare.join("worktrees").join(name);
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let tree = s.plain(name);
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        tree
    };
    let (one, two) = (worktree("one"), worktree("two"));
    let first = s.service.save(&at(&one), &Actor::cli(), note(Kind::Decision, "First", "from one")).await.unwrap();
    let roots = |view: swiss_memory::ProjectView| view.roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
    assert_eq!(roots(s.service.project(&first.project.id).await.unwrap()), [swiss_memory::scope::display(&one)]);

    s.service.save(&at(&two), &Actor::cli(), note(Kind::Decision, "Second", "from two")).await.unwrap();
    assert_eq!(roots(s.service.project(&first.project.id).await.unwrap()), [swiss_memory::scope::display(&one)], "one still stands");

    std::fs::remove_dir_all(&one).unwrap();
    let third = s.service.save(&at(&two), &Actor::cli(), note(Kind::Decision, "Third", "from two again")).await.unwrap();
    assert_eq!(third.project.id, first.project.id);
    assert_eq!(roots(s.service.project(&first.project.id).await.unwrap()), [swiss_memory::scope::display(&two)], "the row follows two");
}

#[tokio::test]
async fn knowledge_scope_reclone_is_not_merged_silently() {
    let s = Scratch::new("reclone");
    let first = s.repo("first", Some("https://user:pw@github.com/acme/app.git"));
    let second = s.repo("second", Some("git@github.com:acme/app"));
    let outcome = s.service.save(&at(&first), &Actor::cli(), note(Kind::Decision, "One", "first clone")).await.unwrap();
    let project = outcome.project.id;
    let err = s.service.save(&at(&second), &Actor::cli(), note(Kind::Decision, "Two", "second clone")).await.unwrap_err();
    assert_eq!(err.code, Code::ProjectAmbiguous);
    assert_eq!(err.extra["possibleMatch"]["projectId"], project.as_str());
    let brief = s.service.brief(&at(&second), &BriefRequest::default()).await.unwrap();
    assert!(brief.markdown.contains("clone of project"), "{}", brief.markdown);
    let view = s.service.bind_root(&project, &second.display().to_string(), false, None).await.unwrap();
    assert_eq!(view.roots.len(), 2);
    assert!(view.roots.iter().all(|r| r.origin_url.as_deref() == Some("github.com/acme/app")), "userinfo stripped");
    s.service.save(&at(&second), &Actor::cli(), note(Kind::Decision, "Two", "second clone")).await.unwrap();
}

#[tokio::test]
async fn knowledge_scope_marker_and_exact_bindings() {
    let s = Scratch::new("marker");
    let portable = s.plain("portable");
    let init = s.service.init(&portable.display().to_string(), None, InitMode::Portable, Some("Portable")).await.unwrap();
    assert!(init.created);
    assert!(portable.join(".swiss").join("memory.json").is_file());
    let outcome = s.service.save(&at(&portable), &Actor::cli(), note(Kind::Knowledge, "Moved", "survives a move")).await.unwrap();
    assert_eq!(outcome.project.id, init.project.id);

    let plain = s.plain("plain");
    assert_eq!(
        code(s.service.save(&at(&plain), &Actor::cli(), note(Kind::Knowledge, "x", "y")).await.unwrap_err()),
        Code::NotFound,
        "no boundary, no silent registration"
    );
    let here = s.service.init(&plain.display().to_string(), None, InitMode::Here, None).await.unwrap();
    assert!(here.identity.starts_with("path:"));
    let sub = plain.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let outcome = s.service.save(&at(&sub), &Actor::cli(), note(Kind::Knowledge, "x", "y")).await.unwrap();
    assert_eq!(outcome.project.id, here.project.id);
}

#[tokio::test]
async fn knowledge_scope_bad_cwd_is_refused() {
    let s = Scratch::new("cwd");
    let relative = Target::dir("relative/dir");
    assert_eq!(code(s.service.list(&relative, &ListRequest::default()).await.unwrap_err()), Code::InvalidCwd);
    let missing = at(&s.dir.join("nope"));
    assert_eq!(code(s.service.list(&missing, &ListRequest::default()).await.unwrap_err()), Code::InvalidCwd);
}

// --- brief ----------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_brief_selects_and_isolates() {
    let s = Scratch::new("brief");
    let root = s.repo("app", None);
    let other = at(&s.repo("other", None));
    let repo = at(&root);
    let mut anchored = note(Kind::Constraint, "Never block the runtime", "The runtime is current_thread.");
    anchored.anchors = vec!["src/**/*.rs".into()];
    let anchored = s.service.save(&repo, &Actor::cli(), anchored).await.unwrap().record.summary.id;
    let dependency = save(&s.service, &repo, "Store opens lazily", "OnceCell on first use.").await;
    s.service.link(&repo, &Actor::cli(), &anchored, &dependency, LinkKind::DependsOn, None).await.unwrap();
    let task = save(&s.service, &repo, "缓存失效策略", "缓存按项目失效，写入时清空。").await;
    let old = save(&s.service, &repo, "Old retry rule", "three tries").await;
    let mut next = note(Kind::Decision, "New retry rule", "backoff");
    next.replaces = Some(old.clone());
    s.service.save(&repo, &Actor::cli(), next).await.unwrap();
    let wrong = save(&s.service, &repo, "Wrong claim", "nope").await;
    s.service.mark(&repo, &Actor::cli(), &wrong, Verdict::Wrong, Some("false")).await.unwrap();
    let foreign = save(&s.service, &other, "Foreign note", "belongs elsewhere").await;
    let global = Target::global(root.display().to_string());
    let shared = s.service.save(&global, &Actor::cli(), note(Kind::Preference, "Answer in Chinese", "中文回答。")).await.unwrap();

    let request = BriefRequest {
        paths: vec!["src/service.rs".into()],
        task: Some("修复缓存失效".into()),
        ..BriefRequest::default()
    };
    let brief = s.service.brief(&repo, &request).await.unwrap();
    let md = &brief.markdown;
    assert!(md.contains("Notes recorded earlier"), "standing note");
    assert!(md.contains("Anchored to your paths") && md.contains(&anchored));
    assert!(md.contains(&dependency), "a linked neighbour rides along");
    assert!(md.contains(&task), "the CJK task finds its note");
    assert!(md.contains(&shared.record.summary.id), "global preferences are included");
    for absent in [&old, &wrong, &foreign] {
        assert!(!md.contains(absent.as_str()), "{absent} must not appear:\n{md}");
    }
    assert!(brief.chars <= 6000);
}

/// A focused brief names what was tried and dropped where the task or the paths point
/// (SPEC §memory.brief): a superseded decision with what replaced it and an invalidated
/// one, a line each under Rejected before and outside the notes it shows as current. A
/// note its successor only reworded, history the task does not hold, and an orientation
/// brief name none.
#[tokio::test]
async fn knowledge_brief_names_the_rejected_precedent() {
    let s = Scratch::new("brief-rejected");
    let root = s.repo("app", None);
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("src").join("queue.rs"), "fn poll() {}\n").expect("write");
    let repo = at(&root);
    let cli = Actor::cli();
    let redis = save(&s.service, &repo, "Use Redis for the session cache", "Sessions live in Redis with a one-hour TTL.").await;
    let mut sqlite = note(
        Kind::Decision,
        "Keep sessions in the SQLite session table",
        "Redis was one more service to run for a few hundred sessions; the store already has WAL.",
    );
    sqlite.replaces = Some(redis.clone());
    let sqlite = s.service.save(&repo, &cli, sqlite).await.unwrap().record.summary.id;
    let memcached = save(&s.service, &repo, "Memcached fronts the session cache", "Measured in the load test.").await;
    s.service.mark(&repo, &cli, &memcached, Verdict::Wrong, Some("the load test ran without it")).await.unwrap();
    // Reworded, not rejected: the successor keeps every word of the claim.
    let wal = save(&s.service, &repo, "Use WAL", "Readers never block the writer.").await;
    let mut wal_mode = note(Kind::Decision, "Use WAL for the session cache store", "Readers never block the writer.");
    wal_mode.replaces = Some(wal.clone());
    s.service.save(&repo, &cli, wal_mode).await.unwrap();
    // History the task does not hold.
    let old_port = save(&s.service, &repo, "Serve the panel on port 8080", "Before the gateway.").await;
    let mut new_port = note(Kind::Decision, "Serve the panel on the gateway port", "One port to open.");
    new_port.replaces = Some(old_port.clone());
    s.service.save(&repo, &cli, new_port).await.unwrap();
    let mut polled = note(Kind::Decision, "Poll the job queue every second", "Cheap enough at ten jobs a minute.");
    polled.anchors = vec!["src/queue.rs".into()];
    let polled = s.service.save(&repo, &cli, polled).await.unwrap().record.summary.id;
    s.service.mark(&repo, &cli, &polled, Verdict::Wrong, Some("the queue notifies now")).await.unwrap();

    let request = BriefRequest { task: Some("add a redis session cache".into()), ..BriefRequest::default() };
    let brief = s.service.brief(&repo, &request).await.unwrap();
    let md = &brief.markdown;
    let rejected = md.split("## Rejected before").nth(1).unwrap_or_else(|| panic!("a rejected section:\n{md}"));
    assert!(rejected.contains(&format!("`{redis}` — replaced by `{sqlite}`")), "{md}");
    assert!(rejected.contains(&format!("`{memcached}` — invalidated")), "{md}");
    for absent in [&wal, &old_port, &polled] {
        assert!(!md.contains(absent.as_str()), "{absent} is no precedent of this task:\n{md}");
    }
    for history in [&redis, &memcached] {
        assert!(!brief.shown.contains(history), "history is not the brief's current context");
    }

    // The paths point at a rejected note: nothing current, then the precedent.
    let request = BriefRequest { paths: vec!["src/queue.rs".into()], ..BriefRequest::default() };
    let brief = s.service.brief(&repo, &request).await.unwrap();
    let md = &brief.markdown;
    assert!(md.contains("Nothing current in memory about these paths."), "{md}");
    assert!(md.contains(&format!("`{polled}` — invalidated")), "{md}");
    assert!(!md.contains(&redis), "{md}");

    let orientation = s.service.brief(&repo, &BriefRequest::default()).await.unwrap();
    assert!(!orientation.markdown.contains("Rejected before"), "{}", orientation.markdown);
}

#[tokio::test]
async fn knowledge_brief_unregistered_directory_says_so() {
    let s = Scratch::new("brief-empty");
    let root = s.repo("fresh", None);
    let brief = s.service.brief(&at(&root), &BriefRequest::default()).await.unwrap();
    assert!(brief.markdown.contains("Not a registered project"), "{}", brief.markdown);
    assert_eq!(s.service.projects().await.unwrap().len(), 1, "a read never registers");
}

// --- search ---------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_search_tiers() {
    let s = Scratch::new("search");
    let repo = at(&s.repo("app", None));
    let cjk = save(&s.service, &repo, "缓存设计", "项目级缓存，按写入失效。").await;
    let ident = save(&s.service, &repo, "OrderService.create validates stock", "It checks stock before insert.").await;
    let mut anchored = note(Kind::Knowledge, "Entry point", "The gateway starts here.");
    anchored.anchors = vec!["src/main.rs".into()];
    let anchored = s.service.save(&repo, &Actor::cli(), anchored).await.unwrap().record.summary.id;

    let top = |query: &str| SearchQuery { text: query.into(), ..SearchQuery::default() };
    let hits = s.service.search(&repo, &top("缓存")).await.unwrap();
    assert_eq!(hits.first().map(|h| h.summary.id.as_str()), Some(cjk.as_str()));
    let hits = s.service.search(&repo, &top("OrderService.create")).await.unwrap();
    assert_eq!(hits.first().map(|h| h.summary.id.as_str()), Some(ident.as_str()));
    let hits = s.service.search(&repo, &top("src/main.rs")).await.unwrap();
    assert_eq!(hits.first().map(|h| h.summary.id.as_str()), Some(anchored.as_str()));
    let hits = s.service.search(&repo, &top(&ident)).await.unwrap();
    assert_eq!(hits.first().map(|h| h.tier), Some("id"));
    assert!(s.service.search(&repo, &top("\"stock* OR (")).await.is_ok(), "FTS metacharacters are data");
    assert!(s.service.search(&repo, &top("zebra")).await.unwrap().is_empty());
    assert_eq!(code(s.service.search(&repo, &top("   ")).await.unwrap_err()), Code::Invalid);
}

#[tokio::test]
async fn a_word_hit_holds_a_share_of_the_query() {
    let s = Scratch::new("word-floor");
    let repo = at(&mapped_repo(&s));
    map_of(&s.service, &repo).await;
    let mut wanted = note(Kind::Pitfall, "Restart the test instance", "Kill it by the PID that owns its port.");
    wanted.domain = Some("memory".into());
    let wanted = s.service.save(&repo, &Actor::cli(), wanted).await.unwrap().record.summary.id;
    let mut strays = Vec::new();
    for (title, content) in [
        ("Panel dev server", "Vite serves the panel on its own port."),
        ("Tunnel forwards", "A tunnel maps a local port to a remote one."),
        ("Proxy listener", "The proxy binds a port per route."),
        ("Firewall prompt", "Windows asks once per port."),
    ] {
        let mut stray = note(Kind::Decision, title, content);
        stray.domain = Some("panel".into());
        strays.push(s.service.save(&repo, &Actor::cli(), stray).await.unwrap().record.summary.id);
    }
    save(&s.service, &repo, "Unrelated", "Nothing in common here.").await;
    let found = |text: &str| {
        let (service, repo) = (&s.service, repo.clone());
        let query = SearchQuery { text: text.into(), limit: 10, ..SearchQuery::default() };
        async move { service.search(&repo, &query).await.unwrap().into_iter().map(|h| h.summary.id).collect::<Vec<_>>() }
    };
    // One common word of four is no hit: bm25 ranks such notes low but kept them, and a
    // context of five took them whenever fewer notes matched well.
    assert_eq!(found("restart the test instance port").await, vec![wanted.clone()]);
    // A query of that one word admits every note holding it.
    assert_eq!(found("port").await.len(), 5);
    // Words no note holds leave the share: they cannot tell notes apart.
    assert_eq!(found("restart test instance zebra quokka").await, vec![wanted.clone()]);

    // The brief's task hits take the same floor, so the four strays no longer outvote the
    // hit and put the caller in their domain.
    let task = BriefRequest { task: Some("restart the test instance port".into()), ..BriefRequest::default() };
    let md = s.service.brief(&repo, &task).await.unwrap().markdown;
    assert!(md.contains("## Where you are\n\n### Memory · `memory`") && md.contains(&wanted), "{md}");
    assert!(!md.contains("`panel`") && strays.iter().all(|id| !md.contains(id.as_str())), "{md}");
    // The task's one hit holds all its words: the brief closes saying so.
    assert!(md.ends_with("\nRetrieval: strong (1 exact or full) — answer from the first hit, checking it against the code it names.\n"), "{md}");

    // A search says the same of its hits, by the word share each carries.
    let searched = |text: &str| {
        let (service, repo) = (&s.service, repo.clone());
        let query = SearchQuery { text: text.into(), limit: 10, ..SearchQuery::default() };
        async move { service.search(&repo, &query).await.unwrap() }
    };
    let hits = searched("restart the test instance port").await;
    assert_eq!(hits[0].word_share, Some(1.0));
    assert!(render::hits(&hits).ends_with("\nRetrieval: strong (1 exact or full) — answer from the first hit, checking it against the code it names.\n"));
    // Five notes hold the one word, filed in two domains: say which is meant.
    let text = render::hits(&searched("port").await);
    assert!(text.ends_with("\nRetrieval: mixed (5 exact or full, split across `memory`, `panel`) — say which domain you mean, or confirm the first hit before relying on it.\n"), "{text}");
    // An exact tier holds the query whatever its words.
    let hits = searched(&wanted).await;
    assert_eq!(hits[0].tier, "id");
    assert!(render::hits(&hits).contains("Retrieval: strong (1 exact or full"));
}

#[tokio::test]
async fn knowledge_search_a_hub_mention_breaks_a_tie() {
    let s = Scratch::new("hub-mention");
    let repo = at(&mapped_repo(&s));
    let (tunnel, router) = ("Deploy waits for the tunnel", "Deploy waits for the router");
    let a = save(&s.service, &repo, tunnel, "Check it first.").await;
    let b = save(&s.service, &repo, router, "Check it first.").await;
    // The hub names the note the id order would put second.
    let (named, other, named_title, other_title) = if a > b { (a, b, tunnel, router) } else { (b, a, router, tunnel) };
    let mut memory = area("memory", "Memory", &["crates/swiss-memory"]);
    memory.body = Some(format!("## Overview\nThe memory crate. Before a release: *{named_title}*."));
    s.service.create_domain(&repo, &Actor::panel(), memory).await.unwrap();
    let searched = |text: &str| {
        let (service, repo) = (&s.service, repo.clone());
        let query = SearchQuery { text: text.into(), limit: 10, ..SearchQuery::default() };
        async move { service.search(&repo, &query).await.unwrap() }
    };
    let ids = |hits: &[swiss_memory::HitView]| hits.iter().map(|h| h.summary.id.clone()).collect::<Vec<_>>();

    // The same words and the same bm25: the hub's note leads by W_HUB, its word evidence
    // as it was - a rank, not a word.
    let hits = searched("deploy waits zebra").await;
    assert_eq!(ids(&hits), vec![named.clone(), other.clone()]);
    assert_eq!(hits[0].score - hits[1].score, swiss_memory::search::W_HUB);
    assert_eq!((hits[0].tier, hits[0].word_share), (hits[1].tier, hits[1].word_share));
    // Both hold every word: the band's top caps the mention and the tie goes to the hub's note.
    let hits = searched("waits for deploy").await;
    assert_eq!(ids(&hits), vec![named.clone(), other.clone()]);
    assert_eq!(hits[0].score, hits[1].score);
    // The exact tiers stay as they are: the other note's title is its own, at the same score
    // the hub's note gets for its title.
    let exact = searched(other_title).await;
    assert_eq!((exact[0].summary.id.as_str(), exact[0].tier), (other.as_str(), "title"));
    let own = searched(named_title).await;
    assert_eq!((own[0].summary.id.as_str(), own[0].tier, own[0].score), (named.as_str(), "title", exact[0].score));
}

// --- domains --------------------------------------------------------------------------------

fn area(path: &str, title: &str, owns: &[&str]) -> DomainRequest {
    DomainRequest {
        path: path.into(),
        title: title.into(),
        summary: Some(format!("{title} in one line")),
        body: None,
        owns: owns.iter().map(|o| o.to_string()).collect(),
        keywords: Vec::new(),
    }
}

/// A repository holding the files the domains below own.
fn mapped_repo(s: &Scratch) -> PathBuf {
    let root = s.repo("app", None);
    for file in [
        "crates/swiss-memory/src/store.rs",
        "crates/swiss-memory/src/brief.rs",
        "crates/swiss-panel/panel/src/app.ts",
        "README.md",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("// {file}\n")).unwrap();
    }
    root
}

async fn map_of(service: &MemoryService, target: &Target) {
    for request in [
        area("memory", "Memory", &["crates/swiss-memory"]),
        area("memory/store", "Store", &["crates/swiss-memory/src/store.rs"]),
        area("panel", "Panel", &["crates/swiss-panel"]),
    ] {
        service.create_domain(target, &Actor::panel(), request).await.expect("domain");
    }
}

fn anchored(kind: Kind, title: &str, anchors: &[&str]) -> SaveRequest {
    let mut request = note(kind, title, &format!("{title}, because."));
    request.anchors = anchors.iter().map(|a| a.to_string()).collect();
    request
}

#[tokio::test]
async fn knowledge_domains_tree_rules() {
    let s = Scratch::new("domains-tree");
    let root = s.repo("app", None);
    let repo = at(&root);
    let top = s.service.create_domain(&repo, &Actor::cli(), area("memory", "记忆", &["crates/swiss-memory/"])).await.unwrap();
    assert!(!top.confirmed, "a CLI domain waits for a person");
    assert_eq!((top.depth, top.revision), (1, 1));
    assert_eq!(top.owns, vec!["crates/swiss-memory"], "owns are anchor paths");
    s.service.create_domain(&repo, &Actor::panel(), area("memory/store", "Store", &[])).await.unwrap();
    s.service.create_domain(&repo, &Actor::panel(), area("memory/store/fts", "FTS", &[])).await.unwrap();
    s.service.create_domain(&repo, &Actor::panel(), area("memory/store/fts/query", "Query", &[])).await.unwrap();
    s.service.create_domain(&repo, &Actor::panel(), area("memory/store/fts/query/rank", "Rank", &[])).await.unwrap();

    let refused = |request: DomainRequest| {
        let service = &s.service;
        let repo = repo.clone();
        async move { service.create_domain(&repo, &Actor::panel(), request).await.unwrap_err().code }
    };
    assert_eq!(refused(area("memory", "Again", &[])).await, Code::DomainExists);
    assert_eq!(refused(area("memory/store/fts/query/rank/x", "Deep", &[])).await, Code::Invalid, "five levels at most");
    assert_eq!(refused(area("orphan/child", "Orphan", &[])).await, Code::Invalid, "a sub-domain needs its parent");
    assert_eq!(refused(area("Memory", "Caps", &[])).await, Code::Invalid, "paths are lowercase");
    assert_eq!(refused(area("x", &"t".repeat(81), &[])).await, Code::Invalid);
    let mut secret = area("x", "Keys", &[]);
    secret.body = Some("deploy with AKIAABCDEFGHIJKLMNOP".into());
    assert_eq!(refused(secret).await, Code::SensitiveContent);
    let global = Target::global(root.display().to_string());
    let err = s.service.create_domain(&global, &Actor::panel(), area("x", "X", &[])).await.unwrap_err();
    assert_eq!(err.code, Code::Invalid, "the global scope has no map");

    // Compare-and-swap; a body or owns change is a verification, a title is not.
    let stale = DomainUpdate { expected_revision: 9, title: Some("Memory".into()), ..DomainUpdate::default() };
    let err = s.service.update_domain(&repo, &Actor::panel(), "memory", stale).await.unwrap_err();
    assert_eq!((err.code, err.extra["current"].as_u64()), (Code::RevisionConflict, Some(1)));
    let titled = s
        .service
        .update_domain(&repo, &Actor::panel(), &top.id, DomainUpdate { expected_revision: 1, title: Some("Memory".into()), ..DomainUpdate::default() })
        .await
        .unwrap();
    assert_eq!((titled.revision, titled.verified_ms), (2, top.verified_ms));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let hub = DomainUpdate { expected_revision: 2, body: Some("## Overview\nThe notes.".into()), ..DomainUpdate::default() };
    let hubbed = s.service.update_domain(&repo, &Actor::panel(), "memory", hub).await.unwrap();
    assert!(hubbed.verified_ms > top.verified_ms);
    let confirmed = s.service.confirm_domain(&repo, &Actor::panel(), "memory").await.unwrap();
    assert!(confirmed.confirmed);

    let list = s.service.domains(&repo).await.unwrap();
    let paths: Vec<&str> = list.items.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        paths,
        ["memory", "memory/store", "memory/store/fts", "memory/store/fts/query", "memory/store/fts/query/rank"],
        "tree order"
    );
    assert!(list.items.iter().all(|d| d.body.is_none()), "the map carries no hubs");
    let detail = s.service.domain(&repo, "memory", true).await.unwrap();
    assert_eq!(detail.domain.body.as_deref(), Some("## Overview\nThe notes."));
    assert_eq!(detail.children.len(), 1);
    let ops: Vec<String> = detail.history.unwrap().into_iter().map(|r| r.op).collect();
    assert_eq!(ops, ["create", "edit", "edit", "confirm"]);
    let err = s.service.domain(&repo, "nowhere", false).await.unwrap_err();
    assert_eq!(err.code, Code::NotFound);
    assert!(err.message.contains("memory/store"), "the error names the map: {}", err.message);
}

#[tokio::test]
async fn knowledge_a_domain_confirmed_whole_takes_its_subtree_and_leaves_what_needs_review() {
    let s = Scratch::new("domains-confirm-whole");
    let root = mapped_repo(&s);
    let repo = at(&root);
    for request in [area("memory", "Memory", &["crates/swiss-memory"]), area("memory/store", "Store", &[]), area("panel", "Panel", &[])] {
        s.service.create_domain(&repo, &Actor::cli(), request).await.unwrap();
    }
    let save = |title: &'static str, domain: Option<&'static str>| {
        let (service, repo) = (&s.service, repo.clone());
        async move {
            let mut request = note(Kind::Decision, title, &format!("{title}, because."));
            request.domain = domain.map(str::to_string);
            service.save(&repo, &Actor::cli(), request).await.unwrap().record.summary.id
        }
    };
    let stored = save("One writer at a time", Some("memory/store")).await;
    let doubted = save("Briefs cap at 6000 chars", Some("memory")).await;
    let panel = save("Views draw through ui", Some("panel")).await;
    let unfiled = save("Ports 19998 and 19999", None).await;
    s.service.mark(&repo, &Actor::cli(), &doubted, Verdict::Outdated, Some("the cap moved")).await.unwrap();

    let done = s.service.confirm_tree(&repo, &Actor::panel(), "memory").await.unwrap();
    assert!(done.domain.confirmed);
    assert_eq!((done.domains, done.notes, done.held), (2, 1, 1), "memory and memory/store, the store note; the doubted one waits");
    let confirmed = |id: String| {
        let (service, repo) = (&s.service, repo.clone());
        async move { service.get(&repo, &id, false).await.unwrap().0.summary.confirmed }
    };
    assert!(confirmed(stored.clone()).await, "a note under a sub-domain is in the subtree");
    assert!(!confirmed(doubted).await, "a note that needs review is left for a verdict");
    assert!(!confirmed(panel).await && !confirmed(unfiled).await, "outside the subtree nothing moves");
    let list = s.service.domains(&repo).await.unwrap();
    let marks: Vec<(&str, bool)> = list.items.iter().map(|d| (d.path.as_str(), d.confirmed)).collect();
    assert_eq!(marks, [("memory", true), ("memory/store", true), ("panel", false)]);

    // Each confirmation is a revision that says what carried it.
    let (_, history) = s.service.get(&repo, &stored, true).await.unwrap();
    let last = history.unwrap().pop().unwrap();
    assert_eq!((last.op.as_str(), last.reason.as_deref()), ("confirm", Some("confirmed with memory")));
    let sub = s.service.domain(&repo, "memory/store", true).await.unwrap().history.unwrap().pop().unwrap();
    assert_eq!((sub.op.as_str(), sub.reason.as_deref()), ("confirm", Some("confirmed with memory")));

    let again = s.service.confirm_tree(&repo, &Actor::panel(), "memory").await.unwrap();
    assert_eq!((again.domains, again.notes, again.held), (0, 0, 1), "idempotent");
    let revisions = s.service.domain(&repo, "memory", true).await.unwrap().history.unwrap().len();
    assert_eq!(revisions, 2, "create and one confirm");
}

#[tokio::test]
async fn knowledge_domains_move_and_delete_keep_notes() {
    let s = Scratch::new("domains-move");
    let root = mapped_repo(&s);
    let repo = at(&root);
    map_of(&s.service, &repo).await;
    let mut filed = note(Kind::Decision, "One writer at a time", "BEGIN IMMEDIATE.");
    filed.domain = Some("memory/store".into());
    let id = s.service.save(&repo, &Actor::cli(), filed).await.unwrap().record.summary.id;

    // A move carries the sub-domains along; the note follows without a new revision.
    let memory = s.service.domain(&repo, "memory", false).await.unwrap().domain;
    let moved = DomainUpdate { expected_revision: memory.revision, path: Some("core".into()), ..DomainUpdate::default() };
    s.service.update_domain(&repo, &Actor::panel(), "memory", moved).await.unwrap();
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!((view.summary.domain.as_deref(), view.summary.revision), (Some("core/store"), 1));
    let store = s.service.domain(&repo, "core/store", true).await.unwrap();
    assert_eq!(store.history.unwrap().last().map(|r| r.op.clone()), Some("move".into()));

    let core = s.service.domain(&repo, "core", false).await.unwrap().domain;
    let under = DomainUpdate { expected_revision: core.revision, path: Some("core/store/core".into()), ..DomainUpdate::default() };
    assert_eq!(s.service.update_domain(&repo, &Actor::panel(), "core", under).await.unwrap_err().code, Code::Invalid);
    let taken = DomainUpdate { expected_revision: core.revision, path: Some("panel".into()), ..DomainUpdate::default() };
    assert_eq!(s.service.update_domain(&repo, &Actor::panel(), "core", taken).await.unwrap_err().code, Code::DomainExists);

    // Delete: by the path, never over sub-domains; the notes go up a level, then unfiled.
    assert_eq!(s.service.delete_domain(&repo, "core", "core").await.unwrap_err().code, Code::Invalid);
    assert_eq!(s.service.delete_domain(&repo, "core/store", "store").await.unwrap_err().code, Code::Invalid);
    let gone = s.service.delete_domain(&repo, "core/store", "core/store").await.unwrap();
    assert_eq!((gone.refiled, gone.to.as_deref()), (1, Some("core")));
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!(view.summary.domain.as_deref(), Some("core"));
    let gone = s.service.delete_domain(&repo, "core", "core").await.unwrap();
    assert_eq!((gone.refiled, gone.to), (1, None));
    let (view, _) = s.service.get(&repo, &id, false).await.unwrap();
    assert_eq!((view.summary.domain, view.summary.status), (None, Status::Active), "a note outlives its domain");
}

#[tokio::test]
async fn knowledge_domains_filing_by_anchors() {
    let s = Scratch::new("domains-filing");
    let root = mapped_repo(&s);
    let repo = at(&root);
    // A note saved before the map exists stays unfiled until autofile.
    let early = s
        .service
        .save(&repo, &Actor::cli(), anchored(Kind::Pitfall, "Early pitfall", &["crates/swiss-panel/panel/src/app.ts"]))
        .await
        .unwrap()
        .record
        .summary;
    assert_eq!(early.domain, None);
    map_of(&s.service, &repo).await;
    let filed_in = |request: SaveRequest| {
        let service = &s.service;
        let repo = repo.clone();
        async move { service.save(&repo, &Actor::cli(), request).await.unwrap().record.summary }
    };
    let store = filed_in(anchored(Kind::Decision, "Store rule", &["crates/swiss-memory/src/store.rs"])).await;
    assert_eq!(store.domain.as_deref(), Some("memory/store"), "the deepest owner");
    let tie = filed_in(anchored(
        Kind::Decision,
        "Store and brief",
        &["crates/swiss-memory/src/store.rs", "crates/swiss-memory/src/brief.rs"],
    ))
    .await;
    assert_eq!(tie.domain.as_deref(), Some("memory"), "a tie goes to the common ancestor");
    let glob = filed_in(anchored(Kind::Decision, "Every source", &["crates/swiss-memory/src/*.rs"])).await;
    assert_eq!(glob.domain.as_deref(), Some("memory"), "a glob by its literal prefix");
    let loose = filed_in(anchored(Kind::Knowledge, "Readme fact", &["README.md"])).await;
    assert_eq!(loose.domain, None, "no owner, no domain");
    let mut by_hand = anchored(Kind::Decision, "Filed by hand", &["crates/swiss-memory/src/store.rs"]);
    by_hand.domain = Some("panel".into());
    let by_hand = filed_in(by_hand).await;
    assert_eq!(by_hand.domain.as_deref(), Some("panel"), "an explicit domain wins");
    let mut unknown = note(Kind::Knowledge, "Nowhere", "x");
    unknown.domain = Some("nowhere".into());
    let err = s.service.save(&repo, &Actor::cli(), unknown).await.unwrap_err();
    assert_eq!(err.code, Code::NotFound);
    assert!(err.extra["paths"].as_array().is_some_and(|p| p.len() == 3));

    // Re-anchoring files an unfiled note; filing alone needs no revision and writes none.
    let reanchored = UpdateRequest {
        expected_revision: 1,
        anchors: Some(vec!["crates/swiss-panel/panel/src/app.ts".into()]),
        ..UpdateRequest::default()
    };
    let view = s.service.update(&repo, &Actor::cli(), &loose.id, reanchored).await.unwrap();
    assert_eq!(view.summary.domain.as_deref(), Some("panel"));
    let moved = UpdateRequest { domain: Some("memory".into()), ..UpdateRequest::default() };
    let view = s.service.update(&repo, &Actor::cli(), &loose.id, moved).await.unwrap();
    assert_eq!((view.summary.domain.as_deref(), view.summary.revision), (Some("memory"), 2));
    let unfiled = UpdateRequest { domain: Some(String::new()), ..UpdateRequest::default() };
    let view = s.service.update(&repo, &Actor::cli(), &loose.id, unfiled).await.unwrap();
    assert_eq!(view.summary.domain, None);

    // Autofile takes the unfiled notes only; a hand filing stays.
    assert_eq!(s.service.autofile(&repo).await.unwrap(), 2, "the early note and the re-anchored one");
    let (view, _) = s.service.get(&repo, &early.id, false).await.unwrap();
    assert_eq!(view.summary.domain.as_deref(), Some("panel"));
    let (view, _) = s.service.get(&repo, &by_hand.id, false).await.unwrap();
    assert_eq!(view.summary.domain.as_deref(), Some("panel"));

    let outcome = s.service.file(&repo, &[store.id.clone(), tie.id.clone()], Some("panel")).await.unwrap();
    assert_eq!(outcome.moved, 2);
    let outcome = s.service.file(&repo, std::slice::from_ref(&store.id), Some("panel")).await.unwrap();
    assert_eq!(outcome.moved, 0, "already there");
    s.service.file(&repo, std::slice::from_ref(&store.id), None).await.unwrap();

    // Reads narrow to a subtree, or to the unfiled.
    let listed = |domain: &str| {
        let service = &s.service;
        let repo = repo.clone();
        let request = ListRequest { domain: Some(domain.into()), ..ListRequest::default() };
        async move {
            let (items, _) = service.list(&repo, &request).await.unwrap();
            let mut titles: Vec<String> = items.into_iter().map(|s| s.title).collect();
            titles.sort();
            titles
        }
    };
    assert_eq!(listed("memory").await, ["Every source"]);
    assert_eq!(listed("none").await, ["Store rule"]);
    assert_eq!(listed("panel").await.len(), 4);
    let query = SearchQuery { text: "store".into(), domain: Some("panel".into()), ..SearchQuery::default() };
    let hits = s.service.search(&repo, &query).await.unwrap();
    assert!(!hits.is_empty() && hits.iter().all(|h| h.summary.domain.as_deref() == Some("panel")));

    let map = s.service.domains(&repo).await.unwrap();
    let panel = map.items.iter().find(|d| d.path == "panel").unwrap();
    assert_eq!(panel.counts.live, 4);
    assert_eq!(map.unfiled.live, 1);
}

#[tokio::test]
async fn knowledge_brief_with_a_map() {
    let s = Scratch::new("brief-map");
    let root = mapped_repo(&s);
    let repo = at(&root);
    map_of(&s.service, &repo).await;
    let store = s.service.domain(&repo, "memory/store", false).await.unwrap().domain;
    let hub = DomainUpdate {
        expected_revision: store.revision,
        body: Some("## Overview\nOne SQLite file: `Store` in `crates/swiss-memory/src/store.rs`.".into()),
        ..DomainUpdate::default()
    };
    s.service.update_domain(&repo, &Actor::panel(), "memory/store", hub).await.unwrap();
    let pitfall = s
        .service
        .save(&repo, &Actor::cli(), anchored(Kind::Pitfall, "Writes need BEGIN IMMEDIATE", &["crates/swiss-memory/src/store.rs"]))
        .await
        .unwrap()
        .record
        .summary
        .id;
    let mut there = note(Kind::Decision, "One connection per process", "A pool of one.");
    there.domain = Some("memory/store".into());
    let there = s.service.save(&repo, &Actor::cli(), there).await.unwrap().record.summary.id;
    let mut elsewhere = note(Kind::Constraint, "Panel strings come from tr()", "Both locales.");
    elsewhere.domain = Some("panel".into());
    let elsewhere = s.service.save(&repo, &Actor::cli(), elsewhere).await.unwrap().record.summary.id;
    let unfiled = save(&s.service, &repo, "Port 19999 is production", "Never restart it.").await;

    let request = BriefRequest { paths: vec!["crates/swiss-memory/src/store.rs".into()], ..BriefRequest::default() };
    let md = s.service.brief(&repo, &request).await.unwrap().markdown;
    assert!(md.contains("## Where you are") && md.contains("One SQLite file"), "{md}");
    // The anchored note in full; the domain's other note counted, `memory_domain` one call away.
    assert!(md.contains(&pitfall) && !md.contains(&there), "{md}");
    assert!(md.contains("In `memory/store`: 1 more note — `memory_domain` lists them all."), "{md}");
    assert!(!md.contains(&elsewhere), "another domain's rule is counted on the map, not listed:\n{md}");
    // A brief about a place answers that place: the map and the unfiled notes are the
    // session start's, not repeated at every call.
    assert!(!md.contains("## Map") && !md.contains(&unfiled), "{md}");

    let start = s.service.brief(&repo, &BriefRequest::default()).await.unwrap().markdown;
    assert!(!start.contains("## Where you are") && start.contains("## Unfiled") && start.contains(&unfiled), "{start}");
    assert!(start.contains("## Map\n- `memory` Memory") && start.contains("  - `memory/store` Store"), "{start}");

    let miss = BriefRequest { task: Some("extended thinking budget".into()), ..BriefRequest::default() };
    let md = s.service.brief(&repo, &miss).await.unwrap().markdown;
    assert!(md.contains("Nothing in memory matches “extended thinking budget”."), "{md}");
    assert!(!md.contains("## Map") && !md.contains("## Unfiled"), "{md}");
}

// --- projects -------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_projects_crud_from_the_panel() {
    let s = Scratch::new("projects");
    let created = s.service.create_project(Some("Docs site"), None, false, None).await.unwrap();
    assert!(created.roots.is_empty());
    let panel = Target::project(created.id.clone());
    let id = s.service.save(&panel, &Actor::panel(), note(Kind::Knowledge, "Hosted on Pages", "GitHub Pages.")).await.unwrap();
    assert_eq!(id.project.id, created.id, "a project without directories still takes notes");

    let docs = s.plain("docs");
    let bound = s.service.bind_root(&created.id, &docs.display().to_string(), true, None).await.unwrap();
    assert_eq!(bound.roots.len(), 1);
    s.service.get(&at(&docs), &id.record.summary.id, false).await.expect("the bound directory selects it");

    let other = s.service.create_project(Some("Other"), None, false, None).await.unwrap();
    let err = s.service.bind_root(&other.id, &docs.display().to_string(), true, None).await.unwrap_err();
    assert_eq!(err.code, Code::RootBound);
    assert_eq!(err.extra["projectId"], created.id.as_str());

    let renamed = s.service.rename_project(&created.id, "  Docs   website ").await.unwrap();
    assert_eq!(renamed.name, "Docs website");
    let identity = bound.roots[0].identity.clone();
    let unbound = s.service.unbind_root(&created.id, &identity).await.unwrap();
    assert!(unbound.roots.is_empty());
    s.service.bind_root(&other.id, &docs.display().to_string(), true, None).await.unwrap();

    let names: Vec<String> = s.service.projects().await.unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["Global", "Docs website", "Other"], "global first, then by name");
    assert_eq!(code(s.service.rename_project("global", "x").await.unwrap_err()), Code::Invalid);
}

#[tokio::test]
async fn knowledge_projects_a_directory_is_one_projects_and_can_move() {
    let s = Scratch::new("roots");
    let root = s.repo("app", None);
    let saved = s.service.save(&at(&root), &Actor::cli(), note(Kind::Decision, "Kept", "by the project's id")).await.unwrap();
    let owner = saved.project.id.clone();

    // One directory, one project, whatever the identity's kind.
    let other = s.service.create_project(Some("Other"), None, false, None).await.unwrap();
    let err = s.service.bind_root(&other.id, &root.display().to_string(), true, None).await.unwrap_err();
    assert_eq!((err.code, err.extra["projectId"].as_str()), (Code::RootBound, Some(owner.as_str())));
    let err = s.service.init(&root.display().to_string(), None, InitMode::Here, None).await.unwrap_err();
    assert_eq!(err.code, Code::RootBound);
    assert_eq!(s.service.projects().await.unwrap().len(), 3, "a refused init registers nothing");

    // The checkout moves: its directory changes in one step and the notes follow the id.
    let moved = s.dir.join("moved");
    std::fs::rename(&root, &moved).unwrap();
    let identity = s.service.project(&owner).await.unwrap().roots[0].identity.clone();
    let view = s.service.rebind_root(&owner, &identity, &moved.display().to_string(), false, None).await.unwrap();
    assert_eq!(view.roots.len(), 1);
    assert_ne!(view.roots[0].identity, identity);
    s.service.get(&at(&moved), &saved.record.summary.id, false).await.expect("the notes followed the project");

    // A refused change keeps the old directory.
    let docs = s.plain("docs");
    s.service.bind_root(&other.id, &docs.display().to_string(), true, None).await.unwrap();
    let err = s
        .service
        .rebind_root(&owner, &view.roots[0].identity, &docs.display().to_string(), true, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, Code::RootBound);
    let kept: Vec<String> = s.service.project(&owner).await.unwrap().roots.into_iter().map(|r| r.identity).collect();
    assert_eq!(kept, [view.roots[0].identity.clone()]);
    let err = s.service.rebind_root(&owner, "path:nowhere", &docs.display().to_string(), true, None).await.unwrap_err();
    assert_eq!(err.code, Code::NotFound);
}

// --- transfer -------------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_transfer_round_trip() {
    let s = Scratch::new("export");
    let repo = at(&s.repo("app", None));
    let a = save(&s.service, &repo, "One", "first").await;
    let b = save(&s.service, &repo, "Two", "second").await;
    s.service.link(&repo, &Actor::cli(), &a, &b, LinkKind::Supports, Some("why")).await.unwrap();
    let keyed = DomainRequest { keywords: vec!["checkout".into(), "结账".into()], ..area("app", "App", &[]) };
    s.service.create_domain(&repo, &Actor::panel(), keyed).await.unwrap();
    s.service.file(&repo, std::slice::from_ref(&a), Some("app")).await.unwrap();
    let checked = vec![EvidenceSpec::parse("commit:abc1234").unwrap()];
    s.service.mark_with(&repo, &Actor::cli(), &a, Verdict::Verified, None, checked).await.unwrap();
    let text = s.service.export(&ExportScope::All).await.unwrap();
    assert!(text.lines().next().unwrap().contains("\"formatVersion\":4"));
    let record_a = text.lines().find(|l| l.contains(&a) && l.contains("\"type\":\"record\"")).unwrap();
    assert!(record_a.contains("\"producer\":\"unknown\"") && record_a.contains("\"transport\":\"cli\""), "{record_a}");
    assert!(record_a.contains("\"evidence\":[") && record_a.contains("\"commitSha\":\"abc1234\""), "{record_a}");
    let domain_line = text.lines().position(|l| l.contains("\"type\":\"domain\"")).expect("a domain line");
    assert!(domain_line < text.lines().position(|l| l.contains("\"type\":\"record\"")).unwrap(), "domains before records");
    let without = |kind: &str| text.lines().filter(|l| !l.contains(kind)).collect::<Vec<_>>().join("\n");

    let t = Scratch::new("import");
    let dry = t.service.import(&text, true, ImportStrategy::Fail).await.unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.records_created, 2);
    assert_eq!(t.service.projects().await.unwrap().len(), 1, "a dry run writes nothing");

    let report = t.service.import(&text, false, ImportStrategy::Fail).await.unwrap();
    assert_eq!((report.records_created, report.links_created, report.domains_created), (2, 1, 1));
    let project = report.projects_created[0].clone();
    let (view, history) = t.service.get(&Target::project(project.clone()), &a, true).await.unwrap();
    assert_eq!(view.links.len(), 1);
    assert_eq!(view.summary.domain.as_deref(), Some("app"), "a note keeps its place on the map");
    assert_eq!(history.unwrap().len(), 2);
    assert_eq!((view.summary.producer, view.summary.transport), (Producer::Unknown, Transport::Cli));
    assert_eq!(view.evidence.len(), 1, "evidence travels with its note");
    assert!(view.evidence[0].current && view.summary.source_verified);
    assert!(t.service.project(&project).await.unwrap().roots.is_empty(), "identities are never imported");
    let map = t.service.domain(&Target::project(project.clone()), "app", false).await.unwrap();
    assert_eq!(map.domain.keywords, ["checkout", "结账"], "keywords travel with their domain");
    let x = Scratch::new("import-keywords");
    let overlong = text.replace("\"结账\"", &format!("{:?}", "k".repeat(80)));
    assert_eq!(code(x.service.import(&overlong, false, ImportStrategy::Fail).await.unwrap_err()), Code::Invalid);

    let again = t.service.import(&text, false, ImportStrategy::Skip).await.unwrap();
    assert_eq!(again.records_skipped.len(), 2);
    assert_eq!(again.domains_skipped, ["app"]);
    assert_eq!(again.links_skipped, 1);
    assert_eq!(code(t.service.import(&text, false, ImportStrategy::Fail).await.unwrap_err()), Code::SimilarExists);

    let broken = text.replace(&b, "m-ffffffffff").replace("\"Two\"", "\"Three\"");
    let only_link: String = broken.lines().filter(|l| l.contains("header") || l.contains("\"link\"")).collect::<Vec<_>>().join("\n");
    assert_eq!(code(t.service.import(&only_link, false, ImportStrategy::Skip).await.unwrap_err()), Code::Invalid);

    // A map the store already has takes the file's notes in; a differing id is the strategy's call.
    let u = Scratch::new("import-map");
    let head: String = text.lines().filter(|l| l.contains("\"header\"") || l.contains("\"project\"")).collect::<Vec<_>>().join("\n");
    let created = u.service.import(&head, false, ImportStrategy::Fail).await.unwrap().projects_created[0].clone();
    let own = Target::project(created.clone());
    let mine = u.service.create_domain(&own, &Actor::panel(), area("app", "Mine", &[])).await.unwrap();
    assert_eq!(code(u.service.import(&text, false, ImportStrategy::Fail).await.unwrap_err()), Code::DomainExists);
    let merged = u.service.import(&text, false, ImportStrategy::Skip).await.unwrap();
    assert_eq!((merged.domains_skipped.len(), merged.records_created), (1, 2));
    let detail = u.service.domain(&own, "app", false).await.unwrap();
    assert_eq!((detail.domain.id, detail.notes.len()), (mine.id, 1));

    // A record filed in a domain the file lacks is refused; a version 2 file imports unfiled.
    let v = Scratch::new("import-old");
    let orphaned = without("\"type\":\"domain\"");
    assert_eq!(code(v.service.import(&orphaned, false, ImportStrategy::Fail).await.unwrap_err()), Code::Invalid);
    let old = orphaned.replace("\"formatVersion\":4", "\"formatVersion\":2");
    v.service.import(&old, false, ImportStrategy::Fail).await.unwrap();
    let (view, _) = v.service.get(&Target::project(project.clone()), &a, false).await.unwrap();
    assert_eq!(view.summary.domain, None);
    let future = text.replace("\"formatVersion\":4", "\"formatVersion\":5");
    assert_eq!(code(v.service.import(&future, false, ImportStrategy::Skip).await.unwrap_err()), Code::UnsupportedSchema);

    // A version 3 file has no provenance: it is derived from the actor, as the store's own
    // migration derives it, and whatever a v3 line claims beside it is not read.
    let w = Scratch::new("import-v3");
    let panel_note = s.service.save(&repo, &Actor::panel(), note(Kind::Decision, "Typed", "By a person.")).await.unwrap().record;
    let agent_note = s.service.save(&repo, &Actor::mcp(Some("codex")), note(Kind::Decision, "Proposed", "By an agent.")).await.unwrap().record;
    let v3: String = s
        .service
        .export(&ExportScope::All)
        .await
        .unwrap()
        .lines()
        .map(|l| {
            let mut value: serde_json::Value = serde_json::from_str(l).unwrap();
            if let Some(map) = value.as_object_mut() {
                if map.get("type").and_then(|t| t.as_str()) == Some("header") {
                    map.insert("formatVersion".into(), serde_json::json!(3));
                }
                map.remove("evidence");
                map.insert("producer".into(), serde_json::json!("bootstrap"));
                map.remove("transport");
            }
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    w.service.import(&v3, false, ImportStrategy::Fail).await.unwrap();
    let derived = |id: String| {
        let w = &w;
        let project = project.clone();
        async move { w.service.get(&Target::project(project), &id, false).await.unwrap().0.summary }
    };
    let typed = derived(panel_note.summary.id.clone()).await;
    assert_eq!((typed.producer, typed.transport), (Producer::Human, Transport::Panel));
    let proposed = derived(agent_note.summary.id.clone()).await;
    assert_eq!((proposed.producer, proposed.transport), (Producer::Agent, Transport::Mcp));
    let shell = derived(b.clone()).await;
    assert_eq!((shell.producer, shell.transport), (Producer::Unknown, Transport::Cli));

    let backup = t.service.backup(None).await.unwrap();
    assert!(backup.is_file());
    let copy = MemoryService::at(&backup);
    assert_eq!(copy.projects().await.unwrap().len(), 2);
}

/// Two people, two stores, the same work under two project ids: one exports a domain, the
/// other reads the report and merges it. The note they both wrote lands once, the note
/// whose title the other's store already holds with other text is a conflict kept out
/// with its links, and a note edited after the first merge comes back as a revision
/// conflict - never a silent skip.
#[tokio::test]
async fn knowledge_transfer_two_stores_merge_a_domain_with_conflicts() {
    let alice = Scratch::new("merge-alice");
    let mine = at(&alice.repo("app", None));
    for (path, title, owns) in [("api", "API", "src/api/"), ("api/auth", "Auth", "src/api/auth/"), ("web", "Web", "src/web/")] {
        alice.service.create_domain(&mine, &Actor::panel(), area(path, title, &[owns])).await.unwrap();
    }
    let filed = |kind: Kind, title: &str, content: &str, domain: &str| SaveRequest { domain: Some(domain.into()), ..note(kind, title, content) };
    let (store, place) = (&alice.service, &mine);
    let saved = move |actor: Actor, request: SaveRequest| async move { store.save(place, &actor, request).await.unwrap().record.summary.id };
    let never = saved(Actor::cli(), filed(Kind::Decision, "Tokens never expire", "Sessions keep their token.", "api/auth")).await;
    let hour = saved(
        Actor::panel(),
        SaveRequest { replaces: Some(never.clone()), ..filed(Kind::Decision, "Tokens expire after an hour", "An hour, then refresh.", "api/auth") },
    )
    .await;
    let rotate = saved(Actor::cli(), filed(Kind::Knowledge, "Refresh tokens rotate on use", "Each refresh mints a new one.", "api/auth")).await;
    let sessions = saved(Actor::cli(), filed(Kind::Knowledge, "Sessions live in SQLite", "One table, keyed by token.", "api/auth")).await;
    let refund = saved(Actor::cli(), filed(Kind::Pitfall, "Retry the refund webhook", "It drops one in fifty.", "api")).await;
    saved(Actor::cli(), filed(Kind::Decision, "Use WAL", "Readers never wait.", "web")).await;
    alice.service.link(&mine, &Actor::cli(), &hour, &sessions, LinkKind::RelatedTo, None).await.unwrap();
    alice.service.link(&mine, &Actor::cli(), &rotate, &hour, LinkKind::DependsOn, None).await.unwrap();
    alice.service.link(&mine, &Actor::cli(), &hour, &refund, LinkKind::Supports, None).await.unwrap();
    let alice_project = alice.service.resolve(&mine).await.unwrap().unwrap().id;

    // The export says what it is: the subtree, the project, who reads it, who stands behind it.
    let scope = ExportScope::Domain { project: alice_project.clone(), domain: "api/auth".into() };
    let text = alice.service.export(&scope).await.unwrap();
    let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(header["scope"], "domain", "{header}");
    assert_eq!((header["project"]["id"].as_str(), header["project"]["name"].as_str()), (Some(alice_project.as_str()), Some("app")));
    assert_eq!((header["domain"].as_str(), header["visibility"].as_str()), (Some("api/auth"), Some("project")));
    assert_eq!((header["notes"].as_u64(), header["confirmed"].as_u64(), header["linksLeftOut"].as_u64()), (Some(4), Some(1), Some(1)));
    assert!(header["exporter"].as_str().is_some_and(|e| e.starts_with("swiss-memory ")), "{header}");
    let ancestor = text.lines().find(|l| l.contains("\"type\":\"domain\"") && l.contains("\"path\":\"api\"")).expect("the parent travels");
    assert!(ancestor.contains("\"revisions\":[]"), "the parent travels hub only: {ancestor}");
    assert!(!text.contains("\"path\":\"web\"") && !text.contains(&refund), "nothing outside the subtree: {text}");
    let whole = alice.service.export(&ExportScope::Project(alice_project.clone())).await.unwrap();
    assert!(whole.lines().next().unwrap().contains("\"scope\":\"project\"") && !whole.lines().next().unwrap().contains("linksLeftOut"));
    let shared = alice.service.export(&ExportScope::Project("global".into())).await.unwrap();
    assert!(shared.lines().next().unwrap().contains("\"visibility\":\"global\""), "{shared}");
    let everything = alice.service.export(&ExportScope::All).await.unwrap();
    assert!(everything.lines().next().unwrap().contains("\"visibility\":\"mixed\""), "{everything}");

    // Bob's store: the same work under its own id, its own `api`, one note the same and one not.
    let bob = Scratch::new("merge-bob");
    let his = at(&bob.repo("app", None));
    let his_api = bob.service.create_domain(&his, &Actor::panel(), area("api", "His API", &["src/api/"])).await.unwrap();
    let his_rotate = save(&bob.service, &his, "Refresh tokens rotate on use", "Rotation is off for now.").await;
    let his_sessions = save(&bob.service, &his, "Sessions live in SQLite", "One table, keyed by token.").await;
    let bob_project = bob.service.resolve(&his).await.unwrap().unwrap().id;
    assert_ne!(bob_project, alice_project);
    let into = |dry_run: bool, strategy: ImportStrategy, project: &str| ImportOptions { dry_run, strategy, into: Some(project.to_string()) };

    let dry = bob.service.import_with(&text, &into(true, ImportStrategy::Skip, &bob_project)).await.unwrap();
    assert_eq!(dry.source.domain.as_deref(), Some("api/auth"), "the report says where the file came from");
    assert_eq!(dry.source.project.as_ref().map(|p| p.id.as_str()), Some(alice_project.as_str()));
    assert_eq!(dry.into.as_deref(), Some(bob_project.as_str()));
    assert!(dry.projects_created.is_empty(), "into lands in a project the store has");
    assert_eq!((dry.domains_created, dry.domains_skipped.clone()), (1, vec!["api".to_string()]));
    assert_eq!((dry.records_created, dry.records_skipped.clone()), (2, vec![sessions.clone()]), "the note both wrote lands once");
    assert_eq!(dry.conflicts.len(), 1, "{:?}", dry.conflicts);
    let conflict = &dry.conflicts[0];
    assert_eq!((conflict.kind, conflict.incoming.id.as_str(), conflict.stored.id.as_str()), (ConflictKind::Title, rotate.as_str(), his_rotate.as_str()));
    assert_eq!((conflict.incoming.excerpt.as_str(), conflict.stored.excerpt.as_str()), ("Each refresh mints a new one.", "Rotation is off for now."));
    assert_eq!((dry.links_created, dry.links_skipped), (2, 1), "a conflict's links stay out with it");
    assert_eq!(bob.service.list(&his, &ListRequest::default()).await.unwrap().0.len(), 2, "a dry run writes nothing");
    let refused = bob.service.import_with(&text, &into(false, ImportStrategy::Fail, &bob_project)).await.unwrap_err();
    assert_eq!(refused.code, Code::SimilarExists);
    assert_eq!(code(bob.service.import_with(&text, &into(true, ImportStrategy::Skip, "global")).await.unwrap_err()), Code::Invalid, "a project's notes never turn global");
    assert_eq!(code(bob.service.import_with(&everything, &into(true, ImportStrategy::Skip, &bob_project)).await.unwrap_err()), Code::Invalid, "into takes one project");

    let merged = bob.service.import_with(&text, &into(false, ImportStrategy::Skip, &bob_project)).await.unwrap();
    assert_eq!((merged.records_created, merged.conflicts.len(), merged.links_created), (2, 1, 2));
    let auth = bob.service.domain(&his, "api/auth", false).await.unwrap();
    assert_eq!(auth.notes.len(), 1, "the live note filed in the new sub-domain: {:?}", auth.notes);
    assert_eq!(bob.service.domain(&his, "api", false).await.unwrap().domain.id, his_api.id, "his map stands");
    let (landed, _) = bob.service.get(&his, &hour, false).await.unwrap();
    assert_eq!(landed.summary.domain.as_deref(), Some("api/auth"));
    let linked: Vec<&str> = landed.links.iter().map(|l| l.id.as_str()).collect();
    assert!(linked.contains(&his_sessions.as_str()) && linked.contains(&never.as_str()), "the twin takes the link: {linked:?}");
    assert_eq!(code(bob.service.get(&his, &rotate, false).await.unwrap_err()), Code::NotFound, "the conflict stayed out");

    // Alice edits after the merge: the next merge names the edit, and Bob's revision stands.
    let edit = UpdateRequest { expected_revision: 1, content: Some("An hour, then refresh; idle sessions after ten minutes.".into()), ..UpdateRequest::default() };
    alice.service.update(&mine, &Actor::cli(), &hour, edit).await.unwrap();
    let again = alice.service.export(&scope).await.unwrap();
    let second = bob.service.import_with(&again, &into(false, ImportStrategy::Skip, &bob_project)).await.unwrap();
    assert_eq!(second.records_created, 0);
    let mut skipped = second.records_skipped.clone();
    skipped.sort();
    let mut expected = vec![never.clone(), sessions.clone()];
    expected.sort();
    assert_eq!(skipped, expected, "the same revision is the same note");
    let revision = second.conflicts.iter().find(|c| c.kind == ConflictKind::Revision).expect("the edit is a conflict");
    assert_eq!((revision.incoming.id.as_str(), revision.incoming.revision, revision.stored.revision), (hour.as_str(), 2, 1));
    assert!(second.conflicts.iter().any(|c| c.kind == ConflictKind::Title), "{:?}", second.conflicts);
    let (kept, _) = bob.service.get(&his, &hour, false).await.unwrap();
    assert_eq!(kept.content, "An hour, then refresh.");
}

// --- concurrency ----------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_concurrency_two_services_share_one_store() {
    let s = Scratch::new("twin");
    let repo = at(&s.repo("app", None));
    let gateway = s.twin();
    let a = save(&s.service, &repo, "From the CLI", "one").await;
    let b = save(&gateway, &repo, "From the gateway", "two").await;
    let (items, _) = s.service.list(&repo, &ListRequest::default()).await.unwrap();
    assert_eq!(items.len(), 2);
    let link = LinkSpec { to: a.clone(), kind: LinkKind::RelatedTo, note: None };
    let mut linked = note(Kind::Knowledge, "Linked at save", "three");
    linked.links = vec![link];
    let c = gateway.save(&repo, &Actor::cli(), linked).await.unwrap().record;
    assert_eq!(c.links.len(), 1);
    let first = UpdateRequest { expected_revision: 1, title: Some("CLI edit".into()), ..UpdateRequest::default() };
    let second = UpdateRequest { expected_revision: 1, title: Some("Gateway edit".into()), ..UpdateRequest::default() };
    s.service.update(&repo, &Actor::cli(), &b, first).await.unwrap();
    assert_eq!(code(gateway.update(&repo, &Actor::cli(), &b, second).await.unwrap_err()), Code::RevisionConflict, "no lost update");
}

// --- a plan the world moved under -------------------------------------------------------------

/// A redraw plan is cut before the run asks its model and lands after: the delete
/// re-derives the redraw's protection inside the write transaction, so a domain a person
/// confirmed while the plan waited is kept - with its revisions - and reported, and so is
/// its parent (SPEC §memory.map Redraw: "a domain a person created, edited or confirmed
/// is never removed, and neither is its parent").
#[tokio::test]
async fn knowledge_redraw_delete_rechecks_the_protection_in_the_transaction() {
    let s = Scratch::new("redraw-guard");
    let repo = at(&s.repo("app", None));
    let sync = swiss_memory::init::init_actor();
    for path in ["src", "src/order", "sql"] {
        s.service.create_domain(&repo, &sync, area(path, path, &[&format!("{path}/")])).await.unwrap();
    }
    let project = s.service.projects().await.unwrap().iter().find(|p| p.id.starts_with("p-")).unwrap().id.clone();
    let plan = s.service.map_redraw_plan(&project).await.unwrap();
    assert_eq!(plan, ["src/order", "sql", "src"], "deepest first, then by path, every domain still machine-only");

    // A person confirms src/order while the run the plan belongs to is asking its model.
    s.service.confirm_domain(&repo, &Actor::panel(), "src/order").await.unwrap();
    let (removed, failed) = s.service.map_redraw(&project, &plan).await;
    assert_eq!(removed, ["sql"], "the untouched domain still goes");
    assert_eq!(failed.len(), 2, "the confirmed domain and its parent are kept and said: {failed:?}");
    let kept: Vec<String> = s.service.domains(&repo).await.unwrap().items.iter().map(|d| d.path.clone()).collect();
    assert_eq!(kept, ["src", "src/order"], "the confirmed domain and its parent survive");
    let detail = s.service.domain(&repo, "src/order", true).await.unwrap();
    assert!(detail.history.as_ref().is_some_and(|h| h.len() >= 2), "the confirmed domain keeps its revision history");
}

/// The edit case of the same protection: one revision by a person makes the domain
/// theirs, and the redraw keeps it even though the plan - cut before the edit - listed it.
#[tokio::test]
async fn knowledge_redraw_delete_keeps_a_domain_a_person_edited() {
    let s = Scratch::new("redraw-edit");
    let repo = at(&s.repo("app", None));
    let sync = swiss_memory::init::init_actor();
    s.service.create_domain(&repo, &sync, area("sql", "SQL", &["sql/"])).await.unwrap();
    let project = s.service.projects().await.unwrap().iter().find(|p| p.id.starts_with("p-")).unwrap().id.clone();
    let plan = s.service.map_redraw_plan(&project).await.unwrap();
    assert_eq!(plan, ["sql"]);

    let edited = s
        .service
        .update_domain(&repo, &Actor::panel(), "sql", DomainUpdate { expected_revision: 1, title: Some("SQL, hand-tuned".into()), ..DomainUpdate::default() })
        .await
        .unwrap();
    assert_eq!(edited.revision, 2);
    let (removed, failed) = s.service.map_redraw(&project, &plan).await;
    assert!(removed.is_empty(), "nothing goes: {removed:?}");
    assert_eq!(failed.len(), 1, "the edited domain is kept and said: {failed:?}");
    let kept: Vec<String> = s.service.domains(&repo).await.unwrap().items.iter().map(|d| d.path.clone()).collect();
    assert_eq!(kept, ["sql"]);
}

/// A recheck's Keep/Merge/Drop land through the same guards a Fix's update already had:
/// the revision its question showed, and the note still judgeable, re-read inside the
/// write transaction - so an answer that returns after the person acted on the note is
/// refused, not applied on stale ground (SPEC §memory.init The recheck: "a person's
/// note, a confirmed one and one a recheck already verified are passed by").
#[tokio::test]
async fn knowledge_recheck_verdicts_guard_the_captured_revision_and_the_scope() {
    let s = Scratch::new("recheck-guard");
    let repo = at(&s.repo("app", None));
    let agent = Actor::mcp(Some("codex"));
    let person = Actor::panel();
    let drop_id = s.service.save(&repo, &agent, note(Kind::Pitfall, "Retry on CI", "Retry flakes twice.")).await.unwrap().record.summary.id;
    let keep_id = s.service.save(&repo, &agent, note(Kind::Decision, "WAL everywhere", "One writer, many readers.")).await.unwrap().record.summary.id;
    let merge_id = s.service.save(&repo, &agent, note(Kind::Knowledge, "Indexes age", "Rebuild them weekly.")).await.unwrap().record.summary.id;

    // While the answers wait, a person acts: the Drop target is confirmed and edited
    // (revision 3), the Keep target is verified by hand (revision 2), the Merge source
    // is confirmed (revision 2).
    s.service.confirm(&repo, &person, &drop_id).await.unwrap();
    let edit = UpdateRequest { expected_revision: 2, content: Some("Retry flakes twice; cap at five.".into()), reason: Some("clarified".into()), ..UpdateRequest::default() };
    s.service.update(&repo, &person, &drop_id, edit).await.unwrap();
    s.service.mark(&repo, &person, &keep_id, Verdict::Verified, Some("checked")).await.unwrap();
    s.service.confirm(&repo, &person, &merge_id).await.unwrap();

    let rechecked = Rechecked {
        verdicts: vec![
            Judged { id: drop_id.clone(), revision: 1, judgement: Judgement::Drop, reason: Some("wrong".into()) },
            Judged { id: keep_id.clone(), revision: 1, judgement: Judgement::Keep, reason: None },
            Judged { id: merge_id.clone(), revision: 1, judgement: Judgement::Merge { into: keep_id.clone() }, reason: Some("same".into()) },
        ],
        ..Rechecked::default()
    };
    let project = s.service.projects().await.unwrap().iter().find(|p| p.id.starts_with("p-")).unwrap().id.clone();
    let mut tally = RecheckScope::default();
    let refusals = s.service.recheck_apply(&project, "run-1", &rechecked, &mut tally).await;

    let (dropped, _) = s.service.get(&repo, &drop_id, false).await.unwrap();
    assert_eq!(dropped.summary.status, Status::Active, "the old Drop must not invalidate a note a person now owns");
    assert!(dropped.summary.confirmed, "the person's confirmation stands");
    assert_eq!(dropped.summary.revision, 3, "the person's edit stands");
    let (merged, _) = s.service.get(&repo, &merge_id, false).await.unwrap();
    assert_eq!(merged.summary.status, Status::Active, "the old Merge must not supersede a confirmed note");
    assert!(merged.summary.confirmed);
    assert_eq!(tally.dropped + tally.kept + tally.merged, 0, "no verdict landed on moved ground: {tally:?}");
    assert_eq!(refusals.len(), 3, "each refusal says why: {refusals:?}");
}

/// A review run (`review --resolve`, SPEC §memory.init The recheck) judges the inbox's
/// notes a model wrote, a confirmed one too: each it leaves verified is confirmed on the
/// person's behalf, and a contradiction whose both sides it kept is lifted - unless the
/// answer links them as contradicting again. A recheck's answer confirms nothing.
#[tokio::test]
async fn knowledge_a_review_run_confirms_what_it_keeps_and_lifts_a_contradiction_it_settles() {
    let s = Scratch::new("review-resolve");
    let repo = at(&s.repo("app", None));
    let agent = Actor::mcp(Some("codex"));
    let person = Actor::panel();
    let save = |title: &'static str, content: &'static str| {
        let (service, repo, agent) = (&s.service, &repo, &agent);
        async move { service.save(repo, agent, note(Kind::Knowledge, title, content)).await.unwrap().record.summary.id }
    };
    let a = save("Refunds go to the wallet", "A refund lands in the wallet, not the card.").await;
    let b = save("Card refunds for card orders", "An order paid by card is refunded to the card.").await;
    let c = save("Retries are capped at three", "The payment client retries three times.").await;
    let d = save("Retries are capped at five", "The payment client retries five times.").await;
    for (from, to) in [(&a, &b), (&c, &d)] {
        s.service.link(&repo, &agent, from, to, LinkKind::Contradicts, None).await.unwrap();
    }
    // A person confirmed one side before handing the inbox over: it is judged all the same.
    let confirmed = s.service.confirm(&repo, &person, &a).await.unwrap().summary.revision;
    let keep = |id: &String, revision: u64| Judged { id: id.clone(), revision, judgement: Judgement::Keep, reason: Some("the code says so".into()) };
    let project = s.service.projects().await.unwrap().iter().find(|p| p.id.starts_with("p-")).unwrap().id.clone();

    // A recheck's reach passes a confirmed note by and confirms nothing.
    let recheck = Rechecked { verdicts: vec![keep(&a, confirmed)], ..Rechecked::default() };
    let mut tally = RecheckScope::default();
    let refusals = s.service.recheck_apply(&project, "run-0", &recheck, &mut tally).await;
    assert_eq!((tally.kept, tally.confirmed, refusals.len()), (0, 0, 1), "{tally:?} {refusals:?}");

    let review = Rechecked {
        verdicts: vec![keep(&a, confirmed), keep(&b, 1), keep(&c, 1), keep(&d, 1)],
        links: vec![Linked { from: d.clone(), to: c.clone(), kind: LinkKind::Contradicts, note: None }],
        reach: Reach::Inbox,
        contradicts: vec![(a.clone(), b.clone()), (c.clone(), d.clone())],
        ..Rechecked::default()
    };
    let mut tally = RecheckScope::default();
    let refusals = s.service.recheck_apply(&project, "run-1", &review, &mut tally).await;
    assert!(refusals.is_empty(), "{refusals:?}");
    assert_eq!((tally.kept, tally.confirmed, tally.lifted), (4, 3, 1), "the confirmed one stays confirmed: {tally:?}");
    for id in [&a, &b, &c, &d] {
        let (view, _) = s.service.get(&repo, id, false).await.unwrap();
        assert!(view.summary.confirmed && view.summary.status == Status::Active, "{id}: {:?}", view.summary);
    }
    assert!(!s.service.unlink(&repo, &a, &b, LinkKind::Contradicts).await.unwrap(), "the settled pair's link is lifted");
    assert!(s.service.unlink(&repo, &c, &d, LinkKind::Contradicts).await.unwrap(), "the pair it linked again stays");
}

