# docs/45 实施提示词 — Data 页的 Redis Stream（最新优先、翻页、Follow、消费组）

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 前提，先验，不满足就停下报告

1. `crates/swiss-it/src/seed.rs` 在 master 上存在（`integration-harness` 于 2026-09-22 以 `f3b6899` 合并，seed 与
   gate 2 都在里面）。没有就停：本任务的 S0 改的就是那个文件。owner 的 filter-repo 做完之后短 hash 会变，
   按 subject `Merge branch 'integration-harness'` 找。
2. Docker 可达：`$env:DOCKER_HOST` 是 `tcp://127.0.0.1:2375`，`curl.exe -s http://127.0.0.1:2375/_ping` 回 `OK`；
   不通先 `wsl -d <distro> --exec true`。gate 2 从 S0 起每一项都要跑，没有 Docker 做不了。
3. `docs/45-redis-stream-spec.md` 状态头是"待实施"，且 `git grep -c 'acme_\|ops_dev' -- docs/45*` 为零。

## 工作目录

```
git worktree add .agents\worktrees\stream -b redis-streams master
cd .agents\worktrees\stream
cd crates\swiss-panel\panel && npm ci && cd ..\..\..
```

基线 master 就是你建 worktree 时的 tip（`git rev-parse --short master`，写进 S0 的提交说明）。
**不要 `cd` 回主 checkout；不碰 19999；不碰别的 worktree。** PowerShell 里 `cd` 混在链式命令里会打断后面的
相对路径——每条命令自己从 worktree 根起。

## 任务

`docs/45-redis-stream-spec.md` 的 S0 → S4。一句话：Data 页打开一个 Redis stream 键，看到的是**最新的 100 条**、
按字段分列的表格；能往老翻页（id 游标）；Follow 开关每秒追新，DOM 里最多 500 行（环形缓冲，用户滚下去了就
不拽人）；消费组只读一张表；网关侧什么都不持有；每个功能点**单测 + 集成测试 + 面板测试 + 真浏览器**四层都有；
每个纯函数、两个 trait 方法、面板文件头都写"为什么这么设计"。

## 先读这些（按顺序，别跳）

1. `AGENTS.md` 全文——四条产品属性；"Ruthlessly small" 是本任务最容易被面板轮询破坏的一条。
2. `docs/45-redis-stream-spec.md` 全文，§2.1（一条命令两个方向）、§2.3（环形缓冲与钉顶）、§2.5（测试矩阵）
   读两遍。矩阵里每一格都是一条要写的测试，不是建议。
3. `crates/swiss-mcp/src/adapters/redis.rs:331-350`（`stream_entries`）、`:368-380`（`RedisReadClient`，
   `call` 就是你发 XREVRANGE / XINFO 的口）、`:405-566`（`type_aware_read`，stream 分支**保留**给 MCP 工具，D8）、
   `:120-200`（命令策略——XREAD/XREADGROUP 禁用是设计，不是 bug）。
4. `crates/swiss-mcp/src/adapters/redis_browser.rs` 全文——`RedisBrowser` 的实现、`self.conn.get()` 的租借、
   既有的单测 stub 怎么写。
5. `crates/swiss-host/src/dbbrowser.rs:758-777`（`RedisBrowser` trait）和 docs/43 M3 给 `list_databases` 加默认
   实现的那次提交（`git log -S list_databases -- crates/swiss-host/src/dbbrowser.rs`）——两个新方法照它的样子。
6. `crates/swiss-data/src/dbbrowser_api.rs`：`key_route` / `key()`（`:925-945`）、`lease_redis`、路由表
   （`:1127-1140`）、`StubRedis` 与它的单测（`:1500-`、`:2980-3010`）。
7. `crates/swiss-panel/panel/src/data-browsers.ts:280-345`（`dbRenderRedisValue`，你的分流点在 `:325`）、
   `:354`（`dbRedisTypedTable`，表格的 house 写法）、`:432`（`dbRedisDisplayText`）、`:454`（`dbRedisCellMenu`）；
   `data-activity.ts:26-50` 与 `:175-190`（**只在 tab 活跃时轮询**的既有写法，Follow 照抄这个自守规则）；
   `terminal-core.ts:286-297`（`isPinned` 的判断与理由，你要写它的镜像 `isTopPinned`）；`db-state.ts`（tab 状态
   放哪）；`types/api.d.ts:756-766`（`ApiDbRedisValue`，你要加 `ApiDbStreamWindow`）。
8. `.agents/rules/panel-proof-of-life.md` 全文——面板改动的"完成"定义。
9. `crates/swiss-it/src/seed.rs:398-470`（redis 的 `fresh` 与 `load_redis_seed`）、`tests/it/redis.rs`（L1 测试的
   命名与 `browser()` 辅助函数）、`tests/it/gateway.rs`（L2 怎么起真网关、`g.api()` 怎么打路由）、
   `tests/it/seed.rs:263-`（守卫测试与 `3016`）。
10. `.agents/skills/swiss-dependency-review/SKILL.md`——本任务**预期零新依赖**；真要加，按它写理由。
11. `docs/38-*`（i18n：新文案全部 `tr()` / `tk()`，守卫会拦裸文案）与 `docs/37` R5（`data-*` 属性派发）。

## 交付顺序

S0 → S1 → S2 → S3 → S4，每项一个 commit，不合并、不跳。

- **S0**：先在 `seed.rs` 里写生成器（1 万条 XADD 走分块 pipeline，与 `load_redis_seed` 同一条连接），再改守卫。
  三处 `3016` 收成常量后 gate 2 的 66 条必须原样绿——这一项不许改任何既有断言的数字以外的东西。
- **S1**：**纯函数先行**，每个先写单测再写实现；`read_stream` 的三条命令走一次 pipeline；`read_key` 对 stream
  的委托是最后一步。swiss-it 的 L1 9 条按 §2.5 的名字写；L2 那条经 `/api/db/{name}/stream` 三个方向各打一次。
  `follow_polling_costs_three_commands_per_tick` 用 `INFO commandstats` 的 `cmdstat_xrevrange` / `cmdstat_xlen` /
  `cmdstat_xinfo` 前后差值——在**邻居索引**的连接上读（`fresh_redis_with_neighbor`），别让计数连接自己污染数字。
- **S2**：`data-stream.ts` 新文件；`h()` / `fill()` / `emptyNode` / `iconNode` 的 house 写法，`$("sheet")` 那套
  不涉及。vitest 分两个文件：纯函数一个、DOM 一个。真浏览器走查用 seed 的 `stream:ticks`（用 swiss-it 起的
  容器端口配一条 19998 上的临时连接，走完删掉，**连接定义不进任何提交**）。
- **S3**：Follow 的定时器只在 `data-stream.ts` 一处创建、一处清除；`document.visibilitychange` 与 tab 切换都要停。
  内存走查：一个 PowerShell 循环经 19998 的 `/api/db/<conn>/command` 每秒 50 条 `XADD stream:ticks * …` 跑 60 s，
  CDP `Performance.getMetrics` 在 10 s 与 60 s 各采一次 `JSHeapUsedSize`，两个数与 DOM 行数进提交说明。
  中文第二遍（文/A → `document.documentElement.lang === "zh-CN"`）。
- **S4**：只改文档；README 那行、docs/22 W3.3、docs/44 §2.4、本文状态头。不写 ADR（spec §6 说了为什么）。

## 门禁（每次提交前）

```
cargo test --workspace
cargo test -p swiss-it --features it
cargo clippy --workspace --all-targets --features it -- -D warnings
cargo tree -d -e normal,build
cargo deny check
cd crates\swiss-panel\panel; npm run check        # S2 起；单独一条命令，不要链在 cd 后面
```

S2/S3 另加真浏览器走查（真点击、fresh load、`touch crates/swiss-panel/src/lib.rs` 后再编 19998、served bytes 验过）。
提交后 `cargo tree --locked --offline --workspace --depth 0 > $null` 自检一次锁文件——本任务不该改 `Cargo.toml`，
所以 `Cargo.lock` 不该有 diff；有就是错。

## 提交规则

- 一个提交一项；说明写清"改了哪些文件、§2.5 矩阵里哪些格由哪条测试覆盖、在哪台机器跑了哪条门、mutation
  记录（红的测试名）、没跑什么及原因"。S3 附内存两点采样。
- **没有测试的功能点不提交**：矩阵里"单测"与"集成"两列都要有对应测试名；review 时按格点名。
- 每个纯函数、`read_stream` / `stream_groups`、`data-stream.ts` 文件头：一段"为什么这么设计"的注释（英文），
  引用 spec 的节号。只写"是什么"的注释不算。
- 不许删测试来过关；不许 `#[ignore]`；不许把"Docker 不在"变成跳过。
- seed、测试名、断言文本里不许出现任何真实系统的名字（`git grep -i -E 'acme_|ops_dev' crates/swiss-it` 必须为零）；
  字段名用 `sym/px/qty/side`，符号用 `AAA…EEE`。
- 代码注释英文，UI 文案走 i18n；提交说明按仓库现有 log 的口吻；结尾不加任何 Co-Authored-By trailer（docs/40 D3）。
- 提交作者是仓库本地已配置的身份，不改。

## 不做什么

见 spec §5。特别是：不碰 `XREAD`/`XREADGROUP` 的禁用；不加服务端订阅 / SSE；不做 XACK / XCLAIM / XTRIM / XDEL
的 UI；不做虚拟滚动；不改 `redis_get` 工具的 stream 语义；不改 `tests/adminapi.rs`；不碰 19999；不合并 master——
合并是 S4 之后 owner 的一次决定。

## 完成之后

最后一条回复给：每项的 commit hash；gate 2 的用例数（基线 66 + 新增）；vitest 的用例数（基线 + 新增）；
§2.5 矩阵逐格的测试名（四列）；§4 八条验收逐条的证据（内存两点数字、commandstats 差值、走查截图路径——
截图只能是 seed 数据）；没做到的事和原因。然后停下——不部署、不合并、不 push。
