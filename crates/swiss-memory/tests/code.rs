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

//! The code index end to end (SPEC §memory.code, ADR-032 phases 5 and 7): derived from a
//! checkout's files, refreshed by hash, deletable without touching a note, and the check
//! a save runs against it - a note from an agent that names code the checkout does not
//! hold, quotes what its file does not say or counts a set wrong is held for review, a
//! person's is only told, and an agent's edit that passes the check releases it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use swiss_memory::code::{Lister, Listing};
use swiss_memory::service::{ListRequest, SaveOutcome, SaveRequest, UpdateOutcome, UpdateRequest};
use swiss_memory::{Actor, EvidenceSpec, Kind, MemResult, MemoryService, Producer, SourceType, Status, Target, Verdict};

/// Every file under the root but `.git`, like `git ls-files` on an untracked tree.
struct Walk {
    gap: Duration,
}

#[async_trait::async_trait]
impl Lister for Walk {
    async fn list(&self, root: &Path) -> MemResult<Listing> {
        let mut files = Vec::new();
        let mut stack = vec![(root.to_path_buf(), String::new())];
        while let Some((dir, rel)) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir").flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if entry.file_type().expect("type").is_dir() {
                    if name != ".git" {
                        stack.push((entry.path(), child));
                    }
                } else {
                    files.push(child);
                }
            }
        }
        files.sort();
        Ok(Listing { files, truncated: false, git: false })
    }

    fn refresh_gap(&self) -> Duration {
        self.gap
    }
}

struct Scratch {
    dir: PathBuf,
    service: MemoryService,
}

impl Scratch {
    fn new(label: &str, gap: Duration) -> Scratch {
        let dir = std::env::temp_dir().join(format!("swiss-code-{label}-{}-{}", std::process::id(), swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(dir.join("repo").join(".git")).expect("scratch repo");
        let dir = dir.canonicalize().expect("canonical scratch");
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        service.set_lister(Some(Arc::new(Walk { gap })));
        let s = Scratch { dir, service };
        let store = "use crate::fresh::probe;\npub struct Store;\nimpl Store {\n    pub async fn write(&self) {}\n}\n\
                     const SQLITE_BUSY_RETRY: u32 = 0;\n";
        s.write("src/store.rs", store);
        s.write("src/fresh.rs", "pub fn probe() {}\n// Store::write opens BEGIN IMMEDIATE: SQLITE_BUSY is never retried.\n");
        s.write("panel/rail.ts", "import { tr } from './i18n';\nexport function fitRailLabels() {}\n");
        s.write("docs/notes.md", "The `retry_budget` knob caps retries.\n");
        std::fs::write(s.root().join("logo.png"), [0x89u8, b'P', b'N', b'G', 0, 0, 1, 2]).expect("binary");
        s
    }

    fn root(&self) -> PathBuf {
        self.dir.join("repo")
    }

    fn repo(&self) -> Target {
        Target::dir(self.root().display().to_string())
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    fn request(&self, title: &str, content: &str, anchors: &[&str], evidence: Vec<EvidenceSpec>) -> SaveRequest {
        SaveRequest {
            kind: Kind::Knowledge,
            title: title.into(),
            content: content.into(),
            anchors: anchors.iter().map(|a| a.to_string()).collect(),
            refs: Vec::new(),
            keywords: Vec::new(),
            links: Vec::new(),
            replaces: None,
            allow_similar: true,
            domain: None,
            evidence,
        }
    }

    async fn save(&self, actor: &Actor, title: &str, content: &str, anchors: &[&str]) -> SaveOutcome {
        self.service.save(&self.repo(), actor, self.request(title, content, anchors, Vec::new())).await.expect("save")
    }

    async fn edit(&self, actor: &Actor, id: &str, revision: u64, content: &str) -> UpdateOutcome {
        let request = UpdateRequest { expected_revision: revision, content: Some(content.into()), ..UpdateRequest::default() };
        self.service.update_checked(&self.repo(), actor, id, request).await.expect("edit")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn agent() -> Actor {
    Actor::mcp(Some("test"))
}

fn rules(outcome: &SaveOutcome) -> Vec<(&str, &str)> {
    outcome.code.as_ref().map(|c| c.findings.iter().map(|f| (f.rule, f.name.as_str())).collect()).unwrap_or_default()
}

#[tokio::test]
async fn code_index_is_derived_rebuildable_and_hash_aware() {
    let s = Scratch::new("index", Duration::from_secs(60));
    let note = s.save(&agent(), "Store writes", "`Store::write` opens BEGIN IMMEDIATE.", &["src/store.rs"]).await;
    let notes = s.service.list(&s.repo(), &ListRequest::default()).await.expect("list").0.len();

    // Built from the files: every text file read, the binary one listed and skipped.
    let report = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((report.listed, report.read, report.skipped, report.removed), (5, 4, 1, 0), "{report:?}");
    let status = s.service.code_status(&s.repo()).await.expect("status");
    assert!(status.indexed && status.files == 5 && status.bytes > 0, "{status:?}");

    // file, symbol, definition, contains, import, reference.
    let store = s.service.code_find(&s.repo(), "Store").await.expect("find");
    assert_eq!(store.defs.iter().map(|d| (d.path.as_str(), d.kind, d.line)).collect::<Vec<_>>(), [("src/store.rs", "struct", 2)]);
    let busy = s.service.code_find(&s.repo(), "SQLITE_BUSY").await.expect("find");
    assert!(busy.defs.is_empty());
    assert_eq!(busy.refs, ["src/fresh.rs"], "a comment names it: a reference, not a definition");
    assert_eq!((busy.in_comments.as_slice(), busy.in_comments_total), (["src/fresh.rs".to_string()].as_slice(), 1));
    let said = swiss_memory::render::code_hits(&busy);
    assert!(said.ends_with("0 in code, 1 only in a comment\nonly a comment names SQLITE_BUSY in these files; they do not use it:\n  src/fresh.rs"), "{said}");
    let budget = s.service.code_find(&s.repo(), "retry_budget").await.expect("find");
    assert_eq!((budget.refs.as_slice(), budget.in_comments.len()), (["docs/notes.md".to_string()].as_slice(), 0), "a file of no language");
    let file = s.service.code_file(&s.repo(), "src/store.rs").await.expect("file");
    assert_eq!(file.lang.as_deref(), Some("rust"));
    assert_eq!(file.defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Store", "write", "SQLITE_BUSY_RETRY"]);
    assert_eq!(file.imports.iter().map(|i| i.spec.as_str()).collect::<Vec<_>>(), ["crate::fresh::probe"]);
    assert!(file.hash.starts_with("sha256:"), "{}", file.hash);

    // Unchanged files are not read again.
    let again = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((again.read, again.touched, again.unchanged), (0, 0, 5), "{again:?}");

    // A changed file is read again, and only it.
    s.write("src/fresh.rs", "pub fn probe() {}\npub fn baseline() {}\n");
    let changed = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((changed.read, changed.unchanged), (1, 4), "{changed:?}");
    assert_eq!(s.service.code_find(&s.repo(), "baseline").await.expect("find").defs[0].path, "src/fresh.rs");
    assert!(s.service.code_find(&s.repo(), "SQLITE_BUSY").await.expect("find").refs.is_empty(), "the comment is gone");

    // The same bytes under a new mtime: hashed, found equal, not extracted again.
    let path = s.root().join("src/store.rs");
    let later = SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).expect("open").set_modified(later).expect("mtime");
    let touched = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((touched.read, touched.touched), (0, 1), "{touched:?}");

    // A deleted file leaves the index with everything it said.
    std::fs::remove_file(s.root().join("panel/rail.ts")).expect("remove");
    let removed = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!(removed.removed, 1, "{removed:?}");
    assert!(s.service.code_find(&s.repo(), "fitRailLabels").await.expect("find").defs.is_empty());

    // Dropped: the index is gone and every note stays as it was.
    assert_eq!(s.service.code_drop(&s.repo()).await.expect("drop"), 4);
    assert!(!s.service.code_status(&s.repo()).await.expect("status").indexed);
    assert_eq!(s.service.list(&s.repo(), &ListRequest::default()).await.expect("list").0.len(), notes);
    let (record, _) = s.service.get(&s.repo(), &note.record.summary.id, false).await.expect("note survives");
    assert_eq!(record.summary.title, "Store writes");

    // Rebuilt from the files alone.
    let rebuilt = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((rebuilt.listed, rebuilt.read), (4, 3), "{rebuilt:?}");
}

#[tokio::test]
async fn code_index_holds_no_credential() {
    // Synthetic shapes, no real key: a dotted API key (32 hex, a dot, 16 alphanumerics) and a
    // long mixed-case token - both read as code-like names to a plain scan.
    let s = Scratch::new("secret", Duration::from_secs(60));
    s.write(".env", "EMBED_API_KEY=0123456789abcdef0123456789abcdef.AbCdEfGh12345678\n");
    let config = "export const sk_live_key = 'AbCdEfGhIjKlMnOp0123456789';\n\
                  const dotted = '0123456789abcdef0123456789abcdef.AbCdEfGh12345678';\n\
                  const pem = `-----BEGIN RSA PRIVATE KEY-----\nMIIEabc\n-----END RSA PRIVATE KEY-----`;\n\
                  export function afterTheKey() {}\n";
    s.write("src/config.ts", config);
    let report = s.service.code_index(&s.repo()).await.expect("index");
    assert_eq!((report.listed, report.skipped), (7, 2), "the binary and the .env: {report:?}");
    let env = s.service.code_file(&s.repo(), ".env").await.expect("listed");
    assert_eq!((env.skipped.as_deref(), env.defs.len(), env.refs), (Some("sensitive"), 0, 0));
    assert!(!env.hash.starts_with("sha256:"), "never opened, so never hashed: {}", env.hash);
    for token in ["AbCdEfGh12345678", "AbCdEfGhIjKlMnOp0123456789", "EMBED_API_KEY"] {
        let hits = s.service.code_find(&s.repo(), token).await.expect("find");
        assert!(hits.defs.is_empty() && hits.refs.is_empty(), "{token}: {hits:?}");
    }
    // The file is still indexed around the redacted spans, each definition at its own line.
    let config = s.service.code_file(&s.repo(), "src/config.ts").await.expect("file");
    let defs: Vec<(&str, u32)> = config.defs.iter().map(|d| (d.name.as_str(), d.line)).collect();
    assert_eq!(defs, [("sk_live_key", 1), ("dotted", 2), ("pem", 3), ("afterTheKey", 6)]);
}

#[tokio::test]
async fn code_check_holds_a_note_naming_code_that_is_not_there() {
    let s = Scratch::new("check", Duration::from_secs(60));
    s.service.code_index(&s.repo()).await.expect("index");

    // A name no file holds: an agent's note is held for review, and says why.
    let typo = s.save(&agent(), "Retry cap", "`retry_budgett` caps the retries of src/store.rs.", &["src/store.rs"]).await;
    assert_eq!(rules(&typo), [("unknown_name", "retry_budgett")]);
    let check = typo.code.as_ref().expect("checked");
    assert!(check.held && check.index == "checked", "{check:?}");
    assert_eq!(typo.record.summary.status, Status::NeedsReview);
    let (_, revisions) = s.service.get(&s.repo(), &typo.record.summary.id, true).await.expect("get");
    let reason = revisions.expect("history")[0].reason.clone().unwrap_or_default();
    assert!(reason.contains("retry_budgett"), "{reason}");

    // What it names is there: in the anchored file (Store, write), or referenced elsewhere
    // and defined nowhere (SQLITE_BUSY, a SQLite code): nothing to say.
    let held = s
        .save(&agent(), "Store writes", "`Store::write` opens BEGIN IMMEDIATE, so SQLITE_BUSY never needs a retry.", &["src/store.rs"])
        .await;
    assert!(rules(&held).is_empty(), "{:?}", held.code);
    assert_eq!(held.record.summary.status, Status::Active);

    // Defined, but not in the files the note anchors: told where, not held.
    let elsewhere = s.save(&agent(), "Rail captions", "fitRailLabels sizes every caption together.", &["src/store.rs"]).await;
    assert_eq!(rules(&elsewhere), [("elsewhere", "fitRailLabels")]);
    assert_eq!(elsewhere.code.as_ref().expect("checked").findings[0].files, ["panel/rail.ts"]);
    assert_eq!(elsewhere.record.summary.status, Status::Active);

    // An anchor or a path that is not there holds the note; a bare file name is found
    // wherever it lives.
    let gone = s.save(&agent(), "Gone", "The probe lives here.", &["src/gone.rs"]).await;
    assert_eq!(rules(&gone), [("missing_anchor", "src/gone.rs")]);
    assert_eq!(gone.record.summary.status, Status::NeedsReview);
    let path = s.save(&agent(), "Paths", "See src/nowhere.rs and `rail.ts` for the caption code.", &[]).await;
    assert_eq!(rules(&path), [("missing_file", "src/nowhere.rs")]);
    assert_eq!(path.record.summary.status, Status::NeedsReview);

    // A person's note is only told.
    let person = s.save(&Actor::cli_as(Producer::Human), "Budget", "`retry_budgett` caps retries.", &[]).await;
    assert_eq!(rules(&person), [("unknown_name", "retry_budgett")]);
    assert!(!person.code.as_ref().expect("checked").held);
    assert_eq!(person.record.summary.status, Status::Active);

    // The same note again writes nothing and checks nothing.
    let again = s.save(&Actor::cli_as(Producer::Human), "Budget", "`retry_budgett` caps retries.", &[]).await;
    assert!(again.duplicate && again.code.is_none());
}

#[tokio::test]
async fn code_check_without_an_index_checks_anchors_only() {
    let s = Scratch::new("none", Duration::from_secs(60));
    let typo = s.save(&agent(), "Retry cap", "`retry_budgett` caps retries.", &["src/store.rs"]).await;
    let check = typo.code.as_ref().expect("checked");
    assert_eq!(check.index, "none");
    assert!(check.findings.is_empty() && !check.held, "no index: a name is not judged");
    assert_eq!(typo.record.summary.status, Status::Active);
    let gone = s.save(&agent(), "Gone", "The probe lives here.", &["src/gone.rs"]).await;
    assert_eq!(rules(&gone), [("missing_anchor", "src/gone.rs")]);
    assert_eq!(gone.record.summary.status, Status::NeedsReview);
}

#[tokio::test]
async fn code_check_refreshes_a_stale_index_before_it_holds() {
    // An index past its gap is refreshed on a miss, so code written since it was built counts.
    let s = Scratch::new("stale", Duration::ZERO);
    s.service.code_index(&s.repo()).await.expect("index");
    s.write("src/budget.rs", "pub const RETRY_BUDGET_MS: u64 = 500;\n");
    let fresh = s.save(&agent(), "Budget", "RETRY_BUDGET_MS bounds one retry.", &[]).await;
    let check = fresh.code.as_ref().expect("checked");
    assert_eq!(check.index, "refreshed");
    assert!(check.findings.is_empty(), "{check:?}");
    assert_eq!(fresh.record.summary.status, Status::Active);

    // Within its gap the index answers as it is: a name written since is not there yet.
    let s = Scratch::new("recent", Duration::from_secs(3600));
    s.service.code_index(&s.repo()).await.expect("index");
    s.write("src/budget.rs", "pub const RETRY_BUDGET_MS: u64 = 500;\n");
    let held = s.save(&agent(), "Budget", "RETRY_BUDGET_MS bounds one retry.", &[]).await;
    assert_eq!(rules(&held), [("unknown_name", "RETRY_BUDGET_MS")]);
    assert_eq!(held.code.as_ref().expect("checked").index, "checked");
}

#[tokio::test]
async fn a_newer_scan_reads_every_file_again_before_a_find_answers() {
    // A find refreshes an index past its gap; within it the index answers as it is - unless
    // an older scan read it: then each file is read again first, once.
    let s = Scratch::new("rescan", Duration::from_secs(3600));
    s.write("src/main/java/job/PauseJob.java", "public class PauseJob {\n    private void publishStaleNotice(Copy copy) {\n    }\n}\n");
    s.service.code_index(&s.repo()).await.expect("index");
    let defs = |hits: &swiss_memory::code::NameHits| hits.defs.iter().map(|d| (d.path.clone(), d.kind, d.line)).collect::<Vec<_>>();
    let found = s.service.code_find(&s.repo(), "publishStaleNotice").await.expect("find");
    assert_eq!(defs(&found), [("src/main/java/job/PauseJob.java".to_string(), "method", 2)]);

    let before = s.service.code_status(&s.repo()).await.expect("status").defs;

    // The index an older scan built: no method among its facts, the scan's number behind.
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(s.dir.join("code.sqlite3"));
    let pool = sqlx::SqlitePool::connect_with(opts).await.expect("open the index");
    sqlx::raw_sql("DELETE FROM code_defs WHERE kind = 'method'; UPDATE code_meta SET value = '1' WHERE key = 'extractor';")
        .execute(&pool)
        .await
        .expect("age the index");
    pool.close().await;
    let service = MemoryService::at(&s.dir.join("store.sqlite3"));
    service.set_lister(Some(Arc::new(Walk { gap: Duration::from_secs(3600) })));
    assert_eq!(service.code_status(&s.repo()).await.expect("status").defs, before - 1, "the old facts answer until the refresh");
    let found = service.code_find(&s.repo(), "publishStaleNotice").await.expect("find");
    assert_eq!(defs(&found), [("src/main/java/job/PauseJob.java".to_string(), "method", 2)], "read again within its gap");
    let again = service.code_index(&s.repo()).await.expect("index");
    assert_eq!((again.read, again.touched), (0, 0), "each file is read again once: {again:?}");
}

fn cited(path: &str, excerpt: Option<&str>) -> EvidenceSpec {
    let mut spec = EvidenceSpec::new(SourceType::Code);
    spec.path = Some(path.into());
    spec.excerpt = excerpt.map(str::to_string);
    spec
}

#[tokio::test]
async fn code_check_holds_an_excerpt_its_file_does_not_hold() {
    // No index: the evidence is read from the files themselves.
    let s = Scratch::new("excerpt", Duration::from_secs(60));
    let save = |title: &'static str, evidence: Vec<EvidenceSpec>| {
        let request = s.request(title, "Writes open BEGIN IMMEDIATE, so a busy store is never retried.", &["src/store.rs"], evidence);
        async { s.service.save(&s.repo(), &agent(), request).await.expect("save") }
    };

    // As the file has it, whitespace aside, or in parts with an elision between: it holds.
    let exact = save("Exact", vec![cited("src/store.rs", Some("impl Store {\n        pub async fn write(&self) {}"))]).await;
    assert_eq!((rules(&exact), exact.record.summary.status), (vec![], Status::Active));
    let elided = save("Elided", vec![cited("src/store.rs", Some("pub struct Store; ... const SQLITE_BUSY_RETRY: u32 = 0;"))]).await;
    assert_eq!((rules(&elided), elided.record.summary.status), (vec![], Status::Active));
    let bare = save("Bare path", vec![cited("src/store.rs", None)]).await;
    assert_eq!(rules(&bare), []);

    // A quote the file does not hold, or a file that is not there: held, and told why.
    let wrong = save("Wrong quote", vec![cited("src/store.rs", Some("pub async fn write(&self, retries: u32) {}"))]).await;
    assert_eq!(rules(&wrong), [("excerpt_mismatch", "src/store.rs")]);
    assert_eq!(wrong.record.summary.status, Status::NeedsReview);
    let gone = save("Gone file", vec![cited("src/gone.rs", Some("fn gone() {}"))]).await;
    assert_eq!(rules(&gone), [("missing_evidence", "src/gone.rs")]);
    assert_eq!(gone.record.summary.status, Status::NeedsReview);

    // The failed quote backs the text it came with: re-anchoring leaves it held and a verified
    // mark is refused; the right quote, or an edit to the text that leaves the quote behind,
    // releases it.
    let id = wrong.record.summary.id.clone();
    let anchors = Some(vec!["src/store.rs".to_string()]);
    let request = UpdateRequest { expected_revision: 1, anchors: anchors.clone(), ..UpdateRequest::default() };
    let moved = s.service.update_checked(&s.repo(), &agent(), &id, request).await.expect("edit");
    assert_eq!(moved.record.summary.status, Status::NeedsReview);
    let err = s.service.mark(&s.repo(), &agent(), &id, Verdict::Verified, None).await.expect_err("still held");
    assert!(err.message.contains("excerpt_mismatch"), "{}", err.message);
    let quote = vec![cited("src/store.rs", Some("const SQLITE_BUSY_RETRY: u32 = 0;"))];
    let request = UpdateRequest { expected_revision: 2, anchors, evidence: quote, ..UpdateRequest::default() };
    let requoted = s.service.update_checked(&s.repo(), &agent(), &id, request).await.expect("edit");
    assert_eq!(requoted.record.summary.status, Status::Active);
    let gone_id = gone.record.summary.id.clone();
    let request = UpdateRequest {
        expected_revision: 1,
        content: Some("Writes open BEGIN IMMEDIATE; a busy store is not retried.".into()),
        ..UpdateRequest::default()
    };
    let rewritten = s.service.update_checked(&s.repo(), &agent(), &gone_id, request).await.expect("edit");
    assert_eq!(rewritten.record.summary.status, Status::Active);

    // A credential file is never read, so never judged.
    s.write(".env", "TOKEN=x\n");
    let secret = save("Secret", vec![cited(".env", Some("TOKEN=y"))]).await;
    assert_eq!(rules(&secret), []);

    // A person's quote is told, not held.
    let request = s.request("Person", "Writes open BEGIN IMMEDIATE.", &["src/store.rs"], vec![cited("src/store.rs", Some("fn nope()"))]);
    let person = s.service.save(&s.repo(), &Actor::cli_as(Producer::Human), request).await.expect("save");
    assert_eq!(rules(&person), [("excerpt_mismatch", "src/store.rs")]);
    assert_eq!(person.record.summary.status, Status::Active);
}

#[tokio::test]
async fn code_check_recounts_a_claim_over_a_set_or_holds_it() {
    let s = Scratch::new("recount", Duration::from_secs(60));
    s.write("src/queue.rs", "pub fn drain_queue() {}\npub const SLOTS: usize = 64;\n");
    s.write("src/flush.rs", "use crate::queue::drain_queue;\npub fn flush() {\n    drain_queue();\n}\n");
    s.write("src/retry.rs", "fn again() {\n    crate::queue::drain_queue();\n}\n");
    s.write("scripts/lib.ts", "export function main() {}\n");
    s.write("scripts/gen-a.ts", "import { main } from './lib';\nmain();\n");
    s.write("scripts/gen-b.ts", "import { main } from './lib';\nmain();\n");
    s.write("scripts/gen-c.ts", "export const skip = 1;\n");
    s.service.code_index(&s.repo()).await.expect("index");
    let bot = agent();
    let saved = |title: &'static str, content: &'static str, anchors: &'static [&'static str]| s.save(&bot, title, content, anchors);

    // A set named in backticks is counted again: right, it holds; wrong, the files are listed.
    let right = saved("Two generators", "Exactly two `scripts/gen-*.ts` scripts call `main()`.", &["scripts/lib.ts"]).await;
    assert_eq!((rules(&right), right.record.summary.status), (vec![], Status::Active));
    let wrong = saved("Three generators", "All 3 `scripts/gen-*.ts` scripts call `main()`, so none skips it.", &["scripts/lib.ts"]).await;
    assert_eq!(rules(&wrong), [("count_mismatch", "all 3")]);
    assert_eq!(wrong.code.as_ref().expect("checked").findings[0].files, ["scripts/gen-c.ts"]);
    assert_eq!(wrong.record.summary.status, Status::NeedsReview);
    let most = saved("Generators that skip", "Most files under `scripts/` mention `skip`.", &["scripts/lib.ts", "scripts/gen-c.ts"]).await;
    assert_eq!(rules(&most), [("count_mismatch", "most")]);

    // A closed set is counted by its code-like names over the whole index: every file that
    // mentions them and defines none of the claim's names.
    let closed = saved("Drain", "`flush` is the only caller of `drain_queue`.", &["src/flush.rs"]).await;
    assert_eq!(rules(&closed), [("count_mismatch", "the only")]);
    assert_eq!(closed.code.as_ref().expect("checked").findings[0].files, ["src/retry.rs"]);
    let named = saved("Drain callers", "No other file calls `drain_queue` than src/flush.rs and src/retry.rs.", &["src/queue.rs"]).await;
    assert_eq!(rules(&named), [], "a file the note names is not another");
    // A plain name is in no index of mentions: nothing to count it by.
    let plain = saved("Probe", "No other file calls `probe`.", &["src/fresh.rs"]).await;
    assert_eq!(rules(&plain), [("unscoped_quantifier", "no other")]);

    // Nothing to count and nothing scoped: held; scoped, a file, a hedge or a quoted constant: fine.
    let bare = saved("Generators", "Exactly two scripts call main.", &["scripts/lib.ts"]).await;
    assert_eq!(rules(&bare), [("unscoped_quantifier", "exactly two")]);
    assert_eq!(bare.record.summary.status, Status::NeedsReview);
    assert!(bare.formation.is_empty(), "the check says it, not the lint too: {:?}", bare.formation);
    for fine in ["Exactly two calls sit in src/store.rs.", "Most generators observed in `scripts/gen-a.ts` call main."] {
        let found = saved("Scoped", fine, &["scripts/lib.ts"]).await;
        assert_eq!(rules(&found), [], "{fine}");
    }
    let slots = "The queue holds exactly 64 slots, so a burst waits.";
    let unquoted = s.service.save(&s.repo(), &agent(), s.request("Slots", slots, &["src/queue.rs"], Vec::new())).await.expect("save");
    assert_eq!(rules(&unquoted), [("unscoped_quantifier", "exactly 64")]);
    let quote = vec![cited("src/queue.rs", Some("pub const SLOTS: usize = 64;"))];
    let constant = s.service.save(&s.repo(), &agent(), s.request("Slots quoted", slots, &["src/queue.rs"], quote)).await.expect("save");
    assert_eq!(rules(&constant), [], "the quote states the number");
}

#[tokio::test]
async fn an_agent_fixes_its_held_note_by_editing_it() {
    let s = Scratch::new("fix", Duration::from_secs(60));
    s.service.code_index(&s.repo()).await.expect("index");
    let held = s.save(&agent(), "Retry cap", "`retry_budgett` caps the retries of src/store.rs.", &["src/store.rs"]).await;
    let id = held.record.summary.id.clone();
    assert_eq!(held.record.summary.status, Status::NeedsReview);

    // Vouching for it does not release it while the check still fails.
    let err = s.service.mark(&s.repo(), &agent(), &id, Verdict::Verified, None).await.expect_err("still held");
    assert!(err.message.contains("unknown_name retry_budgett") && err.message.contains("update"), "{}", err.message);

    // An edit that still fails keeps it held, and says why.
    let still = s.edit(&agent(), &id, 1, "`retry_budgetx` caps the retries of src/store.rs.").await;
    assert_eq!(still.record.summary.status, Status::NeedsReview);
    assert_eq!(still.code.as_ref().map(|c| c.held), Some(true));

    // An edit that passes releases it; no person confirms anything.
    let fixed = s.edit(&agent(), &id, 2, "`SQLITE_BUSY_RETRY` caps the retries of src/store.rs.").await;
    assert_eq!(fixed.record.summary.status, Status::Active);
    assert!(fixed.code.as_ref().is_some_and(|c| !c.held && c.findings.is_empty()), "{:?}", fixed.code);
    let (_, revisions) = s.service.get(&s.repo(), &id, true).await.expect("get");
    let reasons: Vec<String> = revisions.expect("history").iter().map(|r| r.reason.clone().unwrap_or_default()).collect();
    assert!(reasons[1].contains("held for review") && reasons[2].contains("released"), "{reasons:?}");

    // An agent's edit that breaks a clean note holds it.
    let broken = s.edit(&agent(), &id, 3, "`retry_budgett` caps the retries of src/store.rs.").await;
    assert_eq!(broken.record.summary.status, Status::NeedsReview);

    // A note a person marked outdated is not the check's to release.
    let other = s.save(&agent(), "Store writes", "`Store::write` opens BEGIN IMMEDIATE.", &["src/store.rs"]).await;
    let other_id = other.record.summary.id.clone();
    s.service.mark(&s.repo(), &Actor::cli_as(Producer::Human), &other_id, Verdict::Outdated, Some("the lock changed")).await.expect("mark");
    let edited = s.edit(&agent(), &other_id, 2, "`Store::write` opens BEGIN IMMEDIATE before it writes.").await;
    assert_eq!(edited.record.summary.status, Status::NeedsReview);
}
