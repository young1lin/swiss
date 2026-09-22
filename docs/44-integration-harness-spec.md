# 44 — 集成测试底座：真库、自建 MCP、任何机器一条命令

> 状态：**已实施**（branch `integration-harness`，2026-09-22 实施完毕；spec 定稿 2026-09-22；基线 `946b921`
> （master）。逐项：I0 `062928a`、I1 `4740a9b`、I2 `0349c41`、I3 `239895e`、I4 `f707247`、清理补丁（it-reaper +
> 按 age 的 prune）`78cd561`、I5 `3cf62a8`、I6 `2145a44`、I7 见本提交（I5/I6 于同日重做以各自携带
> Cargo.lock——原 `64a8ab4`/`86fa047` 的树检出即 `--locked` 失败）。实施走了 swiss-dependency-review →
> swiss-verify → swiss-review 流程。Docker 的安装在 [44-wsl-docker-setup.md](44-wsl-docker-setup.md)）。
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
  Cargo.toml            # [package] swiss-it, publish = false；feature it = []
  src/lib.rs            # 底座：Engine（三引擎）、Fresh（每测试隔离）、Gateway（进程内网关）、seed 装载
  src/engine.rs         # DOCKER_HOST / SWISS_IT_*_URL 解析，容器懒起，就绪等待，失败信息
  src/seed.rs           # 把 seed/ 里的文件灌进一个 fresh 库；pg 的 template 建立
  src/gateway.rs        # tests/adminapi.rs 的 sandbox()+setup() 抽成可复用的 boot(defs) -> Gateway
  src/bin/it-mcp-server.rs   # L3 的 stdio MCP server（rmcp server + transport-io）
  seed/mysql/schema.sql  seed/mysql/data.sql
  seed/postgres/schema.sql  seed/postgres/data.sql
  seed/redis/keys.txt   # 一行一条命令，UTF-8
  tests/it.rs           # 唯一的测试二进制：#![cfg(feature = "it")]，mod mysql; mod postgres; mod redis; mod adapters; mod proc;
```

**一个测试二进制**是有意的：容器的 `OnceCell` 活在进程里，一个 `tests/*.rs` 一个进程——拆成五个文件
就是五次 MySQL 冷启动。`tests/it.rs` 顶上 `#![cfg(feature = "it")]`：不带 feature 时它是空二进制，
`cargo test --workspace` 在没有 Docker 的机器上照旧绿，编译时间几乎不变。

`swiss-it` 只在 `[workspace] members` 里，**没有任何 crate 依赖它**；它自己的 `[dependencies]` 是
`swiss`（根，path `../..`）、`swiss-core`/`swiss-mcp`（带 `test-utils`）、`testcontainers`、
`testcontainers-modules`（features `mysql`、`postgres`、`redis`）、`sqlx`（与 swiss-mcp 同版同 feature，
用来在容器里数连接与验 seed）、`redis`（同上）、`rmcp`（`client`、`transport-streamable-http-client-reqwest`、
`server`、`transport-io`）、`tokio`、`serde_json`。**全部走 swiss-dependency-review**：每个都是
dev-only，但仍要写清 feature 最小集与 `cargo tree -d` 的结果（§2.8）。

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
    _container: Option<ContainerAsync<…>>,   // None when SWISS_IT_*_URL supplied it
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

镜像与版本写在 `engine.rs` 顶部的一张常量表里，**只此一处**：`mysql:8.4`、`postgres:17`、`redis:7`。
mysql 容器起时给 `--character-set-server=utf8mb4 --collation-server=utf8mb4_0900_ai_ci`，
pg 给 `POSTGRES_INITDB_ARGS=--encoding=UTF8 --locale=C.UTF-8`——UTF-8 是 docs/41 定下的合同，seed
里的中文和 emoji 要在两边都原样回来。

### 2.3 每条测试自己的库（`Fresh`，D6）

```rust
/// A database that belongs to ONE test: created from the seed, named after the test,
/// dropped best-effort on Drop. Tests run in parallel with nothing shared.
pub struct Fresh { pub kind: Kind, pub name: String, pub def: serde_json::Value /* the MCP def */ }

pub async fn fresh(kind: Kind, tag: &str) -> Fresh;     // tag = the test's own name
```

- **mysql**：`CREATE DATABASE it_<tag>_<hex8> CHARACTER SET utf8mb4`，然后把 `seed/mysql/schema.sql`
  + `data.sql` 灌进去（seed 控制在 <200 ms；没有 template 机制）。`def` =
  `{"type":"mysql","host":…,"port":…,"user":"it","password":"it","database":"it_<tag>_<hex8>"}`——
  与 `adapters/mysql.rs:265` `mysql_connect_options` 读的字段一一对应。
- **postgres**：引擎首次就绪时建一次 `it_seed`（灌 schema + data），之后每条测试
  `CREATE DATABASE it_<tag>_<hex8> TEMPLATE it_seed`（毫秒级）。`def` = `{"type":"postgres","url":
  "postgres://it:it@127.0.0.1:<port>/it_<tag>_<hex8>"}`（`adapters/pg.rs:765` 读 `url`）。
- **redis**：从 16 个 db 索引里领一个（`tokio::sync::Semaphore(16)` + 空闲位图），`SELECT n` 后
  `FLUSHDB`，灌 `seed/redis/keys.txt`；`def` = `{"type":"redis","host":…,"port":…,"db":n}`
  （`adapters/redis.rs:766` 读 `db`）。需要第二个库的测试（docs/43 M3 的 keyspace 目录）再领一个。
  归还时再 `FLUSHDB`。
- 用户 `it`/`it`：mysql、pg 各建一个**非超级用户**给 def 用——L1/L2 打库时的权限面和真实部署一样；
  超级账号只有底座自己用（建库、数连接）。
- `Drop`：best-effort `DROP DATABASE`；进程退出容器随 it-reaper 看门狗消失（被连坐杀掉的进程由
  下次启动的超时 prune 兜底），所以漏删不是泄漏。测试名进库名
  是为了失败时 `docker exec` 进去看时能对上号。

### 2.4 seed：按踩过坑的形状设计（D3）

红线：**只有中性名字**（`users`、`orders`……），没有任何真实系统的库名、表名、列注释——docs/40 D2。
以下每一行都对应一条曾经在 19998 上手工验过、今天没有回归测试的东西：

**mysql / postgres 共同的表**（两份 SQL 各写各的方言，形状对齐）：

| 表 | 为了证明什么 |
| --- | --- |
| `users`（`id BIGINT AUTO_INCREMENT` / `bigint generated always as identity`，`name` 含中文与 emoji，`email`，`created_at DATETIME(6)` / `timestamptz`，`balance DECIMAL(20,4)` / `numeric(20,4)`，`flags JSON` / `jsonb`，`status ENUM` / pg enum type，`avatar BLOB` / `bytea`，`is_active`，`big BIGINT UNSIGNED` 存 `18446744073709551615` / `bigint` 存 `9223372036854775807`） | 类型矩阵：BIGINT 越过 JS 安全整数后仍是原文（panel-ts D11 的坑），DECIMAL 不丢位，JSON 原样，二进制列的展示与导出，UTF-8 往返 |
| `orders`（复合主键 `(user_id, seq)`，FK → `users`，`note TEXT NULL`） | 复合主键寻址的编辑、外键结构页、NULL 的编辑与导出 |
| `events`（**无主键**，含两行完全相同） | docs/22 的"rows are addressed by all columns; ambiguous rows are refused" |
| `wide`（60 列） | 网格横向、列裁剪、结构页分页 |
| `big_rows`（1,000 行） | 分页 `offset/limit/total/nextPage`、排序、过滤各算子、导出流式 |
| 视图 `active_users` | 表列表里的 `type`，视图不可编辑 |
| pg 独有：schema `app` 与 `audit`，跨 schema 的 FK，`text[]`、`uuid`、partial index | `?schema=` 过滤、多 schema 的补全与结构 |

**redis**（`keys.txt`，一行一条命令）：五种类型各若干（string 含中文与二进制安全字节、hash 50 字段、
list 100 元素、set、zset）；一个带 TTL 的键、一个 `PERSIST` 的；`ns:sub:leaf` 三层命名空间给树；
一个命名空间下 **3,000** 个键给 `SCAN` 分页；另一个 db 索引里放 2 个键给 `INFO keyspace` 目录；
docs/45 §2.7 补的 **stream ×4**（`stream:ticks` 1 万条 + 消费组 pending 7、`stream:empty`、`stream:one`、
`stream:ragged` 交错字段）在 `seed.rs` 里生成，不进 keys.txt。

seed 自己也有测试（I1）：灌完后逐表数行、逐列验类型，是"seed 还在"的守卫——改 seed 的人先改它。

### 2.5 L1：三个 browser 打真库

每条测试的形状：`let db = fresh(Kind::Mysql, "grid_pages").await; let b = MysqlBrowser::new(&def(db.def), "m")?;`
直接调 `DbBrowser` / `RedisBrowser` trait 方法（`crates/swiss-host/src/dbbrowser.rs:743-`），断言 JSON。
不经 HTTP——路由层的合同 `dbbrowser_api.rs` 已经用 stub 守着，这里守的是 SQL 到真库的那一段。

每个 browser 一组，每组按 trait 方法各至少一条，命名点明 docs/22 的批次：

- **mysql / pg 各**：`list_tables`（分页、`grep`、pg 的 `schema`）；`fetch` 页（排序、每个过滤算子、
  `total`、`nextPage`；BIGINT/DECIMAL/JSON/二进制/NULL 的单元格原文）；`structure`（列、主键、索引、
  外键、DDL 与 `SHOW CREATE` / `pg_get_*` 一致）；`export`（csv 与 sql 两种，流式，与 `fetch` 行数一致）；
  `completion`（表名、FROM 最近表的列）；`activity`（自己的连接出现在监控里）；编辑（主键更新、
  复合主键更新、无主键歧义拒绝、插入、删除，每步回读）；DDL 最小集（建表、改名、删表，每步 `list_tables`
  回读）；docs/43 的 `list_databases`——**如果 `data-full-access` 已并入**：主库 `primary`、其余
  `browsable` 与 `reason`、外库只读；否则只断言 trait 默认的空目录，并在提交说明里写明。
- **redis**：`scan` 分页跨 3,000 键不重不漏；树的三层；五种类型各一条 `read`；TTL 的两种；
  `run_pipeline` 的结构化编辑（hash 字段增删改、list 推入、zset 分数）每步回读；UTF-8 键与值原样；
  两个 db 索引下的 keyspace 目录（同上，视 docs/43 是否并入）。

### 2.6 L2：三个 adapter 经真 rmcp client 走 `/mcp/<name>`

`gateway.rs` 把 `tests/adminapi.rs:50-112` 的 `sandbox()`+`setup()` 抽成
`Gateway::boot(defs: Vec<Value>) -> Gateway`，在 `127.0.0.1:0` 上真监听（不是 `tower::oneshot`——
rmcp client 要一个 URL），返回 `base_url`、`token`、`registry`、`store`。**根 crate 的 `tests/adminapi.rs`
不动**：抽出来的是副本，放在 swiss-it 里；两处漂移由 I5 的一条对照测试守（同一个 def 两边 `/api/mcps`
的回显相等）。

每个引擎一组：

- 注册 def → `list_tools` 恰好是 `mysql_list_tables`/`mysql_query`（pg 三个、redis 三个，与
  `adapters/{mysql,pg,redis}.rs` 的 `name:` 常量对照）；每个工具各打一次真库、断言结果里 seed 的行；
  `list_resources` / `read_resource` 与 `{mysql,pg,redis}_resources.rs` 的形状一致。
- **凭证是引用**：一条 def 的 `password`（redis）/ `url`（pg）写成 `${secret://it-pass}`，先经
  `/api/secrets` 存进 vault，再经工具打库成功，且 `/api/mcps` 的回显与 `/api/db` 的任何答复里**不出现**
  明文——docs/19/25 第一次在真库上闭环。
- **Hot-pluggable 的证明**：连接建立后，经 `/api/mcps/<name>/disable`（现有路由名以代码为准）停掉，
  然后用底座的超级账号到容器里数：mysql `SELECT COUNT(*) FROM information_schema.processlist WHERE
  user='it'`、pg `SELECT count(*) FROM pg_stat_activity WHERE usename='it'`、redis `CLIENT LIST` 里
  `db=<n>` 的条目——**必须为 0**（给一个短的轮询窗口，≤2 s）。再 enable，工具再次可用。这条如果红，
  红的是产品，不是测试。
- `sql.rs` 的只读守卫在真库上：`mysql_query` 一条 `UPDATE` 被拒（或按当前合同放行——以代码为准，
  测试写下现状），`pg_query` 同。

### 2.7 L3：proc 用仓库自带的 MCP server

`src/bin/it-mcp-server.rs`：rmcp `ServerHandler` + `transport-io`（stdio），工具五个，全部为了测 adapter
而不是为了好看：

| 工具 | 为了证明什么 |
| --- | --- |
| `echo {text}` | 往返；UTF-8（中文、emoji）原样 |
| `blob {kb}` | 返回 kb KB 的 JSON——`proc` 转发路径是 `&RawValue`，断言网关吐出的字节与 server 写出的字节**相同**（不是相等的 JSON，是相同的字节） |
| `sleep {ms}` | 超时与取消 |
| `fail {code}` | 错误映射到 MCP error，不把进程打死 |
| `env` | 返回子进程看到的环境变量名列表——docs/16 H1 的清洗（`CLAUDECODE`、`NO_COLOR`、`CI` 等）在 proc 子进程上**成立**；今天这条只在 daemon 启动路径上有测试 |

测试用 `env!("CARGO_BIN_EXE_it-mcp-server")` 拿路径（同 package 的 bin，cargo 保证），def =
`{"type":"proc","command":"<path>","args":[]}`。一组：

- **懒**：网关起来、def 注册后，`sysinfo`/进程表里没有 `it-mcp-server`；第一次 `list_tools` 之后有；
  idle 超时（def 上把 idle 设成 1 s）后又没有。
- disable → 子进程树被杀（Windows 上 Job Object，unix 上进程组——两边各跑各的，`cfg` 分开断言）。
- 上表五个工具各一条。
- stderr 噪声不进 stdout 协议流：server 往 stderr 写一行中文，工具照常返回。

### 2.8 门禁、CI、依赖重量（D5）

**门禁**（AGENTS.md 的块加一行，`.agents/skills/swiss-verify/SKILL.md` 加一条选择规则，
`CONTRIBUTING.md` 的"The gates"加一行）：

```
cargo test --workspace                          # 单元与进程内集成；任何机器
cargo test -p swiss-it --features it            # 真库、真子进程；需要 Docker 或 SWISS_IT_*_URL
```

第二条**必跑**的触发面（swiss-verify 的规则原文）：diff 触及
`crates/swiss-mcp/src/adapters/{mysql,pg,redis}*.rs`、`sql.rs`、`resources.rs`、`proc.rs`、
`crates/swiss-host/src/dbbrowser.rs`、`crates/swiss-data/src/dbbrowser_api.rs`、`crates/swiss-core/src/secure/`、
`src/app.rs`、`src/mcp_link.rs`，或 `crates/swiss-it/**` 自身。其余改动可不跑，但**提交说明要说明没跑**。
`scripts/deploy.ps1` 的门禁段加同一条（部署机就是这台，Docker 在 WSL 里）。

**CI**（`.github/workflows/build.yml`）加一个 job：

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

`release` job 的 `needs` 加上 `integration`——没有真库证据的 tag 不发。预算：整 job < 4 min
（三镜像拉取 ~40 s，冷启动 ~20 s，套件 < 1 min）。

**依赖重量**：`testcontainers`（MIT）+ `testcontainers-modules`（MIT）经 `bollard`（Apache-2.0）拉进
hyper/tokio 的一份——dev-only。必须同时成立：

1. `cargo tree -e normal,build -p swiss` **与实施前逐字相同**（把这条 diff 贴进 I0 的提交说明）——
   二进制的依赖图一根毛都不能动。
2. CI 的"linked twice"检查今天跑的是裸 `cargo tree -d`，**含 dev-deps**。它守的是二进制体积，所以
   改成 `cargo tree -d -e normal,build`（I0 一并改，理由写进 workflow 注释）。否则 bollard 若带来第二个
   hyper，会为一个不发货的图红掉 CI。
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

## 6. ADR-028（草案，I7 落到 docs/07，"what shipped / cost" 两栏那时填）

**ADR-028 — 两条测试门：单元的一条任何机器都绿，集成的一条要 Docker（docs/44）**

Status: Proposed (2026-09-22)。

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
