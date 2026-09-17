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

//! The one-time v1 → v2 migration (docs/11 §5): jobs.json's rows become definitions in
//! the plugins.jobs.config row, and their run facts seed jobs-state.json.
//!
//! Three properties are the whole design:
//!
//! - **Idempotent.** The rewritten jobs.json carries a `migratedAt` marker; a file with
//!   the marker (or no file at all) is a no-op. A crash between the config write and the
//!   marker write re-runs steps 3-6 against a config that already holds the migrated
//!   definitions - and the merge rule (the config side wins) makes that re-run a no-op
//!   rather than a duplication.
//! - **Stops in place.** Any failed step returns Err and leaves jobs.json untouched and
//!   unmarked: the caller (the jobs plugin's create) fails the plugin with the reason,
//!   and the scheduler never ticks over a half-migrated tree.
//! - **Diagnosable, honestly.** The report carries what moved, what conflicted, what
//!   was dropped and where the backup is. This is NOT a transaction (two files are
//!   involved) and nothing here pretends otherwise. Rollback is a documented manual
//!   step: restore jobs.json.v1.bak by hand; the definitions already in config are
//!   NOT cleaned up automatically (docs/11 §5.3).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::{def, JobDef, JobSystem};
use swiss_core::log;
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};
use swiss_host::config_store::ConfigStoreError;

/// What the migration did. Carried to the boot log and to the tests; `conflicts` are
/// the ids where the config row already held a definition and kept it (docs/11 §5.1
/// step 3), `dropped` the v1 rows that no longer parsed (the managed-store rule: one
/// hand-mangled row must not cost the table).
#[derive(Debug)]
pub struct MigrationReport {
    /// True when there was nothing to do: no jobs.json, or one already carrying the
    /// completion marker. Never an error - most boots after the first.
    pub skipped: bool,
    pub migrated: usize,
    pub conflicts: Vec<String>,
    pub dropped: Vec<(String, String)>,
    pub backup: Option<PathBuf>,
}

/// One v1 row's worth of everything the migration moves: the definition it becomes and
/// the run facts it carries (which deliberately do NOT go into config, docs/11 §5.2).
struct Incoming {
    definition: Value,
    last_run_at: Option<i64>,
    last_ok: Option<bool>,
}

/// Run the migration against a system's tree (docs/11 §5.1). Called from the jobs
/// plugin's create - before any apply and long before the tick task exists - which is
/// also why a failure here surfaces as a failed plugin rather than a half-migrated
/// scheduler.
pub fn run(system: &JobSystem) -> Result<MigrationReport, String> {
    // Step 1: no file, or a file already migrated, is a clean no-op.
    let raw = match read_secure_json(&system.jobs_path) {
        Ok(None) => {
            return Ok(MigrationReport {
                skipped: true,
                migrated: 0,
                conflicts: Vec::new(),
                dropped: Vec::new(),
                backup: None,
            })
        }
        Ok(Some(v)) => v,
        Err(err) => return Err(format!("jobs.json migration, step 1 (read): {err}")),
    };
    if raw.get("migratedAt").is_some() {
        return Ok(MigrationReport {
            skipped: true,
            migrated: 0,
            conflicts: Vec::new(),
            dropped: Vec::new(),
            backup: None,
        });
    }

    // Step 2: a sealed copy of the original, never overwritten - a later re-run (crash
    // recovery) must not clobber the one true pre-migration backup with a file whose
    // rows may already have been emptied.
    let backup = system.jobs_path.with_file_name("jobs.json.v1.bak");
    if !backup.exists() {
        std::fs::copy(&system.jobs_path, &backup)
            .map_err(|err| format!("jobs.json migration, step 2 (backup): {err}"))?;
        if let Ok(meta) = std::fs::metadata(&system.jobs_path) {
            // Keep the sealed writer's private permissions on the copy; a plain copy
            // would inherit the directory's defaults.
            let _ = std::fs::set_permissions(&backup, meta.permissions());
        }
    }

    // Step 3: parse the v1 rows. Invalid rows are dropped and recorded, not fatal.
    let entries = raw
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut incoming: BTreeMap<String, Incoming> = BTreeMap::new();
    let mut dropped: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match JobDef::from_json(entry) {
            Ok(def) => {
                let last_run_at = entry
                    .get("lastRunAt")
                    .and_then(Value::as_str)
                    .and_then(swiss_core::util::parse_iso_ms);
                let last_ok = entry.get("lastOk").and_then(Value::as_bool);
                // Same id twice in one file: the last row wins, like every table write.
                incoming.insert(
                    def.name.clone(),
                    Incoming {
                        definition: def::JobDefinition::from_v1(&def).to_config_json(),
                        last_run_at,
                        last_ok,
                    },
                );
            }
            Err(err) => dropped.push((name, err)),
        }
    }

    // Steps 3-4: merge into the config row, CAS-checked, one retry on conflict. The
    // config side wins on the same id: a definition someone saved through the v2
    // editor is newer intent than a file the v1 binary left behind.
    let mut conflicts = Vec::new();
    let mut migrated_to = 0u64;
    for attempt in 0..2 {
        let snap = system.config_store.snapshot();
        let mut row = match system.config_store.plugin_config("jobs") {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let mut defs = match row.get("definitions") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        conflicts.clear();
        for (id, inc) in &incoming {
            if defs.contains_key(id) {
                conflicts.push(id.clone());
            } else {
                defs.insert(id.clone(), inc.definition.clone());
            }
        }
        row.insert("definitions".into(), Value::Object(defs));
        match system
            .config_store
            .update_plugin("jobs", snap.revision, Value::Object(row))
        {
            Ok(after) => {
                migrated_to = after.revision;
                break;
            }
            Err(ConfigStoreError::Conflict) if attempt == 0 => {
                // Someone else saved config between our read and write. Re-read and
                // re-merge ONCE (docs/11 §5.1 step 4); a second conflict means the
                // config is under active editing and must not be fought over.
                continue;
            }
            Err(err) => {
                return Err(format!(
                    "jobs.json migration, step 4 (merge into config): {err}"
                ))
            }
        }
    }

    // Step 5: seed the run facts. Only absent fields fill in - a re-run after a crash
    // must converge on what is already true, never clobber a run that happened since.
    for (id, inc) in &incoming {
        system.state.seed(id, inc.last_run_at, inc.last_ok);
    }

    // Step 6: empty the v1 rows and leave the marker. The emptied `jobs` array is the
    // one real safeguard against an old binary booting the same data directory and
    // scheduling the table a second time (docs/10 §9 step 6).
    let marker = json!({
        "jobs": [],
        "migratedAt": log::iso_now(),
        "migratedTo": format!("config@{migrated_to}"),
        "backup": "jobs.json.v1.bak",
        "count": incoming.len(),
    });
    write_secure_json(&system.jobs_path, &marker)
        .map_err(|err| format!("jobs.json migration, step 6 (write the marker): {err}"))?;

    // Step 7: one info line with everything a human needs to audit or roll back.
    log::log(
        "info",
        "jobs v1 table migrated to config",
        Some(json!({
            "migrated": incoming.len(),
            "conflicts": conflicts.len(),
            "dropped": dropped.len(),
            "backup": backup.display().to_string(),
        })),
    );
    for (id, err) in &dropped {
        log::warn(
            "migration dropped an invalid v1 job entry",
            Some(json!({ "name": id, "err": err })),
        );
    }
    for id in &conflicts {
        log::warn(
            "migration conflict: the config definition wins",
            Some(json!({ "id": id })),
        );
    }

    Ok(MigrationReport {
        skipped: false,
        migrated: incoming.len(),
        conflicts,
        dropped,
        backup: Some(backup),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use swiss_core::secure::statefile::write_secure_json;
    use swiss_host::config_store::ConfigStore;
    use swiss_host::services::actions::LegacyCommandAction;
    use swiss_host::services::RuntimeServices;

    fn test_services() -> Arc<RuntimeServices> {
        let services = RuntimeServices::new();
        services
            .actions
            .register(Arc::new(LegacyCommandAction::new(
                services.supervisor.clone(),
            )))
            .expect("the legacy command capability registers once");
        services
    }

    fn tree(name: &str) -> (std::path::PathBuf, Arc<JobSystem>, Arc<ConfigStore>) {
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-migrate-{}-{}",
            name,
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let jobs_path = dir.join("jobs.json");
        let store = ConfigStore::from_loaded(dir.join("gateway.config.json"), json!({}));
        let sys = JobSystem::open(jobs_path.clone(), test_services(), store.clone());
        (jobs_path, sys, store)
    }

    /// A v1 file exactly as the old binary left it, run facts included.
    fn v1_file(path: &std::path::Path, rows: Value) {
        write_secure_json(path, &json!({ "jobs": rows })).expect("the v1 file writes");
    }

    fn interval_row(name: &str) -> Value {
        json!({
            "name": name,
            "command": "cmd /c echo hi",
            "everySec": 300,
            "enabled": true,
            "timeoutMs": 30000,
            "lastRunAt": "2026-09-01T03:30:00.000Z",
            "lastOk": true,
        })
    }

    /// docs/11 §9 S4: a fresh data directory is a no-op, never an error.
    #[test]
    fn a_fresh_tree_is_a_no_op() {
        let (_path, sys, store) = tree("fresh");
        let before = store.snapshot().revision;
        let report = run(&sys).expect("nothing to fail");
        assert!(report.skipped);
        assert_eq!(report.migrated, 0);
        assert!(report.backup.is_none());
        assert_eq!(store.snapshot().revision, before, "the row never moved");
    }

    /// docs/11 §9 S4: the full happy path - equivalent definitions in config, run facts
    /// in the state file, jobs.json emptied with its marker, and a backup that decrypts
    /// back to exactly what was there.
    #[test]
    fn a_v1_table_moves_into_config_state_and_a_marker() {
        let (path, sys, store) = tree("full");
        v1_file(
            &path,
            json!([interval_row("daily"), {
                "name": "nightly",
                "command": "echo ok",
                "cron": "30 3 * * *",
                "enabled": false,
                "timeoutMs": 60000,
            }]),
        );

        let report = run(&sys).expect("the migration runs");
        assert!(!report.skipped);
        assert_eq!(report.migrated, 2);
        assert!(report.conflicts.is_empty());

        // Config holds the equivalent v2 definitions, keyed by id, with v1 defaults.
        let row = store.plugin_config("jobs");
        let daily = &row["definitions"]["daily"];
        assert_eq!(
            daily["trigger"],
            json!({ "kind": "interval", "everyMs": 300000, "firstRun": "after-interval" })
        );
        assert_eq!(
            daily["action"],
            json!({
                "type": "process.legacy-command",
                "input": { "command": "cmd /c echo hi" },
                "schemaVersion": 1,
            })
        );
        assert_eq!(daily["disabled"], json!(false));
        assert_eq!(daily["timeoutMs"], json!(30000));
        let nightly = &row["definitions"]["nightly"];
        assert_eq!(nightly["trigger"]["kind"], json!("cron"));
        assert_eq!(nightly["trigger"]["expression"], json!("30 3 * * *"));
        assert_eq!(
            nightly["disabled"],
            json!(true),
            "enabled: false flips, not drops"
        );

        // Run facts moved to the state file, not into config (docs/11 §5.2).
        assert!(row["definitions"]["daily"].get("lastRunAt").is_none());
        let st = sys.run_state("daily");
        assert_eq!(
            st.last_run_at,
            swiss_core::util::parse_iso_ms("2026-09-01T03:30:00.000Z")
        );
        assert_eq!(st.last_ok, Some(true));

        // jobs.json: emptied rows plus the marker; the backup decrypts to the original.
        let after = read_secure_json(&path)
            .expect("read back")
            .expect("still there");
        assert_eq!(
            after["jobs"],
            json!([]),
            "the v1 rows must be empty - the old-binary guard"
        );
        assert!(after["migratedAt"].as_str().is_some());
        assert_eq!(after["count"], json!(2));
        assert!(after["migratedTo"]
            .as_str()
            .unwrap_or_default()
            .starts_with("config@"));
        let backup = read_secure_json(&report.backup.clone().expect("a backup"))
            .expect("read back")
            .expect("still there");
        assert_eq!(
            backup["jobs"][0]["name"],
            json!("daily"),
            "the backup is the original, sealed"
        );
    }

    /// docs/11 §9 S4: idempotency - three runs converge to the same single-copy result.
    #[test]
    fn running_three_times_converges_without_duplicates() {
        let (path, sys, store) = tree("idempotent");
        v1_file(&path, json!([interval_row("daily")]));

        for _ in 0..3 {
            run(&sys).expect("every run succeeds");
        }
        let row = store.plugin_config("jobs");
        assert_eq!(
            row["definitions"].as_object().map(|m| m.len()),
            Some(1),
            "no duplicates across re-runs"
        );
        let after = read_secure_json(&path)
            .expect("read back")
            .expect("still there");
        assert_eq!(after["count"], json!(1), "the marker's count is stable");
        assert_eq!(
            sys.run_state("daily").last_ok,
            Some(true),
            "facts seeded once, not clobbered"
        );
    }

    /// docs/11 §9 S4: crash recovery. Steps 3-5 happened (config written, facts seeded),
    /// step 6 never did - the file still holds its rows and no marker. The next run
    /// converges: no duplicates, the marker lands, nothing is executed twice.
    #[test]
    fn a_crash_between_the_config_write_and_the_marker_converges() {
        let (path, sys, store) = tree("crash");
        v1_file(&path, json!([interval_row("daily")]));

        // Simulate the crash: the config row and state hold the migration's work, but
        // jobs.json was never rewritten.
        let snap = store.snapshot();
        let mut row = store.plugin_config("jobs");
        row["definitions"]["daily"] =
            def::JobDefinition::from_v1(&JobDef::from_json(&interval_row("daily")).expect("valid"))
                .to_config_json();
        store
            .update_plugin("jobs", snap.revision, row)
            .expect("the crashed run had written config");
        sys.state.seed(
            "daily",
            swiss_core::util::parse_iso_ms("2026-09-01T03:30:00.000Z"),
            Some(true),
        );

        let report = run(&sys).expect("the recovery run succeeds");
        assert!(!report.skipped);
        assert_eq!(report.migrated, 1);
        // The crashed run's copy already sits in config, so the re-run records it as a
        // conflict - config wins, which for an IDENTICAL definition is the convergence
        // itself: nothing is overwritten, nothing duplicated. The safety is in the rule,
        // not in recognizing sameness.
        assert_eq!(report.conflicts, vec!["daily".to_string()]);
        let merged = store.plugin_config("jobs");
        assert_eq!(
            merged["definitions"].as_object().map(|m| m.len()),
            Some(1),
            "the re-run duplicated nothing"
        );
        let after = read_secure_json(&path)
            .expect("read back")
            .expect("still there");
        assert!(
            after["migratedAt"].as_str().is_some(),
            "the marker finally landed"
        );
    }

    /// docs/11 §9 S4: same id in config and in the v1 file - the config definition wins
    /// and the loss is recorded, not silent.
    #[test]
    fn the_config_definition_wins_a_same_id_conflict() {
        let (path, sys, store) = tree("conflict");
        v1_file(&path, json!([interval_row("daily")]));
        let snap = store.snapshot();
        store
            .update_plugin(
                "jobs",
                snap.revision,
                json!({ "definitions": { "daily": {
                    "title": "rewritten by hand",
                    "trigger": { "kind": "interval", "everyMs": 60000, "firstRun": "immediate" },
                    "action": { "type": "process.legacy-command", "input": { "command": "echo newer" } }
                } } }),
            )
            .expect("the config side pre-exists");

        let report = run(&sys).expect("a conflict is not a failure");
        assert_eq!(report.migrated, 1);
        assert_eq!(report.conflicts, vec!["daily".to_string()]);
        let row = store.plugin_config("jobs");
        assert_eq!(
            row["definitions"]["daily"]["title"],
            json!("rewritten by hand")
        );
        assert_eq!(
            row["definitions"]["daily"]["action"]["input"]["command"],
            json!("echo newer"),
            "the config side won"
        );
        // The backup still holds what the v1 file had - the conflict's loser is not lost.
        let backup = read_secure_json(&report.backup.clone().expect("a backup"))
            .expect("read back")
            .expect("still there");
        assert_eq!(backup["jobs"][0]["command"], json!("cmd /c echo hi"));
    }

    /// docs/11 §9 S4: an unparseable row is dropped and recorded, not fatal - the
    /// managed-store rule, applied to the migration too.
    #[test]
    fn an_invalid_row_is_dropped_and_recorded() {
        let (path, sys, _store) = tree("invalid");
        v1_file(
            &path,
            json!([
                interval_row("good"),
                { "name": "bad", "command": "", "everySec": 60 },
                { "garbage": true },
            ]),
        );
        let report = run(&sys).expect("the rest of the table still migrates");
        assert_eq!(report.migrated, 1);
        assert_eq!(report.dropped.len(), 2, "{:?}", report.dropped);
        let after = read_secure_json(&path)
            .expect("read back")
            .expect("still there");
        assert_eq!(after["count"], json!(1), "only what actually moved counts");
    }

    /// docs/11 §9 S4: a write that cannot persist fails the migration IN PLACE - no
    /// marker, the v1 file untouched - so the next boot can try again.
    #[test]
    fn an_unwritable_config_file_fails_without_touching_the_v1_file() {
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "swiss-migrate-gone-{}",
            swiss_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let jobs_path = dir.join("jobs.json");
        v1_file(&jobs_path, json!([interval_row("daily")]));
        let store =
            ConfigStore::from_loaded(dir.join("gone").join("gateway.config.json"), json!({}));
        let sys = JobSystem::open(jobs_path.clone(), test_services(), store);

        let err = run(&sys).expect_err("the config write cannot succeed");
        assert!(
            err.contains("jobs.json migration, step 4"),
            "the error names the step it stopped at: {err}"
        );
        // In place means in place: the v1 file still holds its rows, no marker, and the
        // backup (step 2, before the failure) is the only thing that changed.
        let raw = read_secure_json(&jobs_path)
            .expect("read back")
            .expect("still there");
        assert_eq!(raw["jobs"].as_array().map(Vec::len), Some(1), "untouched");
        assert!(raw.get("migratedAt").is_none(), "unmarked");
        assert_eq!(
            sys.run_state("daily"),
            crate::jobs::state::JobRunState::default(),
            "no facts were seeded either"
        );
    }
}
