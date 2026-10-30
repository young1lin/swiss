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

//! The two log sources an app has (SPEC §process.apps): the capture file the
//! gateway writes (logs/apps/<name>.log - header lines, then start/stop and
//! app output) and the app's own log file, opened read-only per read with
//! full sharing so the app's rotation is never blocked.
//!
//! Reading returns at most the last 5,000 lines from at most the last 1 MiB.
//! Text decodes UTF-8, else GBK (encoding_rs), else lossily - the
//! supervisor's rule. Every value the app's references resolve to (8 bytes or
//! more) is masked: the references resolve again at READ time, and one that no
//! longer resolves cannot be masked (SPEC §process.apps).

use std::io::Write;
use std::path::{Path, PathBuf};

/// The capture file sweep threshold (SPEC §process.apps): copy-truncate above
/// 10 MiB, on the hourly tick and before each start.
pub const CAPTURE_SWEEP_ABOVE: u64 = 10 * 1024 * 1024;
/// The read caps.
pub const MAX_LINES: usize = 5_000;
pub const MAX_BYTES: u64 = 1024 * 1024;
/// The mask a resolved value (8 bytes or more) becomes.
pub const MASK: &str = "••••••••";

/// Where the capture file for one app lives, under the data dir.
pub fn capture_path(name: &str) -> PathBuf {
    swiss_core::paths::data_path(&["logs", "apps", &format!("{name}.log")])
}

/// Append the header line every action writes first (SPEC §process.apps):
/// `--- <RFC 3339> start by <by> ---`.
pub fn append_header(path: &Path, at: &str, action: &str, by: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "--- {at} {action} by {by} ---")
}

/// Copy-truncate a live capture file (SPEC §process.apps): copy to `.1` (one
/// generation), then cut the file to zero. The app's append-only handle keeps
/// writing at the new end; a line written between the copy and the cut is
/// lost, which is the price of never being in the write path.
pub fn sweep_capture(path: &Path) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > CAPTURE_SWEEP_ABOVE => meta,
        _ => return Ok(()),
    };
    let mut generation = path.to_path_buf().into_os_string();
    generation.push(".1");
    let generation = PathBuf::from(generation);
    let _ = std::fs::remove_file(&generation);
    std::fs::copy(path, &generation)?;
    // Cut to zero WITHOUT truncating the file out of existence: an
    // append-only handle stays valid across the cut.
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(0)?;
    Ok(())
}

/// The read tail: at most [MAX_BYTES] read from the end, at most [MAX_LINES]
/// lines kept, decoded by the supervisor's rule and masked.
pub struct LogTail {
    pub lines: Vec<String>,
    /// The byte size of the whole file - the `after` cursor the next read
    /// sends back.
    pub size: u64,
    pub truncated: bool,
}

/// Decode one capture blob: UTF-8 first, else GBK, else lossily (SPEC
/// §process.apps). Public because the control module decodes the exit tail the same
/// way.
pub fn decode(bytes: &[u8]) -> String {
    // The supervisor's own decoder, verbatim (SPEC §process.apps): UTF-8
    // first, else GBK, else the lossy last resort - one rule, not two.
    swiss_host::services::process::decode_child_output(bytes)
}

/// Mask every resolved value (8 bytes or more) in the text. The values are the
/// ones the app's references resolve to AT READ TIME; one that no longer
/// resolves cannot be masked - its value was a credential the vault no longer
/// holds (SPEC §process.apps).
pub fn mask_values(text: &str, values: &[String]) -> String {
    let mut masked = text.to_string();
    for value in values.iter().filter(|v| v.len() >= 8) {
        if !value.is_empty() {
            masked = masked.replace(value.as_str(), MASK);
        }
    }
    masked
}

/// Read the tail of one file, following the `after` cursor: only what was
/// appended past that byte offset comes back, and a file now SHORTER than the
/// cursor was rotated, so the whole (new) tail comes back flagged as a reset
/// by the caller comparing sizes (SPEC §process.apps).
pub fn read_tail(path: &Path, after: Option<u64>, values: &[String]) -> std::io::Result<Option<LogTail>> {
    // A file that does not exist yet is "no tail", not an error: an app that
    // never ran has no capture, and the API's 404 ("the log file does not
    // exist yet") hangs off this None. The OPEN is the existence decision
    // and the metadata comes from the handle, so a log deleted mid-flow -
    // between a stat and the open it once raced with - is this same None,
    // never the Err the API answers 502.
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            // Windows refuses to OPEN a directory outright, so a path that
            // is not a regular file would arrive here as an error; stat it
            // once to keep the old answer - not a log, no tail.
            match std::fs::metadata(path) {
                Ok(meta) if !meta.is_file() => return Ok(None),
                _ => return Err(err),
            }
        }
    };
    let size = match file.metadata() {
        Ok(meta) if meta.is_file() => meta.len(),
        // Unix opens a directory happily: the same "not a regular file is
        // no tail" rule, decided on the handle.
        Ok(_) => return Ok(None),
        Err(err) => return Err(err),
    };
    let start = if after.map(|a| size < a).unwrap_or(false) {
        0
    } else {
        after.unwrap_or(0).min(size)
    };
    if start == size && after.is_some() {
        // Nothing was appended; an empty answer, not a re-read.
        return Ok(Some(LogTail {
            lines: Vec::new(),
            size,
            truncated: false,
        }));
    }
    // Read at most the last [MAX_BYTES] from the end: an app-declared log can
    // be huge, and a 1 Hz follow must not materialize the whole file every
    // second. The cursor still reports the FULL size so rotation detection and
    // follow offsets keep working; bytes the window skipped say "truncated".
    let from = start.max(size.saturating_sub(MAX_BYTES));
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(from))?;
    let mut window = Vec::new();
    file.read_to_end(&mut window)?;
    let mut truncated = from > start || window.len() as u64 > MAX_BYTES;
    if window.len() as u64 > MAX_BYTES {
        // Keep the LAST MiB, then drop the first (partial) line with it.
        window = window.split_off(window.len() - MAX_BYTES as usize);
        if let Some(nl) = window.iter().position(|b| *b == b'\n') {
            window = window.split_off(nl + 1);
        }
    } else if from > start {
        // The window skipped bytes the cursor allowed: drop the first (partial)
        // line so a cut does not masquerade as a whole line.
        if let Some(nl) = window.iter().position(|b| *b == b'\n') {
            window = window.split_off(nl + 1);
        }
    }
    let text = decode(&window);
    let mut lines: Vec<String> = text.lines().map(|line| mask_values(line, values)).collect();
    if lines.len() > MAX_LINES {
        lines.drain(..lines.len() - MAX_LINES);
        truncated = true;
    }
    Ok(Some(LogTail {
        lines,
        size,
        truncated,
    }))
}

/// Whether this read was a rotation reset: the file is now SHORTER than the
/// cursor the reader sent (copy-truncate), so the tail restarts (SPEC
/// §process.apps).
pub fn was_reset(size: u64, after: Option<u64>) -> bool {
    after.map(|a| size < a).unwrap_or(false)
}

/// Resolve every env and vault value the app's command and env references
/// stand for, at read time, for masking. Missing references are skipped: a
/// value that cannot be resolved cannot be leaked either, and the mask step
/// simply has nothing for it (SPEC §process.apps).
pub fn maskable_values(
    env: &std::collections::BTreeMap<String, String>,
    program: &str,
    args: &[String],
) -> Vec<String> {
    let mut values = Vec::new();
    let mut collect = |input: &str| {
        if let Ok((_, resolved)) = swiss_core::secure::refs::resolve_strict_collect(input) {
            values.extend(resolved);
        }
    };
    collect(program);
    for arg in args {
        collect(arg);
    }
    for value in env.values() {
        collect(value);
    }
    values
}

/// The app's own log path for a declared `log` (with `~` expanded): the
/// LAST component may carry `*`/`?`, resolved to the newest match by mtime
/// on each read (SPEC §process.apps).
pub fn resolve_app_log(log: &str) -> Option<PathBuf> {
    let expanded = crate::config::expand_dir(log)?;
    let text = expanded.to_string_lossy().into_owned();
    if !text.contains('*') && !text.contains('?') {
        return Some(expanded);
    }
    let parent = expanded.parent()?.to_path_buf();
    let pattern = expanded.file_name()?.to_string_lossy().into_owned();
    let mut names: Vec<String> = std::fs::read_dir(&parent)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    // Stable order so the newest-by-mtime pick never depends on readdir order.
    names.sort();
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for name in names {
        if wildcard_match(&pattern, &name) {
            let full = parent.join(&name);
            if let Ok(mtime) = std::fs::metadata(&full).and_then(|m| m.modified()) {
                if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                    best = Some((mtime, full));
                }
            }
        }
    }
    best.map(|(_, path)| path)
}

/// A tiny glob for the log's last component: `*` any run, `?` one char,
/// everything else literal (and on Windows, case-insensitive like the fs).
fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn inner(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => inner(&p[1..], n) || (!n.is_empty() && inner(p, &n[1..])),
            (Some('?'), Some(_)) => inner(&p[1..], &n[1..]),
            (Some(a), Some(b)) => {
                let same = if cfg!(windows) {
                    a.eq_ignore_ascii_case(b)
                } else {
                    a == b
                };
                same && inner(&p[1..], &n[1..])
            }
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    inner(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-process-logs-{name}-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn copy_truncate_keeps_one_generation() {
        let dir = scratch("sweep");
        let path = dir.join("app.log");
        std::fs::write(&path, vec![b'x'; 64]).expect("write");
        // Below the threshold: a no-op.
        sweep_capture(&path).expect("sweep small");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64);
        assert!(!path.with_file_name("app.log.1").exists());

        // Above it: the copy lands in .1, the file cuts to zero, and a second
        // sweep replaces the generation instead of stacking .2.
        std::fs::write(&path, vec![b'x'; CAPTURE_SWEEP_ABOVE as usize + 1]).expect("write big");
        sweep_capture(&path).expect("sweep big");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(path.with_file_name("app.log.1").exists());
        std::fs::write(&path, vec![b'y'; CAPTURE_SWEEP_ABOVE as usize + 1]).expect("write big 2");
        sweep_capture(&path).expect("sweep big 2");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.iter().filter(|n| n.starts_with("app.log")).count(),
            2,
            "one generation only: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_shorter_than_after_resets_the_follow() {
        let dir = scratch("reset");
        let path = dir.join("app.log");
        std::fs::write(&path, "line one\nline two\n").expect("write");
        let first = read_tail(&path, None, &[]).unwrap().unwrap();
        assert_eq!(first.lines, vec!["line one", "line two"]);
        assert!(was_reset(5, Some(first.size)));
        // The sweep cuts the file to zero while the reader's cursor sat at the
        // old end: the follow restarts from the new beginning.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(0).unwrap();
        std::fs::write(&path, "fresh\n").expect("write fresh");
        let second = read_tail(&path, Some(first.size), &[]).unwrap().unwrap();
        assert_eq!(second.lines, vec!["fresh"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gbk_decodes_and_resolved_values_are_masked() {
        // A GBK-encoded Chinese sentence: invalid as UTF-8, decodable as GBK.
        let gbk: &[u8] = &[0xd6, 0xd0, 0xce, 0xc4, 0xc8, 0xd5, 0xd6, 0xbe];
        let text = decode(gbk);
        assert!(text.contains('中'), "the GBK bytes did not decode: {text:?}");

        // An 8+-byte resolved value is masked everywhere it appears; a short
        // one is not (nothing distinctive enough to hide).
        let masked = mask_values(
            "token=supersecret1 and pin=1234",
            &["supersecret1".to_string(), "1234".to_string()],
        );
        assert!(masked.contains("token="), "{masked}");
        assert!(!masked.contains("supersecret1"), "{masked}");
        assert!(masked.contains("pin=1234"), "{masked}");
    }

    #[test]
    fn the_tail_caps_lines_and_bytes() {
        let dir = scratch("caps");
        let path = dir.join("app.log");
        // MAX_LINES + a few: only the last MAX_LINES survive.
        let many: String = (0..MAX_LINES + 7).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, many).expect("write");
        let tail = read_tail(&path, None, &[]).unwrap().unwrap();
        assert_eq!(tail.lines.len(), MAX_LINES);
        assert_eq!(tail.lines.last().unwrap(), &format!("line {}", MAX_LINES + 6));
        assert!(tail.truncated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_app_log_glob_picks_the_newest_match() {
        let dir = scratch("glob");
        std::fs::write(dir.join("a-2026-10-08.log"), b"old").expect("write old");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("a-2026-10-09.log"), b"new").expect("write new");
        std::fs::write(dir.join("b-other.log"), b"nope").expect("write nope");
        let declared = format!("{}{}a-*.log", dir.display(), std::path::MAIN_SEPARATOR);
        let picked = resolve_app_log(&declared);
        assert_eq!(
            picked
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().into_owned()),
            Some("a-2026-10-09.log".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_no_tail_not_an_error() {
        // An app that never ran has no capture: the API's "the log file does
        // not exist yet" 404 hangs off this Ok(None).
        let dir = std::env::temp_dir().join(format!("swiss-log-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("never-started.log");
        assert!(read_tail(&missing, None, &[]).unwrap().is_none());
    }

    #[test]
    fn a_huge_file_reads_only_the_last_mib_but_reports_its_true_size() {
        let dir = std::env::temp_dir().join(format!("swiss-log-huge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("huge.log");
        let mut text = String::new();
        while text.len() < 2 * 1024 * 1024 {
            text.push_str(&format!("line {}\n", text.len()));
        }
        std::fs::write(&p, &text).unwrap();
        let tail = read_tail(&p, None, &[]).unwrap().unwrap();
        assert_eq!(tail.size, text.len() as u64);
        assert!(tail.truncated);
        // The first survivor is a WHOLE line: the window dropped the partial
        // one it cut through.
        assert!(tail.lines[0].starts_with("line "));
    }

    #[test]
    fn a_file_deleted_between_reads_stays_no_tail_not_an_error() {
        // The log route is polled at 1 Hz while the page is open, and the
        // app (or its rotation) can delete the file between two reads. The
        // OPEN is the existence decision, so absence at any point of the
        // call is Ok(None) - the route's 404, never the 502 an Err becomes.
        // What a unit test can simulate honestly is deletion BETWEEN calls;
        // deletion INSIDE the old stat/open window cannot be interleaved
        // deterministically without a filesystem seam, which is why the
        // window was removed rather than tested around.
        let dir = scratch("deleted");
        let path = dir.join("app.log");
        std::fs::write(&path, "one\ntwo\n").expect("write");
        let first = read_tail(&path, None, &[]).unwrap().unwrap();
        assert_eq!(first.lines, vec!["one", "two"]);
        std::fs::remove_file(&path).expect("delete mid-flow");
        assert!(read_tail(&path, None, &[]).unwrap().is_none());
        assert!(read_tail(&path, Some(first.size), &[]).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_named_as_a_log_is_no_tail_not_an_error() {
        // A path that is not a regular file was "no tail" before the
        // open-first restructure and stays one after. This is the branch
        // that keeps it: Windows refuses to OPEN a directory outright, so
        // read_tail stats once on that failure instead of letting a
        // directory named as a log become the route's 502. (Unix opens a
        // directory happily; the handle's own metadata says not-a-file.)
        let dir = scratch("dir-as-log");
        let as_log = dir.join("not-a-log");
        std::fs::create_dir_all(&as_log).expect("dir");
        assert!(read_tail(&as_log, None, &[]).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweeping_a_missing_capture_is_a_noop() {
        // The hourly tick sweeps every DECLARED app, including one that
        // never started and has no capture file yet (SPEC §process.apps):
        // that sweep is a no-op, not a warning.
        let dir = scratch("sweep-missing");
        let missing = dir.join("never-started.log");
        sweep_capture(&missing).expect("sweep missing");
        assert!(!missing.exists());
        assert!(!dir.join("never-started.log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
