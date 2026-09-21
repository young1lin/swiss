/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The builtin remote MCP adapter (docs/34 R7): the remote plugin's agent-facing
//! vocabulary as FIVE tools under /mcp/remote, so a model can drive a build box
//! without a shell.
//!
//! The bridge is deliberately THIN and name-based: this crate never links
//! swiss-remote and knows none of its types. A tool call becomes a run of the
//! matching 'remote.*' action through the host's shared run coordinator - the
//! same accounting the CLI and Jobs use - and the tool result is the run's
//! outcome text plus a trailing exit/status line. An action that is not
//! registered (the remote plugin disabled) answers as an in-band tool error
//! naming it, never a protocol fault.
//!
//! Routing rules every tool's description repeats (docs/34 SS5): 'target' is an
//! ALIAS from the gateway's sealed targets table - never a host; relative paths
//! resolve under the target's workspaceRoot; absolute paths pass through as-is.
//! Paths named by sync/pull are GATEWAY-side paths, a fact the descriptions
//! state because a model on another machine must not mistake them for its own
//! filesystem.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerInfo, Tool, ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use swiss_host::services::runs::{RunState, RunView, SubmitError, SubmitRequest, SubmittedRun};
use swiss_host::services::RuntimeServices;

/// The run owner string MCP-initiated remote calls are booked under. Distinct
/// from "manual" (the /api/runs surface) and "remote" (the plugin's own owner)
/// so neither population's cancels and shutdown sweeps reach these runs.
const MCP_RUN_OWNER: &str = "remote-mcp";

/// The run's actor (docs/41 A1): the token that authenticated this MCP call, as
/// `mcp:<token label>` - the one actor string in the system that is not self-declared.
/// The panel's Run button reaches the same tools without a token: `panel`. Read from
/// the source the server captured at factory time, never from the task-local: rmcp
/// drives call_tool from its own task, where `current_source()` sees nothing.
fn actor_of(source: &crate::calls::CallSource) -> String {
    match (source.via.as_str(), source.client.as_deref()) {
        ("panel", _) => "panel".to_string(),
        (_, Some(client)) => format!("mcp:{client}"),
        (_, None) => "mcp".to_string(),
    }
}

/// The default deadline for one-shot tools (sync/pull/cat/write): the /api/runs
/// surface's own default, which is what a panel-run action of the same shape gets.
const TOOL_TIMEOUT_MS: u64 = 600_000;
/// The default deadline for remote_exec: the remote.exec action's own default
/// (docs/34 SS26), so a model that omits timeoutMs gets the same two hours the
/// CLI does, not a silent ten-minute cap.
const EXEC_TIMEOUT_MS: u64 = 2 * 60 * 60 * 1000;
/// The submit-route ceiling, enforced the same way here: a deadline past this is
/// refused at the tool door rather than accepted and clamped invisibly.
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// The routing sentence every tool description ends with. Written for a model
/// that has never seen the targets table: alias, not host; relative under the
/// root; absolute untouched.
fn routing_note(known: &[String]) -> String {
    if known.is_empty() {
        "Routing: 'target' is an alias from this gateway's remote targets table (none configured yet - list or add them on the Remote Targets page); relative paths resolve under the target's workspaceRoot; absolute paths pass through as-is."
            .to_string()
    } else {
        format!(
            "Routing: 'target' is an alias from this gateway's remote targets table (known: {}); relative paths resolve under the target's workspaceRoot; absolute paths pass through as-is.",
            known.join(", ")
        )
    }
}

/// The 'target' parameter's schema description - the one place a model looks
/// when it is filling the argument in, so it carries the live alias list (or the
/// honest empty fallback) rather than a generic "a target id".
fn target_description(known: &[String]) -> String {
    if known.is_empty() {
        "Target alias from this gateway's remote targets table (none configured yet).".into()
    } else {
        format!(
            "Target alias from this gateway's remote targets table. Known: {}.",
            known.join(", ")
        )
    }
}

/// One tool's definition, built per list call so the target description can
/// carry the CURRENT alias list (targets change at runtime; a cached schema
/// would go stale behind the panel's back).
fn tool_def(name: &str, known: &[String]) -> Tool {
    let target = target_description(known);
    let note = routing_note(known);
    let (description, schema): (String, Value) = match name {
        "remote_exec" => (
            format!(
                "Run one non-interactive command on a remote target (action remote.exec). stdout/stderr come back as this tool's text, a trailing line reports the exit code, and a nonzero exit is returned as an error. There is no PTY: nothing can answer a prompt - sudo is allowed but needs NOPASSWD or -n. {note}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": target },
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The argv to run; argv[0] is the program. Pass words, not a shell line - the gateway quotes. sudo passes (NOPASSWD or -n)."
                    },
                    "cwd": { "type": "string", "description": "Working directory; relative resolves under the target's workspaceRoot." },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Extra environment variables as name to value."
                    },
                    "timeoutMs": { "type": "integer", "description": "Deadline in milliseconds (default two hours)." }
                },
                "required": ["target", "argv"],
            }),
        ),
        "remote_sync" => (
            format!(
                "Upload a file or directory tree from THIS GATEWAY machine to the target (action remote.sync; upload-only, never deletes; a single file may carry a remote 'to' name). 'source' is a path on the machine running this gateway, not on yours. {note}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": target },
                    "source": { "type": "string", "description": "Gateway-side path; a directory walks and uploads (default excludes apply) or a single file uploads alone. Default '.'." },
                    "to": { "type": "string", "description": "Remote name for a single-file upload (ignored for directories)." },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Extra exclude patterns on top of the defaults."
                    }
                },
                "required": ["target"],
            }),
        ),
        "remote_pull" => (
            format!(
                "Download a remote file or whole directory tree to THIS GATEWAY machine (action remote.pull). 'to' is a path on the machine running this gateway, not on yours; omit it to keep the remote's relative path. {note}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": target },
                    "remote": { "type": "string", "description": "Workspace-relative file or directory path on the target." },
                    "to": { "type": "string", "description": "Gateway-side destination path; default the same relative path." }
                },
                "required": ["target", "remote"],
            }),
        ),
        "remote_cat" => (
            format!(
                "Read one remote file and return its bytes as text (action remote.cat). For output a model should not inline, remote_exec or remote_pull are the right shapes. {note}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": target },
                    "remote": { "type": "string", "description": "Workspace-relative file path on the target." }
                },
                "required": ["target", "remote"],
            }),
        ),
        "remote_write" => (
            format!(
                "Write 'content' to one remote file, creating or overwriting it (action remote.write). {note}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": target },
                    "remote": { "type": "string", "description": "Workspace-relative file path on the target." },
                    "content": { "type": "string", "description": "The full file content to write (UTF-8 text)." }
                },
                "required": ["target", "remote", "content"],
            }),
        ),
        other => unreachable!("tool table out of sync: {other}"),
    };
    let schema: JsonObject =
        serde_json::from_value(schema).expect("the literal above is a JSON object");
    let mut tool = Tool::default();
    tool.name = name.to_string().into();
    tool.description = Some(description.into());
    tool.input_schema = Arc::new(schema);
    tool
}

/// Every tool this MCP serves, in a stable order.
const TOOLS: [&str; 5] = [
    "remote_exec",
    "remote_sync",
    "remote_pull",
    "remote_cat",
    "remote_write",
];

// --- tool arguments to action input -------------------------------------------------

/// Read one optional string argument, refusing a present-but-not-a-string value:
/// the same strictness the actions apply on their side of the seam.
fn opt_text(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!("{key} must be a string, got {other}")),
    }
}

/// One required string argument.
fn req_text(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    opt_text(args, key)?.ok_or_else(|| format!("{key} is required"))
}

/// One required array-of-strings argument, refusing non-string items in place.
fn req_strings(args: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    let raw = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{key} must be an array of strings"))?;
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let Some(s) = item.as_str() else {
            return Err(format!("{key} must contain only strings, got {item}"));
        };
        out.push(s.to_string());
    }
    Ok(out)
}

/// The env object (name to value) as the action's K=V pair list - the ONE shape
/// translation this adapter does, because models write objects and the action
/// contract takes pairs.
fn env_pairs(args: &Map<String, Value>) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = args.get("env") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(obj) = raw.as_object() else {
        return Err("env must be an object of name to value".into());
    };
    let mut pairs = Vec::with_capacity(obj.len());
    for (name, value) in obj {
        let Some(value) = value.as_str() else {
            return Err(format!("env.{name} must be a string, got {value}"));
        };
        pairs.push(format!("{name}={value}"));
    }
    Ok(Some(pairs))
}

/// One optional positive deadline, refused past the submit ceiling.
fn opt_timeout(args: &Map<String, Value>) -> Result<Option<u64>, String> {
    match args.get("timeoutMs") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(ms) if ms > 0 && ms <= MAX_TIMEOUT_MS => Ok(Some(ms)),
            _ => Err(format!(
                "timeoutMs must be a positive integer up to {MAX_TIMEOUT_MS}"
            )),
        },
    }
}

/// What one validated tool call wants: the action to run, the input to hand it,
/// and the run-coordinator deadline that keeps the run honest when the action
/// outlives its own patience.
struct Plan {
    action: &'static str,
    input: Value,
    timeout_ms: u64,
}

/// Validate one tool call and build its action input. Unknown arguments are
/// refused naming the tool - the strict-door convention every parser in this
/// codebase follows, so a model's typo fails here instead of being silently
/// dropped half-way to the target.
fn plan(tool: &str, args: &Map<String, Value>) -> Result<Plan, String> {
    let allowed: &[&str] = match tool {
        "remote_exec" => &["target", "argv", "cwd", "env", "timeoutMs"],
        "remote_sync" => &["target", "source", "to", "exclude"],
        "remote_pull" => &["target", "remote", "to"],
        "remote_cat" => &["target", "remote"],
        "remote_write" => &["target", "remote", "content"],
        _ => return Err(format!("unknown tool {tool}")),
    };
    if let Some(key) = args.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("{tool} has no {key:?} argument"));
    }
    let mut input = Map::new();
    input.insert("target".into(), json!(req_text(args, "target")?));
    match tool {
        "remote_exec" => {
            input.insert("argv".into(), json!(req_strings(args, "argv")?));
            if let Some(cwd) = opt_text(args, "cwd")? {
                input.insert("cwd".into(), json!(cwd));
            }
            if let Some(pairs) = env_pairs(args)? {
                input.insert("env".into(), json!(pairs));
            }
            let timeout = opt_timeout(args)?;
            if let Some(ms) = timeout {
                input.insert("timeoutMs".into(), json!(ms));
            }
            Ok(Plan {
                action: "remote.exec",
                input: input.into(),
                timeout_ms: timeout.unwrap_or(EXEC_TIMEOUT_MS),
            })
        }
        "remote_sync" => {
            for key in ["source", "to"] {
                if let Some(v) = opt_text(args, key)? {
                    input.insert(key.into(), json!(v));
                }
            }
            if let Some(exclude) = args.get("exclude") {
                if !exclude.is_null() {
                    let Some(list) = exclude.as_array() else {
                        return Err("exclude must be an array of strings".into());
                    };
                    let mut out = Vec::with_capacity(list.len());
                    for item in list {
                        let Some(s) = item.as_str() else {
                            return Err(format!("exclude must contain only strings, got {item}"));
                        };
                        out.push(s.to_string());
                    }
                    input.insert("exclude".into(), json!(out));
                }
            }
            Ok(Plan {
                action: "remote.sync",
                input: input.into(),
                timeout_ms: TOOL_TIMEOUT_MS,
            })
        }
        "remote_pull" => {
            input.insert("remote".into(), json!(req_text(args, "remote")?));
            if let Some(to) = opt_text(args, "to")? {
                input.insert("to".into(), json!(to));
            }
            Ok(Plan {
                action: "remote.pull",
                input: input.into(),
                timeout_ms: TOOL_TIMEOUT_MS,
            })
        }
        "remote_cat" => {
            input.insert("remote".into(), json!(req_text(args, "remote")?));
            Ok(Plan {
                action: "remote.cat",
                input: input.into(),
                timeout_ms: TOOL_TIMEOUT_MS,
            })
        }
        "remote_write" => {
            input.insert("remote".into(), json!(req_text(args, "remote")?));
            input.insert("content".into(), json!(req_text(args, "content")?));
            Ok(Plan {
                action: "remote.write",
                input: input.into(),
                timeout_ms: TOOL_TIMEOUT_MS,
            })
        }
        _ => Err(format!("unknown tool {tool}")),
    }
}

/// The tool result for one finished run: the outcome text, then a trailing line
/// with the exit code (or the failure's own word for itself) and the run id a
/// model can quote when asking a human what happened. Non-success is an
/// in-band isError result, which MCP clients surface as tool output rather than
/// a protocol fault - a failed build is data, not a broken call.
/// How much run output a tool result may carry. A model context is the scarce
/// resource here: exec can still stream a huge log through the run window into
/// a tool result. Keep the HEAD (the part a model usually needs) and say the rest.
const RESULT_TEXT_CAP: usize = 64 * 1024;

fn run_result(view: &RunView) -> CallToolResponse {
    let ok = view.state == RunState::Succeeded;
    let mut text = view.output.clone().unwrap_or_default();
    if text.len() > RESULT_TEXT_CAP {
        // Cut on a char boundary, keep the head, and let the marker say the rest.
        let mut cut = RESULT_TEXT_CAP;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let full = text.len();
        text.truncate(cut);
        text.push_str(&format!(
            "\n--- output truncated: {} of {} bytes shown; remote.pull the file or read the run for the rest\n",
            cut, full
        ));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let status = if let Some(code) = view.exit_code {
        format!("exit code: {code}")
    } else if view.timed_out {
        "timed out".to_string()
    } else if view.canceled {
        "canceled".to_string()
    } else if let Some(err) = &view.error {
        err.clone()
    } else {
        view.state.as_str().to_string()
    };
    text.push_str(&format!(
        "--- {status} (run {}, {} ms)\n",
        view.run_id,
        view.ms.unwrap_or(0)
    ));
    let result = if ok {
        CallToolResult::success(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::error(vec![ContentBlock::text(text)])
    };
    result.into()
}

/// One in-band error result - the shape every refusal here takes, so a model
/// reads the reason as tool output instead of a dropped request.
fn tool_text(text: &str, is_error: bool) -> CallToolResponse {
    let result = if is_error {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    };
    result.into()
}

/// Submit one plan as a run and wait for its terminal view - the exact
/// accounting path Jobs and /api/runs use, owner-scoped so these runs are their
/// own population in the runs surface.
async fn run_plan(
    services: &Arc<RuntimeServices>,
    planned: Plan,
    actor: String,
) -> Result<RunView, String> {
    let submitted: SubmittedRun = services
        .runs
        .submit(SubmitRequest {
            owner: MCP_RUN_OWNER.to_string(),
            label: format!("mcp:{}", planned.action),
            action_type: planned.action.to_string(),
            input: planned.input,
            timeout_ms: planned.timeout_ms,
            // A model's call is worth holding through a busy pool: the
            // alternative is telling an agent "retry later", which agents do
            // badly.
            queue_if_busy: true,
            actor,
        })
        .map_err(|err| match err {
            SubmitError::Action(m) => {
                format!("{m}; enable the Remote plugin if it is disabled")
            }
            SubmitError::Capacity(m) => m,
        })?;
    submitted
        .done
        .await
        .map_err(|_| "the run ended without a result".to_string())
}

/// The MCP server one request gets: five tools over the shared run machinery.
/// Fresh per request (the rmcp stateless contract); the adapter's shared cells -
/// the services, the alias closure, the log name - ride behind Arcs.
pub struct RemoteServer {
    /// The host's action registry + run coordinator, reached BY NAME.
    pub services: Arc<RuntimeServices>,
    /// Current target aliases, read live per tools/list so the description never
    /// goes stale behind a rename on the targets page.
    pub targets: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The MCP's registry name, followed across renames (call-log key).
    pub name: Option<Arc<std::sync::RwLock<String>>>,
    /// Who is calling, captured at factory time (calls::CallSource).
    pub source: crate::calls::CallSource,
    /// Where the call is recorded.
    pub log: Arc<crate::calls::CallLog>,
}

impl ServerHandler for RemoteServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());
        let mut server_info = Implementation::default();
        server_info.name = "remote".into();
        server_info.version = "1.0".into();
        ServerInfo::new(capabilities).with_server_info(server_info)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let defs = TOOLS
            .iter()
            .map(|name| tool_def(name, &(self.targets)()))
            .collect();
        std::future::ready(Ok(ListToolsResult::with_all_items(defs)))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        // Logged like every adapter call: the panel's Logs tab shows remote tool
        // traffic under this MCP's (current) name, errors included.
        let mcp = self
            .name
            .as_ref()
            .and_then(|n| n.read().ok().map(|g| g.clone()));
        let source = self.source.clone();
        let log = self.log.clone();
        let tool = request.name.to_string();
        // The log row borrows `tool`; the call body owns its own copy, because
        // the logged future must stand alone inside the wrapper.
        let called = tool.clone();
        let args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Null);
        let services = self.services.clone();
        let actor = actor_of(&self.source);
        async move {
            log.logged(
                mcp.as_deref(),
                &source,
                &tool,
                if args.is_null() { None } else { Some(args) },
                async move {
                    let Some(arguments) = request.arguments.clone() else {
                        return Ok(tool_text("arguments are required", true));
                    };
                    let planned = match plan(&called, &arguments) {
                        Ok(planned) => planned,
                        Err(err) => return Ok(tool_text(&err, true)),
                    };
                    match run_plan(&services, planned, actor).await {
                        Ok(view) => Ok(run_result(&view)),
                        Err(err) => Ok(tool_text(&err, true)),
                    }
                },
                |result: &CallToolResponse| {
                    let is_error = match result {
                        CallToolResponse::Complete(r) => r.is_error.unwrap_or(false),
                        _ => true,
                    };
                    let text = match result {
                        CallToolResponse::Complete(r) => r
                            .content
                            .iter()
                            .map(|c| match c {
                                ContentBlock::Text(t) => t.text.clone(),
                                other => format!("[{other:?} content]"),
                            })
                            .collect::<Vec<_>>()
                            .join(
                                "
",
                            ),
                        _ => String::new(),
                    };
                    (!is_error, text)
                },
            )
            .await
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        // Feeds the transport's Mcp-Param-* header validation (see echo's note).
        TOOLS
            .iter()
            .find(|tool| **tool == name)
            .map(|tool| tool_def(tool, &(self.targets)()))
    }
}

/// The builtin adapter: compiled in, no connection, no child - it holds the
/// host service handles and the alias seam, and costs nothing until a call
/// arrives.
///
/// Registered by the remote plugin under the name "remote" (docs/34 R7): the
/// entry's lifecycle follows the plugin's, so disabling Remote withdraws the
/// tools together with the actions they dispatch to.
pub struct RemoteAdapter {
    services: Arc<RuntimeServices>,
    targets: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    name: Arc<std::sync::RwLock<String>>,
    log: Arc<crate::calls::CallLog>,
}

impl RemoteAdapter {
    pub fn new(
        services: Arc<RuntimeServices>,
        log: Arc<crate::calls::CallLog>,
        targets: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        RemoteAdapter {
            services,
            targets,
            name: Arc::new(std::sync::RwLock::new("remote".to_string())),
            log,
        }
    }
}

#[async_trait]
impl super::Adapter for RemoteAdapter {
    fn kind(&self) -> &str {
        "remote"
    }

    async fn build(&self) -> Result<super::McpEndpoint, String> {
        // A fresh server per request (the stateless contract); every server
        // shares this adapter's cells, so renames and alias edits are seen live.
        let services = self.services.clone();
        let targets = self.targets.clone();
        let name = self.name.clone();
        let log = self.log.clone();
        Ok(super::rmcp_endpoint(move |source| RemoteServer {
            services: services.clone(),
            targets: targets.clone(),
            name: Some(name.clone()),
            source,
            log: log.clone(),
        }))
    }

    fn rename(&self, next: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = next.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmcp::model::CallToolRequestParams;
    use swiss_host::services::action::{
        Action, ActionContext, ActionError, ActionOutcome, CancelHandle,
    };

    /// A stand-in for the remote actions: records the input it was called with,
    /// streams one line into the run's live buffer, and mirrors the exec
    /// contract (exit 0 unless argv[0] is "false", whose run then fails with
    /// exit 1).
    struct StubAction {
        name: &'static str,
        seen: std::sync::Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl Action for StubAction {
        fn type_name(&self) -> &'static str {
            self.name
        }

        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }

        // The coordinator only calls execute_with_context; the base method exists
        // to satisfy the trait and is never reached in these tests.
        async fn execute(
            &self,
            _input: &Value,
            _cancel: CancelHandle,
        ) -> Result<ActionOutcome, ActionError> {
            Ok(ActionOutcome::ok())
        }

        async fn execute_with_context(
            &self,
            input: &Value,
            ctx: ActionContext,
        ) -> Result<ActionOutcome, ActionError> {
            self.seen.lock().unwrap().push(input.clone());
            ctx.output.append(&format!("streamed from {}\n", self.name));
            let fails = input
                .get("argv")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .is_some_and(|first| first == "false");
            if fails {
                return Ok(ActionOutcome {
                    ok: false,
                    ms: 1,
                    exit_code: Some(1),
                    error: Some("exit status 1".into()),
                    output: "the command failed\n".into(),
                    ..ActionOutcome::ok()
                });
            }
            let mut outcome = ActionOutcome::ok();
            outcome.output = format!("outcome of {}\n", self.name);
            outcome.exit_code = Some(0);
            Ok(outcome)
        }
    }

    fn alias_closure() -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(|| vec!["dev".to_string(), "build".to_string()])
    }

    /// A server over a fake system: every remote.* action stubbed, two aliases.
    /// Mirrors how the real wiring builds the adapter - only the alias closure
    /// differs (a static list standing in for the sealed targets table).
    fn server_with_actions() -> (Arc<RuntimeServices>, Vec<Arc<StubAction>>) {
        let services = RuntimeServices::new();
        let mut stubs = Vec::new();
        for name in [
            "remote.exec",
            "remote.sync",
            "remote.pull",
            "remote.cat",
            "remote.write",
        ] {
            let stub = Arc::new(StubAction {
                name,
                seen: std::sync::Mutex::new(Vec::new()),
            });
            services.actions.register(stub.clone()).expect("registers");
            stubs.push(stub);
        }
        (services, stubs)
    }

    fn server(services: &Arc<RuntimeServices>) -> RemoteServer {
        RemoteServer {
            services: services.clone(),
            targets: alias_closure(),
            name: None,
            source: crate::calls::CallSource::default(),
            log: crate::calls::test_log(),
        }
    }

    /// Drive one call over the REAL wire shape: a throwaway in-memory session
    /// speaking JSON-RPC, the same seam the panel's tool runner uses.
    async fn call(services: &Arc<RuntimeServices>, tool: &str, args: Value) -> CallToolResult {
        let client = crate::introspect::open_session(server(services))
            .await
            .expect("session opens");
        let mut params = CallToolRequestParams::default();
        params.name = tool.to_string().into();
        params.arguments = args.as_object().cloned();
        let out = client.call_tool(params).await.expect("tools/call answers");
        let _ = client.cancel().await;
        out
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .map(|c| match c {
                ContentBlock::Text(t) => t.text.clone(),
                other => format!("[{other:?} content]"),
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }

    fn is_error(result: &CallToolResult) -> bool {
        result.is_error.unwrap_or(false)
    }

    #[tokio::test]
    async fn lists_exactly_five_tools_with_live_aliases() {
        let (services, _stubs) = server_with_actions();
        let client = crate::introspect::open_session(server(&services))
            .await
            .expect("session opens");
        let listed = client.list_tools(None).await.expect("tools/list answers");
        let names: Vec<String> = listed.tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "remote_exec",
                "remote_sync",
                "remote_pull",
                "remote_cat",
                "remote_write"
            ],
            "the five tools, in the adapter's stable order"
        );
        for tool in &listed.tools {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.contains("workspaceRoot"),
                "{description} must state the routing rule"
            );
            assert!(
                description.contains("known: dev, build"),
                "{description} must carry the live aliases"
            );
            let schema = serde_json::to_value(&*tool.input_schema).expect("schema serializes");
            let target = schema["properties"]["target"]["description"].as_str();
            assert_eq!(
                target,
                Some("Target alias from this gateway's remote targets table. Known: dev, build.")
            );
        }
        let _ = client.cancel().await;
    }

    #[test]
    fn an_empty_targets_table_falls_back_to_the_honest_sentence() {
        let empty: Vec<String> = Vec::new();
        assert!(target_description(&empty).contains("none configured yet"));
        let tool = tool_def("remote_exec", &empty);
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(description.contains("none configured yet"));
        assert!(description.contains("workspaceRoot"));
    }

    #[tokio::test]
    async fn exec_returns_the_outcome_text_and_trailing_exit_code() {
        let (services, stubs) = server_with_actions();
        let out = call(
            &services,
            "remote_exec",
            json!({ "target": "dev", "argv": ["make", "-j8"] }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        let text = text_of(&out);
        assert!(text.contains("outcome of remote.exec"), "{text}");
        assert!(
            text.contains("exit code: 0"),
            "the trailing line reports the exit code: {text}"
        );
        assert!(text.contains("(run "), "the run id rides along: {text}");
        // The action saw exactly the routed input.
        let seen = stubs[0].seen.lock().unwrap();
        assert_eq!(seen[0], json!({ "target": "dev", "argv": ["make", "-j8"] }));
    }

    /// Drive one call through a server whose factory-time source is `source`.
    async fn call_as(
        services: &Arc<RuntimeServices>,
        source: crate::calls::CallSource,
        tool: &str,
        args: Value,
    ) -> CallToolResult {
        let mut srv = server(services);
        srv.source = source;
        let client = crate::introspect::open_session(srv)
            .await
            .expect("session opens");
        let mut params = CallToolRequestParams::default();
        params.name = tool.to_string().into();
        params.arguments = args.as_object().cloned();
        let out = client.call_tool(params).await.expect("tools/call answers");
        let _ = client.cancel().await;
        out
    }

    #[tokio::test]
    async fn the_run_is_booked_to_the_token_that_made_the_call() {
        // docs/41 A1: the actor comes from the source captured at factory time - the
        // one attribution in the system that is authenticated - and rmcp's own task
        // (where the task-local is unset) cannot blank it.
        let (services, _stubs) = server_with_actions();
        let sources = [
            (
                crate::calls::CallSource {
                    via: "mcp".into(),
                    client: Some("claude-code".into()),
                },
                "mcp:claude-code",
            ),
            (
                crate::calls::CallSource {
                    via: "panel".into(),
                    client: None,
                },
                "panel",
            ),
            (crate::calls::CallSource::default(), "mcp"),
        ];
        for (source, want) in sources {
            let out = call_as(
                &services,
                source,
                "remote_exec",
                json!({ "target": "dev", "argv": ["true"] }),
            )
            .await;
            assert!(!is_error(&out), "{}", text_of(&out));
            let newest = services.runs.list().into_iter().next().expect("the run is listed");
            assert_eq!(newest.actor, want);
            assert_eq!(newest.to_json(false)["actor"], want);
        }
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_in_band_error_with_its_code() {
        let (services, _stubs) = server_with_actions();
        let out = call(
            &services,
            "remote_exec",
            json!({ "target": "dev", "argv": ["false"] }),
        )
        .await;
        assert!(is_error(&out), "a failed command is an error result");
        let text = text_of(&out);
        assert!(text.contains("the command failed"), "{text}");
        assert!(text.contains("exit code: 1"), "{text}");
    }

    #[tokio::test]
    async fn the_other_four_tools_map_their_arguments_verbatim() {
        let (services, stubs) = server_with_actions();
        // sync: optional passthrough, excludes included.
        let out = call(
            &services,
            "remote_sync",
            json!({ "target": "dev", "source": "dist/", "to": "site", "exclude": ["*.map"] }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        assert_eq!(
            stubs[1].seen.lock().unwrap()[0],
            json!({ "target": "dev", "source": "dist/", "to": "site", "exclude": ["*.map"] })
        );
        // pull: target + remote + to.
        let out = call(
            &services,
            "remote_pull",
            json!({ "target": "build", "remote": "out/app", "to": "C:/tmp/app" }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        assert_eq!(
            stubs[2].seen.lock().unwrap()[0],
            json!({ "target": "build", "remote": "out/app", "to": "C:/tmp/app" })
        );
        // cat: exactly the two fields.
        let out = call(
            &services,
            "remote_cat",
            json!({ "target": "dev", "remote": "notes.txt" }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        assert_eq!(
            stubs[3].seen.lock().unwrap()[0],
            json!({ "target": "dev", "remote": "notes.txt" })
        );
        // write: content rides as the action's string.
        let out = call(
            &services,
            "remote_write",
            json!({ "target": "dev", "remote": "hello.txt", "content": "hi\n" }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        assert_eq!(
            stubs[4].seen.lock().unwrap()[0],
            json!({ "target": "dev", "remote": "hello.txt", "content": "hi\n" })
        );
    }

    #[tokio::test]
    async fn exec_translates_the_env_object_into_pairs() {
        let (services, stubs) = server_with_actions();
        let out = call(
            &services,
            "remote_exec",
            json!({
                "target": "dev",
                "argv": ["make"],
                "cwd": "build",
                "env": { "BOARD": "rpi", "JOBS": "8" },
                "timeoutMs": 120000
            }),
        )
        .await;
        assert!(!is_error(&out), "{}", text_of(&out));
        // preserve_order keeps the object's insertion order stable for the assert.
        assert_eq!(
            stubs[0].seen.lock().unwrap()[0],
            json!({
                "target": "dev",
                "argv": ["make"],
                "cwd": "build",
                "env": ["BOARD=rpi", "JOBS=8"],
                "timeoutMs": 120000
            })
        );
    }

    #[tokio::test]
    async fn bad_calls_are_refused_in_band_naming_the_problem() {
        let (services, _stubs) = server_with_actions();
        let out = call(&services, "remote_nope", json!({ "target": "dev" })).await;
        assert!(is_error(&out));
        assert!(text_of(&out).contains("unknown tool"), "{}", text_of(&out));

        let out = call(&services, "remote_exec", json!({ "target": "dev" })).await;
        assert!(is_error(&out));
        assert!(text_of(&out).contains("argv"), "{}", text_of(&out));

        let out = call(
            &services,
            "remote_exec",
            json!({ "target": "dev", "argv": ["ls"], "wat": 1 }),
        )
        .await;
        assert!(is_error(&out));
        assert!(
            text_of(&out).contains("wat"),
            "the unknown argument is named: {}",
            text_of(&out)
        );

        let out = call(
            &services,
            "remote_exec",
            json!({ "target": "dev", "argv": ["ls"], "timeoutMs": 0 }),
        )
        .await;
        assert!(is_error(&out));
        assert!(text_of(&out).contains("timeoutMs"), "{}", text_of(&out));
    }

    #[tokio::test]
    async fn a_missing_action_names_the_plugin_not_the_protocol() {
        // No stubs registered at all: the tools exist, the actions do not - the
        // exact shape a disabled Remote plugin presents.
        let services = RuntimeServices::new();
        let out = call(
            &services,
            "remote_cat",
            json!({ "target": "dev", "remote": "x.txt" }),
        )
        .await;
        assert!(is_error(&out));
        let text = text_of(&out);
        assert!(
            text.contains("remote.cat"),
            "the missing action is named: {text}"
        );
        assert!(
            text.contains("enable the Remote plugin"),
            "the fix is named: {text}"
        );
    }
}
