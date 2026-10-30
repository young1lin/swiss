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

//! The daily review (SPEC §memory.settings autoReview): a project's notes whose code
//! changed handed to a review run once a day, at its time of day.

use super::*;

/// How often the autoReview clock looks at the projects' times of day.
pub(super) const AUTO_REVIEW_TICK: Duration = Duration::from_secs(60);

/// The autoReview clock (SPEC §memory.settings autoReview): once a minute, each project
/// whose settings name a time of day gets a review run of its changed notes queued once
/// that time has come today, unless a review run already went at or after it - and another
/// after it while the one before left questions for the next, up to the day's runs. A
/// gateway down at that time queues the run when it is back the same day; a day it is down
/// whole is skipped, not caught up. The run asks whom a review run asks.
pub async fn review_clock(
    service: std::sync::Arc<MemoryService>,
    seats: std::sync::Arc<crate::memory_api::MemoryState>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let mut days = HashMap::new();
    loop {
        tokio::select! {
            _ = cancel.changed() => return,
            _ = tokio::time::sleep(AUTO_REVIEW_TICK) => {}
        }
        match auto_review_tick(&service, now_ms(), today_at, &mut days).await {
            Ok(0) => {}
            Ok(_) => seats.wake_sync(),
            Err(err) => log::warn("memory auto-review could not read the projects' settings", Some(json!({ "err": err.message }))),
        }
    }
}

/// Today's `HH:MM` on the gateway's clock, as unix ms. A time the clock skips (the
/// spring-forward gap) is the hour after; a repeated one, its first pass.
pub fn today_at(slot: &str) -> Option<i64> {
    use chrono::TimeZone as _;
    let (hour, minute) = settings::slot_of(slot)?;
    let at = chrono::Local::now().date_naive().and_hms_opt(hour, minute, 0)?;
    let local = |t: chrono::NaiveDateTime| chrono::Local.from_local_datetime(&t).earliest();
    local(at).or_else(|| local(at + chrono::TimeDelta::hours(1))).map(|t| t.timestamp_millis())
}

/// What the clock remembers of a project's day while the gateway runs: the due time it is
/// for, the review runs it queued since, and whether the day is settled - nothing more to
/// queue until the next. The kept runs ([engine::RUNS_KEPT]) can lose a busy day's review
/// to the runs after it; this memory still speaks for the day.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewDay {
    pub due: i64,
    pub queued: usize,
    pub settled: bool,
}

/// What the clock does about a project's day on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Turn {
    /// Queue a review run of the changed notes.
    Queue,
    /// A review run waits or runs: look again on the next tick.
    Wait,
    /// The day is done.
    Settled,
}

/// One tick of [review_clock]: each project whose time of day `due_of` puts at or before
/// `now` gets a review of its whole inbox queued as [next_review] says. `days` is the
/// clock's memory, a [ReviewDay] a project; an enqueue refused settles the day. Out: how
/// many were queued.
pub async fn auto_review_tick(
    service: &MemoryService,
    now: i64,
    due_of: impl Fn(&str) -> Option<i64>,
    days: &mut HashMap<String, ReviewDay>,
) -> MemResult<usize> {
    let mut queued = 0;
    for auto in service.auto_reviews().await? {
        let Some(due) = due_of(&auto.slot).filter(|due| *due <= now) else { continue };
        let day = days.entry(auto.project.clone()).or_default();
        if day.due != due {
            *day = ReviewDay { due, ..ReviewDay::default() };
        }
        if day.settled {
            continue;
        }
        let runs = match service.sync_runs(&auto.project, engine::RUNS_KEPT).await {
            Ok(runs) => runs,
            Err(err) => {
                log::warn("memory auto-review could not read a project's runs", Some(json!({ "project": auto.project, "err": err.message })));
                continue;
            }
        };
        match next_review(&runs, due, day.queued, auto.runs) {
            Turn::Wait => {}
            Turn::Settled => day.settled = true,
            Turn::Queue => match service.sync_enqueue(&auto.project, review_enqueue(false, None, None, false)).await {
                Ok(_) => {
                    day.queued += 1;
                    queued += 1;
                }
                Err(err) => {
                    log::warn("memory auto-review could not queue a run", Some(json!({ "project": auto.project, "err": err.message })));
                    day.settled = true;
                }
            },
        }
    }
    Ok(queued)
}

/// The clock's turn on a project's day, from its runs (newest first), the day's `due` time,
/// the review runs the clock `queued` since, and the day's `cap` on them. A review run (the
/// whole inbox or its changed notes, not a dry one) that waits or runs is waited out. The
/// day's reviews are those queued, started or finished at or after `due`: none, and the
/// clock queued none, queues the first; none though the clock queued one means the kept
/// runs lost it, and the day is done. Else the newest decides: one that ended ok leaving
/// questions for the next queues another while the day has runs left; anything else - all
/// asked, nothing to ask, a failure, a cancel - settles the day, a failure's retry is
/// tomorrow's.
pub(super) fn next_review(runs: &[Run], due: i64, queued: usize, cap: usize) -> Turn {
    let reviews = || runs.iter().filter(|r| r.trigger == Trigger::Review && !r.dry_run);
    if reviews().any(|r| !r.state.is_done()) {
        return Turn::Wait;
    }
    let at_or_after = |ms: Option<i64>| ms.is_some_and(|ms| ms >= due);
    let today: Vec<&Run> =
        reviews().filter(|r| r.queued_ms >= due || at_or_after(r.started_ms) || at_or_after(r.finished_ms)).collect();
    let Some(newest) = today.first() else {
        return if queued == 0 { Turn::Queue } else { Turn::Settled };
    };
    let more = newest.state == RunState::Ok && newest.scope.recheck.as_ref().is_some_and(|scope| scope.more > 0);
    if more && today.len().max(queued) < cap {
        Turn::Queue
    } else {
        Turn::Settled
    }
}
