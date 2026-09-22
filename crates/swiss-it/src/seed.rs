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
//!   with `FLUSHDB` and reloads `seed/redis/keys.txt`; releasing flushes again
//!   and returns the index. The keyspace-catalog test leases a second index and
//!   puts two keys in it - see [`fresh_redis_with_neighbor`].
//!
//! Every failure here panics rather than returning `Err`: a test that cannot get
//! its database is a red test, not a skip (docs/44 §2.2's rule, applied one
//! layer up).

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::Connection;

use serde_json::{json, Value};
use tokio::sync::{OnceCell, Semaphore, SemaphorePermit};

use crate::engine::{engine, on_worker, Engine, Kind};

/// Bump when any file under `seed/` changes: an existing `it_seed` template (or
/// `it` role) built from an older seed is rebuilt instead of reused.
const SEED_VERSION: i64 = 2;

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
    RedisDb { index: u16, permit: SemaphorePermit<'static> },
}

impl Drop for Fresh {
    fn drop(&mut self) {
        match self.hold.take() {
            None => {}
            Some(Hold::Db { kind, name }) => on_worker(async move {
                let e = engine(kind).await;
                let outcome = match kind {
                    Kind::Mysql => match sqlx::MySqlConnection::connect(&e.root_url).await {
                        Ok(mut c) => sqlx::query(&format!("DROP DATABASE `{name}`"))
                            .execute(&mut c)
                            .await
                            .map(|_| ())
                            .map_err(|err| err.to_string()),
                        Err(err) => Err(err.to_string()),
                    },
                    Kind::Postgres => match sqlx::PgConnection::connect(&e.root_url).await {
                        // WITH (FORCE) detaches stray sessions first; the pool the
                        // adapter held may still have one open when the Fresh drops.
                        Ok(mut c) => sqlx::query(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
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
pub async fn fresh_redis_with_neighbor(tag: &str) -> (Fresh, Fresh) {
    let main = fresh(Kind::Redis, tag).await;
    let neighbor = fresh(Kind::Redis, tag).await;
    let e = engine(Kind::Redis).await;
    let url = format!("{}/{}", e.root_url, neighbor_index(&neighbor));
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

fn neighbor_index(f: &Fresh) -> u16 {
    f.def["db"].as_u64().expect("a redis def carries its db index") as u16
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
    sqlx::query(&format!(
        "CREATE DATABASE `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci"
    ))
    .execute(&mut root)
    .await
    .expect("create the per-test mysql database");
    sqlx::query(&format!("GRANT ALL PRIVILEGES ON `{name}`.* TO '{DEF_USER}'@'%'"))
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
            sqlx::query(&format!(
                "CREATE USER IF NOT EXISTS '{DEF_USER}'@'%' IDENTIFIED BY '{DEF_PASSWORD}'"
            ))
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
    sqlx::query(&format!(
        "CREATE DATABASE \"{name}\" TEMPLATE it_seed OWNER {DEF_USER}"
    ))
    .execute(&mut admin)
    .await
    .expect("clone the per-test database from the seed template");
    drop(admin);
    Fresh {
        kind: Kind::Postgres,
        name: name.clone(),
        def: json!({
            "type": "postgres",
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
                sqlx::query(&format!(
                    "CREATE ROLE {DEF_USER} LOGIN PASSWORD '{DEF_PASSWORD}'"
                ))
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
                sqlx::query(&format!(
                    "CREATE DATABASE it_seed OWNER {DEF_USER}"
                ))
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
    sem: Semaphore,
    free: Mutex<Vec<u16>>,
}

fn redis_pool() -> &'static RedisPool {
    static POOL: OnceLock<RedisPool> = OnceLock::new();
    POOL.get_or_init(|| RedisPool {
        sem: Semaphore::new(16),
        // 0..=15 is every db index a stock redis exposes; the guard test proves the
        // pool cycles instead of draining.
        free: Mutex::new((0u16..=15).rev().collect()),
    })
}

async fn fresh_redis() -> Fresh {
    let e = engine(Kind::Redis).await;
    let permit = redis_pool()
        .sem
        .acquire()
        .await
        .expect("the redis index pool has room for sixteen");
    let index = redis_pool()
        .free
        .lock()
        .expect("redis pool lock")
        .pop()
        .expect("a held permit guarantees a free index");
    let mut conn = redis_connection(e, index).await;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut conn)
        .await
        .expect("flush the leased redis db");
    load_redis_seed(&mut conn).await;
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
