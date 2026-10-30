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

use super::*;

/// One settled scenario must fit well inside this bound; a wedged store
/// fails the test instead of hanging the gate.
const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// The scratch home for one witness run: module-unique, removed first
/// so a rerun after a red inherits nothing.
fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

/// LOST RESERVATION REPLY: the caller refuses (no id, no authority) while
/// the row IS committed - positively read, never inferred from the
/// silence - and it stays genuinely unused: settled, Fresh, resumable,
/// and this agent's one live conversation.
#[tokio::test]
async fn a_lost_reservation_reply_leaves_a_committed_unused_row_and_a_refused_caller() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-lost-reserve-reply");
        let history = LlmHistory::open(&dir).await.expect("open");
        history.test_set_drop_reply(Some("reserve"));
        let run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        assert!(run.id().is_none(), "no proof, no id: {:?}", run.refusal());
        assert!(run.refusal().is_some_and(|why| why.contains("lost")), "the caller fails closed: {:?}", run.refusal());
        // The drain here buys VISIBILITY of the committed row - the
        // read below is a positive SELECT, never an absence inference.
        history.test_set_drop_reply(None);
        history.flush().await;
        let row = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT id, pending, attempt FROM run WHERE agent = ? AND kind = 1")
                        .bind("alpha")
                        .fetch_optional(&pool)
                        .await?
                        .map(|row| Ok::<(i64, i64, i64), sqlx::Error>((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
                        .transpose()
                })
            })
            .await
            .expect("the committed-row read must succeed");
        let (id, pending, attempt) = row.expect("the reservation SQL committed even though its reply was lost");
        assert_eq!((pending, attempt), (0, 0), "the stranded row is settled and genuinely unused");
        assert!(matches!(history.replay_state(id).await, ReplayState::Fresh));
        assert_eq!(history.continuable_open("alpha").await, Some(id), "the committed row is the agent's one live conversation");
        assert!(
            history.resume_conversation(id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
                .await
                .is_ok(),
            "the unused row is resumable: a lost reply is not activity",
        );
        history.close().await.expect("close");
    })
    .await
    .expect("the lost-reservation scenario settles within the bound");
}

/// LOST ACTIVITY REPLY: the mark IS committed (positively read) while the
/// caller holds no authority - begin_round's receipt never arrived, so
/// the recorder's own claim fails, replay and the public resume refuse,
/// and an implicit NEW conversation is blocked until the EXPLICIT close.
#[tokio::test]
async fn a_lost_activity_reply_commits_the_mark_but_grants_no_authority() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-lost-activity-reply");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let id = run.id().expect("reserved");
        history.test_set_drop_reply(Some("activity"));
        let claim = tokio::time::timeout(BOUND, run.begin_round()).await.expect("the claim settles within the bound");
        assert!(
            claim.as_ref().is_err_and(|why| why.contains("lost")),
            "no receipt, no authority: the recorder fails closed: {claim:?}",
        );
        // Visibility drain, then the POSITIVE read: the mark committed.
        history.test_set_drop_reply(None);
        history.flush().await;
        let row = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT pending, attempt FROM run WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    Ok::<(i64, i64), sqlx::Error>((row.try_get(0)?, row.try_get(1)?))
                })
            })
            .await
            .expect("the committed-mark read must succeed");
        assert_eq!(row, (1, 1), "the activity SQL committed even though its reply was lost");
        assert!(
            matches!(history.replay_state(id).await, ReplayState::Refused(_)),
            "replay refuses the unresolved mark",
        );
        assert!(
            matches!(
                history.resume_conversation(id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None).await,
                Err(ResumeError::NoCanonical),
            ),
            "the public resume refuses the same unresolved mark",
        );
        let second = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        assert!(
            second.id().is_none() && second.refusal().is_some_and(|why| why.contains("open-run-exists")),
            "an implicit new conversation is blocked beside the stranded mark: {:?}",
            second.refusal(),
        );
        // The recovery escape is the EXPLICIT close, then a fresh open.
        history.close_run(id).await.expect("the explicit close succeeds").expect("the run was open");
        let fresh = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        assert!(fresh.id().is_some(), "after the explicit close a new conversation opens");
        history.close().await.expect("close");
    })
    .await
    .expect("the lost-activity scenario settles within the bound");
}

/// TOKEN SAFETY (defense in depth, not a witnessed paid-loss story): a
/// STALE terminal completion - an actual terminal ChatCall transaction
/// carrying an attempt token older than the live marker - must not erase
/// the newer marker. The newer marker is positively committed first; the
/// assertion reads the actual pending/attempt through the store and the
/// audit rows (both call rows) are preserved.
#[tokio::test]
async fn a_stale_attempt_completion_cannot_erase_a_newer_marker() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-stale-token");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let id = run.id().expect("reserved");
        // Round one: marked, then TERMINALLY completed - the settle rides
        // the insert's own transaction (the seam under proof).
        let first = run.begin_round().await.expect("the first mark commits");
        let replay = json!([{ "role": "user", "text": "ask one" }]);
        // File one TERMINAL round through the actual insert transaction
        // (the settle rides it); locals live inside the helper so the
        // borrowed ChatCall never outlives its refs.
        fn file_terminal(history: &LlmHistory, id: i64, attempt: Option<u64>, replay: &Value) {
            let wire = json!({});
            let texts = ["answer one".to_string()];
            history.record_chat_in_run(
                id,
                ChatCall {
                    provider: "glm",
                    model: "glm-5.3",
                    agent: Some("alpha"),
                    system: None,
                    prompt: "ask one",
                    turn: Some(0),
                    wire: Some(&wire),
                    request: None,
                    replay: Some(replay),
                    attempt,
                    outcome: ChatOutcome::Ok {
                        texts: &texts,
                        thinking: &[],
                        tool_calls: &[],
                        cut: false,
                        tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                        ms: 5,
                        ttft_ms: None,
                        request_id: "swiss-test-stale-token",
                        stop_reason: None,
                        contents: &Value::Null,
                    },
                },
            );
        }
        file_terminal(&history, id, Some(first), &replay);
        history.flush().await;
        // A NEWER marker, positively committed.
        let second = run.begin_round().await.expect("the newer mark commits");
        assert!(second > first, "the versions are monotonic: {first} -> {second}");
        history.flush().await;
        // The stale completion: the SAME terminal call shape under the
        // OLD token, through the actual insert transaction.
        file_terminal(&history, id, Some(first), &replay);
        history.flush().await;
        let row = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT pending, attempt FROM run WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    Ok::<(i64, i64), sqlx::Error>((row.try_get(0)?, row.try_get(1)?))
                })
            })
            .await
            .expect("the marker read must succeed");
        assert_eq!(
            row,
            (1, second as i64),
            "the newer marker survives the stale completion untouched",
        );
        assert!(
            matches!(history.replay_state(id).await, ReplayState::Refused(_)),
            "the round the newer marker owns is still honestly in flight",
        );
        let calls = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT count(*) FROM call WHERE run_id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    row.try_get::<i64, _>(0)
                })
            })
            .await
            .expect("the audit read must succeed");
        assert_eq!(calls, 2, "both call rows are preserved for audit, stale or not");
        history.close().await.expect("close");
    })
    .await
    .expect("the stale-token scenario settles within the bound");
}

/// REGRESSION RIDER (declared with the r3 authority GO): a newer
/// round's settlement may not be blessed by a PRIOR attempt's
/// canonical call. Attempt one's terminal call lands and settles at
/// its own insert; once a second mark is in flight, only a call that
/// landed under THAT attempt can prove a settle - the earlier round's
/// row must never substitute, and the newer mark stays in flight.
#[tokio::test]
async fn a_newer_round_settles_only_on_its_own_attempt_call() {
    tokio::time::timeout(BOUND, async {
        let dir = home("llm-hist-settle-own-attempt");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let id = run.id().expect("reserved");
        let first = run.begin_round().await.expect("the first mark commits");
        let replay = json!([{ "role": "user", "text": "ask one" }]);
        fn file_terminal(history: &LlmHistory, id: i64, attempt: Option<u64>, replay: &Value) {
            let wire = json!({});
            let texts = ["answer one".to_string()];
            history.record_chat_in_run(
                id,
                ChatCall {
                    provider: "glm",
                    model: "glm-5.3",
                    agent: Some("alpha"),
                    system: None,
                    prompt: "ask one",
                    turn: Some(0),
                    wire: Some(&wire),
                    request: None,
                    replay: Some(replay),
                    attempt,
                    outcome: ChatOutcome::Ok {
                        texts: &texts,
                        thinking: &[],
                        tool_calls: &[],
                        cut: false,
                        tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                        ms: 5,
                        ttft_ms: None,
                        request_id: "swiss-test-settle-own-attempt",
                        stop_reason: None,
                        contents: &Value::Null,
                    },
                },
            );
        }
        file_terminal(&history, id, Some(first), &replay);
        history.flush().await;
        let second = run.begin_round().await.expect("the second mark commits");
        assert!(second > first, "the versions are monotonic: {first} -> {second}");
        // A results settlement for the SECOND round while only the
        // FIRST attempt's call exists: refused, mark untouched.
        let refused = run
            .settle_round(json!({ "tools": [
                { "id": "call_late", "name": "probe", "text": "a late answer" },
            ] }))
            .await;
        assert!(
            refused.as_ref().is_err_and(|why| why.contains("canonical call never landed")),
            "a prior attempt's call must not settle the newer round: {refused:?}",
        );
        history.flush().await;
        let row = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT pending, attempt FROM run WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    Ok::<(i64, i64), sqlx::Error>((row.try_get(0)?, row.try_get(1)?))
                })
            })
            .await
            .expect("the marker read must succeed");
        assert_eq!(
            row,
            (1, second as i64),
            "the newer mark stays in flight: no proof, no clear",
        );
        assert!(
            matches!(history.replay_state(id).await, ReplayState::Refused(_)),
            "the continuation refuses the honestly in-flight round",
        );
        history.close().await.expect("close");
    })
    .await
    .expect("the own-attempt settle scenario settles within the bound");
}
