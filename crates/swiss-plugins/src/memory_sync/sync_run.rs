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

//! One sync run (SPEC §memory.sync): the material gathered, the proposal asked and
//! checked, applied or kept dry, and the stop any run can be told.

use super::*;
use swiss_memory::model::{Domain, Memory};

/// A run's outcome: the finished [Run] and what an applied run moves.
pub struct Executed {
    pub run: Run,
    pub advance: Option<Advance>,
}

pub(super) fn fail(mut run: Run, error: String) -> Executed {
    run.state = RunState::Failed;
    run.error = Some(error);
    run.finished_ms = Some(now_ms());
    Executed { run, advance: None }
}

/// A run stopped on request: what it saved stays, `summary` says what that is.
pub(super) fn cancelled(mut run: Run, summary: String) -> Executed {
    run.state = RunState::Cancelled;
    run.summary = swiss_memory::view::clip(&summary, engine::SUMMARY_CHARS);
    run.finished_ms = Some(now_ms());
    Executed { run, advance: None }
}

/// A running run's stop (SPEC §memory.sync Cancel): the cancel route raises it, and the
/// run sees it between its questions - the ones in flight are dropped (a spawned runner
/// killed with them), what was saved stays, and the run ends cancelled.
#[derive(Clone, Default)]
pub struct Stop(std::sync::Arc<Raised>);

#[derive(Default)]
pub(super) struct Raised {
    raised: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}

impl Stop {
    pub fn raise(&self) {
        self.0.raised.store(true, std::sync::atomic::Ordering::SeqCst);
        self.0.wake.notify_waiters();
    }

    pub fn raised(&self) -> bool {
        self.0.raised.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `work` to its end, or None once the stop is raised - the work dropped where it stood.
    pub async fn until<T>(&self, work: impl std::future::Future<Output = T>) -> Option<T> {
        let wake = self.0.wake.notified();
        tokio::pin!(wake);
        // Registered before the flag is read, so a raise in between still wakes it.
        wake.as_mut().enable();
        if self.raised() {
            return None;
        }
        tokio::select! {
            biased;
            _ = wake => None,
            out = work => Some(out),
        }
    }
}

/// Why a map half did not reach its proposal.
pub(super) enum Halt {
    Failed(String),
    Cancelled,
}

impl From<String> for Halt {
    fn from(why: String) -> Halt {
        Halt::Failed(why)
    }
}

/// What a run cancelled in its map half says: nothing landed.
pub(super) const MAP_CANCELLED: &str = "cancelled while the map was drawn; nothing landed";

/// What the patch's closing line may add to the room it is shaped into.
const PATCH_SLACK: usize = 256;

/// The walk and its prompt, sized to `budget`. Over it the patch gives way first: it is
/// shaped again into the room the rest of the prompt leaves, so every commit still goes
/// and the agent reads what was cut in the checkout. Only a prompt still over is walked
/// again for half as many commits, down to one, which is sent whatever its size; the rest
/// wait for the next run (the walk's head is then the last commit sent).
pub(super) async fn fit(
    root: &Path,
    state: &engine::SyncState,
    since: Option<&str>,
    mut walked: Walk,
    budget: usize,
    prepare: impl Fn(&Walk, usize) -> engine::Prepared,
) -> MemResult<(Walk, engine::Prepared)> {
    loop {
        let mut prepared = prepare(&walked, engine::PATCH_BYTES);
        if prepared.prompt.len() > budget {
            let rest = prepared.prompt.len() - prepared.scope.patch_bytes;
            prepared = prepare(&walked, budget.saturating_sub(rest + PATCH_SLACK));
        }
        if prepared.prompt.len() <= budget || walked.commits.len() <= 1 {
            return Ok((walked, prepared));
        }
        walked = walk(root, state, since, walked.commits.len() / 2).await?;
    }
}

/// The prompt for what one walk holds: its patch kept to the walk's files and to
/// `patch_max` bytes, a credential-named file's section dropped.
pub(super) fn prepare_walk(
    project: &str,
    domains: &[Domain],
    memories: &[Memory],
    walked: &Walk,
    digest: &Digest,
    patch_max: usize,
) -> engine::Prepared {
    let kept: HashSet<String> = walked.changes.iter().map(|c| c.path.clone()).collect();
    let patch = engine::shape_patch(
        &walked.patch_raw,
        |path: &str| {
            if preview::sensitive(path) {
                Keep::Dropped
            } else if kept.contains(path) {
                Keep::Yes
            } else {
                Keep::No
            }
        },
        engine::PATCH_FILE_BYTES,
        patch_max,
    );
    engine::prepare(&Material {
        project,
        domains,
        memories,
        commits: &walked.commits,
        more_commits: walked.more_commits,
        changes: &walked.changes,
        patch: &patch,
        digest,
    })
}

/// Drive one already-running run to its end: gather, ask, apply, and land the run
/// row (the caller sends it to [MemoryService::sync_finish]); every rule of the
/// pure half applies through the engine.
pub async fn execute_run(
    service: &MemoryService,
    mut run: Run,
    config: &SyncConfig,
    ask: &dyn Ask,
    stop: &Stop,
) -> Executed {
    let started = now_ms();
    // The project's checkout and roots.
    let views = match service.projects().await {
        Ok(views) => views,
        Err(err) => return fail(run, err.message),
    };
    let Some(view) = views.iter().find(|v| v.id == run.project) else {
        let error = format!("no project {} to sync", run.project);
        return fail(run, error);
    };
    let root = match preview::root_of(view) {
        Ok(root) => root,
        Err(err) => return fail(run, err.message),
    };
    let roots: Vec<String> = view.roots.iter().map(|r| r.path.clone()).collect();
    let state = match service.sync_state(&run.project).await {
        Ok(state) => state,
        Err(err) => return fail(run, err.message),
    };
    // The git walk.
    let walked = match walk(&root, &state, run.since.as_deref(), engine::COMMITS_MAX).await {
        Ok(walked) => walked,
        Err(err) => return fail(run, err.message),
    };
    run.base = walked.base.clone();
    // The conversations.
    let dirs = if config.transcripts { config.transcript_dir_list() } else { Vec::new() };
    let sources = transcript::sources(&dirs, &roots);
    let digest = if config.transcripts {
        transcript::digest(&sources, &roots, &state.cursors, transcript::DIGEST_BYTES)
    } else {
        Digest::default()
    };
    // A first sync records the baseline: HEAD and every transcript at its size. A dry
    // run records nothing: the baseline belongs to the first real run, and a dry run
    // leaving one would steal it - the run row itself is the audit a dry run keeps.
    if walked.baseline {
        run.state = RunState::Empty;
        run.summary = if run.dry_run {
            "the first sync would record the baseline; a dry run records nothing".into()
        } else {
            "the first sync recorded the baseline".into()
        };
        run.finished_ms = Some(now_ms());
        run.head = Some(walked.head.clone());
        if run.dry_run {
            return Executed { run, advance: None };
        }
        let cursors = if config.transcripts { transcript::baseline(&sources) } else { Vec::new() };
        let present = if config.transcripts { Some(sources.iter().map(|s| s.key.clone()).collect()) } else { None };
        let advance = Advance { head: walked.head.clone(), commits: Vec::new(), cursors, present };
        return Executed { run, advance: Some(advance) };
    }
    // The material, the prompt, the scope summary: as many commits as fit the budget.
    let (domains, memories) = match service.sync_material(&run.project).await {
        Ok(material) => material,
        Err(err) => return fail(run, err.message),
    };
    let prepare = |walked: &Walk, patch_max: usize| prepare_walk(&view.name, &domains, &memories, walked, &digest, patch_max);
    let budget = engine::prompt_budget(ask.context_bytes());
    let (walked, prepared) = match fit(&root, &state, run.since.as_deref(), walked, budget, prepare).await {
        Ok(fitted) => fitted,
        Err(err) => return fail(run, err.message),
    };
    let commits: Vec<engine::CommitLine> = walked
        .commits
        .iter()
        .map(|c| engine::CommitLine { hash: c.hash.clone(), subject: c.subject.clone() })
        .collect();
    run.scope = prepared.scope.clone();
    // An empty scope starts no runner: the run is empty and the state moves. A dry run
    // moves nothing here either - the advance would still prune cursors whose files
    // disappeared, and even a no-op head write is a business write a dry run must not
    // make.
    if commits.is_empty() && digest.text.is_empty() {
        run.state = RunState::Empty;
        run.summary = if run.dry_run {
            "nothing new since the last sync; a dry run moves nothing".into()
        } else {
            "nothing new since the last sync".into()
        };
        run.finished_ms = Some(now_ms());
        run.head = Some(walked.head.clone());
        if run.dry_run {
            return Executed { run, advance: None };
        }
        let present = if config.transcripts { Some(sources.iter().map(|s| s.key.clone()).collect()) } else { None };
        let advance = Advance { head: walked.head.clone(), commits: Vec::new(), cursors: digest.cursors.clone(), present };
        return Executed { run, advance: Some(advance) };
    }
    // The running row says what it sends and whom it asks before the ask, which can run
    // for many minutes (SPEC §memory.sync Progress); the agent's live run carries this
    // run's id as its source.
    run.runner.runner = ask.label();
    run.runner.model = ask.model();
    progress(service, &mut run, Phase::Ask).await;
    // Ask, and on an invalid answer, tell it why and ask once more.
    let mut attempts = 0u32;
    let mut prompt = prepared.prompt.clone();
    let (answer, meta) = loop {
        attempts += 1;
        let asked = within(question_scope("memory.sync", run.project.clone()), ask.ask(&root, &prompt));
        let stdout = match stop.until(asked).await {
            Some(Ok(stdout)) => stdout,
            Some(Err(err)) => return fail(run, err),
            None => return cancelled(run, "cancelled while the runner was asked; nothing applied".into()),
        };
        let (answer, meta) = engine::read_reply(&stdout);
        match answer {
            Answer::Invalid(why) if attempts == 1 => {
                prompt = format!("{prompt}{}", engine::retry_note(&why));
            }
            other => break (other, meta),
        }
    };
    let mut info = RunnerInfo {
        runner: ask.label(),
        model: ask.model(),
        attempts,
        duration_ms: Some((now_ms() - started).max(0) as u64),
        ..RunnerInfo::default()
    };
    info.cost_usd = meta.cost_usd;
    info.turns = meta.turns;
    if info.duration_ms.is_none() {
        info.duration_ms = meta.duration_ms;
    }
    run.runner = info;
    let proposal = match answer {
        Answer::Proposal(proposal) => proposal,
        Answer::Invalid(why) => return fail(run, format!("the runner answered twice without a valid proposal: {why}")),
        Answer::Failed(why) => return fail(run, why),
    };
    run.summary = proposal.summary.clone();
    if run.dry_run {
        run.scope.phase = None;
        run.state = RunState::Ok;
        run.proposal = Some(proposal);
        run.finished_ms = Some(now_ms());
        run.head = Some(walked.head.clone());
        return Executed { run, advance: None };
    }
    progress(service, &mut run, Phase::Apply).await;
    let ops = engine::apply(service, &run.project, &proposal).await;
    run.ops = ops;
    run.scope.phase = None;
    run.state = RunState::Ok;
    run.finished_ms = Some(now_ms());
    run.head = Some(walked.head.clone());
    let present = if config.transcripts { Some(sources.iter().map(|s| s.key.clone()).collect()) } else { None };
    let advance = Advance {
        head: walked.head.clone(),
        commits: commits.iter().map(|c| c.hash.clone()).collect(),
        cursors: digest.cursors.clone(),
        present,
    };
    Executed { run, advance: Some(advance) }
}
