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

//! A ring buffer of recent MCP interactions — port of `traffic.ts`: every JSON-RPC request that
//! crossed the gateway, attributed to the token that sent it and to the client's self-reported
//! name (from `initialize`).
//!
//! This is the "who talked to my MCPs and what did they ask" view that the tool-call log
//! (calls.rs) can't give on its own: it captures initialize / tools/list / resources/list /
//! resources/read too, not just tools/call, and it records the clientInfo a client announces
//! during the handshake.
//!
//! Bounded but durable: the newest KEEP entries also append to one JSONL tail on disk and are
//! restored when the persistent instance is constructed, so a gateway restart no longer blanks
//! this view while the tool-call log keeps its history. The ring stays what reads serve; the
//! file is a recovery tail, bounded by a byte budget like calls.rs.
//!
//! [TrafficLog] is an INSTANCE, shared as an `Arc` between the app context and the proxy layer
//! that records — the S2 instantiation calls.rs finished and SPEC §jobs.migrate gave RunLog: no
//! process-global ring, so two apps in one process (or two tests in one binary) each hold
//! their own and never see each other's rows.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Map, Value};

use swiss_core::platform::{chmod_private, mkdir_private, private_file_mode};
use swiss_core::util::now_ms;
use swiss_host::mask::is_secret_arg_key;

#[derive(Serialize, serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEntry {
    pub seq: u64,
    pub at: String,
    pub mcp: String,
    pub method: String,
    /// Token label that authenticated the request (absent for panel calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Self-reported app name, lifted from initialize params.clientInfo (or server/discover _meta).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// Redacted, clipped summary of the request params.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    /// The full request message (jsonrpc/id/method/params), redacted, for the expandable raw view.
    pub body: String,
    /// The JSON-RPC reply (result/error), redacted + clipped, for the expandable raw view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    pub ok: bool,
    pub ms: u64,
}

const KEEP: usize = 500;
/// Matches the tool-call log's page (CALLS_PAGE_SIZE). Both views are one-line-per-row logs you
/// scan, and 20 rows is about a screen — you page rather than scroll, then page.
const PAGE_SIZE: usize = 20;

const PARAMS_MAX: usize = 512;
/// Cap on the full request body stored for the expandable raw view (keeps the ring + poll small).
const BODY_MAX: usize = 8 * 1024;
/// Cap on the stored reply (same rationale as BODY_MAX).
const RESPONSE_MAX: usize = 8 * 1024;

struct TrafficState {
    ring: Vec<TrafficEntry>,
    seq: u64,
    /// clientInfo is announced only during initialize / server/discover; every later frame
    /// (tools/list, resources/read, …) omits it. Without remembering it, one client splits into
    /// two rows — the few frames that carried a name, and the rest. Keyed by token: the gateway
    /// is stateless HTTP (no session id), and the token is the stable identity we authenticate.
    known_client: std::collections::HashMap<String, (Option<String>, Option<String>)>,
    /// None on a memory-only log: without a file the log behaves exactly as it did before
    /// persistence existed (in-memory ring only — what unit tests and per-test app harnesses
    /// get).
    file: Option<PathBuf>,
    bytes: u64,
}

/// One traffic log: the ring, the sequence, the token→client memory and the armed recovery
/// file, plus everything mutable about writing it. Shared as an `Arc<TrafficLog>` between the
/// app context and the proxy layer that records, so what the proxy writes is exactly what the
/// panel reads — and two logs in one process never see each other's rows (the S2
/// instantiation calls.rs finished; SPEC §jobs.migrate gave RunLog the same shape).
pub struct TrafficLog {
    /// Behind an Arc so the spawned writers can update `bytes` without borrowing the log.
    state: Arc<Mutex<TrafficState>>,
    /// The serialized writer queue (Node chained promises on `trafficQueue`).
    write_queue: Arc<tokio::sync::Mutex<()>>,
    /// Writes registered, counted BEFORE the writer task is spawned, because taking the queue
    /// is not enough on its own: a flush arriving before the task is first polled would find
    /// the queue free and return with the write still pending. (The Node build chained onto the
    /// queue promise inline and had no such window.)
    registered: std::sync::atomic::AtomicU64,
    /// Writes that have reached the file; `flush_traffic` waits for it to catch up.
    completed: tokio::sync::watch::Sender<u64>,
}

impl TrafficLog {
    /// A memory-only log: the ring answers reads and nothing is ever written or restored —
    /// what unit tests and per-test app harnesses want (behaviorally equal to the old
    /// never-armed global).
    pub fn memory() -> TrafficLog {
        TrafficLog {
            state: Arc::new(Mutex::new(TrafficState {
                ring: Vec::new(),
                seq: 0,
                known_client: std::collections::HashMap::new(),
                file: None,
                bytes: 0,
            })),
            write_queue: Arc::new(tokio::sync::Mutex::new(())),
            registered: std::sync::atomic::AtomicU64::new(0),
            completed: tokio::sync::watch::Sender::new(0),
        }
    }

    /// A persistent log writing `traffic.jsonl` under `dir` (the production one is
    /// `data_path(["logs"])`), constructed with its pre-restart tail already restored — the
    /// two halves the old `init_traffic_log` performed on process-global state (construct,
    /// then attach the file), folded into construction because nothing can record into an
    /// instance before it exists. Where that merge once re-numbered ring entries recorded
    /// before the file was armed, an instance starts empty by construction, so the sequence
    /// simply continues from the newest seq on disk.
    pub fn at(dir: PathBuf) -> TrafficLog {
        let log = TrafficLog::memory();
        let file = dir.join("traffic.jsonl");
        let restored: Vec<TrafficEntry> = crate::calls::tail_lines(&file, KEEP)
            .ok()
            .map(|tail| {
                tail.lines
                    .iter()
                    .filter_map(|l| parse_traffic_entry(l))
                    .rev()
                    .collect()
            })
            .unwrap_or_default();
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        if let Ok(mut s) = log.state.lock() {
            // The token→client memory rides along: without it, restored traffic would show
            // "token X" rows until that client happens to send another initialize.
            for e in &restored {
                if let (Some(client), Some(name)) = (&e.client, &e.client_name) {
                    s.known_client.insert(
                        client.clone(),
                        (Some(name.clone()), e.client_version.clone()),
                    );
                }
            }
            s.seq = restored.iter().map(|e| e.seq).max().unwrap_or(0);
            s.ring = restored;
            s.file = Some(file);
            s.bytes = bytes;
        }
        log
    }
}

/// Counts one write as done however the task ends, so a flush can never be left hanging.
struct WriteDone(tokio::sync::watch::Sender<u64>);

impl Drop for WriteDone {
    fn drop(&mut self) {
        let next = *self.0.borrow() + 1;
        self.0.send_replace(next);
    }
}

impl TrafficLog {
    /// Register a write, then run it inside the serialized queue.
    fn spawn_write(
        &self,
        where_: &'static str,
        job: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.registered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let queue = self.write_queue.clone();
        let completed = self.completed.clone();
        tokio::spawn(async move {
            let _done = WriteDone(completed);
            let _guard = queue.lock().await;
            if let Err(err) = job() {
                on_write_error(where_, &err);
            }
        });
    }
}

static LAST_WRITE_ERROR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn on_write_error(where_: &str, err: &str) {
    // A log that cannot be written must never break a request, but silence would be worse.
    let now = now_ms();
    if now.saturating_sub(LAST_WRITE_ERROR.load(std::sync::atomic::Ordering::Relaxed)) < 30_000 {
        return;
    }
    LAST_WRITE_ERROR.store(now, std::sync::atomic::Ordering::Relaxed);
    swiss_core::log::warn(
        "traffic log write failed",
        Some(json!({ "where": where_, "err": err })),
    );
}

/// Redact any value whose key looks like a secret, recursing into objects/arrays.
fn redact(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if !k.is_empty() && is_secret_arg_key(k) {
                        (k.clone(), Value::String("•••".into()))
                    } else {
                        (k.clone(), redact(v))
                    }
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// Lift clientInfo out of the params, leaving the rest to be logged.
///
/// Two shapes carry it: `initialize` puts it at the top level (params.clientInfo), while
/// `server/discover` — the capability probe Claude Code sends — nests it under
/// `params._meta["io.modelcontextprotocol/clientInfo"]`. Without the second path every discover
/// row showed "unknown client" even though the client had just announced itself.
fn split_client_info(params: &Value) -> (Option<String>, Option<String>, Value) {
    let Some(obj) = params.as_object() else {
        return (None, None, params.clone());
    };
    let mut client_name = None;
    let mut client_version = None;
    let mut rest: Map<String, Value> = obj.clone();

    // initialize: top-level clientInfo.
    if let Some(ci) = obj.get("clientInfo").and_then(Value::as_object) {
        client_name = ci.get("name").and_then(Value::as_str).map(str::to_string);
        client_version = ci
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string);
        rest.remove("clientInfo");
    }

    // server/discover: clientInfo nested under _meta. Fill only what the top-level path did not.
    if let Some(meta) = obj.get("_meta").and_then(Value::as_object) {
        if let Some(ci) = meta
            .get("io.modelcontextprotocol/clientInfo")
            .and_then(Value::as_object)
        {
            if client_name.is_none() {
                client_name = ci.get("name").and_then(Value::as_str).map(str::to_string);
            }
            if client_version.is_none() {
                client_version = ci
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
    }
    (client_name, client_version, Value::Object(rest))
}

/// Reduce the raw HTTP response body to the JSON-RPC reply (result/error) a client received.
///
/// The two eras frame the reply differently: legacy (2025) answers a POST over an SSE stream, so
/// the JSON-RPC message rides a `data:` line; modern (2026) answers with a single JSON body.
/// Pull the terminal result/error out of either shape, redact it, and clip it — the same
/// treatment the request body gets.
fn summarize_response(text: Option<&str>) -> Option<String> {
    let text = text?;
    let data_lines: Vec<&str> = text
        .lines()
        .filter_map(|l| {
            let trimmed = l.trim_start();
            trimmed
                .strip_prefix("data:")
                .map(|payload| payload.trim_start())
        })
        .collect();
    let candidates: Vec<&str> = if !data_lines.is_empty() {
        data_lines
    } else {
        vec![text]
    };
    let mut chosen: Option<Value> = None;
    for c in candidates {
        let Ok(obj) = serde_json::from_str::<Value>(c) else {
            continue;
        };
        // Keep the terminal result/error; skip request-related notifications (progress, logging).
        if obj.get("result").is_some() || obj.get("error").is_some() {
            chosen = Some(obj);
        }
    }
    match chosen {
        None => {
            // No parseable JSON-RPC message (a reply too large to capture whole, or a non-JSON
            // body): show the raw text so the view never implies a reply that wasn't inspected.
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(clip(trimmed, RESPONSE_MAX))
            }
        }
        Some(obj) => {
            let redacted = redact(&obj);
            Some(clip(
                &serde_json::to_string(&redacted).unwrap_or_default(),
                RESPONSE_MAX,
            ))
        }
    }
}

impl TrafficLog {
    /// Record one JSON-RPC request (or each, when a client sends a batch). No-op for non-MCP bodies.
    ///
    /// `response_text` is the raw bytes written as the HTTP response (a JSON body or SSE data lines),
    /// so the log shows the reply, not only the request.
    pub fn record_traffic(
        &self,
        mcp: &str,
        body: Option<&Value>,
        client: Option<&str>,
        ok: bool,
        ms: u64,
        response_text: Option<&str>,
    ) {
        let resp = summarize_response(response_text);
        let Some(body) = body else { return };
        let msgs: Vec<&Value> = body
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_else(|| vec![body]);
        let mut to_persist: Vec<TrafficEntry> = Vec::new();
        for m in msgs {
            let Some(method) = m.get("method").and_then(Value::as_str) else {
                continue;
            };
            let params = m.get("params").cloned().unwrap_or(Value::Null);
            let (client_name, client_version, rest) = split_client_info(&params);
            let mut client_name = client_name;
            let mut client_version = client_version;
            let mut learned: Option<(Option<String>, Option<String>)> = None;
            if let Ok(mut s) = self.state.lock() {
                // Remember the name this token announced, so its later (clientInfo-less) frames are
                // attributed.
                if let (Some(client), Some(name)) = (client, client_name.as_deref()) {
                    if !name.is_empty() {
                        s.known_client.insert(
                            client.to_string(),
                            (client_name.clone(), client_version.clone()),
                        );
                    }
                }
                if client_name.is_none() {
                    if let Some(client) = client {
                        learned = s.known_client.get(client).cloned();
                    }
                }
            }
            if let Some((name, version)) = learned {
                if client_name.is_none() {
                    client_name = name;
                }
                if client_version.is_none() {
                    client_version = version;
                }
            }
            let has_params = rest.as_object().is_some_and(|o| !o.is_empty());
            let mut entry = json!({
                "seq": 0, // assigned below, under the state lock
                "at": swiss_core::log::iso_now(),
                "mcp": mcp,
                "method": method,
                "body": clip(&serde_json::to_string(&redact(m)).unwrap_or_default(), BODY_MAX),
                "ok": ok,
                "ms": ms,
            });
            if let Some(client) = client {
                entry["client"] = json!(client);
            }
            if let Some(name) = &client_name {
                entry["clientName"] = json!(name);
                if let Some(v) = &client_version {
                    entry["clientVersion"] = json!(v);
                }
            }
            if has_params {
                entry["params"] = json!(clip(
                    &serde_json::to_string(&redact(&rest)).unwrap_or_default(),
                    PARAMS_MAX
                ));
            }
            if let Some(resp) = &resp {
                entry["response"] = json!(resp);
            }
            let entry: TrafficEntry = serde_json::from_value(entry).unwrap_or(TrafficEntry {
                seq: 0,
                at: String::new(),
                mcp: mcp.to_string(),
                method: method.to_string(),
                client: None,
                client_name: None,
                client_version: None,
                params: None,
                body: String::new(),
                response: None,
                ok,
                ms,
            });
            to_persist.push(entry);
        }
        let mut armed: Option<PathBuf> = None;
        let mut recorded: Vec<TrafficEntry> = Vec::new();
        if let Ok(mut s) = self.state.lock() {
            for mut entry in to_persist {
                s.seq += 1;
                entry.seq = s.seq;
                s.ring.push(entry.clone());
                if s.ring.len() > KEEP {
                    let cut = s.ring.len() - KEEP;
                    s.ring.drain(..cut);
                }
                recorded.push(entry);
            }
            armed = s.file.clone();
        }
        if armed.is_some() {
            for entry in recorded {
                self.persist_traffic(entry);
            }
        }
    }
}

/// The methods that represent something a user asked for, as opposed to the protocol scaffolding
/// (initialize, tools/list, notifications/…). The panel defaults to these so a handshake storm
/// does not bury the four calls you came to look at. Server-side because the filter now runs
/// before paging: filtering a page of 50 in the browser can leave a page showing nothing at all.
const ACTION_METHODS: [&str; 7] = [
    "tools/call",
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "prompts/get",
    "completion/complete",
    "logging/setLevel",
];

impl TrafficLog {
    /// The client identity the panel groups rows by — keep in lock-step with `clientKey` in the
    /// panel's js/traffic.js.
    pub fn client_key_of(&self, e: &TrafficEntry) -> String {
        if e.client_name.is_some() {
            return format!("n:{}", e.client_name.clone().unwrap_or_default());
        }
        if let Some(client) = &e.client {
            return format!("t:{client}");
        }
        "?".into()
    }
}

pub struct TrafficQuery<'a> {
    pub mcp: Option<&'a str>,
    /// Prefixed client key (`n:claude-code` / `t:default`), the same one clear_traffic takes.
    pub client: Option<&'a str>,
    pub method: Option<&'a str>,
    /// Only the methods a user performs, excluding the protocol handshake.
    pub actions_only: bool,
    /// 0-based page of `pageSize`, newest first.
    pub page: usize,
    pub page_size: Option<usize>,
}

impl TrafficLog {
    fn matches(&self, e: &TrafficEntry, q: &TrafficQuery) -> bool {
        if let Some(mcp) = q.mcp {
            if e.mcp != mcp {
                return false;
            }
        }
        if let Some(client) = q.client {
            if self.client_key_of(e) != client {
                return false;
            }
        }
        if let Some(method) = q.method {
            if e.method != method {
                return false;
            }
        }
        if q.actions_only && !ACTION_METHODS.contains(&e.method.as_str()) {
            return false;
        }
        true
    }
}

impl TrafficLog {
    /// One newest-first page of interactions.
    ///
    /// The rows carry no `body`/`response`: each is capped at 8 KB, so a 200-row answer could reach
    /// 3 MB — polled every 6 seconds, to render a dozen visible lines whose raw JSON is hidden until
    /// you expand one. The panel fetches a single entry's payload from read_traffic_entry when a row
    /// is opened. `hasResponse` survives because the collapsed row says whether a reply was captured.
    pub fn read_traffic(&self, q: &TrafficQuery) -> Value {
        let s = self.state.lock().ok();
        let Some(s) = s else {
            return json!({ "entries": [], "total": 0, "totalUnfiltered": 0, "page": 0, "pageSize": PAGE_SIZE, "more": false });
        };
        let page_size = q.page_size.unwrap_or(PAGE_SIZE).clamp(1, KEEP);
        let filtered = q.mcp.is_some() || q.client.is_some() || q.method.is_some() || q.actions_only;
        let all: Vec<&TrafficEntry> = if filtered {
            s.ring.iter().filter(|e| self.matches(e, q)).collect()
        } else {
            s.ring.iter().collect()
        };
        let start = q.page * page_size;
        // The ring is oldest-first; the view is newest-first, so a page is taken from the end.
        let end = all.len().saturating_sub(start);
        let rows: Vec<Value> = if end == 0 {
            Vec::new()
        } else {
            let window = &all[end.saturating_sub(page_size)..end];
            window
                .iter()
                .rev()
                .map(|e| {
                    let mut row = serde_json::to_value(e).unwrap_or(Value::Null);
                    let has_response = row.get("response").is_some();
                    if let Some(o) = row.as_object_mut() {
                        o.remove("body");
                        o.remove("response");
                    }
                    row["hasResponse"] = json!(has_response);
                    row
                })
                .collect()
        };
        json!({
            "entries": rows,
            "total": all.len(),
            "totalUnfiltered": s.ring.len(),
            "page": q.page,
            "pageSize": page_size,
            "more": end.saturating_sub(page_size) > 0,
        })
    }

    /// One entry's raw request and reply, fetched when a row is expanded.
    pub fn read_traffic_entry(&self, seq: u64) -> Option<Value> {
        let s = self.state.lock().ok()?;
        let e = s.ring.iter().find(|x| x.seq == seq)?;
        let mut out = json!({ "body": e.body });
        if let Some(response) = &e.response {
            out["response"] = json!(response);
        }
        Some(out)
    }

    /// Every client seen in the ring, folded to one row each — computed over the WHOLE ring, never
    /// over the current page. This summary is the answer to "who is talking to my gateway", and a
    /// summary derived from page 3 of an actions-only filter would answer a different question each
    /// time you paged. Newest-active first.
    pub fn traffic_clients(&self) -> Vec<Value> {
        let Ok(s) = self.state.lock() else {
            return Vec::new();
        };
        struct Acc {
            label: String,
            tokens: std::collections::BTreeSet<String>,
            mcps: std::collections::BTreeSet<String>,
            count: usize,
            last_at: String,
            last_seq: u64,
        }
        let mut map: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();
        for e in &s.ring {
            let key = self.client_key_of(e);
            let entry = map.entry(key.clone()).or_insert_with(|| {
                let label = if let Some(name) = &e.client_name {
                    match &e.client_version {
                        Some(v) => format!("{name} {v}"),
                        None => name.clone(),
                    }
                } else if let Some(client) = &e.client {
                    format!("token {client}")
                } else {
                    "unknown client".into()
                };
                Acc {
                    label,
                    tokens: std::collections::BTreeSet::new(),
                    mcps: std::collections::BTreeSet::new(),
                    count: 0,
                    last_at: e.at.clone(),
                    last_seq: e.seq,
                }
            });
            // The label can improve mid-ring: the frames before `initialize` land under "token X",
            // and the handshake then names the client. Take the better one rather than whichever came
            // first.
            if let Some(name) = &e.client_name {
                entry.label = match &e.client_version {
                    Some(v) => format!("{name} {v}"),
                    None => name.clone(),
                };
            }
            if let Some(client) = &e.client {
                entry.tokens.insert(client.clone());
            }
            entry.mcps.insert(e.mcp.clone());
            entry.count += 1;
            if e.seq > entry.last_seq {
                entry.last_seq = e.seq;
                entry.last_at = e.at.clone();
            }
        }
        let mut out: Vec<Value> = map
            .into_iter()
            .map(|(key, c)| {
                json!({
                    "key": key,
                    "label": c.label,
                    "tokens": c.tokens.into_iter().collect::<Vec<_>>(),
                    "mcps": c.mcps.into_iter().collect::<Vec<_>>(),
                    "count": c.count,
                    "lastAt": c.last_at,
                    "lastSeq": c.last_seq,
                })
            })
            .collect();
        out.sort_by(|a, b| b["lastSeq"].as_u64().cmp(&a["lastSeq"].as_u64()));
        out
    }

    /// Drop recorded interactions. With a `client_key` (the panel's prefixed key, e.g. `n:claude-code`
    /// or `t:default`) only that client's rows are dropped and the rest kept; without it everything
    /// is cleared. (The panel's Clear button clears the selected client when one is filtered,
    /// otherwise all.)
    ///
    /// Intentionally does NOT clear known_client: "Clear" wipes the log, not the gateway's memory of
    /// which token is which client. Forgetting it here would make ongoing traffic drop back to
    /// "token X" until the client happens to re-initialize — the split looking like it came back.
    pub fn clear_traffic(&self, client_key: Option<&str>) {
        let armed = {
            let Ok(mut s) = self.state.lock() else { return };
            match client_key {
                Some(key) => s.ring.retain(|e| self.client_key_of(e) != key),
                None => {
                    s.ring.clear();
                    s.seq = 0;
                }
            }
            s.file.clone()
        };
        if armed.is_some() {
            // The file follows the ring: rewritten from whatever survives.
            let state = self.state.clone();
            self.spawn_write("clear", move || rewrite_traffic_file(&state));
        }
    }
}

// --- durability: the ring survives a restart ---------------------------------------------------

/// File budget: trim the tail once it passes 2 MB, keeping the newest ~1 MB — the same shape and
/// the same numbers as the call log's index, for a log whose entries can reach ~16 KB.
const TRAFFIC_MAX_BYTES: u64 = 2 * 1024 * 1024;
const TRAFFIC_KEEP_BYTES: u64 = 1024 * 1024;

/// One disk line back to an entry, skipping torn lines (killed mid-append) rather than failing
/// boot.
fn parse_traffic_entry(line: &str) -> Option<TrafficEntry> {
    let e = serde_json::from_str::<TrafficEntry>(line).ok()?;
    if e.mcp.is_empty() || e.method.is_empty() || e.body.is_empty() {
        return None;
    }
    Some(e)
}

impl TrafficLog {
    /// Append one entry. Serialized so lines cannot interleave; never awaited by the caller —
    /// recording traffic must not sit on the hot path of a request.
    fn persist_traffic(&self, entry: TrafficEntry) {
        let state = self.state.clone();
        self.spawn_write("append", move || append_traffic(&state, &entry));
    }
}

fn append_traffic(state: &Arc<Mutex<TrafficState>>, entry: &TrafficEntry) -> Result<(), String> {
    use std::io::Write;
    let file = {
        let s = state.lock().map_err(|_| "state poisoned".to_string())?;
        s.file.clone().ok_or_else(|| "log not armed".to_string())?
    };
    mkdir_private(file.parent().ok_or("no parent")?);
    let line = format!(
        "{}\n",
        serde_json::to_string(entry).map_err(|e| e.to_string())?
    );
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .map_err(|e| e.to_string())?;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    }
    chmod_private(&file, private_file_mode());
    let mut s = state.lock().map_err(|_| "state poisoned".to_string())?;
    s.bytes += line.len() as u64;
    let over = s.bytes > TRAFFIC_MAX_BYTES;
    drop(s);
    if over {
        trim_traffic_log(state)?;
    }
    Ok(())
}

fn rewrite_traffic_file(state: &Arc<Mutex<TrafficState>>) -> Result<(), String> {
    let (file, lines) = {
        let s = state.lock().map_err(|_| "state poisoned".to_string())?;
        let file = s.file.clone().ok_or_else(|| "log not armed".to_string())?;
        let lines = s
            .ring
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap_or_default()))
            .collect::<String>();
        (file, lines)
    };
    swiss_core::atomic_json::write_text_atomic(&file, &lines)?;
    if let Ok(mut s) = state.lock() {
        s.bytes = lines.len() as u64;
    }
    Ok(())
}

/// Drop the oldest lines once the tail outgrows its budget — the call log's trim, with one fix:
/// the read handle is closed BEFORE the tmp→rename swap, because Windows refuses to replace a
/// file that is still open.
fn trim_traffic_log(state: &Arc<Mutex<TrafficState>>) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let file = {
        let s = state.lock().map_err(|_| "state poisoned".to_string())?;
        s.file.clone().ok_or_else(|| "log not armed".to_string())?
    };
    let size = std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
    let start = size.saturating_sub(TRAFFIC_KEEP_BYTES) as usize;
    let mut f = std::fs::File::open(&file).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; size as usize - start];
    f.seek(SeekFrom::Start(start as u64))
        .map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    drop(f);
    // Start at the first line boundary, so the file never begins with half an entry.
    let kept: &[u8] = match buf.iter().position(|b| *b == b'\n') {
        Some(nl) => &buf[nl + 1..],
        None => &[],
    };
    swiss_core::atomic_json::write_text_atomic(&file, &String::from_utf8_lossy(kept))?;
    let kept_len = kept.len() as u64;
    if let Ok(mut s) = state.lock() {
        s.bytes = kept_len;
    }
    swiss_core::log::log(
        "info",
        "traffic log trimmed",
        Some(json!({ "keptBytes": kept_len })),
    );
    Ok(())
}

impl TrafficLog {
    /// Wait for every pending write (the shutdown path and tests).
    pub async fn flush_traffic(&self) {
        let want = self.registered.load(std::sync::atomic::Ordering::SeqCst);
        let mut done = self.completed.subscribe();
        while *done.borrow_and_update() < want {
            if done.changed().await.is_err() {
                break;
            }
        }
        let _guard = self.write_queue.lock().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
    }

    fn q<'a>() -> TrafficQuery<'a> {
        TrafficQuery {
            mcp: None,
            client: None,
            method: None,
            actions_only: false,
            page: 0,
            page_size: None,
        }
    }

    fn rows(page: &Value) -> &Vec<Value> {
        page["entries"].as_array().unwrap()
    }

    fn ring_len(log: &TrafficLog) -> usize {
        log.state.lock().unwrap().ring.len()
    }

    /// A scratch directory and the traffic file inside it, private to this test. One log per
    /// test over its own file is what the instantiation buys, so the durability tests run in
    /// parallel without any lock.
    fn fresh_dir() -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("swiss-traffic-{}", swiss_core::util::random_hex(8)));
        let file = dir.join("traffic.jsonl");
        (dir, file)
    }

    /// What a gateway restart looks like for one log: a fresh instance over the same file.
    fn restart(dir: &std::path::Path) -> TrafficLog {
        TrafficLog::at(dir.to_path_buf())
    }

    // --- isolation: the point of the instance -----------------------------------------------

    /// The S2 instantiation this module now lives in (the shape calls.rs finished): one log
    /// per app, so two logs in one process are two rings that never see each other. Before
    /// the change this test could not be written at all — there was one process-global ring
    /// and every caller shared it, which is also why the tests below used to serialize.
    #[test]
    fn two_logs_in_one_process_never_see_each_others_rows() {
        let a = TrafficLog::memory();
        let b = TrafficLog::memory();
        a.record_traffic(
            "db",
            Some(&request("ping", json!({}))),
            Some("ta"),
            true,
            1,
            None,
        );
        b.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("tb"),
            true,
            1,
            None,
        );

        let pa = a.read_traffic(&q());
        let pb = b.read_traffic(&q());
        assert_eq!(pa["total"], 1);
        assert_eq!(pb["total"], 1);
        assert_eq!(pa["entries"][0]["client"], "ta");
        assert_eq!(pb["entries"][0]["client"], "tb");
        // The sequence is per instance as well: both logs start counting at 1.
        assert_eq!(pa["entries"][0]["seq"], 1);
        assert_eq!(pb["entries"][0]["seq"], 1);
    }

    // --- recording ---

    #[test]
    fn records_one_entry_per_json_rpc_message_and_skips_bodies_with_no_method() {
        let log = TrafficLog::memory();
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            None,
            true,
            3,
            None,
        );
        // A batch is several messages in one body...
        let batch = json!([
            request("tools/call", json!({"name":"a"})),
            request("ping", json!({}))
        ]);
        log.record_traffic("db", Some(&batch), None, true, 1, None);
        // ...and a plain JSON-RPC response carries no method at all.
        log.record_traffic(
            "db",
            Some(&json!({"jsonrpc":"2.0","id":1,"result":{}})),
            None,
            true,
            1,
            None,
        );
        log.record_traffic("db", None, None, true, 1, None);

        assert_eq!(ring_len(&log), 3);
        let page = log.read_traffic(&q());
        let methods: Vec<&str> = rows(&page)
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert_eq!(methods, ["ping", "tools/call", "tools/list"]); // newest first
        assert_eq!(page["total"], 3);
    }

    #[test]
    fn redacts_secret_looking_values_in_both_the_params_and_the_stored_body() {
        let log = TrafficLog::memory();
        let body = request(
            "tools/call",
            json!({"name":"t","arguments":{"password":"hunter2","user":"deploy"}}),
        );
        log.record_traffic("db", Some(&body), None, true, 1, None);
        let page = log.read_traffic(&q());
        let params = rows(&page)[0]["params"].as_str().unwrap();
        assert!(!params.contains("hunter2"));
        assert!(params.contains("deploy"));
        let raw = log.read_traffic_entry(1).unwrap();
        assert!(!raw["body"].as_str().unwrap().contains("hunter2"));
    }

    #[test]
    fn omits_params_entirely_when_nothing_is_left_after_lifting_client_info() {
        let log = TrafficLog::memory();
        let init = request(
            "initialize",
            json!({"clientInfo":{"name":"claude-code","version":"2.0"}}),
        );
        log.record_traffic("db", Some(&init), Some("default"), true, 1, None);
        let page = log.read_traffic(&q());
        assert!(rows(&page)[0].get("params").is_none());
        assert_eq!(rows(&page)[0]["clientName"], "claude-code");
        assert_eq!(rows(&page)[0]["clientVersion"], "2.0");
    }

    // server/discover — the capability probe Claude Code sends — nests clientInfo under _meta.
    // Without that path every discover row showed "unknown client" right after the client
    // announced itself.
    #[test]
    fn reads_client_info_out_of_the_discover_meta_shape_too() {
        let log = TrafficLog::memory();
        let discover = request(
            "server/discover",
            json!({"_meta":{"io.modelcontextprotocol/clientInfo":{"name":"claude-code","version":"3.1"}}}),
        );
        log.record_traffic("db", Some(&discover), Some("default"), true, 1, None);
        let page = log.read_traffic(&q());
        assert_eq!(rows(&page)[0]["clientName"], "claude-code");
        assert_eq!(rows(&page)[0]["clientVersion"], "3.1");
    }

    // clientInfo is announced only during the handshake; without remembering it, one client
    // splits into two rows — the frames that carried a name, and all the rest.
    #[test]
    fn remembers_the_name_a_token_announced_and_attributes_its_later_frames() {
        let log = TrafficLog::memory();
        let init = request(
            "initialize",
            json!({"clientInfo":{"name":"claude-code","version":"2.0"}}),
        );
        log.record_traffic("db", Some(&init), Some("laptop"), true, 1, None);
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("other"),
            true,
            1,
            None,
        );

        let page = log.read_traffic(&q());
        assert_eq!(rows(&page)[0]["clientName"], Value::Null); // a different token, still unnamed
        assert_eq!(rows(&page)[1]["clientName"], "claude-code");
        assert_eq!(rows(&page)[1]["clientVersion"], "2.0");
    }

    #[test]
    fn the_ring_never_grows_past_its_bound() {
        let log = TrafficLog::memory();
        for _ in 0..KEEP + 25 {
            log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 0, None);
        }
        assert_eq!(ring_len(&log), KEEP);
        let page = log.read_traffic(&q());
        assert_eq!(page["totalUnfiltered"], KEEP);
        // The sequence keeps counting past what the ring holds.
        assert_eq!(rows(&page)[0]["seq"], (KEEP + 25) as u64);
    }

    // --- the reply summary ---

    #[test]
    fn summarizes_a_reply_from_either_protocol_era() {
        // Modern (2026): one JSON body.
        let modern =
            summarize_response(Some(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#)).unwrap();
        assert!(modern.contains(r#""result""#));
        // Legacy (2025): the message rides an SSE data: line, behind a notification that must not
        // win over the terminal result.
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let legacy = summarize_response(Some(sse)).unwrap();
        assert!(legacy.contains(r#""ok":true"#));
        assert!(!legacy.contains("progress"));
    }

    #[test]
    fn shows_an_unparseable_reply_raw_rather_than_implying_none_was_inspected() {
        assert_eq!(
            summarize_response(Some("<html>502</html>")).unwrap(),
            "<html>502</html>"
        );
        assert_eq!(summarize_response(Some("   ")), None);
        assert_eq!(summarize_response(None), None);
    }

    #[test]
    fn redacts_the_reply_as_well_as_the_request() {
        let out = summarize_response(Some(r#"{"id":1,"result":{"password":"hunter2"}}"#)).unwrap();
        assert!(!out.contains("hunter2"));
    }

    // --- reading ---

    #[test]
    fn a_page_carries_no_payload_but_says_whether_a_reply_was_captured() {
        let log = TrafficLog::memory();
        log.record_traffic(
            "db",
            Some(&request("tools/call", json!({"name":"t"}))),
            None,
            true,
            1,
            Some(r#"{"id":1,"result":{"content":[]}}"#),
        );
        log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 1, None);

        let page = log.read_traffic(&q());
        // Each payload is capped at 8 KB; a polled 200-row answer carrying them would be megabytes.
        for row in rows(&page) {
            assert!(row.get("body").is_none());
            assert!(row.get("response").is_none());
        }
        assert_eq!(rows(&page)[0]["hasResponse"], false);
        assert_eq!(rows(&page)[1]["hasResponse"], true);

        let raw = log.read_traffic_entry(1).unwrap();
        assert!(raw["body"].as_str().unwrap().contains("tools/call"));
        assert!(raw["response"].as_str().unwrap().contains("result"));
        assert!(log.read_traffic_entry(99).is_none());
    }

    #[test]
    fn pages_newest_first_and_reports_whether_older_rows_exist() {
        let log = TrafficLog::memory();
        for i in 0..25 {
            log.record_traffic(
                "db",
                Some(&request(&format!("m{i}"), json!({}))),
                None,
                true,
                0,
                None,
            );
        }
        let first = log.read_traffic(&q());
        assert_eq!(rows(&first).len(), PAGE_SIZE);
        assert_eq!(rows(&first)[0]["method"], "m24");
        assert_eq!(first["more"], true);

        let second = log.read_traffic(&TrafficQuery { page: 1, ..q() });
        assert_eq!(rows(&second).len(), 5);
        assert_eq!(rows(&second)[0]["method"], "m4");
        assert_eq!(second["more"], false);
        assert_eq!(second["total"], 25);
    }

    #[test]
    fn filters_by_mcp_method_and_client_and_reports_both_totals() {
        let log = TrafficLog::memory();
        let init = request("initialize", json!({"clientInfo":{"name":"claude-code"}}));
        log.record_traffic("db", Some(&init), Some("laptop"), true, 1, None);
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        log.record_traffic(
            "cache",
            Some(&request("tools/list", json!({}))),
            Some("ci"),
            true,
            1,
            None,
        );

        let by_mcp = log.read_traffic(&TrafficQuery {
            mcp: Some("cache"),
            ..q()
        });
        assert_eq!(by_mcp["total"], 1);
        assert_eq!(by_mcp["totalUnfiltered"], 3); // the whole ring, not the filtered slice

        let by_method = log.read_traffic(&TrafficQuery {
            method: Some("tools/list"),
            ..q()
        });
        assert_eq!(by_method["total"], 2);

        // The client key is the prefixed one the panel groups by.
        let by_client = log.read_traffic(&TrafficQuery {
            client: Some("n:claude-code"),
            ..q()
        });
        assert_eq!(by_client["total"], 2);
        let by_token = log.read_traffic(&TrafficQuery {
            client: Some("t:ci"),
            ..q()
        });
        assert_eq!(by_token["total"], 1);
    }

    // The panel defaults to actions so a handshake storm does not bury the calls you came to see.
    #[test]
    fn actions_only_keeps_what_a_user_asked_for_and_drops_the_scaffolding() {
        let log = TrafficLog::memory();
        for method in [
            "initialize",
            "tools/list",
            "notifications/initialized",
            "tools/call",
            "resources/read",
        ] {
            log.record_traffic("db", Some(&request(method, json!({}))), None, true, 0, None);
        }
        let page = log.read_traffic(&TrafficQuery {
            actions_only: true,
            ..q()
        });
        let methods: Vec<&str> = rows(&page)
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert_eq!(methods, ["resources/read", "tools/call"]);
        assert_eq!(page["totalUnfiltered"], 5);
    }

    #[test]
    fn clamps_an_absurd_page_size_and_answers_an_empty_page_past_the_end() {
        let log = TrafficLog::memory();
        log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 0, None);
        let huge = log.read_traffic(&TrafficQuery {
            page_size: Some(100_000),
            ..q()
        });
        assert_eq!(huge["pageSize"], KEEP as u64);
        let past = log.read_traffic(&TrafficQuery { page: 9, ..q() });
        assert!(rows(&past).is_empty());
        assert_eq!(past["more"], false);
    }

    // --- the client summary ---

    #[test]
    fn folds_the_whole_ring_into_one_row_per_client_newest_active_first() {
        let log = TrafficLog::memory();
        // A frame that arrives before the handshake carries no name, so it groups under its
        // token; everything from `initialize` onward groups under the announced name. The two
        // rows for one real client are what the token→client memory exists to keep rare.
        log.record_traffic(
            "db",
            Some(&request("ping", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        let init = request(
            "initialize",
            json!({"clientInfo":{"name":"claude-code","version":"2.0"}}),
        );
        log.record_traffic("cache", Some(&init), Some("laptop"), true, 1, None);
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("ci"),
            true,
            1,
            None,
        );

        let clients = log.traffic_clients();
        assert_eq!(clients.len(), 3);
        assert_eq!(clients[0]["key"], "t:ci"); // newest active first
        let named = clients
            .iter()
            .find(|c| c["key"] == "n:claude-code")
            .unwrap();
        assert_eq!(named["label"], "claude-code 2.0");
        // The handshake and the frame after it — the latter attributed from memory, not from a
        // clientInfo it never sent.
        assert_eq!(named["count"], 2);
        assert_eq!(named["tokens"], json!(["laptop"]));
        assert_eq!(named["mcps"], json!(["cache", "db"]));
        let early = clients.iter().find(|c| c["key"] == "t:laptop").unwrap();
        assert_eq!(early["label"], "token laptop");
        assert_eq!(early["count"], 1);
    }

    #[test]
    fn an_unauthenticated_frame_folds_under_the_unknown_client() {
        let log = TrafficLog::memory();
        log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 1, None);
        let clients = log.traffic_clients();
        assert_eq!(clients[0]["key"], "?");
        assert_eq!(clients[0]["label"], "unknown client");
    }

    // --- clearing ---

    #[test]
    fn clearing_one_client_keeps_the_rest_and_clearing_all_resets_the_sequence() {
        let log = TrafficLog::memory();
        let init = request("initialize", json!({"clientInfo":{"name":"claude-code"}}));
        log.record_traffic("db", Some(&init), Some("laptop"), true, 1, None);
        log.record_traffic(
            "db",
            Some(&request("ping", json!({}))),
            Some("ci"),
            true,
            1,
            None,
        );

        log.clear_traffic(Some("n:claude-code"));
        let page = log.read_traffic(&q());
        assert_eq!(page["total"], 1);
        assert_eq!(rows(&page)[0]["client"], "ci");

        log.clear_traffic(None);
        assert_eq!(log.read_traffic(&q())["total"], 0);
        log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 1, None);
        assert_eq!(log.read_traffic(&q())["entries"][0]["seq"], 1);
    }

    // Clear wipes the log, not the gateway memory of which token is which client: forgetting it
    // would make ongoing traffic drop back to "token X" until the client re-initializes.
    #[test]
    fn clearing_does_not_forget_which_client_a_token_belongs_to() {
        let log = TrafficLog::memory();
        let init = request("initialize", json!({"clientInfo":{"name":"claude-code"}}));
        log.record_traffic("db", Some(&init), Some("laptop"), true, 1, None);
        log.clear_traffic(None);
        log.record_traffic(
            "db",
            Some(&request("tools/list", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        assert_eq!(
            log.read_traffic(&q())["entries"][0]["clientName"],
            "claude-code"
        );
    }

    // --- durability ---

    #[tokio::test]
    async fn appends_entries_redacted_and_restores_them_on_restart() {
        let (dir, file) = fresh_dir();
        let log = TrafficLog::at(dir.clone());
        let body = request(
            "tools/call",
            json!({"name":"t","arguments":{"password":"hunter2"}}),
        );
        log.record_traffic("db", Some(&body), Some("laptop"), true, 4, None);
        log.flush_traffic().await;

        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(
            !on_disk.contains("hunter2"),
            "the tail is as redacted as the ring"
        );

        let log = restart(&dir);
        let page = log.read_traffic(&q());
        assert_eq!(page["total"], 1);
        assert_eq!(rows(&page)[0]["method"], "tools/call");
        assert_eq!(rows(&page)[0]["mcp"], "db");
        // The token to client memory rides along, or restored rows would show "token X" until
        // that client happens to send another initialize.
        log.record_traffic(
            "db",
            Some(&request("ping", json!({}))),
            Some("laptop"),
            true,
            1,
            None,
        );
        log.flush_traffic().await;
    }

    #[tokio::test]
    async fn a_restarted_log_continues_the_sequence_above_the_restored_tail() {
        let (dir, file) = fresh_dir();
        let log = TrafficLog::at(dir.clone());
        for i in 0..3 {
            log.record_traffic(
                "db",
                Some(&request(&format!("old{i}"), json!({}))),
                None,
                true,
                0,
                None,
            );
        }
        log.flush_traffic().await;

        // A restart: the fresh instance restores the tail and continues numbering above it —
        // the invariant the old pre-init merge guarded, which construction now gives for free:
        // an instance's sequence starts at the newest seq on disk, never under it.
        let log = restart(&dir);
        log.record_traffic(
            "db",
            Some(&request("pending", json!({}))),
            None,
            true,
            0,
            None,
        );

        let page = log.read_traffic(&q());
        assert_eq!(page["total"], 4);
        assert_eq!(rows(&page)[0]["method"], "pending");
        assert_eq!(rows(&page)[0]["seq"], 4);
        let seqs: Vec<u64> = rows(&page)
            .iter()
            .map(|r| r["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [4, 3, 2, 1]);
        log.flush_traffic().await;
        let _ = std::fs::remove_file(file);
    }

    #[tokio::test]
    async fn clear_rewrites_the_tail_so_a_restart_cannot_resurrect_cleared_rows() {
        let (dir, file) = fresh_dir();
        let log = TrafficLog::at(dir.clone());
        log.record_traffic("db", Some(&request("gone", json!({}))), None, true, 0, None);
        log.record_traffic(
            "db",
            Some(&request("kept", json!({}))),
            Some("ci"),
            true,
            0,
            None,
        );
        log.flush_traffic().await;

        log.clear_traffic(Some("?")); // the unauthenticated row only
        log.flush_traffic().await;

        let log = restart(&dir);
        let page = log.read_traffic(&q());
        assert_eq!(page["total"], 1);
        assert_eq!(rows(&page)[0]["method"], "kept");

        log.clear_traffic(None);
        log.flush_traffic().await;
        let log = restart(&dir);
        assert_eq!(log.read_traffic(&q())["total"], 0);
        let _ = std::fs::remove_file(file);
    }

    #[tokio::test]
    async fn skips_torn_lines_instead_of_failing_the_restore() {
        let (dir, file) = fresh_dir();
        mkdir_private(file.parent().unwrap());
        let good = json!({
            "seq":1,"at":swiss_core::log::iso_now(),"mcp":"db","method":"ping",
            "body":"{}","ok":true,"ms":1,
        });
        // Killed mid-append: half a line, plus a line that parses but describes nothing.
        std::fs::write(
            &file,
            format!("{good}\n{{\"seq\":2,\"at\":\"20\n{{\"seq\":3}}\n"),
        )
        .unwrap();
        let log = restart(&dir);
        let page = log.read_traffic(&q());
        assert_eq!(page["total"], 1);
        assert_eq!(rows(&page)[0]["method"], "ping");
        let _ = std::fs::remove_file(file);
    }

    #[tokio::test]
    async fn a_memory_log_answers_reads_and_writes_nothing_to_disk() {
        // No file is armed, so nothing is ever spawned or written — the ring alone answers
        // reads, and a flush over zero writes returns immediately.
        let log = TrafficLog::memory();
        log.record_traffic("db", Some(&request("ping", json!({}))), None, true, 0, None);
        log.flush_traffic().await;
        assert_eq!(log.read_traffic(&q())["total"], 1);
    }

    #[tokio::test]
    async fn trims_the_tail_to_its_byte_budget_keeping_the_newest_entries() {
        let (dir, file) = fresh_dir();
        let log = TrafficLog::at(dir.clone());
        let pad = "p".repeat(6 * 1024);
        let mut written = 0;
        while written < TRAFFIC_MAX_BYTES + 64 * 1024 {
            let body = request("tools/call", json!({"name":"t","arguments":{"pad":pad}}));
            log.record_traffic("db", Some(&body), None, true, 0, None);
            log.flush_traffic().await;
            written = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            if written == 0 {
                break;
            }
            if written <= 64 * 1024 {
                break; // the trim already fired and reset the budget
            }
        }
        let size = std::fs::metadata(&file).unwrap().len();
        assert!(size <= TRAFFIC_MAX_BYTES, "trimmed to {size} bytes");
        // The file never begins with half an entry.
        let text = std::fs::read_to_string(&file).unwrap();
        let first = text.lines().next().unwrap_or("");
        assert!(
            parse_traffic_entry(first).is_some(),
            "first line: {first:.80}"
        );
        let _ = std::fs::remove_file(file);
    }
}
