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

//! Recorded misses (SPEC §memory.evaluation Misses): a retrieval that failed the agent or
//! the person who ran it - the query or task, the notes that came back, which of them were
//! wrong, and the notes that should have come (or, when none exists, what was wanted in
//! words). A miss changes no note; it is the evaluation's raw material.
//! [MemoryService::export_misses] turns a project's misses into a fixture ([Fixture]): the
//! project's map and notes, one query per miss that names a wanted note, so a failure seen
//! in real use becomes a query the evaluation scores - and keeps scoring after the fix.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqlx::sqlite::SqliteConnection;
use sqlx::Row;

use crate::error::{MemError, MemResult};
use crate::eval::{self, Fixture, FixtureDomain, FixtureLink, FixtureNote, FixtureProject, FixtureQuery, STYLES};
use crate::evidence::EVIDENCE_PER_CALL_MAX;
use crate::model::{is_record_id, Actor, Evidence, Memory, ANCHOR_PATH_MAX};
use crate::scope::GLOBAL_PROJECT_ID;
use crate::search;
use crate::service::{check_secret, MemoryService, Target};
use crate::store::{self, Filter};

/// A query or task, as long as a brief's task may reasonably run.
pub const QUERY_MAX: usize = 1000;
/// What was wanted, in words.
pub const WANT_MAX: usize = 1000;
pub const SHOWN_MAX: usize = 50;
pub const WRONG_MAX: usize = 20;
pub const WANTED_MAX: usize = 10;
pub const PATHS_MAX: usize = 20;
/// A project keeps its newest this many; recording one past it drops the oldest.
pub const MISSES_MAX: usize = 500;
/// The retrievals a miss can name.
pub const VIAS: [&str; 5] = ["search", "brief", "related", "ask", "domain"];

/// A miss as its recorder states it.
#[derive(Debug, Clone, Default)]
pub struct MissRequest {
    /// The query or task that was run.
    pub query: String,
    /// Which retrieval: one of [VIAS].
    pub via: Option<String>,
    /// One of [STYLES]; left out, the export infers it ([infer_style]).
    pub style: Option<String>,
    /// The notes that came back, in order.
    pub shown: Vec<String>,
    /// Of what came back (or anywhere), the notes that misled.
    pub wrong: Vec<String>,
    /// The notes that should have come.
    pub wanted: Vec<String>,
    /// What was wanted, in words: the only statement when no note holds it.
    pub want: Option<String>,
    /// The files the recorder worked on (a brief's paths).
    pub paths: Vec<String>,
}

/// One recorded miss.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedMiss {
    pub id: String,
    pub project: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    pub shown: Vec<String>,
    pub wrong: Vec<String>,
    pub wanted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub actor: String,
    pub at_ms: i64,
}

/// A project's misses as a fixture.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissExport {
    pub fixture: Fixture,
    /// The misses that became the fixture's queries.
    pub queries: usize,
    /// The misses naming no wanted note the export carries: a note to write, not a ranking
    /// to fix - listed, never made a query.
    pub gaps: Vec<RecordedMiss>,
}

fn new_miss_id() -> String {
    format!("x-{}", swiss_core::util::random_hex(4))
}

fn is_miss_id(value: &str) -> bool {
    value.len() == 10 && value.starts_with("x-") && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Trimmed, non-empty, at most `max` characters, no credential.
fn text(field: &str, raw: &str, max: usize) -> MemResult<String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(MemError::invalid(format!("{field} is empty")));
    }
    let n = text.chars().count();
    if n > max {
        return Err(MemError::invalid(format!("{field} is {n} characters; the limit is {max}")));
    }
    check_secret(field, text)?;
    Ok(text.to_string())
}

/// Note ids, trimmed and deduplicated in order.
fn ids(field: &str, raw: &[String], max: usize) -> MemResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for id in raw.iter().map(|id| id.trim()).filter(|id| !id.is_empty()) {
        if !is_record_id(id) {
            return Err(MemError::invalid(format!("{field}: {id:?} is not a note id (m-…)")));
        }
        if !out.iter().any(|seen| seen == id) {
            out.push(id.to_string());
        }
    }
    if out.len() > max {
        return Err(MemError::invalid(format!("{field}: at most {max} notes")));
    }
    Ok(out)
}

fn choice(field: &str, raw: Option<&str>, choices: &[&str]) -> MemResult<Option<String>> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(value) if choices.contains(&value) => Ok(Some(value.to_string())),
        Some(value) => Err(MemError::invalid(format!("{field}: {value:?} is not one of {}", choices.join(", ")))),
    }
}

/// Repository-relative files, the way a brief's paths are: no glob, no way out of the root.
fn paths(raw: &[String]) -> MemResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in raw.iter().filter(|p| !p.trim().is_empty()) {
        let path = crate::glob::normalize(raw.trim());
        if path.chars().count() > ANCHOR_PATH_MAX || path.contains(['*', '?', '[', '\n', '\r']) || !eval::relative_inside(&path) {
            return Err(MemError::invalid(format!("paths: {raw:?} is not one file relative to the project root")));
        }
        check_secret("paths", &path)?;
        if !out.contains(&path) {
            out.push(path);
        }
    }
    if out.len() > PATHS_MAX {
        return Err(MemError::invalid(format!("paths: at most {PATHS_MAX}")));
    }
    Ok(out)
}

/// Every rule a miss obeys before the store is touched; answers it clean, unsaved.
fn checked(request: &MissRequest) -> MemResult<RecordedMiss> {
    let query = text("query", &request.query, QUERY_MAX)?;
    let want = match request.want.as_deref().filter(|w| !w.trim().is_empty()) {
        Some(raw) => Some(text("want", raw, WANT_MAX)?),
        None => None,
    };
    let wanted = ids("wanted", &request.wanted, WANTED_MAX)?;
    let wrong = ids("wrong", &request.wrong, WRONG_MAX)?;
    if wanted.is_empty() && want.is_none() {
        return Err(MemError::invalid(
            "say what you wanted: the wanted notes' ids, or want - in words, when no note holds it",
        ));
    }
    if let Some(both) = wanted.iter().find(|id| wrong.contains(id)) {
        return Err(MemError::invalid(format!("{both} is both wanted and wrong")));
    }
    Ok(RecordedMiss {
        id: new_miss_id(),
        project: String::new(),
        query,
        via: choice("via", request.via.as_deref(), &VIAS)?,
        style: choice("style", request.style.as_deref(), &STYLES)?,
        shown: ids("shown", &request.shown, SHOWN_MAX)?,
        wrong,
        wanted,
        want,
        paths: paths(&request.paths)?,
        actor: String::new(),
        at_ms: 0,
    })
}

/// The query style a miss recorded without one reads as: a lone path, an identifier among
/// words, two words or four CJK characters at most, else a natural question.
pub fn infer_style(query: &str) -> &'static str {
    let query = query.trim();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.len() == 1 && (query.contains(['/', '\\']) || has_extension(query)) {
        return "path";
    }
    if words.iter().any(|w| is_identifier(w)) {
        return "symbol";
    }
    let short = if search::has_cjk(query) {
        query.chars().filter(|c| !c.is_whitespace()).count() <= 4
    } else {
        words.len() <= 2
    };
    if short {
        "short"
    } else {
        "natural"
    }
}

/// `store.rs`, `README.md`: a stem, a dot, a short extension holding a letter.
fn has_extension(word: &str) -> bool {
    match word.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && ext.bytes().any(|b| b.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// `snake_case`, `Type::method`, `camelCase`, `PascalCase`, `call()`.
fn is_identifier(word: &str) -> bool {
    let word = word.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | ',' | '.' | '?' | '!' | ':' | ';'));
    if word.contains("::") || word.ends_with("()") {
        return true;
    }
    if !word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !word.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if word.contains('_') && word.trim_matches('_').len() > 1 {
        return true;
    }
    // An upper-case letter after a lower-case one: `camelCase`, `PascalCase`.
    let bytes = word.as_bytes();
    bytes.windows(2).any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
}

fn json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn to_json(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

/// A project's misses, newest first.
async fn rows(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Vec<RecordedMiss>> {
    let rows = sqlx::query(
        "SELECT id, project_id, query, via, style, shown_json, wrong_json, wanted_json, want, paths_json, actor, created_ms \
         FROM memory_misses WHERE project_id = ? ORDER BY rowid DESC",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .map(|row| -> MemResult<RecordedMiss> {
            Ok(RecordedMiss {
                id: row.try_get("id")?,
                project: row.try_get("project_id")?,
                query: row.try_get("query")?,
                via: row.try_get("via")?,
                style: row.try_get("style")?,
                shown: json_list(&row.try_get::<String, _>("shown_json")?),
                wrong: json_list(&row.try_get::<String, _>("wrong_json")?),
                wanted: json_list(&row.try_get::<String, _>("wanted_json")?),
                want: row.try_get("want")?,
                paths: json_list(&row.try_get::<String, _>("paths_json")?),
                actor: row.try_get("actor")?,
                at_ms: row.try_get("created_ms")?,
            })
        })
        .collect()
}

/// Evidence in the CLI's compact form, `RELATION=TYPE[:LOCATOR][#SYMBOL]`; None for a path
/// the form cannot carry (one holding `#`).
fn compact(evidence: &Evidence) -> Option<String> {
    let locator = evidence
        .path
        .as_deref()
        .or(evidence.commit_sha.as_deref())
        .or(evidence.session_id.as_deref())
        .or(evidence.run_id.as_deref());
    if evidence.path.as_deref().is_some_and(|p| p.contains('#')) {
        return None;
    }
    let mut out = format!("{}={}", evidence.relation.as_str(), evidence.source_type.as_str());
    if let Some(locator) = locator {
        out.push(':');
        out.push_str(locator);
    }
    if let Some(symbol) = &evidence.symbol {
        out.push('#');
        out.push_str(symbol);
    }
    Some(out)
}

/// A note as a fixture note, keyed by its id; its links and replaces are set after.
async fn fixture_note(conn: &mut SqliteConnection, memory: &Memory) -> MemResult<FixtureNote> {
    let hash = store::content_hash(&memory.title, &memory.content);
    let mut evidence: Vec<String> = Vec::new();
    for piece in store::evidence(conn, &memory.id).await?.iter().filter(|e| e.note_hash == hash) {
        if let Some(text) = compact(piece) {
            if !evidence.contains(&text) && evidence.len() < EVIDENCE_PER_CALL_MAX {
                evidence.push(text);
            }
        }
    }
    Ok(FixtureNote {
        key: memory.id.clone(),
        kind: memory.kind.as_str().to_string(),
        title: memory.title.clone(),
        content: memory.content.clone(),
        anchors: memory.anchors.iter().map(|a| a.path.clone()).collect(),
        refs: memory.refs.clone(),
        keywords: memory.keywords.clone(),
        links: Vec::new(),
        replaces: None,
        domain: memory.domain_path.clone(),
        origin: (!memory.confirmed).then(|| "agent".to_string()),
        evidence,
        drift: false,
    })
}

impl MemoryService {
    /// Record a retrieval that failed (SPEC §memory.evaluation Misses). Every note it names
    /// is one of the project's or a shared one; a project keeps its newest [MISSES_MAX].
    pub async fn record_miss(&self, target: &Target, actor: &Actor, request: MissRequest) -> MemResult<RecordedMiss> {
        let mut miss = checked(&request)?;
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id()?;
            if place.project.is_none() {
                return Err(MemError::not_found("no shared notes yet: a miss names notes that exist"));
            }
            for id in miss.shown.iter().chain(&miss.wrong).chain(&miss.wanted) {
                match store::memory(&mut conn, id).await? {
                    Some(m) if m.scope == project_id || m.scope == GLOBAL_PROJECT_ID => {}
                    _ => return Err(MemError::invalid(format!("{id} is not a note of this project"))),
                }
            }
            miss.project = project_id.clone();
            miss.actor = actor.label.clone();
            miss.at_ms = store::now();
            sqlx::query(
                "INSERT INTO memory_misses (id, project_id, query, via, style, shown_json, wrong_json, wanted_json, want, \
                 paths_json, actor, created_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&miss.id)
            .bind(&miss.project)
            .bind(&miss.query)
            .bind(&miss.via)
            .bind(&miss.style)
            .bind(to_json(&miss.shown))
            .bind(to_json(&miss.wrong))
            .bind(to_json(&miss.wanted))
            .bind(&miss.want)
            .bind(to_json(&miss.paths))
            .bind(&miss.actor)
            .bind(miss.at_ms)
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "DELETE FROM memory_misses WHERE project_id = ? AND rowid NOT IN \
                 (SELECT rowid FROM memory_misses WHERE project_id = ? ORDER BY rowid DESC LIMIT ?)",
            )
            .bind(&project_id)
            .bind(&project_id)
            .bind(MISSES_MAX as i64)
            .execute(&mut *conn)
            .await?;
            Ok(miss)
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// The project's misses, newest first; none where the directory is no project yet.
    pub async fn misses(&self, target: &Target) -> MemResult<Vec<RecordedMiss>> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let Ok(project_id) = place.project_id() else {
            return Ok(Vec::new());
        };
        rows(&mut conn, &project_id).await
    }

    /// Drop the named misses, or every one of the project's when `ids` is empty; answers
    /// how many went. An id the project does not hold is refused.
    pub async fn drop_misses(&self, target: &Target, ids: &[String]) -> MemResult<u64> {
        for id in ids {
            if !is_miss_id(id.trim()) {
                return Err(MemError::invalid(format!("{id:?} is not a miss id (x-…)")));
            }
        }
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            let place = self.locate(&mut conn, target).await?;
            let project_id = place.project_id()?;
            if ids.is_empty() {
                let done = sqlx::query("DELETE FROM memory_misses WHERE project_id = ?").bind(&project_id).execute(&mut *conn).await?;
                return Ok(done.rows_affected());
            }
            let mut dropped = 0;
            for id in ids {
                let done = sqlx::query("DELETE FROM memory_misses WHERE project_id = ? AND id = ?")
                    .bind(&project_id)
                    .bind(id.trim())
                    .execute(&mut *conn)
                    .await?;
                if done.rows_affected() == 0 {
                    return Err(MemError::not_found(format!("no miss {} in this project", id.trim())));
                }
                dropped += done.rows_affected();
            }
            Ok(dropped)
        }
        .await;
        store::finish(&mut conn, outcome).await
    }

    /// The project's misses as a fixture: its domains, its live notes and any other note a
    /// miss names (a superseded one is replaced by its successor when both are carried),
    /// each keyed by its id with its links, its domain, its evidence bound to the text as it
    /// reads now and `agent` for an unconfirmed one; then, oldest first, one query per miss
    /// that names a wanted note - the wanted notes expected, the wrong ones avoided, its
    /// paths and style (inferred when none was recorded). Staleness is not carried.
    pub async fn export_misses(&self, target: &Target) -> MemResult<MissExport> {
        let mut conn = self.store().await?.read().await?;
        let place = self.locate(&mut conn, target).await?;
        let project_id = place.project_id()?;
        let project = place.project.clone().ok_or_else(|| MemError::store("the resolved project vanished"))?;
        let mut misses = rows(&mut conn, &project_id).await?;
        misses.reverse();

        let mut chosen: Vec<Memory> = store::memories(&mut conn, &project_id, &Filter::default())
            .await?
            .into_iter()
            .filter(|m| m.status.is_live())
            .collect();
        let mut carried: HashSet<String> = chosen.iter().map(|m| m.id.clone()).collect();
        // Collected first: a closure-holding iterator kept across an await makes the
        // future too narrow for an axum handler.
        let named: Vec<String> = misses.iter().flat_map(|m| m.wanted.iter().chain(&m.wrong)).cloned().collect();
        for id in &named {
            if carried.contains(id) {
                continue;
            }
            if let Some(memory) = store::memory(&mut conn, id).await? {
                if memory.scope == project_id || memory.scope == GLOBAL_PROJECT_ID {
                    carried.insert(memory.id.clone());
                    chosen.push(memory);
                }
            }
        }
        chosen.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then_with(|| a.id.cmp(&b.id)));

        let mut notes = Vec::with_capacity(chosen.len());
        for memory in &chosen {
            notes.push(fixture_note(&mut conn, memory).await?);
        }
        let at: HashMap<String, usize> = notes.iter().enumerate().map(|(i, n)| (n.key.clone(), i)).collect();
        for memory in &chosen {
            if let Some(&successor) = memory.superseded_by.as_ref().and_then(|s| at.get(s)) {
                if notes[successor].replaces.is_none() {
                    notes[successor].replaces = Some(memory.id.clone());
                }
            }
        }
        for link in store::links_in(&mut conn, &project_id).await? {
            if link.kind.is_system() || !carried.contains(&link.to) {
                continue;
            }
            if let Some(&from) = at.get(&link.from) {
                notes[from].links.push(FixtureLink { to: link.to.clone(), kind: link.kind.as_str().to_string(), note: link.note.clone() });
            }
        }

        let mut domains: Vec<FixtureDomain> = store::domains(&mut conn, &project_id)
            .await?
            .into_iter()
            .map(|d| FixtureDomain {
                path: d.path,
                title: d.title,
                summary: Some(d.summary).filter(|s| !s.is_empty()),
                body: Some(d.body).filter(|b| !b.is_empty()),
                owns: d.owns,
                keywords: d.keywords,
            })
            .collect();
        // A parent's path is a prefix of its sub-domains': sorted, parents come first.
        domains.sort_by(|a, b| a.path.cmp(&b.path));

        let mut queries = Vec::new();
        let mut gaps = Vec::new();
        for miss in &misses {
            let expect: Vec<String> = miss.wanted.iter().filter(|id| carried.contains(*id)).cloned().collect();
            if expect.is_empty() {
                gaps.push(miss.clone());
                continue;
            }
            let avoid = miss.wrong.iter().filter(|id| carried.contains(*id) && !expect.contains(id)).cloned().collect();
            queries.push(FixtureQuery {
                q: miss.query.clone(),
                expect,
                style: miss.style.clone().unwrap_or_else(|| infer_style(&miss.query).to_string()),
                paths: miss.paths.clone(),
                avoid,
            });
        }
        let cjk = misses.iter().filter(|m| search::has_cjk(&m.query)).count();
        let fixture = Fixture {
            about: format!(
                "The retrievals recorded as failed in {} ({}), exported {}: each query one that missed, its expected notes \
                 the ones wanted, its avoided ones those that misled.",
                project.name,
                project.id,
                swiss_core::log::iso_now()
            ),
            project: FixtureProject { name: project.name.clone(), language: if cjk * 2 > misses.len() { "zh" } else { "en" }.into() },
            domains,
            notes,
            queries,
        };
        fixture.check().map_err(|why| MemError::store(format!("the exported fixture does not hold: {why}")))?;
        Ok(MissExport { queries: fixture.queries.len(), fixture, gaps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_miss_without_a_style_reads_as_its_shape() {
        for (query, style) in [
            ("crates/swiss-memory/src/store.rs", "path"),
            ("store.rs", "path"),
            ("src\\brief.rs", "path"),
            ("where is brief_markers decided", "symbol"),
            ("why does PayController retry", "symbol"),
            ("Store::open fails", "symbol"),
            ("retries", "short"),
            ("refund gateway", "short"),
            ("退款网关", "short"),
            ("why was the customer billed twice", "natural"),
            ("为什么客户被重复扣款了", "natural"),
            ("e.g. the v1.2 notes", "natural"),
        ] {
            assert_eq!(infer_style(query), style, "{query}");
        }
    }

    #[test]
    fn a_miss_says_what_was_wanted() {
        let id = "m-0123456789".to_string();
        let ok = MissRequest { query: " refund gateway ".into(), wanted: vec![id.clone(), id.clone()], ..MissRequest::default() };
        let miss = checked(&ok).unwrap();
        assert_eq!((miss.query.as_str(), miss.wanted.len()), ("refund gateway", 1), "trimmed, deduplicated");
        assert!(is_miss_id(&miss.id));
        let words = MissRequest { query: "key rotation".into(), want: Some("how the signing key rotates".into()), ..MissRequest::default() };
        assert!(checked(&words).is_ok(), "words stand in when no note holds it");

        let refused = [
            (MissRequest { query: "x".into(), ..MissRequest::default() }, "wanted"),
            (MissRequest { query: " ".into(), wanted: vec![id.clone()], ..MissRequest::default() }, "query"),
            (MissRequest { query: "x".into(), wanted: vec!["p-01234567".into()], ..MissRequest::default() }, "not a note id"),
            (MissRequest { query: "x".into(), wanted: vec![id.clone()], wrong: vec![id.clone()], ..MissRequest::default() }, "both"),
            (MissRequest { query: "x".into(), wanted: vec![id.clone()], via: Some("grep".into()), ..MissRequest::default() }, "via"),
            (MissRequest { query: "x".into(), wanted: vec![id.clone()], style: Some("vague".into()), ..MissRequest::default() }, "style"),
            (MissRequest { query: "x".into(), wanted: vec![id.clone()], paths: vec!["../etc/passwd".into()], ..MissRequest::default() }, "paths"),
            (MissRequest { query: "x".into(), wanted: vec![id.clone()], paths: vec!["src/*.rs".into()], ..MissRequest::default() }, "paths"),
        ];
        for (request, why) in refused {
            let err = checked(&request).unwrap_err();
            assert!(err.message.contains(why), "{why}: {}", err.message);
        }
    }

    #[test]
    fn evidence_travels_in_the_compact_form() {
        use crate::evidence::EvidenceSpec;
        use crate::model::Relation;
        let bound = |spec: &EvidenceSpec| Evidence {
            id: "e-0123456789".into(),
            memory_id: "m-0123456789".into(),
            revision: 1,
            note_hash: String::new(),
            source_type: spec.source_type,
            relation: spec.relation.unwrap_or(Relation::DerivedFrom),
            path: spec.path.clone(),
            symbol: spec.symbol.clone(),
            commit_sha: spec.commit_sha.clone(),
            session_id: spec.session_id.clone(),
            run_id: spec.run_id.clone(),
            source_hash: None,
            excerpt_hash: None,
            actor: "panel".into(),
            created_ms: 0,
        };
        for raw in [
            "verified_by=code:src/pay/retry.rs#retry",
            "derived_from=commit:abcdef1",
            "derived_from=human",
            "supports=test:tests/a.rs#reuses_the_key",
            "derived_from=session:s-1",
        ] {
            let spec = EvidenceSpec::parse(raw).unwrap();
            let text = compact(&bound(&spec)).unwrap();
            assert_eq!(text, raw);
            assert_eq!(EvidenceSpec::parse(&text).unwrap(), spec, "{raw} reads back");
        }
        let mut odd = EvidenceSpec::parse("code:src/a.rs").unwrap();
        odd.path = Some("src/#odd.rs".into());
        assert_eq!(compact(&bound(&odd)), None, "a path holding # has no compact form");
    }
}
