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

//! Lexical search (SPEC §memory.search): a candidate union first, then one
//! documented, deterministic ranking. Tiers: exact id, exact title, anchor (the
//! query names a file), title prefix, symbol (the query names an identifier, a path or
//! a domain the note holds verbatim - [symbols]), then the word tier - FTS5 OR of quoted terms
//! ranked by bm25 (title weighted 3:1) for a query without CJK, or the CJK tier
//! (IDF-weighted bigram coverage, below) for a query with it - and, only when those
//! come up short and the query carries CJK or punctuation, an escaped substring
//! pass. OR, not AND: natural-language queries carry words no note contains. bm25 ranks
//! a note sharing one stray word low but never drops it, so a word hit must also hold
//! [WORD_SHARE_MIN] of the query's weighted words - the noise OR lets in stays out.
//! A note a domain hub links or names ([hubbed]) ranks a little higher in the word band
//! ([W_HUB]): the hub is the area's map, and a note it points at is the area's own word.
//!
//! The CJK tier exists because the FTS5 tokenizer (unicode61) cannot segment CJK: a
//! run of Han characters is one token, so `缓存` never matches `缓存设计`. The query's
//! CJK runs become character bigrams (a lone character is its own unit) and a mixed
//! query's other terms join them; every row the query admits is scored by the
//! IDF-weighted share of those units its text holds. IDF over the scope is what makes
//! `Claude Code 权限` work in a corpus where every note says Claude Code: the common
//! words weigh almost nothing and the note about 权限 wins.

use std::collections::{HashMap, HashSet};

use sqlx::sqlite::SqliteConnection;

use crate::error::MemResult;
use crate::glob;
use crate::model::{Kind, Memory, Status};
use crate::store::{self, Filter};

pub const W_ID: i64 = 1000;
pub const W_TITLE: i64 = 900;
pub const W_ANCHOR: i64 = 850;
pub const W_PREFIX: i64 = 800;
/// The symbol tier: over every word and fused hit whatever their adjustments, so a note
/// holding the identifier a query names is never outranked by one that only shares its
/// words or its meaning; `W_SYMBOL + round(W_SYMBOL_SPAN · held / of)`. The adjustments
/// can lift a symbol hit over an unconfirmed prefix hit: both answer the query exactly.
pub const W_SYMBOL: i64 = 780;
pub const W_SYMBOL_SPAN: i64 = 15;
/// An identifier held by more than a tenth of the scope - and by more than
/// [SYMBOL_DF_FLOOR] notes - says nothing about which note is meant: no symbol hit.
pub const SYMBOL_DF_SHARE: usize = 10;
pub const SYMBOL_DF_FLOOR: usize = 3;
/// Identifiers a query contributes.
pub const IDENTIFIERS_MAX: usize = 8;
pub const W_LEXICAL_MIN: i64 = 500;
pub const W_LEXICAL_SPAN: i64 = 200;
pub const W_ALL_TERMS: i64 = 50;
pub const W_SUBSTRING: i64 = 10;
/// The CJK tier's band: the lexical band, because it is the same evidence (the
/// query's words are in the note) seen through a tokenizer that cannot segment CJK.
/// A query takes one word tier or the other, so the two never compete; sharing the
/// band keeps a CJK hit above the substring pass and below every exact tier, exactly
/// where an English word hit sits.
pub const W_CJK_MIN: i64 = 500;
pub const W_CJK_SPAN: i64 = 200;
/// The title's share of a CJK score (bm25 weights the title 3:1; this mirrors it).
pub const CJK_TITLE_SHARE: f64 = 0.25;
/// Units a query contributes to the CJK tier.
pub const UNITS_MAX: usize = 64;
/// The least share of a query's word weight a word-tier hit holds - the CJK tier's
/// [Covered::coverage], or [word_coverage] over [word_terms] for a query without CJK: a note
/// sharing one stray common word of a sentence is no hit. bm25 ranks such a note low but
/// never drops it, and a context of five took it whenever fewer notes matched well - four
/// of a natural question's five hits, filed in other domains, were one shared word each.
/// A word no row holds leaves the share's denominator, so a query of one word admits every
/// note holding it.
pub const WORD_SHARE_MIN: f64 = 0.2;
/// Fusion with the semantic tier (SPEC §memory.embedding): reciprocal-rank fusion
/// with the customary k, mapped onto the word band so a hit first in both lists
/// scores what a perfect lexical hit does (W_LEXICAL_MIN + W_LEXICAL_SPAN + W_ALL_TERMS).
pub const RRF_K: f64 = 60.0;
pub const W_FUSED_MIN: i64 = 500;
pub const W_FUSED_SPAN: i64 = 250;
/// The tiers that answer the query exactly; fusion never reorders them.
pub const EXACT_TIERS: [&str; 5] = ["id", "title", "anchor", "prefix", "symbol"];
/// The hub mention ([hubbed]): a word, CJK, substring, fused or semantic hit a hub of the
/// project points at scores this much more, never past [W_WORDS_TOP], and wins a tie the cap
/// leaves. A rank, not a word: the word share and its floor never see it, and the exact tiers
/// stay as they are.
pub const W_HUB: i64 = 30;
/// The word band's top: a perfect lexical hit, a hit first in both fused lists.
pub const W_WORDS_TOP: i64 = W_LEXICAL_MIN + W_LEXICAL_SPAN + W_ALL_TERMS;
/// The least title a hub names: three words, a CJK character counting half a word. A
/// shorter one is a term the hub uses, not the note it means.
pub const HUB_TITLE_WORDS: usize = 3;
pub const BONUS_CONFIRMED: i64 = 20;
pub const PENALTY_NEEDS_REVIEW: i64 = -25;
pub const RECENCY_MAX: i64 = 5;
pub const RECENCY_WINDOW_DAYS: i64 = 90;
pub const SUBSTRING_CAP: u64 = 400;
pub const LEXICAL_CAP: u64 = 200;
pub const DEFAULT_LIMIT: usize = 10;
pub const MAX_LIMIT: usize = 50;
pub const QUERY_MAX: usize = 512;

/// One search request.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    pub kinds: Vec<Kind>,
    pub include_history: bool,
    pub limit: usize,
    /// A domain (id or path) narrowing the search to its subtree, or `none` for the
    /// unfiled notes (SPEC §memory.domains).
    pub domain: Option<String>,
    /// The word tiers alone: the semantic tier stays out even with an embedder set (the
    /// evaluation's semantic OFF, SPEC §memory.evaluation).
    pub words_only: bool,
}

/// One ranked result. `similarity` is the cosine to the query when the semantic
/// tier ran (SPEC §memory.embedding); `share` the share of the query's word weight the
/// note holds when a word tier measured it (the word floor's measure, read by the
/// retrieval readout, crate::readout).
#[derive(Debug, Clone)]
pub struct Hit {
    pub memory: Memory,
    pub score: i64,
    pub tier: &'static str,
    pub similarity: Option<f32>,
    pub share: Option<f64>,
}

/// The statuses a read admits.
pub fn statuses(include_history: bool) -> Vec<Status> {
    if include_history {
        Status::ALL.to_vec()
    } else {
        vec![Status::Active, Status::NeedsReview]
    }
}

/// Lower-cased terms for FTS: runs of letters, digits and `_`, deduplicated, in order.
pub fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.is_empty() {
            continue;
        }
        let term = raw.to_lowercase();
        if !out.contains(&term) {
            out.push(term);
        }
    }
    out
}

/// `"a" OR "b"`: every term quoted, so FTS5 operators in the input stay literal.
pub fn or_expr(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
}

/// The CJK units of `text`: the character bigrams of every CJK run, a lone CJK
/// character as its own unit. Deduplicated, in order, at most `max`.
pub fn cjk_units(text: &str, max: usize) -> Vec<String> {
    fn flush(run: &mut Vec<char>, out: &mut Vec<String>, max: usize) {
        let grams: Vec<String> = match run.len() {
            0 => Vec::new(),
            1 => vec![run[0].to_string()],
            _ => run.windows(2).map(|p| p.iter().collect()).collect(),
        };
        for gram in grams {
            if out.len() < max && !out.contains(&gram) {
                out.push(gram);
            }
        }
        run.clear();
    }
    let mut out: Vec<String> = Vec::new();
    let mut run: Vec<char> = Vec::new();
    for c in text.chars() {
        if is_cjk(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out, max);
        }
    }
    flush(&mut run, &mut out, max);
    out
}

/// The CJK tier's units for a query: its CJK units, then its other terms (the Latin
/// words of a mixed query, lower-cased). At most [UNITS_MAX].
pub fn units(text: &str) -> Vec<String> {
    let mut out = cjk_units(text, UNITS_MAX);
    for term in terms(text) {
        if out.len() >= UNITS_MAX {
            break;
        }
        if !has_cjk(&term) && !out.contains(&term) {
            out.push(term);
        }
    }
    out
}

/// One row's IDF-weighted coverage of a query's units.
#[derive(Debug, Clone)]
pub struct Covered {
    pub id: String,
    /// Σ idf of the units the title or content holds / Σ idf of every unit some row
    /// holds, 0..1.
    pub coverage: f64,
    /// The same share counted over the title alone.
    pub title: f64,
    /// Every unit of the query (including those no row holds) is present.
    pub all: bool,
    /// The matched units, in query order.
    pub matched: Vec<String>,
}

impl Covered {
    /// The title-weighted blend, 0..1.
    pub fn blend(&self) -> f64 {
        (1.0 - CJK_TITLE_SHARE) * self.coverage + CJK_TITLE_SHARE * self.title
    }

    /// The tier score: `W_CJK_MIN + round(W_CJK_SPAN · blend)`, + W_ALL_TERMS when a
    /// query of several units is wholly present.
    pub fn score(&self, units: usize) -> i64 {
        let mut score = W_CJK_MIN + (W_CJK_SPAN as f64 * self.blend()).round() as i64;
        if units > 1 && self.all {
            score += W_ALL_TERMS;
        }
        score
    }
}

/// Score `rows` by IDF-weighted unit coverage. IDF is computed over `rows` (the
/// scope the caller admits): `idf(u) = ln(1 + (N − df + 0.5) / (df + 0.5))`. A unit
/// no row holds (a bigram straddling two words, a term the scope lacks) leaves the
/// denominator: it cannot tell rows apart and would only shrink every score alike.
/// Rows holding no unit are left out; the output follows the input order.
/// A note's keywords count as its title: they are its names, given on purpose.
pub fn coverage<'a>(rows: impl IntoIterator<Item = &'a Memory>, units: &[String]) -> Vec<Covered> {
    let named: Vec<(&str, String, &str)> =
        rows.into_iter().map(|m| (m.id.as_str(), titled(&m.title, &m.keywords), m.content.as_str())).collect();
    coverage_of(named.iter().map(|(id, title, content)| (*id, title.as_str(), *content)), units)
}

/// A title with the keywords under it, as the CJK tier and the brief match them.
pub fn titled(title: &str, keywords: &[String]) -> String {
    if keywords.is_empty() {
        title.to_string()
    } else {
        format!("{title}\n{}", keywords.join("\n"))
    }
}

/// [coverage] over any `(id, title, text)` rows: a domain's title and summary.
pub fn coverage_of<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>, units: &[String]) -> Vec<Covered> {
    weigh(rows, units, |hay, unit| hay.contains(unit))
}

/// Each row's IDF-weighted share of a query's words ([word_terms]): [coverage] with whole
/// words - a term counts where it stands bounded in the title, the keywords or the content,
/// as the FTS5 tokenizer sees it, never inside a longer word (`per` is not in `performance`).
pub fn word_coverage<'a>(rows: impl IntoIterator<Item = &'a Memory>, terms: &[String]) -> Vec<Covered> {
    let named: Vec<(&str, String, &str)> =
        rows.into_iter().map(|m| (m.id.as_str(), titled(&m.title, &m.keywords), m.content.as_str())).collect();
    weigh(named.iter().map(|(id, title, content)| (*id, title.as_str(), *content)), terms, bounded)
}

/// Directory names that lay out a tree rather than name a part of it: a path's words skip
/// them ([path_words]).
pub const PATH_SKELETON: [&str; 32] = [
    "src", "lib", "libs", "crates", "packages", "apps", "app", "pkg", "internal", "cmd", "main", "java", "kotlin",
    "scala", "resources", "source", "sources", "include", "modules", "dist", "build", "out", "target", "bin", "test",
    "tests", "spec", "scripts", "docs", "assets", "public", "static",
];

/// File stems that name no part of a tree, only a role in it: the directory above names the
/// part ([path_words]).
pub const GENERIC_STEMS: [&str; 10] = ["mod", "index", "lib", "main", "init", "__init__", "__main__", "readme", "package", "cargo"];

/// The words a path names: its file's stem - `crates/swiss-memory/src/store.rs` names `store`.
/// The extension and the tree's layout name nothing: `crates`, `src` and `rs` gave every note
/// citing another file of the workspace a fifth of a path query's weight, and the crate's
/// `memory` let in every note mentioning the memory crate, filed anywhere. A stem that is
/// only a role ([GENERIC_STEMS]: `mod.rs`, `index.ts`) gives way to the nearest directory
/// above it that is not [PATH_SKELETON], less the words of a naming convention - those
/// `siblings` (the other directories beside it) also carry: `crates/swiss-memory/src/lib.rs`
/// names `memory`, `swiss` being every crate's.
pub fn path_words(path: &str, siblings: impl Fn(&str, &str) -> Vec<String>) -> Vec<String> {
    let path = path.replace('\\', "/");
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..").collect();
    let Some(file) = segments.pop() else { return Vec::new() };
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => stem,
        _ => file,
    };
    if !GENERIC_STEMS.contains(&stem.to_lowercase().as_str()) {
        return terms(stem);
    }
    let Some(at) = segments.iter().rposition(|s| !PATH_SKELETON.contains(&s.to_lowercase().as_str())) else {
        return terms(stem);
    };
    let convention: Vec<String> = siblings(&segments[..at].join("/"), segments[at]).iter().flat_map(|s| terms(s)).collect();
    let words: Vec<String> = terms(segments[at]).into_iter().filter(|w| !convention.contains(w)).collect();
    if words.is_empty() {
        terms(segments[at])
    } else {
        words
    }
}

/// The directories beside `name` under `parent` that `rows` anchor files in.
fn anchored_siblings(rows: &[&Memory], parent: &str, name: &str) -> Vec<String> {
    let prefix = if parent.is_empty() { String::new() } else { format!("{parent}/") };
    let mut out: Vec<String> = Vec::new();
    for anchor in rows.iter().flat_map(|m| &m.anchors) {
        let Some(rest) = anchor.path.strip_prefix(&prefix) else { continue };
        let Some((dir, _)) = rest.split_once('/') else { continue };
        if dir != name && !out.iter().any(|d| d == dir) {
            out.push(dir.to_string());
        }
    }
    out
}

/// The words a query's share is counted over: its salient terms (stop-words and one-letter
/// Latin tokens dropped, [crate::related::salient]) - "the" and "how" say nothing of which
/// note is meant - with each path it names counted by [path_words], the siblings those the
/// rows' anchors show.
pub fn word_terms(text: &str, rows: &[&Memory]) -> Vec<String> {
    let mut spoken = String::with_capacity(text.len());
    for raw in text.split_whitespace() {
        let token = raw
            .trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\')))
            .trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        let slashed = token.contains(['/', '\\']) && is_code(&token.replace('\\', "/"));
        let words = if slashed { path_words(token, |parent, name| anchored_siblings(rows, parent, name)).join(" ") } else { raw.to_string() };
        spoken.push_str(&words);
        spoken.push(' ');
    }
    crate::related::salient(&spoken)
}

/// Each row's word share of a query without CJK ([word_coverage] over [word_terms]), rows
/// holding none left out; None when the query has no salient word to count, and nothing is
/// floored.
pub fn word_shares<'a>(rows: impl IntoIterator<Item = &'a Memory>, text: &str) -> Option<HashMap<String, f64>> {
    let rows: Vec<&Memory> = rows.into_iter().collect();
    let terms = word_terms(text, &rows);
    if terms.is_empty() {
        return None;
    }
    Some(word_coverage(rows, &terms).into_iter().map(|c| (c.id, c.coverage)).collect())
}

/// The IDF-weighted share of `units` each row holds, a unit held where `holds(text, unit)`
/// says so over the lower-cased title and content (see [coverage]).
fn weigh<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    units: &[String],
    holds: impl Fn(&str, &str) -> bool,
) -> Vec<Covered> {
    if units.is_empty() {
        return Vec::new();
    }
    let rows: Vec<(&str, &str, &str)> = rows.into_iter().collect();
    let held: Vec<Vec<(bool, bool)>> = rows
        .iter()
        .map(|(_, title, content)| {
            let title = title.to_lowercase();
            let content = content.to_lowercase();
            units
                .iter()
                .map(|u| {
                    let in_title = holds(&title, u);
                    (in_title, in_title || holds(&content, u))
                })
                .collect()
        })
        .collect();
    let n = rows.len() as f64;
    let idf: Vec<f64> = (0..units.len())
        .map(|i| {
            let df = held.iter().filter(|row| row[i].1).count() as f64;
            if df == 0.0 {
                0.0
            } else {
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
            }
        })
        .collect();
    let total: f64 = idf.iter().sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for ((id, _, _), row) in rows.iter().zip(&held) {
        if !row.iter().any(|(_, any)| *any) {
            continue;
        }
        let mut got = 0.0;
        let mut in_title = 0.0;
        let mut matched = Vec::new();
        for (i, (title, any)) in row.iter().enumerate() {
            if *any {
                got += idf[i];
                matched.push(units[i].clone());
            }
            if *title {
                in_title += idf[i];
            }
        }
        out.push(Covered {
            id: id.to_string(),
            coverage: (got / total).clamp(0.0, 1.0),
            title: (in_title / total).clamp(0.0, 1.0),
            all: row.iter().all(|(_, any)| *any),
            matched,
        });
    }
    out
}

fn has_punctuation(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_punctuation())
}

/// The identifiers a query names (the symbol tier): whitespace-separated tokens, stripped
/// of the quotes, brackets and sentence punctuation around them, that read as code - a
/// `snake_case` or `SCREAMING_CASE` name, a `camelCase` or `PascalCase` one with an inner
/// capital, a `::` path, a dotted name of two words or more (`OrderService.create`,
/// `store.rs`), a slashed one (`src/store.rs`, `panel/i18n`), a `--flag`. Prose is not:
/// plain words, abbreviations (`e.g.`), numbers, hyphenated English. Case kept,
/// deduplicated, in order, at most [IDENTIFIERS_MAX].
pub fn identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let token = raw
            .trim_start_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')))
            .trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if out.len() < IDENTIFIERS_MAX && is_code(token) && !out.iter().any(|t| t == token) {
            out.push(token.to_string());
        }
    }
    out
}

pub(crate) fn is_code(token: &str) -> bool {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 3 || !chars.iter().any(|c| c.is_alphabetic()) || chars.iter().any(|c| is_cjk(*c)) {
        return false;
    }
    if let Some(rest) = token.strip_prefix("--") {
        return rest.starts_with(char::is_alphabetic);
    }
    let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric());
    let between = |i: usize| i > 0 && word(i - 1) && word(i + 1);
    let dotted = token.split('.').filter(|part| part.chars().count() >= 2).count() >= 2;
    for (i, c) in chars.iter().enumerate() {
        let code = match c {
            '_' => (i > 0 && word(i - 1)) || word(i + 1),
            ':' => chars.get(i + 1) == Some(&':') && i > 0 && word(i + 2),
            '/' => between(i),
            '.' => between(i) && dotted,
            _ => i > 0 && chars[i - 1].is_lowercase() && c.is_uppercase(),
        };
        if code {
            return true;
        }
    }
    false
}

/// `needle` in `hay` with no letter, digit or `_` against either end.
fn bounded(hay: &str, needle: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(at) = hay[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        if !hay[..start].chars().next_back().is_some_and(word) && !hay[end..].chars().next().is_some_and(word) {
            return true;
        }
        from = start + needle.chars().next().map(char::len_utf8).unwrap_or(1);
    }
    false
}

/// Whether a note holds an identifier: verbatim and bounded in its title or content
/// (case kept), as an anchor (the path, a path ending in it, a file under an anchored
/// directory or matching an anchored glob), or as its domain or one above it.
pub fn holds(memory: &Memory, ident: &str) -> bool {
    if bounded(&memory.title, ident) || bounded(&memory.content, ident) || memory.keywords.iter().any(|k| bounded(k, ident)) {
        return true;
    }
    if ident.contains(['/', '.']) {
        let wanted = glob::normalize(ident);
        let suffix = format!("/{wanted}");
        if memory.anchors.iter().any(|a| a.path == wanted || a.path.ends_with(&suffix) || glob::anchor_matches(a, &wanted)) {
            return true;
        }
    }
    let under = format!("{ident}/");
    ident.contains('/') && memory.domain_path.as_deref().is_some_and(|d| d == ident || d.starts_with(&under))
}

/// One note the symbol tier admits: it holds `held` of the query's `of` selective
/// identifiers.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub id: String,
    pub held: usize,
    pub of: usize,
}

impl Symbol {
    pub fn score(&self) -> i64 {
        W_SYMBOL + (W_SYMBOL_SPAN as f64 * self.held as f64 / self.of.max(1) as f64).round() as i64
    }
}

/// The symbol tier over `rows`: every row holding one of the query's [identifiers] that is
/// selective - held by some row, and by no more than a tenth of them or [SYMBOL_DF_FLOOR]
/// (a name every note says picks none). An identifier no row holds leaves the
/// denominator, as a CJK unit does. In row order.
pub fn symbols<'a>(rows: impl IntoIterator<Item = &'a Memory>, text: &str) -> Vec<Symbol> {
    let idents = identifiers(text);
    if idents.is_empty() {
        return Vec::new();
    }
    let rows: Vec<&Memory> = rows.into_iter().collect();
    let held: Vec<Vec<bool>> = rows.iter().map(|m| idents.iter().map(|i| holds(m, i)).collect()).collect();
    let cap = SYMBOL_DF_FLOOR.max(rows.len() / SYMBOL_DF_SHARE);
    let selective: Vec<usize> = (0..idents.len())
        .filter(|&i| {
            let df = held.iter().filter(|row| row[i]).count();
            df > 0 && df <= cap
        })
        .collect();
    rows.iter()
        .zip(&held)
        .filter_map(|(memory, row)| {
            let n = selective.iter().filter(|&&i| row[i]).count();
            (n > 0).then(|| Symbol { id: memory.id.clone(), held: n, of: selective.len() })
        })
        .collect()
}

/// The recency bonus: up to +5, linear over 90 days, zero afterwards.
pub fn recency_bonus(updated_ms: i64, now_ms: i64) -> i64 {
    let age_days = (now_ms - updated_ms).max(0) / 86_400_000;
    if age_days >= RECENCY_WINDOW_DAYS {
        return 0;
    }
    (RECENCY_MAX * (RECENCY_WINDOW_DAYS - age_days) / RECENCY_WINDOW_DAYS).max(0)
}

/// Whether every term occurs in the record's text (the all-terms boost).
fn has_all(memory: &Memory, terms: &[String]) -> bool {
    let hay = format!("{}\n{}", titled(&memory.title, &memory.keywords), memory.content).to_lowercase();
    terms.iter().all(|t| hay.contains(t.as_str()))
}

struct Cand {
    best: i64,
    tier: &'static str,
}

fn offer(by_id: &mut HashMap<String, Cand>, id: &str, score: i64, tier: &'static str) {
    match by_id.get_mut(id) {
        Some(cand) if cand.best >= score => {}
        Some(cand) => {
            cand.best = score;
            cand.tier = tier;
        }
        None => {
            by_id.insert(id.to_string(), Cand { best: score, tier });
        }
    }
}

/// The notes of `rows` a hub of the project's map points at (SPEC §memory.search): its body
/// links one - holds its id - or names one - holds its whole title, [HUB_TITLE_WORDS] words or
/// more. The hub is the area's map, written to send a reader where the area is explained;
/// a note it points at is the area's own word on the subject.
pub async fn hubbed(conn: &mut SqliteConnection, project_id: &str, rows: &[Memory]) -> MemResult<HashSet<String>> {
    let hubs = store::hub_bodies(conn, project_id).await?;
    Ok(hub_named(rows, &hubs))
}

/// [hubbed] over hub texts already read. Both sides read as [hub_tokens], so a Latin title
/// matches whole words and a CJK one its characters in a row; the hubs are indexed once and
/// a note costs the places of its rarest token, not a scan of every hub.
pub fn hub_named<'a>(rows: impl IntoIterator<Item = &'a Memory>, hubs: &[String]) -> HashSet<String> {
    if hubs.is_empty() {
        return HashSet::new();
    }
    let mut known: HashMap<String, usize> = HashMap::new();
    let mut text: Vec<usize> = Vec::new();
    let mut places: Vec<Vec<usize>> = Vec::new();
    for hub in hubs {
        for token in hub_tokens(hub) {
            let next = known.len();
            let id = *known.entry(token).or_insert(next);
            if id == places.len() {
                places.push(Vec::new());
            }
            places[id].push(text.len());
            text.push(id);
        }
        text.push(usize::MAX); // no phrase spans two hubs
    }
    let names = |tokens: &[String]| {
        let Some(want) = tokens.iter().map(|t| known.get(t).copied()).collect::<Option<Vec<usize>>>() else {
            return false;
        };
        let Some((k, rarest)) = want.iter().enumerate().min_by_key(|(_, id)| places[**id].len()) else {
            return false;
        };
        places[*rarest].iter().any(|&p| p >= k && text.get(p - k..p - k + want.len()) == Some(&want[..]))
    };
    rows.into_iter()
        .filter(|m| {
            let title = hub_tokens(&m.title);
            let words: usize = title.iter().map(|t| if t.chars().next().is_some_and(is_cjk) { 1 } else { 2 }).sum();
            (crate::model::is_record_id(&m.id) && names(&hub_tokens(&m.id))) || (words >= 2 * HUB_TITLE_WORDS && names(&title))
        })
        .map(|m| m.id.clone())
        .collect()
}

/// The hub mention's tokens: lower-cased runs of letters, digits and `_`, each CJK character
/// a token of its own.
fn hub_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        let cjk = is_cjk(c);
        let letter = !cjk && (c.is_alphanumeric() || c == '_');
        if !letter && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if cjk {
            out.push(c.to_string());
        } else if letter {
            word.extend(c.to_lowercase());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// A score with the hub mention: [W_HUB] more for a note a hub points at, never past `top`
/// (the band's); an exact tier's score as it is.
fn with_hub(score: i64, tier: &str, hub: bool, top: i64) -> i64 {
    if hub && !EXACT_TIERS.contains(&tier) {
        (score + W_HUB).min(top).max(score)
    } else {
        score
    }
}

/// Run one query against one scope; every tier's predicate carries the scope.
pub async fn run(conn: &mut SqliteConnection, project_id: &str, query: &SearchQuery) -> MemResult<Vec<Hit>> {
    let rows = rows(conn, project_id, query).await?;
    let hubbed = hubbed(conn, project_id, &rows).await?;
    let mut hits = rank(conn, project_id, query, &rows, &hubbed).await?;
    hits.truncate(query.limit.clamp(1, MAX_LIMIT));
    Ok(hits)
}

/// The rows a query admits: its statuses and kinds over one scope.
pub async fn rows(conn: &mut SqliteConnection, project_id: &str, query: &SearchQuery) -> MemResult<Vec<Memory>> {
    store::memories(
        conn,
        project_id,
        &Filter { statuses: statuses(query.include_history), kinds: query.kinds.clone(), ..Filter::default() },
    )
    .await
}

/// Every lexical hit over `rows` (from [rows]), best first and untruncated: the
/// semantic fusion ranks the whole lexical order. `hubbed` (from [hubbed]) takes the hub
/// mention.
pub async fn rank(
    conn: &mut SqliteConnection,
    project_id: &str,
    query: &SearchQuery,
    rows: &[Memory],
    hubbed: &HashSet<String>,
) -> MemResult<Vec<Hit>> {
    let text = query.text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let statuses = statuses(query.include_history);
    let by_record: HashMap<&str, &Memory> = rows.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut by_id: HashMap<String, Cand> = HashMap::new();
    let folded = store::title_norm(text);

    for memory in rows {
        if memory.id == text {
            offer(&mut by_id, &memory.id, W_ID, "id");
        }
        let norm = store::title_norm(&memory.title);
        if norm == folded {
            offer(&mut by_id, &memory.id, W_TITLE, "title");
        } else if norm.starts_with(&folded) {
            offer(&mut by_id, &memory.id, W_PREFIX, "prefix");
        }
    }
    if glob::looks_like_path(text) {
        let wanted = glob::normalize(text);
        for memory in rows {
            let hit = memory.anchors.iter().any(|a| {
                glob::anchor_matches(a, &wanted)
                    || a.path.ends_with(&format!("/{wanted}"))
                    || a.path == wanted
            });
            if hit {
                offer(&mut by_id, &memory.id, W_ANCHOR, "anchor");
            }
        }
    }
    let terms = terms(text);
    let mut shares: HashMap<String, f64> = HashMap::new();
    if has_cjk(text) {
        let units = units(text);
        for covered in coverage(rows, &units) {
            shares.insert(covered.id.clone(), covered.coverage);
            if covered.coverage >= WORD_SHARE_MIN {
                offer(&mut by_id, &covered.id, covered.score(units.len()), "cjk");
            }
        }
    } else if !terms.is_empty() {
        let mut ranked = store::fts(conn, project_id, &or_expr(&terms), &statuses, LEXICAL_CAP).await?;
        if let Some(measured) = word_shares(rows, text) {
            ranked.retain(|(id, _)| measured.get(id).is_some_and(|s| *s >= WORD_SHARE_MIN));
            shares = measured;
        }
        let best = ranked.first().map(|(_, rank)| *rank).unwrap_or(0.0);
        for (id, rank) in &ranked {
            let Some(memory) = by_record.get(id.as_str()) else {
                continue; // filtered out by kind
            };
            let ratio = if best < 0.0 { (rank / best).clamp(0.0, 1.0) } else { 1.0 };
            let mut score = W_LEXICAL_MIN + (W_LEXICAL_SPAN as f64 * ratio).round() as i64;
            if terms.len() > 1 && has_all(memory, &terms) {
                score += W_ALL_TERMS;
            }
            offer(&mut by_id, id, score, "lexical");
        }
    }
    if by_id.len() < limit && (has_cjk(text) || has_punctuation(text)) {
        for id in store::substring(conn, project_id, text, &statuses, SUBSTRING_CAP).await? {
            if by_record.contains_key(id.as_str()) {
                offer(&mut by_id, &id, W_SUBSTRING, "substring");
            }
        }
    }
    // Within a score, the better word evidence first (the symbol tier's ties).
    let words: HashMap<String, i64> = by_id.iter().map(|(id, cand)| (id.clone(), cand.best)).collect();
    for symbol in symbols(rows, text) {
        offer(&mut by_id, &symbol.id, symbol.score(), "symbol");
    }

    let now = store::now();
    let mut hits: Vec<Hit> = by_id
        .into_iter()
        .filter_map(|(id, cand)| {
            let memory = by_record.get(id.as_str())?;
            let score = with_hub(cand.best, cand.tier, hubbed.contains(&id), W_WORDS_TOP) + adjust(memory, now);
            Some(Hit { memory: (*memory).clone(), score, tier: cand.tier, similarity: None, share: shares.get(&id).copied() })
        })
        .collect();
    let word = |h: &Hit| words.get(&h.memory.id).copied().unwrap_or(0);
    let hub = |h: &Hit| !EXACT_TIERS.contains(&h.tier) && hubbed.contains(&h.memory.id);
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| hub(b).cmp(&hub(a)))
            .then_with(|| word(b).cmp(&word(a)))
            .then_with(|| a.memory.id.cmp(&b.memory.id))
    });
    Ok(hits)
}

/// One list's reciprocal-rank contribution at a 0-based rank.
pub fn rrf(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

/// Reciprocal-rank sums of a word ranking and the cosine ranking: an id scores the
/// sum of [rrf] over the lists it is in. The cosine ranking holds every id in `sims`
/// but `skip`, best first (ties by id), so a word hit is credited for its meaning
/// wherever its cosine falls: a floor there dropped a strong word hit to half the
/// score of a weaker one for a cosine a hair under it (found live on 19998). The
/// floor and SEMANTIC_CAP gate only the ids the words missed. `true` marks an id
/// only the cosine ranking holds.
pub fn rrf_sums(words: &[String], sims: &HashMap<String, f32>, skip: &[String], floor: f32) -> HashMap<String, (f64, bool)> {
    let mut sums: HashMap<String, (f64, bool)> = HashMap::new();
    for (rank, id) in words.iter().enumerate() {
        sums.entry(id.clone()).or_insert((rrf(rank), false));
    }
    let mut order: Vec<(&String, f32)> = sims.iter().filter(|(id, _)| !skip.contains(id)).map(|(id, s)| (id, *s)).collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut admitted = 0;
    for (rank, (id, sim)) in order.into_iter().enumerate() {
        if let Some(entry) = sums.get_mut(id) {
            entry.0 += rrf(rank);
        } else if sim >= floor && admitted < crate::embed::SEMANTIC_CAP {
            sums.insert(id.clone(), (rrf(rank), true));
            admitted += 1;
        }
    }
    sums
}

/// Fuse the lexical order (from [rank]) with the semantic one. The exact tiers keep
/// their scores and stay on top, in their order. Every other candidate - a word,
/// CJK or substring hit, or a row the words missed whose cosine is at or above
/// `floor` ([rrf_sums]) - scores
/// `W_FUSED_MIN + round(W_FUSED_SPAN · rrf · (RRF_K + 1) / 2)` plus the record
/// adjustments, where `rrf` sums `1 / (RRF_K + rank)` over the lists it is in - with the hub
/// mention for the ids in `hubbed`. A hit only the semantic list holds is tier `semantic`.
/// Every hit with a current vector carries its cosine. Untruncated, best first.
pub fn fuse(
    lexical: Vec<Hit>,
    rows: &[Memory],
    sims: &HashMap<String, f32>,
    floor: f32,
    now: i64,
    hubbed: &HashSet<String>,
) -> Vec<Hit> {
    let (mut exact, words): (Vec<Hit>, Vec<Hit>) = lexical.into_iter().partition(|h| EXACT_TIERS.contains(&h.tier));
    let placed: Vec<String> = exact.iter().map(|h| h.memory.id.clone()).collect();
    let word_ids: Vec<String> = words.iter().map(|h| h.memory.id.clone()).collect();
    let tiers: HashMap<&str, (&'static str, Option<f64>)> = words.iter().map(|h| (h.memory.id.as_str(), (h.tier, h.share))).collect();
    let by_record: HashMap<&str, &Memory> = rows.iter().map(|m| (m.id.as_str(), m)).collect();
    let top = 2.0 * rrf(0);
    let mut rest: Vec<Hit> = rrf_sums(&word_ids, sims, &placed, floor)
        .into_iter()
        .filter_map(|(id, (sum, meaning_only))| {
            let memory = by_record.get(id.as_str())?;
            let (tier, share) = if meaning_only { ("semantic", None) } else { tiers.get(id.as_str()).copied()? };
            let band = W_FUSED_MIN + (W_FUSED_SPAN as f64 * (sum / top).clamp(0.0, 1.0)).round() as i64;
            let band = with_hub(band, tier, hubbed.contains(&id), W_FUSED_MIN + W_FUSED_SPAN);
            Some(Hit {
                memory: (*memory).clone(),
                score: band + adjust(memory, now),
                tier,
                similarity: sims.get(&id).copied(),
                share,
            })
        })
        .collect();
    // A tie goes to the hub's note (one the band's top capped), then to the better word
    // rank: the words are the exact evidence, meaning the soft one (rrf is symmetric -
    // first by words and second by meaning ties the converse).
    let word_rank: HashMap<&str, usize> = word_ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
    let rank_of = |h: &Hit| word_rank.get(h.memory.id.as_str()).copied().unwrap_or(usize::MAX);
    let hub = |h: &Hit| hubbed.contains(&h.memory.id);
    rest.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| hub(b).cmp(&hub(a)))
            .then_with(|| rank_of(a).cmp(&rank_of(b)))
            .then_with(|| a.memory.id.cmp(&b.memory.id))
    });
    for hit in &mut exact {
        hit.similarity = sims.get(&hit.memory.id).copied();
    }
    exact.extend(rest);
    exact
}

/// The record-level adjustments every ranked hit carries: recency, confirmation and
/// the review penalty.
pub fn adjust(memory: &Memory, now: i64) -> i64 {
    let mut score = recency_bonus(memory.updated_ms, now);
    if memory.confirmed {
        score += BONUS_CONFIRMED;
    }
    if memory.status == Status::NeedsReview {
        score += PENALTY_NEEDS_REVIEW;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_split_identifiers_and_quote_operators() {
        assert_eq!(terms("OrderService.create"), vec!["orderservice", "create"]);
        assert_eq!(terms("NOT a OR b"), vec!["not", "a", "or", "b"]);
        assert_eq!(or_expr(&terms("deploy \"pitfall\"")), "\"deploy\" OR \"pitfall\"");
        assert_eq!(terms("认证 流程"), vec!["认证", "流程"]);
    }

    #[test]
    fn recency_is_bounded_and_monotonic() {
        let now = 1_000_000_000_000;
        assert_eq!(recency_bonus(now, now), RECENCY_MAX);
        assert!(recency_bonus(now - 45 * 86_400_000, now) < RECENCY_MAX);
        assert_eq!(recency_bonus(now - 400 * 86_400_000, now), 0);
    }

    #[test]
    fn cjk_is_detected() {
        assert!(has_cjk("缓存失效"));
        assert!(!has_cjk("cache"));
    }

    fn row(id: &str, title: &str, content: &str) -> Memory {
        Memory {
            id: id.into(),
            scope: "p-test".into(),
            kind: Kind::Knowledge,
            title: title.into(),
            content: content.into(),
            status: Status::Active,
            origin: crate::model::Origin::User,
            actor: "cli".into(),
            producer: crate::model::Producer::Unknown,
            transport: crate::model::Transport::Cli,
            confirmed: false,
            source_verified: false,
            anchors: Vec::new(),
            refs: Vec::new(),
            keywords: Vec::new(),
            domain: None,
            domain_path: None,
            superseded_by: None,
            revision: 1,
            created_ms: 0,
            updated_ms: 0,
            verified_ms: None,
        }
    }

    /// Ranked (id, score) of the CJK tier alone, best first.
    fn cjk_rank(rows: &[Memory], query: &str) -> Vec<(String, i64)> {
        let units = units(query);
        let mut out: Vec<(String, i64)> =
            coverage(rows, &units).into_iter().map(|c| (c.id.clone(), c.score(units.len()))).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    #[test]
    fn cjk_units_are_bigrams_lone_characters_and_mixed_terms() {
        assert_eq!(cjk_units("缓存失效", UNITS_MAX), vec!["缓存", "存失", "失效"]);
        assert_eq!(cjk_units("的 权限", UNITS_MAX), vec!["的", "权限"]);
        assert_eq!(cjk_units("缓存缓存", UNITS_MAX), vec!["缓存", "存缓"]);
        assert_eq!(units("Claude Code 权限"), vec!["权限", "claude", "code"]);
        assert!(units("plain english").iter().all(|u| u == "plain" || u == "english"));
    }

    #[test]
    fn the_cjk_band_is_the_lexical_band() {
        // A CJK word hit lands where an English one would: under every exact tier,
        // over the substring pass.
        assert_eq!(W_CJK_MIN, W_LEXICAL_MIN);
        assert_eq!(W_CJK_SPAN, W_LEXICAL_SPAN);
        const { assert!(W_CJK_MIN + W_CJK_SPAN + W_ALL_TERMS + BONUS_CONFIRMED + RECENCY_MAX < W_PREFIX) };
        const { assert!(W_CJK_MIN + PENALTY_NEEDS_REVIEW > W_SUBSTRING + BONUS_CONFIRMED + RECENCY_MAX) };
    }

    #[test]
    fn a_chinese_query_ranks_by_weighted_coverage() {
        let rows = vec![
            row("m-a", "权限模式", "用 /permissions 设置权限模式，自动接受编辑或每次询问。"),
            row("m-b", "快捷键", "常用快捷键：Esc 中断，可在设置里改键位。"),
            row("m-e", "主题", "在设置里切换主题。"),
            row("m-c", "配置文件", "权限规则写在 settings.json 的 permissions 字段。"),
            row("m-d", "钩子", "PreToolUse 钩子可以在工具运行前拦截。"),
        ];
        let ranked = cjk_rank(&rows, "怎么设置权限模式");
        let ids: Vec<&str> = ranked.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.first(), Some(&"m-a"), "{ranked:?}");
        assert!(ids.contains(&"m-c"), "{ranked:?}");
        assert!(!ids.contains(&"m-d"), "a note sharing no unit is no hit: {ranked:?}");
        let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap_or(usize::MAX);
        assert!(pos("m-c") < pos("m-b"), "权限 outweighs a stray bigram: {ranked:?}");
        for (_, score) in &ranked {
            assert!((W_CJK_MIN..=W_CJK_MIN + W_CJK_SPAN + W_ALL_TERMS).contains(score), "{ranked:?}");
        }
        // The whole phrase present: the all-units bonus, and the title share on top.
        let exact = cjk_rank(&rows, "权限模式");
        assert_eq!(exact[0], ("m-a".to_string(), W_CJK_MIN + W_CJK_SPAN + W_ALL_TERMS));
    }

    #[test]
    fn a_mixed_query_weighs_its_rare_word() {
        // A Claude Code tutorial: every note says Claude Code, one is about 权限.
        let mut rows: Vec<Memory> = (0..6)
            .map(|i| row(&format!("m-{i}"), &format!("Claude Code 技巧 {i}"), "在 Claude Code 里，Claude Code 会读取项目。"))
            .collect();
        rows.push(row("m-perm", "权限模式", "Claude Code 每次编辑前询问权限，可以用 /permissions 调整。"));
        rows.push(row("m-bare", "权限", "只读模式下不会修改文件。"));
        let ranked = cjk_rank(&rows, "Claude Code 权限");
        assert_eq!(ranked[0].0, "m-perm", "every word present wins: {ranked:?}");
        assert_eq!(ranked[1].0, "m-bare", "权限 alone beats Claude Code alone: {ranked:?}");
        let common = ranked.iter().find(|(id, _)| id == "m-0").map(|(_, s)| *s).unwrap_or(0);
        assert!(common < W_CJK_MIN + W_CJK_SPAN / 3, "the common words weigh little: {ranked:?}");
    }

    #[test]
    fn coverage_ignores_units_no_row_holds() {
        let rows = vec![row("m-a", "缓存设计", "写穿缓存，失效靠版本号。")];
        // 我要/要缓/存的/的设 straddle words: no row holds them, so they do not dilute.
        let covered = coverage(&rows, &units("我要缓存的设计"));
        assert_eq!(covered.len(), 1);
        assert!(covered[0].coverage > 0.99, "{covered:?}");
        assert!(!covered[0].all);
        assert!(coverage(&rows, &units("数据库")).is_empty());
    }

    #[test]
    fn a_word_share_counts_whole_words_by_weight() {
        let rows = vec![
            row("m-a", "Restart the test instance", "Kill it by the PID that owns its port."),
            row("m-b", "Panel dev server", "Vite serves the panel on its own port."),
            row("m-c", "Tunnel forwards", "A tunnel maps a local port to a remote one."),
            row("m-d", "Performance budget", "Keep the bundle small."),
        ];
        let shares = word_shares(&rows, "how do I restart the test instance port").expect("salient words");
        assert!(shares["m-a"] > 0.99, "{shares:?}");
        // The common word of four weighs little: no hit under the floor.
        assert!(shares["m-b"] < WORD_SHARE_MIN && shares["m-c"] < WORD_SHARE_MIN, "{shares:?}");
        // Whole words only, as the tokenizer sees them: `per` is not in `performance`.
        assert!(!word_shares(&rows, "per").expect("one word").contains_key("m-d"));
        // A query of one word: every holder holds all of it.
        let port = word_shares(&rows, "port").expect("one word");
        assert_eq!(port.len(), 3);
        assert!(port.values().all(|s| *s > 0.99), "{port:?}");
        // Nothing salient to count: no floor.
        assert!(word_shares(&rows, "how is it").is_none());
    }

    #[test]
    fn a_path_counts_its_stem_and_nearest_named_directory() {
        let alone = |_: &str, _: &str| Vec::new();
        assert_eq!(path_words("crates/swiss-memory/src/store.rs", alone), vec!["store"]);
        assert_eq!(path_words("crates/swiss-tunnels/src/tunnel/ssh.rs", alone), vec!["ssh"]);
        assert_eq!(path_words("scripts/test-instance.ps1", alone), vec!["test", "instance"]);
        assert_eq!(path_words(r"panel\src\locales\zh.ts", alone), vec!["zh"]);
        assert_eq!(path_words("panel/i18n", alone), vec!["i18n"]);
        // A stem that is a role: the directory names the part, less a sibling convention.
        assert_eq!(path_words("crates/swiss-memory/src/lib.rs", alone), vec!["swiss", "memory"]);
        assert_eq!(path_words("src/plugins/mod.rs", alone), vec!["plugins"]);
        assert_eq!(path_words("src/main.rs", alone), vec!["main"]);
        assert_eq!(word_terms("why does crates/swiss-tunnels/src/tunnel/ssh.rs hang?", &[]), vec!["why", "ssh", "hang"]);
        let rows = vec![
            row("m-ssh", "Skip WindowAdjusted while awaiting an SSH reply", "The tunnel's channel request waits."),
            row("m-lib", "Touch crates/swiss-panel/src/lib.rs before a rebuild", "Or the exe keeps the old panel."),
            row("m-it", "Gate 2 runs through crates/swiss-it", "Real databases in containers."),
            anchored(row("m-store", "Store writes run in BEGIN IMMEDIATE", "Deferred begins fail busy."), "crates/swiss-memory/src/store.rs"),
            anchored(row("m-panel", "Panel emit", "Generated, never edited."), "crates/swiss-panel/src/lib.rs"),
        ];
        // The workspace's layout is no evidence: the notes citing other files of it share none
        // of the path's words.
        let shares = word_shares(&rows, "crates/swiss-tunnels/src/tunnel/ssh.rs").expect("path words");
        assert_eq!(shares.keys().collect::<Vec<_>>(), vec!["m-ssh"], "{shares:?}");
        // `swiss-panel` sits beside `swiss-memory`: `swiss` is the workspace's convention, not
        // the crate's name - the note citing the panel's crate holds nothing of the path.
        let refs: Vec<&Memory> = rows.iter().collect();
        assert_eq!(word_terms("crates/swiss-memory/src/lib.rs", &refs), vec!["memory"]);
        let shares = word_shares(&rows, "crates/swiss-memory/src/store.rs").expect("path words");
        assert_eq!(shares.keys().collect::<Vec<_>>(), vec!["m-store"], "{shares:?}");
    }

    #[test]
    fn identifiers_are_the_code_a_query_names() {
        assert_eq!(identifiers("why is retry_budget exhausted"), vec!["retry_budget"]);
        assert_eq!(identifiers("BEGIN IMMEDIATE SQLITE_BUSY"), vec!["SQLITE_BUSY"]);
        assert_eq!(identifiers("Get-NetTCPConnection OwningProcess"), vec!["Get-NetTCPConnection", "OwningProcess"]);
        assert_eq!(
            identifiers("see `OrderService.create` and swiss_memory::search::rank()"),
            vec!["OrderService.create", "swiss_memory::search::rank"]
        );
        assert_eq!(identifiers("what breaks when src/store.rs changes?"), vec!["src/store.rs"]);
        assert_eq!(identifiers("pass --allow-similar twice, then --allow-similar"), vec!["--allow-similar"]);
        assert_eq!(identifiers("缓存 key_of 失效"), vec!["key_of"]);
        // Prose is not code: plain and capitalised words, abbreviations, numbers,
        // hyphenated English, a lone underscore.
        assert!(identifiers("cargo fmt rustfmt Swiss").is_empty());
        assert!(identifiers("e.g. the cache, i.e. a well-known end-to-end test of 3.5 or 1/2").is_empty());
        assert!(identifiers("__ _ a_ -x").is_empty());
    }

    fn anchored(mut memory: Memory, path: &str) -> Memory {
        memory.anchors = vec![crate::model::Anchor { path: path.into(), glob: false, hash: None }];
        memory
    }

    fn symbol_ids(rows: &[Memory], query: &str) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = symbols(rows, query).into_iter().map(|s| (s.id, s.held)).collect();
        out.sort();
        out
    }

    #[test]
    fn a_symbol_hit_holds_what_the_query_names_verbatim() {
        let mut rows = vec![
            row("m-a", "Request budget", "Each request spends retry_budget before it fails over."),
            anchored(row("m-b", "Retries", "The retry budget is spent per request."), "src/pool/budget.rs"),
            row("m-c", "Budgets", "my_retry_budget_v2 is another knob; RETRY_BUDGET a third."),
            row("m-d", "Plurals", "Every locale needs its plural rules."),
        ];
        rows[3].domain_path = Some("panel/i18n".into());
        // The identifier, bounded and case-sensitive: not the words, not a longer name.
        assert_eq!(symbol_ids(&rows, "why is retry_budget exhausted"), vec![("m-a".to_string(), 1)]);
        // A path the note is anchored at, and the domain a note is filed in.
        assert_eq!(symbol_ids(&rows, "what breaks when src/pool/budget.rs changes"), vec![("m-b".to_string(), 1)]);
        assert_eq!(symbol_ids(&rows, "panel/i18n plural forms"), vec![("m-d".to_string(), 1)]);
        // Every identifier counts: a note holding two of two beats one holding one.
        rows[1].content.push_str(" See retry_budget.");
        let both = symbols(&rows, "retry_budget and src/pool/budget.rs");
        let b = both.iter().find(|s| s.id == "m-b").expect("m-b");
        let a = both.iter().find(|s| s.id == "m-a").expect("m-a");
        assert_eq!((b.held, b.of, a.held, a.of), (2, 2, 1, 2));
        assert!(b.score() > a.score());
        assert!((W_SYMBOL..=W_SYMBOL + W_SYMBOL_SPAN).contains(&a.score()));
        // No identifier, no symbol tier.
        assert!(symbols(&rows, "retry budget exhausted").is_empty());
    }

    #[test]
    fn a_common_identifier_is_no_signal() {
        // Held by more than a tenth of the scope (and more than three notes), an identifier
        // says nothing about which note is meant.
        let rows: Vec<Memory> = (0..12)
            .map(|i| row(&format!("m-{i:02}"), "Isolation", if i < 5 { "Tests set SWISS_HOME first." } else { "Other." }))
            .collect();
        assert!(symbols(&rows, "SWISS_HOME isolation").is_empty());
        assert_eq!(symbols(&rows[..8], "SWISS_HOME isolation").len(), 0, "5 of 8");
        let few: Vec<Memory> = rows.iter().skip(2).cloned().collect();
        assert_eq!(symbols(&few, "SWISS_HOME isolation").len(), 3, "3 of 10: at the floor");
    }

    #[test]
    fn the_symbol_tier_sits_between_the_words_and_the_exact_tiers() {
        // Over every word and fused hit, whatever their adjustments; under the anchor tier.
        const { assert!(W_LEXICAL_MIN + W_LEXICAL_SPAN + W_ALL_TERMS + BONUS_CONFIRMED + RECENCY_MAX < W_SYMBOL) };
        const { assert!(W_FUSED_MIN + W_FUSED_SPAN + BONUS_CONFIRMED + RECENCY_MAX < W_SYMBOL) };
        const { assert!(W_SYMBOL + W_SYMBOL_SPAN + BONUS_CONFIRMED + RECENCY_MAX < W_ANCHOR) };
        assert!(EXACT_TIERS.contains(&"symbol"), "fusion never reorders a symbol hit");
    }

    fn sims(pairs: &[(&str, f32)]) -> HashMap<String, f32> {
        pairs.iter().map(|(id, s)| (id.to_string(), *s)).collect()
    }

    #[test]
    fn a_word_hit_keeps_its_meaning_credit_under_the_floor() {
        // a: first by words, cosine a hair under the floor; b: second by words, above it;
        // c: missed by the words, above the floor; d: missed and below it.
        let words = vec!["a".to_string(), "b".to_string()];
        let cos = sims(&[("a", 0.299), ("b", 0.372), ("c", 0.31), ("d", 0.1)]);
        let sums = rrf_sums(&words, &cos, &[], 0.3);
        // The cosine ranking: b, c, a, d.
        assert_eq!(sums["a"], (rrf(0) + rrf(2), false));
        assert_eq!(sums["b"], (rrf(1) + rrf(0), false));
        assert_eq!(sums["c"], (rrf(1), true));
        assert!(!sums.contains_key("d"), "a note the words missed needs the floor");
        // A skipped id (an exact-tier hit) takes no place in the cosine ranking.
        let skipped = rrf_sums(&words, &cos, &["b".to_string()], 0.3);
        assert_eq!(skipped["c"], (rrf(0), true));
        // A higher floor (a model whose unrelated pairs sit high) keeps c out.
        assert!(!rrf_sums(&words, &cos, &[], 0.35).contains_key("c"));
    }

    #[test]
    fn a_fusion_tie_goes_to_the_better_word_rank() {
        let rows = vec![row("m-z", "Ralph Loop 原理", "Stop hook 返回 block"), row("m-a", "强制规则用 Hook", "Ralph Loop 是这条原理的极致")];
        let hit = |m: &Memory| Hit { memory: m.clone(), score: 0, tier: "cjk", similarity: None, share: None };
        // m-z is first by words and second by meaning; m-a the converse: the sums tie.
        let cos = sims(&[("m-a", 0.372), ("m-z", 0.299)]);
        let fused = fuse(vec![hit(&rows[0]), hit(&rows[1])], &rows, &cos, 0.3, 0, &HashSet::new());
        assert_eq!(fused[0].score, fused[1].score);
        assert_eq!(fused[0].memory.id, "m-z", "the id would have put m-a first: {fused:?}");
        assert_eq!(fused[0].similarity, Some(0.299));
        // A hub naming m-a lifts it over the tie: W_HUB, short of the band's top.
        let hubbed = HashSet::from(["m-a".to_string()]);
        let fused = fuse(vec![hit(&rows[0]), hit(&rows[1])], &rows, &cos, 0.3, 0, &hubbed);
        assert_eq!(fused[0].memory.id, "m-a", "{fused:?}");
        let adjusted = adjust(&rows[0], 0);
        assert_eq!(fused[0].score - adjusted, (fused[1].score - adjusted + W_HUB).min(W_FUSED_MIN + W_FUSED_SPAN), "{fused:?}");
        assert!(fused[0].score > fused[1].score, "{fused:?}");
    }

    #[test]
    fn a_hub_links_a_note_by_id_and_names_it_by_its_whole_title() {
        let rows = vec![
            row("m-00000000a1", "Store writes run in BEGIN IMMEDIATE", "Deferred begins fail busy."),
            row("m-00000000a2", "Store writes", "Too short to be named."),
            row("m-00000000a3", "Panel emit needs a build", "The js is generated."),
            row("m-00000000a4", "写穿缓存按版本号失效", "缓存的失效靠版本号。"),
            row("m-00000000a5", "缓存设计", "四个字，半个词一个。"),
            row("m-00000000a6", "Retry budget per request", "A note no hub points at."),
        ];
        let hubs = vec![
            "## Overview\nThe store. Every write: see **store writes run in begin immediate** and m-00000000a3.".to_string(),
            "## 缓存\n写入时走写穿缓存按版本号失效的路径；缓存设计见代码。".to_string(),
            "## Pool\nRequests spend a retry budget".to_string(),
            "per request is how this other hub opens.".to_string(),
        ];
        let mut named: Vec<String> = hub_named(&rows, &hubs).into_iter().collect();
        named.sort();
        // a1 by its title (case and punctuation aside), a3 by its id, a4 by its CJK title in a
        // run; a2's two words and a5's four characters are terms, a6 spans two hubs.
        assert_eq!(named, vec!["m-00000000a1", "m-00000000a3", "m-00000000a4"]);
        // Whole words: a title inside a longer word is not named.
        let hub = vec!["Restore writes run in BEGIN IMMEDIATELY".to_string()];
        assert!(hub_named(&rows, &hub).is_empty());
        assert!(hub_named(&rows, &[]).is_empty());
    }

    #[test]
    fn the_hub_mention_stays_in_the_word_band() {
        // Never past the band's top, so never over the symbol tier, whatever the adjustments.
        const { assert!(W_WORDS_TOP == W_FUSED_MIN + W_FUSED_SPAN) };
        const { assert!(W_WORDS_TOP + BONUS_CONFIRMED + RECENCY_MAX < W_SYMBOL) };
        // Small: a substring hit a hub names stays under every word hit.
        const { assert!(W_SUBSTRING + W_HUB + BONUS_CONFIRMED + RECENCY_MAX < W_LEXICAL_MIN + PENALTY_NEEDS_REVIEW) };
        assert_eq!(with_hub(600, "lexical", true, W_WORDS_TOP), 600 + W_HUB);
        assert_eq!(with_hub(740, "cjk", true, W_WORDS_TOP), W_WORDS_TOP);
        assert_eq!(with_hub(600, "lexical", false, W_WORDS_TOP), 600);
        for tier in EXACT_TIERS {
            assert_eq!(with_hub(W_TITLE, tier, true, W_WORDS_TOP), W_TITLE, "{tier}");
        }
    }
}
