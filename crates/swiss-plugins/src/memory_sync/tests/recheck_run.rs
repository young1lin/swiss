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

//! The recheck and the review run: verdicts applied, contradictions settled, the changed notes.

use super::*;

const PAYMENT_SERVICE: &str = "src/main/java/com/acme/shop/service/PaymentService.java";

fn drafted(title: &str, content: &str, path: &str, excerpt: &str) -> init::Drafted {
    init::Drafted {
        kind: swiss_memory::model::Kind::Constraint,
        title: title.into(),
        content: content.into(),
        anchors: vec![path.into()],
        keywords: Vec::new(),
        evidence: vec![(path.into(), None, excerpt.into())],
    }
}

#[tokio::test]
async fn a_recheck_keeps_fixes_merges_and_drops_what_the_code_bears_out_and_links_the_rest() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("recheck");
    let (service, project) = mapped_service("recheck", &repo_dir).await;
    let target = Target::project(&project);
    let notes = [
        drafted("Placing an order twice charges the customer twice", "place() takes no idempotency key.", ORDER_SERVICE, PLACE),
        drafted("An order is placed before its payment is taken", "OrderService charges the card.", ORDER_SERVICE, PLACE),
        drafted("Retried checkout requests bill the shopper again", "A retry is a second purchase.", ORDER_SERVICE, PLACE),
        drafted("Orders are cached in Redis for an hour", "The cache key is the order id.", "sql/schema.sql", "CREATE TABLE orders"),
        drafted("Cards are charged inside OrderService", "The charge sits in the order flow.", ORDER_SERVICE, "public void charge(Card card) {}"),
    ];
    for note in notes {
        let saved = service.knowledge_save(&project, "r-0000000001", None, note.clone()).await;
        assert!(matches!(saved, Saved::New { .. }), "{} not saved: {saved:?}", note.title);
    }
    let person = swiss_memory::service::SaveRequest {
        kind: swiss_memory::model::Kind::Knowledge,
        title: "Customers sign in with their email address".into(),
        content: "CustomerService looks a customer up by email.".into(),
        anchors: vec!["src/main/java/com/acme/shop/service/CustomerService.java".into()],
        refs: Vec::new(),
        keywords: Vec::new(),
        links: Vec::new(),
        replaces: None,
        allow_similar: true,
        domain: None,
        evidence: Vec::new(),
    };
    service.save(&target, &swiss_memory::model::Actor::panel(), person).await.expect("a person's note");
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let id = |words: &str| items.iter().find(|s| s.title.contains(words)).map(|s| s.id.clone()).expect(words);
    let (twice, placed, retried, redis, cards, email) =
        (id("twice"), id("placed before"), id("Retried"), id("Redis"), id("Cards"), id("email"));
    let reply = wrapped(
        &json!({
            "verdicts": [
                { "id": twice, "verdict": "keep" },
                { "id": placed, "verdict": "fix", "content": "OrderService places the order; PaymentService takes the payment.",
                  "anchors": [ORDER_SERVICE, PAYMENT_SERVICE],
                  "evidence": [{ "path": PAYMENT_SERVICE, "symbol": "place", "excerpt": PLACE }],
                  "reason": "the payment is PaymentService's" },
                { "id": retried, "verdict": "merge", "into": twice, "reason": "the same retry" },
                { "id": redis, "verdict": "drop", "reason": "the code has no cache" },
                { "id": cards, "verdict": "keep" }
            ],
            "links": [
                { "from": twice, "to": placed, "kind": "depends_on", "note": "the charge follows the placing" },
                { "from": twice, "to": email, "kind": "related_to" }
            ]
        })
        .to_string(),
    );
    let runner = Scripted::new(vec![(RECHECK, reply)]);

    // Dry: the questions counted, none asked.
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(true, None, None)).await.expect("enqueue");
    let dry = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(dry.run.state, RunState::Ok, "{}", dry.run.error.clone().unwrap_or_default());
    assert_eq!(dry.run.summary, "would recheck 5 note(s) in 0 domain(s): 1 question(s), 0 for a later run");
    assert!(runner.prompts.lock().expect("lock").is_empty(), "a dry run asks nothing");
    service.sync_finish(&dry.run, None).await.expect("finish");

    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, Some("zh".into()), None)).await.expect("enqueue");
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.trigger, Trigger::Recheck);
    let asked = runner.asked(RECHECK);
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].contains(&format!("### {email}")), "a person's note is never judged");
    assert!(asked[0].contains(&format!("- {email}: ")), "but listed to merge into or link to");
    assert!(asked[0].contains(&format!("### {cards} (revision 1, constraint, needs review: ")), "{}", asked[0]);
    assert!(asked[0].contains("Simplified Chinese"), "{}", asked[0]);
    let scope = done.run.scope.recheck.clone().expect("recheck");
    assert_eq!(
        scope,
        RecheckScope {
            domains: 0,
            notes: 5,
            questions: 1,
            asked: 1,
            kept: 1,
            fixed: 1,
            dropped: 1,
            merged: 1,
            linked: 2,
            held: 1,
            ..RecheckScope::default()
        },
        "{}",
        done.run.summary
    );
    assert!(done.run.summary.starts_with("rechecked 5 of 5 note(s) in 0 domain(s): 1 kept, 1 fixed, 1 dropped, 1 merged"), "{}", done.run.summary);
    assert!(done.run.summary.contains("1 still need review"), "{}", done.run.summary);
    service.sync_finish(&done.run, None).await.expect("finish");

    let get = |id: String| {
        let (service, target) = (&service, &target);
        async move { service.get(target, &id, false).await.expect("get").0 }
    };
    let kept = get(twice.clone()).await;
    assert!(kept.verified_ms.is_some() && kept.summary.status == swiss_memory::model::Status::Active);
    assert!(kept.links.iter().any(|l| l.id == placed && l.kind == swiss_memory::model::LinkKind::DependsOn && l.dir == "out"));
    assert!(kept.links.iter().any(|l| l.id == email && l.kind == swiss_memory::model::LinkKind::RelatedTo));
    let fixed = get(placed.clone()).await;
    assert_eq!(fixed.content, "OrderService places the order; PaymentService takes the payment.");
    assert!(fixed.verified_ms.is_some() && fixed.summary.status == swiss_memory::model::Status::Active);
    let (_, revisions) = service.get(&target, &placed, true).await.expect("history");
    let reasons: Vec<String> = revisions.unwrap_or_default().into_iter().filter_map(|r| r.reason).collect();
    let why = format!("recheck {}: the payment is PaymentService's", done.run.id);
    assert!(reasons.iter().any(|r| r == &why), "{reasons:?}");
    let merged = get(retried).await;
    assert_eq!(merged.summary.status, swiss_memory::model::Status::Superseded);
    assert_eq!(merged.superseded_by.as_deref(), Some(twice.as_str()));
    assert_eq!(get(redis).await.summary.status, swiss_memory::model::Status::Invalidated);
    let held = get(cards.clone()).await;
    assert_eq!(held.summary.status, swiss_memory::model::Status::NeedsReview, "a model's word never releases a held note");

    // The next recheck judges only the note still held.
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(true, None, None)).await.expect("enqueue");
    let again = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(again.run.summary, "would recheck 1 note(s) in 0 domain(s): 1 question(s), 0 for a later run");

    // A runner with tools looks into the code itself: the question says so, and the
    // answer comes from its look, not a fill.
    let looker = Looks { inner: Scripted::new(vec![(RECHECK, KEEP_ALL.to_string())]), looked: AtomicUsize::new(0) };
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, None, None)).await.expect("enqueue");
    let looked = execute_recheck(&service, run, &looker, &Stop::default()).await;
    assert_eq!(looked.run.state, RunState::Ok, "{}", looked.run.error.clone().unwrap_or_default());
    assert_eq!(looker.looked.load(Ordering::SeqCst), 1, "the one question was looked into");
    let asked = looker.inner.asked(RECHECK);
    assert!(asked[0].contains("Look before you judge") && asked[0].contains("as your last message"), "{}", asked[0]);
    let scope = looked.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.asked, scope.held), (1, 1), "a keep still leaves a held note held: {}", looked.run.summary);
}

/// A review run's question opens so.
const RESOLVE: &str = "You resolve the review inbox of the code project";

#[tokio::test]
async fn auto_update_reviews_a_human_note_even_when_its_file_has_not_changed() {
    let repo_dir = java_repo("auto-human");
    let (service, project) = mapped_service("auto-human", &repo_dir).await;
    let target = Target::project(&project);
    let person = swiss_memory::model::Actor::panel();
    let saved = service.save(&target, &person, swiss_memory::service::SaveRequest {
        kind: swiss_memory::model::Kind::Knowledge, title:"Order service charges cards".into(),
        content:"OrderService invokes the card charge.".into(), anchors:vec![ORDER_SERVICE.into()],
        refs:vec![], keywords:vec![], links:vec![], replaces:None, allow_similar:true, domain:None, evidence:vec![],
    }).await.expect("human note");
    let id = saved.record.summary.id;
    service.mark(&target, &person, &id, swiss_memory::Verdict::Outdated, Some("check this note again")).await.expect("pending review");
    service.patch_project_settings(&project, &json!({"autoReview":"03:30"})).await.expect("daily review");
    let due = now_ms();
    assert_eq!(auto_review_tick(&service, due, |_| Some(due), &mut HashMap::new()).await.expect("tick"), 1);
    let run = service.sync_runs(&project, 1).await.expect("runs").into_iter().next().expect("queued review");
    assert!(!run.scope.changed, "the clock must ask about the whole inbox");
    let runner = Scripted::new(vec![(RESOLVE, KEEP_ALL.to_string())]);
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.summary);
    assert_eq!(done.run.scope.recheck.expect("review").kept, 1);
    let (record, _) = service.get(&target, &id, false).await.expect("reviewed note");
    assert_eq!(record.summary.status, swiss_memory::model::Status::Active);
    assert_eq!(record.summary.origin, swiss_memory::model::Origin::User);
    assert!(record.verified_ms.is_some());
}

#[tokio::test]
async fn a_review_run_judges_the_inbox_confirms_what_it_keeps_and_settles_a_contradiction() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("review");
    let (service, project) = mapped_service("review", &repo_dir).await;
    let target = Target::project(&project);
    let person = swiss_memory::model::Actor::panel();
    let notes = [
        drafted("Placing an order twice charges the customer twice", "place() takes no idempotency key.", ORDER_SERVICE, PLACE),
        drafted("An order is placed once per request", "place() runs once for each request.", ORDER_SERVICE, PLACE),
    ];
    for note in notes {
        let saved = service.knowledge_save(&project, "r-0000000001", None, note.clone()).await;
        assert!(matches!(saved, Saved::New { .. }), "{} not saved: {saved:?}", note.title);
    }
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let id = |words: &str| items.iter().find(|s| s.title.contains(words)).map(|s| s.id.clone()).expect(words);
    let (twice, once) = (id("twice"), id("once"));
    let contradicts = swiss_memory::model::LinkKind::Contradicts;
    service.link(&target, &person, &twice, &once, contradicts, None).await.expect("link");
    service.confirm(&target, &person, &twice).await.expect("confirm");
    let runner = Scripted::new(vec![(RESOLVE, KEEP_ALL.to_string())]);

    // Dry: the inbox's notes a model wrote counted - the confirmed one too.
    let (run, _) = service.sync_enqueue(&project, review_enqueue(true, None, None, false)).await.expect("enqueue");
    let dry = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(dry.run.state, RunState::Ok, "{}", dry.run.error.clone().unwrap_or_default());
    assert_eq!(dry.run.summary, "would resolve 2 note(s) in 0 domain(s): 1 question(s), 0 for a later run");
    service.sync_finish(&dry.run, None).await.expect("finish");

    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("enqueue");
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.trigger, Trigger::Review);
    let asked = runner.asked(RESOLVE);
    assert_eq!(asked.len(), 1, "both sides in one question");
    assert!(asked[0].contains("## The contradictions\n"), "{}", asked[0]);
    assert!(asked[0].contains(&format!("### {twice} ")) && asked[0].contains(&format!("### {once} ")), "{}", asked[0]);
    let scope = done.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.notes, scope.kept, scope.confirmed, scope.lifted), (2, 2, 1, 1), "{}", done.run.summary);
    assert!(done.run.summary.starts_with("resolved 2 of 2 note(s)"), "{}", done.run.summary);
    assert!(done.run.summary.contains("; 1 confirmed, 1 contradiction(s) lifted"), "{}", done.run.summary);
    service.sync_finish(&done.run, None).await.expect("finish");
    for id in [&twice, &once] {
        let (record, revisions) = service.get(&target, id, true).await.expect("get");
        assert!(record.summary.confirmed && record.verified_ms.is_some(), "{id}");
        assert!(record.links.iter().all(|l| l.kind != contradicts), "the contradiction is lifted: {id}");
        let reasons: Vec<String> = revisions.unwrap_or_default().into_iter().filter_map(|r| r.reason).collect();
        assert!(reasons.contains(&format!("review {}", done.run.id)), "the verified mark: {reasons:?}");
        // The person confirmed one side; the run confirms the other on their request.
        let confirmed = format!("review {}: the code bears it out; confirmed on the person's request", done.run.id);
        assert_eq!(reasons.contains(&confirmed), *id == once, "{id}: {reasons:?}");
    }

    // Nothing a model wrote waits now: the next review asks nothing, and says so.
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("enqueue");
    let again = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Empty, "{}", again.run.error.clone().unwrap_or_default());
    assert!(again.run.summary.starts_with("the review inbox holds no note waiting for review"), "{}", again.run.summary);
    assert_eq!(runner.asked(RESOLVE).len(), 1, "no second question");
}

/// A changed review's question says so.
const CHANGED: &str = "whose code changed. Workers wrote the notes below";

#[test]
fn a_changed_review_counts_its_settled_notes_apart_from_the_changed_ones() {
    let alone = RecheckScope { notes: 1, kept: 1, confirmed: 1, ..RecheckScope::default() };
    assert_eq!(
        rechecked_words(&alone, recheck::Reach::Changed),
        "reviewed 1 of 1 note(s) whose code changed in 0 domain(s): 1 kept, 0 fixed, 0 dropped, 0 merged, 0 link(s) added; 1 confirmed"
    );
    let settled = RecheckScope { notes: 3, fixed: 2, held: 1, rounds: 1, lifted: 1, ..RecheckScope::default() };
    assert_eq!(
        rechecked_words(&settled, recheck::Reach::Changed),
        "reviewed 3 of 3 note(s) - whose code changed, or an answer set against one - in 0 domain(s): 0 kept, 2 fixed, \
         0 dropped, 0 merged, 0 link(s) added; 0 confirmed, 1 contradiction(s) lifted (1 more round(s) for the \
         contradictions the answers raised); 1 still need review"
    );
}

#[tokio::test]
async fn a_changed_review_judges_the_notes_whose_files_changed_and_the_clock_queues_one_a_day() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("review-changed");
    let (service, project) = mapped_service("review-changed", &repo_dir).await;
    let target = Target::project(&project);
    let notes = [
        drafted("Placing an order twice charges the customer twice", "place() takes no idempotency key.", ORDER_SERVICE, PLACE),
        drafted("A payment is taken once per order", "place() charges once for each order.", PAYMENT_SERVICE, PLACE),
    ];
    for note in notes {
        let saved = service.knowledge_save(&project, "r-0000000001", None, note.clone()).await;
        assert!(matches!(saved, Saved::New { .. }), "{} not saved: {saved:?}", note.title);
    }
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let id = |words: &str| items.iter().find(|s| s.title.contains(words)).map(|s| s.id.clone()).expect(words);
    let (order, payment) = (id("twice"), id("payment"));
    // The order service changes under its note; the payment service stays.
    let edited = std::fs::read_to_string(repo_dir.join(ORDER_SERVICE)).expect("read") + "// retried by the gateway\n";
    std::fs::write(repo_dir.join(ORDER_SERVICE), edited).expect("write");
    let runner = Scripted::new(vec![(CHANGED, KEEP_ALL.to_string())]);

    let (run, _) = service.sync_enqueue(&project, review_enqueue(true, None, None, true)).await.expect("enqueue");
    assert!(run.scope.changed);
    let dry = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert!(dry.run.summary.starts_with("would review 1 note(s) whose code changed in "), "{}", dry.run.summary);
    service.sync_finish(&dry.run, None).await.expect("finish");

    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, true)).await.expect("enqueue");
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let asked = runner.asked(CHANGED);
    assert_eq!(asked.len(), 1, "one question");
    assert!(asked[0].contains(&format!("### {order} ")) && !asked[0].contains(&format!("### {payment} ")), "{}", asked[0]);
    assert!(asked[0].contains("changed since it was last checked"), "{}", asked[0]);
    let scope = done.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.notes, scope.kept, scope.confirmed), (1, 1, 1), "{}", done.run.summary);
    assert!(done.run.summary.starts_with("reviewed 1 of 1 note(s) whose code changed"), "{}", done.run.summary);
    assert!(done.run.summary.contains("; 1 confirmed"), "{}", done.run.summary);
    service.sync_finish(&done.run, None).await.expect("finish");
    let (kept, _) = service.get(&target, &order, false).await.expect("get");
    assert!(kept.summary.confirmed && kept.verified_ms.is_some(), "kept, verified at the file as it is now, and confirmed");
    let (waits, _) = service.get(&target, &payment, false).await.expect("get");
    assert!(!waits.summary.confirmed, "an unchanged note waits for a whole review");

    // Its file checked again, nothing changed: the next changed review asks nothing.
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, true)).await.expect("enqueue");
    let again = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Empty, "{}", again.run.error.clone().unwrap_or_default());
    assert!(again.run.summary.starts_with("no note a model wrote names a file that changed"), "{}", again.run.summary);
    service.sync_finish(&again.run, None).await.expect("finish");
    // A second look finds nothing either: the newest empty review speaks for both.
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, true)).await.expect("enqueue");
    let twice = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(twice.run.state, RunState::Empty, "{}", twice.run.error.clone().unwrap_or_default());
    service.sync_finish(&twice.run, None).await.expect("finish");
    let runs = service.sync_runs(&project, engine::RUNS_KEPT).await.expect("runs");
    let empties: Vec<&str> = runs.iter().filter(|r| r.state == RunState::Empty && r.scope.changed).map(|r| r.id.as_str()).collect();
    assert_eq!(empties, [twice.run.id.as_str()], "the newest empty review stands for the ones before");
    assert!(runs.iter().any(|r| r.id == done.run.id) && runs.iter().any(|r| r.id == dry.run.id), "the reviews that said something stay");

    // The clock: nothing for a project without a time of day.
    let after = now_ms() + 1;
    let mut days = HashMap::new();
    assert_eq!(auto_review_tick(&service, after, |_| Some(after), &mut days).await.expect("tick"), 0, "no autoReview");
    service.patch_project_settings(&project, &json!({ "autoReview": "03:30" })).await.expect("settings");
    assert_eq!(auto_review_tick(&service, after, |_| Some(after - 3_600_000), &mut days).await.expect("tick"), 0, "a review ran since");
    assert_eq!(auto_review_tick(&service, after, |_| Some(after + 60_000), &mut days).await.expect("tick"), 0, "the time has not come");
    assert_eq!(auto_review_tick(&service, after, |slot| (slot == "03:30").then_some(after), &mut days).await.expect("tick"), 1, "its time came");
    let runs = service.sync_runs(&project, 1).await.expect("runs");
    let queued = &runs[0];
    assert!(queued.trigger == Trigger::Review && !queued.scope.changed && !queued.dry_run && queued.state == RunState::Queued);
    let fresh = &mut HashMap::new();
    assert_eq!(auto_review_tick(&service, after, |_| Some(after), fresh).await.expect("tick"), 0, "the queued run is waited out");
    assert_eq!(auto_review_tick(&service, after, |_| Some(after), &mut days).await.expect("tick"), 0, "by the clock's memory too");
    assert_eq!(days[&project], ReviewDay { due: after, queued: 1, settled: false });
    // A whole review asked meanwhile takes the queued run over; changed is a review's alone.
    let (whole, merged) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("whole");
    assert!(merged && whole.id == queued.id && !whole.scope.changed, "the whole inbox holds the changed notes");
    // A sync asked meanwhile waits in its own queue and leaves the review as it is.
    let (sync, merged) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("sync");
    assert!(!merged && sync.id != queued.id && sync.trigger == Trigger::Manual);
    let runs = service.sync_runs(&project, 2).await.expect("runs");
    assert!(runs.iter().any(|r| r.id == queued.id && r.trigger == Trigger::Review), "the review is still a review");
    let wrong = Enqueue { changed: true, ..recheck_enqueue(false, None, None) };
    assert!(service.sync_enqueue(&project, wrong).await.is_err(), "a recheck has no changed");
    // The next day's review, gone from the runs as a busy day's newer runs push it off
    // the kept ones (a cancelled run before its time stands in for that): the clock's
    // memory still speaks for the day, the runs alone would queue a second.
    service.sync_cancel(Some(&project), &queued.id).await.expect("cancel");
    let tomorrow = after + 86_400_000;
    assert_eq!(auto_review_tick(&service, tomorrow, |_| Some(tomorrow), &mut days).await.expect("tick"), 1, "the next day's time came");
    let runs = service.sync_runs(&project, engine::RUNS_KEPT).await.expect("runs");
    let next = runs.iter().find(|r| r.trigger == Trigger::Review && r.state == RunState::Queued).expect("the next day's review");
    service.sync_cancel(Some(&project), &next.id).await.expect("cancel");
    assert_eq!(auto_review_tick(&service, tomorrow, |_| Some(tomorrow), &mut days).await.expect("tick"), 0, "the clock remembers its day");
    assert!(days[&project].settled);
    assert_eq!(auto_review_tick(&service, tomorrow, |_| Some(tomorrow), &mut HashMap::new()).await.expect("tick"), 1, "the runs alone forget it");
    let today = today_at("00:00").expect("midnight");
    assert!(today <= now_ms() && now_ms() - today < 25 * 3_600_000, "today's midnight on the gateway's clock");
    assert_eq!(today_at("3:30"), None);
}

#[tokio::test]
async fn a_review_run_settles_the_contradictions_its_answers_raise_and_leaves_a_stubborn_one_open() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("review-settle");
    let (service, project) = mapped_service("review-settle", &repo_dir).await;
    let target = Target::project(&project);
    let person = swiss_memory::model::Actor::panel();
    let notes = [
        drafted("Placing an order twice charges the customer twice", "place() takes no idempotency key.", ORDER_SERVICE, PLACE),
        drafted("An order is placed once per request", "place() runs once for each request.", ORDER_SERVICE, PLACE),
        drafted("Cancelling an order refunds the card at once", "The refund runs inside the cancel.", ORDER_SERVICE, PLACE),
        drafted("A cancelled order is refunded the next day", "A nightly job refunds cancelled orders.", ORDER_SERVICE, PLACE),
    ];
    for note in notes {
        let saved = service.knowledge_save(&project, "r-0000000001", None, note.clone()).await;
        assert!(matches!(saved, Saved::New { .. }), "{} not saved: {saved:?}", note.title);
    }
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let id = |words: &str| items.iter().find(|s| s.title.contains(words)).map(|s| s.id.clone()).expect(words);
    let (twice, once, at_once, next_day) = (id("twice"), id("once per"), id("at once"), id("next day"));
    let contradicts = swiss_memory::model::LinkKind::Contradicts;
    let contradicted = |id: &str, other: &str| {
        wrapped(
            &json!({
                "verdicts": [{ "id": id, "verdict": "keep" }],
                "links": [{ "from": id, "to": other, "kind": "contradicts", "note": "the other says the opposite" }]
            })
            .to_string(),
        )
    };

    // A note the person confirmed sits outside the inbox; the answer on the other links
    // them as contradicting: the run asks both sides at once, and they settle.
    service.confirm(&target, &person, &once).await.expect("confirm");
    service.confirm(&target, &person, &at_once).await.expect("confirm");
    service.confirm(&target, &person, &next_day).await.expect("confirm");
    let runner = Scripted::new(vec![("## The contradictions\n", KEEP_ALL.to_string()), (RESOLVE, contradicted(&twice, &once))]);
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("enqueue");
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let asked = runner.asked(RESOLVE);
    assert_eq!(asked.len(), 2, "the planned question, then the contradiction it raised");
    assert!(!asked[0].contains("## The contradictions\n") && asked[0].contains(&format!("### {twice} ")), "{}", asked[0]);
    assert!(asked[1].contains("## The contradictions\n"), "{}", asked[1]);
    assert!(asked[1].contains(&format!("### {twice} ")) && asked[1].contains(&format!("### {once} ")), "{}", asked[1]);
    assert!(!asked[1].contains("still linked as contradicting"), "asked side by side once: {}", asked[1]);
    let scope = done.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.questions, scope.asked, scope.rounds, scope.lifted, scope.open), (2, 2, 1, 1, 0), "{}", done.run.summary);
    assert!(done.run.summary.contains("1 contradiction(s) lifted (1 more round(s) for the contradictions the answers raised)"), "{}", done.run.summary);
    assert!(!done.run.summary.contains("still open"), "{}", done.run.summary);
    service.sync_finish(&done.run, None).await.expect("finish");
    for id in [&twice, &once] {
        let (record, _) = service.get(&target, id, true).await.expect("get");
        assert!(record.summary.confirmed, "{id}");
        assert!(record.links.iter().all(|l| l.kind != contradicts), "the raised contradiction is settled: {id}");
    }
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("enqueue");
    let again = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Empty, "no second run needed: {}", again.run.summary);

    // An answer that links the pair again each time: asked side by side twice, told the
    // second time, then left open for the person - the run ends.
    service.link(&target, &person, &at_once, &next_day, contradicts, None).await.expect("link");
    let stubborn = Scripted::new(vec![(RESOLVE, contradicted(&at_once, &next_day))]);
    let (run, _) = service.sync_enqueue(&project, review_enqueue(false, None, None, false)).await.expect("enqueue");
    let left = execute_recheck(&service, run, &stubborn, &Stop::default()).await;
    assert_eq!(left.run.state, RunState::Ok, "{}", left.run.error.clone().unwrap_or_default());
    let asked = stubborn.asked(RESOLVE);
    assert_eq!(asked.len(), 2, "the pair asked side by side twice, no more");
    assert!(asked.iter().all(|p| p.contains("## The contradictions\n")), "{asked:?}");
    assert!(!asked[0].contains("still linked as contradicting"), "{}", asked[0]);
    assert!(asked[1].contains(&format!("it was judged beside {next_day} earlier in this run")), "{}", asked[1]);
    assert!(asked[1].contains(&format!("it was judged beside {at_once} earlier in this run")), "{}", asked[1]);
    let scope = left.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.asked, scope.rounds, scope.lifted, scope.open), (2, 1, 0, 1), "{}", left.run.summary);
    assert!(left.run.summary.contains("; 1 contradiction(s) between the model's notes still open"), "{}", left.run.summary);
    service.sync_finish(&left.run, None).await.expect("finish");
    let (record, _) = service.get(&target, &at_once, false).await.expect("get");
    assert!(record.links.iter().any(|l| l.kind == contradicts), "the contradiction stays for the person");
}

#[tokio::test]
async fn a_recheck_settles_the_contradictions_between_the_models_notes_and_leaves_a_stubborn_one_open() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("recheck-settle");
    let (service, project) = mapped_service("recheck-settle", &repo_dir).await;
    let target = Target::project(&project);
    let person = swiss_memory::model::Actor::panel();
    let notes = [
        drafted("Placing an order twice charges the customer twice", "place() takes no idempotency key.", ORDER_SERVICE, PLACE),
        drafted("An order is placed once per request", "place() runs once for each request.", ORDER_SERVICE, PLACE),
        drafted("Cancelling an order refunds the card at once", "The refund runs inside the cancel.", ORDER_SERVICE, PLACE),
        drafted("A cancelled order is refunded the next day", "A nightly job refunds cancelled orders.", ORDER_SERVICE, PLACE),
    ];
    for note in notes {
        let saved = service.knowledge_save(&project, "r-0000000001", None, note.clone()).await;
        assert!(matches!(saved, Saved::New { .. }), "{} not saved: {saved:?}", note.title);
    }
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let id = |words: &str| items.iter().find(|s| s.title.contains(words)).map(|s| s.id.clone()).expect(words);
    let (twice, once, at_once, next_day) = (id("twice"), id("once per"), id("at once"), id("next day"));
    let contradicts = swiss_memory::model::LinkKind::Contradicts;
    let pair = |a: &str, b: &str| if a < b { format!("- {a} and {b}\n") } else { format!("- {b} and {a}\n") };

    // Two unjudged notes linked as contradicting are asked side by side first; the answer
    // keeps both and links one as contradicting a note checked before, which the run asks
    // beside it in the same run. A recheck confirms nothing.
    service.link(&target, &person, &twice, &once, contradicts, None).await.expect("link");
    for checked in [&at_once, &next_day] {
        service.mark(&target, &person, checked, swiss_memory::model::Verdict::Verified, None).await.expect("mark");
    }
    let raised = wrapped(
        &json!({
            "verdicts": [{ "id": twice, "verdict": "keep" }, { "id": once, "verdict": "keep" }],
            "links": [{ "from": twice, "to": at_once, "kind": "contradicts", "note": "the other says the opposite" }]
        })
        .to_string(),
    );
    let runner = Scripted::new(vec![(pair(&twice, &once).as_str(), raised), (SETTLE, KEEP_ALL.to_string())]);
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(true, None, None)).await.expect("enqueue");
    let dry = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(dry.run.summary, "would recheck 2 note(s) in 0 domain(s): 1 question(s), 0 for a later run");
    service.sync_finish(&dry.run, None).await.expect("finish");
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, None, None)).await.expect("enqueue");
    let done = execute_recheck(&service, run, &runner, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let asked = runner.asked(SETTLE);
    assert_eq!(asked.len(), 2, "the planned pair, then the contradiction its answer raised");
    assert!(runner.asked(RECHECK).is_empty(), "no note was left to judge alone");
    assert!(asked[0].contains("## The contradictions\n") && asked[0].contains(&pair(&twice, &once)), "{}", asked[0]);
    assert!(asked[1].contains(&pair(&twice, &at_once)), "{}", asked[1]);
    assert!(asked[1].contains(&format!("### {twice} ")) && asked[1].contains(&format!("### {at_once} ")), "{}", asked[1]);
    assert!(!asked[1].contains("still linked as contradicting"), "asked side by side once: {}", asked[1]);
    let scope = done.run.scope.recheck.clone().expect("recheck");
    assert_eq!(
        (scope.notes, scope.questions, scope.asked, scope.kept, scope.rounds, scope.lifted, scope.open, scope.confirmed),
        (4, 2, 2, 4, 1, 2, 0, 0),
        "{}",
        done.run.summary
    );
    assert!(done.run.summary.starts_with("rechecked 4 of 4 note(s)"), "{}", done.run.summary);
    assert!(done.run.summary.contains("; 2 contradiction(s) lifted (1 more round(s) for the contradictions the answers raised)"), "{}", done.run.summary);
    assert!(!done.run.summary.contains("confirmed") && !done.run.summary.contains("still open"), "{}", done.run.summary);
    service.sync_finish(&done.run, None).await.expect("finish");
    for id in [&twice, &once, &at_once] {
        let (record, _) = service.get(&target, id, false).await.expect("get");
        assert!(record.verified_ms.is_some() && !record.summary.confirmed, "{id}");
        assert!(record.links.iter().all(|l| l.kind != contradicts), "settled: {id}");
    }

    // An answer that links its note as contradicting another each time: the pair asked
    // side by side twice, told the second time, then left open - the run ends.
    let fresh = drafted("A refund is never part of the cancel request", "cancel() only marks the order.", ORDER_SERVICE, PLACE);
    let saved = service.knowledge_save(&project, "r-0000000002", None, fresh).await;
    assert!(matches!(saved, Saved::New { held: false }), "not saved: {saved:?}");
    let (items, _) = service.list(&target, &swiss_memory::service::ListRequest::default()).await.expect("list");
    let fresh = items.iter().find(|s| s.title.contains("never part of")).map(|s| s.id.clone()).expect("fresh");
    let again = wrapped(
        &json!({
            "verdicts": [{ "id": fresh, "verdict": "keep" }],
            "links": [{ "from": fresh, "to": next_day, "kind": "contradicts", "note": "the other says the opposite" }]
        })
        .to_string(),
    );
    let stubborn = Scripted::new(vec![(RECHECK, again.clone()), (SETTLE, again)]);
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, None, None)).await.expect("enqueue");
    let left = execute_recheck(&service, run, &stubborn, &Stop::default()).await;
    assert_eq!(left.run.state, RunState::Ok, "{}", left.run.error.clone().unwrap_or_default());
    assert_eq!(stubborn.asked(RECHECK).len(), 1);
    let asked = stubborn.asked(SETTLE);
    assert_eq!(asked.len(), 2, "the pair asked side by side twice, no more");
    assert!(!asked[0].contains("still linked as contradicting"), "{}", asked[0]);
    assert!(asked[1].contains(&format!("it was judged beside {next_day} earlier in this run")), "{}", asked[1]);
    let scope = left.run.scope.recheck.clone().expect("recheck");
    assert_eq!((scope.asked, scope.rounds, scope.lifted, scope.open), (3, 2, 0, 1), "{}", left.run.summary);
    assert!(left.run.summary.contains("; 1 contradiction(s) between the model's notes still open"), "{}", left.run.summary);
    service.sync_finish(&left.run, None).await.expect("finish");
    let (record, _) = service.get(&target, &next_day, false).await.expect("get");
    assert!(record.links.iter().any(|l| l.kind == contradicts), "the contradiction stays for a person");
}

/// A runner that looks with tools: [Ask::look] answers, counted.
struct Looks {
    inner: Scripted,
    looked: AtomicUsize,
}

#[async_trait]
impl Ask for Looks {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
        self.inner.ask(root, prompt).await
    }

    async fn fill(&self, _root: &Path, _prompt: &str, _schema: &Value) -> Result<String, String> {
        Err("a looking runner is never asked to fill a recheck".into())
    }

    async fn look(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.looked.fetch_add(1, Ordering::SeqCst);
        self.inner.ask(root, prompt).await
    }

    fn looks(&self) -> bool {
        true
    }

    fn fills_at_once(&self) -> usize {
        1
    }

    fn label(&self) -> String {
        "looks".into()
    }

    fn model(&self) -> Option<String> {
        None
    }
}
