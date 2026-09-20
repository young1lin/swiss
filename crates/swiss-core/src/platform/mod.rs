/*
 * Copyright 2026 The swiss authors
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

//! Platform seams: file privacy, the machine id, process walking, DPAPI, the kill-on-close job
//! guard and local pseudo-terminals all live behind this module so the rest of the crate stays
//! platform-agnostic. It is also where `unsafe` is allowed to live (AGENTS.md): every FFI call
//! the gateway makes is in this directory, and nothing above it needs any.

pub mod pty;

mod privfs;
pub use privfs::{
    chmod_private, mkdir_private, private_file_mode, write_file_private, PRIVATE_DIR_MODE,
    PRIVATE_FILE_MODE,
};

/// The Windows subtree-kill guard. Every child the gateway spawns — proc MCPs, scheduled jobs,
/// local terminals — is assigned through it, so ADR-008 is one mechanism rather than several.
#[cfg(windows)]
mod job;
#[cfg(windows)]
pub use job::KillOnCloseJob;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    descendant_pids, dpapi_protect, dpapi_unprotect, keep_std_handles_from_children, machine_id,
    parent_process, pid_alive, process_tree_working_set, run_entry_read, run_entry_remove,
    run_entry_write, self_private_bytes, self_working_set, tree_kill,
};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::{
    descendant_pids, dpapi_protect, dpapi_unprotect, machine_id, parent_process, pid_alive,
    process_tree_working_set, self_private_bytes, self_working_set, tree_kill,
};

/// The process that spawned THIS one, as far as the platform can say without a subprocess:
/// pid plus image name. The 2026-09-14 deploy incident spent an hour of archaeology on
/// "who started this daemon" - the snapshot answer (a pid and an exe name, taken at boot
/// while the launcher is usually still alive) would have settled it in one log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentProcess {
    pub pid: u32,
    pub name: String,
}

/// BFS from `roots` down a pid -> parent map, returning every pid in the subtrees.
///
/// The two platform walks disagreed about this for as long as they were written out twice, so it
/// lives here once, un-`cfg`'d, and is tested on whatever host runs the suite: a snapshot is a
/// plain map either way, and the rules over it are not platform-specific.
///
/// `exclude` drops one pid and everything reached only through it. The memory walk passes the
/// gateway's own pid — the panel reports the gateway separately, so counting it here would double
/// it. The reaper passes None, because its root IS this process and the whole point is the set of
/// pids that belong to this instance.
pub(crate) fn walk_descendants(
    parents: &std::collections::HashMap<u32, u32>,
    roots: &[u32],
    exclude: Option<u32>,
) -> std::collections::HashSet<u32> {
    let mut queue: std::collections::VecDeque<u32> = roots.iter().copied().collect();
    let mut tree: std::collections::HashSet<u32> = std::collections::HashSet::new();
    while let Some(pid) = queue.pop_front() {
        // `tree.insert` is also the cycle guard: a snapshot is taken process by process and can
        // come back with a parent loop after pid reuse, which would otherwise spin here forever.
        if Some(pid) == exclude || !tree.insert(pid) {
            continue;
        }
        for (child, parent) in parents.iter() {
            if *parent == pid {
                queue.push_back(*child);
            }
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// `cmd.exe -> npx -> node`, the shape a proc MCP actually has, plus an unrelated neighbour.
    fn snapshot() -> HashMap<u32, u32> {
        HashMap::from([
            (1, 0),   // init/system
            (10, 1),  // the gateway
            (11, 10), // cmd.exe
            (12, 11), // npx
            (13, 12), // the real server
            (20, 1),  // a neighbour process
            (21, 20), // its child
        ])
    }

    fn walk(roots: &[u32], exclude: Option<u32>) -> HashSet<u32> {
        walk_descendants(&snapshot(), roots, exclude)
    }

    #[test]
    fn follows_a_chain_to_the_bottom_not_just_the_first_child() {
        // The whole reason the walk exists: on Windows killing cmd.exe does not cascade, so an
        // npx-launched MCP leaves node running. Depth 3 is the common case, not the exotic one.
        assert_eq!(walk(&[11], None), HashSet::from([11, 12, 13]));
    }

    #[test]
    fn a_neighbours_subtree_is_never_swept_in() {
        // This is the reaper's PID-reuse guard. A walk that over-reported here would let the
        // gateway kill a process that was never its own.
        let mine = walk(&[10], None);
        assert!(mine.contains(&13));
        assert!(!mine.contains(&20), "{mine:?}");
        assert!(!mine.contains(&21), "{mine:?}");
    }

    #[test]
    fn the_root_itself_is_part_of_its_own_tree() {
        // reap_proc_pids roots the walk at its own pid and asks "is this pid mine?"; a root left
        // out of its own answer would make the gateway a candidate for its own reaper.
        assert!(walk(&[10], None).contains(&10));
    }

    #[test]
    fn several_roots_merge_into_one_set_without_double_counting() {
        // The memory walk passes every managed child at once, and two roots can share a subtree
        // when one MCP was launched from another.
        assert_eq!(walk(&[11, 12], None), HashSet::from([11, 12, 13]));
    }

    #[test]
    fn an_excluded_pid_takes_the_branch_below_it_with_it() {
        // The memory walk excludes the gateway, and the gateway is the ROOT of everything here —
        // so this is the case that decides whether the panel double-counts the gateway or reports
        // nothing at all. Rooting at the gateway and excluding it must come back empty.
        assert!(walk(&[10], Some(10)).is_empty());
        // Rooted at the children instead, the exclusion touches nothing.
        assert_eq!(walk(&[11], Some(10)), HashSet::from([11, 12, 13]));
    }

    #[test]
    fn a_parent_loop_terminates_instead_of_spinning() {
        // Snapshots are taken process by process, so pid reuse mid-walk can hand back a cycle.
        let looped = HashMap::from([(1, 2), (2, 3), (3, 1)]);
        assert_eq!(
            walk_descendants(&looped, &[1], None),
            HashSet::from([1, 2, 3])
        );
    }

    #[test]
    fn a_pid_the_snapshot_never_saw_is_just_itself() {
        // A child that exited between the ledger write and the walk: it is not in the map, and
        // must come back as a lone pid rather than as an error or an empty set.
        assert_eq!(walk(&[999], None), HashSet::from([999]));
    }

    #[test]
    fn no_roots_is_an_empty_walk_not_the_whole_machine() {
        // A gateway with no proc MCPs configured passes an empty root list every poll.
        assert!(walk(&[], None).is_empty());
    }
}
