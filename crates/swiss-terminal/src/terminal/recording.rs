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

//! The asciicast v2 recorder (SPEC §terminal.recording, ADR-035).
//!
//! The lines ARE the storage: a header object, then `[elapsed, "o", "text"]` for every
//! chunk the shell wrote, buffered and flushed to the terminal history as sealed 64 KiB
//! parts. `asciinema play` and every web player still read the format — reconstructed
//! byte-faithfully by the export route — so the gateway ships no player of its own and
//! the recordings still outlive it.
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
//! a log. Silent rotation was considered and rejected in SPEC §terminal.recording: a session whose
//! recording quietly became "the last 8 MB" is a recording nobody can trust as evidence
//! of what happened at the start.
//!
//! **UTF-8 is reassembled across chunks.** A PTY hands over arbitrary byte boundaries and
//! a JSON string must be valid UTF-8, so a multi-byte character split across two reads
//! would become two replacement characters — which for CJK output means most of the
//! screen. The tail of an incomplete sequence is held back and prepended to the next
//! chunk; only a genuinely invalid byte becomes U+FFFD.

use std::sync::Arc;

use tokio::time::Instant;

use super::history::{TerminalHistory, PART_BYTES};

/// SPEC §terminal.recording suggests 8 MB. A constant rather than config, like the catch-up buffer:
/// it is a line in the memory and disk budget (SPEC §terminal.budget), not a preference.
pub const RECORDING_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The terminal used for playback. xterm.js on the panel side is configured to match.
pub const RECORDING_TERM: &str = "xterm-256color";

/// A UTF-8 sequence is at most 4 bytes, so at most 3 can ever be held back.
const MAX_PENDING: usize = 3;

/// An open recording. One per session, owned by that session's driver — which is also
/// the only writer, so a part append needs no lock and no temp file.
pub struct Recorder {
    history: Arc<TerminalHistory>,
    uid: i64,
    sid: String,
    started: Instant,
    /// The plaintext bytes already handed to the store (flushed parts) plus the buffer.
    written: u64,
    /// The next part ordinal the store will take.
    part: i64,
    /// Event lines accumulated since the last flush; a flush at 64 KiB keeps the writer
    /// queue seeing parts, not PTY reads.
    buffer: Vec<u8>,
    stopped: bool,
    /// Set once the store itself refused (over-share, queue bound): the recording says
    /// so in-band and stops, exactly like the cap — the session keeps running.
    refused: bool,
    /// Set once SOMEONE already stamped the session row in-band (the cap, a store
    /// refusal): finish()'s own trailing stamp must not overwrite it. The queue is
    /// FIFO and the stamp is a whole-row UPDATE, so the last writer wins — an
    /// unconditional trailing stamp would flip capped back to 0 and replace the
    /// cap/refusal reason with the exit reason.
    finalized: bool,
    /// The tail of an incomplete UTF-8 sequence from the previous chunk.
    pending: Vec<u8>,
}

impl Recorder {
    /// Register the session's index row with the store and start its first part buffer.
    /// The header the export reconstructs carries the geometry the session opened at;
    /// every event time after it is relative and comes from the monotonic clock, so a
    /// mid-session NTP step cannot make a recording play backwards.
    pub fn create(
        history: &Arc<TerminalHistory>,
        uid: i64,
        id: &str,
        cols: u16,
        rows: u16,
    ) -> Recorder {
        let begin = history.begin(uid, id, cols, rows, RECORDING_TERM);
        let mut recorder = Recorder {
            history: Arc::clone(history),
            uid,
            sid: id.to_string(),
            started: Instant::now(),
            written: 0,
            part: 0,
            buffer: Vec::with_capacity(PART_BYTES),
            stopped: false,
            refused: false,
            finalized: false,
            pending: Vec::new(),
        };
        if begin.is_err() {
            // The index row never landed: the recording does not exist, and every
            // later part would write against no session row — invisible to the
            // list, the export and the sweep forever. The recording never starts;
            // the admission site already logged the why.
            recorder.refused = true;
            recorder.stopped = true;
            recorder.finalized = true;
        }
        recorder
    }

    /// The export URL of this recording — the session list's `recording` field. The
    /// value exists the moment the recording does, before any part is flushed.
    pub fn export_url(&self) -> String {
        format!("/api/terminal/recordings/{}/cast", self.sid)
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

    /// The end marker SPEC §terminal.recording requires — written into the output stream so it is
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
        self.flush_part();
        // The cap or a store refusal may have stamped the row already DURING the
        // events above: their stamp (capped=1, their reason) is the truth, and the
        // FIFO queue would let this trailing UPDATE overwrite it — so it only runs
        // when nobody finalized first.
        if !self.finalized && self.history.finish(self.uid, reason, false).is_err() {
            // The finish stamp was refused on admission: the row would look live
            // forever (and never sweep). The admission site already logged the why;
            // this names WHICH recording.
            swiss_core::log::warn(
                "recording finish stamp refused by the history store",
                Some(serde_json::json!({ "uid": self.uid })),
            );
        }
    }

    /// Append one `[elapsed, "o", text]` line, or stop at the cap. The stopped guard
    /// matters in finish(): the pending-flush there can trip the cap, and without the
    /// guard finish would write a SECOND cap marker and stamp the session un-capped.
    fn event(&mut self, text: &str) {
        if self.stopped {
            return;
        }
        let line = format!(
            "{}\n",
            serde_json::json!([self.started.elapsed().as_secs_f64(), "o", text])
        );
        if self.written + line.len() as u64 > RECORDING_MAX_BYTES {
            self.stop_at_cap();
            return;
        }
        self.written += line.len() as u64;
        self.buffer.extend_from_slice(line.as_bytes());
        if self.buffer.len() >= PART_BYTES {
            self.flush_part();
        }
    }

    /// Hand the buffered plaintext to the store, in PART_BYTES-sized sealed parts.
    /// A refusal (over-share, queue bound) stops the recording in-band — the store's
    /// honest no gets the same visibility as the cap's, and the session keeps
    /// running either way.
    ///
    /// Parts are cut BY BYTES, never at line boundaries: one event line can be
    /// arbitrarily long (JSON escape expansion multiplies control bytes 6x, so a
    /// 64 KiB control-byte chunk becomes a ~384 KiB line), and a single part past
    /// the 128 KiB cap every reader refuses would admit the recording and then
    /// brick its export forever. The byte stream is the storage format; line
    /// boundaries inside a part are nothing any reader ever looks at.
    fn flush_part(&mut self) {
        if self.buffer.is_empty() || self.refused {
            self.buffer.clear();
            return;
        }
        while !self.refused && !self.buffer.is_empty() {
            let take = self.buffer.len().min(PART_BYTES);
            let chunk: Vec<u8> = self.buffer.drain(..take).collect();
            let ofs = self.written - self.buffer.len() as u64 - chunk.len() as u64;
            if self.history.append_part(self.uid, self.part, ofs, &chunk).is_ok() {
                self.part += 1;
            } else {
                // The store's honest no (over-share, queue bound): stop the recording
                // HERE — writing on would leave a hole in the cumulative offsets that
                // makes the export refuse the whole session forever, and every later
                // flush would be another refused warning.
                self.refused = true;
                self.stopped = true;
                self.finalized = true;
                self.buffer.clear();
                if self.history.finish(
                    self.uid,
                    "recording stopped: the history store refused a write",
                    // capped means THE CAP: a refusal is a different ending, and
                    // mislabeling it sends the operator to inspect the wrong knob.
                    false,
                )
                .is_err()
                {
                    // The stamp itself was refused too: the row will read live until
                    // the sweep's orphan pass collects it. Named, never silent.
                    swiss_core::log::warn(
                        "refused recording's finish stamp was refused in turn",
                        Some(serde_json::json!({ "uid": self.uid })),
                    );
                }
            }
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
        self.buffer.extend_from_slice(line.as_bytes());
        self.written += line.len() as u64;
        self.flush_part();
        // The stamp rides the index row: capped=1 is what the session list shows —
        // and it is FINAL: a finish() whose events tripped the cap must not
        // overwrite it with an un-capped exit stamp afterwards.
        self.finalized = true;
        if self.history.finish(self.uid, "recording stopped at the cap", true).is_err() {
            swiss_core::log::warn(
                "cap finish stamp refused by the history store",
                Some(serde_json::json!({ "uid": self.uid })),
            );
        }
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
    use crate::terminal::history::TerminalHistory;

    async fn open(tag: &str) -> (std::sync::Arc<TerminalHistory>, String) {
        let root = swiss_core::paths::test_home()
            .join("terminal-rec")
            .join(format!("{tag}-{}", swiss_core::util::random_hex(6)));
        let sessions = root.join("terminal");
        let logs = root.join("logs");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::create_dir_all(&logs).expect("logs dir");
        let history = TerminalHistory::open(&sessions, &logs).await.expect("store opens");
        let sid = swiss_core::util::random_hex(16);
        (history, sid)
    }

    async fn lines(history: &TerminalHistory, sid: &str) -> Vec<serde_json::Value> {
        history.flush().await;
        let cast = history
            .export_cast(sid)
            .await
            .expect("exports")
            .expect("a recording exists");
        cast.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("every line is one JSON value"))
            .collect()
    }

    fn uid_for(sid: &str) -> i64 {
        // The one derivation: a test session's row key is exactly what a live open and
        // the importer both compute for the same sid.
        super::super::history::session_uid(sid)
    }

    #[tokio::test]
    async fn a_part_past_the_cap_is_split_not_stored_oversize() {
        // JSON escape expansion multiplies control bytes 6x: a 64 KiB control-byte
        // chunk becomes a ~384 KiB EVENT LINE. One line per part would seal a part
        // no reader can decode — the recording would admit and then never export.
        let (history, sid) = open("split").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        rec.output(&[b'x'; PART_BYTES + 100]);
        rec.finish("the shell exited");
        let out = lines(&history, &sid).await;
        // The oversized line arrives WHOLE — the split only changed how the
        // parts were sealed, never what the export reconstructs. (The line after
        // it is the recorder's end marker, not lost data.)
        let big = out[1][2].as_str().expect("the oversized line");
        assert_eq!(big.len(), PART_BYTES + 100, "nothing lost to the split");
        assert!(big.chars().all(|c| c == 'x'), "nothing mangled either");
    }

    #[tokio::test]
    async fn a_store_that_closes_mid_recording_stops_it_in_band() {
        // The recorder's honest-no path: once the store refuses (here: it closed
        // mid-recording, the drain landed what it could), the recorder stops
        // instead of writing a hole — and what DID land still exports cleanly.
        let root = swiss_core::paths::test_home()
            .join("terminal-rec")
            .join(format!("refused-{}", swiss_core::util::random_hex(6)));
        let sessions = root.join("terminal");
        let logs = root.join("logs");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::create_dir_all(&logs).expect("logs dir");
        let history = TerminalHistory::open(&sessions, &logs).await.expect("store opens");
        let sid = "e".repeat(32);
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        rec.output(&[b'y'; PART_BYTES + 10]);
        history.close().await.expect("drain lands part 0");
        // The store is closed now: the next flush's submit refuses, the recorder
        // stops in-band, and its finish stamp refuses the same way (the honest
        // residual of a CLOSED store — the sweep's orphan pass owns the row).
        rec.output(b"tail that must never land as a hole");
        rec.finish("the shell exited");
        assert!(rec.is_stopped(), "the refusal stopped the recording");

        let again = TerminalHistory::open(&sessions, &logs).await.expect("reopen");
        again.flush().await;
        let cast = again
            .export_cast(&sid)
            .await
            .expect("exports")
            .expect("a recording exists");
        assert!(cast.contains("yyyy"), "the landed part survives");
        assert!(
            !cast.contains("hole"),
            "the refused tail never landed — no hole, just an honest end"
        );
    }

    #[tokio::test]
    async fn a_recording_is_a_v2_header_and_one_line_per_chunk() {
        let (history, sid) = open("basic").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 120, 30);
        rec.output(b"hello ");
        // Real time: the recorder's clock is the tokio Instant, and the elapsed assert
        // below needs it to actually run (a paused clock cannot also drive the store's
        // sqlite pool — the reason these tests are not start_paused).
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        rec.output(b"world");
        rec.finish("the shell exited");

        let out = lines(&history, &sid).await;
        assert_eq!(out[0]["version"], 2);
        assert_eq!(out[0]["width"], 120);
        assert_eq!(out[0]["height"], 30);
        assert_eq!(out[0]["env"]["TERM"], RECORDING_TERM);
        assert_eq!(out[1][1], "o");
        assert_eq!(out[1][2], "hello ");
        assert_eq!(out[2][2], "world");
        // Relative, monotonic, and increasing — the thing a player needs.
        assert!(out[2][0].as_f64().unwrap() >= 0.02, "{:?}", out[2][0]);
        assert!(out[3][2]
            .as_str()
            .unwrap()
            .contains("session ended: the shell exited"));
    }

    #[tokio::test]
    async fn a_multi_byte_character_split_across_two_chunks_survives() {
        // The case that decides whether a Chinese session is readable on playback: the
        // PTY hands over whatever the pipe had, not whole characters.
        let (history, sid) = open("utf8").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        let text = "终端".as_bytes();
        rec.output(&text[..4]); // splits the second character
        rec.output(&text[4..]);
        rec.finish("done");

        let out = lines(&history, &sid).await;
        let seen: String = out[1..3]
            .iter()
            .map(|e| e[2].as_str().unwrap_or_default())
            .collect();
        assert_eq!(seen, "终端", "{out:?}");
    }

    #[tokio::test]
    async fn a_genuinely_invalid_byte_becomes_one_replacement_and_the_rest_survives() {
        let (history, sid) = open("invalid").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        rec.output(&[b'a', 0xff, b'b']);
        rec.finish("done");
        let out = lines(&history, &sid).await;
        assert_eq!(out[1][2], "a\u{fffd}b");
    }

    #[tokio::test]
    async fn the_cap_stops_the_recording_with_a_visible_marker_and_never_rotates() {
        let (history, sid) = open("cap").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        let chunk = vec![b'x'; 64 * 1024];
        while !rec.is_stopped() {
            rec.output(&chunk);
        }
        let out = lines(&history, &sid).await;
        let last = out.last().expect("something was written")[2]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(last.contains("recording stopped at the 8 MB cap"), "{last}");
        // No rotation: the FIRST chunks are still there — a store that quietly became
        // "the last 8 MB" is the same lie the file cap rejected.
        assert_eq!(out[1][2].as_str().unwrap().len(), 64 * 1024);
        assert_eq!(
            out[1][2].as_str().unwrap(),
            "x".repeat(64 * 1024),
            "the first chunk must survive to the end"
        );

        // And past the cap the session is still usable — output simply stops being kept.
        let before = rec.bytes_written();
        rec.output(b"after the cap");
        rec.finish("done");
        assert_eq!(rec.bytes_written(), before);
    }

    #[tokio::test]
    async fn finishing_twice_writes_one_marker() {
        let (history, sid) = open("twice").await;
        let mut rec = Recorder::create(&history, uid_for(&sid), &sid, 80, 24);
        rec.finish("first");
        rec.finish("second");
        let out = lines(&history, &sid).await;
        assert_eq!(out.len(), 2, "header plus exactly one marker: {out:?}");
    }
}
