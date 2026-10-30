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

//! The conversation lane (SPEC §llm.agents Test): which run a live Test send joins
//! (a NEW conversation, reserved open before any provider request, or THAT one,
//! reopened on its own row with its canonical prior context) and every guard that
//! refuses a send before it flies. Each refusal carries the machine code a route
//! answers with; none of them waits on the provider. The route owns only the
//! transport (the seat, the stream, the status codes).

use std::sync::Arc;

use serde_json::Value;

use crate::history::{within, LlmHistory, ReplayState, ResumeError, RunContext, RunRecorder};
use crate::provider::Chat;

/// The continuation guards' ceilings: a conversation past any of them refuses its
/// next send - never a silent clip of the dialogue. The byte ceiling is the chat's
/// [context cap](Chat::context_cap) (the agent's `maxContextKiB`), which bounds ONE
/// request: it is weighed against the prior context the send replays, never the
/// run's record, whose call bodies each re-send the dialogue before them and so
/// sum far past any one request.
pub const RUN_TURN_CAP: i64 = 400;
pub const RUN_CALL_CAP: i64 = 512;

/// The heading of the snapshot a consumer may close a conversation's system prompt
/// with - the Test's environment: where the model works, on what machine, on what
/// day. It is taken once, as the conversation opens, and a continuation replays it as
/// noted, so a send never rewrites what the dialogue began under.
pub const SNAPSHOT_HEADING: &str = "# Environment";

/// What a compacted conversation replays ahead of its checkpoint, as the user's turn
/// the checkpoint answers (see [fold]): the turns it condensed are gone from the replay.
pub const COMPACT_FRAMING: &str = "This conversation was compacted to free its context: the turns before this point are condensed into the checkpoint that follows, which you wrote. Treat it as established background and build on it without restating it; carry on from the messages after it, without acknowledging this note.";

/// The conversation one send joined.
pub struct Lane {
    /// The run's recorder: reserved open (a new conversation) or reopened on its
    /// own row (a continued one) - it notes only what this send adds.
    pub run: RunRecorder,
    /// The run's row id.
    pub run_id: i64,
    /// Whether this send continues an earlier one.
    pub continued: bool,
    /// The canonical prior context the send replays: the last call's sealed
    /// replay log plus its response; empty for a new or never-asked run.
    pub prior: Vec<Value>,
    /// The system prompt the dialogue opened with, as noted - its snapshot
    /// included - which a continuation sends again; None for a new or never-asked
    /// run, which opens with the consumer's own.
    pub system: Option<String>,
}

/// Why a send cannot join a conversation.
#[derive(Debug, PartialEq, Eq)]
pub enum LaneError {
    /// A guard refused: its machine code (`open-run-exists`, `gone`, `closed`,
    /// `context-changed`, `unresolved-tail`, `over-budget`) and the reader's message.
    Refused { code: &'static str, message: String },
    /// The history could not reserve a run row for a new conversation.
    Unavailable(String),
}

fn refused(code: &'static str, message: impl Into<String>) -> LaneError {
    LaneError::Refused { code, message: message.into() }
}

const OPEN_RUN_EXISTS: &str = "this agent already has an open conversation: continue it or close it first";

/// Guard and build the conversation a send joins. `joining` names the run to
/// continue; None opens a new one. `runtime` is the label the run row files
/// under, `system` the agent's system prompt in force - a continuation's
/// dialogue must have opened with it, alone or closed by its snapshot.
pub async fn join(
    history: &Arc<LlmHistory>,
    joining: Option<i64>,
    agent: &str,
    runtime: &str,
    chat: &Chat,
    system: &str,
) -> Result<Lane, LaneError> {
    let Some(joining) = joining else {
        return open(history, agent, runtime, chat).await;
    };
    let (run, prior, noted) = resume(history, joining, agent, runtime, chat, system).await?;
    // The whole dialogue replays, so the byte budget bounds what one request may
    // carry: the replay is the next request's floor, and at the cap no room is
    // left for the new words. Each round past it the loop weighs its own exact
    // ask. Over it refuses, never a silent clip.
    let replayed = serde_json::to_vec(&prior).map(|bytes| bytes.len()).unwrap_or(0);
    if replayed >= chat.context_cap() {
        run.release();
        return Err(refused(
            "over-budget",
            format!(
                "the conversation's replay passed the agent's {} KiB request budget (maxContextKiB): start a New one",
                chat.context_cap() / 1024
            ),
        ));
    }
    if let Ok((turns, _, calls)) = history.conversation_size(joining).await {
        if turns >= RUN_TURN_CAP || calls >= RUN_CALL_CAP {
            run.release();
            return Err(refused(
                "over-budget",
                format!(
                    "the conversation passed {RUN_TURN_CAP} turns or {RUN_CALL_CAP} model calls: start a New one"
                ),
            ));
        }
    }
    Ok(Lane { run, run_id: joining, continued: true, prior, system: noted })
}

/// Move the open conversation `run_id` to the chat's provider and model before a send
/// joins it or a compaction folds it: the reader picked them (the Data agent's model menu).
/// What it named before stays in History's calls; a row that is not an open conversation
/// of this agent here stays as it is, and the guards that follow say why.
pub async fn retarget(history: &Arc<LlmHistory>, run_id: i64, agent: &str, chat: &Chat) -> Result<(), LaneError> {
    history
        .retarget_conversation(run_id, agent, Some(&chat.provider), Some(&chat.model))
        .await
        .map(|_| ())
        .map_err(LaneError::Unavailable)
}

/// The guards a continuation passes, the budget aside: the run reopens on its own row
/// (its agent and scope, the provider, model and wire in force), its dialogue opened
/// with `system`, and its canonical prior context reads. Hands back the reopened
/// recorder, that prior and the system as noted; a refusal lets the recorder go.
async fn resume(
    history: &Arc<LlmHistory>,
    joining: i64,
    agent: &str,
    runtime: &str,
    chat: &Chat,
    system: &str,
) -> Result<(RunRecorder, Vec<Value>, Option<String>), LaneError> {
    // The effective context in force NOW (endpoint + header fingerprint); the
    // resume guard compares it against the run's first sealed call.
    let wire = chat.wire();
    let run = history
        .resume_conversation(joining, agent, runtime, Some(&chat.provider), Some(&chat.model), Some(&wire))
        .await
        .map_err(|err| match err {
            ResumeError::Gone => refused("gone", format!("run {joining} is not an open conversation of this agent")),
            ResumeError::Closed => refused("closed", "that conversation is closed: start a New one"),
            ResumeError::ContextChanged => refused(
                "context-changed",
                "the provider or model in force changed since that conversation opened: start a New one",
            ),
            // The same wire code the replay refusal family answers with: the
            // no-fork contract is one shape whether the door refuses or the
            // replay read does.
            ResumeError::NoCanonical => refused(
                "unresolved-tail",
                "that conversation has activity it cannot prove canonically: start a New one",
            ),
        })?;
    // The system prompt in force: the dialogue's own first word. A changed one
    // changes every answer that follows - a New run, not a mid-dialogue swap.
    let noted = history.system_note_of(joining).await;
    if noted.as_deref().is_some_and(|noted| !opened_with(noted, system)) {
        run.release();
        return Err(refused(
            "context-changed",
            "the agent's system prompt changed since that conversation opened: start a New one",
        ));
    }
    // The CANONICAL prior context (SPEC llm.history): sealed structure only.
    // Every ambiguous shape (an unresolved activity mark, a pre-protocol or
    // pruned body, the migration's legacy mark) refuses here.
    let mut prior = match history.replay_state(joining).await {
        ReplayState::Fresh => Vec::new(),
        ReplayState::Continuable { prior } => prior,
        ReplayState::Refused(why) => {
            run.release();
            return Err(refused("unresolved-tail", why));
        }
    };
    // A dialogue another model answered last (the reader moved it, see [retarget])
    // replays without its signed thinking: a signature is the model's that wrote it and
    // a wire refuses one it cannot verify. The texts, the reasoning and the calls stay;
    // the next sealed round carries the log so, and the check finds nothing to strip.
    if history.last_call_model(joining).await.is_some_and(|(provider, model)| provider != chat.provider || model != chat.model) {
        for entry in &mut prior {
            if let Some(entry) = entry.as_object_mut() {
                entry.remove("signed");
            }
        }
    }
    Ok((run, prior, noted))
}

/// Make the closed conversation `run_id` the scope's open one again (a Data
/// connection's History, Continue): the one open now, if another, closes; `run_id`
/// reopens and must pass the continuation guards, budget aside (a dialogue past it can
/// still be compacted) - moved to the chat's provider and model first when `retarget`
/// (see [retarget]). A refusal puts both back as they were. The caller holds the
/// scope's seat, so no send is in flight on either.
pub async fn reopen(
    history: &Arc<LlmHistory>,
    run_id: i64,
    agent: &str,
    runtime: &str,
    chat: &Chat,
    system: &str,
    retarget: bool,
) -> Result<(), LaneError> {
    let current = history.continuable_open(agent).await;
    if current == Some(run_id) {
        if retarget {
            self::retarget(history, run_id, agent, chat).await?;
        }
        return Ok(());
    }
    if let Some(current) = current {
        history.close_run(current).await.map_err(LaneError::Unavailable)?;
    }
    let put_back = |why: LaneError| async move {
        if let Some(current) = current {
            let _ = history.reopen_run(current, agent).await;
        }
        why
    };
    match history.reopen_run(run_id, agent).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(put_back(refused(
                "gone",
                format!("run {run_id} is not a closed conversation of this agent here that can go on (one that failed, or left a round unresolved, cannot)"),
            ))
            .await)
        }
        Err(why) => return Err(put_back(LaneError::Unavailable(why)).await),
    }
    let before = if retarget {
        match history.retarget_conversation(run_id, agent, Some(&chat.provider), Some(&chat.model)).await {
            Ok(before) => before,
            Err(why) => {
                let _ = history.close_run(run_id).await;
                return Err(put_back(LaneError::Unavailable(why)).await);
            }
        }
    } else {
        None
    };
    match resume(history, run_id, agent, runtime, chat, system).await {
        Ok((run, _, _)) => {
            run.release();
            Ok(())
        }
        Err(why) => {
            if let Some((provider, model)) = before {
                let _ = history.retarget_conversation(run_id, agent, provider.as_deref(), model.as_deref()).await;
            }
            let _ = history.close_run(run_id).await;
            Err(put_back(why).await)
        }
    }
}

/// What a compaction works from (see [fold]).
pub struct Fold {
    /// The new conversation, open in the same scope: the compaction is its opening round.
    pub lane: Lane,
    /// The closed conversation's canonical prior context, replayed for the ask.
    pub prior: Vec<Value>,
    /// The system the closed conversation opened with, its snapshot included: the new
    /// one opens with it too, so its continuations pass the same guard.
    pub system: String,
    /// The closed conversation.
    pub from: i64,
}

/// Compact the conversation `joining` (SPEC §llm.agents Compaction). It must pass the
/// continuation guards, budget aside - a dialogue past its budget is what compaction is
/// for - and hold an answered turn to condense. It closes, and a new conversation opens
/// in the same scope, its source naming the old one (`compact:<id>`). The caller asks
/// [run_compaction](crate::agent::run_compaction) as the new run's opening round, and on
/// any failure calls [undo_fold].
pub async fn fold(
    history: &Arc<LlmHistory>,
    joining: i64,
    agent: &str,
    runtime: &str,
    chat: &Chat,
    system: &str,
) -> Result<Fold, LaneError> {
    let (run, prior, noted) = resume(history, joining, agent, runtime, chat, system).await?;
    run.release();
    if prior.is_empty() {
        return Err(refused("empty", "nothing to compact yet: the conversation has no answered turn"));
    }
    match history.close_run(joining).await {
        Ok(Some(())) => {}
        Ok(None) => return Err(refused("gone", format!("run {joining} is not an open conversation of this agent"))),
        Err(why) => return Err(LaneError::Unavailable(why)),
    }
    let scope = RunContext { source: Some(format!("compact:{joining}")), ..crate::history::context() };
    match within(scope, open(history, agent, runtime, chat)).await {
        Ok(lane) => Ok(Fold { lane, prior, system: noted.unwrap_or_else(|| system.to_string()), from: joining }),
        Err(why) => {
            let _ = history.reopen_run(joining, agent).await;
            Err(why)
        }
    }
}

/// Undo a fold whose checkpoint did not land: the new run files failed - never to
/// continue or reopen - and the conversation it would have replaced reopens. Ok(false)
/// when it could not (another opened meanwhile): the reader finds it in History.
pub async fn undo_fold(history: &Arc<LlmHistory>, folded: i64, from: i64, agent: &str) -> Result<bool, String> {
    history.fail_conversation(folded).await?;
    history.reopen_run(from, agent).await
}

/// Whether a dialogue's noted first word is `system`, alone or closed by the
/// snapshot it opened with ([SNAPSHOT_HEADING]).
fn opened_with(noted: &str, system: &str) -> bool {
    noted == system
        || noted
            .strip_prefix(system)
            .and_then(|rest| rest.strip_prefix("\n\n"))
            .is_some_and(|rest| rest.starts_with(SNAPSHOT_HEADING))
}

/// A NEW conversation: at most one CONTINUABLE open run per agent, reserved open
/// before any provider request.
async fn open(history: &Arc<LlmHistory>, agent: &str, runtime: &str, chat: &Chat) -> Result<Lane, LaneError> {
    // The continuable query, not the open listing: rows the migration marked
    // ambiguous stay visible for audit but never block a fresh conversation -
    // a stale witness cannot wedge the agent.
    if history.continuable_open(agent).await.is_some() {
        return Err(refused("open-run-exists", OPEN_RUN_EXISTS));
    }
    let run = history.open_conversation(agent, runtime, Some(&chat.provider), Some(&chat.model)).await;
    let Some(run_id) = run.id() else {
        // The write-time one-open rule can refuse even past the check above
        // (two handles, two processes): it keeps its own code.
        if run.refusal().is_some_and(|why| why.contains("open-run-exists")) {
            return Err(refused("open-run-exists", OPEN_RUN_EXISTS));
        }
        return Err(LaneError::Unavailable(
            "the llm history could not reserve a run row; the test cannot stream as a conversation".into(),
        ));
    };
    Ok(Lane { run, run_id, continued: false, prior: Vec::new(), system: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chat() -> Chat {
        let providers = crate::provider::parse_all(Some(&json!({
            "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] }
        })))
        .expect("providers");
        Chat::new(&providers["local"], None, 256).expect("chat")
    }

    async fn store() -> (Arc<LlmHistory>, std::path::PathBuf) {
        let dir = swiss_core::paths::test_home().join(format!("llm-lane-{}", swiss_core::util::random_hex(8)));
        (LlmHistory::open(&dir).await.expect("open"), dir)
    }

    fn code(joined: Result<Lane, LaneError>) -> &'static str {
        match joined {
            Err(LaneError::Refused { code, .. }) => code,
            Err(other) => panic!("expected a refusal, got {other:?}"),
            Ok(lane) => panic!("expected a refusal, joined run {}", lane.run_id),
        }
    }

    #[tokio::test]
    async fn a_new_conversation_opens_once_and_continues_by_its_id() {
        let (history, dir) = store().await;
        let chat = chat();
        let lane = join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("a new conversation");
        assert!(!lane.continued && lane.prior.is_empty());
        let run_id = lane.run_id;
        // One continuable open run per agent: a second New refuses beside it.
        assert_eq!(code(join(&history, None, "alpha", "builtin", &chat, "be brief").await), "open-run-exists");
        // A reserved run that never asked continues as its own first send.
        let again = join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief").await.expect("continued");
        assert_eq!((again.run_id, again.continued, again.prior.len()), (run_id, true, 0));
        // Another agent's run reads as absent, never as foreign detail.
        assert_eq!(code(join(&history, Some(run_id), "beta", "builtin", &chat, "be brief").await), "gone");
        assert_eq!(code(join(&history, Some(987_654), "alpha", "builtin", &chat, "be brief").await), "gone");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_closed_conversation_refuses_its_next_send() {
        let (history, dir) = store().await;
        let chat = chat();
        let run_id = join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("opened").run_id;
        history.close_run(run_id).await.expect("closed").expect("the run exists");
        assert_eq!(code(join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief").await), "closed");
        // Closing frees the agent for a New conversation.
        join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("a New one opens");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The one-open rule keys on the scope: a Test (no task) and each Data connection
    /// keep their own open conversation of the same agent, and a run continues only in
    /// the scope it opened in - another's id reads as gone.
    #[tokio::test]
    async fn each_scope_keeps_its_own_open_conversation() {
        use crate::history::{within, RunContext};
        let (history, dir) = store().await;
        let chat = chat();
        let scope = |conn: &str| RunContext { task: Some("data.agent".into()), subject: Some(conn.into()), ..RunContext::default() };
        let test = join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("the Test's").run_id;
        let pg = within(scope("pg"), join(&history, None, "alpha", "builtin", &chat, "be brief")).await.expect("pg's").run_id;
        let my = within(scope("my"), join(&history, None, "alpha", "builtin", &chat, "be brief")).await.expect("my's").run_id;
        assert!(test != pg && pg != my);
        assert_eq!(code(within(scope("pg"), join(&history, None, "alpha", "builtin", &chat, "be brief")).await), "open-run-exists");
        assert_eq!(within(scope("pg"), history.continuable_open("alpha")).await, Some(pg));
        assert_eq!(history.continuable_open("alpha").await, Some(test));
        let again = within(scope("pg"), join(&history, Some(pg), "alpha", "builtin", &chat, "be brief")).await.expect("continued");
        assert_eq!(again.run_id, pg);
        drop(again);
        assert_eq!(code(within(scope("my"), join(&history, Some(pg), "alpha", "builtin", &chat, "be brief")).await), "gone");
        assert_eq!(code(join(&history, Some(pg), "alpha", "builtin", &chat, "be brief").await), "gone");
        assert_eq!(code(within(scope("pg"), join(&history, Some(test), "alpha", "builtin", &chat, "be brief")).await), "gone");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A dialogue that opened with the agent's prompt closed by its snapshot continues
    /// under that prompt and hands the noted system back to replay; one whose prompt
    /// changed, or that carries anything but a snapshot after it, refuses.
    #[tokio::test]
    async fn a_continuation_replays_the_snapshot_its_dialogue_opened_with() {
        let (history, dir) = store().await;
        let chat = chat();
        let opened = format!("be brief\n\n{SNAPSHOT_HEADING}\n- Working directory: C:/work");
        let mut lane = join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("opened");
        let run_id = lane.run_id;
        assert_eq!(lane.system, None, "a new conversation opens with the consumer's own");
        lane.run.note("system", &opened);
        seal_round(&history, &chat, &mut lane.run, &json!({ "messages": [] }), &json!([{ "role": "user", "text": "go" }])).await;
        lane.run.finish(true);
        history.flush().await;
        let again = join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief").await.expect("continued");
        assert_eq!(again.system.as_deref(), Some(opened.as_str()), "the snapshot replays as noted");
        assert_eq!(code(join(&history, Some(run_id), "alpha", "builtin", &chat, "be terse").await), "context-changed");
        assert_eq!(code(join(&history, Some(run_id), "alpha", "builtin", &chat, "be").await), "context-changed");
        assert!(!opened_with("be brief\n\nand cite paths", "be brief"), "only a snapshot may follow the prompt");
        assert!(opened_with("be brief", "be brief"));
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Seal one answered round of the run, as drive_loop files it: the full ask
    /// (`asked`) and the replay log a continuation rebuilds from (`replay`).
    async fn seal_round(history: &Arc<LlmHistory>, chat: &Chat, run: &mut RunRecorder, asked: &Value, replay: &Value) {
        run.begin_round().await.expect("the round's activity mark commits");
        let wire = chat.wire();
        history.record_chat_in_run(
            run.id().expect("reserved"),
            crate::history::ChatCall {
                provider: &chat.provider,
                model: &chat.model,
                agent: Some("alpha"),
                system: Some("be brief"),
                prompt: "ask",
                turn: Some(0),
                wire: Some(&wire),
                request: Some(asked),
                replay: Some(replay),
                attempt: run.attempt(),
                outcome: crate::history::ChatOutcome::Ok {
                    texts: &["ok".to_string()],
                    thinking: &[],
                    tool_calls: &[],
                    cut: false,
                    tokens: crate::provider::Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                    ms: 5,
                    ttft_ms: None,
                    request_id: "swiss-test-lane-budget",
                    stop_reason: None,
                    contents: &json!({}),
                },
            },
        );
        history.flush().await;
    }

    /// The byte budget bounds ONE request: a run whose sealed calls together carry
    /// more than the cap (each re-sends the dialogue before it) continues while its
    /// replay fits, and refuses `over-budget` once the replay itself reaches the cap.
    #[tokio::test]
    async fn the_byte_budget_weighs_the_replay_not_the_record() {
        let (history, dir) = store().await;
        let chat = chat().with_context_kib(64);
        let mut lane = join(&history, None, "alpha", "builtin", &chat, "be brief").await.expect("opened");
        let run_id = lane.run_id;
        // Rounds of 40 KiB asks: each fits the 64 KiB cap, the two together do not.
        let heavy = json!({ "messages": [{ "role": "user", "content": "x".repeat(40 * 1024) }] });
        let small = json!([{ "role": "user", "text": "a short ask" }]);
        seal_round(&history, &chat, &mut lane.run, &heavy, &small).await;
        let mut lane = join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief").await.expect("continued");
        seal_round(&history, &chat, &mut lane.run, &heavy, &small).await;
        let (_, record, _) = history.conversation_size(run_id).await.expect("size");
        assert!(record >= chat.context_cap() as i64, "the record passed the cap: {record}");
        let mut lane = join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief")
            .await
            .expect("a record past the cap still continues while its replay fits");
        assert!(serde_json::to_vec(&lane.prior).expect("prior").len() < chat.context_cap());
        // A replay at the cap leaves the next request no room: the send refuses.
        let long = json!([{ "role": "user", "text": "y".repeat(64 * 1024) }]);
        seal_round(&history, &chat, &mut lane.run, &heavy, &long).await;
        assert_eq!(code(join(&history, Some(run_id), "alpha", "builtin", &chat, "be brief").await), "over-budget");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn data(conn: &str) -> crate::history::RunContext {
        crate::history::RunContext { task: Some("data.agent".into()), subject: Some(conn.into()), ..Default::default() }
    }

    /// One answered conversation in `conn`'s scope, its system noted, closed when `close`.
    async fn answered(history: &Arc<LlmHistory>, chat: &Chat, conn: &str, system: &str, close: bool) -> i64 {
        let mut lane = within(data(conn), join(history, None, "alpha", "builtin", chat, "be brief")).await.expect("opened");
        lane.run.note("system", system);
        seal_round(history, chat, &mut lane.run, &json!({ "messages": [] }), &json!([{ "role": "user", "text": "count the posts" }])).await;
        lane.run.finish(true);
        history.flush().await;
        if close {
            history.close_run(lane.run_id).await.expect("closed").expect("the run exists");
        }
        lane.run_id
    }

    /// History's Continue: a closed conversation reopens in its own scope, the open one
    /// closing; a run that cannot go on - another scope's, a failed one, one whose prompt
    /// changed - refuses and leaves both as they were.
    #[tokio::test]
    async fn a_closed_conversation_reopens_in_its_scope_and_a_refusal_puts_both_back() {
        let (history, dir) = store().await;
        let chat = chat();
        let first = answered(&history, &chat, "pg", "be brief", true).await;
        let second = answered(&history, &chat, "pg", "be brief", false).await;
        let open = || within(data("pg"), history.continuable_open("alpha"));
        assert_eq!(open().await, Some(second));
        within(data("pg"), reopen(&history, first, "alpha", "builtin", &chat, "be brief", false)).await.expect("reopened");
        assert_eq!(open().await, Some(first), "the picked one is open");
        within(data("pg"), join(&history, Some(first), "alpha", "builtin", &chat, "be brief")).await.expect("and continues");
        within(data("pg"), reopen(&history, first, "alpha", "builtin", &chat, "be brief", false)).await.expect("the open one is a no-op");
        within(data("pg"), reopen(&history, second, "alpha", "builtin", &chat, "be brief", false)).await.expect("swapped back");
        assert_eq!(open().await, Some(second));
        let refused = |run_id, system: &'static str, conn: &'static str| {
            let history = history.clone();
            let chat = chat.clone();
            async move {
                match within(data(conn), reopen(&history, run_id, "alpha", "builtin", &chat, system, false)).await {
                    Err(LaneError::Refused { code, .. }) => code,
                    other => panic!("expected a refusal, got {other:?}"),
                }
            }
        };
        // Another connection's conversation reads as gone here.
        let elsewhere = answered(&history, &chat, "my", "be brief", true).await;
        assert_eq!(refused(elsewhere, "be brief", "pg").await, "gone");
        assert_eq!(open().await, Some(second), "a refusal leaves the open one open");
        // The agent's prompt changed since it opened: it cannot go on, and closes again.
        assert_eq!(refused(first, "be terse", "pg").await, "context-changed");
        assert_eq!(open().await, Some(second));
        // A failed conversation never reopens.
        history.fail_conversation(first).await.expect("failed");
        assert_eq!(refused(first, "be brief", "pg").await, "gone");
        assert_eq!(open().await, Some(second));
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The reader moves a conversation to another model (the Data agent's menu): its row
    /// names the pick and the send continues under it - the wire check waiting for a call
    /// to it - replaying the other model's rounds without their signed thinking; one never
    /// moved still refuses a provider it did not open on. A reopen moves the same way, and
    /// one refused puts the row back as it was.
    #[tokio::test]
    async fn a_moved_conversation_continues_on_the_pick_without_another_models_signatures() {
        let (history, dir) = store().await;
        let providers = crate::provider::parse_all(Some(&json!({
            "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] },
            "other": { "style": "anthropic", "baseUrl": "http://127.0.0.1:9/anthropic", "models": [{ "id": "big" }] }
        })))
        .expect("providers");
        let first = Chat::new(&providers["local"], None, 256).expect("chat");
        let picked = Chat::new(&providers["other"], None, 256).expect("chat");
        let mut lane = within(data("pg"), join(&history, None, "alpha", "builtin", &first, "be brief")).await.expect("opened");
        let run_id = lane.run_id;
        lane.run.note("system", "be brief");
        let signed = r#"[{"type":"thinking","thinking":"hm","signature":"sig"}]"#;
        let log = json!([{ "role": "user", "text": "go" }, { "role": "assistant", "text": "on it", "thinking": "hm", "signed": signed }]);
        seal_round(&history, &first, &mut lane.run, &json!({ "messages": [] }), &log).await;
        lane.run.finish(true);
        history.flush().await;
        async fn joined(history: &Arc<LlmHistory>, run_id: i64, chat: &Chat) -> Result<Lane, LaneError> {
            within(data("pg"), join(history, Some(run_id), "alpha", "builtin", chat, "be brief")).await
        }
        async fn reopened(history: &Arc<LlmHistory>, run_id: i64, chat: &Chat, system: &str, retarget: bool) -> Result<(), LaneError> {
            within(data("pg"), reopen(history, run_id, "alpha", "builtin", chat, system, retarget)).await
        }
        assert_eq!(code(joined(&history, run_id, &picked).await), "context-changed", "never moved, another provider is another context");
        within(data("pg"), retarget(&history, run_id, "alpha", &picked)).await.expect("moved");
        let mut lane = joined(&history, run_id, &picked).await.expect("continues on the pick");
        let round = lane.prior.iter().find(|entry| entry["role"] == "assistant").cloned().expect("the round");
        assert!(round.get("signed").is_none(), "another model's signature never replays: {round}");
        assert_eq!((round["text"].as_str(), round["thinking"].as_str()), (Some("on it"), Some("hm")));
        let prior = Value::Array(lane.prior.clone());
        seal_round(&history, &picked, &mut lane.run, &json!({ "messages": [] }), &prior).await;
        lane.run.finish(true);
        history.flush().await;
        joined(&history, run_id, &picked).await.expect("the pick's own call now vouches for its wire");
        assert_eq!(code(joined(&history, run_id, &first).await), "context-changed", "the row names the pick");
        // A refused reopen puts the row back; a reopen that moves goes on.
        history.close_run(run_id).await.expect("closed").expect("the run exists");
        match reopened(&history, run_id, &first, "be terse", true).await {
            Err(LaneError::Refused { code, .. }) => assert_eq!(code, "context-changed"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        reopened(&history, run_id, &picked, "be brief", false).await.expect("the row still names the pick");
        reopened(&history, run_id, &first, "be brief", true).await.expect("the open one moves back");
        joined(&history, run_id, &first).await.expect("and continues there");
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A compaction folds the open conversation: it closes, a new one opens in the same
    /// scope naming it as its source and carrying its prior and noted system; undone, the
    /// new run files failed and the old one is open again. A conversation with no answered
    /// turn has nothing to fold.
    #[tokio::test]
    async fn a_fold_opens_the_next_conversation_and_its_undo_reopens_the_old_one() {
        let (history, dir) = store().await;
        let chat = chat();
        let opened = format!("be brief\n\n{SNAPSHOT_HEADING}\n- Connection: pg");
        let from = answered(&history, &chat, "pg", &opened, false).await;
        let folded = within(data("pg"), fold(&history, from, "alpha", "builtin", &chat, "be brief")).await.expect("folded");
        let Fold { lane, prior, system, from: condensed } = folded;
        assert_eq!((condensed, system.as_str()), (from, opened.as_str()), "the new run opens with the old one's system");
        assert_eq!(prior.first().and_then(|turn| turn["text"].as_str()), Some("count the posts"), "{prior:?}");
        assert!(lane.run_id != from && !lane.continued);
        let next = lane.run_id;
        drop(lane);
        assert_eq!(within(data("pg"), history.continuable_open("alpha")).await, Some(next));
        let listed = within(data("pg"), history.read_conversations("alpha", 0, 20)).await.expect("listed");
        let row = |id: i64| listed["runs"].as_array().and_then(|runs| runs.iter().find(|run| run["id"] == id)).cloned().unwrap_or_default();
        assert_eq!(row(next)["source"].as_str(), Some(format!("compact:{from}").as_str()), "{listed}");
        assert!(row(from)["atEnd"].is_string(), "the old one closed: {listed}");
        // Undone: the new run files failed, the old one is open again.
        assert_eq!(within(data("pg"), undo_fold(&history, next, from, "alpha")).await, Ok(true));
        assert_eq!(within(data("pg"), history.continuable_open("alpha")).await, Some(from));
        let listed = within(data("pg"), history.read_conversations("alpha", 0, 20)).await.expect("listed");
        let failed = listed["runs"].as_array().and_then(|runs| runs.iter().find(|run| run["id"] == next)).cloned().unwrap_or_default();
        assert_eq!(failed["ok"], json!(false), "{listed}");
        // Another connection's list is its own.
        let other = within(data("my"), history.read_conversations("alpha", 0, 20)).await.expect("listed");
        assert_eq!(other["runs"].as_array().map(Vec::len), Some(0), "{other}");
        // Nothing answered yet: nothing to fold, and the conversation stays open.
        let fresh = within(data("my"), join(&history, None, "alpha", "builtin", &chat, "be brief")).await.expect("opened").run_id;
        match within(data("my"), fold(&history, fresh, "alpha", "builtin", &chat, "be brief")).await {
            Err(LaneError::Refused { code, .. }) => assert_eq!(code, "empty"),
            Err(other) => panic!("expected empty, got {other:?}"),
            Ok(fold) => panic!("folded run {}", fold.lane.run_id),
        }
        assert_eq!(within(data("my"), history.continuable_open("alpha")).await, Some(fresh));
        history.close().await.expect("close");
        let _ = std::fs::remove_dir_all(dir);
    }
}
