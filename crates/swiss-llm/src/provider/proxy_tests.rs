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

//! Isolated, finite proxy echo fixtures with auth/status gates before masking checks.

use std::net::SocketAddr;
use std::time::Duration;

use genai::chat::ChatRequest;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{CallError, Chat, ModelEntry, ModelKind, ProviderEntry, Style};
use crate::config;

const DEADLINE: Duration = Duration::from_secs(5);
const REQUEST_MAX: usize = 64 * 1024;
const API_KEY: &str = "api-pass-only-synthetic";
const PROXY_USER: &str = "proxy-user";
const PROXY_PASSWORD: &str = "proxy-pass-only-synthetic";
const BASIC_TOKEN: &str = "cHJveHktdXNlcjpwcm94eS1wYXNzLW9ubHktc3ludGhldGlj";
const CONNECT_BODY_MARKER: &str = "CONNECT_ONLY_PROXY_BODY_MARKER";

#[derive(Clone, Copy)]
enum Echo { Plain, EscapedJson, EncodedBasic, Connect, Combined }

struct References { names: Vec<String> }

impl References {
    fn new(case: &str) -> References { Self::with_password(case, PROXY_PASSWORD) }

    fn with_password(case: &str, password: &str) -> References {
        let names: Vec<String> = ["API", "USER", "PASSWORD"].into_iter()
            .map(|part| format!("SWISS_LLM_PROXY_FIXTURE_{case}_{part}")).collect();
        for (name, value) in names.iter().zip([API_KEY, PROXY_USER, password]) {
            std::env::set_var(name, value);
        }
        References { names }
    }

    fn reference(&self, index: usize) -> String { format!("{}{{{}}}", '$', self.names[index]) }
}

impl Drop for References {
    fn drop(&mut self) { for name in &self.names { std::env::remove_var(name); } }
}

struct Receipt {
    head: String,
    request_body: String,
    response_body: String,
    status_sent: u16,
}

fn header<'a>(head: &'a str, wanted: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(wanted).then_some(value.trim())
    })
}

async fn fake_proxy(echo: Echo) -> (SocketAddr, tokio::task::JoinHandle<Result<Receipt, String>>) {
    fake_proxy_with_password(echo, PROXY_PASSWORD.into()).await
}

async fn fake_proxy_with_password(echo: Echo, password: String) -> (SocketAddr, tokio::task::JoinHandle<Result<Receipt, String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ephemeral loopback proxy");
    let address = listener.local_addr().expect("proxy address");
    let task = tokio::spawn(async move {
        tokio::time::timeout(DEADLINE, async {
            let (mut socket, _) = listener.accept().await.map_err(|err| format!("proxy accept: {err}"))?;
            let mut received = Vec::new();
            let mut scratch = [0u8; 2048];
            let (head, request_body) = loop {
                let count = socket.read(&mut scratch).await.map_err(|err| format!("proxy read: {err}"))?;
                if count == 0 { return Err("proxy request ended before its headers/body".to_string()); }
                if count > REQUEST_MAX - received.len() { return Err("proxy fixture request exceeded its bound".to_string()); }
                received.extend_from_slice(&scratch[..count]);
                let Some(end) = received.windows(4).position(|part| part == b"\r\n\r\n") else { continue };
                let head = String::from_utf8(received[..end].to_vec()).map_err(|_| "proxy headers are not UTF-8".to_string())?;
                let length = header(&head, "content-length").map(str::parse::<usize>).transpose()
                    .map_err(|_| "proxy request length is invalid".to_string())?.unwrap_or(0);
                if length > REQUEST_MAX - end - 4 { return Err("proxy fixture body exceeded its bound".to_string()); }
                if received.len() < end + 4 + length { continue; }
                let body = String::from_utf8(received[end + 4..end + 4 + length].to_vec())
                    .map_err(|_| "proxy request body is not UTF-8".to_string())?;
                break (head, body);
            };
            // The body uses the actual received Basic value for the encoded echo;
            // the test separately proves it equals the synthetic expected value.
            let proxy_auth = header(&head, "proxy-authorization").unwrap_or("MISSING_PROXY_AUTH");
            let message = format!("proxy denied api={API_KEY} password={password}");
            let (content_type, response_body) = match echo {
                Echo::Plain => ("text/plain", message),
                Echo::EscapedJson => ("application/json", json!({ "error": { "message": message } }).to_string()
                    .replace(PROXY_PASSWORD, "proxy-pass-only-\\u0073ynthetic")),
                Echo::EncodedBasic => ("application/json", json!({ "error": { "message": format!("proxy denied api={API_KEY} auth={proxy_auth}") } }).to_string()),
                Echo::Connect => ("text/plain", format!("{CONNECT_BODY_MARKER} api={API_KEY} password={PROXY_PASSWORD} auth={proxy_auth}")),
                Echo::Combined => ("application/json", json!({ "error": { "message": format!("{message} auth={proxy_auth} user={PROXY_USER}") } }).to_string()),
            };
            let answer = format!("HTTP/1.1 407 Proxy Authentication Required\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nproxy-authenticate: Basic realm=\"synthetic-test\"\r\nconnection: close\r\n\r\n{response_body}", response_body.len());
            socket.write_all(answer.as_bytes()).await.map_err(|err| format!("proxy write: {err}"))?;
            Ok(Receipt { head, request_body, response_body, status_sent: 407 })
        }).await.map_err(|_| "proxy fixture deadline expired".to_string())?
    });
    (address, task)
}

async fn request_through_proxy(case: &str, echo: Echo) -> (CallError, Receipt, SocketAddr) {
    let refs = References::new(case);
    let (proxy_address, server) = fake_proxy(echo).await;
    // No origin exists: drop an ephemeral listener before asking the proxy. If a
    // client bypasses the proxy it cannot accidentally reach a real provider.
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("dead loopback origin");
    let origin_address = origin.local_addr().expect("origin address");
    drop(origin);
    let scheme = if matches!(echo, Echo::Connect) { "https" } else { "http" };
    let proxy = format!("http://{}:{}@{proxy_address}", refs.reference(1), refs.reference(2));
    let proxy = config::proxy_url(&proxy, "providers.proxy-fixture.proxy").expect("referenced proxy URL");
    let entry = ProviderEntry {
        name: "proxy-fixture".into(), style: Style::OpenAi,
        base_url: format!("{scheme}://{origin_address}/v1"), api_key: Some(refs.reference(0)),
        models: vec![ModelEntry::new("synthetic-model", ModelKind::Chat)], proxy: Some(proxy),
    };
    let chat = Chat::new(&entry, None, 256).expect("synthetic referenced chat");
    let called = tokio::time::timeout(DEADLINE, chat.exec_streamed(ChatRequest::from_user("synthetic proxy test"), &mut |_| {})).await;
    let served = tokio::time::timeout(DEADLINE, server).await;
    // Collect both finite operations before any credential assertion can fail.
    let receipt = served.expect("proxy join deadline").expect("proxy task").expect("proxy completed response");
    let err = called.expect("chat deadline").err().expect("proxy 407 refuses the request");
    (err, receipt, origin_address)
}

fn assert_http_proxy_gate(err: &CallError, receipt: &Receipt, origin: SocketAddr) {
    assert_eq!(receipt.head.lines().next(), Some(format!("POST http://{origin}/v1/chat/completions HTTP/1.1").as_str()), "absolute-form request actually reached this proxy");
    assert_eq!(header(&receipt.head, "proxy-authorization"), Some(format!("Basic {BASIC_TOKEN}").as_str()), "expected referenced proxy Basic auth actually arrived");
    assert_eq!(header(&receipt.head, "authorization"), Some(format!("Bearer {API_KEY}").as_str()), "separate referenced provider key actually arrived");
    let body: serde_json::Value = serde_json::from_str(&receipt.request_body).expect("actual OpenAI JSON");
    assert_eq!(body["model"], "synthetic-model");
    assert_eq!(receipt.status_sent, 407);
    assert_eq!((err.status, err.kind, err.retryable), (Some(407), "http", false), "actual HTTP status gate must precede masking assertions");
    assert!(!err.message.contains(API_KEY), "existing API-key masking control: {err:?}");
}

#[tokio::test]
async fn provider_proxy_http_plain_echo_masks_resolved_credentials() {
    let (err, receipt, origin) = request_through_proxy("HTTP_PLAIN", Echo::Plain).await;
    assert_http_proxy_gate(&err, &receipt, origin);
    assert!(receipt.response_body.contains(PROXY_PASSWORD), "fixture actually echoed the password");
    assert!(!err.message.contains(PROXY_PASSWORD), "resolved proxy password must not escape: {err:?}");
}

#[tokio::test]
async fn provider_proxy_http_escaped_json_echo_masks_resolved_credentials() {
    let (err, receipt, origin) = request_through_proxy("HTTP_ESCAPED", Echo::EscapedJson).await;
    assert_http_proxy_gate(&err, &receipt, origin);
    assert!(!receipt.response_body.contains(PROXY_PASSWORD), "fixture must exercise decoded, not raw string masking");
    assert!(receipt.response_body.contains("\\u0073"), "fixture uses a JSON-escaped credential byte");
    assert!(!err.message.contains(PROXY_PASSWORD), "mask the decoded proxy password before detail clipping: {err:?}");
}

#[tokio::test]
async fn provider_proxy_http_basic_echo_masks_encoded_credential() {
    let (err, receipt, origin) = request_through_proxy("HTTP_BASIC", Echo::EncodedBasic).await;
    assert_http_proxy_gate(&err, &receipt, origin);
    assert!(receipt.response_body.contains(BASIC_TOKEN), "fixture echoed the actual received Basic credential");
    assert!(!err.message.contains(BASIC_TOKEN), "the known proxy Basic credential is a secret too: {err:?}");
}

#[tokio::test]
async fn provider_proxy_connect_407_body_is_not_a_returned_detail() {
    let (err, receipt, origin) = request_through_proxy("HTTPS_CONNECT", Echo::Connect).await;
    assert_eq!(receipt.head.lines().next(), Some(format!("CONNECT {origin} HTTP/1.1").as_str()), "CONNECT actually reached this proxy");
    assert_eq!(header(&receipt.head, "proxy-authorization"), Some(format!("Basic {BASIC_TOKEN}").as_str()));
    assert_eq!(receipt.status_sent, 407);
    assert!(receipt.response_body.contains(PROXY_PASSWORD));
    assert_eq!((err.status, err.kind, err.retryable), (None, "network", true), "tunnel failure is not an origin HTTP response");
    assert!(err.message.contains("proxy authorization required"), "fixed tunnel 407 classification: {err:?}");
    for needle in [API_KEY, PROXY_PASSWORD, BASIC_TOKEN, CONNECT_BODY_MARKER] {
        assert!(!err.message.contains(needle), "CONNECT never surfaces the response body: {err:?}");
    }
}

#[tokio::test]
async fn provider_proxy_encoded_passwords_match_received_basic_snapshot() {
    for (index, (raw, decoded, expected)) in [
        ("literal+plus", "literal+plus", "cHJveHktdXNlcjpsaXRlcmFsK3BsdXM="),
        ("encoded%2Bplus", "encoded+plus", "cHJveHktdXNlcjplbmNvZGVkK3BsdXM="),
        ("caf%C3%A9%E4%BD%A0", "café你", "cHJveHktdXNlcjpjYWbDqeS9oA=="),
        ("lower%2bplus", "lower+plus", "cHJveHktdXNlcjpsb3dlcitwbHVz"),
        ("bad%xy%2", "bad%xy%2", "cHJveHktdXNlcjpiYWQleHklMg=="),
        ("lossy%fftail", "lossy�tail", "cHJveHktdXNlcjpsb3Nzee+/vXRhaWw="),
    ].into_iter().enumerate() {
        let refs = References::with_password(&format!("ENCODED_{index}"), raw);
        let (address, server) = fake_proxy_with_password(Echo::Combined, decoded.into()).await;
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("dead origin");
        let origin_address = origin.local_addr().expect("origin address");
        drop(origin);
        let entry = ProviderEntry {
            name: "encoded-fixture".into(), style: Style::OpenAi,
            base_url: format!("http://{origin_address}/v1"), api_key: Some(refs.reference(0)),
            models: vec![ModelEntry::new("synthetic-model", ModelKind::Chat)],
            proxy: Some(format!("http://{}:{}@{address}", refs.reference(1), refs.reference(2))),
        };
        let chat = Chat::new(&entry, None, 256).expect("referenced client");
        // A later env edit must not change either the transmitted proxy credential
        // or the masks belonging to this already-built client.
        std::env::set_var(&refs.names[2], "different-password-after-build");
        let called = tokio::time::timeout(DEADLINE, chat.exec_streamed(ChatRequest::from_user("encoded proxy"), &mut |_| {})).await;
        let served = tokio::time::timeout(DEADLINE, server).await;
        let receipt = served.expect("join deadline").expect("server task").expect("proxy response");
        let err = called.expect("call deadline").err().expect("HTTP refusal");
        assert_eq!(receipt.head.lines().next(), Some(format!("POST http://{origin_address}/v1/chat/completions HTTP/1.1").as_str()));
        assert_eq!(header(&receipt.head, "proxy-authorization"), Some(format!("Basic {expected}").as_str()), "wire decoder/encoder control for {raw}");
        assert_eq!((receipt.status_sent, err.status, err.kind, err.retryable), (407, Some(407), "http", false));
        assert!(receipt.response_body.contains(decoded), "actual echoed decoded password");
        for credential in [decoded, expected, API_KEY] { assert!(!err.message.contains(credential), "captured credential {raw}: {err:?}"); }
        assert!(err.message.contains(PROXY_USER), "the username is not anonymized: {err:?}");
        let recorded = chat.wire().to_string();
        for credential in [decoded, expected, API_KEY] { assert!(!recorded.contains(credential), "credential values are not filed"); }
    }
}

#[derive(Clone, Copy)]
enum Consumer { Blocking, Native, Discovery, Embedding }

async fn consumer_refusal(case: &str, consumer: Consumer) {
    let refs = References::new(case);
    let (address, server) = fake_proxy(Echo::Combined).await;
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("dead origin");
    let origin_address = origin.local_addr().expect("origin address");
    drop(origin);
    let entry = ProviderEntry {
        name: "consumer-fixture".into(), style: if matches!(consumer, Consumer::Native) { Style::Anthropic } else { Style::OpenAi },
        base_url: format!("http://{origin_address}/v1"), api_key: Some(refs.reference(0)),
        models: vec![ModelEntry::new("synthetic-model", if matches!(consumer, Consumer::Embedding) { ModelKind::Embedding } else { ModelKind::Chat })],
        proxy: Some(format!("http://{}:{}@{address}", refs.reference(1), refs.reference(2))),
    };
    let mut typed = None;
    let called = tokio::time::timeout(DEADLINE, async {
        match consumer {
            Consumer::Blocking => {
                let chat = Chat::new(&entry, None, 256).expect("chat");
                let result = chat.client.exec_chat(&chat.model_id, ChatRequest::from_user("blocking proxy"), Some(&chat.options)).await;
                result.map(|_| ()).map_err(|source| {
                    let err = super::call_error(&source, &chat.error_secrets, chat.key.is_empty(), 7);
                    typed = Some((err.status, err.kind, err.retryable, err.duration_ms));
                    err.message
                })
            }
            Consumer::Native => {
                let chat = Chat::new(&entry, None, 256).expect("native chat");
                chat.exec_streamed(ChatRequest::from_user("native proxy"), &mut |_| {}).await.map(|_| ()).map_err(|err| {
                    typed = Some((err.status, err.kind, err.retryable, err.duration_ms));
                    err.message
                })
            }
            Consumer::Discovery => super::discover(&entry).await.map(|_| ()),
            Consumer::Embedding => {
                let cfg = crate::embedding::parse(Some(&json!({ "provider": entry.name })), &std::collections::BTreeMap::from([(entry.name.clone(), entry.clone())]))
                    .expect("embedding config").expect("embedding enabled");
                std::env::set_var(&refs.names[0], "stale-provider-key-before-build");
                let client = crate::embedding::EmbedClient::build(&cfg).expect("embedding client");
                std::env::set_var(&refs.names[0], API_KEY);
                std::env::set_var(&refs.names[2], "different-password-after-build");
                client.embed(vec!["synthetic input".into()]).await.map(|_| ())
            }
        }
    }).await;
    let served = tokio::time::timeout(DEADLINE, server).await;
    let receipt = served.expect("join deadline").expect("server task").expect("proxy response");
    let message = called.expect("call deadline").expect_err("407 refusal");
    let (method, path) = match consumer {
        Consumer::Blocking => ("POST", "chat/completions"), Consumer::Native => ("POST", "messages"),
        Consumer::Discovery => ("GET", "models"), Consumer::Embedding => ("POST", "embeddings"),
    };
    assert_eq!(receipt.head.lines().next(), Some(format!("{method} http://{origin_address}/v1/{path} HTTP/1.1").as_str()));
    assert_eq!(header(&receipt.head, "proxy-authorization"), Some(format!("Basic {BASIC_TOKEN}").as_str()));
    assert_eq!(header(&receipt.head, "authorization"), Some(format!("Bearer {API_KEY}").as_str()));
    assert_eq!(receipt.status_sent, 407);
    assert!(message.contains("407"), "actual refusal classification: {message}");
    if let Some((status, kind, retryable, duration)) = typed {
        assert_eq!((status, kind, retryable), (Some(407), "http", false));
        if matches!(consumer, Consumer::Blocking) { assert_eq!(duration, 7); }
    }
    if matches!(consumer, Consumer::Native) { assert_eq!(header(&receipt.head, "x-api-key"), Some(API_KEY), "native dual headers stay intact"); }
    for credential in [API_KEY, PROXY_PASSWORD, BASIC_TOKEN] {
        assert!(receipt.response_body.contains(credential), "fixture actually echoed credential");
        assert!(!message.contains(credential), "captured masks reach this consumer: {message}");
    }
    assert!(message.contains(PROXY_USER), "public username remains visible: {message}");
}

#[tokio::test]
async fn provider_proxy_blocking_genai_status_errors_use_captured_masks() { consumer_refusal("BLOCKING", Consumer::Blocking).await; }

#[tokio::test]
async fn provider_proxy_native_status_errors_use_captured_masks() { consumer_refusal("NATIVE", Consumer::Native).await; }

#[tokio::test]
async fn provider_proxy_discovery_errors_use_captured_masks() { consumer_refusal("DISCOVERY", Consumer::Discovery).await; }

#[tokio::test]
async fn provider_proxy_embedding_errors_preserve_key_rotation_and_masks() { consumer_refusal("EMBEDDING", Consumer::Embedding).await; }

