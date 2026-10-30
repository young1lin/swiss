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

#[tokio::test]
async fn a_close_is_a_queued_exempt_write_not_a_query_mutation() {
    let dir = swiss_core::paths::test_home().join("llm-hist-close-queue");
    let history = LlmHistory::open(&dir).await.expect("open");
    let run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    // Baseline QUIESCENT first: the reserve job's own completion (quota
    // release, the completed counter) lands on the worker AFTER the
    // commit receipt the recorder already holds - the barrier waits it
    // out, race-free, so the deltas below count only the close.
    history
        .store
        .flush_barrier()
        .await
        .expect("the reserve's completion must settle within the barrier");
    let (a0, c0, _, _) = history.store.counters();
    assert_eq!(a0, c0, "baseline quiescent: every admitted write completed before the close is counted");
    // Priced writes refuse from here on; an EXEMPT lifecycle stamp must
    // still be admitted (its only bound is the queue's own).
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
        std::collections::BTreeMap::new(),
    ));
    history
        .close_run(run_id)
        .await
        .expect("the tiny lifecycle stamp is unpriced and must land under an empty quota")
        .expect("the run exists");
    // The documented race-free completion wait: no sleep, no ordering race.
    history
        .store
        .flush_barrier()
        .await
        .expect("a queued close must complete within the barrier");
    let (a1, c1, _, _) = history.store.counters();
    assert_eq!(
        a1 - a0,
        1,
        "close must take the queued write seat - admission accounting must positively count it (query-side mutation counts nothing)"
    );
    assert_eq!(
        c1 - c0,
        1,
        "close must COMPLETE through the queue - completion accounting must positively count it"
    );
    // Non-bypass rider: a PRICED write under the same empty quota still
    // refuses, so exempt admission did not become a quota bypass.
    let before = history.store.counters();
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("rider"),
        system: None,
        prompt: "rider",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["ok".to_string()],
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
            ms: 5,
            ttft_ms: None,
            request_id: "swiss-test-close-queue-rider",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    history
        .store
        .flush_barrier()
        .await
        .expect("the barrier holds even with nothing new admitted");
    assert_eq!(
        history.store.counters(),
        before,
        "a priced write under the empty quota must refuse - exempt must not become a quota bypass"
    );
    history.close().await.expect("close");
}
