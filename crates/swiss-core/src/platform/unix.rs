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

/// A memoizing creation-time lookup for the walks below: /proc/<pid>/stat read once per pid the
/// walk REACHES, never once per edge and never for the whole snapshot (SPEC §process.tree).
fn created_at_probe() -> impl FnMut(u32) -> Option<u64> {
    let mut cache: HashMap<u32, Option<u64>> = HashMap::new();
    move |pid| *cache.entry(pid).or_insert_with(|| process_created_at(pid))
}

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

/// One process's resident set, in bytes - the children rows' memory column.
/// `None` when the pid cannot be read (gone, or another user's): a row
/// without a reading shows none, never 0.
pub fn process_working_set(pid: u32) -> Option<u64> {
    proc_statm_rss(pid)
}

/// Sum of resident sets across the subtrees rooted at `roots`. `None` when no measurement could
/// be taken at all, so the caller reports "pending" rather than a confident zero.
pub fn process_tree_working_set(roots: &[u32]) -> Option<(u64, usize)> {
    let parents = parent_map()?;
    // The gateway is excluded: the panel reports its own footprint separately, so counting it in
    // the subtree total would show it twice. Measure mode: an edge whose creation times cannot
    // be read still counts - a wrong number is repaired next poll (SPEC §process.tree).
    let tree = crate::platform::walk_descendants(
        &parents,
        roots,
        Some(std::process::id()),
        crate::platform::WalkMode::Measure,
        &mut created_at_probe(),
    );

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
/// direction here. The walk is a KILL-side walk (SPEC §process.tree): each parent edge is
/// checked against creation times, and an edge whose times cannot be read does not count.
pub fn descendant_pids(own_pid: u32) -> Option<Vec<u32>> {
    let parents = parent_map()?;
    Some(
        crate::platform::walk_descendants(
            &parents,
            &[own_pid],
            None,
            crate::platform::WalkMode::Kill,
            &mut created_at_probe(),
        )
        .into_iter()
        .collect(),
    )
}

/// The image name of an arbitrary pid: the basename of /proc/<pid>/exe, falling back to comm —
/// the Linux probe's identity rule (SPEC §process.detached). comm is truncated to 15 bytes by
/// the kernel, so a caller matching a longer image against the fallback matches on its first
/// 15 bytes. None without /proc (macOS) or for a gone pid.
pub fn process_name(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            // "node (deleted)" keeps basename "node (deleted)"; the plain name is the common case.
            let name = exe
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.is_empty() {
                return Some(name);
            }
        }
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|s| s.trim().trim_end_matches(char::from(0)).to_string())
            .filter(|s| !s.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// When a process was created, in Unix milliseconds: /proc/<pid>/stat's starttime (field 22,
/// clock ticks since boot) plus btime from /proc/stat (SPEC §process.detached). This is half of
/// a pid's identity against reuse. None without /proc or for a gone pid.
pub fn process_created_at(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let ticks = super::procfs::stat_start_time_ticks(&stat)?;
        let btime =
            super::procfs::boot_time_seconds(&std::fs::read_to_string("/proc/stat").ok()?)?;
        let hz = clock_ticks();
        // Ticks are whole, so the sub-second part of the creation time is only tick-resolution;
        // identity needs consistency, not precision.
        Some((btime + ticks / hz) * 1000 + (ticks % hz) * 1000 / hz)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// The clock-tick frequency /proc/<pid>/stat's starttime is counted in. sysconf answers on every
/// Linux; the fallback of 100 is what every mainstream distro ships, and only a kernel built
/// with a different CONFIG_HZ would ever need it.
#[cfg(target_os = "linux")]
fn clock_ticks() -> u64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        100
    } else {
        hz as u64
    }
}

/// Every LISTENING TCP socket, as (local port, owning pid): the LISTEN rows (state 0A) of
/// /proc/net/tcp and /proc/net/tcp6, joined to their owners by socket inode through one pass
/// over /proc/*/fd (SPEC §process.detached). Without privileges the fd dir of another user's
/// process cannot be read, so such a port reads as owned by pid 0 - "owned by another user" to
/// the caller. Err when /proc/net or /proc itself cannot be read (macOS: Unsupported).
pub fn tcp_listeners() -> std::io::Result<Vec<(u16, u32)>> {
    #[cfg(target_os = "linux")]
    {
        // The join pass first: inode -> pid. Errors reading one pid's fd dir (EACCES for other
        // users) skip that pid alone; a socket it owns stays unclaimed and reads as pid 0 below.
        let mut owners: HashMap<u64, u32> = HashMap::new();
        for entry in std::fs::read_dir("/proc")? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    if let Some(inode) = super::procfs::socket_inode(&target.to_string_lossy()) {
                        owners.entry(inode).or_insert(pid);
                    }
                }
            }
        }
        let mut rows = Vec::new();
        for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let text = std::fs::read_to_string(file)?;
            for (port, inode) in super::procfs::tcp_listen_rows(&text) {
                // An inode with no readable owner is another user's socket: pid 0, so the
                // caller words it "owned by another user" rather than "free".
                rows.push((port, owners.get(&inode).copied().unwrap_or(0)));
            }
        }
        Ok(rows)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the listener table is not readable on this platform",
        ))
    }
}

/// The spawn posture of a program that must OUTLIVE the gateway (SPEC §process.detached): the
/// child calls setsid() between fork and exec - a new SESSION, not just a process group, so a
/// terminal hangup never reaches it. The supervisor's children take a private process group
/// instead (kill(-pgid) teardown); a detached app must not be in any group the gateway can
/// signal wholesale.
pub fn detach_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs in the child between fork and exec. The closure only calls the
    // setsid libc wrapper and formats nothing - async-signal-safe - and runs before any of the
    // child's own threads exist. A failure (EPERM when the child somehow leads its group)
    // aborts the spawn with that error rather than exec'ing attached.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// Unix children inherit only the fds the spawn hands them (everything else is CLOEXEC), so
/// there is nothing to clear here - the Windows twin exists because bInheritHandles hands a
/// child EVERY inheritable handle. Present so the serve path calls one name on both platforms
/// (SPEC §host.daemon).
pub fn keep_std_handles_from_children() {}

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

/// The named variables as THIS process's ancestors hold them, nearest first - see the Windows
/// twin. Linux reads each ancestor's /proc/<pid>/environ (what it was started with); macOS
/// has no /proc and answers nothing, so its callers fall back to their own environment.
pub fn ancestor_env(names: &[&str]) -> Vec<(String, String)> {
    #[cfg(target_os = "linux")]
    {
        let mut found = Vec::new();
        let mut child = std::process::id();
        for _ in 0..12 {
            let Some(parent) = std::fs::read_to_string(format!("/proc/{child}/stat"))
                .ok()
                .and_then(|stat| ppid_from_stat(&stat))
            else {
                break;
            };
            if parent <= 1 {
                break;
            }
            if let Ok(raw) = std::fs::read(format!("/proc/{parent}/environ")) {
                for entry in raw.split(|byte| *byte == 0) {
                    let text = String::from_utf8_lossy(entry);
                    if let Some((name, value)) = text.split_once('=') {
                        if names.contains(&name) {
                            found.push((name.to_string(), value.to_string()));
                        }
                    }
                }
            }
            child = parent;
        }
        found
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = names;
        Vec::new()
    }
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

/// Force-kill a process AND its descendants, returning the pids that were STILL ALIVE after
/// it ran (SPEC §process.tree). Killing a parent does not take its children here any more than
/// on Windows: an `npx`-launched MCP is sh -> node -> the real server, and SIGKILL to the top
/// leaves the rest running under init.
///
/// A survivor is a victim SIGKILL could not be delivered to - EPERM, a process belonging to
/// another user. A successfully-signalled pid is not reported: SIGKILL cannot be blocked or
/// ignored, and a zombie (dead, awaiting its parent's wait) already answers kill(2) and is
/// nobody's survivor.
pub fn tree_kill(pid: u32) -> Vec<u32> {
    // An unknown tree still kills the root, which was named explicitly; only the un-walked
    // descendants leak, exactly as on Windows.
    let victims = descendant_pids(pid).unwrap_or_else(|| vec![pid]);
    let self_pid = std::process::id();
    let mut survivors = Vec::new();
    for victim in victims {
        if victim == self_pid {
            continue; // a corrupted snapshot edge must never turn this into suicide
        }
        let Some(victim) = signalable(victim) else {
            continue;
        };
        // SAFETY: the pid is known positive, so SIGKILL goes to one process, not to a group.
        let refused = unsafe { libc::kill(victim, libc::SIGKILL) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if refused {
            survivors.push(victim as u32);
        }
    }
    survivors
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
