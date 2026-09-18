# 37 — 面板成为真正的现代 TypeScript：撤销 D9，写法与类型一起归位

> 状态：**草案，等 owner 审**。基线 `9332a88`（2026-09-19）——docs/36 的 T0–T4 已落地
> （`tsc` 两份 tsconfig 零错误、63 个测试文件 553 用例绿、发射树新鲜），T5（门禁与文档）未做，
> 本文把它接管过来。
> 前置阅读：`docs/36-panel-typescript-spec.md` §1.2（十三条决定——本文推翻其中的 D9，并改写
> §10 的"不改写法"）、`AGENTS.md` 四条产品属性与"The panel is edited here, directly"、
> `docs/07-decisions.md` ADR-016、`.agents/rules/panel-proof-of-life.md`。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-09-19，看过 T0–T4 的成品之后）：
> "我看了，重构的很烂，不是原生的TS，还是 JQuery 写法，还有一堆的内容只是改了 JS 里面的内容"
> 追加确认（同日）："怎么让它彻底改造成真的现代的 TS 项目"

## 0. 现状与缺口

### 0.1 docs/36 交付了什么（这部分不推翻）

一个 npm 家（`crates/swiss-panel/panel/`）、`build.mjs` 的 ts-blank-space 发射、产物提交进
git、两道新鲜度闸（`panel-emit.test.ts` 与 `build.mjs --check`）、55 个源模块 17,225 行全部带
签名、13 个 `.d.ts` 2,010 行的形状库、`tsc` 两份配置零错误。`cargo build` 与五个 Rust CI 目标
仍然不碰 node，服务路径与模块图一字未动。**脚手架是对的，本文一行不改它。**

### 0.2 它没交付什么：三组实测（基线 `9332a88`）

**第一组——语言还是 2015 年的。**

| 计数 | 现代写法 | 现状 |
| --- | --- | --- |
| `var` 2,178 | `const` / `let` | `const` 17、`let` 3 |
| `function (` 1,016 | 箭头函数 | 78 |
| `.onclick = ` 一类内联赋值 282 | `addEventListener` / 事件委托 | 58 |
| `innerHTML` 134 | 类型安全的构造 | `el()` 存在但没人用在这些位置 |
| `!.` 非空断言 **1,041**（data-grid 180、data-view 121、data-sql 67） | 一次窄化或早返回 | 每次成员访问写一个 `!` |

`var d = state.db;` 之后连写二十个 `d!.conns` / `d!.conn`——写成 `var d = state.db!;` 抹除后
字节完全相同，一次能消掉二十个。这不是规范逼出来的，是没想。

**第二组——类型被掰弯以适配没动的代码。**

| 位置 | 做的事 | 源码注释里写的理由 |
| --- | --- | --- |
| `types/dom.d.ts:245` | 改写全局 `RegExp.test(s: string \| null)` | D9 不允许括号 cast |
| `types/dom.d.ts:178` | 全局 `EventTarget` 加 `closest?` / `tagName?` / `type?: unknown` | 同上 |
| `types/dom.d.ts:78`、`types/terminal-view.d.ts:85` | 全局 `Function` 加 `_t` / `_warned` | 模块级声明会多一个分号 |
| `types/terminal-view.d.ts:78` | 全局 `Window` 增补 | 同上 |
| `tsconfig.json` | `noImplicitThis: false`、`useUnknownInCatchVariables: false` | 修它们要改 token |
| 五个 `.d.ts` | **37 处 `[key: string]: unknown`** | —— |
| `types/dom.d.ts:210` 起 | 虚构 `FilterInput` / `FilterTextArea` / `ActionButton` / `FilterSelect`，把 DOM 自带的 this 参数删掉，142 处调用点用它 | 49 个 handler 读 `this.value` |

`RegExp.test` 那条影响整个程序：任何人写 `re.test(maybeNull)` 从此不报错，而运行时会把 null
强制成字符串 `"null"`。为了一个文件不改 token，全程序的类型说了一句谎。

37 处索引签名是 D10「any 预算为零」的等价漏洞：`[key: string]: unknown` 让任意属性访问都合法，
正则守卫看不见它。于是 D11 想要的「面板是 API 的 spec」没有兑现——`McpDetail`、`JobDef`、
`JobConfigRow` 这些恰恰是最该逐字段对齐 Rust serde 的结构，却都带着开放签名。

另有 73 处 `$<HTMLElement>(...)`——D8 定的默认类型参数就是 `HTMLElement`，这 73 个尖括号是纯噪声。

**第三组——架构还是一个全局可变袋子。**

`util.ts:25` 一个 `var state: PanelState = {...}`，**35 个字段**，把 MCP 行、traffic 分页游标、
db/tun/jobs 三个子状态、token 分组、拖拽槽、菜单开合塞在一起，ambient 可见，每个模块直接改。
谁在什么时候写了哪个字段，只能靠 grep。

### 0.3 根因：两条公理互斥，而机器只校验其中一条

docs/36 同时要了 D4/D10（strict、零 any、类型即 API 契约）和 D9（一个 token 都不许动，验收线是
`git diff -w --ignore-blank-lines a6ee5be` 为空）。这两条在几十个位置正面冲突。冲突时 D9 赢，
因为它是**机器校验**的，而"类型是否诚实"没有任何闸。于是每一次冲突的代价都记在类型上：加一条
全局增补、开一个索引签名、关一个 strict 子项。

**这是本文要修的结构性问题，不是收拾几个文件的问题。** 换掉 D9 的同时必须给出一个同样机器可校验
的替代闸，否则只是把绳子松开。替代闸是 §1.2 M10 的 eslint ratchet：写法本身成为被校验的对象。

### 0.4 一个便宜的事实：工具链没有拦路

ts-blank-space 今天就能发射 `const`、`let`、箭头函数、可选链、`class`、括号 cast——它只抹类型、
不碰值语法，且**逐行保号**（不需要 source map）。挡住现代写法的只有 D9 的验收线，不是发射器。

所以 R0 阶段零成本：不换发射器、不引 bundler、不引 source map、不加运行时依赖、服务形态一字不动，
写法立刻解锁。

## 1. 决定

### 1.1 宪法核对与 ADR

四条产品属性：**Ruthlessly small** 只在发射字节上有量：`const`/`let` 比 `var` 长，箭头比
`function` 短，两边相抵，R5 量出 exe 与嵌入树的实际差并记账（§9）。**Plugin-shaped**、
**Hot-pluggable**、**三种接入方式**：不动进程内任何东西，不加 cargo 依赖，`admin.rs` 不动，
`cargo build` 仍不需要 node。docs/05：盘上与线上没有任何东西移动，`/api/*` 形状不变。

**ADR-024（docs/36 欠的）**：TS + ts-blank-space + 产物提交，选项表见 docs/36 §7.2——本文 R5 补写。
**ADR-025（本文）**：D9 的失败与撤销。三条件齐备：难回头（写法改回去等于再做一遍）、缺上下文时
意外（为什么发射产物不再与 `a6ee5be` 对齐）、真实取舍（放弃"行为零变化可用 diff 证明"，换类型
诚实与可维护的写法）。选项表：A) 维持 D9（否决：0.2 的三组账单只会随每次改动变长）；
B) 撤销 D9，无替代闸（否决：写法会各写各的）；C) **撤销 D9 + eslint ratchet 作为机器闸**（选）；
D) 换 tsc/esbuild 发射 + source map（否决：丢逐行保号，多一层调试间接，且不解决类型说谎）。

### 1.2 十二条决定

| # | 决定 | 理由 |
| --- | --- | --- |
| M1 | **撤销 D9**。验收线不再是"发射产物与迁移前 JS 字节相同"。新验收线：`npm run check` 全绿（typecheck ×2 + lint + `build:check` + vitest 553）+ 每个改写文件有覆盖它的用例 + 19998 实机走查（`.agents/rules/panel-proof-of-life.md`） | 0.3：机器闸从"字节"换到"写法与类型" |
| M2 | **发射管线一字不改**：ts-blank-space、逐行保号、产物提交、无 bundler、无 minify、无 source map、`erasableSyntaxOnly` 保留（现代 TS 本来就不写 `enum`/`namespace`/参数属性） | 0.4：解锁不花钱；`include_str!`、rust_embed、55 个模块路径全部不动 |
| M3 | **删掉五处全局内建增补**（`RegExp`、`EventTarget`、`Function`×2、`Window`），`tsconfig` 恢复 `noImplicitThis` 与 `useUnknownInCatchVariables` 的严格默认 | 全程序的类型不再为几十个调用点说谎 |
| M4 | **49 个 `this.` handler 改读 `e.currentTarget`**，随即删掉 `FilterInput` / `FilterTextArea` / `ActionButton` / `FilterSelect` 四个虚构接口，142 处调用点回到真实 DOM 类型（`$<HTMLInputElement>` 等）；73 处 `$<HTMLElement>` 去掉冗余类型参数 | 一次 49 行的改动，换掉四个假接口 + 一个 strict 子项 |
| M5 | **99 处 `catch (e)` 归位**：`util.ts` 加 `errText(e: unknown): string`（`e instanceof Error ? e.message : String(e)`），调用点一行一换 | 另一个 strict 子项回来 |
| M6 | **`var` → `const`/`let`（2,178 处）**，回调位置的 `function ()` 改箭头——**先做 M4 再做这条**，否则箭头会改掉 `this` 语义 | 顺序是硬性的：M4 不先做，M6 就是一组静默的行为 bug |
| M7 | **1,041 个 `!` 收敛到 100 以下**：在绑定处一次窄化（`const d = state.db!`）或早返回；留下的每个必须带注释说明为什么非空。`@typescript-eslint/no-non-null-assertion` 在 R1 末转 error（白名单按文件列，逐阶段缩短） | 这是"看起来像 TS 却不是 TS"的最大单项 |
| M8 | **ambient 全局类型改成显式导出 + `import type`**。文件名仍是 `.d.ts`（`build.mjs` 不发射它们，改成 `.ts` 会往嵌入树写空文件）；`verbatimModuleSyntax` 已开，`import type` 抹除为空行 | 类型有了出处；哪个模块用哪些形状可被工具回答 |
| M9 | **37 处索引签名按 Rust serde 逐字段补齐**（`crates/*/src/**/api.rs`、`src/adminapi.rs`）。确实开放的透传位置保留签名并注释说明为什么。发现面板读了 Rust 不发的字段 → 按 docs/36 D11 记成 bug 条目，**不**用可选糊过去 | 兑现 D11：面板是 API 的可检查 spec |
| M10 | **eslint + typescript-eslint（type-aware）成为写法闸**，规则按阶段 ratchet 上锁（§7 的表）。`npm run check` 加 `lint`。**不引 prettier**——房子的注释是手工对齐的，prettier 会毁掉它，格式由 review 守 | 替代 D9 的机器闸；新增 3 个 dev 依赖，不进运行时 |
| M11 | **全局 `state` 拆七片**：`mcp` / `traffic` / `db` / `tun` / `jobs` / `tokens` / `ui`，各自 own 在自己的模块里，`util.ts` 只留组合根与导出。一片一次提交 | 35 字段的袋子是"谁改了什么"无法回答的根源 |
| M12 | **渲染层按视图逐个归位**：`innerHTML` 134 处优先转有 XSS 面的位置（渲染远端/用户字符串），改用类型安全的构造；282 处内联 handler 赋值改事件委托（每视图一个根监听 + `data-act` 分发）。**每个视图做完走一遍 proof-of-life** | 最贵最险的一批，也是"jQuery 写法"最刺眼的地方 |

### 1.3 不变的东西（写明，免得"顺手"）

- 服务路径、`index.html`、两份 CSS、`logo.svg`、`js/vendor/**` 一字不动；`/admin/js/main.js` 仍是入口。
- `crates/swiss-panel/src/admin.rs` 不动（rust-embed 文件夹、`mime_of`、`panel_version_stamp`）。
- 55 个模块 1:1 发射，路径与扩展名不变；四处动态 `import()`（`page-core.ts:51`、
  `page-registry.ts:185`、`polling.ts:134/210`）继续按 URL 解析。
- `crates/swiss-jobs/src/jobs/api.rs:358` 的 `include_str!` 断言 `jobs.js` 含 `/api/jobs/` 与
  `probeJobs`：改写 `jobs.ts` 时这两个符号要么保留，要么两侧同一提交改。
- 不改任何 `/api/*` 形状；不碰 19999；UI 的画法、tokens、间距、图标不属于本文（那是
  `swiss-ui-design` 的领域）。
- 553 个用例全部保留。断言源码文本的用例随改写同步，**不许删测试来过关**。

## 2. R0 — 解锁：撤销 D9，装上 eslint，扫掉零风险的噪声

改动只有三件，任何一件都不触碰运行时语义：

1. **文字层撤销 D9**：`docs/36` 头部加一行"D9 与 §10『不改写法』由 docs/37 M1 撤销，验收线见
   docs/37 §10"；`.agents/rules/panel-proof-of-life.md` 第 1 步开头改成"改 `panel/src/*.ts`，
   永不手改 `js/`"，第 2 步的 `node --check` + vitest 换成 `npm run check`。
2. **eslint 落地**：`eslint.config.js`（flat config，eslint 9）+ `typescript-eslint` 的 type-aware
   配置，`parserOptions.projectService` 指向两份 tsconfig。首轮只开当前树已经能过的规则
   （§9 表里标 R0 的那几条），`package.json` 加 `lint` 脚本，`check` 变成
   `typecheck && lint && build:check && test`。
3. **73 处 `$<HTMLElement>(...)` 去掉冗余类型参数**（D8 的默认就是它）。

**验收：** `npm run check` 全绿；改动只落在 `src/**`、`eslint.config.js`、`package.json` 与两个
md；发射产物的 diff 只在 73 行的空白上——这是 D9 撤销前最后一次"字节可证"的改动，留个对照。

## 3. R1 — `this` / `catch` / 五处全局增补：让 strict 真的 strict

顺序是硬性的，因为第三件依赖前两件：

1. **M4**：49 个 handler 从 `function () { …this.value… }` 改成
   `(e) => { const t = e.currentTarget as HTMLInputElement; …t.value… }`。做完删掉 `FilterInput` /
   `FilterTextArea` / `ActionButton` / `FilterSelect`，142 处调用点改回真实 DOM 类型。
2. **M5**：`util.ts` 加 `errText(e: unknown): string`；99 处 `catch` 调用点换成它。
3. **M3**：`tsconfig.json` 删掉 `noImplicitThis: false` 与 `useUnknownInCatchVariables: false` 两行
   连同它们的注释；删掉 `types/dom.d.ts` 的 `Function` / `EventTarget` / `RegExp` 三块与
   `types/terminal-view.d.ts` 的 `Window` / `Function` 两块。
   - `EventTarget.closest` 的调用点改 `(e.target as HTMLElement).closest(".row")`——D9 撤销后括号合法。
   - `Function._t` / `_warned`（toast 的一次性计时器与 main.ts 的两个 guard）改成模块级
     `let toastTimer: ReturnType<typeof setTimeout> | null = null`。
   - **`RegExp.test(string | null)` 是唯一有运行时面的一条**：今天 null 被强制成字符串 `"null"`，
     换成守卫会改变行为。逐个调用点看清楚它想要什么，在 `re.test(s ?? "")` 与
     `s != null && re.test(s)`（更可能是原意）之间选。**每个调用点在提交说明里点名，并配一个
     断言该分支的用例。**

**验收：** `npm run check` 全绿；`src/types/` 下再没有对 Function / EventTarget / RegExp / Window
的增补；`tsconfig.json` 不含任何取 `false` 的 strict 子项；R1 的提交说明列出每个 `RegExp.test`
调用点的选择与对应用例名。

## 4. R2 — 语言层：`var` 退场，`!` 收敛

按目录分批提交（`util` + shell → `data-*` → `views/*` → `jobs` / `tunnels` / `run-history` /
`terminal`），每批独立跑门禁：

- **M6**：`var` → `const`（默认）/ `let`（确实重新赋值的）；回调位置的 `function ()` 改箭头。
  顶层的具名 `function foo()` 声明**保留**——它们被提升、被跨模块引用，改成 `const foo = () =>`
  只会制造 TDZ 风险，没有收益。
- **M7**：`!` 收敛。每个文件做法固定：函数入口一次窄化（`const d = state.db!` 或
  `if (!state.db) return;`），之后的成员访问不带 `!`。data-grid（180）、data-view（121）、
  data-sql（67）三个大户各自单独提交。

**验收：** `npm run check` 全绿；`src/**/*.ts` 行首的 `var ` 计数为 0；`!.` 总数 < 100 且
`no-non-null-assertion` 从 warn 转 error（白名单文件在提交说明里列出，每个带注释）；553 个用例全绿；
**且 `data-grid` / `data-view` / `terminal` 三个视图在 19998 走一遍实机**——R2 是行为风险最高的
一批（箭头函数改 `this`、`const` 改提升语义）。

## 5. R3 — 类型层：类型有出处，API 形状不再开放

- **M8**：`types/*.d.ts` 的 ambient `interface` 改成 `export interface`，55 个源模块按需
  `import type { PanelState } from "./types/state.js"`。文件名保持 `.d.ts`——`build.mjs` 跳过它们，
  改成 `.ts` 会往嵌入树写 13 个空 `.js`，这是本阶段唯一的机械陷阱。
- **M9**：37 处 `[key: string]: unknown` 逐个对账。做法：打开对应的 Rust serde 结构
  （`crates/swiss-mcp/src/**/api.rs`、`crates/swiss-jobs/src/jobs/api.rs`、`src/adminapi.rs` …），
  字段对字段抄。保留签名的位置必须在注释里写明"为什么这里的形状确实是开放的"。

**验收：** `npm run check` 全绿；`consistent-type-imports` 转 error；`src/types/` 里
`[key: string]` 的剩余数逐文件列进提交说明并附理由；发现的 Rust / 面板字段不一致单独成清单
——**本文不修它们**，按 docs/36 D11 另开提交两侧同改。

## 6. R4 — 状态层：35 字段的袋子拆成七片

`PanelState` 按域切开，一片一次提交，顺序按耦合度从低到高：`tokens` → `traffic` → `ui`
（`view` / `menuOpen` / `filter` / `collapsed` / `dragging` / `addGroup` / `draggingGroup` /
`panelVersion`）→ `tun` → `jobs` → `db` → `mcp`。

每片的形状：模块 own 自己的 state（`export const trafficState: TrafficState = {…}`），读写只经过
该模块导出的函数；`util.ts` 的 `state` 在过渡期保留为组合根，字段逐片删空，最后一片做完时它只剩
一个空对象，连同 `PanelState` 一起删掉。

**验收：** 每片一次 `npm run check` + 该域的视图在 19998 实机走查；最后一次提交后 `src` 里
`state.` 的引用为 0，`no-restricted-imports` 禁止任何模块再从 `util.js` 导入可变 state（规则转 error）。

## 7. R5 — 渲染层：`innerHTML` 与 282 个内联 handler（按视图，逐个 proof-of-life）

这是最贵的一批，也是"jQuery 写法"最刺眼的地方。**不做整树一次性替换**，按视图逐个来，顺序按危险
度从低到高：`tokens` → `secrets` → `plugins` → `system` → `mcps` → `remote` → `remote-runs` →
`tunnels` → `jobs` → `data-*` → `terminal`。

每个视图的做法固定：

1. `innerHTML` 里拼接远端 / 用户字符串的位置**优先**改成构造（`el()` 已有，扩成
   `h(tag, props, ...children)` 的类型安全版本）；纯静态骨架的 `innerHTML` 可以留。
2. 该视图的 `.onclick = ` / `.oninput = ` 赋值改成视图根上的一个委托监听器 + `data-act="…"` 分发；
   重绘后不再需要重新挂 handler——今天每次 render 都重挂一遍。
3. 做完这个视图，**按 `.agents/rules/panel-proof-of-life.md` 的完整清单在 19998 走一遍真实点击**：
   每层导航能到、每个 seg 切换、每个 sheet 可见地打开、每个主操作发出请求并反映结果、空状态渲染。

**验收：** 每个视图一次提交，提交说明写清"走查了哪些控件、哪些没走查及原因"；`innerHTML` 与内联
handler 的总数逐阶段记账（§11）；`no-restricted-properties` 对 `innerHTML` 的规则在最后一个视图
落地时转 error（白名单列静态骨架位置）。

## 8. R6 — 门禁、文档、ADR（含 docs/36 欠下的 T5）

### 8.1 门禁接入

- `scripts/deploy.ps1`：cargo test 之前加一个 panel phase（在 `crates/swiss-panel/panel` 下跑
  `npm run check`，含 `build:check` 的新鲜度与 `lint`）；失败即停，与其他 phase 同款。
- `.github/workflows/build.yml`：新 job `panel`（`ubuntu-latest`、`actions/setup-node@v4` node 24、
  `npm ci`、`npm run check`）；五个 Rust job 不动、不依赖它。
- `.agents/rules/panel-proof-of-life.md`：第 3 步加"`npm run build` 之后再 `touch
  crates/swiss-panel/src/lib.rs`"——rust_embed 的指纹陷阱不变。

### 8.2 文档

- `docs/07-decisions.md`：补 **ADR-024**（docs/36 的选择）与 **ADR-025**（本文撤销 D9，选项表见 §1.1）。
- `AGENTS.md`："The panel is edited here, directly" 一条改写——源码在
  `crates/swiss-panel/panel/src/*.ts`，发射产物提交，`npm run check` 是面板门禁，`cargo build`
  不需要 node；`panel-tests/` 的旧路径全部改成 `panel/`（第 92、201 行）。
- `docs/08-testing.md`、`.agents/docs/panel.md`（资产表加 `panel/src` 列、计数刷新）、
  `.agents/skills/swiss-add-plugin` / `swiss-review` / `swiss-ui-design` 里所有指向
  `admin_assets/js` 的编辑指引改指 `panel/src`；`swiss-skill-eval/references/scenarios.md` 同步。
- README docs 表加 docs/36 与 docs/37 两行——docs/36 的两个 md 目前在主 checkout 里仍未跟踪，
  **R6 一并入库**。

**验收：** deploy 的新 phase 用假失败验证会停（临时改坏一个 `.ts` 不重发射）；CI 的 `panel` job 在
PR 上跑绿一次；`panel-tests` 这个词在 AGENTS.md / docs / .agents 下无残留。

## 9. lint ratchet（替代 D9 的机器闸）

| 规则 | 转 error 的阶段 | 它守住什么 |
| --- | --- | --- |
| `@typescript-eslint/no-explicit-any` | R0 | D10 的 any 预算（正则守卫退休，交给 lint） |
| `no-restricted-syntax`：禁止在 `types/**` 里增补内建（Function / EventTarget / RegExp / Window / Element / Array / String） | R1 | §0.2 第二组永不复发 |
| `no-var`、`prefer-const` | R2 | M6 |
| `@typescript-eslint/no-non-null-assertion` | R2 末（带白名单） | M7 |
| `@typescript-eslint/consistent-type-imports` | R3 | M8 |
| `@typescript-eslint/no-unnecessary-condition` | R3 | 索引签名补齐后，死分支现形 |
| `no-restricted-imports`：禁止从 `util.js` 导入可变 state | R4 | M11 |
| `no-restricted-properties`：`innerHTML`（白名单静态骨架） | R5 | M12 |
| `@typescript-eslint/no-floating-promises` | R0 起 warn，R3 转 error | 面板里大量 `apiJson()` 不 await 的位置 |

## 10. 验收与门禁命令

```
# 面板（在 crates/swiss-panel/panel 下）
npm run check          # typecheck ×2 + lint + build:check + vitest 553

# Rust（仓库根；--workspace 不可省）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d          # 不许新增重复依赖；本文不加 cargo 依赖，这条是回归守卫
```

实机（proof-of-life，19998）：R2 三个大视图、R4 每片对应的视图、R5 每个视图——按
`.agents/rules/panel-proof-of-life.md` 的完整清单，**真实指针事件**，不是 `element.click()`。

### 10.1 交付顺序与每提交硬性要求

R0 → R1 → R2 → R3 → R4 → R5 → R6，不许跳。每个提交：

- 只做一件事，提交说明写清"这批改了什么写法、为什么这样改、哪些用例覆盖它"；
- `npm run check` 与 `cargo test --workspace` 在提交前跑过；
- **行为改动（R1 的 `RegExp.test`、R2 的箭头 / `const`、R5 的委托）必须点名并配用例**——这是 D9
  撤销之后唯一剩下的证明手段；
- 不许删测试来过关；不许整文件 `eslint-disable`，单行 disable 必须带理由注释。

## 11. 要记录的数字（每阶段的提交说明里）

基线（`9332a88`）：发射自有 JS **869,275 字节**（vendor 542,324 不计），`var` 2,178，`!.` 1,041，
`innerHTML` 134，内联 handler 282，索引签名 37，全局增补 5，lint 错误 —（尚未接入）。

每阶段记：发射字节与基线的比、上面六个计数的剩余数、`npm run check` 耗时。R6 另记 release exe 的
字节差（与 `a6ee5be` 比）——**Ruthlessly small** 的账要算到最后。

## 12. 不做什么

- 不 bundle、不 minify、不 source map、不引 prettier、不 `cargo fmt` 面板。
- 不换发射器（ts-blank-space 留着），不引任何运行时依赖，不动 `js/vendor/**`。
- 不改 `/api/*` 形状；M9 发现的字段不一致只记录，修复另开提交并同时改两侧。
- 不让 `cargo build`、`build.rs`、五个 Rust CI job 依赖 node。
- 不碰 19999；不碰 UI 的画法与设计语言，那是 `swiss-ui-design` 的领域。
- 不引入框架（React / Vue / lit）：M12 的 `h()` 是一个几十行的构造函数，不是运行时。
