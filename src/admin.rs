//! The dashboard panel — port of `admin.ts`: the Node build's `src/admin` tree (index.html +
//! styles/ + js/ ES modules, no bundler), copied VERBATIM into `src/admin_assets` and embedded
//! (ADR-009: the panel is the spec for the admin API — never edited here).
//!
//! In debug builds rust-embed reads from disk on each request, so a saved edit is live on the
//! next reload, exactly like the Node build's read-per-request; in release the tree is compiled
//! into the binary.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "src/admin_assets"]
struct PanelAssets;

/// The dashboard shell, served no-store so the browser always gets the latest (dev reload; in
/// release the embedded copy moves only with a new binary).
pub fn admin_html() -> Option<String> {
    PanelAssets::get("index.html").map(|f| String::from_utf8_lossy(&f.data).into_owned())
}

/// Content types the panel actually serves; anything else is refused rather than guessed.
fn mime_of(name: &str) -> Option<&'static str> {
    match name.rsplit('.').next()? {
        "html" => Some("text/html; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Serve one file under the panel's asset tree (/admin/styles/…, /admin/js/…). `url_path` is the
/// request path below /admin (already percent-decoded by the router). Only known extensions
/// resolve, and the normalized path must stay inside the tree — a traversal-ish request gets a
/// 404, never a file from outside it. (With the embedded store an escape is impossible by
/// construction; the guard stays because it is the documented contract.)
pub fn admin_asset(url_path: &str) -> Option<(String, &'static str)> {
    // Backslash and ".." segments never name an embedded file — they are refused, not resolved.
    if url_path.contains('\\') || url_path.split('/').any(|seg| seg == "..") {
        return None;
    }
    let rel = url_path.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    let mime = mime_of(rel)?;
    let file = PanelAssets::get(rel)?;
    Some((String::from_utf8_lossy(&file.data).into_owned(), mime))
}

/// A change-detection stamp over the panel tree: every file's relative path and content length,
/// walked and hashed. Any edit to the shell, a stylesheet or one JS module flips it, which is
/// what /api/info hands the panel so it can reload itself after a rebuild. The Node build hashed
/// name + mtime; the embedded tree has no mtimes, so the content length stands in — the stamp
/// only needs to change when the panel changes, and a rebuild that changes nothing re-stamps to
/// the same value, which is the desired behavior anyway.
pub fn panel_version_stamp() -> String {
    use std::fmt::Write as _;
    let mut parts = String::new();
    let mut names: Vec<String> = PanelAssets::iter().map(|n| n.to_string()).collect();
    names.sort();
    for name in names {
        let len = PanelAssets::get(&name).map(|f| f.data.len()).unwrap_or(0);
        let _ = write!(parts, "{name}:{len}|");
    }
    crate::util::to_hex(&sha1_of(parts.as_bytes()))
}

/// SHA-1 over the stamp input — change detection, not security (the same call the Node build
/// made via node:crypto). Hand-rolled to keep the dependency list closed: SHA-1 is 70 lines.
fn sha1_of(data: &[u8]) -> [u8; 20] {
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}
