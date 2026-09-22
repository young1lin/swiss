# 44 — 集成测试底座：真库、自建 MCP、任何机器一条命令

> 状态：**已实施**（branch `integration-harness`，2026-09-22 实施完毕；spec 定稿 2026-09-22；基线 `946b921`
> （master）。逐项：I0 `062928a`、I1 `4740a9b`、I2 `0349c41`、I3 `239895e`、I4 `f707247`、清理补丁（it-reaper +
> 按 age 的 prune）`78cd561`、I5 `3cf62a8`、I6 `2145a44`、I7 见本提交（I5/I6 于同日重做以各自携带
> Cargo.lock——原 `64a8ab4`/`86fa047` 的树检出即 `--locked` 失败）。实施走了 swiss-dependency-review →
> swiss-verify → swiss-review 流程。Docker 的安装在 [44-wsl-docker-setup.md](44-wsl-docker-setup.md)）。
> 增补（2026-09-22，合并 `f3b6899` 后按树反向校对；上文「I7 见本提交」即 `929af06`）：§2 各节由计划语态
> 改为已交付语态并补实施落定的细节——目录树与依赖表（§2.1）、容器标签/端口/超时与收尸三层（§2.2，
> `exit.rs`/`docker_raw.rs`/`it-reaper`，`78cd561`）、Fresh 的实际 SQL 与 pg 模板版本标记（§2.3）、seed
> 的实际形状与计数（§2.4，redis 3,016 键）、三层的实际测试清单与 66 条的分账（§2.5–2.7）、CI job 与
> deploy.ps1 门禁的落地形状（§2.8）。§0 是实施前的记录，按惯例不动。
> 前置阅读：`AGENTS.md`（四条产品属性，载重规则高于本文）、[docs/08](08-testing.md)（测试账本——本文
> 填的是它第 36 行那句承诺的空）、[docs/05](05-wire-compatibility.md)（本文不动密封格式）、
> [docs/40](40-open-source-release-spec.md) D2（真实数据不入库——seed 的红线）、
> [docs/16](16-operations-hardening-spec.md) H1（守护进程环境清洗——L3 要证明的东西）。
>
> 需求原文（owner，2026-09-22）："然后看看，怎么计划，测试的时候，应该独立隔离。测试的时候 mcp 自建，
> Redis 使用 embed Redis（尽可能测试到），MYSQL 和 PGSQL 也是如此。如果需要类似Docker 一样的东西，我本地有
> WSL 可以。目的是集成测试能够不依赖任何环境，类似 test-container，能够还原最新的数据库的内容。然后每次
> 重构或者编写代码，都要经过这个集成测试。因为和本机无关，所以任何地方都能执行集成测试。这样的话，我就能
> 放心重构了"。追认（同日，对第一轮九问）："可以，我来装，分两个不同的内容，一个是 WSL 装 Docker 的文档，
> 一个是 spec，一个是 prompt"——九条推荐全部通过，记在 §1。

## 0. 现状与缺口（读代码得出，逐条给 file:line）

### 0.1 七千七百行数据库代码，零条真库测试

| 文件 | 行数 | 今天怎么被测 |
| --- | ---: | --- |
| `crates/swiss-mcp/src/adapters/pg.rs` | 1,130 | 内联单元测试只覆盖纯函数（SQL 拼接、参数整形） |
| `crates/swiss-mcp/src/adapters/redis.rs` | 1,101 | 同上 |
| `crates/swiss-mcp/src/adapters/pg_browser.rs` | 1,024 | 只经 `swiss-data/dbbrowser_api.rs` 的 StubDb 走路由层 |
| `crates/swiss-mcp/src/adapters/mysql.rs` | 988 | 纯函数 |
| `crates/swiss-mcp/src/adapters/mysql_browser.rs` | 930 | 无内联测试——docs/08 §"例外表"第一行点名 |
| `crates/swiss-mcp/src/adapters/sql.rs` | 616 | 纯函数 |
| `crates/swiss-mcp/src/adapters/redis_browser.rs` | 592 | StubRedis |
| `crates/swiss-mcp/src/adapters/resources.rs` + `{mysql,pg,redis}_resources.rs` | 1,358 | 纯函数 |
| **合计** | **7,739** | **没有一条测试连过真实的 MySQL、PostgreSQL 或 Redis** |

docs/08 第 36–37 行写着"**DB tests self-skip without credentials.** `direct-adapters`、`dbbrowser`、`sql`、
`db-resources` must skip cleanly, not fail, on a machine with no database"——那是 Node 套件的规则，
移植时**没有跟过来**：全树 `grep std::env::var(".*(MYSQL|PG|REDIS|DATABASE)` 为零，`#[ignore]` 只有
`crates/swiss-core/tests/seal_bench.rs` 一处。承诺在，套件不在。

### 0.2 现有的种子，本文在它们之上长

- **进程内的远端 MCP 已有先例**：`tests/http_adapter.rs:54-84` 用 axum 在 `127.0.0.1:0` 起一个 echo
  远端，`HttpAdapter` 打它——http/rest 两种接法的集成测试模式已经存在，本文不重造。
- **网关的进程内启动已有先例**：`tests/adminapi.rs:50-112`——`sandbox()` 造隔离的
  `MCP_GATEWAY_HOME` + 固定 master key，`setup()` 用 `Registry::new` / `ManagedStore::open_at` /
  `AppContext::new` / `build_app` 拼出完整路由。根 crate 是 lib（`src/lib.rs:22-29` 导出 `app`、
  `adminapi` 等），别的 crate 能 dev-depend 它。
- **真实 rmcp client 已在 dev-deps**：根 `Cargo.toml` `[dev-dependencies]` 的 `rmcp` 带
  `transport-streamable-http-client-reqwest`，`tests/adminapi.rs` 已经用它经 `/mcp/<name>` 打网关。
- **proc adapter 的测试绑在平台上**：`adapters/proc.rs:612` 起的是 `cmd_exe()`——Windows 的 `cmd.exe`
  当 MCP 子进程用；unix 上这些路径没有对应物。"mcp 自建"要解的就是它。

### 0.3 底座的事实（决定 D1 的依据，不是偏好）

- **Docker 本机没有**：Windows 侧 `docker` 不在 PATH，WSL `<distro>` 里也没有（`systemctl
  is-active docker` = inactive，`/var/run/docker.sock` 不存在）。CI 的 `ubuntu-latest` 自带 dockerd。
- **嵌入式方案逐个查过**：PostgreSQL 有成熟的 `postgresql_embedded`（下载便携二进制，三平台）；
  **MySQL 没有**嵌入式（只能下 400–600 MB 官方包自己 `mysqld --initialize`，三平台三份缓存归我们管）；
  **Redis 没有 Windows 二进制**，纯 Rust 的 mini-redis 只有 GET/SET——`SCAN`/`HGETALL`/`TTL`/`INFO
  keyspace`/`CLIENT INFO` 一个没有，而那正是 redis_browser 要证明的东西。假 Redis 测不到需求要测的
  "尽可能"。
- 需求里的"embed"落到事实上就是：**能嵌的只有 pg 一种**。拼盘（pg 嵌入 + 其余 Docker）意味着两套
  启动机制、两套故障模式、两份文档——违反仓库"never invent a parallel mechanism"的惯例。三个引擎
  一套 Docker，是 §1 D1。

### 0.4 每次走查都在丢东西

docs/22（W0–W5）、docs/42、docs/43 的每一批都在 19998 上对着 owner 的真实库手工走过——那些库因为
docs/40 D2 永远不能进仓库（docs/43 的四张 m3/m4 截图就是这么被挡在门外的）。于是每次走查的证据只剩
截图和一段"19998 走查"文字，**没有一条能重跑**。重构 `mysql_browser.rs` 的人今天拿到的保证是：
路由层的 stub 还通过。这就是"放心重构"缺的那块。

### 0.5 宪法检查

| 属性 | 本文的关系 |
| --- | --- |
| Ruthlessly small | 整个底座是 dev-only crate `swiss-it`，**不进 `swiss` 的依赖图**（§2.8 有验证命令）；二进制零字节、空闲零成本。 |
| Plugin-shaped | 不动。L2 从插件外面打（`/mcp/<name>`、`/api/db/*`），不给 host 加任何测试钩子。 |
| Hot-pluggable | **本文能证明它**：L2 有一条测试在 Disable 之后到容器里数连接（`SHOW PROCESSLIST` / `pg_stat_activity` / `CLIENT LIST`），必须为零（§2.6）。今天这条属性只有代码注释在担保。 |
| Three ways to bring a tool in | L3 用仓库自带的 stdio MCP server 证明第一种；`tests/http_adapter.rs` 已证第二种；第三种是单元测试的地盘。 |

docs/05：密封格式一字不动；集成测试用 `sandbox()` 那套隔离 home 与固定 master key，写出去的
`managed.json` 只活在 temp 目录。

## 1. 决定（owner，2026-09-22，第一轮九问全部按推荐通过）

| # | 决定 | 落地 |
| --- | --- | --- |
| D1 | **Docker + `testcontainers` crate，三个引擎一套机制**：`mysql:8.4`、`postgres:17`、`redis:7`；另给 `SWISS_IT_{MYSQL,POSTGRES,REDIS}_URL` 三个覆盖变量，有现成库的人可以不起容器 | §2.2 |
| D2 | Docker 装在 **WSL 里的 docker-ce/docker.io**，dockerd 听 `tcp://127.0.0.1:2375`，Windows 侧 `DOCKER_HOST` 指过去；不装 Docker Desktop | [44-wsl-docker-setup.md](44-wsl-docker-setup.md)，owner 自己做 |
| D3 | "还原最新的数据库内容" = **仓库里提交一份 seed（schema + 数据）**，每条测试从它还原；不是真实库的 dump。seed 按踩过坑的形状设计 | §2.4 |
| D4 | 三层：L1 三个 browser + resources + sql 打真库；L2 三个 MCP adapter 经真 rmcp client 走 `/mcp/<name>`；L3 proc 用仓库自带的 Rust MCP server。L4（整机 + `/api/*` golden capture）留下一期 | §2.5–2.7 |
| D5 | 新 dev-only 成员 **`crates/swiss-it`**，测试挂 feature **`it`**（默认关）。两条门：`cargo test --workspace`（单元，任何机器）+ `cargo test -p swiss-it --features it`（集成，需要 Docker）。改到 §2.8 列出的文件必过第二条。CI 加 ubuntu `integration` job | §2.8、§3 I0/I7 |
| D6 | 隔离模型：每个测试进程每引擎一个容器（懒起、it-reaper 看门狗 + atexit 收尸）；**每条测试自己的数据库**（mysql `CREATE DATABASE it_<tag>_<hex8>` 灌 seed；pg 从 template 库 `CREATE DATABASE … TEMPLATE it_seed`；redis 独立 db 索引 + `FLUSHDB`），并行无共享 | §2.3 |
| D7 | MariaDB 第一版不进矩阵，留 TODO | §5 |
| D8 | 编号 docs/44，落在 **master**（基础设施，不绑 Data 页的合并） | 本文 |
| D9 | 进 docs/07 一条 **ADR-028** | §6 |

## 2. 设计

### 2.1 目录

```
crates/swiss-it/
  Cargo.toml            # [package] swiss-it, publish = false；feature it（默认关）点亮全部 optional 依赖
  src/lib.rs            # #![cfg(feature = "it")] 下四个模块：engine / seed / exit / docker_raw
  src/engine.rs         # DOCKER_HOST / SWISS_IT_*_URL 解析，容器懒起，就绪等待，失败三段，启动 prune
  src/seed.rs           # Fresh（mysql 建库灌 seed、pg template、redis 索引池）与 seed 装载
  src/exit.rs           # 容器收尸的登记处：atexit 钩子 + it-reaper 看门狗的 spawn（78cd561）
  src/docker_raw.rs     # 一条裸阻塞 DELETE /containers/{id}?force=1&v=1（tcp/unix/npipe，5 s 超时）
  src/bin/it-reaper.rs  # 看门狗二进制：stdin 第一行 endpoint、其后每行一个容器 id，EOF=父死，逐个 DELETE
  src/bin/it-mcp-server.rs   # L3 的 stdio MCP server（手写 rmcp ServerHandler + AsyncRwTransport）
  seed/mysql/schema.sql  seed/mysql/data.sql
  seed/postgres/schema.sql  seed/postgres/data.sql
  seed/redis/keys.txt   # 一行一条命令（TAB 分隔，# 注释），UTF-8；3,037 行 = 3,017 条命令，落库 3,016 键
  tests/it/main.rs      # 唯一的测试二进制：#![cfg(feature = "it")]，八个 mod：smoke/seed/mysql/pg/redis/gateway/proc/reaper
  tests/it/gateway.rs   # L2 的 boot() 副本（adminapi 的 sandbox()+setup() 抽出来，放测试侧，不进 src/）
```

**一个测试二进制**是有意的：容器的 `OnceCell` 活在进程里，一个 `tests/*.rs` 一个进程——拆成五个文件
就是五次 MySQL 冷启动。`tests/it.rs` 顶上 `#![cfg(feature = "it")]`：不带 feature 时它是空二进制，
`cargo test --workspace` 在没有 Docker 的机器上照旧绿，编译时间几乎不变。

`swiss-it` 只在 `[workspace] members` 里，**没有任何 crate 依赖它**（实施后复核：全仓只有根 manifest 的
members 一行与它自己的 Cargo.toml 提到它）。它自己的 `[dependencies]` **全部 optional、只经 feature
`it` 点亮**（没有 feature 的机器编译的正是 crate 出现前的那三个空目标）：`swiss`（根，L2 的
`build_app`/`AppContext`）、`swiss-core`（L3 重放守护进程的环境清洗 `scrub_process_env`）、`swiss-host`
（`DbBrowser`/`RedisBrowser` trait 与 `ServerDef`）、`swiss-mcp`（带 `test-utils`；browser 与连接参数
构造）、`testcontainers`（`default-features = false`，不要它的 `ring`）、`testcontainers-modules`
（features `mysql`、`postgres`、`redis`）、`sqlx`（0.8，`mysql`+`postgres`，与 swiss-mcp 同版——在容器里
数连接与验 seed）、`redis`（0.27，`tokio-comp`）、`rmcp`（`client`、`server`、
`transport-streamable-http-client-reqwest`）、`tokio`、`futures-util`（docker worker 上的
`catch_unwind`）、`serde_json`、`axum` + `reqwest`（L2 的真监听与 admin API 调用），Windows 侧再一枚
`windows-sys`（Toolhelp32，L3 按进程名数子进程——不开 tasklist 子进程）。**全部走了
swiss-dependency-review**：feature 最小集与 `cargo tree -d` 的结果记在 I0 的提交说明（§2.8）。

### 2.2 引擎解析与生命周期（`engine.rs`）

```rust
pub enum Kind { Mysql, Postgres, Redis }

/// One engine per test process. The first test that needs it starts it; the it-reaper
/// watchdog child (plus the atexit hook on green) tears the container down when the
/// process exits - on every exit path. Resolution order is fixed and printed on failure.
pub async fn engine(kind: Kind) -> &'static Engine;

pub struct Engine {
    pub kind: Kind,
    pub host: String,          // "127.0.0.1"
    pub port: u16,             // whatever Docker published — never 3306/5432/6379 literal
    pub root_url: String,      // superuser URL, for CREATE DATABASE / FLUSHDB / counting connections
    _held: Option<Held>,        // one enum variant per module image; None when SWISS_IT_*_URL supplied it
}
```

解析顺序（D1）：

1. `SWISS_IT_MYSQL_URL` / `SWISS_IT_POSTGRES_URL` / `SWISS_IT_REDIS_URL` 有值 → 直接用，必须是能
   `CREATE DATABASE` 的账号（mysql root / pg superuser / redis 无 ACL 限制），底座启动时验一次。
2. 否则 testcontainers（认 `DOCKER_HOST`，unix socket 或 tcp）。就绪判据用 `testcontainers-modules`
   自带的（mysql 等 "ready for connections" 出现两次；postgres "database system is ready to accept
   connections"；redis "Ready to accept connections"），之后再各做一次真实握手（`SELECT 1` / `PING`）才算
   就绪。
3. 都不行 → **测试失败**，不跳过（D5：带上 `--features it` 就是在要求集成测试；Docker 缺席是失败）。
   失败信息固定三段：解析顺序里每一步为什么没成、`DOCKER_HOST` 当前值、指向
   `docs/44-wsl-docker-setup.md` §4 的那句 `wsl -d <distro> --exec true`。

镜像与版本写在 `engine.rs` 顶部的一张常量表里，**只此一处**：`mysql:8.4`、`postgres:17`、`redis:7`
（容器内端口 3306/5432/6379 也在同一张表里，只是用来向 Docker 要**随机发布端口**——测试从 API 读回，
任何地方都不写死）。mysql 容器起时给 `--character-set-server=utf8mb4 --collation-server=
utf8mb4_0900_ai_ci`（8.4 默认已是它，写明是防未来默认翻转悄悄改掉 seed 的世界），pg 给
`POSTGRES_INITDB_ARGS=--encoding=UTF8 --locale=C.UTF-8`——UTF-8 是 docs/41 定下的合同，seed 里的中文
和 emoji 要在两边都原样回来。启动超时给 **180 s**（默认 60 s——冷起首跑的时间花在拉镜像上，CI 正是
这条路）；日志就绪之后再做真实握手，握手窗口 **40 × 250 ms（10 s 上限）**。

容器**不带固定名字**（名字由 Docker 随机分配），带两个标签：`org.swiss-it.owned=1`——启动 prune 与运行中
`docker ps` 过滤的唯一范围，别的一概不碰；`org.swiss-it.pid=<启动进程 pid>`——仅供验收测试过滤与运维
对账，**刻意不做 prune 依据**：按 pid 判"进程已死"曾删掉并行运行（两个 worktree / 两条 CI）的活引擎，
78cd561 改为只看 age。所有 bollard 流量走**一条专职线程**上的 current_thread runtime（`on_worker`）：
libtest 给每个 `#[tokio::test]` 独立的 reactor，而 hyper 连接只能被注册它的那个 reactor 轮询——两个测试
runtime 各自发 docker 请求会直接 panic。调用方只拿成品值；worker 上的任务 panic 被 `catch_unwind`
接住，worker 不死，下一个引擎还用它。

**收尸三层**（`exit.rs` / `docker_raw.rs` / `it-reaper`，78cd561 定稿。testcontainers-rs 0.27.3 没有
ryuk，`ContainerAsync` 的 Drop 需要活的 tokio runtime，对住在 static 里的值永远不触发——所以收尸必须
自己拥有）：

1. **it-reaper 看门狗**管所有退出路径：第一个容器起来时 spawn 的子进程（stdout/stderr 拉去 null，
   二进制按 `current_exe()` 的 profile 目录找，缺失则响亮地退化为仅 atexit）。协议：stdin 第一行 docker
   endpoint，其后每行一个容器 id。父进程无论怎么死——正常退出、`std::process::exit(101)`、被杀、
   崩溃——stdin 管道都收到 EOF，reaper 对每个 id 做一次阻塞 force-DELETE。libtest 的失败路径正是
   `exit(101)`，Windows 上即 `ExitProcess`，**跳过 CRT 的 atexit 表**：绿路径的钩子恰恰在最常重跑的红跑
   上不触发，看门狗就是为它存在的。Windows 上 spawn 先要 `CREATE_NEW_PROCESS_GROUP |
   CREATE_BREAKAWAY_FROM_JOB`（Ctrl+C 与"job 关闭杀子"都够不着 reaper），job 拒绝 breakaway 则退回仅
   新进程组——连坐杀树时 reaper 一起死，交给第 3 层。
2. **atexit 钩子**是绿路径的快清洁工：对每个登记 id 做一次裸 DELETE；reaper 随后的重复 DELETE 读
   404、按已删处理。裸 DELETE 的线码在 `docker_raw.rs`：tcp/unix/npipe 直写 HTTP/1.1，5 s 读写超时
   （清洁工绝不能吊死在 hung 住的 dockerd 上），路径刻意**不带 `/v1.xx` 版本前缀**——dockerd 自己在升
   最低版本，无前缀则永远用它当前那个。钩子与 reaper 二进制共享同一份线码，不会漂移。
3. **启动 prune** 兜底连坐死亡（断电、把 breakaway 子进程一起带走的杀树）：每次起容器前，删掉**超过
   1 小时**的 `org.swiss-it.owned` 容器（force + 卷）。门槛选 age 而不是身份：整个第二条门热跑 ~20 s，
   一小时的门槛不可能把别人的活引擎误判成残骸，也不需要任何 pid-liveness FFI。验收在
   `tests/it/reaper.rs` 两条，都驱动真实的第二个测试进程：绿子进程跑完全程、父进程的引擎安然无恙
   （旧 pid-prune 就死在这）；`SWISS_IT_FORCE_RED=1` 的子进程（smoke 里该变量的唯一用途）exit 101 后
   ≤10 s 容器清空——收尸的不是它自己的代码。

### 2.3 每条测试自己的库（`Fresh`，D6）

```rust
/// A database that belongs to ONE test: created from the seed, named after the test,
/// dropped best-effort on Drop. Tests run in parallel with nothing shared.
pub struct Fresh { pub kind: Kind, pub name: String, pub def: serde_json::Value /* the MCP def */ }

pub async fn fresh(kind: Kind, tag: &str) -> Fresh;     // tag = the test's own name
```

- **mysql**：`CREATE DATABASE it_<tag>_<hex8> CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci`，
  root 灌 `seed/mysql/schema.sql` + `data.sql`（没有 template 机制），再 `GRANT ALL PRIVILEGES ON
  it_….* TO 'it'@'%'`（登录账号 `it`/`it` 由 `CREATE USER IF NOT EXISTS` 每引擎建一次）。`def` =
  `{"type":"mysql","host":…,"port":…,"user":"it","password":"it","database":"it_<tag>_<hex8>"}`——
  与 `adapters/mysql.rs:265` `mysql_connect_options` 读的字段一一对应。
- **postgres**：引擎首次就绪时建一次 `it_seed` 模板（`CREATE DATABASE it_seed OWNER it` + `ALTER
  DATABASE it_seed WITH IS_TEMPLATE TRUE`），之后每条测试 `CREATE DATABASE it_<tag>_<hex8> TEMPLATE
  it_seed OWNER it`（毫秒级，1,000 行的代价是一次文件拷贝）。模板以 `it` 账号灌 schema + data——克隆连
  所有权一起复制，admin 建的对象克隆后 `it` 反而读不了。模板带版本标记 `it_meta.seed_meta.version`
  （`SEED_VERSION`，现值 2，放在独立 schema 里、public 只留被测对象）：seed 文件一变即重建，指向复用
  服务器的 `SWISS_IT_POSTGRES_URL` 也永远不会端出旧 seed。`def` =
  `{"type":"pg","url":"postgres://it:it@127.0.0.1:<port>/it_<tag>_<hex8>"}`（`adapters/pg.rs:767` 读
  `url`）。
- **redis**：从 16 个 db 索引里领一个（`tokio::sync::Semaphore(16)` + 空闲栈，从 15 往下发），连接
  URL 带 `/n` 后 `FLUSHDB`，灌 `seed/redis/keys.txt`（500 条一批走 pipeline，3,000 键只需几个来回）；
  `def` = `{"type":"redis","host":…,"port":…,"db":n}`（`adapters/redis.rs:766` 读 `db`）。需要第二个
  库的测试（docs/43 M3 的 keyspace 目录）用 `fresh_redis_with_neighbor`：再领一个索引、SET 两个键
  （`neighbor:one`/`neighbor:two`）。归还时先 `FLUSHDB` 再还索引——后来的租约不可能继承上一个测试
  写过的键。
- 用户 `it`/`it`：mysql、pg 各建一个**非超级用户**给 def 用——L1/L2 打库时的权限面和真实部署一样；
  超级账号只有底座自己用（建库、数连接）。
- `Drop`：best-effort `DROP DATABASE`（pg 带 `WITH (FORCE)`，先踢掉 adapter 池里可能还开着的会话）；
  进程退出容器随 it-reaper 看门狗消失（被连坐杀掉的进程由下次启动的超时 prune 兜底），所以漏删不是
  泄漏。测试名进库名是为了失败时 `docker exec` 进去看时能对上号。

### 2.4 seed：按踩过坑的形状设计（D3）

红线：**只有中性名字**（`users`、`orders`……），没有任何真实系统的库名、表名、列注释——docs/40 D2。
以下每一行都对应一条曾经在 19998 上手工验过、今天没有回归测试的东西：

**mysql / postgres 共同的表**（两份 SQL 各写各的方言，形状对齐）：

| 表 | 为了证明什么 |
| --- | --- |
| `users` 8 行（`id BIGINT UNSIGNED AUTO_INCREMENT` / `bigint generated by default as identity`，`name` 含中文、emoji、变音符号与阿文，`email`（一行 NULL），`born_at DATETIME(6)` / `timestamptz`（µs 钉到 `.123456`），`balance DECIMAL(20,4)` / `numeric(20,4)`（含 `-0.0001` 与 `99999999.9999`），`flags JSON` / `jsonb`，`status ENUM('active','idle','banned')` / pg enum type `user_status`，`avatar BLOB` / `bytea`（PNG 魔数字节），`is_active`，`big BIGINT UNSIGNED` 存 `18446744073709551615` / `bigint` 存 `9223372036854775807`，另一行存 `9007199254740993`（2^53+1）） | 类型矩阵：BIGINT 越过 JS 安全整数后仍是原文（panel-ts D11 的坑），DECIMAL 不丢位，JSON 原样，NULL，二进制列的展示与导出，UTF-8 往返 |
| `orders` 6 行（复合主键 `(user_id, seq)`，FK → `users`，`note TEXT NULL`，含 NULL note 与带逗号的 note） | 复合主键寻址的编辑、外键结构页、NULL 的编辑与导出 |
| `events` 4 行（**无主键**，其中两行字节级相同——JSON 列也逐字节一样） | docs/22 的"rows are addressed by all columns; ambiguous rows are refused" |
| `wide`（60 列 `c01`–`c60`，3 行，稀疏 NULL） | 网格横向、列裁剪、结构页分页 |
| `big_rows`（1,000 行，递归 CTE 生成） | 分页 `offset/limit/total/nextPage`、排序、过滤各算子、导出流式 |
| 视图 `active_users`（`status='active'` 的 4 行） | 表列表里的 `type`，视图不可编辑 |
| pg 独有：schema `app`（`documents` 4 行——uuid 主键、`text[]`、跨 schema FK → `public.users`）与 `audit`（`log_entries` 5 行，partial index `… WHERE note IS NOT NULL`） | `?schema=` 过滤、多 schema 的补全与结构、索引谓词原样展示 |

**redis**（`keys.txt`，一行一条命令、TAB 分隔、`#` 注释；全文件 3,037 行 = 3,017 条命令，落库
**3,016** 个键，守卫测试钉死 DBSIZE）：string 5 个（中文 emoji、引号分号、zalgo、空串）、hash 50 字段
（一条 HSET）、list 100 元素（一条 RPUSH）、set 10 成员、zset 10 成员（分数含 `85.5` 与 `-100`）；TTL
一对——`SETEX` 3600 的与 `PERSIST` 掉的孪生；`tree:l1:l2:*` 等三层命名空间给树；
`bulk:key00001..03000` 共 **3,000** 键给 `SCAN` 分页。另一个 db 索引的 2 个键**不在 seed 里**——keyspace
目录测试用 `fresh_redis_with_neighbor` 现场领第二个索引、现场 SET（§2.3）。

seed 自己也有测试（I1，`tests/it/seed.rs` 7 条）：灌完后逐表数行、逐列验类型、验值（u64::MAX、
PNG 魔数、`1990-06-15 08:30:00.123456`、重复行计数），外加三条隔离证明（mysql/pg 两个 fresh 库互不可见、
redis 两个租约互不可见、100 次 `fresh(Redis)` 串行不耗尽索引池）——改 seed 的人先改它。

### 2.5 L1：三个 browser 打真库

每条测试的形状（`tests/it/{mysql,pg,redis}.rs` 的 `browser()` 帮手）：`fresh` 拿到 def 后按 adapter
自己的配方造 browser——def 先解析成 `ServerDef`，经 `mysql_connect_options` / `pg_connect_options` 造
`Lazy` 池（max 4 连接、acquire 5 s，与 adapter 同配方，所以池形行为也是产品的），
`MysqlBrowser::new(database, label, conn)` / `PgBrowser::new(label, conn)`；redis 走
`RedisEngine::new(&def, …).browser()`，def 上的策略位（`allowDestructive` 等）原样随行。直接调
`DbBrowser` / `RedisBrowser` trait 方法（`crates/swiss-host/src/dbbrowser.rs:708` 与 `:758`），断言 JSON。
不经 HTTP——路由层的合同 `dbbrowser_api.rs` 已经用 stub 守着，这里守的是 SQL 到真库的那一段。

每个 browser 一组（交付计数：mysql 16 条、pg 16 条、redis 9 条），命名点明 docs/22 的批次：

- **mysql / pg 各**：`list_tables`（分页、`grep`、pg 的 `schema` 过滤，对象带 Node 式 `type`
  table/view）；`read_table`（排序、每个过滤算子、`total`、`nextPage`；BIGINT/DECIMAL/JSON/二进制/NULL
  的单元格原文；无主键重复行与 60 列 wide 表各一条）；`describe_table`（列、主键、索引、外键、DDL 与
  `SHOW CREATE` / `pg_get_*` 一致，pg 含 partial index）；`export_table`（csv 与 json 两种，与 fetch 行数
  一致）+ `export_sql_dump`（流式 channel，头 `-- swiss SQL dump`、脚恢复 `SET FOREIGN_KEY_CHECKS=1`）；
  `import_table`（映射列的 insert 回读）；`apply_edits`（主键更新、复合主键更新、无主键歧义拒绝、插入、
  删除，每步回读；mysql 无主键删除按全列寻址 + LIMIT 1 剪一刀）；`run_query`（控制台读 + 写放行回读，
  无 LIMIT 的 SELECT 封顶 50 行并如实报 `limitApplied`）；`ddl_op`（建表、改名、删表，每步
  `list_tables` 回读）；`activity`（自己的连接出现在监控里 + kill 一条睡着的查询）；`completion`
  （表名、FROM 最近表的列）；docs/43 的 `list_databases`（**已并入并交付**）：主库 `primary`，mysql 侧
  `information_schema` 标 `system` 仍可浏览、账号读不到的库（`mysql`）干脆不列出，pg 侧 `postgres` 标
  `system`、其余库列出但 `browsable` 为假并给 `reason`（"bound to one database"），`template0/1` 服务端
  过滤；未知库名在拼 SQL 前拒绝。
- **redis**：`scan` 全键域 **3,016** 键不重不漏（DBSIZE 随第一页走）；每页每键带 `type` 与 `ttl`；
  `pattern` + `type` 服务端收窄（树的三层）；五种类型各一条 `read` + 两种 TTL（`SETEX` 窗口内、
  `PERSIST` 后 -1）；`run_command`（UTF-8 键值、`KEYS` 恒拒、def 未开 `allowEval` 时 `EVAL` 拒）；
  `run_pipeline` 的结构化编辑（hash 字段增删改、list 推入、DEL）每步回读；一条被拒的命令否决整批
  且不留半截；严格 def（无 `allowDestructive`）拒 `FLUSHDB`；两个 db 索引下的 keyspace 目录（neighbor
  索引列出但不可浏览，带 reason）。

增补（2026-09-22，`0349c41`）：L1 mysql 组在真服务器上冲出两处产品 bug 并当场随该提交修复——
`swiss-host` `dbbrowser.rs` 的 `typed_ph` 对 JSON 列的比较占位符原是字符串绑定（MySQL 8.4 实测按文本
比较、恒零行，过滤与无主键行寻址全死），改为 `CAST(? AS JSON)`（SET 子句不变）；`swiss-mcp`
`mysql_browser.rs` 补全的列查询读 `information_schema.column_name` 未加别名（MySQL 8
prepared-statement 元数据把无别名结果名大写，行键恒不匹配，FROM 表列补全线上恒空），已加别名。两处
单元 stub 都照不见；合同侧的对应记录在 docs/22 W0.2 处的增补行。

### 2.6 L2：三个 adapter 经真 rmcp client 走 `/mcp/<name>`

副本落在 `tests/it/gateway.rs`（不在 src/——它只服务测试二进制）：`boot(defs: Vec<(&str, Value)>) ->
Gateway`，在 `127.0.0.1:0` 上真监听（不是 `tower::oneshot`——rmcp client 要一个 URL），返回 `port`/
`base_url`、`token`、`registry`、`store`。副本沿用 adminapi 的两行隔离（`MCP_GATEWAY_HOME` 指临时目录 +
`MCP_GATEWAY_MASTER_KEY` 钉成固定 32 字节，token 固定），外加两条 daemon 启动才有的行为：
`Registry::new(60_000, calls)` 的健康探测，与 `registry.start_timer()` 的 **1 s idle-reap sweeper**——
第一版 boot 漏了它，懒 proc 的子进程永远不会被收回，这个坑是 L3 组自己抓住的。**根 crate 的
`tests/adminapi.rs` 不动**；两处漂移由 I5 的一条对照测试守（`gateway_rows_match_the_adminapi_contract`：
同一个 def，`/api/mcps` 行形状逐字段相等，`startedAt` 这类时钟字段豁免）。

每个引擎一组：

- 注册 def → `list_tools` 恰好是 `mysql_list_tables`/`mysql_query`（pg 三个：`pg_describe_table`/
  `pg_list_tables`/`pg_query`；redis 三个：`redis_command`/`redis_read`/`redis_scan`，与
  `adapters/{mysql,pg,redis}.rs` 的 `name:` 常量对照，不多不少）；每个工具各打一次真库、断言结果里 seed
  的行（BIGINT 以精确字符串走完整条链路，`张三 🙂` 原样）；`list_resources` / `read_resource` 与
  `{mysql,pg,redis}_resources.rs` 的形状一致——`mysql://<db>/<table>`（视图也在列）、
  `pg://<db>/public.users`、redis 的 keyspace overview 各读一条。
- **凭证是引用**（`credential_refs_close_the_loop_over_a_real_database`）：mysql def 的 `password` 与
  pg def `url` 里的密码段写成 `${secret://it-pass}`（redis 引擎无 ACL，无凭证可藏），secret 先经面板自己
  的 API 入库（`GET /api/secrets` 拿 rev → `PUT /api/secrets/it-pass`，且必须在 def 注册**之前**——
  adapter 在构建时解析引用），再注册 def、经工具打库成功；`/api/secrets` 只回名字与 rev，
  `/api/mcps/<name>/details` 的回显里是引用不是明文——docs/19/25 第一次在真库上闭环。
- **Hot-pluggable 的证明**（`stopping_an_mcp_releases_its_server_connections`，一个网关三引擎）：各打
  一枪让池里握着活的 `it` 连接并**先证基线非零**，经 `POST /api/mcps/<name>/stop`（现有路由是
  stop/start，不是 disable/enable）停掉，然后用底座的超级账号数——计数按本测试自己的库划界，免得并行
  组的连接背锅：mysql `…processlist WHERE user='it' AND db=?`、pg `pg_stat_activity WHERE usename='it'
  AND datname=$1`、redis 从 neighbor 索引的连接上 `CLIENT LIST` 数 `db=<n>` 条目——**必须为 0**
  （100 ms 间隔轮询，≤2 s）。再 `start`，工具复活。这条如果红，红的是产品，不是测试。
- 写入合同按现状钉住：`mysql_query` / `pg_query` **不是只读**——一条 `UPDATE` 放行并落库，mysql 报
  `affectedRows`、pg 报命令 tag 的 `rowCount`，各自忠于自己的线协议；测试回读验证。

### 2.7 L3：proc 用仓库自带的 MCP server

`src/bin/it-mcp-server.rs`：手写 rmcp `ServerHandler` + `AsyncRwTransport`（tokio 的 stdin/stdout），
`#[tokio::main(flavor = "current_thread")]`；启动即往 stderr 写一行中文，让"噪声不进协议流"从此有
载体。工具五个，全部为了测 adapter 而不是为了好看：

| 工具 | 为了证明什么 |
| --- | --- |
| `echo {text}` | 往返；UTF-8（中文、emoji）原样 |
| `blob {kb}` | 返回恰好 kb×1024 字节的确定性 JSON（钳在 1–512 KB；测试打 64 KB 并在本地按同一构造器重建全文，要**相同的字节**不是相等的 JSON）——`proc` 转发路径是 `&RawValue` |
| `sleep {ms}` | 超时与取消 |
| `fail {code}` | 错误映射到 MCP error，不把进程打死 |
| `env` | 返回子进程看到的环境变量名列表——docs/16 H1 的清洗（`CLAUDECODE`、`NO_COLOR`、`CI` 等）在 proc 子进程上**成立**；实施前这条只在 daemon 启动路径上有测试，现在 L3 直接钉住 |

测试用 `env!("CARGO_BIN_EXE_it-mcp-server")` 拿路径（同 package 的 bin，cargo 保证），def =
`{"type":"proc","command":"<path>","args":[],"idleMs":…}`（`idleMs: 0` = 不收回，只有 stop 能带走
子进程——杀树那条正是这么设的）。一组 7 条，整组用一把锁串行：子进程按 exe 名数**整个进程表**，两个
L3 并行会数到对方的（Windows 用 Toolhelp32 快照、unix 读 `/proc`，直调 API，不开 tasklist 子进程；读
不出按"巨多"算，绝不让"数不了"伪装成"没了"）：

- **懒**：网关起来、def 注册后，进程表里没有 `it-mcp-server`；第一次 `list_tools` 之后有；无流量
  `idleMs: 1500` 后 8 s 窗口内被收回；下一次调用再唤醒一个新子进程。
- stop → 子进程树被杀（Windows 上 Job Object，unix 上进程组——两边各跑各的，`cfg` 分开实现）。
- 上表五个工具各一条：echo 顺带钉 stderr 噪声不进 stdout 协议流（启动那行中文 + 工具照常返回）；fail
  之后进程活着且下一个调用照常答；env 那条先种入 `NO_COLOR`/`CI`/`CLAUDECODE`/`CLAUDE_CODE_IT_L3` 与
  存活标记 `SWISS_IT_L3_MARKER`，重放 `swiss_core::env::scrub_process_env()`——四个噪声名不出现在子
  进程的环境名里，标记还在（继承本身没断）。

### 2.8 门禁、CI、依赖重量（D5）

**门禁**（已交付，I0：AGENTS.md 的命令块、`.agents/skills/swiss-verify/SKILL.md` 的选择规则、
`CONTRIBUTING.md` 的"The gates"各一条）：

```
cargo test --workspace                          # 单元与进程内集成；任何机器
cargo test -p swiss-it --features it            # 真库、真子进程；需要 Docker 或 SWISS_IT_*_URL
```

第二条**必跑**的触发面（swiss-verify 的规则原文）：diff 触及
`crates/swiss-mcp/src/adapters/{mysql,pg,redis}*.rs`、`sql.rs`、`resources.rs`、`proc.rs`、
`crates/swiss-host/src/dbbrowser.rs`、`crates/swiss-data/src/dbbrowser_api.rs`、`crates/swiss-core/src/secure/`、
`src/app.rs`、`src/mcp_link.rs`，或 `crates/swiss-it/**` 自身。其余改动可不跑，但**提交说明要说明没跑**。
`scripts/deploy.ps1` 也已落地同一条（I7 `929af06`）：在单元门与 clippy 之间跑 gate 2，为这一次运行
显式设 `$env:DOCKER_HOST = 'tcp://127.0.0.1:2375'`、跑完即删——部署机就是这台、Docker 在 WSL 里，这一步
因此不依赖用户级变量对部署 shell 可见；非零退出即 `integration gate failed - production left untouched`；
`-SkipGates` 连同其它门一并跳过。

**CI**（`.github/workflows/build.yml`）的 `integration` job——已交付，形状就是：

```yaml
  integration:
    runs-on: ubuntu-latest            # dockerd 自带；mac/Windows runner 不跑——SQL 不分平台
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with: { key: integration }
      - run: sudo apt-get update && sudo apt-get install -y cmake clang
      - run: cargo test -p swiss-it --features it --locked
      - if: failure()
        run: docker ps -a && docker logs $(docker ps -aq --filter ancestor=mysql:8.4) 2>&1 | tail -50
```

job 与 workflow 其余部分同源触发（push 到 `master`/`main`、PR、`v*` tag），`release` 的
`needs: [build, panel, deny, integration]` 已交付——没有真库证据的 tag 不发。预算：整 job < 4 min
（三镜像拉取 ~40 s，冷启动 ~20 s，套件 < 1 min；实际记录：本机热跑整门 ~20 s、66 条，见 `929af06`
的门禁记录与 ADR-028）。

**依赖重量**：`testcontainers`（MIT）+ `testcontainers-modules`（MIT）经 `bollard`（Apache-2.0）拉进
hyper/tokio 的一份——dev-only。必须同时成立：

1. `cargo tree -e normal,build -p swiss` **与实施前逐字相同**（把这条 diff 贴进 I0 的提交说明）——
   二进制的依赖图一根毛都不能动。
2. CI 的"linked twice"检查 I0 已改为 `cargo tree -d -e normal,build --locked`（理由写在该 step 注释
   里，引本文 §2.8）——bollard 带进 dev 图的第二个 hyper 不再能红掉一个不发货的图。该 step 本就不靠
   退出码：裸 `cargo tree -d` 恒 exit 0，且实施前树上已有 RustCrypto 版本散布，所以先打全量，再只在
   tokio/rustls/native-tls/openssl(-sys)/hyper/aws-lc-rs 出现在重复列**首列**时才红。
3. `cargo deny check` 绿——`[graph] all-features = false`，`it` 默认关，deny 看到的是发货图；但 license
   表仍会列出 dev-deps，`THIRD_PARTY_NOTICES.md §3` 的表按其自己的规则（"dev-only dependencies are
   excluded"）**不加**这些 crate。

### 2.9 mutation 检查：测试要能咬人

对一个只交付测试的 spec，"先红后绿"要换一种证法。每组 L1/L2/L3 提交里做一次**记录在提交说明里的
突变**：把被测代码里一处改坏（例如 `mysql_browser.rs` 分页的 `<` 改 `<=`；`redis_browser.rs` 的
`SCAN COUNT` 改成 1 后跳过一批；`proc.rs` 的 idle 计时器不重置），跑 `cargo test -p swiss-it --features it`，
**贴红的那条测试名**，再改回来贴绿。一组至少一次；没有任何一条测试因此变红的组不许提交。

## 3. 工作项（每项一个 commit，顺序即依赖）

| # | 内容 | 验收 |
| --- | --- | --- |
| I0 | `crates/swiss-it` 骨架：`Cargo.toml`（feature `it`，dev-deps 经 swiss-dependency-review）、`engine.rs`（解析顺序、懒起、就绪、失败三段）、`tests/it.rs` 里每引擎一条 smoke（`SELECT 1` / `PING`）；CI dup-check 改 `-e normal,build`；AGENTS.md / CONTRIBUTING / swiss-verify / docs/08 各加一行 | ① 无 Docker 且无 `DOCKER_HOST`：`cargo test --workspace` 与实施前一样绿；`cargo test -p swiss-it --features it` 红，且输出含三段失败信息。② 有 Docker：三条 smoke 绿，`docker ps` 里三个容器，测试进程退出后 ≤10 s 内消失。③ `cargo tree -e normal,build -p swiss` 前后逐字相同（提交说明贴 diff 为空）。④ `cargo deny check` 绿 |
| I1 | seed 三份 + `seed.rs` + `Fresh`（mysql 建库灌 seed、pg template、redis 索引池）+ seed 守卫测试 | 每引擎一条"灌完逐表数行、逐列验类型"的测试；两条测试并行各自 `fresh` 互不可见（一条写入、另一条数不到）；100 次 `fresh(Redis)` 串行不耗尽索引池 |
| I2 | L1 mysql browser 组 | §2.5 每个 trait 方法至少一条；mutation 记录 |
| I3 | L1 pg browser 组（含多 schema） | 同上 |
| I4 | L1 redis browser 组 | 同上；`SCAN` 3,000 键不重不漏 |
| I5 | `gateway.rs` + L2 三组 + 凭证引用 + **hot-plug 连接归零** + 与 `tests/adminapi.rs` 的对照测试 | §2.6 全部；连接归零那条若红，开 issue 记为产品缺陷，测试**保留为红**不许改弱 |
| I6 | `it-mcp-server` + L3 组 | §2.7 全部；Windows 与 unix 的杀树各有断言（CI 的 ubuntu job 跑 unix 那半，本机跑 Windows 那半，提交说明写明各在哪跑过） |
| I7 | CI `integration` job + `release` 的 `needs` + `deploy.ps1` 门禁 + README 表行 + docs/08 账本 + docs/07 **ADR-028** + 本文状态头 | CI 首跑绿是 owner 建仓后的事，本项交付的是文件；本机用 `act` 不做——写明未验 |

门禁命令（每次提交前）：

```
cargo test --workspace
cargo test -p swiss-it --features it
cargo clippy --workspace --all-targets --features it -- -D warnings     # swiss-it 的测试代码也过 clippy
cargo tree -d -e normal,build
cargo deny check
```

## 4. 验收（整体）

1. 一台只有 Rust 工具链 + Docker 的干净机器（CI 的 ubuntu runner 就是），`cargo test -p swiss-it --features it`
   从零到绿，不需要任何预先存在的数据库、账号、环境变量。
2. 同一条命令在本机（Windows，`DOCKER_HOST` 指向 WSL）绿；提交说明贴两边的用例数。
3. `cargo test --workspace` 在**没有** Docker 的机器上仍然绿，且用时与实施前差 < 10%。
4. §2.9 的 mutation 记录每组至少一条。
5. 二进制依赖图逐字未变（I0 ③）；`swiss.exe` 字节数与基线相同。
6. 任何 seed 文件、测试名、断言文本里没有真实库名（`git grep -i -E 'acme_|ops_dev'` 在 `crates/swiss-it` 为零）。
7. Hot-plug 连接归零那条测试存在且绿——或存在且红并带 issue 号。

## 5. 不做什么

- **MariaDB**（D7）：`mariadb:11` 作为第四引擎留 TODO；docs/29 把它算 MySQL 兼容直连，进矩阵时只是
  `engine.rs` 常量表多一行 + 同一组 L1 用两个 Kind 跑。
- **L4 整机 golden capture**（docs/08 的老愿望）：`Gateway::boot` 是它的地基，但 `/api/*` 全量快照
  与比对是另一个 spec。
- **面板的 vitest 不打真库**：面板的合同在 `/api/db` 的形状，stub 守着；真库属于 L1。
- **Windows / macOS 的 CI 集成 job**：SQL 与 RESP 不分平台；proc 的 Windows 半边在本机跑。
- **嵌入式二进制**（`postgresql_embedded` 之类）与 **Docker Desktop**：§0.3 说明了为什么。
- **真实库的 dump**、19999 上任何连接的 def：docs/40 D2。
- **在 WSL 里跑 cargo**：`/mnt/c` 上的 target 目录慢一个数量级；测试进程留在 Windows，只有容器在 WSL。
- 不动 `tests/adminapi.rs`、`tests/http_adapter.rs`——它们是单元门禁的一部分，本文只抽副本。

## 6. ADR-028（定稿已随 I7 `929af06` 落到 docs/07-decisions.md：Accepted 2026-09-22，含 options /
decision / costs / what shipped 与逐项 hash；以下为 spec 定稿时的草案，留作底稿）

**ADR-028 — 两条测试门：单元的一条任何机器都绿，集成的一条要 Docker（docs/44）**

Status: Proposed (2026-09-22)；docs/07 定稿为 Accepted。

Context：7,739 行数据库 adapter/browser 代码没有一条真库测试；每次重构的证据是 19998 上对着不能入库
的真实库手工走一遍。四个选项：

| 选项 | 结果 |
| --- | --- |
| 继续手工走查 | 零可重跑证据；docs/40 D2 让截图也进不了库 |
| 嵌入式二进制（pg 有、mysql 无、redis 无 Windows） | 只能覆盖一个引擎，三套机制 |
| Docker + testcontainers，**集成测试并入 `cargo test --workspace`** | 没有 Docker 的机器（mac/Windows runner、任何路人）连单元测试都红 |
| **Docker + testcontainers，独立 crate + feature `it`，第二条门**（选它） | 单元门任何机器绿；集成门在 Docker 存在处必过；二进制依赖图零变化 |

Decision：`crates/swiss-it`，feature `it` 默认关；AGENTS.md 门禁两条；CI 一个 ubuntu `integration` job，
`release` 依赖它；触及 §2.8 文件的改动必过第二条门。

Consequences：开发机多一个硬依赖（WSL 里的 dockerd，一次性安装，docs/44-wsl-docker-setup.md）；
`cargo test --workspace` 不再是"全部证据"——提交说明要说明第二条门跑没跑。换来的是：
`mysql_browser.rs` 这类文件第一次可以放心地整段重写。
