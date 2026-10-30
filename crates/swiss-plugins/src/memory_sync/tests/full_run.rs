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

//! The full run: business domains, the notes the code teaches, the lead's wrap-up, the stop.

use super::*;

#[tokio::test]
async fn a_full_run_draws_business_domains_then_saves_what_the_code_teaches() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("full");
    let (service, project) = mapped_service("full", &repo_dir).await;

    // Dry: the map's questions are asked, the slices only counted; nothing is written.
    let (run, _) = service.sync_enqueue(&project, full_enqueue(true, Some("zh".into()), false, None)).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let dry = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(dry.run.state, RunState::Ok, "{}", dry.run.error.clone().unwrap_or_default());
    let regroup = dry.run.scope.regroup.clone().expect("the top level was reviewed");
    assert_eq!(regroup.domains.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), ["commerce"]);
    let planned: Vec<String> = dry.run.proposal.clone().expect("a dry run keeps the proposal").domains.into_iter().map(|d| d.path).collect();
    for path in ["commerce", "commerce/order", "commerce/payment", "customer", "config"] {
        assert!(planned.iter().any(|p| p == path), "{path} not in {planned:?}");
    }
    assert!(!planned.iter().any(|p| p == "src" || p == "main"), "never the root's folders: {planned:?}");
    let knowledge = dry.run.scope.knowledge.clone().expect("the slices' plan");
    assert!(knowledge.slices > 0 && knowledge.files > 0, "{knowledge:?}");
    assert_eq!((knowledge.asked, knowledge.saved), (0, 0));
    assert!(runner.asked(SLICE).is_empty(), "a dry run asks no knowledge question");
    assert!(paths_of(&service, &project).await.is_empty(), "a dry run writes nothing");
    service.sync_finish(&dry.run, None).await.expect("finish");

    // For real: the map lands, then every slice is asked and its notes saved, filed in
    // the domain owning the files they were read from.
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, Some("zh".into()), false, None)).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let done = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let paths = paths_of(&service, &project).await;
    for path in ["commerce", "commerce/order", "commerce/payment"] {
        assert!(paths.iter().any(|p| p == path), "{path} not in {paths:?}");
    }
    let knowledge = done.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!((knowledge.asked, knowledge.failed), (knowledge.slices, 0), "{knowledge:?}");
    assert_eq!((knowledge.saved, knowledge.held, knowledge.duplicates), (1, 0, 0), "{knowledge:?} {}", done.run.summary);
    assert_eq!(knowledge.done.len(), knowledge.slices);
    assert_eq!(knowledge.embedding.as_ref().map(|e| e.state.as_str()), Some("off"), "no model: lexical search only");
    assert_eq!(titles_in(&service, &project, "commerce/order").await, [PITFALL]);
    // The recheck at the run's end shows the same file; only the slice question counts here.
    let asked: Vec<String> = runner.asked(&format!("### `{ORDER_SERVICE}`")).into_iter().filter(|p| p.contains(SLICE)).collect();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].contains("`commerce/order`") && asked[0].contains("Simplified Chinese"), "{}", asked[0]);
    assert!(done.run.summary.contains("1 note(s) saved"), "{}", done.run.summary);
    let advance = done.advance.clone().expect("a project that never synced starts here");
    service.sync_finish(&done.run, Some(&advance)).await.expect("finish");

    // Again: the map is drawn, every slice answered and unchanged - nothing is asked.
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let again = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Ok, "{}", again.run.error.clone().unwrap_or_default());
    let knowledge = again.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!((knowledge.asked, knowledge.unchanged), (0, knowledge.slices), "{knowledge:?}");
    assert!(runner.prompts.lock().expect("lock").is_empty(), "nothing new, nothing asked");
    service.sync_finish(&again.run, None).await.expect("finish");

    // A dry run over the landed map cuts the slices the real runs cut: it stands on the
    // map that is there, not only on what it would add, so every slice is unchanged.
    let (run, _) = service.sync_enqueue(&project, full_enqueue(true, None, false, None)).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let dry_again = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(dry_again.run.state, RunState::Ok, "{}", dry_again.run.error.clone().unwrap_or_default());
    let planned = dry_again.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!((planned.slices, planned.unchanged), (knowledge.slices, knowledge.slices), "{planned:?}");
    service.sync_finish(&dry_again.run, None).await.expect("finish");

    // A file changes: its slice alone is asked again, told the note it already has;
    // the same note again is the project's already.
    let edited = std::fs::read_to_string(repo_dir.join(ORDER_SERVICE)).expect("read") + "// retries arrive twice\n";
    std::fs::write(repo_dir.join(ORDER_SERVICE), edited).expect("write");
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, Some(5))).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let changed = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(changed.run.state, RunState::Ok, "{}", changed.run.error.clone().unwrap_or_default());
    let knowledge = changed.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!((knowledge.asked, knowledge.saved, knowledge.duplicates), (1, 0, 1), "{knowledge:?}");
    let asked = runner.asked(SLICE);
    assert!(asked.len() == 1 && asked[0].contains(&format!("- {PITFALL}")), "{asked:?}");
    assert_eq!(titles_in(&service, &project, "commerce/order").await, [PITFALL]);
}

#[tokio::test]
async fn a_stopped_full_run_keeps_what_it_saved_and_the_next_skips_it() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("stop");
    let (service, project) = mapped_service("stop", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let stop = Stop::default();
    let runner =
        StopsAt { inner: shop_runner(commerce_review()), stop: stop.clone(), words: SLICE, after: 1, seen: AtomicUsize::new(0) };
    let config = quiet_config();
    let work = execute_full(&service, run, &config, &runner, None, &stop);
    let halted = tokio::time::timeout(Duration::from_secs(60), work).await.expect("a raised stop ends the run, never hangs it");
    assert_eq!(halted.run.state, RunState::Cancelled, "{}", halted.run.error.clone().unwrap_or_default());
    assert!(halted.advance.is_none(), "a cancelled run moves no sync state");
    assert_eq!(halted.run.scope.phase, Some(Phase::Knowledge), "it says where it stopped");
    let knowledge = halted.run.scope.knowledge.clone().expect("knowledge");
    assert!(knowledge.slices > 1, "{knowledge:?}");
    assert_eq!((knowledge.asked, knowledge.done.len()), (1, 1), "{knowledge:?}");
    let said = format!("cancelled after 1 of {} slice(s)", knowledge.slices);
    assert!(halted.run.summary.contains(&said), "{}", halted.run.summary);
    assert!(paths_of(&service, &project).await.iter().any(|p| p == "commerce"), "the map it landed stays");
    service.sync_finish(&halted.run, None).await.expect("finish");

    // The next full run asks only what the stopped one left.
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let runner = shop_runner(commerce_review());
    let resumed = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(resumed.run.state, RunState::Ok, "{}", resumed.run.error.clone().unwrap_or_default());
    let again = resumed.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!((again.unchanged, again.asked), (1, knowledge.slices - 1), "{again:?}");
    assert_eq!(resumed.run.scope.phase, None, "a run that ended in full says no phase");
}

/// What tells a lead's wrap-up question apart.
const WRAPUP: &str = "You lead the reading of the code project";
const SPANS: &str = "An order is placed before its payment is taken";

/// A lead: it answers the review and each domain's wrap-up - handing one piece of each
/// wrap-up to the workers first, the way a builtin lead's delegate does - and nothing as
/// a plain fill, so a review asked of it the wrong way fails. With `stops` it raises the
/// stop at its first wrap-up and never answers it.
struct Leading {
    review: String,
    stops: Option<Stop>,
    prompts: Mutex<Vec<String>>,
    delegated: Mutex<Vec<String>>,
}

impl Leading {
    fn new(review: String, stops: Option<Stop>) -> Leading {
        Leading { review, stops, prompts: Mutex::new(Vec::new()), delegated: Mutex::new(Vec::new()) }
    }
}

#[async_trait]
impl Ask for Leading {
    async fn ask(&self, _root: &Path, _prompt: &str) -> Result<String, String> {
        Err("a lead is asked as a lead".into())
    }

    async fn fill(&self, _root: &Path, _prompt: &str, _schema: &Value) -> Result<String, String> {
        Err("a lead is asked as a lead".into())
    }

    async fn lead(&self, _root: &Path, prompt: &str, _schema: &Value, workers: Option<&dyn Ask>) -> Result<String, String> {
        self.prompts.lock().expect("lock").push(prompt.to_string());
        if prompt.contains(REVIEW) {
            return Ok(self.review.clone());
        }
        if !prompt.contains(WRAPUP) {
            return Err("no rule answers this question".into());
        }
        if let Some(stop) = &self.stops {
            stop.raise();
            return std::future::pending().await;
        }
        let workers = workers.filter(|w| w.takes_tasks()).ok_or("no workers to delegate to")?;
        let answer = workers.task("Where is the payment taken?", Some(Effort::Low)).await?;
        self.delegated.lock().expect("lock").push(answer);
        Ok(wrapped(
            &json!({
                "summary": "Takes an order and keeps a retried one from charging twice.",
                "keywords": ["idempotency", "order"],
                "notes": [{
                    "kind": "constraint", "title": SPANS, "content": "OrderService places it; PaymentService charges after.",
                    "anchors": [ORDER_SERVICE], "keywords": ["order", "payment"],
                    "evidence": [{ "path": ORDER_SERVICE, "symbol": "place", "excerpt": "public void place(Order order) {}" }]
                }]
            })
            .to_string(),
        ))
    }

    fn fills_at_once(&self) -> usize {
        2
    }

    fn label(&self) -> String {
        "builtin:planner".into()
    }

    fn model(&self) -> Option<String> {
        None
    }
}

#[tokio::test]
async fn a_lead_reviews_the_top_level_then_wraps_up_each_domain_the_run_wrote_in() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("lead");
    let (service, project) = mapped_service("lead", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    // The workers' own review would keep the draft: only the lead's draws `commerce`.
    let runner = shop_runner(wrapped(&json!({ "domains": [] }).to_string()));
    let lead = Leading::new(commerce_review(), None);
    let done = execute_full(&service, run, &quiet_config(), &runner, Some(&lead), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert!(runner.asked(REVIEW).is_empty(), "the review is the lead's");
    assert!(!runner.asked(AREA).is_empty() && !runner.asked(SLICE).is_empty(), "areas and slices are the workers'");
    assert!(paths_of(&service, &project).await.iter().any(|p| p == "commerce/order"), "the lead's review drew the map");
    assert_eq!(done.run.runner.lead.as_deref(), Some("builtin:planner"));

    // One domain got a note from its slices, so one wrap-up: told that note and the files.
    let asked: Vec<String> = lead.prompts.lock().expect("lock").iter().filter(|p| p.contains(WRAPUP)).cloned().collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    for said in ["## The domain `commerce/order`", PITFALL, &format!("- {ORDER_SERVICE}")] {
        assert!(asked[0].contains(said), "{said} missing:\n{}", asked[0]);
    }
    let delegated = lead.delegated.lock().expect("lock").clone();
    assert_eq!(delegated, ["worker at Some(Low): Where is the payment taken?"]);
    assert_eq!(
        done.run.scope.wrapup,
        Some(WrapupScope { domains: 1, wrapped: 1, hubs: 1, saved: 1, held: 0, failed: 0 }),
        "{}",
        done.run.summary
    );
    let hubs = service.map_domains(&project).await.expect("map");
    let hub = hubs.iter().find(|d| d.path == "commerce/order").expect("hub");
    assert_eq!(hub.summary, "Takes an order and keeps a retried one from charging twice.");
    assert!(hub.keywords.iter().any(|k| k == "idempotency"), "{:?}", hub.keywords);
    let mut titles = titles_in(&service, &project, "commerce/order").await;
    titles.sort();
    assert_eq!(titles, [SPANS, PITFALL]);
    assert!(done.run.ops.iter().any(|o| o.op == "domain.update" && o.target == "commerce/order" && o.outcome == engine::Outcome::Applied));
    assert!(done.run.summary.contains("the lead wrapped up 1 of 1 domain(s): 1 hub(s) refreshed, 1 note(s) saved"), "{}", done.run.summary);
    assert_eq!(done.run.scope.phase, None);

    // Then the workers recheck the notes nobody judged - the slice's and the lead's, kept - one
    // request a question, the code the notes name shown in it; the lead is not asked.
    assert!(!lead.prompts.lock().expect("lock").iter().any(|p| p.contains(RECHECK)), "the recheck is the workers'");
    let rechecks = runner.asked(RECHECK);
    assert_eq!(rechecks.len(), 1, "{rechecks:?}");
    assert!(rechecks[0].contains("The domain `commerce/order`") && rechecks[0].contains(SPANS), "{}", rechecks[0]);
    assert!(rechecks[0].contains(&format!("### `{ORDER_SERVICE}`\n```java\n")), "the code inlined: {}", rechecks[0]);
    let recheck = done.run.scope.recheck.clone().expect("the closing recheck");
    assert_eq!(
        (recheck.domains, recheck.notes, recheck.questions, recheck.asked, recheck.kept, recheck.failed),
        (1, 2, 1, 1, 2, 0),
        "{recheck:?}"
    );
    assert!(done.run.summary.contains("rechecked 2 of 2 note(s) in 1 domain(s): 2 kept"), "{}", done.run.summary);
    let request = swiss_memory::service::ListRequest { domain: Some("commerce/order".into()), ..Default::default() };
    let (items, _) = service.list(&Target::project(&project), &request).await.expect("list");
    for item in items {
        let (record, _) = service.get(&Target::project(&project), &item.id, false).await.expect("get");
        assert!(record.verified_ms.is_some(), "{} verified by the recheck", record.summary.title);
    }
    // A recheck right after finds nothing to judge, and says so.
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, None, None)).await.expect("enqueue");
    let again = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Empty, "{}", again.run.error.clone().unwrap_or_default());
    assert!(again.run.summary.starts_with("no note waits for a recheck"), "{}", again.run.summary);
    assert_eq!(runner.asked(RECHECK).len(), 1, "no second question");
}

#[tokio::test]
async fn a_full_runs_closing_recheck_settles_the_contradiction_its_answer_raises() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("full-settle");
    let (service, project) = mapped_service("full-settle", &repo_dir).await;
    let evidence = json!([{ "path": ORDER_SERVICE, "symbol": "place", "excerpt": PLACE }]);
    let two = notes_reply(json!([
        { "kind": "pitfall", "title": PITFALL, "content": "place() takes no idempotency key: a retried request places a second order.",
          "anchors": [ORDER_SERVICE], "keywords": ["order", "retry"], "evidence": evidence },
        { "kind": "constraint", "title": "An order is placed once per request", "content": "place() runs once for each request.",
          "anchors": [ORDER_SERVICE], "keywords": ["order"], "evidence": evidence }
    ]));
    let runner = Scripted::new(vec![
        (SETTLE, KEEP_ALL.to_string()),
        (RECHECK, KEEP_AND_CONTRADICT.to_string()),
        (REVIEW, commerce_review()),
        (AREA, fill_reply("Area", &[])),
        (&format!("### `{ORDER_SERVICE}`"), two),
        (SLICE, notes_reply(json!([]))),
    ]);
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let done = execute_full(&service, run, &quiet_config(), &runner, None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.scope.knowledge.clone().expect("knowledge").saved, 2, "{}", done.run.summary);
    assert_eq!(runner.asked(RECHECK).len(), 1);
    let asked = runner.asked(SETTLE);
    assert_eq!(asked.len(), 1, "the contradiction the closing recheck raised, asked in the same run");
    assert!(asked[0].contains("## The contradictions\n") && asked[0].contains(PITFALL), "{}", asked[0]);
    let recheck = done.run.scope.recheck.clone().expect("the closing recheck");
    assert_eq!(
        (recheck.notes, recheck.questions, recheck.asked, recheck.kept, recheck.rounds, recheck.lifted, recheck.open),
        (4, 2, 2, 4, 1, 1, 0),
        "{recheck:?}"
    );
    assert!(done.run.summary.contains("; 1 contradiction(s) lifted (1 more round(s) for the contradictions the answers raised)"), "{}", done.run.summary);
    let (items, _) = service.list(&Target::project(&project), &swiss_memory::service::ListRequest::default()).await.expect("list");
    assert_eq!(items.len(), 2);
    for item in items {
        let (record, _) = service.get(&Target::project(&project), &item.id, false).await.expect("get");
        assert!(record.verified_ms.is_some() && !record.summary.confirmed, "{}", record.summary.title);
        assert!(record.links.iter().all(|l| l.kind != swiss_memory::model::LinkKind::Contradicts), "{}", record.summary.title);
    }
}

#[tokio::test]
async fn a_stop_during_the_wrapup_keeps_what_the_slices_saved() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("lead-stop");
    let (service, project) = mapped_service("lead-stop", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let stop = Stop::default();
    let runner = shop_runner(commerce_review());
    let lead = Leading::new(commerce_review(), Some(stop.clone()));
    let config = quiet_config();
    let work = execute_full(&service, run, &config, &runner, Some(&lead), &stop);
    let halted = tokio::time::timeout(Duration::from_secs(60), work).await.expect("a raised stop ends the run, never hangs it");
    assert_eq!(halted.run.state, RunState::Cancelled, "{}", halted.run.error.clone().unwrap_or_default());
    assert_eq!(halted.run.scope.phase, Some(Phase::Wrapup), "it says where it stopped");
    let knowledge = halted.run.scope.knowledge.clone().expect("knowledge");
    assert_eq!(knowledge.done.len(), knowledge.slices, "every slice was answered before the wrap-up");
    assert!(halted.run.summary.contains("cancelled while the lead wrapped up 0 of 1 domain(s)"), "{}", halted.run.summary);
    assert_eq!(titles_in(&service, &project, "commerce/order").await, [PITFALL], "the slices' notes stay");
}
