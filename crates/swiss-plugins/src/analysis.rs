/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The Data agent's analysis ledger (SPEC §data.analysis): the server-side truth behind an
//! analysis, not the chat text and not the browser's memory.
//!
//! An analysis is a folder under the home (`llm/analysis/<taskId>/`): `task.json` holds the
//! conditions revisions, the execution attempts and the result metadata, and one NDJSON file per
//! result holds its rows. The four kinds of object are deliberately distinct, because they have
//! different lifetimes:
//!
//! - a REVISION is one version of the business conditions plus the SQL that answers them. Editing
//!   either makes a new revision; naming an older one as the parent makes a branch.
//! - an EXECUTION is one attempt to run one revision. Two runs of the same SQL are two executions
//!   and may legitimately return different data.
//! - a RESULT is what one execution committed: a stable `resultId`, its columns, its row count,
//!   whether it was truncated, and the rows themselves on disk. Results are immutable; a re-run
//!   makes a new one and never rewrites the old.
//! - a CHECK is one piece of evidence about a result (a row-count reconciliation, a time-zone
//!   check...). The result's verification state is derived from them, and starts undetermined:
//!   "the SQL did not error" is not "the answer is right".
//!
//! Consistency rules the rest of the system relies on:
//!
//! - a result committed after its revision stopped being current is marked superseded. It stays
//!   readable as history, but it never becomes the analysis's current answer.
//! - an execution still `running` when the store is opened is marked `interrupted` (reconcile):
//!   the process died with it, so it must never look like it is still going, nor like it finished.
//! - cancelling is its own state, distinct from failing and from the page stopping its spinner.
//! - zero rows is a result, not a proof of absence; its verification note says so.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use swiss_core::atomic_json::{write_json_atomic, write_text_atomic};
use swiss_core::util::random_hex;
use swiss_host::services::catalog::CatalogRegistry;
use tokio::sync::Notify;

/// The most rows one execution keeps. Past it the answer is truncated, and says so.
pub const ROWS_MAX: u32 = 20_000;
/// The default row budget: enough for a report, small enough to stay a pocket tool.
pub const ROWS_DEFAULT: u32 = 2_000;
/// Stored result bytes, independent of the row cap: a single wide row can exhaust it.
pub const RESULT_BYTES_MAX: usize = 20 * 1024 * 1024;
/// The most rows one page read returns.
pub const PAGE_MAX: usize = 200;
/// The longest statement the ledger accepts.
pub const SQL_MAX: usize = 20_000;
/// The default execution timeout; the agent may ask for less, never more than TIMEOUT_MAX.
pub const TIMEOUT_DEFAULT_MS: u64 = 30_000;
pub const TIMEOUT_MAX_MS: u64 = 120_000;
/// The most distinct x values a chart series groups into; past it the aggregation belongs in SQL.
const SERIES_GROUPS_MAX: usize = 200;
/// The longest free-text note on a revision or a check.
const NOTE_MAX: usize = 2_000;

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

fn id() -> String {
    random_hex(8)
}

/// One execution's lifecycle. `Interrupted` is what a restart finds; it is not a failure the
/// database reported, and it is not a cancellation anyone asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// How far the evidence goes. `Undetermined` is the default and is a honest answer, not a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verification {
    Undetermined,
    Confirmed,
    Partial,
    Failed,
}

impl Verification {
    pub fn of(state: &str) -> Option<Verification> {
        match state {
            "undetermined" => Some(Verification::Undetermined),
            "confirmed" => Some(Verification::Confirmed),
            "partial" => Some(Verification::Partial),
            "failed" => Some(Verification::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub id: String,
    pub parent: Option<String>,
    pub at: String,
    pub note: String,
    /// The business conditions, as the agent or the user stated them. Kept whole: what changed
    /// between two revisions is the audit trail, so nothing here is summarised away.
    pub conditions: Value,
    pub sql: String,
    /// Named parameters the conditions refer to. Recorded with every execution so a result can
    /// say exactly what it was computed from; the read gate still sees one plain statement.
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Execution {
    pub id: String,
    pub revision: String,
    pub sql: String,
    pub params: Value,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: RunStatus,
    pub rows: u64,
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    pub result: Option<String>,
    /// True when a newer revision became current while this ran. Its result is history, not the
    /// answer: a late completion must never overwrite the conditions the user moved on to.
    pub superseded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: String,
    pub at: String,
    /// What was checked: "rowCount", "timeZone", "joinFanout", "coverage", ... free-form so the
    /// agent names the risk it actually addressed.
    pub kind: String,
    pub state: Verification,
    pub note: String,
    /// Who asserted it: "agent", "user", or a route name. Self-assessment is labelled as such.
    pub by: String,
    /// Source execution reference. This is provenance, not proof that a claim is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// A backend-computed observation over one immutable result; absent on annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartSpec {
    /// bar | line | area | scatter. A declarative description only: nothing here is ever
    /// evaluated as code, on the server or in the page.
    pub kind: String,
    pub x: String,
    pub y: Vec<String>,
    pub title: String,
}

/// One check as a caller states it. A struct rather than another five parameters, because the
/// state and the evidence that justifies it have to travel together to be readable at all.
pub struct CheckDraft<'a> {
    pub kind: &'a str,
    pub state: &'a str,
    pub note: &'a str,
    pub by: &'a str,
    /// Optional provenance for an annotation; it never authorizes a passing check.
    pub evidence: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultMeta {
    pub id: String,
    pub execution: String,
    pub revision: String,
    pub at: String,
    pub columns: Vec<String>,
    pub row_count: u64,
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub verification: Verification,
    pub note: String,
    pub checks: Vec<Check>,
    pub chart: Option<ChartSpec>,
    pub superseded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub conn: String,
    pub kind: String,
    pub created_at: String,
    pub updated_at: String,
    pub saved: bool,
    /// The revision the analysis currently stands on. Results are read against this.
    pub current: String,
    pub revisions: Vec<Revision>,
    pub executions: Vec<Execution>,
    pub results: Vec<ResultMeta>,
}

impl Task {
    pub fn revision(&self, id: &str) -> Option<&Revision> {
        self.revisions.iter().find(|r| r.id == id)
    }

    pub fn result(&self, id: &str) -> Option<&ResultMeta> {
        self.results.iter().find(|r| r.id == id)
    }

    /// The analysis's answer: the newest committed result of the current revision. A superseded
    /// result is never it, however new it is.
    pub fn current_result(&self) -> Option<&ResultMeta> {
        self.results.iter().rev().find(|r| r.revision == self.current && !r.superseded)
    }

    pub fn head(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "conn": self.conn, "kind": self.kind,
            "createdAt": self.created_at, "updatedAt": self.updated_at, "saved": self.saved,
            "current": self.current, "revisions": self.revisions.len(),
            "executions": self.executions.len(), "results": self.results.len(),
            "result": self.current_result().map(|r| json!({
                "id": r.id, "rowCount": r.row_count, "truncated": r.truncated,
                "verification": r.verification, "at": r.at,
            })),
        })
    }
}

/// What executes one statement for the ledger. The production runner is the connection catalog's
/// read gate; tests substitute a stub, so the ledger's state machine is tested without a database.
#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, sql: &str, limit: u32) -> Result<Value, String>;
}

/// Runs through the same lease and read-only gate the Data agent's other reads use: the ledger
/// widens no permission, and a dialect without reads simply refuses.
pub struct CatalogRunner {
    catalog: Arc<CatalogRegistry>,
    conn: String,
}

impl CatalogRunner {
    pub fn new(catalog: Arc<CatalogRegistry>, conn: &str) -> CatalogRunner {
        CatalogRunner { catalog, conn: conn.to_string() }
    }
}

#[async_trait::async_trait]
impl Runner for CatalogRunner {
    async fn run(&self, sql: &str, limit: u32) -> Result<Value, String> {
        let lease = self.catalog.lease(&self.conn, "analysis").map_err(|e| e.to_string())?;
        let browser = match lease.flavor() {
            swiss_host::dbbrowser::BrowserFlavor::Db(db) => db.clone(),
            _ => return Err(format!("{} is not a MySQL or PostgreSQL connection", self.conn)),
        };
        // The lease outlives the call: it is what keeps the connection alive while it runs.
        let bound = json!(limit);
        let out = self.catalog.sql_history.observe(&self.conn, browser.label(), lease.dialect(),
            browser.run_read_query(sql, Some(&bound)), Clone::clone).await;
        drop(lease);
        out
    }
}

/// One execution's cancellation: a flag for the state, a notify to wake the select. Cancelling
/// drops the driver future at its await point and releases the lease; it does not pretend the
/// database already stopped, and the execution's record says only that we stopped waiting.
struct Cancel {
    flag: AtomicBool,
    wake: Notify,
}

pub struct AnalysisStore {
    root: PathBuf,
    /// One writer at a time: a task file is read, mutated and atomically replaced under it.
    write: Mutex<()>,
    cancels: Mutex<HashMap<String, Arc<Cancel>>>,
}

/// A dropped HTTP/Agent future or a failed result commit must not leave a phantom live run.
/// Driver cancellation is not acknowledged by this scope, so the terminal state is interrupted.
struct ExecutionLifetime<'a> { store: &'a AnalysisStore, task: String, execution: String }

impl Drop for ExecutionLifetime<'_> {
    fn drop(&mut self) {
        let Ok(_guard) = self.store.write.lock() else { return };
        if let Ok(mut map) = self.store.cancels.lock() { map.remove(&self.execution); }
        let Ok(mut task) = self.store.get(&self.task) else { return };
        let Some(run) = task.executions.iter_mut().find(|e| e.id == self.execution && e.status == RunStatus::Running) else { return };
        run.status = RunStatus::Interrupted;
        run.finished_at = Some(now());
        run.error = Some("execution ended before result commit; database cancellation is unconfirmed".into());
        let _ = self.store.put(&task);
    }
}

impl AnalysisStore {
    pub fn at(root: PathBuf) -> Arc<AnalysisStore> {
        Arc::new(AnalysisStore { root, write: Mutex::new(()), cancels: Mutex::new(HashMap::new()) })
    }

    /// Where every analysis lives, next to the Data agent's own memory notes. Under the
    /// gateway's own home (`~/.swiss`), not the user's: `home_dir()` here strayed state
    /// outside the one home the gateway keeps (paths.rs), landing consoles in `~/llm`.
    pub fn home() -> Arc<AnalysisStore> {
        AnalysisStore::at(swiss_core::paths::data_dir().join("llm").join("analysis"))
    }

    /// Adopts the tree 0.2.0 left in `~/llm` (SPEC §core.home): `analysis` and `consoles` move
    /// into the gateway home's `llm/`, beside which [AnalysisStore::home] and the console store
    /// open. Boot calls it once, before either store exists; nothing in it is overwritten.
    pub fn adopt_stray_home() {
        let data_dir = swiss_core::paths::data_dir();
        let Some(stray) = stray_llm(&data_dir, &swiss_core::paths::home_dir()) else { return };
        let report = adopt(&stray, &data_dir.join("llm"));
        if report.moved > 0 || !report.left.is_empty() {
            swiss_core::log::log("info", "llm state adopted from ~/llm", Some(json!({
                "from": stray.display().to_string(), "moved": report.moved, "left": report.left,
            })));
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, task: &str) -> PathBuf {
        self.root.join(task)
    }

    fn task_path(&self, task: &str) -> PathBuf {
        self.dir(task).join("task.json")
    }

    fn rows_path(&self, task: &str, result: &str) -> PathBuf {
        self.dir(task).join("rows").join(format!("{result}.ndjson"))
    }

    /// Ids come from the OS CSPRNG, but a caller-supplied path segment never does: a task id that
    /// is not our hex shape is simply not found, so no route can walk out of the folder.
    fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_hexdigit())
    }

    pub fn get(&self, task: &str) -> Result<Task, String> {
        if !Self::valid_id(task) {
            return Err("unknown analysis".into());
        }
        let text = std::fs::read_to_string(self.task_path(task)).map_err(|_| "unknown analysis".to_string())?;
        let mut task: Task = serde_json::from_str(&text).map_err(|why| format!("the analysis file is unreadable: {why}"))?;
        for result in &mut task.results { result.verification = verify(&result.checks); }
        Ok(task)
    }

    fn put(&self, task: &Task) -> Result<(), String> {
        let dir = self.dir(&task.id);
        std::fs::create_dir_all(dir.join("rows")).map_err(|why| why.to_string())?;
        let body = serde_json::to_value(task).map_err(|why| why.to_string())?;
        write_json_atomic(&self.task_path(&task.id), &body)
    }

    /// Every analysis, newest first; `conn` narrows to one connection's.
    pub fn list(&self, conn: Option<&str>) -> Vec<Value> {
        let mut out: Vec<Task> = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
            if let Ok(task) = self.get(&id) {
                if conn.is_none_or(|c| c == task.conn) {
                    out.push(task);
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out.iter().map(Task::head).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(&self, conn: &str, kind: &str, title: &str, sql: &str, conditions: Value, params: Value, note: &str) -> Result<Task, String> {
        if conn.trim().is_empty() {
            return Err("conn is required".into());
        }
        check_sql(sql)?;
        check_params(&params)?;
        let revision = Revision {
            id: id(), parent: None, at: now(), note: bounded(note),
            conditions, sql: sql.to_string(), params,
        };
        let task = Task {
            id: id(), title: if title.trim().is_empty() { "Untitled analysis".into() } else { bounded(title) },
            conn: conn.to_string(), kind: kind.to_string(), created_at: now(), updated_at: now(),
            saved: false, current: revision.id.clone(), revisions: vec![revision],
            executions: Vec::new(), results: Vec::new(),
        };
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        self.put(&task)?;
        Ok(task)
    }

    /// A new revision of the conditions or the SQL. `base` may be any existing revision, which is
    /// what a branch is; the analysis's current pointer moves to the new one, and any result still
    /// being computed for an older revision is superseded from here on.
    pub fn revise(&self, task: &str, base: &str, sql: &str, conditions: Value, params: Value, note: &str) -> Result<Task, String> {
        check_sql(sql)?;
        check_params(&params)?;
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let mut task = self.get(task)?;
        if task.revision(base).is_none() {
            return Err("baseRevision: no such revision on this analysis".into());
        }
        let revision = Revision {
            id: id(), parent: Some(base.to_string()), at: now(), note: bounded(note),
            conditions, sql: sql.to_string(), params,
        };
        task.current = revision.id.clone();
        task.revisions.push(revision);
        task.updated_at = now();
        self.put(&task)?;
        Ok(task)
    }

    pub fn save(&self, task: &str, saved: bool) -> Result<Task, String> {
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let mut task = self.get(task)?;
        task.saved = saved;
        task.updated_at = now();
        self.put(&task)?;
        Ok(task)
    }

    pub fn remove(&self, task: &str) -> Result<(), String> {
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let task = self.get(task)?;
        std::fs::remove_dir_all(self.dir(&task.id)).map_err(|why| why.to_string())
    }

    /// Boot recovery: an execution the last process left `running` did not finish. Mark it
    /// interrupted and keep whatever results were already committed. Returns how many.
    pub fn reconcile(&self) -> usize {
        let mut fixed = 0;
        let Ok(entries) = std::fs::read_dir(&self.root) else { return 0 };
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(mut task) = self.get(&id) else { continue };
            let mut touched = false;
            for run in &mut task.executions {
                if run.status == RunStatus::Running {
                    run.status = RunStatus::Interrupted;
                    run.finished_at = Some(now());
                    run.error = Some("the process stopped before this execution finished".into());
                    touched = true;
                    fixed += 1;
                }
            }
            if touched {
                let _ = self.put(&task);
            }
        }
        fixed
    }

    pub fn cancel(&self, execution: &str) -> bool {
        let found = self.cancels.lock().map(|m| m.get(execution).cloned()).ok().flatten();
        match found {
            Some(c) => {
                c.flag.store(true, Ordering::SeqCst);
                c.wake.notify_waiters();
                true
            }
            None => false,
        }
    }

    /// Run one revision and commit its result. The answer is the execution summary plus the new
    /// result's metadata; the rows stay on disk and are read back by page, never dumped whole
    /// into a model context.
    pub async fn execute(&self, runner: &dyn Runner, task: &str, revision: Option<&str>, max_rows: Option<u32>, timeout_ms: Option<u64>) -> Result<Value, String> {
        let budget = max_rows.unwrap_or(ROWS_DEFAULT).clamp(1, ROWS_MAX);
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(TIMEOUT_DEFAULT_MS).clamp(1_000, TIMEOUT_MAX_MS));
        let (run_id, sql, params) = {
            let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
            let mut task = self.get(task)?;
            let revision_id = revision.map(str::to_string).unwrap_or_else(|| task.current.clone());
            let Some(rev) = task.revision(&revision_id).cloned() else {
                return Err("revisionId: no such revision on this analysis".into());
            };
            check_params(&rev.params)?;
            let run = Execution {
                id: id(), revision: revision_id, sql: rev.sql.clone(), params: rev.params.clone(),
                started_at: now(), finished_at: None, status: RunStatus::Running, rows: 0,
                truncated: false, elapsed_ms: 0, error: None, result: None, superseded: false,
            };
            run_id_cancel_guard(self, &run.id)?;
            let run_id = run.id.clone();
            let sql = run.sql.clone();
            let params = run.params.clone();
            task.executions.push(run);
            task.updated_at = now();
            self.put(&task)?;
            (run_id, sql, params)
        };

        let _lifecycle = ExecutionLifetime { store: self, task: task.to_string(), execution: run_id.clone() };
        let cancel = self.cancels.lock().ok().and_then(|m| m.get(&run_id).cloned());
        let started = Instant::now();
        // One extra row is the truncation probe: the driver stops at budget + 1, and that extra
        // row is what tells a full answer from a cut one. Nothing here claims completeness it
        // did not measure.
        let outcome = match cancel {
            Some(ref c) => {
                tokio::select! {
                    out = tokio::time::timeout(timeout, runner.run(&sql, budget + 1)) => {
                        match out {
                            Ok(res) => res,
                            Err(_) => Err(format!("the statement did not finish within {} ms", timeout.as_millis())),
                        }
                    }
                    _ = c.wake.notified() => Err("cancelled".to_string()),
                }
            }
            None => match tokio::time::timeout(timeout, runner.run(&sql, budget + 1)).await {
                Ok(res) => res,
                Err(_) => Err(format!("the statement did not finish within {} ms", timeout.as_millis())),
            },
        };
        let cancelled = cancel.as_ref().is_some_and(|c| c.flag.load(Ordering::SeqCst));
        let elapsed = started.elapsed().as_millis() as u64;

        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        if let Ok(mut map) = self.cancels.lock() {
            map.remove(&run_id);
        }
        let mut task = self.get(task)?;
        // Read the CURRENT revision again after the await: if the user moved on while this ran,
        // the result still commits, but as history rather than as the answer.
        let superseded = task.executions.iter().find(|r| r.id == run_id).is_some_and(|r| r.revision != task.current);
        let Some(run) = task.executions.iter_mut().find(|r| r.id == run_id) else {
            return Err("the execution disappeared from the analysis".into());
        };
        run.finished_at = Some(now());
        run.elapsed_ms = elapsed;
        run.superseded = superseded;
        match outcome {
            Err(why) => {
                run.status = if cancelled { RunStatus::Cancelled } else { RunStatus::Failed };
                run.error = Some(if cancelled { "cancelled by request".into() } else { why.clone() });
                task.updated_at = now();
                self.put(&task)?;
                if cancelled {
                    Ok(json!({ "execution": run_id, "status": "cancelled", "elapsedMs": elapsed }))
                } else {
                    Err(why)
                }
            }
            Ok(reply) => {
                let columns: Vec<String> = reply["columns"].as_array().map(|cols| {
                    cols.iter().filter_map(|c| c.as_str().map(str::to_string)).collect()
                }).unwrap_or_default();
                let mut rows = reply["rows"].as_array().cloned().unwrap_or_default();
                for row in &mut rows { exact_json(row); }
                // The driver has its own ceiling (10,000) and clamps our budget to it, then says
                // the cap bit with `limitApplied`. Reading only our own budget meant a 20,000-row
                // budget against a driver that stopped at 10,000 produced a result marked
                // COMPLETE while the query had more: the chart and the comparison then called a
                // prefix "the data" (2026-11-01).
                let driver_capped = reply.get("limitApplied").and_then(Value::as_i64);
                let mut truncated = driver_capped.is_some() || rows.len() as u64 > u64::from(budget);
                if truncated {
                    rows.truncate(budget as usize);
                }
                let result_id = id();
                let mut text = String::new();
                let mut kept = 0;
                for row in &rows {
                    let line = serde_json::to_string(row).map_err(|why| why.to_string())?;
                    if text.len() + line.len() + 1 > RESULT_BYTES_MAX { truncated = true; break; }
                    text.push_str(&line);
                    text.push('\n');
                    kept += 1;
                }
                rows.truncate(kept);
                write_text_atomic(&self.rows_path(&task.id, &result_id), &text)?;
                let note = if rows.is_empty() && !truncated {
                    "0 rows matched. That is what this query returned, not proof that nothing exists: conditions, time zone or coverage can all empty a result.".to_string()
                } else if truncated {
                    // The number that matters is what was KEPT, which is the driver's ceiling
                    // when that is below the budget - not the budget that was asked for.
                    format!("Truncated at {} rows{}: the query had more. Totals over this result are totals over a prefix, not over the data.",
                        rows.len(),
                        match driver_capped {
                            Some(at) if (at as u64) < u64::from(budget) => format!(" (the connection returns at most {at}; the analysis asked for {budget})"),
                            _ => String::new(),
                        })
                } else {
                    String::new()
                };
                let meta = ResultMeta {
                    id: result_id.clone(), execution: run_id.clone(), revision: run.revision.clone(),
                    at: now(), columns, row_count: rows.len() as u64, truncated, elapsed_ms: elapsed,
                    verification: Verification::Undetermined, note, checks: Vec::new(), chart: None,
                    superseded,
                };
                run.status = RunStatus::Completed;
                run.rows = meta.row_count;
                run.truncated = truncated;
                run.result = Some(result_id);
                task.results.push(meta);
                task.updated_at = now();
                self.put(&task)?;
                let meta = task.results.last().cloned().ok_or("the result was not recorded")?;
                Ok(json!({
                    "execution": run_id, "status": "completed", "elapsedMs": elapsed,
                    "params": params, "superseded": superseded,
                    "result": serde_json::to_value(&meta).unwrap_or(Value::Null),
                }))
            }
        }
    }

    /// One page of a committed result. Row identity is the result id plus the row's position:
    /// results are immutable, so "row 2 of result X" means the same thing in every later turn.
    pub fn rows(&self, task: &str, result: &str, offset: usize, limit: usize) -> Result<Value, String> {
        if !Self::valid_id(result) {
            return Err("unknown result".into());
        }
        let task = self.get(task)?;
        let Some(meta) = task.result(result) else { return Err("unknown result".into()) };
        let limit = limit.clamp(1, PAGE_MAX);
        let file = std::fs::File::open(self.rows_path(&task.id, result)).map_err(|why| why.to_string())?;
        let mut rows = Vec::new();
        let mut ids = Vec::new();
        for (n, line) in BufReader::new(file).lines().enumerate() {
            if n < offset {
                continue;
            }
            if rows.len() >= limit {
                break;
            }
            let line = line.map_err(|why| why.to_string())?;
            let row: Value = serde_json::from_str(&line).map_err(|why| why.to_string())?;
            rows.push(row);
            ids.push(format!("{}:{}", meta.id, n));
        }
        let more = u64::try_from(offset + rows.len()).unwrap_or(u64::MAX) < meta.row_count;
        Ok(json!({
            "result": meta.id, "revision": meta.revision, "columns": meta.columns,
            "offset": offset, "limit": limit, "returned": rows.len(), "total": meta.row_count,
            "truncated": meta.truncated, "verification": meta.verification, "note": meta.note,
            "rows": rows, "rowIds": ids,
            "more": more,
        }))
    }

    /// Row-level comparison of two committed results, keyed on the named columns or on the whole
    /// row. Both directions are reported, with counts over every row - not over a sample. With a
    /// key, equal keys pair their rows and expose field-level changes; duplicate keys are matched
    /// exactly first and the ambiguous remainder is explicitly diagnosed.
    pub fn compare(&self, task: &str, left: &str, right: &str, keys: &[String]) -> Result<Value, String> {
        let task = self.get(task)?;
        let (Some(a), Some(b)) = (task.result(left), task.result(right)) else {
            return Err("compare needs two results of this analysis".into());
        };
        if a.id == b.id {
            return Err("compare needs two different results".into());
        }
        for key in keys {
            if !a.columns.contains(key) || !b.columns.contains(key) { return Err(format!("comparison key is not a column of both results: {key}")); }
        }
        let a_rows = read_all(&self.rows_path(&task.id, &a.id))?;
        let b_rows = read_all(&self.rows_path(&task.id, &b.id))?;
        let key_of = |meta: &ResultMeta, row: &Value| -> String {
            if keys.is_empty() { return canonical_row(meta, row); }
            let mut value = Value::Array(keys.iter().map(|k| {
                let cell = meta.columns.iter().position(|c| c == k).and_then(|at| cell_opt(row, &meta.columns, at));
                match cell { Some(value) => json!([1, value]), None => json!([0]) }
            }).collect());
            value.sort_all_objects();
            value.to_string()
        };
        let key_values = |meta: &ResultMeta, row: &Value| -> Value {
            let mut out = Map::new();
            for key in keys {
                if let Some(at) = meta.columns.iter().position(|c| c == key) {
                    if let Some(value) = cell_opt(row, &meta.columns, at) { out.insert(key.clone(), value.clone()); }
                }
            }
            Value::Object(out)
        };
        let mut a_groups: HashMap<String, Vec<&Value>> = HashMap::new();
        let mut b_groups: HashMap<String, Vec<&Value>> = HashMap::new();
        for row in &a_rows { a_groups.entry(key_of(a, row)).or_default().push(row); }
        for row in &b_rows { b_groups.entry(key_of(b, row)).or_default().push(row); }
        let mut all_keys: Vec<String> = a_groups.keys().cloned().collect();
        all_keys.extend(b_groups.keys().filter(|k| !a_groups.contains_key(*k)).cloned());
        all_keys.sort();
        let diagnostics = |meta: &ResultMeta, rows: &[Value], groups: &HashMap<String, Vec<&Value>>| -> Value {
            let duplicate_rows: u64 = if keys.is_empty() { 0 } else { groups.values().map(|rows| rows.len().saturating_sub(1) as u64).sum() };
            let duplicate_keys = if keys.is_empty() { 0 } else { groups.values().filter(|rows| rows.len() > 1).count() };
            let null_rows = if keys.is_empty() { 0 } else {
                rows.iter().filter(|row| keys.iter().any(|key| {
                    meta.columns.iter().position(|c| c == key).is_some_and(|at| cell(row, &meta.columns, at).is_null())
                })).count()
            };
            json!({ "duplicateRows": duplicate_rows, "duplicateKeys": duplicate_keys, "nullRows": null_rows })
        };
        let key_issues = json!({
            "left": diagnostics(a, &a_rows, &a_groups),
            "right": diagnostics(b, &b_rows, &b_groups),
        });
        let columns: Vec<String> = a.columns.iter().chain(b.columns.iter()).cloned().fold(Vec::new(), |mut out, col| {
            if !out.contains(&col) { out.push(col); }
            out
        });
        let differences = |left_row: &Value, right_row: &Value| -> Vec<Value> {
            columns.iter().filter(|col| !keys.contains(col)).filter_map(|col| {
                let av = a.columns.iter().position(|c| c == col).and_then(|at| cell_opt(left_row, &a.columns, at));
                let bv = b.columns.iter().position(|c| c == col).and_then(|at| cell_opt(right_row, &b.columns, at));
                (av != bv).then(|| json!({ "column": col, "left": av, "right": bv, "leftMissing": av.is_none(), "rightMissing": bv.is_none() }))
            }).collect()
        };
        let mut added = 0_u64;
        let mut removed = 0_u64;
        let mut shared = 0_u64;
        let mut changed = 0_u64;
        let mut unchanged = 0_u64;
        let mut added_sample = Vec::new();
        let mut removed_sample = Vec::new();
        let mut changed_sample = Vec::new();
        for key in all_keys {
            let av = a_groups.get(&key).cloned().unwrap_or_default();
            let bv = b_groups.get(&key).cloned().unwrap_or_default();
            shared += av.len().min(bv.len()) as u64;
            let mut left_rest = Vec::new();
            let mut right_by_row: HashMap<String, Vec<usize>> = HashMap::new();
            for (at, row) in bv.iter().enumerate() { right_by_row.entry(canonical_row(b, row)).or_default().push(at); }
            let mut matched = vec![false; bv.len()];
            // Match identical rows in linear time before pairing the remaining duplicate keys.
            for left_row in av {
                if let Some(at) = right_by_row.get_mut(&canonical_row(a, left_row)).and_then(Vec::pop) {
                    matched[at] = true;
                    unchanged += 1;
                } else { left_rest.push(left_row); }
            }
            let right_rest: Vec<&Value> = bv.into_iter().enumerate().filter(|(at, _)| !matched[*at]).map(|(_, row)| row).collect();
            let paired = left_rest.len().min(right_rest.len());
            for at in 0..paired {
                let fields = differences(left_rest[at], right_rest[at]);
                if fields.is_empty() { unchanged += 1; continue; }
                changed += 1;
                if changed_sample.len() < 20 {
                    changed_sample.push(json!({
                        "key": key_values(a, left_rest[at]), "left": left_rest[at], "right": right_rest[at], "fields": fields,
                    }));
                }
            }
            let removed_here = left_rest.len().saturating_sub(paired);
            let added_here = right_rest.len().saturating_sub(paired);
            removed += removed_here as u64;
            added += added_here as u64;
            if removed_here > 0 && removed_sample.len() < 20 {
                removed_sample.push(json!({ "key": key_values(a, left_rest[paired]), "row": left_rest[paired], "count": removed_here }));
            }
            if added_here > 0 && added_sample.len() < 20 {
                added_sample.push(json!({ "key": key_values(b, right_rest[paired]), "row": right_rest[paired], "count": added_here }));
            }
        }
        Ok(json!({
            "left": { "result": a.id, "revision": a.revision, "at": a.at, "rows": a.row_count, "truncated": a.truncated },
            "right": { "result": b.id, "revision": b.revision, "at": b.at, "rows": b.row_count, "truncated": b.truncated },
            "keys": keys, "added": added, "removed": removed, "shared": shared,
            "changed": changed, "unchanged": unchanged,
            "addedSample": added_sample, "removedSample": removed_sample, "changedSample": changed_sample,
            "columnsSame": a.columns == b.columns,
            "columns": {
                "onlyLeft": a.columns.iter().filter(|c| !b.columns.contains(c)).collect::<Vec<_>>(),
                "onlyRight": b.columns.iter().filter(|c| !a.columns.contains(c)).collect::<Vec<_>>(),
            },
            "keyIssues": key_issues, "sampleLimit": 20, "duplicatePairing": "exact rows first, then stored order; duplicate keys cannot establish row identity",
            "note": if a.truncated || b.truncated {
                "At least one side was truncated: this compares the kept prefixes, not the whole data."
            } else { "" },
        }))
    }

    /// Attach one piece of evidence. The verification state is recomputed from all checks, so one
    /// failed check cannot be talked over by later optimistic ones.
    ///
    /// Caller assertions remain annotations. Even a successful execution cannot certify a
    /// business claim: only a measured rule creates a passing check.
    pub fn record_check(&self, task: &str, result: &str, draft: CheckDraft<'_>) -> Result<Value, String> {
        let CheckDraft { kind, state, note, by, evidence } = draft;
        let Some(state) = Verification::of(state) else {
            return Err("state: undetermined, confirmed, partial or failed".into());
        };
        if kind.trim().is_empty() {
            return Err("kind is required".into());
        }
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let mut task = self.get(task)?;
        if state == Verification::Confirmed {
            return Err("evidence: confirmation requires a measured validation rule, not an execution id".into());
        }
        let evidence = evidence.map(bounded);
        let Some(meta) = task.results.iter_mut().find(|r| r.id == result) else {
            return Err("unknown result".into());
        };
        meta.checks.push(Check {
            id: id(), at: now(), kind: bounded(kind), state, note: bounded(note), by: bounded(by),
            evidence,
            measurement: None,
        });
        meta.verification = verify(&meta.checks);
        task.updated_at = now();
        let out = serde_json::to_value(meta).unwrap_or(Value::Null);
        self.put(&task)?;
        Ok(out)
    }

    /// Evaluate a constraint ourselves. These checks prove only facts about the saved rows,
    /// never the business meaning of SQL or the completeness of the source database.
    pub fn validate(&self, task: &str, result: &str, rule: &Value) -> Result<Value, String> {
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let mut task = self.get(task)?;
        let meta = task.result(result).ok_or("unknown result")?;
        let rows = read_all(&self.rows_path(&task.id, result))?;
        let kind = rule["type"].as_str().unwrap_or_default();
        let columns: Vec<&str> = rule["columns"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let mut positions = Vec::new();
        if kind != "rowCount" {
            if columns.is_empty() { return Err("columns: at least one column is required".into()) }
            for name in &columns {
                positions.push(meta.columns.iter().position(|c| c == name).ok_or_else(|| format!("not a column: {name}"))?);
            }
        }
        let (actual, expected) = match kind {
            "rowCount" => (rows.len() as u64, rule["expected"].as_u64().ok_or("expected: a nonnegative integer is required")?),
            "nonNull" => (rows.iter().filter(|r| positions.iter().any(|at| cell(r, &meta.columns, *at).is_null())).count() as u64, 0),
            "unique" => {
                let mut seen = std::collections::HashSet::new();
                let mut duplicates = 0;
                for r in &rows {
                    let key: Vec<&Value> = positions.iter().map(|at| cell(r, &meta.columns, *at)).collect();
                    if !seen.insert(serde_json::to_string(&key).map_err(|why| why.to_string())?) { duplicates += 1; }
                }
                (duplicates, 0)
            }
            _ => return Err("type: unique, nonNull or rowCount".into()),
        };
        let state = if actual != expected { Verification::Failed }
            else if meta.truncated { Verification::Partial }
            else if rows.is_empty() && kind != "rowCount" { Verification::Undetermined }
            else { Verification::Confirmed };
        let evidence = meta.execution.clone();
        let measurement = json!({ "result": result, "revision": meta.revision, "rule": rule,
            "actual": actual, "expected": expected, "scope": "storedRows", "truncated": meta.truncated });
        let meta = task.results.iter_mut().find(|r| r.id == result).ok_or("unknown result")?;
        meta.checks.push(Check { id: id(), at: now(), kind: kind.into(), state,
            note: "Checks cover stored rows only; business definitions and source coverage remain unverified.".into(),
            by: "runtime".into(), evidence: Some(evidence), measurement: Some(measurement) });
        meta.verification = verify(&meta.checks);
        let out = serde_json::to_value(meta).map_err(|why| why.to_string())?;
        task.updated_at = now();
        self.put(&task)?;
        Ok(out)
    }

    /// Store a chart description against a result. Only columns the result actually has are
    /// accepted, and only from a fixed list of kinds: the page renders this declaratively and
    /// never evaluates generated code.
    pub fn set_chart(&self, task: &str, result: &str, spec: &Value) -> Result<Value, String> {
        let _guard = self.write.lock().map_err(|_| "the analysis store is poisoned")?;
        let mut task = self.get(task)?;
        let Some(meta) = task.results.iter_mut().find(|r| r.id == result) else {
            return Err("unknown result".into());
        };
        let kind = spec["kind"].as_str().unwrap_or_default();
        if !matches!(kind, "bar" | "line" | "area" | "scatter") {
            return Err("chart.kind: bar, line, area or scatter".into());
        }
        let x = spec["x"].as_str().unwrap_or_default().to_string();
        if !meta.columns.contains(&x) {
            return Err("chart.x: not a column of this result".into());
        }
        let y: Vec<String> = spec["y"].as_array().map(|ys| {
            ys.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        }).unwrap_or_default();
        if y.is_empty() || y.len() > 4 {
            return Err("chart.y: one to four column names".into());
        }
        for col in &y {
            if !meta.columns.contains(col) {
                return Err(format!("chart.y: {col} is not a column of this result"));
            }
        }
        let chart = ChartSpec {
            kind: kind.to_string(), x, y,
            title: bounded(spec["title"].as_str().unwrap_or_default()),
        };
        meta.chart = Some(chart);
        task.updated_at = now();
        let out = serde_json::to_value(meta).unwrap_or(Value::Null);
        self.put(&task)?;
        Ok(out)
    }

    /// The chart's numbers, computed over EVERY stored row rather than over the page on screen.
    /// Counts are exact; sums are f64 and are labelled approximate, because an exact decimal total
    /// is a question for the database, not for a JSON file.
    pub fn series(&self, task: &str, result: &str, x: &str, y: &[String]) -> Result<Value, String> {
        let task = self.get(task)?;
        let Some(meta) = task.result(result) else { return Err("unknown result".into()) };
        let Some(x_at) = meta.columns.iter().position(|c| c == x) else {
            return Err("x: not a column of this result".into());
        };
        let mut y_at = Vec::new();
        for col in y {
            let Some(at) = meta.columns.iter().position(|c| c == col) else {
                return Err(format!("y: {col} is not a column of this result"));
            };
            y_at.push(at);
        }
        if y_at.is_empty() {
            return Err("y: at least one column".into());
        }
        let rows = read_all(&self.rows_path(&task.id, result))?;
        let mut groups: HashMap<String, (Vec<f64>, Vec<u64>, u64)> = HashMap::new();
        let mut order: Vec<(String, String)> = Vec::new();
        let mut approximate = false;
        let mut invalid = 0_u64;
        for row in &rows {
            let mut category = cell(row, &meta.columns, x_at).clone();
            category.sort_all_objects();
            let key = category.to_string();
            let label = match &category {
                Value::String(s) if serde_json::from_str::<Value>(s).is_err() => s.clone(),
                _ => key.clone(),
            };
            if !groups.contains_key(&key) {
                if groups.len() >= SERIES_GROUPS_MAX {
                    return Err(format!("more than {SERIES_GROUPS_MAX} distinct {x} values: aggregate in SQL first"));
                }
                order.push((key.clone(), label));
                groups.insert(key.clone(), (vec![0.0; y_at.len()], vec![0; y_at.len()], 0));
            }
            let Some((sums, counts, rows_in)) = groups.get_mut(&key) else { continue };
            *rows_in += 1;
            for (i, at) in y_at.iter().enumerate() {
                let value = cell(row, &meta.columns, *at);
                let number = match value {
                    Value::Number(n) => {
                        if n.as_i64().is_none() && n.as_u64().is_none() { approximate = true; }
                        n.as_f64()
                    }
                    Value::String(s) => {
                        // Decimal strings remain exact in storage; the chart is approximate.
                        approximate = true;
                        s.parse::<f64>().ok()
                    }
                    Value::Null => continue,
                    _ => None,
                };
                let Some(v) = number.filter(|n| n.is_finite()) else { invalid += 1; continue };
                let sum = sums[i] + v;
                if !sum.is_finite() { return Err(format!("chart sum overflow for {col}; aggregate in SQL first", col = y[i])); }
                sums[i] = sum;
                counts[i] += 1;
                if sum.abs() > 9_007_199_254_740_991.0 { approximate = true; }
            }
        }
        let points: Vec<Value> = order.iter().map(|(key, label)| {
            let (sums, counts, rows_in) = groups.get(key).cloned().unwrap_or_default();
            let mut point = Map::new();
            point.insert("x".into(), json!(label));
            point.insert("rows".into(), json!(rows_in));
            let mut values = Map::new();
            for (i, col) in y.iter().enumerate() {
                let value = json!({ "sum": sums[i], "n": counts[i] });
                values.insert(col.clone(), value.clone());
                if !matches!(col.as_str(), "x" | "rows" | "values") { point.insert(col.clone(), value); }
            }
            point.insert("values".into(), Value::Object(values));
            Value::Object(point)
        }).collect();
        Ok(json!({
            "result": meta.id, "x": x, "y": y, "points": points,
            "rows": meta.row_count, "truncated": meta.truncated, "approximate": approximate, "invalid": invalid,
            "note": if meta.truncated {
                "Computed over the kept (truncated) rows only."
            } else if approximate {
                "Sums are approximate (floating point); exact decimal totals belong in SQL."
            } else { "" },
        }))
    }

    /// A scatter chart's observations, one point per stored row. Unlike `series`, this never
    /// groups or sums: repeated x values remain separate points. Values retain their exact JSON
    /// representation; the view labels the numeric conversion needed for SVG as approximate.
    pub fn scatter_series(&self, task: &str, result: &str, x: &str, y: &[String]) -> Result<Value, String> {
        let task = self.get(task)?;
        let Some(meta) = task.result(result) else { return Err("unknown result".into()) };
        let Some(x_at) = meta.columns.iter().position(|c| c == x) else {
            return Err("x: not a column of this result".into());
        };
        if y.is_empty() { return Err("y: at least one column".into()); }
        let mut y_at = Vec::new();
        for col in y {
            let Some(at) = meta.columns.iter().position(|c| c == col) else {
                return Err(format!("y: {col} is not a column of this result"));
            };
            y_at.push(at);
        }
        let rows = read_all(&self.rows_path(&task.id, result))?;
        let mut approximate = false;
        let mut invalid = 0_u64;
        let mut numeric = |value: &Value| -> bool {
            match value {
                Value::Number(n) => {
                    if n.as_i64().is_none() && n.as_u64().is_none()
                        || n.as_i64().is_some_and(|v| !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&v))
                        || n.as_u64().is_some_and(|v| v > 9_007_199_254_740_991) { approximate = true; }
                    n.as_f64().is_some_and(f64::is_finite)
                }
                Value::String(s) => {
                    let valid = s.parse::<f64>().ok().is_some_and(f64::is_finite);
                    if valid { approximate = true; }
                    valid
                }
                _ => false,
            }
        };
        let mut points = Vec::new();
        for row in &rows {
            let x_value = cell(row, &meta.columns, x_at);
            let valid_x = numeric(x_value);
            let mut point = Map::new();
            point.insert("x".into(), x_value.clone());
            let mut valid_y = true;
            let mut values = Map::new();
            for (i, col) in y.iter().enumerate() {
                let value = cell(row, &meta.columns, y_at[i]);
                valid_y &= numeric(value);
                values.insert(col.clone(), value.clone());
                if !matches!(col.as_str(), "x" | "values") { point.insert(col.clone(), value.clone()); }
            }
            point.insert("values".into(), Value::Object(values));
            if valid_x && valid_y { points.push(Value::Object(point)); }
            else { invalid += 1; }
        }
        Ok(json!({
            "result": meta.id, "mode": "scatter", "x": x, "y": y, "points": points,
            "rows": meta.row_count, "invalid": invalid, "truncated": meta.truncated, "approximate": approximate,
            "note": if meta.truncated {
                "Points cover the kept (truncated) rows only; numeric display may be approximate."
            } else if approximate || invalid > 0 {
                "Numeric display may be approximate; rows with a non-numeric or infinite coordinate are omitted."
            } else { "" },
        }))
    }
}

// Normalize legacy positional rows and current object rows through their column names.
fn canonical_row(meta: &ResultMeta, row: &Value) -> String {
    let map: Map<String, Value> = meta.columns.iter().enumerate().filter_map(|(at, col)| cell_opt(row, &meta.columns, at).map(|value| (col.clone(), value.clone()))).collect();
    let mut value = Value::Object(map);
    value.sort_all_objects();
    value.to_string()
}

/// One cell of a stored row, by column position.
///
/// Rows are written exactly as the driver replied: an OBJECT keyed by column name. Reading them
/// positionally (`row.get(0)`) found nothing, so every row fell into one empty group and a chart
/// or a comparison over real data showed zeros (2026-11-01). A positional array is still read
/// too, so a result written by an older build keeps working.
fn cell<'a>(row: &'a Value, columns: &[String], at: usize) -> &'a Value {
    cell_opt(row, columns, at).unwrap_or(&Value::Null)
}

/// Comparison retains absence separately from a present JSON null.
fn cell_opt<'a>(row: &'a Value, columns: &[String], at: usize) -> Option<&'a Value> {
    match row {
        Value::Array(items) => items.get(at),
        Value::Object(map) => columns.get(at).and_then(|c| map.get(c)),
        _ => None,
    }
}

/// JSON numbers outside JavaScript's exact integer range travel as strings, including nested
/// values. The database driver's decimal strings already carry their original precision.
fn exact_json(value: &mut Value) {
    match value {
        Value::Number(n) if n.as_u64().is_some_and(|v| v > 9_007_199_254_740_991)
            || n.as_i64().is_some_and(|v| v < -9_007_199_254_740_991) => { *value = Value::String(n.to_string()); }
        Value::Array(items) => { for item in items { exact_json(item); } }
        Value::Object(map) => { for item in map.values_mut() { exact_json(item); } }
        _ => {}
    }
}

/// Passing local checks give partial verification: business meaning and source coverage remain
/// open. Legacy self-asserted checks cannot promote a result simply because they were stored.
fn verify(checks: &[Check]) -> Verification {
    if checks.iter().any(|c| c.state == Verification::Failed) {
        return Verification::Failed;
    }
    if checks.iter().any(|c| c.state == Verification::Partial) {
        return Verification::Partial;
    }
    if checks.iter().any(|c| c.state == Verification::Undetermined) {
        return Verification::Undetermined;
    }
    if checks.iter().any(|c| c.state == Verification::Confirmed && c.measurement.is_some()) {
        return Verification::Partial;
    }
    Verification::Undetermined
}

fn read_all(path: &Path) -> Result<Vec<Value>, String> {
    let file = std::fs::File::open(path).map_err(|why| why.to_string())?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|why| why.to_string())?;
        out.push(serde_json::from_str(&line).map_err(|why| why.to_string())?);
    }
    Ok(out)
}

fn check_sql(sql: &str) -> Result<(), String> {
    if sql.trim().is_empty() {
        return Err("sql is required".into());
    }
    if sql.chars().count() > SQL_MAX {
        return Err(format!("sql: at most {SQL_MAX} characters"));
    }
    Ok(())
}

fn check_params(params: &Value) -> Result<(), String> {
    if params.is_null() || params.as_object().is_some_and(|p| p.is_empty()) { Ok(()) }
    else { Err("bound parameters are not supported by this runner; supply complete read SQL rather than unexecuted parameter metadata".into()) }
}

fn bounded(text: &str) -> String {
    text.chars().take(NOTE_MAX).collect()
}

/// Where 0.2.0 sealed the ledger and the consoles: `~/llm`, the user's home rather than the
/// gateway's. Only the default home adopts it. 0.2.0 wrote every instance's records to that one
/// directory, so an instance under a `$SWISS_HOME` of its own (a scratch or test home) never
/// carries off the default instance's records.
fn stray_llm(data_dir: &Path, user_home: &Path) -> Option<PathBuf> {
    (data_dir == swiss_core::paths::default_data_dir_in(user_home)).then(|| user_home.join("llm"))
}

/// What one adoption did: how many renames moved a tree or an entry, and the stray paths left in
/// place because the home already holds an entry of that name (or the rename failed).
#[derive(Debug, Default, PartialEq)]
struct Adopted {
    moved: usize,
    left: Vec<String>,
}

fn adopt(stray: &Path, llm: &Path) -> Adopted {
    let mut report = Adopted::default();
    for name in ["analysis", "consoles"] {
        let from = stray.join(name);
        if from.is_dir() {
            move_merging(&from, &llm.join(name), &mut report);
        }
    }
    // `~/llm` is a plain name the user may also use: it goes only once nothing else is in it.
    let _ = std::fs::remove_dir(stray);
    report
}

/// Moves `from` to `to`: whole, in one rename, when `to` is free; otherwise entry by entry into
/// the directory already there. A file both sides hold stays where it is - both copies survive,
/// and the stray one is named in the report for the user to settle.
fn move_merging(from: &Path, to: &Path, report: &mut Adopted) {
    if !to.exists() {
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(from, to) {
            Ok(()) => report.moved += 1,
            Err(e) => report.left.push(format!("{}: {e}", from.display())),
        }
        return;
    }
    let entries = match (from.is_dir() && to.is_dir()).then(|| std::fs::read_dir(from)) {
        Some(Ok(entries)) => entries,
        _ => {
            report.left.push(from.display().to_string());
            return;
        }
    };
    for entry in entries.flatten() {
        move_merging(&entry.path(), &to.join(entry.file_name()), report);
    }
    let _ = std::fs::remove_dir(from);
}

fn run_id_cancel_guard(store: &AnalysisStore, run: &str) -> Result<(), String> {
    let mut map = store.cancels.lock().map_err(|_| "the analysis store is poisoned")?;
    map.insert(run.to_string(), Arc::new(Cancel { flag: AtomicBool::new(false), wake: Notify::new() }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn store() -> Arc<AnalysisStore> {
        let dir = std::env::temp_dir().join(format!("swiss-analysis-{}", random_hex(6)));
        AnalysisStore::at(dir)
    }

    struct Stub {
        rows: usize,
        columns: Vec<String>,
        calls: AtomicUsize,
        fail: AtomicBool,
        hold: AtomicBool,
    }

    impl Stub {
        fn of(rows: usize) -> Stub {
            Stub {
                rows, columns: vec!["id".into(), "total".into()], calls: AtomicUsize::new(0),
                fail: AtomicBool::new(false), hold: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl Runner for Stub {
        async fn run(&self, _sql: &str, limit: u32) -> Result<Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err("the database refused the statement".into());
            }
            while self.hold.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let n = self.rows.min(limit as usize);
            let rows: Vec<Value> = (0..n).map(|i| json!([i + 1, (i + 1) * 10])).collect();
            Ok(json!({ "columns": self.columns, "rows": rows }))
        }
    }

    fn task(store: &AnalysisStore) -> Task {
        store.create("pg1", "postgres", "VIP customers", "select 1", json!({ "minOrders": 3 }), json!({}), "first pass").expect("create")
    }

    /// The driver's REAL shape: one JSON object per row, keyed by column, plus the `limitApplied`
    /// note when its own ceiling bit. Every other stub here returns positional arrays, which is
    /// exactly why a chart and a comparison could read nothing from a real database and still
    /// pass their tests (2026-11-01).
    struct Objects {
        rows: Vec<Value>,
        capped: Option<i64>,
    }

    #[async_trait::async_trait]
    impl Runner for Objects {
        async fn run(&self, _sql: &str, _limit: u32) -> Result<Value, String> {
            let mut out = json!({ "columns": ["region", "amount"], "rows": self.rows });
            if let Some(at) = self.capped {
                out["limitApplied"] = json!(at);
            }
            Ok(out)
        }
    }

    struct Reply(Value);

    #[async_trait::async_trait]
    impl Runner for Reply {
        async fn run(&self, _sql: &str, _limit: u32) -> Result<Value, String> { Ok(self.0.clone()) }
    }

    #[tokio::test]
    async fn comparing_reordered_columns_and_nested_objects_does_not_invent_changes() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Reply(json!({ "columns": ["a", "b"], "rows": [[1, { "one": 1, "two": 2 }]] })), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Reply(json!({ "columns": ["b", "a"], "rows": [{ "b": { "two": 2, "one": 1 }, "a": 1 }] })), &task.id, None, None, None).await.expect("b");
        for keys in [vec![], vec!["a".to_string()]] {
            let diff = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &keys).expect("compare");
            assert_eq!((diff["added"].as_u64(), diff["removed"].as_u64(), diff["changed"].as_u64()), (Some(0), Some(0), Some(0)), "{diff}");
        }
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn analysis_context_rejects_excess_and_invalid_row_ids() {
        let store = store();
        let task = task(&store);
        let result = store.execute(&Stub::of(25), &task.id, None, None, None).await.expect("run");
        let id = result["result"]["id"].as_str().expect("result");
        let ids: Vec<String> = (0..21).map(|i| format!("{id}:{i}")).collect();
        assert!(crate::llm_db_analysis::view_context(&store, "pg1", &json!({ "taskId": task.id, "resultId": id, "rowIds": ids })).is_err());
        assert!(crate::llm_db_analysis::view_context(&store, "pg1", &json!({ "taskId": task.id, "resultId": id, "rowIds": [42] })).is_err());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn comparison_canonicalizes_object_keys_and_distinguishes_missing_cells() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Reply(json!({ "columns": ["key", "amount"], "rows": [{ "key": { "a": 1, "b": { "x": 1, "y": 2 } } }] })), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Reply(json!({ "columns": ["key", "amount"], "rows": [{ "key": { "b": { "y": 2, "x": 1 }, "a": 1 }, "amount": null }] })), &task.id, None, None, None).await.expect("b");
        let diff = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &["key".into()]).expect("compare");
        assert_eq!((diff["added"].as_u64(), diff["removed"].as_u64(), diff["changed"].as_u64()), (Some(0), Some(0), Some(1)), "{diff}");
        assert_eq!(diff["changedSample"][0]["fields"][0]["leftMissing"], true);
        assert_eq!(diff["changedSample"][0]["fields"][0]["rightMissing"], false);
        let whole = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &[]).expect("whole");
        assert_eq!((whole["added"].as_u64(), whole["removed"].as_u64()), (Some(1), Some(1)), "{whole}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn comparison_refuses_a_key_one_side_does_not_have() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Reply(json!({ "columns": ["id", "amount"], "rows": [[1, 2]] })), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Reply(json!({ "columns": ["key", "amount"], "rows": [["k", 2]] })), &task.id, None, None, None).await.expect("b");
        let why = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &["id".into()])
            .expect_err("a key the right side lacks cannot pair rows");
        assert!(why.contains("not a column of both results"), "{why}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn chart_columns_named_x_rows_or_values_do_not_replace_coordinates() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Reply(json!({ "columns": ["position", "x", "rows", "values"], "rows": [{ "position": 100, "x": 7, "rows": 8, "values": 9 }] })), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("result");
        let columns = ["x".into(), "rows".into(), "values".into()];
        let scatter = store.scatter_series(&task.id, result, "position", &columns).expect("scatter");
        assert_eq!(scatter["points"][0]["x"], 100);
        assert_eq!(scatter["points"][0]["values"], json!({ "x": 7, "rows": 8, "values": 9 }));
        let series = store.series(&task.id, result, "position", &columns).expect("series");
        assert_eq!(series["points"][0]["x"], "100");
        assert_eq!(series["points"][0]["rows"], 1);
        assert_eq!(series["points"][0]["values"]["x"]["sum"], 7.0);
        let _ = std::fs::remove_dir_all(store.root());
    }

    fn region(rows: &[(&str, i64)]) -> Objects {
        Objects {
            rows: rows.iter().map(|(name, amount)| json!({ "region": name, "amount": amount })).collect(),
            capped: None,
        }
    }

    #[tokio::test]
    async fn a_run_commits_a_result_that_a_second_process_can_read_back() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(3), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("result id").to_string();
        assert_eq!(out["result"]["rowCount"], json!(3));
        assert_eq!(out["result"]["verification"], json!("undetermined"));

        // Reopened from disk, with no memory of the run: the rows and their identity survive.
        let reopened = AnalysisStore::at(store.root().to_path_buf());
        let page = reopened.rows(&task.id, &result, 0, 2).expect("page");
        assert_eq!(page["rows"], json!([[1, 10], [2, 20]]));
        assert_eq!(page["more"], json!(true));
        assert_eq!(page["rowIds"][0], json!(format!("{result}:0")));
        let last = reopened.rows(&task.id, &result, 2, 2).expect("last page");
        assert_eq!(last["rows"], json!([[3, 30]]));
        assert_eq!(last["more"], json!(false));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn the_row_budget_marks_a_cut_result_instead_of_calling_it_complete() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(50), &task.id, None, Some(10), None).await.expect("run");
        assert_eq!(out["result"]["rowCount"], json!(10));
        assert_eq!(out["result"]["truncated"], json!(true));
        assert!(out["result"]["note"].as_str().unwrap_or_default().contains("Truncated"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn zero_rows_is_not_reported_as_proof_of_absence() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(0), &task.id, None, None, None).await.expect("run");
        assert_eq!(out["result"]["rowCount"], json!(0));
        assert!(out["result"]["note"].as_str().unwrap_or_default().contains("not proof"));
        assert_eq!(out["result"]["verification"], json!("undetermined"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_revision_makes_a_new_version_and_a_late_result_becomes_history() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(2);
        let first = store.execute(&stub, &task.id, None, None, None).await.expect("first");
        let old_result = first["result"]["id"].as_str().expect("id").to_string();
        let revised = store.revise(&task.id, &task.current, "select 2", json!({ "minOrders": 4 }), json!({}), "threshold 4").expect("revise");
        assert_ne!(revised.current, task.current);
        assert_eq!(revised.revisions.len(), 2);
        assert_eq!(revised.revisions[1].parent.as_deref(), Some(task.current.as_str()));

        // A run that belongs to the old revision, finishing after the revision changed: kept, but
        // marked superseded, and never the analysis's current answer.
        let late = store.execute(&stub, &task.id, Some(&task.current), None, None).await.expect("late");
        assert_eq!(late["superseded"], json!(true));
        let now = store.get(&task.id).expect("get");
        assert_eq!(now.current_result().map(|r| r.id.clone()), None);
        assert_eq!(now.results.len(), 2);
        assert!(now.result(&old_result).is_some(), "the old result is still readable history");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn two_runs_of_one_revision_are_two_results_not_one_overwritten() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Stub::of(1), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Stub::of(3), &task.id, None, None, None).await.expect("b");
        assert_ne!(a["result"]["id"], b["result"]["id"]);
        assert!(a["execution"] != b["execution"]);
        let task = store.get(&task.id).expect("get");
        assert_eq!(task.results.len(), 2);
        assert_eq!(task.current_result().map(|r| r.row_count), Some(3));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_restart_marks_an_unfinished_execution_interrupted() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(1);
        stub.hold.store(true, Ordering::SeqCst);
        let running = {
            let store = store.clone();
            let id = task.id.clone();
            tokio::spawn(async move { store.execute(&stub, &id, None, None, None).await })
        };
        // Wait until the execution is on disk as running, then simulate the process dying.
        for _ in 0..100 {
            let on_disk = store.get(&task.id).expect("get");
            if on_disk.executions.iter().any(|r| r.status == RunStatus::Running) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let reopened = AnalysisStore::at(store.root().to_path_buf());
        assert_eq!(reopened.reconcile(), 1);
        let after = reopened.get(&task.id).expect("get");
        assert_eq!(after.executions[0].status, RunStatus::Interrupted);
        assert!(after.results.is_empty(), "nothing was committed, so nothing is claimed");
        running.abort();
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn cancelling_a_run_records_a_cancellation_not_a_failure() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(1);
        stub.hold.store(true, Ordering::SeqCst);
        let running = {
            let store = store.clone();
            let id = task.id.clone();
            tokio::spawn(async move { store.execute(&stub, &id, None, None, None).await })
        };
        let mut run_id = String::new();
        for _ in 0..100 {
            let on_disk = store.get(&task.id).expect("get");
            if let Some(run) = on_disk.executions.first() {
                run_id = run.id.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!run_id.is_empty());
        assert!(store.cancel(&run_id), "a live execution can be cancelled");
        let out = running.await.expect("join").expect("cancelled run answers");
        assert_eq!(out["status"], json!("cancelled"));
        let after = store.get(&task.id).expect("get");
        assert_eq!(after.executions[0].status, RunStatus::Cancelled);
        assert!(!store.cancel(&run_id), "a finished execution is not cancellable");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_failed_statement_commits_no_result() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(1);
        stub.fail.store(true, Ordering::SeqCst);
        let why = store.execute(&stub, &task.id, None, None, None).await.expect_err("fails");
        assert!(why.contains("refused"));
        let after = store.get(&task.id).expect("get");
        assert_eq!(after.executions[0].status, RunStatus::Failed);
        assert!(after.results.is_empty());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_comparison_counts_both_directions_over_every_row() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Stub::of(2), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Stub::of(3), &task.id, None, None, None).await.expect("b");
        let diff = store.compare(&task.id, a["result"]["id"].as_str().unwrap_or_default(), b["result"]["id"].as_str().unwrap_or_default(), &[]).expect("compare");
        assert_eq!(diff["added"], json!(1));
        assert_eq!(diff["removed"], json!(0));
        assert_eq!(diff["shared"], json!(2));
        let keyed = store.compare(&task.id, a["result"]["id"].as_str().unwrap_or_default(), b["result"]["id"].as_str().unwrap_or_default(), &["id".into()]).expect("keyed");
        assert_eq!(keyed["added"], json!(1));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn evidence_moves_the_verification_state_and_a_failure_dominates() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(2), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id").to_string();
        let execution = out["execution"].as_str().expect("execution id").to_string();
        // A bare "confirmed" is the model asserting its own homework: refused, with nothing behind
        // it (2026-11-01). Naming an execution of this analysis is what makes it evidence.
        let err = store.record_check(&task.id, &result, CheckDraft { kind: "rowCount", state: "confirmed", note: "matches the orders table count", by: "agent", evidence: None })
            .expect_err("no evidence, no confirmation");
        assert!(err.contains("measured"), "{err}");
        let err = store.record_check(&task.id, &result, CheckDraft { kind: "rowCount", state: "confirmed", note: "matches", by: "agent", evidence: Some("nope") })
            .expect_err("an execution that is not this analysis's");
        assert!(err.contains("measured"), "{err}");
        let one = store.validate(&task.id, &result, &json!({ "type": "rowCount", "expected": 2 })).expect("check");
        assert_eq!(one["verification"], json!("partial"));
        assert_eq!(one["checks"][0]["measurement"]["actual"], json!(2));
        assert_eq!(one["checks"][0]["evidence"], json!(execution));
        let two = store.record_check(&task.id, &result, CheckDraft { kind: "joinFanout", state: "failed", note: "the join duplicates customers", by: "agent", evidence: None }).expect("check");
        assert_eq!(two["verification"], json!("failed"));
        let three = store.validate(&task.id, &result, &json!({ "type": "rowCount", "expected": 2 })).expect("check");
        assert_eq!(three["verification"], json!("failed"), "a failed check is not talked over");
        assert_eq!(three["checks"].as_array().map(|c| c.len()), Some(3));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn an_execution_that_did_not_complete_is_no_evidence() {
        let store = store();
        let task = task(&store);
        let ok = store.execute(&Stub::of(2), &task.id, None, None, None).await.expect("run");
        let result = ok["result"]["id"].as_str().expect("id").to_string();
        let failed = Stub::of(2);
        failed.fail.store(true, Ordering::SeqCst);
        assert!(store.execute(&failed, &task.id, None, None, None).await.is_err(), "the stub fails");
        let execution = store.get(&task.id).expect("task").executions.last().expect("attempt").id.clone();
        let err = store.record_check(&task.id, &result, CheckDraft { kind: "rowCount", state: "confirmed", note: "claim", by: "agent", evidence: Some(&execution) })
            .expect_err("a failed attempt proves nothing");
        assert!(err.contains("measured"), "{err}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_chart_is_declarative_and_refuses_columns_the_result_lacks() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(2), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id").to_string();
        let err = store.set_chart(&task.id, &result, &json!({ "kind": "bar", "x": "nope", "y": ["total"] })).expect_err("bad column");
        assert!(err.contains("not a column"));
        let err = store.set_chart(&task.id, &result, &json!({ "kind": "javascript:alert(1)", "x": "id", "y": ["total"] })).expect_err("bad kind");
        assert!(err.contains("chart.kind"));
        let ok = store.set_chart(&task.id, &result, &json!({ "kind": "bar", "x": "id", "y": ["total"], "title": "Totals" })).expect("chart");
        assert_eq!(ok["chart"]["kind"], json!("bar"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_chart_reads_the_rows_the_driver_really_wrote() {
        // Rows arrive as objects keyed by column. Read positionally, every row landed in one
        // unnamed group with a zero total: a chart over real data showed nothing (2026-11-01).
        let store = store();
        let task = task(&store);
        let out = store.execute(&region(&[("north", 10), ("north", 5), ("south", 7)]), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id").to_string();
        let series = store.series(&task.id, &result, "region", &["amount".to_string()]).expect("series");
        let points = series["points"].as_array().expect("points");
        assert_eq!(points.len(), 2, "two regions, not one bucket of nothing: {series}");
        assert_eq!(points[0]["x"], json!("north"));
        assert_eq!(points[0]["amount"]["sum"], json!(15.0));
        assert_eq!(points[0]["amount"]["n"], json!(2));
        assert_eq!(points[1]["x"], json!("south"));
        assert_eq!(points[1]["amount"]["sum"], json!(7.0));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn chart_categories_preserve_json_types() {
        let store = store();
        let task = task(&store);
        let data = Objects { rows: vec![json!({ "region": null, "amount": 2 }), json!({ "region": "null", "amount": 3 }), json!({ "region": 1, "amount": 4 }), json!({ "region": "1", "amount": 5 })], capped: None };
        let out = store.execute(&data, &task.id, None, None, None).await.expect("run");
        let series = store.series(&task.id, out["result"]["id"].as_str().expect("id"), "region", &["amount".into()]).expect("series");
        let points = series["points"].as_array().expect("points");
        assert_eq!(points.len(), 4, "{series}");
        assert_eq!(points.iter().map(|p| p["values"]["amount"]["sum"].as_f64().expect("sum")).collect::<Vec<_>>(), vec![2.0, 3.0, 4.0, 5.0]);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn chart_ignores_nonfinite_inputs_and_rejects_overflow() {
        let store = store();
        let task = task(&store);
        let data = Objects { rows: vec![json!({ "region": "north", "amount": "NaN" }), json!({ "region": "north", "amount": "Infinity" }), json!({ "region": "north", "amount": "2" })], capped: None };
        let out = store.execute(&data, &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id");
        let series = store.series(&task.id, result, "region", &["amount".into()]).expect("series");
        assert_eq!(series["invalid"], json!(2));
        assert_eq!(series["points"][0]["values"]["amount"]["sum"], json!(2.0));
        assert_eq!(series["points"][0]["values"]["amount"]["n"], json!(1));
        let overflow = Objects { rows: vec![json!({ "region": "north", "amount": "1e308" }), json!({ "region": "north", "amount": "1e308" })], capped: None };
        let out = store.execute(&overflow, &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id");
        assert!(store.series(&task.id, result, "region", &["amount".into()]).expect_err("overflow").contains("overflow"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_keyed_comparison_tells_object_rows_apart() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&region(&[("north", 10), ("north", 5), ("south", 7)]), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&region(&[("north", 10), ("north", 5), ("east", 3)]), &task.id, None, None, None).await.expect("b");
        let diff = store.compare(
            &task.id,
            a["result"]["id"].as_str().unwrap_or_default(),
            b["result"]["id"].as_str().unwrap_or_default(),
            &["region".into()],
        ).expect("compare");
        // Keyed on nothing (the old behaviour) every row was the same row: shared 3, added 0.
        assert_eq!(diff["shared"], json!(2));
        assert_eq!(diff["added"], json!(1), "east is new");
        assert_eq!(diff["removed"], json!(1), "south is gone");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_driver_that_caps_below_the_budget_still_makes_a_truncated_result() {
        // The budget is 20,000; the driver returns at most 10,000 and says so with `limitApplied`.
        // Reading only our own budget called those 10,000 rows the whole answer (2026-11-01).
        let store = store();
        let task = task(&store);
        let rows: Vec<(String, i64)> = (0..10).map(|i| (format!("r{i}"), i)).collect();
        let stub = Objects {
            rows: rows.iter().map(|(name, amount)| json!({ "region": name, "amount": amount })).collect(),
            capped: Some(10),
        };
        let out = store.execute(&stub, &task.id, None, Some(ROWS_MAX), None).await.expect("run");
        assert_eq!(out["result"]["rowCount"], json!(10));
        assert_eq!(out["result"]["truncated"], json!(true), "the driver stopped at its own ceiling");
        let note = out["result"]["note"].as_str().unwrap_or_default();
        assert!(note.contains("Truncated at 10 rows"), "{note}");
        assert!(note.contains("at most 10"), "the note names the connection's ceiling: {note}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_scatter_series_keeps_duplicate_x_values_as_independent_points() {
        // SPEC §data.analysis: a scatter plot is not a grouped sum. Its numeric x axis and every
        // stored point survive, including two observations at the same x.
        let store = store();
        let task = task(&store);
        let data = Objects {
            rows: vec![
                json!({ "region": 1, "amount": 10 }),
                json!({ "region": 2, "amount": 20 }),
                json!({ "region": 2, "amount": 25 }),
                json!({ "region": 100, "amount": 30 }),
            ],
            capped: None,
        };
        let out = store.execute(&data, &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("result").to_string();
        let series = store.scatter_series(&task.id, &result, "region", &["amount".into()]).expect("scatter");
        let points = series["points"].as_array().expect("points");
        assert_eq!(points.len(), 4, "duplicate x values are not aggregated away: {series}");
        assert_eq!(points[0]["x"], json!(1));
        assert_eq!(points[1]["amount"], json!(20));
        assert_eq!(points[2]["x"], json!(2));
        assert_eq!(points[2]["amount"], json!(25));
        assert_eq!(points[3]["x"], json!(100));
        assert_eq!(series["mode"], json!("scatter"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_keyed_comparison_reports_changed_fields_and_key_diagnostics() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&region(&[("north", 10), ("south", 7), ("west", 4)]), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&region(&[("north", 12), ("south", 7), ("east", 3)]), &task.id, None, None, None).await.expect("b");
        let diff = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &["region".into()]).expect("compare");
        assert_eq!(diff["changed"], json!(1));
        assert_eq!(diff["unchanged"], json!(1));
        assert_eq!(diff["added"], json!(1));
        assert_eq!(diff["removed"], json!(1));
        assert_eq!(diff["changedSample"][0]["fields"][0]["column"], json!("amount"));
        assert_eq!(diff["changedSample"][0]["fields"][0]["left"], json!(10));
        assert_eq!(diff["changedSample"][0]["fields"][0]["right"], json!(12));

        let duplicate = store.execute(&region(&[("north", 10), ("north", 11)]), &task.id, None, None, None).await.expect("duplicate");
        let diagnosed = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), duplicate["result"]["id"].as_str().expect("duplicate"), &["region".into()]).expect("compare duplicates");
        assert_eq!(diagnosed["keyIssues"]["right"]["duplicateRows"], json!(1));

        let null_data = Objects { rows: vec![json!({ "region": Value::Null, "amount": 1 })], capped: None };
        let null_result = store.execute(&null_data, &task.id, None, None, None).await.expect("null key");
        let nulls = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), null_result["result"]["id"].as_str().expect("null"), &["region".into()]).expect("compare nulls");
        assert_eq!(nulls["keyIssues"]["right"]["nullRows"], json!(1));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_chart_series_is_computed_over_all_stored_rows() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(4), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("id").to_string();
        let series = store.series(&task.id, &result, "id", &["total".into()]).expect("series");
        let points = series["points"].as_array().expect("points");
        assert_eq!(points.len(), 4);
        assert_eq!(points[3]["total"]["sum"], json!(40.0));
        assert_eq!(points[3]["rows"], json!(1));
        let err = store.series(&task.id, &result, "missing", &["total".into()]).expect_err("bad x");
        assert!(err.contains("not a column"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_saved_analysis_survives_and_a_deleted_one_takes_its_rows_with_it() {
        let store = store();
        let task = task(&store);
        store.execute(&Stub::of(1), &task.id, None, None, None).await.expect("run");
        let saved = store.save(&task.id, true).expect("save");
        assert!(saved.saved);
        assert_eq!(store.list(Some("pg1")).len(), 1);
        assert_eq!(store.list(Some("other")).len(), 0);
        store.remove(&task.id).expect("remove");
        assert!(store.get(&task.id).is_err());
        assert!(!store.dir(&task.id).exists());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_task_id_is_never_a_path() {
        let store = store();
        assert!(store.get("../../gateway.config").is_err());
        assert!(store.rows("..", "..", 0, 1).is_err());
    }

    /// The home's own address (SPEC §core.home): joining onto the user's home once landed the
    /// ledger - and with it the console store, which rides this root - in `~/llm` outside the
    /// one home that owns every byte of state.
    #[test]
    fn the_home_is_the_gateway_home_not_the_users() {
        // Pin the scratch home before reading it: test_home() installs SWISS_HOME lazily, so a
        // sibling test installing it between two reads here would compare two different homes.
        // Equality, not a prefix: the scratch user profile sits inside the scratch home, so the
        // old `~/llm` began with it too.
        let home = swiss_core::paths::test_home();
        assert_eq!(AnalysisStore::home().root(), home.join("llm").join("analysis"),
            "the ledger lives under the gateway home (~/.swiss), never the user's own");
    }

    fn adopt_scratch() -> PathBuf {
        std::env::temp_dir().join(format!("swiss-adopt-{}", random_hex(6)))
    }

    fn plant(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
        std::fs::write(path, text).expect("plant");
    }

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// 0.2.0's stray tree moves whole into a home that has neither tree, and a second boot finds
    /// nothing to do. A file of the user's own keeps `~/llm` where it is.
    #[test]
    fn the_stray_trees_move_whole_and_a_foreign_file_keeps_its_folder() {
        let base = adopt_scratch();
        let (stray, llm) = (base.join("llm"), base.join(".swiss").join("llm"));
        plant(stray.join("analysis").join("t1").join("task.json"), "ledger");
        plant(stray.join("consoles").join("6d79").join("a.json"), "sealed");
        plant(stray.join("notes.txt"), "mine");
        std::fs::create_dir_all(llm.join("db-memory")).expect("the home's own llm dir");

        assert_eq!(adopt(&stray, &llm), Adopted { moved: 2, left: Vec::new() });
        assert_eq!(read(llm.join("analysis").join("t1").join("task.json")), "ledger");
        assert_eq!(read(llm.join("consoles").join("6d79").join("a.json")), "sealed");
        assert!(!stray.join("analysis").exists() && !stray.join("consoles").exists());
        assert_eq!(read(stray.join("notes.txt")), "mine", "the user's own file is never touched");

        assert_eq!(adopt(&stray, &llm), Adopted::default(), "the second boot moves nothing");
        std::fs::remove_file(stray.join("notes.txt")).expect("unplant");
        adopt(&stray, &llm);
        assert!(!stray.exists(), "an emptied ~/llm goes");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A home that already holds records (a build with the fix ran before the stray tree was
    /// adopted) takes the stray entries beside its own; a name both sides hold is never
    /// overwritten - the stray copy stays and is reported.
    #[test]
    fn a_home_with_records_merges_the_stray_tree_and_never_overwrites() {
        let base = adopt_scratch();
        let (stray, llm) = (base.join("llm"), base.join(".swiss").join("llm"));
        plant(stray.join("consoles").join("6d79").join("a.json"), "stray a");
        plant(stray.join("consoles").join("6d79").join("b.json"), "stray b");
        plant(stray.join("analysis").join("t2").join("task.json"), "stray t2");
        plant(llm.join("consoles").join("6d79").join("a.json"), "home a");
        plant(llm.join("analysis").join("t1").join("task.json"), "home t1");

        let report = adopt(&stray, &llm);
        assert_eq!(report.moved, 2, "b.json and t2: {report:?}");
        let conflict = stray.join("consoles").join("6d79").join("a.json");
        assert_eq!(report.left, vec![conflict.display().to_string()]);
        assert_eq!(read(llm.join("consoles").join("6d79").join("a.json")), "home a");
        assert_eq!(read(llm.join("consoles").join("6d79").join("b.json")), "stray b");
        assert_eq!(read(llm.join("analysis").join("t1").join("task.json")), "home t1");
        assert_eq!(read(llm.join("analysis").join("t2").join("task.json")), "stray t2");
        assert_eq!(read(conflict), "stray a", "both copies survive");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn only_the_default_home_adopts_the_stray_tree() {
        let user = std::env::temp_dir().join("swiss-adopt-user");
        let default = swiss_core::paths::default_data_dir_in(&user);
        assert_eq!(stray_llm(&default, &user), Some(user.join("llm")));
        assert_eq!(stray_llm(&user.join("scratch-home"), &user), None,
            "a $SWISS_HOME of its own never carries off the default instance's records");
    }

    #[tokio::test]
    async fn the_execution_budget_is_capped_and_the_driver_is_asked_for_one_extra_row() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(5);
        store.execute(&stub, &task.id, None, Some(1_000_000), None).await.expect("run");
        assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
        let out = store.execute(&stub, &task.id, None, Some(2), None).await.expect("run");
        assert_eq!(out["result"]["truncated"], json!(true));
        assert_eq!(out["result"]["rowCount"], json!(2));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_successful_execution_does_not_prove_a_claim() {
        // SPEC §data.analysis: a completed SELECT is not validation evidence.
        let store = store();
        let task = task(&store);
        let out = store.execute(&Stub::of(2), &task.id, None, None, None).await.expect("run");
        let why = store.record_check(&task.id, out["result"]["id"].as_str().expect("result"), CheckDraft {
            kind: "joinFanout", state: "confirmed", note: "no duplicates", by: "agent",
            evidence: out["execution"].as_str(),
        }).expect_err("an execution cannot certify itself");
        assert!(why.contains("measured"), "{why}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn measured_constraints_find_duplicates_and_never_certify_business_meaning() {
        let store = store();
        let task = task(&store);
        let out = store.execute(&region(&[("north", 10), ("north", 5)]), &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("result");
        let checked = store.validate(&task.id, result, &json!({ "type": "unique", "columns": ["region"] })).expect("validate");
        assert_eq!(checked["verification"], "failed");
        assert_eq!(checked["checks"][0]["measurement"]["actual"], 1);
        assert_eq!(checked["checks"][0]["measurement"]["result"], result);
        let later = store.validate(&task.id, result, &json!({ "type": "nonNull", "columns": ["region"] })).expect("validate");
        assert_eq!(later["verification"], "failed", "a passing check cannot erase a failure");
        assert!(store.validate(&task.id, result, &json!({ "type": "unique", "columns": ["missing"] })).is_err());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn dropping_an_execution_never_leaves_a_phantom_running_record() {
        let store = store();
        let task = task(&store);
        let stub = Stub::of(1);
        stub.hold.store(true, Ordering::SeqCst);
        let mut pending = Box::pin(store.execute(&stub, &task.id, None, None, None));
        tokio::select! {
            _ = &mut pending => panic!("held query completed"),
            _ = tokio::task::yield_now() => {}
        }
        assert_eq!(store.get(&task.id).expect("task").executions[0].status, RunStatus::Running);
        drop(pending);
        let after = store.get(&task.id).expect("task");
        assert_eq!(after.executions[0].status, RunStatus::Interrupted);
        assert!(!store.cancel(&after.executions[0].id));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn result_numbers_stay_exact_for_a_javascript_reader() {
        let store = store();
        let task = task(&store);
        let data = Objects { rows: vec![json!({ "region": "x", "amount": 9_007_199_254_740_993_u64 })], capped: None };
        let out = store.execute(&data, &task.id, None, None, None).await.expect("run");
        let result = out["result"]["id"].as_str().expect("result");
        assert_eq!(store.rows(&task.id, result, 0, 1).expect("rows")["rows"][0]["amount"], "9007199254740993");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_comparison_refuses_keys_missing_from_either_result() {
        let store = store();
        let task = task(&store);
        let a = store.execute(&Stub::of(1), &task.id, None, None, None).await.expect("a");
        let b = store.execute(&Stub::of(1), &task.id, None, None, None).await.expect("b");
        let why = store.compare(&task.id, a["result"]["id"].as_str().expect("a"), b["result"]["id"].as_str().expect("b"), &["missing".into()]).expect_err("unknown keys cannot match as null");
        assert!(why.contains("column"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[tokio::test]
    async fn a_byte_budget_limits_materialized_results_even_with_very_wide_rows() {
        let store = store();
        let task = task(&store);
        let wide = "x".repeat(11 * 1024 * 1024);
        let data = Objects { rows: vec![json!({ "region": "a", "amount": wide }), json!({ "region": "b", "amount": wide })], capped: None };
        let out = store.execute(&data, &task.id, None, None, None).await.expect("run");
        assert_eq!(out["result"]["rowCount"], 1);
        assert_eq!(out["result"]["truncated"], true);
        let _ = std::fs::remove_dir_all(store.root());
    }
}
