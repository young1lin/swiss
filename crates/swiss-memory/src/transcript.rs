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

//! Coding-agent conversations as sync input (SPEC §memory.sync): where a project's
//! transcripts live - Claude Code's and DSH's, each recognised by its layout - what of
//! them is new since the cursors, and the redacted digest the runner reads. Only the
//! text people and the agent wrote is kept - never tool calls, tool results, reasoning,
//! reminders, injected context or command wrappers - and every credential shape is
//! replaced before the digest leaves this module. Blocking file reads: callers run it
//! on the blocking pool.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::secrets;

/// The first words of a sync worker's prompt: a session opening with them is skipped.
pub const MARKER: &str = "[swiss memory sync]";
/// One message's cap, in characters.
pub const MESSAGE_CHARS: usize = 2000;
/// The digest's cap, in bytes.
pub const DIGEST_BYTES: usize = 80 * 1024;
/// The cursor of a skipped session (a sync worker's own, or a DSH session that is a
/// subagent's or ran outside the roots): never read again.
pub const SKIPPED: i64 = -1;
/// How much of a DSH log's start is read for its header line.
const HEAD_BYTES: u64 = 64 * 1024;

/// User texts that are tool or harness output, not something a person wrote.
const DROPPED: [&str; 9] = [
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "task-notification",
    "subagent-message",
    "user-memory-input",
];

/// Claude Code's project directory name for a working directory: every character
/// outside `[A-Za-z0-9]` becomes `-`, one per UTF-16 unit as its JavaScript counts.
pub fn munge(dir: &str) -> String {
    let mut out = String::with_capacity(dir.len());
    for c in dir.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    out
}

/// Whether `cwd` is `root` or lies inside it (separators and a verbatim prefix
/// ignored; case-insensitive on Windows).
pub fn within(cwd: &str, root: &str) -> bool {
    fn norm(path: &str) -> String {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        path.replace('\\', "/").trim_end_matches('/').to_string()
    }
    let (cwd, root) = (norm(cwd.trim()), norm(root.trim()));
    if cwd.len() < root.len() || !cwd.is_char_boundary(root.len()) {
        return false;
    }
    let head = &cwd[..root.len()];
    let same = if cfg!(windows) { head.eq_ignore_ascii_case(&root) } else { head == root };
    same && (cwd.len() == root.len() || cwd.as_bytes()[root.len()] == b'/')
}

/// DSH's project directory name for a working directory (its `projectKey`): path
/// separators and the drive colon become one `-` a run, every other code unit outside
/// `[A-Za-z0-9._-]` (and `~`) becomes `~XXXX`, leading dashes go, the slug is cut at
/// 251 units and wrapped in `--`.
pub fn dsh_key(dir: &str) -> String {
    let mut readable = String::with_capacity(dir.len());
    let mut run = false;
    for c in dir.chars() {
        if matches!(c, '/' | '\\' | ':') {
            if !run {
                readable.push('-');
            }
            run = true;
            continue;
        }
        run = false;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            readable.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                readable.push_str(&format!("~{unit:04X}"));
            }
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    // Every unit is ASCII by now, so a byte cut is a unit cut.
    format!("--{}--", &slug[..slug.len().min(251)])
}

/// Which tool wrote a transcript file, recognised by where it was found; it decides
/// how the file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Claude Code: `<dir>/<munged cwd>/<session>.jsonl`, every line an event that
    /// carries its `cwd`.
    ClaudeCode,
    /// DSH: `<dir>/<project key>/<session>/session[.vN].jsonl`, the first line a header
    /// naming the session's `cwd`; with `zstd` the file is `….jsonl.zstd`, the same
    /// lines in independent zstd frames, one per batch appended.
    Dsh { zstd: bool },
}

/// One transcript file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    /// The cursor key: the file's full path.
    pub key: String,
    pub len: u64,
    pub modified_ms: i64,
    pub layout: Layout,
}

/// The transcript files of a project with these roots, least recently written first.
/// A directory under one of `dirs` is read by its name: DSH's `--<key>--` for a root
/// or a subdirectory of one (`--<key>-…--`) holds a directory per session, each read
/// at its newest log generation; Claude Code's `<munged root>` and its siblings
/// `<munged root>-…` hold `*.jsonl` directly (`subagents/` sits one level deeper and
/// is never listed). A sibling's lines - a DSH session by its header - count only
/// when their `cwd` lies in a root.
pub fn sources(dirs: &[PathBuf], roots: &[String]) -> Vec<Source> {
    let bare: Vec<&str> = roots.iter().map(|r| strip_verbatim(r).trim_end_matches(['/', '\\'])).filter(|r| !r.is_empty()).collect();
    let names: Vec<String> = bare.iter().map(|r| munge(r)).collect();
    let keys: Vec<String> = bare.iter().map(|r| dsh_key(r)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if keys.iter().any(|k| dsh_owned(&name, k)) {
                let Ok(sessions) = std::fs::read_dir(entry.path()) else { continue };
                for session in sessions.flatten() {
                    if let Some((file, zstd)) = dsh_log(&session.path()) {
                        listed(file, Layout::Dsh { zstd }, &mut seen, &mut out);
                    }
                }
            } else if names.iter().any(|n| owned_name(&name, n)) {
                let Ok(files) = std::fs::read_dir(entry.path()) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        listed(path, Layout::ClaudeCode, &mut seen, &mut out);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.modified_ms.cmp(&b.modified_ms).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Add one regular file, once.
fn listed(path: PathBuf, layout: Layout, seen: &mut HashSet<String>, out: &mut Vec<Source>) {
    let Ok(meta) = std::fs::metadata(&path) else { return };
    if !meta.is_file() {
        return;
    }
    let key = path.display().to_string();
    if !seen.insert(key.clone()) {
        return;
    }
    let modified_ms = modified_ms(&meta);
    out.push(Source { path, key, len: meta.len(), modified_ms, layout });
}

fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A DSH session directory's current log: the highest generation
/// (`session.jsonl` is 0, `session.vN.jsonl` N), the most recently written when both
/// encodings hold it; `None` for anything else.
fn dsh_log(dir: &Path) -> Option<(PathBuf, bool)> {
    let mut best: Option<(u64, i64, PathBuf, bool)> = None;
    for file in std::fs::read_dir(dir).ok()?.flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        let (plain, zstd) = match name.strip_suffix(".zstd") {
            Some(plain) => (plain.to_string(), true),
            None => (name.clone(), false),
        };
        let Some(version) = dsh_generation(&plain) else { continue };
        let Ok(meta) = file.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = modified_ms(&meta);
        if best.as_ref().is_none_or(|(v, m, _, _)| (version, modified) > (*v, *m)) {
            best = Some((version, modified, file.path(), zstd));
        }
    }
    best.map(|(_, _, path, zstd)| (path, zstd))
}

/// The generation a canonical DSH log name carries: `session.jsonl` 0,
/// `session.vN.jsonl` N (no leading zero, never `v0`).
fn dsh_generation(name: &str) -> Option<u64> {
    if name == "session.jsonl" {
        return Some(0);
    }
    let digits = name.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `name` is the DSH key itself or a subdirectory's (`--<slug>-…--`).
fn dsh_owned(name: &str, key: &str) -> bool {
    let stem = &key[..key.len() - 2];
    if name.len() < key.len() || !name.is_char_boundary(stem.len()) || !name.ends_with("--") {
        return false;
    }
    let head = &name[..stem.len()];
    let same = if cfg!(windows) { head.eq_ignore_ascii_case(stem) } else { head == stem };
    same && (name.len() == key.len() || name.as_bytes()[stem.len()] == b'-')
}

fn strip_verbatim(path: &str) -> &str {
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

/// `name` is the munged root itself or a sibling of it.
fn owned_name(name: &str, munged: &str) -> bool {
    if munged.is_empty() || name.len() < munged.len() || !name.is_char_boundary(munged.len()) {
        return false;
    }
    let head = &name[..munged.len()];
    let same = if cfg!(windows) { head.eq_ignore_ascii_case(munged) } else { head == munged };
    same && (name.len() == munged.len() || name.as_bytes()[munged.len()] == b'-')
}

/// Every file at its current size: the first sync's starting point.
pub fn baseline(sources: &[Source]) -> Vec<(String, i64)> {
    sources.iter().map(|s| (s.key.clone(), s.len as i64)).collect()
}

/// The bytes not read yet, summed (a skipped session counts nothing; a file that
/// shrank below its cursor counts whole).
pub fn pending_bytes(sources: &[Source], cursors: &HashMap<String, i64>) -> u64 {
    sources
        .iter()
        .map(|s| match cursors.get(&s.key).copied().unwrap_or(0) {
            SKIPPED => 0,
            at if at < 0 || at as u64 > s.len => s.len,
            at => s.len - at as u64,
        })
        .sum()
}

/// The conversations since the cursors, ready for the prompt.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Digest {
    /// The redacted digest; empty when nothing new was said.
    pub text: String,
    pub sessions: usize,
    pub messages: usize,
    /// Sessions with new text that did not fit and wait for the next run.
    pub waiting: usize,
    /// The cursors to record once the run applies, by file key.
    pub cursors: Vec<(String, i64)>,
}

/// Read every source from its cursor to its last complete line (zstd: frame), oldest
/// first, into a digest of at most `budget` bytes. A session that does not fit is cut
/// at a message - its cursor stops at that message's line or frame - and the sessions
/// after it wait. A file shorter than its cursor was rewritten and is read from the
/// start.
pub fn digest(sources: &[Source], roots: &[String], cursors: &HashMap<String, i64>, budget: usize) -> Digest {
    let mut out = Digest::default();
    let mut full = false;
    for source in sources {
        let from = match cursors.get(&source.key).copied().unwrap_or(0) {
            SKIPPED => continue,
            at if at < 0 || at as u64 > source.len => 0,
            at => at as u64,
        };
        if from == source.len {
            continue;
        }
        if full {
            out.waiting += 1;
            continue;
        }
        let room = budget.saturating_sub(out.text.len());
        let Ok(session) = read_session(source, from, roots, room) else { continue };
        if session.skipped {
            out.cursors.push((source.key.clone(), SKIPPED));
            continue;
        }
        if session.messages.is_empty() {
            if session.end > from {
                out.cursors.push((source.key.clone(), session.end as i64));
            }
            continue;
        }
        let mut chunk = header(source, &session.messages[0]);
        // The chunk's length before each message taken.
        let mut before = Vec::new();
        let mut cut = None;
        for (i, message) in session.messages.iter().enumerate() {
            let line = format!("[{}] {}\n", message.role, message.text);
            if out.text.len() + chunk.len() + line.len() > budget {
                cut = Some(i);
                break;
            }
            before.push(chunk.len());
            chunk.push_str(&line);
        }
        let mut kept = before.len();
        let cursor = match cut {
            Some(i) => {
                full = true;
                // A cursor stops where a read can resume: a zstd frame holds several
                // messages, so those of the cut one's frame already taken go back and are
                // read again next run - unless that frame alone outgrows the digest, when
                // the rest of it is dropped rather than hold every later run.
                let at = session.messages[i].start;
                let first = session.messages[..i].iter().position(|m| m.start == at).unwrap_or(i);
                if first == 0 && i > 0 && out.text.is_empty() {
                    session.messages[i].next
                } else {
                    chunk.truncate(before.get(first).copied().unwrap_or(chunk.len()));
                    kept = first;
                    at
                }
            }
            None => session.end,
        };
        if cut.is_some() && cursor < source.len {
            out.waiting += 1;
        }
        if kept > 0 {
            out.text.push_str(&chunk);
            out.sessions += 1;
            out.messages += kept;
        }
        if cursor > from {
            out.cursors.push((source.key.clone(), cursor as i64));
        }
    }
    out
}

fn header(source: &Source, first: &Message) -> String {
    let at = first.at.get(..16).map(|t| format!(" · {} UTC", t.replace('T', " "))).unwrap_or_default();
    match source.layout {
        Layout::ClaudeCode => {
            let id: String = source.path.file_stem().map(|s| s.to_string_lossy().chars().take(8).collect()).unwrap_or_default();
            format!("\n### Session {id}{at}\n")
        }
        // A DSH log is named for its generation; the session is its directory.
        Layout::Dsh { .. } => {
            let id: String = source
                .path
                .parent()
                .and_then(Path::file_name)
                .map(|s| s.to_string_lossy().chars().take(8).collect())
                .unwrap_or_default();
            format!("\n### DSH session {id}{at}\n")
        }
    }
}

#[derive(Debug)]
struct Message {
    role: &'static str,
    text: String,
    at: String,
    /// Where a read that takes this message resumes from: the offset of its line, or
    /// of its zstd frame.
    start: u64,
    /// The offset after that line or frame.
    next: u64,
}

#[derive(Debug, Default)]
struct Session {
    messages: Vec<Message>,
    /// The offset after the last complete line (zstd: frame) read.
    end: u64,
    /// Never read again: a sync worker's own session, or a DSH session that is a
    /// subagent's or ran outside the roots.
    skipped: bool,
}

/// One session's reading, line by line.
struct Collect<'a> {
    roots: &'a [String],
    layout: Layout,
    session: Session,
    first_user: bool,
    collected: usize,
    /// A seeded (forked) DSH session read from its start: it opens with the history
    /// it inherited, which ends at the last `session/end-seed` that says so.
    seeded: bool,
}

impl Collect<'_> {
    /// Take one complete line; false once the session turns out to be skipped.
    fn line(&mut self, raw: &[u8], start: u64, next: u64) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(raw) else { return true };
        if self.seeded && value.get("type").and_then(Value::as_str) == Some("session/end-seed") && value.pointer("/data/inherited").is_some() {
            self.session.messages.clear();
            self.collected = 0;
            self.first_user = true;
            return true;
        }
        let kept = match self.layout {
            Layout::ClaudeCode => claude_message(&value, self.roots),
            Layout::Dsh { .. } => dsh_message(&value),
        };
        let Some((role, text, at)) = kept else { return true };
        if role == "user" && std::mem::take(&mut self.first_user) && text.starts_with(MARKER) {
            self.session.skipped = true;
            return false;
        }
        self.collected += text.len() + 16;
        self.session.messages.push(Message { role, text, at, start, next });
        true
    }

    /// Whether the kept text passed `room` - the digest cuts before then anyway. A
    /// seeded session reads on to where its own history starts.
    fn full(&self, room: usize) -> bool {
        !self.seeded && self.collected > room
    }
}

/// The kept messages from `from` to the last complete line (zstd: frame).
fn read_session(source: &Source, from: u64, roots: &[String], room: usize) -> std::io::Result<Session> {
    let mut collect = Collect {
        roots,
        layout: source.layout,
        session: Session { end: from, ..Session::default() },
        first_user: true,
        collected: 0,
        seeded: false,
    };
    if let Layout::Dsh { zstd } = source.layout {
        // Nothing is read before the header is whole: a log just created waits.
        let Some(head) = dsh_head(&source.path, zstd)? else { return Ok(collect.session) };
        if head.delegated || !head.cwd.as_deref().is_some_and(|cwd| roots.iter().any(|root| within(cwd, root))) {
            collect.session.skipped = true;
            return Ok(collect.session);
        }
        collect.seeded = head.seeded && from == 0;
        if zstd {
            read_frames(&source.path, from, room, &mut collect)?;
            return Ok(collect.session);
        }
    }
    read_lines(&source.path, from, room, &mut collect)?;
    Ok(collect.session)
}

/// Lines from `from` to the last complete one.
fn read_lines(path: &Path, from: u64, room: usize, collect: &mut Collect) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            return Ok(());
        }
        let start = collect.session.end;
        collect.session.end += read as u64;
        let next = collect.session.end;
        if !collect.line(&line, start, next) {
            return Ok(());
        }
        if collect.full(room) {
            return Ok(());
        }
    }
}

/// zstd frames from `from` to the last complete one, each one batch of whole lines.
/// A torn last frame waits for the rest of it; a whole frame that does not decode is
/// passed over, as no later read could do better.
fn read_frames(path: &Path, from: u64, room: usize, collect: &mut Collect) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut at = 0;
    while at < bytes.len() {
        let size = match zstd::zstd_safe::find_frame_compressed_size(&bytes[at..]) {
            Ok(size) if size > 0 => size,
            _ => return Ok(()),
        };
        let start = from + at as u64;
        let frame = &bytes[at..at + size];
        at += size;
        let next = from + at as u64;
        collect.session.end = next;
        let Ok(plain) = zstd::stream::decode_all(frame) else { continue };
        for line in plain.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            if !collect.line(line, start, next) {
                return Ok(());
            }
        }
        if collect.full(room) {
            return Ok(());
        }
    }
    Ok(())
}

/// A DSH log's header: whose session it is and where it ran.
#[derive(Debug, PartialEq, Eq)]
struct DshHead {
    cwd: Option<String>,
    /// A subagent's session: its prompt is the parent's delegation, not a person.
    delegated: bool,
    seeded: bool,
}

/// The header of a DSH log - its first line, or its first frame's - or `None` while
/// it is not whole yet.
fn dsh_head(path: &Path, zstd: bool) -> std::io::Result<Option<DshHead>> {
    let mut start = Vec::new();
    File::open(path)?.take(HEAD_BYTES).read_to_end(&mut start)?;
    let first = if zstd {
        let Ok(size) = zstd::zstd_safe::find_frame_compressed_size(&start) else { return Ok(None) };
        let Ok(plain) = zstd::stream::decode_all(&start[..size]) else { return Ok(None) };
        plain
    } else {
        start
    };
    let Some(end) = first.iter().position(|b| *b == b'\n') else { return Ok(None) };
    let Ok(value) = serde_json::from_slice::<Value>(&first[..end]) else { return Ok(None) };
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    Ok(Some(DshHead {
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_string),
        delegated: value.get("origin").and_then(Value::as_str) == Some("subagent")
            || value.get("delegationDepth").and_then(Value::as_u64).unwrap_or(0) > 0,
        seeded: value.get("isSeeded").and_then(Value::as_bool).unwrap_or(false),
    }))
}

/// A Claude Code line's kept text: a user or assistant line's `text` blocks, cleaned,
/// redacted and clipped; `None` for everything else.
fn claude_message(line: &Value, roots: &[String]) -> Option<(&'static str, String, String)> {
    let role = match line.get("type").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let flag = |name: &str| line.get(name).and_then(Value::as_bool).unwrap_or(false);
    if flag("isMeta") || flag("isSidechain") || flag("isCompactSummary") || flag("isVisibleInTranscriptOnly") {
        return None;
    }
    if let Some(cwd) = line.get("cwd").and_then(Value::as_str) {
        if !roots.iter().any(|root| within(cwd, root)) {
            return None;
        }
    }
    let raw = texts(line.get("message")?.get("content")?)?;
    let at = line.get("timestamp").and_then(Value::as_str).unwrap_or_default().to_string();
    kept(role, &raw, at)
}

/// A DSH event's kept text: a `user/message` the person typed (`source.kind` "user":
/// injected instructions, rules, skills, the clock, agent mail and compaction
/// checkpoints carry other kinds) or an `assistant/message`, `text` blocks only -
/// never reasoning or tool calls - cleaned, redacted and clipped.
fn dsh_message(line: &Value) -> Option<(&'static str, String, String)> {
    let data = line.get("data")?;
    let (role, content) = match line.get("type").and_then(Value::as_str)? {
        "user/message" if data.pointer("/source/kind").and_then(Value::as_str) == Some("user") => ("user", data.get("content")?),
        "assistant/message" => ("assistant", data.get("message")?.get("content")?),
        _ => return None,
    };
    let raw = texts(content)?;
    let at = line.get("time").and_then(Value::as_i64).map(iso_of_ms).unwrap_or_default();
    kept(role, &raw, at)
}

/// A message's `content`: the string itself, or its `text` blocks joined.
fn texts(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn kept(role: &'static str, raw: &str, at: String) -> Option<(&'static str, String, String)> {
    let text = clean(role, raw)?;
    // Redact before clipping: a cut through a credential would leave a fragment
    // no rule recognises.
    let text = clip(&secrets::redact(&text), MESSAGE_CHARS);
    Some((role, text, at))
}

/// `ms` since the epoch as `YYYY-MM-DDTHH:MM:SS` UTC (the civil date after Howard
/// Hinnant's days-from-civil inverse).
fn iso_of_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Reminders stripped; harness output, interruptions and continuation summaries
/// dropped; a slash command kept as `/name args` when it carried arguments.
fn clean(role: &str, raw: &str) -> Option<String> {
    let stripped = strip_elements(raw, "system-reminder");
    let text = stripped.trim();
    if text.is_empty() {
        return None;
    }
    if role == "user" {
        if text.starts_with("This session is being continued") || text.starts_with("[Request interrupted by user") {
            return None;
        }
        if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
            let name = element(text, "command-name").unwrap_or_default().trim();
            let args = element(text, "command-args").unwrap_or_default().trim();
            return (!args.is_empty()).then(|| format!("{name} {args}").trim().to_string());
        }
        if DROPPED.iter().any(|tag| text.starts_with(&format!("<{tag}>"))) {
            return None;
        }
    }
    Some(text.to_string())
}

/// `text` without its `<tag>…</tag>` elements (an unclosed one runs to the end).
fn strip_elements(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&open) {
        out.push_str(&rest[..at]);
        rest = match rest[at..].find(&close) {
            Some(end) => &rest[at + end + close.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The content of the first `<tag>…</tag>`.
fn element<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{tag}>"))?;
    Some(&text[start..start + end])
}

/// At most `chars` characters, an ellipsis marking a cut.
fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A synthetic key of the GLM shape that is no real key.
    const DOTTED: &str = "0123456789abcdef0123456789abcdef.AbCdEfGh12345678";

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-mem-transcript-{}-{}",
            std::process::id(),
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn user(cwd: &str, text: &str) -> String {
        json!({"type": "user", "cwd": cwd, "timestamp": "2026-10-05T13:01:02.000Z",
               "message": {"role": "user", "content": text}})
        .to_string()
    }

    fn assistant(cwd: &str, blocks: Value) -> String {
        json!({"type": "assistant", "cwd": cwd, "timestamp": "2026-10-05T13:01:09.000Z",
               "message": {"role": "assistant", "content": blocks}})
        .to_string()
    }

    fn write(path: &Path, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(path, text).expect("write fixture");
    }

    #[test]
    fn munge_matches_claude_code_names() {
        assert_eq!(munge(r"C:\Users\dev\swiss"), "C--Users-dev-swiss");
        assert_eq!(munge("/home/me/my_app.v2"), "-home-me-my-app-v2");
        assert_eq!(munge("/srv/缓存"), "-srv---");
        assert_eq!(munge("/srv/a😀"), "-srv-a--");
    }

    #[test]
    fn within_compares_whole_segments() {
        assert!(within("/srv/app", "/srv/app"));
        assert!(within("/srv/app/crates/x", "/srv/app/"));
        assert!(!within("/srv/app-old", "/srv/app"));
        assert!(!within("/srv", "/srv/app"));
        assert!(within(r"C:\Users\dev\swiss\crates", r"\\?\C:\Users\dev\swiss"));
        if cfg!(windows) {
            assert!(within(r"c:\dev\swiss", r"C:\Users\dev\swiss"));
        }
    }

    #[test]
    fn sources_find_the_root_and_its_subdirectory_sessions_only() {
        let base = temp_dir();
        let root = "/srv/app";
        for (dir, file) in [("-srv-app", "a.jsonl"), ("-srv-app-crates", "b.jsonl"), ("-srv-apple", "c.jsonl"), ("-srv-app", "d.txt")] {
            std::fs::create_dir_all(base.join(dir)).expect("dir");
            std::fs::write(base.join(dir).join(file), "{}\n").expect("file");
        }
        std::fs::create_dir_all(base.join("-srv-app").join("s1").join("subagents")).expect("dir");
        std::fs::write(base.join("-srv-app").join("s1").join("subagents").join("x.jsonl"), "{}\n").expect("file");
        let mut found: Vec<String> = sources(&[base.clone(), base.join("missing")], &[root.into()])
            .iter()
            .map(|s| s.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
            .collect();
        found.sort();
        assert_eq!(found, ["a.jsonl", "b.jsonl"]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn digest_keeps_only_what_people_and_the_agent_wrote() {
        let base = temp_dir();
        let root = "/srv/app";
        std::fs::create_dir_all(base.join("-srv-app")).expect("dir");
        let file = base.join("-srv-app").join("0f1e2d3c-aaaa.jsonl");
        write(
            &file,
            &[
                user(root, "Why does the cache <system-reminder>secret plan</system-reminder>expire early?"),
                json!({"type": "user", "isMeta": true, "cwd": root, "message": {"content": "meta"}}).to_string(),
                assistant(root, json!([{"type": "thinking", "thinking": "hidden"}])),
                assistant(root, json!([{"type": "tool_use", "name": "Read", "input": {}}])),
                user(root, "ignored")
                    .replace("\"content\":\"ignored\"", "\"content\":[{\"type\":\"tool_result\",\"content\":\"file body\"}]"),
                assistant(root, json!([{"type": "text", "text": "The TTL is read in seconds but stored in ms."}])),
                user(root, "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>the cache</command-args>"),
                user(root, "<command-name>/clear</command-name>\n<command-args></command-args>"),
                user(root, "<local-command-stdout>ok</local-command-stdout>"),
                user(root, "[Request interrupted by user]"),
                user(root, "This session is being continued from a previous conversation..."),
                user("/srv/other", "elsewhere"),
                json!({"type": "assistant", "isSidechain": true, "cwd": root,
                       "message": {"content": [{"type": "text", "text": "side"}]}})
                .to_string(),
                user(root, &format!("export ANTHROPIC_AUTH_TOKEN={DOTTED} then go")),
            ],
        );
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        let out = digest(&found, &[root.into()], &HashMap::new(), DIGEST_BYTES);
        assert_eq!(out.sessions, 1);
        assert_eq!(out.messages, 4, "{}", out.text);
        assert!(out.text.contains("### Session 0f1e2d3c · 2026-10-05 13:01 UTC"), "{}", out.text);
        assert!(out.text.contains("[user] Why does the cache expire early?"), "{}", out.text);
        assert!(out.text.contains("[assistant] The TTL is read in seconds but stored in ms."), "{}", out.text);
        assert!(out.text.contains("[user] /review the cache"), "{}", out.text);
        assert!(out.text.contains("[redacted:"), "{}", out.text);
        for gone in ["secret plan", "meta", "hidden", "file body", "/clear", "ok", "interrupted", "continued", "elsewhere", "side", DOTTED] {
            assert!(!out.text.contains(gone), "{gone} leaked: {}", out.text);
        }
        assert_eq!(out.cursors, vec![(found[0].key.clone(), found[0].len as i64)]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn cursors_resume_skip_partial_lines_and_reset_on_shrink() {
        let base = temp_dir();
        let root = "/srv/app";
        std::fs::create_dir_all(base.join("-srv-app")).expect("dir");
        let file = base.join("-srv-app").join("s.jsonl");
        write(&file, &[user(root, "first question")]);
        let first_len = std::fs::metadata(&file).expect("meta").len();
        let mut text = std::fs::read_to_string(&file).expect("read");
        text.push_str(&user(root, "second question"));
        text.push('\n');
        text.push_str(&user(root, "half a line")[..20]);
        std::fs::write(&file, &text).expect("append");
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        let cursors = HashMap::from([(found[0].key.clone(), first_len as i64)]);
        let out = digest(&found, &[root.into()], &cursors, DIGEST_BYTES);
        assert_eq!(out.messages, 1);
        assert!(out.text.contains("second question") && !out.text.contains("first question"), "{}", out.text);
        let complete = text.rfind('\n').expect("newline") as i64 + 1;
        assert_eq!(out.cursors, vec![(found[0].key.clone(), complete)]);
        assert_eq!(pending_bytes(&found, &cursors), found[0].len - first_len);
        // A cursor past the end means the file was rewritten: read it whole.
        let past = HashMap::from([(found[0].key.clone(), found[0].len as i64 + 100)]);
        assert_eq!(digest(&found, &[root.into()], &past, DIGEST_BYTES).messages, 2);
        // A skipped session stays skipped.
        let skipped = HashMap::from([(found[0].key.clone(), SKIPPED)]);
        assert_eq!(digest(&found, &[root.into()], &skipped, DIGEST_BYTES), Digest::default());
        assert_eq!(pending_bytes(&found, &skipped), 0);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_sync_workers_own_session_is_skipped_for_good() {
        let base = temp_dir();
        let root = "/srv/app";
        std::fs::create_dir_all(base.join("-srv-app")).expect("dir");
        let file = base.join("-srv-app").join("w.jsonl");
        write(
            &file,
            &[
                user(root, &format!("{MARKER}\nYou keep this project's memory map current.")),
                assistant(root, json!([{"type": "text", "text": "{\"summary\": \"x\"}"}])),
            ],
        );
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        let out = digest(&found, &[root.into()], &HashMap::new(), DIGEST_BYTES);
        assert_eq!((out.text.as_str(), out.cursors.clone()), ("", vec![(found[0].key.clone(), SKIPPED)]));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_full_digest_cuts_at_a_message_and_later_sessions_wait() {
        let base = temp_dir();
        let root = "/srv/app";
        std::fs::create_dir_all(base.join("-srv-app")).expect("dir");
        let long = "x".repeat(3000);
        let older = base.join("-srv-app").join("older.jsonl");
        write(&older, &[user(root, &long), user(root, &long), user(root, &long)]);
        let newer = base.join("-srv-app").join("newer.jsonl");
        write(&newer, &[user(root, "later")]);
        // Make the order certain whatever the file system's timestamp resolution.
        let mut found = sources(std::slice::from_ref(&base), &[root.into()]);
        found.sort_by_key(|s| s.path.file_name() != Some(std::ffi::OsStr::new("older.jsonl")));
        let out = digest(&found, &[root.into()], &HashMap::new(), 4500);
        assert_eq!((out.sessions, out.messages, out.waiting), (1, 2, 2), "{}", out.text);
        assert!(out.text.contains(&format!("{}…", "x".repeat(MESSAGE_CHARS))));
        let line = user(root, &long).len() as i64 + 1;
        assert_eq!(out.cursors, vec![(found[0].key.clone(), 2 * line)]);
        // The next run resumes at the cut and reaches the newer session.
        let cursors: HashMap<String, i64> = out.cursors.into_iter().collect();
        let next = digest(&found, &[root.into()], &cursors, DIGEST_BYTES);
        assert_eq!((next.sessions, next.messages, next.waiting), (2, 2, 0), "{}", next.text);
        std::fs::remove_dir_all(&base).ok();
    }

    /// 2026-10-05T13:01:02Z.
    const DSH_TIME: i64 = 1_791_205_262_000;

    fn dsh_header(cwd: &str, extra: Value) -> String {
        let mut head = json!({"type": "session", "version": 4, "id": "s", "createdAt": DSH_TIME, "cwd": cwd,
                              "isSeeded": false, "delegationDepth": 0});
        if let (Some(head), Value::Object(extra)) = (head.as_object_mut(), extra) {
            head.extend(extra);
        }
        head.to_string()
    }

    fn dsh_user(kind: &str, text: &str) -> String {
        json!({"type": "user/message", "seq": 9, "time": DSH_TIME,
               "data": {"content": [{"type": "text", "text": text}], "source": {"kind": kind}, "role": "user", "id": "u"},
               "surfaceOp": "append"})
        .to_string()
    }

    fn dsh_assistant(blocks: Value) -> String {
        json!({"type": "assistant/message", "seq": 10, "time": DSH_TIME + 7000,
               "data": {"turn": 1, "step": 1, "message": {"role": "assistant", "content": blocks, "source": {"kind": "model"}}},
               "surfaceOp": "append"})
        .to_string()
    }

    /// One appended batch as DSH writes it: whole lines in one zstd frame.
    fn frame(lines: &[String]) -> Vec<u8> {
        let mut text = lines.join("\n");
        text.push('\n');
        zstd::encode_all(text.as_bytes(), 0).expect("compress")
    }

    fn dsh_session(base: &Path, key: &str, session: &str, name: &str, bytes: &[u8]) -> PathBuf {
        let dir = base.join(key).join(session);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join(name);
        std::fs::write(&file, bytes).expect("write log");
        file
    }

    #[test]
    fn dsh_key_matches_dsh_names() {
        // The first is a real directory under ~/.dsh/sessions.
        assert_eq!(dsh_key(r"C:\Users\user\dev\swiss"), "--C-Users-user-dev-swiss--");
        assert_eq!(dsh_key("/home/me/my_app.v2"), "--home-me-my_app.v2--");
        assert_eq!(dsh_key("/srv/缓存"), "--srv-~7F13~5B58--");
        assert_eq!(dsh_key("/srv/a😀"), "--srv-a~D83D~DE00--");
        assert_eq!(dsh_key("/srv/a~b c"), "--srv-a~007Eb~0020c--");
        assert_eq!(dsh_key(r"C:\\x//y"), "--C-x-y--");
        assert_eq!(dsh_key("/"), "--root--");
        assert_eq!(dsh_key(&format!("/{}", "a".repeat(300))).len(), 2 + 251 + 2);
        assert_eq!(iso_of_ms(0), "1970-01-01T00:00:00");
        assert_eq!(iso_of_ms(951_782_400_000), "2000-02-29T00:00:00");
        assert_eq!(iso_of_ms(1_792_024_598_151), "2026-10-15T00:36:38");
        assert_eq!(iso_of_ms(-1), "1969-12-31T23:59:59");
    }

    #[test]
    fn sources_recognise_each_layout_by_its_directory() {
        let base = temp_dir();
        let root = "/srv/app";
        let log = frame(&[dsh_header(root, json!({}))]);
        dsh_session(&base, "--srv-app--", "s1", "session.v4.jsonl.zstd", &log);
        // An older generation beside the current one is the same session, migrated.
        dsh_session(&base, "--srv-app--", "s1", "session.v3.jsonl", b"{}\n");
        dsh_session(&base, "--srv-app-crates--", "s2", "session.v4.jsonl", b"{}\n");
        dsh_session(&base, "--srv-app--", "s3", "session.jsonl", b"{}\n");
        for (key, session, name) in [
            ("--srv-apple--", "s4", "session.v4.jsonl"),
            ("--srv-app--", "s5", "notes.txt"),
            ("--srv-app--", "s6", "session.v04.jsonl"),
            ("--srv-app--", "s7", "session.v0.jsonl"),
        ] {
            dsh_session(&base, key, session, name, b"{}\n");
        }
        std::fs::create_dir_all(base.join("-srv-app")).expect("dir");
        std::fs::write(base.join("-srv-app").join("a.jsonl"), "{}\n").expect("file");
        let mut found: Vec<(String, Layout)> = sources(std::slice::from_ref(&base), &[root.into()])
            .iter()
            .map(|s| {
                let rel = s.path.strip_prefix(&base).expect("under base").display().to_string().replace('\\', "/");
                (rel, s.layout)
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            found,
            [
                ("--srv-app--/s1/session.v4.jsonl.zstd".to_string(), Layout::Dsh { zstd: true }),
                ("--srv-app--/s3/session.jsonl".to_string(), Layout::Dsh { zstd: false }),
                ("--srv-app-crates--/s2/session.v4.jsonl".to_string(), Layout::Dsh { zstd: false }),
                ("-srv-app/a.jsonl".to_string(), Layout::ClaudeCode),
            ]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_dsh_session_keeps_what_the_person_and_the_agent_wrote() {
        let base = temp_dir();
        let root = "/srv/app";
        let mut log = frame(&[dsh_header(root, json!({}))]);
        log.extend(frame(&[
            dsh_user("agent-instructions", "AGENTS.md says hi"),
            dsh_user("time-context", "It is Monday"),
            dsh_user("user", "Why does the cache <system-reminder>secret plan</system-reminder>expire early?"),
        ]));
        log.extend(frame(&[
            dsh_assistant(json!([{"type": "reasoning", "text": "hidden"},
                                 {"type": "text", "text": "The TTL is read in seconds but stored in ms."},
                                 {"type": "tool-call", "id": "c", "name": "read", "arguments": "{}"}])),
            json!({"type": "tool/result", "seq": 11, "time": DSH_TIME, "data": {"output": "file body"}}).to_string(),
        ]));
        log.extend(frame(&[dsh_user("compact-checkpoint", "summary of earlier turns")]));
        log.extend(frame(&[dsh_user("user", &format!("export ANTHROPIC_AUTH_TOKEN={DOTTED} then go"))]));
        let whole = log.len() as u64;
        let tail = frame(&[dsh_user("user", "a later question")]);
        log.extend(&tail[..tail.len() / 2]);
        let file = dsh_session(&base, "--srv-app--", "0f1e2d3c-aaaa", "session.v4.jsonl.zstd", &log);
        // A subagent's session, and one a sibling directory holds that ran elsewhere.
        dsh_session(&base, "--srv-app--", "sub", "session.v4.jsonl",
                    format!("{}\n{}\n", dsh_header(root, json!({"origin": "subagent", "delegationDepth": 1})), dsh_user("user", "do the subtask")).as_bytes());
        dsh_session(&base, "--srv-app-old--", "old", "session.v4.jsonl",
                    format!("{}\n{}\n", dsh_header("/srv/app-old", json!({})), dsh_user("user", "elsewhere")).as_bytes());

        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        assert_eq!(found.len(), 3);
        let out = digest(&found, &[root.into()], &HashMap::new(), DIGEST_BYTES);
        assert_eq!((out.sessions, out.messages), (1, 3), "{}", out.text);
        assert!(out.text.contains("### DSH session 0f1e2d3c · 2026-10-05 13:01 UTC"), "{}", out.text);
        assert!(out.text.contains("[user] Why does the cache expire early?"), "{}", out.text);
        assert!(out.text.contains("[assistant] The TTL is read in seconds but stored in ms."), "{}", out.text);
        assert!(out.text.contains("[redacted:"), "{}", out.text);
        for gone in ["AGENTS.md", "Monday", "secret plan", "hidden", "file body", "summary of earlier", "subtask", "elsewhere", "later question", DOTTED] {
            assert!(!out.text.contains(gone), "{gone} leaked: {}", out.text);
        }
        let key = file.display().to_string();
        let mut cursors = out.cursors.clone();
        cursors.sort();
        let mut expected: Vec<(String, i64)> = found
            .iter()
            .filter(|s| s.key != key)
            .map(|s| (s.key.clone(), SKIPPED))
            .chain([(key.clone(), whole as i64)])
            .collect();
        expected.sort();
        assert_eq!(cursors, expected, "the torn frame waits; the subagent's and the stranger's are skipped for good");

        // The frame completes; the next run reads it alone.
        let mut bytes = std::fs::read(&file).expect("read");
        bytes.extend(&tail[tail.len() / 2..]);
        std::fs::write(&file, &bytes).expect("complete");
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        let cursors: HashMap<String, i64> = out.cursors.into_iter().collect();
        let next = digest(&found, &[root.into()], &cursors, DIGEST_BYTES);
        assert_eq!(next.messages, 1, "{}", next.text);
        assert!(next.text.contains("[user] a later question"), "{}", next.text);
        assert_eq!(next.cursors, vec![(key, bytes.len() as i64)]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_forked_dsh_session_drops_the_history_it_inherited() {
        let base = temp_dir();
        let root = "/srv/app";
        let end_seed = |data: Value| json!({"type": "session/end-seed", "seq": 5, "time": DSH_TIME, "data": data}).to_string();
        let lines = [
            dsh_header(root, json!({"isSeeded": true, "parentSession": "p"})),
            dsh_user("user", "the parent's question"),
            dsh_assistant(json!([{"type": "text", "text": "the parent's answer"}])),
            end_seed(json!({"inherited": 3})),
            end_seed(json!({})),
            dsh_user("user", "the fork's own question"),
        ];
        let file = dsh_session(&base, "--srv-app--", "fork", "session.v4.jsonl", format!("{}\n", lines.join("\n")).as_bytes());
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        let out = digest(&found, &[root.into()], &HashMap::new(), DIGEST_BYTES);
        assert_eq!(out.messages, 1, "{}", out.text);
        assert!(out.text.contains("the fork's own question") && !out.text.contains("parent"), "{}", out.text);
        assert_eq!(out.cursors, vec![(file.display().to_string(), std::fs::metadata(&file).expect("meta").len() as i64)]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_cut_inside_a_zstd_frame_resumes_at_the_frame() {
        let base = temp_dir();
        let root = "/srv/app";
        // Words, not one run: a long run reads as a token and is redacted short.
        let long = |n: usize| format!("{n} {}", "word ".repeat(300).trim_end());
        let head = frame(&[dsh_header(root, json!({}))]);
        let first = frame(&[dsh_user("user", "first")]);
        let batch = frame(&[dsh_user("user", &long(1)), dsh_user("user", &long(2)), dsh_user("user", &long(3))]);
        let log = [head.clone(), first.clone(), batch.clone()].concat();
        let file = dsh_session(&base, "--srv-app--", "s", "session.v4.jsonl.zstd", &log);
        let found = sources(std::slice::from_ref(&base), &[root.into()]);
        // Room for "first" and two of the batch: the batch's two go back with it.
        let out = digest(&found, &[root.into()], &HashMap::new(), 3500);
        assert_eq!((out.messages, out.waiting), (1, 1), "{}", out.text);
        let key = file.display().to_string();
        let at_batch = (head.len() + first.len()) as i64;
        assert_eq!(out.cursors, vec![(key.clone(), at_batch)]);
        let cursors: HashMap<String, i64> = out.cursors.into_iter().collect();
        let next = digest(&found, &[root.into()], &cursors, DIGEST_BYTES);
        assert_eq!(next.messages, 3, "{}", next.text);
        assert_eq!(next.cursors, vec![(key.clone(), log.len() as i64)]);
        // A frame that alone outgrows the digest keeps what fits and moves past it.
        let alone = digest(&found, &[root.into()], &cursors, 3500);
        assert_eq!((alone.messages, alone.waiting), (2, 0), "{}", alone.text);
        assert_eq!(alone.cursors, vec![(key, log.len() as i64)]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn clip_and_strip_handle_edges() {
        assert_eq!(clip("缓存设计", 2), "缓存…");
        assert_eq!(clip("short", 10), "short");
        assert_eq!(strip_elements("a<system-reminder>x</system-reminder>b<system-reminder>open", "system-reminder"), "ab");
        assert_eq!(element("<command-args>go</command-args>", "command-args"), Some("go"));
        assert_eq!(baseline(&[]), Vec::new());
    }
}
