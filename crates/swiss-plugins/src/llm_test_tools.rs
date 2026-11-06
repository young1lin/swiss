// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

//! The panel Test exercises the builtin catalog through the production implementations.
//! Proposal operations stay in MemoryTools' local buffer: no memory engine applies them.
//! Delegation uses a real, single-request worker on the tested agent's model, with no
//! recursive tools. File tools (read, write, edit, glob, grep) stay inside the selected
//! directory under the user's home; the shell starts there and can inspect its repository.

use std::path::{Path, PathBuf};
use async_trait::async_trait;
use serde_json::Value;
use swiss_llm::agent::{AgentConfig, Effort, ToolAnswer, ToolSpec, Toolbox};
use swiss_llm::provider::Chat;
use swiss_llm::telemetry::Session;
use crate::llm_tools::FsTools;
use crate::memory_agent::{answer_once, LeadTools, MemoryTools};
use crate::memory_sync::Ask;
use crate::llm_shell::ShellTools;

pub(crate) struct TestTools<'a> {
    root: PathBuf,
    taken: Vec<&'static str>,
    files: FsTools,
    memory: MemoryTools,
    worker: TestWorker<'a>,
    shell: ShellTools,
}

impl<'a> TestTools<'a> {
    pub(crate) fn new(root: &Path, taken: &[&'static str], agent: &'a AgentConfig, chat: &'a Chat, session: &'a Session, shell: ShellTools) -> Self {
        Self {
            root: root.to_path_buf(), taken: taken.to_vec(),
            files: FsTools::new(root, taken), memory: MemoryTools::new(root, taken), worker: TestWorker { agent, chat, session }, shell,
        }
    }
}

/// Shared with the inspector; constructing definitions performs no file, shell or model calls.
pub(crate) fn specs(root: &Path, taken: &[&'static str], shell: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut specs: Vec<ToolSpec> = MemoryTools::new(root, taken).specs().into_iter().filter(|s| s.name != "bash").collect();
    specs.extend(crate::memory_agent::lead_specs(taken, &[]).into_iter().filter(|s| s.name == "delegate"));
    specs.extend(FsTools::new(root, taken).specs());
    specs.extend(shell);
    let catalog = swiss_llm::agent::Runtime::Builtin.tools();
    // The shell's model-facing name (powershell on Windows) sorts at its key's place.
    specs.sort_by_key(|s| {
        let name = if s.name == crate::llm_shell::TOOL { "bash" } else { s.name.as_str() };
        catalog.iter().position(|t| t.name == name).unwrap_or(usize::MAX)
    });
    specs
}

#[async_trait]
impl Toolbox for TestTools<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        specs(&self.root, &self.taken, self.shell.specs())
    }

    async fn call(&mut self, name: &str, args: &Value) -> ToolAnswer {
        match name {
            // "bash" on Windows reaches the shell too, which refuses it: never memory's read-only git.
            "bash" | "powershell" | "job_output" | "job_kill" => self.shell.call(name, args).await,
            name if crate::llm_tools::FsTools::NAMES.contains(&name) => self.files.call(name, args).await,
            "delegate" => LeadTools::new(&self.root, &self.taken, Some(&self.worker)).call(name, args).await,
            _ => self.memory.call(name, args).await,
        }
    }
}

struct TestWorker<'a> {
    agent: &'a AgentConfig,
    chat: &'a Chat,
    session: &'a Session,
}

#[async_trait]
impl Ask for TestWorker<'_> {
    async fn ask(&self, _root: &Path, prompt: &str) -> Result<String, String> {
        self.task(prompt, None).await
    }

    async fn fill(&self, _root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.task(prompt, None).await
    }

    async fn task(&self, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        let chat = match effort {
            Some(effort) => self.chat.clone().with_effort(Some(effort.reasoning())),
            None => self.chat.clone(),
        };
        Ok(answer_once(self.agent, &chat, self.session, "chat", prompt).await?.text)
    }

    fn label(&self) -> String { format!("test:{}", self.agent.name) }
    fn model(&self) -> Option<String> { Some(self.chat.model.clone()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use swiss_llm::{LlmConfig, LlmState};

    #[tokio::test]
    async fn the_test_dispatches_real_file_git_proposal_and_worker_tools() {
        let heard = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = heard.clone();
        let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            sink.lock().expect("requests").push(body);
            async {
                let chunk = json!({
                    "id": "worker", "object": "chat.completion.chunk", "model": "tiny",
                    "choices": [{"index": 0, "delta": {"role": "assistant", "content": "worker checked the file"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 4, "completion_tokens": 3, "total_tokens": 7}
                });
                ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve"); });
        let state = LlmState::new();
        state.apply(LlmConfig::parse(&json!({
            "providers": {"local": {"style": "openai", "baseUrl": base, "models": [{"id": "tiny"}]}},
            "agents": {"test": {"provider": "local"}}
        })).expect("config"));
        let agent = state.agent("test").expect("agent");
        let chat = agent.chat(&state.config().providers).expect("chat");
        let session = state.session(&agent, "llm.test");
        let root = swiss_core::paths::test_home().join(format!("test-tools-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&root).expect("dir");
        std::fs::write(root.join("notes.txt"), "worker evidence").expect("file");
        let taken: Vec<&str> = swiss_llm::agent::Runtime::Builtin.tools().iter().map(|t| t.name).collect();
        let services = swiss_host::services::RuntimeServices::new(swiss_host::services::BootInfo::default());
        crate::llm_shell::register(&services).expect("shell action");
        let shell = ShellTools::new(&root, &taken, &services, crate::llm_shell::scope("test", Some(1)), None);
        let mut tools = TestTools::new(&root, &taken, &agent, &chat, &session, shell);
        let specs = tools.specs();
        let shell_tool = crate::llm_shell::TOOL;
        let bash = specs.iter().find(|s| s.name == shell_tool).expect("shell tool");
        for field in ["workdir", "timeoutMs", "run_in_background"] {
            assert!(bash.schema["properties"].get(field).is_some(), "the shell must expose {field} to the model");
        }
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.iter().position(|n| *n == shell_tool), Some(5), "the shell keeps bash's catalog place: {names:?}");
        assert_eq!(names[..5], ["read", "write", "edit", "glob", "grep"], "the file tools lead, in catalog order");
        assert!(!names.contains(&"bash") || shell_tool == "bash", "no second shell under bash's name: {names:?}");
        assert_eq!(tools.call("read", &json!({"path": "notes.txt"})).await.text, "worker evidence");
        assert!(tools.call("glob", &json!({"pattern": "*.txt"})).await.text.contains("notes.txt"));
        assert!(tools.call("grep", &json!({"pattern": "evidence"})).await.text.contains("notes.txt"));
        assert!(tools.call("write", &json!({"path": "made.txt", "content": "made"})).await.ok);
        assert!(tools.call("edit", &json!({"path": "made.txt", "old_string": "made", "new_string": "edited"})).await.ok);
        assert_eq!(tools.call("read", &json!({"path": "made.txt"})).await.text, "edited");
        assert!(tools.call(shell_tool, &json!({"command": "git --version"})).await.text.contains("git version"));
        let upper = if cfg!(windows) { "'shell'.ToUpper()" } else { "printf shell | tr a-z A-Z" };
        assert!(tools.call(shell_tool, &json!({"command": upper})).await.text.contains("SHELL"));
        if cfg!(windows) { assert!(!tools.call("bash", &json!({"command": "git log"})).await.ok, "bash names no tool on Windows"); }
        assert!(!tools.call("sync", &json!({"invalid": {}})).await.ok);
        assert!(tools.call("sync", &json!({"note": {"kind": "knowledge", "title": "test", "content": "test only"}})).await.ok);
        let reply = tools.call("delegate", &json!({"tasks": [{"task": "Check the evidence", "paths": ["notes.txt"], "effort": "low"}]})).await;
        assert!(reply.ok && reply.text.contains("worker checked the file"), "{}", reply.text);
        {
            let asks = heard.lock().expect("requests");
            assert_eq!(asks.len(), 1, "delegate actually asked a worker once");
            assert!(asks[0].get("tools").is_none(), "workers cannot recursively delegate");
            assert!(asks[0].to_string().contains("worker evidence"));
        }
        let shell = ShellTools::new(&root, &["read"], &services, crate::llm_shell::scope("test", Some(1)), None);
        let mut narrowed = TestTools::new(&root, &["read"], &agent, &chat, &session, shell);
        for name in ["bash", "powershell", "sync", "delegate", "write", "edit", "glob", "grep", "list"] {
            assert!(!narrowed.call(name, &json!({})).await.ok, "{name} was not selected");
        }
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }
}
