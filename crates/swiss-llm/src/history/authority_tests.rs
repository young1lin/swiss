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

//! REGRESSION WITNESSES for the two landed authority/accounting
//! contracts. Wired into history/mod.rs as
//! `#[cfg(test)] mod authority_tests;` beside `mod version_tests;`.
//! Both contracts were earned RED at their designed phases before the
//! fixes landed and are green since; each witness now holds its contract
//! and goes red again only on a regression - the historical reds are
//! recorded below, at each witness, as the behavior they pin.
//!
//! Witness A holds: no next-round authorization without the round's
//! canonical call - settle_round (history/recorder.rs) proves, inside its OWN
//! transaction and before any write, that THIS attempt's canonical
//! Ok-with-tools call row actually landed (the CURRENT attempt's durable
//! proof, never an EXISTS-any-prior-call guess), and only then clears the
//! pending/attempt mark. The refusal is driven through the EXISTING
//! production policy refresh seat: after the reservation and the activity
//! mark are positively awaited and the store is quiescent, set_quota with
//! an EMPTY share map makes the store's own admission refuse the real
//! call (the close-queue seam's empty-share precedent), and the ordinary
//! default quota is restored through the same seat BEFORE the small real
//! settlement, which then admits. This is a POLICY-FLIP call-refusal /
//! settle-admission witness - the set_quota facade is a test-driven seam
//! and production normally runs the default policy, so this is NOT an
//! operator runtime configuration feature; it is NOT a simultaneous
//! same-quota-threshold proof, and NOT a paid/provider-outbound proof.
//! The contract is observed as begin_round refusing on the same recorder,
//! the same returned attempt token and the same exact wire ids the loop
//! would have settled. Historically red here: the settle once cleared the
//! mark on token match ALONE - no call row, settle Ok, pending 0 - and
//! the next round minted attempt 2 over a round whose sealed body never
//! landed.
//!
//! Witness B holds: the terminal clear inside insert_call's transaction
//! (history/rows.rs) is ATOMIC and weight-clean - it sets tail = NULL and
//! tail_plain = 0 together, so once the raw tail reference is NULL the
//! decoded continuation size (conversation_size sums tail_plain) carries
//! no tail weight at all; the budget is exactly the calls' own
//! writer-known weights. Historically red here: the clear once nulled
//! the reference but kept the settled weight - tail_plain read 138
//! against the contracted 0 - so the budget still charged a tail that no
//! longer existed.
//!
//! Hermetic by construction: one store handle per scenario (no
//! cross-store FIFO assumptions), every step an awaited receipt, a flush
//! or a production read; no sleeps; no provider, no network, no outbound,
//! no paid-loss claim - these witnesses pin store authorization and
//! accounting only. The whole scenario is bounded (BOUND) so a wedged
//! store fails the test instead of hanging the gate.

#![cfg(test)]

use super::*;

/// The bound on every park in these witnesses: one settled scenario must
/// fit well inside it, and a wedged store fails the test instead of
/// hanging the gate.
const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// The scratch home for one witness run: test_home() (the gate's isolated
/// SWISS_HOME) plus a module-unique subdirectory, removed first so a rerun
/// after a red cannot inherit a half-built database.
fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("the scratch home removes first");
    }
    dir
}

/// The marker row's (pending, attempt), read positively through the store.
async fn marker_of(history: &Arc<LlmHistory>, run_id: i64) -> (i64, i64) {
    history
        .store
        .query(move |pool| {
            Box::pin(async move {
                let row = sqlx::query("SELECT pending, attempt FROM run WHERE id = ?")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await?;
                Ok::<(i64, i64), sqlx::Error>((row.try_get(0)?, row.try_get(1)?))
            })
        })
        .await
        .expect("the marker read must succeed")
}

/// The run's call rows, counted positively through the store.
async fn calls_of(history: &Arc<LlmHistory>, run_id: i64) -> i64 {
    history
        .store
        .query(move |pool| {
            Box::pin(async move {
                sqlx::query("SELECT count(*) FROM call WHERE run_id = ?")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await?
                    .try_get(0)
            })
        })
        .await
        .expect("the call count read must succeed")
}

/// WITNESS A: a settlement may not authorize a next round without the
/// canonical call - the CURRENT attempt's durable proof, not any prior
/// call's presence. The round's small, canonical Ok-with-tools body is
/// REFUSED by a real runtime policy flip on the existing set_quota
/// facade (an empty share map; a test-driven seam - production normally
/// runs the default policy, so this is not an operator runtime
/// configuration feature), positively proven absent while the activity
/// mark persists; the ordinary default quota is restored before the
/// small results settlement - the same attempt receipt, the same exact
/// wire id the loop files - which then must NOT open the next-round
/// gate. A POLICY-FLIP call-refusal / settle-admission witness: not a
/// simultaneous same-quota-threshold proof, not a paid/provider-outbound
/// proof.
#[tokio::test]
async fn a_settlement_may_not_authorize_a_next_round_without_the_canonical_call() {
    let settled = tokio::time::timeout(BOUND, async {
        let mut trail: Vec<String> = Vec::new();
        let dir = home("llm-hist-authority-no-canonical");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let id = run.id().expect("reserved");

        // The round's ADMITTED activity mark: the receipt is the attempt
        // token every later write must match - never an arbitrary number.
        let first = run.begin_round().await.expect("the activity mark commits");
        trail.push(format!("attempt {first} committed"));

        // A quiescent baseline: every prior write has completed, so the
        // admission counters move only for what happens next.
        history.flush().await;
        let (admitted_before, completed_before, _, _) = history.store.counters();

        // The POLICY FLIP: the existing production re-enrollment seat
        // (LlmHistory::set_quota -> store.set_quota) under an EMPTY share
        // map - every priced write refuses, the same refusal the
        // close-queue seam pins. A legitimate policy change between the
        // call's filing and its dispatch result; no unadmitted SQL.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
            std::collections::BTreeMap::new(),
        ));
        trail.push("the share map emptied: priced writes refuse".into());

        // The round's Ok-with-tools ask, shaped exactly as the loop files
        // it and SMALL - a legitimate canonical replay log, tool list and
        // digest, well inside every production bound. The store's own
        // admission refuses the write; no job exists, no SQL runs.
        let context = json!([{ "role": "user", "text": "check the probe" }]);
        let asked = json!({ "tools": [ { "name": "probe" } ] });
        let digest: Vec<(String, String, String)> = vec![(
            "probe".to_string(),
            "{}".to_string(),
            "call_probe".to_string(),
        )];
        let texts = ["Two checks.".to_string()];
        history.record_chat_in_run(
            id,
            ChatCall {
                provider: "glm",
                model: "glm-5.3",
                agent: Some("alpha"),
                system: Some("You are a test."),
                prompt: "check the probe",
                turn: Some(0),
                wire: Some(&json!({})),
                request: Some(&asked),
                replay: Some(&context),
                attempt: run.attempt(),
                outcome: ChatOutcome::Ok {
                    texts: &texts,
                    thinking: &[],
                    tool_calls: &digest,
                    cut: false,
                    tokens: crate::provider::Tokens {
                        input: 1,
                        output: 1,
                        cache_read: 0,
                        cache_creation: 0,
                    },
                    ms: 5,
                    ttft_ms: None,
                    request_id: "swiss-test-authority-refused",
                    stop_reason: Some("tool_calls"),
                    contents: &json!({}),
                },
            },
        );
        history.flush().await;

        // Positively proven refused: the admission counters never moved
        // (no seat was taken), the canonical call is absent, and the live
        // marker is unchanged.
        let (admitted_after, completed_after, _, _) = history.store.counters();
        assert_eq!(
            (admitted_after, completed_after),
            (admitted_before, completed_before),
            "the refused round never took the admission seat",
        );
        let calls = calls_of(&history, id).await;
        assert_eq!(calls, 0, "the refused round filed no canonical call");
        trail.push("the canonical call is positively absent: admission denied".into());
        let (pending, attempt) = marker_of(&history, id).await;
        assert_eq!((pending, attempt), (1, first as i64), "the mark persists in flight");
        trail.push(format!("the mark persists: pending={pending} attempt={attempt}"));

        // The ordinary default quota restored through the SAME refresh
        // seat, BEFORE the settlement - the settle's admission is then the
        // production default quota's own answer.
        history.set_quota(swiss_host::history::quota::QuotaPolicy::default());
        trail.push("the ordinary default quota restored".into());

        // The small results settlement the loop would file: the SAME
        // returned attempt (the recorder's own receipt) and the SAME exact
        // wire id the refused round's digest carried. Its estimate is tiny,
        // so the production admission seat takes it.
        let tail = json!({ "tools": [
            { "id": "call_probe", "name": "probe", "text": "the answer is 42" },
        ] });
        let outcome = run.settle_round(tail).await;
        trail.push(match &outcome {
            Ok(()) => "the settlement admitted and reported success".into(),
            Err(why) => format!("the settlement refused: {why}"),
        });
        history.flush().await;
        let calls_after = calls_of(&history, id).await;
        let (pending_after, attempt_after) = marker_of(&history, id).await;
        trail.push(format!(
            "after the settlement attempt: calls={calls_after} pending={pending_after} attempt={attempt_after}"
        ));

        // THE CONTRACT: without the canonical call, no next-round
        // authorization. The recorder is the loop's own; the token and ids
        // are the ones it actually observed. The durable proof is landed:
        // settle_round proves THIS attempt's Ok call inside its own
        // transaction before any write. Historically red here: the settle
        // once cleared the mark on token match alone (no call row, settle
        // Ok, pending 0) and this minted a fresh attempt.
        let next = run.begin_round().await;
        assert!(
            next.is_err(),
            "a next round must not be authorized without the round's canonical call: got {:?} - trail {trail:?}",
            next,
        );
        trail.push("the next round was refused without the canonical call".into());

        history.close().await.expect("close");
        trail
    })
    .await
    .expect("the no-canonical-settlement witness settles within the bound");
    assert!(!settled.is_empty(), "the witness ran to its close: {settled:?}");
}

/// WITNESS B: a cleared tail leaves no stale weight in the continuation
/// budget. A real landed tool call, its awaited sealed settlement, then a
/// real token-carrying terminal round on the next positively committed
/// mark - the raw tail reference must read NULL, and the decoded
/// continuation size must carry none of the settled tail's weight.
#[tokio::test]
async fn a_cleared_tail_leaves_no_stale_weight_in_the_continuation_budget() {
    let settled = tokio::time::timeout(BOUND, async {
        let mut trail: Vec<String> = Vec::new();
        let dir = home("llm-hist-authority-tail-weight");
        let history = LlmHistory::open(&dir).await.expect("open");
        let mut run = history
            .open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3"))
            .await;
        let id = run.id().expect("reserved");

        // Round one: a REAL landed tool call, small enough to admit.
        let first = run.begin_round().await.expect("the activity mark commits");
        let context = json!([{ "role": "user", "text": "check the probe" }]);
        let digest: Vec<(String, String, String)> = vec![(
            "probe".to_string(),
            "{}".to_string(),
            "call_probe".to_string(),
        )];
        let texts = ["Two checks.".to_string()];
        history.record_chat_in_run(
            id,
            ChatCall {
                provider: "glm",
                model: "glm-5.3",
                agent: Some("alpha"),
                system: Some("You are a test."),
                prompt: "check the probe",
                turn: Some(0),
                wire: Some(&json!({})),
                request: Some(&json!({})),
                replay: Some(&context),
                attempt: run.attempt(),
                outcome: ChatOutcome::Ok {
                    texts: &texts,
                    thinking: &[],
                    tool_calls: &digest,
                    cut: false,
                    tokens: crate::provider::Tokens {
                        input: 1,
                        output: 1,
                        cache_read: 0,
                        cache_creation: 0,
                    },
                    ms: 5,
                    ttft_ms: None,
                    request_id: "swiss-test-authority-tail",
                    stop_reason: Some("tool_calls"),
                    contents: &json!({}),
                },
            },
        );
        history.flush().await;
        assert_eq!(calls_of(&history, id).await, 1, "the tool round's call landed");
        trail.push(format!("round one landed on attempt {first}"));

        // The AUTHORITATIVE results settlement: awaited, sealed, exact ids.
        run.settle_round(json!({ "tools": [
            { "id": "call_probe", "name": "probe", "text": "the answer is 42" },
        ] }))
            .await
            .expect("the completed round's results settle");
        history.flush().await;

        // Control BEFORE: the raw reference is live and carries weight.
        let (tail_live, tail_plain_live) = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT tail, tail_plain FROM run WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    Ok::<(Option<String>, i64), sqlx::Error>((
                        row.try_get(0)?,
                        row.try_get(1)?,
                    ))
                })
            })
            .await
            .expect("the tail control read must succeed");
        assert!(tail_live.is_some(), "the settled tail's raw reference is live");
        assert!(
            tail_plain_live > 0,
            "the settled tail carries a positive decoded weight: {tail_plain_live}",
        );
        trail.push(format!("tail live, weight {tail_plain_live}"));

        // The next round, positively committed: the receipt, then the
        // marker read, before the terminal call files against its token.
        let second = run.begin_round().await.expect("the second mark commits");
        assert!(second > first, "the versions are monotonic: {first} -> {second}");
        let (pending, attempt) = marker_of(&history, id).await;
        assert_eq!((pending, attempt), (1, second as i64), "the second mark is positively committed");
        trail.push(format!("round two marked on attempt {second}"));

        // The TERMINAL call: a plain answer, no tool calls, carrying the
        // current attempt token - the settle rides its own transaction.
        let terminal_context = json!([
            { "role": "user", "text": "check the probe" },
            { "role": "assistant", "text": "Two checks." },
            { "role": "tool", "id": "call_probe", "text": "the answer is 42" },
            { "role": "user", "text": "summarize" },
        ]);
        let answer = ["done".to_string()];
        history.record_chat_in_run(
            id,
            ChatCall {
                provider: "glm",
                model: "glm-5.3",
                agent: Some("alpha"),
                system: Some("You are a test."),
                prompt: "summarize",
                turn: Some(1),
                wire: Some(&json!({})),
                request: Some(&json!({})),
                replay: Some(&terminal_context),
                attempt: run.attempt(),
                outcome: ChatOutcome::Ok {
                    texts: &answer,
                    thinking: &[],
                    tool_calls: &[],
                    cut: false,
                    tokens: crate::provider::Tokens {
                        input: 1,
                        output: 1,
                        cache_read: 0,
                        cache_creation: 0,
                    },
                    ms: 5,
                    ttft_ms: None,
                    request_id: "swiss-test-authority-terminal",
                    stop_reason: None,
                    contents: &json!({}),
                },
            },
        );
        history.flush().await;
        assert_eq!(calls_of(&history, id).await, 2, "the terminal round landed");

        // The raw tail reference must read NULL: the terminal settle's own
        // contract (the control half).
        let (tail_after, tail_plain_after) = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT tail, tail_plain FROM run WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?;
                    Ok::<(Option<String>, i64), sqlx::Error>((
                        row.try_get(0)?,
                        row.try_get(1)?,
                    ))
                })
            })
            .await
            .expect("the cleared-tail read must succeed");
        assert!(tail_after.is_none(), "the terminal round cleared the raw tail reference: {tail_after:?}");
        trail.push("the raw tail reference is NULL".into());

        // THE CONTRACT: a NULL tail contributes no decoded weight - the
        // terminal clear zeroes tail_plain atomically with the reference,
        // in insert_call's own transaction. Historically red here: the
        // clear once nulled the reference but kept the weight (tail_plain
        // read 138 against the contracted 0), so the budget still charged
        // the settled tail.
        assert_eq!(
            tail_plain_after, 0,
            "a cleared tail must leave no stale decoded weight: {tail_plain_after} (was {tail_plain_live}) - trail {trail:?}",
        );

        // The same contract through the public size read: with no turn
        // rows and a NULL tail, the continuation budget is exactly the
        // calls' own writer-known weights - no tail share, none stale.
        let turns: i64 = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT count(*) FROM turn WHERE run_id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?
                        .try_get(0)
                })
            })
            .await
            .expect("the turn count read must succeed");
        assert_eq!(turns, 0, "the scenario notes no turns");
        let call_weights: i64 = history
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query("SELECT COALESCE(sum(body_plain), 0) FROM call WHERE run_id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await?
                        .try_get(0)
                })
            })
            .await
            .expect("the call weights read must succeed");
        let (_, budget, _) = history.conversation_size(id).await.expect("the size read must succeed");
        assert_eq!(
            budget, call_weights,
            "the continuation budget carries no stale tail weight once the tail is NULL: budget {budget} vs call weights {call_weights} - trail {trail:?}",
        );
        trail.push("the continuation budget carries no stale tail weight".into());

        history.close().await.expect("close");
        trail
    })
    .await
    .expect("the cleared-tail-weight witness settles within the bound");
    assert!(!settled.is_empty(), "the witness ran to its close: {settled:?}");
}
