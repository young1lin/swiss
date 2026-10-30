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

use super::*;

/// A review run of the changed notes as the clock reads one: queued, started and ended
/// `state` around `queued`, leaving `more` questions for the next.
fn review(queued: i64, state: &str, more: usize) -> Run {
    serde_json::from_value(json!({
        "id": format!("r-{queued}"), "project": "p", "trigger": "review", "dryRun": false, "state": state,
        "queuedMs": queued, "dueMs": queued, "startedMs": queued, "finishedMs": queued + 1,
        "scope": { "changed": true, "recheck": { "more": more } },
    }))
    .expect("run")
}

#[test]
fn the_clock_queues_a_days_reviews_while_the_one_before_left_questions_for_the_next() {
    let due = 1_000_000;
    // Nothing yet today: the first is queued; yesterday's runs say nothing of today.
    assert_eq!(next_review(&[], due, 0, 3), Turn::Queue);
    assert_eq!(next_review(&[review(due - 500, "ok", 4)], due, 0, 3), Turn::Queue);
    // One waits or runs: it is waited out.
    let mut waiting = review(due + 10, "queued", 0);
    (waiting.started_ms, waiting.finished_ms) = (None, None);
    assert_eq!(next_review(&[waiting], due, 1, 3), Turn::Wait);
    // It ended leaving questions for the next: another, while the day has runs left.
    let first = review(due + 10, "ok", 4);
    assert_eq!(next_review(std::slice::from_ref(&first), due, 1, 3), Turn::Queue);
    let second = review(due + 20, "ok", 2);
    assert_eq!(next_review(&[second.clone(), first.clone()], due, 2, 3), Turn::Queue);
    let third = review(due + 30, "ok", 1);
    assert_eq!(next_review(&[third, second.clone(), first.clone()], due, 3, 3), Turn::Settled, "the day's runs are spent");
    assert_eq!(next_review(&[second.clone(), first.clone()], due, 2, 2), Turn::Settled, "a smaller day");
    assert_eq!(next_review(&[second, first], due, 0, 2), Turn::Settled, "a review asked by hand today counts too");
    // All asked, nothing to ask, a failure or a cancel: the day is done.
    for (state, more) in [("ok", 0), ("empty", 0), ("failed", 3), ("cancelled", 3)] {
        assert_eq!(next_review(&[review(due + 10, state, more)], due, 1, 3), Turn::Settled, "{state}");
    }
    // The kept runs lost the clock's review: its memory settles the day.
    assert_eq!(next_review(&[], due, 1, 3), Turn::Settled);
    // A run in flight at the time counts for the day; a dry one never does.
    let mut across = review(due - 100, "ok", 0);
    across.finished_ms = Some(due + 100);
    assert_eq!(next_review(&[across], due, 0, 3), Turn::Settled);
    let mut dry = review(due + 10, "ok", 0);
    dry.dry_run = true;
    assert_eq!(next_review(&[dry], due, 0, 3), Turn::Queue);
}

#[tokio::test]
async fn the_clock_takes_the_days_runs_from_the_settings() {
    let dir = scratch("daily-review");
    let (service, project) = service_with_project("daily-review", &dir).await;
    service.patch_project_settings(&project, &json!({ "autoReview": "03:30", "autoReviewRuns": 2 })).await.expect("settings");
    let auto = swiss_memory::settings::AutoReview { project: project.clone(), slot: "03:30".into(), runs: 2 };
    assert_eq!(service.auto_reviews().await.expect("reviews"), [auto]);
    let global = service.patch_project_settings(swiss_memory::GLOBAL_PROJECT_ID, &json!({ "autoReviewRuns": 2 })).await;
    assert!(global.expect_err("the global scope").message.contains("nothing to review"));

    // A review that ran at today's time and left questions for the next.
    let due = now_ms();
    let leave = |more: usize| {
        let service = &service;
        async move {
            let swiss_memory::sync::Next::Run(run) = service.sync_next(now_ms()).await.expect("next") else { panic!("a queued run") };
            let mut run = *run;
            run.state = RunState::Ok;
            run.finished_ms = Some(now_ms());
            run.scope.recheck = Some(RecheckScope { more, ..RecheckScope::default() });
            service.sync_finish(&run, None).await.expect("finish");
        }
    };
    service.sync_enqueue(&project, review_enqueue(false, None, None, true)).await.expect("enqueue");
    leave(4).await;
    let mut days = HashMap::new();
    assert_eq!(tick(&service, due, &mut days).await, 1, "the next questions get a run");
    assert_eq!(tick(&service, due, &mut days).await, 0, "it is waited out");
    leave(2).await;
    assert_eq!(tick(&service, due, &mut days).await, 0, "two runs are the day's");
    assert_eq!(days[&project], ReviewDay { due, queued: 1, settled: true });

    // Left out, the day takes the default.
    service.patch_project_settings(&project, &json!({ "autoReviewRuns": null })).await.expect("settings");
    assert_eq!(service.auto_reviews().await.expect("reviews")[0].runs, swiss_memory::settings::AUTO_REVIEW_RUNS_DEFAULT);
    assert_eq!(tick(&service, due, &mut HashMap::new()).await, 1, "a third run under the default");
}

/// One clock tick now, with today's time at `due`.
async fn tick(service: &MemoryService, due: i64, days: &mut HashMap<String, ReviewDay>) -> usize {
    auto_review_tick(service, now_ms(), move |_| Some(due), days).await.expect("tick")
}
