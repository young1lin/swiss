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

//! A project's own settings (SPEC §memory.settings): the llm agents its runs ask, the
//! one that leads them, whether its notes are embedded, the documents it keeps and when
//! its changed notes are reviewed each day. A setting a project leaves out is decided a layer down - one run's
//! own choice first, then the project's, then the memory plugin's `sync` block, then the
//! agent added last ([choose]). Settings are the machine's, not knowledge: never exported,
//! gone with the project.

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{MemError, MemResult};
use crate::scope::GLOBAL_PROJECT_ID;
use crate::service::MemoryService;
use crate::store;

/// The most agents one layer names: a run's workers.
pub const AGENTS_MAX: usize = 8;
/// An llm agent's name: 1..=32 chars of [a-z0-9-], the first not a dash - the llm plugin's
/// rule (SPEC §llm.agents), restated because this crate does not link that one.
pub const NAME_MAX: usize = 32;

pub fn name_ok(name: &str) -> bool {
    let len = name.chars().count();
    (1..=NAME_MAX).contains(&len)
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What one layer says about a run's agents: nothing (the next layer down decides), off
/// (none, and no layer below is asked), or these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Choice {
    #[default]
    Inherit,
    Off,
    Named(Vec<String>),
}

impl Choice {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Choice::Inherit)
    }

    /// Read `field`: absent or null inherits, `"off"` is off, a name or a list of names
    /// (`one` refuses a list of more than one) names them. A name list is trimmed, kept in
    /// its order, and holds no name twice.
    pub fn parse(value: Option<&Value>, field: &str, one: bool) -> Result<Choice, String> {
        let shape = if one { "an llm agent's name or \"off\"" } else { "an llm agent's name, a list of names, or \"off\"" };
        let names: Vec<&str> = match value {
            None | Some(Value::Null) => return Ok(Choice::Inherit),
            Some(Value::String(s)) if s.trim() == "off" => return Ok(Choice::Off),
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(items)) if !one => {
                let mut names = Vec::new();
                for item in items {
                    names.push(item.as_str().ok_or_else(|| format!("{field} must be {shape}"))?);
                }
                names
            }
            Some(_) => return Err(format!("{field} must be {shape}")),
        };
        let mut out: Vec<String> = Vec::new();
        for name in names.iter().map(|n| n.trim()) {
            if !name_ok(name) {
                return Err(format!("{field} names llm agents: 1..={NAME_MAX} chars of [a-z0-9-], got {name:?}"));
            }
            if out.iter().any(|n| n == name) {
                return Err(format!("{field} names {name:?} twice"));
            }
            out.push(name.to_string());
        }
        if out.is_empty() {
            return Err(format!("{field} names no agent: leave it out to inherit, or say \"off\""));
        }
        if out.len() > AGENTS_MAX {
            return Err(format!("{field} names {} agents; at most {AGENTS_MAX}", out.len()));
        }
        Ok(Choice::Named(out))
    }

    /// The choice as the API shows it: null, `"off"`, the names (`one`: the name).
    pub fn to_json(&self, one: bool) -> Value {
        match self {
            Choice::Inherit => Value::Null,
            Choice::Off => json!("off"),
            Choice::Named(names) if one => json!(names.first()),
            Choice::Named(names) => json!(names),
        }
    }
}

/// A run's record keeps its own choice as `null`, `"off"` or the list of names.
impl Serialize for Choice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json(false).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Choice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Choice, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Choice::parse(Some(&value), "the run's agents", false).map_err(de::Error::custom)
    }
}

/// The layer a run's agents were decided at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Layer {
    /// The run's own request (`--agent`, `--lead`, the panel's pick for this run).
    Run,
    /// The project's settings.
    Project,
    /// The memory plugin's `sync` block.
    Plugin,
    /// The agent added last on the LLM Agents page.
    Newest,
}

/// What [choose] decided: the names, and the layer that named them - or, with no names,
/// the layer that said off (None: no layer named any, and no newest agent stood in).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    pub names: Vec<String>,
    pub from: Option<Layer>,
}

/// The first layer that says anything decides: the run's own choice, the project's, the
/// plugin block's; past all three, `newest` stands in when there is one (a lead and a
/// nudge's run pass None: neither falls back).
pub fn choose(run: &Choice, project: &Choice, plugin: &Choice, newest: Option<&str>) -> Pick {
    for (layer, choice) in [(Layer::Run, run), (Layer::Project, project), (Layer::Plugin, plugin)] {
        match choice {
            Choice::Inherit => continue,
            Choice::Off => return Pick { names: Vec::new(), from: Some(layer) },
            Choice::Named(names) => return Pick { names: names.clone(), from: Some(layer) },
        }
    }
    match newest {
        Some(name) => Pick { names: vec![name.to_string()], from: Some(Layer::Newest) },
        None => Pick::default(),
    }
}

impl Pick {
    /// The pick as the API shows it: the names and the layer.
    pub fn to_json(&self) -> Value {
        json!({ "names": self.names, "from": self.from })
    }
}

/// One project's settings; every field left out inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    /// The agents the project's runs ask: one worker or a pool of them.
    pub agents: Choice,
    /// The agent a map or full run's judgment goes to (SPEC §memory.init Lead).
    pub lead: Choice,
    /// Whether the project's notes are embedded (SPEC §memory.embedding): None inherits
    /// (on whenever a model is configured), Some(false) never, Some(true) even if a later
    /// default turns embedding off. The global scope may set it too: it is about search,
    /// not a checkout.
    pub embedding: Option<bool>,
    /// The sources of the documents the project keeps (SPEC §memory.docs): directories or
    /// files inside the checkout, each [crate::docs::clean_source]. Empty keeps none.
    pub docs: Vec<String>,
    /// The time of day, `HH:MM` on the gateway's clock, a review of the whole inbox,
    /// including human notes, is queued each day (SPEC §memory.settings autoReview); None
    /// queues none.
    pub auto_review: Option<String>,
    /// How many review runs the daily review may queue in a day: a later one only while the
    /// one before left questions for the next. None is [AUTO_REVIEW_RUNS_DEFAULT].
    pub auto_review_runs: Option<usize>,
}

/// The review runs a daily review queues in a day unless its settings say otherwise, and
/// the most they may say.
pub const AUTO_REVIEW_RUNS_DEFAULT: usize = 3;
pub const AUTO_REVIEW_RUNS_MAX: usize = 10;

/// The fields a settings object may carry.
const FIELDS: [&str; 6] = ["agents", "lead", "embedding", "docs", "autoReview", "autoReviewRuns"];

/// An `HH:MM` time of day as (hour, minute): 00:00 to 23:59, two digits each.
pub fn slot_of(text: &str) -> Option<(u32, u32)> {
    let (hour, minute) = text.split_once(':')?;
    let two = |part: &str| match part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()) {
        true => part.parse::<u32>().ok(),
        false => None,
    };
    let (hour, minute) = (two(hour)?, two(minute)?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

impl ProjectSettings {
    /// Read a whole settings object; an unknown field is refused.
    pub fn parse(value: &Value) -> Result<ProjectSettings, String> {
        let obj = value.as_object().ok_or("the settings must be an object")?;
        check_fields(obj)?;
        Ok(ProjectSettings {
            agents: Choice::parse(obj.get("agents"), "agents", false)?,
            lead: Choice::parse(obj.get("lead"), "lead", true)?,
            embedding: embedding_of(obj.get("embedding"))?,
            docs: docs_of(obj.get("docs"))?,
            auto_review: auto_review_of(obj.get("autoReview"))?,
            auto_review_runs: auto_review_runs_of(obj.get("autoReviewRuns"))?,
        })
    }

    /// Apply a merge patch: a field the patch carries is set (null inherits again), one it
    /// leaves out keeps its value.
    pub fn patched(&self, patch: &Value) -> Result<ProjectSettings, String> {
        let obj = patch.as_object().ok_or("the settings patch must be an object")?;
        check_fields(obj)?;
        let mut next = self.clone();
        if obj.contains_key("agents") {
            next.agents = Choice::parse(obj.get("agents"), "agents", false)?;
        }
        if obj.contains_key("lead") {
            next.lead = Choice::parse(obj.get("lead"), "lead", true)?;
        }
        if obj.contains_key("embedding") {
            next.embedding = embedding_of(obj.get("embedding"))?;
        }
        if obj.contains_key("docs") {
            next.docs = docs_of(obj.get("docs"))?;
        }
        if obj.contains_key("autoReview") {
            next.auto_review = auto_review_of(obj.get("autoReview"))?;
        }
        if obj.contains_key("autoReviewRuns") {
            next.auto_review_runs = auto_review_runs_of(obj.get("autoReviewRuns"))?;
        }
        Ok(next)
    }

    /// Every field, null where it inherits.
    pub fn to_json(&self) -> Value {
        json!({
            "agents": self.agents.to_json(false),
            "lead": self.lead.to_json(true),
            "embedding": self.embedding,
            "docs": self.docs,
            "autoReview": self.auto_review,
            "autoReviewRuns": self.auto_review_runs,
        })
    }

    /// What the store keeps: the fields that say something.
    fn stored(&self) -> Value {
        let mut out = Map::new();
        if !self.agents.is_inherit() {
            out.insert("agents".into(), self.agents.to_json(false));
        }
        if !self.lead.is_inherit() {
            out.insert("lead".into(), self.lead.to_json(true));
        }
        if let Some(embedding) = self.embedding {
            out.insert("embedding".into(), json!(embedding));
        }
        if !self.docs.is_empty() {
            out.insert("docs".into(), json!(self.docs));
        }
        if let Some(slot) = &self.auto_review {
            out.insert("autoReview".into(), json!(slot));
        }
        if let Some(runs) = self.auto_review_runs {
            out.insert("autoReviewRuns".into(), json!(runs));
        }
        Value::Object(out)
    }
}

/// The autoReviewRuns field's shape: null takes the default, else 1 to [AUTO_REVIEW_RUNS_MAX].
fn auto_review_runs_of(value: Option<&Value>) -> Result<Option<usize>, String> {
    let shape = format!("autoReviewRuns must be a whole number 1 to {AUTO_REVIEW_RUNS_MAX}, or null for {AUTO_REVIEW_RUNS_DEFAULT}");
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_u64().filter(|n| (1..=AUTO_REVIEW_RUNS_MAX as u64).contains(n)) {
            Some(runs) => Ok(Some(runs as usize)),
            None => Err(format!("{shape}, got {n}")),
        },
        Some(_) => Err(shape),
    }
}

/// The autoReview field's shape: null queues none, else an `HH:MM` time of day.
fn auto_review_of(value: Option<&Value>) -> Result<Option<String>, String> {
    let shape = "autoReview must be a time of day HH:MM (00:00 to 23:59), or null for none";
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => match slot_of(text.trim()) {
            Some((hour, minute)) => Ok(Some(format!("{hour:02}:{minute:02}"))),
            None => Err(format!("{shape}, got {text:?}")),
        },
        Some(_) => Err(shape.to_string()),
    }
}

/// The embedding field's shape: null inherits, a bool is the answer.
fn embedding_of(value: Option<&Value>) -> Result<Option<bool>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(on)) => Ok(Some(*on)),
        Some(_) => Err("embedding must be true, false or null (inherit)".to_string()),
    }
}

/// The docs field's shape: null or absent keeps none, else a list of sources.
fn docs_of(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut raw = Vec::new();
            for item in items {
                raw.push(item.as_str().ok_or("docs must be a list of paths inside the checkout")?.to_string());
            }
            crate::docs::clean_sources(&raw)
        }
        Some(_) => Err("docs must be a list of paths inside the checkout".to_string()),
    }
}

fn check_fields(obj: &Map<String, Value>) -> Result<(), String> {
    match obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        Some(key) => Err(format!("the settings have no {key:?} field")),
        None => Ok(()),
    }
}

impl MemoryService {
    /// A project's settings; a project that never set any inherits everything.
    pub async fn project_settings(&self, project_id: &str) -> MemResult<ProjectSettings> {
        let mut conn = self.store().await?.read().await?;
        if store::project(&mut conn, project_id).await?.is_none() {
            return Err(MemError::not_found(format!("no project {project_id}")));
        }
        settings_of(&mut conn, project_id).await
    }

    /// Patch a project's settings ([ProjectSettings::patched]) and answer them as saved.
    /// The global scope runs no sync: it names no agents.
    pub async fn patch_project_settings(&self, project_id: &str, patch: &Value) -> MemResult<ProjectSettings> {
        let store = self.store().await?;
        let mut conn = store.write().await?;
        let outcome = async {
            if store::project(&mut conn, project_id).await?.is_none() {
                return Err(MemError::not_found(format!("no project {project_id}")));
            }
            let next = settings_of(&mut conn, project_id).await?.patched(patch).map_err(MemError::invalid)?;
            if project_id == GLOBAL_PROJECT_ID && (!next.agents.is_inherit() || !next.lead.is_inherit()) {
                return Err(MemError::invalid("the global scope has no checkout to sync: it names no agents"));
            }
            if project_id == GLOBAL_PROJECT_ID && !next.docs.is_empty() {
                return Err(MemError::invalid("the global scope has no checkout: it keeps no documents"));
            }
            if project_id == GLOBAL_PROJECT_ID && (next.auto_review.is_some() || next.auto_review_runs.is_some()) {
                return Err(MemError::invalid("the global scope has no checkout: no file under its notes changes, nothing to review"));
            }
            let text = serde_json::to_string(&next.stored()).map_err(|e| MemError::store(e.to_string()))?;
            store::set_project_settings(&mut conn, project_id, &text, store::now()).await?;
            Ok(next)
        }
        .await;
        store::finish(&mut conn, outcome).await
    }
}

async fn settings_of(conn: &mut sqlx::SqliteConnection, project_id: &str) -> MemResult<ProjectSettings> {
    match store::project_settings(conn, project_id).await? {
        None => Ok(ProjectSettings::default()),
        Some(text) => {
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| MemError::store(format!("the settings of project {project_id}: {e}")))?;
            ProjectSettings::parse(&value).map_err(|why| MemError::store(format!("the settings of project {project_id}: {why}")))
        }
    }
}

/// Whether a project's notes are embedded (SPEC §memory.embedding): its own setting,
/// on (true) where it inherits. A settings row that will not read reads as inherit - a
/// read like search degrades to the default, it does not die on the store's bug.
pub async fn embedding_on(conn: &mut sqlx::SqliteConnection, project_id: &str) -> MemResult<bool> {
    let Some(text) = store::project_settings(conn, project_id).await? else {
        return Ok(true);
    };
    let read = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| ProjectSettings::parse(&value).ok())
        .map(|settings| settings.embedding);
    Ok(read.unwrap_or(None).unwrap_or(true))
}

/// The projects whose settings turn embedding off (SPEC §memory.embedding): the
/// indexer never embeds their notes. A row that will not read is skipped, not fatal.
pub async fn embedding_off_projects(conn: &mut sqlx::SqliteConnection) -> MemResult<Vec<String>> {
    let mut out = Vec::new();
    for (project_id, text) in store::all_project_settings(conn).await? {
        let off = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| ProjectSettings::parse(&value).ok())
            .is_some_and(|settings| settings.embedding == Some(false));
        if off {
            out.push(project_id);
        }
    }
    Ok(out)
}

impl MemoryService {
    /// Whether this project's notes are embedded, opening the store's read itself.
    pub async fn embedding_on(&self, project_id: &str) -> MemResult<bool> {
        let mut conn = self.store().await?.read().await?;
        embedding_on(&mut conn, project_id).await
    }

    /// The projects whose settings queue a daily review of their changed notes (SPEC
    /// §memory.settings autoReview). A row that will not read is skipped.
    pub async fn auto_reviews(&self) -> MemResult<Vec<AutoReview>> {
        let mut conn = self.store().await?.read().await?;
        let mut out = Vec::new();
        for (project, text) in store::all_project_settings(&mut conn).await? {
            let Some(settings) = serde_json::from_str::<Value>(&text).ok().and_then(|value| ProjectSettings::parse(&value).ok())
            else {
                continue;
            };
            if let Some(slot) = settings.auto_review.filter(|_| project != GLOBAL_PROJECT_ID) {
                let runs = settings.auto_review_runs.unwrap_or(AUTO_REVIEW_RUNS_DEFAULT);
                out.push(AutoReview { project, slot, runs });
            }
        }
        Ok(out)
    }
}

/// A project's daily review as its settings set it: its time of day and how many review
/// runs it may queue in a day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReview {
    pub project: String,
    pub slot: String,
    pub runs: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Choice {
        Choice::Named(names.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn a_choice_reads_off_a_name_or_a_list() {
        let read = |value: Value, one: bool| Choice::parse(Some(&value), "agents", one);
        assert_eq!(Choice::parse(None, "agents", false), Ok(Choice::Inherit));
        assert_eq!(read(Value::Null, false), Ok(Choice::Inherit));
        assert_eq!(read(json!(" off "), false), Ok(Choice::Off));
        assert_eq!(read(json!(" glm "), false), Ok(named(&["glm"])));
        assert_eq!(read(json!(["b", " a"]), false), Ok(named(&["b", "a"])), "the order is kept");
        assert!(read(json!(["a", "a"]), false).expect_err("twice").contains("twice"));
        assert!(read(json!([]), false).expect_err("empty").contains("no agent"));
        assert!(read(json!(["Not A Name"]), false).expect_err("a name").contains("[a-z0-9-]"));
        assert!(read(json!(["a", "b"]), true).is_err(), "a lead is one agent");
        let nine: Vec<String> = (0..9).map(|i| format!("a{i}")).collect();
        assert!(read(json!(nine), false).expect_err("too many").contains("at most 8"));
        assert!(read(json!(3), false).is_err());
        // The run record's spelling reads back as written.
        for choice in [Choice::Inherit, Choice::Off, named(&["x", "y"])] {
            let text = serde_json::to_string(&choice).expect("serialize");
            assert_eq!(serde_json::from_str::<Choice>(&text).expect("deserialize"), choice, "{text}");
        }
    }

    #[test]
    fn the_first_layer_that_says_anything_decides() {
        let none = Choice::Inherit;
        let pick = |run: &Choice, project: &Choice, plugin: &Choice, newest| choose(run, project, plugin, newest);
        let from = |names: &[&str], layer| Pick { names: names.iter().map(|n| n.to_string()).collect(), from: Some(layer) };
        assert_eq!(pick(&named(&["r"]), &named(&["p"]), &named(&["g"]), Some("n")), from(&["r"], Layer::Run));
        assert_eq!(pick(&none, &named(&["p", "q"]), &named(&["g"]), Some("n")), from(&["p", "q"], Layer::Project));
        assert_eq!(pick(&none, &none, &named(&["g"]), Some("n")), from(&["g"], Layer::Plugin));
        assert_eq!(pick(&none, &none, &none, Some("n")), from(&["n"], Layer::Newest));
        assert_eq!(pick(&none, &none, &none, None), Pick::default(), "nothing named and no agent to stand in");
        // Off stops the walk: no layer below is asked, the newest agent included.
        assert_eq!(pick(&none, &Choice::Off, &named(&["g"]), Some("n")), from(&[], Layer::Project));
        assert_eq!(pick(&named(&["r"]), &Choice::Off, &none, None), from(&["r"], Layer::Run), "a run's own pick beats a project's off");
    }

    #[test]
    fn a_patch_sets_what_it_carries_and_keeps_the_rest() {
        let set = ProjectSettings::default()
            .patched(&json!({ "agents": ["a", "b"], "lead": "boss" }))
            .expect("patch");
        assert_eq!((set.agents.clone(), set.lead.clone()), (named(&["a", "b"]), named(&["boss"])));
        assert_eq!(
            set.to_json(),
            json!({ "agents": ["a", "b"], "lead": "boss", "embedding": null, "docs": [], "autoReview": null, "autoReviewRuns": null })
        );
        let kept = set.patched(&json!({ "lead": null })).expect("patch");
        assert_eq!((kept.agents.clone(), kept.lead.clone()), (named(&["a", "b"]), Choice::Inherit));
        assert_eq!(kept.stored(), json!({ "agents": ["a", "b"] }), "the store keeps what says something");
        assert_eq!(ProjectSettings::parse(&kept.stored()), Ok(kept.clone()));
        assert!(kept.patched(&json!({ "agent": "a" })).expect_err("unknown").contains("\"agent\""));
        assert!(kept.patched(&json!({ "lead": ["a"] })).is_err(), "a lead is one name");
    }

    #[test]
    fn an_embedding_is_a_bool_or_an_inherit() {
        let read = |value: Value| ProjectSettings::default().patched(&json!({ "embedding": value }));
        assert_eq!(read(Value::Null).expect("null").embedding, None, "null inherits again");
        assert_eq!(read(json!(false)).expect("off").embedding, Some(false));
        assert_eq!(read(json!(true)).expect("on").embedding, Some(true));
        assert!(read(json!("off")).expect_err("a word").contains("true, false or null"));
        // The store keeps what says something: an unset embedding is not stored.
        let off = read(json!(false)).expect("off");
        assert_eq!(off.stored(), json!({ "embedding": false }));
        assert_eq!(ProjectSettings::parse(&off.stored()), Ok(off.clone()));
        let kept = off.patched(&json!({ "agents": "a" })).expect("patch");
        assert_eq!(kept.embedding, Some(false), "a field left out keeps its value");
        assert_eq!(kept.stored(), json!({ "agents": ["a"], "embedding": false }));
    }

    #[test]
    fn an_auto_review_is_a_time_of_day_or_none() {
        let read = |value: Value| ProjectSettings::default().patched(&json!({ "autoReview": value }));
        let set = read(json!(" 03:30 ")).expect("a time");
        assert_eq!(set.auto_review.as_deref(), Some("03:30"));
        assert_eq!(set.stored(), json!({ "autoReview": "03:30" }));
        assert_eq!(ProjectSettings::parse(&set.stored()), Ok(set.clone()));
        assert_eq!(read(json!("23:59")).expect("the last minute").auto_review.as_deref(), Some("23:59"));
        assert_eq!(read(Value::Null).expect("null").auto_review, None, "null queues none");
        for bad in [json!("24:00"), json!("9:30"), json!("09:60"), json!("0930"), json!("off"), json!(930), json!("+9:30")] {
            assert!(read(bad.clone()).expect_err("not a time").contains("HH:MM"), "{bad}");
        }
        assert_eq!(set.patched(&json!({ "lead": "a" })).expect("patch").auto_review, set.auto_review, "a field left out keeps its value");
        assert_eq!(slot_of("07:05"), Some((7, 5)));
        assert_eq!(slot_of("7:05"), None);
    }

    #[test]
    fn a_daily_reviews_runs_are_one_to_ten_or_the_default() {
        let read = |value: Value| ProjectSettings::default().patched(&json!({ "autoReviewRuns": value }));
        let set = read(json!(5)).expect("five");
        assert_eq!(set.auto_review_runs, Some(5));
        assert_eq!(set.stored(), json!({ "autoReviewRuns": 5 }));
        assert_eq!(ProjectSettings::parse(&set.stored()), Ok(set.clone()));
        assert_eq!(read(json!(AUTO_REVIEW_RUNS_MAX)).expect("the most").auto_review_runs, Some(AUTO_REVIEW_RUNS_MAX));
        assert_eq!(read(Value::Null).expect("null").auto_review_runs, None, "null takes the default");
        for bad in [json!(0), json!(AUTO_REVIEW_RUNS_MAX + 1), json!(-1), json!(2.5), json!("3")] {
            assert!(read(bad.clone()).expect_err("not a count").contains("autoReviewRuns must be a whole number 1 to 10"), "{bad}");
        }
        assert_eq!(set.patched(&json!({ "autoReview": "03:30" })).expect("patch").auto_review_runs, Some(5), "a field left out keeps its value");
    }

    #[test]
    fn docs_are_sources_inside_the_checkout() {
        let read = |value: Value| ProjectSettings::default().patched(&json!({ "docs": value }));
        let set = read(json!([".\\.claude\\skills\\business-logic\\", "docs"])).expect("sources");
        assert_eq!(set.docs, [".claude/skills/business-logic", "docs"]);
        assert_eq!(set.stored(), json!({ "docs": [".claude/skills/business-logic", "docs"] }));
        assert_eq!(ProjectSettings::parse(&set.stored()), Ok(set.clone()));
        assert!(read(Value::Null).expect("null").docs.is_empty(), "null keeps none");
        assert!(read(json!("docs")).expect_err("a word").contains("a list"));
        assert!(read(json!(["../elsewhere"])).expect_err("outside").contains("inside the checkout"));
        assert!(read(json!(["C:\\abs"])).expect_err("absolute").contains("absolute"));
        assert_eq!(set.patched(&json!({ "lead": "a" })).expect("patch").docs, set.docs, "a field left out keeps its value");
    }
}
