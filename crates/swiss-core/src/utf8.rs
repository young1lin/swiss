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

//! Byte windows that end on character boundaries (SPEC §remote.utf8).
//!
//! Every place that turns a byte range into text — a live run's cursor window, a
//! recorded run's file window, a tail ring, a streamed `cat` — used to cut at an
//! arbitrary byte offset and `from_utf8_lossy` the piece. A three-byte 汉字 straddling
//! the cut became two U+FFFD, one at the end of this read and one at the start of the
//! next; the CLI polls every 128 KiB, so a Chinese build log showed it reliably. The
//! rule now: a window never ends inside a sequence and never starts inside one. What
//! falls off the end is the next window's first bytes; what a window's start lands in
//! the middle of (an evicted ring, a tail kept by byte count) is skipped. Only bytes
//! that are genuinely not UTF-8 are still replaced.
//!
//! Three pure functions over slices so the callers keep their own buffers and cursors.

/// The length of the longest prefix of `bytes` that does not end inside a multi-byte
/// sequence. A trailing lead byte with too few continuation bytes after it is left
/// out; anything that is already not UTF-8 is left in (the caller's lossy decode marks
/// it). Never trims more than three bytes, and never trims a complete window down to
/// nothing for a reason other than "this is the first three bytes of one character".
pub fn char_boundary_end(bytes: &[u8]) -> usize {
    let len = bytes.len();
    // Walk back over at most three continuation bytes to find the last lead byte.
    let mut i = len;
    let mut trailing = 0usize;
    while i > 0 && trailing < 3 && (bytes[i - 1] & 0b1100_0000) == 0b1000_0000 {
        i -= 1;
        trailing += 1;
    }
    if i == 0 {
        // Nothing but continuation bytes (or empty): not the tail of a sequence we can
        // complete; keep everything and let the decoder mark it.
        return len;
    }
    let lead = bytes[i - 1];
    let need = match lead {
        0b1100_0000..=0b1101_1111 => 2,
        0b1110_0000..=0b1110_1111 => 3,
        0b1111_0000..=0b1111_0111 => 4,
        // ASCII or an invalid lead: the trailing bytes are not ours to hold back.
        _ => return len,
    };
    if trailing + 1 < need {
        i - 1
    } else {
        len
    }
}

/// The offset of the first byte of `bytes` that is not a continuation byte — where a
/// window that was cut mid-character can honestly begin. At most three bytes are
/// skipped; a slice of nothing but continuation bytes is returned whole (offset 0) so
/// the decoder can mark it rather than the window silently emptying.
pub fn char_boundary_start(bytes: &[u8]) -> usize {
    let mut i = 0usize;
    while i < bytes.len() && i < 3 && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
        i += 1;
    }
    if i == bytes.len() && !bytes.is_empty() {
        0
    } else {
        i
    }
}

/// `bytes` trimmed at both ends to character boundaries — the slice a lossy decode of a
/// window should see.
pub fn window(bytes: &[u8]) -> &[u8] {
    let start = char_boundary_start(bytes);
    let rest = &bytes[start..];
    &rest[..char_boundary_end(rest)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAN: &[u8] = "中".as_bytes(); // e4 b8 ad
    const EMOJI: &[u8] = "😀".as_bytes(); // f0 9f 98 80
    const E_ACUTE: &[u8] = "é".as_bytes(); // c3 a9

    #[test]
    fn a_complete_window_is_left_alone() {
        for s in ["", "abc", "中文", "café", "a😀b"] {
            let b = s.as_bytes();
            assert_eq!(char_boundary_end(b), b.len(), "{s:?}");
            assert_eq!(char_boundary_start(b), 0, "{s:?}");
            assert_eq!(window(b), b, "{s:?}");
        }
    }

    #[test]
    fn a_trailing_partial_sequence_is_held_back_whatever_its_width() {
        for (ch, name) in [(E_ACUTE, "2-byte"), (HAN, "3-byte"), (EMOJI, "4-byte")] {
            for cut in 1..ch.len() {
                let mut b = b"ok ".to_vec();
                b.extend_from_slice(&ch[..cut]);
                assert_eq!(char_boundary_end(&b), 3, "{name} cut at {cut}");
                assert_eq!(window(&b), b"ok ", "{name} cut at {cut}");
            }
        }
    }

    #[test]
    fn a_leading_partial_sequence_is_skipped_whatever_its_width() {
        for (ch, name) in [(E_ACUTE, "2-byte"), (HAN, "3-byte"), (EMOJI, "4-byte")] {
            for cut in 1..ch.len() {
                let mut b = ch[cut..].to_vec();
                b.extend_from_slice(b" ok");
                assert_eq!(char_boundary_start(&b), ch.len() - cut, "{name} cut at {cut}");
                assert_eq!(window(&b), b" ok", "{name} cut at {cut}");
            }
        }
    }

    #[test]
    fn the_two_halves_of_a_cut_character_decode_clean_when_rejoined() {
        // The property the cursor windows rely on: read 1 stops before the partial
        // character, read 2 starts at it - no U+FFFD in either.
        let text = "日志：编译完成";
        let bytes = text.as_bytes();
        let cut = 7; // inside the second 汉字
        let first = char_boundary_end(&bytes[..cut]);
        let a = String::from_utf8_lossy(&bytes[..first]);
        let b = String::from_utf8_lossy(&bytes[first..]);
        assert!(!a.contains('\u{FFFD}') && !b.contains('\u{FFFD}'));
        assert_eq!(format!("{a}{b}"), text);
    }

    #[test]
    fn genuinely_invalid_bytes_are_not_trimmed_away() {
        // A stray continuation byte after ASCII is not the start of anything: keep it,
        // the decoder replaces it - trimming would hide data.
        assert_eq!(char_boundary_end(b"ab\x80"), 3);
        // An invalid lead (0xff) with nothing after it: same.
        assert_eq!(char_boundary_end(b"ab\xff"), 3);
        // Nothing but continuation bytes: returned whole, both ends.
        assert_eq!(char_boundary_start(b"\x80\x80"), 0);
        assert_eq!(window(b"\x80\x80"), b"\x80\x80");
    }

    #[test]
    fn at_most_three_bytes_move_at_either_end() {
        // Four continuation bytes cannot be the tail of one character (max 3): the
        // window is returned whole rather than trimmed into the previous read.
        let mut b = b"x".to_vec();
        b.extend_from_slice(&[0x80, 0x80, 0x80, 0x80]);
        assert_eq!(char_boundary_end(&b), b.len());
        assert_eq!(char_boundary_start(&[0x80, 0x80, 0x80, 0x80, b'y']), 3);
    }
}
