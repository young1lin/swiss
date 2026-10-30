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

//! The code index's own SQLite file (SPEC §memory.code): `code.sqlite3`, beside the memory
//! store and never joined to it. Everything in it is derived from a checkout's files, so a
//! schema this build does not know is dropped and built again, never migrated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};
use sqlx::Row;

use crate::code::extract::{Facts, EXTRACTOR};
use crate::error::{MemError, MemResult};

/// The index's schema; any other version is dropped and rebuilt. 2: a reference says
/// whether only a comment makes it.
pub const CODE_SCHEMA: i64 = 2;
/// Rows one multi-row INSERT carries (two or three variables each, well under SQLite's limit).
const ROWS_PER_INSERT: usize = 400;

const TABLES: &str = "
CREATE TABLE IF NOT EXISTS code_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS code_roots (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL UNIQUE,
    indexed_ms INTEGER NOT NULL,
    listed INTEGER NOT NULL,
    truncated INTEGER NOT NULL,
    git INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS code_files (
    id INTEGER PRIMARY KEY,
    root_id INTEGER NOT NULL REFERENCES code_roots(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL,
    hash TEXT NOT NULL,
    lang TEXT,
    skipped TEXT,
    UNIQUE (root_id, path)
);
CREATE TABLE IF NOT EXISTS code_names (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS code_defs (
    file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
    name_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    PRIMARY KEY (file_id, line, name_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS code_defs_name ON code_defs(name_id);
CREATE TABLE IF NOT EXISTS code_refs (
    file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
    name_id INTEGER NOT NULL,
    comment INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (file_id, name_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS code_refs_name ON code_refs(name_id);
CREATE TABLE IF NOT EXISTS code_imports (
    file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
    spec TEXT NOT NULL,
    line INTEGER NOT NULL,
    PRIMARY KEY (file_id, spec)
) WITHOUT ROWID;
";

const DROP: &str = "
DROP TABLE IF EXISTS code_imports;
DROP TABLE IF EXISTS code_refs;
DROP TABLE IF EXISTS code_defs;
DROP TABLE IF EXISTS code_names;
DROP TABLE IF EXISTS code_files;
DROP TABLE IF EXISTS code_roots;
DELETE FROM code_meta;
";

/// The definition kinds [crate::code::extract] yields, so a row reads back as a `&'static str`.
const KINDS: [&str; 17] = [
    "fn", "method", "struct", "enum", "trait", "type", "mod", "const", "static", "union", "macro", "class", "interface",
    "var", "namespace", "record", "object",
];

/// Mark a built SQL string as audited (sqlx 0.9's SqlSafeStr gate): every fragment this
/// file interpolates is a table name of its own or a run of `?` marks - names and paths
/// reach the database only as bound parameters.
fn audited(sql: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql)
}

pub fn kind_of(text: &str) -> &'static str {
    KINDS.iter().find(|k| **k == text).copied().unwrap_or("other")
}

/// The index's file handle: one connection, like the store's.
pub struct Db {
    pub pool: SqlitePool,
    pub path: PathBuf,
}

impl Db {
    /// Open (creating if absent) the index at `path`; a schema of another version is dropped.
    pub async fn open(path: &Path) -> MemResult<Db> {
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
        let mut conn = pool.acquire().await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS code_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)").execute(&mut *conn).await?;
        let version: Option<String> =
            sqlx::query_scalar("SELECT value FROM code_meta WHERE key = 'schema'").fetch_optional(&mut *conn).await?;
        if version.as_deref() != Some(CODE_SCHEMA.to_string().as_str()) {
            sqlx::raw_sql(DROP).execute(&mut *conn).await?;
            sqlx::raw_sql(TABLES).execute(&mut *conn).await?;
            sqlx::query("INSERT INTO code_meta (key, value) VALUES ('schema', ?)")
                .bind(CODE_SCHEMA.to_string())
                .execute(&mut *conn)
                .await?;
        }
        // Facts another scan read are stale, not wrong: each file is read again on its root's
        // next refresh - due at once - and what it held answers until then.
        let extractor: Option<String> =
            sqlx::query_scalar("SELECT value FROM code_meta WHERE key = 'extractor'").fetch_optional(&mut *conn).await?;
        if extractor.as_deref() != Some(EXTRACTOR.to_string().as_str()) {
            sqlx::query("UPDATE code_files SET size = -1, mtime_ms = -1, hash = ''").execute(&mut *conn).await?;
            sqlx::query("INSERT INTO code_meta (key, value) VALUES ('extractor', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
                .bind(EXTRACTOR.to_string())
                .execute(&mut *conn)
                .await?;
        }
        drop(conn);
        Ok(Db { pool, path: path.to_path_buf() })
    }
}

/// One indexed checkout.
#[derive(Debug, Clone)]
pub struct RootRow {
    pub id: i64,
    pub indexed_ms: i64,
    pub listed: i64,
    pub truncated: bool,
    pub git: bool,
    /// A file waits to be read again by the scan this build runs: a refresh is due.
    pub stale: bool,
}

/// What the index holds of one file, to compare a new stat with.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub size: i64,
    pub mtime_ms: i64,
    pub hash: String,
}

pub async fn root(conn: &mut SqliteConnection, root: &str) -> MemResult<Option<RootRow>> {
    let row = sqlx::query(
        "SELECT id, indexed_ms, listed, truncated, git, \
         EXISTS (SELECT 1 FROM code_files f WHERE f.root_id = code_roots.id AND f.hash = '') AS stale \
         FROM code_roots WHERE root = ?",
    )
        .bind(root)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(match row {
        Some(row) => Some(RootRow {
            id: row.try_get("id")?,
            indexed_ms: row.try_get("indexed_ms")?,
            listed: row.try_get("listed")?,
            truncated: row.try_get::<i64, _>("truncated")? != 0,
            git: row.try_get::<i64, _>("git")? != 0,
            stale: row.try_get::<i64, _>("stale")? != 0,
        }),
        None => None,
    })
}

/// The root's row, created empty (indexed at 0) when absent.
pub async fn ensure_root(conn: &mut SqliteConnection, root: &str) -> MemResult<i64> {
    sqlx::query("INSERT INTO code_roots (root, indexed_ms, listed, truncated, git) VALUES (?, 0, 0, 0, 0) ON CONFLICT(root) DO NOTHING")
        .bind(root)
        .execute(&mut *conn)
        .await?;
    Ok(sqlx::query_scalar("SELECT id FROM code_roots WHERE root = ?").bind(root).fetch_one(&mut *conn).await?)
}

pub async fn stamp_root(conn: &mut SqliteConnection, id: i64, now: i64, listed: usize, truncated: bool, git: bool) -> MemResult<()> {
    sqlx::query("UPDATE code_roots SET indexed_ms = ?, listed = ?, truncated = ?, git = ? WHERE id = ?")
        .bind(now)
        .bind(listed as i64)
        .bind(truncated as i64)
        .bind(git as i64)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn files(conn: &mut SqliteConnection, root_id: i64) -> MemResult<HashMap<String, FileRow>> {
    let rows = sqlx::query("SELECT id, path, size, mtime_ms, hash FROM code_files WHERE root_id = ?")
        .bind(root_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        out.insert(
            row.try_get("path")?,
            FileRow { id: row.try_get("id")?, size: row.try_get("size")?, mtime_ms: row.try_get("mtime_ms")?, hash: row.try_get("hash")? },
        );
    }
    Ok(out)
}

/// The file's row written (inserted or replaced) with no facts under it: its id.
pub async fn put_file(
    conn: &mut SqliteConnection,
    root_id: i64,
    path: &str,
    stat: (i64, i64),
    hash: &str,
    lang: Option<&str>,
    skipped: Option<&str>,
) -> MemResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO code_files (root_id, path, size, mtime_ms, hash, lang, skipped) VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(root_id, path) DO UPDATE SET size = excluded.size, mtime_ms = excluded.mtime_ms, hash = excluded.hash, \
         lang = excluded.lang, skipped = excluded.skipped RETURNING id",
    )
    .bind(root_id)
    .bind(path)
    .bind(stat.0)
    .bind(stat.1)
    .bind(hash)
    .bind(lang)
    .bind(skipped)
    .fetch_one(&mut *conn)
    .await?;
    for table in ["code_defs", "code_refs", "code_imports"] {
        sqlx::query(audited(format!("DELETE FROM {table} WHERE file_id = ?"))).bind(id).execute(&mut *conn).await?;
    }
    Ok(id)
}

/// A file whose bytes did not change: only its stat moves.
pub async fn touch_file(conn: &mut SqliteConnection, id: i64, stat: (i64, i64)) -> MemResult<()> {
    sqlx::query("UPDATE code_files SET size = ?, mtime_ms = ? WHERE id = ?")
        .bind(stat.0)
        .bind(stat.1)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn delete_file(conn: &mut SqliteConnection, id: i64) -> MemResult<()> {
    sqlx::query("DELETE FROM code_files WHERE id = ?").bind(id).execute(&mut *conn).await?;
    Ok(())
}

/// Name ids, interned in batches through a cache one run keeps.
#[derive(Default)]
pub struct Names {
    cache: HashMap<String, i64>,
}

impl Names {
    /// The ids of `names`, inserting those the index does not hold yet.
    async fn ids(&mut self, conn: &mut SqliteConnection, names: &[&str]) -> MemResult<Vec<i64>> {
        let mut missing: Vec<&str> = names.iter().copied().filter(|n| !self.cache.contains_key(*n)).collect();
        missing.sort_unstable();
        missing.dedup();
        for chunk in missing.chunks(ROWS_PER_INSERT) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let mut select = sqlx::query(audited(format!("SELECT id, name FROM code_names WHERE name IN ({marks})")));
            for name in chunk {
                select = select.bind(*name);
            }
            for row in select.fetch_all(&mut *conn).await? {
                self.cache.insert(row.try_get("name")?, row.try_get("id")?);
            }
            let fresh: Vec<&str> = chunk.iter().copied().filter(|n| !self.cache.contains_key(*n)).collect();
            if fresh.is_empty() {
                continue;
            }
            let values = vec!["(?)"; fresh.len()].join(", ");
            let sql = format!("INSERT INTO code_names (name) VALUES {values} RETURNING id, name");
            let mut insert = sqlx::query(audited(sql));
            for name in &fresh {
                insert = insert.bind(*name);
            }
            for row in insert.fetch_all(&mut *conn).await? {
                self.cache.insert(row.try_get("name")?, row.try_get("id")?);
            }
        }
        names
            .iter()
            .map(|n| self.cache.get(*n).copied().ok_or_else(|| MemError::store(format!("the code index lost the name {n}"))))
            .collect()
    }
}

/// Write one file's facts under its (fresh) row.
pub async fn put_facts(conn: &mut SqliteConnection, names: &mut Names, file_id: i64, facts: &Facts) -> MemResult<()> {
    let wanted: Vec<&str> = facts
        .defs
        .iter()
        .map(|d| d.name.as_str())
        .chain(facts.refs.iter().chain(&facts.comment_refs).map(String::as_str))
        .collect();
    let ids = names.ids(conn, &wanted).await?;
    let (def_ids, ref_ids) = ids.split_at(facts.defs.len());
    let (code_ids, comment_ids) = ref_ids.split_at(facts.refs.len());
    let refs: Vec<(i64, i64)> = code_ids.iter().map(|id| (*id, 0)).chain(comment_ids.iter().map(|id| (*id, 1))).collect();
    let defs: Vec<(i64, &str, i64)> = facts.defs.iter().zip(def_ids).map(|(d, id)| (*id, d.kind, d.line as i64)).collect();
    for chunk in defs.chunks(ROWS_PER_INSERT) {
        let values = vec!["(?, ?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!("INSERT OR IGNORE INTO code_defs (file_id, name_id, kind, line) VALUES {values}");
        let mut insert = sqlx::query(audited(sql));
        for (name_id, kind, line) in chunk {
            insert = insert.bind(file_id).bind(*name_id).bind(*kind).bind(*line);
        }
        insert.execute(&mut *conn).await?;
    }
    for chunk in refs.chunks(ROWS_PER_INSERT) {
        let values = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!("INSERT OR IGNORE INTO code_refs (file_id, name_id, comment) VALUES {values}");
        let mut insert = sqlx::query(audited(sql));
        for (name_id, comment) in chunk {
            insert = insert.bind(file_id).bind(*name_id).bind(*comment);
        }
        insert.execute(&mut *conn).await?;
    }
    for chunk in facts.imports.chunks(ROWS_PER_INSERT) {
        let values = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!("INSERT OR IGNORE INTO code_imports (file_id, spec, line) VALUES {values}");
        let mut insert = sqlx::query(audited(sql));
        for import in chunk {
            insert = insert.bind(file_id).bind(&import.spec).bind(import.line as i64);
        }
        insert.execute(&mut *conn).await?;
    }
    Ok(())
}

/// Names no definition or reference holds any more.
pub async fn prune_names(conn: &mut SqliteConnection) -> MemResult<u64> {
    let done = sqlx::query(
        "DELETE FROM code_names WHERE NOT EXISTS (SELECT 1 FROM code_defs d WHERE d.name_id = code_names.id) \
         AND NOT EXISTS (SELECT 1 FROM code_refs r WHERE r.name_id = code_names.id)",
    )
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected())
}

/// Delete one root and everything under it: the files it held.
pub async fn drop_root(conn: &mut SqliteConnection, root_id: i64) -> MemResult<u64> {
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM code_files WHERE root_id = ?").bind(root_id).fetch_one(&mut *conn).await?;
    sqlx::query("DELETE FROM code_roots WHERE id = ?").bind(root_id).execute(&mut *conn).await?;
    Ok(files as u64)
}

/// Delete every root: the files they held.
pub async fn drop_all(conn: &mut SqliteConnection) -> MemResult<u64> {
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM code_files").fetch_one(&mut *conn).await?;
    sqlx::query("DELETE FROM code_roots").execute(&mut *conn).await?;
    sqlx::query("DELETE FROM code_names").execute(&mut *conn).await?;
    Ok(files as u64)
}

/// The file's size on disk as SQLite sees it (its WAL included).
pub async fn bytes(conn: &mut SqliteConnection) -> MemResult<u64> {
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count").fetch_one(&mut *conn).await?;
    let size: i64 = sqlx::query_scalar("PRAGMA page_size").fetch_one(&mut *conn).await?;
    Ok((pages * size).max(0) as u64)
}

/// A root's counts: (files, skipped, defs, refs, imports, names).
pub async fn counts(conn: &mut SqliteConnection, root_id: i64) -> MemResult<[u64; 6]> {
    let row = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM code_files WHERE root_id = ?1) AS files, \
         (SELECT COUNT(*) FROM code_files WHERE root_id = ?1 AND skipped IS NOT NULL) AS skipped, \
         (SELECT COUNT(*) FROM code_defs d JOIN code_files f ON f.id = d.file_id WHERE f.root_id = ?1) AS defs, \
         (SELECT COUNT(*) FROM code_refs r JOIN code_files f ON f.id = r.file_id WHERE f.root_id = ?1) AS refs, \
         (SELECT COUNT(*) FROM code_imports i JOIN code_files f ON f.id = i.file_id WHERE f.root_id = ?1) AS imports, \
         (SELECT COUNT(*) FROM code_names) AS names",
    )
    .bind(root_id)
    .fetch_one(&mut *conn)
    .await?;
    let mut out = [0u64; 6];
    for (slot, column) in out.iter_mut().zip(["files", "skipped", "defs", "refs", "imports", "names"]) {
        *slot = row.try_get::<i64, _>(column)?.max(0) as u64;
    }
    Ok(out)
}

/// Where a root defines `name`: (path, kind, line), by path then line.
pub async fn defs_of(conn: &mut SqliteConnection, root_id: i64, name: &str) -> MemResult<Vec<(String, &'static str, u32)>> {
    let rows = sqlx::query(
        "SELECT f.path, d.kind, d.line FROM code_names n JOIN code_defs d ON d.name_id = n.id \
         JOIN code_files f ON f.id = d.file_id WHERE n.name = ? AND f.root_id = ? ORDER BY f.path, d.line",
    )
    .bind(name)
    .bind(root_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((row.try_get("path")?, kind_of(&row.try_get::<String, _>("kind")?), row.try_get::<i64, _>("line")? as u32));
    }
    Ok(out)
}

/// The files of a root that mention a name ([refs_of]).
pub struct Mentions {
    /// The first files, each with whether only a comment names it there: the files whose
    /// code names it before the others, each part by path.
    pub files: Vec<(String, bool)>,
    /// How many files mention it.
    pub total: u64,
    /// How many of those name it only in a comment.
    pub in_comments: u64,
}

impl Mentions {
    /// The listed files, whether their code or a comment names it.
    pub fn paths(self) -> Vec<String> {
        self.files.into_iter().map(|(path, _)| path).collect()
    }
}

/// The first `limit` files of a root that mention `name`, and how many do.
pub async fn refs_of(conn: &mut SqliteConnection, root_id: i64, name: &str, limit: usize) -> MemResult<Mentions> {
    let rows = sqlx::query(
        "SELECT f.path, r.comment FROM code_names n JOIN code_refs r ON r.name_id = n.id \
         JOIN code_files f ON f.id = r.file_id WHERE n.name = ? AND f.root_id = ? ORDER BY r.comment, f.path",
    )
    .bind(name)
    .bind(root_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut mentions = Mentions { files: Vec::with_capacity(rows.len().min(limit)), total: rows.len() as u64, in_comments: 0 };
    for row in rows {
        let comment = row.try_get::<i64, _>("comment")? != 0;
        mentions.in_comments += u64::from(comment);
        if mentions.files.len() < limit {
            mentions.files.push((row.try_get("path")?, comment));
        }
    }
    Ok(mentions)
}

/// The root's file `rel` names: itself, or the one file whose path ends in `/rel`.
pub async fn file_named(conn: &mut SqliteConnection, root_id: i64, rel: &str) -> MemResult<Option<String>> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT path FROM code_files WHERE root_id = ?1 AND (path = ?2 OR (length(path) > length(?2) \
         AND substr(path, length(path) - length(?2)) = '/' || ?2)) ORDER BY path = ?2 DESC, length(path) LIMIT 1",
    )
    .bind(root_id)
    .bind(rel)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(found)
}

/// Every path of a root (a glob anchor is tested against them).
pub async fn paths(conn: &mut SqliteConnection, root_id: i64) -> MemResult<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT path FROM code_files WHERE root_id = ? ORDER BY path").bind(root_id).fetch_all(&mut *conn).await?)
}

/// One file of a root, with what it says.
pub struct FileFacts {
    pub size: i64,
    pub hash: String,
    pub lang: Option<String>,
    pub skipped: Option<String>,
    pub defs: Vec<(String, &'static str, u32)>,
    pub imports: Vec<(String, u32)>,
    pub refs: u64,
}

pub async fn file(conn: &mut SqliteConnection, root_id: i64, path: &str) -> MemResult<Option<FileFacts>> {
    let Some(row) = sqlx::query("SELECT id, size, hash, lang, skipped FROM code_files WHERE root_id = ? AND path = ?")
        .bind(root_id)
        .bind(path)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let id: i64 = row.try_get("id")?;
    let mut defs = Vec::new();
    for def in sqlx::query(
        "SELECT n.name, d.kind, d.line FROM code_defs d JOIN code_names n ON n.id = d.name_id WHERE d.file_id = ? ORDER BY d.line, n.name",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    {
        defs.push((def.try_get("name")?, kind_of(&def.try_get::<String, _>("kind")?), def.try_get::<i64, _>("line")? as u32));
    }
    let mut imports = Vec::new();
    let sql = "SELECT spec, line FROM code_imports WHERE file_id = ? ORDER BY line, spec";
    for import in sqlx::query(sql).bind(id).fetch_all(&mut *conn).await? {
        imports.push((import.try_get("spec")?, import.try_get::<i64, _>("line")? as u32));
    }
    let refs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM code_refs WHERE file_id = ?").bind(id).fetch_one(&mut *conn).await?;
    Ok(Some(FileFacts {
        size: row.try_get("size")?,
        hash: row.try_get("hash")?,
        lang: row.try_get("lang")?,
        skipped: row.try_get("skipped")?,
        defs,
        imports,
        refs: refs.max(0) as u64,
    }))
}

// --- the survey's bulk reads (SPEC §memory.map) ------------------------------------------

/// One listed file as a survey reads it.
#[derive(Debug, Clone)]
pub struct SurveyFile {
    pub id: i64,
    pub path: String,
    pub lang: Option<String>,
    pub skipped: bool,
    pub size: i64,
}

/// Every file of a root, by path.
pub async fn survey_files(conn: &mut SqliteConnection, root_id: i64) -> MemResult<Vec<SurveyFile>> {
    let rows = sqlx::query("SELECT id, path, lang, skipped, size FROM code_files WHERE root_id = ? ORDER BY path")
        .bind(root_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(SurveyFile {
            id: row.try_get("id")?,
            path: row.try_get("path")?,
            lang: row.try_get("lang")?,
            skipped: row.try_get::<Option<String>, _>("skipped")?.is_some(),
            size: row.try_get("size")?,
        });
    }
    Ok(out)
}

/// Every definition of a root, with how many other files of the root mention its name:
/// (file id, name, kind, line, fan-in).
pub async fn survey_defs(conn: &mut SqliteConnection, root_id: i64) -> MemResult<Vec<(i64, String, &'static str, u32, u32)>> {
    let rows = sqlx::query(
        "SELECT d.file_id, n.name, d.kind, d.line, (SELECT COUNT(*) FROM code_refs r JOIN code_files rf ON rf.id = r.file_id \
         WHERE r.name_id = d.name_id AND rf.root_id = ?1 AND r.file_id != d.file_id) AS fanin \
         FROM code_defs d JOIN code_names n ON n.id = d.name_id JOIN code_files f ON f.id = d.file_id WHERE f.root_id = ?1",
    )
    .bind(root_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((
            row.try_get("file_id")?,
            row.try_get("name")?,
            kind_of(&row.try_get::<String, _>("kind")?),
            row.try_get::<i64, _>("line")?.max(0) as u32,
            row.try_get::<i64, _>("fanin")?.max(0) as u32,
        ));
    }
    Ok(out)
}

/// Rows [survey_uses] answers at most: a survey samples, it never needs every use.
const SURVEY_USES_MAX: i64 = 200_000;

/// Which file mentions a name that one or two other files of the root define (a name four
/// characters or longer - a short or common one says nothing about who uses whom):
/// (mentioning file id, defining file id, name).
pub async fn survey_uses(conn: &mut SqliteConnection, root_id: i64) -> MemResult<Vec<(i64, i64, String)>> {
    let rows = sqlx::query(
        "WITH picked AS (SELECT d.name_id FROM code_defs d JOIN code_files f ON f.id = d.file_id WHERE f.root_id = ?1 \
         GROUP BY d.name_id HAVING COUNT(DISTINCT d.file_id) <= 2) \
         SELECT r.file_id AS src, d.file_id AS dst, n.name FROM picked p JOIN code_names n ON n.id = p.name_id \
         JOIN code_defs d ON d.name_id = p.name_id JOIN code_files df ON df.id = d.file_id AND df.root_id = ?1 \
         JOIN code_refs r ON r.name_id = p.name_id JOIN code_files rf ON rf.id = r.file_id AND rf.root_id = ?1 \
         WHERE length(n.name) >= 4 AND r.file_id != d.file_id GROUP BY r.file_id, d.file_id, n.name LIMIT ?2",
    )
    .bind(root_id)
    .bind(SURVEY_USES_MAX)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((row.try_get("src")?, row.try_get("dst")?, row.try_get("name")?));
    }
    Ok(out)
}

/// Every import of a root: (file id, spec).
pub async fn survey_imports(conn: &mut SqliteConnection, root_id: i64) -> MemResult<Vec<(i64, String)>> {
    let rows = sqlx::query("SELECT i.file_id, i.spec FROM code_imports i JOIN code_files f ON f.id = i.file_id WHERE f.root_id = ?")
        .bind(root_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push((row.try_get("file_id")?, row.try_get("spec")?));
    }
    Ok(out)
}
