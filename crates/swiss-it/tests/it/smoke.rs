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

//! The I0 smokes (SPEC §testing.it): one real round trip per engine through the engine
//! table. These prove resolution, readiness and - together with the process exiting -
//! the exit-hook cleanup story end to end; the L1/L2/L3 suites hang off the same
//! `engine()` calls.

use sqlx::Connection;
use swiss_it::engine::{engine, Kind};

#[tokio::test]
async fn mysql_answers_select_one() {
    let e = engine(Kind::Mysql).await;
    let mut conn = sqlx::MySqlConnection::connect(&e.root_url)
        .await
        .expect("the mysql engine's root URL connects");
    let one: i64 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut conn)
        .await
        .expect("SELECT 1 answers");
    assert_eq!(one, 1);
}

#[tokio::test]
async fn postgres_answers_select_one() {
    let e = engine(Kind::Postgres).await;
    let mut conn = sqlx::PgConnection::connect(&e.root_url)
        .await
        .expect("the postgres engine's root URL connects");
    // A bare literal is INT4 in postgres; decode what it actually is.
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut conn)
        .await
        .expect("SELECT 1 answers");
    assert_eq!(one, 1);
}

#[tokio::test]
async fn redis_answers_ping() {
    let e = engine(Kind::Redis).await;
    let client = redis::Client::open(e.root_url.as_str()).expect("the redis URL parses");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("the redis engine's URL connects");
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("PING answers");
    assert_eq!(pong, "PONG");

    // The reaper acceptance test (reaper.rs) runs this one test in a child process
    // with SWISS_IT_FORCE_RED=1 to prove a RED run still removes its containers:
    // libtest's failure path is std::process::exit(101), which on Windows is
    // ExitProcess and skips the CRT atexit hook entirely. Nothing else sets this
    // variable and nothing else may - it exists for exactly that one test.
    if std::env::var_os("SWISS_IT_FORCE_RED").is_some() {
        panic!("SWISS_IT_FORCE_RED is set: the deliberate red for the reaper test");
    }
}
