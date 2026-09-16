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

//! A persisted ledger of the child-process PIDs this gateway instance has spawned for proc MCPs
//! — port of `proc-pids.ts`.
//!
//! Why: a proc MCP child (cmd -> npx -> server) is only torn down by `ProcAdapter::close`,
//! which runs on a GRACEFUL shutdown. When the gateway is hard-killed (the watchdog's force
//! kill, a crash), no close runs and the subtree can survive as orphans — and a fresh instance
//! has no way to know those PIDs by command alone (an arbitrary proc command isn't on any fixed
//! match list). So each successful build records its wrapper PID here; on the next boot, before
//! any proc MCP starts, we tree-kill every recorded PID that is still alive and not a
//! descendant of THIS instance.
//!
//! The descendant guard is the PID-reuse safety: a stale ledger entry whose PID the OS has
//! since reused for one of our own freshly-spawned children is left alone.
//!
//! The ledger is written atomically (temp+rename): being killed mid-write (exactly the scenario
//! this exists for) must not leave it torn, or the next boot would reap the wrong set.
//!
//! The ledger file is PORT-SCOPED: pidfiles promise two instances on different ports coexist,
//! and a shared ledger let instance B's reap kill instance A's LIVE proc children — "alive and
//! not my descendant" cannot tell an orphan from a neighbour's child (see `index.ts`).
//!
//! The Node build's reap was async only because its process-tree walk spawned a PowerShell; the
//! platform layer here walks with direct syscalls, so the whole ledger is synchronous.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

use swiss_core::atomic_json::write_json_atomic;
use swiss_core::log;
use swiss_core::platform::{descendant_pids, pid_alive, tree_kill};

/// Where the ledger lives (a gitignored runtime file). Set once at boot (see `main.rs`).
static FILE: OnceLock<PathBuf> = OnceLock::new();

/// `.proc-pids-<port>.json` under the data dir — the exact file-name scheme the Node build
/// uses, load-bearing for coexisting instances (a shared file lets one instance reap another's
/// live children). The port comes from the loaded config; the default matches `paths`.
pub fn proc_pid_file(port: u16) -> PathBuf {
    swiss_core::paths::data_path(&[&format!(".proc-pids-{port}.json")])
}

/// Point the ledger at its file. Idempotent-by-ignoring: boot sets it once, before any proc
/// MCP starts (Node's `setProcPidFile`).
pub fn set_proc_pid_file(path: PathBuf) {
    let _ = FILE.set(path);
}

/// A pid entry the ledger accepts: a finite positive number that fits the OS pid type. Node
/// kept any finite `number > 0`; the u32 cast is what the rest of this build works in.
fn keep(v: &Value) -> Option<u32> {
    let n = v.as_f64()?;
    if !n.is_finite() || n <= 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
        return None;
    }
    Some(n as u32)
}

/// Read the ledger at `file`. Missing, torn (a half-written pre-atomic file), or wrong-shaped
/// entries all read as empty; `note` rewrites the file whole.
fn read_ledger(file: &Path) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    parsed
        .get("pids")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(keep).collect())
        .unwrap_or_default()
}

fn write_ledger(file: &Path, pids: &[u32]) {
    // `{"pids": [...]}`, two-space pretty — byte-identical to the Node build's writeJsonAtomic.
    if let Err(err) = write_json_atomic(file, &json!({ "pids": pids })) {
        log::warn("proc-pid ledger save failed", Some(json!({ "err": err })));
    }
}

/// Record a proc child PID once build succeeds, so a future boot can reap it if this one dies
/// before close runs. Idempotent. No-op until `set_proc_pid_file` has pointed at a file.
pub fn note_proc_pid(pid: u32) {
    let Some(file) = FILE.get() else { return };
    if pid == 0 {
        return;
    }
    note_in(file, pid);
}

fn note_in(file: &Path, pid: u32) {
    let mut pids = read_ledger(file);
    if pids.contains(&pid) {
        return;
    }
    pids.push(pid);
    write_ledger(file, &pids);
}

/// Drop a PID once its child closed cleanly, so it is no longer a candidate for orphan reaping.
pub fn drop_proc_pid(pid: u32) {
    let Some(file) = FILE.get() else { return };
    if pid == 0 {
        return;
    }
    drop_in(file, pid);
}

fn drop_in(file: &Path, pid: u32) {
    let pids = read_ledger(file);
    if !pids.contains(&pid) {
        return;
    }
    write_ledger(
        file,
        &pids
            .iter()
            .copied()
            .filter(|p| *p != pid)
            .collect::<Vec<_>>(),
    );
}

/// Reap proc children a PREVIOUS instance orphaned. Reads the ledger, clears it (this instance
/// rebuilds it as proc MCPs come up), then tree-kills every recorded PID that is still alive
/// AND not a descendant of THIS instance. Call once at boot, before any proc MCP starts.
///
/// Returns the PIDs it killed (empty when the ledger was empty — in which case the descendant
/// walk is skipped entirely, so the common no-proc boot stays cheap).
pub fn reap_proc_pids(own_pid: u32) -> Vec<u32> {
    let Some(file) = FILE.get() else {
        return Vec::new();
    };
    reap_in(file, own_pid)
}

fn reap_in(file: &Path, own_pid: u32) -> Vec<u32> {
    let pids = read_ledger(file);
    // Clear first: a clean start repopulates it; never carry stale entries forward.
    write_ledger(file, &[]);
    if pids.is_empty() {
        return Vec::new(); // nothing recorded -> nothing to reap, and no descendant walk
    }
    // Fail CLOSED on an unknown tree: without the descendant set the PID-reuse guard below is a
    // guess, and guessing wrong kills a live child of THIS instance. The orphans stay for the
    // next boot — the lesser evil by far.
    let Some(descendants) = descendant_pids(own_pid) else {
        log::error(
            "process snapshot failed — skipping the orphan-proc reap this boot",
            None,
        );
        return Vec::new();
    };
    let mine: std::collections::HashSet<u32> = descendants.into_iter().collect();
    let mut killed = Vec::new();
    for pid in pids {
        if mine.contains(&pid) {
            continue; // belongs to THIS instance now — never touch it (PID-reuse guard)
        }
        if !pid_alive(pid) {
            continue; // already gone — closed cleanly last run, or the OS reaped it
        }
        log::warn(
            "reaping orphaned proc child from a previous gateway instance",
            Some(json!({ "pid": pid })),
        );
        tree_kill(pid);
        killed.push(pid);
    }
    if !killed.is_empty() {
        log::log(
            "info",
            "proc-pid reap complete",
            Some(json!({ "killed": killed.len() })),
        );
    }
    killed
}

#[cfg(test)]
mod tests {
    // Ported from test/proc-pids.test.ts in spirit: the ledger's JSON shape, its tolerance of
    // torn files, and the reap's skip rules. The one behaviour NOT tested is an actual
    // tree-kill of a live foreign PID — every pid recorded here is either this process (the
    // descendant guard skips it) or a value nothing owns.
    use super::*;

    fn temp_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swiss-procpids-{tag}-{}",
            swiss_core::util::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(".proc-pids-19999.json")
    }

    #[test]
    fn ledger_shape_is_pretty_pids_array() {
        let file = temp_file("shape");
        note_in(&file, 4242);
        note_in(&file, 8484);
        let text = std::fs::read_to_string(&file).unwrap();
        // Byte-for-byte what the Node build's writeJsonAtomic(JSON.stringify(x, null, 2)) emits.
        assert_eq!(text, "{\n  \"pids\": [\n    4242,\n    8484\n  ]\n}");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn note_is_idempotent_and_drop_removes() {
        let file = temp_file("note");
        note_in(&file, 100);
        note_in(&file, 100); // second note must not duplicate the entry
        assert_eq!(read_ledger(&file), vec![100]);
        drop_in(&file, 999); // absent pid is a no-op
        assert_eq!(read_ledger(&file), vec![100]);
        drop_in(&file, 100);
        assert_eq!(read_ledger(&file), Vec::<u32>::new());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\n  \"pids\": []\n}"
        );
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn a_missing_or_torn_file_reads_empty() {
        let file = temp_file("torn");
        assert!(read_ledger(&file).is_empty()); // missing
        std::fs::write(&file, "{\"pids\": [12").unwrap(); // torn mid-array
        assert!(read_ledger(&file).is_empty());
        // Non-numeric / non-positive entries are dropped; numbers survive.
        std::fs::write(&file, "{\"pids\": [7, \"x\", 0, -3, 1.5, 99999999999]}").unwrap();
        assert_eq!(read_ledger(&file), vec![7]);
        // A scalar `pids` is not an array — empty, not an error.
        std::fs::write(&file, "{\"pids\": 7}").unwrap();
        assert!(read_ledger(&file).is_empty());
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn reap_clears_first_and_skips_own_pid_and_dead_pids() {
        let file = temp_file("reap");
        note_in(&file, std::process::id()); // a live pid — but OURS: the guard must skip it
        note_in(&file, 999_999_905); // alive-check fails: nothing owns this pid
        let killed = reap_in(&file, std::process::id());
        assert!(
            killed.is_empty(),
            "neither own process nor a dead pid is killed"
        );
        // The ledger was cleared BEFORE the walk: a fresh boot starts from [].
        assert_eq!(read_ledger(&file), Vec::<u32>::new());
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn reap_on_an_empty_ledger_is_a_no_op() {
        let file = temp_file("empty");
        assert!(reap_in(&file, std::process::id()).is_empty());
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn the_file_name_is_port_scoped() {
        let path = proc_pid_file(19999);
        assert_eq!(
            path.file_name().and_then(std::ffi::OsStr::to_str),
            Some(".proc-pids-19999.json")
        );
        assert!(path.starts_with(swiss_core::paths::data_dir()));
    }
}
