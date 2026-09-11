# 交给实施模型的 Prompt

> 复制下面整段给实施模型。它假设模型在 `<repo>` 目录下工作，
> 旁边有 `..\local-mcp-gateway`（Node 仓库），并且能用 `.agents/skills/agent-browser` 操作浏览器。

---

你在两个相邻仓库里工作：

- `<repo>` — Rust 版网关（cargo workspace，产物是单个 `lmg.exe`）。
- `<node-repo>` — Node/TypeScript 版，它的 `src/admin/` 是两边共用的
  面板源码。Rust 仓库里的 `crates/lmg-panel/src/admin_assets/` 是它的逐字节拷贝，**只能复制，不能手改**；
  测试 `the_tree_is_byte_for_byte_the_node_builds` 会拦住任何分叉。

任务：实施 `docs/15-terminal-paste-and-local-shell-spec.md`。先完整读完这份 spec，再读它开头列的
前置文档，尤其是 `AGENTS.md` —— 它的规则高于 spec 和这段话里的任何便利。

**动手前先读这些文件，理解现状再改**（spec §0 已经把根因和证据写清了，不要重新猜）：

- `../local-mcp-gateway/src/admin/js/views/terminal.js`（`wireTerminal`、`sendInput`、`openSession`）
- `../local-mcp-gateway/src/admin/js/terminal-core.js`（纯函数层，`targetRows`）
- `../local-mcp-gateway/test/admin-terminal.test.ts`
- `crates/lmg-core/src/platform/pty/conpty.rs`（`default_shell`、`command_line`）
- `crates/lmg-terminal/src/terminal/local.rs`、`config.rs`
- `src/plugins/terminal.rs`、`src/plugins/terminal_api.rs`
- `crates/lmg-host/src/host/api.rs` 的 `put_config`
- `../local-mcp-gateway/src/admin/js/views/plugins.js`（确认它确实没有配置表单）

**交付**（spec §3 的两个提交，顺序不能反）：

1. P1 粘贴/复制键位 —— 只改 Node 仓库面板，再复制回 Rust 仓库。
2. P2 本地 shell 默认 pwsh + 终端页可开关 —— Rust + Node + 复制。

每个提交的硬性要求：

- 行为变化先有测试（spec §1.3 / §2.3 列了最少集合）。
- 门禁全绿：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`；
  Node 仓库 `npm run typecheck` 与 `npx vitest run`。
- 代码注释英文，风格跟周围一致（这两个仓库的注释都在解释「为什么」，不是复述代码）。
- 面板文件在 git 索引里是 CRLF，写文件时保持 CRLF，否则整文件 diff。
- 不提交 `gateway.config.json`、`.env`、`managed.json`、`tunnels.json`、`master.key`、`*.log`、
  `~/.mcp-gateway/terminal/*.cast`。
- Commit message 末尾按 `AGENTS.md` 的 attribution 规则。

**运行与验证的铁律**（AGENTS.md 有，这里再说一遍因为踩过坑）：

- 19999 是生产实例，**不要停、不要重启、不要部署**。所有实测都在 19998：
  `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`，然后
  `$env:MCP_GATEWAY_PORT = "19998"; & target-test\release\lmg.exe serve`。永远用环境变量，
  不要 `--port`（它会写进配置）。
- 停 19998 只按端口找 PID：`Get-NetTCPConnection -LocalPort 19998`。绝不 `Get-Process lmg`。
- 两个实例共用 `~/.mcp-gateway/gateway.config.json`。spec §4.2 在 19998 上保存 terminal 配置会写进
  生产配置——验收完把 `local.enabled` 恢复成用户要的值（用户要的是**打开、pwsh**），并在汇报里写明
  你最终留下的是什么。（自 docs/16 H2 起这一条作废：19998 用 `scripts/test-instance.ps1` 起在自己的
  测试家 `%LOCALAPPDATA%\lmg-test-home`，保存只写测试家。原文保留作历史。）
- 浏览器实测用 `.agents/skills/agent-browser`（先 `agent-browser skills get core`）。这台机器上它
  冷启动慢，每条命令前加 `timeout`，只开一个命名会话，结束 `agent-browser close`。
  粘贴的验证手段：`press Control+v` 后截图看有没有 `^V`；对 `.xterm-helper-textarea` 派发合成
  `ClipboardEvent('paste')` 看文本是否到达 shell。

**不要做**：改 vendored 的 xterm 文件；给 Plugins 页做通用 schema 表单；加新的 crate 依赖
（spec 里的改动一个都不需要）；把 `default_shell` 的探测放进每次 HTTP 请求。

**汇报格式**：两个提交的 hash；每个门禁的实际输出末尾几行（不是「通过了」三个字）；19998 上
验收 §4.1 / §4.2 的截图或终端文本；你留在生产配置里的 terminal 配置最终值；任何与 spec 的偏差
及理由——偏差不是问题，隐瞒才是。
