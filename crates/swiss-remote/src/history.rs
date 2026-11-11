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

//! The remote run log — the durable half of every remote.exec / sync / pull (2026-09-18).
//!
//! The coordinator's finished ring is 32 views in memory, gone at restart; the build an
//! agent kicked off at 3 a.m. must still be readable in the morning. This is the
//! [RunHistorySink] the remote plugin registers at start and removes at stop: one JSON
//! line per finished run in `logs/remote/runs.jsonl` — the run view exactly as
//! `/api/runs` showed it, plus what was asked (target, argv, cwd; env KEYS only, values
//! may be secrets; a remote.write's content as its SIZE only, since the file written may
//! be a credential) — and the full output stream in `logs/remote/out/<runId>.txt`, teed
//! as it flows. A remote.write's body is kept too, but SEALED, beside the output as
//! `out/<runId>.content` (SPEC §remote.history): the owner wants to see what a write wrote, and a
//! written .env must not sit on disk in clear. The live buffer keeps a 256 KiB window; the file keeps the head up to
//! [Limits::max_output_bytes], and past that the line keeps the last 64 KiB as `tail`, so
//! a capped build log still shows both ends — the error is usually at the end.
//!
//! Three budgets, oldest-first: age, total bytes on disk (index + output files), and a
//! run count so the index itself stays small. Checked in O(1) on every finish and every
//! page read from three tracked numbers; the full pass (two streaming walks of the index
//! and a tmp+rename rewrite, never the whole file in memory) runs only when one is over.
//! One promise sits above the budgets (SPEC §remote.history): a line younger than
//! [AUDIT_WINDOW_MS] is never dropped by a budget. Under byte pressure the window's
//! OUTPUT FILES go, oldest first, and the line says so (`outputEvicted: true`; its
//! `tail`, when it has one, stays); under count pressure the window simply runs over,
//! and a pass that could satisfy nothing holds off until the oldest protected line
//! leaves the window rather than rewriting the index on every read.
//! Reading a page walks the index from its END in blocks — the calls.rs / runlog.rs
//! idiom — so a page costs a page, however long the history behind it.
//!
//! An INSTANCE: the directory and the budgets are fields, so a test holds one on a
//! scratch directory and two systems in one process never see each other's files.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use swiss_core::platform::{chmod_private, mkdir_private, private_file_mode};
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};
use swiss_core::util::{now_ms, parse_iso_ms};
use swiss_host::services::action::RunOutputTee;
use swiss_host::services::runs::{HistoryAck, RunHistorySink, RunView, SubmitRequest};

/// Records older than this age out (the user's retention: 30 days).
pub const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Records younger than this are never dropped by a budget (SPEC §remote.history): the seven days
/// the owner can always trace back. Their output files may go under byte pressure; the
/// line — what ran, who, exit, tail — stays until the age limit.
pub const AUDIT_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The whole log — index plus every output file — stays under this (500 MiB).
pub const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// A count cap so a flood of tiny runs cannot grow the index past a few megabytes.
pub const MAX_RUNS: usize = 5000;
/// One run's output file stops growing here; the record keeps the tail past it.
pub const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// What a capped run keeps of its end (the ring's finished-keep size, SPEC §host.actions).
pub const TAIL_BYTES: usize = 64 * 1024;
/// One output read never materialises more than this — the live route's own ceiling.
pub const MAX_READ_BYTES: usize = 128 * 1024;
pub const PAGE_SIZE: usize = 20;
/// What a remote.write's sealed body keeps at most (the owner's choice, 2026-09-28); a
/// longer file keeps its head and says so.
pub const CONTENT_MAX_BYTES: usize = 256 * 1024;

const INDEX_FILE: &str = "runs.jsonl";
const OUT_DIR: &str = "out";

/// The budgets. Constants today; fields so a test can shrink them to a few lines.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_age_ms: u64,
    pub max_total_bytes: u64,
    pub max_runs: usize,
    pub max_output_bytes: u64,
    /// The protected window ([AUDIT_WINDOW_MS]): lines this young survive every budget.
    pub audit_window_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_age_ms: MAX_AGE_MS,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_runs: MAX_RUNS,
            max_output_bytes: MAX_OUTPUT_BYTES,
            audit_window_ms: AUDIT_WINDOW_MS,
        }
    }
}

/// The three numbers the O(1) budget check reads, kept current by every append and
/// every rewrite. Rebuilt from the index at open.
#[derive(Debug, Default, Clone, Copy)]
struct Ledger {
    /// Index bytes plus the on-disk size of every output file.
    total_bytes: u64,
    runs: usize,
    /// `endedAt` of the first (oldest) line, epoch ms.
    oldest_ended_ms: Option<u64>,
    /// Set by a pass that left a budget over because everything left is protected: no
    /// pass before this instant can do better, so none runs (a new output file under
    /// byte pressure clears it - that file is fair game).
    hold_until_ms: Option<u64>,
}

pub struct RunHistory {
    dir: PathBuf,
    limits: Limits,
    /// The module's own database (store.rs), attached by the plugin's start. While
    /// attached this jsonl tree stays a WRITE-THROUGH spill — the tee file and the
    /// index line land exactly as before, so the tree remains a complete fallback
    /// and the recovery source for a crash between spill and commit — and the
    /// database is the query authority.
    store: tokio::sync::RwLock<Option<Arc<crate::store::RemoteHistory>>>,
    /// The highest wire run id the database held at attach — the coordinator's
    /// numbering floor, cached because the sink trait reads it synchronously.
    run_floor: std::sync::atomic::AtomicU64,
    ledger: Mutex<Ledger>,
    /// The tees handed out at begin and not yet closed — finish finds its file here —
    /// each with the sanitized input, so a run in flight can say what it is running
    /// before its record exists. At most max_concurrent entries (the coordinator's
    /// bound), never more.
    open: Mutex<HashMap<u64, (Arc<OutputFile>, Value)>>,
    /// The REMOTE.HISTORY notice (SPEC §remote.history), attached by the system that
    /// owns this log. Publishes and reads nothing else, so a pass may call it under
    /// the ledger lock.
    notice: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The open passes rewrote rows before any notice could exist; the first attach
    /// publishes for them (SPEC §remote.history: the open at start).
    rewrote_at_open: std::sync::atomic::AtomicBool,
    /// The trail followers (SPEC §remote.history "Provenance"): a run whose input names an
    /// agent origin gets its submitting call looked up in that agent's log.
    trails: crate::trail::Trails,
}

/// One run's output file while it streams: the tee the coordinator writes into. Past
/// the cap the file is closed and only a bounded tail ring keeps filling, so the
/// per-run memory is zero until a run is actually enormous.
struct OutputFile {
    inner: Mutex<OutputFileInner>,
}

struct OutputFileInner {
    file: Option<BufWriter<File>>,
    written: u64,
    cap: u64,
    /// Bytes that did not fit — the tail ring, allocated only once the cap is hit.
    overflow: Option<VecDeque<u8>>,
}

impl RunOutputTee for OutputFile {
    fn write(&self, bytes: &[u8]) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let room = inner.cap.saturating_sub(inner.written) as usize;
        let take = room.min(bytes.len());
        if take > 0 {
            if let Some(file) = inner.file.as_mut() {
                // A failed write drops the file; the record then says how much landed.
                if file.write_all(&bytes[..take]).is_err() {
                    inner.file = None;
                }
            }
            inner.written += take as u64;
        }
        let rest = &bytes[take..];
        if !rest.is_empty() {
            // The cap: the file is complete at exactly cap bytes; the rest rings.
            if let Some(mut file) = inner.file.take() {
                let _ = file.flush();
            }
            let ring = inner
                .overflow
                .get_or_insert_with(|| VecDeque::with_capacity(TAIL_BYTES));
            ring.extend(rest.iter().copied());
            let excess = ring.len().saturating_sub(TAIL_BYTES);
            if excess > 0 {
                ring.drain(..excess);
            }
        }
    }
}

impl OutputFile {
    /// Flush and close: (bytes in the file, the tail past the cap when there is one).
    fn close(&self) -> (u64, Option<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut file) = inner.file.take() {
            let _ = file.flush();
        }
        // The ring kept the LAST tail bytes by count, so it may start inside a character
        // (SPEC §remote.utf8): decode from the first boundary, not from byte zero.
        let tail = inner.overflow.take().map(|ring| {
            let bytes = ring.into_iter().collect::<Vec<u8>>();
            String::from_utf8_lossy(swiss_core::utf8::window(&bytes)).into_owned()
        });
        (inner.written, tail)
    }
}

/// What a page read asks for (SPEC §remote.history): the cursor and size the pane pages with,
/// and the predicates the audit surface adds. `since` (inclusive) and `until`
/// (exclusive) are epoch ms against the record's `endedAt`. The index is in finish
/// order, so a line older than `since` ends the walk (finishes a few ms apart can
/// land out of order - a boundary that fine is not one an audit turns on).
#[derive(Debug, Clone, Copy, Default)]
pub struct PageQuery<'a> {
    pub before: Option<u64>,
    pub limit: usize,
    pub target: Option<&'a str>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub actor: Option<&'a str>,
}

/// An instant as a query string spells it: epoch ms, or ISO-8601.
pub fn parse_instant(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw.parse::<u64>().ok();
    }
    parse_iso_ms(raw).and_then(|ms| u64::try_from(ms).ok())
}

/// A remote.write's kept body ([RunHistory::content]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredContent {
    pub text: String,
    pub truncated: bool,
}

/// One bounded read of a recorded run's output file.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub text: String,
    pub cursor: u64,
    pub next_cursor: u64,
    /// The file's length — a reader knows when it has everything.
    pub total: u64,
}

impl RunHistory {
    /// A log over `dir` (`logs/remote` in the gateway; created on first record). One
    /// streaming walk of the index rebuilds the ledger; output files the index does not
    /// name (a crash mid-run) are removed — nothing would ever evict them otherwise.
    pub fn open(dir: PathBuf) -> Arc<Self> {
        Self::open_with(dir, Limits::default())
    }

    pub fn open_with(dir: PathBuf, limits: Limits) -> Arc<Self> {
        let history = RunHistory {
            dir,
            limits,
            store: tokio::sync::RwLock::new(None),
            run_floor: std::sync::atomic::AtomicU64::new(0),
            ledger: Mutex::new(Ledger::default()),
            open: Mutex::new(HashMap::new()),
            notice: Mutex::new(None),
            rewrote_at_open: std::sync::atomic::AtomicBool::new(false),
            trails: crate::trail::Trails::default(),
        };
        // Before the ledger is built, so it counts what the passes sealed and masked.
        let sealed = history.seal_legacy_content();
        let masked = history.mask_recorded_literals();
        history
            .rewrote_at_open
            .store(sealed || masked, std::sync::atomic::Ordering::SeqCst);
        let mut known = HashSet::new();
        let ledger = history.scan(|line| {
            known.insert(line.run_id);
        });
        *history.ledger.lock().unwrap_or_else(|e| e.into_inner()) = ledger;
        if let Ok(entries) = std::fs::read_dir(history.dir.join(OUT_DIR)) {
            for entry in entries.filter_map(|e| e.ok()) {
                let orphan = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| {
                        n.strip_suffix(".txt")
                            .or_else(|| n.strip_suffix(".content"))
                    })
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|id| !known.contains(&id));
                if orphan {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Arc::new(history)
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Attach the REMOTE.HISTORY notice. Idempotent; the first attach also publishes
    /// for rows the open passes rewrote before any notice existed (SPEC
    /// §remote.history). The database, when attached later or already attached,
    /// gets the same notice — its hourly sweep removes rows on its own.
    pub fn set_notice(&self, notice: Arc<dyn Fn() + Send + Sync>) {
        if let Ok(attached) = self.store.try_read() {
            if let Some(store) = attached.as_ref() {
                store.set_notice(notice.clone());
            }
        }
        *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(notice);
        if self
            .rewrote_at_open
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.changed();
        }
    }

    /// What the query answers moved. The callback owes this type's locks nothing.
    fn changed(&self) {
        let notice = self
            .notice
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(notice) = notice {
            notice();
        }
    }

    /// The sanitized input of a run that began and has not finished — for the active
    /// rows of the runs page, which the coordinator's view does not carry.
    pub fn in_flight_input(&self, run_id: u64) -> Option<Value> {
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run_id)
            .map(|(_, input)| input.clone())
    }


    // --- the module database seam (store.rs, SPEC `§host.history`) ----------------------------

    /// Attach the module's database: it becomes the query authority and the final
    /// commit of every record; this jsonl tree keeps its write-through spill role.
    /// The coordinator's numbering floor rises to the database's highest run id, so
    /// a restart never files two runs under one number.
    pub async fn attach_history(&self, history: Arc<crate::store::RemoteHistory>) {
        if let Some(floor) = history.last_run_id().await {
            self.run_floor
                .fetch_max(floor, std::sync::atomic::Ordering::SeqCst);
        }
        // The database's hourly sweep removes rows on its own: it owes the notice
        // itself, so the spill forwards what it holds (SPEC §remote.history).
        let notice = self
            .notice
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(notice) = notice {
            history.set_notice(notice);
        }
        *self.store.write().await = Some(history.clone());
        // The trail searches the last gateway left unfinished go on (SPEC §remote.history
        // "Provenance").
        self.trails.resume(history).await;
    }

    /// Detach — the plugin's stop, before the bounded close. Returns the database it
    /// held, so the stop can close it bounded after the accepted work drained.
    pub async fn detach_history(&self) -> Option<Arc<crate::store::RemoteHistory>> {
        self.store.write().await.take()
    }

    async fn attached(&self) -> Option<Arc<crate::store::RemoteHistory>> {
        self.store.read().await.clone()
    }

    /// [Self::query] over whichever authority holds the log now.
    pub async fn query_records(&self, q: PageQuery<'_>) -> (Vec<Value>, Option<u64>) {
        if let Some(h) = self.attached().await {
            return h.page(&q).await;
        }
        self.query(q)
    }

    /// [Self::get] over the attached authority first.
    pub async fn get_record(&self, run_id: u64) -> Option<Value> {
        if let Some(h) = self.attached().await {
            return h.get(run_id).await;
        }
        self.get(run_id)
    }

    /// [Self::output] over the attached authority first.
    pub async fn output_record(
        &self,
        run_id: u64,
        after: u64,
        max: usize,
    ) -> Option<OutputChunk> {
        if let Some(h) = self.attached().await {
            return h.output(run_id, after, max).await;
        }
        self.output(run_id, after, max)
    }

    /// The trail filed for a run (SPEC §remote.history "Provenance"): only the database
    /// keeps one, so a log without it has none.
    pub async fn trail_record(&self, run_id: u64) -> Option<Value> {
        self.attached().await?.trail(run_id).await
    }

    /// The trail readers, for tests that swap in their own.
    pub fn trails(&self) -> &crate::trail::Trails {
        &self.trails
    }

    /// [Self::content] over the attached authority first.
    pub async fn content_record(&self, run_id: u64) -> Result<Option<StoredContent>, String> {
        if let Some(h) = self.attached().await {
            return h.content(run_id).await;
        }
        self.content(run_id)
    }

    /// [Self::clear] over whichever authority holds the log. A clear resets the wire
    /// records, never the coordinator's numbering and never the internal identity.
    pub async fn clear_records(&self) {
        let had = {
            let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            (ledger.total_bytes, ledger.runs)
        };
        let mut moved = had != (0, 0);
        if let Some(h) = self.attached().await {
            // The database keeps its own retention: rows can sit there while the spill
            // ledger reads empty, so its count decides the move too (SPEC
            // §remote.history). A failed clear leaves the spill's own verdict.
            let cleared = h.clear().await;
            if let Ok(gone) = cleared {
                moved = moved || gone > 0;
            }
        }
        self.clear();
        // A clear over an empty record answers nothing new: not a move (SPEC
        // §remote.history).
        if moved {
            self.changed();
        }
    }

    /// What the log holds right now: (bytes on disk, runs).
    pub fn usage(&self) -> (u64, usize) {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        (ledger.total_bytes, ledger.runs)
    }

    /// Where the log lives (`<home>/logs/remote`): the index file and the `out/` files.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    fn output_path(&self, run_id: u64) -> PathBuf {
        self.dir.join(OUT_DIR).join(format!("{run_id}.txt"))
    }

    fn content_path(&self, run_id: u64) -> PathBuf {
        self.dir.join(OUT_DIR).join(format!("{run_id}.content"))
    }

    /// A remote.write's body as the run sent it (its first [CONTENT_MAX_BYTES]), when the
    /// record kept one. Err when the file is there but this machine's key cannot open it.
    pub fn content(&self, run_id: u64) -> Result<Option<StoredContent>, String> {
        let Some(v) = read_secure_json(&self.content_path(run_id))? else {
            return Ok(None);
        };
        Ok(Some(StoredContent {
            text: v["content"].as_str().unwrap_or_default().to_string(),
            truncated: v["truncated"].as_bool().unwrap_or(false),
        }))
    }

    /// Seal a remote.write's body beside its record, under the machine key every state
    /// file uses. Returns the file's size on disk (0 when there is none) and whether the
    /// body was cut at [CONTENT_MAX_BYTES]. Kept whatever the outcome: a failed write's
    /// body is what failed to land.
    fn seal_content(&self, run_id: u64, request: &SubmitRequest) -> (u64, bool) {
        if request.action_type != "remote.write" {
            return (0, false);
        }
        let Some(content) = request.input.get("content").and_then(Value::as_str) else {
            return (0, false);
        };
        self.seal_body(run_id, content)
    }

    /// An exec's stdin: the script or SQL the run actually ran. The owner's finding
    /// (2026-10-10): a record whose input says only `bash -s` answers nothing about
    /// WHAT ran. Sealed beside the record exactly as a write's body is - a stdin can
    /// carry a secret in clear, so the index never holds the text; the machine key
    /// opens it on Show. A write run has no stdin, an exec run no written body, so
    /// the two share the sealed slot without ever colliding.
    fn seal_stdin(&self, run_id: u64, request: &SubmitRequest) -> (u64, bool) {
        if request.action_type != "remote.exec" {
            return (0, false);
        }
        let Some(stdin) = request.input.get("stdin").and_then(Value::as_str) else {
            return (0, false);
        };
        if stdin.is_empty() {
            return (0, false);
        }
        self.seal_body(run_id, stdin)
    }

    /// Seal one body beside its record: the write half of [RunHistory::content], shared by
    /// a finishing run and by the pass that moves a legacy record's clear body across.
    fn seal_body(&self, run_id: u64, content: &str) -> (u64, bool) {
        let mut end = content.len().min(CONTENT_MAX_BYTES);
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = end < content.len();
        let path = self.content_path(run_id);
        mkdir_private(&self.dir.join(OUT_DIR));
        let body = json!({ "content": &content[..end], "truncated": truncated });
        if write_secure_json(&path, &body).is_err() {
            return (0, false);
        }
        chmod_private(&path, private_file_mode());
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        (bytes, truncated)
    }

    /// Records written before 2026-09-28 carry a remote.write's body in CLEAR under
    /// `input.content` - the shape 9a6aebe stopped writing and R12 replaced with a sealed
    /// file. The owner, on such a row: "this still isn't solved" - the panel shows a sealed
    /// body only, so those writes looked empty while their bodies sat in the index. One pass
    /// at open moves each across: the body is sealed beside its record and the line is
    /// rewritten the way [RunHistory::record] writes one today. A line whose seal fails (no
    /// machine key yet) is left exactly as it is - the next open tries again rather than
    /// dropping what it could not keep. Nothing to move costs one streaming walk and no
    /// rewrite, which is every start after the first.
    /// Whether the pass rewrote rows (SPEC §remote.history: the open at start).
    fn seal_legacy_content(&self) -> bool {
        if !self.any_line(|m| legacy_body(m).is_some()) {
            return false;
        }
        self.rewrite_index(|m| {
            let moved = match (m.get("runId").and_then(Value::as_u64), legacy_body(m)) {
                (Some(run_id), Some(body)) => {
                    let body = body.to_string();
                    let (bytes, truncated) = self.seal_body(run_id, &body);
                    (bytes > 0).then_some((body.len(), bytes, truncated))
                }
                _ => None,
            };
            if let Some((clear_bytes, file_bytes, truncated)) = moved {
                if let Some(Value::Object(input)) = m.get_mut("input") {
                    input.remove("content");
                    input.insert("contentBytes".into(), json!(clear_bytes));
                }
                m.insert("contentStored".into(), json!(true));
                m.insert("contentFileBytes".into(), json!(file_bytes));
                if truncated {
                    m.insert("contentTruncated".into(), json!(true));
                }
            }
        })
    }

    /// Records written before 2026-10-22 keep an exec's argv and cwd as typed, so a password
    /// typed into a command line (`REDISCLI_AUTH=<value>` inside a `bash -c` word, on target
    /// test, 2026-10-21) sat in every one - the panel's Runs page, `swiss run audit` and its
    /// exports. One pass at open masks each such value the way [RunHistory::record] masks
    /// one today ([mask_record]); the database rows get the same pass in
    /// [crate::store::RemoteHistory::open]. Nothing to mask costs one streaming walk.
    fn mask_recorded_literals(&self) -> bool {
        if !self.any_line(|m| mask_record(&mut m.clone())) {
            return false;
        }
        self.rewrite_index(|m| {
            mask_record(m);
        })
    }

    /// Whether any whole line of the index passes `test` - one streaming walk, stopping at
    /// the first.
    fn any_line(&self, test: impl Fn(&Map<String, Value>) -> bool) -> bool {
        let Ok(file) = File::open(self.index_path()) else {
            return false;
        };
        BufReader::new(file)
            .split(b'\n')
            .map_while(Result::ok)
            .any(|line| matches!(parse_line(&line), Some(Value::Object(m)) if test(&m)))
    }

    /// The index rewritten line by line through `edit`, into a temporary file renamed over
    /// it - never the whole file in memory. A torn line (a kill mid-append) is dropped, as in
    /// evict; a failed rewrite leaves the index exactly as it was.
    fn rewrite_index(&self, mut edit: impl FnMut(&mut Map<String, Value>)) -> bool {
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        let rewrite = (|| -> std::io::Result<()> {
            let src = File::open(self.index_path())?;
            let mut out = BufWriter::new(File::create(&tmp)?);
            for line in BufReader::new(src).split(b'\n').map_while(Result::ok) {
                let Some(Value::Object(mut m)) = parse_line(&line) else {
                    continue;
                };
                edit(&mut m);
                out.write_all(serde_json::to_string(&Value::Object(m))?.as_bytes())?;
                out.write_all(b"\n")?;
            }
            out.flush()?;
            drop(out);
            std::fs::rename(&tmp, self.index_path())
        })();
        if rewrite.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        chmod_private(&self.index_path(), private_file_mode());
        true
    }

    /// A page of records, newest first, strictly before `before` when given, filtered by
    /// target id when given. Returns the page and the cursor for the next (older) one.
    pub fn page(
        &self,
        before: Option<u64>,
        limit: usize,
        target: Option<&str>,
    ) -> (Vec<Value>, Option<u64>) {
        self.query(PageQuery {
            before,
            limit,
            target,
            ..PageQuery::default()
        })
    }

    /// [Self::page] with every predicate (SPEC §remote.history): the time window and the actor,
    /// on top of the cursor and the target. Still one walk from the end, one page's
    /// worth of records in memory.
    pub fn query(&self, q: PageQuery<'_>) -> (Vec<Value>, Option<u64>) {
        self.enforce_budget();
        let limit = q.limit.clamp(1, 100);
        let mut records: Vec<Value> = Vec::with_capacity(limit + 1);
        self.walk_back(|v| {
            let id = v.get("runId").and_then(Value::as_u64).unwrap_or(0);
            if q.before.is_some_and(|b| id >= b) {
                return true;
            }
            let ended = record_ended_ms(&v);
            if q.since.is_some_and(|s| ended < s) {
                return false; // older than the window: so is everything before it
            }
            if q.until.is_some_and(|u| ended >= u) {
                return true;
            }
            if q.target.is_some_and(|t| record_target(&v) != Some(t)) {
                return true;
            }
            if q.actor
                .is_some_and(|a| v.get("actor").and_then(Value::as_str) != Some(a))
            {
                return true;
            }
            records.push(v);
            records.len() <= limit
        });
        let take = records.len().min(limit);
        let page: Vec<Value> = records.drain(..take).collect();
        let next_before = if records.is_empty() {
            None
        } else {
            page.last()
                .and_then(|v| v.get("runId").and_then(Value::as_u64))
        };
        (page, next_before)
    }

    /// One record by run id — a walk from the newest, so a recent run is found in one
    /// block and a miss costs the index.
    pub fn get(&self, run_id: u64) -> Option<Value> {
        let mut found = None;
        self.walk_back(|v| {
            if v.get("runId").and_then(Value::as_u64) == Some(run_id) {
                found = Some(v);
                false
            } else {
                true
            }
        });
        found
    }

    /// Read the recorded output from byte `after`, at most `max` bytes (clamped to
    /// [MAX_READ_BYTES]). None when no output file exists for the id — the caller
    /// distinguishes "unknown run" from "ran silently" through [Self::get].
    pub fn output(&self, run_id: u64, after: u64, max: usize) -> Option<OutputChunk> {
        let mut file = File::open(self.output_path(run_id)).ok()?;
        let total = file.metadata().map(|m| m.len()).unwrap_or(0);
        let from = after.min(total);
        let max = max.clamp(1, MAX_READ_BYTES);
        let want = (total - from).min(max as u64) as usize;
        let mut buf = vec![0u8; want];
        if want > 0 {
            if file.seek(SeekFrom::Start(from)).is_err() {
                return None;
            }
            let mut got = 0;
            while got < want {
                match file.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(_) => break,
                }
            }
            buf.truncate(got);
        }
        // Character boundaries (SPEC §remote.utf8): a window that would end inside a
        // character stops before it - unless that would leave nothing, or the bytes are
        // the file's last (a capped file can end mid-character; the record's `tail`
        // carries the rest) - so the follower reads the character whole next time.
        let at_end = from + buf.len() as u64 >= total;
        let keep = swiss_core::utf8::char_boundary_end(&buf);
        if keep < buf.len() && keep > 0 && !at_end {
            buf.truncate(keep);
        }
        let next = from + buf.len() as u64;
        Some(OutputChunk {
            text: String::from_utf8_lossy(&buf).into_owned(),
            cursor: from,
            next_cursor: next,
            total,
        })
    }

    /// Drop every record and output file. The panel's Clear.
    pub fn clear(&self) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_file(self.index_path());
        let _ = std::fs::remove_dir_all(self.dir.join(OUT_DIR));
        *ledger = Ledger::default();
    }

    /// One streaming pass over the index, oldest first, rebuilding the ledger; `seen`
    /// gets each line's facts. Torn lines (a kill mid-append) are skipped, not fatal.
    fn scan(&self, mut seen: impl FnMut(&LineFacts)) -> Ledger {
        let mut ledger = Ledger::default();
        let Ok(file) = File::open(self.index_path()) else {
            return ledger;
        };
        let reader = BufReader::new(file);
        for line in reader.split(b'\n').map_while(Result::ok) {
            let facts = LineFacts::parse(&line);
            ledger.total_bytes += line.len() as u64 + 1;
            let Some(facts) = facts else {
                continue;
            };
            ledger.total_bytes += facts.output_bytes;
            ledger.runs += 1;
            if ledger.oldest_ended_ms.is_none() {
                ledger.oldest_ended_ms = Some(facts.ended_ms);
            }
            seen(&facts);
        }
        ledger
    }

    /// Walk the index newest-first in fixed blocks, handing each record to `keep_going`
    /// until it says stop or the file starts. Ported from runlog.rs's page reader.
    fn walk_back(&self, mut keep_going: impl FnMut(Value) -> bool) {
        let Ok(mut file) = File::open(self.index_path()) else {
            return;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        const CHUNK: u64 = 8 * 1024;
        let mut tail: Vec<u8> = Vec::new();
        let mut pos = len;
        while pos > 0 {
            let take = CHUNK.min(pos) as usize;
            pos -= take as u64;
            let mut buf = vec![0u8; take];
            if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut buf).is_err() {
                return;
            }
            buf.append(&mut tail);
            tail = buf;
            while let Some(nl) = tail.iter().rposition(|b| *b == b'\n') {
                let line = tail.split_off(nl + 1);
                tail.truncate(nl);
                if let Some(v) = parse_line(&line) {
                    if !keep_going(v) {
                        return;
                    }
                }
            }
        }
        if !tail.is_empty() {
            if let Some(v) = parse_line(&tail) {
                keep_going(v);
            }
        }
    }

    /// The O(1) check; the full pass only when a budget is over — and not while a hold
    /// says the pass could change nothing (age-out ignores the hold: an expired line is
    /// always droppable).
    fn enforce_budget(&self) {
        let ledger = *self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        let aged = ledger
            .oldest_ended_ms
            .is_some_and(|oldest| now.saturating_sub(oldest) > self.limits.max_age_ms);
        let over =
            ledger.total_bytes > self.limits.max_total_bytes || ledger.runs > self.limits.max_runs;
        if !aged && !over {
            return;
        }
        if !aged && ledger.hold_until_ms.is_some_and(|until| now < until) {
            return;
        }
        self.evict(now);
    }

    /// Bring the budgets back under, oldest first, without touching a protected line
    /// (SPEC §remote.history). Two phases: whole records outside the window (their output files
    /// go with them; an expired one goes whatever the budgets say), then — for the
    /// byte budget only — the output files of the window's oldest runs, whose lines
    /// stay and are rewritten with `outputEvicted: true`. What is still over after that
    /// is protected, and the ledger holds the next pass until the oldest protected
    /// line leaves the window. The index is rewritten tmp + rename (the read handle is
    /// closed first, Windows refuses to replace an open file) only when something
    /// changed, and the ledger is rebuilt from what survived. Two streaming walks;
    /// never the whole index in memory.
    fn evict(&self, now: u64) {
        let _guard = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines: Vec<LineFacts> = Vec::new();
        let mut total = 0u64;
        {
            let Ok(file) = File::open(self.index_path()) else {
                return;
            };
            for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
                let bytes = line.len() as u64 + 1;
                if let Some(f) = LineFacts::parse(&line) {
                    total += bytes + f.output_bytes;
                    lines.push(LineFacts {
                        line_bytes: bytes,
                        ..f
                    });
                } else {
                    total += bytes; // a torn line is dropped by the rewrite below
                }
            }
        }
        let window_from = now.saturating_sub(self.limits.audit_window_ms);
        let protected = |f: &LineFacts| f.ended_ms >= window_from;
        let mut runs = lines.len();
        let mut dropped: HashSet<u64> = HashSet::new();
        // Phase 1: whole records, oldest first, never a protected one.
        for f in &lines {
            let aged = now.saturating_sub(f.ended_ms) > self.limits.max_age_ms;
            let over = total > self.limits.max_total_bytes || runs > self.limits.max_runs;
            if !aged && !over {
                break;
            }
            if !aged && protected(f) {
                continue;
            }
            total -= f.line_bytes + f.output_bytes;
            runs -= 1;
            dropped.insert(f.run_id);
        }
        // Phase 2: the byte budget against the window - output files only, oldest first.
        let mut stripped: HashSet<u64> = HashSet::new();
        for f in lines.iter().filter(|f| !dropped.contains(&f.run_id)) {
            if total <= self.limits.max_total_bytes {
                break;
            }
            if f.output_bytes == 0 {
                continue;
            }
            total -= f.output_bytes;
            stripped.insert(f.run_id);
        }
        // Still over: everything left is protected. Hold until the oldest of it ages
        // out of the window; nothing before then can change the answer.
        let still_over = total > self.limits.max_total_bytes || runs > self.limits.max_runs;
        let hold_until = still_over.then(|| {
            lines
                .iter()
                .filter(|f| !dropped.contains(&f.run_id))
                .map(|f| f.ended_ms.saturating_add(self.limits.audit_window_ms))
                .min()
                .unwrap_or(now + 60_000)
                .max(now + 1_000)
        });
        if dropped.is_empty() && stripped.is_empty() {
            drop(_guard);
            self.ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hold_until_ms = hold_until;
            return;
        }
        // Rewrite: keep every well-formed line not in the dropped set; a stripped line is
        // re-serialised with the eviction on it.
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        let rewrite = (|| -> std::io::Result<()> {
            let src = File::open(self.index_path())?;
            let mut out = BufWriter::new(File::create(&tmp)?);
            for line in BufReader::new(src).split(b'\n').map_while(Result::ok) {
                match LineFacts::parse(&line) {
                    Some(f) if dropped.contains(&f.run_id) => {}
                    Some(f) if stripped.contains(&f.run_id) => {
                        if let Some(Value::Object(mut m)) = parse_line(&line) {
                            m.insert("outputEvicted".into(), json!(true));
                            // The sealed write body is an evictable file like the output.
                            if m.get("contentStored") == Some(&json!(true)) {
                                m.insert("contentStored".into(), json!(false));
                                m.insert("contentEvicted".into(), json!(true));
                            }
                            // ... and so is the sealed exec stdin in the same slot: the
                            // line must not advertise a file this pass just deleted.
                            if m.get("stdinStored") == Some(&json!(true)) {
                                m.insert("stdinStored".into(), json!(false));
                                m.insert("stdinEvicted".into(), json!(true));
                            }
                            out.write_all(serde_json::to_string(&Value::Object(m))?.as_bytes())?;
                            out.write_all(b"\n")?;
                        }
                    }
                    Some(_) => {
                        out.write_all(&line)?;
                        out.write_all(b"\n")?;
                    }
                    None => {}
                }
            }
            out.flush()?;
            drop(out);
            std::fs::rename(&tmp, self.index_path())
        })();
        if rewrite.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        chmod_private(&self.index_path(), private_file_mode());
        for id in dropped.iter().chain(stripped.iter()) {
            let _ = std::fs::remove_file(self.output_path(*id));
            let _ = std::fs::remove_file(self.content_path(*id));
        }
        // Rebuild rather than trust the arithmetic: the rewrite is the truth now.
        drop(_guard);
        let mut ledger = self.scan(|_| {});
        ledger.hold_until_ms = hold_until;
        *self.ledger.lock().unwrap_or_else(|e| e.into_inner()) = ledger;
        // A pass that removed a line or evicted a file moved the answer; one that
        // changed nothing stays silent, so a read can never feed its own notice
        // (SPEC §remote.history).
        self.changed();
    }

    /// Append one finished run: the line, then the budgets. `Err` means the record is
    /// NOT durably visible — the acknowledged-completion contract turns that into an
    /// explicit status instead of the silent skip it used to be.
    fn record(
        &self,
        view: &RunView,
        request: &SubmitRequest,
        tee: &OutputFile,
    ) -> Result<Value, String> {
        let (output_bytes, tail) = tee.close();
        if output_bytes == 0 {
            let _ = std::fs::remove_file(self.output_path(view.run_id));
        }
        let (content_bytes, content_truncated) = self.seal_content(view.run_id, request);
        let (stdin_bytes, stdin_truncated) = self.seal_stdin(view.run_id, request);
        let mut line = match view.to_json(false) {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        line.insert("input".into(), sanitized_input(&request.input));
        line.insert("outputBytes".into(), json!(output_bytes));
        if let Some(tail) = tail {
            line.insert("outputCapped".into(), json!(true));
            line.insert("tail".into(), json!(tail));
        }
        if content_bytes > 0 {
            line.insert("contentStored".into(), json!(true));
            line.insert("contentFileBytes".into(), json!(content_bytes));
            if content_truncated {
                line.insert("contentTruncated".into(), json!(true));
            }
        }
        if stdin_bytes > 0 {
            line.insert("stdinStored".into(), json!(true));
            line.insert("stdinFileBytes".into(), json!(stdin_bytes));
            if stdin_truncated {
                line.insert("stdinTruncated".into(), json!(true));
            }
        }
        // The input is masked already; a label and an error are texts a caller typed too.
        mask_record(&mut line);
        let Ok(mut text) = serde_json::to_string(&line) else {
            return Err("serializing the run record failed".into());
        };
        text.push('\n');
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        mkdir_private(&self.dir);
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("appending {}: {e}", self.index_path().display()))?;
        chmod_private(&self.index_path(), private_file_mode());
        ledger.total_bytes += text.len() as u64 + output_bytes + content_bytes + stdin_bytes;
        ledger.runs += 1;
        if ledger.oldest_ended_ms.is_none() {
            ledger.oldest_ended_ms = view.ended_at_ms;
        }
        if output_bytes + content_bytes + stdin_bytes > 0 && ledger.total_bytes > self.limits.max_total_bytes {
            // The file just written is something a pass can evict: let it look.
            ledger.hold_until_ms = None;
        }
        drop(ledger);
        // Housekeeping past the durable line: the record IS visible; eviction that
        // cannot run now is the next pass's business, not a phantom write failure.
        self.enforce_budget();
        Ok(Value::Object(line))
    }
}

impl RunHistorySink for RunHistory {
    fn begin(&self, run_id: u64, request: &SubmitRequest) -> Option<Arc<dyn RunOutputTee>> {
        if !request.action_type.starts_with("remote.") {
            return None;
        }
        let out_dir = self.dir.join(OUT_DIR);
        mkdir_private(&out_dir);
        let path = self.output_path(run_id);
        // No file, no record of the stream — but the run's line is still written at
        // finish (with outputBytes 0), so the history never silently skips a run.
        let file = File::create(&path).ok().map(|f| {
            chmod_private(&path, private_file_mode());
            BufWriter::with_capacity(64 * 1024, f)
        });
        let tee = Arc::new(OutputFile {
            inner: Mutex::new(OutputFileInner {
                file,
                written: 0,
                cap: self.limits.max_output_bytes,
                overflow: None,
            }),
        });
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(run_id, (tee.clone(), sanitized_input(&request.input)));
        // The sink hook is synchronous: a store mid-attach simply gets no trail for this run.
        if let Ok(store) = self.store.try_read() {
            if let Some(store) = store.as_ref() {
                self.trails.follow(run_id, &request.input, store.clone());
            }
        }
        Some(tee)
    }

    fn finish<'a>(
        &'a self,
        view: RunView,
        request: SubmitRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HistoryAck> + Send + 'a>> {
        Box::pin(async move {
            let tee = self
                .open
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&view.run_id);
            let Some((tee, _)) = tee else {
                // Never announced to us (begin declined it) — nothing was owed, so the
                // honest acknowledgment is Recorded, not a phantom failure.
                return HistoryAck::Recorded;
            };
            let line = match self.record(&view, &request, &tee) {
                Ok(line) => line,
                Err(reason) => return HistoryAck::Unavailable { reason },
            };
            // The module database (store.rs): the spill above already made the
            // record recoverable; the acknowledged commit is the database's — a
            // failure there is an honest Unavailable, never a silent skip.
            if let Some(history) = self.attached().await {
                let out = self.output_path(view.run_id);
                let content = self.content_path(view.run_id);
                return match history
                    .record_run(&line, Some(&out), Some(&content))
                    .await
                {
                    // The notice leaves only once the change is visible through the
                    // query authority (SPEC §bus.topics) — the database's commit,
                    // when it holds the log; the spill line otherwise.
                    Ok(()) => {
                        self.changed();
                        HistoryAck::Recorded
                    }
                    Err(reason) => HistoryAck::Unavailable { reason },
                };
            }
            self.changed();
            HistoryAck::Recorded
        })
    }

    /// The highest id on file - the floor the coordinator numbers from once this record
    /// is attached, so a restart never files two runs under one number. The maximum,
    /// not the last line: a record written by two numberings has its highest id
    /// anywhere. One streaming walk, once per plugin start.
    fn last_run_id(&self) -> Option<u64> {
        let mut last = None;
        self.scan(|f| last = Some(last.map_or(f.run_id, |l: u64| l.max(f.run_id))));
        let floor = self.run_floor.load(std::sync::atomic::Ordering::SeqCst);
        last.map(|l| l.max(floor)).or((floor > 0).then_some(floor))
    }
}

/// The facts the budget pass needs from one line, without keeping the line.
#[derive(Debug, Clone)]
struct LineFacts {
    run_id: u64,
    ended_ms: u64,
    /// What the run's evictable files hold on disk - the output file, plus a write's sealed
    /// body: zero for each once a pass evicted it, whatever the line's `outputBytes` (the
    /// size the run produced) still says.
    output_bytes: u64,
    line_bytes: u64,
}

impl LineFacts {
    fn parse(line: &[u8]) -> Option<Self> {
        let v = parse_line(line)?;
        let run_id = v.get("runId")?.as_u64()?;
        let ended_ms = record_ended_ms(&v);
        let evicted = v
            .get("outputEvicted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output_bytes = if evicted {
            0
        } else {
            v.get("outputBytes").and_then(Value::as_u64).unwrap_or(0)
        };
        let content_bytes = if v.get("contentStored").and_then(Value::as_bool) == Some(true) {
            v.get("contentFileBytes")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        } else {
            0
        };
        // The sealed exec stdin rides the same evictable file slot as a write body,
        // so a rebuilt ledger must count its bytes too - or the byte budget
        // undercounts and a stdin-only row can never be reclaimed.
        let stdin_bytes = if v.get("stdinStored").and_then(Value::as_bool) == Some(true) {
            v.get("stdinFileBytes").and_then(Value::as_u64).unwrap_or(0)
        } else {
            0
        };
        let output_bytes = output_bytes + content_bytes + stdin_bytes;
        Some(LineFacts {
            run_id,
            ended_ms,
            output_bytes,
            line_bytes: line.len() as u64 + 1,
        })
    }
}

fn parse_line(line: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(line).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(text).ok()
}

/// When a record ended, epoch ms; a line without the field reads as the beginning of
/// time (the age budget treats it the same way).
fn record_ended_ms(v: &Value) -> u64 {
    v.get("endedAt")
        .and_then(Value::as_str)
        .and_then(parse_iso_ms)
        .and_then(|ms| u64::try_from(ms).ok())
        .unwrap_or(0)
}

/// The target a record ran against: the outcome's meta first (what actually resolved),
/// the input second (a run that failed before resolving still names what was asked).
fn record_target(v: &Value) -> Option<&str> {
    v.get("meta")
        .and_then(|m| m.get("target"))
        .or_else(|| v.get("input").and_then(|i| i.get("target")))
        .and_then(Value::as_str)
}

/// The clear body a pre-2026-09-28 line carries, when it has one. A line written since
/// keeps `input.contentBytes` and never `input.content`, so the field is the whole marker.
fn legacy_body(line: &Map<String, Value>) -> Option<&str> {
    line.get("input")?.get("content")?.as_str()
}

/// The submission's input with every `env` object reduced to its key list: an exec's
/// env pairs are user-provided and may carry a token; the record says WHICH variables
/// were set, never what to. Bodies - a write's content, an exec's stdin - shrink to
/// their byte counts. Every other text - an exec's argv and cwd, a file action's paths -
/// keeps its words with any credential literal's value masked ([masked_literals]).
fn sanitized_input(input: &Value) -> Value {
    let Value::Object(obj) = input else {
        return Value::Null;
    };
    let mut out = Map::new();
    for (k, v) in obj {
        if k == "env" {
            let keys: Vec<&String> = match v {
                Value::Object(env) => env.keys().collect(),
                _ => Vec::new(),
            };
            out.insert("envKeys".into(), json!(keys));
        } else if k == "content" {
            // remote.write's file body: the audit keeps that a file was written and how
            // big, never what (a .env, a key, a password file) - 2026-09-28.
            let bytes = v.as_str().map(str::len).unwrap_or(0);
            out.insert("contentBytes".into(), json!(bytes));
        } else if k == "stdin" {
            // remote.exec's stdin: a script or SQL that may hold a password in clear. Same
            // rule as a write's body - how much was sent, never what.
            let bytes = v.as_str().map(str::len).unwrap_or(0);
            out.insert("stdinBytes".into(), json!(bytes));
        } else {
            out.insert(k.clone(), masked_literals(v));
        }
    }
    Value::Object(out)
}

/// A recorded field with every credential literal's value masked (SPEC §remote.security): a
/// text alone, an array of texts read as the one command line an argv makes. remote.exec
/// refuses a literal in argv or cwd, but a run refused is still recorded, and records written
/// before the refusal hold them - [RunHistory::open] masks those with this same function.
fn masked_literals(value: &Value) -> Value {
    use swiss_core::secure::literal;
    match value {
        Value::String(text) => Value::String(literal::mask(text)),
        Value::Array(items) if items.iter().all(Value::is_string) => {
            let words: Vec<String> = items.iter().filter_map(Value::as_str).map(str::to_string).collect();
            json!(literal::mask_argv(&words))
        }
        other => other.clone(),
    }
}

/// [masked_literals] over every text of a record a caller typed: its input's fields, its
/// label and its error. Whether anything changed - an open-time pass rewrites only then.
pub(crate) fn mask_record(line: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    let mut mask = |value: &mut Value| {
        let masked = masked_literals(value);
        if masked != *value {
            *value = masked;
            changed = true;
        }
    };
    if let Some(Value::Object(input)) = line.get_mut("input") {
        input.values_mut().for_each(&mut mask);
    }
    for key in ["label", "error"] {
        if let Some(value) = line.get_mut(key) {
            mask(value);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_host::services::runs::RunState;

    fn scratch() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("swiss-rhist-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn request(action: &str, input: Value) -> SubmitRequest {
        SubmitRequest {
            owner: "remote".into(),
            label: "build".into(),
            action_type: action.into(),
            input,
            timeout_ms: 60_000,
            queue_if_busy: false,
            actor: "test".to_string(),
        }
    }

    fn view(run_id: u64, ended_at_ms: u64) -> RunView {
        let mut meta = Map::new();
        meta.insert("target".into(), json!("build"));
        RunView {
            run_id,
            owner: "remote".into(),
            label: "build".into(),
            actor: "test".to_string(),
            action_type: "remote.exec".into(),
            state: RunState::Succeeded,
            queued_at_ms: ended_at_ms.saturating_sub(10),
            started_at_ms: Some(ended_at_ms.saturating_sub(10)),
            ended_at_ms: Some(ended_at_ms),
            ms: Some(10),
            pid: None,
            exit_code: Some(0),
            timed_out: false,
            canceled: false,
            error: None,
            output: Some("head".into()),
            chars: Some(4),
            output_truncated: false,
            meta,
        }
    }

    /// One whole run through the sink: begin, stream, finish.
    fn run(history: &RunHistory, run_id: u64, output: &[u8], ended_at_ms: u64) {
        let req = request(
            "remote.exec",
            json!({ "target": "build", "argv": ["make"] }),
        );
        let tee = history
            .begin(run_id, &req)
            .expect("a remote run is recorded");
        if !output.is_empty() {
            tee.write(output);
        }
        finish_recorded(history, view(run_id, ended_at_ms), req);
    }

    /// Drive the acknowledged finish to completion the way the coordinator's bounded
    /// await would, and hold it to its contract: these records owed a line each.
    fn finish_recorded(history: &RunHistory, view: RunView, req: SubmitRequest) {
        let ack = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a minimal runtime for the sink's finish")
            .block_on(history.finish(view, req));
        assert_eq!(ack, HistoryAck::Recorded);
    }

    fn write(history: &RunHistory, run_id: u64, content: &str) {
        let req = request(
            "remote.write",
            json!({ "target": "build", "remote": ".env", "content": content }),
        );
        let _tee = history.begin(run_id, &req).expect("recorded");
        finish_recorded(history, view(run_id, now_ms()), req);
    }

    /// One record put back the way the log wrote it BEFORE 2026-09-28: the body in clear
    /// under `input.content`, no `contentBytes`, no sealed file beside it.
    fn make_legacy(dir: &std::path::Path, run_id: u64, content: &str) {
        let path = dir.join(INDEX_FILE);
        let raw = std::fs::read_to_string(&path).expect("the index");
        let lines: Vec<String> = raw
            .lines()
            .map(|line| {
                let Some(Value::Object(mut m)) = parse_line(line.as_bytes()) else {
                    return line.to_string();
                };
                if m.get("runId").and_then(Value::as_u64) != Some(run_id) {
                    return line.to_string();
                }
                m.remove("contentStored");
                m.remove("contentFileBytes");
                m.remove("contentTruncated");
                if let Some(Value::Object(input)) = m.get_mut("input") {
                    input.remove("contentBytes");
                    input.insert("content".into(), json!(content));
                }
                serde_json::to_string(&Value::Object(m)).expect("a line")
            })
            .collect();
        std::fs::write(&path, lines.join("\n") + "\n").expect("rewrite");
        let _ = std::fs::remove_file(dir.join(OUT_DIR).join(format!("{run_id}.content")));
    }

    #[test]
    fn a_remote_write_keeps_its_content_sealed_beside_the_record() {
        // The owner's two asks, both kept: see what a write wrote (2026-09-28, "I don't
        // know what was written"), and never a written password on disk in clear.
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        write(&history, 9, "DB_PASSWORD=s3cret\n");
        let (page, _) = history.page(None, 20, None);
        assert_eq!(page[0]["input"]["remote"], ".env");
        assert_eq!(page[0]["input"]["contentBytes"], 19);
        assert!(page[0]["input"].get("content").is_none());
        assert_eq!(page[0]["contentStored"], true);
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(!raw.contains("s3cret"), "{raw}");
        let sealed = std::fs::read_to_string(dir.join(OUT_DIR).join("9.content")).unwrap();
        assert!(!sealed.contains("s3cret"), "the body is sealed: {sealed}");
        let kept = history.content(9).expect("opens").expect("kept");
        assert_eq!(kept.text, "DB_PASSWORD=s3cret\n");
        assert!(!kept.truncated);
        // Not a write, no body.
        run(&history, 10, b"ok", now_ms());
        assert_eq!(history.content(10), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_write_keeps_its_head_cut_on_a_character_boundary() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        // Two-byte characters straddle the cap: the cut lands on a boundary, below it.
        let body = "\u{e9}".repeat(CONTENT_MAX_BYTES / 2 + 10);
        write(&history, 3, &body);
        let kept = history.content(3).expect("opens").expect("kept");
        assert!(kept.truncated);
        assert!(kept.text.len() <= CONTENT_MAX_BYTES);
        assert!(body.starts_with(&kept.text));
        let (page, _) = history.page(None, 20, None);
        assert_eq!(page[0]["contentTruncated"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sealed_body_counts_against_the_budget_and_leaves_with_its_record() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        write(&history, 1, "a=1\n");
        let on_disk = std::fs::metadata(dir.join(OUT_DIR).join("1.content"))
            .unwrap()
            .len();
        let index = std::fs::metadata(dir.join(INDEX_FILE)).unwrap().len();
        assert_eq!(
            history.usage(),
            (index + on_disk, 1),
            "the body is in the budget"
        );
        assert_eq!(
            RunHistory::open(dir.clone()).usage(),
            (index + on_disk, 1),
            "and a reopen counts it the same"
        );
        history.clear();
        assert!(!dir.join(OUT_DIR).join("1.content").exists());
        assert_eq!(history.content(1), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_records_clear_body_is_sealed_when_the_log_opens() {
        // The owner, on a write recorded before the sealing landed: "this still isn't
        // solved." Its body was never lost - it sat in the index in CLEAR, which is what
        // 9a6aebe set out to stop, and the panel shows a sealed body only. Opening the log
        // moves it across: the panel can read it, runs.jsonl no longer holds it.
        let dir = scratch();
        {
            let history = RunHistory::open(dir.clone());
            write(&history, 10, "DB_PASSWORD=s3cret\n");
            run(&history, 11, b"ok", now_ms());
        }
        make_legacy(&dir, 10, "DB_PASSWORD=s3cret\n");
        let before = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(
            before.contains("s3cret"),
            "the fixture is the old shape: {before}"
        );

        let history = RunHistory::open(dir.clone());
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(
            !raw.contains("s3cret"),
            "the clear body left the index: {raw}"
        );
        let kept = history.content(10).expect("opens").expect("sealed at open");
        assert_eq!(kept.text, "DB_PASSWORD=s3cret\n");
        assert!(!kept.truncated);
        let (page, _) = history.page(None, 20, None);
        let line = page.iter().find(|v| v["runId"] == 10).expect("the record");
        assert_eq!(line["contentStored"], true);
        assert_eq!(line["input"]["contentBytes"], 19);
        assert!(line["input"].get("content").is_none());
        assert_eq!(
            line["input"]["remote"], ".env",
            "the record is otherwise untouched"
        );
        // And the sealed file is in the budget, like one written today.
        let sealed = std::fs::metadata(dir.join(OUT_DIR).join("10.content"))
            .unwrap()
            .len();
        let index = std::fs::metadata(dir.join(INDEX_FILE)).unwrap().len();
        let out11 = std::fs::metadata(dir.join(OUT_DIR).join("11.txt"))
            .unwrap()
            .len();
        assert_eq!(history.usage(), (index + sealed + out11, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_seal_at_open_leaves_a_record_it_has_already_moved_alone() {
        // Every seal draws a fresh nonce, so a second pass would rewrite both files. A
        // restart is not a rewrite: the pass is for lines carrying a clear body, only.
        let dir = scratch();
        {
            let history = RunHistory::open(dir.clone());
            write(&history, 4, "a=1\n");
        }
        make_legacy(&dir, 4, "a=1\n");
        let _ = RunHistory::open(dir.clone());
        let sealed = std::fs::read(dir.join(OUT_DIR).join("4.content")).unwrap();
        let index = std::fs::read(dir.join(INDEX_FILE)).unwrap();

        let history = RunHistory::open(dir.clone());
        assert_eq!(
            std::fs::read(dir.join(OUT_DIR).join("4.content")).unwrap(),
            sealed,
            "the body was not sealed a second time"
        );
        assert_eq!(
            std::fs::read(dir.join(INDEX_FILE)).unwrap(),
            index,
            "and the index was not rewritten"
        );
        assert_eq!(history.content(4).unwrap().unwrap().text, "a=1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An exec's stdin is a script or a batch of SQL - it may carry a password typed in
    /// clear, and it is up to 1 MiB. The record keeps how much was sent, never what.
    #[test]
    fn an_exec_stdin_is_recorded_as_its_size_only() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let stdin = "ALTER USER app PASSWORD 'hunter2-\u{e9}';\n";
        let req = request(
            "remote.exec",
            json!({ "target": "db", "argv": ["psql", "-X"], "stdin": stdin }),
        );
        let _tee = history.begin(9, &req).expect("recorded");
        finish_recorded(&history, view(9, now_ms()), req);

        let (page, _) = history.page(None, 20, None);
        assert_eq!(page[0]["input"]["stdinBytes"], stdin.len());
        assert!(page[0]["input"].get("stdin").is_none(), "{}", page[0]["input"]);
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(!raw.contains("hunter2"), "{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_remote_run_is_teed_to_disk_and_recorded_with_env_keys_only() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let req = request(
            "remote.exec",
            json!({ "target": "build", "argv": ["make", "-j8"], "env": { "TOKEN": "s3cret" } }),
        );
        let tee = history.begin(7, &req).expect("recorded");
        tee.write(b"line one\n");
        tee.write(b"line two\n");
        finish_recorded(&history, view(7, now_ms()), req);

        let (page, next) = history.page(None, 20, None);
        assert_eq!(page.len(), 1);
        assert!(next.is_none());
        let row = &page[0];
        assert_eq!(row["runId"], 7);
        assert_eq!(row["state"], "succeeded");
        assert_eq!(
            row["actor"], "test",
            "who ran it is on the line (SPEC §remote.history)"
        );
        assert_eq!(row["outputBytes"], 18);
        assert_eq!(row["meta"]["target"], "build");
        assert_eq!(row["input"]["argv"], json!(["make", "-j8"]));
        assert_eq!(row["input"]["envKeys"], json!(["TOKEN"]));
        assert!(
            row["input"].get("env").is_none(),
            "values never land on disk"
        );
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(!raw.contains("s3cret"), "{raw}");

        let chunk = history.output(7, 0, usize::MAX).expect("the stream");
        assert_eq!(chunk.text, "line one\nline two\n");
        assert_eq!(chunk.total, 18);
        assert_eq!(chunk.next_cursor, 18);
        // A cursor read continues where the last one ended.
        let more = history.output(7, 9, 4).unwrap();
        assert_eq!(more.text, "line");
        assert_eq!(more.next_cursor, 13);
        assert_eq!(history.get(7).unwrap()["runId"], 7);
        assert!(history.get(8).is_none());
        assert_eq!(history.usage().1, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_credential_typed_into_a_command_is_recorded_masked_and_old_lines_are_masked_at_open() {
        // 2026-10-21: a redis password typed into a `bash -c` word on target test sat in
        // every record of those runs. Synthetic values here.
        use swiss_core::secure::literal::MASK;
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let script = "R(){ sudo -n docker exec -e REDISCLI_AUTH=Xq7pLm2Rv9 redis redis-cli \"$@\"; }; R ping";
        let req = request(
            "remote.exec",
            json!({ "target": "build", "argv": ["bash", "-c", script], "cwd": "/srv/x?token=Xq7pLm2Rv9" }),
        );
        let _tee = history.begin(11, &req).expect("recorded");
        let mut refused = view(11, now_ms());
        refused.label = "redis-cli -a Xq7pLm2Rv9 ping".into();
        refused.error = Some("exit 1 after --password=Xq7pLm2Rv9".into());
        finish_recorded(&history, refused, req);
        run(&history, 12, b"PONG\n", now_ms());
        let row = history.get(11).expect("the record");
        assert_eq!(
            row["input"]["argv"][2],
            json!(format!("R(){{ sudo -n docker exec -e REDISCLI_AUTH={MASK} redis redis-cli \"$@\"; }}; R ping"))
        );
        assert_eq!(row["input"]["cwd"], json!(format!("/srv/x?token={MASK}")));
        assert_eq!(row["label"], json!(format!("redis-cli -a {MASK} ping")));
        assert_eq!(row["error"], json!(format!("exit 1 after --password={MASK}")));
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(!raw.contains("Xq7pLm2Rv9"), "{raw}");

        // The lines as a log before the fix kept them: the value in clear, and a torn tail.
        std::fs::write(dir.join(INDEX_FILE), raw.replace(MASK, "Xq7pLm2Rv9") + "{\"runId\":13,").unwrap();
        drop(history);
        let history = RunHistory::open(dir.clone());
        let raw = std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap();
        assert!(!raw.contains("Xq7pLm2Rv9"), "{raw}");
        assert_eq!(history.get(11).expect("kept")["input"]["argv"][2], row["input"]["argv"][2]);
        assert_eq!(history.get(12).expect("untouched")["input"]["argv"], json!(["make"]));
        assert_eq!(history.output(12, 0, usize::MAX).expect("its output").text, "PONG\n");
        // Nothing left to mask: the next open leaves the file as it is.
        drop(history);
        let _history = RunHistory::open(dir.clone());
        assert_eq!(std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap(), raw);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_producers_runs_are_declined() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let req = request("shell.run", json!({}));
        assert!(history.begin(1, &req).is_none());
        // A finish for a run never begun is a no-op, not a phantom line.
        // A finish for a run never begun acknowledges cleanly, not a phantom line.
        finish_recorded(&history, view(1, now_ms()), req);
        assert_eq!(history.page(None, 20, None).0.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_silent_run_is_recorded_without_an_output_file() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        run(&history, 3, b"", now_ms());
        assert!(!dir.join(OUT_DIR).join("3.txt").exists());
        assert_eq!(history.get(3).unwrap()["outputBytes"], 0);
        assert!(history.output(3, 0, 10).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_output_file_caps_and_the_record_keeps_the_tail() {
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_output_bytes: 100,
                ..Limits::default()
            },
        );
        let req = request("remote.exec", json!({ "target": "build" }));
        let tee = history.begin(9, &req).unwrap();
        for i in 0..30 {
            tee.write(format!("row {i:02} ....\n").as_bytes()); // 12 bytes each, 360 total
        }
        finish_recorded(&history, view(9, now_ms()), req);
        let chunk = history.output(9, 0, usize::MAX).unwrap();
        assert_eq!(chunk.total, 100, "the file stops at the cap");
        assert!(chunk.text.starts_with("row 00"));
        let row = history.get(9).unwrap();
        assert_eq!(row["outputCapped"], true);
        assert_eq!(row["outputBytes"], 100);
        let tail = row["tail"].as_str().unwrap();
        assert!(tail.ends_with("row 29 ....\n"), "{tail:?}");
        assert_eq!(tail.len(), 260, "everything past the cap, under TAIL_BYTES");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const DAY: u64 = 24 * 60 * 60 * 1000;

    /// The index file's bytes - what a pass rewrote, or did not.
    fn index_bytes(dir: &std::path::Path) -> Vec<u8> {
        std::fs::read(dir.join(INDEX_FILE)).unwrap_or_default()
    }

    #[test]
    fn the_byte_budget_evicts_the_oldest_with_its_output_file() {
        // Outside the seven-day window (SPEC §remote.history) the budgets work as they always
        // did: whole records go, oldest first.
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_total_bytes: 2_000,
                ..Limits::default()
            },
        );
        let body = vec![b'x'; 500];
        let base = now_ms() - 8 * DAY;
        for id in 1..=6 {
            run(&history, id, &body, base + id);
        }
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert!(ids.len() < 6, "something was evicted: {ids:?}");
        assert_eq!(ids[0], 6, "newest first, newest kept");
        assert!(!ids.contains(&1), "the oldest went first");
        assert!(!dir.join(OUT_DIR).join("1.txt").exists());
        assert!(dir.join(OUT_DIR).join("6.txt").exists());
        let (bytes, runs) = history.usage();
        assert!(bytes <= 2_000, "{bytes}");
        assert_eq!(runs, ids.len());
        // A fresh open rebuilds the same ledger from the file.
        let reopened = RunHistory::open(dir.clone());
        assert_eq!(reopened.usage(), (bytes, runs));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_run_count_budget_holds() {
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_runs: 3,
                ..Limits::default()
            },
        );
        let base = now_ms() - 8 * DAY;
        for id in 1..=5 {
            run(&history, id, b"hi\n", base + id);
        }
        let (page, next) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![5, 4, 3]);
        assert!(next.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_window_keeps_every_line_under_byte_pressure_and_gives_up_output_files_oldest_first() {
        // SPEC §remote.history: six runs today, 500 bytes each (a line is ~300 more), a 3.5 KB
        // budget. Before: half of them gone. Now: all six lines stay; the oldest
        // output files go, the lines say so, and the budget holds on what is on disk.
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_total_bytes: 3_500,
                ..Limits::default()
            },
        );
        let body = vec![b'x'; 500];
        let base = now_ms();
        for id in 1..=6 {
            run(&history, id, &body, base + id);
        }
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(
            ids,
            vec![6, 5, 4, 3, 2, 1],
            "no line inside the window is dropped"
        );
        let evicted: Vec<u64> = page
            .iter()
            .filter(|v| v["outputEvicted"] == true)
            .map(|v| v["runId"].as_u64().unwrap())
            .collect();
        assert!(!evicted.is_empty(), "the byte budget had to take something");
        assert!(
            evicted.contains(&1),
            "the oldest output goes first: {evicted:?}"
        );
        assert!(
            !evicted.contains(&6),
            "the newest keeps its file: {evicted:?}"
        );
        // The oldest evicted, the newest kept: no gap in the middle.
        let newest_evicted = *evicted.iter().max().unwrap();
        for id in 1..=6u64 {
            let on_disk = dir.join(OUT_DIR).join(format!("{id}.txt")).exists();
            assert_eq!(on_disk, id > newest_evicted, "run {id}");
        }
        // The line still says what the run produced; the file is what is gone.
        let one = history.get(1).unwrap();
        assert_eq!(one["outputBytes"], 500);
        assert_eq!(one["outputEvicted"], true);
        assert!(history.output(1, 0, 10).is_none());
        assert!(history.output(6, 0, 10).is_some());
        let (bytes, runs) = history.usage();
        assert!(bytes <= 3_500, "{bytes}");
        assert_eq!(runs, 6);
        // A fresh open counts an evicted output as the zero bytes it is on disk.
        let reopened = RunHistory::open(dir.clone());
        assert_eq!(reopened.usage(), (bytes, runs));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_outside_the_window_go_whole_before_any_output_inside_it() {
        // Three runs eight days ago, three today, 500 bytes each (~800 with the line),
        // a 3 KB budget the six overshoot and the window's three fit: the old ones are
        // dropped (files and lines), the window keeps every file.
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_total_bytes: 3_000,
                ..Limits::default()
            },
        );
        let body = vec![b'x'; 500];
        let old = now_ms() - 8 * DAY;
        let today = now_ms();
        for id in 1..=3 {
            run(&history, id, &body, old + id);
        }
        for id in 4..=6 {
            run(&history, id, &body, today + id);
        }
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert!(!ids.contains(&1), "{ids:?}");
        assert!(
            ids.contains(&6) && ids.contains(&5) && ids.contains(&4),
            "{ids:?}"
        );
        for id in 4..=6u64 {
            assert!(
                dir.join(OUT_DIR).join(format!("{id}.txt")).exists(),
                "run {id}"
            );
            assert!(history.get(id).unwrap().get("outputEvicted").is_none());
        }
        assert!(history.usage().0 <= 3_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_count_cap_yields_to_the_window_and_the_pass_then_holds() {
        // Five runs today against a cap of three: all five stay (the cap is for the
        // index's size, and five lines are no size), and the next reads do not rewrite
        // the index over and over - the pass holds until a line leaves the window.
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_runs: 3,
                ..Limits::default()
            },
        );
        let base = now_ms();
        for id in 1..=5 {
            run(&history, id, b"hi\n", base + id);
        }
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1]);
        assert_eq!(history.usage().1, 5);
        let hold = history.ledger.lock().unwrap().hold_until_ms;
        assert!(hold.is_some_and(|until| until > now_ms()), "{hold:?}");
        let before = index_bytes(&dir);
        let mtime = std::fs::metadata(dir.join(INDEX_FILE))
            .unwrap()
            .modified()
            .unwrap();
        for _ in 0..3 {
            history.page(None, 20, None);
        }
        assert_eq!(index_bytes(&dir), before);
        assert_eq!(
            std::fs::metadata(dir.join(INDEX_FILE))
                .unwrap()
                .modified()
                .unwrap(),
            mtime,
            "no rewrite while the hold stands"
        );
        // Two old runs behind them: those go, the five stay, the cap is still over.
        run(&history, 6, b"old\n", base - 8 * DAY);
        run(&history, 7, b"old\n", base - 8 * DAY + 1);
        assert_eq!(
            history.usage().1,
            7,
            "the hold stands: the appends did not rewrite"
        );
        history.ledger.lock().unwrap().hold_until_ms = None; // the window moved on
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(
            ids,
            vec![5, 4, 3, 2, 1],
            "the old two went, the window stayed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_output_file_under_byte_pressure_lifts_the_hold() {
        // A budget the index alone overshoots: every output file in the window is
        // already gone and the pass holds. The next run's file is still evictable, so
        // its record lifts the hold and the pass takes that file too - the promise is
        // the line, never the bytes.
        let dir = scratch();
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_total_bytes: 100,
                ..Limits::default()
            },
        );
        let base = now_ms();
        run(&history, 1, b"one\n", base + 1);
        run(&history, 2, b"two\n", base + 2);
        assert!(history.ledger.lock().unwrap().hold_until_ms.is_some());
        assert!(!dir.join(OUT_DIR).join("1.txt").exists());
        assert!(!dir.join(OUT_DIR).join("2.txt").exists());
        run(&history, 3, b"three\n", base + 3);
        assert!(
            !dir.join(OUT_DIR).join("3.txt").exists(),
            "the new file went too"
        );
        assert_eq!(history.get(3).unwrap()["outputEvicted"], true);
        assert_eq!(history.usage().1, 3, "and every line is still there");
        // A silent run adds nothing a pass could take: the hold stands, no rewrite.
        let before = index_bytes(&dir);
        run(&history, 4, b"", base + 4);
        let after = index_bytes(&dir);
        assert!(
            after.starts_with(&before),
            "append only: {}",
            String::from_utf8_lossy(&after)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_records_age_out_on_the_next_read() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let old = now_ms() - MAX_AGE_MS - 60_000;
        run(&history, 1, b"old\n", old);
        run(&history, 2, b"new\n", now_ms());
        // The append after the old one already found it expired: only the new remains.
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![2]);
        assert!(!dir.join(OUT_DIR).join("1.txt").exists());
        assert!(dir.join(OUT_DIR).join("2.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pages_walk_newest_first_with_a_cursor_and_a_target_filter() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let base = now_ms();
        for id in 1..=7 {
            let target = if id % 2 == 0 { "even" } else { "odd" };
            let req = request("remote.exec", json!({ "target": target }));
            let tee = history.begin(id, &req).unwrap();
            tee.write(b"x");
            let mut v = view(id, base + id);
            v.meta.insert("target".into(), json!(target));
            finish_recorded(&history, v, req);
        }
        let (first, next) = history.page(None, 3, None);
        let ids: Vec<u64> = first.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![7, 6, 5]);
        assert_eq!(next, Some(5));
        let (second, next) = history.page(next, 3, None);
        let ids: Vec<u64> = second
            .iter()
            .map(|v| v["runId"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![4, 3, 2]);
        assert_eq!(next, Some(2));
        let (third, next) = history.page(next, 3, None);
        let ids: Vec<u64> = third.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![1]);
        assert!(next.is_none());
        let (odd, _) = history.page(None, 20, Some("odd"));
        let ids: Vec<u64> = odd.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![7, 5, 3, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_query_narrows_by_window_and_actor_and_stops_walking_at_since() {
        // Seven runs a minute apart, actors alternating: the window picks the middle,
        // the actor picks its half, and the two compose. `since` ends the walk rather
        // than filtering it, so the page below the window costs nothing.
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let base = now_ms() - 10 * 60_000;
        for id in 1..=7u64 {
            let req = request("remote.exec", json!({ "target": "build" }));
            let tee = history.begin(id, &req).unwrap();
            tee.write(b"x");
            let mut v = view(id, base + id * 60_000);
            v.actor = if id % 2 == 0 {
                "mcp:claude".into()
            } else {
                "cli:jdoe@box".into()
            };
            finish_recorded(&history, v, req);
        }
        let ids = |page: &[Value]| -> Vec<u64> {
            page.iter().map(|v| v["runId"].as_u64().unwrap()).collect()
        };
        let q = PageQuery {
            limit: 20,
            ..PageQuery::default()
        };
        // since inclusive, until exclusive, both against endedAt.
        let (page, next) = history.query(PageQuery {
            since: Some(base + 3 * 60_000),
            until: Some(base + 6 * 60_000),
            ..q
        });
        assert_eq!(ids(&page), vec![5, 4, 3]);
        assert!(next.is_none());
        let (page, _) = history.query(PageQuery {
            actor: Some("mcp:claude"),
            ..q
        });
        assert_eq!(ids(&page), vec![6, 4, 2]);
        let (page, _) = history.query(PageQuery {
            since: Some(base + 3 * 60_000),
            actor: Some("cli:jdoe@box"),
            ..q
        });
        assert_eq!(ids(&page), vec![7, 5, 3]);
        // The cursor still pages inside a window.
        let (page, next) = history.query(PageQuery {
            since: Some(base + 2 * 60_000),
            limit: 2,
            ..q
        });
        assert_eq!(ids(&page), vec![7, 6]);
        assert_eq!(next, Some(6));
        let (page, next) = history.query(PageQuery {
            since: Some(base + 2 * 60_000),
            limit: 2,
            before: next,
            ..q
        });
        assert_eq!(ids(&page), vec![5, 4]);
        assert_eq!(next, Some(4));
        let (page, next) = history.query(PageQuery {
            since: Some(base + 2 * 60_000),
            limit: 2,
            before: next,
            ..q
        });
        assert_eq!(ids(&page), vec![3, 2]);
        assert!(
            next.is_none(),
            "the walk stopped at since: nothing older is offered"
        );
        // A window with nothing in it is an empty page, not an error.
        let (page, _) = history.query(PageQuery {
            until: Some(base),
            ..q
        });
        assert!(page.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_record_reports_its_highest_run_id_as_the_numbering_floor() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        assert_eq!(history.last_run_id(), None, "an empty record has no floor");
        // Two numberings interleaved on file: 0..3 from one process, 0..1 from the next
        // (the pre-sequence world) - the floor is the maximum, wherever it sits.
        let base = now_ms();
        for (id, at) in [(0u64, 1u64), (1, 2), (2, 3), (3, 4), (0, 5), (1, 6)] {
            run(&history, id, b"x", base + at);
        }
        assert_eq!(history.last_run_id(), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instants_parse_as_epoch_ms_or_iso() {
        assert_eq!(parse_instant("1700000000000"), Some(1_700_000_000_000));
        assert_eq!(parse_instant(" 1700000000000 "), Some(1_700_000_000_000));
        assert_eq!(
            parse_instant("2023-11-14T22:13:20Z"),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            parse_instant("2023-11-14T22:13:20.000Z"),
            Some(1_700_000_000_000)
        );
        assert_eq!(parse_instant(""), None);
        assert_eq!(
            parse_instant("7d"),
            None,
            "relative spellings are the CLI's, not the API's"
        );
        assert_eq!(parse_instant("yesterday"), None);
    }

    #[test]
    fn orphaned_output_files_are_removed_at_open_and_clear_forgets_everything() {
        let dir = scratch();
        {
            let history = RunHistory::open(dir.clone());
            run(&history, 1, b"kept\n", now_ms());
            write(&history, 2, "kept=1\n");
            // A crash mid-run: files the index never learned about, of both kinds.
            std::fs::write(dir.join(OUT_DIR).join("99.txt"), b"orphan").unwrap();
            std::fs::write(dir.join(OUT_DIR).join("98.content"), b"orphan").unwrap();
        }
        let history = RunHistory::open(dir.clone());
        assert!(!dir.join(OUT_DIR).join("99.txt").exists());
        assert!(!dir.join(OUT_DIR).join("98.content").exists());
        assert!(dir.join(OUT_DIR).join("1.txt").exists());
        assert!(dir.join(OUT_DIR).join("2.content").exists());
        history.clear();
        assert_eq!(history.usage(), (0, 0));
        assert!(history.page(None, 20, None).0.is_empty());
        assert!(!dir.join(OUT_DIR).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A counting notice: what every notices test holds against the log.
    fn counting_notice() -> (Arc<std::sync::atomic::AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let closure = {
            let count = count.clone();
            Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }) as Arc<dyn Fn() + Send + Sync>
        };
        (count, closure)
    }

    fn notices(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn a_record_filed_publishes_once_and_a_read_publishes_nothing() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let (count, notice) = counting_notice();
        history.set_notice(notice);

        run(&history, 1, b"kept\n", now_ms());
        assert_eq!(notices(&count), 1, "the filing is announced");
        run(&history, 2, b"also kept\n", now_ms());
        assert_eq!(notices(&count), 2, "each filing once");

        // The read path runs the O(1) budget check on every page: a pass that
        // changed nothing publishes nothing, or the notice would feed its own
        // read forever (SPEC §remote.history).
        assert!(!history.page(None, 20, None).0.is_empty());
        assert_eq!(notices(&count), 2, "a read announces nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_publishes_when_the_record_held_something_and_only_then() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let (count, notice) = counting_notice();
        history.set_notice(notice);
        run(&history, 1, b"kept\n", now_ms());
        assert_eq!(notices(&count), 1);

        // The same minimal runtime finish_recorded uses: clear_records is async.
        let clear = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a minimal runtime for the clear");
        clear.block_on(history.clear_records());
        assert_eq!(notices(&count), 2, "a clear over a held record moves the answer");
        clear.block_on(history.clear_records());
        assert_eq!(notices(&count), 2, "a clear over an empty record does not");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_evicting_pass_publishes_more_than_its_filings_alone() {
        let dir = scratch();
        // A byte budget one record cannot stay under: the second filing must evict
        // the first, and that eviction is a publish the filing alone does not
        // explain (SPEC §remote.history: a retention pass that removed a line).
        let history = RunHistory::open_with(
            dir.clone(),
            Limits {
                max_total_bytes: 1,
                max_runs: 1,
                audit_window_ms: 0,
                ..Limits::default()
            },
        );
        let (count, notice) = counting_notice();
        history.set_notice(notice);

        run(&history, 1, b"first\n", now_ms());
        run(&history, 2, b"second\n", now_ms());
        assert!(
            notices(&count) > 2,
            "two filings and at least one eviction: {}",
            notices(&count)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_open_passes_publish_once_a_notice_exists() {
        // A legacy record (a clear body in the index) plus a masked literal: both
        // open passes rewrite rows. A clean log's open passes rewrite nothing.
        let dir = scratch();
        {
            let history = RunHistory::open(dir.clone());
            write(&history, 1, "SECRET=1\n");
            make_legacy(&dir, 1, "SECRET=1\n");
        }
        let history = RunHistory::open(dir.clone());
        let (count, notice) = counting_notice();
        history.set_notice(notice);
        assert_eq!(notices(&count), 1, "the open passes' move is announced once");
        let (_, again) = counting_notice();
        history.set_notice(again);
        assert_eq!(notices(&count), 1, "and never again");

        let clean = RunHistory::open(scratch());
        let (count, notice) = counting_notice();
        clean.set_notice(notice);
        assert_eq!(notices(&count), 0, "a clean open moved nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
