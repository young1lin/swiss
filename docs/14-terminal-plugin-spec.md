# 14 — 终端插件实施规范（网页终端 / SSH / 本地 Shell）

> 状态：**已实施**。T1–T4 `310ffe2`→`378d8fd`，T5 `3877c17`，T6（Node 仓库）`873a238`，T7 `616b530`，
> T8 为本文所在提交。全部实施偏差见 §8.1。
> 前置阅读：`AGENTS.md`（它的规则高于本文任何便利）、`docs/09-toolbox-plugin-architecture.md`
> §3/§4/§9（插件契约、共享能力、加一个插件要做什么）、`docs/12-remaining-work-spec.md` W3
> （连接目录 —— 本文的能力契约照它抄）、`docs/07-decisions.md` ADR-004 / ADR-008 / ADR-009 / ADR-010。
> **本仓库的 `crates/lmg-panel/src/admin_assets/` 一个字节都不能改。** 面板改动先落在
> `../local-mcp-gateway/src/admin/`，再整目录复制回来。
> 新写的代码注释一律英文；文档散文中文。

## 0. 怎么用这份文档

分成 T1–T8 八个阶段，**一个阶段一个提交**。每个阶段独立可验收、独立可回退，且每个阶段结束时
`cargo build --release` 仍然出一个能跑的 `lmg.exe`。行为变化先有测试。

门禁，每个提交前全过：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d          # 多出来的第二份 TLS 栈、第二个 runtime、第二个 rand，都算评审失败
```

**这份规范与 `docs/13` 会在同一片区域相遇**：`docs/13` 改面板导航的渲染，本文加一个新页面。
两者的交界只有一处 —— 新插件贡献一个 `PageDescriptor`，导航自己会把它排进去。派活的顺序见文末。

## 1. 要做什么

一个插件，页面里是一个真正的终端，能开两种会话：

- **远端**：SSH 到用户已经在 Tunnels 里配好的那些主机 —— 同一份连接定义、同一份 host key TOFU、
  同一份凭据。不要求用户再配一遍。
- **本地**：本机的一个 shell（Windows 上 ConPTY，unix 上 openpty），默认**关闭**，要显式打开。

参照对象是 JumpServer。**先把「更先进」这句话说清楚**，否则它会变成一张永远做不完的功能清单：

| | JumpServer | 本插件 |
| --- | --- | --- |
| 部署 | Docker Compose、MySQL、Redis、多个服务 | 一个 exe，无外部依赖，回环端口 |
| 定位 | 组织级堡垒机：多租户、RBAC、账号金库、工单审批 | 单用户的开发者工具箱，跟 Data / Tunnels / Jobs 在同一个进程里 |
| 连接来源 | 自己的资产库 | **复用用户已有的隧道连接** —— 不存第二份主机与凭据 |
| 录制 | 自有格式 + 自带播放器 | **asciicast v2**，`asciinema play` 与任何网页播放器都能放 |
| 前端 | xterm.js | xterm.js（同一个，见 §2） |
| 内存 | 数百 MB 起 | 见 §7 的预算，超了就是 bug |

所以「更先进」兑现在四件具体的事上：**零部署**、**不重复存主机与凭据**、**标准可回放的录制**、
**跟其它插件同一套启停/配置/503 语义**。多租户 RBAC 与账号金库明确不做，理由是这个进程只服务
`127.0.0.1` 的一个人，把组织级的授权模型搬进来只会增加 14 MB 预算里的常驻字节，换不到任何安全性。

## 2. 前端：xterm.js，以及面板资源管道的三条硬约束

**框架选 xterm.js（`@xterm/xterm` 5.x），不用再比价。** VS Code 的集成终端、JumpServer 的 Luna、
Wetty、code-server 全都是它；能与它相提并论的替代品不存在。要装的东西：

- `@xterm/xterm` —— 终端本体与 `xterm.css`
- `@xterm/addon-fit` —— 尺寸自适应，`resize` 帧的数据来源
- `@xterm/addon-unicode11` —— 宽字符与 emoji 的列宽，中文用户直接受益
- `@xterm/addon-web-links` —— 输出里的 URL 可点
- `@xterm/addon-webgl` —— WebGL 渲染器；**它是纯 JS，没有 wasm**，这一点必须在 vendoring 时亲自
  确认（见下面的约束 1），确认不了就退回默认 DOM 渲染器并把结论写进提交信息

### 面板资源管道的三条硬约束（先读这三条，再决定怎么 vendor）

1. **只能是 UTF-8 文本，且扩展名只能是 `.html` / `.css` / `.js` / `.svg`。**
   `crates/lmg-panel/src/admin.rs` 的 `mime_of()` 只认这四种，`admin_asset()` 用
   `String::from_utf8_lossy` 返回 `String`。**字体、wasm、png 一律进不来** —— 不是难，是这条路
   不存在。所以：终端字体用系统等宽栈（`ui-monospace, SFMono-Regular, Consolas, "Cascadia Mono",
   monospace`），不要 Nerd Font、不要 web font。
2. **没有打包器，没有 npm 运行时。** 面板是无依赖的原生 ES 模块。xterm 的 UMD 产物要么本身可以
   当 ES 模块 import，要么就自己写一个十几行的本地 shim 把 UMD 的全局导出成具名导出。选后者时
   shim 归属 `js/vendor/`，并在文件头注明来源包名与**精确版本号**。
3. **面板是从 Node 仓库复制过来的（ADR-009）。** vendored 的文件也是面板的一部分，所以它们必须先
   落在 `../local-mcp-gateway/src/admin/js/vendor/`，再整树复制——包括纯 Rust 插件的页面，照样先在
   Node 仓库里安家（试金石插件 docs/12 W4 当时就是这么做的）。

### vendoring 的规矩

- 落地路径：`js/vendor/xterm/<version>/…`，版本号写进目录名。升级 = 新目录 + 改 import，旧目录
  在同一个提交里删掉，`git log` 于是能回答「这版终端是哪个 xterm」。
- 不许 minify、不许改一个字节。要 patch 就在自己的代码里包一层。
- 体积记在案：当前面板 41 个文件 / 511 KB。**vendoring 之后重新量一次，把两个数写进提交信息**，
  并核对 §7 的二进制体积预算。

## 3. 传输：WebSocket，以及它到底加了多少依赖

`axum` 现在是 `default-features = false, features = ["http1","json","query","tokio"]`，没有 `ws`。
终端要加上 `ws`。**加之前先算清楚代价** —— AGENTS.md 说每个依赖都要为自己的重量辩护。

`axum` 0.8.9 的 `ws` feature 展开是：

```
ws = ["dep:hyper", "tokio", "dep:tokio-tungstenite", "dep:sha1", "dep:base64"]
```

对着本仓库的 `Cargo.lock` 逐个核对（已核）：`hyper` 已在（axum 的 http1 就靠它）、`sha1` 0.10 已在、
`base64` 0.22.1 已在（与本仓库直接依赖的 0.23.1 并存，是既有事实，不是这次引入的）。

**净新增只有 `tokio-tungstenite` 0.29 与它带的 `tungstenite`。** 这是可以接受的价格，也是本文允许
的唯一一次依赖扩张。实施时用 `cargo tree -d` 复核，若 `tungstenite` 拖进了第二份 `rand` 或第二份
`http`，停下来先解决，别带着走。

**为什么不用 SSE + POST 绕开它**：SSE 是单向的，输入得走另一条 POST 通道，于是要自己做输入排序、
自己做半连接的存活探测、自己处理「输出流断了但输入流还活着」。省下一个 crate，换回三类 bug。不换。

**WS 的鉴权**：`/api/*` 今天没有 bearer，唯一的边界是 `src/app.rs` 的 `loopback_guard`
（peer IP + `Host` + `Origin`）。浏览器发起 WS 握手时**会**带 `Origin`，所以现有守卫对 WS 一样有效。
但握手没法带自定义 header，所以再加一层一次性票据（见 §8 的 `POST /api/terminal/sessions`）：
票据单次有效、10 秒过期、绑定会话 id。理由不是今天不够，是**把「能开一个终端」这件事显式化**，
将来谁要松动 Origin 检查，也松不到终端上。

## 4. 远端：SSH 走宿主能力契约，不走 crate 边界

`lmg-tunnels` 里已经有整套东西：`SshConnDef`（`crates/lmg-tunnels/src/tunnel/types.rs:103`）、
`SshConnection`（`ssh.rs`）、host key TOFU（`fingerprint()` + `SshHooks::on_host_key` +
`trust_host_key`）、连接的引用计数与共享（`manager.rs` 的 `conns` + `refs()`）。russh 0.63.2 的
channel 也**已经**支持 PTY，不需要任何新的 SSH 依赖：

```rust
// russh 0.63.2, verified signatures in channels/mod.rs
channel.request_pty(want_reply, term, col_width, row_height, pix_width, pix_height, &modes).await
channel.request_shell(want_reply).await
channel.window_change(col_width, row_height, pix_width, pix_height).await
channel.data(reader).await            // or data_bytes(impl Into<Bytes>)
```

**但 `lmg-terminal` 不许依赖 `lmg-tunnels`。** ADR-010 把这条写死了：五个子系统 crate 谁都不依赖
谁；出现这种需求，正确的读法是**宿主契约缺了一样东西**（docs/12 W3 的连接目录就是这么来的）。

所以加第二个共享能力，形状照 `crates/lmg-host/src/services/catalog.rs` 抄：

```rust
//! The interactive shell capability — who can open a PTY on a remote host, and for how long.
//! Same shape as services::catalog: ONE provider registers on start, consumers take a
//! session-scoped lease, and a provider stopping withdraws (no new leases), drains, then
//! closes. The terminal plugin never links an SSH client; the tunnels plugin never learns
//! that a terminal exists.

/// One host an interactive shell can be opened on. Identity is the tunnels connection id:
/// two servers that merely share host and port are NOT the same target (docs/09 §3).
pub struct ShellTarget {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// The provider's live connection state, so the panel can grey a target out honestly.
    pub state: String,
}

/// A transport-neutral duplex PTY. Deliberately NOT a russh type: this is what keeps the
/// consumer crate free of an SSH client.
pub struct PtySession { /* reader stream, bounded writer, resize, close */ }

#[async_trait]
pub trait ShellProvider: Send + Sync {
    fn list(&self) -> Vec<ShellTarget>;
    /// Open one interactive PTY. The provider owns the connection, its credentials and its
    /// host-key policy; the caller sees bytes and nothing else. While the returned session
    /// is alive the provider MUST keep the underlying client alive (it holds a ref), and
    /// dropping it is what allows the client to be released.
    async fn open(&self, id: &str, holder: &str, size: PtySize) -> Result<PtySession, ShellError>;
}
```

- 注册表 `ShellRegistry` 进 `RuntimeServices`（`services/mod.rs`），与 `catalog` 并列，**在插件之外
  构造**，这样 provider 停了再起还是同一个座位。
- provider 由 **Tunnels 插件**在 `start()` 时注册、`stop()` 时 withdraw-then-drain。实现放
  `crates/lmg-tunnels/`，它是唯一碰 russh 的地方。
- `SshLike` trait 加一个 `open_shell(...)`；引用计数**必须走 manager**，新增
  `TunnelManager::open_shell(conn_id, size) -> ShellSessionGuard`，guard 的 `Drop` 走 manager 现有的
  release 路径（`manager.rs:390` 那段 `refs().fetch_sub`）。别在 provider 里重写一遍引用计数：
  今天的规则是「最后一个用完的把连接关掉」，两处实现迟早对不上，对不上的表现是隧道被终端关掉。
- 能力探测：`server.rs` 现有的 `set_capability_probe` 扩一条 `"ssh-shell"`。

**不要给终端插件写 `requires: ["ssh-shell"]`。** `requires` 不满足会让整个插件进
`waitingDependency`，而本地会话根本不需要 SSH。正确做法是 `GET /api/terminal/targets` 借
`CatalogPresence` 的思路，把远端目标的缺席说清楚（「tunnels 插件已停用」而不是「没有主机」）。

## 5. 本地：ConPTY / openpty，不引 `portable-pty`

已核对：`windows` 0.58（本仓库已依赖）里三个 ConPTY 入口都在 ——
`CreatePseudoConsole` / `ResizePseudoConsole` / `ClosePseudoConsole`（`Win32/System/Console`），
配套的 `InitializeProcThreadAttributeList` / `UpdateProcThreadAttribute` /
`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`（`Win32/System/Threading`，本仓库已开该 feature）。
要新开的 feature 只有两个：`Win32_System_Console`、`Win32_System_Pipes`。

`portable-pty` 能省下这段 FFI，但它会拖进自己的一串依赖，且在 Windows 上还带一条 winpty 的兼容
路径 —— 这个进程的预算是 15 MB，AGENTS.md 的原话是「一个新依赖要为自己的重量辩护」。手写 FFI
大约 150 行，且这个仓库已经有 Windows FFI 的落脚点。

**位置**：`crates/lmg-core/src/platform/pty.rs`，暴露一个安全 API
（`open_pty(size, command, env, cwd) -> (PtyMaster, Child)`）。这样 `unsafe` 全部留在 `lmg-core` 的
platform 模块里（AGENTS.md：unsafe 只属于 Windows FFI 边界），`lmg-terminal` 一行 unsafe 都没有。
unix 侧用已有的 `libc` 依赖走 `openpty`。

**子进程回收**：本地 shell 必须进 Job Object（ADR-008，`KILL_ON_JOB_CLOSE`）。逻辑已经在
`crates/lmg-host/src/services/process.rs`，把里面的 job 分配抽成一个可复用的函数即可。
**不要**把 PTY 塞进 `Supervisor` 的采集路径：那条路径是给「跑完就结束、输出有硬字节上限」的
action 用的，交互式会话既不结束也没有上限，塞进去只会把两种语义都弄坏。要复用的只有「把子进程
挂到 job 上」这一件事。

**读取线程**：Windows 上 ConPTY 的输出是匿名管道，是阻塞句柄，tokio 轮询不了。所以每个本地会话
要一个 `spawn_blocking` 的读线程。运行时是 `current_thread`（ADR-003），但 `spawn_blocking` 用的是
独立的阻塞线程池，不冲突。**代价是一个线程栈**，这是本地会话要设上限的直接原因（§7）。
远端会话没有这个问题：russh 的 channel 本来就是异步的。

**默认 shell 的解析顺序**（docs/15 §2.1 定稿，实现是 `conpty.rs` 里的纯函数族）：Windows 上
`pwsh.exe`（PATH 可解析）→ `powershell.exe` → `COMSPEC` → 裸 `cmd.exe`；unix 不变
（`$SHELL` → `/bin/sh`）。在 PATH 上找可执行按 `CreateProcessW` 的规矩来：名字原样先试，
无扩展名才追加 `PATHEXT`；探测只问 `metadata().is_file()`——WindowsApps 下的执行别名
（reparse point）过这一关，而 `canonicalize` 会把它溶进版本化的包目录里，路径随每次 Store
更新失效，所以不用。`GET /api/terminal/targets` 的 `local.shell` 报**解析后的绝对路径**，
`local.shells` 是插件 start 时探测一次并缓存的候选表（pwsh、Windows PowerShell、cmd，加
`%ProgramFiles%` 下的 PowerShell 7 与 Git bash 两个 PATH 之外的固定位置），绝不在每次 GET
时碰文件系统。

## 6. 安全与审计

1. **本地 shell 默认关闭。** `plugins.terminal.config.local.enabled` 默认 `false`，写进
   `config_schema`。开关在**终端页**：目标下拉旁的齿轮（或本地行缺席时的「Local shell is
   off — turn it on」）打开 Local shell 设置 sheet（docs/15 §2；Plugins 页的通用 schema
   表单是 docs/09 P3 的承诺，仍单独立项不做）。理由要说明白：网关本来就以用户身份运行，本地
   shell 不给**本地**攻击者任何新东西；但它把一个回环 HTTP 端口变成了任意代码执行入口，这个
   升级值得用户亲手点一下。
2. **远端目标可以再收窄。** `plugins.terminal.config.allowedTargets: []`（空 = 全部隧道连接）。
3. **凭据永远不落地在这里。** 终端从头到尾看不到密码或私钥 —— provider 开好 channel 递过来的是
   字节流。配置里若真需要写凭据，规矩不变：`${ENV_VAR}` 引用，绝不写字面量。
4. **录制：asciicast v2，只录输出。** 路径 `~/.mcp-gateway/terminal/<sessionId>.cast`，一行 JSON
   一个事件，与 `calls.jsonl` 同样的原子写与体积上限。**只录输出不录输入**是有意的：asciicast 本来
   就是输出格式，而不录键盘就顺带避开了「把 sudo 密码录进文件」这类问题（不回显的输入在输出流里
   本就没有）。单文件上限（建议 8 MB）到了就停录并写一条明确的结束标记，**不要静默轮转**。
   这些文件带真实会话内容，`.gitignore` 里必须有，跟 `*.log` 同级。
5. **不做命令黑名单。** 在一条交互式 PTY 字节流上做命令过滤是安全剧场：`e""cho`、别名、粘贴、
   TUI 程序、`base64 -d | sh`，随便一条都能绕过。宁可诚实地说「不做」，也不要给用户一个假的护栏。
   要限制就在远端主机上用真正的机制（sudoers、受限 shell）。
6. **会话上限与超时**：`maxSessions`（默认 4）、`maxSessionsPerTarget`（默认 2）、
   `idleTimeoutMinutes`（默认 30）。超时关闭要给终端里写一行可见的原因，不能悄悄断。
7. **WS 断开 ≠ 会话立刻死**：留一个 `graceSeconds`（默认 60）的重连窗口，期间输出进一个**有上限的**
   追赶缓冲（64 KB，见 §7）。窗口过了才真正 kill。笔记本合盖不该杀掉一个正在跑的编译。
8. **不丢字节，只做背压。** 输出通道满了就**停止读取** PTY / SSH channel，让 SSH 的窗口机制和管道
   自己把压力传回去 —— 这正是真实 tty 的行为。**绝不丢弃字节**：丢一段转义序列，终端显示就永久
   错位。若发送队列持续满超过 `stallSeconds`（默认 30），关闭会话并说明原因。

## 7. 内存与体积预算

这个项目存在的理由是 14 MB（`docs/01`）。终端是最容易把它吹掉的一个功能，所以预算写在前面，
**实测数字写进 `docs/01`**，超了就按 §11 的退路走。

| 项 | 预算 | 怎么量 | 实测（2026-09-11，release，活网关） |
| --- | --- | --- | --- |
| 二进制体积（release + mongo） | +≤ 800 KB | 当前 9,972,224 字节（ADR-010），加 vendored xterm 之后重量一次 | **7,925,760 字节**：vendored xterm 的重量被其后的 mongo（ADR-012）与 http-tools 删除抵消有余，比基线还小 2.0 MB |
| 插件加载、零会话时的 RSS | +≤ 1.0 MB | 任务管理器工作集，与今天的 14.0 MB 对比 | **≈ 0**：禁用/启用插件的工作集差 22,904↔22,940 KB，在测量噪声内 |
| 每个空闲远端会话 | +≤ 256 KB | 开 1 个和开 4 个，各量一次，差值除以 3 | **250.7 KB**（ADR-011：1 会话 +586 KB 含通道一次性成本，4 会话 24,596 KB） |
| 每个空闲本地会话 | +≤ 1.0 MB | 含 `spawn_blocking` 线程栈；这是本地会话另设上限的原因 | **108 KB**（临时启用 local、开 1 个 cmd 会话的工作集差；子进程内存另计） |
| 断线后的追赶缓冲 | 64 KB × 会话数，硬上限 | 代码里是常量，不是配置项 | §10 第 6 条实测：断开 8 秒后重连，L8→L60 连续补齐无缺无重 |

**面板侧的体积不算进 RSS 但算进 exe**：`rust-embed` 在 release 下把资源编进只读段，不用不驻留。
即便如此也要量，并写进提交信息。

若实测超预算：先看是不是 WebGL addon 的 JS 太大（可去），再看是不是追赶缓冲写成了无上限。
最后的退路是把整个插件放到一个 cargo feature 后面，**但这是退路，
不是默认设计** —— 先量，再决定，别一上来就加 feature 门。

## 8. 线上契约

```
GET    /api/terminal/targets
       -> { "local": { "enabled": bool, "shell": "pwsh.exe" },
            "remote": { "presence": "serving" | "stopping" | "absent",
                        "reason": "tunnels plugin is disabled ...",   // absent 时才有
                        "targets": [ { id, label, host, port, username, state } ] } }

POST   /api/terminal/sessions        { target: "local" | "<connId>", cols, rows, shell? }
       -> 201 { "id": "...", "ticket": "...", "recording": "<path>" | null }

GET    /api/terminal/sessions
       -> [ { id, target, label, opened, bytesOut, attached: bool, recording } ]

GET    /api/terminal/sessions/{id}/stream?ticket=…          (WebSocket upgrade)

POST   /api/terminal/sessions/{id}/resize   { cols, rows }  // 冗余通道，见下
DELETE /api/terminal/sessions/{id}
```

**附着是扇出,不是接管**(2026-09-11 起):同一个会话可以有多个附着 socket,输出复制给每一个,
任一附着的输入都进 PTY;一个 socket 掉线只移除它自己,**最后一个**掉线才开始宽限计时。这正是
"两个页面开同一个会话"的语义 —— 第二个页面附着不许弄瞎第一个。(`POST …/ticket` 为重连与第二
页面附着 mint 新票的路由。)

WS 帧：

- **二进制帧 = 原始 PTY 字节**，两个方向都是。不做 base64、不做 JSON 包装 —— 一个终端每秒可以
  产生几 MB，包一层就是白烧 CPU 和内存。
- **文本帧 = 一个小 JSON 控制对象**：
  `{"t":"resize","cols":120,"rows":30}`（客户端→服务端）、
  `{"t":"exit","code":0}`、`{"t":"error","message":"…"}`、`{"t":"stalled"}`（服务端→客户端）。
- 心跳用 WS 自己的 ping/pong，不要自己发 `{"t":"ping"}`。

`cols` / `rows` 服务端 clamp 到 `1..=1000`，越界是拒绝不是截断到默认值 —— 一个 0 列的 PTY 在远端
是未定义行为。

`POST /resize` 与 resize 帧同时存在是有意的：WS 断开重连的窗口期里没有帧可发，但面板可能已经因为
窗口变化需要改尺寸。两条路走同一个函数。

插件描述符：

```rust
PluginDescriptor {
    id: "terminal", kind: "terminal", label: "Terminal", version: "0.1",
    pages: vec![page("terminal", "terminal", "Terminal", 70, true)],
    routes: vec!["/api/terminal".into()],   // route_owner does prefix matching
    restart_on_config_change: true,          // the allowlist and the local switch are read at start
    requires: Vec::new(),                    // NOT ["ssh-shell"] - see the end of section 4
    ..
}
```

`order: 70` 排在 `jobs` 的 50 之后、`plugins` 的 1000 之前。按 `docs/13` 的分组规则，它自成
一个一级组，没有二级栏。

### 8.1 实施偏差记录（实施后补，2026-09-10）

实施与本文的每一处出入，按阶段收录；这里的每一条在对应提交信息里有完整的推理。

**T2（§4，`1fc2d12`）**

- `open_shell` 的返回不是 `-> ShellSessionGuard`：guard 随泵任务**走进去**而不是返回给调用方 ——
  只有泵知道会话何时真正结束，返回给 provider 的 guard 没有地方存活。
- 停止窗口 3s，不是连接目录的 5s：挂着的终端不会自己交还，排空只为还在落地的 open。
- 等 `want_reply` 的回执时必须**跳过 `WindowAdjusted`**：russh 把通道窗口调整同普通消息一样从
  `wait()` 投递，而 OpenSSH 恰好在 shell 启动瞬间发一条 2 MiB 的调整 —— 不跳过它，
  每次远程 open 都死于 "answered unexpectedly"（真机首次连接抓到，诊断靠把枚举变体
  打进错误文本后重放）。

**T3（§5，`eb01733`）**

- windows features 三个不是两个：`Win32_System_Pipes` 新增，`Win32_Security` 显式点名
  （原本只经 DPAPI 传递依赖；删 DPAPI 的人不该顺手带走 pty 和作业守卫）。
- `open_pty` 返回 `PtyHandle` + `PtyPump` 一对：ConPTY 的输出管道 tokio 轮询不了，本地会话
  就是值一条阻塞线程 —— 线程预算就是这个拆分的原因。
- `KillOnCloseJob` 从 lmg-host 下沉到 `lmg_core::platform`：pty 缝隙在 lmg-host 之下，
  需要同一份子树保证。

**T4（§8，`378d8fd`）**

- `ticket(id)` 成为 `open()` 之外的第二个入口 → §8 路由表因此多一条补铸路由（见 T5）。
- 录像只写 `"o"` 事件，不写 `"r"`。
- `TerminalError` 手写 `From<ShellError>`，不用 `#[from]`：消费者不该反过来规定契约的形状。

**T5（§8，`3877c17`）**

- 描述符草案里 `page(…, 70, true)` 的第 5 参是**错的**：`sidebar:true` 的含义是"本页渲染进
  MCP 侧栏布局"（壳层 `page-registry.js` 对没有该标志的每个页面隐藏 `.sidebar`），只有
  mcps 页该有。照抄草案让终端页旁边常驻 MCP 服务器列表 —— 首次真人浏览器点击发现，
  修正为 `false` 并在插件工厂测试里钉死。
- `POST /api/terminal/sessions/{id}/ticket` 不在 §8 路由表里：ticket 活 10s、宽限 60s，
  开球时的 ticket 永远活不到重连，必须先补铸一张。
- 不带 ticket 的 stream 答 **400** 并点名 mint 路由（表里暗示默认 403；缺 ticket 是客户端
  bug，不是被拒的凭证）。
- `shutdown()` 越过逐会话 Close，直接丢弃表的命令发送端 → 停止中的插件对客户端说
  `closed: the terminal plugin is stopping`，把 "closed on request" 留给 DELETE 路径。

**T6（§2，Node 仓库 `873a238`）**

- xterm 的 UMD 构建不能 `import()`：addon-unicode11 绑定顶层 `this`，在模块里是 undefined
  → 改为经典 `<script>` 标签注入（`load-classic.js`），每包一个 `index.js` shim 再导出命名
  导出。vendored 字节未动，版本号在目录名里，升级即新目录加改一行 import。
- WebGL addon 先核验为纯 JS（无 WebAssembly、无 .wasm fetch、无 base64 载荷）后随包发布；
  上下文丢失即 dispose，回落 DOM 渲染 —— 三条资源约束里 "不改 mime_of" 因此原样成立。

**T7（`616b530`）**：无偏差；`the_tree_is_byte_for_byte_the_node_builds` 真跑真过。

**docs/15（本文 §5/§6.1 的补丁，2026-09-12）**

- `default_shell` 的解析顺序落在 `conpty.rs` 的纯函数族里（`find_on_path_with` /
  `default_shell_in` / `shell_candidates_in`；PATH、PATHEXT、COMSPEC、ProgramFiles 全部
  作为入参，测试用临时目录拼环境，不改进程环境）。追加的 PATHEXT 扩展名规范为小写：
  Windows 匹配本来不分大小写，但返回的路径该读作 `pwsh.exe`，不是 PATHEXT 的大写拼写。
- **`local.shell` 此前只改 targets 视图的标签，从不影响真正拉起的程序**——保存 "Git Bash"
  仍会开默认 shell。本次在 `TerminalSessions::open` 的本地分支补上「请求级 `shell`
  覆盖 > 配置 `local.shell` > 平台默认」的顺序。spec §2.2 的改动清单没有这一条，但不补
  上，设置 sheet 的 Shell 输入框就是装饰；偏差记录于此。

**实施后验证**（debug 构建，`127.0.0.1:19998`，scratch home）：插件清页（order 70）、
vendored 资源与 content-type、targets 诚实空态、真 ConPTY 本地会话经 WS 字节回环、列表
attached/bytesOut、asciicast v2 落盘可读、DELETE 后清表 —— 11/11 通过；强杀网关后
cmd 子进程 0 残留（§10 第 8 条）。

**真主机补测**（2026-09-11，release `3c3fd7f`，开发机/构建机）：§10 第 3 条三项机器验证
通过——中文+`✓` 回显字节完整、`vim` 打开（~ 波浪线行）后 `:q!` 干净退出、`htop` 运行后 `q` 退出；
另经 Chrome（CDP 键盘注入）在面板里开真实远端会话，输入 `echo 面板-中文-直通OK` 回显字形正确。
每远端会话 RSS 边际成本 (4−1)/3 = 250.7 KB（ADR-011），在 ≤ 256 KB 预算行内。

**§10 逐条机器复验**（2026-09-11，release 活网关，除注明外均按条目原文验证）：第 1、2 条随
`e6fc64c` 门禁通过；第 4 条 tunnels 停用后 targets 为 `absent` 且文案点名 "the tunnels plugin
is not running"，恢复后两台主机重新 `connected`；第 5 条 terminal 停用后 `/api/terminal/*`
答 503 JSON（含可操作的 re-enable 提示），清单行 `state:disabled`；第 6 条断开 8 秒重连，追赶
缓冲从断开点 L8 连续补到 L60 无缺无重，断开超过宽限的会话从列表消失；第 7 条 `yes` 洪水
5.3 MB/10 秒，网关工作集 22.9→23.2→22.9 MB 无持续增长，Ctrl-C 后 3 秒零字节；第 9 条 cast
文件头合法 asciinema v2（`~/.mcp-gateway/terminal/*.cast`，8.4 MB 正好顶到录制上限）；第 10 条
见 §7 实测列，五行全部在预算内。第 8 条（强杀无孤儿）以 T7 的 scratch-home 验证为准。

## 9. 阶段

### T1 — 能力契约（`lmg-host`，无行为变化）

`crates/lmg-host/src/services/shell.rs`：`ShellTarget` / `PtySession` / `ShellError` /
`ShellProvider` / `ShellRegistry`，形状照 `catalog.rs`；`RuntimeServices` 加 `shells` 字段。
测试：重复注册要吵着失败；withdraw 之后不再发放；drain 超时返回还欠着的数量。
这一阶段没有 provider、没有 consumer，四条门禁必须全绿。

### T2 — SSH provider（`lmg-tunnels`）

`SshLike::open_shell`、`TunnelManager::open_shell` + `ShellSessionGuard`（引用计数走 manager 现有
release 路径），Tunnels 插件在 `start`/`stop` 里注册/撤销 provider。
测试：用 `SshLike` 的假实现（`manager.rs` 已有这个测试形态）断言——开会话后 `refs` 加一、guard drop
后减一并在归零时关闭；provider withdraw 后新会话被拒且错误文案里点名 tunnels 插件。

### T3 — 本地 PTY（`lmg-core` + job object）

`crates/lmg-core/src/platform/pty.rs`（Windows ConPTY / unix openpty），`Cargo.toml` 加
`Win32_System_Console`、`Win32_System_Pipes`；`services/process.rs` 抽出 job 分配函数。
测试（Windows 上跑真的进程）：开一个 `cmd /c echo hi` 的 PTY 读到 `hi`；resize 不报错；
把网关进程强杀之后子进程不残留（这条**用任务管理器人工确认**并写进提交信息，测试测不了）。

### T4 — 会话机（`lmg-terminal` crate）

新 crate，依赖 `lmg-core` + `lmg-host`，**不依赖任何其它子系统 crate**。会话表、票据、上限、
超时、背压、断线宽限、asciicast 写入器。
这一阶段**不接 HTTP**：全部逻辑对着假的 `ShellProvider` 与假的本地 PTY 做单元测试。
测试：票据单次有效 + 10 秒过期 + 绑定会话；超过 `maxSessions` 被拒且文案说明；
idle 超时关闭并写可见原因；发送队列满时读取暂停而**不丢字节**；asciicast 到上限停录并写结束标记。

### T5 — HTTP + WS（`axum/ws`）

`Cargo.toml` 加 `ws` feature，路由按 §8 挂进 `server.rs` 的 `extra` 树（**必须在 `build_app` 的
loopback 守卫之内**，理由见 `src/app.rs` 那段注释），插件工厂放 `src/plugins/terminal.rs`，
在 `server.rs` 里 register。
测试：真路由上的 WS 握手（票据对/错/过期/重放各一条）；插件 disabled 时 `/api/terminal/*` 是那条
结构化 503；`cargo tree -d` 无新增重复。

### T6 — 面板页面（**Node 仓库**）

`../local-mcp-gateway/src/admin/js/vendor/xterm/<version>/…` + `js/views/terminal.js`
（+ 需要的 `js/terminal-*.js`）。§2 的三条约束逐条核对。目标列表放侧边栏还是页内，由实现者定，
但**不要**去改共享 `.sidebar` 的内容归属（那是 `docs/13` §7 明确列为不做的事）。
Node 网关上这一页不出现：终端插件不在它的 `legacy` 清单里，所以不渲染，也不报错。
门禁：Node 仓库 `npx vitest run`。

### T7 — 整树复制回本仓库

```powershell
Remove-Item -Recurse -Force crates\lmg-panel\src\admin_assets
Copy-Item -Recurse ..\local-mcp-gateway\src\admin crates\lmg-panel\src\admin_assets
```

整棵树，不挑文件（ADR-009）。立刻 `cargo test -p lmg-panel`，
`the_tree_is_byte_for_byte_the_node_builds` 必须**通过**而不是 skip。这一提交只有 `admin_assets/`。

### T8 — 实测与文档

- 按 §7 的表逐项量，数字写进 `docs/01`（新增一行终端的条目）与提交信息。
- `docs/02` 加 `lmg-terminal` 到 crate 图（`lmg-core ← lmg-host ← { …, lmg-terminal } ← lmg`）。
- `docs/07` 加一条 ADR：为什么是 WebSocket 而不是 SSE、为什么手写 ConPTY 而不是 `portable-pty`、
  为什么终端不依赖 `lmg-tunnels`。**把量出来的体积与 RSS 写进去**，ADR-010 的规矩在这儿一样成立：
  没测过的收益不许写。
- `README.md` 文档表格里 `docs/14` 那行的状态改掉；`AGENTS.md` 的「Never commit」清单加
  `~/.mcp-gateway/terminal/*.cast`。
- 本文状态行改成「已实施」+ 完成基线提交号。

## 10. 验收

1. 四条门禁全绿，`cargo tree -d` 只多出 `tokio-tungstenite` / `tungstenite`。
2. `cargo test -p lmg-panel the_tree_is_byte_for_byte` 通过（不是 skip）。
3. 浏览器里：对一台已配好的隧道主机开会话 → 能登录、能 `vim`、能 `htop`、中文与 emoji 不错位、
   拖窗口改大小后远端 `stty size` 跟着变。
4. 关掉 tunnels 插件 → 远端目标列表变空且**文案点名 tunnels 插件**；本地会话（若已启用）不受影响。
5. 关掉 terminal 插件 → `/api/terminal/*` 全是结构化 503，一级导航那格带 `· off`。
6. 拔网线/断开 WS 60 秒内重连 → 会话还在，缺的输出补上；超过宽限 → 会话已关闭且列表里没有它。
7. 在会话里跑 `yes` 十秒再 Ctrl-C → 进程 RSS 不持续增长（背压生效），终端显示不错位（没丢字节）。
8. 强杀 `lmg.exe` → 任务管理器里没有残留的 shell 子进程（ADR-008）。
9. `asciinema play ~/.mcp-gateway/terminal/<id>.cast` 能回放。
10. §7 的每一行都有实测数字，且都在预算内 —— 否则按 §11 处理，不要「先合了再说」。

## 11. 不做什么，以及退路

- **不做多租户 / RBAC / 账号金库 / 工单审批。** 见 §1。
- **不做命令黑名单。** 见 §6.5。
- **不做 SFTP / 文件管理器。** 它是另一个插件，另一份规范；混进来会让这份规范做不完。
- **不做会话共享 / 协同观看。** 单用户回环进程里，它的唯一用途是演示。
- **不改 `mime_of` 去支持字体或 wasm。** 那会把面板的资源管道从「只能是文本」放宽成「什么都行」，
  而这条限制正是它今天可以整树字节比对的原因之一。
- **不给 `PageDescriptor` 加字段**，也不改 `/api/plugins` 的形状。
- **退路**：若 §7 的实测超预算，或 ConPTY 的 FFI 明显失控（超过约 300 行、或在多次调试后仍
  不稳定），允许二选一 —— (a) 整个插件挪到一个 cargo feature 后面；(b) 本地会话改用
  `portable-pty`，远端保持自研。**无论选哪个都要在 `docs/07` 写一条 ADR 说明为什么**，
  不许悄悄换掉本文的决定。
