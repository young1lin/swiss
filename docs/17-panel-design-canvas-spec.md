# 17 — 面板视觉刷新：设计稿（Design Canvas）规范

> 状态：**待做**。产出物是一张 `/design` 画布（多画板、可拖拽编辑的 Artifact），不是代码。
> 它是 [18](18-panel-visual-refresh-spec.md)（实施规范）的**视觉输入**：用户在画布上确认方向后，
> 实施模型按 18 落地；两者冲突时，画布决定「长什么样」，18 决定「怎么做、不能碰什么」。
> 本文散文中文；画布上的文字（UI 文案）一律英文，与真实面板一致。

## 0. 为什么先出设计稿

现在的面板（`crates/swiss-panel/src/admin_assets/`）底子是规整的：五档字号、4pt 网格、四层表面、
亮/暗两套 token（`styles/base.css` 开头 100 行）。它"不好看"不是配色错了，而是**构图**问题：

1. 六个顶层 tab 是四种页面骨架（MCP 是 sidebar+详情；Tunnels/Jobs/Plugins 是居中页；Data 是灰底上
   两个嵌套白卡；Terminal 是全出血黑框），切 tab 像切四个产品。
2. 内容在 pane 里再居中一次（`.pane > * { margin-inline: auto }`），产生左右两条空白带，内容"漂"着。
3. 列表行每行三四个等权重按钮，红色 Delete 逐行重复；`base.css` 自己写的规则是"一个主操作，其余进
   溢出菜单"，MCP 详情遵守了，Jobs/Tunnels 没有。
4. 颜色没有信息量：`mysql`/`redis` 同红，`pg`/`http` 同蓝；蓝色状态点没人知道是什么；蓝按钮、蓝
   switch、蓝 tab 高亮、彩色 chip 同时在场，眼睛没有落点。
5. 图标是 Unicode 字符（`↻ ☾ ☀ ⋯ + ›`），粗细基线各不相同；Token 是右上角唯一的文字按钮。
6. 空态弱："Select an MCP" 孤悬灰块中央；Data 的提示是空白大卡左上角一行小字。Terminal 的空态
   （图标 + 标题 + 提示）反而是对的。

要的方向是 **Linear / Vercel dashboard 那一路的"克制"**：单色 + 一个 accent、标题负字距、hairline
卡片、暗色近黑、一套线性图标、`tnum` 数字。不是玻璃拟态、不是渐变、不是大圆角。

先画再写代码，是因为这些是品味判断，用户要**看着**决定，而不是读 CSS 猜。

## 1. 画布里要有什么

一张画布，**八块画板**，全部 1440 × 900（面板的常见桌面尺寸），按下面顺序从左到右、两行排列
（上行亮色，下行暗色）。画板名用英文。

| # | 画板名 | 内容 | 主题 |
| --- | --- | --- | --- |
| 1 | `MCP / Servers — detail` | 左 sidebar（三个分组、8 个 MCP、类型标签、状态点），右侧 `mysql` 详情：标题、描述、状态行、Tools 2 / Resources / Prompts / Run / Config / Logs 分段、两张工具卡（名字 + 描述 + 参数 + Try + switch）、右上 Stop + ⋯ | 亮 |
| 2 | `MCP / Servers — empty` | 同 1 的 sidebar，右侧未选中的空态：图标 + "Select an MCP" + 一行提示 + ghost 按钮 "Add an MCP" | 亮 |
| 3 | `Jobs` | 列表页骨架：页面标题 + 一行描述；分区标题 "Scheduled commands" 右侧 New（唯一实心按钮）；四行 job（状态点、名字、mono 命令、cron 与 next 时间）；每行只有 **Run now + ⋯**；页脚 "4 jobs · 3 on" | 亮 |
| 4 | `Traffic` | Clients 改成紧凑表格（名字/版本、token、路径列表、最近时间、请求数右对齐 tnum）；下面 Activity 列表 6 行（method、截断的 JSON、client、路径、ok、耗时、时间） | 亮 |
| 5 | `MCP / Servers — detail` | 同 1 | 暗 |
| 6 | `Jobs` | 同 3 | 暗 |
| 7 | `Data` | 左：连接下拉 + 过滤框 + 4 个 redis key（名字、类型、ttl）；右：选中一个 key 的值视图（标题、元信息行、值区）。去掉"卡中卡"，用 hairline 分隔 | 暗 |
| 8 | `Components` | 组件样张，不是页面：按钮四态（primary / default / ghost / disabled）、switch 开关、状态点（up / idle 空心 / error / starting）、类型标签（单色版）、分段控件、输入框 + 焦点环、⋯ 菜单展开态（含一条红色 Delete 项与分隔线）、toast、空态模板、四档文字（title / head / body / label / caption）、图标集 16px 一排 | 亮 + 暗各一半 |

每块画板都要包含**顶栏**（品牌 + 一级 tab + 右侧内存/刷新/主题/Token 图标）和**页栏**（见 §2.4），
因为这两处是最先决定"像不像一个产品"的地方。

真实数据用截图里的：MCP 名 `mysql shop redis shop-redis web-reader web-search-prime zai-vision colab`；
分组 `DEFAULT 7 / LEARN 1 / FORTEST 0`；job 名 `env-check claude-hello-0559 claude-hello-1100 claude-hello-1601`；
内存 `22.6 MB`；client 名 `mcp-gateway 1.0 / w2-drive 1.0 / rh-mcp-client 0.0.1`。不要 lorem ipsum。

## 2. 设计决定（画板必须体现的）

这些是给设计模型的**约束**，不是建议。用户看画布时会核对。

### 2.1 颜色

- 亮色：画布近白（`#fafafa`），sidebar 一档灰（`#f4f4f5`），卡片纯白，**只靠 1px hairline**
  （`rgba(0,0,0,.08)`）区分，不用投影。文字三档：`#111114 / #6b7280 / #9ca3af`。
- 暗色：偏暖近黑（`#0f1012`），sidebar/顶栏 `#141518`，卡片 `#191a1e`，卡片顶边一条 4% 白的高光。
  **不是**现在的蓝灰（`#17181c / #24262c`）。文字 `#ededef / #9a9ca3 / #66686f`。
- 一个 accent（蓝 `#2563eb` 亮 / `#3b82f6` 暗）。**每块页面画板最多一个实心 accent 按钮。**
- 红色只出现在：error 状态点、⋯ 菜单里的 Delete 项、错误 toast。行内没有红色按钮。
- 绿色只出现在 up 状态点。amber 只出现在 starting/stopping。
- 类型标签（`mysql / pg / http / uvx / proc`）**全部单色**：灰底（5% 黑）灰字 mono 11px。

### 2.2 文字

- 标题 22px / 600 / 字距 −0.02em。分区标题 11px 大写 / 500 / 字距 +0.06em / text-3。
- 正文 13px，次要 12px，说明 11px。全局 `tabular-nums`：内存、耗时、计数、时间列对齐。
- 字体按 Inter 画（实施时嵌入或落到 Segoe UI Variable，见 18 §V1）。
- mono 只给"你会复制的值"：路径、命令、工具名、JSON、key 名。**标题永远不是 mono。**

### 2.3 布局

- 内容**左对齐**，`max-width` 920px（宽表 1180px），空白只在右侧。不居中。
- 行高：列表行 40–44px（现在 ~64px）。sidebar 行 30px 不变。
- 页面骨架统一为两种：**sidebar + 详情**（MCP、Data）与 **列表页**（Tunnels、Jobs、Plugins）。列表页
  的头部 = 标题 + 描述 + 右侧一个主按钮；分区标题行 = 大写小标题 + 右侧计数。
- 圆角：卡片 8px，按钮/行 6px，标签 4px。不出现 >10px 的圆角。

### 2.4 顶栏与页栏

- 顶栏 48px：左 shield 图标 18px + `swiss` 字标（600，−0.02em）；一级 tab 分段控件；右侧
  `22.6 MB`（mono tnum 小胶囊）、刷新 / 主题 / **钥匙**三个 16px 线性图标按钮。**没有文字按钮。**
- 顶栏下**始终有一条 36px 页栏**（现在只有 MCP 组有，切 tab 会跳）：左侧是该组的二级 tab（≥2 页时，
  如 `Servers | Traffic`）或者该页的名字；右侧是该页的计数文案（`8 MCPs · 4 up` / `9 rules · 9 active`）。
  计数从顶栏挪到这里。

### 2.5 图标

- 一套 16px 线性图标（Lucide 风格，stroke 1.5）：refresh-cw、sun、moon、key、plus、ellipsis、
  chevron-right、search、play、history、pencil、trash、power、terminal、database、server、
  plug、clock、check、x。Components 画板把它们排一排。
- 不出现任何 Unicode 符号图标。

### 2.6 状态与控件

- 状态点 6px；`idle`（lazy、未起子进程）画成**空心圆环**而不是蓝色实心；每个点都有 tooltip 文案
  （画板上用一个展开的 tooltip 示意即可）。
- sidebar 选中态：左侧 2px accent 竖条 + 8% accent 淡底；不再是"白卡浮起"。
- 焦点环：accent 55% 透明的 2px 外框，offset 1px。
- switch 与现在相同（accent 色、38×22）。
- ⋯ 菜单：hairline 边、8px 圆角、项高 30px、危险项红字、分隔线；Components 画板画展开态。

### 2.7 空态模板

垂直居中：20px 线性图标（text-3）→ 15px/550 标题（text-2）→ 12px 说明（text-3）→ 可选一个 ghost
按钮。MCP 空态、Data 空态、列表页无数据都用它。Terminal 已经是这个形态，画板不必再画 Terminal。

## 3. 不要画的

- 渐变、玻璃拟态、多层投影、彩色标题、插画风空态、大于 10px 的圆角。
- 新功能：命令面板、设置页、新的页面。这是刷新，不是改版。
- 移动端。支持的最窄宽度是 960px，画板不需要窄屏版本。
- 中文 UI 文案。面板是英文的。

## 4. 验收

用户在画布上逐板核对 §2 的每一条；画板与 §2 冲突处以 §2 为准修改，除非用户明确说"就要这样"。
用户确认后把画布 URL 记到本文头部「状态」一行，18 的实施模型据此开工。

## 5. 交给设计模型的 Prompt

> 复制下面整段。它假设模型能调用 `/design`（design canvas）技能，并且能用 `agent-browser` 打开
> `http://127.0.0.1:19999/` 看现状（只看，不点任何会改状态的东西——Delete、Disable、Stop、Run now、
> 保存表单都不能碰；切 tab、选中 MCP、切主题可以）。

---

你在 `<repo>` 工作。任务：为这个项目的管理面板出一张**设计稿画布**，
规范在 `docs/17-panel-design-canvas-spec.md`，先把它读完；再读 `AGENTS.md` 的「The product」一节
和 `crates/swiss-panel/src/admin_assets/styles/base.css` 开头 100 行（现有 token 和它的设计意图），
以及 `docs/13-panel-navigation-spec.md` §2（两级导航为什么是现在这样）。

看现状：用 agent-browser 开一个**自己命名的会话**（`agent-browser session id --scope worktree --prefix design`），
打开 `http://127.0.0.1:19999/`，1440×900，把六个顶层 tab、MCP 详情（点 sidebar 里的 `mysql`）、
Traffic、以及切到暗色后的 MCP 详情和 Jobs 各截一张。**19999 是用户的生产实例**：只看，不改；
不要点 Delete / Disable / Stop / Run now / Test / 任何保存按钮；不要开 Terminal 会话。看完把主题切回
（点一次主题按钮或 `localStorage.removeItem('swiss_theme')`）并 `agent-browser close`。

然后用 `/design` 做画布：八块画板，尺寸、内容、主题、顺序按 spec §1 的表；每一块都体现 §2 的全部
约束（§2 是硬约束，不是建议）；§3 列的东西一样都不要画。用户会拖着画板改，所以元素要拆得开
（每个按钮、每行、每个标签是独立元素，不是一张图）。UI 文案英文，数据用 spec §1 末尾列的真名。

交付：画布的 Artifact 链接，加一段不超过十行的说明：你在 §2 之外自己做的判断有哪些（例如具体
的行高、间距数字），以及你觉得 §2 里哪一条画出来效果不好、建议用户复核。不要写代码，不要改仓库
里任何文件。
