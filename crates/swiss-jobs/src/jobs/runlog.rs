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

//! Per-job run history on disk - the "log what happened" half of the feature, port of the
//! calls.ts shape applied to scheduled commands.
//!
//! One JSON line per run in logs/jobs/<job>.jsonl: when it ran, what triggered it (timer or
//! manual), how long it took, the exit code or the error, and the tail of the output. The log
//! survives a restart - which is exactly when you want to know whether the 3 a.m. job ran. The
//! file is capped two ways, like the call log: by bytes (a chatty command cannot grow it
//! forever) and by age (a quiet job's years-old records do age out).
//!
//! Simpler than calls.rs on purpose: there is no async append queue (the scheduler's
//! one-run-at-a-time claim per job already serializes writers) and no body layer (the runner
//! caps the output before it ever reaches here).
//!
//! [RunLog] is an INSTANCE owned by the JobSystem (SPEC §jobs.runlog): the directory, the
//! per-job write states and the budgets are fields, not process globals - two systems in
//! one process (or two tests in one binary) each hold their own and cannot see each
//! other's files. Reading a page walks the file from its END in blocks and stops when the
//! page is full, so the cost of a page tracks the page size, not the history behind it.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{Map, Value};

use swiss_core::platform::{chmod_private, mkdir_private, private_file_mode};
use swiss_core::util::{now_ms, parse_iso_ms};

pub const RUNS_PAGE_SIZE: usize = 20;

/// Byte and age budgets. The defaults are today's values; the retention config starts
/// driving them when definitions move into the plugin config row (SPEC §jobs.apply, §jobs).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Trim the file down to keep_bytes once it passes max_bytes.
    pub max_bytes: u64,
    pub keep_bytes: u64,
    /// Entries older than this age out on the hourly sweep.
    pub max_age_ms: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_bytes: 2 * 1024 * 1024,
            keep_bytes: 1024 * 1024,
            // 180 days, like the call log's retention.
            max_age_ms: 180 * 24 * 60 * 60 * 1000,
        }
    }
}

/// How often the age sweep runs per job (day-resolution boundary, same reasoning as the
/// call log's SWEEP_EVERY_MS).
const SWEEP_EVERY_MS: u64 = 60 * 60 * 1000;

/// The retention config row's budgets as [Limits] (SPEC §jobs.config -> §8). days drives the
/// age cap; maxBytesPerJob drives both the trim trigger and (at half) the size it trims
/// down to, preserving the default 2:1 ratio. maxHistoryBytes has no per-file home - it
/// is the TOTAL ceiling this build accepts and stores but does not yet enforce across
/// files; that cross-file sweep arrives with the S5 semantics that need it.
pub fn limits_of_retention(retention: &super::def::Retention) -> Limits {
    Limits {
        max_bytes: retention.max_bytes_per_job,
        keep_bytes: retention.max_bytes_per_job / 2,
        max_age_ms: i64::from(retention.days) * 24 * 60 * 60 * 1000,
    }
}

/// Per-job write state: the next sequence number, the file size, and when the age sweep
/// last ran. Recovered from the file on first touch, so sequence numbers survive a restart.
struct FileState {
    seq: u64,
    bytes: u64,
    swept_at: u64,
}

/// The per-job run history for ONE directory tree. Cheap to construct; the JobSystem
/// holds exactly one for the lifetime of the plugin.
pub struct RunLog {
    dir: PathBuf,
    states: Mutex<HashMap<String, FileState>>,
    /// Behind a mutex only so a config apply can swap the budgets while running -
    /// contention is a non-issue at one append per job run.
    limits: Mutex<Limits>,
    /// Bytes read from disk by read_page since construction. Test-observable on purpose:
    /// the bounded-read promise of cursor pagination is exactly what it measures.
    bytes_read: AtomicU64,
}

impl RunLog {
    /// A log writing into `dir` (created on first append) with the default budgets.
    pub fn at(dir: PathBuf) -> RunLog {
        RunLog {
            dir,
            states: Mutex::new(HashMap::new()),
            limits: Mutex::new(Limits::default()),
            bytes_read: AtomicU64::new(0),
        }
    }

    /// Swap the budgets - what a config apply does when retention changes (SPEC §jobs.apply:)
    /// the new caps bind from the NEXT write; existing files age out under them.
    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock().unwrap_or_else(|e| e.into_inner()) = limits;
    }

    /// Total bytes read_page has pulled off disk. A page of `limit` records must cost
    /// roughly (limit + 1) lines however long the history is - that is the invariant.
    pub fn bytes_read_total(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    fn file_for(&self, job: &str) -> PathBuf {
        self.dir.join(format!("{job}.jsonl"))
    }

    /// Append one run record, assigning its sequence number (inserted first, so the line
    /// reads seq-at-trigger-... just like a call line). The record map is consumed field
    /// by field.
    pub fn append_run(&self, job: &str, record: Map<String, Value>) -> Result<u64, String> {
        let path = self.file_for(job);
        let mut line = Map::new();
        let seq = {
            let mut states = self.states.lock().map_err(|e| e.to_string())?;
            let st = states
                .entry(job.to_string())
                .or_insert_with(|| init_state(&path));
            st.seq += 1;
            st.seq
        };
        line.insert("seq".into(), Value::from(seq));
        for (k, v) in record {
            line.insert(k, v);
        }
        let text = format!(
            "{}\n",
            serde_json::to_string(&Value::Object(line)).map_err(|e| e.to_string())?
        );
        mkdir_private(&self.dir);
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        chmod_private(&path, private_file_mode());
        let bytes_added = text.len() as u64;
        let (total, swept_at) = {
            let mut states = self.states.lock().map_err(|e| e.to_string())?;
            let st = states
                .entry(job.to_string())
                .or_insert_with(|| init_state(&path));
            st.bytes += bytes_added;
            (st.bytes, st.swept_at)
        };
        if total
            > self
                .limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .max_bytes
        {
            self.trim(job, &path)?;
        }
        if now_ms().saturating_sub(swept_at) > SWEEP_EVERY_MS {
            self.sweep_age(job, &path)?;
        }
        Ok(seq)
    }

    /// One page of a job's history, newest first, optionally starting strictly before a
    /// seq. Reads BACKWARDS from the end of the file in fixed-size blocks and stops as
    /// soon as one more record than the page needs has been collected (the extra decides
    /// whether a next page exists), so a page of 20 costs twenty-odd lines however many
    /// years sit behind it. Torn lines (a kill mid-append) are skipped, not fatal - same
    /// policy as the call log readers; a read error mid-walk returns what was collected.
    pub fn read_page(
        &self,
        job: &str,
        before: Option<u64>,
        limit: usize,
    ) -> (Vec<Value>, Option<u64>) {
        let limit = limit.clamp(1, 100);
        let path = self.file_for(job);
        let Ok(mut file) = std::fs::File::open(&path) else {
            return (Vec::new(), None);
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            return (Vec::new(), None);
        }
        const CHUNK: u64 = 8 * 1024;
        let mut records: Vec<Value> = Vec::with_capacity(limit + 1);
        let mut tail: Vec<u8> = Vec::new();
        let mut pos = len;
        while pos > 0 && records.len() <= limit {
            let take = CHUNK.min(pos) as usize;
            pos -= take as u64;
            let mut buf = vec![0u8; take];
            if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut buf).is_err() {
                break; // best effort: hand back what was collected
            }
            self.bytes_read.fetch_add(take as u64, Ordering::Relaxed);
            // The chunk goes in FRONT of the not-yet-split bytes; complete lines are
            // then split off the end, i.e. newest first, until only a partial line
            // prefix remains (an older chunk may complete it).
            let mut merged = buf;
            merged.append(&mut tail);
            tail = merged;
            while records.len() <= limit {
                let Some(nl) = tail.iter().rposition(|b| *b == b'\n') else {
                    break;
                };
                let mut line: Vec<u8> = tail.split_off(nl + 1);
                tail.truncate(nl);
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let text = String::from_utf8_lossy(&line);
                if text.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue; // torn line, same skip policy as before
                };
                let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
                if before.is_none_or(|b| seq < b) {
                    // Records are ENCOUNTERED newest-first - within a block the split
                    // walks from the end, and every later block is older - so a push
                    // keeps the running list newest-first for free.
                    records.push(v);
                }
            }
        }
        // Whatever is left when the file starts is its oldest (possibly torn) line.
        if pos == 0 && !tail.is_empty() && records.len() <= limit {
            let text = String::from_utf8_lossy(&tail).trim().to_string();
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
                if before.is_none_or(|b| seq < b) {
                    records.push(v);
                }
            }
        }
        let take = records.len().min(limit);
        let page: Vec<Value> = records.drain(..take).collect();
        // A next cursor exists only when an older record was actually seen.
        let next_before = if records.is_empty() {
            None
        } else {
            page.last()
                .and_then(|v| v.get("seq").and_then(Value::as_u64))
        };
        (page, next_before)
    }

    /// Drop the oldest history once the file outgrows its budget, keeping the newest
    /// keep_bytes. The read handle is dropped BEFORE the tmp-rename swap: Windows refuses
    /// to replace a file that is still open. Ported from calls.rs's trim.
    fn trim(&self, job: &str, path: &std::path::Path) -> Result<(), String> {
        let limits = *self.limits.lock().unwrap_or_else(|e| e.into_inner());
        let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size <= limits.keep_bytes {
            return Ok(());
        }
        let start = size - limits.keep_bytes;
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; limits.keep_bytes as usize];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        drop(file);
        // Start at the first line boundary so the file never begins with half an entry.
        let kept: &[u8] = match buf.iter().position(|b| *b == b'\n') {
            Some(nl) => &buf[nl + 1..],
            None => &[],
        };
        swiss_core::atomic_json::write_text_atomic(path, &String::from_utf8_lossy(kept))?;
        if let Ok(mut states) = self.states.lock() {
            if let Some(st) = states.get_mut(job) {
                st.bytes = kept.len() as u64;
            }
        }
        Ok(())
    }

    /// Drop entries older than the age budget. The file is append-only and chronological,
    /// so everything expired is a prefix; a torn line has no usable date and goes with the
    /// prefix.
    fn sweep_age(&self, job: &str, path: &std::path::Path) -> Result<(), String> {
        if let Ok(mut states) = self.states.lock() {
            if let Some(st) = states.get_mut(job) {
                st.swept_at = now_ms();
            }
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let limits = *self.limits.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff = now_ms() as i64 - limits.max_age_ms;
        let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
        let mut keep_from = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let at = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("at").and_then(Value::as_str).map(str::to_string))
                .and_then(|at| parse_iso_ms(&at));
            if at.map(|t| t >= cutoff).unwrap_or(false) {
                keep_from = i;
                break;
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
        swiss_core::atomic_json::write_text_atomic(path, &out)?;
        if let Ok(mut states) = self.states.lock() {
            if let Some(st) = states.get_mut(job) {
                st.bytes = out.len() as u64;
            }
        }
        Ok(())
    }

    /// Age-and-trim every job's log in this directory. Called from the hourly sweep task
    /// next to the call-log sweep, because per-append checks only ever fire for a job
    /// that is still running - the opposite of the idle log that needs ageing out.
    pub fn sweep_all(&self) {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(job) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let path = self.dir.join(name.as_ref());
            let _ = self.trim(job, &path);
            let _ = self.sweep_age(job, &path);
        }
    }
}

/// A job name becomes a filename under the log directory, so it is validated at every
/// entry point: non-empty, at most 64 bytes, and only the filename-safe ASCII subset.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Recover one job's file state from disk: the last line's seq plus the file size.
fn init_state(path: &std::path::Path) -> FileState {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let seq = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .rfind(|l: &&str| !l.is_empty())
                .and_then(|last| serde_json::from_str::<Value>(last).ok())
                .and_then(|v| v.get("seq").and_then(Value::as_u64))
        })
        .unwrap_or(0);
    FileState {
        seq,
        bytes,
        swept_at: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A private scratch directory per test - the instance carries it, so no lock is
    /// needed and no two tests can see each other's files.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-runlog-{}-{}",
            name,
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    fn record(ok: bool) -> Map<String, Value> {
        let mut m = json!({
            "at": swiss_core::log::iso_now(),
            "trigger": "timer",
            "ok": ok,
            "ms": 12,
            "output": "done",
            "chars": 4,
        })
        .as_object()
        .expect("an object")
        .clone();
        if !ok {
            m.insert("error".into(), json!("boom"));
            m.insert("exitCode".into(), json!(2));
        }
        m
    }

    #[test]
    fn names_that_become_filenames_are_rejected_up_front() {
        assert!(valid_name("backup"));
        assert!(valid_name("db-vacuum.2"));
        assert!(!valid_name(""));
        assert!(!valid_name("."));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a\\b"));
        assert!(!valid_name("with space"));
        assert!(!valid_name(&"x".repeat(65)));
    }

    #[test]
    fn appends_assign_sequential_numbers_and_pages_read_newest_first() {
        let log = RunLog::at(scratch("seq"));
        for i in 0..5 {
            let seq = log.append_run("job", record(i != 1)).expect("append");
            assert_eq!(seq, (i + 1) as u64);
        }
        let (page, next) = log.read_page("job", None, 3);
        let seqs: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![5, 4, 3], "newest first: {page:?}");
        assert_eq!(next, Some(3), "the cursor is the oldest seq in the page");

        let (page, next) = log.read_page("job", next, 3);
        let seqs: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![2, 1]);
        assert_eq!(next, None, "no older entries remain");
    }

    #[test]
    fn sequence_numbers_continue_across_a_restart_of_the_writer_state() {
        let dir = scratch("restart");
        let first = RunLog::at(dir.clone());
        first.append_run("job", record(true)).expect("append");
        first.append_run("job", record(true)).expect("append");
        // Simulate a restart: a NEW instance over the same directory, empty write state.
        let reopened = RunLog::at(dir);
        let seq = reopened.append_run("job", record(true)).expect("append");
        assert_eq!(seq, 3, "recovered from the last line on disk");
    }

    #[test]
    fn optional_fields_stay_absent_not_null() {
        let log = RunLog::at(scratch("absent"));
        log.append_run("job", record(true)).expect("append");
        let text = std::fs::read_to_string(log.file_for("job")).expect("readable");
        let line: Value =
            serde_json::from_str(text.lines().next().expect("one line")).expect("json");
        assert!(line.get("error").is_none(), "absent, not null: {line}");
        assert!(line.get("exitCode").is_none(), "absent, not null: {line}");
        assert_eq!(line["trigger"], json!("timer"));
    }

    #[test]
    fn the_byte_budget_trims_the_oldest_lines() {
        let log = RunLog::at(scratch("trim"));
        // Big outputs so a handful of appends cross the 2 MB budget.
        let mut big = record(true);
        big.insert("output".into(), json!("x".repeat(60 * 1024)));
        for _ in 0..40 {
            log.append_run("job", big.clone()).expect("append");
        }
        let len = std::fs::metadata(log.file_for("job")).expect("stat").len();
        assert!(
            len <= log
                .limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .max_bytes
                + 128 * 1024,
            "trimmed to about keep_bytes: {len}"
        );
        let (page, _) = log.read_page("job", None, 100);
        let seqs: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().unwrap_or(0))
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[0] > w[1]),
            "still newest first after the trim: {seqs:?}"
        );
        assert!(
            !seqs.contains(&1),
            "the oldest lines were dropped: {seqs:?}"
        );
    }

    #[test]
    fn expired_entries_age_out_on_a_sweep() {
        let log = RunLog::at(scratch("age"));
        // A stamp 200 days old: past the 180-day retention, day-resolution boundary.
        let old = {
            let t = time::OffsetDateTime::now_utc() - time::Duration::days(200);
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        };
        let mut old_rec = record(true);
        old_rec.insert("at".into(), json!(old));
        log.append_run("job", old_rec).expect("append");
        log.append_run("job", record(true)).expect("append");
        log.sweep_all();
        let (page, _) = log.read_page("job", None, 100);
        assert_eq!(page.len(), 1, "only the fresh entry survives: {page:?}");
        let ats: Vec<&str> = page.iter().filter_map(|v| v["at"].as_str()).collect();
        assert_ne!(ats, vec![old.as_str()]);
    }

    #[test]
    fn a_page_for_a_job_that_never_ran_is_empty() {
        let log = RunLog::at(scratch("never"));
        let (page, next) = log.read_page("never", None, 20);
        assert!(page.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn two_instances_on_different_directories_never_see_each_other() {
        // The whole point of the instantiation (SPEC §jobs.runlog): what used to be
        // process-global DIR/STATES is now per-instance, provably.
        let a = RunLog::at(scratch("iso-a"));
        let b = RunLog::at(scratch("iso-b"));
        a.append_run("job", record(true)).expect("append");
        a.append_run("job", record(true)).expect("append");
        b.append_run("job", record(true)).expect("append");
        let (pa, next) = a.read_page("job", None, 20);
        assert_eq!(pa.len(), 2, "instance A sees only its own file");
        assert_eq!(next, None);
        let (pb, next) = b.read_page("job", None, 20);
        assert_eq!(pb.len(), 1, "instance B sees only its own file");
        assert_eq!(next, None);
        // And the sequence counters are independent too.
        let seq = b.append_run("job", record(true)).expect("append");
        assert_eq!(seq, 2, "B's counter is not A's counter");
    }

    #[test]
    fn pagination_walks_to_the_bottom_with_bounded_reads() {
        let log = RunLog::at(scratch("bounded"));
        // A file far longer than any page: 2000 records, ~100 bytes each (~200 KB).
        let mut wide = record(true);
        wide.insert("output".into(), json!("y".repeat(64)));
        for _ in 0..2000 {
            log.append_run("wide", wide.clone()).expect("append");
        }
        let file_len = std::fs::metadata(log.file_for("wide")).expect("stat").len();
        assert!(
            file_len > 150 * 1024,
            "the fixture is long enough: {file_len}"
        );

        // The first page reads a bounded number of bytes however deep the history is:
        // (limit + 1) records' worth plus at most one chunk of slack.
        let (page, next) = log.read_page("wide", None, 20);
        assert_eq!(page.len(), 20);
        assert_eq!(
            page[0]["seq"].as_u64(),
            Some(2000),
            "newest first from the tail"
        );
        assert_eq!(next, Some(1981));
        let read = log.bytes_read_total();
        assert!(
            read <= 8 * 1024 + 21 * 512,
            "a 20-record page must not scan the file: {read} bytes of {file_len}"
        );
        assert!(read < file_len / 4, "{read} vs {file_len}");

        // Walking the cursor to the bottom ends with an absent nextBefore and visits
        // every record exactly once, in order.
        let mut seen: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().unwrap_or(0))
            .collect();
        let mut cursor = next;
        while let Some(before) = cursor {
            let (page, next) = log.read_page("wide", Some(before), 20);
            if page.is_empty() {
                assert!(next.is_none(), "no phantom cursor past the bottom");
                break;
            }
            seen.extend(page.iter().map(|v| v["seq"].as_u64().unwrap_or(0)));
            cursor = next;
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 2000, "every record visited exactly once");
        assert_eq!(*seen.first().unwrap(), 1);
        assert_eq!(*seen.last().unwrap(), 2000);
    }
}
