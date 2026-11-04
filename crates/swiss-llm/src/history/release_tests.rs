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

//! The stale-round release (see release.rs): an interrupted conversation whose drive is
//! gone continues; one whose drive still runs answers busy.

use super::release::{holder_drives, RELEASED_RESULT};
use super::*;

/// One scenario must settle well inside this bound; a wedged store fails instead of hanging.
const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

/// File one tool round of run `id` under `attempt`: the model asked `calls` (name, id).
fn file_tool_round(history: &LlmHistory, id: i64, attempt: Option<u64>, calls: &[(&str, &str)]) {
    file_tool_round_on(history, id, attempt, calls, &json!({}));
}

/// The same round, sealed beside the wire facts it flew on.
fn file_tool_round_on(history: &LlmHistory, id: i64, attempt: Option<u64>, calls: &[(&str, &str)], wire: &Value) {
    let replay = json!([{ "role": "user", "text": "count the rows" }]);
    let texts = ["Checking.".to_string()];
    let tool_calls: Vec<(String, String, String)> =
        calls.iter().map(|(name, id)| (name.to_string(), "{}".to_string(), id.to_string())).collect();
    history.record_chat_in_run(
        id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "count the rows",
            turn: Some(0),
            wire: Some(wire),
            request: None,
            replay: Some(&replay),
            attempt,
            outcome: ChatOutcome::Ok {
                texts: &texts,
                thinking: &[],
                tool_calls: &tool_calls,
                cut: false,
                tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                ms: 5,
                ttft_ms: None,
                request_id: "swiss-test-release",
                stop_reason: None,
                contents: &Value::Null,
            },
        },
    );
}

async fn resume(history: &Arc<LlmHistory>, id: i64) -> Result<RunRecorder, ResumeError> {
    history.resume_conversation(id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None).await
}

#[test]
fn a_holder_drives_only_while_the_process_that_set_it_lives() {
    assert!(holder_drives("not a holder", 1), "a holder that does not parse fails closed");
    assert!(!holder_drives("0:5:1", 1), "a process that is gone drives nothing");
    let started = swiss_core::platform::process_created_at(std::process::id()).unwrap_or(0);
    if started != 0 {
        let reused = format!("{}:{}:1", std::process::id(), started + 1);
        assert!(!holder_drives(&reused, 1), "a live pid that started at another time is another process");
    }
    let unknown_handle = format!("{}:{started}:{}", std::process::id(), u64::MAX);
    assert!(!holder_drives(&unknown_handle, 1), "a handle of this process that is gone drives nothing");
}

/// A Stop mid-tool whose round could not settle: the round's call landed, the audit tail
/// holds the one result in hand. The door pairs it, and answers the call that never
/// reported with an UNKNOWN verdict - never a made-up result.
#[tokio::test]
async fn a_released_tool_round_keeps_the_results_in_hand_and_marks_the_rest_unknown() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-release-audit-tail");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
        let id = run.id().expect("reserved");
        let attempt = run.begin_round().await.expect("the mark commits");
        file_tool_round(&history, id, Some(attempt), &[("probe", "call_a"), ("probe", "call_b")]);
        history.flush().await;
        run.file_tail(json!({ "tools": [{ "id": "call_a", "name": "probe", "text": "rows: 3" }] }));
        history.flush().await;
        drop(run);
        let mut resumed = resume(&history, id).await.expect("the stale round is released at the door");
        let prior = match history.replay_state(id).await {
            ReplayState::Continuable { prior } => prior,
            ReplayState::Refused(why) => panic!("a released round must replay: {why}"),
            ReplayState::Fresh => panic!("the tool round must not disappear"),
        };
        let tools: Vec<(&Value, &Value)> = prior
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| (&message["id"], &message["text"]))
            .collect();
        assert_eq!(
            tools,
            vec![(&json!("call_a"), &json!("rows: 3")), (&json!("call_b"), &json!(RELEASED_RESULT))],
            "every call answered once, in call order: {prior:?}"
        );
        assert!(resumed.begin_round().await.is_ok(), "the next round claims on the released attempt");
        history.close().await.expect("close");
    })
    .await
    .expect("the release settles within the bound");
}

/// A mark another handle's live drive holds is in flight: the door answers busy and
/// releases nothing. Once that drive is gone, the same door releases it.
#[tokio::test]
async fn a_mark_another_handle_still_drives_is_busy_until_its_drive_ends() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-release-busy");
        let first = LlmHistory::open(&dir).await.expect("open first");
        let second = LlmHistory::open(&dir).await.expect("open second");
        let mut run = first.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
        let id = run.id().expect("reserved");
        run.begin_round().await.expect("the mark commits");
        assert!(
            matches!(resume(&second, id).await, Err(ResumeError::Busy)),
            "a round still driven is never released from under its drive"
        );
        drop(run);
        assert!(resume(&second, id).await.is_ok(), "the drive is gone: the door releases its round");
        assert!(matches!(second.replay_state(id).await, ReplayState::Fresh), "no round filed: nothing replays");
        first.close().await.expect("close first");
        second.close().await.expect("close second");
    })
    .await
    .expect("the busy scenario settles within the bound");
}

/// A gateway restart mid-round: the process that set the mark is gone (its holder is
/// rewritten to a pid no process has, as a dead daemon's reads to the next one). The
/// next handle's door releases the round, and the cut call reads UNKNOWN.
#[tokio::test]
async fn a_round_a_restart_cut_is_released_by_the_next_handle() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-release-restart");
        let before = LlmHistory::open(&dir).await.expect("open");
        let mut run = before.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
        let id = run.id().expect("reserved");
        let attempt = run.begin_round().await.expect("the mark commits");
        file_tool_round(&before, id, Some(attempt), &[("probe", "call_cut")]);
        before.flush().await;
        // The process dies here: no tail, no settle, no finish.
        before
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("UPDATE run SET holder = '0:1:1' WHERE id = ?").bind(id).execute(&pool).await.map(|_| ())
                })
            })
            .await
            .expect("the holder names a gone process");
        drop(run);
        before.close().await.expect("close");
        drop(before);
        let after = LlmHistory::open(&dir).await.expect("reopen");
        assert!(resume(&after, id).await.is_ok(), "the restart's stranded round is released");
        match after.replay_state(id).await {
            ReplayState::Continuable { prior } => assert!(
                prior.iter().any(|message| message["id"] == "call_cut" && message["text"] == RELEASED_RESULT),
                "the cut call reads UNKNOWN: {prior:?}"
            ),
            ReplayState::Refused(why) => panic!("the released conversation must replay: {why}"),
            ReplayState::Fresh => panic!("the filed tool round must not disappear"),
        }
        after.close().await.expect("close");
    })
    .await
    .expect("the restart scenario settles within the bound");
}

/// A deadline that dropped the drive mid-tool (as a Data or Test loop past its agent's
/// timeoutMs once was; no agent run has a time cap now, but the rows such a cut filed
/// stay continuable): the tool round sealed on its wire, then the deadline filed its stand-in -
/// the wait, no wire facts, no replay - and the drive went away. The stand-in seals
/// nothing the model was told, so the door reads past it: the same provider, model and
/// wire continue, the cut call reads UNKNOWN, and the stand-in stays on record.
#[tokio::test]
async fn a_deadline_that_dropped_the_drive_leaves_the_conversation_continuable_on_its_wire() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-release-deadline");
        let history = LlmHistory::open(&dir).await.expect("open");
        let wire = json!({ "endpoint": "https://glm.example/v4/messages", "headers": { "x-style": "anthropic" } });
        let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
        let id = run.id().expect("reserved");
        let attempt = run.begin_round().await.expect("the mark commits");
        file_tool_round_on(&history, id, Some(attempt), &[("run_sql", "call_slow")], &wire);
        history.flush().await;
        history.record_chat_timeout_in_run(id, ChatTimeout {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "count the rows",
            turn: None,
            waited_ms: 1_800_000,
        });
        history.flush().await;
        drop(run);
        let resumed = history
            .resume_conversation(id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), Some(&wire))
            .await;
        let mut resumed = match resumed {
            Ok(resumed) => resumed,
            Err(why) => panic!("the provider, model and wire are the ones in force; a deadline strands nothing: {why:?}"),
        };
        match history.replay_state(id).await {
            ReplayState::Continuable { prior } => assert!(
                prior.iter().any(|message| message["id"] == "call_slow" && message["text"] == RELEASED_RESULT),
                "the call the deadline cut reads UNKNOWN: {prior:?}"
            ),
            ReplayState::Refused(why) => panic!("the conversation must replay past the stand-in: {why}"),
            ReplayState::Fresh => panic!("the sealed tool round must not disappear"),
        }
        assert!(resumed.begin_round().await.is_ok(), "the next round claims on the released attempt");
        let calls: i64 = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query_scalar("SELECT count(*) FROM call WHERE run_id = ?").bind(id).fetch_one(&pool).await
                })
            })
            .await
            .expect("count the calls");
        assert_eq!(calls, 2, "the deadline's stand-in stays on record beside the round it cut");
        history.close().await.expect("close");
    })
    .await
    .expect("the deadline scenario settles within the bound");
}
