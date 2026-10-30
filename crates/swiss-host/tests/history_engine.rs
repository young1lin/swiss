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

//! E1 — the linked history engine: the WAL-reset gate and the capability probe
//! (SPEC §arch.deps, the engine policy this suite enforces).
//!
//! The scratch home is installed before any constructor (SPEC §testing.rules): the
//! file-database leg opens only paths under `swiss_core::paths::test_home()`, never the
//! operator's home, and the probe itself is in-memory and disposable.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use swiss_core::paths::test_home;
use swiss_host::history::engine::{self, EngineCaps};

fn caps_for(version: &str) -> EngineCaps {
    EngineCaps {
        version: version.to_string(),
        source_id: String::new(),
        fts5: true,
        trigram: true,
        contentless_delete: true,
        incremental_vacuum: true,
    }
}

#[tokio::test]
async fn engine_rejects_unpatched_wal_before_import() {
    // The affected line is 3.7.0 through 3.51.2 (sqlite.org/wal.html#walresetbug);
    // 3.46.0 — what the workspace locked before the alignment — is inside it.
    for affected in ["3.7.0", "3.44.0", "3.46.0", "3.50.2", "3.51.1", "3.51.2"] {
        assert!(
            !engine::wal_reset_fixed(affected),
            "{affected} is in the affected line and must not pass the WAL gate"
        );
        assert!(
            caps_for(affected).wal_ready().is_err(),
            "{affected} must be refused before any WAL open or import"
        );
    }
    // Fixed: 3.51.3 itself, any later release, and the two official backports.
    for fixed in ["3.44.6", "3.50.7", "3.51.3", "3.52.0", "4.0.0"] {
        assert!(engine::wal_reset_fixed(fixed), "{fixed} carries the fix");
        assert!(caps_for(fixed).wal_ready().is_ok());
    }
    // The gate fails closed on anything it cannot parse as a release triple.
    for malformed in ["", "3", "3.51", "3.51.3.1", "three.fifty.one", "3.51.x"] {
        assert!(
            !engine::wal_reset_fixed(malformed),
            "{malformed:?} must not pass the WAL gate"
        );
    }

    // The linked engine — the one the shipping exe carries — must pass on its observed
    // identity, not on the lockfile's say-so. This is the assertion that went red while the
    // workspace still locked libsqlite3-sys 0.30.1 (SQLite 3.46.0, affected).
    let caps = engine::probe().await.expect("probe the linked engine");
    caps.wal_ready().expect("the linked SQLite must be 3.51.3+ / a reviewed backport");
    caps.history_ready().expect("the linked engine must carry FTS5 trigram, contentless delete and incremental vacuum");
}

#[tokio::test]
async fn engine_probes_contentless_delete_and_trigram() {
    let caps = engine::probe().await.expect("probe the linked engine");
    assert!(caps.fts5, "FTS5 must be compiled in (found: {})", caps.version);
    assert!(caps.trigram, "the trigram tokenizer must find substrings");
    assert!(caps.contentless_delete, "contentless FTS5 tables must accept deletes");
    assert!(caps.incremental_vacuum, "incremental vacuum must work");

    // The same capabilities on a scratch FILE database — the shape an owner store uses —
    // opened only under the scratch home installed before this pool existed.
    let logs = test_home().join("logs");
    std::fs::create_dir_all(&logs).expect("create the scratch logs dir");
    let db = logs.join("engine-probe.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool: SqlitePool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await
        .expect("open the scratch database");

    // WAL must actually engage on the fixed engine: journal_mode reads back "wal".
    let mode: String = sqlx::query("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .expect("read journal_mode")
        .try_get(0)
        .expect("journal_mode value");
    assert_eq!(mode.to_ascii_lowercase(), "wal", "the gate exists so WAL can be used");

    // Trigram + contentless-delete on disk: create, substring-match, delete, observe empty.
    sqlx::query("CREATE VIRTUAL TABLE probe_fts USING fts5(body, content='', tokenize='trigram', contentless_delete=1)")
        .execute(&pool)
        .await
        .expect("create the trigram contentless-delete table");
    sqlx::query("INSERT INTO probe_fts(rowid, body) VALUES (7, 'the quick swiss engine probe')")
        .execute(&pool)
        .await
        .expect("seed the probe table");
    let hit: i64 = sqlx::query("SELECT rowid FROM probe_fts WHERE probe_fts MATCH '\"iss eng\"'")
        .fetch_one(&pool)
        .await
        .expect("trigram phrase query")
        .try_get(0)
        .expect("rowid");
    assert_eq!(hit, 7, "a quoted trigram phrase must find the substring");
    sqlx::query("DELETE FROM probe_fts WHERE rowid = 7").execute(&pool).await
        .expect("delete from the contentless table");
    let left: i64 = sqlx::query("SELECT count(*) FROM probe_fts")
        .fetch_one(&pool)
        .await
        .expect("count after delete")
        .try_get(0)
        .expect("count");
    assert_eq!(left, 0, "the delete must be visible through the index");

    pool.close().await;
}
