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

//! The queue worker (SPEC §memory.sync The worker): runs one at a time in queue order,
//! each on the workers and the lead its layers choose (SPEC §memory.settings).

use super::*;

/// The queue's driver: at boot the stale runs fail, then runs go one at a time in
/// queue order, the worker idling on a nudge between them (SPEC §memory.sync The
/// worker). It never starts a run itself: [MemoryService::sync_next] owns every
/// rule; this loop only feeds it.
pub async fn sync_worker(
    service: std::sync::Arc<MemoryService>,
    seats: std::sync::Arc<crate::memory_api::MemoryState>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    // A stopped gateway fails what it left mid-flight; this one starts clean.
    if let Err(err) = service.sync_interrupt().await {
        log::warn("memory sync could not clear stale runs", Some(json!({ "err": err.message })));
    }
    loop {
        let next = match service.sync_next(now_ms()).await {
            Ok(next) => next,
            Err(err) => {
                log::warn("memory sync queue read failed", Some(json!({ "err": err.message })));
                tokio::select! {
                    _ = cancel.changed() => return,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                continue;
            }
        };
        match next {
            Next::Run(run) => {
                let id = run.id.clone();
                let stop = seats.sync_stop(&id);
                // Every model run this sync run starts names it as its source.
                let source = RunContext { source: Some(format!("memory:{}", run.id)), ..RunContext::default() };
                let executed = within(source, run_one(&service, &seats, *run, &stop)).await;
                if let Err(err) = service.sync_finish(&executed.run, executed.advance.as_ref()).await {
                    log::warn("memory sync run did not land", Some(json!({ "err": err.message })));
                }
                // Unseated only once the end landed: a cancel that read the run running
                // until then still finds the stop it raises.
                seats.sync_stop_done(&id);
            }
            Next::Wait(due) => {
                // A due in the past sleeps out at once; the nudge and the stop cut in.
                let until = tokio::time::Instant::now() + Duration::from_millis((due - now_ms()).max(0) as u64);
                let nudge = seats.sync_wake();
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = nudge.notified() => {}
                    _ = cancel.changed() => return,
                }
            }
            Next::Idle => {
                let nudge = seats.sync_wake();
                tokio::select! {
                    _ = nudge.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    _ = cancel.changed() => return,
                }
            }
        }
    }
}

/// The workers and the lead a run asks (SPEC §memory.settings), with the layer each was
/// decided at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chosen {
    pub agents: Pick,
    pub lead: Pick,
}

/// Each decided on its own, the first layer that says anything winning: the run's own pick
/// (`agents`, `lead`), the project's settings, the plugin block's. Past all three the
/// workers fall back to the agent added last - but a nudge's run asks only an agent someone
/// named - and the lead to none.
pub fn choose(
    agents: &Choice,
    lead: &Choice,
    nudge: bool,
    project: &ProjectSettings,
    config: Option<&SyncConfig>,
    llm: &LlmState,
) -> Chosen {
    let (block_agents, block_lead) = config.map(|c| (&c.agents, &c.lead)).unwrap_or((&Choice::Inherit, &Choice::Inherit));
    let newest = if nudge { None } else { llm.newest_agent() };
    Chosen {
        agents: settings::choose(agents, &project.agents, block_agents, newest.as_deref()),
        lead: settings::choose(lead, &project.lead, block_lead, None),
    }
}

/// Why a run has no workers, as its failure says it: the layer that said off, or that no
/// layer named one.
pub fn no_workers(pick: &Pick, nudge: bool) -> String {
    match pick.from {
        Some(Layer::Run) => "this run's agents are off".into(),
        Some(Layer::Project) => "the project's settings turn its agents off".into(),
        Some(Layer::Plugin) => "sync.agent is off".into(),
        _ if nudge => {
            "no agent is named for this project: a nudge's run asks only one the project's settings or sync.agent name".into()
        }
        _ => "no llm agent to ask: add one on the LLM Agents page, or name one in the project's settings or sync.agent".into(),
    }
}

/// One queued run: its workers and its lead ([choose]) become runners - the workers one
/// [Pool] - and the run is driven to its end. A map run needs no sync block: without an
/// agent it draws the skeleton alone. A full run and a recheck ask. A docs run reads without
/// one, its documents then waiting for a card. A sync run reads Claude Code's conversations,
/// so only a plugin row with a sync block runs one.
pub(super) async fn run_one(
    service: &std::sync::Arc<MemoryService>,
    seats: &crate::memory_api::MemoryState,
    run: Run,
    stop: &Stop,
) -> Executed {
    let config = seats.sync_config();
    let syncs = matches!(run.trigger, Trigger::Manual | Trigger::Nudge);
    if syncs && config.is_none() {
        return fail(run, "the plugin row has no sync block; a queued run cannot be asked (SPEC §memory.sync)".into());
    }
    let project = match service.project_settings(&run.project).await {
        Ok(project) => project,
        Err(err) => return fail(run, err.message),
    };
    let nudge = run.trigger == Trigger::Nudge;
    let chosen = choose(&run.scope.agents, &run.scope.lead, nudge, &project, config.as_ref(), seats.llm());
    let pool = match chosen.agents.names.is_empty() {
        true => None,
        false => match Pool::of(seats.llm(), &chosen.agents.names) {
            Ok(pool) => Some(pool),
            Err(why) => return fail(run, why),
        },
    };
    let config = config.unwrap_or_else(SyncConfig::empty);
    let workers = pool.as_ref().map(|p| p as &dyn Ask);
    let executed = match run.trigger {
        // A recheck asks the workers alone: the lead does not decide it. A review run too.
        Trigger::Recheck | Trigger::Review => match workers {
            Some(ask) => execute_recheck(service, run, ask, stop).await,
            None => {
                let why = no_workers(&chosen.agents, nudge);
                return fail(run, format!("{why}: a recheck asks a model (SPEC §memory.init The recheck)"));
            }
        },
        // A docs run cards with the workers alone, and reads without them.
        Trigger::Docs => {
            let workers = workers.ok_or_else(|| no_workers(&chosen.agents, nudge));
            execute_docs(service, run, workers, stop).await
        }
        Trigger::Map | Trigger::Full => {
            let lead = match chosen.lead.names.first().map(|name| runner(seats.llm(), name)).transpose() {
                Ok(lead) => lead,
                Err(why) => return fail(run, format!("the lead: {why}")),
            };
            match (run.trigger, workers) {
                (Trigger::Full, Some(ask)) => execute_full(service, run, &config, ask, lead.as_deref(), stop).await,
                (Trigger::Full, None) => {
                    let why = no_workers(&chosen.agents, nudge);
                    return fail(run, format!("{why}: a full run asks a model (SPEC §memory.init)"));
                }
                _ => execute_map(service, run, &config, workers, lead.as_deref(), stop).await,
            }
        }
        Trigger::Manual | Trigger::Nudge => match workers {
            Some(ask) => execute_run(service, run, &config, ask, stop).await,
            None => {
                let why = no_workers(&chosen.agents, nudge);
                return fail(run, format!("{why}: the queued run cannot be asked"));
            }
        },
    };
    tallied(executed, pool.as_ref())
}

/// A run that asked more than one worker records how each went.
pub(super) fn tallied(mut executed: Executed, pool: Option<&Pool>) -> Executed {
    if let Some(pool) = pool.filter(|p| p.member_count() > 1) {
        executed.run.runner.workers = pool.tally();
    }
    executed
}
