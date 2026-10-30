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

//! A fixture MCP: an in-process tool server with resources, for the cases that need a live one.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_mcp::adapters::resources::{
    ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate,
};
use swiss_mcp::adapters::tool_server::{Engine, ServerMeta, ToolDef};

// --- a fixture MCP -----------------------------------------------------------------------------

/// An in-process MCP with a tool, a resource and a health probe — the Rust stand-in for the Node
/// suite's `fixtures/stdio-echo.mjs`.
///
/// The Node build spawned a child `node` whenever a test needed something with resources to page
/// and a probe that answers. A Rust test has no `node` to lean on and needs none: `DirectAdapter`
/// serves any `Engine`, and nothing in the admin API can tell this apart from a database adapter.
pub(super) struct Fixture {
    name: std::sync::RwLock<String>,
}

impl Fixture {
    pub(super) fn new(name: &str) -> Self {
        Self {
            name: std::sync::RwLock::new(name.to_string()),
        }
    }

    fn name(&self) -> String {
        self.name.read().map(|n| n.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Engine for Fixture {
    fn kind(&self) -> &'static str {
        "fixture"
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "echo".into(),
            description: "echo back the message".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"],
            }),
        }]
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        if tool != "echo" {
            return Err(format!("unknown tool: {tool}"));
        }
        Ok(json!({ "echo": args.get("msg").cloned().unwrap_or(Value::Null) }))
    }

    fn meta(&self) -> ServerMeta {
        ServerMeta {
            name: Some(self.name()),
            description: None,
            target: None,
            limits: None,
        }
    }

    fn resources(&self) -> Option<Arc<dyn ResourceProvider>> {
        Some(Arc::new(FixtureResources {
            uri: format!("echo://{}", self.name()),
        }))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        Some(Ok(()))
    }

    fn rename(&self, next: &str) {
        if let Ok(mut name) = self.name.write() {
            *name = next.to_string();
        }
    }
}

/// One resource, named after the MCP — enough to prove the resources page is served from the
/// engine rather than from a cache.
struct FixtureResources {
    uri: String,
}

#[async_trait]
impl ResourceProvider for FixtureResources {
    async fn list(&self, _cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        Ok(ResourcePage {
            resources: vec![ResourceEntry {
                uri: self.uri.clone(),
                name: "echo".into(),
                description: None,
                mime_type: Some("text/plain".into()),
            }],
            next_cursor: None,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        if uri != self.uri {
            return Err(ResourceFault(format!("unknown resource: {uri}")));
        }
        Ok(vec![ResourceBody {
            uri: self.uri.clone(),
            mime_type: Some("text/plain".into()),
            text: "hello".into(),
        }])
    }
}
