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

//! Building the draft (SPEC §memory.map): the areas off the skeleton and the manifests, their
//! levels (children before parents), and the plan and outline a run is shown.

use super::*;

/// The deepest area owning `path`; between owners of one depth, the longer `owns` entry.
fn owner_of(units: &[Unit], path: &str) -> Option<usize> {
    let mut best: Option<(usize, (usize, usize))> = None;
    for (i, unit) in units.iter().enumerate() {
        let depth = unit.path.split('/').count();
        for entry in &unit.owns {
            let key = (depth, entry.len());
            if best.is_none_or(|(_, have)| key > have) && owns_path(entry, path) {
                best = Some((i, key));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// A text file's head, at most `lines` lines and `bytes` bytes, cut at a line.
fn head(text: &str, lines: usize, bytes: usize) -> String {
    let mut out = String::new();
    for line in text.lines().take(lines) {
        if out.len() + line.len() + 1 > bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What reads a checkout's text for the survey: a repo-relative path to at most `max`
/// bytes of its text - None for a missing, binary or credential-named file. The text
/// arrives redacted.
pub type Reader<'a> = dyn Fn(&str, usize) -> Option<String> + 'a;

impl Survey {
    /// Draft the map of `project` from its index. `existing` is the map it has: an area
    /// whose path is on it is kept as it is, and one whose code it already owns is left out.
    /// `draw` carries a review's regrouping and the domains to draw as if absent.
    pub fn build(project: &str, indexed: &Indexed, existing: &[Domain], read: &Reader<'_>, draw: &Draw) -> Survey {
        let existing: Vec<Domain> = existing.iter().filter(|d| !draw.ignore.contains(&d.path)).cloned().collect();
        let existing = &existing[..];
        let files = &indexed.files;
        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        let rules = read(".gitattributes", MANIFEST_BYTES).map(|text| marked_rules(&text)).unwrap_or_default();
        let code: Vec<usize> = (0..files.len()).filter(|i| is_code(&files[*i]) && !marked(&rules, &paths[*i])).collect();
        let manifests: Vec<Manifest> = files
            .iter()
            .filter(|f| is_manifest(&f.path) && !f.path.split('/').any(|d| d.starts_with('.') || SKIP_DIRS.contains(&d)))
            .filter_map(|f| read(&f.path, MANIFEST_BYTES).and_then(|text| read_manifest(&f.path, &text)))
            .collect();
        let groups = regrouped(skeleton(&paths, &code, &manifests), draw.regroup.as_ref());

        // Name the groups on the map: unique among siblings, parents before children.
        let mut units: Vec<Unit> = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut place = |group: &Group, parent: Option<usize>, units: &mut Vec<Unit>| -> usize {
            let prefix = parent.map(|p| format!("{}/", units[p].path)).unwrap_or_default();
            let base = slug(&group.name);
            let mut path = format!("{prefix}{base}");
            let mut n = 2;
            while taken.contains(&path) {
                path = format!("{prefix}{base}-{n}");
                n += 1;
            }
            taken.insert(path.clone());
            units.push(Unit {
                path,
                title: clip(&group.title, DOMAIN_TITLE_MAX),
                kind: group.kind,
                base: group.base.clone(),
                owns: group.owns.iter().take(DOMAIN_OWNS_MAX).cloned().collect(),
                parent,
                children: Vec::new(),
                files: Vec::new(),
                langs: Vec::new(),
                bytes: 0,
                manifest: group.manifest.map(|i| manifests[i].clone()),
                readme: None,
                listing: Vec::new(),
                symbols: Vec::new(),
                entries: Vec::new(),
                uses: Vec::new(),
                used_by: Vec::new(),
                excerpts: Vec::new(),
                kept: false,
            });
            let at = units.len() - 1;
            if let Some(p) = parent {
                units[p].children.push(at);
            }
            at
        };
        // Depth first, so each area's sub-areas follow it before its next sibling.
        let mut stack: Vec<(&Group, Option<usize>)> = groups.iter().rev().map(|g| (g, None)).collect();
        while let Some((group, parent)) = stack.pop() {
            let at = place(group, parent, &mut units);
            stack.extend(group.children.iter().rev().map(|c| (c, Some(at))));
        }

        // What the map has already: kept paths, and areas whose code it owns left out.
        let map = Map::new(existing);
        let owners = map.owners();
        let have: HashSet<&str> = existing.iter().map(|d| d.path.as_str()).collect();
        let mapped_files = code.iter().filter(|f| owners.owner(&paths[**f]).is_some()).count();
        let mut owner: Vec<Option<usize>> = vec![None; files.len()];
        for f in &code {
            owner[*f] = owner_of(&units, &paths[*f]);
        }
        for (i, unit) in units.iter_mut().enumerate() {
            unit.files = code.iter().filter(|f| owner[**f] == Some(i)).map(|f| paths[*f].clone()).collect();
        }
        let mut dropped: HashSet<usize> = HashSet::new();
        for (i, unit) in units.iter_mut().enumerate() {
            if have.contains(unit.path.as_str()) {
                unit.kept = true;
                continue;
            }
            let parent_gone = unit.parent.is_some_and(|p| dropped.contains(&p));
            let owned = unit.files.iter().filter(|p| owners.owner(p).is_some()).count();
            if parent_gone || (!unit.files.is_empty() && owned * 2 >= unit.files.len()) {
                dropped.insert(i);
            }
        }
        // A review's parent owning no code itself goes when every sub-area went.
        for i in (0..units.len()).rev() {
            let unit = &units[i];
            if unit.kind == UnitKind::Group && !unit.kept && unit.files.is_empty() && unit.children.iter().all(|c| dropped.contains(c)) {
                dropped.insert(i);
            }
        }

        // Each area's facts: languages, size, files with their most used names, symbols, entries.
        let mut defs_of: HashMap<usize, Vec<&Def>> = HashMap::new();
        let mut defined: HashMap<String, Vec<String>> = HashMap::new();
        for def in &indexed.defs {
            defs_of.entry(def.file).or_default().push(def);
            let at = defined.entry(def.name.clone()).or_default();
            if at.len() < 4 {
                at.push(paths[def.file].clone());
            }
        }
        let index_of: HashMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
        for unit in units.iter_mut() {
            let mut langs: BTreeMap<String, usize> = BTreeMap::new();
            let mut ranked: Vec<(String, u64, u64, Vec<String>)> = Vec::new();
            let mut symbols: Vec<Symbol> = Vec::new();
            for path in &unit.files {
                let i = index_of[path.as_str()];
                let lang = files[i].lang.clone().unwrap_or_else(|| extension(path));
                *langs.entry(lang).or_default() += 1;
                let size = files[i].size.max(0) as u64;
                unit.bytes += size;
                let mut defs: Vec<&Def> = defs_of.get(&i).cloned().unwrap_or_default();
                defs.sort_by(|a, b| b.fanin.cmp(&a.fanin).then_with(|| a.line.cmp(&b.line)));
                let weight: u64 = defs.iter().map(|d| d.fanin as u64).sum();
                let shown: Vec<String> = defs.iter().take(FILE_DEFS_SHOWN).map(|d| d.name.clone()).collect();
                ranked.push((path.clone(), size, weight, shown));
                for d in defs {
                    let named = d.name.len() >= 3 && !matches!(d.name.as_str(), "main" | "new" | "default" | "init");
                    if named && !matches!(d.kind, "const" | "static" | "var" | "mod" | "macro") {
                        symbols.push(Symbol { name: d.name.clone(), kind: d.kind, path: path.clone(), fanin: d.fanin });
                    }
                }
                let name = last_segment(path);
                if ENTRY_FILES.contains(&name) || name.ends_with("Application.java") || name.ends_with("Application.kt") {
                    unit.entries.push(path.clone());
                }
            }
            unit.langs = langs.into_iter().collect();
            unit.langs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ranked.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)).then_with(|| a.0.cmp(&b.0)));
            unit.listing = ranked.into_iter().take(PROMPT_FILES_MAX).map(|(p, s, _, d)| (p, s, d)).collect();
            symbols.sort_by(|a, b| b.fanin.cmp(&a.fanin).then_with(|| a.name.cmp(&b.name)));
            symbols.dedup_by(|a, b| a.name == b.name);
            symbols.truncate(SYMBOLS_MAX);
            unit.symbols = symbols;
            unit.entries.sort();
            unit.entries.truncate(6);
        }

        // Relations: names used across areas, and imports that resolve to another area.
        let resolver = Resolver::new(files, &manifests);
        // A name is matched by its spelling alone, so a use counts only where a dependency
        // could hold it: within one language, and from one package into another only when
        // the first one's manifest depends on the second.
        let package: Vec<Option<usize>> = paths
            .iter()
            .map(|path| {
                (0..manifests.len())
                    .filter(|m| manifests[*m].dir.is_empty() || path.starts_with(&format!("{}/", manifests[*m].dir)))
                    .max_by_key(|m| manifests[*m].dir.len())
            })
            .collect();
        let may_use = |src: usize, dst: usize| {
            if let (Some(a), Some(b)) = (&files[src].lang, &files[dst].lang) {
                if a != b {
                    return false;
                }
            }
            match (package[src], package[dst]) {
                (Some(a), Some(b)) if a != b => match &manifests[a].deps {
                    Some(deps) if !manifests[b].ids.is_empty() => manifests[b].ids.iter().any(|id| deps.contains(id)),
                    _ => true,
                },
                _ => true,
            }
        };
        // A name the mentioning area defines itself is its own (`FORMAT_VERSION` in two
        // packages): the mention binds there, not to the other definition.
        let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
        for def in &indexed.defs {
            definers.entry(def.name.as_str()).or_default().push(def.file);
        }
        let mut edges: HashMap<(usize, usize), (u32, Vec<String>)> = HashMap::new();
        let mut add = |from: usize, to: usize, via: Option<&str>| {
            if from == to {
                return;
            }
            let edge = edges.entry((from, to)).or_default();
            edge.0 += 1;
            if let Some(name) = via {
                if edge.1.len() < 4 && !edge.1.iter().any(|n| n == name) {
                    edge.1.push(name.to_string());
                }
            }
        };
        for (src, dst, name) in &indexed.uses {
            if let (Some(a), Some(b)) = (owner.get(*src).copied().flatten(), owner.get(*dst).copied().flatten()) {
                let own = definers.get(name.as_str()).is_some_and(|at| at.iter().any(|f| owner.get(*f).copied().flatten() == Some(a)));
                if !own && may_use(*src, *dst) {
                    add(a, b, Some(name));
                }
            }
        }
        for (file, spec) in &indexed.imports {
            let Some(from) = owner.get(*file).copied().flatten() else { continue };
            let Some(target) = resolver.resolve(&paths[*file], spec) else { continue };
            let to = match index_of.get(target.as_str()) {
                Some(i) => owner[*i],
                None => owner_of(&units, &target).or_else(|| owner_of(&units, &format!("{target}/x"))),
            };
            if let Some(to) = to {
                add(from, to, None);
            }
        }
        // A relation inside one family is the parent's business: between areas it is
        // drawn at the level both share, a child to its parent's siblings' sub-areas included.
        let related = |a: usize, b: usize| {
            let line = |mut u: usize| {
                let mut out = vec![u];
                while let Some(p) = units[u].parent {
                    out.push(p);
                    u = p;
                }
                out
            };
            line(a).contains(&b) || line(b).contains(&a)
        };
        let mut uses: Vec<Vec<Relation>> = vec![Vec::new(); units.len()];
        let mut used_by: Vec<Vec<Relation>> = vec![Vec::new(); units.len()];
        let mut lift: HashMap<(usize, usize), (u32, Vec<String>)> = HashMap::new();
        for ((a, b), (weight, via)) in edges {
            if related(a, b) {
                continue;
            }
            let pairs = {
                let mut out = vec![(a, b)];
                // The parents see their children's relations too.
                let (pa, pb) = (units[a].parent.unwrap_or(a), units[b].parent.unwrap_or(b));
                if (pa, pb) != (a, b) && pa != pb && !related(pa, pb) {
                    out.push((pa, pb));
                }
                out
            };
            for pair in pairs {
                let entry = lift.entry(pair).or_default();
                entry.0 += weight;
                for name in &via {
                    if entry.1.len() < 4 && !entry.1.contains(name) {
                        entry.1.push(name.clone());
                    }
                }
            }
        }
        for ((a, b), (weight, via)) in lift {
            uses[a].push(Relation { unit: b, weight, via: via.clone() });
            used_by[b].push(Relation { unit: a, weight, via });
        }
        for (i, unit) in units.iter_mut().enumerate() {
            let order = |list: &mut Vec<Relation>| {
                list.sort_by(|x, y| y.weight.cmp(&x.weight).then_with(|| x.unit.cmp(&y.unit)));
                list.truncate(RELATIONS_MAX);
            };
            unit.uses = std::mem::take(&mut uses[i]);
            order(&mut unit.uses);
            unit.used_by = std::mem::take(&mut used_by[i]);
            order(&mut unit.used_by);
        }

        // The texts: the README beside each area, and the heads of its first files.
        let readme_in = |dir: &str| {
            ["README.md", "readme.md", "README", "README.rst", "README.txt"]
                .iter()
                .find_map(|name| read(&join(dir, name), README_BYTES * 2))
                .map(|text| head(&text, 40, README_BYTES))
                .filter(|text| !text.trim().is_empty())
        };
        let readme = readme_in("");
        for (i, unit) in units.iter_mut().enumerate() {
            if dropped.contains(&i) || unit.kept {
                continue;
            }
            if matches!(unit.kind, UnitKind::Package | UnitKind::Dir) && !unit.base.is_empty() {
                unit.readme = readme_in(&unit.base);
            }
            let mut picks: Vec<String> = unit.entries.clone();
            for (path, _, _) in &unit.listing {
                if !picks.contains(path) {
                    picks.push(path.clone());
                }
            }
            for path in picks.into_iter().take(EXCERPTS_MAX) {
                let Some(text) = read(&path, EXCERPT_BYTES * 4) else { continue };
                let shown = head(&text, EXCERPT_LINES, EXCERPT_BYTES);
                if !shown.trim().is_empty() {
                    let tag = extension(&path);
                    unit.excerpts.push((path, tag, shown));
                }
            }
        }

        // The draft keeps what it will create and the kept parents of those.
        let mut keep: Vec<bool> = (0..units.len()).map(|i| !dropped.contains(&i)).collect();
        for i in 0..units.len() {
            if units[i].kept && !units.iter().enumerate().any(|(j, u)| u.parent == Some(i) && !dropped.contains(&j) && !u.kept) {
                keep[i] = false;
            }
        }
        let renumber: Vec<Option<usize>> = {
            let mut n = 0;
            keep.iter()
                .map(|k| {
                    if *k {
                        n += 1;
                        Some(n - 1)
                    } else {
                        None
                    }
                })
                .collect()
        };
        let owned_files = units.iter().enumerate().filter(|(i, _)| keep[*i]).map(|(_, u)| u.files.len()).sum();
        let mut sources: Vec<(String, u64)> = code.iter().map(|f| (paths[*f].clone(), files[*f].size.max(0) as u64)).collect();
        sources.extend(files.iter().filter(|f| is_doc(&f.path) && !marked(&rules, &f.path)).map(|f| (f.path.clone(), f.size.max(0) as u64)));
        sources.sort();
        let mut kept_units: Vec<Unit> = Vec::new();
        for (i, mut unit) in units.into_iter().enumerate() {
            if !keep[i] {
                continue;
            }
            unit.parent = unit.parent.and_then(|p| renumber[p]);
            unit.children = unit.children.iter().filter_map(|c| renumber[*c]).collect();
            let remap = |list: Vec<Relation>| -> Vec<Relation> {
                list.into_iter().filter_map(|r| renumber[r.unit].map(|unit| Relation { unit, ..r })).collect()
            };
            unit.uses = remap(unit.uses);
            unit.used_by = remap(unit.used_by);
            kept_units.push(unit);
        }

        Survey {
            project: project.to_string(),
            units: kept_units,
            readme,
            truncated: indexed.truncated,
            code_files: code.len(),
            owned_files,
            mapped_files,
            sources,
            listed: paths.into_iter().collect(),
            defined,
            on_map: existing.iter().map(|d| d.path.clone()).collect(),
        }
    }

    /// The areas to ask about, children before parents: each inner list may be asked at once.
    pub fn levels(&self) -> Vec<Vec<usize>> {
        let ask = |depth: usize| -> Vec<usize> {
            (0..self.units.len()).filter(|i| !self.units[*i].kept && self.units[*i].path.split('/').count() == depth).collect()
        };
        (1..=DOMAIN_DEPTH_MAX).rev().map(ask).filter(|level| !level.is_empty()).collect()
    }

    /// How many areas will be asked about.
    pub fn asks(&self) -> usize {
        self.units.iter().filter(|u| !u.kept).count()
    }

    /// The draft as `--plan` prints it and a run's scope keeps it.
    pub fn plan(&self) -> Vec<PlanUnit> {
        self.units
            .iter()
            .map(|u| PlanUnit {
                path: u.path.clone(),
                title: u.title.clone(),
                kind: u.kind,
                owns: u.owns.clone(),
                files: u.files.len(),
                uses: u.uses.iter().map(|r| self.units[r.unit].path.clone()).collect(),
                kept: u.kept,
            })
            .collect()
    }

    /// The draft in a few lines, for a terminal.
    pub fn outline(&self) -> String {
        let mut out = String::new();
        for u in &self.units {
            let indent = "  ".repeat(u.path.split('/').count() - 1);
            let kept = if u.kept { " (on the map already)" } else { "" };
            let _ = writeln!(out, "{indent}{}  {}  · {} file(s) · owns {}{kept}", u.path, u.title, u.files.len(), u.owns.join(", "));
            if !u.uses.is_empty() {
                let uses: Vec<&str> = u.uses.iter().map(|r| self.units[r.unit].path.as_str()).collect();
                let _ = writeln!(out, "{indent}  uses {}", uses.join(", "));
            }
        }
        if self.units.is_empty() && self.mapped_files > 0 {
            let _ = writeln!(out, "(nothing new to draw: the map already owns {} of the code files)", self.mapped_files);
        } else if self.units.is_empty() {
            out.push_str("(no area found: the code index holds no code outside tests, docs and vendored files)\n");
        }
        let _ = writeln!(out, "{} of {} code file(s) owned{}", self.owned_files, self.code_files, if self.truncated { "; the listing was cut" } else { "" });
        out
    }
}
