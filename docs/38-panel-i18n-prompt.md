# docs/38 实施提示词 — 面板国际化（文/A 切换，英文为 key，中文字典）

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

先在主 checkout（`<repo>`，分支 `master`，基线 `c585831`）建
worktree，然后**只在 worktree 里工作**：

```
git worktree add .agents/worktrees/panel-i18n -b panel-i18n master
```

工作目录：`<repo>\.agents\worktrees\panel-i18n`。面板的 npm 家
在 `crates/swiss-panel/panel`，`node_modules` 不进 git——第一件事在那里 `npm ci`，否则 `npm run
check` 报 `'tsc' is not recognized`。

**不要 `cd` 回主 checkout。** PowerShell 里 `cd` 混在链式命令中会打断后面每一个相对路径——构建与
实例脚本各起一条命令，从 worktree 根跑。

## 任务

`docs/38-panel-i18n-spec.md` 定义的 I0 → I10。一句话：面板今天 700 多条英文文案内联在 61 个 `.ts`
里，没有任何 i18n 机制。加一个 `tr()`（英文原文即 key）、一本中文字典 `locales/zh.ts`（按需
`import()`）、`#appZone` 里一个 文/A 按钮（Lucide `languages`，翻转，偏好存 `localStorage.swiss_lang`），
切换时不 reload 而是 `navigatePage(currentView(), true)` 重挂当前页；然后按视图把 700 条文案扫成
key，两道机器闸（完整性扫描 + 覆盖 ratchet）进 `npm run check`。默认英文，不探测浏览器语言。

## 先读这些（按顺序，别跳）

1. `docs/38-panel-i18n-spec.md` —— 全文。§1.2 的 L1–L12 是决定，§1.3 词表是所有译文的依据，
   §2 是 I0 的 API 形状与验收，§4 的表是每个阶段的文件清单与坑。
2. `.agents/rules/panel-proof-of-life.md` —— 全文，逐条。每个阶段在 19998 上**切成中文**走一遍。
3. `crates/swiss-panel/panel/src/main.ts` 95–133 行 —— 主题按钮：偏好、翻转、首帧前脚本。语言按钮
   照它写。`index.html` 26–38（首帧脚本）、43–89（sprite）、114–124（`#ctxBar` 与 `#appZone`）。
4. `crates/swiss-panel/panel/src/page-registry.ts` 249–283 行 —— `navigatePage(id, force)` 与
   `canLeave`；26–39 行的 `legacy[].label` / `GROUP_LABELS` 是 L7（`tk()`）的第一个用例。
5. `crates/swiss-panel/panel/test/non-null-ratchet.test.ts` 与 `panel-emit.test.ts` —— 完整性扫描
   照前者用 `typescript` AST，文件表用后者的 `listSources()`。
6. `crates/swiss-panel/panel/eslint.config.*` —— R1–R5 的 ratchet 行在那里；覆盖 ratchet（I10）
   是下一行。
7. `docs/37-panel-modern-typescript-spec.md` §7 与 §1.3 —— 你依赖的重画机制，以及不许动的东西。

## 交付顺序

一个阶段一个提交，不许跳阶段（I0 可拆"机制"与"按钮 + chrome"两次）：

| 阶段 | 内容 | 关键风险 |
| --- | --- | --- |
| I0 | `i18n.ts`（`tr` / `trn` / `tk` / `locale` / `loadLocale` / `install`）、`locales/zh.ts` 骨架、`#langBtn` + `i-languages`、首帧脚本一行、`paintChrome()`、`i18n.test.ts` + `i18n-toggle.test.ts` + `i18n-complete.test.ts` | `main.ts` 顶层 `await loadLocale()` 必须在任何画东西的调用之前；`import` 的模块先执行——**模块顶层不许调 `tr()`** |
| I1 | 壳与共享：page-registry、plugin-palette、pane、page-core、menu、dropdown、groups、group-logic、add-sheet、fields、polling、connect、sidebar、immersive、util（`whenLabel` 传 `locale()`） | 标签表改 `tk()`，画时 `tr(p.label)`；`emptyNode` 自己不翻，调用点翻 |
| I2 | plugins / secrets / system | 插件 `name` / `lastError` 来自 Rust，原样进占位符 |
| I3 | tokens / traffic | `label + " copied…"` 改占位符 |
| I4 | mcps：detail、logs、run | `<code>` 里的命令不翻；logs 的计数句全占位符 |
| I5 | remote / remote-runs | 分组名 `default` 是标识符 |
| I6 | tunnels / tunnel-sheets / tunnel-forwards | 表单校验消息集中在 sheets |
| I7 | jobs / run-history / jobs-v2 | `swiss-jobs` 的 `include_str!` 守着 `jobs.js` 里的 `/api/jobs/` 与 `probeJobs` |
| I8a | data-grid、data-view、data-sql、data-ddl、data-structure | 15 处手写复数 → `trn`；`toLocaleString(locale())` |
| I8b | data-browsers、data-csv、data-form、data-edit、data-cell、data-activity、data-filters、data-suggest、data-value | redis 多行 confirm、CSV 计数 |
| I9 | terminal / terminal-settings / terminal-core / term-overlay | xterm 是 vendor，不翻 |
| I10 | 覆盖 ratchet 转 error（白名单为零）、`--sans` 补 CJK 字体、AGENTS.md 一条、proof-of-life 规则半句、README 一行、spec 状态头 | 选择器写不进的边角改写法，不加豁免 |

每个阶段的固定动作：扫字面量 → `zh.ts` 加一段 → 该视图的测试加中文挂载用例 → `npm run check`
→ 重建 19998 → 切中文走查 → 提交。

## 门禁（每次提交前）

```
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + lint + build:check + vitest（含完整性扫描）

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

实机验证（每个阶段）：

```
scripts/test-instance.ps1 -Stop
# 先在 crates/swiss-panel/panel 下 npm run build，再 touch crates/swiss-panel/src/lib.rs，
# 否则 rust_embed 的指纹不变，release 二进制里还是旧面板
$env:CARGO_TARGET_DIR="target-test"; cargo build --release
scripts/test-instance.ps1 -Fresh
```

然后用 agent-browser 在 19998 上走**真实指针事件**（CDP input，不是 `element.click()`）：先点 文/A
切成中文，确认 `document.documentElement.lang === "zh-CN"`，再按 proof-of-life 全清单走该阶段的视图：
每层导航能到、每个 seg 切换、每个 sheet `hidden === false` 且量得到 bounding rect、每个主操作发出
请求且 UI 反映结果、空状态渲染——**并且看得见的文案是中文**。对照发射产物核对服务字节（面板服务在
`/`）。**走不了的流程（缺凭据、外部端点）如实列为未验证并写明原因。**

agent-browser 的已知坑：冷启动后第一次 `open` 可能让包装器超时，但会话已经建好——下一条命令用同一个
`AGENT_BROWSER_SESSION` 接着走；`find role option click --name X --exact` 选下拉项；没有对话框时
`dialog accept` 报错是正常的。

## 提交规则

- 一个提交一个阶段；提交说明写清"扫了哪些文件、几条 key、哪些用例覆盖、19998 走了什么、没走什么及
  原因"，并贴 spec §7 的四个数（剩余裸字面量、`zh.ts` 条目、`zh.js` 字节、发射字节增量）。
- 行为改动必须点名并配用例；不许删测试来过关；不许整文件 `eslint-disable`（单行 disable 必须带理由
  注释）。
- 不许手改 `crates/swiss-panel/src/admin_assets/js/**`——那是发射产物，改 `panel/src/*.ts` 后
  `npm run build`。
- 译文按 spec §1.3 词表与风格；一个英文两种含义时改英文，不加上下文机制（L12）。
- 提交信息结尾按仓库惯例署名。

## 不做什么

见规范 §8。特别是：不翻 Rust 端任何字串、不探测浏览器语言、不做服务端偏好、不引 i18n 库、不 reload、
不改 `index.html` 的英文静态文案、不改 `/api/*`、**不碰 19999**——合并 master 与部署是 I10 之后 owner
的一次决定，不在本会话里做。

## 完成之后

`swiss-verify` → `swiss-live-verify` → `swiss-review` 三步走完，再向 owner 汇报：每阶段的四个数、
中文走查覆盖了哪些视图、哪些流程未验证及原因。
