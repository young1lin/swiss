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

//! Versioned schema steps for module history databases (SPEC §host.history).
//!
//! One step's DDL and its version stamp commit in the SAME transaction, so a crash between
//! them can leave neither applied-and-unstamped nor stamped-but-unapplied. An unknown NEWER
//! version is an explicit owner error that leaves the store untouched — never a drop-and-
//! recreate that would erase authoritative history.

use sqlx::sqlite::SqlitePool;
use sqlx::Row;

/// The one row `meta` keeps about the schema itself.
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// The base table every history database carries from version 0: identity and policy state.
/// Step 0 runs inside the same transaction discipline as every later step.
pub const BASE_DDL: &str = "CREATE TABLE IF NOT EXISTS meta (\
key TEXT PRIMARY KEY, \
value TEXT NOT NULL)";

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("the store carries schema v{found}, newer than this build's v{known}; refusing to touch it")]
    NewerSchema { found: i64, known: i64 },
    #[error("schema step {step} (to v{to}) failed: {source}")]
    StepFailed {
        step: usize,
        to: i64,
        source: sqlx::Error,
    },
}

/// One migration step: the DDL statements that move the store to `to_version`, applied in
/// order inside one transaction together with the version stamp.
pub struct SchemaStep {
    pub to_version: i64,
    /// Each statement runs as one `execute`; a failure of any rolls the whole step back.
    pub ddl: &'static [&'static str],
}

impl SchemaStep {
    pub const fn new(to_version: i64, ddl: &'static [&'static str]) -> Self {
        SchemaStep { to_version, ddl }
    }
}

/// Read the store's schema version without mutating anything. `None` when the database is
/// new (no `meta` table yet).
pub async fn schema_version(pool: &SqlitePool) -> Result<Option<i64>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    version_on(&mut conn).await
}

async fn version_on(conn: &mut sqlx::SqliteConnection) -> Result<Option<i64>, sqlx::Error> {
    let has_meta: i64 = sqlx::query(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
    )
    .fetch_one(&mut *conn)
    .await?
    .try_get(0)?;
    if has_meta == 0 {
        return Ok(None);
    }
    let row = sqlx::query("SELECT value FROM meta WHERE key = ?")
        .bind(SCHEMA_VERSION_KEY)
        .fetch_optional(&mut *conn)
        .await?;
    row.map(|r| {
        let text: String = r.try_get(0)?;
        text.parse::<i64>().map_err(|e| {
            sqlx::Error::ColumnDecode {
                index: SCHEMA_VERSION_KEY.into(),
                source: Box::new(e),
            }
        })
    })
    .transpose()
}

/// Bring the store up to the latest step. Steps must be ordered by increasing `to_version`;
/// step 0 (the base table, version 0) is created here when the database is new.
///
/// Invariants this function is the only writer of:
/// - a step's DDL and its stamp commit together, or not at all;
/// - a version strictly newer than the last known step is refused with the store untouched;
/// - applied steps never run twice (the stamp gates each one).
pub async fn migrate(pool: &SqlitePool, steps: &[SchemaStep]) -> Result<i64, SchemaError> {
    let known = steps.last().map(|s| s.to_version).unwrap_or(0);
    let mut current = schema_version(pool).await.map_err(|source| SchemaError::StepFailed {
        step: 0,
        to: 0,
        source,
    })?;
    if let Some(found) = current.filter(|found| *found > known) {
        return Err(SchemaError::NewerSchema { found, known });
    }

    let base = SchemaStep::new(0, &[BASE_DDL]);
    for (i, step) in std::iter::once((0, &base)).chain(steps.iter().enumerate()) {
        if current.is_some_and(|v| v >= step.to_version) {
            continue;
        }
        // Serialize writers BEFORE reading the stamp. A peer may have applied this
        // step since our initial read; the locked re-read skips it without retrying
        // an ALTER or borrowing a second connection from a single-connection pool.
        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|source| SchemaError::StepFailed { step: i, to: step.to_version, source })?;
        current = version_on(&mut tx).await.map_err(|source| SchemaError::StepFailed {
            step: i, to: step.to_version, source,
        })?;
        if let Some(found) = current.filter(|found| *found > known) {
            return Err(SchemaError::NewerSchema { found, known });
        }
        if current.is_some_and(|v| v >= step.to_version) {
            tx.rollback().await.map_err(|source| SchemaError::StepFailed {
                step: i, to: step.to_version, source,
            })?;
            continue;
        }
        // DDL and stamp still commit together; a failed step rolls back completely.
        for ddl in step.ddl {
            sqlx::query(*ddl).execute(&mut *tx).await.map_err(|source| SchemaError::StepFailed {
                step: i, to: step.to_version, source,
            })?;
        }
        stamp(&mut tx, step.to_version)
            .await
            .map_err(|source| SchemaError::StepFailed { step: i, to: step.to_version, source })?;
        tx.commit().await.map_err(|source| SchemaError::StepFailed {
            step: i,
            to: step.to_version,
            source,
        })?;
        current = Some(step.to_version);
    }
    Ok(current.unwrap_or(0))
}

async fn stamp(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO meta(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(SCHEMA_VERSION_KEY)
        .bind(version.to_string())
        .execute(&mut **tx)
        .await
        .map(|_| ())
}
