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

//! The map's first draft (SPEC §memory.map): `swiss memory init --map` draws a project's
//! domains from the code as it is now - never from its history. This is the pure half:
//! the skeleton read off the code index and the manifests (packages, directories, and
//! business stems in a layered layout), the relations between the areas (who uses whose
//! names, who imports whom), one bounded prompt per area whose answer fills fixed fields,
//! those fields checked against the index, and the hubs assembled from them by one
//! template - parents after their children, told what their children are.
//!
//! The model never draws the map and never writes a hub: it names and explains what the
//! skeleton found. A model that answers badly, or no model at all, still leaves a map -
//! every hub falls back to what the index says.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::code::db;
use crate::domain::{owns_path, Map};
use crate::error::{MemError, MemResult};
use crate::model::{Domain, DOMAIN_DEPTH_MAX, DOMAIN_OWNS_MAX, DOMAIN_SEGMENT_MAX, DOMAIN_SUMMARY_MAX, DOMAIN_TITLE_MAX, KEYWORDS_MAX, KEYWORD_MAX};
use crate::secrets;
use crate::service::{MemoryService, Target};
use crate::store;
use crate::sync::{DomainOp, Proposal};
use crate::transcript::MARKER;
use crate::view::clip;

mod manifests;
mod draft;
mod question;
mod review;

pub use self::manifests::*;
pub use self::draft::*;
pub use self::question::*;
pub use self::review::*;

/// Areas one map draws at most: each costs the model one question.
pub const UNITS_MAX: usize = 150;
/// Top-level areas, and sub-areas of one area, at most.
const TOP_MAX: usize = 14;
const CHILDREN_MAX: usize = 10;
/// Levels the rules draw at most: a review's new parent takes one more on top.
const SKELETON_DEPTH: usize = DOMAIN_DEPTH_MAX - 1;
/// Business stems on the top level at most, besides the [TOP_MAX] other areas: what a
/// review groups into the project's domains, so it sees them all.
const STEMS_MAX: usize = 48;
/// Code files that make a directory an area of its own.
const UNIT_FILES_MIN: usize = 3;
/// Code files that make an area worth splitting.
const SPLIT_FILES_MIN: usize = 12;
/// Files sharing a name stem that make them an area in a flat directory.
const STEM_FILES_MIN: usize = 3;
/// What one area's prompt carries at most.
const PROMPT_FILES_MAX: usize = 30;
const FILE_DEFS_SHOWN: usize = 6;
const SYMBOLS_MAX: usize = 15;
const RELATIONS_MAX: usize = 8;
const EXCERPTS_MAX: usize = 3;
const EXCERPT_LINES: usize = 60;
const EXCERPT_BYTES: usize = 2500;
const README_BYTES: usize = 1500;
const MANIFEST_BYTES: usize = 16 * 1024;
/// The answer's limits: items per list and characters per item.
const OVERVIEW_CHARS: usize = 900;
const ITEM_CHARS: usize = 240;

/// Directories whose files are no area's code: tests, vendored, built or generated.
const SKIP_DIRS: [&str; 33] = [
    "node_modules", "vendor", "third_party", "third-party", "dist", "build", "target", "out", "obj", "coverage",
    "__pycache__", "generated", "test", "tests", "__tests__", "testdata", "test-data", "test_data", "testing", "fixtures",
    "e2e", "spec", "specs", "benches", "bench", "benchmarks", "examples", "example", "samples", "mocks", "__mocks__",
    "docs", "doc",
];
/// Source files the index reads no names from, still counted as an area's code.
const MORE_SOURCE: [&str; 20] = [
    "vue", "svelte", "swift", "dart", "lua", "ex", "exs", "erl", "hs", "ml", "fs", "clj", "sql", "proto", "graphql",
    "gql", "zig", "m", "mm", "groovy",
];
/// Code that serves the program rather than being it: schemas, migrations, interface
/// definitions and scripts. Where it lies never decides where the program's areas are:
/// it is cut apart from the program's code, and what no program area owns becomes areas
/// of its own beside them (`sql/` beside a Java project's business areas).
const SUPPORT_SOURCE: [&str; 9] = ["sql", "proto", "graphql", "gql", "sh", "bash", "zsh", "ps1", "psm1"];
/// Manifests that make their directory a package.
const MANIFESTS: [&str; 12] = [
    "Cargo.toml", "package.json", "go.mod", "pom.xml", "build.gradle", "build.gradle.kts", "pyproject.toml", "setup.py",
    "composer.json", "Gemfile", "mix.exs", "pubspec.yaml",
];
/// Directories that only group what is under them: their children are the areas.
const WRAPPERS: [&str; 18] = [
    "src", "lib", "source", "sources", "main", "java", "kotlin", "scala", "app", "internal", "pkg", "packages", "crates",
    "apps", "libs", "modules", "components", "features",
];
/// Directory names of a technical layer: two or more side by side mean the business areas
/// cut across them, by file name.
const LAYERS: [&str; 55] = [
    "controller", "controllers", "service", "services", "repository", "repositories", "repo", "repos", "dao", "daos",
    "dal", "model", "models", "entity", "entities", "dto", "dtos", "vo", "po", "bo", "domain", "mapper", "mappers",
    "handler", "handlers", "api", "web", "rest", "resource", "resources", "impl", "views", "serializers", "schemas",
    "routes", "router", "routers", "store", "stores", "reducers", "actions", "job", "jobs", "workers", "listeners",
    "events", "commands", "queries", "facade", "converter", "converters", "client", "clients", "remote", "usecases",
];
/// Words that end a class name by its role (`Ctl` of `InvoiceCtl`, `Query` of
/// `UserQuery`): past a business word they name no part of that business.
const ROLES: [&str; 30] = [
    "ctl", "controller", "service", "impl", "mapper", "dao", "repository", "repo", "entity", "dto", "vo", "po", "bo",
    "ro", "req", "res", "request", "response", "query", "param", "params", "form", "job", "listener", "handler",
    "enum", "exception", "converter", "client", "facade",
];
/// First words of a file name that name no business: such a file stays with its parent.
const GENERIC: [&str; 44] = [
    "index", "main", "mod", "lib", "util", "utils", "helper", "helpers", "common", "base", "abstract", "default", "types",
    "type", "constants", "const", "config", "configuration", "app", "application", "test", "tests", "init", "setup",
    "core", "shared", "misc", "api", "impl", "interface", "interfaces", "model", "models", "service", "services",
    "controller", "handler", "get", "set", "new", "simple", "generic", "the", "global",
];
/// File names that are where a program or a package starts.
const ENTRY_FILES: [&str; 16] = [
    "main.rs", "lib.rs", "main.go", "main.py", "__main__.py", "app.py", "index.ts", "index.tsx", "index.js", "main.ts",
    "main.js", "server.ts", "server.js", "cli.py", "manage.py", "main.c",
];

// --- what the survey reads -----------------------------------------------------------------

/// One listed file of the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub path: String,
    pub lang: Option<String>,
    /// The index read its names (not binary, not too large, not data).
    pub read: bool,
    pub size: i64,
}

/// One definition: in which listed file, and how many other files mention its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub file: usize,
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
    pub fanin: u32,
}

/// What the code index holds of one checkout, read in bulk: indexes are into `files`.
#[derive(Debug, Clone, Default)]
pub struct Indexed {
    pub files: Vec<Listed>,
    pub defs: Vec<Def>,
    /// (mentioning file, defining file, name).
    pub uses: Vec<(usize, usize, String)>,
    pub imports: Vec<(usize, String)>,
    /// The listing stopped at a cap.
    pub truncated: bool,
}

/// A package's manifest, as far as a survey reads one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// The package's directory ("" at the root).
    pub dir: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The names a dependency names this package by, as [package_key] folds them.
    #[serde(skip)]
    pub ids: Vec<String>,
    /// The packages it depends on, folded the same way; None when its format's
    /// dependencies are not read, so it gates nothing.
    #[serde(skip)]
    pub deps: Option<Vec<String>>,
}

// --- the skeleton --------------------------------------------------------------------------

/// How an area was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitKind {
    /// A directory with a manifest.
    Package,
    Dir,
    /// The files sharing a name stem: in a flat directory, or across a layered layout.
    Stem,
    /// A business domain the model's review drew over areas the rules found: their parent,
    /// or the one area they make together.
    Group,
}

impl UnitKind {
    fn says(self) -> &'static str {
        match self {
            UnitKind::Package => "a package",
            UnitKind::Dir => "a directory",
            UnitKind::Stem => "the files sharing one name stem",
            UnitKind::Group => "a business domain drawn over areas the rules found",
        }
    }
}

/// What a draft is drawn with besides the code: the review's regrouping, and the domains
/// to draw as if the map did not have them (a redraw's, about to be removed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draw {
    pub regroup: Option<Regroup>,
    pub ignore: Vec<String>,
}

/// The model's review of a draft's top level (SPEC §memory.map): business domains drawn
/// over the areas the rules found. The review regroups and renames; it never draws an
/// area of its own, never splits one and never moves a file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Regroup {
    pub domains: Vec<Regrouped>,
    /// The entries, members and paths the check refused, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
}

/// One business domain of a review: over its `members` (top-level paths of the draft),
/// their parent - or, `merge`, the one area they are. One member is a rename.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Regrouped {
    pub path: String,
    pub title: String,
    pub members: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub merge: bool,
}

/// One group of code the skeleton found, before it is named on the map.
#[derive(Debug, Clone)]
struct Group {
    kind: UnitKind,
    /// The raw name its path segment is made from.
    name: String,
    title: String,
    /// The directory it lives in ("" at the root).
    base: String,
    owns: Vec<String>,
    files: Vec<usize>,
    manifest: Option<usize>,
    children: Vec<Group>,
}

/// A name the index defines, as a prompt and a hub cite it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub path: String,
    pub fanin: u32,
}

/// One relation to another area: its index, how strong, through which names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub unit: usize,
    pub weight: u32,
    pub via: Vec<String>,
}

/// One area of the drafted map, with everything its prompt carries.
#[derive(Debug, Clone)]
pub struct Unit {
    /// The domain path.
    pub path: String,
    /// The title when the model gives none.
    pub title: String,
    pub kind: UnitKind,
    pub base: String,
    pub owns: Vec<String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// The code files it owns (deepest area wins), as listed paths.
    pub files: Vec<String>,
    pub langs: Vec<(String, usize)>,
    pub bytes: u64,
    pub manifest: Option<Manifest>,
    pub readme: Option<String>,
    /// The files a prompt lists: path, bytes, the names it defines that are used most.
    pub listing: Vec<(String, u64, Vec<String>)>,
    pub symbols: Vec<Symbol>,
    pub entries: Vec<String>,
    pub uses: Vec<Relation>,
    pub used_by: Vec<Relation>,
    /// (path, language tag, first lines - redacted).
    pub excerpts: Vec<(String, String, String)>,
    /// The map already has this path: the draft keeps it as it is.
    pub kept: bool,
}

/// A whole draft: the areas, parents before children, and what checks an answer.
#[derive(Debug, Clone)]
pub struct Survey {
    pub project: String,
    pub units: Vec<Unit>,
    /// The project's README, the head of it.
    pub readme: Option<String>,
    pub truncated: bool,
    /// Code files read, and those the drafted areas own.
    pub code_files: usize,
    pub owned_files: usize,
    /// Code files the map the project has already owns.
    pub mapped_files: usize,
    /// What a full initialization reads besides the map (SPEC §memory.init): the code
    /// files and the docs, with their sizes.
    pub sources: Vec<(String, u64)>,
    listed: HashSet<String>,
    defined: HashMap<String, Vec<String>>,
    /// The paths the map has (a redraw's aside): a review's domain takes none of them.
    on_map: BTreeSet<String>,
}

/// One area as `--plan` and the run's scope show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanUnit {
    pub path: String,
    pub title: String,
    pub kind: UnitKind,
    pub owns: Vec<String>,
    pub files: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub kept: bool,
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|at| &path[..at]).unwrap_or("")
}

fn join(base: &str, rest: &str) -> String {
    if base.is_empty() {
        rest.to_string()
    } else {
        format!("{base}/{rest}")
    }
}

fn extension(path: &str) -> String {
    let name = last_segment(path);
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// A test file by its name, wherever it lives.
fn is_test_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or(&lower);
    lower.contains(".test.")
        || lower.contains(".spec.")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.starts_with("test_")
        || stem == "tests"
        || stem == "conftest"
        || (name.ends_with("Test.java") || name.ends_with("Tests.java") || name.ends_with("Test.kt") || name.ends_with("Spec.scala"))
}

/// Whether a listed file is code an area answers for.
pub fn is_code(file: &Listed) -> bool {
    if !file.read {
        return false;
    }
    let segments: Vec<&str> = file.path.split('/').collect();
    let (name, dirs) = segments.split_last().map(|(n, d)| (*n, d)).unwrap_or(("", &[]));
    if dirs.iter().any(|d| d.starts_with('.') || SKIP_DIRS.contains(&d.to_ascii_lowercase().as_str())) {
        return false;
    }
    if is_test_name(name) || name.ends_with(".d.ts") || name.contains(".min.") || name.contains(".generated.") {
        return false;
    }
    file.lang.is_some() || MORE_SOURCE.contains(&extension(&file.path).as_str())
}

/// Whether a listed file is a doc a full initialization reads (SPEC §memory.init): prose
/// under the project's own directories - not a changelog, nothing vendored or built.
pub fn is_doc(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    let (name, dirs) = segments.split_last().map(|(n, d)| (*n, d)).unwrap_or(("", &[]));
    let vendored = ["node_modules", "vendor", "third_party", "third-party", "dist", "build", "target", "out", "obj", "coverage", "generated"];
    if dirs.iter().any(|d| d.starts_with('.') || vendored.contains(&d.to_ascii_lowercase().as_str())) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let log = ["changelog", "changes", "history", "license", "licence", "notice", "authors", "contributors"]
        .iter()
        .any(|l| lower.starts_with(l));
    !log && matches!(extension(path).as_str(), "md" | "markdown" | "rst" | "adoc")
}

/// Whether a code file serves the program rather than being it (see [SUPPORT_SOURCE]).
fn is_support(path: &str) -> bool {
    SUPPORT_SOURCE.contains(&extension(path).as_str())
}

/// The words of a file name as byte ranges: split at `_`, `-`, `.` and case changes.
fn spans(name: &str) -> Vec<(usize, usize)> {
    let base = name.split('.').next().unwrap_or(name);
    let mut out = Vec::new();
    let mut offset = 0;
    for part in base.split(['_', '-']) {
        let bytes = part.as_bytes();
        let mut start = 0;
        for i in 1..bytes.len() {
            let (prev, here) = (bytes[i - 1], bytes[i]);
            let next_lower = bytes.get(i + 1).is_some_and(|b| b.is_ascii_lowercase());
            // `orderItem` | `HTTPServer` → `HTTP` `Server`.
            let cut = (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && here.is_ascii_uppercase()
                || prev.is_ascii_uppercase() && here.is_ascii_uppercase() && next_lower;
            if cut {
                out.push((offset + start, offset + i));
                start = i;
            }
        }
        if start < part.len() {
            out.push((offset + start, offset + part.len()));
        }
        offset += part.len() + 1;
    }
    out.into_iter().filter(|(a, b)| b > a).collect()
}

/// The words of a file name: split at `_`, `-`, `.` and case changes.
fn words(name: &str) -> Vec<&str> {
    spans(name).into_iter().map(|(a, b)| &name[a..b]).collect()
}

/// How many leading words of a name are no business: an interface's `I` (`IOrderService`),
/// then the project's shared `prefix` (lower case: `sys` of `SysUserMapper`) - each only
/// when a word follows it.
fn lead_words(words: &[&str], prefix: Option<&str>) -> usize {
    let mut at = 0;
    if words.len() >= 2 && words[0] == "I" && words[1].starts_with(|c: char| c.is_ascii_uppercase()) {
        at = 1;
    }
    if let Some(prefix) = prefix {
        if words.len() > at + 1 && words[at].eq_ignore_ascii_case(prefix) {
            at += 1;
        }
    }
    at
}

/// A file name's business stem.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StemOf {
    /// The stem word, lower case and singular: what joins files.
    key: String,
    /// The stem word as the name spells it: what the area is called.
    word: String,
    /// The name up to the end of the stem word (`ISysUser` of `ISysUserService`): what a
    /// glob of the area's files starts with.
    lead: String,
}

/// A file name's business stem: its first word past an interface's `I` and the shared
/// `prefix`. A word shorter than three letters takes the words after it until the stem is
/// long enough (`EcPay` of `EcPayChatService`, `Fx` of `FxRateMapper`), never a generic or
/// a layer's word: then two letters stand alone when a word follows (`Pm` of `PmService`).
/// None for a generic or too short stem.
fn stem_of(name: &str, prefix: Option<&str>) -> Option<StemOf> {
    let spans = spans(name);
    let all: Vec<&str> = spans.iter().map(|(a, b)| &name[*a..*b]).collect();
    let mut at = lead_words(&all, prefix);
    let (start, mut end) = *spans.get(at)?;
    while end - start < 3 {
        let Some(next) = all.get(at + 1).map(|w| w.to_ascii_lowercase()) else { break };
        if GENERIC.contains(&next.as_str()) || LAYERS.contains(&next.as_str()) || ROLES.contains(&next.as_str()) {
            break;
        }
        at += 1;
        end = spans[at].1;
    }
    let first = &name[start..end];
    let lower = first.to_ascii_lowercase();
    let short = lower.len() < 2 || (lower.len() == 2 && at + 1 >= all.len());
    if short || !lower.bytes().all(|b| b.is_ascii_alphanumeric()) || GENERIC.contains(&lower.as_str()) {
        return None;
    }
    let key = match lower.strip_suffix('s') {
        Some(single) if single.len() >= 3 && !single.ends_with('s') => single.to_string(),
        _ => lower,
    };
    Some(StemOf { key, word: first.to_string(), lead: name[..end].to_string() })
}

/// The first word most of these files' names share without naming any business by itself
/// (`Sys` of `SysUserController`, `ISysRoleService`, `SysMenuMapper`): half the names or
/// more start with it, three of them or more go on with different business words, three
/// in four of those that start with it go on with a business word (not a layer: `Order` of
/// `OrderController`, `OrderService`, `OrderItem` is the business), and no file is named
/// by it alone (an `Order.java` makes `Order` an entity).
fn shared_prefix(paths: &[String], files: &[usize]) -> Option<String> {
    #[derive(Default)]
    struct First {
        names: usize,
        business: usize,
        after: HashSet<String>,
        alone: bool,
    }
    if files.len() < 6 {
        return None;
    }
    let mut firsts: BTreeMap<String, First> = BTreeMap::new();
    for f in files {
        let all = words(last_segment(&paths[*f]));
        let skip = lead_words(&all, None);
        let Some(first) = all.get(skip) else { continue };
        let entry = firsts.entry(first.to_ascii_lowercase()).or_default();
        entry.names += 1;
        match all.get(skip + 1).map(|w| w.to_ascii_lowercase()) {
            Some(next) if next.len() >= 3 && !GENERIC.contains(&next.as_str()) && !LAYERS.contains(&next.as_str()) => {
                entry.business += 1;
                entry.after.insert(next);
            }
            Some(_) => {}
            None => entry.alone = true,
        }
    }
    let (word, first) = firsts.into_iter().max_by(|a, b| a.1.names.cmp(&b.1.names).then_with(|| b.0.cmp(&a.0)))?;
    let fits = first.names * 2 >= files.len() && first.after.len() >= 3 && first.business * 4 >= first.names * 3 && !first.alone;
    (fits && word.len() >= 2 && word.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(word)
}

/// What a stem area is called: the shortest spelling its files use (`job` of `job`, `jobs`).
/// The key folds a plural away to join them and is no name - `redis` keys as `redi`.
fn stem_name(literals: &BTreeSet<String>) -> Option<String> {
    literals.iter().min_by_key(|l| l.len()).cloned()
}

/// The longest directory every path lies in.
fn common_dir<'a>(paths: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<Vec<&str>> = None;
    for path in paths {
        let dirs: Vec<&str> = path.split('/').collect();
        let dirs = &dirs[..dirs.len().saturating_sub(1)];
        common = Some(match common {
            None => dirs.to_vec(),
            Some(have) => have.iter().zip(dirs).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    common.unwrap_or_default().join("/")
}

fn title_of(raw: &str) -> String {
    let spaced: String = raw.chars().map(|c| if c == '-' || c == '_' { ' ' } else { c }).collect();
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => raw.to_string(),
    }
}

/// A name made a path segment: `[a-z0-9_-]`, at most [DOMAIN_SEGMENT_MAX].
pub fn slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed: String = out.trim_matches(['-', '_']).chars().take(DOMAIN_SEGMENT_MAX).collect();
    let trimmed = trimmed.trim_end_matches(['-', '_']).to_string();
    if trimmed.is_empty() {
        "area".into()
    } else {
        trimmed
    }
}

/// Split the code files `files` (indexes into `paths`) into areas: the directories under
/// their common directory (a wrapper such as `src` opened), business stems across a
/// layered layout, or stems of a flat directory. What fits no area stays with the caller.
fn split(paths: &[String], files: &[usize]) -> Vec<Group> {
    let base = common_dir(files.iter().map(|f| paths[*f].as_str()));
    let prefix = shared_prefix(paths, files);
    let mut dirs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut loose: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for f in files {
        let path = &paths[*f];
        let rest = if base.is_empty() { path.as_str() } else { &path[base.len() + 1..] };
        match rest.split_once('/') {
            Some((child, _)) => dirs.entry(join(&base, child)).or_default().push(*f),
            None => loose.entry(base.clone()).or_default().push(*f),
        }
    }
    // A wrapper directory is opened where the code under it forks into two directories or
    // more - down a chain of single directories first (`src/main/java/com/acme/shop`),
    // twice at most.
    for _ in 0..2 {
        let wrappers: Vec<(String, String)> = dirs
            .iter()
            .filter(|(d, _)| WRAPPERS.contains(&last_segment(d).to_ascii_lowercase().as_str()))
            .filter_map(|(d, members)| {
                let deep = common_dir(members.iter().map(|f| paths[*f].as_str()));
                let inner: HashSet<&str> = members
                    .iter()
                    .filter_map(|f| paths[*f][deep.len() + 1..].split_once('/').map(|(c, _)| c))
                    .collect();
                (inner.len() >= 2).then(|| (d.clone(), deep))
            })
            .collect();
        if wrappers.is_empty() {
            break;
        }
        for (wrapper, deep) in wrappers {
            for f in dirs.remove(&wrapper).unwrap_or_default() {
                let rest = &paths[f][deep.len() + 1..];
                match rest.split_once('/') {
                    Some((child, _)) => dirs.entry(join(&deep, child)).or_default().push(f),
                    None => loose.entry(deep.clone()).or_default().push(f),
                }
            }
        }
    }
    let layered: Vec<&String> = dirs.keys().filter(|d| LAYERS.contains(&last_segment(d).to_ascii_lowercase().as_str())).collect();
    if layered.len() >= 2 {
        if let Some(groups) = layered_groups(paths, &dirs, &layered, prefix.as_deref()) {
            return groups;
        }
    }
    let mut groups: Vec<Group> = dirs
        .iter()
        .filter(|(_, fs)| fs.len() >= UNIT_FILES_MIN)
        .map(|(dir, fs)| Group {
            kind: UnitKind::Dir,
            name: last_segment(dir).to_string(),
            title: title_of(last_segment(dir)),
            base: dir.clone(),
            owns: vec![dir.clone()],
            files: fs.clone(),
            manifest: None,
            children: Vec::new(),
        })
        .collect();
    for (dir, fs) in &loose {
        let mut stems: BTreeMap<String, Stem> = BTreeMap::new();
        for f in fs {
            if let Some(stem) = stem_of(last_segment(&paths[*f]), prefix.as_deref()) {
                let entry = stems.entry(stem.key).or_default();
                entry.files.push(*f);
                entry.words.insert(stem.word);
                entry.leads.insert(stem.lead);
            }
        }
        for (key, Stem { files: members, words: spelled, leads, .. }) in stems {
            // `memory.rs` beside `memory/`: one area.
            let twin = groups
                .iter_mut()
                .find(|g| g.kind == UnitKind::Dir && dir_of(&g.base) == dir && stem_of(&g.name, None).is_some_and(|s| s.key == key));
            if let Some(group) = twin {
                for f in &members {
                    if group.owns.len() < DOMAIN_OWNS_MAX {
                        group.owns.push(paths[*f].clone());
                    }
                }
                group.files.extend(members);
                continue;
            }
            if members.len() < STEM_FILES_MIN {
                continue;
            }
            let owns: Vec<String> = leads.iter().map(|l| join(dir, &format!("{l}*"))).collect();
            let literal = stem_name(&spelled).unwrap_or(key);
            groups.push(Group {
                kind: UnitKind::Stem,
                name: literal.clone(),
                title: title_of(&literal),
                base: dir.clone(),
                owns,
                files: members,
                manifest: None,
                children: Vec::new(),
            });
        }
    }
    groups
}

/// One business stem: its files, the stem word's spellings, the names' leads up to it,
/// and (in a layered layout) the layers it is in.
#[derive(Debug, Default)]
struct Stem {
    files: Vec<usize>,
    words: BTreeSet<String>,
    leads: BTreeSet<String>,
    layers: BTreeSet<String>,
}

/// A layered layout's business areas: the stems found in two layers or more, the largest
/// [STEMS_MAX] of them by the files they own, each owning its files under the directory the layers lie in (never
/// beyond it: a file at the checkout's root makes the split's base the root); the other
/// directories stay directory areas. None when the layers share no business.
fn layered_groups(
    paths: &[String],
    dirs: &BTreeMap<String, Vec<usize>>,
    layered: &[&String],
    prefix: Option<&str>,
) -> Option<Vec<Group>> {
    let probes: Vec<String> = layered.iter().map(|d| join(d, "x")).collect();
    let base = common_dir(probes.iter().map(String::as_str));
    let base = base.as_str();
    let mut stems: BTreeMap<String, Stem> = BTreeMap::new();
    for layer in layered {
        for f in &dirs[*layer] {
            if let Some(stem) = stem_of(last_segment(&paths[*f]), prefix) {
                let entry = stems.entry(stem.key).or_default();
                entry.files.push(*f);
                entry.words.insert(stem.word);
                entry.leads.insert(stem.lead);
                entry.layers.insert((*layer).clone());
            }
        }
    }
    let across: Vec<(String, Stem)> = stems.into_iter().filter(|(_, s)| s.files.len() >= 2 && s.layers.len() >= 2).collect();
    if across.len() < 2 {
        return None;
    }
    // A directory in a layer named for a business (`web/dto/orders` for `team`,
    // `web/res/invoice` for `invoice`) is that business's whole: the files in it named for
    // nothing (`BuyerOrderStat`) too, and one named for another business (the directory, a
    // longer entry, owns it on the map). The outermost such directory names a file's, by the
    // longest business it starts with - among all of them, so a `feedback/` past the cap is
    // never `feed`'s - and goes to it when `kept` keeps it.
    let claim = |kept: &dyn Fn(usize) -> bool| {
        let mut named: Vec<BTreeSet<String>> = vec![BTreeSet::new(); across.len()];
        let mut moved: HashMap<usize, usize> = HashMap::new();
        for layer in layered {
            for f in &dirs[*layer] {
                let segments: Vec<&str> = paths[*f][layer.len() + 1..].split('/').collect();
                let mut at = (*layer).clone();
                for segment in &segments[..segments.len() - 1] {
                    at = join(&at, segment);
                    let lower = segment.to_ascii_lowercase();
                    if LAYERS.contains(&lower.as_str()) || ROLES.contains(&lower.as_str()) {
                        continue;
                    }
                    let owner = across
                        .iter()
                        .enumerate()
                        .filter(|(_, (key, _))| lower == *key || (key.len() >= 3 && lower.starts_with(key.as_str())))
                        .max_by_key(|(_, (key, _))| key.len());
                    if let Some((i, _)) = owner {
                        if kept(i) {
                            named[i].insert(at.clone());
                            moved.insert(*f, i);
                        }
                        break;
                    }
                }
            }
        }
        let mut owned: Vec<Vec<usize>> =
            across.iter().map(|(_, s)| s.files.iter().copied().filter(|f| !moved.contains_key(f)).collect()).collect();
        for (f, i) in &moved {
            owned[*i].push(*f);
        }
        (owned, named)
    };
    // A stem those directories took most files of (`OpenAble…` in `reports/` and
    // `exports/`) was a word the business uses, not a business: it is gone.
    let business = |i: usize, owned: &[Vec<usize>]| owned[i].len() >= 2 && owned[i].len() * 2 >= across[i].1.files.len();
    // The largest [STEMS_MAX] businesses by the files they own, their directories' too: `Pm`
    // names a few classes and its `web/req/pm`, `web/res/pm` and `remote/pm` hold many more.
    let (owned, _) = claim(&|_| true);
    let mut ranked: Vec<usize> = (0..across.len()).filter(|i| business(*i, &owned)).collect();
    ranked.sort_by(|a, b| owned[*b].len().cmp(&owned[*a].len()).then_with(|| across[*a].0.cmp(&across[*b].0)));
    ranked.truncate(STEMS_MAX);
    // Claimed again by the businesses kept: a directory named for one cut stays its files'.
    let (mut owned, mut named) = claim(&|i| ranked.contains(&i));
    let mut groups: Vec<Group> = ranked
        .iter()
        .map(|i| (&across[*i], std::mem::take(&mut owned[*i]), std::mem::take(&mut named[*i])))
        .map(|((key, stem), mut files, dirs)| {
            let literal = stem_name(&stem.words).unwrap_or_else(|| key.clone());
            files.sort_unstable();
            let mut owns: Vec<String> = stem.leads.iter().map(|l| join(base, &format!("**/{l}*"))).collect();
            owns.extend(dirs);
            Group {
                kind: UnitKind::Stem,
                name: literal.clone(),
                title: title_of(&literal),
                base: base.to_string(),
                owns,
                files,
                manifest: None,
                children: Vec::new(),
            }
        })
        .collect();
    for (dir, fs) in dirs {
        if !layered.contains(&dir) && fs.len() >= UNIT_FILES_MIN {
            groups.push(Group {
                kind: UnitKind::Dir,
                name: last_segment(dir).to_string(),
                title: title_of(last_segment(dir)),
                base: dir.clone(),
                owns: vec![dir.clone()],
                files: fs.clone(),
                manifest: None,
                children: Vec::new(),
            });
        }
    }
    Some(groups)
}

/// The source directories a root package's code lives in.
const ROOT_SOURCE: [&str; 7] = ["src", "lib", "app", "source", "cmd", "internal", "pkg"];

/// The parts of a large area: its split, where a split that finds the whole area again,
/// or one part, is no split - and one part holding the area's code is a step down to look
/// inside (a package whose sources sit one directory deeper, `web/src/…` beside a build
/// script), twice at most. A stem area holds some of each directory's files, never all of
/// one: its parts are its longer stems ([stem_parts]). Empty when the area has no two parts.
fn parts_of(paths: &[String], group: &Group) -> Vec<Group> {
    let cut = |group: &Group| if group.kind == UnitKind::Stem { stem_parts(paths, group) } else { split(paths, &group.files) };
    let mut parts = cut(group);
    parts.retain(|c| c.files.len() < group.files.len());
    for _ in 0..2 {
        if parts.len() != 1 {
            break;
        }
        let part = parts.remove(0);
        parts = cut(&part);
        parts.retain(|c| c.files.len() < part.files.len());
    }
    if parts.len() < 2 {
        return Vec::new();
    }
    parts.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.name.cmp(&b.name)));
    parts
}

/// The parts of a stem area: the files whose names go on past its stem with one more
/// business word (`Position` of `InvoiceLineMapper` in `invoice`), [STEM_FILES_MIN]
/// of them or more. A role's word (`InvoiceCtl`, `InvoiceMapper`) names no part. A part
/// owns its files the way the area does (`…/**/InvoiceLine*`), so it never reaches
/// past the area.
fn stem_parts(paths: &[String], group: &Group) -> Vec<Group> {
    // A stem area owns `<root><lead>*` entries; a lead never holds a `/`.
    let globs: Vec<(&str, &str)> = group
        .owns
        .iter()
        .filter_map(|entry| {
            let entry = entry.strip_suffix('*')?;
            let cut = entry.rfind('/').map_or(0, |at| at + 1);
            (!entry[cut..].contains(['*', '?'])).then(|| (&entry[..cut], &entry[cut..]))
        })
        .collect();
    let mut stems: BTreeMap<String, Stem> = BTreeMap::new();
    for f in &group.files {
        let name = last_segment(&paths[*f]);
        let Some((root, lead)) = globs.iter().filter(|(_, l)| !l.is_empty() && name.starts_with(l)).max_by_key(|(_, l)| l.len()) else {
            continue;
        };
        // The next word starts where the lead ends: `Contractor` goes on with no new word.
        let rest = &name[lead.len()..];
        if !rest.starts_with(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            continue;
        }
        let Some(stem) = stem_of(rest, None) else { continue };
        if ROLES.contains(&stem.key.as_str()) || LAYERS.contains(&stem.key.as_str()) {
            continue;
        }
        let entry = stems.entry(stem.key).or_default();
        entry.files.push(*f);
        entry.words.insert(stem.word);
        entry.leads.insert(format!("{root}{lead}{}*", stem.lead));
    }
    let mut parts: Vec<Group> = stems
        .into_iter()
        .filter(|(_, s)| s.files.len() >= STEM_FILES_MIN)
        .map(|(key, stem)| {
            let literal = stem_name(&stem.words).unwrap_or(key);
            Group {
                kind: UnitKind::Stem,
                name: literal.clone(),
                title: title_of(&literal),
                base: group.base.clone(),
                owns: stem.leads.into_iter().collect(),
                files: stem.files,
                manifest: None,
                children: Vec::new(),
            }
        })
        .collect();
    if parts.len() < 2 {
        return Vec::new();
    }
    parts.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.name.cmp(&b.name)));
    parts
}

/// Whether a group, or one of its sub-groups, owns a path.
fn covers(group: &Group, path: &str) -> bool {
    group.owns.iter().any(|entry| owns_path(entry, path)) || group.children.iter().any(|c| covers(c, path))
}

/// The areas of the support code no program area owns: one for the directory it all lies
/// in, or one per directory at the top of the checkout. A file alone at the root, or a
/// directory with fewer than [UNIT_FILES_MIN] such files, is no area.
fn support_groups(paths: &[String], files: &[usize]) -> Vec<Group> {
    let base = common_dir(files.iter().map(|f| paths[*f].as_str()));
    let mut dirs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for f in files {
        let dir = match paths[*f].split_once('/') {
            _ if !base.is_empty() => base.clone(),
            Some((top, _)) => top.to_string(),
            None => continue,
        };
        dirs.entry(dir).or_default().push(*f);
    }
    let mut groups: Vec<Group> = dirs
        .into_iter()
        .filter(|(_, fs)| fs.len() >= UNIT_FILES_MIN)
        .map(|(dir, fs)| Group {
            kind: UnitKind::Dir,
            name: last_segment(&dir).to_string(),
            title: title_of(last_segment(&dir)),
            owns: vec![dir.clone()],
            base: dir,
            files: fs,
            manifest: None,
            children: Vec::new(),
        })
        .collect();
    groups.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.name.cmp(&b.name)));
    groups
}

/// The top level: one area per package when the code is in packages (the root package
/// keeps its own source directories), else the directories and stems of the checkout;
/// then the large areas split, largest first ([split_largest_first]). Support code (schemas, scripts) is cut apart
/// first: it decides none of that, and what no area owns of it is areas of its own.
fn skeleton(paths: &[String], code: &[usize], manifests: &[Manifest]) -> Vec<Group> {
    let (support, program): (Vec<usize>, Vec<usize>) = code.iter().partition(|f| is_support(&paths[**f]));
    let (code, support) = if program.is_empty() { (support, Vec::new()) } else { (program, support) };
    let code = &code[..];
    let mut packages: Vec<(usize, &Manifest)> =
        manifests.iter().enumerate().filter(|(_, m)| !m.dir.is_empty()).collect();
    packages.sort_by(|a, b| a.1.dir.cmp(&b.1.dir));
    // The outermost packages only: a package inside one is that one's business.
    let mut outer: Vec<(usize, &Manifest)> = Vec::new();
    for (i, m) in packages {
        if !outer.iter().any(|(_, o)| m.dir.starts_with(&format!("{}/", o.dir))) {
            outer.push((i, m));
        }
    }
    let under = |dir: &str, path: &str| path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/';
    let mut top: Vec<Group> = Vec::new();
    let mut rest: Vec<usize> = code.to_vec();
    let in_packages: usize = code.iter().filter(|f| outer.iter().any(|(_, m)| under(&m.dir, &paths[**f]))).count();
    if outer.len() >= 2 || (outer.len() == 1 && in_packages * 10 >= code.len() * 3) {
        let mut slugs: Vec<String> = outer.iter().map(|(_, m)| slug(last_segment(&m.dir))).collect();
        strip_common_prefix(&mut slugs);
        for ((i, m), name) in outer.iter().zip(slugs) {
            let files: Vec<usize> = rest.iter().copied().filter(|f| under(&m.dir, &paths[*f])).collect();
            if files.is_empty() {
                continue;
            }
            rest.retain(|f| !under(&m.dir, &paths[*f]));
            top.push(Group {
                kind: UnitKind::Package,
                title: m.name.clone().unwrap_or_else(|| title_of(last_segment(&m.dir))),
                name,
                base: m.dir.clone(),
                owns: vec![m.dir.clone()],
                files,
                manifest: Some(*i),
                children: Vec::new(),
            });
        }
        if let Some((i, root)) = manifests.iter().enumerate().find(|(_, m)| m.dir.is_empty()) {
            let dirs: Vec<&str> = ROOT_SOURCE.iter().copied().filter(|d| rest.iter().any(|f| under(d, &paths[*f]))).collect();
            if !dirs.is_empty() {
                let files: Vec<usize> = rest.iter().copied().filter(|f| dirs.iter().any(|d| under(d, &paths[*f]))).collect();
                rest.retain(|f| !dirs.iter().any(|d| under(d, &paths[*f])));
                let name = root.name.clone().unwrap_or_else(|| "app".into());
                top.push(Group {
                    kind: UnitKind::Package,
                    title: name.clone(),
                    name,
                    base: String::new(),
                    owns: dirs.iter().map(|d| d.to_string()).collect(),
                    files,
                    manifest: Some(i),
                    children: Vec::new(),
                });
            }
        }
        if !rest.is_empty() {
            top.extend(split(paths, &rest));
        }
    } else {
        top = split(paths, code);
    }
    // One area holding all the program's code says nothing: its parts are the top level.
    if top.len() == 1 && top[0].files.len() >= SPLIT_FILES_MIN {
        let parts = parts_of(paths, &top[0]);
        if !parts.is_empty() {
            top = parts;
        }
    }
    top.sort_by(|a, b| b.files.len().cmp(&a.files.len()).then_with(|| a.name.cmp(&b.name)));
    let (mut stems, mut others) = (0, 0);
    top.retain(|g| {
        let (count, max) = if g.kind == UnitKind::Stem { (&mut stems, STEMS_MAX) } else { (&mut others, TOP_MAX) };
        *count += 1;
        *count <= max
    });
    let leftover: Vec<usize> = support.into_iter().filter(|f| !top.iter().any(|g| covers(g, &paths[*f]))).collect();
    let mut beside = support_groups(paths, &leftover);
    beside.truncate(TOP_MAX.saturating_sub(others).max(1));
    let room = UNITS_MAX.saturating_sub(top.len() + beside.len());
    split_largest_first(paths, &mut top, room);
    top.extend(beside);
    top
}

// The top level always fits the map: the split below spends what is left.
const _: () = assert!(TOP_MAX + STEMS_MAX + 1 < UNITS_MAX);

/// Split the areas largest first - each into its parts ([parts_of]), a part as soon as it
/// is the largest area left - while the map has `room` for more, [SKELETON_DEPTH] levels
/// deep at most: how much code an area holds decides how deep the map goes there, so a
/// 200-file area reaches its fourth level while a 20-file one stays whole. The last split
/// that fits keeps its largest parts.
fn split_largest_first(paths: &[String], top: &mut [Group], mut room: usize) {
    // (files, first come first, the indexes down to the area)
    let mut queue: BinaryHeap<(usize, Reverse<usize>, Vec<usize>)> =
        top.iter().enumerate().map(|(i, group)| (group.files.len(), Reverse(i), vec![i])).collect();
    let mut seen = top.len();
    while let Some((files, _, route)) = queue.pop() {
        if files < SPLIT_FILES_MIN || room < 2 {
            break;
        }
        if route.len() >= SKELETON_DEPTH {
            continue;
        }
        let Some((first, rest)) = route.split_first() else { continue };
        let mut group = &mut top[*first];
        for i in rest {
            group = &mut group.children[*i];
        }
        let mut parts = parts_of(paths, group);
        parts.truncate(room.min(CHILDREN_MAX));
        if parts.len() < 2 {
            continue;
        }
        room -= parts.len();
        for (i, part) in parts.iter().enumerate() {
            let mut down = route.clone();
            down.push(i);
            queue.push((part.files.len(), Reverse(seen), down));
            seen += 1;
        }
        group.children = parts;
    }
}

/// The paths the top-level groups take on the map, in order: each name's slug, a second
/// one alike numbered (as [Survey::build] places them).
fn top_paths(groups: &[Group]) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    groups
        .iter()
        .map(|group| {
            let base = slug(&group.name);
            let mut path = base.clone();
            let mut n = 2;
            while taken.contains(&path) {
                path = format!("{base}-{n}");
                n += 1;
            }
            taken.insert(path.clone());
            path
        })
        .collect()
}

/// The skeleton with a review's regrouping applied (SPEC §memory.map): each business
/// domain stands where its first member stood. One member is renamed; members merged
/// become one area owning what each owned (nested instead when that is more entries than
/// a domain holds); members nested become the sub-areas of a new parent that owns nothing
/// itself. An entry naming no group left is skipped.
fn regrouped(groups: Vec<Group>, regroup: Option<&Regroup>) -> Vec<Group> {
    let Some(regroup) = regroup.filter(|r| !r.domains.is_empty()) else { return groups };
    let paths = top_paths(&groups);
    let index: HashMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
    let mut slots: Vec<Option<Group>> = groups.into_iter().map(Some).collect();
    let mut placed: Vec<Option<Group>> = vec![None; slots.len()];
    for entry in &regroup.domains {
        let at: Vec<usize> = entry.members.iter().filter_map(|m| index.get(m.as_str()).copied()).collect();
        let Some(&first) = at.iter().find(|i| slots[**i].is_some()) else { continue };
        let mut members: Vec<Group> = at.iter().filter_map(|i| slots[*i].take()).collect();
        let title = if entry.title.trim().is_empty() { title_of(&entry.path) } else { entry.title.clone() };
        let probes: Vec<String> = members.iter().map(|g| join(&g.base, "x")).collect();
        let base = common_dir(probes.iter().map(String::as_str));
        // A merge owning more entries than a domain holds would lose code: its members nest.
        let fits = members.iter().map(|m| m.owns.len()).sum::<usize>() <= DOMAIN_OWNS_MAX;
        let group = if members.len() == 1 {
            let mut only = members.remove(0);
            only.name = entry.path.clone();
            only.title = title;
            only
        } else if entry.merge && fits {
            let mut owns: Vec<String> = Vec::new();
            let mut files: Vec<usize> = Vec::new();
            let mut children: Vec<Group> = Vec::new();
            for member in members {
                for entry in member.owns {
                    if !owns.contains(&entry) {
                        owns.push(entry);
                    }
                }
                files.extend(member.files);
                children.extend(member.children);
            }
            files.sort_unstable();
            files.dedup();
            children.truncate(CHILDREN_MAX);
            Group { kind: UnitKind::Group, name: entry.path.clone(), title, base, owns, files, manifest: None, children }
        } else {
            let files: Vec<usize> = members.iter().flat_map(|m| m.files.iter().copied()).collect();
            Group { kind: UnitKind::Group, name: entry.path.clone(), title, base, owns: Vec::new(), files, manifest: None, children: members }
        };
        placed[first] = Some(group);
    }
    placed.into_iter().zip(slots).filter_map(|(placed, left)| placed.or(left)).collect()
}

/// `swiss-core`, `swiss-memory` → `core`, `memory`: a prefix every sibling shares says nothing.
fn strip_common_prefix(slugs: &mut [String]) {
    if slugs.len() < 2 {
        return;
    }
    let first = slugs[0].clone();
    let mut cut = 0;
    for (i, c) in first.char_indices() {
        if c == '-' || c == '_' {
            let prefix = &first[..=i];
            if slugs.iter().all(|s| s.starts_with(prefix) && s.len() > prefix.len()) {
                cut = i + 1;
            } else {
                break;
            }
        }
    }
    if cut > 0 {
        for s in slugs.iter_mut() {
            *s = s[cut..].to_string();
        }
    }
}

// --- the service ---------------------------------------------------------------------------

impl MemoryService {
    /// Draft a project's map from its checkout as it is now (SPEC §memory.map): the code
    /// index refreshed first, then read whole; manifests, READMEs and the heads of a few
    /// files read from the checkout, redacted, credential-named files never opened.
    pub async fn map_survey(&self, project_id: &str, draw: &Draw) -> MemResult<Survey> {
        let target = Target::project(project_id);
        let (project, existing) = {
            let mut conn = self.store().await?.read().await?;
            let project = store::project(&mut conn, project_id)
                .await?
                .ok_or_else(|| MemError::not_found(format!("no project {project_id}")))?;
            (project.name, store::domains(&mut conn, project_id).await?)
        };
        let root = self.code_root(&target).await?;
        self.code.refresh(&root).await?;
        let key = crate::code::root_key(&root);
        let row = self.code.root_row(&key).await?.ok_or_else(|| MemError::store("the code index lost its root"))?;
        let mut conn = self.code.db().await?.pool.acquire().await?;
        let listed = db::survey_files(&mut conn, row.id).await?;
        let defs = db::survey_defs(&mut conn, row.id).await?;
        let uses = db::survey_uses(&mut conn, row.id).await?;
        let imports = db::survey_imports(&mut conn, row.id).await?;
        drop(conn);
        let by_id: HashMap<i64, usize> = listed.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
        let indexed = Indexed {
            files: listed
                .into_iter()
                .map(|f| Listed { path: f.path, lang: f.lang, read: !f.skipped, size: f.size })
                .collect(),
            defs: defs
                .into_iter()
                .filter_map(|(file, name, kind, line, fanin)| by_id.get(&file).map(|f| Def { file: *f, name, kind, line, fanin }))
                .collect(),
            uses: uses
                .into_iter()
                .filter_map(|(src, dst, name)| Some((*by_id.get(&src)?, *by_id.get(&dst)?, name)))
                .collect(),
            imports: imports.into_iter().filter_map(|(file, spec)| by_id.get(&file).map(|f| (*f, spec))).collect(),
            truncated: row.truncated,
        };
        let draw = draw.clone();
        tokio::task::spawn_blocking(move || {
            let read = |rel: &str, max: usize| read_checkout(&root, rel, max);
            Survey::build(&project, &indexed, &existing, &read, &draw)
        })
        .await
        .map_err(|err| MemError::store(format!("the survey failed: {err}")))
    }
}

/// At most `max` bytes of a checkout file's text, redacted; None for a credential-named,
/// missing or binary file.
pub fn read_checkout(root: &Path, rel: &str, max: usize) -> Option<String> {
    use std::io::Read;
    if secrets::sensitive_path(rel) || rel.split('/').any(|part| part == "..") {
        return None;
    }
    let file = std::fs::File::open(root.join(rel)).ok()?;
    let mut bytes = Vec::with_capacity(max.min(64 * 1024));
    file.take(max as u64).read_to_end(&mut bytes).ok()?;
    if bytes.iter().take(8 * 1024).any(|b| *b == 0) {
        return None;
    }
    Some(secrets::redact(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests;
