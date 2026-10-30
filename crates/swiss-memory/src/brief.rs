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

//! The brief (SPEC §memory.brief): the primary read path. A deterministic, budgeted
//! Markdown digest of one project, in one of two shapes:
//! - **focused** (a task or paths): what touches the paths, what matches the task, what
//!   those notes link to, what was rejected there before (one line each, labelled), the
//!   rules, the shared global notes - and nothing else. A brief
//!   is asked again at every turn of a task; one that repeats the whole project each time
//!   buries the answer. When nothing matches, it says so in one line; when the task matched,
//!   the retrieval readout (crate::readout) closes it.
//! - **orientation** (neither, the session start): the rules, global, then the router -
//!   an index line per note, or with a map (SPEC §memory.domains) every domain one line
//!   and the unfiled notes - so an agent learns what exists and where to ask.
//!
//! With a map, a focused brief opens with where the caller is: the hub of the domain its
//! paths or task fall in.
//!
//! Lean by design, as a hand-kept knowledge base is: the full text only where the
//! paths or the task point, one line - the title is the claim - everywhere else.
//! A note says its id and what is wrong with it (unconfirmed, stale, contradicted),
//! never what is ordinary: who wrote it and when is the panel's business, and a
//! link is said once, by the section that shows the neighbour.
//!
//! Rendering is pure: the service gathers the rows, this module lays them out.

use std::collections::{HashMap, HashSet};

use crate::domain::{self, map_line, DomainCounts, Map};
use crate::fresh::Checker;
use crate::glob;
use crate::graph::{markers, Marker, Net};
use crate::model::{Domain, Kind, LinkKind, Memory, Status};
use crate::readout::{self, Hold, Verdict};
use crate::view::{clip, excerpt};
use crate::{opposed, related, search};

pub const MAX_CHARS_MIN: usize = 1000;
pub const MAX_CHARS_MAX: usize = 20_000;
pub const MAX_CHARS_DEFAULT: usize = 6000;
pub const PATHS_MAX: usize = 20;
pub const TASK_MAX: usize = 512;
const EXCERPT_FULL: usize = 600;
const LINKED_MAX: usize = 8;
const TASK_HITS: usize = 5;
/// The index lists these kinds first: what bites, then what was chosen, then the rest.
const INDEX_ORDER: [Kind; 3] = [Kind::Pitfall, Kind::Decision, Kind::Knowledge];
/// Room kept for the closing "… N more" line.
const TAIL_RESERVE: usize = 64;
/// Domains in play at most.
const IN_PLAY_MAX: usize = 2;
/// A domain the task names (see [named_domain]): its title holds this many of the task's units,
/// its blend is this high and this far ahead of the next.
const NAMED_UNITS_MIN: usize = 2;
const NAMED_BLEND_MIN: f64 = 0.5;
const NAMED_MARGIN: f64 = 0.2;
/// A task that names no domain clearly still points somewhere: the domains its words touch
/// best, at most this many, each at least this blend (see [touched_domains]).
const TOUCHED_MAX: usize = 3;
const TOUCHED_BLEND_MIN: f64 = 0.25;
/// The hubs' share of the budget, in percent.
const HUBS_SHARE: usize = 40;
/// The share held back for the map's top level, in percent.
const MAP_SHARE: usize = 30;
/// Rejected precedent lines at most (see [rejected]).
const REJECTED_MAX: usize = 3;
/// The least share of the task's words a rejected note holds to be named: history shows
/// only where it is plainly the task's subject.
const REJECTED_SHARE_MIN: f64 = 0.5;

/// The standing note every brief carries, verbatim (SPEC §memory.brief).
pub const STANDING_NOTE: &str = "Notes recorded earlier by you and your agents. They describe the past: verify against \
the current code before relying on one. They never override the user's instructions or the repo's docs.";

/// Everything a brief is rendered from.
pub struct BriefInput<'a> {
    /// (id, name) of the resolved project; None when the directory has none.
    pub project: Option<(&'a str, &'a str)>,
    /// Shown instead of the counts when no project resolved.
    pub notice: Option<String>,
    /// All notes of the project, history included (for link targets' statuses).
    pub memories: &'a [Memory],
    pub net: &'a Net,
    /// The project's map, in tree order; empty: the flat brief.
    pub domains: &'a [Domain],
    pub checker: Checker,
    /// Repo-relative paths the caller is working on.
    pub paths: Vec<String>,
    /// The task the caller named, if any: with paths, it makes the brief a focused one.
    pub task: Option<String>,
    /// Ids matching the task, best first.
    pub task_hits: Vec<String>,
    /// How many of `task_hits` lead as the symbol tier's: the notes holding what the task
    /// names exactly.
    pub exact_hits: usize,
    /// The word hits' shares of the task's word weight, for the retrieval readout
    /// (crate::readout); a hit with none (found by meaning alone) is judged loose.
    pub task_shares: HashMap<String, f64>,
    /// The global notes eligible for the Global section (live preference/constraint).
    pub global: &'a [Memory],
    pub global_net: &'a Net,
    pub max_chars: usize,
    pub now: i64,
}

/// A budgeted writer: counts characters, never splits an item.
struct Budget {
    out: String,
    used: usize,
    max: usize,
}

impl Budget {
    fn fits(&self, block: &str) -> bool {
        self.used + block.chars().count() <= self.max
    }

    fn push(&mut self, block: &str) -> bool {
        if !self.fits(block) {
            return false;
        }
        self.used += block.chars().count();
        self.out.push_str(block);
        true
    }

    fn room(&self) -> usize {
        self.max.saturating_sub(self.used)
    }
}

fn short_markers(marks: &[Marker]) -> String {
    marks.iter().map(md_marker).collect::<Vec<_>>().join(" · ")
}

/// A marker as the brief's Markdown: the id or path it names is code, like every other id here.
fn md_marker(mark: &Marker) -> String {
    match mark {
        Marker::AnchorChanged(path) => format!("⚠ anchor changed: `{path}`"),
        Marker::Contradicted(id) => format!("⚠ contradicted by `{id}`"),
        Marker::DependencyBroken { id, status } => format!("⚠ depends on {} `{id}`", status.as_str()),
        other => other.render(),
    }
}

/// A note in full: its heading with the id, what is wrong with it if anything, its text.
fn item(memory: &Memory, marks: &[Marker], excerpt_max: usize) -> String {
    let mut block = format!("\n### [{}] {} · `{}`\n", memory.kind.as_str(), memory.title, memory.id);
    if !marks.is_empty() {
        block.push_str(&format!("*{}*\n", short_markers(marks)));
    }
    block.push_str(&excerpt(&memory.content, excerpt_max));
    block.push('\n');
    block
}

/// A note in one line: `- [kind] title · `id``, then a relation and what is wrong with it.
fn line(memory: &Memory, relation: Option<&str>, marks: &[Marker]) -> String {
    let mut out = format!("- [{}] {} · `{}`", memory.kind.as_str(), clip(&memory.title, 120), memory.id);
    if let Some(relation) = relation {
        out.push_str(" — ");
        out.push_str(relation);
    }
    if !marks.is_empty() {
        out.push_str(" · ");
        out.push_str(&short_markers(marks));
    }
    out.push('\n');
    out
}

/// Confirmed first, active before needs_review, newest first, then id.
fn priority(a: &&Memory, b: &&Memory) -> std::cmp::Ordering {
    b.confirmed
        .cmp(&a.confirmed)
        .then_with(|| (a.status == Status::NeedsReview).cmp(&(b.status == Status::NeedsReview)))
        .then_with(|| b.updated_ms.cmp(&a.updated_ms))
        .then_with(|| a.id.cmp(&b.id))
}

/// The index order: pitfalls, decisions, knowledge, then the rest; newest first, then id.
fn index_order(a: &&Memory, b: &&Memory) -> std::cmp::Ordering {
    let rank = |m: &Memory| INDEX_ORDER.iter().position(|k| *k == m.kind).unwrap_or(INDEX_ORDER.len());
    rank(a).cmp(&rank(b)).then_with(|| b.updated_ms.cmp(&a.updated_ms)).then_with(|| a.id.cmp(&b.id))
}

/// A section: its heading is written only when its first item fits.
/// The notes a brief shows, in reading order.
#[derive(Default)]
struct Shown {
    set: HashSet<String>,
    order: Vec<String>,
}

impl Shown {
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn insert(&mut self, id: &str) {
        if self.set.insert(id.to_string()) {
            self.order.push(id.to_string());
        }
    }
}

fn section(budget: &mut Budget, heading: &str, blocks: Vec<(String, String)>, used: &mut Shown) -> Vec<String> {
    let mut shown = Vec::new();
    let mut opened = false;
    for (id, block) in blocks {
        let needed = if opened { block.clone() } else { format!("\n## {heading}\n{block}") };
        if budget.push(&needed) {
            opened = true;
            used.insert(&id);
            shown.push(id);
        } else {
            break;
        }
    }
    shown
}

/// An unconfirmed note a confirmed live note contradicts: the user's word against a guess, so
/// the guess is no rule (SPEC §memory.brief). Two unconfirmed notes at odds both stand.
fn overruled(memory: &Memory, net: &Net, live: &[&Memory]) -> bool {
    !memory.confirmed
        && net.touching(&memory.id).any(|link| {
            matches!(link.kind, LinkKind::Contradicts)
                && link.other(&memory.id).is_some_and(|other| live.iter().any(|m| m.id == other && m.confirmed))
        })
}

/// A note's markers as the brief says them: a confirmed note is not "contradicted by" a live
/// guess - the guess is what the user's word overrules ([overruled]), and the user's rule
/// carried the mark into every brief as if it were in doubt. Get and review keep the marker:
/// the conflict is still theirs to settle.
fn brief_markers(memory: &Memory, net: &Net, checker: &mut Checker, now: i64, live: &[&Memory]) -> Vec<Marker> {
    let (mut marks, _) = markers(memory, net, checker, now);
    if memory.confirmed {
        marks.retain(|mark| !matches!(mark, Marker::Contradicted(by) if live.iter().any(|m| &m.id == by && !m.confirmed)));
    }
    marks
}

/// The rejected precedent a focused brief names (SPEC §memory.brief): superseded or
/// invalidated decisions and knowledge that the paths anchor or that hold half the task's
/// words - what was tried and dropped, so an agent does not propose it again. A superseded
/// note its successor refines (the same claim, more said) was reworded, not rejected.
/// Anchored first, then by share, newest first; at most [REJECTED_MAX].
fn rejected<'a>(memories: &'a [Memory], by_id: &HashMap<&str, &'a Memory>, paths: &[String], task: Option<&str>) -> Vec<&'a Memory> {
    let candidates: Vec<&Memory> = memories
        .iter()
        .filter(|m| matches!(m.status, Status::Superseded | Status::Invalidated) && matches!(m.kind, Kind::Decision | Kind::Knowledge))
        .filter(|m| {
            m.status != Status::Superseded
                || !m.superseded_by.as_deref().and_then(|s| by_id.get(s)).is_some_and(|s| opposed::refines(&m.title, &s.title))
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    // Shares over every note, history too, so a word only the rejected note holds still weighs.
    let shares: HashMap<String, f64> = match task {
        Some(task) if search::has_cjk(task) => {
            search::coverage(memories, &search::units(task)).into_iter().map(|c| (c.id, c.coverage)).collect()
        }
        Some(task) => search::word_shares(memories, task).unwrap_or_default(),
        None => HashMap::new(),
    };
    let mut named: Vec<(bool, f64, &Memory)> = candidates
        .into_iter()
        .filter_map(|m| {
            let anchored = m.anchors.iter().any(|a| paths.iter().any(|p| glob::anchor_matches(a, p)));
            let share = shares.get(&m.id).copied().unwrap_or(0.0);
            (anchored || share >= REJECTED_SHARE_MIN).then_some((anchored, share, m))
        })
        .collect();
    named.sort_by(|a, b| {
        b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)).then_with(|| b.2.updated_ms.cmp(&a.2.updated_ms)).then_with(|| a.2.id.cmp(&b.2.id))
    });
    named.into_iter().take(REJECTED_MAX).map(|(_, _, m)| m).collect()
}

/// Where a superseded note's line of successors ends: the first live one, else the last.
fn successor<'a>(memory: &Memory, by_id: &HashMap<&str, &'a Memory>) -> Option<&'a Memory> {
    let mut at = *by_id.get(memory.superseded_by.as_deref()?)?;
    let mut seen = HashSet::from([memory.id.as_str()]);
    while !at.status.is_live() && seen.insert(at.id.as_str()) {
        match at.superseded_by.as_deref().and_then(|s| by_id.get(s)) {
            Some(next) => at = next,
            None => break,
        }
    }
    Some(at)
}

/// A rejected note in one line: what replaced it (its title too when the brief does not
/// show it), or that it was invalidated.
fn rejected_line(memory: &Memory, next: Option<&Memory>, used: &Shown) -> String {
    let mut out = format!("- [{}] {} · `{}`", memory.kind.as_str(), clip(&memory.title, 120), memory.id);
    match next.filter(|_| memory.status == Status::Superseded) {
        Some(next) => {
            out.push_str(&format!(" — replaced by `{}`", next.id));
            if !used.contains(&next.id) {
                out.push_str(&format!(": {}", clip(&next.title, 80)));
            }
            if !next.status.is_live() {
                out.push_str(&format!(" (itself {})", next.status.as_str()));
            }
        }
        None => out.push_str(&format!(" — {}", memory.status.as_str())),
    }
    out.push('\n');
    out
}

/// The domains in play (SPEC §memory.brief): the deepest owners of the given paths - most
/// paths first, then the first named - else, with no path given, the domain holding most of
/// the task hits, a tie to the better-ranked hit; when no domain holds two hits, the domain the
/// task names, if one does - then `true`. At most [IN_PLAY_MAX].
fn in_play<'a>(
    map: &Map<'a>,
    paths: &[String],
    task: Option<&str>,
    task_hits: &[String],
    live: &[&Memory],
) -> (Vec<&'a Domain>, bool) {
    let mut tally: Vec<(&'a Domain, usize, usize)> = Vec::new();
    let mut probed = false;
    for (at, path) in paths.iter().enumerate() {
        let probe = if path.contains(['*', '?']) { domain::literal_prefix(path) } else { path.as_str() };
        if probe.is_empty() {
            continue;
        }
        probed = true;
        let Some(owner) = map.owner(probe) else { continue };
        match tally.iter_mut().find(|(d, _, _)| d.id == owner.id) {
            Some((_, n, _)) => *n += 1,
            None => tally.push((owner, 1, at)),
        }
    }
    // A path no domain owns is in none: the task's hits never say where the caller is - the
    // chat service of a map with no chat domain was "in" the live room its task words hit.
    if tally.is_empty() && !probed {
        for (at, id) in task_hits.iter().enumerate() {
            let Some(filed) = live.iter().find(|m| &m.id == id).and_then(|m| m.domain.as_deref()) else { continue };
            let Some(domain) = map.by_id(filed) else { continue };
            match tally.iter_mut().find(|(d, _, _)| d.id == domain.id) {
                Some((_, n, _)) => *n += 1,
                None => tally.push((domain, 1, at)),
            }
        }
        // Hits scattered one to a domain (or none) leave the vote to a tie-break: the task's own
        // words decide, when a domain's name holds them clearly. One hit is a vote.
        let scattered = tally.len() != 1 && tally.iter().all(|(_, n, _)| *n == 1);
        if let Some(named) = task.filter(|_| scattered).and_then(|task| named_domain(map, task)) {
            return (vec![named], true);
        }
        tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
        tally.truncate(1);
    }
    tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    (tally.into_iter().take(IN_PLAY_MAX).map(|(d, _, _)| d).collect(), false)
}

/// The domain a task names: its title (with its keywords) and summary scored on the task's units (search's CJK
/// coverage; the Latin words without stop-words) - the best only when its title holds
/// [NAMED_UNITS_MIN] of them and it is clear of the next. Coverage counts only units some domain
/// holds, so one shared word scores a domain 1.0, and one generic word in a title is no name:
/// "HTTP 示例" took an ASR question by 示例 and 模型. Seen in the wild: "订单统计少了一笔退款" had five hits
/// in five domains and the first was the platform's; "订单实时统计" holds 订单 and 统计.
fn named_domain<'a>(map: &Map<'a>, task: &str) -> Option<&'a Domain> {
    let (units, scored) = task_coverage(map, task);
    let best = scored.first()?;
    let next = scored.get(1).map(|c| c.blend()).unwrap_or(0.0);
    let domain = map.by_id(&best.id)?;
    let title = search::titled(&domain.title, &domain.keywords).to_lowercase();
    let named = units.iter().filter(|u| title.contains(u.as_str())).count() >= NAMED_UNITS_MIN;
    (named && best.blend() >= NAMED_BLEND_MIN && best.blend() - next >= NAMED_MARGIN).then_some(domain)
}

/// The task's units (search's CJK units; the Latin words without stop-words) and every domain
/// they touch, its title (with its keywords) and summary scored by coverage, best first.
fn task_coverage(map: &Map<'_>, task: &str) -> (Vec<String>, Vec<search::Covered>) {
    let mut units = search::cjk_units(task, search::UNITS_MAX);
    for term in related::salient(task) {
        if units.len() < search::UNITS_MAX && !search::has_cjk(&term) && !units.contains(&term) {
            units.push(term);
        }
    }
    let titles: Vec<String> = map.domains.iter().map(|d| search::titled(&d.title, &d.keywords)).collect();
    let rows = map.domains.iter().zip(&titles).map(|(d, title)| (d.id.as_str(), title.as_str(), d.summary.as_str()));
    let mut scored = search::coverage_of(rows, &units);
    // Stable: equals keep the map's order.
    scored.sort_by(|a, b| b.blend().total_cmp(&a.blend()));
    (units, scored)
}

/// The domains a task's words touch when it names none clearly - the backend and its page
/// both hold "cron", or a generic verb outranks the subject - so the agent still knows where
/// to look: the best [TOUCHED_MAX], each at least [TOUCHED_BLEND_MIN] and holding two of the
/// task's units (one, for a task of one) - one generic word touches nothing ("extended
/// thinking" is no business of the domain that keeps Extended JSON).
fn touched_domains<'a>(map: &Map<'a>, task: &str) -> Vec<&'a Domain> {
    let (units, scored) = task_coverage(map, task);
    let least = NAMED_UNITS_MIN.min(units.len());
    scored
        .iter()
        .filter(|c| c.blend() >= TOUCHED_BLEND_MIN && c.matched.len() >= least)
        .take(TOUCHED_MAX)
        .filter_map(|c| map.by_id(&c.id))
        .collect()
}

/// A hub cut to `room` characters at a paragraph (then a line), ending with where the rest
/// is; None when not even the pointer fits.
fn cut_hub(body: &str, room: usize, path: &str) -> Option<String> {
    let body = body.trim();
    if body.chars().count() <= room {
        return Some(body.to_string());
    }
    let pointer = format!("*… the rest of this hub: `memory_domain` path `{path}`*");
    // A cut inside a fence closes it, so the pointer is prose again.
    let close = |text: &str| if text.matches("```").count() % 2 == 1 { format!("{text}\n```") } else { text.to_string() };
    let fits = |text: &str| close(text).chars().count() + 2 + pointer.chars().count() <= room;
    let mut kept = String::new();
    for paragraph in body.split("\n\n") {
        let next = if kept.is_empty() { paragraph.to_string() } else { format!("{kept}\n\n{paragraph}") };
        if !fits(&next) {
            break;
        }
        kept = next;
    }
    if kept.is_empty() {
        // A first paragraph longer than the room: its first lines.
        for text_line in body.lines() {
            let next = if kept.is_empty() { text_line.to_string() } else { format!("{kept}\n{text_line}") };
            if !fits(&next) {
                break;
            }
            kept = next;
        }
    }
    let out = if kept.is_empty() { pointer } else { format!("{}\n\n{pointer}", close(&kept)) };
    (out.chars().count() <= room).then_some(out)
}

/// One map line, indented by depth.
fn map_entry(domain: &Domain, counts: &HashMap<String, DomainCounts>) -> String {
    let indent = "  ".repeat(domain.depth().saturating_sub(1));
    format!("{indent}- {}\n", map_line(domain, counts.get(&domain.id).copied().unwrap_or_default()))
}

/// A rendered brief: the markdown, and the notes it shows in reading order (what an
/// agent reading it is told: the evaluation's context, SPEC §memory.evaluation).
#[derive(Debug, Clone)]
pub struct Rendered {
    pub markdown: String,
    pub shown: Vec<String>,
}

pub fn render(input: BriefInput<'_>) -> String {
    compose(input).markdown
}

pub fn compose(mut input: BriefInput<'_>) -> Rendered {
    let max = input.max_chars.clamp(MAX_CHARS_MIN, MAX_CHARS_MAX);
    let mut budget = Budget { out: String::new(), used: 0, max };
    let now = input.now;
    let live: Vec<&Memory> = input.memories.iter().filter(|m| m.status.is_live()).collect();
    let map = Map::new(input.domains);
    let mapped = input.project.is_some() && !map.is_empty();
    let (counts, _) = domain::counts(input.memories);

    // 1. Header.
    let mut header = match input.project {
        Some((id, name)) => {
            let review = live.iter().filter(|m| m.status == Status::NeedsReview).count();
            let mut counts = format!("{} live notes", live.len());
            if review > 0 {
                counts.push_str(&format!(" · {review} need review"));
            }
            if mapped {
                counts.push_str(&format!(" · {} domains", input.domains.len()));
            }
            format!("# Project memory: {} ({id})\n{counts}\n", clip(name, 60))
        }
        None => "# Project memory\n".to_string(),
    };
    if let Some(notice) = &input.notice {
        header.push_str(notice);
        header.push('\n');
    }
    header.push_str(&format!("> {STANDING_NOTE}\n"));
    budget.max = budget.max.max(header.chars().count());
    budget.push(&header);
    if input.project.is_some() && live.is_empty() {
        budget.push("\nNo notes for this project yet.\n");
    }

    let mut used = Shown::default();
    let mut focus: Vec<String> = Vec::new();
    let paths: Vec<String> = input.paths.iter().map(|p| glob::normalize(p)).filter(|p| !p.is_empty()).collect();
    let task = input.task.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let focused = task.is_some() || !paths.is_empty();

    // The map's top level is held back from everything above it, so the rules never
    // hide the map.
    let full_max = budget.max;
    if mapped && !focused {
        let top: usize = "\n## Map\n".chars().count()
            + input.domains.iter().filter(|d| d.depth() == 1).map(|d| map_entry(d, &counts).chars().count()).sum::<usize>();
        let reserve = top.min(max * MAP_SHARE / 100);
        budget.max = full_max.saturating_sub(reserve).max(budget.used);
    }
    // The task's retrieval readout closes the brief, so its room is held back too: the exact
    // hits hold what the task names, a word hit its share of the task's words.
    let judged = task.and_then(|_| {
        readout::judge(input.task_hits.iter().enumerate().filter_map(|(at, id)| {
            let memory = live.iter().find(|m| &m.id == id)?;
            let hold = match input.task_shares.get(id) {
                _ if at < input.exact_hits => Hold::Exact,
                Some(share) => Hold::Words(*share),
                None => Hold::Loose,
            };
            Some((hold, memory.domain_path.as_deref()))
        }))
    });
    let answered = judged.as_ref().is_some_and(|r| r.verdict == Verdict::Strong);
    let readout = judged.map(|r| format!("\n{}", r.line()));
    if let Some(line) = &readout {
        budget.max = budget.max.saturating_sub(line.chars().count()).max(budget.used);
    }

    // 2. Where you are: the hubs of the domains in play, then their sub-domains.
    let (here, named) = if mapped { in_play(&map, &paths, task, &input.task_hits, &live) } else { (Vec::new(), false) };
    if !here.is_empty() {
        let each = max * HUBS_SHARE / 100 / here.len();
        let mut opened = false;
        for domain in &here {
            let mut head = String::new();
            if !opened {
                head.push_str("\n## Where you are\n");
            }
            head.push_str(&format!("\n### {} · `{}`\n", domain.title, domain.path));
            if !domain.confirmed {
                head.push_str("*unconfirmed*\n");
            }
            if !domain.summary.is_empty() {
                head.push_str(&domain.summary);
                head.push('\n');
            }
            if !budget.push(&head) {
                break;
            }
            opened = true;
            if !domain.body.trim().is_empty() {
                let room = each.min(budget.room()).saturating_sub(2);
                if let Some(hub) = cut_hub(&domain.body, room, &domain.path) {
                    budget.push(&format!("\n{hub}\n"));
                }
            }
            let mut first = true;
            for sub in map.children(&domain.path) {
                let entry = map_entry(sub, &counts).trim_start().to_string();
                let block = if first { format!("\nSub-domains:\n{entry}") } else { entry };
                if !budget.push(&block) {
                    break;
                }
                first = false;
            }
        }
    }
    // 2b. No domain in play, but the task's words touch some: those, one line each.
    let touched = match task {
        Some(task) if mapped && here.is_empty() => touched_domains(&map, task),
        _ => Vec::new(),
    };
    if !touched.is_empty() {
        let mut block = "\n## Domains your task's words touch\nNone clearly: read the likeliest with `memory_domain`.\n".to_string();
        for domain in &touched {
            block.push_str(map_entry(domain, &counts).trim_start());
        }
        budget.push(&block);
    }

    // 3. Anchored.
    if !paths.is_empty() {
        let mut hits: Vec<&Memory> = live
            .iter()
            .copied()
            .filter(|m| m.anchors.iter().any(|a| paths.iter().any(|p| glob::anchor_matches(a, p))))
            .collect();
        hits.sort_by(priority);
        let blocks = hits
            .into_iter()
            .map(|m| {
                let marks = brief_markers(m, input.net, &mut input.checker, now, &live);
                (m.id.clone(), item(m, &marks, EXCERPT_FULL))
            })
            .collect();
        focus.extend(section(&mut budget, "Anchored to your paths", blocks, &mut used));
    }

    // 4. Task. The domains in play, with the domains above and under them, are where the
    // task is. Its subject is known when its words name a domain or the symbol tier found
    // what it names: then a word hit filed anywhere else shares words with the task, not its
    // subject - a line each after the domains' notes (6b).
    let kin: HashSet<String> = here
        .iter()
        .flat_map(|d| map.subtree(d).into_iter().chain(map.ancestors(d).into_iter().map(|a| a.id.clone())))
        .collect();
    let exact: HashSet<&str> = input.task_hits.iter().take(input.exact_hits).map(String::as_str).collect();
    let subject = named || (!exact.is_empty() && !here.is_empty());
    let picked: Vec<&Memory> = input
        .task_hits
        .iter()
        .filter_map(|id| live.iter().find(|m| &m.id == id).copied())
        .filter(|m| !used.contains(&m.id))
        .take(TASK_HITS)
        .collect();
    // A hit after the first that the readout calls the tail - a word or two of the task, or its
    // meaning alone - is a line in 6b too: shown in full it read as the first did, and of the
    // hits after the first a model judged about a quarter of any use (§memory.evaluation). The
    // first is the readout's, even when its paths already showed it (3).
    let lead = input.task_hits.iter().find(|id| live.iter().any(|m| &m.id == *id)).map(String::as_str);
    let tail = |m: &Memory| {
        let hold = match input.task_shares.get(&m.id) {
            _ if exact.contains(m.id.as_str()) => Hold::Exact,
            Some(share) => Hold::Words(*share),
            None => Hold::Loose,
        };
        lead != Some(m.id.as_str()) && hold.tail()
    };
    let (hits, aside): (Vec<&Memory>, Vec<&Memory>) = picked.iter().copied().partition(|m| {
        (!subject || exact.contains(m.id.as_str()) || m.domain.as_deref().is_some_and(|d| kin.contains(d))) && !tail(m)
    });
    // What the linked notes follow: the anchored notes, then the hits that are the subject -
    // exact, filed where the task is, or unfiled (with no domain in play, every hit). A word
    // hit filed elsewhere shows, but its neighbours are another domain's business: a stray
    // `request` pulled in the production port and, by its links, every instance script.
    let mut strong: Vec<String> = focus.clone();
    let follows = |m: &Memory| {
        here.is_empty() || exact.contains(m.id.as_str()) || m.domain.as_deref().is_none_or(|d| kin.contains(d))
    };
    let followed: HashSet<&str> = hits.iter().filter(|m| follows(m)).map(|m| m.id.as_str()).collect();
    if !hits.is_empty() {
        let blocks = hits
            .into_iter()
            .map(|m| {
                let marks = brief_markers(m, input.net, &mut input.checker, now, &live);
                (m.id.clone(), item(m, &marks, EXCERPT_FULL))
            })
            .collect();
        let shown = section(&mut budget, "Related to your task", blocks, &mut used);
        strong.extend(shown.iter().filter(|id| followed.contains(id.as_str())).cloned());
        focus.extend(shown);
    }

    // 5. Linked: live 1-hop neighbours of the strong notes shown above.
    if !strong.is_empty() {
        let mut lines: Vec<(String, String)> = Vec::new();
        for shown in &strong {
            for link in input.net.touching(shown) {
                if !matches!(link.kind, LinkKind::DependsOn | LinkKind::Contradicts | LinkKind::Supports) {
                    continue;
                }
                let Some(other) = link.other(shown) else { continue };
                if used.contains(other) || lines.iter().any(|(id, _)| id == other) {
                    continue;
                }
                let Some(memory) = live.iter().find(|m| m.id == other).copied() else { continue };
                let mut marks = brief_markers(memory, input.net, &mut input.checker, now, &live);
                // The relation already says it: no "contradicted by" the note it contradicts.
                marks.retain(|m| !matches!(m, Marker::Contradicted(by) if by == shown));
                let relation = format!("{} `{shown}`", link.kind.phrase(link.from == memory.id));
                lines.push((memory.id.clone(), line(memory, Some(&relation), &marks)));
                if lines.len() >= LINKED_MAX {
                    break;
                }
            }
            if lines.len() >= LINKED_MAX {
                break;
            }
        }
        section(&mut budget, "Linked", lines, &mut used);
    }

    let by_id: HashMap<&str, &Memory> = input.memories.iter().map(|m| (m.id.as_str(), m)).collect();
    let dropped = if focused { rejected(input.memories, &by_id, &paths, task) } else { Vec::new() };

    // A focused brief that found nothing says so: the agent reads the code instead of a
    // neighbouring note (SKILL.md), and the router is one `memory_domain` call away.
    if focused && here.is_empty() && touched.is_empty() && focus.is_empty() && input.project.is_some() && !live.is_empty() {
        let current = if dropped.is_empty() { "" } else { " current" };
        let miss = match task {
            Some(task) => format!("\nNothing{current} in memory matches “{}”.\n", clip(task, 80)),
            None => format!("\nNothing{current} in memory about these paths.\n"),
        };
        budget.push(&miss);
    }

    // 6. In <path>: how many more notes each in-play domain files, and where they are - not a
    // line each. A focused brief is asked at every turn, and listing the domain's every note
    // each time repeated its index as the project's once was: a model judged about one such
    // line in eight of any use (§memory.evaluation). Its rules follow in 7.
    for domain in &here {
        let more = live
            .iter()
            .filter(|m| m.domain.as_deref() == Some(domain.id.as_str()) && !used.contains(&m.id))
            .filter(|m| !matches!(m.kind, Kind::Constraint | Kind::Preference))
            .count();
        if more > 0 {
            let notes = if more == 1 { "note" } else { "notes" };
            budget.push(&format!("\nIn `{}`: {more} more {notes} — `memory_domain` lists them all.\n", domain.path));
        }
    }

    // 6b. The task's words elsewhere, and its tail (see 4) - which a strong readout leaves out:
    // it says to answer from the first hit, and counts the tail; listed, the tail was what a
    // model judged of least use (§memory.evaluation). A mixed or weak one keeps it: there the
    // first hit may be wrong and the tail the alternatives.
    let blocks = aside
        .into_iter()
        .filter(|m| !used.contains(&m.id) && !(answered && tail(m)))
        .map(|m| {
            let marks = brief_markers(m, input.net, &mut input.checker, now, &live);
            (m.id.clone(), line(m, None, &marks))
        })
        .collect();
    section(&mut budget, "Also matching your task", blocks, &mut used);

    // 6c. Rejected before: history, labelled as such - never part of what the brief shows
    // as current (`shown`, the evaluation's context), like the readout.
    let mut opened = false;
    for memory in &dropped {
        let line = rejected_line(memory, successor(memory, &by_id), &used);
        let block = if opened {
            line
        } else {
            format!("\n## Rejected before\nSuperseded or invalidated: do not propose these again without a new reason.\n{line}")
        };
        if !budget.push(&block) {
            break;
        }
        opened = true;
    }

    // 7. Rules: the constraints and preferences, one line each - the title states the rule;
    // the ones the paths or the task point at are above in full. A pitfall is not a rule: it
    // shows where it bites, and in the index. With a map, the unfiled rules and those of the
    // domains in play and their ancestors: a rule filed elsewhere is that domain's. An
    // unconfirmed rule a confirmed note contradicts is no rule of the project's: an agent's
    // "run cargo fmt" against the user's "hand-formatted" rode every brief marked contradicted,
    // a rule to act on in the very place the brief says what to act on.
    let ruled: HashSet<&str> = here
        .iter()
        .flat_map(|d| map.ancestors(d).into_iter().chain(std::iter::once(*d)))
        .map(|d| d.id.as_str())
        .collect();
    let mut rules: Vec<&Memory> = live
        .iter()
        .copied()
        .filter(|m| matches!(m.kind, Kind::Constraint | Kind::Preference) && !used.contains(&m.id))
        .filter(|m| !mapped || m.domain.as_deref().is_none_or(|d| ruled.contains(d)))
        .filter(|m| !overruled(m, input.net, &live))
        .collect();
    rules.sort_by(priority);
    let blocks = rules
        .into_iter()
        .map(|m| {
            let marks = brief_markers(m, input.net, &mut input.checker, now, &live);
            (m.id.clone(), line(m, None, &marks))
        })
        .collect();
    section(&mut budget, "Rules", blocks, &mut used);

    // 8. Global, capped at a fifth of the budget.
    if !input.global.is_empty() {
        let cap = budget.used + max / 5;
        let outer = budget.max;
        budget.max = cap.min(outer);
        let mut global: Vec<&Memory> = input.global.iter().filter(|m| m.status.is_live()).collect();
        global.sort_by(priority);
        let mut none = Checker::disabled();
        let blocks = global
            .into_iter()
            .map(|m| {
                let (marks, _) = markers(m, input.global_net, &mut none, now);
                (m.id.clone(), line(m, None, &marks))
            })
            .collect();
        section(&mut budget, "Global", blocks, &mut used);
        budget.max = outer;
    }
    budget.max = full_max;
    if let Some(line) = &readout {
        budget.push(line);
    }

    // 9. Map (orientation only): every domain one line in tree order; when they do not all
    // fit, the top level first, then the next level down.
    if mapped && !focused {
        let mut room = budget.room();
        let heading = "\n## Map\n".chars().count();
        let mut chosen: HashSet<&str> = HashSet::new();
        if room > heading {
            room -= heading;
            let total = input.domains.len();
            'levels: for depth in 1..=crate::model::DOMAIN_DEPTH_MAX {
                for domain in input.domains.iter().filter(|d| d.depth() == depth) {
                    let cost = map_entry(domain, &counts).chars().count();
                    let reserve = if chosen.len() + 1 < total { TAIL_RESERVE } else { 0 };
                    if cost + reserve > room {
                        break 'levels;
                    }
                    room -= cost;
                    chosen.insert(domain.id.as_str());
                }
            }
        }
        if !chosen.is_empty() {
            let mut block = String::from("\n## Map\n");
            for domain in input.domains.iter().filter(|d| chosen.contains(d.id.as_str())) {
                block.push_str(&map_entry(domain, &counts));
            }
            let hidden = input.domains.len() - chosen.len();
            if hidden > 0 {
                block.push_str(&format!("… {hidden} more domains — memory_domain\n"));
            }
            budget.max = budget.max.max(budget.used + block.chars().count());
            budget.push(&block);
        }
    }

    // 10. Index (Unfiled, with a map; orientation only): everything else, one line each -
    // pitfalls, decisions, knowledge, then the rest; newest first within a kind.
    if input.project.is_some() && !focused {
        let heading = if mapped { "Unfiled" } else { "Index" };
        let mut rest: Vec<&Memory> = live
            .iter()
            .copied()
            .filter(|m| !used.contains(&m.id))
            .filter(|m| !mapped || m.domain.is_none())
            .collect();
        rest.sort_by(index_order);
        let total = rest.len();
        let mut shown = 0usize;
        let mut opened = false;
        for memory in &rest {
            let marks = brief_markers(memory, input.net, &mut input.checker, now, &live);
            let entry = line(memory, None, &marks);
            let block = if opened { entry } else { format!("\n## {heading}\n{entry}") };
            let reserve = if shown + 1 < total { TAIL_RESERVE } else { 0 };
            if budget.used + block.chars().count() + reserve > budget.max {
                break;
            }
            budget.push(&block);
            used.insert(&memory.id);
            opened = true;
            shown += 1;
        }
        if shown < total {
            let more = format!("… {} more — memory_search / memory_get\n", total - shown);
            let block = if opened { more } else { format!("\n## {heading}\n{more}") };
            budget.max = budget.max.max(budget.used + block.chars().count());
            budget.push(&block);
        }
    }
    Rendered { markdown: budget.out, shown: used.order }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Anchor, Link, Origin};

    fn note(id: &str, kind: Kind, title: &str, anchors: &[&str], updated: i64) -> Memory {
        Memory {
            id: id.into(),
            scope: "p-00000001".into(),
            kind,
            title: title.into(),
            content: format!("{title} - because reasons."),
            status: Status::Active,
            origin: Origin::Agent,
            actor: "mcp:codex".into(),
            producer: crate::model::Producer::Agent,
            transport: crate::model::Transport::Mcp,
            confirmed: false,
            source_verified: false,
            anchors: anchors
                .iter()
                .map(|p| Anchor { path: p.to_string(), glob: p.contains('*'), hash: None })
                .collect(),
            refs: Vec::new(),
            keywords: Vec::new(),
            domain: None,
            domain_path: None,
            superseded_by: None,
            revision: 1,
            created_ms: updated,
            updated_ms: updated,
            verified_ms: None,
        }
    }

    fn input<'a>(memories: &'a [Memory], net: &'a Net, empty: &'a Net, paths: &[&str]) -> BriefInput<'a> {
        BriefInput {
            project: Some(("p-00000001", "swiss")),
            notice: None,
            memories,
            net,
            domains: &[],
            checker: Checker::disabled(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            task: None,
            task_hits: Vec::new(),
            exact_hits: 0,
            task_shares: HashMap::new(),
            global: &[],
            global_net: empty,
            max_chars: 6000,
            now: 10 * 86_400_000,
        }
    }

    #[test]
    fn anchored_notes_lead_and_their_dependencies_follow() {
        let memories = vec![
            note("m-0000000001", Kind::Pitfall, "BEGIN IMMEDIATE for writes", &["crates/swiss-memory/src/store.rs"], 5),
            note("m-0000000002", Kind::Decision, "One connection per process", &[], 4),
            note("m-0000000003", Kind::Knowledge, "Unrelated fact", &[], 3),
        ];
        let links = vec![Link {
            from: "m-0000000001".into(),
            to: "m-0000000002".into(),
            kind: LinkKind::DependsOn,
            note: None,
            actor: "cli".into(),
            created_ms: 0,
        }];
        let net = Net::from_memories(&memories, links);
        let empty = Net::default();
        let text = render(input(&memories, &net, &empty, &["crates/swiss-memory/src/store.rs"]));
        assert!(text.contains(STANDING_NOTE));
        let anchored = text.find("## Anchored to your paths").expect("anchored section");
        let linked = text.find("## Linked").expect("linked section");
        assert!(anchored < linked);
        assert!(text.contains("### [pitfall] BEGIN IMMEDIATE for writes · `m-0000000001`\n*unconfirmed*\n"));
        // The link is said once, by the neighbour's line.
        assert!(text.contains("- [decision] One connection per process · `m-0000000002` — required by `m-0000000001` · unconfirmed\n"));
        assert!(!text.contains("links:"));
        // Focused: what the paths touch, never the rest of the project.
        assert!(!text.contains("m-0000000003") && !text.contains("## Index"), "{text}");
        assert_eq!(text.matches("m-0000000002").count(), 1, "a record appears in one section only");
        // Nothing ordinary is said: not who wrote it, not when.
        for noise in ["mcp:codex", "agent (", "today", "need review"] {
            assert!(!text.contains(noise), "{noise}:\n{text}");
        }
    }

    #[test]
    fn rules_are_one_line_and_a_pitfall_is_not_a_rule() {
        let mut rule = note("m-0000000001", Kind::Constraint, "Never run cargo fmt", &[], 5);
        rule.confirmed = true;
        rule.content = format!("Never run cargo fmt. {}", "The tree is hand-formatted. ".repeat(20));
        let mut pitfall = note("m-0000000002", Kind::Pitfall, "Windows locks the running exe", &[], 4);
        pitfall.confirmed = true;
        let mut decision = note("m-0000000003", Kind::Decision, "One store per home", &[], 6);
        decision.confirmed = true;
        let memories = vec![rule, pitfall, decision];
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();
        let text = render(input(&memories, &net, &empty, &[]));
        let rules = text.find("## Rules\n- [constraint] Never run cargo fmt · `m-0000000001`\n").expect("the rule, one line");
        assert!(!text.contains("hand-formatted"), "a rule's text stays behind memory_get");
        // The index: pitfalls before decisions, whatever their age; confirmed notes carry no tail.
        let index = text.find("## Index\n- [pitfall] Windows locks the running exe · `m-0000000002`\n- [decision] One store per home · `m-0000000003`\n");
        assert!(index.is_some_and(|at| at > rules), "{text}");
    }

    #[test]
    fn a_guess_the_user_contradicts_is_no_rule() {
        let mut user = note("m-0000000001", Kind::Constraint, "The tree is hand-formatted", &[], 5);
        user.confirmed = true;
        let guess = note("m-0000000002", Kind::Preference, "Run cargo fmt before committing", &[], 6);
        let tabs = note("m-0000000003", Kind::Preference, "Tabs in the panel", &[], 4);
        let spaces = note("m-0000000004", Kind::Preference, "Spaces in the panel", &[], 3);
        let memories = vec![user, guess, tabs, spaces];
        let contradicts = |from: &str, to: &str| Link {
            from: from.into(),
            to: to.into(),
            kind: LinkKind::Contradicts,
            note: None,
            actor: "cli".into(),
            created_ms: 0,
        };
        let net = Net::from_memories(&memories, vec![contradicts("m-0000000001", "m-0000000002"), contradicts("m-0000000003", "m-0000000004")]);
        let empty = Net::default();
        let text = render(input(&memories, &net, &empty, &[]));
        let rules = text.find("## Rules\n").expect("rules");
        let index = text.find("## Index\n").expect("index");
        let at = |head: &str| text.find(head).unwrap_or_else(|| panic!("{head}:\n{text}"));
        // The user's rule stands, unmarked: the guess it overrules casts no doubt on it. Two
        // guesses at odds both stay rules, each marked.
        for head in [
            "- [constraint] The tree is hand-formatted · `m-0000000001`\n",
            "- [preference] Tabs in the panel · `m-0000000003` · unconfirmed · ⚠ contradicted by `m-0000000004`",
            "- [preference] Spaces in the panel · `m-0000000004` · unconfirmed · ⚠ contradicted by `m-0000000003`",
        ] {
            assert!(at(head) > rules && at(head) < index, "{head}:\n{text}");
        }
        // The guess the user contradicts is no rule: a line in the index, marked.
        let guess = "- [preference] Run cargo fmt before committing · `m-0000000002` · unconfirmed · ⚠ contradicted by `m-0000000001`\n";
        assert!(at(guess) > index, "{text}");
        // Where the task points at it, it shows in full.
        let mut asked = input(&memories, &net, &empty, &[]);
        asked.task = Some("cargo fmt".into());
        asked.task_hits = vec!["m-0000000002".into()];
        let text = render(asked);
        assert!(text.contains("## Related to your task\n\n### [preference] Run cargo fmt before committing · `m-0000000002`\n"), "{text}");
        // The user's rule follows it as a link, which says the conflict once.
        assert!(text.contains("- [constraint] The tree is hand-formatted · `m-0000000001` — contradicts `m-0000000002`\n"), "{text}");
        // Where the paths point at the user's note, in full: still no mark.
        let mut anchored = memories.clone();
        anchored[0].anchors = vec![crate::model::Anchor { path: "rustfmt.toml".into(), glob: false, hash: None }];
        let net = Net::from_memories(&anchored, vec![contradicts("m-0000000001", "m-0000000002")]);
        let text = render(input(&anchored, &net, &empty, &["rustfmt.toml"]));
        assert!(text.contains("### [constraint] The tree is hand-formatted · `m-0000000001`\nThe tree"), "{text}");
        // A confirmed note at odds with another confirmed one keeps the mark: neither overrules.
        let mut both = memories.clone();
        both[1].confirmed = true;
        let net = Net::from_memories(&both, vec![contradicts("m-0000000001", "m-0000000002")]);
        let text = render(input(&both, &net, &empty, &[]));
        assert!(text.contains("- [constraint] The tree is hand-formatted · `m-0000000001` · ⚠ contradicted by `m-0000000002`\n"), "{text}");
    }

    #[test]
    fn a_task_brief_closes_with_the_retrieval_readout() {
        let mut memories: Vec<Memory> = (1..=6)
            .map(|n| note(&format!("m-000000000{n}"), Kind::Knowledge, &format!("Note number {n}"), &["src/lib.rs"], n))
            .collect();
        memories[0].content = format!("{} ", "Long text that fills the budget.".repeat(40));
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();
        let asked = |hits: &[&str], exact: usize, shares: &[(&str, f64)]| {
            let mut brief = input(&memories, &net, &empty, &[]);
            brief.task = Some("what the task asks".into());
            brief.task_hits = hits.iter().map(|h| h.to_string()).collect();
            brief.exact_hits = exact;
            brief.task_shares = shares.iter().map(|(id, s)| (id.to_string(), *s)).collect();
            brief
        };
        // The symbol tier's hit holds what the task names; a word hit holding a fifth shares a
        // word: answer, and the line closes the brief.
        let text = render(asked(&["m-0000000002", "m-0000000003"], 1, &[("m-0000000003", 0.25)]));
        assert!(
            text.ends_with(
                "\nRetrieval: strong (1 exact or full, 1 sharing a word or two) — answer from the first hit, checking it against the \
                 code it names.\n"
            ),
            "{text}"
        );
        // Every hit a word or two, or found by meaning alone: rephrase.
        let text = render(asked(&["m-0000000002", "m-0000000003"], 0, &[("m-0000000002", 0.3)]));
        assert!(text.contains("\nRetrieval: weak (2 sharing a word or two) — no hit holds much of the query"), "{text}");
        // Half the words: confirm.
        let text = render(asked(&["m-0000000002"], 0, &[("m-0000000002", 0.5)]));
        assert!(text.contains("\nRetrieval: mixed (1 partial) — confirm the first hit"), "{text}");
        // Its room is held back: at the least budget, a brief full of text still ends with it.
        let mut tight = asked(&["m-0000000001", "m-0000000002"], 2, &[]);
        tight.max_chars = MAX_CHARS_MIN;
        let text = render(tight);
        assert!(text.chars().count() <= MAX_CHARS_MIN && text.ends_with("checking it against the code it names.\n"), "{text}");
        // No task, no readout: the paths point, nothing was retrieved; and a task matching nothing
        // already says so.
        assert!(!render(input(&memories, &net, &empty, &["src/lib.rs"])).contains("Retrieval:"));
        assert!(!render(input(&memories, &net, &empty, &[])).contains("Retrieval:"));
        let text = render(asked(&[], 0, &[]));
        assert!(text.contains("Nothing in memory matches") && !text.contains("Retrieval:"), "{text}");
    }

    #[test]
    fn a_marker_names_its_id_or_path_as_code() {
        assert_eq!(md_marker(&Marker::Contradicted("m-0000000009".into())), "⚠ contradicted by `m-0000000009`");
        assert_eq!(md_marker(&Marker::AnchorChanged("src/*.rs".into())), "⚠ anchor changed: `src/*.rs`");
        assert_eq!(md_marker(&Marker::Unconfirmed), "unconfirmed");
        assert_eq!(
            short_markers(&[Marker::Unconfirmed, Marker::Contradicted("m-0000000009".into())]),
            "unconfirmed · ⚠ contradicted by `m-0000000009`"
        );
    }

    #[test]
    fn the_index_reports_what_did_not_fit() {
        let memories: Vec<Memory> = (0..200)
            .map(|i| note(&format!("m-{i:010x}"), Kind::Knowledge, &format!("fact number {i} {}", "x".repeat(60)), &[], i))
            .collect();
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.max_chars = 2000;
        let text = render(brief);
        assert!(text.chars().count() <= 2000);
        assert!(text.contains(" more — memory_search / memory_get"));
    }

    #[test]
    fn an_empty_project_says_so() {
        let net = Net::default();
        let empty = Net::default();
        let text = render(input(&[], &net, &empty, &[]));
        assert!(text.contains("No notes for this project yet."));
    }

    fn domain(id: &str, path: &str, title: &str, owns: &[&str], body: &str) -> Domain {
        Domain {
            id: id.into(),
            scope: "p-00000001".into(),
            path: path.into(),
            title: title.into(),
            summary: format!("{title} in one line"),
            body: body.into(),
            owns: owns.iter().map(|o| o.to_string()).collect(),
            keywords: Vec::new(),
            origin: Origin::User,
            actor: "panel".into(),
            confirmed: true,
            revision: 1,
            created_ms: 0,
            updated_ms: 0,
            verified_ms: 0,
        }
    }

    fn filed(mut memory: Memory, domain: &Domain) -> Memory {
        memory.domain = Some(domain.id.clone());
        memory.domain_path = Some(domain.path.clone());
        memory.confirmed = true;
        memory
    }

    #[test]
    fn a_map_brief_is_layered() {
        let hub = "## Overview\nThe knowledge core: `MemoryService` in `crates/swiss-memory/src/service.rs`.\n\n\
                   ## Entry points\n- CLI `src/memory_cli.rs`\n- MCP `src/plugins/memory_mcp.rs`";
        let mut panel = domain("d-00000003", "panel", "Panel", &["crates/swiss-panel"], "");
        panel.confirmed = false;
        let domains = vec![
            domain("d-00000001", "memory", "Memory", &["crates/swiss-memory"], hub),
            domain("d-00000002", "memory/store", "Store", &["crates/swiss-memory/src/store.rs"], "SQLite, one connection."),
            panel,
        ];
        let (memory, store, panel) = (&domains[0], &domains[1], &domains[2]);
        let memories = vec![
            filed(note("m-0000000001", Kind::Pitfall, "BEGIN IMMEDIATE for writes", &["crates/swiss-memory/src/store.rs"], 9), store),
            filed(note("m-0000000002", Kind::Decision, "One connection per process", &[], 8), store),
            filed(note("m-0000000003", Kind::Constraint, "Panel strings come from tr()", &[], 7), panel),
            filed(note("m-0000000004", Kind::Constraint, "Notes are never truncated on write", &[], 6), memory),
            {
                let mut rule = note("m-0000000005", Kind::Constraint, "Never run cargo fmt", &[], 5);
                rule.confirmed = true;
                rule
            },
            note("m-0000000006", Kind::Knowledge, "Port 19999 is production", &[], 4),
            filed(note("m-0000000007", Kind::Knowledge, "The rail is virtualised", &[], 3), panel),
        ];
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();

        // Working on the store: its hub, its notes, the rules that bind there - not the map.
        let mut brief = input(&memories, &net, &empty, &["crates/swiss-memory/src/store.rs"]);
        brief.domains = &domains;
        let text = render(brief);
        assert!(text.contains("3 domains"), "{text}");
        let order = [
            "## Where you are",
            "## Anchored to your paths",
            "\nIn `memory/store`: 1 more note — `memory_domain` lists them all.\n",
            "## Rules",
        ];
        let at: Vec<usize> = order.iter().map(|h| text.find(h).unwrap_or_else(|| panic!("{h}:\n{text}"))).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(text.contains("\n### Store · `memory/store`\nStore in one line\n\nSQLite, one connection.\n"), "{text}");
        // The domain's other note is counted, not listed: `memory_domain` reads it.
        assert!(!text.contains("m-0000000002"), "{text}");
        // Rules: the unfiled one and the ancestor's, never another domain's.
        assert!(text.contains("- [constraint] Notes are never truncated on write · `m-0000000004`\n"));
        assert!(text.contains("- [constraint] Never run cargo fmt · `m-0000000005`\n"));
        assert!(!text.contains("m-0000000003") && !text.contains("m-0000000007"), "{text}");
        assert!(!text.contains("## Map") && !text.contains("## Unfiled") && !text.contains("m-0000000006"), "{text}");

        // The session start: the router - rules, the map, the unfiled notes; no hub.
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.domains = &domains;
        let text = render(brief);
        assert!(!text.contains("## Where you are") && !text.contains("lists them all"), "{text}");
        assert!(text.contains("m-0000000005") && !text.contains("m-0000000004"), "{text}");
        assert!(text.contains(
            "\n## Map\n- `memory` Memory — Memory in one line · 1 note\n  - `memory/store` Store — Store in one line · 2 notes\n\
             - `panel` Panel — Panel in one line · 2 notes · unconfirmed\n"
        ), "{text}");
        assert!(text.contains("\n## Unfiled\n- [knowledge] Port 19999 is production · `m-0000000006`"));
        assert!(!text.contains("## Index"));

        // A task hit in a domain puts that domain in play.
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.domains = &domains;
        brief.task = Some("virtual rail".into());
        brief.task_hits = vec!["m-0000000007".into()];
        let text = render(brief);
        assert!(text.contains("\n### Panel · `panel`\n*unconfirmed*\n"), "{text}");
        assert!(text.contains("- [constraint] Panel strings come from tr() · `m-0000000003`"), "{text}");
        assert!(!text.contains("## Map") && !text.contains("Nothing in memory"), "{text}");
        // Working on a file no domain owns, the same hit says nothing of where you are.
        let mut brief = input(&memories, &net, &empty, &["crates/swiss-im/src/chat.rs"]);
        brief.domains = &domains;
        brief.task = Some("virtual rail".into());
        brief.task_hits = vec!["m-0000000007".into()];
        let text = render(brief);
        assert!(!text.contains("## Where you are") && !text.contains("### Panel"), "{text}");
        assert!(text.contains("The rail is virtualised · `m-0000000007`"), "{text}");

        // A task nothing matches: said in one line, and no map to fill the gap.
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.domains = &domains;
        brief.task = Some("extended thinking".into());
        let text = render(brief);
        assert!(text.contains("\nNothing in memory matches “extended thinking”.\n"), "{text}");
        assert!(!text.contains("## Map") && !text.contains("## Unfiled") && !text.contains("## Where you are"), "{text}");
        // The project-wide rules still bind.
        assert!(text.contains("m-0000000005"), "{text}");

        // Words two domains hold alike name neither: both are listed, one line each.
        let mut jobs = domain("d-00000004", "jobs", "Scheduled jobs", &["crates/swiss-jobs"], "");
        jobs.keywords = vec!["cron".into()];
        let mut page = domain("d-00000005", "panel/jobs", "Jobs page", &["crates/swiss-panel/src/jobs.ts"], "");
        page.keywords = vec!["cron".into(), "scheduled jobs".into()];
        let both = vec![domains[0].clone(), domains[2].clone(), jobs, page];
        let mut brief = input(&[], &empty, &empty, &[]);
        brief.domains = &both;
        brief.task = Some("add cron validation to scheduled jobs".into());
        let text = render(brief);
        assert!(!text.contains("## Where you are"), "{text}");
        assert!(text.contains("\n## Domains your task's words touch\nNone clearly: read the likeliest with `memory_domain`.\n- `"), "{text}");
        assert!(text.contains("- `jobs` Scheduled jobs — Scheduled jobs in one line\n"), "{text}");
        assert!(text.contains("- `panel/jobs` Jobs page — Jobs page in one line\n"), "{text}");
        assert!(!text.contains("`memory` Memory") && !text.contains("- `panel` Panel"), "{text}");
        // One word in common is no pointer.
        let mut brief = input(&[], &empty, &empty, &[]);
        brief.domains = &both;
        brief.task = Some("cron expression thinking budget".into());
        let text = render(brief);
        assert!(!text.contains("## Domains your task's words touch"), "{text}");

        // A long hub is cut at a paragraph and says where the rest is.
        let long = format!("{hub}\n\n{}", "A paragraph of flow. ".repeat(200));
        let big = vec![domain("d-00000001", "memory", "Memory", &["crates/swiss-memory"], &long)];
        let mut brief = input(&memories, &net, &empty, &["crates/swiss-memory/src/brief.rs"]);
        brief.domains = &big;
        brief.max_chars = 2000;
        let text = render(brief);
        assert!(text.chars().count() <= 2000, "{}", text.chars().count());
        assert!(text.contains("## Entry points"), "{text}");
        assert!(text.contains("*… the rest of this hub: `memory_domain` path `memory`*"), "{text}");
    }

    #[test]
    fn scattered_task_hits_leave_the_domain_to_the_task_s_words() {
        let mut rt = domain("d-00000003", "orders/rt", "订单实时统计", &[], "");
        rt.summary = "V3 推送订单事件，RT listener 增量更新 dws 统计".into();
        let domains = vec![
            domain("d-00000001", "platform", "平台与横切机制", &[], ""),
            domain("d-00000002", "payment", "支付与结算", &[], ""),
            rt,
        ];
        let memories = vec![
            filed(note("m-0000000001", Kind::Pitfall, "定时任务没有分布式锁，统计跑两遍", &[], 9), &domains[0]),
            filed(note("m-0000000002", Kind::Pitfall, "支付对账统计漏了 return", &[], 8), &domains[1]),
            filed(note("m-0000000003", Kind::Pitfall, "RT listener 吞掉异常，事件丢失", &[], 7), &domains[2]),
            filed(note("m-0000000004", Kind::Knowledge, "定时任务单机运行", &[], 6), &domains[0]),
        ];
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();
        let brief = |task: &str, hits: &[&str]| {
            let mut brief = input(&memories, &net, &empty, &[]);
            brief.domains = &domains;
            brief.task = Some(task.into());
            brief.task_hits = hits.iter().map(|h| h.to_string()).collect();
            brief.task_shares = hits.iter().map(|h| (h.to_string(), 0.5)).collect();
            render(brief)
        };
        // One hit in each of two domains: the task's words name a third, which says how many notes
        // it files; the scattered hits are a line each after that, never in full.
        let text = brief("订单统计少了一笔退款", &["m-0000000001", "m-0000000002"]);
        assert!(text.contains("\n### 订单实时统计 · `orders/rt`\n"), "{text}");
        let order = ["## Where you are", "\nIn `orders/rt`: 1 more note — `memory_domain` lists them all.\n", "## Also matching your task\n- [pitfall] 定时任务没有分布式锁，统计跑两遍 · `m-0000000001`\n- [pitfall] 支付对账统计漏了 return · `m-0000000002`\n"];
        let at: Vec<usize> = order.iter().map(|h| text.find(h).unwrap_or_else(|| panic!("{h}:\n{text}"))).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(!text.contains("### 平台与横切机制") && !text.contains("## Related to your task") && !text.contains("because reasons"), "{text}");
        // Two hits in one domain are a vote, whatever the words.
        let text = brief("订单统计少了一笔退款", &["m-0000000001", "m-0000000002", "m-0000000004"]);
        assert!(text.contains("\n### 平台与横切机制 · `platform`\n") && !text.contains("`orders/rt`"), "{text}");
        // One shared word is no name, nor one in the title and one in the summary: the tie goes
        // to the better-ranked hit as before, in full.
        for task in ["支付少了一笔", "统计少了一笔 dws"] {
            let text = brief(task, &["m-0000000001", "m-0000000002"]);
            assert!(text.contains("\n### 平台与横切机制 · `platform`\n"), "{text}");
            assert!(!text.contains("### 支付与结算") && !text.contains("### 订单实时统计"), "{text}");
            assert!(text.contains("## Related to your task") && !text.contains("## Also matching"), "{text}");
        }
    }

    #[test]
    fn a_stray_word_hit_s_neighbours_stay_out() {
        let domains = vec![
            domain("d-00000001", "mcp", "MCP servers", &[], ""),
            domain("d-00000002", "build", "Build and ship", &[], ""),
        ];
        let memories = vec![
            filed(note("m-0000000001", Kind::Decision, "A proc server idles until DEFAULT_IDLE_MS", &[], 9), &domains[0]),
            filed(note("m-0000000002", Kind::Constraint, "Port 19999 takes no request while iterating", &[], 8), &domains[1]),
            filed(note("m-0000000003", Kind::Pitfall, "Stop the test instance by its PID", &[], 7), &domains[1]),
            filed(note("m-0000000004", Kind::Knowledge, "Reaping waits for the last call", &[], 6), &domains[0]),
        ];
        let link = |from: &str, to: &str| Link {
            from: from.into(),
            to: to.into(),
            kind: LinkKind::DependsOn,
            note: None,
            actor: "cli".into(),
            created_ms: 0,
        };
        let net = Net::from_memories(&memories, vec![link("m-0000000001", "m-0000000004"), link("m-0000000002", "m-0000000003")]);
        let empty = Net::default();
        let task = "what happens after DEFAULT_IDLE_MS with no request";
        let brief = |mapped: bool, exact: usize| {
            let mut brief = input(&memories, &net, &empty, &[]);
            if mapped {
                brief.domains = &domains;
            }
            brief.task = Some(task.into());
            brief.task_hits = vec!["m-0000000001".into(), "m-0000000002".into()];
            brief.exact_hits = exact;
            brief.task_shares = HashMap::from([("m-0000000002".to_string(), 0.5)]);
            render(brief)
        };
        let linked = |text: &str| text.split("## Linked").nth(1).map(|s| s.split("\n## ").next().unwrap_or("").to_string());
        // The symbol tier found what the task names: the word hit filed elsewhere shares a word,
        // not the subject - one line after the domain's notes, and its neighbour stays out.
        let text = brief(true, 1);
        assert!(text.contains("## Related to your task\n\n### [decision] A proc server idles"), "{text}");
        assert!(text.contains("## Also matching your task\n- [constraint] Port 19999 takes no request while iterating"), "{text}");
        assert!(linked(&text).is_some_and(|l| l.contains("m-0000000004")), "{text}");
        assert!(!text.contains("m-0000000003"), "{text}");
        // No exact hit: the domain in play is the first hit's and both show in full, but only the
        // hit filed where the task is brings its links.
        let text = brief(true, 0);
        assert!(text.contains("### [constraint] Port 19999 takes no request") && !text.contains("## Also matching"), "{text}");
        assert!(linked(&text).is_some_and(|l| l.contains("m-0000000004")) && !text.contains("m-0000000003"), "{text}");
        // No map, no domain in play: every hit is the subject.
        let text = brief(false, 1);
        assert!(linked(&text).is_some_and(|l| l.contains("m-0000000003") && l.contains("m-0000000004")), "{text}");
    }

    #[test]
    fn a_task_hit_sharing_a_word_or_two_is_a_line() {
        let memories: Vec<Memory> =
            (1..=5).map(|n| note(&format!("m-000000000{n}"), Kind::Knowledge, &format!("Note number {n}"), &[], n)).collect();
        let link = Link {
            from: "m-0000000003".into(),
            to: "m-0000000005".into(),
            kind: LinkKind::DependsOn,
            note: None,
            actor: "cli".into(),
            created_ms: 0,
        };
        let net = Net::from_memories(&memories, vec![link]);
        let empty = Net::default();
        let brief = |hits: &[&str], exact: usize, shares: &[(&str, f64)]| {
            let mut brief = input(&memories, &net, &empty, &[]);
            brief.task = Some("what the task asks".into());
            brief.task_hits = hits.iter().map(|h| h.to_string()).collect();
            brief.exact_hits = exact;
            brief.task_shares = shares.iter().map(|(id, s)| (id.to_string(), *s)).collect();
            render(brief)
        };
        let text = brief(
            &["m-0000000001", "m-0000000002", "m-0000000003", "m-0000000004"],
            0,
            &[("m-0000000001", 0.3), ("m-0000000002", 0.5), ("m-0000000003", 0.25)],
        );
        // The first hit is in full whatever it holds, a later one holding half the task's words too.
        assert!(text.contains("## Related to your task\n\n### [knowledge] Note number 1 · `m-0000000001`\n"), "{text}");
        assert!(text.contains("\n### [knowledge] Note number 2 · `m-0000000002`\n"), "{text}");
        // A word or two, or meaning alone (no share): a line each, and their links are not followed
        // - the readout is mixed, so they may be the alternatives to a wrong first hit.
        assert!(
            text.contains(
                "## Also matching your task\n- [knowledge] Note number 3 · `m-0000000003` · unconfirmed\n\
                 - [knowledge] Note number 4 · `m-0000000004` · unconfirmed\n"
            ),
            "{text}"
        );
        assert!(!text.contains("## Linked") && !text.contains("m-0000000005"), "{text}");
        // The readout still judges every hit.
        assert!(text.contains("Retrieval: mixed (1 partial, 3 sharing a word or two)"), "{text}");
        // An exact hit holds what the task names, whatever its words: in full, its links followed.
        let text = brief(&["m-0000000004", "m-0000000003"], 2, &[]);
        assert!(text.contains("\n### [knowledge] Note number 3 · `m-0000000003`\n") && !text.contains("## Also matching"), "{text}");
        assert!(text.contains("## Linked\n- [knowledge] Note number 5 · `m-0000000005` — required by `m-0000000003`"), "{text}");
        // A strong readout - the first hit holds what the task names - leaves the tail out: it says
        // to answer from that hit and counts the rest. A partial hit still shows in full.
        let text = brief(&["m-0000000004", "m-0000000001", "m-0000000003"], 1, &[("m-0000000001", 0.3), ("m-0000000003", 0.5)]);
        assert!(text.contains("\n### [knowledge] Note number 3 · `m-0000000003`\n"), "{text}");
        assert!(!text.contains("## Also matching") && !text.contains("`m-0000000001`"), "{text}");
        assert!(text.contains("Retrieval: strong (1 exact or full, 1 partial, 1 sharing a word or two)"), "{text}");
        // The first hit is the readout's even when the paths already showed it: the next one is not
        // promoted to first, and a word or two of the task stays a line.
        let mut anchored = memories.clone();
        anchored[0].anchors = vec![Anchor { path: "src/one.rs".into(), glob: false, hash: None }];
        let mut paths = input(&anchored, &net, &empty, &["src/one.rs"]);
        paths.task = Some("what the task asks".into());
        paths.task_hits = vec!["m-0000000001".into(), "m-0000000002".into(), "m-0000000004".into()];
        paths.task_shares = HashMap::from([("m-0000000001".to_string(), 0.5), ("m-0000000002".to_string(), 0.3)]);
        let text = render(paths);
        assert!(text.contains("## Anchored to your paths\n\n### [knowledge] Note number 1 · `m-0000000001`\n"), "{text}");
        assert!(
            text.contains(
                "## Also matching your task\n- [knowledge] Note number 2 · `m-0000000002` · unconfirmed\n\
                 - [knowledge] Note number 4 · `m-0000000004` · unconfirmed\n"
            ),
            "{text}"
        );
        assert!(!text.contains("## Related to your task"), "{text}");
    }

    #[test]
    fn a_flat_task_brief_is_the_hits_and_the_rules() {
        let mut rule = note("m-0000000001", Kind::Constraint, "Never run cargo fmt", &[], 5);
        rule.confirmed = true;
        let memories = vec![
            rule,
            note("m-0000000002", Kind::Pitfall, "Windows locks the running exe", &[], 4),
            note("m-0000000003", Kind::Knowledge, "One store per home", &[], 3),
        ];
        let net = Net::from_memories(&memories, Vec::new());
        let empty = Net::default();
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.task = Some("release build fails".into());
        brief.task_hits = vec!["m-0000000002".into()];
        let text = render(brief);
        assert!(text.contains("## Related to your task\n\n### [pitfall] Windows locks the running exe"), "{text}");
        assert!(text.contains("## Rules\n- [constraint] Never run cargo fmt"), "{text}");
        assert!(!text.contains("## Index") && !text.contains("m-0000000003"), "{text}");
        // A blank task is no task: the session-start brief.
        let mut brief = input(&memories, &net, &empty, &[]);
        brief.task = Some("  ".into());
        assert!(render(brief).contains("## Index"));
    }

    #[test]
    fn a_hub_cut_closes_its_fence() {
        let body = format!("Intro.\n\n```rust\nfn a() {{}}\n\nfn b() {{}}\n```\n\n{}", "Tail. ".repeat(25));
        let cut = cut_hub(&body, 90, "x").expect("fits");
        assert!(cut.starts_with("Intro.\n\n```rust\nfn a() {}\n```\n\n*…"), "{cut}");
        assert_eq!(cut_hub(&body, 500, "x").as_deref(), Some(body.trim()));
        assert_eq!(cut_hub(&body, 10, "x"), None);
    }
}
