//! The proc adapter — port of `adapters/proc.ts`, plugged into `adapters/proxy.rs`'s
//! `ProxyServer` through the [`ChildRemote`] implementation of the `RemoteMcp` seam.
//!
//! Hosts an arbitrary stdio MCP server (spawned via npx/uvx/python/...) by spawning it once and
//! proxying every MCP request to it over stdio. One shared child per MCP: the per-request
//! proxy servers all wrap the same child client, which multiplexes concurrent requests by
//! JSON-RPC id.
//!
//! Subtree teardown on Windows is a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` —
//! strictly better than the Node build's `taskkill /T /F`: it also covers a hard kill of the
//! gateway itself (the OS closes the job handle with the process). The boot-time ledger
//! ([`crate::proc_pids`]) stays as the backstop for whatever that misses. All FFI for the job
//! lives in the [`win`] module below and is the only `unsafe` in this file.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo,
    ClientRequest, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, PingRequest,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ServerResult,
};
use rmcp::service::{Peer, PeerRequestOptions, RunningService};
use rmcp::{serve_client, ClientHandler, RoleClient};

use swiss_host::config::ServerDef;

/// Cap on retained child stderr. Enough to diagnose a failed launch, small enough to ignore.
/// Counted in chars, mirroring the JS `.length`/`slice` the Node build bounded the ring with.
pub const STDERR_MAX: usize = 64 * 1024;

/// How long a subtree kill waits for the root to report its exit status before giving up —
/// TerminateProcess/SIGKILL cannot be ignored, this only bounds a wedged wait.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// `Number(process.env.X) || default` — a missing, non-numeric, zero or negative value all fall
/// back (JS: NaN/0 are falsy).
fn parse_env_ms(raw: Option<String>, default: u64) -> u64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as u64)
        .unwrap_or(default)
}

/// Deadline for the stdio initialize handshake. Generous — an npx/uvx cold start (a first
/// download) can take tens of seconds — but finite, so a child that spawns yet never speaks MCP
/// fails the start instead of wedging the MCP in "starting" forever. Override with
/// `PROC_HANDSHAKE_TIMEOUT_MS`.
fn handshake_timeout_ms() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| parse_env_ms(std::env::var("PROC_HANDSHAKE_TIMEOUT_MS").ok(), 60_000))
}

/// Deadline for one proxied tool call. The SDK's own default is 60s, which is under what a child
/// doing real work needs: vision/inference calls routinely run 20-50s and a large screenshot
/// runs past the minute, so that invisible default failed them after the model had already been
/// billed for the work. Finite, so a wedged child still fails the call instead of pinning the
/// request forever. Override globally with `PROC_CALL_TIMEOUT_MS`, or per-MCP with `"timeoutMs"`
/// in config.
pub fn proc_call_timeout_ms() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| parse_env_ms(std::env::var("PROC_CALL_TIMEOUT_MS").ok(), 180_000))
}

// tokenize_command and decode_child_output live in swiss_host::services::process (the process
// supervisor shares both with this adapter); re-exported so this stays their historical home
// for every existing caller. The unit tests for both moved along with the code.
#[cfg(windows)]
use swiss_core::platform::KillOnCloseJob;
pub use swiss_host::services::process::{decode_child_output, tokenize_command};

/// Keep the tail of the stderr ring, bounded by chars (Node: `slice(-STDERR_MAX)` on a buffer
/// measured in UTF-16 units — chars are the closest Rust unit).
fn cap_ring(buf: &mut String, max_chars: usize) {
    let len = buf.chars().count();
    if len > max_chars {
        let keep: String = buf.chars().skip(len - max_chars).collect();
        *buf = keep;
    }
}

/// The program file to spawn for a launch token: resolved through PATH+PATHEXT (which is how
/// the Node build's cross-spawn found `npx.cmd`) when possible, else the raw name — so an
/// unresolvable command still fails at spawn with the OS's own error naming the command, the
/// analogue of Node's `spawn ENOENT`.
fn resolve_program(cmd: &str, path: &str) -> String {
    match swiss_host::pathenv::resolve_command(cmd, path) {
        Some(resolved) => resolved.to_string_lossy().into_owned(),
        None => cmd.to_string(),
    }
}

#[cfg(windows)]
fn is_batch_file(program: &str) -> bool {
    std::path::Path::new(program)
        .extension()
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| e == "cmd" || e == "bat")
}

/// `%COMSPEC%` — the interpreter that knows how to run a batch file.
#[cfg(windows)]
fn cmd_exe() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
}

// --- cross-spawn's cmd.exe escaping (lib/util/escape.js + parse.js), ported verbatim -------------
// The naive "quote every token" this route used first breaks on real arguments: an argument
// carrying a quote or a trailing backslash corrupts the line cmd.exe re-parses, and an unescaped
// `%`/`!`/`&` is command-line injection into the shim. cross-spawn is the battle-tested
// reference (it is what the Node build spawned through), so its algorithm is the spec here.

/// The metacharacters cmd.exe treats specially (robvanderwoude.com/escapechars.php) — note the
/// space and the wildcards: `metaCharsRegExp` in escape.js.
#[cfg(windows)]
fn is_cmd_meta(c: char) -> bool {
    matches!(
        c,
        '(' | ')'
            | ']'
            | '['
            | '%'
            | '!'
            | '^'
            | '"'
            | '`'
            | '<'
            | '>'
            | '&'
            | '|'
            | ';'
            | ','
            | ' '
            | '*'
            | '?'
    )
}

/// escape.js `escape.command`: ^-escape every metachar, no quoting — a `^ ` keeps one token where
/// a quoted path would have its spaces re-interpreted when cmd re-parses the line.
#[cfg(windows)]
fn cmd_escape_program(program: &str) -> String {
    let mut out = String::with_capacity(program.len());
    for c in program.chars() {
        if is_cmd_meta(c) {
            out.push('^');
        }
        out.push(c);
    }
    out
}

/// escape.js `escape.argument` — the qntm.org/cmd rules, in the backtracking-free form of
/// cross-spawn PR #160: every backslash run that precedes a `"` (including the run at the end of
/// the string, which the closing quote below puts a quote after) doubles, every `"` gains a `\`,
/// the whole thing is wrapped in quotes, and then every metachar — those quotes included — is
/// ^-escaped. `double_escape` repeats that ^ pass for a node_modules/.bin cmd shim, whose inner
/// `node` call re-parses the line a second time (parse.js `isCmdShimRegExp`).
#[cfg(windows)]
fn cmd_escape_arg(arg: &str, double_escape: bool) -> String {
    let chars: Vec<char> = arg.chars().collect();
    let mut inner = String::with_capacity(arg.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let run = i - start;
                // A run is "before a quote" when the next char is one, or when there is no next
                // char — the wrapper below appends its closing quote right there.
                let before_quote = i == chars.len() || chars[i] == '"';
                let times = if before_quote { run * 2 } else { run };
                for _ in 0..times {
                    inner.push('\\');
                }
            }
            '"' => {
                inner.push('\\');
                inner.push('"');
                i += 1;
            }
            c => {
                inner.push(c);
                i += 1;
            }
        }
    }
    let mut out = format!("\"{inner}\"");
    for _ in 0..(1 + usize::from(double_escape)) {
        out = out
            .chars()
            .map(|c| {
                if is_cmd_meta(c) {
                    format!("^{c}")
                } else {
                    c.to_string()
                }
            })
            .collect();
    }
    out
}

/// parse.js `isCmdShimRegExp` (`node_modules[\\/].bin[\\/][^\\/]+\.cmd$`) — a .cmd file whose
/// name is the ONE segment directly inside a `node_modules/.bin` directory (the npm shim
/// generated for a package bin; hand-rolled per ADR-007).
#[cfg(windows)]
fn is_cmd_shim(program: &str) -> bool {
    let lower = program.to_ascii_lowercase().replace('/', "\\");
    const MARKER: &str = "\\node_modules\\.bin\\";
    let Some(at) = lower.find(MARKER) else {
        return false;
    };
    let rest = &lower[at + MARKER.len()..];
    // The segment after `.bin\` must be a single separator-free name ending in `.cmd`.
    !rest.is_empty() && !rest.contains('\\') && rest.ends_with(".cmd")
}

/// A spawned child awaiting (or holding) its MCP session: the process itself plus the pid the
/// ledger and the memory view know it by.
struct SpawnedChild {
    pid: u32,
    child: tokio::process::Child,
    /// The kill-on-close job holding the subtree. `None` when creation/assignment failed —
    /// kills then fall back to the platform tree-kill by PID.
    #[cfg(windows)]
    job: Option<KillOnCloseJob>,
}

impl SpawnedChild {
    /// Whether the root child has already exited on its own (Node's `childExited()` — the
    /// try_wait makes it exact, not a guess). An unreadable status reads as exited, which only
    /// ever skips a fallback PID kill — the safe direction.
    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Kill the whole subtree rooted at this child (Node's `treeKill`, replaced by the job).
    async fn kill_subtree(&mut self) {
        #[cfg(windows)]
        {
            if let Some(job) = self.job.take() {
                // Closing the kill-on-close job handle terminates every member — any depth,
                // PID-safe (no taskkill-style PID-reuse hazard) — and this runs even when the
                // root already exited: surviving grandchildren stay in the job, and the close
                // kills exactly them.
                drop(job);
                let _ = tokio::time::timeout(KILL_WAIT, self.child.wait()).await;
                return;
            }
        }
        // No job (creation/assignment failed, or off Windows): kill by PID as the Node build
        // did. Skip when the root already exited — its PID is free for the OS to hand to
        // something else, and a kill-by-PID would take the innocent process down with it.
        if !self.exited() {
            #[cfg(windows)]
            swiss_core::platform::tree_kill(self.pid);
            #[cfg(not(windows))]
            let _ = self.child.start_kill(); // kill(pid, SIGKILL) on the recorded root
        }
        let _ = tokio::time::timeout(KILL_WAIT, self.child.wait()).await;
    }
}

/// The client side the gateway presents to the child: `{ name: "mcp-gateway", version: "1.0" }`
/// with no capabilities (Node: `new Client({ name: "mcp-gateway", version: "1.0" }, {
/// capabilities: {} })`). A no-op handler otherwise — the proxy drives every request.
struct ProcClientHandler;

impl ClientHandler for ProcClientHandler {
    fn get_info(&self) -> ClientInfo {
        let mut client_info = Implementation::default();
        client_info.name = "mcp-gateway".into();
        client_info.version = "1.0".into();
        ClientInfo::new(ClientCapabilities::default(), client_info)
    }
}

/// The proxy settings for one child — port of `proxyOpts()`: build() and the per-request
/// factory must agree, and a field added to one copy but not the other silently changes
/// behavior after the first request.
#[derive(Clone)]
struct ProxyCfg {
    description: Option<String>,
    expose_resources: bool,
    expose_prompts: bool,
    call_timeout_ms: u64,
}

/// Literal `false` on the def hides; absent — or any other JSON value — exposes. The Node build
/// cast loosely (`def.exposeResources as boolean | undefined` then `!== false`), so a string
/// "false" from a hand-edited config still exposes; port that truth table exactly.
fn flag_or_exposed(def: &ServerDef, key: &str) -> bool {
    def.get(key) != Some(&Value::Bool(false))
}

fn proxy_cfg_from_def(def: &ServerDef) -> ProxyCfg {
    ProxyCfg {
        description: def.get_str("description").map(str::to_string),
        expose_resources: flag_or_exposed(def, "exposeResources"),
        expose_prompts: flag_or_exposed(def, "exposePrompts"),
        // `timeoutMs ?? PROC_CALL_TIMEOUT_MS` — raise the global for a child that does slow
        // work (image analysis, long scrapes); lower it for one that should always be quick.
        call_timeout_ms: def
            .get_number("timeoutMs")
            .map(|n| n.max(0.0) as u64)
            .unwrap_or_else(proc_call_timeout_ms),
    }
}

/// The child side of the `proxy::RemoteMcp` seam: every call goes to the ONE shared rmcp client
/// over the child's stdio (multiplexed by JSON-RPC id). Reading the cell per call means a
/// stop/start between requests is picked up without rebuilding the endpoint; no live child is
/// the same "not started" the Node build's makeServer() threw.
struct ChildRemote {
    peer: Arc<std::sync::RwLock<Option<Peer<RoleClient>>>>,
}

impl ChildRemote {
    fn peer(&self) -> Result<Peer<RoleClient>, String> {
        self.peer
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .ok_or_else(|| "not started".to_string())
    }
}

/// A cursor string into the params the rmcp client methods take. The cursors are opaque;
/// handing them back and forth unchanged is the whole of per-page paging (Node's `cursorOf`).
fn page_of(cursor: Option<String>) -> Option<PaginatedRequestParams> {
    let cursor = cursor.filter(|c| !c.is_empty())?;
    let mut params = PaginatedRequestParams::default();
    params.cursor = Some(cursor);
    Some(params)
}

#[async_trait]
impl super::proxy::RemoteMcp for ChildRemote {
    async fn list_tools(&self, cursor: Option<String>) -> Result<ListToolsResult, String> {
        self.peer()?
            .list_tools(page_of(cursor))
            .await
            .map_err(|err| err.to_string())
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        timeout_ms: Option<u64>,
    ) -> Result<CallToolResult, String> {
        let peer = self.peer()?;
        // The one option this remote sets: the call deadline (`PROC_CALL_TIMEOUT_MS` or the
        // def's `timeoutMs`). Built by hand because the typed convenience (`call_tool_once`)
        // takes no options — the timeout is the option that must be set.
        let mut options = PeerRequestOptions::default();
        if let Some(ms) = timeout_ms {
            options.timeout = Some(Duration::from_millis(ms));
        }
        let mut request = CallToolRequest::default();
        request.params = params;
        let handle = peer
            .send_request_with_option(ClientRequest::CallToolRequest(request), options)
            .await
            .map_err(|err| err.to_string())?;
        let result = handle
            .await_response()
            .await
            .map_err(|err| err.to_string())?;
        match result {
            ServerResult::CallToolResult(result) => Ok(result),
            // An MRTR round (input required / a materialized task) cannot pause at the
            // gateway: there is no interactive client behind the forwarding to answer it, and
            // the Node build's SDK had no passthrough either — callTool resolved or threw.
            ServerResult::InputRequiredResult(_) => Err(
                "child requested client input mid-call (MRTR); unsupported through the proc proxy"
                    .into(),
            ),
            ServerResult::CreateTaskResult(_) => Err(
                "child returned a task result (SEP-2663); unsupported through the proc proxy"
                    .into(),
            ),
            _ => Err("unexpected response to tools/call".into()),
        }
    }

    async fn list_resources(&self, cursor: Option<String>) -> Result<ListResourcesResult, String> {
        self.peer()?
            .list_resources(page_of(cursor))
            .await
            .map_err(|err| err.to_string())
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, String> {
        match self
            .peer()?
            .read_resource_once(params)
            .await
            .map_err(|err| err.to_string())?
        {
            ReadResourceResponse::Complete(result) => Ok(result),
            _ => Err(
                "child requested client input mid-call (MRTR); unsupported through the proc proxy"
                    .into(),
            ),
        }
    }

    async fn list_prompts(&self, cursor: Option<String>) -> Result<ListPromptsResult, String> {
        self.peer()?
            .list_prompts(page_of(cursor))
            .await
            .map_err(|err| err.to_string())
    }

    async fn get_prompt(&self, params: GetPromptRequestParams) -> Result<GetPromptResult, String> {
        match self
            .peer()?
            .get_prompt_once(params)
            .await
            .map_err(|err| err.to_string())?
        {
            GetPromptResponse::Complete(result) => Ok(result),
            _ => Err(
                "child requested client input mid-call (MRTR); unsupported through the proc proxy"
                    .into(),
            ),
        }
    }
}

/// The live child plus its established MCP session, held between `build()` and `close()`.
struct LiveChild {
    spawned: SpawnedChild,
    client: RunningService<RoleClient, ProcClientHandler>,
}

/// One hosted proc MCP — port of `ProcAdapter`. Spawns on `build()`, tree-kills on `close()`;
/// lazy by default (see `registry::is_lazy`, which already encodes "proc is lazy unless the def
/// says otherwise" — no def-level default lives here).
pub struct ProcAdapter {
    command: String,
    /// Per-MCP environment overrides applied over the inherited environment (Node's `opts.env`).
    /// String values only — the Node spawn required strings too; a non-string entry is skipped
    /// rather than failing the whole MCP.
    env: BTreeMap<String, String>,
    cwd: Option<String>,
    cfg: Arc<ProxyCfg>,
    /// Disabled tool names, shared live with every per-request ProxyServer (see
    /// ProxyOpts::disabled — the deliberate divergence note lives there).
    disabled: super::ToolToggle,
    name: Arc<std::sync::RwLock<String>>,
    /// Ring buffer of child stderr. Survives close() on purpose (as it did in Node): the
    /// panel's log view still shows the last run's output after a stop.
    stderr: Arc<std::sync::Mutex<String>>,
    /// The live child + client. A tokio mutex: close() awaits the client teardown while
    /// holding it, and the registry never runs two lifecycle ops on one entry anyway.
    live: tokio::sync::Mutex<Option<LiveChild>>,
    /// PID mirror for the sync `pids()` view.
    pid: std::sync::Mutex<Option<u32>>,
    /// Shared-child mirror for `ping()` and the per-request proxy servers.
    peer: Arc<std::sync::RwLock<Option<Peer<RoleClient>>>>,
    /// Where calls through this adapter are recorded.
    log: std::sync::Arc<crate::calls::CallLog>,
}

impl ProcAdapter {
    /// `def` must already be a `resolve_def()` clone (env refs expanded), exactly as
    /// `make_adapter` hands it over.
    pub fn new(def: &ServerDef, name: &str, log: std::sync::Arc<crate::calls::CallLog>) -> Self {
        let env: BTreeMap<String, String> = def
            .get("env")
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        // Persisted toggles ride the def (register_one/store seed `disabledTools`); seeded the
        // same way DirectAdapter does it, so a toggle survives a restart.
        let disabled: std::collections::HashSet<String> = def
            .get("disabledTools")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            command: def.get_str("command").unwrap_or("").to_string(),
            env,
            cwd: def.get_str("cwd").map(str::to_string),
            cfg: Arc::new(proxy_cfg_from_def(def)),
            disabled: Arc::new(std::sync::RwLock::new(disabled)),
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            stderr: Arc::new(std::sync::Mutex::new(String::new())),
            live: tokio::sync::Mutex::new(None),
            pid: std::sync::Mutex::new(None),
            peer: Arc::new(std::sync::RwLock::new(None)),
            log,
        }
    }

    /// The teardown half of close() — drop the ledger entry, kill the subtree, then release
    /// the SDK handle (Node's order exactly).
    async fn teardown(live: LiveChild) {
        let mut live = live;
        // Closed cleanly -> no longer a candidate for orphan reaping on next boot. This runs
        // BEFORE the kill: a tree we are about to kill must not be reaped again by a future
        // boot that races our own shutdown.
        swiss_host::proc_pids::drop_proc_pid(live.spawned.pid);
        // Kill the subtree BEFORE releasing the SDK handle: closing only the direct child does
        // NOT cascade on Windows, and the grandchild server would survive as an orphan.
        live.spawned.kill_subtree().await;
        let _ = live.client.close().await; // best effort by contract
    }
}

/// Pump child stderr into the ring until the pipe ends.
async fn pump_stderr(mut stderr: tokio::process::ChildStderr, ring: Arc<std::sync::Mutex<String>>) {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            // EOF (child gone) or a dropped pipe — the ring keeps what it captured.
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = decode_child_output(&buf[..n]);
                if let Ok(mut guard) = ring.lock() {
                    guard.push_str(&text);
                    cap_ring(&mut guard, STDERR_MAX);
                }
            }
        }
    }
}

#[async_trait]
impl super::Adapter for ProcAdapter {
    fn kind(&self) -> &str {
        "proc"
    }

    fn tool_toggle(&self) -> Option<super::ToolToggle> {
        Some(self.disabled.clone())
    }

    async fn build(&self) -> Result<super::McpEndpoint, String> {
        // A child left over from a previous run of this adapter (a stop that raced the build)
        // must never leak: tear it down before spawning the replacement.
        if let Some(stale) = self.live.lock().await.take() {
            Self::teardown(stale).await;
        }

        let mut tokens = tokenize_command(&self.command);
        let Some(cmd) = tokens.first().cloned() else {
            return Err("empty command".into());
        };
        let args = tokens.split_off(1);

        // Inherit the full environment so npx/uvx/python behave as if launched from the user's
        // shell (the Node SDK's sanitized default omits PATHEXT/APPDATA/...). Then put
        // ~/.local/bin (uv/uvx) and %APPDATA%\npm back on PATH — a detached `swiss start` often
        // inherits a PATH missing those user-level bins. Per-MCP env wins over both.
        let path = swiss_host::pathenv::login_path_from_env();
        let program = resolve_program(&cmd, &path);
        // cross-spawn's Windows route: CreateProcess cannot execute a batch file — npm's `npx`
        // IS `npx.cmd` — so a .cmd/.bat program goes through cmd.exe instead. `/d` skips the
        // AutoRun scripts, `/s` keeps the argument quoting verbatim, and raw_arg writes the tail
        // untouched (std's own escaping would mangle cmd's quote rules). The line itself is
        // built with cross-spawn's escapers above — quotes, `%`, trailing backslashes and all.
        // Everything cmd goes on to spawn (cmd.exe -> npx -> the real server) inherits the job
        // object below all the same, so the subtree still dies with one handle.
        #[cfg(windows)]
        let mut command = if is_batch_file(&program) {
            let shim = is_cmd_shim(&program);
            let mut line = cmd_escape_program(&program);
            for a in &args {
                line.push(' ');
                line.push_str(&cmd_escape_arg(a, shim));
            }
            let mut c = tokio::process::Command::new(cmd_exe());
            c.raw_arg(format!("/d /s /c \"{line}\""));
            c
        } else {
            let mut c = tokio::process::Command::new(&program);
            c.args(&args);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = tokio::process::Command::new(&program);
            c.args(&args);
            c
        };
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("PATH", &path);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW — what windowsHide: true maps to in Node's spawn; without it a
            // console-less daemon flashes a console window for every child. tokio's Command
            // exposes creation_flags directly on Windows (no CommandExt import needed).
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let child = command.spawn().map_err(|err| err.to_string())?;
        let pid = child.id().unwrap_or(0);

        // Assign the child to a kill-on-close job AT SPAWN: everything it goes on to create
        // (cmd.exe -> npx -> the real server) inherits the job, so one handle owns the whole
        // subtree — for graceful close AND for a hard kill of the gateway itself.
        #[cfg(windows)]
        let job = {
            let assigned = child.raw_handle().and_then(KillOnCloseJob::assign);
            if assigned.is_none() {
                swiss_core::log::warn(
                    "proc child not under a job object — PID kill fallback in use",
                    Some(serde_json::json!({ "pid": pid })),
                );
            }
            assigned
        };

        let mut spawned = SpawnedChild {
            pid,
            child,
            #[cfg(windows)]
            job,
        };

        // Capture child stderr for the logs view. Started before the handshake so a wrapper
        // that fails instantly still leaves its last words in the ring.
        if let Some(stderr) = spawned.child.stderr.take() {
            let ring = self.stderr.clone();
            tokio::spawn(pump_stderr(stderr, ring));
        }

        let stdout = spawned.child.stdout.take();
        let stdin = spawned.child.stdin.take();
        let (stdout, stdin) = match (stdout, stdin) {
            (Some(out), Some(inp)) => (out, inp),
            _ => {
                spawned.kill_subtree().await;
                return Err("child stdio was not piped".into());
            }
        };

        // The initialize handshake under a deadline: a spawned-but-silent child must fail the
        // start (and be cleaned up below) rather than hang the MCP in "starting" forever.
        // Dropping the timed-out future is the AbortController — the half-open session never
        // completes, and the kill below removes the process.
        let timeout_ms = handshake_timeout_ms();
        let client = match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            serve_client(ProcClientHandler, (stdout, stdin)),
        )
        .await
        {
            Ok(Ok(client)) => client,
            Ok(Err(err)) => {
                // The child spawned but the handshake failed (bad command line, non-MCP
                // output, early exit). rmcp closed ITS side only; on Windows that does NOT
                // cascade to grandchildren, so tree-kill the whole subtree before rethrowing —
                // otherwise every failed launch orphans a process.
                spawned.kill_subtree().await;
                return Err(err.to_string());
            }
            Err(_) => {
                spawned.kill_subtree().await;
                return Err(format!("proc handshake timed out after {timeout_ms}ms"));
            }
        };

        let peer = client.peer().clone();
        if pid != 0 {
            // Ledger it, so a future boot can reap this child if we die hard before close runs.
            swiss_host::proc_pids::note_proc_pid(pid);
        }
        *self.live.lock().await = Some(LiveChild { spawned, client });
        if let Ok(mut cell) = self.pid.lock() {
            *cell = Some(pid);
        }
        if let Ok(mut cell) = self.peer.write() {
            *cell = Some(peer);
        }

        // A fresh proxy server per request (see rmcp_endpoint); every one of them wraps the
        // SAME child client, which multiplexes concurrent requests by JSON-RPC id.
        let remote: Arc<ChildRemote> = Arc::new(ChildRemote {
            peer: self.peer.clone(),
        });
        let name_cell = self.name.clone();
        let cfg = self.cfg.clone();
        let disabled_cell = self.disabled.clone();
        let log = self.log.clone();
        Ok(super::rmcp_endpoint(move |source| {
            super::proxy::ProxyServer::new(
                remote.clone(),
                super::proxy::ProxyOpts {
                    name: name_cell.clone(),
                    description: cfg.description.clone(),
                    expose_resources: cfg.expose_resources,
                    expose_prompts: cfg.expose_prompts,
                    // The def's timeoutMs ?? PROC_CALL_TIMEOUT_MS, resolved at construction.
                    call_timeout_ms: Some(cfg.call_timeout_ms),
                    // Node passed no remoteCaps for a proc child: it "answers before its
                    // capabilities are read", so the toggles alone decide what is announced.
                    remote_caps: None,
                    disabled: disabled_cell.clone(),
                    log: log.clone(),
                },
                source,
            )
        }))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let peer = self.peer.read().ok().and_then(|guard| guard.clone())?;
        // One ping request; any reply means the child is alive (Node's client.ping() throws
        // when the child died — the Err here is what marks the entry down).
        let pinged = peer
            .send_request(ClientRequest::PingRequest(PingRequest::default()))
            .await;
        Some(match pinged {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        })
    }

    async fn close(&self) {
        let live = self.live.lock().await.take();
        if let Some(live) = live {
            Self::teardown(live).await;
        }
        if let Ok(mut cell) = self.pid.lock() {
            *cell = None;
        }
        if let Ok(mut cell) = self.peer.write() {
            *cell = None;
        }
    }

    fn logs(&self) -> Option<String> {
        Some(
            self.stderr
                .lock()
                .ok()
                .map(|guard| guard.clone())
                .unwrap_or_default(),
        )
    }

    fn pids(&self) -> Vec<u32> {
        // The PID of the spawned child, so the memory view can measure this subtree (see
        // mem.rs — the walk fans out to descendants from here).
        match self.pid.lock() {
            Ok(guard) => guard.map(|pid| vec![pid]).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    // Ported from test/proc.test.ts (tokenizeCommand / decodeChildOutput assertions). Anything
    // needing a REAL child process — spawn, handshake, tree-kill — is deliberately not a unit
    // test here: it belongs to the integration suite driving a live gateway with a proc MCP.
    use super::*;
    use crate::adapters::Adapter as _;

    #[test]
    fn tokenize_splits_on_whitespace() {
        assert_eq!(
            tokenize_command("npx -y @scope/pkg"),
            vec!["npx", "-y", "@scope/pkg"]
        );
        assert_eq!(tokenize_command("  spaced   out  "), vec!["spaced", "out"]);
        assert_eq!(tokenize_command(""), Vec::<String>::new());
    }

    // --- cross-spawn escape.js, pinned to the same outputs the Node package produces ----------

    #[cfg(windows)]
    #[test]
    fn cmd_escape_matches_cross_spawn_examples() {
        // escape.argument, plain value: quoted; the quotes are metachars themselves.
        assert_eq!(cmd_escape_arg("hello", false), r#"^"hello^""#);
        // A backslash run before the closing quote doubles; before a non-quote it does not.
        assert_eq!(cmd_escape_arg(r"a\b", false), r#"^"a\b^""#);
        assert_eq!(cmd_escape_arg(r"a\\", false), r#"^"a\\\\^""#);
        // An embedded quote gains a backslash AND ^-escapes as a metachar.
        assert_eq!(cmd_escape_arg(r#"a"b"#, false), r#"^"a\^"b^""#);
        // Every metachar — space included — is ^-escaped even inside the quotes.
        assert_eq!(cmd_escape_arg("a b&c", false), r#"^"a^ b^&c^""#);
        // A node_modules/.bin shim re-runs the ^ pass — which escapes the inserted carets too.
        assert_eq!(cmd_escape_arg("a b", true), r#"^^^"a^^^ b^^^""#);
        // escape.command ^-escapes without quoting.
        assert_eq!(
            cmd_escape_program(r"C:\Program Files\nodejs\npx.cmd"),
            r"C:\Program^ Files\nodejs\npx.cmd"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cmd_shim_detection_matches_parse_js() {
        assert!(is_cmd_shim(r"C:\repo\node_modules\.bin\my-tool.CMD"));
        // Not a shim: the .cmd is not directly inside node_modules/.bin.
        assert!(!is_cmd_shim(r"C:\Users\x\AppData\Roaming\npm\npx.cmd"));
        assert!(!is_cmd_shim(r"C:\repo\node_modules\.bin\sub\tool.cmd"));
        assert!(!is_cmd_shim(r"C:\tools\server.exe"));
    }

    #[test]
    fn tokenize_honors_double_and_single_quotes() {
        assert_eq!(
            tokenize_command(r#"node "C:\my dir\run.js" --flag"#),
            vec!["node", r"C:\my dir\run.js", "--flag"]
        );
        assert_eq!(
            tokenize_command("sh -c 'echo hi there'"),
            vec!["sh", "-c", "echo hi there"]
        );
        // Single quotes make everything literal — no double-quote processing inside them.
        assert_eq!(tokenize_command(r#"x 'a "b" c'"#), vec!["x", r#"a "b" c"#]);
    }

    #[test]
    fn tokenize_backslash_is_a_separator_not_an_escape() {
        // Windows path separators survive untouched outside quotes...
        assert_eq!(
            tokenize_command(r"C:\tools\server.exe"),
            vec![r"C:\tools\server.exe"]
        );
        // ...inside double quotes only \" and \\ are escapes; \a stays literal.
        assert_eq!(tokenize_command(r#""a\bc""#), vec![r"a\bc"]);
        assert_eq!(tokenize_command(r#""a\\b""#), vec![r"a\b"]);
        assert_eq!(tokenize_command(r#""a\"b""#), vec!["a\"b"]);
        // A trailing token is kept; an empty quoted string is dropped (Node's `if (cur)`).
        assert_eq!(tokenize_command(r#"one "two""#), vec!["one", "two"]);
        assert_eq!(tokenize_command(r#""""#), Vec::<String>::new());
    }

    #[test]
    fn decode_keeps_valid_utf8() {
        assert_eq!(decode_child_output("plain ascii".as_bytes()), "plain ascii");
        assert_eq!(decode_child_output("中文".as_bytes()), "中文");
    }

    #[test]
    fn decode_falls_back_to_gbk_for_non_utf8_bytes() {
        // 「不是」 in GBK (B2 BB CA C7): not valid UTF-8 as a whole (the second pair breaks it),
        // decodable as GBK — the cmd.exe-on-Chinese-Windows case the fallback exists for.
        assert_eq!(decode_child_output(&[0xB2, 0xBB, 0xCA, 0xC7]), "不是");
    }

    #[test]
    fn decode_replaces_bytes_gbk_cannot_decode() {
        // A lone 0xFF is invalid in UTF-8 AND undefined in GBK — the lossy last resort
        // replaces it rather than failing.
        assert_eq!(decode_child_output(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
    }

    #[test]
    fn the_stderr_ring_keeps_only_the_tail() {
        let mut ring = "x".repeat(STDERR_MAX + 10);
        cap_ring(&mut ring, STDERR_MAX);
        assert_eq!(ring.chars().count(), STDERR_MAX);
        assert!(ring.chars().all(|c| c == 'x'));
        // The TAIL is what survives — the newest output is the diagnostically useful half.
        let mut ring: String = format!("{}TAIL", "h".repeat(STDERR_MAX + 5));
        cap_ring(&mut ring, STDERR_MAX);
        assert!(ring.ends_with("TAIL"));
        // Under the cap: untouched.
        let small = "short".to_string();
        let mut ring = small.clone();
        cap_ring(&mut ring, STDERR_MAX);
        assert_eq!(ring, small);
    }

    #[test]
    fn env_timeout_parsing_matches_number_or_default() {
        assert_eq!(parse_env_ms(None, 60_000), 60_000);
        assert_eq!(parse_env_ms(Some(String::new()), 60_000), 60_000);
        assert_eq!(parse_env_ms(Some("abc".into()), 60_000), 60_000);
        assert_eq!(parse_env_ms(Some("0".into()), 60_000), 60_000); // 0 is falsy in JS
        assert_eq!(parse_env_ms(Some("-5".into()), 60_000), 60_000);
        assert_eq!(parse_env_ms(Some("9000".into()), 60_000), 9_000);
        assert_eq!(parse_env_ms(Some(" 12000 ".into()), 60_000), 12_000);
    }

    #[test]
    fn proxy_cfg_resolves_toggles_and_timeout_from_the_def() {
        let def: ServerDef = serde_json::from_value(serde_json::json!({
            "type": "proc",
            "command": "npx -y pkg",
            "description": "what this is for",
            "timeoutMs": 5000,
            "exposeResources": false,
            "exposePrompts": true,
        }))
        .unwrap();
        let cfg = proxy_cfg_from_def(&def);
        assert_eq!(cfg.description.as_deref(), Some("what this is for"));
        assert!(!cfg.expose_resources);
        assert!(cfg.expose_prompts);
        assert_eq!(cfg.call_timeout_ms, 5_000);

        // Absent flags expose (the default); a string "false" exposes too — the Node build's
        // loose-cast truth table, ported exactly.
        let loose: ServerDef = serde_json::from_value(serde_json::json!({
            "type": "proc", "command": "x", "exposeResources": "false",
        }))
        .unwrap();
        let cfg = proxy_cfg_from_def(&loose);
        assert!(cfg.expose_resources);
        assert!(cfg.expose_prompts);
        assert_eq!(cfg.call_timeout_ms, 180_000);
    }

    #[test]
    fn the_adapter_reads_launch_fields_from_the_def() {
        let def: ServerDef = serde_json::from_value(serde_json::json!({
            "type": "proc",
            "command": "uvx mcp-server-fetch",
            "cwd": "C:\\work",
            "env": { "A": "1", "B": 2, "C": "3" },
        }))
        .unwrap();
        let adapter = ProcAdapter::new(&def, "fetch", crate::calls::test_log());
        assert_eq!(adapter.command, "uvx mcp-server-fetch");
        assert_eq!(adapter.cwd.as_deref(), Some("C:\\work"));
        // Non-string env entries are skipped, string ones kept.
        assert_eq!(
            adapter.env,
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("C".to_string(), "3".to_string())
            ])
        );
        // A not-yet-started adapter has no pid and no captured stderr.
        assert!(adapter.pids().is_empty());
        assert_eq!(adapter.logs().as_deref(), Some(""));
        assert_eq!(adapter.kind(), "proc");
        adapter.rename("fetch2");
        let renamed = adapter.name.read().ok().map(|guard| guard.clone());
        assert_eq!(renamed.as_deref(), Some("fetch2"));
    }
}
