# 15 — 终端页两处缺口：粘贴，与本地 PowerShell 7

> 状态：**已实施**（2026-09-12）。P1：Node `0cd1090`、本仓 mirror `4bc1f1a`；P2 为本仓
> 「feat(terminal): local shell defaults to pwsh」提交（Rust + Node + mirror）。实施偏差记在
> docs/14 §8.1。本文只写清「是什么、为什么、改哪里、怎么验」，不含实现代码。
> 前置阅读：`AGENTS.md`（规则高于本文）、`docs/14-terminal-plugin-spec.md`（终端插件的整体设计，
> 本文是它的补丁，不重述）、`docs/09-toolbox-plugin-architecture.md` §3/§4（插件配置契约）。
> **本仓库的 `crates/swiss-panel/src/admin_assets/` 一个字节都不能手改。** 面板改动先落在
> `../node-original/src/admin/`，再整目录复制回来，`the_tree_is_byte_for_byte_the_node_builds`
> 测试是门禁。代码注释一律英文；文档散文中文。

## 0. 结论先行（2026-09-11 在 19998 上实测）

两个问题都不是「坏了」，是「按 xterm.js / 插件的默认值在工作，而默认值不是 Windows 用户要的」。

| 现象 | 根因 | 证据 |
|---|---|---|
| 终端里 **Ctrl+V 粘贴无效** | xterm.js 把 Ctrl+V 当成终端控制键 `^V`（0x16，tty 的 lnext）发给 shell；它只给 Ctrl+Shift+V / Shift+Insert 留粘贴。面板没有装 `attachCustomKeyEventHandler`，所以拿到的是 xterm 的 Linux 习惯 | 在会话里按 Ctrl+V，屏幕出现 `^V`；对 `.xterm-helper-textarea` 派发合成 `paste` 事件，文本正常到达远端 bash（括号粘贴高亮）—— 说明 paste 管道完好，坏的只是按键映射 |
| **连不上本地 PowerShell 7** | ① 本地 shell 默认关闭（`plugins.terminal.config.local.enabled = false`，docs/14 §6.1 有意为之）；② docs/14 §6.1 承诺「面板的插件配置页自动能改」，但 Plugins 页至今只有 Enable/Disable，**没有任何配置表单**，而 `gateway.config.json` 是加密的，用户没有手改的路；③ 即使打开，Windows 默认程序是 `COMSPEC`（`cmd.exe`），不是 pwsh | `GET /api/terminal/targets` → `{"local":{"enabled":false,"shell":"C:\\WINDOWS\\system32\\cmd.exe"}}`；`views/plugins.js` 只有 `toggle()`；`crates/swiss-core/src/platform/pty/conpty.rs::default_shell()` 读 `COMSPEC` |

已经存在、**不用重做**的东西：

- 本地 PTY 全链路（ConPTY、job object、读线程、背压）—— `crates/swiss-core/src/platform/pty/`、
  `crates/swiss-terminal/src/terminal/local.rs`。`local.rs` 的测试已经证明默认 shell 能开、能收字节。
- 配置解析 `TerminalConfig::parse`（`crates/swiss-terminal/src/terminal/config.rs`）已接受
  `local.enabled` / `local.shell`，且有 `"shell": "pwsh.exe"` 的测试用例。
- `PUT /api/plugins/terminal/config`（`crates/swiss-host/src/host/api.rs::put_config`）：body
  `{ "config": {...}, "revision": n }`，校验后落盘；terminal 的 descriptor 标了
  `restart_on_config_change: true`，所以保存即重启插件——**会关闭所有活动会话**，面板会收到
  带原因的关闭帧。
- `POST /api/terminal/sessions` 已接受可选 `shell` 字段做单次覆盖（`src/plugins/terminal_api.rs`）。
- 本机 `pwsh.exe` 在 PATH 上（Store 版，`%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe` 执行别名），
  `CreateProcessW(lpApplicationName = NULL)` 走 PATH 解析，裸名可用。

## 1. P1 — 粘贴与复制：给终端 Windows Terminal 的键位

### 1.1 目标行为

| 动作 | 无选区 | 有选区 |
|---|---|---|
| Ctrl+V / Ctrl+Shift+V / Shift+Insert | 粘贴 | 粘贴（替换不了选区，终端没有这个概念） |
| Ctrl+C | 发 `^C`（SIGINT，**不能变**） | 复制选区，并清除选区（Windows Terminal 语义）；不发 `^C` |
| Ctrl+Shift+C / Ctrl+Insert | 无事 | 复制 |
| 右键 | 粘贴剪贴板 | 复制选区并清除选区 |
| Shift+右键 | 浏览器原生菜单（逃生口） | 同左 |
| 多行粘贴 | 原样走 xterm 的括号粘贴（远端 bash / PSReadLine 都认），**不要**自己做 `\n → \r` 之类的改写 | |

不做：中键粘贴（X11 习惯）、粘贴前确认弹窗（多行粘贴警告是另一个特性，不在本文）。

### 1.2 改哪里

全部在 Node 仓库 `src/admin/js/`，Rust 侧零改动。

1. **纯逻辑进 `terminal-core.js`**（这个文件的约定就是「无 DOM、可单测」）：
   - `export function keyAction(ev, hasSelection)` → `"paste" | "copy" | "sigint" | null`。
     输入是 `{ key, code, ctrlKey, shiftKey, altKey, metaKey, type }` 的鸭子类型，不要接
     `KeyboardEvent` 实例，测试才能构造。只在 `type === "keydown"` 上判定；`keyup`/`keypress`
     一律 `null`（xterm 会把三种事件都递给 custom handler）。
   - `export function mouseAction(ev, hasSelection)` → `"paste" | "copy" | "menu" | null`，
     只看 `button === 2` 和 `shiftKey`。
2. **接线在 `views/terminal.js::wireTerminal`**，紧跟 `term.onData(...)`：
   - `term.attachCustomKeyEventHandler(function (ev) { ... })`：
     - `"paste"` → **`return false`，不要 `preventDefault`**。xterm 拿到 `false` 就跳过自己的
       处理（不会再产出 `^V`），浏览器随后对聚焦的 `.xterm-helper-textarea` 触发原生 `paste`
       事件，xterm 已有的 paste 监听会把文本经 `onData` 送上 WebSocket。已验证这条管道是通的。
       **不要**用 `navigator.clipboard.readText()` 走键盘粘贴——原生事件不需要权限提示。
     - `"copy"` → `navigator.clipboard.writeText(term.getSelection())`，然后
       `term.clearSelection()`，`return false`。`writeText` 在 `http://127.0.0.1` 这类
       potentially-trustworthy origin 上可用，失败（极少）时 toast 一句，不静默。
     - 其余 `return true`，xterm 照常。
   - 右键：在 `holder` 上监听 `contextmenu`：`"paste"` → `ev.preventDefault()`，
     `navigator.clipboard.readText().then(text => term.paste(text))`——`term.paste()` 是 xterm
     的公开 API，自动走括号粘贴。`readText` 首次会弹一次浏览器权限提示，被拒时 toast
     「剪贴板读取被浏览器拒绝，用 Ctrl+V」。`"copy"` 同上。`"menu"` 不 `preventDefault`。
   - 焦点：这些动作之后都要 `term.focus()`，否则右键粘贴完键盘输入丢到页面上。
3. **不要**改 vendored 的 xterm 文件（docs/14 §2 的三条硬约束）；不要加 `rightClickSelectsWord`。

### 1.3 测试

- vitest `test/admin-terminal.test.ts`（已存在，加用例）：`keyAction` 覆盖上表每一行，外加
  Alt+V / Cmd+V(mac 不管) / keyup 三个反例；`mouseAction` 四种组合。
- 浏览器实测（agent-browser，19998，见 §4）：开一个会话，`press Control+v` 后屏幕**不再出现
  `^V`**；对 textarea 派发合成 paste 事件仍然到达 shell；`press Control+c` 在无选区时 shell 收到
  `^C`（提示符换行）。

## 2. P2 — 本地 shell：默认 pwsh，面板上能开

### 2.1 目标行为

1. Windows 上本地 shell 的**默认程序**顺序：`pwsh.exe`（PATH 可解析）→ `powershell.exe` →
   `COMSPEC` → `cmd.exe`。unix 不变（`$SHELL` → `/bin/sh`）。`GET /api/terminal/targets` 的
   `local.shell` 报**解析后的绝对路径**，面板的 `local · …` 标签才是真话。
2. `GET /api/terminal/targets` 的 `local` 多一个字段 `shells: [{ "program": "<abs path>",
   "label": "PowerShell 7" | "Windows PowerShell" | "cmd" | "Git Bash" | "<basename>" }]`：本机
   探测到的候选。探测只看 PATH 加固定几处（`%ProgramFiles%\PowerShell\7\pwsh.exe`、
   `%ProgramFiles%\Git\bin\bash.exe`），**不读注册表**。探测在插件 start 时做一次并缓存，
   不在每次 GET 时跑文件系统。
3. **终端页**加一个「Local shell」设置入口（目标下拉旁一个小齿轮，或下拉里最后一行
   「Local shell settings…」），打开一个 sheet：
   - `Enabled` 开关（默认关，旁边一行 docs/14 §6.1 的理由，一句话）。
   - `Shell` 输入框，带候选下拉（来自 `local.shells`），可手填任意路径。
   - 保存 = `PUT /api/plugins/terminal/config`，body 里**只改 `local`，其余键原样回传**
     （先 `GET /api/plugins/terminal/config` 拿当前 config + revision）。
   - 保存前若有活动会话，`confirm("Saving restarts the terminal plugin and closes N sessions.")`。
     这是 `restart_on_config_change` 的诚实代价，不要绕。
   - 保存后重新拉 targets，本地行出现在下拉里并选中。
4. 目标下拉的本地行标签：`local · PowerShell 7`（用 `shells` 里匹配到的 label），匹配不到就
   basename。
5. 本地 shell 关闭时下拉**不出现**本地行（现状），但空态文案改为可点：「Local shell is off —
   turn it on」直接打开上面的 sheet。

不做：Plugins 页的通用 schema 表单（docs/09 P3 的承诺，单独立项）；每会话选不同 shell 的
UI（API 已支持 `shell` 字段，留给以后）；cwd / env 配置。

### 2.2 改哪里

**Rust**

- `crates/swiss-core/src/platform/pty/conpty.rs::default_shell()`：改成按 §2.1 第 1 条解析。
  把「在 PATH 上找可执行」抽成 `pub fn find_on_path(name: &str, path: &str) -> Option<PathBuf>`
  之类的**纯函数**（PATH 作为参数传入，测试不碰真实环境），注意 `PATHEXT`、执行别名
  （WindowsApps 下的 reparse point：`metadata().is_file()` 为真即可，不要 `canonicalize`，
  它会解析到 `WindowsApps\Microsoft.PowerShell_…` 里去，路径难看且版本一变就失效）。
- `crates/swiss-terminal/src/terminal/local.rs`：`LocalShell` trait 加 `fn candidates(&self)
  -> Vec<ShellCandidate>`；`LocalShells` 在构造时探测一次。测试用的假 shell 返回固定列表。
- `src/plugins/terminal_api.rs` 的 targets 路由：`local` 对象加 `shells`。**保持现有字段不动**
  （面板旧代码读 `local.enabled` / `local.shell`）。
- `src/plugins/terminal.rs` 的 `config_schema`：`local.shell` 的 description 补一句默认顺序。

**Node（面板）**

- `terminal-core.js`：`targetRows()` 用 `local.shells` 给本地行取 label；新增
  `localShellLabel(local)` 纯函数。
- `views/terminal.js`：设置入口 + sheet（复用 `add-sheet.js` / `tunnel-sheets.js` 的 sheet 骨架，
  样式类 `.sheet`、`.fld`、`.two` 都在 `views.css` 里，**不要新写一套**）。
- `styles/views.css`：只在确有需要时加规则；先用现有的。

**文档**

- `docs/14-terminal-plugin-spec.md` §5 末尾加一段「默认 shell 的解析顺序」，§6.1 把「面板的插件
  配置页自动能改」改成指向本文的入口；§8.1 加一条实施偏差记录。
- `README.md` 终端一节：一句话说明怎么打开本地 shell。

### 2.3 测试

- Rust：`find_on_path` 的表驱动测试（有/无 `.exe` 后缀、PATHEXT、第一个命中优先、空 PATH）；
  `default_shell()` 在「PATH 里塞一个假 `pwsh.exe`」的临时目录下返回它（测试内自己拼 PATH 传参，
  不改进程环境）；targets 路由的 JSON 形状测试（`local.shells` 是数组、`local.shell` 是绝对路径）。
- Node：`targetRows` 的 label 用例；sheet 的保存 payload 用例（只改 `local`，revision 透传）。
- 浏览器实测：见 §4。

## 3. 顺序与提交

两个提交，各自独立可回退：

1. `fix(panel): terminal — Ctrl+V pastes, Ctrl+C copies a selection, right-click paste`
   （Node 仓库一个提交 + 本仓库一个「mirror」提交，和以往一样）。
2. `feat(terminal): local shell defaults to pwsh, panel can turn it on`
   （Rust + Node + mirror）。

门禁，每个提交前全过：

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
# Node 仓库
npm run typecheck && npx vitest run
```

## 4. 验收（在 19998 上，19999 一根手指都不碰）

```powershell
# 构建到隔离目录，起测试实例
$env:CARGO_TARGET_DIR = "target-test"; cargo build --release
$env:SWISS_PORT = "19998"; & target-test\release\swiss.exe serve
```

1. **粘贴**：开一个远端会话（或本地），按 Ctrl+V → 剪贴板内容出现在提示符后，**没有** `^V`。
   选中一段屏幕文字按 Ctrl+C → 剪贴板拿到它，shell 没收到 `^C`；不选中按 Ctrl+C → 提示符换行。
   右键 → 粘贴；Shift+右键 → 浏览器菜单。
2. **本地 pwsh**：全新配置（`local` 未设置）下，`GET /api/terminal/targets` 的 `local.shell`
   以 `pwsh.exe` 结尾，`local.shells` 至少含 PowerShell 7 与 cmd。终端页 → Local shell 设置 →
   打开 Enabled → 保存 → 下拉出现 `local · PowerShell 7` → Open session → 看到 `PS C:\…>`
   提示符，`$PSVersionTable.PSVersion` 打出 7.x。关掉 Enabled 保存后本地行消失、空态文案可点。
3. **回归**：`docs/14` §10 的验收项跑一遍（尤其：会话关闭时 `swiss.exe` 的子进程树被回收；
   `~/.swiss/terminal/*.cast` 仍在录）。

注意事项（都是这次踩过的坑）：

- 19998 与 19999 共用 `~/.swiss/gateway.config.json`：在 19998 上保存 terminal 配置**会
  写进生产配置**，19999 下次重启就生效。验收 §4.2 做完要把 `local.enabled` 改回用户想要的值，
  并在汇报里写明。
- 19998 起来后 tunnels 插件会尝试绑同一批本地端口，全部因「被 19999 持有」失败，属正常噪音。
- 杀 19998 只按端口 PID：`Get-NetTCPConnection -LocalPort 19998`，绝不 `Get-Process swiss`。
- agent-browser 在这台机器上冷启动要一两分钟，命令前加 `timeout`，别叠加起多个会话；结束用
  `agent-browser close`。
