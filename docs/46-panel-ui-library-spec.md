# 46 — B 方案：面板 UI 库（`panel/src/ui/` + `ui.css`）与全插件页整理

> 状态：**草案**（分支 `panel-ui`，基线 `6d096a0`，2026-09-23）。P0 本文。
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

### 0.2 代码里的数字（基线 `6d096a0`）

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

- [ ] SSH Connections：说明一行；New group 变图标；规则数从副行挪到右侧一列（`cols`）；删掉与上方栏重复的底部计数，
      底部只留 revision。
- [ ] Port Forwards：四个按钮收成 `New` + ⋯（Start all / Stop all）+ 新建分组图标；本地端口单独一列；错误改成副行
      一句红字（`err`），每行一样高，完整原因在 title。

### 3.5 Settings（P5）

- [ ] Plugins：状态与开关一致时**不画点**（开关已经说了开 / 关）；状态异常（failed、waitingDependency、not-built）时画
      红 / 琥珀点，副行是原因。副行 `pages: a, b, c` 收成 "3 pages"（完整列表在 title）；说明一行；底部只留 revision。
- [ ] Secrets：删掉每行重复的那句；说明一行；Copy ref 变成带 `copy` 图标的 ghost 按钮。
- [ ] System：说明一行；行用 `kvRow` / `row`。

### 3.6 Jobs 与 Remote（P6）

- [ ] Jobs：说明一行；删掉 `SCHEDULED COMMANDS` 标题；三个按钮收成 `New` + ⋯（New (advanced)）+ 新建分组图标；
      cron 翻成人话（"Daily 03:00"、"Every 15 min"，悬停看原始 cron；翻不了的原样显示）——**复用** sheet 里
      已有的 `describeCron`（`jobs.ts:374`），不写第二个翻译器；下次运行用相对时间（"in 3 h"），上次运行也用
      相对时间，基于 `whenLabel`（`util.ts:123`）扩出未来方向；上次失败才标红（`tag` tone bad）；停用的 job 是一个 `Off` 标签而不是整行变灰。
      运行记录（sheet 里的 `.call`）用 `timeline`。
- [ ] Remote › Targets：说明一行；New group 变图标；加一列"最近一次运行"（相对时间 + 退出码 tag），数据取现有的
      runs 接口，**不加 API**；接口给不出时这一列不画。
- [ ] Remote › Runs：`timeline`（`who` = 目标 · 来源）；非 0 退出码是红色 tag；Clear 进 ⋯。

### 3.7 Data（P7）

- [ ] 列注释（表头第二行、Columns tab 的 Comment 列、表头悬停卡）从 `--green` 改成 `--text-3` / `--text-2`；
      删掉 CSS 里自称例外的注释。
- [ ] 表列表选中：竖条 + 浅底 + `--w-emph`，不变蓝（规则 15）；抽屉里的当前库 / 连接同样处理（勾号列代替蓝字）。
- [ ] 网格去竖线；行高 +4px；行首复选框与删除按钮只在悬停该行或已勾选时出现（键盘焦点在行内时也出现）。
- [ ] `.db-tabs` 换成 `seg()`；`.ctx-menu` 换成 `.menu.float`；`.db-chip` / `.db-keytype` 换成 `tag()`。
- [ ] 状态栏里与表头重复的可编辑说明去掉（实施时截图确认是哪一句，写进 commit）。

### 3.8 Terminal（P8）

- [ ] `.term-page` 的独立色板删掉，改用 `--term-*` token（U12）：暗色舞台比 `--bg` 深一级（`#0b0c0e`），
      亮色主题下舞台 `#17181b`；按钮用面板 accent；状态点用面板的 green / amber / red。
- [ ] 会话 tab 与 Data 的对象 tab 同一种形状（顶部 2px accent）。
- [ ] 目标选择器用面板的 dropdown（`styleSelect`），不再是深色原生 select。
- [ ] xterm 的 `theme.background` / `cursor` / `selection` 从 `--term-*` 读（挂载时 + 主题切换时），ANSI 16 色不动
      （那是内容，不是 chrome）。

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
| **P9** | 收尾：棘轮收到目标值、`views.css` 里迁完页面的残留清零、数字（§7）写进状态头、截图进 `docs/assets/46/`、README 状态、docs/33 状态头的修订记录 | 全部 |

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
