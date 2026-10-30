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

//! The live registry (SPEC llm.history Live): a running run leads the first page with
//! what it is doing, its dialogue reads the turns noted so far, the job that started it
//! finds it by its source, and once it files - or its recorder goes away - it is live no
//! more.

#![cfg(test)]

use super::*;

/// The scratch home for one test: the gate's isolated SWISS_HOME plus a unique
/// subdirectory, removed first so a rerun after a red starts clean.
fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("the scratch home removes first");
    }
    dir
}

fn scope(source: &str, parent: Option<i64>) -> RunContext {
    RunContext {
        task: Some("memory.sync".into()),
        subject: Some("shop".into()),
        source: Some(source.into()),
        parent,
    }
}

fn ids(page: &Value) -> Vec<i64> {
    page["runs"].as_array().into_iter().flatten().filter_map(|run| run["id"].as_i64()).collect()
}

#[tokio::test]
async fn a_running_run_lists_live_with_its_turns_so_far_and_files_whole() {
    let history = LlmHistory::open(&home("llm-hist-live-run")).await.expect("the history opens");
    // A run that already filed, so the live one has a page to lead.
    let mut done = history.run_recorder("lead", "builtin", Some("glm"), Some("glm-5.3"));
    done.reserve().await;
    done.note("user", "an earlier ask");
    done.finish(true);
    let made = within(scope("memory:r-1", None), async { history.run_recorder("lead", "builtin", Some("glm"), Some("glm-5.3")) });
    let mut run = made.await;
    run.reserve().await;
    let id = run.id().expect("the run reserves its id");
    run.note("system", "be brief");
    run.note("user", "what changed?");
    run.begin_round().await.expect("a one-shot's round needs no activity mark");
    run.tap().streamed(1200);
    run.note("assistant", "tool call read_file [c1]: {\"path\":\"a.rs\"}");
    run.calling("read_file");

    // The first page leads with it, saying what it does now.
    let page = history.read_runs(0, 25, None, None).await.expect("the page reads");
    assert_eq!(ids(&page).first(), Some(&id), "the live run leads: {page}");
    let first = &page["runs"][0];
    assert!(first["ok"].is_null(), "no outcome yet");
    assert_eq!(first["live"]["round"], 1);
    assert_eq!(first["live"]["step"], "tool");
    assert_eq!(first["live"]["tool"], "read_file");
    assert_eq!(first["live"]["streamed"], 1200);
    assert!(first["live"]["elapsedMs"].is_u64() && first["live"]["stepMs"].is_u64());
    assert_eq!(first["turns"], 3, "the turns noted so far");
    assert_eq!(first["tools"]["read_file"], 1, "its tool calls so far");
    assert_eq!(first["head"], "what changed?", "named by its first ask");
    assert_eq!(first["source"], "memory:r-1");
    // Its agent's page has it, another agent's and the open conversations' do not, and a
    // later page leads with nothing.
    assert!(ids(&history.read_runs(0, 25, Some("lead".into()), None).await.expect("read")).contains(&id));
    assert!(!ids(&history.read_runs(0, 25, Some("other".into()), None).await.expect("read")).contains(&id));
    assert!(!ids(&history.read_runs(0, 25, None, Some(true)).await.expect("read")).contains(&id));
    assert!(!ids(&history.read_runs(1, 1, None, None).await.expect("read")).contains(&id));

    // Its dialogue so far, live.
    let dialogue = history.read_dialogue(id).await.expect("the dialogue reads").expect("the run is there");
    let roles: Vec<&str> = dialogue["turns"].as_array().expect("turns").iter().filter_map(|t| t["role"].as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant"]);
    assert_eq!(dialogue["live"]["tool"], "read_file");

    // The job that started it finds it by its source; another job finds nothing.
    let job = history.read_runs_by_source("memory:r-1".into()).await.expect("the job's runs read");
    assert_eq!(ids(&job), [id]);
    assert_eq!(job["runs"][0]["live"]["step"], "tool");
    assert!(ids(&history.read_runs_by_source("memory:r-2".into()).await.expect("read")).is_empty());

    run.note("tool", "read_file [c1]: fn a() {}");
    run.note("final", "nothing changed");
    run.finish(true);

    // Filed whole, live no more.
    let page = history.read_runs(0, 25, None, None).await.expect("the page reads");
    let first = &page["runs"][0];
    assert_eq!(first["id"], id);
    assert_eq!(first["ok"], true);
    assert!(first.get("live").is_none(), "a filed run is not live: {first}");
    assert_eq!(first["turns"], 5);
    let dialogue = history.read_dialogue(id).await.expect("read").expect("the run");
    assert_eq!(dialogue["turns"].as_array().map(Vec::len), Some(5), "every turn once");
    assert!(dialogue.get("live").is_none());
    let job = history.read_runs_by_source("memory:r-1".into()).await.expect("read");
    assert_eq!(ids(&job), [id]);
    assert!(job["runs"][0].get("live").is_none());
}

#[tokio::test]
async fn a_recorder_dropped_unfiled_leaves_nothing_live() {
    let history = LlmHistory::open(&home("llm-hist-live-drop")).await.expect("the history opens");
    let made = within(scope("memory:r-3", None), async { history.run_recorder("lead", "builtin", None, None) });
    let mut run = made.await;
    run.reserve().await;
    let id = run.id().expect("reserved");
    run.note("user", "a question a deadline will cut");
    assert!(ids(&history.read_runs(0, 25, None, None).await.expect("read")).contains(&id));
    drop(run);
    // A reserved one-shot that never filed hides again, as a stranded row does.
    assert!(!ids(&history.read_runs(0, 25, None, None).await.expect("read")).contains(&id));
    assert!(ids(&history.read_runs_by_source("memory:r-3".into()).await.expect("read")).is_empty());
}

#[tokio::test]
async fn a_live_run_a_tool_started_leads_only_its_agents_page() {
    let history = LlmHistory::open(&home("llm-hist-live-child")).await.expect("the history opens");
    let made = within(scope("memory:r-4", Some(7)), async { history.run_recorder("helper", "builtin", None, None) });
    let mut run = made.await;
    run.reserve().await;
    let id = run.id().expect("reserved");
    assert!(!ids(&history.read_runs(0, 25, None, None).await.expect("read")).contains(&id), "under its parent, not beside it");
    assert!(ids(&history.read_runs(0, 25, Some("helper".into()), None).await.expect("read")).contains(&id));
    assert_eq!(ids(&history.read_runs_by_source("memory:r-4".into()).await.expect("read")), [id]);
    run.finish(false);
}
