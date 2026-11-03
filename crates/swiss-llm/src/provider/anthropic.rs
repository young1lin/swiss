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

//! The Anthropic Messages wire, swiss-owned like the OpenAI one (SPEC §llm.providers):
//! the request body and the reading of its named SSE events. genai's adapter only
//! speaks thinking to the Claude models it lists, so an Anthropic-compatible endpoint
//! (GLM, Kimi, DeepSeek) could never hear an effort; this module builds the body
//! itself. A round's signed thinking blocks travel back in its ThoughtSignature part,
//! so a thinking model's tool turn replays with its signature chain intact.

use std::collections::{BTreeMap, BTreeSet};

use genai::chat::{ChatOptions, ChatRequest, ChatRole, ContentPart, MessageContent, ReasoningEffort, StopReason, StreamEnd, ToolCall, Usage};
use serde_json::{json, Value};

use super::stream::{wire_model, Assembly, Step, StreamFault};
use super::Delta;

/// The API version every request names.
pub(super) const VERSION: &str = "2023-06-01";
/// The answer's ceiling when the options carry none: the wire requires one.
const MAX_TOKENS_DEFAULT: u32 = 4096;
/// The smallest thinking budget the wire takes.
const BUDGET_MIN: u32 = 1024;
/// The budgets an older Claude thinks on, by effort (genai's own mapping).
const BUDGET_MEDIUM: u32 = 8000;
const BUDGET_HIGH: u32 = 24000;

/// Whether a model thinks on a token budget rather than an effort: a Claude older than
/// the 4.6 generation ("claude-3-7-sonnet", "claude-sonnet-4-5", "claude-opus-4-1",
/// "claude-sonnet-4-20250514"). Every other model - a newer Claude, or GLM, Kimi or
/// DeepSeek behind an Anthropic-compatible endpoint - takes adaptive thinking and an
/// effort.
fn thinks_on_budget(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let Some(at) = model.find("claude-") else { return false };
    let mut parts = model[at + "claude-".len()..].split(['-', '.']);
    let first = parts.next().unwrap_or("");
    // The old naming puts the version first: "claude-3-5-sonnet".
    if !first.is_empty() && first.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else { return false };
    // A dated id ("claude-sonnet-4-20250514") carries no minor version.
    let minor = parts.next().and_then(|p| p.parse::<u32>().ok()).filter(|m| *m < 100).unwrap_or(0);
    (major, minor) < (4, 6)
}

/// The `thinking` (and `output_config`) one effort puts on the body. No effort sends
/// neither: the model thinks as it does by default (GLM always does, Claude does not).
fn put_thinking(payload: &mut Value, model: &str, effort: Option<&ReasoningEffort>, max_tokens: u32) {
    let word = match effort {
        None => return,
        Some(ReasoningEffort::None) => {
            payload["thinking"] = json!({ "type": "disabled" });
            return;
        }
        Some(ReasoningEffort::Budget(budget)) => {
            payload["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            return;
        }
        Some(ReasoningEffort::Minimal | ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::Medium) => "medium",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::XHigh) => "xhigh",
        Some(ReasoningEffort::Max) => "max",
    };
    if thinks_on_budget(model) {
        let budget = match word {
            "low" => BUDGET_MIN,
            "medium" => BUDGET_MEDIUM,
            _ => BUDGET_HIGH,
        };
        // The budget stays under the answer's ceiling; a ceiling too small to think
        // in asks without thinking rather than be refused.
        let budget = budget.min(max_tokens.saturating_sub(1));
        if budget >= BUDGET_MIN {
            payload["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
        return;
    }
    payload["thinking"] = json!({ "type": "adaptive" });
    payload["output_config"] = json!({ "effort": word });
}

/// The signed thinking blocks one round carried, as [Reading] wrote them into the
/// round's ThoughtSignature part: each a `thinking` block with its signature or a
/// `redacted_thinking` block, replayed exactly as they came.
fn signed_blocks(text: &str) -> Result<Vec<Value>, StreamFault> {
    const UNREADABLE: StreamFault = StreamFault::Protocol("the Anthropic request carries an unreadable thinking signature");
    let blocks: Vec<Value> = serde_json::from_str(text).map_err(|_| UNREADABLE)?;
    for block in &blocks {
        let whole = match block["type"].as_str() {
            Some("thinking") => block["thinking"].is_string() && block["signature"].as_str().is_some_and(|s| !s.is_empty()),
            Some("redacted_thinking") => block["data"].as_str().is_some_and(|d| !d.is_empty()),
            _ => false,
        };
        if !whole {
            return Err(UNREADABLE);
        }
    }
    Ok(blocks)
}

/// The request body for one streamed round trip. System messages join the system
/// field; a tool's answers ride as `tool_result` blocks of a user turn; consecutive
/// turns of one role merge into one message, since the wire alternates. Reasoning
/// without a signature never replays here (the wire refuses unsigned thinking), and
/// signed thinking replays unless this request turns thinking off.
pub(super) fn payload(model: &str, request: ChatRequest, options: &ChatOptions) -> Result<Value, StreamFault> {
    let (effort, model) = wire_model(model, options);
    let replay_thinking = !matches!(effort, Some(ReasoningEffort::None));
    let mut system: Vec<String> = request.system.into_iter().filter(|s| !s.is_empty()).collect();
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for message in request.messages {
        let (role, blocks) = match message.role {
            ChatRole::System | ChatRole::User => {
                if !message.content.is_text_only() {
                    return Err(StreamFault::Protocol("the Anthropic request contains unsupported message content"));
                }
                let text = message.content.joined_texts().unwrap_or_default();
                if matches!(message.role, ChatRole::System) {
                    if !text.is_empty() {
                        system.push(text);
                    }
                    continue;
                }
                let blocks = if text.is_empty() { Vec::new() } else { vec![json!({ "type": "text", "text": text })] };
                ("user", blocks)
            }
            ChatRole::Assistant => {
                let (mut signed, mut texts, mut calls) = (Vec::new(), Vec::new(), Vec::new());
                for part in message.content {
                    match part {
                        ContentPart::Text(text) if text.is_empty() => {}
                        ContentPart::Text(text) => texts.push(json!({ "type": "text", "text": text })),
                        ContentPart::ReasoningContent(_) => {}
                        ContentPart::ThoughtSignature(blocks) => {
                            if replay_thinking {
                                signed.extend(signed_blocks(&blocks)?);
                            }
                        }
                        ContentPart::ToolCall(call) => {
                            if call.call_id.trim().is_empty() || call.fn_name.trim().is_empty() || !call.fn_arguments.is_object() {
                                return Err(StreamFault::Protocol("the Anthropic request contains a malformed tool call"));
                            }
                            calls.push(json!({ "type": "tool_use", "id": call.call_id, "name": call.fn_name, "input": call.fn_arguments }));
                        }
                        _ => return Err(StreamFault::Protocol("the Anthropic request contains unsupported assistant content")),
                    }
                }
                // The wire wants a thinking turn's blocks first, then its text, then its calls.
                signed.extend(texts);
                signed.extend(calls);
                ("assistant", signed)
            }
            ChatRole::Tool => {
                let mut blocks = Vec::new();
                for part in message.content {
                    match part {
                        ContentPart::ToolResponse(response) if !response.call_id.trim().is_empty() => {
                            blocks.push(json!({ "type": "tool_result", "tool_use_id": response.call_id, "content": response.content }));
                        }
                        _ => return Err(StreamFault::Protocol("the Anthropic request contains a malformed tool response")),
                    }
                }
                ("user", blocks)
            }
        };
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((last, held)) if *last == role => held.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }
    let messages: Vec<Value> = turns.into_iter().map(|(role, content)| json!({ "role": role, "content": content })).collect();
    let max_tokens = options.max_tokens.unwrap_or(MAX_TOKENS_DEFAULT);
    // Automatic caching keeps the breakpoint at the growing conversation's tail.
    let mut payload = json!({ "model": model, "max_tokens": max_tokens, "messages": messages, "stream": true,
        "cache_control": { "type": "ephemeral", "ttl": "5m" } });
    if !system.is_empty() {
        payload["system"] = json!(system.join("\n\n"));
    }
    put_thinking(&mut payload, model, effort.as_ref(), max_tokens);
    if let Some(temperature) = options.temperature { payload["temperature"] = json!(temperature); }
    if let Some(top_p) = options.top_p { payload["top_p"] = json!(top_p); }
    if !options.stop_sequences.is_empty() { payload["stop_sequences"] = json!(options.stop_sequences); }
    if let Some(tools) = request.tools {
        let tools: Vec<Value> = tools.into_iter().map(|tool| {
            let mut one = json!({
                "name": tool.name.as_str(),
                "input_schema": tool.schema.unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            });
            if let Some(description) = tool.description { one["description"] = json!(description); }
            one
        }).collect();
        if !tools.is_empty() {
            payload["tools"] = Value::Array(tools);
        }
    }
    Ok(payload)
}

/// One content block as it arrives.
enum Block {
    Text(String),
    Thinking { text: String, signature: String },
    Redacted(String),
    /// The call's id and name from its start, its input as the start gave it and the
    /// JSON fragments streamed after.
    ToolUse { id: String, name: String, start: Value, fragments: String },
    /// A block this reader does not use (a server tool's, a citation's): kept so its
    /// deltas are recognized and passed over.
    Other,
}

/// Token counts as the wire reports them: the start's, then the closing delta's, each
/// field the latest seen.
#[derive(Default)]
struct WireUsage {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_creation: Option<i64>,
}

impl WireUsage {
    fn merge(&mut self, usage: &Value) {
        let n = |key: &str| usage.get(key).and_then(Value::as_i64);
        self.input = n("input_tokens").or(self.input);
        self.output = n("output_tokens").or(self.output);
        self.cache_read = n("cache_read_input_tokens").or(self.cache_read);
        self.cache_creation = n("cache_creation_input_tokens").or(self.cache_creation);
    }

    /// In genai's (OpenAI's) sense, where the prompt count includes the cache.
    fn usage(&self) -> Option<Usage> {
        if self.input.is_none() && self.output.is_none() {
            return None;
        }
        let (read, created) = (self.cache_read.unwrap_or(0), self.cache_creation.unwrap_or(0));
        let prompt = self.input.unwrap_or(0) + read + created;
        let output = self.output.unwrap_or(0);
        let mut usage: Usage = serde_json::from_value(json!({
            "prompt_tokens": prompt,
            "completion_tokens": output,
            "total_tokens": prompt + output,
            "prompt_tokens_details": { "cached_tokens": read, "cache_creation_tokens": created },
        }))
        .ok()?;
        usage.compact_details();
        Some(usage)
    }
}

/// The reading of one Anthropic stream: blocks by index, the stop, the usage.
#[derive(Default)]
pub(super) struct Reading {
    blocks: BTreeMap<u64, Block>,
    stop: Option<String>,
    usage: WireUsage,
    response_id: Option<String>,
}

const MALFORMED: StreamFault = StreamFault::Protocol("the provider's stream chunk was malformed");

impl Reading {
    fn index(data: &Value) -> Result<u64, StreamFault> {
        data["index"].as_u64().ok_or(MALFORMED)
    }

    fn start(&mut self, data: &Value) -> Result<Vec<Delta>, StreamFault> {
        let index = Self::index(data)?;
        let block = &data["content_block"];
        let text = |key: &str| block[key].as_str().unwrap_or("").to_string();
        let mut deltas = Vec::new();
        let opened = match block["type"].as_str() {
            Some("text") => {
                let first = text("text");
                if !first.is_empty() { deltas.push(Delta::Text(first.clone())); }
                Block::Text(first)
            }
            Some("thinking") => {
                let first = text("thinking");
                if !first.is_empty() { deltas.push(Delta::Thinking(first.clone())); }
                Block::Thinking { text: first, signature: text("signature") }
            }
            Some("redacted_thinking") => Block::Redacted(text("data")),
            Some("tool_use") => Block::ToolUse { id: text("id"), name: text("name"), start: block["input"].clone(), fragments: String::new() },
            Some(_) => Block::Other,
            None => return Err(MALFORMED),
        };
        if self.blocks.insert(index, opened).is_some() {
            return Err(StreamFault::Protocol("the provider restarted a content block"));
        }
        Ok(deltas)
    }

    fn delta(&mut self, data: &Value) -> Result<Vec<Delta>, StreamFault> {
        let index = Self::index(data)?;
        let block = self.blocks.get_mut(&index).ok_or(StreamFault::Protocol("the provider streamed into a block it never started"))?;
        let delta = &data["delta"];
        let piece = |key: &str| delta[key].as_str().ok_or(MALFORMED);
        const MISMATCH: StreamFault = StreamFault::Protocol("the provider's delta does not match its block");
        match (delta["type"].as_str(), block) {
            (Some("text_delta"), Block::Text(text)) => {
                let one = piece("text")?;
                text.push_str(one);
                Ok(if one.is_empty() { Vec::new() } else { vec![Delta::Text(one.to_string())] })
            }
            (Some("thinking_delta"), Block::Thinking { text, .. }) => {
                let one = piece("thinking")?;
                text.push_str(one);
                Ok(if one.is_empty() { Vec::new() } else { vec![Delta::Thinking(one.to_string())] })
            }
            (Some("signature_delta"), Block::Thinking { signature, .. }) => {
                signature.push_str(piece("signature")?);
                Ok(Vec::new())
            }
            (Some("input_json_delta"), Block::ToolUse { fragments, .. }) => {
                fragments.push_str(piece("partial_json")?);
                Ok(Vec::new())
            }
            (Some("text_delta" | "thinking_delta" | "signature_delta" | "input_json_delta"), Block::Other) => Ok(Vec::new()),
            (Some("text_delta" | "thinking_delta" | "signature_delta" | "input_json_delta"), _) => Err(MISMATCH),
            // A delta kind this reader does not use (citations) changes nothing it keeps.
            (Some(_), _) => Ok(Vec::new()),
            (None, _) => Err(MALFORMED),
        }
    }

    fn finish(&mut self) -> Result<StreamEnd, StreamFault> {
        let this = std::mem::take(self);
        let stop = this.stop.ok_or(StreamFault::Protocol("the provider's stream has no stop reason"))?;
        let cut = stop == "max_tokens";
        let (mut text, mut thinking, mut signed, mut calls) = (String::new(), Vec::new(), Vec::new(), Vec::new());
        let mut ids = BTreeSet::new();
        for block in this.blocks.into_values() {
            match block {
                Block::Text(one) => text.push_str(&one),
                Block::Thinking { text: one, signature } => {
                    if !signature.is_empty() {
                        signed.push(json!({ "type": "thinking", "thinking": one, "signature": signature }));
                    }
                    if !one.is_empty() { thinking.push(one); }
                }
                Block::Redacted(data) => {
                    if !data.is_empty() { signed.push(json!({ "type": "redacted_thinking", "data": data })); }
                }
                // A cut turn's calls may be half-written; it never executes them.
                Block::ToolUse { .. } if cut => {}
                Block::ToolUse { id, name, start, fragments } => {
                    if id.trim().is_empty() || name.trim().is_empty() || !ids.insert(id.clone()) {
                        return Err(StreamFault::Protocol("the provider's tool call id or name is missing or duplicated"));
                    }
                    let arguments = if fragments.trim().is_empty() {
                        if start.is_null() { json!({}) } else { start }
                    } else {
                        serde_json::from_str(&fragments).map_err(|_| StreamFault::Protocol("the provider's tool call arguments are not valid JSON"))?
                    };
                    if !arguments.is_object() {
                        return Err(StreamFault::Protocol("the provider's tool call arguments are not a JSON object"));
                    }
                    calls.push(ContentPart::ToolCall(ToolCall { call_id: id, fn_name: name, fn_arguments: arguments, thought_signatures: None }));
                }
                Block::Other => {}
            }
        }
        if stop == "tool_use" && calls.is_empty() {
            return Err(StreamFault::Protocol("the provider finished tool calls without a call"));
        }
        let mut parts = Vec::new();
        if !signed.is_empty() {
            parts.push(ContentPart::ThoughtSignature(Value::Array(signed).to_string()));
        }
        if !text.is_empty() {
            parts.push(ContentPart::Text(text));
        }
        parts.extend(calls);
        Ok(StreamEnd {
            captured_usage: this.usage.usage(),
            captured_stop_reason: Some(StopReason::from(stop)),
            captured_content: Some(MessageContent::from_parts(parts)),
            captured_reasoning_content: (!thinking.is_empty()).then(|| thinking.join("\n\n")),
            captured_response_id: this.response_id,
        })
    }
}

impl Assembly for Reading {
    fn event(&mut self, event: Option<&str>, data: &str) -> Result<Step, StreamFault> {
        let parsed: Value = serde_json::from_str(data).map_err(|_| MALFORMED)?;
        if !parsed.is_object() {
            return Err(MALFORMED);
        }
        // The data names its own type; a compatible endpoint that leaves it out still
        // frames the event's name.
        let kind = parsed["type"].as_str().or(event).unwrap_or("");
        let deltas = match kind {
            "message_start" => {
                let message = &parsed["message"];
                if let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) {
                    self.response_id = Some(id.to_string());
                }
                self.usage.merge(&message["usage"]);
                Vec::new()
            }
            "content_block_start" => self.start(&parsed)?,
            "content_block_delta" => self.delta(&parsed)?,
            "message_delta" => {
                if let Some(stop) = parsed["delta"]["stop_reason"].as_str() {
                    self.stop = Some(stop.to_string());
                }
                self.usage.merge(&parsed["usage"]);
                Vec::new()
            }
            "message_stop" => return self.finish().map(Step::End),
            "error" => return Err(StreamFault::Provider(data.to_string())),
            // content_block_stop, ping and any event this reader does not use.
            _ => Vec::new(),
        };
        Ok(Step::Deltas(deltas))
    }

    fn began(&self) -> bool {
        // A block opens when the model starts thinking, writing or calling a tool;
        // message_start and ping come before any of it.
        !self.blocks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai::chat::{ChatMessage, Tool, ToolResponse};
    use std::io;

    fn options(effort: Option<ReasoningEffort>, max_tokens: u32) -> ChatOptions {
        ChatOptions { max_tokens: Some(max_tokens), reasoning_effort: effort, ..ChatOptions::default() }
    }

    fn frame(data: Value) -> String {
        format!("event: {}\ndata: {data}\n\n", data["type"].as_str().unwrap_or("x"))
    }

    async fn read(frames: &[Value]) -> Result<(StreamEnd, Vec<(&'static str, String)>), StreamFault> {
        let body: String = frames.iter().cloned().map(frame).collect();
        let source = futures_util::stream::iter(vec![Ok::<_, io::Error>(body.into_bytes())]);
        let mut observed = Vec::new();
        let mut sink = |delta| {
            observed.push(match delta { Delta::Thinking(text) => ("thinking", text), Delta::Text(text) => ("text", text) });
            std::future::ready(true)
        };
        let end = super::super::stream::drive(source, &mut sink, Reading::default(), &mut None).await?.expect("this sink always continues");
        Ok((end, observed))
    }

    fn start(index: u64, block: Value) -> Value {
        json!({ "type": "content_block_start", "index": index, "content_block": block })
    }

    fn delta(index: u64, delta: Value) -> Value {
        json!({ "type": "content_block_delta", "index": index, "delta": delta })
    }

    fn stop(reason: &str) -> [Value; 2] {
        [
            json!({ "type": "message_delta", "delta": { "stop_reason": reason }, "usage": { "output_tokens": 40 } }),
            json!({ "type": "message_stop" }),
        ]
    }

    #[test]
    fn cache_context_defaults_to_five_minutes_on_the_anthropic_wire_only() {
        for model in ["claude-sonnet-4-6", "glm-5.3-flash"] {
            let sent = payload(model, ChatRequest::from_user("q"), &options(None, 4096)).expect("payload");
            assert_eq!(sent["cache_control"], json!({ "type": "ephemeral", "ttl": "5m" }));
        }
        let openai = super::super::stream::openai_payload("m", ChatRequest::from_user("q"), &options(None, 4096)).expect("payload");
        assert!(openai.get("cache_control").is_none(), "an Anthropic field never reaches an OpenAI endpoint");
    }

    #[test]
    fn effort_maps_to_adaptive_thinking_except_on_an_older_claude() {
        let body = |model: &str, effort: Option<ReasoningEffort>, max: u32| {
            payload(model, ChatRequest::from_user("q"), &options(effort, max)).expect("payload")
        };
        // GLM (and any non-Claude model) hears adaptive thinking and the effort word.
        let glm = body("glm-5.3-flash", Some(ReasoningEffort::Low), 8192);
        assert_eq!((glm["thinking"].clone(), glm["output_config"].clone()), (json!({ "type": "adaptive" }), json!({ "effort": "low" })));
        assert_eq!(body("glm-5.3-flash", Some(ReasoningEffort::Max), 8192)["output_config"]["effort"], "max");
        assert_eq!(body("claude-opus-5-5", Some(ReasoningEffort::Medium), 8192)["output_config"]["effort"], "medium");
        assert_eq!(body("claude-sonnet-4-6", Some(ReasoningEffort::Minimal), 8192)["output_config"]["effort"], "low");
        // No effort: neither field - the model thinks as it does by default.
        let plain = body("glm-5.3-flash", None, 8192);
        assert!(plain.get("thinking").is_none() && plain.get("output_config").is_none(), "{plain}");
        // A model-name suffix is the effort when the options carry none, and leaves the name.
        let suffixed = body("glm-5.3-flash-high", None, 8192);
        assert_eq!((suffixed["model"].as_str(), suffixed["output_config"]["effort"].as_str()), (Some("glm-5.3-flash"), Some("high")));
        // An older Claude thinks on a budget kept under the ceiling, or not at all.
        for model in ["claude-sonnet-4-5", "claude-3-7-sonnet-latest", "claude-opus-4-1", "claude-sonnet-4-20250514"] {
            let legacy = body(model, Some(ReasoningEffort::High), 8192);
            assert_eq!(legacy["thinking"], json!({ "type": "enabled", "budget_tokens": 8191 }), "{model}");
            assert!(legacy.get("output_config").is_none(), "{model}");
        }
        assert_eq!(body("claude-haiku-4-5", Some(ReasoningEffort::Low), 8192)["thinking"]["budget_tokens"], 1024);
        assert!(body("claude-haiku-4-5", Some(ReasoningEffort::Low), 1024).get("thinking").is_none(), "no room to think");
        assert_eq!(body("glm-5.3-flash", Some(ReasoningEffort::None), 8192)["thinking"], json!({ "type": "disabled" }));
        assert_eq!(body("glm-5.3-flash", Some(ReasoningEffort::Budget(2000)), 8192)["thinking"], json!({ "type": "enabled", "budget_tokens": 2000 }));
    }

    #[test]
    fn payload_spells_turns_tools_and_signed_thinking_the_wire_way() {
        let signed = json!([
            { "type": "thinking", "thinking": "weigh it", "signature": "sig-1" },
            { "type": "redacted_thinking", "data": "opaque" }
        ])
        .to_string();
        let call = ToolCall { call_id: "toolu_1".into(), fn_name: "read".into(), fn_arguments: json!({ "path": "a.rs" }), thought_signatures: None };
        let request = ChatRequest::from_system("be brief")
            .append_message(ChatMessage::system("and exact"))
            .append_message(ChatMessage::user("first"))
            .append_message(ChatMessage::assistant(MessageContent::from_parts(vec![
                ContentPart::ReasoningContent("unsigned, never replayed".into()),
                ContentPart::Text("looking".into()),
                ContentPart::ToolCall(call),
                ContentPart::ThoughtSignature(signed.clone()),
            ])))
            .append_message(ChatMessage::from(ToolResponse::new("toolu_1", "fn main() {}")))
            .append_message(ChatMessage::user("go on"))
            .with_tools([Tool::new("read").with_description("Read a file").with_schema(json!({ "type": "object", "properties": { "path": { "type": "string" } } })), Tool::new("bare")]);
        let sent = payload("glm-5.3-flash", request.clone(), &options(Some(ReasoningEffort::Low), 4096)).expect("payload");
        assert_eq!(sent["system"], "be brief\n\nand exact");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["max_tokens"], 4096);
        let messages = sent["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 3, "the tool answer and the next prompt merge into one user turn: {sent}");
        assert_eq!(messages[0], json!({ "role": "user", "content": [{ "type": "text", "text": "first" }] }));
        let assistant = messages[1]["content"].as_array().expect("blocks");
        let kinds: Vec<&str> = assistant.iter().filter_map(|b| b["type"].as_str()).collect();
        assert_eq!(kinds, ["thinking", "redacted_thinking", "text", "tool_use"], "signed thinking first, then text, then calls");
        assert_eq!(assistant[0]["signature"], "sig-1");
        assert_eq!(assistant[3], json!({ "type": "tool_use", "id": "toolu_1", "name": "read", "input": { "path": "a.rs" } }));
        assert!(!sent.to_string().contains("unsigned, never replayed"));
        assert_eq!(messages[2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "toolu_1", "content": "fn main() {}" }));
        assert_eq!(messages[2]["content"][1], json!({ "type": "text", "text": "go on" }));
        assert_eq!(sent["tools"][0]["name"], "read");
        assert_eq!(sent["tools"][0]["description"], "Read a file");
        assert_eq!(sent["tools"][1]["input_schema"], json!({ "type": "object", "properties": {} }), "the wire requires a schema");
        // Thinking turned off replays no thinking block.
        let off = payload("glm-5.3-flash", request, &options(Some(ReasoningEffort::None), 4096)).expect("payload");
        assert!(!off.to_string().contains("sig-1"), "{off}");
        // A forged or broken signature part refuses rather than send a guess.
        let broken = ChatRequest::from_user("q").append_message(ChatMessage::assistant(MessageContent::from_parts(vec![
            ContentPart::ThoughtSignature(json!([{ "type": "thinking", "thinking": "x" }]).to_string()),
        ])));
        assert!(matches!(payload("m", broken, &options(None, 4096)), Err(StreamFault::Protocol(_))));
    }

    #[tokio::test]
    async fn stream_reads_thinking_text_and_tool_calls_with_their_signature() {
        let mut frames = vec![
            json!({ "type": "message_start", "message": { "id": "msg_7", "usage": { "input_tokens": 100, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 50, "output_tokens": 1 } } }),
            start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })),
            delta(0, json!({ "type": "thinking_delta", "thinking": "which file" })),
            delta(0, json!({ "type": "signature_delta", "signature": "sig-" })),
            delta(0, json!({ "type": "signature_delta", "signature": "abc" })),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "ping" }),
            start(1, json!({ "type": "text", "text": "" })),
            delta(1, json!({ "type": "text_delta", "text": "reading " })),
            delta(1, json!({ "type": "text_delta", "text": "now" })),
            start(2, json!({ "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} })),
            delta(2, json!({ "type": "input_json_delta", "partial_json": "{\"path\":" })),
            delta(2, json!({ "type": "input_json_delta", "partial_json": "\"a.rs\"}" })),
            start(3, json!({ "type": "tool_use", "id": "toolu_2", "name": "list", "input": {} })),
        ];
        frames.extend(stop("tool_use"));
        let (end, observed) = read(&frames).await.expect("stream");
        assert_eq!(observed, [("thinking", "which file".into()), ("text", "reading ".into()), ("text", "now".into())]);
        assert_eq!(end.captured_response_id.as_deref(), Some("msg_7"));
        assert_eq!(end.captured_reasoning_content.as_deref(), Some("which file"));
        assert!(matches!(end.captured_stop_reason, Some(StopReason::ToolCall(_))));
        let content = end.captured_content.expect("content");
        assert_eq!(content.texts(), ["reading now"]);
        let calls = content.tool_calls();
        assert_eq!((calls[0].fn_name.as_str(), calls[0].fn_arguments.clone()), ("read", json!({ "path": "a.rs" })));
        assert_eq!((calls[1].fn_name.as_str(), calls[1].fn_arguments.clone()), ("list", json!({})), "a call with no input fragments takes its start input");
        let signed: Value = serde_json::from_str(content.thought_signatures()[0]).expect("signed json");
        assert_eq!(signed, json!([{ "type": "thinking", "thinking": "which file", "signature": "sig-abc" }]));
        // The usage in genai's sense: the prompt includes the cache, which Tokens takes back out.
        let usage = end.captured_usage.expect("usage");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (Some(750), Some(40)));
        assert_eq!(super::super::Tokens::of(&usage), super::super::Tokens { input: 100, output: 40, cache_read: 600, cache_creation: 50 });
    }

    #[tokio::test]
    async fn stream_keeps_a_cut_turns_text_and_drops_its_half_written_call() {
        let mut frames = vec![
            json!({ "type": "message_start", "message": { "id": "msg_8", "usage": { "input_tokens": 5 } } }),
            start(0, json!({ "type": "text", "text": "partial" })),
            start(1, json!({ "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} })),
            delta(1, json!({ "type": "input_json_delta", "partial_json": "{\"pa" })),
        ];
        frames.extend(stop("max_tokens"));
        let (end, observed) = read(&frames).await.expect("cut stream");
        assert_eq!(observed, [("text", "partial".into())], "a start block's own text is a delta too");
        assert!(matches!(end.captured_stop_reason, Some(StopReason::MaxTokens(_))));
        let content = end.captured_content.expect("content");
        assert_eq!(content.texts(), ["partial"]);
        assert!(content.tool_calls().is_empty());
    }

    #[tokio::test]
    async fn stream_refuses_what_it_cannot_trust() {
        let message = json!({ "type": "message_start", "message": { "id": "m" } });
        let cases: Vec<(Vec<Value>, &str)> = vec![
            (vec![message.clone(), json!({ "type": "message_stop" })], "no stop reason"),
            (vec![message.clone(), delta(0, json!({ "type": "text_delta", "text": "x" }))], "never started"),
            (vec![message.clone(), start(0, json!({ "type": "text" })), start(0, json!({ "type": "text" }))], "restarted"),
            (vec![message.clone(), start(0, json!({ "type": "text" })), delta(0, json!({ "type": "thinking_delta", "thinking": "x" }))], "does not match"),
            (
                [vec![message.clone(), start(0, json!({ "type": "tool_use", "id": "t", "name": "read" })), delta(0, json!({ "type": "input_json_delta", "partial_json": "[1]" }))], stop("tool_use").to_vec()].concat(),
                "not a JSON object",
            ),
            ([vec![message.clone()], stop("tool_use").to_vec()].concat(), "without a call"),
            (vec![message.clone(), start(0, json!({ "type": "text", "text": "never stopped" }))], "before a framed terminal"),
        ];
        for (frames, needle) in cases {
            match read(&frames).await {
                Err(StreamFault::Protocol(why)) => assert!(why.contains(needle), "{needle}: {why}"),
                Err(other) => panic!("{needle}: {other:?}"),
                Ok(_) => panic!("{needle}: accepted"),
            }
        }
        let error = json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } });
        assert!(matches!(read(&[message, error]).await, Err(StreamFault::Provider(body)) if body.contains("Overloaded")));
    }

    #[test]
    fn budget_thinking_is_told_by_the_claude_generation() {
        for model in ["claude-3-5-sonnet-20241022", "claude-sonnet-4-5", "claude-opus-4-1-20250805", "claude-haiku-4-5", "claude-sonnet-4-20250514"] {
            assert!(thinks_on_budget(model), "{model}");
        }
        for model in ["claude-opus-4-6", "claude-sonnet-4-6", "claude-opus-4-7", "claude-opus-5-5", "claude-haiku-5-5", "glm-5.3-flash", "kimi-k2", "deepseek-v4-pro"] {
            assert!(!thinks_on_budget(model), "{model}");
        }
    }

    #[tokio::test]
    async fn the_first_token_is_the_first_block_the_model_opens_not_the_message_start() {
        use futures_util::StreamExt;
        use std::time::{Duration, Instant};
        // The opening frames land at once; the model opens its first block 40 ms later.
        let opening: String = [json!({ "type": "message_start", "message": { "id": "m" } }), json!({ "type": "ping" })].into_iter().map(frame).collect();
        let mut answer = vec![start(0, json!({ "type": "thinking", "thinking": "" })), delta(0, json!({ "type": "thinking_delta", "thinking": "hm" }))];
        answer.extend(stop("end_turn"));
        let answer: String = answer.into_iter().map(frame).collect();
        let source = Box::pin(futures_util::stream::iter([(0, opening), (40, answer)]).then(|(wait, body)| async move {
            tokio::time::sleep(Duration::from_millis(wait)).await;
            Ok::<_, io::Error>(body.into_bytes())
        }));
        let sent = Instant::now();
        let mut began = None;
        let mut quiet = |_: Delta| std::future::ready(true);
        super::super::stream::drive(source, &mut quiet, Reading::default(), &mut began).await.expect("stream").expect("end");
        let ttft = began.expect("a first token").duration_since(sent);
        assert!(ttft >= Duration::from_millis(40), "the first token waited for the block: {ttft:?}");
        // A reply that opens no block at all names no first token.
        let bare: String = [json!({ "type": "message_start", "message": { "id": "m" } })].into_iter().chain(stop("end_turn")).map(frame).collect();
        let mut silent = None;
        super::super::stream::drive(futures_util::stream::iter([Ok::<_, io::Error>(bare.into_bytes())]), &mut quiet, Reading::default(), &mut silent)
            .await.expect("stream").expect("end");
        assert!(silent.is_none());
    }
}
