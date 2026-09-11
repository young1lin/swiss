# 11 — Jobs v2 实施规范

> 状态：**S1–S6 全部已实施**，本文从实施规范转为实施记录与验收依据。
> 写作基线 `5f18951`，完成基线 `18ad5f2`。
> S1 `6bcdf9e` 定义模型 · 收尾 `242d89f` 旧占位行宽容启动 · S2 `542339d` 状态与日志实例化 ·
> S3 `c579c85` 配置成为事实来源 · S4 `b14896f` 迁移 · S5 `e9cf6ca` 调度语义 · S6 `18ad5f2` 面板。
> 下文的字段契约、边界与拒绝规则**就是现在代码的行为**；改代码前先改这里，别让两边分叉。
> 前置阅读：`AGENTS.md`（它的规则高于本文的任何便利）、`docs/10-config-driven-jobs.md`（契约来源）。

## 0. 怎么用这份文档

- 分成 S1–S6 六个阶段，**一个阶段一个提交**，每个阶段独立可验收、独立可回退。
- 每个阶段的"完成"是机械可检查的：本文列出的测试全部存在且通过，加上四条门禁命令全绿。
- 行为变化先有测试：改动前失败、改动后通过（AGENTS.md「Making changes」）。
- **本仓库的 `crates/lmg-panel/src/admin_assets/` 一个字节都不能改。** 面板改动先落在 `../local-mcp-gateway/src/admin/`，再整目录复制回来，见 S6。
- 新写的代码注释一律英文。文档散文可以中文。
- 每个阶段提交前跑：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 1. 现状与目标的差距

下表写于基线 `5f18951`，记录的是**当时**的差距——每一行现在都已经填平，留着是为了让后来者看见每个
设计决定是冲着哪个具体问题去的。文件路径已按 workspace 拆分（`4147e8a`）更新，行号则是历史值，
不要拿它去定位今天的代码；当前模块位置见 `docs/02-architecture.md`。

| 位置 | 当时 | 目标（现已实现） | 阶段 |
| --- | --- | --- | --- |
| `crates/lmg-jobs/src/jobs/mod.rs:55` `JobDef` | v1：name + command + everySec/cron + enabled + timeoutMs + cwd，**lastRunAt / lastOk 混在同一个结构体里** | v2 `JobDefinition`：稳定 id、title、labels、disabled、trigger、action、policy；运行状态搬走 | S1, S2 |
| `crates/lmg-jobs/src/jobs/mod.rs:178` `JobStore` | 密封 `jobs.json` 是定义的唯一来源，也是 lastRun 的写入点 | 定义来自 `gateway.config.json → plugins.jobs.config.definitions`；`jobs.json` 迁移后只读、清空、留备份 | S3, S4 |
| `src/builtin.rs:362` `validate_jobs_config` | 只检查每项是不是 object，descriptor 里自己写着 "the scheduler reads jobs.json today" | 完整 v2 校验，是配置写入的服务端权威 | S1, S3 |
| `crates/lmg-jobs/src/jobs/schedule.rs` | 每次求值重新 parse cron；无 occurrence 持久化、无 misfire、无 DST 语义 | 编译一次复用；持久化 occurrence key；misfire 与 DST 明确 | S5 |
| `crates/lmg-jobs/src/jobs/mod.rs:264` `JobSystem.runtime` | 每任务一个 bool，overlap 只有 skip | overlap = skip / queue-one，经 `RunCoordinator` 判定；retry 属于一次 occurrence | S5 |
| `crates/lmg-host/src/services/runs.rs:45` 容量默认值 | 注释说"jobs 的 reconciler 会覆盖"，但 `set_capacity` 只有测试在调用 | `plugins.jobs.config.maxConcurrentRuns / maxQueuedRuns` 真正驱动 `set_capacity` | S3 |
| `crates/lmg-jobs/src/jobs/runlog.rs:34` | `DIR` / `STATES` 是进程级 `OnceLock`，测试靠 `RUNLOG_TEST_LOCK` 串行 | 实例化为 `RunLog`，由 `JobSystem` 持有；测试锁删除 | S2 |
| `crates/lmg-jobs/src/jobs/runlog.rs` 读取 | `read_page` 读整个文件 | 从文件尾部按块读的游标分页；模块总预算淘汰 | S2, S5 |
| `crates/lmg-panel/src/admin_assets/js/jobs.js` | 手写字段表单 + 6s 轮询 | schema 驱动的编辑器、runId 模型、游标历史 | S6 |

## 2. 不可破坏的约束

实现过程中以下每一条都不能为了省事而让步：

1. **密封格式冻结。** 一切落盘走 `secure::statefile::{read_secure_json, write_secure_json}`，不新增第二种存储格式（docs/05）。新增的状态文件也是密封 + 私有权限。
2. **凭据只以 `${ENV_VAR}` 引用存在。** 配置里存引用，运行时解析；解析后的值不回写配置、不进错误信息、不进运行索引；解析出的值注册进输出遮盖（`services::actions` 里 `resolve_with_secrets` 已经这么做）。
3. **`current_thread` runtime。** 不新增线程池，不为每个任务开一个常驻 task。调度仍是一个 tick task。
4. **配置、网络、磁盘路径上不允许 `.unwrap()`。** 一个坏任务不能影响其他任务，更不能影响别的插件。
5. **接受但不执行的字段等于撒谎。** 任何 v2 字段，如果当前阶段还没有真正实现它的语义，解析器必须**拒绝**非默认值，错误信息里写明"尚未实现"。宁可 400，不可静默忽略。
6. **面板是管理 API 的规格。** 任何 `/api/*` 响应形状的改动，先在 Node 仓库改面板，再复制。
7. **loopback 边界不变。** 新端点一律挂在现有 `/api` 守卫内部。

## 3. v2 数据模型

### 3.1 定义存放位置

任务定义属于插件配置行，不再有可写的 `jobs.json`：

```json
{
  "schemaVersion": 2,
  "plugins": {
    "jobs": {
      "kind": "jobs",
      "disabled": false,
      "config": {
        "schemaVersion": 2,
        "maxConcurrentRuns": 2,
        "maxQueuedRuns": 32,
        "retention": { "days": 180, "maxBytesPerJob": 2097152, "maxHistoryBytes": 67108864 },
        "definitions": {
          "nightly-vacuum": {
            "title": "Nightly vacuum",
            "labels": ["db", "maintenance"],
            "disabled": false,
            "trigger": { "kind": "cron", "expression": "30 3 * * *", "timezone": "local" },
            "action": {
              "type": "process.exec",
              "input": {
                "program": "psql",
                "args": ["-d", "app", "-c", "VACUUM ANALYZE"],
                "env": { "PGPASSWORD": "${APP_DB_PASSWORD}" }
              }
            },
            "timeoutMs": 600000,
            "overlap": "skip",
            "misfire": "skip",
            "retry": { "maxAttempts": 1, "delayMs": 0, "backoff": "fixed", "retryOn": ["failure"] },
            "output": { "capture": "tail", "maxBytes": 16384 }
          }
        }
      }
    }
  }
}
```

**任务的稳定身份是 map 的键**，不是 `title`。改 `title` 不改历史归属；改键等于删一个建一个（历史留在旧键的日志里，见 §7.3）。

键的合法性沿用 `runlog::valid_name`：1–64 个字符，只允许字母、数字、`.`、`_`、`-`。理由是它同时是 `logs/jobs/<id>.jsonl` 的文件名。

### 3.2 字段契约

`config` 层（插件级）：

| 字段 | 类型 | 默认 | 边界 | 说明 |
| --- | --- | --- | --- | --- |
| `schemaVersion` | u32 | 缺省视为 2 | 只接受 `2` | 出现别的值 → 拒绝整份配置，不降级解析 |
| `maxConcurrentRuns` | usize | 2 | 1..=64 | 应用到 `RunCoordinator::set_capacity` |
| `maxQueuedRuns` | usize | 32 | 0..=1024 | 同上 |
| `retention.days` | u32 | 180 | 1..=3650 | 单任务日志的年龄上限 |
| `retention.maxBytesPerJob` | u64 | 2 MiB | 64 KiB..=64 MiB | 单任务日志字节上限 |
| `retention.maxHistoryBytes` | u64 | 64 MiB | 1 MiB..=1 GiB | Jobs 历史总预算 |
| `definitions` | map | `{}` | 最多 512 项 | 键即任务 id |

`definitions.<id>` 层：

| 字段 | 类型 | 默认 | 边界 | 说明 |
| --- | --- | --- | --- | --- |
| `title` | string | 缺省用 id | ≤ 200 字符 | 仅显示 |
| `labels` | string[] | `[]` | ≤ 16 项，每项 ≤ 64 字符 | 仅分组/过滤 |
| `disabled` | bool | false | — | 缺省启用；停用后仍可查历史、仍可手动 Run now |
| `trigger` | object | **必填** | 见 §3.3 | |
| `action` | object | **必填** | 见 §3.4 | |
| `timeoutMs` | u64 | 600000 | 1000..=86400000 | **每次尝试**的期限 |
| `overlap` | enum | `"skip"` | `skip` \| `queue-one` | 见 §6.2 |
| `misfire` | enum | `"skip"` | `skip` \| `run-once` | 见 §6.3 |
| `retry` | object | 见下 | 见 §6.4 | |
| `output.capture` | enum | `"tail"` | `tail` \| `none` | `none` 表示不保留正文，只留大小证据 |
| `output.maxBytes` | usize | 16384 | 1024..=1048576 | 读取时执行，不是读完再截 |

`retry` 默认 `{ "maxAttempts": 1, "delayMs": 0, "backoff": "fixed", "retryOn": ["failure"] }`，即**默认不自动重复有副作用的操作**。边界：`maxAttempts` 1..=10，`delayMs` 0..=3600000，`backoff` ∈ {`fixed`, `exponential`}，`retryOn` ⊆ {`failure`, `timeout`}（空数组等于不重试）。

**总期限的硬校验**：`maxAttempts * (timeoutMs + delayMs) ≤ 86400000`（24 小时）。校验期就要算出来并拒绝，而不是等运行时才发现一个任务可以近乎无限地重试。

未知字段一律拒绝（`additionalProperties: false` 的语义），错误信息里写出字段名和该层的合法字段清单——照 `services::actions` 里 `parse_exec_input` 的写法。数字不接受字符串、不接受负数、不接受小数（`crates/lmg-jobs/src/jobs/api.rs` 的 `whole_number` 是现成参考，它容忍 `60.0` 这种 JS 写法）。

### 3.3 trigger

```json
{ "kind": "manual" }
{ "kind": "interval", "everyMs": 300000, "firstRun": "after-interval" }
{ "kind": "cron", "expression": "30 3 * * *", "timezone": "local" }
```

- `manual`：不自动触发，但**所有已配置任务都支持显式 Run now**（docs/10 §4）。
- `interval.everyMs`：1000..=31536000000（1 秒到 365 天）。`firstRun` ∈ {`after-interval`（默认）, `immediate`}。锚点见 §6.1。
- `cron.expression`：5 字段，沿用 `schedule::CronExpr`（vixie 的 DOM/DOW OR 规则已实现，不要改它的语义）。`timezone` 当前只接受 `"local"`；出现别的值按 §2 规则 5 拒绝，信息写明 only "local" is supported。

`kind` 三选一，多写或不写都是错误。v1 的"everySec 与 cron 恰好一个"这条规则在 v2 由 `kind` 天然表达。

### 3.4 action

```json
{ "type": "process.exec", "input": { "program": "...", "args": ["..."] }, "schemaVersion": 1 }
```

- `type`：能力 id 字符串。**校验时不要求它当前已注册**（provider 可能正被停用），但要在 §7.1 的列表里报告 `actionAvailable: false`。
- `input`：交给对应 Action 自己的 schema 校验。入口是 `ActionRegistry::get(type)` 拿到 impl 后调用它的输入解析。provider 未注册时配置**仍然可以保存**（否则停用 process 插件就再也改不了任务），但 `PUT` 的响应里带 `warnings: ["action process.exec is not registered; runs will be refused until its plugin is enabled"]`。
- `schemaVersion`：可选，默认 1，留给 action 输入形状的演进。

**不允许**在 action 层引入 shell 语义。shell 是显式的 `cmd /c` / `sh -c` 参数（docs/10 §6）。

### 3.5 Rust 类型

新文件 `crates/lmg-jobs/src/jobs/def.rs`。类型是纯数据 + 纯函数，不碰 IO，便于全量单测：

```rust
pub struct JobsConfig {
    pub max_concurrent_runs: usize,
    pub max_queued_runs: usize,
    pub retention: Retention,
    /// Definitions in configuration order; the key is the job id.
    pub definitions: Vec<JobDefinition>,
}

pub struct JobDefinition {
    pub id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub disabled: bool,
    pub trigger: Trigger,
    pub action: ActionRef,
    pub timeout_ms: u64,
    pub overlap: Overlap,
    pub misfire: Misfire,
    pub retry: RetryPolicy,
    pub output: OutputPolicy,
}

pub enum Trigger {
    Manual,
    Interval { every_ms: u64, first_run: FirstRun },
    /// The expression is compiled ONCE, at parse time - never per tick.
    Cron { expression: String, compiled: CronExpr },
}
```

入口是 `JobsConfig::parse(&Value) -> Result<JobsConfig, ConfigError>`，其中

```rust
pub struct ConfigError {
    /// Dotted path of the offending field, e.g. "definitions.nightly.retry.maxAttempts".
    pub path: String,
    pub message: String,
}
```

错误必须带路径：面板要把错误指到具体字段上，这是 §7.2 表单可用的前提。

## 4. 运行状态：第二个文件

定义是用户意图，运行状态是网关维护的事实，两者不能共用一个可写文件（docs/10 §5）。

新文件 `~/.mcp-gateway/jobs-state.json`（密封，私有权限），由 `crates/lmg-jobs/src/jobs/state.rs` 拥有：

```json
{
  "version": 1,
  "jobs": {
    "nightly-vacuum": {
      "lastOccurrenceKey": "cron:2026-09-09T03:30",
      "lastRunAt": "2026-09-09T03:30:00.412Z",
      "lastOk": true,
      "lastRunId": 41,
      "consecutiveFailures": 0
    }
  }
}
```

规则：

- 只存**小而有界**的东西。输出、命令、凭据引用一律不进这个文件。
- 每个任务一行；定义被删除后，其状态行在下一次写入时清掉（历史留在日志里）。
- 写入时机：occurrence 认领时（`lastOccurrenceKey`、`lastRunAt`）与终态落定时（`lastOk`、`lastRunId`、`consecutiveFailures`）。
- 写失败是 `warn`，不是 `Err`：运行已经发生了，为了一次状态写入而拒绝运行是更糟的交易。这一条与"配置写失败必须报错"的差别要写进代码注释——现有 `JobSystem::claim` 已经有一段这样的注释，照它的口径写。
- 文件损坏或缺失 = 空状态，不是启动失败。

## 5. 迁移：v1 `jobs.json` → 配置 + 状态

这是唯一一次跨文件迁移，必须幂等、可回退、可诊断（docs/10 §9 步骤 4–6）。

### 5.1 时机与顺序

在 **Jobs 插件 start 时**执行，早于调度器启动。`JobsMigration::run()` 的步骤如下；任何一步失败都停在原地并把插件标记为 `failed`（`lastError` 说明卡在哪一步），**绝不带着半迁移状态启动 scheduler**：

1. 读 `jobs.json`。不存在，或其中已有 `migratedAt` → 直接返回 `Done`（幂等）。
2. 把原文件按密封格式复制成 `jobs.json.v1.bak`（私有权限）。已存在则不覆盖。
3. 把每一行 v1 转成 v2 定义（§5.2），与配置里已有的 `definitions` **合并**：**同 id 时配置一方胜出**，并在迁移日志里记一条 `conflict`。
4. 用当前 revision 做 CAS 写入配置（`ConfigStore::update_plugin("jobs", rev, merged)`）。冲突 → 重读一次并重试一次；再冲突 → 失败退出（有人正在同时改配置，不该硬抢）。
   **注意 `update_plugin` 替换的是整个 `config` 对象**，所以 `merged` 必须是读-改-写之后的完整配置（`maxConcurrentRuns`、`retention` 等既有键一并带上），只传 `definitions` 会把其他键抹掉。
5. 把 v1 的 `lastRunAt` / `lastOk` 写进 `jobs-state.json`。
6. 把 `jobs.json` 改写为 `{"jobs": [], "migratedAt": "<iso>", "migratedTo": "config@<revision>", "backup": "jobs.json.v1.bak", "count": <n>}`。**`jobs` 数组必须清空**：这是防止旧二进制回到同一数据目录后按老表重复执行任务的唯一有效手段（docs/10 §9 步骤 6）。
7. 在网关日志里留一条 `info`：迁移了几条、冲突几条、备份在哪。

崩溃恢复：第 4 步成功、第 6 步没写成 → 下次启动看不到 marker，会重跑 3–6；第 3 步的合并规则保证配置里已有的定义不被覆盖，所以重跑是安全的。这个性质要有测试（§9 S4）。

### 5.2 v1 → v2 映射

| v1 | v2 |
| --- | --- |
| `name` | 定义的键；`title` 同值 |
| `command` + `cwd` | `action = { type: "process.legacy-command", input: { command, cwd? } }` |
| `everySec: N` | `trigger = { kind: "interval", everyMs: N*1000, firstRun: "after-interval" }` |
| `cron: "..."` | `trigger = { kind: "cron", expression: "...", timezone: "local" }` |
| `enabled: false` | `disabled: true` |
| `timeoutMs` | `timeoutMs`（同值） |
| `lastRunAt` / `lastOk` | 写入 `jobs-state.json`，**不进配置** |

**必须用 `process.legacy-command` 而不是 `process.exec`**：老的 tokenizer 和宽松的 `${VAR}` 展开是既有命令的语义，改成严格解析会悄悄改变用户已有任务的含义（`crates/lmg-host/src/services/actions.rs` 里 `LegacyCommandAction` 的注释说明了这一点）。

### 5.3 回退

回退 = 用旧二进制 + 把 `jobs.json.v1.bak` 改回 `jobs.json`。文档要写明：配置里迁移进去的 `definitions` 不会被自动清理，回退后新旧两处可能都有定义——这正是第 6 步清空 `jobs` 数组要防的重复执行，回退时需要人工确认。**不要声称迁移是事务性的**：跨两个文件，它不是。

## 6. 调度语义

### 6.1 occurrence 与锚点

引入 occurrence key（字符串），是"这一次该跑的那一次"的稳定身份：

- cron：`cron:<本地墙钟分钟>`，如 `cron:2026-09-09T03:30`。**不带时区偏移**——这正是 DST 秋季重复的那一分钟只跑一次的实现方式（第二次的 key 与第一次相同，被 `lastOccurrenceKey` 去重）。
- interval：`interval:<该次应触发的 unix 毫秒>`。
- 手动：不写 occurrence key（手动运行不移动调度锚点）。

`lastOccurrenceKey` 落盘（§4），所以重启、休眠恢复、系统时钟跳变都靠它判断"这一次是不是已经跑过"，而不是靠进程内计时器。

interval 锚点：`lastRunAt` 存在就用它，否则用网关启动时间（现有 `JobSystem.anchor_ms` 的语义，保留）。`firstRun: "immediate"` 时首次锚点取 0，即启动后第一 tick 就到期。

### 6.2 overlap

判定权交给 `RunCoordinator`，删掉 `JobSystem.runtime` 那张 bool 表：

- 提交时 label 用 **`job:<id>`**（不是裸 id）。理由：`RunCoordinator::count_for_label` 不区分 owner，裸 id 会让一个手动 `/api/runs` 提交的同名 label 意外挡住调度任务。
- `skip`：`count_for_label("job:<id>") > 0` → 记一条 `skipped` occurrence（`reason: "overlap"`），不提交。
- `queue-one`：`queue_if_busy = true` 提交；队列满时 `SubmitError::Capacity` → 记 `skipped`（`reason: "capacity"`）并按 §7.1 报告。

无论跳过还是拒绝，**occurrence 锚点照样前移**——否则池子满的时候任务会每秒重试一次（现有 `JobSystem::claim` 的注释已经指出这一点，保留这个不变量并为它留测试）。

### 6.3 misfire

启动时以及每次 tick 时，用 `lastOccurrenceKey` 算出"从上次认领到现在错过了几次"：

- `skip`（默认）：把锚点直接推到**最近一次**应触发时刻，只记一条 `missed` 汇总记录（`missedCount: N`），不补跑。
- `run-once`：最多补跑一次（用最近一次错过的 occurrence key），其余同上汇总。

停机几天再开机，绝不允许产生 N 次补跑（docs/10 §4）。

### 6.4 retry

重试属于**一次 occurrence 内部**，由 jobs 这个 producer 实现，不进 `RunCoordinator`：

- 每次尝试是一次独立的 `submit`（因此照样受全局并发限额约束），run 记录里带 `occurrenceKey`、`attempt`（从 1 起）、`attempts`（总数）。
- 只有 `retryOn` 里列出的终态才重试：`failure` = 退出码非 0 或 spawn 失败；`timeout` = `timedOut`。**`canceled` 永不重试**（有人明确要求停止）；容量不足与能力缺失这类 REFUSAL 也不重试（重试只会继续撞同一堵墙）。
- `delayMs` 的等待必须可取消：等待期间插件停用要立刻中止整个 occurrence。做法是把 `tokio::time::sleep` 与 `CancelHandle::cancelled()` 一起 `select!`，并把 occurrence 任务注册到 `PluginScope`。
- `backoff: exponential` = `delayMs * 2^(attempt-1)`，仍受 §3.2 的总期限校验约束。

### 6.5 next-due 预算

现在每 tick 对每个任务重新求值（cron 还会重新 parse）。改为：

- 定义加载/变更时编译一次 `CronExpr`（§3.5 已把 `compiled` 放进 `Trigger::Cron`）。
- 维护 `HashMap<String, i64>` 的 next-due 表，occurrence 认领后只重算该任务的下一次。
- tick 仍是每秒一次，但只做一次 map 扫描比较，不做日历推算。`CronExpr::next_after` 只在重算某个任务时调用。

### 6.6 时钟抽象

DST 与休眠测试不能依赖跑测试那台机器的时区。引入：

```rust
/// The clock the scheduler reads. The real one is chrono::Local; tests inject a fake with a
/// synthetic DST rule so the same assertions run in any machine timezone.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    fn local_from_ms(&self, ms: i64) -> chrono::NaiveDateTime;
    /// None when the wall-clock time does not exist (spring-forward gap).
    fn ms_from_local(&self, t: &chrono::NaiveDateTime) -> Option<i64>;
}
```

不新增依赖（**不要**引入 `chrono-tz`）。`JobSystem` 持有 `Arc<dyn Clock>`，默认实现走 `chrono::Local`。

## 7. API

### 7.1 v1 `/api/jobs`：保持形状，扩展字段

现有面板依赖这些字段，**一个都不能少、不能改类型**：`name`、`command`、`everySec`|`cron`、`enabled`、`timeoutMs`、`cwd`、`lastRunAt`、`lastOk`、`running`、`nextDueAt`。

v2 追加（新增字段是安全的，面板会忽略不认识的键）：`id`、`title`、`labels`、`trigger`、`action`、`overlap`、`misfire`、`retry`、`output`、`source`（`"config"`）、`actionAvailable`（bool）、`editableInV1`（bool）、`configRevision`。

投影规则：

- `name` = id；`enabled` = `!disabled`。
- `command`：`process.legacy-command` 的任务直接取 `input.command`；`process.exec` 的任务合成一个**只读展示串**（program 加引号包裹的 args），并置 `editableInV1: false`。
- `everySec`：仅当 `trigger.kind == "interval"` 且 `everyMs % 1000 == 0` 时出现；否则不出现（absent-not-null）。
- `PUT /api/jobs/{name}` 在 `editableInV1: false` 的任务上返回 **409**，信息指向 `PUT /api/plugins/jobs/config`。这是诚实：v1 形状表达不了那个定义，覆盖就会丢字段。

### 7.2 v2 编辑：复用现成的配置接口

**不新增 CRUD 端点。** v2 的定义编辑就是插件配置写入，`PUT /api/plugins/jobs/config`（`crates/lmg-host/src/host/api.rs`）已经具备 docs/10 §5 要求的全部性质：带 revision 的 CAS、服务端校验、持久化失败报 500、成功后 reconcile。

需要补的只有两件：

1. `validate_jobs_config`（`src/builtin.rs`）换成 `JobsConfig::parse` 的完整校验，错误信息带字段路径。
2. 让 Jobs 支持**不重启地应用配置**——见 §8。

一个客户端必须知道的性质：这个 PUT **整体替换** `config` 对象，所以编辑一个任务的正确做法是
`GET /api/plugins/jobs/config` → 改其中一项 → 带着刚读到的 revision PUT 回完整对象。这也是
S6 要求"表单与 JSON 编辑器往返无损、保留表单不认识的合法字段"的原因：丢字段在这里等于删配置。

### 7.3 运行与历史

- `POST /api/jobs/{id}/run`：默认保持同步等待（现有行为；AI agent 和人都要那条记录）。body 里 `{"async": true}` → **202** + `{"runId": N}`。两条路径提交的是同一个 coordinator run，不允许出现第二条执行路径。
- `GET /api/jobs/{id}/runs?cursor=&limit=`：`cursor` 为新参数，`before` 保留为别名。读取必须**从文件尾部按块读**，不再把整个文件解析成 DOM。
- 任务被删除或停用后，历史照样可读（日志文件不随定义消失）。
- run 记录新增字段：`runId`、`occurrenceKey`（手动运行没有）、`attempt`、`attempts`、`outcome`（`"ran"` | `"skipped"` | `"missed"` | `"refused"`）。`outcome != "ran"` 的记录没有 `exitCode` / `output`。现有字段（`at`、`trigger`、`ok`、`ms`、`pid`、`exitCode`、`timedOut`、`canceled`、`error`、`preview`、`output`、`chars`、`seq`）保持不变。

## 8. 插件契约的一处扩展：apply 而不是 restart

Jobs 的 `restart_on_config_change` 现在是 `true`（`src/builtin.rs:394`）。今天这是无害的：配置行还不是任务表，重启只是把一个空壳插件停了再起。**S3 之后它就不无害了**——配置里就是任务表，每改一个任务都会把插件停掉，而 `JobsInstance::stop` 会取消并等待 Jobs 名下所有在途运行。编辑一个任务的定义顺手杀掉另一个正在跑的任务，是不可接受的。

留 `true` 不行，改回 `false` 更不行（那样编辑就完全不生效）。需要的是第三种语义。

因此给 `PluginInstance` 加第三种语义（`crates/lmg-host/src/host/factory.rs` + `crates/lmg-host/src/host/engine.rs`）：

```rust
/// Apply a new config row IN PLACE, without a restart. Default: NotApplicable, so a plugin
/// that says nothing keeps today's restart-or-note behaviour exactly.
async fn apply_config(&self, _config: &Value) -> ApplyOutcome { ApplyOutcome::NotApplicable }

pub enum ApplyOutcome { Applied, NotApplicable, Failed(String) }
```

`reconcile_locked` 的 `(true, true)` 分支改为先调 `apply_config`：

- `Applied` → 记录新的 `config_revision`，不动实例。
- `NotApplicable` → 走现有逻辑（`restart_on_config_change` 决定重启还是只记 revision）。
- `Failed(err)` → 实例保持 Active，`lastError` 置为该错误，`config_revision` **不前移**——这就是 docs/10 §5 要的 desired 与 actual 差异可见。`PUT` 仍返回 200（配置确实存下了），响应体加 `"applied": false, "error": "..."`。

Jobs 的 `apply_config` 语义：

- 新增/修改/删除的定义对**后续 occurrence** 生效；已经开始的运行持有旧快照直到结束（docs/10 §4「修改中任务」）。
- 删除一个定义 = 取消它后续的计划，不取消它正在跑的那一次，也不删历史。
- `maxConcurrentRuns` / `maxQueuedRuns` 变化 → `RunCoordinator::set_capacity`（降低不杀在途运行，现有实现已经如此）。
- `retention` 变化 → 更新 `RunLog` 的上限，下一次写入时生效。

## 9. 阶段划分与验收

### S1 — v2 定义模型（纯逻辑，无行为变化）

新增 `crates/lmg-jobs/src/jobs/def.rs`：§3.5 的类型、`JobsConfig::parse`、v1↔v2 投影函数。**不接线**，`JobSystem` 还不使用它。

`validate_jobs_config` 改为调用 `JobsConfig::parse`，于是 `PUT /api/plugins/jobs/config` 立刻获得真校验。此时 `definitions` 仍然不驱动任何调度——descriptor 的 `config_schema` 描述必须继续如实说明这一点。

按 §2 规则 5：`retry`、`misfire`、`overlap: "queue-one"`、`output.capture: "none"` 在本阶段一律拒绝非默认值，错误信息写 not implemented until S5。

测试（`crates/lmg-jobs/src/jobs/def.rs` 内联）：
- 每个字段的下界、上界、类型错误各一条，断言错误 `path` 正确。
- 未知字段被拒绝，且信息里列出该层的合法字段。
- 三种 trigger 的解析与拒绝（`kind` 缺失、两个 kind、cron 字段数不对、timezone 非 local）。
- 总期限校验：`maxAttempts * (timeoutMs + delayMs) > 24h` 被拒。
- v1→v2 与 v2→v1 投影往返：v1 可表达的定义往返无损；`process.exec` 定义投影出 `editableInV1: false`。
- 512 项上限、id 命名规则。

### S2 — 运行状态与日志实例化

新增 `crates/lmg-jobs/src/jobs/state.rs`（§4）。`crates/lmg-jobs/src/jobs/runlog.rs` 的 `DIR` / `STATES` / `RUNLOG_TEST_LOCK` 全部删除，改成 `RunLog { dir, states: Mutex<HashMap<..>>, limits }`，由 `JobSystem` 持有；测试各自建自己的临时目录。

`JobDef` 的 `last_run_ms` / `last_ok` 字段移除，改从 `JobsState` 读；`jobs.json` 写入时不再写这两个键（读取时仍接受，供 S4 迁移用）。

游标分页：`RunLog::read_page(cursor, limit)` 从文件尾部按块读，只解析需要的那些行。

测试：
- 状态文件往返；损坏文件 = 空状态且不 panic；写失败只 warn。
- 两个 `RunLog` 实例指向不同目录互不干扰（证明进程级状态确实没了）。
- 游标分页：翻到底给出 absent 的 `nextBefore`；用一个远超 limit 行数的文件断言读取字节数有界。
- 现有 runlog 测试全部保留并去掉锁。

### S3 — 配置成为定义的事实来源

`JobSystem` 改为从 `ConfigStore` 读定义（`plugin_config("jobs")` → `JobsConfig::parse`），`JobStore` 降级为只读的迁移输入。§8 的 `apply_config` 在本阶段落地，容量与 retention 开始被配置驱动。§7.1 的投影与 409 规则落地。

同时把 Jobs descriptor 的 `restart_on_config_change` 改成 `false`：`apply_config` 接管之后它不该再有机会生效，留着 `true` 等于给"编辑一个任务顺手杀掉另一个在跑的任务"留一条后门。

测试：
- 配置里的定义驱动调度（用注入时钟断言到期集合）。
- `PUT /api/plugins/jobs/config` 改一个定义 → 无重启生效；正在跑的运行不被取消（关键回归）。
- 删除定义 → 后续不再触发，历史仍可读，在途运行不被杀。
- 容量变更真正到达 `RunCoordinator`（读 `capacity()` 断言）。
- revision 冲突 409；磁盘写失败 500；`editableInV1: false` 的任务经 v1 PUT 得到 409。
- `/api/jobs` 列表形状：v1 字段逐个断言（防止面板被悄悄改坏）。
- `action.input` 经 `ActionRegistry::get(type)` 拿到的 impl 按其自身 schema 校验：不合法的 input 是 400、错误带字段路径（S1 在 `parse_action` 留下的 TODO(S3)，在此落地）。
- provider 未注册时配置仍可保存，但 `PUT` 响应带 `warnings`、`/api/jobs` 列表对该任务报 `actionAvailable: false`（§3.4、§7.1）。

### S4 — 迁移

新增 `crates/lmg-jobs/src/jobs/migrate.rs`，实现 §5。

测试：
- 全新数据目录（无 `jobs.json`）：no-op。
- 有 v1 任务：迁移后配置里有等价定义、状态文件有 lastRun、`jobs.json` 的 `jobs` 为空且带 marker、备份存在且可解密。
- 幂等：连跑三次结果相同，不产生重复定义。
- 崩溃恢复：在第 4 步之后、第 6 步之前中断，再次启动能收敛且不重复执行。
- 冲突：配置里已有同 id 定义 → 配置胜出，日志里有 conflict 记录。
- 迁移失败 → 插件 `failed`，scheduler 没有启动（断言没有任何 occurrence 被认领）。

### S5 — 调度语义补全

§6 全部：occurrence 持久化、misfire、DST、overlap（含 queue-one）、retry、next-due 预算、`Clock` 抽象。S1 里那些"暂不支持"的拒绝分支同步放开。

测试（全部用注入时钟，**不允许 sleep 真实时间**）：
- interval 锚点：从未跑过的任务等一个周期；`firstRun: "immediate"` 立刻到期；长时间运行结束的瞬间不会立刻再触发。
- cron：同一分钟内重启不重复触发（现有规则的回归）；DST 春季不存在的本地时间被跳过；秋季重复的本地分钟只跑一次。
- misfire：停机 3 天后启动，`skip` 只记一条汇总；`run-once` 恰好补一次。
- overlap：`skip` 时第二次被记为 skipped 且锚点前移；`queue-one` 时排一个后继，队列满时是可见的拒绝。
- retry：失败重试到 `maxAttempts`；`canceled` 不重试；REFUSAL 不重试；`exponential` 的间隔序列正确；插件停用时正在 `delayMs` 等待的 occurrence 立刻中止。
- next-due：一个 1000 个任务的表，一次 tick 调用 `CronExpr::next_after` 的次数不超过必要值（用计数器断言）。

### S6 — 面板

**先在 `../local-mcp-gateway/src/admin/` 改**，Node 端 `node --check` 通过，再整目录复制回本仓库，并附 SHA256 一致性核对（现有做法）。

改动范围：
- Jobs 列表读 v2 字段（title / labels / trigger 摘要 / 下次触发 / 上次结果）。
- 编辑器：v1 可表达的任务保留现有表单；其余走 schema 驱动的表单 + JSON 高级编辑器，**两者往返无损**，保留表单不认识的合法字段（docs/10 §5）。
- action 输入表单由 `GET /api/actions` 的 schema 生成，复用现有 `run.js` 的 schema→表单逻辑。
- Run now 走 `{"async": true}` + `/api/runs/{id}` 轮询；关闭页面不影响后台任务。
- 历史用游标分页。

测试：Node 仓库的 vitest 覆盖（docs/08 的规矩：模块的测试文件一起移植）；本仓库这边断言嵌入资源与 Node 源逐字节一致。

## 10. 明确不做的事

- 不引入新的 cron 方言（秒级字段、`@daily`、`L`/`W`/`#`）。
- 不引入 `chrono-tz` 或任何新依赖；`timezone` 只支持 `local`。
- 不做任务之间的依赖/DAG、不做分布式调度、不承诺外部副作用 exactly-once。
- 不把 `jobs.json` 变成双写来源；迁移之后它只是一个带 marker 的空壳加一份备份。
- 不为 v2 新增 CRUD 端点（§7.2）。
- 不动面板以外的 Node 仓库代码。

## 11. 验收清单（我 review 时逐条对）

1. 四条门禁命令的输出，贴在 PR 描述里。
2. `git diff --stat` 里 `crates/lmg-panel/src/admin_assets/` 是否为 0（S6 除外，且 S6 必须附 SHA256 核对）。
3. 每个 v2 字段：解析测试、边界测试、错误路径断言各存在。
4. 任何"接受但未实现"的字段是否按 §2 规则 5 被拒绝。
5. 迁移的幂等与崩溃恢复测试，是否真的中断在两步之间，而不是把完整流程调用两次。
6. 调度测试是否用注入时钟；`grep -rn "sleep" crates/lmg-jobs/src/jobs/` 不应出现真实时间等待（`delayMs` 的实现除外，且它必须与取消 `select!`）。
7. `jobs.json` 迁移后 `jobs` 数组是否真的清空（这是防重复执行的关键）。
8. `RunCoordinator` 的容量是否被配置真正驱动（`set_capacity` 有调用方）。
9. 定义编辑是否会取消在途运行（不应该）。
10. 新注释是否全英文；是否有新的 `.unwrap()` 落在配置/磁盘/网络路径上。
