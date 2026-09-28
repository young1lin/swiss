# 19 — 密钥库（Secret Vault）：保存一次，处处引用，设备绑定

> 需求原文（用户，2026-09-12）："保存密钥，后续其他服务只需要引用密钥即可，运行时会自动替换成
> 保存的密钥，然后密钥只和设备绑定也就是那个 DPAPI。" 追加确认（同日）：值只进不出（忘了只能
> 重存）；引用缺失硬失败；Node 侧同款实现留在 scope；**任何地方**使用新语法的字符串都会在运行
> 时被替换成真值——这是基础层，其他插件都依赖它，每个使用面都要有单元与集成测试，不能等跑
> 起来才知道坏没坏。
> 本 spec 把它落成一件小而完整的东西：一个名字空间隔离的密钥库 `secrets.json`——同封印格式、
> 只经 `secret://name` 引用展开输出、面板可管理、永不进子进程环境。
>
> 增补（2026-09-27，替换值）：用户反馈"密钥设置了后就固定了，不能覆盖"。`PUT /api/secrets/{name}`
> 一直是覆盖语义，看起来改不了有两个原因：① `make_adapter` 构建时把引用解析进 adapter，已加载的
> MCP 一直拿着旧值，直到网关重启；② 面板没有覆盖入口——行的 ⋯ 只有删除，在表单里敲同名还会把
> 它挪进下拉框当前的分组。现在 PUT 成功后重建每个引用该名字的 MCP（`${secret://name}` 出现在
> 任一字符串里，或旧的整值 `secret://name`），走编辑路由同一条 `update_def`：在跑的重启到新值，
> 懒启动的回到 idle 由下次请求唤醒，启动失败的重试（旧值多半就是原因），停止的保持停止；应答
> 多出 `refreshed` / `failed` 两个名单。面板行 ⋯ 加"替换值…"（表单填入名字与它所在分组、光标
> 进值框），名字已存在时主按钮显示"替换"；替换先确认、不改分组，toast 列出重新加载的 MCP，
> 重建失败的以错误 toast 点名。SSH 隧道在每次连接、任务在每次运行时解析引用，不受此缓存影响。
> 已知未做：删除密钥同样不会让已加载的 MCP 立刻失去旧值，要到下次重建才失败。测试：
> `tests/http_adapter.rs` 的 `replacing_a_secret_rebuilds_the_mcps_that_reference_it`（远端只认
> 真 token，重试启动成功即证明重建后的 header 带的是新值），`admin-secrets.test.ts` 五例。
>
> 增补（2026-09-28，默认值）：用户原话"执行命令可以使用 `${secret://sss:test}`，`:` 后面表示默认值，sss
> 表示密钥的 Key……页面上展示的，还是只是这个密钥的 key"。语法 `${secret://name:default}`：第一个 `:` 之后、
> 右花括号之前的文本（所以不能含 `}`）在库里**没有**这个名字时顶替；库里有就用库里的值。名字本身不允许 `:`
> （`[a-z][a-z0-9-]{0,63}`），所以这个形式以前是非法引用，不会改变任何已有字符串的含义。默认值是 operator
> 自己写下的字面量，不收集进输出掩码；不带默认值的引用缺失时照旧硬失败；名字非法时有默认值也不救。改在唯一的
> 解析器 `swiss_core::secure::refs::resolve_families`，因此**所有**引用面（MCP 配置、header、SSH、任务、
> 远程执行）同时获得；配置校验 `is_env_ref` 与"替换值后重建哪些 MCP"的判定 `refs::names_secret` 认同一个
> 形式。远程执行（docs/34 R10）第一次把引用带进 argv / env / cwd。测试：`refs.rs` 四例
> （`a_default_stands_in_only_when_the_secret_is_missing` 等）、`config.rs` 的 `is_env_ref` 用例。

## 0. 现状与缺口（为什么是它、为什么是现在）

这套代码库的凭证模型已经走了两步，第三步一直没人铺：

1. **`${ENV_VAR}` 引用模型**（AGENTS 载重规则）：gateway.config.json / managed.json /
   tunnels.json / jobs.json 里的凭证在盘上永远是引用，展开只发生在使用边界——适配器构建时
   （`crates/swiss-host/src/config.rs` 的 `resolve_env_refs`）、SSH 连接时
   (`crates/swiss-tunnels/src/tunnel/ssh.rs`，从 swiss-host 导入同一函数)、job 运行时
   (`crates/swiss-jobs/src/jobs/runner.rs` 的宽松副本)、匹配前（`mcpmatch.rs`）。
2. **env.json 封印存储**（`crates/swiss-core/src/secure/envstore.rs`，Node 侧
   `secure/envstore.ts` 的移植）：明文 `.env` 在首次读取时被吞并、封进 AES-256-GCM 信封，
   之后 `env_lookup` 走"overlay 优先、进程环境兜底"的 dotenv 语义。

缺口有两个，一个是 UX 的，一个是安全的：

- **UX 缺口**：保存一个密钥至今仍是"手改 .env → 重启"。面板没有入口，只在添加表单的 hint 里
  劝你（add-sheet 两处："Prefer a `${ENV_VAR}` reference: the key then lives in .env…"）。
- **安全缺口（本质）**：env store 同时扮演两个角色——它既是凭证的配置间接层，又是"每个子进程
  都能读的环境"（envstore.rs 模块注释末句：子进程在 spawn 时显式合并 overlay）。为一个 http MCP
  存的第三方 API key，会被每个 proc MCP、每个 job 脚本、每个本地 shell 原样读到。密钥引用想要
  的语义不是这个：**只有引用它的那一个使用点拿得到值**。

所以本 spec 不做"给 env store 加 UI"，而是加一个名字空间隔离的密钥库：同封印格式、独立文件、
独立查找路径、只经引用展开输出。env store 保持原样（给子进程用的环境变量是它的正当职责）。

## 1. 目标与非目标

**目标**

- 面板可保存 / 覆盖 / 删除 / 列出密钥（列出=只列名字与更新时间，值永不回传浏览器）。
- 通用替换：任何使用面（MCP http/rest/proc、隧道、job、Test 端点、匹配）里的
  `secret://name` 字符串在运行时自动替换成真值——引用者无需任何新配置面。
- 设备绑定：存储沿用 master.key 的 DPAPI（Windows，per-user）保护链，文件复制离机即废。
- 子进程隔离：密钥库的值不进 env overlay、不进任何子进程环境、不进日志与 API 响应。
- `swiss export`/`import` 打通（换机是封印绑定的唯一官方逃生口，密钥不能缺席）。

**非目标**

- 不做轮换 / 过期提醒 / 按引用的 ACL / 密钥生成器 / 多机同步 / TOTP。
- 不做跨平台钥匙串抽象：绑定随 master.key 的平台实现走（Windows=DPAPI；docs/05 的论证不重复）。
- 不动 `${ENV_VAR}` 语义（缺失→空串的宽松行为保留：环境变量是机器现状，缺席是常态）。
- 不迁移 `SWISS_TOKEN`：token 认证读的是 `env_lookup`（bootstrap 播种进 env store），搬它
  会牵动 auth、`swiss creds`、token 配对逻辑，收益为零。
- 不给面板做"查看已存密钥"（reveal）：值只进不出，忘了就重存。这是刻意的单向门。

## 2. 设计

**定位先说清：基础层，不是可停用的插件。** 密钥库落在 swiss-core / swiss-host——所有插件
crate 本来就依赖的那一层，"其他插件都依赖这个 core"在 crate 图上早已成立。它不能注册成一个
普通插件：docs/09 的插件三态（enabled/running/visible）允许停用，而被全家依赖的东西没有"停用"
态——同一个理由决定了 Plugins 页必须是宿主资产（"被禁用的插件无法服务把它切回来的页面"）。
面板呈现走宿主常驻面（Gateway 页），API 走宿主路由；对 docs/09 的合同修订只有一条：
**插件的配置字符串若含凭证，到达使用点前必须过共享解析器**（见 D4）。
> 修订（2026-09-15，docs/26）：Secrets 页行序可拖（组内 + 跨组一次手势），vault 增加第三张表 `order`。

### D1 引用语法：`secret://name`（URI scheme 型）

> **修订（2026-09-15，docs/25 / ADR-019）**：引用语法改为 `${secret://name}`（`${...}` 信封 + 内部 scheme）。
> 裸 scheme 嵌在任意字符串里没有 token 边界——URL 里的 `secret://aaa` 与引用不可区分，这是语法层不可判定的
> 缺陷，不是实现 bug。名字文法（`[a-z][a-z0-9-]{0,63}`）与「使用点展开、缺失硬失败」不变；加载时整值自动迁移
> （docs/25 E2）。下文的选型 survey 保留为历史记录——它 survey 的对象是「整字符串引用 + 专用客户端」，
> 未覆盖「token 嵌在任意字符串里」的场景。

语法选型survey过业界现状：1Password `op://vault/item/field`、Doppler `doppler://…`、
Infisical `infisical://…`——新一代密钥平台清一色 **URI scheme 型**；AWS 用
`{{resolve:secretsmanager:…}}`（动词开头、冗长）；GitHub Actions 用 `${{ secrets.X }}`
（那是模板表达式，与 Helm/Go 模板同形易混）；Docker Compose 的 `${VAR}` 是纯 env 插值，
根本没有密钥概念。scheme 型胜出的理由在这里全部成立：

- **自描述**：字符串自己说"值在哪"，日志与错误读起来是自然语言
  （`references secret://stripe-key which is not in the vault`）；
- **全局可 grep**：`secret://` 是唯一的 9 字符 token，CI 与审计能一次扫出所有引用位；
- **可扩展不换语法**：将来要字段/版本，`secret://name#field`、`?version=2` 都是合法 URI
  延伸，今天不用预留；
- **与 `${ENV}` 不同字符类**：env 插值是花括号族，密钥引用是 URI 族，肉眼与文法双重不可混。

- `name` 文法：`[a-z][a-z0-9-]{0,63}`（小写 kebab）。大写名字直接拒绝，错误说明"这是
  密钥引用，名字用小写 kebab"。
- 前进兼容是免费的：现行扫描器只认 `${[A-Z0-9_]+}`（config.rs，ADR-007 手写），
  `secret://x` 不含 `${`，旧构建把它当字面文本留在原处——可见的坏（header 里出现明文
  引用、API 401），而不是静默泄密。
- **扫描器收敛到 swiss-core**：新模块 `crates/swiss-core/src/secure/refs.rs`，一个函数
  同时认两种引用（env 查 overlay+进程环境，secret 查密钥库）。jobs runner 的宽松副本换掉，
  tunnels 不再从 swiss-host 导入——按 AGENTS 的 crate 图原则，跨子系统共享的东西本来就该
  放 core/host 合同里，这次补上（记 ADR-014）。

### D2 存储：secrets.json，与 env store 同形

- 扁平 `name -> value`（字符串→字符串），与 envstore 的形状一致，零新机制。
- 走 `write_secure_json` / `read_secure_json`（statefile.rs）：AES-256-GCM、HKDF 每文件钥、
  master.key DPAPI per-user。**封印格式冻结**（docs/05 载重规则）——这里只是又一份状态文件，
  不碰信封本身。
- 设备绑定 = master.key 的绑定：复制 secrets.json 离机（或换 Windows 用户）打不开，BY DESIGN。

### D3 加载与内存

- boot 时一次读入 `RwLock<HashMap<String,String>>`（同 overlay 模式，envstore.rs 的
  `OnceLock<RwLock<…>>`），不进 `env_lookup` 的查找路径。
- 诚实的内存注记：不引入 zeroize（ADR-007 的依赖纪律），String 移动后擦除不可保证。接受，
  理由：值本就必须驻留以供连接/运行时展开；攻击面在文件与子进程侧，不在 Rust 堆。

### D4 展开规则：通用替换合同 + 失败语义（env 宽松、secret 硬失败）

**通用合同**：任何来自状态文件的字符串，在到达实际使用点（发起请求 / 建连接 / 起进程 /
跑脚本）之前，必须经过 `swiss_core::secure::refs::resolve`——含 `secret://` 就替换成真值，
含 `${ENV}` 照旧展开。不是"四个白名单边界"，是一条合同；现有使用面全部遵守，未来的插件
用同一个函数就自动继承（docs/09 合同修订，见 §2 定位）。当前使用面清单（测试按此逐面覆盖）：

1. MCP http/rest：header、url、body 模板（config.rs `resolve_def_checked`，整树递归）；
2. MCP proc：args、env（同一 `resolve_def_checked` 路径）；
3. 隧道：connection 的 password / keyPassphrase、rule 的字段（ssh.rs 连接时）；
4. job：command、env 值（runner.rs 运行时）；
5. 面板的 MCP Test 端点（/api/mcpdefs/test，服务端展开后真连一次）；
6. mcpmatch：匹配前按值比较（resolve 失败按不匹配处理）。

**失败语义**：

- `${ENV}` 缺失 → 空串（不变；环境是机器现状，缺席是常态）。
- `secret://name` 缺失 → **硬失败**，错误点名使用面与名字，值永不进错误文本：
  - MCP 适配器构建：启动被拒，reason 形如 `mysql: headers.Authorization references
    secret://stripe-key which is not in the vault`（与"端口缺失不得进监听器"同一哲学：
    宁可不启，不静默空串）；
  - 隧道：连接失败 reason 同款措辞（进既有 FailureKind 文案）；
  - job：该次 run 记录失败，原因同款（不弹进程）；
  - mcpmatch：解析失败的候选按不匹配处理，warn 一次。
- 为什么不对称：env 是机器现状；vault 引用是操作者的显式声明，缺席是配置错误，面板上一眼可修。

### D5 API（挂 src/app.rs，与既有 /api 同款 loopback 边界）

- `GET /api/secrets` → `{"secrets":[{"name":"…","at":<unix-ms>}],"rev":N}`——名字与 rev，
  **值绝不出现**。
- `PUT /api/secrets/:name` body `{"value":"…","rev":N}` → 新建或覆盖。
- `DELETE /api/secrets/:name` body `{"rev":N}`。
- rev 不匹配 → 409（与插件 enable/disable 同款纪律：两个面板开着也不静默覆盖）。
- 名字文法在 PUT 校验；`at`（更新时间）由服务端写。

### D6 面板：Gateway 页加 Secrets 区

- 位置：宿主页（plugins 视图，`pluginId: "host"`）列表下方——密钥是宿主机制，不注册新页面，
  不动 docs/13 的导航合同。
- 形态：名字列表 + 新增/覆盖表单（write-only 输入，不回显）+ 删除（确认文案风格同 job 删除）。
- 编辑表单里已是 `secret://name` 的字段照旧显示引用串：`is_env_ref`（config.rs）的掩码
  判定扩展到 secret 引用，面板侧无需新逻辑。
- add-sheet 两处 hint 文案改为指向密钥库（"`secret://context7` —— 在 Gateway 页的 Secrets
  里保存一次"）。

### D7 export / import

- `export_state()`（src/daemon.rs）的 bundle 增加 `"secrets"` 段；版本号保持 1（加法不破格），
  `import_state` 视缺段为空。export 本就是唯一明文离机通道（daemon.rs 模块注释），密钥缺席
  会让"换机"半身不遂。

### D8 子进程隔离（本 spec 的核心安全断言）

- spawn 时合并进子进程环境的是 **env overlay**；密钥库不在其中，也没有第二条合并路径。
- 这条断言由测试锁死（见 §3 Phase 2），不靠 review 记忆。

### D9 共存与快照

- env.json 原样保留，不搬迁、不同步；两处面板文案讲清分工：env=给子进程的环境变量，
  secrets=只给引用处用的密钥。
- `scripts/test-instance.ps1` 的 `$StateFiles` 清单加 `secrets.json`（19998 的家是快照）。

## 3. 实施顺序与门禁

每阶段测试先行（改前红、改后绿），Node 仓库 `npx vitest run` 全量，Rust 仓库
`cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`。
面板改动只在 Node 仓库做、整目录复制回 `crates/swiss-panel/src/admin_assets/`（ADR-009）。
**Node 服务端同步实现同款路由与存储**（它的 secure/envstore.ts 同款封印）——面板字节同源，
Node 没有路由等于破了它自己的面板。

- **Phase 1（core+host）**：refs.rs 双引用扫描器（单测：混合串、`[A-Z0-9_]+` env、
  `secret://kebab-name`、非法名报错、旧文法不受扰、`secret://` 不被 env 扫描器误食）；
  secretstore.rs（封印读写 + 文法校验单测）；config/actions 接新解析器 + 失败语义；
  /api/secrets 三条路由（oneshot 集成测：CRUD、GET 全文无值、rev 409）。
- **Phase 2（tunnels+jobs）**：换共享解析器（ssh.rs 去掉对 swiss-host 的导入；runner.rs
  副本退役）；隧道连接失败 reason 单测；job run 失败记录单测；**子进程隔离集成测**——
  库里存一个密钥，job 跑 `cmd /c set`，断言输出含 job 自带 env 变量、不含密钥。

**每个使用面一张测试表（D4 清单逐面覆盖，缺一不发货）**——使用面的行为 = 单元测试锁解析
结果与错误措辞，集成测试锁真跑起来的端到端：

| 使用面 | 单元测试 | 集成测试 |
|---|---|---|
| http/rest header、url、body | resolve 结果串、缺失报错措辞 | echo 探针断言收到的 header 是真值 |
| proc args、env | 同上（resolve_def_checked 树递归含数组/嵌套） | 子进程把收到的 env 回显，断言含真值 |
| 隧道 password / keyPassphrase | 缺失 reason 措辞 | 连接路径展开（既有假 SSH 测试架） |
| job command、env 值 | 缺失 run 失败记录 | job 输出含替换后真值；隔离测见上 |
| 面板 Test 端点 | — | PUT 密钥 → Test 200；删密钥 → Test 失败且不泄值 |
| mcpmatch | 解析失败=不匹配 | 匹配到引用同一密钥的两条定义 |
| 通用回归 | 表驱动：六面 × {命中、缺失、混合 env+secret} | — |
- **Phase 3（面板）**：Gateway 页 Secrets 区（vitest：渲染只含名字、表单 write-only、
  删除确认、掩码显示 `secret://…` 引用）；hint 文案；复制回 Rust。
- **Phase 4（收尾）**：export/import 段 + 回归测试；test-instance.ps1 清单；19998 实测
  （保存 → 引用 → 隔离 → export）；ADR-014 入 docs/07。

19999 全程不动；部署照旧是最后一步、且只在用户看过 19998 之后。

**实施备注（与文本的偏差，均已随提交落地）**：

- D2 说"平得跟 env store 一样"——rev 需要一个计数器，所以信封是
  `{"rev": N, "secrets": {…}}`，值在 rev 下平铺一层（随 Phase 1 提交记录）。
- import 是合并不是镜像：bundle 里没提到的名字保留原值（与 env 段语义一致），合并
  读的是文件而非内存 vault——`swiss import` 跑在没注入过 vault 的新 CLI 进程里。
  面板/进程内 vault 在重启后才看到导入结果，与 env 段的既有行为相同。
- 面板编辑表单显示引用串：**整值恰为一个引用**时放行（`secret://name`、`${VAR}`
  同规则，沿用 mask.ts 既有契约）；`Bearer secret://x` 这类混合值仍打码——它和
  `Bearer ${X}` 的既有行为一致，混合值本身可能就是机密。验收第 1 条的引用串以
  精确引用头（`X-Api-Key=secret://context7`）为准。
- `swiss import`（CLI 进程）写盘后，正在运行的 daemon 不热重载 vault；重启生效。
  验收第 6 条按"export → 删 → import → 重启 → 引用照常解析"执行。

## 4. 验收清单（19998，全部要过）

1. Gateway 页保存 `context7` 的 key → managed.json 里 http header 写
   `secret://context7` → MCP Test 通过，面板编辑表单显示引用串。
2. 删除该密钥 → MCP 启动被拒，错误点名 `context7`；值不出现在任何输出。
3. job `cmd /c set` 的输出不含库内任何密钥（用 job 自带 env 变量作对照阳性）。
4. secrets.json 原始字节里搜不到明文 key。
5. `GET /api/secrets` 响应全文不含值。
6. `swiss export` 含 secrets 段；import 后引用照常解析。
7. Secrets 区 CRUD 全流程 + rev 冲突 409。

## 5. 不做什么（重申）

不加依赖（zeroize 也不加）、不改封印格式、不动 `${ENV}`、不迁移 token、不做 reveal、
不做过期与轮换、不做非 Windows 绑定抽象、不把 env store 并进来。

## 6. 交给实施模型的 Prompt

按 docs/19 实施。语法是 `secret://name`（URI scheme 型，选型依据见 D1）；替换是**通用
合同**（D4）：任何使用面的字符串到达实际使用点前必过共享解析器，§3 的使用面测试表逐面覆盖、
缺一不发货。密钥库是 swiss-core 基础层（§2 定位），不是可停用插件。
先读锚点：`crates/swiss-core/src/secure/envstore.rs`（overlay 模式与封印
读写的样板）、`crates/swiss-host/src/config.rs`（`resolve_env_refs`/`is_env_ref`）、
`crates/swiss-tunnels/src/tunnel/ssh.rs` 与 `crates/swiss-jobs/src/jobs/runner.rs`（两个
使用边界）、`src/daemon.rs`（export_state）、`src/app.rs`（路由挂载）、Node 侧
`secure/envstore.ts` 与 `src/admin/js/views/plugins.js`（Gateway 页）。
按 §3 的四个阶段推进，每阶段一提交（Rust 侧一个 + Node 侧一个），测试先行，门禁全绿才进下一
阶段。偏差（措辞、命名、结构）记录进提交信息与最终汇报；颜色/文案类留给用户过目。
面板测试模式沿用 admin-navigation / admin-row-menu 的 fake-DOM 契约测试；Rust 集成测试走
tower oneshot，不起真端口。19998 实测按 §4 清单，截图留证；**19999 不动**。
