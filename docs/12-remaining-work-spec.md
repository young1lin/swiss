# 12 — 剩余工作实施规范（文档校正 / 内存实测 / P4 / P6 / P5）

> 状态：**全部已实施**。写作基线 `5f18951`，完成基线 `4147e8a`。
> W1 文档校正 · W2 内存实测（2026-09-11，见下）· W3 连接目录 `9e76351` · W4 试金石插件 `9cdd7c6`
> · W5 workspace 拆分 `4147e8a`。W2 最终在 `3c3fd7f`（ADR-012 之后的 release）上完成：
> 本机没有独立 DB 服务，mysql/pg/redis 全部经网关自己的 SSH 隧道到达，压测用官方 MCP SDK
> 客户端直打各适配器端点（`../local-mcp-gateway/scripts/w2-drive.mjs`），Node 侧同数据目录
> 同流量顺序复测（隧道本地端口互斥，无法并排）。数字在 docs/01 的表里。
> Jobs 的部分单独在 [11](11-jobs-v2-implementation-spec.md)。
> 前置阅读：`AGENTS.md`、`docs/09-toolbox-plugin-architecture.md` §3/§4/§10。

## 0. 怎么用这份文档

五项工作 W1–W5，彼此独立可交付，**一项一个提交**。建议顺序：

```
W1 文档校正  →  W2 内存实测  →  W3 (P4) 连接能力解耦  →  W4 (P6) 试金石插件  →  W5 (P5) workspace
```

W1 最便宜且防止后来者判断失误；W2 是整个项目前提的复核，越早知道越好；W3 在 W5 之前，因为**先把依赖理顺再搬目录**，反过来只是把纠缠的依赖搬进了 crate 边界里，然后被编译器一次性顶回来。

W4 与 docs/09 的 P5→P6 顺序相反，是有意的：试金石插件是发现契约缺口最便宜的方式，而 workspace 拆分是机械搬迁，应该在契约稳定之后做一次，而不是在契约还会变的时候做两次。

门禁命令（每项提交前全过）：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d          # 重复的 TLS 栈或 runtime 必须让评审失败
```

---

## W1 — 文档校正 ✅ 已完成（与本文同一提交）

`5f18951` 实现了插件宿主，但几处文档还停在实现之前，会让下一个读者（人或模型）做出错误判断。
这一项已经做完，下面是改动记录，不需要重做——**但接手 W3–W5 时请核对它仍然属实**。

改过的五处：

1. **`docs/09` 第 3 行**：`> 状态：设计提议，尚未实施重构。代码基线：81882cc。`
   → 改为：P1–P3 已在 `5f18951` 落地（PluginHost、ActionRegistry/RunCoordinator/Supervisor、面板 PageRegistry 与 `/api/plugins`），未实施的是 P4–P6；代码基线更新为 `5f18951`。
2. **`docs/09` §11「本次实际完成与未完成」**：整节重写成当前事实。原文"没有实现上述 v2 schema/插件宿主"已经不成立；v2 schema 仍未实现这一半要保留，并指向 docs/11。
3. **`README.md`**（"The toolbox proposals are not implemented configuration or API documentation." 那一段）：改为——docs/09 的插件宿主与 docs/10 的执行半边已实现；配置驱动的 Jobs（docs/10 §5、§9 步骤 3–7）仍是提议，实施规范见 docs/11。README 的文档表格补上 docs/11 与 docs/12 两行。
4. **`docs/06` 的「Current implementation status」**：补一句插件宿主与共享运行服务已落地，并明确 Phase 6（并行运行一周后切换）与 docs/01 的内存实测仍未做。
5. **`docs/10` 第 3 行**：原文"设计提议，尚未实现"同样过期——执行半边已经是代码。改为逐条列出已落地与未落地的小节，并指向 docs/11。

顺带处理了一个仓库卫生问题：`.claude/` 里只有 Claude Code 的 agent worktree 缓存，已经进
`.gitignore`（`/.claude/worktrees/` 与 `/.claude/settings.local.json`）。将来若出现希望团队共享的
项目级 `.claude/settings.json`，那一个文件应当提交。

**验收**：`grep -rn "尚未实施\|not implemented configuration" docs/ README.md` 无残留；
`git status` 里没有说不清来历的未跟踪文件。

---

## W2 — 内存实测：把 docs/01 剩下的两行填掉

这是整个项目存在的理由。`docs/01-goals-and-memory-budget.md` 的表里现在有三个数字：Node 基线 117.5 MB、Phase 0 spike 9.1 MB、Phase 1（面板 + echo，全功能二进制）14.0 MB——最后这个是 W5 拆分时按同一套方法量的，不需要外部服务。**仍然空着的是 Phase 2 和 Phase 4**，也就是真正有说服力的两行：它们要的是活的 mysql / pg / redis 和一条真实 SSH 目标。

### 测量方法（照 docs/01 §"如何测"执行，此处固化为可重复步骤）

1. 构建：`cargo build --release`（出厂组合）。
2. Node 与 Rust **对同一个数据目录**，分别监听 19999 / 19998。
3. 用真实 MCP 客户端依次触达：echo → mysql → pg → 2×redis → 1×http；每个至少 10 次调用，总计不少于 60 次请求。
4. 三个数字一起记，缺一个都不算完整：
   - `/api/memory` 的 `gatewayMb`（进程自报）
   - 任务管理器的工作集（Working Set）
   - 线程数与私有字节（Private Bytes）
5. 每一行都写清"哪些 adapter 是活的"，以及 `proc` 子进程是睡着还是醒着——一个醒着的 `uvx` 子进程 50–150 MB，混进来这张表就没有意义了（AGENTS.md 的 non-goal）。

### 要填的行

| 行 | 条件 | 状态 |
| --- | --- | --- |
| Rust Phase 1 | echo + 面板，无 DB | ✅ 14.0 MB（`4147e8a`，2026-09-10） |
| Rust Phase 2 | mysql + pg + 2×redis | ✅ 21.8 MB（`3c3fd7f`，2026-09-11，DB 全经自身隧道） |
| Rust Phase 4 | 全功能（含 tunnels、http） | ✅ 22.4 MB（`3c3fd7f`，2026-09-11，60/60 调用；Node 同流量复测 113.8 MB） |

### 判定

没有预算带，没有 kill line：数字本身就是交付物（用户 2026-09-11 的决定——内存数字只做
记录，不做门槛；debug 构建超多少都无所谓）。唯一的判定是与 Node 同流量对照的差距
（Node 基线 117.5 MB，2026-09-11 复测 113.8 MB）。测完把数字连同日期、构建 commit 一起
写进 docs/01 的表，并在 README 的一句话摘要里更新。

**验收**：docs/01 的表里 Phase 2 与 Phase 4 两行有数字、有日期、有条件说明。

---

## W3（docs/09 P4）— MCP 与 Data 的连接能力解耦 ✅ 已完成（`9e76351`）

### 现状与问题

`src/builtin.rs` 的 `DataInstance` 自己承认了：Data **不拥有任何资源**，每次浏览都借 MCP adapter 已有的连接池或子进程，因此**停用 MCP 会连带把 Data 打瘫**，而 Data 的插件行却显示为正常。这是 docs/09 §4 诚实原则的一个缺口：两个插件号称能独立启停，实际上一个依附另一个。

现在的接缝是 `src/app.rs` 的 `browser_resolver()`——一个闭包，每次调用扫描 registry 并返回 `BrowsableConnection` 列表。它足够解耦得动，但它没有租约（lease）语义：Data 无法表达"我正在用这条连接"，MCP 也无法知道"现在关掉会打断谁"。

### 目标契约

新增 `crates/swiss-host/src/services/catalog.rs`，与 `ActionRegistry` 同级的**类型化能力注册**：

```rust
/// A connection definition someone can browse or call, independent of who owns the driver.
/// The key must carry configuration identity, credential context and read-only semantics -
/// two connections that merely share host and port are NOT the same connection.
pub struct ConnectionInfo {
    pub id: String,
    pub label: String,
    pub dialect: String,
    pub readonly: bool,
    pub state: String,
}

pub trait ConnectionCatalog: Send + Sync {
    fn list(&self) -> Vec<ConnectionInfo>;
    /// Take a lease on one connection. While a lease is held the provider must keep the
    /// underlying pool or child alive; dropping the lease is what allows it to close.
    fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError>;
}

pub enum CatalogError {
    /// No such connection id.
    Unknown(String),
    /// The provider is shutting down and will not hand out new leases.
    Withdrawing(String),
    /// The connection exists but has no browsable driver behind it (echo, http, ...).
    NotBrowsable(String),
}
```

`ConnectionLease` 的 `Drop` 释放租约，不需要调用方记得。

注册表规则（照 `ActionRegistry` 的形状）：

- 同一时刻**只允许一个** provider 注册 `ConnectionCatalog`；重复注册返回错误，不允许后者静默覆盖前者（docs/09 §2 的 `provide` 冲突可见）。
- provider（MCP 插件）停止时撤销注册；撤销后 `lease()` 返回 `Withdrawing`。
- 已发出的租约**不会**被撤销打断：MCP 停止时先撤销注册（不再发新租约），再等待现有租约释放或超时（建议 5 秒），然后关闭连接。超时要在日志里报告"带着 N 个未释放的租约关闭"，而不是假装干净地关掉了。

### 两侧的改动

**MCP 插件（provider）**：把 `browser_resolver()` 的逻辑搬到 registry 侧的一个 `RegistryCatalog` 实现里，start 时注册、stop 时撤销。`src/app.rs` 的 `browser_resolver()` 保留为薄适配层直到 Data 改完，然后删掉。

**Data 插件（consumer）**：

- 每次 `/api/db` 请求内部取租约、用完释放（请求级租约，不是长期持有）。
- catalog 不可用时，`/api/db` 返回 **503**，信息里点名缺失的 provider（"the mcp plugin provides database connections and is currently disabled"），而不是返回一个空列表让用户以为自己没有数据库。
- Data 的插件行如实声明它对 catalog 能力的依赖：descriptor 增加 `requires: Vec<String>`（能力名，不是插件 id），inventory 里展示。这样面板能显示"Data 依赖 connection-catalog，当前无提供者"。

### 测试

- 租约持有期间 provider 停止：连接不被关闭，租约释放后才关闭；日志有一条等待记录。
- 超时路径：租约不释放时，5 秒后带告警关闭，并且**告警文本包含未释放的数量**。
- 重复注册被拒绝，先注册者仍然有效。
- MCP 停用 → `/api/db` 是 503 且点名 mcp；Data 插件行显示依赖未满足。
- Data 停用 → 它自己的租约全部释放，MCP 的连接不受影响（反向不误伤）。
- 连接 key 的身份：同 host/port 但凭据上下文不同的两个定义，不会被合并成一条（这是 docs/09 §3 点名的坑）。
- 反复启停 100 次：租约数、连接数回到空闲基线，不线性增长（docs/10 §9 验收最后一条）。

### 明确不做

- 不在本项工作里改 `managed.json` 的连接定义格式（docs/09 §3：先适配层映射）。
- 不把数据库驱动移进 plugin-api；驱动留在可选实现里。
- 不引入连接共享的跨插件缓存策略调整——本项只做所有权与生命周期，不做性能改动。

---

## W4（docs/09 P6）— 试金石插件：证明"加一个工具不用改核心" ✅ 已完成（`9cdd7c6`）

> 插件本体已于 2026-09-11 按用户决定删除（连同面板视图与专属测试）。试金石的使命——从零加插件、
> `host/` 零改动——已经完成，并由终端插件在同一路径上再次验证。下文保留为实施记录。

docs/09 §9 要的验收样例是 **HTTP 工具插件**。选它有一个现成理由：`reqwest` 已经在依赖图里（`crates/swiss-mcp/src/adapters/http.rs` 用着），所以这个插件不会让二进制变大，符合"每个新依赖都要证明自己的重量"。

### 范围（刻意小）

一个 `http-tools` 插件，提供：

- 一个 action：`http.request`（method、url、headers、body、timeoutMs；`${ENV_VAR}` 严格解析并注册进输出遮盖；响应体按 `output.maxBytes` 同款上限截断）。
- 一个页面：一个最小的请求构造器 + 响应查看器，表单**完全由 `GET /api/actions` 的 schema 生成**。
- 一行配置：`plugins.http-tools.config.allowedHosts`（默认空 = 全放行；填了就是允许列表）。

### 这项工作的真正验收标准

功能本身不重要，**下面这条才是**：

> 从零加上这个插件，`crates/swiss-host/src/host/` 下**一个文件都不需要改**，`src/server.rs` 只增加一行 `register`。

如果做的过程中发现必须改 host，那就说明契约有缺口——**先把缺口作为独立提交修掉**（并在 docs/09 §4 里补上这条契约），再回来加插件。这正是试金石的用途：它是用来发现问题的，不是用来展示成功的。

### 测试

- 插件启停：能力注册/撤销，页面出现/消失，路由边界 503。
- `allowedHosts` 生效：不在列表里的 host 是可见的拒绝，不是静默失败。
- 输出上限与凭据遮盖（复用 `services::process` 的遮盖工具，不要写第二份）。
- 一个"契约完整性"测试：断言 `host/` 目录下没有出现任何插件 id 的字符串常量（`grep` 式断言即可，粗糙但有效）。

---

## W5（docs/09 P5）— workspace 拆分 ✅ 已完成（`4147e8a`）

**这是机械搬迁，放在最后。** 它的价值是把依赖方向变成编译器能强制的东西，不是省内存——docs/09 §8 说得很清楚，**任何"拆目录省内存"的说法都不许写进提交信息或文档**。

### 建议的 crate 边界

| crate | 内容 | 允许依赖 |
| --- | --- | --- |
| `swiss-core` | `paths` `log` `util` `atomic_json` `mask` `paging` `platform/` `secure/` | 无（只依赖外部 crate） |
| `swiss-host` | `config_store` `host/` `services/` `auth` `local_only` `token` `app`（路由骨架） | core |
| `swiss-mcp` | `registry` `adapters/` `calls` `traffic` `managed` `mcp_import` `introspect` | core, host |
| `swiss-data` | `dbbrowser` `dbbrowser_api` `*_browser` | core, host |
| `swiss-tunnels` | `tunnel/` | core, host |
| `swiss-jobs` | `jobs/` | core, host |
| `swiss-panel` | `admin` + `admin_assets/`（rust-embed） | core |
| `swiss`（bin） | `main` `cli` `daemon` `server` `pidfile` `bootstrap` `subsystems` `skill_install` | 全部 |

强制规则：

- 依赖方向单向：`core ← host ← 各插件 crate ← bin`。**插件 crate 之间禁止互相依赖**——W3 之后 Data 不再依赖 MCP，这条才成立，所以顺序不能反。
- 产物仍是**单个静态 exe**，`-C target-feature=+crt-static`（ADR-006），CI 的五个目标不变。
- 拆分提交里**不允许有任何行为改动**：diff 应该几乎全是移动和 `use` 路径调整。行为改动另开提交。

### 验收

- 五个目标全部构建通过（CI 绿）。
- `cargo tree -d` 无重复 TLS 栈 / runtime。
- release 二进制体积与 W2 测得的 RSS **没有回归**（拆分前后各测一次，写进提交信息）。
- `git diff --stat` 里非移动性改动的行数接近 0，评审可以逐个确认。

---

## 交付与验收清单（我 review 时逐条对）

| 项 | 关键验收点 |
| --- | --- |
| W1 | 四处文档与事实一致；`git status` 干净 |
| W2 | docs/01 三行有数字、日期、条件；超预算时附诊断 |
| W3 | 租约生命周期测试齐全；MCP 停用时 `/api/db` 是点名的 503；100 次启停无泄漏 |
| W4 | `crates/swiss-host/src/host/` 零改动（若有改动，必须是独立的契约修补提交）；`src/server.rs` 只加一行 |
| W5 | 五目标构建；`cargo tree -d` 干净；无行为改动；无"省内存"的表述 |

通用（每一项都查）：

1. 四条门禁命令的输出贴在 PR 描述里。
2. `crates/swiss-panel/src/admin_assets/` 的改动为 0（W4 的页面除外，且必须走 Node 先行 + SHA256 核对）。
3. 新注释全英文。
4. 配置 / 磁盘 / 网络路径上没有新的 `.unwrap()`。
5. 新依赖必须在提交信息里说明它的重量，并且是 `default-features = false` 起步。
