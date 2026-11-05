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

//! Where a secret's value comes from (SPEC §host.cli): `env:VAR`, `file:PATH` or
//! `dotenv:PATH#KEY`. The value is read here, in the caller's process, because the caller's
//! environment and files are not the daemon's.
//!
//! An error here never quotes what it was given to read. A source that makes no sense may be the
//! value itself typed in the wrong place, and a dotenv line that does not parse may hold one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::remote_cli::decode_text;

const THE_SOURCES: &str = "a source is env:VAR, file:PATH or dotenv:PATH#KEY";

#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Env(String),
    File(String),
    Dotenv { path: String, key: String },
}

impl Source {
    pub fn parse(spec: &str) -> Result<Source, String> {
        if let Some(var) = spec.strip_prefix("env:") {
            if var.is_empty() {
                return Err(format!("env: names no variable; {THE_SOURCES}"));
            }
            return Ok(Source::Env(var.to_string()));
        }
        if let Some(path) = spec.strip_prefix("file:") {
            if path.is_empty() {
                return Err(format!("file: names no path; {THE_SOURCES}"));
            }
            return Ok(Source::File(path.to_string()));
        }
        if let Some(rest) = spec.strip_prefix("dotenv:") {
            // The last `#`: a key has none, and a path may.
            return match rest.rsplit_once('#') {
                Some((path, key)) if !path.is_empty() && is_dotenv_key(key) => Ok(Source::Dotenv {
                    path: path.to_string(),
                    key: key.to_string(),
                }),
                _ => Err(format!("dotenv: names no path and key; {THE_SOURCES}")),
            };
        }
        Err(format!("the source is not one this build knows; {THE_SOURCES}"))
    }

    /// How a message names this source. It holds a name or a path, never a value.
    pub fn label(&self) -> String {
        match self {
            Source::Env(var) => format!("env:{var}"),
            Source::File(path) => format!("file:{path}"),
            Source::Dotenv { path, key } => format!("dotenv:{path}#{key}"),
        }
    }

    /// The text this source holds. A relative path resolves against `base`.
    pub fn read(&self, base: &Path) -> Result<String, String> {
        match self {
            Source::Env(var) => env_value(var, std::env::var_os(var)),
            Source::File(path) => read_text(&resolve(path, base), path),
            Source::Dotenv { path, key } => {
                let text = read_text(&resolve(path, base), path)?;
                match dotenv_value(&text, key) {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => Err(format!("{path} sets no {key}")),
                    Err(line) => Err(format!("{path}: line {line} is not KEY=VALUE")),
                }
            }
        }
    }
}

fn env_value(var: &str, raw: Option<OsString>) -> Result<String, String> {
    match raw {
        None => Err(format!("the environment variable {var} is not set in this process")),
        Some(raw) => raw
            .into_string()
            .map_err(|_| format!("the environment variable {var} is not text")),
    }
}

fn read_text(path: &Path, label: &str) -> Result<String, String> {
    let raw = std::fs::read(path).map_err(|err| format!("cannot read {label}: {err}"))?;
    decode_text(&raw).ok_or_else(|| format!("{label} is not text"))
}

/// `~/x` under the home directory, a relative path under `base`, an absolute one as it is.
fn resolve(path: &str, base: &Path) -> PathBuf {
    let path = match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => swiss_core::paths::home_dir().join(rest),
        None => PathBuf::from(path),
    };
    base.join(path)
}

/// One trailing line ending goes: what `echo`, a pipe and most editors add. Anything before it is
/// the value, so a key file keeps its inner lines.
pub fn trim_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

fn is_dotenv_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The value `wanted` has in a dotenv file, the last one when it is set twice, as the loaders have
/// it. The whole file must parse: a value read past a line this cannot make sense of could be the
/// wrong one. `Err` is the number of the first line that did not parse.
fn dotenv_value(text: &str, wanted: &str) -> Result<Option<String>, usize> {
    let mut found = None;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map_or(line, str::trim_start);
        let (key, raw) = line.split_once('=').ok_or(number)?;
        let key = key.trim_end();
        if !is_dotenv_key(key) {
            return Err(number);
        }
        let value = dotenv_text(raw).ok_or(number)?;
        if key == wanted {
            found = Some(value);
        }
    }
    Ok(found)
}

/// What stands right of the `=`. Single quotes are literal; double quotes take `\n`, `\r`, `\t`,
/// `\"` and `\\`, and any other backslash stays, so a Windows path survives. Unquoted, a `#` starts
/// a comment only after whitespace. Nothing interpolates: `$HOME` is six characters.
fn dotenv_text(raw: &str) -> Option<String> {
    let after_a_quote = |rest: &str| {
        let rest = rest.trim_start();
        rest.is_empty() || rest.starts_with('#')
    };
    let quoted = raw.trim_start();
    if let Some(body) = quoted.strip_prefix('\'') {
        let (value, rest) = body.split_once('\'')?;
        return after_a_quote(rest).then(|| value.to_string());
    }
    if let Some(body) = quoted.strip_prefix('"') {
        let mut value = String::new();
        let mut chars = body.chars();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    other => {
                        value.push('\\');
                        value.push(other);
                    }
                },
                other => value.push(other),
            }
        }
        return after_a_quote(chars.as_str()).then_some(value);
    }
    let end = raw
        .char_indices()
        .find(|&(at, c)| c == '#' && raw[..at].ends_with(char::is_whitespace))
        .map_or(raw.len(), |(at, _)| at);
    Some(raw[..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "sk-live-sentinel-7f3a";

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swiss-sources-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    // SPEC §host.cli: a source that makes no sense may be the value typed in the wrong place.
    #[test]
    fn an_unknown_source_is_refused_without_quoting_it() {
        for spec in [SENTINEL.to_string(), format!("vault:{SENTINEL}")] {
            let err = Source::parse(&spec).unwrap_err();
            assert!(!err.contains("sentinel"), "{err}");
            assert!(err.contains("env:VAR"), "says what to write: {err}");
        }
        for spec in ["env:", "file:", "dotenv:", "dotenv:.env", "dotenv:.env#", "dotenv:#KEY"] {
            assert!(Source::parse(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn a_source_parses_with_a_windows_path_and_a_hash_in_it() {
        assert_eq!(Source::parse("env:GITHUB_TOKEN"), Ok(Source::Env("GITHUB_TOKEN".into())));
        assert_eq!(Source::parse("file:C:\\keys\\a"), Ok(Source::File("C:\\keys\\a".into())));
        assert_eq!(
            Source::parse("dotenv:C:\\a#b\\.env#ZAI_KEY"),
            Ok(Source::Dotenv { path: "C:\\a#b\\.env".into(), key: "ZAI_KEY".into() })
        );
    }

    #[test]
    fn a_missing_variable_says_so_and_a_set_one_is_the_value() {
        assert_eq!(env_value("A", Some(OsString::from("v"))), Ok("v".to_string()));
        assert!(env_value("A_VAR", None).unwrap_err().contains("A_VAR"));
    }

    // SPEC §host.cli: a file may be UTF-16 or carry a BOM, and only one trailing newline goes.
    #[test]
    fn a_file_source_decodes_what_windows_writes() {
        let dir = scratch();
        std::fs::write(dir.join("plain"), "line one\nline two\n\n").expect("write");
        let read = Source::File("plain".into()).read(&dir).expect("read");
        assert_eq!(trim_newline(read), "line one\nline two\n");

        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "tok\r\n".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(dir.join("utf16"), utf16).expect("write");
        let read = Source::File("utf16".into()).read(&dir).expect("read");
        assert_eq!(trim_newline(read), "tok");

        std::fs::write(dir.join("binary"), [0x41, 0xFF, 0x42]).expect("write");
        let err = Source::File("binary".into()).read(&dir).unwrap_err();
        assert!(err.contains("not text"), "{err}");
        assert!(Source::File("absent".into()).read(&dir).is_err());

        // An absolute path ignores the base.
        let absolute = dir.join("plain").to_string_lossy().into_owned();
        assert!(Source::File(absolute).read(Path::new("/nowhere")).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dotenv_file_reads_quotes_exports_and_comments_without_interpolating() {
        let text = r##"
# a comment
PLAIN=abc
export EXPORTED=def
SPACED = ghi # a comment
HASHED=pa#ss
SINGLE='it'
DOUBLE="a\"b\\c\nd" # a comment
PATHLIKE="C:\keys\a"
LITERAL=$HOME
QUOTED_LITERAL='${HOME} #x'
EMPTY=
TWICE=first
TWICE=second
dotted.name-x=ok
"##;
        let value = |key: &str| dotenv_value(text, key).expect("parses");
        assert_eq!(value("PLAIN").as_deref(), Some("abc"));
        assert_eq!(value("EXPORTED").as_deref(), Some("def"));
        assert_eq!(value("SPACED").as_deref(), Some("ghi"));
        assert_eq!(value("HASHED").as_deref(), Some("pa#ss"));
        assert_eq!(value("SINGLE").as_deref(), Some("it"));
        assert_eq!(value("DOUBLE").as_deref(), Some("a\"b\\c\nd"));
        assert_eq!(value("PATHLIKE").as_deref(), Some("C:\\keys\\a"));
        assert_eq!(value("LITERAL").as_deref(), Some("$HOME"));
        assert_eq!(value("QUOTED_LITERAL").as_deref(), Some("${HOME} #x"));
        assert_eq!(value("EMPTY").as_deref(), Some(""));
        assert_eq!(value("TWICE").as_deref(), Some("second"));
        assert_eq!(value("dotted.name-x").as_deref(), Some("ok"));
        assert_eq!(value("ABSENT"), None);
    }

    // SPEC §host.cli: "a parse error gives a line number and never the line".
    #[test]
    fn a_dotenv_parse_error_gives_the_line_number_and_never_the_line() {
        let dir = scratch();
        for (bad, line) in [
            (format!("A=1\n{SENTINEL}\n"), 2),
            (format!("A=1\n\n# note\nB=\"{SENTINEL}\n"), 4),
            (format!("B='{SENTINEL}\n"), 1),
            (format!("B='{SENTINEL}' trailing\n"), 1),
            (format!("{SENTINEL} x=1\n"), 1),
        ] {
            std::fs::write(dir.join(".env"), &bad).expect("write");
            let err = Source::Dotenv { path: ".env".into(), key: "B".into() }.read(&dir).unwrap_err();
            assert_eq!(err, format!(".env: line {line} is not KEY=VALUE"), "{bad}");
        }
        std::fs::write(dir.join(".env"), "A=1\n").expect("write");
        let err = Source::Dotenv { path: ".env".into(), key: "B".into() }.read(&dir).unwrap_err();
        assert_eq!(err, ".env sets no B");
        let _ = std::fs::remove_dir_all(dir);
    }
}
