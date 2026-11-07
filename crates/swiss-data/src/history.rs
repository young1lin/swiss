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

//! The console history (SPEC §data.history, §host.history): one `logs/data.db` row per
//! statement or command a console ran - `{ts, target, conn, dialect, stmt, ok, error?,
//! rows?, ms}` - kept by the DATABASE it ran in, never by the browser that sent it.
//!
//! `target` is the browser's own label, the database the request really reached: `app @
//! host:3306` for MySQL and pg, `host:6379 db 0` for redis, `<db> @ <hosts>` for a mongo
//! command. A pg schema switch is not a different database and a MySQL console runs on its
//! configured one, so neither can file a statement under the wrong list; two connection
//! names pointed at one database read one history. The browser-local list this replaces
//! (`swiss.dbSqlHistory`, 50 entries, every connection in one list) showed a pg statement
//! under a MySQL connection.
//!
//! Retention is a row bound: the newest [KEEP_ROWS] survive, swept along the append path
//! every [TRIM_EVERY] admits. A statement longer than [STMT_MAX] bytes keeps its head and
//! says it was cut. A write that is refused warns ONCE per open and never fails the run it
//! records: the history is a record, not a gate (the process history's rule).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{Durability, ModuleHistoryStore, OwnerPolicy};

/// The row bound: the newest rows survive, the rest go. A hundred thousand statements is
/// years of one developer's consoles in a few tens of MB.
pub const KEEP_ROWS: i64 = 100_000;
/// The retention sweep rides every TRIM_EVERY admits, not every write.
const TRIM_EVERY: u64 = 256;
/// A statement is stored up to this many bytes (cut on a char boundary, `cut` set): a
/// pasted dump is a run worth recording, not a row worth a megabyte.
pub const STMT_MAX: usize = 64 * 1024;
/// The driver's error, cut the same way: the first lines say what went wrong.
const ERROR_MAX: usize = 4 * 1024;
/// One page of the read route, at most.
pub const PAGE_MAX: usize = 500;

static DATA_SCHEMA: &[SchemaStep] = &[SchemaStep::new(
    1,
    &[
        // One row per statement run. IF NOT EXISTS for the shared-home first boot (the
        // process schema's reason): the stamp still gates every sequential migration.
        "CREATE TABLE IF NOT EXISTS runs (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, \
         ts INTEGER NOT NULL, \
         target TEXT NOT NULL, \
         conn TEXT NOT NULL, \
         dialect TEXT NOT NULL, \
         stmt TEXT NOT NULL, \
         cut INTEGER NOT NULL DEFAULT 0, \
         ok INTEGER NOT NULL, \
         error TEXT, \
         rows INTEGER, \
         ms INTEGER)",
        "CREATE INDEX IF NOT EXISTS runs_target ON runs (target, id DESC)",
    ],
)];

/// Layer the history onto the /api/db router: with `None` the consoles record nothing and
/// the read pair answers 503, which is what a composition without a home's logs should see.
pub fn mounted<S>(router: axum::Router<S>, history: Option<Arc<DataHistory>>) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match history {
        Some(h) => router.layer(axum::Extension(h)),
        None => router,
    }
}

/// One run as the routes record it: what ran, where, and how it ended.
#[derive(Clone, Debug, PartialEq)]
pub struct NewRun {
    pub target: String,
    pub conn: String,
    pub dialect: String,
    pub stmt: String,
    pub ok: bool,
    pub error: Option<String>,
    pub rows: Option<i64>,
    pub ms: u64,
}

impl NewRun {
    /// A console reply's outcome as a run: `rows` is the reply's `rowCount` when it has one,
    /// the error the driver's own words.
    pub fn of(target: String, conn: &str, dialect: &str, stmt: &str, out: &Result<Value, String>, ms: u64) -> Self {
        NewRun {
            target,
            conn: conn.to_string(),
            dialect: dialect.to_string(),
            stmt: stmt.to_string(),
            ok: out.is_ok(),
            error: out.as_ref().err().cloned(),
            rows: out.as_ref().ok().and_then(|v| v.get("rowCount")).and_then(Value::as_i64),
            ms,
        }
    }
}

/// One stored run, as the read route answers it.
#[derive(Clone, Debug)]
pub struct Run {
    pub id: i64,
    pub ts: u64,
    pub conn: String,
    pub dialect: String,
    pub stmt: String,
    pub cut: bool,
    pub ok: bool,
    pub error: Option<String>,
    pub rows: Option<i64>,
    pub ms: Option<i64>,
}

impl Run {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "ts": self.ts,
            "conn": self.conn,
            "dialect": self.dialect,
            "stmt": self.stmt,
            "ok": self.ok,
        });
        let o = v.as_object_mut().expect("the literal above is an object");
        if self.cut {
            o.insert("cut".into(), json!(true));
        }
        if let Some(e) = &self.error {
            o.insert("error".into(), json!(e));
        }
        if let Some(r) = self.rows {
            o.insert("rows".into(), json!(r));
        }
        if let Some(ms) = self.ms {
            o.insert("ms".into(), json!(ms));
        }
        v
    }
}

impl swiss_host::dbbrowser::execution::SqlRecorder for DataHistory {
    fn record(&self, run: swiss_host::dbbrowser::execution::SqlRun) {
        self.record(NewRun { target: run.target, conn: run.conn, dialect: run.dialect,
            stmt: run.stmt, ok: run.ok, error: run.error, rows: run.rows, ms: run.ms });
    }
}

/// Cut `s` to at most `max` bytes on a char boundary; true when anything was cut.
fn cut_at(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// The one writer and reader of the console history. Cheap to construct closed (no IO, the
/// seam's invariant): the Data plugin's start opens it and its stop closes it, so a disabled
/// Data view opens nothing.
pub struct DataHistory {
    store: Arc<ModuleHistoryStore>,
    /// The logs directory data.db lives in.
    logs_dir: PathBuf,
    /// Admits since open: the retention sweep rides every TRIM_EVERYth.
    appends: AtomicU64,
    /// Warn once per open, whatever went wrong.
    warned: AtomicBool,
}

impl DataHistory {
    pub fn new(logs_dir: PathBuf) -> Arc<Self> {
        let policy = OwnerPolicy {
            db_file: logs_dir.join("data.db"),
            queue_byte_bound: 8 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Normal,
            schema: DATA_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("data", policy);
        Arc::new(DataHistory {
            store,
            logs_dir,
            appends: AtomicU64::new(0),
            warned: AtomicBool::new(false),
        })
    }

    /// The file this store opens. The Data plugin publishes it; nothing else is handed the path.
    pub fn database_file(&self) -> PathBuf {
        self.store.policy().db_file.clone()
    }

    /// Open `<logs>/data.db`. The caller is the Data plugin's start, never a constructor.
    pub async fn open(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.logs_dir)
            .map_err(|e| format!("creating the logs directory: {e}"))?;
        self.store
            .open()
            .await
            .map_err(|e| format!("opening the data history database: {e}"))?;
        // The declared default share, no protected window: a console history is the
        // operator's convenience, and the honest over-share denial is the plain reason.
        self.store
            .set_quota(swiss_host::history::quota::QuotaPolicy::default(), false);
        self.warned.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Close bounded (SPEC §host.history): withdraw, drain, release.
    pub async fn close(&self) -> Result<(), swiss_host::history::store::OwnerLifecycleError> {
        self.store.close().await
    }

    /// Re-enroll under a declared share (the composition root's seam, and the tests'
    /// wedge shape). The open-time enrollment stays [QuotaPolicy::default].
    pub fn set_quota(&self, policy: swiss_host::history::quota::QuotaPolicy) {
        self.store.set_quota(policy, false);
    }

    /// Record one run. Synchronous bounded admission: the queue takes it now or refuses it,
    /// and a refusal warns once - a history write never fails the run it records.
    pub fn record(&self, run: NewRun) {
        let trim = self.appends.fetch_add(1, Ordering::Relaxed).is_multiple_of(TRIM_EVERY);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let (stmt, cut) = cut_at(&run.stmt, STMT_MAX);
        let error = run.error.map(|e| cut_at(&e, ERROR_MAX).0);
        let estimated = stmt.len() + error.as_ref().map(String::len).unwrap_or(0) + run.target.len() + run.conn.len() + 96;
        let NewRun { target, conn, dialect, ok, rows, ms, .. } = run;
        let submitted = self.store.submit(estimated, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO runs (ts, target, conn, dialect, stmt, cut, ok, error, rows, ms) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(ts)
                .bind(&target)
                .bind(&conn)
                .bind(&dialect)
                .bind(&stmt)
                .bind(cut as i64)
                .bind(ok as i64)
                .bind(&error)
                .bind(rows)
                .bind(ms as i64)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                if trim {
                    sqlx::query("DELETE FROM runs WHERE id <= \
                                 (SELECT id FROM runs ORDER BY id DESC LIMIT 1 OFFSET ?)")
                        .bind(KEEP_ROWS)
                        .execute(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        });
        if let Err(err) = submitted {
            self.warn_once(format!("the data history write was refused: {err}"));
        }
    }

    /// One page of a database's history, newest FIRST: the runs older than `before` (an id
    /// from the previous page), optionally only those whose statement holds `q` (a literal,
    /// ASCII case folded - §host.history's search rule). The bool says an older page exists.
    /// Every admitted write has landed before the read looks, so a run is in the very next
    /// page.
    pub async fn page(
        &self,
        target: &str,
        q: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<Run>, bool), String> {
        if let Err(err) = self.store.flush_barrier().await {
            self.warn_once(format!("the data history barrier: {err}"));
        }
        let limit = limit.clamp(1, PAGE_MAX) as i64;
        let target = target.to_string();
        let q = q.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let mut rows = self
            .store
            .query(move |pool| {
                Box::pin(async move {
                    sqlx::query_as::<_, RunRow>(
                        "SELECT id, ts, conn, dialect, stmt, cut, ok, error, rows, ms FROM runs \
                         WHERE target = ? AND id < ? AND (? IS NULL OR instr(lower(stmt), lower(?)) > 0) \
                         ORDER BY id DESC LIMIT ?",
                    )
                    .bind(&target)
                    .bind(before.unwrap_or(i64::MAX))
                    .bind(&q)
                    .bind(&q)
                    .bind(limit + 1)
                    .fetch_all(&pool)
                    .await
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        Ok((rows.into_iter().map(RunRow::into_run).collect(), more))
    }

    /// Forget one database's history; answers how many runs went. Exempt from the share: a
    /// clear is how an operator gets bytes back, never something the budget may refuse.
    pub async fn clear(&self, target: &str) -> Result<u64, String> {
        let target = target.to_string();
        let gone = Arc::new(AtomicU64::new(0));
        let count = gone.clone();
        self.store
            .submit_exempt(128, move |pool| {
                Box::pin(async move {
                    let r = sqlx::query("DELETE FROM runs WHERE target = ?")
                        .bind(&target)
                        .execute(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                    count.store(r.rows_affected(), Ordering::Relaxed);
                    Ok(())
                })
            })
            .map_err(|e| e.to_string())?;
        self.store.flush_barrier().await.map_err(|e| e.to_string())?;
        Ok(gone.load(Ordering::Relaxed))
    }

    /// The hourly maintenance seat's body (SPEC §host.history, §data.history): the row
    /// bound swept first, on submit_exempt - a quota that refuses its own reclamation
    /// can never get the bytes back, and the share would wedge with no exit (the store
    /// contract) - then the compaction a reclaimed file needs (VACUUM plus a TRUNCATE
    /// checkpoint: in WAL mode the vacuum's rewrite lands in the log, and a log left at
    /// that size counts against the share as much as the free pages did), and the
    /// store's own pass (checkpoint, ledger reconciliation) with the usage refresh that
    /// ends it. Without this pass an open that measured a file already at its share
    /// denies every write forever: the per-write trim rides ADMITTED writes, so a
    /// denied one can never start the reclaim, and the boot sweep is what un-wedges a
    /// restart.
    pub async fn maintain(&self) {
        let gone = Arc::new(AtomicU64::new(0));
        let count = gone.clone();
        let submitted = self.store.submit_exempt(128, move |pool| {
            Box::pin(async move {
                let r = sqlx::query(
                    "DELETE FROM runs WHERE id <= \
                     (SELECT id FROM runs ORDER BY id DESC LIMIT 1 OFFSET ?)",
                )
                .bind(KEEP_ROWS)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                count.store(r.rows_affected(), Ordering::Relaxed);
                Ok(())
            })
        });
        let swept = match submitted {
            Ok(()) => {
                // The sweep lands before anything prices the file: a compaction ahead
                // of it would rewrite pages the sweep is about to free.
                self.store.flush_barrier().await.map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        };
        if let Err(err) = swept {
            self.warn_once(format!("the data history sweep was refused: {err}"));
        }
        let gone = gone.load(Ordering::Relaxed);
        let share = self.store.share().unwrap_or(u64::MAX);
        let on_disk = swiss_host::history::quota::measure(&self.store.policy().db_file).total();
        if gone > 0 || on_disk > share {
            // The hourly pass of a healthy store does not pay a rewrite: the compaction
            // runs only when the sweep freed pages or the file sits over its share -
            // the wedged shape this whole pass exists to end. A busy reader makes
            // either step fail; the next pass tries again.
            let compacted = self
                .store
                .submit_exempt(64, |pool| {
                    Box::pin(async move {
                        sqlx::query("VACUUM").execute(&pool).await.map_err(|e| e.to_string())?;
                        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                            .execute(&pool)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(())
                    })
                })
                .map_err(|e| e.to_string());
            if let Err(err) = compacted {
                self.warn_once(format!("the data history compaction failed: {err}"));
            }
        }
        let receipt = match self.store.maintain().await {
            Ok(receipt) => receipt,
            Err(err) => {
                self.warn_once(format!("the data history maintain failed: {err}"));
                return;
            }
        };
        let after = swiss_host::history::quota::measure(&self.store.policy().db_file).total();
        if gone > 0 || receipt.vacuumed || after != on_disk {
            swiss_core::log::log(
                "info",
                "data.db maintained",
                Some(json!({
                    "rows": gone,
                    "vacuumed": receipt.vacuumed,
                    "bytesBefore": on_disk,
                    "bytesAfter": after,
                })),
            );
        }
    }

    fn warn_once(&self, message: String) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            swiss_core::log::warn(&message, None);
        }
    }
}

/// One stored row as it comes off the query; the i64 -> bool folds live in `into_run`.
struct RunRow {
    id: i64,
    ts: i64,
    conn: String,
    dialect: String,
    stmt: String,
    cut: i64,
    ok: i64,
    error: Option<String>,
    rows: Option<i64>,
    ms: Option<i64>,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for RunRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row as _;
        Ok(RunRow {
            id: row.try_get("id")?,
            ts: row.try_get("ts")?,
            conn: row.try_get("conn")?,
            dialect: row.try_get("dialect")?,
            stmt: row.try_get("stmt")?,
            cut: row.try_get("cut")?,
            ok: row.try_get("ok")?,
            error: row.try_get("error")?,
            rows: row.try_get("rows")?,
            ms: row.try_get("ms")?,
        })
    }
}

impl RunRow {
    fn into_run(self) -> Run {
        Run {
            id: self.id,
            ts: self.ts.max(0) as u64,
            conn: self.conn,
            dialect: self.dialect,
            stmt: self.stmt,
            cut: self.cut != 0,
            ok: self.ok != 0,
            error: self.error,
            rows: self.rows,
            ms: self.ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo's scratch-home pattern: a unique subtree under SWISS_HOME (pinned by
    /// test-utils), so a test database never touches a real home.
    fn scratch(tag: &str) -> PathBuf {
        let dir = swiss_core::paths::test_home().join("data-history").join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn run(target: &str, stmt: &str, ok: bool) -> NewRun {
        NewRun {
            target: target.to_string(),
            conn: "c".to_string(),
            dialect: "mysql".to_string(),
            stmt: stmt.to_string(),
            ok,
            error: (!ok).then(|| "boom".to_string()),
            rows: ok.then_some(3),
            ms: 7,
        }
    }

    #[tokio::test]
    async fn each_database_reads_only_its_own_runs_newest_first() {
        let h = DataHistory::new(scratch("targets"));
        h.open().await.expect("open");
        h.record(run("app @ h:3306", "SELECT 1", true));
        h.record(run("shop @ h:5432", "SELECT now()", true));
        h.record(run("app @ h:3306", "UPDATE t SET a = 1", false));
        let (mine, more) = h.page("app @ h:3306", None, None, 10).await.expect("page");
        assert!(!more);
        assert_eq!(mine.iter().map(|r| r.stmt.as_str()).collect::<Vec<_>>(), vec!["UPDATE t SET a = 1", "SELECT 1"]);
        assert_eq!(mine[0].error.as_deref(), Some("boom"));
        assert_eq!(mine[1].rows, Some(3));
        assert_eq!(mine[1].ms, Some(7));
        let (pg, _) = h.page("shop @ h:5432", None, None, 10).await.expect("page");
        assert_eq!(pg.len(), 1, "a pg run never lands under the mysql database");
        h.close().await.expect("close");
    }

    #[tokio::test]
    async fn pages_walk_back_by_id_and_q_is_a_folded_literal() {
        let h = DataHistory::new(scratch("pages"));
        h.open().await.expect("open");
        for i in 0..5 {
            h.record(run("t", &format!("select {i} from Orders"), true));
        }
        h.record(run("t", "PING", true));
        let (first, more) = h.page("t", None, None, 2).await.expect("first");
        assert!(more);
        assert_eq!(first[0].stmt, "PING");
        let (second, _) = h.page("t", None, Some(first[1].id), 2).await.expect("second");
        assert_eq!(second[0].stmt, "select 3 from Orders");
        let (hits, more) = h.page("t", Some("ORDERS"), None, 10).await.expect("q");
        assert_eq!(hits.len(), 5);
        assert!(!more);
        let (none, _) = h.page("t", Some("%"), None, 10).await.expect("literal");
        assert!(none.is_empty(), "% is a byte, not a wildcard");
        h.close().await.expect("close");
    }

    #[tokio::test]
    async fn a_long_statement_keeps_its_head_and_says_so() {
        let h = DataHistory::new(scratch("cut"));
        h.open().await.expect("open");
        let long = format!("INSERT INTO t VALUES {}", "('é'),".repeat(STMT_MAX / 4));
        h.record(run("t", &long, true));
        let (rows, _) = h.page("t", None, None, 1).await.expect("page");
        assert!(rows[0].cut);
        assert!(rows[0].stmt.len() <= STMT_MAX);
        assert!(long.starts_with(&rows[0].stmt));
        assert_eq!(rows[0].to_json()["cut"], json!(true));
        h.close().await.expect("close");
    }

    #[tokio::test]
    async fn clear_forgets_one_database_and_survives_reopen() {
        let dir = scratch("clear");
        let h = DataHistory::new(dir.clone());
        h.open().await.expect("open");
        h.record(run("a", "SELECT 1", true));
        h.record(run("a", "SELECT 2", true));
        h.record(run("b", "SELECT 3", true));
        assert_eq!(h.clear("a").await.expect("clear"), 2);
        assert!(h.page("a", None, None, 10).await.expect("a").0.is_empty());
        h.close().await.expect("close");
        let again = DataHistory::new(dir);
        again.open().await.expect("reopen");
        assert_eq!(again.page("b", None, None, 10).await.expect("b").0.len(), 1, "kept on disk");
        again.close().await.expect("close");
    }

    #[tokio::test]
    async fn the_maintenance_pass_keeps_a_table_inside_the_row_bound() {
        let h = DataHistory::new(scratch("maintain"));
        h.open().await.expect("open");
        h.record(run("t", "SELECT 1", true));
        h.record(run("t", "SELECT 2", true));
        h.maintain().await;
        let (rows, _) = h.page("t", None, None, 10).await.expect("page");
        assert_eq!(rows.len(), 2, "a table inside the row bound survives the pass");
        h.close().await.expect("close");
    }

    /// The reported wedge, reproduced and answered: a store enrolled over a share
    /// smaller than its file denies every write - the per-write trim rides ADMITTED
    /// writes, so a denied one can never start the reclaim - and the maintenance pass
    /// is the only thing that buys the bytes back and re-opens admission.
    #[tokio::test]
    async fn a_store_over_its_share_denies_then_resumes_after_the_maintenance_pass() {
        let h = DataHistory::new(scratch("wedge"));
        h.open().await.expect("open");
        let fat = format!("SELECT {}", "x".repeat(STMT_MAX - 10));
        h.record(run("t", &fat, true));
        h.record(run("t", &fat, true));
        // The operator's clear: the rows go but their pages stay (nothing vacuums
        // yet), and the fresh enrollment measures that file - the shape of a restart
        // over a store that filled its share.
        h.clear("t").await.expect("clear");
        // The share sits between the reclaimed floor (the empty schema, plus the
        // 32 KB -shm sidecar an open WAL connection always carries) and the fat file.
        let shares = std::collections::BTreeMap::from([("data".to_string(), 96 * 1024u64)]);
        h.set_quota(swiss_host::history::quota::QuotaPolicy::from_shares(shares));
        h.record(run("t", "SELECT denied", true));
        let (rows, _) = h.page("t", None, None, 10).await.expect("page");
        assert!(rows.is_empty(), "the over-share write was refused");
        h.maintain().await;
        let (rows, _) = h.page("t", None, None, 10).await.expect("page");
        assert!(rows.is_empty(), "nothing landed during the pass itself");
        h.record(run("t", "SELECT back", true));
        let (rows, _) = h.page("t", None, None, 10).await.expect("page");
        assert_eq!(rows.len(), 1, "admission resumed once the pass reclaimed and re-priced");
        h.close().await.expect("close");
    }

    #[tokio::test]
    async fn a_closed_store_refuses_reads_and_swallows_writes() {
        let h = DataHistory::new(scratch("closed"));
        h.record(run("t", "SELECT 1", true));
        assert!(h.page("t", None, None, 10).await.is_err(), "a closed store answers its state, never a silent empty page");
    }

    #[test]
    fn a_reply_becomes_a_run() {
        let ok = NewRun::of("t".into(), "c", "postgres", "SELECT 1", &Ok(json!({"rowCount": 4})), 9);
        assert_eq!((ok.ok, ok.rows, ok.error.clone()), (true, Some(4), None));
        let bad = NewRun::of("t".into(), "c", "postgres", "SELEC 1", &Err("syntax error".into()), 2);
        assert_eq!((bad.ok, bad.rows, bad.error.as_deref()), (false, None, Some("syntax error")));
    }
    #[tokio::test]
    async fn observed_agent_attempts_are_durable_and_keep_repeated_executions() {
        use swiss_host::dbbrowser::execution::{submitted, SqlHistorySeat};
        let dir = scratch("observed");
        let h = DataHistory::new(dir.clone());
        h.open().await.expect("open");
        let seat = SqlHistorySeat::default();
        seat.install(h.clone());
        for _ in 0..2 {
            let _ = seat.observe("c", "db @ host".into(), "pg", async {
                submitted("SELECT 1 LIMIT 50");
                Ok::<_, String>(json!({ "rowCount": 1 }))
            }, Clone::clone).await;
        }
        let _ = seat.observe("c", "db @ host".into(), "pg", async {
            submitted("SELECT missing LIMIT 50");
            Err::<Value, _>("missing relation".to_string())
        }, Clone::clone).await;
        let _ = seat.observe("c", "db @ host".into(), "pg", async {
            Err::<Value, _>("policy refused before submission".to_string())
        }, Clone::clone).await;
        seat.clear();
        h.close().await.expect("close");
        let reopened = DataHistory::new(dir);
        reopened.open().await.expect("reopen");
        let (rows, more) = reopened.page("db @ host", None, None, 10).await.expect("history");
        assert!(!more);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|r| r.stmt == "SELECT 1 LIMIT 50").count(), 2);
        assert!(!rows[0].ok);
        assert_eq!(rows[0].error.as_deref(), Some("missing relation"));
        assert!(reopened.page("other db", None, None, 10).await.expect("other").0.is_empty());
        reopened.close().await.expect("close");
    }

}
