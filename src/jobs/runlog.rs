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
//! caps the output before it ever reaches here). Reading a page reads the whole file, which
//! the byte cap keeps at 2 MB and an admin poll touches a few times a minute at most.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

use crate::platform::{chmod_private, mkdir_private, private_file_mode};
use crate::util::{now_ms, parse_iso_ms};

/// Index budget per job: trimmed to the newest KEEP_BYTES once exceeded.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const KEEP_BYTES: u64 = 1024 * 1024;
/// Retention: 180 days, checked at most hourly per job (day-resolution boundary, same
/// reasoning as the call log's SWEEP_EVERY_MS).
const MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;
const SWEEP_EVERY_MS: u64 = 60 * 60 * 1000;
pub const RUNS_PAGE_SIZE: usize = 20;

static DIR: OnceLock<Mutex<PathBuf>> = OnceLock::new();

fn log_dir() -> PathBuf {
    DIR.get_or_init(|| Mutex::new(crate::paths::data_path(&["logs", "jobs"])))
        .lock()
        .map(|d| d.clone())
        .unwrap_or_else(|_| crate::paths::data_path(&["logs", "jobs"]))
}

/// Point the log somewhere else (tests use a temp directory). Mirrors set_call_log_dir: the
/// per-job write states are forgotten too, so they re-cover from the new location.
pub fn set_run_log_dir(dir: PathBuf) {
    if let Ok(mut holder) = DIR.get_or_init(|| Mutex::new(dir.clone())).lock() {
        *holder = dir;
    }
    if let Ok(mut m) = states().lock() {
        m.clear();
    }
}

/// Per-job write state: the next sequence number, the file size, and when the age sweep last
/// ran. Recovered from the file on first touch, so sequence numbers survive a restart.
struct FileState {
    seq: u64,
    bytes: u64,
    swept_at: u64,
}

static STATES: OnceLock<Mutex<HashMap<String, FileState>>> = OnceLock::new();

/// Serializes every test that touches the process-global DIR/STATES above - the sync ones in
/// this module's tests and the async execute() tests in mod.rs/api.rs. A tokio mutex for the
/// same reason paths::DATA_DIR_LOCK is one: async callers hold it across awaits, and a std
/// mutex held across an await can deadlock the single-threaded test runtime.
#[cfg(test)]
pub(crate) static RUNLOG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn states() -> &'static Mutex<HashMap<String, FileState>> {
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_for(job: &str) -> PathBuf {
    log_dir().join(format!("{job}.jsonl"))
}

/// A job name becomes a filename under logs/jobs/, so it is validated at every entry point:
/// non-empty, at most 64 bytes, and only the filename-safe ASCII subset.
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

/// Append one run record, assigning its sequence number (inserted first, so the line reads
/// seq-at-trigger-... just like a call line). The record map is consumed field by field.
pub fn append_run(job: &str, record: Map<String, Value>) -> Result<u64, String> {
    let path = file_for(job);
    let mut line = Map::new();
    let seq = {
        let mut states = states().lock().map_err(|e| e.to_string())?;
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
    let text = format!("{}\n", serde_json::to_string(&Value::Object(line)).map_err(|e| e.to_string())?);
    mkdir_private(&log_dir());
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
        let mut states = states().lock().map_err(|e| e.to_string())?;
        let st = states
            .entry(job.to_string())
            .or_insert_with(|| init_state(&path));
        st.bytes += bytes_added;
        (st.bytes, st.swept_at)
    };
    if total > MAX_BYTES {
        trim(job, &path)?;
    }
    if now_ms().saturating_sub(swept_at) > SWEEP_EVERY_MS {
        sweep_age(job, &path)?;
    }
    Ok(seq)
}

/// One page of a job's history, newest first, optionally starting strictly before a seq.
/// Torn lines (a kill mid-append) are skipped, not fatal - same policy as the call log readers.
pub fn read_page(job: &str, before: Option<u64>, limit: usize) -> (Vec<Value>, Option<u64>) {
    let limit = limit.clamp(1, 100);
    let Ok(text) = std::fs::read_to_string(file_for(job)) else {
        return (Vec::new(), None);
    };
    let mut all: Vec<Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    if let Some(before) = before {
        all.retain(|v| v.get("seq").and_then(Value::as_u64).unwrap_or(0) < before);
    }
    let take = all.len().min(limit);
    let page: Vec<Value> = all.split_off(all.len() - take).into_iter().rev().collect();
    let next_before = if take == limit && !all.is_empty() {
        page.last()
            .and_then(|v| v.get("seq").and_then(Value::as_u64))
    } else {
        None
    };
    (page, next_before)
}

/// Drop the oldest history once the file outgrows its budget, keeping the newest KEEP_BYTES.
/// The read handle is dropped BEFORE the tmp-rename swap: Windows refuses to replace a file
/// that is still open. Ported from calls.rs's trim.
fn trim(job: &str, path: &std::path::Path) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size <= KEEP_BYTES {
        return Ok(());
    }
    let start = size - KEEP_BYTES;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    use std::io::{Read, Seek, SeekFrom};
    let mut buf = vec![0u8; KEEP_BYTES as usize];
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    file.read_exact(&mut buf).map_err(|e| e.to_string())?;
    drop(file);
    // Start at the first line boundary so the file never begins with half an entry.
    let kept: &[u8] = match buf.iter().position(|b| *b == b'\n') {
        Some(nl) => &buf[nl + 1..],
        None => &[],
    };
    crate::atomic_json::write_text_atomic(path, &String::from_utf8_lossy(kept))?;
    if let Ok(mut states) = states().lock() {
        if let Some(st) = states.get_mut(job) {
            st.bytes = kept.len() as u64;
        }
    }
    Ok(())
}

/// Drop entries older than MAX_AGE_MS. The file is append-only and chronological, so
/// everything expired is a prefix; a torn line has no usable date and goes with the prefix.
fn sweep_age(job: &str, path: &std::path::Path) -> Result<(), String> {
    if let Ok(mut states) = states().lock() {
        if let Some(st) = states.get_mut(job) {
            st.swept_at = now_ms();
        }
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let cutoff = now_ms() as i64 - MAX_AGE_MS;
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
    crate::atomic_json::write_text_atomic(path, &out)?;
    if let Ok(mut states) = states().lock() {
        if let Some(st) = states.get_mut(job) {
            st.bytes = out.len() as u64;
        }
    }
    Ok(())
}

/// Age-and-trim every job's log. Called from the hourly sweep task next to the call-log sweep,
/// because per-append checks only ever fire for a job that is still running - the opposite of
/// the idle log that needs ageing out.
pub fn sweep_all() {
    let dir = log_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(job) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let path = dir.join(name.as_ref());
        let _ = trim(job, &path);
        let _ = sweep_age(job, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lock() -> tokio::sync::MutexGuard<'static, ()> {
        RUNLOG_TEST_LOCK.blocking_lock()
    }

    /// A private scratch dir per test, pointed at through set_run_log_dir. Call after lock().
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lmg-runlog-{}", crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        set_run_log_dir(dir.clone());
        dir
    }

    fn record(ok: bool) -> Map<String, Value> {
        let mut m = json!({
            "at": crate::log::iso_now(),
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
        let _g = lock();
        let _dir = scratch();
        for i in 0..5 {
            let seq = append_run("job", record(i != 1)).expect("append");
            assert_eq!(seq, (i + 1) as u64);
        }
        let (page, next) = read_page("job", None, 3);
        let seqs: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![5, 4, 3], "newest first: {page:?}");
        assert_eq!(next, Some(3), "the cursor is the oldest seq in the page");

        let (page, next) = read_page("job", next, 3);
        let seqs: Vec<u64> = page
            .iter()
            .map(|v| v["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![2, 1]);
        assert_eq!(next, None, "no older entries remain");
    }

    #[test]
    fn sequence_numbers_continue_across_a_restart_of_the_writer_state() {
        let _g = lock();
        let _dir = scratch();
        append_run("job", record(true)).expect("append");
        append_run("job", record(true)).expect("append");
        // Simulate a restart: forget the in-memory state, keep the file.
        if let Ok(mut m) = states().lock() {
            m.clear();
        }
        let seq = append_run("job", record(true)).expect("append");
        assert_eq!(seq, 3, "recovered from the last line on disk");
    }

    #[test]
    fn optional_fields_stay_absent_not_null() {
        let _g = lock();
        let _dir = scratch();
        append_run("job", record(true)).expect("append");
        let text = std::fs::read_to_string(file_for("job")).expect("readable");
        let line: Value = serde_json::from_str(text.lines().next().expect("one line")).expect("json");
        assert!(line.get("error").is_none(), "absent, not null: {line}");
        assert!(line.get("exitCode").is_none(), "absent, not null: {line}");
        assert_eq!(line["trigger"], json!("timer"));
    }

    #[test]
    fn the_byte_budget_trims_the_oldest_lines() {
        let _g = lock();
        let _dir = scratch();
        // Big outputs so a handful of appends cross the 2 MB budget.
        let mut big = record(true);
        big.insert("output".into(), json!("x".repeat(60 * 1024)));
        for _ in 0..40 {
            append_run("job", big.clone()).expect("append");
        }
        let len = std::fs::metadata(file_for("job")).expect("stat").len();
        assert!(len <= MAX_BYTES + 128 * 1024, "trimmed to about KEEP_BYTES: {len}");
        let (page, _) = read_page("job", None, 100);
        let seqs: Vec<u64> = page.iter().map(|v| v["seq"].as_u64().unwrap_or(0)).collect();
        assert!(
            seqs.windows(2).all(|w| w[0] > w[1]),
            "still newest first after the trim: {seqs:?}"
        );
        assert!(!seqs.contains(&1), "the oldest lines were dropped: {seqs:?}");
    }

    #[test]
    fn expired_entries_age_out_on_a_sweep() {
        let _g = lock();
        let _dir = scratch();
        // Seed the file directly so entries can carry dates the clock cannot reach.
        let old = {
            use time::format_description::well_known::Rfc3339;
            let t = time::OffsetDateTime::now_utc() - time::Duration::days(200);
            t.format(&Rfc3339).unwrap_or_default()
        };
        let mut old_rec = record(true);
        old_rec.insert("at".into(), json!(old));
        append_run("job", old_rec).expect("append");
        append_run("job", record(true)).expect("append");
        sweep_all();
        let (page, _) = read_page("job", None, 100);
        assert_eq!(page.len(), 1, "only the fresh entry survives: {page:?}");
        let ats: Vec<&str> = page.iter().filter_map(|v| v["at"].as_str()).collect();
        assert_ne!(ats, vec![old.as_str()]);
    }

    #[test]
    fn a_page_for_a_job_that_never_ran_is_empty() {
        let _g = lock();
        let _dir = scratch();
        let (page, next) = read_page("never", None, 20);
        assert!(page.is_empty());
        assert_eq!(next, None);
    }
}
