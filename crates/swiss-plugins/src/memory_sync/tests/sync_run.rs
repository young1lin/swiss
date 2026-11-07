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

//! One sync run: the baseline, the proposal applied or kept dry, the retries and the stop.

use super::*;

fn proposal_reply() -> String {
    wrapped(
        &json!({
            "summary": "one note",
            "domains": [],
            "notes": [ { "kind": "knowledge", "title": "The third commit adds c", "content": "c.txt holds three." } ],
            "updates": [],
            "marks": []
        })
        .to_string(),
    )
}

#[tokio::test]
async fn a_run_records_the_baseline_then_applies_a_proposal() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("exec");
    let (service, project) = service_with_project("exec", &repo_dir).await;
    let config = quiet_config();
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Empty);
    let advance = done.advance.expect("baseline advance");
    assert_eq!(advance.head, head_of(&repo_dir));
    service.sync_finish(&done.run, Some(&advance)).await.expect("finish");
    // A commit arrives; the next run asks and applies.
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "fourth: five"]);
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert!(!done.run.ops.is_empty(), "the proposal applied");
    assert_eq!(done.run.runner.runner, "canned");
    let advance = done.advance.expect("advance");
    assert_eq!(advance.head, head_of(&repo_dir));
    assert_eq!(advance.commits.len(), 1);
    service.sync_finish(&done.run, Some(&advance)).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    assert_eq!(state.head.as_deref(), Some(head_of(&repo_dir).as_str()));
}

fn rev_of(dir: &std::path::Path, rev: &str) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", rev]).output().expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn a_prompt_over_the_budget_cuts_its_patch_then_sends_fewer_commits() {
    if !git_present() {
        return;
    }
    let dir = repo("fit");
    // Two more commits: four wait after the first.
    for (file, text, subject) in [("d.txt", "four\n", "fourth: four"), ("e.txt", "five\n", "fifth: five")] {
        std::fs::write(dir.join(file), text).expect("write");
        git_at(&dir, &["add", "."]);
        git_at(&dir, &["commit", "-qm", subject]);
    }
    let state = engine::SyncState { head: Some(rev_of(&dir, "HEAD~4")), ..Default::default() };
    let prepare = |walked: &Walk, patch_max: usize| prepare_walk("fit", &[], &[], walked, &Digest::default(), patch_max);
    let all = || walk(&dir, &state, None, engine::COMMITS_MAX);
    let subjects = |walked: &Walk| walked.commits.iter().map(|c| c.subject.clone()).collect::<Vec<_>>();
    // Within the budget: the walk as it was.
    let (kept, whole) = fit(&dir, &state, None, all().await.expect("walk"), engine::PROMPT_BYTES, &prepare).await.expect("fit");
    assert_eq!(kept.commits.len(), 4);
    assert!(!kept.more_commits && !whole.scope.more_commits);
    assert!(!whole.prompt.contains("the patch stops here"));
    // Just over: the patch gives way and every commit still goes.
    let budget = whole.prompt.len() - 1;
    let (kept, prepared) = fit(&dir, &state, None, all().await.expect("walk"), budget, &prepare).await.expect("fit");
    assert_eq!(kept.commits.len(), 4);
    assert!(!kept.more_commits && prepared.prompt.len() <= budget);
    assert!(prepared.scope.patch_bytes < whole.scope.patch_bytes && prepared.scope.patch_cut);
    assert!(prepared.prompt.contains("the patch stops here"), "the prompt says to read the rest in the checkout");
    // A budget only the two-commit prompt without its patch meets: four halve to two, the
    // head the last one sent.
    let budget = prepare(&walk(&dir, &state, None, 2).await.expect("two"), 0).prompt.len();
    let (kept, prepared) = fit(&dir, &state, None, all().await.expect("walk"), budget, &prepare).await.expect("fit");
    assert_eq!(subjects(&kept), ["second: two", "third: three"]);
    assert_eq!(kept.head, rev_of(&dir, "HEAD~2"));
    assert!(prepared.prompt.len() <= budget);
    assert!(kept.more_commits && prepared.scope.more_commits);
    assert!(prepared.prompt.contains("more wait for the next sync"), "the prompt says the rest wait");
    // A budget no prompt meets: one commit goes whatever its size.
    let (kept, prepared) = fit(&dir, &state, None, all().await.expect("walk"), 1, &prepare).await.expect("fit");
    assert_eq!(subjects(&kept), ["second: two"]);
    assert_eq!(kept.head, rev_of(&dir, "HEAD~3"));
    assert!(kept.patch_raw.contains("two two") && !kept.patch_raw.contains("five"), "the patch is the sent commit's");
    assert_eq!(prepared.scope.commits.len(), 1);
}

#[tokio::test]
async fn an_invalid_answer_is_asked_once_more() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("retry");
    let (service, project) = service_with_project("retry", &repo_dir).await;
    // A real first run records the baseline (a dry run records nothing).
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    // The baseline first, so the run reaches the ask.
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![not_json_reply()]), &Stop::default()).await;
    service
        .sync_finish(&baseline.run, baseline.advance.as_ref())
        .await
        .expect("finish");
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "second: five"]);
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let canned = Canned::new(vec![not_json_reply(), proposal_reply()]);
    let done = execute_run(&service, run, &quiet_config(), &canned, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.runner.attempts, 2);
    let prompts = canned.prompts.lock().expect("lock");
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("not a valid proposal"), "the retry carries the why");
}

#[tokio::test]
async fn a_dry_run_keeps_the_proposal_and_applies_nothing() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("dry");
    let (service, project) = service_with_project("dry", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    service.sync_finish(&baseline.run, baseline.advance.as_ref()).await.expect("finish");
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "second: five"]);
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(true, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &quiet_config(), &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok);
    assert!(done.run.proposal.is_some(), "the dry run keeps the proposal for review");
    assert!(done.run.ops.is_empty());
    assert!(done.advance.is_none(), "a dry run moves nothing");
    let state = service.sync_state(&project).await.expect("state");
    assert_ne!(state.head.as_deref(), Some(head_of(&repo_dir).as_str()));
    let (_, memories) = service.sync_material(&project).await.expect("material");
    assert!(memories.is_empty(), "nothing landed");
}

/// The config with transcripts on, reading one scratch directory.
fn transcripts_config(dir: &std::path::Path) -> SyncConfig {
    SyncConfig::parse(&json!({
        "agent": "distiller",
        "transcripts": "claude-code",
        "transcriptDirs": [dir.display().to_string()]
    }))
    .expect("config")
}

#[tokio::test]
async fn a_first_dry_run_leaves_the_baseline_for_the_first_real_run() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("dry-first");
    let transcripts = scratch("dry-first-transcripts");
    let munged = transcripts.join(swiss_memory::transcript::munge(&bound_root(&repo_dir)));
    std::fs::create_dir_all(&munged).expect("dir");
    std::fs::write(munged.join("s.jsonl"), "{\"x\":1}\n").expect("file");
    let config = transcripts_config(&transcripts);
    let (service, project) = service_with_project("dry-first", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(true, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Empty, "{}", done.run.error.clone().unwrap_or_default());
    assert!(done.advance.is_none(), "a dry run moves nothing - not even a first baseline");
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    assert!(state.head.is_none(), "the business head stays unset");
    assert!(state.cursors.is_empty(), "the transcript cursors stay unset");

    // The first real run still sees a first sync: it records the baseline itself.
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Empty, "{}", done.run.error.clone().unwrap_or_default());
    assert!(done.advance.is_some(), "the first real run records the baseline");
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    assert_eq!(state.head.as_deref(), Some(head_of(&repo_dir).as_str()));
    assert_eq!(state.cursors.len(), 1, "the baseline cursor lands with the real run");
}

#[tokio::test]
async fn a_dry_run_over_an_empty_range_keeps_the_cursors() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("dry-empty");
    let transcripts = scratch("dry-empty-transcripts");
    let munged = transcripts.join(swiss_memory::transcript::munge(&bound_root(&repo_dir)));
    std::fs::create_dir_all(&munged).expect("dir");
    std::fs::write(munged.join("s.jsonl"), "{\"x\":1}\n").expect("file");
    let config = transcripts_config(&transcripts);
    let (service, project) = service_with_project("dry-empty", &repo_dir).await;

    // A real first sync baselines HEAD and the transcript's size.
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    assert_eq!(state.cursors.len(), 1, "the baseline cursor is set");

    // The transcript disappears; a dry run over the now-empty range must not prune
    // the cursor or touch the head - only a real run moves business state.
    std::fs::remove_file(munged.join("s.jsonl")).expect("remove");
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(true, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Empty, "{}", done.run.error.clone().unwrap_or_default());
    assert!(done.advance.is_none(), "the empty-range branch is dry too");
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    assert_eq!(state.cursors.len(), 1, "a dry run keeps the cursor");
    assert_eq!(state.head.as_deref(), Some(head_of(&repo_dir).as_str()), "the head stays at the real baseline");
}

#[tokio::test]
async fn a_dsh_session_reaches_the_prompt_beside_a_claude_code_one() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("dsh");
    let cwd = bound_root(&repo_dir);
    let transcripts = scratch("dsh-transcripts");
    let config = transcripts_config(&transcripts);
    let (service, project) = service_with_project("dsh", &repo_dir).await;
    // The first sync records the baseline, before either conversation.
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &config, &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");

    // Then a Claude Code session and a DSH one, both in the repository, side by side in
    // the one configured directory.
    let claude = transcripts.join(swiss_memory::transcript::munge(&cwd));
    std::fs::create_dir_all(&claude).expect("dir");
    let line = json!({"type": "user", "cwd": cwd, "timestamp": "2026-10-05T13:01:02.000Z",
                      "message": {"role": "user", "content": "Why is the importer slow on Windows?"}});
    std::fs::write(claude.join("c.jsonl"), format!("{line}\n")).expect("claude session");
    let session = transcripts.join(swiss_memory::transcript::dsh_key(&cwd)).join("d1");
    std::fs::create_dir_all(&session).expect("dir");
    // One appended batch of whole lines a frame, as DSH writes its .zstd logs.
    let frame = |lines: &[Value]| {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        zstd::encode_all(text.as_bytes(), 0).expect("compress")
    };
    let time = 1_791_205_262_000i64;
    let header = json!({"type": "session", "version": 4, "id": "d1", "createdAt": time, "cwd": cwd,
                        "isSeeded": false, "delegationDepth": 0});
    let user = |kind: &str, text: &str| {
        json!({"type": "user/message", "time": time,
               "data": {"content": [{"type": "text", "text": text}], "source": {"kind": kind}}})
    };
    let assistant = json!({"type": "assistant/message", "time": time + 7000, "data": {"message": {"role": "assistant", "content": [
        {"type": "reasoning", "text": "Reasoning the person never saw"},
        {"type": "text", "text": "The DSH queue stalls because the worker holds the lock."}
    ]}}});
    let log = [
        frame(&[header]),
        frame(&[user("agent-instructions", "Injected project instructions"), user("user", "Why does the DSH queue stall?")]),
        frame(&[assistant]),
    ]
    .concat();
    let dsh_file = session.join("session.v4.jsonl.zstd");
    std::fs::write(&dsh_file, &log).expect("dsh session");

    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let canned = Canned::new(vec![proposal_reply()]);
    let done = execute_run(&service, run, &config, &canned, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    {
        let prompts = canned.prompts.lock().expect("lock");
        for said in [
            "Why is the importer slow on Windows?",
            "### DSH session d1 · 2026-10-05 13:01 UTC",
            "Why does the DSH queue stall?",
            "The DSH queue stalls because the worker holds the lock.",
        ] {
            assert!(prompts[0].contains(said), "{said} missing from the prompt");
        }
        for unsaid in ["Injected project instructions", "Reasoning the person never saw"] {
            assert!(!prompts[0].contains(unsaid), "{unsaid} reached the prompt");
        }
    }
    service.sync_finish(&done.run, done.advance.as_ref()).await.expect("finish");
    let state = service.sync_state(&project).await.expect("state");
    let at = |file: &Path| state.cursors.get(&file.display().to_string()).copied();
    assert_eq!(at(&dsh_file), Some(log.len() as i64), "the DSH cursor sits after its last frame");
    assert_eq!(at(&claude.join("c.jsonl")), Some(line.to_string().len() as i64 + 1));
}

#[tokio::test]
async fn a_runner_that_never_answers_fails_the_run() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("fail");
    let (service, project) = service_with_project("fail", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![not_json_reply()]), &Stop::default()).await;
    service.sync_finish(&baseline.run, baseline.advance.as_ref()).await.expect("finish");
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "second: five"]);
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &quiet_config(), &Canned::new(vec![not_json_reply()]), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Failed);
    assert!(done.run.error.as_deref().unwrap_or_default().contains("twice"));
    assert_eq!(done.run.runner.attempts, 2);
    assert!(done.advance.is_none());
}

#[tokio::test]
async fn a_stop_raised_before_the_question_answers_at_once() {
    let stop = Stop::default();
    assert_eq!(stop.until(async { 7 }).await, Some(7));
    stop.raise();
    assert!(stop.raised());
    assert_eq!(stop.until(async { 7 }).await, None, "a raised stop never starts the work");
    let clone = stop.clone();
    assert!(clone.raised(), "the worker's clone and the route's share one stop");
}

/// A runner that, asked, reads the run's saved row before it answers: what the panel
/// following a running run sees while the agent works.
struct Peeks<'a> {
    service: &'a MemoryService,
    project: String,
    id: String,
    seen: std::sync::Mutex<Option<Run>>,
}

#[async_trait::async_trait]
impl Ask for Peeks<'_> {
    async fn ask(&self, _root: &std::path::Path, _prompt: &str) -> Result<String, String> {
        let saved = self.service.sync_run(&self.project, &self.id).await.map_err(|err| err.message)?;
        *self.seen.lock().expect("lock") = Some(saved);
        Ok(proposal_reply())
    }

    async fn fill(&self, root: &std::path::Path, prompt: &str, _schema: &serde_json::Value) -> Result<String, String> {
        self.ask(root, prompt).await
    }

    fn label(&self) -> String {
        "peeks".into()
    }

    fn model(&self) -> Option<String> {
        Some("glm-5.3".into())
    }
}

#[tokio::test]
async fn a_running_run_says_what_it_sends_and_whom_it_asks_before_the_answer() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("progress");
    let (service, project) = service_with_project("progress", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![proposal_reply()]), &Stop::default()).await;
    service.sync_finish(&baseline.run, baseline.advance.as_ref()).await.expect("finish");
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "second: five"]);
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let peeks = Peeks { service: &service, project: project.clone(), id: run.id.clone(), seen: std::sync::Mutex::new(None) };
    let done = execute_run(&service, run, &quiet_config(), &peeks, &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    // While the agent was asked, the row already held the phase, the runner and the scope.
    let seen = peeks.seen.lock().expect("lock").take().expect("the runner was asked");
    assert_eq!(seen.scope.phase, Some(Phase::Ask));
    assert_eq!(seen.runner.runner, "peeks");
    assert_eq!(seen.runner.model.as_deref(), Some("glm-5.3"));
    assert_eq!(seen.scope.commits.len(), 1, "the commits it sends");
    assert!(seen.scope.prompt_bytes > 0);
    // Finished, the run is in no phase.
    assert_eq!(done.run.scope.phase, None);
    assert_eq!(done.run.runner.runner, "peeks");
}
