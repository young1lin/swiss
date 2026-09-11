# 交给实施模型的 Prompt

> 复制下面整段给实施模型。它假设模型在 `<repo>` 目录下工作，
> 旁边有 `..\local-mcp-gateway`（Node 仓库，只有 H6 会碰）。

---

你在 `<repo>` 工作 —— Rust 版网关，cargo workspace，产物是
单个 `lmg.exe`。旁边的 `<node-repo>` 是 Node 版；它的 `src/admin/`
是两边共用的面板源码，本仓库的 `crates/lmg-panel/src/admin_assets/` 是它的逐字节拷贝，只能整目录
复制、不能手改，测试 `the_tree_is_byte_for_byte_the_node_builds` 会拦住任何分叉。

任务：实施 `docs/16-operations-hardening-spec.md`（六条运维加固：H1 守护进程环境清洗、H2 隔离的
19998 测试家、H3 构建戳与一步部署脚本、H4 Rust CI、H5 RustCrypto 双份栈体检、H6 可选的面板拆分）。
先把这份 spec 读完，再读它开头列的前置文档，尤其 `AGENTS.md` —— 它的规则高于 spec 和这段话。

动手前先读这些文件，理解现状再改（spec §0 已经把根因和证据写清了，不要重新猜）：

- `src/daemon.rs`（`start_daemon`、`daemon_status`、`health_ok`，以及它的 tests 里的假 entry 基建）
- `src/bootstrap.rs`（`serve` 的入口；注意 edition 2024 里 `set_var` / `remove_var` 是 unsafe，
  仿照 231 行附近的现有写法）
- `src/cli.rs`（`--version`、`status_json`）
- `src/pidfile.rs`、`crates/lmg-core/src/paths.rs`（`data_dir` 与 `MCP_GATEWAY_HOME`）
- `src/app.rs` 的 `/health`、`src/adminapi.rs` 的 `/api/info`
- `crates/lmg-terminal/src/terminal/local.rs::shell_command` 及其测试（H1 的前情，保留不动）
- `crates/lmg-host/src/services/process.rs` 与 `crates/lmg-mcp/src/adapters/proc.rs` 里子进程
  怎么建环境（确认它们全盘继承——这就是 H1 存在的理由）
- `docs/05-wire-compatibility.md`「The master key」一节（H2 拷贝 `master.key` 为什么同机有效）
- `crates/lmg-panel/src/admin.rs` 的 `the_tree_is_byte_for_byte_the_node_builds`（H4 靠它的 `CI=1` 口子）
- `../local-mcp-gateway/.github/workflows/ci.yml`（H4 的参照）

交付：**每条一个提交，顺序 H1 → H2 → H3 → H4 → H5 →（H6，可选）**。每个提交的硬性要求：

- 行为变化先有测试（spec 每节的「测试」小节是最少集合），且要证明它在没有修复时会红。
- 门禁全绿：`cargo test --workspace`；`cargo clippy --workspace --all-targets -- -D warnings`。
  `--workspace` 不可省。H6 另加 Node 仓库 `npm run typecheck` 与 `npx vitest run`。
- 不加任何 crate 依赖（`build.rs` 用 `std::process::Command` 调 git）。`cargo tree -d` 不能出现新的
  双份。
- 代码注释英文，解释「为什么」，风格跟周围一致；文档散文中文。
- 不提交 `gateway.config.json`、`.env`、`managed.json`、`tunnels.json`、`master.key`、`*.log`、
  `~/.mcp-gateway/terminal/*.cast`、`$env:LOCALAPPDATA\lmg-test-home\` 里的任何东西。
- Commit message 末尾按 `AGENTS.md` 的 attribution 规则。

运行与验证的铁律：

- **19999 是生产实例，不停、不重启、不部署。** H3 的 `scripts/deploy.ps1` 只写、只用假 entry 测，
  绝不对 19999 执行；部署由用户自己做。
- 所有实测在 19998。H2 落地之前按旧规矩：
  `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`，
  `$env:MCP_GATEWAY_PORT = "19998"; & target-test\release\lmg.exe serve`；永远用环境变量，
  不要 `--port`（它会写进配置）。H2 落地之后改用你自己写的 `scripts/test-instance.ps1`。
- 停 19998 只按端口找 PID：`Get-NetTCPConnection -LocalPort 19998`。绝不 `Get-Process lmg`。
- `cargo build` 报 `Access is denied (os error 5)`，是因为要覆盖的 exe 正被某个实例占着——先停那个
  实例，不要换目录绕过去。
- 你自己的工具 shell 很可能就带着 `NO_COLOR=1`（`Get-ChildItem env:` 看一眼，把看到的写进
  H1 的清单注释里）。这既是 H1 的验收素材，也是提醒：你从工具 shell 里起的 19998，在 H1 落地前
  就是「被污染的守护进程」的活样本。
- `cargo fmt --check` 目前有 51 处 diff——**不要**顺手 fmt 整个仓库，也不要把 fmt 加进 CI。

不要做：白名单式重建环境；给 `serve` 加 `--home` 参数；引入 rustfmt / nextest / 新 crate；
重构 `manager.rs` / `jobs/mod.rs` / `dbbrowser.rs`；H6 之外碰面板；对 19999 做任何事。

汇报格式：每个提交的 hash 与一句话；每个门禁的实际输出末尾几行（不是「通过了」三个字）；
spec §7 每一条验收的证据（终端文本或截图，H4 是绿勾 URL，H5 是 `cargo tree -d` 与二进制大小
的前后对比）；你在 agent shell 里实际观察到的环境变量清单；任何与 spec 的偏差及理由——偏差不是
问题，隐瞒才是。
