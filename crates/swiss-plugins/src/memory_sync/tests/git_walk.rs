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

//! The git walk and the hooks that nudge a sync.

use super::*;

#[test]
fn hooks_install_and_uninstall_the_marked_block() {
    if !git_present() {
        return;
    }
    let dir = scratch("hooks");
    git_at(&dir, &["init", "-q"]);
    // A LOCAL hooks path: a machine-wide core.hooksPath would otherwise point the
    // install at the operator's real hooks directory.
    let hooks = scratch("hookshome");
    git_at(&dir, &["config", "core.hooksPath", &hooks.display().to_string()]);
    // An operator hook of their own: kept through both directions.
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let pre = hooks.join("pre-push");
    std::fs::write(&pre, "#!/bin/sh\necho mine\n").expect("write");
    let exe = scratch("exe").join("swiss.exe");
    let touched = hooks_install(&dir, &exe, "C:/homes/sw", 19_998).expect("install");
    assert_eq!(touched.len(), 2);
    let text = std::fs::read_to_string(&pre).expect("read");
    assert!(text.starts_with("#!/bin/sh"), "the shebang stays first: {text}");
    assert!(text.contains("memory sync --nudge") && text.contains("SWISS_PORT=19998"), "{text}");
    assert!(text.contains("echo mine"), "the operator's line stays: {text}");
    hooks_install(&dir, &exe, "C:/homes/sw", 19_998).expect("again");
    let again = std::fs::read_to_string(&pre).expect("read");
    assert_eq!(again.matches(HOOK_BEGIN).count(), 1, "idempotent: {again}");
    hooks_uninstall(&dir).expect("uninstall");
    let after = std::fs::read_to_string(&pre).expect("read");
    assert!(!after.contains("swiss memory"), "the block is gone: {after}");
    assert!(after.contains("echo mine"), "the operator's own line stays: {after}");
    // A file the block alone filled disappears entirely.
    assert!(!hooks.join("post-merge").exists(), "a block-only file is deleted");
}

#[tokio::test]
async fn the_walk_baselines_then_walks_commits() {
    if !git_present() {
        return;
    }
    let dir = repo("walk");
    let head = head_of(&dir);
    let first = walk(&dir, &engine::SyncState::default(), None, engine::COMMITS_MAX).await.expect("baseline");
    assert!(first.baseline);
    assert_eq!(first.head, head);
    let c1 = {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "HEAD~1"])
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let state = engine::SyncState { head: Some(c1.clone()), ..Default::default() };
    let walked = walk(&dir, &state, None, engine::COMMITS_MAX).await.expect("walk");
    assert!(!walked.baseline);
    assert_eq!(walked.commits.len(), 1);
    assert_eq!(walked.commits[0].subject, "third: three");
    assert_eq!(walked.head, head);
    let paths: Vec<&str> = walked.changes.iter().map(|c| c.path.as_str()).collect();
    assert!(paths.contains(&"c.txt"), "statuses carry the change: {paths:?}");
    assert!(!paths.contains(&".env"), "the credential-named file never enters: {paths:?}");
    assert!(!walked.commits[0].files.iter().any(|f| f == ".env"), "nor the files: {:?}", walked.commits[0].files);
    assert!(walked.patch_raw.contains("three"), "the patch carries c.txt");
    assert!(!walked.patch_raw.contains("SECRET="), "the patch drops the credential file");
    let done = engine::SyncState { head: Some(head.clone()), ..Default::default() };
    let current = walk(&dir, &done, None, engine::COMMITS_MAX).await.expect("current");
    assert!(current.commits.is_empty());
    assert_eq!(current.head, head);
    let since = walk(&dir, &engine::SyncState::default(), Some(&c1), engine::COMMITS_MAX).await.expect("since");
    assert_eq!(since.commits.len(), 1);
}

#[tokio::test]
async fn a_rebased_head_gives_way_to_the_merge_base() {
    if !git_present() {
        return;
    }
    let dir = repo("rebase");
    git_at(&dir, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(dir.join("s.txt"), "side\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "side: s"]);
    let side_head = head_of(&dir);
    git_at(&dir, &["checkout", "-q", "-"]);
    std::fs::write(dir.join("d.txt"), "main\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "main: d"]);
    let state = engine::SyncState { head: Some(side_head), ..Default::default() };
    let walked = walk(&dir, &state, None, engine::COMMITS_MAX).await.expect("walk");
    assert_eq!(walked.commits.len(), 2, "from the merge base to HEAD");
    assert!(!walked.commits.iter().any(|c| c.subject == "side: s"), "the vanished branch is not walked");
}

#[tokio::test]
async fn a_run_ends_on_heads_first_parent_line_so_a_merges_other_side_comes_with_it() {
    if !git_present() {
        return;
    }
    let dir = repo("merge");
    let base = head_of(&dir);
    // A side branch off the base, a commit on the main line, then a merge: HEAD is the
    // merge, and neither side's newest commit has the other side behind it.
    git_at(&dir, &["checkout", "-q", "-b", "side"]);
    let mut side = Vec::new();
    for (file, subject) in [("s1.txt", "side: one"), ("s2.txt", "side: two")] {
        std::fs::write(dir.join(file), "side\n").expect("write");
        git_at(&dir, &["add", "."]);
        git_at(&dir, &["commit", "-qm", subject]);
        side.push(head_of(&dir));
    }
    git_at(&dir, &["checkout", "-q", "-"]);
    std::fs::write(dir.join("m.txt"), "main\n").expect("write");
    git_at(&dir, &["add", "."]);
    git_at(&dir, &["commit", "-qm", "main: one"]);
    let main = head_of(&dir);
    git_at(&dir, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let merge = head_of(&dir);
    let subjects = |walked: &Walk| {
        let mut all: Vec<String> = walked.commits.iter().map(|c| c.subject.clone()).collect();
        all.sort();
        all
    };
    let state = engine::SyncState { head: Some(base.clone()), ..Default::default() };
    let walked = walk(&dir, &state, None, engine::COMMITS_MAX).await.expect("walk");
    assert_eq!(subjects(&walked), ["main: one", "side: one", "side: two"], "both sides of the merge");
    assert_eq!(walked.head, merge);
    assert!(!walked.more_commits);
    assert!(walked.patch_raw.contains("m.txt") && walked.patch_raw.contains("s2.txt"), "the patch holds both sides");
    // At most two: the line's first step alone, and the merged side waits.
    let walked = walk(&dir, &state, None, 2).await.expect("walk");
    assert_eq!(subjects(&walked), ["main: one"]);
    assert_eq!(walked.head, main);
    assert!(walked.more_commits);
    // Synced commits are neither sent nor counted as waiting.
    let state = engine::SyncState { head: Some(base), synced: side.into_iter().collect(), ..Default::default() };
    let walked = walk(&dir, &state, None, engine::COMMITS_MAX).await.expect("walk");
    assert_eq!(subjects(&walked), ["main: one"]);
    assert_eq!(walked.head, merge);
    assert!(!walked.more_commits);
}

#[test]
fn a_first_step_over_the_limit_alone_sends_its_oldest_commits() {
    // rev-list --topo-order --reverse --parents: m merges x1..x3 onto the base (b0, outside
    // the range), y follows it.
    let listed = "x1 b0\nx2 x1\nx3 x2\nm b0 x3\ny m\n";
    let none = HashSet::new();
    let (chosen, end, waiting) = choose_commits(listed, "y", &none, 2);
    assert_eq!(chosen, ["x1", "x2"]);
    assert_eq!((end.as_str(), waiting), ("x2", 2));
    // Room for the whole step: the run ends on the merge, the line's own commit.
    let (chosen, end, waiting) = choose_commits(listed, "y", &none, 3);
    assert_eq!(chosen, ["x1", "x2", "x3"]);
    assert_eq!((end.as_str(), waiting), ("m", 1));
    // Every commit synced: nothing is sent and the run ends on HEAD.
    let synced: HashSet<&str> = ["x1", "x2", "x3", "y"].into_iter().collect();
    let (chosen, end, waiting) = choose_commits(listed, "y", &synced, 2);
    assert!(chosen.is_empty());
    assert_eq!((end.as_str(), waiting), ("y", 0));
}
