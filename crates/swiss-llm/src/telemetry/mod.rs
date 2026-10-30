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

//! Agent telemetry (SPEC §llm.telemetry): every agent run reported as OTLP log records in
//! Claude Code's own event vocabulary, so a collector built for Claude Code - cc-otel, a
//! Grafana board over its tables - reads swiss's agents with no new mapping:
//!
//! | event          | attributes beyond the common ones                                    |
//! |----------------|----------------------------------------------------------------------|
//! | `user_prompt`  | `prompt_length`, `prompt` (only with `logPrompts`)                   |
//! | `api_request`  | `model`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `duration_ms`, `ttft_ms` (when the reply had output), `request_id` |
//! | `api_error`    | `model`, `error`, `status_code`, `duration_ms`, `attempt` (+ cc-otel's `error.*`) |
//! | `tool_decision`| `tool_name`, `decision`, `source`                                    |
//! | `tool_result`  | `tool_name`, `success`, `duration_ms`, `tool_result_size_bytes`, `decision_type`, `decision_source` |
//!
//! Every record carries `event.name`, `event.timestamp`, `event.sequence`, `session.id`
//! (one per run), `prompt.id`, `terminal.type` = "swiss", and swiss's own `swiss.agent`
//! and `swiss.consumer`; the body is `claude_code.<event>` as Claude Code writes it. The
//! resource says who sent it: `service.name` (default "swiss" - a reader prices a
//! non-Claude-Code service itself rather than trusting a cost), `service.version`,
//! `os.type`, `host.arch`. Token counts are Claude's semantics: `input_tokens` excludes
//! the cache (see [crate::provider::Tokens]). No cost is reported: swiss holds no price
//! list, and a reader that has one computes it.
//!
//! A `claude` agent is not reported from here at all: its child gets the OTEL env
//! ([TelemetryConfig::claude_env]) and reports itself, exactly as Claude Code does under
//! any host - the same endpoint, the same protocol, `swiss.agent` added to its resource.
//!
//! The wire is the block's `protocol`: `grpc`, `http/protobuf` or `http/json`.

mod otlp;
mod reporter;
mod transport;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::agent::names_credential;
use crate::config::{clip, flag, has_refs, http_url, object, only, printable, text};
use crate::provider::{CallError, Chat, Reply};

pub use reporter::{Reporter, TelemetryStatus};
pub use transport::{Accepted, ExportError};

use otlp::{key_value, LogRecord, Scalar};

/// The keys the block may carry.
const TELEMETRY_KEYS: [&str; 6] = ["enabled", "protocol", "endpoint", "headers", "serviceName", "logPrompts"];
const HEADERS_MAX: usize = 16;
const HEADER_NAME_MAX: usize = 64;
const HEADER_VALUE_MAX: usize = 2000;
const SERVICE_NAME_MAX: usize = 64;
/// How much of a prompt a `user_prompt` record carries when prompts are logged.
const PROMPT_MAX: usize = 16_000;
/// The resource's `service.name` when the block names none.
pub const SERVICE_NAME_DEFAULT: &str = "swiss";
/// The instrumentation scope every record is filed under.
const SCOPE_NAME: &str = "swiss.llm";

/// The OTLP wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Grpc,
    HttpProtobuf,
    HttpJson,
}

impl Protocol {
    /// The name OTEL_EXPORTER_OTLP_PROTOCOL uses.
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Grpc => "grpc",
            Protocol::HttpProtobuf => "http/protobuf",
            Protocol::HttpJson => "http/json",
        }
    }
}

/// The checked `telemetry` block.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryConfig {
    /// Off keeps the settings and sends nothing.
    pub enabled: bool,
    pub protocol: Protocol,
    /// The collector's base URL, never a signal path: the logs path is appended per wire,
    /// and a spawned Claude Code appends its own per signal.
    pub endpoint: String,
    /// Extra request headers; a credential-named one carries a `${...}` reference.
    pub headers: Vec<(String, String)>,
    pub service_name: String,
    /// Whether `user_prompt` records carry the prompt text (Claude Code's
    /// OTEL_LOG_USER_PROMPTS); off, they carry only its length.
    pub log_prompts: bool,
}

/// The `telemetry` block; None when the row has none.
pub(crate) fn parse(block: Option<&Value>) -> Result<Option<TelemetryConfig>, String> {
    let Some(block) = block.filter(|b| !b.is_null()) else { return Ok(None) };
    let place = "telemetry";
    let obj = object(block, place)?;
    only(obj, place, &TELEMETRY_KEYS)?;
    let enabled = flag(obj, place, "enabled")?.unwrap_or(true);
    let protocol = match text(obj, place, "protocol")?.as_deref() {
        None | Some("grpc") => Protocol::Grpc,
        Some("http/protobuf") => Protocol::HttpProtobuf,
        Some("http/json") => Protocol::HttpJson,
        Some(other) => {
            return Err(format!("telemetry.protocol must be \"grpc\", \"http/protobuf\" or \"http/json\", got {other:?}"))
        }
    };
    let endpoint = match text(obj, place, "endpoint")? {
        Some(url) => http_url(&url, "telemetry.endpoint")?,
        None => {
            return Err(
                "telemetry.endpoint is required: the collector's base URL (cc-otel: http://127.0.0.1:4317 for grpc, http://127.0.0.1:4318 for http)"
                    .into(),
            )
        }
    };
    let endpoint = match protocol {
        Protocol::Grpc => {
            // gRPC routes on the service path alone; a base path would never reach it.
            let path = reqwest::Url::parse(&endpoint).map(|u| u.path().to_string()).unwrap_or_default();
            if !path.is_empty() && path != "/" {
                return Err("telemetry.endpoint for grpc is scheme://host:port, with no path".into());
            }
            endpoint
        }
        // An endpoint typed with its signal path keeps only the base.
        _ => endpoint.strip_suffix("/v1/logs").map(str::to_string).unwrap_or(endpoint),
    };
    let headers = parse_headers(obj.get("headers"))?;
    let service_name = text(obj, place, "serviceName")?.unwrap_or_else(|| SERVICE_NAME_DEFAULT.to_string());
    if !printable(&service_name, SERVICE_NAME_MAX) {
        return Err(format!("telemetry.serviceName: at most {SERVICE_NAME_MAX} printable characters"));
    }
    let log_prompts = flag(obj, place, "logPrompts")?.unwrap_or(false);
    Ok(Some(TelemetryConfig { enabled, protocol, endpoint, headers, service_name, log_prompts }))
}

fn parse_headers(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let map = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("telemetry.headers must be an object of names to values".into()),
    };
    if map.len() > HEADERS_MAX {
        return Err(format!("telemetry.headers holds {} names; at most {HEADERS_MAX}", map.len()));
    }
    let mut out = Vec::new();
    for (name, value) in map {
        let legal = (1..=HEADER_NAME_MAX).contains(&name.len())
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !legal {
            return Err(format!("telemetry.headers names are 1..={HEADER_NAME_MAX} chars of [A-Za-z0-9-_], got {name:?}"));
        }
        let name = name.to_ascii_lowercase();
        if matches!(name.as_str(), "content-type" | "content-length" | "te" | "host") || name.starts_with("grpc-") {
            return Err(format!("telemetry.headers {name} is set by the exporter itself"));
        }
        let value = value.as_str().ok_or_else(|| format!("telemetry.headers {name} must be a string"))?;
        if !printable(value, HEADER_VALUE_MAX) {
            return Err(format!("telemetry.headers {name}: at most {HEADER_VALUE_MAX} printable characters"));
        }
        swiss_core::secure::refs::check_secret_refs(value).map_err(|err| format!("telemetry.headers {name}: {err}"))?;
        // "Bearer ${secret://otel}" is the usual shape, so the credential sits in a reference
        // anywhere in the value; a credential-named header without one is refused, unquoted.
        if names_credential(&name) && !has_refs(value) {
            return Err(format!(
                "telemetry.headers {name} is credential-named: its value must hold a ${{secret://name}} or ${{ENV}} reference, never the credential itself"
            ));
        }
        out.push((name, value.to_string()));
    }
    Ok(out)
}

impl TelemetryConfig {
    /// The env that makes a spawned Claude Code report itself here: telemetry on, logs and
    /// metrics over OTLP, this block's protocol and endpoint, its headers resolved now (the
    /// child's env only - never stored, never logged), and `swiss.agent` on the child's
    /// resource so a reader can tell swiss's children from a person's sessions.
    pub fn claude_env(&self, agent: &str) -> Result<Vec<(String, String)>, String> {
        let mut out: Vec<(String, String)> = [
            ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
            ("OTEL_LOGS_EXPORTER", "otlp"),
            ("OTEL_METRICS_EXPORTER", "otlp"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", self.protocol.as_str()),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", self.endpoint.as_str()),
            ("OTEL_LOG_USER_PROMPTS", if self.log_prompts { "1" } else { "0" }),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        out.push(("OTEL_RESOURCE_ATTRIBUTES".into(), format!("swiss.agent={agent}")));
        if !self.headers.is_empty() {
            let mut pairs = Vec::new();
            for (name, value) in &self.headers {
                let resolved = swiss_core::secure::refs::resolve(value).map_err(|err| format!("telemetry.headers {name}: {err}"))?;
                // The OTEL env list is comma-separated pairs whose values are URL-decoded.
                let encoded = resolved.replace('%', "%25").replace(',', "%2C");
                pairs.push(format!("{name}={encoded}"));
            }
            out.push(("OTEL_EXPORTER_OTLP_HEADERS".into(), pairs.join(",")));
        }
        Ok(out)
    }

    /// The resource every export of ours carries.
    fn resource(&self) -> otlp::Resource {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        otlp::Resource {
            attributes: vec![
                key_value("service.name", Scalar::String(self.service_name.clone())),
                key_value("service.version", Scalar::String(env!("CARGO_PKG_VERSION").into())),
                key_value("os.type", Scalar::String(os.into())),
                key_value("host.arch", Scalar::String(arch.into())),
            ],
        }
    }

    /// One request around a batch of records.
    fn request(&self, records: Vec<LogRecord>) -> otlp::ExportLogsServiceRequest {
        otlp::ExportLogsServiceRequest {
            resource_logs: vec![otlp::ResourceLogs {
                resource: Some(self.resource()),
                scope_logs: vec![otlp::ScopeLogs {
                    scope: Some(otlp::InstrumentationScope { name: SCOPE_NAME.into(), version: env!("CARGO_PKG_VERSION").into() }),
                    log_records: records,
                }],
            }],
        }
    }
}

/// The panel's Test: an empty export, which every OTLP collector accepts and none stores -
/// it proves the endpoint, the protocol and the headers without writing a row anywhere.
/// The answer is the round trip in milliseconds.
pub async fn probe(cfg: &TelemetryConfig) -> Result<u64, String> {
    let wire = transport::Wire::build(cfg)?;
    let started = Instant::now();
    wire.export(&otlp::ExportLogsServiceRequest::default()).await.map_err(|err| err.message)?;
    Ok(started.elapsed().as_millis() as u64)
}

/// One run's reporting handle. A disabled session ([Session::disabled]) is free: every
/// method returns at once and nothing is built.
pub struct Session {
    sink: Option<Arc<Reporter>>,
    id: String,
    agent: String,
    consumer: String,
    log_prompts: bool,
    sequence: AtomicU64,
}

impl Session {
    /// A session that reports nothing: telemetry off, an agent with `report: false`, a test.
    pub fn disabled() -> Session {
        Session {
            sink: None,
            id: String::new(),
            agent: String::new(),
            consumer: String::new(),
            log_prompts: false,
            sequence: AtomicU64::new(0),
        }
    }

    pub(crate) fn new(sink: Arc<Reporter>, agent: &str, consumer: &str, log_prompts: bool) -> Session {
        Session {
            sink: Some(sink),
            id: uuid(),
            agent: agent.to_string(),
            consumer: consumer.to_string(),
            log_prompts,
            sequence: AtomicU64::new(0),
        }
    }

    /// Whether anything this session records goes anywhere.
    pub fn reporting(&self) -> bool {
        self.sink.is_some()
    }

    /// The run's `session.id` (empty when disabled).
    pub fn id(&self) -> &str {
        &self.id
    }

    fn emit(&self, event: &'static str, prompt_id: &str, attrs: Vec<(&'static str, Scalar)>) {
        let Some(sink) = &self.sink else { return };
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut attributes = vec![
            key_value("event.name", Scalar::String(event.into())),
            key_value("event.timestamp", Scalar::String(swiss_core::log::iso_now())),
            key_value("event.sequence", Scalar::Int(sequence as i64)),
            key_value("session.id", Scalar::String(self.id.clone())),
            key_value("terminal.type", Scalar::String("swiss".into())),
            key_value("swiss.agent", Scalar::String(self.agent.clone())),
            key_value("swiss.consumer", Scalar::String(self.consumer.clone())),
        ];
        if !prompt_id.is_empty() {
            attributes.push(key_value("prompt.id", Scalar::String(prompt_id.into())));
        }
        attributes.extend(attrs.into_iter().map(|(k, v)| key_value(k, v)));
        sink.push(LogRecord {
            time_unix_nano: nanos,
            severity_number: otlp::SEVERITY_INFO,
            severity_text: "INFO".into(),
            body: Some(otlp::string(format!("claude_code.{event}"))),
            attributes,
            observed_time_unix_nano: nanos,
            event_name: event.into(),
        });
    }

    /// A task handed to the agent: `user_prompt`. Returns the `prompt.id` every record of
    /// this task carries (empty when disabled).
    pub fn prompt(&self, text: &str) -> String {
        if self.sink.is_none() {
            return String::new();
        }
        let prompt_id = uuid();
        let mut attrs = vec![("prompt_length", Scalar::Int(text.chars().count() as i64))];
        if self.log_prompts {
            attrs.push(("prompt", Scalar::String(clip(text, PROMPT_MAX))));
        }
        self.emit("user_prompt", &prompt_id, attrs);
        prompt_id
    }

    /// One answered model request: `api_request`.
    pub fn api_request(&self, prompt_id: &str, chat: &Chat, reply: &Reply) {
        if self.sink.is_none() {
            return;
        }
        let t = reply.tokens;
        let mut attrs = vec![
            ("model", Scalar::String(chat.model.clone())),
            ("input_tokens", Scalar::Int(t.input as i64)),
            ("output_tokens", Scalar::Int(t.output as i64)),
            ("cache_read_tokens", Scalar::Int(t.cache_read as i64)),
            ("cache_creation_tokens", Scalar::Int(t.cache_creation as i64)),
            ("duration_ms", Scalar::Int(reply.duration_ms as i64)),
            ("request_id", Scalar::String(reply.request_id.clone())),
            ("swiss.provider", Scalar::String(chat.provider.clone())),
        ];
        // Claude Code's own name for the time to first token, which cc-otel files as TTFT;
        // a reply with no output has none, and a missing one reads as unknown, never 0.
        if let Some(ttft) = reply.ttft_ms {
            attrs.push(("ttft_ms", Scalar::Int(ttft as i64)));
        }
        self.emit("api_request", prompt_id, attrs);
    }

    /// One failed model request: `api_error`, in Claude Code's attributes and the
    /// `error.*` ones cc-otel files.
    pub fn api_error(&self, prompt_id: &str, chat: &Chat, err: &CallError) {
        if self.sink.is_none() {
            return;
        }
        let status = err.status.map(|s| s.to_string()).unwrap_or_else(|| "undefined".into());
        self.emit(
            "api_error",
            prompt_id,
            vec![
                ("model", Scalar::String(chat.model.clone())),
                ("error", Scalar::String(err.message.clone())),
                ("status_code", Scalar::String(status)),
                ("duration_ms", Scalar::Int(err.duration_ms as i64)),
                ("attempt", Scalar::Int(1)),
                ("error.type", Scalar::String(err.kind.into())),
                ("error.message", Scalar::String(err.message.clone())),
                ("error.code", Scalar::Int(err.status.map(i64::from).unwrap_or(0))),
                ("error.retryable", Scalar::Int(i64::from(err.retryable))),
                ("swiss.provider", Scalar::String(chat.provider.clone())),
            ],
        );
    }

    /// The model called a tool the consumer offers: `tool_decision`. The consumer's config
    /// is what allowed it, so the decision is always an accept from config.
    pub fn tool_decision(&self, prompt_id: &str, tool: &str) {
        if self.sink.is_none() {
            return;
        }
        self.emit(
            "tool_decision",
            prompt_id,
            vec![
                ("tool_name", Scalar::String(tool.into())),
                ("decision", Scalar::String("accept".into())),
                ("source", Scalar::String("config".into())),
            ],
        );
    }

    /// The tool answered: `tool_result`.
    pub fn tool_result(&self, prompt_id: &str, tool: &str, ok: bool, duration_ms: u64, size: usize) {
        if self.sink.is_none() {
            return;
        }
        self.emit(
            "tool_result",
            prompt_id,
            vec![
                ("tool_name", Scalar::String(tool.into())),
                ("success", Scalar::String(ok.to_string())),
                ("duration_ms", Scalar::Int(duration_ms as i64)),
                ("tool_result_size_bytes", Scalar::Int(size as i64)),
                ("decision_type", Scalar::String("accept".into())),
                ("decision_source", Scalar::String("config".into())),
            ],
        );
    }
}

/// A random UUID-shaped id (v4 layout), as Claude Code's session and prompt ids are.
fn uuid() -> String {
    let hex = swiss_core::util::random_hex(16);
    let mut bytes: Vec<char> = hex.chars().collect();
    bytes[12] = '4';
    bytes[16] = ['8', '9', 'a', 'b'][bytes[16].to_digit(16).unwrap_or(0) as usize % 4];
    let s: String = bytes.into_iter().collect();
    format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(value: Value) -> Result<Option<TelemetryConfig>, String> {
        parse(Some(&value))
    }

    #[test]
    fn the_block_parses_with_its_defaults() {
        let cfg = block(json!({ "endpoint": "http://127.0.0.1:4317" })).expect("parse").expect("on");
        assert_eq!((cfg.enabled, cfg.protocol, cfg.service_name.as_str(), cfg.log_prompts), (true, Protocol::Grpc, "swiss", false));
        let http = block(json!({ "protocol": "http/protobuf", "endpoint": "http://127.0.0.1:4318/v1/logs", "enabled": false }))
            .expect("parse")
            .expect("block");
        assert_eq!((http.endpoint.as_str(), http.enabled), ("http://127.0.0.1:4318", false));
        assert_eq!(parse(None).expect("none"), None);
        assert_eq!(parse(Some(&Value::Null)).expect("null"), None);
    }

    #[test]
    fn the_block_refuses_what_would_not_export() {
        let refused = [
            (json!({}), "endpoint is required"),
            (json!({ "endpoint": "grpc://h:4317" }), "http:// or https://"),
            (json!({ "endpoint": "http://h:4317/otlp" }), "no path"),
            (json!({ "endpoint": "http://h:4317", "protocol": "udp" }), "protocol must be"),
            (json!({ "endpoint": "http://h:4317", "headers": { "authorization": "Bearer tok-live-1" } }), "credential-named"),
            (json!({ "endpoint": "http://h:4317", "headers": { "content-type": "x" } }), "exporter itself"),
            (json!({ "endpoint": "http://h:4317", "headers": { "bad name": "x" } }), "headers names"),
            (json!({ "endpoint": "http://h:4317", "sampling": 1 }), "no \"sampling\" field"),
        ];
        for (value, needle) in refused {
            let err = block(value.clone()).expect_err(&value.to_string());
            assert!(err.contains(needle), "{value}: {err}");
            assert!(!err.contains("tok-live-1"), "a credential is never echoed: {err}");
        }
        assert!(block(json!({ "endpoint": "http://h:4317", "headers": { "Authorization": "Bearer ${secret://otel}" } })).is_ok());
    }

    #[test]
    fn claude_env_points_the_child_at_the_same_collector() {
        std::env::set_var("SWISS_LLM_TEST_OTEL_HDR", "a,b%c");
        let cfg = block(json!({
            "protocol": "http/json", "endpoint": "https://otel.example:4318/",
            "headers": { "x-auth-token": "${SWISS_LLM_TEST_OTEL_HDR}" }
        }))
        .expect("parse")
        .expect("on");
        let env: std::collections::BTreeMap<String, String> = cfg.claude_env("distiller").expect("env").into_iter().collect();
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");
        assert_eq!(env["OTEL_EXPORTER_OTLP_ENDPOINT"], "https://otel.example:4318");
        assert_eq!(env["OTEL_EXPORTER_OTLP_HEADERS"], "x-auth-token=a%2Cb%25c");
        assert_eq!(env["OTEL_RESOURCE_ATTRIBUTES"], "swiss.agent=distiller");
        assert_eq!(env["OTEL_LOG_USER_PROMPTS"], "0");
    }

    #[test]
    fn a_session_files_claude_code_shaped_records() {
        let reporter = Reporter::new();
        let cfg = block(json!({ "endpoint": "http://127.0.0.1:4317", "logPrompts": true })).expect("parse");
        reporter.configure(cfg.as_ref());
        let session = reporter.session("distiller", "memory.sync");
        assert!(session.reporting() && session.id().len() == 36);
        let prompt_id = session.prompt("file the facts");
        session.tool_decision(&prompt_id, "read");
        session.tool_result(&prompt_id, "read", true, 12, 400);
        let records = reporter.drain_for_test();
        assert_eq!(records.len(), 3);
        let attr = |record: &LogRecord, key: &str| {
            record.attributes.iter().find(|kv| kv.key == key).and_then(|kv| kv.value.clone()).and_then(|v| v.value)
        };
        assert_eq!(attr(&records[0], "event.name"), Some(Scalar::String("user_prompt".into())));
        assert_eq!(attr(&records[0], "prompt"), Some(Scalar::String("file the facts".into())));
        assert_eq!(attr(&records[2], "success"), Some(Scalar::String("true".into())));
        assert_eq!(attr(&records[2], "event.sequence"), Some(Scalar::Int(2)));
        assert_eq!(attr(&records[2], "prompt.id"), Some(Scalar::String(prompt_id)));
        assert_eq!(records[1].body, Some(otlp::string("claude_code.tool_decision")));
        // A disabled session builds nothing.
        let quiet = Session::disabled();
        assert_eq!(quiet.prompt("x"), "");
        assert!(reporter.drain_for_test().is_empty());
    }

    #[tokio::test]
    async fn an_api_request_carries_the_time_to_first_token() {
        use crate::provider::tests::{entry, fake_stream_server};
        let frames = [
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let body: String = frames.iter().map(|data| format!("data: {data}

")).collect();
        let (url, server) = fake_stream_server(200, body).await;
        let chat = Chat::new(&entry(crate::provider::Style::Anthropic, url), None, 1024).expect("chat");
        // Nothing here runs the exporter, and the port is closed besides: no record leaves.
        let reporter = Reporter::new();
        reporter.configure(block(json!({ "endpoint": "http://127.0.0.1:9" })).expect("parse").as_ref());
        let session = reporter.session("lead", "llm.test");
        assert_eq!(crate::provider::ask(&chat, None, "say it", &session).await.expect("answer"), "done");
        server.await.expect("server");
        let records = reporter.drain_for_test();
        let request = records.iter().find(|record| record.event_name == "api_request").expect("api_request");
        let int = |key: &str| match request.attributes.iter().find(|kv| kv.key == key).and_then(|kv| kv.value.clone()).and_then(|v| v.value) {
            Some(Scalar::Int(n)) => n,
            other => panic!("{key}: {other:?}"),
        };
        let (ttft, duration) = (int("ttft_ms"), int("duration_ms"));
        assert!((0..=duration).contains(&ttft), "ttft {ttft} within duration {duration}");
    }
}
