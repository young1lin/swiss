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

//! The agent log readers behind a trail (SPEC §remote.history "Provenance"). Each reader turns
//! one agent's own log into the same three events - the person asked, the agent said, the
//! agent called a tool - and one matcher picks the call that submitted the run:
//!
//! - Claude Code: `<config>/projects/<dir>/<session>.jsonl`, plus `<session>/subagents/*.jsonl`;
//! - DSH: `<DSH_HOME>/sessions/<dir>/session-<id>/session(.vN).jsonl(.zstd)`, zstd frames of
//!   whole lines;
//! - Codex: `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-*.jsonl`, the recent ones;
//! - opencode: its SQLite database (`part` rows joined to their `message`), read-only.
//!
//! Codex and opencode hand their shell no session id, so their reader looks at every
//! conversation written around the run's start; the match is by the run's own words.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use super::TrailAsk;

/// How much of a plain log's end is read.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;
/// How much of a compressed DSH log's end is read.
const ZSTD_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// A call logged this long after the run began belongs to a later run.
const AFTER_SLACK_MS: i64 = 10_000;
/// A call logged this long before the run began is too old to be its own.
const BEFORE_WINDOW_MS: i64 = 30 * 60 * 1000;
/// The least share of the run's words a call must hold.
const MIN_SCORE: f64 = 0.6;
const ASK_MAX_CHARS: usize = 2000;
const SAID_MAX_CHARS: usize = 600;
const SAID_KEPT: usize = 4;
const CALL_MAX_CHARS: usize = 2000;
/// The most Codex rollouts one look reads.
const CODEX_FILES: usize = 8;
/// The most opencode rows one look reads.
const OPENCODE_ROWS: i64 = 4000;

/// One thing a conversation log holds, in the order it was logged.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Asked { at: i64, text: String },
    Said { at: i64, text: String },
    Call { at: i64, id: String, tool: String, text: String },
}

/// One conversation's events and the session they belong to.
#[derive(Debug, Default)]
pub struct Stream {
    pub session: String,
    pub events: Vec<Event>,
}

/// Whether `log` has the shape the agent's own log has for that session - the guard that
/// keeps a run input from pointing the reader at any other file.
pub fn shaped_like(agent: &str, session: &str, log: &Path) -> bool {
    let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let grandparent = |p: &Path| p.parent().and_then(Path::parent).map(name).unwrap_or_default();
    match agent {
        "claude-code" => {
            uuid_like(session) && name(log) == format!("{session}.jsonl") && grandparent(log) == "projects"
        }
        "dsh" => {
            session.strip_prefix("session-").is_some_and(uuid_like)
                && name(log) == session
                && grandparent(log) == "sessions"
        }
        "codex" => (session.is_empty() || uuid_like(session)) && name(log) == "sessions",
        "opencode" => {
            let file = name(log);
            (session.is_empty() || plain_id(session))
                && file.ends_with(".db")
                && log.to_string_lossy().to_lowercase().contains("opencode")
        }
        _ => false,
    }
}

fn uuid_like(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn plain_id(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Read the agent's log and return the trail of the call that submitted the run, `None`
/// while no call matches yet.
pub fn find(ask: &TrailAsk) -> Result<Option<Value>, String> {
    let streams = match ask.agent.as_str() {
        "claude-code" => claude_streams(ask)?,
        "dsh" => dsh_streams(ask)?,
        "codex" => codex_streams(ask)?,
        "opencode" => return opencode_find(ask),
        other => return Err(format!("no log reader for {other}")),
    };
    Ok(pick(ask, &streams).map(|(s, i, score)| trail_of(ask, &streams[s], i, score)))
}

/// The best call for the run: unclaimed, inside the time window, naming swiss, holding the
/// most of the run's words (at least [MIN_SCORE] of them); the latest wins a tie.
pub fn pick(ask: &TrailAsk, streams: &[Stream]) -> Option<(usize, usize, f64)> {
    let mut best: Option<(usize, usize, f64, i64)> = None;
    for (s, stream) in streams.iter().enumerate() {
        for (i, event) in stream.events.iter().enumerate() {
            let Event::Call { at, id, tool, text } = event else { continue };
            if *at > ask.before_ms + AFTER_SLACK_MS || *at < ask.before_ms - BEFORE_WINDOW_MS || ask.claimed.contains(id) {
                continue;
            }
            let hay = format!("{tool} {text}");
            if !hay.to_lowercase().contains("swiss") {
                continue;
            }
            let score = score(&ask.words, &hay);
            if score < MIN_SCORE {
                continue;
            }
            let better = match best {
                None => true,
                Some((.., best_score, best_at)) => {
                    score > best_score + 1e-9 || ((score - best_score).abs() <= 1e-9 && *at >= best_at)
                }
            };
            if better {
                best = Some((s, i, score, *at));
            }
        }
    }
    best.map(|(s, i, score, _)| (s, i, score))
}

/// The share of `words` found in `hay`.
pub fn score(words: &[String], hay: &str) -> f64 {
    if words.is_empty() {
        return 0.0;
    }
    let found = words.iter().filter(|word| hay.contains(word.as_str())).count();
    found as f64 / words.len() as f64
}

/// The trail of the call at `index`: the person's last ask before it and the agent's last
/// few words between the two.
pub fn trail_of(ask: &TrailAsk, stream: &Stream, index: usize, score: f64) -> Value {
    let events = &stream.events;
    let Event::Call { at, id, tool, text } = &events[index] else {
        return Value::Null;
    };
    let asked_at = events[..index].iter().rposition(|e| matches!(e, Event::Asked { .. }));
    let asked = asked_at.map(|k| match &events[k] {
        Event::Asked { at, text } => json!({ "at": at, "text": clip(text, ASK_MAX_CHARS) }),
        _ => Value::Null,
    });
    let from = asked_at.map_or(0, |k| k + 1);
    let mut said: Vec<Value> = events[from..index]
        .iter()
        .filter_map(|e| match e {
            Event::Said { at, text } => Some(json!({ "at": at, "text": clip(text, SAID_MAX_CHARS) })),
            _ => None,
        })
        .collect();
    let calls_before = events[from..index].iter().filter(|e| matches!(e, Event::Call { .. })).count();
    if said.len() > SAID_KEPT {
        said.drain(..said.len() - SAID_KEPT);
    }
    json!({
        "found": true,
        "agent": ask.agent,
        "session": stream.session,
        "callId": id,
        "tool": tool,
        "at": at,
        "score": (score * 100.0).round() / 100.0,
        "asked": asked.unwrap_or(Value::Null),
        "said": said,
        "callsBefore": calls_before,
        "call": clip(text, CALL_MAX_CHARS),
    })
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// A person's or agent's text without what the harness wrapped around it: the
/// `<system-reminder>` and `<environment_context>` blocks go, the other tags become spaces.
pub fn clean(text: &str) -> String {
    let mut rest = text.to_string();
    for tag in ["system-reminder", "environment_context"] {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        while let Some(start) = rest.find(&open) {
            let end = rest[start..].find(&close).map_or(rest.len(), |e| start + e + close.len());
            rest.replace_range(start..end, " ");
        }
    }
    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let tag: String = chars.clone().take_while(|&c| c != '>').take(41).collect();
            let closed = chars.clone().nth(tag.chars().count()) == Some('>');
            let name = tag.strip_prefix('/').unwrap_or(&tag);
            if closed && tag.len() <= 40 && !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '_') {
                (0..=tag.chars().count()).for_each(|_| {
                    chars.next();
                });
                out.push(' ');
                continue;
            }
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_asked(events: &mut Vec<Event>, at: i64, text: &str) {
    let text = clean(text);
    // A harness notice that arrives as the person's turn is not their request.
    if !text.is_empty() && !text.starts_with("task-notification") {
        events.push(Event::Asked { at, text });
    }
}

fn push_said(events: &mut Vec<Event>, at: i64, text: &str) {
    let text = clean(text);
    if !text.is_empty() {
        events.push(Event::Said { at, text });
    }
}

/// A tool input as the text it ran: the shell command when there is one, else the JSON.
fn call_text(input: &Value) -> String {
    let input = match input {
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(parsed @ Value::Object(_)) => parsed,
            _ => return raw.clone(),
        },
        other => other.clone(),
    };
    match input.get("command") {
        Some(Value::String(command)) => command.clone(),
        Some(Value::Array(argv)) => {
            let argv: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
            shell_script(&argv).map_or_else(|| argv.join(" "), str::to_string)
        }
        _ => input.to_string(),
    }
}

/// The script a shell was handed, when an argv is only a shell, its switches and that script:
/// Codex logs `[<path>\powershell.exe, -Command, <script>]` (or `bash -lc <script>`), and the
/// shell's path is most of a one-line call while saying nothing about what ran (2026-11-11).
fn shell_script<'a>(argv: &[&'a str]) -> Option<&'a str> {
    let [shell, switches @ .., flag, script] = argv else { return None };
    let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let is_shell = ["bash", "sh", "zsh", "dash", "fish", "powershell", "pwsh", "cmd"].contains(&name);
    let runs_script = ["-c", "-lc", "-ic", "-command", "/c"].contains(&flag.to_ascii_lowercase().as_str());
    let only_switches = switches.iter().all(|s| s.starts_with('-') || s.starts_with('/'));
    (is_shell && runs_script && only_switches).then_some(*script)
}

fn texts_of(blocks: &Value, kind: &str) -> String {
    match blocks {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|b| b["type"] == kind)
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The last `max` bytes of a file as text, starting at a line.
fn read_tail(path: &Path, max: u64) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("the agent's log cannot be read: {}", e.kind()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        let cut = text.find('\n').map_or(text.len(), |n| n + 1);
        text.drain(..cut);
    }
    Ok(text)
}

fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// The files in `dir` whose name passes `keep`, modified at or after `since_ms`, newest first.
fn recent_files(dir: &Path, keep: impl Fn(&str) -> bool, since_ms: i64) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.file_name().to_str().is_some_and(&keep))
        .map(|e| (modified_ms(&e.path()), e.path()))
        .filter(|(at, _)| *at >= since_ms)
        .collect();
    files.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    files.into_iter().map(|(_, path)| path).collect()
}

/// The `n` sub-directories of `dir` with the greatest names.
fn newest_dirs(dir: &Path, n: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> =
        entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
    dirs.sort();
    dirs.into_iter().rev().take(n).collect()
}

fn iso_ms(value: &Value) -> i64 {
    value.as_str().and_then(swiss_core::util::parse_iso_ms).unwrap_or(0)
}

// --- Claude Code ----------------------------------------------------------------------------

fn claude_streams(ask: &TrailAsk) -> Result<Vec<Stream>, String> {
    let mut streams =
        vec![Stream { session: ask.session.clone(), events: claude_events(&read_tail(&ask.log, TAIL_BYTES)?) }];
    // A subagent's calls are logged beside the main conversation.
    let subagents = ask.log.with_extension("").join("subagents");
    for path in recent_files(&subagents, |n| n.ends_with(".jsonl"), ask.before_ms - BEFORE_WINDOW_MS) {
        if let Ok(text) = read_tail(&path, TAIL_BYTES) {
            streams.push(Stream { session: ask.session.clone(), events: claude_events(&text) });
        }
    }
    Ok(streams)
}

pub fn claude_events(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        if ["isMeta", "isCompactSummary", "isVisibleInTranscriptOnly"].iter().any(|flag| entry[flag] == true) {
            continue;
        }
        let at = iso_ms(&entry["timestamp"]);
        let content = &entry["message"]["content"];
        match entry["type"].as_str() {
            Some("user") => push_asked(&mut events, at, &texts_of(content, "text")),
            // What the person typed while the agent was mid-turn is logged as a queued
            // command, never as a user line (seen 2026-11-11); a harness-queued one is not theirs.
            Some("attachment") => {
                let queued = &entry["attachment"];
                if queued["type"] == "queued_command"
                    && (queued["origin"]["kind"] == "human" || queued["humanTurn"] == true)
                {
                    push_asked(&mut events, at, &texts_of(&queued["prompt"], "text"));
                }
            }
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => push_said(&mut events, at, block["text"].as_str().unwrap_or_default()),
                        Some("tool_use") => events.push(Event::Call {
                            at,
                            id: block["id"].as_str().unwrap_or_default().to_string(),
                            tool: block["name"].as_str().unwrap_or_default().to_string(),
                            text: call_text(&block["input"]),
                        }),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    events
}

// --- DSH ------------------------------------------------------------------------------------

fn dsh_streams(ask: &TrailAsk) -> Result<Vec<Stream>, String> {
    let file = recent_files(&ask.log, |n| n.starts_with("session") && n.contains(".jsonl"), 0)
        .into_iter()
        .next()
        .ok_or("the agent's log cannot be read: no session file")?;
    let compressed = file.extension().is_some_and(|e| e == "zstd" || e == "zst");
    let text = if compressed { read_zstd_tail(&file)? } else { read_tail(&file, TAIL_BYTES)? };
    Ok(vec![Stream { session: ask.session.clone(), events: dsh_events(&text) }])
}

/// The decoded end of a log written as concatenated zstd frames, each a batch of whole lines.
/// Read from the middle, the first frame is found by its magic number and kept only when the
/// frames from there walk cleanly to the end; a frame still being written is left out.
pub fn read_zstd_tail(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("the agent's log cannot be read: {}", e.kind()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(ZSTD_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    let starts: Vec<usize> = if start == 0 {
        vec![0]
    } else {
        bytes.windows(4).enumerate().filter(|(_, w)| *w == MAGIC).map(|(i, _)| i).collect()
    };
    let last_magic = bytes.windows(4).rposition(|w| w == MAGIC).unwrap_or(0);
    for first in starts {
        let mut pos = first;
        let mut plain = Vec::new();
        let mut clean_walk = true;
        while pos < bytes.len() {
            let Ok(size) = zstd::zstd_safe::find_frame_compressed_size(&bytes[pos..]) else {
                // Only the frame being written may be cut short.
                clean_walk = pos == last_magic;
                break;
            };
            match zstd::stream::decode_all(&bytes[pos..pos + size]) {
                Ok(frame) => plain.extend_from_slice(&frame),
                Err(_) => {
                    clean_walk = false;
                    break;
                }
            }
            pos += size;
        }
        if clean_walk && (!plain.is_empty() || first == 0) {
            return Ok(String::from_utf8_lossy(&plain).into_owned());
        }
    }
    Err("the agent's log could not be decoded".into())
}

pub fn dsh_events(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let at = entry["time"].as_i64().unwrap_or(0);
        let data = &entry["data"];
        match entry["type"].as_str() {
            Some("user/message") if data["source"]["kind"] == "user" => {
                push_asked(&mut events, at, &texts_of(&data["content"], "text"))
            }
            Some("agent/inbox/spliced") => {
                for message in data["inserted"].as_array().into_iter().flatten() {
                    if message["role"] == "user" && message["source"]["kind"] == "user" {
                        push_asked(&mut events, at, &texts_of(&message["content"], "text"));
                    }
                }
            }
            Some("assistant/message") => push_said(&mut events, at, &texts_of(&data["message"]["content"], "text")),
            Some("tool/call") => events.push(Event::Call {
                at,
                id: data["callId"].as_str().unwrap_or_default().to_string(),
                tool: data["name"].as_str().unwrap_or_default().to_string(),
                text: call_text(&data["arguments"]),
            }),
            // A call a programmatic tool script made from inside one tool call.
            Some("tool/ptc-dispatch-start") => events.push(Event::Call {
                at,
                id: data["subCallId"].as_str().unwrap_or_default().to_string(),
                tool: data["name"].as_str().unwrap_or_default().to_string(),
                text: call_text(&data["arguments"]),
            }),
            _ => {}
        }
    }
    events
}

// --- Codex ----------------------------------------------------------------------------------

fn codex_streams(ask: &TrailAsk) -> Result<Vec<Stream>, String> {
    if !ask.log.is_dir() {
        return Err("the agent's log cannot be read: no sessions folder".into());
    }
    let since = ask.before_ms - BEFORE_WINDOW_MS;
    let keep = |name: &str| {
        name.starts_with("rollout-")
            && name.ends_with(".jsonl")
            && (ask.session.is_empty() || name.ends_with(&format!("{}.jsonl", ask.session)))
    };
    // Rollouts live under YYYY/MM/DD of the day the conversation BEGAN: only the newest days
    // are walked, and with a session id a whole month of them - a conversation kept open for
    // weeks still writes to the folder of its first day, and the name narrows the walk anyway.
    let days = if ask.session.is_empty() { 3 } else { 31 };
    let mut files: Vec<PathBuf> = Vec::new();
    for year in newest_dirs(&ask.log, 2) {
        for month in newest_dirs(&year, 2) {
            for day in newest_dirs(&month, days) {
                files.extend(recent_files(&day, keep, since));
            }
        }
    }
    files.sort_by_key(|path| std::cmp::Reverse(modified_ms(path)));
    files.truncate(CODEX_FILES);
    Ok(files
        .iter()
        .filter_map(|path| {
            let text = read_tail(path, TAIL_BYTES).ok()?;
            let name = path.file_stem()?.to_str()?;
            let session = name.get(name.len().saturating_sub(36)..).filter(|s| uuid_like(s)).unwrap_or_default();
            Some(Stream { session: session.to_string(), events: codex_events(&text) })
        })
        .collect())
}

pub fn codex_events(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let at = iso_ms(&entry["timestamp"]);
        let payload = &entry["payload"];
        match (entry["type"].as_str(), payload["type"].as_str()) {
            (Some("response_item"), Some("message")) => match payload["role"].as_str() {
                Some("user") => {
                    // Codex sends its AGENTS.md and environment as user turns too: only the
                    // items it marks as the person's own text are their ask.
                    let kinds = payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"].as_array();
                    let text = payload["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .filter(|(k, item)| match kinds {
                            Some(kinds) => kinds.get(*k).and_then(Value::as_str) == Some("user.text"),
                            None => !item["text"].as_str().unwrap_or_default().trim_start().starts_with('<'),
                        })
                        .filter_map(|(_, item)| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    push_asked(&mut events, at, &text);
                }
                Some("assistant") => push_said(&mut events, at, &texts_of(&payload["content"], "output_text")),
                _ => {}
            },
            (Some("response_item"), Some("function_call")) => events.push(Event::Call {
                at,
                id: payload["call_id"].as_str().unwrap_or_default().to_string(),
                tool: payload["name"].as_str().unwrap_or_default().to_string(),
                text: call_text(&payload["arguments"]),
            }),
            (Some("response_item"), Some("custom_tool_call")) => events.push(Event::Call {
                at,
                id: payload["call_id"].as_str().unwrap_or_default().to_string(),
                tool: payload["name"].as_str().unwrap_or_default().to_string(),
                text: payload["input"].as_str().unwrap_or_default().to_string(),
            }),
            // The command itself, logged when it ended, stamped with when it started.
            (Some("event_msg"), Some("item_completed")) if payload["item"]["type"] == "CommandExecution" => {
                let item = &payload["item"];
                events.push(Event::Call {
                    at: payload["started_at_ms"].as_i64().unwrap_or(at),
                    id: item["id"].as_str().unwrap_or_default().to_string(),
                    tool: "shell".to_string(),
                    text: call_text(&json!({ "command": item["command"] })),
                });
            }
            _ => {}
        }
    }
    events
}

// --- opencode -------------------------------------------------------------------------------

fn opencode_find(ask: &TrailAsk) -> Result<Option<Value>, String> {
    let work = opencode_read(ask);
    match tokio::runtime::Handle::try_current() {
        // The follower calls readers on a blocking thread, where waiting on the runtime is allowed.
        Ok(handle) => handle.block_on(work),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(work),
    }
}

async fn opencode_read(ask: &TrailAsk) -> Result<Option<Value>, String> {
    use sqlx::{Connection, Row};
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(&ask.log).read_only(true);
    let mut conn = sqlx::SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| format!("the agent's database cannot be opened: {e}"))?;
    let rows = sqlx::query(
        "SELECT p.session_id, p.time_created, p.data, coalesce(json_extract(m.data, '$.role'), '') \
         FROM part p JOIN message m ON m.id = p.message_id \
         WHERE p.time_created >= ? AND p.time_created <= ? AND (?3 = '' OR p.session_id = ?3) \
         ORDER BY p.time_created LIMIT ?",
    )
    .bind(ask.before_ms - BEFORE_WINDOW_MS)
    .bind(ask.before_ms + AFTER_SLACK_MS)
    .bind(&ask.session)
    .bind(OPENCODE_ROWS)
    .fetch_all(&mut conn)
    .await
    .map_err(|e| format!("the agent's database cannot be read: {e}"))?;
    let mut streams: Vec<Stream> = Vec::new();
    for row in rows {
        let (Ok(session), Ok(at), Ok(data), Ok(role)) =
            (row.try_get::<String, _>(0), row.try_get::<i64, _>(1), row.try_get::<String, _>(2), row.try_get::<String, _>(3))
        else {
            continue;
        };
        let index = match streams.iter().position(|s| s.session == session) {
            Some(index) => index,
            None => {
                streams.push(Stream { session, events: Vec::new() });
                streams.len() - 1
            }
        };
        if let Ok(part) = serde_json::from_str::<Value>(&data) {
            opencode_event(&mut streams[index].events, at, &role, &part);
        }
    }
    let Some((s, i, score)) = pick(ask, &streams) else { return Ok(None) };
    let has_ask = streams[s].events[..i].iter().any(|e| matches!(e, Event::Asked { .. }));
    if !has_ask {
        // The person's request may be older than the window: fetch the session's last one.
        let call_at = match &streams[s].events[i] {
            Event::Call { at, .. } => *at,
            _ => ask.before_ms,
        };
        let earlier = sqlx::query(
            "SELECT p.time_created, p.data FROM part p JOIN message m ON m.id = p.message_id \
             WHERE p.session_id = ? AND p.time_created < ? AND json_extract(m.data, '$.role') = 'user' \
             AND json_extract(p.data, '$.type') = 'text' ORDER BY p.time_created DESC LIMIT 1",
        )
        .bind(&streams[s].session)
        .bind(call_at)
        .fetch_optional(&mut conn)
        .await
        .map_err(|e| format!("the agent's database cannot be read: {e}"))?;
        if let Some(row) = earlier {
            let mut found = Vec::new();
            if let (Ok(at), Ok(data)) = (row.try_get::<i64, _>(0), row.try_get::<String, _>(1)) {
                if let Ok(part) = serde_json::from_str::<Value>(&data) {
                    opencode_event(&mut found, at, "user", &part);
                }
            }
            let added = found.len();
            streams[s].events.splice(0..0, found);
            return Ok(Some(trail_of(ask, &streams[s], i + added, score)));
        }
    }
    Ok(Some(trail_of(ask, &streams[s], i, score)))
}

pub fn opencode_event(events: &mut Vec<Event>, at: i64, role: &str, part: &Value) {
    match part["type"].as_str() {
        Some("text") if part["synthetic"] != true => {
            let text = part["text"].as_str().unwrap_or_default();
            match role {
                "user" => push_asked(events, at, text),
                "assistant" => push_said(events, at, text),
                _ => {}
            }
        }
        Some("tool") => events.push(Event::Call {
            at: part["state"]["time"]["start"].as_i64().unwrap_or(at),
            id: part["callID"].as_str().unwrap_or_default().to_string(),
            tool: part["tool"].as_str().unwrap_or_default().to_string(),
            text: call_text(&part["state"]["input"]),
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn ask(agent: &str, words: &[&str], before_ms: i64) -> TrailAsk {
        TrailAsk {
            agent: agent.into(),
            session: String::new(),
            log: PathBuf::new(),
            words: words.iter().map(|w| w.to_string()).collect(),
            before_ms,
            claimed: HashSet::new(),
        }
    }

    #[test]
    fn each_agent_log_has_its_own_shape() {
        let id = "00000000-1111-4222-8333-444444444444";
        let claude = PathBuf::from(format!("/h/.claude/projects/C--src/{id}.jsonl"));
        assert!(shaped_like("claude-code", id, &claude));
        assert!(!shaped_like("claude-code", id, Path::new("/h/.ssh/id_ed25519")));
        let dsh = PathBuf::from(format!("/h/.dsh/sessions/--C-dev--/session-{id}"));
        assert!(shaped_like("dsh", &format!("session-{id}"), &dsh));
        assert!(!shaped_like("dsh", id, &dsh), "the DSH session id carries its prefix");
        assert!(shaped_like("codex", "", Path::new("/h/.codex/sessions")));
        assert!(!shaped_like("codex", "", Path::new("/h/.codex")));
        assert!(shaped_like("opencode", "", Path::new("/h/.local/share/opencode/opencode.db")));
        assert!(!shaped_like("opencode", "", Path::new("/h/app/data.db")));
        assert!(!shaped_like("cursor", id, &claude));
    }

    #[test]
    fn the_harness_wrapping_is_cleaned_off() {
        let text = "<system-reminder>\nsecret context\n</system-reminder>check <command-args>test</command-args> nginx";
        assert_eq!(clean(text), "check test nginx");
        assert_eq!(clean("a <b> c"), "a c");
        assert_eq!(clean("x < y and 3<4"), "x < y and 3<4");
    }

    #[test]
    fn the_matching_call_is_the_latest_unclaimed_one_holding_the_words() {
        let line = |ts: &str, ty: &str, content: Value| {
            json!({ "type": ty, "timestamp": ts, "message": { "content": content } }).to_string()
        };
        let log = [
            line("2026-11-11T01:00:00Z", "user", json!("nginx on test returns 502, check it")),
            line("2026-11-11T01:00:01Z", "assistant", json!([{ "type": "text", "text": "Looking at the error log." }])),
            line("2026-11-11T01:00:02Z", "assistant", json!([{ "type": "tool_use", "id": "t1", "name": "Bash",
                "input": { "command": "swiss remote exec test --why 'the 502' -- tail -n 50 /var/log/nginx/error.log" } }])),
            line("2026-11-11T01:00:05Z", "assistant", json!([{ "type": "tool_use", "id": "t2", "name": "Bash",
                "input": { "command": "swiss remote exec test --why 'the 502' -- tail -n 50 /var/log/nginx/error.log" } }])),
            line("2026-11-11T01:00:09Z", "assistant", json!([{ "type": "tool_use", "id": "t3", "name": "Bash",
                "input": { "command": "tail -n 50 /var/log/nginx/error.log" } }])),
            json!({ "type": "user", "isMeta": true, "timestamp": "2026-11-11T01:00:06Z",
                "message": { "content": "skill body" } }).to_string(),
        ]
        .join("\n");
        let events = claude_events(&log);
        assert!(!events.iter().any(|e| matches!(e, Event::Asked { text, .. } if text == "skill body")));
        let run_at = swiss_core::util::parse_iso_ms("2026-11-11T01:00:06Z").unwrap();
        let stream = Stream { session: "s".into(), events };
        let words = ["test", "tail", "-n", "50", "/var/log/nginx/error.log", "the", "502"];
        let mut a = ask("claude-code", &words, run_at);
        let (s, i, score) = pick(&a, std::slice::from_ref(&stream)).expect("matched");
        let trail = trail_of(&a, &stream, i, score);
        assert_eq!(s, 0);
        assert_eq!(trail["callId"], "t2", "the latest call before the run; t3 does not name swiss");
        assert_eq!(trail["asked"]["text"], "nginx on test returns 502, check it");
        assert_eq!(trail["said"][0]["text"], "Looking at the error log.");
        assert_eq!(trail["callsBefore"], 1);
        a.claimed.insert("t2".into());
        let (_, i, _) = pick(&a, std::slice::from_ref(&stream)).expect("the earlier call");
        assert!(matches!(&stream.events[i], Event::Call { id, .. } if id == "t1"));
        a.claimed.insert("t1".into());
        assert!(pick(&a, std::slice::from_ref(&stream)).is_none());
        let early = ask("claude-code", &words, run_at - 3_600_000);
        assert!(pick(&early, std::slice::from_ref(&stream)).is_none(), "a call after the run began is not its own");
        let other = ask("claude-code", &["prod", "systemctl", "restart", "redis"], run_at);
        assert!(pick(&other, std::slice::from_ref(&stream)).is_none(), "too few words in common");
    }

    #[test]
    fn a_message_typed_mid_turn_is_the_latest_ask() {
        // The shape Claude Code 2.1 logs a message queued while it works (2026-11-11).
        let queued = |ts: &str, kind: &str, human: bool, prompt: &str| {
            json!({ "type": "attachment", "timestamp": ts, "attachment": { "type": "queued_command",
                "prompt": prompt, "commandMode": "prompt", "origin": { "kind": kind }, "humanTurn": human } })
            .to_string()
        };
        let log = [
            json!({ "type": "user", "timestamp": "2026-11-11T01:00:00Z", "message": { "content": "slim the skill" } })
                .to_string(),
            queued("2026-11-11T01:05:00Z", "human", true, "then test it and quick-deploy"),
            queued("2026-11-11T01:06:00Z", "task-notification", false, "a background job ended"),
            json!({ "type": "queue-operation", "operation": "enqueue", "timestamp": "2026-11-11T01:05:00Z",
                "content": "then test it and quick-deploy" })
            .to_string(),
        ]
        .join("\n");
        let asks: Vec<String> = claude_events(&log)
            .into_iter()
            .filter_map(|e| match e {
                Event::Asked { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(asks, ["slim the skill", "then test it and quick-deploy"]);
    }

    #[test]
    fn dsh_events_come_from_its_own_event_names() {
        let lines = [
            json!({ "type": "user/message", "time": 10, "data": { "content": [{ "type": "text", "text": "restart the api" }], "source": { "kind": "user" } } }),
            json!({ "type": "user/message", "time": 11, "data": { "content": [{ "type": "text", "text": "injected" }], "source": { "kind": "system" } } }),
            json!({ "type": "assistant/message", "time": 12, "data": { "message": { "content": [{ "type": "text", "text": "On it." }] } } }),
            json!({ "type": "tool/call", "time": 13, "data": { "callId": "c1", "name": "bash", "arguments": "{\"command\":\"swiss remote exec test -- systemctl restart api\"}" } }),
        ];
        let text = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let events = dsh_events(&text);
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(&events[2], Event::Call { text, .. } if text == "swiss remote exec test -- systemctl restart api"));
    }

    #[test]
    fn a_zstd_log_is_read_from_its_tail_frames() {
        let dir = std::env::temp_dir().join(format!("swiss-trail-zstd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.v2.jsonl.zstd");
        let mut bytes = Vec::new();
        for i in 0..3 {
            let lines = format!("{{\"type\":\"turn/start\",\"time\":{i}}}\n");
            bytes.extend(zstd::stream::encode_all(lines.as_bytes(), 3).unwrap());
        }
        bytes.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0x00]);
        std::fs::write(&path, &bytes).unwrap();
        let text = read_zstd_tail(&path).expect("decoded");
        assert_eq!(text.lines().count(), 3, "the frame being written is left out: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn codex_keeps_only_the_persons_own_text_and_the_command() {
        let lines = [
            json!({ "type": "response_item", "timestamp": "2026-11-11T01:00:00Z", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "# AGENTS.md instructions" }, { "type": "input_text", "text": "deploy to test" }],
                "internal_chat_message_metadata_passthrough": { "content_item_kinds": ["agents_md.instructions", "user.text"] } } }),
            json!({ "type": "event_msg", "timestamp": "2026-11-11T01:00:09Z", "payload": { "type": "item_completed", "started_at_ms": 5,
                "item": { "type": "CommandExecution", "id": "x1", "command": ["pwsh", "-Command", "swiss remote exec test -- uptime"] } } }),
        ];
        let text = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let events = codex_events(&text);
        assert!(matches!(&events[0], Event::Asked { text, .. } if text == "deploy to test"));
        assert!(matches!(&events[1], Event::Call { at: 5, text, .. } if text == "swiss remote exec test -- uptime"), "the script, not the shell");
    }

    #[test]
    fn a_shell_wrapper_gives_way_to_its_script() {
        let ps = r"C:\Program Files\WindowsApps\OpenAI.CodexPrimaryRuntime_x64\dependencies\native\powershell.exe";
        let text = |argv: Value| call_text(&json!({ "command": argv }));
        assert_eq!(text(json!([ps, "-Command", "swiss remote exec test -- pwd"])), "swiss remote exec test -- pwd");
        assert_eq!(text(json!(["/bin/bash", "-lc", "ls -la"])), "ls -la");
        assert_eq!(text(json!(["pwsh.exe", "-NoProfile", "-NonInteractive", "-Command", "Get-Date"])), "Get-Date");
        assert_eq!(text(json!(["cmd.exe", "/C", "dir"])), "dir");
        // Anything else stays the whole argv: a plain program, an argument between, no script flag.
        assert_eq!(text(json!(["swiss", "remote", "exec", "test", "-c", "x"])), "swiss remote exec test -c x");
        assert_eq!(text(json!(["bash", "deploy.sh", "-c", "x"])), "bash deploy.sh -c x");
        assert_eq!(text(json!(["bash", "-c"])), "bash -c");
    }

    #[test]
    fn opencode_parts_become_events() {
        let mut events = Vec::new();
        opencode_event(&mut events, 1, "user", &json!({ "type": "text", "text": "check disk on test" }));
        opencode_event(&mut events, 2, "user", &json!({ "type": "text", "text": "x", "synthetic": true }));
        opencode_event(&mut events, 3, "assistant", &json!({ "type": "tool", "tool": "bash", "callID": "k",
            "state": { "input": { "command": "swiss remote exec test -- df -h" }, "time": { "start": 4 } } }));
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], Event::Call { at: 4, id, .. } if id == "k"));
    }
}
