/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Anchor matching (SPEC §memory.brief): a small hand-written glob - `*` within one
//! path segment, `**` across segments, `?` one character - plus the directory rule
//! (a path under an anchored directory matches it). Paths are repo-relative and
//! `/`-separated by the time they get here.

use crate::model::Anchor;

/// Whether `path` matches the glob `pattern`. Dynamic programming over
/// (pattern, path) positions: linear in their product, never exponential.
pub fn matches(pattern: &str, path: &str) -> bool {
    once(pattern, path, false)
}

/// Whether `path` matches `pattern` or lies under a directory that does (`crates/*` owns
/// `crates/x/src/lib.rs`): one pass, in which a match may also end at any `/`.
pub fn matches_or_under(pattern: &str, path: &str) -> bool {
    once(pattern, path, true)
}

/// One path against a pattern split on the spot, with nothing kept: the literal head before
/// the first wildcard is compared as is (a repo's globs mostly share a long one), and whatever
/// matches holds the literal pieces after it in order - most paths fail that without the table.
fn once(pattern: &str, path: &str, under: bool) -> bool {
    let cut = pattern.find(['*', '?']).unwrap_or(pattern.len());
    let Some(rest) = path.strip_prefix(&pattern[..cut]) else { return false };
    in_order(pieces(&pattern[cut..]), rest) && table(&pattern[cut..].chars().collect::<Vec<char>>(), rest, under)
}

/// A glob split once, for testing many paths (coverage reads every file of a checkout).
pub struct Glob<'a> {
    head: &'a str,
    pieces: Vec<&'a str>,
    rest: Vec<char>,
}

impl<'a> Glob<'a> {
    pub fn new(pattern: &'a str) -> Glob<'a> {
        let cut = pattern.find(['*', '?']).unwrap_or(pattern.len());
        Glob { head: &pattern[..cut], pieces: pieces(&pattern[cut..]).collect(), rest: pattern[cut..].chars().collect() }
    }

    /// [matches_or_under] for this glob.
    pub fn matches_or_under(&self, path: &str) -> bool {
        let Some(rest) = path.strip_prefix(self.head) else { return false };
        in_order(self.pieces.iter().copied(), rest) && table(&self.rest, rest, true)
    }
}

/// The literal pieces between a glob's wildcards, in order. The `/` after a `**` may be
/// skipped (`a/**/b` matches `a/b`), so it is no part of the piece after it.
fn pieces(pattern: &str) -> impl Iterator<Item = &str> {
    let mut tail = Some(pattern);
    let mut globstar = false;
    std::iter::from_fn(move || loop {
        let now = tail?;
        let end = now.find(['*', '?']).unwrap_or(now.len());
        let piece = if globstar { now[..end].strip_prefix('/').unwrap_or(&now[..end]) } else { &now[..end] };
        if end == now.len() {
            tail = None;
        } else {
            globstar = now[end..].starts_with("**");
            tail = Some(&now[end + if globstar { 2 } else { 1 }..]);
        }
        if !piece.is_empty() {
            return Some(piece);
        }
    })
}

/// Whether `path` holds `pieces` in order.
fn in_order<'p>(mut pieces: impl Iterator<Item = &'p str>, path: &str) -> bool {
    let mut from = 0;
    pieces.all(|piece| match path[from..].find(piece) {
        Some(at) => {
            from += at + piece.len();
            true
        }
        None => false,
    })
}

/// The table over what follows the head: `p` the pattern's rest, `rest` the path's.
fn table(p: &[char], rest: &str, under: bool) -> bool {
    let s: Vec<char> = rest.chars().collect();
    let mut memo = vec![None; (p.len() + 1) * (s.len() + 1)];
    step(p, &s, 0, 0, under, &mut memo)
}

fn step(p: &[char], s: &[char], pi: usize, si: usize, under: bool, memo: &mut Vec<Option<bool>>) -> bool {
    let slot = pi * (s.len() + 1) + si;
    if let Some(known) = memo[slot] {
        return known;
    }
    let result = if pi == p.len() {
        si == s.len() || (under && s[si] == '/')
    } else if p[pi] == '*' && p.get(pi + 1) == Some(&'*') {
        // `**`: any run, slashes included. `a/**/b` also matches `a/b`, so a
        // following `/` may be skipped - but only at a segment boundary.
        let rest = pi + 2;
        let mut found = false;
        for end in si..=s.len() {
            if step(p, s, rest, end, under, memo) {
                found = true;
                break;
            }
            if p.get(rest) == Some(&'/') && (end == si || s[end - 1] == '/') && step(p, s, rest + 1, end, under, memo) {
                found = true;
                break;
            }
        }
        found
    } else if p[pi] == '*' {
        let mut found = false;
        for end in si..=s.len() {
            if step(p, s, pi + 1, end, under, memo) {
                found = true;
                break;
            }
            if end < s.len() && s[end] == '/' {
                break;
            }
        }
        found
    } else if si < s.len() && ((p[pi] == '?' && s[si] != '/') || p[pi] == s[si]) {
        step(p, s, pi + 1, si + 1, under, memo)
    } else {
        false
    };
    memo[slot] = Some(result);
    result
}

/// Normalize a caller's path the way anchors are stored: `\` to `/`, no leading
/// `./`, no trailing `/`.
pub fn normalize(path: &str) -> String {
    let mut out = path.trim().replace('\\', "/");
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.to_string();
    }
    out.trim_end_matches('/').to_string()
}

/// Whether a concrete repo-relative `path` touches `anchor`: exact, glob, or the
/// path lies under an anchored directory.
pub fn anchor_matches(anchor: &Anchor, path: &str) -> bool {
    if anchor.glob {
        return matches(&anchor.path, path);
    }
    let base = anchor.path.trim_end_matches('/');
    path == base || (path.len() > base.len() && path.starts_with(base) && path.as_bytes()[base.len()] == b'/')
}

/// Whether two anchors overlap (association's anchor signal): equal paths, or one
/// side's glob matches the other's concrete path.
pub fn anchors_overlap(a: &Anchor, b: &Anchor) -> bool {
    a.path == b.path || (a.glob && !b.glob && matches(&a.path, &b.path)) || (b.glob && !a.glob && matches(&b.path, &a.path))
}

/// Whether a query names a file: a `/` or `\`, or a final segment with a short
/// lower-case extension (`store.rs`, `memory.ts`, `ci.yaml`) - not `Type.method`.
pub fn looks_like_path(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return false;
    }
    if text.contains('/') || text.contains('\\') {
        return true;
    }
    match text.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && ext.chars().any(|c| c.is_ascii_lowercase())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(path: &str) -> Anchor {
        Anchor { path: path.into(), glob: path.contains('*') || path.contains('?'), hash: None }
    }

    #[test]
    fn single_star_stays_inside_a_segment() {
        assert!(matches("src/*.rs", "src/lib.rs"));
        assert!(!matches("src/*.rs", "src/deep/lib.rs"));
        assert!(matches("src/?.rs", "src/a.rs"));
        assert!(!matches("src/?.rs", "src/ab.rs"));
    }

    #[test]
    fn double_star_crosses_segments_and_may_be_empty() {
        assert!(matches("crates/**/store.rs", "crates/swiss-memory/src/store.rs"));
        assert!(matches("crates/**/store.rs", "crates/store.rs"));
        assert!(!matches("crates/**/store.rs", "crates/xstore.rs"));
        assert!(matches("docs/**", "docs/a/b/c.md"));
        assert!(matches("**/*.ts", "panel/src/views/memory.ts"));
    }

    #[test]
    fn many_stars_do_not_explode() {
        let pattern = "*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        let path = "a".repeat(300);
        assert!(!matches(pattern, &path));
    }

    #[test]
    fn a_match_may_end_at_a_directory_in_one_pass() {
        assert!(matches_or_under("crates/*", "crates/swiss-panel/panel/src/app.ts"));
        assert!(matches_or_under("src/**/Act*.java", "src/a/ActX.java"));
        assert!(matches_or_under("src/**/Act*.java", "src/ActX.java"));
        assert!(matches_or_under("src/main*", "src/main/x.java"));
        assert!(!matches_or_under("src/*.rs", "src/deep/lib.rs"));
        assert!(!matches_or_under("src/**/Act*.java", "src/a/Activity.kt"));
        assert!(!matches("src/**/Act*.java", "src/a/ActX.java/inner.txt"));
        // The same answer as matching the path and each of its directories in turn.
        let patterns = [
            "src/**/Act*.java", "crates/*", "crates/*.rs", "**/*.ts", "a/**", "a/**/b", "a/?/c", "*", "**", "docs/文档*/**",
            "x/**/*/y", "a*b/c", "a/b", "?",
        ];
        let paths = [
            "src/ActX.java", "src/a/b/ActY.java", "src/a/Act/inner.txt", "crates/x/y.rs", "crates/x.rs", "a/b", "a/b/c",
            "a/b/c/d", "a/x/c/d", "panel/src/a.ts", "docs/文档一/a/b.md", "x/y", "x/q/y", "x/q/r/y/z", "ab/c/d", "axxb/c", "b",
        ];
        for pattern in patterns {
            for path in paths {
                let one_by_one = matches(pattern, path) || path.match_indices('/').any(|(at, _)| matches(pattern, &path[..at]));
                assert_eq!(matches_or_under(pattern, path), one_by_one, "{pattern} vs {path}");
            }
        }
    }

    #[test]
    fn directories_cover_their_children() {
        assert!(anchor_matches(&anchor("crates/swiss-memory"), "crates/swiss-memory/src/store.rs"));
        assert!(!anchor_matches(&anchor("crates/swiss-mem"), "crates/swiss-memory/src/store.rs"));
        assert!(anchor_matches(&anchor("src/*.rs"), "src/app.rs"));
    }

    #[test]
    fn path_like_queries_are_recognized() {
        assert!(looks_like_path("store.rs"));
        assert!(looks_like_path("src/app.rs"));
        assert!(!looks_like_path("OrderService.create"));
        assert!(!looks_like_path("v1.2"));
        assert!(!looks_like_path("two words.rs"));
        assert_eq!(normalize(".\\src\\app.rs"), "src/app.rs");
    }
}
