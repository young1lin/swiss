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

//! Memory measured against real models (SPEC §memory.evaluation), never in a default run: each
//! test here is ignored and skips itself without its SWISS_LIVE_* variables, read from the
//! environment or else the repo's .env, as the e2e live file reads them (SWISS_LIVE_ENV names
//! another file). A key reaches its client as a `${SWISS_LIVE_*_API_KEY}` reference, never as a
//! value here, and nothing here prints one.
//!
//! `cargo test -j 4 -p swiss-plugins --lib memory_live -- --ignored --nocapture --test-threads 1`

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use serde_json::{json, Value};
use swiss_llm::embedding::EmbeddingConfig;
use swiss_llm::provider::Chat;
use swiss_memory::eval::{self, Fixture, FixtureNote, Mode};
use swiss_memory::{BriefRequest, MemoryService, Target};

use super::memory_embed::LlmEmbedder;

/// How many notes of each fixture the judged pass asks about.
const JUDGED: usize = 12;
/// The judged pass's calls in flight at once.
const IN_FLIGHT: usize = 3;

/// One provider named by SWISS_LIVE_<PREFIX>_*: the base URL and the model are required.
pub(crate) struct Live {
    pub(crate) prefix: &'static str,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) style: String,
}

impl Live {
    pub(crate) fn from_env(prefix: &'static str) -> Option<Live> {
        static DOTENV: Once = Once::new();
        DOTENV.call_once(load_dotenv);
        let var = |name: &str| {
            std::env::var(format!("SWISS_LIVE_{prefix}_{name}")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        Some(Live {
            prefix,
            base_url: var("BASE_URL")?.trim_end_matches('/').to_string(),
            model: var("MODEL")?,
            style: var("STYLE").unwrap_or_else(|| "openai".into()),
        })
    }

    /// The key as the env reference the client resolves; None when the variable is unset.
    pub(crate) fn key(&self) -> Option<String> {
        let name = format!("SWISS_LIVE_{}_API_KEY", self.prefix);
        std::env::var_os(&name).is_some().then(|| format!("${{{name}}}"))
    }
}

/// The repo's .env (or SWISS_LIVE_ENV) into this process's environment, a variable already set
/// winning: `NAME=value` lines, `#` comments, a value optionally quoted.
fn load_dotenv() {
    let file = std::env::var_os("SWISS_LIVE_ENV").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
    let Ok(text) = std::fs::read_to_string(file) else { return };
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        let Some((name, value)) = line.trim_start_matches("export ").split_once('=') else { continue };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || std::env::var_os(name).is_some() {
            continue;
        }
        let value = value.trim();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or(""),
            _ => value.split(" #").next().unwrap_or("").trim_end(),
        };
        std::env::set_var(name, value);
    }
}

fn embedding_config(live: &Live) -> EmbeddingConfig {
    EmbeddingConfig {
        provider: "live".into(),
        base_url: live.base_url.clone(),
        api_key: live.key(),
        proxy: None,
        model: live.model.clone(),
        dimensions: None,
        batch: 16,
        timeout_ms: 30_000,
        query_prefix: String::new(),
        document_prefix: String::new(),
        min_similarity: std::env::var("SWISS_LIVE_EMBED_FLOOR").ok().and_then(|f| f.trim().parse().ok()),
    }
}

/// The chat model at low effort: a model that always thinks would otherwise spend the answer's
/// tokens on it.
fn chat(live: &Live) -> Chat {
    let mut provider = json!({ "style": live.style, "baseUrl": live.base_url, "models": [{ "id": live.model, "kind": "chat" }] });
    if let Some(key) = live.key() {
        provider["apiKey"] = json!(key);
    }
    let config = swiss_llm::LlmConfig::parse(&json!({ "providers": { "live": provider } })).expect("the live provider");
    Chat::new(&config.providers["live"], Some(&live.model), 4096)
        .expect("chat client")
        .with_effort(Some(swiss_llm::agent::Effort::Low.reasoning()))
}

/// One answer, asked again once when the first call fails.
async fn ask(chat: &Chat, system: &str, prompt: &str) -> String {
    let session = swiss_llm::telemetry::Session::disabled();
    match swiss_llm::provider::ask(chat, Some(system), prompt, &session).await {
        Ok(text) => text,
        Err(_) => swiss_llm::provider::ask(chat, Some(system), prompt, &session).await.unwrap_or_else(|err| panic!("the chat model failed twice: {err}")),
    }
}

fn fixtures() -> Vec<(&'static str, Fixture)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swiss-memory/tests/fixtures");
    ["swiss-history.json", "cc-tutorial.json"]
        .into_iter()
        .map(|name| (name, Fixture::parse(&std::fs::read_to_string(dir.join(name)).expect("fixture")).expect(name)))
        .collect()
}

/// A fixture loaded into a fresh store under a scratch repository.
struct Scratch {
    dir: PathBuf,
    root: PathBuf,
    service: MemoryService,
    /// Fixture key -> note id.
    ids: HashMap<String, String>,
}

impl Scratch {
    async fn load(fixture: &Fixture) -> Scratch {
        let dir = std::env::temp_dir().join(format!("swiss-live-eval-{}-{}", std::process::id(), swiss_core::util::random_hex(6)));
        std::fs::create_dir_all(dir.join("repo").join(".git")).expect("scratch repo");
        let dir = dir.canonicalize().expect("canonical scratch");
        let root = dir.join("repo");
        let service = MemoryService::at(&dir.join("store.sqlite3"));
        let ids = eval::load(&service, &root, fixture).await.expect("load").ids;
        Scratch { dir, root, service, ids }
    }

    /// Every note embedded by `config`'s endpoint.
    async fn embed_all(&self, config: &EmbeddingConfig) {
        self.service.set_embedder(Some(Arc::new(LlmEmbedder::build(config, None).expect("embedder"))));
        loop {
            let run = self.service.index_pending(64).await.expect("index");
            assert!(run.error.is_none() && run.failed == 0, "indexing failed: {:?}", run.error);
            if run.pending == 0 {
                break;
            }
        }
    }

    fn close(self) {
        let Scratch { dir, service, .. } = self;
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The retrieval fixtures scored with a real embedding provider: each fixture loads into a
/// scratch store and every mode is scored twice - no embedder, then every note embedded - and
/// printed side by side. SWISS_LIVE_EMBED_BASE_URL, _MODEL and _API_KEY name an OpenAI-style
/// endpoint; SWISS_LIVE_EMBED_FLOOR overrides the similarity floor.
#[tokio::test]
#[ignore = "calls a real embedding provider: set SWISS_LIVE_EMBED_*"]
async fn fixtures_score_with_a_real_embedder() {
    let Some(live) = Live::from_env("EMBED") else {
        eprintln!("SWISS_LIVE_EMBED_BASE_URL or SWISS_LIVE_EMBED_MODEL unset: nothing to score");
        return;
    };
    let config = embedding_config(&live);
    for (name, fixture) in fixtures() {
        let scratch = Scratch::load(&fixture).await;
        let mut reports = Vec::new();
        for mode in Mode::ALL {
            reports.push(eval::evaluate(&scratch.service, &scratch.root, &fixture, mode).await.expect("evaluate"));
        }
        scratch.embed_all(&config).await;
        for mode in Mode::ALL {
            let report = eval::evaluate(&scratch.service, &scratch.root, &fixture, mode).await.expect("evaluate");
            println!("--- {name} with {}\n{}", config.model, report.table());
            for miss in &report.misses {
                println!("miss [{}] {:?}: expected {:?}, got {:?}", miss.style, miss.query, miss.expected, miss.got);
            }
            reports.push(report);
        }
        println!("--- {name}: no embedder, then {} (floor {:?})\n{}", config.model, config.min_similarity, eval::compare(&reports));
        let hybrid = reports.iter().rfind(|r| r.mode == Mode::Hybrid).expect("hybrid");
        assert_eq!(hybrid.setup.state, "ready", "{name}: every note embedded");
        assert!(hybrid.semantic_queries > 0, "{name}: the semantic tier ran");
        scratch.close();
    }
}

const ASK_SYSTEM: &str = "A project's memory holds the note below. Write what a developer on this project would type \
to a coding agent when this note is what the agent needs to know, though the developer has not read it: one short \
question or request in LANGUAGE, at most 25 words (60 characters in Chinese), from the problem they face, in their \
own words. It must not carry the note's answer, its rules or its details, nor restate its title. Name no note and \
quote nothing. Answer with the question alone.";

const GRADE_SYSTEM: &str = "You grade the context a project-memory system handed a coding agent for a developer's \
request. Each note in the context carries its id in backquotes. Judge from the context alone. Answer with one JSON \
object and nothing else: {\"relevant\": [ids of the notes that help with this request], \"misleading\": [ids of the \
notes that would steer the agent wrong on it], \"answerable\": true when the context holds what the request needs, \
else false, \"missing\": \"what the request needs that the context lacks, or an empty string\"}. A note that is \
merely beside the point is neither relevant nor misleading.";

/// Up to [JUDGED] current notes (none another replaces), evenly spaced through the fixture:
/// the same notes every run.
fn sample(fixture: &Fixture) -> Vec<&FixtureNote> {
    let replaced: HashSet<&str> = fixture.notes.iter().filter_map(|n| n.replaces.as_deref()).collect();
    let current: Vec<&FixtureNote> = fixture.notes.iter().filter(|n| !replaced.contains(n.key.as_str())).collect();
    let take = JUDGED.min(current.len());
    (0..take).map(|i| current[i * current.len() / take]).collect()
}

/// The model's request for `note`, on one line and within the brief's task limit.
async fn request_for(chat: &Chat, language: &str, note: &FixtureNote) -> String {
    let answer = ask(chat, &ASK_SYSTEM.replace("LANGUAGE", language), &format!("Title: {}\n\n{}", note.title, note.content)).await;
    let line = answer.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches(|c: char| c == '"' || c == '\u{201c}' || c == '\u{201d}' || c == '\u{300c}' || c == '\u{300d}');
    line.chars().take(swiss_memory::brief::TASK_MAX).collect()
}

/// One graded brief.
struct Graded {
    key: String,
    request: String,
    shown: usize,
    source_shown: bool,
    /// The request's source note.
    source: String,
    /// Where each shown note stands.
    placed: HashMap<String, Placed>,
    /// None when the grade was no JSON object.
    grade: Option<Grade>,
}

struct Grade {
    relevant: HashSet<String>,
    misleading: HashSet<String>,
    answerable: bool,
    missing: String,
}

/// The grade's JSON object, keeping only ids the brief showed.
fn parse_grade(answer: &str, shown: &[String]) -> Option<Grade> {
    let value: Value = serde_json::from_str(answer.get(answer.find('{')?..=answer.rfind('}')?)?).ok()?;
    let ids = |field: &str| {
        let named: HashSet<&str> = value[field].as_array().map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim_matches('`')).collect()).unwrap_or_default();
        shown.iter().filter(|id| named.contains(id.as_str())).cloned().collect::<HashSet<String>>()
    };
    Some(Grade {
        relevant: ids("relevant"),
        misleading: ids("misleading"),
        answerable: value["answerable"].as_bool()?,
        missing: value["missing"].as_str().unwrap_or("").trim().to_string(),
    })
}

/// Where a shown note stands in its brief.
#[derive(Debug, PartialEq)]
struct Placed {
    /// The `## ` heading above the note's own line, a path cut off.
    section: String,
    /// Its place in that section, from 1.
    rank: usize,
    /// Its kind, as its line names it.
    kind: String,
    /// The relation its line names (`depends on`, `supports` ...), empty for none.
    relation: String,
    /// The note that relation names, empty for none.
    origin: String,
}

/// Each shown note's place: its own line is the item heading (`### [kind] title · `id``) or
/// the list line (`- [kind] title · `id` — relation `other``) carrying its id first - a
/// mention in another note's text or relation is not where it stands.
fn placed(markdown: &str, shown: &[String]) -> HashMap<String, Placed> {
    let mut section = String::new();
    let mut rank = 0;
    let mut found: HashMap<String, Placed> = HashMap::new();
    for line in markdown.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h.split(" `").next().unwrap_or(h).trim().to_string();
            rank = 0;
            continue;
        }
        if !line.starts_with("### [") && !line.starts_with("- [") {
            continue;
        }
        let Some(at) = line.find("· `") else { continue };
        let rest = &line[at + "· `".len()..];
        let Some(end) = rest.find('`') else { continue };
        let id = &rest[..end];
        if !shown.iter().any(|s| s == id) || found.contains_key(id) {
            continue;
        }
        rank += 1;
        let kind = line.split_once('[').and_then(|(_, r)| r.split_once(']')).map(|(k, _)| k.to_string()).unwrap_or_default();
        let (relation, origin) = match rest[end..].split_once(" — ") {
            Some((_, r)) => {
                let (phrase, named) = r.split_once(" `").unwrap_or((r, ""));
                (phrase.trim().to_string(), named.split('`').next().unwrap_or("").to_string())
            }
            None => Default::default(),
        };
        found.insert(id.to_string(), Placed { section: section.clone(), rank, kind, relation, origin });
    }
    found
}

/// Over the graded briefs, per key a shown note's place gives - with its brief's places, for an
/// origin's (None leaves it out): notes shown, graded relevant, graded misleading - sorted.
fn tally(graded: &[Graded], key: impl Fn(&Placed, &HashMap<String, Placed>) -> Option<String>) -> Vec<(String, usize, usize, usize)> {
    let mut rows: Vec<(String, usize, usize, usize)> = Vec::new();
    for g in graded {
        let Some(grade) = &g.grade else { continue };
        for (id, place) in &g.placed {
            let Some(key) = key(place, &g.placed) else { continue };
            let at = match rows.iter().position(|r| r.0 == key) {
                Some(at) => at,
                None => {
                    rows.push((key, 0, 0, 0));
                    rows.len() - 1
                }
            };
            rows[at].1 += 1;
            rows[at].2 += usize::from(grade.relevant.contains(id));
            rows[at].3 += usize::from(grade.misleading.contains(id));
        }
    }
    rows.sort();
    rows
}

/// Each request's brief (the task alone, as `brief-task` runs it), graded by the model.
async fn judge(chat: &Chat, scratch: &Scratch, fixture: &Fixture, requests: &[(String, String)]) -> Vec<Graded> {
    let titles: HashMap<&str, &str> = fixture.notes.iter().filter_map(|n| Some((scratch.ids.get(&n.key)?.as_str(), n.title.as_str()))).collect();
    let target = Target::dir(scratch.root.display().to_string());
    let mut briefs = Vec::new();
    for (key, request) in requests {
        let brief = scratch
            .service
            .brief(&target, &BriefRequest { paths: Vec::new(), task: Some(request.clone()), max_chars: None, exclude_global: true })
            .await
            .expect("brief");
        briefs.push((key, request, brief));
    }
    let mut graded = Vec::new();
    for chunk in briefs.chunks(IN_FLIGHT) {
        let answers = futures_util::future::join_all(chunk.iter().map(|(_, request, brief)| {
            let listed: String = brief.shown.iter().map(|id| format!("- `{id}` {}\n", titles.get(id.as_str()).unwrap_or(&""))).collect();
            let prompt = format!("Request: {request}\n\nContext:\n{}\n\nThe notes the context shows, by id:\n{listed}", brief.markdown);
            async move { ask(chat, GRADE_SYSTEM, &prompt).await }
        }))
        .await;
        for ((key, request, brief), answer) in chunk.iter().zip(answers) {
            graded.push(Graded {
                key: key.to_string(),
                request: request.to_string(),
                shown: brief.shown.len(),
                source_shown: brief.shown.contains(&scratch.ids[key.as_str()]),
                source: scratch.ids[key.as_str()].clone(),
                placed: placed(&brief.markdown, &brief.shown),
                grade: parse_grade(&answer, &brief.shown),
            });
        }
    }
    graded
}

/// One pass's row: the source note shown, briefs graded answerable, and the shares of the shown
/// notes graded relevant and misleading (over the graded briefs).
fn row(label: &str, graded: &[Graded]) -> String {
    let n = graded.len().max(1) as f64;
    let rated: Vec<&Grade> = graded.iter().filter_map(|g| g.grade.as_ref()).collect();
    let shown: usize = graded.iter().filter(|g| g.grade.is_some()).map(|g| g.shown).sum();
    let share = |count: usize| if shown == 0 { 0.0 } else { count as f64 / shown as f64 };
    format!(
        "{label:<34} {:>3} {:>7.3} {:>11.3} {:>9.3} {:>11.3} {:>6.1} {:>9}",
        graded.len(),
        graded.iter().filter(|g| g.source_shown).count() as f64 / n,
        rated.iter().filter(|g| g.answerable).count() as f64 / rated.len().max(1) as f64,
        share(rated.iter().map(|g| g.relevant.len()).sum()),
        share(rated.iter().map(|g| g.misleading.len()).sum()),
        graded.iter().map(|g| g.shown).sum::<usize>() as f64 / n,
        graded.len() - rated.len(),
    )
}

/// The judged pass: a chat model writes a developer's request from each sampled note, the brief
/// for that request is built, and the model grades the brief as the agent receives it - which
/// notes help, which mislead, whether it answers the request and what it lacks. The requests
/// are written once and every pass judges the same ones: no embedder, then (with
/// SWISS_LIVE_EMBED_*) every note embedded. SWISS_LIVE_LLM_BASE_URL, _MODEL, _STYLE
/// (`openai` | `anthropic`) and _API_KEY name the chat model.
#[tokio::test]
#[ignore = "calls a real chat model: set SWISS_LIVE_LLM_* (and SWISS_LIVE_EMBED_* for the embedded pass)"]
async fn a_model_judges_the_briefs_for_its_requests() {
    let Some(llm) = Live::from_env("LLM") else {
        eprintln!("SWISS_LIVE_LLM_BASE_URL or SWISS_LIVE_LLM_MODEL unset: nothing to judge");
        return;
    };
    let chat = chat(&llm);
    let embed = Live::from_env("EMBED").map(|live| embedding_config(&live));
    for (name, fixture) in fixtures() {
        let scratch = Scratch::load(&fixture).await;
        let language = if fixture.project.language == "zh" { "Chinese" } else { "English" };
        let notes = sample(&fixture);
        let mut requests = Vec::new();
        for chunk in notes.chunks(IN_FLIGHT) {
            let written = futures_util::future::join_all(chunk.iter().map(|note| request_for(&chat, language, note))).await;
            requests.extend(chunk.iter().map(|note| note.key.clone()).zip(written));
        }
        assert!(requests.iter().all(|(_, r)| !r.is_empty()), "{name}: every note has a request");
        let mut passes = vec![("brief-task, no embedder".to_string(), judge(&chat, &scratch, &fixture, &requests).await)];
        if let Some(config) = &embed {
            scratch.embed_all(config).await;
            passes.push((format!("brief-task, {} floor {:?}", config.model, config.min_similarity), judge(&chat, &scratch, &fixture, &requests).await));
        }
        println!("--- {name}: {} notes' requests written and graded by {}", requests.len(), llm.model);
        for (label, graded) in &passes {
            println!("{label}");
            for g in graded {
                let verdict = match &g.grade {
                    Some(grade) => format!(
                        "relevant {}/{}, misleading {}, {}{}",
                        grade.relevant.len(),
                        g.shown,
                        grade.misleading.len(),
                        if grade.answerable { "answerable" } else { "NOT answerable" },
                        if grade.missing.is_empty() { String::new() } else { format!("; missing: {}", grade.missing) }
                    ),
                    None => "ungraded".into(),
                };
                println!("  [{}] {} {:?}: {verdict}", if g.source_shown { "src" } else { "---" }, g.key, g.request);
            }
        }
        println!("{:<34} {:>3} {:>7} {:>11} {:>9} {:>11} {:>6} {:>9}", "pass", "n", "source", "answerable", "relevant", "misleading", "shown", "ungraded");
        for (label, graded) in &passes {
            println!("{}", row(label, graded));
            assert!(graded.iter().any(|g| g.grade.is_some()), "{name}, {label}: the model graded a brief");
        }
        const RELATED: &str = "Related to your task";
        for (label, graded) in &passes {
            println!("{label}: shown / relevant / misleading");
            let at = |p: &Placed| if p.section == RELATED { format!("{RELATED} #{}", p.rank) } else { p.section.clone() };
            let tables = [
                tally(graded, |p, _| Some(p.section.clone())),
                tally(graded, |p, _| (p.section == RELATED).then(|| at(p))),
                tally(graded, |p, _| (p.section == "Linked").then(|| format!("Linked, {}", p.relation))),
                tally(graded, |p, all| {
                    (p.section == "Linked").then(|| format!("Linked from {}", all.get(&p.origin).map(at).unwrap_or_default()))
                }),
                tally(graded, |p, _| (!matches!(p.section.as_str(), RELATED | "Linked")).then(|| format!("{}, {}", p.section, p.kind))),
            ];
            for (key, shown, relevant, misleading) in tables.into_iter().flatten() {
                println!("  {key:<34} {shown:>4} {relevant:>4} {misleading:>4}");
            }
            let mut sources: Vec<(String, usize)> = Vec::new();
            for g in graded {
                let at = match g.placed.get(&g.source) {
                    Some(p) if p.section == RELATED => format!("{RELATED} #{}", p.rank),
                    Some(p) => p.section.clone(),
                    None => "not shown".into(),
                };
                match sources.iter_mut().find(|(k, _)| *k == at) {
                    Some(row) => row.1 += 1,
                    None => sources.push((at, 1)),
                }
            }
            sources.sort();
            println!("  the source note stands in: {}", sources.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "));
        }
        scratch.close();
    }
}

#[test]
fn a_grade_counts_only_the_ids_the_brief_showed() {
    let shown = vec!["m-1".to_string(), "m-2".to_string(), "m-3".to_string()];
    let answer = "```json\n{\"relevant\": [\"`m-1`\", \"m-2\", \"m-9\"], \"misleading\": [\"m-3\"], \"answerable\": true, \"missing\": \"\"}\n```";
    let grade = parse_grade(answer, &shown).expect("graded");
    assert_eq!((grade.relevant.len(), grade.misleading.len(), grade.answerable, grade.missing.as_str()), (2, 1, true, ""));
    assert!(grade.relevant.contains("m-1") && !grade.relevant.contains("m-9"));
    assert!(parse_grade("no verdict", &shown).is_none());
    assert!(parse_grade("{\"relevant\": []}", &shown).is_none(), "answerable is required");
}

#[test]
fn a_shown_note_stands_on_its_own_line_not_where_it_is_mentioned() {
    let markdown = "# Project memory\n\n## Where you are\n\n### Deploy · `deploy`\n\n## Related to your task\n\n### [constraint] Port · `m-1`\ntext `m-2`\n\n### [pitfall] Lock · `m-5`\n\n## Linked\n- [knowledge] B · `m-2` — supports `m-1`\n\n## In `deploy`\n- [decision] C · `m-3`\n";
    let shown: Vec<String> = ["m-1", "m-5", "m-2", "m-3", "m-4"].map(String::from).to_vec();
    let found = placed(markdown, &shown);
    let at = |id: &str| found.get(id).map(|p| (p.section.as_str(), p.rank, p.kind.as_str(), p.relation.as_str(), p.origin.as_str()));
    assert_eq!(at("m-1"), Some(("Related to your task", 1, "constraint", "", "")));
    assert_eq!(at("m-5"), Some(("Related to your task", 2, "pitfall", "", "")));
    assert_eq!(at("m-2"), Some(("Linked", 1, "knowledge", "supports", "m-1")));
    assert_eq!(at("m-3"), Some(("In", 1, "decision", "", "")));
    assert_eq!(at("m-4"), None);
    assert!(!found.contains_key("deploy"), "a domain heading is no note");
}
