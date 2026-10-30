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

//! The one native SQLite engine module history may run on, and the gate that keeps an
//! unfixed engine out of WAL mode (SPEC §arch.deps).
//!
//! SQLite's WAL-reset race (https://sqlite.org/wal.html#walresetbug) corrupts databases,
//! rarely but really, when two connections write or checkpoint at the same instant on
//! releases 3.7.0 through 3.51.2. Module history keeps writer, query and maintenance
//! connections alive on the same owner database, so that race is a precondition, not a
//! tuning option: an owner calls [`EngineCaps::history_ready`] before it opens or imports a
//! history file, and the gate fails closed on any engine it cannot vouch for. The workspace
//! pins libsqlite3-sys 0.37.0 (bundled SQLite 3.51.3) so the gate passes; a backport may
//! replace it only through [`WAL_RESET_BACKPORTS`], with its reviewed source id recorded
//! beside the entry.

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection, Row};

/// The first SQLite release with the WAL-reset race fixed (2026-03-13).
const WAL_RESET_FIXED: (u64, u64, u64) = (3, 51, 3);

/// Releases older than `WAL_RESET_FIXED` that upstream fixed anyway — the two official
/// backports named in the WAL-reset notice. A new entry may only land together with the
/// reviewed upstream source id that justifies it; the gate never widens on version math
/// alone, because an intermediate release between backports can still be affected.
const WAL_RESET_BACKPORTS: &[(u64, u64, u64)] = &[(3, 44, 6), (3, 50, 7)];

/// `Some((major, minor, patch))` for a well-formed SQLite version string, `None` for
/// anything else. The gate treats unparseable versions as unfixed — it fails closed.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True when `version` names an SQLite release without the WAL-reset race: 3.51.3 or any
/// later release, or one of the reviewed backports.
pub fn wal_reset_fixed(version: &str) -> bool {
    let Some(v) = parse_version(version) else { return false };
    v >= WAL_RESET_FIXED || WAL_RESET_BACKPORTS.contains(&v)
}

/// What the linked SQLite engine is and can do, observed from the running library — never
/// inferred from the lockfile or a bundled header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCaps {
    /// `sqlite_version()`, e.g. "3.51.3".
    pub version: String,
    /// `sqlite_source_id()` — build date and checksum of the exact amalgamation.
    pub source_id: String,
    /// FTS5 compiled in (`PRAGMA compile_options` carries `ENABLE_FTS5`).
    pub fts5: bool,
    /// The trigram tokenizer works and finds substrings (the literal-search base).
    pub trigram: bool,
    /// Contentless FTS5 tables accept deletes (`contentless_delete=1`, SQLite >= 3.43).
    pub contentless_delete: bool,
    /// `auto_vacuum=incremental` plus `PRAGMA incremental_vacuum` work (bounded reclamation).
    pub incremental_vacuum: bool,
}

/// Why an engine may not carry module history. The WAL variant names the linked version so
/// the failure says what to align, not just "no".
#[derive(Debug, thiserror::Error)]
pub enum HistoryEngineError {
    #[error("linked SQLite {linked} still carries the WAL-reset race (fixed in 3.51.3; sqlite.org/wal.html#walresetbug)")]
    WalResetRace { linked: String },
    #[error("linked SQLite {linked} has no FTS5 with the trigram tokenizer: literal search cannot be built")]
    NoTrigram { linked: String },
    #[error("linked SQLite {linked} cannot delete from contentless FTS5 tables: index maintenance cannot be built")]
    NoContentlessDelete { linked: String },
    #[error("linked SQLite {linked} has no incremental vacuum: bounded reclamation cannot be built")]
    NoIncrementalVacuum { linked: String },
}

impl EngineCaps {
    /// The WAL gate alone: the linked release is free of the WAL-reset race. Every owner
    /// passes this before opening or importing a WAL-mode database.
    pub fn wal_ready(&self) -> Result<(), HistoryEngineError> {
        if wal_reset_fixed(&self.version) {
            Ok(())
        } else {
            Err(HistoryEngineError::WalResetRace { linked: self.version.clone() })
        }
    }

    /// The full precondition for module history: the WAL gate plus every capability the
    /// stores need. Checks the observed flags, not the version, so a capability question
    /// is never answered by version math.
    pub fn history_ready(&self) -> Result<(), HistoryEngineError> {
        self.wal_ready()?;
        if !self.fts5 || !self.trigram {
            return Err(HistoryEngineError::NoTrigram { linked: self.version.clone() });
        }
        if !self.contentless_delete {
            return Err(HistoryEngineError::NoContentlessDelete { linked: self.version.clone() });
        }
        if !self.incremental_vacuum {
            return Err(HistoryEngineError::NoIncrementalVacuum { linked: self.version.clone() });
        }
        Ok(())
    }
}

/// Probe the linked engine on a private in-memory connection: identity from
/// `sqlite_version()`/`sqlite_source_id()`, capabilities exercised with the statements the
/// history stores will actually run. The probe is disposable — it opens no file, leaves no
/// state behind, and may run at first use before any owner database exists.
pub async fn probe() -> Result<EngineCaps, sqlx::Error> {
    let opts = SqliteConnectOptions::new().in_memory(true);
    let mut conn = opts.connect().await?;
    let caps = probe_on(&mut conn).await;
    conn.close().await?;
    caps
}

async fn probe_on(conn: &mut SqliteConnection) -> Result<EngineCaps, sqlx::Error> {
    let row = sqlx::query("SELECT sqlite_version(), sqlite_source_id()")
        .fetch_one(&mut *conn)
        .await?;
    let version: String = row.try_get(0)?;
    let source_id: String = row.try_get(1)?;
    let compile_options = sqlx::query("PRAGMA compile_options")
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect::<Vec<_>>();
    let fts5 = compile_options.iter().any(|o| o == "ENABLE_FTS5");

    // Each capability is exercised with the statements the stores will issue, not inferred
    // from the version string: a distro build can lack FTS5 at any version.
    let trigram = fts5 && trigram_works(conn).await;
    let contentless_delete = fts5 && contentless_delete_works(conn).await;
    let incremental_vacuum = incremental_vacuum_works().await;

    Ok(EngineCaps { version, source_id, fts5, trigram, contentless_delete, incremental_vacuum })
}

/// Prove the trigram tokenizer by finding a substring a token-based index cannot express:
/// "iss eng" inside "the quick swiss engine probe", queried as a quoted phrase.
async fn trigram_works(conn: &mut SqliteConnection) -> bool {
    let created = sqlx::query(
        "CREATE VIRTUAL TABLE swiss_engine_trigram USING fts5(body, tokenize='trigram')",
    )
    .execute(&mut *conn)
    .await
    .is_ok();
    if !created {
        return false;
    }
    let found = async {
        sqlx::query("INSERT INTO swiss_engine_trigram(rowid, body) VALUES (1, 'the quick swiss engine probe')")
            .execute(&mut *conn)
            .await?;
        let row = sqlx::query(
            "SELECT rowid FROM swiss_engine_trigram WHERE swiss_engine_trigram MATCH '\"iss eng\"'",
        )
        .fetch_one(&mut *conn)
        .await?;
        row.try_get::<i64, _>(0)
    }
    .await
    .is_ok_and(|rowid| rowid == 1);
    let _ = sqlx::query("DROP TABLE swiss_engine_trigram").execute(&mut *conn).await;
    found
}

/// Prove contentless-delete by deleting from a `content='' && contentless_delete=1` table:
/// the DELETE is the statement older engines refuse ("cannot DELETE from contentless fts5
/// table"), and the count proves the row is gone rather than merely accepted.
async fn contentless_delete_works(conn: &mut SqliteConnection) -> bool {
    let created = sqlx::query(
        "CREATE VIRTUAL TABLE swiss_engine_cd USING fts5(body, content='', contentless_delete=1)",
    )
    .execute(&mut *conn)
    .await
    .is_ok();
    if !created {
        return false;
    }
    let worked = async {
        sqlx::query("INSERT INTO swiss_engine_cd(rowid, body) VALUES (1, 'swiss engine probe')")
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM swiss_engine_cd WHERE rowid = 1").execute(&mut *conn).await?;
        let row = sqlx::query("SELECT count(*) FROM swiss_engine_cd").fetch_one(&mut *conn).await?;
        row.try_get::<i64, _>(0)
    }
    .await
    .is_ok_and(|count| count == 0);
    let _ = sqlx::query("DROP TABLE swiss_engine_cd").execute(&mut *conn).await;
    worked
}

/// Prove incremental vacuum on a database of its own: `auto_vacuum = 2` sticks only on a
/// database that has never carried a table, so this leg gets a dedicated in-memory
/// connection — the FTS5 probes above have already created and dropped tables on the shared
/// one. Set the mode, create the first table, run `PRAGMA incremental_vacuum`, and read the
/// mode back: the readback is what makes this a capability observation instead of a pragma
/// echo.
async fn incremental_vacuum_works() -> bool {
    let Ok(mut conn) = SqliteConnectOptions::new().in_memory(true).connect().await else {
        return false;
    };
    let outcome = async {
        sqlx::query("PRAGMA auto_vacuum = 2").execute(&mut conn).await?;
        sqlx::query("CREATE TABLE swiss_engine_vac(x)").execute(&mut conn).await?;
        sqlx::query("PRAGMA incremental_vacuum(1)").execute(&mut conn).await?;
        let row = sqlx::query("PRAGMA auto_vacuum").fetch_one(&mut conn).await?;
        row.try_get::<i64, _>(0)
    }
    .await;
    let _ = conn.close().await;
    outcome.is_ok_and(|mode| mode == 2)
}
