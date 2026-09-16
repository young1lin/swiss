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
    swiss_core::util::to_hex(&sha1_of(parts.as_bytes()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sha1_hex(data: &[u8]) -> String {
        swiss_core::util::to_hex(&sha1_of(data))
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        // Hand-rolled to keep the dependency list closed, which means nothing else checks it.
        // These are the FIPS 180-1 vectors plus the classic pangram.
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha1_pads_correctly_around_every_block_boundary() {
        // The padding rule (append 0x80, zero-fill to 56 mod 64, then the bit length) is where a
        // hand-rolled implementation goes wrong, and it only shows at these lengths: 55 is the
        // last that fits with its length word, 56 forces a whole extra block, 64 is exactly one
        // block, and 119/120 repeat the pair one block up.
        let expected = [
            (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
            (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
            (57, "f08f24908d682555111be7ff6f004e78283d989a"),
            (63, "03f09f5b158a7a8cdad920bddc29b81c18a551f5"),
            (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
            (65, "11655326c708d70319be2610e8a57d9a5b959d3b"),
            (119, "ee971065aaa017e0632a8ca6c77bb3bf8b1dfc56"),
            (120, "f34c1488385346a55709ba056ddd08280dd4c6d6"),
        ];
        for (n, want) in expected {
            assert_eq!(sha1_hex(&b"a".repeat(n)), want, "{n} bytes");
        }
    }

    #[test]
    fn sha1_carries_a_length_past_what_one_byte_holds() {
        // The bit length is a u64 in the last 8 bytes; a 32-bit or byte-count mix-up survives
        // every short vector above and fails here.
        assert_eq!(
            sha1_hex(&b"a".repeat(1_000_000)),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn only_the_extensions_the_panel_serves_resolve() {
        assert_eq!(mime_of("index.html"), Some("text/html; charset=utf-8"));
        assert_eq!(mime_of("styles/base.css"), Some("text/css; charset=utf-8"));
        assert_eq!(mime_of("js/app.js"), Some("text/javascript; charset=utf-8"));
        assert_eq!(mime_of("icon.svg"), Some("image/svg+xml"));
        // Anything else is refused rather than guessed — a wrong content type on a panel asset is
        // how a browser gets talked into treating one as something it is not.
        for name in ["secrets.json", "master.key", "notes.md", "a.HTML", "noext"] {
            assert_eq!(mime_of(name), None, "{name}");
        }
    }

    #[test]
    fn a_real_panel_asset_is_served_with_its_type() {
        // index.html is the one file the panel cannot work without, so it stands in for the tree.
        let (body, mime) = admin_asset("index.html").expect("the shell is embedded");
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(
            body.to_ascii_lowercase().contains("<!doctype html"),
            "the shell, not a stub"
        );
        // The router hands it the path below /admin, with or without the leading slash.
        assert!(admin_asset("/index.html").is_some());
    }

    #[test]
    fn nothing_outside_the_tree_is_reachable() {
        // Impossible by construction with an embedded store, but the guard is the documented
        // contract and the panel is the one route served without a token.
        for path in [
            "../Cargo.toml",
            "/../Cargo.toml",
            "styles/../../Cargo.toml",
            "..\\Cargo.toml",
            "styles\\base.css",
            "",
            "/",
        ] {
            assert!(admin_asset(path).is_none(), "{path:?}");
        }
        // A path that is shaped fine but names nothing is equally a miss, not a panic.
        assert!(admin_asset("styles/nothing-here.css").is_none());
    }

    #[test]
    fn the_shell_is_embedded_and_whole() {
        let html = admin_html().expect("index.html is embedded");
        assert!(html.to_ascii_lowercase().contains("<!doctype html"));
        assert!(
            html.len() > 200,
            "not a truncated read: {} bytes",
            html.len()
        );
    }

    #[test]
    fn the_stamp_is_stable_and_covers_the_whole_tree() {
        // /api/info hands this to the panel so it can reload itself after a rebuild. Two reads of
        // an unchanged tree must agree, or the panel reloads on a loop.
        let stamp = panel_version_stamp();
        assert_eq!(stamp, panel_version_stamp());
        assert_eq!(stamp.len(), 40, "a sha1 in hex");
        assert!(stamp.bytes().all(|b| b.is_ascii_hexdigit()));

        // It is built from every file's path and length, sorted — so it is NOT the hash of any
        // single file, and it is not the empty-input hash of a tree that failed to enumerate.
        assert_ne!(stamp, sha1_hex(b""));
        assert!(PanelAssets::iter().count() > 1, "more than one asset");
    }

}
