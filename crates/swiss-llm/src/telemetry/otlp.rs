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

//! OTLP's logs messages - the fields an export of ours carries and the answer's partial
//! success - written from opentelemetry-proto's field tags (collector/logs/v1, logs/v1,
//! common/v1, resource/v1) with prost's derive: no protoc, no build script. A field we
//! never send is simply absent; protobuf readers skip what they do not know and default
//! what we leave out.
//!
//! The same request also renders as OTLP/JSON ([to_json]): the proto3 JSON mapping with
//! OTLP's own deviations - lowerCamelCase keys, 64-bit integers as decimal strings, enums
//! as their numbers.

use serde_json::{json, Map, Value};

#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_logs: Vec<ResourceLogs>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogsServiceResponse {
    #[prost(message, optional, tag = "1")]
    pub partial_success: Option<ExportLogsPartialSuccess>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ExportLogsPartialSuccess {
    #[prost(int64, tag = "1")]
    pub rejected_log_records: i64,
    #[prost(string, tag = "2")]
    pub error_message: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceLogs {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_logs: Vec<ScopeLogs>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ScopeLogs {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub log_records: Vec<LogRecord>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InstrumentationScope {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct LogRecord {
    #[prost(fixed64, tag = "1")]
    pub time_unix_nano: u64,
    /// SeverityNumber; an enum on the wire is its varint.
    #[prost(int32, tag = "2")]
    pub severity_number: i32,
    #[prost(string, tag = "3")]
    pub severity_text: String,
    #[prost(message, optional, tag = "5")]
    pub body: Option<AnyValue>,
    #[prost(message, repeated, tag = "6")]
    pub attributes: Vec<KeyValue>,
    #[prost(fixed64, tag = "11")]
    pub observed_time_unix_nano: u64,
    /// OTLP 1.5's event name. Claude Code's readers key on the `event.name` attribute,
    /// which every record also carries; this field is for the readers that moved on.
    #[prost(string, tag = "12")]
    pub event_name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AnyValue {
    #[prost(oneof = "Scalar", tags = "1, 2, 3, 4")]
    pub value: Option<Scalar>,
}

/// The AnyValue kinds we send: no arrays, maps or bytes.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Scalar {
    #[prost(string, tag = "1")]
    String(String),
    #[prost(bool, tag = "2")]
    Bool(bool),
    #[prost(int64, tag = "3")]
    Int(i64),
    #[prost(double, tag = "4")]
    Double(f64),
}

/// OTLP's SeverityNumber for INFO.
pub const SEVERITY_INFO: i32 = 9;

pub fn string(value: impl Into<String>) -> AnyValue {
    AnyValue { value: Some(Scalar::String(value.into())) }
}

pub fn key_value(key: impl Into<String>, value: Scalar) -> KeyValue {
    KeyValue { key: key.into(), value: Some(AnyValue { value: Some(value) }) }
}

/// The request as OTLP/JSON.
pub fn to_json(request: &ExportLogsServiceRequest) -> Value {
    let resource_logs: Vec<Value> = request
        .resource_logs
        .iter()
        .map(|rl| {
            let mut out = Map::new();
            if let Some(resource) = &rl.resource {
                out.insert("resource".into(), json!({ "attributes": attributes_json(&resource.attributes) }));
            }
            let scopes: Vec<Value> = rl
                .scope_logs
                .iter()
                .map(|sl| {
                    let mut scope = Map::new();
                    if let Some(s) = &sl.scope {
                        scope.insert("scope".into(), json!({ "name": s.name, "version": s.version }));
                    }
                    scope.insert("logRecords".into(), Value::Array(sl.log_records.iter().map(record_json).collect()));
                    Value::Object(scope)
                })
                .collect();
            out.insert("scopeLogs".into(), Value::Array(scopes));
            Value::Object(out)
        })
        .collect();
    json!({ "resourceLogs": resource_logs })
}

fn record_json(record: &LogRecord) -> Value {
    let mut out = Map::new();
    out.insert("timeUnixNano".into(), Value::String(record.time_unix_nano.to_string()));
    out.insert("observedTimeUnixNano".into(), Value::String(record.observed_time_unix_nano.to_string()));
    out.insert("severityNumber".into(), json!(record.severity_number));
    out.insert("severityText".into(), Value::String(record.severity_text.clone()));
    if !record.event_name.is_empty() {
        out.insert("eventName".into(), Value::String(record.event_name.clone()));
    }
    if let Some(body) = &record.body {
        out.insert("body".into(), any_json(body));
    }
    out.insert("attributes".into(), attributes_json(&record.attributes));
    Value::Object(out)
}

fn attributes_json(attributes: &[KeyValue]) -> Value {
    Value::Array(
        attributes
            .iter()
            .map(|kv| json!({ "key": kv.key, "value": kv.value.as_ref().map(any_json).unwrap_or_else(|| json!({})) }))
            .collect(),
    )
}

fn any_json(value: &AnyValue) -> Value {
    match &value.value {
        Some(Scalar::String(s)) => json!({ "stringValue": s }),
        Some(Scalar::Bool(b)) => json!({ "boolValue": b }),
        // int64 is a decimal string in the proto3 JSON mapping.
        Some(Scalar::Int(n)) => json!({ "intValue": n.to_string() }),
        Some(Scalar::Double(d)) => json!({ "doubleValue": d }),
        None => json!({}),
    }
}

/// The rejected count and message in an OTLP/JSON answer, when it reports a partial success.
pub fn partial_from_json(body: &[u8]) -> Option<(i64, String)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let partial = value.get("partialSuccess")?;
    let rejected = match partial.get("rejectedLogRecords") {
        Some(Value::String(s)) => s.parse().ok()?,
        Some(n) => n.as_i64()?,
        None => 0,
    };
    let message = partial.get("errorMessage").and_then(Value::as_str).unwrap_or_default().to_string();
    Some((rejected, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn sample() -> ExportLogsServiceRequest {
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource { attributes: vec![key_value("service.name", Scalar::String("swiss".into()))] }),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope { name: "swiss.llm".into(), version: "1.0.0".into() }),
                    log_records: vec![LogRecord {
                        time_unix_nano: 1_700_000_000_000_000_001,
                        observed_time_unix_nano: 1_700_000_000_000_000_001,
                        severity_number: SEVERITY_INFO,
                        severity_text: "INFO".into(),
                        body: Some(string("claude_code.api_request")),
                        attributes: vec![
                            key_value("event.name", Scalar::String("api_request".into())),
                            key_value("input_tokens", Scalar::Int(1200)),
                            key_value("cost_usd", Scalar::Double(0.5)),
                        ],
                        event_name: "api_request".into(),
                    }],
                }],
            }],
        }
    }

    #[test]
    fn the_request_round_trips_through_its_own_wire() {
        let request = sample();
        let bytes = request.encode_to_vec();
        assert_eq!(ExportLogsServiceRequest::decode(bytes.as_slice()).expect("decode"), request);
        // Field 1 of the request is a length-delimited ResourceLogs: tag byte 0x0a.
        assert_eq!(bytes[0], 0x0a);
        // An empty export is an empty message: what the probe sends.
        assert!(ExportLogsServiceRequest::default().encode_to_vec().is_empty());
    }

    #[test]
    fn the_json_mapping_follows_otlp() {
        let value = to_json(&sample());
        let record = &value["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(record["timeUnixNano"], "1700000000000000001");
        assert_eq!(record["severityNumber"], 9);
        assert_eq!(record["body"]["stringValue"], "claude_code.api_request");
        assert_eq!(record["attributes"][1]["value"]["intValue"], "1200");
        assert_eq!(record["attributes"][2]["value"]["doubleValue"], 0.5);
        assert_eq!(value["resourceLogs"][0]["resource"]["attributes"][0]["key"], "service.name");
        assert_eq!(value["resourceLogs"][0]["scopeLogs"][0]["scope"]["name"], "swiss.llm");
    }

    #[test]
    fn a_partial_success_is_read_from_either_wire() {
        let answer = ExportLogsServiceResponse {
            partial_success: Some(ExportLogsPartialSuccess { rejected_log_records: 2, error_message: "too old".into() }),
        };
        let decoded = ExportLogsServiceResponse::decode(answer.encode_to_vec().as_slice()).expect("decode");
        assert_eq!(decoded.partial_success.expect("partial").rejected_log_records, 2);
        assert_eq!(partial_from_json(br#"{"partialSuccess":{"rejectedLogRecords":"3","errorMessage":"x"}}"#), Some((3, "x".into())));
        assert_eq!(partial_from_json(br#"{}"#), None);
    }
}
