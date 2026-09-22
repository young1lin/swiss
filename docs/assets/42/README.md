# Data 页面重构 — 四套布局方案对照

打开 `data-layout-mockup.html`（双击即可，纯静态、零依赖）。顶部深色条是**选型工具条**，不属于设计本身：

| 控件 | 作用 |
| --- | --- |
| Study | A / B / C / D 四套布局（键盘 `1`–`4` 直接切） |
| Engine | MySQL / PostgreSQL / Redis —— 同一套布局在三种引擎下的样子 |
| State | 该引擎下的全部状态（SQL 18 种、Redis 12 种） |
| Theme | 亮/暗双主题 |
| Width | 1440 / 900，验证窄屏不塌 |

色板、字号、间距、圆角全部**逐字抄自** `crates/swiss-panel/src/admin_assets/styles/base.css` 的 token 块，所以看到的差别只有结构差别，没有配色漂移。外框高度跟随窗口，底部对齐的部分（D 的 dock、C 的状态条）在小屏也能完整看到。

## 现在这一版的问题（对着代码，不是对着感觉）

红箭头指的那一行是 `data-grid.ts:455 renderDbToolbar()` 的 `.db-head-ctl`。它一行里塞了：

```
[Data|Form|Columns|Indexes|Foreign Keys|DDL]  [50▾] [1–50 of 550,968] [‹][›] [Refresh] [+Row] [CSV] [Export…] [Import…] [SQL] [⋯]
```

1. **切视图的控件和执行动作的控件同权重、同一行**。分段控件（L3）和 8 个动作按钮长得一样重，眼睛分不出"这几个换我在看什么"和"这几个会改数据"。
2. **一行里有 6 个非图标按钮**，违反 swiss-ui-design 规则 4（一个视图一个主动作，其余进 `⋯`）；Export / Import / CSV / Refresh 都是低频项，本就该收进溢出菜单。
3. **六个结构 tab 撑满了 L3 预算**。Columns / Indexes / Foreign Keys 是同一件事（表结构）的三张表，可以折成一个 Structure，内部再分。
4. **标题块和 tab 条抢同一行的左侧**，tab 条被挤到中间浮着，没有对齐锚点。
5. **侧栏在列表开始前叠了 4 层控件**（连接 select、schema select、grep、排序行），1712 张表的源列表却用了两行高的行（名字 + `table · ~9,309 rows · 7.0 MB`），翻起来很累。
6. **一次只能开一个对象**。成熟 Web 客户端（DbGate / CloudBeaver / DataGrip）都是"打开的对象 = tab"，可以一边看 A 表一边写 SQL；我们换表就丢过滤、丢分页、丢编辑缓冲。
7. **SQL 控制台是个开关，不是个地方**。展开时把网格往下顶，收起时查询上下文就没了。
8. **Redis 复用了同一副骨架**（tables→keys、SQL→Command），但 Redis 的天然形状是"键空间树 + 类型化值面板"，不是表格。450 个键铺平在一列里没法用。

## 四套方案

| | 方案 | 一句话 | 主要代价 |
| --- | --- | --- | --- |
| **A** | Two rows | 把那一行拆成两行：上行身份 + `⋯`，下行视图切换 + 只属于当前视图的控件。6 个结构 tab 折成 4 个 | 最小，DOM 结构基本不动，单对象模型保留 |
| **B** | Object tabs | 打开的对象变成 tab（两张表 + 一个 SQL 编辑器同时在），侧栏变成真的树，Redis 按 `:` 前缀折叠命名空间 | 需要把 `DbState` 拆成 per-tab 状态 |
| **C** | Quiet | 只留一条 36px 面包屑；分页、耗时、可编辑性下沉到底部状态条；过滤器变成一条 `WHERE` 查询栏；低频动作靠 `⌘K` | 网格最大、噪音最低，但发现性依赖命令面板 |
| **D** | Docked workspace | 上网格下 dock（SQL / Value / Row / Messages / Activity，可拖高度），控制台不再顶走网格，值查看器不再是 modal | 最能干，但固定吃掉一块纵向空间 |

四套都**没有动 App Shell**：左侧 rail（L1）、上方 context bar（L2）原样保留，改的只有 page body 自己那块（swiss-ui-design §2、§6C 的 workspace 模板）。

## 覆盖到的状态

**SQL（MySQL / PostgreSQL 各 18 种）**：常规浏览、带过滤、缓冲编辑（脏格 + 新行 + 删除行 + amber 提交条）、只读连接、无主键表、空表、驱动报错原样透出、Form 单记录、Structure、DDL、SQL 控制台双结果 tab、执行计划、活动监控、无连接空态、CSV 导入向导、值查看器、新建表 sheet、溢出菜单展开。

**Redis（12 种）**：键列表、string / hash / list / set / zset / stream 六种类型、缓冲编辑 + 命令预览、命令控制台、SCAN 空结果、重命名/TTL/删除菜单、无连接。

PostgreSQL 额外体现多 schema 分组、`nextval` 默认值、`jsonb` 列、FK 跳转箭头；MySQL 体现列注释（中文）、近似行数、`SHOW CREATE TABLE` 形态的 DDL。

## 结论：选了 B

owner 2026-09-21 定案：**B（对象页签）为骨架**，并入 **C** 的三个零件（底部状态条、单行侧栏行、
monospace `WHERE` 栏），**D** 的 dock 留作第二期。理由、状态切分表、T1–T5 的交付顺序与验收，
全部在 [`docs/42-data-object-tabs-spec.md`](../../42-data-object-tabs-spec.md)；交给实施会话的
零上下文提示词在 [`docs/42-data-object-tabs-prompt.md`](../../42-data-object-tabs-prompt.md)。

一句话的理由：A 只治工具条拥挤，不治换表即丢过滤、分页和缓冲编辑这个真缺口；C 的 `⌘K` 优先是
桌面习语，对偶尔打开的面板发现性是净损失；D 仍然只握一个对象，且它是 B 的第二期而不是替代品
（顺序反了要付两遍钱）。B 是唯一正面解决单对象模型的，并且顺带把工具条和 Redis 键空间一起做掉。

这个 worktree 的 `crates/swiss-panel/panel/node_modules` 还没装，第一次跑 `npm run check` 前先在
该目录 `npm ci`。测试面是 31 个 `admin-data-*.test.ts`（4,454 行），T1 要求它们**只改访问器名**就全绿。
