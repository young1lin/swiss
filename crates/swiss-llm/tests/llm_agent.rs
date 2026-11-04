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

//! The agent loop driven directly (no routes, no panel): the cancellation
//! contracts that only the loop itself can witness - a pending tool never
//! blocks a cancel, an already-completed tool's facts survive it, and a
//! cancelled round keeps the conversation open - and where the words a reader
//! sends mid-run go in (and where they do not). The wire is a local one-shot
//! SSE server; every test owns its scratch home; every wait is bounded.

use serde_json::{json, Value};
use swiss_llm::agent::{intentioned, run_loop, tools_bytes, LoopEvent, RoundUsage, ToolAnswer, ToolSpec, Toolbox};
use swiss_llm::history::{CancelFlag, LlmHistory, ReplayState, Steer, SteerRefused};
use swiss_llm::telemetry::Session;

fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

/// A one-shot SSE wire answering each request with the next canned frame
/// group (the OpenAI chat stream shape), content-length framed like the
/// route tests' wires.
async fn sse_wire(frames: Vec<String>) -> String {
    sse_wire_kept(frames).await.0
}

/// Every request body a wire answered, in order.
type Kept = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// [sse_wire], keeping each request body it answered.
async fn sse_wire_kept(frames: Vec<String>) -> (String, Kept) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let kept = Kept::default();
    let bodies = kept.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for frames in frames {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    bodies.lock().expect("kept").push(String::from_utf8_lossy(&buf[head_end + 4..]).into_owned());
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        frames.len(),
                        frames
                    );
                    socket.write_all(answer.as_bytes()).await.expect("write");
                    break;
                }
            }
        }
    });
    (url, kept)
}

/// The controlled toolbox: `quick` answers at once; `stall` waits on a gate
/// that only the test opens (and the cancel must not need).
struct Gated {
    stall_entered: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl Toolbox for Gated {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "quick".into(),
                description: "answers at once".into(),
                schema: json!({ "type": "object", "properties": {} }),
            },
            ToolSpec {
                name: "stall".into(),
                description: "waits for a gate the test holds".into(),
                schema: json!({ "type": "object", "properties": {} }),
            },
        ]
    }

    async fn call(&mut self, name: &str, _args: &Value) -> ToolAnswer {
        match name {
            "quick" => ToolAnswer { text: "the answer is 42".into(), ok: true },
            // Held: a real tool mid-execution. ENTRY is announced before the
            // pend, so the test cancels only once execution provably began; the
            // cancel must abort the wait and the result it leaves behind must
            // say execution is UNKNOWN.
            _ => {
                self.stall_entered.notify_one();
                std::future::pending::<ToolAnswer>().await
            }
        }
    }
}

/// R3: a cancel during a pending tool aborts the wait within the bound, keeps
/// the completed tool's facts, records the pending one as UNKNOWN (never a
/// success) and the one never reached as NOT RUN, and leaves the conversation
/// open and continuable for a later send. A loop that waited the tool out
/// forever fails the bounded drive in seconds.
#[tokio::test]
async fn cancel_during_a_pending_tool_records_unknown_and_files_prior_results() {
    // Wire grammar proven by the R5 fixture (upstream-isolation order): each
    // tool call gets its OWN delta frames - id+name together in the call's
    // first frame, the arguments in the following frame - so a stall-entry
    // timeout can only witness pending-tool cancellation, never the already
    // witnessed one-delta-many-fields parsing defect.
    let ask = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Two checks.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_quick\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_stall\",\"type\":\"function\",\"function\":{\"name\":\"stall\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":2,\"id\":\"call_after\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":2,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let answer = concat!(
        "data: {\"id\":\"c2\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"done.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c2\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let base = sse_wire(vec![ask, answer]).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let entry = row.provider("local").expect("provider");
    let dir = home("llm-agent-pending-tool");
    let history = LlmHistory::open(&dir).await.expect("open");
    // Recording, as the route's chat does: the interrupted round settles only over its filed call.
    let chat = swiss_llm::provider::Chat::new(entry, Some("tiny"), 64)
        .expect("chat")
        .recording(Some(history.clone()), Some("alpha"));
    let mut run = history.open_conversation("alpha", "builtin", Some("local"), Some("tiny")).await;
    let run_id = run.id().expect("reserved");
    // CancelFlag::new already returns the shared Arc handle.
    let cancel = CancelFlag::new();
    run.set_cancel(cancel.clone());

    // The drive owns the toolbox mutably, so the entry gate is shared through
    // an Arc the test keeps - never borrowed back out of the toolbox.
    let stall_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut toolbox = Gated { stall_entered: stall_entered.clone() };
    let session = Session::disabled();
    let mut events: Vec<String> = Vec::new();
    let mut on_event = |event: LoopEvent| {
        if let LoopEvent::Text(piece) = event {
            events.push(piece);
        }
    };

    let cancel_for_drive = cancel.clone();
    let drive = run_loop(
        &chat,
        "You are a test.",
        "check both",
        &mut toolbox,
        4,
        &session,
        &mut run,
        &mut on_event,
    );
    // Bounded: on today's behavior the loop waits the stalled tool out forever
    // - the guard witnesses the red in seconds instead of hanging the suite.
    // Order by gate, not time: the cancel fires only once stall ANNOUNCES ITS
    // ENTRY (quick finishing earlier must not fire it - with awaited durable
    // writes, that could cancel before execution provably began), then the
    // drive must return.
    let driven = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::pin!(drive);
        loop {
            tokio::select! {
                _ = stall_entered.notified() => {
                    cancel_for_drive.request();
                }
                outcome = &mut drive => return outcome,
            }
        }
    })
    .await;
    // Phase proof, not inference: ONLY the stall_entered arm requests the
    // flag, so a requested cancel positively proves the stall tool actually
    // began executing - even when the drive times out below.
    assert!(
        cancel.requested(),
        "the stall-entry gate must have fired before this cancellation oracle"
    );
    let outcome = driven
        .expect("the cancel must abort the stalled tool within the bound - today it waits forever")
        .expect("the loop returns Ok even when cancelled");
    assert!(outcome.cancelled, "the outcome names the cancel, not a success");
    let _ = &mut on_event;
    let _ = events.len();
    // Completed facts survive; the pending tool's result says execution is
    // UNKNOWN, never success; the conversation stays open for a later send.
    let turns = history.read_turns(run_id).await.expect("read").expect("run");
    let joined = turns.to_string();
    assert!(joined.contains("the answer is 42"), "the completed tool's facts survive the cancel: {joined}");
    assert!(
        joined.contains("call_stall") && joined.contains("nknown"),
        "the pending tool's result must say execution UNKNOWN, never success: {joined}"
    );
    assert!(
        joined.contains("call_after") && joined.contains("not run"),
        "the call the dispatch never reached is noted as not run: {joined}"
    );
    let open = history.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("open read");
    assert_eq!(open["runs"].as_array().map(Vec::len), Some(1), "a cancelled conversation stays open");
    // ...and CONTINUABLE: the interrupted round settled with one answer per call,
    // in call order, so the next send replays it instead of refusing the run as
    // unresolved (the Stop-then-send the Data agent's reader expects).
    history.flush().await;
    match history.replay_state(run_id).await {
        ReplayState::Continuable { prior } => {
            let tools: Vec<(Value, Value)> = prior
                .iter()
                .filter(|message| message["role"] == "tool")
                .map(|message| (message["id"].clone(), message["text"].clone()))
                .collect();
            assert_eq!(
                tools,
                vec![
                    (json!("call_quick"), json!("the answer is 42")),
                    (json!("call_stall"), json!("execution unknown - cancelled before it could finish")),
                    (json!("call_after"), json!("not run - cancelled before it started")),
                ],
                "every call of the interrupted round is answered by its exact id: {prior:?}"
            );
        }
        ReplayState::Refused(why) => panic!("a Stop during a tool must leave the conversation continuable: {why}"),
        ReplayState::Fresh => panic!("the interrupted round must not disappear"),
    }
    assert!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("local"), Some("tiny"), None)
            .await
            .is_ok(),
        "the next send reopens the interrupted conversation"
    );
    history.close().await.expect("close");
}
/// A wire that answers the first request with `first` and then HOLDS the second one:
/// the request is read in full, `asked` is notified, and no byte of an answer ever comes
/// (the socket stays open until the test ends) - a model that thinks and never speaks.
async fn wire_holding_the_second(first: String, asked: std::sync::Arc<tokio::sync::Notify>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut held = Vec::new();
        for round in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    break;
                }
            }
            if round == 0 {
                let answer = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    first.len(),
                    first
                );
                socket.write_all(answer.as_bytes()).await.expect("write");
            } else {
                asked.notify_one();
                held.push(socket);
            }
        }
        std::future::pending::<()>().await;
    });
    url
}

/// A Stop while the model is still thinking over a settled tool round (the Data agent's
/// run_sql answered, the next round asked, nothing streamed yet) files the wait as a
/// cancelled call that settles the round: the conversation stays open AND continuable,
/// and the next send reopens it instead of refusing "activity it cannot prove canonically".
#[tokio::test]
async fn a_stop_during_the_model_wait_after_a_tool_round_leaves_the_conversation_continuable() {
    let ask = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"One check.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_quick\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let second_asked = std::sync::Arc::new(tokio::sync::Notify::new());
    let base = wire_holding_the_second(ask, second_asked.clone()).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let dir = home("llm-agent-stop-in-wait");
    let history = LlmHistory::open(&dir).await.expect("open");
    // Recording, as the route's chat does.
    let chat = swiss_llm::provider::Chat::new(row.provider("local").expect("provider"), Some("tiny"), 64)
        .expect("chat")
        .recording(Some(history.clone()), Some("alpha"));
    let mut run = history.open_conversation("alpha", "builtin", Some("local"), Some("tiny")).await;
    let run_id = run.id().expect("reserved");
    let cancel = CancelFlag::new();
    run.set_cancel(cancel.clone());
    let mut toolbox = Gated { stall_entered: std::sync::Arc::new(tokio::sync::Notify::new()) };
    let session = Session::disabled();
    let mut on_event = |_event: LoopEvent| {};
    let stop = cancel.clone();
    let drive = run_loop(&chat, "You are a test.", "check it", &mut toolbox, 4, &session, &mut run, &mut on_event);
    let driven = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::pin!(drive);
        loop {
            tokio::select! {
                _ = second_asked.notified() => stop.request(),
                outcome = &mut drive => return outcome,
            }
        }
    })
    .await;
    assert!(cancel.requested(), "the Stop fired only once the second round was asked");
    let outcome = driven.expect("the Stop ends the wait within the bound").expect("a cancelled loop returns Ok");
    assert!(outcome.cancelled, "the outcome names the cancel");
    history.flush().await;
    match history.replay_state(run_id).await {
        ReplayState::Continuable { prior } => {
            assert!(
                prior.iter().any(|message| message["role"] == "tool" && message["id"] == "call_quick"),
                "the settled tool round replays: {prior:?}"
            );
        }
        ReplayState::Refused(why) => panic!("a Stop during the model wait must leave the conversation continuable: {why}"),
        ReplayState::Fresh => panic!("the conversation's rounds must not disappear"),
    }
    assert!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("local"), Some("tiny"), None)
            .await
            .is_ok(),
        "the next send reopens the stopped conversation"
    );
    history.close().await.expect("close");
}

/// The same Stop when the cancelled round can NOT file - every history write refused at
/// that moment, so the round's mark stays set with no call to clear it (the shape a
/// conversation stranded by an interrupt used to keep for good). The drive is gone, so
/// the next send's door releases the round: the settled tool round still replays with
/// its real result, and the reopened conversation may fly its next round.
#[tokio::test]
async fn a_stop_whose_cancelled_round_cannot_file_still_leaves_the_conversation_continuable() {
    let ask = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"One check.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_quick\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let second_asked = std::sync::Arc::new(tokio::sync::Notify::new());
    let base = wire_holding_the_second(ask, second_asked.clone()).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let dir = home("llm-agent-stop-unfiled");
    let history = LlmHistory::open(&dir).await.expect("open");
    let chat = swiss_llm::provider::Chat::new(row.provider("local").expect("provider"), Some("tiny"), 64)
        .expect("chat")
        .recording(Some(history.clone()), Some("alpha"));
    let mut run = history.open_conversation("alpha", "builtin", Some("local"), Some("tiny")).await;
    let run_id = run.id().expect("reserved");
    let cancel = CancelFlag::new();
    run.set_cancel(cancel.clone());
    let mut toolbox = Gated { stall_entered: std::sync::Arc::new(tokio::sync::Notify::new()) };
    let session = Session::disabled();
    let mut on_event = |_event: LoopEvent| {};
    let stop = cancel.clone();
    let wedge = history.clone();
    let drive = run_loop(&chat, "You are a test.", "check it", &mut toolbox, 4, &session, &mut run, &mut on_event);
    let driven = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::pin!(drive);
        loop {
            tokio::select! {
                _ = second_asked.notified() => {
                    // Every write from here on refuses: the cancelled call and the
                    // run's finish cannot land, so the second round's mark stays set.
                    wedge.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(Default::default()));
                    stop.request();
                }
                outcome = &mut drive => return outcome,
            }
        }
    })
    .await;
    let outcome = driven.expect("the Stop ends the wait within the bound").expect("a cancelled loop returns Ok");
    assert!(outcome.cancelled, "the outcome names the cancel");
    drop(run);
    history.set_quota(swiss_host::history::quota::QuotaPolicy::default());
    history.flush().await;
    assert!(
        matches!(history.replay_state(run_id).await, ReplayState::Refused(_)),
        "before the door the round is still marked in flight"
    );
    let mut resumed = history
        .resume_conversation(run_id, "alpha", "builtin", Some("local"), Some("tiny"), None)
        .await
        .expect("the door releases the stranded round and the conversation reopens");
    match history.replay_state(run_id).await {
        ReplayState::Continuable { prior } => {
            assert!(
                prior.iter().any(|message| message["role"] == "tool"
                    && message["id"] == "call_quick"
                    && message["text"] == "the answer is 42"),
                "the settled tool round replays with its real result: {prior:?}"
            );
        }
        ReplayState::Refused(why) => panic!("a released round must leave the conversation continuable: {why}"),
        ReplayState::Fresh => panic!("the conversation's rounds must not disappear"),
    }
    assert!(resumed.begin_round().await.is_ok(), "the reopened conversation may fly its next round");
    history.close().await.expect("close");
}

/// The request budget is the agent's own (`maxContextKiB` on the chat): a round
/// over it refuses before anything flies, naming the budget, and the same ask
/// under the default budget goes out and answers.
#[tokio::test]
async fn a_round_over_the_chats_context_budget_refuses_before_it_flies() {
    let answer = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"read it.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    // One answer only: the refused round must never reach the wire.
    let base = sse_wire(vec![answer]).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let entry = row.provider("local").expect("provider");
    let session = Session::disabled();
    let prompt = "x".repeat(70 * 1024);
    let mut toolbox = Gated { stall_entered: std::sync::Arc::new(tokio::sync::Notify::new()) };
    let mut on_event = |_event: LoopEvent| {};

    let small = swiss_llm::provider::Chat::new(entry, Some("tiny"), 64).expect("chat").with_context_kib(64);
    assert_eq!(small.context_cap(), 64 * 1024);
    let mut run = small.run_recorder("builtin");
    let refused = run_loop(&small, "You are a test.", &prompt, &mut toolbox, 4, &session, &mut run, &mut on_event).await;
    let why = refused.expect_err("70 KiB of prompt is over a 64 KiB budget");
    assert!(why.contains("64 KiB budget") && why.contains("maxContextKiB"), "{why}");

    let roomy = swiss_llm::provider::Chat::new(entry, Some("tiny"), 64).expect("chat");
    let mut run = roomy.run_recorder("builtin");
    let driven = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_loop(&roomy, "You are a test.", &prompt, &mut toolbox, 4, &session, &mut run, &mut on_event),
    )
    .await
    .expect("bounded")
    .expect("the default budget takes it");
    assert_eq!(driven.summary, "read it.");
}

/// A toolbox whose one tool, while it runs, has the reader post words to the live
/// inbox: what a person types while the agent works.
struct Steering {
    steer: std::sync::Arc<Steer>,
}

#[async_trait::async_trait]
impl Toolbox for Steering {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "quick".into(),
            description: "answers at once".into(),
            schema: json!({ "type": "object", "properties": {} }),
        }]
    }

    async fn call(&mut self, _name: &str, _args: &Value) -> ToolAnswer {
        self.steer.post("q1", "also list the ports").expect("the inbox takes words mid-run");
        ToolAnswer { text: "the answer is 42".into(), ok: true }
    }
}

/// One closing answer round of `text`.
fn closing_round(id: &str, text: &str) -> String {
    let open = json!({ "id": id, "object": "chat.completion.chunk", "model": "tiny",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text }, "finish_reason": null }] });
    let stop = json!({ "id": id, "object": "chat.completion.chunk", "model": "tiny",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 } });
    format!("data: {open}\n\ndata: {stop}\n\ndata: [DONE]\n\n")
}

/// Words the reader sends while a tool runs go in at the next round boundary: the
/// next ask carries them as a user message after the tool's answer, the surface hears
/// them under the reader's id, the run files them as a user turn after the tool's,
/// and the inbox is left empty. Each round's usage reaches the surface too: its bytes
/// against the budget, the system prompt and tool definitions apart, its tokens.
#[tokio::test]
async fn words_sent_mid_run_go_in_after_the_tool_answers() {
    let ask = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"One check.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_quick\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let (base, kept) = sse_wire_kept(vec![ask, closing_round("c2", "done.")]).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let dir = home("llm-agent-steer");
    let history = LlmHistory::open(&dir).await.expect("open");
    // Recording, as the route's chat does: a tool round settles only over its filed call.
    let chat = swiss_llm::provider::Chat::new(row.provider("local").expect("provider"), Some("tiny"), 64)
        .expect("chat")
        .recording(Some(history.clone()), Some("alpha"));
    let mut run = history.open_conversation("alpha", "builtin", Some("local"), Some("tiny")).await;
    let run_id = run.id().expect("reserved");
    let steer = Steer::new();
    run.set_steer(steer.clone());
    let mut toolbox = Steering { steer: steer.clone() };
    let tools_weight = tools_bytes(&intentioned(toolbox.specs()));
    let session = Session::disabled();
    let mut steered: Vec<(String, String)> = Vec::new();
    let mut rounds: Vec<RoundUsage> = Vec::new();
    let mut tools_took: Vec<u64> = Vec::new();
    let mut on_event = |event: LoopEvent| match event {
        LoopEvent::Steered { id, text } => steered.push((id, text)),
        LoopEvent::Usage(usage) => rounds.push(usage),
        LoopEvent::ToolResult { ms, .. } => tools_took.push(ms),
        _ => {}
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_loop(&chat, "You are a test.", "check it", &mut toolbox, 4, &session, &mut run, &mut on_event),
    )
    .await
    .expect("bounded")
    .expect("the loop answers");
    assert_eq!((outcome.summary.as_str(), outcome.turns), ("done.", 2));
    assert_eq!(steered, vec![("q1".to_string(), "also list the ports".to_string())]);
    assert_eq!(rounds.len(), 2, "one usage a round: {rounds:?}");
    for round in &rounds {
        assert_eq!((round.cap, round.system, round.tools), (chat.context_cap(), "You are a test.".len(), tools_weight));
        assert!(round.tools > 0 && round.bytes > round.system + round.tools, "{round:?}");
    }
    assert_eq!((rounds[0].tokens.input, rounds[0].tokens.output), (9, 3));
    assert_eq!((rounds[1].tokens.input, rounds[1].tokens.output), (5, 1));
    assert!(rounds[1].bytes > rounds[0].bytes, "the second round carries the first's dialogue: {rounds:?}");
    // A streamed round says when its first output came, within the round's own time; the
    // tool's answer says how long the tool took.
    assert!(rounds.iter().all(|round| round.ttft_ms.is_some_and(|ttft| ttft <= round.ms)), "{rounds:?}");
    assert_eq!(tools_took.len(), 1, "one tool answered");

    let bodies = kept.lock().expect("kept").clone();
    assert_eq!(bodies.len(), 2, "two asks flew");
    assert!(!bodies[0].contains("also list the ports"), "the first ask flew before the words were sent");
    let second: Value = serde_json::from_str(&bodies[1]).expect("the second ask is JSON");
    let messages = second["messages"].as_array().expect("messages");
    let last = messages.last().expect("a last message");
    assert_eq!(last["role"], "user", "the words are the reader's: {last}");
    assert!(last.to_string().contains("also list the ports"), "{last}");
    let before = &messages[messages.len() - 2];
    assert_eq!(before["role"], "tool", "they follow the tool's answer: {}", bodies[1]);
    assert!(before.to_string().contains("the answer is 42"), "{before}");

    let turns = history.read_turns(run_id).await.expect("read").expect("run");
    let roles: Vec<(String, String)> = turns["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .map(|t| (t["role"].as_str().unwrap_or("").to_string(), t["preview"].as_str().unwrap_or("").to_string()))
        .collect();
    let tool_at = roles.iter().position(|(role, text)| role == "tool" && text.contains("the answer is 42"));
    let words_at = roles.iter().position(|(role, text)| role == "user" && text.contains("also list the ports"));
    assert!(
        matches!((tool_at, words_at), (Some(tool), Some(words)) if tool < words),
        "the words file as a user turn after the tool's: {roles:?}"
    );
    assert!(steer.close().is_empty(), "the loop took them: nothing is left to send");
    history.close().await.expect("close");
}

/// Words that arrive while the closing round flies are never taken: no round follows to
/// read them, so they stay in the inbox, and its close hands them back for the reader's
/// next send and refuses anything later.
#[tokio::test]
async fn words_the_closing_round_never_reached_stay_in_the_inbox() {
    let (base, kept) = sse_wire_kept(vec![closing_round("c1", "read it.")]).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let chat = swiss_llm::provider::Chat::new(row.provider("local").expect("provider"), Some("tiny"), 64).expect("chat");
    let mut run = chat.run_recorder("builtin");
    let steer = Steer::new();
    run.set_steer(steer.clone());
    steer.post("late", "and the ports").expect("posted");
    let mut toolbox = Steering { steer: steer.clone() };
    let session = Session::disabled();
    let mut heard = 0;
    let mut on_event = |event: LoopEvent| {
        if matches!(event, LoopEvent::Steered { .. }) {
            heard += 1;
        }
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_loop(&chat, "You are a test.", "read it", &mut toolbox, 4, &session, &mut run, &mut on_event),
    )
    .await
    .expect("bounded")
    .expect("the loop answers");
    assert_eq!(outcome.summary, "read it.");
    assert_eq!(heard, 0, "no boundary took them");
    assert!(!kept.lock().expect("kept")[0].contains("and the ports"), "the ask never carried them");
    assert_eq!(steer.close(), vec![("late".to_string(), "and the ports".to_string())]);
    assert_eq!(steer.post("later", "more"), Err(SteerRefused::Closed), "a closed inbox refuses");
}

/// The inbox itself: posts queue in order up to the cap, a withdrawn one is gone (and
/// a second withdraw says so), a take empties it.
#[test]
fn the_inbox_queues_withdraws_and_takes_in_order() {
    let steer = Steer::new();
    for i in 0..swiss_llm::history::STEER_MAX {
        steer.post(&format!("m{i}"), &format!("words {i}")).expect("under the cap");
    }
    assert_eq!(steer.post("over", "one too many"), Err(SteerRefused::Full));
    assert!(steer.withdraw("m1"), "a waiting message comes back");
    assert!(!steer.withdraw("m1"), "only once");
    let taken = steer.take();
    assert_eq!(taken.len(), swiss_llm::history::STEER_MAX - 1);
    assert_eq!(taken[0], ("m0".to_string(), "words 0".to_string()));
    assert_eq!(taken[1].0, "m2", "the order holds around the withdrawn one");
    assert!(steer.take().is_empty(), "a take empties it");
}

/// A tool whose answer echoes a stored vault value, as `cat .env` would.
struct Leaky;

#[async_trait::async_trait]
impl Toolbox for Leaky {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "quick".into(),
            description: "answers at once".into(),
            schema: json!({ "type": "object", "properties": {} }),
        }]
    }

    async fn call(&mut self, _name: &str, _args: &Value) -> ToolAnswer {
        ToolAnswer { text: "API_KEY=loop-leak-value-0042\nshort=v".into(), ok: true }
    }
}

/// Whatever put a stored value into a tool's answer - a file a command wrote, a config read
/// back - the model never reads it: the next ask, the surface's result and the filed turn
/// all carry the mask, and a short value (under the mask's 8-byte floor) stays text.
#[tokio::test]
async fn a_stored_value_in_a_tool_answer_reaches_the_model_masked() {
    tokio::task::spawn_blocking(|| {
        let _guard = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
        let path = swiss_core::paths::test_home().join("secrets.json");
        swiss_core::secure::secretstore::inject_vault(&path);
        let rev = swiss_core::secure::secretstore::vault_rev();
        swiss_core::secure::secretstore::put_secret(&path, "loop-leak", "loop-leak-value-0042", rev).expect("plant");
    })
    .await
    .expect("the plant task");
    let ask = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_quick\",\"type\":\"function\",\"function\":{\"name\":\"quick\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"tiny\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let (base, kept) = sse_wire_kept(vec![ask, closing_round("c2", "done.")]).await;
    let row = swiss_llm::config::LlmConfig::parse(&json!({
        "providers": { "local": { "style": "openai", "baseUrl": base + "/v1", "models": [{ "id": "tiny" }] } }
    }))
    .expect("row");
    let dir = home("llm-agent-mask");
    let history = LlmHistory::open(&dir).await.expect("open");
    let chat = swiss_llm::provider::Chat::new(row.provider("local").expect("provider"), Some("tiny"), 64)
        .expect("chat")
        .recording(Some(history.clone()), Some("alpha"));
    let mut run = history.open_conversation("alpha", "builtin", Some("local"), Some("tiny")).await;
    let run_id = run.id().expect("reserved");
    let session = Session::disabled();
    let mut heard: Vec<String> = Vec::new();
    let mut on_event = |event: LoopEvent| {
        if let LoopEvent::ToolResult { text, .. } = event {
            heard.push(text);
        }
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_loop(&chat, "You are a test.", "read the env", &mut Leaky, 4, &session, &mut run, &mut on_event),
    )
    .await
    .expect("bounded")
    .expect("the loop answers");
    assert_eq!(outcome.summary, "done.");
    let masked = format!("API_KEY={}\nshort=v", swiss_core::secure::literal::MASK);
    assert_eq!(heard, vec![masked.clone()], "the surface hears the mask");
    let bodies = kept.lock().expect("kept").clone();
    assert!(!bodies[1].contains("loop-leak-value-0042"), "the next ask never carries the value: {}", bodies[1]);
    assert!(bodies[1].contains("API_KEY="), "{}", bodies[1]);
    let turns = history.read_turns(run_id).await.expect("read").expect("run").to_string();
    assert!(!turns.contains("loop-leak-value-0042"), "the filed turn carries the mask: {turns}");
    history.close().await.expect("close");
}
