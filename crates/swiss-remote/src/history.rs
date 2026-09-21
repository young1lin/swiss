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
//! may be secrets) — and the full output stream in `logs/remote/out/<runId>.txt`, teed
//! as it flows. The live buffer keeps a 256 KiB window; the file keeps the head up to
//! [Limits::max_output_bytes], and past that the line keeps the last 64 KiB as `tail`, so
//! a capped build log still shows both ends — the error is usually at the end.
//!
//! Three budgets, oldest-first: age, total bytes on disk (index + output files), and a
//! run count so the index itself stays small. Checked in O(1) on every finish and every
//! page read from three tracked numbers; the full pass (two streaming walks of the index
//! and a tmp+rename rewrite, never the whole file in memory) runs only when one is over.
//! One promise sits above the budgets (docs/41 A2): a line younger than
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
use swiss_core::util::{now_ms, parse_iso_ms};
use swiss_host::services::action::RunOutputTee;
use swiss_host::services::runs::{RunHistorySink, RunView, SubmitRequest};

/// Records older than this age out (the user's retention: 30 days).
pub const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Records younger than this are never dropped by a budget (docs/41 A2): the seven days
/// the owner can always trace back. Their output files may go under byte pressure; the
/// line — what ran, who, exit, tail — stays until the age limit.
pub const AUDIT_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The whole log — index plus every output file — stays under this (500 MiB).
pub const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// A count cap so a flood of tiny runs cannot grow the index past a few megabytes.
pub const MAX_RUNS: usize = 5000;
/// One run's output file stops growing here; the record keeps the tail past it.
pub const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// What a capped run keeps of its end (the ring's finished-keep size, docs/34 §17).
pub const TAIL_BYTES: usize = 64 * 1024;
/// One output read never materialises more than this — the live route's own ceiling.
pub const MAX_READ_BYTES: usize = 128 * 1024;
pub const PAGE_SIZE: usize = 20;

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
    ledger: Mutex<Ledger>,
    /// The tees handed out at begin and not yet closed — finish finds its file here —
    /// each with the sanitized input, so a run in flight can say what it is running
    /// before its record exists. At most max_concurrent entries (the coordinator's
    /// bound), never more.
    open: Mutex<HashMap<u64, (Arc<OutputFile>, Value)>>,
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
            let ring = inner.overflow.get_or_insert_with(|| VecDeque::with_capacity(TAIL_BYTES));
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
        // (docs/41 U3): decode from the first boundary, not from byte zero.
        let tail = inner.overflow.take().map(|ring| {
            let bytes = ring.into_iter().collect::<Vec<u8>>();
            String::from_utf8_lossy(swiss_core::utf8::window(&bytes)).into_owned()
        });
        (inner.written, tail)
    }
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
            ledger: Mutex::new(Ledger::default()),
            open: Mutex::new(HashMap::new()),
        };
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
                    .and_then(|n| n.strip_suffix(".txt"))
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

    /// The sanitized input of a run that began and has not finished — for the active
    /// rows of the runs page, which the coordinator's view does not carry.
    pub fn in_flight_input(&self, run_id: u64) -> Option<Value> {
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run_id)
            .map(|(_, input)| input.clone())
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

    /// A page of records, newest first, strictly before `before` when given, filtered by
    /// target id when given. Returns the page and the cursor for the next (older) one.
    pub fn page(
        &self,
        before: Option<u64>,
        limit: usize,
        target: Option<&str>,
    ) -> (Vec<Value>, Option<u64>) {
        self.enforce_budget();
        let limit = limit.clamp(1, 100);
        let mut records: Vec<Value> = Vec::with_capacity(limit + 1);
        self.walk_back(|v| {
            let id = v.get("runId").and_then(Value::as_u64).unwrap_or(0);
            if before.is_some_and(|b| id >= b) {
                return true;
            }
            if target.is_some_and(|t| record_target(&v) != Some(t)) {
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
            page.last().and_then(|v| v.get("runId").and_then(Value::as_u64))
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
        // Character boundaries (docs/41 U1): a window that would end inside a
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
        let over = ledger.total_bytes > self.limits.max_total_bytes
            || ledger.runs > self.limits.max_runs;
        if !aged && !over {
            return;
        }
        if !aged && ledger.hold_until_ms.is_some_and(|until| now < until) {
            return;
        }
        self.evict(now);
    }

    /// Bring the budgets back under, oldest first, without touching a protected line
    /// (docs/41 A2). Two phases: whole records outside the window (their output files
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
                    lines.push(LineFacts { line_bytes: bytes, ..f });
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
            self.ledger.lock().unwrap_or_else(|e| e.into_inner()).hold_until_ms = hold_until;
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
        }
        // Rebuild rather than trust the arithmetic: the rewrite is the truth now.
        drop(_guard);
        let mut ledger = self.scan(|_| {});
        ledger.hold_until_ms = hold_until;
        *self.ledger.lock().unwrap_or_else(|e| e.into_inner()) = ledger;
    }

    /// Append one finished run: the line, then the budgets.
    fn record(&self, view: &RunView, request: &SubmitRequest, tee: &OutputFile) {
        let (output_bytes, tail) = tee.close();
        if output_bytes == 0 {
            let _ = std::fs::remove_file(self.output_path(view.run_id));
        }
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
        let Ok(mut text) = serde_json::to_string(&Value::Object(line)) else {
            return;
        };
        text.push('\n');
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        mkdir_private(&self.dir);
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if appended.is_err() {
            return;
        }
        chmod_private(&self.index_path(), private_file_mode());
        ledger.total_bytes += text.len() as u64 + output_bytes;
        ledger.runs += 1;
        if ledger.oldest_ended_ms.is_none() {
            ledger.oldest_ended_ms = view.ended_at_ms;
        }
        if output_bytes > 0 && ledger.total_bytes > self.limits.max_total_bytes {
            // The file just written is something a pass can evict: let it look.
            ledger.hold_until_ms = None;
        }
        drop(ledger);
        self.enforce_budget();
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
        Some(tee)
    }

    fn finish(&self, view: &RunView, request: &SubmitRequest) {
        let tee = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&view.run_id);
        let Some((tee, _)) = tee else {
            return; // never announced to us (begin declined it) — nothing to record
        };
        self.record(view, request, &tee);
    }
}

/// The facts the budget pass needs from one line, without keeping the line.
#[derive(Debug, Clone)]
struct LineFacts {
    run_id: u64,
    ended_ms: u64,
    /// What the output file holds on disk: zero once a pass evicted it, whatever the
    /// line's `outputBytes` (the size the run produced) still says.
    output_bytes: u64,
    line_bytes: u64,
}

impl LineFacts {
    fn parse(line: &[u8]) -> Option<Self> {
        let v = parse_line(line)?;
        let run_id = v.get("runId")?.as_u64()?;
        let ended_ms = v
            .get("endedAt")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms)
            .and_then(|ms| u64::try_from(ms).ok())
            .unwrap_or(0);
        let evicted = v.get("outputEvicted").and_then(Value::as_bool).unwrap_or(false);
        let output_bytes = if evicted {
            0
        } else {
            v.get("outputBytes").and_then(Value::as_u64).unwrap_or(0)
        };
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

/// The target a record ran against: the outcome's meta first (what actually resolved),
/// the input second (a run that failed before resolving still names what was asked).
fn record_target(v: &Value) -> Option<&str> {
    v.get("meta")
        .and_then(|m| m.get("target"))
        .or_else(|| v.get("input").and_then(|i| i.get("target")))
        .and_then(Value::as_str)
}

/// The submission's input with every `env` object reduced to its key list: an exec's
/// env pairs are user-provided and may carry a token; the record says WHICH variables
/// were set, never what to.
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
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_host::services::runs::RunState;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-rhist-{}",
            swiss_core::util::random_hex(8)
        ));
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
        let req = request("remote.exec", json!({ "target": "build", "argv": ["make"] }));
        let tee = history.begin(run_id, &req).expect("a remote run is recorded");
        if !output.is_empty() {
            tee.write(output);
        }
        history.finish(&view(run_id, ended_at_ms), &req);
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
        history.finish(&view(7, now_ms()), &req);

        let (page, next) = history.page(None, 20, None);
        assert_eq!(page.len(), 1);
        assert!(next.is_none());
        let row = &page[0];
        assert_eq!(row["runId"], 7);
        assert_eq!(row["state"], "succeeded");
        assert_eq!(row["actor"], "test", "who ran it is on the line (docs/41 A1)");
        assert_eq!(row["outputBytes"], 18);
        assert_eq!(row["meta"]["target"], "build");
        assert_eq!(row["input"]["argv"], json!(["make", "-j8"]));
        assert_eq!(row["input"]["envKeys"], json!(["TOKEN"]));
        assert!(row["input"].get("env").is_none(), "values never land on disk");
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
    fn other_producers_runs_are_declined() {
        let dir = scratch();
        let history = RunHistory::open(dir.clone());
        let req = request("shell.run", json!({}));
        assert!(history.begin(1, &req).is_none());
        // A finish for a run never begun is a no-op, not a phantom line.
        history.finish(&view(1, now_ms()), &req);
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
        history.finish(&view(9, now_ms()), &req);
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
        // Outside the seven-day window (docs/41 A2) the budgets work as they always
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
        // docs/41 A2: six runs today, 500 bytes each (a line is ~300 more), a 3.5 KB
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
        assert_eq!(ids, vec![6, 5, 4, 3, 2, 1], "no line inside the window is dropped");
        let evicted: Vec<u64> = page
            .iter()
            .filter(|v| v["outputEvicted"] == true)
            .map(|v| v["runId"].as_u64().unwrap())
            .collect();
        assert!(!evicted.is_empty(), "the byte budget had to take something");
        assert!(evicted.contains(&1), "the oldest output goes first: {evicted:?}");
        assert!(!evicted.contains(&6), "the newest keeps its file: {evicted:?}");
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
        assert!(ids.contains(&6) && ids.contains(&5) && ids.contains(&4), "{ids:?}");
        for id in 4..=6u64 {
            assert!(dir.join(OUT_DIR).join(format!("{id}.txt")).exists(), "run {id}");
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
        let mtime = std::fs::metadata(dir.join(INDEX_FILE)).unwrap().modified().unwrap();
        for _ in 0..3 {
            history.page(None, 20, None);
        }
        assert_eq!(index_bytes(&dir), before);
        assert_eq!(
            std::fs::metadata(dir.join(INDEX_FILE)).unwrap().modified().unwrap(),
            mtime,
            "no rewrite while the hold stands"
        );
        // Two old runs behind them: those go, the five stay, the cap is still over.
        run(&history, 6, b"old\n", base - 8 * DAY);
        run(&history, 7, b"old\n", base - 8 * DAY + 1);
        assert_eq!(history.usage().1, 7, "the hold stands: the appends did not rewrite");
        history.ledger.lock().unwrap().hold_until_ms = None; // the window moved on
        let (page, _) = history.page(None, 20, None);
        let ids: Vec<u64> = page.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1], "the old two went, the window stayed");
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
        assert!(!dir.join(OUT_DIR).join("3.txt").exists(), "the new file went too");
        assert_eq!(history.get(3).unwrap()["outputEvicted"], true);
        assert_eq!(history.usage().1, 3, "and every line is still there");
        // A silent run adds nothing a pass could take: the hold stands, no rewrite.
        let before = index_bytes(&dir);
        run(&history, 4, b"", base + 4);
        let after = index_bytes(&dir);
        assert!(after.starts_with(&before), "append only: {}", String::from_utf8_lossy(&after));
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
            history.finish(&v, &req);
        }
        let (first, next) = history.page(None, 3, None);
        let ids: Vec<u64> = first.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![7, 6, 5]);
        assert_eq!(next, Some(5));
        let (second, next) = history.page(next, 3, None);
        let ids: Vec<u64> = second.iter().map(|v| v["runId"].as_u64().unwrap()).collect();
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
    fn orphaned_output_files_are_removed_at_open_and_clear_forgets_everything() {
        let dir = scratch();
        {
            let history = RunHistory::open(dir.clone());
            run(&history, 1, b"kept\n", now_ms());
            // A crash mid-run: a file the index never learned about.
            std::fs::write(dir.join(OUT_DIR).join("99.txt"), b"orphan").unwrap();
        }
        let history = RunHistory::open(dir.clone());
        assert!(!dir.join(OUT_DIR).join("99.txt").exists());
        assert!(dir.join(OUT_DIR).join("1.txt").exists());
        history.clear();
        assert_eq!(history.usage(), (0, 0));
        assert!(history.page(None, 20, None).0.is_empty());
        assert!(!dir.join(OUT_DIR).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
