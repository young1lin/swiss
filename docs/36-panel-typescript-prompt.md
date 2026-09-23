# 交给实施模型的 Prompt（docs/36）

> 复制下面整段给实施模型。它假设模型在一个**新的 git worktree** 里工作：
> `<repo>\.agents\worktrees\panel-ts`，分支 `panel-ts`，
> 从 `master`（基线 `69853b7`）切出。worktree 由用户先建好：
> `git worktree add .agents\worktrees\panel-ts -b panel-ts master`。

---

你在 `<repo>\.agents\worktrees\panel-ts` 工作——这是仓库
`swiss`（产品名 swiss）的一个 git worktree，分支 `panel-ts`。它是 Rust cargo
workspace，产物是单个 `swiss.exe`；管理面板是 `crates/swiss-panel/src/admin_assets/` 下的纯 ES
module，由 rust-embed 嵌进 exe。

任务：实施 `docs/36-panel-typescript-spec.md`——把面板 55 个 JS 模块**一次全部**迁到 TypeScript
（`strict` 全开、零 `any`），源码搬到 `crates/swiss-panel/panel/src/**/*.ts`，用 `ts-blank-space`
把类型抹成空格发射回 `admin_assets/js/**/*.js`（逐行对应、注释保留、产物提交进 git），接上
`npm run check` 门禁、CI job、部署脚本，写 ADR-024 并更新所有指向旧工作流的文档。先把这份 spec
读完，再读它开头列的前置文档，尤其 `AGENTS.md`——它的规则高于 spec 和这段话。

动手前先读这些文件，理解现状再改（spec §0 已把根因、数字和实测写清，不要重新测一遍）：

- `crates/swiss-panel/src/admin_assets/js/util.js`（`state` 大包、`$`、`el`、`apiJson`——D8 的契约就落在这里）
- `crates/swiss-panel/src/admin_assets/js/data-view.js` 头 90 行（`dbFreshState()` 工厂，`DbState` 的原型）
- `crates/swiss-panel/src/admin_assets/js/views/terminal.js` 头 40 行（vendor 的相对 import，D7 的镜像 `.d.ts` 就是为它）
- `crates/swiss-panel/src/admin.rs`（rust-embed 文件夹、`mime_of`、`panel_version_stamp`——**不改**）
- `crates/swiss-panel/panel-tests/package.json`、`vitest.config.ts`、`test/admin-panel.test.ts`
  （71 行起的模块图遍历要继续走发射后的 `js/` 树；539/573 行读源码文本的改读 `.ts`）
- `crates/swiss-jobs/src/jobs/api.rs:358`（`include_str!` 指向发射产物——这是产物必须留在原路径的理由之一）
- `scripts/deploy.ps1`（phase 的写法，T5 照样加一个）、`.github/workflows/build.yml`（T5 加 `panel` job）
- `.agents/rules/panel-proof-of-life.md`（面板改动的验证铁律；T5 要改它的第 1–3 步）
- `.agents/docs/panel.md`、`docs/08-testing.md`、`docs/07-decisions.md` ADR-016 与 ADR-023（ADR 的写法）
- `src/adminapi.rs` 与各 `crates/*/src/**/api.rs`：`types/api.d.ts` 的字段逐一从这里的 serde 结构抄

交付：**T0 → T1 → T2 → T3 → T4 → T5，每条一个提交**（T2–T4 可按文件组再拆）。每个提交的硬性要求：

- vitest 全绿（基线 60 文件 / 544 用例，一个都不能少）；`npm run build:check` 干净。
- `git diff -w --ignore-blank-lines 69853b7 -- crates/swiss-panel/src/admin_assets/js` 为空，或提交
  说明逐行点名并附测试（spec D9、§8）。T0 更严：`git diff --stat` 为空，字节相同。
- `npm run typecheck` 的剩余错误数写进每个提交说明，单调下降，T4 归零，T5 起是门禁。
- `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings` 绿；`--workspace`
  不可省。`cargo tree -d` 输出与基线相同——本任务**不加任何 cargo 依赖**。
- 只加类型，不改写法：`var` 留 `var`，函数表达式留着，不换 `const`/箭头/class，不拆文件、不重命名导出、
  不 fmt。运行时判空是行为变化，不许为了消 possibly-null 而加；`$()` 按 D8 声明为非空。
- `any` 为零（`panel-no-any.test.ts` 守）；面板读了 Rust 没发的字段，写进提交说明，不用 `any` 或可选糊过。
- 代码注释英文、解释"为什么"、风格跟周围一致；文档散文中文。
- 不提交 `gateway.config.json`、`.env`、`managed.json`、`tunnels.json`、`master.key`、`*.log`、
  `~/.swiss/terminal/*.cast`、`$env:LOCALAPPDATA\swiss-test-home\` 里的任何东西、`panel/node_modules/`。
- 提交说明末尾按 `AGENTS.md` 的 attribution 规则。

worktree 的几个现实：

- `crates/swiss-panel/panel/node_modules` 是 gitignored 的，这个 worktree 要自己 `npm ci`；改名
  `panel-tests` → `panel` 之后 `.gitignore` 那行同步改。
- `target-test/` 相对于 worktree 根：`$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`
  在 worktree 根跑，第一次是冷构建（~4 分钟，opt-level z + fat LTO，正常）。
- PowerShell 里 `cd` 进链式命令会弄坏后面的相对路径：构建与实例脚本在 worktree 根各自单独一条命令跑。
- rust_embed 指纹不含资源内容：改了 `admin_assets` 不会重编 swiss-panel。每次 release 重建前
  `touch crates/swiss-panel/src/lib.rs`，然后核对服务出来的字节，不只看 "Finished"。

运行与验证的铁律：

- **19999 是生产实例，不停、不重启、不部署。** `scripts/deploy.ps1` 只改、只用假失败测它会停，绝不对
  19999 执行；部署由用户自己做。
- 所有实测在 19998：`scripts/test-instance.ps1 -Fresh` 起、`-Stop` 停（按端口找 PID，绝不
  `Get-Process swiss`）。起之前先 `Get-NetTCPConnection -LocalPort 19998` 确认端口空着——其他
  worktree 的会话也可能在用它；被占就等，不要换端口，永远不要 `--port`。
- 面板改动的"完成"定义在 `.agents/rules/panel-proof-of-life.md`：真浏览器、新加载、真指针事件，走完
  每个 tab / seg / sheet / 主动作 / 空状态，控制台零错误（模块图链接失败只在这里现形）。T4 与 T5 各
  走一遍完整清单；T1–T3 至少走被改文件所属的页面。不能验证的写 NOT verified 和原因，不打勾。
- 用 `swiss-verify` 选门禁命令，`swiss-live-verify` 做实机验证，`swiss-review` 在每个提交前审一遍
  diff；T5 结束用 `swiss-memory-record` 在 19998 量一次空闲内存记入 docs/01。
- `cargo build` 报 `Access is denied (os error 5)`：exe 被某个实例占着，先 `-Stop`，不要换目录绕。

不要做：bundler、minify、source map、eslint/prettier；给 vendor 写完整类型（只声明面板用到的导出）；
改 `js/vendor/**`、`index.html`、`styles/`、`admin.rs`；改任何 `/api/*` 形状；让 `cargo build`、
`build.rs` 或五个 Rust CI job 依赖 node；把 `any` 藏进 `.d.ts`；为了让 strict 过而加运行时判空；
碰 19999。

做完 T5 后停下，汇报：每个提交的 hash 与 typecheck 剩余数曲线、§9.3 的数字、proof-of-life 走过的
清单与 NOT verified 项、D11 发现的 API 字段不一致列表。不要自己合并到 master，不要 deploy。
