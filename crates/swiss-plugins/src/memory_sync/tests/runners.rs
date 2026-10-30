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

//! The runners: each runtime's spawn, its tools and prompt, its seat, a chat answer.

use super::*;

#[tokio::test]
async fn the_sync_runner_is_a_seated_gateway_child() {
    // SPEC §process.children / §security.rules: the runner's spawn carries a
    // Process-page row while it lives and drops it at every end - timeout,
    // error and clean exit all pass through the same guards.
    let children = swiss_host::services::children::ChildRegistry::new();
    set_children_seat(children.clone());
    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        ("cmd", ["/c", "ping", "-n", "2", "127.0.0.1"].iter().map(|s| s.to_string()).collect())
    } else {
        ("sh", ["-c", "sleep 2"].iter().map(|s| s.to_string()).collect())
    };
    let dir = scratch("runner-seat");
    let run = {
        let program = program.to_string();
        let dir = dir.clone();
        tokio::spawn(async move {
            spawn_and_collect(std::path::Path::new(&program), &dir, &args, &[], 30_000, "").await
        })
    };
    let mut seen = false;
    for _ in 0..400 {
        if children
            .rows()
            .iter()
            .any(|r| r.owner == "memory" && r.kind == swiss_host::services::children::ChildKind::Agent)
        {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(seen, "the runner's row is listed while it runs: {:?}", children.rows().iter().map(|r| r.label.clone()).collect::<Vec<_>>());
    let answer = run.await.expect("the task joins").expect("the runner answers");
    assert!(!answer.trim().is_empty(), "the child printed something: {answer:?}");
    assert!(
        children.rows().iter().all(|r| r.owner != "memory"),
        "the row left with the child"
    );
    // Leave the seat as the tests found it.
    *CHILDREN.write().unwrap_or_else(|e| e.into_inner()) = None;
}

#[test]
fn a_named_agent_becomes_its_runtimes_runner() {
    let llm = LlmState::new();
    assert!(runner(&llm, "b").err().expect("off").contains("off"), "the llm plugin is off");
    llm.apply(
        swiss_llm::LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "m" }, { "id": "other" }] } },
            "agents": {
                "b": { "provider": "local" },
                "c": { "runtime": "chat", "provider": "local", "model": "other" },
                "cc": { "runtime": "claude" },
                "p": { "runtime": "pi", "model": "glm/glm-5.3-flash" },
                "db": { "runtime": "db", "provider": "local" }
            }
        }))
        .expect("row"),
    );
    let shown = |name: &str| runner(&llm, name).map(|ask| (ask.label(), ask.model()));
    assert_eq!(shown("b"), Ok(("builtin:b".into(), Some("m".into()))));
    assert_eq!(shown("c"), Ok(("chat:c".into(), Some("other".into()))));
    assert_eq!(shown("cc"), Ok(("claude:cc".into(), None)));
    assert_eq!(shown("p"), Ok(("pi:p".into(), Some("glm/glm-5.3-flash".into()))));
    assert!(shown("gone").expect_err("no such agent").contains("gone"));
    // The Data page's agent runs no memory task, named or as the newest fallback.
    assert!(shown("db").expect_err("a DB agent").contains("serves the Data page alone"));
    assert_eq!(llm.newest_agent().as_deref(), Some("p"));
}

#[test]
fn a_spawned_runtime_gets_the_tools_and_the_prompt_its_agent_set() {
    let row = swiss_llm::LlmConfig::parse(&json!({
        "agents": {
            "cc": { "runtime": "claude" },
            "narrow": { "runtime": "claude", "tools": ["Glob", "Read"], "systemPrompt": "Prefer Grep." },
            "blind": { "runtime": "claude", "tools": [] },
            "p": { "runtime": "pi", "model": "glm/glm-5.3-flash" },
            "pn": { "runtime": "pi", "model": "glm/glm-5.3-flash", "tools": [], "systemPrompt": "Be terse." }
        }
    }))
    .expect("row");
    let claude = |name: &str| ClaudeAsk { agent: row.agents[name].clone(), telemetry: None, history: None }.args();
    let pi = |name: &str| PiAsk { agent: row.agents[name].clone(), history: None }.args();
    let after = |args: &[String], flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
    let has = |args: &[String], flag: &str| args.iter().any(|a| a == flag);
    let all = claude("cc");
    assert_eq!(after(&all, "--tools").as_deref(), Some("Read,Grep,Glob"), "no list: the whole offer");
    assert_eq!(after(&all, "--allowedTools").as_deref(), Some("Read,Grep,Glob"));
    assert!(!has(&all, "--append-system-prompt"), "nothing appended without a prompt");
    let narrow = claude("narrow");
    assert_eq!(after(&narrow, "--tools").as_deref(), Some("Read,Glob"), "the offer's order");
    assert_eq!(after(&narrow, "--append-system-prompt").as_deref(), Some("Prefer Grep."));
    let blind = claude("blind");
    assert_eq!(after(&blind, "--tools").as_deref(), Some(""), "an empty --tools disables every tool");
    assert!(!has(&blind, "--allowedTools") && has(&blind, "--restricted"));
    assert_eq!(after(&pi("p"), "--tools").as_deref(), Some("read,grep,find,ls"));
    let pn = pi("pn");
    assert!(has(&pn, "--no-tools") && !has(&pn, "--tools"), "{pn:?}");
    assert_eq!(after(&pn, "--append-system-prompt").as_deref(), Some("Be terse."));
    assert_eq!(after(&pn, "--model").as_deref(), Some("glm/glm-5.3-flash"));
    assert!(this_run(&["Read", "Glob"]).contains("working directory is the checkout. Your tools: Read, Glob."));
    assert!(this_run(&[]).contains("no tools"));
}

#[test]
fn the_longest_system_prompt_still_fits_a_windows_command_line() {
    // CreateProcess takes at most 32767 UTF-16 units, and the proposal schema rides
    // inline beside the prompt; each argument is quoted and its quotes escaped.
    let prompt = "Cite the \"path\" of every file you read. ".repeat(swiss_llm::agent::PROMPT_MAX / 40);
    let row = swiss_llm::LlmConfig::parse(&json!({
        "agents": { "cc": { "runtime": "claude", "model": "sonnet", "systemPrompt": prompt } }
    }))
    .expect("row");
    let args = ClaudeAsk { agent: row.agents["cc"].clone(), telemetry: None, history: None }.args();
    let program = 260;
    let line: usize = program + args.iter().map(|a| a.encode_utf16().count() + 3 + a.matches('"').count()).sum::<usize>();
    assert!(line < 32_767, "the command line is {line} units");
}

#[tokio::test]
async fn a_spawned_runner_works_in_the_checkout() {
    if !git_present() {
        return;
    }
    let dir = scratch("cwd");
    git_at(&dir, &["init", "-q"]);
    let args = vec!["rev-parse".to_string(), "--show-toplevel".to_string()];
    let out = spawn_and_collect(Path::new("git"), &dir, &args, &[], 30_000, "").await.expect("git");
    let shown = PathBuf::from(out.trim()).canonicalize().expect("toplevel");
    assert_eq!(shown, dir.canonicalize().expect("dir"), "the child's working directory is the checkout");
}

/// An openai-style provider answering each request with the next of `bodies`, handing
/// back what it was sent.
async fn provider_answering(bodies: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}/v1", listener.local_addr().expect("addr"));
    let task = tokio::spawn(async move {
        let mut sent = Vec::new();
        for body in bodies {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let received = loop {
                let n = socket.read(&mut chunk).await.expect("read");
                assert!(n > 0, "the request ended early");
                buf.extend_from_slice(&chunk[..n]);
                let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    break String::from_utf8_lossy(&buf[head_end + 4..]).into_owned();
                }
            };
            sent.push(received);
            // The chat runtime asks with stream: true, so the wire answers
            // event frames: the text as one delta, the finish with usage,
            // then DONE. An empty text stays empty (the delta is skipped).
            let content = body["choices"][0]["message"]["content"].as_str().unwrap_or("");
            let finish = body["choices"][0]["finish_reason"].as_str().unwrap_or("stop");
            let frames = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({ "id": "c-1", "object": "chat.completion.chunk", "model": "m",
                        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": content } }] }),
                json!({ "id": "c-1", "object": "chat.completion.chunk", "model": "m",
                        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
                        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 } })
            );
            let answer = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{frames}",
                frames.len()
            );
            socket.write_all(answer.as_bytes()).await.expect("write");
        }
        sent
    });
    (url, task)
}

fn completion(content: &str, finish: &str) -> Value {
    json!({ "id": "c-1", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": finish }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 } })
}

#[tokio::test]
async fn a_chat_answer_with_no_text_is_asked_once_more() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("empty-answer");
    let (service, project) = service_with_project("empty-answer", &repo_dir).await;
    // A real first run records the baseline (a dry run records nothing).
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![not_json_reply()]), &Stop::default()).await;
    service
        .sync_finish(&baseline.run, baseline.advance.as_ref())
        .await
        .expect("finish");
    std::fs::write(repo_dir.join("e.txt"), "five\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "second: five"]);
    // A thinking model that spent its budget thinking answers with no text at all, cut.
    let proposal = json!({ "summary": "one note", "domains": [], "updates": [], "marks": [],
                           "notes": [{ "kind": "knowledge", "title": "e.txt holds five", "content": "The second commit adds e.txt." }] });
    let (url, server) = provider_answering(vec![completion("", "length"), completion(&proposal.to_string(), "stop")]).await;
    let llm = LlmState::new();
    llm.apply(
        swiss_llm::LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": url, "models": [{ "id": "m" }] } },
            "agents": { "c": { "runtime": "chat", "provider": "local" } }
        }))
        .expect("row"),
    );
    let ask = runner(&llm, "c").expect("runner");
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &quiet_config(), ask.as_ref(), &Stop::default()).await;
    assert_eq!(done.run.state, RunState::Ok, "{}", done.run.error.clone().unwrap_or_default());
    assert_eq!(done.run.runner.attempts, 2);
    let sent = server.await.expect("server");
    assert!(sent[1].contains("not a valid proposal") && sent[1].contains("token limit"), "the retry says why");
}

#[tokio::test]
async fn a_builtin_answer_cut_while_thinking_with_nothing_filed_fails_the_run() {
    if !git_present() {
        return;
    }
    let repo_dir = repo("cut-builtin");
    let (service, project) = service_with_project("cut-builtin", &repo_dir).await;
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let baseline = execute_run(&service, run, &quiet_config(), &Canned::new(vec![not_json_reply()]), &Stop::default()).await;
    service
        .sync_finish(&baseline.run, baseline.advance.as_ref())
        .await
        .expect("finish");
    std::fs::write(repo_dir.join("f.txt"), "six\n").expect("write");
    git_at(&repo_dir, &["add", "."]);
    git_at(&repo_dir, &["commit", "-qm", "third: six"]);
    // The loop's closing round spent the whole answer thinking: no text, no tool call, cut.
    let (url, server) = provider_answering(vec![completion("", "length")]).await;
    let llm = LlmState::new();
    llm.apply(
        swiss_llm::LlmConfig::parse(&json!({
            "providers": { "local": { "style": "openai", "baseUrl": url, "models": [{ "id": "m" }] } },
            "agents": { "b": { "provider": "local", "maxTokens": 4096 } }
        }))
        .expect("row"),
    );
    let ask = runner(&llm, "b").expect("runner");
    let (run, _) = service.sync_enqueue(&project, manual_enqueue(false, None)).await.expect("enqueue");
    let done = execute_run(&service, run, &quiet_config(), ask.as_ref(), &Stop::default()).await;
    let why = done.run.error.clone().unwrap_or_default();
    assert_eq!(done.run.state, RunState::Failed, "{why}");
    assert!(why.contains("4096-token cap (maxTokens)") && why.contains("nothing was filed"), "{why}");
    // The commit was not read: the cursor stays and it waits for the next run.
    assert!(done.advance.is_none(), "a failed run moves nothing");
    assert_eq!(server.await.expect("server").len(), 1, "one round, no retry");
}
