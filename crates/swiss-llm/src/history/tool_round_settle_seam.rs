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

/// File a REAL successful tool-call round exactly as the loop files it: the
/// admitted call row (an Ok outcome, its tool-call digest, the canonical
/// replay context) for a run whose activity mark begin_round committed -
/// the authoritative completion seam, attempt token and all. No provider,
/// no network: the filing itself is the seam under test.
async fn file_tool_round(
    history: &Arc<LlmHistory>,
    agent: &str,
    calls: &[(&str, &str, &str)],
) -> (i64, RunRecorder) {
    let mut run = history.open_conversation(agent, "builtin", Some("glm"), Some("glm-5.3")).await;
    let id = run.id().expect("reserved");
    run.begin_round().await.expect("the activity mark commits");
    let context = json!([{ "role": "user", "text": "check both" }]);
    let digest: Vec<(String, String, String)> = calls
        .iter()
        .map(|(name, args, call_id)| (name.to_string(), args.to_string(), call_id.to_string()))
        .collect();
    let wire = json!({});
    let asked = json!({});
    history.record_chat_in_run(
        id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some(agent),
            system: Some("You are a test."),
            prompt: "check both",
            turn: Some(0),
            wire: Some(&wire),
            request: Some(&asked),
            replay: Some(&context),
            attempt: run.attempt(),
            outcome: ChatOutcome::Ok {
                texts: &["Two checks.".to_string()],
                thinking: &[],
                tool_calls: &digest,
                cut: false,
                tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                ms: 5,
                ttft_ms: None,
                request_id: "swiss-test-tool-round",
                stop_reason: Some("tool_calls"),
                contents: &json!({}),
            },
        },
    );
    history.flush().await;
    (id, run)
}

#[tokio::test]
async fn an_unresolved_tool_round_stays_fail_closed_until_its_results_land() {
    let dir = swiss_core::paths::test_home().join("llm-hist-tool-round-settle");
    let history = LlmHistory::open(&dir).await.expect("open");
    let (id, mut run) = file_tool_round(
        &history,
        "alpha",
        &[("quick", "{}", "call_quick"), ("stall", "{}", "call_stall")],
    )
    .await;
    // The round flew and sealed, but its tool RESULTS are unresolved: no
    // answers, no UNKNOWN verdicts, nothing. A continuation must fail
    // closed here - replaying the round with dangling calls would ask the
    // provider a request it cannot answer truthfully.
    match history.replay_state(id).await {
        ReplayState::Refused(_) => {}
        ReplayState::Continuable { prior } => panic!("an unresolved tool round must refuse continuation, not replay dangling calls: {prior:?}"),
        ReplayState::Fresh => panic!("a round with unresolved tool calls is not fresh"),
    }
    // The AUTHORITATIVE results are what reopen it - the completed round's
    // own settlement seam: the results tail is SEALED, the activity mark
    // cleared, one transaction, the receipt awaited. Preserved EXACTLY:
    // each result paired by its exact wire id, the completed answer and
    // the UNKNOWN verdict alike.
    run.settle_round(json!({ "tools": [
        { "id": "call_quick", "name": "quick", "text": "the answer is 42" },
        { "id": "call_stall", "name": "stall", "text": "execution unknown - cancelled before it could finish" },
    ] }))
        .await
        .expect("the authoritative results settle their round");
    history.flush().await;
    match history.replay_state(id).await {
        ReplayState::Continuable { prior } => {
            let tools: Vec<&Value> = prior.iter().filter(|one| one["role"] == "tool").collect();
            assert_eq!(tools.len(), 2, "both exact results ride the resumed context: {prior:?}");
            assert_eq!(tools[0]["id"], json!("call_quick"), "pairing by exact wire id: {prior:?}");
            assert_eq!(tools[0]["text"], json!("the answer is 42"), "the completed result is preserved verbatim: {prior:?}");
            assert_eq!(tools[1]["id"], json!("call_stall"), "pairing by exact wire id: {prior:?}");
            assert_eq!(
                tools[1]["text"],
                json!("execution unknown - cancelled before it could finish"),
                "the UNKNOWN verdict is preserved verbatim: {prior:?}",
            );
        }
        ReplayState::Refused(why) => panic!("landed authoritative results must reopen the round with each exact result: {why}"),
        ReplayState::Fresh => panic!("a round with landed results is not fresh"),
    }
    history.close().await.expect("close");
}
