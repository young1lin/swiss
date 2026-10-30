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

//! What a save checks against the checkout (SPEC §memory.code): the files a note anchors,
//! the paths it names and the code it names.
//!
//! - **missing_anchor**: a file anchor that is not a file under the root (read live, no index
//!   needed), or a glob the indexed files never match.
//! - **missing_file**: a path the note names (`src/x.rs`: a `/` and a code or document
//!   extension) that is neither indexed (as itself or as the tail of one indexed path) nor on
//!   disk.
//! - **unknown_name**: a name the note writes as code - `snake_case`, `lowerCamel`, `$x`, a
//!   name in backticks - that no indexed file defines or mentions, and that begins no family
//!   of names in the anchored files (a key family's stem, [holds_stem]).
//! - **elsewhere** (advisory): a name not in the anchored files but defined in up to
//!   [ELSEWHERE_MAX] other files, which the finding lists.
//! - **missing_evidence**: a piece of code, test or doc evidence citing a path that is not a
//!   file under the root (read live).
//! - **excerpt_mismatch**: an evidence excerpt its file does not hold, whitespace runs taken
//!   as one space, a string's escapes as what they stand for, and an elision (`...`) or a
//!   redaction marker letting parts stand apart in order (read live).
//! - **unscoped_quantifier**: a claim over a set ([formation::set_claims]) that names no set
//!   to re-count, no file, no hedge, and no excerpt stating its number.
//! - **count_mismatch**: a claim over a set that names its set, re-counted in the index, that
//!   comes out otherwise; the finding lists the files.
//!
//! All but elsewhere hold a note from anyone but a person for review: its status is
//! needs_review, its revision says why. A plain word is never unknown (prose is not code),
//! and without an index only anchors, evidence and unscoped claims are judged. An index cut
//! at its listing cap never calls anything absent and re-counts nothing.

use std::path::Path;

use serde::Serialize;

use crate::code::db;
use crate::code::extract::{Lang, NAME_MAX};
use crate::evidence::EvidenceSpec;
use crate::formation::{self, Quantity, SetClaim};
use crate::fresh::{self, Probe, HASH_SIZE_LIMIT};
use crate::glob;
use crate::model::{Anchor, SourceType};
use crate::search::is_code;
use crate::secrets;

/// An elsewhere finding is made for a name at most this many files define.
pub const ELSEWHERE_MAX: usize = 5;
/// Claims one note is checked for, at most.
pub const CLAIMS_MAX: usize = 60;
/// Anchored files read for the names a note writes, at most.
const ANCHOR_READS_MAX: usize = 20;
/// What a revision's reason starts its account of a hold with.
pub const HELD: &str = "held for review: ";
/// A count_mismatch lists at most this many files.
const RECOUNT_FILES_MAX: usize = 8;
/// A set over this many files is not read to re-count it.
const MEMBERS_READ_MAX: usize = 300;
/// Extensions a named path may carry besides a code language's.
const FILE_EXTS: [&str; 22] = [
    "md", "markdown", "txt", "rst", "json", "jsonc", "toml", "yaml", "yml", "html", "htm", "css", "scss", "less", "sql", "xml",
    "ini", "cfg", "conf", "proto", "vue", "svelte",
];

/// What a save's code check found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeCheck {
    /// `checked` (an index answered), `refreshed` (a stale one was refreshed first) or
    /// `none` (no index: anchors only).
    pub index: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_ms: Option<i64>,
    /// The names and paths checked.
    pub names: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<CodeFinding>,
    /// The note was saved as needs_review because of the findings.
    pub held: bool,
    /// The index could not be read; anchors alone were judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeFinding {
    pub rule: &'static str,
    pub name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

impl CodeCheck {
    /// The create revision's reason when the note is held: the findings that hold it.
    pub fn reason(&self, max: usize) -> String {
        let found: Vec<String> = self.findings.iter().filter(|f| f.holds()).map(|f| format!("{} {}", f.rule, f.name)).collect();
        crate::view::clip(&format!("{HELD}{}", found.join(", ")), max)
    }
}

impl CodeFinding {
    /// Whether the finding holds the note (elsewhere only informs).
    pub fn holds(&self) -> bool {
        self.rule != "elsewhere"
    }
}

/// One thing a note says about the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A repo-relative file path.
    Path(String),
    /// A name written as code: judged absent when the index holds it nowhere.
    Code(String),
    /// A name that may be code or prose: only ever placed elsewhere.
    Plain(String),
}

impl Claim {
    pub fn text(&self) -> &str {
        match self {
            Claim::Path(t) | Claim::Code(t) | Claim::Plain(t) => t,
        }
    }
}

/// The claims of a note's title and content, in first-seen order, each once.
pub fn claims(title: &str, content: &str) -> Vec<Claim> {
    let mut out: Vec<Claim> = Vec::new();
    let push = |claim: Claim, out: &mut Vec<Claim>| {
        if out.len() < CLAIMS_MAX && !out.iter().any(|c| c.text() == claim.text()) {
            out.push(claim);
        }
    };
    for text in [title, content] {
        // Backticked spans first: a span marks its text as code.
        for (index, span) in text.split('`').enumerate() {
            if index % 2 == 1 && !span.is_empty() && span.is_ascii() && !span.contains(char::is_whitespace) {
                for claim in classify(span, true) {
                    push(claim, &mut out);
                }
            }
        }
        // Then every token, with anything outside ASCII (CJK prose and its punctuation) as a gap.
        let plain: String = text.chars().map(|c| if c.is_ascii() && c != '`' { c } else { ' ' }).collect();
        for token in plain.split_whitespace() {
            for claim in classify(token, false) {
                push(claim, &mut out);
            }
        }
    }
    out
}

/// The claims one token makes.
fn classify(raw: &str, marked: bool) -> Vec<Claim> {
    let token = raw
        .trim_start_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | '/' | '~')))
        .trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '/')));
    if token.len() < 3
        || token.contains("://")
        || token.starts_with(['/', '~'])
        || token.starts_with("../")
        || token.contains(['*', '?', '{', '}', '\\', '[', ']', '<', '>', '=', ',', '"', '\''])
    {
        return Vec::new();
    }
    let token = token.trim_start_matches("./");
    if let Some(path) = file_path(token) {
        return vec![Claim::Path(path)];
    }
    // A bare file name (`config.toml`, `session.json`) is often a runtime file no listing
    // holds: neither a path nor a name.
    if has_file_ext(token) {
        return Vec::new();
    }
    if !(marked || is_code(token)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for atom in token.split("::").flat_map(|part| part.split('.')) {
        let atom = atom.trim_end_matches("()");
        if atom.len() < 3 || atom.len() > NAME_MAX || !is_identifier(atom) {
            continue;
        }
        if is_strong(atom, marked) && !is_pattern(atom) {
            out.push(Claim::Code(atom.to_string()));
        } else {
            out.push(Claim::Plain(atom.to_string()));
        }
    }
    out
}

/// A token that names a file path: a `/`, and a last segment with a code or document
/// extension; any `:line` or `#anchor` after it dropped.
fn file_path(token: &str) -> Option<String> {
    if !token.contains('/') || token.ends_with('/') {
        return None;
    }
    let token = token.split(['#']).next().unwrap_or(token);
    let token = match token.rsplit_once(':') {
        Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit() || c == '-') => head,
        _ => token,
    };
    if token.contains(':') || token.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return None;
    }
    has_file_ext(token).then(|| token.to_string())
}

/// The path a prose token or a code span names, as [claims] reads it.
pub(crate) fn path_of(raw: &str) -> Option<String> {
    match classify(raw, true).as_slice() {
        [Claim::Path(path)] => Some(path.clone()),
        _ => None,
    }
}

/// The names a code span writes (`Store::write` is two), as [claims] reads them.
pub(crate) fn names_of(span: &str) -> Vec<String> {
    classify(span, true)
        .into_iter()
        .filter_map(|claim| match claim {
            Claim::Code(name) | Claim::Plain(name) => Some(name),
            Claim::Path(_) => None,
        })
        .collect()
}

fn has_file_ext(token: &str) -> bool {
    let name = token.rsplit('/').next().unwrap_or(token);
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty() && (Lang::of(name).is_some() || FILE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        }
        None => false,
    }
}

fn is_identifier(word: &str) -> bool {
    word.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A name written unmistakably as code: an inner `_` or a `$`, or a lower-case start with
/// a capital inside (`fitRailLabels`); or, in backticks, any code-like name. `PascalCase`
/// outside backticks is as often a product name (`GitHub`, `DeepSeek`): plain.
fn is_strong(atom: &str, marked: bool) -> bool {
    let inner_underscore = atom.trim_matches('_').contains('_');
    let lower_camel = atom.starts_with(|c: char| c.is_ascii_lowercase()) && atom.chars().any(|c| c.is_ascii_uppercase());
    inner_underscore || atom.contains('$') || lower_camel || (marked && is_code(atom))
}

/// A name the code builds rather than writes whole: a prefix or a suffix (`welfare_task_`,
/// `_coin_tradeRule_light`), or a lower-case name with a placeholder for its last part
/// (`tstd_jour_NNN`, `auto_post_text_N`, `log_yyyyMMdd`). No file holds it as written, so
/// it is never called absent - only placed elsewhere, like a plain name.
fn is_pattern(atom: &str) -> bool {
    if atom.starts_with('_') || atom.ends_with('_') {
        return true;
    }
    let Some((head, last)) = atom.rsplit_once('_') else { return false };
    let repeated = last.chars().next().is_some_and(|first| first.is_ascii_uppercase() && last.chars().all(|c| c == first));
    let date = matches!(last, "yyyyMMdd" | "YYYYMMDD" | "yyyyMM" | "YYYYMM" | "yyyy" | "YYYY");
    (repeated || date) && head.chars().any(|c| c.is_ascii_lowercase())
}

/// Whether `word` occurs in `text` with no identifier character on either side.
pub fn holds_word(text: &str, word: &str) -> bool {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = start == 0 || !ident(bytes[start - 1]);
        let after = end >= bytes.len() || !ident(bytes[end]);
        if before && after {
            return true;
        }
        from = start + word.len().max(1);
    }
    false
}

/// Whether `stem` begins two or more different names in `text`, cut at an `_` or before a
/// digit: the stem a note writes for a family of keys (`shop_email_coupon` for
/// `shop_email_coupon_name_1` .. `_9`, `verify_id_CC` for one key per country code).
/// One name alone is no family - `send_log` beside a lone `send_log_detail` stays unknown.
pub fn holds_stem(text: &str, stem: &str) -> bool {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    if stem.is_empty() {
        return false;
    }
    let ends_in_digit = stem.ends_with(|c: char| c.is_ascii_digit());
    let bytes = text.as_bytes();
    let mut first: Option<&str> = None;
    let mut from = 0;
    while let Some(at) = text[from..].find(stem) {
        let start = from + at;
        let end = start + stem.len();
        from = start + stem.len().max(1);
        if start > 0 && ident(bytes[start - 1]) {
            continue;
        }
        match bytes.get(end) {
            Some(b'_') => {}
            Some(b) if b.is_ascii_digit() && !ends_in_digit => {}
            _ => continue,
        }
        let tail = bytes[end..].iter().take_while(|b| ident(**b)).count();
        let name = &text[start..end + tail];
        match first {
            None => first = Some(name),
            Some(seen) if seen != name => return true,
            Some(_) => {}
        }
    }
    false
}

/// The text of the note's file anchors, read live (each up to the hashing limit; a file
/// that holds credentials never).
pub fn anchored_text(root: &Path, anchors: &[Anchor]) -> String {
    let mut out = String::new();
    for anchor in anchors.iter().filter(|a| !a.glob && !secrets::sensitive_path(&a.path)).take(ANCHOR_READS_MAX) {
        let path = root.join(&anchor.path);
        let Ok(canonical) = path.canonicalize() else { continue };
        if !canonical.starts_with(root) {
            continue;
        }
        match std::fs::metadata(&canonical) {
            Ok(meta) if meta.is_file() && meta.len() <= HASH_SIZE_LIMIT => {}
            _ => continue,
        }
        if let Ok(bytes) = std::fs::read(&canonical) {
            out.push_str(&String::from_utf8_lossy(&bytes));
            out.push('\n');
        }
    }
    out
}

/// The file anchors that are not there (no index needed).
pub fn missing_anchors(root: &Path, anchors: &[Anchor]) -> Vec<CodeFinding> {
    anchors
        .iter()
        .filter(|a| !a.glob && fresh::probe(root, &a.path) == Probe::Missing)
        .map(|a| CodeFinding {
            rule: "missing_anchor",
            name: a.path.clone(),
            message: format!("the anchor {} is not a file or directory of this checkout", a.path),
            files: Vec::new(),
        })
        .collect()
}

/// A file under the root, read live: absent, not to be read (outside the root, a directory,
/// over the hashing limit, a credential file), or its text.
enum Read {
    Missing,
    Skipped,
    Text(String),
}

fn read_under(root: &Path, rel: &str) -> Read {
    if secrets::sensitive_path(rel) || rel.contains(['*', '?']) {
        return Read::Skipped;
    }
    let Ok(canonical) = root.join(rel).canonicalize() else { return Read::Missing };
    if !canonical.starts_with(root) {
        return Read::Skipped;
    }
    match std::fs::metadata(&canonical) {
        Ok(meta) if meta.is_file() && meta.len() <= HASH_SIZE_LIMIT => {}
        _ => return Read::Skipped,
    }
    match std::fs::read(&canonical) {
        Ok(bytes) => Read::Text(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => Read::Skipped,
    }
}

/// The evidence a note cites, against the files (no index needed): each code, test or doc
/// path is a file of the checkout, and each excerpt is in its file.
pub fn evidence_findings(root: &Path, proofs: &[EvidenceSpec]) -> Vec<CodeFinding> {
    let mut out: Vec<CodeFinding> = Vec::new();
    for spec in proofs.iter().filter(|s| matches!(s.source_type, SourceType::Code | SourceType::Test | SourceType::Doc)) {
        let Some(path) = spec.path.as_deref() else { continue };
        let found = match read_under(root, path) {
            Read::Missing => CodeFinding {
                rule: "missing_evidence",
                name: path.to_string(),
                message: format!("the evidence {path} is not a file of this checkout: cite the file the fact rests on"),
                files: Vec::new(),
            },
            Read::Text(text) if spec.excerpt.as_deref().is_some_and(|e| !quoted(&text, e)) => CodeFinding {
                rule: "excerpt_mismatch",
                name: path.to_string(),
                message: format!("the excerpt cited from {path} is not in it: copy the lines as the file has them"),
                files: vec![path.to_string()],
            },
            _ => continue,
        };
        if !out.iter().any(|f| f.rule == found.rule && f.name == found.name) {
            out.push(found);
        }
    }
    out
}

/// Whether `excerpt` is in `text`, every whitespace run taken as one space and a string's
/// escapes as what they stand for ([unescaped]); an elision (`...` or `…`) or a
/// `[redacted:<rule>]` marker - a value the model was never shown - lets the parts around
/// it stand apart, in order.
pub fn quoted(text: &str, excerpt: &str) -> bool {
    let squash = |t: &str| unescaped(t).split_whitespace().collect::<Vec<_>>().join(" ");
    let text = squash(text);
    let mut from = 0;
    let mut parts = 0;
    for part in elided(excerpt).into_iter().map(squash).filter(|p| !p.is_empty()) {
        match text[from..].find(&part) {
            Some(at) => from += at + part.len(),
            None => return false,
        }
        parts += 1;
    }
    parts > 0
}

/// The pieces of an excerpt between its elisions and its redaction markers. A recheck's
/// marker for code it left out (`⋯ lines 3-9 not shown ⋯`, `⋯ 812 bytes not shown ⋯`),
/// copied along with the code around it, is an elision too.
fn elided(excerpt: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let shown = excerpt.split('\u{22ef}').filter(|piece| !unshown(piece));
    for piece in shown.flat_map(|p| p.split("...")).flat_map(|p| p.split('\u{2026}')) {
        let mut rest = piece;
        while let Some(at) = rest.find("[redacted:") {
            let Some(close) = rest[at..].find(']') else { break };
            out.push(&rest[..at]);
            rest = &rest[at + close + 1..];
        }
        out.push(rest);
    }
    out
}

/// Whether a piece between two `⋯` is a recheck's marker for what it left out:
/// `lines A-B not shown` or `N bytes not shown`.
fn unshown(piece: &str) -> bool {
    let piece = piece.trim();
    let Some(what) = piece.strip_suffix(" not shown") else { return false };
    let count = what.strip_prefix("lines ").or_else(|| what.strip_suffix(" bytes")).unwrap_or_default();
    !count.is_empty() && count.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// A text with a string literal's escapes read as what they stand for: a backslash before
/// a quote, a slash or another backslash dropped, a written `\n`, `\r` or `\t` a space. A
/// model quoting a SQL or JSON string drops or doubles an escape level as it writes its
/// own JSON (`lang=\"en\"` comes back `lang="en"`); the words it quotes are the same.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('n' | 'r' | 't') => {
                chars.next();
                out.push(' ');
            }
            Some('"' | '\'' | '/' | '\\') => {}
            _ => out.push(c),
        }
    }
    out
}

/// The claims over a set that nothing re-counts or scopes; a count that a quoted excerpt
/// states (a constant, `const SLOTS: usize = 64`) rests on that quote.
pub fn unbacked_sets(claims: &[SetClaim], excerpts: &[&str]) -> Vec<CodeFinding> {
    let mut out: Vec<CodeFinding> = Vec::new();
    for claim in claims.iter().filter(|c| c.unbacked()) {
        let stated = match claim.quantity {
            Quantity::Count(n) | Quantity::All(Some(n)) => {
                let word = claim.phrase.rsplit(' ').next().unwrap_or_default();
                excerpts.iter().any(|e| holds_word(e, &n.to_string()) || holds_word(&e.to_lowercase(), word))
            }
            _ => false,
        };
        if stated || out.iter().any(|f| f.name == claim.phrase) {
            continue;
        }
        out.push(CodeFinding {
            rule: "unscoped_quantifier",
            name: claim.phrase.clone(),
            message: formation::unbacked_message(&claim.phrase),
            files: Vec::new(),
        });
    }
    out
}

/// The members of a claim's sets among a root's paths: a glob's matches, a directory's files.
fn members(claim: &SetClaim, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| claim.sets.iter().any(|s| if s.ends_with('/') { p.starts_with(s.as_str()) } else { glob::matches(s, p) }))
        .cloned()
        .collect()
}

/// The files the index says mention every name (each a code-like name, which the index
/// keeps every mention of), and the files that define any name of the claim; both sorted.
async fn holders(
    conn: &mut sqlx::sqlite::SqliteConnection,
    root_id: i64,
    claim: &SetClaim,
) -> crate::error::MemResult<(Vec<String>, Vec<String>)> {
    let mut all: Option<Vec<String>> = None;
    let mut definers = Vec::new();
    for name in &claim.names {
        let defs: Vec<String> = db::defs_of(conn, root_id, name).await?.into_iter().map(|(path, _, _)| path).collect();
        if is_code(name) {
            let mut files = db::refs_of(conn, root_id, name, usize::MAX).await?.paths();
            files.extend(defs.iter().cloned());
            files.sort();
            files.dedup();
            all = Some(match all {
                None => files,
                Some(kept) => kept.into_iter().filter(|f| files.binary_search(f).is_ok()).collect(),
            });
        }
        definers.extend(defs);
    }
    definers.sort();
    definers.dedup();
    Ok((all.unwrap_or_default(), definers))
}

/// One claim over a set re-counted; None when it holds or there is nothing to count. A
/// set's members are read live (a plain name like `main` is in no index of mentions);
/// a closed set with no set named is counted over the whole index by its code-like names.
/// `named` says whether the note anchors, names or cites a file.
pub async fn recount(
    conn: &mut sqlx::sqlite::SqliteConnection,
    root_id: i64,
    root: &Path,
    claim: &SetClaim,
    paths: &[String],
    named: &(dyn Fn(&str) -> bool + Send + Sync),
) -> crate::error::MemResult<Option<CodeFinding>> {
    if claim.sets.is_empty() && (claim.quantity != Quantity::Closed || !claim.names.iter().any(|n| is_code(n)) || claim.hedged) {
        return Ok(None);
    }
    let set = members(claim, paths);
    if set.len() > MEMBERS_READ_MAX {
        return Ok(None);
    }
    let (held, definers) = if claim.names.is_empty() { (Vec::new(), Vec::new()) } else { holders(conn, root_id, claim).await? };
    let holding: Vec<String> = if claim.names.is_empty() {
        set.clone()
    } else {
        let (base, files, names) = (root.to_path_buf(), set.clone(), claim.names.clone());
        tokio::task::spawn_blocking(move || {
            files
                .into_iter()
                .filter(|f| match read_under(&base, f) {
                    Read::Text(text) => names.iter().all(|n| holds_word(&text, n)),
                    _ => false,
                })
                .collect()
        })
        .await
        .map_err(|err| crate::error::MemError::store(format!("the set read failed: {err}")))?
    };
    let what = |files: &[String]| {
        let mut shown: Vec<&str> = files.iter().take(RECOUNT_FILES_MAX).map(String::as_str).collect();
        if files.len() > RECOUNT_FILES_MAX {
            shown.push("...");
        }
        shown.join(", ")
    };
    let sets = claim.sets.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
    let names = claim.names.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
    let holding_what = if claim.names.is_empty() { String::new() } else { format!(" mentioning {names}") };
    let (message, files) = match claim.quantity {
        Quantity::Count(n) if holding.len() != n as usize => {
            (format!("the index finds {} file(s) of {sets}{holding_what}, not {n}: {}", holding.len(), what(&holding)), holding)
        }
        Quantity::All(Some(n)) if set.len() != n as usize => {
            (format!("{sets} matches {} file(s), not {n}: {}", set.len(), what(&set)), set)
        }
        Quantity::All(_) if !claim.names.is_empty() => {
            let lacking: Vec<String> = set.into_iter().filter(|p| !named(p) && !holding.contains(p)).collect();
            if lacking.is_empty() {
                return Ok(None);
            }
            (format!("{} file(s) of {sets} do not mention {names}: {}", lacking.len(), what(&lacking)), lacking)
        }
        Quantity::Closed if !claim.names.is_empty() => {
            let scope = if claim.sets.is_empty() { held } else { holding };
            let outside: Vec<String> = scope.into_iter().filter(|p| !named(p) && definers.binary_search(p).is_err()).collect();
            if outside.is_empty() {
                return Ok(None);
            }
            (format!("{names} also appear in {} file(s) this note does not name: {}", outside.len(), what(&outside)), outside)
        }
        Quantity::Most if !claim.names.is_empty() && holding.len() * 2 <= set.len() => {
            (format!("{} of the {} file(s) of {sets} mention {names}: not most", holding.len(), set.len()), holding)
        }
        _ => return Ok(None),
    };
    Ok(Some(CodeFinding {
        rule: "count_mismatch",
        name: claim.phrase.clone(),
        message: format!("\"{}\": {message}", claim.phrase),
        files: files.into_iter().take(RECOUNT_FILES_MAX).collect(),
    }))
}

/// One claim against an index; None when it holds.
pub async fn judge(
    conn: &mut sqlx::sqlite::SqliteConnection,
    root_id: i64,
    root: &Path,
    claim: &Claim,
    anchors: &[Anchor],
    text: &str,
    complete: bool,
) -> crate::error::MemResult<Option<CodeFinding>> {
    let in_anchors = |path: &str| anchors.iter().any(|a| glob::anchor_matches(a, path));
    match claim {
        Claim::Path(path) => {
            if !complete || db::file_named(conn, root_id, path).await?.is_some() || fresh::probe(root, path) != Probe::Missing {
                return Ok(None);
            }
            Ok(Some(CodeFinding {
                rule: "missing_file",
                name: path.clone(),
                message: format!("{path} is not a file of this checkout"),
                files: Vec::new(),
            }))
        }
        Claim::Code(name) | Claim::Plain(name) => {
            let code = matches!(claim, Claim::Code(_));
            if (!code && anchors.is_empty()) || holds_word(text, name) || holds_stem(text, name) {
                return Ok(None);
            }
            let defs = db::defs_of(conn, root_id, name).await?;
            let refs = db::refs_of(conn, root_id, name, usize::MAX).await?.paths();
            if defs.iter().any(|(path, _, _)| in_anchors(path)) || refs.iter().any(|path| in_anchors(path)) {
                return Ok(None);
            }
            let mut files: Vec<String> = defs.into_iter().map(|(path, _, _)| path).collect();
            files.dedup();
            if !files.is_empty() {
                if anchors.is_empty() || files.len() > ELSEWHERE_MAX {
                    return Ok(None);
                }
                return Ok(Some(CodeFinding {
                    rule: "elsewhere",
                    name: name.clone(),
                    message: format!("{name} is defined in {}, not in the files this note anchors", files.join(", ")),
                    files,
                }));
            }
            if !code || !refs.is_empty() || !complete {
                return Ok(None);
            }
            Ok(Some(CodeFinding {
                rule: "unknown_name",
                name: name.clone(),
                message: format!("{name} is in no file of this checkout: nothing defines or mentions it"),
                files: Vec::new(),
            }))
        }
    }
}

/// Glob anchors that no indexed file matches.
pub fn empty_globs(paths: &[String], anchors: &[Anchor]) -> Vec<CodeFinding> {
    anchors
        .iter()
        .filter(|a| a.glob)
        .filter(|a| {
            let pattern = glob::Glob::new(&a.path);
            !paths.iter().any(|p| pattern.matches_or_under(p))
        })
        .map(|a| CodeFinding {
            rule: "missing_anchor",
            name: a.path.clone(),
            message: format!("the glob anchor {} matches no file of this checkout", a.path),
            files: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(found: &[Claim]) -> Vec<String> {
        found
            .iter()
            .map(|c| match c {
                Claim::Path(t) => format!("path {t}"),
                Claim::Code(t) => format!("code {t}"),
                Claim::Plain(t) => format!("plain {t}"),
            })
            .collect()
    }

    #[test]
    fn claims_split_paths_code_and_plain_names() {
        let found = claims(
            "Retry budget",
            "`Store::write` sets retry_budget (see src/store.rs:42, ./docs/SPEC.md#memory). GitHub and DeepSeek \
             are products; `session.json` is a runtime file; https://example.com/a_b.rs is a link; \
             调用fitRailLabels时 and $scopeId too. `BEGIN` and `probe()`, --producer, x86_64.",
        );
        assert_eq!(
            texts(&found),
            [
                "plain Store",
                "plain write",
                "plain BEGIN",
                "plain probe",
                "code retry_budget",
                "path src/store.rs",
                "path docs/SPEC.md",
                "plain GitHub",
                "plain DeepSeek",
                "code fitRailLabels",
                "code $scopeId",
                "code x86_64",
            ]
        );
    }

    #[test]
    fn a_name_built_at_runtime_is_never_called_absent() {
        let found = claims(
            "Keys",
            "`welfare_task_` + id, `_coin_tradeRule_light`, `tstd_jour_NNN`, auto_post_text_N, log_yyyyMMdd; \
             but MAX_N, send_log and x86_64 stay code.",
        );
        assert_eq!(
            texts(&found),
            [
                "plain welfare_task_",
                "plain _coin_tradeRule_light",
                "plain tstd_jour_NNN",
                "plain auto_post_text_N",
                "plain log_yyyyMMdd",
                "code MAX_N",
                "code send_log",
                "code x86_64",
            ]
        );
    }

    #[test]
    fn bounded_words_only() {
        assert!(holds_word("const SQLITE_BUSY: u32", "SQLITE_BUSY"));
        assert!(!holds_word("const SQLITE_BUSY_RETRY: u32", "SQLITE_BUSY"));
        assert!(holds_word("a SQLITE_BUSY_RETRY and SQLITE_BUSY.", "SQLITE_BUSY"));
        assert!(!holds_word("$scopeIdX", "scopeId"));
    }

    #[test]
    fn a_stem_holds_only_for_a_family_of_names() {
        let sql = "('shop_email_coupon_name_1'), ('shop_email_coupon_name_2'), \
                   ('verify_id_CC112233440'), ('verify_id_CC112233441'), ('send_log_detail')";
        assert!(holds_stem(sql, "shop_email_coupon"));
        assert!(holds_stem(sql, "verify_id_CC"));
        // One name is no family, a stem inside a longer word is none, nor is a whole word.
        assert!(!holds_stem(sql, "send_log"));
        assert!(!holds_stem(sql, "email_coupon"));
        assert!(!holds_stem("send_log send_log", "send_log"));
        assert!(!holds_stem("x", ""));
    }

    #[test]
    fn an_excerpt_matches_across_escape_levels_and_redaction() {
        let sql = r#"VALUES ('201172_etl_content', NULL, '<!DOCTYPE html>\r\n<html lang=\"en\">\r\n<a class=\"email\">mail</a>');"#;
        assert!(quoted(sql, r#"<!DOCTYPE html>\r\n<html lang="en">"#));
        assert!(quoted(sql, "<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(quoted(sql, r#"<a class=\\\"email\\\">mail</a>"#));
        assert!(!quoted(sql, r#"<a class="phone">mail</a>"#));
        let config = "VALUES ('aliyun_whatsapp_config', 'accessKeyId', 'AKIDEXAMPLEONLY', NULL);";
        assert!(quoted(config, "VALUES ('aliyun_whatsapp_config', 'accessKeyId', '[redacted:mixed_token]', NULL);"));
        assert!(!quoted(config, "VALUES ('aliyun_whatsapp_config', 'secret', '[redacted:mixed_token]', NULL);"));
        // An excerpt that is nothing but the withheld value proves nothing.
        assert!(!quoted(config, "[redacted:mixed_token]"));
        // A Windows path or a regex keeps its other backslashes on both sides alike.
        assert!(quoted(r"C:\Users\dev\a.txt and \d+", r"C:\Users\dev\a.txt and \d+"));
    }

    #[test]
    fn a_recheck_marker_copied_into_an_excerpt_is_an_elision() {
        let sql = "INSERT INTO t_mail (code, body) VALUES ('kyc_pass', '<p>Your KYC passed</p>'), ('kyc_fail', '<p>Rejected</p>');";
        assert!(quoted(sql, "VALUES ('kyc_pass', ⋯ 25 bytes not shown ⋯ ('kyc_fail', '<p>Rejected</p>');"));
        assert!(quoted(sql, "INSERT INTO t_mail (code, body)\n⋯ lines 2-40 not shown ⋯\n('kyc_fail'"));
        // The marker lets parts stand apart in order; it never makes text up.
        assert!(!quoted(sql, "('kyc_fail', ⋯ 25 bytes not shown ⋯ ('kyc_pass'"));
        assert!(!quoted(sql, "VALUES ('kyc_pass', ⋯ a note ⋯ ('kyc_fail'"));
        assert!(!quoted(sql, "⋯ 25 bytes not shown ⋯"));
    }

    #[test]
    fn paths_need_a_slash_and_a_known_extension() {
        assert_eq!(file_path("src/x.rs"), Some("src/x.rs".to_string()));
        assert_eq!(file_path("src/x.rs:12-20"), Some("src/x.rs".to_string()));
        assert_eq!(file_path("and/or"), None);
        assert_eq!(file_path("target/release/swiss.exe"), None);
        assert_eq!(file_path("src/plugins/"), None);
        assert_eq!(file_path("a//b.rs"), None);
        assert_eq!(file_path(".agents/.env"), None);
    }
}
