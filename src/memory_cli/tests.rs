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

//! The CLI's tests: every command against a scratch home, its exit code and its output, text
//! and --json.

use super::*;
use std::path::Path;

struct Capture(std::sync::Mutex<Vec<String>>);

impl Out for Capture {
    fn line(&self, text: &str) {
        self.0.lock().expect("capture").push(format!("out:{text}"));
    }
    fn err(&self, text: &str) {
        self.0.lock().expect("capture").push(format!("err:{text}"));
    }
}

impl Capture {
    fn new() -> Capture {
        Capture(std::sync::Mutex::new(Vec::new()))
    }

    fn text(&self) -> String {
        self.0.lock().expect("capture").join("\n")
    }

    fn stdout(&self) -> String {
        self.0
            .lock()
            .expect("capture")
            .iter()
            .filter_map(|l| l.strip_prefix("out:"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A scratch home with a Git repo and a store; the service is handed to
/// run_with directly, so no environment variable is touched.
struct Scratch {
    home: PathBuf,
    service: MemoryService,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let home = std::env::temp_dir().join(format!(
            "swiss-memcli-{label}-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(home.join("repo").join(".git")).expect("scratch repo");
        let home = home.canonicalize().expect("canonical home");
        let service = MemoryService::at(&home.join("store.sqlite3"));
        Scratch { home, service }
    }

    fn dir(&self, name: &str) -> String {
        swiss_memory::scope::display(&self.home.join(name))
    }

    async fn run(&self, argv: &[&str]) -> (i32, Capture) {
        let out = Capture::new();
        let exit = run_with(argv.iter().map(|s| s.to_string()).collect(), &out, &self.service).await;
        (exit, out)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn saved_id(out: &Capture) -> String {
    let body: Value = serde_json::from_str(&out.stdout()).expect("save --json answers JSON");
    body["record"]["id"].as_str().expect("id").to_string()
}

#[tokio::test]
async fn knowledge_cli_round_trip_from_a_nested_directory() {
    let scratch = Scratch::new("round");
    let nested = scratch.home.join("repo").join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let nested = swiss_memory::scope::display(&nested);
    let (exit, out) = scratch
        .run(&["save", "--type", "pitfall", "--title", "Deploy via scripts/deploy.ps1", "--json", "--cwd", &nested, "--", "-why: the script owns ordering"])
        .await;
    assert_eq!(exit, 0, "{}", out.text());
    let id = saved_id(&out);

    let (exit, out) = scratch.run(&["search", "deploy", "--cwd", &nested]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains(&id), "{}", out.text());

    let (exit, out) = scratch.run(&["show", &id, "--cwd", &scratch.dir("repo"), "--history"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("-why: the script owns ordering"), "{}", out.text());
    assert!(out.stdout().contains("r1 create"), "{}", out.text());

    let (exit, out) = scratch.run(&["brief", "--cwd", &nested, "--task", "deploy"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains(&id), "{}", out.text());
}

#[tokio::test]
async fn knowledge_cli_exit_codes() {
    let scratch = Scratch::new("exit");
    let repo = scratch.dir("repo");
    let (_, out) = scratch.run(&["save", "--type", "decision", "--title", "Use WAL", "--json", "--cwd", &repo, "readers never block"]).await;
    let id = saved_id(&out);

    let (exit, _) = scratch.run(&["edit", &id, "--revision", "1", "--title", "WAL mode", "--cwd", &repo]).await;
    assert_eq!(exit, 0);
    let (exit, out) = scratch.run(&["edit", &id, "--revision", "1", "--title", "again", "--cwd", &repo]).await;
    assert_eq!(exit, 3, "a stale revision is a conflict: {}", out.text());
    assert!(out.text().contains("current revision 2"), "{}", out.text());

    let (exit, out) = scratch.run(&["save", "--type", "decision", "--title", "wal mode", "--cwd", &repo, "other"]).await;
    assert_eq!(exit, 3, "{}", out.text());
    assert!(out.text().contains("--replaces"), "{}", out.text());

    let (exit, _) = scratch.run(&["mark", &id, "outdated", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "outdated needs a reason");
    let (exit, _) = scratch.run(&["list", "--bogus", "x"]).await;
    assert_eq!(exit, 2, "unknown flags are usage errors");

    // A plain directory is no project; the shared notes work from anywhere.
    std::fs::create_dir_all(scratch.home.join("plain")).expect("plain dir");
    let plain = scratch.dir("plain");
    let (exit, out) = scratch.run(&["save", "--type", "knowledge", "--title", "Orphan", "--cwd", &plain, "x"]).await;
    assert_eq!(exit, 1, "{}", out.text());
    assert!(out.text().contains("memory_not_found"), "{}", out.text());
    let (exit, out) = scratch.run(&["save", "--type", "preference", "--title", "Short answers", "--global", "--cwd", &plain, "stated by the user"]).await;
    assert_eq!(exit, 0, "{}", out.text());
}

/// `code index|status|find|file|drop` (SPEC §memory.code), and what a save says when
/// a note names code the checkout does not hold: held unless a person wrote it.
#[tokio::test]
async fn knowledge_cli_code_index_and_a_held_save() {
    let scratch = Scratch::new("code");
    scratch.service.set_lister(Some(Arc::new(swiss_plugins::memory_preview::CheckoutLister)));
    let repo = scratch.dir("repo");
    std::fs::create_dir_all(scratch.home.join("repo").join("src")).expect("src");
    std::fs::write(scratch.home.join("repo").join("src").join("budget.rs"), "pub const RETRY_BUDGET_MS: u64 = 500;\n")
        .expect("source");

    let (exit, out) = scratch.run(&["code", "status", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("no code index"), "{}", out.text());
    let (exit, out) = scratch.run(&["code", "index", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let report: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!((report["listed"].as_u64(), report["read"].as_u64(), report["git"].as_bool()), (Some(1), Some(1), Some(false)));
    let (exit, out) = scratch.run(&["code", "find", "RETRY_BUDGET_MS", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("const RETRY_BUDGET_MS src/budget.rs:1"), "{}", out.text());
    let (exit, out) = scratch.run(&["code", "file", "budget.rs", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let file: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!((file["path"].as_str(), file["lang"].as_str()), (Some("src/budget.rs"), Some("rust")));

    let note = ["save", "--type", "knowledge", "--title", "Retry budget", "--cwd", &repo, "retry_budgett caps one retry."];
    let (exit, out) = scratch.run(&note).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("code (unknown_name): retry_budgett"), "{}", out.text());
    let id = out.stdout().split_whitespace().nth(1).expect("saved ID").to_string();
    let accept = format!("held for review (needs_review): fix it with `swiss memory edit {id} --revision 1`");
    assert!(out.stdout().contains(&accept), "{}", out.text());
    let (exit, out) = scratch.run(&["list", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("needs_review"), "{}", out.text());
    let person = ["save", "--type", "knowledge", "--title", "Budget", "--producer", "human", "--cwd", &repo, "retry_budgett caps it."];
    let (exit, out) = scratch.run(&person).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("code (unknown_name)") && !out.stdout().contains("held"), "{}", out.text());

    let (exit, out) = scratch.run(&["code", "status", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("1 files (0 skipped), 1 definitions"), "{}", out.text());
    let (exit, out) = scratch.run(&["code", "drop", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("dropped the code index of 1 files; no note changed"), "{}", out.text());
    let (exit, out) = scratch.run(&["list", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("Retry budget") && out.stdout().contains("Budget"), "{}", out.text());
    let (exit, out) = scratch.run(&["code", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "{}", out.text());
    assert!(out.text().contains("code index | status"), "{}", out.text());
}

/// A shell cannot tell a person from an agent: `--producer` declares who wrote a note, and
/// `--evidence` says what it rests on (ADR-032); `show` prints both.
#[tokio::test]
async fn knowledge_cli_provenance_and_evidence() {
    let scratch = Scratch::new("evidence");
    let repo = scratch.dir("repo");
    let (exit, out) = scratch
        .run(&[
            "save", "--type", "decision", "--title", "Retries reuse the key", "--producer", "bootstrap", "--evidence",
            "bootstrap:run-1", "--evidence", "code:src/retry.rs#retry", "--json", "--cwd", &repo, "So a replay is idempotent.",
        ])
        .await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    let record = &body["record"];
    assert_eq!((record["producer"].as_str(), record["transport"].as_str()), (Some("bootstrap"), Some("cli")));
    assert_eq!(record["evidence"].as_array().map(Vec::len), Some(2), "{record}");
    assert_eq!(record["sourceVerified"], json!(false), "derived from is not verified by");
    let id = saved_id(&out);

    let (exit, out) = scratch.run(&["save", "--type", "decision", "--title", "X", "--producer", "sync", "--cwd", &repo, "y"]).await;
    assert_eq!(exit, 2, "sync is the product's own producer: {}", out.text());
    assert!(out.text().contains("human | agent | bootstrap | import"), "{}", out.text());
    let (exit, out) = scratch.run(&["save", "--type", "decision", "--title", "X", "--evidence", "code:src/a.rs:12", "--cwd", &repo, "y"]).await;
    assert_eq!(exit, 2, "{}", out.text());
    assert!(out.text().contains("line number"), "{}", out.text());
    let (exit, out) = scratch.run(&["save", "--type", "decision", "--title", "X", "--evidence", "frob:x", "--cwd", &repo, "y"]).await;
    assert_eq!(exit, 2, "{}", out.text());
    assert!(out.text().contains("--evidence [RELATION=]TYPE"), "{}", out.text());

    let (exit, out) = scratch.run(&["mark", &id, "outdated", "--reason", "moved", "--evidence", "code:src/a.rs", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "evidence backs a verified verdict only: {}", out.text());
    let (exit, out) = scratch.run(&["mark", &id, "verified", "--evidence", "test:tests/retry.rs#reuses_the_key", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = scratch.run(&["edit", &id, "--evidence", "session:s-1", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "evidence alone needs no revision: {}", out.text());
    assert!(out.stdout().contains("revision 2"), "evidence alone writes no revision: {}", out.text());

    let (exit, out) = scratch.run(&["show", &id, "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let text = out.stdout();
    assert!(text.contains("by bootstrap via cli (cli)") && text.contains("source-verified"), "{text}");
    assert!(text.contains("- verified_by test tests/retry.rs#reuses_the_key (rev 2)"), "{text}");
    assert!(text.contains("- derived_from code src/retry.rs#retry (rev 1)"), "{text}");
    assert!(text.contains("- derived_from session s-1 (rev 2)"), "{text}");
}

/// `rules` prints what a note is (ADR-032) for any writer outside swiss; `lint` checks a
/// note's shape without saving it, and a save prints the same hints.
#[tokio::test]
async fn knowledge_cli_rules_and_formation_lint() {
    let scratch = Scratch::new("rules");
    let (exit, out) = scratch.run(&["rules"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert_eq!(out.stdout(), swiss_memory::formation::NOTE_RULES.trim_end());
    let (exit, out) = scratch.run(&["rules", "--bootstrap"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().starts_with("## Notes") && out.stdout().contains("There is no quota"), "{}", out.text());

    let chain = "Calls: `a()` -> `b()` -> `c()` -> `d()`. Exactly two calls block.";
    let (exit, out) = scratch.run(&["lint", "--type", "knowledge", "--title", "Calls", chain, "--json"]).await;
    assert_eq!(exit, 0, "advisory, never a failure: {}", out.text());
    let body: serde_json::Value = serde_json::from_str(&out.stdout()).expect("json");
    let rules: Vec<&str> = body["formation"].as_array().expect("array").iter().filter_map(|l| l["rule"].as_str()).collect();
    assert_eq!(rules, ["call_chain", "quantifier"]);
    let (exit, out) = scratch.run(&["lint", "--type", "decision", "--title", "Fine", "A full queue blocks the producer."]).await;
    assert_eq!((exit, out.stdout()), (0, "the note reads as one claim".to_string()));
    let (exit, out) = scratch.run(&["lint", "--type", "preference", "--title", "Short answers", "Answers stay short."]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().starts_with("hint (user_words): a preference is the user's rule"), "{}", out.text());

    let repo = scratch.dir("repo");
    let (exit, out) = scratch.run(&["save", "--type", "knowledge", "--title", "Calls", "--cwd", &repo, chain]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("\nhint (call_chain): a call chain retells the code"), "{}", out.text());
    // The CLI may be an agent with a shell: its preference quotes the user.
    let (exit, out) = scratch.run(&["save", "--type", "preference", "--title", "Short answers", "--cwd", &repo, "Answers stay short."]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("\nhint (user_words): "), "{}", out.text());
}

#[tokio::test]
async fn knowledge_cli_links_and_graph() {
    let scratch = Scratch::new("graph");
    let repo = scratch.dir("repo");
    let (_, a) = scratch.run(&["save", "--type", "knowledge", "--title", "Payment key", "--json", "--cwd", &repo, "one key per order"]).await;
    let a = saved_id(&a);
    let link = format!("depends_on:{a}");
    let (exit, b) = scratch
        .run(&["save", "--type", "pitfall", "--title", "Retries double charge", "--link", &link, "--json", "--cwd", &repo, "without the key"])
        .await;
    assert_eq!(exit, 0, "{}", b.text());
    let b = saved_id(&b);

    let (exit, out) = scratch.run(&["graph", &a, "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains(&format!("{b} → {a} (depends_on)")), "{}", out.text());
    let (_, out) = scratch.run(&["related", &a, "--cwd", &repo]).await;
    assert!(out.stdout().contains(&b), "{}", out.text());

    let (exit, out) = scratch.run(&["unlink", &b, "depends_on", &a, "--cwd", &repo]).await;
    assert_eq!((exit, out.stdout().starts_with("unlinked")), (0, true), "{}", out.text());
}

#[tokio::test]
async fn knowledge_cli_hook_never_fails_the_session() {
    let scratch = Scratch::new("hook");
    let out = Capture::new();
    let argv: Vec<String> = ["brief", "--hook"].iter().map(|s| s.to_string()).collect();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(json!({ "cwd": "relative/dir", "session_id": "x" }).to_string());
    assert_eq!(hook(&argv, &out, &scratch.service, rx).await, 0);
    assert!(out.stdout().is_empty(), "nothing on stdout on failure: {}", out.text());

    let out = Capture::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(json!({ "cwd": scratch.dir("repo") }).to_string());
    assert_eq!(hook(&argv, &out, &scratch.service, rx).await, 0);
    assert!(out.stdout().contains("Not a registered project"), "the stdin cwd is used: {}", out.text());

    let (exit, out) = scratch.run(&["hook", "print", "claude-code"]).await;
    assert_eq!(exit, 0);
    assert!(out.stdout().contains("swiss memory brief --hook"));
    // The session's end nudges the sync, so a conversation is read without a commit.
    assert!(out.stdout().contains("\"SessionEnd\"") && out.stdout().contains("swiss memory sync --nudge"));
    let snippet = out.stdout();
    let json_part = &snippet[snippet.find('{').unwrap()..=snippet.rfind('}').unwrap()];
    let parsed: Value = serde_json::from_str(json_part).expect("the snippet's JSON parses");
    assert!(parsed["hooks"]["SessionEnd"].is_array());
}

#[tokio::test]
async fn knowledge_cli_projects_and_transfer() {
    let scratch = Scratch::new("projects");
    let (exit, out) = scratch.run(&["project", "add", "Shared", "docs", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    let id = body["project"]["id"].as_str().expect("id").to_string();
    assert_eq!(body["project"]["name"], "Shared docs");

    let repo = scratch.dir("repo");
    let (exit, out) = scratch.run(&["project", "bind", "--id", &id, "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (_, _) = scratch.run(&["save", "--type", "knowledge", "--title", "Bound note", "--cwd", &repo, "lands in Shared docs"]).await;

    let file = scratch.home.join("export.jsonl");
    let file_text = swiss_memory::scope::display(&file);
    let (exit, out) = scratch.run(&["export", "--cwd", &repo, "--output", &file_text]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(std::fs::read_to_string(Path::new(&file)).expect("export file").contains("Bound note"));
    let (exit, _) = scratch.run(&["export", "--cwd", &repo, "--output", &file_text]).await;
    assert_eq!(exit, 1, "export never overwrites");

    let (exit, out) = scratch.run(&["import", &file_text, "--dry-run", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let report: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(report["recordsSkipped"].as_array().map(Vec::len), Some(1), "{report}");
    assert_eq!(report["source"]["project"]["name"], "Shared docs", "the report echoes the header: {report}");

    // One domain travels to another project: the header says so, and a title the
    // receiving project holds with other text comes back as a conflict.
    let (exit, out) = scratch.run(&["domain", "add", "api", "--title", "API", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = scratch.run(&["save", "--type", "decision", "--title", "Filed note", "--domain", "api", "--cwd", &repo, "in the api domain"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let part = scratch.home.join("api.jsonl");
    let part_text = swiss_memory::scope::display(&part);
    let (exit, out) = scratch.run(&["export", "--cwd", &repo, "--domain", "api", "--output", &part_text]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let exported = std::fs::read_to_string(&part).expect("domain export");
    assert!(exported.lines().next().is_some_and(|h| h.contains("\"scope\":\"domain\"")) && !exported.contains("Bound note"), "{exported}");
    let (exit, _) = scratch.run(&["export", "--all", "--domain", "api"]).await;
    assert_eq!(exit, 2, "--domain takes one project");
    let theirs = Scratch::new("projects-theirs");
    let other = theirs.dir("other");
    std::fs::create_dir_all(Path::new(&other).join(".git")).expect("other repo");
    let (exit, out) = theirs.run(&["save", "--type", "decision", "--title", "Filed note", "--cwd", &other, "other text here"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = theirs.run(&["import", &part_text, "--into", ".", "--cwd", &other]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let said = out.stdout();
    assert!(said.contains(&format!("from: domain api of Shared docs ({id})")) && said.contains("into: p-"), "{said}");
    assert!(said.contains("1 conflicts") && said.contains("has the title of") && said.contains("other text here"), "{said}");
    let nowhere = theirs.dir("nowhere");
    std::fs::create_dir_all(&nowhere).expect("nowhere dir");
    let (exit, _) = theirs.run(&["import", &part_text, "--into", ".", "--cwd", &nowhere]).await;
    assert_eq!(exit, 2, "--into . needs a registered project here");

    let (exit, out) = scratch.run(&["project", "delete", &id, "--confirm", &id]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("2 notes"), "{}", out.text());
}

#[tokio::test]
async fn knowledge_cli_misses() {
    let scratch = Scratch::new("misses");
    let repo = scratch.dir("repo");
    let (_, out) = scratch
        .run(&["save", "--type", "decision", "--title", "Retries reuse the idempotency key", "--json", "--cwd", &repo, "A retried charge sends the same key."])
        .await;
    let wanted = saved_id(&out);
    let (_, out) = scratch
        .run(&["save", "--type", "knowledge", "--title", "Support answers timeouts", "--json", "--cwd", &repo, "A customer timeout goes to support."])
        .await;
    let wrong = saved_id(&out);

    let (exit, out) = scratch
        .run(&["misses", "add", "customer billed twice", "--wanted", &wanted, "--wrong", &wrong, "--via", "search", "--json", "--cwd", &repo])
        .await;
    assert_eq!(exit, 0, "{}", out.text());
    let miss: Value = serde_json::from_str(&out.stdout()).expect("json");
    let miss_id = miss["id"].as_str().expect("id").to_string();
    assert_eq!(miss["actor"], "cli");
    let (exit, out) = scratch.run(&["misses", "add", "key rotation", "--want", "how the signing key rotates", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = scratch.run(&["misses", "add", "x", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "nothing wanted is a validation error: {}", out.text());

    let (exit, out) = scratch.run(&["misses", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains(&miss_id) && out.stdout().contains("wanted: how the signing key rotates"), "{}", out.text());

    let file = scratch.home.join("misses.json");
    let file_text = swiss_memory::scope::display(&file);
    let (exit, out) = scratch.run(&["misses", "export", "--output", &file_text, "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.text().contains("1 queries") && out.text().contains("gap "), "{}", out.text());
    let fixture = swiss_memory::eval::Fixture::parse(&std::fs::read_to_string(&file).expect("export file")).expect("a fixture");
    assert_eq!(fixture.queries[0].expect, std::slice::from_ref(&wanted));
    let (exit, _) = scratch.run(&["misses", "export", "--output", &file_text, "--cwd", &repo]).await;
    assert_eq!(exit, 1, "export never overwrites");

    let (exit, _) = scratch.run(&["misses", "drop", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "ids or --all");
    let (exit, out) = scratch.run(&["misses", "drop", &miss_id, "--cwd", &repo]).await;
    assert_eq!((exit, out.stdout().contains("dropped 1")), (0, true), "{}", out.text());
    let (exit, out) = scratch.run(&["misses", "drop", "--all", "--cwd", &repo]).await;
    assert_eq!((exit, out.stdout().contains("dropped 1")), (0, true), "{}", out.text());
    let (_, out) = scratch.run(&["misses", "--cwd", &repo]).await;
    assert!(out.stdout().contains("no misses recorded"), "{}", out.text());
}

/// `init --map --plan` (SPEC §memory.map): the areas the checkout would be cut into,
/// offline - no model, no gateway; the map's own flags need --map.
#[tokio::test]
async fn knowledge_cli_init_map_plan() {
    let scratch = Scratch::new("map");
    scratch.service.set_lister(Some(Arc::new(swiss_plugins::memory_preview::CheckoutLister)));
    let repo = scratch.dir("repo");
    let src = scratch.home.join("repo").join("src");
    for (area, files) in [("billing", ["invoice", "refund", "tax"]), ("orders", ["cart", "checkout", "order"])] {
        std::fs::create_dir_all(src.join(area)).expect("dir");
        for file in files {
            std::fs::write(src.join(area).join(format!("{file}.rs")), format!("pub fn {file}() {{}}\n")).expect("source");
        }
    }
    let (exit, out) = scratch.run(&["init", "--wait", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "{}", out.text());
    assert!(out.text().contains("go with --map, --full or --recheck"), "{}", out.text());
    let (exit, out) = scratch.run(&["init", "--map", "--plan", "--lang", "zh", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "--plan asks no model: {}", out.text());
    let (exit, out) = scratch.run(&["init", "--map", "--plan", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("registered"), "init still registers: {}", out.text());
    assert!(out.stdout().contains("billing") && out.stdout().contains("orders"), "{}", out.text());
    assert!(out.stdout().contains("2 area(s) to describe"), "{}", out.text());
    let (exit, out) = scratch.run(&["init", "--map", "--plan", "--json", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!((body["asks"].as_u64(), body["codeFiles"].as_u64()), (Some(2), Some(6)));
    assert_eq!(body["plan"][0]["owns"], json!(["src/billing"]));
    // Nothing was written: the plan is a preview.
    let (_, out) = scratch.run(&["domain", "list", "--cwd", &repo]).await;
    assert!(!out.stdout().contains("billing"), "{}", out.text());

    // A redraw sets aside only what an earlier map drew alone: a person's domain stays.
    let (exit, _) = scratch.run(&["domain", "add", "billing", "--title", "Billing", "--owns", "src/billing", "--cwd", &repo]).await;
    assert_eq!(exit, 0);
    let (exit, out) = scratch.run(&["init", "--map", "--plan", "--redraw", "--json", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["removed"], json!([]), "{body}");
    assert_eq!(body["asks"].as_u64(), Some(1), "billing is drawn; orders is left: {body}");

    // The full run's flags: refused before anything reaches the gateway.
    for (args, says) in [
        (&["init", "--map", "--full"][..], "pick one"),
        (&["init", "--full", "--recheck"][..], "pick one"),
        (&["init", "--map", "--max-asks", "3"][..], "--max-asks goes with --full or --recheck"),
        (&["init", "--full", "--max-asks", "0"][..], "at least 1"),
        (&["init", "--recheck", "--max-asks", "0"][..], "at least 1"),
        (&["init", "--full", "--plan", "--wait"][..], "--plan asks no model"),
        (&["init", "--recheck", "--plan"][..], "a recheck draws no map"),
        (&["init", "--recheck", "--redraw"][..], "a recheck draws no map"),
        (&["init", "--redraw"][..], "go with --map, --full or --recheck"),
        (&["init", "--agent", "glm"][..], "go with --map, --full or --recheck"),
        (&["init", "--recheck", "--lead", "big"][..], "--lead goes with --map or --full"),
        (&["init", "--map", "--plan", "--agent", "glm"][..], "--plan asks no model"),
        (&["init", "--map", "--agent", "Not A Name"][..], "--agent names llm agents"),
        (&["init", "--full", "--agent", "glm,glm"][..], "twice"),
        (&["init", "--full", "--agent", "inherit"][..], "leaving --agent out"),
        (&["init", "--map", "--lead", "a,b"][..], "names one agent"),
        (&["review", "--wait"][..], "go with --resolve"),
        (&["review", "--resolve", "--max-asks", "0"][..], "at least 1"),
        (&["review", "--resolve", "--lead", "big"][..], "review takes no --lead"),
        (&["review", "--resolve", "--agent", "Not A Name"][..], "--agent names llm agents"),
    ] {
        let mut args = args.to_vec();
        args.extend(["--cwd", repo.as_str()]);
        let (exit, out) = scratch.run(&args).await;
        assert_eq!(exit, 2, "{args:?}: {}", out.text());
        assert!(out.text().contains(says), "{args:?}: {}", out.text());
    }
}

/// `--agent` and `--lead` reach a run's body as the API takes them (SPEC §memory.settings).
#[test]
fn the_agent_flags_reach_the_body_as_the_api_takes_them() {
    let argv = |words: &[&str]| parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>()).ok().expect("parsed");
    let body = run_body(&argv(&["init", "--full", "--agent", " glm, big ", "--lead", "planner"]), "p-1", "full", None);
    let body = body.ok().expect("a body");
    assert_eq!((&body["agents"], &body["lead"]), (&json!(["glm", "big"]), &json!("planner")));
    let body = run_body(&argv(&["init", "--map", "--agent", "off"]), "p-1", "map", None).ok().expect("a body");
    assert_eq!(body["agents"], json!("off"));
    assert!(body.get("lead").is_none(), "left out, the lead inherits: {body}");
    let body = run_body(&argv(&["init", "--map"]), "p-1", "map", None).ok().expect("a body");
    assert!(body.get("agents").is_none(), "left out, the agents inherit: {body}");
    // Only a project's settings take inherit, as null: a run inherits by leaving it out.
    assert_eq!(choice_flag("agent", "inherit", false, true).ok(), Some(Value::Null));
    assert!(choice_flag("agent", "inherit", false, false).is_err());
    assert_eq!(choice_flag("lead", " off ", true, false).ok(), Some(json!("off")));
}

/// `project settings` reads a project's own settings and patches what it is given
/// (SPEC §memory.settings).
#[tokio::test]
async fn knowledge_cli_project_settings() {
    let scratch = Scratch::new("settings");
    let repo = scratch.dir("repo");
    let (exit, out) = scratch.run(&["project", "settings", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "{}", out.text());
    assert!(out.text().contains("not a registered project"), "{}", out.text());
    let (exit, out) = scratch.run(&["init", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());

    let (exit, out) = scratch.run(&["project", "settings", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"], json!({ "agents": null, "lead": null, "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
    let id = body["project"].as_str().expect("id").to_string();

    let (exit, out) = scratch.run(&["project", "settings", "--cwd", &repo, "--agent", "glm,big", "--lead", "planner", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"], json!({ "agents": ["glm", "big"], "lead": "planner", "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
    // A patch keeps what it leaves out; inherit hands a field back to the layer below.
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--lead", "inherit", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"], json!({ "agents": ["glm", "big"], "lead": null, "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null }), "{body}");
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--agent", "off"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("agents: off") && out.stdout().contains("lead: inherit"), "{}", out.text());
    // The embedding switch: on, off, inherit (SPEC §memory.embedding).
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--embedding", "off", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"]["embedding"], json!(false), "{body}");
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("embedding: off"), "{}", out.text());
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--embedding", "inherit"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("embedding: inherit"), "{}", out.text());
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--embedding", "maybe"]).await;
    assert_eq!(exit, 2, "{}", out.text());
    // The daily review of the changed notes: a time of day, or off.
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review", "03:30", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"]["autoReview"], json!("03:30"), "{body}");
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("auto-review: daily at 03:30"), "{}", out.text());
    assert!(out.stdout().contains("up to 3 runs (the default) a day"), "{}", out.text());
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review", "3:30"]).await;
    assert_eq!(exit, 2, "{}", out.text());
    // The day's runs while one leaves questions for the next: 1 to 10, or the default.
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review-runs", "5", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"]["autoReviewRuns"], json!(5), "{body}");
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id]).await;
    assert!(exit == 0 && out.stdout().contains("up to 5 runs a day"), "{}", out.text());
    for bad in ["0", "11", "many"] {
        let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review-runs", bad]).await;
        assert_eq!(exit, 2, "{bad}: {}", out.text());
    }
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review-runs", "default", "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!(body["settings"]["autoReviewRuns"], Value::Null, "{body}");
    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--auto-review", "off"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("auto-review: off"), "{}", out.text());
    let (exit, out) = scratch.run(&["project", "settings", "--id", swiss_memory::GLOBAL_PROJECT_ID, "--auto-review", "03:30"]).await;
    assert_ne!(exit, 0, "{}", out.text());
    assert!(out.text().contains("nothing to review"), "{}", out.text());

    let (exit, out) = scratch.run(&["project", "settings", "--id", &id, "--agent", "Not A Name"]).await;
    assert_eq!(exit, 2, "refused before the store: {}", out.text());
    let (exit, out) = scratch.run(&["project", "settings", "--id", swiss_memory::GLOBAL_PROJECT_ID, "--agent", "glm"]).await;
    assert_ne!(exit, 0, "{}", out.text());
    assert!(out.text().contains("names no agents"), "{}", out.text());
}

/// `docs` names the folders whose documents a project keeps, and searches and reads what
/// a docs run kept (SPEC §memory.docs).
#[tokio::test]
async fn knowledge_cli_docs() {
    let scratch = Scratch::new("docs");
    let repo = scratch.dir("repo");
    let skill = scratch.home.join("repo/.claude/skills/logic");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(skill.join("stats-job.md"), "# Stats jobs\n## Inactive copy check\nPause a copy after 30 idle days.\n")
        .expect("doc");
    let (exit, out) = scratch.run(&["init", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let project = serde_json::from_str::<Value>(&out.stdout()).expect("json")["project"]["id"].as_str().expect("id").to_string();

    let (exit, out) = scratch.run(&["docs", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("keeps no documents"), "{}", out.text());
    let (exit, out) = scratch.run(&["docs", "add", ".claude/skills/logic/", "docs", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("keeps the documents of: .claude/skills/logic, docs"), "{}", out.text());
    let (exit, out) = scratch.run(&["docs", "remove", "docs", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert_eq!(serde_json::from_str::<Value>(&out.stdout()).expect("json")["docs"], json!([".claude/skills/logic"]));
    let (exit, out) = scratch.run(&["project", "settings", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("docs: .claude/skills/logic"), "{}", out.text());
    for bad in [
        vec!["docs", "add", "../outside", "--cwd", &repo],
        vec!["docs", "remove", "never-named", "--cwd", &repo],
        vec!["docs", "add", "--cwd", &repo],
        vec!["docs", "search", "idle", "--kind", "roadmap", "--cwd", &repo],
        vec!["docs", "bogus", "--cwd", &repo],
    ] {
        let (exit, out) = scratch.run(&bad).await;
        assert_eq!(exit, 2, "{bad:?}: {}", out.text());
    }

    // What a docs run's read keeps, the CLI searches and reads.
    scratch.service.docs_read(&project, false).await.expect("read");
    let (exit, out) = scratch.run(&["docs", "list", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("- [no card yet] .claude/skills/logic/stats-job.md - Stats jobs"), "{}", out.text());
    let (exit, out) = scratch.run(&["docs", "search", "idle", "days", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains(".claude/skills/logic/stats-job.md:2 › Stats jobs › Inactive copy check"), "{}", out.text());
    let (exit, out) = scratch.run(&["docs", "read", "stats-job.md", "--section", "1", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("30 idle days"), "{}", out.text());
    let (exit, out) = scratch.run(&["docs", "read", "nowhere.md", "--cwd", &repo]).await;
    assert_ne!(exit, 0, "{}", out.text());
}

#[tokio::test]
async fn knowledge_cli_domains() {
    let scratch = Scratch::new("domains");
    let repo = scratch.dir("repo");
    let (exit, out) = scratch.run(&["domain", "add", "app", "--title", "App", "--owns", "src", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    assert_eq!((body["domain"]["path"].as_str(), body["domain"]["confirmed"].as_bool()), (Some("app"), Some(false)));
    let (exit, out) = scratch.run(&["domain", "add", "app/api", "--title", "API", "--owns", "src/api", "--keywords", "route，路由、 endpoint", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (_, out) = scratch.run(&["domain", "show", "app/api", "--cwd", &repo]).await;
    assert!(out.stdout().contains("Keywords: route, 路由, endpoint"), "either comma splits: {}", out.text());
    let (exit, _) = scratch.run(&["domain", "add", "app", "--title", "Again", "--cwd", &repo]).await;
    assert_eq!(exit, 3, "a taken path is a conflict");

    // A note is filed by its anchors; the answer says where.
    let (exit, out) = scratch
        .run(&["save", "--type", "pitfall", "--title", "Handlers validate first", "--anchor", "src/api/orders.rs", "--cwd", &repo, "x"])
        .await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("filed in app/api"), "{}", out.text());
    let (_, out) = scratch.run(&["save", "--type", "knowledge", "--title", "Loose note", "--json", "--cwd", &repo, "y"]).await;
    let loose = saved_id(&out);

    let (exit, out) = scratch.run(&["domain", "list", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("`app/api` API") && out.stdout().contains("Unfiled: 1"), "{}", out.text());
    let (_, out) = scratch.run(&["list", "--domain", "app", "--cwd", &repo]).await;
    assert!(out.stdout().contains("Handlers validate first") && !out.stdout().contains("Loose note"), "{}", out.text());
    let (_, out) = scratch.run(&["list", "--domain", "none", "--cwd", &repo]).await;
    assert!(out.stdout().contains("Loose note") && !out.stdout().contains("Handlers"), "{}", out.text());
    let (_, out) = scratch.run(&["search", "handlers", "--domain", "app/api", "--cwd", &repo]).await;
    assert!(out.stdout().contains("Handlers validate first"), "{}", out.text());

    // Confirmed whole: the domain, the one under it and the note filed there; the anchored
    // note names a file the checkout lacks, so the code check held it and it waits.
    let (exit, out) = scratch.run(&["save", "--type", "knowledge", "--title", "Errors are JSON", "--domain", "app/api", "--cwd", &repo, "z"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = scratch.run(&["domain", "confirm", "app", "--all", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("app confirmed with what is under it - domains: 2, notes: 1"), "{}", out.text());
    assert!(out.stdout().contains("1 need review first"), "{}", out.text());
    let (_, out) = scratch.run(&["list", "--unconfirmed", "--cwd", &repo]).await;
    let left = out.stdout();
    assert!(left.contains("Loose note") && left.contains("Handlers") && !left.contains("Errors are JSON"), "{}", out.text());

    // Filing needs no revision; a content edit still does.
    let (exit, out) = scratch.run(&["edit", &loose, "--domain", "app", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, _) = scratch.run(&["edit", &loose, "--title", "Renamed", "--cwd", &repo]).await;
    assert_eq!(exit, 2);
    let (exit, out) = scratch.run(&["domain", "file", &loose, "--none", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("\"moved\": 1"), "{}", out.text());

    // Move carries the subtree; delete hands the notes to the parent.
    let (exit, out) = scratch.run(&["domain", "edit", "app", "--revision", "2", "--path", "core", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (exit, out) = scratch.run(&["domain", "show", "core/api", "--history", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("Handlers validate first") && out.stdout().contains("moved with app"), "{}", out.text());
    let (exit, _) = scratch.run(&["domain", "delete", "core", "--confirm", "core", "--cwd", &repo]).await;
    assert_eq!(exit, 2, "sub-domains first");
    let (exit, out) = scratch.run(&["domain", "delete", "core/api", "--confirm", "core/api", "--cwd", &repo]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("2 notes moved to core"), "{}", out.text());
    let (exit, out) = scratch.run(&["domain", "confirm", "core", "--cwd", &repo, "--json"]).await;
    assert_eq!(exit, 0, "{}", out.text());
    assert!(out.stdout().contains("\"confirmed\": true"), "{}", out.text());
    let (exit, out) = scratch.run(&["domain", "autofile", "--cwd", &repo]).await;
    assert_eq!((exit, out.stdout()), (0, "0 notes filed by their anchors".to_string()));

    // The checkout moves; the project and its notes stay.
    let (_, out) = scratch.run(&["project", "list", "--json"]).await;
    let body: Value = serde_json::from_str(&out.stdout()).expect("json");
    let repo_project = body["projects"].as_array().and_then(|all| all.iter().find(|p| p["id"] != "global")).expect("the repo's project");
    let project = repo_project["id"].as_str().expect("id").to_string();
    let identity = repo_project["roots"][0]["identity"].as_str().expect("identity").to_string();
    std::fs::create_dir_all(scratch.home.join("moved").join(".git")).expect("moved repo");
    let moved = scratch.dir("moved");
    let (exit, out) = scratch.run(&["project", "rebind", &project, &identity, "--cwd", &moved]).await;
    assert_eq!(exit, 0, "{}", out.text());
    let (_, out) = scratch.run(&["domain", "show", "core", "--cwd", &moved]).await;
    assert!(out.stdout().contains("Handlers validate first"), "{}", out.text());
    let (exit, out) = scratch.run(&["project", "add", "Other", "--path", &moved, "--exact"]).await;
    assert_eq!(exit, 3, "a directory belongs to one project: {}", out.text());
}

/// `ask` refuses a malformed ask before it calls the gateway: no question, a bad
/// agent name, two agents, or "off" are usage errors (exit 2).
#[tokio::test]
async fn knowledge_cli_ask_refuses_a_malformed_ask_locally() {
    let scratch = Scratch::new("ask");
    let repo = scratch.dir("repo");
    for (argv, why) in [
        (vec!["ask", "--cwd", repo.as_str()], "no question"),
        (vec!["ask", "  ", "--cwd", repo.as_str()], "an empty question"),
        (vec!["ask", "why?", "--agent", "Not A Name", "--cwd", repo.as_str()], "a bad agent name"),
        (vec!["ask", "why?", "--agent", "a,b", "--cwd", repo.as_str()], "two agents"),
        (vec!["ask", "why?", "--agent", "off", "--cwd", repo.as_str()], "off asks nobody"),
        (vec!["ask", "why?", "and?", "--cwd", repo.as_str()], "two questions"),
    ] {
        let (exit, out) = scratch.run(&argv).await;
        assert_eq!(exit, 2, "{why}: {}", out.text());
    }
}
