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

//! Platform seams: file privacy, the machine id, process walking, DPAPI, the kill-on-close job
//! guard and local pseudo-terminals all live behind this module so the rest of the crate stays
//! platform-agnostic. It is also where `unsafe` is allowed to live (AGENTS.md): every FFI call
//! the gateway makes is in this directory, and nothing above it needs any.

pub mod pty;

#[cfg(target_os = "macos")]
mod macos_keychain;
#[cfg(target_os = "macos")]
pub use macos_keychain::{keychain_create, keychain_read};

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

// Pure /proc text parsers, un-`cfg`'d so any host runs their tests (the same contract as
// `walk_descendants` below); only the I/O that feeds them is Linux-only.
mod procfs;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    descendant_pids, detach_command, detach_command_without_breakaway, dpapi_protect,
    dpapi_unprotect, expand_env, keep_std_handles_from_children, machine_id, machine_path_read,
    parent_process, pid_alive, process_created_at, process_name, process_tree_working_set,
    process_working_set, run_entry_read, run_entry_remove, run_entry_write, self_private_bytes,
    self_working_set, tcp_listener_pid, tcp_listeners, tree_kill, user_path_read,
    user_path_write,
};

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::{
    descendant_pids, detach_command, dpapi_protect, dpapi_unprotect, keep_std_handles_from_children,
    kill_process_group, machine_id, parent_process, pid_alive, process_created_at, process_name,
    process_tree_working_set, process_working_set, self_private_bytes, self_working_set,
    tcp_listeners, tree_kill,
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

/// What a walk is for, which decides how an edge whose creation times cannot be read counts
/// (SPEC §process.tree): a wrong MEMORY number is repaired by the next poll, a wrong KILL is
/// not - so measuring keeps the edge and killing drops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkMode {
    /// Count edges whose times cannot be read - over-reporting a number is the safe direction.
    Measure,
    /// Drop edges whose times cannot be read - never act on a link that may reach a stranger.
    Kill,
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
    mode: WalkMode,
    created_at: &mut dyn FnMut(u32) -> Option<u64>,
) -> std::collections::HashSet<u32> {
    let mut queue: std::collections::VecDeque<u32> = roots.iter().copied().collect();
    let mut tree: std::collections::HashSet<u32> = std::collections::HashSet::new();
    while let Some(pid) = queue.pop_front() {
        // `tree.insert` is also the cycle guard: a snapshot is taken process by process and can
        // come back with a parent loop after pid reuse, which would otherwise spin here forever.
        if Some(pid) == exclude || !tree.insert(pid) {
            continue;
        }
        // The parent's time is read once per reached pid (the caller's lookup memoizes); a root
        // with no readable time still stands - it was named explicitly, not reached by an edge.
        let parent_created = created_at(pid);
        for (child, parent) in parents.iter() {
            if *parent != pid {
                continue;
            }
            // Toolhelp's parent pid outlives the parent: once a pid is reused, the new process
            // "adopts" the dead one's orphans, and a walk from it would reach strangers. An edge
            // counts only when the child was created at or after its parent (SPEC §process.tree).
            let child_created = created_at(*child);
            let edge_counts = match (parent_created, child_created) {
                (Some(parent_at), Some(child_at)) => child_at >= parent_at,
                // Unreadable times: keep the edge for a measurement, drop it for a kill.
                _ => mode == WalkMode::Measure,
            };
            if edge_counts {
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

    /// Measure with no creation times at all: every edge counts, which is exactly the
    /// semantics the older tests below were written against.
    fn walk(roots: &[u32], exclude: Option<u32>) -> HashSet<u32> {
        walk_descendants(&snapshot(), roots, exclude, WalkMode::Measure, &mut |_: u32| None)
    }

    #[test]
    fn the_walk_drops_a_child_created_before_its_parent() {
        // The stale Toolhelp link (SPEC §process.tree): pid 12's snapshot edge names 11 as its
        // parent, but 12 was created BEFORE 11 - the parent's pid was reused and the newcomer
        // "adopted" the dead one's orphan. Measure or Kill, the edge is a lie, and a walk from
        // 11 must not reach 12 or anything behind it.
        let times = HashMap::from([(11u32, 200u64), (12u32, 100u64), (13u32, 300u64)]);
        for mode in [WalkMode::Measure, WalkMode::Kill] {
            let tree = walk_descendants(
                &snapshot(),
                &[11],
                None,
                mode,
                &mut |pid: u32| times.get(&pid).copied(),
            );
            assert_eq!(tree, HashSet::from([11]), "a reused pid adopted a stranger ({mode:?})");
        }
    }

    #[test]
    fn an_edge_without_times_counts_for_measuring_not_for_killing() {
        // 12's creation time cannot be read (a pid that refuses PROCESS_QUERY, one already
        // exiting): the memory reading keeps the edge - a wrong number is repaired by the next
        // poll - and the kill drops it, because acting on an unreadable link could reach a
        // stranger (SPEC §process.tree).
        let times = HashMap::from([(11u32, 100u64), (13u32, 300u64)]);
        let measure = walk_descendants(
            &snapshot(),
            &[11],
            None,
            WalkMode::Measure,
            &mut |pid: u32| times.get(&pid).copied(),
        );
        assert_eq!(measure, HashSet::from([11, 12, 13]), "measuring keeps unreadable edges");
        let kill = walk_descendants(
            &snapshot(),
            &[11],
            None,
            WalkMode::Kill,
            &mut |pid: u32| times.get(&pid).copied(),
        );
        assert_eq!(kill, HashSet::from([11]), "killing drops unreadable edges");
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
            walk_descendants(&looped, &[1], None, WalkMode::Measure, &mut |_: u32| None),
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
