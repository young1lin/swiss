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

//! A docs run (SPEC §memory.docs): the documents the project keeps read, and each new or
//! changed one carded with the code it names.

use super::*;

/// What a docs run's read did, as a summary says it.
pub(super) fn docs_read_words(scope: &DocsScope) -> String {
    let mut said = format!(
        "read {} document(s) from {} source(s): {} added, {} changed, {} unchanged, {} removed",
        scope.files, scope.sources, scope.added, scope.changed, scope.unchanged, scope.removed
    );
    if !scope.missing.is_empty() {
        said = format!("{said}; not in the checkout: {}", scope.missing.join(", "));
    }
    if scope.unread > 0 {
        said = format!("{said}; {} file(s) not kept", scope.unread);
    }
    if scope.relinked > 0 {
        said = format!("{said}; {} card(s) re-linked to the code as it is now", scope.relinked);
    }
    said
}

/// What one card question is about: its first document, and how many more it shows.
pub(super) fn card_subject(slice: &docs::CardSlice) -> String {
    let first = slice.docs.first().map(|d| d.path.clone()).unwrap_or_default();
    match slice.docs.len() {
        0 | 1 => first,
        n => format!("{first} (+{})", n - 1),
    }
}

/// Drive one docs run to its end (SPEC §memory.docs): the sources read - each new or
/// changed document kept whole, each one no source holds deleted - then the code index
/// refreshed and every card's code names looked up again in it, then the documents
/// waiting for a card carded by the workers at their concurrency, at most `maxAsks`
/// questions, each card landing as it comes. `workers` is Err with why there are none:
/// the read and the look-up land alone and the documents wait. A dry run keeps nothing and
/// asks nothing: it counts.
pub async fn execute_docs(service: &MemoryService, mut run: Run, workers: Result<&dyn Ask, String>, stop: &Stop) -> Executed {
    let started = now_ms();
    let mut info = match &workers {
        Ok(ask) => RunnerInfo { runner: ask.label(), model: ask.model(), ..RunnerInfo::default() },
        Err(_) => RunnerInfo::default(),
    };
    let views = match service.projects().await {
        Ok(views) => views,
        Err(err) => return fail(run, err.message),
    };
    let Some(view) = views.iter().find(|v| v.id == run.project) else {
        let error = format!("no project {} to read documents for", run.project);
        return fail(run, error);
    };
    let (name, root) = match preview::root_of(view) {
        Ok(root) => (view.name.clone(), root),
        Err(err) => return fail(run, err.message),
    };
    let report = match service.docs_read(&run.project, run.dry_run).await {
        Ok(report) => report,
        Err(err) => return fail(run, err.message),
    };
    let mut scope = DocsScope::of_read(&report);
    let cap = run.scope.max_asks.unwrap_or(docs::CARD_ASKS_DEFAULT).max(1);
    let done = |mut run: Run, scope: DocsScope, summary: String, state: RunState| {
        run.scope.docs = Some(scope);
        run.scope.phase = None;
        run.summary = swiss_memory::view::clip(&summary, engine::SUMMARY_CHARS);
        run.state = state;
        run.finished_ms = Some(now_ms());
        Executed { run, advance: None }
    };
    if report.sources.is_empty() {
        let (summary, state) = match report.removed {
            0 => ("the project names no docs source: name one with `swiss memory docs add <path>`".to_string(), RunState::Empty),
            n if run.dry_run => (format!("the project names no docs source: {n} document(s) it kept would be removed"), RunState::Ok),
            n => (format!("the project names no docs source: the {n} document(s) it kept were removed"), RunState::Ok),
        };
        return done(run, scope, summary, state);
    }
    let read = docs_read_words(&scope);
    if run.dry_run {
        let questions = docs::pack(&report.waiting).len();
        scope.questions = questions.min(cap);
        scope.more = questions - scope.questions;
        let summary = format!(
            "would {read}; {} document(s) would wait for a card: {} question(s), {} for a later run",
            scope.waiting, scope.questions, scope.more
        );
        return done(run, scope, summary, RunState::Ok);
    }
    let target = Target::project(&run.project);
    if let Err(err) = service.code_index(&target).await {
        log::warn("memory docs could not refresh the code index", Some(json!({ "err": err.message })));
    }
    match service.docs_relink(&run.project).await {
        Ok(relinked) => scope.relinked = relinked,
        Err(err) => log::warn("memory docs could not look the cards' code names up again", Some(json!({ "err": err.message }))),
    }
    let read = docs_read_words(&scope);
    let workers = match workers {
        Ok(ask) => ask,
        Err(why) => {
            let summary = format!("{read}; {why}: {} document(s) wait for a card", scope.waiting);
            return done(run, scope, summary, RunState::Ok);
        }
    };
    let (waiting, domains) = match (service.docs_uncarded(&run.project).await, service.docs_domains(&run.project).await) {
        (Ok(waiting), Ok(domains)) => (waiting, domains),
        (Err(err), _) | (_, Err(err)) => return fail(run, err.message),
    };
    scope.waiting = waiting.len();
    let slices = docs::card_slices(waiting);
    scope.questions = slices.len().min(cap);
    scope.more = slices.len() - scope.questions;
    run.scope.docs = Some(scope.clone());
    if scope.questions == 0 {
        let state = if scope.added + scope.changed + scope.removed + scope.relinked == 0 { RunState::Empty } else { RunState::Ok };
        return done(run, scope, format!("{read}; every document has its card"), state);
    }
    progress(service, &mut run, Phase::Docs).await;
    let language = run.scope.language.clone();
    let schema = docs::card_schema();
    let by = workers.label();
    let asked: Vec<(docs::CardSlice, String)> = slices
        .into_iter()
        .take(scope.questions)
        .map(|slice| {
            let prompt = docs::card_prompt(&name, &slice, &domains, language.as_deref());
            (slice, prompt)
        })
        .collect();
    let (root_at, schema, domains_at) = (&root, &schema, &domains);
    let mut answers = futures_util::stream::iter(asked)
        .map(|(slice, prompt)| async move {
            let bytes = prompt.len();
            let subject = card_subject(&slice);
            let checked = ask_checked(Asker::Fill(workers), root_at, prompt, schema, "valid cards", |value| {
                docs::check_cards(&slice, domains_at, value)
            });
            let checked = within(question_scope("memory.docs", subject), checked).await;
            (slice, bytes, checked)
        })
        .buffer_unordered(workers.fills_at_once().max(1));
    let mut failures: Vec<String> = Vec::new();
    // A raised stop drops the questions in flight; the cards that landed stay.
    while let Some(Some((slice, bytes, (answer, attempts, meta)))) = stop.until(answers.next()).await {
        run.scope.prompt_bytes += bytes;
        tally(&mut info, attempts, &meta);
        scope.asked += 1;
        match answer {
            Ok((cards, refused)) => {
                scope.refused += refused.len();
                for card in cards {
                    let Some(doc) = slice.docs.iter().find(|d| d.path == card.path) else { continue };
                    match service.docs_card_save(&run.project, doc, &card, &by).await {
                        Ok(saved) => {
                            if saved.saved {
                                scope.carded += 1;
                            } else {
                                scope.stale += 1;
                            }
                            scope.symbols += saved.symbols;
                            scope.dropped += saved.dropped;
                        }
                        Err(err) => failures.push(format!("{}: {}", doc.path, err.message)),
                    }
                }
            }
            Err(why) => {
                scope.failed += 1;
                failures.push(format!("{}: {why}", card_subject(&slice)));
            }
        }
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = info.clone();
        run.scope.docs = Some(scope.clone());
        if let Err(err) = service.sync_save(&run).await {
            log::warn("memory docs progress did not land", Some(json!({ "err": err.message })));
        }
    }
    drop(answers);
    info.duration_ms = Some((now_ms() - started).max(0) as u64);
    run.runner = info;
    let mut said = format!(
        "{read}; carded {} of {} document(s), {} code name(s) found in the code index",
        scope.carded, scope.waiting, scope.symbols
    );
    if scope.more > 0 {
        said = format!("{said}; {} question(s) wait for the next docs run", scope.more);
    }
    if stop.raised() && scope.asked < scope.questions {
        run.scope.docs = Some(scope);
        return cancelled(run, format!("cancelled after it {said}"));
    }
    if scope.failed == scope.asked {
        let first = failures.first().cloned().unwrap_or_default();
        run.scope.docs = Some(scope.clone());
        return fail(run, format!("the runner carded none of the {} document question(s): {first}", scope.asked));
    }
    if scope.failed > 0 {
        said = format!("{said}; {} question(s) failed ({})", scope.failed, failures.first().cloned().unwrap_or_default());
    }
    done(run, scope, said, RunState::Ok)
}
