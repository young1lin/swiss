/*
 * Copyright 2026 The swiss authors
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

//! The asciicast v2 recorder (docs/14 §6.4).
//!
//! One file per session at `~/.mcp-gateway/terminal/<sessionId>.cast`, one JSON value per
//! line: a header object, then `[elapsed, "o", "text"]` for every chunk the shell wrote.
//! That is the whole format, which is the point of choosing it — `asciinema play` and
//! every web player already read it, so the gateway ships no player of its own and the
//! recordings outlive it.
//!
//! ## Three decisions worth stating
//!
//! **Output only.** Not a size optimisation: asciicast is an output format, and not
//! recording the keyboard is what keeps a `sudo` password out of the file. Input that is
//! not echoed does not appear in the output stream to begin with, so there is nothing to
//! filter and no filter to get wrong. `"i"` events (asciicast's optional input stream)
//! and `"r"` resize events are both deliberately not written: the first would undo the
//! sentence above, and the second is a v2 extension the spec did not ask for — the header
//! carries the geometry the session opened at.
//!
//! **A cap, and no rotation.** 8 MB, then recording stops with a marker line that says so
//! in the terminal's own output stream, so it is visible on playback rather than only in
//! a log. Silent rotation was considered and rejected in docs/14 §6.4: a session whose
//! recording quietly became "the last 8 MB" is a recording nobody can trust as evidence
//! of what happened at the start.
//!
//! **UTF-8 is reassembled across chunks.** A PTY hands over arbitrary byte boundaries and
//! a JSON string must be valid UTF-8, so a multi-byte character split across two reads
//! would become two replacement characters — which for CJK output means most of the
//! screen. The tail of an incomplete sequence is held back and prepended to the next
//! chunk; only a genuinely invalid byte becomes U+FFFD.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use tokio::time::Instant;

use swiss_core::platform::{chmod_private, mkdir_private, private_file_mode};

/// docs/14 §6.4 suggests 8 MB. A constant rather than config, like the catch-up buffer:
/// it is a line in the memory and disk budget (docs/14 §7), not a preference.
pub const RECORDING_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The terminal used for playback. xterm.js on the panel side is configured to match.
pub const RECORDING_TERM: &str = "xterm-256color";

/// A UTF-8 sequence is at most 4 bytes, so at most 3 can ever be held back.
const MAX_PENDING: usize = 3;

/// An open recording. One per session, owned by that session's driver — which is also
/// the only writer, so a line append needs no lock and no temp file.
pub struct Recorder {
    file: File,
    path: PathBuf,
    started: Instant,
    written: u64,
    /// Set once the cap has been hit and the marker written. The session keeps running;
    /// only the recording ends.
    stopped: bool,
    /// The tail of an incomplete UTF-8 sequence from the previous chunk.
    pending: Vec<u8>,
}

impl Recorder {
    /// Create `<dir>/<id>.cast` and write the header. The directory is created 0700 and
    /// the file 0600: these files hold real session output.
    pub fn create(dir: &Path, id: &str, cols: u16, rows: u16) -> std::io::Result<Recorder> {
        mkdir_private(dir);
        let path = dir.join(format!("{id}.cast"));
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(private_file_mode());
        }
        let mut file = options.open(&path)?;
        chmod_private(&path, private_file_mode());

        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            // Wall-clock seconds, as the format wants; every event time after this is
            // relative and comes from the monotonic clock instead, so a mid-session NTP
            // step cannot make a recording play backwards.
            "timestamp": swiss_core::util::now_ms() / 1000,
            "env": { "TERM": RECORDING_TERM },
        });
        let line = format!("{header}\n");
        file.write_all(line.as_bytes())?;
        Ok(Recorder {
            file,
            path,
            started: Instant::now(),
            written: line.len() as u64,
            stopped: false,
            pending: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Record one chunk of shell output. Errors are swallowed on purpose: a full disk
    /// must not take a live terminal down with it, and the session list still reports the
    /// path so the truncation is visible where the recording is.
    pub fn output(&mut self, bytes: &[u8]) {
        if self.stopped || bytes.is_empty() {
            return;
        }
        let text = self.decode(bytes);
        if text.is_empty() {
            return;
        }
        self.event(&text);
    }

    /// The end marker docs/14 §6.4 requires — written into the output stream so it is
    /// visible on playback, not just in a log nobody replays. Closing twice is a no-op.
    pub fn finish(&mut self, reason: &str) {
        if self.stopped {
            return;
        }
        // Flush a held-back partial sequence as the replacement character it turned out
        // to be: the shell is gone, so no continuation byte is coming.
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            self.event(&text);
        }
        let marker = format!("\r\n[gateway] session ended: {reason}\r\n");
        self.event(&marker);
        self.stopped = true;
        let _ = self.file.flush();
    }

    /// Append one `[elapsed, "o", text]` line, or stop at the cap.
    fn event(&mut self, text: &str) {
        let line = format!(
            "{}\n",
            serde_json::json!([self.started.elapsed().as_secs_f64(), "o", text])
        );
        if self.written + line.len() as u64 > RECORDING_MAX_BYTES {
            self.stop_at_cap();
            return;
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
    }

    /// The cap marker, written past the cap on purpose: a recording that ends without
    /// saying why is indistinguishable from one that was truncated by a crash.
    fn stop_at_cap(&mut self) {
        self.stopped = true;
        let note = format!(
            "\r\n[gateway] recording stopped at the {} MB cap; the session is still running \
             and nothing is being rotated\r\n",
            RECORDING_MAX_BYTES / (1024 * 1024)
        );
        let line = format!(
            "{}\n",
            serde_json::json!([self.started.elapsed().as_secs_f64(), "o", note])
        );
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
        let _ = self.file.flush();
    }

    /// Bytes -> a valid UTF-8 string, holding back the tail of a split character.
    fn decode(&mut self, bytes: &[u8]) -> String {
        let mut buf: Vec<u8> = Vec::with_capacity(self.pending.len() + bytes.len());
        buf.append(&mut self.pending);
        buf.extend_from_slice(bytes);

        let mut out = String::new();
        let mut rest = &buf[..];
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    // SAFETY-free equivalent of from_utf8_unchecked: valid_up_to is by
                    // definition a valid boundary, so this cannot fail.
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                    match err.error_len() {
                        // A truly invalid byte: emit U+FFFD and carry on past it.
                        Some(bad) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &rest[valid + bad..];
                        }
                        // A truncated sequence at the end: hold it for the next chunk,
                        // unless it is so long it cannot be one (defensive; from_utf8
                        // does not produce this, and dropping it would lose bytes).
                        None => {
                            let tail = &rest[valid..];
                            if tail.len() <= MAX_PENDING {
                                self.pending.extend_from_slice(tail);
                            } else {
                                out.push(char::REPLACEMENT_CHARACTER);
                            }
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = swiss_core::paths::test_home()
            .join("terminal-rec")
            .join(format!("{name}-{}", swiss_core::util::random_hex(6)));
        mkdir_private(&dir);
        dir
    }

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("the cast file is readable")
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("every line is one JSON value"))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_recording_is_a_v2_header_and_one_line_per_chunk() {
        let dir = scratch("basic");
        let mut rec = Recorder::create(&dir, "sess-1", 120, 30).expect("creates");
        rec.output(b"hello ");
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        rec.output(b"world");
        rec.finish("the shell exited");

        let out = lines(rec.path());
        assert_eq!(out[0]["version"], 2);
        assert_eq!(out[0]["width"], 120);
        assert_eq!(out[0]["height"], 30);
        assert_eq!(out[0]["env"]["TERM"], RECORDING_TERM);
        assert_eq!(out[1][1], "o");
        assert_eq!(out[1][2], "hello ");
        assert_eq!(out[2][2], "world");
        // Relative, monotonic, and increasing — the thing a player needs.
        assert!(out[2][0].as_f64().unwrap() >= 0.5, "{:?}", out[2][0]);
        assert!(out[3][2]
            .as_str()
            .unwrap()
            .contains("session ended: the shell exited"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_multi_byte_character_split_across_two_chunks_survives() {
        // The case that decides whether a Chinese session is readable on playback: the
        // PTY hands over whatever the pipe had, not whole characters.
        let dir = scratch("utf8");
        let mut rec = Recorder::create(&dir, "sess-1", 80, 24).expect("creates");
        let text = "终端".as_bytes();
        rec.output(&text[..4]); // splits the second character
        rec.output(&text[4..]);
        rec.finish("done");

        let out = lines(rec.path());
        let seen: String = out[1..3]
            .iter()
            .map(|e| e[2].as_str().unwrap_or_default())
            .collect();
        assert_eq!(seen, "终端", "{out:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_genuinely_invalid_byte_becomes_one_replacement_and_the_rest_survives() {
        let dir = scratch("invalid");
        let mut rec = Recorder::create(&dir, "sess-1", 80, 24).expect("creates");
        rec.output(&[b'a', 0xff, b'b']);
        rec.finish("done");
        let out = lines(rec.path());
        assert_eq!(out[1][2], "a\u{fffd}b");
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_stops_the_recording_with_a_visible_marker_and_never_rotates() {
        let dir = scratch("cap");
        let mut rec = Recorder::create(&dir, "sess-1", 80, 24).expect("creates");
        let chunk = vec![b'x'; 64 * 1024];
        while !rec.is_stopped() {
            rec.output(&chunk);
        }
        let out = lines(rec.path());
        let last = out.last().expect("something was written")[2]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(last.contains("recording stopped at the 8 MB cap"), "{last}");
        // No rotation: one file, and it is the beginning of the session, not the end.
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a second file means something rotated"
        );
        assert_eq!(out[1][2].as_str().unwrap().len(), 64 * 1024);

        // And past the cap the session is still usable — output simply stops being kept.
        let before = std::fs::metadata(rec.path()).unwrap().len();
        rec.output(b"after the cap");
        rec.finish("done");
        assert_eq!(std::fs::metadata(rec.path()).unwrap().len(), before);
    }

    #[tokio::test(start_paused = true)]
    async fn finishing_twice_writes_one_marker() {
        let dir = scratch("twice");
        let mut rec = Recorder::create(&dir, "sess-1", 80, 24).expect("creates");
        rec.finish("first");
        rec.finish("second");
        let out = lines(rec.path());
        assert_eq!(out.len(), 2, "header plus exactly one marker: {out:?}");
    }
}
