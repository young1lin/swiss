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

//! The manifests and the relations (SPEC §memory.map): packages and their dependencies read off
//! Cargo.toml, package.json, go.mod and their kin, and who imports whom between the areas.

use super::*;

/// The value of `key = "value"` inside the TOML table `table`.
fn toml_value(text: &str, table: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == format!("[{table}]");
            continue;
        }
        if !inside {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        if k.trim() == key {
            let v = v.trim();
            let quote = v.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            return v[1..].find(quote).map(|end| v[1..1 + end].to_string());
        }
    }
    None
}

/// The text inside the first `<tag>` of an XML text.
fn xml_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{tag}>"))?;
    Some(text[start..start + end].trim().to_string())
}

/// Read one manifest; None when the file says it is no package (a Cargo workspace alone).
pub fn read_manifest(path: &str, text: &str) -> Option<Manifest> {
    let file = last_segment(path).to_string();
    let mut out = Manifest { dir: dir_of(path).to_string(), file: file.clone(), name: None, description: None, ids: Vec::new(), deps: None };
    match file.as_str() {
        "Cargo.toml" => {
            out.name = Some(toml_value(text, "package", "name")?);
            out.description = toml_value(text, "package", "description");
            out.deps = Some(cargo_deps(text));
        }
        "pyproject.toml" => {
            out.name = toml_value(text, "project", "name").or_else(|| toml_value(text, "tool.poetry", "name"));
            out.description =
                toml_value(text, "project", "description").or_else(|| toml_value(text, "tool.poetry", "description"));
        }
        "package.json" | "composer.json" => {
            let value: Value = serde_json::from_str(text).ok()?;
            out.name = value.get("name").and_then(Value::as_str).map(str::to_string);
            out.description = value.get("description").and_then(Value::as_str).map(str::to_string);
            if file == "package.json" {
                let tables = ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];
                out.deps = Some(
                    tables
                        .iter()
                        .filter_map(|t| value.get(*t).and_then(Value::as_object))
                        .flat_map(|deps| deps.keys().map(|k| package_key(k)))
                        .collect(),
                );
            }
        }
        "go.mod" => {
            out.name = text.lines().find_map(|l| l.trim().strip_prefix("module ")).map(|m| m.trim().to_string());
            out.deps = Some(go_deps(text));
        }
        "pom.xml" => {
            // The parent's coordinates come first; the project's own follow it.
            let own = match text.find("</parent>") {
                Some(at) => &text[at..],
                None => text,
            };
            out.name = xml_value(own, "name").or_else(|| xml_value(own, "artifactId"));
            out.description = xml_value(own, "description");
            // A dependency names the artifact, never the display name.
            out.ids.extend(xml_value(own, "artifactId").map(|a| package_key(&a)));
            out.deps = Some(
                own.split("<dependency>").skip(1).filter_map(|block| xml_value(block, "artifactId")).map(|a| package_key(&a)).collect(),
            );
        }
        "pubspec.yaml" => {
            let field = |key: &str| {
                text.lines().find_map(|l| l.strip_prefix(key)).map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            };
            out.name = field("name:");
            out.description = field("description:");
        }
        _ => {}
    }
    out.name = out.name.filter(|n| !n.trim().is_empty());
    out.description = out.description.map(|d| clip(d.trim(), 300)).filter(|d| !d.is_empty());
    if out.file != "pom.xml" {
        out.ids.extend(out.name.as_deref().map(package_key));
    }
    Some(out)
}

/// A package name as dependencies compare it: lowercase, `_` as `-` (Cargo treats them alike).
fn package_key(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// What a Cargo manifest depends on: every key of its `dependencies`, `dev-dependencies` and
/// `build-dependencies` tables (a target's included), a renamed one by its `package`. The
/// workspace's own table declares versions for the members and is no dependency.
fn cargo_deps(text: &str) -> Vec<String> {
    const TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut out: Vec<String> = Vec::new();
    let mut table = false;
    // `[dependencies.name]`: the entry its header names, which a `package` line renames.
    let mut single: Option<usize> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            let header = t.trim_start_matches('[').trim_end_matches(']').trim();
            table = false;
            single = None;
            if header.starts_with("workspace.") {
                continue;
            }
            let segments: Vec<&str> = header.split('.').map(|s| s.trim().trim_matches(['"', '\''])).collect();
            match segments.iter().rposition(|s| TABLES.contains(s)) {
                Some(at) if at + 1 == segments.len() => table = true,
                Some(at) if at + 2 == segments.len() => {
                    out.push(package_key(segments[at + 1]));
                    single = Some(out.len() - 1);
                }
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = t.split_once('=') else { continue };
        let key = key.trim();
        if table {
            // `name = "1"`, `name = { path = "..", package = "real" }`, `name.workspace = true`.
            let name = key.split('.').next().unwrap_or(key).trim().trim_matches(['"', '\'']);
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            out.push(package_key(&inline_package(value).unwrap_or_else(|| name.to_string())));
        } else if let Some(at) = single {
            if key == "package" {
                if let Some(real) = quoted(value.trim()) {
                    out[at] = package_key(&real);
                }
            }
        }
    }
    out
}

/// The `package = "..."` inside an inline table, if any - the key, not the word inside a
/// path or a feature (`path = "../package"`).
fn inline_package(value: &str) -> Option<String> {
    value.match_indices("package").find_map(|(at, word)| {
        let before = value[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '"' | '\'' | '/' | '.')) {
            return None;
        }
        let rest = value[at + word.len()..].trim_start().strip_prefix('=')?;
        quoted(rest.trim_start())
    })
}

/// The text of a leading `"..."` or `'...'`.
fn quoted(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    text[1..].find(quote).map(|end| text[1..1 + end].to_string())
}

/// The modules a `go.mod` requires, from `require x v` lines and `require ( … )` blocks.
fn go_deps(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block = false;
    for line in text.lines() {
        let t = line.split("//").next().unwrap_or("").trim();
        if block {
            if t == ")" {
                block = false;
            } else if let Some(module) = t.split_whitespace().next() {
                out.push(package_key(module));
            }
        } else if let Some(rest) = t.strip_prefix("require ").or_else(|| t.strip_prefix("require(")) {
            let rest = rest.trim();
            if rest.starts_with('(') || t.ends_with('(') {
                block = true;
            } else if let Some(module) = rest.split_whitespace().next() {
                out.push(package_key(module));
            }
        }
    }
    out
}

/// The files a repository marks as not its own source in its root `.gitattributes` -
/// `linguist-generated` (emitted code, committed) or `linguist-vendored` - as (glob, set)
/// rules in file order: the last rule matching a path decides, as in Git.
pub(super) fn marked_rules(text: &str) -> Vec<(String, bool)> {
    let mut rules = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("[attr]") {
            continue;
        }
        let mut parts = t.split_whitespace();
        let Some(pattern) = parts.next() else { continue };
        let mut set = None;
        for attr in parts {
            let attr = attr.to_ascii_lowercase();
            let (name, on) = match attr.strip_prefix('-').or_else(|| attr.strip_prefix('!')) {
                Some(name) => (name.to_string(), false),
                None => match attr.split_once('=') {
                    Some((name, value)) => (name.to_string(), value != "false"),
                    None => (attr.clone(), true),
                },
            };
            if name == "linguist-generated" || name == "linguist-vendored" {
                set = Some(on);
            }
        }
        if let Some(on) = set {
            // A pattern without a slash matches a name at any depth; one with a slash is
            // anchored at the root.
            let pattern = pattern.trim_end_matches('/');
            let glob = match pattern.strip_prefix('/') {
                Some(anchored) => anchored.to_string(),
                None if pattern.contains('/') => pattern.to_string(),
                None => format!("**/{pattern}"),
            };
            rules.push((glob, on));
        }
    }
    rules
}

/// Whether the rules mark `path` as not the repository's own source.
pub(super) fn marked(rules: &[(String, bool)], path: &str) -> bool {
    let mut out = false;
    for (glob, on) in rules {
        let hit = match glob.strip_prefix("**/") {
            // `**/name` at the root as well as below it.
            Some(name) if !name.contains('/') => crate::glob::matches(name, last_segment(path)),
            _ => crate::glob::matches(glob, path),
        };
        if hit {
            out = *on;
        }
    }
    out
}

pub(super) fn is_manifest(path: &str) -> bool {
    let name = last_segment(path);
    MANIFESTS.contains(&name) || name.ends_with(".csproj")
}

// --- relations ---------------------------------------------------------------------------

/// What an import spec names in this checkout: a listed file, or a directory.
pub(super) struct Resolver<'a> {
    listed: HashMap<&'a str, usize>,
    /// A path without its extension, by its last two segments.
    tails: HashMap<String, Vec<usize>>,
    /// Package name → directory (Rust names with `_`).
    packages: HashMap<String, String>,
    /// Go module path → directory.
    modules: Vec<(String, String)>,
    files: &'a [Listed],
}

impl<'a> Resolver<'a> {
    pub(super) fn new(files: &'a [Listed], manifests: &[Manifest]) -> Resolver<'a> {
        let mut tails: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            let no_ext = match f.path.rsplit_once('.') {
                Some((head, _)) if !head.ends_with('/') && head.len() > dir_of(&f.path).len() => head,
                _ => f.path.as_str(),
            };
            let parts: Vec<&str> = no_ext.rsplit('/').take(2).collect();
            let key = parts.iter().rev().copied().collect::<Vec<_>>().join("/");
            tails.entry(key).or_default().push(i);
        }
        let mut packages = HashMap::new();
        let mut modules = Vec::new();
        for m in manifests {
            if let Some(name) = &m.name {
                packages.insert(name.replace('-', "_"), m.dir.clone());
                packages.insert(name.clone(), m.dir.clone());
                if m.file == "go.mod" {
                    modules.push((name.clone(), m.dir.clone()));
                }
            }
        }
        modules.sort_by_key(|m| std::cmp::Reverse(m.0.len()));
        Resolver { listed: files.iter().enumerate().map(|(i, f)| (f.path.as_str(), i)).collect(), tails, packages, modules, files }
    }

    fn file(&self, path: &str) -> Option<String> {
        self.listed.get(path).map(|i| self.files[*i].path.clone())
    }

    /// The listed path an import from `from` names, or a directory of the checkout.
    pub(super) fn resolve(&self, from: &str, spec: &str) -> Option<String> {
        let spec = spec.trim();
        if spec.is_empty() || spec.len() > 300 {
            return None;
        }
        if spec.starts_with("./") || spec.starts_with("../") {
            let joined = normalize(&join(dir_of(from), spec))?;
            return self.with_extensions(&joined);
        }
        if let Some(rest) = spec.strip_prefix("crate::") {
            return self.rust_module(from, rest);
        }
        if let Some((module, dir)) = self.modules.iter().find(|(m, _)| spec == m || spec.starts_with(&format!("{m}/"))) {
            return Some(join(dir, spec[module.len()..].trim_start_matches('/')).trim_end_matches('/').to_string());
        }
        let first = spec.split(['/', ':', '.', '\\']).next().unwrap_or(spec);
        let scoped = if spec.starts_with('@') { spec.splitn(3, '/').take(2).collect::<Vec<_>>().join("/") } else { String::new() };
        if let Some(dir) = self.packages.get(&scoped).or_else(|| self.packages.get(first)) {
            return Some(dir.clone()).filter(|d| !d.is_empty());
        }
        // A quoted include, `a.b.Class` or `a\b\C`: by the path's last two segments.
        let path_like = if spec.contains('/') {
            spec.rsplit_once('.').filter(|(_, e)| e.len() <= 4).map(|(h, _)| h.to_string()).unwrap_or_else(|| spec.to_string())
        } else {
            spec.trim_end_matches(".*").replace(['.', '\\'], "/")
        };
        let parts: Vec<&str> = path_like.rsplit('/').take(2).collect();
        let key = parts.iter().rev().copied().collect::<Vec<_>>().join("/");
        match self.tails.get(&key).map(Vec::as_slice) {
            Some([only]) => Some(self.files[*only].path.clone()),
            Some(many) => many.iter().map(|i| &self.files[*i].path).find(|p| dir_of(p) == dir_of(from)).cloned(),
            None => None,
        }
    }

    fn with_extensions(&self, base: &str) -> Option<String> {
        if let Some(found) = self.file(base) {
            return Some(found);
        }
        for ext in ["ts", "tsx", "js", "jsx", "mjs", "vue", "svelte", "py", "rb", "php", "h", "hpp", "rs", "go"] {
            if let Some(found) = self.file(&format!("{base}.{ext}")) {
                return Some(found);
            }
        }
        for index in ["index.ts", "index.tsx", "index.js", "index.jsx", "mod.rs", "__init__.py"] {
            if let Some(found) = self.file(&join(base, index)) {
                return Some(found);
            }
        }
        None
    }

    /// `crate::a::b` from a file of a Rust package: `src/a/b.rs`, `src/a.rs`, `src/a/`.
    fn rust_module(&self, from: &str, rest: &str) -> Option<String> {
        let mut dir = dir_of(from);
        let src = loop {
            if self.listed.contains_key(join(dir, "lib.rs").as_str()) || self.listed.contains_key(join(dir, "main.rs").as_str()) {
                break dir.to_string();
            }
            if dir.is_empty() {
                return None;
            }
            dir = dir_of(dir);
        };
        let segments: Vec<&str> = rest.split("::").take_while(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).collect();
        for n in (1..=segments.len()).rev() {
            let module = join(&src, &segments[..n].join("/"));
            for candidate in [format!("{module}.rs"), join(&module, "mod.rs")] {
                if let Some(found) = self.file(&candidate) {
                    return Some(found);
                }
            }
        }
        None
    }
}

/// `a/./b/../c` → `a/c`; None when it climbs out of the checkout.
fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out.join("/"))
}
