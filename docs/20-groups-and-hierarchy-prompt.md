# 交给实施模型的 Prompt

> 复制下面整段给一个**全新**的实施会话。它假设模型在 `<repo>`
> 目录下工作，旁边有 `..\node-original`（Node 仓库——只当面板源码的存放处用，它的服务端一行不改）。
> 写 spec 的会话不实施；这是 swiss-spec 的规矩。

---

你在 `<repo>` 工作 —— Rust 版网关 swiss，cargo workspace，产物是
单个 `swiss.exe`。旁边的 `<node-repo>` 是 Node 版；它的 `src/admin/` 是面板
源码，本仓库的 `crates/swiss-panel/src/admin_assets/` 是它的逐字节拷贝，只能整目录复制、不能手改，
测试 `the_tree_is_byte_for_byte_the_node_builds` 会拦住任何分叉。**本任务不改 Node 的服务端
（`src/*.ts`），只改 `src/admin/` 并复制回来**——用户明确决定只做本项目。

任务：实施 `docs/20-groups-and-hierarchy-spec.md`（分组与层级：一个 `Groups` 模型、一族
`/api/groups/{scope}` 路由、一个面板 `groups.js` 组件，覆盖 MCP / 隧道连接 / 隧道转发 / Jobs /
Secrets / Tokens 六个作用域，加 Data 下拉的 optgroup）。先把 spec 读完，再读它开头列的前置文档，
尤其 `AGENTS.md`——它的规则高于 spec 和这段话。**设计语言以 `.claude/skills/swiss-design/SKILL.md`
为准**：动到任何像素之前先读它，改完拿它的 checklist 自查。

动手前先读这些文件，理解现状再改（spec §0 已经把根因和证据写清了，不要重新猜）：

- `crates/swiss-host/src/managed.rs`（`load_groups`/`group_of`/`set_groups`/`rename_group`——G1 要把
  它们换成 `Groups` 的调用，行为等价，现有测试不改）
- `src/adminapi.rs:398-744`（tokens、secrets、order、groups 四组路由；G1 在这里挂新路由族，G2 退役旧的）
- `src/subsystems.rs`、`src/builtin.rs:773-799`（作用域在哪注册；`/api/tokens` 宿主所有的那条断言是
  新路由族"宿主所有"的先例）
- `crates/swiss-tunnels/src/tunnel/store.rs`（`connGroups`/`ruleGroups`、`DEFAULT_GROUP` 保留字——
  G2 的迁移对象）与 `tunnel/api.rs:409-470`（要退役的三条路由）
- `crates/swiss-jobs/src/jobs/def.rs`、`mod.rs`（`JobDefinition`、配置行、`apply_config` 的不重启保证）
- `crates/swiss-core/src/secure/secretstore.rs`（`Vault`、`list_secrets`、`rev`）
- `src/app.rs:493-580`（`visual_order`——Data 下拉排序已经按组切片，G5 只是把组名带出来）
- 面板：`js/sidebar.js`、`js/menu.js`（`wireDrag`/`patchSidebar`）、`js/tunnels.js:10-160`、
  `js/polling.js:68-88`、`js/traffic.js`（`tunGroupHeadHtml`）、`js/add-sheet.js`、`js/tunnel-sheets.js`、
  `js/jobs.js`、`js/jobs-v2.js`、`js/views/secrets.js`、`js/views/tokens.js`、`js/data-view.js:302-331`、
  `styles/base.css:286-383`（侧栏与组头的现有规则及其注释——注释里的**理由**要保留到新规则里）、
  `styles/views.css:56-72,119,300-325`、`index.html` 的 SVG sprite
- `docs/05-wire-compatibility.md`（G2/G4/G6/G7 各追加一段落盘变化）
- `docs/07-decisions.md` ADR-013/014（ADR-015 的格式参照）
- `.agents/docs/panel.md`、`mcp.md`、`tunnels.md`（审计文档，实施后按 `ask-swiss` 的流程刷新；不要手改
  行号）

交付：**每项一个提交，顺序 G1 → G2 → G3 → G4 → G5 → G6 → G7 → G8**（G2 可拆成 Rust 与面板复制
两个提交）。每个提交的硬性要求：

- 行为变化先有测试（spec 每节的「测试」小节是最少集合），且要证明它在没有修复时会红。
- 门禁全绿：`cargo test --workspace`；`cargo clippy --workspace --all-targets -- -D warnings`；
  `cargo tree -d` 不出现新的双份。`--workspace` 不可省。面板改动另跑 Node 仓库
  `npx vitest run test/admin-pages.test.ts test/admin-groups.test.ts`（后者是 G3 新建的纯函数测试）。
- 不加 crate 依赖。
- 代码注释英文，解释「为什么」，风格跟周围一致；文档散文中文；UI 文案英文。
- 不提交 `gateway.config.json`、`.env`、`managed.json`、`tunnels.json`、`secrets.json`、`master.key`、
  `*.log`、`~/.swiss/terminal/*.cast`、`$env:LOCALAPPDATA\swiss-test-home\` 里的任何东西。
- Commit message 末尾按 `AGENTS.md` 的 attribution 规则。

运行与验证的铁律：

- **19999 是生产实例，不停、不重启、不部署。** 部署由用户自己用 `scripts/deploy.ps1` 做。
- 所有实测在 19998：`scripts/test-instance.ps1`（`-Fresh` 拿干净家；默认快照用户状态——G2 的
  `tunnels.json` 迁移正好拿真实文件验）。面板是 `rust_embed` 进 exe 的，每次面板改动要
  `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release` 再重启 19998 才能看见。
- 停 19998 只按端口找 PID（`-Stop` 就是这么做的）。绝不 `Get-Process swiss`。
- 浏览器实测用 `agent-browser` skill，自己起一个命名 session，每项亮/暗各截一张，存
  `docs/assets/20/`；spec §9 列了要截的七张。
- `cargo build` 报 `Access is denied (os error 5)`：exe 被某个实例占着——先停那个实例，不要换目录绕。
- `cargo fmt --check` 本来就有 diff——不要顺手 fmt 整仓。

不要做：嵌套组；跨作用域的组；给组上颜色或图标；把 docs/13 的一级导航也换成 `groups.js`；
改 Node 服务端；为了"简单"把 `proc` MCP 改成积极启动；用 `serde_json::Value` 走转发路径；
在宿主里写按作用域的 match arm（用 `GroupScopes` 查表）。

做完 G8 后：跑 `swiss-review` skill 自审一遍整条分支，把 spec 状态行改成"已实施 + 完成基线提交"，
然后停下来——部署是用户的事。
