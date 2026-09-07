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
//! restored on boot (`init_traffic_log`), so a gateway restart no longer blanks this view while
//! the tool-call log keeps its history. The ring stays what reads serve; the file is a recovery
//! tail, bounded by a byte budget like calls.rs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::mask::is_secret_arg_key;
use crate::platform::{chmod_private, mkdir_private, private_file_mode};
use crate::util::now_ms;

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
    /// None until init_traffic_log arms it: without a file the module behaves exactly as it did
    /// before persistence existed (in-memory ring only — what unit tests get).
    file: Option<PathBuf>,
    bytes: u64,
}

fn state() -> &'static Mutex<TrafficState> {
    static STATE: OnceLock<Mutex<TrafficState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(TrafficState {
            ring: Vec::new(),
            seq: 0,
            known_client: std::collections::HashMap::new(),
            file: None,
            bytes: 0,
        })
    })
}

/// The serialized writer queue (Node chained promises on `trafficQueue`).
fn write_queue() -> &'static Arc<tokio::sync::Mutex<()>> {
    static Q: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    Q.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
}

static LAST_WRITE_ERROR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn on_write_error(where_: &str, err: &str) {
    // A log that cannot be written must never break a request, but silence would be worse.
    let now = now_ms();
    if now.saturating_sub(LAST_WRITE_ERROR.load(std::sync::atomic::Ordering::Relaxed)) < 30_000 {
        return;
    }
    LAST_WRITE_ERROR.store(now, std::sync::atomic::Ordering::Relaxed);
    crate::log::warn(
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

/// Record one JSON-RPC request (or each, when a client sends a batch). No-op for non-MCP bodies.
///
/// `response_text` is the raw bytes written as the HTTP response (a JSON body or SSE data lines),
/// so the log shows the reply, not only the request.
pub fn record_traffic(
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
        if let Ok(mut s) = state().lock() {
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
            "at": crate::log::iso_now(),
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
    if let Ok(mut s) = state().lock() {
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
            persist_traffic(entry);
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

/// The client identity the panel groups rows by — keep in lock-step with `clientKey` in the
/// panel's js/traffic.js.
pub fn client_key_of(e: &TrafficEntry) -> String {
    if e.client_name.is_some() {
        return format!("n:{}", e.client_name.clone().unwrap_or_default());
    }
    if let Some(client) = &e.client {
        return format!("t:{client}");
    }
    "?".into()
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

fn matches(e: &TrafficEntry, q: &TrafficQuery) -> bool {
    if let Some(mcp) = q.mcp {
        if e.mcp != mcp {
            return false;
        }
    }
    if let Some(client) = q.client {
        if client_key_of(e) != client {
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

/// One newest-first page of interactions.
///
/// The rows carry no `body`/`response`: each is capped at 8 KB, so a 200-row answer could reach
/// 3 MB — polled every 6 seconds, to render a dozen visible lines whose raw JSON is hidden until
/// you expand one. The panel fetches a single entry's payload from read_traffic_entry when a row
/// is opened. `hasResponse` survives because the collapsed row says whether a reply was captured.
pub fn read_traffic(q: &TrafficQuery) -> Value {
    let s = state().lock().ok();
    let Some(s) = s else {
        return json!({ "entries": [], "total": 0, "totalUnfiltered": 0, "page": 0, "pageSize": PAGE_SIZE, "more": false });
    };
    let page_size = q.page_size.unwrap_or(PAGE_SIZE).clamp(1, KEEP);
    let filtered = q.mcp.is_some() || q.client.is_some() || q.method.is_some() || q.actions_only;
    let all: Vec<&TrafficEntry> = if filtered {
        s.ring.iter().filter(|e| matches(e, q)).collect()
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
pub fn read_traffic_entry(seq: u64) -> Option<Value> {
    let s = state().lock().ok()?;
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
pub fn traffic_clients() -> Vec<Value> {
    let Ok(s) = state().lock() else {
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
        let key = client_key_of(e);
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
pub fn clear_traffic(client_key: Option<&str>) {
    let armed = {
        let Ok(mut s) = state().lock() else { return };
        match client_key {
            Some(key) => s.ring.retain(|e| client_key_of(e) != key),
            None => {
                s.ring.clear();
                s.seq = 0;
            }
        }
        s.file.clone()
    };
    if armed.is_some() {
        // The file follows the ring: rewritten from whatever survives.
        let queue = write_queue().clone();
        tokio::spawn(async move {
            let _guard = queue.lock().await;
            if let Err(err) = rewrite_traffic_file() {
                on_write_error("clear", &err);
            }
        });
    }
}

// --- durability: the ring survives a restart ---------------------------------------------------

/// File budget: trim the tail once it passes 2 MB, keeping the newest ~1 MB — the same shape and
/// the same numbers as the call log's index, for a log whose entries can reach ~16 KB.
const TRAFFIC_MAX_BYTES: u64 = 2 * 1024 * 1024;
const TRAFFIC_KEEP_BYTES: u64 = 1024 * 1024;

fn traffic_file() -> PathBuf {
    crate::paths::data_path(&["logs", "traffic.jsonl"])
}

/// One disk line back to an entry, skipping torn lines (killed mid-append) rather than failing
/// boot.
fn parse_traffic_entry(line: &str) -> Option<TrafficEntry> {
    let e = serde_json::from_str::<TrafficEntry>(line).ok()?;
    if e.mcp.is_empty() || e.method.is_empty() || e.body.is_empty() {
        return None;
    }
    Some(e)
}

/// Turn durability on and restore the newest KEEP entries from disk.
///
/// Called once by the gateway's entry point before it starts listening, so the first panel poll
/// already sees the pre-restart history. Where the Node build raced this load against live
/// traffic with a pending-list, this port runs it synchronously at boot (std fs, one small read)
/// — nothing can record before the listener exists. Idempotent: a second call is a no-op.
pub fn init_traffic_log() {
    let mut s = match state().lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if s.file.is_some() {
        return;
    }
    s.file = Some(traffic_file());
    let restored: Vec<TrafficEntry> = crate::calls::tail_lines(&traffic_file(), KEEP)
        .ok()
        .map(|tail| {
            tail.lines
                .iter()
                .filter_map(|l| parse_traffic_entry(l))
                .rev()
                .collect()
        })
        .unwrap_or_default();
    s.bytes = std::fs::metadata(traffic_file())
        .map(|m| m.len())
        .unwrap_or(0);
    let pending: Vec<TrafficEntry> = std::mem::take(&mut s.ring);
    let max = restored.iter().map(|e| e.seq).max().unwrap_or(0);
    let mut merged = restored;
    // The token→client memory rides along: without it, restored traffic would show "token X"
    // rows until that client happens to send another initialize.
    for e in &merged {
        if let (Some(client), Some(name)) = (&e.client, &e.client_name) {
            s.known_client.insert(
                client.clone(),
                (Some(name.clone()), e.client_version.clone()),
            );
        }
    }
    let mut next = max;
    for mut e in pending {
        next += 1; // renumber above the restored tail — never a collision
        e.seq = next;
        merged.push(e);
    }
    s.seq = next;
    if merged.len() > KEEP {
        let cut = merged.len() - KEEP;
        merged.drain(..cut);
    }
    s.ring = merged;
}

/// Append one entry. Serialized so lines cannot interleave; never awaited by the caller —
/// recording traffic must not sit on the hot path of a request.
fn persist_traffic(entry: TrafficEntry) {
    let queue = write_queue().clone();
    tokio::spawn(async move {
        let _guard = queue.lock().await;
        if let Err(err) = append_traffic(&entry) {
            on_write_error("append", &err);
        }
    });
}

fn append_traffic(entry: &TrafficEntry) -> Result<(), String> {
    use std::io::Write;
    let file = traffic_file();
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
    let mut s = state().lock().map_err(|_| "state poisoned".to_string())?;
    s.bytes += line.len() as u64;
    let over = s.bytes > TRAFFIC_MAX_BYTES;
    drop(s);
    if over {
        trim_traffic_log()?;
    }
    Ok(())
}

fn rewrite_traffic_file() -> Result<(), String> {
    let lines = {
        let s = state().lock().map_err(|_| "state poisoned".to_string())?;
        s.ring
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap_or_default()))
            .collect::<String>()
    };
    crate::atomic_json::write_text_atomic(&traffic_file(), &lines)?;
    if let Ok(mut s) = state().lock() {
        s.bytes = lines.len() as u64;
    }
    Ok(())
}

/// Drop the oldest lines once the tail outgrows its budget — the call log's trim, with one fix:
/// the read handle is closed BEFORE the tmp→rename swap, because Windows refuses to replace a
/// file that is still open.
fn trim_traffic_log() -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let file = traffic_file();
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
    crate::atomic_json::write_text_atomic(&file, &String::from_utf8_lossy(kept))?;
    let kept_len = kept.len() as u64;
    if let Ok(mut s) = state().lock() {
        s.bytes = kept_len;
    }
    crate::log::log(
        "info",
        "traffic log trimmed",
        Some(json!({ "keptBytes": kept_len })),
    );
    Ok(())
}

/// Wait for every pending write (the shutdown path and tests).
pub async fn flush_traffic() {
    let _guard = write_queue().lock().await;
}
