//! Per-MCP log of tool invocations — port of `calls.ts`.
//!
//! Captured at the tool-call handler — the one point every path crosses (an MCP client over HTTP,
//! and the panel's Run button) — so the log holds exactly the text the caller received, not a
//! reconstruction of it. Child stderr (`Adapter::logs`) is a different thing entirely: it exists
//! only for proc MCPs and says nothing about what was asked or answered.
//!
//! The log lives on DISK and survives a restart, which is when you most want to know what the
//! last calls were. Nothing accumulates in the heap.
//!
//! Two layers, because an index and a payload want opposite things:
//! - `<mcp>.jsonl` — one line per call carrying the metadata and the first PREVIEW_MAX characters
//!   of the reply. Small lines keep browsing cheap: a page is one short read from the END of the
//!   file, no matter how much history is behind it.
//! - `bodies/<mcp>/<seq>.txt` — the complete reply, written only when it exceeds the preview.
//!   Fetched by seq when someone actually asks for it. The newest BODY_KEEP are retained per MCP.
//!
//! Where the Node build carried `via`/`client` across the SDK in an AsyncLocalStorage, this port
//! uses a tokio task-local: the router scopes it around the request, every `.await` inside the
//! handler (including rmcp's) inherits it, and a handler running outside any scope reads the
//! "mcp" default — the same `?? "mcp"` fallback.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;
use tokio::task_local;

use crate::log;
use crate::mask::is_secret_arg_key;
use crate::platform::{chmod_private, mkdir_private, private_file_mode};
use crate::util::{now_ms, parse_iso_ms};

#[derive(serde::Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CallEntry {
    /// Monotonic per-MCP sequence, recovered from the file across restarts.
    pub seq: u64,
    pub at: String,
    pub tool: String,
    /// "mcp" for a client on the HTTP endpoint, "panel" for the dashboard's Run/Try button.
    pub via: String,
    /// The token label that authenticated the request, when via is "mcp" (absent for panel calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    pub ok: bool,
    pub ms: u64,
    /// Arguments as JSON, secret-looking values redacted.
    pub args: String,
    /// The reply (or the error message): the preview in a page, the whole thing from read_call().
    pub output: String,
    /// Length of the full reply, even when `output` here is only the preview.
    pub chars: usize,
    /// True when `output` is only the head of the reply — the UI offers to fetch the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<bool>,
    /// True when the complete reply is on disk and can still be fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<bool>,
    /// Set by read_call when the reply was written but has since been pruned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_gone: Option<bool>,
}

const ARGS_MAX: usize = 4 * 1024;
/// Held inline in the index line, so a page needs no extra reads.
const PREVIEW_MAX: usize = 2 * 1024;
/// Ceiling on a stored reply. renderResult already caps DB results at 256 KB; a proc child is
/// free to return anything, and one reply must not become the biggest file in the project.
const BODY_MAX: usize = 1024 * 1024;
/// Complete replies kept per MCP. Older calls keep their metadata and preview, not their payload.
const BODY_KEEP: usize = 50;
/// Index budget per MCP: trimmed to the newest KEEP_BYTES once exceeded (~2 KB per line).
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const KEEP_BYTES: u64 = 1024 * 1024;
/// Retention. The byte budget alone is not a retention policy: it only bites on an MCP that is
/// BUSY. A rarely-used one sits under the budget forever, so its log keeps arguments and replies
/// from years ago that nobody asked to keep. Age is the second bound, and the two are
/// independent — a busy MCP is cut by bytes, a quiet one by age.
const MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;
/// Sweep at most this often per MCP. Age only matters at day resolution; checking on every
/// append would read the whole index on the hot path for a boundary that moves once a day.
const SWEEP_EVERY_MS: u64 = 60 * 60 * 1000;
pub const CALLS_PAGE_SIZE: usize = 20;
pub const TOOL_HISTORY_MAX: usize = 300;

const REDACTED: &str = "•••";

static LOG_DIR: OnceLock<Mutex<PathBuf>> = OnceLock::new();

fn log_dir() -> PathBuf {
    LOG_DIR
        .get_or_init(|| Mutex::new(crate::paths::data_path(&["logs", "calls"])))
        .lock()
        .map(|d| d.clone())
        .unwrap_or_else(|_| crate::paths::data_path(&["logs", "calls"]))
}

/// Point the log somewhere else (tests use a temp directory).
pub fn set_call_log_dir(dir: PathBuf) {
    if let Ok(mut holder) = LOG_DIR.get_or_init(|| Mutex::new(dir.clone())).lock() {
        *holder = dir;
    }
    if let Ok(mut m) = files().lock() {
        m.clear();
    }
}

fn file_for(mcp: &str) -> PathBuf {
    log_dir().join(format!("{mcp}.jsonl"))
}
/// A directory per MCP, so one name can never be mistaken for the prefix of another.
fn body_dir(mcp: &str) -> PathBuf {
    log_dir().join("bodies").join(mcp)
}
fn body_file(mcp: &str, seq: u64) -> PathBuf {
    body_dir(mcp).join(format!("{seq}.txt"))
}

/// Per-file write state: next sequence, current size, and the queue that serializes appends.
struct FileState {
    seq: u64,
    bytes: u64,
    /// When the age sweep last ran for this MCP; 0 means "not since boot", so the first append
    /// sweeps.
    swept_at: u64,
    /// Serializes every writer against this file (Node chained promises on `queue`).
    queue: Arc<tokio::sync::Mutex<()>>,
    /// Appends `record_call` has registered, counted BEFORE it spawns the writer.
    registered: u64,
    /// Appends that have reached the file. A reader waits for this to catch up with `registered`
    /// (see `flush_calls`): taking the queue is not enough on its own, because the spawned writer
    /// has not taken it yet when the reader arrives. The Node build got this ordering for free —
    /// it chained onto the queue promise inline, with nothing to spawn.
    completed: tokio::sync::watch::Sender<u64>,
}

fn new_file_state(seq: u64, bytes: u64) -> FileState {
    FileState {
        seq,
        bytes,
        swept_at: 0,
        queue: Arc::new(tokio::sync::Mutex::new(())),
        registered: 0,
        completed: tokio::sync::watch::Sender::new(0),
    }
}

/// Counts one append as done however the writer ends — including an early return, so a reader
/// waiting on it can never be left hanging.
struct AppendDone(Arc<Mutex<FileState>>);

impl Drop for AppendDone {
    fn drop(&mut self) {
        if let Ok(s) = self.0.lock() {
            let next = *s.completed.borrow() + 1;
            s.completed.send_replace(next);
        }
    }
}

fn files() -> &'static Mutex<HashMap<String, Arc<Mutex<FileState>>>> {
    static FILES: OnceLock<Mutex<HashMap<String, Arc<Mutex<FileState>>>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(HashMap::new()))
}

static LAST_ERROR_LOG: AtomicU64 = AtomicU64::new(0);

fn on_error(where_: &str, err: &str) {
    // A log that cannot be written must never break a tool call, but silence would be worse.
    let now = now_ms();
    if now.saturating_sub(LAST_ERROR_LOG.load(Ordering::Relaxed)) < 30_000 {
        return;
    }
    LAST_ERROR_LOG.store(now, Ordering::Relaxed);
    log::warn(
        "call log write failed",
        Some(serde_json::json!({ "where": where_, "err": err })),
    );
}

/// Old name -> current name, for calls already in flight when a rename happened.
///
/// `record_call` fires without being awaited, so a rename landing between capture and append
/// would have written a fresh `<oldname>.jsonl` moments after the file was moved, stranding the
/// entry in a file nothing ever reads again. Cleared by forget_call_alias when the freed name is
/// registered again, so a NEW MCP reusing an old name never inherits the redirect.
fn aliases() -> &'static Mutex<HashMap<String, String>> {
    static ALIASES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ALIASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn current_name(mcp: &str) -> String {
    let Ok(map) = aliases().lock() else {
        return mcp.to_string();
    };
    let mut name = mcp.to_string();
    // Bounded walk: a chain of renames is short, and a cycle must not hang an append.
    for _ in 0..8 {
        match map.get(&name) {
            Some(next) => name = next.clone(),
            None => break,
        }
    }
    name
}

/// Drop a redirect, because this name now belongs to a different MCP.
pub fn forget_call_alias(name: &str) {
    if let Ok(mut map) = aliases().lock() {
        map.remove(name);
    }
}

/// The state for one MCP's log, creating it (and recovering seq/size from disk) on first touch.
fn state(mcp: &str) -> Arc<Mutex<FileState>> {
    // Fast path: already known. The lock discipline here is a plain double-checked pattern —
    // creating the entry twice would only re-recover the same numbers from disk.
    if let Ok(map) = files().lock() {
        if let Some(s) = map.get(mcp) {
            return s.clone();
        }
    }
    let fresh = Mutex::new(new_file_state(0, 0));
    // Recover the sequence and size from whatever is already on disk, once per MCP per boot.
    if let Ok(meta) = std::fs::metadata(file_for(mcp)) {
        if let Ok(mut s) = fresh.lock() {
            s.bytes = meta.len();
        }
        if let Ok(tail) = tail_lines(&file_for(mcp), 1) {
            if let Some(first) = tail.lines.first() {
                if let Ok(entry) = serde_json::from_str::<CallEntry>(first) {
                    if let Ok(mut s) = fresh.lock() {
                        s.seq = entry.seq;
                    }
                }
            }
        }
    }
    let arc = Arc::new(fresh);
    if let Ok(mut map) = files().lock() {
        map.entry(mcp.to_string()).or_insert_with(|| arc.clone());
    }
    arc
}

/// Clip at a CHAR boundary (JS sliced UTF-16 units; a byte slice through a multi-byte character
/// would panic), with the same "+N more characters" marker.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let rest = text.chars().count() - max;
    format!("{head}\n… +{rest} more characters")
}

/// Recursively redact values whose OBJECT KEY looks like a credential — the replacer the Node
/// build passed to JSON.stringify (`SECRET_ARG_KEY_RE`), ported as a tree walk.
fn redact_args(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if !k.is_empty() && is_secret_arg_key(k) {
                        (k.clone(), Value::String(REDACTED.into()))
                    } else {
                        (k.clone(), redact_args(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_args).collect()),
        other => other.clone(),
    }
}

fn args_text(args: Option<&Value>) -> String {
    let Some(args) = args else {
        return String::new();
    };
    if args.is_null() {
        return String::new();
    }
    let redacted = redact_args(args);
    match serde_json::to_string(&redacted) {
        Ok(json) => clip(&json, ARGS_MAX),
        Err(_) => "[arguments could not be serialized]".into(),
    }
}

/// Flatten a CallToolResult's content blocks into the text a caller actually sees. Takes the
/// serialized result (rmcp's wire shape is the SDK's: `{content: [{type, text}...]}`).
pub fn content_text(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    content
        .iter()
        .map(|c| match c.get("type").and_then(Value::as_str) {
            Some("text") => c
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            Some(other) => format!("[{other} content]"),
            None => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The record one tool call leaves behind. `output` is the rendered result text, or the error
/// message when `ok` is false.
pub struct CallRecord {
    pub tool: String,
    pub args: Option<Value>,
    pub ok: bool,
    pub ms: u64,
    pub output: String,
}

task_local! {
    /// Which entry point a call came in through — scoped by the router around the request
    /// (AsyncLocalStorage in the Node build; there is nowhere to carry it as a parameter across
    /// the MCP SDK).
    static CALL_VIA: String;
    /// The token label that authenticated this request — set at the bearer gate for client
    /// calls, absent for panel calls (which have no token).
    static CALL_CLIENT: Option<String>;
}

/// Who is making this call: the entry point ("mcp" for a client on the HTTP endpoint, "panel"
/// for the dashboard's Run/Try button) and the authenticating token's label.
///
/// rmcp drives a ServerHandler from its own spawned task, where a task-local cannot reach — so
/// the context is captured where it IS known (the request task, at server-factory time: both
/// the HTTP service and the panel's in-memory session build the server inside the scoped task)
/// and carried INTO the server instance. This struct is that carrier.
#[derive(Debug, Clone)]
pub struct CallSource {
    pub via: String,
    pub client: Option<String>,
}

impl Default for CallSource {
    fn default() -> Self {
        Self {
            via: "mcp".to_string(),
            client: None,
        }
    }
}

/// Capture the current context — called at server-factory time (see `CallSource`).
pub fn current_source() -> CallSource {
    CallSource {
        via: CALL_VIA
            .try_with(|v| v.clone())
            .unwrap_or_else(|_| "mcp".to_string()),
        client: CALL_CLIENT.try_with(|c| c.clone()).unwrap_or(None),
    }
}

/// Run `f` with every server it builds attributed to the panel's Run button.
pub async fn with_call_source_panel<F: std::future::Future<Output = T>, T>(f: F) -> T {
    CALL_VIA
        .scope("panel".to_string(), CALL_CLIENT.scope(None, f))
        .await
}

/// Run `f` with every server it builds attributed to an MCP client's token label.
pub async fn with_call_client_mcp<F: std::future::Future<Output = T>, T>(
    client: String,
    f: F,
) -> T {
    CALL_VIA
        .scope("mcp".to_string(), CALL_CLIENT.scope(Some(client), f))
        .await
}

/// Append one call to an MCP's log.
///
/// Deliberately not awaited by the caller: a tool call must not wait on a disk write. Appends for
/// one MCP are queued so lines cannot interleave, and readers flush that queue first, so a call
/// is always visible to the very next read.
pub fn record_call(raw_mcp: Option<&str>, source: &CallSource, rec: CallRecord) {
    let Some(raw_mcp) = raw_mcp else { return }; // a nameless adapter (tests) simply isn't logged
    let mcp = current_name(raw_mcp);
    let state = state(&mcp);
    let output = if rec.output.chars().count() > BODY_MAX {
        clip(&rec.output, BODY_MAX)
    } else {
        rec.output.clone()
    };
    let long = output.chars().count() > PREVIEW_MAX;
    let entry = CallEntry {
        seq: 0, // assigned inside the queue, so concurrent calls cannot claim the same number
        at: crate::log::iso_now(),
        tool: rec.tool,
        via: source.via.clone(),
        client: source.client.clone(),
        ok: rec.ok,
        ms: rec.ms,
        args: args_text(rec.args.as_ref()),
        output: if long {
            output.chars().take(PREVIEW_MAX).collect()
        } else {
            output.clone()
        },
        chars: rec.output.chars().count(),
        preview: long.then_some(true),
        body: long.then_some(true),
        body_gone: None,
    };
    let mcp_for_task = mcp.clone();
    // Count the append BEFORE spawning. A reader that flushes in the window between here and the
    // writer's first poll would otherwise find the queue free, take it, and serve a page missing
    // the call that had just been made.
    {
        let Ok(mut s) = state.lock() else { return };
        s.registered += 1;
    }
    tokio::spawn(async move {
        let _done = AppendDone(state.clone());
        let queue = {
            let Ok(s) = state.lock() else { return };
            s.queue.clone()
        };
        let _guard = queue.lock().await;
        let mut entry = entry;
        let result = append_one(&mcp_for_task, &state, &mut entry, &output, long).await;
        if let Err(err) = result {
            on_error("append", &err);
        }
    });
}

async fn append_one(
    mcp: &str,
    state: &Arc<Mutex<FileState>>,
    entry: &mut CallEntry,
    output: &str,
    long: bool,
) -> Result<(), String> {
    {
        let Ok(mut s) = state.lock() else {
            return Err("state poisoned".into());
        };
        s.seq += 1;
        entry.seq = s.seq;
    }
    mkdir_private(&log_dir());
    if long {
        mkdir_private(&body_dir(mcp));
        std::fs::write(body_file(mcp, entry.seq), output.as_bytes()).map_err(|e| e.to_string())?;
        chmod_private(&body_file(mcp, entry.seq), private_file_mode());
        // Keep the newest BODY_KEEP payloads; the index keeps every call either way.
        keep_newest_bodies(mcp)?;
    }
    let line = format!(
        "{}\n",
        serde_json::to_string(entry).map_err(|e| e.to_string())?
    );
    append_bytes(&file_for(mcp), line.as_bytes())?;
    chmod_private(&file_for(mcp), private_file_mode());
    let bytes = line.len() as u64;
    let (total, swept_at) = {
        let Ok(mut s) = state.lock() else {
            return Ok(());
        };
        s.bytes += bytes;
        (s.bytes, s.swept_at)
    };
    if total > MAX_BYTES {
        trim(mcp, state)?;
    }
    // Inside the queue, so a sweep can never race an append into a half-written file.
    if now_ms().saturating_sub(swept_at) > SWEEP_EVERY_MS {
        sweep_age(mcp, state)?;
    }
    Ok(())
}

/// Raw append (Node's appendFile): no atomicity needed — one line is one `write`, and a torn line
/// is dropped by the readers.
fn append_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

/// Keep the newest BODY_KEEP stored payloads for one MCP, dropping the rest.
///
/// By directory listing rather than by arithmetic on the sequence. A body is written only for a
/// reply that outgrew the preview, so the numbered files are SPARSE: deleting `seq - BODY_KEEP`
/// by name would usually target a file that never existed while every genuinely old payload — up
/// to BODY_MAX each — stayed on disk for the life of the data directory. Called after each body
/// write, so the directory settles at BODY_KEEP files; the listing it walks is that same size,
/// which is why doing it every time is cheap.
fn keep_newest_bodies(mcp: &str) -> Result<(), String> {
    let names = match std::fs::read_dir(body_dir(mcp)) {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // nothing stored yet
    };
    let mut seqs: Vec<u64> = names
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let stem = name.strip_suffix(".txt")?;
            stem.parse::<u64>().ok()
        })
        .collect();
    if seqs.len() <= BODY_KEEP {
        return Ok(());
    }
    seqs.sort_unstable();
    for seq in &seqs[..seqs.len() - BODY_KEEP] {
        let _ = std::fs::remove_file(body_file(mcp, *seq));
    }
    Ok(())
}

/// Drop the oldest history once the file outgrows its budget, keeping the newest KEEP_BYTES.
/// The read handle is fully dropped BEFORE the tmp→rename swap: Windows refuses to replace a
/// file that is still open.
fn trim(mcp: &str, state: &Arc<Mutex<FileState>>) -> Result<(), String> {
    let path = file_for(mcp);
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let start = size.saturating_sub(KEEP_BYTES) as usize;
    let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; size as usize - start];
    file.seek(SeekFrom::Start(start as u64))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut buf).map_err(|e| e.to_string())?;
    drop(file);
    // Start at the first line boundary, so the file never begins with half an entry.
    let kept: &[u8] = match buf.iter().position(|b| *b == b'\n') {
        Some(nl) => &buf[nl + 1..],
        None => &[],
    };
    crate::atomic_json::write_text_atomic(&path, &String::from_utf8_lossy(kept))?;
    let kept_len = kept.len() as u64;
    if let Ok(mut s) = state.lock() {
        s.bytes = kept_len;
    }
    log::log(
        "info",
        "call log trimmed",
        Some(serde_json::json!({ "mcp": mcp, "keptBytes": kept_len })),
    );
    Ok(())
}

/// Drop entries older than MAX_AGE_MS, and the payload files that belonged to them.
///
/// The index is append-only and chronological, so everything expired is a prefix of the file:
/// find the first line still in range and keep the rest. Reading the whole index is affordable
/// precisely because MAX_BYTES caps it at 2 MB, and this runs at most hourly per MCP, from inside
/// the write queue — never on a read.
///
/// A torn line (killed mid-append) has no usable date; it is dropped with the expired prefix
/// rather than treated as current, which is the same thing the readers do to it.
fn sweep_age(mcp: &str, state: &Arc<Mutex<FileState>>) -> Result<(), String> {
    if let Ok(mut s) = state.lock() {
        s.swept_at = now_ms();
    }
    let path = file_for(mcp);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(()), // no file yet
    };
    let cutoff = now_ms() as i64 - MAX_AGE_MS;
    let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
    let mut keep_from = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let at = serde_json::from_str::<CallEntry>(line)
            .ok()
            .and_then(|e| parse_iso_ms(&e.at));
        match at {
            Some(t) if t >= cutoff => {
                keep_from = i;
                break;
            }
            _ => continue, // torn or unparseable: leave it in the expired prefix
        }
    }
    if keep_from == 0 {
        return Ok(()); // nothing has expired
    }
    let kept: Vec<&str> = lines[keep_from..].to_vec();
    let out = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    crate::atomic_json::write_text_atomic(&path, &out)?;
    if let Ok(mut s) = state.lock() {
        s.bytes = out.len() as u64;
    }

    // The payloads of the dropped entries are now unreachable — read_call finds no index line for
    // them, so they would sit on disk forever. BODY_KEEP only prunes while an MCP is still being
    // called.
    let oldest_kept: u64 = kept
        .first()
        .and_then(|l| serde_json::from_str::<CallEntry>(l).ok())
        .map(|e| e.seq)
        .unwrap_or(u64::MAX);
    prune_bodies(mcp, oldest_kept)?;
    log::log(
        "info",
        "call log aged out",
        Some(serde_json::json!({ "mcp": mcp, "dropped": keep_from, "kept": kept.len() })),
    );
    Ok(())
}

/// Remove payload files for entries no longer in the index.
fn prune_bodies(mcp: &str, oldest_kept_seq: u64) -> Result<(), String> {
    let names = match std::fs::read_dir(body_dir(mcp)) {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // no payloads were ever written for this MCP
    };
    for entry in names.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(seq) = name
            .strip_suffix(".txt")
            .and_then(|s| s.parse::<u64>().ok())
        {
            if seq < oldest_kept_seq {
                let _ = std::fs::remove_file(body_dir(mcp).join(name.as_ref()));
            }
        }
    }
    Ok(())
}

/// One retention pass over every log on disk. Awaitable, so a test does not have to guess. The
/// caller runs this on boot plus an hourly timer: the per-append sweep alone leaves exactly the
/// logs retention is FOR — an MCP that stops being called never appends again, so its history
/// would sit there past the cutoff forever. Walks the DIRECTORY rather than the live state map,
/// so it also reaches logs belonging to MCPs that are no longer registered at all.
pub async fn sweep_call_logs() {
    let names = match std::fs::read_dir(log_dir()) {
        Ok(rd) => rd,
        Err(_) => return, // nothing logged yet
    };
    let mut states: Vec<(String, Arc<Mutex<FileState>>)> = Vec::new();
    for entry in names.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(mcp) = name.strip_suffix(".jsonl") else {
            continue;
        };
        states.push((mcp.to_string(), state(mcp)));
    }
    for (mcp, st) in states {
        // Through the queue, like every other writer, so a sweep cannot land mid-append.
        let queue = st.lock().ok().map(|s| s.queue.clone());
        let Some(queue) = queue else { continue };
        let _guard = queue.lock().await;
        if let Err(err) = sweep_age(&mcp, &st) {
            on_error("sweep", &err);
        }
    }
}

/// Wait for pending appends (one MCP, or all) to reach the file.
pub async fn flush_calls(mcp: Option<&str>) {
    let states: Vec<Arc<Mutex<FileState>>> = match mcp {
        Some(name) => vec![state(name)],
        None => files()
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default(),
    };
    for st in states {
        // Wait for every append registered so far to reach the file...
        let waited = st
            .lock()
            .ok()
            .map(|s| (s.registered, s.completed.subscribe()));
        if let Some((want, mut done)) = waited {
            while *done.borrow_and_update() < want {
                if done.changed().await.is_err() {
                    break;
                }
            }
        }
        // ...then take the queue, so a caller that flushes in order to rewrite the file (a sweep,
        // a trim) still cannot land mid-append.
        let queue = st.lock().ok().map(|s| s.queue.clone());
        let Some(queue) = queue else { continue };
        let _guard = queue.lock().await;
    }
}

/// Read the last `want` complete lines of a file, newest first.
///
/// Reads backwards in chunks so paging the newest calls costs one small read no matter how long
/// the history is. Splitting on 0x0A is safe for UTF-8 (a newline byte cannot occur inside a
/// multi-byte sequence), so chunk boundaries never corrupt a character.
pub struct Tail {
    pub lines: Vec<String>,
    pub more: bool,
}

pub fn tail_lines(path: &std::path::Path, want: usize) -> Result<Tail, String> {
    const CHUNK: usize = 64 * 1024;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return Ok(Tail {
                lines: Vec::new(),
                more: false,
            })
        }
    };
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let mut pos = size;
    let mut acc: Vec<u8> = Vec::new();
    let mut at_start = false;
    loop {
        if pos == 0 {
            at_start = true;
            break;
        }
        let len = (CHUNK as u64).min(pos) as usize;
        pos -= len as u64;
        let mut buf = vec![0u8; len];
        file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        acc.splice(..0, buf);
        // Count complete lines currently held; the first partial line is dropped unless we hit BOF.
        let complete = acc.iter().filter(|b| **b == b'\n').count();
        if complete > want {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc).into_owned();
    let text = if !at_start {
        match text.find('\n') {
            Some(nl) => text[nl + 1..].to_string(),
            None => String::new(),
        }
    } else {
        text
    };
    let all: Vec<String> = text
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let keep = all.len().min(want);
    let lines: Vec<String> = all[all.len() - keep..].iter().rev().cloned().collect();
    Ok(Tail {
        lines,
        more: !at_start || all.len() > want,
    })
}

fn parse_lines(lines: &[String]) -> Vec<CallEntry> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<CallEntry>(line).ok())
        .collect()
    // A torn line (killed mid-append) is skipped rather than failing the page.
}

/// One page of an MCP's log, newest first. Each entry carries the preview already stored in the
/// index.
pub async fn read_calls(mcp: &str, page: i64, page_size: i64) -> Value {
    flush_calls(Some(mcp)).await;
    let size = page_size.clamp(1, 100) as usize;
    let page = page.max(0) as usize;
    let from = page * size;
    let tail = tail_lines(&file_for(mcp), from + size + 1).unwrap_or(Tail {
        lines: Vec::new(),
        more: false,
    });
    let entries = parse_lines(&tail.lines);
    let slice: Vec<&CallEntry> = entries.iter().skip(from).take(size).collect();
    let calls: Vec<Value> = slice
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    serde_json::json!({
        "calls": calls,
        "page": page,
        "pageSize": size,
        "more": entries.len() > from + size || tail.more,
    })
}

/// One entry with its reply in full — what the panel's "Show full result" button fetches.
///
/// Scans the index (bounded at MAX_BYTES, and this runs on a click rather than on a poll) for the
/// metadata, then reads the payload beside it. A reply whose payload has been pruned comes back
/// as the preview, flagged, rather than as a silently short result.
pub async fn read_call(mcp: &str, seq: u64) -> Option<CallEntry> {
    flush_calls(Some(mcp)).await;
    let tail = tail_lines(&file_for(mcp), usize::MAX).ok()?;
    let entry = parse_lines(&tail.lines)
        .into_iter()
        .find(|e| e.seq == seq)?;
    if entry.body != Some(true) {
        return Some(entry);
    }
    match std::fs::read_to_string(body_file(mcp, seq)) {
        Ok(output) => {
            let mut full = entry.clone();
            full.output = output;
            full.preview = None;
            Some(full)
        }
        Err(_) => {
            let mut gone = entry.clone();
            gone.body_gone = Some(true);
            Some(gone)
        }
    }
}

/// The newest DISTINCT runs of ONE tool, newest first — the Run tab's refill dropdown.
///
/// Runs whose arguments are identical are collapsed to one entry — the newest occurrence —
/// because the dropdown answers "what was last executed", not "how often it was executed".
/// Distinctness is decided on the FULL stored arguments, never on the clipped preview: two long
/// argument sets sharing a 96-character prefix are different runs and must not merge. The limit
/// counts DISTINCT entries. `q`, when given, keeps only the runs whose FULL stored arguments
/// contain it (case-insensitive) — the match is deliberately NOT on the clipped preview.
pub async fn read_tool_history(mcp: &str, tool: &str, limit: usize, q: Option<&str>) -> Vec<Value> {
    flush_calls(Some(mcp)).await;
    let size = limit.clamp(1, TOOL_HISTORY_MAX);
    let needle = q.unwrap_or("").trim().to_ascii_lowercase();
    let Ok(tail) = tail_lines(&file_for(mcp), usize::MAX) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in parse_lines(&tail.lines) {
        if e.tool != tool {
            continue;
        }
        if !needle.is_empty() && !e.args.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        if !seen.insert(e.args.clone()) {
            continue; // same arguments → same dropdown entry; the newest is already in
        }
        let mut row = serde_json::json!({
            "seq": e.seq,
            "at": e.at,
            "via": e.via,
            "ok": e.ok,
            "ms": e.ms,
            "args": one_line(&e.args),
        });
        if let Some(client) = e.client {
            row["client"] = serde_json::json!(client);
        }
        out.push(row);
        if out.len() >= size {
            break;
        }
    }
    out
}

/// Fold whitespace and clip, so a stored multi-line args blob still reads as one label line.
fn one_line(text: &str) -> String {
    let folded: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = 96;
    if folded.chars().count() <= max {
        return folded;
    }
    let head: String = folded.chars().take(max).collect();
    format!("{head}…")
}

/// Forget an MCP's history (the panel's Clear button, and deleting an MCP).
pub async fn clear_calls(mcp: &str) {
    flush_calls(Some(mcp)).await;
    if let Ok(mut map) = files().lock() {
        map.remove(mcp);
    }
    let _ = std::fs::remove_file(file_for(mcp));
    let _ = std::fs::remove_dir_all(body_dir(mcp));
}

/// Follow a rename, so the history isn't stranded under a name that no longer exists.
pub async fn rename_calls(from: &str, to: &str) {
    flush_calls(Some(from)).await;
    let carried = files().lock().ok().and_then(|mut m| {
        m.remove(from).and_then(|s| {
            let (seq, bytes) = s.lock().ok().map(|st| (st.seq, st.bytes))?;
            // Fresh append accounting: the flush above already drained everything registered
            // against the old name.
            let arc = Arc::new(Mutex::new(new_file_state(seq, bytes)));
            m.insert(to.to_string(), arc.clone());
            Some(())
        })
    });
    if let Ok(mut map) = aliases().lock() {
        map.insert(from.to_string(), to.to_string());
        // Re-point anything that already pointed at `from`, so a second rename doesn't leave a
        // stale hop.
        let stale: Vec<String> = map
            .iter()
            .filter(|(old, target)| *target == from && *old != from)
            .map(|(old, _)| old.clone())
            .collect();
        for old in stale {
            map.insert(old, to.to_string());
        }
    }
    if std::fs::rename(file_for(from), file_for(to)).is_ok() && carried.is_none() {
        // The state for `to` was created above only when there WAS state to carry; a file that
        // appears without one recovers its seq on first read like a fresh boot.
    }
    let _ = std::fs::rename(body_dir(from), body_dir(to)); // no stored payloads is fine
}

/// Wrap a tool-call handler so every invocation lands in the log. `source` says who made the
/// call (carried by the server instance, see `CallSource`). `present` turns the result into the
/// logged text and says whether it counts as an error — a tool that reports failure in-band
/// (`isError: true`) never throws, and logging that as a success would make the log lie.
pub async fn logged<T, E: std::fmt::Display, F, P>(
    mcp: Option<&str>,
    source: &CallSource,
    tool: &str,
    args: Option<Value>,
    run: F,
    present: P,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
    P: FnOnce(&T) -> (bool, String),
{
    let t0 = std::time::Instant::now();
    match run.await {
        Ok(result) => {
            let (ok, output) = present(&result);
            record_call(
                mcp,
                source,
                CallRecord {
                    tool: tool.to_string(),
                    args,
                    ok,
                    ms: t0.elapsed().as_millis() as u64,
                    output,
                },
            );
            Ok(result)
        }
        Err(err) => {
            let message = err.to_string();
            record_call(
                mcp,
                source,
                CallRecord {
                    tool: tool.to_string(),
                    args,
                    ok: false,
                    ms: t0.elapsed().as_millis() as u64,
                    output: message,
                },
            );
            Err(err)
        }
    }
}

/// Drop one MCP in-memory write state without touching its files — what a gateway restart looks
/// like for a single log. Test-only seam: the production path drops state per directory switch.
#[cfg(test)]
pub(crate) fn forget_file_state(mcp: &str) {
    if let Ok(mut map) = files().lock() {
        map.remove(mcp);
    }
}

/// One scratch log directory for the whole test binary, installed once. Every test uses its own
/// MCP names inside it, so nothing has to move the directory out from under a parallel test.
#[cfg(test)]
pub(crate) fn test_log_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("lmg-calls-test-{}", crate::util::random_hex(8)));
        set_call_log_dir(dir.clone());
        dir
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> CallSource {
        CallSource::default()
    }

    fn rec(tool: &str, args: Option<Value>, ok: bool, output: &str) -> CallRecord {
        CallRecord {
            tool: tool.to_string(),
            args,
            ok,
            ms: 1,
            output: output.to_string(),
        }
    }

    /// Every read flushes pending appends, so reading is enough to see a call just recorded.
    async fn entries(mcp: &str) -> Vec<CallEntry> {
        let page = read_calls(mcp, 0, CALLS_PAGE_SIZE as i64).await;
        serde_json::from_value(page["calls"].clone()).unwrap_or_default()
    }

    fn days_ago(n: i64) -> String {
        use time::format_description::well_known::Rfc3339;
        (time::OffsetDateTime::now_utc() - time::Duration::days(n))
            .format(&Rfc3339)
            .unwrap_or_default()
    }

    /// Write an index directly, so entries can carry dates the clock cannot reach.
    async fn seed(mcp: &str, rows: &[(u64, String, bool)]) {
        clear_calls(mcp).await;
        let dir = test_log_dir();
        mkdir_private(&dir);
        let lines: Vec<String> = rows
            .iter()
            .map(|(seq, at, body)| {
                let mut v = json!({
                    "seq": seq, "at": at, "tool": "t", "via": "mcp",
                    "ok": true, "ms": 1, "args": "", "output": "x", "chars": 1,
                });
                if *body {
                    v["preview"] = json!(true);
                    v["body"] = json!(true);
                }
                v.to_string()
            })
            .collect();
        std::fs::write(dir.join(format!("{mcp}.jsonl")), format!("{}\n", lines.join("\n"))).unwrap();
        if rows.iter().any(|r| r.2) {
            let bodies = dir.join("bodies").join(mcp);
            std::fs::create_dir_all(&bodies).unwrap();
            for (seq, _, body) in rows {
                if *body {
                    std::fs::write(bodies.join(format!("{seq}.txt")), "payload").unwrap();
                }
            }
        }
        forget_file_state(mcp); // the seeded file is now the only state
    }

    // --- the log itself ---

    #[tokio::test]
    async fn a_recorded_call_is_visible_to_the_very_next_read() {
        test_log_dir();
        let mcp = "visible-next-read";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("t", None, true, "one"));
        // The append runs in its own task; a reader must wait for it rather than merely finding
        // the write queue unlocked, which it is right up until that task first runs.
        assert_eq!(entries(mcp).await.len(), 1);
    }

    #[tokio::test]
    async fn records_the_arguments_and_the_reply_newest_first() {
        test_log_dir();
        let mcp = "records-newest-first";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("a", Some(json!({"sql":"SELECT 1"})), true, "one"));
        record_call(Some(mcp), &source(), rec("b", None, false, "boom"));

        let list = entries(mcp).await;
        assert_eq!(list.iter().map(|e| e.tool.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(list[1].args, r#"{"sql":"SELECT 1"}"#);
        assert_eq!(list[1].output, "one");
        assert!(!list[0].ok);
        assert_eq!(list[0].args, ""); // no arguments is not the string "null"
        assert_eq!(list[0].seq, 2);
    }

    #[tokio::test]
    async fn redacts_secret_looking_argument_values() {
        test_log_dir();
        let mcp = "redacts-secrets";
        clear_calls(mcp).await;
        record_call(
            Some(mcp),
            &source(),
            rec(
                "t",
                Some(json!({"user":"deploy","password":"hunter2","nested":{"apiToken":"abc"}})),
                true,
                "",
            ),
        );
        let args = &entries(mcp).await[0].args;
        assert!(args.contains("deploy"));
        assert!(!args.contains("hunter2"));
        assert!(!args.contains("abc"));
    }

    // This copy once lacked authorization|api[_-]?key: the same request was redacted in the
    // traffic view yet written PLAINTEXT here — and this file is the one that persists to disk.
    #[tokio::test]
    async fn redacts_api_key_and_authorization_args() {
        test_log_dir();
        let mcp = "redacts-wordlist";
        clear_calls(mcp).await;
        record_call(
            Some(mcp),
            &source(),
            rec(
                "t",
                Some(json!({
                    "apiKey":"sk-live-1","Authorization":"Bearer jt",
                    "passphrase":"pp","note":"keep"
                })),
                true,
                "",
            ),
        );
        let args = &entries(mcp).await[0].args;
        assert!(!args.contains("sk-live-1"));
        assert!(!args.contains("Bearer jt"));
        assert!(!args.contains("pp"));
        assert!(args.contains("keep"));
    }

    #[tokio::test]
    async fn survives_a_restart_and_keeps_counting() {
        test_log_dir();
        let mcp = "survives-restart";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("before", None, true, "kept"));
        flush_calls(Some(mcp)).await;

        forget_file_state(mcp); // same on-disk log, all in-memory state dropped
        let first = &entries(mcp).await[0];
        assert_eq!((first.tool.as_str(), first.output.as_str(), first.seq), ("before", "kept", 1));

        record_call(Some(mcp), &source(), rec("after", None, true, ""));
        assert_eq!(entries(mcp).await[0].seq, 2); // sequence continued, not restarted
    }

    #[tokio::test]
    async fn pages_newest_first_and_reports_whether_older_entries_exist() {
        test_log_dir();
        let mcp = "pages";
        clear_calls(mcp).await;
        for i in 1..=CALLS_PAGE_SIZE + 5 {
            record_call(Some(mcp), &source(), rec(&format!("t{i}"), None, true, "x"));
        }

        let first = read_calls(mcp, 0, CALLS_PAGE_SIZE as i64).await;
        assert_eq!(first["calls"].as_array().unwrap().len(), CALLS_PAGE_SIZE);
        assert_eq!(first["calls"][0]["tool"], format!("t{}", CALLS_PAGE_SIZE + 5));
        assert_eq!(first["more"], true);

        let second = read_calls(mcp, 1, CALLS_PAGE_SIZE as i64).await;
        assert_eq!(second["calls"].as_array().unwrap().len(), 5);
        assert_eq!(second["calls"][0]["tool"], "t5");
        assert_eq!(second["more"], false);
    }

    #[tokio::test]
    async fn previews_a_long_reply_in_a_page_and_serves_it_whole_by_seq() {
        test_log_dir();
        let mcp = "long-reply";
        clear_calls(mcp).await;
        // Bigger than any DB result the gateway will render (render_result caps those at 256 KB).
        let rows: Vec<Value> = (0..4000).map(|i| json!({"i": i, "pad": "y".repeat(60)})).collect();
        let big = serde_json::to_string(&json!({ "rows": rows })).unwrap();
        assert!(big.len() > 262_000);
        record_call(Some(mcp), &source(), rec("wide", None, true, &big));

        let entry = entries(mcp).await.remove(0);
        assert_eq!(entry.preview, Some(true));
        assert_eq!(entry.body, Some(true));
        // The index line stays small, so paging stays cheap...
        assert_eq!(entry.output.chars().count(), 2048);
        // ...while the reported size is the real one.
        assert_eq!(entry.chars, big.chars().count());

        let full = read_call(mcp, entry.seq).await.unwrap();
        assert_eq!(full.output, big); // whole, and therefore still valid JSON
        assert_eq!(full.preview, None);
        let parsed: Value = serde_json::from_str(&full.output).unwrap();
        assert_eq!(parsed["rows"].as_array().unwrap().len(), 4000);
    }

    #[tokio::test]
    async fn keeps_a_short_reply_inline_with_no_payload_file() {
        test_log_dir();
        let mcp = "short-reply";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("small", None, true, "PONG"));
        let entry = entries(mcp).await.remove(0);
        assert_eq!(entry.preview, None);
        assert_eq!(entry.body, None);
        assert_eq!(read_call(mcp, entry.seq).await.unwrap().output, "PONG");
    }

    #[tokio::test]
    async fn reports_a_pruned_payload_instead_of_returning_a_short_result_as_whole() {
        test_log_dir();
        let mcp = "pruned-payload";
        clear_calls(mcp).await;
        let big = "z".repeat(5000);
        record_call(Some(mcp), &source(), rec("wide", None, true, &big));
        flush_calls(Some(mcp)).await;
        let entry = entries(mcp).await.remove(0);
        std::fs::remove_file(test_log_dir().join("bodies").join(mcp).join(format!("{}.txt", entry.seq)))
            .unwrap(); // what pruning eventually does

        let full = read_call(mcp, entry.seq).await.unwrap();
        assert_eq!(full.body_gone, Some(true));
        assert_eq!(full.chars, 5000);
    }

    #[tokio::test]
    async fn tags_the_source_of_a_call_and_defaults_to_mcp() {
        test_log_dir();
        let mcp = "call-source";
        clear_calls(mcp).await;
        assert_eq!(current_source().via, "mcp");
        with_call_source_panel(async {
            let src = current_source();
            assert_eq!(src.via, "panel");
            record_call(Some(mcp), &src, rec("t", None, true, ""));
        })
        .await;
        assert_eq!(entries(mcp).await[0].via, "panel");
        record_call(Some(mcp), &source(), rec("t2", None, true, ""));
        assert_eq!(entries(mcp).await[0].via, "mcp");
    }

    #[tokio::test]
    async fn carries_the_authenticating_token_label_onto_a_client_call() {
        test_log_dir();
        let mcp = "call-client";
        clear_calls(mcp).await;
        with_call_client_mcp("laptop".to_string(), async {
            let src = current_source();
            assert_eq!(src.client.as_deref(), Some("laptop"));
            record_call(Some(mcp), &src, rec("t", None, true, ""));
        })
        .await;
        assert_eq!(entries(mcp).await[0].client.as_deref(), Some("laptop"));
    }

    #[tokio::test]
    async fn logs_a_failed_handler_as_a_failed_call_and_returns_the_error() {
        test_log_dir();
        let mcp = "logged-failure";
        clear_calls(mcp).await;
        let out: Result<Value, String> = logged(
            Some(mcp),
            &source(),
            "explode",
            Some(json!({"a":1})),
            async { Err("no such table: nope".to_string()) },
            |_: &Value| (true, String::new()),
        )
        .await;
        assert_eq!(out.unwrap_err(), "no such table: nope");
        let entry = entries(mcp).await.remove(0);
        assert!(!entry.ok);
        assert_eq!(entry.output, "no such table: nope");
    }

    // A tool that reports failure in-band never throws; logging that as a success would make the
    // log lie.
    #[tokio::test]
    async fn logs_an_in_band_is_error_result_as_a_failure() {
        test_log_dir();
        let mcp = "logged-in-band";
        clear_calls(mcp).await;
        let result = json!({"isError": true, "content":[{"type":"text","text":"refused"}]});
        let out: Result<Value, String> = logged(
            Some(mcp),
            &source(),
            "t",
            None,
            async { Ok(result.clone()) },
            |r: &Value| (r["isError"] != json!(true), content_text(r)),
        )
        .await;
        assert!(out.is_ok());
        let entry = entries(mcp).await.remove(0);
        assert!(!entry.ok);
        assert_eq!(entry.output, "refused");
    }

    #[tokio::test]
    async fn follows_a_rename_and_drops_the_log_on_clear() {
        test_log_dir();
        let (from, to) = ("rename-src", "rename-dst");
        clear_calls(from).await;
        clear_calls(to).await;
        record_call(Some(from), &source(), rec("t", None, true, ""));
        rename_calls(from, to).await;
        assert!(entries(from).await.is_empty());
        assert_eq!(entries(to).await.len(), 1);
        clear_calls(to).await;
        assert!(entries(to).await.is_empty());
        // The alias is the point of rename_calls, but it is process-global: left in place, every
        // later record_call(from) would silently file under `to`.
        forget_call_alias(from);
    }

    #[tokio::test]
    async fn ignores_a_call_with_no_mcp_name() {
        test_log_dir();
        record_call(None, &source(), rec("t", None, true, ""));
        assert!(entries("").await.is_empty());
    }

    #[test]
    fn flattens_non_text_content_blocks() {
        let mixed = json!({"content":[{"type":"text","text":"a"},{"type":"image"}]});
        assert_eq!(content_text(&mixed), "a\n[image content]");
        assert_eq!(content_text(&json!({})), "");
    }

    #[tokio::test]
    async fn files_a_call_issued_under_the_old_name_into_the_renamed_log() {
        test_log_dir();
        let (a, b) = ("inflight-a", "inflight-b");
        clear_calls(a).await;
        clear_calls(b).await;
        record_call(Some(a), &source(), rec("t", Some(json!({})), true, "first"));
        rename_calls(a, b).await;
        // logged() captured the old name before the rename began; the append lands after it.
        record_call(Some(a), &source(), rec("t", Some(json!({})), true, "second"));
        let outputs: Vec<String> = entries(b).await.into_iter().map(|e| e.output).collect();
        assert_eq!(outputs, ["second", "first"]);
        assert!(entries(a).await.is_empty());
        clear_calls(b).await;
        forget_call_alias(a);
    }

    #[tokio::test]
    async fn stops_redirecting_once_the_old_name_is_in_use_again() {
        test_log_dir();
        let (c, d) = ("redirect-c", "redirect-d");
        clear_calls(c).await;
        clear_calls(d).await;
        rename_calls(c, d).await;
        forget_call_alias(c); // a brand-new MCP registered under the freed name
        record_call(Some(c), &source(), rec("t", Some(json!({})), true, "fresh"));
        assert!(entries(d).await.is_empty());
        let outputs: Vec<String> = entries(c).await.into_iter().map(|e| e.output).collect();
        assert_eq!(outputs, ["fresh"]);
        clear_calls(c).await;
        clear_calls(d).await;
    }

    #[tokio::test]
    async fn skips_a_torn_line_instead_of_failing_the_page() {
        test_log_dir();
        let mcp = "torn-line";
        clear_calls(mcp).await;
        let dir = test_log_dir();
        mkdir_private(&dir);
        let good = json!({
            "seq":1,"at":days_ago(0),"tool":"t","via":"mcp","ok":true,"ms":1,
            "args":"","output":"whole","chars":5,
        });
        // A process killed mid-append leaves the last line half written.
        std::fs::write(
            dir.join(format!("{mcp}.jsonl")),
            format!("{good}\n{{\"seq\":2,\"at\":\"20"),
        )
        .unwrap();
        forget_file_state(mcp);
        let list = entries(mcp).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].output, "whole");
    }

    // --- retention ---

    #[tokio::test]
    async fn drops_entries_past_the_half_year_cutoff_and_keeps_everything_inside_it() {
        test_log_dir();
        let mcp = "aged-cutoff";
        seed(
            mcp,
            &[
                (1, days_ago(400), false),
                (2, days_ago(200), false),
                (3, days_ago(179), false), // just inside
                (4, days_ago(1), false),
            ],
        )
        .await;
        sweep_call_logs().await;
        flush_calls(Some(mcp)).await;
        let seqs: Vec<u64> = entries(mcp).await.into_iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [4, 3]);
    }

    #[tokio::test]
    async fn deletes_the_payload_files_of_entries_that_aged_out_and_keeps_the_rest() {
        test_log_dir();
        let mcp = "aged-bodies";
        seed(mcp, &[(1, days_ago(400), true), (2, days_ago(10), true)]).await;
        sweep_call_logs().await;
        flush_calls(Some(mcp)).await;
        let bodies = test_log_dir().join("bodies").join(mcp);
        assert!(!bodies.join("1.txt").exists());
        assert!(bodies.join("2.txt").exists());
    }

    // The point of the boot sweep: the per-append check only ever fires for a live MCP, so a log
    // nobody appends to is exactly the one that would keep year-old arguments forever.
    #[tokio::test]
    async fn sweeps_a_log_whose_mcp_is_never_called_again() {
        test_log_dir();
        let mcp = "retired-mcp";
        seed(mcp, &[(1, days_ago(365), false), (2, days_ago(300), false)]).await;
        sweep_call_logs().await;
        flush_calls(Some(mcp)).await;
        assert!(entries(mcp).await.is_empty());
        clear_calls(mcp).await;
    }

    #[tokio::test]
    async fn leaves_a_log_alone_when_nothing_in_it_has_expired() {
        test_log_dir();
        let mcp = "aged-untouched";
        seed(mcp, &[(1, days_ago(5), false), (2, days_ago(2), false)]).await;
        let path = test_log_dir().join(format!("{mcp}.jsonl"));
        let before = std::fs::read_to_string(&path).unwrap();
        sweep_call_logs().await;
        flush_calls(Some(mcp)).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before); // not rewritten
    }

    #[tokio::test]
    async fn keeps_appending_correctly_after_a_sweep() {
        test_log_dir();
        let mcp = "aged-then-append";
        seed(mcp, &[(1, days_ago(400), false), (7, days_ago(3), false)]).await;
        sweep_call_logs().await;
        flush_calls(Some(mcp)).await;
        record_call(Some(mcp), &source(), rec("after", Some(json!({})), true, "ok"));
        let list = entries(mcp).await;
        // The sequence continues from the surviving tail rather than restarting.
        assert_eq!(list.iter().map(|e| e.seq).collect::<Vec<_>>(), [8, 7]);
        assert_eq!(list[0].tool, "after");
    }

    // --- read_tool_history: the Run tab refill dropdown ---

    #[tokio::test]
    async fn lists_one_tools_newest_runs_first_skipping_other_tools() {
        test_log_dir();
        let mcp = "history-basic";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("pg_query", Some(json!({"sql":"SELECT 1"})), true, "a"));
        record_call(Some(mcp), &source(), rec("pg_tables", Some(json!({})), true, "b"));
        record_call(Some(mcp), &source(), rec("pg_query", Some(json!({"sql":"SELECT 2"})), true, "c"));
        flush_calls(Some(mcp)).await;

        let rows = read_tool_history(mcp, "pg_query", 10, None).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["args"], r#"{"sql":"SELECT 2"}"#);
        assert_eq!(rows[1]["args"], r#"{"sql":"SELECT 1"}"#);
    }

    #[tokio::test]
    async fn cuts_a_long_args_preview_off() {
        test_log_dir();
        let mcp = "history-clip";
        clear_calls(mcp).await;
        record_call(
            Some(mcp),
            &source(),
            rec("t", Some(json!({"sql": "S".repeat(400)})), true, ""),
        );
        flush_calls(Some(mcp)).await;
        let rows = read_tool_history(mcp, "t", 10, None).await;
        let args = rows[0]["args"].as_str().unwrap();
        // One huge argument set must not bloat the dropdown.
        assert_eq!(args.chars().count(), 97);
        assert!(args.ends_with('…'));
    }

    #[tokio::test]
    async fn collapses_repeated_runs_of_the_same_arguments_keeping_the_newest() {
        test_log_dir();
        let mcp = "history-collapse";
        clear_calls(mcp).await;
        for _ in 0..3 {
            record_call(Some(mcp), &source(), rec("t", Some(json!({"sql":"SELECT 1"})), true, ""));
        }
        record_call(Some(mcp), &source(), rec("t", Some(json!({"sql":"SELECT 2"})), true, ""));
        flush_calls(Some(mcp)).await;
        let rows = read_tool_history(mcp, "t", 10, None).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["args"], r#"{"sql":"SELECT 2"}"#);
        assert_eq!(rows[1]["seq"], 3); // the newest occurrence of the repeated run
    }

    #[tokio::test]
    async fn keeps_two_long_argument_sets_apart_that_share_a_clipped_preview_prefix() {
        test_log_dir();
        let mcp = "history-prefix";
        clear_calls(mcp).await;
        let shared = "S".repeat(200);
        record_call(Some(mcp), &source(), rec("t", Some(json!({"sql": format!("{shared}A")})), true, ""));
        record_call(Some(mcp), &source(), rec("t", Some(json!({"sql": format!("{shared}B")})), true, ""));
        flush_calls(Some(mcp)).await;
        // Distinctness is decided on the FULL stored arguments, never on the clipped preview.
        assert_eq!(read_tool_history(mcp, "t", 10, None).await.len(), 2);
    }

    #[tokio::test]
    async fn filters_by_a_case_insensitive_substring_of_the_full_arguments() {
        test_log_dir();
        let mcp = "history-filter";
        clear_calls(mcp).await;
        let pad = "S".repeat(200);
        record_call(Some(mcp), &source(), rec("t", Some(json!({"sql": format!("{pad}NEEDLE")})), true, ""));
        record_call(Some(mcp), &source(), rec("t", Some(json!({"sql": format!("{pad}OTHER")})), true, ""));
        flush_calls(Some(mcp)).await;
        // The match reaches past the preview clip, and ignores case.
        let rows = read_tool_history(mcp, "t", 10, Some("needle")).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["seq"], 1);
        assert!(read_tool_history(mcp, "t", 10, Some("  ")).await.len() == 2); // blank filters nothing
    }

    #[tokio::test]
    async fn honours_a_smaller_limit_and_never_exceeds_the_history_max() {
        test_log_dir();
        let mcp = "history-limit";
        clear_calls(mcp).await;
        for i in 0..4 {
            record_call(Some(mcp), &source(), rec("t", Some(json!({"i": i})), true, ""));
        }
        flush_calls(Some(mcp)).await;
        assert_eq!(read_tool_history(mcp, "t", 2, None).await.len(), 2);
        assert_eq!(read_tool_history(mcp, "t", 0, None).await.len(), 1); // clamped up to 1
        let all = read_tool_history(mcp, "t", usize::MAX, None).await;
        assert_eq!(all.len(), 4);
        assert!(all.len() <= TOOL_HISTORY_MAX);
    }

    #[tokio::test]
    async fn answers_an_empty_list_for_a_tool_that_was_never_called() {
        test_log_dir();
        let mcp = "history-empty";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("t", None, true, ""));
        flush_calls(Some(mcp)).await;
        assert!(read_tool_history(mcp, "never", 10, None).await.is_empty());
    }

    // --- stored body pruning ---

    // 55 oversized replies → body files at seqs 1..55. Real logs are sparser (only a reply over
    // the preview is stored at all); arithmetic on `seq - BODY_KEEP` missed the gaps and left
    // genuinely old payloads on disk forever.
    #[tokio::test]
    async fn prunes_stored_body_files_by_directory_listing_not_sequence_arithmetic() {
        test_log_dir();
        let mcp = "body-pruning";
        clear_calls(mcp).await;
        let big = "x".repeat(3 * 1024);
        for _ in 0..55 {
            record_call(Some(mcp), &source(), rec("t", None, true, &big));
            flush_calls(Some(mcp)).await;
        }
        assert_eq!(read_call(mcp, 1).await.unwrap().body_gone, Some(true)); // past the keep window
        assert_eq!(read_call(mcp, 5).await.unwrap().body_gone, Some(true));
        assert_eq!(read_call(mcp, 6).await.unwrap().body_gone, None); // the 50 newest stay whole
        assert_eq!(read_call(mcp, 55).await.unwrap().output.chars().count(), 3 * 1024);
    }

    #[tokio::test]
    async fn read_call_answers_none_for_a_sequence_that_was_never_written() {
        test_log_dir();
        let mcp = "read-call-missing";
        clear_calls(mcp).await;
        record_call(Some(mcp), &source(), rec("t", None, true, "x"));
        flush_calls(Some(mcp)).await;
        assert!(read_call(mcp, 99).await.is_none());
    }
}
