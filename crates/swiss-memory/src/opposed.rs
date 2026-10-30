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

//! The save-time contradiction hint (SPEC §memory.capture): a fresh note whose title
//! asserts the opposite of a live note on the same file - `Never retry the refund
//! webhook` saved beside `Always retry the refund webhook` - is answered with that note,
//! so the writer supersedes the stale one or links the two before a brief hands an agent
//! both. Deterministic and offline: a title's claim is its words with the polarity words
//! taken out, and two titles are opposed when their claims mostly coincide and exactly
//! one of them is negated. It is a hint, never a refusal; a contradiction worded apart
//! goes unnoticed, which costs nothing the save did not already cost.

use serde::Serialize;

use crate::related::STOP_WORDS;
use crate::search::{self, UNITS_MAX};

/// Words that negate the claim they stand in.
const NEGATORS: [&str; 10] = ["never", "not", "no", "none", "nothing", "cannot", "avoid", "dont", "doesnt", "without"];
/// Words that only state a claim's force: taken out of the claim, negating nothing.
const AFFIRMERS: [&str; 12] =
    ["always", "must", "do", "does", "should", "shall", "need", "needs", "required", "requires", "ever", "only"];
/// CJK negations, longest first so `不要` is one negation, not `不` and an affirmer.
const CJK_NEGATORS: [&str; 18] = [
    "永远不", "不要", "不能", "不得", "不可", "不应", "不该", "不必", "不用", "禁止", "严禁", "切勿", "从不", "绝不", "无需", "别", "勿", "不",
];
/// Words holding `不` that negate nothing: `不同` is "different", `不断` "continually".
const CJK_NOT_NEGATIONS: [&str; 8] = ["不同", "不过", "不断", "不仅", "不少", "不错", "不久", "不管"];
const CJK_AFFIRMERS: [&str; 9] = ["必须", "务必", "总是", "始终", "一定", "永远", "需要", "应该", "要"];
/// Two claims are one when they share at least this share of their words (Jaccard)...
const SAME_CLAIM_SHARE: f64 = 0.5;
/// ...and at least this many words: one shared word is a topic, not a claim.
const SAME_CLAIM_WORDS: usize = 2;

/// A live note a fresh save's title contradicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opposed {
    pub id: String,
    pub title: String,
    /// An anchor the two notes share.
    pub anchor: String,
    /// What to do about it.
    pub message: String,
}

impl Opposed {
    pub fn new(id: &str, title: &str, anchor: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            anchor: anchor.to_string(),
            message: format!(
                "contradicts? {id} is on {anchor} too and its title asserts the opposite: if it no longer holds, mark it \
                 outdated (a save with replaces supersedes it in one step); if both hold where each applies, link the two \
                 with contradicts"
            ),
        }
    }
}

/// A title read as a claim: whether it is negated, and its words without the polarity.
#[derive(Debug, PartialEq)]
struct Claim {
    negated: bool,
    words: Vec<String>,
}

/// Whether two titles assert opposite things: one negated, the other not, about mostly
/// the same words.
pub fn opposed(a: &str, b: &str) -> bool {
    let (a, b) = (claim(a), claim(b));
    if a.negated == b.negated {
        return false;
    }
    let shared = a.words.iter().filter(|w| b.words.contains(w)).count();
    let union = a.words.len() + b.words.len() - shared;
    shared >= SAME_CLAIM_WORDS && union > 0 && shared as f64 / union as f64 >= SAME_CLAIM_SHARE
}

/// Whether `new` only refines `old`: the same polarity and every word of the old claim
/// still in the new one - `Use WAL` superseded by `Use WAL mode for every store` was
/// reworded, not rejected (the brief's rejected precedent, SPEC §memory.brief).
pub fn refines(old: &str, new: &str) -> bool {
    let (old, new) = (claim(old), claim(new));
    old.negated == new.negated && old.words.iter().all(|w| new.words.contains(w))
}

fn claim(title: &str) -> Claim {
    let text = title.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    let mut negations = 0;
    let mut words: Vec<String> = Vec::new();
    fn push(word: String, words: &mut Vec<String>) {
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }

    // The CJK half: negations counted and cut, affirmers cut, the rest as bigrams.
    let mut cjk = text.clone();
    for (i, keep) in CJK_NOT_NEGATIONS.iter().enumerate() {
        cjk = cjk.replace(keep, &format!("\u{0}{i}\u{0}"));
    }
    for negator in CJK_NEGATORS {
        negations += cjk.matches(negator).count();
        cjk = cjk.replace(negator, " ");
    }
    for affirmer in CJK_AFFIRMERS {
        cjk = cjk.replace(affirmer, " ");
    }
    for (i, keep) in CJK_NOT_NEGATIONS.iter().enumerate() {
        cjk = cjk.replace(&format!("\u{0}{i}\u{0}"), keep);
    }
    for unit in search::cjk_units(&cjk, UNITS_MAX) {
        push(unit, &mut words);
    }

    // The Latin half: `don't` and `isn't` stay one word, so their negation is seen.
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')) {
        let word = raw.trim_matches('\'');
        if word.is_empty() || word.chars().any(search::is_cjk) {
            continue;
        }
        if NEGATORS.contains(&word) || word.ends_with("n't") {
            negations += 1;
            continue;
        }
        // A lone letter is no word; a lone digit is: `3 times` and `5 times` differ.
        if AFFIRMERS.contains(&word) || STOP_WORDS.contains(&word) || (word.len() == 1 && word.chars().all(|c| c.is_ascii_alphabetic())) {
            continue;
        }
        push(stem(word), &mut words);
    }
    Claim { negated: negations % 2 == 1, words }
}

/// A crude English stem, enough that `retry`, `retries`, `retried` and `retrying` - or
/// `cache`, `caches` and `cached` - read as one word.
fn stem(word: &str) -> String {
    for suffix in ["ies", "ied"] {
        if let Some(base) = word.strip_suffix(suffix).filter(|b| b.chars().count() >= 2) {
            return format!("{base}y");
        }
    }
    for suffix in ["ing", "ed", "es", "s", "e"] {
        if suffix == "s" && word.ends_with("ss") {
            continue;
        }
        if let Some(base) = word.strip_suffix(suffix).filter(|b| b.chars().count() >= 3) {
            return base.to_string();
        }
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opposite_assertion_is_opposed_and_a_restatement_is_not() {
        for (a, b) in [
            ("Never retry the refund webhook", "Always retry the refund webhook"),
            ("Retry the refund webhook", "Don't retry the refund webhook"),
            ("The refund webhook must be retried", "The refund webhook must not be retried"),
            ("Retries of the refund webhook are safe", "Retrying the refund webhook is not safe"),
            ("Cache the domain map per process", "Do not cache the domain map per process"),
            ("不要重试退款回调", "退款回调超时后要重试"),
            ("部署前必须备份数据库", "部署前不必备份数据库"),
        ] {
            assert!(opposed(a, b), "{a} / {b}");
            assert!(opposed(b, a), "{b} / {a}");
        }
        for (a, b) in [
            ("Always retry the refund webhook", "Retry the refund webhook"),
            ("Never retry the refund webhook", "The refund webhook is never retried"),
            ("Never log the API key", "Retry the refund webhook"),
            ("Don't retry the refund webhook", "Don't block the checkout page on the webhook"),
            ("Never retry", "Always retry"),
            ("退款回调要重试", "退款回调必须重试"),
            ("不同项目的笔记互不可见", "不同项目的笔记共用一个数据库"),
        ] {
            assert!(!opposed(a, b), "{a} / {b}");
        }
    }

    #[test]
    fn a_successor_that_keeps_the_claim_refines_it() {
        assert!(refines("Use WAL", "Use WAL mode for every store"));
        assert!(refines("退款回调要重试", "退款回调要重试三次"));
        assert!(!refines("Use Redis for the session cache", "Use the SQLite session table"));
        assert!(!refines("Retry the webhook 3 times", "Retry the webhook 5 times"));
        assert!(!refines("Retry the refund webhook", "Never retry the refund webhook"), "a flip is a rejection");
    }

    #[test]
    fn a_title_s_claim_drops_its_polarity() {
        assert_eq!(claim("Don't retry the refund webhook"), Claim { negated: true, words: vec!["retry".into(), "refund".into(), "webhook".into()] });
        assert!(!claim("It is not true that you can't retry").negated, "two negations cancel");
        assert!(!claim("不同项目互不干扰").words.is_empty());
        assert!(!claim("不同项目共享数据库").negated, "不同 negates nothing");
        for (word, base) in [("retries", "retry"), ("retried", "retry"), ("retrying", "retry"), ("cached", "cach"), ("cache", "cach"), ("process", "process"), ("processes", "process")] {
            assert_eq!(stem(word), base, "{word}");
        }
    }
}
