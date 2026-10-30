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

/// Text that does not compress away: a body codec must not hide the bytes the
/// trim is meant to free.
fn noise(seed: u64, len: usize) -> String {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            char::from(b"0123456789abcdef"[(state >> 60) as usize])
        })
        .collect()
}

/// File one chat whose single message is its own piece.
fn file(history: &LlmHistory, run: Option<i64>, tag: &str, seed: u64) {
    let request = json!({ "messages": [{ "role": "user", "content": format!("{tag} {}", noise(seed, 16 * 1024)) }] });
    let texts = ["ok".to_string()];
    let contents = json!({});
    let call = ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("trim"),
        system: Some("You are a test."),
        prompt: tag,
        turn: Some(0),
        wire: None,
        request: Some(&request),
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &texts,
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
            ms: 5,
            ttft_ms: None,
            request_id: tag,
            stop_reason: None,
            contents: &contents,
        },
    };
    match run {
        Some(run) => history.record_chat_in_run(run, call),
        None => history.record_chat(call),
    }
}

async fn calls(history: &LlmHistory) -> Vec<(i64, String)> {
    history
        .store
        .query(|pool| Box::pin(async move { sqlx::query_as("SELECT id, request_id FROM call ORDER BY id").fetch_all(&pool).await }))
        .await
        .expect("the calls read")
}

/// A history of `count` plain calls behind one open conversation, its share set
/// so the live pages sit at `percent` of it.
async fn crowded(tag: &str, count: u64, percent: u64) -> (Arc<LlmHistory>, i64) {
    let dir = swiss_core::paths::test_home().join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    let history = LlmHistory::open(&dir).await.expect("open");
    let run = history.open_conversation("keep", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    file(&history, Some(run_id), "open-conversation", 7);
    for n in 0..count {
        file(&history, None, &format!("plain-{n:03}"), 100 + n);
    }
    history.flush().await;
    let live = live_bytes(&history.store).await.expect("the live bytes");
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
        [("llm".to_string(), live * 100 / percent)].into_iter().collect(),
    ));
    (history, run_id)
}

#[tokio::test]
async fn a_trim_drops_the_oldest_history_and_its_orphan_pieces_but_never_an_open_conversation() {
    let (history, _) = crowded("llm-hist-size-trim", 48, 95).await;
    let share = history.store.share().expect("enrolled");
    let before = swiss_host::history::quota::measure(&history.store.policy().db_file).total();
    let trimmed = history.trim().await;
    assert!(trimmed.calls > 0, "a history over 90% of its share must lose its oldest calls: {trimmed:?}");
    assert!(trimmed.pieces > 0, "the pieces only the dropped calls named must go too: {trimmed:?}");
    let live = live_bytes(&history.store).await.expect("the live bytes");
    assert!(live <= share / 100 * TRIM_GOAL_PCT, "the live pages fit the goal: {live} of {share}");
    let after = swiss_host::history::quota::measure(&history.store.policy().db_file).total();
    assert!(after < before, "the compaction shrinks the file itself: {before} -> {after}");
    let left = calls(&history).await;
    assert!(left.iter().any(|(_, tag)| tag == "open-conversation"), "an open conversation's call is never trimmed, though it is the oldest: {left:?}");
    assert!(!left.iter().any(|(_, tag)| tag == "plain-000"), "the oldest plain call goes first: {left:?}");
    assert!(left.iter().any(|(_, tag)| tag == "plain-047"), "the newest call stays: {left:?}");
    for (id, tag) in &left {
        assert!(
            history.read_call_body(*id).await.expect("the body read").is_some(),
            "every call left still opens whole - no piece it names was swept: {tag}",
        );
    }
    // Under its share now, a second pass has nothing to do.
    assert_eq!(history.trim().await, Trimmed::default(), "a history under 90% of its share is left alone");
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_write_refused_over_the_share_starts_a_trim_and_the_next_write_lands() {
    let (history, _) = crowded("llm-hist-size-trim-refused", 48, 110).await;
    file(&history, None, "refused", 1);
    history.flush().await;
    assert!(!calls(&history).await.iter().any(|(_, tag)| tag == "refused"), "a write over the share is refused");
    // The refusal started the trim; wait it out, bounded.
    let waited = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while history.trim_gate.running.load(Ordering::Acquire) || history.trim_gate.last_ms.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "the refused write must start a trim that finishes");
    file(&history, None, "after", 2);
    history.flush().await;
    assert!(calls(&history).await.iter().any(|(_, tag)| tag == "after"), "with the trim done, writes land again");
    history.close().await.expect("close");
}
