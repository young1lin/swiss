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

//! What a note is (SPEC §memory.capture, ADR-032): the one text every writer that
//! forms notes reads - the sync's runner, a bootstrap (`swiss memory rules`), the
//! save policy - and the advisory lint a save answers with.
//!
//! The rules exist because of a measured failure: a bootstrap told to write 6 to 12
//! notes a domain, 25 to 80 lines each in five fixed sections with a call chain,
//! produced 1,995 notes averaging 3,220 characters and 12.5 claims, and only half of
//! a checked sample was wholly correct - the retold code and the global claims were
//! where the errors sat. The lint flags those shapes; it never refuses a save.
//!
//! The rerun to these rules (phase 6) left a quarter of its notes not wholly correct,
//! and most of their errors were claims over a set: a count, "the only", "every
//! other", "most". [set_claims] finds those; the code check (§memory.code) re-counts
//! the ones that name their set and holds an agent's note that neither names it nor
//! says what it read.

use serde::Serialize;

use crate::code::check;
use crate::model::Kind;
use crate::search::{is_cjk, is_code};

/// A note's usual size, in characters.
pub const TYPICAL_CHARS: (usize, usize) = (300, 900);
/// Over this many characters a note is linted `long`.
pub const LONG_CHARS: usize = 1200;
/// A fenced block over this many lines is linted `code`.
const CODE_LINES_MAX: usize = 8;
/// This many hop lines (or this many arrows on one line) make a call chain.
const CHAIN_LINES: usize = 3;
/// A sentence holding one of these says what it covers: its claim over a set is scoped.
const SCOPED: [&str; 6] = ["in the inspected scope", "observed", "appear", "as far as", "stated by the user", "checked"];
/// The words before a number that make a count: `exactly two`, `only 3`, `all 7`.
const COUNT_LEADS: [&str; 4] = ["exactly", "only", "just", "all"];
const NUMBER_WORDS: [&str; 12] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
/// The phrases that close a set: nothing beyond what the note names.
const CLOSED: [&str; 8] =
    ["the only", "the sole", "no other", "nothing else", "none of the other", "nowhere else", "anywhere else", "no one else"];
/// The phrases that claim every member but the one named.
const EVERY_OTHER: [&str; 2] = ["every other", "all other"];
/// `most` before these is not a majority (`most likely`), nor after `at`, `the` or `almost`.
const NOT_MAJORITY: [&str; 5] = ["likely", "recent", "recently", "important", "of all"];
/// A note yields at most this many claims over a set.
const SET_CLAIMS_MAX: usize = 20;
/// A restated line's normalized form (letters and digits, lower-cased) is at least this
/// long: a shorter match is usually a bare name the note discusses, not the code back.
const RESTATE_MIN_CHARS: usize = 8;
/// At least this many restated lines, or the note quotes the code rather than retells it.
const RESTATE_LINES: usize = 2;
/// This share of the note's non-trivial characters coming verbatim from its anchored
/// files is a restatement: a quote or two inside real prose stays under it.
const RESTATE_SHARE: f64 = 0.5;

/// The note rules: Markdown bullets under a `###` heading, verbatim in the sync's prompt
/// and the bootstrap's, and printed by `swiss memory rules`.
pub const NOTE_RULES: &str = "\
### What a note is
Code is ground truth. Memory is not code documentation: an agent reads the code; a note holds what the code \
cannot tell it.
- **One central claim.** The title states it in the present tense; the content gives at most three facts it rests \
on, why it holds - and why not the obvious alternative - and where the evidence is. Typically 300 to 900 \
characters of plain sentences: no headings, no template, no call chain, no example walk-through. Every claim is \
one more thing to get wrong: leave out a fact the claim does not need.
- **Quote what a fact rests on.** Give each fact's file as evidence with an excerpt: the line or two you read, \
copied letter for letter. The save compares the excerpt with the file and holds a note whose quote is not there.
- **Worth a note:** a decision and what it traded away, a constraint, a pitfall and its fix, the rationale behind \
code that looks wrong, behaviour the code does not make obvious, a business invariant, a compatibility \
requirement, a procedure people must follow.
- **Not a note:** anything a reader derives mechanically from the code - signatures, call chains, file lists, \
what a function does step by step, defaults the code states - nor the history of a change, task progress or \
speculation. A claim with no why is a fact the code already holds.
- **A claim over a set is re-counted.** A count (exactly two, all 7), the only, no other, every other or most \
holds only if you name the set in backticks - a glob or a directory (`scripts/gen-*.ts`) - and what each member \
holds (`main`): the save counts it again in the code index and holds the note when the count differs. Otherwise \
say what you read: \"in `path`\", \"observed in `path`\", \"in the inspected scope\". Always and never are about \
one place's behaviour: write them only of the code you read.
- **Be exact where checks find notes wrong most often:** name the file a quote or symbol is really in (a code \
comment is not the README), keep directions as the code has them (inner/outer, before/after, `and`/`or`), copy \
names letter for letter, and cite a test only when you saw it assert the claim.
- **A rule the user gave is in their words.** A preference, or a decision the user made, opens its content with \
what they said, quoted letter for letter and dated (`> \"Keep answers short.\" - stated by the user, 2026-10-21`), \
then why it holds. A paraphrase drifts from what was asked, and the date tells a reader which of two asks is newer.
- **Anchors** are the files the claim is about; **refs** name the commit, URL or `path: heading` it comes from; \
**keywords** are up to 8 terms a reader would search for that the title does not hold - the business term, a \
synonym, the main name - each in English and in the team's language (`refund`, `退款`), so a search in either \
language finds the note.
- **Zero notes is a valid answer.** A few true notes beat many: write one only when you would bet on it.
";

/// The notes half of a bootstrap prompt: no quota, the note rules, the answer shape.
pub fn bootstrap_rules() -> String {
    format!(
        "## Notes\n\
         Besides the hub, write the notes this domain earns. There is no quota: most domains earn a few, some none, \
         and every note is a claim a check against the code will hold you to. The hub is the map; a note says what \
         the map cannot - never repeat the hub.\n\n\
         {NOTE_RULES}\n\
         Each note: `key` (`n1`, `n2`, ... your handle for links), `kind` (`pitfall`, `constraint`, `decision` or \
         `knowledge`), `title` (the claim, present tense, at most 150 characters), `content` (its facts, why and why \
         not, in plain sentences), `anchors` (the 1 to 4 repository files the claim is about), `evidence` (1 to 3 \
         `{{\"path\", \"symbol\", \"excerpt\"}}`: a file a fact rests on, the symbol, and the line or two you read \
         there, copied letter for letter) and `links` (`{{\"to\", \"kind\", \"why\"}}` to another of your notes or a \
         listed note id, only where the connection is real: `supports`, `depends_on`, `related_to` or \
         `contradicts`).\n"
    )
}

/// One advisory finding about a note's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lint {
    /// `sections` | `long` | `call_chain` | `code` | `quantifier` | `restates_code` | `user_words`.
    pub rule: &'static str,
    pub message: String,
}

/// The findings on one note, in a fixed rule order; empty when it reads as an atomic claim.
pub fn lint(kind: Kind, title: &str, content: &str) -> Vec<Lint> {
    let mut headings = 0;
    let mut hop_lines = 0;
    let mut widest_chain = 0;
    let mut longest_block = 0;
    let mut prose = String::new();
    let mut fence: Option<usize> = None;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = match fence {
                Some(lines) => {
                    longest_block = longest_block.max(lines);
                    None
                }
                None => Some(0),
            };
            continue;
        }
        let arrows = line.matches("->").count() + line.matches('\u{2192}').count();
        if line.matches(" -- ").count() >= 2 || arrows >= 2 {
            hop_lines += 1;
        }
        widest_chain = widest_chain.max(arrows);
        match fence.as_mut() {
            Some(lines) => *lines += 1,
            None => {
                let hashes = trimmed.chars().take_while(|c| *c == '#').count();
                if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                    headings += 1;
                }
                prose.push_str(line);
                prose.push('\n');
            }
        }
    }
    if let Some(lines) = fence {
        longest_block = longest_block.max(lines);
    }

    let mut out = Vec::new();
    if headings >= 2 {
        out.push(Lint {
            rule: "sections",
            message: format!(
                "{headings} headings: a note with sections is a document - keep the one claim and split the rest into \
                 notes of their own"
            ),
        });
    }
    let chars = content.chars().count();
    if chars > LONG_CHARS {
        out.push(Lint {
            rule: "long",
            message: format!(
                "{chars} characters: a note is typically {} to {} - keep the claim, its why and its evidence, and drop \
                 what the code already says",
                TYPICAL_CHARS.0, TYPICAL_CHARS.1
            ),
        });
    }
    if hop_lines >= CHAIN_LINES || widest_chain >= CHAIN_LINES {
        out.push(Lint {
            rule: "call_chain",
            message: "a call chain retells the code - name the symbol where the behaviour lives and say why it is so"
                .into(),
        });
    }
    if longest_block > CODE_LINES_MAX {
        out.push(Lint {
            rule: "code",
            message: format!(
                "a {longest_block}-line code block - quote a line or two at most; the code itself is the ground truth"
            ),
        });
    }
    if kind != Kind::Preference {
        let mut phrases: Vec<String> = Vec::new();
        for claim in set_claims(title, &prose).into_iter().filter(SetClaim::unbacked) {
            if !phrases.contains(&claim.phrase) {
                phrases.push(claim.phrase);
            }
        }
        if !phrases.is_empty() {
            out.push(Lint { rule: "quantifier", message: unbacked_message(&phrases.join(", ")) });
        }
    }
    out
}

/// Lower-cased letters and digits only: punctuation, spacing and case are the note's to
/// choose, the words are not.
fn restated_norm(line: &str) -> String {
    line.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// The Markdown a note wraps code in - bullets, numbers, quotes, backticks - off a line.
fn restated_line(line: &str) -> &str {
    fn numbered(s: &str) -> Option<&str> {
        let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
        let rest = s.get(digits..)?;
        let rest = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))?;
        (digits > 0).then_some(rest)
    }
    let mut s = line.trim();
    loop {
        let cut = s
            .strip_prefix("- ")
            .or_else(|| s.strip_prefix("* "))
            .or_else(|| s.strip_prefix("+ "))
            .or_else(|| s.strip_prefix("> "))
            .or_else(|| numbered(s));
        match cut {
            Some(rest) => s = rest.trim_start(),
            None => break,
        }
    }
    s.trim_matches('`')
}

/// Whether the note retells its anchored files - a formation hint, never a refusal: the
/// save reads the anchored text for the code check anyway, and a note whose lines are
/// the code's own words back is what the rules call not-a-note (a signature, a file
/// list, a step-by-step). `anchored` is the files' text as [crate::code::check]
/// read it; empty (no checkout, no anchors) never rests anything.
pub fn restates_code(content: &str, anchored: &str) -> Option<Lint> {
    if anchored.is_empty() {
        return None;
    }
    let hay = restated_norm(anchored);
    let mut total = 0usize;
    let mut matched = 0usize;
    let mut lines = 0usize;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            continue; // the fence itself; the lines inside a pasted block count like any other
        }
        let needle = restated_norm(restated_line(line));
        let len = needle.chars().count();
        if len < 4 {
            continue; // blank, decorative or a bare short word: nothing to restate
        }
        total += len;
        if len >= RESTATE_MIN_CHARS && hay.contains(needle.as_str()) {
            matched += len;
            lines += 1;
        }
    }
    (lines >= RESTATE_LINES && matched as f64 / total.max(1) as f64 >= RESTATE_SHARE).then(|| Lint {
        rule: "restates_code",
        message: format!(
            "most of the note appears verbatim in its anchored files ({matched} of {total} characters) - the code \
             already says this; keep what it cannot: why it is so, why not the obvious alternative, what bites"
        ),
    })
}

/// Whether a preference lacks the user's words - a formation hint, never a refusal: a
/// preference is the user's rule, and the note holds it best as what they said, quoted
/// and dated, with the why after it. The save asks it only of a note a person did not
/// type themselves (an agent's, SPEC §memory.capture); `swiss memory lint` of any draft.
pub fn user_words(kind: Kind, content: &str) -> Option<Lint> {
    (kind == Kind::Preference && !(quotes_someone(content) && dated(content))).then(|| Lint {
        rule: "user_words",
        message: "a preference is the user's rule: open its content with their words, quoted letter for letter and \
                  dated (> \"Keep answers short.\" - stated by the user, 2026-10-21), then why it holds - a paraphrase \
                  drifts from what they asked, and the date tells a reader which of two asks is newer"
            .into(),
    })
}

/// A quote: a Markdown quote line, or words in quotation marks - two words at least, or
/// four CJK characters, so a quoted name (`"main"`) is no quote.
fn quotes_someone(content: &str) -> bool {
    let words = |span: &str| span.split_whitespace().count() >= 2 || span.chars().filter(|c| is_cjk(*c)).count() >= 4;
    if content.lines().any(|line| line.trim_start().strip_prefix('>').is_some_and(|rest| words(rest.trim()))) {
        return true;
    }
    [('"', '"'), ('\u{201c}', '\u{201d}'), ('\u{300c}', '\u{300d}'), ('\u{300e}', '\u{300f}')].iter().any(|(open, close)| {
        let mut rest = content;
        while let Some(start) = rest.find(*open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(*close) else { break };
            if words(&after[..end]) {
                return true;
            }
            rest = &after[end + close.len_utf8()..];
        }
        false
    })
}

/// Whether the text holds a date: `2026-10-21`, or `2026年10月`.
fn dated(content: &str) -> bool {
    let chars: Vec<char> = content.chars().collect();
    let digits = |from: usize, n: usize| from + n <= chars.len() && chars[from..from + n].iter().all(char::is_ascii_digit);
    (0..chars.len()).any(|i| {
        let boundary = i == 0 || !chars[i - 1].is_ascii_digit();
        boundary
            && digits(i, 4)
            && ((chars.get(i + 4) == Some(&'-') && digits(i + 5, 2) && chars.get(i + 7) == Some(&'-') && digits(i + 8, 2))
                || (chars.get(i + 4) == Some(&'年') && digits(i + 5, 1)))
    })
}

/// What to do about a claim over a set that nothing re-counts.
pub fn unbacked_message(phrases: &str) -> String {
    format!(
        "a claim over a set that nothing re-counts ({phrases}) - name the set in backticks, a glob or a directory \
         (`scripts/gen-*.ts`), and what each member holds (`main`), or say what you read (\"in `path`\", \"observed \
         in `path`\", \"in the inspected scope\")"
    )
}

/// One sentence's claim over a set (ADR-032 phase 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClaim {
    /// The words that make it one, lower-cased: `exactly two`, `the only`, `most`.
    pub phrase: String,
    pub quantity: Quantity,
    /// The sentence's backticked globs and directories: the set a re-count covers.
    pub sets: Vec<String>,
    /// The sentence's backticked names: what a member of the set holds.
    pub names: Vec<String>,
    /// The sentence names a file it is about.
    pub in_file: bool,
    /// The sentence says it covers only what was read ([SCOPED]).
    pub hedged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    /// `exactly two`, `only 3`: so many members hold the names.
    Count(u32),
    /// `all 7`: the set has so many members, each holding the names; `every other`: each
    /// member but the files the note names holds them.
    All(Option<u32>),
    /// `the only`, `no other`, `nothing else`: no file beyond those the note names holds them.
    Closed,
    /// `most`: more than half the members hold them.
    Most,
}

impl SetClaim {
    /// Nothing can re-count it and nothing scopes it: a hint in the lint, a hold in the
    /// code check. A closed set with no set named is counted by a code-like name (`retry_budget`,
    /// `fitRailLabels`), the names whose every mention the code index keeps.
    pub fn unbacked(&self) -> bool {
        let countable = self.quantity == Quantity::Closed && self.names.iter().any(|n| is_code(n));
        !self.hedged && !self.in_file && self.sets.is_empty() && !countable
    }
}

/// The claims over a set in a note's title and prose, in order; a code span is a name,
/// never a claim.
pub fn set_claims(title: &str, content: &str) -> Vec<SetClaim> {
    let mut out = Vec::new();
    // A fenced block is code, not a claim.
    let mut fenced = false;
    let prose: String = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
                return false;
            }
            !fenced
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (prose, spans) in sentences(title).into_iter().chain(sentences(&prose)) {
        let lower = prose.to_lowercase();
        let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let found = quantities(&words);
        if found.is_empty() {
            continue;
        }
        let mut sets = Vec::new();
        let mut names = Vec::new();
        let mut in_file = prose.split_whitespace().any(|token| check::path_of(token).is_some());
        for span in &spans {
            let span = span.trim().trim_start_matches("./");
            if span.is_empty() || span.contains(char::is_whitespace) {
                continue;
            }
            if span.contains('*') || (span.ends_with('/') && span.len() > 1) {
                sets.push(span.to_string());
            } else if check::path_of(span).is_some() {
                in_file = true;
            } else {
                for claim in check::names_of(span) {
                    if !names.contains(&claim) {
                        names.push(claim);
                    }
                }
            }
        }
        let hedged = SCOPED.iter().any(|s| lower.contains(s));
        for (phrase, quantity) in found {
            if out.len() < SET_CLAIMS_MAX {
                out.push(SetClaim { phrase, quantity, sets: sets.clone(), names: names.clone(), in_file, hedged });
            }
        }
    }
    out
}

/// The sentences of a text, each as its prose (code spans blanked) and its code spans. A
/// period ends a sentence only before a space or the end, so `a.b` and `src/x.rs` stay whole.
fn sentences(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let (mut prose, mut spans, mut span) = (String::new(), Vec::new(), None::<String>);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            match span.take() {
                Some(done) => spans.push(done),
                None => span = Some(String::new()),
            }
            prose.push(' ');
            continue;
        }
        if let Some(open) = span.as_mut() {
            if c == '\n' {
                // An unclosed span ends at the line.
                prose.push_str(open);
                span = None;
            } else {
                open.push(c);
                continue;
            }
        }
        let ends = match c {
            '\n' | '!' | '?' | ';' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}' | '\u{ff1b}' => true,
            '.' => chars.peek().is_none_or(|n| n.is_whitespace()),
            _ => false,
        };
        if ends {
            if !prose.trim().is_empty() {
                out.push((std::mem::take(&mut prose), std::mem::take(&mut spans)));
            }
            prose.clear();
            spans.clear();
        } else {
            prose.push(c);
        }
    }
    if let Some(open) = span {
        prose.push_str(&open);
    }
    if !prose.trim().is_empty() {
        out.push((prose, spans));
    }
    out
}

/// The quantities one sentence's words state, each with its phrase.
fn quantities(words: &[&str]) -> Vec<(String, Quantity)> {
    let mut out: Vec<(String, Quantity)> = Vec::new();
    let mut push = |phrase: String, quantity: Quantity| {
        if !out.iter().any(|(p, _)| *p == phrase) {
            out.push((phrase, quantity));
        }
    };
    let starts = |at: usize, phrase: &str| {
        let parts: Vec<&str> = phrase.split(' ').collect();
        words.len() >= at + parts.len() && words[at..at + parts.len()] == parts[..]
    };
    for (at, word) in words.iter().enumerate() {
        // `the only one` closes a set; it does not count one.
        if COUNT_LEADS.contains(word) && !(*word == "only" && at > 0 && words[at - 1] == "the") {
            if let Some(n) = words.get(at + 1).and_then(|w| number(w)) {
                let phrase = format!("{word} {}", words[at + 1]);
                push(phrase, if *word == "all" { Quantity::All(Some(n)) } else { Quantity::Count(n) });
                continue;
            }
        }
        if let Some(phrase) = CLOSED.iter().find(|p| starts(at, p)) {
            push(phrase.to_string(), Quantity::Closed);
        } else if let Some(phrase) = EVERY_OTHER.iter().find(|p| starts(at, p)) {
            push(phrase.to_string(), Quantity::All(None));
        } else if *word == "most" {
            let before = at.checked_sub(1).map(|b| words[b]);
            let after = words.get(at + 1).copied();
            let majority = !matches!(before, Some("at" | "the" | "almost"))
                && after.is_some_and(|a| !NOT_MAJORITY.iter().any(|n| starts(at + 1, n) || *n == a));
            if majority {
                push("most".to_string(), Quantity::Most);
            }
        }
    }
    out
}

/// A count as written: digits up to 999, or one to twelve in words.
fn number(word: &str) -> Option<u32> {
    if let Some(at) = NUMBER_WORDS.iter().position(|w| *w == word) {
        return Some(at as u32 + 1);
    }
    word.parse::<u32>().ok().filter(|n| (1..1000).contains(n) && word.len() <= 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(found: &[Lint]) -> Vec<&'static str> {
        found.iter().map(|l| l.rule).collect()
    }

    /// The shape the 2026-10-07 bootstrap wrote, with synthetic content.
    fn templated() -> String {
        let mut text = String::from(
            "### What happens\nWhen `Queue.push` is called with a full buffer the call always blocks and every \
             waiter is woken in order. The buffer holds exactly 64 slots.\n\n### Why\nIt protects the consumer.\n\n\
             ### Code path\n```text\nQueue.push() -- src/queue.ts -- checks the slot count\nBuffer.wait() -- \
             src/buffer.ts -- parks the caller\nScheduler.wake() -- src/sched.ts -- wakes the waiters\n```\n\n\
             ### Example\n```ts\n",
        );
        for i in 0..14 {
            text.push_str(&format!("const step{i} = queue.push(item{i})\n"));
        }
        text.push_str("```\n\n### When you change this\nKeep the order. No test pins this.\n");
        while text.chars().count() < 2600 {
            text.push_str("The waiter list is drained front to back after each pop.\n");
        }
        text
    }

    const ATOMIC: &str = "The queue blocks a producer instead of dropping its item when the buffer is full. \
        Why: the consumer replays events by sequence number, and a dropped item would leave a gap it cannot \
        detect (observed in `src/replay.ts`, which trusts the numbering). Why not a bounded drop with a counter: \
        the replay has no way to ask for the missing item again. Evidence: `Queue.push` in `src/queue.ts`.";

    #[test]
    fn restating_the_anchored_files_is_a_hint_and_quoting_is_not() {
        let file = "pub fn retry_key(order: u64) -> String {\n    format!(\"retry-{order}\")\n}\n";
        let retold = "The file:\n- `pub fn retry_key(order: u64) -> String {`\n- `format!(\"retry-{order}\")`\n";
        let lint = restates_code(retold, file).expect("the note is the file back");
        assert_eq!(lint.rule, "restates_code");
        let prose = "The key is derived from the order id so a retried charge is not charged twice.";
        assert!(restates_code(prose, file).is_none(), "prose about the file is not the file");
        assert!(restates_code(retold, "").is_none(), "nothing anchored, nothing restated");
    }

    #[test]
    fn the_rules_carry_the_principles_and_no_quota_or_template() {
        for wanted in [
            "Code is ground truth",
            "Memory is not code documentation",
            "why not",
            "300 to 900 characters",
            "Zero notes is a valid answer",
            "in the inspected scope",
            "at most three facts",
            "copied letter for letter",
            "`scripts/gen-*.ts`",
            "holds the note when the count differs",
            "A rule the user gave is in their words",
        ] {
            assert!(NOTE_RULES.contains(wanted), "the rules lack {wanted:?}");
        }
        let bootstrap = bootstrap_rules();
        assert!(bootstrap.contains(NOTE_RULES), "the bootstrap reads the same rules");
        assert!(bootstrap.contains("`evidence` (1 to 3") && bootstrap.contains("\"excerpt\""), "{bootstrap}");
        for text in [NOTE_RULES, bootstrap.as_str()] {
            for gone in ["6 to 12", "25 to 80", "What happens", "Code path", "When you change this", "hops", "at least one link"] {
                assert!(!text.contains(gone), "the rules still say {gone:?}");
            }
        }
    }

    #[test]
    fn an_old_template_note_is_linted_on_every_count() {
        let found = lint(Kind::Knowledge, "Queue.push blocks on a full buffer", &templated());
        assert_eq!(rules(&found), ["sections", "long", "call_chain", "code", "quantifier"], "{found:#?}");
        let quantifier = &found[4].message;
        assert!(quantifier.contains("(exactly 64)"), "a count is a claim over a set; always and every alone are not: {quantifier}");
    }

    #[test]
    fn an_atomic_note_is_clean() {
        assert!((300..=900).contains(&ATOMIC.chars().count()), "the sample is typical: {}", ATOMIC.chars().count());
        assert_eq!(lint(Kind::Decision, "A full queue blocks the producer", ATOMIC), []);
    }

    fn claims(text: &str) -> Vec<(String, Quantity)> {
        set_claims("", text).into_iter().map(|c| (c.phrase, c.quantity)).collect()
    }

    #[test]
    fn a_claim_over_a_set_is_a_count_a_closed_set_or_a_majority() {
        // The shapes the phase-6 sample found wrong (synthetic text).
        assert_eq!(claims("Exactly two scripts call main()."), [("exactly two".into(), Quantity::Count(2))]);
        assert_eq!(claims("It keeps only 3 workers."), [("only 3".into(), Quantity::Count(3))]);
        assert_eq!(claims("All 7 generators verify it."), [("all 7".into(), Quantity::All(Some(7)))]);
        assert_eq!(claims("Every other generator skips the cache."), [("every other".into(), Quantity::All(None))]);
        assert_eq!(claims("`flush` is the only caller of `drain`."), [("the only".into(), Quantity::Closed)]);
        assert_eq!(claims("It is the only one that drains."), [("the only".into(), Quantity::Closed)]);
        assert_eq!(claims("No other module writes the file; nothing else reads it."), [
            ("no other".into(), Quantity::Closed),
            ("nothing else".into(), Quantity::Closed)
        ]);
        assert_eq!(claims("Most chunks are shorter than the cap."), [("most".into(), Quantity::Most)]);
        // What phase 6 flagged and the sample found right: a word about one place's behaviour.
        for plain in [
            "The loop always retries.",
            "Every request retries twice before the gateway gives up.",
            "It never blocks, and it runs only when the queue is full.",
            "A leaf takes at most 4 notes; the most recent run wins; it most likely fails.",
            "The `only_once` flag and `exactly_two()` gate the retry.",
            "Retries reuse each other's key.",
        ] {
            assert_eq!(claims(plain), [], "{plain}");
        }
    }

    #[test]
    fn a_claim_over_a_set_names_its_set_or_says_what_it_read() {
        let one = |text: &str| set_claims("", text).into_iter().next().expect("a claim");
        let named = one("Exactly two `scripts/gen-*.ts` scripts call `main()`.");
        assert_eq!((named.sets.as_slice(), named.names.as_slice()), (&["scripts/gen-*.ts".to_string()][..], &["main".to_string()][..]));
        assert!(!named.unbacked());
        let dir = one("Most files under `src/plugins/` import `Store::write`.");
        assert_eq!((dir.sets, dir.names), (vec!["src/plugins/".to_string()], vec!["Store".to_string(), "write".to_string()]));
        // A closed set is re-counted by a code-like name alone, over every file that holds it; a
        // plain one (`probe`) the index keeps no mention of.
        assert!(!one("No other file calls `drain_queue`.").unbacked());
        assert!(one("No other file calls `probe`.").unbacked());
        // A file names the place a count is about; a hedge says it covers what was read.
        assert!(!one("Exactly two calls sit in src/store.rs.").unbacked());
        assert!(!one("In `crates/x/lib.rs` exactly two calls retry.").unbacked());
        assert!(!one("Most chunks observed in the replay are short.").unbacked());
        assert!(!one("In the inspected scope no other module writes it.").unbacked());
        // Nothing to re-count and nothing scoped.
        for bare in [
            "Exactly two scripts call main().",
            "Most chunks are short.",
            "No other module writes it.",
            "Every other `generator` skips it.",
        ] {
            assert!(one(bare).unbacked(), "{bare}");
        }
        // The title is a sentence too; a period inside a path or a name ends nothing.
        let titled = set_claims("Exactly one store.rs writer", "The `a.b` call and src/x.rs stay whole. Fine.");
        assert_eq!(titled.len(), 1);
        assert!(!titled[0].in_file && titled[0].phrase == "exactly one", "{titled:?}");
    }

    #[test]
    fn an_unbacked_claim_over_a_set_is_a_hint_and_a_preference_is_not_linted() {
        let flagged = lint(Kind::Pitfall, "Retries", "Exactly two clients retry before the gateway gives up.");
        assert_eq!(rules(&flagged), ["quantifier"]);
        assert!(flagged[0].message.contains("(exactly two)"), "{}", flagged[0].message);
        assert_eq!(rules(&lint(Kind::Pitfall, "The only retrying client", "It retries twice.")), ["quantifier"], "the title counts");
        for fine in [
            "Exactly two clients observed in `src/client.ts` retry.",
            "Exactly two `src/clients/*.ts` clients call `retry`.",
            "Every request retries twice.",
        ] {
            assert_eq!(lint(Kind::Pitfall, "Retries", fine), [], "{fine}");
        }
        assert_eq!(lint(Kind::Preference, "No mocks", "No other test mocks the database: stated by the user."), []);
        assert_eq!(lint(Kind::Preference, "One style", "Exactly one style is allowed."), []);
    }

    #[test]
    fn a_preference_without_the_user_s_dated_words_is_a_hint() {
        let hint = |content: &str| user_words(Kind::Preference, content).map(|l| l.rule);
        assert_eq!(hint("Answers stay short: the user reads them on a phone."), Some("user_words"));
        assert_eq!(hint("> \"Keep answers short.\" - stated by the user, 2026-10-21\n\nThey read them on a phone."), None);
        assert_eq!(hint("On 2026-10-21 the user said \u{201c}keep answers short\u{201d}: they read on a phone."), None);
        assert_eq!(hint("用户 2026年10月21日 说：「回答要简短一些」，因为在手机上看。"), None);
        assert_eq!(hint("The user said \"keep answers short\"."), Some("user_words"), "undated");
        assert_eq!(hint("Name the branch \"main\", as decided on 2026-10-21."), Some("user_words"), "a quoted name is no quote");
        assert_eq!(hint("Released 12026-10-210 with \"two words\"."), Some("user_words"), "no date inside a longer number");
        assert_eq!(user_words(Kind::Decision, "Answers stay short."), None, "a preference only");
    }

    #[test]
    fn one_arrow_or_one_heading_is_not_a_chain_or_a_document() {
        let text = "## Note\nA save goes `save` -> `insert_memory` in one transaction, because the FTS trigger \
                    must see the row.";
        assert_eq!(lint(Kind::Knowledge, "Saves are one transaction", text), []);
        let chain = "Calls run:\n- `a()` -> `b()` -> `c()` -> `d()`\n";
        assert_eq!(rules(&lint(Kind::Knowledge, "Chain", chain)), ["call_chain"]);
    }
}
