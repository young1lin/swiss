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

//! A recheck and a review run (SPEC §memory.init The recheck, The review run): the notes
//! judged against the code in batches, the answers applied, the contradictions they
//! raise settled side by side.

use super::*;

/// What a recheck stands on: the workers that judge, the checkout, the project's name,
/// the checkout's files and the notes it judges (a review run's: the inbox).
pub(super) struct RecheckAt<'a> {
    pub(super) workers: &'a dyn Ask,
    pub(super) root: &'a Path,
    pub(super) project: &'a str,
    pub(super) sources: &'a [String],
    pub(super) reach: recheck::Reach,
}

/// The recheck (SPEC §memory.init The recheck): the notes a model wrote and nobody judged
/// since, at most `cap` questions, asked of the workers at their concurrency - never a
/// lead's loop with delegation (a turn each, at the lead's effort, three questions at a
/// time: 8 minutes a question, 10 hours for a 2,000-note project). Each question shows
/// the code its notes name. Workers that can look work each question in their own loop -
/// open what was not shown, search for the names a note gives - so a question holds
/// fewer notes and less code ([recheck::LOOKING]); workers that cannot answer it in one
/// request from what is shown ([recheck::ONE_REQUEST]). Each answer lands as it comes; the
/// contradictions its answers raise or leave standing are then settled ([settle]).
/// Out: the counts, the failures, and whether the stop cut it short.
pub(super) async fn recheck_phase(
    service: &MemoryService,
    run: &mut Run,
    at: RecheckAt<'_>,
    cap: usize,
    info: &mut RunnerInfo,
    started: i64,
    stop: &Stop,
) -> (RecheckScope, Vec<String>, bool) {
    let mut failures: Vec<String> = Vec::new();
    let looking = at.workers.looks();
    let plan = match service.recheck_plan(&run.project, at.sources, recheck::caps(looking), at.reach).await {
        Ok(plan) => plan,
        Err(err) => {
            failures.push(err.message);
            return (RecheckScope::default(), failures, false);
        }
    };
    let questions = plan.batches.len().min(cap);
    let mut scope = RecheckScope {
        domains: plan.domains,
        notes: plan.notes,
        questions,
        more: plan.batches.len() - questions,
        ..RecheckScope::default()
    };
    run.scope.recheck = Some(scope.clone());
    if questions == 0 {
        return (scope, failures, false);
    }
    progress(service, run, Phase::Recheck).await;
    let batches: Vec<recheck::Batch> = plan.batches.into_iter().take(questions).collect();
    // How often each contradiction was asked side by side this run.
    let mut sides: HashMap<(String, String), usize> = HashMap::new();
    for pair in batches.iter().flat_map(|b| b.contradicts.iter()) {
        *sides.entry(pair.clone()).or_default() += 1;
    }
    let mut asking = Asking { scope: &mut scope, failures: &mut failures, near: HashSet::new(), info, started, stop };
    recheck_round(service, run, &at, batches, &mut asking).await;
    settle(service, run, &at, cap, &mut sides, &mut asking).await;
    run.scope.recheck = Some(scope.clone());
    let stopped = stop.raised() && scope.asked < scope.questions;
    (scope, failures, stopped)
}

/// Rounds a recheck or review run asks after its planned questions to settle the
/// contradictions its answers raised or left standing, and how often one contradiction is
/// asked side by side.
pub(super) const SETTLE_ROUNDS: usize = 3;
pub(super) const SIDE_BY_SIDE: usize = 2;

/// What a recheck's questions tally into as they land.
pub(super) struct Asking<'a> {
    pub(super) scope: &'a mut RecheckScope,
    pub(super) failures: &'a mut Vec<String>,
    /// The notes the answers judged or linked as contradicting: where a recheck's
    /// contradictions are looked for once its planned questions are asked.
    pub(super) near: HashSet<String>,
    pub(super) info: &'a mut RunnerInfo,
    pub(super) started: i64,
    pub(super) stop: &'a Stop,
}

/// The contradictions a run's answers raised or left standing, settled in the same run
/// (SPEC §memory.init The recheck, The review run): a review run reads the inbox again - a
/// note linked as contradicting one outside it puts that one in it - a recheck the
/// contradictions between two of the model's unconfirmed notes, one the answers judged or
/// linked; each is asked side by side, a pair asked before told so; [SETTLE_ROUNDS] rounds
/// at most, a pair [SIDE_BY_SIDE] times at most, within the cap. A contradiction with a
/// person's note stays the person's, and a recheck's with a confirmed one the inbox's.
/// What still stands is counted `open`.
pub(super) async fn settle(
    service: &MemoryService,
    run: &mut Run,
    at: &RecheckAt<'_>,
    cap: usize,
    sides: &mut HashMap<(String, String), usize>,
    asking: &mut Asking<'_>,
) {
    let caps = recheck::caps(at.workers.looks());
    for round in 0..=SETTLE_ROUNDS {
        if asking.stop.raised() {
            return;
        }
        // A changed review settles as a recheck does: its inbox tells no contradiction.
        let plan = match at.reach {
            recheck::Reach::Inbox => service.recheck_plan(&run.project, at.sources, caps, at.reach).await,
            _ => service.settle_plan(&run.project, at.sources, caps, &asking.near).await,
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(err) => {
                asking.failures.push(err.message);
                return;
            }
        };
        let standing: Vec<recheck::Batch> = plan.batches.into_iter().filter(|b| !b.contradicts.is_empty()).collect();
        asking.scope.open = standing.iter().map(|b| b.contradicts.len()).sum();
        let room = cap.saturating_sub(asking.scope.questions);
        let due = |b: &recheck::Batch| b.contradicts.iter().any(|pair| sides.get(pair).copied().unwrap_or(0) < SIDE_BY_SIDE);
        let mut batches: Vec<recheck::Batch> = standing.into_iter().filter(due).take(room).collect();
        if batches.is_empty() || round == SETTLE_ROUNDS {
            return;
        }
        for batch in &mut batches {
            for (a, b) in batch.contradicts.clone() {
                let asked = sides.entry((a.clone(), b.clone())).or_default();
                if *asked > 0 {
                    for note in &mut batch.notes {
                        let other = if note.id == a { &b } else if note.id == b { &a } else { continue };
                        note.waits.push(format!(
                            "it was judged beside {other} earlier in this run, and the two are still linked as contradicting: \
                             settle it - keep the side the code bears out and fix or drop the other, or fix both to say the \
                             case each holds for; linking them again leaves it for a person."
                        ));
                    }
                }
                *asked += 1;
            }
        }
        asking.scope.rounds += 1;
        asking.scope.questions += batches.len();
        asking.scope.notes += batches.iter().map(|b| b.notes.len()).sum::<usize>();
        run.scope.recheck = Some(asking.scope.clone());
        recheck_round(service, run, at, batches, asking).await;
    }
}

/// One round of recheck questions, asked of the workers at their concurrency, each answer
/// landing as it comes; a raised stop drops the questions in flight.
pub(super) async fn recheck_round(service: &MemoryService, run: &mut Run, at: &RecheckAt<'_>, batches: Vec<recheck::Batch>, asking: &mut Asking<'_>) {
    let looking = at.workers.looks();
    let language = run.scope.language.clone();
    let known: HashSet<&str> = at.sources.iter().map(String::as_str).collect();
    let exists = |path: &str| known.contains(path);
    let schema = recheck::recheck_schema();
    let asked: Vec<(recheck::Batch, String)> = batches
        .into_iter()
        .map(|batch| {
            let prompt = recheck::recheck_prompt(at.project, &batch, language.as_deref(), looking);
            (batch, prompt)
        })
        .collect();
    let (workers, root, schema, exists) = (at.workers, at.root, &schema, &exists);
    let asker = if looking { Asker::Look(workers) } else { Asker::Fill(workers) };
    let at_once = workers.fills_at_once().max(1);
    let Asking { scope, failures, near, info, started, stop } = asking;
    let (started, stop) = (*started, *stop);
    let mut answers = futures_util::stream::iter(asked)
        .map(|(batch, prompt)| async move {
            let bytes = prompt.len();
            let subject = format!("{} ({} notes)", batch.domain.as_deref().unwrap_or("unfiled"), batch.notes.len());
            let checked = ask_checked(asker, root, prompt, schema, "a valid recheck", |value| {
                recheck::check_recheck(&batch, value, exists, looking)
            });
            let checked = within(question_scope("memory.recheck", subject), checked).await;
            (batch, bytes, checked)
        })
        .buffer_unordered(at_once);
    // A raised stop drops the questions in flight; what landed stays.
    while let Some(Some((batch, bytes, (answer, attempts, meta)))) = stop.until(answers.next()).await {
        run.scope.prompt_bytes += bytes;
        tally(info, attempts, &meta);
        scope.asked += 1;
        match answer {
            Ok(rechecked) => {
                scope.refused += rechecked.dropped.len();
                scope.left += batch.notes.len().saturating_sub(rechecked.verdicts.len());
                near.extend(batch.notes.iter().map(|n| n.id.clone()));
                for link in rechecked.links.iter().filter(|l| l.kind == swiss_memory::model::LinkKind::Contradicts) {
                    near.extend([link.from.clone(), link.to.clone()]);
                }
                let refusals = service.recheck_apply(&run.project, &run.id, &rechecked, scope).await;
                failures.extend(refusals);
            }
            Err(why) => {
                scope.failed += 1;
                let place = batch.domain.clone().unwrap_or_else(|| "unfiled".into());
                failures.push(format!("{place}: {why}"));
            }
        }
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = (**info).clone();
        run.scope.recheck = Some((**scope).clone());
        if let Err(err) = service.sync_save(run).await {
            log::warn("memory recheck progress did not land", Some(json!({ "err": err.message })));
        }
    }
}

/// What a recheck came to, as a summary says it, with the contradictions it lifted, settled
/// and left open; a review run's adds what it confirmed.
pub(super) fn rechecked_words(scope: &RecheckScope, reach: recheck::Reach) -> String {
    let did = match reach {
        recheck::Reach::Inbox => "resolved",
        recheck::Reach::Changed => "reviewed",
        _ => "rechecked",
    };
    // A changed review's settle rounds judge the notes an answer set against a changed one,
    // whose code may not have changed: the count says so.
    let changed = match reach {
        recheck::Reach::Changed if scope.rounds > 0 => " - whose code changed, or an answer set against one -",
        recheck::Reach::Changed => " whose code changed",
        _ => "",
    };
    let mut said = format!(
        "{did} {} of {} note(s){changed} in {} domain(s): {} kept, {} fixed, {} dropped, {} merged, {} link(s) added",
        scope.kept + scope.fixed + scope.dropped + scope.merged + scope.held,
        scope.notes,
        scope.domains,
        scope.kept,
        scope.fixed,
        scope.dropped,
        scope.merged,
        scope.linked
    );
    if reach == recheck::Reach::Inbox {
        said = format!("{said}; {} confirmed, {} contradiction(s) lifted", scope.confirmed, scope.lifted);
    } else if reach == recheck::Reach::Changed {
        said = format!("{said}; {} confirmed", scope.confirmed);
        if scope.lifted + scope.rounds > 0 {
            said = format!("{said}, {} contradiction(s) lifted", scope.lifted);
        }
    } else if scope.lifted + scope.rounds > 0 {
        said = format!("{said}; {} contradiction(s) lifted", scope.lifted);
    }
    if scope.rounds > 0 {
        said = format!("{said} ({} more round(s) for the contradictions the answers raised)", scope.rounds);
    }
    if scope.open > 0 {
        said = format!("{said}; {} contradiction(s) between the model's notes still open", scope.open);
    }
    if scope.held > 0 {
        said = format!("{said}; {} still need review", scope.held);
    }
    if scope.more > 0 {
        let next = if reach.reviews() { "review" } else { "recheck" };
        said = format!("{said}; {} question(s) wait for the next {next}", scope.more);
    }
    said
}

/// The checkout's files a recheck may quote: the survey's sources.
pub(super) async fn checkout_sources(service: &MemoryService, project: &str) -> Result<Vec<String>, String> {
    let survey = service.map_survey(project, &Draw::default()).await.map_err(|err| err.message)?;
    Ok(survey.sources.into_iter().map(|(path, _)| path).collect())
}

/// Drive one recheck run to its end (SPEC §memory.init The recheck): the unfiled notes filed
/// by their anchors, then the notes a model wrote and nobody judged since, judged against
/// the code they name by the sync agent. A review run judges the review inbox's notes a
/// model wrote instead, confirms what the code bears out and settles the contradictions
/// (`review --resolve`). A dry run counts the questions and asks none.
pub async fn execute_recheck(service: &MemoryService, mut run: Run, ask: &dyn Ask, stop: &Stop) -> Executed {
    let started = now_ms();
    let reach = match run.trigger {
        Trigger::Review if run.scope.changed => recheck::Reach::Changed,
        Trigger::Review => recheck::Reach::Inbox,
        _ => recheck::Reach::Unchecked,
    };
    let mut info = RunnerInfo { runner: ask.label(), model: ask.model(), ..RunnerInfo::default() };
    let views = match service.projects().await {
        Ok(views) => views,
        Err(err) => return fail(run, err.message),
    };
    let Some(view) = views.iter().find(|v| v.id == run.project) else {
        let error = format!("no project {} to recheck", run.project);
        return fail(run, error);
    };
    let (name, root) = match preview::root_of(view) {
        Ok(root) => (view.name.clone(), root),
        Err(err) => return fail(run, err.message),
    };
    let sources = match checkout_sources(service, &run.project).await {
        Ok(sources) => sources,
        Err(error) => return fail(run, error),
    };
    let cap = run.scope.max_asks.unwrap_or(recheck::RECHECK_ASKS_DEFAULT).max(1);
    if run.dry_run {
        let plan = match service.recheck_plan(&run.project, &sources, recheck::caps(ask.looks()), reach).await {
            Ok(plan) => plan,
            Err(err) => return fail(run, err.message),
        };
        let questions = plan.batches.len().min(cap);
        let scope = RecheckScope {
            domains: plan.domains,
            notes: plan.notes,
            questions,
            more: plan.batches.len() - questions,
            ..RecheckScope::default()
        };
        let would = match reach {
            recheck::Reach::Inbox => "would resolve",
            recheck::Reach::Changed => "would review",
            _ => "would recheck",
        };
        let changed = if reach == recheck::Reach::Changed { " whose code changed" } else { "" };
        run.summary = format!(
            "{would} {} note(s){changed} in {} domain(s): {} question(s), {} for a later run",
            scope.notes, scope.domains, scope.questions, scope.more
        );
        run.scope.recheck = Some(scope);
        run.state = RunState::Ok;
        run.finished_ms = Some(now_ms());
        return Executed { run, advance: None };
    }
    if let Err(err) = service.autofile(&Target::project(&run.project)).await {
        log::warn("memory recheck could not file the unfiled notes", Some(json!({ "err": err.message })));
    }
    let at = RecheckAt { workers: ask, root: &root, project: &name, sources: &sources, reach };
    let (scope, failures, stopped) = recheck_phase(service, &mut run, at, cap, &mut info, started, stop).await;
    info.duration_ms = Some((now_ms() - started).max(0) as u64);
    run.runner = info;
    let said = rechecked_words(&scope, reach);
    if stopped {
        return cancelled(run, format!("cancelled while it {said}"));
    }
    run.scope.phase = None;
    if scope.asked > 0 && scope.failed == scope.asked {
        let first = failures.first().cloned().unwrap_or_default();
        return fail(run, format!("the runner answered none of the {} recheck question(s): {first}", scope.asked));
    }
    let mut summary = match (scope.notes, reach) {
        (0, recheck::Reach::Inbox) => "the review inbox holds no note waiting for review".to_string(),
        (0, recheck::Reach::Changed) => {
            "no note a model wrote names a file that changed since it was last checked: nothing to review".to_string()
        }
        (0, recheck::Reach::Unchecked) => {
            "no note waits for a recheck: a person wrote, confirmed or a recheck judged every one".to_string()
        }
        _ => said,
    };
    if scope.failed > 0 {
        summary = format!("{summary}; {} question(s) failed ({})", scope.failed, failures.first().cloned().unwrap_or_default());
    }
    run.summary = swiss_memory::view::clip(&summary, engine::SUMMARY_CHARS);
    run.state = if scope.notes == 0 { RunState::Empty } else { RunState::Ok };
    run.finished_ms = Some(now_ms());
    Executed { run, advance: None }
}
