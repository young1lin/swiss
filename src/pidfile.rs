//! What `lmg start` records about the daemon it detached, so that a later `lmg stop` /
//! `lmg status` — run from any directory, possibly after a reboot — can find it and be sure it
//! is still ours — port of `pidfile.ts`.
//!
//! `entry` and `node` are not decoration: a detached daemon outlives the CLI that started it, so
//! by the time anyone reads this file the PID may have been recycled by the OS into an unrelated
//! process. Killing it blind is how a supervisor takes down someone else's work, so `stop`
//! confirms identity (see daemon.rs) before it signals anything.

use serde_json::Value;

use crate::atomic_json::write_json_atomic;
use crate::paths::{data_dir, data_path};
use crate::platform::pid_alive;

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
/// parse failure has to report "not running" rather than crash out of `lmg status`.
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

    #[test]
    fn records_parse_strictly_and_drop_unknown_keys() {
        let rec = parse_pid_record(&json!({
            "pid": 4242, "port": 19998, "entry": "C:\\lmg.exe", "node": "C:\\lmg.exe",
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
            &json!({ "pid": 5, "port": 19998, "entry": "", "node": "b", "startedAt": "c" })
        )
        .is_none());
        assert!(parse_pid_record(&json!({ "pid": 5, "port": 19998 })).is_none());
        assert!(parse_pid_record(&json!("nope")).is_none());
    }
}
