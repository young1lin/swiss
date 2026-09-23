# 18 — 面板视觉刷新：实施规范

> 状态：**已实施（V1–V7，`b7131aa` 起）**。前置：[17](17-panel-design-canvas-spec.md) 的画布经用户确认（URL 记在 17 的
> 状态行）。画布没确认也能开工——本文 §2 的决定已经足够具体；画布确认后若与 §2 冲突，**视觉以画布
> 为准，约束（§0、§3、§5）以本文为准**。
> **V3 三修订（UI 统一，现行规则，2026-10）：页栏恢复常驻（见 [docs/13](13-panel-navigation-spec.md)
> 文头「D5 三修订」）。多页插件在 Plugin Context Bar 用紧凑切换器；单页插件画静态位置标签（不画假
> 下拉）；workspace 页（Terminal、Data）普通模式同样在栏下——workspace 只表示全幅身体，不再表示插件
> 自带全部 chrome。普通页面 Focus 保留 40px 流内页栏及最右侧应用控件；声明 shell control slot 的
> workspace 可在全页模式把同一组控件停靠到自身工具条并将页栏归零，当前 Terminal 使用该能力。**
> **V3 已再修订（自适应外壳，2026-09，~~单页插件不画页栏 / workspace 插件自带全部 chrome~~ 已被上条
> 取代）：多页插件在 Plugin Context Bar 用紧凑切换器。为避免 36px 抖动而保留空/重复页栏的取舍正式
> 作废——页面切换本来就是上下文切换。count chip 仍在 Context Bar 右侧（`#countChip` id 未变）。
> 沉浸模式同批重定义：折叠整个应用 chrome（Rail + Context Bar，visibility 而非 display，角落退出
> 按钮仍逃逸），资源侧栏与 workspace 页的身体 chrome 永不折叠。**
> 前置阅读：`AGENTS.md`（规则高于本文）、`docs/13-panel-navigation-spec.md`（两级导航；本文 V3 修订它的
> D5）、`crates/swiss-panel/src/admin_assets/styles/base.css` 开头 100 行（现有 token 及其理由——本文
> 改的是值和几条规则，不是推翻那段注释的思路）。
> 代码注释一律英文，解释「为什么」；文档散文中文。

## 0. 边界：什么不能碰

- **面板源码在 `../local-mcp-gateway/src/admin/`**。本仓库的 `crates/swiss-panel/src/admin_assets/` 是
  它的逐字节拷贝，改完整目录复制回来；`the_tree_is_byte_for_byte_the_node_builds` 会拦住任何分叉。
- **不加依赖、不加构建步骤。** 没有 bundler、没有 Tailwind、没有 npm 包、没有 CSS 预处理。面板是
  直接从磁盘/嵌入树服务的 ES module + 两个 CSS 文件，改完还是。
- **不改任何 `/api/*` 的形状**，不加新 API。这是纯前端刷新；Rust 侧唯一可能的改动在 V8（可选），
  且只是资源服务的 MIME。
- **不加功能。** 命令面板、设置页、新页面都不在这里。行为不变：每个按钮点了以后发生的事和现在一样，
  只是长得不一样、放的位置不一样。
- **19999 是用户的生产实例**，不停、不重启、不部署。实测全在 19998（`scripts/test-instance.ps1`）。
- **不「顺手」重排文件、重命名类、跑 fmt。** 类名只在本文点名处改。

## 1. 现状与问题（2026-09-12 截图核对）

| 现象 | 位置 | 本文条目 |
| --- | --- | --- |
| 亮色是「灰画布放白卡」，暗色是蓝灰系（`#17181c/#24262c`），整体像 VS Code 默认主题 | `base.css` `:root` 与 `[data-theme=dark]` | V1 |
| 标题字距默认、`--text-2` 偏重，描述行抢标题的戏 | `base.css` 类型 token、`views.css` `.pane-title/.pane-desc` | V1 |
| 数字（`22.6 MB`、`203 ms`、`305 requests`）不等宽 | 全局 | V1 |
| `↻ ☾ ☀ ⋯ + ›` 是 Unicode 字符；Token 是顶栏唯一文字按钮 | `index.html`、`sidebar.js`、`pane.js`、`tunnels.js` 等模板 | V2 |
| 二级栏只在 MCP 组出现，切 tab 内容上下跳 ~40px；计数 chip 松散地浮在一级 tab 旁 | `index.html` `#subBar`、`page-registry.js` | V3 |
| pane 内容再居中一次，左右两条空白带 | `views.css` `.pane > * { margin-inline: auto }` | V4 |
| 列表行 ~64px 高；Data 是灰底上两个嵌套白卡；Traffic clients 是 8 张卡装两行字；Plugins 行 `· xx v0.1 · active · pages: xx` + 裸露的 `revision 7416…` | `views.css` tunnels/data 段、`traffic.js`、`views/plugins.js` | V4 |
| Jobs 每行 4 个等权按钮、Tunnels 3 个，红色 Delete 逐行重复 | `polling.js` `jobRowHtml/connRowHtml/ruleRowHtml`、`jobs.js`、`tunnels.js` | V5 |
| 类型标签 10 种颜色但 `mysql/redis` 同红、`pg/http` 同蓝；`idle` 蓝点无解释；sidebar 选中是「白卡浮起」 | `base.css` `.side-type[data-tag]`、`.dot`、`.side-row[aria-selected]` | V6 |
| 空态弱：MCP「Select an MCP」、Data「Select a key on the left」 | `pane.js`、`data-view.js`、各列表页的 `.empty` | V7 |
| 顶栏 <900px 右侧被裁 | `base.css` `.toolbar` | V3 |

**已经对的、保留不动**：`base.css` 头注释的三条规则（mono 只给可复制的值；饱和色只给状态；文字要
有 measure）；`.sw` switch；`.seg` 分段控件的形态；Terminal 的空态（`.term-empty`）；`menu.js`
的 `popupMenu`（V5 直接复用）；主题 pre-paint 脚本。

## 2. 设计决定

### V1 — Token：颜色、文字、表面、焦点、动效（只改 `base.css`）

**颜色（亮）**

```css
--bg: #fafafa;  --sidebar: #f4f4f5;  --bar: #ffffff;  --card: #ffffff;  --field: #ffffff;
--text: #111114;  --text-2: #6b7280;  --text-3: #9ca3af;
--sep: rgba(0, 0, 0, 0.08);  --sep-soft: rgba(0, 0, 0, 0.05);  --hover: rgba(0, 0, 0, 0.04);
--accent: #2563eb;  --green: #16a34a;  --red: #dc2626;  --amber: #d97706;
--shadow-card: 0 0 0 1px var(--sep);            /* hairline only; no drop shadow */
--shadow-btn: none;
```

**颜色（暗）**——偏暖近黑，用亮度表层级，不用投影：

```css
--bg: #0f1012;  --sidebar: #141518;  --bar: #141518;  --card: #191a1e;  --field: #0f1012;
--text: #ededef;  --text-2: #9a9ca3;  --text-3: #66686f;
--sep: rgba(255, 255, 255, 0.08);  --sep-soft: rgba(255, 255, 255, 0.05);  --hover: rgba(255, 255, 255, 0.05);
--accent: #3b82f6;  --green: #4ade80;  --red: #f87171;  --amber: #fbbf24;
--shadow-card: 0 0 0 1px var(--sep), inset 0 1px 0 rgba(255, 255, 255, 0.04);  /* hairline + top highlight */
```

`--shadow-sheet` / `--shadow-pop` 保留投影（浮层需要与页面分离），但把 `0 0 0 1px` 那一层改成
`var(--sep)`。

**文字**

- `body { font-feature-settings: "cv11", "ss01"; font-variant-numeric: tabular-nums; }`——Inter 装了
  就生效，没装无害。
- `.pane-title { font-weight: 600; letter-spacing: -0.02em; }`（在 `views.css`，随 V4 一起改也行）。
- `.side-cap / .group-cap / .sec-cap { font-weight: 500; letter-spacing: .06em; }`。
- `--sans` 改成 `"Inter", "Segoe UI Variable Text", "Segoe UI", -apple-system, system-ui, sans-serif`：
  Windows 11 上落到 Segoe UI Variable，比 Segoe UI 有光学尺寸。加 `font-optical-sizing: auto`。

**圆角**：`--r-card: 8px; --r-row: 6px; --r-btn: 6px;` 标签 4px（V6）。

**焦点环**：`:focus-visible { outline: 2px solid color-mix(in srgb, var(--accent) 55%, transparent); outline-offset: 1px; }`。

**按钮**：`.btn` 保持 hairline（`inset 0 0 0 1px var(--sep)`），去掉 `--shadow-btn`。`.btn.primary`
不变。**新增** `.btn.ghost`：无边、无底、`color: var(--text-2)`，hover 才显 `--hover`——顶栏与行内
次要动作用它。`.btn.danger` 这个类**保留但只允许出现在 `.menu` 里**（V5 会把所有行内 danger 移走）。

**动效**：`.sheet`、`.menu.float` 进场 `opacity 0→1` + `translateY(4px→0)`，120ms `ease-out`，
在 `prefers-reduced-motion` 下关闭。现有 `.12s ease` 的 hover 过渡保留。

> 测试（Node `test/admin-panel.test.ts`）：`base.css` 不再包含 `0 1px 2px`（卡片投影）与
> `#17181c`；包含 `tabular-nums`。这些是回归锚点，不是设计的证明——设计的证明是截图。

### V2 — 图标：一份内联 SVG sprite，替换所有 Unicode 图标

- `index.html` `<body>` 顶部放一个 `<svg hidden>` sprite，Lucide 风格、`viewBox="0 0 24 24"`、
  `stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"`。
  每个 `<symbol id="i-<name>">`。以手写 path 为主、不引入 lucide 包；个别字形沿用 Lucide 的
  ISC 许可 path 数据原文（署名见 THIRD_PARTY_NOTICES §2）。二十个图标 ≤ 4 KB。
  清单：`refresh-cw sun moon key plus ellipsis chevron-right search play history pencil trash power
  terminal database server plug clock check x`。
- `util.js` 新增 `icon(name, label?)`：返回 `<svg class="ic" aria-hidden="true"><use href="#i-name"/></svg>`
  （带 label 时改为 `role="img" aria-label`）。`.ic { width: 16px; height: 16px; flex: none; }`。
- 替换点（grep `&#` 与 `⋯ … › ↻ ☾ ☀ ✓ ×` 找全）：顶栏三个按钮（refresh-cw / sun|moon / key）；
  `#addBtn` 与分组头的 `+`（plus）；分组折叠 `›`（chevron-right，展开时 `transform: rotate(90deg)`）；
  pane 头的 `⋯`（ellipsis）。`.dd-chev` 保留 CSS 画的（它已经是矢量）。
- Token 按钮：`<button class="btn ghost icon" id="tokenBtn" title="Token" aria-label="Token">` + key 图标。
  `main.js` 里 `paintThemeBtn` 改成切换 `<use href>`，不再写 `innerHTML` 字符。

> 测试：`index.html` 含 `<symbol id="i-refresh-cw"`、`i-key`、`i-ellipsis`；`index.html` 与 `js/**`
> 不再含 `&#8635;` `&#9790;` `&#9728;` `&#9681;`；`icon("key")` 的返回值含 `href="#i-key"`。

### V3 — 页栏：始终存在的一条，计数搬到这里（修订 docs/13 D5）

docs/13 选了「顶栏与 shell 之间整宽一条」（D6），理由成立，位置不动。改的是 D5「只有一页时隐藏」：
**页栏始终显示**，36px 高。

- 左：该组 ≥2 页时是二级 `.seg`（`Servers | Traffic`）；只有一页时是页名（`Jobs`）以 `--f-body`/500
  显示——不是空条。
- 右：`#countChip` 从顶栏移到这里（`8 MCPs · 4 up` / `9 rules · 9 active`），`--f-label` / `--text-3`
  / tabular。写它的有两处——`page-registry.js` 里 `currentPageCount()` 的内联调用处与 `polling.js` 的
  `updateCountChip()`——目标都换成页栏里的节点；模块的 `countText()` 契约不变。
- 顶栏：去掉 `#countChip`；右侧改为 `memChip`（`font-family: var(--mono)`）+ 三个 `.btn.ghost.icon`，
  `gap: 2px`。`@media (max-width: 960px) { #memChip { display: none } }`；`.toolbar { min-width: 0 }`，
  `.seg` 允许 `overflow-x: auto` 且隐藏滚动条。**支持的最窄宽度是 960px**：到 960 为止不得裁切；
  以下不保证布局，但除了出现水平滚动条外不得有其他破坏。
- `docs/13-panel-navigation-spec.md` 头部状态行加一句：「D5 由 docs/18 V3 修订：页栏常驻」。

> 测试：`test/admin-navigation.test.ts` 现有的「一页时 `#subBar` hidden」断言反转为「常驻且含页名」；
> 二页时含两个 tab；`#countChip` 在页栏内而不在 `.toolbar` 内。

### V4 — 布局：左对齐、密度、三个页面的骨架

- `views.css`：`.pane > * { max-width: var(--measure); margin-inline: 0; }`——**去掉居中**，改掉那段
  「Centred, not left-hugging」注释（写清为什么改回来：sidebar 已经把 pane 推离左边，再居中就是两条
  空白带）。`--measure: 920px; --measure-wide: 1180px;`。`.pane { padding: var(--s5) var(--s8) var(--s8); }`。
- `.tun-row { padding: var(--s2) var(--s4); min-height: 42px; }`，`.tun-sub` 与 `.tun-name` 行高收到 1.35。
- **Data**：`views.css` data 段——外层灰画布上不再有「卡中卡」：key 列表与值区各自是 `--card` 底 +
  hairline，二者之间一条 `--sep` 竖线，不再各自带圆角卡片套在另一张卡里。具体选择器实施时读
  `views.css` 302–440 行与 `data-view.js`；只动容器的边框/背景/圆角，不动网格与编辑逻辑。
- **Traffic clients**：`traffic.js` 里 clients 从卡片改为紧凑行：`display: grid; grid-template-columns:
  minmax(160px, 1fr) auto minmax(0, 2fr) auto auto; gap: var(--s3); min-height: 36px;`，列 = 名字/版本
  （mono）、token、路径（截断）、最近时间、请求数（右对齐 tabular）。点击行仍然过滤（行为不变）。
- **Plugins**（`views/plugins.js`）：行 = 状态点 + 名字 + 一行灰字（只留 `pages: mcps, traffic` 或
  `no page`；`v0.1` 与 `active` 去掉——版本进 title，状态由点表达）；右侧 Disable/Enable 按钮改为
  `.sw` switch（`role="switch" aria-checked`），busy 时 `disabled`。`revision …` 移到列表页脚
  （`.tun-foot`）的右侧，`--f-caption` mono。API 调用与 `busy` 处理不变。

> 测试：`test/admin-plugins-view.test.ts`——行含 `role="switch"`、不含 `>Disable<`；
> `views.css` 含 `margin-inline: 0`。

### V5 — 行操作：一个主操作 + ⋯，红色离开行

复用 `menu.js` 的 `popupMenu(anchor, items)`（items: `{label, fn, danger, sep}`），anchor 是按钮的
`getBoundingClientRect()`。

| 行 | 主操作（唯一 `.btn`） | ⋯ 菜单 |
| --- | --- | --- |
| Job（`jobRowHtml`） | `Run now`（busy 时 `…` disabled 保持） | History · Edit · ─ · Delete（danger） |
| SSH 连接（`connRowHtml`） | `Test` | Edit · ─ · Delete |
| 端口规则（`ruleRowHtml`） | `Start` / `Stop`（按状态） | Edit · Force free（仅 `portOwner` 时出现，danger）· ─ · Delete |
| MCP 详情头（`pane.js`） | 已是 Stop/Start + ⋯ | 不动 |

- 行模板里 `data-hist` / `data-edit` / `data-del` / `data-free` 这些 hook 改为一个 `data-more` 按钮
  （`.btn.ghost.icon` + ellipsis 图标）；`jobs.js` / `tunnels.js` 的绑定改成在 `data-more` 点击时
  `popupMenu(...)`，各项的 `fn` 就是原来绑在那些按钮上的函数——**函数本身不动**。
- `.tun-acts` 里只允许出现一个非 icon 的 `.btn`；`.btn.danger` 不得出现在 `.tun-row` 内。
- `.menu` 项：高 30px、`--f-body`、hairline 边、`--r-card` 圆角、danger 项 `--red` 字。

> 测试：`test/admin-jobs-v2.test.ts`（或新建 `admin-row-actions.test.ts`）——`jobRowHtml(j)` 输出
> 恰有一个非 icon `.btn`、含 `data-more`、不含 `data-del` 与 `class="btn danger"`；
> `connRowHtml` / `ruleRowHtml` 同理；`portOwner` 有值时菜单 items 含 Force free。

### V6 — 状态与标签：颜色只给状态，形状表达 idle

- `.dot { width: 6px; height: 6px; }`；`.dot.idle { background: transparent; box-shadow: inset 0 0 0 1.5px var(--text-3); }`
  （空心环 = 没在跑，随时能起）。**每个 `.dot` 带 `title`**：`up · 203 ms` / `idle — starts on first request`
  / `error: <reason>` / `starting`；模板里已有的状态字符串直接复用。
- **类型标签单色**：删掉 `base.css` 里 `.side-type[data-tag=…]` 的 20 行色表，只留
  `.side-type { font-family: var(--mono); font-size: var(--f-caption); color: var(--text-2); background: var(--sep-soft); border-radius: 4px; padding: 1px 5px; }`。
  `data-tag` 属性保留在 DOM 里（测试与将来都可能用）。
- **sidebar 选中态**：`.side-row[aria-selected="true"] { background: color-mix(in srgb, var(--accent) 8%, transparent); box-shadow: inset 2px 0 0 var(--accent); }`
  `.side-name` 选中时 `color: var(--text); font-weight: 600;`（不再变蓝——蓝条已经说明了）。
  改掉那段「Finder / Xcode source-list」注释。
- `.seg button[aria-selected="true"]` 亮色去掉投影，只留 `--card` 底 + hairline；暗色同。

> 测试：`base.css` 不含 `--tag-fg: #`（色表已删）；含 `inset 0 0 0 1.5px`（空心环）。

### V7 — 空态：一个模板，四处使用

- `util.js` 新增 `emptyHtml({ icon, title, hint, action })` → `<div class="empty"><div>` + 20px 图标
  （`.ic.big`，`--text-3`）+ `<h2>` + `<p class="hint">` + 可选 `<button class="btn ghost" data-empty-action>`。
- 使用处：`pane.js` 未选中 MCP（icon `server`，"Select an MCP"，action "Add an MCP" → 触发现有的
  add-sheet）；`data-view.js` 未选 key/表（icon `database`）；`jobs.js` / `tunnels.js` / `plugins.js`
  的列表空态（icon `clock` / `plug` / `power`）。Terminal 的 `.term-empty` 保持原样（它在黑框里有自己
  的 token）。
- `.empty h2 { font-size: var(--f-head); font-weight: 550; color: var(--text-2); }`、
  `.empty p { font-size: var(--f-label); color: var(--text-3); max-width: 44ch; }`、
  图标与标题间 `--s3`，标题与说明间 `--s1`，说明与按钮间 `--s4`。

> 测试：`emptyHtml({icon:"server", title:"Select an MCP"})` 含 `href="#i-server"` 与 `<h2>Select an MCP</h2>`；
> 带 `action` 时含 `data-empty-action`。

### V8（可选）— 嵌入 Inter

只在 V1–V7 都落地、用户看过 19998 之后再决定。代价与做法：

- 字体文件 `admin/fonts/InterVariable.woff2`，**subset 到 latin + latin-ext**，门槛 ≤ 130 KB；
  `@font-face { font-family: Inter; src: url(/admin/fonts/InterVariable.woff2) format("woff2"); font-weight: 100 900; font-display: swap; }`。
  `.gitattributes` 已把 `*.woff2` 标为 binary。
- **Rust 侧必须改**：`crates/swiss-panel/src/admin.rs::admin_asset` 现在把资源转成 `String`
  （`from_utf8_lossy`）且 `mime_of` 只认 html/css/js/svg——二进制会被打坏。改为返回 `Cow<[u8]>`，
  `mime_of` 加 `"woff2" => "font/woff2"`；调用方（`src/app.rs` 的 admin 路由）相应改 body 类型。
  一条测试：`admin_asset("fonts/InterVariable.woff2")` 的字节与嵌入文件逐字节相等。
- Node 侧 `src/router.js` 的静态服务需要能发 woff2（实施时确认它是否按扩展名白名单）。
- 提交信息里写：exe 体积变化（`ls -l target/release/swiss.exe` 前后）、19998 冷启动 RSS 变化。
  体积增长 > 200 KB 或 RSS 有可测增长（> 0.5 MB）则**不合入**，把数字写进 docs/07 的 ADR 备忘。

## 3. 顺序、提交与门禁

**V1 → V2 → V3 → V4 → V5 → V6 → V7 →（V8）**，一阶段一个提交（Node 仓库一个、本仓库一个「copy
panel」提交，或者 Node 侧攒到一起再一次复制——二选一，但**本仓库每次复制都是完整目录**）。
不能换序：V2 的 `icon()` 是 V5/V7 的依赖；V3 动了 `page-registry.js`，V4 动 `views.css` 的同一片区域。

每个提交：

- 行为变化先有测试（每节「测试」是最少集合），证明改前红、改后绿。视觉变化没有单元测试，用截图。
- Node 仓库：`npx vitest run`（全量；`test/admin-jobs-v2.test.ts:76` 有一个**既有**的 typecheck
  失败，不是你的，不要修）。
- 本仓库复制后：`cargo test --workspace`；`cargo clippy --workspace --all-targets -- -D warnings`。
  `--workspace` 不可省。
- 代码注释英文；改掉一段注释时要保留它原来记录的「为什么」，加上现在为什么变。

## 4. 怎么验：截图对照

每个阶段在 19998 上（`scripts/test-instance.ps1 -Start`；重建用
`$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`，≈2 分钟；面板是 rust_embed 的，
改了就要重建 + 重启 19998）用 agent-browser **命名会话**截图：

- 视口 1440×900：MCP 空态、MCP 详情（`mysql` → Tools）、MCP Config、Traffic、Tunnels、Data、Jobs、
  Terminal（不开会话）、Gateway；再切暗色重截 MCP 详情、Jobs、Data。
- 视口 960×900：MCP 详情、Jobs——顶栏不得裁切。
- 截图放 scratchpad，**不提交进仓库**（仓库根现在那几张 `term-*.png` 是历史遗留，不要再添）。
  提交信息里列出核对过的画面。
- 全部阶段完成后与 17 的画布逐板对照，差异写进最终汇报。

**19999 不动。** 用户看过 19998 之后自己部署。

## 5. 不做什么

- 不加 `⌘K` 命令面板、不加设置页、不改任何 API、不改 Rust 侧（V8 除外）。
- 不引入 lucide / heroicons npm 包、不引入 Tailwind、不引入任何构建步骤。
- 不重排 `views.css` 的段落顺序、不合并两个 CSS 文件、不把类名批量改名。
- 不改 Terminal 视图的内部（xterm 主题、黑框）——它有自己的 token 体系（docs/14）。
- 不做 <960px 的布局。
- 不做玻璃拟态、渐变、多层投影、大圆角、彩色标题、插画。

## 6. 交给实施模型的 Prompt

> 复制下面整段。它假设模型在 `<repo>` 下工作，旁边有
> `..\local-mcp-gateway`（Node 仓库，面板源码在那里）。

---

你在 `<repo>` 工作 —— Rust 版网关，产物是单个 `swiss.exe`。
旁边 `<node-repo>` 是 Node 版；它的 `src/admin/` 是面板源码的**唯一
事实来源**，本仓库 `crates/swiss-panel/src/admin_assets/` 是逐字节拷贝，只能整目录复制、不能手改，
测试 `the_tree_is_byte_for_byte_the_node_builds` 会拦住任何分叉。

任务：实施 `docs/18-panel-visual-refresh-spec.md`（面板视觉刷新，V1 token → V2 图标 → V3 页栏 →
V4 布局 → V5 行操作 → V6 状态与标签 → V7 空态；V8 可选，先不做）。先把 spec 读完，再读它开头列的
前置文档，尤其 `AGENTS.md`——它的规则高于 spec 和这段话。如果 `docs/17-panel-design-canvas-spec.md`
的状态行里有画布 URL，用 Artifact 工具读它：视觉以画布为准，约束以 18 为准。

动手前读这些文件，理解现状再改（spec §1 已经列了根因，不要重新猜）：

- `src/admin/styles/base.css` 全文（尤其头 100 行的注释——你改的是值和几条规则，注释里的「为什么」
  要保留并追加「现在为什么变」）
- `src/admin/styles/views.css` 的 `.pane > *`、`.empty`、`.seg`、tunnels 段、data 段
- `src/admin/index.html`、`js/main.js`（顶栏、主题按钮）、`js/page-registry.js`（两级导航、
  `currentPageCount`——它与 `polling.js` 的 `updateCountChip()` 是写 `#countChip` 的两处）、
  `js/util.js`（你要加 `icon()` 与 `emptyHtml()` 的地方）
- `js/menu.js::popupMenu`（V5 直接复用）、`js/polling.js` 的三个 `*RowHtml`、`js/jobs.js` 与
  `js/tunnels.js` 里对 `data-hist/data-edit/data-del/data-free` 的绑定、`js/views/plugins.js`、
  `js/traffic.js`、`js/pane.js`（MCP 空态与详情头）、`js/data-view.js`（Data 空态）
- `test/admin-panel.test.ts`、`test/admin-navigation.test.ts`、`test/admin-jobs-v2.test.ts`、
  `test/admin-plugins-view.test.ts`（你要扩展的测试在这几个文件里，照它们的写法）
- `docs/13-panel-navigation-spec.md` §3 D5/D6（V3 修订 D5，保留 D6）

交付：**每个阶段一个提交，顺序 V1 → V7**。Node 仓库提交后整目录复制到本仓库并提交（提交信息
`chore(panel): copy panel — docs/18 V<n>`）。每个提交的硬性要求：

- 行为变化先有测试（spec 每节「测试」是最少集合），证明没改时会红。
- 门禁全绿：Node `npx vitest run`（全量；`test/admin-jobs-v2.test.ts:76` 有既有 typecheck 失败，
  不是你的，别修）；本仓库 `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。
- 不加任何依赖（npm 或 crate）、不加构建步骤、不改任何 `/api/*` 形状、不加功能。
- 代码注释英文；文档散文中文。
- 不提交 `gateway.config.json`、`.env`、`managed.json`、`tunnels.json`、`master.key`、`*.log`、截图。
- Commit message 英文、照 `git log` 现有提交的口吻写；不加 attribution / Co-Authored-By trailer
  （两仓库的 AGENTS.md 都没有 attribution 规则，历史提交也没有 trailer）。

运行与验证的铁律：

- **19999 是生产实例，不停、不重启、不部署。**
- 实测全在 19998：`scripts/test-instance.ps1 -Start`（第一次可 `-Fresh`）；面板是 rust_embed 的，
  每次面板改动要 `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`（≈2 分钟）再
  `scripts/test-instance.ps1 -Stop` / `-Start`。永远不要 `--port`。
- 停 19998 只用 `scripts/test-instance.ps1 -Stop`（按端口 PID）。绝不 `Get-Process swiss`。
- agent-browser 用自己命名的会话（`agent-browser session id --scope worktree --prefix refresh`），
  按 spec §4 的清单截图（亮 9 张、暗 3 张、960 宽 2 张），放 scratchpad，提交信息里列出核对过的画面。
  做完 `agent-browser close`。
- `cargo build` 报 `os error 5` 是 exe 被 19998 占着——先 `-Stop`，不要换目录绕。

不要做：spec §5 列的一切；「顺手」fmt、重排 CSS 段落、批量改类名；在 Rust 侧改任何东西（V8 不在
本次范围）；碰 Terminal 视图内部。

汇报时按 V1–V7 各一段：改了什么、测试加了什么、截图核对了哪些画面、与 17 画布的差异（若有），
以及你自己拿不准、建议用户复核的地方——尤其是颜色数值这种品味判断。
