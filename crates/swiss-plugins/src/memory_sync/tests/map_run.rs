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

//! The map run: the domains drawn, the skeleton, the redraw, the notes filed, the stop.

use super::*;

/// Two business directories of three files each: the map draws `billing` and `orders`.
fn code_repo(label: &str) -> PathBuf {
    let dir = scratch(label);
    git_at(&dir, &["init", "-q"]);
    git_at(&dir, &["config", "user.email", "t@example.com"]);
    git_at(&dir, &["config", "user.name", "T"]);
    for (area, files) in [("billing", ["invoice", "refund", "tax"]), ("orders", ["cart", "checkout", "order"])] {
        std::fs::create_dir_all(dir.join("src").join(area)).expect("dir");
        for file in files {
            let source = format!("pub fn {file}_total(amount: u64) -> u64 {{\n    amount\n}}\n");
            std::fs::write(dir.join("src").join(area).join(format!("{file}.rs")), source).expect("write");
        }
    }
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "the shop"]);
    dir
}

#[tokio::test]
async fn a_map_run_draws_the_domains_and_starts_the_sync() {
    if !git_present() {
        return;
    }
    let repo_dir = code_repo("map");
    let (service, project) = mapped_service("map", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, Some("zh".into()), false)).await.expect("enqueue");
    let canned = Canned::new(vec![fill_reply("Billing", &["refund", "退款"]), fill_reply("Orders", &["cart", "购物车"])]);
    let done = execute_map(&service, run, &quiet_config(), Some(&canned), None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!((done.run.scope.units, done.run.scope.asks, done.run.scope.filled), (2, 2, 2));
    assert_eq!(done.run.runner.attempts, 2);
    let created: Vec<&str> = done.run.ops.iter().filter(|o| o.op == "domain.create").map(|o| o.target.as_str()).collect();
    assert_eq!(created, ["billing", "orders"], "{:?}", done.run.ops);
    {
        let prompts = canned.prompts.lock().expect("lock");
        assert!(prompts.iter().all(|p| p.contains("Simplified Chinese")), "the language rides in every question");
    }
    let advance = done.advance.expect("a project that never synced starts here");
    assert_eq!((advance.head.as_str(), advance.commits.len()), (head_of(&repo_dir).as_str(), 0));
    service.sync_finish(&done.run, Some(&advance)).await.expect("finish");
    let map = service.domains(&swiss_memory::service::Target::project(&project)).await.expect("map");
    let billing = map.items.iter().find(|d| d.path == "billing").expect("billing");
    assert_eq!(billing.title, "Billing");
    assert!(billing.keywords.iter().any(|k| k == "退款"), "{:?}", billing.keywords);
    let detail = service.domain(&swiss_memory::service::Target::project(&project), "billing", false).await.expect("hub");
    let body = detail.domain.body.clone().unwrap_or_default();
    assert!(body.contains("src/billing/refund.rs"), "a checked path stays: {body}");
    assert!(!body.contains("src/nowhere.rs"), "a made-up path is dropped: {body}");
    let state = service.sync_state(&project).await.expect("state");
    assert_eq!(state.head.as_deref(), Some(head_of(&repo_dir).as_str()));

    // Again: what the map has is kept, nothing is asked, the sync keeps its own head.
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("enqueue");
    let canned = Canned::new(vec![fill_reply("Never", &[])]);
    let again = execute_map(&service, run, &quiet_config(), Some(&canned), None, &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Ok, "{}", again.run.error.clone().unwrap_or_default());
    assert_eq!((again.run.scope.asks, again.run.ops.len()), (0, 0));
    assert_eq!(canned.calls.load(Ordering::SeqCst), 0);
    assert!(again.advance.is_none());
}

#[tokio::test]
async fn a_map_without_a_runner_is_the_skeleton_and_a_dry_one_writes_nothing() {
    if !git_present() {
        return;
    }
    let repo_dir = code_repo("skeleton");
    let (service, project) = mapped_service("skeleton", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, map_enqueue(true, None, false)).await.expect("enqueue");
    let done = execute_map(&service, run, &quiet_config(), None, None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let proposal = done.run.proposal.clone().expect("a dry run keeps the proposal");
    let paths: Vec<&str> = proposal.domains.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, ["billing", "orders"]);
    assert!(proposal.summary.contains("described 0"), "{}", proposal.summary);
    assert!(done.run.ops.is_empty() && done.advance.is_none());
    let map = service.domains(&swiss_memory::service::Target::project(&project)).await.expect("map");
    assert!(map.items.is_empty(), "a dry run writes nothing");

    // A runner that describes nothing is broken: nothing is drawn.
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("enqueue");
    let done = execute_map(&service, run, &quiet_config(), Some(&Canned::new(vec![not_json_reply()])), None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Failed);
    assert!(done.run.error.as_deref().unwrap_or_default().contains("described none"), "{:?}", done.run.error);
    assert_eq!(done.run.runner.attempts, 4, "each area is asked once more");
    let map = service.domains(&swiss_memory::service::Target::project(&project)).await.expect("map");
    assert!(map.items.is_empty());
}

#[tokio::test]
async fn map_runs_and_sync_runs_queue_apart() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("apart");
    let (service, project) = service_with_project("apart", &repo_dir).await;
    let (sync, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("sync");
    let (map, merged) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("map");
    assert!(!merged && map.id != sync.id && map.trigger == Trigger::Map);
    let (again, merged) = service.sync_enqueue(&project, map_enqueue(false, Some("ja".into()), false)).await.expect("again");
    assert!(merged && again.id == map.id);
    assert_eq!(again.scope.language.as_deref(), Some("ja"), "the newer language wins");
    let (manual, merged) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("manual");
    assert!(merged && manual.id == sync.id && manual.trigger == Trigger::Manual);
    let since = Enqueue { since: Some("HEAD~1".into()), ..map_enqueue(false, None, false) };
    assert!(service.sync_enqueue(&project, since).await.is_err(), "a map takes no since");
    assert!(service.sync_enqueue(&project, map_enqueue(false, Some("zh; rm".into()), false)).await.is_err());
}

#[tokio::test]
async fn a_map_stopped_while_drawn_lands_nothing() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("stop-map");
    let (service, project) = mapped_service("stop-map", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, full_enqueue(false, None, false, None)).await.expect("enqueue");
    let stop = Stop::default();
    let runner =
        StopsAt { inner: shop_runner(commerce_review()), stop: stop.clone(), words: AREA, after: 0, seen: AtomicUsize::new(0) };
    let config = quiet_config();
    let work = execute_full(&service, run, &config, &runner, None, &stop);
    let halted = tokio::time::timeout(Duration::from_secs(60), work).await.expect("a raised stop ends the run, never hangs it");
    assert_eq!(halted.run.state, RunState::Cancelled, "{}", halted.run.error.clone().unwrap_or_default());
    assert_eq!(halted.run.summary, MAP_CANCELLED);
    assert_eq!(halted.run.scope.phase, Some(Phase::Areas));
    assert!(halted.run.scope.knowledge.is_none(), "no slice was cut");
    assert!(paths_of(&service, &project).await.is_empty(), "a map cut short lands nothing");
}

#[tokio::test]
async fn a_redraw_draws_again_only_what_nobody_touched() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("redraw");
    let (service, project) = mapped_service("redraw", &repo_dir).await;
    let target = Target::project(&project);
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("enqueue");
    let skeleton = execute_map(&service, run, &quiet_config(), None, None, &Stop::default()).await;
    assert_eq!(skeleton.run.state, RunState::Ok, "{}", skeleton.run.error.clone().unwrap_or_default());
    service.sync_finish(&skeleton.run, skeleton.advance.as_ref()).await.expect("finish");
    let drawn = paths_of(&service, &project).await;
    for path in ["order", "payment", "customer", "config"] {
        assert!(drawn.iter().any(|p| p == path), "{path} not in {drawn:?}");
    }
    // A person confirms `payment` and files a note under `order`.
    service.confirm_domain(&target, &swiss_memory::model::Actor::panel(), "payment").await.expect("confirm");
    let note = swiss_memory::service::SaveRequest {
        kind: swiss_memory::model::Kind::Knowledge,
        title: "Orders ship from one warehouse".into(),
        content: "The order service assumes a single stock location.".into(),
        anchors: vec![ORDER_SERVICE.into()],
        refs: Vec::new(),
        keywords: Vec::new(),
        links: Vec::new(),
        replaces: None,
        allow_similar: false,
        domain: Some("order".into()),
        evidence: Vec::new(),
    };
    service.save(&target, &swiss_memory::model::Actor::panel(), note).await.expect("save");
    let mut removable: Vec<String> = drawn.iter().filter(|p| *p != "payment").cloned().collect();
    removable.sort();

    // Dry, its review unreadable: the draft stands, the removal is only listed.
    let runner = Scripted::new(vec![(REVIEW, not_json_reply()), (AREA, fill_reply("Area", &[]))]);
    let (run, _) = service.sync_enqueue(&project, map_enqueue(true, None, true)).await.expect("enqueue");
    let dry = execute_map(&service, run, &quiet_config(), Some(&runner), None, &Stop::default()).await;
    assert_eq!(dry.run.state, RunState::Ok, "{}", dry.run.error.clone().unwrap_or_default());
    assert_eq!(dry.run.scope.removed, removable);
    assert!(dry.run.scope.regroup.is_none());
    assert!(dry.run.summary.contains("the review of the top level"), "{}", dry.run.summary);
    assert_eq!(runner.asked(REVIEW).len(), 2, "an unreadable review is asked once more");
    let planned: Vec<String> = dry.run.proposal.clone().expect("proposal").domains.into_iter().map(|d| d.path).collect();
    assert!(planned.iter().any(|p| p == "order") && !planned.iter().any(|p| p == "payment"), "{planned:?}");
    assert_eq!(paths_of(&service, &project).await, drawn, "a dry run removes nothing");
    service.sync_finish(&dry.run, None).await.expect("finish");

    // For real, `order` renamed by the review: the untouched domains go and are drawn
    // again, the confirmed one stays, the note follows its file into the new domain.
    let rename = wrapped(&json!({ "domains": [{ "path": "ordering", "title": "Ordering", "members": ["order"] }] }).to_string());
    let runner = Scripted::new(vec![(REVIEW, rename), (AREA, fill_reply("Area", &[]))]);
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, true)).await.expect("enqueue");
    let done = execute_map(&service, run, &quiet_config(), Some(&runner), None, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.scope.removed, removable);
    let paths = paths_of(&service, &project).await;
    assert!(paths.iter().any(|p| p == "ordering") && paths.iter().any(|p| p == "payment"), "{paths:?}");
    assert!(!paths.iter().any(|p| p == "order"), "{paths:?}");
    assert_eq!(titles_in(&service, &project, "ordering").await, ["Orders ship from one warehouse"]);
    service.sync_finish(&done.run, None).await.expect("finish");

    // A redraw goes with a map or a full run only.
    let redraw_sync = Enqueue { redraw: true, ..manual_enqueue(false, None) };
    assert!(service.sync_enqueue(&project, redraw_sync).await.is_err());
    let capped_map = Enqueue { max_asks: Some(3), ..map_enqueue(false, None, false) };
    assert!(service.sync_enqueue(&project, capped_map).await.is_err());
}

#[tokio::test]
async fn a_map_run_files_the_notes_saved_before_their_code_had_a_domain() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("late-area");
    let (service, project) = mapped_service("late-area", &repo_dir).await;
    let target = Target::project(&project);
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("enqueue");
    let first = execute_map(&service, run, &quiet_config(), None, None, &Stop::default()).await;
    assert_eq!(first.run.state, RunState::Ok, "{}", first.run.error.clone().unwrap_or_default());
    service.sync_finish(&first.run, first.advance.as_ref()).await.expect("finish");

    // Invoices land after the map: a note on them is saved with no domain owning its file.
    let base = repo_dir.join("src/main/java/com/acme/shop");
    for (layer, name) in [("controller", "InvoiceController"), ("service", "InvoiceService"), ("repository", "InvoiceRepository")] {
        let source = format!("package com.acme.shop.{layer};\n\npublic class {name} {{}}\n");
        std::fs::write(base.join(layer).join(format!("{name}.java")), source).expect("write");
    }
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "invoices"]);
    let note = swiss_memory::service::SaveRequest {
        kind: swiss_memory::model::Kind::Knowledge,
        title: "An invoice is issued once per order".into(),
        content: "The invoice service refuses a second invoice for one order.".into(),
        anchors: vec!["src/main/java/com/acme/shop/service/InvoiceService.java".into()],
        refs: Vec::new(),
        keywords: Vec::new(),
        links: Vec::new(),
        replaces: None,
        allow_similar: false,
        domain: None,
        evidence: Vec::new(),
    };
    service.save(&target, &swiss_memory::model::Actor::panel(), note).await.expect("save");
    assert!(!paths_of(&service, &project).await.iter().any(|p| p == "invoice"));

    // The next map run draws `invoice`, and the note follows its file into it.
    let (run, _) = service.sync_enqueue(&project, map_enqueue(false, None, false)).await.expect("enqueue");
    let second = execute_map(&service, run, &quiet_config(), None, None, &Stop::default()).await;
    assert_eq!(second.run.state, RunState::Ok, "{}", second.run.error.clone().unwrap_or_default());
    assert!(second.run.ops.iter().any(|o| o.op == "domain.create" && o.target == "invoice"), "{:?}", second.run.ops);
    assert_eq!(titles_in(&service, &project, "invoice").await, ["An invoice is issued once per order"]);
}
