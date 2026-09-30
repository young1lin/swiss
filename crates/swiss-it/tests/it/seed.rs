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

//! Seed guard tests (SPEC §testing.it): after a `fresh`, count every table, check every
//! column type, and verify the values the seeds exist to pin. A broken seed is a
//! broken fixture for every later test - these make it a loud, named failure.
//!
//! Connection strings are built FROM the def (the contract the adapters parse),
//! never from engine internals: if the def cannot connect, that is the bug.

use serde_json::Value;
use sqlx::Connection;
use swiss_it::engine::{engine, Kind};
use swiss_it::seed::{
    fresh, fresh_redis_exclusive, fresh_redis_many, fresh_redis_with_neighbor,
    hold_redis_indices, REDIS_SEED_KEYS,
};

fn mysql_url(def: &Value) -> String {
    format!(
        "mysql://{}:{}@{}:{}/{}",
        def["user"].as_str().expect("mysql def: user"),
        def["password"].as_str().expect("mysql def: password"),
        def["host"].as_str().expect("mysql def: host"),
        def["port"].as_u64().expect("mysql def: port"),
        def["database"].as_str().expect("mysql def: database"),
    )
}

fn redis_url(def: &Value) -> String {
    format!(
        "redis://{}:{}/{}",
        def["host"].as_str().expect("redis def: host"),
        def["port"].as_u64().expect("redis def: port"),
        def["db"].as_u64().expect("redis def: db"),
    )
}

async fn mysql_conn(def: &Value) -> sqlx::MySqlConnection {
    sqlx::MySqlConnection::connect(&mysql_url(def))
        .await
        .expect("the mysql def connects")
}

async fn pg_conn(def: &Value) -> sqlx::PgConnection {
    sqlx::PgConnection::connect(def["url"].as_str().expect("postgres def: url"))
        .await
        .expect("the postgres def connects")
}

async fn redis_conn(def: &Value) -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(redis_url(def).as_str()).expect("open the redis def");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("connect the redis def")
}

#[tokio::test]
async fn mysql_seed_tables_types_and_values() {
    let f = fresh(Kind::Mysql, "guard").await;
    let mut c = mysql_conn(&f.def).await;

    for (table, expected) in [
        ("users", 8i64),
        ("orders", 6),
        ("events", 4),
        ("wide", 3),
        ("big_rows", 1000),
        // The view: users with status = 'active' are ids 1, 3, 5, 7.
        ("active_users", 4),
    ] {
        let got: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{table}`")))
            .fetch_one(&mut c)
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        assert_eq!(got, expected, "table {table}");
    }

    for (column, expected) in [
        // DATA_TYPE carries the base type only - the "unsigned" qualifier lives in
        // COLUMN_TYPE; the guard pins the base and the value roundtrip does the rest.
        ("big", "bigint"),
        ("balance", "decimal"),
        ("flags", "json"),
        ("avatar", "blob"),
        ("status", "enum"),
        ("born_at", "datetime"),
    ] {
        let got: String =
            sqlx::query_scalar("SELECT CAST(data_type AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'users' AND column_name = ?")
                .bind(column)
                .fetch_one(&mut c)
                .await
                .unwrap_or_else(|e| panic!("type of users.{column}: {e}"));
        assert_eq!(got, expected, "column users.{column}");
    }

    let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.name");
    assert_eq!(name, "张三 🙂");

    let big: u64 = sqlx::query_scalar("SELECT big FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.big");
    assert_eq!(big, u64::MAX);

    let balance: String = sqlx::query_scalar("SELECT CAST(balance AS CHAR) FROM users WHERE id = 2")
        .fetch_one(&mut c)
        .await
        .expect("read users.balance as text");
    assert_eq!(balance, "-0.0001");

    let born: String = sqlx::query_scalar("SELECT CAST(born_at AS CHAR) FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.born_at as text");
    assert_eq!(born, "1990-06-15 08:30:00.123456");

    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = 2")
        .fetch_one(&mut c)
        .await
        .expect("read users.email");
    assert!(email.is_none());

    let avatar: Vec<u8> = sqlx::query_scalar("SELECT avatar FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.avatar");
    assert_eq!(avatar, vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    let dups: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE at = '2026-06-01 12:00:00.000000' AND kind = 'tick'",
    )
    .fetch_one(&mut c)
    .await
    .expect("count duplicate event rows");
    assert_eq!(dups, 2, "the two byte-identical rows must both be there");

    let bulk: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM big_rows WHERE n BETWEEN 1 AND 1000")
        .fetch_one(&mut c)
        .await
        .expect("count bulk rows");
    assert_eq!(bulk, 1000);
}

#[tokio::test]
async fn postgres_seed_tables_types_and_values() {
    let f = fresh(Kind::Postgres, "guard").await;
    let mut c = pg_conn(&f.def).await;

    for (table, expected) in [
        ("users", 8i64),
        ("orders", 6),
        ("events", 4),
        ("wide", 3),
        ("big_rows", 1000),
        ("app.documents", 4),
        ("audit.log_entries", 5),
        ("active_users", 4),
    ] {
        let got: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(&mut c)
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        assert_eq!(got, expected, "table {table}");
    }

    for (column, expected) in [
        ("big", "bigint"),
        ("balance", "numeric"),
        ("flags", "jsonb"),
        ("avatar", "bytea"),
        ("status", "USER-DEFINED"),
        ("born_at", "timestamp with time zone"),
    ] {
        let got: String =
            sqlx::query_scalar("SELECT data_type FROM information_schema.columns                                 WHERE table_schema = 'public' AND table_name = 'users'                                   AND column_name = $1")
                .bind(column)
                .fetch_one(&mut c)
                .await
                .unwrap_or_else(|e| panic!("type of users.{column}: {e}"));
        assert_eq!(got, expected, "column users.{column}");
    }

    let tags_type: String =
        sqlx::query_scalar("SELECT data_type FROM information_schema.columns                             WHERE table_schema = 'app' AND table_name = 'documents'                               AND column_name = 'tags'")
            .fetch_one(&mut c)
            .await
            .expect("type of app.documents.tags");
    assert_eq!(tags_type, "ARRAY");

    let schemas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.schemata WHERE schema_name IN ('app', 'audit')",
    )
    .fetch_one(&mut c)
    .await
    .expect("count the seeded schemas");
    assert_eq!(schemas, 2);

    let partial: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes WHERE schemaname = 'audit'           AND indexname = 'log_entries_noted_at'",
    )
    .fetch_one(&mut c)
    .await
    .expect("look up the partial index");
    assert_eq!(partial, 1, "the partial index must exist");

    let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.name");
    assert_eq!(name, "张三 🙂");

    let big: i64 = sqlx::query_scalar("SELECT big FROM users WHERE id = 1")
        .fetch_one(&mut c)
        .await
        .expect("read users.big");
    assert_eq!(big, i64::MAX);

    let epoch: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM born_at)::BIGINT FROM users WHERE id = 2")
        .fetch_one(&mut c)
        .await
        .expect("read the epoch timestamp");
    assert_eq!(epoch, 0);

    let uuid: String = sqlx::query_scalar("SELECT id::text FROM app.documents ORDER BY id LIMIT 1")
        .fetch_one(&mut c)
        .await
        .expect("read a document uuid");
    assert_eq!(uuid, "00000000-0000-4000-8000-000000000001");

    let tags: Vec<String> =
        sqlx::query_scalar("SELECT tags FROM app.documents WHERE title = 'invoice-42'")
            .fetch_one(&mut c)
            .await
            .expect("read a tags array");
    assert_eq!(tags, vec!["final".to_string(), "accounting".to_string()]);

    let dups: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE at = '2026-06-01 12:00:00+00' AND kind = 'tick'",
    )
    .fetch_one(&mut c)
    .await
    .expect("count duplicate event rows");
    assert_eq!(dups, 2);
}

#[tokio::test]
async fn redis_seed_keys_types_and_ttls() {
    let f = fresh(Kind::Redis, "guard").await;
    let mut c = redis_conn(&f.def).await;

    // 3,010 SET/SETEXT lines + one key each for hash, list, set and zset, plus
    // the four generated stream keys (SPEC §data.streams) - one number, held by the
    // seed crate so a seed change moves it once.
    let size: i64 = redis::cmd("DBSIZE").query_async(&mut c).await.expect("DBSIZE");
    assert_eq!(size, REDIS_SEED_KEYS);

    for (key, expected) in [
        ("users:1", "string"),
        ("ns:h:profile", "hash"),
        ("ns:list:queue", "list"),
        ("ns:set:tags", "set"),
        ("ns:zset:ladder", "zset"),
        ("bulk:key01500", "string"),
    ] {
        let got: String = redis::cmd("TYPE").arg(key).query_async(&mut c).await.unwrap_or_else(|e| panic!("TYPE {key}: {e}"));
        assert_eq!(got, expected, "key {key}");
    }

    let name: String = redis::cmd("GET").arg("users:1").query_async(&mut c).await.expect("GET users:1");
    assert_eq!(name, "张三 🙂");

    let llen: i64 = redis::cmd("LLEN").arg("ns:list:queue").query_async(&mut c).await.expect("LLEN");
    assert_eq!(llen, 100);
    let hlen: i64 = redis::cmd("HLEN").arg("ns:h:profile").query_async(&mut c).await.expect("HLEN");
    assert_eq!(hlen, 50);
    let scard: i64 = redis::cmd("SCARD").arg("ns:set:tags").query_async(&mut c).await.expect("SCARD");
    assert_eq!(scard, 10);
    let zcard: i64 = redis::cmd("ZCARD").arg("ns:zset:ladder").query_async(&mut c).await.expect("ZCARD");
    assert_eq!(zcard, 10);
    let score: f64 = redis::cmd("ZSCORE").arg("ns:zset:ladder").arg("beta").query_async(&mut c).await.expect("ZSCORE");
    assert_eq!(score, 85.5);

    let expiring: i64 = redis::cmd("TTL").arg("ns:ttl:ephemeral").query_async(&mut c).await.expect("TTL ephemeral");
    assert!(expiring > 0 && expiring <= 3600, "ephemeral TTL was {expiring}");
    let persisted: i64 = redis::cmd("TTL").arg("ns:ttl:persisted").query_async(&mut c).await.expect("TTL persisted");
    assert_eq!(persisted, -1, "the PERSISTed key must have no TTL");

    let bulk: String = redis::cmd("GET").arg("bulk:key01500").query_async(&mut c).await.expect("GET bulk");
    assert_eq!(bulk, "bulk value 01500");

    // SPEC §data.streams: the four generated stream keys. TYPE pins that they exist and
    // really are streams (an empty stream is a key with a type and no entries),
    // XLEN pins the bulk volume the window tests page through, and XINFO GROUPS
    // pins the `feed` group's read-but-not-ACKed seven - the pending count the
    // consumer-group surface reports. RESP2 hands each group back as a flat
    // [key, value, ...] array, so the walk is pair-wise.
    for key in ["stream:ticks", "stream:empty", "stream:one", "stream:ragged"] {
        let got: String = redis::cmd("TYPE").arg(key).query_async(&mut c).await.unwrap_or_else(|e| panic!("TYPE {key}: {e}"));
        assert_eq!(got, "stream", "key {key}");
    }
    let xlen: i64 = redis::cmd("XLEN").arg("stream:ticks").query_async(&mut c).await.expect("XLEN stream:ticks");
    assert_eq!(xlen, 10_000, "stream:ticks carries its ten thousand seeded entries");
    let groups = redis::cmd("XINFO")
        .arg("GROUPS")
        .arg("stream:ticks")
        .query_async::<redis::Value>(&mut c)
        .await
        .expect("XINFO GROUPS stream:ticks");
    let mut feed_pending: Option<i64> = None;
    let mut feed_consumers: Option<i64> = None;
    if let redis::Value::Array(rows) = groups {
        for row in rows {
            let redis::Value::Array(flat) = row else { continue };
            let mut name: Option<String> = None;
            let mut pending: Option<i64> = None;
            let mut consumers: Option<i64> = None;
            let mut key: Option<String> = None;
            for item in flat {
                match item {
                    redis::Value::BulkString(b) => {
                        let s = String::from_utf8_lossy(&b).into_owned();
                        match key.take() {
                            Some(k) if k == "name" => name = Some(s),
                            Some(_) => {}
                            None => key = Some(s),
                        }
                    }
                    redis::Value::Int(n) => match key.take().as_deref() {
                        Some("pending") => pending = Some(n),
                        Some("consumers") => consumers = Some(n),
                        _ => {}
                    },
                    _ => {}
                }
            }
            if name.as_deref() == Some("feed") {
                feed_pending = pending;
                feed_consumers = consumers;
            }
        }
    }
    assert_eq!(feed_pending, Some(7), "the feed group read seven entries it never ACKed");
    assert_eq!(feed_consumers, Some(1), "one consumer (c1) read them");
}

#[tokio::test]
async fn mysql_two_fresh_databases_are_invisible_to_each_other() {
    let a = fresh(Kind::Mysql, "isoa").await;
    let b = fresh(Kind::Mysql, "isob").await;
    let mut ca = mysql_conn(&a.def).await;
    let mut cb = mysql_conn(&b.def).await;

    sqlx::query("INSERT INTO users (name, balance, big, born_at) VALUES ('marker', 0, 0, '2026-01-01 00:00:00')")
        .execute(&mut ca)
        .await
        .expect("write through the first def");

    let in_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE name = 'marker'")
        .fetch_one(&mut ca)
        .await
        .expect("count in the first database");
    let in_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE name = 'marker'")
        .fetch_one(&mut cb)
        .await
        .expect("count in the second database");
    assert_eq!(in_a, 1);
    assert_eq!(in_b, 0, "the second fresh database must not see the first one's write");
}

#[tokio::test]
async fn postgres_two_fresh_databases_are_invisible_to_each_other() {
    let a = fresh(Kind::Postgres, "isoa").await;
    let b = fresh(Kind::Postgres, "isob").await;
    let mut ca = pg_conn(&a.def).await;
    let mut cb = pg_conn(&b.def).await;

    sqlx::query("INSERT INTO users (name, balance, big, born_at) VALUES ('marker', 0, 0, '2026-01-01 00:00:00+00')")
        .execute(&mut ca)
        .await
        .expect("write through the first def");

    let in_a: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = 'marker'")
        .fetch_one(&mut ca)
        .await
        .expect("count in the first database");
    let in_b: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = 'marker'")
        .fetch_one(&mut cb)
        .await
        .expect("count in the second database");
    assert_eq!(in_a, 1);
    assert_eq!(in_b, 0, "the second fresh database must not see the first one's write");
}

#[tokio::test]
async fn redis_two_leased_indexes_are_invisible_to_each_other() {
    // Both indexes out of ONE lease (SPEC §data.streams): two separate fresh_redis()
    // calls would park this test between acquisitions still holding the first
    // - the wait-while-holding shape the one-lease rule exists to kill.
    let mut leased = fresh_redis_many(2).await;
    let b = leased.pop().expect("the second index of the lease");
    let a = leased.pop().expect("the first index of the lease");
    let mut ca = redis_conn(&a.def).await;
    let mut cb = redis_conn(&b.def).await;

    redis::cmd("SET").arg("marker").arg("1").query_async::<()>(&mut ca).await.expect("SET through the first def");

    let in_a: bool = redis::cmd("EXISTS").arg("marker").query_async(&mut ca).await.expect("EXISTS in the first index");
    let in_b: bool = redis::cmd("EXISTS").arg("marker").query_async(&mut cb).await.expect("EXISTS in the second index");
    assert!(in_a);
    assert!(!in_b, "the second leased index must not see the first one's write");
}

#[tokio::test]
async fn a_hundred_redis_freshes_do_not_drain_the_index_pool() {
    for i in 0..100 {
        let f = fresh(Kind::Redis, "pool").await;
        let mut c = redis_conn(&f.def).await;
        let size: i64 = redis::cmd("DBSIZE").query_async(&mut c).await.expect("DBSIZE");
        // Every lease starts from FLUSHDB + a full reload: a previous iteration's
        // state leaking through would show up here as a wrong count.
        assert_eq!(size, REDIS_SEED_KEYS, "iteration {i}");
    }
}

/// SPEC §data.streams: multi-index leasing must be one atomic acquire, and this is
/// the deterministic proof. The order below is chosen so the one-at-a-time
/// shape cannot pass by scheduling luck:
///
/// - `a` holds 15 of 16 permits; exactly one is free.
/// - `c` (the exclusive sixteen) queues first. One-at-a-time, it takes that
///   last free permit immediately, then parks waiting for its 15 missing ones.
/// - `b` (a neighbor pair) queues behind it and parks waiting for its first.
/// - `drop(a)` returns 15 permits. The FIFO queue holds b's waiter; c's NEXT
///   acquire only enqueues after c is resumed, so b is ahead of it. b takes
///   one of its two and parks again; c drains the remaining 14, holds 15, and
///   waits for one more. Each now holds exactly what the other needs: mutual
///   wait, hang.
///
/// Atomic leasing makes the same steps safe: c's `acquire_many(16)` is ONE
/// all-or-nothing waiter ahead of b's, so b receives no permit before c
/// completes - a waiter only ever waits on holders that are running, never on
/// another waiter, and FIFO cannot order that into a cycle.
///
/// The tasks are spawned (not merely joined) so the semaphore sees them as
/// independent waiters, exactly like parallel #[tokio::test]s sharing the pool.
/// The yield rounds park each at its acquire deterministically on the
/// single-threaded test runtime; the engine is warmed FIRST because
/// `fresh_redis_exclusive` leases before it resolves the engine, and the
/// ordering argument wants c parked at the semaphore, not inside a boot.
#[tokio::test]
async fn exclusive_and_neighbor_leases_never_deadlock() {
    let _ = engine(Kind::Redis).await;
    let a = hold_redis_indices(15).await; // fifteen of sixteen held; one free
    let c = tokio::spawn(fresh_redis_exclusive("dlx")); // queues for all 16
    for _ in 0..3 {
        tokio::task::yield_now().await; // park c on its acquire
    }
    let b = tokio::spawn(fresh_redis_with_neighbor("dln")); // queues behind c
    for _ in 0..3 {
        tokio::task::yield_now().await; // park b on its acquire
    }
    drop(a); // the 15 come back; queue order decides who gets them
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        c.await.expect("the exclusive lease returns");
        b.await.expect("the neighbor pair returns");
    })
    .await
    .expect("the queued leases complete - a timeout here is the one-at-a-time deadlock come back");
}
