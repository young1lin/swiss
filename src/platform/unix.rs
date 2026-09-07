//! The non-Windows platform seam: machine id from /etc/machine-id, working sets from /proc.

use std::collections::{HashMap, HashSet, VecDeque};

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

/// Sum of resident sets across the subtrees rooted at `roots` (BFS by ppid from /proc).
pub fn process_tree_working_set(roots: &[u32]) -> Option<(u64, usize)> {
    let self_pid = std::process::id();
    let mut parents: HashMap<u32, u32> = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return None;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        // /proc/<pid>/stat: pid (comm) state ppid ... — comm may contain spaces and parens.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some(after) = stat.rsplit(')').next() {
                if let Some(ppid) = after.split_whitespace().nth(1) {
                    if let Ok(ppid) = ppid.parse::<u32>() {
                        parents.insert(pid, ppid);
                    }
                }
            }
        }
    }

    let mut queue: VecDeque<u32> = roots.iter().copied().collect();
    let mut tree: HashSet<u32> = HashSet::new();
    while let Some(pid) = queue.pop_front() {
        if pid == self_pid || !tree.insert(pid) {
            continue;
        }
        for (child, parent) in parents.iter() {
            if *parent == pid {
                queue.push_back(*child);
            }
        }
    }
    let mut total = 0u64;
    let mut count = 0usize;
    for pid in &tree {
        if let Some(ws) = read_proc_status_pages(*pid).or_else(|| proc_statm_rss(*pid)) {
            total += ws;
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some((total, count))
    }
}
