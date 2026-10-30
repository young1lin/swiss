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

//! The retrieval readout (SPEC §memory.search, §memory.brief): one line closing a search's
//! hits and a task brief that judges what came back - how many hits answer the query exactly
//! or hold nearly all of its words, how many share only a word or two with it, whether the
//! ones that carry it are filed in domains apart - and says what to do with it. An agent
//! handed five hits read the fifth, one shared word, as it read the first; the line says
//! which of them carry the query, deterministically, from the tiers and word shares the
//! ranking already measured.

use crate::search;

/// A word hit holding at least this share of the query's word weight holds the query.
pub const STRONG_SHARE: f64 = 0.8;
/// A word hit under this share holds a word or two of the query: the tail.
pub const TAIL_SHARE: f64 = 0.4;

/// How one hit holds the query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// An exact tier: the id, the title, an anchor, a title prefix, or an identifier the query
    /// names (search::EXACT_TIERS).
    Exact,
    /// A word tier, with the share of the query's word weight the note holds.
    Words(f64),
    /// Meaning alone (the semantic tier) or the substring pass: no word share to judge.
    Loose,
}

impl Hold {
    /// A search hit's hold: its tier, then the word share the word tiers measured.
    pub fn of(tier: &str, share: Option<f64>) -> Hold {
        if search::EXACT_TIERS.contains(&tier) {
            return Hold::Exact;
        }
        match share {
            Some(share) => Hold::Words(share),
            None => Hold::Loose,
        }
    }

    fn strong(self) -> bool {
        match self {
            Hold::Exact => true,
            Hold::Words(share) => share >= STRONG_SHARE,
            Hold::Loose => false,
        }
    }

    /// A word or two of the query ([TAIL_SHARE]), or meaning alone.
    pub fn tail(self) -> bool {
        match self {
            Hold::Exact => false,
            Hold::Words(share) => share < TAIL_SHARE,
            Hold::Loose => true,
        }
    }
}

/// What the hits say, as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The first hit holds the query and nothing that holds it is filed apart: answer.
    Strong,
    /// Something holds part of the query, or what holds it is split across domains: confirm.
    Mixed,
    /// Every hit shares a word or two at most: rephrase.
    Weak,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Strong => "strong",
            Verdict::Mixed => "mixed",
            Verdict::Weak => "weak",
        }
    }
}

/// The judgement of one retrieval.
#[derive(Debug, Clone, PartialEq)]
pub struct Readout {
    pub verdict: Verdict,
    /// Hits holding the query: an exact tier or [STRONG_SHARE] of its words.
    pub strong: usize,
    /// Hits holding between [TAIL_SHARE] and [STRONG_SHARE] of its words.
    pub partial: usize,
    /// Hits sharing a word or two (under [TAIL_SHARE]), or held by meaning or a substring alone.
    pub tail: usize,
    /// The domains the deciding hits (the strong ones, else the partial ones) are filed in,
    /// one per family (a domain and those under it are one); empty unless more than one.
    pub split: Vec<String>,
}

/// Judge the hits in rank order, each with its hold and the path of the domain it is filed
/// in; None for no hits (the caller already says nothing matched).
pub fn judge<'a>(hits: impl IntoIterator<Item = (Hold, Option<&'a str>)>) -> Option<Readout> {
    let hits: Vec<(Hold, Option<&str>)> = hits.into_iter().collect();
    let (top, _) = *hits.first()?;
    let strong = hits.iter().filter(|(hold, _)| hold.strong()).count();
    let tail = hits.iter().filter(|(hold, _)| hold.tail()).count();
    let partial = hits.len() - strong - tail;
    let deciding = hits
        .iter()
        .filter(|(hold, _)| if strong > 0 { hold.strong() } else { !hold.tail() && !hold.strong() })
        .filter_map(|(_, domain)| *domain);
    let split = families(deciding);
    let split = if split.len() > 1 { split } else { Vec::new() };
    let verdict = if top.strong() && split.is_empty() {
        Verdict::Strong
    } else if strong == 0 && partial == 0 {
        Verdict::Weak
    } else {
        Verdict::Mixed
    };
    Some(Readout { verdict, strong, partial, tail, split })
}

/// One path per family, the shallowest, in path order: `build` and `build/instances` are one.
fn families<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut paths: Vec<&str> = paths.collect();
    paths.sort_unstable();
    paths.dedup();
    let mut kept: Vec<String> = Vec::new();
    for path in paths {
        if !kept.iter().any(|k| path.starts_with(&format!("{k}/"))) {
            kept.push(path.to_string());
        }
    }
    kept
}

impl Readout {
    /// `Retrieval: strong (1 exact or full, 4 sharing a word or two) — answer from the first
    /// hit, checking it against the code it names.` and a newline.
    pub fn line(&self) -> String {
        let mut held = Vec::new();
        if self.strong > 0 {
            held.push(format!("{} exact or full", self.strong));
        }
        if self.partial > 0 {
            held.push(format!("{} partial", self.partial));
        }
        if self.tail > 0 {
            held.push(format!("{} sharing a word or two", self.tail));
        }
        if !self.split.is_empty() {
            let split: Vec<String> = self.split.iter().map(|d| format!("`{d}`")).collect();
            held.push(format!("split across {}", split.join(", ")));
        }
        let advice = match self.verdict {
            Verdict::Strong => "answer from the first hit, checking it against the code it names",
            Verdict::Mixed if !self.split.is_empty() => "say which domain you mean, or confirm the first hit before relying on it",
            Verdict::Mixed => "confirm the first hit against the code before relying on it",
            Verdict::Weak => "no hit holds much of the query: rephrase with an identifier, a path or the note's own words, or read the code",
        };
        format!("Retrieval: {} ({}) — {advice}.\n", self.verdict.as_str(), held.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(hits: &[(Hold, Option<&str>)]) -> Readout {
        judge(hits.iter().copied()).expect("hits")
    }

    #[test]
    fn a_hit_holds_by_its_tier_then_its_share() {
        assert_eq!(Hold::of("symbol", Some(0.1)), Hold::Exact, "an exact tier holds whatever its words");
        assert_eq!(Hold::of("anchor", None), Hold::Exact);
        assert_eq!(Hold::of("lexical", Some(0.5)), Hold::Words(0.5));
        assert_eq!(Hold::of("cjk", Some(0.9)), Hold::Words(0.9));
        assert_eq!(Hold::of("semantic", None), Hold::Loose);
        assert_eq!(Hold::of("substring", None), Hold::Loose);
        assert!(judge(std::iter::empty()).is_none(), "no hits: the caller says nothing matched");
    }

    #[test]
    fn the_readout_says_strong_mixed_or_weak() {
        // The first hit names what was asked; the rest share a word: answer.
        let strong = verdict(&[(Hold::Exact, Some("build")), (Hold::Words(0.3), Some("panel")), (Hold::Loose, None)]);
        assert_eq!((strong.verdict, strong.strong, strong.partial, strong.tail), (Verdict::Strong, 1, 0, 2));
        assert_eq!(
            strong.line(),
            "Retrieval: strong (1 exact or full, 2 sharing a word or two) — answer from the first hit, checking it against the code it names.\n"
        );
        // Every word held is as good as exact; a domain and one under it are one family.
        let full = verdict(&[(Hold::Words(0.9), Some("build")), (Hold::Words(0.85), Some("build/instances"))]);
        assert_eq!((full.verdict, full.split.len()), (Verdict::Strong, 0));

        // Half the words: confirm.
        let partial = verdict(&[(Hold::Words(0.5), Some("panel")), (Hold::Words(0.25), Some("host"))]);
        assert_eq!((partial.verdict, partial.partial, partial.tail), (Verdict::Mixed, 1, 1));
        assert!(partial.line().ends_with("— confirm the first hit against the code before relying on it.\n"), "{}", partial.line());
        // A strong hit below a partial one: confirm the first, it is not the strong one.
        let buried = verdict(&[(Hold::Words(0.6), None), (Hold::Words(0.95), None)]);
        assert_eq!(buried.verdict, Verdict::Mixed);

        // Strong hits filed in domains apart: say which.
        let split = verdict(&[(Hold::Exact, Some("panel")), (Hold::Exact, Some("build/instances")), (Hold::Words(0.2), Some("host"))]);
        assert_eq!((split.verdict, split.split.clone()), (Verdict::Mixed, vec!["build/instances".to_string(), "panel".to_string()]));
        assert_eq!(
            split.line(),
            "Retrieval: mixed (2 exact or full, 1 sharing a word or two, split across `build/instances`, `panel`) — say which domain you \
             mean, or confirm the first hit before relying on it.\n"
        );
        // Only the deciding hits split: a tail hit elsewhere does not, nor does an unfiled one.
        let one = verdict(&[(Hold::Exact, Some("panel")), (Hold::Words(0.1), Some("host")), (Hold::Exact, None)]);
        assert_eq!((one.verdict, one.split.len()), (Verdict::Strong, 0));

        // A word or two each, or meaning alone: rephrase.
        let weak = verdict(&[(Hold::Words(0.3), Some("panel")), (Hold::Words(0.2), Some("host")), (Hold::Loose, None)]);
        assert_eq!((weak.verdict, weak.tail), (Verdict::Weak, 3));
        assert_eq!(
            weak.line(),
            "Retrieval: weak (3 sharing a word or two) — no hit holds much of the query: rephrase with an identifier, a path or the \
             note's own words, or read the code.\n"
        );
    }
}
