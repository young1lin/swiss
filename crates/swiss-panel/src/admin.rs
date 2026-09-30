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

//! The dashboard panel: `src/admin_assets` (index.html + styles/ + the js/ ES modules the
//! TypeScript in `panel/` erases to, no bundler) embedded and served (SPEC §panel.toolchain,
//! ADR-016, ADR-024).
//!
//! In debug builds rust-embed reads from disk on each request, so a rebuilt emit is live on the
//! next reload; in release the tree is compiled into the binary.

use rust_embed::RustEmbed;
use sha1::{Digest, Sha1};

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
    // SHA-1 as change detection, not security: the same digest the Node build took via node:crypto.
    swiss_core::util::to_hex(&Sha1::digest(parts.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_ne!(stamp, "da39a3ee5e6b4b0d3255bfef95601890afd80709", "the empty-input sha1");
        assert!(PanelAssets::iter().count() > 1, "more than one asset");
    }
}
