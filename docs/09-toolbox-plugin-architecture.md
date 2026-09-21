# 09 — 开发瑞士军刀插件架构

> 状态：**P1–P6 全部已实施**。代码基线：`2937034`。
> P1–P3（`6910f75`）：PluginHost 与插件生命周期、ActionRegistry / RunCoordinator /
> ProcessSupervisor、面板 PageRegistry 与 `/api/plugins` 清单。
> P4（`2026831`）：连接目录 `crates/swiss-host/src/services/catalog.rs`，Data 不再依赖 MCP。
> P6（`3d81d30`，后随用户决定移除）：试金石插件——加它没有改动 `crates/swiss-host/src/host/` 一行；
> 契约随后由终端插件（docs/14）在同一路径上再次验证。
> P5（`2937034`）：workspace 拆分成八个 crate，仍是一个 `swiss.exe`（体积 +1.1%）。
> 实施过程见 [12](12-remaining-work-spec.md)。
> 产品方向：一个低内存、单进程的开发工具宿主；MCP 是重要插件，但不再是其他功能必须依附的核心。
> 本文不改变现有线上配置、接口或前端源文件归属。Jobs 详细契约见 [10](10-config-driven-jobs.md)。

## 1. 结论：学 RH 的组合方式，不复制它的运行成本

推荐“小宿主 + 进程内插件 + 能力注册 + 页面贡献 + 配置驱动”。

- 一个 Rust 可执行程序，一个 Tokio `current_thread` runtime，一个 HTTP listener。
- 插件默认启用；耗内存的连接、缓存、子进程按需创建。启用、运行、页面可见是三个不同状态。
- 插件可拥有零个、一个或多个页面，页面不是后端模块划分的依据。
- 通用 Action 可以被页面、CLI、Jobs 调用；不为每种入口复制业务。
- 首阶段采用静态链接的内置插件，支持运行状态启停；新增 Rust 插件实现仍需重新编译。
- Cargo workspace 用来建立可检查的代码依赖边界，不用来宣称节省运行内存。

不推荐首阶段上 DLL、WASM、Node 插件宿主、微服务、一个插件一个进程、完整事件溯源平台或任意脚本配置求值。进程隔离只在执行外部命令/既有 proc MCP、或未来确实需要隔离不可信扩展时使用。

## 2. RH 实际参考了什么

检查对象是本机安装的 `@example/rh` **0.1.1-rc.2** 发布包，不是假定存在的 monorepo apps 源码。以下路径相对于 `RH_ROOT/node_modules/@example/`。

| 已核实的 RH 源码 | 实际机制 | 本项目取舍 |
| --- | --- | --- |
| `cordis-plugin-loader/src/config/entry.ts`（EntryOptions、Entry.update） | 配置 row 包含 id/name/config/disabled/inject，另有 group 嵌套；配置修改与更换实现分开处理（本设计暂不采用 group） | 用稳定实例 ID、编译内置 factory、typed config 和明确重启策略 |
| `cordis/src/fiber.ts`（resolveConfig、FiberState、_unload） | 插件 schema 校验；依赖影响状态；资源 effect 统一清理、等待异步 disposer | PluginScope 收回任务/服务/订阅；Rust 明确规定停止阶段和依赖顺序 |
| `cordis/src/reflect.ts`（provide） | 服务注册有所有者；撤销影响消费者；重复提供冲突可见 | typed capability + generation + dependency graph，拒绝重复注册 |
| `rh-client-modules/lib/index.js` | 从同一插件树发现客户端贡献，生成带版本的 bundle 入口 | 从同一 inventory 生成页面列表和资源版本，不再靠逐个请求 API 猜插件是否存在 |
| `rh-client-ui-settings-plugin-inventory/lib/client.js:285–292` | 用 slots 注册 settings tab，带稳定 id/order/label | 导航、设置页、工具栏均为可贡献插槽，不在 main.js 堆 switch |
| `rh-jobs/README.md`、`rh-jobs-local/lib/index.js` | 进程内运行登记、容量控制、取消、终态 first-wins；执行由 producer 提供 | 共享 RunRegistry，Jobs Scheduler 只是一个 producer |
| `rh-schedule/README.md` | 独立的会话持久化提醒；不是 Jobs cron 配置系统 | 只借鉴确定的持久化确认和恢复语义，不移植 Agent/Session 依赖 |

几个容易误解的区别：

1. 所检查 RH loader 使用顶层 YAML **row 数组**，不是本文的 JSON `plugins` 对象。本文有意沿用 Rust 项目的 JSON/密封存储，借鉴语义，不声称格式兼容。
2. RH `_unload()` 用 `Promise.all` 等待 disposer；不能把它描述成“每个异步 disposer 严格顺序完成”。本项目应显式先停入口/消费者，再释放 provider。
3. RH 插件 inventory 页面是只读投影，不等于已经提供了本项目需要的运行时启停控制台。
4. RH 的 `!!js`、动态 VM 插件、npm 安装、代理 Context、多 Agent scope、HMR 都不是这里的必需品。
5. RH Jobs 没有现成的配置定义任务层。本项目的 cron、任务定义、重试和历史迁移需要自己设计。

## 3. 模块边界

```text
Host
  HTTP / Loopback / Auth / Config / PluginHost / Inventory
  ActionRegistry / RunRegistry / ScopedResources / PageRegistry

Plugins
  MCP       Registry, protocol, tools, resources, Traffic, call history
  Tunnels   SSH connections, forwards, reconnection, tunnel pages
  Data      database browsing, queries, transactional edits
  Jobs      configured definitions, triggers, scheduler, job pages
  Process   command actions over the shared process supervisor
  Future    HTTP tools, Git tools, formatters, port tools, ...

Lazy shared capabilities
  database connections / HTTP transport / process supervisor / secure storage
```

MCP 与 Traffic 属于同一个插件；可以是两个子页。Tunnels、Data、Jobs 独立启停。Process 是可无独立页面的能力插件，不要求导航出现一个新标签。

### Host 里只留下横切机制

允许保留：安全边界、配置和密钥接口、能力与页面注册、插件状态机、运行登记、资源计量。

不允许留下：MCP 工具选择逻辑、SQL 表浏览、SSH 规则业务、cron 解析、某插件的缓存或前端 state。不能把原先的大 AppContext 换名成巨大的 Services 对象。

### Data 与 MCP：共享资源，但不互相依附

当前 Data 通过 `AppContext.browser_resolver()` 查询 MCP Registry，数据库 browser 和 MCP tools 已共享底层连接。保留共享、移除强依赖：

- 提取 ConnectionCatalog / ConnectionProvider 契约；驱动仍在可选实现里，不进入 plugin-api。
- MCP tools 与 Data browser 获取同一连接定义的 lease；连接 key 必须包含配置身份、凭据上下文和只读语义，不能仅凭 host/port 合并。
- Data 关闭后释放它自己的 lease；MCP 仍在用的连接不能关。MCP 关闭而 Data 仍在用时反之亦然。
- 原始连接定义保留稳定 ID；现有 MCP def 先由适配层映射，不在第一步改动 managed.json。
- 接口解耦完成前，Data 应诚实声明当前依赖，不能仅移动文件就宣称可以独立运行。

### Tunnels：可选关联，不形成依赖环

Tunnels 的 MCP 健康展示、rename/delete guard 通过可选只读能力实现。实际网络依赖通过资源 lease/依赖关系登记；停止仍被使用的隧道返回使用者列表，由用户决定是否级联。

依赖 SSH 的连接仍需要“隧道可用后再连接”的恢复机制，不能简单把启动顺序改成随机并行。单独使用的转发规则可保持常开，不能只因 MCP 没流量就回收。

### Jobs：只拥有调度，不垄断后台执行

Jobs Scheduler 产生日程运行；页面/CLI 也可以产生命令运行。共享 RunRegistry 记录 producer/provider/jobId 与取消控制器。停用 Jobs 只停其计划及其拥有的运行，不停止其他页面启动的独立工作。

## 4. 插件契约：六项贡献 + 一份资源所有权

每个插件提供：

1. **Descriptor**：kind、version、configSchemaVersion、默认启用、必要/可选能力。
2. **Config**：typed schema、默认值、校验和迁移；UI 可读的 schema 数据。
3. **Services / Actions**：可注入服务及可调用业务能力；稳定 ID，按插件作用域注册。
4. **Routes**：自己的 API 路由；统一经过 host 的 loopback/auth/body-limit 边界。
5. **Pages / Slots**：页面入口、导航位置、排序、设置页、可选操作贡献。
6. **Lifecycle**：prepare/start/apply/stop 与健康、错误、资源计数。

`PluginScope` 负责资源：受监督 task handles、取消信号、注册 token、连接 lease、子进程 owner、缓存。允许每插件多个 Tokio task，但不能丢失所有者；task 不是线程，不应为了“少 task”牺牲取消的正确性。

声明和实例分开：Descriptor 可常驻，但停用的插件不能保留昂贵实例。公共 API 使用 typed trait/DTO；只在配置/管理边界使用 JSON Value，转发路径继续 RawValue/bytes。

### 状态和更新

```text
Disabled
EnabledIdle -> WaitingDependency -> Starting -> Active
                                      |           |
                                    Failed <- Stopping -> EnabledIdle / Disabled
```

实现时使用清晰枚举，Disabled、依赖缺失、配置错误、启动失败、未编译必须可区分。默认启用的无配置插件可以 idle；Jobs 有启用计划、Tunnels 有常开规则时在后台启动，不能等打开页面。

- 首次启动 single-flight；并发请求共享一个启动结果。
- 配置更新先验证，持久化 desired revision 后 reconcile；运行中实例维护 actual revision。
- 支持安全在线修改的字段走 apply；其余明确标为 plugin restart 或 host restart。不能把所有配置修改都当热更新。
- 缺依赖由注册/撤销事件触发状态变化，不为每个待启动插件开轮询任务。
- Host 自身安全/配置不可用才导致整体启动失败；单个 MCP/可选插件失败必须隔离、记录并可诊断，不机械照搬 RH 全树启动断言。

### 停用必须真的释放

停止顺序：关闭新请求入口 → 阻止新触发 → drain/cancel 在途工作 → 收回 reader/子进程 → flush 有界日志 → 注销贡献与释放 lease → drop 实例。消费者先于 provider 停止；中途失败有超时和错误状态。

Axum 路由不能永久捕获昂贵 `Arc<PluginInstance>` 后只修改一个 enabled bool。可让轻量路由代理请求时获取实例 lease，或整体替换可路由快照；停止后卸下旧实例并等待在途引用归还。所有版本的路由始终置于统一安全边界内。

当前 `AppContext → Registry → evictor callback → AppContext` 强引用环（`src/app.rs:93`）需要改成 Weak/可注销回调。Traffic/calls/runlog 的全局 OnceLock 状态也需要逐步实例化，不能把它们遗留为进程常驻状态。

## 5. 配置体系：一个事实来源，不是多个互相覆盖的开关

建议新的逻辑 schema 用 `schemaVersion: 2` 与 `plugins.<id> = {kind?, disabled?, config?}`。这是目标格式，不是本次已修改的生产配置。

- 同名内置 kind 可以省略；自定义实例 ID 必须提供 kind。重复实例仅在插件声明支持时允许。
- 编译进发行版的内置插件缺省开启；缺配置行不关闭。未编译的插件显示 not-built，不能假装可以启用。
- 第一阶段只有内置默认值 + 一份用户配置，不引入层层 profile/overlay。
- 将来确需配置片段时按稳定 ID 替换整行，来源可追踪；不采用难以预测的任意深度 merge。boot、validate、dump-config 共用一套解析和默认值逻辑。
- `disabled` 是插件行元数据，不写进运行历史。已知 schema 内未知字段拒绝并给出路径，不能把拼写错误当成功。
- 现有 root-level jobs/tunnels 开关、servers 和 managed.json 先经 v1 adapter 转成内存规范；不同时维护两套可写来源。
- v2 迁移后配置所有权应统一到 ConfigStore，即使物理存储分文件，UI/API/CLI 仍走同一事务与 revision。具体文件迁移需要显式确认和备份，不能随第一次启用静默完成。
- 稳定 schema 输出表单元数据；先评估轻量 schema 方案，不能为自动表单引入另一套 runtime。安全默认值来自后端，前端不是权威校验器。

凭据 `${ENV_VAR}`、密封格式、文件权限、Loopback/Host/Origin 和旧 MCP 客户端路径继续是硬边界。

## 6. 页面扩展：Shell 不认识具体插件

页面注册示例（提议）：

```json
{
  "id": "http.requests",
  "pluginId": "http",
  "slot": "nav.tools",
  "label": "HTTP",
  "order": 50,
  "path": "#/http",
  "entry": "/admin/plugins/http/index.js"
}
```

Shell 只做 inventory、路由选择、布局、通用通知、主题、共享组件和页面挂载。插件自己提供 mount/update/unmount；导航、settings、toolbar 使用有限且有文档的 slot contract。

- 一插件可贡献多个页面或不贡献页面，纯前端 formatter 不要求启动后端实例。
- The panel's level-one navigation groups pages by `pluginId`, takes each group's order from the smallest page `order` in it, and adds no host fields to do it; two-level rendering and the fallback group labels are specified in `docs/13-panel-navigation-spec.md`.
- 点击页面才 dynamic import 入口；切走注销订阅/DOM/轮询/大查询结果。ESM 模块缓存未必卸载，不承诺 JS 代码立即从浏览器内存消失。
- 页面通过注入的 api/actions/run/config 客户端访问能力，不读取其他插件的全局 state。
- 后端统一 inventory 投影 compiled/desired/actual/lastError/pages/actions，前端不能以探测 `/api/jobs` 的成功失败来决定完整能力树。
- 停用插件时页面展示不可用原因，配置和历史仍可由通用管理界面查看；不要静默删掉错误提示。
- 路由、Action、页面 ID 冲突在注册时拒绝。页面资源路径必须受限，禁止路径穿越与任意远端脚本 URL。
- 注册表中可有受限通用表格/表单页面；复杂 Data 等页面保留手写实现，不强行用 JSON 描述整个 UI。

当前前端仍以 Node 项目的 `src/admin` 为唯一源，`crates/swiss-panel/src/admin_assets` 是逐字节副本。页面注册化先在 Node 源完成再同步，不在 Rust 侧 fork。若以后把前端归属迁到独立 web 工作区，需单独更新 ADR/AGENTS 并统一两边消费同一制品；不能借“项目规整”偷偷改规则。

## 7. 项目规整：先逻辑分层，再用 workspace 固化边界

第一步在单 crate 内抽出 `host`、`plugin_api`、`services`、`plugins`，保持现有 pub mod 兼容转发，让测试不因搬文件大面积重写。禁止“整理目录”和行为变化混在一个巨型 commit。

当接口可用、至少两个插件通过契约复用资源后，再提取少量 workspace crates。目标示意：

```text
apps/swiss/                   CLI and composition root
crates/plugin-api/          descriptors, lifecycle and capability contracts
crates/host/                config, HTTP shell, inventory, scoped resources
crates/platform/            OS, process and secure-file primitives
crates/connections/         lazy driver providers, feature-gated implementations
plugins/mcp/                MCP and Traffic
plugins/tunnels/            SSH and forwards
plugins/data/               database UI/API
plugins/jobs/               configuration scheduler
plugins/process/            process Action provider
web/                       shell/contracts and generated distribution assets
tests/                     composition, compatibility, lifecycle, memory
```

这是目标布局，不是当前目录承诺；web 源归属受上一节约束。

依赖方向：`apps → host + plugins`；`plugins → plugin-api + 必需的共享实现`；`host → plugin-api`。host 不 import 业务插件；插件不能引用其他插件的 private module；通过 capability contract 调用。驱动实现不进入 plugin-api；一条 composition 表负责链接内置 factories。

Cargo features 表达编译能力，不取代运行时开关。发行版保留默认开启的业务功能，精简构建显式裁剪；依赖统一 workspace 版本与 default-features=false，避免额外 TLS/runtime。最终产物仍是一个 exe。

## 8. 内存：度量资源，不用拆目录制造幻觉

本次只读运行实例采样：gateway 23.1 MiB、MCP children 76.6 MiB，3 个进程。这是一次即时采样，不是新架构验收成绩，也不等于浏览器内存。当前统计根来自 Registry，独立 Jobs 子进程尚未纳入该集合。

优先修的具体点：

1. `crates/swiss-jobs/src/jobs/runner.rs` 的 read_to_end → 有界尾缓冲；取消必须收回两个管道 reader。
2. `src/app.rs` 的请求 Value DOM、整份响应缓存 → envelope + 原始字节、受限流式旁路预览，保持脱敏与背压。
3. `crates/swiss-mcp/src/traffic.rs` 按条数的 ring + 每条 spawned 写入 → 总字节预算、有界 writer 队列、可见的饱和策略。
4. 全局缓存、路由引用环和 detached task → 插件实例所有权与可验证释放。
5. 保留 proc 默认 lazy/idle-reap、不探测计费 HTTP/rest、现有共享池；调整池大小前先测实际并发与健康检查。

目标 inventory 指标：每插件 task 数、活动连接/lease、缓存/队列字节、在途运行数、子进程树内存；全局 working set/private bytes 分开显示。普通 allocator 无法精确按插件分摊整个 Rust 堆，不编造“每插件 RSS”。

验收测试包含：仅宿主、默认功能全开但无任务、典型 MCP 工作负载、Jobs 大输出、并发转发、100 轮启停。比较 release 可执行程序、相同工作负载、相同预热条件；RSS 未立刻下降不等于泄漏，但持续线性增长必须失败。

## 9. 新功能怎么加：以 HTTP 工具为验收样例

1. 注册 `http` descriptor/config schema。
2. 提供 `http.request` Action，复用共享 HTTP transport，保持明确的目标/授权策略。
3. 贡献 `http.requests` 页面与可选工具栏按钮。
4. 页面直接调用 Action；Jobs 通过配置引用同一 Action；CLI 用同一入口。
5. 在 composition 表链接一次，写 schema/lifecycle/route/权限/内存测试。

验收标准：除 composition 注册和构建清单外，不修改 host 主业务逻辑、Jobs 调度 switch 或 Shell 具体页面 switch。禁用 HTTP 插件后，引用该 Action 的任务显示 dependency-unavailable，而其他任务与 MCP 继续工作。

“放入一个第三方包即可安装”属于后续扩展层：预定义 Action/通用页面的纯数据扩展最轻；自定义前端代码是受信任代码；新后端实现仍需构建，或采用额外设计的隔离 IPC 插件。不把静态内置插件宣传成 DLL 热加载。

## 10. 可执行改造顺序与退出条件

| 阶段 | 主要提交 | 退出条件 |
| --- | --- | --- |
| P0 基线与测量 | `b1fdc32`；记录历史兼容与已知资源风险 | 默认测试、Clippy 通过；不修改运行配置 |
| P1 最小宿主契约 | PluginDescriptor、PluginScope、inventory、显式组合表；先包装 Jobs/Tunnels | 安全路由不漏挂；双插件可独立启停；失败隔离；资源清理可测试 |
| P2 运行服务与 Jobs | 有界输出、ProcessSupervisor、Action/RunRegistry、v2 Jobs schema | Jobs 配置不混 lastRun；保存失败可见；取消/并发/队列/漏跑测试通过 |
| P3 页面贡献 | Node 前端先实现 PageRegistry/slots + schema 编辑，复制面板 | 新页只注册贡献；切页清理；后台任务不依附页面；旧链接兼容 |
| P4 MCP/Data 解耦 | 抽连接能力、MCP+Traffic 实例化、处理循环引用 | MCP/Data 独立启停不误关共享资源；全部旧 MCP/API/密封测试继续通过 |
| P5 workspace 与裁剪 | 机械搬迁成受依赖检查的 crates，完善 feature matrix | 所有目标平台构建/测试；release 内存无回归；产物仍单 exe |
| P6 扩展试金石 | 一个 HTTP/formatter 插件，而非一次实现所有工具 | 不修改核心 switch 即可贡献页面/Action/配置；生命周期测试可复用 |

P1/P2 中已知的输出上限、取消、保存失败等安全/正确性修复先落地，不等到全部模块迁移。每个阶段独立可回退，行为变化先有回归测试。目录搬迁与协议变化分开提交。

## 11. 实施状态

> 本节是分阶段的施工记录，按落地顺序读。下面 P1–P3 的部分写于 `6910f75`，后面三条是它之后的事；全部六项都已完成，顶部的状态行是权威。

**P1–P3（`6910f75`）。**

- P1 最小宿主契约：`crates/swiss-host/src/host/` 的 PluginDescriptor / PluginScope / PluginHost，显式组合表在
  `src/builtin.rs`；六态生命周期、并发 start 单飞、路由边界的结构化 503、失败隔离（一个插件
  起不来是一行状态，不是启动失败）。启停经 ConfigStore 写入，CAS 冲突返回 409。
- P2 运行服务：ActionRegistry（能力按名字解析，不再对子系统种类做 match）、边读边限量的
  ProcessSupervisor（Windows Job Object / POSIX 进程组，子进程树随之消亡）、RunCoordinator
  （有界、按 owner 作用域、终态 first-wins、期限走与手动停止相同的取消路径）。Jobs 由执行者
  变成这个池的 producer，`/api/actions` 与 `/api/runs` 是宿主自有路由。
- P3 页面贡献：面板的 `page-core.js` / `page-registry.js` / `views/plugins.js` 从 `/api/plugins`
  清单渲染标签页，Node 参考源先改、再逐字节复制。

**P4（`2026831`）连接目录。** `crates/swiss-host/src/services/catalog.rs` 成为 Data 取连接的唯一
入口，provider 由 MCP 侧在 start 时注册、stop 时撤销。原来的问题——Data 不拥有资源，停用 MCP
会连带打瘫它——因此消失：Data 现在看到的是一份可能为空的目录，而不是一个不见了的模块。

**P6（`3d81d30`）试金石插件。** 一个一次性的 HTTP 请求工具插件。它存在的意义不是这个功能，而是
证明“加一个插件不用动宿主”：加它没有改 `crates/swiss-host/src/host/` 一行。任务完成后，插件本体
已按用户决定删除（2026-09-11，连同面板视图与专属测试）；契约的活性证明由终端插件承担——它走的
正是同一条 descriptor/action/page 路径。

**P5（`2937034`）workspace 拆分。** 八个 crate，仍是一个 `swiss.exe`（体积 +1.1%）。放在最后是
对的：先让契约在单 crate 里跑通，再用 manifest 把已经成立的边界固化——`swiss-data` 的
Cargo.toml 里没有 `swiss-mcp`，所以那条边再也回不来。见 ADR-010。

docs/10 的配置驱动那一半（v2 schema、配置作为定义的唯一来源、`jobs.json` 迁移、schema 驱动的
面板）也已在 `580b8dc`–`e40d3fa` 落地，分阶段记录见 [11](11-jobs-v2-implementation-spec.md)。
剩下的唯一未完成项是 [06](06-roadmap.md) 的 Phase 6 切换周（日历活，不是代码）。
