# 46 — B 方案：面板 UI 库（`panel/src/ui/` + `ui.css`）与全插件页整理

> 状态：**已实施**（分支 `panel-ui`，基线 `69b2460`，2026-09-23 起草；P0–P9 于同分支逐阶段提交）。P9 收尾数字（§7 口径，2026-09-24 实测）：
>
> - `views.css` 60,001 B / 227 类（基线 94,663 B / 349 类，同一计数器：去注释后选择器里出现的不同 `.class`；§0.2 手数 352）；
>   G7 冻结随降 76,412 → 60,001。`ui.css` 57,516 B / 162 类；`base.css` 26,238 B / 33 类（基线 36,969 B / 80 类）。
> - `font-weight` 取值 4 种，全部走 `--w-*` token（`--w-body/--w-emph/--w-name/--w-title`；基线 9 种、54 处字面）。
> - 字面值（G4 口径：px+hex+rgb，token 块外）：`views.css` 247 → **159**；`ui.css` 72、`base.css` 22（地板：滚动条与壳的固定宽度）。
> - G2（views.css 改库类，P9 起连 `svg`/`use` 元素主语一起算）与 G5（视图里的库类字面值）**双双归零**，表已清空——新命中直接红。
> - 自有发射 JS（`admin_assets/js` 去掉 vendor）1,442,512 B（基线 1,302,336 B）；vendor（xterm + cronstrue）770,288 B；
>   `swiss.exe` 10,554,368 B（P9 终局构建，`target-test`）。
> - 陈列页 2,482 + 28,483 + 11,143 = 42,108 B（P1b 为 32,637 B，差额是此后入库的组件）。
> - 截图（虚构数据，19996 空 home + 本地 Docker 演示库）：`docs/assets/46/` 下 01 MCP Config、02 Data orders、03 Terminal
>   各亮/暗一张，04 陈列页（亮 en、暗 zh），05 Data 结构页（暗 zh）。
>
> 数字命令：`(Get-Item crates/swiss-panel/src/admin_assets/styles/*.css).Length`；类数用一段 `node` 去注释后收集选择器里的
> `.class`（基线同一脚本跑在 `git show 69b2460:…` 上）；G4/G7 各自的 vitest 门（`test/css-literals`、`test/css-size`）；
> `node` 遍历 `admin_assets/js` 求和（vendor 目录单列）；`(Get-Item target-test/release/swiss.exe).Length`。
> 前置阅读：`.agents/skills/swiss-ui-design/SKILL.md` 全文（本文 P1c 改写它）；
> `.agents/rules/panel-proof-of-life.md`；`docs/35-grouped-list-spec.md`（band 与内联表单）；
> `docs/39-shell-refresh-spec.md`（壳：rail、上方栏，本文一层都不挪）；`docs/37` §7（`h()`/`fill()`）；
> `docs/33` C3（Logs 的 JSON 代码块，本文 P2 修订其中两条）。
> 视觉参照：`docs/assets/46/directions.html`（双击打开，现状 / A / B / C 四个预设，左侧 rail 换页，
> 按住空格对比现状）。数据全是虚构的。代码注释英文，中文只在本文。

> 需求原文（用户，2026-09-23）：
> 1. 给 MCP › Servers › Logs 的暗色截图："看看我这个项目，是否要彻底重构下 UI 才行？……怎么说呢，没那种高级感"
> 2. "生成个临时的 HTML，我看看你的几个选项？……注意，要可交互的"，追加"放到本地就行了，HTML"
> 3. 看了只有 MCP 页的原型后："你这个 B 方案不错，但是只有 MCP 的，没有其他的样式展示，例如 Data Jobs
>    Terminal Remote 等等"
> 4. 看了 13 页的原型后："就 B 方案吧，我建议这次 spec 连对应的 skill 也要改了，.agents/skills/swiss-ui-design
>    这个也要改了，统一 UI 风格。构建独立的 UI 库，保证风格统一。这样可以吧"
> 5. 四个追问按推荐定下（库在树内、系统字体、组件陈列页随二进制发、每迁一页删一页的旧 CSS），然后：
>    "没问题，写 spec 后，review 一下，有问题就改，没问题就直接干了"
>
> 6. 写 spec 途中追加："我建议是，你先自己把控好，自建的组件库。后续设计可以直接复用组件，而不是纯自己手搓"
>
> 第 5 条改变了 swiss-spec 的默认流程：写 spec 的会话**自己实施**。交接 prompt（`46-…-prompt.md`）照写，
> 用途变成上下文压缩后的续作说明。第 6 条是 U17：库先行，而且以后的**设计稿**也用库拼，不再手写一套样式。

## 0. 现状与缺口

### 0.1 结论先说

不需要推倒重来。层级是对的（rail = L1、上方栏 = L2、页内 seg = L3，docs/39），token 是对的
（五级字阶、4pt 网格、灰阶面）。"不高级"来自两件事：

1. **看得见的**：满屏粗体、线太多、Windows 经典滚动条、页头随内容滚走、每页的说明写三四行、
   同一信息在一屏重复出现（§0.3）。
2. **看不见的**：规则只写在 skill 里，没有代码逼着页面遵守。每个页面自己拼行、拼页头、拼按钮区，
   所以每页都有一点不一样，一次整理之后还会再走样（§0.2）。

第 2 条是本文的主题：把规则变成**唯一的写法**——一个树内 UI 库，加上几道机器门禁。

### 0.2 代码里的数字（基线 `69b2460`）

| 量 | 值 | 说明 |
| --- | --- | --- |
| `styles/views.css` | 94,663 字节 / 1,175 行 / **352** 个不同的类 | 其中 `db-*` 130 个、`term-*` 33 个、`config-*` 15 个、`hist-*` 11 个 |
| `styles/base.css` | 36,969 字节 / 580 行 / 80 个类 | token、壳、按钮、字段、组 |
| `views.css` 里的 px 字面值 | **247** 处 | token 之外手写的尺寸 |
| `font-weight` 取值 | **9 种**：400 / 450 / 500 / 550 / 560 / 590 / 600 / 650 / 700 | 54 处，全是字面值 |
| 自有发射 JS（`js/**` 非 vendor） | 1,302,336 字节 | |
| 同一个模式手写在几个模块里 | `pane-head` 10、`sec-head` 6、`"dot` 12、`call-sum` 4、`row-main` 4、`tun-row` 3 | `grep` 于 `panel/src`，见 §0.4 |

同一个"行"有四套实现：`.row`（Token、Secrets、Remote）、`.tun-row`（Tunnels、Plugins、System）、
`.call`（Logs、Traffic、Remote Runs、Jobs 运行记录）、`.cli-row`（Traffic 客户端）。
同一个"分段控件"有两套：`.seg` 与 `.db-tabs`（样式逐行复制，含暗色覆盖）。
同一个"弹出菜单"有两套：`.menu` 与 `.ctx-menu`。

### 0.3 看得见的问题（19998 实测截图，2026-09-23；原型的"现状"预设照此复刻）

全局四条：

| 问题 | 现状 |
| --- | --- |
| 经典滚动条 | 只有 Data 与 Terminal 自己处理过滚动条，其余是带上下箭头的 Windows 经典滚动条 |
| 满屏粗体 | 侧栏名 500，组名 / 选中 / 当前 tab 600，工具名 550；暗色下没有层次 |
| 线太多 | 卡片外框 + 行分隔线 + 网格竖线 + rail 竖线，暗色下像格子纸 |
| 页头随内容滚 | 整个 `.pane` 一起滚（`views.css:18`），MCP 详情往下翻 Logs 时 Tools…Logs 被截一半 |

分页（每条都对应 §3 的一个验收项）：

| 页 | 问题 |
| --- | --- |
| MCP › Servers › Logs | 每行完整日期 + `mcp` + `default` + `N chars`，逐行重复；13 条相同的 PING 各占一行 |
| MCP › Traffic | 页首写着 `TRAFFIC`，上方栏的标签也叫 Traffic；活动行和 Logs 一样吵 |
| MCP › Token | 说明 4 句（规则 20 说最多 2 句），把表单推到下面 |
| Tunnels › SSH Connections | 底部 "3 connections, 1 connected" 与上方栏计数重复 |
| Tunnels › Port Forwards | 一排四个按钮（New / New group / Start all / Stop all，违反规则 4）；错误多占一整行红色等宽字，行高不齐 |
| Data | 列注释是饱和绿色（违反规则 2，CSS 注释自称"deliberate exception"，skill 里却没有这条例外）；选中的表是蓝字（违反规则 15）；网格横竖线全画 |
| Jobs | 说明三句 + `SCHEDULED COMMANDS` 标题（页面本身就是 Jobs）+ 三个按钮；cron 原文与 12 小时制绝对时间 |
| Terminal | 自带一套灰（舞台 `#2b2b2b`、按钮 `#427ab3`，`views.css:901`），和面板的 `#0f1012` / `#3b82f6` 对不上 |
| Remote › Targets | 三行内容，说明占一半 |
| Remote › Runs | 完整日期 + 目标 + 来源 + 耗时 + 退出码挤成一串灰字，失败的不显眼 |
| Settings › Plugins | 每行一个绿点加一个蓝色开关，说的是同一件事；底部计数与上方栏重复 |
| Settings › Secrets | "— substituted at run time wherever a credential is used" 每行重复一次；说明 4 句 |
| Settings › System | 说明两句（可接受，收一行） |

### 0.4 规则与代码已经互相矛盾的地方

skill 是宪法，但它有几处已经和代码对不上。本文 P1c 一并修掉：

- §3 说 rail 是 **48px 纯图标**；代码是 **56px 带名字**（docs/39 S1 后记：owner 反转了纯图标）。
- §16 规则 5、8 与 §17 的表引用 `groups.js`、`menu.js`、`js/plugin-palette.js`——源是 `.ts`（docs/36）。
- §15 说 "Inter is honored first"；这台机器（和大多数 Windows 机器）没有 Inter，实际渲染是
  Segoe UI Variable，token 的微调（`cv11`、`ss01`）对它无效。
- 规则 2 的例外只列了代码块语法色；`.db-col-comment` / `.db-det-comment` / `.db-tip .t-comment`
  的绿色是第二个例外，只写在 CSS 注释里。本文**取消**这个例外（§3.7），而不是把它写进 skill。
- 规则 15（选中是竖条 + 浅底，不变蓝）：`.db-table.sel .db-table-name { color: var(--accent) }`
  与 `.db-drow.on { color: var(--accent) }` 违反它。
- §9 说"一个页面应该像 Swiss 里的另一个工具，而不是嵌进来的另一个应用"；Terminal 的独立调色板
  正是后者。

### 0.5 便宜的事实

- `h()` / `fill()`（docs/37 R5）已经是全面板唯一的建节点方式，没有 innerHTML。组件就是返回节点的函数，
  不需要框架、不需要打包器。
- `mountGroup`（`groups.ts`）、`emptyNode`（`util.ts`）、`popupMenu`（`menu.ts`）、`styleSelect`
  （`dropdown.ts`）、`openSheet`（`add-sheet.ts`）、JSON 代码块（`json-view.ts`）已经是组件，只是散在各处、
  类名没有归属。
- 棘轮测试的写法现成：`test/non-null-ratchet.test.ts`（每文件冻结计数，只许减）。
- `admin_asset()`（`crates/swiss-panel/src/admin.rs`）会服务 `admin_assets/` 下任何 `.html`，所以
  `/admin/ui.html` 不需要改一行 Rust。
- 原型（`docs/assets/46/directions.html`）已经把 B 在 13 页上画出来并被 owner 选中，本文的每个视觉决定
  都能在那里按住空格和现状对比。

## 1. 决定

### 1.1 宪法核对

- **Ruthlessly small**：库是**替换**，不是叠加。页面删掉自己的类，`views.css` 只许变小（门禁 G7）。不加依赖、
  不加字体、不加打包器；陈列页随二进制发，估计 < 20 KB（P9 记实数）。
- **Plugin-shaped**：一个新插件页从此由 `ui/` 的组件拼出来，作者不需要读 1,175 行 CSS 才知道"行"长什么样。
  这正是"加一个插件 = 描述符 + 动作 + 页面"在面板侧该有的样子。
- **Hot-pluggable / 三种接入方式**：不涉及。
- 装载规则（loopback、凭据引用、无 ping、懒启动、封装格式）：一条不碰。API 形状一个字段不改（§8）。

### 1.2 ADR

写 **ADR-029**（docs/07）：库在树内、页面只组合不定样式、门禁用棘轮。三条都满足：难以回退（全部页面迁完后
回退就是重写）、没有上下文会意外（"为什么 views.css 里不许写 `.btn`？"）、有真实取舍（见 U1 的表）。

### 1.3 决定清单

| # | 决定 |
| --- | --- |
| **U1** | **库在树内**：`crates/swiss-panel/panel/src/ui/*.ts` + `crates/swiss-panel/src/admin_assets/styles/ui.css`。不拆 npm 包、不拆 crate、不引第三方组件库。 |
| **U2** | **库的边界**：`ui/` 只准 import `../h.js`、`../i18n.js` 和 `./*`。不碰 api、状态、视图。门禁 G1。 |
| **U3** | **CSS 三层**：`base.css` = token + 重置 + 壳（rail、上方栏、侧栏）；`ui.css` = 每个组件的类；`views.css` = 只剩工作区页面（Data、Terminal）的骨架和没有复用形状的局部布局。加载顺序 base → ui → views。`views.css` 不许以 `ui.css` 的类为选择器主体（门禁 G2，棘轮）。 |
| **U4** | **组件清单**见 §2。已有的组件（groups、menu、dropdown、sheet、json-view、empty）**原地保留**，它们的类搬进 `ui.css`，由 `ui/index.ts` 统一导出；新组件写在 `ui/`。 |
| **U5** | **字重四级**：`--w-body 400`、`--w-name 450`、`--w-emph 500`、`--w-title 600`。所有 `font-weight` 用这四个 token（门禁 G3，零容忍）。暗色 `--text` 从 `#ededef` 收到 `#e4e4e7`。 |
| **U6** | **字体**：系统字体栈不变，**不内嵌 Inter**（C 方案不做）。`--sans` 里 Inter 保留在第一位（装了就用），但 skill 与 base.css 的字体说明改成"设计以 Segoe UI Variable / SF 为准"。 |
| **U7** | **少画线**：卡片内行分隔线从文字列开始（inset，System Settings 的做法）；事件列表（§2.4 timeline）不画外框也不画行线，展开的一行才是卡片；Data 网格去竖线；rail 右边线去掉。 |
| **U8** | **细滚动条**：全局 10px、透明轨道、无箭头、圆角滑块；Firefox 走 `scrollbar-width: thin`。 |
| **U9** | **页头固定**：内容页的 `paneHead` 吸顶，滚动后下方出现一条 hairline；MCP 详情的资源头 + seg 吸顶，滚动后收起说明行。 |
| **U10** | **事件列表一个组件**（`timeline`）：Logs、Traffic 活动、Remote Runs、Jobs 运行记录共用。时间单独一列、日期变成按天分组的标题、耗时右对齐一列（≥ 1 s 琥珀色）、连续相同的项合并 ×N、失败是红色标签。 |
| **U11** | **说明一行**：规则 20 从"最多两句"收紧为"**一句，一行**（`--measure` 下不折行）"。逐页的文案在 §3，中英两份。 |
| **U12** | **Terminal 用面板 token**：`--term-*` token 进 base.css 的 token 块，从面板色推出；Terminal 仍然在亮色主题下保持暗色舞台。 |
| **U13** | **陈列页**：`/admin/ui.html` 渲染 `ui/` 的每个组件的每个状态，明暗、中英可切。随二进制发（隐藏路径，不进导航）。 |
| **U14** | **门禁**七道（§4），全部进 `npm run check`。 |
| **U15** | **skill 与库在 P1 内一起落地**（P1c 紧跟 P1b，P2 开始前 skill 已经描述库），skill 不描述还不存在的东西；逐页整理的规则在各页落地时补进 skill 的例子。 |
| **U16** | **每迁一页，删一页的旧 CSS 与旧类**。迁完的页面不许留旧类名做兼容（记忆：refactor means changing the code）。 |
| **U17** | **库先行，设计也用库**（需求第 6 条）。P1 把 13 页要用的形状一次备齐并在陈列页走查通过，之后才迁第一页；迁页时缺一个形状，先停下来补进库（组件 + ui.css + 陈列页 + 测试），再回到页面。以后的设计稿不再像 `docs/assets/46/directions.html` 那样手写一套 `.m-*` 样式：设计 = 陈列页里的一个**场景**（§2.6），用真组件和虚构数据拼出整页，明暗中英开关现成。 |
| **U18** | **回到顶部**（owner 2026-09-23 在 P2 前追加："MCP log 太长了……不管是往下还是往上都太麻烦了"）：一个浮在 pane 右下角的圆形图标按钮，pane 滚过一屏后出现，点一下 pane 平滑回到顶部（`prefers-reduced-motion` 下直接跳）。它是**壳**的：启动时在 `#pane` 上装一个，所有内容页共用；工作区页面（`.pane.full`）从不滚动，所以从不出现。库里是只依赖 DOM 的机制 `toTop(scroller)`（§2.5 的"通用机制"类），陈列页在自己的 pane 上也装一个。 |

U1 的取舍：

| 选项 | 好处 | 代价 | 结论 |
| --- | --- | --- | --- |
| 维持现状（只有 skill） | 零成本 | 规则靠自觉；§0.2 的数字就是结果 | 否 |
| **树内库 + 棘轮门禁** | 零依赖、零构建步骤、和 `h()` 同一种写法；门禁让规则变成测试失败 | 一次性迁移；棘轮数字要随页更新 | **是** |
| 独立 npm 包 / workspace 包 | 边界最硬 | 要打包器或第二套发射；和"cargo build 不需要 node"冲突（ADR-024） | 否 |
| 独立 crate（第二个 rust_embed） | Rust 侧边界清楚 | 面板资源分两处嵌入，版本戳（`panel_version_stamp`）要合并；收益为零 | 否 |
| 第三方组件库 / 框架 | 组件现成 | 违反"no framework, no npm dependency"（skill §15），体积 | 否 |

### 1.4 不变的东西（写明，免得"顺手"）

- L1 / L2 / L3 的归属与形状（rail 座位、上方栏下划线 tab、页内 pill seg、Data 的对象卡片 tab）。
- rail 56px 带名字、`fitRailLabels`、Focus 模式的进出位置、Terminal 的 dock。
- 组件的**行为**：groups 的拖拽与 `+`/`⋯`、menu 的定位与层级、dropdown、sheet 的打开顺序（`hidden = false` 先于 `fill`）。
- 任何 `/api/*` 的形状；任何 `data-*` 钩子的名字（测试与委托监听依赖它们）——迁移时换类名，不换钩子。
- Data 的网格引擎、编辑缓冲、对象 tab 机制（docs/42、43）；Terminal 的会话、录制、快捷键。

## 2. 组件（`panel/src/ui/`）

所有组件是纯函数：参数进，`HTMLElement` 出，用 `h()` 构建，不挂处理函数（交互走视图的委托监听，
钩子是参数里传进来的 `data`）。每个组件：一个 `ui/*.ts` 函数 + `ui.css` 里的类 + 陈列页的一节 +
vitest 用例。下面是 P1 的初始集；签名以实现为准，但**形状**（参数名、槽位）按这里来。

### 2.1 基础

```ts
// ui/button.ts
btn(label: string, o?: { kind?: "primary" | "ghost" | "danger"; icon?: string; id?: string;
    data?: AttrMap; title?: string; disabled?: boolean }): HTMLButtonElement
iconBtn(icon: string, label: string, o?: { id?: string; data?: AttrMap; title?: string;
    pressed?: boolean; ghost?: boolean; disabled?: boolean }): HTMLButtonElement   // aria-label = label, always
moreBtn(label: string, o?: { id?: string; data?: AttrMap }): HTMLButtonElement   // the ⋯

// ui/icon.ts — iconNode moves here; util.ts keeps a re-export for its import sites
iconNode(name: string, label?: string): SVGSVGElement   // label -> role=img + aria-label; else aria-hidden

// ui/status.ts
dot(state: "up" | "down" | "error" | "idle" | "starting" | "stopping" | "off", title: string): HTMLElement
// "off" is the bare grey .dot - no class for a rule that would only repeat the base one
tag(text: string, o?: { tone?: "bad" | "warn"; mono?: boolean; title?: string }): HTMLElement
// tone is STATE only (a non-zero exit, a failed run) — rule 2; descriptive tags are monochrome

// ui/switch.ts
sw(on: boolean, label: string, o?: { id?: string; data?: AttrMap; title?: string; disabled?: boolean }): HTMLButtonElement
```

### 2.2 页面骨架

```ts
// ui/page.ts
paneHead(o: { title?: HChild; desc?: HChild; sub?: HChild; actions?: HChild[] }): HTMLElement
// sticky; the shell toggles .pane.scrolled from ONE scroll listener on #pane (P1a).
// title only on a RESOURCE head (the selected MCP): a content page's location is the bar's (skill §7)
section(o: { cap?: string; tools?: HChild[] }, ...body: HChild[]): HTMLElement   // .sec-head + body
card(...rows: HChild[]): HTMLElement                                          // the .group surface
pageFoot(o: { note?: HChild; rev?: string }): HTMLElement   // prose left, the revision (mono) right;
                                              // never a count the bar already shows
inlineForm(...controls: HChild[]): HTMLElement
emptyNode(o: { icon: string; title: string; hint?: string; action?: string }): HTMLElement  // moves from util.ts

// P2 additions (§3.2)
resHead(o: { title: HChild; desc?: HChild; sub?: HChild; actions?: HChild[]; nav?: HTMLElement }): HChild[]
// the RESOURCE head as two pinned layers: [.pane-head.res (title + actions), .res-meta (desc + sub,
// scrolls away beneath it), .pane-nav (the seg, pins under the title row)]
// ui/to-top.ts - a DOM-only mechanism (§2.5)
toTop(scroller: HTMLElement): HTMLButtonElement   // shown past one screen; click scrolls to the top
```

### 2.3 行

```ts
// ui/row.ts — the ONE list row (replaces .row / .tun-row / the remote row)
row(o: {
  lead?: HChild;           // a dot() or an icon — the leading column
  name: HChild;            // identity, sans, --w-name
  sub?: HChild;            // one line, --f-label, --text-2; mono only when it is a value (rule 1)
  err?: string;            // replaces sub: one red line, ellipsized, full text in title
  cols?: Array<HChild | { v: HChild; mono?: boolean; title?: string }>;
                           // right-aligned value columns (a port, a rule count, a last run)
  toggle?: HTMLElement;    // a sw()
  primary?: HTMLButtonElement;   // at most ONE non-icon button (rule 4, by type)
  more?: HTMLButtonElement;      // the ⋯
  data?: AttrMap; draggable?: boolean; muted?: boolean; title?: string;
}): HTMLElement
kvRow(label: string, value: HChild, o?: { mono?: boolean; title?: string }): HTMLElement   // config / system pairs
```

inset 分隔线的位置由行首列决定：有 `lead` 时从名字那一列开始（`--row-inset`），与 groups 的对齐契约
（base.css 注释里的 x 坐标表）一致。

### 2.4 事件列表

```ts
// ui/timeline.ts
interface TimelineItem {
  id: string;              // stable key: the open-state and delegated hooks use it
  at: number;              // epoch ms
  title: HChild;           // tool / command, mono (a value you would type)
  arg?: string;            // one line of arguments, ellipsized
  who?: string;            // client / target, only where it varies (Traffic, Runs)
  ms?: number;             // duration; >= slowMs renders amber
  status?: { text: string; tone: "bad" | "warn" };   // a failure is a red tag, not a red row
  same?: string;           // collapse signature: consecutive equal signatures fold into ×N
  data?: AttrMap;
}
timeline(items: TimelineItem[], o: {
  open?: Set<string>;
  body?: (it: TimelineItem, run: TimelineItem[]) => HChild;   // the view owns the expanded body;
                                                              // run = every item a ×N row stands for
  slowMs?: number; dayHeads?: boolean; now?: number;
  showWho?: boolean;                                          // overrides "only when it varies"
}): HTMLElement
timelineToggle(root: HTMLElement, id: string, body?: HChild): boolean   // pure DOM; the new state
dayLabel(at: number, now: number): string    // "Today" / "Yesterday" / locale date — tr() keys
timeLabel(at: number): string                // HH:MM:SS, 24-hour
fmtMs(ms: number): string                    // "12 ms" / "1.2 s" / "61 s", cut where rounding lands
collapseRuns(items: TimelineItem[]): TimelineItem[][]   // ×N folding, never across a day
```

一行的列：`[chev] [时间 HH:MM:SS tnum] [title] [arg] [who] [×N] [status] [ms 右对齐]`。

- **按天分组的标题吸顶**，停在固定页头的下沿：壳用一个 `ResizeObserver` 量固定页头的高度，写进
  `#pane` 的 `--pane-head-h`，日期标题 `top: var(--pane-head-h)`。没有固定页头的页面这个变量是 0。
- **每行都一样的元信息不进行**：`via`、字符数进展开区的 meta 行；`who` 只在当前已加载的这一页里
  取值不止一种时才进行（Logs 的客户端通常只有一个，Traffic 的通常有多个）。
- **交互不在组件里**：展开 / 收起、×N 展开由视图的委托监听处理，组件只提供一个纯 DOM 帮手
  `timelineToggle(root, id)`，给定节点改类与 `aria-expanded`，不挂监听。

### 2.5 其他：已有的机制（P1b 修订，见 §9）

初稿写"原地保留，由 `ui/index.ts` 转出"。这和 U2 冲突：`ui/index.ts` 若转出 `../groups.js`，库就 import 了
api 与状态，G1 当场失败，陈列页也没法只靠假数据渲染。改成按"它依赖什么"分三类：

| 类 | 放哪 | 例子 |
| --- | --- | --- |
| **纯标记** | 写进 `ui/`，由 `ui/index.ts` 导出 | `groupNode`（`ui/group.ts`，组的带、两个按钮、body；返回各部件而不是一个节点）、`seg`（`ui/seg.ts`，替掉手写的 `.seg` 与 `.db-tabs`，按钮上的 data 钩子名由页面传入，迁页不改委托监听） |
| **通用机制**：只依赖 DOM 的交互（开 / 关 / 定位 / 键盘） | P1b-2 已搬进 `ui/`；调用点一次全部改指向，原模块不留转出（§9） | `ui/menu.ts`：`popupMenu` / `clampMenuPos` / `closeMenu` / `menuOpen`（menu.ts、pane.ts、ui-state.ts 搬来，行类型从 `types/dom.d.ts` 搬来）；`ui/select.ts`：`styleSelect` / `initSelects`（原 dropdown.ts）；`ui/json-view.ts`：代码块 `jsonCodeNode` 与解析（原 json-view.ts）；`ui/sheet.ts`：`sheet` 框架 / `showSheet` / `closeSheet` / `initSheet` / `openFieldSheet`（原 add-sheet.ts） |
| **绑定应用的行为**：API 写入、拖放、存储、侧栏 | 留在原处，**建在库上** | `mountGroup` / `newGroupFlow`（groups.ts：用 `groupNode` 画，自己接折叠、拖放、+ 与 ⋯、API）、MCP 新增表单、`patchSidebar` |

`ui/index.ts` 因此只转出 `ui/` 自己的模块（G1 同时钉住"`ui/` 下每个模块都由 index 转出"）。

机制的键盘分层（P1b-2 实测定下）：一个层自己处理了的键**不再往外传**，面板的 Escape 链与侧栏方向键
（main.ts，document 冒泡阶段）只收没人处理的键。浮动菜单的方向键与 Escape 停在菜单；下拉列表打开时它的键
在 document 捕获阶段处理，Escape 与方向键停住，Tab 关列表、焦点回到触发器、保留默认动作让焦点继续往后走；
触发器上用来打开列表的键停在触发器；sheet 是模态的——`initSheet` 在 `#sheet` 宿主上拦住里面按下的键，只放
Escape 出去给 main.ts 关 sheet。`showSheet` 先显示宿主再填内容，点背板关闭。MCP 新增表单（`openSheet`）
绑着字段定义、详情与轮询，仍留在 add-sheet.ts，建在 `sheet` / `showSheet` 上。`.ctx-menu`
（Data 的右键菜单）并入 `.menu.float` 不变。新类 `.lrow` / `.kv` / `.tl` 与旧的 `.row` / `.tun-row` / `.call`
并存，旧类随页面迁移删除（U16）。

### 2.6 陈列页（U13）

- `admin_assets/ui.html`：只链 `base.css` + `ui.css`，脚本 `js/ui-gallery.js`（源 `panel/src/ui-gallery.ts`）。
- sprite 只有一份：陈列页启动时 `fetch("/admin/index.html")`，把其中的 `<svg hidden>` 节点移进自己的 body。
- 右上角三个开关：明 / 暗、中 / 英、宽度（1440 / 960）。每个组件一节：名字、它在 skill §17 的词、每个状态。
- 文案走 `tr()`（`gallery.*` 键，中英两份）；示例数据是虚构的。
- **场景**（U17）：组件目录之后是整页场景——内容页（页头 + 内联表单 + 两个组）、资源页（侧栏 + 固定资源头 + seg +
  timeline）、事件页（按天分组的 timeline，含 ×N 与失败）、空状态页。场景只许调用 `ui/` 与 §2.5 的组件，
  不许带自己的 CSS；地址 `/admin/ui.html#scene-<id>`。以后提一个新设计，就是加一个场景。
- 门禁 G6：`ui/index.ts` 的每个导出组件必须在陈列页出现；场景模块不许写 `style=` 或自带类名以外的样式
  （场景里 `h()` 的 `class` 只能是 `base.css` / `ui.css` 定义过的类——壳的 `.shell`、`.sidebar` 在 `base.css`；
  手写 `ui.css` 已有的形状由 G5 管，陈列页与场景都是 0），否则套件失败。视图（明暗、中英、宽度）放在查询串
  `?theme=&lang=&w=` 里，一个视图就是一个链接，切换不写面板自己的偏好。

## 3. 逐页整理（验收）

每页一节，每节的项就是该页 commit 的验收清单。"说明"指 `paneHead` 的 desc，中英两份文案同时改。
每页迁完：该页不再有 `h(…, { class: … })` 手写 `ui.css` 已有的形状；它独有的旧类从 `views.css` 删掉；
棘轮数字（G5、G7）在同一个 commit 里下调。

### 3.1 全局（P1a）

- [x] 字重四级 token，54 处 `font-weight` 全换成 token；暗色 `--text` `#e4e4e7`。按角色而不是按旧数字归类（映射表在
      P1a-2 的 commit 里）；UA 默认的 bold（`b`、`strong`、`h1–h6`、`th`）也收到 `--w-emph`。
- [x] 全局细滚动条（U8）；Terminal 舞台的滚动条规则删掉，改成在 `.term-page` 上重指 `--scroll-thumb` token。
      Data 没有自己的滚动条样式——它只有两条"隐藏横向滚动条"的 tab 条规则，那是有意的，保留。
- [x] 卡片内行分隔线 inset；rail 右边线去掉；Traffic 客户端表（`.cli-row`）按原型去掉行线。
      过渡期：MCP Tools 行的分隔线从 chevron 列开始（16px），P2 用 `row({lead})` 重建时对齐到名字列。
- [x] `paneHead` 吸顶 + `.pane.scrolled` hairline；`#pane` 上一个 scroll 监听（passive），换页时复位
      （`pane-scroll.ts`）。只作用于内容页（`.pane > .wide > .pane-head`）；MCP 资源头在 P2 与 seg 一起固定。
      `--pane-head-h` 随 timeline（§2.4）在 P2 落地。

### 3.2 MCP › Servers（P2）

涉及的文件：`pane.ts`（详情头与 ⋯ 菜单）、`connect.ts`（tab 正文）、`logs.ts`（Logs 与 Tools / Resources / Prompts）、
`run.ts` 与 `run-history.ts`（Run 与调用记录）、`detail.ts`、`polling.ts`、`sidebar.ts`、`add-sheet.ts` 与 `fields.ts`
（新增 / 编辑表单）。迁完后这些文件的 G5 为 0。分三个 commit：**P2-1** 壳（回到顶部、`--pane-head-h`）与资源头，
**P2-2** Logs，**P2-3** 其余 tab、侧栏、表单。缺的形状照 U17 先补进库。

- [x] 回到顶部（U18）：`toTop(#pane)` 由壳在启动时装一次，挂在 `body` 上、`position: fixed`；pane 滚过一屏
      （`scrollTop > clientHeight`）出现，回到一屏以内消失；隐藏时 `visibility: hidden`，不进 Tab 顺序。
      层级低于菜单（30）、sheet 背板（40）、toast（50）。实走时补的两件：它是启动时建的一次性 chrome，
      切换语言靠 `paintChrome` 按 `#toTop` 重写标签（否则中文界面里读作 "Back to top"）；`.pane` 的下内边距
      让出按钮的高度，滚到底时最后一行停在按钮上方，不被它盖住。
- [x] 资源头（P2 实施前修订，原文"`scrollTop > 48` 收起说明与状态行，`< 8` 展开，滞回防抖动"）：改成**两层吸顶**。
      名字 + 动作一行吸顶；说明与状态行照常滚走，从名字行下面穿过；seg 滚到名字行下沿时停住。停下来的样子与原文相同，
      但没有 JS 状态，也不会因为头变矮改变滚动高度（滞回要防的正是这个）。壳用 `ResizeObserver` 量两层的高度，
      写进 `#pane` 的 `--pin-title-h`（seg 停住的位置）与 `--pane-head-h`（按天分组的标题停住的位置，§2.4）。
      库里是 `resHead()`：名字行、说明与状态行、seg 三块，由它一起画。
- [x] MCP 详情用 `.wide` 量度（资源页模板，陈列页的资源场景就是这样）。所有 tab 仍是同一个宽度，换 tab 不改页宽。
- [x] Logs → `timeline`：行内只留时间；日期是按天分组的标题；耗时一列；`via` / `client` / 字符数进展开区的
      meta 行；连续相同的调用（同 tool + 同参数 + 同结果）合并 ×N，展开 ×N 列出每一次。失败是行上的红色标签
      （`failed`）。展开状态按调用记，一个 ×N 行只要有一次被打开就算开着：轮询带来一条相同的新调用、行首的 seq
      变了，正在读的那一行也不会被合上。展开区只在打开时才画（关着的行不建代码块）。×N 按页合并：跨页的一串
      相同调用在两页各显示一段。库里补了 `timelineMeta()`（展开区的 meta 行）、`pager()`、`failNote()`、
      `filterInput()`。
- [x] 展开区：短 JSON（紧凑形式 ≤ 80 字符）一行显示；每块一个可见的复制按钮，"复制原文"进 ⋯。
      **修订 docs/33 C3**（它规定两个复制按钮都可见、JSON 一律缩进展开），在 docs/33 状态头记一笔。
      库里是 `valueBlock()`（标题、说明、工具一行，下面是正文）与 `jsonCodeNode(v, all, { oneLine })`；
      JSON 后面跟着文字（figma 的截图说明）时不压成一行。
- [x] Tools 行、侧栏行的字重按 U5。Tools / Resources / Prompts 换成 `row()`（P2-3a）：名字是 `<code>`（工具名是要敲的值），
      一行说明，行尾是 Try 与对客户端的开关（资源是 Read）。工具与 prompt 的完整记录（完整说明、输入 schema /
      参数）用 `row({ detail })` 原地展开：名字与说明成为原生 `<details>`，行的按钮留在外面，点它们不会展开。
      参数名那一行（加粗必填）不再挂在行上，进了展开的记录（原型 B 的行只有名字、说明、开关）。被关掉的工具是
      自己的一节（"已停用"），不再是卡片里的一条小标题。
- [x] Run 标签页（P2-3b）：表单用库里新加的 `ui/form.ts`（`form`、`field`、`checkField`、`pair`、`formActions`、`hint`，
      沿用 ui.css 里已有的 `.field` / `.fld` / `.check` / `.two` / `.form-actions` / `.hint`）。参数字段由 schema 生成
      （与 Jobs 的动作表单共用 `argFieldsNode`），名字后是必填星号与类型（`field({ meta })`，等宽、灰）。运行结果移到
      卡片下面，是与 Logs 展开区同形的 `valueBlock`：短 JSON 一行、错误是红字、超过 200 行的回复退回纯文本并在块内
      滚动（60vh，原来的输出框也是有界的）。"过往运行"弹层里的参数与结果也换成同一个块；资源内容用库里的 `sheet()`。
- [x] Config 与表单（P2-3c）：编辑表单、新增 sheet、组字段、MCP 的类型字段（`fields.ts`）都用 `ui/form.ts`。只读视图：
      每项设置是 `kvRow`（主机、URL、命令等值用等宽；说明与面板自己的词——"开机启动"、开 / 关——用无衬线，规则 1）；
      类型与徽标是 `tag()`；保存过的版本是 `row()`（恢复是唯一的文字按钮，删除是行尾的垃圾桶图标，规则 4）；隧道依赖是
      带状态点的 `row()` 放在"依赖"一节，连接池可能过期的提示走 `err` 行。插件被关掉时的页面是 `emptyNode`，页面加载中是
      `note({ busy })`（`page-registry.ts`）。走查时发现并修了两个既有的 sheet 排版问题：`.two` 只有在 `.form` 里或是
      `.sheet-body` 的直接子元素时才是两列网格，新增 sheet 里按类型生成的成对字段因此上下叠放——现在 `.two` 自己就是
      两列网格；平铺容器里的字段与成对字段之间没有间距——现在是一条 `:is(.fld, .two) + :is(.fld, .two)` 规则，网格
      容器里归零（由 gap 负责）。
      **`polling.ts` 不在这一步**：它画的是 Tunnels 的连接 / 转发行与 Jobs 的任务行（`tun-row`），随那两页迁移（P4 / P5）。
- [x] P1b 留下的两件：`.seg` 自带的 `margin-bottom` 交给页面的流（组件不带摆放，P2-1 已做）；`sidebar.ts` 的
      `sideRowNode` 换成 `sideRow()`（P2-3a）。（`popupMenu` 改用 `h()` 等到最后一个手写微型 DOM 的套件——Data 的——换成 happy-dom，即 P7。）

### 3.3 MCP › Traffic 与 Token（P3）

- [x] Traffic：去掉页首的 `TRAFFIC` 标题；Actions / Everything 分段挪到 Activity 一节的头上（它筛的是活动）；
      Clear 进 ⋯；活动列表用 `timeline`（`who` = 客户端 · 服务器，只在不同时成列；相同的交互合并 ×N；展开区是 meta 行 +
      请求 / 回复两个 `valueBlock`，打开时才取）。客户端表格留在 `views.css`（页面自己的五列表格），放进 `card()`。
      走查时发现：直接冷加载 `#traffic` 时上方栏一直显示"0 MCP"——只有 Servers 页会加载 MCP 列表；现在 Traffic 页
      挂载和每次轮询都加载它所计数的列表（`views/traffic.ts`，新测试 `admin-traffic-view.test.ts`）。
- [x] Token：说明收一行；New group 变 `folder-plus` 图标按钮（与侧栏同一个图标、同一个意思，规则 7）；
      token id 用等宽（会被复制的值）；行用 `row()`（"复制时使用"是名字旁的 `tag`）；一次性密钥框是表单卡片，紧跟在
      新建表单下面。`tokens.createdWhen` 的译文里不再自带分隔点（分隔符归排版管）。

### 3.4 Tunnels（P4）

- [x] SSH Connections：说明一行；New group 变图标；规则数从副行挪到右侧一列（`cols`）；删掉与上方栏重复的底部计数，
      底部只留 revision——隧道数据没有 revision，所以底部整行删掉。
- [x] Port Forwards：四个按钮收成 `New` + ⋯（Start all / Stop all）+ 新建分组图标；本地端口单独一列；错误改成副行
      一句红字（`err`），每行一样高，完整原因在 title。
- [x] 两个 sheet（P4 顺带）：新建 / 编辑连接与规则都用 `sheet()` + `ui/form.ts`；库里补了 `formCap()`（表单里的分组小标题）、
      `formFold()`（Advanced 折叠）、`field({ action })`（控件旁的一个按钮：私钥路径 + Browse），以及 `stackSheet()`
      ——sheet 上面再叠一层（私钥选择器）：它有自己的背板，Escape 只关它自己（以前会穿到壳的 Escape 链，把下面的 sheet
      关掉、选择器还浮着）。私钥路径那一行和下一个标签贴在一起的问题随 `field()` 的堆叠间距一起修掉。行里的状态点
      `tunDot()`：重连中是琥珀色脉冲；停着的是灰点。`dot(state, null)` 给"已被外层解释过"的点（服务列表里不存在的 MCP）
      ——不带 title，不进辅助技术。库行 `row({ draggable })` 的拖动反馈（`.dragging` / `.drop-before` / `.drop-after`）补进 ui.css——之前只在 `.tun-row`
      上，隧道行换成库行后拖动时看不到落点线。

### 3.5 Settings（P5）

- [x] Plugins：状态与开关一致时**不画点**（开关已经说了开 / 关）；状态异常（failed、waitingDependency、not-built）时画
      红 / 琥珀点，副行是原因。副行 `pages: a, b, c` 收成 "3 pages"（完整列表在 title）；说明一行；底部只留 revision。
      实施：`stateDot()`——开且 active / 开且 idle（插件宿主的懒启动）/ 关，都不画点；进行中是琥珀脉冲，failed 是红点，
      `lastError` 是行的红字（`row({ err })`）。点放在名字后面而不是 lead 列：大多数行没有点，只给一行加 lead 会把它的
      名字挤出对齐。依赖没满足不画点，是副行上的 `tag("no provider", { tone: "warn" })`——静止的琥珀点会被读成启动中的
      脉冲。"· off" 字样删掉（开关已经说了）。轮询原地更新：行和开关是同一个节点，按过的开关不丢焦点。
- [x] Secrets：删掉每行重复的那句；说明一行；Copy ref 变成带 `copy` 图标的 ghost 按钮。New group 变 `folder-plus` 图标，
      创建表单是 `inlineForm()`。ui.css `.inline-form > input.grow` 随它最后一个使用者删掉（960 宽时两个输入框各约
      320px，最长的占位符放得下）。
- [x] System：说明一行；行用 `kvRow` / `row`。实施：一行 `row()`；退出确认改用 `sheet()` + `showSheet()`。Quit 按钮
      按原型保留红色——这页唯一的操作就是这个，没有 ⋯ 可以收，确认在它打开的 sheet 里。

### 3.6 Jobs 与 Remote（P6）

- [x] Jobs：说明一行；删掉 `SCHEDULED COMMANDS` 标题；三个按钮收成 `New` + ⋯（New (advanced)）+ 新建分组图标；
      cron 翻成人话（"Daily 03:00"、"Every 15 min"，悬停看原始 cron；翻不了的原样显示）——**复用** sheet 里
      已有的 `describeCron`（`jobs.ts:374`），不写第二个翻译器；下次运行用相对时间（"in 3 h"），上次运行也用
      相对时间，基于 `whenLabel`（`util.ts:123`）扩出未来方向；上次失败才标红（`tag` tone bad）；停用的 job 是一个 `Off` 标签而不是整行变灰。
      运行记录（sheet 里的 `.call`）用 `timeline`。
      实施：行是 `row()`，行名后面是 `Off` / `#label` 标签；副行是命令（mono，有 v2 title 时前面加 id）；右边三列
      用库里新加的定宽列 `row({ cols: [{ w }] })`（s / m / l，按网格 token 算宽，超长省略号，完整值在 title），
      上下对齐：计划（`schedFromJob` → `schedToBody().say`，就是 sheet 里那句话；cronstrue 没加载或说不出时显示原样，
      title 永远是原样）、下次运行、上次运行（成功 "OK · 8 hr. ago"，失败是红 `tag`，没跑过 "Never run"）。
      相对时间是库里新加的 `relTime()`（`Intl.RelativeTimeFormat`，short，numeric auto：词和复数由语言自己给，
      不加词条），不是从 `whenLabel` 扩出来的——`whenLabel` 仍给 title 里的绝对时间。行首不画点：job 没有开关可对照，
      绿色"已排期"点只是在重复"下次运行"那一列；运行中才在名字后画琥珀脉冲点。原来关掉的 job 的点 title 是
      "idle — starts on first request"，那是 MCP 懒启动的话。运行记录 sheet 是 `sheet()` + `timeline()`：
      标题是触发方式，参数列是输出第一行，失败是红 tag（有退出码时就是 "exit 1"），skipped 之类不算运行的是琥珀 tag、
      不画耗时；相同的连续运行折成 ×N；展开是 meta 行 + Output 值块（`readableBody`，与 Logs / Traffic 同一个）。
      两个编辑 sheet 顺带迁到 `sheet()` + `ui/form.ts`：库里补了 `field({ group })`——一个标题下有好几个控件时
      （星期按钮、retry-on 的两个勾选）用 `role=group` 而不是 `<label>`：`<label>` 会把标题上的点击交给里面第一个
      可标注元素，"Days" 会按下周日。v1 sheet 原来 Environment 标题直接压在 Options 上（空节，环境变量归到了
      Options 下），现在各标题管各自的字段。v2 sheet 的 "sheet wide" 是一个从没有样式定义过的类，删掉；
      "first firing" 那一格其实只是一句说明，改成 trigger 字段的 hint。`.sheet-cap` 随最后一个使用者删掉。
      走查（960 暗色）发现：一次运行的输出很长时，运行记录的行和 Output 块比 sheet 还宽，在 sheet 边上被截断——
      `.sheet-body` 与 `.tl` 都是隐式 `auto` 列的 grid，auto 列不会窄过子项的 min-content（不换行的摘要行全长）。
      两者都改成 `minmax(0, 1fr)`，任何 sheet、任何页面上的时间线都受益（Logs / Traffic 走查过）。
      schedule 分段的英文标签原来是小写 id（"interval"），改成与其它分段一致的首字母大写。
- [x] Remote › Targets：说明一行；New group 变图标；加一列"最近一次运行"（相对时间 + 退出码 tag），数据取现有的
      runs 接口，**不加 API**；接口给不出时这一列不画。
      实施：页头是 `paneHead`（说明一行 + 副行"N 个端点由隧道提供服务" + [新建分组图标, Add target]）；行是 `row()`，
      副行是 标签 · 端点 · mono 根目录 · 能力。行首原来的点是端点状态（实心 = 已连接、空心 = 空闲），可"空闲"就是隧道
      按需才连的正常状态，每行一个空心点只是在重复"一切正常"——改成与 Settings 同一条规则：只有状态不对时才画，
      连接中是琥珀点、出错是红点，title 说状态。"最近一次运行"一列（`cols` 的 l 宽）：没有运行 "No runs"；
      进行中 / 排队是琥珀脉冲点 + "running" / "queued"；结束的是相对时间，失败前面加红 `tag`（"exit 101"、"timed out"），
      title 是 "Last run: 完整时间 · 命令"。数据：`/api/remote/runs` 一页 + 进行中的运行按目标折叠；一页没覆盖到的目标
      （`nextBefore` 还有下一页时）再用 `?limit=1&target=` 各问一次，每次进页面只问一次；接口报错时整列不画、不弹 toast。
      轮询时行结构没变就只替换变了的那一格（行节点、⋯ 菜单、拖拽都不动），列数对不上才整体重画。
      新建 / 编辑 sheet 迁到 `sheet()` + `ui/form.ts`：第一个字段叫 "Alias"，hint 是 "命令调用所用的名字：
      swiss remote exec <alias>"；能力是 `field({ group })` 下的三个勾选。
      走查发现：**编辑目标一保存就报 "target.id is required"**——更新路由要求 body 里带 `id`（它拒绝改名），
      面板编辑时从来不发 `id`，旧测试把这个 body 钉住了。先写 RED 测试，再改成总是发 `id`；`RemoteTargetBody.id`
      在类型上改成必填。
- [x] Remote › Runs：`timeline`（`who` = 目标 · 来源）；非 0 退出码是红色 tag；Clear 进 ⋯。
      实施：标题是动作（exec / sync / pull / cat / write），参数列是命令（sync 是 "源 → 目标"）；失败类（exit N、
      canceled、timed out、failed）一律红 tag，与 Jobs 同一规则；相同的连续运行折成 ×N。库里 `timeline` 补了
      `live: { text, queued }`：进行中的项在耗时列的位置画琥珀脉冲点 + "running" / "queued"（`.tl-live`），gallery 有一条。
      展开：meta 行（#id · 目标 · 来源 · mono cwd · 状态 · N 次相同的运行）+ Output 值块（`readableBody`，失败时红色；
      截断时说 "a of b" 并有 Load more）；输出被截断过的再加一个 Tail 值块；进行中的是 Live output 值块，带 Cancel，
      随拉取刷新。页头是 `paneHead`：副行是保留策略那句（"已记录 N 次运行 · 大小 上限 … · 保留 30 天 …"），
      动作是 [目标筛选 select, ⋯（Clear，红色）]；翻页用 `pager()`。
      走查发现：第一版把筛选和 ⋯ 放在列表上方一个没有标题的 section head 里，页头下面又多出一行只装按钮的条——
      先写 RED 测试（页头里没有 `.sec-head`，actions 就是 [select, ⋯]），再挪进 `paneHead`。
      CSS：`.call*` 整块、旧 `.row` 家族（`.row-main` / `.row-sel` / `.rowmsg*` / `.row-act*` / `.k` / `.v`）、`.chev`、
      Remote 两页各自的块随最后的使用者删掉；views.css 从 67801 降到 64049 字节。

### 3.7 Data（P7）

- [x] 列注释（表头第二行、Columns tab 的 Comment 列、表头悬停卡）从 `--green` 改成 `--text-3` / `--text-2`；
      删掉 CSS 里自称例外的注释。
      实施：表头第二行 `.db-col-comment` 用 `--text-3`；悬停卡的 `.t-comment` 与 Columns tab 的 Comment 列用 `--text-2`
      （正文里的说明比表头下的小字深一级）。
      2026-09-28 补记：类型也另起一行（负责人："我 id 其实很短，这个类型应该换行展示，如果有注释，还要再换行"）。
      表头现在是三行：名字（带 PK / FK / 排序标记）、`.db-col-type` 类型、`.db-col-comment` 注释；列宽取最长的
      一行，不再是名字和类型并排的宽度。`.db-col-type` 去掉 6px 左缩进，用正文字重，320px 封顶加省略号（MySQL
      的长 enum）；行表单的类型行是同一个类，一并贴齐名字。真浏览器走查又发现：没有注释的列那一行是空 div，
      高度为 0，表头居中后名字比邻列低半行；`.db-col-comment:empty::before` 放一个不换行空格，空行也占一行，
      各列名字对齐（走查量得名字 / 类型 / 注释三行在每列都是同一高度）。测试：`data-look.test.ts` 的
      "a column header stacks its name, its type and its comment"。
- [x] 表列表选中：竖条 + 浅底 + `--w-emph`，不变蓝（规则 15）；抽屉里的当前库 / 连接同样处理（勾号列代替蓝字）。
      实施：`.db-table.sel` 是 8% accent 浅底 + 2px inset accent 竖条，名字 `--text` / `--w-emph`。连接与库的抽屉每行
      最前一列是勾号（`.db-drow-tick`，当前项放 accent 的 `i-check`，其余行留同宽的空位，名字对齐），当前行加粗。
      连接行的方言：有图标的是裸图标（`.db-row-mark`，`--text-3`，悬停 `--text-2`），没有图标的（sqlite）是
      `tag({ mono })`。
- [x] 网格去竖线；行高 +4px；行首复选框与删除按钮只在悬停该行或已勾选时出现（键盘焦点在行内时也出现）。
      实施：`th` / `td` 只留横线；`td` 上下各 6px（表头 4px，所以行比表头高 4px）。`.db-selbox` / `.db-act` 在行没有
      hover、没有 focus-within、没勾选、也没有暂存的删除 / 插入时是 `opacity: 0`，仍占位、仍能 Tab 到；表头的全选框
      一直在。表头悬停时列宽把手显出 `--sep`，指到把手本身才是 accent。
      走查发现（Redis 值表）：暂存的插入行从来没有移除按钮——插入行传的是"不可删"，派发器里 `data-rins` 那一支永远
      走不到，空行塌成 13px 的一条绿，只能 Discard 全部；改成插入行总有 ×（与 SQL 网格的 `data-irm` 同形，list 也有）。
      field / value 表头借 `db-rowctl` 表示"不排序"，于是每列都拿到控制列的 58px，控制列被撑到表宽的三分之一；改用
      `db-nosort`。两处都先写 RED 测试。
- [x] `.db-tabs` 换成 `seg()`；`.ctx-menu` 换成 `.menu.float`；`.db-chip` / `.db-keytype` 换成 `tag()`。
      实施：表 tab 的 Data / Form / Structure / DDL、Structure 下的 Columns / Indexes / Foreign keys、脚本的结果 tab
      都是 `seg()`（结果 tab 一多就横向滚，不画滚动条）。单元格、结果单元格、Table、CSV 的菜单全部走 `popupMenu`
      （`#menu.menu.float`，`role=menu`，打开时第一项得焦点，Escape 关）；`.ctx-menu*`、`.db-tabs*`、`.db-chip*`、
      `.db-keytype` 整块删掉。Activity 里本面板自己的会话是 `tag()`（"this panel"，title 说明）。
      走查发现：① `dbSyncKind` 还要求 `#dbSqlExplain` 存在，而 docs/43 M4 把 Explain / Format 收进溢出菜单后这个元素
      没了：早退每次都触发，pane 的 ⋯ 从没显示过，Redis 的键搜索框写着 "Filter tables"。② 那次折叠还丢了"Redis 命令
      没有 Explain / Format"。③ 修好 ① 后表头出现两个挨着的 ⋯（对象的与 pane 的），打开两个不同的菜单——改成一个头
      一个 ⋯：SQL 连接上对象的 ⋯ 末尾是分隔线 + Activity…，pane 的 ⋯ 只在没有打开对象时出现。④ Redis 值视图的 meta
      行还留着 M4 之前的 "+ Field" 与 ⋯，那个 "+ Field" 丢了 `data-radd`，真点击什么也不做——meta 行只留事实，动作只在
      表头。四处都先写 RED 测试。
- [x] 状态栏里与表头重复的可编辑说明去掉（实施时截图确认是哪一句，写进 commit）。
      实施：去掉的是 `renderDbStatus` 里的可编辑说明——"editable — changes buffer until Commit"，或服务端给的只读
      原因——表头的 meta 行已经说过一遍。状态栏只留分页、每页行数、连接。
      P7-1 数字：G4 views.css 字面值降到 222；G5 `data-browsers.ts` 10 → 8；G7 views.css 64049 → 63479 字节。
- [x] （P7-2）Data 的 sheet（data-cell / csv / ddl / value / browsers）迁 `sheet()` + `ui/form.ts`，按钮迁 `btn()` /
      `iconBtn()`，`pane-title` 换成 views.css 自己的类，`db-tab-dot` / `hint` 走库；G5 的 data-* 各行归零。
      实施：五个 sheet 全部是 `showSheet(sheet(...))`：单元格编辑与值查看器的头是 列名 + `sub`（所在的表与行）；
      CSV 导入的模式切换是 `seg()`，映射与预览在空的时候 `hidden`（sheet-body 是 grid，空盒子也占两道间距）；
      DDL 三个 sheet 用 `field` / `pair` / `checkField` / `field({ group })`；Redis 的重命名换成库里的
      `openFieldSheet`——名字为空时就地提示，RENAME 被拒时 sheet 留着、输入的名字还在（旧的手画 sheet 先关再发命令，
      被拒就丢了）。按钮全部是 `btn()` / `iconBtn()` / `moreBtn()`：对象头的 ⋯ 与 pane 的 ⋯ 跟其它页一样是 ghost；
      提交栏的 Commit 是 primary（`.db-bar .btn.commit` 删掉）；流的 pill 与 gap 用 data 钩子（`data-stream`）代替
      没有样式的类；"+ 筛选" 与 "+ 行" 同尺寸（`.db-filter-add` 的 12px 删掉）。对象头的标题只用 `.db-title`
      （补上 `--w-title`）；页签上的暂存圆点是 `heldDot()`。
      库里新增：`sheet({ sub })`（mono 的值，标题基线上，放不下就换行）、`heldDot(title | null)`、`iconBtn` / `moreBtn`
      的 `hidden`；`moreBtn` 自带 `aria-haspopup="menu"`（每个 ⋯ 都开菜单）。
      走查发现：① 禁用的 primary 看不见——`.btn:disabled` 把底色换成 `--card`，primary 的白字落在白底上，DDL sheet
      在输入名字之前只剩一个 Cancel；库里加 `.btn.primary:disabled`（保留 accent 底色、变淡），陈列页加了一行。
      ② pair 里的勾选框与左边字段的标题齐平，而不是与输入框齐平（索引的 Unique、Jobs 的 Disabled 都是）；库里让
      pair 中的 check 字段底对齐、抬起输入框自身的内边距与边框，居中在输入框上。③ 单元格编辑头写 "PK" 却印出整行：
      缓冲行的地址有意带上每一列的原值（docs/22 W4.2，乐观锁），有主键的表现在只显示主键列。④ Form 页记录动作还是
      Unicode ✕ / ↩，改成与网格相同的 `i-x` / `i-undo`。⑤ CSV 文本框清空后旧的预览不消失。①–④ 先写 RED 测试。
      G5 的 data-* 13 行（87 个 token）全部归零；因为树里只剩 19 个，G5 的"不是瞎数"检查改成数一段固定源码，
      不再要求全树 > 50（P9 时全树应为 0）。G4 views.css 222 → 220；G7 views.css 63479 → 63072 字节。
- [x] （P7-3）手写微型 DOM 的四个套件（admin-revisions、admin-row-menu、admin-data-redis-cellmenu、
      admin-logs-pagination）换成 happy-dom，`popupMenu` 改用 `h()` 构建、`wireMenu` 的 typeof 防护一起删（见偏差表
      P1b-2 那一行）。P7-2 试过：`h()` 版的菜单只让这四个套件挂（它们的桩不从文本子节点算 `textContent`），
      happy-dom 的套件全绿。
      实施：菜单的每一行由 `h()` 画（字形、字、下一级的箭头、暂存圆点 `heldDot(null)`，顺序不变），`wireMenu` 不再
      判断 DOM 方法在不在。admin-revisions、admin-row-menu、admin-data-redis-cellmenu 三个换成 happy-dom：真的
      contextmenu / click 事件，真的冒泡——row-menu 那个回归（打开菜单的点击冒到 document 又把菜单关掉）用变异验证过
      仍然能抓到。与原计划不同：admin-logs-pagination 保留它的微型 DOM——它的 40 个用例数的是 FakeNode 上的重绘
      次数、错误条与 focus 调用，真 DOM 不记这些；它的桩改成像真 DOM 一样从子节点算 `textContent`（admin-data-grid-focus
      的桩同样处理）。

### 3.8 Terminal（P8）

- [x] `.term-page` 的独立色板删掉，改用 `--term-*` token（U12）：暗色舞台比 `--bg` 深一级（`#0b0c0e`），
      亮色主题下舞台 `#17181b`；按钮用面板 accent；状态点用面板的 green / amber / red。
      实施（P8-2）：views.css 里整套 `--t-*`（9 个定义 + 58 处引用）删掉；`.term-page` 上把面板 token 重指向 `--term-*`
      （`--bg`/`--bar`/`--card`/`--field`/`--text*`/`--sep*`/`--hover`），`--accent` 与 green/amber/red 保持面板自己的——
      库控件（objTab、下拉脸、btn、iconBtn）落进 bar/foot 就自动落在舞台上，页面不再自绘深色。术语映射：
      `--t-accent`→`--accent`、`--t-ok/warn/bad`→`--green/--amber/--red`、其余按同名后缀。
- [x] 会话 tab 与 Data 的对象 tab 同一种形状（顶部 2px accent）。
      实施（P8-1，Data 那一半）：库里新增 `objTab`（`ui/tab.ts` + `ui.css .otab*`），Data 的对象 tab 条迁上去——选中卡顶部 2px accent、
      与下方表面同底合并；关闭是一个真 `button.otab-close`（角色上不再是 tab 里套一个 span[role=button]），未选中时 hover 才出现；
      过滤计数是 `.otab-n`，暂存写入圆点是 `heldDot()` 带 aria-label；焦点环画在卡内部（棘轮式的 inset box-shadow），滚动条裁不掉。
      旧的 `.db-tab` 一块随迁移删掉（`.db-tab-rename` / `-add` / `-more` 留在 views.css，是页面自己的部分）；`tnum` 成为 base.css 的
      工具类，views.css 里两处写错的 `font-variant-numeric: tnum`（不是合法值，浏览器整条丢弃）改成 `tabular-nums`。
      实施（P8-2，Terminal 这一半）：会话 tab 迁上 `objTab`——button 里套 span 的关闭位、Unicode `×` 与铃 `●` 一并退役，
      铃是 `heldDot()` 的 CSS 圆点（带 aria-label，选中该页签即应答）；条容器 `.term-tabs` 只做底边对齐，
      委托事件改答 objTab 的 data 钩子（`data-term` 选页签、`data-termx` 关闭，close 先判）；改名输入替换 `.otab-name`。
- [x] 目标选择器用面板的 dropdown（`styleSelect`），不再是深色原生 select。
      实施（P8-2）：picker 就是普通 `h("select")`，`initSelects` 的观察器自动给它 `.dd` 脸；页面重指向的 token 让它
      直接坐在深色 bar 上，`.term-pick` 一块（含 focus 环）删除。
- [x] xterm 的 `theme.background` / `cursor` / `selection` 从 `--term-*` 读（挂载时 + 主题切换时），ANSI 16 色不动
      （那是内容，不是 chrome）。
      实施（P8-2）：`termTheme()`/`readTermTokens()`/`applyTermTheme()` 移入 terminal-core（reader 注入，单测钉住
      chrome/内容之split与空 token 回退）；terminal.ts 在 render() 里挂 `MutationObserver` 盯 `data-theme`，
      翻转即对每个打开的终端重设 `options.theme`（unmount 断开）；`selectionBackground` 用 `--term-sel`。

### 3.9 收尾（P9）

- [x] 棘轮收到地板：G2 `FROZEN_VIOLATIONS = 0`、G5 表清空、G4 `views.css` 冻结 159、G7 冻结 60,001 B。
- [x] G2 把 `svg`/`use` 元素主语算作 `.ic`（`iconByElement`，单测钉住）。原因：P9 第一遍把 `.x .ic { … }` 改写成
      `.x svg { … }`、`.x .btn` 改写成 `.x button`，门绿了，规则一条没少——类名换成元素名就绕开了门。重做：
      - 图标尺寸与颜色是库的旋钮：`ui.css .ic` 读 `--ic`（尺寸）与 `--ic-ink`（颜色），宿主在**自己的**元素上设，
        views.css 不再有一条以 `svg` 为主语的规则（config 折叠箭头、keylist、Data 抽屉行与勾、tab 条、主键/外键/行操作标、
        终端跳底胶囊、命令面板行与钉）。
      - Data 提交栏：按钮不再各自 `margin-left:auto`，摘要是 `.db-bar-sum { flex: 1 }`，按钮自然聚在栏尾。
      - DDL 列表格的删行按钮改 `iconBtn({ ghost: true })`，删掉 `.db-ddl-grid tbody button` 的尺寸覆盖。
      - Jobs 计划 seg 撑满控件位用库的 `seg({ fill: true })`（`.seg.fill`，陈列页有一条），删掉 `.sched .seg button`。
      - 两个下拉的宽度落在 select 自己的类上（`db-fsel`、`db-csel`，`ui/select.ts` 把 ownClasses 带到触发器）。
- [x] views.css 残留删除（每条都核过是死规则或与库重复）：`.hist-head .dot`、`.serves .dot`、`.db-console-row .hint`、
      `.db-data-ctl`、`.db-inline-edit.null-on`、`.db-ddl-colpick label`、`.db-headrow button`、过时的铃注释。
- [x] Jobs 不可用态改用 `emptyNode({ icon: "clock", … })`（`test/jobs-unavailable.test.ts`，旧实现下红）。
- [x] zh 文案：57 个值里的半角逗号改全角；新门 `test/i18n-zh-style.test.ts`（逗号挨着汉字或打头即红）。
- [x] 截图入 `docs/assets/46/`（见文首）。
- [x] 走查抓到的两个 bug，各带先红后绿的测试：
      - 终端跳底胶囊点不中：xterm 的链接层（内联 `z-index: 2`）盖在它上面，命中测试落到 xterm。`.term-jump` 提到 `z-index: 3`
        （仍在 IME 的 5 之下），`test/admin-terminal.test.ts` 读 views.css 钉住 > 2。
      - 上方栏会话计数：中文面板上读作「1 live」（`countText()` 拼了英文，L10b 扫描器不看返回值），且新开/关闭会话后不刷新
        （只有别的页的轮询顺手重画它）。改 `trn("terminal.nLive.*")`，`reload()` 与 `poll()` 末尾都写一次。
- 未验证（照实写）：
  - 跳底胶囊的出现靠脚本派发的 `WheelEvent`（agent-browser 的滚轮滚不动 xterm）；点击本身是真实指针事件。
  - Jobs 不可用态在 19996 上到不了（它带 jobs 子系统），只有单测覆盖。
  - L10b 扫描器不查函数返回值里的裸字面量——跟进项，本阶段没改。
  - Data 页离开再回来会落回演示缓存态：P9 之前就有，未查。

## 4. 门禁（全部在 `crates/swiss-panel/panel/test/`，进 `npm run check`）

| # | 测试文件 | 断言 | 形式 |
| --- | --- | --- | --- |
| G1 | `ui-boundary.test.ts` | `src/ui/**` 只 import `../h.js`、`../i18n.js`、`./*` | 零容忍 |
| G2 | `ui-css-ownership.test.ts` | `views.css` 的规则不以 `ui.css` 定义的类为选择器主体 | 棘轮（P1 冻结，逐页下调，P9 目标 0） |
| G3 | `css-weights.test.ts` | 三个 CSS 文件的每个 `font-weight` 都是 `var(--w-*)` | 零容忍 |
| G4 | `css-literals.test.ts` | token 块之外的 px / hex / rgb 字面值计数（`0`、`1px`、`2px`、`-1px` 与百分比不计） | 每文件棘轮；`ui.css` 目标 0 |
| G5 | `ui-class-ratchet.test.ts` | 每个 `src/**/*.ts`（`ui/` 除外）里 `h()` 的 `class` 字面值与 `el()` 的类参数中，**属于 `ui.css` 的类名**个数——库的形状只该由库画 | 每文件棘轮；迁完的视图目标 0 |
| G6 | `ui-gallery.test.ts` | `ui/index.ts` 导出的每个组件都在陈列页渲染；陈列页在 happy-dom 里无异常 | 零容忍 |
| G7 | `css-size.test.ts` | `views.css` 字节数 ≤ 冻结值 | 棘轮 |

棘轮的约定照 `non-null-ratchet.test.ts`：冻结表写在测试文件里，数字只许变小，变小时在同一个 commit 里
把表改成新值；新文件默认 0。

## 5. 交付：P0 → P9

一项一个（或几个）commit，顺序即依赖。每个阶段：`npm run check` 绿 → 19998 真浏览器走查（§7）→ commit。

| 阶段 | 内容 | 依赖 |
| --- | --- | --- |
| **P0** | 本文、`46-…-prompt.md`、ADR-029、README 文档表一行、`docs/assets/46/directions.html` | — |
| **P1a-1** | `ui.css` 从 base / views 里**搬**出组件规则，一个值都不改。搬动会改变层叠顺序（base → ui → views），所以这一步单独成 commit，验收是**像素不变**：搬之前和之后，MCP / Servers、Token、Port Forwards、Data、Terminal 五页在明暗两套下截图逐像素比对 | P0 |
| **P1a-2** | token（U5、U8、U12 的 `--term-*`）；§3.1 全局四项；门禁 G2、G3、G4、G7 | P1a-1 |
| **P1b** | `ui/` 组件（§2.1–2.5，13 页要用的形状一次备齐，U17）+ 陈列页与四个场景 + 门禁 G1、G5、G6；陈列页在 19998 上明暗中英走查通过后才进 P2 | P1a-2 |
| **P1c** | skill 改写（§6）+ `.agents/docs/style-design.md` 清单更新 | P1b |
| **P2** | MCP › Servers（§3.2） | P1 |
| **P3** | MCP › Traffic、Token（§3.3） | P2（timeline 在 P2 落地并被 Logs 验过） |
| **P4** | Tunnels（§3.4） | P1 |
| **P5** | Settings（§3.5） | P1 |
| **P6** | Jobs、Remote（§3.6） | P2 |
| **P7** | Data（§3.7） | P1 |
| **P8** | Terminal（§3.8） | P1 |
| **P9** | 收尾：棘轮收到目标值、`views.css` 里迁完页面的残留清零、数字（§7）写进状态头、截图进 `docs/assets/46/`、README 状态、docs/33 状态头的修订记录；zh 字典里约 50 处句中半角逗号改全角（docs/38 的译文风格，P7 走查发现） | 全部 |

## 6. skill 改写（P1c，`.agents/skills/swiss-ui-design/SKILL.md`）

- **新 §0（放在最前）**：UI 库是唯一的画法。页面组合 `ui/` 的组件；新形状先进库（`ui/*.ts` + `ui.css` + 陈列页 + 测试），
  再给页面用；`views.css` 不许重定义库的类。设计稿 = 陈列页的一个场景，不手写样式（U17）。附七道门禁的一句话说明。
- **§3**：rail 改回事实——56px、带名字、`fitRailLabels` 统一字号；纯图标的说法删掉。
- **§9 视觉语言**：加入 U5（四级字重）、U7（少画线）、U8（滚动条）、U9（页头固定）。
- **§12 实施纪律**：第 2、4 条改成"先看 `ui/index.ts` 和陈列页"；加"迁一页删一页"（U16）。
- **§15**：文件布局加 `ui.css`、`ui/`、`ui.html`；字体说明按 U6 改；token 名单加 `--w-*`、`--term-*`。
- **§16 规则**：规则 2 明确"列注释是灰色"；规则 20 收紧为一句一行（U11）；新增规则 21 字重四级、22 线
  （inset 分隔、事件列表无框、网格无竖线）、23 页头固定、24 事件列表共用 timeline（时间列、按天分组、
  耗时列、×N、失败是标签）、25 一屏不重复（上方栏已有的计数、每行都一样的元信息、与标签同名的标题）。
- **§17 词汇表**：每个词对到 `ui/` 的函数与 `ui.css` 的类；加 timeline、kvRow、tag、pageFoot、陈列页；
  `js/*.js` 路径改成 `.ts`。
- **§18 检查清单**：加七道门禁的命令与"陈列页明暗两张截图"。
- **§19 解剖图**：页头固定、inset 分隔、New group 是 `folder-plus` 图标。

本地未跟踪的副本 `.claude/skills/swiss-ui-design/SKILL.md`（主检出，gitignored）在合并后同步一次。

## 7. 验收与门禁命令

```
# 面板（crates/swiss-panel/panel，第一次先 npm ci）
npm run check          # typecheck ×2 + lint + build:check + vitest（含 G1–G7）

# Rust（worktree 根；--workspace 不可省）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d -e normal,build   # 本文不加依赖，回归守卫
```

实机（proof-of-life，每个阶段，19998）：先 `npm run build`，再 `touch crates/swiss-panel/src/lib.rs`，
`CARGO_TARGET_DIR=target-test` 构建 release，`scripts/test-instance.ps1 -Stop` / `-Fresh`。agent-browser 真实指针事件，
冷加载，1440 与 960 两个宽度、明暗两套、中英各走一遍；每页走到的清单写进 commit。19998 被别的会话占用时
用同一脚本的其他端口（docs/39 用过 19997），并在 commit 里写明。**走不了的流程如实列为未验证并写原因。**

要记录的数字（P9 写进状态头）：`views.css` / `ui.css` / `base.css` 字节与类数；`font-weight` 取值种数（9 → 4）；
`views.css` px 字面值（247 → ?）；自有发射 JS 字节；`swiss.exe` 字节；陈列页字节。

## 8. 不做什么

- 不做 C：不内嵌 Inter、不做浮起画布（§9 的"不要大卡片包住工作区"保持）。
- 不改 L1 / L2 / L3 的任何形状与归属，不改 rail 宽度，不改 Focus 模式。
- 不改任何 API 形状，不加 API（Targets 的"最近一次运行"用现有接口，给不出就不画）。
- 不重写 Data 的网格引擎、编辑缓冲、对象 tab；不改 Terminal 的会话 / 录制 / 快捷键行为；不改 xterm 的 ANSI 色。
- 不引依赖、不加打包器、不加 web font、不加第二种强调色。
- 不给陈列页进导航（它不是用户功能）；不给它加 API 调用。
- 不碰 19999；部署是 owner 的决定（`scripts/deploy.ps1`，全部阶段完成后）。

## 9. 审阅记录（P0，2026-09-23）

owner 要求"写 spec 后，review 一下，有问题就改"。初稿对照代码审了一遍，改了这些：

| 问题 | 改法 |
| --- | --- |
| 按天分组的标题吸顶会被固定页头盖住（两个 sticky 都是 `top: 0`） | 壳量页头高度写进 `--pane-head-h`，日期标题停在它下面（§2.4） |
| "每行都一样的元信息不进行"对 Traffic 不成立——客户端在那里是变化的 | `who` 按当前页的取值种数决定进不进行（§2.4） |
| G5 原来数"所有 class 字面值"，Data 的几百个局部类会淹没信号，也证明不了页面没手搓库的形状 | 改成只数**属于 `ui.css` 的类名**，迁完的视图目标 0（§4） |
| 把规则从 base / views 搬进 ui.css 会改层叠顺序，和视觉改动混在一个 commit 里就说不清哪次变化是哪来的 | 拆成 P1a-1 纯搬动（像素比对验收）与 P1a-2 视觉改动（§5） |
| Jobs 的 cron 人话翻译会写第二个翻译器 | 复用 `describeCron`（`jobs.ts:374`）与 `whenLabel`（§3.6） |
| Plugins "去掉状态点"会把 failed / waitingDependency 这类异常一起藏掉 | 只在状态与开关一致时不画点，异常照画并在副行写原因（§3.5） |
| 组件"不挂处理函数"与 timeline 的展开冲突 | 组件只给纯 DOM 帮手 `timelineToggle`，监听留在视图（§2.4） |
| owner 途中追加"组件库先把控好，后续设计复用组件" | 加 U17：P1 备齐全部形状并走查后才迁页；设计稿改用陈列页的场景（§1.3、§2.6、§6） |
| （P1b 实施时）§2.5 "已有组件原地保留、由 `ui/index.ts` 转出"与 U2 冲突：转出 groups.ts 就把 api 与状态带进了库 | 按依赖分三类：纯标记进 `ui/`；只依赖 DOM 的机制 P1b-2 搬进 `ui/`；绑定应用的行为留原处、建在库上（§2.5） |
| （P1b 实施时）组件的类名写错不会有任何测试发现——页面只是多一个没样式的盒子 | `ui-components.test.ts` 把每个组件的每个选项渲染一遍，断言画出的每个类都在 `base.css` / `ui.css` 里有规则；它当场抓到 `dot("off")` 的无样式类 |
| （P1b-2）§2.5 写"原模块留转出，调用点逐步改" | 改成一次全部改指向（脚本改了 30 多个文件的 import），不留转出：转出是第二个家，搬完就没人再去删它 |
| （P1b-2）§2.5 写的 `codeBlock` | 整个 json-view 搬进 `ui/`，名字沿用 `jsonCodeNode`，Logs 的调用不变 |
| （P1b-2）菜单开着没有该从 DOM 推出来 | 开关留作 `ui/menu.ts` 自己的状态（`menuOpen`）：假 DOM 的测试里 `getElementById` 会凭空造节点，从 DOM 推会永远是"开着" |
| （P1b-2）`popupMenu` 改用 `h()` 构建后，4 个测试套件（admin-revisions、admin-row-menu、admin-data-redis-cellmenu、admin-logs-pagination）的手写微型 DOM 挂了：它们不会从文本子节点算 `textContent`，也没有 `focus` | 这一步 `popupMenu` 原样搬、留着这些防护；那几个套件随拥有它们的页面（P2 MCP、P7 Data）换成 happy-dom 时，再把菜单改到 `h()` 上 |
| （P1b-2 实测）下拉列表里按 Escape：旧代码先把 `openState` 置空再读 `openState.trig`，每次都抛错，键落到 main.ts 的 Escape 链，把下拉所在的 sheet 连同已填的内容一起关了 | 先取触发器再关；键在捕获阶段处理并停住（上文"键盘分层"） |
| （P1b-2 实测）菜单、下拉列表、下拉触发器、sheet 里按方向键，main.ts 的侧栏方向键也会动，在 sheet 背后换了选中的 MCP；sheet 里按 `/` 会把焦点拽出对话框 | 各层停住自己的键；sheet 由 `initSheet` 设为模态，只放 Escape 出去 |
| （P1b-2）菜单行的"有暂存改动"圆点 `.db-tab-dot` 只有 views.css 有规则，库画的类不在库的样式表里 | 规则搬进 ui.css，尺寸用 `--dot`；G4 views.css 字面值 253 → 251，G7 views.css 76412 → 76099 字节 |
| （P1b-3）§2.6 写"场景里的 class 只能是 `ui.css` 的类"，但场景画的是整页，壳的 `.shell` / `.sidebar` / `.side-head` 在 `base.css` | G6 查画出来的每个类都在 `base.css` ∪ `ui.css` 里有规则；"不手写库的形状"交给 G5（数 `ui.css` 的类），陈列页与场景冻结为 0（§2.6 已改） |
| （P1b-3）`pane()` 进库，但 `.pane` 的框（内边距、量度、`.wide`）在 views.css，只链 base + ui 的陈列页画不出页面 | 框搬进 ui.css；Data / Terminal 的通栏改成通用的 `.pane.full`（两页在 `db-host` / `term-host` 旁加上它）；G7 views.css 76099 → 74519 字节 |
| （P1b-3）四个场景要用而库里没有：侧栏的一行、节标题下的一句说明 | 加 `sideRow()`（侧栏行的形状；`sidebar.ts` 的 `sideRowNode` 在 P2 换成它）、`section({ note })`；`pane()` 同上 |
| （P1b-3 陈列页查出）`.dot` 不在 flex 里就是 0 宽；`iconBtn({ pressed })` 画得和没按下一样 | `.dot` 加 `inline-block`；`aria-pressed="true"` 取强调色与一层淡底 |
| （P1b-3 陈列页查出）菜单行带图标或"有暂存改动"圆点时不是 flex：图标贴着字，圆点是有尺寸的内联 span、根本没有盒子——Data 标签栏溢出菜单从 docs/43 M1 起就这样，有暂存改动的行看上去是干净的 | `.menu button:has(> .ic, > .db-tab-dot)` 成为 flex 行（间距 `--s2`）；19997 上 Data 溢出菜单实测图标与字间距 8px、圆点 6×6 |
| （P1b-3 陈列页查出）timeline 展开的正文放两块代码（参数与返回）时上下贴死，成一整块灰 | `.tl-body` 改成纵向 flex、间距 `--s2`（P2 的 Logs 用 timeline 时正是参数 + 返回两块） |
| （P1b-3 陈列页查出）空状态说明 `max-width: 44ch`，一个中文字约 2ch：23 个字的中文说明第二行只剩一个字 | `.empty p` 加 `text-wrap: balance` |
| （P1b-3）admin-panel 的"整张模块图能链接"测试只读目标文件的**第一个** `export {}`，也不跟 `export … from`：`ui/index.js` 在它眼里只导出 btn / iconBtn / moreBtn，桶文件本身的转出从没被查过 | 读全部导出子句；`export { a } from` 与 import 同样解析、同样查名字（改错一个转出名当场红） |
| （P1b-3）G5 的基线 | 冻结在 35 个文件、623 个 `ui.css` 类名；只许降，降了同一个 commit 改表 |
| （P1b-3，留给 P2）`.seg` 自带 `margin-bottom`——摆放写进了组件，放进 kv 行就多出一截 | 不在这步动：它改 MCP 详情的纵向节奏，P2 迁 MCP 时把间距交给页面的流 |
| （P1b-3）陈列页字节：§1 估计 < 20 KB | 实数 ui.html 2,482 + ui-gallery.js 20,029 + ui-scenes.js 10,126 = 32,637 字节，另有中英各 166 个 `gallery.*` 键；超估计，P9 汇总时一并记 |
| （P2 前，owner 2026-09-23）"MCP log 太长了……不管是往下还是往上都太麻烦了"，要一个右下角回到顶部的浮动按钮 | 加 U18 与 §3.2 一项：壳在 `#pane` 上装一个 `toTop()`，所有内容页共用 |
| （P2 前，owner 2026-09-23）看 19999 的 Logs 仍是旧样子，问为什么 B 没落到页面上——P1 只建了库与陈列页，页面从 P2 起才迁 | P2 提到 P1c 之前做（P1c 只改 skill 与清单，页面看不出区别）；P1c 紧跟 P2，skill 照 P2 落地后的实际形状写。U15 "P2 开始前 skill 已经描述库"因此放宽为"P3 开始前" |
| （P2 前自审）§3.2 资源头的"`scrollTop > 48` 收起、`< 8` 展开"：收起让头变矮、滚动高度随之变小，内容短的页面会在两个阈值之间来回跳，滞回只能减少不能消除；还要一段 JS 状态 | 两层吸顶（§3.2）：名字行吸顶，说明与状态行从它下面滚走，seg 停在它下沿。停下来的样子相同，零 JS 状态，头的高度从不变化 |
