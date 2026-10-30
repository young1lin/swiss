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

//! One checked question (SPEC §memory.map, §memory.init): asked, and asked once more on
//! an answer its check refuses, under the history scope that says what it is for; its
//! cost tallied on the run's runner row.

use super::*;

/// One checked answer: what the check made of it or why not, the questions it took and
/// what they cost.
pub(super) type Checked<T> = (Result<T, String>, u32, engine::ReplyMeta);

/// One area's answer.
pub(super) type Filled = Checked<Fill>;

pub(super) fn add_cost(total: &mut engine::ReplyMeta, meta: &engine::ReplyMeta) {
    if let Some(usd) = meta.cost_usd {
        total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + usd);
    }
    if let Some(turns) = meta.turns {
        total.turns = Some(total.turns.unwrap_or(0) + turns);
    }
}

/// The runner's row counts one more answer's questions and cost.
pub(super) fn tally(info: &mut RunnerInfo, attempts: u32, meta: &engine::ReplyMeta) {
    info.attempts += attempts;
    if let Some(usd) = meta.cost_usd {
        info.cost_usd = Some(info.cost_usd.unwrap_or(0.0) + usd);
    }
    if let Some(turns) = meta.turns {
        info.turns = Some(info.turns.unwrap_or(0) + turns);
    }
}

/// Who answers a checked question: a runner's fill, a runner looking with its tools, or a
/// lead with the run's workers at hand.
#[derive(Clone, Copy)]
pub(super) enum Asker<'a> {
    Fill(&'a dyn Ask),
    Look(&'a dyn Ask),
    Lead(&'a dyn Ask, Option<&'a dyn Ask>),
}

impl Asker<'_> {
    async fn answer(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        match *self {
            Asker::Fill(ask) => ask.fill(root, prompt, schema).await,
            Asker::Look(ask) => ask.look(root, prompt, schema).await,
            Asker::Lead(lead, workers) => lead.lead(root, prompt, schema, workers).await,
        }
    }
}

/// The history scope of one memory question (SPEC llm.history): what it is for and what
/// it is about, under the sync run the scope already names as its source.
pub(super) fn question_scope(task: &str, subject: impl Into<String>) -> RunContext {
    RunContext { task: Some(task.into()), subject: Some(subject.into()), ..context() }
}

/// What one notes question is about: its domain and the first file it reads.
pub(super) fn slice_subject(slice: &init::Slice) -> String {
    let first = slice.files().first().map(|f| f.to_string()).unwrap_or_default();
    match &slice.domain {
        Some(domain) if !first.is_empty() => format!("{domain}: {first}"),
        Some(domain) => domain.clone(),
        None => first,
    }
}

/// One question asked, and asked once more on an answer `check` refuses; `shape` names
/// what a good answer is.
pub(super) async fn ask_checked<T>(
    ask: Asker<'_>,
    root: &Path,
    prompt: String,
    schema: &Value,
    shape: &str,
    check: impl Fn(&Value) -> Result<T, String>,
) -> Checked<T> {
    let mut cost = engine::ReplyMeta::default();
    let mut prompt = prompt;
    for attempt in 1..=2u32 {
        let stdout = match ask.answer(root, &prompt, schema).await {
            Ok(stdout) => stdout,
            Err(err) => return (Err(err), attempt, cost),
        };
        let (object, meta) = engine::read_object(&stdout);
        add_cost(&mut cost, &meta);
        let why = match object {
            Ok(value) => match check(&value) {
                Ok(checked) => return (Ok(checked), attempt, cost),
                Err(why) => why,
            },
            Err(Answer::Failed(why)) => return (Err(why), attempt, cost),
            Err(Answer::Invalid(why)) => why,
            Err(Answer::Proposal(_)) => format!("the answer is not {shape}"),
        };
        if attempt == 2 {
            return (Err(format!("answered twice without {shape}: {why}")), attempt, cost);
        }
        prompt = format!("{prompt}{}", survey::retry_note(&why));
    }
    // The second attempt returns above; this return is for the compiler, never taken,
    // and keeps the path free of a panic.
    (Err(format!("asked twice without {shape}")), 2, cost)
}

/// One area asked; [Survey::check] holds its answer to the code index.
pub(super) async fn fill_one(ask: &dyn Ask, root: &Path, survey: &Survey, at: usize, prompt: String, schema: &Value) -> Filled {
    let asked = ask_checked(Asker::Fill(ask), root, prompt, schema, "a valid description", |value| survey.check(at, value));
    within(question_scope("memory.map", survey.units[at].path.clone()), asked).await
}
