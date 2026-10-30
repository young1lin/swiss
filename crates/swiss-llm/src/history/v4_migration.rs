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

use super::*;

/// A home at the OLD v3 shape: the first three schema steps' DDL, the
/// version stamped 3, and whatever rows the caller seeds - built through
/// a plain sqlite pool so the store's own open migrates it for real.
async fn v3_home(tag: &str, seed_calls: i64) -> std::path::PathBuf {
    let dir = swiss_core::paths::test_home().join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let url = format!("sqlite://{}?mode=rwc", dir.join("llm.db").to_string_lossy());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("v3 pool");
    // The host base schema (the meta table's DDL) exists BEFORE any
    // migration step: a real v3 home has it, so the scratch database
    // must too - only then does the version stamp describe an actual
    // old home instead of inventing a table the store never saw.
    sqlx::query(swiss_host::history::schema::BASE_DDL)
        .execute(&pool)
        .await
        .expect("base ddl");
    // LLM_SCHEMA is &[SchemaStep] and ddl is &'static [&'static str]:
    // destructure the element so the query sees the &str literal itself,
    // never a double reference.
    for step in &LLM_SCHEMA[..3] {
        for &ddl in step.ddl {
            sqlx::query(ddl).execute(&pool).await.expect("v3 ddl");
        }
    }
    sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?)")
        .bind("schema_version")
        .bind("3")
        .execute(&pool)
        .await
        .expect("stamp");
    // The two-handles witness shape: TWO open conversation rows of one
    // agent, one closed row, and (for the canonical variant) a call row
    // pointing at the first open run with no replay-bearing body.
    let open_at = |n: i64| format!("2026-01-0{n}T00:00:00Z");
    for n in 1..=2 {
        sqlx::query(
            "INSERT INTO run (at, at_end, agent, runtime, provider, model, ok, ms, turns, kind)
             VALUES (?, NULL, 'alpha', 'builtin', 'glm', 'glm-5.3', NULL, 0, 0, 1)",
        )
        .bind(open_at(n))
        .execute(&pool)
        .await
        .expect("open row");
    }
    sqlx::query(
        "INSERT INTO run (at, at_end, agent, runtime, provider, model, ok, ms, turns, kind)
         VALUES (?, '2026-01-03T00:00:00Z', 'alpha', 'builtin', 'glm', 'glm-5.3', 1, 5, 1, 1)",
    )
    .bind(open_at(3))
    .execute(&pool)
    .await
    .expect("closed row");
    for _ in 0..seed_calls {
        sqlx::query(
            "INSERT INTO call (at, kind, provider, model, agent, ok, ms, preview_text, chars, run_id)
             VALUES (?, 'chat', 'glm', 'glm-5.3', 'alpha', 1, 5, 'old', 3, 1)",
        )
        .bind(open_at(1))
        .execute(&pool)
        .await
        .expect("call row");
    }
    pool.close().await;
    dir
}

#[tokio::test]
async fn duplicate_old_open_rows_migrate_without_loss_and_never_count_continuable() {
    let dir = v3_home("llm-hist-v4-dup", 0).await;
    // The migration itself must open cleanly: the one-open rule is enforced
    // at WRITE time (a conditional insert), never by a unique index, so
    // duplicate old-open rows cannot fail it.
    let history = LlmHistory::open(&dir).await.expect("the duplicate old-open home migrates");
    history.flush().await;
    let pending: Vec<i64> = history
        .store
        .query(|pool| {
            Box::pin(async move {
                let rows = sqlx::query(
                    "SELECT id, pending FROM run WHERE agent = 'alpha' AND at_end IS NULL ORDER BY id",
                )
                .fetch_all(&pool)
                .await?;
                Ok::<Vec<i64>, sqlx::Error>(
                    rows.iter().map(|row| row.try_get::<i64, _>("pending").unwrap_or(-2)).collect(),
                )
            })
        })
        .await
        .expect("read");
    assert_eq!(pending.len(), 2, "both duplicate open rows survive: {pending:?}");
    assert!(pending.iter().all(|one| *one == -1), "every old-open row is marked ambiguous: {pending:?}");
    // Their continuation fails closed with the honest one-time policy, and
    // the open listing still shows them (audit stays readable).
    for run_id in 1..=2 {
        assert!(matches!(history.replay_state(run_id).await, ReplayState::Refused(_)), "run {run_id} refuses");
    }
    let listed = history.read_runs(0, 10, Some("alpha".to_string()), Some(true)).await.expect("list");
    assert_eq!(listed["runs"].as_array().map(Vec::len), Some(2), "the audit rows stay readable");
    // The outward one-open contract counts CONTINUABLE rows only: none
    // here, and a fresh conversation of the same agent is not blocked.
    assert!(history.continuable_open("alpha").await.is_none());
    let run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let fresh = run.id().expect("a stale ambiguous row never wedges the agent");
    assert_eq!(history.continuable_open("alpha").await, Some(fresh), "exactly one continuable row now");
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_canonical_old_open_run_with_activity_refuses_continuation() {
    let dir = v3_home("llm-hist-v4-canonical", 1).await;
    let history = LlmHistory::open(&dir).await.expect("migrates");
    history.flush().await;
    // HONEST fixture scope: the migration marks every old-open run -1
    // BEFORE any body question, so this refusal proves the CONSERVATIVE
    // old-open migration itself - body presence is never reached, and
    // the bare call row only records that rows of any shape ride the
    // same one-time policy. Fail closed, never a note parse; every row
    // stays preserved.
    assert!(matches!(history.replay_state(1).await, ReplayState::Refused(_)));
    assert!(history.continuable_open("alpha").await.is_none());
    history.close().await.expect("close");
}

#[tokio::test]
async fn a_fresh_home_keeps_the_reserved_unused_positive() {
    let dir = swiss_core::paths::test_home().join("llm-hist-v4-fresh");
    let _ = std::fs::remove_dir_all(&dir);
    let history = LlmHistory::open(&dir).await.expect("open");
    let run = history.open_conversation("alpha", "builtin", Some("glm"), Some("glm-5.3")).await;
    let id = run.id().expect("reserved");
    // A reservation is not activity: the run is continuable as its own
    // first send, exactly the fresh-unused positive.
    assert!(matches!(history.replay_state(id).await, ReplayState::Fresh));
    assert_eq!(history.continuable_open("alpha").await, Some(id));
    history.close().await.expect("close");
}
