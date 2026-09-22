# 10 — Jobs 完全配置驱动

> 状态：**全部已实现**。基线：`2937034`。
> 执行的一半（`6910f75`）：Action / RunCoordinator / ProcessSupervisor 这套共享执行服务，
> Jobs 由执行者变成它的 producer（§2、§6 的执行部分、§7 的 `/api/actions`、`/api/runs`）。
> 配置的一半（`580b8dc`–`e40d3fa`）：v2 schema 与字段契约（§4）、配置作为定义的唯一事实来源
> （§5）、cron/misfire/DST 的持久化语义、`jobs.json` 迁移、schema 驱动的面板（§7）。
> 分阶段的实施记录与验收标准见 [11 — Jobs v2 实施规范](11-jobs-v2-implementation-spec.md)。
> 配套架构：[09 — 开发瑞士军刀插件架构](09-toolbox-plugin-architecture.md)。

## 1. “完全配置化”是什么意思

不是把任意业务写进配置语言，也不是让用户为每个任务编译插件。目标是：

- 一个已注册 Action 的任务，从创建、编辑、校验、调度、并发、超时、重试到输出保留，都由一份有版本的定义决定。
- UI 表单、JSON 编辑器、CLI 导入和 API 操作同一份定义，运行状态不混入定义。
- 页面和 CLI 手动执行、定时任务调用同一个 Action，不通过 shell 再启动 `swiss` 调自己。
- 增加一种新执行能力只注册 Action 及其输入 schema，不修改调度器。任意新业务实现仍然需要代码。
- 配置缺省即使用安全、可见的内置默认值；`disabled: true` 显式停用。默认启用不代表创建示例任务或自动执行导入的未知命令。导入先预览、校验，再显式应用。

## 2. 三个职责，不能再混在一起

| 组件 | 负责 | 不负责 |
| --- | --- | --- |
| JobDefinitionStore | 配置版本、schema 校验、持久化、导入导出 | lastRun、运行 PID、调度计时器 |
| Scheduler | 触发时间、错过触发策略、并发和队列、运行索引 | 如何连接 MCP、怎样启动进程、各插件业务 |
| ActionRegistry / Executor | 查找能力、校验输入、执行、取消、返回结果 | cron 和任务编辑页面 |

执行链：`trigger → occurrence → RunCoordinator → ActionRegistry → provider`。

RH 参考的是 `rh-jobs` 的运行登记、容量控制、取消和 first-wins 终态，而不是抄一个 cron 系统：所检查版本的 RH Jobs 是进程内后台任务注册表；独立的 `rh-schedule` 是会话提醒系统，既不消费 Jobs 注册表，也不是配置定义的 cron。RH 的容量控制是按 owner 的准入检查，超限立即拒绝，不维护运行队列。这里的持久化任务定义、cron 和有界排队层都属于本项目新增设计。

`process.exec` 是通用进程执行插件提供的 Action；MCP 插件提供 `mcp.call`。这些名字是本提议的稳定能力 ID，不是当前已存在的 API。

轻量的 `RunRegistry / RunCoordinator` 是共享运行服务，统一登记定时和手动运行；Jobs Scheduler 只是其中一个 producer。停用 Jobs 不会让其他插件失去手动 Action 执行能力，也不能取消其他 producer 的运行。每次运行登记 producer、provider 和可选 jobId，按所有权取消；不要求引入一套常驻消息队列。增补（2026-09-21，`d67db29`，docs/41 A1）：每次运行现在还登记 **actor**——谁发起了这次运行；Jobs 的运行记 `jobs`，`POST /api/runs` 接受调用方自报的可选 `actor`（CLI `cli:<user>@<host>`、面板 `panel`），缺省或乱写记 `api`，运行记录（`RunView`，即 `/api/runs` 响应的 `actor` 字段）与面板 Runs 页都显示它。通用进程监督器供 proc MCP 与 Jobs 共用，但两者的“常驻 MCP 子进程”和“一次性命令”生命周期仍不同。

## 3. 建议配置示例

下例是未来 `gateway.config.json` 解密后的逻辑结构片段，不改变 AES-GCM/HKDF 封装，不创建新的明文生产配置。`plugins` 每个 key 是插件实例 ID，省略 `kind` 时按同名内置插件解析。

```json
{
  "schemaVersion": 2,
  "plugins": {
    "mcp": {},
    "tunnels": {},
    "data": {},
    "process": {},
    "jobs": {
      "disabled": false,
      "config": {
        "maxConcurrentRuns": 2,
        "maxQueuedRuns": 32,
        "maxHistoryBytes": 67108864,
        "defaults": {
          "timeoutMs": 600000,
          "overlap": "skip",
          "misfire": "skip",
          "retry": { "maxAttempts": 1, "delayMs": 1000 },
          "output": { "capture": "tail", "maxBytes": 16384 },
          "retention": { "days": 180, "maxBytesPerJob": 2097152 }
        },
        "definitions": {
          "rust-check": {
            "title": "Rust checks",
            "disabled": false,
            "trigger": { "type": "interval", "everyMs": 3600000 },
            "action": {
              "type": "process.exec",
              "input": {
                "program": "cargo",
                "args": ["check"],
                "cwd": "${WORKSPACE_DIR}",
                "env": {}
              }
            },
            "policy": { "timeoutMs": 300000 }
          }
        }
      }
    }
  }
}
```

示例的 `WORKSPACE_DIR` 是用户显式设置的普通环境变量，不是宿主自动提供的工作区占位符；缺失时按必需变量解析失败，不推断为当前目录。

第二种触发器示例：`{"type":"cron","expression":"30 3 * * *","timezone":"local"}`。第一阶段支持 `local` 与 `UTC`，命名时区需明确评估数据库依赖后再加入；不认识的 timezone 必须拒绝，不能悄悄当 local。

复杂任务首先通过一个受监督的脚本完成；后续确有需求再由独立 `sequence` Action 提供有限串行步骤。第一阶段不实现 DAG、任意表达式解释器、动态 eval 或分布式工作流。

## 4. 字段契约和默认行为

| 类别 | 必须表达的内容 | 默认与约束 |
| --- | --- | --- |
| 身份 | 稳定 ID、title、labels、disabled | ID 与显示名称分开；缺省启用；改标题不改历史归属 |
| 触发 | manual / interval / cron | 三选一；所有已配置任务均支持显式 Run now；没有合法触发器时报错 |
| interval | everyMs、首次触发、运行锚点 | 正整数且有上限；首次等一个周期；使用持久化 occurrence 锚点，不用页面打开时间 |
| cron | expression、timezone | 5 字段；本地旧配置沿用 local；明确 DOM/DOW、DST 和重启语义 |
| Action | type、input、schemaVersion | provider 必须存在或报告缺失依赖；输入按对应 schema 校验 |
| 超时 | timeoutMs | 缺省 10 分钟；包含整个运行的有限期限，不能因为无限重试绕过 |
| 重叠 | skip / queue-one | 缺省 skip；同一任务已有运行或排队时不重复创建；queue-one 最多一个后继 |
| 总并发 | maxConcurrentRuns、maxQueuedRuns | 缺省 2、32；严格有界；队列也保存小定义引用而非大输入副本 |
| 队列满 | 定时触发与手动请求的结果 | 定时记录 skipped/capacity；手动返回可见的容量错误，不能假装已执行 |
| 漏跑 | skip / run-once | 缺省 skip；run-once 最多补一次，不补出停机数天的任务风暴 |
| 重试 | maxAttempts、delayMs、backoff、retryOn | 缺省 1 次总尝试，即不自动重复有副作用的操作；配置重试需确认幂等性 |
| 输出 | capture、maxBytes、可选受限落盘 | 缺省保留尾部 16 KiB；管道始终排空；字节上限在读取时执行 |
| 日志 | days、maxBytesPerJob、maxHistoryBytes | 缺省沿用 180 天与单任务 2 MiB；Jobs 历史总预算缺省 64 MiB，按最旧记录淘汰且保留运行摘要 |
| 退出 | drain / cancel、graceMs | 缺省先 drain 3 秒，之后取消并收回进程树；无法取消的 Action 显式报告 |
| 修改中任务 | configRevision、resolved policy | 已开始的运行持有旧快照；修改只作用于后续运行；删除默认移除后续计划 |

每个数值同时有合法上下界，UI 和 API 共用后端校验。`timeoutMs: "bad"`、负数、溢出间隔或未知字段不能退回默认值伪装成功。旧版兼容解析与 v2 严格解析分开。

### cron 的边界

- 保留既有 5 字段与 DOM/DOW 匹配兼容测试；额外补全通配步长等边界，再宣称兼容某一种 cron 方言。
- DST 春季不存在的当地时间默认跳过；秋季重复的当地分钟默认只运行一次。持久化 wall-clock occurrence key，不能只凭进程内计时器去重。
- 休眠恢复、系统时钟跳变、服务重启都通过 misfire 策略处理。下一次 due 提前算好，不在每次 UI 轮询里按分钟扫描一年。
- 绝不承诺外部副作用 exactly-once。崩溃前后可能无法确认外部执行结果；标记 interrupted/unknown，按显式恢复策略决定是否重试。

## 5. 配置和状态的单一事实来源

建议 v2 的任务定义统一属于 `gateway.config.json → plugins.jobs.config.definitions`；不同时在该位置和 `jobs.json` 维护可写任务表。

- 配置：用户意图、定义版本、Action 输入（仅凭据引用）。
- 运行状态：独立状态存储中的 runId、jobId、configRevision、occurrence、状态、时间、退出码；按固定上限保留内存索引。
- 历史：磁盘分段/有界日志，按游标分页读取，不每次把全文件解析成 DOM。
- 已配置任务停用后仍可查询历史。插件整体停用时，新执行不得隐式唤醒被显式关闭的插件。

UI 是配置编辑器，不是第二个数据库。表单与 JSON 高级编辑器必须往返无损，保留不属于当前表单的合法字段。原始含密钥引用的配置只通过受保护的配置接口读取；普通运行状态接口仍然脱敏。

配置写入顺序：`携带旧 revision → 解析/校验/依赖检查 → 持久化成功 → 发布新 desired revision → reconcile`。

- revision 冲突返回 409，不能后保存的人静默覆盖先保存的人。
- 持久化失败返回错误，不能只 warn 后向 UI 返回保存成功。
- 应用阶段的网络/进程失败显示 desired 与 actual 的差异，不谎称外部副作用能被事务回滚。
- 对文件导入进行整体校验与差异预览；禁用、改名、删除会影响哪些任务必须可见。

## 6. 执行和安全

- `process.exec` 使用 program + args 数组；对每个参数做变量替换，不把替换后的字符串重新拆词。包含空格的环境变量不能变成多个参数。
- shell 是显式 `process.shell` 能力或明确的 `cmd /c`、`sh -c` 调用，不能默认把参数交给 shell。配置文件也是代码执行权限边界，不从访问页面/导入预览中执行任务。
- `${ENV_VAR}` 在执行时解析；必需变量缺失时失败并指出变量名，不能输出值。迁移旧命令仍走旧 tokenizer 兼容路径，不擅自改变已有命令含义。
- 每个 Action 声明输入 schema、能力权限、是否可取消及幂等提示；外部 token 调用有副作用 Action 不能绕过授权。管理接口继续保留 loopback / Host / Origin 保护。
- 凭据引用不应被 resolved snapshot 或错误日志写回；stdout/stderr 也经过公共敏感值遮盖。不能保证识别任意程序自行编码的秘密，因此输出访问和文件权限仍重要。
- Windows 进程树 Job Object、Unix process group 的所有权放在共享 process 服务，而不是让 Jobs import `adapters::proc` 私有实现。
- 取消需覆盖 root、descendants、管道 reader、等待 task；超时 drop JoinHandle 不等于取消 reader。最终必须 join 或明确报告未收回资源。
- 普通 Action 错误不能击穿其他插件；进程内的 panic/abort/OOM 不具备跨插件故障隔离。不可信扩展应进隔离进程，而不是授予整个宿主权限。

## 7. API 与界面

新增通用能力接口（提议）：

- `GET /api/actions`：可用能力及 schema / 可取消性 / provider 状态。
- `POST /api/runs`：提交一次手动运行，立即返回 runId；不让浏览器 HTTP 请求承担后台任务的所有权。
- `GET /api/runs/{id}`、`POST /api/runs/{id}/cancel`：状态与取消。
- `GET /api/config/...`、带 revision 的更新接口：配置编辑，不混入运行结果。

现有 `/api/jobs`、`PUT /api/jobs/{name}`、`POST /api/jobs/{name}/run`、历史端点保持兼容适配；旧同步 run 接口可以等待同一个 runId 的结果，不创建第二条执行路径。旧列表形状保持不变，v2 扩展使用显式版本化接口。

页面包括：任务列表、定义编辑（表单/JSON）、校验与下一次触发预览、手动运行与取消、游标历史。新增 Action 的表单来自 schema，复杂能力可贡献自己的局部编辑器，不修改 Jobs 主页面的 switch。

## 8. 当前实现的差距（基线源码）

| 位置 | 已有 | 下一步 |
| --- | --- | --- |
| `crates/swiss-jobs/src/jobs/mod.rs` | JobDef、密封 JobStore、每秒一个 scheduler、每任务互斥 | 定义与 lastRun 分离；原子配置更新；追踪并回收所有运行任务；总并发和容量限制 |
| `crates/swiss-jobs/src/jobs/runner.rs` | 超时、命令解析、平台进程树、输出结果 | read_to_end 改为边读边限量；完整取消 reader；进程能力上移到共享服务 |
| `crates/swiss-jobs/src/jobs/schedule.rs` | interval、当地 cron | 编译后复用规则；维护 next due；持久化 occurrence / misfire / DST |
| `crates/swiss-jobs/src/jobs/runlog.rs` | 单任务磁盘大小和年龄限制 | 实例化状态；有界尾读和分页；模块总预算 |
| `crates/swiss-jobs/src/jobs/api.rs` | CRUD、同步 run、历史 | 共用 schema；严格验证；真实保存失败；适配统一 RunCoordinator |
| `crates/swiss-panel/src/admin_assets/js/jobs.js` | 手工字段表单和列表轮询 | Node 参考源先改；统一页面注册、schema 编辑和通用 run 视图后再同步 |

基线还存在测试隔离欠账：若干设置进程级 runlog 目录的测试没有统一持有 `RUNLOG_TEST_LOCK`。本次测试通过不能证明这些并发窗口不存在；资源实例化会消除这类全局测试状态，改造前应先补锁和回归测试。

## 9. 迁移和验收

1. 先修输出边读边限量、取消和持久化报错等资源/正确性问题，不先扩充任务语法。
2. 抽出 Action 与 ProcessSupervisor；旧 command 适配成 `process.legacy-command`，保留 tokenizer 行为。
3. 建 v2 schema、配置编辑接口、RunCoordinator；保留旧 API 适配。
4. 显式迁移旧密封 `jobs.json`：导出脱敏差异预览；成功写入新的密封配置后，迁移 lastRun 到运行状态并标记旧源只读。每一步幂等、可回退，不在禁用时迁移或改写状态。
5. 跨文件迁移需要带源版本/摘要的迁移日志与完成标记。启动发现未完成迁移时先恢复/回退，再启动 scheduler；不能因两份文件同时存在而执行两遍。备份仍保持密封格式和私有权限。
6. 不支持 v2 的旧二进制不得继续写入同一数据目录；回退使用迁移前的完整备份。仅保持密封格式相同不等于新旧 schema 可互写。
7. UI 切到 schema + runId 模型，最后加入其他 Action provider。

验收至少覆盖：

- 旧任务/加密文件/API 兼容；明文凭据不进入导出或运行索引。
- JSON 与表单双向保存、revision 冲突、磁盘失败、非法字段/超大数值。
- 重叠、总并发、队列满、重试幂等提醒、修改期间运行快照。
- chrono 虚拟时间边界、DST、休眠恢复与重启漏跑；纯调度测试不 sleep。
- 大量输出时保留缓冲大小恒定，stdout/stderr 不阻塞子进程。
- 取消、超时、插件关闭后任务和子进程树清空；关闭页面不会取消后台任务。
- 禁用/启用重复 100 次，任务计数、连接计数、缓存字节回到空闲边界，不线性增长。
