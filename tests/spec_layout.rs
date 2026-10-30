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

//! The specification holds together (SPEC §about).
//!
//! `docs/SPEC.md` is the front page and `docs/spec/` the numbered chapters: a file
//! `NNNN-area.md`, or a directory `NNNN-area/` whose `README.md` carries the title and lists
//! the numbered part files holding the sections. These tests hold the two to each other: the
//! chapter and part numbering, each title and section anchor against its file name, the size
//! of each file, the front page's chapter table and anchor map against the headings, the
//! table's Crate column against the workspace's manifests (every crate specified by exactly one
//! chapter), and every `§` citation in the tree against an anchor that exists. A failure lists
//! what is wrong and, for the table, the parts list and the map, the lines it expected, ready
//! to paste.
//!
//! Parsed by hand: ADR-007 keeps `regex` out of the tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The separator between a heading's anchor and its title.
const DASH: &str = " — ";

/// The chapter that may carry `## ADR-NNN — …` sections instead of anchored ones.
const DECISIONS: &str = "decisions";

/// Lines one file under `docs/spec/` may hold; a chapter that outgrows it becomes a directory
/// of parts (SPEC §about).
const MAX_LINES: usize = 1000;

/// The chapter table's columns: `| No. | Chapter | Crate | Area |`.
const TABLE_COLUMNS: usize = 4;

/// The Crate cell of a chapter that specifies no crate.
const NO_CRATE: &str = "—";

/// Directories a citation scan starts from, relative to the repository root.
const SCAN_ROOTS: &[&str] = &[
    "src",
    "crates",
    "tests",
    "scripts",
    "e2e",
    ".agents/skills",
    ".github",
    "docs",
    "AGENTS.md",
    "README.md",
    "CONTRIBUTING.md",
    "Cargo.toml",
    "build.rs",
];

/// Extensions of the files a citation scan reads.
const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "ts", "js", "mjs", "md", "ps1", "sh", "toml", "yml", "json", "css", "html",
];

/// Directory names a citation scan never enters: build trees, installs, recorded test data
/// (fixtures quote citations as data, not as claims) and third-party copies.
const SKIPPED_DIRS: &[&str] = &["node_modules", "fixtures", "vendor", ".git", "reference", "worktrees"];

/// History quotes anchors as they were when it was written; it is never rewritten.
const SKIPPED_FILES: &[&str] = &["CHANGELOG.md"];

struct Section {
    /// The heading text after `## `, e.g. `§host.vault — The secret vault`.
    text: String,
    /// The citable anchor without its `§`, e.g. `host.vault`; `None` for an ADR.
    anchor: Option<String>,
    /// The file holding it, as the front page links it: `spec/0012-memory/01-record.md`.
    link: String,
}

struct Chapter {
    number: String,
    slug: String,
    /// The file carrying the title, as the front page links it: `spec/0004-host.md`, or
    /// `spec/0012-memory/README.md` for a chapter in parts.
    link: String,
    /// The title text after `# `, e.g. `§host — The host`.
    title: String,
    sections: Vec<Section>,
}

/// One file of a chapter and what its headings must be.
struct File<'a> {
    /// Where it lives under `docs/`, e.g. `spec/0012-memory/01-record.md`.
    link: String,
    slug: &'a str,
    /// The first line after the `#` title.
    byline: String,
    /// The title carries the chapter's anchor (a chapter file, a parts README) rather than
    /// naming a part.
    anchored_title: bool,
    /// `##` sections may appear here; a parts README lists its parts and holds none.
    holds_sections: bool,
}

struct Parsed {
    title: Option<String>,
    sections: Vec<Section>,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The lines of a Markdown file outside code fences, numbered from 1.
fn prose_lines(text: &str) -> Vec<(usize, &str)> {
    let mut fence: Option<&str> = None;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (None, Some(m)) => fence = Some(m),
            (Some(open), Some(m)) if open == m => fence = None,
            (None, None) => out.push((i + 1, line)),
            _ => {}
        }
    }
    out
}

/// A heading's level and text: `## §host.vault — …` is `(2, "§host.vault — …")`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if level == 0 {
        return None;
    }
    line[level..].strip_prefix(' ').map(|text| (level, text.trim_end()))
}

/// The id GitHub gives a heading: lowercased, everything but letters, digits, `-`, `_` and
/// spaces dropped, spaces turned into `-`. `§host.vault — The secret vault` becomes
/// `hostvault--the-secret-vault`.
fn github_slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

fn is_citation_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

/// An anchor a citation can name: an ASCII letter first, citation characters after, no
/// trailing dot (a citation scan strips it as sentence punctuation).
fn is_anchor(token: &str) -> bool {
    token.starts_with(|c: char| c.is_ascii_lowercase())
        && token.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && !token.ends_with('.')
}

/// `ADR-NNN` with exactly three digits; the number.
fn adr_number(text: &str) -> Option<u32> {
    let digits = text.strip_prefix("ADR-")?;
    if digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse().ok()
    } else {
        None
    }
}

/// `0004-host` read with 4 digits, `01-record` with 2: the number and the slug.
fn numbered(stem: &str, digits: usize) -> Option<(&str, &str)> {
    let (number, slug) = stem.split_once('-')?;
    let valid = number.len() == digits && number.bytes().all(|b| b.is_ascii_digit()) && is_anchor(slug) && !slug.contains('.');
    valid.then_some((number, slug))
}

/// A parts README's list item, `1. [The record](01-record.md) — …`.
fn is_part_item(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". [")
}

/// Reads every chapter under `docs/spec/`, pushing what breaks the layout into `errors`.
fn chapters(errors: &mut Vec<String>) -> Vec<Chapter> {
    let dir = root().join("docs/spec");
    let mut out = Vec::new();
    for name in sorted_names(&dir) {
        let path = dir.join(&name);
        let stem = if path.is_dir() { Some(name.as_str()) } else { name.strip_suffix(".md") };
        let Some((number, slug)) = stem.and_then(|s| numbered(s, 4)) else {
            errors.push(format!("docs/spec/{name}: a chapter is a file NNNN-area.md or a directory NNNN-area/, nothing else lives here"));
            continue;
        };
        let expected = format!("{:04}", out.len() + 1);
        if number != expected {
            errors.push(format!("docs/spec/{name}: numbered {number}, expected {expected} (chapters count up from 0001 without gaps)"));
        }
        out.push(if path.is_dir() {
            chapter_in_parts(&path, &name, number, slug, errors)
        } else {
            chapter_in_one_file(&name, number, slug, errors)
        });
    }
    out
}

fn chapter_in_one_file(name: &str, number: &str, slug: &str, errors: &mut Vec<String>) -> Chapter {
    let file = File {
        link: format!("spec/{name}"),
        slug,
        byline: format!("Chapter {number} of the [swiss specification](../SPEC.md)."),
        anchored_title: true,
        holds_sections: true,
    };
    let parsed = parse(&file, &mut 0, errors);
    Chapter {
        number: number.to_owned(),
        slug: slug.to_owned(),
        link: file.link,
        title: parsed.title.unwrap_or_default(),
        sections: parsed.sections,
    }
}

fn chapter_in_parts(dir: &Path, name: &str, number: &str, slug: &str, errors: &mut Vec<String>) -> Chapter {
    let readme = File {
        link: format!("spec/{name}/README.md"),
        slug,
        byline: format!("Chapter {number} of the [swiss specification](../../SPEC.md)."),
        anchored_title: true,
        holds_sections: false,
    };
    if !dir.join("README.md").is_file() {
        errors.push(format!("docs/spec/{name}/: a chapter in parts carries its title in README.md"));
        return Chapter { number: number.to_owned(), slug: slug.to_owned(), link: readme.link, title: String::new(), sections: Vec::new() };
    }
    let mut last_adr = 0;
    let head = parse(&readme, &mut last_adr, errors);

    let mut sections = Vec::new();
    let mut listed = Vec::new();
    for part in sorted_names(dir).into_iter().filter(|n| n != "README.md") {
        let Some((part_number, _)) = part.strip_suffix(".md").and_then(|s| numbered(s, 2)) else {
            errors.push(format!("docs/spec/{name}/{part}: a part is a file NN-part.md beside README.md, nothing else lives here"));
            continue;
        };
        let expected = format!("{:02}", listed.len() + 1);
        if part_number != expected {
            errors.push(format!("docs/spec/{name}/{part}: numbered {part_number}, expected {expected} (parts count up from 01 without gaps)"));
        }
        let file = File {
            link: format!("spec/{name}/{part}"),
            slug,
            byline: format!("Part {part_number} of [§{slug}](README.md), chapter {number} of the [swiss specification](../../SPEC.md)."),
            anchored_title: false,
            holds_sections: true,
        };
        let parsed = parse(&file, &mut last_adr, errors);
        listed.push(format!("{}. [{}]({part})", listed.len() + 1, parsed.title.unwrap_or_default()));
        sections.extend(parsed.sections);
    }
    if listed.is_empty() {
        errors.push(format!("docs/spec/{name}/: a chapter in parts holds at least one NN-part.md"));
    }

    let text = read(&root().join("docs").join(&readme.link));
    let items: Vec<&str> = prose_lines(&text).into_iter().map(|(_, l)| l.trim_end()).filter(|l| is_part_item(l)).collect();
    let list_holds = items.len() == listed.len()
        && items.iter().zip(&listed).all(|(item, prefix)| {
            item.strip_prefix(prefix.as_str()).and_then(|rest| rest.strip_prefix(DASH)).is_some_and(|what| !what.trim().is_empty())
        });
    if !list_holds {
        errors.push(format!(
            "docs/{}: the parts list must read, in this order (each followed by ` — <what it covers>`):\n{}",
            readme.link,
            listed.join("\n")
        ));
    }
    Chapter {
        number: number.to_owned(),
        slug: slug.to_owned(),
        link: readme.link,
        title: head.title.unwrap_or_default(),
        sections,
    }
}

/// Reads one chapter file's title and sections; `last_adr` carries the decision numbering
/// across the files of one chapter.
fn parse(file: &File, last_adr: &mut u32, errors: &mut Vec<String>) -> Parsed {
    let text = read(&root().join("docs").join(&file.link));
    let slug = file.slug;
    let at = |line: usize| format!("docs/{}:{line}", file.link);
    let count = text.lines().count();
    if count > MAX_LINES {
        errors.push(format!(
            "docs/{}: {count} lines, past {MAX_LINES}; split the chapter into parts, or a chapter in parts into more parts (SPEC §about)",
            file.link
        ));
    }

    let mut title: Option<String> = None;
    let mut sections = Vec::new();
    let lines = prose_lines(&text);
    for (index, &(n, line)) in lines.iter().enumerate() {
        let Some((level, h)) = heading(line) else {
            continue;
        };
        match level {
            1 => {
                let anchor = h.strip_prefix('§').and_then(|rest| rest.split_once(DASH)).map(|(a, _)| a);
                if title.is_some() {
                    errors.push(format!("{}: a second `#` title; a file has exactly one", at(n)));
                } else if file.anchored_title && anchor != Some(slug) {
                    errors.push(format!("{}: the title must read `# §{slug} — <title>`", at(n)));
                } else if !file.anchored_title && (h.starts_with('§') || h.starts_with("ADR-")) {
                    errors.push(format!("{}: a part's title names the part; the chapter's anchor is on README.md's title", at(n)));
                } else {
                    let byline = lines[index + 1..].iter().map(|(_, l)| l.trim()).find(|l| !l.is_empty());
                    if byline != Some(file.byline.as_str()) {
                        errors.push(format!("{}: the title is followed by `{}`", at(n), file.byline));
                    }
                }
                title.get_or_insert_with(|| h.to_owned());
            }
            2 => {
                if title.is_none() {
                    errors.push(format!("{}: a section before the title", at(n)));
                }
                if !file.holds_sections {
                    errors.push(format!("{}: a chapter in parts keeps its sections in the part files", at(n)));
                } else if let Some(rest) = h.strip_prefix('§') {
                    let anchor = rest.split_once(DASH).map(|(a, _)| a);
                    match anchor.filter(|a| is_anchor(a) && a.strip_prefix(slug).is_some_and(|s| s.len() > 1 && s.starts_with('.'))) {
                        Some(anchor) => sections.push(Section { text: h.to_owned(), anchor: Some(anchor.to_owned()), link: file.link.clone() }),
                        None => errors.push(format!("{}: a section reads `## §{slug}.<sub> — <title>`", at(n))),
                    }
                } else if let Some(number) = h.split_once(DASH).and_then(|(id, _)| adr_number(id)).filter(|_| slug == DECISIONS) {
                    if number != *last_adr + 1 {
                        errors.push(format!("{}: ADR-{number:03} follows ADR-{:03}; decisions count up without gaps", at(n), *last_adr));
                    }
                    *last_adr = number;
                    sections.push(Section { text: h.to_owned(), anchor: None, link: file.link.clone() });
                } else if slug == DECISIONS {
                    errors.push(format!("{}: a decision reads `## ADR-NNN — <title>`", at(n)));
                } else {
                    errors.push(format!("{}: a section reads `## §{slug}.<sub> — <title>`", at(n)));
                }
            }
            _ => {
                if h.starts_with('§') || h.starts_with("ADR-") {
                    errors.push(format!("{}: anchors live on `##` headings only, not at level {level}", at(n)));
                }
            }
        }
    }
    if title.is_none() {
        errors.push(format!("docs/{}: no `#` title", file.link));
    }
    Parsed { title, sections }
}

/// The chapter table's rows up to the crate column: `| 0004 | [§host](spec/0004-host.md) |`.
fn expected_table(chapters: &[Chapter]) -> Vec<String> {
    chapters
        .iter()
        .map(|c| format!("| {} | [§{}]({}) |", c.number, c.slug, c.link))
        .collect()
}

/// The anchor map's non-blank lines, in chapter and file order.
fn expected_map(chapters: &[Chapter]) -> Vec<String> {
    let mut out = Vec::new();
    for c in chapters {
        out.push(format!("### {} · [{}]({})", c.number, c.title, c.link));
        for s in &c.sections {
            out.push(format!("- [{}]({}#{})", s.text, s.link, github_slug(&s.text)));
        }
    }
    out
}

/// The map as it is pasted: a blank line after each chapter's line and before the next one.
fn render_map(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.starts_with("### ") {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            out.push_str("\n\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The non-blank prose lines under the front page's `## <name>`, up to the next `##`.
fn front_section<'a>(lines: &[(usize, &'a str)], name: &str) -> Vec<&'a str> {
    let start = lines.iter().position(|(_, l)| heading(l) == Some((2, name)));
    let Some(start) = start else {
        return Vec::new();
    };
    lines[start + 1..]
        .iter()
        .map(|(_, l)| l.trim_end())
        .take_while(|l| !matches!(heading(l), Some((1 | 2, _))))
        .filter(|l| !l.is_empty())
        .collect()
}

/// The chapter table's rows: the lines under `## Chapters` whose first cell is a number.
fn chapter_rows<'a>(lines: &[(usize, &'a str)]) -> Vec<&'a str> {
    front_section(lines, "Chapters")
        .into_iter()
        .filter(|l| l.split('|').nth(1).is_some_and(|cell| cell.trim().bytes().all(|b| b.is_ascii_digit()) && !cell.trim().is_empty()))
        .collect()
}

/// A row's cells without the empty ones outside the outer pipes.
fn cells(row: &str) -> Vec<&str> {
    let inner = row.trim().strip_prefix('|').and_then(|r| r.strip_suffix('|')).unwrap_or(row);
    inner.split('|').map(str::trim).collect()
}

/// A Crate cell's names: `—` names none, otherwise backticked names joined by `, `; `None`
/// when the cell reads any other way.
fn crate_names(cell: &str) -> Option<Vec<&str>> {
    if cell == NO_CRATE {
        return Some(Vec::new());
    }
    cell.split(',')
        .map(|item| item.trim().strip_prefix('`').and_then(|n| n.strip_suffix('`')).filter(|n| !n.is_empty() && !n.contains('`')))
        .collect()
}

/// The `name` in a manifest's `[package]` table, read by hand.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if let Some((key, value)) = line.split_once('=').filter(|_| in_package) {
            if key.trim() == "name" {
                return Some(value.trim().trim_matches('"').to_owned());
            }
        }
    }
    None
}

/// The paths in the root manifest's `[workspace] members` list, on one line or many, comments
/// skipped.
fn workspace_members(manifest: &str) -> Vec<String> {
    let mut list = String::new();
    let mut open = false;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or("");
        let rest = if open {
            line
        } else {
            match line.trim_start().strip_prefix("members").and_then(|r| r.trim_start().strip_prefix('=')) {
                Some(r) => {
                    open = true;
                    r
                }
                None => continue,
            }
        };
        if let Some((head, _)) = rest.split_once(']') {
            list.push_str(head);
            break;
        }
        list.push_str(rest);
        list.push(',');
    }
    list.split(',')
        .map(|item| item.trim().trim_start_matches('[').trim().trim_matches('"').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn fail(errors: &[String]) {
    assert!(errors.is_empty(), "the specification's layout is broken:\n{}", errors.join("\n"));
}

#[test]
fn chapters_are_numbered_and_anchored_by_their_file_names() {
    let mut errors = Vec::new();
    let chapters = chapters(&mut errors);
    assert!(!chapters.is_empty(), "docs/spec/ holds no chapters");

    let mut seen = BTreeSet::new();
    for c in &chapters {
        if c.slug == "about" || !seen.insert(c.slug.clone()) {
            errors.push(format!("docs/{}: §{} is defined twice", c.link, c.slug));
        }
        let mut ids = BTreeSet::new();
        for s in &c.sections {
            if let Some(anchor) = &s.anchor {
                if !seen.insert(anchor.clone()) {
                    errors.push(format!("docs/{}: §{anchor} is defined twice", s.link));
                }
            }
            if !ids.insert((s.link.as_str(), github_slug(&s.text))) {
                errors.push(format!("docs/{}: two headings share the link id #{}", s.link, github_slug(&s.text)));
            }
        }
    }
    fail(&errors);
}

#[test]
fn front_page_table_and_map_match_the_chapters() {
    let mut errors = Vec::new();
    let chapters = chapters(&mut errors);
    let text = read(&root().join("docs/SPEC.md"));
    let lines = prose_lines(&text);

    for (n, line) in &lines {
        if let Some((level, h)) = heading(line) {
            let anchored = h.starts_with('§') || h.starts_with("ADR-");
            if anchored && !(level == 2 && h.starts_with(&format!("§about{DASH}"))) {
                errors.push(format!("docs/SPEC.md:{n}: only `## §about` is anchored here; `{h}` belongs in its chapter"));
            }
        }
    }
    if !lines.iter().any(|(_, l)| heading(l).is_some_and(|(level, h)| level == 2 && h.starts_with(&format!("§about{DASH}")))) {
        errors.push("docs/SPEC.md: no `## §about — …` section".to_owned());
    }

    let rows = chapter_rows(&lines);
    let table = expected_table(&chapters);
    let table_holds = rows.len() == table.len()
        && rows.iter().zip(&table).all(|(row, prefix)| {
            row.starts_with(prefix.as_str()) && cells(row).len() == TABLE_COLUMNS && cells(row).iter().all(|cell| !cell.is_empty())
        });
    if !table_holds {
        errors.push(format!(
            "docs/SPEC.md: the chapter table's rows must begin, in this order, with (each followed by ` <crates> | <area> |`):\n{}",
            table.join("\n")
        ));
    }

    let map = front_section(&lines, "Anchor map");
    let expected = expected_map(&chapters);
    if map != expected {
        let first = map.iter().zip(&expected).position(|(have, want)| have != want).unwrap_or(map.len().min(expected.len()));
        errors.push(format!(
            "docs/SPEC.md: the anchor map differs from the headings at its line {} (have `{}`, want `{}`); the map should read:\n\n{}",
            first + 1,
            map.get(first).copied().unwrap_or("<end>"),
            expected.get(first).map(String::as_str).unwrap_or("<end>"),
            render_map(&expected),
        ));
    }
    fail(&errors);
}

#[test]
fn every_crate_is_specified_by_exactly_one_chapter() {
    let root = root();
    let manifest = read(&root.join("Cargo.toml"));
    let mut crates: Vec<String> = package_name(&manifest).into_iter().collect();
    for member in workspace_members(&manifest) {
        let path = root.join(&member).join("Cargo.toml");
        let name = package_name(&read(&path)).unwrap_or_else(|| panic!("{}: no [package] name", path.display()));
        crates.push(name);
    }
    assert!(crates.len() > 2, "the workspace lists only {crates:?}; is the members parser right?");

    let text = read(&root.join("docs/SPEC.md"));
    let lines = prose_lines(&text);
    let mut errors = Vec::new();
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    for row in chapter_rows(&lines) {
        let cells = cells(row);
        let number = cells[0];
        let Some(names) = cells.get(2).filter(|_| cells.len() == TABLE_COLUMNS).and_then(|cell| crate_names(cell)) else {
            errors.push(format!(
                "docs/SPEC.md: chapter {number}'s Crate cell must be `{NO_CRATE}` or backticked crate names joined by `, ` (the columns are | No. | Chapter | Crate | Area |)"
            ));
            continue;
        };
        for name in names {
            if !crates.iter().any(|c| c == name) {
                errors.push(format!("docs/SPEC.md: chapter {number} names `{name}`, which is no crate of the workspace"));
            } else if let Some(first) = owners.insert(name, number) {
                errors.push(format!("docs/SPEC.md: `{name}` is named by chapters {first} and {number}; one chapter specifies a crate"));
            }
        }
    }
    for c in &crates {
        if !owners.contains_key(c.as_str()) {
            errors.push(format!("docs/SPEC.md: the crate `{c}` has no chapter; name it in the Crate cell of the chapter that specifies it (SPEC §about)"));
        }
    }
    fail(&errors);
}

/// Every text file under the scan roots, skipping build trees and recorded data.
fn scanned_files() -> Vec<PathBuf> {
    let root = root();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = SCAN_ROOTS.iter().map(|r| root.join(r)).filter(|p| p.exists()).collect();
    while let Some(path) = stack.pop() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.is_dir() {
            if name.starts_with("target") || SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let entries = fs::read_dir(&path).unwrap_or_else(|e| panic!("list {}: {e}", path.display()));
            stack.extend(entries.map(|entry| entry.expect("a directory entry").path()));
        } else if !SKIPPED_FILES.contains(&name.as_str())
            && path.extension().is_some_and(|ext| TEXT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()))
        {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// The `§` citations on one line: an ASCII letter after the `§`, citation characters after
/// it, a trailing dot dropped as punctuation. `§<area>` and `§3` are prose, not citations.
fn citations(line: &str) -> Vec<&str> {
    line.match_indices('§')
        .filter_map(|(i, mark)| {
            let rest = &line[i + mark.len()..];
            let end = rest.find(|c: char| !is_citation_char(c)).unwrap_or(rest.len());
            let token = rest[..end].trim_end_matches('.');
            token.starts_with(|c: char| c.is_ascii_alphabetic()).then_some(token)
        })
        .collect()
}

#[test]
fn every_citation_names_an_anchor_that_exists() {
    // A broken heading is the chapter test's to report; here it only drops that anchor.
    let chapters = chapters(&mut Vec::new());
    let mut errors = Vec::new();
    let mut anchors = BTreeSet::from(["about".to_owned()]);
    for c in &chapters {
        anchors.insert(c.slug.clone());
        anchors.extend(c.sections.iter().filter_map(|s| s.anchor.clone()));
    }

    let root = root();
    let files = scanned_files();
    assert!(files.len() > 100, "the scan found only {} files; are the roots right?", files.len());
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let shown = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            for token in citations(line) {
                if !anchors.contains(token) {
                    errors.push(format!("{shown}:{}: §{token} names no chapter or section", i + 1));
                }
            }
        }
    }
    fail(&errors);
}

#[test]
fn the_parsers_read_what_the_layout_writes() {
    assert_eq!(github_slug("§host.vault — The secret vault"), "hostvault--the-secret-vault");
    assert_eq!(
        github_slug("ADR-033 — Module-owned history: MCP's calls and traffic move into `logs/mcp.db`"),
        "adr-033--module-owned-history-mcps-calls-and-traffic-move-into-logsmcpdb"
    );
    assert_eq!(citations("see SPEC §host.vault. And §about, not §<area> or §3"), ["host.vault", "about"]);
    assert_eq!(heading("## §host.vault — x"), Some((2, "§host.vault — x")));
    assert_eq!(heading("#!/usr/bin/env bash"), None);
    assert_eq!(adr_number("ADR-007"), Some(7));
    assert_eq!(adr_number("ADR-7"), None);
    assert_eq!(numbered("0004-host", 4), Some(("0004", "host")));
    assert_eq!(numbered("01-record", 2), Some(("01", "record")));
    assert_eq!(numbered("4-host", 4), None);
    assert!(is_part_item("1. [The record](01-record.md) — notes"));
    assert!(!is_part_item("**Parts.** six files"));
    assert_eq!(cells("| 0004 | [§host](spec/0004-host.md) | `swiss-host`, `swiss` | Host |").len(), 4);
    assert_eq!(crate_names("`swiss-host`, `swiss`"), Some(vec!["swiss-host", "swiss"]));
    assert_eq!(crate_names("—"), Some(Vec::new()));
    assert_eq!(crate_names("swiss-host"), None);
    assert_eq!(crate_names("``"), None);
    assert_eq!(package_name("[workspace]\nname = \"no\"\n[package]\nname = \"swiss\"\n"), Some("swiss".to_owned()));
    assert_eq!(workspace_members("[workspace]\nmembers = [\n    \"crates/a\",\n    # why\n    \"crates/b\",\n]\n"), ["crates/a", "crates/b"]);
    assert_eq!(workspace_members("members = [\"crates/a\", \"crates/b\"]"), ["crates/a", "crates/b"]);
    let fenced = prose_lines("a\n```text\n# not a heading\n```\nb");
    assert_eq!(fenced, [(1, "a"), (5, "b")]);
}
