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

//! One export on the wire, in the protocol the block names:
//!
//! - `grpc` - `POST {endpoint}/opentelemetry.proto.collector.logs.v1.LogsService/Export`
//!   over HTTP/2 (prior knowledge on plain http, ALPN on https), the message in gRPC's
//!   5-byte length-prefixed frame, the verdict in `grpc-status` - a header on a
//!   trailers-only answer, a trailer otherwise.
//! - `http/protobuf` / `http/json` - `POST {endpoint}/v1/logs`, any 2xx accepted.
//!
//! Retryable failures follow the OTLP spec (HTTP 429/502/503/504; gRPC CANCELLED,
//! DEADLINE_EXCEEDED, RESOURCE_EXHAUSTED, ABORTED, UNAVAILABLE; any network failure): the
//! reporter keeps that batch and tries again. Anything else would fail again unchanged, so
//! the batch is dropped and the error is what the status says. No error quotes a resolved
//! header value.

use std::time::Duration;

use http::header::{HeaderName, HeaderValue, CONTENT_TYPE, TE};
use http_body_util::BodyExt;
use prost::Message;

use super::otlp::{self, ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse};
use super::{Protocol, TelemetryConfig};
use crate::config::{causes, clip, percent_decode, scrubbed_detail_many, scrubbed_many};

const GRPC_PATH: &str = "/opentelemetry.proto.collector.logs.v1.LogsService/Export";
const HTTP_PATH: &str = "/v1/logs";
/// One export's whole budget; the reporter's next tick is the retry.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ERROR_MAX: usize = 300;

/// A failed export: what to show, and whether the same batch may succeed later.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportError {
    pub message: String,
    pub retryable: bool,
}

/// An accepted export; `rejected` > 0 when the collector took it only in part.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Accepted {
    pub rejected: i64,
    pub message: String,
}

/// One telemetry block's client: the URL, the protocol, the headers resolved.
pub struct Wire {
    client: reqwest::Client,
    url: String,
    protocol: Protocol,
    headers: Vec<(HeaderName, HeaderValue)>,
    /// What the headers' references resolved to: masked out of every error.
    secrets: Vec<String>,
}

impl Wire {
    /// Build the client and resolve the header references now. The error names the
    /// header, never its value.
    pub fn build(cfg: &TelemetryConfig) -> Result<Wire, String> {
        // A collector is local or named outright: the ambient proxy env is not consulted.
        let mut builder = reqwest::Client::builder().no_proxy().timeout(EXPORT_TIMEOUT).connect_timeout(CONNECT_TIMEOUT);
        if cfg.protocol == Protocol::Grpc {
            builder = builder.http2_prior_knowledge();
        }
        let client = builder.build().map_err(|err| format!("the telemetry HTTP client could not be built: {err}"))?;
        let url = match cfg.protocol {
            Protocol::Grpc => format!("{}{GRPC_PATH}", cfg.endpoint),
            Protocol::HttpProtobuf | Protocol::HttpJson => format!("{}{HTTP_PATH}", cfg.endpoint),
        };
        let mut headers = Vec::new();
        let mut secrets = Vec::new();
        for (name, value) in &cfg.headers {
            let (resolved, collected) = swiss_core::secure::refs::resolve_collect(value)
                .map_err(|err| format!("telemetry.headers {name}: {err}"))?;
            let header_name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| format!("telemetry.headers {name:?} is not a header name"))?;
            let header_value = HeaderValue::from_str(&resolved)
                .map_err(|_| format!("telemetry.headers {name}: the resolved value is not a legal header value"))?;
            headers.push((header_name, header_value));
            secrets.extend(collected.into_iter().filter(|s| !s.is_empty()));
            if resolved != *value {
                secrets.push(resolved);
            }
        }
        Ok(Wire { client, url, protocol: cfg.protocol, headers, secrets })
    }

    /// Send one request.
    pub async fn export(&self, request: &ExportLogsServiceRequest) -> Result<Accepted, ExportError> {
        let outcome = match self.protocol {
            Protocol::Grpc => self.grpc(request.encode_to_vec()).await,
            Protocol::HttpProtobuf => self.http("application/x-protobuf", request.encode_to_vec()).await,
            Protocol::HttpJson => {
                let body = serde_json::to_vec(&otlp::to_json(request)).unwrap_or_default();
                self.http("application/json", body).await
            }
        };
        outcome.map_err(|mut err| {
            err.message = scrubbed_many(&err.message, &self.secrets);
            err
        })
    }

    fn post(&self, content_type: &'static str, body: Vec<u8>) -> reqwest::RequestBuilder {
        let mut request = self.client.post(&self.url).header(CONTENT_TYPE, content_type).body(body);
        for (name, value) in &self.headers {
            request = request.header(name.clone(), value.clone());
        }
        request
    }

    fn unreachable(&self, err: reqwest::Error) -> ExportError {
        let hint = match self.protocol {
            // A gRPC call to an OTLP/HTTP port dies in the HTTP/2 handshake.
            Protocol::Grpc => " (an OTLP/HTTP port such as 4318 wants an http protocol)",
            _ => "",
        };
        let what = if err.is_timeout() { "timed out" } else { "failed" };
        ExportError {
            message: format!("the export to {} {what}: {}{hint}", self.url, clip(&scrubbed_many(&causes(&err.without_url()), &self.secrets), ERROR_MAX)),
            retryable: true,
        }
    }

    async fn grpc(&self, payload: Vec<u8>) -> Result<Accepted, ExportError> {
        let mut framed = Vec::with_capacity(payload.len() + 5);
        framed.push(0);
        framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        framed.extend_from_slice(&payload);
        let response = self
            .post("application/grpc", framed)
            .header(TE, "trailers")
            .header("grpc-timeout", format!("{}S", EXPORT_TIMEOUT.as_secs()))
            .send()
            .await
            .map_err(|err| self.unreachable(err))?;
        let status = response.status();
        if !status.is_success() {
            // A gRPC server answers 200 even to a failed call: anything else is a proxy or a
            // listener that does not speak gRPC.
            return Err(ExportError {
                message: format!("{} answered HTTP {} - not a gRPC endpoint", self.url, status.as_u16()),
                retryable: matches!(status.as_u16(), 429 | 502 | 503 | 504),
            });
        }
        let head = grpc_status(response.headers());
        let response: http::Response<reqwest::Body> = response.into();
        let collected = response.into_body().collect().await.map_err(|err| ExportError {
            message: format!("the collector's answer broke off: {}", clip(&scrubbed_many(&err.to_string(), &self.secrets), ERROR_MAX)),
            retryable: true,
        })?;
        let trailed = collected.trailers().and_then(grpc_status);
        let body = collected.to_bytes();
        let (code, message) = head.or(trailed).ok_or_else(|| ExportError {
            message: format!("{} answered without a grpc-status - not a gRPC endpoint", self.url),
            retryable: false,
        })?;
        if code != 0 {
            return Err(ExportError {
                message: grpc_refusal(code, &scrubbed_many(&message, &self.secrets)),
                retryable: matches!(code, 1 | 4 | 8 | 10 | 14),
            });
        }
        let partial = body
            .get(5..)
            .and_then(|frame| ExportLogsServiceResponse::decode(frame).ok())
            .and_then(|answer| answer.partial_success);
        Ok(accepted(partial, &self.secrets))
    }

    async fn http(&self, content_type: &'static str, body: Vec<u8>) -> Result<Accepted, ExportError> {
        let response = self.post(content_type, body).send().await.map_err(|err| self.unreachable(err))?;
        let status = response.status();
        let bytes = response.bytes().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ExportError {
                message: format!("the collector answered {}: {}", status.as_u16(), scrubbed_detail_many(&bytes, &self.secrets)),
                retryable: matches!(status.as_u16(), 429 | 502 | 503 | 504),
            });
        }
        let partial = match self.protocol {
            Protocol::HttpJson => otlp::partial_from_json(&bytes)
                .map(|(rejected_log_records, error_message)| ExportLogsPartialSuccess { rejected_log_records, error_message }),
            _ => ExportLogsServiceResponse::decode(bytes.as_ref()).ok().and_then(|answer| answer.partial_success),
        };
        Ok(accepted(partial, &self.secrets))
    }
}

fn accepted(partial: Option<ExportLogsPartialSuccess>, secrets: &[String]) -> Accepted {
    partial
        .map(|p| Accepted { rejected: p.rejected_log_records, message: clip(&scrubbed_many(&p.error_message, secrets), ERROR_MAX) })
        .unwrap_or_default()
}

/// `grpc-status` and the percent-decoded `grpc-message`, when the map carries a status.
fn grpc_status(headers: &http::HeaderMap) -> Option<(u32, String)> {
    let code = headers.get("grpc-status")?.to_str().ok()?.trim().parse().ok()?;
    let message = headers.get("grpc-message").and_then(|v| v.to_str().ok()).map(percent_decode).unwrap_or_default();
    Some((code, message))
}

fn grpc_refusal(code: u32, message: &str) -> String {
    let name = match code {
        1 => "CANCELLED",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        10 => "ABORTED",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        16 => "UNAUTHENTICATED",
        _ => "error",
    };
    let hint = if code == 12 { " - the server speaks gRPC but serves no OTLP logs service" } else { "" };
    let said = if message.is_empty() { String::new() } else { format!(": {}", clip(message, ERROR_MAX)) };
    format!("the collector refused the export with gRPC {name} ({code}){said}{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::parse;
    use serde_json::json;

    #[test]
    fn grpc_statuses_read_and_say_what_they_mean() {
        let mut headers = http::HeaderMap::new();
        headers.insert("grpc-status", HeaderValue::from_static("14"));
        headers.insert("grpc-message", HeaderValue::from_static("no%20backend"));
        assert_eq!(grpc_status(&headers), Some((14, "no backend".into())));
        assert!(grpc_status(&http::HeaderMap::new()).is_none());
        assert!(grpc_refusal(12, "").contains("no OTLP logs service"));
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%2Cb"), "a,b");
    }

    #[tokio::test]
    async fn the_http_wires_post_to_v1_logs_with_the_headers() {
        std::env::set_var("SWISS_LLM_TEST_OTEL_TOKEN", "tok-otel-9876");
        let (url, server) = crate::provider::tests::fake_server(200, "{}".into()).await;
        let cfg = parse(Some(&json!({
            "protocol": "http/json",
            "endpoint": url,
            "headers": { "authorization": "Bearer ${SWISS_LLM_TEST_OTEL_TOKEN}", "x-team": "core" }
        })))
        .expect("parse")
        .expect("on");
        let wire = Wire::build(&cfg).expect("wire");
        let accepted = wire.export(&ExportLogsServiceRequest::default()).await.expect("export");
        assert_eq!(accepted, Accepted::default());
        let (head, body) = server.await.expect("server");
        assert!(head.starts_with("POST /v1/logs"), "{head}");
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer tok-otel-9876") && lower.contains("x-team: core"), "{head}");
        assert!(lower.contains("content-type: application/json"), "{head}");
        assert_eq!(body, r#"{"resourceLogs":[]}"#);
    }

    #[tokio::test]
    async fn an_http_refusal_is_said_and_never_quotes_a_header() {
        std::env::set_var("SWISS_LLM_TEST_OTEL_TOKEN2", "tok-secret-4321");
        let (url, _server) = crate::provider::tests::fake_server(401, r#"{"message":"bad token tok-secret-4321"}"#.into()).await;
        let cfg = parse(Some(&json!({
            "protocol": "http/protobuf",
            "endpoint": url,
            "headers": { "authorization": "${SWISS_LLM_TEST_OTEL_TOKEN2}" }
        })))
        .expect("parse")
        .expect("on");
        let err = Wire::build(&cfg).expect("wire").export(&ExportLogsServiceRequest::default()).await.expect_err("401");
        assert!(err.message.contains("401") && !err.retryable, "{err:?}");
        assert!(!err.message.contains("tok-secret-4321"), "{err:?}");
        // Nothing listening: a network failure, worth a retry.
        let cfg = parse(Some(&json!({ "protocol": "http/protobuf", "endpoint": "http://127.0.0.1:9" }))).expect("parse").expect("on");
        let err = Wire::build(&cfg).expect("wire").export(&ExportLogsServiceRequest::default()).await.expect_err("down");
        assert!(err.retryable, "{err:?}");
        // The reason, not reqwest's bare "error sending request": the OS error is named.
        assert!(err.message.contains("os error"), "{err:?}");
    }

    #[tokio::test]
    async fn masking_http_refusals_decodes_all_keys_before_clipping() {
        let escaped = r#"k-"quoted\credential"#;
        let body = json!({ "message": format!("first-secret {}{escaped}", "x".repeat(285)) }).to_string();
        let (url, server) = crate::provider::tests::fake_server(401, body).await;
        let cfg = parse(Some(&json!({ "protocol": "http/json", "endpoint": url }))).expect("parse").expect("on");
        let mut wire = Wire::build(&cfg).expect("wire");
        wire.secrets = vec!["first-secret".into(), escaped.into()];
        let err = wire.export(&ExportLogsServiceRequest::default()).await.expect_err("refused");
        server.await.expect("server");
        assert!(!err.message.contains("first-secret") && !err.message.contains("k-\"quo"), "{err:?}");
        assert!(err.message.contains("***") && !err.retryable, "{err:?}");
    }

    #[test]
    fn masking_partial_refusals_preserves_count_without_secret_prefixes() {
        let secret = "partial-secret";
        let result = accepted(Some(ExportLogsPartialSuccess {
            rejected_log_records: 3,
            error_message: format!("{}{secret}", "x".repeat(295)),
        }), &[secret.into()]);
        assert_eq!(result.rejected, 3);
        assert!(!result.message.contains("parti"), "{result:?}");
        assert!(result.message.contains("***"), "{result:?}");
    }

    /// A one-call gRPC collector on h2: reads the framed request, answers `status` either
    /// as a trailers-only header or as a trailer after a framed response.
    async fn fake_grpc(code: u32, trailers_only: bool) -> (String, tokio::task::JoinHandle<(String, Vec<u8>)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let mut conn = h2::server::handshake(socket).await.expect("handshake");
            let (request, mut respond) = conn.accept().await.expect("stream").expect("request");
            // The connection reads the body's frames and writes the answer only while polled:
            // a body arriving after its headers would otherwise wait for the client's timeout.
            let driver = tokio::spawn(async move { while conn.accept().await.is_some() {} });
            let path = request.uri().path().to_string();
            let mut body = request.into_body();
            let mut received = Vec::new();
            while let Some(chunk) = body.data().await {
                let chunk = chunk.expect("chunk");
                let _ = body.flow_control().release_capacity(chunk.len());
                received.extend_from_slice(&chunk);
            }
            let status = code.to_string();
            if trailers_only {
                let answer = http::Response::builder()
                    .header("content-type", "application/grpc")
                    .header("grpc-status", status.as_str())
                    .header("grpc-message", "busy%20now")
                    .body(())
                    .expect("response");
                respond.send_response(answer, true).expect("send");
            } else {
                let answer = http::Response::builder().header("content-type", "application/grpc").body(()).expect("response");
                let mut stream = respond.send_response(answer, false).expect("send");
                stream.send_data(bytes::Bytes::from_static(&[0, 0, 0, 0, 0]), false).expect("data");
                let mut trailers = http::HeaderMap::new();
                trailers.insert("grpc-status", HeaderValue::from_str(&status).expect("status"));
                stream.send_trailers(trailers).expect("trailers");
            }
            // Keep driving the connection until the client hangs up.
            let _ = tokio::time::timeout(Duration::from_secs(2), driver).await;
            (path, received)
        });
        (url, task)
    }

    #[tokio::test]
    async fn grpc_frames_the_request_and_reads_the_trailer() {
        let (url, server) = fake_grpc(0, false).await;
        let cfg = parse(Some(&json!({ "protocol": "grpc", "endpoint": url }))).expect("parse").expect("on");
        let request = ExportLogsServiceRequest {
            resource_logs: vec![otlp::ResourceLogs { resource: None, scope_logs: Vec::new() }],
        };
        Wire::build(&cfg).expect("wire").export(&request).await.expect("accepted");
        let (path, received) = server.await.expect("server");
        assert_eq!(path, GRPC_PATH);
        let payload = request.encode_to_vec();
        assert_eq!(received[0], 0, "uncompressed");
        assert_eq!(u32::from_be_bytes(received[1..5].try_into().expect("len")) as usize, payload.len());
        assert_eq!(&received[5..], payload.as_slice());
    }

    #[tokio::test]
    async fn a_grpc_refusal_in_the_headers_is_retryable_when_unavailable() {
        let (url, _server) = fake_grpc(14, true).await;
        let cfg = parse(Some(&json!({ "protocol": "grpc", "endpoint": url }))).expect("parse").expect("on");
        let err = Wire::build(&cfg).expect("wire").export(&ExportLogsServiceRequest::default()).await.expect_err("refused");
        assert!(err.retryable && err.message.contains("UNAVAILABLE") && err.message.contains("busy now"), "{err:?}");
    }
}
