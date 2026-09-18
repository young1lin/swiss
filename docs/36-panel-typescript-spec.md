# 36 — 面板迁到 TypeScript：一步到位，服务出去的仍是逐行对应的 JS

> 状态：**待实施**。基线 `a6ee5be`（2026-09-18）。T0–T4 已落地（`9332a88`）。
> **D9 与 §10「不改写法」已由 docs/37 M1 撤销**——验收线不再是发射产物与迁移前 JS 字节相同，
> 见 docs/37 §10；本文其余十二条决定继续有效。
> 前置阅读：`AGENTS.md`（规则高于本文）、`docs/07-decisions.md` ADR-016（本文推翻其中"no build
> step"一句，见 §1.1 与 ADR-024）、`docs/08-testing.md`、`.agents/rules/panel-proof-of-life.md`、
> `.agents/docs/panel.md`（面板地图）、`crates/swiss-panel/src/admin.rs`（嵌入与服务）。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-09-18）："看看项目，js 是不是已经可以迁移到 TypeScript 了，因为现在好像代码
> 有点多了，好像没办法维护了，类型有点没办法推断了。" 追加确认（同日）："我建议是一步到位，全部重构
> 好的那种，后续就沿用 TS 的写法就扩展方便多了。"
> 本 spec 把它落成：`crates/swiss-panel/panel/src/**/*.ts` 成为面板源码，`strict` 全开，55 个模块
> 一次全部迁完；服务出去的 `admin_assets/js/**/*.js` 是**类型被抹成空白后逐行对应的提交产物**——
> 浏览器看到的模块图、路径、行号与今天完全一样，`cargo build` 依旧不需要 node。

## 0. 现状与缺口（为什么是它、为什么是现在）

面板是 55 个自有 ES module（不含 vendor），17,225 行，2026-09-13 起在本仓库直接编辑
（ADR-016）。它的验收套件 `crates/swiss-panel/panel-tests/` 已经是 `.ts`（60 个文件、544 个用例），
但 vitest 走 esbuild 只剥类型不检查，所以**没有任何一行面板代码被类型检查过**。

在 scratchpad 里对当前源码跑 `tsc --allowJs --checkJs --noEmit` 得到的数字（2026-09-18）：

| 模式 | 错误 | 构成 |
| --- | --- | --- |
| 非 strict | 353 | 342 个 TS2339——`$()`/`el()` 返回 `HTMLElement`，`.value` `.checked` `.closest` 全要窄化；真信号约十条（§8） |
| strict | 3,840 | 1,162 隐式 any 参数（TS7006）、1,339 possibly-null（TS18047/TS2531）、816 DOM 窄化、255 隐式 any 变量、129 索引签名 |

推断断掉的根源不是语法，是三处结构：

1. **`util.js` 的 `state` 大包**：48 个 key，被 20 个模块各自赋值；`db: null`、`list: []` 让 TS 推成
   `null` / `never[]`——`jobs.js:191` 的 `Property 'name' does not exist on type 'never'` 就是用户说
   的"推不出来"。
2. **`apiJson()` 无返回类型**：`/api/*` 的形状只存在于读它的代码里，"面板 JS 是 admin API 的 spec"
   这句话没有可检查的载体。
3. **`$()`/`el()` 返回宽类型**：每个 `.value` 都是一次隐式假设。

另一个缺口是 `.agents/rules/panel-proof-of-life.md` 记录的事故类：`import { closeSheet } from
"../util.js"` 而 export 在 `add-sheet.js`——vitest 抓不到，浏览器整个模块图拒绝链接，页面死掉，
三次报"live verified"。这正是 `tsc` 的 TS2305，一行不漏。

### 0.1 一个实测决定了发射工具

在 scratchpad 里把 55 个文件用 `tsc` 原样发射（`allowJs`、`target es2022`、`removeComments false`）
再和源码比：63 个文件 0 个字节相同；去掉全部空白后仍有 7 个文件 token 不同——tsc 的 printer 会重排
（4 空格缩进、拆 `if (x) y;`、**丢掉 `{` 同行的尾注释和空函数体里的注释**）。这套代码库把注释当"已经付
过一次代价的 bug 记录"，服务产物里丢注释、行号漂移都不可接受。

`ts-blank-space`（Bloomberg，0.9.0，对 `typescript` 5.9.3）实测：类型语法原位替换成空格，注释、
换行、行号、引号、一切排版逐字节保留（`import type` 整行变空行）。它只接受"可抹除"语法（无 enum /
namespace / 参数属性 / 装饰器），`tsc --erasableSyntaxOnly` 正好把这条当门禁。当前 55 个文件里
这类语法为 0 处。

## 1. 决定

### 1.1 宪法核对与 ADR

四条产品属性（AGENTS.md）：本文不动进程内任何东西——不加 cargo 依赖、不动 `admin.rs`、`cargo
build` 不需要 node；服务出去的字节只在"类型抹成空格"处变化（§2.4 有上限）。**Ruthlessly small**
上的唯一代价是空格：实施时量出 exe 与嵌入树的字节差并记入提交（§9.3）。Plugin-shaped / Hot-pluggable /
三种接入方式不受影响。docs/05：盘上与线上没有任何东西移动。

推翻的是 ADR-016 里"the panel stays plain ES modules served straight from disk — no bundler, no
build step"中的**后半句**：以后面板有一个发射步骤（`npm run build`），但仍然没有 bundler，模块
1:1、路径不变、`cargo build` 与 CI 的五个 Rust 目标完全不碰 node。这满足 ADR 三条件（难回头、缺上下文
时意外、真实取舍），T5 在 docs/07 写 ADR-024，选项表见 §7.2。

### 1.2 十三条决定

| # | 决定 | 理由 |
| --- | --- | --- |
| D1 | 源码家：`crates/swiss-panel/panel/`（由 `panel-tests/` 改名），`src/**/*.ts` 与今天 `js/` 树同相对路径（`views/…` 保留），`test/**/*.test.ts` 是原验收套件 | 一个 npm 家：源码、类型、测试、发射脚本；`package.json` 已在那里 |
| D2 | 发射：`panel/build.mjs` 用 `ts-blank-space` 把 `src/**/*.ts`（排除 `*.d.ts`）写到 `../src/admin_assets/js/**/*.js`；**不 bundle、不改扩展名、不动 `js/vendor/`、不删文件**；写入前比对，内容相同不写（debug 构建按请求读盘，mtime 不该乱跳） | §0.1 |
| D3 | **发射产物提交进 git**。`.gitattributes` 标 `linguist-generated`。新鲜度由两道闸守住：vitest 用例 `panel-emit.test.ts`（内存里重发射比对磁盘，并揪孤儿 `.js`）和 `build.mjs --check`（deploy / CI 用同一函数） | `cargo build`、五个 CI 目标、`include_str!`（`crates/swiss-jobs/src/jobs/api.rs:358`）都不需要 node；一个新 checkout 直接 `cargo build --release` |
| D4 | 类型检查：`tsc --noEmit`，`strict: true`、`verbatimModuleSyntax`、`erasableSyntaxOnly`、`isolatedModules`、`target es2022`、`module esnext`、`moduleResolution bundler`、`lib: es2022 + dom + dom.iterable`。两份 tsconfig：`tsconfig.json`（src，**无** node 类型）与 `tsconfig.test.json`（extends，加 test 与 `@types/node`） | 浏览器代码看不见 `process`/`Buffer`；测试照常用 `node:fs` |
| D5 | 模块说明符保持 `./util.js`（TS 解析到 `util.ts`；发射后原样服务）；类型导入一律 `import type`（`verbatimModuleSyntax` 强制），抹除后成空行 | 服务出去的 import 图与今天逐字相同 |
| D6 | 纯类型放 `panel/src/types/*.d.ts`：**ambient 全局**（`interface PanelState`、`DbState`、`TunState`、`JobsState`、每个 `/api/*` 响应形状、`MenuItem` 等），源码里不 import 就能用；`.d.ts` 永不发射 | 没有空 `types.js` 进嵌入树；一个共享 state 包的项目不需要 import 仪式 |
| D7 | vendor 的类型面：`panel/src/vendor/<同路径>/index.d.ts` 镜像 `js/vendor/…` 的相对路径（xterm 五个入口、cronstrue），只声明面板实际调用的导出；`build.mjs` 跳过 `.d.ts`，永不写 `js/vendor/` | `views/terminal.ts` 的 `../vendor/xterm/xterm-5.5.0/index.js` 无需改一个字 |
| D8 | `util.ts` 契约：`$<T extends HTMLElement = HTMLElement>(id): T`（**非空**——今天的代码就是这么假设的；strict 下 1,339 处 possibly-null 由此消失，运行时一字不改）、`el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?, text?): HTMLElementTagNameMap[K]`、`apiJson<T = unknown>(path, opts?): Promise<T \| null>`、`export const state: PanelState` | 三处结构缺口（§0）在根上修 |
| D9 | **只加类型，不改写法**：`var` 还是 `var`，`function () {}` 还是它，不换 `const`/箭头/class/可选链，不重排、不 fmt。可检验形式：迁移终态的 `git diff -w --ignore-blank-lines a6ee5be -- crates/swiss-panel/src/admin_assets/js` **为空**，除非该行在提交说明里被点名为运行时修复并附测试（§8） | 服务字节只在空格处动；行为零变化可以用 diff 证明，不靠信任 |
| D10 | `any` 预算为零：`src/**/*.ts` 不含 `: any` / `as any` / `<any>` / `any[]`（`panel-no-any.test.ts` 用正则守），`src/vendor/**/*.d.ts` 与 `types/` 也不例外；fetch 边界用 `unknown` + `apiJson<T>` 的受信转换（无运行时校验，与今天一致） | "一步到位"的可检验定义 |
| D11 | API 形状从 Rust 侧 serde 结构抄（`crates/*/src/**/api.rs`、`src/adminapi.rs`），逐字段；面板读了 Rust 没发的字段 → 那是发现了 bug，写进提交说明，**不**用 `any`/可选糊过去 | `types/api.d.ts` 成为"面板是 API 的 spec"的可检查载体 |
| D12 | 门禁：`npm run check` = `typecheck`（两份 tsconfig）+ `vitest run`；进 `scripts/deploy.ps1` gates 阶段（node 只成为**部署机**的门禁）；CI 加一个 `panel` job（ubuntu、node 24、`npm ci`、`npm run check`），五个 Rust job 不动 | 新鲜度与类型在 PR 与部署两处都拦 |
| D13 | 开发循环：`npm run dev` = `build.mjs --watch`；debug 构建按请求读盘，保存 → 发射 → 刷新即生效。release 重建仍要 `touch crates/swiss-panel/src/lib.rs`（rust_embed 指纹陷阱不变） | proof-of-life 的规则只多一句"先 build" |

### 1.3 不变的东西（写明，免得"顺手"）

- 服务路径、`index.html`、两份 CSS、`logo.svg`、`js/vendor/**` 一字不动；`/admin/js/main.js` 仍是入口。
- `crates/swiss-panel/src/admin.rs` 不动：rust-embed 文件夹、`mime_of`、`panel_version_stamp`（长度
  戳会因空格变化，面板照常自我重载——这是期望行为）。
- 没有 bundler、没有 source map、没有 eslint/prettier、没有新 cargo 依赖、没有 `unsafe`。
- 测试套件的 60 个文件、544 个用例全部保留；只改 import 路径（106 处 `../../src/admin_assets/js/…`
  → `../src/…`，仍写 `.js` 说明符）。读源码文本断言的用例（`admin-panel.test.ts` 539/573 行一类）
  改读 `.ts`；**模块图遍历**（71 行起）继续走发射后的 `js/` 树——浏览器链接的是它。

## 2. T0 — 工具链与搬家：零注解、字节相同

把 `panel-tests/` 改名 `panel/`；`git mv` 55 个 `.js` 到 `panel/src/**/*.ts`（内容不改一字）；加
`typescript`、`ts-blank-space` 到 devDependencies（精确到 minor：`~5.9`、`~0.9`，`package-lock`
锁死——发射的确定性依赖它）；写 `tsconfig.json`、`tsconfig.test.json`、`build.mjs`、npm scripts
（`build` / `build:check` / `dev` / `typecheck` / `test` / `check`）；改 `.gitignore` 那一行；
`.gitattributes` 加 `crates/swiss-panel/src/admin_assets/js/**/*.js linguist-generated=true`。

`build.mjs` 的行为契约（也是它的测试）：

- 遍历 `src/**/*.ts`，跳过 `*.d.ts`；输出 `../src/admin_assets/js/<rel>.js`，LF。
- `ts-blank-space` 报"不可抹除语法"→ 非零退出、指出文件行号。
- 不裁空白、不改任何非类型字节（T0 的字节相同性靠这条）。
- `--check`：不写，只比；任何差异或**孤儿**（`js/` 下非 vendor 的 `.js` 没有 `.ts` 对应）→ 非零退出并列出。
- `--watch`：`fs.watch` 递归 `src/`，改一个只发一个。
- 永不写 `js/vendor/`，永不删文件（删模块 = 手删 `.ts` 与 `.js` 两处，`--check` 的孤儿检查兜底）。

**测试（先红后绿）：**

- `panel-emit.test.ts`：对每个非 `.d.ts` 源，内存发射 == 磁盘 `.js`；无孤儿。用例先在一个源里塞一个
  注解、不重发射，断言它红；重发射后绿。
- `panel-build-script.test.ts`：含 `enum` 的临时源让 `build.mjs` 非零退出；`--check` 对孤儿非零退出；
  内容相同时不改写文件 mtime。
- `admin-panel.test.ts` 模块图遍历仍对 `js/` 树通过；`vitest run` 544 全绿。
- **字节相同**：T0 提交后 `git diff --stat a6ee5be -- crates/swiss-panel/src/admin_assets/js` 为空
  （提交说明贴这条命令的输出）。
- `npm run typecheck` 此时**不绿**（约 3,840 处）——T0 的提交说明记录这个数字；它从 T1 起单调下降，
  T4 归零，T5 才把它接成门禁（§9.2）。

## 3. T1 — 类型的根：`types/` 与 `util.ts` 契约

- `panel/src/types/state.d.ts`：`PanelState`（48 个 key 全部命名，每个 key 的注释从 `util.js:25` 与各
  视图的 `xxxFreshState()` 搬过来）、`DbState`（`data-view.js` 的工厂）、`TunState`、`JobsState`、
  `RemoteState` 等；`d.db` 一类可空字段显式 `| null`。
- `panel/src/types/api.d.ts`：每个 `/api/*` 响应与请求体，按 D11 从 Rust 抄；类型名带路径感
  （`ApiMcpRow`、`ApiTunnelsResponse`、`ApiDbDataPage`…），每个类型头上一行注释指向 Rust 的结构名与文件。
- `panel/src/types/dom.d.ts`：`MenuItem`（`{label, fn, danger?} | {sep: true}` 这个 union 正是
  `tunnels.js:256` 的报错根因）、`EmptyStateSpec`、`SheetSpec` 等跨模块的小形状。
- `panel/src/vendor/**/index.d.ts`：D7。
- `util.ts` 按 D8 全部签名化，`state` 用 `PanelState` 标注；`toast`、`icon`、`esc`、`emptyHtml`、
  `dbReqGuard`（`accepts(token)` 泛型）全部有签名。

**测试：** `util.ts`、`types/**` 在 `tsc -p tsconfig.json` 下无错误（用 `tsc --noEmit` + 列表
过滤，或临时 `include` 只含这几个文件——提交说明贴数字）；`panel-no-any.test.ts` 落地并对
`util.ts`/`types/**` 绿；vitest 544 绿；`git diff -w --ignore-blank-lines` 于 `js/util.js` 为空。

## 4. T2 — 共享核心 strict 归零

顺序按被 import 的次数从高到低：`page-core`、`page-registry`、`menu`、`add-sheet`、`polling`、
`sidebar`、`pane`、`dropdown`、`fields`、`main`、`immersive`、`run`、`connect`、`plugin-palette`、
`group-logic`、`groups`、`detail`、`logs`、`traffic`、`term-overlay`、`terminal-core`。

每个文件的做法固定：参数与返回值签名；`querySelector` 结果用 `as HTMLElement` /
`!`（**不**加运行时判空——那是 D9 的运行时变化）；事件处理器 `(e: MouseEvent) => void`；
`state.xxx` 的形状回到 `types/state.d.ts` 补齐而不是就地 `as`。

**测试：** 这批文件 `tsc` 零错误（提交说明贴剩余总数）；vitest 绿；对应 `js/` 文件的
`git diff -w --ignore-blank-lines` 为空或逐行点名（§8）。

## 5. T3 — `data-*` 十四个模块 strict 归零

`data-view`（`DbState` 工厂）→ `data-browsers` → `data-grid` → `data-cell` → `data-edit` →
`data-filters` → `data-sql` → `data-structure` → `data-ddl` → `data-csv` → `data-form` →
`data-suggest` → `data-value` → `data-activity`。5,961 行；`data-grid.js:277` 覆写
`window.fetch` 的那段要给出正确的 `typeof fetch` 签名而非放宽。

data-view ↔ data-structure、data-grid ↔ data-cell 两个已接受的循环 import（文件头注释有记录）在
TS 下依旧合法（只在函数内跨界调用）；`import type` 不参与循环。

**测试：** 同 §4；`admin-data-*.test.ts`（30 个文件）全绿。

## 6. T4 — 视图与大页 strict 归零，`typecheck` 到 0

`views/*`（12 个）、`jobs`、`jobs-v2`、`run-history`、`tunnels`、`tunnel-sheets`、`views/terminal`
（1,184 行，xterm 类型面在 `src/vendor/xterm/**`）。

**测试：** `npm run typecheck` 两份 tsconfig **零错误**；`panel-no-any.test.ts` 对全树绿；vitest
绿；全树 `git diff -w --ignore-blank-lines a6ee5be -- crates/swiss-panel/src/admin_assets/js` 为空
或逐行点名；发射树字节数与基线（820,680）之比记入提交说明（§9.3）。

## 7. T5 — 门禁、文档、ADR

### 7.1 门禁接入

- `scripts/deploy.ps1`：cargo test 之前加 `Phase 'panel: npm run check'`（在 `crates/swiss-panel/panel`
  下，含 `build:check` 的新鲜度）；失败即停，与其他 phase 同款。
- `.github/workflows/build.yml`：新 job `panel`（`ubuntu-latest`，`actions/setup-node@v4` node 24，
  `npm ci`，`npm run check`）；五个 Rust job 不动、不依赖它。
- `.agents/rules/panel-proof-of-life.md`：第 1 步开头加"改 `panel/src/*.ts`，永不手改 `js/`"；第 2 步
  `node --check` + vitest → `npm run check`（`node --check` 的功能被 `tsc` 覆盖）；第 3 步加"`npm run
  build` 后再 `touch lib.rs`"。

### 7.2 文档

- `docs/07-decisions.md` **ADR-024**：选项表——A) 留 JS + `checkJs`/JSDoc（无构建步骤；owner 否决：
  之后要用 TS 写法扩展）；B) TS + tsc 发射（重排、丢注释、行号漂移，§0.1 实测）；C) **TS +
  ts-blank-space 发射、产物提交**（选）；D) TS + `build.rs` 里调 node（cargo 与五个 CI 目标不得依赖
  node）。记录代价：一个 `npm run build` 步骤、两个 dev 依赖、空格带来的字节。
- `AGENTS.md`："The panel is edited here, directly" 一条改写（源码在 `panel/src`，发射产物提交，
  `npm run check` 是面板门禁，`cargo build` 不需要 node）；Commands 块加 `npm run check`。
- `docs/08-testing.md`、`.agents/docs/panel.md`（资产表加 `panel/src` 列、计数刷新）、
  `.agents/skills/swiss-add-plugin`、`swiss-review`、`swiss-ui-design` 里所有 `admin_assets/js` 的
  编辑指引改指 `panel/src`；`.agents/skills/swiss-skill-eval/references/scenarios.md` 同步。
- README docs 表加 docs/36 一行。
- `crates/swiss-jobs/src/jobs/api.rs:358` 的 `include_str!` 路径**不改**（发射产物就在那），注释里把
  "copied tree"改成"emitted tree"。

**测试：** `deploy.ps1` 的 phase 用假失败（临时改坏一个 `.ts` 不重发射）验证会停；CI job 在 PR 上
跑绿一次；proof-of-life 按新清单在 19998 走一遍（§9.4）。

## 8. 检查器已经找到的真实发现（实施前知道，别重新猜）

来自 §0 的非 strict 跑；每条要么是纯类型修复（运行时零变化），要么点名为运行时修复并配测试：

| 位置 | 报错 | 处置 |
| --- | --- | --- |
| `tunnels.js:256` | `{ sep: boolean }` 不可赋给 `{ label; fn }` | 纯类型：`MenuItem` union（§3） |
| `jobs.js:191–202` | `state.jobs.list` 推成 `never[]` | 纯类型：`JobsState` |
| `jobs.js:827` | `$("jv-form-to-json").onclick()` 少一个参数 | 纯类型：该 handler 的 `e` 声明可选；**不**造假事件 |
| `add-sheet.js:106` 等 9 处 | `.select()` / `.selectionStart` 在 `HTMLElement` 上 | 纯类型：`$<HTMLInputElement>` |
| `views/terminal.js:219` | `webkitAudioContext` | 纯类型：`Window` 扩展声明 |
| `data-grid.js:277` | `window.fetch = function (input, init)` | 纯类型：`typeof fetch` 签名 |

strict 跑的 1,339 处 possibly-null 里，若某处 `$()` 的 id **确实**可能不在 DOM 上（视图未挂载时的轮询
回调是典型），那是既有 bug：写用例复现、修、在提交说明点名——这是 D9 允许的唯一一类运行时变化。

## 9. 验收

### 9.1 门禁命令

```powershell
# 面板（在 crates/swiss-panel/panel 下）
npm ci
npm run check            # typecheck (two tsconfigs) + build:check + vitest run
# Rust（仓库根；--workspace 不可省）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d            # 不得出现新的双份（本 spec 不加 cargo 依赖，输出应与基线相同）
```

### 9.2 交付顺序与每提交硬性要求

**T0 → T1 → T2 → T3 → T4 → T5，每条一个提交**（T2–T4 内部可按文件组再拆，但每个提交都要 vitest 绿、
`typecheck` 剩余数单调下降并写进提交说明）。`npm run typecheck` 从 T4 起必须为零，T5 起是门禁。

每个提交：vitest 全绿；`build:check` 干净（发射产物与源一致，无孤儿）；`git diff -w
--ignore-blank-lines a6ee5be -- crates/swiss-panel/src/admin_assets/js` 为空或提交说明逐行点名；
不动 `js/vendor/**`、`index.html`、`styles/`、`admin.rs`；不加 cargo 依赖；代码注释英文；
提交说明末尾按 AGENTS.md 的 attribution 规则。

### 9.3 要记录的数字（T4 与 T5 的提交说明）

- `typecheck` 耗时（冷/热）；`vitest run` 耗时。
- 发射树字节数 vs 基线 820,680；`target-test\release\swiss.exe` 大小 vs 同基线构建。
- 用 `swiss-memory-record` 在 19998 上量一次空闲内存并记入 docs/01——预期无变化，但要有数字。

### 9.4 实机验证（proof-of-life，19998）

按 `.agents/rules/panel-proof-of-life.md` 的新清单：`npm run build` → `touch
crates/swiss-panel/src/lib.rs` → `CARGO_TARGET_DIR=target-test cargo build --release` →
`scripts/test-instance.ps1 -Fresh` → 真浏览器、真指针事件走完**每个**顶层 tab、每个 seg、每个
sheet 开合、每页一个主动作、空状态；浏览器控制台零错误（模块图链接失败在这里现形）。不能验证的
流程列为 NOT verified 并写原因。

## 10. 不做什么

- 不 bundle、不 minify、不 source map、不 eslint/prettier、不 `cargo fmt`。
- 不改写法（D9）：不把 `var` 换 `const`、不换箭头函数、不引入 class、不拆文件、不重命名导出。
- 不动 `js/vendor/**`（xterm、cronstrue 仍是手工放置的 JS）；不给 vendor 写完整类型，只声明用到的导出。
- 不改任何 `/api/*` 形状；D11 发现的字段不一致只记录，修复另开提交并同时改两侧。
- 不让 `cargo build`、`build.rs`、五个 Rust CI job 依赖 node。
- 不碰 19999。
