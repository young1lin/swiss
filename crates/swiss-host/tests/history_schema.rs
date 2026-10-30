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

//! E2 — host schema tests (SPEC §host.history, planned): a step's DDL and its version
//! stamp commit together, and an unknown NEWER version leaves the store untouched.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use swiss_core::paths::test_home;
use swiss_host::history::schema::{migrate, schema_version, SchemaError, SchemaStep};

static GOOD_V1: &[SchemaStep] = &[SchemaStep::new(1, &[
    "CREATE TABLE keep (id INTEGER PRIMARY KEY, text TEXT NOT NULL)",
])];

/// A step whose second statement cannot run: the first created a table, the second hits a
/// table that does not exist. This is the crash-between-DDL-and-stamp shape, made
/// deterministic without fault injection machinery.
static HALF_BAD_V2: &[SchemaStep] = &[
    SchemaStep::new(1, &["CREATE TABLE keep (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"]),
    SchemaStep::new(2, &[
        "CREATE TABLE step2 (x)",
        "INSERT INTO missing_table VALUES (1)",
    ]),
];

async fn scratch_pool(name: &str) -> SqlitePool {
    let logs = test_home().join("logs");
    std::fs::create_dir_all(&logs).expect("create the scratch logs dir");
    let opts = SqliteConnectOptions::new()
        .filename(logs.join(name))
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("open the scratch pool")
}

#[tokio::test]
async fn schema_step_and_version_commit_together() {
    let pool = scratch_pool("schema-atomic.db").await;
    migrate(&pool, GOOD_V1).await.expect("first migration applies");

    // A step that fails halfway: the version must stay at 1 AND the half-applied DDL must
    // be gone — neither applied-but-unstamped nor stamped-but-unapplied may survive.
    let err = migrate(&pool, HALF_BAD_V2).await.unwrap_err();
    assert!(
        matches!(err, SchemaError::StepFailed { step: 1, .. }),
        "the failing step is reported: {err}"
    );

    // A FRESH connection to the same file: what survived is what committed.
    let path = pool.connect_options().get_filename().to_path_buf();
    pool.close().await;
    let reopened = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .expect("reopen the scratch pool");
    assert_eq!(schema_version(&reopened).await.unwrap(), Some(1), "the stamp never moved");
    let step2: i64 = sqlx::query("SELECT count(*) FROM sqlite_master WHERE name = 'step2'")
        .fetch_one(&reopened)
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(step2, 0, "the half-applied DDL rolled back with the failed stamp");

    // And migration is resumable: a fixed step 2 applies cleanly on retry.
    static FIXED_V2: &[SchemaStep] = &[
        SchemaStep::new(1, &["CREATE TABLE keep (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"]),
        SchemaStep::new(2, &["CREATE TABLE step2 (x)", "INSERT INTO step2 VALUES (1)"]),
    ];
    migrate(&reopened, FIXED_V2).await.expect("retry applies");
    assert_eq!(schema_version(&reopened).await.unwrap(), Some(2));
    reopened.close().await;
}

#[tokio::test]
async fn unknown_schema_leaves_store_untouched() {
    let pool = scratch_pool("schema-newer.db").await;
    migrate(&pool, GOOD_V1).await.expect("first migration applies");
    sqlx::query("INSERT INTO keep (text) VALUES ('authoritative')")
        .execute(&pool)
        .await
        .unwrap();

    // A future build stamped a newer version; this build must refuse without dropping or
    // recreating anything.
    sqlx::query("UPDATE meta SET value = '99' WHERE key = 'schema_version'")
        .execute(&pool)
        .await
        .unwrap();
    let err = migrate(&pool, GOOD_V1).await.unwrap_err();
    match err {
        SchemaError::NewerSchema { found: 99, known: 1 } => {}
        other => panic!("expected the newer-schema refusal, got: {other}"),
    }

    let rows: i64 = sqlx::query("SELECT count(*) FROM keep")
        .fetch_one(&pool)
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(rows, 1, "the authoritative record survived the refusal");
    assert_eq!(
        schema_version(&pool).await.unwrap(),
        Some(99),
        "the refusal did not rewrite the stamp"
    );
    pool.close().await;
}

#[tokio::test]
async fn duplicate_columns_report_the_schema_error_without_exhausting_the_pool() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_millis(100))
        .connect("sqlite::memory:")
        .await
        .expect("open a single-connection pool");
    migrate(&pool, GOOD_V1).await.expect("v1");
    sqlx::query("ALTER TABLE keep ADD COLUMN owner_pid INTEGER")
        .execute(&pool).await.expect("the column already exists without a v2 stamp");
    static DUPLICATE_V2: &[SchemaStep] = &[
        SchemaStep::new(1, &["CREATE TABLE keep (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"]),
        SchemaStep::new(2, &["ALTER TABLE keep ADD COLUMN owner_pid INTEGER"]),
    ];
    let err = migrate(&pool, DUPLICATE_V2).await.expect_err("an unstamped duplicate is not a completed migration");
    assert!(
        matches!(&err, SchemaError::StepFailed { source: sqlx::Error::Database(_), .. }),
        "the database error must survive, rather than waiting for our own connection: {err:?}",
    );
    assert_eq!(schema_version(&pool).await.unwrap(), Some(1));
    pool.close().await;
}

#[tokio::test]
async fn concurrent_migrations_recheck_the_stamp_under_the_write_lock() {
    static V2: &[SchemaStep] = &[
        SchemaStep::new(1, &["CREATE TABLE keep (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"]),
        SchemaStep::new(2, &[
            "ALTER TABLE keep ADD COLUMN owner_pid INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE keep ADD COLUMN owner_created INTEGER NOT NULL DEFAULT 0",
        ]),
    ];
    for round in 0..8 {
        let a = scratch_pool(&format!("schema-concurrent-{}-{round}.db", std::process::id())).await;
        let path = a.connect_options().get_filename().to_path_buf();
        let b = SqlitePoolOptions::new().max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(1))
            .connect_with(SqliteConnectOptions::new().filename(path))
            .await.expect("the peer opens the same database");
        // Includes the first-open race: neither meta nor the v1 table exists yet.
        let (first, second) = tokio::join!(migrate(&a, V2), migrate(&b, V2));
        assert_eq!(first.expect("first migrator"), 2);
        assert_eq!(second.expect("peer migrator"), 2);
        assert_eq!(schema_version(&a).await.unwrap(), Some(2));
        let _: (i64, i64) = sqlx::query_as("SELECT owner_pid, owner_created FROM keep LIMIT 1")
            .fetch_optional(&a).await.expect("both columns exist").unwrap_or((0, 0));
        a.close().await;
        b.close().await;
    }
}
