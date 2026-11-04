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

//! The runners a question is asked of (SPEC §memory.sync, §llm.agents): the [Ask] seam
//! and the llm plugin's agents behind it by runtime - one chat request, a spawned
//! `claude`, a spawned `pi` - each spawned one seated as a gateway child.

use super::*;

/// One question to the runner: the prompt in, its stdout out. The retry note is
/// appended by the caller on an invalid first answer.
#[async_trait]
pub trait Ask: Send + Sync {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String>;
    /// One area's question of a map run (SPEC §memory.map): no tools - the material is
    /// in the prompt - and the answer pinned to `schema` where the runtime can pin one.
    async fn fill(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String>;
    /// How many of a run's questions (a map's areas, an init's slices) may be out at
    /// once: the agent's `concurrency` (SPEC §llm.agents).
    fn fills_at_once(&self) -> usize {
        1
    }
    /// The most one of its requests carries, in bytes: a builtin or chat agent's
    /// `maxContextKiB` (SPEC §llm.agents); a sync run sizes its prompt by it
    /// ([engine::prompt_budget]). None for a spawned runtime, which has no cap of swiss's.
    fn context_bytes(&self) -> Option<usize> {
        None
    }
    /// A lead's question (SPEC §memory.init Lead): the review of the top level, a domain's
    /// wrap-up. A builtin lead works it in its own loop with read and delegate - pieces
    /// handed to `workers`, each at the effort it picks; any other runtime answers it as
    /// it answers a fill.
    async fn lead(&self, root: &Path, prompt: &str, schema: &Value, _workers: Option<&dyn Ask>) -> Result<String, String> {
        self.fill(root, prompt, schema).await
    }
    /// Whether a lead may hand this runner a piece of its work ([Ask::task]): a runtime
    /// that asks a provider.
    fn takes_tasks(&self) -> bool {
        false
    }
    /// One piece of a lead's work: the answer as plain text, asked at `effort` over the
    /// agent's own. Only a runner that [takes tasks](Ask::takes_tasks) is handed one.
    async fn task(&self, _prompt: &str, _effort: Option<Effort>) -> Result<String, String> {
        Err(format!("{} takes no delegated task", self.label()))
    }
    /// The workers a lead may name for one task ([Ask::task_on]): a [Pool]'s members that
    /// take tasks. A lone runner names none.
    fn roster(&self) -> Vec<Worker> {
        Vec::new()
    }
    /// One task for the worker of the [roster](Ask::roster) named `agent`; None - or a
    /// runner with no roster - asks it as [Ask::task] does.
    async fn task_on(&self, _agent: Option<&str>, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        self.task(prompt, effort).await
    }
    /// A recheck's question (SPEC §memory.init The recheck), worked with the tools to look
    /// at the code where the runtime has them: a builtin agent opens files and searches
    /// the checkout in its own loop, a spawned one with its read-only file tools. Its last
    /// message is the answer. A runner without tools answers it as a fill.
    async fn look(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        self.fill(root, prompt, schema).await
    }
    /// Whether [Ask::look] really looks: a recheck then shows less code a question, says
    /// the rest can be opened, and takes a verdict on a note whose files it did not show.
    fn looks(&self) -> bool {
        false
    }
    /// A memory question (SPEC §memory.rag), worked with the project's read-only
    /// memory tools where the runtime can loop: a builtin agent searches, walks and
    /// reads in its own bounded loop, and its last message is the answer. A runtime
    /// that cannot loop never comes here - the caller hands it the gateway's
    /// retrieval inlined and asks once, over [Ask::rags].
    async fn rag(&self, prompt: &str, tools: &mut dyn swiss_llm::agent::Toolbox) -> Result<String, String> {
        let _ = (prompt, tools);
        Err(format!("{} works a memory question as one ask", self.label()))
    }
    /// Whether [Ask::rag] really loops over the tools.
    fn rags(&self) -> bool {
        false
    }
    /// The checkout tools [Ask::rag] takes beside the memory ones (SPEC §memory.rag): the
    /// agent's share of read and bash, to check what memory says against the code. A runner
    /// that does not loop takes none.
    fn rag_looks(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// The [RunnerInfo] label: "<runtime>:<agent>" ("builtin:distiller").
    fn label(&self) -> String;
    fn model(&self) -> Option<String>;
}

/// The runner a named llm agent becomes, by its runtime; Err is the sentence a failed
/// run (and the sync sheet) shows - the plugin off, no such agent, a provider gone.
pub fn runner(llm: &LlmState, name: &str) -> Result<Box<dyn Ask>, String> {
    let agent = llm.agent(name)?;
    let config = llm.config();
    Ok(match agent.runtime {
        Runtime::Builtin => {
            let chat = agent
                .chat(&config.providers)?
                .recording(llm.history(), Some(&agent.name));
            let session = llm.session(&agent, CONSUMER);
            Box::new(BuiltinAsk::new(agent, chat, session))
        }
        Runtime::Chat => {
            let chat = agent
                .chat(&config.providers)?
                .recording(llm.history(), Some(&agent.name));
            let session = llm.session(&agent, CONSUMER);
            Box::new(ChatAsk { agent, chat, session })
        }
        Runtime::Claude => Box::new(ClaudeAsk { agent, telemetry: config.telemetry.clone(), history: llm.history() }),
        Runtime::Pi => Box::new(PiAsk { agent, history: llm.history() }),
        Runtime::Db => return Err(format!("{name} is a DB agent: it serves the Data page alone; name a task agent here")),
    })
}

/// What the memory task offers a spawned runtime, in its own spelling: the read-only file
/// tools. The agent's `tools` narrows the offer; nothing widens it.
pub(super) const CLAUDE_OFFER: [&str; 3] = ["Read", "Grep", "Glob"];
pub(super) const PI_OFFER: [&str; 4] = ["read", "grep", "find", "ls"];

/// The section a runner appends to the engine's prompt: where the agent stands and the
/// tools this run hands it (the task's offer, narrowed by the agent).
pub(super) fn this_run(tools: &[&str]) -> String {
    if tools.is_empty() {
        "\n## This run\nYou have no tools this run: judge from the material above alone.\n".into()
    } else {
        format!("\n## This run\nYour working directory is the checkout. Your tools: {}.\n", tools.join(", "))
    }
}

/// The `chat` runtime: one request to the agent's provider under its system prompt, no
/// tools. Its answer is wrapped in the claude result shape so [engine::read_reply] reads
/// it unchanged.
pub struct ChatAsk {
    pub(super) agent: AgentConfig,
    pub(super) chat: Chat,
    pub(super) session: Session,
}

#[async_trait]
impl Ask for ChatAsk {
    async fn ask(&self, _root: &Path, prompt: &str) -> Result<String, String> {
        let prompt = format!("{prompt}{}", this_run(&[]));
        // A chat runtime's whole run is one ask. An empty or cut answer is the reader's to
        // judge: it is asked once more, like any invalid one.
        let answered = answer_once(&self.agent, &self.chat, &self.session, "chat", &prompt).await?;
        // The provider's text is the answer the way `claude -p` would carry a text result.
        Ok(text_result(&answered).to_string())
    }

    async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.ask(root, prompt).await
    }

    fn fills_at_once(&self) -> usize {
        self.agent.concurrency as usize
    }

    fn context_bytes(&self) -> Option<usize> {
        Some(self.chat.context_cap())
    }

    fn takes_tasks(&self) -> bool {
        true
    }

    /// A worker's task: one request, at the effort the lead picked over the agent's own.
    async fn task(&self, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        let chat = match effort {
            Some(effort) => self.chat.clone().with_effort(Some(effort.reasoning())),
            None => self.chat.clone(),
        };
        Ok(answer_once(&self.agent, &chat, &self.session, "chat", prompt).await?.text)
    }

    fn label(&self) -> String {
        format!("chat:{}", self.agent.name)
    }

    fn model(&self) -> Option<String> {
        Some(self.chat.model.clone())
    }
}

/// The run record of a spawned runtime (`claude`, `pi`): the task fed on stdin and
/// the collected stdout are the whole dialogue - a user turn and a final one around
/// whatever the child did in between (SPEC llm.history: every agent conversation
/// files its run, spawned runtimes included). Without a history the notes no-op.
pub(super) async fn run_recorded(
    history: &Option<std::sync::Arc<swiss_llm::history::LlmHistory>>,
    agent: &AgentConfig,
    runtime: &str,
    prompt: &str,
    ask: impl std::future::Future<Output = Result<String, String>>,
) -> Result<String, String> {
    let mut run = match history {
        Some(history) => history.run_recorder(&agent.name, runtime, None, agent.model.as_deref()),
        None => swiss_llm::history::RunRecorder::none(),
    };
    // Reserved before the child starts, so the run lists live while its process runs
    // (SPEC llm.history Live); a refused reservation still files the run at its end.
    run.reserve().await;
    run.note("user", prompt);
    match ask.await {
        Ok(answer) => {
            run.note("final", &answer);
            run.finish(true);
            Ok(answer)
        }
        Err(err) => {
            run.fail(&err);
            Err(err)
        }
    }
}

/// The spawned `claude` per the SPEC's flag list, in the checkout: restricted to the
/// agent's share of Read/Grep/Glob, no session persistence, JSON out, the proposal schema
/// pinned, the agent's system prompt appended, the prompt on stdin. A reporting agent's
/// child gets the OTEL env and exports its own events.
pub struct ClaudeAsk {
    pub(super) agent: AgentConfig,
    pub(super) telemetry: Option<TelemetryConfig>,
    pub(super) history: Option<std::sync::Arc<swiss_llm::history::LlmHistory>>,
}

impl ClaudeAsk {
    pub(super) fn args(&self) -> Vec<String> {
        self.args_for(&self.agent.narrow(&CLAUDE_OFFER), &engine::proposal_schema())
    }

    fn args_for(&self, tools: &[&str], schema: &Value) -> Vec<String> {
        let tools = tools.join(",");
        // An empty --tools disables every tool; --allowedTools then has nothing to allow.
        let mut args: Vec<String> = vec!["-p".into(), "--restricted".into(), "--tools".into(), tools.clone()];
        if !tools.is_empty() {
            args.extend(["--allowedTools".into(), tools]);
        }
        args.extend([
            "--strict-mcp-config".into(),
            "--no-session-persistence".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--output-format".into(),
            "json".into(),
            // --json-schema carries the schema itself (Claude Code 2.1.x parses the
            // argument as JSON); an earlier draft passed a file path and the CLI refused it.
            "--json-schema".into(),
            schema.to_string(),
        ]);
        if let Some(model) = &self.agent.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(extra) = self.agent.system_prompt() {
            args.extend(["--append-system-prompt".into(), extra.into_owned()]);
        }
        args
    }
}

#[async_trait]
impl Ask for ClaudeAsk {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
        let program = self.agent.program()?;
        let env = self.agent.child_env(self.telemetry.as_ref())?;
        let prompt = format!("{prompt}{}", this_run(&self.agent.narrow(&CLAUDE_OFFER)));
        let args = self.args();
        let ask = spawn_and_collect(&program, root, &args, &env, &prompt);
        run_recorded(&self.history, &self.agent, "claude", &prompt, ask).await
    }

    async fn fill(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        let program = self.agent.program()?;
        let env = self.agent.child_env(self.telemetry.as_ref())?;
        let prompt = format!("{prompt}{}", this_run(&[]));
        let args = self.args_for(&[], schema);
        let ask = spawn_and_collect(&program, root, &args, &env, &prompt);
        run_recorded(&self.history, &self.agent, "claude", &prompt, ask).await
    }

    /// A recheck's question with the agent's share of Read/Grep/Glob, the answer pinned
    /// to `schema`.
    async fn look(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        let program = self.agent.program()?;
        let env = self.agent.child_env(self.telemetry.as_ref())?;
        let tools = self.agent.narrow(&CLAUDE_OFFER);
        let prompt = format!("{prompt}{}", this_run(&tools));
        let args = self.args_for(&tools, schema);
        let ask = spawn_and_collect(&program, root, &args, &env, &prompt);
        run_recorded(&self.history, &self.agent, "claude", &prompt, ask).await
    }

    fn looks(&self) -> bool {
        !self.agent.narrow(&CLAUDE_OFFER).is_empty()
    }

    fn fills_at_once(&self) -> usize {
        self.agent.concurrency as usize
    }

    fn label(&self) -> String {
        format!("claude:{}", self.agent.name)
    }

    fn model(&self) -> Option<String> {
        self.agent.model.clone()
    }
}

/// The children registry seat for the sync runner's spawns (SPEC §process.children):
/// set once by the memory plugin's start, cleared for a fresh test composition.
/// Unset (the offline CLI, unit tests) simply means no Process-page row - the
/// platform guards below still apply.
pub(super) static CHILDREN: std::sync::RwLock<Option<std::sync::Arc<swiss_host::services::children::ChildRegistry>>> =
    std::sync::RwLock::new(None);

/// Seat the registry the plugin's start owns. Overwrites so an enable cycle
/// re-seats the same long-lived registry, and a test can reset it.
pub fn set_children_seat(registry: std::sync::Arc<swiss_host::services::children::ChildRegistry>) {
    *CHILDREN.write().unwrap_or_else(|e| e.into_inner()) = Some(registry);
}


/// Spawn a runner child in the checkout `root` with `args` and `env` (references already
/// resolved), feed `prompt` on stdin and collect its stdout (capped at
/// [RUNNER_STDOUT_MAX]) and a stderr clip; shared by the spawned runtimes (`claude`,
/// `pi`). The working directory is what their file tools read relative to - and what
/// `claude --restricted` confines them to.
///
/// The runner is a gateway child like every other (SPEC §security.rules): a job
/// object (Windows) or private process group (Unix) owns its subtree, the PID
/// ledger carries it until its death is confirmed, and a seated registry shows
/// the row on the Process page - stoppable there, killed with the gateway.
///
/// No time cap: the runner runs as long as its work takes. The sync's Stop drops this
/// future (`kill_on_drop` ends the child) and the Process-page row kills its tree.
pub(super) async fn spawn_and_collect(
    program: &Path,
    root: &Path,
    args: &[String],
    env: &[(String, String)],
    prompt: &str,
) -> Result<String, String> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = tokio::process::Command::new(program);
    command
        .args(&arg_refs)
        .current_dir(root)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env("SWISS_MEMORY_SYNC", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in env {
        command.env(name, value);
    }
    #[cfg(windows)]
    {
        // The daemon has no console; without this the runner flashes one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        // Private process group (pgid == the child's pid): the Unix counterpart
        // of the job object below - one kill(-pgid) reaches descendants at any
        // depth, and the runner never catches signals aimed at the gateway.
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("the sync runner {program:?} could not start: {err}"))?;
    let pid = child.id();
    // The subtree guard, assigned at spawn: everything the runner goes on to
    // create inherits the job, so a hard-killed gateway - the one path
    // kill_on_drop cannot run on - still ends the whole tree when the handle
    // closes (SPEC §process.tree).
    #[cfg(windows)]
    let _job = child.raw_handle().and_then(swiss_core::platform::KillOnCloseJob::assign);
    // The PID ledger: a gateway that dies mid-run leaves the runner to the next
    // boot's reap. Dropped only after the child's death is confirmed below -
    // a note outliving its pid is a reuse hazard for that reap.
    if let Some(pid) = pid {
        swiss_host::proc_pids::note_proc_pid(pid);
    }
    // The Process-page row, while the child lives (SPEC §process.children):
    // owner memory, stop is a tree kill - the runner has no richer lifecycle.
    let _child_row = pid
        .zip(CHILDREN.read().unwrap_or_else(|e| e.into_inner()).clone())
        .map(|(p, children)| {
            children.register(swiss_host::services::children::ChildSpec {
                pid: p,
                owner: "memory".to_string(),
                kind: swiss_host::services::children::ChildKind::Agent,
                label: program
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| program.to_string_lossy().into_owned()),
                stop: Some(swiss_host::services::children::child_stop_fn(move || {
                    let pid = p;
                    async move {
                        let killed = swiss_core::platform::tree_kill(pid);
                        if killed.is_empty() {
                            Err(format!("pid {pid} was not killed - already gone, or not ours"))
                        } else {
                            Ok(())
                        }
                    }
                })),
            })
        });
    let unnote = |pid: Option<u32>| {
        if let Some(pid) = pid {
            swiss_host::proc_pids::drop_proc_pid(pid);
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
            let prompt = prompt.to_string();
            // Feeding then dropping closes the pipe: the runner reads its question to EOF.
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(prompt.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        let read = async {
            use tokio::io::AsyncReadExt;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let (out_pipe, err_pipe) = (child.stdout.take(), child.stderr.take());
            let read_out = async {
                if let Some(mut pipe) = out_pipe {
                    let mut chunk = [0u8; 8192];
                    // Past the cap the bytes are drained, not stored: closing the
                    // read end early once killed a chatty runner with EPIPE long
                    // before its final - the useful - line arrived.
                    while let Ok(n) = pipe.read(&mut chunk).await {
                        if n == 0 {
                            break;
                        }
                        if stdout.len() < RUNNER_STDOUT_MAX {
                            let room = RUNNER_STDOUT_MAX - stdout.len();
                            let take = room.min(n);
                            stdout.extend_from_slice(&chunk[..take]);
                        }
                    }
                }
            };
            let read_err = async {
                if let Some(mut pipe) = err_pipe {
                    let mut chunk = [0u8; 8192];
                    // Same drain-past-cap discipline as stdout.
                    while let Ok(n) = pipe.read(&mut chunk).await {
                        if n == 0 {
                            break;
                        }
                        if stderr.len() < 4096 {
                            let room = 4096 - stderr.len();
                            let take = room.min(n);
                            stderr.extend_from_slice(&chunk[..take]);
                        }
                    }
                }
            };
            let ((), ()) = tokio::join!(read_out, read_err);
            let status = child.wait().await;
            (stdout, stderr, status)
        };
        // No clock bounds the read: a runner works as long as its work takes.
        let (stdout, stderr, status) = read.await;
        // The wait answered: the child is dead, the note can go.
        unnote(pid);
        let status = status.map_err(|err| format!("the sync runner did not finish: {err}"))?;
        let text = String::from_utf8_lossy(&stdout).into_owned();
        if !status.success() {
            let why = String::from_utf8_lossy(&stderr).trim().chars().take(300).collect::<String>();
            return Err(format!("the sync runner exited with {}: {why}", status.code().unwrap_or(-1)));
        }
        Ok(text)
}

/// A provider's answer in the claude result shape; a cut one says so, for the reader.
pub fn text_result(answered: &provider::Answered) -> Value {
    let mut result = json!({ "type": "result", "subtype": "success", "result": answered.text });
    if answered.cut {
        result["stop_reason"] = json!("max_tokens");
    }
    result
}

/// The `pi` runtime: a spawned `pi` (pi-coding-agent) in print mode, in the checkout -
/// the agent's share of read/grep/find/ls, no session, the agent's system prompt
/// appended, the prompt on stdin. The model comes as `provider/model` from its
/// `~/.pi/agent/models.json`. pi has no telemetry hook, so a pi agent never reports.
pub struct PiAsk {
    pub(super) agent: AgentConfig,
    pub(super) history: Option<std::sync::Arc<swiss_llm::history::LlmHistory>>,
}

impl PiAsk {
    #[cfg(test)]
    pub(super) fn args(&self) -> Vec<String> {
        self.args_for(&self.agent.narrow(&PI_OFFER))
    }

    fn args_for(&self, tools: &[&str]) -> Vec<String> {
        let model = self.agent.model.clone().unwrap_or_default();
        // `--mode text`: the print mode's stdout is the final answer alone. The
        // json mode was measured first and lost: every delta event repeats the
        // whole partial message, so a thinking answer quadratically blows past
        // any read cap before its last line - the useful one - is ever written.
        let mut args: Vec<String> = ["-p", "--mode", "text", "--no-session"].into_iter().map(String::from).collect();
        if tools.is_empty() {
            args.push("--no-tools".into());
        } else {
            args.extend(["--tools".into(), tools.join(",")]);
        }
        args.extend(["--model".into(), model]);
        if let Some(extra) = self.agent.system_prompt() {
            args.extend(["--append-system-prompt".into(), extra.into_owned()]);
        }
        args
    }
}

impl PiAsk {
    async fn spawn(&self, root: &Path, prompt: &str, tools: &[&str]) -> Result<String, String> {
        let program = self.agent.program()?;
        let env = self.agent.child_env(None)?;
        let prompt = format!("{prompt}{}", this_run(tools));
        let spawn = async {
            let answer = spawn_and_collect(&program, root, &self.args_for(tools), &env, &prompt).await?;
            if answer.trim().is_empty() {
                return Err(format!(
                    "the {} agent's pi produced no text; check its command, its model and pi's models.json",
                    self.agent.name
                ));
            }
            // The claude result shape, so the engine's reader is shared.
            Ok(json!({ "type": "result", "subtype": "success", "result": answer }).to_string())
        };
        run_recorded(&self.history, &self.agent, "pi", &prompt, spawn).await
    }
}

#[async_trait]
impl Ask for PiAsk {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
        self.spawn(root, prompt, &self.agent.narrow(&PI_OFFER)).await
    }

    async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.spawn(root, prompt, &[]).await
    }

    async fn look(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
        self.spawn(root, prompt, &self.agent.narrow(&PI_OFFER)).await
    }

    fn looks(&self) -> bool {
        !self.agent.narrow(&PI_OFFER).is_empty()
    }

    fn fills_at_once(&self) -> usize {
        self.agent.concurrency as usize
    }

    fn label(&self) -> String {
        format!("pi:{}", self.agent.name)
    }

    fn model(&self) -> Option<String> {
        self.agent.model.clone()
    }
}
