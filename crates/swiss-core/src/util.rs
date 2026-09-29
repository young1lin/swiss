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

//! Small shared helpers. Kept dependency-free: hex and random strings by hand rather than
//! pulling `hex`/`rand` extras the binary does not otherwise need.

use rand::TryRng;

/// `Date.now()` — epoch milliseconds, the timestamp currency of the ported code.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `Date.parse(iso)` → epoch milliseconds; None when the string is not an RFC3339 timestamp.
pub fn parse_iso_ms(iso: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::parse(iso, &Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// Lowercase hex of `n` random bytes — the id/secret recipe used across tokens, tmp names and
/// fixture keys (`randomBytes(n).toString("hex")` in the Node build).
pub fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .expect("the OS CSPRNG answered an error");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh 24-byte hex secret — the same recipe the README shows for a hand-made token.
pub fn new_secret() -> String {
    random_hex(24)
}

/// `Buffer.from(x, "hex").toString("hex")` — lowercase normalization for comparisons.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality over byte strings — `timingSafeEqual` in the Node build. Different
/// lengths return false immediately (Node's version throws; callers there pre-checked lengths).
pub fn timing_safe_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_epoch_milliseconds() {
        let now = now_ms();
        // Somewhere after 2020 and before 2100 — enough to catch seconds/nanos confusion, which is
        // the failure this would actually have: every ported timestamp is a JS `Date.now()`.
        assert!(now > 1_577_836_800_000, "{now}");
        assert!(now < 4_102_444_800_000, "{now}");
    }

    #[test]
    fn parses_an_iso_timestamp_and_refuses_anything_else() {
        // The pid file and the call log both store `iso_now()` strings and subtract them later.
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-09-08T12:00:00.500Z"),
            Some(1_788_868_800_500)
        );
        // An offset is RFC3339 too, and normalizes to the same instant as its UTC form.
        assert_eq!(
            parse_iso_ms("2026-09-08T14:00:00+02:00"),
            parse_iso_ms("2026-09-08T12:00:00Z")
        );
        for bad in ["", "not a date", "2026-09-08", "1788004800500"] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn iso_now_round_trips_through_the_parser() {
        // The two are used as a pair — `swiss status` subtracts the pid file's stamp from now.
        let stamp = crate::log::iso_now();
        let parsed = parse_iso_ms(&stamp).expect("iso_now writes what parse_iso_ms reads");
        assert!((parsed - now_ms() as i64).abs() < 5_000, "{stamp}");
    }

    #[test]
    fn random_hex_is_n_bytes_of_lowercase_hex() {
        for n in [0, 1, 8, 24, 32] {
            let s = random_hex(n);
            assert_eq!(s.len(), n * 2, "{n} bytes is {} chars", n * 2);
            assert!(s
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        }
        // It is the recipe behind every token and id in the gateway, so it had better not repeat.
        assert_ne!(random_hex(24), random_hex(24));
    }

    #[test]
    fn a_new_secret_is_a_24_byte_hex_string() {
        // The same shape the README shows for a hand-made token, so one can be swapped for the
        // other without the token check noticing.
        let s = new_secret();
        assert_eq!(s.len(), 48);
        assert!(s.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(to_hex(&[]), "");
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(to_hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn constant_time_equality_matches_only_identical_byte_strings() {
        // It guards the bearer token: a length-varying or early-returning compare leaks the
        // secret one byte at a time.
        assert!(timing_safe_eq(b"", b""));
        assert!(timing_safe_eq(b"secret", b"secret"));
        assert!(!timing_safe_eq(b"secret", b"secreT"));
        assert!(!timing_safe_eq(b"secret", b"secrets")); // Node's version throws here; this is false
        assert!(!timing_safe_eq(b"secret", b""));
        assert!(!timing_safe_eq(&[0x00], &[0x01]));
    }
}
