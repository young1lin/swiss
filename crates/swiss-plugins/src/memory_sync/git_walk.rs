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

//! The git walk (SPEC §memory.sync): the commits a sync run reads since its cursor,
//! and how much waits to be read.

use super::*;

/// What one run sends: the commits (with their files), the changed files, the patch.
#[derive(Debug, Default)]
pub struct Walk {
    /// The first sync: only the starting point is recorded.
    pub baseline: bool,
    pub base: Option<String>,
    /// The commit the run advances to (the last one it sends, or HEAD when it sends none).
    pub head: String,
    pub commits: Vec<CommitFiles>,
    pub more_commits: bool,
    pub changes: Vec<Change>,
    /// The raw `diff -M --unified=20` over the kept files; [engine::shape_patch] shapes it.
    pub patch_raw: String,
}

/// The commits since `state.head` (or `since`), at most `max`, their files and statuses,
/// and the patch - a first sync records only the baseline. A head that is no longer an
/// ancestor of HEAD gives way to their merge base.
pub async fn walk(root: &Path, state: &engine::SyncState, since: Option<&str>, max: usize) -> MemResult<Walk> {
    let head = git(root, &["rev-parse", "HEAD"], 64).await?.text().trim().to_string();
    if head.is_empty() {
        return Err(MemError::invalid("the checkout has no commits yet; there is nothing to sync"));
    }
    let start = match (&state.head, since) {
        (Some(h), _) if h.len() == 7 || h.len() == 8 || h.len() == 40 => h.clone(),
        (_, Some(s)) => preview::revision(s)?,
        _ => String::new(),
    };
    let mut walk = Walk { baseline: start.is_empty(), base: None, head: head.clone(), ..Walk::default() };
    if walk.baseline {
        return Ok(walk);
    }
    // A rebase or a branch switch: the base gives way to the merge base.
    let base = match git(root, &["merge-base", &start, &head], 64).await {
        Ok(out) => out.text().trim().to_string(),
        Err(_) => String::new(),
    };
    let base = if base.is_empty() { start } else { base };
    walk.base = Some(base.clone());
    if base == head {
        return Ok(walk);
    }
    // The unsynced commits, oldest first, at most `max`; the rest wait.
    let listed = git(root, &["rev-list", "--topo-order", "--reverse", "--parents", &format!("{base}..{head}")], 4 << 20)
        .await?
        .text();
    let synced: HashSet<&str> = state.synced.iter().map(String::as_str).collect();
    let (chosen, end, waiting) = choose_commits(&listed, &head, &synced, max);
    walk.more_commits = waiting > 0;
    if chosen.is_empty() {
        return Ok(walk);
    }
    walk.head = end;
    // Subjects and files in one walk: NUL hash US subject US file US file ... NUL hash ...
    let log = git(
        root,
        &[
            "log", "--topo-order", "--reverse", "--no-merges", "--format=%x00%H%x01%ct%x01%s%x01", "--name-only",
            &format!("{base}..{}", walk.head),
        ],
        4 << 20,
    )
    .await?
    .text();
    let set: HashSet<&str> = chosen.iter().map(String::as_str).collect();
    let mut commits = Vec::new();
    let mut files: HashSet<String> = HashSet::new();
    let mut rest = log.as_str();
    while let Some(open) = rest.find('\0') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('\u{1}') else { break };
        let hash = &after[..close];
        let tail = &after[close + 1..];
        // The last record has no closing NUL: it runs to the end of the output.
        let end = tail.find('\0');
        let block = &tail[..end.unwrap_or(tail.len())];
        // committer epoch US subject US the name-only lines
        let mut parts = block.splitn(3, '\u{1}');
        let at_ms = parts.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let subject = parts.next().unwrap_or("");
        let file_block = parts.next().unwrap_or("");
        if set.contains(hash) {
            let mut commit_files = Vec::new();
            for file in file_block.split('\n').map(str::trim).filter(|f| !f.is_empty()) {
                if !preview::sensitive(file) {
                    commit_files.push(file.to_string());
                    files.insert(file.to_string());
                }
            }
            commits.push(CommitFiles {
                hash: hash.to_string(),
                at_ms,
                subject: subject.trim().to_string(),
                files: commit_files,
            });
        }
        // Keep the closing NUL: it opens the next record.
        rest = match end {
            Some(end) => &tail[end..],
            None => "",
        };
    }
    walk.commits = commits;
    // Statuses over the whole range, kept to the run's files.
    let statuses = git(root, &["diff", "--name-status", "--no-renames", "-z", &base, &walk.head], 4 << 20)
        .await?
        .text();
    let mut fields = statuses.split('\0').map(|field| field.trim_start_matches('\n'));
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if status.is_empty() || path.is_empty() {
            break;
        }
        let status = status.chars().next().unwrap_or('M');
        if !preview::sensitive(path) && files.contains(path) {
            walk.changes.push(Change { status, path: path.to_string() });
        }
    }
    // The patch over the kept files only: one pathspec separator, then the names.
    let mut args: Vec<String> = vec!["diff".into(), "-M".into(), "--unified=20".into(), base.clone(), walk.head.clone()];
    if !files.is_empty() {
        args.push("--".into());
        for file in &files {
            args.push(file.clone());
        }
    }
    if files.is_empty() {
        // Every changed file was credential-named: there is no patch to send.
        return Ok(walk);
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    walk.patch_raw = git(root, &arg_refs, PATCH_READ_BYTES).await?.text();
    Ok(walk)
}

/// The commits a run sends out of `listed` (`rev-list --topo-order --reverse --parents`
/// over the range, oldest first), the commit it ends on, and how many unsynced commits
/// wait after it. A run ends on a step of HEAD's first-parent line - the furthest whose
/// unsynced non-merge commits stay within `max` - so `base..end` holds exactly what it
/// sends: ending on a branch's own commit would leave out the other side of a merge. A
/// first step over `max` alone sends its oldest `max` and ends on the last of them.
pub(super) fn choose_commits(listed: &str, head: &str, synced: &HashSet<&str>, max: usize) -> (Vec<String>, String, usize) {
    let rows: Vec<Vec<&str>> =
        listed.lines().map(|line| line.split_whitespace().collect::<Vec<_>>()).filter(|row| !row.is_empty()).collect();
    let parents: HashMap<&str, &[&str]> = rows.iter().map(|row| (row[0], &row[1..])).collect();
    // HEAD's first-parent line inside the range, oldest first.
    let mut line = Vec::new();
    let mut at = head;
    while let Some(up) = parents.get(at) {
        line.push(at);
        match up.first() {
            Some(first) => at = first,
            None => break,
        }
    }
    line.reverse();
    // The step of the line that brings each commit in.
    let mut step: HashMap<&str, usize> = HashMap::new();
    for (i, tip) in line.iter().enumerate() {
        let mut stack = vec![*tip];
        while let Some(commit) = stack.pop() {
            if step.contains_key(commit) {
                continue;
            }
            // A parent outside the range is the base's side.
            let Some(up) = parents.get(commit) else { continue };
            step.insert(commit, i);
            stack.extend(up.iter().copied());
        }
    }
    let pending: Vec<(&str, usize)> = rows
        .iter()
        .filter(|row| row.len() <= 2 && !synced.contains(row[0]))
        .map(|row| (row[0], step.get(row[0]).copied().unwrap_or(usize::MAX)))
        .collect();
    let mut sent = 0;
    let mut end = None;
    for i in 0..line.len() {
        sent += pending.iter().filter(|(_, s)| *s == i).count();
        if sent > max {
            break;
        }
        end = Some(i);
    }
    if let Some(k) = end.filter(|k| pending.iter().any(|(_, s)| s <= k)) {
        let chosen: Vec<String> = pending.iter().filter(|(_, s)| *s <= k).map(|(h, _)| h.to_string()).collect();
        let waiting = pending.len() - chosen.len();
        return (chosen, line[k].to_string(), waiting);
    }
    // Nothing unsynced, or the first step that brings any is over `max` alone.
    let first = pending.iter().map(|(_, s)| *s).min();
    let chosen: Vec<String> =
        pending.iter().filter(|(_, s)| Some(*s) == first).take(max).map(|(h, _)| h.to_string()).collect();
    let end = chosen.last().cloned().unwrap_or_else(|| head.to_string());
    let waiting = pending.len() - chosen.len();
    (chosen, end, waiting)
}

/// What is pending since the head, for the panel: the unsynced commits and the
/// unread transcript bytes.
pub async fn pending(root: &Path, dirs: &[PathBuf], roots: &[String], state: &engine::SyncState) -> (u64, u64) {
    let commits = match &state.head {
        Some(head) if head.len() >= 7 => {
            let base = match git(root, &["merge-base", head, "HEAD"], 64).await {
                Ok(out) => out.text().trim().to_string(),
                Err(_) => String::new(),
            };
            let base = if base.is_empty() { head.clone() } else { base };
            git(root, &["rev-list", "--count", "--no-merges", &format!("{base}..HEAD")], 16)
                .await
                .ok()
                .and_then(|out| out.text().trim().parse::<u64>().ok())
                .unwrap_or(0)
        }
        _ => 0,
    };
    let sources = transcript::sources(dirs, roots);
    let bytes = transcript::pending_bytes(&sources, &state.cursors);
    (commits, bytes)
}
