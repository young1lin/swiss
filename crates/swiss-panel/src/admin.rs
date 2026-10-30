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
//! next reload. In release the tree is compiled in gzip-compressed (build.rs) and goes out as
//! stored: `Content-Encoding: gzip` to a client that takes it (every browser), inflated for one
//! that does not. Either way the body is the embedded bytes themselves, borrowed from the binary
//! rather than copied per request - mermaid alone is 2.8 MB.

use std::borrow::Cow;
use std::io::Read as _;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use sha1::{Digest, Sha1};

#[cfg(debug_assertions)]
#[derive(RustEmbed)]
#[folder = "src/admin_assets"]
struct PanelAssets;

#[cfg(not(debug_assertions))]
#[derive(RustEmbed)]
#[folder = "$OUT_DIR/admin_assets_gz"]
struct PanelAssets;

/// Whether the embedded bytes are gzip: build.rs compresses the tree for release builds only.
const STORED_GZIP: bool = cfg!(not(debug_assertions));

/// How long a browser may keep a response.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// The shell: answered behind the session gate, so never stored.
    Never,
    /// An asset: stored, but revalidated on every use (`no-cache` + ETag). A new build is picked
    /// up on the next load exactly as under `no-store`, while an unchanged module costs a bodiless
    /// 304 instead of its bytes again.
    Revalidate,
}

/// The dashboard shell, served no-store so the browser always gets the latest (dev reload; in
/// release the embedded copy moves only with a new binary).
pub fn shell(request: &HeaderMap) -> Option<Response> {
    let file = PanelAssets::get("index.html")?;
    let hash = file.metadata.sha256_hash();
    Some(respond(file.data, hash, STORED_GZIP, "text/html; charset=utf-8", request, Keep::Never))
}

/// Content types the panel actually serves; anything else is refused rather than guessed.
fn mime_of(name: &str) -> Option<&'static str> {
    match name.rsplit('.').next()? {
        "html" => Some("text/html; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        // .mjs: a vendored upstream ES module keeps its own name (js/vendor/markdown-it).
        "js" | "mjs" => Some("text/javascript; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Serve one file under the panel's asset tree (/admin/styles/…, /admin/js/…). `url_path` is the
/// request path below /admin (already percent-decoded by the router). Only known extensions
/// resolve, and the normalized path must stay inside the tree — a traversal-ish request gets a
/// 404, never a file from outside it. (With the embedded store an escape is impossible by
/// construction; the guard stays because it is the documented contract.)
pub fn asset(url_path: &str, request: &HeaderMap) -> Option<Response> {
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
    let hash = file.metadata.sha256_hash();
    Some(respond(file.data, hash, STORED_GZIP, mime, request, Keep::Revalidate))
}

/// One embedded file as a response. `stored` is the bytes as embedded (gzip when `stored_gzip`),
/// `hash` their SHA-256 (rust-embed's, a compile-time constant in release). The ETag names the
/// encoding sent as well as the bytes, since gzip and identity are two representations of one URL.
fn respond(
    stored: Cow<'static, [u8]>,
    hash: [u8; 32],
    stored_gzip: bool,
    mime: &'static str,
    request: &HeaderMap,
    keep: Keep,
) -> Response {
    let gzip = stored_gzip && accepts_gzip(request);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if keep == Keep::Never { "no-store" } else { "no-cache" }),
    );
    if stored_gzip {
        headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if keep == Keep::Revalidate {
        let etag = format!("\"{}{}\"", swiss_core::util::to_hex(&hash), if gzip { "-gz" } else { "" });
        if let Ok(value) = HeaderValue::from_str(&etag) {
            headers.insert(header::ETAG, value);
        }
        if none_match(request, &etag) {
            return (StatusCode::NOT_MODIFIED, headers).into_response();
        }
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    let body = if gzip {
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        Body::from(stored)
    } else if stored_gzip {
        // Only a client that refuses gzip lands here (curl without --compressed, a bare test
        // request); the inflated copy lives for this one response.
        let mut raw = Vec::with_capacity(stored.len() * 4);
        if flate2::read::GzDecoder::new(&stored[..]).read_to_end(&mut raw).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "panel asset does not inflate").into_response();
        }
        Body::from(raw)
    } else {
        Body::from(stored)
    };
    (headers, body).into_response()
}

/// Whether Accept-Encoding takes gzip (RFC 9110 §12.5.3): a `gzip` or `x-gzip` entry decides,
/// else a `*` entry does, and a q of zero (or one that does not parse) refuses. No header means
/// identity only.
fn accepts_gzip(request: &HeaderMap) -> bool {
    let mut star = false;
    let entries = request
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','));
    for entry in entries {
        let (coding, params) = entry.split_once(';').unwrap_or((entry, ""));
        let weight = params
            .split(';')
            .map(str::trim)
            .find_map(|param| param.strip_prefix("q=").or_else(|| param.strip_prefix("Q=")))
            .map_or(1.0, |q| q.trim().parse::<f32>().unwrap_or(0.0));
        let coding = coding.trim();
        if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") {
            return weight > 0.0;
        }
        if coding == "*" {
            star = weight > 0.0;
        }
    }
    star
}

/// Whether If-None-Match already names `etag`, or is `*`. The comparison is the weak one RFC 9110
/// §13.1.2 asks of this header, so a `W/` a proxy put in front still matches.
fn none_match(request: &HeaderMap, etag: &str) -> bool {
    request
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}

/// A change-detection stamp over the panel tree: every file's relative path and content digest
/// (rust-embed's SHA-256), sorted and hashed. Any edit to the shell, a stylesheet or one JS
/// module flips it - a same-length edit too, which the content length that stood in before
/// could miss - and that is what /api/info hands the panel so it can reload itself after a
/// rebuild. A rebuild that changes nothing re-stamps to the same value, since build.rs's gzip is
/// deterministic. The digests are compile-time constants in release; a debug build hashes
/// each file as it reads it from disk.
pub fn panel_version_stamp() -> String {
    let mut names: Vec<_> = PanelAssets::iter().collect();
    names.sort();
    let mut digest = Sha1::new();
    for name in names {
        if let Some(file) = PanelAssets::get(&name) {
            digest.update(name.as_bytes());
            digest.update(b":");
            digest.update(file.metadata.sha256_hash());
            digest.update(b"|");
        }
    }
    // SHA-1 as change detection, not security: the same digest the Node build took via node:crypto.
    swiss_core::util::to_hex(&digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    async fn body_of(response: Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("an in-memory body")
            .to_vec()
    }

    fn request(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(name, HeaderValue::from_str(value).expect("a test header"));
        }
        headers
    }

    fn header_of(response: &Response, name: header::HeaderName) -> Option<&str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[test]
    fn only_the_extensions_the_panel_serves_resolve() {
        assert_eq!(mime_of("index.html"), Some("text/html; charset=utf-8"));
        assert_eq!(mime_of("styles/base.css"), Some("text/css; charset=utf-8"));
        assert_eq!(mime_of("js/app.js"), Some("text/javascript; charset=utf-8"));
        assert_eq!(mime_of("js/vendor/markdown-it/15.0.2/markdown-it.esm.min.mjs"), Some("text/javascript; charset=utf-8"));
        assert_eq!(mime_of("icon.svg"), Some("image/svg+xml"));
        // Anything else is refused rather than guessed — a wrong content type on a panel asset is
        // how a browser gets talked into treating one as something it is not.
        for name in ["secrets.json", "master.key", "notes.md", "a.HTML", "noext"] {
            assert_eq!(mime_of(name), None, "{name}");
        }
    }

    #[tokio::test]
    async fn a_real_panel_asset_is_served_with_its_type() {
        // index.html is the one file the panel cannot work without, so it stands in for the tree.
        let response = asset("index.html", &HeaderMap::new()).expect("the shell is embedded");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, header::CONTENT_TYPE), Some("text/html; charset=utf-8"));
        assert_eq!(header_of(&response, header::CACHE_CONTROL), Some("no-cache"));
        let body = String::from_utf8(body_of(response).await).expect("utf-8");
        assert!(body.to_ascii_lowercase().contains("<!doctype html"), "the shell, not a stub");
        // The router hands it the path below /admin, with or without the leading slash.
        assert!(asset("/index.html", &HeaderMap::new()).is_some());
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
            assert!(asset(path, &HeaderMap::new()).is_none(), "{path:?}");
        }
        // A path that is shaped fine but names nothing is equally a miss, not a panic.
        assert!(asset("styles/nothing-here.css", &HeaderMap::new()).is_none());
    }

    #[tokio::test]
    async fn the_shell_is_embedded_whole_and_never_stored() {
        let response = shell(&HeaderMap::new()).expect("index.html is embedded");
        assert_eq!(header_of(&response, header::CACHE_CONTROL), Some("no-store"));
        // Behind the session gate: no validator, so nothing invites a browser to keep it.
        assert_eq!(header_of(&response, header::ETAG), None);
        let html = String::from_utf8(body_of(response).await).expect("utf-8");
        assert!(html.to_ascii_lowercase().contains("<!doctype html"));
        assert!(html.len() > 200, "not a truncated read: {} bytes", html.len());
    }

    #[tokio::test]
    async fn an_unchanged_asset_revalidates_to_a_bodiless_304() {
        let first = asset("styles/base.css", &HeaderMap::new()).expect("embedded");
        let etag = header_of(&first, header::ETAG).expect("an asset carries an ETag").to_string();
        assert!(etag.starts_with('"') && etag.ends_with('"') && etag.len() == 66, "{etag}");

        // The browser's revalidation: the tag it holds, alone, weak-prefixed, or in a list.
        let weak = format!("W/{etag}");
        let listed = format!("\"other\", {etag}");
        for held in [etag.as_str(), weak.as_str(), listed.as_str(), "*"] {
            let again = asset("styles/base.css", &request(&[(header::IF_NONE_MATCH, held)])).expect("embedded");
            assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{held}");
            assert_eq!(header_of(&again, header::ETAG), Some(etag.as_str()));
            assert_eq!(header_of(&again, header::CACHE_CONTROL), Some("no-cache"));
            assert!(body_of(again).await.is_empty(), "a 304 carries no body");
        }
        // A tag from another build is not this file: the bytes come back whole.
        let stale = asset("styles/base.css", &request(&[(header::IF_NONE_MATCH, "\"0000\"")])).expect("embedded");
        assert_eq!(stale.status(), StatusCode::OK);
        assert!(!body_of(stale).await.is_empty());
    }

    #[tokio::test]
    async fn a_gzip_stored_file_goes_out_as_stored_or_inflated() {
        // What a release build embeds: debug tests read the raw tree, so this one gzips its own.
        let raw = b"export const answer = 42;\n".repeat(200);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&raw).expect("in memory");
        let gz = gz.finish().expect("in memory");
        let hash = [7u8; 32];
        let send = |pairs: &[(header::HeaderName, &str)]| {
            respond(Cow::Owned(gz.clone()), hash, true, "text/javascript; charset=utf-8", &request(pairs), Keep::Revalidate)
        };

        // A browser: the stored bytes as they are, marked, and keyed on what the client accepts.
        let browser = send(&[(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")]);
        assert_eq!(header_of(&browser, header::CONTENT_ENCODING), Some("gzip"));
        assert_eq!(header_of(&browser, header::VARY), Some("accept-encoding"));
        let gz_tag = header_of(&browser, header::ETAG).expect("tagged").to_string();
        assert!(gz_tag.ends_with("-gz\""), "{gz_tag}");
        assert_eq!(body_of(browser).await, gz);

        // A client that takes no gzip gets the file inflated, under the identity tag.
        for accept in [None, Some("identity"), Some("gzip;q=0"), Some("br, *;q=0"), Some("gzip;q=nope")] {
            let pairs: Vec<_> = accept.map(|a| (header::ACCEPT_ENCODING, a)).into_iter().collect();
            let plain = send(&pairs);
            assert_eq!(header_of(&plain, header::CONTENT_ENCODING), None, "{accept:?}");
            assert_eq!(header_of(&plain, header::VARY), Some("accept-encoding"));
            let tag = header_of(&plain, header::ETAG).expect("tagged").to_string();
            assert_ne!(tag, gz_tag, "two representations, two tags");
            assert_eq!(body_of(plain).await, raw, "{accept:?}");
        }

        // The tag held for one encoding does not revalidate the other.
        let other = send(&[(header::IF_NONE_MATCH, gz_tag.as_str())]);
        assert_eq!(other.status(), StatusCode::OK);
        let same = send(&[(header::ACCEPT_ENCODING, "gzip"), (header::IF_NONE_MATCH, gz_tag.as_str())]);
        assert_eq!(same.status(), StatusCode::NOT_MODIFIED);
    }

    #[test]
    fn accept_encoding_is_read_by_its_weights() {
        let takes = |value: &str| accepts_gzip(&request(&[(header::ACCEPT_ENCODING, value)]));
        assert!(takes("gzip"));
        assert!(takes("GZIP"));
        assert!(takes("x-gzip"));
        assert!(takes("br;q=1.0, gzip;q=0.5"));
        assert!(takes("*"));
        assert!(takes("gzip ; q=0.001"));
        assert!(!takes("gzip;q=0"));
        assert!(!takes("gzip;q=0.000"));
        assert!(!takes("br, deflate"));
        assert!(!takes("identity"));
        // An explicit gzip entry outranks the wildcard, whichever way round they come.
        assert!(!takes("*, gzip;q=0"));
        assert!(!takes("gzip;q=0, *"));
        assert!(takes("*;q=0, gzip"));
        assert!(!accepts_gzip(&HeaderMap::new()), "no header: identity only");
        // Two header lines read as one list.
        assert!(accepts_gzip(&request(&[(header::ACCEPT_ENCODING, "br"), (header::ACCEPT_ENCODING, "gzip")])));
    }

    #[test]
    fn the_stamp_is_stable_and_covers_the_whole_tree() {
        // /api/info hands this to the panel so it can reload itself after a rebuild. Two reads of
        // an unchanged tree must agree, or the panel reloads on a loop.
        let stamp = panel_version_stamp();
        assert_eq!(stamp, panel_version_stamp());
        assert_eq!(stamp.len(), 40, "a sha1 in hex");
        assert!(stamp.bytes().all(|b| b.is_ascii_hexdigit()));

        // It is built from every file's path and digest, sorted — so it is NOT the hash of any
        // single file, and it is not the empty-input hash of a tree that failed to enumerate.
        assert_ne!(stamp, "da39a3ee5e6b4b0d3255bfef95601890afd80709", "the empty-input sha1");
        assert!(PanelAssets::iter().count() > 1, "more than one asset");
    }
}
