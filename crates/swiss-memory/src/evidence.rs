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

//! Evidence as a caller states it (ADR-032): what a note was derived from, what supports
//! it, and what verifies it. Every entry point reads it here - the CLI's compact
//! `[RELATION=]TYPE[:LOCATOR][#SYMBOL]`, the API's and MCP's JSON - so the rules are one
//! set: a source names a path, a symbol, a commit, a session or a run, never a line number;
//! only code, a test, a commit or a doc a reader can check verifies a note.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{MemError, MemResult};
use crate::fresh::{self, Probe};
use crate::model::{is_evidence_id, new_evidence_id, Evidence, Memory, Relation, SourceType, ANCHOR_PATH_MAX};
use crate::service::check_secret;
use crate::store;

/// One call attaches at most this many.
pub const EVIDENCE_PER_CALL_MAX: usize = 10;
/// A note carries at most this many, across its revisions.
pub const EVIDENCE_PER_RECORD_MAX: usize = 50;
const SYMBOL_MAX: usize = 200;
const LABEL_MAX: usize = 120;
const EXCERPT_MAX: usize = 4096;

/// Evidence as stated, before it is bound to a note's revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSpec {
    pub source_type: SourceType,
    /// Absent: the operation's default (`derived_from` on a save or an edit, `verified_by` on
    /// a verified mark).
    pub relation: Option<Relation>,
    pub path: Option<String>,
    pub symbol: Option<String>,
    pub commit_sha: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    /// The quoted text the claim rests on: only its hash is kept.
    pub excerpt: Option<String>,
}

/// The API's and MCP's spelling of one entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Wire {
    source_type: String,
    #[serde(default)]
    relation: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    commit_sha: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    excerpt: Option<String>,
}

fn invalid(message: impl std::fmt::Display) -> MemError {
    MemError::invalid(format!("evidence: {message}"))
}

fn source_type(raw: &str) -> MemResult<SourceType> {
    SourceType::parse(raw.trim()).ok_or_else(|| invalid(format!("unknown type {raw:?}; one of {}", SourceType::choices())))
}

fn relation(raw: &str) -> MemResult<Relation> {
    Relation::parse(raw.trim()).ok_or_else(|| invalid(format!("unknown relation {raw:?}; one of {}", Relation::choices())))
}

fn some(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_string())
}

impl EvidenceSpec {
    pub fn new(source_type: SourceType) -> EvidenceSpec {
        EvidenceSpec {
            source_type,
            relation: None,
            path: None,
            symbol: None,
            commit_sha: None,
            session_id: None,
            run_id: None,
            excerpt: None,
        }
    }

    /// The CLI's compact form, `[RELATION=]TYPE[:LOCATOR][#SYMBOL]`: `code:src/retry.rs#retry`,
    /// `verified_by=test:tests/retry.rs#reuses_the_key`, `commit:abc1234`, `session:ID`,
    /// `bootstrap:RUN`, `human`. The locator is a path (code, test, doc), a commit sha, a
    /// session id or a run id (bootstrap, import). One JSON object in the API's spelling is
    /// read as the API reads it, so the CLI can quote an excerpt.
    pub fn parse(raw: &str) -> MemResult<EvidenceSpec> {
        let raw = raw.trim();
        if raw.starts_with('{') {
            let value: Value = serde_json::from_str(raw).map_err(|e| invalid(format!("not one JSON object: {e}")))?;
            return EvidenceSpec::list_from_json(&Value::Array(vec![value]))?.pop().ok_or_else(|| invalid("an empty object"));
        }
        let colon = raw.find(':').unwrap_or(raw.len());
        let (relation, rest) = match raw.find('=') {
            Some(at) if at < colon => (Some(self::relation(&raw[..at])?), &raw[at + 1..]),
            _ => (None, raw),
        };
        let (kind, locator) = rest.split_once(':').unwrap_or((rest, ""));
        let mut spec = EvidenceSpec::new(source_type(kind)?);
        spec.relation = relation;
        let (locator, symbol) = match locator.split_once('#') {
            Some((locator, symbol)) => (locator, Some(symbol)),
            None => (locator, None),
        };
        spec.symbol = symbol.and_then(some);
        let locator = some(locator);
        match spec.source_type {
            SourceType::Code | SourceType::Test | SourceType::Doc => spec.path = locator,
            SourceType::Commit => spec.commit_sha = locator,
            SourceType::Session => spec.session_id = locator,
            SourceType::Bootstrap | SourceType::Import => spec.run_id = locator,
            SourceType::Human if locator.is_some() => return Err(invalid("human evidence names no source; pass `human`")),
            SourceType::Human => {}
        }
        spec.checked(Relation::DerivedFrom)
    }

    /// The API's and MCP's `evidence`: an array of `{sourceType, relation?, path?, symbol?,
    /// commitSha?, sessionId?, runId?, excerpt?}`. An unknown field is refused, so a line
    /// number cannot ride along unnoticed.
    pub fn list_from_json(value: &Value) -> MemResult<Vec<EvidenceSpec>> {
        let Value::Array(items) = value else {
            return Err(invalid("pass an array of {sourceType, relation, path, symbol, commitSha, sessionId, runId, excerpt}"));
        };
        if items.len() > EVIDENCE_PER_CALL_MAX {
            return Err(invalid(format!("at most {EVIDENCE_PER_CALL_MAX} at a time")));
        }
        let mut out = Vec::with_capacity(items.len());
        for (n, item) in items.iter().enumerate() {
            let wire: Wire =
                serde_json::from_value(item.clone()).map_err(|e| MemError::invalid(format!("evidence[{n}]: {e}")))?;
            let mut spec = EvidenceSpec::new(source_type(&wire.source_type)?);
            spec.relation = wire.relation.as_deref().map(relation).transpose()?;
            spec.path = wire.path.as_deref().and_then(some);
            spec.symbol = wire.symbol.as_deref().and_then(some);
            spec.commit_sha = wire.commit_sha.as_deref().and_then(some);
            spec.session_id = wire.session_id.as_deref().and_then(some);
            spec.run_id = wire.run_id.as_deref().and_then(some);
            spec.excerpt = wire.excerpt.filter(|e| !e.trim().is_empty());
            out.push(spec.checked(Relation::DerivedFrom).map_err(|e| MemError::invalid(format!("evidence[{n}]: {}", e.message)))?);
        }
        Ok(out)
    }

    /// Every rule, with `default` standing in for an absent relation; answers the spec
    /// normalized (a lowercase sha, a clean path).
    pub(crate) fn checked(mut self, default: Relation) -> MemResult<EvidenceSpec> {
        let kind = self.source_type;
        let relation = self.relation.unwrap_or(default);
        if relation == Relation::VerifiedBy && !kind.verifies() {
            return Err(invalid(format!(
                "verified_by needs code, a test, a commit or a doc a reader can check; {} evidence derives or supports a note",
                kind.as_str()
            )));
        }
        let needs = match kind {
            SourceType::Code | SourceType::Test | SourceType::Doc => self.path.is_none().then_some("a path"),
            SourceType::Commit => self.commit_sha.is_none().then_some("a commit sha"),
            SourceType::Session => self.session_id.is_none().then_some("a session id"),
            SourceType::Bootstrap => self.run_id.is_none().then_some("a run id"),
            SourceType::Import | SourceType::Human => None,
        };
        if let Some(what) = needs {
            return Err(invalid(format!("{} evidence needs {what}", kind.as_str())));
        }
        if let Some(path) = &self.path {
            self.path = Some(clean_path(path)?);
        }
        if let Some(symbol) = &self.symbol {
            if symbol.contains(['\n', '\r']) || symbol.chars().count() > SYMBOL_MAX {
                return Err(invalid(format!("a symbol is one line of at most {SYMBOL_MAX} characters")));
            }
            if is_line_number(symbol) {
                return Err(invalid(format!("{symbol:?} is a line number, not an identity: name the symbol")));
            }
            check_secret("evidence", symbol)?;
        }
        if let Some(sha) = &self.commit_sha {
            let sha = sha.to_ascii_lowercase();
            if !(7..=64).contains(&sha.len()) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid(format!("{sha:?} is not a commit sha (7 to 64 hex digits)")));
            }
            self.commit_sha = Some(sha);
        }
        for label in [&self.session_id, &self.run_id].into_iter().flatten() {
            if label.chars().count() > LABEL_MAX || label.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(invalid(format!("an id is at most {LABEL_MAX} characters without spaces")));
            }
            check_secret("evidence", label)?;
        }
        if let Some(excerpt) = &self.excerpt {
            if excerpt.len() > EXCERPT_MAX {
                return Err(invalid(format!("an excerpt is at most {EXCERPT_MAX} bytes")));
            }
        }
        Ok(self)
    }

    /// Bind to `memory`'s current text, stamping the cited file's hash when it lies under
    /// `root`.
    pub(crate) fn bind(&self, memory: &Memory, default: Relation, root: Option<&Path>, actor: &str, now: i64) -> Evidence {
        let source_hash = match (&self.path, root) {
            (Some(path), Some(root)) => match fresh::probe(root, path) {
                Probe::Hash(hash) => Some(hash),
                _ => None,
            },
            _ => None,
        };
        Evidence {
            id: new_evidence_id(),
            memory_id: memory.id.clone(),
            revision: memory.revision,
            note_hash: store::content_hash(&memory.title, &memory.content),
            source_type: self.source_type,
            relation: self.relation.unwrap_or(default),
            path: self.path.clone(),
            symbol: self.symbol.clone(),
            commit_sha: self.commit_sha.clone(),
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            source_hash,
            excerpt_hash: self.excerpt.as_deref().map(excerpt_hash),
            actor: actor.to_string(),
            created_ms: now,
        }
    }
}

/// An exported piece obeys every rule an attached one does, and belongs to the note whose
/// line carries it; its hashes keep their shapes, the excerpt itself never travels.
pub(crate) fn check_imported(evidence: &mut Evidence, memory: &Memory) -> MemResult<()> {
    if !is_evidence_id(&evidence.id) {
        return Err(invalid(format!("{:?} is not an evidence id", evidence.id)));
    }
    if evidence.memory_id != memory.id {
        return Err(invalid(format!("{} belongs to {}, not {}", evidence.id, evidence.memory_id, memory.id)));
    }
    if evidence.revision == 0 || evidence.revision > memory.revision {
        return Err(invalid(format!("{} is bound to revision {}, the note has {}", evidence.id, evidence.revision, memory.revision)));
    }
    let hex = |text: &str| text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex(&evidence.note_hash) {
        return Err(invalid(format!("{}: noteHash is not a sha256", evidence.id)));
    }
    if let Some(hash) = &evidence.excerpt_hash {
        if !hash.strip_prefix("sha256:").is_some_and(hex) {
            return Err(invalid(format!("{}: excerptHash is not sha256:<hex>", evidence.id)));
        }
    }
    if let Some(hash) = &evidence.source_hash {
        if hash.len() > LABEL_MAX || hash.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(invalid(format!("{}: sourceHash is not a file hash", evidence.id)));
        }
    }
    if evidence.actor.chars().count() > LABEL_MAX || evidence.actor.contains(['\n', '\r']) {
        return Err(invalid(format!("{}: an actor is one line of at most {LABEL_MAX} characters", evidence.id)));
    }
    check_secret("evidence", &evidence.actor)?;
    let spec = EvidenceSpec {
        source_type: evidence.source_type,
        relation: Some(evidence.relation),
        path: evidence.path.take(),
        symbol: evidence.symbol.take(),
        commit_sha: evidence.commit_sha.take(),
        session_id: evidence.session_id.take(),
        run_id: evidence.run_id.take(),
        excerpt: None,
    }
    .checked(evidence.relation)?;
    evidence.path = spec.path;
    evidence.symbol = spec.symbol;
    evidence.commit_sha = spec.commit_sha;
    evidence.session_id = spec.session_id;
    evidence.run_id = spec.run_id;
    Ok(())
}

/// Check a whole call's evidence against the operation's default relation.
pub(crate) fn check_all(specs: &[EvidenceSpec], default: Relation) -> MemResult<Vec<EvidenceSpec>> {
    if specs.len() > EVIDENCE_PER_CALL_MAX {
        return Err(invalid(format!("at most {EVIDENCE_PER_CALL_MAX} at a time")));
    }
    let mut out: Vec<EvidenceSpec> = Vec::with_capacity(specs.len());
    for spec in specs {
        let spec = spec.clone().checked(default)?;
        if !out.contains(&spec) {
            out.push(spec);
        }
    }
    Ok(out)
}

/// A repository-relative file: no glob, no way out of the root, no line number.
fn clean_path(raw: &str) -> MemResult<String> {
    let path = crate::glob::normalize(raw);
    if path.is_empty() || path.chars().count() > ANCHOR_PATH_MAX || path.contains(['\n', '\r']) {
        return Err(invalid(format!("a path is one line of 1 to {ANCHOR_PATH_MAX} characters")));
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    if path.starts_with('/') || drive || path.split('/').any(|segment| segment == "..") {
        return Err(invalid(format!("{path} must be relative to the project root, inside it")));
    }
    if path.contains(['*', '?', '[']) {
        return Err(invalid(format!("{path} is a glob; evidence names one file")));
    }
    if let Some((_, tail)) = path.rsplit_once(':') {
        if is_line_number(tail) {
            return Err(invalid(format!("{path}: a line number is not an identity; name the symbol with #")));
        }
    }
    check_secret("evidence", &path)?;
    Ok(path)
}

/// `42`, `42-50`, `L42`, `L42-L50`.
fn is_line_number(text: &str) -> bool {
    let digits = |part: &str| {
        let part = part.strip_prefix('L').unwrap_or(part);
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
    };
    match text.split_once('-') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(text),
    }
}

/// `sha256:` + the hex of the excerpt with line ends normalized and trailing space dropped.
pub fn excerpt_hash(excerpt: &str) -> String {
    let body = excerpt.replace("\r\n", "\n");
    let body = body.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    let digest: [u8; 32] = Sha256::digest(body.trim().as_bytes()).into();
    format!("sha256:{}", swiss_core::util::to_hex(&digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_reads_one_json_object_the_api_way() {
        let spec = EvidenceSpec::parse(r#"{"sourceType": "code", "path": "src/retry.rs", "excerpt": "fn retry() {}"}"#).unwrap();
        assert_eq!((spec.path.as_deref(), spec.excerpt.as_deref()), (Some("src/retry.rs"), Some("fn retry() {}")));
        let err = EvidenceSpec::parse(r#"{"sourceType": "code", "path": "src/retry.rs", "line": 4}"#).unwrap_err();
        assert!(err.message.contains("line"), "an unknown field is refused: {}", err.message);
    }

    #[test]
    fn the_compact_form_names_its_parts() {
        let spec = EvidenceSpec::parse("verified_by=code:src/Retry.java#Retry#run").unwrap();
        assert_eq!(spec.source_type, SourceType::Code);
        assert_eq!(spec.relation, Some(Relation::VerifiedBy));
        assert_eq!(spec.path.as_deref(), Some("src/Retry.java"));
        assert_eq!(spec.symbol.as_deref(), Some("Retry#run"), "a Java member keeps its #");
        let commit = EvidenceSpec::parse("commit:ABCDEF1").unwrap();
        assert_eq!(commit.commit_sha.as_deref(), Some("abcdef1"));
        assert_eq!(commit.relation, None, "the operation decides");
        assert_eq!(EvidenceSpec::parse(" human ").unwrap(), EvidenceSpec::new(SourceType::Human));
        assert_eq!(EvidenceSpec::parse("import").unwrap().run_id, None);
        assert_eq!(EvidenceSpec::parse("bootstrap:run-1").unwrap().run_id.as_deref(), Some("run-1"));
        assert!(EvidenceSpec::parse("human:alice").is_err());
    }

    #[test]
    fn line_numbers_are_not_identities() {
        for text in ["42", "42-50", "L42", "L42-L50"] {
            assert!(is_line_number(text), "{text}");
        }
        for text in ["retry", "v2", "L", "42a", "-", "x-1"] {
            assert!(!is_line_number(text), "{text}");
        }
        assert!(clean_path("src/a.rs:12").is_err());
        assert_eq!(clean_path(".\\src\\a.rs").unwrap(), "src/a.rs");
    }

    #[test]
    fn a_verified_mark_needs_a_checkable_source() {
        let session = EvidenceSpec::parse("session:s-1").unwrap();
        assert!(check_all(std::slice::from_ref(&session), Relation::DerivedFrom).is_ok());
        assert!(check_all(&[session], Relation::VerifiedBy).is_err());
        let code = EvidenceSpec::parse("code:src/a.rs").unwrap();
        assert_eq!(check_all(&[code.clone(), code], Relation::VerifiedBy).unwrap().len(), 1, "one call, one copy");
    }

    #[test]
    fn an_excerpt_hash_ignores_line_ends() {
        assert_eq!(excerpt_hash("fn run() {\r\n  go();  \r\n}\n"), excerpt_hash("fn run() {\n  go();\n}"));
        assert!(excerpt_hash("x").starts_with("sha256:"));
    }
}
