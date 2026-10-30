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

//! Addressing a row that has no primary key (SPEC §data.edits): every column, with long text
//! and binary compared by their MD5.

use serde_json::{json, Map, Value};

use super::edit::{ph, typed_ph};
use super::ident::quote_ident;
use super::model::BrowseColumn;
use super::DbDialect;

// --- addressing a row without a key (SPEC §data.edits) ------------------------------------------------

/// The length past which a text or binary value addresses its row through md5() instead of
/// itself. Adminer's own line (select.inc.php:458 — there "the value is too long for the
/// URL"; here it keeps the WHERE, and the W1.7 read-back SELECT that shares it, from
/// carrying whole paragraphs as bound parameters).
pub const EDIT_ADDR_MD5_MIN: usize = 64;

/// The information_schema data_types whose values are addressable text or bytes: adminer's
/// text_type() (functions.inc.php:84 — char|text, plus enum/set on MySQL) and the blob and
/// bytea kinds is_blob() adds. Substring match because the catalog spells full names
/// ("character varying", "mediumtext", "varbinary"); no numeric or temporal type contains
/// any of these fragments.
pub(super) fn is_text_or_binary(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    [
        "char", "text", "blob", "binary", "bytea", "enum", "set", "json", "xml",
    ]
    .iter()
    .any(|frag| t.contains(frag))
}

/// The byte kinds of is_text_or_binary: values whose WIRE form is \\x hex (the adapters render
/// them so, PG bytea text format), whose bytes never round-trip a `col = ?` text bind, and
/// whose md5 address must digest the DECODED bytes — never the hex ride, never a lossy
/// string (a digest of mangled text matches nothing and the edit reads back as a false 409).
pub fn is_binary_type(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    ["blob", "binary", "bytea"]
        .iter()
        .any(|frag| t.contains(frag))
}

/// The \\x + lowercase-hex wire form of raw bytes — the string a binary column's value rides
/// the JSON API as, both dialects (PG bytea text format; MySQL rendered the same way so the
/// panel and the readback side see one spelling).
pub fn bytea_hex(bytes: &[u8]) -> String {
    format!("\\x{}", swiss_core::util::to_hex(bytes))
}

/// Decode the \\x wire form back to bytes; None when the string is not that form (then the
/// caller digests the string's own bytes — a hand-typed plain value addresses as itself).
pub fn bytea_unhex(s: &str) -> Option<Vec<u8>> {
    let hex = s.strip_prefix("\\x")?;
    if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// MD5 over the bytes, lowercase hex — the spelling both dialects' own md5() produce.
pub fn md5_hex(input: &[u8]) -> String {
    md5_digest(input)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// MD5 (RFC 1321), hand-rolled exactly the way swiss-panel's sha1 is: the digest only
/// ADDRESSES rows here (SPEC §data.edits), it guards nothing, and a crypto crate pulled into
/// the host for seventy lines of fixed arithmetic would be the heavier dependency, not the
/// lighter one. md-5 already sits in the lock file behind sqlx, but reaching into another
/// crate's transitive pocket is not a dependency policy either. Verified against the RFC's
/// own test vectors in the tests below.
fn md5_digest(msg: &[u8]) -> [u8; 16] {
    // Round constants: abs(sin(i + 1)) * 2^32, i = 0..64 — RFC 1321 §3.4.
    const K: [u32; 64] = [
        3614090360, 3905402710, 606105819, 3250441966, 4118548399, 1200080426, 2821735955,
        4249261313, 1770035416, 2336552879, 4294925233, 2304563134, 1804603682, 4254626195,
        2792965006, 1236535329, 4129170786, 3225465664, 643717713, 3921069994, 3593408605,
        38016083, 3634488961, 3889429448, 568446438, 3275163606, 4107603335, 1163531501,
        2850285829, 4243563512, 1735328473, 2368359562, 4294588738, 2272392833, 1839030562,
        4259657740, 2763975236, 1272893353, 4139469664, 3200236656, 681279174, 3936430074,
        3572445317, 76029189, 3654602809, 3873151461, 530742520, 3299628645, 4096336452,
        1126891415, 2878612391, 4237533241, 1700485571, 2399980690, 4293915773, 2240044497,
        1873313359, 4264355552, 2734768916, 1309151649, 4149444226, 3174756917, 718787259,
        3951481745,
    ];
    // Per-round shift amounts, RFC 1321 §3.4.
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;
    // Padding: 0x80, zeros to 56 mod 64, then the bit length little-endian.
    let mut data = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((msg.len() as u64).wrapping_mul(8)).to_le_bytes());
    for block in data.chunks(64) {
        let mut m = [0u32; 16];
        for (i, word) in block.chunks(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// One md5-addressed comparison — md5(col) = ? — for a text or binary value past
/// EDIT_ADDR_MD5_MIN bytes. The placeholder stays bare on purpose: the bound value is the
/// 32-char digest (text on both dialects), never the column's own type.
pub(super) fn md5_address(dialect: DbDialect, quoted: &str, bytes: &[u8], params: &mut Vec<Value>) -> String {
    params.push(json!(md5_hex(bytes)));
    format!("md5({quoted}) = {}", ph(dialect, params))
}

/// The bytes a TEXT value addresses by (its own utf-8 form) — the W4.1 fold, unchanged.
pub(super) fn text_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::String(s) => s.as_bytes().to_vec(),
        other => other.to_string().into_bytes(),
    }
}

/// The bytes a BINARY column's wire value stands for: \\x hex decodes to the raw bytes;
/// anything else (a hand-typed plain string) addresses as its own bytes.
pub(super) fn binary_wire_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::String(s) => bytea_unhex(s).unwrap_or_else(|| s.as_bytes().to_vec()),
        other => other.to_string().into_bytes(),
    }
}

/// Why a row cannot be addressed by its values: a NULL ("NULL is ambiguous" — adminer's
/// unique_array comment, functions.inc.php:331 — col = NULL matches nothing) or a column
/// the row map does not carry at all.
pub(super) enum AddressGap {
    Null(String),
    Missing(String),
}

impl AddressGap {
    pub(super) fn message(&self, op: &str) -> String {
        match self {
            AddressGap::Null(c) => format!(
                "{op} needs a non-NULL value for every column (no primary key): {c} is NULL"
            ),
            AddressGap::Missing(c) => {
                format!("{op} needs a value for every column (no primary key): {c} is missing")
            }
        }
    }
}

/// The every-column address of one row (SPEC §data.edits): `col = value` per table column,
/// typed placeholders as everywhere else, md5(col) for long text/binary values. Values come
/// from the row map the panel buffered (the pk map, which carries the whole original row on
/// a keyless table). Appends the bound parameters in comparison order.
pub(super) fn address_where(
    dialect: DbDialect,
    columns: &[BrowseColumn],
    row: &Map<String, Value>,
    params: &mut Vec<Value>,
) -> Result<String, AddressGap> {
    let mut parts: Vec<String> = Vec::with_capacity(columns.len());
    for c in columns {
        let quoted =
            quote_ident(dialect, &c.name).map_err(|_| AddressGap::Missing(c.name.clone()))?;
        let Some(v) = row.get(&c.name) else {
            return Err(AddressGap::Missing(c.name.clone()));
        };
        if v.is_null() {
            return Err(AddressGap::Null(c.name.clone()));
        }
        if is_binary_type(&c.data_type) {
            // Binary columns fold at ANY length: `col = ?` would bind the \\x hex text,
            // which can never equal the column's bytes. The digest runs over the decoded
            // bytes (binary_wire_bytes) — md5(col) in SQL sees the raw column bytes.
            parts.push(md5_address(dialect, &quoted, &binary_wire_bytes(v), params));
        } else if is_text_or_binary(&c.data_type) && value_len(v) > EDIT_ADDR_MD5_MIN {
            parts.push(md5_address(dialect, &quoted, &text_bytes(v), params));
        } else {
            params.push(v.clone());
            parts.push(format!(
                "{quoted} = {}",
                typed_ph(dialect, params, Some(c.data_type.as_str()))
            ));
        }
    }
    Ok(parts.join(" AND "))
}

/// A value's length in bytes — the >64 test of EDIT_ADDR_MD5_MIN is about payload size, not
/// character count (adminer's strlen is bytes too).
pub(super) fn value_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(a) => a.iter().map(value_len).sum::<usize>() + 2,
        Value::Object(o) => o.iter().map(|(k, v)| k.len() + value_len(v) + 4).sum(),
        other => other.to_string().len(),
    }
}

/// The ambiguous-row refusal (SPEC §data.edits): Postgres has no UPDATE … LIMIT, so an
/// every-column address matching more than one row fails the whole batch instead of
/// quietly picking a winner. MySQL clips with LIMIT 1 and reports 1, so the check there
/// never fires — kept dialect-blind because the verdict is the adapter's, not the builder's.
pub fn ambiguous_row_error(op: &str, affected: u64) -> String {
    format!(
        "ambiguous row: the {op} matched {affected} rows — refusing to pick one; the batch was rolled back"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytea_wire_form_round_trips_and_refuses_malformed_hex() {
        let bytes = vec![0x00u8, 0xFF, b'x', 0x10];
        assert_eq!(bytea_hex(&bytes), "\\x00ff7810");
        assert_eq!(bytea_unhex(&bytea_hex(&bytes)), Some(bytes));
        assert_eq!(bytea_unhex("\\x"), Some(Vec::new()));
        assert_eq!(bytea_unhex("\\x0"), None); // odd digit count
        assert_eq!(bytea_unhex("\\xzz"), None); // not hex
        assert_eq!(bytea_unhex("plain"), None); // not the wire form
        assert!(is_binary_type("bytea"));
        assert!(is_binary_type("VARBINARY(16)"));
        assert!(is_binary_type("mediumblob"));
        assert!(!is_binary_type("varchar(80)"));
        assert!(!is_binary_type("text"));
    }

    #[test]
    fn md5_matches_the_rfc1321_vectors() {
        // The RFC's own suite plus block-boundary padding cases (55 bytes pads inside one
        // block, 56 needs a second, 64 forces a whole extra one) — the same discipline
        // swiss-panel's hand-rolled sha1 follows.
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(md5_hex(&[b'x'; 100]), "aed563ecafb4bcc5654c597a421547b2");
        assert_eq!(md5_hex(&[b'a'; 55]), "ef1772b6dff9a122358552954ad0df65");
        assert_eq!(md5_hex(&[b'a'; 56]), "3b0c8ac703f828b04c6c197006d17218");
        assert_eq!(md5_hex(&[b'a'; 64]), "014842d480b571495a4a0363793f7367");
    }

    #[test]
    fn ambiguous_row_error_names_the_count() {
        let err = ambiguous_row_error("update", 2);
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("2 rows"), "{err}");
    }
}
