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

//! A full run (SPEC §memory.init): the map, then the code and docs read in slices into
//! notes, and the lead's wrap-up of each domain the run wrote in.

use super::*;

/// Drive one full run to its end (SPEC §memory.init): the map as a map run draws it, then
/// the project's code and docs read in slices against the map it left - one question per
/// slice an earlier full run did not answer while unchanged, at most `maxAsks` - each
/// answer's notes saved as it comes: checked against the code, held for review when a
/// quote is not in the file, skipped when the project has it. With a lead, the lead then
/// wraps up each domain the run saved notes in ([wrap_up]). A dry run carries the map's
/// proposal and the slices' plan; it asks only the map's questions.
pub async fn execute_full(
    service: &MemoryService,
    mut run: Run,
    config: &SyncConfig,
    ask: &dyn Ask,
    lead: Option<&dyn Ask>,
    stop: &Stop,
) -> Executed {
    let started = now_ms();
    let mut info = RunnerInfo { runner: ask.label(), model: ask.model(), lead: lead.map(|l| l.label()), ..RunnerInfo::default() };
    let mapped = match map_phase(service, &mut run, Some(ask), lead, &mut info, started, stop).await {
        Ok(mapped) => mapped,
        Err(Halt::Failed(error)) => return fail(run, error),
        Err(Halt::Cancelled) => {
            run.runner = info;
            return cancelled(run, MAP_CANCELLED.into());
        }
    };
    let project = run.project.clone();
    let head = head_of(&mapped.root).await;
    run.head = head.clone();
    let map_summary = run.summary.clone();
    let cap = run.scope.max_asks.unwrap_or(init::ASKS_DEFAULT).max(1);
    let answered = match service.knowledge_answered(&project).await {
        Ok(answered) => answered,
        Err(err) => return fail(run, err.message),
    };
    // The slices: against the map the proposal would leave (dry), or the one it left.
    let domains = if run.dry_run {
        match service.map_domains(&project).await {
            Ok(current) => init::drawn_domains(&current, &mapped.redraw, &mapped.survey),
            Err(err) => return fail(run, err.message),
        }
    } else {
        land_map(service, &mut run, &mapped).await;
        match service.map_domains(&project).await {
            Ok(domains) => domains,
            Err(err) => return fail(run, err.message),
        }
    };
    let sliced = match service.knowledge_slices(&project, mapped.survey.sources.clone(), Some(domains.clone())).await {
        Ok(sliced) => sliced,
        Err(err) => return fail(run, err.message),
    };
    let (mut knowledge, todo) = init::knowledge_plan(&sliced, &answered, cap);
    if run.dry_run {
        run.summary = swiss_memory::view::clip(
            &format!(
                "{map_summary}; would read {} file(s) in {} slice(s): ask {}, {} unchanged, {} for a later run",
                knowledge.files,
                knowledge.slices,
                todo.len(),
                knowledge.unchanged,
                knowledge.more
            ),
            engine::SUMMARY_CHARS,
        );
        knowledge.done.clear();
        run.scope.knowledge = Some(knowledge);
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = info;
        run.state = RunState::Ok;
        run.proposal = Some(mapped.proposal);
        run.finished_ms = Some(now_ms());
        return Executed { run, advance: None };
    }
    run.scope.knowledge = Some(knowledge.clone());
    progress(service, &mut run, Phase::Knowledge).await;
    let asking = todo.len();
    let schema = init::knowledge_schema();
    let language = run.scope.language.clone();
    let mut errors: Vec<String> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    // The notes each question lists, read before any goes out: the store has one
    // connection, and a question in flight must never hold it while a save waits.
    let mut known: HashMap<Option<String>, Vec<String>> = HashMap::new();
    for &at in &todo {
        let domain = sliced.slices[at].domain.clone();
        if let std::collections::hash_map::Entry::Vacant(slot) = known.entry(domain) {
            match service.knowledge_known(&project, slot.key().as_deref()).await {
                Ok(titles) => slot.insert(titles),
                Err(err) => return fail(run, err.message),
            };
        }
    }
    let known = std::sync::Mutex::new(known);
    // The notes this run saved, by the domain they were filed in: what a lead wraps up.
    let mut written: BTreeMap<String, Vec<init::Drafted>> = BTreeMap::new();
    {
        let (project, name, domains, root, schema, language) =
            (&project, &mapped.survey.project, &domains, &mapped.root, &schema, language.as_deref());
        let (slices, known) = (&sliced.slices, &known);
        let mut answers = futures_util::stream::iter(todo)
            .map(|at| {
                let slice = &slices[at];
                async move {
                    // Built when its turn comes, so it lists the notes the slices before it saved.
                    let titles = known.lock().ok().and_then(|k| k.get(&slice.domain).cloned()).unwrap_or_default();
                    let domain = slice.domain.as_deref().and_then(|path| domains.iter().find(|d| d.path == path));
                    let prompt = init::knowledge_prompt(name, slice, domain, &titles, language);
                    let bytes = prompt.len();
                    let subject = slice_subject(slice);
                    let checked = ask_checked(Asker::Fill(ask), root, prompt, schema, "a valid notes list", |value| {
                        init::check_notes(slice, value)
                    });
                    let checked = within(question_scope("memory.notes", subject), checked).await;
                    (at, bytes, checked)
                }
            })
            .buffer_unordered(ask.fills_at_once().max(1));
        // A raised stop drops the questions in flight; the answers already in are saved.
        while let Some(Some((at, bytes, (answer, attempts, meta)))) = stop.until(answers.next()).await {
            let slice = &slices[at];
            run.scope.prompt_bytes += bytes;
            tally(&mut info, attempts, &meta);
            knowledge.asked += 1;
            match answer {
                Ok((notes, dropped)) => {
                    knowledge.proposed += notes.len() + dropped.len();
                    knowledge.refused += dropped.len();
                    refusals.extend(dropped);
                    // The saves go one at a time: each sees the notes saved before it.
                    for note in notes {
                        let drafted = note.clone();
                        match service.knowledge_save(project, &run.id, slice.domain.as_deref(), note).await {
                            Saved::New { held } => {
                                knowledge.saved += 1;
                                knowledge.held += usize::from(held);
                                if let Ok(mut known) = known.lock() {
                                    known.entry(slice.domain.clone()).or_default().push(drafted.title.clone());
                                }
                                if let Some(domain) = &slice.domain {
                                    written.entry(domain.clone()).or_default().push(drafted);
                                }
                            }
                            Saved::Duplicate => knowledge.duplicates += 1,
                            Saved::Refused(why) => {
                                knowledge.refused += 1;
                                refusals.push(why);
                            }
                        }
                    }
                    knowledge.done.push(slice.hash.clone());
                }
                Err(why) => {
                    knowledge.failed += 1;
                    errors.push(format!("{}: {why}", slice.files().first().copied().unwrap_or_default()));
                }
            }
            info.duration_ms = Some((now_ms() - started).max(0) as u64);
            run.runner = info.clone();
            run.scope.knowledge = Some(knowledge.clone());
            // The panel and `--wait` read the slices so far off the running row.
            if let Err(err) = service.sync_save(&run).await {
                log::warn("memory init progress did not land", Some(json!({ "err": err.message })));
            }
        }
    }
    // The scope says what THIS project's notes land in: a project whose settings turn
    // embedding off (§memory.embedding) lands none, whatever the index at large says.
    knowledge.embedding = match service.embedding_on(&run.project).await {
        Ok(false) => Some(EmbeddingScope { state: "off".into(), ..Default::default() }),
        _ => service.embedding_status().await.ok().map(|status| EmbeddingScope::from(&status)),
    };
    run.scope.knowledge = Some(knowledge.clone());
    if stop.raised() && knowledge.asked < asking {
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = info;
        let summary = format!(
            "{map_summary}; cancelled after {} of {asking} slice(s): {} note(s) saved ({} held for review, {} known); \
             a later full run skips the slices answered",
            knowledge.asked, knowledge.saved, knowledge.held, knowledge.duplicates
        );
        return cancelled(run, summary);
    }
    // A runner that answered no slice is a broken runner: the map stands, the run failed.
    if knowledge.asked > 0 && knowledge.failed == knowledge.asked {
        run.scope.phase = None;
        let first = errors.first().cloned().unwrap_or_default();
        let error = format!("the map landed, but the runner answered none of the {} slice(s): {first}", knowledge.asked);
        return fail(run, error);
    }
    let mut summary = format!(
        "{map_summary}; {} note(s) saved ({} held for review, {} known) from {} of {} slice(s)",
        knowledge.saved, knowledge.held, knowledge.duplicates, knowledge.asked, knowledge.slices
    );
    if let Some(lead) = lead.filter(|_| !written.is_empty()) {
        let at = WrapAt { lead, workers: ask, root: &mapped.root, project: &mapped.survey.project, sliced: &sliced };
        let (wrapup, failures, stopped) = wrap_up(service, &mut run, at, written, &mut info, started, stop).await;
        let mut said = format!(
            "the lead wrapped up {} of {} domain(s): {} hub(s) refreshed, {} note(s) saved",
            wrapup.wrapped, wrapup.domains, wrapup.hubs, wrapup.saved
        );
        if wrapup.failed > 0 {
            said = format!("{said}, {} failed ({})", wrapup.failed, failures.first().cloned().unwrap_or_default());
        }
        if stopped {
            info.duration_ms = Some((now_ms() - started).max(0) as u64);
            run.runner = info;
            return cancelled(run, format!("{summary}; cancelled while {said}"));
        }
        summary = format!("{summary}; {said}");
    }
    // The closing recheck: the notes nobody judged since a model wrote them, this run's too.
    let sources: Vec<String> = mapped.survey.sources.iter().map(|(path, _)| path.clone()).collect();
    let at = RecheckAt {
        workers: ask,
        root: &mapped.root,
        project: &mapped.survey.project,
        sources: &sources,
        reach: recheck::Reach::Unchecked,
    };
    let (rechecked, failures, stopped) =
        recheck_phase(service, &mut run, at, recheck::RECHECK_ASKS_DEFAULT, &mut info, started, stop).await;
    if rechecked.questions > 0 {
        let said = rechecked_words(&rechecked, recheck::Reach::Unchecked);
        if stopped {
            info.duration_ms = Some((now_ms() - started).max(0) as u64);
            run.runner = info;
            return cancelled(run, format!("{summary}; cancelled while it {said}"));
        }
        summary = format!("{summary}; {said}");
        if rechecked.failed > 0 {
            let first = failures.first().cloned().unwrap_or_default();
            summary = format!("{summary}; {} recheck question(s) failed ({first})", rechecked.failed);
        }
    } else if let Some(first) = failures.first() {
        summary = format!("{summary}; the recheck did not start ({first})");
    }
    run.scope.phase = None;
    if knowledge.more > 0 {
        summary = format!("{summary}; {} slice(s) wait for the next full run", knowledge.more);
    }
    if knowledge.failed > 0 {
        summary = format!("{summary}; {} slice(s) failed ({})", knowledge.failed, errors.first().cloned().unwrap_or_default());
    }
    if knowledge.refused > 0 {
        summary = format!("{summary}; {} note(s) refused ({})", knowledge.refused, refusals.first().cloned().unwrap_or_default());
    }
    info.duration_ms = Some((now_ms() - started).max(0) as u64);
    run.runner = info;
    run.summary = swiss_memory::view::clip(&summary, engine::SUMMARY_CHARS);
    run.state = if knowledge.slices == 0 && mapped.proposal.domains.is_empty() { RunState::Empty } else { RunState::Ok };
    run.finished_ms = Some(now_ms());
    let advance = baseline(service, &project, config, head, &mapped.roots).await;
    Executed { run, advance }
}

/// What a full run's wrap-up stands on: the lead, the workers it delegates to, the checkout,
/// the project's name and the run's slices (the files each domain owns).
pub(super) struct WrapAt<'a> {
    lead: &'a dyn Ask,
    workers: &'a dyn Ask,
    root: &'a Path,
    project: &'a str,
    sliced: &'a init::Sliced,
}

/// The lead's wrap-up of a full run (SPEC §memory.init Lead): one question per domain the
/// run saved notes in - the hub as it stands, those notes, the files the domain owns - at
/// the lead's own concurrency. Each answer lands as it comes: the summary and keywords on
/// the hub at the revision read (a hub edited meanwhile is left as it is), the notes saved
/// like a slice's, held to the domain's files. A question that fails leaves the domain as
/// the slices left it. Out: the counts, the failures, and whether the stop cut it short.
pub(super) async fn wrap_up(
    service: &MemoryService,
    run: &mut Run,
    at: WrapAt<'_>,
    written: BTreeMap<String, Vec<init::Drafted>>,
    info: &mut RunnerInfo,
    started: i64,
    stop: &Stop,
) -> (WrapupScope, Vec<String>, bool) {
    let mut scope = WrapupScope { domains: written.len(), ..WrapupScope::default() };
    let mut failures: Vec<String> = Vec::new();
    run.scope.wrapup = Some(scope.clone());
    progress(service, run, Phase::Wrapup).await;
    // The hubs as the map left them, read before any question goes out: the store has one
    // connection, and a question in flight must never hold it while a save waits.
    let hubs = match service.map_domains(&run.project).await {
        Ok(hubs) => hubs,
        Err(err) => {
            scope.failed = scope.domains;
            failures.push(err.message);
            run.scope.wrapup = Some(scope.clone());
            return (scope, failures, false);
        }
    };
    let mut files_of: HashMap<&str, BTreeSet<String>> = HashMap::new();
    for slice in &at.sliced.slices {
        if let Some(domain) = &slice.domain {
            files_of.entry(domain.as_str()).or_default().extend(slice.files().into_iter().map(str::to_string));
        }
    }
    let language = run.scope.language.clone();
    let mut questions: Vec<(swiss_memory::model::Domain, init::Slice, String)> = Vec::new();
    for (path, notes) in &written {
        let Some(hub) = hubs.iter().find(|d| d.path == *path) else {
            scope.failed += 1;
            failures.push(format!("{path}: the map has no such domain now"));
            continue;
        };
        let files: Vec<String> = files_of.get(path.as_str()).map(|f| f.iter().cloned().collect()).unwrap_or_default();
        let listed: Vec<&str> = files.iter().map(String::as_str).collect();
        let prompt = init::wrapup_prompt(at.project, hub, notes, &listed, language.as_deref());
        questions.push((hub.clone(), init::Slice::of_files(Some(path.clone()), files), prompt));
    }
    let schema = init::wrapup_schema();
    let (lead, workers, root, schema) = (at.lead, at.workers, at.root, &schema);
    let mut answers = futures_util::stream::iter(questions)
        .map(|(hub, files, prompt)| async move {
            let bytes = prompt.len();
            let asker = Asker::Lead(lead, Some(workers));
            let checked =
                ask_checked(asker, root, prompt, schema, "a valid wrap-up", |value| init::check_wrapup(&files, &hub, value));
            let checked = within(question_scope("memory.wrapup", hub.path.clone()), checked).await;
            (hub, bytes, checked)
        })
        .buffer_unordered(lead.fills_at_once().max(1));
    // A raised stop drops the questions in flight; what landed stays.
    while let Some(Some((hub, bytes, (answer, attempts, meta)))) = stop.until(answers.next()).await {
        run.scope.prompt_bytes += bytes;
        tally(info, attempts, &meta);
        match answer {
            Ok(wrapped) => {
                scope.wrapped += 1;
                if wrapped.summary.is_some() || wrapped.keywords.is_some() {
                    let op = engine::DomainOp {
                        path: hub.path.clone(),
                        expected_revision: Some(hub.revision),
                        summary: wrapped.summary,
                        keywords: wrapped.keywords,
                        ..engine::DomainOp::default()
                    };
                    let proposal = engine::Proposal { domains: vec![op], ..engine::Proposal::default() };
                    let landed = engine::apply(service, &run.project, &proposal).await;
                    scope.hubs += landed.iter().filter(|o| o.outcome == engine::Outcome::Applied).count();
                    run.ops.extend(landed);
                }
                for note in wrapped.notes {
                    match service.knowledge_save(&run.project, &run.id, Some(&hub.path), note).await {
                        Saved::New { held } => {
                            scope.saved += 1;
                            scope.held += usize::from(held);
                        }
                        Saved::Duplicate | Saved::Refused(_) => {}
                    }
                }
            }
            Err(why) => {
                scope.failed += 1;
                failures.push(format!("{}: {why}", hub.path));
            }
        }
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = info.clone();
        run.scope.wrapup = Some(scope.clone());
        if let Err(err) = service.sync_save(run).await {
            log::warn("memory wrap-up progress did not land", Some(json!({ "err": err.message })));
        }
    }
    let stopped = stop.raised() && scope.wrapped + scope.failed < scope.domains;
    (scope, failures, stopped)
}
