//! Small shared helpers. Kept dependency-free: hex and random strings by hand rather than
//! pulling `hex`/`rand` extras the binary does not otherwise need.

use rand::RngCore;

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
    rand::rngs::OsRng.fill_bytes(&mut bytes);
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
