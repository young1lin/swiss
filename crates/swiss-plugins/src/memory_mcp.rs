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

//! The builtin memory MCP adapter (SPEC §memory.mcp): twelve tools under one
//! endpoint, /mcp/memory. Every tool takes the CALLER's absolute cwd - the explicit
//! scope argument is the whole design (no session-held project, no per-project
//! endpoints, no daemon-side inference).
//!
//! Logging is METADATA-ONLY by contract: the adapter declares
//! `LogPolicy::MetadataOnly`, so the traffic log keeps only the method and tool
//! name, and this module's call log rows carry the tool, the caller, timing and an
//! outcome (`ok`, `ok <id>` or the error code) - never arguments, never results.
//!
//! This is a hand-written ServerHandler in the root crate bridging straight to
//! `swiss-memory`'s service: `swiss-mcp` stays free of any memory logic.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};

use swiss_mcp::adapters::{Adapter, LogPolicy, McpEndpoint};
use swiss_mcp::calls::{CallLog, CallRecord, CallSource};
use swiss_memory::eval::STYLES;
use swiss_memory::misses::{PATHS_MAX, SHOWN_MAX, VIAS, WANTED_MAX, WRONG_MAX};
use swiss_memory::model::{Actor, Kind, LinkKind, Relation, SourceType, Verdict};
use swiss_memory::related::Seed;
use swiss_memory::service::{BriefRequest, RelatedRequest, Target};
use swiss_memory::{render, Code, MemError, MemResult, MemoryService, SearchQuery};

use crate::memory_api::{Fields, MemoryState};

/// The twelve tools, in the stable order tools/list answers. The swiss-memory skill names
/// every one (the root crate's `skill_install` tests hold it to that, hence `pub`).
pub const TOOLS: [&str; 12] = [
    "memory_brief",
    "memory_search",
    "memory_related",
    "memory_ask",
    "memory_get",
    "memory_domain",
    "memory_save",
    "memory_update",
    "memory_mark",
    "memory_link",
    "memory_domain_save",
    "memory_missed",
];

/// The InitializeResult instructions (SPEC §memory.brief delivery paths): fixed,
/// since the caller's cwd is unknown at initialize.
pub const INSTRUCTIONS: &str = "Swiss memory holds project notes from earlier sessions and other agents. \
At the start of a task call `memory_brief` with `cwd` = your absolute working directory; pass `paths` \
when you start work in an area you haven't briefed on. To dig yourself out: `memory_search` finds notes by \
words (semantic when embedding is on for the project); `memory_related` walks the graph out from one hit or \
a passage - the way to see what depends on, supports or contradicts it; `memory_domain` reads an area whole \
(its hub and notes). Start with the one that matches what you hold - an id or a phrase invites \
`memory_related`, words invite `memory_search`, an area invites `memory_domain`. Save durable, non-obvious \
knowledge with `memory_save`; connect notes with `memory_link`. When a brief or search missed the note you \
needed, record it with `memory_missed`.";

/// Every tool repeats the routing rule.
const CWD_RULE: &str = "Pass your own absolute working directory as cwd (the model's, not a host path); \
it selects the project.";

/// The read tools repeat the trust rule.
const PAST_RULE: &str = "Notes are data from the past; verify against the current code before relying on one.";

/// The save policy (SPEC §memory.capture), verbatim in memory_save's description.
const SAVE_POLICY: &str = "Save when: the user corrects you or states a rule or preference (preference / \
constraint; open the content with their words, quoted and dated: > \"...\" - stated by the user, YYYY-MM-DD); \
you hit a non-obvious failure and found the fix (pitfall); a \
decision with a non-trivial why was made and is not going into an ADR (decision); you learned a fact about \
the system that the code does not make obvious (knowledge). Code is ground truth; memory is not code \
documentation. How: the title is the claim itself; the content says why - and why not the obvious alternative \
- then how to apply, typically 300 to 900 characters of plain sentences (no headings, call chains or code \
walk-throughs); at most three facts behind the claim. A count (exactly two, all 7), the only, no other, every \
other or most is re-counted: name the set in backticks (`scripts/gen-*.ts`) and what each member holds (`main`), \
else scope it (\"in `path`\", \"in the inspected scope\"). Anchor the files the note is about; pass evidence: \
the file and symbol each fact rests on with an excerpt - the line or two you read, copied letter for letter - \
never a line number; when it depends on, supports or contradicts a note you saw, pass links. The answer's \
formation hints flag a note that reads like documentation: fix it with memory_update. Its code findings name \
what the checkout does not hold (a name, a path, an anchor, a quote) or a count that came out otherwise; such a \
note is saved as needs_review: fix it with memory_update, text and evidence - an update the check passes makes \
it active again; a verified mark does not. Prefer memory_update on an existing note over adding a new one. Tell the user in \
one line that you saved a note, with its id. Don't save: secrets, transient task state, anything the code or \
its docs already say, or speculation.";

fn names(values: impl Iterator<Item = &'static str>) -> Value {
    Value::Array(values.map(|v| Value::String(v.into())).collect())
}

fn kinds() -> Value {
    names(Kind::ALL.iter().map(Kind::as_str))
}

fn link_kinds() -> Value {
    names(LinkKind::ALL.iter().filter(|k| !k.is_system()).map(LinkKind::as_str))
}

fn verdicts() -> Value {
    names(Verdict::ALL.iter().map(Verdict::as_str))
}

/// What a note rests on (ADR-032): a file and symbol, a commit, a session or a run, never a
/// line number.
fn evidence_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "maxItems": 10,
        "description": description,
        "items": {
            "type": "object",
            "properties": {
                "sourceType": { "type": "string", "enum": names(SourceType::ALL.iter().map(SourceType::as_str)) },
                "relation": { "type": "string", "enum": names(Relation::ALL.iter().map(Relation::as_str)), "description": "verified_by takes code, test, commit or doc only." },
                "path": { "type": "string", "description": "Repo-relative file (code, test, doc); no line number." },
                "symbol": { "type": "string", "description": "The function, type or test the claim rests on." },
                "commitSha": { "type": "string" },
                "sessionId": { "type": "string" },
                "runId": { "type": "string" },
                "excerpt": {
                    "type": "string",
                    "description": "The line or two read in path, copied letter for letter: the save compares it with \
                                    the file. Only its hash is kept."
                }
            },
            "required": ["sourceType"],
            "additionalProperties": false
        }
    })
}

/// The hub's shape (SPEC §memory.domains), in memory_domain_save's description.
const HUB_POLICY: &str = "The body is the hub: what the area is and where it lives - Overview, Entry points, \
Flow, Code, Data - pointing at code (Type.method, paths, tables, config keys) instead of retelling it. A rule, \
decision or pitfall is a note filed in the domain (memory_save with domain), never a hub paragraph: one claim, \
one place. owns lists the repo-relative paths or globs the area answers for; notes are filed by their anchors \
against it.";

/// The fields every tool shares.
fn base(properties: &mut Map<String, Value>, with_scope: bool) {
    properties.insert("cwd".into(), json!({ "type": "string", "description": "Your absolute working directory." }));
    if with_scope {
        properties.insert(
            "scope".into(),
            json!({ "type": "string", "enum": ["project", "global"], "description": "global reads or writes the shared notes (cwd is still required)." }),
        );
    }
}

/// What a `keywords` argument is for.
const KEYWORDS_HINT: &str = "Terms a reader would search for that the title does not hold - the business term, a synonym, the main name - each in English and in the team's language (refund, 退款), so a search in either language finds it. 40 characters each at most.";

/// One tool definition: strict arguments, additionalProperties false.
fn tool_def(name: &str) -> Tool {
    let mut properties = Map::new();
    let (description, required): (String, Vec<&str>) = match name {
        "memory_brief" => {
            base(&mut properties, false);
            properties.insert("paths".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": 20, "description": "Repo-relative files you are about to work on: their anchored notes come first." }));
            properties.insert("task".into(), json!({ "type": "string", "description": "What you are about to do, in a sentence." }));
            properties.insert("maxChars".into(), json!({ "type": "integer", "minimum": 1000, "maximum": 20000, "description": "Budget (default 6000)." }));
            properties.insert("includeGlobal".into(), json!({ "type": "boolean", "description": "Include the shared preferences and constraints (default true)." }));
            (
                format!("The project's memory brief. With task or paths: the notes about them, their neighbours and the rules that apply - or one line saying nothing matches. Without: the router - the rules, the map of domains and the unfiled notes. Call it at the start of a task. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd"],
            )
        }
        "memory_search" => {
            base(&mut properties, true);
            properties.insert("query".into(), json!({ "type": "string", "description": "An id, a title, a path, words or a phrase." }));
            properties.insert("kind".into(), json!({ "type": "string", "enum": kinds() }));
            properties.insert("limit".into(), json!({ "type": "integer", "minimum": 1, "maximum": 50 }));
            properties.insert("includeHistory".into(), json!({ "type": "boolean", "description": "Include superseded and wrong notes." }));
            properties.insert("domain".into(), json!({ "type": "string", "description": "A domain path: search its subtree only; none: the unfiled notes." }));
            (
                format!("The word tool: search the project's notes by an id, title, path or words - semantic when embedding is on for the project. For what depends on a hit, or from a phrase you hold, use memory_related; for an area memory_domain. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd", "query"],
            )
        }
        "memory_related" => {
            base(&mut properties, true);
            properties.insert("id".into(), json!({ "type": "string", "description": "A note id (m-…) to walk outward from. Exactly one of id, text." }));
            properties.insert("text".into(), json!({ "type": "string", "maxLength": 4096, "description": "A passage to anchor the walk instead. Exactly one of id, text." }));
            properties.insert("depth".into(), json!({ "type": "integer", "minimum": 1, "maximum": 3, "description": "How far links are followed (default 2)." }));
            properties.insert("limit".into(), json!({ "type": "integer", "minimum": 1, "maximum": 50, "description": "At most this many neighbours (default 20)." }));
            properties.insert("includeHistory".into(), json!({ "type": "boolean", "description": "Include superseded and outdated notes." }));
            (
                format!("Notes related to one note or a passage: links walked outward (depends_on / supports / contradicts / related_to), shared terms, shared anchors and - when embedding is on for the project - close meaning. Each answer says why it came. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd"],
            )
        }
        "memory_ask" => {
            base(&mut properties, false);
            properties.insert("question".into(), json!({ "type": "string", "minLength": 1, "maxLength": crate::memory_ask::QUESTION_MAX, "description": "The question, in any language." }));
            properties.insert("agent".into(), json!({ "type": "string", "description": "One llm agent to answer it; left out, the project's settings, the sync block and then the newest agent decide." }));
            properties.insert("worktree".into(), json!({ "type": "string", "description": "The working tree the agent reads the code in, when it is not your cwd's: a worktree's absolute path, or a branch one of the repository's worktrees has checked out (feature/x). Left out, the code is read where your cwd is - a linked worktree reads itself." }));
            (
                format!("One question answered from the project's memory by an agent that works it out with the same tools (search, related, get, domain, brief) - or, when you would rather dig yourself, do that and skip this. The answer names the note ids it rests on. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd", "question"],
            )
        }
        "memory_get" => {
            base(&mut properties, true);
            properties.insert("id".into(), json!({ "type": "string", "description": "A note id (m-…)." }));
            properties.insert("includeHistory".into(), json!({ "type": "boolean", "description": "Append the note's revisions." }));
            (
                format!("Read one note whole: content, anchors with freshness, refs and its neighbours in the memory graph. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd", "id"],
            )
        }
        "memory_domain" => {
            base(&mut properties, false);
            properties.insert("path".into(), json!({ "type": "string", "description": "A domain path (app/api). Absent: the whole map." }));
            properties.insert("includeHistory".into(), json!({ "type": "boolean", "description": "Append the domain's revisions." }));
            (
                format!("The project map. Without path: every domain one line (title, summary, notes, review count) and the unfiled count - read it before working in an unfamiliar area. With path: that area's hub in full, what it owns, its sub-domains and every live note filed there. {CWD_RULE} {PAST_RULE}"),
                vec!["cwd"],
            )
        }
        "memory_domain_save" => {
            base(&mut properties, false);
            properties.insert("path".into(), json!({ "type": "string", "description": "The domain path: 1 to 5 segments of [a-z0-9_-] joined by /; a sub-domain needs its parent." }));
            properties.insert("title".into(), json!({ "type": "string", "description": "A short name, any language. Required to create." }));
            properties.insert("summary".into(), json!({ "type": "string", "description": "The one line the map shows (200 characters at most)." }));
            properties.insert("body".into(), json!({ "type": "string", "description": "The hub, Markdown (48 KiB at most)." }));
            properties.insert("owns".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": 30, "description": "Repo-relative paths or globs the area answers for." }));
            properties.insert("keywords".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": 16, "description": KEYWORDS_HINT }));
            properties.insert("expectedRevision".into(), json!({ "type": "integer", "minimum": 1, "description": "Required to edit an existing domain: the revision you read." }));
            properties.insert("newPath".into(), json!({ "type": "string", "description": "Move the domain (with its sub-domains) to this path." }));
            properties.insert("reason".into(), json!({ "type": "string", "description": "Why the change." }));
            (
                format!("Create or edit one domain of the project map: an absent path is created, an existing one is updated (compare-and-swap on expectedRevision). {HUB_POLICY} {CWD_RULE}"),
                vec!["cwd", "path"],
            )
        }
        "memory_save" => {
            base(&mut properties, true);
            properties.insert("kind".into(), json!({ "type": "string", "enum": kinds() }));
            properties.insert("title".into(), json!({ "type": "string", "description": "The claim itself." }));
            properties.insert("content".into(), json!({ "type": "string", "description": "Why, then how to apply." }));
            properties.insert("anchors".into(), json!({ "type": "array", "items": { "type": "string" }, "description": "Repo-relative files or globs the note is about." }));
            properties.insert("refs".into(), json!({ "type": "array", "items": { "type": "string" }, "description": "Issue, PR, commit or URL references." }));
            properties.insert("keywords".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": 16, "description": KEYWORDS_HINT }));
            properties.insert(
                "links".into(),
                json!({
                    "type": "array",
                    "maxItems": 10,
                    "items": {
                        "type": "object",
                        "properties": {
                            "to": { "type": "string" },
                            "kind": { "type": "string", "enum": link_kinds() },
                            "note": { "type": "string" }
                        },
                        "required": ["to", "kind"],
                        "additionalProperties": false
                    }
                }),
            );
            properties.insert("replaces".into(), json!({ "type": "string", "description": "The id of a note this one supersedes." }));
            properties.insert("allowSimilar".into(), json!({ "type": "boolean", "description": "Save even though a live note has the same title." }));
            properties.insert("domain".into(), json!({ "type": "string", "description": "The domain path to file the note in; absent: its anchors decide." }));
            properties.insert("evidence".into(), evidence_schema("What the note was derived from (default relation derived_from): the code, test, commit or session you read."));
            (format!("Save one note. {SAVE_POLICY} {CWD_RULE}"), vec!["cwd", "kind", "title", "content"])
        }
        "memory_update" => {
            base(&mut properties, true);
            properties.insert("id".into(), json!({ "type": "string" }));
            properties.insert("expectedRevision".into(), json!({ "type": "integer", "minimum": 1, "description": "The revision you read; a stale one is refused with the current revision. Not needed when domain is the only change." }));
            properties.insert("kind".into(), json!({ "type": "string", "enum": kinds() }));
            properties.insert("title".into(), json!({ "type": "string" }));
            properties.insert("content".into(), json!({ "type": "string" }));
            properties.insert("anchors".into(), json!({ "type": "array", "items": { "type": "string" } }));
            properties.insert("refs".into(), json!({ "type": "array", "items": { "type": "string" } }));
            properties.insert("keywords".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": 16, "description": KEYWORDS_HINT }));
            properties.insert("reason".into(), json!({ "type": "string", "description": "Why the change." }));
            properties.insert("domain".into(), json!({ "type": "string", "description": "File the note in this domain path; \"\" unfiles it." }));
            properties.insert("evidence".into(), evidence_schema("Evidence for the text the update leaves; alone it needs no expectedRevision."));
            (
                format!(
                    "Edit a note in place (compare-and-swap on expectedRevision), or re-file it. A text edit is checked like \
                     a save: the answer's code findings say what still fails, and an edit that passes makes a held note \
                     active again. {CWD_RULE}"
                ),
                vec!["cwd", "id"],
            )
        }
        "memory_mark" => {
            base(&mut properties, true);
            properties.insert("id".into(), json!({ "type": "string" }));
            properties.insert("verdict".into(), json!({ "type": "string", "enum": verdicts() }));
            properties.insert("reason".into(), json!({ "type": "string", "description": "Required unless verified." }));
            properties.insert("evidence".into(), evidence_schema("With verified only: the code, test, commit or doc you checked the note against (default relation verified_by)."));
            (
                format!("Judge a note against the current code: verified (still true), outdated (needs review) or wrong (no longer listed). {CWD_RULE}"),
                vec!["cwd", "id", "verdict"],
            )
        }
        "memory_link" => {
            base(&mut properties, true);
            properties.insert("from".into(), json!({ "type": "string" }));
            properties.insert("to".into(), json!({ "type": "string" }));
            properties.insert("kind".into(), json!({ "type": "string", "enum": link_kinds() }));
            properties.insert("note".into(), json!({ "type": "string", "description": "Why the two are connected." }));
            properties.insert("remove".into(), json!({ "type": "boolean", "description": "Remove the link instead." }));
            (
                format!("Connect two notes in the memory graph (from depends_on / supports / contradicts / related_to to), or remove a link. {CWD_RULE}"),
                vec!["cwd", "from", "to", "kind"],
            )
        }
        "memory_missed" => {
            base(&mut properties, false);
            let ids = |what: &str, max: usize| json!({ "type": "array", "items": { "type": "string" }, "maxItems": max, "description": what });
            properties.insert("query".into(), json!({ "type": "string", "description": "The query or task you ran, as you ran it." }));
            properties.insert("via".into(), json!({ "type": "string", "enum": names(VIAS.into_iter()), "description": "The tool that missed." }));
            properties.insert("style".into(), json!({ "type": "string", "enum": names(STYLES.into_iter()), "description": "The query's shape; left out, it is read from the query." }));
            properties.insert("shown".into(), ids("The ids that came back, in order.", SHOWN_MAX));
            properties.insert("wrong".into(), ids("Of those (or any), the ids that misled you.", WRONG_MAX));
            properties.insert("wanted".into(), ids("The ids you should have got - found later by another search, a link or the map.", WANTED_MAX));
            properties.insert("want".into(), json!({ "type": "string", "description": "What you wanted, in words: required when no note holds it (wanted empty)." }));
            properties.insert("paths".into(), json!({ "type": "array", "items": { "type": "string" }, "maxItems": PATHS_MAX, "description": "The brief's paths, when it was a brief." }));
            (
                format!("Record a retrieval that failed you: a brief, search or related call that missed the note you needed, ranked it low or brought a misleading one. Changes no note; the project's misses become evaluation queries, so the miss is scored until it is fixed. Pass wanted (ids) or want (words). {CWD_RULE}"),
                vec!["cwd", "query"],
            )
        }
        _ => (String::new(), Vec::new()),
    };
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });
    let mut tool = Tool::default();
    tool.name = name.to_string().into();
    tool.description = Some(description.into());
    tool.input_schema = Arc::new(schema.as_object().cloned().unwrap_or_else(JsonObject::new));
    tool
}

/// A tool result carrying text; errors are in-band (`isError: true`) so a model
/// reads the reason as tool output instead of a dropped request.
fn tool_text(text: String, is_error: bool) -> CallToolResponse {
    let result = if is_error {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    };
    result.into()
}

/// The MCP server one request gets. Fresh per request (the stateless contract);
/// the shared cells ride behind Arcs.
pub struct MemoryServer {
    pub state: Arc<MemoryState>,
    pub name: Option<Arc<std::sync::RwLock<String>>>,
    pub source: CallSource,
    pub log: Arc<CallLog>,
}

/// The call's target: the required absolute cwd, and `scope: global` selecting the
/// shared notes. The service validates the directory itself.
fn target(f: &Fields<'_>) -> MemResult<Target> {
    let cwd = match f.0.get("cwd") {
        Some(Value::String(cwd)) if !cwd.trim().is_empty() => cwd.clone(),
        _ => return Err(MemError::invalid_cwd("cwd is required: pass your own absolute working directory")),
    };
    match f.text("scope")?.as_deref() {
        None | Some("project") => Ok(Target::dir(cwd)),
        Some("global") => Ok(Target::global(cwd)),
        Some(other) => Err(MemError::invalid(format!("scope must be project or global, not {other:?}"))),
    }
}

/// One tool run: (the client's text, the call log's outcome).
type Outcome = MemResult<(String, String)>;

fn json_text(value: Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

impl MemoryServer {
    async fn call(&self, tool: &str, args: &Map<String, Value>) -> Outcome {
        let service = self
            .state
            .current()
            .ok_or_else(|| MemError::new(swiss_memory::Code::StoreError, "the memory plugin is not serving on this gateway"))?;
        let actor = Actor::mcp(self.source.client.as_deref());
        let f = Fields(args);
        match tool {
            "memory_brief" => brief(&service, &f).await,
            "memory_search" => search(&service, &f).await,
            "memory_related" => related(&service, &f).await,
            "memory_ask" => {
                f.only(&["cwd", "scope", "question", "agent", "worktree"])?;
                let target = target(&f)?;
                let question = f.required("question")?;
                let agent = f.text("agent")?.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
                let worktree = f.text("worktree")?.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
                let answered = crate::memory_ask::answer(
                    &self.state,
                    &service,
                    &target,
                    &question,
                    agent.as_deref(),
                    worktree.as_deref(),
                )
                .await?;
                let body = json!({
                    "answer": answered.text,
                    "agent": answered.agent,
                    "model": answered.model,
                    "mode": answered.mode,
                });
                Ok((json_text(body), format!("ok {}", answered.agent)))
            }
            "memory_get" => {
                f.only(&["cwd", "scope", "id", "includeHistory"])?;
                let target = target(&f)?;
                let history = f.flag("includeHistory")?.unwrap_or(false);
                let (record, revisions) = service.get(&target, &f.required("id")?, history).await?;
                Ok((render::record(&record, revisions.as_deref()), format!("ok {}", record.summary.id)))
            }
            "memory_domain" => {
                f.only(&["cwd", "path", "includeHistory"])?;
                let target = target(&f)?;
                match f.text("path")?.filter(|p| !p.trim().is_empty()) {
                    None => {
                        let list = service.domains(&target).await?;
                        Ok((render::map(&list), format!("ok {}", list.items.len())))
                    }
                    Some(path) => {
                        let detail = service.domain(&target, &path, f.flag("includeHistory")?.unwrap_or(false)).await?;
                        Ok((render::domain(&detail), format!("ok {}", detail.domain.id)))
                    }
                }
            }
            "memory_domain_save" => {
                f.only(&["cwd", "path", "title", "summary", "body", "owns", "keywords", "expectedRevision", "newPath", "reason"])?;
                let target = target(&f)?;
                let path = f.required("path")?;
                // The path decides: absent is a create, present an edit.
                let (view, created) = match service.domain(&target, &path, false).await {
                    Ok(_) => (service.update_domain(&target, &actor, &path, f.domain_update("newPath")?).await?, false),
                    Err(err) if err.code == Code::NotFound => {
                        for key in ["expectedRevision", "newPath", "reason"] {
                            if f.0.get(key).is_some_and(|v| !v.is_null()) {
                                return Err(MemError::invalid(format!("{key}: {path} does not exist yet; creating it takes no {key}")));
                            }
                        }
                        (service.create_domain(&target, &actor, f.domain_request()?).await?, true)
                    }
                    Err(err) => return Err(err),
                };
                let body = json!({ "id": view.id, "path": view.path, "revision": view.revision, "created": created, "confirmed": view.confirmed });
                Ok((json_text(body), format!("ok {}", view.id)))
            }
            "memory_save" => {
                f.only(&["cwd", "scope", "kind", "title", "content", "anchors", "refs", "keywords", "links", "replaces", "allowSimilar", "domain", "evidence"])?;
                let target = target(&f)?;
                let outcome = service.save(&target, &actor, f.save_request()?).await?;
                let id = outcome.record.summary.id.clone();
                let mut body = json!({
                    "id": id,
                    "revision": outcome.record.summary.revision,
                    "duplicate": outcome.duplicate,
                    "similar": outcome.similar.iter().map(|s| json!({ "id": s.id, "title": s.title })).collect::<Vec<_>>(),
                    "project": outcome.project,
                    "domain": outcome.record.summary.domain,
                });
                if !outcome.contradicts.is_empty() {
                    body["contradicts"] = json!(outcome.contradicts);
                }
                if !outcome.formation.is_empty() {
                    body["formation"] = json!(outcome.formation);
                }
                if let Some(code) = outcome.code.as_ref().filter(|c| !c.findings.is_empty()) {
                    body["status"] = json!(outcome.record.summary.status);
                    body["code"] = json!(code);
                }
                Ok((json_text(body), format!("ok {id}")))
            }
            "memory_update" => {
                f.only(&["cwd", "scope", "id", "expectedRevision", "kind", "title", "content", "anchors", "refs", "keywords", "reason", "domain", "evidence"])?;
                let target = target(&f)?;
                let outcome = service.update_checked(&target, &actor, &f.required("id")?, f.update_request()?).await?;
                let s = &outcome.record.summary;
                let mut body = json!({ "id": s.id, "revision": s.revision, "domain": s.domain });
                if let Some(code) = &outcome.code {
                    body["status"] = json!(s.status);
                    if !code.findings.is_empty() {
                        body["code"] = json!(code);
                    }
                }
                Ok((json_text(body), format!("ok {}", s.id)))
            }
            "memory_mark" => {
                f.only(&["cwd", "scope", "id", "verdict", "reason", "evidence"])?;
                let target = target(&f)?;
                let reason = f.text("reason")?;
                let evidence = f.evidence("evidence")?;
                let record =
                    service.mark_with(&target, &actor, &f.required("id")?, f.verdict("verdict")?, reason.as_deref(), evidence).await?;
                let s = &record.summary;
                let body = json!({ "id": s.id, "status": s.status, "revision": s.revision, "sourceVerified": s.source_verified });
                Ok((json_text(body), format!("ok {}", s.id)))
            }
            "memory_link" => {
                f.only(&["cwd", "scope", "from", "to", "kind", "note", "remove"])?;
                let target = target(&f)?;
                let (from, to, kind) = (f.required("from")?, f.required("to")?, f.link_kind("kind")?);
                let changed = if f.flag("remove")?.unwrap_or(false) {
                    if f.text("note")?.is_some() {
                        return Err(MemError::invalid("note: not with remove"));
                    }
                    service.unlink(&target, &from, &to, kind).await?
                } else {
                    let note = f.text("note")?;
                    service.link(&target, &actor, &from, &to, kind, note.as_deref()).await?.1
                };
                let body = json!({ "from": from, "to": to, "kind": kind, "changed": changed });
                Ok((json_text(body), "ok".into()))
            }
            "memory_missed" => {
                f.only(&["cwd", "query", "via", "style", "shown", "wrong", "wanted", "want", "paths"])?;
                let target = target(&f)?;
                let miss = service.record_miss(&target, &actor, f.miss_request()?).await?;
                let body = json!({ "id": miss.id, "project": miss.project });
                Ok((json_text(body), format!("ok {}", miss.id)))
            }
            other => Err(MemError::invalid(format!("unknown tool {other}"))),
        }
    }
}

async fn brief(service: &MemoryService, f: &Fields<'_>) -> Outcome {
    f.only(&["cwd", "paths", "task", "maxChars", "includeGlobal"])?;
    let target = target(f)?;
    let request = BriefRequest {
        paths: f.texts("paths")?.unwrap_or_default(),
        task: f.text("task")?,
        max_chars: f.number("maxChars")?.map(|n| n as usize),
        exclude_global: !f.flag("includeGlobal")?.unwrap_or(true),
    };
    let out = service.brief(&target, &request).await?;
    Ok((out.markdown, "ok".into()))
}

async fn related(service: &MemoryService, f: &Fields<'_>) -> Outcome {
    f.only(&["cwd", "scope", "id", "text", "depth", "limit", "includeHistory"])?;
    let target = target(f)?;
    // Exactly one anchor: an id walks the graph from a note, a passage from its terms.
    let seed = match (f.text("id")?, f.text("text")?) {
        (Some(id), None) => Seed::Id(id),
        (None, Some(text)) => Seed::Text(text),
        (Some(_), Some(_)) => return Err(MemError::invalid("id and text: pass exactly one")),
        (None, None) => return Err(MemError::invalid("id or text: pass exactly one")),
    };
    let request = RelatedRequest {
        seed,
        depth: f.number("depth")?.map(|n| n as u32),
        limit: f.number("limit")?.map(|n| n as usize),
        include_history: f.flag("includeHistory")?.unwrap_or(false),
    };
    let outcome = service.related(&target, &request).await?;
    let mut body = render::related(&outcome.items);
    // The similar tier not running is worth a word, not silence (SPEC §memory.embedding).
    if let Some(state) = outcome.semantic.filter(|s| s.state != "on") {
        let why = state.reason.as_deref().unwrap_or(state.state);
        body.push_str(&format!("Similar meaning not matched: {why}.\n"));
    }
    Ok((body, format!("ok {}", outcome.items.len())))
}

async fn search(service: &MemoryService, f: &Fields<'_>) -> Outcome {
    f.only(&["cwd", "scope", "query", "kind", "limit", "includeHistory", "domain"])?;
    let target = target(f)?;
    let query = SearchQuery {
        text: f.required("query")?,
        kinds: f.kind("kind")?.into_iter().collect(),
        include_history: f.flag("includeHistory")?.unwrap_or(false),
        limit: f.number("limit")?.map(|n| n as usize).unwrap_or(swiss_memory::search::DEFAULT_LIMIT),
        domain: f.text("domain")?,
        words_only: false,
    };
    let hits = service.search(&target, &query).await?;
    Ok((render::hits(&hits), format!("ok {}", hits.len())))
}

impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());
        let mut server_info = Implementation::default();
        server_info.name = "memory".into();
        server_info.version = "2.0".into();
        ServerConfig::new(capabilities).with_server_info(server_info).with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(TOOLS.iter().map(|name| tool_def(name)).collect())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let tool = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        async move {
            // Unknown tools stay protocol errors; business errors are in-band.
            if !TOOLS.contains(&tool.as_str()) {
                return Err(ErrorData::invalid_params(format!("unknown tool {tool}"), None));
            }
            let started = std::time::Instant::now();
            let (text, summary, is_error) = match self.call(&tool, &args).await {
                Ok((text, summary)) => (text, summary, false),
                Err(err) => (json_text(err.to_json()), err.code.as_str().to_string(), true),
            };
            // METADATA-ONLY: the tool, the caller, timing and the outcome - never
            // the arguments, never the text the client gets.
            let mcp = self.name.as_ref().and_then(|n| n.read().ok()).map(|guard| guard.clone());
            self.log.record_call(
                mcp.as_deref(),
                &self.source,
                CallRecord { tool, args: None, ok: !is_error, ms: started.elapsed().as_millis() as u64, output: summary },
            );
            Ok(tool_text(text, is_error))
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOLS.iter().find(|tool| **tool == name).map(|tool| tool_def(tool))
    }
}

/// The builtin adapter: compiled in, no connection, no child - it holds the state
/// slot and the call log, and costs nothing until a call arrives.
pub struct MemoryMcpAdapter {
    state: Arc<MemoryState>,
    name: Arc<std::sync::RwLock<String>>,
    log: Arc<CallLog>,
}

impl MemoryMcpAdapter {
    pub fn new(state: Arc<MemoryState>, log: Arc<CallLog>) -> Self {
        MemoryMcpAdapter { state, name: Arc::new(std::sync::RwLock::new("memory".to_string())), log }
    }
}

#[async_trait]
impl Adapter for MemoryMcpAdapter {
    fn kind(&self) -> &str {
        "memory"
    }

    /// Notes and queries never reach the traffic log (SPEC §memory.mcp logging).
    fn log_policy(&self) -> LogPolicy {
        LogPolicy::MetadataOnly
    }

    async fn build(&self) -> Result<McpEndpoint, String> {
        // A fresh server per request (the stateless contract); shared cells ride
        // behind Arcs so renames and lifecycle changes are seen live.
        let state = self.state.clone();
        let name = self.name.clone();
        let log = self.log.clone();
        Ok(swiss_mcp::adapters::rmcp_endpoint(move |source| MemoryServer {
            state: state.clone(),
            name: Some(name.clone()),
            source,
            log: log.clone(),
        }))
    }

    fn rename(&self, next: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = next.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn knowledge_mcp_tool_surface() {
        let defs: Vec<Tool> = TOOLS.iter().map(|name| tool_def(name)).collect();
        assert_eq!(defs.len(), 12);
        // The instructions name the strategy choice (SPEC §memory.mcp): words, graph, area.
        for word in ["memory_search", "memory_related", "memory_domain"] {
            assert!(INSTRUCTIONS.contains(word), "the instructions name {word}");
        }
        for tool in &defs {
            let schema = serde_json::to_value(&*tool.input_schema).expect("schema serializes");
            assert_eq!(schema["additionalProperties"], Value::Bool(false), "{}", tool.name);
            let required = schema["required"].as_array().expect("required");
            assert!(required.contains(&json!("cwd")), "{} requires cwd", tool.name);
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(description.contains("absolute working directory"), "{} states the routing rule", tool.name);
        }
        let save = tool_def("memory_save");
        let policy = save.description.as_deref().unwrap_or_default();
        assert!(policy.contains("Save when"), "memory_save carries the policy");
        assert!(policy.contains("Code is ground truth") && policy.contains("why not"), "and what a note is (ADR-032): {policy}");
        let hub = tool_def("memory_domain_save");
        assert!(hub.description.as_deref().unwrap_or_default().contains("Entry points"), "memory_domain_save carries the hub's shape");
        let link = serde_json::to_value(&*tool_def("memory_link").input_schema).expect("schema");
        assert!(!link["properties"]["kind"]["enum"].as_array().expect("enum").contains(&json!("supersedes")), "supersedes is system-only");
        for name in ["memory_save", "memory_update", "memory_mark"] {
            let schema = serde_json::to_value(&*tool_def(name).input_schema).expect("schema");
            let items = &schema["properties"]["evidence"]["items"];
            assert_eq!(items["additionalProperties"], Value::Bool(false), "{name}: a line number cannot ride along");
            assert!(items["properties"]["sourceType"]["enum"].as_array().expect("enum").contains(&json!("commit")), "{name}");
        }
    }

    // --- the wire: real JSON-RPC sessions ----------------------------------------------

    /// A server over a real service in a scratch home with two repos and a call log on disk.
    struct Wire {
        home: PathBuf,
        state: Arc<MemoryState>,
        log: Arc<CallLog>,
    }

    impl Wire {
        fn new(label: &str) -> Wire {
            let home = std::env::temp_dir().join(format!(
                "swiss-memwire-{label}-{}-{}",
                std::process::id(),
                swiss_core::util::random_hex(6)
            ));
            for repo in ["a", "b"] {
                std::fs::create_dir_all(home.join(repo).join(".git")).expect("scratch repo");
            }
            let home = home.canonicalize().expect("canonical home");
            let state = Arc::new(MemoryState::new());
            state.install(Arc::new(MemoryService::at(&home.join("store.sqlite3"))));
            std::fs::create_dir_all(home.join("calls")).expect("log dir");
            let log = Arc::new(CallLog::at(home.join("calls")));
            Wire { home, state, log }
        }

        fn repo(&self, name: &str) -> String {
            swiss_memory::scope::display(&self.home.join(name))
        }

        async fn call(&self, tool: &str, args: Value) -> CallToolResult {
            let server = MemoryServer {
                state: self.state.clone(),
                name: Some(Arc::new(std::sync::RwLock::new("memory".to_string()))),
                source: CallSource::default(),
                log: self.log.clone(),
            };
            let client = swiss_mcp::introspect::open_session(server).await.expect("session opens");
            let mut params = CallToolRequestParams::default();
            params.name = tool.to_string().into();
            params.arguments = args.as_object().cloned();
            let out = client.call_tool(params).await.expect("tools/call answers");
            let _ = client.cancel().await;
            out
        }
    }

    impl Drop for Wire {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .map(|c| match c {
                ContentBlock::Text(t) => t.text.clone(),
                other => format!("[{other:?} content]"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn is_error(result: &CallToolResult) -> bool {
        result.is_error.unwrap_or(false)
    }

    fn error_code(result: &CallToolResult) -> String {
        let body: Value = serde_json::from_str(&text_of(result)).unwrap_or(Value::Null);
        body["code"].as_str().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn knowledge_mcp_requires_an_absolute_cwd() {
        let wire = Wire::new("cwd");
        let out = wire.call("memory_search", json!({ "query": "anything" })).await;
        assert!(is_error(&out));
        assert_eq!(error_code(&out), "memory_invalid_cwd", "{}", text_of(&out));

        let out = wire.call("memory_brief", json!({ "cwd": "relative/dir" })).await;
        assert_eq!(error_code(&out), "memory_invalid_cwd", "{}", text_of(&out));

        let out = wire.call("memory_get", json!({ "cwd": wire.repo("a"), "id": "m-0000000000", "extra": 1 })).await;
        assert_eq!(error_code(&out), "memory_invalid", "unknown fields are refused: {}", text_of(&out));
    }

    #[tokio::test]
    async fn knowledge_mcp_related_walks_the_graph_from_a_seed() {
        let wire = Wire::new("related");
        let seed = wire
            .call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "pitfall",
                "title": "Alpha retries double charges",
                "content": "Why: no idempotency key. How: pass one.",
                "anchors": ["src/pay.rs"], "keywords": ["retry"] })).await;
        let body: Value = serde_json::from_str(&text_of(&seed)).expect("save answers JSON");
        let seed_id = body["id"].as_str().expect("id").to_string();
        let other = wire
            .call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "decision",
                "title": "Charges carry an idempotency key",
                "content": "Why: retries double charges otherwise. How: key per cart." })).await;
        let body: Value = serde_json::from_str(&text_of(&other)).expect("save answers JSON");
        let other_id = body["id"].as_str().expect("id").to_string();
        let linked = wire
            .call("memory_link", json!({ "cwd": wire.repo("a"), "from": other_id, "to": seed_id,
                "kind": "depends_on" })).await;
        assert!(!is_error(&linked), "{}", text_of(&linked));

        // From the note: the link walks outward and the answer says why.
        let out = wire.call("memory_related", json!({ "cwd": wire.repo("a"), "id": seed_id })).await;
        let text = text_of(&out);
        assert!(!is_error(&out), "{}", text);
        assert!(text.contains(&other_id), "the linked note comes: {text}");
        assert!(text.contains("why: depends_on"), "the reason rides along: {text}");

        // From a passage: shared terms carry it, no id needed.
        let out = wire
            .call("memory_related", json!({ "cwd": wire.repo("a"), "text": "retries double the charge without a key" }))
            .await;
        let text = text_of(&out);
        assert!(text.contains(&seed_id), "a phrase anchors the walk: {text}");

        // Exactly one anchor: both, or neither, is refused.
        let both = wire
            .call("memory_related", json!({ "cwd": wire.repo("a"), "id": seed_id, "text": "words" }))
            .await;
        assert_eq!(error_code(&both), "memory_invalid", "{}", text_of(&both));
        let neither = wire.call("memory_related", json!({ "cwd": wire.repo("a") })).await;
        assert_eq!(error_code(&neither), "memory_invalid", "{}", text_of(&neither));
        // The unknown-field rule holds here too.
        let extra = wire
            .call("memory_related", json!({ "cwd": wire.repo("a"), "id": seed_id, "bogus": 1 }))
            .await;
        assert_eq!(error_code(&extra), "memory_invalid", "{}", text_of(&extra));
    }

    #[tokio::test]
    async fn knowledge_mcp_missed_records_a_failed_retrieval() {
        let wire = Wire::new("missed");
        let saved = wire
            .call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "decision",
                "title": "Charges carry an idempotency key",
                "content": "Why: retries double charges otherwise. How: key per cart." })).await;
        let body: Value = serde_json::from_str(&text_of(&saved)).expect("save answers JSON");
        let id = body["id"].as_str().expect("id").to_string();

        let out = wire
            .call("memory_missed", json!({ "cwd": wire.repo("a"), "query": "customer billed twice",
                "via": "search", "wanted": [id] })).await;
        assert!(!is_error(&out), "{}", text_of(&out));
        let body: Value = serde_json::from_str(&text_of(&out)).expect("missed answers JSON");
        assert!(body["id"].as_str().expect("id").starts_with("x-"), "{body}");
        let service = wire.state.current().expect("service");
        let listed = service.misses(&Target::dir(wire.repo("a"))).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].query.as_str(), listed[0].actor.as_str()), ("customer billed twice", "mcp"));

        // Wanted ids or words; a vocabulary for via; the unknown-field rule.
        let out = wire.call("memory_missed", json!({ "cwd": wire.repo("a"), "query": "x" })).await;
        assert_eq!(error_code(&out), "memory_invalid", "{}", text_of(&out));
        let out = wire.call("memory_missed", json!({ "cwd": wire.repo("a"), "query": "x", "want": "y", "via": "grep" })).await;
        assert_eq!(error_code(&out), "memory_invalid", "{}", text_of(&out));
        let out = wire.call("memory_missed", json!({ "cwd": wire.repo("a"), "query": "x", "want": "y", "bogus": 1 })).await;
        assert_eq!(error_code(&out), "memory_invalid", "{}", text_of(&out));
        // Another project's directory cannot claim this project's note.
        wire.call("memory_save", json!({ "cwd": wire.repo("b"), "kind": "knowledge",
            "title": "Beta keeps its own notes", "content": "Why: isolation. How: by cwd." })).await;
        let out = wire.call("memory_missed", json!({ "cwd": wire.repo("b"), "query": "x", "wanted": [id] })).await;
        assert_eq!(error_code(&out), "memory_invalid", "{}", text_of(&out));
    }

    #[tokio::test]
    async fn knowledge_mcp_projects_are_isolated_by_cwd() {
        let wire = Wire::new("iso");
        let saved = wire
            .call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "pitfall", "title": "Alpha retries double charge", "content": "Why: no key." }))
            .await;
        assert!(!is_error(&saved), "{}", text_of(&saved));
        let body: Value = serde_json::from_str(&text_of(&saved)).expect("save answers JSON");
        let id = body["id"].as_str().expect("id").to_string();
        assert_eq!(body["project"]["registered"], true, "the first save registers the repo");
        assert!(body.get("formation").is_none(), "a one-claim note carries no hint: {body}");
        let doc = "## What happens\nEvery retry doubles.\n## Why\nNo key.";
        let saved = wire.call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "pitfall", "title": "Retries double", "content": doc })).await;
        let body: Value = serde_json::from_str(&text_of(&saved)).expect("save answers JSON");
        let rules: Vec<&str> = body["formation"].as_array().expect("hints").iter().filter_map(|l| l["rule"].as_str()).collect();
        assert_eq!(rules, ["sections"], "the lint answers, the save still lands: {body}");
        assert!(body.get("code").is_none(), "nothing named, nothing found: {body}");
        // A count nothing re-counts is held; the agent's own edit that scopes it releases it.
        let count = json!({ "cwd": wire.repo("a"), "kind": "knowledge", "title": "Retrying clients",
                            "content": "Exactly two clients retry the charge." });
        let body: Value = serde_json::from_str(&text_of(&wire.call("memory_save", count).await)).expect("save answers JSON");
        let found = (body["status"].as_str(), body["code"]["findings"][0]["rule"].as_str());
        assert_eq!(found, (Some("needs_review"), Some("unscoped_quantifier")));
        assert!(body.get("formation").is_none(), "the check says it once: {body}");
        let held = body["id"].as_str().expect("id").to_string();
        let marked = wire.call("memory_mark", json!({ "cwd": wire.repo("a"), "id": held, "verdict": "verified" })).await;
        assert_eq!(error_code(&marked), "memory_invalid", "an agent fixes a held note, it does not vouch for it: {}", text_of(&marked));
        let fix = json!({ "cwd": wire.repo("a"), "id": held, "expectedRevision": 1,
                          "content": "In the inspected scope exactly two clients retry the charge." });
        let body: Value = serde_json::from_str(&text_of(&wire.call("memory_update", fix).await)).expect("update answers JSON");
        assert_eq!((body["status"].as_str(), body.get("code")), (Some("active"), None), "{body}");
        // An agent's note anchored on a file the checkout lacks is held for review, and says why.
        let gone = json!({ "cwd": wire.repo("a"), "kind": "knowledge", "title": "Gone", "content": "Why: moved.",
                           "anchors": ["src/gone.rs"] });
        let saved = wire.call("memory_save", gone).await;
        let body: Value = serde_json::from_str(&text_of(&saved)).expect("save answers JSON");
        assert_eq!((body["status"].as_str(), body["code"]["held"].as_bool()), (Some("needs_review"), Some(true)), "{body}");
        assert_eq!(body["code"]["findings"][0]["rule"], "missing_anchor", "{body}");

        let found = wire.call("memory_search", json!({ "cwd": wire.repo("a"), "query": "Alpha retries" })).await;
        assert!(text_of(&found).contains(&id), "{}", text_of(&found));
        let other = wire.call("memory_search", json!({ "cwd": wire.repo("b"), "query": "Alpha retries" })).await;
        assert!(!text_of(&other).contains(&id), "project b never sees a's note: {}", text_of(&other));
        let get = wire.call("memory_get", json!({ "cwd": wire.repo("b"), "id": id })).await;
        assert_eq!(error_code(&get), "memory_not_found");

        let marked = wire.call("memory_mark", json!({ "cwd": wire.repo("a"), "id": id, "verdict": "outdated" })).await;
        assert_eq!(error_code(&marked), "memory_invalid", "outdated needs a reason");
        let brief = wire.call("memory_brief", json!({ "cwd": wire.repo("a"), "task": "retries" })).await;
        assert!(text_of(&brief).contains(&id), "{}", text_of(&brief));
    }

    /// An agent says what a note rests on (ADR-032); `memory_get` shows who wrote it and the
    /// evidence, and a verified mark answers whether a checkable source now backs the text.
    #[tokio::test]
    async fn knowledge_mcp_provenance_and_evidence() {
        let wire = Wire::new("evidence");
        let a = wire.repo("a");
        // The save reads cited code where it says it is (§memory.code).
        for (path, text) in [("src/retry.rs", "pub fn retry() {}\n"), ("tests/retry.rs", "fn reuses_the_key() {}\n")] {
            let file = wire.home.join("a").join(path);
            std::fs::create_dir_all(file.parent().expect("a parent")).expect("scratch dir");
            std::fs::write(file, text).expect("cited file");
        }
        let saved = wire
            .call(
                "memory_save",
                json!({ "cwd": a, "kind": "decision", "title": "Retries reuse the key", "content": "So a replay is idempotent.",
                        "evidence": [{ "sourceType": "code", "path": "src/retry.rs", "symbol": "retry" }] }),
            )
            .await;
        assert!(!is_error(&saved), "{}", text_of(&saved));
        let id = serde_json::from_str::<Value>(&text_of(&saved)).expect("json")["id"].as_str().expect("id").to_string();

        let session = wire.call("memory_mark", json!({ "cwd": a, "id": id, "verdict": "verified", "evidence": [{ "sourceType": "session", "sessionId": "s-1" }] })).await;
        assert_eq!(error_code(&session), "memory_invalid", "a session does not verify: {}", text_of(&session));
        let stray = wire.call("memory_mark", json!({ "cwd": a, "id": id, "verdict": "verified", "evidence": [{ "sourceType": "code", "path": "src/a.rs", "lines": "1-9" }] })).await;
        assert_eq!(error_code(&stray), "memory_invalid", "{}", text_of(&stray));
        let marked = wire.call("memory_mark", json!({ "cwd": a, "id": id, "verdict": "verified", "evidence": [{ "sourceType": "test", "path": "tests/retry.rs", "symbol": "reuses_the_key" }] })).await;
        let body: Value = serde_json::from_str(&text_of(&marked)).expect("json");
        assert_eq!((body["revision"].as_u64(), body["sourceVerified"].as_bool()), (Some(2), Some(true)), "{body}");

        let shown = text_of(&wire.call("memory_get", json!({ "cwd": a, "id": id })).await);
        assert!(shown.contains("by agent via mcp") && shown.contains("source-verified"), "{shown}");
        assert!(shown.contains("- derived_from code src/retry.rs#retry (rev 1)") && shown.contains("- verified_by test tests/retry.rs#reuses_the_key (rev 2)"), "{shown}");
    }

    #[tokio::test]
    async fn knowledge_mcp_domains() {
        let wire = Wire::new("domains");
        let a = wire.repo("a");
        let empty = wire.call("memory_domain", json!({ "cwd": a })).await;
        assert!(text_of(&empty).contains("no map yet"), "{}", text_of(&empty));

        // An absent path is created (unconfirmed: an agent drew it), a present one edited.
        let made = wire
            .call("memory_domain_save", json!({ "cwd": a, "path": "pay", "title": "Payments", "summary": "Charging and refunds", "owns": ["src/pay"], "body": "## Entry points\n- `PayController`" }))
            .await;
        assert!(!is_error(&made), "{}", text_of(&made));
        let body: Value = serde_json::from_str(&text_of(&made)).expect("json");
        assert_eq!((body["created"].as_bool(), body["confirmed"].as_bool(), body["revision"].as_u64()), (Some(true), Some(false), Some(1)));
        let stale = wire.call("memory_domain_save", json!({ "cwd": a, "path": "pay", "summary": "x" })).await;
        assert_eq!(error_code(&stale), "memory_invalid", "an edit needs the revision: {}", text_of(&stale));
        let early = wire.call("memory_domain_save", json!({ "cwd": a, "path": "ship", "title": "Shipping", "expectedRevision": 1 })).await;
        assert_eq!(error_code(&early), "memory_invalid", "{}", text_of(&early));
        let edited = wire.call("memory_domain_save", json!({ "cwd": a, "path": "pay", "summary": "Charging, refunds", "expectedRevision": 1 })).await;
        assert!(!is_error(&edited), "{}", text_of(&edited));
        wire.call("memory_domain_save", json!({ "cwd": a, "path": "pay/refunds", "title": "Refunds", "owns": ["src/pay/refund"] })).await;

        // A save is filed by its anchors and says where; an update can re-file alone.
        let refund = wire.home.join("a").join("src").join("pay").join("refund");
        std::fs::create_dir_all(&refund).expect("refund dir");
        std::fs::write(refund.join("mod.rs"), "pub fn settle() {}\n").expect("anchored file");
        let saved = wire
            .call("memory_save", json!({ "cwd": a, "kind": "pitfall", "title": "Refunds settle next day", "content": "Why: the bank batches.", "anchors": ["src/pay/refund/mod.rs"] }))
            .await;
        let body: Value = serde_json::from_str(&text_of(&saved)).expect("json");
        assert_eq!(body["domain"], "pay/refunds", "{body}");
        let id = body["id"].as_str().expect("id").to_string();
        let refiled = wire.call("memory_update", json!({ "cwd": a, "id": id, "domain": "pay" })).await;
        let body: Value = serde_json::from_str(&text_of(&refiled)).expect("json");
        assert_eq!((body["domain"].as_str(), body["revision"].as_u64()), (Some("pay"), Some(1)), "{body}");
        let retitled = wire.call("memory_update", json!({ "cwd": a, "id": id, "title": "Refunds settle later" })).await;
        assert_eq!(error_code(&retitled), "memory_invalid", "content still needs the revision");

        let map = text_of(&wire.call("memory_domain", json!({ "cwd": a })).await);
        assert!(map.contains("- `pay` Payments — Charging, refunds · 1 note · unconfirmed") && map.contains("  - `pay/refunds` Refunds"), "{map}");
        let hub = text_of(&wire.call("memory_domain", json!({ "cwd": a, "path": "pay" })).await);
        assert!(hub.contains("`PayController`") && hub.contains("Refunds settle next day") && hub.contains("- src/pay"), "{hub}");
        let found = text_of(&wire.call("memory_search", json!({ "cwd": a, "query": "refunds", "domain": "pay/refunds" })).await);
        assert!(!found.contains(&id), "pay/refunds no longer holds it: {found}");

        // Another project never sees this map.
        let other = text_of(&wire.call("memory_domain", json!({ "cwd": wire.repo("b"), "path": "pay" })).await);
        assert!(other.contains("memory_not_found"), "{other}");
    }

    #[tokio::test]
    async fn knowledge_mcp_calls_never_log_content() {
        let wire = Wire::new("log");
        let saved = wire
            .call("memory_save", json!({ "cwd": wire.repo("a"), "kind": "decision", "title": "SENTINELTITLE routing", "content": "SENTINELBODY because" }))
            .await;
        assert!(!is_error(&saved), "{}", text_of(&saved));
        let found = wire.call("memory_search", json!({ "cwd": wire.repo("a"), "query": "SENTINELTITLE" })).await;
        assert!(text_of(&found).contains("SENTINELTITLE"), "the CLIENT sees the title");
        let refused = wire.call("memory_search", json!({ "cwd": wire.repo("a"), "query": "SENTINELQUERY", "bogus": true })).await;
        assert!(is_error(&refused));

        // The log is written by a spawned task: poll briefly rather than sleep long.
        let log_file = wire.home.join("calls").join("memory.jsonl");
        let mut lines = String::new();
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&log_file) {
                if text.lines().count() >= 3 {
                    lines = text;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(lines.lines().count() >= 3, "every call was logged: {lines:?}");
        for line in lines.lines() {
            assert!(!line.contains("SENTINEL"), "no argument or result text in the log: {line}");
            let entry: Value = serde_json::from_str(line).expect("log lines are JSON");
            assert_eq!(entry["args"].as_str().unwrap_or_default(), "", "args are dropped by policy");
            let output = entry["output"].as_str().unwrap_or_default();
            assert!(output.starts_with("ok") || output.starts_with("memory_"), "output is an outcome: {output}");
        }
    }

    /// memory_ask refuses a malformed ask as memory_invalid, and a gateway with no agent
    /// to answer says memory_agent_unavailable (SPEC §memory.rag).
    #[tokio::test]
    async fn knowledge_mcp_ask_refuses_a_malformed_ask_and_says_no_agent() {
        let wire = Wire::new("ask");
        let long = "x".repeat(crate::memory_ask::QUESTION_MAX + 1);
        for (args, why) in [
            (json!({ "cwd": wire.repo("a"), "question": "  " }), "an empty question"),
            (json!({ "cwd": wire.repo("a"), "question": long }), "a question past the bound"),
            (json!({ "cwd": wire.repo("a"), "question": "why?", "project": "global" }), "an unknown field"),
            (json!({ "cwd": wire.repo("a"), "question": "why?", "agent": "a,b" }), "two agents"),
        ] {
            let out = wire.call("memory_ask", args).await;
            assert!(is_error(&out), "{why}");
            assert_eq!(error_code(&out), "memory_invalid", "{why}: {}", text_of(&out));
        }
        let out = wire.call("memory_ask", json!({ "cwd": wire.repo("a"), "question": "why retries?" })).await;
        assert!(is_error(&out));
        assert_eq!(error_code(&out), "memory_agent_unavailable", "{}", text_of(&out));
        assert!(text_of(&out).contains("no llm agent to ask"), "the sentence says why: {}", text_of(&out));
    }
}
