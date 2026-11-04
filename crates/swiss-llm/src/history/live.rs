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

//! The runs in flight (SPEC llm.history Live): what each is doing right now - the round it
//! is on, the step it waits on and for how long, what the round's stream has carried, and
//! the turns noted so far - held in memory beside the store and read beside the filed rows.
//! Nothing here is filed: the run row and its turns still land whole at finish, and a
//! restart forgets every entry (a row a stopped gateway stranded is not live).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use super::*;

/// The turns a recorder notes: its own list, shared with the live registry so a running
/// run's dialogue reads without a copy kept beside it.
pub(super) type Notes = Arc<Mutex<Vec<Note>>>;

/// What a live run waits on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    /// A provider request in flight - for a spawned runtime, its process.
    Model,
    /// One of its tools running.
    Tool(String),
}

struct LiveRun {
    started: Instant,
    notes: Notes,
    /// The loop's rounds begun so far; a single ask stays at 0.
    round: u32,
    step: Step,
    since: Instant,
    /// The characters the current round's stream has carried, thinking and text.
    streamed: u64,
}

/// Every run in flight in this process, by its reserved id.
#[derive(Default)]
pub(super) struct LiveRuns(Mutex<HashMap<i64, LiveRun>>);

/// The longest opening of a live run's first ask its row carries.
const HEAD_CHARS: usize = 400;

impl LiveRuns {
    fn with(&self, id: i64, change: impl FnOnce(&mut LiveRun)) {
        if let Ok(mut runs) = self.0.lock() {
            if let Some(run) = runs.get_mut(&id) {
                change(run);
            }
        }
    }

    /// A reserved (or resumed) run goes live, waiting on its first ask.
    pub(super) fn begin(&self, id: i64, notes: Notes) {
        let now = Instant::now();
        if let Ok(mut runs) = self.0.lock() {
            runs.insert(id, LiveRun { started: now, notes, round: 0, step: Step::Model, since: now, streamed: 0 });
        }
    }

    /// The loop begins a round: its request is about to fly.
    pub(super) fn round(&self, id: i64) {
        self.with(id, |run| {
            run.round += 1;
            run.step = Step::Model;
            run.since = Instant::now();
            run.streamed = 0;
        });
    }

    /// One of the round's tools starts.
    pub(super) fn tool(&self, id: i64, name: &str) {
        self.with(id, |run| {
            run.step = Step::Tool(name.to_string());
            run.since = Instant::now();
        });
    }

    /// The round's stream carried `chars` more.
    pub(super) fn streamed(&self, id: i64, chars: usize) {
        self.with(id, |run| run.streamed += chars as u64);
    }

    /// The run filed (or its recorder went away): it is no longer live.
    pub(super) fn end(&self, id: i64) {
        if let Ok(mut runs) = self.0.lock() {
            runs.remove(&id);
        }
    }

    /// Whether run `id` is in flight here: a recorder of this handle holds it.
    pub(super) fn has(&self, id: i64) -> bool {
        self.0.lock().is_ok_and(|runs| runs.contains_key(&id))
    }

    pub(super) fn ids(&self) -> Vec<i64> {
        self.0.lock().map(|runs| runs.keys().copied().collect()).unwrap_or_default()
    }

    /// One run's live facts as a read attaches them, or None when it is not running: how
    /// long it has run, the round, the step and how long it has waited on it, what the
    /// round's stream carried, the turns noted so far, its tool calls by tool and the
    /// opening of its first ask.
    pub(super) fn view(&self, id: i64) -> Option<Value> {
        let runs = self.0.lock().ok()?;
        let run = runs.get(&id)?;
        let notes = run.notes.lock().ok()?;
        let mut tools: BTreeMap<String, i64> = BTreeMap::new();
        for note in notes.iter().filter(|note| note.role == "assistant") {
            if let Some(rest) = note.text.strip_prefix("tool call ") {
                let name = rest.split([' ', ':']).next().unwrap_or("").trim();
                if !name.is_empty() {
                    *tools.entry(name.to_string()).or_default() += 1;
                }
            }
        }
        let head = notes
            .iter()
            .find(|note| note.role == "user")
            .map(|note| note.text.chars().take(HEAD_CHARS).collect::<String>());
        let (step, tool) = match &run.step {
            Step::Model => ("model", None),
            Step::Tool(name) => ("tool", Some(name.clone())),
        };
        Some(json!({
            "elapsedMs": run.started.elapsed().as_millis() as u64,
            "round": run.round,
            "step": step,
            "tool": tool,
            "stepMs": run.since.elapsed().as_millis() as u64,
            "streamed": run.streamed,
            "turns": notes.len(),
            "tools": tools,
            "head": head,
        }))
    }

    /// The turns a live run noted so far, owned (at, role, text): the dialogue read lists
    /// them after the filed ones.
    pub(super) fn notes(&self, id: i64) -> Option<Vec<(String, String, String)>> {
        let runs = self.0.lock().ok()?;
        let notes = runs.get(&id)?.notes.lock().ok()?;
        Some(notes.iter().map(|note| (note.at.clone(), note.role.clone(), note.text.clone())).collect())
    }
}

/// What a running run's provider stream reports into the registry while the recorder is
/// borrowed by the round (see [RunRecorder::tap]). A history-less run's tap reports nothing.
pub struct LiveTap(Option<(Arc<LlmHistory>, i64)>);

impl LiveTap {
    /// The round's stream carried `chars` more characters.
    pub fn streamed(&self, chars: usize) {
        if let Some((history, id)) = &self.0 {
            history.live.streamed(*id, chars);
        }
    }
}

impl RunRecorder {
    /// The tap the round's stream reports through.
    pub fn tap(&self) -> LiveTap {
        LiveTap(self.history.clone().zip(self.id))
    }

    /// One of the round's tools starts: the live step names it.
    pub fn calling(&self, tool: &str) {
        if let (Some(history), Some(id)) = (&self.history, self.id) {
            history.live.tool(id, tool);
        }
    }
}

impl Drop for RunRecorder {
    /// A recorder dropped unfiled (a consumer's deadline dropped the future driving it)
    /// leaves no run looking live.
    fn drop(&mut self) {
        if let (Some(history), Some(id)) = (&self.history, self.id) {
            history.live.end(id);
        }
    }
}
