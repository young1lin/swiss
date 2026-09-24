# 16 — 运维加固：守护进程的环境、隔离的测试实例、一步部署、CI，与依赖体检

> 状态：**H1–H5 已实施**（2026-09-12；提交 03823c3 / 83008c4 / 8f6b7af / H5 见 git log，H4 经核已由
> 既有 `.github/workflows/build.yml` 全覆盖、未另建 ci.yml）。H6（可选）另行处理。本文只写清
> 「是什么、为什么、改哪里、怎么验」，不含实现代码。
> 前置阅读：`AGENTS.md`（规则高于本文）、`docs/05-wire-compatibility.md`（状态目录与密钥，H2 要用）、
> `docs/14-terminal-plugin-spec.md` §5 末尾「本地 shell 的环境」（H1 的前情）。
> 代码注释一律英文；文档散文中文。本仓库的 `crates/swiss-panel/src/admin_assets/` 一个字节都不能手改
> （本文 H6 之外的条目都不碰面板）。

## 0. 为什么有这份文档

2026-09-11 一天里踩到的坑，每一个都不是某个功能的 bug，而是**运行方式**留下的洞：

| 现象 | 根因 | 这份文档的条目 |
|---|---|---|
| 19999 上的本地 PowerShell 7 黑白无色 | 部署 19999 的那个 agent 工具 shell 带着 `NO_COLOR=1`；`swiss start` 把启动者的整个环境原样交给守护进程，守护进程再原样交给每个子进程。终端插件已在 `local.rs::shell_command` 单点堵住（提交 `fcf012c`），但 jobs 的子进程、MCP 的 stdio 子进程仍然全盘继承 | H1 |
| 在 19998 上保存 terminal 配置，写进了生产的 `gateway.config.json` | 两个实例共用 `~/.swiss`。`SWISS_HOME` 早就存在（`crates/swiss-core/src/paths.rs::data_dir`），只是没有一条把它用于 19998 的规矩和脚本 | H2 |
| 「19999 跑的还是旧二进制」被误以为已部署；`cargo build` 报 `Access is denied (os error 5)` | 部署是三步手工仪式（stop → build → start），顺序错一步就失败；而且没有任何地方能看出**正在运行的**二进制是哪次构建 | H3 |
| 门禁全靠人跑 | Rust 仓库没有 CI；Node 仓库有三个 workflow | H4 |
| `cargo tree -d` 里 RustCrypto 整条栈双份 | russh 0.63 拉的是 `aes-gcm 0.11` / `digest 0.11` / `getrandom 0.3` 这一代，本仓库自己的 sealing 用的是 `aes-gcm 0.10` 一代 | H5 |
| `views/terminal.js` 707 行还在长 | 一个文件里同时住着 xterm 接线、会话机、Local shell 设置表 | H6（可选） |

**已经存在、不用重做**的东西：`SWISS_HOME`（`paths.rs`）、`/health`（`src/app.rs`，
无鉴权，返回 `{"ok":true}`）、`/api/info`（`src/adminapi.rs`，返回 `tokenEnv` 与 `panelVersion`）、
`swiss start / stop / status / logs / token / --version`（`src/cli.rs`、`src/daemon.rs`）、
pid 文件与日志都在 `data_dir()` 下按端口命名（`src/pidfile.rs`）。

## 1. H1 — 守护进程的环境：启动者的口味不是网关的口味

### 1.1 目标行为

网关是守护进程，它的环境应当是「这台机器上这个用户的环境」，而不是「碰巧敲了 `swiss start` 的那个
shell 的环境」。具体地：

- 一组**明确列出**的变量在网关进程里不存在，无论启动者带不带：那些告诉程序「你在 agent 里 / 你在
  CI 里 / 别画颜色」的变量。第一版清单（实施时以真实观察为准——先在一个 agent 工具 shell 里
  `Get-ChildItem env:` 看看到底带了什么，再定）：
  - `NO_COLOR`（2026-09-11 实测的元凶）
  - `CI`、`TF_BUILD`、`GITHUB_ACTIONS`（jobs 跑的脚本会据此关掉交互和颜色）
  - `CLAUDECODE`、`CLAUDE_CODE_ENTRYPOINT`，以及所有 `CLAUDE_CODE_` 前缀
  - `WT_SESSION`、`WT_PROFILE_ID`（Windows Terminal 会话标识，子进程据此以为自己在 WT 里）
  - `TERM_PROGRAM`、`TERM_PROGRAM_VERSION`
- **不**碰 `PATH`、`HOME`/`USERPROFILE`、`SystemRoot`、`COMSPEC`、`PATHEXT`、`TEMP`、代理变量、
  `SWISS_*`。这不是白名单，是黑名单：删掉少数几个有毒的，其余原样。
- 两条路径都要覆盖：`swiss start` spawn 出的守护进程（`src/daemon.rs::start_daemon` 的
  `Command`），以及直接 `swiss serve`（19998 就是这么起的，进程内自己清）。
- 清单只写在**一个地方**，每一项旁边一句英文注释说为什么。终端插件里的
  `env_remove("NO_COLOR")` 保留（它是「标签页的环境」这一层的语义，不依赖守护进程是否干净），
  但注释要指向本节。

### 1.2 改哪里

- `crates/swiss-core/src/env.rs`（新建，或放进现有的 `util` 旁边）：`pub const LAUNCHER_NOISE: &[&str]`
  与 `pub fn launcher_noise_prefixes() -> &[&str]`，加一个纯函数
  `fn is_launcher_noise(name: &str) -> bool`（大小写不敏感——Windows 变量名不分大小写）。
- `src/daemon.rs::start_daemon`：对 `command` 逐项 `env_remove`。
- `src/bootstrap.rs`（`serve` 的入口）：进程一启动就 `remove_var` 同一清单——注意 edition 2024
  的 `set_var`/`remove_var` 是 unsafe，必须在任何线程起来之前做，仿照
  `bootstrap.rs:231` 现有写法。
- `docs/14` §5 那段「本地 shell 的环境」补一句：守护进程层已按 docs/16 §1 清过。

### 1.3 测试

- `is_launcher_noise`：`NO_COLOR` / `no_color` / `CLAUDE_CODE_FOO` 为真；`PATH` / `NO_COLORS_PLEASE`
  / `CIRCLE` 为假（前缀匹配只对明确的前缀项生效，不是「以 CI 开头」）。
- `start_daemon` 已有测试基建（`src/daemon.rs` tests 里用假 entry 启动）。加一条：种上
  `NO_COLOR=1` 后 `start` 一个把自己环境 dump 到文件的假 entry（或复用现有的假 entry 写法），
  断言 dump 里没有 `NO_COLOR`，有 `PATH`。
- 现有 `crates/swiss-terminal` 的 `a_local_pwsh_keeps_its_colours_under_a_no_color_launcher`
  继续保留，不动。

## 2. H2 — 19998 用自己的家：一个脚本，一条规矩

### 2.1 目标行为

- 一个 `scripts/test-instance.ps1`：
  - `-Start`（默认）：把 `~/.swiss` 里的状态**拷贝**（不是链接）到测试家
    `$env:LOCALAPPDATA\swiss-test-home\`（固定路径，便于 `swiss logs` 之类手工查看）——
    拷 `master.key`、`gateway.config.json`、`managed.json`、`tunnels.json`、`jobs.json`、
    `jobs-state.json`、`env.json`；**不拷** `gateway-*.pid`、`gateway-*.log`、`terminal/`。
    然后 `$env:SWISS_HOME = <测试家>; $env:SWISS_PORT = "19998"`，
    `Start-Process target-test\release\swiss.exe serve`（隐藏窗口，stdout/stderr 落到测试家的
    `serve.out` / `serve.err`），等 `/health` 200 或 20 秒超时后报告 pid。
  - `-Stop`：只按 `Get-NetTCPConnection -LocalPort 19998` 的 OwningProcess 杀，找不到就说没在跑。
    **绝不** `Get-Process swiss`。
  - `-Fresh`：先删测试家再 `-Start`（要一个干净实例时用）。
  - 脚本开头一段注释写清：DPAPI 保护的 `master.key` 同机同用户可用，所以拷贝有效；跨机器无效
    （docs/05 §「The master key」）。
- 拷贝是单向快照：19998 上的任何保存只改测试家。AGENTS.md「Live testing ports」一节改写：
  用脚本起停，删掉「keep it read-only」和「验收完记得恢复配置」这两条——它们不再需要。
- `docs/15-terminal-paste-and-local-shell-prompt.md` 里「两个实例共用配置」那段加一行
  「自 docs/16 起改用 `scripts/test-instance.ps1`」，不删原文（它是历史）。

### 2.2 测试

PowerShell 脚本没有单元测试；验收在 §7。但 Rust 侧要确认一件事并写成测试（如果还没有）：
`data_dir()` 在 `SWISS_HOME` 指向不存在的目录时，`serve` 会创建它而不是 panic
（`start_daemon` 里有 `create_dir_all(data_dir())`，`serve` 路径核对一下）。

## 3. H3 — 一步部署，以及「正在跑的是哪次构建」

### 3.1 目标行为

- 二进制知道自己是哪次构建：`build.rs`（根 crate `swiss`）在编译时取 `git rev-parse --short HEAD`
  与 `git status --porcelain` 是否为空，输出 `SWISS_GIT_HASH`（如 `fcf012c` / `23047d8-dirty`）和
  `SWISS_BUILD_TIME`（RFC 3339，UTC）。没有 git 或不在仓库里时两者为 `unknown`，**构建不能失败**。
  `rerun-if-changed` 指向 `.git/HEAD` 与 `.git/refs/heads`。不加任何 crate 依赖。
- `swiss --version` 打印 `swiss 0.1.0 (fcf012c, 2026-09-11T13:16:40Z)`。
- `/health` 加 `build: { "hash": "...", "time": "..." }`（无鉴权路由；loopback 上暴露短 hash 可以
  接受——写进注释）。`/api/info` 同样加 `build`。
- `swiss status` 多打一行：`build: fcf012c (2026-09-11T13:16:40Z)`；并且把**磁盘上 entry 的构建**
  （对 pid 文件里的 `entry` 跑 `--version`）和**运行中的构建**（`/health` 的 `build`）比一下，
  不一致时多一行 `note: the binary on disk is 9f1c2ab; the running daemon is fcf012c — restart to pick it up`。
  JSON 输出（`status_json`）同样带 `build` 与 `diskBuild`。
- `scripts/deploy.ps1`：
  1. `cargo test --workspace` 与 `cargo clippy --workspace --all-targets -- -D warnings`（可用
     `-SkipGates` 跳过，默认不跳）；
  2. `target\release\swiss.exe stop`（没在跑也继续）；
  3. `cargo build --release`（**在 stop 之后**——这就是 `os error 5` 的答案）；
  4. `target\release\swiss.exe start --no-open`；
  5. `target\release\swiss.exe status`，并断言 `/health` 的 `build.hash` 等于刚构建的
     `swiss.exe --version` 报的 hash，不等就非零退出并打印两者。
  脚本必须从**用户自己的**终端跑（H1 落地前从 agent shell 跑会把噪音带进去；落地后也仍然建议）。
  README 的 Getting started 补一行「部署用 `scripts/deploy.ps1`」。

### 3.2 测试

- `build.rs` 的产物用 `env!("SWISS_GIT_HASH")` 取；测试断言它非空且要么是 `unknown`，要么匹配
  `^[0-9a-f]{7,}(-dirty)?$`。
- `/health` 与 `/api/info` 的现有 oneshot 测试加断言 `build.hash` 存在。
- `daemon_status` 的现有测试基建里加一条：假 entry 的 `--version` 报一个 hash，`/health` 报另一个，
  `status` 的结果里 `note` 非空。

### 3.3 2026-09-21 追记：生产从 `bin\swiss.exe` 跑，不再从 `target\` 跑

owner 的要求："创建一个 bin 目录，每次部署的时候，拷贝到 bin 目录下，启动，并且 git ignore bin
目录下的文件……不要放到 target 目录启动了……我可以设置环境变量，这样可以在其他的应用中使用
swiss remote 功能"。

- `scripts/deploy.ps1` 的顺序变为：门禁 → `cargo build --release`（**老守护进程还在服务**，因为它
  持有的是 `bin\swiss.exe`，链接器写的是 `target\release\swiss.exe`，两者不再是同一个文件）→
  stop → `Copy-Item target\release\swiss.exe bin\swiss.exe`（进程释放句柄可能慢半拍，拷贝重试
  20 次 × 250 ms）→ `bin\swiss.exe start --no-open` → status → 3.1 的 hash 证明（对 `bin\swiss.exe
  --version` 与 `/health`）。停机窗口从一次 release 构建（约 4 分钟）缩到 stop + copy + start（几秒）。
- 一次性迁移：pid 文件里 `entry` 还是 `target\release\swiss.exe` 的守护进程（旧布局）在构建**前**
  停掉——这一次链接器确实要覆盖它持有的文件；之后的部署都走上面的顺序。
- `bin/` 进 `.gitignore`：它是构建产物。
- `bin\swiss.exe start` / `stop` / `status` 从任何目录都能用：`start` 起的是 `current_exe()`（即
  `bin\swiss.exe`），`stop` / `status` 靠 home 里的 pid 文件；`swiss autostart on` 若打开，注册的
  也是 `bin\` 这个稳定路径。
- PATH 由 owner 自己设：脚本只在 `bin\` 不在 PATH 上时打印一行怎么加（改用户的 PATH 不是部署
  脚本该做的事）。加上之后任何程序都能 `swiss remote exec <target> -- ...`（CLI 从 home 读 token，
  与 19999 同一用户即可）。
- 证据（2026-09-21，commit 010b7d1）：
  - 迁移那次：pid 文件 `entry` 是 `target\release\swiss.exe`，脚本在构建前停了它（阶段行
    "stopping the daemon first: it runs out of target\release\swiss.exe, which the build must
    overwrite"），装到 `bin\swiss.exe` 后 `/health` 与 `bin\swiss.exe --version` 同为 `010b7d1`，
    pid 文件 `entry` 变为 `bin\swiss.exe`。
  - 再部署一次（同一提交，全部门禁）：阶段顺序 build → stop → install → start，构建期间老进程
    一直在服务；两个 200 ms 一次的 `/health` 探针都量到 **3.1 s** 停机（09:45:56.1 → 09:45:59.2 UTC）。
  - 从 `C:\Users\<user>` 直接跑 `bin\swiss.exe status`、`bin\swiss.exe remote targets` 都能到 19999。

增补（2026-09-22，`929af06`）：上面顺序里的「门禁」块自此在单元门与 clippy 之间多了一条门
`cargo test -p swiss-it --features it`（真库集成，docs/44；脚本为这一次运行显式设
`$env:DOCKER_HOST = 'tcp://127.0.0.1:2375'`、跑完即删，非零退出同样 production left untouched，
`-SkipGates` 连同其它门一并跳过）。形状见 docs/44 §2.8；同提交还给
`.github/workflows/build.yml` 加了 ubuntu 的 `integration` job，`release` 需要它。

## 4. H4 — Rust 仓库的 CI

### 4.1 目标行为

- `.github/workflows/ci.yml`：`push` 到 `main` 与 `pull_request` 触发；`windows-latest` 一个 job：
  `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`，环境里
  `CI=1`（`the_tree_is_byte_for_byte_the_node_builds` 看见它才允许在没有 sibling 的机器上跳过——
  `crates/swiss-panel/src/admin.rs` 已经这么写了）。用 `Swatinem/rust-cache` 缓存（这是 action，
  不是 crate 依赖）。
- **不加 `cargo fmt --check`**：2026-09-11 `cargo fmt --check` 有 51 处 diff，仓库从未以 rustfmt
  为准。要不要引入 rustfmt 是另一份 ADR 的事，不在这里顺手做。
- `ubuntu-latest` 的 job **第二步再加**，且只有在本地（或 WSL）验证过 `cargo test --workspace`
  在 Linux 上真的绿之后；不绿就不加，并在 PR/提交信息里写明卡在哪。Windows 才是这个项目的
  第一平台（docs/01）。
- Windows runner 上有 `pwsh`，所以 `a_local_pwsh_keeps_its_colours_under_a_no_color_launcher`
  会真跑；它自己在找不到 pwsh 时跳过，不用改。

### 4.2 验收

workflow 文件本地无法运行；验收是第一次推送后的绿勾。提交信息里贴 run 的 URL。

## 5. H5 — RustCrypto 双份栈：查清楚，能合就合，不能合就写下来

### 5.1 目标行为

- 先 `cargo tree -d -e normal` 把双份的每一对（`aead`、`aes`、`aes-gcm`、`cipher`、`digest`、
  `block-buffer`、`crypto-common`、`generic-array`、`getrandom` ×3、`const-oid`、`der`、`base64`）
  各自的引入路径列出来，写进本节的实施记录。
- 本仓库**自己**直接依赖的那一侧（sealing 用的 `aes-gcm`、`sha2`、`hmac`、`rand`…）尝试升到
  russh 0.63 拉的那一代。ADR 里的 sealing 格式（docs/05）是字节级契约：升级后
  `scripts/seal-fixture.mts` 生成的 Node 侧夹具必须原样能开——现有的 envelope 往返测试就是门禁。
- 合不掉的（比如 `getrandom 0.2` 是某个传递依赖钉死的），写一段 ADR-013 到 `docs/07-decisions.md`：
  哪几对、谁钉的、体积代价多少（`target\release\swiss.exe` 升级前后各量一次，写进去）。
- 硬约束：`cargo tree -d` 里**不能新增**任何一对；TLS 栈与运行时保持单份（AGENTS.md 原话）。

### 5.2 测试

现有全部测试 + `seal-fixture` 往返。没有新测试——这一条是依赖体检，不是行为变化。

### 5.3 实施记录（2026-09-12）

已合：本仓四个 crate 的直接依赖全部升到 russh 0.63 那一代（aes-gcm 0.11、sha2 0.11、hkdf 0.13、
rand 0.9），九对重复（aead、aes、aes-gcm、cipher、ctr、ghash、polyval、universal-hash、inout）
整体消失；`opens_a_node_sealed_fixture` 在新栈上原样通过（冻结格式无损）。体积
7,972,352 → 7,966,208 字节。合不掉的一侧全部查明了归属（sqlx 0.8.6 钉 sha2/hmac/hkdf/rand 0.8
一代，axum 0.8.9 钉 base64 0.22，pageant/process-wrap 钉 windows 0.62，syn 3 仅 proc-macro），
逐条写进 **ADR-013**（`docs/07-decisions.md`）。未新增任何一对；TLS 栈与运行时单份。

## 6. H6（可选）— `views/terminal.js` 拆出设置表

只在 H1–H5 都交付之后再做，做不做都不算失败。把 `openLocalSheet` / `saveLocalSheet` 移到
`../node-original/src/admin/js/views/terminal-settings.js`，`terminal.js` 只 import 一个
`openLocalSheet`。纯函数不动（都在 `terminal-core.js`），测试不动。改完整树复制回
`admin_assets/`，面板文件保持 CRLF。

## 7. 顺序、提交与验收

每一条一个提交，顺序 H1 → H2 → H3 → H4 → H5 →（H6）。每个提交自己过门禁：
`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`；碰了面板的
（只有 H6）再加 Node 仓库的 `npm run typecheck` 与 `npx vitest run`。

验收全部在 19998 上，**19999 一根手指都不碰**（H3 的 `deploy.ps1` 只写、只在假 entry 上测，
不对 19999 跑——部署由用户自己做）：

- H1：从一个 `$env:NO_COLOR = "1"` 的 PowerShell 里用 `scripts/test-instance.ps1 -Start`（H2）起
  19998，开一个本地终端跑 `$env:NO_COLOR; $env:CLAUDECODE; $PSStyle.OutputRendering`，前两个空、
  第三个 `Host`。再跑一个 job（任意一个已有的、会打印环境的 action，没有就临时建一个再删），
  输出里没有 `NO_COLOR`。
- H2：在 19998 的终端页保存一次 Local shell 设置，然后 `Get-Item ~/.swiss/gateway.config.json`
  的 `LastWriteTime` 没变，测试家的变了。`-Stop` 之后 19999 的 `/health` 仍是 200。
- H3：`target-test\release\swiss.exe --version` 打印 hash；19998 的 `/health` 里有同一个 hash；
  `swiss status`（带 `SWISS_HOME` 指向测试家、端口 19998）打印 `build:` 行。
- H4：绿勾的 URL。
- H5：`cargo tree -d` 前后对比与二进制大小前后对比，贴进提交信息。

## 8. 不做什么

- 不做白名单式的环境重建（那会在某台机器上删掉一个没人想到的变量，比如代理）。
- 不给 `serve` 加 `--home` 参数——环境变量已经够用，多一个参数就多一个「写进配置」的坑
  （`--port` 的教训，AGENTS.md）。
- 不引入 rustfmt、不引入 `cargo-nextest`、不加任何 crate 依赖（H3 的 `build.rs` 用 `std::process::Command`
  调 git 就够）。
- 不重构 `tunnel/manager.rs` / `jobs/mod.rs` / `dbbrowser.rs` 这几个大文件——它们大，但都有整套测试
  罩着，拆分的风险大于收益，另立文档再议。
