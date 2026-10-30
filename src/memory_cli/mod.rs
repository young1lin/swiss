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

//! `swiss memory` (SPEC §memory.cli): the OFFLINE surface. It opens the same store
//! the gateway holds, gateway running or not, and never talks HTTP - WAL plus
//! BEGIN IMMEDIATE writes keep two processes on one home honest (ADR-031).
//!
//! Exit codes: 0 ok, 1 failure, 2 usage (validation included), 3 conflict
//! (`memory_revision_conflict`, `memory_similar_exists`, `memory_project_ambiguous`,
//! `memory_root_bound`, `memory_domain_exists`). `--json` writes deterministic JSON to stdout - the error
//! object `{code, message, …}` too - and diagnostics to stderr.
//!
//! Scope: the process cwd (never mutated) or `--cwd`; `--global` selects the shared
//! notes. `GIT_DIR` is honoured for the process cwd only and passed to the service
//! explicitly. `brief --hook` is the session-start hook: it always exits 0, prints
//! nothing on stdout on any failure and gives up after two seconds.

use std::collections::HashSet;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use swiss_memory::model::{Actor, Kind, LinkKind, Producer, Status, Verdict};
use swiss_memory::service::{
    BriefRequest, GraphRequest, InitMode, LinkSpec, ListRequest, RelatedRequest, SaveRequest, Target, UpdateRequest,
};
use swiss_memory::settings::Choice;
use swiss_memory::transfer::{Header, IMPORT_MAX_BYTES};
use swiss_memory::{
    formation, render, Code, ConflictKind, DomainRequest, DomainUpdate, EvidenceSpec, ExportScope, ImportOptions, ImportStrategy,
    MemError, MemoryService, MissRequest, SearchQuery, Seed,
};

mod sync;

use self::sync::*;

/// Where output goes - a trait so tests can capture.
pub trait Out {
    fn line(&self, text: &str);
    fn err(&self, text: &str);
}

pub struct StdOut;

impl Out for StdOut {
    fn line(&self, text: &str) {
        println!("{text}");
    }
    fn err(&self, text: &str) {
        eprintln!("{text}");
    }
}

const USAGE: &str = "usage: swiss memory <brief|save|search|related|ask|graph|list|show|edit|link|unlink|mark|confirm|restore|review|purge|domain|code|docs|init|project|status|sync|export|import|backup|misses|hook|rules|lint> [args]";

const MISSES_USAGE: &str = "usage: swiss memory misses [list] [--json]\n\
     swiss memory misses add QUERY [--wanted ID,ID] [--want TEXT] [--wrong ID,ID] [--shown ID,ID] [--via search|brief|related|ask|domain] [--style S] [--path P]... [--json]  (a retrieval that missed: what you wanted, by note id or in words)\n\
     swiss memory misses export [--output F]  (the misses as an evaluation fixture; the gaps on stderr)\n\
     swiss memory misses drop ID... | --all";

const SYNC_USAGE: &str = "usage: swiss memory sync [--dry-run] [--since REV] [--agent A[,B...]|off] [--wait] [--json]\n\
     swiss memory sync status [--json]\n\
     swiss memory sync cancel RUN_ID  (a queued run never starts; a running one stops between questions)\n\
     swiss memory sync --nudge  (a hook's call: exit 0 always, the cwd from stdin JSON)\n\
     swiss memory sync hooks install|uninstall";

const REVIEW_USAGE: &str = "usage: swiss memory review [--global] [--json]  (the notes waiting for a person)\n\
     swiss memory review --resolve [--changed] [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent A[,B...]|off] [--json]  (the sync agent judges the inbox's notes a model wrote against the code: confirms what it bears out, settles the contradictions; --changed: only the notes whose anchored files changed)";

const CODE_USAGE: &str = "usage: swiss memory code index | status | find NAME | file PATH | drop [--all]";

const DOCS_USAGE: &str = "usage: swiss memory docs [list] [--json]\n\
     swiss memory docs add PATH... | remove PATH...  (the checkout's folders or files whose documents the project keeps)\n\
     swiss memory docs sync [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent A[,B...]|off] [--json]  (read them; the sync agent cards each new or changed one)\n\
     swiss memory docs search WORDS... [--kind K] [--limit N] [--json]\n\
     swiss memory docs read DOC [--section N] [--json]  (DOC: an id d-…, a path or its unique tail)";

const INIT_USAGE: &str = "usage: swiss memory init [--here|--portable] [--name N] [--json]\n\
     swiss memory init --map [--redraw] [--plan] [--dry-run] [--wait] [--lang TAG] [--agent A[,B...]|off] [--lead L|off] [--json]  (draw the map from the code as it is now)
     swiss memory init --full [--redraw] [--plan] [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent A[,B...]|off] [--lead L|off] [--json]  (the map, then notes from every source file; semantic search when an embedding model is set)
     swiss memory init --recheck [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent A[,B...]|off] [--json]  (the notes a model wrote and nobody judged, judged again against the code)
     --agent and --lead pick this run's own agents, over the project's settings (`swiss memory project settings`) and the sync block; left out, the newest llm agent";

/// `--evidence`'s shape, for usage errors.
const EVIDENCE_USAGE: &str = "--evidence [RELATION=]TYPE[:LOCATOR][#SYMBOL], e.g. code:src/retry.rs#retry, \
     verified_by=test:tests/retry.rs#reuses_the_key, commit:abc1234, session:ID, \
     or one JSON object {sourceType, path, symbol, excerpt, ...}";

const DOMAIN_USAGE: &str = "usage: swiss memory domain list | show PATH [--history] | add PATH --title T [--summary S] [--owns A,B] [--keywords K1,K2] [--stdin] | edit PATH --revision N [--path NEW] [--title T] [--summary S] [--owns A,B] [--keywords K1,K2] [--stdin] | confirm PATH [--all] | verify PATH | delete PATH --confirm PATH | file ID... --to PATH|--none | autofile";

/// The hook's whole budget (SPEC §memory.brief delivery paths).
const HOOK_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a gateway call waits for its answer.
const GATEWAY_WAIT: Duration = Duration::from_secs(30);
/// How long `ask` waits: the answering agent loops over the memory's tools, a turn at a
/// time, for minutes; its own timeout ends it on the gateway first.
const ASK_WAIT: Duration = Duration::from_secs(20 * 60);

/// Flags that take no value; every other `--flag` takes one.
const BARE: [&str; 25] = [
    "json", "hook", "stdin", "global", "all", "history", "unconfirmed", "allow-similar", "here", "portable", "exact",
    "new-project", "dry-run", "no-global", "none", "bootstrap", "nudge", "wait", "map", "plan",
    "full", "redraw", "recheck", "resolve", "changed",
];

/// Repeatable flags.
const MULTI: [&str; 5] = ["path", "anchor", "ref", "link", "evidence"];

/// One failure: a usage mistake, a service error or an IO failure.
enum Fail {
    Usage(String),
    Mem(MemError),
    Io(String),
}

impl From<MemError> for Fail {
    fn from(err: MemError) -> Fail {
        Fail::Mem(err)
    }
}

fn usage(message: impl Into<String>) -> Fail {
    Fail::Usage(message.into())
}

type Run = Result<(), Fail>;

/// One parsed invocation.
struct Args {
    command: String,
    positional: Vec<String>,
    values: Vec<(String, String)>,
    bare: HashSet<String>,
}

/// `--flag value`, `--flag=value`, bare flags from [BARE], and positionals; `--`
/// ends the flags (for text starting with a dash).
fn parse(argv: &[String]) -> Result<Args, Fail> {
    let mut items = argv.iter();
    let command = items.next().cloned().ok_or_else(|| usage(format!("a command is required; {USAGE}")))?;
    let mut args = Args { command, positional: Vec::new(), values: Vec::new(), bare: HashSet::new() };
    let mut flags_done = false;
    while let Some(arg) = items.next() {
        let flag = match arg.strip_prefix("--") {
            Some("") if !flags_done => {
                flags_done = true;
                continue;
            }
            Some(flag) if !flags_done => flag,
            _ => {
                args.positional.push(arg.clone());
                continue;
            }
        };
        let (key, inline) = match flag.split_once('=') {
            Some((key, value)) => (key, Some(value.to_string())),
            None => (flag, None),
        };
        if BARE.contains(&key) {
            if inline.is_some() {
                return Err(usage(format!("--{key} takes no value")));
            }
            args.bare.insert(key.to_string());
            continue;
        }
        let value = match inline {
            Some(value) => value,
            None => items.next().cloned().ok_or_else(|| usage(format!("--{key} needs a value")))?,
        };
        if !MULTI.contains(&key) && args.values.iter().any(|(k, _)| k == key) {
            return Err(usage(format!("--{key} is given twice")));
        }
        args.values.push((key.to_string(), value));
    }
    Ok(args)
}

impl Args {
    /// Refuse flags the command does not take and a wrong positional count.
    fn allow(&self, flags: &[&str], positional: std::ops::RangeInclusive<usize>) -> Run {
        let given = self.values.iter().map(|(k, _)| k.as_str()).chain(self.bare.iter().map(String::as_str));
        for key in given {
            if !flags.contains(&key) {
                return Err(usage(format!("{} takes no --{key}", self.command)));
            }
        }
        if !positional.contains(&self.positional.len()) {
            return Err(usage(format!("{}: wrong number of arguments; {USAGE}", self.command)));
        }
        Ok(())
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn values(&self, key: &str) -> Vec<String> {
        self.values.iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
    }

    fn has(&self, key: &str) -> bool {
        self.bare.contains(key)
    }

    fn json(&self) -> bool {
        self.has("json")
    }

    fn number(&self, key: &str) -> Result<Option<u64>, Fail> {
        self.value(key)
            .map(|v| v.parse::<u64>().map_err(|_| usage(format!("--{key} must be a non-negative integer"))))
            .transpose()
    }

    fn pos(&self, index: usize) -> &str {
        self.positional.get(index).map(String::as_str).unwrap_or_default()
    }

    /// The directory and `GIT_DIR` the scope comes from.
    fn dir(&self) -> Result<(String, Option<PathBuf>), Fail> {
        if let Some(cwd) = self.value("cwd") {
            return Ok((cwd.to_string(), None));
        }
        let here = std::env::current_dir().map_err(|e| Fail::Io(format!("the current directory is unreadable: {e}")))?;
        // A relative GIT_DIR is relative to the process cwd; join keeps an absolute one.
        let git_dir = std::env::var_os("GIT_DIR").filter(|v| !v.is_empty()).map(|v| here.join(v));
        Ok((here.display().to_string(), git_dir))
    }

    fn target(&self) -> Result<Target, Fail> {
        let (cwd, git_dir) = self.dir()?;
        Ok(Target::Dir { cwd, git_dir, global: self.has("global") })
    }

    fn kind(&self) -> Result<Option<Kind>, Fail> {
        self.value("type")
            .map(|v| Kind::parse(v).ok_or_else(|| usage(format!("--type must be one of {}", Kind::choices()))))
            .transpose()
    }

    /// Comma-separated values of a closed vocabulary.
    fn list<T>(&self, key: &str, parse: fn(&str) -> Option<T>, choices: fn() -> String) -> Result<Vec<T>, Fail> {
        let Some(raw) = self.value(key) else {
            return Ok(Vec::new());
        };
        raw.split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| parse(v).ok_or_else(|| usage(format!("--{key}: {v:?} is not one of {}", choices()))))
            .collect()
    }

    /// TEXT, `--stdin` or `--<flag>`, exactly one.
    fn text(&self, positional: Option<usize>, flag: Option<&str>) -> Result<Option<String>, Fail> {
        let mut sources = Vec::new();
        if let Some(text) = positional.and_then(|i| self.positional.get(i)) {
            sources.push(text.clone());
        }
        if let Some(text) = flag.and_then(|f| self.value(f)) {
            sources.push(text.to_string());
        }
        if self.has("stdin") {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| Fail::Io(format!("read stdin: {e}")))?;
            sources.push(text);
        }
        match sources.len() {
            0 => Ok(None),
            1 => Ok(sources.pop()),
            _ => Err(usage("pass the text one way: an argument, a flag or --stdin")),
        }
    }
}

/// Print a value as JSON or as text.
fn emit(out: &dyn Out, json: bool, value: &impl Serialize, text: impl FnOnce() -> String) {
    if json {
        let body = serde_json::to_string_pretty(value).unwrap_or_else(|err| json!({ "error": err.to_string() }).to_string());
        out.line(&body);
    } else {
        out.line(text().trim_end_matches('\n'));
    }
}

/// Report a failure and answer its exit code.
fn report(fail: Fail, json: bool, out: &dyn Out) -> i32 {
    match fail {
        Fail::Usage(message) => {
            out.err(&message);
            2
        }
        Fail::Io(message) => {
            out.err(&message);
            1
        }
        Fail::Mem(err) => {
            if json {
                out.line(&serde_json::to_string_pretty(&err.to_json()).unwrap_or_default());
            }
            let mut message = format!("{}: {}", err.code.as_str(), err.message);
            if let Some(current) = err.extra.get("current") {
                message.push_str(&format!(" (current revision {current}; re-read the note and retry)"));
            }
            if let Some(similar) = err.extra.get("similar") {
                message.push_str(&format!(
                    " (similar: {}; pass --replaces {} to supersede it or --allow-similar)",
                    similar["id"].as_str().unwrap_or_default(),
                    similar["id"].as_str().unwrap_or_default()
                ));
            }
            out.err(&message);
            if err.code.is_conflict() {
                3
            } else if matches!(err.code, Code::Invalid | Code::InvalidCwd) {
                2
            } else {
                1
            }
        }
    }
}

/// The entry the CLI dispatches to (cli.rs, before the generic parser: --stdin
/// bodies must survive untouched).
pub async fn main(argv: Vec<String>) -> i32 {
    let service = MemoryService::at(&MemoryService::default_path());
    service.set_lister(Some(Arc::new(swiss_plugins::memory_preview::CheckoutLister)));
    if argv.first().map(String::as_str) == Some("brief") && argv.iter().any(|a| a == "--hook") {
        return hook(&argv, &StdOut, &service, hook_stdin()).await;
    }
    run_with(argv, &StdOut, &service).await
}

/// The CLI over a given service: the seam the tests drive with a scratch store.
pub async fn run_with(argv: Vec<String>, out: &dyn Out, service: &MemoryService) -> i32 {
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(fail) => return report(fail, false, out),
    };
    let json = args.json();
    match dispatch(&args, service, out).await {
        Ok(()) => 0,
        Err(fail) => report(fail, json, out),
    }
}

async fn dispatch(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    match a.command.as_str() {
        "brief" => brief(a, s, out).await,
        "save" | "add" => save(a, s, out).await,
        "search" => search(a, s, out).await,
        "related" => related(a, s, out).await,
        "ask" => ask(a, s, out).await,
        "graph" => graph(a, s, out).await,
        "list" => list(a, s, out).await,
        "show" => show(a, s, out).await,
        "edit" => edit(a, s, out).await,
        "link" | "unlink" => link(a, s, out).await,
        "mark" => mark(a, s, out).await,
        "confirm" | "restore" => confirm_or_restore(a, s, out).await,
        "review" => review(a, s, out).await,
        "purge" => purge(a, s, out).await,
        "domain" => domain(a, s, out).await,
        "code" => code(a, s, out).await,
        "docs" => docs_cli(a, s, out).await,
        "init" => init(a, s, out).await,
        "project" => project(a, s, out).await,
        "status" => status(a, s, out).await,
        "sync" => sync_cli(a, s, out).await,
        "export" => export(a, s, out).await,
        "import" => import(a, s, out).await,
        "backup" => backup(a, s, out).await,
        "misses" => misses_cli(a, s, out).await,
        "hook" => hook_print(a, out),
        "rules" => rules(a, out),
        "lint" => lint(a, out),
        other => Err(usage(format!("unknown command {other:?}; {USAGE}"))),
    }
}

// --- the brief and its hook --------------------------------------------------------------

fn brief_request(a: &Args) -> Result<BriefRequest, Fail> {
    Ok(BriefRequest {
        paths: a.values("path"),
        task: a.value("task").map(str::to_string),
        max_chars: a.number("max-chars")?.map(|n| n as usize),
        exclude_global: a.has("no-global"),
    })
}

async fn brief(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "path", "task", "max-chars", "no-global", "json"], 0..=0)?;
    let output = s.brief(&a.target()?, &brief_request(a)?).await?;
    emit(out, a.json(), &output, || output.markdown.clone());
    Ok(())
}

/// The hook's stdin, read on a thread so a silent pipe cannot stall the runtime;
/// a terminal is never read.
fn hook_stdin() -> tokio::sync::oneshot::Receiver<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if !std::io::stdin().is_terminal() {
        std::thread::spawn(move || {
            let mut text = String::new();
            if std::io::stdin().read_to_string(&mut text).is_ok() {
                let _ = tx.send(text);
            }
        });
    }
    rx
}

/// `brief --hook`: exit 0 always, stdout only on success, two seconds at most.
/// A JSON object on stdin with a `cwd` (Claude Code's hook input) picks the
/// directory unless `--cwd` was given; other fields are ignored.
async fn hook(argv: &[String], out: &dyn Out, s: &MemoryService, stdin: tokio::sync::oneshot::Receiver<String>) -> i32 {
    let work = async {
        let a = parse(argv)?;
        a.allow(&["cwd", "path", "task", "max-chars", "no-global", "hook"], 0..=0)?;
        let target = match a.value("cwd") {
            Some(_) => a.target()?,
            None => {
                let input = stdin.await.unwrap_or_default();
                let cwd = serde_json::from_str::<Value>(&input)
                    .ok()
                    .and_then(|v| v.get("cwd").and_then(Value::as_str).map(str::to_string));
                match cwd {
                    Some(cwd) => Target::dir(cwd),
                    None => a.target()?,
                }
            }
        };
        Ok::<_, Fail>(s.brief(&target, &brief_request(&a)?).await?.markdown)
    };
    match tokio::time::timeout(HOOK_TIMEOUT, work).await {
        Ok(Ok(markdown)) => out.line(markdown.trim_end_matches('\n')),
        Ok(Err(Fail::Usage(m) | Fail::Io(m))) => out.err(&format!("swiss memory brief --hook: {m}")),
        Ok(Err(Fail::Mem(err))) => out.err(&format!("swiss memory brief --hook: {}: {}", err.code.as_str(), err.message)),
        Err(_) => out.err("swiss memory brief --hook: gave up after 2 s"),
    }
    0
}

fn hook_print(a: &Args, out: &dyn Out) -> Run {
    a.allow(&[], 2..=2)?;
    if a.pos(0) != "print" {
        return Err(usage("usage: swiss memory hook print claude-code|codex|cursor"));
    }
    let snippet = match a.pos(1) {
        "claude-code" => {
            "Add to ~/.claude/settings.json (or the project's .claude/settings.json):\n\n\
{\n  \"hooks\": {\n    \"SessionStart\": [\n      { \"hooks\": [ { \"type\": \"command\", \"command\": \"swiss memory brief --hook\" } ] }\n    ],\n    \"SessionEnd\": [\n      { \"hooks\": [ { \"type\": \"command\", \"command\": \"swiss memory sync --nudge\" } ] }\n    ]\n  }\n}\n\n\
SessionStart prints the brief into the new session; SessionEnd nudges the gateway's sync,\n\
which reads the finished conversation (and any new commits) when the sync agent is set."
        }
        "codex" => {
            "Add to ~/.codex/AGENTS.md:\n\n\
Before starting work, run `swiss memory brief` and read its notes; they describe the past, so verify against the current code before relying on one."
        }
        "cursor" => {
            "Save as .cursor/rules/swiss-memory.mdc:\n\n\
---\ndescription: Swiss memory brief\nalwaysApply: true\n---\nBefore starting work, run `swiss memory brief` and read its notes; they describe the past, so verify against the current code before relying on one."
        }
        other => return Err(usage(format!("no hook snippet for {other:?}; pick claude-code, codex or cursor"))),
    };
    out.line(snippet);
    Ok(())
}

// --- notes --------------------------------------------------------------------------------

/// `--link KIND:ID`.
fn link_specs(a: &Args) -> Result<Vec<LinkSpec>, Fail> {
    a.values("link")
        .iter()
        .map(|raw| {
            let (kind, to) = raw.split_once(':').ok_or_else(|| usage(format!("--link {raw:?}: write KIND:ID")))?;
            let kind = LinkKind::parse(kind).ok_or_else(|| usage(format!("--link kind must be one of {}", LinkKind::choices())))?;
            Ok(LinkSpec { to: to.to_string(), kind, note: None })
        })
        .collect()
}

/// `--evidence SPEC`, repeatable (ADR-032).
fn evidence(a: &Args) -> Result<Vec<EvidenceSpec>, Fail> {
    a.values("evidence")
        .iter()
        .map(|raw| {
            EvidenceSpec::parse(raw).map_err(|e| {
                if e.message.contains("unknown type") || e.message.contains("unknown relation") {
                    usage(format!("{}; {EVIDENCE_USAGE}", e.message))
                } else {
                    Fail::Mem(e)
                }
            })
        })
        .collect()
}

/// The CLI's actor: who wrote the note is unknown to a shell unless `--producer` says so.
fn cli_actor(a: &Args) -> Result<Actor, Fail> {
    match a.value("producer") {
        None => Ok(Actor::cli()),
        Some(raw) => Producer::declared(raw)
            .map(Actor::cli_as)
            .ok_or_else(|| usage(format!("--producer must be one of {}", Producer::declarable()))),
    }
}

async fn save(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(
        &[
            "cwd", "global", "type", "title", "stdin", "anchor", "ref", "keywords", "link", "replaces", "allow-similar",
            "domain", "producer", "evidence", "json",
        ],
        0..=1,
    )?;
    let actor = cli_actor(a)?;
    let request = SaveRequest {
        kind: a.kind()?.ok_or_else(|| usage(format!("--type is required: {}", Kind::choices())))?,
        title: a.value("title").ok_or_else(|| usage("--title is required"))?.to_string(),
        content: a.text(Some(0), None)?.ok_or_else(|| usage("pass the note text or --stdin"))?,
        anchors: a.values("anchor"),
        refs: a.values("ref"),
        keywords: keywords(a).unwrap_or_default(),
        links: link_specs(a)?,
        replaces: a.value("replaces").map(str::to_string),
        allow_similar: a.has("allow-similar"),
        domain: a.value("domain").map(str::to_string),
        evidence: evidence(a)?,
    };
    let outcome = s.save(&a.target()?, &actor, request).await?;
    emit(out, a.json(), &outcome, || {
        let r = &outcome.record.summary;
        let mut text = if outcome.duplicate {
            format!("{} already holds this note; nothing written", r.id)
        } else {
            format!("saved {} (revision {}) in {} ({})", r.id, r.revision, outcome.project.name, outcome.project.id)
        };
        if let Some(domain) = r.domain.as_deref().filter(|_| !outcome.duplicate) {
            text.push_str(&format!("\nfiled in {domain}"));
        }
        if outcome.project.registered {
            text.push_str(&format!("\nregistered the project {} ({})", outcome.project.name, outcome.project.id));
        }
        for hint in &outcome.contradicts {
            text.push_str(&format!(
                "\n{} - mark it with `swiss memory mark {} outdated`, or link the two with `swiss memory link {} contradicts {}`",
                hint.message, hint.id, r.id, hint.id
            ));
        }
        for hint in &outcome.similar {
            text.push_str(&format!("\nsimilar: {} {}", hint.id, hint.title));
        }
        for hint in &outcome.formation {
            text.push_str(&format!("\nhint ({}): {}", hint.rule, hint.message));
        }
        if let Some(code) = &outcome.code {
            for finding in &code.findings {
                text.push_str(&format!("\ncode ({}): {}", finding.rule, finding.message));
            }
            if code.held {
                text.push_str(&format!(
                    "\nheld for review (needs_review): fix it with `swiss memory edit {} --revision {}`, which releases it \
                     once the check passes, or accept it with `swiss memory mark {} verified`",
                    r.id, r.revision, r.id
                ));
            }
        }
        text
    });
    Ok(())
}

// --- the code index -----------------------------------------------------------------------

/// `code index|status|find NAME|file PATH|drop [--all]` (SPEC §memory.code).
async fn code(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    match a.pos(0) {
        "index" => {
            a.allow(&["cwd", "json"], 1..=1)?;
            let r = s.code_index(&a.target()?).await?;
            emit(out, a.json(), &r, || {
                let mut text = format!(
                    "indexed {}: {} files listed, {} read, {} touched, {} unchanged, {} removed, {} skipped\n\
                     {} definitions, {} references, {} imports read in {} ms",
                    r.root, r.listed, r.read, r.touched, r.unchanged, r.removed, r.skipped, r.defs, r.refs, r.imports, r.took_ms
                );
                if r.truncated {
                    text.push_str("\nthe listing stopped at its cap: a name it misses is never called absent");
                }
                text
            });
        }
        "status" => {
            a.allow(&["cwd", "json"], 1..=1)?;
            let st = s.code_status(&a.target()?).await?;
            emit(out, a.json(), &st, || match st.indexed_ms {
                None => format!("{}: no code index; build one with `swiss memory code index`", st.root),
                Some(at) => format!(
                    "{}: {} files ({} skipped), {} definitions, {} references, {} imports; indexed {} ago\nindex file: {} bytes, {} names",
                    st.root,
                    st.files,
                    st.skipped,
                    st.defs,
                    st.refs,
                    st.imports,
                    ago(at),
                    st.bytes,
                    st.names
                ),
            });
        }
        "find" => {
            a.allow(&["cwd", "json"], 2..=2)?;
            let hits = s.code_find(&a.target()?, a.pos(1)).await?;
            emit(out, a.json(), &hits, || render::code_hits(&hits));
        }
        "file" => {
            a.allow(&["cwd", "json"], 2..=2)?;
            let f = s.code_file(&a.target()?, a.pos(1)).await?;
            emit(out, a.json(), &f, || {
                let mut text = format!("{} ({} bytes, {})", f.path, f.size, f.lang.as_deref().unwrap_or("no language"));
                if let Some(why) = &f.skipped {
                    text.push_str(&format!("\nnot read: {why}"));
                }
                for def in &f.defs {
                    text.push_str(&format!("\n{}:{} {} {}", f.path, def.line, def.kind, def.name));
                }
                for import in &f.imports {
                    text.push_str(&format!("\nimports {} (line {})", import.spec, import.line));
                }
                text.push_str(&format!("\n{} code-like names mentioned", f.refs));
                text
            });
        }
        "drop" => {
            a.allow(&["cwd", "all", "json"], 1..=1)?;
            let files = if a.has("all") { s.code_drop_all().await? } else { s.code_drop(&a.target()?).await? };
            emit(out, a.json(), &json!({ "dropped": files }), || {
                format!("dropped the code index of {files} files; no note changed")
            });
        }
        _ => return Err(usage(CODE_USAGE)),
    }
    Ok(())
}

/// `docs` (SPEC §memory.docs): the documents a project keeps whole - designs, plans,
/// pitfalls, references - named by the sources in its settings, read and carded by a docs
/// run on the gateway, searched and read here.
async fn docs_cli(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    match a.pos(0) {
        "" | "list" => {
            a.allow(&["cwd", "json"], 0..=1)?;
            let list = s.docs_list(&a.target()?).await?;
            emit(out, a.json(), &list, || render::docs_list(&list));
        }
        "add" | "remove" => {
            a.allow(&["cwd", "json"], 2..=usize::MAX)?;
            let project = sync_project_id(a, s).await?;
            let mut sources = s.project_settings(&project).await?.docs;
            for raw in &a.positional[1..] {
                let source = swiss_memory::docs::clean_source(raw).map_err(usage)?;
                match (a.pos(0), sources.iter().position(|kept| *kept == source)) {
                    ("add", None) => sources.push(source),
                    ("add", Some(_)) => {}
                    (_, Some(at)) => {
                        sources.remove(at);
                    }
                    (_, None) => return Err(usage(format!("{source} is not a docs source of this project"))),
                }
            }
            let settings = s.patch_project_settings(&project, &json!({ "docs": sources })).await?;
            let body = json!({ "project": project, "docs": settings.docs });
            emit(out, a.json(), &body, || {
                let named = if settings.docs.is_empty() { "none".to_string() } else { settings.docs.join(", ") };
                format!(
                    "{project} keeps the documents of: {named}\n\
                     `swiss memory docs sync` reads them and has the sync agent card each one"
                )
            });
        }
        "sync" => {
            a.allow(&["cwd", "json", "dry-run", "wait", "lang", "max-asks", "agent"], 1..=1)?;
            let project = sync_project_id(a, s).await?;
            let max_asks = match a.value("max-asks") {
                None => None,
                Some(raw) => match raw.trim().parse::<u64>() {
                    Ok(n) if n >= 1 => Some(n),
                    _ => return Err(usage(format!("--max-asks takes a whole number of at least 1, not {raw}"))),
                },
            };
            let body = run_body(a, &project, "docs", max_asks)?;
            let id = queue_run(a, out, &body, "docs").await?;
            if a.has("wait") {
                return wait_run(&project, &id, a.json(), out).await;
            }
        }
        "search" => {
            a.allow(&["cwd", "json", "kind", "limit"], 2..=usize::MAX)?;
            let kind = match a.value("kind") {
                None => None,
                Some(raw) => Some(
                    swiss_memory::docs::DocKind::parse(raw)
                        .ok_or_else(|| usage(format!("--kind must be one of {}", swiss_memory::docs::DocKind::choices())))?,
                ),
            };
            let limit = match a.value("limit") {
                None => swiss_memory::docs::SEARCH_DEFAULT,
                Some(raw) => raw.trim().parse::<usize>().map_err(|_| usage(format!("--limit takes a whole number, not {raw}")))?,
            };
            let hits = s.docs_search(&a.target()?, &a.positional[1..].join(" "), limit, kind).await?;
            emit(out, a.json(), &hits, || render::doc_hits(&hits));
        }
        "read" => {
            a.allow(&["cwd", "json", "section"], 2..=2)?;
            let section = match a.value("section") {
                None => None,
                Some(raw) => Some(raw.trim().parse::<u32>().map_err(|_| usage(format!("--section takes a section number, not {raw}")))?),
            };
            let view = s.doc_view(&a.target()?, a.pos(1), section).await?;
            emit(out, a.json(), &view, || render::doc_view(&view));
        }
        _ => return Err(usage(DOCS_USAGE)),
    }
    Ok(())
}

/// A past moment as a short age.
fn ago(at_ms: i64) -> String {
    let secs = ((swiss_core::util::now_ms() as i64 - at_ms) / 1000).max(0);
    match secs {
        0..=89 => format!("{secs} s"),
        90..=5399 => format!("{} min", secs / 60),
        5400..=129_599 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// `rules [--bootstrap]`: what a note is (ADR-032), for a writer outside swiss - the
/// note rules, or the notes half of a bootstrap prompt.
fn rules(a: &Args, out: &dyn Out) -> Run {
    a.allow(&["bootstrap"], 0..=0)?;
    let text = if a.has("bootstrap") { formation::bootstrap_rules() } else { formation::NOTE_RULES.to_string() };
    out.line(text.trim_end());
    Ok(())
}

/// `lint [--type K] [--title T] TEXT|--stdin`: the formation lint a save answers with,
/// without saving. Advisory: it exits 0 whatever it finds.
fn lint(a: &Args, out: &dyn Out) -> Run {
    a.allow(&["type", "title", "stdin", "json"], 0..=1)?;
    let kind = a.kind()?.unwrap_or(Kind::Knowledge);
    let content = a.text(Some(0), None)?.ok_or_else(|| usage("pass the note text or --stdin"))?;
    let mut found = formation::lint(kind, a.value("title").unwrap_or_default(), &content);
    found.extend(formation::user_words(kind, &content));
    emit(out, a.json(), &json!({ "formation": found }), || {
        if found.is_empty() {
            return "the note reads as one claim".into();
        }
        found.iter().map(|l| format!("hint ({}): {}", l.rule, l.message)).collect::<Vec<_>>().join("\n")
    });
    Ok(())
}

async fn search(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "type", "limit", "history", "domain", "json"], 1..=usize::MAX)?;
    let query = SearchQuery {
        text: a.positional.join(" "),
        kinds: a.kind()?.into_iter().collect(),
        include_history: a.has("history"),
        limit: a.number("limit")?.map(|n| n as usize).unwrap_or(swiss_memory::search::DEFAULT_LIMIT),
        domain: a.value("domain").map(str::to_string),
        words_only: false,
    };
    let hits = s.search(&a.target()?, &query).await?;
    emit(out, a.json(), &json!({ "items": hits }), || render::hits(&hits));
    Ok(())
}

async fn related(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "text", "stdin", "depth", "limit", "history", "json"], 0..=1)?;
    let seed = match (a.positional.first(), a.text(None, Some("text"))?) {
        (Some(id), None) => Seed::Id(id.clone()),
        (None, Some(text)) => Seed::Text(text),
        _ => return Err(usage("related takes an ID, --text T or --stdin")),
    };
    let request = RelatedRequest {
        seed,
        depth: a.number("depth")?.map(|n| n.min(u32::MAX as u64) as u32),
        limit: a.number("limit")?.map(|n| n as usize),
        include_history: a.has("history"),
    };
    let outcome = s.related(&a.target()?, &request).await?;
    emit(out, a.json(), &json!({ "items": outcome.items }), || render::related(&outcome.items));
    Ok(())
}

/// `ask` (SPEC §memory.rag): one question answered from the project's memory by
/// the agent its layers name. Through the gateway - it needs the llm - over the
/// same API the panel and MCP use. `--worktree` names the working tree the code is
/// read in: a branch one has checked out, or a path - a relative one (`.`, `../wt`)
/// is this shell's, so the gateway gets it absolute.
async fn ask(a: &Args, _s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "agent", "worktree", "json"], 0..=1)?;
    let Some(question) = a.positional.first().filter(|q| !q.trim().is_empty()) else {
        return Err(usage("ask takes a question: swiss memory ask \"what did we decide about retries?\""));
    };
    let (cwd, _) = a.dir()?;
    let mut body = json!({ "cwd": cwd, "question": question.trim() });
    if a.has("global") {
        body["scope"] = json!("global");
    }
    if let Some(agent) = a.value("agent").map(str::trim).filter(|n| !n.is_empty()) {
        if agent == "off" {
            return Err(usage("--agent names the one to answer; leave it out for the layers"));
        }
        choice_flag("agent", agent, true, false)?; // the same rule the gateway checks with
        body["agent"] = json!(agent);
    }
    if let Some(worktree) = a.value("worktree").map(str::trim).filter(|w| !w.is_empty()) {
        let dotted = worktree == "." || worktree == ".." || ["./", ".\\", "../", "..\\"].iter().any(|p| worktree.starts_with(p));
        body["worktree"] = if dotted {
            let here = std::env::current_dir().map_err(|e| Fail::Io(format!("the current directory is unreadable: {e}")))?;
            json!(here.join(worktree).display().to_string())
        } else {
            json!(worktree)
        };
    }
    let (status, value) = gateway_call_within(reqwest::Method::POST, "/api/knowledge/ask", Some(&body), ASK_WAIT).await?;
    if status != 200 {
        return Err(Fail::Io(gateway_message(&value, status)));
    }
    emit(out, a.json(), &value, || {
        let agent = value.get("agent").and_then(Value::as_str).unwrap_or_default();
        format!("{}\n\n— {agent}", value.get("answer").and_then(Value::as_str).unwrap_or_default())
    });
    Ok(())
}

async fn graph(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "depth", "limit", "history", "json"], 0..=1)?;
    let request = GraphRequest {
        center: a.positional.first().cloned(),
        depth: a.number("depth")?.map(|n| n.min(u32::MAX as u64) as u32),
        include_history: a.has("history"),
        limit: a.number("limit")?.map(|n| n as usize),
    };
    let view = s.graph(&a.target()?, &request).await?;
    emit(out, a.json(), &view, || render::graph(&view));
    Ok(())
}

async fn list(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "type", "status", "unconfirmed", "history", "limit", "domain", "json"], 0..=0)?;
    let request = ListRequest {
        kinds: a.list("type", Kind::parse, Kind::choices)?,
        statuses: a.list("status", Status::parse, Status::choices)?,
        unconfirmed: a.has("unconfirmed"),
        history: a.has("history"),
        limit: a.number("limit")?.map(|n| n as usize),
        domain: a.value("domain").map(str::to_string),
    };
    let (items, truncated) = s.list(&a.target()?, &request).await?;
    emit(out, a.json(), &json!({ "items": items, "truncated": truncated }), || {
        let mut text = render::summaries(&items);
        if truncated {
            text.push_str("(more: narrow with --type/--status/--domain or raise --limit)\n");
        }
        text
    });
    Ok(())
}

async fn show(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "history", "json"], 1..=1)?;
    let (record, revisions) = s.get(&a.target()?, a.pos(0), a.has("history")).await?;
    let mut body = json!({ "record": record });
    if let Some(revisions) = &revisions {
        body["revisions"] = json!(revisions);
    }
    emit(out, a.json(), &body, || render::record(&record, revisions.as_deref()));
    Ok(())
}

async fn edit(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(
        &[
            "cwd", "global", "revision", "title", "stdin", "content", "type", "anchor", "ref", "keywords", "reason", "domain",
            "evidence", "json",
        ],
        1..=1,
    )?;
    let anchors = a.values("anchor");
    let refs = a.values("ref");
    let content = a.text(None, Some("content"))?;
    let proofs = evidence(a)?;
    // Filing and evidence alone leave the text as it is: they need no revision.
    let filing_only = (a.value("domain").is_some() || !proofs.is_empty())
        && content.is_none()
        && [a.value("title"), a.value("type")].iter().all(Option::is_none)
        && anchors.is_empty()
        && refs.is_empty()
        && a.value("keywords").is_none();
    let revision = match a.number("revision")? {
        Some(revision) => revision,
        None if filing_only => 0,
        None => return Err(usage("--revision is required (the revision you read)")),
    };
    let request = UpdateRequest {
        expected_revision: revision,
        kind: a.kind()?,
        title: a.value("title").map(str::to_string),
        content,
        anchors: (!anchors.is_empty()).then_some(anchors),
        refs: (!refs.is_empty()).then_some(refs),
        keywords: keywords(a),
        reason: a.value("reason").map(str::to_string),
        domain: a.value("domain").map(str::to_string),
        evidence: proofs,
    };
    let outcome = s.update_checked(&a.target()?, &Actor::cli(), a.pos(0), request).await?;
    let r = &outcome.record.summary;
    emit(out, a.json(), &outcome, || {
        let mut text = format!("{} now at revision {} ({})", r.id, r.revision, r.status.as_str());
        if let Some(code) = &outcome.code {
            for finding in &code.findings {
                text.push_str(&format!("\ncode ({}): {}", finding.rule, finding.message));
            }
        }
        text
    });
    Ok(())
}

async fn link(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let removing = a.command == "unlink";
    if removing {
        a.allow(&["cwd", "global", "json"], 3..=3)?;
    } else {
        a.allow(&["cwd", "global", "note", "json"], 3..=3)?;
    }
    let (from, kind, to) = (a.pos(0), a.pos(1), a.pos(2));
    let kind = LinkKind::parse(kind).ok_or_else(|| usage(format!("KIND must be one of {}", LinkKind::choices())))?;
    let target = a.target()?;
    let changed = if removing {
        s.unlink(&target, from, to, kind).await?
    } else {
        s.link(&target, &Actor::cli(), from, to, kind, a.value("note")).await?.1
    };
    let body = json!({ "from": from, "to": to, "kind": kind, "changed": changed });
    emit(out, a.json(), &body, || match (removing, changed) {
        (false, true) => format!("linked {from} {} {to}", kind.as_str()),
        (false, false) => format!("{from} {} {to} was already linked", kind.as_str()),
        (true, true) => format!("unlinked {from} {} {to}", kind.as_str()),
        (true, false) => format!("no {} link between {from} and {to}", kind.as_str()),
    });
    Ok(())
}

async fn mark(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "reason", "evidence", "json"], 2..=2)?;
    let verdict = Verdict::parse(a.pos(1)).ok_or_else(|| usage(format!("the verdict must be one of {}", Verdict::choices())))?;
    let record = s.mark_with(&a.target()?, &Actor::cli(), a.pos(0), verdict, a.value("reason"), evidence(a)?).await?;
    let r = &record.summary;
    emit(out, a.json(), &json!({ "record": record }), || {
        format!("{} is {} (revision {})", r.id, r.status.as_str(), r.revision)
    });
    Ok(())
}

async fn confirm_or_restore(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let record = if a.command == "confirm" {
        a.allow(&["cwd", "global", "json"], 1..=1)?;
        s.confirm(&a.target()?, &Actor::cli(), a.pos(0)).await?
    } else {
        a.allow(&["cwd", "global", "reason", "json"], 1..=1)?;
        let reason = a.value("reason").ok_or_else(|| usage("--reason is required"))?;
        s.restore(&a.target()?, &Actor::cli(), a.pos(0), reason).await?
    };
    let r = &record.summary;
    emit(out, a.json(), &json!({ "record": record }), || {
        format!("{} is {}{} (revision {})", r.id, r.status.as_str(), if r.confirmed { ", confirmed" } else { "" }, r.revision)
    });
    Ok(())
}

async fn review(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    if a.has("resolve") {
        return review_resolve(a, s, out).await;
    }
    if ["changed", "dry-run", "wait", "lang", "max-asks", "agent"].iter().any(|flag| a.has(flag) || a.value(flag).is_some()) {
        return Err(usage(format!("--changed, --dry-run, --wait, --lang, --max-asks and --agent go with --resolve; {REVIEW_USAGE}")));
    }
    a.allow(&["cwd", "global", "json"], 0..=0)?;
    let items = s.review(&a.target()?).await?;
    emit(out, a.json(), &json!({ "items": items }), || {
        if items.is_empty() {
            "Nothing to review.".into()
        } else {
            format!(
                "{}\nconfirm: swiss memory confirm ID · judge: swiss memory mark ID verified|outdated|wrong --reason R\n\
                 or hand the notes a model wrote to the sync agent: swiss memory review --resolve",
                render::summaries(&items)
            )
        }
    });
    Ok(())
}

/// `review --resolve` (SPEC §memory.init The recheck): the review inbox's notes a model
/// wrote handed to the sync agent - judged against the code, confirmed ones too; what the
/// code bears out is confirmed, the contradictions settled side by side, and what the code
/// check still holds left for a person. `--changed` hands over only the notes whose
/// anchored files changed; `--dry-run` says how many questions it would ask; `--wait`
/// follows the run to its end.
async fn review_resolve(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "json", "resolve", "changed", "dry-run", "wait", "lang", "max-asks", "agent"], 0..=0)?;
    let max_asks = match a.value("max-asks") {
        None => None,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n >= 1 => Some(n),
            _ => return Err(usage(format!("--max-asks takes a whole number of at least 1, not {raw}"))),
        },
    };
    let project = sync_project_id(a, s).await?;
    let mut body = run_body(a, &project, "review", max_asks)?;
    if a.has("changed") {
        body["changed"] = json!(true);
    }
    let id = queue_run(a, out, &body, "review").await?;
    if a.has("wait") {
        return wait_run(&project, &id, a.json(), out).await;
    }
    Ok(())
}

async fn purge(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "confirm", "json"], 1..=1)?;
    let confirm = a.value("confirm").ok_or_else(|| usage("purge is permanent: repeat the id with --confirm ID"))?;
    s.purge(&a.target()?, a.pos(0), confirm).await?;
    emit(out, a.json(), &json!({ "deleted": true }), || format!("{} purged", a.pos(0)));
    Ok(())
}

// --- domains ------------------------------------------------------------------------------

/// `--owns A,B`: repo-relative paths or globs, comma-separated.
fn owns(a: &Args) -> Option<Vec<String>> {
    a.value("owns").map(|raw| raw.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect())
}

/// `--keywords refund,退款`: a comma list (an ideographic comma too); `--keywords ""` clears them.
fn keywords(a: &Args) -> Option<Vec<String>> {
    a.value("keywords").map(|raw| raw.split([',', '，', '、']).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect())
}

/// `domain …` (SPEC §memory.domains): the map, one domain's hub and notes, and the
/// writes. `--stdin` is the hub's body.
async fn domain(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let actor = Actor::cli();
    let view = match a.pos(0) {
        "list" => {
            a.allow(&["cwd", "json"], 1..=1)?;
            let list = s.domains(&a.target()?).await?;
            emit(out, a.json(), &list, || render::map(&list));
            return Ok(());
        }
        "show" => {
            a.allow(&["cwd", "history", "json"], 2..=2)?;
            let detail = s.domain(&a.target()?, a.pos(1), a.has("history")).await?;
            emit(out, a.json(), &json!({ "domain": detail }), || render::domain(&detail));
            return Ok(());
        }
        "add" => {
            a.allow(&["cwd", "title", "summary", "owns", "keywords", "stdin", "json"], 2..=2)?;
            let request = DomainRequest {
                path: a.pos(1).to_string(),
                title: a.value("title").ok_or_else(|| usage("--title is required"))?.to_string(),
                summary: a.value("summary").map(str::to_string),
                body: a.text(None, None)?,
                owns: owns(a).unwrap_or_default(),
                keywords: keywords(a).unwrap_or_default(),
            };
            s.create_domain(&a.target()?, &actor, request).await?
        }
        "edit" => {
            a.allow(&["cwd", "revision", "path", "title", "summary", "owns", "keywords", "stdin", "reason", "json"], 2..=2)?;
            if a.values("path").len() > 1 {
                return Err(usage("--path is given twice"));
            }
            let request = DomainUpdate {
                expected_revision: a.number("revision")?.ok_or_else(|| usage("--revision is required (the revision you read)"))?,
                path: a.value("path").map(str::to_string),
                title: a.value("title").map(str::to_string),
                summary: a.value("summary").map(str::to_string),
                body: a.text(None, None)?,
                owns: owns(a),
                keywords: keywords(a),
                reason: a.value("reason").map(str::to_string),
            };
            s.update_domain(&a.target()?, &actor, a.pos(1), request).await?
        }
        "confirm" => {
            a.allow(&["cwd", "all", "json"], 2..=2)?;
            if a.has("all") {
                let done = s.confirm_tree(&a.target()?, &actor, a.pos(1)).await?;
                emit(out, a.json(), &done, || {
                    let mut line = format!("{} confirmed with what is under it - domains: {}, notes: {}", done.domain.path, done.domains, done.notes);
                    if done.held > 0 {
                        line.push_str(&format!(
                            "
{} need review first - judge them: swiss memory review, then swiss memory mark ID verified|outdated|wrong",
                            done.held
                        ));
                    }
                    line
                });
                return Ok(());
            }
            s.confirm_domain(&a.target()?, &actor, a.pos(1)).await?
        }
        "verify" => {
            a.allow(&["cwd", "json"], 2..=2)?;
            s.verify_domain(&a.target()?, &actor, a.pos(1)).await?
        }
        "delete" => {
            a.allow(&["cwd", "confirm", "json"], 2..=2)?;
            let confirm = a.value("confirm").ok_or_else(|| usage("deleting a domain is permanent: repeat its path with --confirm PATH"))?;
            let outcome = s.delete_domain(&a.target()?, a.pos(1), confirm).await?;
            emit(out, a.json(), &outcome, || match &outcome.to {
                Some(parent) => format!("deleted {}; {} notes moved to {parent}", outcome.path, outcome.refiled),
                None => format!("deleted {}; {} notes left unfiled", outcome.path, outcome.refiled),
            });
            return Ok(());
        }
        "file" => {
            a.allow(&["cwd", "to", "none", "json"], 2..=usize::MAX)?;
            let to = match (a.value("to"), a.has("none")) {
                (Some(to), false) => Some(to),
                (None, true) => None,
                _ => return Err(usage("file takes --to PATH or --none")),
            };
            let outcome = s.file(&a.target()?, &a.positional[1..], to).await?;
            emit(out, a.json(), &outcome, || match &outcome.domain {
                Some(d) => format!("{} notes filed in {}", outcome.moved, d.path),
                None => format!("{} notes unfiled", outcome.moved),
            });
            return Ok(());
        }
        "autofile" => {
            a.allow(&["cwd", "json"], 1..=1)?;
            let filed = s.autofile(&a.target()?).await?;
            emit(out, a.json(), &json!({ "filed": filed }), || format!("{filed} notes filed by their anchors"));
            return Ok(());
        }
        _ => return Err(usage(DOMAIN_USAGE)),
    };
    emit(out, a.json(), &json!({ "domain": view }), || {
        let mark = if view.confirmed { "" } else { ", unconfirmed" };
        format!("{} (revision {}{mark})\n{}", view.path, view.revision, view.line())
    });
    Ok(())
}

// --- projects -----------------------------------------------------------------------------

async fn init(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let flags = [
        "cwd", "here", "portable", "name", "json", "map", "full", "recheck", "redraw", "plan", "dry-run", "wait", "lang",
        "max-asks", "agent", "lead",
    ];
    a.allow(&flags, 0..=0)?;
    let (map, full, recheck) = (a.has("map"), a.has("full"), a.has("recheck"));
    if usize::from(map) + usize::from(full) + usize::from(recheck) > 1 {
        return Err(usage(format!("--map, --full and --recheck are three runs: pick one; {INIT_USAGE}")));
    }
    let picks_given = a.value("agent").is_some() || a.value("lead").is_some();
    let run_flags =
        a.has("plan") || a.has("dry-run") || a.has("wait") || a.has("redraw") || a.value("lang").is_some() || picks_given;
    if !map && !full && !recheck && run_flags {
        return Err(usage(format!(
            "--plan, --dry-run, --wait, --lang, --redraw, --agent and --lead go with --map, --full or --recheck; {INIT_USAGE}"
        )));
    }
    if recheck && a.value("lead").is_some() {
        return Err(usage(format!("--lead goes with --map or --full: only they hand judgment to a lead; {INIT_USAGE}")));
    }
    if recheck && (a.has("plan") || a.has("redraw")) {
        return Err(usage(format!(
            "--plan and --redraw go with --map or --full: a recheck draws no map, and --dry-run is its preview; {INIT_USAGE}"
        )));
    }
    if a.value("max-asks").is_some() && !full && !recheck {
        return Err(usage(format!(
            "--max-asks goes with --full or --recheck: only they ask per slice or per batch; {INIT_USAGE}"
        )));
    }
    if a.has("plan") && (a.has("dry-run") || a.has("wait") || a.value("lang").is_some() || picks_given) {
        return Err(usage(format!(
            "--plan asks no model: it takes no --dry-run, --wait, --lang, --agent or --lead; {INIT_USAGE}"
        )));
    }
    let max_asks = match a.value("max-asks") {
        None => None,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n >= 1 => Some(n),
            _ => return Err(usage(format!("--max-asks takes a whole number of at least 1, not {raw}"))),
        },
    };
    // A misnamed agent is refused before init registers anything.
    picks(a, &mut json!({}))?;
    let mode = match (a.has("here"), a.has("portable")) {
        (true, true) => return Err(usage("--here and --portable exclude each other")),
        (true, false) => InitMode::Here,
        (false, true) => InitMode::Portable,
        (false, false) => InitMode::Auto,
    };
    let (cwd, git_dir) = a.dir()?;
    let outcome = s.init(&cwd, git_dir.as_deref(), mode, a.value("name")).await?;
    if map || full || recheck {
        if !a.json() {
            out.line(&init_line(&outcome));
        }
        if full {
            return init_full(a, out, &outcome.project.id, max_asks).await;
        }
        if recheck {
            return init_recheck(a, out, &outcome.project.id, max_asks).await;
        }
        return init_map(a, s, out, &outcome.project.id).await;
    }
    emit(out, a.json(), &outcome, || init_line(&outcome));
    Ok(())
}

/// What `init` says it did.
fn init_line(outcome: &swiss_memory::InitOutcome) -> String {
    let p = &outcome.project;
    let mut text = if outcome.created {
        format!("registered {} ({}) at {}", p.name, p.id, outcome.identity)
    } else {
        format!("{} ({}) already holds {}", p.name, p.id, outcome.identity)
    };
    if let Some(marker) = &outcome.marker_file {
        text.push_str(&format!("\nwrote {marker}: commit it so every clone joins this project"));
    }
    text
}

/// `init --map` (SPEC §memory.map): `--plan` prints the areas the checkout would be cut
/// into, offline and asking no model (`--redraw`: with the domains an earlier map drew and
/// nobody touched set aside); otherwise a map run is queued on the gateway, whose worker
/// has the sync agent review the top level, describe each area, and draws the domains.
async fn init_map(a: &Args, s: &MemoryService, out: &dyn Out, project: &str) -> Run {
    if a.has("plan") {
        let removed = if a.has("redraw") { s.map_redraw_plan(project).await? } else { Vec::new() };
        let draw = swiss_memory::survey::Draw { regroup: None, ignore: removed.clone() };
        let survey = s.map_survey(project, &draw).await?;
        let review = survey.review_prompt(None).is_some();
        let plan = json!({
            "project": project,
            "plan": survey.plan(),
            "asks": survey.asks(),
            "review": review,
            "removed": removed,
            "codeFiles": survey.code_files,
            "ownedFiles": survey.owned_files,
            "mappedFiles": survey.mapped_files,
            "truncated": survey.truncated,
        });
        emit(out, a.json(), &plan, || {
            let mut text = String::new();
            if !removed.is_empty() {
                text.push_str(&format!("the redraw removes {} domain(s) an earlier map drew: {}\n", removed.len(), removed.join(", ")));
            }
            text.push_str(&survey.outline());
            if review {
                text.push_str("the top level goes to the sync agent first, to be grouped into business domains\n");
            }
            text.push_str(&format!(
                "{} area(s) to describe: `swiss memory init --map` asks the sync agent one question each",
                survey.asks()
            ));
            text
        });
        return Ok(());
    }
    let body = run_body(a, project, "map", None)?;
    let id = queue_run(a, out, &body, "map").await?;
    if a.has("wait") {
        return wait_run(project, &id, a.json(), out).await;
    }
    Ok(())
}

/// `init --recheck` (SPEC §memory.init The recheck): the notes a model wrote and nobody
/// judged since, a domain at a time, put back to the sync agent against the code - kept,
/// fixed, merged or dropped, and linked. `--dry-run` says how many questions it would ask;
/// `--wait` follows the run to its end.
async fn init_recheck(a: &Args, out: &dyn Out, project: &str, max_asks: Option<u64>) -> Run {
    let body = run_body(a, project, "recheck", max_asks)?;
    let id = queue_run(a, out, &body, "recheck").await?;
    if a.has("wait") {
        return wait_run(project, &id, a.json(), out).await;
    }
    Ok(())
}

/// The body queuing a map, full, recheck, docs or review run (`kind`) from its command's flags.
fn run_body(a: &Args, project: &str, kind: &str, max_asks: Option<u64>) -> Result<Value, Fail> {
    let mut body = json!({ "project": project, kind: true });
    picks(a, &mut body)?;
    if a.has("dry-run") {
        body["dryRun"] = json!(true);
    }
    if a.has("redraw") {
        body["redraw"] = json!(true);
    }
    if let Some(lang) = a.value("lang") {
        body["language"] = json!(lang);
    }
    if let Some(n) = max_asks {
        body["maxAsks"] = json!(n);
    }
    Ok(body)
}

/// `--agent` and `--lead` into a run's body as `agents` and `lead` (SPEC §memory.settings).
fn picks(a: &Args, body: &mut Value) -> Run {
    if let Some(raw) = a.value("agent") {
        body["agents"] = choice_flag("agent", raw, false, false)?;
    }
    if let Some(raw) = a.value("lead") {
        body["lead"] = choice_flag("lead", raw, true, false)?;
    }
    Ok(())
}

/// A `--agent`/`--lead` value as the API takes it: `off`, one name, or (`one` refuses it) a
/// comma list; `inherit` is null - the layer below decides - where `inherit` allows it (a
/// project's settings: a run inherits by leaving the flag out). Checked here by the rule
/// the gateway checks it with, so a typo is a usage error before anything is sent.
fn choice_flag(flag: &str, raw: &str, one: bool, inherit: bool) -> Result<Value, Fail> {
    let raw = raw.trim();
    let value = match raw {
        "inherit" if inherit => return Ok(Value::Null),
        "inherit" => return Err(usage(format!("a run inherits by leaving --{flag} out"))),
        "off" => json!("off"),
        _ if one => json!(raw),
        _ => json!(raw.split(',').map(str::trim).collect::<Vec<_>>()),
    };
    if one && raw.contains(',') {
        return Err(usage(format!("--{flag} names one agent, not {raw:?}")));
    }
    Choice::parse(Some(&value), &format!("--{flag}"), one).map_err(usage)?;
    Ok(value)
}

/// Queue the run on the gateway and say so; the id of the run it queued or merged into.
async fn queue_run(a: &Args, out: &dyn Out, body: &Value, kind: &str) -> Result<String, Fail> {
    let (status, value) = gateway_call(reqwest::Method::POST, "/api/knowledge/sync", Some(body)).await?;
    if status != 202 {
        return Err(Fail::Io(gateway_message(&value, status)));
    }
    let run = value.get("run").cloned().unwrap_or(Value::Null);
    let id = run.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
    if a.json() {
        out.line(&run.to_string());
    } else if value.get("merged").and_then(Value::as_bool) == Some(true) {
        out.line(&format!("a {kind} run is already queued; merged into it"));
    } else {
        out.line(&format!("queued {kind} run {id}; `swiss memory sync status` follows it"));
    }
    Ok(id)
}

/// `init --full` (SPEC §memory.init): the map, then the project's code and docs read in
/// slices, the sync agent asked what each teaches, the notes saved - with the semantic
/// index when an embedding model is set, lexical search only without. `--plan` asks the
/// gateway what the run would read (no model asked); `--wait` follows the run, then the
/// index embedding the new notes.
async fn init_full(a: &Args, out: &dyn Out, project: &str, max_asks: Option<u64>) -> Run {
    if a.has("plan") {
        let mut path = format!("/api/knowledge/sync/full?project={project}");
        if a.has("redraw") {
            path.push_str("&redraw=1");
        }
        if let Some(n) = max_asks {
            path.push_str(&format!("&maxAsks={n}"));
        }
        let (status, value) = gateway_call(reqwest::Method::GET, &path, None).await?;
        if status != 200 {
            return Err(Fail::Io(gateway_message(&value, status)));
        }
        emit(out, a.json(), &value, || full_plan_text(&value));
        return Ok(());
    }
    let body = run_body(a, project, "full", max_asks)?;
    let id = queue_run(a, out, &body, "full").await?;
    if !a.has("wait") {
        return Ok(());
    }
    let run = wait_for(project, &id).await?;
    say_run(&run, &id, a.json(), out);
    if a.json() || run.get("dryRun").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    if let Some(knowledge) = run.pointer("/scope/knowledge") {
        out.line(&knowledge_line(knowledge));
    }
    follow_embedding(out).await
}

/// What `init --full --plan` prints.
fn full_plan_text(plan: &Value) -> String {
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut text = String::new();
    if let Some(removed) = plan.get("removed").and_then(Value::as_array).filter(|r| !r.is_empty()) {
        let names: Vec<&str> = removed.iter().filter_map(Value::as_str).collect();
        text.push_str(&format!("the redraw removes {} domain(s) an earlier map drew: {}\n", names.len(), names.join(", ")));
    }
    for unit in plan.get("plan").and_then(Value::as_array).into_iter().flatten() {
        let path = unit.get("path").and_then(Value::as_str).unwrap_or_default();
        text.push_str(&format!("- {path} ({} file(s))\n", n(unit, "files")));
    }
    if plan.get("review").and_then(Value::as_bool) == Some(true) {
        text.push_str("the top level goes to the sync agent first, to be grouped into business domains\n");
    }
    text.push_str(&format!("map: {} area(s) to describe\n", n(plan, "asks")));
    let k = &plan["knowledge"];
    text.push_str(&format!(
        "knowledge: {} file(s), {} byte(s) in {} slice(s): {} question(s), {} unchanged since the last full run, {} for a later run\n",
        n(k, "files"),
        n(k, "bytes"),
        n(k, "slices"),
        n(k, "asks"),
        n(k, "unchanged"),
        n(k, "more")
    ));
    text.push_str(&embedding_line(&plan["embedding"]));
    text
}

/// A full run's knowledge counts.
fn knowledge_line(k: &Value) -> String {
    let n = |key: &str| k.get(key).and_then(Value::as_u64).unwrap_or(0);
    format!(
        "knowledge: {} of {} slice(s) asked ({} unchanged, {} for a later run, {} failed); {} note(s) saved, {} held for review, {} the project had, {} refused",
        n("asked"),
        n("slices"),
        n("unchanged"),
        n("more"),
        n("failed"),
        n("saved"),
        n("held"),
        n("duplicates"),
        n("refused")
    )
}

/// The semantic index in one line: off is lexical search only.
fn embedding_line(e: &Value) -> String {
    let state = e.get("state").and_then(Value::as_str).unwrap_or("off");
    if state == "off" {
        return "embedding: off - notes are found by their words only; set an embedding model on the llm plugin's Embedding page for semantic search".into();
    }
    let model = e.get("model").and_then(Value::as_str).unwrap_or("?");
    let n = |key: &str| e.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut line = format!("embedding: {state} ({model}) - {} indexed, {} pending", n("indexed"), n("pending"));
    if let Some(error) = e.get("lastError").or_else(|| e.get("error")).and_then(Value::as_str) {
        line.push_str(&format!("; last error: {error}"));
    }
    line
}

/// After a full run: follow the index embedding its notes, a poll every two seconds for
/// ten minutes at most; it keeps going in the gateway after the CLI stops following.
async fn follow_embedding(out: &dyn Out) -> Run {
    let mut last = Value::Null;
    for _ in 0..300 {
        let (status, value) = gateway_call(reqwest::Method::GET, "/api/knowledge/embedding", None).await?;
        if status != 200 {
            return Err(Fail::Io(gateway_message(&value, status)));
        }
        last = value.get("status").cloned().unwrap_or(Value::Null);
        let state = last.get("state").and_then(Value::as_str).unwrap_or("off");
        let pending = last.get("pending").and_then(Value::as_u64).unwrap_or(0);
        if state == "off" || state == "error" || pending == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    out.line(&embedding_line(&last));
    Ok(())
}

fn project_lines(projects: &[swiss_memory::ProjectView]) -> String {
    let mut text = String::new();
    for p in projects {
        let c = &p.counts;
        text.push_str(&format!(
            "- {} {} · {} live · {} needs review · {} unconfirmed\n",
            p.id, p.name, c.live, c.needs_review, c.unconfirmed
        ));
        for root in &p.roots {
            let reach = if root.reachable { "" } else { ", unreachable" };
            text.push_str(&format!("  {} ({}{reach}) · {}\n", root.path, root.kind, root.identity));
        }
    }
    if text.is_empty() {
        text.push_str("No projects.\n");
    }
    text
}

async fn project(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let sub = a.pos(0);
    let view = match sub {
        "list" => {
            a.allow(&["json"], 1..=1)?;
            let projects = s.projects().await?;
            emit(out, a.json(), &json!({ "projects": projects }), || project_lines(&projects));
            return Ok(());
        }
        "add" => {
            a.allow(&["path", "exact", "json"], 2..=usize::MAX)?;
            let name = a.positional[1..].join(" ");
            s.create_project(Some(&name), a.value("path"), a.has("exact"), None).await?
        }
        "bind" => {
            a.allow(&["id", "new-project", "cwd", "exact", "json"], 1..=1)?;
            let (path, git_dir) = a.dir()?;
            match (a.value("id"), a.has("new-project")) {
                (Some(id), false) => s.bind_root(id, &path, a.has("exact"), git_dir.as_deref()).await?,
                (None, true) => s.create_project(None, Some(&path), a.has("exact"), git_dir.as_deref()).await?,
                _ => return Err(usage("bind takes --id P or --new-project")),
            }
        }
        "rebind" => {
            a.allow(&["cwd", "exact", "json"], 3..=3)?;
            let (path, git_dir) = a.dir()?;
            s.rebind_root(a.pos(1), a.pos(2), &path, a.has("exact"), git_dir.as_deref()).await?
        }
        "unbind" => {
            a.allow(&["json"], 3..=3)?;
            s.unbind_root(a.pos(1), a.pos(2)).await?
        }
        "rename" => {
            a.allow(&["json"], 3..=usize::MAX)?;
            s.rename_project(a.pos(1), &a.positional[2..].join(" ")).await?
        }
        "settings" => {
            a.allow(&["id", "cwd", "agent", "lead", "embedding", "auto-review", "auto-review-runs", "json"], 1..=1)?;
            let id = match a.value("id") {
                Some(id) => id.to_string(),
                None => s.resolve(&a.target()?).await?.map(|p| p.id).ok_or_else(|| {
                    usage("this directory is not a registered project; pass --id P or run `swiss memory init` first")
                })?,
            };
            let mut patch = serde_json::Map::new();
            if let Some(raw) = a.value("agent") {
                patch.insert("agents".into(), choice_flag("agent", raw, false, true)?);
            }
            if let Some(raw) = a.value("lead") {
                patch.insert("lead".into(), choice_flag("lead", raw, true, true)?);
            }
            if let Some(raw) = a.value("embedding") {
                patch.insert("embedding".into(), match raw.trim() {
                    "on" => json!(true),
                    "off" => json!(false),
                    "inherit" => Value::Null,
                    _ => return Err(usage("--embedding takes on, off or inherit")),
                });
            }
            // A daily review of the notes whose code changed, at HH:MM; off queues none.
            if let Some(raw) = a.value("auto-review") {
                patch.insert("autoReview".into(), match raw.trim() {
                    "off" => Value::Null,
                    slot if swiss_memory::settings::slot_of(slot).is_some() => json!(slot),
                    _ => return Err(usage("--auto-review takes a time of day HH:MM (00:00 to 23:59) or off")),
                });
            }
            // The review runs that day may take, each after one that left questions for the next.
            if let Some(raw) = a.value("auto-review-runs") {
                use swiss_memory::settings::{AUTO_REVIEW_RUNS_DEFAULT, AUTO_REVIEW_RUNS_MAX};
                patch.insert("autoReviewRuns".into(), match raw.trim() {
                    "default" => Value::Null,
                    n => match n.parse::<usize>() {
                        Ok(runs) if (1..=AUTO_REVIEW_RUNS_MAX).contains(&runs) => json!(runs),
                        _ => {
                            return Err(usage(format!(
                                "--auto-review-runs takes 1 to {AUTO_REVIEW_RUNS_MAX}, or default ({AUTO_REVIEW_RUNS_DEFAULT})"
                            )))
                        }
                    },
                });
            }
            // No field given reads the settings; any given is a merge patch.
            let settings = match patch.is_empty() {
                true => s.project_settings(&id).await?,
                false => s.patch_project_settings(&id, &Value::Object(patch)).await?,
            };
            let body = json!({ "project": id, "settings": settings.to_json() });
            emit(out, a.json(), &body, || settings_lines(&id, &body["settings"]));
            return Ok(());
        }
        "delete" => {
            a.allow(&["confirm", "json"], 2..=2)?;
            let confirm = a.value("confirm").ok_or_else(|| usage("deleting a project is permanent: repeat its id with --confirm P"))?;
            let (records, links) = s.delete_project(a.pos(1), confirm).await?;
            let body = json!({ "deleted": { "records": records, "links": links } });
            emit(out, a.json(), &body, || format!("deleted {}: {records} notes, {links} links", a.pos(1)));
            return Ok(());
        }
        _ => {
            return Err(usage(
                "usage: swiss memory project list | add NAME | bind | rebind P IDENTITY [--cwd D] [--exact] | unbind P IDENTITY | rename P NAME | delete P --confirm P | settings [--id P|--cwd D] [--agent A[,B...]|off|inherit] [--lead L|off|inherit] [--embedding on|off|inherit] [--auto-review HH:MM|off] [--auto-review-runs N|default]",
            ))
        }
    };
    emit(out, a.json(), &json!({ "project": view }), || project_lines(std::slice::from_ref(&view)));
    Ok(())
}

/// A project's settings as text: each field, or what it inherits from.
fn settings_lines(id: &str, settings: &Value) -> String {
    let field = |value: &Value| match value {
        Value::Null => "inherit (the sync block, then the newest llm agent)".to_string(),
        Value::String(s) => s.clone(),
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    };
    let lead = match &settings["lead"] {
        Value::Null => "inherit (the sync block's lead, or none)".to_string(),
        other => field(other),
    };
    let embedding = match &settings["embedding"] {
        Value::Bool(on) => if *on { "on" } else { "off" }.to_string(),
        _ => "inherit (on whenever a model is set)".to_string(),
    };
    let docs = match settings["docs"].as_array() {
        Some(sources) if !sources.is_empty() => field(&settings["docs"]),
        _ => "none (`swiss memory docs add PATH` names one)".to_string(),
    };
    let runs = match settings["autoReviewRuns"].as_u64() {
        Some(1) => "1 run".to_string(),
        Some(n) => format!("{n} runs"),
        None => format!("{} runs (the default)", swiss_memory::settings::AUTO_REVIEW_RUNS_DEFAULT),
    };
    let auto_review = match settings["autoReview"].as_str() {
        Some(slot) => format!("daily at {slot}, the notes whose code changed, up to {runs} a day"),
        None => "off".to_string(),
    };
    format!(
        "{id}\n  agents: {}\n  lead: {lead}\n  embedding: {embedding}\n  docs: {docs}\n  auto-review: {auto_review}",
        field(&settings["agents"])
    )
}

async fn status(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["json"], 0..=0)?;
    let status = s.status().await?;
    emit(out, a.json(), &status, || {
        format!(
            "store: {}\nschema: {}\nthe MCP endpoint's state is on the panel's Memory page\nprojects:\n{}",
            status.db_path,
            status.schema_version,
            project_lines(&status.projects)
        )
    });
    Ok(())
}

// --- transfer -----------------------------------------------------------------------------

async fn export(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["cwd", "global", "all", "output", "domain"], 0..=0)?;
    let scope = match (a.has("global"), a.has("all")) {
        (true, true) => return Err(usage("--global and --all exclude each other")),
        (_, true) if a.value("domain").is_some() => return Err(usage("--domain takes one project's domain; leave out --all")),
        (true, false) => ExportScope::Project(swiss_memory::GLOBAL_PROJECT_ID.to_string()),
        (false, true) => ExportScope::All,
        (false, false) => {
            let project = s.resolve(&a.target()?).await?;
            let project = project.ok_or_else(|| usage("this directory is not a registered project; pass --global or --all"))?;
            ExportScope::Project(project.id)
        }
    };
    let scope = match (scope, a.value("domain")) {
        (ExportScope::Project(project), Some(domain)) => ExportScope::Domain { project, domain: domain.to_string() },
        (scope, _) => scope,
    };
    let text = s.export(&scope).await?;
    match a.value("output") {
        Some(path) => {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| Fail::Io(format!("{path}: {e} (export never overwrites a file)")))?;
            file.write_all(text.as_bytes()).map_err(|e| Fail::Io(format!("{path}: {e}")))?;
            out.err(&format!("wrote {path}"));
        }
        None => out.line(text.trim_end_matches('\n')),
    }
    Ok(())
}

async fn import(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["dry-run", "strategy", "json", "into", "cwd"], 1..=1)?;
    let strategy = match a.value("strategy") {
        None => ImportStrategy::Skip,
        Some(raw) => ImportStrategy::parse(raw).ok_or_else(|| usage("--strategy must be skip or fail"))?,
    };
    // `--into .`: the project of the cwd (or --cwd), the usual landing of a colleague's file.
    let into = match a.value("into") {
        None => None,
        Some(".") => {
            let project = s.resolve(&a.target()?).await?;
            Some(project.ok_or_else(|| usage("--into .: this directory is not a registered project"))?.id)
        }
        Some(id) => Some(id.to_string()),
    };
    let path = a.pos(0);
    let size = std::fs::metadata(path).map_err(|e| Fail::Io(format!("{path}: {e}")))?.len();
    if size > IMPORT_MAX_BYTES as u64 {
        return Err(Fail::Mem(MemError::invalid(format!("{path} is larger than {} MiB", IMPORT_MAX_BYTES / (1024 * 1024)))));
    }
    let text = std::fs::read_to_string(path).map_err(|e| Fail::Io(format!("{path}: {e}")))?;
    let options = ImportOptions { dry_run: a.has("dry-run"), strategy, into };
    let report = s.import_with(&text, &options).await?;
    emit(out, a.json(), &report, || {
        let mut text = String::new();
        if let Some(source) = source_line(&report.source) {
            text.push_str(&format!("from: {source}\n"));
        }
        if let Some(into) = &report.into {
            text.push_str(&format!("into: {into}\n"));
        }
        text.push_str(&format!(
            "{}{} notes and {} links created; {} notes and {} links skipped",
            if report.dry_run { "dry run (nothing written): " } else { "" },
            report.records_created,
            report.links_created,
            report.records_skipped.len(),
            report.links_skipped
        ));
        if !report.conflicts.is_empty() {
            text.push_str(&format!("\n{} conflicts, kept out (the store's notes stand):", report.conflicts.len()));
            for c in &report.conflicts {
                let what = match c.kind {
                    ConflictKind::Revision => format!("{} r{} in the file, r{} here", c.incoming.id, c.incoming.revision, c.stored.revision),
                    ConflictKind::Title => format!("{} has the title of {}, with other text", c.incoming.id, c.stored.id),
                };
                text.push_str(&format!("\n  line {} {:?} {what}", c.line, c.incoming.title));
                text.push_str(&format!("\n    file:  {}\n    store: {}", c.incoming.excerpt, c.stored.excerpt));
            }
        }
        if !report.projects_created.is_empty() {
            text.push_str(&format!("\nnew projects (bind them with `swiss memory project bind`): {}", report.projects_created.join(", ")));
        }
        text
    });
    Ok(())
}

/// What a file says of itself, on one line; None for an older build's bare header.
fn source_line(h: &Header) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match (&h.domain, &h.project) {
        (Some(domain), Some(p)) => parts.push(format!("domain {domain} of {} ({})", p.name, p.id)),
        (None, Some(p)) => parts.push(format!("project {} ({})", p.name, p.id)),
        _ => parts.extend(h.scope.clone()),
    }
    if let Some(visibility) = &h.visibility {
        parts.push(format!("read by: {visibility}"));
    }
    if let Some(notes) = h.notes {
        parts.push(format!("{notes} notes, {} confirmed by a person", h.confirmed.unwrap_or(0)));
    }
    if let Some(left) = h.links_left_out.filter(|n| *n > 0) {
        parts.push(format!("{left} links left out (an end outside the domain)"));
    }
    parts.extend(h.exporter.clone());
    (!parts.is_empty()).then(|| parts.join(" · "))
}

// --- misses (SPEC §memory.evaluation Misses) ----------------------------------------------

/// `misses [list] | add QUERY … | export [--output F] | drop ID…|--all`: the retrievals
/// recorded as failed, and the fixture they make.
async fn misses_cli(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    let ids = |key: &str| -> Vec<String> {
        a.value(key).map(|raw| raw.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect()).unwrap_or_default()
    };
    match a.pos(0) {
        "" | "list" => {
            a.allow(&["cwd", "global", "json"], 0..=1)?;
            let items = s.misses(&a.target()?).await?;
            emit(out, a.json(), &items, || {
                if items.is_empty() {
                    return "no misses recorded".to_string();
                }
                let mut text = String::new();
                for m in &items {
                    let what = match (&m.want, m.wanted.is_empty()) {
                        (_, false) => format!("wanted {}", m.wanted.join(", ")),
                        (Some(want), true) => format!("wanted: {want}"),
                        (None, true) => String::new(),
                    };
                    let wrong = if m.wrong.is_empty() { String::new() } else { format!("; wrong {}", m.wrong.join(", ")) };
                    let via = m.via.as_deref().map(|v| format!(" via {v}")).unwrap_or_default();
                    text.push_str(&format!("{} {:?}{via} - {what}{wrong} ({}, {} ago)\n", m.id, m.query, m.actor, ago(m.at_ms)));
                }
                text
            });
        }
        "add" => {
            a.allow(&["cwd", "global", "json", "via", "style", "shown", "wrong", "wanted", "want", "path", "producer"], 2..=2)?;
            let request = MissRequest {
                query: a.pos(1).to_string(),
                via: a.value("via").map(str::to_string),
                style: a.value("style").map(str::to_string),
                shown: ids("shown"),
                wrong: ids("wrong"),
                wanted: ids("wanted"),
                want: a.value("want").map(str::to_string),
                paths: a.values("path"),
            };
            let miss = s.record_miss(&a.target()?, &cli_actor(a)?, request).await?;
            emit(out, a.json(), &miss, || format!("recorded miss {} in {}; no note changed", miss.id, miss.project));
        }
        "export" => {
            a.allow(&["cwd", "global", "output"], 1..=1)?;
            let export = s.export_misses(&a.target()?).await?;
            let text = serde_json::to_string_pretty(&export.fixture).map_err(|e| Fail::Io(format!("the fixture: {e}")))?;
            match a.value("output") {
                Some(path) => {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .map_err(|e| Fail::Io(format!("{path}: {e} (export never overwrites a file)")))?;
                    file.write_all(text.as_bytes()).map_err(|e| Fail::Io(format!("{path}: {e}")))?;
                    out.err(&format!("wrote {path}"));
                }
                None => out.line(&text),
            }
            out.err(&format!("{} queries, {} notes; {} misses name no note to expect", export.queries, export.fixture.notes.len(), export.gaps.len()));
            for gap in &export.gaps {
                out.err(&format!("gap {} {:?}: {}", gap.id, gap.query, gap.want.as_deref().unwrap_or("the wanted notes are gone")));
            }
        }
        "drop" => {
            a.allow(&["cwd", "global", "all", "json"], 1..=1 + swiss_memory::misses::MISSES_MAX)?;
            let ids: Vec<String> = a.positional[1..].to_vec();
            if a.has("all") == !ids.is_empty() {
                return Err(usage("misses drop: pass miss ids (x-…) or --all"));
            }
            let dropped = s.drop_misses(&a.target()?, &ids).await?;
            emit(out, a.json(), &json!({ "dropped": dropped }), || format!("dropped {dropped} misses; no note changed"));
        }
        _ => return Err(usage(MISSES_USAGE)),
    }
    Ok(())
}

async fn backup(a: &Args, s: &MemoryService, out: &dyn Out) -> Run {
    a.allow(&["output", "json"], 0..=0)?;
    let output = a.value("output").ok_or_else(|| usage("backup requires --output F"))?;
    let path = s.backup(Some(std::path::Path::new(output))).await?;
    let shown = swiss_memory::scope::display(&path);
    emit(out, a.json(), &json!({ "path": shown }), || format!("backed up to {shown}"));
    Ok(())
}

#[cfg(test)]
mod tests;
