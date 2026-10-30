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

//! The file tools the llm plugin's own tasks offer a builtin agent (SPEC §llm.agents).
//! Memory's tasks offer read over their checkout ([read_tool] is shared, not forked);
//! the panel's agent Test, which a person drives, offers read, write and edit here and
//! glob and grep ([crate::llm_search]) over its selected directory, so a Test can look
//! at and change files instead of guessing.
//!
//! None of them leaves the root the consumer pinned (an absolute path naming something
//! inside it is fine - users hand the model absolute paths), none opens a credential-
//! named file, and what they serve is redacted: the model on the other end is a remote
//! party, the same rule memory's walk follows.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_llm::agent::{ToolAnswer, ToolSpec, Toolbox};

/// One read serves at most this many bytes (memory's runner uses the same cap).
const READ_CAP: usize = 64 * 1024;
/// One write carries at most this many bytes: a Test writes source and notes, not data.
const WRITE_CAP: usize = 1024 * 1024;
/// edit opens a file of at most this many bytes.
const EDIT_CAP: usize = 4 * 1024 * 1024;

/// read: one file, pinned inside the root, credential-named files refused, and what
/// it serves redacted like the memory walk's own material - the model is a remote
/// party. An absolute path is accepted when it names something inside the root:
/// users paste absolute paths into prompts without knowing the pin exists.
pub(crate) fn read_tool(root: &Path, args: &Value) -> String {
    read_capped(root, args, READ_CAP)
}

/// [read_tool] serving at most `cap` bytes a read: a loop that reads many files keeps its
/// context small this way, and a long file is read on a window at a time.
pub(crate) fn read_capped(root: &Path, args: &Value, cap: usize) -> String {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return "error: read needs a path string".into();
    };
    let from = args.get("from").and_then(Value::as_u64).map(|n| n.max(1) as usize);
    let count = args.get("lines").and_then(Value::as_u64).map(|n| n.max(1) as usize);
    let full = match pinned(root, path) {
        Ok(full) => full,
        Err(err) => return format!("error: {err}"),
    };
    if credential_inside(root, &full) {
        return format!("error: {path} looks like a credential file; the runner never reads those");
    }
    match std::fs::read(&full) {
        Ok(bytes) => {
            let text = swiss_memory::secrets::redact(&String::from_utf8_lossy(&bytes));
            if from.is_none() && count.is_none() && text.len() <= cap {
                return text;
            }
            lines_of(path, &text, from.unwrap_or(1), count, cap)
        }
        Err(err) => format!("error: {path}: {err}"),
    }
}

/// A window of whole lines: `count` lines (or as many as fit `cap` bytes) from line
/// `from`, headed by where they sit, and closed by where to read on when the file
/// runs past them. The lines are the file's own, unnumbered, so a quote copied from
/// them is the file's text.
fn lines_of(path: &str, text: &str, from: usize, count: Option<usize>, cap: usize) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = from - 1;
    if start >= lines.len() {
        return format!("error: {path} has {} lines", lines.len());
    }
    let limit = count.map_or(lines.len(), |count| start + count).min(lines.len());
    let (mut out, mut end) = (String::new(), start);
    while end < limit && out.len() + lines[end].len() <= cap {
        out.push_str(lines[end]);
        end += 1;
    }
    if end == start {
        // One line longer than the cap: its head, cut.
        out = lines[start].to_string();
        cut_utf8(&mut out, cap);
        end = start + 1;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let mut said = format!("(lines {}-{} of {})\n{out}", start + 1, end, lines.len());
    if end < lines.len() {
        said.push_str(&format!("(the file runs on: read from {} for more)", end + 1));
    }
    said
}

/// The read tool's schema: the path, and an optional window of lines for a long file.
pub(crate) fn read_schema(path: &str) -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["path"],
        "properties": {
            "path": {"type": "string", "description": path},
            "from": {"type": "integer", "minimum": 1, "description": "the first line to read (1 is the first); unset, the file's start"},
            "lines": {"type": "integer", "minimum": 1, "description": "how many lines to read; unset, as many as one read serves"}
        }
    })
}

/// write: a whole file, created (with its missing directories) or replaced. The target
/// is pinned like read's, a credential-named one refused, and what is written is the
/// model's text as given: a `${secret://name}` stays text, so no value lands in a file.
pub(crate) fn write_tool(root: &Path, args: &Value) -> String {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return "error: write needs a path string".into();
    };
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return "error: write needs the content string".into();
    };
    if content.len() > WRITE_CAP {
        return format!("error: the content is {} bytes; one write carries at most {} KiB", content.len(), WRITE_CAP / 1024);
    }
    let full = match pinned_new(root, path) {
        Ok(full) => full,
        Err(err) => return format!("error: {err}"),
    };
    if credential_inside(root, &full) {
        return format!("error: {path} looks like a credential file; the runner never writes those");
    }
    if full.is_dir() {
        return format!("error: {path} is a directory");
    }
    let existed = full.exists();
    if let Some(parent) = full.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return format!("error: {path}: {err}");
        }
    }
    match std::fs::write(&full, content) {
        Ok(()) => format!("{} {path} ({} bytes)", if existed { "replaced" } else { "created" }, content.len()),
        Err(err) => format!("error: {path}: {err}"),
    }
}

/// edit: replace an exact string in a text file - once, where it must be unique, or
/// everywhere with replace_all. A file kept with CRLF line ends still matches a quote
/// written with `\n` (read serves the file's own ends, but a model writes `\n`), and the
/// replacement takes the file's ends.
pub(crate) fn edit_tool(root: &Path, args: &Value) -> String {
    let field = |key: &str| args.get(key).and_then(Value::as_str);
    let (Some(path), Some(old), Some(new)) = (field("path"), field("old_string"), field("new_string")) else {
        return "error: edit needs path, old_string and new_string strings".into();
    };
    let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    if old.is_empty() {
        return "error: old_string is empty; to create a file or replace all of it, use write".into();
    }
    if old == new {
        return "error: old_string and new_string are the same; nothing would change".into();
    }
    let full = match pinned(root, path) {
        Ok(full) => full,
        Err(err) => return format!("error: {err}"),
    };
    if credential_inside(root, &full) {
        return format!("error: {path} looks like a credential file; the runner never edits those");
    }
    let bytes = match std::fs::read(&full) {
        Ok(bytes) => bytes,
        Err(err) => return format!("error: {path}: {err}"),
    };
    if bytes.len() > EDIT_CAP {
        return format!("error: {path} is {} bytes; edit opens files of at most {} MiB", bytes.len(), EDIT_CAP / (1024 * 1024));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return format!("error: {path} is not UTF-8 text; edit changes text files only");
    };
    let crlf = !text.contains(old) && text.contains("\r\n") && old.contains('\n') && !old.contains("\r\n");
    let (old, new) = if crlf {
        (old.replace('\n', "\r\n"), new.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        (old.to_string(), new.to_string())
    };
    let found = text.matches(old.as_str()).count();
    if found == 0 {
        if old.contains("[redacted:") {
            return format!("error: old_string quotes a redacted span of {path}; the file holds the real value there, which edit never matches - quote the text around it");
        }
        return format!("error: old_string was not found in {path}; read the file and quote its text exactly");
    }
    if found > 1 && !all {
        return format!("error: old_string occurs {found} times in {path}; quote more of the text around it to make it unique, or set replace_all");
    }
    let changed = if all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    match std::fs::write(&full, changed) {
        Ok(()) => format!("edited {path}: {} replacement{}", if all { found } else { 1 }, if all && found > 1 { "s" } else { "" }),
        Err(err) => format!("error: {path}: {err}"),
    }
}

/// Whether a pinned path names a credential file, judged by where it sits in the root.
fn credential_inside(root: &Path, full: &Path) -> bool {
    let pinned = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    credential_named(&full.strip_prefix(&pinned).unwrap_or(full).to_string_lossy())
}

/// [pinned] for a path that may not exist yet - write's target. Its nearest existing
/// ancestor is pinned, and below that only plain names may follow (a `..` there names no
/// component), so a new path cannot climb back out of the root it proved inside.
pub(crate) fn pinned_new(root: &Path, path: &str) -> Result<PathBuf, String> {
    let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    let mut base = joined.as_path();
    let mut names = Vec::new();
    while !base.exists() {
        let (Some(name), Some(parent)) = (base.file_name(), base.parent()) else {
            return Err(format!("{path} does not name a file under the root"));
        };
        names.push(name.to_os_string());
        base = parent;
    }
    let (Ok(pin), Ok(mut full)) = (root.canonicalize(), base.canonicalize()) else {
        return Err("the root cannot be read".into());
    };
    if !full.starts_with(&pin) {
        return Err(format!("{path} is outside the root"));
    }
    full.extend(names.iter().rev());
    Ok(full)
}

/// Resolve `path` against the root and prove it stays inside it. A relative path
/// joins the root; an absolute one is taken as given and then pinned, so
/// `C:\\Users\\me\\notes\\a.txt` works while `C:\\Windows\\win.ini`
/// refuses. `..` that escapes, a symlink that points out and a missing path all
/// refuse with a plain sentence the model can act on.
pub(crate) fn pinned(root: &Path, path: &str) -> Result<PathBuf, String> {
    let Ok(pinned) = root.canonicalize() else {
        return Err("the root cannot be read".into());
    };
    let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    let Ok(full) = joined.canonicalize() else {
        return Err(format!("no such path: {path}"));
    };
    if !full.starts_with(&pinned) {
        return Err(format!("{path} is outside the root"));
    }
    Ok(full)
}

/// A path as a person writes it: the verbatim prefix Windows' canonical form carries
/// (`\?\C:\...`, `\?\UNC\server\share`) dropped, which a model would copy into
/// commands that then fail.
pub(crate) fn plain_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(share) = text.strip_prefix(r"\?\UNC\") {
        return format!(r"\{share}");
    }
    text.strip_prefix(r"\?\").map(str::to_string).unwrap_or(text)
}

/// Truncate at a cap without slicing through a multi-byte character: String's
/// own truncate panics on a non-boundary index.
pub(crate) fn cut_utf8(text: &mut String, cap: usize) {
    let mut at = cap;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    text.truncate(at);
}

/// Whether a path names a file the runner never shows the model: the store's own
/// sensitive paths (the previews and the code index refuse the same) and credential
/// names. `\\` and `/` are one separator, so a Windows path is judged like a repo one.
pub(crate) fn credential_named(path: &str) -> bool {
    let slashed = path.replace('\\', "/");
    let name = slashed.rsplit('/').next().unwrap_or_default().to_lowercase();
    swiss_memory::secrets::sensitive_path(&slashed)
        || CREDENTIAL_FILES.iter().any(|bad| name.contains(bad))
}

/// File names the read tool never opens, whatever the model asks for. The memory
/// engine drops these from patches for the same reason (SPEC §memory.sync).
const CREDENTIAL_FILES: [&str; 10] = [
    ".env", "id_rsa", "id_ed25519", "known_hosts", "master.key", "session.json",
    "credentials", ".pem", ".pfx", ".p12",
];

/// The file toolbox the panel's Test offers a builtin agent over one root beside memory's
/// read: write, edit, glob and grep, the agent's taken share of them the only ones built
/// and the only ones callable. The searches walk on a blocking thread: a big tree must
/// not hold the runtime.
pub(crate) struct FsTools {
    root: PathBuf,
    taken: Vec<&'static str>,
}

impl FsTools {
    /// The names this toolbox answers, in catalog order.
    pub(crate) const NAMES: [&'static str; 4] = ["write", "edit", "glob", "grep"];

    /// `taken` is the agent's selected tools, in catalog order.
    pub(crate) fn new(root: &Path, taken: &[&'static str]) -> FsTools {
        FsTools { root: root.to_path_buf(), taken: taken.to_vec() }
    }
}

#[async_trait]
impl Toolbox for FsTools {
    fn specs(&self) -> Vec<ToolSpec> {
        let path = "path relative to the root, or absolute when inside it";
        let all = vec![
            ToolSpec {
                name: "write".into(),
                description: "Write a whole file under the root: creates it (and any missing directories) or replaces all of its content. At most 1 MiB; credential files refused. To change part of an existing file, use edit.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["path", "content"],
                    "properties": {
                        "path": {"type": "string", "description": path},
                        "content": {"type": "string", "description": "the file's whole new content"}
                    }
                }),
            },
            ToolSpec {
                name: "edit".into(),
                description: "Replace an exact string in a text file under the root. old_string must match the file's text exactly (copy it from read, indentation included) and occur once, unless replace_all replaces every occurrence. Credential files refused.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["path", "old_string", "new_string"],
                    "properties": {
                        "path": {"type": "string", "description": path},
                        "old_string": {"type": "string", "description": "the exact text to replace"},
                        "new_string": {"type": "string", "description": "the text to put in its place"},
                        "replace_all": {"type": "boolean", "description": "replace every occurrence; unset, old_string must occur once"}
                    }
                }),
            },
            ToolSpec {
                name: "glob".into(),
                description: "Find files under the root whose path matches a glob pattern (**/*.rs, src/**/*.{ts,tsx}, *.md), newest first, at most 500. The pattern is relative to path (the root when unset); one without / matches that directory's own files only. .git, node_modules, target and similar trees are skipped unless the pattern names them.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["pattern"],
                    "properties": {
                        "pattern": {"type": "string", "description": "the glob: * within a name, ** across directories, ? one character, {a,b} alternatives, [abc] a class"},
                        "path": {"type": "string", "description": "the directory to search; unset, the root"}
                    }
                }),
            },
            ToolSpec {
                name: "grep".into(),
                description: "Search file contents under the root by regular expression (Rust regex syntax), line by line. output_mode files_with_matches (the default) lists matching files newest first; content shows path:line:text, with context lines around each match; count gives matches per file. Binary, oversized and credential files are skipped; output is redacted.".into(),
                schema: json!({
                    "type": "object", "additionalProperties": false, "required": ["pattern"],
                    "properties": {
                        "pattern": {"type": "string", "description": "the regular expression, matched against each line"},
                        "path": {"type": "string", "description": "a directory or one file to search; unset, the root"},
                        "glob": {"type": "string", "description": "only files matching this glob: *.rs matches names at any depth, a pattern with / matches the path"},
                        "output_mode": {"type": "string", "enum": ["files_with_matches", "content", "count"]},
                        "ignore_case": {"type": "boolean"},
                        "context": {"type": "integer", "minimum": 0, "maximum": 10, "description": "content mode: lines shown before and after each match"},
                        "head_limit": {"type": "integer", "minimum": 1, "description": "at most this many lines (content) or files; unset, 200 lines or 500 files"}
                    }
                }),
            },
        ];
        all.into_iter().filter(|spec| self.taken.contains(&spec.name.as_str())).collect()
    }

    async fn call(&mut self, name: &str, args: &Value) -> ToolAnswer {
        // A tool the agent did not take is unknown here, whatever the model calls.
        let text = match name {
            "write" if self.taken.contains(&"write") => write_tool(&self.root, args),
            "edit" if self.taken.contains(&"edit") => edit_tool(&self.root, args),
            "glob" | "grep" if self.taken.contains(&name) => {
                let (root, args, grep) = (self.root.clone(), args.clone(), name == "grep");
                tokio::task::spawn_blocking(move || {
                    if grep { crate::llm_search::grep_tool(&root, &args) } else { crate::llm_search::glob_tool(&root, &args) }
                })
                .await
                .unwrap_or_else(|err| format!("error: the search failed: {err}"))
            }
            other if self.taken.is_empty() => {
                format!("error: unknown tool {other}; this task has no tools")
            }
            other => format!("error: unknown tool {other}; use {}", self.taken.join(", ")),
        };
        let ok = !text.starts_with("error:");
        ToolAnswer { text, ok }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swiss-llm-tools-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn read_serves_a_file_and_refuses_the_sensitive() {
        let root = dir();
        std::fs::write(root.join("note.txt"), "hello").expect("write");
        std::fs::write(root.join(".env"), "KEY=x").expect("write");
        assert!(read_tool(&root, &json!({"path": "note.txt"})).contains("hello"));
        assert!(read_tool(&root, &json!({"path": ".env"})).contains("credential"));
        assert!(read_tool(&root, &json!({"path": "missing.txt"})).starts_with("error:"));
    }

    #[test]
    fn read_serves_a_window_of_lines_and_says_where_to_read_on() {
        let root = dir();
        let text: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        std::fs::write(root.join("long.txt"), &text).expect("write");
        let window = read_tool(&root, &json!({"path": "long.txt", "from": 4, "lines": 3}));
        assert!(window.starts_with("(lines 4-6 of 10)\nline 4\nline 5\nline 6\n"), "{window}");
        assert!(window.ends_with("read from 7 for more)"), "{window}");
        let tail = read_tool(&root, &json!({"path": "long.txt", "from": 9}));
        assert_eq!(tail, "(lines 9-10 of 10)\nline 9\nline 10\n");
        assert!(read_tool(&root, &json!({"path": "long.txt", "from": 11})).starts_with("error:"));
        // A whole file under the cap reads as itself, no header.
        assert_eq!(read_tool(&root, &json!({"path": "long.txt"})), text);
    }

    #[test]
    fn read_accepts_an_absolute_path_inside_and_refuses_one_outside() {
        let root = dir();
        std::fs::write(root.join("note.txt"), "hello").expect("write");
        let absolute = root.join("note.txt");
        assert!(read_tool(&root, &json!({ "path": absolute.to_string_lossy() })).contains("hello"));
        assert!(
            read_tool(&root, &json!({"path": "C:\\Windows\\win.ini"})).contains("outside"),
            "an absolute path outside the root must refuse"
        );
    }

    #[test]
    fn write_creates_or_replaces_a_pinned_file_and_never_a_credential() {
        let root = dir();
        let made = write_tool(&root, &json!({"path": "deep/new/note.txt", "content": "first"}));
        assert_eq!(made, "created deep/new/note.txt (5 bytes)");
        assert_eq!(std::fs::read_to_string(root.join("deep/new/note.txt")).expect("written"), "first");
        let again = write_tool(&root, &json!({"path": "deep/new/note.txt", "content": "second ${secret://api-key}"}));
        assert!(again.starts_with("replaced"), "{again}");
        // A reference is text to a content sink: what lands is what the model wrote.
        assert_eq!(std::fs::read_to_string(root.join("deep/new/note.txt")).expect("written"), "second ${secret://api-key}");
        assert!(write_tool(&root, &json!({"path": ".env", "content": "KEY=x"})).contains("credential"));
        assert!(!root.join(".env").exists());
        assert!(write_tool(&root, &json!({"path": "../escape.txt", "content": "x"})).starts_with("error:"));
        assert!(write_tool(&root, &json!({"path": "new/../../escape.txt", "content": "x"})).starts_with("error:"));
        assert!(!root.parent().expect("parent").join("escape.txt").exists(), "nothing lands outside the root");
        assert!(write_tool(&root, &json!({"path": "deep", "content": "x"})).contains("is a directory"));
        let big = "x".repeat(WRITE_CAP + 1);
        assert!(write_tool(&root, &json!({"path": "big.txt", "content": big})).contains("at most"));
    }

    #[test]
    fn edit_replaces_a_unique_string_or_every_one_and_keeps_crlf() {
        let root = dir();
        std::fs::write(root.join("a.rs"), "fn a() {}\nfn b() {}\nfn a2() {}\n").expect("write");
        let edit = |args: Value| edit_tool(&root, &args);
        assert!(edit(json!({"path": "a.rs", "old_string": "fn a", "new_string": "fn z"})).contains("occurs 2 times"));
        assert!(edit(json!({"path": "a.rs", "old_string": "nope", "new_string": "x"})).contains("not found"));
        assert!(edit(json!({"path": "a.rs", "old_string": "", "new_string": "x"})).contains("use write"));
        assert_eq!(edit(json!({"path": "a.rs", "old_string": "fn b", "new_string": "fn c"})), "edited a.rs: 1 replacement");
        assert_eq!(edit(json!({"path": "a.rs", "old_string": "fn a", "new_string": "fn z", "replace_all": true})), "edited a.rs: 2 replacements");
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).expect("read"), "fn z() {}\nfn c() {}\nfn z2() {}\n");
        // A CRLF file matches a quote written with \n, and the new text takes CRLF.
        std::fs::write(root.join("w.txt"), "one\r\ntwo\r\nthree\r\n").expect("write");
        assert!(edit(json!({"path": "w.txt", "old_string": "one\ntwo", "new_string": "1\n2"})).starts_with("edited"));
        assert_eq!(std::fs::read_to_string(root.join("w.txt")).expect("read"), "1\r\n2\r\nthree\r\n");
        std::fs::write(root.join("id_rsa"), "key").expect("write");
        assert!(edit(json!({"path": "id_rsa", "old_string": "key", "new_string": "x"})).contains("credential"));
        assert!(edit(json!({"path": "a.rs", "old_string": "[redacted:mixed_token]", "new_string": "x"})).contains("redacted span"));
    }

    #[tokio::test]
    async fn the_toolbox_builds_only_the_taken_share() {
        let root = dir();
        let mut all = FsTools::new(&root, &FsTools::NAMES);
        assert_eq!(all.specs().iter().map(|s| s.name.clone()).collect::<Vec<_>>(), FsTools::NAMES);
        let mut one = FsTools::new(&root, &["write"]);
        assert_eq!(one.specs().len(), 1);
        assert_eq!(one.specs()[0].name, "write");
        let none = FsTools::new(&root, &[]);
        assert!(none.specs().is_empty());
        assert!(all.call("write", &json!({"path": "x.txt", "content": "found me"})).await.ok);
        let globbed = all.call("glob", &json!({"pattern": "*.txt"})).await;
        assert!(globbed.ok && globbed.text.contains("x.txt"), "{}", globbed.text);
        let grepped = all.call("grep", &json!({"pattern": "found"})).await;
        assert!(grepped.ok && grepped.text.contains("x.txt"), "{}", grepped.text);
        let refused = one.call("glob", &json!({"pattern": "*"})).await;
        assert!(!refused.ok, "a tool the agent did not take refuses");
    }
}
