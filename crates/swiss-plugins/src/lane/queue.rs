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

//! The lane's queue (SPEC §lane.queue): who holds a slot, who waits, and what finished.
//!
//! Pure state behind one mutex - no I/O, no timers - so every rule is tested without a server.
//! Admission is first come, first served in arrival order: whenever the lane changes, the
//! oldest waiters take the free slots. A holder learns its turn through [Lane::subscribe], a
//! watch on the revision, and asks [Lane::seat] what the revision means for it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Map, Value};

/// How many finished commands the lane keeps for the page, newest first.
pub const RECENT_MAX: usize = 30;
/// A command line as the lane keeps it, at most this many characters.
pub const CMD_MAX: usize = 400;
/// A working directory as the lane keeps it, at most this many characters.
pub const CWD_MAX: usize = 1024;
/// The caller's agent name and session id, at most this many characters each.
const BY_MAX: usize = 64;
const SESSION_MAX: usize = 128;
/// The capacity a config may ask for.
pub const CAPACITY_MAX: u64 = 16;
/// How many caps a config may carry, and how long a name or a value may be.
const ENV_MAX: usize = 32;
const ENV_NAME_MAX: usize = 64;
const ENV_VALUE_MAX: usize = 256;

/// The plugin row (SPEC §lane.config).
#[derive(Clone, Debug, PartialEq)]
pub struct LaneConfig {
    /// How many commands may run at once. 1 is the point of the lane.
    pub capacity: usize,
    /// Variables a command gets when its caller's environment does not set them.
    pub env: Vec<(String, String)>,
}

impl Default for LaneConfig {
    fn default() -> Self {
        LaneConfig { capacity: 1, env: default_env() }
    }
}

impl LaneConfig {
    /// Read a row. The host owns `enabled`; an absent `env` means the defaults, an empty
    /// object means none.
    pub fn parse(config: &Value) -> Result<Self, String> {
        let empty = Map::new();
        let obj = match config {
            Value::Null => &empty,
            Value::Object(obj) => obj,
            _ => return Err("lane config must be an object".into()),
        };
        if let Some(key) = obj.keys().find(|k| !matches!(k.as_str(), "enabled" | "capacity" | "env")) {
            return Err(format!("lane config has no {key:?} field"));
        }
        let capacity = match obj.get("capacity") {
            None | Some(Value::Null) => 1,
            Some(value) => match value.as_u64() {
                Some(n) if (1..=CAPACITY_MAX).contains(&n) => n as usize,
                _ => return Err(format!("capacity must be an integer from 1 to {CAPACITY_MAX}")),
            },
        };
        let env = match obj.get("env") {
            None | Some(Value::Null) => default_env(),
            Some(Value::Object(map)) => {
                if map.len() > ENV_MAX {
                    return Err(format!("env holds at most {ENV_MAX} variables"));
                }
                let mut env = Vec::with_capacity(map.len());
                for (name, value) in map {
                    if !env_name_ok(name) {
                        return Err(format!("env name {name:?} must be letters, digits and _, not starting with a digit"));
                    }
                    let Some(value) = value.as_str() else {
                        return Err(format!("env.{name} must be a string"));
                    };
                    if value.chars().count() > ENV_VALUE_MAX || value.contains('\0') {
                        return Err(format!("env.{name} must be at most {ENV_VALUE_MAX} characters"));
                    }
                    env.push((name.clone(), value.to_string()));
                }
                env
            }
            Some(_) => return Err("env must be an object of NAME: \"value\" strings".into()),
        };
        Ok(LaneConfig { capacity, env })
    }
}

fn env_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    name.len() <= ENV_NAME_MAX
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The caps a command gets when neither its caller nor the row sets them (SPEC §lane.config).
/// rustc jobs are bound by memory, not cores - one job can take gigabytes - so cargo gets a
/// fixed 2. vitest workers follow the panel's own rule: half the cores, at least 2, at most 4.
pub fn default_env() -> Vec<(String, String)> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let workers = (cores / 2).clamp(2, 4);
    vec![
        ("CARGO_BUILD_JOBS".to_string(), "2".to_string()),
        ("VITEST_MAX_WORKERS".to_string(), workers.to_string()),
    ]
}

/// Who asks for a slot, as the caller describes itself. Every field is display only: the
/// lane never runs, signals or opens anything a ticket names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ticket {
    pub cmd: String,
    pub cwd: String,
    pub by: String,
    pub session: Option<String>,
    pub pid: Option<u32>,
}

impl Ticket {
    /// Read an enter body: `{cmd, cwd?, by?, session?, pid?, resume?}`. Long text is clipped,
    /// not refused: a slot matters more than a perfect label. Returns the ticket and `resume`.
    pub fn from_body(body: &Value) -> Result<(Ticket, bool), String> {
        let text = |key: &str, max: usize| -> Result<Option<String>, String> {
            match body.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => {
                    let s = s.trim();
                    Ok((!s.is_empty()).then(|| clip(s, max)))
                }
                Some(_) => Err(format!("{key} must be a string")),
            }
        };
        let cmd = text("cmd", CMD_MAX)?.ok_or_else(|| "cmd is required".to_string())?;
        let pid = match body.get("pid") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| "pid must be a process id".to_string())?,
            ),
        };
        let resume = match body.get("resume") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("resume must be true or false".into()),
        };
        let ticket = Ticket {
            cmd,
            cwd: text("cwd", CWD_MAX)?.unwrap_or_default(),
            by: text("by", BY_MAX)?.unwrap_or_else(|| "cli".to_string()),
            session: text("session", SESSION_MAX)?,
            pid,
        };
        Ok((ticket, resume))
    }
}

/// At most `max` characters, cut on a character boundary, with an ellipsis when cut.
pub fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => {
            let mut out = text[..cut].to_string();
            out.push('…');
            out
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    id: u64,
    ticket: Ticket,
    entered_at: u64,
    started_at: Option<u64>,
    /// Re-announced after a gateway restart while its command was already running: admitted
    /// at once, whatever the capacity, because the command is running either way.
    resumed: bool,
}

/// How a command left the lane.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Its caller reported the exit (`None`: no code, a signal took it).
    Exit(Option<i32>),
    /// Its caller's connection ended without a report: the agent was killed or gave up.
    Gone,
    /// The page released it: it runs on, no longer counted.
    Released,
}

#[derive(Clone, Debug)]
struct Finished {
    entry: Entry,
    ended_at: u64,
    outcome: Outcome,
}

/// What one holder's place in the lane is now.
#[derive(Clone, Debug, PartialEq)]
pub enum Seat {
    /// Not yet: `ahead` commands hold or wait for a slot before this one; `running` describes
    /// the ones holding a slot now.
    Waiting { ahead: usize, running: Vec<Value> },
    /// The slot is this command's: run, with these caps for what the caller left unset.
    Running { env: Vec<(String, String)> },
    /// The page released it: run (or keep running) without the lane.
    Released,
    /// The lane stopped (the plugin was disabled or the gateway is shutting down).
    Closed,
    /// The lane does not know the id: it finished, or never was.
    Unknown,
}

struct Inner {
    config: LaneConfig,
    next_id: u64,
    /// In arrival order; a resumed entry joins at the end like any other.
    entries: Vec<Entry>,
    /// Newest first.
    recent: VecDeque<Finished>,
    revision: u64,
    closed: bool,
}

/// The machine-wide lane. Shared by the routes and every holder stream.
pub struct Lane {
    inner: Mutex<Inner>,
    changed: tokio::sync::watch::Sender<u64>,
    notice: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    // A panic under the lock leaves plain data behind; the queue stays usable.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Lane {
    /// `notice` publishes LANE.QUEUE; it runs under the lock, so events leave in commit order.
    pub fn new(config: LaneConfig, notice: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (changed, _) = tokio::sync::watch::channel(0);
        Lane {
            inner: Mutex::new(Inner {
                config,
                next_id: 1,
                entries: Vec::new(),
                recent: VecDeque::new(),
                revision: 0,
                closed: false,
            }),
            changed,
            notice,
        }
    }

    /// A watch that moves on every change; a holder re-reads its [Seat] when it does.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Take a place. A `resume` (the caller was already running when the gateway restarted)
    /// is running at once; anyone else waits behind everyone already there.
    pub fn enter(&self, ticket: Ticket, resume: bool) -> u64 {
        let mut inner = lock(&self.inner);
        let id = inner.next_id;
        inner.next_id += 1;
        let now = swiss_core::util::now_ms();
        inner.entries.push(Entry {
            id,
            ticket,
            entered_at: now,
            started_at: resume.then_some(now),
            resumed: resume,
        });
        self.moved(&mut inner);
        id
    }

    /// What `id`'s place is now.
    pub fn seat(&self, id: u64) -> Seat {
        let inner = lock(&self.inner);
        if inner.closed {
            return Seat::Closed;
        }
        let Some(at) = inner.entries.iter().position(|e| e.id == id) else {
            let released = inner
                .recent
                .iter()
                .any(|f| f.entry.id == id && f.outcome == Outcome::Released);
            return if released { Seat::Released } else { Seat::Unknown };
        };
        if inner.entries[at].started_at.is_some() {
            return Seat::Running { env: inner.config.env.clone() };
        }
        let ahead = inner
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != at && (e.started_at.is_some() || *i < at))
            .count();
        let running = inner
            .entries
            .iter()
            .filter(|e| e.started_at.is_some())
            .map(|e| {
                json!({
                    "cmd": e.ticket.cmd,
                    "by": e.ticket.by,
                    "startedAt": e.started_at,
                })
            })
            .collect();
        Seat::Waiting { ahead, running }
    }

    /// The caller reports its command's end. `false`: the lane does not hold that id.
    pub fn finish(&self, id: u64, exit: Option<i32>) -> bool {
        self.remove(id, |entry| entry.started_at.is_some().then_some(Outcome::Exit(exit)))
    }

    /// The caller's connection ended. A running command that never reported is `Gone`; a
    /// waiter simply leaves.
    pub fn leave(&self, id: u64) {
        self.remove(id, |entry| entry.started_at.is_some().then_some(Outcome::Gone));
    }

    /// The page lets a command go: it stops counting, and the caller runs or keeps running
    /// without the lane. `false`: the lane does not hold that id.
    pub fn release(&self, id: u64) -> bool {
        self.remove(id, |_| Some(Outcome::Released))
    }

    /// Drop `id`; `record` says whether (and how) it goes into the recent list.
    fn remove(&self, id: u64, record: impl FnOnce(&Entry) -> Option<Outcome>) -> bool {
        let mut inner = lock(&self.inner);
        let Some(at) = inner.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let entry = inner.entries.remove(at);
        if let Some(outcome) = record(&entry) {
            inner.recent.push_front(Finished { entry, ended_at: swiss_core::util::now_ms(), outcome });
            inner.recent.truncate(RECENT_MAX);
        }
        self.moved(&mut inner);
        true
    }

    /// A new row, in place: the queue and every holder stay (SPEC §lane.config).
    pub fn apply(&self, config: LaneConfig) {
        let mut inner = lock(&self.inner);
        inner.config = config;
        self.moved(&mut inner);
    }

    /// The plugin stops: every holder hears `closed` and the queue empties.
    pub fn close(&self) {
        let mut inner = lock(&self.inner);
        inner.closed = true;
        inner.entries.clear();
        self.moved(&mut inner);
    }

    /// GET /api/lane.
    pub fn snapshot(&self) -> Value {
        let inner = lock(&self.inner);
        let env: Map<String, Value> = inner
            .config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let running: Vec<Value> =
            inner.entries.iter().filter(|e| e.started_at.is_some()).map(entry_json).collect();
        let waiting: Vec<Value> =
            inner.entries.iter().filter(|e| e.started_at.is_none()).map(entry_json).collect();
        let recent: Vec<Value> = inner.recent.iter().map(finished_json).collect();
        json!({
            "capacity": inner.config.capacity,
            "env": env,
            "revision": inner.revision,
            "running": running,
            "waiting": waiting,
            "recent": recent,
        })
    }

    /// Every change ends here: admit whoever the change made room for, move the revision,
    /// wake the holders, and tell the page.
    fn moved(&self, inner: &mut Inner) {
        if !inner.closed {
            let now = swiss_core::util::now_ms();
            let mut running = inner.entries.iter().filter(|e| e.started_at.is_some()).count();
            for entry in inner.entries.iter_mut() {
                if running >= inner.config.capacity {
                    break;
                }
                if entry.started_at.is_none() {
                    entry.started_at = Some(now);
                    running += 1;
                }
            }
        }
        inner.revision += 1;
        self.changed.send_replace(inner.revision);
        if let Some(notice) = &self.notice {
            notice();
        }
    }
}

fn entry_json(e: &Entry) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(e.id));
    o.insert("cmd".into(), json!(e.ticket.cmd));
    o.insert("cwd".into(), json!(e.ticket.cwd));
    o.insert("by".into(), json!(e.ticket.by));
    if let Some(session) = &e.ticket.session {
        o.insert("session".into(), json!(session));
    }
    if let Some(pid) = e.ticket.pid {
        o.insert("pid".into(), json!(pid));
    }
    o.insert("enteredAt".into(), json!(e.entered_at));
    if let Some(at) = e.started_at {
        o.insert("startedAt".into(), json!(at));
    }
    if e.resumed {
        o.insert("resumed".into(), json!(true));
    }
    Value::Object(o)
}

fn finished_json(f: &Finished) -> Value {
    let mut v = entry_json(&f.entry);
    if let Value::Object(o) = &mut v {
        o.insert("endedAt".into(), json!(f.ended_at));
        let outcome = match &f.outcome {
            Outcome::Exit(code) => {
                if let Some(code) = code {
                    o.insert("exit".into(), json!(code));
                }
                "exit"
            }
            Outcome::Gone => "gone",
            Outcome::Released => "released",
        };
        o.insert("outcome".into(), json!(outcome));
    }
    v
}
