# 39 — 壳的减法：图标 rail、下划线页签、一种读数、记住上次的页

> 状态：**已实施**（分支 `refactor-ui`，2026-09-20）。P0 `02c9a7f`（S4 记住上次的页）、
> P1 `70839b8`（S2/S3/S7/S8 标题 + 下划线页签 + 溢出 ⋯，含一处实测修得的延迟重排）、
> P2 `dc8e453`（S1/S5/S6 图标 rail、一种读数、侧栏 glyph 去底）。门禁全绿：`npm run check`
> 70 文件 621 用例、`cargo test --workspace`、`clippy -D warnings`。走查在 **19997**（同脚本、
> 独立测试 home）——另一会话的 panel-i18n 实例 18:25 起占用 19998；1440/960/480、明暗两套、
> 真指针。数字（§4 口径）：`base.css` 31,870 → 34,212 字节；自有发射 JS 958,105 → 968,594
> 字节（+`last-page.js` 3,752）；`swiss.exe` 9,748,480 字节；MCP/Servers 首屏带框矩形：侧栏
> 类型小方块 9 → 0、bar 换页器框 1 → 0、rail 9px 标签 7 → 0,机械枚举的着色/描边元素 17 个
> （其中状态点 9、结构面 3,用户读作"框"的控件 4 个:搜索框 + 3 条组 band,另加选中座的悬停
> 底),≤ §0.1 基线 20、达到 ≤9 的目标。合并 master、部署是 owner 的决定。
>
> **S1 后记（2026-09-21）**：owner 反转了图标-only，座位重新带名字。名字回来后 48px 装不下
> 英文长名（`Terminal` 在 10px 下 37px，座位里每边只剩 2px），rail 改为 **56px**，座位的
> `--s1` padding 即标题的空气；字号由 `page-registry.fitRailLabels` 对**整条 rail 取一个值**
> （10px 起、9px 下限、再长走省略号），focus 模式退出时随 resize 重量。纯算数在
> `railCaptionSize`，套件 `test/rail-caption-fit.test.ts`；真浏览器走查（19998，1280/480、
> 英/中、focus 进出、注入 13 字名）见 commit 记录。
> 前置阅读：`.claude/skills/swiss-ui-design/SKILL.md` §1–§8（四层与归属——本文一层都不挪）、§16
> 规则 1 / 2 / 9 / 15 / 18；`.agents/rules/panel-proof-of-life.md`；`docs/36-panel-typescript-spec.md`
> §1.2（D2 发射、D3 产物提交）；`docs/37-panel-modern-typescript-spec.md` §7（`h()` 与重画）。
> 代码注释英文；中文只在本文。

> 需求原文（用户，2026-09-20）：看了 MCP / Servers 页截图问"这样设计，是否优雅，我打算改成更加优雅
> 的展示方式的"；追加："侧边栏需要图标啊，context bar 你看看怎么做才好。seg 就不要拆了吧，我只是让你
> 看看一级二级怎么设计比较好"。四个方案（`docs/assets/39/shell-mockup.html`，preset A–D，双击打开，
> 键 `1`–`4` 切换）看过后选 **C**。三个追问："子页面很多怎么办？Terminal 那种特殊的处理怎么办？我从 A
> 点了 C，A 一开始是 A2 页面，再点 A 的时候应该访问 A2 而不是 A1"——答案是 §1.2 的 S3 / S7 / S4。
> "行吧，那你来写 spec，我交给其他的模型来做。"

## 0. 现状与缺口

### 0.1 一屏的框与嗓音（mockup preset A，就是今天的 CSS）

MCP / Servers 这一屏有独立描边或填充的矩形约 **20** 个：rail 选中座、`MCP / Servers ▾` 的框、搜索框、
folder-plus、两条组 band、9 个类型图标的小方块、seg 容器 + 选中块、`Disable`、`⋯`、工具卡、`Try`、
toggle。字体处理 6–7 种：22px 标题、13px 描述、mono 路径、label 灰字、mono 工具名、mono 灰参数、
按钮字、rail 下 **9px** 标签、`22 MB` 另一种 mono。层级是对的（rail = L1，context bar = L2，seg = L3），
不优雅是因为边太多、嗓音太多。本文只减壳（L1 / L2）的边和嗓音；页面主体（seg、工具行、`Disable`）**不动**。

### 0.2 三个机制缺口（实测，基线 `f6dd448`）

| 缺口 | 在哪 | 后果 |
| --- | --- | --- |
| rail 座位永远回插件的第一页 | `page-registry.ts:90` `data: { view: g.pages[0].id }`；`plugin-palette.ts:133` `go(g.pages[0].id)` | 在 MCP / Traffic，去 Jobs，点回 MCP → Servers。用户原话："应该访问 A2 而不是 A1" |
| L2 藏在菜单里，切换器的框是补丁 | `base.css .ctx-switch`（注释：纯面包屑没人发现，`36235d6` 加了框） | 三页的插件要点开菜单才知道自己有几页；框是全 bar 最重的东西 |
| rail 标签 9px | `base.css .rail-btn-label { font-size: 9px }` | 全面板唯一一个不在五级字阶（22/15/13/12/11）里的字号；读不清，等于没有 |

另外两处小噪音：`#countChip`（12px sans `--text-3`）与 `#memChip`（mono，视觉更大）并排是两种控件
（`base.css` `#memChip { font-family: var(--mono) }`）；侧栏类型 glyph 坐在 5% 灰底的小方块里
（`.side-type` + `.side-type:has(.ic)`），9 行 9 个盒子。

### 0.3 便宜的事实

- `paintNavigation()`（`page-registry.ts:211`）已经在每次 `navigatePage` 后统一重画 rail + context
  bar；tab 条只是 `paintPluginContext()` 换一种画法。
- `pageMenuItems(current)`（`page-registry.ts:138`）已经是"该插件全部页、当前页打勾、不可用页标
  `· off`"的纯函数，且有测试——tab 溢出的 `⋯` 菜单直接复用。
- Focus 模式（`immersive.ts`）与 Terminal 的 `[data-shell-focus-slot]` dock 一字不用改：bar 高度、
  `#appZone` 的搬家、`immersive-docked` 收成 0 高都和 bar 左半边画什么无关。
- `localStorage` 先例三个：`swiss_theme`（main.ts）、`swiss.rail.pinned`（plugin-palette.ts）、分组
  折叠表（groups.ts `collapseKey(scope)`）。"记住上次的页"照抄。
- mockup 已在 1440×820 明暗两套下截图确认：preset C 的 CSS 就是 §3 的 CSS，照搬即可。

## 1. 决定

### 1.1 宪法核对与 ADR

四条产品属性一条都不碰：改的是 CSS 与两百行 TypeScript，发射产物变小（9px 标签与 `.ctx-switch` 的
规则删掉），`/api/*` 与 Rust 零改动，进程内存不动（不跑 `swiss-memory-record`）。**四层与归属不变**：
L1 仍在 rail，L2 仍在 context bar，L3 仍在页体——本文换的是 L1 座位和 L2 切换器的**形**，不是它们的
主人。skill §4 里"当前页控件必须看得出可点"仍然成立：tab 本身就是可点的形。

**不写 ADR。** 可逆（CSS 回退一个提交）、不意外（GitHub 仓库导航 / Linear 的形）、没有真正的取舍
（备选 B 是 C 的子集，D 改归属、吃 136px 宽度，已否决——理由留在 mockup 的 preset 说明里）。

### 1.2 十二条决定

| # | 决定 | 理由 |
| --- | --- | --- |
| S1 | **rail 只留图标**：宽 48px，座位 36px 高，glyph 18px，`title` 是 tooltip；选中态不变（`--hover` 底 + 左缘 2px `--accent`）；`.rail-btn-label` 与它的 9px 规则**删除**；`RAIL_LIMIT` 7、`⋯` 座位、palette 全部不动 | 9px 读不清等于没有；插件的名字改由 bar 左侧的标题承担（S2），一屏仍然有一处写着当前插件叫什么 |
| S2 | **context bar 左侧 = 标题 + 页签**。标题 `#pageTitle`：插件 glyph（16px，`--text-2`，与 rail 座位同一个 `glyphNode(g)`）+ 插件名（`--f-body` 600 `--text`）。多页插件在标题后画 `#pageTabs`：每页一个 `<a class="ctx-tab" href="#id">`，`--text-2` 500，当前页 `aria-current="page"`、`--text` 600、底边 2px `--accent` 压在 bar 的 border 上。单页插件只有标题。`#pageBtn` / `.ctx-switch` / `#pageLoc` **删除** | 三层三种形：rail 座位 → 下划线 tab → 页内 pill seg，一眼分层；glyph 让标题不会被读成第四个 tab；单页与多页共用一个标题元素，bar 40px 不变，页体起点不变 |
| S3 | **页签溢出**：按声明顺序装，装不下的进末尾 `⋯` tab（`popupMenu` + `pageMenuItems()`，列**全部**页、当前页打勾）；当前页永远可见——落在溢出区时顶掉最后一个可见 tab。纯函数 `fitTabs()` 决定，`ResizeObserver` 在 `#pageTabs` 上重算。规范：**一个插件 ≤ 5 页**，写进 `swiss-add-plugin` skill | 今天最多 3 页，960px 下装得下 6–7 个；`⋯` 是安全网，不是设计目标——超过 5 页几乎都是把 L3 当 L2 |
| S4 | **每个插件记住上次访问的页**：`last-page.ts`（叶模块）`rememberLastPage(pluginId, pageId)` / `targetPageFor(group, last, usable)`；`navigatePage` 在 `setCurrentView(id)` 后记一笔；rail 座位与 palette 的点击改走 `targetPageFor`；`localStorage["swiss.lastPage"]`（JSON `Record<pluginId, pageId>`）。兜底：记住的页不在该组、或 `unavailable()` → `pages[0]`。座位上的 `data-view` **仍是** `pages[0].id` | 用户原话；`jobs.ts:61` 用 `#railNav [data-view="jobs"]` 找座位，`admin-navigation.test.ts:162-169` 断言它——目标页在点击时算，不写进属性 |
| S5 | **读数一种嗓音**：`#countChip` 与 `#memChip` 同 `--f-label` / `--text-3`；mem 仍 mono 但降到 `--f-caption`（x 高度与旁边的 sans 对齐）；两者之间一个 `·`（`#countChip:not(:empty)::after`）；mem **留在 bar 上**；960px 以下隐藏 mem 的规则不变 | 内存数是产品主张（"small"），不藏；但它和 count 是一行读数，不是两种控件 |
| S6 | **侧栏类型 glyph 去底**：`.side-type:has(.ic)` 无灰底、无内边距、`--text-3`；行 hover / 选中时 `--text-2`；**文字 chip**（白名单外的 tag，如 `node`）保持今天的 mono + 5% 底 | 9 个小方块是 §0.1 里最多的一类框；文字 chip 本来就是另一种东西，不为它发明第三种 |
| S7 | **Focus 模式**：`#pageTitle`、`#pageTabs` 加入 `body.immersive` 的隐藏表（替换 `#pageBtn` / `#pageLoc` 两行）；`immersive-docked`、Terminal 的 dock、`#expandBtn` 的位置一字不动 | Terminal 是单页 workspace，正常模式走标题分支，Focus 走它自己的 dock——两条路都已存在 |
| S8 | **tab 是链接不是 tablist**：`<nav id="pageTabs" aria-label="Pages">` 里的 `<a href>`，点击走浏览器 hash → 已有的 `hashchange` → `navigatePage`；键盘 Tab / Enter 原生可达；不加 `role="tablist"` | 页签切的是整页（有 URL），不是页内面板；中键、复制链接免费得到；`canLeave` 拦截路径不变（`navigatePage` 早退时 `replaceState` 回当前 hash） |
| S9 | **不加 token、不加 sprite**：所有值走现有 token；字面量只有 tab 底线的 2px（与选中态左缘 2px 同一个数）和 glyph 的 18px / 16px（icon 尺寸本来就是字面量：`.ic` 16、rail 17、chevron 12） | skill §18 第一条 |
| S10 | **测试**：`fitTabs()` 与 `targetPageFor()` 各一个纯函数用例文件；`admin-navigation.test.ts` 的切换器断言改成 tab 断言（§2 列了每一条）；`admin-rail.test.ts` 不动 | 行为改动配用例；断言用户看得见的状态（`aria-current`、`hidden`），不断言存在性 |
| S11 | **文档同步**：`swiss-ui-design` skill §4（context bar 的形）、§17（`context bar` / `segmented control` 两行）、§3（rail 无标签）；`.agents/docs/style-design.md` 的 rail / ctxbar 条目；`swiss-add-plugin` skill 加"≤ 5 页"与"给你的插件在 `GLYPHS` 加一个 sprite glyph"；README docs 表加一行；本文状态头 | skill §12 第 6 条：旧决定被替代时不留矛盾的"已实施" |
| S12 | **交付**：P0 → P3 各一个提交（§2），每提交 `npm run check` + `cargo test --workspace` + 19998 真点走查；合并与部署是 P3 之后 owner 的决定 | 房子的规则 |
> **修订（2026-10-30，业主决定）**：S1 的 icon-only 形态被推翻——rail 座位重新带上插件名（glyph 18px 在上，标签 `--f-caption` 11px 在下，座位 36→44px，rail 仍 48px 宽）。9px 时代的教训是"出字阶的字号读不了"，不是"文字本身多余"；这次标签回到五级字阶内。`⋯` More 座不是领域，保持纯图标。tooltip（含插件故障文案）与 context bar 标题不变。

### 1.3 不变的东西（写明，免得"顺手"）

- L1 / L2 / L3 的归属；`page-registry` 的 `PageDescriptor` / `PageGroup` 契约；`/api/plugins` 形状；
  Rust 任何一行；19999。
- 页体：seg（用户明确"seg 就不要拆"）、工具行、`Disable` / `⋯`、toggle、卡片、measure——**一个像素不动**。
- 侧栏：248px、搜索框、folder-plus、组 band、行高、选中态、拖拽——只改 S6 那一条 glyph 规则。
- `RAIL_LIMIT` 7、pin 逻辑、palette 的搜索与分区、`GLYPHS` 表与 puzzle 兜底（一个没有专属 glyph 的
  第三方插件在 icon-only rail 上是拼图块 + tooltip；两个这样的插件在 rail 上分不开——**已知、接受**：
  今天没有第三方插件，palette 列全名，给 `GLYPHS` 加一行是一行的事，S11 把它写进插件作者清单）。
- Focus 模式的进入 / 退出位置、Esc 链、resize 派发、Terminal dock。
- deep link（`#traffic` 直达，不经过 rail，也不看 lastPage）、浏览器前进后退（`hashchange` 按 id 走）、
  `canLeave` 拦截（Data 有未保存编辑时不换页、不写 lastPage）。
- `index.html` 的英文静态文案是英文版（docs/38 的约定）；本文新增的可见文案只有 `⋯` tab 的
  `title="All pages"` 与 `<nav aria-label="Pages">` 两条。
- 发射管线（docs/36 D2 / D3）：改 `panel/src/*.ts` → `npm run build`，不手改 `admin_assets/js/**`。

### 1.4 与 docs/38（面板 i18n，分支 `panel-i18n`）的关系

两条分支都改 `page-registry.ts`、`index.html` 的 `#ctxBar`、`base.css`。本文**不依赖** docs/38，也不
等它。规则：**后合并的一方 rebase**。若 38 先进 master：本文的两条新文案改 `tr()`，`fill(title, …)` 里
的 `g.label` 已经是 `tk()` 标记过的 key（38 I1）——完整性扫描会指出漏的。若本文先进：38 的 I1 扫
`page-registry.ts` 时把这两条一起扫掉。两边都不需要对方的任何函数。

## 2. 交付：P0 → P3

### 2.1 P0 — 记住上次的页（S4）

最小、独立、用户问的那个。一次提交。

`panel/src/last-page.ts`（叶模块，只 import `types`）：

```ts
export const LAST_PAGE_KEY = "swiss.lastPage";              // JSON Record<pluginId, pageId>, per browser
export function loadLastPages(): Record<string, string>;   // localStorage → {} on absent / garbage / throw
export function rememberLastPage(pluginId: string, pageId: string): void;  // merge + write; write errors are swallowed
/** The page a plugin's seat opens: the last one visited when it is still a member of the group
 *  and usable, else the group's first page. Pure; the seat and the palette both call it. */
export function targetPageFor(group: { id: string; pages: { id: string }[] }, last: Record<string, string>, usable: (id: string) => boolean): string;
```

接线（`page-registry.ts`）：`navigatePage` 在 `setCurrentView(id)` 之后
`rememberLastPage(currentGroup()!.id, id)`（组 id，不是 `page.pluginId`——描述符上它是可选字段，`groups()` 已经替 legacy 表兜过底，座位与 `targetPageFor` 用的也是 `g.id`）；
`paintPluginRail` 的点击：`[data-group]` 命中时 `navigatePage(targetPageFor(groupOf(dataset.group), loadLastPages(), (id) => !unavailable(registry.get(id))))`——座位的 `data-view` 属性照旧是 `pages[0].id`（S4 的理由）；
`plugin-palette.ts:133` 同样改（`go` 收一个 group 而不是 id，或 palette 自己调 `targetPageFor`——二选一，别两处各写一遍逻辑）。

**验收：** `panel/test/last-page.test.ts`（node，stub `localStorage`）：
- `targetPageFor({id:"mcp", pages:[{id:"mcps"},{id:"traffic"},{id:"tokens"}]}, {mcp:"traffic"}, () => true)` → `traffic`；
  `last` 缺 → `mcps`；`last.mcp = "jobs"`（不在组内）→ `mcps`；`usable("traffic") === false` → `mcps`。
- `loadLastPages()`：无存储 → `{}`；垃圾 JSON → `{}`；`getItem` 抛 → `{}`；`rememberLastPage` 后再读能拿到，
  `setItem` 抛不冒泡。
- `admin-navigation.test.ts` 加一条：走 `#traffic` 再走 `#jobs`，点 MCP 座位的 `onclick` → 当前视图是
  `traffic`（断言 `currentView()` 与 `aria-current`），且座位 markup 仍含 `data-view="mcps"`（`:162` 那条不动）。

19998：MCP → Traffic，点 Jobs，点 MCP 座位 → Traffic；刷新，点 Jobs 再点 MCP → 仍 Traffic；在 palette 里点
MCP → Traffic；`#tokens` 直达 → Token（deep link 不看记忆）；Data 页有未保存编辑时点 MCP → 留在 Data，
`localStorage["swiss.lastPage"]` 不变。

### 2.2 P1 — 标题 + 页签 + 溢出（S2、S3、S7、S8）

一次提交。`index.html` 的 `#ctxBar`（`:114-124`）改成：

```html
<header class="ctxbar" id="ctxBar" hidden>
  <span class="ctx-title" id="pageTitle" hidden><!-- glyph + plugin label --></span>
  <nav class="ctx-tabs" id="pageTabs" aria-label="Pages" hidden><!-- one .ctx-tab per page, ⋯ when they overflow --></nav>
  <span class="grow"></span>
  <span class="chip" id="countChip"></span>
  <span class="chip" id="memChip" title="swiss resident set">mem …</span>
  <span class="app-zone" id="appZone">…unchanged…</span>
</header>
```

`#pageBtn` 与 `#pageLoc` 删除；`.ctx-switch` / `.ctx-plugin` / `.ctx-sep` / `.ctx-page` / `.ctx-loc` 的 CSS 删除
（`.ctx-page` 若别处仍用则留）。`base.css:360-363` 的隐藏表换成 `#pageTitle, #pageTabs, #countChip, #memChip`。

`paintPluginContext()` 重写（`page-registry.ts:150-208`）：

```ts
const title = $("pageTitle"), tabs = $("pageTabs");
const show = !!(current && page);
bar.hidden = !show && !immersive;
if (!show) { title.hidden = true; tabs.hidden = true; return; }
title.hidden = false;
fill(title, glyphNode(current), h("span", { class: "ctx-name" }, current.label));   // glyphNode from plugin-palette
title.title = off ? (off.lastError || "Plugin disabled") : "";
if (current.pages.length >= 2) {
  tabs.hidden = false;
  fill(tabs, ...current.pages.map((p) => tabNode(p, p.id === currentView())));
  layoutTabs();              // measure, hide the overflow, add the ⋯ tab if any
} else { tabs.hidden = true; }
```

`tabNode(p, active)`：`h("a", { class: "ctx-tab", href: p.path || "#" + p.id, aria: active ? { current: "page" } : {}, title: off ? … : "" }, p.label + (off ? " · off" : ""))`。
不可用的页照画（标 `· off`、`aria-disabled="true"`，点击仍走 `navigatePage`，落在今天的"unavailable"
空态——和菜单时代一样）。

`layoutTabs()`：`#pageTabs` 是 `flex: 1; min-width: 0; overflow: hidden`。全部 tab 画出后读各自
`offsetWidth` 与容器 `clientWidth`，交给纯函数：

```ts
/** Which tabs stay visible in `avail` px. All fit → all visible. Otherwise reserve `moreWidth` for
 *  the ⋯ tab, keep tabs in order while they fit, and guarantee the active one: when it fell into
 *  the overflow it takes the last visible slot. Returns ids; the caller toggles `hidden`. */
export function fitTabs(tabs: { id: string; width: number }[], activeId: string, avail: number, moreWidth: number): { visible: string[]; overflow: string[] };
```

有溢出时在 `#pageTabs` 末尾加 `<button class="ctx-tab ctx-more" title="All pages" aria-haspopup="menu">`
（`iconNode("ellipsis")`），点击 `popupMenu(rect, pageMenuItems(current) + fn → navigatePage)`——就是今天
`btn.onclick` 那段（`:180-196`）搬过来；`aria-expanded` 的收尾监听照搬。`ResizeObserver` 一个，观察
`#pageTabs`，回调只调 `layoutTabs()`；`paintPluginContext` 每次重画前 `disconnect()` 再 `observe()`。
happy-dom / node 下没有布局：`offsetWidth` 为 0 → `fitTabs` 全部可见，测试环境自然退化，不需要 mock。

**验收：** `panel/test/fit-tabs.test.ts`：三 tab 各 60、`avail` 300 → 全可见、无溢出；`avail` 150、`more` 32、
active 第一个 → 可见 `[a]`（60 + 32 ≤ 150 装不下第二个 60 + 32 = 152）、溢出 `[b, c]`；同样宽度 active
是 `c` → 可见 `[c]`、溢出 `[a, b]`（当前页顶掉最后一个可见）；`avail` 0 → 可见只有 active；空表 → 空。

`admin-navigation.test.ts` 逐条改：
- `:202` "MCP carries Servers / Traffic / Token in the switcher and its menu" → 断言 `#pageTabs` 三个
  `.ctx-tab`，`href` 分别 `#mcps` / `#traffic` / `#tokens`，Servers 带 `aria-current="page"`，`#pageTitle`
  含 `MCP` 与 `#i-mcp`。
- `:218` / `:224` deep link → 对应 tab 带 `aria-current`，其余没有。
- `:238` tunnels 两页 → 两个 tab。
- `:258` 单页 → `#pageTitle` 可见含 `Data`，`#pageTabs.hidden === true`。
- `:272` terminal → 同上，且 `layoutOf` 仍 `workspace`。
- `:281` "page chips sit left, the app trio owns the far right" → 标题 / tabs 在 `.grow` 之前，`#countChip`、
  `#memChip`、`#appZone` 在之后（顺序断言照旧）。
- `:330` 全 off 的多页组 → tab 标 `· off`、`aria-disabled`。
- `pageMenuItems` 的现有用例不动（`⋯` 复用它）。

19998（1440 与 960 两个宽度，明暗两套）：每个插件座位 → 标题 glyph + 名字正确；MCP / Tunnels / Settings 的
tab 全部可点、`aria-current` 跟着走、hash 跟着变；单页插件（Data / Jobs / Terminal）无 tab、bar 高度 40 不变、
页体起点 y 与多页插件一致（量 `#pane` 的 `getBoundingClientRect().top`，两类页面相等）；Focus 模式下标题与
tab 隐藏、Terminal 的 dock 仍收成 0；**溢出 `⋯` 今天用真实插件到不了**（最多 3 页）——把窗口收到 480px 逼
出来，点 `⋯` 菜单能切页，**在提交说明里写明这是压到支持底线以下逼出的**。

### 2.3 P2 — rail 只留图标、读数、侧栏 glyph（S1、S5、S6）

一次提交，纯 CSS + 删两处 label 节点。

`base.css` rail 段（`:257-290`）：

```css
.rail { width: 48px; … }                          /* was 56 */
.rail-btn { height: 36px; gap: 0; … }             /* was 48 with a caption */
.rail-btn .ic { width: 18px; height: 18px; }      /* was 17 */
/* .rail-btn-label: DELETED with its 9px rule */
```

`railSeat()` 与 `moreSeat()` 不再 `h("span", { class: "rail-btn-label" }, …)`；`title` 已经是全名
（`g.label`，off 时带原因），tooltip 就是它。`admin-navigation.test.ts:158` 的座位 markup 断言若含
`rail-btn-label` 则同步去掉；`admin-rail.test.ts` 不受影响。

读数（`base.css:292` 附近）：

```css
.ctxbar .chip { color: var(--text-3); padding: 2px 0; }
#countChip:not(:empty)::after { content: "·"; margin: 0 var(--s2); color: var(--text-3); }
#memChip { font-family: var(--mono); font-size: var(--f-caption); }   /* one step down: x-height meets the sans beside it */
.ctxbar .app-zone { margin-left: var(--s2); }
```

`#memChip` 是 `.chip.button` 时 hover 的规则不变。960px 是支持下限：mem 数字离开时，那颗只为它服务的 `·` 一并离开（`#memChip, #countChip::after { display: none }`）——右侧空无一物的分隔符是孤儿。

侧栏 glyph（`base.css .side-type` 段）：

```css
.side-type:has(.ic) { padding: 0; background: none; color: var(--text-3); }
.side-row:hover .side-type:has(.ic), .side-row[aria-selected="true"] .side-type:has(.ic) { color: var(--text-2); }
```

文字 chip 的规则一字不动。每条改动的规则旁边的 CSS 注释改成新的理由（skill §18 倒数第二条）。

**验收：** `admin-navigation.test.ts` 座位 markup 不含 `rail-btn-label`；`admin-type-icons.test.ts` 不动
（glyph 的 aria-label 契约没变）。19998：rail 48px、七个座位 + `⋯`、hover 每个座位有 tooltip、选中态
左缘 accent；`10 MCPs · 4 up · 22 MB` 一行同色；960px 下 mem 消失、`·` 也消失（`:not(:empty)` 断
的是 count，所以要看 count 为空的页——Terminal——`·` 不画）；侧栏 glyph 无底、hover 变亮；明暗两套。
`grep -n "#[0-9a-f]\{6\}\|[0-9]\+px" styles/base.css` 对 diff 只多出 48 / 36 / 18 三个尺寸字面量，无颜色字面量。

### 2.4 P3 — 文档与收尾（S11）

一次提交：
- `.claude/skills/swiss-ui-design/SKILL.md`：§3 去掉"glyph over caption"的描述（rail 是 icon-only，名字在
  bar 标题）；§4 多页插件的示意改成 `[glyph] MCP   Servers  Traffic  Token`，单页 `[glyph] Data`，加一句
  "tab 是 L2 的形，pill seg 是 L3 的形，二者不互换"；§17 `context bar` 行改写，`segmented control` 行加
  "never in the context bar"；§13 清单加"插件 ≤ 5 页？"。
- `.agents/docs/style-design.md`：rail、ctxbar、side-type 三条按落地后的 CSS 更新。
- `.claude/skills/swiss-add-plugin/SKILL.md`：加"≤ 5 页"与"在 `plugin-palette.ts GLYPHS` 给插件一个 sprite glyph"。
- `README.md` docs 表加 docs/39 一行。
- 本文状态头改"已实施"，写落地提交与 §4 的数字。
- `docs/assets/39/` 加 19998 截图：`01-mcp-servers-{light,dark}.png`、`02-single-page-terminal-dark.png`、
  `03-tabs-overflow-480px-dark.png`、`04-focus-mode-dark.png`（命名照 `docs/assets/20/`）。mockup 文件保留，
  它是四个方案的记录。

## 3. CSS 全文（P1 + P2 新增的规则，照抄 mockup preset C）

```css
/* --- context bar: title + page tabs (docs/39) ------------------------------------------------
   The bar's left half names the plugin ONCE - its rail glyph and its label - and lays the
   plugin's pages out as underline tabs. Three levels, three shapes: a rail seat, an underlined
   tab, a pill segment inside the page. The title wears the glyph so it can never be read as a
   fourth tab, and so an icon-only rail still has the plugin's name on screen. */
.ctx-title { display: inline-flex; align-items: center; gap: var(--s2); font-weight: 600; color: var(--text); margin-right: var(--s3); min-width: 0; }
.ctx-title .ic { color: var(--text-2); }
.ctx-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ctx-tabs { display: flex; align-self: stretch; gap: 2px; flex: 1; min-width: 0; overflow: hidden; }
.ctx-tab {
  position: relative; display: inline-flex; align-items: center; padding: 0 var(--s2);
  color: var(--text-2); font-size: var(--f-body); font-weight: 500; text-decoration: none; white-space: nowrap;
}
.ctx-tab:hover { color: var(--text); }
.ctx-tab[aria-current="page"] { color: var(--text); font-weight: 600; }
/* The current page's mark sits ON the bar's bottom hairline: the same 2px accent as a selected
   row's leading edge, turned horizontal. */
.ctx-tab[aria-current="page"]::after {
  content: ""; position: absolute; left: var(--s2); right: var(--s2); bottom: -1px; height: 2px;
  background: var(--accent); border-radius: 1px 1px 0 0;
}
.ctx-tab[aria-disabled="true"] { color: var(--text-3); }
.ctx-more { padding: 0 var(--s2); color: var(--text-3); }
.ctx-more:hover, .ctx-more[aria-expanded="true"] { color: var(--text); }
```

## 4. 验收与门禁命令

```
# 面板（在 crates/swiss-panel/panel 下，自己一条命令；第一次先 npm ci）
npm run check          # typecheck ×2 + lint + build:check + vitest

# Rust（worktree 根；--workspace 不可省）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d          # 本文不加依赖，这条是回归守卫
```

实机（proof-of-life，19998，每个阶段）：

```
scripts/test-instance.ps1 -Stop
# 先在 crates/swiss-panel/panel 下 npm run build，再 touch crates/swiss-panel/src/lib.rs，
# 否则 rust_embed 指纹不变，release 二进制里还是旧面板
$env:CARGO_TARGET_DIR="target-test"; cargo build --release
scripts/test-instance.ps1 -Fresh
```

agent-browser 真实指针事件（CDP input，不是 `element.click()`），冷加载后走 §2 每个阶段列的清单，
1440 与 960 两个宽度、明暗两套。**走不了的流程如实列为未验证并写明原因**（溢出 `⋯` 属于此类：
写明是压到 480px 逼出的）。

要记录的数字（P3 写进状态头）：`base.css` 字节（基线 → 终值）、发射自有 JS 字节（`js/**` 非 vendor）、
`swiss.exe` 字节、MCP / Servers 一屏的矩形数（基线 ~20 → 目标 ≤ 9：rail 选中座、搜索框、folder-plus、
两条 band、seg 容器 + 选中块、`Disable`、`⋯`、工具卡、`Try`、toggle 里减掉 9 个 glyph 方块与切换器的框）。

## 5. 不做什么

- 不做 D（宽 rail 嵌套页面）、不做 hover 展开标签的 rail、不给 rail 加第二种宽度。
- 不拆 seg（用户原话）、不动页体任何元素：`Disable`、工具行三层字、toggle 的饱和度、卡片——那是另一篇。
- 不给 `/api/plugins` 加 icon 字段（`plugin-palette.ts` 顶部注释的理由仍然成立）；不引 tab 组件、不引任何
  依赖、不加 token、不加 sprite。
- 不做 L3 的"记住"：MCP 页记住选中的 server、server 记住 seg tab 已经在 `ui-state` / `McpDetail.tab` 里。
- 不做 `⋯` 之外的多页方案（二级 rail、下拉 + tab 混合）；超过 5 页是插件的问题，不是壳的。
- 不改 Terminal 的 dock、不改 Focus 的进出位置、不改 `RAIL_LIMIT`、不碰 19999。
- 不写 ADR（§1.1）。
