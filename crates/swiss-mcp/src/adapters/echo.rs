/*
 * Copyright 2026 The swiss authors
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

//! The echo adapter — port of `adapters/echo.ts`.
//!
//! The Node build calls this "the shipped demo MCP" and a health probe: one tool, no driver, no
//! child process. It is the first MCP a new user clicks, so its behaviour is pinned by the same
//! tests as everything else. A NAMED echo server logs its calls like every other adapter (the
//! wrapper arrives with `calls.rs` in Phase 2); the nameless variant stays unlogged, which the
//! call log already handles by dropping unnamed MCPs.

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
use serde_json::json;

/// One `echo` tool: returns its `msg` argument as text. `ServerHandler` is implemented by hand
/// rather than through the `#[tool_router]` macros — the Node build registers `tools/list` and
/// `tools/call` handlers directly, and the hand-written form ports that shape exactly while
/// keeping the `macros` feature (and `pastey`) out of the build.
#[derive(Clone)]
pub struct EchoServer {
    /// The MCP's registry name, followed across renames — the call-log wrapper reads it at call
    /// time. `None` is the nameless singleton (tests), whose calls are not logged.
    pub name: Option<Arc<std::sync::RwLock<String>>>,
    /// Who is calling — captured at factory time and carried here, because the handler runs on
    /// rmcp's own task where no task-local reaches (see `calls::CallSource`).
    pub source: crate::calls::CallSource,
    /// Where the call is recorded: the app's CallLog, threaded in at construction.
    pub log: std::sync::Arc<crate::calls::CallLog>,
}

/// The default is the nameless singleton (tests): no registry name, calls not recorded.
/// Manual because [CallLog] has neither Default nor Debug - an Arc to it is all a server
/// needs, and a default server simply records nowhere.
impl Default for EchoServer {
    fn default() -> Self {
        Self {
            name: None,
            source: crate::calls::CallSource::default(),
            log: std::sync::Arc::new(crate::calls::CallLog::at(
                std::env::temp_dir()
                    .join(format!("swiss-echo-nolog-{}", swiss_core::util::random_hex(8))),
            )),
        }
    }
}

/// The tool definition, byte-for-byte the schema the Node build publishes. The rmcp model types
/// are `#[non_exhaustive]`, so they are built from `Default` + field assignment rather than
/// struct literals.
fn echo_tool() -> Tool {
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "properties": { "msg": { "type": "string" } },
        "required": ["msg"],
    }))
    .expect("the literal above is a JSON object");
    let mut tool = Tool::default();
    tool.name = "echo".into();
    tool.description = Some("echo back the message".into());
    tool.input_schema = Arc::new(schema);
    tool
}

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());
        let mut server_info = Implementation::default();
        server_info.name = "echo".into();
        server_info.version = "1.0".into();
        ServerInfo::new(capabilities).with_server_info(server_info)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(vec![echo_tool()])))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        // Wrapped so the panel's Logs tab shows echo calls too — the shipped demo MCP is the
        // first one a new user clicks, and an empty log there reads like a broken log.
        let mcp = self
            .name
            .as_ref()
            .and_then(|n| n.read().ok().map(|g| g.clone()));
        let source = self.source.clone();
        let log = self.log.clone();
        let tool = request.name.to_string();
        let args = request
            .arguments
            .clone()
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);
        async move {
            log.logged(
                mcp.as_deref(),
                &source,
                &tool,
                if args.is_null() { None } else { Some(args) },
                async move {
                    // String(msg ?? "") — a missing or non-string argument echoes as empty
                    // text, not an error; the Node build behaves the same and clients rely on
                    // the gentle shape.
                    let text = request
                        .arguments
                        .as_ref()
                        .and_then(|args| args.get("msg"))
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    Ok::<_, ErrorData>(
                        CallToolResult::success(vec![ContentBlock::text(text)]).into(),
                    )
                },
                |result: &CallToolResponse| {
                    // echo never reports failure in-band, so every reply logs ok. The text is
                    // flattened the way contentText does on the wire ({content:[{type,text}]}).
                    let text = match result {
                        CallToolResponse::Complete(r) => r
                            .content
                            .iter()
                            .map(|c| match c {
                                ContentBlock::Text(t) => t.text.clone(),
                                other => format!("[{other:?} content]"),
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => String::new(),
                    };
                    (true, text)
                },
            )
            .await
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        // Feeds the transport's SEP-2243 `Mcp-Param-*` header validation, which looks the called
        // tool's schema up by name. Returning the definition (rather than the default None)
        // keeps that validation working exactly as it does for macro-built servers.
        (name == "echo").then(echo_tool)
    }
}

/// A named echo adapter — port of `makeEchoAdapter`. Calls land in the Logs tab under this MCP's
/// name; the name is mutable (rename support, like every other adapter) so a renamed MCP keeps
/// logging under its new name instead of splitting the log across two keys.
pub struct EchoAdapter {
    name: Arc<std::sync::RwLock<String>>,
    log: std::sync::Arc<crate::calls::CallLog>,
}

impl EchoAdapter {
    pub fn new(name: &str, log: std::sync::Arc<crate::calls::CallLog>) -> Self {
        Self {
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            log,
        }
    }
}

#[async_trait]
impl super::Adapter for EchoAdapter {
    fn kind(&self) -> &str {
        "echo"
    }

    async fn build(&self) -> Result<super::McpEndpoint, String> {
        // A fresh server per request — see rmcp_endpoint: concurrent requests never share one
        // transport-bound server instance. Each shares this adapter's name cell, so the log
        // wrapper always sees the current registry name, and receives the call source at factory
        // time (see calls::CallSource).
        let name = self.name.clone();
        let log = self.log.clone();
        Ok(super::rmcp_endpoint(move |source| EchoServer {
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

    // The trait's async methods return `impl Future` and are driven end-to-end by the
    // integration tests through a real client; these unit tests pin the pure parts only.
    #[test]
    fn tool_definition_matches_the_node_build() {
        let tool = echo_tool();
        assert_eq!(tool.name, "echo");
        assert_eq!(tool.description.as_deref(), Some("echo back the message"));
        let schema = serde_json::to_value(&*tool.input_schema).expect("schema serializes");
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"],
            })
        );
    }

    #[test]
    fn get_tool_returns_echo_by_name_only() {
        assert!(EchoServer::default().get_tool("echo").is_some());
        assert!(EchoServer::default().get_tool("nope").is_none());
    }
}
