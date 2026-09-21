# 42 — Data 的对象页签：一次握住多个对象

> 状态：**草案，待 owner 复核**。基线 `801c834`（master，2026-09-21）。实施分支 `data-full-access`
> （worktree `.agents/worktrees/data`，建于 `755eb9e`，**落地前先 rebase 到 master**，中间三个提交是
> docs/41 的 remote 工作，与本文无关）。视觉参考 `docs/assets/42/data-layout-mockup.html`（双击打开，
> 键 `1`–`4` 切方案，`2` 就是本文的目标；Engine 与 State 两个下拉覆盖 MySQL / PostgreSQL 各 18 态、
> Redis 12 态）。
>
> 前置阅读：`.claude/skills/swiss-ui-design/SKILL.md` §1–§8（四层与归属——本文新增的页签**不新增层**，
> 见 §1.2）、§16 规则 3 / 4 / 5 / 11 / 16 / 18 / 19；`.agents/rules/panel-proof-of-life.md` 全文；
> `docs/21-data-web-gap-analysis.md`（35 条差距的排序与"不做"清单）；`docs/22-data-parity-spec.md`
> （W0–W5 三十二项**已全部交付**——所以本文不是补功能，是改信息架构）；`docs/37-panel-modern-typescript-spec.md`
> §7（`h()` 与重画）与 R4（`db-state.ts` 为什么长这样）。
> 代码注释与 UI 文案是英文；中文只在本文。

> 需求原文（owner，2026-09-21，附 Data 页截图，红箭头指向工具条那一行）：
> "在 `.agents/worktrees` 下面，创建一个 Worktree，叫 data 然后这个分支就叫 data-full-access，这个分支
> 主要是把 Data 的页面做个重构。相关测试代码，也做个重构。你可以看看，页面上，好多元素，但是总感觉和
> 成熟的 Web 版本数据库连接的工具差很多。你可以看看相关的功能介绍文档。然后写个 HTML，看看我这几个
> Redis、PGSQL，MYSQL 等，这几个要怎么布局，怎么设计比较好，HTML 放到本地，让我选择不同的页面风格
> 我看看哪个更好点"
>
> 追加一："尽量每种情况都要考虑到"（→ mockup 覆盖 30 种状态，§1.1 表末）。
>
> 追加二（看过四方案后）："我感觉除了 A，每个都有它的特点和优点，如果专注于 Web 端体验的话，你觉得
> 哪个会更好？" → 答 **B（对象页签）为骨架，C 的状态条与单行侧栏并入，D 的 dock 留作后续**，理由见
> §1.1。owner："你的建议非常棒，我建议开始写零上下文的 spec 我丢给其他的模型来执行。"

## 0. 现状与缺口

### 0.1 红箭头那一行

`data-grid.ts:455 renderDbToolbar()` 的 `.db-head-ctl`，一行里同权重地排着：

```
[Data|Form|Columns|Indexes|Foreign Keys|DDL]  [50▾] [1–50 of 550,968] [‹][›] [Refresh] [+Row] [CSV] [Export…] [Import…] [SQL] [⋯]
```

四条硬伤，每条对着一条 swiss-ui-design 规则：

| # | 问题 | 违反 |
| --- | --- | --- |
| 0.1.1 | 切视图的分段控件（L3）与 8 个执行动作的按钮长得一样重，眼睛分不出"换我在看什么"与"会改数据" | §16 规则 18（同类控件归一列） |
| 0.1.2 | 一行 6 个非图标 `.btn`（Refresh / +Row / CSV / Export… / Import… / SQL） | §16 规则 4（一视图一主动作，其余进 `⋯`） |
| 0.1.3 | 六个结构 tab 吃满 L3 预算；Columns / Indexes / Foreign Keys 是同一件事（表结构）的三张表 | §13 清单第 8 条 |
| 0.1.4 | 标题块与 tab 条抢同一行左侧，tab 条浮在中间没有对齐锚点 | §16 规则 16（一个面一条边） |

### 0.2 真正的缺口：单对象模型

工具条拥挤是症状。owner 说的"和成熟的 Web 版本差很多"，差的是**一次只能握住一个对象**。

`data-view.ts:872 dbOpenTable()` 与 `:916 dbFkOpen()` 的函数体几乎逐行相同，都是同一段"重置对象"的仪式：

```ts
d.table = …; d.schema = …;
d.offset = 0; d.order = null; d.dir = "asc"; d.filters = …;
d.tab = "data"; d.detail = null;
d.sqlResult = null; d.sqlResults = null; d.sqlTab = 0;
dbDropEdits();                       // updates / deletes / inserts 全丢
```

后果，按代价排序：

1. **换表即丢工作**：过滤、排序、分页、列宽焦点、以及**缓冲中的编辑**（`dbDropEdits` 清空 `updates` /
   `deletes` / `inserts`，用户要先答一个 confirm 才丢得掉）。
2. **FK 跳转把源表顶掉**。docs/22 W5.2 已经做好的跳转——这一页最有用的功能——代价是"你正在看的那张表没了"。
   回来要重新在 1,712 张表里找一遍、重新加过滤。
3. **SQL 控制台是个开关不是个地方**（`sqlOpen: boolean`）。展开时把网格往下顶，收起时查询上下文还在
   state 里但看不见；`dbOpenTable` 还会顺手把结果页签全清掉（`sqlResults = null`，代码注释自认："opening
   a table closes every result tab"）。
4. **浏览器帮不上忙**。Web 版数据库客户端普遍长着自己的页签，不是审美趋同而是约束使然：浏览器自己的
   tab 对数据库会话是废的——新开一个就是一块冷面板，没连接、没 lease、什么都要重新捞。桌面应用可以开
   第二个窗口，Web 面板不能，**页签是窗口在 Web 上的替代品**。

### 0.3 Redis 复用了表的骨架

`renderDbTables()`（`data-view.ts`）里 Redis 走的是同一副骨架：tables → keys、SQL → Command。键列表平铺
在一列里，一行两高（key + `type · ttl 120s`），450 个键靠 `dbRedisCompare` 排序后从头滚。Redis 键空间的
天然形状是 `:` 分段的树，不是一张平表。

### 0.4 便宜的事实（有利于本次重构）

- **状态只有一个出口**。`db-state.ts` 的 `dbView()` 是全域唯一读口，131 处调用分布在 14 个文件里
  （`data-view.ts` 37、`data-grid.ts` 25、`data-browsers.ts` 15、`data-sql.ts` 13、其余个位数）。
  换句话说切分状态**不需要满仓库找 `state.db`**，只需要把这 131 处按字段归类。
- **49 个字段本身就分成两类**（`db-state.ts` 的注释还写着 47，自 docs/37 起漂了两个）。按"属于连接"
  与"属于对象"划一刀，界线干净利落，见 §2.2。
- **渲染早就是构造 + 委派**。`renderDbView()` 一次建骨架，`#pane` 上每种事件挂**一个**属性式委派监听器
  （docs/37 R5），控件靠 id / `data-*` 寻址，没有 per-render 的 wiring。页签条加进去不会踩到监听器堆叠。
- **`dbOpenTable` 与 `dbFkOpen` 已经是同一个函数的两份拷贝**（§0.2）。它们正是"开一个页签"的诞生地：
  两处合流成 `dbOpenTab()`，重复消失，多页签同时到手。
- **结果页签已经存在**。docs/22 W4.3 的 `sqlResults[]` + `sqlTab` 就是一套小页签，只是作用域挂错了地方
  （挂在全局而不是挂在一个 SQL 对象上）。

## 1. 决定

### 1.1 为什么是 B

mockup 四个方案，逐个交代结论（owner 已排除 A）：

| | 方案 | 取 | 舍 |
| --- | --- | --- | --- |
| A | Two rows：把那一行拆两行 | — | **舍**。只治 §0.1，不碰 §0.2 的单对象模型；owner 已排除 |
| **B** | **Object tabs：打开的对象成为页签** | **取为骨架** | 唯一正面解决 §0.2 的方案；顺带把 §0.1 一并做掉（工具条被限定到当前页签后自然只剩该页签的控件）；§0.3 的键空间树也在它的侧栏改造里 |
| C | Quiet：底部状态条 + `⌘K` 发现性 | **取零件**：状态条（分页 / 耗时 / 可编辑性下沉）、单行侧栏行、monospace `WHERE` 栏 | **舍骨架**。`⌘K` 优先是桌面习语（TablePlus / Linear），赌用户每天用八小时；swiss 面板是偶尔打开的，藏起来的功能等于没有。且它不改变"能同时握住几个东西" |
| D | Docked workspace：上网格下 dock | **留作后续**（见 §9） | 是 B 的第二期不是替代品："一个页签的 body 分成上下两块"。顺序反了要付两遍钱（先在单对象页面上做 dock，等页签来了 dock 的作用域还得重做）。且它永久吃掉一块纵向空间，而 48px rail + 44px context bar 之后纵向本就最稀缺 |

mockup 覆盖的状态（实施时逐个对照，`docs/assets/42/data-layout-mockup.html` 的 State 下拉）：
SQL 各 18 态 —— browse / filters / edits / readonly / nopk / empty / error / form / structure / ddl /
sql / explain / activity / noconn / import / value / newtable / menu；Redis 12 态 —— keys / hash /
string / list / set / zset / stream / edits / command / empty / ttl / noconn。

### 1.2 层级归属：**不新增层**

这是复核时第一个会被问的问题，先答死。

swiss-ui-design §1 把 L3 写成 "Page-local navigation / **resource navigation**"，并且给 Data 的举例就是
`database / schema / table / object`。也就是说：

- **对象页签 = L3 的"资源导航"那一半。** 它不是新机制，它是把 Data 今天**隐式**的资源选择（侧栏单选）
  变成显式且可多持有的。
- **seg 药丸（Data / Form / Structure / DDL）= L3 的"页内分节"那一半**，作用域在**一个**资源之内。

这两半同页共存的先例仓库里已经有了：**MCP / Servers** 就是「侧栏选服务器（资源）+ seg 选
Tools/Resources/Prompts/Run/Config/Logs（分节）」。Data 的页签与 MCP 的侧栏只差一件事：**能同时持有多个
选择**。机制之所以不同（页签 vs 列表单选），是因为 Data 的资源又多又要交叉比对，而 §6 本就允许不同内容
类型选不同的 body 模板。

**结论：不动 L1 / L2 / L3 的定义，不改 skill 的 §1。** §7 要加的只有一句：Data 的资源导航用页签，因为它
要多持有。

### 1.3 页签条长什么样（三种形状不许互相串味）

面板里已经有两种"一排可切换的东西"，页签是第三种，必须一眼可分：

| 形状 | 归属 | 长相 | 在哪 |
| --- | --- | --- | --- |
| 下划线页签 | L2，context bar 拥有 | 无边框文字 + bar 底 hairline 上 2px accent | `MCP  Servers Traffic Token` |
| seg 药丸 | L3 分节，页体拥有 | 容器 + 选中块，`--r-pill` | 今天的 `[Data|Form|…]` |
| **卡片页签（新）** | **L3 资源，页体拥有** | **前导类型 glyph + 名字 + 尾随关闭 ×；整条坐在 `--sidebar` 面上，选中的那个抬到 `--bg` 并与下方网格连成一个面（无下边线）** | 本文新增 |

卡片页签**不许**长成下划线（那是 L2 的形），**不许**长成药丸（那是分节的形）。它自带 × 和 glyph，本来
就是另一件事：一个可开、可关、可重排的对象。选中页签与网格连成一个面这一条是 §16 规则 16（一个面一条边）
的直接应用——页签不是标签，它是这块面的顶边。

### 1.4 决定清单

| # | 决定 | 理由 |
| --- | --- | --- |
| D1 | 状态切成 `DbConnState`（连接域）+ `DbTab[]`（对象域），见 §2 | §0.4 的 131 处调用可按字段机械归类 |
| D2 | 页签三种 kind：`table` / `sql` / `key`，外加 `activity`；`sqlOpen` 与 `activity: boolean` **退休** | 控制台与活动监控本来就是"打开后离开再回来"的东西，即页签 |
| D3 | 页签上限 **8**；超出时淘汰**最久未访问且不脏**的那个；脏页签永不被淘汰 | "ruthlessly small"——每个页签握着最多一页 500 行 |
| D4 | 后台页签丢弃 `data`（行页），激活时按其 `offset` / `filters` 重新拉 | 同上；过滤与分页是**便宜**的状态，行是**贵**的 |
| D5 | 关闭脏页签要确认；`hasPendingChanges()` 问**所有**页签；`canLeave()` 一次性报总数 | 今天的守卫只看单条记录，多页签下会漏 |
| D6 | `dbOpenTable` 与 `dbFkOpen` 合流为 `dbOpenTab()`；**FK 跳转开新页签，源页签原样留着** | §0.2 第 2 条，docs/22 W5.2 的正解 |
| D7 | `DbState.tab` 改名 `pane`；`sqlTab` 改名 `resultTab` | "tab" 在本文后有了第二个含义，一个名字不许指两件事 |
| D8 | 侧栏成树：SQL 按 Tables / Views / Routines 分节（`ApiDbTableRow.type`）；Redis 按 `:` 分段折叠，单子节点做路径压缩 | §0.3；压缩是为了 `stream:orders` 不会造出一个只装一项的 `stream` 文件夹 |
| D9 | 侧栏行由两行改**一行**：名字 + 右对齐的次要信息（行数 / 类型），`--text-3`、tnum | C 的零件；1,712 行的列表两行高翻不动 |
| D10 | 工具条只画**当前页签**的控件；分页、耗时、可编辑性下沉到底部状态条 | §0.1.1 / §0.1.2；C 的零件 |
| D11 | 结构六页签折成四个：`Data / Form / Structure / DDL`，Columns + Indexes + Foreign Keys 并入 Structure 内部分节 | §0.1.3；`DB_TABS` 由 6 → 4 |
| D12 | 页签条与状态条**不进 focus 模式的隐藏表** | 它们是页体，不是壳 |
| D13 | 只恢复"活动页签"，不持久化整个工作区 | 深链与 `last-page.ts` 的契约不扩大 |

### 1.5 一个都不动

App Shell（rail L1 / context bar L2 / focus 模式）、`/api/db` 的 12 条路由与任何响应形状、连接租借
（`ConnectionCatalog` / `ConnectionLease`，Data 不拥有连接）、凭据纪律（`${...}` 信封）、`page-registry`、
`last-page.ts`、Terminal 的 dock、docs/22 已交付的 32 项功能语义（只搬位置，不改行为）。

## 2. 状态切分（本文的心脏）

### 2.1 今天

`crates/swiss-panel/panel/src/db-state.ts`（155 行）持有一个 **49 字段**的单记录，`dbView()` 是全域唯一
读口。`mountDbView()` / `unmountDbView()` 管生命周期，记录**永不为 null**（docs/37 R4 的设计，保留）。

### 2.2 切分表（逐字段，一个不漏）

**连接域 `DbConnState`（17 个）**——换页签时**不变**，换连接时重置：

| 字段 | 说明 |
| --- | --- |
| `conns` | `/api/db` 的连接列表（其实是 app 域，随记录一起放这儿） |
| `conn` | 当前连接名 |
| `tables` `tablesTotal` `tablesPage` `tablesLimit` `more` | 侧栏表列表的一页 |
| `grep` | 侧栏过滤词 |
| `schemaFilter` | pg 的 schema 过滤 |
| `sort` `sortDir` | 侧栏排序 |
| `gridCfg` | 列宽 / 隐藏列——**本来就是 per-connection**（字段自己的注释这么写） |
| `history` `favorites` | 控制台历史与收藏（localStorage 背书） |
| `redis` | `{ keys, cursor, done, total }`——Redis 侧栏的键列表，即 `tables` 的对位 |
| `redisType` | SCAN TYPE 过滤（侧栏） |
| `redisError` | 上次 SCAN 失败标记（侧栏） |

**对象域 `DbTab`（30 个）**——每个页签一份：

| kind | 字段 |
| --- | --- |
| 公共 | `loading`、`sel`、`selAnchor`、`focus`（`table` 与 `sql` 都有网格） |
| `table` | `table` `schema` `data` `filters` `pageSize` `offset` `order` `dir` `sqlPreview` `updates` `deletes` `inserts` `pane`(原 `tab`) `formIdx` `detail` `detailBusy` `conflict` |
| `sql` | `sqlText` `sqlResult` `sqlResults` `resultTab`(原 `sqlTab`) `sqlBusy` |
| `key` | `redisKey` `redisValue` `redisEdits` |
| `activity` | `activityRows` |

**退休（2 个）**：`sqlOpen`（控制台成为 kind）、`activity: boolean`（活动监控成为 kind）。

17 + 30 + 2 = 49。✓（划分**完备且不相交**，实施时可用同样的清单自检）

### 2.3 新的访问器

`db-state.ts` 对外换成三个读口，`dbView()` **删除**（不留兼容别名——留了就会有人继续往里写）：

```ts
/** The connection-scoped record: the sidebar's list, its filters, and what the console
 *  remembers. Shared by every open tab; reset when the connection changes. */
export function dbConn(): DbConnState;

/** The active tab's record. Never null: a fresh mount opens one empty `table` tab, so the
 *  view always has somewhere to paint. */
export function dbTab(): DbTab;

/** Every open tab, in strip order. The active one is `dbTabs()[dbActiveIndex()]`. */
export function dbTabs(): DbTab[];
export function dbActiveIndex(): number;
```

`DbTab` 是**判别联合**（`kind` 为判别式）。TypeScript 的收窄是这次切分的安全网：
`if (t.kind !== "table") return;` 之后 `t.filters` 才可见，写错 kind 在 `npm run check` 的 typecheck
里就红，不会拖到浏览器。**不许**用可选字段把四种 kind 压成一个大接口——那等于把类型检查关掉。

### 2.4 131 处调用怎么改

字段名在两域之间**没有重名**，所以归类是确定的：`d.grep` → `dbConn().grep`，`d.filters` →
`dbTab()` 收窄后的 `.filters`。逐模块过，不要全仓库正则替换（`data-browsers.ts` 里有同名局部变量）。

## 3. T1 — 状态切分，单页签（**不可见**重构）

**这一阶段结束时，页面看起来和今天一模一样。** 这是交付顺序里最重要的一条：把机械的大改动与可见的新
功能分开，出问题时能立刻知道是哪一类。

### 3.1 做什么

1. `types/state.d.ts` 加 `DbConnState`、`DbTab`（判别联合）、`DbTabKind`。
2. `db-state.ts` 改写：两个记录、四个访问器（§2.3）、`freshConnState()` / `freshTab(kind)`。
   `dbTabs()` 恒返回**长度为 1** 的数组，`dbActiveIndex()` 恒 `0`。没有开/关页签的入口。
3. 14 个文件的 131 处 `dbView()` 按 §2.2 归类改写。
4. `tab` → `pane`、`sqlTab` → `resultTab` 两处改名（D7），含 `data-structure.ts` 的 `DB_TABS` /
   `dbSetTab` / `dbRenderTabs` 与 `data-sql.ts` 的 `dbResultTabLabel`。
5. `sqlOpen` 与 `activity: boolean` 暂时**保留**在 `DbConnState` 上（T2 才退休）——本阶段不许有行为变化。

### 3.2 验收

- **既有 31 个 `admin-data-*.test.ts`（4,454 行）全绿，且只改访问器名与状态搭建**。任何一条用例需要
  改**断言**，都说明 T1 漏改了行为——回去查，不许改断言迁就实现。
- 新增 `test/db-state-split.test.ts`：
  - `dbConn()` 与 `dbTab()` 是两个对象，改后者不动前者；
  - `unmountDbView()` 后两者都回到 fresh 字面量，`dbIsMounted()` 为 false；
  - 三次 mount / unmount 循环后视图仍可工作（沿用 `admin-data-state.test.ts` 立的契约）；
  - `DbTab` 的四种 kind 各自 fresh 后只带自己那组字段。
- `npm run check` 全绿（typecheck ×2 + lint + emit 新鲜度 + vitest）。
- **19998 真浏览器走查**：按 `panel-proof-of-life` 清单跑一遍**今天的**全部流程（浏览、过滤、编辑提交、
  Form、Structure、DDL、SQL 控制台双结果、Activity、Redis 六型、导入、值查看器）。这一阶段的走查标准
  是"**和重构前没有任何区别**"。

## 4. T2 — 页签条

### 4.1 做什么

1. **开**：`dbOpenTab(spec)` 合流 `dbOpenTable` 与 `dbFkOpen`（D6）。同一个对象已开 → 激活它，不重复开。
2. **关**：页签上的 ×；脏页签先 confirm（D5）。关掉活动页签后激活右邻，没有右邻则左邻；关掉最后一个
   → 回到空态（`emptyNode`，§16 规则 11）。
3. **切**：点页签即激活；`Ctrl+Tab` / `Ctrl+Shift+Tab` 循环（§16 规则 19：高频手势不靠发现）。
4. **上限与淘汰**：D3 / D4。淘汰是纯函数 `dbEvictTarget(tabs, activeIndex)`，返回下标或 `null`。
5. **控制台成为页签**：侧栏底部（或页签条尾部）一个 `+ SQL`，开一个 `sql` kind 页签。`sqlOpen` 退休，
   `.db-console` 不再是 `hidden` 切换的块，而是 `sql` 页签的 body。
6. **Activity 成为页签**：`dbMore` 菜单里的入口改为开一个 `activity` 页签；`activity: boolean` 退休。
   5 秒轮询的启停跟着"该页签是否活动"走（`dbActivityPollStop` 已有）。
7. **守卫**：`views/data.ts` 的 `hasPendingChanges()` / `canLeave()` 改为遍历所有页签（D5）。
8. CSS：`.db-tabstrip` / `.db-tab` / `.db-tab.sel` / `.db-tab-close`，形状按 §1.3。只用既有 token，
   零颜色字面量。

### 4.2 验收

新增 `test/db-tabs.test.ts`（纯函数优先，DOM 其次）：

| 用例 | 断言 |
| --- | --- |
| 开同一张表两次 | `dbTabs().length === 1`，且它被激活 |
| 开第 9 张表 | 长度停在 8，被淘汰的是最久未访问**且不脏**的那个 |
| 8 个页签全脏，再开第 9 个 | 不淘汰任何一个；**拒绝开新页签并给出提示**（不许静默丢用户的编辑） |
| FK 跳转 | **新开**一个页签，源页签的 `filters` / `offset` / `updates` 原样还在 ← docs/22 W5.2 的回归 |
| 关脏页签 | 走 confirm；答否则页签还在 |
| 任一后台页签有缓冲 | `hasPendingChanges()` 为 true |
| `canLeave()` | 只问一次，数字是所有页签的总和 |
| 后台页签被激活 | `data` 为 null 时按其 `offset` / `filters` 重新拉（D4），过滤与分页**没丢** |

**19998 真浏览器走查**（`panel-proof-of-life` 第 4 条，真 CDP 点击，不是 `element.click()`）：
开三张表 + 一个 SQL 页签；在 A 表加过滤并改一格（不提交）→ 切到 B 表 → 切回 A：**过滤还在、脏格还在、
分页还在**。在 A 表点一个 FK → 新页签打开目标表，A 仍在条上。关掉脏的 A → 确认框出现。`Ctrl+Tab`
循环一圈。每个页签 × 的点击用真指针坐标命中。Redis 连接下开三个键页签同样走一遍。

## 5. T3 — 侧栏成树

### 5.1 做什么

1. **SQL**：按 `ApiDbTableRow.type` 分 `Tables` / `Views` / `Routines` 三节，用 docs/20 §4 的 band
   形状：**复用 `groups.ts` 的 `mountGroup(cfg, slice)`，`density: "side"`**（§16 规则 5——不要另造一套；`slice()` 负责把行分组，`mountGroup` 画 band + 折叠 + 记忆）。pg 的多 schema 分组保持在这之上。
2. **Redis**：`redisNamespaceTree(keys)` 纯函数——按 `:` 逐段分组，**单子节点路径压缩**（D8）。
   `user:1001:profile` + `user:1002:profile` → 一个 `user` 节点两个孩子；`stream:orders` 独一份 →
   直接是根上一行，不造 `stream` 文件夹。
3. **单行行**（D9）：名字 + 右对齐次要信息，`--text-3`、tnum、`--f-caption`。`.db-table-meta` 由块改
   行内右对齐。完整信息进 `title`（今天 Redis 行已经这么做）。

### 5.2 验收

新增 `test/db-tree.test.ts`，全是纯函数：

- `redisNamespaceTree([])` → `[]`；
- 两个同前缀键 → 一个节点两孩子；
- 无 `:` 的键坐在根上；
- 单子节点链压缩成一行（`a:b:c` 独一份 → 一行 `a:b:c`，不是三层空文件夹）；
- 1,000 个键的分组是稳定序（同 `dbRedisCompare` 的口径）；
- SQL 分节：`type` 缺失的行归 `Tables`（服务端不保证给，`ApiDbTableRow.type` 是可选的）。

**19998 走查**：MySQL（1,712 张表）展开/折叠三节、grep 之后树仍成立；pg 的 schema × 类型双层不塌；
Redis 命名空间展开到叶子并开成页签。1440 与 900 两个宽度，明暗两套。

## 6. T4 — 工具条收敛 + 状态条

### 6.1 做什么

1. **工具条只画当前页签的控件**（D10）。`table` 页签：标题 + `pane` 的 seg + 一个主动作 + `⋯`。
   `sql` 页签：`Run` + `⋯`（Explain / Format / History / 收藏进 `⋯`）。`key` 页签：类型相称的
   `+ Field / + Item / + Member` + `⋯`。`activity` 页签：`Refresh` + `⋯`。
2. **状态条**（C 的零件）：页体底部一条，承载 `1–50 of 550,968`、翻页、耗时、可编辑性/只读原因
   （`ApiDbDataPage.editNote`）。今天的 `.db-bar`（提交条）保留其职责不变，状态条是另一条。
3. **六折四**（D11）：`DB_TABS` → `Data / Form / Structure / DDL`；Structure 内部再分
   Columns / Indexes / Foreign Keys（`data-structure.ts` 已有三张表的渲染，只是换个容器）。
4. `Refresh` / `CSV` / `Export…` / `Import…` 进 `⋯`。

### 6.2 验收

新增 `test/db-toolbar.test.ts`：

- **`table` / `sql` / `key` / `activity` 四种页签下，工具条里的非图标 `.btn` 数量 ≤ 1**
  （§16 规则 4 的机器门禁——这一条比任何截图都硬）；
- `sql` 页签的工具条里没有分页器、没有 `+ Row`；
- `DB_TABS.length === 4`；
- 状态条文本在 `editable: false` 时带上 `editNote` 的原文；
- Redis `key` 页签的主动作随类型变（hash → `+ Field`，list → `+ Item`，zset → `+ Member`；
  string 无主动作）。

**19998 走查**：四种页签各自的工具条逐个按钮真点；`⋯` 展开后 Export / Import 仍可达并可完成一次
真导出；只读连接下状态条说出原因；**第二遍走中文**（`文/A`，`document.documentElement.lang === "zh-CN"`，
docs/38），新文案全部有 `en.ts` / `zh.ts` 两份（`npm run check` 的字典完整性门禁会抓漏）。

## 7. T5 — 文档

1. `swiss-ui-design` skill §7 加一句（§1.2 的结论：Data 的资源导航用页签，因为它要多持有）；
   §16 若需新增"卡片页签"的形状，写进组件词汇表。
2. `.agents/docs/data.md` 的面板模块地图按新结构重写（9 个模块的职责有移动）。
3. `docs/21` 的差距表：被本文关掉的条目标注 `→ docs/42`。
4. README 文档表加一行（§8）。
5. 本文状态头改"已实施"，附最终数字（`npm run check` 的文件/用例数、`base.css` + `views.css` 字节、
   自有发射 JS 字节、`swiss.exe` 字节）与四张截图进 `docs/assets/42/`。
6. **ADR-026** 落进 `docs/07-decisions.md`（草案见 §10）。

## 8. 门禁与交付顺序

### 8.1 门禁（每个提交前，全绿才提交）

```bash
# 面板 —— 在 crates/swiss-panel/panel 下，自己一条命令（PowerShell 里 cd 混进链式命令会打断后面每个相对路径）
npm run check          # typecheck ×2 + eslint + emit 新鲜度 + vitest

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

`npm run check` 顺带跑 docs/38 的两道门禁：字典完整性（每个 locale 都要有 key）与 bare-literal 零门槛
（可见文案不许硬编码）。本文新增的每一句 UI 文案都要 `en.ts` + `zh.ts` 两份。

### 8.2 交付顺序：一阶段一提交，不许跳

| 阶段 | 内容 | 关键风险 |
| --- | --- | --- |
| **T1** | §3 状态切分，单页签，**零可见变化** | 既有 31 个测试文件必须**只改访问器名**就全绿。要改断言 = 漏改了行为，回去查。`data-browsers.ts` 有同名局部变量，别全仓库正则替换 |
| **T2** | §4 页签条 | 淘汰策略先写成纯函数再接 UI；`#pane` 的委派监听器是**属性式**赋值（`pane.onclick = …`），重画不许改成 `addEventListener`（会堆叠）；8 个全脏时是**拒绝开新的**，不是静默丢 |
| **T3** | §5 侧栏成树 | 复用 `groups.ts` 的 `mountGroup(cfg, slice)`（`density: "side"`），不要另造；折叠状态走它自带的 `loadCollapsed` / `saveCollapsed`；`ApiDbTableRow.type` 是可选字段，缺失归 `Tables`；happy-dom 无布局，别依赖 `offsetWidth` |
| **T4** | §6 工具条 + 状态条 | 规则 4 的 `.btn` 计数用例是硬门禁；状态条与既有 `.db-bar`（提交条）是**两条**，职责不许混 |
| **T5** | §7 文档 + ADR-026 | 旧的"已实施"描述一条不留 |

每阶段的固定动作：改 `panel/src` → 用例 → `npm run check` → `npm run build` → **`touch crates/swiss-panel/src/lib.rs`**
→ release 构建 → 重启 19998 → 真浏览器走查（该阶段的清单，1440 与 900 两宽，明暗两套）→ 提交。

**rust_embed 陷阱**：资产内容不进指纹，改了 `admin_assets` **不会**触发 swiss-panel 重编译——release 构建
会说 "Finished" 而二进制里仍是旧面板。每次重建 19998 前先 `touch crates/swiss-panel/src/lib.rs`，然后
**验服务出来的字节**，不是只看构建状态。

### 8.3 实例纪律

19998 是隔离测试实例（`scripts/test-instance.ps1 -Stop` / `-Fresh`，`CARGO_TARGET_DIR=target-test`）。
**19999 是 owner 的，一次都不许碰。** 19998 若被别的会话占着，换 19997（同脚本、独立测试 home，docs/39 的先例）。

## 9. 不在范围内

| 不做 | 为什么 |
| --- | --- |
| **D 的 dock**（上网格下 dock，SQL / Value / Row / Messages / Activity） | 是本文的**第二期**：一个页签的 body 再分上下。等 T1–T4 落地并用过一段时间再判断还缺不缺。先做会付两遍钱（§1.1） |
| C 的 `⌘K` 命令面板 | 桌面习语，发现性对偶尔打开的面板是净损失（§1.1） |
| 页签拖拽重排、页签分屏 | 上限 8 个的条上，重排的收益小于机械成本 |
| 工作区持久化（重开面板恢复全部页签） | D13：只恢复活动页签，不扩大 `last-page.ts` 的契约 |
| 虚拟滚动、可视化建表器、ERD / schema diff、完整元数据缓存、子进程连接 | docs/21 #31–#35 的"不做"清单，本文不翻案 |
| 任何 `/api/db` 路由或响应形状的改动 | 本文是纯前端的信息架构重构。若发现确实需要新端点，**停下来报告**，不要顺手加 |

## 10. ADR-026 草案（T5 落进 `docs/07-decisions.md`）

**题**：Data 的资源导航是对象页签，而页签是 L3 的一半，不是新的一层。

**语境**：Data 今天一次只能握住一个对象，换表即丢过滤、分页与缓冲编辑；FK 跳转把源表顶掉。成熟的 Web
数据库客户端普遍长着自己的页签，因为浏览器自己的 tab 对数据库会话无用（新 tab = 冷面板，无连接无
lease）——页签是窗口在 Web 上的替代品。

**选项**：

| 选项 | 取 | 舍 |
| --- | --- | --- |
| 保持单对象，只拆工具条（mockup A） | 改动最小 | 不解决丢工作；owner 已排除 |
| **对象页签（B）** | 正面解决；顺带收敛工具条；Redis 键空间树随之 | `DbState` 49 字段要切分，131 处调用要归类；内存要设上限 |
| 底部 dock（D） | 同屏可见网格 + 控制台 | 仍只握一个对象；永久吃纵向空间；顺序上应在 B 之后 |
| 命令面板 + 极简（C） | 密度最高 | 桌面习语，发现性差；不改变能握住几个 |

**决定**：B。页签是 L3 "resource navigation" 的显式化，**不新增层级**——先例是 MCP / Servers 的
「侧栏选资源 + seg 选分节」，Data 与它只差"能多持有"。页签用第三种形状（卡片 + glyph + 关闭 ×），
与 L2 的下划线页签、L3 分节的药丸一眼可分。

**代价，说清楚**：
- 每个页签握着最多一页 500 行 → 上限 8、后台丢 `data` 激活重拉、脏页签永不淘汰（D3 / D4）。这是对
  "ruthlessly small" 的直接让步，用上限把它框死。
- 导航守卫从"问一条记录"变成"问所有页签"（D5），漏一处就会静默丢用户的编辑。
- 8 个全脏时**拒绝开新页签**——宁可挡住用户，不可静默丢数据。

**如何回退**：T1 是不可见重构，T2–T4 是其上的功能。真要退，退 T2–T4 三个提交，T1 的切分留着即可
（单页签数组等价于今天的单记录）。
