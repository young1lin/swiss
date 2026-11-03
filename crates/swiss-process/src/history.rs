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

//! The process history (SPEC §process.apps, §host.history): one `logs/process.db`
//! row per outcome - `{ts, app, action, by, ok, message, pid?, image?, exitCode?,
//! forced?}`. The jsonl era's wire shape survives whole; what changed is the store
//! underneath - the owner-scoped pool, the bounded writer queue and the generation
//! the host provides, the same seat mcp.db, remote.db, terminal.db and llm.db ride.
//! Output is never copied into it; output stays in the capture file.
//!
//! The legacy tree (`logs/process-history.jsonl` and its `.1` generation) is an
//! IMPORT SOURCE, inventoried and left in place - an operator retires it explicitly
//! after the cutover is verified; the importer never deletes anything it has not
//! proven redundant (ADR-033's contract, the same as mcp's and terminal's).
//!
//! Retention moves from the 1 MiB rotation to a row bound: the newest
//! [KEEP_ROWS] rows survive, swept along the append path every [TRIM_EVERY] admits.
//! A write that is refused or a barrier that fails warns ONCE per open and never
//! fails the action it records: the history is a record, not a gate.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use swiss_host::history::factory::HistoryStoreFactory;
use swiss_host::history::schema::SchemaStep;
use swiss_host::history::store::{Durability, ModuleHistoryStore, OwnerPolicy};

/// One outcome, as it lands in the store and as the routes read it back.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Unix milliseconds.
    pub ts: u64,
    pub app: String,
    /// start | stop | restart | exit.
    pub action: String,
    /// panel | cli | boot | plugin - attribution, not authorization.
    pub by: String,
    pub ok: bool,
    pub message: String,
    pub pid: Option<u32>,
    pub image: Option<String>,
    pub exit_code: Option<i32>,
    pub forced: Option<bool>,
}

impl HistoryEntry {
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "ts": self.ts,
            "app": self.app,
            "action": self.action,
            "by": self.by,
            "ok": self.ok,
            "message": self.message,
        });
        let obj = value.as_object_mut().expect("the literal above is an object");
        if let Some(pid) = self.pid {
            obj.insert("pid".into(), serde_json::json!(pid));
        }
        if let Some(image) = &self.image {
            obj.insert("image".into(), serde_json::json!(image));
        }
        if let Some(exit_code) = self.exit_code {
            obj.insert("exitCode".into(), serde_json::json!(exit_code));
        }
        if let Some(forced) = self.forced {
            obj.insert("forced".into(), serde_json::json!(forced));
        }
        value
    }

    /// The import's reader: a legacy jsonl line back into an entry. Lines that are
    /// not one (a torn tail, a hand edit) are skipped by the caller, never fatal.
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(HistoryEntry {
            ts: value.get("ts")?.as_u64()?,
            app: value.get("app")?.as_str()?.to_string(),
            action: value.get("action")?.as_str()?.to_string(),
            by: value.get("by")?.as_str().unwrap_or("panel").to_string(),
            ok: value.get("ok")?.as_bool()?,
            message: value.get("message")?.as_str().unwrap_or_default().to_string(),
            pid: value.get("pid").and_then(serde_json::Value::as_u64).and_then(|p| u32::try_from(p).ok()),
            image: value.get("image").and_then(serde_json::Value::as_str).map(str::to_string),
            exit_code: value
                .get("exitCode")
                .and_then(serde_json::Value::as_i64)
                .and_then(|c| i32::try_from(c).ok()),
            forced: value.get("forced").and_then(serde_json::Value::as_bool),
        })
    }
}

/// meta key holding the legacy import's state ("done" after the fold).
const IMPORT_KEY: &str = "process-history-import";

/// The row bound that replaces the 1 MiB rotation: the newest rows survive, the
/// rest go. 5,000 rows of a busy gateway is months of outcomes in under a MiB.
pub const KEEP_ROWS: i64 = 5_000;
/// The retention sweep rides every TRIM_EVERY admits, not every write.
const TRIM_EVERY: u64 = 64;
/// The pid column's "absent" value: NOT NULL keeps the import's natural key whole
/// (a UNIQUE over a nullable column treats NULLs as distinct and never collides).
const NO_PID: i64 = -1;

static PROCESS_SCHEMA: &[SchemaStep] = &[SchemaStep::new(
    1,
    &[
        // One row per outcome: the jsonl line's exact fields plus the internal id the
        // page orders by. The UNIQUE tail is the import's idempotence - the jsonl era
        // carried no ids, and two rows identical on every field are one outcome
        // recorded twice (INSERT OR IGNORE everywhere, including the live path).
        // IF NOT EXISTS for the same reason terminal's schema carries it: the home is
        // shared, and two first opens can interleave before either stamp lands - the
        // stamp still gates every SEQUENTIAL migration; this only rescues the
        // concurrent first boot.
        "CREATE TABLE IF NOT EXISTS history (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, \
         ts INTEGER NOT NULL, \
         app TEXT NOT NULL, \
         action TEXT NOT NULL, \
         by TEXT NOT NULL, \
         ok INTEGER NOT NULL, \
         message TEXT NOT NULL, \
         pid INTEGER NOT NULL DEFAULT -1, \
         image TEXT, \
         exit_code INTEGER, \
         forced INTEGER, \
         UNIQUE (ts, app, action, message, pid))",
        "CREATE INDEX IF NOT EXISTS history_page ON history (id DESC)",
        "CREATE INDEX IF NOT EXISTS history_app ON history (app, id DESC)",
    ],
)];

/// The one writer and reader of the process history store. Cheap to construct in
/// the closed state (no IO - the seam's own invariant); opening is the enabled
/// plugin's job, closing its stop's.
pub struct ProcessHistory {
    store: Arc<ModuleHistoryStore>,
    /// The logs directory: process.db lives here, the legacy jsonl tree is imported
    /// from here and left in place.
    logs_dir: PathBuf,
    /// Admits since open: the retention sweep rides every TRIM_EVERYth.
    appends: AtomicU64,
    /// Warn once per open, whatever went wrong (the jsonl era's rule, kept whole).
    warned: AtomicBool,
}

impl ProcessHistory {
    /// Construct the closed store. No IO, no file: a disabled process plugin opens
    /// nothing at all, and the seam answers every write and read of a closed store
    /// with an honest error instead of a side-effecting open.
    pub fn new(logs_dir: PathBuf) -> Arc<Self> {
        let policy = OwnerPolicy {
            db_file: logs_dir.join("process.db"),
            queue_byte_bound: 8 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(10),
            durability: Durability::Normal,
            schema: PROCESS_SCHEMA,
        };
        let store = HistoryStoreFactory::new().create("process", policy);
        Arc::new(ProcessHistory {
            store,
            logs_dir,
            appends: AtomicU64::new(0),
            warned: AtomicBool::new(false),
        })
    }

    /// The file this store opens. The Process plugin publishes it on its own seat.
    pub fn database_file(&self) -> PathBuf {
        self.store.policy().db_file.clone()
    }

    /// Open (and if needed import into) `<logs>/process.db`. The caller is the
    /// plugin's start - never a constructor - so a disabled process opens nothing.
    pub async fn open(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.logs_dir)
            .map_err(|e| format!("creating the logs directory: {e}"))?;
        self.store
            .open()
            .await
            .map_err(|e| format!("opening the process history database: {e}"))?;
        // The declared default share, no protected window: outcomes are convenience
        // history, and the honest over-share denial is the plain reason.
        self.store
            .set_quota(swiss_host::history::quota::QuotaPolicy::default(), false);
        self.import_legacy()
            .await
            .map_err(|e| format!("importing the legacy process history: {e}"))?;
        Ok(())
    }

    /// Close bounded (SPEC §host.history): withdraw, drain, release. The caller is
    /// the plugin's stop; an unresolved close is its warning, never a swallow.
    pub async fn close(&self) -> Result<(), swiss_host::history::store::OwnerLifecycleError> {
        self.store.close().await
    }

    /// Append one outcome. Synchronous bounded admission: the queue either takes the
    /// insert now or refuses it; a refusal warns once and still returns Ok - a
    /// history write never fails the action it records (SPEC §process.apps). The
    /// retention sweep rides the same job every TRIM_EVERY admits.
    pub fn append(&self, entry: &HistoryEntry) -> Result<(), String> {
        let trim = self.appends.fetch_add(1, Ordering::Relaxed).is_multiple_of(TRIM_EVERY);
        let (ts, app, action, by, ok, message, pid, image, exit_code, forced) = (
            entry.ts as i64,
            entry.app.clone(),
            entry.action.clone(),
            entry.by.clone(),
            entry.ok as i64,
            entry.message.clone(),
            entry.pid.map(|p| p as i64).unwrap_or(NO_PID),
            entry.image.clone(),
            entry.exit_code,
            entry.forced.map(|f| f as i64),
        );
        let estimated = message.len() + app.len() + image.as_ref().map(|s| s.len()).unwrap_or(0) + 160;
        let submitted = self.store.submit(estimated, move |pool| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT OR IGNORE INTO history \
                     (ts, app, action, by, ok, message, pid, image, exit_code, forced) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(ts)
                .bind(&app)
                .bind(&action)
                .bind(&by)
                .bind(ok)
                .bind(&message)
                .bind(pid)
                .bind(&image)
                .bind(exit_code)
                .bind(forced)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                if trim {
                    sqlx::query("DELETE FROM history WHERE id NOT IN \
                                 (SELECT id FROM history ORDER BY id DESC LIMIT ?)")
                        .bind(KEEP_ROWS)
                        .execute(&pool)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        });
        if let Err(err) = submitted {
            self.warn_once(format!("the process history write was refused: {err}"));
        }
        Ok(())
    }

    /// Read the newest `limit` entries, newest LAST, optionally one app's. The
    /// jsonl era's visibility rule kept whole: every admitted write has landed
    /// before the read looks, so a recorded outcome is visible to the very next
    /// read (the barrier the seam provides).
    pub async fn read(&self, app: Option<&str>, limit: usize) -> Vec<HistoryEntry> {
        if let Err(err) = self.store.flush_barrier().await {
            self.warn_once(format!("the process history barrier: {err}"));
        }
        let limit = limit.clamp(1, 10_000) as i64;
        // The query closure is 'static (the seam's contract): own the filter,
        // never borrow the caller's str.
        let app = app.map(str::to_string);
        let rows = match app {
            Some(app) => self
                .store
                .query(move |pool| {
                    Box::pin(async move {
                        sqlx::query_as::<_, HistoryRow>("
                            SELECT id, ts, app, action, by, ok, message, pid, image, exit_code, forced \
                            FROM history WHERE app = ? ORDER BY id DESC LIMIT ?"
                        )
                        .bind(app)
                        .bind(limit)
                        .fetch_all(&pool)
                        .await
                    })
                })
                .await,
            None => self
                .store
                .query(|pool| {
                    Box::pin(async move {
                        sqlx::query_as::<_, HistoryRow>("
                            SELECT id, ts, app, action, by, ok, message, pid, image, exit_code, forced \
                            FROM history ORDER BY id DESC LIMIT ?"
                        )
                        .bind(limit)
                        .fetch_all(&pool)
                        .await
                    })
                })
                .await,
        };
        match rows {
            Ok(mut rows) => {
                rows.reverse();
                rows.into_iter().map(HistoryRow::into_entry).collect()
            }
            Err(err) => {
                self.warn_once(format!("the process history read failed: {err}"));
                Vec::new()
            }
        }
    }

    /// Fold the legacy jsonl tree in, once. Idempotence is the UNIQUE tail plus the
    /// meta stamp: a crash mid-fold re-runs the fold, INSERT OR IGNORE keeps every
    /// survived row single, and the tree itself is never touched beyond a read.
    async fn import_legacy(&self) -> Result<(), String> {
        let done: Option<String> = self
            .store
            .query(|pool| {
                Box::pin(async move {
                    sqlx::query_scalar::<_, String>("SELECT value FROM meta WHERE key = ?")
                        .bind(IMPORT_KEY)
                        .fetch_optional(&pool)
                        .await
                })
            })
            .await
            .map_err(|e| e.to_string())?;
        if done.as_deref() == Some("done") {
            return Ok(());
        }
        // Oldest first: the .1 generation predates the current file (the jsonl
        // rotation's order, kept so ids order like time did).
        let mut entries = Vec::new();
        for name in ["process-history.jsonl.1", "process-history.jsonl"] {
            let path = self.logs_dir.join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(entry) = HistoryEntry::from_json(&value) {
                        entries.push((
                            entry.ts as i64,
                            entry.app,
                            entry.action,
                            entry.by,
                            entry.ok as i64,
                            entry.message,
                            entry.pid.map(|p| p as i64).unwrap_or(NO_PID),
                            entry.image,
                            entry.exit_code,
                            entry.forced.map(|f| f as i64),
                        ));
                    }
                }
            }
        }
        let estimated = entries.len() * 160 + 256;
        self.store
            .submit(estimated, move |pool| {
                Box::pin(async move {
                    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                    for row in &entries {
                        let (ts, app, action, by, ok, message, pid, image, exit_code, forced) = row;
                        sqlx::query(
                            "INSERT OR IGNORE INTO history \
                             (ts, app, action, by, ok, message, pid, image, exit_code, forced) \
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        )
                        .bind(ts)
                        .bind(app)
                        .bind(action)
                        .bind(by)
                        .bind(ok)
                        .bind(message)
                        .bind(pid)
                        .bind(image)
                        .bind(exit_code)
                        .bind(forced)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                    sqlx::query("INSERT INTO meta(key, value) VALUES (?, 'done') \
                                 ON CONFLICT(key) DO UPDATE SET value = excluded.value")
                        .bind(IMPORT_KEY)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    tx.commit().await.map_err(|e| e.to_string())?;
                    Ok(())
                })
            })
            .map_err(|e| e.to_string())?;
        // open() returns with the fold landed: the first read sees the history it
        // imported, not a promise of one.
        self.store.flush_barrier().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Warn once per open, whatever went wrong: the operator sees it in the
    /// gateway log, the action never sees it at all.
    fn warn_once(&self, message: String) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            swiss_core::log::warn(&message, None);
        }
    }
}

/// One stored row, as it comes off the query. `sqlx::FromRow` by hand: the pid's
/// NO_PID mapping and the i64->bool folds live in one place, `into_entry`.
struct HistoryRow {
    ts: i64,
    app: String,
    action: String,
    by: String,
    ok: i64,
    message: String,
    pid: i64,
    image: Option<String>,
    exit_code: Option<i64>,
    forced: Option<i64>,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for HistoryRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row as _;
        Ok(HistoryRow {
            ts: row.try_get("ts")?,
            app: row.try_get("app")?,
            action: row.try_get("action")?,
            by: row.try_get("by")?,
            ok: row.try_get("ok")?,
            message: row.try_get("message")?,
            pid: row.try_get("pid")?,
            image: row.try_get("image")?,
            exit_code: row.try_get("exit_code")?,
            forced: row.try_get("forced")?,
        })
    }
}

impl HistoryRow {
    fn into_entry(self) -> HistoryEntry {
        HistoryEntry {
            ts: self.ts.max(0) as u64,
            app: self.app,
            action: self.action,
            by: self.by,
            ok: self.ok != 0,
            message: self.message,
            pid: (self.pid != NO_PID).then(|| self.pid.max(0) as u32),
            image: self.image,
            exit_code: self.exit_code.and_then(|c| i32::try_from(c).ok()),
            forced: self.forced.map(|f| f != 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo's scratch-home pattern: a unique subtree under SWISS_HOME (pinned by
    /// test-utils), so a test database never touches a real home.
    fn scratch(tag: &str) -> PathBuf {
        let dir = swiss_core::paths::test_home().join("process-history").join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn entry(ts: u64, app: &str, action: &str, ok: bool) -> HistoryEntry {
        HistoryEntry {
            ts,
            app: app.to_string(),
            action: action.to_string(),
            by: "panel".to_string(),
            ok,
            message: format!("{action} {ok}"),
            pid: Some(ts as u32),
            image: None,
            exit_code: None,
            forced: None,
        }
    }

    #[tokio::test]
    async fn appends_land_newest_last_and_filter_by_app() {
        let dir = scratch("append");
        let history = ProcessHistory::new(dir);
        history.open().await.expect("open");
        history.append(&entry(1, "cc-otel", "start", true)).expect("append");
        history.append(&entry(2, "svc", "start", true)).expect("append");
        history.append(&entry(3, "cc-otel", "stop", false)).expect("append");
        let all = history.read(None, 10).await;
        assert_eq!(
            all.iter().map(|e| e.ts).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "newest LAST, the jsonl era's order"
        );
        let otel = history.read(Some("cc-otel"), 10).await;
        assert_eq!(otel.len(), 2);
        assert_eq!(otel[1].action, "stop");
        assert_eq!(otel[1].pid, Some(3), "the pid column survives its NO_PID encoding");
        let newest = history.read(None, 1).await;
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].ts, 3);
        history.close().await.expect("close");
    }

    #[tokio::test]
    async fn the_legacy_jsonl_imports_once_and_survives_reopen() {
        let dir = scratch("import");
        std::fs::write(
            dir.join("process-history.jsonl"),
            concat!(
                "{\"ts\":1,\"app\":\"old\",\"action\":\"start\",\"by\":\"boot\",\"ok\":true,\"message\":\"started\",\"pid\":11}\n",
                "{\"ts\":2,\"app\":\"old\",\"action\":\"stop\",\"by\":\"panel\",\"ok\":true,\"message\":\"stopped\"}\n",
                "not json at all\n",
            ),
        )
        .expect("write legacy");
        std::fs::write(
            dir.join("process-history.jsonl.1"),
            "{\"ts\":0,\"app\":\"old\",\"action\":\"exit\",\"by\":\"plugin\",\"ok\":false,\"message\":\"exit 1\",\"pid\":9,\"exitCode\":1,\"forced\":true}\n",
        )
        .expect("write generation");
        let history = ProcessHistory::new(dir.clone());
        history.open().await.expect("open imports");
        let rows = history.read(None, 10).await;
        assert_eq!(rows.len(), 3, "the torn line skipped, both generations folded: {rows:?}");
        assert_eq!(rows[0].ts, 0, "the .1 generation is the oldest");
        assert_eq!(rows[0].exit_code, Some(1));
        assert_eq!(rows[0].forced, Some(true));
        assert_eq!(rows[2].pid, None, "an absent pid reads back absent");
        history.close().await.expect("close");
        // Reopen: the fold never runs twice, the tree stays in place.
        let again = ProcessHistory::new(dir);
        again.open().await.expect("reopen");
        assert_eq!(again.read(None, 10).await.len(), 3, "no duplicates after reopen");
        again.append(&entry(9, "old", "start", true)).expect("append");
        assert_eq!(again.read(None, 10).await.len(), 4);
        again.close().await.expect("close");
    }

    #[tokio::test]
    async fn the_row_bound_keeps_the_newest_rows() {
        let dir = scratch("bound");
        let history = ProcessHistory::new(dir);
        history.open().await.expect("open");
        let total = KEEP_ROWS as u64 + TRIM_EVERY + 8;
        for ts in 0..total {
            history.append(&entry(ts, "svc", "start", true)).expect("append");
        }
        let rows = history.read(None, usize::MAX).await;
        // The sweep is periodic (every TRIM_EVERY admits), so the exact count
        // rides where the last sweep landed - but the survivors are exactly the
        // NEWEST, contiguous, and never fewer than the bound nor more than one
        // sweep window over it.
        assert!(rows.len() >= KEEP_ROWS as usize, "never under the bound: {}", rows.len());
        assert!(
            rows.len() <= KEEP_ROWS as usize + TRIM_EVERY as usize,
            "never more than a sweep window over: {}",
            rows.len()
        );
        let last = rows.last().unwrap().ts;
        assert_eq!(last, total - 1, "the newest survived");
        assert_eq!(rows.first().unwrap().ts, last - rows.len() as u64 + 1, "contiguous from the newest");
        history.close().await.expect("close");
    }
}
