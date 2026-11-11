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

//! Why a remote run ran (SPEC §remote.history "Provenance"). Three layers, each its own
//! evidence:
//!
//! - `why` - the reason the caller gave, one sentence, in the run's input;
//! - `origin` - where the caller ran: the coding agent, its session, its working directory and
//!   its conversation log, read by the CLI from the environment the agent gave its shell;
//! - the trail - the agent's own call that submitted the run, the person's request before it
//!   and what the agent said between, read from the agent's log after the run began.
//!
//! The trail needs nothing from the agent but the log it keeps anyway, written on this
//! machine a few seconds behind the call: Claude Code and DSH hand their shell commands a
//! session id, so their reader opens that one conversation; Codex and opencode do not, so
//! theirs looks through the conversations written around the run's start and matches the
//! call by the run's own words. No hook, no agent setting. This module owns its readers
//! ([logs]) and depends on no other module's notion of a conversation.

pub mod logs;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

/// The input fields every remote action accepts besides its own.
pub const FIELDS: [&str; 2] = ["why", "origin"];
/// The longest reason a run carries, in characters.
pub const WHY_MAX_CHARS: usize = 300;
/// The fields `origin` may carry, each a short text.
const ORIGIN_FIELDS: [&str; 5] = ["agent", "version", "session", "cwd", "log"];
const ORIGIN_FIELD_MAX: usize = 1024;
/// The agents whose logs this module reads.
pub const READABLE_AGENTS: [&str; 4] = ["claude-code", "dsh", "codex", "opencode"];
/// The agents whose shell carries no session id: their log is a folder or database of many
/// conversations, and the session is whichever one holds the call.
const SESSIONLESS_AGENTS: [&str; 2] = ["codex", "opencode"];

/// A recorded trail stays under this many bytes (its texts are clipped well before).
pub const TRAIL_MAX_BYTES: usize = 16 * 1024;

/// Refuse a malformed `why` or `origin` (the strict-input convention, SPEC §remote.actions).
pub fn validate(obj: &Map<String, Value>) -> Result<(), String> {
    if let Some(why) = obj.get("why") {
        let text = why.as_str().ok_or("input.why must be a string")?;
        if text.trim().is_empty() {
            return Err("input.why is empty: say in one sentence which request this run serves".into());
        }
        if text.chars().count() > WHY_MAX_CHARS {
            return Err(format!("input.why is longer than {WHY_MAX_CHARS} characters; one sentence is enough"));
        }
    }
    if let Some(origin) = obj.get("origin") {
        let fields = origin.as_object().ok_or("input.origin must be an object")?;
        for (key, value) in fields {
            if !ORIGIN_FIELDS.contains(&key.as_str()) {
                return Err(format!("input.origin.{key} is not a known field"));
            }
            let text = value.as_str().ok_or_else(|| format!("input.origin.{key} must be a string"))?;
            if text.len() > ORIGIN_FIELD_MAX {
                return Err(format!("input.origin.{key} is longer than {ORIGIN_FIELD_MAX} bytes"));
            }
        }
    }
    Ok(())
}

/// What one trail search asks of an agent's log.
#[derive(Debug, Clone)]
pub struct TrailAsk {
    pub agent: String,
    pub session: String,
    pub log: PathBuf,
    /// Words the submitting call's input should hold: the target, the command's words, the
    /// paths, the reason. The score is the share of them found.
    pub words: Vec<String>,
    /// The run's start, epoch ms: a call logged after it (and a little slack) is not this run's.
    pub before_ms: i64,
    /// Call ids earlier runs already took from this log.
    pub claimed: HashSet<String>,
}

/// The run's `origin` as a trail search, when the agent is one this module reads and the
/// log path has that agent's own shape - a run input never turns this into a reader of an
/// arbitrary file.
pub fn ask_of(input: &Value, before_ms: i64) -> Option<TrailAsk> {
    let origin = input.get("origin")?.as_object()?;
    let agent = origin.get("agent")?.as_str()?.to_string();
    if !READABLE_AGENTS.contains(&agent.as_str()) {
        return None;
    }
    let session = match origin.get("session").and_then(Value::as_str) {
        Some(session) => session.to_string(),
        None if SESSIONLESS_AGENTS.contains(&agent.as_str()) => String::new(),
        None => return None,
    };
    let log = expand_home(origin.get("log")?.as_str()?, &swiss_core::paths::home_dir());
    if !logs::shaped_like(&agent, &session, &log) {
        return None;
    }
    Some(TrailAsk {
        agent,
        session,
        log,
        words: words_of(input),
        before_ms,
        claimed: HashSet::new(),
    })
}

impl TrailAsk {
    /// The search as the database keeps it until the trail is filed
    /// ([crate::store::RemoteHistory::record_pending_trail]), so a restart can look again.
    /// The words go masked, as one text: a credential typed in the command never lands in
    /// the row, at the cost of that one word in the match.
    pub fn to_pending(&self) -> Value {
        json!({
            "agent": self.agent,
            "session": self.session,
            "log": self.log.to_string_lossy(),
            "words": mask_text(&self.words.join(" ")),
            "beforeMs": self.before_ms,
        })
    }

    /// A kept search read back, held to the rules of [ask_of]: a row in the database is no
    /// more a way to read an arbitrary file than a run input is.
    pub fn from_pending(kept: &Value) -> Option<TrailAsk> {
        let agent = kept.get("agent")?.as_str()?.to_string();
        if !READABLE_AGENTS.contains(&agent.as_str()) {
            return None;
        }
        let session = kept.get("session")?.as_str()?.to_string();
        if session.is_empty() && !SESSIONLESS_AGENTS.contains(&agent.as_str()) {
            return None;
        }
        let log = PathBuf::from(kept.get("log")?.as_str()?);
        if !logs::shaped_like(&agent, &session, &log) {
            return None;
        }
        Some(TrailAsk {
            agent,
            session,
            log,
            words: kept.get("words")?.as_str()?.split_whitespace().map(str::to_string).collect(),
            before_ms: kept.get("beforeMs")?.as_i64()?,
            claimed: HashSet::new(),
        })
    }
}

/// `~/x` (or `~\x`) under `home`; any other path as given.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// The most words a match weighs.
const MAX_WORDS: usize = 40;

/// The words a submitting call's text should contain: every text the run was given except
/// the env values, the stdin and a write's body (the CLI read those from elsewhere), split on
/// whitespace, one character words dropped.
pub fn words_of(input: &Value) -> Vec<String> {
    let mut texts: Vec<&str> = Vec::new();
    if let Value::Object(obj) = input {
        for (key, value) in obj {
            match key.as_str() {
                "env" | "stdin" | "content" | "origin" | "timeoutMs" | "verbose" | "delta" => {}
                _ => collect_texts(value, &mut texts),
            }
        }
    }
    let mut seen = HashSet::new();
    let mut words = Vec::new();
    for text in texts {
        for word in text.split_whitespace() {
            if word.chars().count() < 2 || !seen.insert(word.to_string()) {
                continue;
            }
            words.push(word.to_string());
            if words.len() == MAX_WORDS {
                return words;
            }
        }
    }
    words
}

fn collect_texts<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items.iter().for_each(|item| collect_texts(item, out)),
        _ => {}
    }
}

/// The reader the follower calls: the real one reads the agent's log ([logs::find]); tests
/// swap in their own.
pub type Finder = Arc<dyn Fn(&TrailAsk) -> Result<Option<Value>, String> + Send + Sync>;

/// How far behind the call each agent writes its log: Claude Code about three seconds
/// (measured 2026-11-11), DSH at the end of a batch.
const FIRST_LOOK: Duration = Duration::from_secs(2);
const EARLY_EVERY: Duration = Duration::from_secs(3);
const LATE_EVERY: Duration = Duration::from_secs(20);
const EARLY_FOR: Duration = Duration::from_secs(60);
/// After the run ended, how long the follower keeps looking.
const AFTER_END: Duration = Duration::from_secs(45);
/// Never follow past the longest a run may last.
const FOLLOW_AT_MOST: Duration = Duration::from_secs(25 * 60 * 60);
/// Call ids remembered per log.
const CLAIMED_PER_LOG: usize = 256;
/// Logs remembered at once.
const CLAIMED_LOGS: usize = 64;

/// When the follower looks: the real pace by default, a fast one in tests.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub first: Duration,
    pub early_every: Duration,
    pub late_every: Duration,
    pub early_for: Duration,
    pub after_end: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Pace {
            first: FIRST_LOOK,
            early_every: EARLY_EVERY,
            late_every: LATE_EVERY,
            early_for: EARLY_FOR,
            after_end: AFTER_END,
        }
    }
}

impl Pace {
    /// Every wait `step`, giving up three steps after the run ended (tests).
    pub fn every(step: Duration) -> Self {
        Pace { first: step, early_every: step, late_every: step, early_for: step, after_end: step * 3 }
    }
}

/// The call ids the trails took, so no two runs take one call.
#[derive(Default)]
struct Claimed {
    /// Taken since this start, per log.
    per_log: HashMap<PathBuf, VecDeque<String>>,
    /// Taken by the trails filed before this start (any log), read once from the database
    /// when the follower resumes ([Trails::resume]).
    before_start: HashSet<String>,
}

impl Claimed {
    fn of(&self, log: &Path) -> HashSet<String> {
        self.per_log.get(log).into_iter().flatten().chain(&self.before_start).cloned().collect()
    }
}

/// The trail follower: one background task per run that carries an agent origin.
pub struct Trails {
    finder: Mutex<Option<Finder>>,
    pace: Mutex<Pace>,
    claimed: Arc<Mutex<Claimed>>,
}

impl Default for Trails {
    fn default() -> Self {
        Trails {
            finder: Mutex::new(Some(Arc::new(logs::find))),
            pace: Mutex::new(Pace::default()),
            claimed: Arc::new(Mutex::new(Claimed::default())),
        }
    }
}

impl Trails {
    /// Replace the reader (tests), or turn following off with `None`.
    pub fn set_finder(&self, finder: Option<Finder>) {
        *self.finder.lock().unwrap_or_else(|e| e.into_inner()) = finder;
    }

    /// Change when the follower looks (tests).
    pub fn set_pace(&self, pace: Pace) {
        *self.pace.lock().unwrap_or_else(|e| e.into_inner()) = pace;
    }

    fn finder(&self) -> Option<Finder> {
        self.finder.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn pace(&self) -> Pace {
        *self.pace.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start following one run: look in its agent's log until the call that submitted it
    /// shows, then file the trail. A run without a readable origin is left alone.
    pub fn follow(&self, run_id: u64, input: &Value, store: Arc<crate::store::RemoteHistory>) {
        let Some(finder) = self.finder() else {
            return;
        };
        let Some(ask) = ask_of(input, swiss_core::util::now_ms() as i64) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(follow(run_id, ask, finder, self.pace(), store, self.claimed.clone(), false));
    }

    /// Pick up the searches a stopped gateway left unfinished (the database keeps each until
    /// its trail is filed). Every run of that gateway is over - runs die with it - so each
    /// search looks for [AFTER_END] more, then files what it found or that nothing matched.
    /// The call ids filed since [FOLLOW_AT_MOST] ago stay taken, for these searches and the
    /// new runs alike. With following off, the kept searches wait for a start that follows,
    /// or for the sweep.
    pub async fn resume(&self, store: Arc<crate::store::RemoteHistory>) {
        let Some(finder) = self.finder() else {
            return;
        };
        let since = swiss_core::util::now_ms() as i64 - FOLLOW_AT_MOST.as_millis() as i64;
        let earlier = store.claimed_calls(since).await;
        self.claimed.lock().unwrap_or_else(|e| e.into_inner()).before_start.extend(earlier);
        let kept = store.pending_trails().await;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        for (run_id, search) in kept {
            match TrailAsk::from_pending(&search) {
                Some(ask) => {
                    runtime.spawn(follow(run_id, ask, finder.clone(), self.pace(), store.clone(), self.claimed.clone(), true));
                }
                None => {
                    let text = |key: &str| search.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                    let trail = json!({
                        "found": false, "agent": text("agent"), "session": text("session"),
                        "reason": "the gateway restarted, and the search it kept for this run's call is not one it can read"
                    });
                    store.record_trail(run_id, &trail).await;
                }
            }
        }
    }
}

async fn follow(
    run_id: u64,
    mut ask: TrailAsk,
    finder: Finder,
    pace: Pace,
    store: Arc<crate::store::RemoteHistory>,
    claimed: Arc<Mutex<Claimed>>,
    resumed: bool,
) {
    if !resumed {
        store.record_pending_trail(run_id, &ask.to_pending()).await;
    }
    let began = Instant::now();
    // A run of a gateway that stopped is already over.
    let mut ended: Option<Instant> = resumed.then(Instant::now);
    let mut wait = pace.first;
    let mut last_error: Option<String>;
    loop {
        tokio::time::sleep(wait).await;
        ask.claimed = claimed.lock().unwrap_or_else(|e| e.into_inner()).of(&ask.log);
        let look = ask.clone();
        let reader = finder.clone();
        match tokio::task::spawn_blocking(move || reader(&look)).await {
            Ok(Ok(Some(trail))) => {
                if let Some(id) = trail.get("callId").and_then(Value::as_str) {
                    claim(&claimed, &ask.log, id);
                }
                store.record_trail(run_id, &trail).await;
                return;
            }
            Ok(Ok(None)) => last_error = None,
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => last_error = Some("the log reader stopped".into()),
        }
        if ended.is_none() && store.get(run_id).await.is_some() {
            ended = Some(Instant::now());
        }
        let done = ended.is_some_and(|at| at.elapsed() >= pace.after_end && began.elapsed() >= pace.early_for / 2);
        if done || began.elapsed() >= FOLLOW_AT_MOST {
            let reason = last_error.unwrap_or_else(|| "no call in the agent's log matched this run".into());
            store.record_trail(run_id, &unmatched(&ask, &reason)).await;
            return;
        }
        wait = if began.elapsed() < pace.early_for { pace.early_every } else { pace.late_every };
    }
}

fn claim(claimed: &Mutex<Claimed>, log: &Path, id: &str) {
    let mut claimed = claimed.lock().unwrap_or_else(|e| e.into_inner());
    let per_log = &mut claimed.per_log;
    if !per_log.contains_key(log) && per_log.len() >= CLAIMED_LOGS {
        per_log.clear();
    }
    let ids = per_log.entry(log.to_path_buf()).or_default();
    ids.push_back(id.to_string());
    while ids.len() > CLAIMED_PER_LOG {
        ids.pop_front();
    }
}

/// The trail of a run whose call never showed: said so, never left blank.
pub fn unmatched(ask: &TrailAsk, reason: &str) -> Value {
    json!({ "found": false, "agent": ask.agent, "session": ask.session, "reason": reason })
}

/// A trail as filed: masked again (a reader's output is never trusted to have done it) and
/// cut to [TRAIL_MAX_BYTES] by dropping the agent's earliest words first.
pub fn filed(trail: &Value) -> Value {
    let mut trail = masked(trail);
    while serde_json::to_string(&trail).map(|t| t.len()).unwrap_or(0) > TRAIL_MAX_BYTES {
        let Some(said) = trail.get_mut("said").and_then(Value::as_array_mut) else { break };
        if said.is_empty() {
            break;
        }
        said.remove(0);
    }
    trail
}

/// Every text with its credential literals and stored vault values masked.
fn masked(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(mask_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(masked).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), masked(v))).collect()),
        other => other.clone(),
    }
}

pub fn mask_text(text: &str) -> String {
    swiss_core::secure::secretstore::mask_stored(&swiss_core::secure::literal::mask(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn why_and_origin_are_checked_strictly() {
        let ok = json!({ "why": "check nginx for the 502 the user saw", "origin": { "agent": "dsh", "session": "s1" } });
        assert!(validate(ok.as_object().unwrap()).is_ok());
        for (bad, says) in [
            (json!({ "why": 3 }), "must be a string"),
            (json!({ "why": "   " }), "is empty"),
            (json!({ "why": "x".repeat(WHY_MAX_CHARS + 1) }), "longer than"),
            (json!({ "origin": "dsh" }), "must be an object"),
            (json!({ "origin": { "pid": "1" } }), "origin.pid is not a known field"),
            (json!({ "origin": { "agent": 1 } }), "must be a string"),
        ] {
            let err = validate(bad.as_object().unwrap()).expect_err("refused");
            assert!(err.contains(says), "{err}");
        }
    }

    #[test]
    fn the_words_skip_what_the_call_never_typed() {
        let input = json!({
            "target": "test", "argv": ["sh", "-c", "tail -n 50 /var/log/app.log"],
            "env": { "TOKEN": "zzz-secret" }, "stdin": "SELECT 1", "why": "find the 502",
            "origin": { "agent": "dsh", "session": "abc" }, "timeoutMs": 60000
        });
        let words = words_of(&input);
        for want in ["test", "sh", "-c", "tail", "-n", "50", "/var/log/app.log", "find", "the", "502"] {
            assert!(words.contains(&want.to_string()), "{want} in {words:?}");
        }
        for never in ["zzz-secret", "SELECT", "abc", "60000", "dsh"] {
            assert!(!words.contains(&never.to_string()), "{never} in {words:?}");
        }
    }

    #[test]
    fn only_a_readable_agent_with_its_own_log_shape_is_followed() {
        let home = std::env::temp_dir();
        let claude = json!({ "origin": {
            "agent": "claude-code", "session": "00000000-1111-4222-8333-444444444444",
            "log": "~/.claude/projects/C--src-app/00000000-1111-4222-8333-444444444444.jsonl"
        }});
        assert!(ask_of(&claude, 0).is_some());
        let elsewhere = json!({ "origin": {
            "agent": "claude-code", "session": "00000000-1111-4222-8333-444444444444",
            "log": "~/.ssh/id_ed25519"
        }});
        assert!(ask_of(&elsewhere, 0).is_none(), "a log path of another shape is never read");
        let unknown = json!({ "origin": { "agent": "cursor", "session": "x", "log": "~/a.jsonl" } });
        assert!(ask_of(&unknown, 0).is_none());
        let codex = json!({ "origin": { "agent": "codex", "log": "~/.codex/sessions" } });
        assert!(ask_of(&codex, 0).is_some_and(|ask| ask.session.is_empty()), "codex needs no session");
        let dsh = json!({ "origin": { "agent": "dsh", "log": "~/.dsh/sessions/x/session-1" } });
        assert!(ask_of(&dsh, 0).is_none(), "dsh does");
        assert_eq!(expand_home("~/a/b.jsonl", &home), home.join("a/b.jsonl"));
        assert_eq!(expand_home("C:/a.jsonl", &home), PathBuf::from("C:/a.jsonl"));
    }

    #[test]
    fn a_kept_search_reads_back_masked_and_only_with_its_own_log_shape() {
        let session = "00000000-1111-4222-8333-444444444444";
        let input = json!({ "target": "dev", "argv": ["redis-cli", "-a", "hunter2hunter2", "ping"], "origin": {
            "agent": "claude-code", "session": session,
            "log": format!("~/.claude/projects/C--src/{session}.jsonl")
        }});
        let ask = ask_of(&input, 42).expect("a readable origin");
        let kept = ask.to_pending();
        assert!(!kept.to_string().contains("hunter2hunter2"), "{kept}");
        let back = TrailAsk::from_pending(&kept).expect("reads back");
        assert_eq!((back.agent.as_str(), back.session.as_str(), back.before_ms), ("claude-code", session, 42));
        assert_eq!(back.log, ask.log);
        assert!(back.words.contains(&"ping".to_string()), "{:?}", back.words);
        let mut elsewhere = kept.clone();
        elsewhere["log"] = json!("C:/Users/x/.ssh/id_ed25519");
        assert!(TrailAsk::from_pending(&elsewhere).is_none(), "a kept row is never a way to read another file");
        let mut sessionless = kept.clone();
        sessionless["session"] = json!("");
        assert!(TrailAsk::from_pending(&sessionless).is_none(), "claude-code needs its session");
        assert!(TrailAsk::from_pending(&json!({ "agent": "cursor" })).is_none());
    }

    #[test]
    fn a_filed_trail_is_masked_and_bounded() {
        let said: Vec<Value> = (0..40).map(|i| json!({ "text": format!("{i} {}", "x".repeat(900)) })).collect();
        let trail = json!({
            "found": true, "call": "redis-cli -a hunter2hunter2 ping",
            "asked": { "text": "ping redis" }, "said": said
        });
        let filed = filed(&trail);
        let text = serde_json::to_string(&filed).unwrap();
        assert!(text.len() <= TRAIL_MAX_BYTES, "{}", text.len());
        assert!(!text.contains("hunter2hunter2"), "{text}");
        let kept = filed["said"].as_array().unwrap();
        assert!(kept.last().unwrap()["text"].as_str().unwrap().starts_with("39 "), "the latest words stay");
    }
}
