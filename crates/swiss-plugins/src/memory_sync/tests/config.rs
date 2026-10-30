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

//! The `sync` block, read field by field, and whom a run asks.

use super::*;

#[test]
fn empty_is_the_empty_block() {
    // empty() hand-builds what parse reads out of {}; the two must not drift apart.
    assert_eq!(SyncConfig::empty(), block(json!({})).expect("an empty block is valid"));
}

/// An llm row with two builtin agents over an unreachable provider: `b` is added last.
fn two_agents() -> swiss_llm::config::LlmConfig {
    swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] } },
        "agents": { "a": { "provider": "local" }, "b": { "provider": "local" } }
    }))
    .expect("row")
}

/// The first layer that says anything decides the workers and the lead (SPEC
/// §memory.settings): the run's own pick, the project's settings, the sync block; past
/// them the workers fall back to the newest agent - never for a nudge - and the lead to
/// none.
#[test]
fn a_run_asks_the_agents_the_first_layer_names() {
    let llm = LlmState::new();
    llm.apply(two_agents());
    let none = ProjectSettings::default();
    let pick = |names: &[&str], from: Layer| Pick { names: names.iter().map(|n| n.to_string()).collect(), from: Some(from) };
    let chosen = choose(&Choice::Inherit, &Choice::Inherit, false, &none, None, &llm);
    assert_eq!(chosen, Chosen { agents: pick(&["b"], Layer::Newest), lead: Pick::default() });
    let nudged = choose(&Choice::Inherit, &Choice::Inherit, true, &none, None, &llm);
    assert!(nudged.agents.names.is_empty(), "a nudge asks only an agent someone named");
    assert!(no_workers(&nudged.agents, true).contains("a nudge's run asks only one"));

    let block = SyncConfig::parse(&json!({ "agent": "a", "lead": "a" })).expect("block");
    let chosen = choose(&Choice::Inherit, &Choice::Inherit, true, &none, Some(&block), &llm);
    assert_eq!(chosen, Chosen { agents: pick(&["a"], Layer::Plugin), lead: pick(&["a"], Layer::Plugin) });
    let project = ProjectSettings::parse(&json!({ "agents": ["a", "b"], "lead": "b" })).expect("settings");
    let chosen = choose(&Choice::Inherit, &Choice::Inherit, false, &project, Some(&block), &llm);
    assert_eq!(chosen, Chosen { agents: pick(&["a", "b"], Layer::Project), lead: pick(&["b"], Layer::Project) });
    // Each is decided on its own: the run turns the workers off and keeps the project's lead.
    let chosen = choose(&Choice::Off, &Choice::Inherit, false, &project, Some(&block), &llm);
    assert_eq!(chosen, Chosen { agents: pick(&[], Layer::Run), lead: pick(&["b"], Layer::Project) });
    assert_eq!(no_workers(&chosen.agents, false), "this run's agents are off");
    let off = ProjectSettings::parse(&json!({ "agents": "off" })).expect("settings");
    let chosen = choose(&Choice::Inherit, &Choice::Inherit, false, &off, Some(&block), &llm);
    assert_eq!(no_workers(&chosen.agents, false), "the project's settings turn its agents off");
    llm.stop();
    let chosen = choose(&Choice::Inherit, &Choice::Inherit, false, &none, None, &llm);
    assert!(no_workers(&chosen.agents, false).contains("no llm agent to ask"), "a stopped plugin has no newest");
}

/// A run with no workers fails saying which layer turned them off; a named agent the llm
/// row does not hold fails the run on its name.
#[tokio::test]
async fn a_run_without_its_agents_fails_naming_why() {
    let llm = LlmState::new();
    llm.apply(two_agents());
    let seats = crate::memory_api::MemoryState::with_llm(llm);
    let dir = scratch("chosen");
    let (service, project) = service_with_project("chosen", &dir).await;
    let service = std::sync::Arc::new(service);
    service.patch_project_settings(&project, &json!({ "agents": "off" })).await.expect("settings");
    let (run, _) = service.sync_enqueue(&project, recheck_enqueue(false, None, None)).await.expect("enqueue");
    let done = run_one(&service, &seats, run, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Failed);
    let error = done.run.error.clone().unwrap_or_default();
    assert!(error.starts_with("the project's settings turn its agents off: a recheck asks a model"), "{error}");
    service.sync_finish(&done.run, None).await.expect("finish");
    // The run's own pick outranks the project's off, and names an agent nobody added.
    let request = Enqueue { agents: named(&["zz"]), ..recheck_enqueue(false, None, None) };
    let (run, _) = service.sync_enqueue(&project, request).await.expect("enqueue");
    assert_eq!(run.scope.agents, named(&["zz"]), "the record keeps the run's pick");
    let done = run_one(&service, &seats, run, &Stop::default()).await;
    let error = done.run.error.clone().unwrap_or_default();
    assert!(error.contains("agent \"zz\""), "{error}");
}

#[test]
fn the_sync_block_is_checked_field_by_field() {
    // Left out, the agent is decided below the block: a project's settings or the newest.
    let off = block(json!({})).expect("empty inherits");
    assert_eq!((off.agents.clone(), off.auto, off.min_interval_sec), (Choice::Inherit, true, 600));
    assert!(off.transcript_dirs.is_empty());
    assert_eq!(block(json!({ "agent": "off" })).expect("off").agents, Choice::Off);
    assert_eq!(block(json!({ "agent": "" })).expect("empty").agents, Choice::Off, "an empty name stays off");
    let named = block(json!({ "agent": " distiller ", "minIntervalSec": 60 })).expect("named");
    assert_eq!((named.agents.clone(), named.min_interval_sec), (self::named(&["distiller"]), 60));
    assert!(block(json!({ "agent": "Not A Name" })).is_err());
    // A list is a pool; it shows as the list, one agent as its name.
    let pool = block(json!({ "agent": ["glm", "big"] })).expect("pool");
    assert_eq!(pool.agents, self::named(&["glm", "big"]));
    assert_eq!(pool.to_json()["agent"], json!(["glm", "big"]));
    assert!(block(json!({ "agent": ["glm", "glm"] })).expect_err("twice").contains("sync.agent"));
    // The llm plugin's name rule, restated in the memory crate: the two agree.
    for name in ["glm", "a-1", "0x", "-a", "A", "a_b", "", &"x".repeat(32), &"x".repeat(33)] {
        assert_eq!(settings::name_ok(name), swiss_llm::config::name_ok(name), "{name:?}");
    }
    // A row from before the move says where its runner went.
    let moved = block(json!({ "runner": "claude" })).expect_err("moved");
    assert!(moved.contains("sync.agent") && moved.contains("llm"), "{moved}");
    assert!(block(json!({ "agent": "x", "oops": 1 })).is_err());
    assert!(block(json!({ "transcriptDirs": ["relative"] })).is_err());
    assert!(block(json!({ "transcriptDirs": ["~/notes"] })).is_ok(), "~ expands");
    assert!(!block(json!({ "transcripts": "off" })).expect("off").transcripts);
    assert!(block(json!({ "minIntervalSec": 86_401 })).is_err());
    let shown = named.to_json();
    assert_eq!((shown["agent"].clone(), shown["minIntervalSec"].clone()), (json!("distiller"), json!(60)));
    assert_eq!(shown["transcripts"], json!("claude-code"));
    assert!(off.to_json().get("agent").is_none(), "an inherited agent is not shown");
    assert_eq!(block(json!({ "agent": "off" })).expect("off").to_json()["agent"], json!("off"));
    // A lead is one agent named the same way; the API shows it only when set.
    let led = block(json!({ "agent": "distiller", "lead": " planner " })).expect("led");
    assert_eq!(led.lead, self::named(&["planner"]));
    assert_eq!(led.to_json()["lead"], json!("planner"));
    assert_eq!(block(json!({ "lead": "off" })).expect("off").lead, Choice::Off);
    assert!(block(json!({ "lead": "Not A Name" })).expect_err("a name").contains("sync.lead"));
    assert!(block(json!({ "lead": ["a", "b"] })).expect_err("one").contains("sync.lead"));
    assert!(off.to_json().get("lead").is_none());
}

#[test]
fn a_nudge_never_runs_sooner_than_the_interval() {
    let now = 1_000_000i64;
    assert_eq!(nudge_due(None, 600, now), now + NUDGE_DELAY_MS);
    let recent = now - 10_000;
    assert_eq!(nudge_due(Some(recent), 600, now), recent + 600_000);
    assert_eq!(nudge_due(Some(recent), 0, now), now + NUDGE_DELAY_MS, "a zero interval still keeps the delay");
}

#[test]
fn transcripts_off_reads_no_conversation_directories() {
    // The panel's pending count and the walk both ask this one method; with
    // conversations off it must name no directory, or a machine with years of
    // Claude Code sessions reports hundreds of unread megabytes (found live).
    let off = quiet_config();
    assert!(off.transcript_dir_list().is_empty());
    let on = SyncConfig::parse(&json!({ "agent": "distiller", "transcripts": "claude-code" })).expect("parse");
    let dirs = on.transcript_dir_list();
    // Left out, the dirs are Claude Code's and DSH's both.
    for tail in [[".claude", "projects"], [".dsh", "sessions"]] {
        assert!(dirs.iter().any(|d| d.ends_with(Path::new(tail[0]).join(tail[1]))), "{tail:?} missing from {dirs:?}");
    }
}
