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

//! The memory question (SPEC §memory.rag): one natural-language question answered
//! from a project's notes by the agent its layers name - the same layers a run
//! asks (the ask's own `agent` param, the project's settings, the sync block, the
//! agent added last), and a read, so no nudge rule: the newest agent may answer.
//!
//! A builtin agent works the question itself in a bounded loop over read-only
//! memory tools ([AskTools]) - the notes, the documents the project keeps (SPEC
//! §memory.docs) and the code index - and, when the scope has a checkout, the
//! recheck's read and bash over it, to check what memory says against the code; a
//! spawned runtime (claude, pi) cannot call them, so the gateway retrieves first - a
//! search, a related expansion and the documents' sections - inlines what it found,
//! and asks once. Either way the answer names the note ids and the document paths it
//! rests on.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};
use swiss_llm::agent::{ToolAnswer, ToolSpec, Toolbox};
use swiss_memory::docs::{self, DocKind};
use swiss_memory::related::Seed;
use swiss_memory::search::{SearchQuery, QUERY_MAX};
use swiss_memory::service::{BriefRequest, RelatedRequest, Target};
use swiss_memory::settings::{Choice, ProjectSettings};
use swiss_memory::{render, Code, MemError, MemResult, MemoryService};

use crate::memory_agent::{MemoryTools, LOOK_OFFER};
use crate::memory_api::MemoryState;
use crate::memory_sync::{self, Ask};

/// The read-only memory tools a question may work with, in the stable order the
/// loop offers them; the checkout's read and bash ([LOOK_OFFER]) follow when it has them.
pub const ASK_OFFER: [&str; 8] = ["search", "docs", "doc", "code", "related", "get", "domain", "brief"];

/// The most a question may say, in characters (SPEC §memory.rag); the MCP schema's bound.
pub const QUESTION_MAX: usize = 4000;

/// The most retrieval a one-shot runtime is handed, in bytes: the notes, then the
/// documents' sections, each its own budget.
const RETRIEVAL_CAP: usize = 8 * 1024;
const RETRIEVAL_DOCS_CAP: usize = 8 * 1024;

/// The section appended to a question a builtin agent loops on; `looks` is the share of
/// the checkout's read and bash it took, and with either it checks memory against the code.
fn ask_addendum(looks: &[&str]) -> String {
    let mut p = String::from(
        "## This run\nYou are answering from the project's memory with read-only tools: search \
         (notes by words), docs (the documents the project keeps - designs, plans, pitfalls, \
         references - by words), doc (one document, or one of its sections), code (where a class, \
         method or function is defined), related (the graph out from a note or a passage), get \
         (one note whole), domain (an area's hub and notes), brief (the standing brief). Search the \
         notes and the documents first, then follow what you find: a document's card names the \
         code it is about, and code says where that is now. Say where code lives only as a tool \
         answered it - a definition code lists, a path a card or a note cites; when code lists no \
         definition, name the files it says mention the name, never a file you guess from a class \
         name or a theme. A file that mentions a name is not where that thing is done, and one code \
         marks as naming it only in a comment does not use it at all: say what a file does only from \
         a definition code lists or lines a note or a document shows, else call it a file that \
         mentions the name. A rule a document states in general (\"notifications go through X\") \
         places one case only when code shows that case's file using X outside a comment. A design, \
         plan, proposal or rejected document says what was intended, not \
         what the code does: say which you are reporting.\n",
    );
    if !looks.is_empty() {
        if looks.contains(&"read") {
            p.push_str("read opens one file of the checkout (from a line on, for a long one). ");
        }
        if looks.contains(&"bash") {
            p.push_str(
                "bash runs one read-only git command in it: `git grep -n -w NAME` finds where a name is written, \
                 `git ls-files PATTERN` whether a file is there now, `git log --oneline -3 -- PATH` what last \
                 changed it. Quote a pattern as a shell would; no shell runs, so a pipe or `;` is refused - call \
                 bash again instead. ",
            );
        }
        p.push_str(
            "Memory lags the code: a document's card says the commit it was last checked at, and a note or a \
             document can still describe a plan or an older version. Check each specific you state against the \
             code before you state it - a route and the class that serves it, a file or script path, a count, a \
             retry, a state, a value - and cite a file only once code, the checkout's listing or a read showed it \
             is there now; what a file you read shows counts as a tool's answer. What a step does - whether it \
             retries, what a failure leaves behind - you read in the method that does it, never in a document's \
             table: find the method, then read its file from that line. When the code and a note or a document \
             disagree, the code wins: report what the code does, and name the note or the document that says \
             otherwise as out of date. When memory disagrees with itself - a note says a step never retries, a \
             document says it retries three times - the code decides: read it before you answer, and never state \
             both. Call several tools in one turn when you can: the loop has a turn budget, and the answer takes \
             the last turn.\n",
        );
    }
    p.push_str(
        "Your last message is the answer and it names the note ids (m-…), the document paths and \
         the code paths it rests on - or says plainly that memory holds nothing for the question.",
    );
    p
}

/// The read-only memory tools over one scope (SPEC §memory.rag): every answer the
/// render the CLI gives, so ids ride along for the citing.
pub struct AskTools<'a> {
    service: &'a MemoryService,
    target: Target,
    /// The checkout's read and bash, when the scope has one and the agent took them.
    look: Option<MemoryTools>,
}

impl<'a> AskTools<'a> {
    pub fn new(service: &'a MemoryService, target: &Target) -> AskTools<'a> {
        AskTools { service, target: target.clone(), look: None }
    }

    /// With the checkout's tools too: `taken` is the agent's share of [LOOK_OFFER], read
    /// and bash over `root` in a recheck's smaller pieces. None taken adds nothing.
    pub fn looking(mut self, root: &Path, taken: &[&'static str]) -> AskTools<'a> {
        let taken: Vec<&'static str> = taken.iter().copied().filter(|t| LOOK_OFFER.contains(t)).collect();
        if !taken.is_empty() {
            self.look = Some(MemoryTools::looking(root, &taken));
        }
        self
    }
}

#[async_trait]
impl Toolbox for AskTools<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = vec![
            ToolSpec {
                name: "search".into(),
                description: "Notes by words: an id, a title, a path or terms (semantic when \
                    embedding is on for the project).".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["query"],
                    "properties": {
                        "query": {"type": "string", "description": "what to look for"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 20}
                    }
                }),
            },
            ToolSpec {
                name: "docs".into(),
                description: "The documents the project keeps (designs, plans, pitfalls, references) by \
                    words: the sections that match, each with its document's card and the code it names.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["query"],
                    "properties": {
                        "query": {"type": "string", "description": "what to look for: words, a phrase, a path"},
                        "kind": {"type": "string", "enum": DocKind::ALL.map(DocKind::as_str), "description": "only documents of this kind"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 20}
                    }
                }),
            },
            ToolSpec {
                name: "doc".into(),
                description: "One document: its card and outline, and its text - whole when it is short, \
                    else the section asked for.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["doc"],
                    "properties": {
                        "doc": {"type": "string", "description": "a document id (d-…) or its path"},
                        "section": {"type": "integer", "minimum": 0, "description": "a section number from the outline"}
                    }
                }),
            },
            ToolSpec {
                name: "code".into(),
                description: "Where a name (a class, method, function) is defined in the code, and which \
                    files mention it.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["name"],
                    "properties": {"name": {"type": "string", "description": "the name as the code spells it"}}
                }),
            },
            ToolSpec {
                name: "related".into(),
                description: "The memory graph out from one note or a passage: what depends \
                    on, supports, contradicts or shares terms with it.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "id": {"type": "string", "description": "a note id (m-…); exactly one of id, text"},
                        "text": {"type": "string", "description": "a passage; exactly one of id, text"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 20}
                    }
                }),
            },
            ToolSpec {
                name: "get".into(),
                description: "One note whole: content, anchors, refs, links.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["id"],
                    "properties": {"id": {"type": "string", "description": "a note id (m-…)"}}
                }),
            },
            ToolSpec {
                name: "domain".into(),
                description: "The project map, or one area's hub and notes.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {"path": {"type": "string", "description": "a domain path (app/api); absent lists them all"}}
                }),
            },
            ToolSpec {
                name: "brief".into(),
                description: "The standing brief of the project, or the notes about a task.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {"task": {"type": "string", "description": "what you are about to do"}}
                }),
            },
        ];
        if let Some(look) = &self.look {
            specs.extend(look.specs());
        }
        specs
    }

    async fn call(&mut self, name: &str, args: &Value) -> ToolAnswer {
        // The checkout's tools answer for themselves: pinned to the checkout, credential
        // files refused, what they serve redacted (SPEC §memory.init The recheck).
        if let Some(look) = self.look.as_mut().filter(|_| LOOK_OFFER.contains(&name)) {
            return look.call(name, args).await;
        }
        let text = self.tool(name, args).await;
        let ok = !text.starts_with("error:");
        ToolAnswer { text, ok }
    }
}

impl AskTools<'_> {
    /// One tool run; an error text is the answer, marked not ok by [call].
    async fn tool(&self, name: &str, args: &Value) -> String {
        let limit = |args: &Value| {
            args.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(10)
        };
        match name {
            "search" => {
                let Some(query) = args.get("query").and_then(Value::as_str).map(str::trim) else {
                    return "error: search takes {\"query\": \"…\"}".into();
                };
                if query.is_empty() {
                    return "error: search says nothing to look for".into();
                }
                let request = SearchQuery {
                    text: query.to_string(),
                    limit: limit(args),
                    ..SearchQuery::default()
                };
                match self.service.search(&self.target, &request).await {
                    Ok(hits) => render::hits(&hits),
                    Err(err) => format!("error: {err}"),
                }
            }
            "docs" => {
                let Some(query) = args.get("query").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()) else {
                    return "error: docs takes {\"query\": \"…\"}".into();
                };
                let kind = match args.get("kind").and_then(Value::as_str) {
                    None => None,
                    Some(kind) => match DocKind::parse(kind) {
                        Some(kind) => Some(kind),
                        None => return format!("error: kind is one of {}", DocKind::choices()),
                    },
                };
                let limit = args.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(docs::SEARCH_DEFAULT);
                let query: String = query.chars().take(QUERY_MAX).collect();
                match self.service.docs_search(&self.target, &query, limit, kind).await {
                    Ok(hits) => render::doc_hits(&hits),
                    Err(err) => format!("error: {err}"),
                }
            }
            "doc" => {
                let Some(key) = args.get("doc").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()) else {
                    return "error: doc takes {\"doc\": \"d-… or a path\"}".into();
                };
                let section = args.get("section").and_then(Value::as_u64).map(|n| n.min(u32::MAX as u64) as u32);
                match self.service.doc_view(&self.target, key, section).await {
                    Ok(view) => render::doc_view(&view),
                    Err(err) => format!("error: {err}"),
                }
            }
            "code" => {
                let Some(name) = args.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) else {
                    return "error: code takes {\"name\": \"…\"}".into();
                };
                match self.service.code_find(&self.target, name).await {
                    Ok(hits) => render::code_hits(&hits),
                    Err(err) => format!("error: {err}"),
                }
            }
            "related" => {
                let seed = match (args.get("id").and_then(Value::as_str), args.get("text").and_then(Value::as_str)) {
                    (Some(id), None) if !id.trim().is_empty() => Seed::Id(id.trim().to_string()),
                    (None, Some(text)) if !text.trim().is_empty() => Seed::Text(text.trim().to_string()),
                    _ => return "error: related takes {\"id\": \"m-…\"} or {\"text\": \"…\"}, exactly one".into(),
                };
                let request = RelatedRequest {
                    seed,
                    depth: None,
                    limit: Some(limit(args)),
                    include_history: false,
                };
                match self.service.related(&self.target, &request).await {
                    Ok(outcome) => {
                        let mut body = render::related(&outcome.items);
                        if let Some(state) = outcome.semantic.filter(|s| s.state != "on") {
                            let why = state.reason.as_deref().unwrap_or(state.state);
                            body.push_str(&format!("Similar meaning not matched: {why}.\n"));
                        }
                        body
                    }
                    Err(err) => format!("error: {err}"),
                }
            }
            "get" => {
                let Some(id) = args.get("id").and_then(Value::as_str).map(str::trim) else {
                    return "error: get takes {\"id\": \"m-…\"}".into();
                };
                match self.service.get(&self.target, id, false).await {
                    Ok((record, _)) => render::record(&record, None),
                    Err(err) => format!("error: {err}"),
                }
            }
            "domain" => {
                let path = args.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
                match path {
                    None => match self.service.domains(&self.target).await {
                        Ok(list) => render::map(&list),
                        Err(err) => format!("error: {err}"),
                    },
                    Some(path) => match self.service.domain(&self.target, path, false).await {
                        Ok(detail) => render::domain(&detail),
                        Err(err) => format!("error: {err}"),
                    },
                }
            }
            "brief" => {
                let request = BriefRequest {
                    task: args.get("task").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(String::from),
                    ..BriefRequest::default()
                };
                match self.service.brief(&self.target, &request).await {
                    Ok(out) => out.markdown,
                    Err(err) => format!("error: {err}"),
                }
            }
            other => format!("error: unknown tool {other}; use one of {}", ASK_OFFER.join(", ")),
        }
    }
}

/// What the gateway hands a runtime that cannot call the tools: a search over the
/// question, then a related expansion of it, then the documents' sections that answer
/// it - rendered, the notes and the documents each cut to its own budget.
pub async fn retrieve(service: &MemoryService, target: &Target, question: &str) -> MemResult<String> {
    // The search takes the question's head: a query has its own bound (SPEC §memory.search),
    // a question may say more.
    let query = SearchQuery {
        text: question.chars().take(QUERY_MAX).collect(),
        limit: 8,
        ..SearchQuery::default()
    };
    let hits = service.search(target, &query).await?;
    let related = service
        .related(
            target,
            &RelatedRequest {
                seed: Seed::Text(question.to_string()),
                depth: None,
                limit: Some(8),
                include_history: false,
            },
        )
        .await?;
    let mut out = String::from("## Notes the words found\n");
    out.push_str(&render::hits(&hits));
    out.push_str("\n## Notes the graph found from the question\n");
    out.push_str(&render::related(&related.items));
    let mut out = cut(out, RETRIEVAL_CAP);
    let sections = service.docs_search(target, &query.text, docs::SEARCH_DEFAULT, None).await?;
    if !sections.is_empty() {
        out.push_str("\n\n## Documents the project keeps\n");
        out.push_str(&cut(render::doc_hits(&sections), RETRIEVAL_DOCS_CAP));
    }
    Ok(out)
}

/// `text` at most `cap` bytes, cut on a character boundary - never inside one, which
/// would panic - and said to be cut.
fn cut(mut text: String, cap: usize) -> String {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n… (cut)");
    text
}

/// One question answered (SPEC §memory.rag): the agent is chosen by the same
/// layers a run asks, the first named worker answers, and the reply says which
/// agent and what it rested on.
pub struct Answered {
    pub text: String,
    pub agent: String,
    pub model: Option<String>,
    /// "tools": the agent looped over the memory tools; "inline": the gateway
    /// retrieved and the agent answered once.
    pub mode: &'static str,
}

/// A question as asked (SPEC §memory.rag): something to ask, at most [QUESTION_MAX]
/// characters, trimmed.
pub fn checked_question(question: &str) -> MemResult<&str> {
    let question = question.trim();
    if question.is_empty() {
        return Err(MemError::invalid("question says nothing"));
    }
    if question.chars().count() > QUESTION_MAX {
        return Err(MemError::invalid(format!("question: at most {QUESTION_MAX} characters")));
    }
    Ok(question)
}

/// No agent to answer, or the one asked failed (SPEC §memory.rag): the sentence says why.
fn unavailable(why: String) -> MemError {
    MemError::new(Code::AgentUnavailable, why)
}

/// The agent a question goes to (SPEC §memory.rag): the ask's own pick, the settings
/// of the project the scope selects - a project id names it, a cwd resolves to the
/// project registered over it, the way every read finds it - the sync block, then
/// the newest agent. None anywhere is the no-workers sentence.
pub async fn answerer(
    state: &MemoryState,
    service: &MemoryService,
    target: &Target,
    agent: Option<&str>,
) -> MemResult<String> {
    let pick = match agent.map(str::trim).filter(|a| !a.is_empty()) {
        Some("off") => return Err(MemError::invalid("agent names the one to answer; leave it out for the layers")),
        Some(name) => Choice::parse(Some(&json!(name)), "agent", true).map_err(MemError::invalid)?,
        None => Choice::Inherit,
    };
    // An unregistered directory, or a global scope never written, carries no settings.
    let settings = match service.resolve(target).await? {
        Some(project) => service.project_settings(&project.id).await?,
        None => ProjectSettings::default(),
    };
    let seat = state.sync_seat();
    let chosen = memory_sync::choose(&pick, &Choice::Inherit, false, &settings, seat.config.as_ref(), state.llm());
    chosen.agents.names.first().cloned().ok_or_else(|| unavailable(memory_sync::no_workers(&chosen.agents, false)))
}

/// Resolve the answerer ([answerer]) and ask it; `worktree` names the working tree the
/// code is read in when it is not the cwd's own ([root_of]).
pub async fn answer(
    state: &MemoryState,
    service: &MemoryService,
    target: &Target,
    question: &str,
    agent: Option<&str>,
    worktree: Option<&str>,
) -> MemResult<Answered> {
    let question = checked_question(question)?;
    let name = answerer(state, service, target, agent).await?;
    let ask = memory_sync::runner(state.llm(), &name).map_err(unavailable)?;
    let (text, mode) = asked_by(ask.as_ref(), service, target, question, worktree).await?;
    Ok(Answered { text, agent: ask.label(), model: ask.model(), mode })
}

/// The question over a runner already resolved: a looper works the read-only tools
/// itself, with the checkout's read and bash when the scope has a checkout and the
/// agent takes them; a spawned runtime cannot call them, so the gateway retrieves,
/// inlines what it found and asks once in the checkout the scope names. The runner's
/// failure is [Code::AgentUnavailable]; the store's keeps its own code. A working tree
/// the caller names is refused when the project has none such, and otherwise scopes the
/// memory tools too: it is one of the project's own, and a note on a file only its
/// branch has reads fresh there, not missing from the main checkout.
async fn asked_by(
    ask: &dyn Ask,
    service: &MemoryService,
    target: &Target,
    question: &str,
    worktree: Option<&str>,
) -> MemResult<(String, &'static str)> {
    let named = match worktree.filter(|w| !w.trim().is_empty()) {
        Some(_) => Some(root_of(service, target, worktree).await?),
        None => None,
    };
    let scoped = named.as_ref().map(|root| Target::dir(root.display().to_string()));
    let target = scoped.as_ref().unwrap_or(target);
    if ask.rags() {
        let mut looks = ask.rag_looks();
        let mut tools = AskTools::new(service, target);
        if let Some(root) = &named {
            tools = tools.looking(root, &looks);
        } else if !looks.is_empty() {
            // A scope without a checkout still answers from memory, without the code's tools.
            match root_of(service, target, None).await {
                Ok(root) => tools = tools.looking(&root, &looks),
                Err(err) if err.code == Code::Invalid => looks.clear(),
                Err(err) => return Err(err),
            }
        }
        let prompt = format!("# The question\n{question}\n\n{}", ask_addendum(&looks));
        Ok((ask.rag(&prompt, &mut tools).await.map_err(unavailable)?, "tools"))
    } else {
        let root = match named {
            Some(root) => root,
            None => root_of(service, target, None).await?,
        };
        let found = retrieve(service, target, question).await?;
        let prompt = format!(
            "# The question\n{question}\n\n# What the project's memory holds\n{found}\n\n\
             Answer from the material above. Name the note ids (m-…), the document paths and the code \
             paths your answer rests on, or say plainly that memory holds nothing for the question."
        );
        Ok((ask.ask(&root, &prompt).await.map_err(unavailable)?, "inline"))
    }
}

/// Where the code's tools and a spawned runtime work one question: the working tree
/// `worktree` names - a checkout's absolute path or a branch one has checked out, of
/// the scope's own repository - else the scope's checkout. A cwd resolves to the
/// working tree it is in (SPEC §memory.scope) - a subdirectory to its repository's
/// top, a linked worktree to itself, so a branch's new files are there to read - since
/// notes and cards cite paths from a top and an agent's cwd can be deeper; one with no
/// boundary above it is its own. A project id reads its first root, the one a preview
/// opens. A scope with none has no answer to give it.
async fn root_of(service: &MemoryService, target: &Target, worktree: Option<&str>) -> MemResult<PathBuf> {
    if worktree.is_some_and(|w| !w.trim().is_empty()) {
        let named = service.checkout_for(target, worktree).await?;
        return named.ok_or_else(|| MemError::invalid("this scope has no working tree to read"));
    }
    match target {
        Target::Dir { cwd, .. } => Ok(service.checkout(target).await?.unwrap_or_else(|| PathBuf::from(cwd))),
        Target::Project(id) => {
            let views = service.projects().await?;
            let root = views
                .iter()
                .find(|v| &v.id == id)
                .and_then(|v| v.roots.first())
                .map(|r| PathBuf::from(&r.path));
            root.ok_or_else(|| MemError::invalid("this scope has no directory for a spawned runtime to work in"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use swiss_memory::model::{Kind, LinkKind};
    use swiss_memory::service::SaveRequest;

    /// A scratch home with one repo and a service over it.
    struct Bench {
        home: PathBuf,
        service: Arc<MemoryService>,
    }

    impl Bench {
        fn new(label: &str) -> Bench {
            let home = std::env::temp_dir().join(format!(
                "swiss-memask-{label}-{}-{}",
                std::process::id(),
                swiss_core::util::random_hex(6)
            ));
            std::fs::create_dir_all(home.join("repo").join(".git")).expect("scratch repo");
            let home = home.canonicalize().expect("canonical home");
            Bench { home: home.clone(), service: Arc::new(MemoryService::at(&home.join("store.sqlite3"))) }
        }

        fn target(&self) -> Target {
            Target::dir(self.home.join("repo").display().to_string())
        }

        /// One saved note, its id back.
        async fn save(&self, title: &str, content: &str, keywords: &[&str]) -> String {
            self.save_at(&self.target(), "src/pay.rs", title, content, keywords).await
        }

        /// One note saved from `target`, anchored to `anchor`.
        async fn save_at(&self, target: &Target, anchor: &str, title: &str, content: &str, keywords: &[&str]) -> String {
            let request = SaveRequest {
                kind: Kind::Decision,
                title: title.to_string(),
                content: content.to_string(),
                anchors: vec![anchor.to_string()],
                refs: Vec::new(),
                keywords: keywords.iter().map(|k| k.to_string()).collect(),
                links: Vec::new(),
                replaces: None,
                allow_similar: false,
                domain: None,
                evidence: Vec::new(),
            };
            let out = self
                .service
                .save(target, &swiss_memory::model::Actor::panel(), request)
                .await
                .expect("save lands");
            out.record.summary.id
        }
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    /// A scripted Ask: it records every prompt it was handed and answers from the script.
    /// A looper takes `looks` of the checkout's tools, records the tools it was offered,
    /// and calls `script` in order, keeping each answer.
    struct FakeAsk {
        loops: bool,
        looks: Vec<&'static str>,
        script: Vec<(&'static str, Value)>,
        reply: Result<String, String>,
        prompts: Mutex<Vec<String>>,
        roots: Mutex<Vec<PathBuf>>,
        offered: Mutex<Vec<Vec<String>>>,
        answers: Mutex<Vec<String>>,
    }

    impl FakeAsk {
        fn looper(reply: &str) -> FakeAsk {
            FakeAsk {
                loops: true,
                looks: Vec::new(),
                script: Vec::new(),
                reply: Ok(reply.to_string()),
                prompts: Mutex::new(Vec::new()),
                roots: Mutex::new(Vec::new()),
                offered: Mutex::new(Vec::new()),
                answers: Mutex::new(Vec::new()),
            }
        }

        fn one_shot(reply: &str) -> FakeAsk {
            FakeAsk { loops: false, ..FakeAsk::looper(reply) }
        }
    }

    #[async_trait]
    impl Ask for FakeAsk {
        async fn fill(&self, root: &Path, prompt: &str, _schema: &Value) -> Result<String, String> {
            self.ask(root, prompt).await
        }

        async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
            self.roots.lock().expect("roots").push(root.to_path_buf());
            self.prompts.lock().expect("prompts").push(prompt.to_string());
            self.reply.clone()
        }

        async fn rag(&self, prompt: &str, tools: &mut dyn Toolbox) -> Result<String, String> {
            self.prompts.lock().expect("prompts").push(prompt.to_string());
            self.offered.lock().expect("offered").push(tools.specs().into_iter().map(|s| s.name).collect());
            for (name, args) in &self.script {
                let answer = tools.call(name, args).await.text;
                self.answers.lock().expect("answers").push(answer);
            }
            self.reply.clone()
        }

        fn rags(&self) -> bool {
            self.loops
        }

        fn rag_looks(&self) -> Vec<&'static str> {
            self.looks.clone()
        }

        fn label(&self) -> String {
            "fake:echo".into()
        }

        fn model(&self) -> Option<String> {
            Some("fake-model".into())
        }
    }

    #[tokio::test]
    async fn the_tools_read_what_the_project_holds() {
        let bench = Bench::new("tools");
        let target = bench.target();
        let retry = bench.save("Retries double charges", "Why: no idempotency key.", &["retry"]).await;
        let keyed = bench
            .save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"])
            .await;
        bench
            .service
            .link(
                &target,
                &swiss_memory::model::Actor::panel(),
                &keyed,
                &retry,
                LinkKind::DependsOn,
                None,
            )
            .await
            .expect("link lands");
        let tools = AskTools::new(&bench.service, &target);

        // Search by words, related by the graph, get one whole: each renders ids.
        let hit = tools.tool("search", &json!({ "query": "idempotency", "limit": 5 })).await;
        assert!(hit.contains(&keyed), "search finds it: {hit}");
        let near = tools.tool("related", &json!({ "id": retry })).await;
        assert!(near.contains(&keyed) && near.contains("why: depends_on"), "related walks: {near}");
        let whole = tools.tool("get", &json!({ "id": keyed })).await;
        assert!(whole.contains("Why: retries double otherwise."), "get reads it whole: {whole}");
        let domains = tools.tool("domain", &json!({})).await;
        assert!(!domains.starts_with("error:"), "domain answers: {domains}");
        let brief = tools.tool("brief", &json!({ "task": "touch the pay path" })).await;
        assert!(!brief.starts_with("error:"), "brief answers: {brief}");

        // The rules: related takes exactly one anchor, an empty search says so, an
        // unknown tool is named.
        assert!(tools.tool("related", &json!({ "id": retry, "text": "x" })).await.starts_with("error:"));
        assert!(tools.tool("search", &json!({ "query": "  " })).await.starts_with("error:"));
        assert!(tools.tool("teleport", &json!({})).await.starts_with("error:"));
    }

    #[tokio::test]
    async fn a_looper_works_the_tools_a_one_shot_gets_the_retrieval_inlined() {
        let bench = Bench::new("flow");
        let target = bench.target();
        let keyed = bench
            .save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"])
            .await;

        // The looper: the addendum rides the prompt, the reply is the answer.
        let looper = FakeAsk::looper("Keyed per cart (see the note above).");
        let (text, mode) = asked_by(&looper, &bench.service, &target, "what about retries?", None)
            .await
            .expect("the looper answers");
        assert_eq!(mode, "tools");
        assert_eq!(text, "Keyed per cart (see the note above).");
        let prompt = looper.prompts.lock().expect("prompts")[0].clone();
        assert!(prompt.contains("what about retries?"), "the question rides: {prompt}");
        assert!(prompt.contains("names the note ids"), "the addendum rides: {prompt}");
        assert!(looper.roots.lock().expect("roots").is_empty(), "a looper needs no checkout");

        // The one-shot: the gateway retrieves, inlines what it found, asks once in the
        // checkout the scope names.
        let one_shot = FakeAsk::one_shot("Keyed per cart.");
        let (text, mode) = asked_by(&one_shot, &bench.service, &target, "what about retries?", None)
            .await
            .expect("the one-shot answers");
        assert_eq!(mode, "inline");
        assert_eq!(text, "Keyed per cart.");
        let prompt = one_shot.prompts.lock().expect("prompts")[0].clone();
        assert!(prompt.contains(&keyed), "the retrieval inlines the note: {prompt}");
        assert!(prompt.contains("Notes the words found"), "the sections say what they are: {prompt}");
        let root = one_shot.roots.lock().expect("roots")[0].clone();
        assert_eq!(root.as_path(), bench.home.join("repo").as_path(), "the ask happens in the scope's checkout");

        // A question longer than a search query still retrieves: the search takes its head.
        let long = format!("what about retries? {}", "and the idempotency key ".repeat(40));
        assert!(long.chars().count() > QUERY_MAX);
        let (_, mode) = asked_by(&one_shot, &bench.service, &target, &long, None).await.expect("a long question answers");
        assert_eq!(mode, "inline");
        assert!(one_shot.prompts.lock().expect("prompts")[1].contains(&keyed), "the head found the note");

        // A failing runner surfaces its sentence; a scope without a checkout has none to
        // give a spawned runtime.
        let broken = FakeAsk { reply: Err("the model did not answer".into()), ..FakeAsk::one_shot("") };
        let err = asked_by(&broken, &bench.service, &target, "what?", None).await.expect_err("the failure rides");
        assert_eq!((err.code, err.message.as_str()), (Code::AgentUnavailable, "the model did not answer"));
        let no_root = asked_by(&FakeAsk::one_shot("x"), &bench.service, &Target::project("p-nope"), "what?", None).await;
        let no_root = no_root.expect_err("a project with no root cannot host a spawned runtime");
        assert_eq!(no_root.code, Code::Invalid, "the store's answer keeps its code: {}", no_root.message);
        assert!(no_root.message.contains("no directory"), "{}", no_root.message);
    }

    /// A looper checks memory against the code (SPEC §memory.rag): with a checkout and the
    /// agent's read and bash it is offered both after the memory tools and told the code
    /// wins, and both answer from the checkout - pinned inside it, credential files refused,
    /// secrets redacted, git only. Narrowed, it gets its share; no checkout or none taken, the
    /// memory tools alone and no such rule.
    #[tokio::test]
    async fn a_looper_checks_memory_against_the_code_with_the_checkouts_read_and_bash() {
        let bench = Bench::new("look");
        let target = bench.target();
        let keyed = bench
            .save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"])
            .await;
        let repo = bench.home.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("src/pay.rs"), "fn charge() {}\nconst DB: &str = \"jdbc:mysql://root:s3cr3t@db:3306/app\";\n")
            .expect("code");
        std::fs::write(repo.join(".env"), "PAY_KEY=x\n").expect("env");
        std::fs::write(bench.home.join("outside.txt"), "not the checkout's\n").expect("outside");

        let looker = FakeAsk {
            looks: vec!["read", "bash"],
            script: vec![
                ("read", json!({ "path": "src/pay.rs" })),
                ("read", json!({ "path": ".env" })),
                ("read", json!({ "path": "../outside.txt" })),
                ("bash", json!({ "command": "rm -rf src" })),
                ("bash", json!({ "command": "git push" })),
                ("search", json!({ "query": "idempotency" })),
            ],
            ..FakeAsk::looper("Charged once: src/pay.rs.")
        };
        let (text, mode) = asked_by(&looker, &bench.service, &target, "how are charges retried?", None).await.expect("answers");
        assert_eq!((text.as_str(), mode), ("Charged once: src/pay.rs.", "tools"));
        let mut both: Vec<&str> = ASK_OFFER.to_vec();
        both.extend(LOOK_OFFER);
        assert_eq!(looker.offered.lock().expect("offered")[0], both, "the checkout's tools follow memory's");
        let prompt = looker.prompts.lock().expect("prompts")[0].clone();
        assert!(prompt.contains("the code wins") && prompt.contains("`git ls-files PATTERN`"), "{prompt}");
        // Memory against itself (a note says never, a document says three times) goes to the code too.
        assert!(prompt.contains("the code decides") && prompt.contains("never state both"), "{prompt}");
        let answers = looker.answers.lock().expect("answers").clone();
        assert!(answers[0].contains("fn charge()") && !answers[0].contains("s3cr3t"), "read redacts: {}", answers[0]);
        assert!(answers[1].starts_with("error:") && answers[1].contains("credential"), "{}", answers[1]);
        assert!(answers[2].starts_with("error:") && answers[2].contains("outside the root"), "{}", answers[2]);
        assert!(answers[3].starts_with("error: only git"), "{}", answers[3]);
        assert!(answers[4].starts_with("error:") && answers[4].contains("allowlist"), "{}", answers[4]);
        assert!(answers[5].contains(&keyed), "the memory tools still answer: {}", answers[5]);

        // Narrowed to read, the agent gets read alone and the prompt names only it.
        let reader = FakeAsk { looks: vec!["read"], ..FakeAsk::looper("x") };
        asked_by(&reader, &bench.service, &target, "how are charges retried?", None).await.expect("answers");
        let offered = reader.offered.lock().expect("offered")[0].clone();
        assert_eq!(offered.last().map(String::as_str), Some("read"));
        assert!(!offered.iter().any(|t| t == "bash"), "{offered:?}");
        let prompt = reader.prompts.lock().expect("prompts")[0].clone();
        assert!(prompt.contains("read opens one file") && !prompt.contains("bash runs"), "{prompt}");

        // None taken, or a scope with no checkout: the memory tools alone, no code rule.
        let blind = FakeAsk::looper("x");
        asked_by(&blind, &bench.service, &target, "how are charges retried?", None).await.expect("answers");
        let rootless = FakeAsk { looks: vec!["read", "bash"], ..FakeAsk::looper("x") };
        asked_by(&rootless, &bench.service, &Target::project("p-nope"), "how are charges retried?", None)
            .await
            .expect("a scope with no checkout still answers from memory");
        for ask in [&blind, &rootless] {
            assert_eq!(ask.offered.lock().expect("offered")[0], ASK_OFFER);
            assert!(!ask.prompts.lock().expect("prompts")[0].contains("the code wins"));
        }
    }

    /// A question asked from below a working tree's top reads that tree (SPEC
    /// §memory.scope, §memory.rag): a package directory asks at the repository's top, and
    /// a linked worktree an agent works its branch in asks in the worktree - its branch's
    /// files are there, not in the main checkout - never in the deeper cwd, since notes
    /// cite paths from a top.
    #[tokio::test]
    async fn a_subdirectory_asks_at_its_top_and_a_worktree_in_itself() {
        let bench = Bench::new("deeper");
        bench.save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"]).await;
        let (repo, tree) = with_worktree(&bench);

        for (cwd, top, said) in [
            (repo.join("proxy").join("internal"), &repo, "func onMain()"),
            (tree.join("proxy"), &tree, "func onBranch()"),
        ] {
            let target = Target::dir(cwd.display().to_string());
            let looker = FakeAsk {
                looks: vec!["read"],
                script: vec![("read", json!({ "path": "proxy/main.go" }))],
                ..FakeAsk::looper("x")
            };
            asked_by(&looker, &bench.service, &target, "where are charges?", None).await.expect("answers");
            let answers = looker.answers.lock().expect("answers").clone();
            assert!(answers[0].contains(said), "{}: read opens its working tree's top: {}", cwd.display(), answers[0]);

            let one_shot = FakeAsk::one_shot("x");
            asked_by(&one_shot, &bench.service, &target, "where are charges?", None).await.expect("answers");
            assert_eq!(&one_shot.roots.lock().expect("roots")[0], top, "{}: the runtime works at the top", cwd.display());
        }
    }

    /// A question names the working tree its code is read in (SPEC §memory.rag): a branch
    /// a worktree of the scope's repository has checked out, or that worktree's absolute
    /// path, reads there from any cwd of the project or by its id; a branch no tree has,
    /// or a path outside the project, is refused - even for an agent that takes no code
    /// tools - never read in the main checkout instead.
    #[tokio::test]
    async fn a_named_worktree_or_branch_is_where_the_code_is_read() {
        let bench = Bench::new("named");
        bench.save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"]).await;
        let (repo, tree) = with_worktree(&bench);
        let project = bench.service.resolve(&bench.target()).await.expect("resolve").expect("registered").id;
        let tree_path = tree.join("proxy").display().to_string();
        std::fs::write(tree.join("proxy/notify.go"), "func notify() {}\n").expect("branch-only code");
        let on_branch = Target::dir(tree.display().to_string());
        let notify = bench.save_at(&on_branch, "proxy/notify.go", "Notify sends from the branch", "Why: the branch adds it.", &[]).await;

        for (target, worktree) in [
            (bench.target(), "feature/notify"),
            (bench.target(), "refs/heads/feature/notify"),
            (Target::project(project.clone()), "feature/notify"),
            (bench.target(), tree_path.as_str()),
        ] {
            let looker = FakeAsk {
                looks: vec!["read"],
                script: vec![("read", json!({ "path": "proxy/main.go" })), ("get", json!({ "id": notify }))],
                ..FakeAsk::looper("x")
            };
            asked_by(&looker, &bench.service, &target, "where are charges?", Some(worktree)).await.expect("answers");
            let answers = looker.answers.lock().expect("answers").clone();
            assert!(answers[0].contains("func onBranch()"), "{worktree}: read opens the worktree: {}", answers[0]);
            // The memory tools judge freshness in the named tree too: the branch's file is there.
            assert!(answers[1].contains("proxy/notify.go · ok"), "{worktree}: get judges in the worktree: {}", answers[1]);
            let one_shot = FakeAsk::one_shot("x");
            asked_by(&one_shot, &bench.service, &target, "where are charges?", Some(worktree)).await.expect("answers");
            assert_eq!(one_shot.roots.lock().expect("roots")[0], tree, "{worktree}: the runtime works in the worktree");
        }
        // Unnamed, the main checkout judges: the branch's file is not there.
        let unnamed = FakeAsk { script: vec![("get", json!({ "id": notify }))], ..FakeAsk::looper("x") };
        asked_by(&unnamed, &bench.service, &bench.target(), "where?", None).await.expect("answers");
        let answers = unnamed.answers.lock().expect("answers").clone();
        assert!(answers[0].contains("proxy/notify.go · missing"), "{}", answers[0]);
        // The main checkout's branch is the main checkout.
        let one_shot = FakeAsk::one_shot("x");
        asked_by(&one_shot, &bench.service, &bench.target(), "where?", Some("master")).await.expect("answers");
        assert_eq!(one_shot.roots.lock().expect("roots")[0], repo);

        let other = bench.home.join("other");
        std::fs::create_dir_all(other.join(".git")).expect("another repository");
        let other = other.display().to_string();
        for (worktree, says) in [("feature/gone", "no working tree"), (other.as_str(), "not a checkout of this project")] {
            for ask in [FakeAsk::looper("x"), FakeAsk::one_shot("x")] {
                let err = asked_by(&ask, &bench.service, &bench.target(), "where?", Some(worktree)).await;
                let err = err.expect_err("a tree the project does not have is refused");
                assert_eq!(err.code, Code::Invalid, "{}", err.message);
                assert!(err.message.contains(says), "{worktree}: {}", err.message);
            }
        }
    }

    /// The bench's repo with a package directory and a linked worktree on
    /// `feature/notify` (the main checkout on `master`), each with its own `proxy/main.go`.
    fn with_worktree(bench: &Bench) -> (PathBuf, PathBuf) {
        let repo = bench.home.join("repo");
        std::fs::create_dir_all(repo.join("proxy").join("internal")).expect("package dir");
        std::fs::write(repo.join("proxy/main.go"), "func onMain() {}\n").expect("code");
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/master\n").expect("main HEAD");
        let admin = repo.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&admin).expect("admin dir");
        std::fs::write(admin.join("commondir"), "../..\n").expect("commondir");
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/feature/notify\n").expect("worktree HEAD");
        let tree = repo.join(".agents").join("worktrees").join("wt");
        std::fs::create_dir_all(tree.join("proxy")).expect("worktree");
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).expect("worktree .git");
        std::fs::write(admin.join("gitdir"), format!("{}\n", tree.join(".git").display())).expect("admin gitdir");
        std::fs::write(tree.join("proxy/main.go"), "func onBranch() {}\n").expect("branch code");
        (repo, tree)
    }

    #[tokio::test]
    async fn the_docs_tools_read_the_documents_the_project_keeps() {
        let bench = Bench::new("docs");
        let target = bench.target();
        bench.save("Charges carry an idempotency key", "Why: retries double otherwise.", &["idempotency"]).await;
        let project = bench.service.resolve(&target).await.expect("resolve").expect("registered").id;
        let skill = bench.home.join("repo/.claude/skills/logic/order");
        std::fs::create_dir_all(&skill).expect("skill dir");
        std::fs::write(
            skill.join("stats-job.md"),
            "# 订单统计定时任务\n## 各任务详解\n### OrderSweepCheckJob\n长期未结算的订单配置自动关闭，阈值 stale_days_limit（缺省 30 天）。\n",
        )
        .expect("doc");
        bench.service.patch_project_settings(&project, &json!({ "docs": [".claude/skills/logic"] })).await.expect("sources");
        bench.service.docs_read(&project, false).await.expect("read");
        let tools = AskTools::new(&bench.service, &target);
        assert_eq!(tools.specs().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ASK_OFFER);

        // docs finds the section, doc reads it, both open with the trust line.
        let found = tools.tool("docs", &json!({ "query": "30 天没有订单被暂停" })).await;
        assert!(found.contains(".claude/skills/logic/order/stats-job.md:3 › 订单统计定时任务 › 各任务详解 › OrderSweepCheckJob"), "{found}");
        assert!(found.starts_with(render::DOCS_TRUST) && found.contains("no card yet"), "{found}");
        let read = tools.tool("doc", &json!({ "doc": "order/stats-job.md", "section": 2 })).await;
        assert!(read.contains("## Section 2: 订单统计定时任务 › 各任务详解 › OrderSweepCheckJob"), "{read}");
        assert!(read.contains("stale_days_limit"), "{read}");
        assert!(tools.tool("docs", &json!({ "query": "订单", "kind": "roadmap" })).await.starts_with("error: kind is one of"));
        assert!(tools.tool("doc", &json!({ "doc": "nowhere.md" })).await.starts_with("error:"));
        assert!(tools.tool("code", &json!({})).await.starts_with("error:"));

        // A one-shot gets the documents inlined beside the notes.
        let one_shot = FakeAsk::one_shot("OrderSweepCheckJob.");
        asked_by(&one_shot, &bench.service, &target, "找到 30 天没有订单，被暂停订单的相关代码", None).await.expect("answers");
        let prompt = one_shot.prompts.lock().expect("prompts")[0].clone();
        assert!(prompt.contains("## Documents the project keeps") && prompt.contains("stats-job.md"), "{prompt}");
    }

    /// The retrieval's cut lands on a character boundary: notes written in Chinese put three
    /// bytes in a character, and a cut inside one panics - with the release profile's
    /// abort, the whole gateway.
    #[test]
    fn the_retrieval_cut_never_splits_a_character() {
        let text = "## Notes the words found\n".to_string() + &"重试会重复扣款，需要幂等键。".repeat(400);
        assert!(!text.is_char_boundary(RETRIEVAL_CAP), "the fixture puts the cap inside a character");
        let out = cut(text.clone(), RETRIEVAL_CAP);
        let kept = out.strip_suffix("\n… (cut)").expect("the cut is said");
        assert!(kept.len() <= RETRIEVAL_CAP && RETRIEVAL_CAP - kept.len() < 4, "{}", kept.len());
        assert!(text.starts_with(kept));
        assert_eq!(cut("short".into(), RETRIEVAL_CAP), "short", "under the cap, untouched");
    }

    /// The answerer comes from the layers (SPEC §memory.rag): the ask's own agent, the
    /// settings of the project the scope selects - by its id or by a cwd inside it alike -
    /// the sync block, then the agent added last; none anywhere says why.
    #[tokio::test]
    async fn the_answerer_comes_from_the_layers_by_id_and_by_cwd_alike() {
        let bench = Bench::new("layers");
        let target = bench.target();
        bench.save("Charges carry an idempotency key", "Why: retries double otherwise.", &[]).await;
        let project = bench.service.resolve(&target).await.expect("resolve").expect("registered").id;
        let by_id = Target::project(project.clone());
        let llm = swiss_llm::LlmState::new();
        let state = MemoryState::with_llm(llm.clone());

        let none = answerer(&state, &bench.service, &target, None).await.expect_err("no llm agent");
        assert_eq!(none.code, Code::AgentUnavailable);
        assert!(none.message.contains("no llm agent to ask"), "{}", none.message);

        llm.apply(
            swiss_llm::config::LlmConfig::parse(&json!({
                "providers": { "local": { "style": "openai", "baseUrl": "http://127.0.0.1:9/v1", "models": [{ "id": "tiny" }] } },
                "agents": { "a": { "provider": "local" }, "b": { "provider": "local" } }
            }))
            .expect("row"),
        );
        let newest = answerer(&state, &bench.service, &target, None).await.expect("newest");
        assert_eq!(newest, "b", "named nowhere, the agent added last answers");
        bench.service.patch_project_settings(&project, &json!({ "agents": ["a"] })).await.expect("settings");
        for scope in [&target, &by_id] {
            let named = answerer(&state, &bench.service, scope, None).await.expect("the project's");
            assert_eq!(named, "a", "the project's settings, by id or by cwd: {scope:?}");
            let own = answerer(&state, &bench.service, scope, Some(" b ")).await.expect("the ask's own");
            assert_eq!(own, "b", "the ask's own pick outranks them: {scope:?}");
        }
        bench.service.patch_project_settings(&project, &json!({ "agents": "off" })).await.expect("settings");
        let off = answerer(&state, &bench.service, &target, None).await.expect_err("off");
        assert_eq!(off.code, Code::AgentUnavailable);
        assert!(off.message.contains("the project's settings turn its agents off"), "{}", off.message);

        for bad in ["Not A Name", "a,b", "off"] {
            let refused = answerer(&state, &bench.service, &target, Some(bad)).await.expect_err("refused");
            assert_eq!(refused.code, Code::Invalid, "{bad}: {}", refused.message);
        }
        let missing = answerer(&state, &bench.service, &Target::project("p-00000000"), None).await.expect_err("no project");
        assert_eq!(missing.code, Code::NotFound);
        assert_eq!(checked_question("  ").expect_err("empty").code, Code::Invalid);
        assert!(checked_question(&"x".repeat(QUESTION_MAX + 1)).is_err(), "too long");
        assert_eq!(checked_question(" why? ").expect("trimmed"), "why?");
    }
}
