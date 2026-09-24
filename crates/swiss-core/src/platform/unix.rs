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

//! The non-Windows platform seam: machine id from /etc/machine-id, working sets from /proc,
//! liveness and killing through libc.
//!
//! It exports exactly the same names, with exactly the same signatures, as `windows.rs` — the two
//! drifted apart once (`descendant_pids` returned a bare `Vec` here and an `Option` there, which
//! did not compile against the shared caller), and `mod.rs` now re-exports both sides from one
//! list so a future divergence is a build error rather than a surprise on another platform.
//!
//! The /proc reads degrade honestly on macOS, which has no /proc: the walkers return `None`
//! ("unknown"), never a confident zero, and callers fail closed on that. `pid_alive` and
//! `tree_kill` go through libc and work everywhere.

use std::collections::HashMap;

use super::ParentProcess;

/// The machine id: `/etc/machine-id`, then `/var/lib/dbus/machine-id`. World-readable on Linux,
/// so it binds to the MACHINE, not the user — still enough to make a copied data dir useless
/// elsewhere. None when neither file exists.
pub fn machine_id() -> Option<String> {
    for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(text) = std::fs::read_to_string(p) {
            let id = text.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn read_proc_status_pages(pid: u32) -> Option<u64> {
    // VmRSS line: "VmRSS:	   1234 kB" — resident bytes = kB * 1024.
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn proc_statm_rss(pid: u32) -> Option<u64> {
    // statm's second field is resident pages; page size from sysconf.
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(rss_pages * page_size())
}

fn page_size() -> u64 {
    // Linux page size is 4096 on every mainstream distro; statm's unit is pages regardless.
    4096
}

/// This process's own resident set, in bytes.
pub fn self_working_set() -> u64 {
    proc_statm_rss(std::process::id()).unwrap_or(0)
}

/// Private commit has no /proc counter; VmRSS stands in (see mem.rs for the field mapping).
pub fn self_private_bytes() -> u64 {
    self_working_set()
}

/// Sum of resident sets across the subtrees rooted at `roots`. `None` when no measurement could
/// be taken at all, so the caller reports "pending" rather than a confident zero.
pub fn process_tree_working_set(roots: &[u32]) -> Option<(u64, usize)> {
    let parents = parent_map()?;
    // The gateway is excluded: the panel reports its own footprint separately, so counting it in
    // the subtree total would show it twice.
    let tree = crate::platform::walk_descendants(&parents, roots, Some(std::process::id()));

    let mut total = 0u64;
    let mut count = 0usize;
    for pid in &tree {
        if let Some(ws) = read_proc_status_pages(*pid).or_else(|| proc_statm_rss(*pid)) {
            total += ws;
            count += 1;
        }
    }
    // Roots sampled but ALL gone by walk time would sum to a confident 0 MB; report "no
    // measurement" instead, the same contract the Windows walker keeps.
    if count == 0 {
        None
    } else {
        Some((total, count))
    }
}

/// pid -> parent for every process /proc will admit to. `None` when /proc cannot be read at all
/// (macOS, a container without it mounted), which callers must treat as "the tree is unknown".
fn parent_map() -> Option<HashMap<u32, u32>> {
    let dir = std::fs::read_dir("/proc").ok()?;
    let mut parents: HashMap<u32, u32> = HashMap::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some(ppid) = ppid_from_stat(&stat) {
                parents.insert(pid, ppid);
            }
        }
    }
    // /proc exists but held no processes: not a real snapshot, so say so rather than hand back an
    // empty map, which reads as "this pid has no children".
    if parents.is_empty() {
        return None;
    }
    Some(parents)
}

/// The ppid out of a /proc/<pid>/stat line.
///
/// Field 2 is `comm`, wrapped in parens, and it can contain BOTH spaces and parens — a process
/// named `foo) 1 2 3 (bar` is legal, and is exactly how this parse gets spoofed. Splitting on the
/// LAST paren is what the kernel's own consumers do, so the fields after it are the trustworthy
/// ones.
fn ppid_from_stat(stat: &str) -> Option<u32> {
    let after = stat.rsplit_once(')')?.1;
    // After the comm: state, then ppid.
    after.split_whitespace().nth(1)?.parse().ok()
}

/// `own_pid` and every descendant. `None` = the snapshot failed, so the tree is UNKNOWN and the
/// caller must fail closed — see the Windows twin for why under-reporting is the dangerous
/// direction here.
pub fn descendant_pids(own_pid: u32) -> Option<Vec<u32>> {
    let parents = parent_map()?;
    Some(
        crate::platform::walk_descendants(&parents, &[own_pid], None)
            .into_iter()
            .collect(),
    )
}

/// A pid that can be signalled without hitting something else by accident.
///
/// `kill` reads non-positive pids as process GROUPS: 0 is "every process in my group" and -1 is
/// "every process I am allowed to signal". A pid that does not fit a positive `pid_t` must
/// therefore never reach it, or a reap takes down the user's own session.
fn signalable(pid: u32) -> Option<libc::pid_t> {
    if pid == 0 || pid > libc::pid_t::MAX as u32 {
        return None;
    }
    Some(pid as libc::pid_t)
}

/// Whether a pid names a live process — the pid-file reader's existence probe.
/// The process that spawned THIS one: parent pid + image name. Linux reads both from
/// /proc (getppid + the parent's comm). On macOS there is no /proc and the seam
/// degrades honestly to None - "parent unknown" in the boot log, never a guess.
pub fn parent_process() -> Option<ParentProcess> {
    let pid = unsafe { libc::getppid() } as u32;
    if pid == 0 {
        return None; // re-parented to init (launcher died): no name to attach
    }
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().trim_end_matches(char::from(0)).to_string());
    Some(ParentProcess {
        pid,
        name: name.unwrap_or_default(),
    })
}

pub fn pid_alive(pid: u32) -> bool {
    let Some(pid) = signalable(pid) else {
        return false;
    };
    // SAFETY: kill(2) with signal 0 delivers nothing; it runs only the existence and permission
    // checks. The pid is known positive, so this cannot address a process group.
    let res = unsafe { libc::kill(pid, 0) };
    if res == 0 {
        return true;
    }
    // EPERM means it exists and belongs to somebody else — still alive, and still a pid this
    // gateway must neither reap nor treat as free.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Force-kill a process AND its descendants. Killing a parent does not take its children here any
/// more than on Windows: an `npx`-launched MCP is sh -> node -> the real server, and SIGKILL to
/// the top leaves the rest running under init.
pub fn tree_kill(pid: u32) {
    // An unknown tree still kills the root, which was named explicitly; only the un-walked
    // descendants leak, exactly as on Windows.
    let victims = descendant_pids(pid).unwrap_or_else(|| vec![pid]);
    let self_pid = std::process::id();
    for victim in victims {
        if victim == self_pid {
            continue; // a corrupted snapshot edge must never turn this into suicide
        }
        let Some(victim) = signalable(victim) else {
            continue;
        };
        // SAFETY: the pid is known positive, so SIGKILL goes to one process, not to a group.
        unsafe {
            libc::kill(victim, libc::SIGKILL);
        }
    }
}

/// Kill every process in a private process group the supervisor created at spawn. The
/// gateway's children start with their own process group, so one negative-pid kill takes the
/// whole tree — including descendants a snapshot walk could have missed — with no enumeration
/// race. The group dies with its last member, so a stale signal simply returns ESRCH.
pub fn kill_process_group(pgid: u32) {
    // SAFETY: kill(2) on a process group this gateway created at spawn; the caller treats
    // the return value as advisory and reaps the root through wait(), which is the source of
    // truth for its death.
    unsafe {
        libc::kill(-(pgid as i32), libc::SIGKILL);
    }
}

/// DPAPI exists only on Windows; the key source list on other platforms omits it.
pub fn dpapi_unprotect(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is a Windows-only key source".into())
}

pub fn dpapi_protect(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is a Windows-only key source".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_ppid_past_a_comm_that_contains_spaces_and_parens() {
        assert_eq!(ppid_from_stat("42 (node) S 7 42 42 0 -1 4194304"), Some(7));
        // The spoof: a process name carrying its own parens and digits.
        assert_eq!(
            ppid_from_stat("42 (foo) 1 2 3 (bar) S 7 42 42"),
            Some(7),
            "the LAST paren ends comm"
        );
        assert_eq!(ppid_from_stat("1 (systemd) S 0 1 1 0"), Some(0));
        assert_eq!(ppid_from_stat("garbage with no parens"), None);
        assert_eq!(ppid_from_stat("42 (node) S"), None); // truncated read
    }

    #[test]
    fn parent_process_reports_a_real_pid_for_the_test_runner() {
        // getppid always knows the launcher; the name is /proc-only, so macOS is allowed
        // to answer empty - the boot log says "name unknown", not a guess.
        let parent = parent_process().expect("getppid cannot fail");
        assert_ne!(parent.pid, 0);
        assert_ne!(parent.pid, std::process::id());
        #[cfg(target_os = "linux")]
        assert!(!parent.name.is_empty());
    }

    #[test]
    fn a_non_positive_pid_is_never_signalable() {
        // kill(0, ...) is "my whole process group" and kill(-1, ...) is "everything I may touch";
        // either one turns a reap into taking down the user's session.
        assert_eq!(signalable(0), None);
        assert_eq!(signalable(u32::MAX), None);
        assert_eq!(signalable(libc::pid_t::MAX as u32 + 1), None);
        assert_eq!(signalable(1), Some(1));
        assert_eq!(signalable(4242), Some(4242));
    }

    #[test]
    fn pid_zero_is_never_alive() {
        // Guarding the probe as well as the kill: a pid file that somehow holds 0 must read as
        // "not running", not as "the whole process group answered".
        assert!(!pid_alive(0));
        assert!(!pid_alive(u32::MAX));
    }

    #[test]
    fn this_process_is_alive_and_a_ghost_is_not() {
        assert!(pid_alive(std::process::id()));
        // Above the pid_max of any mainstream configuration, so it names nothing.
        assert!(!pid_alive(2_147_483_646));
    }

    #[test]
    fn the_walk_finds_this_process_in_its_own_tree() {
        // Skipped where there is no /proc (macOS), which is itself the documented contract.
        if let Some(tree) = descendant_pids(std::process::id()) {
            assert!(tree.contains(&std::process::id()), "{tree:?}");
        }
    }

    #[test]
    fn dpapi_is_refused_by_name_rather_than_silently_failing() {
        // key.rs walks the candidate sources in order; this one has to say why it is not usable.
        assert!(dpapi_protect(b"x").unwrap_err().contains("Windows-only"));
        assert!(dpapi_unprotect(b"x").unwrap_err().contains("Windows-only"));
    }
}
