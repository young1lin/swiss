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

//! REGRESSION WITNESS: the stale-reader authorization contract for the
//! activity protocol (SPEC llm.history). This module is wired into history/mod.rs
//! as `#[cfg(test)] mod version_tests;`. begin_round binds its claim to the
//! activity version THIS recorder observed - its reservation or its resume
//! read - through the expected-version CAS, so a recorder whose prior
//! predates a competing writer's settled round is refused, never silently
//! re-blessed. Historically this file was the expected-RED holder of that
//! missing rule (the pre-guard CAS bound to pending = 0 only); it now stands
//! as the permanent pin that the rule is never lost again.
//!
//! The scenario, receipt-ordered (every await is a mark receipt, a flush, or a
//! production read; no sleeps, no cross-handle schedule assumptions - cross-handle
//! order is established only by observing durable state, the same synchronizer the
//! existing two-handle tests use): a reader (B) resumes an open conversation and
//! reads its canonical prior; an independent writer (A) then commits ANOTHER real
//! terminal round on the same run through its own resumed recorder - awaited
//! begin_round receipt, sealed ChatCall carrying A's attempt token, awaited flush;
//! the production replay read proves A's paid marker is now the canonical tail
//! (an observation only - it never rebinds B's recorder). B, whose prior
//! predates A's round, must then be REFUSED a new round before any
//! outbound: its stale basis must not silently displace A's paid turn from the
//! canonical chain. Before the prior-version guard, this claim succeeded on
//! the reopened pending = 0 gate and the test held the red; with the
//! basis-bound CAS it must refuse forever after - this witness is that
//! permanent regression contract.
//!
//! Hermetic by construction: the home rides swiss_core::paths::test_home() (the
//! gate-installed scratch SWISS_HOME), wiped per run by the home() helper below -
//! no OS temp helpers, no direct unadmitted SQL, no provider, no network, no
//! hand-assigned version numbers: every attempt token quoted here was minted by a
//! real awaited begin_round receipt.

#![cfg(test)]

use super::*;

/// The bound on every park in this witness: one settled scenario must fit well
/// inside it, and a wedged store fails the test instead of hanging the gate.
const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// The scratch home for one run of this witness: test_home() (the gate's isolated
/// SWISS_HOME) plus a module-unique subdirectory, removed first so a rerun after a
/// red cannot inherit a half-built database.
fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

/// File ONE real terminal round through the production seams: the caller's own
/// recorder has already committed its activity mark (the awaited begin_round
/// receipt this helper is handed back), and this files the sealed ChatCall that
/// completes it - a plain answered ask with a canonical replay log, no tool calls,
/// so the round settles its mark cleanly and the run reads Continuable right after.
/// The replay context is exactly what drive_loop seals: the prior the reader
/// rebuilt, plus this round's user turn.
fn file_terminal_round(
    history: &Arc<LlmHistory>,
    run_id: i64,
    attempt: Option<u64>,
    context: &[Value],
    user_text: &str,
    answer_text: &str,
    request_id: &str,
) {
    let replay = Value::Array(context.to_vec());
    let asked = json!({});
    let wire = json!({});
    history.record_chat_in_run(
        run_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: Some("You are a test."),
            prompt: user_text,
            turn: Some(0),
            wire: Some(&wire),
            request: Some(&asked),
            replay: Some(&replay),
            attempt,
            outcome: ChatOutcome::Ok {
                texts: &[answer_text.to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                ms: 5,
                ttft_ms: None,
                request_id,
                stop_reason: None,
                contents: &json!({}),
            },
        },
    );
}

/// The stale-reader witness. A seeds one settled terminal round; B resumes and
/// reads the canonical prior (the old context); A commits a second real terminal
/// round on its own resumed recorder with an awaited mark receipt and a flush; the
/// production replay read (from B's handle - cross-handle durable visibility)
/// proves A's paid marker is the canonical tail - observation only, never a
/// rebind; and only then does B attempt its claim on the recorder it originally
/// resumed, which must refuse the stale basis before anything could fly.
#[tokio::test]
async fn a_stale_reader_may_not_begin_a_round_on_a_moved_context() {
    let settled = tokio::time::timeout(BOUND, async {
        // The observed-state trail every diagnostic quotes.
        let mut trail: Vec<String> = Vec::new();

        // Two handles, one home: the store's own claimed concurrency surface.
        let dir = home("llm-hist-version-stale-reader");
        let handle_a = LlmHistory::open(&dir).await.expect("open first");
        let handle_b = LlmHistory::open(&dir).await.expect("open second");
        trail.push("handles open".into());

        // --- Seed: A opens the conversation and files one REAL terminal round ---
        let mut seed = handle_a
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let run_id = seed.id().expect("reserved");
        seed.begin_round().await.expect("the seed round's activity mark commits");
        let seed_context = json!([{ "role": "user", "text": "seed ask" }]);
        file_terminal_round(
            &handle_a,
            run_id,
            seed.attempt(),
            seed_context.as_array().unwrap(),
            "seed ask",
            "seed answer SEED-V1",
            "swiss-test-version-seed",
        );
        handle_a.flush().await;
        trail.push(format!("seed round settled on run {run_id}"));

        // The seed is canonically continuable: the prior the lane would hand a reader.
        let prior_v1 = match handle_b.replay_state(run_id).await {
            ReplayState::Continuable { prior } => prior,
            ReplayState::Refused(why) => panic!("the seed round must leave the run continuable: {why}"),
            ReplayState::Fresh => panic!("a conversation with a sealed seed round is not fresh"),
        };
        assert!(
            prior_v1.iter().any(|m| m["text"].as_str().is_some_and(|t| t.contains("SEED-V1"))),
            "the seed answer is the canonical tail: {prior_v1:?}"
        );
        trail.push(format!("prior_v1 captured, {} messages", prior_v1.len()));

        // --- B: a legitimate resume of the SAME identity, prior read BEFORE A's round ---
        // wire: None keeps the provenance guard out of the way exactly as the
        // existing resume tests do; agent/runtime/provider/model all match.
        let mut b = handle_b
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
            .await
            .expect("B resumes the open conversation legitimately");
        assert_eq!(b.id(), Some(run_id));
        let b_prior = match handle_b.replay_state(run_id).await {
            ReplayState::Continuable { prior } => prior,
            ReplayState::Refused(why) => panic!("B's replay read of the settled seed must be continuable: {why}"),
            ReplayState::Fresh => panic!("a conversation with a sealed seed round is not fresh"),
        };
        assert_eq!(
            b_prior.len(),
            prior_v1.len(),
            "B read the same settled context the seed left: trail {trail:?}"
        );

        // --- A: commits ANOTHER real terminal round via its own resumed recorder ---
        let mut a2 = handle_a
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
            .await
            .expect("A resumes its own conversation");
        let a2_attempt = a2.begin_round().await.expect("A's second activity mark commits");
        trail.push(format!("A committed attempt {a2_attempt}"));
        // drive_loop's shape exactly: the prior A rebuilt, plus this round's user turn.
        let mut a2_context = prior_v1.clone();
        a2_context.push(json!({ "role": "user", "text": "the second ask" }));
        file_terminal_round(
            &handle_a,
            run_id,
            a2.attempt(),
            &a2_context,
            "the second ask",
            "paid answer PAID-A2",
            "swiss-test-version-a2",
        );
        handle_a.flush().await;
        trail.push("A's paid round settled".into());

        // --- PROOF: A's paid marker is visible from the production replay read ---
        // Read from B's handle: cross-handle durable visibility, no shared memory.
        // Observation only - prior_v2 never reaches B's recorder; the claim below
        // stands on B's originally resumed basis.
        let prior_v2 = match handle_b.replay_state(run_id).await {
            ReplayState::Continuable { prior } => prior,
            ReplayState::Refused(why) => panic!("A's settled terminal round must keep the run continuable: {why}"),
            ReplayState::Fresh => panic!("a conversation with sealed rounds is not fresh"),
        };
        assert!(
            prior_v2.iter().any(|m| m["text"].as_str().is_some_and(|t| t.contains("PAID-A2"))),
            "A's paid marker must be the canonical tail before B claims: trail {trail:?}, prior {prior_v2:?}"
        );
        assert_eq!(
            prior_v2.len(),
            prior_v1.len() + 2,
            "A's round added its user turn and its answer: trail {trail:?}"
        );
        trail.push("A's paid turn proven in the canonical chain".into());

        // --- THE WITNESS: B's claim must refuse its stale basis BEFORE any outbound ---
        // The recorder is the one B resumed before A's round, unchanged by the
        // observation reads above; its prior (prior_v1) predates A's round and
        // the run's canonical context has moved. Letting B begin here would fly a
        // request whose sealed replay displaces A's paid turn - refuse.
        // Pre-guard this claim returned Ok on the reopened pending = 0 gate;
        // the basis-bound CAS must refuse it for good - the regression this
        // witness permanently pins.
        match b.begin_round().await {
            Err(why) => trail.push(format!("B's stale claim refused: {why}")),
            Ok(attempt) => panic!(
                "B must not begin a round on a prior that predates A's paid round - \
                the claim would fly a stale context (attempt {attempt} minted on a moved \
                chain): trail {trail:?}"
            ),
        }

        handle_b.close().await.expect("close second");
        handle_a.close().await.expect("close first");
        trail
    })
    .await
    .expect("the stale-reader witness settles within the bound");
    assert!(!settled.is_empty(), "the witness ran to its close: {settled:?}");
}
