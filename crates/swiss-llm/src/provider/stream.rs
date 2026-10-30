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

//! Stream semantics over the maintained SSE grammar. The raw feed checks
//! frame/response budgets before copying into the parser, and yields no more than
//! one frame at a time: the parser's private event queue cannot grow with a burst.
//! [drive] is the one SSE loop; each wire ([Accumulator] for OpenAI, the anthropic
//! module's assembly for Anthropic) says what one event means through [Assembly].

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use futures_util::{Stream, StreamExt};
use genai::chat::{ContentPart, MessageContent, StopReason, StreamEnd, ToolCall, Usage};
use serde::Deserialize;
use serde_json::Value;

use super::Delta;

pub(super) const FRAME_MAX: usize = 1024 * 1024;
pub(super) const TOTAL_MAX: usize = 16 * 1024 * 1024;
pub(super) const ERROR_BODY_MAX: usize = 64 * 1024;
pub(super) const MODEL_BODY_MAX: usize = 1024 * 1024;

const FRAME_LIMIT: &str = "the provider's stream frame exceeded its byte budget";
const TOTAL_LIMIT: &str = "the provider's stream exceeded its byte budget";

type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub(super) enum StreamFault {
    Protocol(&'static str),
    Source(BoxError),
    // Kept whole so the caller decodes and masks every credential before clipping.
    Provider(String),
}

// The capability check and both wires' serializers share genai's public effort
// suffix normalization. An explicit effort keeps the configured model unchanged.
// A trailing "-max" is a vendor's tier far more often than an effort (qwen3.8-max,
// qwen-max, gpt-5.1-codex-max): read as one, qwen3.8-max went out as "qwen3.8". It
// stays in the id; Max is the agent's effort field to ask.
pub(super) fn wire_model<'a>(model: &'a str, options: &genai::chat::ChatOptions) -> (Option<genai::chat::ReasoningEffort>, &'a str) {
    use genai::chat::ReasoningEffort;
    match &options.reasoning_effort {
        Some(effort) => (Some(effort.clone()), model),
        None => match ReasoningEffort::from_model_name(model) {
            (Some(ReasoningEffort::Max), _) => (None, model),
            read => read,
        },
    }
}

// OpenAI's reasoning models refuse max_tokens and take max_completion_tokens: the
// o-series and every GPT from 5 on (gpt-5-mini, gpt-6-astra, gpt-6.1-terra). A
// gpt-oss or gpt-4o served elsewhere keeps max_tokens.
fn takes_completion_tokens(model: &str) -> bool {
    ["o1", "o3", "o4"].iter().any(|series| model.starts_with(series))
        || model.strip_prefix("gpt-").and_then(|rest| rest.split(['-', '.']).next()).and_then(|major| major.parse::<u32>().ok()).is_some_and(|major| major >= 5)
}

// This path consumes the canonical parts Swiss actually constructs. It does not
// translate native signatures/binary content into an invented OpenAI contract.
pub(super) fn openai_payload(model: &str, request: genai::chat::ChatRequest, options: &genai::chat::ChatOptions) -> Result<Value, StreamFault> {
    use genai::chat::{ChatRole, ReasoningEffort};
    let (effort, model) = wire_model(model, options);
    let mut messages = Vec::new();
    if let Some(system) = request.system { messages.push(serde_json::json!({ "role": "system", "content": system })); }
    for message in request.messages {
        let user = matches!(&message.role, ChatRole::User);
        match message.role {
            ChatRole::System | ChatRole::User => {
                if !message.content.is_text_only() {
                    return Err(StreamFault::Protocol("the OpenAI request contains unsupported message content"));
                }
                let text = message.content.joined_texts();
                if user || text.is_some() {
                    let role = if user { "user" } else { "system" };
                    messages.push(serde_json::json!({ "role": role, "content": text.unwrap_or_default() }));
                }
            }
            ChatRole::Assistant => {
                let mut texts = Vec::new();
                let mut thinking = Vec::new();
                let mut calls = Vec::new();
                for part in message.content {
                    match part {
                        ContentPart::Text(text) => texts.push(text),
                        ContentPart::ReasoningContent(text) => thinking.push(text),
                        // Anthropic's signed thinking (a log replayed after the agent
                        // changed wires): OpenAI has no spelling for it, so it stays behind.
                        ContentPart::ThoughtSignature(_) => {}
                        ContentPart::ToolCall(call) => {
                            if call.call_id.trim().is_empty() || call.fn_name.trim().is_empty() || !call.fn_arguments.is_object() {
                                return Err(StreamFault::Protocol("the OpenAI request contains a malformed tool call"));
                            }
                            calls.push(serde_json::json!({ "type": "function", "id": call.call_id,
                                "function": { "name": call.fn_name, "arguments": call.fn_arguments.to_string() } }));
                        }
                        _ => return Err(StreamFault::Protocol("the OpenAI request contains unsupported assistant content")),
                    }
                }
                let mut message = serde_json::json!({ "role": "assistant", "content": texts.join("\n\n") });
                if !calls.is_empty() { message["tool_calls"] = Value::Array(calls); }
                if !thinking.is_empty() { message["reasoning_content"] = Value::String(thinking.join("\n")); }
                messages.push(message);
            }
            ChatRole::Tool => {
                for part in message.content {
                    match part {
                        ContentPart::ToolResponse(response) if !response.call_id.trim().is_empty() => {
                            messages.push(serde_json::json!({ "role": "tool", "content": response.content, "tool_call_id": response.call_id }));
                        }
                        _ => return Err(StreamFault::Protocol("the OpenAI request contains a malformed tool response")),
                    }
                }
            }
        }
    }
    let mut payload = serde_json::json!({ "model": model, "messages": messages, "stream": true });
    if let Some(keyword) = effort.as_ref().and_then(ReasoningEffort::as_keyword) { payload["reasoning_effort"] = serde_json::json!(keyword); }
    if options.capture_usage.unwrap_or(false) { payload["stream_options"] = serde_json::json!({ "include_usage": true }); }
    if let Some(max) = options.max_tokens {
        let key = if takes_completion_tokens(model) { "max_completion_tokens" } else { "max_tokens" };
        payload[key] = serde_json::json!(max);
    }
    if let Some(temperature) = options.temperature { payload["temperature"] = serde_json::json!(temperature); }
    if let Some(top_p) = options.top_p { payload["top_p"] = serde_json::json!(top_p); }
    if let Some(seed) = options.seed { payload["seed"] = serde_json::json!(seed); }
    if !options.stop_sequences.is_empty() { payload["stop"] = serde_json::json!(options.stop_sequences); }
    if let Some(tools) = request.tools {
        let tools = tools.into_iter().map(|tool| {
            let strict = tool.strict.unwrap_or(false);
            let mut schema = tool.schema;
            if strict { if let Some(schema) = &mut schema { strict_objects(schema); } }
            serde_json::json!({ "type": "function", "function": { "name": tool.name,
                "description": tool.description, "parameters": schema, "strict": strict } })
        }).collect();
        payload["tools"] = Value::Array(tools);
    }
    Ok(payload)
}

fn strict_objects(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("object") { object.insert("additionalProperties".into(), Value::Bool(false)); }
            for value in object.values_mut() { strict_objects(value); }
        }
        Value::Array(array) => for value in array { strict_objects(value); },
        _ => {}
    }
}

#[derive(Debug)]
pub(super) enum BodyFault<E> {
    TooLarge,
    Source(E),
}

pub(super) async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, BodyFault<reqwest::Error>> {
    if response.content_length().is_some_and(|size| size > limit as u64) {
        return Err(BodyFault::TooLarge);
    }
    collect_bytes(response.bytes_stream(), limit).await
}

async fn collect_bytes<S, D, E>(source: S, limit: usize) -> Result<Vec<u8>, BodyFault<E>>
where
    S: Stream<Item = Result<D, E>>,
    D: AsRef<[u8]>,
{
    let mut source = Box::pin(source);
    let mut out = Vec::new();
    while let Some(chunk) = source.next().await {
        let chunk = chunk.map_err(BodyFault::Source)?;
        let bytes = chunk.as_ref();
        if bytes.len() > limit - out.len() {
            return Err(BodyFault::TooLarge);
        }
        if !bytes.is_empty() {
            // A fixed upper bound avoids both geometric over-allocation and
            // quadratic reserve_exact growth on adversarial one-byte chunks.
            if out.capacity() == 0 { out.reserve_exact(limit); }
            out.extend_from_slice(bytes);
        }
    }
    Ok(out)
}

#[derive(Clone, Copy)]
struct Limits {
    frame: usize,
    total: usize,
}

impl Default for Limits {
    fn default() -> Self { Limits { frame: FRAME_MAX, total: TOTAL_MAX } }
}

#[derive(Clone, Copy, Default)]
struct FrameCounter {
    bytes: usize,
    nonempty_line: bool,
    after_cr: bool,
    after_boundary: bool,
}

impl FrameCounter {
    // Only delimiter accounting lives here. Field grammar, UTF-8, BOM and
    // multiline data belong to sse-stream. CR dispatches without lookahead;
    // its optional following LF is delimiter overhead, still total-counted.
    fn byte(&mut self, byte: u8, limit: usize) -> Result<bool, &'static str> {
        if self.after_cr {
            self.after_cr = false;
            if byte == b'\n' {
                if !self.after_boundary {
                    if self.bytes == limit { return Err(FRAME_LIMIT); }
                    self.bytes += 1;
                }
                self.after_boundary = false;
                return Ok(false);
            }
        }
        self.after_boundary = false;
        if self.bytes == limit { return Err(FRAME_LIMIT); }
        self.bytes += 1;
        if matches!(byte, b'\r' | b'\n') {
            let boundary = !self.nonempty_line;
            self.nonempty_line = false;
            self.after_cr = byte == b'\r';
            self.after_boundary = boundary;
            if boundary { self.bytes = 0; }
            Ok(boundary)
        } else {
            self.nonempty_line = true;
            Ok(false)
        }
    }
}

#[derive(Debug)]
enum FeedFault<E> {
    Source(E),
    Limit(&'static str),
}

impl<E: Error> std::fmt::Display for FeedFault<E> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedFault::Source(err) => write!(out, "{err}"),
            FeedFault::Limit(message) => out.write_str(message),
        }
    }
}

impl<E: Error + 'static> Error for FeedFault<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self { FeedFault::Source(err) => Some(err), FeedFault::Limit(_) => None }
    }
}

struct BoundedFeed<S, D> {
    source: Pin<Box<S>>,
    pending: Option<D>,
    offset: usize,
    counter: FrameCounter,
    total: usize,
    limits: Limits,
    finished: bool,
    #[cfg(test)]
    copied: usize,
}

impl<S, D> BoundedFeed<S, D> {
    fn new(source: S, limits: Limits) -> Self {
        BoundedFeed {
            source: Box::pin(source), pending: None, offset: 0,
            counter: FrameCounter::default(), total: 0, limits, finished: false,
            #[cfg(test)]
            copied: 0,
        }
    }
}

impl<S, D, E> Stream for BoundedFeed<S, D>
where
    S: Stream<Item = Result<D, E>>,
    D: AsRef<[u8]> + Unpin,
{
    type Item = Result<Cursor<Vec<u8>>, FeedFault<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished { return Poll::Ready(None); }
        loop {
            if let Some(pending) = &this.pending {
                let bytes = pending.as_ref();
                if this.offset == bytes.len() {
                    this.pending = None;
                    continue;
                }
                let start = this.offset;
                let mut end = start;
                let mut counter = this.counter;
                // Keep a CR's optional LF in its own feed item. It cannot add an
                // event or inflate the next frame's copied buffer by one byte.
                let skip_lf = counter.after_cr && counter.after_boundary && bytes[start] == b'\n';
                while end < bytes.len() {
                    let boundary = match counter.byte(bytes[end], this.limits.frame) {
                        Ok(boundary) => boundary,
                        Err(message) => {
                            this.finished = true;
                            this.pending = None;
                            return Poll::Ready(Some(Err(FeedFault::Limit(message))));
                        }
                    };
                    end += 1;
                    if boundary || skip_lf { break; }
                }
                // The entire known prefix passed preflight before this copy.
                #[cfg(test)]
                { this.copied += end - start; }
                let copied = bytes[start..end].to_vec();
                this.offset = end;
                this.counter = counter;
                return Poll::Ready(Some(Ok(Cursor::new(copied))));
            }
            match this.source.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Err(err))) => {
                    this.finished = true;
                    return Poll::Ready(Some(Err(FeedFault::Source(err))));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    let size = chunk.as_ref().len();
                    if size > this.limits.total - this.total {
                        this.finished = true;
                        return Poll::Ready(Some(Err(FeedFault::Limit(TOTAL_LIMIT))));
                    }
                    this.total += size;
                    this.pending = Some(chunk);
                    this.offset = 0;
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct WireChunk {
    id: Option<String>,
    choices: Option<Vec<WireChoice>>,
    usage: Option<Value>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct WireChoice {
    index: u64,
    #[serde(default)]
    delta: WireDelta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    role: Option<String>,
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Option<Vec<WireCall>>,
}

#[derive(Deserialize)]
struct WireCall {
    index: u64,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<WireFunction>,
}

#[derive(Deserialize)]
struct WireFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct CallFragments {
    id: String,
    name: String,
    arguments: String,
}

/// What one SSE event did to a wire's assembly: the deltas it produced, in order,
/// or the terminal that ends the stream.
pub(super) enum Step {
    Deltas(Vec<Delta>),
    End(StreamEnd),
}

/// One wire's reading of its SSE events. `event` is the frame's `event:` name when it
/// carries one; `data` is never blank (the driver skips those).
pub(super) trait Assembly {
    fn event(&mut self, event: Option<&str>, data: &str) -> Result<Step, StreamFault>;
    /// Whether the model has begun its answer: a piece of thinking, text or a tool call
    /// has arrived, not just the stream's opening frames.
    fn began(&self) -> bool;
}

#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    calls: BTreeMap<u64, CallFragments>,
    finish: Option<String>,
    usage: Option<Usage>,
    response_id: Option<String>,
}

impl Accumulator {
    fn push(&mut self, data: &str) -> Result<Vec<Delta>, StreamFault> {
        let chunk: WireChunk = serde_json::from_str(data)
            .map_err(|_| StreamFault::Protocol("the provider's stream chunk was malformed"))?;
        if chunk.error.is_some() { return Err(StreamFault::Provider(data.to_string())); }
        if let Some(id) = chunk.id.filter(|id| !id.is_empty()) {
            if self.response_id.as_ref().is_some_and(|previous| previous != &id) {
                return Err(StreamFault::Protocol("the provider changed its stream response id"));
            }
            self.response_id = Some(id);
        }
        if let Some(value) = chunk.usage {
            if let Ok(mut usage) = serde_json::from_value::<Usage>(value) {
                usage.compact_details();
                if usage.prompt_tokens.is_some() || usage.completion_tokens.is_some() || usage.total_tokens.is_some()
                    || usage.prompt_tokens_details.is_some() || usage.completion_tokens_details.is_some() {
                    self.usage = Some(usage);
                }
            }
        }
        let choices = chunk.choices.ok_or(StreamFault::Protocol("the provider's stream chunk has no choices"))?;
        if choices.is_empty() { return Ok(Vec::new()); }
        if choices.len() != 1 || choices[0].index != 0 {
            return Err(StreamFault::Protocol("the provider's stream has an unexpected choice index"));
        }
        let choice = choices.into_iter().next().ok_or(StreamFault::Protocol("the provider's stream has no choice"))?;
        let delta = choice.delta;
        if delta.role.as_ref().is_some_and(|role| role != "assistant") {
            return Err(StreamFault::Protocol("the provider's stream has an unexpected message role"));
        }
        let reasoning = delta.reasoning_content.or(delta.reasoning);
        let has_payload = delta.content.as_ref().is_some_and(|part| !part.is_empty())
            || reasoning.as_ref().is_some_and(|part| !part.is_empty())
            || delta.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
        if self.finish.is_some() && (has_payload || choice.finish_reason.is_some()) {
            return Err(StreamFault::Protocol("the provider sent content after its finish reason"));
        }
        if let Some(finish) = &choice.finish_reason {
            if !matches!(finish.as_str(), "stop" | "length" | "content_filter" | "tool_calls") {
                return Err(StreamFault::Protocol("the provider's stream has an invalid or interrupted finish reason"));
            }
        }
        // No mutually exclusive branches: text, reasoning and every tool fragment
        // from the same event survive, including a payload on the finish event.
        let mut deltas = Vec::new();
        if let Some(reasoning) = reasoning.filter(|part| !part.is_empty()) {
            self.reasoning.push_str(&reasoning);
            deltas.push(Delta::Thinking(reasoning));
        }
        if let Some(text) = delta.content.filter(|part| !part.is_empty()) {
            self.text.push_str(&text);
            deltas.push(Delta::Text(text));
        }
        for fragment in delta.tool_calls.unwrap_or_default() {
            if fragment.kind.as_ref().is_some_and(|kind| kind != "function") {
                return Err(StreamFault::Protocol("the provider's stream tool type is not function"));
            }
            // Sparse indices never resize a Vec or fabricate calls/ids in gaps.
            let call = self.calls.entry(fragment.index).or_default();
            if let Some(id) = fragment.id.filter(|id| !id.is_empty()) {
                if !call.id.is_empty() && call.id != id {
                    return Err(StreamFault::Protocol("the provider changed a tool call id"));
                }
                call.id = id;
            }
            if let Some(function) = fragment.function {
                if let Some(name) = function.name { call.name.push_str(&name); }
                if let Some(arguments) = function.arguments { call.arguments.push_str(&arguments); }
            }
        }
        if let Some(finish) = choice.finish_reason { self.finish = Some(finish); }
        Ok(deltas)
    }

    fn finish(&mut self) -> Result<StreamEnd, StreamFault> {
        let this = std::mem::take(self);
        let finish = this.finish.ok_or(StreamFault::Protocol("the provider's stream has no valid finish reason"))?;
        let mut parts = Vec::new();
        if !this.text.is_empty() { parts.push(ContentPart::Text(this.text)); }
        if finish == "tool_calls" {
            if this.calls.is_empty() {
                return Err(StreamFault::Protocol("the provider finished tool calls without a call"));
            }
            let mut ids = BTreeSet::new();
            for call in this.calls.into_values() {
                if call.id.trim().is_empty() || call.name.trim().is_empty() || !ids.insert(call.id.clone()) {
                    return Err(StreamFault::Protocol("the provider's tool call id or name is missing or duplicated"));
                }
                let arguments: Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| StreamFault::Protocol("the provider's tool call arguments are not valid JSON"))?;
                if !arguments.is_object() {
                    return Err(StreamFault::Protocol("the provider's tool call arguments are not a JSON object"));
                }
                parts.push(ContentPart::ToolCall(ToolCall {
                    call_id: call.id, fn_name: call.name, fn_arguments: arguments, thought_signatures: None,
                }));
            }
        } else if finish == "stop" && !this.calls.is_empty() {
            return Err(StreamFault::Protocol("the provider's tool calls have no tool-call finish reason"));
        }
        // Length/filter terminals preserve useful text but cannot execute calls.
        Ok(StreamEnd {
            captured_usage: this.usage,
            captured_stop_reason: Some(StopReason::from(finish)),
            captured_content: Some(MessageContent::from_parts(parts)),
            captured_reasoning_content: (!this.reasoning.is_empty()).then_some(this.reasoning),
            captured_response_id: this.response_id,
        })
    }
}

impl Assembly for Accumulator {
    fn event(&mut self, _event: Option<&str>, data: &str) -> Result<Step, StreamFault> {
        if data.trim() == "[DONE]" { return self.finish().map(Step::End); }
        self.push(data).map(Step::Deltas)
    }

    fn began(&self) -> bool {
        !self.text.is_empty() || !self.reasoning.is_empty() || !self.calls.is_empty()
    }
}

fn parser_fault<E: Error + Send + Sync + 'static>(fault: sse_stream::Error) -> StreamFault {
    match fault {
        sse_stream::Error::Body(body) => match body.downcast::<FeedFault<E>>() {
            Ok(fault) => match *fault {
                FeedFault::Source(err) => StreamFault::Source(Box::new(err)),
                FeedFault::Limit(message) => StreamFault::Protocol(message),
            },
            Err(_) => StreamFault::Protocol("the provider's stream body could not be read"),
        },
        _ => StreamFault::Protocol("the provider's stream frame was malformed"),
    }
}

/// The OpenAI wire's reading of its stream, for [drive].
pub(super) fn openai_assembly() -> impl Assembly {
    Accumulator::default()
}

#[cfg(test)]
async fn openai_stream<S, D, E, F, Fut>(source: S, sink: &mut F) -> Result<Option<StreamEnd>, StreamFault>
where
    S: Stream<Item = Result<D, E>>,
    D: AsRef<[u8]> + Unpin,
    E: Error + Send + Sync + 'static,
    F: FnMut(Delta) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    drive(source, sink, Accumulator::default(), &mut None).await
}

/// The one SSE loop under every wire: bounded feed, maintained grammar, each event
/// handed to the wire's assembly and its deltas awaited out before the source is
/// polled again. A stream that ends before its terminal is a protocol fault. `began`
/// takes the moment the model's first output arrived (the time to first token), set
/// before that output is handed on, so a slow sink never delays it.
pub(super) async fn drive<S, D, E, F, Fut, A>(
    source: S,
    sink: &mut F,
    mut assembly: A,
    began: &mut Option<Instant>,
) -> Result<Option<StreamEnd>, StreamFault>
where
    S: Stream<Item = Result<D, E>>,
    D: AsRef<[u8]> + Unpin,
    E: Error + Send + Sync + 'static,
    F: FnMut(Delta) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
    A: Assembly,
{
    let feed = BoundedFeed::new(source, Limits::default());
    let mut events = Box::pin(sse_stream::SseStream::from_bytes_stream(feed));
    while let Some(event) = events.next().await {
        let event = event.map_err(parser_fault::<E>)?;
        let Some(data) = event.data else { continue };
        if data.trim().is_empty() { continue; }
        match assembly.event(event.event.as_deref(), &data)? {
            Step::End(end) => return Ok(Some(end)),
            Step::Deltas(deltas) => {
                if began.is_none() && assembly.began() {
                    *began = Some(Instant::now());
                }
                for delta in deltas {
                    // Await delivery before polling again. false drops even a same-chunk
                    // cached terminal/tool batch; no biased-select or flag race remains.
                    if !sink(delta).await { return Ok(None); }
                }
            }
        }
    }
    Err(StreamFault::Protocol("the provider's stream ended before a framed terminal event"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use serde_json::json;

    fn chunk(delta: Value, finish: Value) -> Value {
        json!({ "id": "request-1", "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
    }

    fn frames(chunks: &[Value], ending: &str) -> Vec<u8> {
        let mut body: String = chunks.iter().map(|chunk| format!("data: {chunk}{ending}{ending}")).collect();
        body.push_str(&format!("data: [DONE]{ending}{ending}"));
        body.into_bytes()
    }

    async fn read(parts: Vec<Vec<u8>>) -> Result<(StreamEnd, Vec<(&'static str, String)>), StreamFault> {
        let source = futures_util::stream::iter(parts.into_iter().map(Ok::<_, io::Error>));
        let mut observed = Vec::new();
        let mut sink = |delta| {
            observed.push(match delta { Delta::Thinking(text) => ("thinking", text), Delta::Text(text) => ("text", text) });
            std::future::ready(true)
        };
        let end = openai_stream(source, &mut sink).await?.expect("this sink always continues");
        Ok((end, observed))
    }

    fn call(index: u64, id: Option<&str>, name: &str, arguments: &str) -> Value {
        let mut fragment = json!({ "index": index, "type": "function", "function": { "name": name, "arguments": arguments } });
        if let Some(id) = id { fragment["id"] = json!(id); }
        fragment
    }

    #[tokio::test]
    async fn stream_bytes_every_split_preserves_bom_utf8_crlf_multiline() {
        let payload = chunk(json!({ "content": "你好 🚀", "reasoning_content": "想一想" }), Value::Null);
        let data: String = serde_json::to_string_pretty(&payload).expect("JSON").lines()
            .map(|line| format!("data: {line}\r\n")).collect();
        let mut body = format!("\u{feff}: comment\r\nevent: message\r\nid: event-1\r\n{data}\r\n").into_bytes();
        body.extend(frames(&[
            chunk(json!({}), json!("stop")),
            json!({ "choices": [], "usage": { "prompt_tokens": 1000, "completion_tokens": 50,
                "prompt_tokens_details": { "cached_tokens": 600, "cache_creation_tokens": 100 } } })
        ], "\r\n"));
        for split in 0..=body.len() {
            let (end, observed) = read(vec![body[..split].to_vec(), Vec::new(), body[split..].to_vec()]).await.expect("split frame");
            assert_eq!(end.captured_first_text(), Some("你好 🚀"), "split {split}");
            assert_eq!(end.captured_reasoning_content.as_deref(), Some("想一想"), "split {split}");
            assert_eq!(observed, [("thinking", "想一想".into()), ("text", "你好 🚀".into())]);
            let usage = end.captured_usage.expect("tail usage");
            assert_eq!(usage.prompt_tokens, Some(1000));
            assert_eq!(usage.prompt_tokens_details.expect("cache").cached_tokens, Some(600));
        }
        let (end, _) = read(body.iter().map(|byte| vec![*byte]).collect()).await.expect("one byte per item");
        assert_eq!(end.captured_first_text(), Some("你好 🚀"));
    }

    #[tokio::test]
    async fn stream_bytes_line_endings_and_empty_chunks_agree() {
        for ending in ["\n", "\r\n", "\r"] {
            let body = frames(&[chunk(json!({ "content": "one" }), json!("stop"))], ending);
            let parts = body.iter().flat_map(|byte| [Vec::new(), vec![*byte]]).collect();
            let (end, observed) = read(parts).await.expect("valid line endings");
            assert_eq!(end.captured_first_text(), Some("one"));
            assert_eq!(observed, [("text", "one".into())]);
        }
    }

    #[tokio::test]
    async fn stream_bytes_reject_truncated_done_invalid_utf8_and_json() {
        let prefix = format!("data: {}\n\n", chunk(json!({ "content": "partial" }), json!("stop")));
        let mut cases: Vec<Vec<u8>> = ["data: [DONE]", "data: [DONE]\n", "data: [DONE]\r\n"]
            .into_iter().map(|tail| format!("{prefix}{tail}").into_bytes()).collect();
        cases.push(b"data: {\"choices\":[\xff]}\n\n".to_vec());
        cases.push(b"data: \xf0\x9f".to_vec());
        cases.push(b"data: not JSON\n\n".to_vec());
        let mut observed = Vec::new();
        for body in cases { observed.push(matches!(read(vec![body]).await, Err(StreamFault::Protocol(_)))); }
        assert_eq!(observed, vec![true; 6]);
    }

    #[tokio::test]
    async fn stream_bytes_finish_reason_is_required_valid_and_final() {
        let mut cases = Vec::new();
        for finish in [Value::Null, json!(17), json!(true), json!([]), json!(""), json!("unknown"), json!("aborted"), json!("insufficient_system_resource")] {
            cases.push(frames(&[chunk(json!({ "content": "partial" }), finish)], "\n"));
        }
        let mut missing = chunk(json!({}), Value::Null);
        missing["choices"][0].as_object_mut().expect("choice").remove("finish_reason");
        cases.push(frames(&[missing], "\n"));
        cases.push(frames(&[chunk(json!({}), json!("stop")), chunk(json!({ "content": "late" }), Value::Null)], "\n"));
        let mut observed = Vec::new();
        for body in cases { observed.push(matches!(read(vec![body]).await, Err(StreamFault::Protocol(_)))); }
        assert_eq!(observed, vec![true; 10]);
    }

    #[tokio::test]
    async fn stream_bytes_parallel_sparse_fragments_keep_all_sibling_content() {
        let body = frames(&[
            chunk(json!({ "content": "now", "reasoning_content": "why", "tool_calls": [
                call(9, Some("call-b"), "sy", "{\"p\":\"b"), call(2, Some("call-a"), "re", "{\"p\":\"a")
            ] }), Value::Null),
            chunk(json!({ "tool_calls": [call(2, None, "ad", "\"}"), call(9, Some("call-b"), "nc", "\"}")] }), Value::Null),
            chunk(json!({ "content": "tail", "reasoning_content": "end" }), json!("tool_calls"))
        ], "\n");
        let (end, observed) = read(vec![body]).await.expect("complete calls");
        assert_eq!(end.captured_first_text(), Some("nowtail"));
        assert_eq!(end.captured_reasoning_content.as_deref(), Some("whyend"));
        let calls = end.captured_tool_calls().expect("calls");
        assert_eq!(calls.len(), 2, "no fabricated gap calls");
        assert_eq!((calls[0].call_id.as_str(), calls[0].fn_name.as_str()), ("call-a", "read"));
        assert_eq!((calls[1].call_id.as_str(), calls[1].fn_name.as_str()), ("call-b", "sync"));
        assert_eq!(calls[0].fn_arguments, json!({ "p": "a" }));
        assert_eq!(calls[1].fn_arguments, json!({ "p": "b" }));
        assert_eq!(observed, [("thinking", "why".into()), ("text", "now".into()), ("thinking", "end".into()), ("text", "tail".into())]);
    }

    #[tokio::test]
    async fn stream_bytes_bad_call_metadata_or_arguments_never_execute() {
        let mut calls = vec![call(0, None, "read", "{}"), call(0, Some(" "), "read", "{}"), call(0, Some("a"), "", "{}")];
        for arguments in ["", "{", "[]", "17", "\"text\""] { calls.push(call(0, Some("a"), "read", arguments)); }
        let mut missing_index = call(0, Some("a"), "read", "{}");
        missing_index.as_object_mut().expect("call").remove("index");
        calls.push(missing_index);
        let mut wrong_index = call(0, Some("a"), "read", "{}");
        wrong_index["index"] = json!(-1);
        calls.push(wrong_index);
        let mut wrong_type = call(0, Some("a"), "read", "{}");
        wrong_type["type"] = json!("custom");
        calls.push(wrong_type);
        let mut observed = Vec::new();
        for call in calls {
            let body = frames(&[chunk(json!({ "tool_calls": [call] }), json!("tool_calls"))], "\n");
            observed.push(matches!(read(vec![body]).await, Err(StreamFault::Protocol(_))));
        }
        for chunks in [
            vec![chunk(json!({ "tool_calls": [call(0, Some("a"), "read", "{}"), call(1, Some("a"), "sync", "{}")] }), json!("tool_calls"))],
            vec![chunk(json!({ "tool_calls": [call(0, Some("a"), "read", "{")] }), Value::Null),
                chunk(json!({ "tool_calls": [call(0, Some("b"), "", "}")] }), json!("tool_calls"))]
        ] { observed.push(matches!(read(vec![frames(&chunks, "\n")]).await, Err(StreamFault::Protocol(_)))); }
        assert!(observed.into_iter().all(|rejected| rejected));
        let (end, _) = read(vec![frames(&[chunk(json!({ "tool_calls": [call(u64::MAX, Some("huge-index"), "read", "{}")] }), json!("tool_calls"))], "\n")])
            .await.expect("a sparse index is a map key, not an allocation size");
        assert_eq!(end.captured_tool_calls().expect("call").len(), 1);
    }

    #[tokio::test]
    async fn stream_bytes_cut_or_filtered_reply_keeps_text_not_calls() {
        for finish in ["length", "content_filter"] {
            let (end, _) = read(vec![frames(&[chunk(json!({ "content": "partial", "tool_calls": [call(0, None, "read", "{")] }), json!(finish))], "\n")])
                .await.expect("useful partial text");
            assert_eq!(end.captured_first_text(), Some("partial"));
            assert!(end.captured_tool_calls().expect("content").is_empty());
        }
    }

    #[tokio::test]
    async fn stream_bytes_frame_and_total_limits_precede_copy() {
        for size in [FRAME_MAX, FRAME_MAX + 1] {
            let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'x'; size])]);
            let mut feed = BoundedFeed::new(source, Limits::default());
            let result = feed.next().await.expect("item");
            if size == FRAME_MAX {
                assert_eq!(result.expect("L accepted").get_ref().len(), FRAME_MAX);
                assert_eq!(feed.copied, FRAME_MAX);
            } else {
                assert!(matches!(result, Err(FeedFault::Limit(FRAME_LIMIT))));
                assert_eq!(feed.copied, 0, "oversized known frame prefix never enters the parser");
            }
        }
        for size in [TOTAL_MAX, TOTAL_MAX + 1] {
            let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'\n'; size])]);
            let mut feed = BoundedFeed::new(source, Limits::default());
            let result = feed.next().await.expect("item");
            if size == TOTAL_MAX {
                assert_eq!(result.expect("L accepted").get_ref().len(), 1);
                assert_eq!(feed.total, TOTAL_MAX);
            } else {
                assert!(matches!(result, Err(FeedFault::Limit(TOTAL_LIMIT))));
                assert_eq!(feed.copied, 0, "oversized source chunk is rejected before any copy/feed");
            }
        }
        let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'x'; 32]), Ok(vec![b'x'; 33])]);
        let mut feed = BoundedFeed::new(source, Limits { frame: 64, total: 128 });
        assert_eq!(feed.next().await.expect("first").expect("prefix").get_ref().len(), 32);
        assert!(matches!(feed.next().await.expect("second"), Err(FeedFault::Limit(FRAME_LIMIT))));
        assert_eq!(feed.copied, 32, "an over-budget continuation is never copied");
    }

    #[tokio::test]
    async fn stream_bytes_body_collection_checks_l_and_l_plus_one() {
        let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'a'; 32]), Ok(vec![b'b'; 32])]);
        let body = collect_bytes(source, 64).await.expect("L body");
        assert_eq!(body.len(), 64);
        let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'a'; 32]), Ok(vec![b'b'; 33])]);
        assert!(matches!(collect_bytes(source, 64).await, Err(BodyFault::TooLarge)));
        let source = futures_util::stream::iter([Ok::<_, io::Error>(vec![b'a'; 65])]);
        assert!(matches!(collect_bytes(source, 64).await, Err(BodyFault::TooLarge)));
    }

    struct ProbeStream {
        parts: VecDeque<Vec<u8>>,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
        pending_at_end: bool,
    }

    impl Stream for ProbeStream {
        type Item = Result<Vec<u8>, io::Error>;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            this.polls.fetch_add(1, Ordering::SeqCst);
            match this.parts.pop_front() {
                Some(part) => Poll::Ready(Some(Ok(part))),
                None if this.pending_at_end => Poll::Pending,
                None => Poll::Ready(None),
            }
        }
    }

    impl Drop for ProbeStream {
        fn drop(&mut self) { self.dropped.store(true, Ordering::SeqCst); }
    }

    fn probe(parts: Vec<Vec<u8>>, pending_at_end: bool) -> (ProbeStream, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let polls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        (ProbeStream { parts: parts.into(), polls: polls.clone(), dropped: dropped.clone(), pending_at_end }, polls, dropped)
    }

    #[tokio::test]
    async fn stream_bytes_bare_cr_done_does_not_wait_for_lookahead() {
        let (source, polls, dropped) = probe(vec![frames(&[chunk(json!({ "content": "one" }), json!("stop"))], "\r")], true);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), openai_stream(source, &mut |_| std::future::ready(true)))
            .await.expect("bare CR must dispatch without waiting for another chunk").expect("complete CR stream").expect("end");
        assert_eq!(result.captured_first_text(), Some("one"));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stream_bytes_sink_false_drops_cached_terminal_without_next_poll() {
        let body = frames(&[chunk(json!({ "content": "one" }), Value::Null), chunk(json!({ "content": "two" }), json!("stop"))], "\n");
        let (source, polls, dropped) = probe(vec![body], false);
        let mut callbacks = 0;
        let result = openai_stream(source, &mut |_| { callbacks += 1; std::future::ready(false) }).await.expect("sink stop");
        assert!(result.is_none());
        assert_eq!(callbacks, 1);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stream_bytes_backpressure_awaits_sink_and_drains_in_order() {
        let parts = vec![
            format!("data: {}\n\n", chunk(json!({ "content": "one" }), Value::Null)).into_bytes(),
            format!("data: {}\n\n", chunk(json!({ "content": "two" }), Value::Null)).into_bytes(),
            frames(&[chunk(json!({}), json!("stop"))], "\n")
        ];
        let (source, polls, dropped) = probe(parts, false);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.send(Delta::Text("queued".into())).await.expect("prefill");
        let seen = Arc::new(AtomicUsize::new(0));
        let observed = seen.clone();
        let mut sink = move |delta| {
            observed.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            async move { tx.send(delta).await.is_ok() }
        };
        let mut future = Box::pin(openai_stream(source, &mut sink));
        futures_util::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending(), "a full sink must suspend the driver");
            Poll::Ready(())
        }).await;
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert_eq!(polls.load(Ordering::SeqCst), 1, "no next byte source poll while delivery waits");
        assert!(matches!(rx.recv().await, Some(Delta::Text(text)) if text == "queued"));
        let (result, output) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(future, async {
                let mut output = Vec::new();
                for _ in 0..2 {
                    match rx.recv().await.expect("delta") { Delta::Text(text) => output.push(text), Delta::Thinking(_) => panic!("text fixture") }
                }
                output
            })
        }).await.expect("deterministic drain must terminate");
        assert_eq!(output, ["one", "two"]);
        assert_eq!(result.expect("stream").expect("end").captured_first_text(), Some("onetwo"));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stream_bytes_dropped_waiting_future_keeps_observed_prefix_only() {
        let body = frames(&[chunk(json!({ "content": "one" }), Value::Null), chunk(json!({ "content": "two" }), json!("stop"))], "\n");
        let (source, polls, dropped) = probe(vec![body], false);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        tx.send(Delta::Text("queued".into())).await.expect("prefill");
        let seen = Arc::new(Mutex::new(String::new()));
        let observed = seen.clone();
        let mut sink = move |delta| {
            if let Delta::Text(text) = &delta { observed.lock().expect("prefix").push_str(text); }
            let tx = tx.clone();
            async move { tx.send(delta).await.is_ok() }
        };
        let mut future = Box::pin(openai_stream(source, &mut sink));
        futures_util::future::poll_fn(|cx| { assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        drop(future);
        assert_eq!(&*seen.lock().expect("prefix"), "one");
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stream_bytes_reasoning_alias_and_source_error_keep_their_types() {
        let (end, observed) = read(vec![frames(&[chunk(json!({ "content": "answer", "reasoning": "ollama thought" }), json!("stop"))], "\n")])
            .await.expect("compatible reasoning alias");
        assert_eq!(end.captured_reasoning_content.as_deref(), Some("ollama thought"));
        assert_eq!(observed, [("thinking", "ollama thought".into()), ("text", "answer".into())]);
        let first = format!("data: {}\n\n", chunk(json!({ "content": "partial" }), Value::Null)).into_bytes();
        let source = futures_util::stream::iter([
            Ok(first), Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic transport timeout"))
        ]);
        let mut callbacks = 0;
        let fault = openai_stream(source, &mut |_| { callbacks += 1; std::future::ready(true) }).await.expect_err("transport error");
        assert_eq!(callbacks, 1);
        match fault {
            StreamFault::Source(source) => assert_eq!(source.downcast_ref::<io::Error>().expect("original error type").kind(), io::ErrorKind::TimedOut),
            other => panic!("not a typed transport error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_bytes_strict_field_errors_and_provider_errors_are_explicit() {
        for prefix in ["unknown: x\n\n", "id: a\nid: b\n\n", "event: a\nevent: b\n\n"] {
            assert!(matches!(read(vec![prefix.as_bytes().to_vec()]).await, Err(StreamFault::Protocol(_))));
        }
        let body = b"data: {\"error\":{\"message\":\"bad key k-echo\"}}\n\n".to_vec();
        assert!(matches!(read(vec![body]).await, Err(StreamFault::Provider(message)) if message.contains("k-echo")));
        let (end, observed) = read(vec![frames(&[chunk(json!({ "content": "k-success-verbatim" }), json!("stop"))], "\n")]).await.expect("successful content");
        assert_eq!(end.captured_first_text(), Some("k-success-verbatim"));
        assert_eq!(observed, [("text", "k-success-verbatim".into())]);
    }

    #[tokio::test]
    async fn a_tool_call_with_no_words_still_marks_the_first_token() {
        let mut quiet = |_: Delta| std::future::ready(true);
        // The role frame alone is no output; a call is, though it hands the sink nothing.
        let body = frames(&[
            chunk(json!({ "role": "assistant" }), Value::Null),
            chunk(json!({ "tool_calls": [call(0, Some("call-a"), "read", "{}")] }), json!("tool_calls")),
        ], "
");
        let mut began = None;
        drive(futures_util::stream::iter([Ok::<_, io::Error>(body)]), &mut quiet, Accumulator::default(), &mut began)
            .await.expect("stream").expect("end");
        assert!(began.is_some(), "a tool call is the model's first output");
        // A reply that never said anything names no first token.
        let body = frames(&[chunk(json!({ "role": "assistant" }), json!("stop"))], "
");
        let mut silent = None;
        drive(futures_util::stream::iter([Ok::<_, io::Error>(body)]), &mut quiet, Accumulator::default(), &mut silent)
            .await.expect("stream").expect("end");
        assert!(silent.is_none());
    }
}
