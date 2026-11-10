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

//! The llm module's own history database (SPEC llm.history): every chat and embedding
//! round trip, every agent conversation. The acceptance set: a recorded call is visible
//! to the very next read (L2), a long body chunks, seals and reads back intact (the
//! body contract), a run's turns carry the whole dialogue, previews never lie. (The
//! L1 case - a constructed plugin opens no database - lives with the plugin, in
//! swiss-plugins' llm.rs, where the constructor is.) Every test builds its own scratch home.

use sqlx::Row;
use swiss_llm::history::{ChatCall, ChatOutcome, LlmHistory};
use swiss_llm::provider::{CallError, Tokens};

fn home(name: &str) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

#[tokio::test]
async fn a_recorded_call_is_visible_to_the_very_next_read() {
    let dir = home("llm-hist-l2-visibility");
    let history = LlmHistory::open(&dir).await.expect("open");
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("distiller"),
        system: Some("be brief"),
        prompt: "what is a port?",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["a loopback address".to_string()],
            thinking: &["weighing the question first".to_string()],
            tool_calls: &[],
            cut: false,
            tokens: Tokens { input: 10, output: 4, cache_read: 0, cache_creation: 0 },
            ms: 812,
            ttft_ms: None,
            request_id: "swiss-test-1",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let calls = page["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 1, "{page}");
    let call = &calls[0];
    assert_eq!(call["kind"], "chat");
    assert_eq!(call["provider"], "glm");
    assert_eq!(call["agent"], "distiller");
    assert_eq!(call["ok"], true);
    assert_eq!(call["tokensIn"], 10);
    assert_eq!(call["tokensOut"], 4);
    assert_eq!(call["requestId"], "swiss-test-1");
    assert!(call["preview"].as_str().unwrap().contains("a loopback address"));
    // The body expands to the sealed request digest plus the answer.
    let id = call["id"].as_i64().unwrap();
    let body = history.read_call_body(id).await.expect("read").expect("row");
    assert_eq!(body["bodyGone"], false);
    let text = body["body"].as_str().expect("text");
    assert!(text.contains("what is a port?"), "{text}");
    assert!(text.contains("a loopback address"));
    assert!(text.contains("\"thinking\""), "the reasoning files beside the texts: {text}");
    assert!(text.contains("weighing the question first"), "{text}");
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_call_keeps_the_whole_ask_and_answer_wire_not_a_digest() {
    let dir = home("llm-hist-full-wire");
    let history = LlmHistory::open(&dir).await.expect("open");
    let wire = serde_json::json!({
        "endpoint": "https://example.test/api/anthropic/v1/messages",
        "headers": {
            "content-type": "application/json",
            "anthropic-version": "2023-06-01",
            "x-api-key": "(credential, not recorded)",
            "authorization": "(credential, not recorded)"
        },
        "options": {"max_tokens": 1024}
    });
    let asked = serde_json::json!({
        "system": "be brief",
        "messages": [
            {"role": "user", "content": "what is a port?"}
        ],
        "tools": [{"name": "read"}]
    });
    let contents = serde_json::json!([{"Text": "a loopback address"}]);
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("distiller"),
        system: Some("be brief"),
        prompt: "what is a port?",
        turn: Some(2),
        wire: Some(&wire),
        request: Some(&asked),
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["a loopback address".to_string()],
            thinking: &[],
            tool_calls: &[("read".into(), "{\"path\":\"notes.md\"}".into(), "call-7".into())],
            cut: false,
            tokens: Tokens { input: 10, output: 4, cache_read: 0, cache_creation: 0 },
            ms: 500,
            ttft_ms: None,
            request_id: "swiss-wire-1",
            stop_reason: Some("tool_call"),
            contents: &contents,
        },
    });
    history.flush().await;
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let call = &page["calls"].as_array().unwrap()[0];
    let body = history.read_call_body(call["id"].as_i64().unwrap()).await.expect("read").expect("row");
    let text = body["body"].as_str().expect("text");
    // The whole ask: endpoint, headers, options, messages, tools.
    assert!(text.contains("https://example.test/api/anthropic/v1/messages"), "{text}");
    assert!(text.contains("anthropic-version"), "{text}");
    assert!(text.contains("\"messages\""), "the full message list rides: {text}");
    assert!(text.contains("what is a port?"), "{text}");
    assert!(text.contains("\"tools\""), "the offered tools ride: {text}");
    // The whole answer: stop reason, request id, the tool call's call id, contents.
    assert!(text.contains("tool_call"), "{text}");
    assert!(text.contains("swiss-wire-1"), "{text}");
    assert!(text.contains("call-7"), "{text}");
    assert!(text.contains("contents"), "the full parsed content rides: {text}");
    // A credential header is named but never valued: the placeholder is the value.
    assert!(!text.contains("Bearer "), "no bearer value anywhere: {text}");
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_run_joins_its_calls_by_window() {
    let dir = home("llm-hist-run-calls");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.run_recorder("distiller", "builtin", Some("glm"), Some("glm-5.3"));
    run.note("user", "look and answer");
    run.note("final", "done");
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("distiller"),
        system: None,
        prompt: "look and answer",
        turn: Some(0),
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["done".to_string()],
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: Tokens::default(),
            ms: 5,
            ttft_ms: None,
            request_id: "swiss-join-1",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    // A different agent's call outside the run must not join.
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("other"),
        system: None,
        prompt: "not this run",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["x".to_string()],
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: Tokens::default(),
            ms: 1,
            ttft_ms: None,
            request_id: "swiss-join-2",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    // The run's window closes at finish; the calls must sit inside it, as the
    // loop's really do (noted first, filed last).
    run.finish(true);
    history.flush().await;
    let runs = history.read_runs(0, 10, None, None).await.expect("read");
    let run_id = runs["runs"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    let joined = history.read_calls_of_run(run_id).await.expect("read").expect("row");
    let calls = joined["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 1, "only this agent's calls join: {joined}");
    assert_eq!(calls[0]["agent"], "distiller");
    assert_eq!(calls[0]["requestId"], "swiss-join-1");
    assert!(
        history.read_calls_of_run(9999).await.expect("read").is_none(),
        "an absent run is a None, not an error"
    );
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_failed_call_records_its_error_and_an_embedding_its_width() {
    let dir = home("llm-hist-error-and-embedding");
    let history = LlmHistory::open(&dir).await.expect("open");
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: None,
        system: None,
        prompt: "hello",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Err(&CallError {
            message: "the provider answered 429: slow down".into(),
            status: Some(429),
            kind: "http",
            retryable: true,
            duration_ms: 120,
        }),
    });
    let asked: Vec<String> = vec!["note one".into(), "note two is longer".into(), "third".into()];
    let vectors: Vec<Vec<f32>> = vec![vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]; 3];
    history.record_embedding(swiss_llm::history::EmbedCall { provider: "ollama", model: "bge-m3", agent: Some("memory"), texts: &asked, ms: 40, outcome: Ok(vectors.as_slice()) });
    history.record_embedding(swiss_llm::history::EmbedCall { provider: "ollama", model: "bge-m3", agent: Some("memory"), texts: &["alone".to_string()], ms: 15, outcome: Err("no route") });
    history.flush().await;
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let calls = page["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 3, "{page}");
    assert_eq!(calls[0]["kind"], "embedding", "newest first: the failed embedding");
    assert_eq!(calls[0]["ok"], false);
    assert_eq!(calls[1]["kind"], "embedding");
    assert_eq!(calls[1]["ok"], true);
    assert_eq!(calls[2]["ok"], false, "the 429 chat");
    assert_eq!(calls[2]["status"], 429);
    // The kind filter narrows without touching the query.
    let chats = history.read_calls(0, 10, Some("chat"), None).await.expect("read");
    assert_eq!(chats["calls"].as_array().unwrap().len(), 1);
    // The needle matches provider and agent, ASCII-folded contains.
    let needles = history.read_calls(0, 10, None, Some("GLM")).await.expect("read");
    assert_eq!(needles["calls"].as_array().unwrap().len(), 1);
    // The ok embedding's body carries the asked texts and every vector, rounded -
    // the input and the answer are inspectable, not just counted (SPEC llm.history).
    let ok_id = calls[1]["id"].as_i64().expect("id");
    let ok_body = history.read_call_body(ok_id).await.expect("body").expect("some");
    let text = ok_body["body"].as_str().expect("sealed text");
    assert!(text.contains("\"texts\""), "{text}");
    assert!(text.contains("note two is longer"), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(text).expect("the body is JSON");
    let vectors = parsed["response"]["vectors"].as_array().expect("vectors");
    assert_eq!(vectors.len(), 3, "every vector of the batch: {text}");
    assert_eq!(vectors[0].as_array().expect("first").len(), 8);
    assert_eq!(vectors[0][0], serde_json::json!(0.1), "values keep f32 scale");
    // The failed one spells its error.
    let err_id = calls[0]["id"].as_i64().expect("id");
    let err_body = history.read_call_body(err_id).await.expect("body").expect("some");
    assert!(err_body["body"].as_str().expect("sealed text").contains("no route"));
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_long_call_body_chunks_seals_and_reads_back_intact() {
    let dir = home("llm-hist-long-body");
    let history = LlmHistory::open(&dir).await.expect("open");
    let answer = "swiss ".repeat(20_000); // ~120 KB, two sealed parts of 64 KiB plus a tail
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: None,
        system: None,
        prompt: "sing",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: std::slice::from_ref(&answer),
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: Tokens::default(),
            ms: 5,
            ttft_ms: None,
            request_id: "swiss-long",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    history.flush().await;
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let call = &page["calls"].as_array().unwrap()[0];
    assert_eq!(call["hasBody"], true, "a body past the preview always chunks");
    let chars = call["chars"].as_i64().unwrap() as usize;
    assert!(chars > 100_000, "{chars}");
    assert!(call["preview"].as_str().unwrap().len() <= 2_100, "the preview stays a preview");
    let body = history.read_call_body(call["id"].as_i64().unwrap())
        .await
        .expect("read")
        .expect("row");
    let text = body["body"].as_str().expect("text");
    assert!(text.contains(&answer[..40]), "the body reads back from its first chunk");
    assert!(text.len() > 100_000, "the whole body reassembles: {}", text.len());
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_run_files_its_whole_dialogue() {
    let dir = home("llm-hist-run-dialogue");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.run_recorder("distiller", "builtin", Some("glm"), Some("glm-5.3"));
    run.note("system", "be brief");
    run.note("user", "read notes and file a proposal");
    run.note("assistant", "tool call read: notes.md");
    run.note("tool", "read: the notes text");
    run.note("final", "proposal filed");
    run.finish(true);
    history.flush().await;
    let runs = history.read_runs(0, 10, None, None).await.expect("read");
    let list = runs["runs"].as_array().expect("runs");
    assert_eq!(list.len(), 1, "{runs}");
    assert_eq!(list[0]["agent"], "distiller");
    assert_eq!(list[0]["runtime"], "builtin");
    assert_eq!(list[0]["ok"], true);
    assert_eq!(list[0]["turns"], 5);
    let run_id = list[0]["id"].as_i64().unwrap();
    let detail = history.read_turns(run_id).await.expect("read").expect("row");
    let turns = detail["turns"].as_array().expect("turns");
    let roles: Vec<&str> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "tool", "final"]);
    // One turn body expands in full.
    let body = history.read_turn_body(run_id, 2).await.expect("read").expect("row");
    assert_eq!(body["body"].as_str().unwrap(), "read notes and file a proposal");
    // A run that never happened leaves no row: the none-recorder no-ops.
    let mut none = swiss_llm::history::RunRecorder::none();
    none.note("user", "never recorded");
    none.finish(true);
    history.flush().await;
    let runs = history.read_runs(0, 10, None, None).await.expect("read");
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1, "the none recorder files nothing");
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_chain_that_will_not_open_says_gone_not_absent() {
    // Corrupt one chunk's codec after the fact (a row that exists, a chain that
    // will not open): the read must answer the row with bodyGone, never a 404,
    // and never an unwrap panic. An EMPTY body answers body="" gone=false -
    // the all-tool-calls assistant turn is mainline, not damage.
    let dir = home("llm-hist-gone-not-absent");
    let history = LlmHistory::open(&dir).await.expect("open");
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: None,
        system: None,
        prompt: "say nothing worth sealing",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["short".to_string()],
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: Tokens { input: 3, output: 1, cache_read: 0, cache_creation: 0 },
            ms: 40,
            ttft_ms: None,
            request_id: "swiss-test-gone",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    let mut run = history.run_recorder("corruptor", "builtin", None, None);
    run.note("assistant", "");
    run.note("final", "a turn with a real body");
    run.finish(true);
    history.flush().await;
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let call_id = page["calls"][0]["id"].as_i64().unwrap();
    let runs = history.read_runs(0, 10, None, None).await.expect("read");
    let run_id = runs["runs"][0]["id"].as_i64().unwrap();
    // Before any damage: the empty assistant turn answers empty-and-present, not gone.
    let clean = history.read_turn_body(run_id, 1).await.expect("read").expect("row");
    assert_eq!(clean["body"], "");
    assert_eq!(clean["bodyGone"], false, "an empty turn is not a pruned one: {clean}");
    let present = history.read_turn_body(run_id, 2).await.expect("read").expect("row");
    assert_eq!(present["body"], "a turn with a real body");
    assert_eq!(present["bodyGone"], false);
    history.close().await.expect("close");

    // Break the sealed chains behind both rows, then reopen.
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", dir.join("llm.db").display()))
        .await
        .expect("pool");
    sqlx::query("UPDATE call_body SET codec = 99")
        .execute(&pool)
        .await
        .expect("corrupt call body");
    sqlx::query("UPDATE turn_body SET codec = 99")
        .execute(&pool)
        .await
        .expect("corrupt turn body");
    pool.close().await;

    let history = LlmHistory::open(&dir).await.expect("reopen");
    let body = history.read_call_body(call_id).await.expect("read").expect("row");
    assert_eq!(body["bodyGone"], true, "a refused chain is gone, not absent: {body}");
    assert_eq!(body["body"], "");
    // The corrupted chain on turn 2 says gone; the EMPTY turn 1 still answers
    // empty-and-present (its zero-chunk chain has nothing to refuse).
    let gone = history.read_turn_body(run_id, 2).await.expect("read").expect("row");
    assert_eq!(gone["bodyGone"], true, "the corrupted turn chain: {gone}");
    assert_eq!(gone["body"], "");
    let empty = history.read_turn_body(run_id, 1).await.expect("read").expect("row");
    assert_eq!(empty["bodyGone"], false, "no chain to refuse: {empty}");
    // The turn list still answers both rows with their previews.
    let detail = history.read_turns(run_id).await.expect("read").expect("row");
    assert_eq!(detail["turns"].as_array().unwrap().len(), 2);
    history.close().await.expect("close");
}
#[tokio::test]
async fn resent_messages_are_stored_once_not_once_per_round_trip() {
    // The loop resends the whole prior conversation every round trip, so whole-body
    // storage grows with the square of the dialogue. The piece store keeps each
    // message once: two round trips sharing messages, system, wire and tools file one
    // copy of each piece, and both bodies still read back whole and equal.
    let dir = home("llm-hist-piece-dedup");
    let history = LlmHistory::open(&dir).await.expect("open");
    let wire = serde_json::json!({
        "endpoint": "https://example.test/api/anthropic/v1/messages",
        "headers": {"content-type": "application/json"},
        "options": {"max_tokens": 1024}
    });
    let first_ask = serde_json::json!({
        "system": "be brief",
        "messages": [
            {"role": "user", "content": "what is a port?"},
            {"role": "assistant", "content": "a loopback address"}
        ],
        "tools": [{"name": "read"}]
    });
    let second_ask = serde_json::json!({
        "system": "be brief",
        "messages": [
            {"role": "user", "content": "what is a port?"},
            {"role": "assistant", "content": "a loopback address"},
            {"role": "user", "content": "and a tunnel?"}
        ],
        "tools": [{"name": "read"}]
    });
    for (ask, prompt) in [(&first_ask, "what is a port?"), (&second_ask, "and a tunnel?")] {
        history.record_chat(ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("distiller"),
            system: Some("be brief"),
            prompt,
            turn: None,
            wire: Some(&wire),
            request: Some(ask),
            replay: None,
            attempt: None,
            outcome: ChatOutcome::Ok {
                texts: &["answered".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens::default(),
                ms: 3,
                ttft_ms: None,
                request_id: "swiss-dedup",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        });
    }
    history.flush().await;
    // Both bodies read back with the shared conversation whole.
    let calls = history.read_calls(0, 10, None, None).await.expect("read");
    let rows = calls["calls"].as_array().expect("calls");
    assert_eq!(rows.len(), 2);
    // The list is newest first: the second ask (three messages) is rows[0].
    for (i, ask) in [second_ask, first_ask].iter().enumerate() {
        let id = rows[i]["id"].as_i64().unwrap();
        let body = history.read_call_body(id).await.expect("read").expect("row");
        assert_eq!(body["bodyGone"], false);
        let text = body["body"].as_str().expect("text");
        let parsed: serde_json::Value = serde_json::from_str(text).expect("whole body");
        let messages = parsed["request"]["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), ask["messages"].as_array().unwrap().len(), "round trip {i}");
        assert!(text.contains("a loopback address"));
        assert!(text.contains("be brief"), "the system piece resolves");
        assert!(text.contains("example.test"), "the wire piece resolves");
        assert!(text.contains("\"read\""), "the tools piece resolves");
    }
    // The piece table holds each UNIQUE piece once: the wire, the system prompt,
    // the tools array and three messages (two shared, one new) - six pieces, where
    // a whole-body store would have chunked both bodies in full.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}?mode=ro", dir.join("llm.db").to_string_lossy()))
        .await
        .expect("pool");
    let pieces: i64 = sqlx::query("SELECT count(*) FROM piece")
        .fetch_one(&pool)
        .await
        .expect("count")
        .try_get(0)
        .expect("scalar");
    assert_eq!(pieces, 6, "each unique piece once: {pieces}");
    pool.close().await;
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_dialogue_is_one_fetch_with_full_texts_and_call_digests() {
    // The conversation view reads ONE route: every turn with its whole text (a turn
    // longer than the preview cap comes back uncut) and every provider call with its
    // prompt's opening and answer digested out of the body - the full wire stays a click away.
    let dir = home("llm-hist-dialogue");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.run_recorder("distiller", "builtin", Some("glm"), Some("glm-5.3"));
    run.note("system", "be brief");
    let long = format!("read notes and file a proposal. {}", "detail ".repeat(600));
    run.note("user", &long);
    run.note("assistant", "calling read");
    run.note("tool", "the notes say: ship it");
    run.note("final", "done");
    let wire = serde_json::json!({
        "endpoint": "https://example.test/api/anthropic/v1/messages",
        "headers": {"content-type": "application/json"},
        "options": {"max_tokens": 1024}
    });
    let asked = serde_json::json!({
        "system": "be brief",
        "messages": [{"role": "user", "content": "read the notes"}],
        "tools": [{"name": "read"}]
    });
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("distiller"),
        system: Some("be brief"),
        prompt: &long,
        turn: Some(1),
        wire: Some(&wire),
        request: Some(&asked),
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["done".to_string()],
            thinking: &[],
            tool_calls: &[("read".into(), "{\"path\":\"notes.md\"}".into(), "call-9".into())],
            cut: false,
            tokens: Tokens { input: 12, output: 4, cache_read: 0, cache_creation: 0 },
            ms: 7,
            ttft_ms: Some(3),
            request_id: "swiss-dialogue-1",
            stop_reason: Some("tool_call: tool_use"),
            contents: &serde_json::json!([]),
        },
    });
    run.finish(true);
    history.flush().await;
    let runs = history.read_runs(0, 10, None, None).await.expect("read");
    let run_id = runs["runs"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    let dialogue = history.read_dialogue(run_id).await.expect("read").expect("row");
    let turns = dialogue["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 5);
    assert_eq!(turns[1]["role"], "user");
    let text = turns[1]["text"].as_str().expect("full text");
    assert_eq!(text, long, "the whole turn text, not the preview cut");
    assert!(text.len() > 2048, "longer than the preview cap, so the cut would show: {}", text.len());
    let calls = dialogue["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    // A loop repeats the ask's prompt on every call: a call carries its opening, the
    // user turn above the whole.
    assert_eq!(call["prompt"].as_str(), Some(long.chars().take(400).collect::<String>().as_str()));
    assert!(call["preview"].as_str().is_some_and(|p| p.chars().count() <= 400));
    assert_eq!(call["stopReason"], "tool_call: tool_use");
    assert_eq!(call["ttftMs"], 3, "the first output's wait, as the round said it");
    let tool_calls = call["toolCalls"].as_array().expect("tool calls");
    assert_eq!(tool_calls[0]["name"], "read");
    assert_eq!(tool_calls[0]["callId"], "call-9");
    let answer = call["answer"].as_array().expect("answer");
    assert_eq!(answer[0], "done");
    // The continuation guard reads the system turn alone, as the dialogue has it.
    assert_eq!(history.system_note_of(run_id).await.as_deref(), Some("be brief"));
    history.close().await.expect("close");
}

/// Deterministic high-entropy ASCII of `len` chars: a xorshift64* stream
/// (full-period 64-bit state, none of the short cycles a low-bit LCG falls
/// into) rendered six bits at a time onto a 64-symbol printable alphabet,
/// no dependency. LZ4 finds no runs to squeeze here, so the sealed body
/// keeps its bytes and the physical weight a test grows is real, not
/// compressible theater.
fn high_entropy_ascii(len: usize) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let mixed = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        for shift in [0, 6, 12, 18, 24, 30, 36, 42, 48, 54] {
            if out.len() >= len {
                break;
            }
            out.push(ALPHABET[((mixed >> shift) & 0x3F) as usize] as char);
        }
    }
    out
}

/// A tiny chat record with an overridable prompt, the quota tests' grain.
fn record_small(history: &LlmHistory, prompt: &str) {
    record_small_in(history, None, prompt);
}

/// [record_small] inside a run when one is given.
fn record_small_in(history: &LlmHistory, run: Option<i64>, prompt: &str) {
    let call = ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: None,
        system: None,
        prompt,
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
            tokens: Tokens { input: 3, output: 1, cache_read: 0, cache_creation: 0 },
            ms: 5,
            ttft_ms: None,
            request_id: "swiss-test-q",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    };
    match run {
        Some(run) => history.record_chat_in_run(run, call),
        None => history.record_chat(call),
    }
}

/// Completion refreshes physical usage before the next admission. High-entropy
/// prompts leave real bytes behind even after compression and sealing, so the next
/// large prompt cannot reuse the headroom the previous one already consumed.
#[tokio::test]
async fn completed_calls_refresh_the_physical_share_snapshot() {
    let dir = home("llm-hist-maintain-refresh");
    let history = LlmHistory::open(&dir).await.expect("open");
    let open = history.open_conversation("leaker", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run = Some(open.id().expect("the conversation is reserved"));
    record_small_in(&history, run, "seed record");
    history.flush().await;
    let (calls, _, _) = history.usage().await;
    assert_eq!(calls, 1);

    // Enroll from the SAME public physical measure set_quota snapshots, with
    // headroom for ONE high-entropy prompt: the runtime bound must cut the second.
    let baseline = swiss_host::history::quota::measure(&dir.join("llm.db")).total();
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("llm".to_string(), baseline + 260_000);
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares));

    // The first write lands; its measured bytes plus the next estimate cross the share.
    let noise = high_entropy_ascii(2 * 200_000);
    record_small_in(&history, run, &format!("leak 0 {}", &noise[0..200_000]));
    history.flush().await;
    record_small_in(&history, run, &format!("leak 1 {}", &noise[200_000..]));
    history.flush().await;
    let (grown, _, _) = history.usage().await;
    assert_eq!(
        grown, calls + 1,
        "the landed write is spent; the write past the share is refused without waiting for maintain",
    );

    // The landed weight is REAL, not a compressed-away artifact: the persisted
    // body reads back WHOLE through the public body seam.
    let page = history.read_calls(0, 24, None, None).await.expect("read");
    let rows = page["calls"].as_array().expect("calls");
    let long = rows
        .iter()
        .find(|row| row["preview"].as_str().is_some_and(|text| text.contains("leak 0 ")))
        .expect("the leak row is listed");
    let id = long["id"].as_i64().expect("row id");
    let body = history.read_call_body(id).await.expect("body read").expect("body exists");
    assert_eq!(body["bodyGone"], false, "the body is present, not reclaimed");
    let text = body["body"].as_str().expect("sealed body text");
    let decoded: serde_json::Value = serde_json::from_str(text).expect("the persisted body decodes as JSON");
    let exact = format!("leak 0 {}", &noise[0..200_000]);
    assert_eq!(
        decoded["request"]["prompt"].as_str(),
        Some(exact.as_str()),
        "the persisted request.prompt is EXACTLY the whole 200 KiB prompt",
    );

    // Maintenance keeps the recent conversation and its sealed body intact.
    history.maintain(180 * 24 * 60 * 60 * 1000).await;
    let refreshed = swiss_host::history::quota::measure(&dir.join("llm.db")).total();
    assert!(
        refreshed > baseline,
        "the landed body is on disk: refreshed {refreshed} vs baseline {baseline}",
    );
    // An oversized request is still refused after maintenance.
    let big = high_entropy_ascii(500_000);
    record_small_in(&history, run, &format!("post-refresh {big}"));
    history.flush().await;
    let (after, _, _) = history.usage().await;
    assert_eq!(after, grown, "the refreshed snapshot refuses the oversized write");
    history.close().await.expect("close");
}

/// The sweep is the quota's exit, not its victim: with the share exhausted every
/// record submit is refused, yet the sweep still deletes - a quota that refuses
/// its own reclamation path would wedge with no exit (the store contract).
#[tokio::test]
async fn the_sweep_reclaims_even_when_the_share_is_exhausted() {
    let dir = home("llm-hist-sweep-quota-exit");
    let history = LlmHistory::open(&dir).await.expect("open");
    record_small(&history, "first record");
    record_small(&history, "second record");
    history.flush().await;
    let (calls, _, bytes) = history.usage().await;
    assert!(calls >= 2, "two calls recorded");

    // Wedge the owner: a share below the measured file refuses every plain submit.
    let mut shares = std::collections::BTreeMap::new();
    shares.insert("llm".to_string(), bytes.saturating_sub(1).max(1));
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares));
    record_small(&history, "refused record");
    history.flush().await;
    let (wedged, _, _) = history.usage().await;
    assert_eq!(wedged, calls, "a plain record is refused at the exhausted share");

    // The exit: the sweep submits exempt and still deletes every row. Age zero
    // makes the cutoff "now"; the sleep keeps it strictly after the writes.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    history.sweep(0).await;
    history.flush().await;
    let (swept, runs, _) = history.usage().await;
    assert_eq!(swept, 0, "the exempt sweep reclaims what the quota refused to");
    assert_eq!(runs, 0, "no runs were recorded, none should appear");
    history.close().await.expect("close");
}

// --- run reservation and the conversation lifecycle (SPEC llm.agents Test) --------------------

/// The allocator's core promise: ids are DATABASE-allocated, so two handles on
/// one directory - two threads, two processes - never reserve the same row, and
/// every reservation is a real row.
#[tokio::test]
async fn two_handles_reserving_at_once_never_share_an_id() {
    let dir = home("llm-hist-two-handles");
    let first = LlmHistory::open(&dir).await.expect("open");
    let second = LlmHistory::open(&dir).await.expect("open again");
    let mut a = first.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    let mut b = second.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    a.reserve().await;
    b.reserve().await;
    let (ida, idb) = (a.id().expect("a reserved"), b.id().expect("b reserved"));
    assert_ne!(ida, idb, "two live reservations never share a row");
    a.finish(true);
    b.finish(true);
    first.close().await.expect("close");
    second.close().await.expect("close again");
}

/// A restart never reuses an orphan's id: the orphan's pending row stays its own
/// (its calls joined to it by the explicit relation), and the next reservation
/// takes the NEXT row.
#[tokio::test]
async fn a_restart_never_reuses_an_orphaned_reservation() {
    let dir = home("llm-hist-orphan-restart");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut orphan = history.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    orphan.reserve().await;
    let orphan_id = orphan.id().expect("reserved");
    // The orphan's call joined its row before the "crash" (no finish ever ran).
    history.record_chat_in_run(
        orphan_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "what is a port?",
            turn: Some(0),
            wire: None,
            request: None,
            replay: None,
            attempt: None,
            outcome: ChatOutcome::Ok {
                texts: &["a loopback address".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 10, output: 4, cache_read: 0, cache_creation: 0 },
                ms: 812,
                ttft_ms: None,
                request_id: "swiss-test-orphan",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        },
    );
    drop(orphan);
    history.close().await.expect("close");

    let reopened = LlmHistory::open(&dir).await.expect("reopen");
    let mut fresh = reopened.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    fresh.reserve().await;
    let fresh_id = fresh.id().expect("reserved");
    assert_ne!(fresh_id, orphan_id, "an orphan's id is never reused");
    // The orphan's calls still belong to the orphan, not the fresh row.
    let calls = reopened.read_calls_of_run(orphan_id).await.expect("read").expect("row");
    assert_eq!(calls["calls"].as_array().map(Vec::len), Some(1), "{calls}");
    let joined = reopened.read_calls_of_run(fresh_id).await.expect("read").expect("row");
    assert_eq!(joined["calls"].as_array().map(Vec::len), Some(0), "{joined}");
    fresh.finish(true);
    reopened.close().await.expect("close");
}

/// A call row names its run on the wire (SPEC llm.history: "calls carry
/// their run"): a scoped row carries the REAL storage linkage on BOTH the
/// run-scoped listing and the flat page, and a one-shot's row carries a
/// PRESENT null - the key exists, so an absent key can never ride Value
/// indexing's Null and pass.
#[tokio::test]
async fn call_rows_carry_their_run_linkage_on_the_wire() {
    let dir = home("llm-hist-run-linkage");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    run.reserve().await;
    let run_id = run.id().expect("reserved");
    history.record_chat_in_run(
        run_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "scoped ask",
            turn: Some(0),
            wire: None,
            request: None,
            replay: None,
            attempt: None,
            outcome: ChatOutcome::Ok {
                texts: &["a scoped answer".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 10, output: 4, cache_read: 0, cache_creation: 0 },
                ms: 812,
                ttft_ms: None,
                request_id: "swiss-test-linkage",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        },
    );
    record_small(&history, "one-shot ask");
    run.finish(true);
    history.flush().await;

    // Run-scoped: the row names the run it was filed under.
    let scoped = history.read_calls_of_run(run_id).await.expect("read").expect("row");
    let rows = scoped["calls"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "{scoped}");
    assert!(
        rows[0].as_object().expect("an object").contains_key("runId"),
        "runId is a present field on the run-scoped row"
    );
    assert_eq!(
        rows[0]["runId"],
        serde_json::json!(run_id),
        "the scoped row carries its real run linkage"
    );

    // Flat page: the scoped row names the run; the one-shot's null is PRESENT.
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    let flat = page["calls"].as_array().cloned().unwrap_or_default();
    assert_eq!(flat.len(), 2, "{page}");
    for row in &flat {
        assert!(
            row.as_object().expect("an object").contains_key("runId"),
            "runId is a present field on every call row, never an absent key reading as null: {row}"
        );
    }
    let scoped_row = flat
        .iter()
        .find(|r| r["preview"].as_str().is_some_and(|p| p.contains("scoped ask")))
        .expect("the scoped row is listed");
    assert_eq!(scoped_row["runId"], serde_json::json!(run_id), "the flat page exposes the run linkage");
    let oneshot = flat
        .iter()
        .find(|r| r["preview"].as_str().is_some_and(|p| p.contains("one-shot ask")))
        .expect("the one-shot row is listed");
    assert_eq!(
        oneshot["runId"],
        serde_json::Value::Null,
        "a one-shot carries a PRESENT null run linkage"
    );
    history.close().await.expect("close");
}

/// A pending one-shot lists only while it is LIVE in this process (SPEC
/// llm.history Live): with no outcome and its live facts until its finish
/// writes the outcome. One no recorder holds any more - stranded by a stopped
/// gateway - never lists (the live tests pin that half).
#[tokio::test]
async fn a_pending_one_shot_lists_live_until_its_finish() {
    let dir = home("llm-hist-pending-hidden");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut shot = history.run_recorder("alpha", "builtin", Some("glm"), Some("glm-5.3"));
    shot.reserve().await;
    shot.note("user", "ping");
    let live = history.read_runs(0, 10, None, None).await.expect("read");
    let runs = live["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "a live one-shot lists: {live}");
    assert!(runs[0]["ok"].is_null(), "no outcome yet: {live}");
    assert_eq!(runs[0]["live"]["round"], 0, "{live}");
    assert_eq!(runs[0]["turns"], 1, "{live}");
    shot.finish(true);
    let shown = history.read_runs(0, 10, None, None).await.expect("read");
    let runs = shown["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "{shown}");
    assert_eq!(runs[0]["ok"], true, "{shown}");
    assert_eq!(runs[0]["turns"], 1, "{shown}");
    assert!(runs[0].get("live").is_none(), "{shown}");
    history.close().await.expect("close");
}

/// The conversation lifecycle: born OPEN and discoverable, a finish that files
/// turns but never closes it, a close that is neutral and idempotent.
#[tokio::test]
async fn a_conversation_is_born_open_and_finish_keeps_it_open() {
    let dir = home("llm-hist-open-lifecycle");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved open");
    let open = history.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("read");
    let runs = open["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "born open and discoverable: {open}");
    assert_eq!(runs[0]["id"], run_id, "{open}");
    // A send files its turns; the run stays open for the next send.
    run.note("user", "first ask");
    run.note("assistant", "first answer");
    run.finish(false);
    let still = history.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("read");
    assert_eq!(
        still["runs"].as_array().map(Vec::len),
        Some(1),
        "a finish never closes a conversation: {still}"
    );
    let turns = history.read_turns(run_id).await.expect("read").expect("row");
    assert_eq!(turns["turns"].as_array().map(Vec::len), Some(2), "{turns}");
    // The reader pressed New: neutral, idempotent, then read-only.
    assert!(history.close_run(run_id).await.expect("close").is_some());
    assert!(history.close_run(run_id).await.expect("close again").is_some(), "idempotent");
    let gone = history.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("read");
    assert_eq!(gone["runs"].as_array().map(Vec::len), Some(0), "{gone}");
    history.close().await.expect("close");
}

/// A conversation reopens on its own row and accumulates: the second session's
/// turns append after the first's (unique seqs), and the resume guards refuse
/// exactly what the route turns into its 409 codes.
#[tokio::test]
async fn a_conversation_resumes_accumulates_and_guards() {
    let dir = home("llm-hist-resume");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    // FIXTURE NORMALIZATION (the no-fork contract): the first session now
    // files a REAL terminal round - an awaited activity mark and a sealed
    // ChatCall whose canonical replay log the continuation rebuilds from -
    // the exact shape drive_loop seals. A notes-only session is the
    // forbidden used-turns-without-a-call shape and refuses by design, so
    // the positive resume rides a legitimately canonical dialogue; the
    // identity, seq, append and close assertions below keep their shape.
    run.note("user", "first ask");
    let attempt = run.begin_round().await.expect("the first round's activity mark commits");
    let replay = serde_json::json!([{ "role": "user", "text": "first ask" }]);
    history.record_chat_in_run(
        run_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "first ask",
            turn: Some(0),
            wire: None,
            request: None,
            replay: Some(&replay),
            attempt: Some(attempt),
            outcome: ChatOutcome::Ok {
                texts: &["first answer".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 4, output: 3, cache_read: 0, cache_creation: 0 },
                ms: 11,
                ttft_ms: None,
                request_id: "swiss-test-resume-first",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        },
    );
    run.note("assistant", "first answer");
    run.finish(true);
    // Wrong agent reads as absent; a changed model refuses context-changed.
    assert!(matches!(
        history.resume_conversation(run_id, "beta", "builtin", Some("glm"), Some("glm-5.3"), None).await,
        Err(swiss_llm::history::ResumeError::Gone)
    ));
    assert!(matches!(
        history.resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-4"), None).await,
        Err(swiss_llm::history::ResumeError::ContextChanged)
    ));
    let mut again = history
        .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
        .await
        .expect("resume");
    assert_eq!(again.id(), Some(run_id));
    again.note("user", "second ask");
    again.finish(true);
    let turns = history.read_turns(run_id).await.expect("read").expect("row");
    let seqs = turns["turns"]
        .as_array()
        .map(|turns| turns.iter().map(|turn| turn["seq"].as_i64().unwrap()).collect::<Vec<_>>())
        .expect("seqs");
    assert_eq!(seqs, vec![1, 2, 3], "appended after the first session's canonical round: {turns}");
    // Closed conversations never reopen.
    history.close_run(run_id).await.expect("close");
    assert!(matches!(
        history.resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None).await,
        Err(swiss_llm::history::ResumeError::Closed)
    ));
    history.close().await.expect("close");
}

/// The context guard reads the EFFECTIVE endpoint, not the provider's name: a
/// same-name entry edited to another endpoint (or style on the same URL, which
/// the header fingerprint carries) refuses the continuation - old thinking
/// provenance is invalid, a new vendor must not read the old dialogue.
#[tokio::test]
async fn a_same_name_provider_with_another_endpoint_refuses_the_resume() {
    let dir = home("llm-hist-endpoint-guard");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    // FIXTURE NORMALIZATION (the no-fork contract): the run's one call now
    // carries a canonical replay mark - the stricter resume gate requires
    // exactly that for the matched-wire positive, while every endpoint and
    // style NEGATIVE below fires on the wire comparison before the gate and
    // keeps its assertion untouched.
    run.note("user", "first ask");
    let attempt = run.begin_round().await.expect("the round's activity mark commits");
    let sent_wire = serde_json::json!({
        "endpoint": "https://old.example.test/v1/chat/completions",
        "headers": { "content-type": "application/json", "authorization": "(credential, not recorded)" },
        "options": {},
    });
    history.record_chat_in_run(
        run_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "what is a port?",
            turn: Some(0),
            wire: Some(&sent_wire),
            request: None,
            replay: Some(&serde_json::json!([{ "role": "user", "text": "what is a port?" }])),
            attempt: Some(attempt),
            outcome: ChatOutcome::Ok {
                texts: &["a loopback address".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 10, output: 4, cache_read: 0, cache_creation: 0 },
                ms: 812,
                ttft_ms: None,
                request_id: "swiss-test-endpoint-guard",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        },
    );
    run.finish(true);
    // Same name, same model - ANOTHER endpoint: the refusal names the context.
    let moved = serde_json::json!({
        "endpoint": "https://new.example.test/v1/chat/completions",
        "headers": { "content-type": "application/json", "authorization": "(credential, not recorded)" },
        "options": {},
    });
    assert!(matches!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), Some(&moved))
            .await,
        Err(swiss_llm::history::ResumeError::ContextChanged)
    ));
    // A style swap on the SAME endpoint (the anthropic fingerprint) refuses too.
    let restyled = serde_json::json!({
        "endpoint": "https://old.example.test/v1/chat/completions",
        "headers": { "content-type": "application/json", "anthropic-version": "2023-06-01", "x-api-key": "(credential, not recorded)" },
        "options": {},
    });
    assert!(matches!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), Some(&restyled))
            .await,
        Err(swiss_llm::history::ResumeError::ContextChanged)
    ));
    // The wire the first call actually spoke still resumes.
    let same = sent_wire.clone();
    assert!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), Some(&same))
            .await
            .is_ok()
    );
    history.close().await.expect("close");
}
/// R6: the continuation budget counts DECODED authoritative materialization -
/// full plain UTF-8 bytes: turn bodies, each call's ADDITIVE reference weight
/// (envelope plus every content-addressed piece it re-sends, writer-known at
/// seal time) and the referenced tail - never a 2 KiB preview, never char
/// counts, never the ciphertext/base64 footprint, never a physical-dedup
/// undercount. A run whose CALL body carries hundreds of KiB with tiny turn
/// notes must report that weight.
#[tokio::test]
async fn the_budget_counts_decoded_full_plain_bytes_not_previews() {
    let dir = home("llm-hist-budget-bytes");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    run.note("user", "one small ask");
    run.finish(true);
    // The sealed call body is the heavy part: a REAL genai request whose one
    // user message carries ~600 KiB, serialized by the public request_json -
    // exactly the request.messages shape record_chat_scoped copies, so the
    // weight that lands is the weight a real heavy round would land. Every
    // TURN stays tiny; chars-of-turns would answer ~14.
    let filler = "x".repeat(600 * 1024);
    let asked = genai::chat::ChatRequest::default()
        .append_message(genai::chat::ChatMessage::user(format!("heavy round {filler}")));
    let heavy = swiss_llm::provider::request_json(&asked);
    history.record_chat_in_run(
        run_id,
        ChatCall {
            provider: "glm",
            model: "glm-5.3",
            agent: Some("alpha"),
            system: None,
            prompt: "heavy round",
            turn: Some(0),
            wire: None,
            request: Some(&heavy),
            replay: None,
            attempt: None,
            outcome: ChatOutcome::Ok {
                texts: &["ok".to_string()],
                thinking: &[],
                tool_calls: &[],
                cut: false,
                tokens: Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
                ms: 5,
                ttft_ms: None,
                request_id: "swiss-test-budget-bytes",
                stop_reason: None,
                contents: &serde_json::json!({}),
            },
        },
    );
    // LANDING + FULL PIECE DECODE phase (before the budget oracle): the
    // persisted call reads back with request.messages EXACTLY the serialized
    // ones, and the whole 600 KiB payload decodes out of the sealed chunks.
    // The budget witness must weigh real decoded material - never a payload
    // the recorder discarded before the body ever split.
    let listed = history.read_calls_of_run(run_id).await.expect("calls of run").expect("run known");
    let call_id = listed["calls"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_i64())
        .expect("the call row is listed");
    let body = history.read_call_body(call_id).await.expect("body read").expect("body exists");
    let decoded: serde_json::Value =
        serde_json::from_str(body["body"].as_str().expect("sealed body text")).expect("body decodes");
    assert_eq!(
        decoded["request"]["messages"], heavy["messages"],
        "the persisted request messages are EXACTLY the serialized ones",
    );
    let joined = decoded["request"]["messages"].to_string();
    assert!(joined.contains(&filler), "the full 600 KiB payload decodes from the sealed pieces");
    let (turns, materialized, calls) = history.conversation_size(run_id).await.expect("size");
    assert_eq!((turns, calls), (1, 1), "the row counts stay exact");
    assert!(
        materialized >= 600 * 1024,
        "the budget must weigh the DECODED full body bytes ({materialized} < 600 KiB): a preview or chars-only sum blinds the guard"
    );
    history.close().await.expect("close");
}

/// R8: at most ONE open conversation per agent across handles. The route-side
/// check races straight through two handles; the invariant must hold at the
/// store. Distinct one-shot reservations keep their own ids regardless.
#[tokio::test]
async fn two_handles_cannot_both_hold_an_open_conversation_for_one_agent() {
    let dir = home("llm-hist-one-open-per-agent");
    let first = LlmHistory::open(&dir).await.expect("open first");
    let second = LlmHistory::open(&dir).await.expect("open second");
    let _a = first.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let _b = second.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let runs = first.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("open read");
    let open = runs["runs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        open.len(),
        1,
        "one agent holds at most one open conversation across handles: {runs}"
    );
    // One-shot reservations are a different lifecycle: distinct ids, always valid.
    let mut shot_a = first.run_recorder("alpha", "chat", Some("glm"), Some("glm-5.3"));
    let mut shot_b = second.run_recorder("alpha", "chat", Some("glm"), Some("glm-5.3"));
    shot_a.reserve().await;
    shot_b.reserve().await;
    let (ida, idb) = (shot_a.id(), shot_b.id());
    assert!(ida.is_some() && idb.is_some() && ida != idb, "distinct one-shot ids: {ida:?} vs {idb:?}");
    first.close().await.expect("close first");
    second.close().await.expect("close second");
}

/// One-open holds in the ACTIVE state too: a run whose round is IN FLIGHT
/// (its activity mark committed by begin_round, not yet settled by a sealed
/// completion) is still this agent's one live conversation. A second handle
/// must not reserve a new one beside it - neither through the write-time
/// conditional INSERT nor through the lane's prestream oracle. No provider,
/// no network: the mark's own receipt is the witness.
#[tokio::test]
async fn a_second_conversation_cannot_open_while_a_round_is_in_flight() {
    let dir = home("llm-hist-one-open-in-flight");
    let first = LlmHistory::open(&dir).await.expect("open first");
    let second = LlmHistory::open(&dir).await.expect("open second");
    let mut a = first.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let id = a.id().expect("reserved");
    // Positively await the mark's commit: begin_round answers only through
    // the oneshot the admitted job sends after the UPDATE lands.
    a.begin_round().await.expect("the round's activity mark commits");
    let b = second.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    assert!(b.id().is_none(), "a second conversation must not reserve while one is in flight: id {:?} refusal {:?}", b.id(), b.refusal());
    assert!(b.refusal().is_some_and(|why| why.contains("open-run-exists")), "the refusal names the one-open rule: {:?}", b.refusal());
    let runs = first.read_runs(0, 10, Some("alpha".into()), Some(true)).await.expect("open read");
    assert_eq!(runs["runs"].as_array().map(Vec::len), Some(1), "exactly one live row while the round is in flight: {runs}");
    // The outward one-open oracle must agree: an in-flight round is a live
    // conversation of this agent, not an invisible one.
    assert_eq!(first.continuable_open("alpha").await, Some(id), "the one-open oracle counts the in-flight run");
    first.close().await.expect("close first");
    second.close().await.expect("close second");
}

/// A run whose every round was interrupted before its call could file (turns
/// noted, no call row at all - a Stop or a refused write on the first round)
/// still continues: the model knows no sealed round, so its context starts with
/// the next send, and the noted turns stay in the transcript, never replayed.
#[tokio::test]
async fn a_used_run_without_a_sealed_round_continues_from_an_empty_context() {
    let dir = home("llm-hist-no-fork");
    let history = LlmHistory::open(&dir).await.expect("open");
    let mut run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    run.note("user", "the ask that flew");
    run.finish(true);
    let resumed = history
        .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
        .await;
    assert!(resumed.is_ok(), "an interrupted first round never strands the conversation");
    assert!(
        matches!(history.replay_state(run_id).await, swiss_llm::history::ReplayState::Fresh),
        "no sealed round: nothing is replayed as a guess"
    );
    history.close().await.expect("close");
}

/// The positive control for the no-fork rule: a GENUINELY unused reservation
/// (opened, never sent, no turns, no calls) keeps its agreed resume - the
/// fail-closed rule may not swallow the fresh case.
#[tokio::test]
async fn a_genuinely_unused_row_still_resumes() {
    let dir = home("llm-hist-unused-resumes");
    let history = LlmHistory::open(&dir).await.expect("open");
    let run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    assert!(
        history
            .resume_conversation(run_id, "alpha", "builtin", Some("glm"), Some("glm-5.3"), None)
            .await
            .is_ok(),
        "an unused reservation resumes - nothing was attempted, nothing can lie"
    );
    history.close().await.expect("close");
}

/// R7: a reservation goes through the admitted write queue like every other
/// write. The queue bound is wedged deterministically - unawaited big submits,
/// no drain race - and the reservation must REFUSE (no id, no row), not write
/// around the admission with a private query.
#[tokio::test]
async fn a_reservation_respects_the_write_admission() {
    let dir = home("llm-hist-reserve-admission");
    let history = LlmHistory::open(&dir).await.expect("open");
    // Deterministic admission wedge: a share map with NO llm entry refuses
    // every admitted submit (NotEnrolled) permanently - no queue filling, no
    // drain race, stable across every await in between.
    history.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(
        std::collections::BTreeMap::new(),
    ));
    let mut run = history.run_recorder("alpha", "chat", Some("glm"), Some("glm-5.3"));
    run.reserve().await;
    assert!(
        run.id().is_none(),
        "a reservation past a refusing admission must refuse honestly - the admitted queue is load-bearing, a query side-door is not"
    );
    history.close().await.expect("close");
}

// RETIRED: the synthetic note()+finish(true) virgin-fork case. It reached a
// blind-row refusal RED but never exercised a REAL provider boundary - and
// the honest fix (pre-outbound admitted activity) would legitimately leave a
// refused send's row unused, so this oracle could only be satisfied by an
// unadmitted write. The real no-fork seam (receipt-gated quota flip, actual
// outbound, all authoritative writes refused, continuation must refuse) lives
// in the ROOT suite as a_used_run_whose_writes_were_all_refused_refuses_continuation.

/// Lifecycle writes belong to the queued, generation-stamped contract, not to
/// a private query: after the store is withdrawn, a close must REFUSE, never
/// stamp a stale at_end on a dead store.
#[tokio::test]
async fn a_close_after_withdraw_never_writes_a_stale_lifecycle_stamp() {
    let dir = home("llm-hist-stale-close");
    let history = LlmHistory::open(&dir).await.expect("open");
    let run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let run_id = run.id().expect("reserved");
    history.close().await.expect("withdraw");
    let closed = history.close_run(run_id).await;
    assert!(
        closed.is_err(),
        "a lifecycle write on a withdrawn store must refuse - the queued, generation-stamped path is the only honest one"
    );
}

#[tokio::test]
async fn a_run_says_what_it_was_for_who_started_it_and_what_it_started() {
    use swiss_llm::history::{context, within, RunContext};
    let dir = home("llm-hist-run-context");
    let history = LlmHistory::open(&dir).await.expect("open");
    let scope = RunContext {
        task: Some("memory.recheck".into()),
        subject: Some("orders (6 notes)".into()),
        source: Some("memory:12".into()),
        parent: None,
    };
    // The lead: a tool loop that reads twice and delegates once.
    let lead_id = within(scope.clone(), async {
        assert_eq!(context(), scope, "the scope is what the code under it reads");
        let mut run = history.run_recorder("lead", "builtin", Some("glm"), Some("glm-5.3"));
        run.reserve().await;
        run.note("user", "recheck these notes");
        run.note("assistant", "tool call read [c1]: {\"path\":\"a.java\"}");
        run.note("tool", "read [c1]: class A {}");
        run.note("assistant", "tool call read [c2]: {\"path\":\"b.java\"}");
        run.note("assistant", "tool call delegate [c3]: {\"tasks\":[]}");
        run.note("final", "{}");
        let id = run.id().expect("reserved");
        run.finish(true);
        id
    })
    .await;
    // A worker its delegate call started: the same source, the lead as its parent.
    let child = RunContext { task: Some("memory.task".into()), subject: Some("read a.java".into()), source: scope.source.clone(), parent: Some(lead_id) };
    within(child, async {
        let mut run = history.run_recorder("test", "builtin", Some("glm"), Some("glm-5.3"));
        run.reserve().await;
        run.note("user", "read a.java");
        run.note("final", "it holds A");
        run.finish(true);
    })
    .await;
    // Out of any scope a run says nothing of either.
    let mut bare = history.run_recorder("alpha", "builtin", None, None);
    bare.note("user", "hi");
    bare.finish(true);
    history.flush().await;

    let page = history.read_runs(0, 10, None, None).await.expect("read");
    let runs = page["runs"].as_array().expect("runs");
    let ids: Vec<i64> = runs.iter().filter_map(|r| r["id"].as_i64()).collect();
    assert_eq!(runs.len(), 2, "a child run sits under its parent, not on the page: {page}");
    let lead = runs.iter().find(|r| r["id"] == lead_id).expect("the lead is listed");
    assert_eq!(lead["task"], "memory.recheck");
    assert_eq!(lead["subject"], "orders (6 notes)");
    assert_eq!(lead["source"], "memory:12");
    assert_eq!(lead["children"], 1);
    assert_eq!(lead["tools"], serde_json::json!({"delegate": 1, "read": 2}));
    let other = runs.iter().find(|r| r["id"] != lead_id).expect("the bare run");
    assert!(other["task"].is_null() && other["parent"].is_null() && other["tools"] == serde_json::json!({}), "{other}");
    // The agent filter lists children too: it asks for one agent's runs, wherever they sit.
    let tests = history.read_runs(0, 10, Some("test".into()), None).await.expect("read");
    assert_eq!(tests["runs"].as_array().map(Vec::len), Some(1), "{tests}");

    let sheet = history.read_turns(lead_id).await.expect("read").expect("the lead");
    let children = sheet["children"].as_array().expect("children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["task"], "memory.task");
    assert_eq!(children[0]["subject"], "read a.java");
    assert!(sheet.get("parentRun").is_none(), "the lead has no parent");
    let child_id = children[0]["id"].as_i64().expect("id");
    assert!(!ids.contains(&child_id));
    let child_sheet = history.read_turns(child_id).await.expect("read").expect("the child");
    assert_eq!(child_sheet["parentRun"]["id"], lead_id, "{child_sheet}");
    assert_eq!(child_sheet["parentRun"]["task"], "memory.recheck");
    history.close().await.expect("close");
}

/// SPEC §llm.history: a committed call notifies, and the read after flush sees it.
/// A sweep that removes nothing publishes nothing; one that removes rows does. The hook
/// itself asks a reader of its own, so each notice proves the commit came first.
#[tokio::test]
async fn a_committed_call_notifies_after_it_is_readable_and_an_empty_sweep_does_not() {
    use std::sync::{Arc, Mutex};
    let dir = home("llm-hist-notice");
    let history = LlmHistory::open(&dir).await.expect("open");
    let db = dir.join("llm.db");
    let seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    history.set_history_notice(Some(Arc::new(move || {
        let rows = count_seen_by_another_reader(&db, "SELECT count(*) FROM call", &[]);
        sink.lock().unwrap().push(rows);
    })));
    let take = || std::mem::take(&mut *seen.lock().unwrap());
    history.record_chat(ChatCall {
        provider: "glm",
        model: "glm-5.3",
        agent: Some("distiller"),
        system: None,
        prompt: "notice-needle",
        turn: None,
        wire: None,
        request: None,
        replay: None,
        attempt: None,
        outcome: ChatOutcome::Ok {
            texts: &["noticed".to_string()],
            thinking: &[],
            tool_calls: &[],
            cut: false,
            tokens: Tokens { input: 1, output: 1, cache_read: 0, cache_creation: 0 },
            ms: 4,
            ttft_ms: None,
            request_id: "",
            stop_reason: None,
            contents: &serde_json::json!({}),
        },
    });
    history.flush().await;
    assert_eq!(take(), vec![1], "the notice saw the committed call");
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    assert!(
        page["calls"][0]["preview"].as_str().unwrap_or("").contains("noticed"),
        "{page}"
    );
    history.sweep(10_i64 * 365 * 24 * 60 * 60 * 1000).await;
    assert_eq!(take(), Vec::<i64>::new(), "a sweep that removes nothing publishes nothing");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    history.sweep(0).await;
    assert_eq!(take(), vec![0], "the notice saw the rows gone");
    let page = history.read_calls(0, 10, None, None).await.expect("read");
    assert!(page["calls"].as_array().unwrap().is_empty(), "{page}");
    history.close().await.expect("close");
}

/// What a reader of its own sees in llm.db at this instant: one count on a fresh
/// connection, on a thread and runtime of its own, so neither the store's pool nor its
/// write queue stands between. A notice hook calls it, and it must not await on the
/// writer's runtime - the answer is what a page re-reading on that notice would get.
fn count_seen_by_another_reader(db: &std::path::Path, sql: &'static str, binds: &[&str]) -> i64 {
    let url = format!("sqlite://{}?mode=ro", db.to_string_lossy().replace('\\', "/"));
    let binds: Vec<String> = binds.iter().map(|b| b.to_string()).collect();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a reader runtime")
            .block_on(async move {
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect(&url)
                    .await
                    .expect("reach the store's file");
                let mut query = sqlx::query_scalar(sql);
                for bind in binds {
                    query = query.bind(bind);
                }
                let n: i64 = query.fetch_one(&pool).await.expect("count");
                pool.close().await;
                n
            })
    })
    .join()
    .expect("the reader thread")
}

