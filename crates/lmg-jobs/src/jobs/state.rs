//! Run state, the second file (docs/11 §4). Definitions are user intent; this file is
//! gateway-maintained FACT about runs. The two must not share a writable file
//! (docs/10 §5): editing a definition can never fabricate or lose a "last ran" fact.
//!
//! `~/.mcp-gateway/jobs-state.json`, sealed and private-permission like every state
//! file (docs/11 §2 rule 1). Only SMALL BOUNDED facts live here - one line per job:
//! when it last started, whether that run exited 0, the coordinator's run id, and a
//! consecutive-failure counter. Output, command text and credential references never
//! enter this file.
//!
//! Two load rules and one write rule:
//! - a missing file is empty state (nothing has run yet), not an error;
//! - a corrupt file is empty state plus a warn, never a boot failure - the run LOG
//!   still holds the history, this file only holds scheduling facts;
//! - a failed WRITE is a warn, not an Err. The run already happened; refusing to
//!   acknowledge a run because its state line could not be written is the worse trade.
//!   This is deliberately the opposite of the config-write rule (a config that cannot
//!   persist must answer with an error, docs/10 §5): config is intent the user asked
//!   to save, state is a fact the gateway records best-effort. The same trade the
//!   best-effort anchor write in JobSystem::claim has always made.
//!
//! A deleted definition's line is dropped at the next write (forget) - the history
//! stays in the run log, which outlives the job that wrote it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use lmg_core::log;
use lmg_core::secure::statefile::{read_secure_json, write_secure_json};
use lmg_core::util::parse_iso_ms;

/// RFC3339 UTC (millisecond precision, Z-suffixed) from a unix millisecond stamp - the same
/// shape log::iso_now produces, so a reader parses lastRunAt with the same util it parses
/// every other timestamp here. chrono rather than the time crate: from_timestamp_millis is
/// right here and chrono is already linked for the local-time cron work.
pub fn iso_of_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// One job's scheduling facts. Fields the current stage does not populate yet
/// (last_occurrence_key, last_run_id) are still round-tripped: the file shape is the
/// contract, and S5 fills them without a format change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobRunState {
    /// The occurrence identity of the last CLAIMED run, e.g. "cron:2026-09-09T03:30"
    /// (docs/11 §6.1). Manual runs do not set it.
    pub last_occurrence_key: Option<String>,
    /// When the last run STARTED (the interval anchor), unix milliseconds.
    pub last_run_at: Option<i64>,
    /// Whether that last run exited 0.
    pub last_ok: Option<bool>,
    /// The shared coordinator's run id for the last run.
    pub last_run_id: Option<u64>,
    /// Failures in a row, reset by any success. Bounded by reality: it counts runs.
    pub consecutive_failures: u32,
}

impl JobRunState {
    /// The wire shape, camelCase like every state file. Absent-not-null throughout.
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        if let Some(key) = &self.last_occurrence_key {
            m.insert("lastOccurrenceKey".into(), json!(key));
        }
        if let Some(ms) = self.last_run_at {
            m.insert("lastRunAt".into(), json!(iso_of_ms(ms)));
        }
        if let Some(ok) = self.last_ok {
            m.insert("lastOk".into(), json!(ok));
        }
        if let Some(id) = self.last_run_id {
            m.insert("lastRunId".into(), json!(id));
        }
        if self.consecutive_failures > 0 {
            m.insert(
                "consecutiveFailures".into(),
                json!(self.consecutive_failures),
            );
        }
        Value::Object(m)
    }

    /// Tolerant by field: this file is gateway-maintained, but a hand edit or a partial
    /// write must not cost the whole table (the managed-store rule). A wrong-typed or
    /// unparsable field reads as absent.
    fn from_json(v: &Value) -> JobRunState {
        let get_iso = |key: &str| -> Option<i64> {
            v.get(key).and_then(Value::as_str).and_then(parse_iso_ms)
        };
        JobRunState {
            last_occurrence_key: v
                .get("lastOccurrenceKey")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            last_run_at: get_iso("lastRunAt"),
            last_ok: v.get("lastOk").and_then(Value::as_bool),
            last_run_id: v.get("lastRunId").and_then(Value::as_u64),
            consecutive_failures: v
                .get("consecutiveFailures")
                .and_then(Value::as_u64)
                .map(|n| n.min(u32::MAX as u64) as u32)
                .unwrap_or(0),
        }
    }
}

/// The loaded-and-owned state table. One instance per JobSystem; no process-global
/// anything (that is the whole point of this stage - two systems in one process must
/// never share run state the way the old runlog globals did).
pub struct JobsState {
    path: PathBuf,
    jobs: Mutex<HashMap<String, JobRunState>>,
}

impl JobsState {
    /// Load, tolerantly: missing or corrupt file reads as empty state (a corrupt one
    /// warns - it deserves to be noticed, not to stop the scheduler).
    pub fn open(path: PathBuf) -> Self {
        let mut jobs = HashMap::new();
        match read_secure_json(&path) {
            Ok(Some(Value::Object(root))) => {
                if let Some(Value::Object(map)) = root.get("jobs") {
                    for (name, body) in map {
                        if crate::jobs::runlog::valid_name(name) {
                            jobs.insert(name.clone(), JobRunState::from_json(body));
                        }
                    }
                }
            }
            // No file yet: empty state, silently - nothing has run, ever.
            Ok(_) => {}
            Err(err) => log::warn(
                "jobs-state.json load failed - starting with empty run state",
                Some(json!({ "err": err, "path": path.display().to_string() })),
            ),
        }
        JobsState {
            path,
            jobs: Mutex::new(jobs),
        }
    }

    /// One job's current facts (defaults when unknown).
    pub fn get(&self, job: &str) -> JobRunState {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job)
            .cloned()
            .unwrap_or_default()
    }

    /// Stamp the claim of an occurrence: the run is starting NOW, so the interval
    /// anchor moves to its START (a long run must not queue an immediate re-run when it
    /// finishes). Best-effort persist - see the module header for why this write may
    /// warn and still succeed. The anchor moves for the OCCURRENCE, not for a success:
    /// a refused occurrence still counts, or the job would re-fire every tick while
    /// the pool is full.
    pub fn claim(&self, job: &str, at_ms: i64, occurrence_key: Option<&str>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let st = jobs.entry(job.to_string()).or_default();
        st.last_run_at = Some(at_ms);
        if let Some(key) = occurrence_key {
            st.last_occurrence_key = Some(key.to_string());
        }
        self.persist(&jobs);
    }

    /// Record the terminal facts of the last run. `run_id` is the shared
    /// coordinator's id when the run actually submitted; a refused occurrence has
    /// none and does not settle - a run that could not start did not exit, and
    /// lastOk must not claim otherwise.
    pub fn settle(&self, job: &str, ok: bool, run_id: Option<u64>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let st = jobs.entry(job.to_string()).or_default();
        st.last_ok = Some(ok);
        st.last_run_id = run_id;
        st.consecutive_failures = if ok {
            0
        } else {
            st.consecutive_failures.saturating_add(1)
        };
        self.persist(&jobs);
    }

    /// Migration-only (docs/11 §5.1 step 5): seed a v1 row's run facts without
    /// overwriting anything newer. A crash between the config write and the jobs.json
    /// marker re-runs the seed against a state file that may already hold migrated
    /// facts - or facts for a run that happened since - and the migration must converge,
    /// not clobber. Best-effort persist, like every run-path write here.
    pub fn seed(&self, job: &str, last_run_at: Option<i64>, last_ok: Option<bool>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let st = jobs.entry(job.to_string()).or_default();
        if st.last_run_at.is_none() {
            st.last_run_at = last_run_at;
        }
        if st.last_ok.is_none() {
            st.last_ok = last_ok;
        }
        self.persist(&jobs);
    }

    /// Drop a deleted definition's line. The run history stays in the log file, which
    /// outlives the job that wrote it; only the scheduling facts go.
    pub fn forget(&self, job: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if jobs.remove(job).is_some() {
            self.persist(&jobs);
        }
    }

    /// Seal the table to disk. The error is only logged: every caller is on a RUN path
    /// where the run has already happened - the exact opposite trade of
    /// JobStore::save, whose caller MUST hear about a config write that did not
    /// persist (docs/10 §5).
    fn persist(&self, jobs: &HashMap<String, JobRunState>) {
        let body = json!({
            "version": 1,
            "jobs": jobs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect::<Map<String, Value>>(),
        });
        if let Err(err) = write_secure_json(&self.path, &body) {
            log::warn(
                "jobs-state.json persist failed",
                Some(json!({ "err": err, "path": self.path.display().to_string() })),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lmg-jobs-state-{}-{}",
            name,
            lmg_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("jobs-state.json")
    }

    #[test]
    fn state_round_trips_through_the_sealed_file() {
        lmg_core::secure::key::use_test_master_key();
        let path = scratch("roundtrip");
        {
            let st = JobsState::open(path.clone());
            st.claim("nightly", 1_757_400_000_412, Some("cron:2026-09-09T03:30"));
            st.settle("nightly", false, Some(41));
            st.claim("other", 500, None);
            st.settle("other", true, None);
        }
        let st = JobsState::open(path);
        let nightly = st.get("nightly");
        assert_eq!(nightly.last_run_at, Some(1_757_400_000_412));
        assert_eq!(
            nightly.last_occurrence_key.as_deref(),
            Some("cron:2026-09-09T03:30")
        );
        assert_eq!(nightly.last_ok, Some(false));
        assert_eq!(nightly.last_run_id, Some(41));
        assert_eq!(nightly.consecutive_failures, 1);
        let other = st.get("other");
        assert_eq!(other.last_ok, Some(true));
        assert_eq!(other.consecutive_failures, 0, "a success resets the streak");
        assert_eq!(other.last_run_id, None);
        // The streak keeps counting until a success.
        let st2 = JobsState::open(scratch("streak"));
        st2.settle("j", false, None);
        st2.settle("j", false, None);
        st2.settle("j", true, None);
        assert_eq!(st2.get("j").consecutive_failures, 0);
    }

    #[test]
    fn the_file_holds_iso_timestamps_and_absent_not_null_fields() {
        lmg_core::secure::key::use_test_master_key();
        let path = scratch("shape");
        let st = JobsState::open(path.clone());
        st.claim("j", 1_757_400_000_412, None);
        st.settle("j", true, Some(7));
        // Read the sealed file back through the same util every other reader uses.
        let raw = lmg_core::secure::statefile::read_secure_json(&path)
            .expect("readable")
            .expect("written");
        let line = &raw["jobs"]["j"];
        assert!(
            line["lastRunAt"].as_str().unwrap_or("").ends_with('Z'),
            "{line}"
        );
        assert!(
            lmg_core::util::parse_iso_ms(line["lastRunAt"].as_str().unwrap_or("")).is_some(),
            "the same util parses it back: {line}"
        );
        assert_eq!(line["lastOk"], json!(true));
        assert_eq!(line["lastRunId"], json!(7));
        assert!(
            line.get("lastOccurrenceKey").is_none(),
            "absent, not null: {line}"
        );
        assert!(
            line.get("consecutiveFailures").is_none(),
            "zero is omitted: {line}"
        );
    }

    #[test]
    fn a_corrupt_file_is_empty_state_not_a_boot_failure() {
        lmg_core::secure::key::use_test_master_key();
        let path = scratch("corrupt");
        std::fs::write(&path, b"this is not a sealed envelope").expect("write garbage");
        let st = JobsState::open(path.clone());
        assert_eq!(
            st.get("anything"),
            JobRunState::default(),
            "empty, no panic"
        );
        // And it recovers: the next persist overwrites the garbage with a real envelope.
        st.claim("j", 123, None);
        let re = JobsState::open(path);
        assert_eq!(re.get("j").last_run_at, Some(123));
    }

    #[test]
    fn a_missing_file_is_empty_state_without_a_warning_path() {
        // A path that does not exist yet is the "nothing has run" case.
        let st = JobsState::open(scratch("missing").with_file_name("nope.json"));
        assert_eq!(st.get("j"), JobRunState::default());
    }

    #[test]
    fn a_write_failure_warns_and_keeps_the_fact_in_memory() {
        lmg_core::secure::key::use_test_master_key();
        // A parent directory that does not exist: every persist fails, nothing panics,
        // nothing returns an error - the claim is a fact about a run that is happening
        // regardless of the disk.
        let dir = std::env::temp_dir().join(format!(
            "lmg-jobs-state-never-{}",
            lmg_core::util::random_hex(8)
        ));
        let st = JobsState::open(dir.join("gone").join("jobs-state.json"));
        st.claim("j", 42, None);
        st.settle("j", true, None);
        assert_eq!(
            st.get("j").last_run_at,
            Some(42),
            "the fact survives in memory"
        );
        assert_eq!(st.get("j").last_ok, Some(true));
    }

    #[test]
    fn forget_drops_the_line_and_keeps_it_dropped() {
        lmg_core::secure::key::use_test_master_key();
        let path = scratch("forget");
        let st = JobsState::open(path.clone());
        st.claim("gone", 1, None);
        st.settle("gone", true, None);
        st.forget("gone");
        let re = JobsState::open(path);
        assert_eq!(
            re.get("gone"),
            JobRunState::default(),
            "the line is gone after reload"
        );
        // Forgetting an unknown job writes nothing and breaks nothing.
        st.forget("never-existed");
    }

    #[test]
    fn unknown_keys_and_wrong_types_read_as_absent() {
        let v = json!({
            "lastRunAt": "not-a-timestamp",
            "lastOk": "yes",
            "lastRunId": "41",
            "consecutiveFailures": -3,
            "surprise": 1,
        });
        let st = JobRunState::from_json(&v);
        assert_eq!(st, JobRunState::default(), "each bad field degrades alone");
    }
}
