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

//! The sync adapters' tests, a file per part; the helpers they share live here.

use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

mod config;
mod runners;
mod git_walk;
mod sync_run;
mod map_run;
mod full_run;
mod recheck_run;
mod daily_review;
mod docs_run;

fn block(value: Value) -> Result<SyncConfig, String> {
    SyncConfig::parse(&value)
}

/// Tests that need a checkout skip when git is not usable on this machine.
fn git_present() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "swiss-sync-{label}-{}-{}",
        std::process::id(),
        swiss_core::util::random_hex(6)
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn git_at(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn head_of(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Three commits: a.txt, b.txt, then c.txt plus a credential-named file.
fn repo(label: &str) -> PathBuf {
    let dir = scratch(label);
    git_at(&dir, &["init", "-q"]);
    git_at(&dir, &["config", "user.email", "t@example.com"]);
    git_at(&dir, &["config", "user.name", "T"]);
    std::fs::write(dir.join("a.txt"), "one\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "first: one"]);
    std::fs::write(dir.join("b.txt"), "two two\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "second: two"]);
    std::fs::write(dir.join("c.txt"), "three\n").expect("write");
    std::fs::write(dir.join(".env"), "SECRET=x\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "third: three"]);
    dir
}

/// A checkout's path as the service binds a project's root: canonical, the verbatim
/// prefix off. Transcript session directories are named after this form, never the
/// raw path - a temp dir whose text is not its canonical form (a Windows 8.3 short
/// name, as a CI runner's temp is) would munge into a name no walk finds.
fn bound_root(dir: &Path) -> String {
    swiss_memory::scope::display(&dir.canonicalize().expect("the checkout canonicalizes"))
}

fn named(names: &[&str]) -> Choice {
    Choice::Named(names.iter().map(|n| n.to_string()).collect())
}

struct Canned {
    replies: Vec<String>,
    prompts: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl Canned {
    fn new(replies: Vec<String>) -> Canned {
        Canned { replies, prompts: Mutex::new(Vec::new()), calls: AtomicUsize::new(0) }
    }
}

#[async_trait]
impl Ask for Canned {
    async fn ask(&self, _root: &Path, prompt: &str) -> Result<String, String> {
        self.prompts.lock().expect("lock").push(prompt.to_string());
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.replies[i.min(self.replies.len() - 1)].clone())
    }

    async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.ask(root, prompt).await
    }

    fn label(&self) -> String {
        "canned".into()
    }

    fn model(&self) -> Option<String> {
        Some("canned".into())
    }
}

fn wrapped(text: &str) -> String {
    json!({ "type": "result", "subtype": "success", "result": text }).to_string()
}

fn not_json_reply() -> String {
    wrapped("I could not shape that.")
}

async fn service_with_project(label: &str, repo_dir: &Path) -> (MemoryService, String) {
    let dir = scratch(&format!("{label}-store"));
    let service = MemoryService::at(&dir.join("store.sqlite3"));
    let project = service
        .create_project(
            Some("Synced"),
            Some(&swiss_memory::scope::display(repo_dir)),
            true,
            None,
        )
        .await
        .expect("project");
    (service, project.id)
}

fn quiet_config() -> SyncConfig {
    SyncConfig::parse(&json!({ "agent": "distiller", "transcripts": "off" })).expect("config")
}

fn fill_reply(title: &str, keywords: &[&str]) -> String {
    wrapped(
        &json!({
            "title": title,
            "summary": format!("{title} for the shop's customers."),
            "overview": format!("{title} is where the shop keeps its money right."),
            "concepts": [{ "name": "refund", "meaning": "money sent back" }],
            "entryPoints": [{ "name": "src/billing/refund.rs", "role": "refunds start here" }, { "name": "src/nowhere.rs", "role": "made up" }],
            "flow": [],
            "code": [],
            "data": [],
            "dependsOn": [],
            "keywords": keywords
        })
        .to_string(),
    )
}

async fn mapped_service(label: &str, repo_dir: &Path) -> (MemoryService, String) {
    let (service, project) = service_with_project(label, repo_dir).await;
    service.set_lister(Some(std::sync::Arc::new(crate::memory_preview::CheckoutLister)));
    (service, project)
}

/// A runner answering by what each question holds: the first rule whose words the
/// prompt has answers; a question no rule knows fails.
struct Scripted {
    rules: Vec<(String, String)>,
    prompts: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(rules: Vec<(&str, String)>) -> Scripted {
        Scripted { rules: rules.into_iter().map(|(w, r)| (w.to_string(), r)).collect(), prompts: Mutex::new(Vec::new()) }
    }

    /// The questions asked that hold `words`.
    fn asked(&self, words: &str) -> Vec<String> {
        self.prompts.lock().expect("lock").iter().filter(|p| p.contains(words)).cloned().collect()
    }
}

#[async_trait]
impl Ask for Scripted {
    async fn ask(&self, _root: &Path, prompt: &str) -> Result<String, String> {
        self.prompts.lock().expect("lock").push(prompt.to_string());
        let rule = self.rules.iter().find(|(words, _)| prompt.contains(words.as_str()));
        let reply = |reply: &String| match reply.as_str() {
            KEEP_ALL => keep_all(prompt),
            KEEP_AND_CONTRADICT => keep_and_contradict(prompt),
            _ => reply.clone(),
        };
        rule.map(|(_, r)| reply(r)).ok_or_else(|| "no rule answers this question".to_string())
    }

    async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.ask(root, prompt).await
    }

    fn fills_at_once(&self) -> usize {
        4
    }

    fn takes_tasks(&self) -> bool {
        true
    }

    async fn task(&self, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        Ok(format!("worker at {effort:?}: {prompt}"))
    }

    fn label(&self) -> String {
        "scripted".into()
    }

    fn model(&self) -> Option<String> {
        Some("scripted".into())
    }
}

/// What tells each question apart.
const REVIEW: &str = "## The draft's top level";
const AREA: &str = "## The area `";
const SLICE: &str = "## Where these files sit";
const RECHECK: &str = "You recheck the memory of the code project";
/// A recheck's question on the sides of a contradiction opens so.
const SETTLE: &str = "You settle the contradictions in the memory of the code project";
const ORDER_SERVICE: &str = "src/main/java/com/acme/shop/service/OrderService.java";
/// The order service's one method, as an excerpt quotes it.
const PLACE: &str = "public void place(Order order) {}";
const PITFALL: &str = "Placing an order twice charges the customer twice";
/// A scripted reply standing for "keep every note the recheck question judges".
const KEEP_ALL: &str = "<keep every note judged>";

/// A scripted reply standing for "keep every note judged, and link the first two as
/// contradicting".
const KEEP_AND_CONTRADICT: &str = "<keep every note judged, the first two contradicting>";

/// The notes a recheck question judges, in order.
fn judged_ids(prompt: &str) -> Vec<String> {
    prompt
        .lines()
        .filter_map(|line| line.strip_prefix("### m-"))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|hex| format!("m-{hex}"))
        .collect()
}

/// A recheck answer keeping every note the question judges.
fn keep_all(prompt: &str) -> String {
    let verdicts: Vec<Value> = judged_ids(prompt).into_iter().map(|id| json!({ "id": id, "verdict": "keep" })).collect();
    wrapped(&json!({ "verdicts": verdicts, "links": [] }).to_string())
}

/// A recheck answer keeping every note the question judges, the first linked as
/// contradicting the second.
fn keep_and_contradict(prompt: &str) -> String {
    let ids = judged_ids(prompt);
    let verdicts: Vec<Value> = ids.iter().map(|id| json!({ "id": id, "verdict": "keep" })).collect();
    let links: Vec<Value> = match ids.as_slice() {
        [first, second, ..] => vec![json!({ "from": first, "to": second, "kind": "contradicts", "note": "they say the opposite" })],
        _ => Vec::new(),
    };
    wrapped(&json!({ "verdicts": verdicts, "links": links }).to_string())
}

/// A Spring-style checkout: the code three folders down under `src/main/java`, laid
/// out by layer, the SQL beside it. The rules cut it by business word: `order`,
/// `payment`, `customer`, `config` - never `src` and `sql` from the root.
fn java_repo(label: &str) -> PathBuf {
    let dir = scratch(label);
    git_at(&dir, &["init", "-q"]);
    git_at(&dir, &["config", "user.email", "t@example.com"]);
    git_at(&dir, &["config", "user.name", "T"]);
    let base = dir.join("src/main/java/com/acme/shop");
    let classes = [
        ("controller", ["OrderController", "PaymentController", "CustomerController"].as_slice()),
        ("service", &["OrderService", "OrderServiceImpl", "PaymentService", "CustomerService"]),
        ("repository", &["OrderRepository", "PaymentRepository", "CustomerRepository"]),
        ("model", &["Order", "Payment", "Customer"]),
        ("config", &["WebConfig", "DbConfig", "SecurityConfig"]),
    ];
    for (layer, names) in classes {
        std::fs::create_dir_all(base.join(layer)).expect("dir");
        for name in names {
            let source = format!(
                "package com.acme.shop.{layer};\n\npublic class {name} {{\n    public void place(Order order) {{}}\n}}\n"
            );
            std::fs::write(base.join(layer).join(format!("{name}.java")), source).expect("write");
        }
    }
    std::fs::create_dir_all(dir.join("sql")).expect("dir");
    for name in ["schema", "data", "upgrade_v2"] {
        std::fs::write(dir.join("sql").join(format!("{name}.sql")), "CREATE TABLE orders (id BIGINT PRIMARY KEY);\n").expect("write");
    }
    std::fs::write(dir.join("README.md"), "# Shop\n\nAn online shop: orders, payments and customers.\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "the shop"]);
    dir
}

fn notes_reply(notes: Value) -> String {
    wrapped(&json!({ "notes": notes }).to_string())
}

fn pitfall_reply() -> String {
    notes_reply(json!([{
        "kind": "pitfall",
        "title": PITFALL,
        "content": "place() takes no idempotency key: a retried request places a second order.",
        "anchors": [ORDER_SERVICE],
        "keywords": ["order", "retry"],
        "evidence": [{ "path": ORDER_SERVICE, "symbol": "place", "excerpt": "public void place(Order order) {}" }]
    }]))
}

fn shop_runner(review: String) -> Scripted {
    Scripted::new(vec![
        (RECHECK, KEEP_ALL.to_string()),
        (REVIEW, review),
        (AREA, fill_reply("Area", &[])),
        (&format!("### `{ORDER_SERVICE}`"), pitfall_reply()),
        (SLICE, notes_reply(json!([]))),
    ])
}

fn commerce_review() -> String {
    wrapped(
        &json!({ "domains": [{ "path": "commerce", "title": "Commerce", "members": ["order", "payment"], "merge": false }] })
            .to_string(),
    )
}

async fn paths_of(service: &MemoryService, project: &str) -> Vec<String> {
    let map = service.domains(&Target::project(project)).await.expect("map");
    map.items.iter().map(|d| d.path.clone()).collect()
}

async fn titles_in(service: &MemoryService, project: &str, domain: &str) -> Vec<String> {
    let request = swiss_memory::service::ListRequest { domain: Some(domain.into()), ..Default::default() };
    let (items, _) = service.list(&Target::project(project), &request).await.expect("list");
    items.into_iter().map(|s| s.title).collect()
}

/// A runner that answers the questions holding `words` until `after` of them, then
/// raises the run's stop and never answers - a person pressing Stop mid-run.
struct StopsAt {
    inner: Scripted,
    stop: Stop,
    words: &'static str,
    after: usize,
    seen: AtomicUsize,
}

#[async_trait]
impl Ask for StopsAt {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
        if prompt.contains(self.words) && self.seen.fetch_add(1, Ordering::SeqCst) >= self.after {
            self.stop.raise();
            return std::future::pending().await;
        }
        self.inner.ask(root, prompt).await
    }

    async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.ask(root, prompt).await
    }

    fn fills_at_once(&self) -> usize {
        4
    }

    fn label(&self) -> String {
        "stops".into()
    }

    fn model(&self) -> Option<String> {
        None
    }
}
