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

//! What `swiss start` records about the daemon it detached, so that a later `swiss stop` /
//! `swiss status` — run from any directory, possibly after a reboot — can find it and be sure it
//! is still ours — port of `pidfile.ts`.
//!
//! `entry` and `node` are not decoration: a detached daemon outlives the CLI that started it, so
//! by the time anyone reads this file the PID may have been recycled by the OS into an unrelated
//! process. Killing it blind is how a supervisor takes down someone else's work, so `stop`
//! confirms identity (see daemon.rs) before it signals anything.

use serde_json::Value;

use swiss_core::atomic_json::write_json_atomic;
use swiss_core::paths::{data_dir, data_path};
use swiss_core::platform::pid_alive;

#[derive(Debug, Clone, PartialEq)]
pub struct PidRecord {
    pub pid: u32,
    /// The port it was told to listen on — how `stop`/`status` reach its /health endpoint.
    pub port: u16,
    /// Absolute path of the executable that runs it.
    pub entry: String,
    /// The binary that runs it (the gateway's own exe; the field name is the file format's).
    pub node: String,
    pub started_at: String,
}

/// `gateway-<port>.pid`, so two instances on different ports never collide.
pub fn pid_file_path(port: u16) -> std::path::PathBuf {
    data_path(&[&format!("gateway-{port}.pid")])
}

pub fn log_file_path(port: u16) -> std::path::PathBuf {
    data_path(&[&format!("gateway-{port}.log")])
}

fn pos_int(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn non_empty(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Validate a parsed pid file. Returns a NEW record holding only the known fields — unknown keys
/// are dropped rather than rejected, so a file written by another version stays readable.
///
/// A partial record is refused outright instead of being filled with defaults: every field here
/// is used to decide whether to kill a process, and a guessed default is exactly the wrong input
/// to that decision.
pub fn parse_pid_record(raw: &Value) -> Option<PidRecord> {
    let obj = raw.as_object()?;
    let pid = pos_int(obj.get("pid"))?;
    let port = pos_int(obj.get("port"))? as u16;
    let entry = non_empty(obj.get("entry"))?.to_string();
    let node = non_empty(obj.get("node"))?.to_string();
    let started_at = non_empty(obj.get("startedAt"))?.to_string();
    Some(PidRecord {
        pid,
        port,
        entry,
        node,
        started_at,
    })
}

/// The record for a port, or None when there is none to be had. Missing, empty and torn files
/// are all "no daemon": this file's whole purpose is to be read after an unclean kill, so a
/// parse failure has to report "not running" rather than crash out of `swiss status`.
pub fn read_pid_file(port: u16) -> Option<PidRecord> {
    let text = std::fs::read_to_string(pid_file_path(port)).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    parse_pid_record(&parsed)
}

/// Written atomically: being killed mid-write must not leave a torn file that hides a live daemon.
pub fn write_pid_file(rec: &PidRecord) {
    let _ = write_json_atomic(
        &pid_file_path(rec.port),
        &serde_json::json!({
            "pid": rec.pid,
            "port": rec.port,
            "entry": rec.entry,
            "node": rec.node,
            "startedAt": rec.started_at,
        }),
    );
}

/// Idempotent — `stop` calls it on paths that may already be gone.
pub fn remove_pid_file(port: u16) {
    let _ = std::fs::remove_file(pid_file_path(port));
}

/// Every port with a pid file, ascending — `gateway-<port>.pid` matched by hand (no regex).
pub fn list_daemon_ports() -> Vec<u16> {
    let Ok(entries) = std::fs::read_dir(data_dir()) else {
        return Vec::new(); // no data dir yet — nothing has ever run
    };
    let mut ports: Vec<u16> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let stem = name.strip_prefix("gateway-")?.strip_suffix(".pid")?;
            if stem.is_empty() || stem.len() > 5 || !stem.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            stem.parse::<u16>().ok()
        })
        .collect();
    ports.sort_unstable();
    ports
}

/// Whether a pid names a live process.
pub fn is_pid_alive(pid: u32) -> bool {
    pid_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The pid files of every port share one data dir, and `list_daemon_ports` reads all of them,
    /// so the tests that write one take this first and leave the dir as they found it. The lock is
    /// the data dir's, not this module's: the daemon tests plant pid files in the same directory.
    fn pid_dir() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = swiss_core::paths::DATA_DIR_LOCK.blocking_lock();
        swiss_core::paths::test_home();
        sweep();
        guard
    }

    /// Remove every pid file in the scratch data dir.
    fn sweep() {
        for port in list_daemon_ports() {
            remove_pid_file(port);
        }
    }

    fn rec(port: u16) -> PidRecord {
        PidRecord {
            pid: 4242,
            port,
            entry: "D:\\dev\\mcp-gateway\\swiss.exe".into(),
            node: "D:\\dev\\mcp-gateway\\swiss.exe".into(),
            started_at: "2026-08-14T15:00:00.000Z".into(),
        }
    }

    /// The record as it is written to disk — the file format's own key names.
    fn raw(r: &PidRecord) -> Value {
        json!({
            "pid": r.pid, "port": r.port, "entry": r.entry,
            "node": r.node, "startedAt": r.started_at,
        })
    }

    #[test]
    fn lives_in_the_data_dir_named_by_port() {
        // Named by port, and kept in the data dir rather than beside the config: `swiss status` has
        // to find a running daemon from any cwd, and the port is what tells two instances apart.
        let home = swiss_core::paths::test_home();
        // Pin it: paths reads MCP_GATEWAY_HOME fresh on every call, and a parallel test in
        // this binary may install its own scratch home between our two lines otherwise —
        // the equality below only holds while the variable points at OUR home.
        unsafe { std::env::set_var("MCP_GATEWAY_HOME", &home) };
        assert_eq!(pid_file_path(19999), home.join("gateway-19999.pid"));
        assert_eq!(log_file_path(19999), home.join("gateway-19999.log"));
    }

    #[test]
    fn round_trips_a_record() {
        let _lock = pid_dir();
        let r = rec(19991);
        write_pid_file(&r);
        assert_eq!(read_pid_file(19991), Some(r));
        sweep();
    }

    #[test]
    fn reports_nothing_when_no_daemon_was_ever_started() {
        let _lock = pid_dir();
        assert_eq!(read_pid_file(19992), None);
        assert_eq!(list_daemon_ports(), Vec::<u16>::new());
    }

    #[test]
    fn treats_a_torn_or_non_json_file_as_absent_instead_of_failing() {
        // The file exists precisely to be read after an unclean kill, so a torn one must not blow
        // up — that would make `swiss status` fail instead of reporting "not running".
        let _lock = pid_dir();
        std::fs::write(pid_file_path(19993), r#"{"pid":4242,"po"#).expect("write a torn file");
        assert_eq!(read_pid_file(19993), None);
        std::fs::write(pid_file_path(19993), "").expect("write an empty file");
        assert_eq!(read_pid_file(19993), None);
        sweep();
    }

    #[test]
    fn rejects_anything_that_is_not_a_whole_record() {
        // Every field is load-bearing: `port` is how stop() reaches /health, and `entry` is the
        // witness that this pid is still OUR gateway rather than a number the OS recycled.
        for bad in [Value::Null, json!([]), json!("x"), json!(42), json!({})] {
            assert!(parse_pid_record(&bad).is_none(), "{bad}");
        }
        let good = rec(19999);
        assert_eq!(parse_pid_record(&raw(&good)), Some(good));
    }

    #[test]
    fn records_parse_strictly_and_drop_unknown_keys() {
        let rec = parse_pid_record(&json!({
            "pid": 4242, "port": 19998, "entry": "C:\\swiss.exe", "node": "C:\\swiss.exe",
            "startedAt": "2026-09-07T10:00:00.000Z", "futureField": true,
        }))
        .expect("valid record parses");
        assert_eq!(rec.pid, 4242);
        assert_eq!(rec.port, 19998);

        // Every refusal in the Node build: non-integer, zero, missing and empty fields.
        assert!(parse_pid_record(
            &json!({ "pid": 0, "port": 19998, "entry": "a", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": 1.5, "port": 19998, "entry": "a", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": -1, "port": 19998, "entry": "a", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": "4242", "port": 19998, "entry": "a", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": 5, "port": "19998", "entry": "a", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": 5, "port": 19998, "entry": "", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": 5, "port": 19998, "entry": "a", "node": "", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(
            &json!({ "pid": 5, "port": 19998, "entry": "a", "node": "b", "startedAt": "" })
        )
        .is_none());
        assert!(parse_pid_record(&json!({ "pid": 5, "port": 19998 })).is_none());
        assert!(parse_pid_record(&json!("nope")).is_none());
    }

    #[test]
    fn removes_the_file_and_removing_one_already_gone_is_not_an_error() {
        let _lock = pid_dir();
        write_pid_file(&rec(19994));
        assert!(pid_file_path(19994).exists());
        remove_pid_file(19994);
        assert!(!pid_file_path(19994).exists());
        remove_pid_file(19994); // idempotent: `stop` calls it on paths that may already be gone
    }

    #[test]
    fn lists_the_ports_with_a_pid_file_sorted_ignoring_everything_else_in_the_dir() {
        let _lock = pid_dir();
        let home = swiss_core::paths::test_home();
        write_pid_file(&rec(19995));
        write_pid_file(&rec(8080));
        std::fs::write(home.join("managed.json"), "{}").expect("write");
        std::fs::write(home.join("gateway-notaport.pid"), "{}").expect("write");
        std::fs::write(home.join("gateway-19995.log"), "").expect("write");
        assert_eq!(list_daemon_ports(), vec![8080, 19995]);
        sweep();
        for junk in ["managed.json", "gateway-notaport.pid", "gateway-19995.log"] {
            let _ = std::fs::remove_file(home.join(junk));
        }
    }

    #[test]
    fn knows_whether_a_pid_is_alive() {
        assert!(is_pid_alive(std::process::id()));
        // Odd and enormous — it cannot be a live pid on any platform this ships to.
        assert!(!is_pid_alive(2_147_483_647));
        // Never signal pid 0: that is the whole process group.
        assert!(!is_pid_alive(0));
    }
}
