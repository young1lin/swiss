# 19 — 密钥库（Secret Vault）：保存一次，处处引用，设备绑定

> 需求原文（用户，2026-09-12）："保存密钥，后续其他服务只需要引用密钥即可，运行时会自动替换成
> 保存的密钥，然后密钥只和设备绑定也就是那个 DPAPI。"
> 本 spec 把它落成一件小而完整的东西：一个名字空间隔离的密钥库 `secrets.json`——同封印格式、
> 只经 `${secret:NAME}` 引用展开输出、面板可管理、永不进子进程环境。

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
- 四个既有展开边界全部识别 `${secret:NAME}`，运行时自动替换——引用者无需任何新配置面。
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

### D1 引用语法：`${secret:NAME}`

- `NAME` 文法：`[a-z][a-z0-9-]{0,63}`（小写 kebab）。为什么小写：与 `${UPPER_ENV}` 在肉眼
  与文法两层都不可混淆——大写名字直接拒绝，错误信息说明"这是密钥引用，名字用小写 kebab"。
- 前进兼容是免费的：现行扫描器只认 `[A-Z0-9_]+`（config.rs:79 起，ADR-007 手写），旧构建把
  `${secret:x}` 当字面文本留在原处——可见的坏（header 里出现明文引用、API 401），而不是
  静默泄密。
- **扫描器收敛到 swiss-core**：新模块 `crates/swiss-core/src/secure/refs.rs`，一个函数同时认
  两种引用（env 查 overlay+进程环境，secret 查密钥库）。jobs runner 的宽松副本换掉，
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

### D4 展开与失败语义：env 宽松、secret 硬失败

- `${ENV}` 缺失 → 空串（不变）。
- `${secret:NAME}` 缺失 → **硬失败**，错误点名使用处与名字，值永不进错误文本：
  - MCP 适配器构建：启动被拒，reason 形如 `mysql: headers.Authorization references secret
    'x' which is not in the vault`（与"端口缺失不得进监听器"同一哲学：宁可不启，不静默空串）；
  - 隧道：连接失败 reason 同款措辞（进既有 FailureKind 文案）；
  - job：该次 run 记录失败，原因同款（不弹进程）；
  - mcpmatch：解析失败的候选按不匹配处理，warn 一次。
- 为什么不对称：env 是机器现状，缺席正常；vault 引用是操作者的显式声明，缺席是配置错误，
  面板上一眼可修。

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
- 编辑表单里已是 `${secret:NAME}` 的字段照旧显示引用串：`is_env_ref`（config.rs）的掩码
  判定扩展到 secret 引用，面板侧无需新逻辑。
- add-sheet 两处 hint 文案改为指向密钥库（"`${secret:context7}` —— 在 Gateway 页的 Secrets
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

- **Phase 1（core+host）**：refs.rs 双引用扫描器（单测：混合串、`[A-Z0-9_]+` env、kebab
  secret、非法名报错、旧文法不受扰）；secretstore.rs（封印读写 + 文法校验单测）；
  config/actions 接新解析器 + 失败语义；/api/secrets 三条路由（oneshot 集成测：CRUD、
  GET 全文无值、rev 409）。
- **Phase 2（tunnels+jobs）**：换共享解析器（ssh.rs 去掉对 swiss-host 的导入；runner.rs
  副本退役）；隧道连接失败 reason 单测；job run 失败记录单测；**子进程隔离集成测**——
  库里存一个密钥，job 跑 `cmd /c set`，断言输出含 job 自带 env 变量、不含密钥。
- **Phase 3（面板）**：Gateway 页 Secrets 区（vitest：渲染只含名字、表单 write-only、
  删除确认、掩码显示 `${secret:…}` 引用）；hint 文案；复制回 Rust。
- **Phase 4（收尾）**：export/import 段 + 回归测试；test-instance.ps1 清单；19998 实测
  （保存 → 引用 → 隔离 → export）；ADR-014 入 docs/07。

19999 全程不动；部署照旧是最后一步、且只在用户看过 19998 之后。

## 4. 验收清单（19998，全部要过）

1. Gateway 页保存 `context7` 的 key → managed.json 里 http header 写
   `${secret:context7}` → MCP Test 通过，面板编辑表单显示引用串。
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

按 docs/19 实施。先读锚点：`crates/swiss-core/src/secure/envstore.rs`（overlay 模式与封印
读写的样板）、`crates/swiss-host/src/config.rs`（`resolve_env_refs`/`is_env_ref`）、
`crates/swiss-tunnels/src/tunnel/ssh.rs` 与 `crates/swiss-jobs/src/jobs/runner.rs`（两个
使用边界）、`src/daemon.rs`（export_state）、`src/app.rs`（路由挂载）、Node 侧
`secure/envstore.ts` 与 `src/admin/js/views/plugins.js`（Gateway 页）。
按 §3 的四个阶段推进，每阶段一提交（Rust 侧一个 + Node 侧一个），测试先行，门禁全绿才进下一
阶段。偏差（措辞、命名、结构）记录进提交信息与最终汇报；颜色/文案类留给用户过目。
面板测试模式沿用 admin-navigation / admin-row-menu 的 fake-DOM 契约测试；Rust 集成测试走
tower oneshot，不起真端口。19998 实测按 §4 清单，截图留证；**19999 不动**。
