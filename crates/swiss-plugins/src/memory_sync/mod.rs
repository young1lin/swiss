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

//! The sync adapters (SPEC §memory.sync): the `sync` config block, the git walk and
//! the transcript digest that gather one run's material, the runner the proposal is
//! asked of - the llm plugin's agent the block names (SPEC §llm.agents), by its
//! runtime: memory's own tools in the builtin loop, one chat request, a spawned
//! `claude` per the SPEC's flag list or a spawned `pi` - and the queue worker that
//! drives runs one at a time. Every rule lives in swiss-memory's sync.rs; this module
//! only reaches the world. Hooks and the CLI's gateway calls live here too, called
//! from the root crate's `memory_cli`.
//!
//! One namespace over its files: this one holds the `sync` block, the enqueue requests
//! and the imports every file shares (each takes `use super::*`); the git walk, the
//! runners, one checked question, each run kind, the hooks, the worker and the daily
//! review have a file each.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use serde_json::{json, Map, Value};
use swiss_core::log;
use swiss_llm::agent::{AgentConfig, Effort, Runtime};
use swiss_llm::history::{context, within, RunContext};
use swiss_llm::provider::{self, Chat};
use swiss_llm::telemetry::{Session, TelemetryConfig};
use swiss_llm::LlmState;
use swiss_memory::docs;
use swiss_memory::domain::CommitFiles;
use swiss_memory::init::{self, Saved};
use swiss_memory::recheck;
use swiss_memory::service::{MemoryService, Target};
use swiss_memory::settings::{self, Choice, Layer, Pick, ProjectSettings};
use swiss_memory::survey::{self, Draw, Fill, Survey};
use swiss_memory::sync::{self as engine};
use swiss_memory::sync::{Advance, Answer, Change, Keep, Material, Next, Run, RunState, RunnerInfo};
use swiss_memory::sync::{DocsScope, EmbeddingScope, Enqueue, Phase, RecheckScope, Trigger, WrapupScope};
use swiss_memory::transcript::{self, Digest};
use swiss_memory::{MemError, MemResult};

use crate::memory_agent::{answer_once, BuiltinAsk};
use crate::memory_pool::{Pool, Worker};
use crate::memory_preview::{self as preview, git};

mod git_walk;
mod runners;
mod sync_run;
mod question;
mod map_run;
mod full_run;
mod recheck_run;
mod docs_run;
mod hooks;
mod worker;
mod daily_review;

pub use self::git_walk::*;
pub use self::runners::*;
pub use self::sync_run::*;
use self::question::*;
pub use self::map_run::*;
pub use self::full_run::*;
pub use self::recheck_run::*;
pub use self::docs_run::*;
pub use self::hooks::*;
pub use self::worker::*;
pub use self::daily_review::*;

/// The keys a `sync` block may carry.
const KEYS: [&str; 6] = ["agent", "lead", "transcripts", "transcriptDirs", "auto", "minIntervalSec"];
/// The keys the block carried before runners moved to the llm plugin's agents: refused
/// with the way forward, never silently ignored.
const MOVED: [&str; 6] = ["runner", "provider", "command", "model", "env", "timeoutMs"];
/// What a sync run's reporting session names as its consumer.
const CONSUMER: &str = "memory.sync";

const DIRS_MAX: usize = 8;
const INTERVAL_MAX_SEC: u64 = 86_400;
const INTERVAL_DEFAULT_SEC: u64 = 600;
/// A nudge's run is due this long after the nudge, and never sooner than
/// `minIntervalSec` after the last run finished.
pub const NUDGE_DELAY_MS: i64 = 30_000;
/// How long the patch is read raw before [engine::shape_patch] cuts it.
const PATCH_READ_BYTES: usize = 512 * 1024;
/// The runner's stdout cap (SPEC §memory.sync The worker).
const RUNNER_STDOUT_MAX: usize = 4 * 1024 * 1024;

/// The memory plugin's `sync` block, checked and with its defaults filled.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncConfig {
    /// The llm plugin's agents a run asks (`agent`: one name, a list - a pool - or off);
    /// left out, a project's settings or the agent added last decide (SPEC
    /// §memory.settings). Whether an agent exists is the llm row's business, asked at run
    /// time: the two rows save apart.
    pub agents: Choice,
    /// The llm agent a map or full run's judgment goes to (SPEC §memory.init Lead): the
    /// review of the top level and each domain's wrap-up; none leaves both to the workers.
    pub lead: Choice,
    /// Whether coding-agent conversations - Claude Code's and DSH's - are read
    /// (`transcripts: claude-code | off`; the value predates DSH and stays).
    pub transcripts: bool,
    pub transcript_dirs: Vec<PathBuf>,
    /// Whether a nudge queues runs (`auto`).
    pub auto: bool,
    pub min_interval_sec: u64,
}

fn text(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("sync.{key} must be a string")),
    }
}

/// The agents at `key` ([Choice::parse]; `one` for a single agent); an empty name is off,
/// as it always was.
fn agent_choice(obj: &Map<String, Value>, key: &str, one: bool) -> Result<Choice, String> {
    match obj.get(key) {
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Choice::Off),
        value => Choice::parse(value, &format!("sync.{key}"), one),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl SyncConfig {
    /// No block: no agent named, the defaults otherwise - hand-built, the same values
    /// [SyncConfig::parse] reads out of an empty block (a test holds the two together).
    pub fn empty() -> SyncConfig {
        SyncConfig {
            agents: Choice::Inherit,
            lead: Choice::Inherit,
            transcripts: true,
            transcript_dirs: Vec::new(),
            auto: true,
            min_interval_sec: INTERVAL_DEFAULT_SEC,
        }
    }

    /// The block of a whole plugin config row: None when the row has none.
    pub fn from_plugin(config: &Value) -> Result<Option<SyncConfig>, String> {
        match config.get("sync") {
            None | Some(Value::Null) => Ok(None),
            Some(block) => SyncConfig::parse(block).map(Some),
        }
    }

    /// Check one block; every error names its field as `sync.<field>`.
    pub fn parse(block: &Value) -> Result<SyncConfig, String> {
        let obj = block.as_object().ok_or("sync must be an object")?;
        if let Some(key) = obj.keys().find(|k| MOVED.contains(&k.as_str())) {
            return Err(format!(
                "sync.{key} moved to the llm plugin: define the runner as an agent on the LLM Agents page and name it with sync.agent"
            ));
        }
        if let Some(key) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("sync has no {key:?} field"));
        }
        let agents = agent_choice(obj, "agent", false)?;
        let lead = agent_choice(obj, "lead", true)?;
        let transcripts = match text(obj, "transcripts")?.as_deref().map(str::trim) {
            None | Some("claude-code") => true,
            Some("off") => false,
            Some(other) => return Err(format!("sync.transcripts must be \"claude-code\" or \"off\", got {other:?}")),
        };
        let transcript_dirs = match obj.get("transcriptDirs") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => {
                if items.len() > DIRS_MAX {
                    return Err(format!("sync.transcriptDirs holds {} paths; at most {DIRS_MAX}", items.len()));
                }
                let mut out = Vec::new();
                for item in items {
                    let raw = item.as_str().ok_or("sync.transcriptDirs entries must be strings")?;
                    let path = PathBuf::from(shellexpand(raw)?);
                    if !path.is_absolute() {
                        return Err(format!("sync.transcriptDirs entries must be absolute or ~/ paths, got {raw:?}"));
                    }
                    out.push(path);
                }
                out
            }
            Some(_) => return Err("sync.transcriptDirs must be an array of paths".into()),
        };
        let auto = match obj.get("auto") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("sync.auto must be a boolean".into()),
        };
        let min_interval_sec = match obj.get("minIntervalSec") {
            None | Some(Value::Null) => INTERVAL_DEFAULT_SEC,
            Some(value) => match value.as_u64() {
                Some(n) if n <= INTERVAL_MAX_SEC => n,
                _ => return Err(format!("sync.minIntervalSec must be an integer in 0..={INTERVAL_MAX_SEC}")),
            },
        };
        Ok(SyncConfig { agents, lead, transcripts, transcript_dirs, auto, min_interval_sec })
    }

    /// The block as the admin API shows it: defaults elided, one agent as its name.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        match &self.agents {
            Choice::Inherit => {}
            Choice::Named(names) if names.len() == 1 => {
                out.insert("agent".into(), json!(names[0]));
            }
            agents => {
                out.insert("agent".into(), agents.to_json(false));
            }
        }
        if !self.lead.is_inherit() {
            out.insert("lead".into(), self.lead.to_json(true));
        }
        out.insert("transcripts".into(), json!(if self.transcripts { "claude-code" } else { "off" }));
        if !self.transcript_dirs.is_empty() {
            out.insert(
                "transcriptDirs".into(),
                json!(self.transcript_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()),
            );
        }
        out.insert("auto".into(), json!(self.auto));
        if self.min_interval_sec != INTERVAL_DEFAULT_SEC {
            out.insert("minIntervalSec".into(), json!(self.min_interval_sec));
        }
        Value::Object(out)
    }

    /// Where conversations live: the block's dirs, else Claude Code's
    /// (`$CLAUDE_CONFIG_DIR/projects` when set, then `~/.claude/projects`) and DSH's
    /// (`$DSH_HOME/sessions` when set, then `~/.dsh/sessions`); the reader tells the
    /// two layouts apart by their directory names.
    pub fn transcript_dir_list(&self) -> Vec<PathBuf> {
        // `transcripts: off` reads no conversations at all: no digest, and the
        // panel's pending count stays at zero unread bytes.
        if !self.transcripts {
            return Vec::new();
        }
        if !self.transcript_dirs.is_empty() {
            return self.transcript_dirs.clone();
        }
        let mut out = Vec::new();
        for (var, home, sub) in [("CLAUDE_CONFIG_DIR", ".claude", "projects"), ("DSH_HOME", ".dsh", "sessions")] {
            if let Ok(config) = std::env::var(var) {
                if !config.trim().is_empty() {
                    out.push(Path::new(&config).join(sub));
                }
            }
            if let Some(dir) = home_dir() {
                out.push(dir.join(home).join(sub));
            }
        }
        out
    }
}

/// `~` at the start, else unchanged; never an error (an unexpandable `~` stays a
/// path the walk will simply not find).
fn shellexpand(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed == "~" {
        if let Some(home) = home_dir() {
            return Ok(home.display().to_string());
        }
    } else if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Some(home) = home_dir() {
            return Ok(home.join(rest).display().to_string());
        }
    }
    Ok(trimmed.to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// When a nudge may run: 30 s after the nudge, and never before the interval.
pub fn nudge_due(last_finished_ms: Option<i64>, min_interval_sec: u64, now: i64) -> i64 {
    let earliest = now + NUDGE_DELAY_MS;
    match last_finished_ms {
        Some(finished) => finished + (min_interval_sec as i64) * 1000,
        None => earliest,
    }
    .max(earliest)
}

/// The enqueue request a manual run sends (due now).
pub fn manual_enqueue(dry_run: bool, since: Option<String>) -> Enqueue {
    Enqueue { trigger: Trigger::Manual, dry_run, since, due_ms: now_ms(), language: None, redraw: false, max_asks: None, agents: Choice::Inherit, lead: Choice::Inherit, changed: false }
}

/// The enqueue request a map run sends (due now).
pub fn map_enqueue(dry_run: bool, language: Option<String>, redraw: bool) -> Enqueue {
    Enqueue { trigger: Trigger::Map, dry_run, since: None, due_ms: now_ms(), language, redraw, max_asks: None, agents: Choice::Inherit, lead: Choice::Inherit, changed: false }
}

/// The enqueue request a full run sends (due now).
pub fn full_enqueue(dry_run: bool, language: Option<String>, redraw: bool, max_asks: Option<usize>) -> Enqueue {
    Enqueue { trigger: Trigger::Full, dry_run, since: None, due_ms: now_ms(), language, redraw, max_asks, agents: Choice::Inherit, lead: Choice::Inherit, changed: false }
}

/// The enqueue request a recheck sends (due now).
pub fn recheck_enqueue(dry_run: bool, language: Option<String>, max_asks: Option<usize>) -> Enqueue {
    Enqueue { trigger: Trigger::Recheck, dry_run, since: None, due_ms: now_ms(), language, redraw: false, max_asks, agents: Choice::Inherit, lead: Choice::Inherit, changed: false }
}

/// The enqueue request a review run sends (due now): the inbox handed to the workers, or
/// with `changed` its notes whose anchored files changed.
pub fn review_enqueue(dry_run: bool, language: Option<String>, max_asks: Option<usize>, changed: bool) -> Enqueue {
    Enqueue { trigger: Trigger::Review, dry_run, since: None, due_ms: now_ms(), language, redraw: false, max_asks, agents: Choice::Inherit, lead: Choice::Inherit, changed }
}

/// The enqueue request a docs run sends (due now).
pub fn docs_enqueue(dry_run: bool, language: Option<String>, max_asks: Option<usize>) -> Enqueue {
    Enqueue { trigger: Trigger::Docs, dry_run, since: None, due_ms: now_ms(), language, redraw: false, max_asks, agents: Choice::Inherit, lead: Choice::Inherit, changed: false }
}

/// The enqueue request a nudge sends: due per [nudge_due].
pub fn nudge_enqueue(last_finished_ms: Option<i64>, min_interval_sec: u64) -> Enqueue {
    Enqueue {
        trigger: Trigger::Nudge,
        dry_run: false,
        since: None,
        due_ms: nudge_due(last_finished_ms, min_interval_sec, now_ms()),
        language: None,
        redraw: false,
        max_asks: None,
        agents: Choice::Inherit,
        lead: Choice::Inherit,
        changed: false,
    }
}

/// The first queued run's due time, for [crate::memory_api]'s sync sheet.
pub fn wait_until(next: &Next) -> Option<i64> {
    match next {
        Next::Wait(due) => Some(*due),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
