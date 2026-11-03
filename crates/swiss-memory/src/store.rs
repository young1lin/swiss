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

//! The SQLite+FTS5 store (SPEC §memory.store, ADR-031): one database under the Swiss
//! home, WAL, a 5 s busy timeout, versioned migrations and BEGIN IMMEDIATE
//! mutations - the transaction shape the Phase 0 spike paid for (a deferred
//! read-then-upgrade returns SQLITE_BUSY on a stale snapshot no timeout can retry).
//!
//! Pool: exactly one connection, acquired lazily. The busy traffic is OTHER
//! PROCESSES (a CLI run beside the gateway), which WAL plus the timeout absorb.
//! Every row function here takes the caller's connection, so one service operation
//! acquires once and runs in one transaction - a second acquire inside an operation
//! would wait on itself.
//!
//! The FTS5 index is derived (external content, trigger-maintained) and therefore
//! rebuildable from the rows at any time; nothing but the rows is authoritative.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Row, Sqlite};

use crate::error::{Code, MemError, MemResult};
use crate::model::{
    legacy_provenance, Anchor, Domain, DomainRevision, DomainSnapshot, Evidence, Kind, Link, LinkKind, Memory, Origin, Producer,
    Project, Relation, Revision, Root, RootKind, Snapshot, SourceType, Status, Transport,
};
use crate::scope::GLOBAL_PROJECT_ID;
use crate::sync::{Run, RunState, Trigger};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = 11;

/// One pooled connection.
pub type Conn = PoolConnection<Sqlite>;

/// Mark a dynamically built SQL string as audited (sqlx 0.9's SqlSafeStr gate).
/// Every fragment this store interpolates is a closed-vocabulary `as_str()` value or
/// a clamped integer - user text reaches the database only through bind parameters.
fn audited(sql: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql)
}

/// The v2 schema, one statement per entry so a failure names its own object.
const SCHEMA_V2: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        created_ms INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS project_roots (
        identity TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        root_path TEXT NOT NULL,
        origin_url TEXT,
        added_ms INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_roots_project ON project_roots(project_id)",
    "CREATE INDEX IF NOT EXISTS idx_roots_origin ON project_roots(origin_url)",
    "CREATE TABLE IF NOT EXISTS memories (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        title_norm TEXT NOT NULL,
        content TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        status TEXT NOT NULL,
        origin TEXT NOT NULL,
        actor TEXT NOT NULL,
        confirmed INTEGER NOT NULL DEFAULT 0,
        superseded_by TEXT,
        revision INTEGER NOT NULL,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL,
        verified_ms INTEGER
    )",
    "CREATE INDEX IF NOT EXISTS idx_mem_scope ON memories(project_id, status, kind)",
    "CREATE INDEX IF NOT EXISTS idx_mem_title ON memories(project_id, title_norm)",
    "CREATE INDEX IF NOT EXISTS idx_mem_hash ON memories(project_id, content_hash)",
    "CREATE TABLE IF NOT EXISTS memory_anchors (
        memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        idx INTEGER NOT NULL,
        path TEXT NOT NULL,
        is_glob INTEGER NOT NULL,
        hash TEXT,
        PRIMARY KEY (memory_id, idx)
    )",
    "CREATE TABLE IF NOT EXISTS memory_refs (
        memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        idx INTEGER NOT NULL,
        ref TEXT NOT NULL,
        PRIMARY KEY (memory_id, idx)
    )",
    "CREATE TABLE IF NOT EXISTS memory_links (
        from_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        to_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        note TEXT,
        actor TEXT NOT NULL,
        created_ms INTEGER NOT NULL,
        PRIMARY KEY (from_id, to_id, kind)
    )",
    "CREATE INDEX IF NOT EXISTS idx_links_to ON memory_links(to_id)",
    "CREATE TABLE IF NOT EXISTS memory_revisions (
        memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        revision INTEGER NOT NULL,
        op TEXT NOT NULL,
        actor TEXT NOT NULL,
        reason TEXT,
        at_ms INTEGER NOT NULL,
        snapshot_json TEXT NOT NULL,
        PRIMARY KEY (memory_id, revision)
    )",
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
        title, content, content='memories', content_rowid='rowid',
        tokenize='unicode61 remove_diacritics 2'
    )",
    "CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memory_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
    END",
    "CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, title, content)
        VALUES ('delete', old.rowid, old.title, old.content);
    END",
    "CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE OF title, content ON memories BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, title, content)
        VALUES ('delete', old.rowid, old.title, old.content);
        INSERT INTO memory_fts(rowid, title, content) VALUES (new.rowid, new.title, new.content);
    END",
];

/// v3: the embedding index (SPEC §memory.embedding). Derived like the FTS index -
/// one vector per note, dropped with it, rebuildable from the rows by re-embedding.
/// `model` is the provider fingerprint, `vec` little-endian f32 L2-normalized, and
/// `text_hash` the hash of the `title + "\n" + content` it was made from. One
/// fingerprint holds one width: the service drops the others ([drop_other_widths]).
const SCHEMA_V3: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_vectors (
        memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
        model TEXT NOT NULL,
        dims INTEGER NOT NULL,
        text_hash TEXT NOT NULL,
        vec BLOB NOT NULL,
        at_ms INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_vectors_model ON memory_vectors(model)",
];

/// v4: the project map (SPEC §memory.domains). A note's `domain_id` is added beside these
/// (see [Store::migrate]): deleting a domain re-files its notes in the service, and the
/// column's ON DELETE SET NULL is only the backstop.
const SCHEMA_V4: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_domains (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        title TEXT NOT NULL,
        summary TEXT NOT NULL DEFAULT '',
        body TEXT NOT NULL DEFAULT '',
        owns_json TEXT NOT NULL DEFAULT '[]',
        origin TEXT NOT NULL,
        actor TEXT NOT NULL,
        confirmed INTEGER NOT NULL DEFAULT 0,
        revision INTEGER NOT NULL,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL,
        verified_ms INTEGER NOT NULL,
        UNIQUE (project_id, path)
    )",
    "CREATE TABLE IF NOT EXISTS memory_domain_revisions (
        domain_id TEXT NOT NULL REFERENCES memory_domains(id) ON DELETE CASCADE,
        revision INTEGER NOT NULL,
        op TEXT NOT NULL,
        actor TEXT NOT NULL,
        reason TEXT,
        at_ms INTEGER NOT NULL,
        snapshot_json TEXT NOT NULL,
        PRIMARY KEY (domain_id, revision)
    )",
];

/// v5: sync's bookkeeping (SPEC §memory.sync) - where each project's last sync left off and
/// its runs. Not knowledge: never exported, gone with the project.
const SCHEMA_V5: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_sync_state (
        project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
        head TEXT NOT NULL,
        updated_ms INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS memory_sync_commits (
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        sha TEXT NOT NULL,
        at_ms INTEGER NOT NULL,
        PRIMARY KEY (project_id, sha)
    )",
    "CREATE TABLE IF NOT EXISTS memory_sync_cursors (
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        file TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (project_id, file)
    )",
    "CREATE TABLE IF NOT EXISTS memory_sync_runs (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        cause TEXT NOT NULL,
        dry_run INTEGER NOT NULL,
        state TEXT NOT NULL,
        queued_ms INTEGER NOT NULL,
        due_ms INTEGER NOT NULL,
        started_ms INTEGER,
        finished_ms INTEGER,
        since TEXT,
        base TEXT,
        head TEXT,
        scope_json TEXT NOT NULL DEFAULT '{}',
        ops_json TEXT NOT NULL DEFAULT '[]',
        runner_json TEXT NOT NULL DEFAULT '{}',
        proposal_json TEXT,
        summary TEXT NOT NULL DEFAULT '',
        error TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_sync_runs_project ON memory_sync_runs(project_id, queued_ms)",
    "CREATE INDEX IF NOT EXISTS idx_sync_runs_state ON memory_sync_runs(state, due_ms)",
];

/// v6: evidence (ADR-032). Each row is bound to the note's text when it was attached
/// (`revision`, `note_hash`); the note's `producer` and `transport` columns are added beside
/// these (see [Store::migrate]).
const SCHEMA_V6: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_evidence (
        id TEXT PRIMARY KEY,
        memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        revision INTEGER NOT NULL,
        note_hash TEXT NOT NULL,
        source_type TEXT NOT NULL,
        relation TEXT NOT NULL,
        path TEXT,
        symbol TEXT,
        commit_sha TEXT,
        session_id TEXT,
        run_id TEXT,
        source_hash TEXT,
        excerpt_hash TEXT,
        actor TEXT NOT NULL,
        created_ms INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_evidence_memory ON memory_evidence(memory_id, revision)",
];

/// v7: search keywords (SPEC §memory.model). The notes and the domains gain a `keywords`
/// column (one keyword per line; see [Store::migrate]) and the FTS index a third column read
/// from it - the index is derived, so it is dropped and rebuilt from the rows, once.
const FTS_V7: &[&str] = &[
    "DROP TRIGGER IF EXISTS memories_ai",
    "DROP TRIGGER IF EXISTS memories_ad",
    "DROP TRIGGER IF EXISTS memories_au",
    "DROP TABLE IF EXISTS memory_fts",
    "CREATE VIRTUAL TABLE memory_fts USING fts5(
        title, content, keywords, content='memories', content_rowid='rowid',
        tokenize='unicode61 remove_diacritics 2'
    )",
    "CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memory_fts(rowid, title, content, keywords) VALUES (new.rowid, new.title, new.content, new.keywords);
    END",
    "CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, title, content, keywords)
        VALUES ('delete', old.rowid, old.title, old.content, old.keywords);
    END",
    "CREATE TRIGGER memories_au AFTER UPDATE OF title, content, keywords ON memories BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, title, content, keywords)
        VALUES ('delete', old.rowid, old.title, old.content, old.keywords);
        INSERT INTO memory_fts(rowid, title, content, keywords) VALUES (new.rowid, new.title, new.content, new.keywords);
    END",
    "INSERT INTO memory_fts(memory_fts) VALUES ('rebuild')",
];

/// v8: a project's own settings (SPEC §memory.settings) - the agents its runs ask. Like
/// sync's rows, the machine's and not knowledge: never exported, gone with the project.
const SCHEMA_V8: &[&str] = &["CREATE TABLE IF NOT EXISTS memory_project_settings (
        project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
        settings_json TEXT NOT NULL,
        updated_ms INTEGER NOT NULL
    )"];

/// v9: the documents a project keeps (SPEC §memory.docs) - each whole, cut into sections,
/// the sections indexed. The index holds the sections' text segmented (CJK runs as
/// bigrams, [crate::docs::segment]), so the docs code writes it - no trigger does - and
/// deletes its rows before the sections go: nothing cascades into a virtual table. Like
/// the settings, re-read from the checkout and never exported.
const SCHEMA_V9: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_docs (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        source TEXT NOT NULL,
        title TEXT NOT NULL,
        hash TEXT NOT NULL,
        bytes INTEGER NOT NULL,
        cut INTEGER NOT NULL DEFAULT 0,
        kind TEXT,
        summary TEXT NOT NULL DEFAULT '',
        keywords TEXT NOT NULL DEFAULT '',
        symbols_json TEXT NOT NULL DEFAULT '[]',
        domain_id TEXT REFERENCES memory_domains(id) ON DELETE SET NULL,
        card_hash TEXT,
        card_by TEXT,
        read_ms INTEGER NOT NULL,
        carded_ms INTEGER,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL,
        UNIQUE (project_id, path)
    )",
    "CREATE INDEX IF NOT EXISTS idx_docs_domain ON memory_docs(domain_id)",
    // `id` is the index's rowid: an INTEGER PRIMARY KEY, so a VACUUM never renumbers it.
    "CREATE TABLE IF NOT EXISTS memory_doc_sections (
        id INTEGER PRIMARY KEY,
        doc_id TEXT NOT NULL REFERENCES memory_docs(id) ON DELETE CASCADE,
        ord INTEGER NOT NULL,
        heading TEXT NOT NULL,
        line INTEGER NOT NULL,
        text TEXT NOT NULL,
        UNIQUE (doc_id, ord)
    )",
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_doc_fts USING fts5(
        heading, text, keywords, tokenize = 'unicode61 remove_diacritics 2'
    )",
];

/// v11: the retrievals an agent or a person recorded as failed (SPEC §memory.evaluation
/// Misses) - each the query, what came back, what of it was wrong and what was wanted, the
/// note ids as JSON arrays. The evaluation's raw material, not knowledge: never in the
/// JSONL transfer, cascade-deleted with the project.
const SCHEMA_V11: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS memory_misses (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        query TEXT NOT NULL,
        via TEXT,
        style TEXT,
        shown_json TEXT NOT NULL DEFAULT '[]',
        wrong_json TEXT NOT NULL DEFAULT '[]',
        wanted_json TEXT NOT NULL DEFAULT '[]',
        want TEXT,
        paths_json TEXT NOT NULL DEFAULT '[]',
        actor TEXT NOT NULL,
        created_ms INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_misses_project ON memory_misses(project_id, created_ms)",
];

/// The v1 development schema's tables, dropped when an EMPTY v1 store is rebuilt.
const V1_TABLES: &[&str] = &[
    "DROP TRIGGER IF EXISTS memories_ai",
    "DROP TRIGGER IF EXISTS memories_ad",
    "DROP TRIGGER IF EXISTS memories_au",
    "DROP TABLE IF EXISTS memory_fts",
    "DROP TABLE IF EXISTS memory_links",
    "DROP TABLE IF EXISTS memory_sources",
    "DROP TABLE IF EXISTS memory_revisions",
    "DROP TABLE IF EXISTS memories",
    "DROP TABLE IF EXISTS project_roots",
    "DROP TABLE IF EXISTS projects",
    "DELETE FROM schema_migrations",
];

/// One store handle: the pool and the file it serves.
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
}

impl Store {
    /// Open (creating if absent) and migrate the database at `path`. The parent
    /// directory is created OS-private (SPEC §core.home).
    pub async fn open(path: &Path) -> MemResult<Store> {
        if let Some(parent) = path.parent() {
            swiss_core::platform::mkdir_private(parent);
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|err| MemError::store(format!("open {}: {err}", path.display())))?;
        let store = Store { pool, path: path.to_path_buf() };
        store.migrate().await?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn close(&self) {
        self.pool.close().await;
    }

    /// A connection for reads.
    pub async fn read(&self) -> MemResult<Conn> {
        Ok(self.pool.acquire().await?)
    }

    /// A connection inside BEGIN IMMEDIATE: end it with [finish].
    pub async fn write(&self) -> MemResult<Conn> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        Ok(conn)
    }

    async fn migrate(&self) -> MemResult<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_ms INTEGER NOT NULL)")
            .execute(&mut *conn)
            .await?;
        let applied: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM schema_migrations")
            .fetch_one(&mut *conn)
            .await?;
        if applied > SCHEMA_VERSION {
            return Err(MemError::new(
                Code::UnsupportedSchema,
                format!(
                    "the memory store {} carries schema version {applied}; this build reads version {SCHEMA_VERSION}. \
                     Upgrade swiss; the store is left untouched",
                    self.path.display()
                ),
            ));
        }
        let legacy: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'memory_sources'",
        )
        .fetch_one(&mut *conn)
        .await?;
        if applied == 1 || legacy > 0 {
            // A v1 development store. Empty: rebuild it in place. Holding notes: refuse,
            // never wipe them.
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memories").fetch_one(&mut *conn).await.unwrap_or(1);
            if rows > 0 {
                return Err(MemError::new(
                    Code::UnsupportedSchema,
                    format!(
                        "the memory store {} was written by a development build (schema 1) and holds {rows} notes; \
                         back it up and delete it, then retry",
                        self.path.display()
                    ),
                ));
            }
            for stmt in V1_TABLES.iter().copied() {
                sqlx::raw_sql(stmt).execute(&mut *conn).await?;
            }
        }
        for stmt in SCHEMA_V2
            .iter()
            .chain(SCHEMA_V3)
            .chain(SCHEMA_V4)
            .chain(SCHEMA_V5)
            .chain(SCHEMA_V6)
            .chain(SCHEMA_V8)
            .chain(SCHEMA_V9)
            .chain(SCHEMA_V11)
            .copied()
        {
            sqlx::raw_sql(stmt)
                .execute(&mut *conn)
                .await
                .map_err(|err| MemError::store(format!("migrate: {err}")))?;
        }
        // v4's column on the v2 table: SQLite has no ADD COLUMN IF NOT EXISTS.
        let has_domain: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name = 'domain_id'")
                .fetch_one(&mut *conn)
                .await?;
        if has_domain == 0 {
            sqlx::raw_sql("ALTER TABLE memories ADD COLUMN domain_id TEXT REFERENCES memory_domains(id) ON DELETE SET NULL")
                .execute(&mut *conn)
                .await
                .map_err(|err| MemError::store(format!("migrate: {err}")))?;
        }
        sqlx::raw_sql("CREATE INDEX IF NOT EXISTS idx_mem_domain ON memories(domain_id)")
            .execute(&mut *conn)
            .await
            .map_err(|err| MemError::store(format!("migrate: {err}")))?;
        // v10's column on the v9 table: the code names a card gave, looked up again as the
        // code index changes (SPEC §memory.docs The card). Null on a card written before:
        // the next docs run asks for it again.
        let has_names: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('memory_docs') WHERE name = 'names_json'")
                .fetch_one(&mut *conn)
                .await?;
        if has_names == 0 {
            sqlx::raw_sql("ALTER TABLE memory_docs ADD COLUMN names_json TEXT")
                .execute(&mut *conn)
                .await
                .map_err(|err| MemError::store(format!("migrate: {err}")))?;
        }
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let provenance = Store::add_provenance(&mut conn).await;
        finish(&mut conn, provenance).await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let keywords = Store::add_keywords(&mut conn).await;
        finish(&mut conn, keywords).await?;
        let now = now();
        sqlx::query("INSERT OR IGNORE INTO projects (id, name, created_ms) VALUES (?, 'Global', ?)")
            .bind(GLOBAL_PROJECT_ID)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO schema_migrations (version, applied_ms) VALUES (?, ?)")
            .bind(SCHEMA_VERSION)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// v7's columns and FTS index, in one transaction: a store whose index reads the
    /// keywords already is left alone.
    async fn add_keywords(conn: &mut SqliteConnection) -> MemResult<()> {
        for table in ["memories", "memory_domains"] {
            let sql = format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'keywords'");
            let has: i64 = sqlx::query_scalar(audited(sql)).fetch_one(&mut *conn).await?;
            if has == 0 {
                let alter = format!("ALTER TABLE {table} ADD COLUMN keywords TEXT NOT NULL DEFAULT ''");
                sqlx::raw_sql(audited(alter)).execute(&mut *conn).await.map_err(|err| MemError::store(format!("migrate: {err}")))?;
            }
        }
        let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('memory_fts') WHERE name = 'keywords'")
            .fetch_one(&mut *conn)
            .await?;
        if indexed > 0 {
            return Ok(());
        }
        for stmt in FTS_V7.iter().copied() {
            sqlx::raw_sql(stmt).execute(&mut *conn).await.map_err(|err| MemError::store(format!("migrate: {err}")))?;
        }
        Ok(())
    }

    /// v6's columns on the v2 table, in one transaction with their backfill: a note written before
/// gains the producer and transport its actor implies ([legacy_provenance]), once - a store
/// that has the columns is never derived again.
async fn add_provenance(conn: &mut SqliteConnection) -> MemResult<()> {
    let has: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name = 'producer'")
        .fetch_one(&mut *conn)
        .await?;
    if has > 0 {
        return Ok(());
    }
    for stmt in [
        "ALTER TABLE memories ADD COLUMN producer TEXT NOT NULL DEFAULT 'unknown'",
        "ALTER TABLE memories ADD COLUMN transport TEXT NOT NULL DEFAULT 'cli'",
    ] {
        sqlx::raw_sql(stmt).execute(&mut *conn).await.map_err(|err| MemError::store(format!("migrate: {err}")))?;
    }
    let pairs = sqlx::query("SELECT DISTINCT origin, actor FROM memories").fetch_all(&mut *conn).await?;
    for row in pairs {
        let origin: String = row.try_get("origin")?;
        let actor: String = row.try_get("actor")?;
        let parsed = Origin::parse(&origin).ok_or_else(|| MemError::store(format!("migrate: unknown origin {origin}")))?;
        let (producer, transport) = legacy_provenance(parsed, &actor);
        sqlx::query("UPDATE memories SET producer = ?, transport = ? WHERE origin = ? AND actor = ?")
            .bind(producer.as_str())
            .bind(transport.as_str())
            .bind(&origin)
            .bind(&actor)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// A consistent copy through `VACUUM INTO` (SPEC §memory.store): never "checkpoint
    /// then copy", which another process can write between. Written beside the target
    /// and renamed into place.
    pub async fn backup_into(&self, target: &Path) -> MemResult<()> {
        if let Some(parent) = target.parent() {
            swiss_core::platform::mkdir_private(parent);
        }
        let tmp = target.with_extension(format!("tmp-{}", swiss_core::util::random_hex(4)));
        let mut conn = self.pool.acquire().await?;
        sqlx::query("VACUUM INTO ?")
            .bind(tmp.display().to_string())
            .execute(&mut *conn)
            .await?;
        drop(conn);
        if target.exists() {
            std::fs::remove_file(target).map_err(|e| MemError::store(format!("{}: {e}", target.display())))?;
        }
        std::fs::rename(&tmp, target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            MemError::store(format!("{}: {e}", target.display()))
        })
    }
}

/// COMMIT on Ok, ROLLBACK on Err. A failed COMMIT would leave the transaction open on
/// the pool's single connection, so it is rolled back before reporting.
pub async fn finish<T>(conn: &mut SqliteConnection, outcome: MemResult<T>) -> MemResult<T> {
    match outcome {
        Ok(value) => {
            if let Err(err) = sqlx::query("COMMIT").execute(&mut *conn).await {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(err.into());
            }
            Ok(value)
        }
        Err(err) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            Err(err)
        }
    }
}

pub fn now() -> i64 {
    swiss_core::util::now_ms() as i64
}

/// An SQL IN fragment over closed-vocabulary values, prefixed with AND - empty means
/// no filter.
fn in_clause(column: &str, values: &[&'static str]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let list = values.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", ");
    format!(" AND {column} IN ({list})")
}

fn status_clause(column: &str, statuses: &[Status]) -> String {
    in_clause(column, &statuses.iter().map(Status::as_str).collect::<Vec<_>>())
}

// --- projects ---------------------------------------------------------------------------

pub async fn projects(conn: &mut SqliteConnection) -> MemResult<Vec<Project>> {
    let rows = sqlx::query("SELECT id, name, created_ms FROM projects ORDER BY name, id").fetch_all(&mut *conn).await?;
    let mut out: Vec<Project> = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(Project {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            created_ms: row.try_get("created_ms")?,
            roots: Vec::new(),
        });
    }
    let roots = sqlx::query(
        "SELECT identity, project_id, kind, root_path, origin_url, added_ms FROM project_roots ORDER BY added_ms, identity",
    )
    .fetch_all(&mut *conn)
    .await?;
    for row in roots {
        let owner: String = row.try_get("project_id")?;
        if let Some(project) = out.iter_mut().find(|p| p.id == owner) {
            project.roots.push(root_of(&row)?);
        }
    }
    Ok(out)
}

pub async fn project(conn: &mut SqliteConnection, id: &str) -> MemResult<Option<Project>> {
    let Some(row) = sqlx::query("SELECT id, name, created_ms FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let roots = sqlx::query(
        "SELECT identity, project_id, kind, root_path, origin_url, added_ms FROM project_roots WHERE project_id = ? \
         ORDER BY added_ms, identity",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(Project {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        created_ms: row.try_get("created_ms")?,
        roots: roots.iter().map(root_of).collect::<MemResult<Vec<_>>>()?,
    }))
}

fn root_of(row: &SqliteRow) -> MemResult<Root> {
    let kind: String = row.try_get("kind")?;
    Ok(Root {
        identity: row.try_get("identity")?,
        kind: RootKind::parse(&kind).ok_or_else(|| MemError::store(format!("unknown root kind {kind}")))?,
        path: row.try_get("root_path")?,
        origin_url: row.try_get("origin_url")?,
        added_ms: row.try_get("added_ms")?,
    })
}

pub async fn insert_project(conn: &mut SqliteConnection, id: &str, name: &str, created_ms: i64) -> MemResult<()> {
    sqlx::query("INSERT INTO projects (id, name, created_ms) VALUES (?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(created_ms)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn rename_project(conn: &mut SqliteConnection, id: &str, name: &str) -> MemResult<bool> {
    let done = sqlx::query("UPDATE projects SET name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *conn).await?;
    Ok(done.rows_affected() > 0)
}

/// Delete a project and everything in it; answers (records, links) removed.
pub async fn delete_project(conn: &mut SqliteConnection, id: &str) -> MemResult<(u64, u64)> {
    let links: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM memory_links WHERE from_id IN (SELECT id FROM memories WHERE project_id = ?) \
         OR to_id IN (SELECT id FROM memories WHERE project_id = ?)",
    )
    .bind(id)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    // Rows first, one statement: the FTS trigger fires per row and the cascades take
    // anchors, refs, revisions and links with them.
    let records = sqlx::query("DELETE FROM memories WHERE project_id = ?").bind(id).execute(&mut *conn).await?;
    crate::docs::drop_project_docs(conn, id).await?;
    sqlx::query("DELETE FROM projects WHERE id = ?").bind(id).execute(&mut *conn).await?;
    Ok((records.rows_affected(), links.max(0) as u64))
}

/// The project owning one identity.
pub async fn root_owner(conn: &mut SqliteConnection, identity: &str) -> MemResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT project_id FROM project_roots WHERE identity = ?")
        .bind(identity)
        .fetch_optional(&mut *conn)
        .await?)
}

pub async fn insert_root(conn: &mut SqliteConnection, project_id: &str, root: &Root) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO project_roots (identity, project_id, kind, root_path, origin_url, added_ms) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&root.identity)
    .bind(project_id)
    .bind(root.kind.as_str())
    .bind(&root.path)
    .bind(&root.origin_url)
    .bind(root.added_ms)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn delete_root(conn: &mut SqliteConnection, project_id: &str, identity: &str) -> MemResult<bool> {
    let done = sqlx::query("DELETE FROM project_roots WHERE project_id = ? AND identity = ?")
        .bind(project_id)
        .bind(identity)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Point one identity's row at the directory it resolves to now.
pub async fn move_root(conn: &mut SqliteConnection, identity: &str, path: &str) -> MemResult<()> {
    sqlx::query("UPDATE project_roots SET root_path = ? WHERE identity = ?")
        .bind(path)
        .bind(identity)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Every registered exact-directory identity (the resolver's `is_exact` set).
pub async fn path_identities(conn: &mut SqliteConnection) -> MemResult<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT identity FROM project_roots WHERE kind = 'path'")
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Every bound directory: (project id, identity, root path).
pub async fn root_paths(conn: &mut SqliteConnection) -> MemResult<Vec<(String, String, String)>> {
    let rows = sqlx::query("SELECT project_id, identity, root_path FROM project_roots ORDER BY added_ms")
        .fetch_all(&mut *conn)
        .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("project_id")?, row.try_get("identity")?, row.try_get("root_path")?)))
        .collect()
}

/// Projects already carrying `origin` under some identity: (project id, identity).
pub async fn roots_by_origin(conn: &mut SqliteConnection, origin: &str) -> MemResult<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT project_id, identity FROM project_roots WHERE origin_url = ? ORDER BY added_ms")
        .bind(origin)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("project_id")?, row.try_get("identity")?)))
        .collect()
}

/// Per-project counts: (live, needs_review, unconfirmed live, history, links).
pub async fn counts(conn: &mut SqliteConnection) -> MemResult<HashMap<String, [u64; 5]>> {
    let mut out: HashMap<String, [u64; 5]> = HashMap::new();
    let rows = sqlx::query(
        "SELECT project_id, status, confirmed, COUNT(*) AS n FROM memories GROUP BY project_id, status, confirmed",
    )
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        let project: String = row.try_get("project_id")?;
        let status: String = row.try_get("status")?;
        let confirmed: i64 = row.try_get("confirmed")?;
        let n = row.try_get::<i64, _>("n")?.max(0) as u64;
        let slot = out.entry(project).or_insert([0; 5]);
        match Status::parse(&status) {
            Some(s) if s.is_live() => {
                slot[0] += n;
                if s == Status::NeedsReview {
                    slot[1] += n;
                }
                if confirmed == 0 {
                    slot[2] += n;
                }
            }
            _ => slot[3] += n,
        }
    }
    let links = sqlx::query(
        "SELECT m.project_id AS project_id, COUNT(*) AS n FROM memory_links l JOIN memories m ON m.id = l.from_id \
         GROUP BY m.project_id",
    )
    .fetch_all(&mut *conn)
    .await?;
    for row in links {
        let project: String = row.try_get("project_id")?;
        out.entry(project).or_insert([0; 5])[4] = row.try_get::<i64, _>("n")?.max(0) as u64;
    }
    Ok(out)
}

// --- records ----------------------------------------------------------------------------

const MEMORY_COLUMNS: &str = "id, project_id, kind, title, content, keywords, status, origin, actor, producer, transport, \
                              confirmed, superseded_by, revision, created_ms, updated_ms, verified_ms, domain_id, \
                              (SELECT d.path FROM memory_domains d WHERE d.id = memories.domain_id) AS domain_path, \
                              EXISTS (SELECT 1 FROM memory_evidence e WHERE e.memory_id = memories.id \
                                      AND e.note_hash = memories.content_hash AND e.relation = 'verified_by' \
                                      AND e.source_type IN ('code', 'commit', 'test', 'doc')) AS source_verified";

/// The filter of a scope listing.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub statuses: Vec<Status>,
    pub kinds: Vec<Kind>,
    pub unconfirmed_only: bool,
    pub limit: Option<u64>,
}

/// Rows of one scope, newest update first, anchors and refs attached.
pub async fn memories(conn: &mut SqliteConnection, project_id: &str, filter: &Filter) -> MemResult<Vec<Memory>> {
    let mut sql = format!(
        "SELECT {MEMORY_COLUMNS} FROM memories WHERE project_id = ?{}{}",
        status_clause("status", &filter.statuses),
        in_clause("kind", &filter.kinds.iter().map(Kind::as_str).collect::<Vec<_>>()),
    );
    if filter.unconfirmed_only {
        sql.push_str(" AND confirmed = 0");
    }
    sql.push_str(" ORDER BY updated_ms DESC, id ASC");
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {}", limit.min(1_000_000)));
    }
    let rows = sqlx::query(audited(sql)).bind(project_id).fetch_all(&mut *conn).await?;
    let mut out = rows.iter().map(memory_of).collect::<MemResult<Vec<_>>>()?;
    attach(conn, project_id, &mut out).await?;
    Ok(out)
}

/// One record with anchors and refs, wherever it lives (callers check the scope).
pub async fn memory(conn: &mut SqliteConnection, id: &str) -> MemResult<Option<Memory>> {
    let sql = format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?");
    let Some(row) = sqlx::query(audited(sql)).bind(id).fetch_optional(&mut *conn).await? else {
        return Ok(None);
    };
    let mut memory = memory_of(&row)?;
    let anchors = sqlx::query("SELECT path, is_glob, hash FROM memory_anchors WHERE memory_id = ? ORDER BY idx")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    for row in anchors {
        memory.anchors.push(anchor_of(&row)?);
    }
    memory.refs = sqlx::query_scalar("SELECT ref FROM memory_refs WHERE memory_id = ? ORDER BY idx")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(Some(memory))
}

/// Batch-attach anchors and refs to rows of one scope.
async fn attach(conn: &mut SqliteConnection, project_id: &str, rows: &mut [Memory]) -> MemResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let index: HashMap<String, usize> = rows.iter().enumerate().map(|(i, m)| (m.id.clone(), i)).collect();
    let anchors = sqlx::query(
        "SELECT a.memory_id AS memory_id, a.path AS path, a.is_glob AS is_glob, a.hash AS hash FROM memory_anchors a \
         JOIN memories m ON m.id = a.memory_id WHERE m.project_id = ? ORDER BY a.memory_id, a.idx",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    for row in anchors {
        let id: String = row.try_get("memory_id")?;
        if let Some(i) = index.get(&id) {
            rows[*i].anchors.push(anchor_of(&row)?);
        }
    }
    let refs = sqlx::query(
        "SELECT r.memory_id AS memory_id, r.ref AS ref FROM memory_refs r JOIN memories m ON m.id = r.memory_id \
         WHERE m.project_id = ? ORDER BY r.memory_id, r.idx",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    for row in refs {
        let id: String = row.try_get("memory_id")?;
        if let Some(i) = index.get(&id) {
            rows[*i].refs.push(row.try_get("ref")?);
        }
    }
    Ok(())
}

fn anchor_of(row: &SqliteRow) -> MemResult<Anchor> {
    Ok(Anchor {
        path: row.try_get("path")?,
        glob: row.try_get::<i64, _>("is_glob")? != 0,
        hash: row.try_get("hash")?,
    })
}

fn memory_of(row: &SqliteRow) -> MemResult<Memory> {
    let kind: String = row.try_get("kind")?;
    let status: String = row.try_get("status")?;
    let origin: String = row.try_get("origin")?;
    let producer: String = row.try_get("producer")?;
    let transport: String = row.try_get("transport")?;
    Ok(Memory {
        id: row.try_get("id")?,
        scope: row.try_get("project_id")?,
        kind: Kind::parse(&kind).ok_or_else(|| MemError::store(format!("unknown kind {kind}")))?,
        title: row.try_get("title")?,
        content: row.try_get("content")?,
        status: Status::parse(&status).ok_or_else(|| MemError::store(format!("unknown status {status}")))?,
        origin: Origin::parse(&origin).ok_or_else(|| MemError::store(format!("unknown origin {origin}")))?,
        actor: row.try_get("actor")?,
        producer: Producer::parse(&producer).ok_or_else(|| MemError::store(format!("unknown producer {producer}")))?,
        transport: Transport::parse(&transport).ok_or_else(|| MemError::store(format!("unknown transport {transport}")))?,
        confirmed: row.try_get::<i64, _>("confirmed")? != 0,
        source_verified: row.try_get::<i64, _>("source_verified")? != 0,
        anchors: Vec::new(),
        refs: Vec::new(),
        keywords: keywords_of(&row.try_get::<String, _>("keywords")?),
        domain: row.try_get("domain_id")?,
        domain_path: row.try_get("domain_path")?,
        superseded_by: row.try_get("superseded_by")?,
        revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
        created_ms: row.try_get("created_ms")?,
        updated_ms: row.try_get("updated_ms")?,
        verified_ms: row.try_get("verified_ms")?,
    })
}

/// The `keywords` column: one keyword per line (a keyword is one line by [check_keywords]).
///
/// [check_keywords]: crate::model::check_keywords
pub fn keywords_text(keywords: &[String]) -> String {
    keywords.join("\n")
}

fn keywords_of(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
}

/// The normalized title equality key: case-folded, whitespace collapsed.
pub fn title_norm(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The exact-duplicate key: normalized title and content.
pub fn content_hash(title: &str, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let body = content.replace("\r\n", "\n");
    let body = body.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    let mut hasher = Sha256::new();
    hasher.update(title_norm(title).as_bytes());
    hasher.update([0u8]);
    hasher.update(body.trim().as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    swiss_core::util::to_hex(&digest)
}

/// Insert a record with its anchors and refs (no revision row).
pub async fn insert_memory(conn: &mut SqliteConnection, memory: &Memory) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO memories (id, project_id, kind, title, title_norm, content, content_hash, keywords, status, origin, \
         actor, producer, transport, confirmed, superseded_by, revision, created_ms, updated_ms, verified_ms, domain_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&memory.id)
    .bind(&memory.scope)
    .bind(memory.kind.as_str())
    .bind(&memory.title)
    .bind(title_norm(&memory.title))
    .bind(&memory.content)
    .bind(content_hash(&memory.title, &memory.content))
    .bind(keywords_text(&memory.keywords))
    .bind(memory.status.as_str())
    .bind(memory.origin.as_str())
    .bind(&memory.actor)
    .bind(memory.producer.as_str())
    .bind(memory.transport.as_str())
    .bind(memory.confirmed as i64)
    .bind(&memory.superseded_by)
    .bind(memory.revision as i64)
    .bind(memory.created_ms)
    .bind(memory.updated_ms)
    .bind(memory.verified_ms)
    .bind(&memory.domain)
    .execute(&mut *conn)
    .await?;
    write_children(conn, memory).await
}

/// Rewrite every mutable column of a record, plus its anchors and refs.
pub async fn update_memory(conn: &mut SqliteConnection, memory: &Memory) -> MemResult<()> {
    sqlx::query(
        "UPDATE memories SET kind = ?, title = ?, title_norm = ?, content = ?, content_hash = ?, keywords = ?, status = ?, \
         confirmed = ?, superseded_by = ?, revision = ?, updated_ms = ?, verified_ms = ?, domain_id = ? WHERE id = ?",
    )
    .bind(memory.kind.as_str())
    .bind(&memory.title)
    .bind(title_norm(&memory.title))
    .bind(&memory.content)
    .bind(content_hash(&memory.title, &memory.content))
    .bind(keywords_text(&memory.keywords))
    .bind(memory.status.as_str())
    .bind(memory.confirmed as i64)
    .bind(&memory.superseded_by)
    .bind(memory.revision as i64)
    .bind(memory.updated_ms)
    .bind(memory.verified_ms)
    .bind(&memory.domain)
    .bind(&memory.id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM memory_anchors WHERE memory_id = ?").bind(&memory.id).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM memory_refs WHERE memory_id = ?").bind(&memory.id).execute(&mut *conn).await?;
    write_children(conn, memory).await
}

async fn write_children(conn: &mut SqliteConnection, memory: &Memory) -> MemResult<()> {
    for (idx, anchor) in memory.anchors.iter().enumerate() {
        sqlx::query("INSERT INTO memory_anchors (memory_id, idx, path, is_glob, hash) VALUES (?, ?, ?, ?, ?)")
            .bind(&memory.id)
            .bind(idx as i64)
            .bind(&anchor.path)
            .bind(anchor.glob as i64)
            .bind(&anchor.hash)
            .execute(&mut *conn)
            .await?;
    }
    for (idx, reference) in memory.refs.iter().enumerate() {
        sqlx::query("INSERT INTO memory_refs (memory_id, idx, ref) VALUES (?, ?, ?)")
            .bind(&memory.id)
            .bind(idx as i64)
            .bind(reference)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Physical delete; cascades children, revisions and links. Answers whether a row went.
pub async fn delete_memory(conn: &mut SqliteConnection, id: &str) -> MemResult<bool> {
    let done = sqlx::query("DELETE FROM memories WHERE id = ?").bind(id).execute(&mut *conn).await?;
    Ok(done.rows_affected() > 0)
}

/// A live record of the scope with this exact content hash.
pub async fn live_by_hash(conn: &mut SqliteConnection, project_id: &str, hash: &str) -> MemResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM memories WHERE project_id = ? AND content_hash = ? AND status IN ('active', 'needs_review') \
         ORDER BY created_ms LIMIT 1",
    )
    .bind(project_id)
    .bind(hash)
    .fetch_optional(&mut *conn)
    .await?)
}

/// A live record of the scope with this normalized title: (id, revision, title).
pub async fn live_by_title(
    conn: &mut SqliteConnection,
    project_id: &str,
    norm: &str,
) -> MemResult<Option<(String, u64, String)>> {
    let row = sqlx::query(
        "SELECT id, revision, title FROM memories WHERE project_id = ? AND title_norm = ? \
         AND status IN ('active', 'needs_review') ORDER BY created_ms LIMIT 1",
    )
    .bind(project_id)
    .bind(norm)
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some(row) => Ok(Some((row.try_get("id")?, row.try_get::<i64, _>("revision")?.max(0) as u64, row.try_get("title")?))),
        None => Ok(None),
    }
}

/// Every live record of the scope whose normalized title (`by_hash`: content hash) is
/// `key`, oldest first: an import passes over the notes it wrote itself.
pub async fn live_ids_by(conn: &mut SqliteConnection, project_id: &str, key: &str, by_hash: bool) -> MemResult<Vec<String>> {
    let column = if by_hash { "content_hash" } else { "title_norm" };
    let sql = format!(
        "SELECT id FROM memories WHERE project_id = ? AND {column} = ? AND status IN ('active', 'needs_review') \
         ORDER BY created_ms, id"
    );
    Ok(sqlx::query_scalar(audited(sql)).bind(project_id).bind(key).fetch_all(&mut *conn).await?)
}

// --- evidence ---------------------------------------------------------------------------

const EVIDENCE_COLUMNS: &str = "id, memory_id, revision, note_hash, source_type, relation, path, symbol, commit_sha, \
                                session_id, run_id, source_hash, excerpt_hash, actor, created_ms";

fn evidence_of_row(row: &SqliteRow) -> MemResult<Evidence> {
    let source: String = row.try_get("source_type")?;
    let relation: String = row.try_get("relation")?;
    Ok(Evidence {
        id: row.try_get("id")?,
        memory_id: row.try_get("memory_id")?,
        revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
        note_hash: row.try_get("note_hash")?,
        source_type: SourceType::parse(&source).ok_or_else(|| MemError::store(format!("unknown source type {source}")))?,
        relation: Relation::parse(&relation).ok_or_else(|| MemError::store(format!("unknown relation {relation}")))?,
        path: row.try_get("path")?,
        symbol: row.try_get("symbol")?,
        commit_sha: row.try_get("commit_sha")?,
        session_id: row.try_get("session_id")?,
        run_id: row.try_get("run_id")?,
        source_hash: row.try_get("source_hash")?,
        excerpt_hash: row.try_get("excerpt_hash")?,
        actor: row.try_get("actor")?,
        created_ms: row.try_get("created_ms")?,
    })
}

/// A note's evidence, oldest first. Answers whether a row went in (an import repeats ids).
pub async fn insert_evidence(conn: &mut SqliteConnection, evidence: &Evidence) -> MemResult<bool> {
    let sql = format!("INSERT OR IGNORE INTO memory_evidence ({EVIDENCE_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)");
    let done = sqlx::query(audited(sql))
        .bind(&evidence.id)
        .bind(&evidence.memory_id)
        .bind(evidence.revision as i64)
        .bind(&evidence.note_hash)
        .bind(evidence.source_type.as_str())
        .bind(evidence.relation.as_str())
        .bind(&evidence.path)
        .bind(&evidence.symbol)
        .bind(&evidence.commit_sha)
        .bind(&evidence.session_id)
        .bind(&evidence.run_id)
        .bind(&evidence.source_hash)
        .bind(&evidence.excerpt_hash)
        .bind(&evidence.actor)
        .bind(evidence.created_ms)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// A note's evidence, oldest first.
pub async fn evidence(conn: &mut SqliteConnection, memory_id: &str) -> MemResult<Vec<Evidence>> {
    let sql = format!("SELECT {EVIDENCE_COLUMNS} FROM memory_evidence WHERE memory_id = ? ORDER BY created_ms, revision, id");
    let rows = sqlx::query(audited(sql)).bind(memory_id).fetch_all(&mut *conn).await?;
    rows.iter().map(evidence_of_row).collect()
}

pub async fn evidence_count(conn: &mut SqliteConnection, memory_id: &str) -> MemResult<usize> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM memory_evidence WHERE memory_id = ?").bind(memory_id).fetch_one(&mut *conn).await?;
    Ok(n.max(0) as usize)
}

// --- revisions --------------------------------------------------------------------------

pub async fn insert_revision(conn: &mut SqliteConnection, id: &str, revision: &Revision) -> MemResult<()> {
    let snapshot = serde_json::to_string(&revision.snapshot).map_err(|e| MemError::store(e.to_string()))?;
    sqlx::query(
        "INSERT INTO memory_revisions (memory_id, revision, op, actor, reason, at_ms, snapshot_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(revision.revision as i64)
    .bind(&revision.op)
    .bind(&revision.actor)
    .bind(&revision.reason)
    .bind(revision.at_ms)
    .bind(snapshot)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A record's history, oldest first.
pub async fn revisions(conn: &mut SqliteConnection, id: &str) -> MemResult<Vec<Revision>> {
    let rows = sqlx::query(
        "SELECT revision, op, actor, reason, at_ms, snapshot_json FROM memory_revisions WHERE memory_id = ? ORDER BY revision",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let snapshot: String = row.try_get("snapshot_json")?;
        let snapshot: Snapshot =
            serde_json::from_str(&snapshot).map_err(|e| MemError::store(format!("revision snapshot of {id}: {e}")))?;
        out.push(Revision {
            revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
            op: row.try_get("op")?,
            actor: row.try_get("actor")?,
            reason: row.try_get("reason")?,
            at_ms: row.try_get("at_ms")?,
            snapshot,
        });
    }
    Ok(out)
}

// --- domains ----------------------------------------------------------------------------

const DOMAIN_COLUMNS: &str = "id, project_id, path, title, summary, body, owns_json, keywords, origin, actor, confirmed, \
                              revision, created_ms, updated_ms, verified_ms";

fn domain_of(row: &SqliteRow) -> MemResult<Domain> {
    let origin: String = row.try_get("origin")?;
    let owns: String = row.try_get("owns_json")?;
    let id: String = row.try_get("id")?;
    Ok(Domain {
        owns: serde_json::from_str(&owns).map_err(|e| MemError::store(format!("owns of {id}: {e}")))?,
        id,
        scope: row.try_get("project_id")?,
        path: row.try_get("path")?,
        title: row.try_get("title")?,
        summary: row.try_get("summary")?,
        body: row.try_get("body")?,
        keywords: keywords_of(&row.try_get::<String, _>("keywords")?),
        origin: Origin::parse(&origin).ok_or_else(|| MemError::store(format!("unknown origin {origin}")))?,
        actor: row.try_get("actor")?,
        confirmed: row.try_get::<i64, _>("confirmed")? != 0,
        revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
        created_ms: row.try_get("created_ms")?,
        updated_ms: row.try_get("updated_ms")?,
        verified_ms: row.try_get("verified_ms")?,
    })
}

/// A project's map, in tree order.
pub async fn domains(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Vec<Domain>> {
    let sql = format!("SELECT {DOMAIN_COLUMNS} FROM memory_domains WHERE project_id = ?");
    let rows = sqlx::query(audited(sql)).bind(project_id).fetch_all(&mut *conn).await?;
    let mut out = rows.iter().map(domain_of).collect::<MemResult<Vec<_>>>()?;
    out.sort_by(|a, b| crate::model::domain_order(&a.path, &b.path));
    Ok(out)
}

/// The hubs of a project's map that say anything (the search's hub mention, §memory.search).
pub async fn hub_bodies(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT body FROM memory_domains WHERE project_id = ? AND body <> '' ORDER BY path")
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?)
}

/// One domain, wherever it lives (callers check the scope).
pub async fn domain(conn: &mut SqliteConnection, id: &str) -> MemResult<Option<Domain>> {
    let sql = format!("SELECT {DOMAIN_COLUMNS} FROM memory_domains WHERE id = ?");
    let row = sqlx::query(audited(sql)).bind(id).fetch_optional(&mut *conn).await?;
    row.as_ref().map(domain_of).transpose()
}

fn owns_json(domain: &Domain) -> MemResult<String> {
    serde_json::to_string(&domain.owns).map_err(|e| MemError::store(e.to_string()))
}

pub async fn insert_domain(conn: &mut SqliteConnection, domain: &Domain) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO memory_domains (id, project_id, path, title, summary, body, owns_json, keywords, origin, actor, \
         confirmed, revision, created_ms, updated_ms, verified_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&domain.id)
    .bind(&domain.scope)
    .bind(&domain.path)
    .bind(&domain.title)
    .bind(&domain.summary)
    .bind(&domain.body)
    .bind(owns_json(domain)?)
    .bind(keywords_text(&domain.keywords))
    .bind(domain.origin.as_str())
    .bind(&domain.actor)
    .bind(domain.confirmed as i64)
    .bind(domain.revision as i64)
    .bind(domain.created_ms)
    .bind(domain.updated_ms)
    .bind(domain.verified_ms)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Rewrite every mutable column of a domain.
pub async fn update_domain(conn: &mut SqliteConnection, domain: &Domain) -> MemResult<()> {
    sqlx::query(
        "UPDATE memory_domains SET path = ?, title = ?, summary = ?, body = ?, owns_json = ?, keywords = ?, confirmed = ?, \
         revision = ?, updated_ms = ?, verified_ms = ? WHERE id = ?",
    )
    .bind(&domain.path)
    .bind(&domain.title)
    .bind(&domain.summary)
    .bind(&domain.body)
    .bind(owns_json(domain)?)
    .bind(keywords_text(&domain.keywords))
    .bind(domain.confirmed as i64)
    .bind(domain.revision as i64)
    .bind(domain.updated_ms)
    .bind(domain.verified_ms)
    .bind(&domain.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn delete_domain(conn: &mut SqliteConnection, id: &str) -> MemResult<bool> {
    let done = sqlx::query("DELETE FROM memory_domains WHERE id = ?").bind(id).execute(&mut *conn).await?;
    Ok(done.rows_affected() > 0)
}

pub async fn insert_domain_revision(conn: &mut SqliteConnection, id: &str, revision: &DomainRevision) -> MemResult<()> {
    let snapshot = serde_json::to_string(&revision.snapshot).map_err(|e| MemError::store(e.to_string()))?;
    sqlx::query(
        "INSERT INTO memory_domain_revisions (domain_id, revision, op, actor, reason, at_ms, snapshot_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(revision.revision as i64)
    .bind(&revision.op)
    .bind(&revision.actor)
    .bind(&revision.reason)
    .bind(revision.at_ms)
    .bind(snapshot)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A domain's history, oldest first.
pub async fn domain_revisions(conn: &mut SqliteConnection, id: &str) -> MemResult<Vec<DomainRevision>> {
    let rows = sqlx::query(
        "SELECT revision, op, actor, reason, at_ms, snapshot_json FROM memory_domain_revisions WHERE domain_id = ? \
         ORDER BY revision",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let snapshot: String = row.try_get("snapshot_json")?;
        let snapshot: DomainSnapshot =
            serde_json::from_str(&snapshot).map_err(|e| MemError::store(format!("revision snapshot of {id}: {e}")))?;
        out.push(DomainRevision {
            revision: row.try_get::<i64, _>("revision")?.max(0) as u64,
            op: row.try_get("op")?,
            actor: row.try_get("actor")?,
            reason: row.try_get("reason")?,
            at_ms: row.try_get("at_ms")?,
            snapshot,
        });
    }
    Ok(out)
}

/// File one note (None: unfile it). Organisation only: no revision, no `updated_ms`.
/// Answers whether the note moved.
pub async fn file_memory(conn: &mut SqliteConnection, id: &str, domain: Option<&str>) -> MemResult<bool> {
    let done = sqlx::query("UPDATE memories SET domain_id = ? WHERE id = ? AND domain_id IS NOT ?")
        .bind(domain)
        .bind(id)
        .bind(domain)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Move every note of one domain to another (None: unfiled): the count moved.
pub async fn refile(conn: &mut SqliteConnection, from: &str, to: Option<&str>) -> MemResult<u64> {
    let done = sqlx::query("UPDATE memories SET domain_id = ? WHERE domain_id = ?")
        .bind(to)
        .bind(from)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected())
}

// --- links ------------------------------------------------------------------------------

fn link_of(row: &SqliteRow) -> MemResult<Link> {
    let kind: String = row.try_get("kind")?;
    Ok(Link {
        from: row.try_get("from_id")?,
        to: row.try_get("to_id")?,
        kind: LinkKind::parse(&kind).ok_or_else(|| MemError::store(format!("unknown link kind {kind}")))?,
        note: row.try_get("note")?,
        actor: row.try_get("actor")?,
        created_ms: row.try_get("created_ms")?,
    })
}

/// Every link touching one record, both directions.
pub async fn links_touching(conn: &mut SqliteConnection, id: &str) -> MemResult<Vec<Link>> {
    let rows = sqlx::query(
        "SELECT from_id, to_id, kind, note, actor, created_ms FROM memory_links WHERE from_id = ? OR to_id = ? \
         ORDER BY kind, from_id, to_id",
    )
    .bind(id)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(link_of).collect()
}

/// Every link of one scope (links never cross scopes, so the `from` end decides).
pub async fn links_in(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Vec<Link>> {
    let rows = sqlx::query(
        "SELECT l.from_id AS from_id, l.to_id AS to_id, l.kind AS kind, l.note AS note, l.actor AS actor, \
         l.created_ms AS created_ms FROM memory_links l JOIN memories m ON m.id = l.from_id WHERE m.project_id = ? \
         ORDER BY l.from_id, l.to_id, l.kind",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(link_of).collect()
}

pub async fn link(conn: &mut SqliteConnection, from: &str, to: &str, kind: LinkKind) -> MemResult<Option<Link>> {
    let row = sqlx::query(
        "SELECT from_id, to_id, kind, note, actor, created_ms FROM memory_links WHERE from_id = ? AND to_id = ? AND kind = ?",
    )
    .bind(from)
    .bind(to)
    .bind(kind.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    row.as_ref().map(link_of).transpose()
}

pub async fn insert_link(conn: &mut SqliteConnection, link: &Link) -> MemResult<()> {
    sqlx::query("INSERT INTO memory_links (from_id, to_id, kind, note, actor, created_ms) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&link.from)
        .bind(&link.to)
        .bind(link.kind.as_str())
        .bind(&link.note)
        .bind(&link.actor)
        .bind(link.created_ms)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn set_link_note(conn: &mut SqliteConnection, link: &Link) -> MemResult<()> {
    sqlx::query("UPDATE memory_links SET note = ?, actor = ? WHERE from_id = ? AND to_id = ? AND kind = ?")
        .bind(&link.note)
        .bind(&link.actor)
        .bind(&link.from)
        .bind(&link.to)
        .bind(link.kind.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn delete_link(conn: &mut SqliteConnection, from: &str, to: &str, kind: LinkKind) -> MemResult<bool> {
    let done = sqlx::query("DELETE FROM memory_links WHERE from_id = ? AND to_id = ? AND kind = ?")
        .bind(from)
        .bind(to)
        .bind(kind.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn link_count(conn: &mut SqliteConnection, id: &str) -> MemResult<u64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_links WHERE from_id = ? OR to_id = ?")
        .bind(id)
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(n.max(0) as u64)
}

// --- lexical ----------------------------------------------------------------------------

/// FTS5 MATCH over one scope: (id, bm25) best first. bm25 is negative; smaller is better.
pub async fn fts(
    conn: &mut SqliteConnection,
    project_id: &str,
    expr: &str,
    statuses: &[Status],
    cap: u64,
) -> MemResult<Vec<(String, f64)>> {
    let rows = sqlx::query(audited(fts_sql(statuses, cap))).bind(expr).bind(project_id).fetch_all(&mut *conn).await?;
    rows.iter().map(|row| Ok((row.try_get("id")?, row.try_get("rank")?))).collect()
}

/// The [fts] statement: binds the MATCH expression, then the project id. CROSS JOIN pins
/// the order: the MATCH runs once and each hit's note is found by rowid. Left to choose,
/// SQLite walks the scope's notes and re-runs the MATCH for every one of them.
fn fts_sql(statuses: &[Status], cap: u64) -> String {
    format!(
        "SELECT m.id AS id, bm25(memory_fts, 3.0, 1.0, 2.0) AS rank FROM memory_fts CROSS JOIN memories m ON m.rowid = memory_fts.rowid \
         WHERE memory_fts MATCH ? AND m.project_id = ?{} ORDER BY rank, m.id LIMIT {}",
        status_clause("m.status", statuses),
        cap.min(10_000)
    )
}

/// The escaped-LIKE pass over one scope (CJK fragments, punctuation-rich identifiers).
pub async fn substring(
    conn: &mut SqliteConnection,
    project_id: &str,
    needle: &str,
    statuses: &[Status],
    cap: u64,
) -> MemResult<Vec<String>> {
    let sql = format!(
        "SELECT id FROM memories WHERE project_id = ? AND (title LIKE ? ESCAPE '\\' OR content LIKE ? ESCAPE '\\' OR keywords LIKE ? ESCAPE '\\'){} \
         ORDER BY updated_ms DESC, id LIMIT {}",
        status_clause("status", statuses),
        cap.min(10_000)
    );
    let pattern = format!("%{}%", like_escape(needle));
    Ok(sqlx::query_scalar(audited(sql))
        .bind(project_id)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .fetch_all(&mut *conn)
        .await?)
}

/// Escape a LIKE needle: % _ and the backslash itself.
pub fn like_escape(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 4);
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// --- vectors ----------------------------------------------------------------------------

/// One live note's embedding input: (id, title, content), across every project,
/// newest update first. The projects whose settings turn embedding off (SPEC
/// §memory.embedding) are skipped: their notes are not embedding input.
pub async fn live_texts(conn: &mut SqliteConnection, skip_projects: &[String]) -> MemResult<Vec<(String, String, String)>> {
    let rows = sqlx::query(
        "SELECT id, project_id, title, content FROM memories WHERE status IN ('active', 'needs_review') ORDER BY updated_ms DESC, id",
    )
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .filter(|row| {
            let project: String = row.try_get("project_id").unwrap_or_default();
            !skip_projects.contains(&project)
        })
        .map(|row| Ok((row.try_get("id")?, row.try_get("title")?, row.try_get("content")?)))
        .collect()
}

/// One stored vector's key: what made it, from which text, how wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorKey {
    pub model: String,
    pub text_hash: String,
    pub dims: usize,
}

/// Every stored vector's key, by note id.
pub async fn vector_keys(conn: &mut SqliteConnection) -> MemResult<HashMap<String, VectorKey>> {
    let rows = sqlx::query("SELECT memory_id, model, text_hash, dims FROM memory_vectors").fetch_all(&mut *conn).await?;
    rows.iter()
        .map(|row| {
            let dims: i64 = row.try_get("dims")?;
            let key =
                VectorKey { model: row.try_get("model")?, text_hash: row.try_get("text_hash")?, dims: dims.max(0) as usize };
            Ok((row.try_get("memory_id")?, key))
        })
        .collect()
}

/// Store (or replace) one note's vector. A note purged since it was read is skipped,
/// not an error: the vector would only dangle.
pub async fn put_vector(
    conn: &mut SqliteConnection,
    id: &str,
    model: &str,
    text_hash: &str,
    vector: &[f32],
    at_ms: i64,
) -> MemResult<bool> {
    let done = sqlx::query(
        "INSERT INTO memory_vectors (memory_id, model, dims, text_hash, vec, at_ms) \
         SELECT ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM memories WHERE id = ?) \
         ON CONFLICT(memory_id) DO UPDATE SET model = excluded.model, dims = excluded.dims, \
         text_hash = excluded.text_hash, vec = excluded.vec, at_ms = excluded.at_ms",
    )
    .bind(id)
    .bind(model)
    .bind(vector.len() as i64)
    .bind(text_hash)
    .bind(crate::embed::to_bytes(vector))
    .bind(at_ms)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The vectors of one scope under one model, whatever their width: id -> (text_hash,
/// vector). A row whose blob does not decode to its `dims` is skipped.
pub async fn scope_vectors(
    conn: &mut SqliteConnection,
    project_id: &str,
    model: &str,
) -> MemResult<HashMap<String, (String, Vec<f32>)>> {
    let rows = sqlx::query(
        "SELECT v.memory_id AS id, v.text_hash AS text_hash, v.dims AS dims, v.vec AS vec FROM memory_vectors v \
         JOIN memories m ON m.id = v.memory_id WHERE m.project_id = ? AND v.model = ?",
    )
    .bind(project_id)
    .bind(model)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = HashMap::new();
    for row in rows {
        let bytes: Vec<u8> = row.try_get("vec")?;
        let dims: i64 = row.try_get("dims")?;
        if let Some(vector) = crate::embed::from_bytes(&bytes).filter(|v| v.len() as i64 == dims) {
            out.insert(row.try_get("id")?, (row.try_get("text_hash")?, vector));
        }
    }
    Ok(out)
}

/// One note's stored vector row as (model, dims, vec bytes), undecoded: the Data page
/// reads it to show the numbers (SPEC §data.sqlite "Decoded cells").
pub async fn stored_vector(conn: &mut SqliteConnection, memory_id: &str) -> MemResult<Option<(String, i64, Vec<u8>)>> {
    let row = sqlx::query("SELECT model, dims, vec FROM memory_vectors WHERE memory_id = ?")
        .bind(memory_id)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some(row) => Ok(Some((row.try_get("model")?, row.try_get("dims")?, row.try_get("vec")?))),
        None => Ok(None),
    }
}

/// The widths stored under `model` as (dims, vectors), the most recently written
/// first. More than one means the index is stale (SPEC §memory.embedding).
pub async fn vector_widths(conn: &mut SqliteConnection, model: &str) -> MemResult<Vec<(usize, usize)>> {
    let rows = sqlx::query(
        "SELECT dims, COUNT(*) AS n, MAX(at_ms) AS newest FROM memory_vectors WHERE model = ? \
         GROUP BY dims ORDER BY newest DESC, n DESC, dims",
    )
    .bind(model)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .map(|row| {
            let dims: i64 = row.try_get("dims")?;
            let n: i64 = row.try_get("n")?;
            Ok((dims.max(0) as usize, n.max(0) as usize))
        })
        .collect()
}

/// Drop the vectors under `model` of any width but `dims` - one fingerprint holds one
/// width: the count removed. Their notes are pending again.
pub async fn drop_other_widths(conn: &mut SqliteConnection, model: &str, dims: usize) -> MemResult<u64> {
    let done = sqlx::query("DELETE FROM memory_vectors WHERE model = ? AND dims <> ?")
        .bind(model)
        .bind(dims as i64)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected())
}

/// Drop the vectors of one model (or every vector): the count removed.
pub async fn drop_vectors(conn: &mut SqliteConnection, model: Option<&str>) -> MemResult<u64> {
    let done = match model {
        Some(model) => sqlx::query("DELETE FROM memory_vectors WHERE model = ?").bind(model).execute(&mut *conn).await?,
        None => sqlx::query("DELETE FROM memory_vectors").execute(&mut *conn).await?,
    };
    Ok(done.rows_affected())
}

// --- sync ------------------------------------------------------------------------------

const RUN_COLUMNS: &str = "id, project_id, cause, dry_run, state, queued_ms, due_ms, started_ms, finished_ms, since, base, \
                           head, scope_json, ops_json, runner_json, proposal_json, summary, error";

fn run_of(row: &SqliteRow) -> MemResult<Run> {
    let id: String = row.try_get("id")?;
    let cause: String = row.try_get("cause")?;
    let state: String = row.try_get("state")?;
    let proposal: Option<String> = row.try_get("proposal_json")?;
    Ok(Run {
        project: row.try_get("project_id")?,
        trigger: Trigger::parse(&cause).ok_or_else(|| MemError::store(format!("unknown trigger {cause}")))?,
        dry_run: row.try_get::<i64, _>("dry_run")? != 0,
        state: RunState::parse(&state).ok_or_else(|| MemError::store(format!("unknown run state {state}")))?,
        queued_ms: row.try_get("queued_ms")?,
        due_ms: row.try_get("due_ms")?,
        started_ms: row.try_get("started_ms")?,
        finished_ms: row.try_get("finished_ms")?,
        since: row.try_get("since")?,
        base: row.try_get("base")?,
        head: row.try_get("head")?,
        scope: from_json(&row.try_get::<String, _>("scope_json")?, "scope", &id)?,
        ops: from_json(&row.try_get::<String, _>("ops_json")?, "ops", &id)?,
        runner: from_json(&row.try_get::<String, _>("runner_json")?, "runner", &id)?,
        proposal: proposal.map(|text| from_json(&text, "proposal", &id)).transpose()?,
        summary: row.try_get("summary")?,
        error: row.try_get("error")?,
        id,
    })
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str, what: &str, id: &str) -> MemResult<T> {
    serde_json::from_str(text).map_err(|e| MemError::store(format!("{what} of run {id}: {e}")))
}

fn to_json<T: serde::Serialize>(value: &T) -> MemResult<String> {
    serde_json::to_string(value).map_err(|e| MemError::store(e.to_string()))
}

/// Every project's settings as stored (SPEC §memory.settings): (project id, json).
pub async fn all_project_settings(conn: &mut SqliteConnection) -> MemResult<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT project_id, settings_json FROM memory_project_settings").fetch_all(&mut *conn).await?;
    rows.iter().map(|row| Ok((row.try_get("project_id")?, row.try_get("settings_json")?))).collect()
}

/// A project's settings as stored (SPEC §memory.settings); None when it never set any.
pub async fn project_settings(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT settings_json FROM memory_project_settings WHERE project_id = ?")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?)
}

pub async fn set_project_settings(conn: &mut SqliteConnection, project_id: &str, settings: &str, now: i64) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO memory_project_settings (project_id, settings_json, updated_ms) VALUES (?, ?, ?)          ON CONFLICT (project_id) DO UPDATE SET settings_json = excluded.settings_json, updated_ms = excluded.updated_ms",
    )
    .bind(project_id)
    .bind(settings)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Where the project's last sync left off.
pub async fn sync_head(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT head FROM memory_sync_state WHERE project_id = ?")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?)
}

pub async fn set_sync_head(conn: &mut SqliteConnection, project_id: &str, head: &str, now: i64) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO memory_sync_state (project_id, head, updated_ms) VALUES (?, ?, ?) \
         ON CONFLICT (project_id) DO UPDATE SET head = excluded.head, updated_ms = excluded.updated_ms",
    )
    .bind(project_id)
    .bind(head)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The commits a sync already sent.
pub async fn synced_commits(conn: &mut SqliteConnection, project_id: &str) -> MemResult<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT sha FROM memory_sync_commits WHERE project_id = ?")
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Record sent commits, keeping the newest `keep`.
pub async fn record_commits(conn: &mut SqliteConnection, project_id: &str, shas: &[String], now: i64, keep: usize) -> MemResult<()> {
    for sha in shas {
        sqlx::query("INSERT OR IGNORE INTO memory_sync_commits (project_id, sha, at_ms) VALUES (?, ?, ?)")
            .bind(project_id)
            .bind(sha)
            .bind(now)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query(
        "DELETE FROM memory_sync_commits WHERE project_id = ? AND rowid NOT IN \
         (SELECT rowid FROM memory_sync_commits WHERE project_id = ? ORDER BY at_ms DESC, rowid DESC LIMIT ?)",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(keep as i64)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The byte cursor of each transcript file, by its full path.
pub async fn sync_cursors(conn: &mut SqliteConnection, project_id: &str) -> MemResult<HashMap<String, i64>> {
    let rows = sqlx::query("SELECT file, position FROM memory_sync_cursors WHERE project_id = ?")
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter()
        .map(|row| Ok((row.try_get::<String, _>("file")?, row.try_get::<i64, _>("position")?)))
        .collect()
}

/// Move cursors; with `present`, the cursors of files no longer there are dropped.
pub async fn set_sync_cursors(
    conn: &mut SqliteConnection,
    project_id: &str,
    cursors: &[(String, i64)],
    present: Option<&HashSet<String>>,
) -> MemResult<()> {
    for (file, position) in cursors {
        sqlx::query(
            "INSERT INTO memory_sync_cursors (project_id, file, position) VALUES (?, ?, ?) \
             ON CONFLICT (project_id, file) DO UPDATE SET position = excluded.position",
        )
        .bind(project_id)
        .bind(file)
        .bind(position)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(present) = present {
        let known: Vec<String> = sqlx::query_scalar("SELECT file FROM memory_sync_cursors WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&mut *conn)
            .await?;
        for file in known.iter().filter(|f| !present.contains(*f)) {
            sqlx::query("DELETE FROM memory_sync_cursors WHERE project_id = ? AND file = ?")
                .bind(project_id)
                .bind(file)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

pub async fn insert_run(conn: &mut SqliteConnection, run: &Run) -> MemResult<()> {
    sqlx::query(
        "INSERT INTO memory_sync_runs (id, project_id, cause, dry_run, state, queued_ms, due_ms, started_ms, finished_ms, \
         since, base, head, scope_json, ops_json, runner_json, proposal_json, summary, error) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run.id)
    .bind(&run.project)
    .bind(run.trigger.as_str())
    .bind(run.dry_run as i64)
    .bind(run.state.as_str())
    .bind(run.queued_ms)
    .bind(run.due_ms)
    .bind(run.started_ms)
    .bind(run.finished_ms)
    .bind(&run.since)
    .bind(&run.base)
    .bind(&run.head)
    .bind(to_json(&run.scope)?)
    .bind(to_json(&run.ops)?)
    .bind(to_json(&run.runner)?)
    .bind(run.proposal.as_ref().map(to_json).transpose()?)
    .bind(&run.summary)
    .bind(&run.error)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Write back a run's every mutable column.
pub async fn save_run(conn: &mut SqliteConnection, run: &Run) -> MemResult<()> {
    sqlx::query(
        "UPDATE memory_sync_runs SET cause = ?, state = ?, due_ms = ?, started_ms = ?, finished_ms = ?, since = ?, \
         base = ?, head = ?, scope_json = ?, ops_json = ?, runner_json = ?, proposal_json = ?, summary = ?, error = ? \
         WHERE id = ?",
    )
    .bind(run.trigger.as_str())
    .bind(run.state.as_str())
    .bind(run.due_ms)
    .bind(run.started_ms)
    .bind(run.finished_ms)
    .bind(&run.since)
    .bind(&run.base)
    .bind(&run.head)
    .bind(to_json(&run.scope)?)
    .bind(to_json(&run.ops)?)
    .bind(to_json(&run.runner)?)
    .bind(run.proposal.as_ref().map(to_json).transpose()?)
    .bind(&run.summary)
    .bind(&run.error)
    .bind(&run.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn run(conn: &mut SqliteConnection, id: &str) -> MemResult<Option<Run>> {
    let sql = format!("SELECT {RUN_COLUMNS} FROM memory_sync_runs WHERE id = ?");
    let row = sqlx::query(audited(sql)).bind(id).fetch_optional(&mut *conn).await?;
    row.as_ref().map(run_of).transpose()
}

/// A project's runs, the newest first.
pub async fn runs(conn: &mut SqliteConnection, project_id: &str, limit: usize) -> MemResult<Vec<Run>> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM memory_sync_runs WHERE project_id = ? ORDER BY queued_ms DESC, rowid DESC LIMIT {}",
        limit.min(1000)
    );
    let rows = sqlx::query(audited(sql)).bind(project_id).fetch_all(&mut *conn).await?;
    rows.iter().map(run_of).collect()
}

/// The project's queued run of one kind: dry or not, of [crate::sync::Trigger::kind]
/// (a map, a full, a recheck, a review, a docs or a sync run). The causes listed here
/// are every kind but sync's: a cause missing from them waits in the sync queue.
pub async fn queued_run(conn: &mut SqliteConnection, project_id: &str, dry_run: bool, kind: &str) -> MemResult<Option<Run>> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM memory_sync_runs WHERE project_id = ? AND dry_run = ? AND state = 'queued' \
         AND (CASE WHEN cause IN ('map', 'full', 'recheck', 'review', 'docs') THEN cause ELSE 'sync' END) = ? ORDER BY queued_ms LIMIT 1"
    );
    let row = sqlx::query(audited(sql))
        .bind(project_id)
        .bind(dry_run as i64)
        .bind(kind)
        .fetch_optional(&mut *conn)
        .await?;
    row.as_ref().map(run_of).transpose()
}

/// The slices every ended full run of the project answered (not dry, whatever its end:
/// a failed or cancelled run still answered what it saved), by content hash - skipped
/// while unchanged (SPEC §memory.init).
pub async fn full_runs_done(conn: &mut SqliteConnection, project_id: &str) -> MemResult<HashSet<String>> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM memory_sync_runs WHERE project_id = ? AND cause = 'full' AND dry_run = 0 \
         AND state NOT IN ('queued', 'running')"
    );
    let rows = sqlx::query(audited(sql)).bind(project_id).fetch_all(&mut *conn).await?;
    let mut done: HashSet<String> = HashSet::new();
    for row in &rows {
        if let Some(knowledge) = run_of(row)?.scope.knowledge {
            done.extend(knowledge.done);
        }
    }
    Ok(done)
}

/// The queued run due first, whatever its project.
pub async fn next_queued_run(conn: &mut SqliteConnection) -> MemResult<Option<Run>> {
    let sql = format!("SELECT {RUN_COLUMNS} FROM memory_sync_runs WHERE state = 'queued' ORDER BY due_ms, queued_ms LIMIT 1");
    let row = sqlx::query(audited(sql)).fetch_optional(&mut *conn).await?;
    row.as_ref().map(run_of).transpose()
}

/// When the project's last finished run finished.
pub async fn last_finished(conn: &mut SqliteConnection, project_id: &str) -> MemResult<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT MAX(finished_ms) FROM memory_sync_runs WHERE project_id = ?")
        .bind(project_id)
        .fetch_one(&mut *conn)
        .await?)
}

/// Fail every queued or running run (a restart): the count.
pub async fn interrupt_runs(conn: &mut SqliteConnection, now: i64) -> MemResult<u64> {
    let done = sqlx::query(
        "UPDATE memory_sync_runs SET state = 'failed', finished_ms = ?, error = 'interrupted: the gateway stopped first' \
         WHERE state IN ('queued', 'running')",
    )
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected())
}

/// Keep a project's newest `keep` runs (queued and running ones always stay).
pub async fn prune_runs(conn: &mut SqliteConnection, project_id: &str, keep: usize) -> MemResult<()> {
    sqlx::query(
        "DELETE FROM memory_sync_runs WHERE project_id = ? AND state NOT IN ('queued', 'running') AND id NOT IN \
         (SELECT id FROM memory_sync_runs WHERE project_id = ? ORDER BY queued_ms DESC, rowid DESC LIMIT ?)",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(keep as i64)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Delete these runs of a project; queued and running ones stay.
pub async fn delete_runs(conn: &mut SqliteConnection, project_id: &str, ids: &[String]) -> MemResult<()> {
    for id in ids {
        sqlx::query("DELETE FROM memory_sync_runs WHERE project_id = ? AND id = ? AND state NOT IN ('queued', 'running')")
            .bind(project_id)
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_ignore_case_and_trailing_space() {
        assert_eq!(title_norm("  Use  BEGIN Immediate "), "use begin immediate");
        assert_eq!(content_hash("A title", "body  \r\nline\n"), content_hash("a  TITLE", "body\nline"));
        assert_ne!(content_hash("A title", "body"), content_hash("A title", "other"));
    }

    #[test]
    fn the_verified_sql_names_the_sources_that_verify() {
        let listed: Vec<&str> = SourceType::ALL.iter().filter(|s| s.verifies()).map(SourceType::as_str).collect();
        let sql = listed.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ");
        assert!(MEMORY_COLUMNS.contains(&format!("e.source_type IN ({sql})")), "{MEMORY_COLUMNS}");
        assert!(MEMORY_COLUMNS.contains(&format!("e.relation = '{}'", Relation::VerifiedBy.as_str())));
    }

    /// The lexical tier runs its MATCH once and looks each hit's note up by rowid. Planned the
    /// other way round - the scope's notes outer, the MATCH re-run for every one of them, as
    /// SQLite 3.51 chose by itself - a 24-term association over 2,000 notes took 1.3 s.
    #[tokio::test]
    async fn the_lexical_tier_matches_once_and_looks_its_hits_up() {
        let dir = std::env::temp_dir().join(format!("swiss-store-fts-{}-{}", std::process::id(), swiss_core::util::random_hex(4)));
        let store = Store::open(&dir.join("store.sqlite3")).await.unwrap();
        let mut conn = store.read().await.unwrap();
        for statuses in [Vec::new(), crate::search::statuses(false), crate::search::statuses(true)] {
            let rows = sqlx::query(audited(format!("EXPLAIN QUERY PLAN {}", fts_sql(&statuses, 200))))
                .bind("\"redis\" OR \"key\"")
                .bind("p-00000001")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            let plan: Vec<String> = rows.iter().map(|row| row.try_get::<String, _>("detail").unwrap()).collect();
            assert!(plan.first().is_some_and(|step| step.starts_with("SCAN memory_fts VIRTUAL TABLE")), "{plan:?}");
            assert!(plan.iter().any(|step| step.contains("SEARCH m USING INTEGER PRIMARY KEY (rowid=?)")), "{plan:?}");
        }
        drop(conn);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn like_needles_escape_their_own_characters() {
        assert_eq!(like_escape("50%_of"), "50\\%\\_of");
        assert_eq!(like_escape("plain"), "plain");
    }

    /// A store written by the v5 build (ADR-032): every note gains its producer and transport,
    /// derived from the actor that wrote it, once; nothing else changes, and no evidence is
    /// invented.
    #[tokio::test]
    async fn a_v5_store_gains_provenance_from_its_actors() {
        use sqlx::Connection;
        let dir = std::env::temp_dir().join(format!("swiss-store-v5-{}-{}", std::process::id(), swiss_core::util::random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite3");
        let notes = [
            ("m-0000000001", "user", "panel", 1),
            ("m-0000000002", "user", "cli", 0),
            ("m-0000000003", "agent", "mcp:claude", 0),
            ("m-0000000004", "agent", "mcp", 0),
            ("m-0000000005", "agent", "sync", 0),
        ];
        {
            let opts = SqliteConnectOptions::new().filename(&path).create_if_missing(true).foreign_keys(true);
            let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
            sqlx::raw_sql("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_ms INTEGER NOT NULL)")
                .execute(&mut conn)
                .await
                .unwrap();
            for stmt in SCHEMA_V2.iter().chain(SCHEMA_V3).chain(SCHEMA_V4).chain(SCHEMA_V5).copied() {
                sqlx::raw_sql(stmt).execute(&mut conn).await.unwrap();
            }
            sqlx::raw_sql("ALTER TABLE memories ADD COLUMN domain_id TEXT REFERENCES memory_domains(id) ON DELETE SET NULL")
                .execute(&mut conn)
                .await
                .unwrap();
            sqlx::raw_sql("INSERT INTO schema_migrations VALUES (5, 0); INSERT INTO projects VALUES ('p-00000001', 'App', 0)")
                .execute(&mut conn)
                .await
                .unwrap();
            for (id, origin, actor, confirmed) in notes {
                sqlx::query(
                    "INSERT INTO memories (id, project_id, kind, title, title_norm, content, content_hash, status, origin, \
                     actor, confirmed, revision, created_ms, updated_ms) \
                     VALUES (?, 'p-00000001', 'decision', ?, ?, 'body', ?, 'active', ?, ?, ?, 1, 0, 0)",
                )
                .bind(id)
                .bind(id)
                .bind(id)
                .bind(content_hash(id, "body"))
                .bind(origin)
                .bind(actor)
                .bind(confirmed)
                .execute(&mut conn)
                .await
                .unwrap();
            }
            conn.close().await.unwrap();
        }
        let expect = [
            (Producer::Human, Transport::Panel, true),
            (Producer::Unknown, Transport::Cli, false),
            (Producer::Agent, Transport::Mcp, false),
            (Producer::Agent, Transport::Mcp, false),
            (Producer::Sync, Transport::Internal, false),
        ];
        for round in 0..2 {
            // The second open finds the columns there and derives nothing again.
            let store = Store::open(&path).await.unwrap();
            let mut conn = store.read().await.unwrap();
            let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations").fetch_one(&mut *conn).await.unwrap();
            assert_eq!(version, SCHEMA_VERSION);
            for ((id, _, actor, _), (producer, transport, confirmed)) in notes.iter().zip(expect) {
                let memory = memory(&mut conn, id).await.unwrap().unwrap();
                assert_eq!((memory.producer, memory.transport, memory.confirmed), (producer, transport, confirmed), "{actor}");
                assert_eq!(memory.actor, *actor, "the actor label is kept");
                assert!(!memory.source_verified);
            }
            let evidence: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_evidence").fetch_one(&mut *conn).await.unwrap();
            assert_eq!(evidence, 0, "round {round}: no evidence is invented");
            if round == 0 {
                // A producer set after the migration survives the next open.
                sqlx::query("UPDATE memories SET producer = 'bootstrap' WHERE id = 'm-0000000002'").execute(&mut *conn).await.unwrap();
            }
            drop(conn);
            store.pool.close().await;
            if round == 0 {
                let after = Store::open(&path).await.unwrap();
                let mut conn = after.read().await.unwrap();
                let kept = memory(&mut conn, "m-0000000002").await.unwrap().unwrap();
                assert_eq!(kept.producer, Producer::Bootstrap);
                sqlx::query("UPDATE memories SET producer = 'unknown' WHERE id = 'm-0000000002'").execute(&mut *conn).await.unwrap();
                drop(conn);
                after.pool.close().await;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
