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

//! Per-test databases, restored from the committed seeds (docs/44 §2.3/§2.4).
//!
//! [`fresh`] is the one call a test makes: it resolves the engine (container or
//! override URL), carves out a database only this test can see, loads the seed into
//! it and hands back the MCP `def` the adapters parse - same field names, same
//! shapes, so what the suite exercises is what the panel would post.
//!
//! Isolation model, one per engine:
//!
//! - MySQL: `CREATE DATABASE it_<tag>_<hex8>`, schema + data loaded as the
//!   container's root account, then `GRANT`ed to the non-privileged `it` user
//!   the def authenticates as. Dropping the Fresh drops the database.
//! - PostgreSQL: the seed is loaded once into a template database (`it_seed`,
//!   marked `IS_TEMPLATE`); every Fresh is `CREATE DATABASE ... TEMPLATE it_seed
//!   OWNER it` - restoring 1,000 rows costs one file copy instead of 1,000
//!   inserts. A version marker in the template rebuilds it when a seed file
//!   changes, so an override URL pointing at a reused server never serves a
//!   stale seed.
//! - Redis: redis has no per-test databases beyond the 16 numbered indexes, so
//!   they are leased from a pool (a semaphore plus a free list); a lease starts
//!   with `FLUSHDB`, reloads `seed/redis/keys.txt` and generates the four stream
//!   keys (docs/45 §2.7); releasing flushes again
//!   and returns the index. A test takes every index it will hold in ONE lease
//!   ([`fresh_redis_many`] / [`hold_redis_indices`] are each a single atomic
//!   `acquire_many_owned`) - waiting while holding part of a lease is the
//!   deadlock docs/45 S1 fixed. The keyspace-catalog test leases a second index
//!   and puts two keys in it - see [`fresh_redis_with_neighbor`].
//!
//! Every failure here panics rather than returning `Err`: a test that cannot get
//! its database is a red test, not a skip (docs/44 §2.2's rule, applied one
//! layer up).

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// The harness formats its own DDL (per-test database names, the def role, from constants and
// test tags); DDL takes no bind parameters, so those statements say `AssertSqlSafe` (sqlx 0.9).
use sqlx::{AssertSqlSafe, Connection};

use serde_json::{json, Value};
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};

use crate::engine::{engine, on_worker, Engine, Kind};

/// Bump when any file under `seed/` changes: an existing `it_seed` template (or
/// `it` role) built from an older seed is rebuilt instead of reused.
/// Version 3: the redis seed grew four generated stream keys (docs/45 §2.7).
const SEED_VERSION: i64 = 3;

/// How many keys a freshly leased redis db holds (docs/45 §2.7): 3,016 replayed
/// from keys.txt plus the four generated stream keys. A pub const because the
/// seed guards and the L1 scan tests ask this same question in three files — a
/// seed change must move ONE number, not send the next change literal-hunting.
pub const REDIS_SEED_KEYS: i64 = 3_020;

/// The account every def authenticates as. Not a superuser - the browsers must
/// work through the same non-privileged path the panel's defs use.
const DEF_USER: &str = "it";
const DEF_PASSWORD: &str = "it";

/// A per-test database. Dropping it releases the resource (drops the database /
/// flushes and returns the redis index), best-effort: a failure is printed, not
/// propagated - one test's cleanup must not fail another test's teardown.
pub struct Fresh {
    pub kind: Kind,
    /// The database name (mysql/pg) or `db<index>` label (redis) - for messages.
    pub name: String,
    /// The MCP def for this database: exactly the JSON the panel would post.
    pub def: Value,
    hold: Option<Hold>,
}

enum Hold {
    Db { kind: Kind, name: String },
    RedisDb { index: u16, permit: OwnedSemaphorePermit },
}

impl Drop for Fresh {
    fn drop(&mut self) {
        match self.hold.take() {
            None => {}
            Some(Hold::Db { kind, name }) => on_worker(async move {
                let e = engine(kind).await;
                let outcome = match kind {
                    Kind::Mysql => match sqlx::MySqlConnection::connect(&e.root_url).await {
                        Ok(mut c) => sqlx::query(AssertSqlSafe(format!("DROP DATABASE `{name}`")))
                            .execute(&mut c)
                            .await
                            .map(|_| ())
                            .map_err(|err| err.to_string()),
                        Err(err) => Err(err.to_string()),
                    },
                    Kind::Postgres => match sqlx::PgConnection::connect(&e.root_url).await {
                        // WITH (FORCE) detaches stray sessions first; the pool the
                        // adapter held may still have one open when the Fresh drops.
                        Ok(mut c) => sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\" WITH (FORCE)")))
                            .execute(&mut c)
                            .await
                            .map(|_| ())
                            .map_err(|err| err.to_string()),
                        Err(err) => Err(err.to_string()),
                    },
                    Kind::Redis => Ok(()),
                };
                if let Err(err) = outcome {
                    eprintln!("swiss-it: could not drop {kind} database {name}: {err}");
                }
            }),
            Some(Hold::RedisDb { index, permit }) => {
                on_worker(async move {
                    let e = engine(Kind::Redis).await;
                    let url = format!("{}/{}", e.root_url, index);
                    if let Ok(client) = redis::Client::open(url.as_str()) {
                        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
                            if let Err(err) =
                                redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await
                            {
                                eprintln!("swiss-it: could not flush redis db{index}: {err}");
                            }
                        }
                    }
                });
                // The index goes back only after the flush was attempted, so a later
                // lease can never inherit keys a dropped test wrote.
                redis_pool().free.lock().expect("redis pool lock").push(index);
                drop(permit);
            }
        }
    }
}

/// Resolve a per-test database for `kind`, restored from the seed. `tag` names the
/// test in the database name; it is sanitized, not trusted - it never reaches SQL
/// unquoted anyway.
pub async fn fresh(kind: Kind, tag: &str) -> Fresh {
    match kind {
        Kind::Mysql => fresh_mysql(tag).await,
        Kind::Postgres => fresh_postgres(tag).await,
        // Redis isolates by leased index, not by name - the tag is a mysql/pg thing.
        Kind::Redis => fresh_redis().await,
    }
}

/// A redis Fresh plus a second leased index holding exactly two keys, for the
/// keyspace-catalog surface (docs/43 M3): the catalog must list both dbs and must
/// not confuse which is which.
///
/// Both indexes come out of the ONE `fresh_redis_many(2)` lease - the rule every
/// multi-index test follows (docs/45 S1 fix): leasing them one at a time parks a
/// test in the semaphore's FIFO queue still holding the first, which is exactly
/// the deadlock shape the atomic lease exists to kill.
pub async fn fresh_redis_with_neighbor(_tag: &str) -> (Fresh, Fresh) {
    let mut leased = fresh_redis_many(2).await;
    let neighbor = leased.pop().expect("the neighbor half of the lease");
    let main = leased.pop().expect("the main half of the lease");
    let e = engine(Kind::Redis).await;
    let url = format!("{}/{}", e.root_url, neighbor.def["db"]);
    let client = redis::Client::open(url.as_str()).expect("open the neighbor redis db");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("connect the neighbor redis db");
    redis::cmd("SET")
        .arg("neighbor:one")
        .arg("1")
        .query_async::<()>(&mut conn)
        .await
        .expect("seed the first neighbor key");
    redis::cmd("SET")
        .arg("neighbor:two")
        .arg("2")
        .query_async::<()>(&mut conn)
        .await
        .expect("seed the second neighbor key");
    (main, neighbor)
}

/// The other fifteen indexes of an exclusive lease, held unseeded (see
/// `fresh_redis_exclusive`): the seat, not the database. Dropping the hold
/// flushes each held db and returns every index + permit - the same contract a
/// redis Fresh's Drop carries out, so a later lease can never inherit keys a
/// holder wrote.
pub struct RedisHold {
    indexes: Vec<(u16, OwnedSemaphorePermit)>,
}

impl RedisHold {
    /// Detach one `(index, permit)` pair - the half a caller intends to seed
    /// into a real Fresh (see `fresh_redis_exclusive`). The remainder still
    /// drops clean.
    pub fn take(&mut self) -> Option<(u16, OwnedSemaphorePermit)> {
        self.indexes.pop()
    }
}

impl Drop for RedisHold {
    fn drop(&mut self) {
        for (index, permit) in std::mem::take(&mut self.indexes) {
            on_worker(async move {
                let e = engine(Kind::Redis).await;
                let url = format!("{}/{}", e.root_url, index);
                if let Ok(client) = redis::Client::open(url.as_str()) {
                    if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
                        if let Err(err) =
                            redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await
                        {
                            eprintln!("swiss-it: could not flush redis db{index}: {err}");
                        }
                    }
                }
            });
            // The index goes back only after the flush was attempted, so a later
            // lease can never inherit keys a dropped holder wrote.
            redis_pool().free.lock().expect("redis pool lock").push(index);
            drop(permit);
        }
    }
}

/// Hold `n` redis indexes with nothing seeded: the seat alone. One atomic
/// lease (it parks holding nothing, like every lease here), so a test can hold
/// the whole pool - or fifteen sixteenths of it - while queued leases wait
/// their turn. The deadlock guard uses this to shape the semaphore's FIFO
/// queue deterministically.
pub async fn hold_redis_indices(n: usize) -> RedisHold {
    RedisHold {
        indexes: lease_redis_indices(n).await,
    }
}

/// A seeded Fresh PLUS every other redis index the pool has, held empty (docs/45
/// S2.1): INFO commandstats is SERVER-wide, so a measurement that wants to count only
/// this test's commands must hold all sixteen indexes - any concurrently running redis
/// test would land its commands in the same counters.
///
/// The lease comes BEFORE the engine call, so this future reaches the semaphore
/// with no prior awaits - the deadlock guard relies on exactly that park point.
/// The fifteen extras stay unseeded in the hold: holding the seat costs one
/// FLUSHDB at Drop, while fifteen seed loads would replay 3,020 keys and 10,000
/// XADDs apiece for nobody's benefit - pure seconds on every gate run.
pub async fn fresh_redis_exclusive(_tag: &str) -> (Fresh, RedisExclusive) {
    let mut rest = hold_redis_indices(16).await;
    let e = engine(Kind::Redis).await;
    let (index, permit) = rest.take().expect("sixteen held indexes have a spare");
    let main = seed_redis_index(e, index, permit).await;
    (main, RedisExclusive { rest })
}

/// The fifteen unseeded seats behind an exclusive lease. No Drop impl of its own
/// on purpose: the `RedisHold` field IS the cleanup - dropping it flushes and
/// returns every remaining index.
pub struct RedisExclusive {
    #[allow(dead_code)] // held to be dropped; the drop does the work
    rest: RedisHold,
}

// --- MySQL -----------------------------------------------------------------------------------------

static MYSQL_USER: OnceCell<()> = OnceCell::const_new();

async fn fresh_mysql(tag: &str) -> Fresh {
    let e = engine(Kind::Mysql).await;
    ensure_mysql_user(e).await;
    let name = database_name(tag);
    let mut root = sqlx::MySqlConnection::connect(&e.root_url)
        .await
        .expect("connect the mysql engine's root URL");
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci"
    )))
    .execute(&mut root)
    .await
    .expect("create the per-test mysql database");
    sqlx::query(AssertSqlSafe(format!("GRANT ALL PRIVILEGES ON `{name}`.* TO '{DEF_USER}'@'%'")))
        .execute(&mut root)
        .await
        .expect("grant the def account its per-test database");
    let mut db = sqlx::MySqlConnection::connect(&format!("{}/{}", e.root_url, name))
        .await
        .expect("connect the fresh mysql database");
    sqlx::raw_sql(include_str!("../seed/mysql/schema.sql"))
        .execute(&mut db)
        .await
        .expect("load the mysql schema seed");
    sqlx::raw_sql(include_str!("../seed/mysql/data.sql"))
        .execute(&mut db)
        .await
        .expect("load the mysql data seed");
    Fresh {
        kind: Kind::Mysql,
        name: name.clone(),
        def: json!({
            "type": "mysql",
            "host": e.host,
            "port": e.port,
            "user": DEF_USER,
            "password": DEF_PASSWORD,
            "database": name,
        }),
        hold: Some(Hold::Db { kind: Kind::Mysql, name }),
    }
}

/// The `it` account exists once per engine; the per-database grants happen in
/// [`fresh_mysql`], so this only creates the login.
async fn ensure_mysql_user(e: &'static Engine) {
    MYSQL_USER
        .get_or_init(|| async {
            let mut root = sqlx::MySqlConnection::connect(&e.root_url)
                .await
                .expect("connect the mysql engine's root URL");
            sqlx::query(AssertSqlSafe(format!(
                "CREATE USER IF NOT EXISTS '{DEF_USER}'@'%' IDENTIFIED BY '{DEF_PASSWORD}'"
            )))
            .execute(&mut root)
            .await
            .expect("create the def account");
        })
        .await;
}

// --- PostgreSQL ------------------------------------------------------------------------------------

static PG_TEMPLATE: OnceCell<()> = OnceCell::const_new();

async fn fresh_postgres(tag: &str) -> Fresh {
    let e = engine(Kind::Postgres).await;
    ensure_pg_template(e).await;
    let name = database_name(tag);
    let mut admin = sqlx::PgConnection::connect(&e.root_url)
        .await
        .expect("connect the postgres engine's admin URL");
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE \"{name}\" TEMPLATE it_seed OWNER {DEF_USER}"
    )))
    .execute(&mut admin)
    .await
    .expect("clone the per-test database from the seed template");
    drop(admin);
    Fresh {
        kind: Kind::Postgres,
        name: name.clone(),
        def: json!({
            "type": "pg",
            "url": format!(
                "postgres://{DEF_USER}:{DEF_PASSWORD}@{}:{}/{}",
                e.host, e.port, name
            ),
        }),
        hold: Some(Hold::Db { kind: Kind::Postgres, name }),
    }
}

/// Build the template database and the def account once per engine. Reused across
/// servers (override URLs) only while the version marker matches.
async fn ensure_pg_template(e: &'static Engine) {
    PG_TEMPLATE
        .get_or_init(|| async {
            let mut admin = sqlx::PgConnection::connect(&e.root_url)
                .await
                .expect("connect the postgres engine's admin URL");
            let has_role: i64 =
                sqlx::query_scalar("SELECT count(*) FROM pg_roles WHERE rolname = $1")
                    .bind(DEF_USER)
                    .fetch_one(&mut admin)
                    .await
                    .expect("look up the def role");
            if has_role == 0 {
                sqlx::query(AssertSqlSafe(format!(
                    "CREATE ROLE {DEF_USER} LOGIN PASSWORD '{DEF_PASSWORD}'"
                )))
                .execute(&mut admin)
                .await
                .expect("create the def role");
            }
            let exists: i64 =
                sqlx::query_scalar("SELECT count(*) FROM pg_database WHERE datname = 'it_seed'")
                    .fetch_one(&mut admin)
                    .await
                    .expect("look up the seed template");
            let stale = exists == 1 && template_version(e).await != Some(SEED_VERSION);
            if stale {
                sqlx::query("ALTER DATABASE it_seed WITH IS_TEMPLATE FALSE")
                    .execute(&mut admin)
                    .await
                    .expect("unmark the stale seed template");
                sqlx::query("DROP DATABASE it_seed")
                    .execute(&mut admin)
                    .await
                    .expect("drop the stale seed template");
            }
            if exists == 0 || stale {
                sqlx::query(AssertSqlSafe(format!(
                    "CREATE DATABASE it_seed OWNER {DEF_USER}"
                )))
                .execute(&mut admin)
                .await
                .expect("create the seed template database");
                sqlx::query("ALTER DATABASE it_seed WITH IS_TEMPLATE TRUE")
                    .execute(&mut admin)
                    .await
                    .expect("mark the seed template as a template");
                // Load as the def account, NOT as the admin: a clone copies table
                // ownership verbatim, so objects created by postgres would stay
                // unreadable to `it` in every database cloned from the template.
                let mut t = sqlx::PgConnection::connect(&format!(
                    "postgres://{DEF_USER}:{DEF_PASSWORD}@{}:{}/it_seed",
                    e.host, e.port
                ))
                .await
                .expect("connect the fresh seed template as the def account");
                sqlx::raw_sql(include_str!("../seed/postgres/schema.sql"))
                    .execute(&mut t)
                    .await
                    .expect("load the postgres schema seed");
                sqlx::raw_sql(include_str!("../seed/postgres/data.sql"))
                    .execute(&mut t)
                    .await
                    .expect("load the postgres data seed");
                // The version marker lives in its own schema: public must contain
                // exactly the seeded application objects, so a browser walking the
                // default schema sees only the catalog it is being tested against.
                sqlx::query("CREATE SCHEMA it_meta")
                    .execute(&mut t)
                    .await
                    .expect("create the seed marker schema");
                sqlx::query("CREATE TABLE it_meta.seed_meta (version BIGINT NOT NULL)")
                    .execute(&mut t)
                    .await
                    .expect("create the seed version marker");
                sqlx::query("INSERT INTO it_meta.seed_meta (version) VALUES ($1)")
                    .bind(SEED_VERSION)
                    .execute(&mut t)
                    .await
                    .expect("stamp the seed version");
            }
        })
        .await;
}

/// The version a surviving `it_seed` carries; `None` when it has no marker (or
/// cannot be opened) - both mean "rebuild it".
async fn template_version(e: &Engine) -> Option<i64> {
    let mut t = sqlx::PgConnection::connect(&pg_db_url(e, "it_seed"))
        .await
        .ok()?;
    sqlx::query_scalar("SELECT version FROM it_meta.seed_meta LIMIT 1")
        .fetch_one(&mut t)
        .await
        .ok()
}

fn pg_db_url(e: &Engine, name: &str) -> String {
    // root_url points at the admin database; swap the path segment.
    let base = e.root_url.rsplit_once('/').expect("a postgres URL has a path") ;
    format!("{}/{}", base.0, name)
}

// --- Redis -----------------------------------------------------------------------------------------

struct RedisPool {
    sem: std::sync::Arc<Semaphore>,
    free: Mutex<Vec<u16>>,
}

fn redis_pool() -> &'static RedisPool {
    static POOL: OnceLock<RedisPool> = OnceLock::new();
    POOL.get_or_init(|| RedisPool {
        // An Arc so multi-index leases can take `acquire_many_owned` and split the
        // owned permit into per-index singles (see lease_redis_indices).
        sem: std::sync::Arc::new(Semaphore::new(16)),
        // 0..=15 is every db index a stock redis exposes; the guard test proves the
        // pool cycles instead of draining.
        free: Mutex::new((0u16..=15).rev().collect()),
    })
}

/// Lease `n` distinct redis indexes as ONE atomic step: `acquire_many_owned(n)` takes
/// all n permits in a single FIFO queue entry, then `split(1)` breaks the owned permit
/// into n singles, each paired with a popped index.
///
/// Waiting while holding NOTHING is the entire reason this cannot deadlock. A test
/// that takes its indexes one at a time (acquire; ...; acquire) sits in the semaphore's
/// FIFO queue BETWEEN acquisitions while still holding what it already took: the
/// exclusive commandstats lease (docs/45 S2.1) waits for every last permit, a
/// neighbor pair waits for its second, each holds what the other needs, and a FIFO
/// semaphore makes that mutual wait a hang - not a scheduling accident. This function
/// is the one place indexes are leased, and it never waits holding any.
async fn lease_redis_indices(n: usize) -> Vec<(u16, OwnedSemaphorePermit)> {
    // One FIFO queue entry for all n permits. `acquire_many_owned` is
    // all-or-nothing: this future either returns holding everything or parks
    // holding nothing - the property every lease shape in this file builds on.
    let mut permit = redis_pool()
        .sem
        .clone()
        .acquire_many_owned(n as u32)
        .await
        .expect("the redis index pool has room for sixteen");
    let mut free = redis_pool().free.lock().expect("redis pool lock");
    (0..n)
        .map(|_| {
            let permit = permit
                .split(1)
                .expect("the lease owns exactly n permits");
            let index = free
                .pop()
                .expect("a held permit guarantees a free index");
            (index, permit)
        })
        .collect()
}

/// The post-lease half of a redis Fresh: flush the leased db, load the seed, and wrap
/// the index + permit in a Fresh whose Drop flushes and returns both.
async fn seed_redis_index(e: &'static Engine, index: u16, permit: OwnedSemaphorePermit) -> Fresh {
    let mut conn = redis_connection(e, index).await;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut conn)
        .await
        .expect("flush the leased redis db");
    load_redis_seed(&mut conn).await;
    load_redis_streams(&mut conn).await;
    Fresh {
        kind: Kind::Redis,
        name: format!("db{index}"),
        def: json!({
            "type": "redis",
            "host": e.host,
            "port": e.port,
            "db": index,
        }),
        hold: Some(Hold::RedisDb { index, permit }),
    }
}

/// One seeded Fresh from a one-index lease - the single-index case. The doc
/// comment on `fresh` explains why this is its own named function.
async fn fresh_redis() -> Fresh {
    let e = engine(Kind::Redis).await;
    let (index, permit) = lease_redis_indices(1)
        .await
        .into_iter()
        .next()
        .expect("a one-index lease has one element");
    seed_redis_index(e, index, permit).await
}

/// `n` seeded redis Freshes out of ONE atomic lease. The docs/45 S1 fix rule:
/// a test takes every redis index it will hold in a single lease - one
/// `acquire_many` parks holding nothing, so any queue position is safe.
/// Splitting one test's indexes across two leases re-introduces the
/// wait-while-holding shape the rule exists to kill.
pub async fn fresh_redis_many(n: usize) -> Vec<Fresh> {
    let e = engine(Kind::Redis).await;
    let mut freshes = Vec::with_capacity(n);
    for (index, permit) in lease_redis_indices(n).await {
        freshes.push(seed_redis_index(e, index, permit).await);
    }
    freshes
}

async fn redis_connection(
    e: &'static Engine,
    index: u16,
) -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(format!("{}/{}", e.root_url, index).as_str())
        .expect("open the leased redis db");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("connect the leased redis db")
}

/// Replay `seed/redis/keys.txt`: one command per line, TAB-separated, `#` comments.
/// Chunked through a pipeline so 3,000 bulk keys cost a handful of round trips.
async fn load_redis_seed(conn: &mut redis::aio::MultiplexedConnection) {
    let commands: Vec<Vec<&str>> = include_str!("../seed/redis/keys.txt")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect();
    for chunk in commands.chunks(500) {
        let mut pipe = redis::pipe();
        for args in chunk {
            pipe.cmd(args[0]).arg(&args[1..]).ignore();
        }
        pipe.query_async::<()>(conn)
            .await
            .expect("load a chunk of the redis seed");
    }
}

/// The four stream keys the seed GENERATES on top of keys.txt (docs/45 §2.7).
///
/// Why generated, not keys.txt lines: `stream:ticks` is 10,000 XADDs — the mysql
/// seed uses a recursive CTE and the pg seed generate_series for the same reason,
/// bulk rows do not belong in a replayed text file. And why explicit entry ids
/// (`1700000000000 + i*100`): a stream id's millisecond half IS its timestamp, so
/// pinned ids make every derived `ts` a fixed string a test can assert against,
/// and the 100 ms spacing keeps `stream_id_ms` honest at every rank. The `feed`
/// group has read seven entries it never ACKed, so the consumer-group surface
/// has a pending count that is not zero; the other three keys are the edge
/// shapes the window code must survive — an empty-but-existing stream (only
/// XGROUP MKSTREAM can make one), a single entry, and five entries whose field
/// sets interleave, which is how the column-union rule earns its keep.
async fn load_redis_streams(conn: &mut redis::aio::MultiplexedConnection) {
    const TICKS: i64 = 10_000;
    const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "EEE"];
    // Chunked like load_redis_seed: one pipeline is one bounded round trip, so the
    // whole stream is twenty pipelines, not ten thousand serialized waits.
    for start in (0..TICKS).step_by(500) {
        let mut pipe = redis::pipe();
        for i in start..(start + 500).min(TICKS) {
            pipe.cmd("XADD")
                .arg("stream:ticks")
                .arg(format!("{}-0", 1_700_000_000_000_i64 + i * 100))
                .arg("sym")
                .arg(SYMS[(i % 5) as usize])
                .arg("px")
                .arg(format!("{:.2}", (i % 1000) as f64 / 100.0))
                .arg("qty")
                .arg(1 + i % 50)
                .arg("side")
                .arg(if i % 2 == 0 { "b" } else { "s" })
                .ignore();
        }
        pipe.query_async::<()>(conn)
            .await
            .expect("load a chunk of stream:ticks");
    }
    // The group starts at 0 (not $), so its lag is every entry minus the seven
    // delivered here; XREADGROUP through this raw connection is fine — the
    // CONNECTION_BREAKING blacklist guards the gateway's shared adapter handle,
    // not a seed loader that owns its connection.
    redis::cmd("XGROUP")
        .arg("CREATE")
        .arg("stream:ticks")
        .arg("feed")
        .arg("0")
        .query_async::<()>(conn)
        .await
        .expect("create the feed group");
    redis::cmd("XREADGROUP")
        .arg("GROUP")
        .arg("feed")
        .arg("c1")
        .arg("COUNT")
        .arg(7)
        .arg("STREAMS")
        .arg("stream:ticks")
        .arg(">")
        .query_async::<redis::Value>(conn)
        .await
        .expect("read seven entries into the feed group's pending list");

    // An empty stream can only be born by XGROUP ... MKSTREAM — no XADD leaves
    // zero entries — which is exactly the shape the empty-state UI needs on
    // disk (docs/45 §2.5 “empty / single” row).
    redis::cmd("XGROUP")
        .arg("CREATE")
        .arg("stream:empty")
        .arg("watchers")
        .arg("$")
        .arg("MKSTREAM")
        .query_async::<()>(conn)
        .await
        .expect("create the empty stream");
    redis::cmd("XADD")
        .arg("stream:one")
        .arg("1700000000000-0")
        .arg("a")
        .arg("1")
        .query_async::<String>(conn)
        .await
        .expect("create the single-entry stream");
    let ragged: [(&str, &[(&str, &str)]); 5] = [
        ("1700000000000-0", &[("a", "1")]),
        ("1700000000100-0", &[("a", "2"), ("b", "2")]),
        ("1700000000200-0", &[("b", "3"), ("c", "3")]),
        ("1700000000300-0", &[("c", "4")]),
        ("1700000000400-0", &[("a", "5"), ("c", "5"), ("d", "5")]),
    ];
    for (id, fields) in ragged {
        let mut cmd = redis::cmd("XADD");
        cmd.arg("stream:ragged").arg(id);
        for (k, v) in fields {
            cmd.arg(k).arg(v);
        }
        cmd.query_async::<String>(conn)
            .await
            .expect("create a ragged stream entry");
    }
}

// --- shared ----------------------------------------------------------------------------------------

fn database_name(tag: &str) -> String {
    let mut clean: String = tag
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    clean.truncate(24);
    if clean.is_empty() {
        clean.push('t');
    }
    format!("it_{clean}_{:08x}", uniqueness())
}

/// Nanoseconds mixed with the pid and a counter: unique within a process, and no
/// two processes that might share an override-URL server collide on one name.
fn uniqueness() -> u32 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .subsec_nanos() as u64;
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    (nanos ^ (n << 12) ^ ((std::process::id() as u64) << 4)) as u32
}
