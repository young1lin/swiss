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

//! The repo's own stdio MCP server for the L3 suite (docs/44 SS2.7).
//!
//! Five tools, none of them for looks - each one exists to make a proc-adapter
//! behavior observable from the far side of a real child process:
//!
//! - `echo {text}` - UTF-8 (Chinese, emoji, quotes) survives the stdio round trip.
//! - `blob {kb}` - a kb-KB JSON document as TEXT; the gateway's proc path forwards
//!   payloads as &RawValue, so what comes back must be the SAME BYTES the server
//!   wrote - not merely equal JSON.
//! - `sleep {ms}` - a tool that takes its time: timeouts and cancellation have
//!   something real to bite on.
//! - `fail {code}` - a deliberate MCP error: error mapping without killing the
//!   process (the next call still answers).
//! - `env` - the environment NAMES the child sees: the daemon scrubs the launcher's
//!   noise (docs/16 H1) before any child inherits it.
//!
//! Startup writes one Chinese line to stderr ON PURPOSE: stderr noise must never
//! leak into the stdout protocol stream, and this server is the proof vehicle.

use std::future::Future;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerInfo, Tool, ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{RoleServer, ServerHandler};
use serde_json::json;

/// The deterministic blob document: exactly `kb` * 1024 BYTES of valid JSON, so
/// the test can rebuild the same string locally and demand byte equality.
pub fn blob_document(kb: usize) -> String {
    let head = format!("{{\"kb\":{kb},\"blob\":\"");
    let tail = "\"}";
    let middle = kb.saturating_mul(1024).saturating_sub(head.len() + tail.len());
    let filler = "swiss-it-l3-fill.";
    let mut body = String::with_capacity(head.len() + middle + tail.len());
    body.push_str(&head);
    let mut left = middle;
    while left > 0 {
        let take = left.min(filler.len());
        body.push_str(&filler[..take]);
        left -= take;
    }
    body.push_str(tail);
    body
}

/// The five tool definitions. Hand-written `ServerHandler` (no macros feature),
/// the same shape the shipped adapters use.
struct L3Server;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // The deliberate stderr noise (see the module doc): if this ever leaked into
    // stdout, the JSON-RPC stream would corrupt and no test below would pass.
    eprintln!("swiss-it: L3 server is up; the stderr noise must never reach the stdout protocol stream");
    let transport =
        AsyncRwTransport::<RoleServer, _, _>::new(tokio::io::stdin(), tokio::io::stdout());
    match rmcp::service::serve_server(L3Server, transport).await {
        Ok(service) => {
            // Serves until the gateway closes stdin (stop, reap, or our own death).
            let _ = service.waiting().await;
        }
        Err(e) => {
            eprintln!("swiss-it: serve ended: {e}");
            std::process::exit(1);
        }
    }
}

fn tool(name: &str, description: &str, props: serde_json::Value) -> Tool {
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "properties": props,
    }))
    .expect("the literal above is a JSON object");
    let mut t = Tool::default();
    t.name = name.to_string().into();
    t.description = Some(description.to_string().into());
    t.input_schema = std::sync::Arc::new(schema);
    t
}

fn tools() -> Vec<Tool> {
    vec![
        tool(
            "echo",
            "echo the text back, UTF-8 intact",
            json!({ "text": { "type": "string" } }),
        ),
        tool(
            "blob",
            "return a kb-KB JSON document as text, byte-for-byte deterministic",
            json!({ "kb": { "type": "number" } }),
        ),
        tool(
            "sleep",
            "sleep ms milliseconds, then answer",
            json!({ "ms": { "type": "number" } }),
        ),
        tool(
            "fail",
            "answer with a deliberate MCP error carrying the code; the process stays alive",
            json!({ "code": { "type": "number" } }),
        ),
        tool("env", "the environment variable NAMES the child sees, one per line", json!({})),
    ]
}

impl ServerHandler for L3Server {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());
        let mut info = Implementation::default();
        info.name = "it-mcp-server".into();
        info.version = "1.0".into();
        ServerInfo::new(capabilities).with_server_info(info)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let name = request.name.to_string();
        let args = request.arguments;
        async move {
            let arg = |key: &str| args.as_ref().and_then(|a| a.get(key));
            match name.as_str() {
                "echo" => {
                    let text = arg("text")
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
                }
                "blob" => {
                    let kb = arg("kb").and_then(serde_json::Value::as_u64).unwrap_or(1) as usize;
                    // Clamp: this server is a test instrument, not a firehose.
                    let kb = kb.clamp(1, 512);
                    let doc = blob_document(kb);
                    assert_eq!(doc.len(), kb * 1024, "the builder sizes the document exactly");
                    Ok(CallToolResult::success(vec![ContentBlock::text(doc)]).into())
                }
                "sleep" => {
                    let ms = arg("ms").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    // A real pause, not a ready future: a caller's timeout (and the
                    // suite's elapsed-time assert) needs something to race against.
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "slept {ms}ms"
                    ))])
                    .into())
                }
                "fail" => {
                    let code = arg("code").and_then(serde_json::Value::as_i64).unwrap_or(0);
                    Err(ErrorData::internal_error(
                        format!("it-l3-fail-{code} (deliberate)"),
                        None,
                    ))
                }
                "env" => {
                    let mut names: Vec<String> = std::env::vars_os()
                        .map(|(k, _)| k.to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    Ok(CallToolResult::success(vec![ContentBlock::text(names.join("\n"))]).into())
                }
                other => Err(ErrorData::invalid_params(
                    format!("unknown tool: {other}"),
                    None,
                )),
            }
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }
}
