// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

//! The search tools the panel's Test offers a builtin agent (SPEC §llm.agents): glob finds
//! files by path pattern, grep searches their contents by regular expression. Both walk
//! only under the Test's pinned root, never follow a link out of it, skip the trees a
//! search almost never means (.git, node_modules, target and the like) unless the pattern
//! names one, never open a credential-named file, and stop at a walk budget - saying so -
//! rather than stall a turn on a whole home directory. What grep serves is redacted: the
//! model on the other end is a remote party. Paths come back relative to the root, `/`
//! separated, which read, write and edit take as given.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use regex::{Regex, RegexBuilder};
use serde_json::Value;

use crate::llm_tools::{credential_named, cut_utf8, pinned};

/// Directory names a walk does not enter unless the pattern names them: version-control
/// stores, dependency and build trees, and a Windows profile's application data.
const SKIPPED: [&str; 8] = [".git", ".hg", ".svn", "node_modules", "target", "__pycache__", ".venv", "AppData"];
/// A walk visits at most this many entries, and stops at this deadline.
const WALK_ENTRIES: usize = 200_000;
const WALK_TIME: Duration = Duration::from_secs(10);
/// glob names at most this many files; grep lists at most this many by default.
const FILES_CAP: usize = 500;
/// grep's content mode shows at most this many lines by default.
const LINES_CAP: usize = 200;
/// grep skips a file larger than this, and reads at most this much in one search.
const GREP_FILE_CAP: u64 = 4 * 1024 * 1024;
const GREP_BYTES: u64 = 256 * 1024 * 1024;
/// One answer carries at most this many bytes, and one shown line at most this many.
const ANSWER_CAP: usize = 64 * 1024;
const LINE_CAP: usize = 500;

/// glob: the files under `path` whose path relative to it matches the pattern, newest
/// first.
pub(crate) fn glob_tool(root: &Path, args: &Value) -> String {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return "error: glob needs a pattern string".into();
    };
    let pattern = pattern.replace('\\', "/");
    let pattern = pattern.trim_start_matches("./");
    if Path::new(pattern).is_absolute() || pattern.starts_with('/') {
        return "error: the pattern is relative to path; put the directory in path and the pattern below it".into();
    }
    let (base, shown) = match searched(root, args) {
        Ok(found) => found,
        Err(err) => return format!("error: {err}"),
    };
    let matcher = match glob_regex(pattern) {
        Ok(matcher) => matcher,
        Err(err) => return format!("error: {err}"),
    };
    // The pattern's leading literal directories are walked to, not searched for.
    let segments: Vec<&str> = pattern.split('/').collect();
    let fixed = segments.iter().take_while(|s| !s.contains(['*', '?', '[', '{'])).count().min(segments.len() - 1);
    let start = segments[..fixed].iter().fold(base.clone(), |path, s| path.join(s));
    let mut walk = Walk::new(root, pattern);
    let mut found: Vec<(String, SystemTime)> = Vec::new();
    walk.files(&start, |full, meta| {
        if let Some(rel) = relative(&base, full) {
            if matcher.is_match(&rel) {
                found.push((walk_shown(root, full), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
            }
        }
        true
    });
    if found.is_empty() {
        let mut said = format!("no files match {pattern} under {shown}");
        if !pattern.contains('/') {
            said.push_str(&format!(" (a pattern without / matches only that directory's own files; **/{pattern} searches below it)"));
        }
        return said + &walk.note();
    }
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = found.len();
    let mut lines: Vec<String> = found.into_iter().take(FILES_CAP).map(|(path, _)| path).collect();
    if total > FILES_CAP {
        lines.push(format!("... {} more (cut at {FILES_CAP}; narrow the pattern or path)", total - FILES_CAP));
    }
    lines.join("\n") + &walk.note()
}

/// How grep shows what it found.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Files,
    Content,
    Count,
}

/// grep: lines matching a regular expression in the files under `path` (or that one file).
pub(crate) fn grep_tool(root: &Path, args: &Value) -> String {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return "error: grep needs a pattern string".into();
    };
    let mode = match args.get("output_mode").and_then(Value::as_str) {
        None | Some("files_with_matches") => Mode::Files,
        Some("content") => Mode::Content,
        Some("count") => Mode::Count,
        Some(other) => return format!("error: output_mode {other:?} is not files_with_matches, content or count"),
    };
    let ignore_case = args.get("ignore_case").and_then(Value::as_bool).unwrap_or(false);
    let context = args.get("context").and_then(Value::as_u64).unwrap_or(0).min(10) as usize;
    let limit = args.get("head_limit").and_then(Value::as_u64).map(|n| n.max(1) as usize);
    let matcher = match RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(matcher) => matcher,
        Err(err) => return format!("error: the pattern is not a regular expression: {err}"),
    };
    // A filter without / judges the name at any depth; one with / the path below `path`.
    let filter = match args.get("glob").and_then(Value::as_str) {
        Some(glob) => match glob_regex(&glob.replace('\\', "/")) {
            Ok(regex) => Some((regex, glob.contains(['/', '\\']))),
            Err(err) => return format!("error: glob: {err}"),
        },
        None => None,
    };
    let (base, shown) = match searched(root, args) {
        Ok(found) => found,
        Err(err) => return format!("error: {err}"),
    };
    let named = args.get("glob").and_then(Value::as_str).unwrap_or_default().replace('\\', "/");
    let mut walk = Walk::new(root, &named);
    let cap = limit.unwrap_or(if mode == Mode::Content { LINES_CAP } else { FILES_CAP });
    let (mut out, mut files, mut matches, mut bytes, mut cut) = (Vec::<String>::new(), Vec::<(String, SystemTime)>::new(), 0usize, 0u64, false);
    walk.files(&base, |full, meta| {
        if let Some((regex, by_path)) = &filter {
            let judged = if *by_path { relative(&base, full) } else { full.file_name().map(|n| n.to_string_lossy().into_owned()) };
            if !judged.is_some_and(|judged| regex.is_match(&judged)) {
                return true;
            }
        }
        if meta.len() > GREP_FILE_CAP {
            return true;
        }
        bytes += meta.len();
        if bytes > GREP_BYTES {
            cut = true;
            return false;
        }
        let Ok(raw) = std::fs::read(full) else { return true };
        if raw[..raw.len().min(8192)].contains(&0) {
            return true;
        }
        let text = String::from_utf8_lossy(&raw);
        let path = walk_shown(root, full);
        match mode {
            Mode::Files => {
                if text.lines().any(|line| matcher.is_match(line)) {
                    files.push((path, meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
                }
            }
            Mode::Count => {
                let n = text.lines().filter(|line| matcher.is_match(line)).count();
                if n > 0 {
                    if out.len() >= cap {
                        cut = true;
                        return false;
                    }
                    matches += n;
                    out.push(format!("{path}:{n}"));
                }
            }
            Mode::Content => {
                let lines: Vec<&str> = text.lines().collect();
                let hits: Vec<usize> = (0..lines.len()).filter(|&i| matcher.is_match(lines[i])).collect();
                let mut shown_to = 0usize;
                for &hit in &hits {
                    if out.len() >= cap {
                        cut = true;
                        return false;
                    }
                    let from = hit.saturating_sub(context).max(shown_to);
                    let to = (hit + context + 1).min(lines.len());
                    if context > 0 && shown_to > 0 && from > shown_to {
                        out.push("--".into());
                    }
                    for (i, line) in lines.iter().enumerate().take(to).skip(from) {
                        let mut line = line.trim_end_matches('\r').to_string();
                        cut_utf8(&mut line, LINE_CAP);
                        let mark = if hits.binary_search(&i).is_ok() { ':' } else { '-' };
                        out.push(format!("{path}{mark}{}{mark}{line}", i + 1));
                    }
                    shown_to = to;
                }
            }
        }
        true
    });
    // A group's context may run past the limit: the answer holds to it.
    if out.len() > cap {
        cut = true;
        out.truncate(cap);
    }
    if mode == Mode::Files {
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if files.len() > cap {
            cut = true;
            files.truncate(cap);
        }
        out = files.into_iter().map(|(path, _)| path).collect();
    }
    if out.is_empty() {
        return format!("no matches for {pattern} under {shown}") + &walk.note();
    }
    let mut text = swiss_memory::secrets::redact(&out.join("\n"));
    if mode == Mode::Count {
        text.push_str(&format!("\n({matches} matching lines in {} files)", out.len()));
    }
    if cut {
        text.push_str("\n(cut at the limit; narrow the pattern, path or glob, or raise head_limit)");
    }
    if text.len() > ANSWER_CAP {
        cut_utf8(&mut text, ANSWER_CAP);
        text.push_str("\n(cut at 64 KiB; narrow the search)");
    }
    text + &walk.note()
}

/// The directory (or file) a search starts from - `path` pinned inside the root, the root
/// when unset - and how the answer names it.
fn searched(root: &Path, args: &Value) -> Result<(PathBuf, String), String> {
    let path = args.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()).unwrap_or(".");
    Ok((pinned(root, path)?, path.to_string()))
}

/// `full` relative to `base`, `/` separated; None when it is not below it.
fn relative(base: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(base).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
}

/// How an answer names a file: relative to the root, which every file tool takes.
fn walk_shown(root: &Path, full: &Path) -> String {
    let pin = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    relative(&pin, full).unwrap_or_else(|| full.display().to_string())
}

/// A glob as an anchored regular expression over a `/`-separated relative path: `**/`
/// any directories (none included), a trailing `**` anything, `*` within one name, `?`
/// one character, `{a,b}` alternatives, `[..]` a class (`[!..]` negated). Case is
/// ignored on Windows, whose file system ignores it too.
fn glob_regex(pattern: &str) -> Result<Regex, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let (mut re, mut depth, mut i) = (String::from("^"), 0usize, 0usize);
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:[^/]*/)*");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            ',' if depth > 0 => re.push('|'),
            '[' => match (i + 2..chars.len()).find(|&j| chars[j] == ']') {
                Some(end) => {
                    let class: String = chars[i + 1..end].iter().collect();
                    let class = class.strip_prefix('!').map_or(class.clone(), |rest| format!("^{rest}"));
                    re.push('[');
                    re.push_str(&class.replace('[', "\\["));
                    re.push(']');
                    i = end + 1;
                    continue;
                }
                None => re.push_str("\\["),
            },
            other => re.push_str(&regex::escape(&other.to_string())),
        }
        i += 1;
    }
    if depth > 0 {
        return Err("the pattern opens a { it never closes".into());
    }
    re.push('$');
    RegexBuilder::new(&re)
        .case_insensitive(cfg!(windows))
        .build()
        .map_err(|err| format!("the pattern does not read as a glob: {err}"))
}

/// One bounded walk: entries visited, its deadline, and which skipped names the pattern
/// named (and so are entered).
struct Walk {
    root: PathBuf,
    entered: Vec<&'static str>,
    visited: usize,
    deadline: Instant,
    stopped: bool,
}

impl Walk {
    fn new(root: &Path, pattern: &str) -> Walk {
        let named = |name: &str| pattern.split('/').any(|seg| seg.eq_ignore_ascii_case(name));
        Walk {
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            entered: SKIPPED.iter().copied().filter(|name| named(name)).collect(),
            visited: 0,
            deadline: Instant::now() + WALK_TIME,
            stopped: false,
        }
    }

    /// Every file under `start` (or `start` itself when it is a file), names sorted per
    /// directory, until `each` answers false or the budget runs out. Links are never
    /// followed - one could lead out of the root - and credential-named files never
    /// reach `each`.
    fn files(&mut self, start: &Path, mut each: impl FnMut(&Path, &std::fs::Metadata) -> bool) {
        let Ok(meta) = std::fs::symlink_metadata(start) else { return };
        if meta.is_file() {
            if !self.credential(start) {
                each(start, &meta);
            }
            return;
        }
        let mut stack = vec![start.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<std::fs::DirEntry> = read.flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            let mut dirs = Vec::new();
            for entry in entries {
                self.visited += 1;
                if self.visited > WALK_ENTRIES || Instant::now() > self.deadline {
                    self.stopped = true;
                    return;
                }
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                if kind.is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !SKIPPED.contains(&name.as_str()) || self.entered.contains(&name.as_str()) {
                        dirs.push(path);
                    }
                } else if kind.is_file() && !self.credential(&path) {
                    let Ok(meta) = entry.metadata() else { continue };
                    if !each(&path, &meta) {
                        return;
                    }
                }
            }
            // Popped last-in first-out: pushed reversed, the directories run in name order.
            stack.extend(dirs.into_iter().rev());
        }
    }

    fn credential(&self, full: &Path) -> bool {
        credential_named(&relative(&self.root, full).unwrap_or_else(|| full.to_string_lossy().into_owned()))
    }

    /// What the answer adds when the walk ran out of budget before the tree did.
    fn note(&self) -> String {
        if self.stopped {
            format!("\n(the walk stopped at its budget of {WALK_ENTRIES} entries or {} s; narrow path)", WALK_TIME.as_secs())
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("swiss-llm-search-{}", swiss_core::util::random_hex(8)));
        for (path, text) in [
            ("src/lib.rs", "pub fn alpha() {}\nfn beta() {}\n"),
            ("src/deep/mod.rs", "fn alpha_two() {}\n"),
            ("src/app.ts", "const alpha = 1;\n"),
            ("README.md", "# Alpha\nalpha notes\n"),
            ("node_modules/pkg/index.js", "alpha in a dependency\n"),
            ("target/debug/out.rs", "fn alpha() {}\n"),
            (".env", "ALPHA_KEY=alpha\n"),
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
            std::fs::write(full, text).expect("write");
        }
        std::fs::write(root.join("blob.bin"), b"alpha\0binary").expect("write");
        root
    }

    #[test]
    fn a_glob_reads_as_an_anchored_path_regex() {
        let matches = |glob: &str, path: &str| glob_regex(glob).expect("glob").is_match(path);
        assert!(matches("**/*.rs", "lib.rs") && matches("**/*.rs", "src/deep/mod.rs"));
        assert!(matches("*.rs", "lib.rs") && !matches("*.rs", "src/lib.rs"), "* stays within one name");
        assert!(matches("src/**", "src/a/b.txt") && !matches("src/**", "other/a.txt"));
        assert!(matches("src/*.{rs,ts}", "src/app.ts") && !matches("src/*.{rs,ts}", "src/app.js"));
        assert!(matches("file?.txt", "file1.txt") && !matches("file?.txt", "file10.txt"));
        assert!(matches("[ab].md", "a.md") && matches("[!ab].md", "c.md") && !matches("[!ab].md", "a.md"));
        assert!(matches("a+b(c).txt", "a+b(c).txt"), "regex metacharacters are literal");
        assert!(glob_regex("{a,b").is_err());
    }

    #[test]
    fn glob_finds_files_newest_first_and_skips_dependency_trees() {
        let root = tree();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("src/deep/mod.rs"), "fn alpha_two() {}\n").expect("touch");
        let found = glob_tool(&root, &json!({"pattern": "**/*.rs"}));
        assert_eq!(found, "src/deep/mod.rs\nsrc/lib.rs", "newest first, target skipped");
        assert_eq!(glob_tool(&root, &json!({"pattern": "*.rs", "path": "src"})), "src/lib.rs", "relative to path, shown from the root");
        assert_eq!(glob_tool(&root, &json!({"pattern": "target/**/*.rs"})), "target/debug/out.rs", "a named tree is entered");
        assert!(glob_tool(&root, &json!({"pattern": "*.rs"})).contains("**/*.rs searches below it"));
        assert!(!glob_tool(&root, &json!({"pattern": "**/*"})).contains(".env"), "a credential file is never named");
        assert!(glob_tool(&root, &json!({"pattern": "*", "path": ".."})).contains("outside the root"));
    }

    #[test]
    fn grep_lists_shows_and_counts_matches_skipping_what_it_must() {
        let root = tree();
        let files = grep_tool(&root, &json!({"pattern": "alpha"}));
        let mut listed: Vec<&str> = files.lines().collect();
        listed.sort();
        assert_eq!(listed, ["README.md", "src/app.ts", "src/deep/mod.rs", "src/lib.rs"], "no binary, credential or dependency file");
        let content = grep_tool(&root, &json!({"pattern": "^fn", "path": "src/lib.rs", "output_mode": "content"}));
        assert_eq!(content, "src/lib.rs:2:fn beta() {}");
        let around = grep_tool(&root, &json!({"pattern": "beta", "path": "src", "output_mode": "content", "context": 1}));
        assert_eq!(around, "src/lib.rs-1-pub fn alpha() {}\nsrc/lib.rs:2:fn beta() {}");
        let counted = grep_tool(&root, &json!({"pattern": "alpha", "glob": "*.md", "output_mode": "count", "ignore_case": true}));
        assert_eq!(counted, "README.md:2\n(2 matching lines in 1 files)");
        let only_src = grep_tool(&root, &json!({"pattern": "alpha", "glob": "src/**/*.rs"}));
        assert_eq!(only_src.lines().count(), 2, "{only_src}");
        assert!(grep_tool(&root, &json!({"pattern": "("})).starts_with("error:"));
        assert!(grep_tool(&root, &json!({"pattern": "zzz"})).starts_with("no matches"));
        let capped = grep_tool(&root, &json!({"pattern": "alpha", "output_mode": "content", "head_limit": 1}));
        assert!(capped.contains("cut at the limit"), "{capped}");
    }

    #[test]
    fn grep_redacts_what_it_shows() {
        let root = tree();
        std::fs::write(root.join("leak.txt"), "plain line\nkey: random AbCdEfGhIjKlMnOpQrStUv1234 word\n").expect("write");
        let shown = grep_tool(&root, &json!({"pattern": "key", "path": "leak.txt", "output_mode": "content"}));
        assert_eq!(shown, "leak.txt:2:key: random [redacted:mixed_token] word");
    }
}
