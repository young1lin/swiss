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

//! A map run (SPEC §memory.map): the checkout surveyed, its top level reviewed, each area
//! described, the domains landed; a full run draws its map the same way.

use super::*;

/// A map or full run's map half, up to its proposal.
pub(super) struct Mapped {
    pub(super) survey: Survey,
    pub(super) proposal: engine::Proposal,
    pub(super) root: PathBuf,
    pub(super) roots: Vec<String>,
    /// The redraw's domains: set aside by the survey, removed just before the map lands.
    pub(super) redraw: Vec<String>,
}

/// The map half of a map or full run (SPEC §memory.map): the domains a redraw removes set
/// aside, the survey of the checkout as it is now, the model's review of its top level -
/// the areas regrouped into business domains - then one question per area not on the map
/// yet - children before parents, a level at a time, checked against the code index - and
/// the proposal. Without a runner the skeleton alone is drawn: every hub then holds only
/// what the index shows. A lead, when the block names one, answers the review in the
/// runner's place. Err fails the run.
pub(super) async fn map_phase(
    service: &MemoryService,
    run: &mut Run,
    ask: Option<&dyn Ask>,
    lead: Option<&dyn Ask>,
    info: &mut RunnerInfo,
    started: i64,
    stop: &Stop,
) -> Result<Mapped, Halt> {
    let views = service.projects().await.map_err(|err| err.message)?;
    let view = views.iter().find(|v| v.id == run.project).ok_or_else(|| format!("no project {} to map", run.project))?;
    let root = preview::root_of(view).map_err(|err| err.message)?;
    let roots: Vec<String> = view.roots.iter().map(|r| r.path.clone()).collect();
    let redraw = if run.scope.redraw { service.map_redraw_plan(&run.project).await.map_err(|err| err.message)? } else { Vec::new() };
    run.scope.removed = redraw.clone();
    let mut draw = Draw { regroup: None, ignore: redraw.clone() };
    let mut survey = service.map_survey(&run.project, &draw).await.map_err(|err| err.message)?;
    let language = run.scope.language.clone();
    let mut failures: Vec<String> = Vec::new();
    // The review: the rules cut the code by its folders; the model, reading what each area
    // holds, groups them the way the business does. A review that fails keeps the draft.
    let reviewer = lead.map(|lead| Asker::Lead(lead, ask)).or(ask.map(Asker::Fill));
    if let Some((reviewer, prompt)) = reviewer.zip(survey.review_prompt(language.as_deref())) {
        run.scope.prompt_bytes += prompt.len();
        progress(service, run, Phase::Review).await;
        let schema = survey::review_schema();
        let review = ask_checked(reviewer, &root, prompt, &schema, "a valid review", |value| survey.check_review(value));
        let review = within(question_scope("memory.review", view.name.clone()), review);
        let Some((answer, attempts, meta)) = stop.until(review).await else {
            return Err(Halt::Cancelled);
        };
        tally(info, attempts, &meta);
        match answer {
            Ok(regroup) => {
                if !regroup.domains.is_empty() {
                    draw.regroup = Some(regroup.clone());
                    survey = service.map_survey(&run.project, &draw).await.map_err(|err| err.message)?;
                }
                run.scope.regroup = Some(regroup);
            }
            Err(why) => failures.push(format!("the review of the top level: {why}")),
        }
    }
    run.scope.units = survey.units.len();
    run.scope.asks = if ask.is_some() { survey.asks() } else { 0 };
    run.scope.files = survey.code_files;
    run.scope.domains = survey.units.iter().filter(|u| !u.kept).map(|u| u.path.clone()).collect();
    let mut fills: Vec<Option<Fill>> = vec![None; survey.units.len()];
    if let Some(ask) = ask {
        progress(service, run, Phase::Areas).await;
        let schema = survey::fill_schema();
        for level in survey.levels() {
            let questions: Vec<(usize, String)> =
                level.into_iter().map(|at| (at, survey.prompt(at, &fills, language.as_deref()))).collect();
            run.scope.prompt_bytes += questions.iter().map(|(_, p)| p.len()).sum::<usize>();
            let level = futures_util::stream::iter(questions)
                .map(|(at, prompt)| {
                    let (survey, root, schema) = (&survey, &root, &schema);
                    async move { (at, fill_one(ask, root, survey, at, prompt, schema).await) }
                })
                .buffer_unordered(ask.fills_at_once().max(1))
                .collect::<Vec<(usize, Filled)>>();
            let Some(answers) = stop.until(level).await else {
                return Err(Halt::Cancelled);
            };
            for (at, (answer, attempts, meta)) in answers {
                tally(info, attempts, &meta);
                match answer {
                    Ok(fill) => fills[at] = Some(fill),
                    Err(why) => failures.push(format!("{}: {why}", survey.units[at].path)),
                }
            }
            run.scope.filled = fills.iter().filter(|f| f.is_some()).count();
            info.duration_ms = Some((now_ms() - started).max(0) as u64);
            run.runner = info.clone();
            // The panel and `--wait` read the areas so far off the running row.
            if let Err(err) = service.sync_save(run).await {
                log::warn("memory map progress did not land", Some(json!({ "err": err.message })));
            }
        }
        // A runner that described nothing is a broken runner, not a map: nothing is written.
        if run.scope.asks > 0 && run.scope.filled == 0 {
            let first = failures.last().cloned().unwrap_or_default();
            return Err(Halt::Failed(format!("the runner described none of the {} area(s): {first}", run.scope.asks)));
        }
    }
    let proposal = survey.proposal(&fills);
    run.summary = proposal.summary.clone();
    if !failures.is_empty() {
        let shown: Vec<&str> = failures.iter().take(3).map(String::as_str).collect();
        run.summary = format!("{}; {} answer(s) missing, the index's outline kept ({})", run.summary, failures.len(), shown.join("; "));
        run.summary = swiss_memory::view::clip(&run.summary, engine::SUMMARY_CHARS);
    }
    Ok(Mapped { survey, proposal, root, roots, redraw })
}

/// The running row says its phase, for the panel and `sync status` to read.
pub(super) async fn progress(service: &MemoryService, run: &mut Run, phase: Phase) {
    run.scope.phase = Some(phase);
    if let Err(err) = service.sync_save(run).await {
        log::warn("memory sync progress did not land", Some(json!({ "err": err.message })));
    }
}

/// The map lands: the redraw's domains removed, deepest first - their notes move to the
/// parent or go unfiled - then the proposal applied, then the unfiled notes filed by their
/// anchors: those the redraw left, and those saved before a new domain owned their code.
pub(super) async fn land_map(service: &MemoryService, run: &mut Run, mapped: &Mapped) {
    if !mapped.redraw.is_empty() {
        let (removed, failed) = service.map_redraw(&run.project, &mapped.redraw).await;
        run.scope.removed = removed;
        if !failed.is_empty() {
            run.summary = format!("{}; the redraw kept {} domain(s) ({})", run.summary, failed.len(), failed.join("; "));
            run.summary = swiss_memory::view::clip(&run.summary, engine::SUMMARY_CHARS);
        }
    }
    run.ops = engine::apply(service, &run.project, &mapped.proposal).await;
    let drew = run.ops.iter().any(|o| o.op == "domain.create" && o.outcome == engine::Outcome::Applied);
    if !mapped.redraw.is_empty() || drew {
        if let Err(err) = service.autofile(&Target::project(&run.project)).await {
            log::warn("memory map could not file the unfiled notes", Some(json!({ "err": err.message })));
        }
    }
}

pub(super) async fn head_of(root: &Path) -> Option<String> {
    match git(root, &["rev-parse", "HEAD"], 64).await {
        Ok(out) => Some(out.text().trim().to_string()).filter(|h| !h.is_empty()),
        Err(_) => None,
    }
}

/// A project that never synced starts its sync at its map: HEAD and the conversations as
/// they are now are the baseline, so the first sync reads only what comes after.
pub(super) async fn baseline(service: &MemoryService, project: &str, config: &SyncConfig, head: Option<String>, roots: &[String]) -> Option<Advance> {
    let synced = match service.sync_state(project).await {
        Ok(state) => state.head.is_some(),
        Err(_) => true,
    };
    match head {
        Some(head) if !synced => {
            let sources = if config.transcripts { transcript::sources(&config.transcript_dir_list(), roots) } else { Vec::new() };
            let cursors = transcript::baseline(&sources);
            let present = if config.transcripts { Some(sources.iter().map(|s| s.key.clone()).collect()) } else { None };
            Some(Advance { head, commits: Vec::new(), cursors, present })
        }
        _ => None,
    }
}

/// Drive one map run to its end ([map_phase], then the domains created parents first). A
/// project that never synced starts its sync here ([baseline]).
pub async fn execute_map(
    service: &MemoryService,
    mut run: Run,
    config: &SyncConfig,
    ask: Option<&dyn Ask>,
    lead: Option<&dyn Ask>,
    stop: &Stop,
) -> Executed {
    let started = now_ms();
    let mut info = ask.map(|a| RunnerInfo { runner: a.label(), model: a.model(), ..RunnerInfo::default() }).unwrap_or_default();
    info.lead = lead.map(|l| l.label());
    let mapped = match map_phase(service, &mut run, ask, lead, &mut info, started, stop).await {
        Ok(mapped) => mapped,
        Err(Halt::Failed(error)) => return fail(run, error),
        Err(Halt::Cancelled) => {
            run.runner = info;
            return cancelled(run, MAP_CANCELLED.into());
        }
    };
    run.scope.phase = None;
    if ask.is_some() || lead.is_some() {
        info.duration_ms = Some((now_ms() - started).max(0) as u64);
        run.runner = info;
    }
    let head = head_of(&mapped.root).await;
    run.head = head.clone();
    if run.dry_run {
        run.state = RunState::Ok;
        run.proposal = Some(mapped.proposal);
        run.finished_ms = Some(now_ms());
        return Executed { run, advance: None };
    }
    land_map(service, &mut run, &mapped).await;
    run.state = RunState::Ok;
    run.finished_ms = Some(now_ms());
    let advance = baseline(service, &run.project, config, head, &mapped.roots).await;
    Executed { run, advance }
}
