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

//! The docs run: the documents kept and carded with the code they name.

use super::*;

#[tokio::test]
async fn a_docs_run_keeps_the_documents_and_cards_them_with_the_code_they_name() {
    if !git_present() {
        return;
    }
    let repo_dir = java_repo("docs");
    let skill = ".claude/skills/logic";
    let doc = format!("{skill}/order/pitfalls.md");
    let plan = format!("{skill}/plan.md");
    std::fs::create_dir_all(repo_dir.join(skill).join("order")).expect("dir");
    std::fs::write(repo_dir.join(&doc), "# 下单踩坑\n## 重复下单\n重试会重复下单：OrderService.place 没有幂等键。\n").expect("write");
    std::fs::write(repo_dir.join(&plan), "# Plan: idempotency\nAdd a key to place().\n").expect("write");
    let (service, project) = mapped_service("docs", &repo_dir).await;
    let target = Target::project(&project);

    // No source: nothing to read, and the summary says how to name one.
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(false, None, None)).await.expect("enqueue");
    assert_eq!(run.trigger, Trigger::Docs);
    let none = execute_docs(&service, run, Err("unused".into()), &Stop::default()).await;
    assert_eq!(none.run.state, RunState::Empty);
    assert!(none.run.summary.contains("docs add"), "{}", none.run.summary);

    // Dry: counted, nothing kept.
    service.patch_project_settings(&project, &json!({ "docs": [skill] })).await.expect("sources");
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(true, None, None)).await.expect("enqueue");
    let dry = execute_docs(&service, run, Err("unused".into()), &Stop::default()).await;
    assert_eq!(dry.run.state, RunState::Ok, "{}", dry.run.error.clone().unwrap_or_default());
    let scope = dry.run.scope.docs.clone().expect("scope");
    assert_eq!((scope.files, scope.added, scope.waiting, scope.questions), (2, 2, 2, 1), "{scope:?}");
    assert!(service.docs_list(&target).await.expect("list").docs.is_empty(), "a dry run keeps nothing");

    // No workers: the read lands and the documents wait for a card.
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(false, None, None)).await.expect("enqueue");
    let read = execute_docs(&service, run, Err("no llm agent to ask".into()), &Stop::default()).await;
    assert_eq!(read.run.state, RunState::Ok);
    assert!(read.run.summary.contains("no llm agent to ask: 2 document(s) wait for a card"), "{}", read.run.summary);

    // The workers card them; each code name lands where the code index defines it -
    // `OrderService.place` in OrderService.java, though every class defines a place().
    let cards = wrapped(
        &json!({ "docs": [
            { "path": doc, "kind": "pitfalls", "summary": "重试会重复下单：place() 缺幂等键。", "domain": "",
              "keywords": ["幂等", "idempotency"], "symbols": ["OrderService.place", "NoSuchThing"] },
            { "path": plan, "kind": "plan", "summary": "Add an idempotency key to place().", "domain": "",
              "keywords": ["idempotency"], "symbols": [] }
        ] })
        .to_string(),
    );
    let runner = Scripted::new(vec![("You file documents", cards)]);
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(false, Some("zh".into()), Some(5))).await.expect("enqueue");
    let done = execute_docs(&service, run, Ok(&runner), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    let scope = done.run.scope.docs.clone().expect("scope");
    assert_eq!((scope.waiting, scope.asked, scope.carded, scope.symbols, scope.dropped), (2, 1, 2, 1, 1), "{scope:?}");
    let prompt = runner.asked("You file documents").pop().expect("asked");
    assert!(prompt.contains(&format!("### `{doc}`")) && prompt.contains("Simplified Chinese"), "{prompt}");
    let hits = service.docs_search(&target, "重复下单", 3, None).await.expect("search");
    assert_eq!(hits[0].doc.path, doc);
    assert_eq!(hits[0].doc.kind, Some(docs::DocKind::Pitfalls));
    let named: Vec<&str> = hits[0].doc.symbols.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(named, ["src/main/java/com/acme/shop/service/OrderService.java"]);
    assert!(hits[0].doc.card_by.as_deref() == Some("scripted"));

    // Every document carded and none changed: the next run has nothing to do.
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(false, None, None)).await.expect("enqueue");
    let again = execute_docs(&service, run, Ok(&runner), &Stop::default()).await;
    assert_eq!(again.run.state, RunState::Empty, "{}", again.run.summary);
    assert_eq!(runner.asked("You file documents").len(), 1, "nothing asked again");

    // A name the card gave and the code lacked is defined since: the next run finds it,
    // the card's names kept and looked up again, no model asked.
    std::fs::write(
        repo_dir.join("src/main/java/com/acme/shop/service/NoSuchThing.java"),
        "package com.acme.shop.service;\n\npublic class NoSuchThing {}\n",
    )
    .expect("write");
    let (run, _) = service.sync_enqueue(&project, docs_enqueue(false, None, None)).await.expect("enqueue");
    let relinked = execute_docs(&service, run, Ok(&runner), &Stop::default()).await;
    assert_eq!(relinked.run.state, RunState::Ok, "{}", relinked.run.summary);
    assert_eq!(relinked.run.scope.docs.clone().expect("scope").relinked, 1);
    assert!(relinked.run.summary.contains("1 card(s) re-linked to the code as it is now"), "{}", relinked.run.summary);
    assert_eq!(runner.asked("You file documents").len(), 1, "nothing asked again");
    let hits = service.docs_search(&target, "NoSuchThing", 3, None).await.expect("search");
    assert_eq!(hits[0].doc.path, doc, "the name found joins the words its document is found by");
    let named: Vec<&str> = hits[0].doc.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(named, ["OrderService.place", "NoSuchThing"]);
}
