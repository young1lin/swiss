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

//! What one file says about code (SPEC §memory.code): its **definitions** (a declaration
//! keyword and the name after it, line by line - the ctags way, no grammar), its
//! **imports** (the module or path a `use`, `import`, `require` or `#include` names) and
//! the code-like names it **references** anywhere in its text.
//!
//! No parser: tree-sitter with the grammars this index needs first (TypeScript, TSX,
//! Rust, Python) adds 4.5 MB to a 13.6 MB binary, a third, for definitions a keyword scan
//! finds on the lines that declare them. What a scan misses - a destructured binding, a
//! grouped Go `const (...)`, a C function - is simply not a definition here; the
//! references still hold the name, so a check never calls it absent.

use std::collections::HashSet;

use crate::search::is_code;

/// A name is 3 to this many characters.
pub const NAME_MAX: usize = 64;
/// Distinct names one file references, at most.
pub const REFS_MAX: usize = 5_000;
/// Definitions and imports one file yields, at most each.
pub const DEFS_MAX: usize = 2_000;
/// An import spec is cut at this many characters.
const SPEC_MAX: usize = 200;
/// What the scan reads out of a file: a change here leaves every stored fact stale, so the
/// index reads each file again (SPEC §memory.code Refresh). 2: Java-family methods. 3: a
/// redacted span keeps its lines, so a definition past a private key is at its own line.
/// 4: a name only a comment holds is told from one the code uses.
pub const EXTRACTOR: i64 = 4;

/// A family of languages that declare alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    /// TypeScript and JavaScript.
    Script,
    Python,
    Go,
    /// Java, Kotlin, Scala, C#.
    Java,
    Ruby,
    Php,
    Shell,
    PowerShell,
    /// C and C++: includes only, no definitions.
    C,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Script => "script",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Ruby => "ruby",
            Lang::Php => "php",
            Lang::Shell => "shell",
            Lang::PowerShell => "powershell",
            Lang::C => "c",
        }
    }

    /// By extension; a file of no listed language is read for its references only.
    pub fn of(path: &str) -> Option<Lang> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        Some(match ext.as_str() {
            "rs" => Lang::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Lang::Script,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            "java" | "kt" | "kts" | "scala" | "cs" => Lang::Java,
            "rb" => Lang::Ruby,
            "php" => Lang::Php,
            "sh" | "bash" | "zsh" => Lang::Shell,
            "ps1" | "psm1" => Lang::PowerShell,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => Lang::C,
            _ => return None,
        })
    }

    /// Words that may precede a declaration keyword (TypeScript class members: [MEMBER_MODIFIERS]).
    fn modifiers(&self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &["pub", "async", "unsafe", "extern", "default"],
            Lang::Script => &["export", "default", "declare", "abstract", "async"],
            Lang::Python => &["async"],
            Lang::Java => &[
                "public", "private", "protected", "internal", "static", "final", "abstract", "sealed", "open", "data",
                "inline", "override", "suspend", "partial", "readonly", "case", "enum", "annotation", "value",
                "synchronized", "native", "default", "strictfp", "transient", "volatile", "virtual", "async", "extern",
                "unsafe",
            ],
            Lang::Php => &["abstract", "final", "public", "private", "protected", "static", "readonly"],
            _ => &[],
        }
    }

    /// Declaration keywords and the kind of definition each makes.
    fn declarations(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::Rust => &[
                ("fn", "fn"),
                ("struct", "struct"),
                ("enum", "enum"),
                ("trait", "trait"),
                ("type", "type"),
                ("mod", "mod"),
                ("const", "const"),
                ("static", "static"),
                ("union", "union"),
                ("macro_rules!", "macro"),
            ],
            Lang::Script => &[
                ("function", "fn"),
                ("function*", "fn"),
                ("class", "class"),
                ("interface", "interface"),
                ("type", "type"),
                ("enum", "enum"),
                ("const", "const"),
                ("let", "var"),
                ("var", "var"),
                ("namespace", "namespace"),
            ],
            Lang::Python => &[("def", "fn"), ("class", "class")],
            Lang::Go => &[("func", "fn"), ("type", "type"), ("const", "const"), ("var", "var")],
            Lang::Java => &[
                ("class", "class"),
                ("interface", "interface"),
                ("enum", "enum"),
                ("record", "record"),
                ("struct", "struct"),
                ("object", "object"),
                ("trait", "trait"),
                ("fun", "fn"),
                ("def", "fn"),
            ],
            Lang::Ruby => &[("def", "fn"), ("class", "class"), ("module", "mod")],
            Lang::Php => &[("function", "fn"), ("class", "class"), ("interface", "interface"), ("trait", "trait")],
            Lang::Shell | Lang::PowerShell => &[("function", "fn")],
            Lang::C => &[],
        }
    }
}

/// One definition: a name, its kind, its 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
}

/// One import: the module or path named, its 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub spec: String,
    pub line: u32,
}

/// What one file says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub defs: Vec<Def>,
    pub imports: Vec<Import>,
    /// The code-like names in the text, each once, in first-seen order; in a file of a
    /// language, those its code holds outside comments.
    pub refs: Vec<String>,
    /// The code-like names only a comment of a language's file holds: a mention, not a use.
    pub comment_refs: Vec<String>,
}

/// Words a method heuristic must never read as a name.
const NOT_A_METHOD: [&str; 18] = [
    "if", "for", "while", "switch", "catch", "return", "function", "else", "do", "try", "with", "new", "typeof",
    "await", "yield", "super", "this", "constructor",
];

/// Statement words a Java-family method's return type is never: `return foo(x);`,
/// `throw new E(…)`, `else run(…)`, Kotlin's `is T -> f(x)`.
const NOT_A_TYPE: [&str; 14] =
    ["return", "throw", "new", "else", "case", "yield", "await", "assert", "goto", "do", "in", "is", "as", "using"];

/// The facts of one text file at `path`.
pub fn extract(path: &str, text: &str) -> Facts {
    let Some(lang) = Lang::of(path) else {
        return Facts { refs: references(text), ..Facts::default() };
    };
    // A file that names a thing only in a comment does not use it (SPEC §memory.code Find).
    let refs = references(&uncommented(lang, text));
    let in_code: HashSet<&str> = refs.iter().map(String::as_str).collect();
    let comment_refs: Vec<String> = references(text)
        .into_iter()
        .filter(|name| !in_code.contains(name.as_str()))
        .take(REFS_MAX.saturating_sub(refs.len()))
        .collect();
    let mut facts = Facts { refs, comment_refs, ..Facts::default() };
    let mut in_go_imports = false;
    for (index, line) in text.lines().enumerate() {
        let number = index as u32 + 1;
        if facts.imports.len() < DEFS_MAX {
            if let Some(spec) = import_of(lang, line, &mut in_go_imports) {
                let spec: String = spec.chars().take(SPEC_MAX).collect();
                if !spec.is_empty() && !facts.imports.iter().any(|i| i.spec == spec) {
                    facts.imports.push(Import { spec, line: number });
                }
            }
        }
        if facts.defs.len() < DEFS_MAX {
            if let Some((name, kind)) = definition(lang, line) {
                facts.defs.push(Def { name, kind, line: number });
            }
        }
    }
    facts
}

/// The code-like names in `text` ([is_code]: an inner `_`, an inner capital), each once.
pub fn references(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for word in words(text) {
        if out.len() >= REFS_MAX {
            break;
        }
        if word.len() >= 3 && word.len() <= NAME_MAX && is_code(word) && seen.insert(word) {
            out.push(word.to_string());
        }
    }
    out
}

/// Identifier tokens: an ASCII letter, `_` or `$`, then letters, digits, `_` or `$`.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut at = 0;
    std::iter::from_fn(move || {
        while at < bytes.len() {
            let b = bytes[at];
            if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
                // A word glued to a digit on its left (`9abc`) is not one; one against
                // CJK text (`调用fitRailLabels时`) is.
                let glued = at > 0 && bytes[at - 1].is_ascii_digit();
                let start = at;
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'$') {
                    at += 1;
                }
                if !glued {
                    return Some(&text[start..at]);
                }
            } else {
                at += 1;
            }
        }
        None
    })
}

/// `text` with its comments blanked: each byte of a comment a space, its newlines kept, its
/// strings as they are (a key in a string literal is code). A lexer this small misreads the
/// odd raw string or regex; what it cannot place stays code, as every mention once was.
fn uncommented(lang: Lang, text: &str) -> String {
    let b = text.as_bytes();
    let slashes = !matches!(lang, Lang::Python | Lang::Ruby | Lang::Shell | Lang::PowerShell);
    let mut out = b.to_vec();
    let mut at = 0;
    while at < b.len() {
        let rest = &b[at..];
        let comment_end = if (slashes && rest.starts_with(b"//")) || hash_comment(lang, b, at) {
            Some(line_end(b, at))
        } else if slashes && rest.starts_with(b"/*") {
            Some(past(b, at + 2, b"*/"))
        } else if lang == Lang::PowerShell && rest.starts_with(b"<#") {
            Some(past(b, at + 2, b"#>"))
        } else if lang == Lang::Ruby && rest.starts_with(b"=begin") && (at == 0 || b[at - 1] == b'\n') {
            Some(line_end(b, past(b, at, b"\n=end")))
        } else {
            None
        };
        match comment_end {
            Some(end) => {
                for byte in &mut out[at..end] {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
                at = end;
            }
            None => at = past_literal(lang, b, at),
        }
    }
    // Every blanked span starts and ends at an ASCII byte, so the bytes stay UTF-8; were
    // they not, the text as it is reads every mention as code.
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// Whether a `#` at `at` opens a line comment in `lang`.
fn hash_comment(lang: Lang, b: &[u8], at: usize) -> bool {
    if b[at] != b'#' {
        return false;
    }
    match lang {
        Lang::Python | Lang::Ruby => true,
        // `#[Attribute]` is PHP 8's attribute.
        Lang::Php => b.get(at + 1) != Some(&b'['),
        // `$#` and `${#name}` are not comments: a comment starts a word.
        Lang::Shell | Lang::PowerShell => at == 0 || matches!(b[at - 1], b' ' | b'\t' | b'\r' | b'\n' | b';' | b'(' | b'|' | b'&'),
        _ => false,
    }
}

/// The index of the newline at or after `from`, or the end.
fn line_end(b: &[u8], from: usize) -> usize {
    b[from.min(b.len())..].iter().position(|c| *c == b'\n').map_or(b.len(), |i| from + i)
}

/// The index just past the first `mark` at or after `from`, or the end.
fn past(b: &[u8], from: usize, mark: &[u8]) -> usize {
    b[from.min(b.len())..].windows(mark.len()).position(|w| w == mark).map_or(b.len(), |i| from + i + mark.len())
}

/// Past the string or character literal that opens at `at`, or one byte on when none does.
fn past_literal(lang: Lang, b: &[u8], at: usize) -> usize {
    let quote = b[at];
    match quote {
        // PowerShell's escape: the byte after it is plain.
        b'`' if lang == Lang::PowerShell => at + 2,
        b'`' if lang == Lang::Script => close(b, at + 1, quote, Some(b'\\'), true),
        b'`' if lang == Lang::Go => close(b, at + 1, quote, None, true),
        b'"' | b'\'' if matches!(lang, Lang::Python | Lang::Java) && b[at..].starts_with(&[quote; 3]) => past(b, at + 3, &[quote; 3]),
        b'\'' if matches!(lang, Lang::Rust | Lang::Go | Lang::Java | Lang::C) => char_end(b, at).unwrap_or(at + 1),
        b'"' | b'\'' => {
            let spans = matches!(lang, Lang::Rust | Lang::Ruby | Lang::Php | Lang::Shell | Lang::PowerShell);
            let escape = match (lang, quote) {
                (Lang::Shell | Lang::PowerShell, b'\'') => None,
                (Lang::PowerShell, _) => Some(b'`'),
                _ => Some(b'\\'),
            };
            close(b, at + 1, quote, escape, spans)
        }
        _ => at + 1,
    }
}

/// Past the `quote` closing a literal whose body starts at `from`: `escape` makes the byte
/// after it plain, and a newline ends a literal that does not `span` lines.
fn close(b: &[u8], from: usize, quote: u8, escape: Option<u8>, spans: bool) -> usize {
    let mut at = from;
    while at < b.len() {
        match b[at] {
            c if Some(c) == escape => at += 2,
            b'\n' if !spans => return at,
            c if c == quote => return at + 1,
            _ => at += 1,
        }
    }
    b.len()
}

/// Past a character literal at `at` (`'x'`, `'é'`, `'\n'`, `'\u{1F600}'`); None for a
/// quote that opens none, such as a Rust lifetime (`'a`).
fn char_end(b: &[u8], at: usize) -> Option<usize> {
    let body = b.get(at + 1..)?;
    if body.first() == Some(&b'\\') {
        let close = body.iter().skip(2).take(10).position(|c| *c == b'\'' || *c == b'\n')?;
        return (body[2 + close] == b'\'').then_some(at + 1 + 2 + close + 1);
    }
    let width = match *body.first()? {
        c if c < 0x80 => 1,
        c if c >= 0xF0 => 4,
        c if c >= 0xE0 => 3,
        _ => 2,
    };
    (body.get(width) == Some(&b'\'')).then_some(at + 1 + width + 1)
}

/// Whether `word` is a name: 3 to [NAME_MAX] identifier characters, not starting with a digit.
fn is_name(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && word.len() <= NAME_MAX
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '-')
}

/// The leading identifier of `s` (PowerShell's may hold `-`).
fn leading_name(s: &str, dashes: bool) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '$' || (dashes && *c == '-')))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// The first whitespace-separated word of `s` and the rest after it.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(|c: char| c.is_whitespace() || c == '(' || c == '<' || c == '{' || c == ':' || c == '=').unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Modifiers a class member may carry in TypeScript and JavaScript.
const MEMBER_MODIFIERS: [&str; 10] =
    ["public", "private", "protected", "static", "readonly", "override", "get", "set", "async", "abstract"];

/// The definition one line makes, if any.
fn definition(lang: Lang, line: &str) -> Option<(String, &'static str)> {
    let indented = line.starts_with([' ', '\t']);
    let mut rest = line.trim_start();
    if rest.starts_with("//") || rest.starts_with('*') || rest.starts_with('#') {
        return None;
    }
    let declares = |word: &str| lang.declarations().iter().find(|(k, _)| *k == word).map(|(_, kind)| *kind);
    loop {
        let (word, after) = split_word(rest);
        if lang == Lang::Java {
            // An annotation before a member (`@Override`, `@Scheduled(cron = "…")`) and a
            // generic method's type parameters (`<T>`) come before its return type; an
            // annotation type (`@interface`) declares like an interface.
            let at = rest.trim_start();
            if let Some(named) = at.strip_prefix('@') {
                rest = if named.starts_with("interface") { named } else { past_annotation(named)? };
                continue;
            }
            if at.starts_with('<') {
                rest = past_generic(at)?;
                continue;
            }
        }
        if word.is_empty() {
            return None;
        }
        // Rust's `pub(crate)` and `extern "C"`.
        if lang == Lang::Rust && word == "pub" && after.starts_with('(') {
            rest = after.split_once(')')?.1;
            continue;
        }
        if lang == Lang::Rust && word == "extern" && after.trim_start().starts_with('"') {
            rest = after.trim_start()[1..].split_once('"')?.1;
            continue;
        }
        let next = split_word(after).0;
        // A word both modifier and keyword (Kotlin's `enum class`, Rust's `const fn`,
        // TypeScript's `const enum`) modifies only before another keyword or modifier.
        let modifier = lang.modifiers().contains(&word)
            || matches!(word, "const" | "static") && matches!(next, "fn" | "unsafe" | "async" | "extern" | "function" | "enum")
            || lang == Lang::Script && indented && MEMBER_MODIFIERS.contains(&word) && !next.is_empty();
        if modifier && (declares(word).is_none() || declares(next).is_some() || lang.modifiers().contains(&next)) {
            rest = after;
            continue;
        }
        if let Some(kind) = declares(word) {
            return declared(lang, kind, after);
        }
        // `name() {` in a shell script.
        if lang == Lang::Shell && !indented && after.trim_start().starts_with("()") && is_name(word) {
            return Some((word.to_string(), "fn"));
        }
        // An indented `name(...) {` in a class or object body.
        if lang == Lang::Script && indented {
            return method(word, after);
        }
        // An indented `Type name(...)` in a Java-family class or interface body.
        if lang == Lang::Java && indented {
            return java_method(rest);
        }
        // A module-level SCREAMING constant in Python.
        let screaming = word.len() >= 3 && word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        let assigned = after.trim_start().starts_with('=') && !after.trim_start().starts_with("==");
        if lang == Lang::Python && !indented && screaming && assigned {
            return Some((word.to_string(), "const"));
        }
        return None;
    }
}

/// The name after a declaration keyword.
fn declared(lang: Lang, kind: &'static str, after: &str) -> Option<(String, &'static str)> {
    let mut rest = after.trim_start();
    // `function* gen`, Ruby's `def self.name`, Go's `func (r *T) Name`.
    rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
    if lang == Lang::Ruby {
        rest = rest.strip_prefix("self.").unwrap_or(rest);
    }
    if lang == Lang::Go && kind == "fn" && rest.starts_with('(') {
        rest = rest.split_once(')')?.1.trim_start();
    }
    // Kotlin's `fun <T> name` and `fun Receiver.name`, `fun List<T>?.name`.
    if lang == Lang::Java && kind == "fn" {
        if rest.starts_with('<') {
            rest = past_generic(rest)?.trim_start();
        }
        loop {
            let head = leading_name(rest, false);
            let tail = &rest[head.len()..];
            let tail = if tail.starts_with('<') { past_generic(tail)? } else { tail };
            let tail = tail.strip_prefix('?').unwrap_or(tail);
            match tail.strip_prefix('.') {
                Some(member) if !head.is_empty() => rest = member,
                _ => break,
            }
        }
    }
    let name = leading_name(rest, lang == Lang::PowerShell);
    (name.len() >= 2 && is_name(name)).then(|| (name.to_string(), kind))
}

/// `name<T>(args): R {` - an indented line opening a method body, not a call.
fn method(word: &str, after: &str) -> Option<(String, &'static str)> {
    if !is_name(word) || NOT_A_METHOD.contains(&word) || word.contains('-') {
        return None;
    }
    let rest = after.trim_start();
    let rest = match rest.strip_prefix('<') {
        Some(generic) => generic.split_once('>')?.1.trim_start(),
        None => rest,
    };
    let args = rest.strip_prefix('(')?;
    let tail = args[args.rfind(')')? + 1..].trim();
    let opens = match tail.strip_prefix(':') {
        Some(ret) => ret.ends_with('{') && !ret.contains("=>"),
        None => tail == "{",
    };
    opens.then(|| (word.to_string(), "method"))
}

/// The text after the balanced `open`…`close` run `s` starts with (quotes skipped), None
/// when the line ends inside it.
fn past_balanced(s: &str, open: u8, close: u8) -> Option<&str> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (at, b) in s.bytes().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            _ if b == open => depth += 1,
            _ if b == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&s[at + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The text after a generic's `<…>` (`s` starts with `<`).
fn past_generic(s: &str) -> Option<&str> {
    past_balanced(s, b'<', b'>')
}

/// The text after an annotation's name and arguments (`s` is past its `@`).
fn past_annotation(s: &str) -> Option<&str> {
    let name = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.')).unwrap_or(s.len());
    if name == 0 {
        return None;
    }
    let rest = &s[name..];
    match rest.trim_start().starts_with('(') {
        true => past_balanced(rest.trim_start(), b'(', b')'),
        false => Some(rest),
    }
}

/// `Type name(args) {` - a Java-family method: a return type (dotted, generic, an array, C#'s
/// `?`), a name, its arguments, and a body, a `;` (an interface's, an abstract one), a
/// `throws`, C#'s `where` or `=>`, an annotation member's `default`, or nothing (the brace on
/// the next line, the arguments running on). A call (`run(x);`, `a.b(x)`), an assignment
/// (`T x = f(x)`) and a constructor (no return type) are none.
fn java_method(rest: &str) -> Option<(String, &'static str)> {
    let rest = rest.trim_start();
    let ty = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.')).unwrap_or(rest.len());
    let word = &rest[..ty];
    if ty == 0 || !is_name(word.split('.').next_back().unwrap_or(word)) || NOT_A_TYPE.contains(&word) || NOT_A_METHOD.contains(&word) {
        return None;
    }
    let mut after = &rest[ty..];
    if after.starts_with('<') {
        after = past_generic(after)?;
    }
    after = after.strip_prefix('?').unwrap_or(after);
    while let Some(dims) = after.trim_start().strip_prefix("[]") {
        after = dims;
    }
    let named = after.trim_start();
    if named.len() == after.len() {
        return None;
    }
    let name = leading_name(named, false);
    if name.len() < 2 || !is_name(name) || NOT_A_METHOD.contains(&name) || NOT_A_TYPE.contains(&name) {
        return None;
    }
    let mut tail = named[name.len()..].trim_start();
    if tail.starts_with('<') {
        tail = past_generic(tail)?.trim_start();
    }
    if !tail.starts_with('(') {
        return None;
    }
    let Some(after_args) = past_balanced(tail, b'(', b')') else {
        // The arguments run on to the next line.
        return Some((name.to_string(), "method"));
    };
    let end = after_args.trim();
    let opens = end.is_empty()
        || end.starts_with('{')
        || end == ";"
        || end.starts_with("=>")
        || ["throws", "where", "default"].iter().any(|word| end.strip_prefix(word).is_some_and(|r| r.starts_with([' ', '\t'])));
    opens.then(|| (name.to_string(), "method"))
}

/// The import one line makes, if any.
fn import_of(lang: Lang, line: &str, in_go_block: &mut bool) -> Option<String> {
    let t = line.trim();
    let quoted = |s: &str| -> Option<String> {
        let open = s.find(['\'', '"', '`'])?;
        let quote = s[open..].chars().next()?;
        let body = &s[open + 1..];
        body.find(quote).map(|end| body[..end].to_string())
    };
    match lang {
        Lang::Rust => {
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let t = if t.starts_with("pub(") { t.split_once(')').map(|(_, r)| r.trim_start())? } else { t };
            if let Some(path) = t.strip_prefix("use ") {
                return Some(path.trim_end_matches(';').trim().to_string());
            }
            t.strip_prefix("extern crate ").map(|r| r.trim_end_matches(';').trim().to_string())
        }
        Lang::Script => {
            let from = t.starts_with("import ") || t.starts_with("import{") || (t.starts_with("export ") && t.contains(" from "));
            if from {
                return match t.rfind(" from ") {
                    Some(at) => quoted(&t[at..]),
                    None => quoted(t),
                };
            }
            ["require(", "import("].iter().find_map(|call| t.find(call).and_then(|at| quoted(&t[at..])))
        }
        Lang::Python => {
            if let Some(rest) = t.strip_prefix("from ") {
                return rest.split_whitespace().next().map(str::to_string);
            }
            t.strip_prefix("import ").and_then(|r| r.split([',', ' ']).find(|s| !s.is_empty()).map(str::to_string))
        }
        Lang::Go => {
            if *in_go_block {
                if t.starts_with(')') {
                    *in_go_block = false;
                    return None;
                }
                return quoted(t);
            }
            let rest = t.strip_prefix("import")?;
            if rest.trim_start().starts_with('(') {
                *in_go_block = true;
                return None;
            }
            quoted(rest)
        }
        Lang::Java => t.strip_prefix("import ").map(|r| {
            let r = r.strip_prefix("static ").unwrap_or(r);
            r.trim_end_matches(';').trim().to_string()
        }),
        Lang::Ruby => ["require_relative ", "require "].iter().find_map(|k| t.strip_prefix(k).and_then(quoted)),
        Lang::Php => {
            if let Some(r) = t.strip_prefix("use ") {
                return Some(r.trim_end_matches(';').trim().to_string());
            }
            ["require_once", "include_once", "require", "include"].iter().find_map(|k| t.strip_prefix(k).and_then(quoted))
        }
        Lang::Shell => ["source ", ". "].iter().find_map(|k| t.strip_prefix(k).map(|r| r.trim().trim_matches(['"', '\'']).to_string())),
        Lang::PowerShell => {
            let lower = t.to_ascii_lowercase();
            if lower.starts_with("import-module ") {
                return t.split_whitespace().nth(1).map(|s| s.trim_matches(['"', '\'']).to_string());
            }
            t.strip_prefix(". ").map(|r| r.trim().trim_matches(['"', '\'']).to_string())
        }
        Lang::C => {
            let r = t.strip_prefix('#')?.trim_start().strip_prefix("include")?.trim();
            let inner = r.strip_prefix('<').and_then(|s| s.split_once('>')).map(|(s, _)| s.to_string());
            inner.or_else(|| quoted(r))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(path: &str, text: &str) -> Vec<(String, &'static str)> {
        extract(path, text).defs.into_iter().map(|d| (d.name, d.kind)).collect()
    }

    fn names<'a>(found: &'a [(String, &'static str)]) -> Vec<&'a str> {
        found.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn rust_declarations_and_uses() {
        let text = "use crate::store::Store;\npub(crate) mod fresh;\npub struct Checker {\n    budget: usize,\n}\n\
                    impl Checker {\n    pub const fn new() -> Checker { todo!() }\n    pub(crate) async fn probe(&self) {}\n}\n\
                    const CHECK_CAP: usize = 50;\nextern \"C\" fn raw() {}\nmacro_rules! vocabulary { () => {} }\n\
                    // fn commented() {}\nlet x = fn_like();\n";
        let found = defs("src/fresh.rs", text);
        assert_eq!(names(&found), ["fresh", "Checker", "new", "probe", "CHECK_CAP", "raw", "vocabulary"]);
        let kinds: Vec<&str> = found.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, ["mod", "struct", "fn", "fn", "const", "fn", "macro"]);
        let imports: Vec<String> = extract("src/fresh.rs", text).imports.into_iter().map(|i| i.spec).collect();
        assert_eq!(imports, ["crate::store::Store"]);
    }

    #[test]
    fn script_declarations_methods_and_imports() {
        let text = "import { tr } from \"../i18n\";\nimport './side-effect.css';\nexport * from './types';\n\
                    const fs = require('node:fs');\nexport default async function fitRailLabels(rail: HTMLElement) {\n}\n\
                    export const wireLabel = (s: string) => s;\nexport interface SaveReply { ok: boolean }\n\
                    export type Mode = 'a' | 'b';\nclass Rail {\n  private readonly seats = new Map();\n  \
                    async measure(seat: Seat): Promise<number> {\n    if (seat) {\n      doThing(seat);\n    }\n  }\n  \
                    get width() {\n  }\n}\nconst { a, b } = pair;\ndescribe('rail', () => {\n";
        let found = defs("panel/src/rail.ts", text);
        assert_eq!(names(&found), ["fs", "fitRailLabels", "wireLabel", "SaveReply", "Mode", "Rail", "measure", "width"]);
        assert_eq!(found[1].1, "fn");
        assert_eq!(found[5].1, "class");
        assert_eq!(found[6].1, "method");
        let imports: Vec<String> = extract("panel/src/rail.ts", text).imports.into_iter().map(|i| i.spec).collect();
        assert_eq!(imports, ["../i18n", "./side-effect.css", "./types", "node:fs"]);
    }

    #[test]
    fn python_go_and_shells() {
        let py = "from swiss.store import Store\nimport os, sys\nMAX_RETRIES = 3\nclass Pool:\n    async def take(self):\n        pass\n\
                  def helper():\n    LOCAL = 1\n";
        assert_eq!(names(&defs("tools/pool.py", py)), ["MAX_RETRIES", "Pool", "take", "helper"]);
        let imports: Vec<String> = extract("tools/pool.py", py).imports.into_iter().map(|i| i.spec).collect();
        assert_eq!(imports, ["swiss.store", "os"]);

        let go = "package lanes\nimport (\n\t\"context\"\n\tlog \"github.com/x/log\"\n)\nfunc (l *Lane) Take(ctx context.Context) error {\n}\n\
                  type Lane struct{}\nconst Max = 8\n";
        assert_eq!(names(&defs("lanes/lane.go", go)), ["Take", "Lane", "Max"]);
        let specs = |path: &str, text: &str| -> Vec<String> { extract(path, text).imports.into_iter().map(|i| i.spec).collect() };
        assert_eq!(specs("lanes/lane.go", go), ["context", "github.com/x/log"]);

        assert_eq!(names(&defs("scripts/a.sh", "deploy() {\n  run\n}\nfunction check {\n}\n")), ["deploy", "check"]);
        assert_eq!(names(&defs("scripts/a.ps1", "function Stop-ByPid {\n}\n. ./common.ps1\n")), ["Stop-ByPid"]);
        assert_eq!(specs("scripts/a.ps1", ". ./common.ps1\nImport-Module Pester\n"), ["./common.ps1", "Pester"]);
    }

    #[test]
    fn java_family_methods_are_definitions_their_calls_are_not() {
        let java = "package com.example.shop.job;\n\
                    @Component\n\
                    public class OrderReviewCheckJob {\n\
                    \x20   private static final int DAYS = 30;\n\
                    \x20   private final NotificationBusinessPublisher publisher;\n\
                    \x20   public OrderReviewCheckJob(NotificationBusinessPublisher publisher) {\n\
                    \x20   @Scheduled(cron = \"0 0 3 * * ?\")\n\
                    \x20   public void checkStaleOrders() {\n\
                    \x20       List<OrderPO> copies = mapper.selectActive(DAYS);\n\
                    \x20       for (OrderPO copy : copies) {\n\
                    \x20           if (idle(copy)) pause(copy);\n\
                    \x20           sendStaleNotice(copy);\n\
                    \x20           this.publisher.publish(copy);\n\
                    \x20           return compute(copy);\n\
                    \x20           throw new IllegalStateException(\"x\");\n\
                    \x20       } else if (x) {\n\
                    \x20       String text = format(copy);\n\
                    \x20   @Override public String toString() { return name; }\n\
                    \x20   private void publishStaleNotice(OrderPO copy) {\n\
                    \x20   public static <T extends Base> Map<String, List<T>> group(List<T> items) throws IOException {\n\
                    \x20   protected abstract int[] counts(@Param(\"days\") int days);\n\
                    \x20   synchronized java.util.Optional<Long> latest(Long id,\n\
                    \x20                                                Long before) {\n\
                    \x20   String value() default \"\";\n\
                    \x20   // private void commented(Foo foo) {\n\
                    \x20    * @see #sendStaleNotice(OrderPO)\n\
                    \x20   ACTIVE(\"a\"),\n\
                    }\n\
                    public @interface Audited {\n";
        let found = defs("src/main/java/com/example/shop/job/OrderReviewCheckJob.java", java);
        assert_eq!(
            names(&found),
            ["OrderReviewCheckJob", "checkStaleOrders", "toString", "publishStaleNotice", "group", "counts", "latest", "value", "Audited"]
        );
        let kinds: Vec<&str> = found.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, ["class", "method", "method", "method", "method", "method", "method", "method", "interface"]);
        assert_eq!(extract("src/Job.java", java).defs[3].line, 19, "the line it is declared on");

        let cs = "namespace Shop {\n    public sealed class Orders {\n        public async Task<IActionResult> Get(int id)\n        {\n\
                  \x20       public override string ToString() => $\"{Id}\";\n        public T Find<T>(int id) where T : class {\n\
                  \x20       using (var scope = Begin()) {\n            await Save(order);\n            yield return Next(order);\n";
        assert_eq!(names(&defs("src/Orders.cs", cs)), ["Orders", "Get", "ToString", "Find"]);

        let kt = "class Pause {\n    suspend fun <T> retry(block: () -> T): T {\n    fun String?.toSlug(): String = lower()\n\
                  \x20   private val repo by lazy(::open)\n    is Idle -> pause(copy)\n}\n";
        assert_eq!(names(&defs("src/Pause.kt", kt)), ["Pause", "retry", "toSlug"]);
    }

    #[test]
    fn references_are_the_code_like_names_once() {
        let refs = references("Store::write opens BEGIN IMMEDIATE; SQLITE_BUSY and retry_budget, fitRailLabels(x), \
                               retry_budget again, a plain word, 9abc_def, $scopeId, 调用wireLabel时");
        assert_eq!(refs, ["SQLITE_BUSY", "retry_budget", "fitRailLabels", "$scopeId", "wireLabel"]);
        // Markdown, JSON and config text is read the same way.
        assert_eq!(extract("docs/SPEC.md", "`memory_invalid_cwd` and `useState`").refs, ["memory_invalid_cwd", "useState"]);
        assert!(extract("docs/SPEC.md", "fn not_a_def() {}").defs.is_empty(), "a file of no language declares nothing");
    }

    #[test]
    fn a_name_only_a_comment_holds_is_a_mention_not_a_use() {
        let split = |path: &str, text: &str| {
            let facts = extract(path, text);
            (facts.refs, facts.comment_refs)
        };
        let java = "// Pushed by OrderReviewCheckJob through GoogleFirebaseSupport.\n/**\n * @see #sendStaleNotice\n */\n\
                    public class NotifySupport {\n    private static final String HOST = \"http://push.example/order_note_path\"; // send_side_note\n\
                    \x20   void send() { push(RISK_PAUSE_KEY); /* old_push_name */ char quote = '\"'; String text = \"/* in_a_string */\"; }\n}\n";
        assert_eq!(
            split("src/NotifySupport.java", java),
            (
                vec!["NotifySupport".to_string(), "order_note_path".into(), "RISK_PAUSE_KEY".into(), "in_a_string".into()],
                vec![
                    "OrderReviewCheckJob".to_string(),
                    "GoogleFirebaseSupport".into(),
                    "sendStaleNotice".into(),
                    "send_side_note".into(),
                    "old_push_name".into()
                ]
            ),
            "a string's `//` and `/*` are code; a char literal's quote opens no string"
        );
        let rust = "fn keep<'a>(s: &'a str) -> &'a str { s } // rust_note\nlet q = '\\''; // after_quote\nlet url = \"a//b\"; real_name();\n";
        assert_eq!(split("src/keep.rs", rust), (vec!["real_name".to_string()], vec!["rust_note".to_string(), "after_quote".into()]), "a lifetime is no literal");
        let python = "# load_config is read first\ndef run():\n    path = \"a#b_c\"  # trailing_note\n    return \"\"\"has # no_comment_here\"\"\"\n";
        assert_eq!(split("tools/run.py", python), (vec!["b_c".to_string(), "no_comment_here".into()], vec!["load_config".to_string(), "trailing_note".into()]));
        let shell = "echo $# ${#items_list}; # shell_note\n";
        assert_eq!(split("run.sh", shell), (vec!["items_list".to_string()], vec!["shell_note".to_string()]), "`$#` and `${{#` are not comments");
        let ps = "<# block_note #>\n$items_count = 1 # ps_note\nWrite-Host \"a`\"b # not_ps_note\"\n";
        assert_eq!(split("run.ps1", ps), (vec!["$items_count".to_string(), "not_ps_note".into()], vec!["block_note".to_string(), "ps_note".into()]));
        let ts = "const line = `// in_template ${x}`; // script_note\n";
        assert_eq!(split("panel/a.ts", ts), (vec!["in_template".to_string()], vec!["script_note".to_string()]));
        assert_eq!(split("docs/a.md", "// md_note"), (vec!["md_note".to_string()], Vec::new()), "a file of no language marks none");
    }

    #[test]
    fn languages_go_by_extension() {
        assert_eq!(Lang::of("a/b.rs"), Some(Lang::Rust));
        assert_eq!(Lang::of("a/b.TSX"), Some(Lang::Script));
        assert_eq!(Lang::of("a/b.kt"), Some(Lang::Java));
        assert_eq!(Lang::of("a/b.hpp"), Some(Lang::C));
        assert_eq!(Lang::of("Makefile"), None);
        assert_eq!(Lang::of("a.b/README"), None);
    }
}
