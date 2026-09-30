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

//! The engine table: how the three real databases arrive (SPEC §testing.it).
//!
//! One engine per kind per test process. The first test that needs a kind starts it;
//! everything afterwards - including tests on other libtest runtimes - only reads the
//! finished value. Resolution order is fixed and printed on failure:
//!
//! 1. `SWISS_IT_<KIND>_URL` - an existing database used as-is, verified once by a real
//!    handshake. The account must be allowed to CREATE DATABASE (mysql root, pg
//!    superuser, redis without ACL). A set-but-dead override is a hard failure: no
//!    silent fallback to containers, or a typo'd URL would mask itself as a green run
//!    against a container nobody asked for.
//! 2. testcontainers - the module images, tags from the one table below, ports published
//!    by Docker and read back through the API (never a literal 3306/5432/6379).
//! 3. Neither answers -> the test FAILS, not skips: asking for `--features it` is
//!    asking for real databases (SPEC §testing.it), and the panic carries the fixed
//!    three-part report of `failure_report`.
//!
//! Two properties of testcontainers-rs 0.27.3 shape this module and are load-bearing:
//!
//! - **No ryuk.** The Java ecosystem's reaper container does not exist here (grep the
//!   crate: zero matches); removal is `ContainerAsync`'s Drop, which needs a live
//!   tokio runtime and so never fires for a value held in a static. `exit` owns the
//!   story instead: an it-reaper watchdog child (our own ryuk, minus the container)
//!   removes every started id when the parent's stdin pipe closes on ANY exit path,
//!   an atexit hook repeats the deletes on the green path, and each start first
//!   prunes owned leftovers that are an hour old - the residue of runs whose
//!   watchdog died with them (power cut, tree kill).
//! - **One runtime for docker traffic.** libtest gives every `#[tokio::test]` its own
//!   current-thread reactor, and a hyper connection may only be polled by the reactor
//!   that registered it. All bollard traffic therefore runs on the single dedicated
//!   worker of `on_worker`; callers only ever see finished values.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::FutureExt;
use sqlx::Connection;
use tokio::sync::OnceCell;

use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::mysql::Mysql as MysqlImage;
use testcontainers_modules::postgres::Postgres as PostgresImage;
use testcontainers_modules::redis::Redis as RedisImage;

use crate::exit;

/// Containers this harness owns carry this label; the startup prune and the honesty of
/// `docker ps` during a run depend on it. Namespaced so nothing else is ever touched.
const OWNED_LABEL: &str = "org.swiss-it.owned";
/// The pid that started the container, as the label's value. Informational: it lets
/// the acceptance tests filter a child run's containers and an operator match a
/// leaked container to a process. It is deliberately NOT a prune guard - a recorded
/// pid says whose container it was, never whether that process still lives, and a
/// pid-based prune deleted parallel runs' live engines (see prune_stale's doc).
const PID_LABEL: &str = "org.swiss-it.pid";

/// The three engines SPEC §testing.it fixed the matrix to. MariaDB is deliberately absent
/// (D7): when it enters it is one more variant and one more table row, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Mysql,
    Postgres,
    Redis,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Mysql => "mysql",
            Kind::Postgres => "postgres",
            Kind::Redis => "redis",
        })
    }
}

impl Kind {
    /// The env var that bypasses containers for this engine - the escape hatch for a
    /// machine that already runs the real thing (SPEC §testing.it).
    pub fn env_var(self) -> &'static str {
        match self {
            Kind::Mysql => "SWISS_IT_MYSQL_URL",
            Kind::Postgres => "SWISS_IT_POSTGRES_URL",
            Kind::Redis => "SWISS_IT_REDIS_URL",
        }
    }

    /// The image table - versions live in this one place and nowhere else (SPEC §testing.it):
    /// mysql:8.4, postgres:17, redis:7.
    fn image(self) -> (&'static str, &'static str) {
        match self {
            Kind::Mysql => ("mysql", "8.4"),
            Kind::Postgres => ("postgres", "17"),
            Kind::Redis => ("redis", "7"),
        }
    }

    /// The port inside the container; what Docker publishes instead is what `Engine`
    /// holds, read back through the API.
    fn internal_port(self) -> u16 {
        match self {
            Kind::Mysql => 3306,
            Kind::Postgres => 5432,
            Kind::Redis => 6379,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Kind::Mysql => "MySQL 8.4",
            Kind::Postgres => "PostgreSQL 17",
            Kind::Redis => "Redis 7",
        }
    }
}

/// A resolved engine: where it answers and the privileged URL the harness itself uses
/// (CREATE DATABASE, FLUSHDB, counting connections). Tests never see `root_url` -
/// SPEC §testing.it's `Fresh` hands them a non-privileged def instead.
pub struct Engine {
    pub kind: Kind,
    /// Where the published port answers ("127.0.0.1").
    pub host: String,
    /// Whatever Docker published for the internal port - never the well-known literal.
    pub port: u16,
    /// The superuser URL (or ACL-free redis URL) the harness uses.
    pub root_url: String,
    /// Keeps the container alive for the rest of the process. Removal is `exit`'s
    /// business, not Drop's - see the module docs.
    _held: Option<Held>,
}

/// `ContainerAsync<I>` is generic over the module image type and the three share no
/// object-safe trait, so the held container is an enum with one variant per module.
enum Held {
    Mysql(ContainerAsync<MysqlImage>),
    Postgres(ContainerAsync<PostgresImage>),
    Redis(ContainerAsync<RedisImage>),
}

impl Held {
    fn id(&self) -> &str {
        // One arm per variant: the containers are distinct types, so no or-pattern.
        match self {
            Held::Mysql(c) => c.id(),
            Held::Postgres(c) => c.id(),
            Held::Redis(c) => c.id(),
        }
    }
}

static MYSQL: OnceCell<Engine> = OnceCell::const_new();
static POSTGRES: OnceCell<Engine> = OnceCell::const_new();
static REDIS: OnceCell<Engine> = OnceCell::const_new();

/// One engine per kind per test process. Racing callers wait on the same start; later
/// ones - on other runtimes - only read the finished value.
pub async fn engine(kind: Kind) -> &'static Engine {
    let cell = match kind {
        Kind::Mysql => &MYSQL,
        Kind::Postgres => &POSTGRES,
        Kind::Redis => &REDIS,
    };
    cell.get_or_init(|| async { build(kind).await }).await
}

/// Resolve or die with the report. A panic here is the documented failure mode: the
/// test that asked for a real database without one gets a red test, not a skip.
async fn build(kind: Kind) -> Engine {
    let docker_host = std::env::var("DOCKER_HOST").ok().filter(|v| !v.is_empty());
    let override_url = std::env::var(kind.env_var()).ok().filter(|v| !v.is_empty());
    match try_build(kind, override_url.as_deref(), docker_host.as_deref()).await {
        Ok(engine) => engine,
        Err(steps) => panic!("{}", failure_report(kind, docker_host.as_deref(), &steps)),
    }
}

/// The fixed resolution order of SPEC §testing.it; every step that does not answer appends
/// one line the report will print.
async fn try_build(
    kind: Kind,
    override_url: Option<&str>,
    docker_host: Option<&str>,
) -> Result<Engine, Vec<String>> {
    let mut steps = Vec::new();
    if let Some(url) = override_url {
        return match connect_until_ready(kind, url).await {
            Ok(()) => {
                let (host, port) =
                    parse_endpoint(kind, url).map_err(|e| vec![format!(
                        "{} connected but its URL would not parse into host/port: {e}",
                        kind.env_var()
                    )])?;
                Ok(Engine {
                    kind,
                    host,
                    port,
                    root_url: url.to_string(),
                    _held: None,
                })
            }
            Err(e) => Err(vec![format!(
                "{} is set, so containers are not tried; its handshake failed: {e}",
                kind.env_var()
            )]),
        };
    }
    steps.push(format!("{}: not set", kind.env_var()));
    match start_container(kind) {
        Ok(engine) => Ok(engine),
        Err(e) => {
            steps.push(format!(
                "starting {} through testcontainers (DOCKER_HOST={}) failed: {e}",
                kind.describe(),
                docker_host.unwrap_or("<not set>"),
            ));
            Err(steps)
        }
    }
}

/// Run `fut` on the one docker worker runtime and hand back its value.
///
/// libtest gives every `#[tokio::test]` its own current-thread reactor, and a hyper
/// connection may only be polled by the reactor that registered it - bollard traffic
/// from two test runtimes would panic ("driver has shutdown" / "not registered"). One
/// dedicated thread, one runtime, every docker call; the caller only ever sees a
/// finished value. A panicking task is caught so the worker outlives it; its result
/// channel then errors, which fails the engine with a clear message.
pub(crate) fn on_worker<T, F>(fut: F) -> T
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
    static TASKS: OnceLock<tokio::sync::mpsc::UnboundedSender<Task>> = OnceLock::new();
    let tx = TASKS.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Task>();
        std::thread::Builder::new()
            .name("swiss-it-docker".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("swiss-it: cannot build the docker worker runtime: {e}");
                        return;
                    }
                };
                rt.block_on(async {
                    while let Some(task) = rx.recv().await {
                        // A panic must not take the worker down: the next engine still
                        // needs it. The panicked task's result channel is dropped,
                        // which is exactly the failure its caller reports.
                        let _ = std::panic::AssertUnwindSafe(task).catch_unwind().await;
                    }
                });
            })
            .expect("spawn the swiss-it docker worker thread");
        tx
    });
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(Box::pin(async move {
        let _ = done_tx.send(fut.await);
    }))
    .expect("the swiss-it docker worker is alive");
    done_rx
        .recv()
        .unwrap_or_else(|_| panic!("a task panicked on the swiss-it docker worker"))
}

/// Start the engine's container. Runs wholly on the docker worker; the module's own
/// ready conditions (log matching) gate the start, then a real handshake gates the
/// return - readiness is a query, not a log line (SPEC §testing.it).
fn start_container(kind: Kind) -> Result<Engine, String> {
    on_worker(async move {
        prune_stale().await;
        let pid = std::process::id().to_string();
        let (name, tag) = kind.image();
        let image_ref = format!("{name}:{tag}");
        // 180 s instead of the default 60: a cold first run may spend most of it
        // pulling the image (CI does exactly that).
        let held = match kind {
            Kind::Mysql => Held::Mysql(
                MysqlImage::default()
                    .with_tag(tag)
                    // 8.4's server defaults are already utf8mb4/utf8mb4_0900_ai_ci;
                    // stating them keeps a future default flip from silently changing
                    // the seed's world. The image's entrypoint prepends `mysqld` to a
                    // command line that starts with a flag.
                    .with_cmd([
                        "--character-set-server=utf8mb4",
                        "--collation-server=utf8mb4_0900_ai_ci",
                    ])
                    .with_label(OWNED_LABEL, "1")
                    .with_label(PID_LABEL, &pid)
                    .with_startup_timeout(Duration::from_secs(180))
                    .start()
                    .await
                    .map_err(|e| format!("{image_ref}: {e}"))?,
            ),
            Kind::Postgres => Held::Postgres(
                PostgresImage::default()
                    .with_tag(tag)
                    // UTF-8 is the product's contract with the panel (SPEC §remote.utf8): the
                    // seed's CJK and emoji must survive initdb's locale defaults.
                    .with_env_var(
                        "POSTGRES_INITDB_ARGS",
                        "--encoding=UTF8 --locale=C.UTF-8",
                    )
                    .with_label(OWNED_LABEL, "1")
                    .with_label(PID_LABEL, &pid)
                    .with_startup_timeout(Duration::from_secs(180))
                    .start()
                    .await
                    .map_err(|e| format!("{image_ref}: {e}"))?,
            ),
            Kind::Redis => Held::Redis(
                RedisImage::default()
                    .with_tag(tag)
                    .with_label(OWNED_LABEL, "1")
                    .with_label(PID_LABEL, &pid)
                    .with_startup_timeout(Duration::from_secs(180))
                    .start()
                    .await
                    .map_err(|e| format!("{image_ref}: {e}"))?,
            ),
        };
        let host = held
            .host_of()
            .await
            .map_err(|e| format!("reading the published host back: {e}"))?
            .to_string();
        let port = held
            .port_of(kind.internal_port())
            .await
            .map_err(|e| format!("reading the published port back: {e}"))?;
        let endpoint = docker_endpoint();
        // Armed before the handshake: a started-but-unready container must still be
        // removed at exit, not leaked because resolution never finished.
        exit::arm(&endpoint, held.id());
        let root_url = match kind {
            // The mysql module starts root with an empty password; postgres ships its
            // postgres/postgres defaults; redis answers without ACL.
            Kind::Mysql => format!("mysql://root@{host}:{port}"),
            Kind::Postgres => format!("postgres://postgres:postgres@{host}:{port}/postgres"),
            Kind::Redis => format!("redis://{host}:{port}"),
        };
        connect_until_ready(kind, &root_url)
            .await
            .map_err(|e| format!("container up on {host}:{port} but never answered: {e}"))?;
        Ok(Engine {
            kind,
            host,
            port,
            root_url,
            _held: Some(held),
        })
    })
}

impl Held {
    /// The published host, as a string; the url::Host type itself stays unnamed because
    /// `url` is testcontainers' dependency, not ours.
    async fn host_of(&self) -> Result<String, String> {
        let host = match self {
            Held::Mysql(c) => c.get_host().await,
            Held::Postgres(c) => c.get_host().await,
            Held::Redis(c) => c.get_host().await,
        }
        .map_err(|e| e.to_string())?;
        Ok(host.to_string())
    }

    async fn port_of(&self, internal: u16) -> Result<u16, String> {
        match self {
            Held::Mysql(c) => c.get_host_port_ipv4(internal).await,
            Held::Postgres(c) => c.get_host_port_ipv4(internal).await,
            Held::Redis(c) => c.get_host_port_ipv4(internal).await,
        }
        .map_err(|e| e.to_string())
    }
}

/// The docker endpoint this process's testcontainers client talks to, replicated here
/// because the crate keeps its Config private: DOCKER_HOST wins, else the platform
/// default. Used to aim the exit-time cleanup at the same daemon the API used.
fn docker_endpoint() -> String {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        if !h.is_empty() {
            return h;
        }
    }
    if cfg!(unix) {
        "unix:///var/run/docker.sock".to_string()
    } else {
        "npipe:////./pipe/docker_engine".to_string()
    }
}

/// Best-effort removal of OWNED containers older than one hour - the tail of the
/// cleanup story the it-reaper watchdog and the atexit hook own for everything that
/// can still run code (see `exit`). This prune only catches the residue of runs
/// whose watchdog died WITH them (a power cut, a tree kill that swallowed the
/// breakaway child) and therefore may outlive the process that could delete it.
///
/// The guard is AGE, not identity: this repo once judged staleness by the pid label,
/// which deleted the live engines of any parallel gate-2 run (two worktrees, two CI
/// jobs) as readily as a real orphan's. A living suite never comes close to the
/// hour (the whole second gate runs in ~20 s), so time cannot mistake "someone
/// else's live engine" for residue, and no pid-liveness FFI is needed. Never
/// touches an unlabelled container.
async fn prune_stale() {
    let hour_ago = unix_now() - 3600;
    let work = async {
        let docker = testcontainers::core::client::docker_client_instance()
            .await
            .map_err(|e| format!("docker client: {e}"))?;
        let list =
            testcontainers::bollard::query_parameters::ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&std::collections::HashMap::from([(
                    "label".to_string(),
                    vec![format!("{OWNED_LABEL}=1")],
                )]))
                .build();
        let seen = docker
            .list_containers(Some(list))
            .await
            .map_err(|e| format!("list: {e}"))?;
        for c in seen {
            // Only age decides (see the doc above): an hour beats every live run,
            // and no pid check exists - a recorded pid says whose container it was,
            // never whether that process still lives anywhere this host can ask.
            let ancient = c.created.is_some_and(|created| created <= hour_ago);
            if !ancient {
                continue;
            }
            let Some(id) = c.id else { continue };
            let rm =
                testcontainers::bollard::query_parameters::RemoveContainerOptionsBuilder::new()
                    .force(true)
                    .v(true)
                    .build();
            match docker.remove_container(&id, Some(rm)).await {
                Ok(()) => eprintln!("swiss-it: pruned leftover container {id}"),
                Err(e) => {
                    eprintln!("swiss-it: could not prune leftover container {id}: {e}")
                }
            }
        }
        Ok::<(), String>(())
    };
    if let Err(e) = work.await {
        eprintln!("swiss-it: stale-container prune skipped: {e}");
    }
}

/// Seconds since the unix epoch, the same clock docker's `Created` label uses -
/// std only, no clock crate for one subtraction.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX)
}

/// A real round trip before anything is declared ready. The port being published does
/// not mean the first SYN from this side is answered yet, so the probe gets a short
/// retry window (40 x 250 ms = 10 s ceiling).
async fn connect_until_ready(kind: Kind, url: &str) -> Result<(), String> {
    let mut last = String::from("never attempted");
    for _ in 0..40 {
        match probe(kind, url).await {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(last)
}

/// One honest query per engine: SELECT 1, SELECT 1, PING.
async fn probe(kind: Kind, url: &str) -> Result<(), String> {
    match kind {
        Kind::Mysql => {
            let mut conn =
                sqlx::MySqlConnection::connect(url).await.map_err(|e| e.to_string())?;
            let one: i64 = sqlx::query_scalar("SELECT 1")
                .fetch_one(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            if one == 1 {
                Ok(())
            } else {
                Err(format!("SELECT 1 answered {one}"))
            }
        }
        Kind::Postgres => {
            let mut conn =
                sqlx::PgConnection::connect(url).await.map_err(|e| e.to_string())?;
            // A bare literal is INT4 in postgres; decode what it actually is.
            let one: i32 = sqlx::query_scalar("SELECT 1")
                .fetch_one(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            if one == 1 {
                Ok(())
            } else {
                Err(format!("SELECT 1 answered {one}"))
            }
        }
        Kind::Redis => {
            let client = redis::Client::open(url).map_err(|e| e.to_string())?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| e.to_string())?;
            let pong: String = redis::cmd("PING")
                .query_async(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            if pong == "PONG" {
                Ok(())
            } else {
                Err(format!("PING answered {pong:?}"))
            }
        }
    }
}

/// host/port as the Engine fields will hold them, parsed with the same libraries the
/// adapters use - no hand-rolled URL grammar anywhere.
fn parse_endpoint(kind: Kind, url: &str) -> Result<(String, u16), String> {
    match kind {
        Kind::Mysql => {
            let o = url
                .parse::<sqlx::mysql::MySqlConnectOptions>()
                .map_err(|e| e.to_string())?;
            Ok((o.get_host().to_string(), o.get_port()))
        }
        Kind::Postgres => {
            let o = url
                .parse::<sqlx::postgres::PgConnectOptions>()
                .map_err(|e| e.to_string())?;
            Ok((o.get_host().to_string(), o.get_port()))
        }
        Kind::Redis => {
            let c = redis::Client::open(url).map_err(|e| e.to_string())?;
            match c.get_connection_info().addr().clone() {
                redis::ConnectionAddr::Tcp(h, p) => Ok((h, p)),
                other => Err(format!("not a TCP redis endpoint: {other:?}")),
            }
        }
    }
}

/// The fixed three-part failure report (SPEC §testing.it): every resolution step and why it
/// did not answer; the current DOCKER_HOST; the one actionable sentence for the most
/// common cause, pointing at the runbook section that lists the rest.
fn failure_report(kind: Kind, docker_host: Option<&str>, steps: &[String]) -> String {
    let mut out = format!(
        "the {} engine could not be resolved: `--features it` asks for a real \
         database (SPEC §testing.it)

",
        kind.describe(),
    );
    out.push_str("1. resolution, step by step:
");
    for s in steps {
        out.push_str(&format!("   - {s}
"));
    }
    out.push_str(&format!(
        "
2. DOCKER_HOST is currently {}
",
        docker_host.map_or("not set".to_string(), |h| format!("{h:?}")),
    ));
    out.push_str(
        "
3. if Docker lives in WSL and this is the first run of the day, the distro may \
           still be down; bring it up first:
\twsl -d <distro> --exec true
\
           and run the suite again. The other known traps - dockerd not started, \
           random published ports, user-level env vars a service cannot see, proxies - \
           are SPEC §testing.it.
",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_report_has_the_three_fixed_parts() {
        let steps = vec![
            "SWISS_IT_MYSQL_URL: not set".to_string(),
            "starting MySQL 8.4 through testcontainers (DOCKER_HOST=tcp://127.0.0.1:1) failed: refused"
                .to_string(),
        ];
        let r = failure_report(Kind::Mysql, Some("tcp://127.0.0.1:1"), &steps);
        assert!(r.contains("1. resolution"), "{r}");
        assert!(r.contains("SWISS_IT_MYSQL_URL: not set"), "{r}");
        assert!(
            r.contains("2. DOCKER_HOST is currently \"tcp://127.0.0.1:1\""),
            "{r}"
        );
        assert!(r.contains("wsl -d <distro> --exec true"), "{r}");
        assert!(r.contains("SPEC §testing.it"), "{r}");
        assert!(r.contains("the first run of the day"), "{r}");
        // An unset DOCKER_HOST is reported as exactly that, never as an empty value.
        let r2 = failure_report(Kind::Redis, None, &[]);
        assert!(r2.contains("2. DOCKER_HOST is currently not set"), "{r2}");
    }

    #[test]
    fn failure_report_for_a_dead_override_names_it_and_says_containers_were_not_tried() {
        let steps = vec![format!(
            "{} is set, so containers are not tried; its handshake failed: connection refused",
            Kind::Postgres.env_var()
        )];
        let r = failure_report(Kind::Postgres, Some("tcp://127.0.0.1:2375"), &steps);
        assert!(r.contains(Kind::Postgres.env_var()), "{r}");
        assert!(r.contains("containers are not tried"), "{r}");
        assert!(
            r.contains("2. DOCKER_HOST is currently \"tcp://127.0.0.1:2375\""),
            "{r}"
        );
    }
}
