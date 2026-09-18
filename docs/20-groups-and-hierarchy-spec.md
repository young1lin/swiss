# 20 — 分组与层级：一个模型、一族 API、一个组件

> 状态：**已实施（G1–G8，`72dc010` 起，完成于本提交）**。基线 `109397e`（2026-09-13）。配套交接提示词：[20-groups-and-hierarchy-prompt.md](20-groups-and-hierarchy-prompt.md)。
> **§4 视觉模型修订（tree 模型，2026-09）：分组头不再是 `--sep-soft` 色带，而是树节点——
> chevron、folder 图标（新 sprite `i-folder`）、名称、计数，透明底、hover 才有轻背景；grip 与 ⋯
> hover/focus 才显现，且 grip 移到头部末尾以稳定树列的 x 坐标。缩进按「最终排版的文字 x 坐标」计量：
> side 密度子行文本比父名右移 26px（body 缩进 42px），page 密度右移 28px（body 缩进 54px）；
> 竖向 guide 改挂在 `.grp::before`，从 chevron 列降到最后一行的中线，折叠即消失。
> 空组文案缩短为 "No items — drop here or press +"（不可拖的 scope 只说 "No items"）。
> 组件、API、localStorage 键、拖拽语义全部不变；`aria-expanded` 补上。**
> **§4 第二次修订（docs/35，2026-09-18）：tree 模型只保留在侧栏密度；页面密度改为「组即卡片」——
> `.grp` 自身就是 `.group` 卡，36px `--sep-soft` 色带作组头（chevron、名称、计数，不画 folder），
> 行直接铺在色带下、通栏。组头整条可拖（没有 grip，`i-grip` 已删），`+`/`⋯` 在 dragstart 取消拖拽以
> 保住点击；组的 before/after 落点是整块 `.grp`（头和成员都算），行的 drop-into 落点是组头或空行。
> 本文 §4.1 的图与「抓手」条目以 docs/35 为准。**
> 设计语言的总纲从本文起由 `.claude/skills/swiss-design/SKILL.md` 承载：本文 §4 是那份语言在「分组列表」
> 上的展开，两者冲突时以 skill 为准（skill 改了要回来改这里）。
> 前置阅读：`AGENTS.md`（规则高于本文）、`docs/18-panel-visual-refresh-spec.md`（本文沿用它的 token、
> 图标 sprite、空态模板与「一个主操作 + ⋯」规则）、`docs/13-panel-navigation-spec.md`（页面分组是另一
> 回事——按 pluginId 分一级导航——本文不碰）、`docs/05-wire-compatibility.md`（§2.3 动了三个状态文件）。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-09-13）："我这个应用页面很奇怪，为了实现可拖动，这个分组和分组内的东西，设计
> 很烂。还有，其他的地方也有分组，也有类似之类的东西的 … 把这个页面交互优化下，所有要新增内容的地方，
> 都应该有分组，用来区分，这是哪个分组的内容。并且需要能展示出明显的层级关系。我建议你重新考虑设计下
> 整个项目，给出一个统一的风格设计 spec"
> 第一轮确认（同日）：(1) 分组覆盖面——"是，都要，能有分组语义的，都要"；(2) 存储模型——"我建议一套
> 代码维护"；(3) 新增规则——"按你的建议来"；(4) 拖拽——"组也可以拖，但是层级和内部的内容区别开来"；
> (6) Node 侧——"什么 node 不 node，我这个项目做就行了"；(7) 交付切分——接受。(5) 层级画法未答，按本文
> 建议执行。追加（同日）："专门写个 swiss-design skill 专门用来写所有的设计风格，整体设计风格内容"。

## 0. 现状与缺口（为什么是它、为什么是现在）

> 本节（含其行号引用，如 `src/adminapi.rs:481-`、`js/data-view.js:309-331`）描述的是**实施前**
> 的代码——它们是 2026-09-13 截图核对时的证据坐标，实施后各文件行号已漂移，按函数名找，不按行号找。

2026-09-13 在 19999 上逐页截图核对，以及读了三处服务端存储之后，问题可以说得很具体：

### 0.1 三套分组模型，各自一份代码

| 列表 | 有分组？ | 存储 | 服务端 | 面板 |
| --- | --- | --- | --- | --- |
| MCP 侧栏 | 有 | `managed.json` `groups`/`mcpGroups`/`order`；`default` 是**普通组、存在列表里**，第一个槽位是兜底 | `crates/swiss-host/src/managed.rs:138-168,444-567`；路由 `src/adminapi.rs:612-744`（`PUT /api/order`、`PUT /api/groups`、`POST /api/groups/{name}/rename`、`PUT /api/mcps/{name}/group`） | `js/sidebar.js`（树头 + 抓手 + 行拖拽）、`js/menu.js`（`wireDrag`/`patchSidebar`） |
| Tunnels 连接 / 转发 | 有，两套 | `tunnels.json` `connGroups`/`ruleGroups`；`default` **隐式、保留字、不存**（`store.rs:18`） | `crates/swiss-tunnels/src/tunnel/store.rs:83-101,565-`；路由 `tunnel/api.rs:409-470`（`/api/tunnels/groups/{kind}`、`…/rename` 的 body 是 `{from,to}`，而 MCP 的是 `{name}`） | `js/tunnels.js:10-160`（第二份拖拽实现）、`js/polling.js:74-88`（第二份 `grouped()`）、`js/traffic.js` `tunGroupHeadHtml`（第二种组头） |
| Jobs | **没有** | 定义在 `jobs` 插件配置行里（docs/11），`labels: string[]` 只做过滤 | — | `js/jobs.js:51-59` 单个 `.group` 卡 |
| Secrets | 有（2026-09-15 修订：docs/26，用户要求行可拖） | `secrets.json` `{rev, secrets, groups, secretGroups, order}` | `src/adminapi.rs:481-` | `js/views/secrets.js` 内联表单 + 分组列表 |
| Tokens | 没有 | `managed.json` `tokens:[{id,label,secret,createdAt}]` | `src/adminapi.rs:398-` | `js/views/tokens.js` |
| Data 连接下拉 | 没有 | — | `/api/db` 已按侧栏**视觉顺序**排（`src/app.rs:546-580`） | `js/data-view.js:309-331` 一个扁平 `<select>` |

同一件事——"一份有序的组名列表 + 每项一个组名 + 第一组兜底"——写了两遍服务端、两遍面板，且两遍在
细节上已经分叉（`default` 是否可改名、rename 的 body、删除组时确认文案里的兜底名）。第三、四、五处
再各写一遍是不可能的；这是用户"一套代码维护"的字面意思。

### 0.2 侧栏的层级读不出来（对应用户截图）

- 组头是 11px 全大写 caption、`--text-3` 灰（`base.css:350-355`），子行是 13px `--text` 500 字重、30px
  高（`base.css:305-316`）：**组头比子行还轻**。缩进只有 12px（`base.css:376`），没有引导线。用户看到
  的是"一列 MCP 中间夹了几行小字"，不是"三个文件夹"。
- 组头同时承担拖动（抓手，hover 才现）、折叠、`+`、`⋯` 四件事。`base.css:365-367` 自己写的规则是
  "one persistent glyph per group is a hierarchy, two would be a toolbar"，然后放了三个。
- 两个一样的 `+`：搜索框旁的 `#addBtn` 是 **New group**（`index.html:81`、`add-sheet.js:125`），组头的
  `+` 是 **Add an MCP**。同一字形、两个意思、相距 30px。
- 空组（截图里的 `FORTEST 0`）只剩一行 caption，没有任何"这是个空容器"的暗示；折叠态与空态长得一样。
- Tunnels 页的组头是 `.sec-head` caption 加白卡（`views.css:308-322`），和侧栏树头不是一个东西，只是行为
  相似——同一个用户在两个 tab 之间切换时要学两遍。

### 0.3 "新增"不说去哪

| 入口 | 现状 |
| --- | --- |
| MCP 组头 `+` | 弹层标题 "Add an MCP to ForTest" ✅（`add-sheet.js:9-11`） |
| MCP 空态 "Add an MCP" | `openSheet(null)` → 静默进第一组 ❌（`pane.js:57`） |
| Tunnels 顶部蓝色 **New** | `pendingGroup = null` → 静默进 default ❌；弹层标题只写 "New SSH connection"（`tunnels.js:266-267`） |
| Tunnels 组头 `+` | ✅ 但弹层标题仍不写组名 |
| Jobs **New** / **New (advanced)** | 无组可言 |
| Secrets **Store** / Tokens 新建 | 无组可言 |

用户的规则很清楚：**每个新增入口都必须让人在提交前看见"它会进哪个组"**。

## 1. 目标与非目标

**目标**

- G-模型：一个 Rust 模块定义"分组"的全部语义，六个作用域（MCP、隧道连接、隧道转发、Jobs、Secrets、
  Tokens）都用它；`default` 在每个作用域里都是普通组。
- G-API：一族 `/api/groups/{scope}/…` 路由，宿主挂一次，按作用域分发；旧的四条 MCP 路由和三条 Tunnels
  路由退役（面板是唯一调用方，同一提交切换）。
- G-组件：面板一个 `groups.js` 模块渲染所有分组列表，两种密度（侧栏树 / 页面卡），同一套组头解剖、
  拖放、键盘、菜单、空态。
- G-层级：组头与子项**一眼可分**——组头是一条带底色的 28px 标题带，子项缩进 20px 并有引导线。
- G-新增：每个创建弹层 / 内联表单都有 **Group** 字段；组头 `+` 打开时标题写明 "… in *group*"；侧栏
  的双 `+` 消歧。
- G-Data：Data 连接下拉按 MCP 组分 `<optgroup>`。

**非目标**

- 不动一级导航（docs/13 的按 pluginId 分组是另一回事）。
- 不做嵌套组（组里再套组）。一层足够；嵌套会把"第一组兜底"变成树上的路径问题。
- 不做跨作用域的组（一个组同时装 MCP 和 job）。作用域之间的名字可以重复，互不相干。
- 不做批量多选拖拽、不做组内排序以外的排序（按名字 / 按状态排序是过滤器的事，不是分组的事）。
- 不动 Node 仓库的服务端（用户决定：只做本项目）。面板文件仍经 `../local-mcp-gateway/src/admin/`
  编辑后整目录复制——那只是为了让 `the_tree_is_byte_for_byte_the_node_builds` 继续成立，
  Node 的 `src/*.ts` 一行不改。Node 服务端的同款实现记入 §7 待办，与 docs/19 同款处理。
- Jobs 的 `labels` 不动、不迁移成组。label 是多对多的标签（过滤），group 是单亲的分区（有序、可改名、
  可删、有兜底）——两种语义，两个字段。

## 2. 一个模型：`swiss_host::groups`

### 2.1 语义（六个作用域共享，一处实现，一处测试）

```rust
// crates/swiss-host/src/groups.rs — pure data, no I/O; every scope's store embeds one of these.
pub struct Groups {
    /// Display order. Never empty: the FIRST entry is the sink for unassigned members and for
    /// the members of a deleted group — the slot, never the name `default`, carries that role.
    names: Vec<String>,
    /// Sparse: an absent id renders in the first group. Stores the canonical casing of the name.
    members: BTreeMap<String, String>,
}
```

不变量（每条一个单元测试，全部在 `groups.rs` 里，六个作用域不再各测一遍）：

1. `names` 永不为空；`set_names([])` → `Err("at least one group must remain")`。
2. 组名 trim 后非空、≤ 64 字符、大小写不敏感唯一（"Docs" 与 "docs" 是同一个组）。
3. `group_of(id)`：成员表里有且该组仍存在 → 该组；否则 → `names[0]`。
4. `set_names(next)`：整表替换；被省略的组被删除，其成员从 `members` 移除（自然落入 `names[0]`）；
   `default` 和任何名字一样可以被省略。
5. `rename(from, to)`：原地改名，保住槽位，成员跟着走；`to` 撞已有组（不区分大小写）→ `Err`。
6. `assign(id, Some(g))`：`g` 必须存在（不区分大小写匹配，存规范大小写）；`assign(id, None)` = 删除
   显式项（渲染在第一组）。
7. `order`（排序）不属于 `Groups`：每个作用域已各有一份平铺顺序（MCP `order`、tunnels 数组顺序、
   jobs 数组顺序、tokens 按创建时间；secrets 于 docs/26 起为 vault 的 `order` 数组——用户要求行可拖），`Groups` 只按组切片。这与 sidebar.js 注释里
   "One flat order sliced by group is why moving an MCP between groups never has to rewrite the
   ordering" 的理由一致。
8. 序列化形状固定为 `{ "groups": [names…], "<scope>Groups": { id: name } }`，键名由嵌入它的
   store 决定（§2.3）。
9. `set_names_pinning(next, ids)`（换序路径专用）：整表替换前，若原第一组**存活但被降位**，
   只靠兜底槽位渲染（无显式项）的成员先被钉到该组名上——换序只动顺序，不动任何成员的渲染组。
   钉的是**新列表的拼写**：一次整表替换可以同时降位并改拼写，而 retain 与各 loader 的成员
   过滤都是精确匹配，旧拼写的钉会被同一次调用丢掉。
   第一组仍居首 → 无事可钉；第一组被省略删除 → 按第 4 条下沉（那是删除契约，不是换序）。
   各作用域的成员 id 来源：mcps 由 registry 提供（config 源 MCP 无 managed 条目，store 自己数
   不全）；tokens/jobs 由各自 store 内部枚举；tunnels 成员在行字段上，钉的是 `row.group`；
   secrets 的模型本来就按渲染组稠密物化，天然免疫。2026-10 修复：此前对 g2 点"上移"会把 g1
   下从未显式指派的成员全部吸进 g2——槽位换了主人，兜底成员跟着槽位走。

### 2.2 作用域注册

```rust
// crates/swiss-host/src/groups.rs
pub trait GroupScope: Send + Sync {
    fn names(&self) -> Vec<String>;
    fn set_names(&self, next: Vec<String>) -> Result<Vec<String>, String>;
    fn rename(&self, from: &str, to: &str) -> Result<(Vec<String>, usize), String>; // (names, moved)
    fn assign(&self, id: &str, group: Option<&str>) -> Result<String, String>;      // canonical name
    fn set_order(&self, ids: Vec<String>) -> Result<Vec<String>, String>;
    fn has_member(&self, id: &str) -> bool; // 404 for an unknown MCP/job/token, not a silent write
}
pub struct GroupScopes(RwLock<HashMap<&'static str, Arc<dyn GroupScope>>>);
```

`GroupScopes` 挂在 `AppContext` 上；`src/subsystems.rs` 组装时把六个作用域注册进去（MCP 与 tokens 由
`ManagedStore` 实现，conns/rules 由 `TunnelStore` 实现两次，jobs 由 `JobSystem` 实现，secrets 由
`secretstore` 的一个薄包装实现）。宿主没有 match arm——路由按 `scope` 字符串查表。作用域名固定：
`mcps` `conns` `rules` `jobs` `secrets` `tokens`。这是 docs/09 "宿主只留跨切面机制"的正用：分组是机制，
每个作用域的成员是业务。

### 2.3 落盘（docs/05 §状态文件 追加一段）

| 作用域 | 文件 | 变化 | 迁移 |
| --- | --- | --- | --- |
| mcps | `managed.json` | 无（`groups` + `mcpGroups` + `groupsV2` 已经是这个形状） | 无 |
| conns / rules | `tunnels.json` | `connGroups`/`ruleGroups` **从此包含 `default`**；行上 `group: null` 仍合法（= 第一组） | 加载时列表里没有 `default`（不区分大小写）→ 前插；写回带 `tunnelGroupsV2: true`（与 managed.json 的 `groupsV2` 同一手法：标记之后列表按字面读，删掉的 `default` 不再复活） |
| jobs | `jobs` 插件配置行（`gateway.config.json` `plugins.jobs.config`） | 行增 `groups: [names]`，每个定义增 `group?: string` | 缺 `groups` → `["default"]`；缺 `group` → 第一组。**校验器**（`validate_config`）跑 `Groups` 的不变量，坏值 400 不落盘 |
| secrets | `secrets.json` | 增 `groups: [names]`、`secretGroups: {name: group}`；`secrets` 映射不变 | 缺 → `["default"]` / `{}`；值仍只进不出，组名走 GET 列表 |
| tokens | `managed.json` | 增 `tokenGroups: [names]`、`tokenMembers: {id: group}` | 缺 → `["default"]` / `{}` |

全部是**追加字段**：旧文件照读，Node 封印的夹具（`scripts/seal-fixture.mts` 的产物）不需要重生成——
docs/05 的测试断言"Rust 能打开 Node 封的文件"仍成立。反方向（Node 读 Rust 写的 `tunnels.json` 看到
`connGroups` 里有 `default`）不在 scope（§1 非目标）。

## 3. 一族 API

宿主在 `src/adminapi.rs` 挂一次（与 `/api/tokens` 同属宿主机制），body/响应形状六个作用域完全一致：

| 方法 | 路径 | body | 200 | 错误 |
| --- | --- | --- | --- | --- |
| PUT | `/api/groups/{scope}` | `{ groups: [names] }` | `{ groups }` | 400 不变量；404 未知 scope |
| POST | `/api/groups/{scope}/rename` | `{ from, to }` | `{ groups, moved }` | 404 未知组；400 撞名/空名 |
| PUT | `/api/groups/{scope}/members/{id}` | `{ group: name \| null }` | `{ group }`（规范大小写） | 404 未知成员；400 未知组 |
| PUT | `/api/groups/{scope}/order` | `{ order: [ids] }` | `{ order }` | 400 非字符串数组 |

**读**不加路由：每个作用域现有的 GET 列表已经/将要带 `groups`（顶层）和 `group`（每行）——
`/api/mcps`、`/api/tunnels`（`connGroups`/`ruleGroups` 改名为 `groups.conns`/`groups.rules`？**不改**，
沿用旧键，面板适配器映射；改键名是没有收益的破坏）、`/api/jobs`、`/api/secrets`、`/api/tokens` 各加
`groups` 顶层键与每行 `group`。一次轮询拿全，和现在一样。

**退役**（同一提交，面板同步切换）：`PUT /api/order`、`PUT /api/groups`（无 scope）、
`POST /api/groups/{name}/rename`（无 scope；与新路由同模式，必须让位）、`PUT /api/mcps/{name}/group`、
`PUT /api/tunnels/groups/{kind}`、`POST /api/tunnels/groups/{kind}/rename`、
`PUT /api/tunnels/groups/{kind}/{id}`、`PUT /api/tunnels/order`。仓库内除面板与其测试外没有调用方
（2026-09-13 grep：`.agents/docs/*.md` 里的引用是审计文档，实施后按 ask-swiss 的流程刷新）。

## 4. 一个组件：`groups.js`

### 4.1 解剖（两种密度共用）

```
┌ .grp ────────────────────────────────────────────────────────┐
│ .grp-head  [›] Learn                          3   [+]  [⋯]   │  ← 28px 侧栏 / 36px 页面；底色 --sep-soft；圆角 --r-row
│ .grp-body ┃ ● colab                                   uvx    │  ← 缩进 20px；┃ 是 1px --sep 引导线，从 head 底到最后一行
│           ┃ ● notebook                                 http   │
│           ┃ Empty — drop rows here or press +                │  ← 空组：一行 --text-3，28px，不可选
└──────────────────────────────────────────────────────────────┘
```

组头（`.grp-head`）：
- **名字**：`--f-body` 13px、600、**混合大小写**、`--text`。不再是 11px 全大写 caption——caption 是给
  "分区标题"（Scheduled commands、Clients）用的，组是用户自己起名的容器，得像个名字。
- **计数**：`--text-3`、tnum、跟在名字后 8px；折叠时保留（这是折叠态与空态的区别：`0` 对 `3`）。
- **chevron**：12px，展开时旋 90°；整条头（除按钮）是折叠热区。
- **`+`**：常显 .55 透明度（"新增进这个组"是高频动作）；**`⋯`**：hover/focus 才现。两者与 docs/18 一致。
- **抓手**：hover 才现（.7），在 chevron 左侧 14px；**仍是组头上唯一可拖的东西**（`sidebar.js:110-118` 的
  理由成立：按钮不能住在可拖元素里）。用户要的是"组可以拖"，不是"抓手常显"——层级由底色与引导线表达，
  不再依赖抓手。
- **底色**：`--sep-soft` 一整条，hover 叠 `--hover`。这是和子项拉开的主要手段：子项永远在透明底上。

子项（`.grp-body > *`）：
- 侧栏密度：`.side-row` 30px（不变）；页面密度：`.group` 白卡里的 `.row`（不变）。
- 缩进 **20px**（原 12px），左侧引导线 1px `--sep`，从组头底边到最后一行中线；折叠时线消失。
- 拖放高亮（不变）：行 before/after 2px accent 边；组头 drop-into 1.5px accent 环；组头 before/after
  是组排序。

### 4.2 行为（一处实现）

| 行为 | 规则 |
| --- | --- |
| 折叠 | 按作用域记 localStorage（`swiss.groups.<scope>.collapsed`），改名时迁移键 |
| 行拖拽 | 整行可拖；落在行上 = 排序 + 改组一次完成；落在组头 = 追加到该组末尾（空组唯一入口） |
| 组拖拽 | 抓手拖；落在别的组头上下半 = before/after；`⋯` 菜单 Move up / Move down 是精确路径，边缘不出现 |
| 键盘 | ↑↓ 走可见行；←→ 在组头上折叠/展开；Alt+↑↓ 组内挪动（不越组，`sidebar.js:93-107` 的理由）；Delete 不做任何事 |
| `⋯` 菜单 | Move up · Move down ─ Rename… ─ Delete group（红）。删除确认文案统一："Delete group 'X'? Its N item(s) move to 'Y'. Nothing is removed." 其中 Y 是**删除后的第一组**（tunnels.js:94-95 写死 `default` 是错的） |
| 搜索 | 过滤行；无命中的组隐藏；有命中的组强制展开（不改存储的折叠态） |
| 单组时 | 组头照画（用户要看见"这是哪个组"），但 `⋯` 里没有 Move |

### 4.3 新增规则（用户第 3 问，"按你的建议来"）

1. **组头 `+`** 打开该作用域的创建弹层，标题 "New job in *learn*" / "Add an MCP to *learn*"（MCP 沿用现
   有措辞），Group 字段预填且**可改**（不隐藏——"我以为我点的是那个组"是常见误操作）。
2. **页面级 New / 空态按钮** 打开同一个弹层，Group 字段默认 = 该作用域上次使用的组（localStorage
   `swiss.groups.<scope>.last`），没有则第一组。任何路径都不再静默落组。
3. **Group 字段**是一个 `<select>`：现有组按存储顺序 + 分隔 + "New group…"（选中即弹一字段的组名
   sheet，成功后回填并选中——`pane.js:190-197` 已有这个手法）。
4. **内联表单**（Secrets 的 name/value/Store、Tokens 的 label/Create）：Group select 放在主按钮左侧，同一
   行；规则 2 同样适用。
5. 侧栏搜索框旁的 `#addBtn`：图标改 `folder-plus`（sprite 新增一枚），tooltip/aria "New group" 不变。
   组头的 `+` 保持 `plus`。**同一页面不允许两个 `plus` 表示两种动作**——这条写进 swiss-design skill。

### 4.4 Data 下拉

`/api/db` 每行增 `group`（`src/app.rs` 的 `visual_order` 已经算出组切片，只是没带出来）；`renderDbSide`
按组画 `<optgroup label="learn">`。单组时不画 optgroup（一个 optgroup 只是噪音）。

## 5. 分项与验收（每项一个提交，测试先行）

### G1 — `swiss_host::groups` + 作用域注册 + 路由族（Rust，只加不减）

- 新文件 `crates/swiss-host/src/groups.rs`：`Groups`、`GroupScope`、`GroupScopes`。
- `ManagedStore` 用 `Groups` 替换自己的 `groups`/`mcp_groups` 实现（行为不变，`groupsV2` 语义不变）。
- `src/adminapi.rs` 挂 §3 四条路由；`src/subsystems.rs` 注册 `mcps` 作用域。旧路由**本项保留**（G2 退役）。

测试：
- `groups.rs` 单元测试覆盖 §2.1 八条不变量（每条一个 `#[test]`）。
- `managed.rs` 现有分组测试全绿不改（证明替换是等价的）。
- `adminapi` 集成测试（`tower::ServiceExt::oneshot`）：四条路由在 `mcps` 上的 200/400/404；
  `/api/groups/nope` → 404 `unknown scope: nope`。

### G2 — 面板 `groups.js`：侧栏 + Tunnels 切到一个组件；旧路由退役

- 新 `js/groups.js`：`mountGroups({ scope, host, density, rows, rowNode, onOpen, api })`；`sidebar.js`
  与 `tunnels.js` 的分组/拖拽/菜单代码删除，只剩各自的行渲染与打开详情。`polling.js` 的
  `tunGrouped` 与 `sidebar.js` 的 `groupedMcps` 合并为 `groups.js` 的一个 `slice(rows, names, groupOf)`。
- `base.css` §4.1 的 `.grp` 规则替换 `.side-group/.grp-*` 与 `views.css` 的 `.tun-group/.tun-sec*`。
- `TunnelStore` 实现 `GroupScope` 两次（conns/rules），`tunnels.json` 迁移（§2.3）；注册 `conns`/`rules`。
- 退役 §3 列出的八条旧路由及其测试。

测试：
- Rust：`store.rs` 迁移测试——旧形状（无 `default`、`group: null` 行）加载后 `names()[0] == "default"`，
  写回带 `tunnelGroupsV2: true`；再加载不重复插入；删除 `default` 后重载不复活。旧路由 404。
- Rust：`the_tree_is_byte_for_byte_the_node_builds` 绿（复制流程照旧）。
- 实测（19998，agent-browser，亮/暗各一张）：侧栏三组、一空组；拖一行进空组；抓手拖组换序；`⋯` Move；
  Tunnels 两个 tab 的组头与侧栏同解剖；键盘 ↑↓←→。截图存 `docs/assets/20/`（若目录不存在则建）。

### G3 — 新增规则：Group 字段、标题、双 `+` 消歧（面板）

- `add-sheet.js`（MCP）、`tunnel-sheets.js`（连接 / 转发）加 Group select；`openSheet(group)` 的 `group`
  为 `null` 时取 last-used；标题带组名。
- 空态 "Add an MCP" 走同一路径。
- `index.html` sprite 加 `folder-plus`；`#addBtn` 换图标。

测试：
- 面板没有 DOM 测试基建（docs/13 N1 只测纯函数）：`groups.js` 里把 "默认组解析"（`resolveDefaultGroup
  (scope, names, lastUsed)`）和 "标题构造" 抽成纯函数，加进 Node 仓库 `test/admin-pages.test.ts` 同款的
  一个新 `test/admin-groups.test.ts`（只跑 vitest，这是面板源码所在仓库的既有门禁——不算改 Node 服务端）。
- 实测：从组头 `+`、从顶部 New、从空态各开一次弹层，Group 字段值正确；提交后行落在选中的组。

### G4 — Jobs 分组（Rust + 面板）

- `JobDefinition` 增 `group: Option<String>`；jobs 配置行增 `groups`；`validate_config` 跑不变量；
  `JobSystem` 实现 `GroupScope`（`set_order` 改写配置行里数组顺序）；注册 `jobs`。
- `/api/jobs` 每行带 `group`，顶层带 `groups`。
- `jobs.js`：列表改为 `mountGroups(density: "page")`；`jobs-v2.js` 两个弹层（New / New (advanced)）加
  Group 字段。

测试：
- `def.rs`/`api.rs`：缺 `groups` 的旧行解析出 `["default"]`；`group` 指向不存在的组 → 400；
  `PUT /api/groups/jobs/members/x` 未知 job → 404；改组后 `/api/jobs` 行上的 `group` 更新且
  `configRevision` 前进（走 `apply_config`，不重启实例——docs/11 §8 的既有保证要有测试证明没丢）。
- 实测：新建 job 到新组，Run now 仍可用；拖 job 换组不触发重跑。

### G5 — Data 下拉分组（Rust + 面板）

- `/api/db` 行增 `group`；`renderDbSide` 画 optgroup。

测试：
- `dbbrowser_api`/`app.rs` 测试：两组各一连接 → 响应行携带正确 `group`，顺序仍是视觉顺序。
- 实测：下拉出现组名分隔。

### G6 — Secrets 分组（Rust + 面板）

- `secretstore` 增 `groups`/`secretGroups`（值仍只进不出）；一个 `SecretScope` 包装实现 `GroupScope`
  （`set_order` → 400 `secrets have no manual order`，面板不显示拖拽）；注册 `secrets`。
- `views/secrets.js`：内联表单加 Group select；列表改 `mountGroups(density: "page", draggable: false)`。

测试：
- `secretstore` 单元：旧文件缺字段 → 默认；改组不改 `rev` 语义（改组是一次写，`rev` 前进）；
  GET 永不带值（既有断言保持）。
- 实测：存一个密钥到新组；改名组；删组后密钥仍在（落第一组）。

### G7 — Tokens 分组（Rust + 面板）

- `ManagedStore` 增 `tokenGroups`/`tokenMembers`，实现第二个 `GroupScope`；注册 `tokens`。
- `views/tokens.js`：创建行加 Group select；列表改 `mountGroups`；"Use" 标记不受组影响。

测试：
- `managed.rs`：旧文件 → 默认；rotate/revoke 不动组；删组后 token 仍可用（落第一组）。
- 实测：`connect.js` 的复制命令仍取到 Use 标记的 token。

### G8 — 文档收口

- docs/05 追加 §2.3 的三段；docs/07 追加 ADR-015（§8）；docs/18 状态行改为"已实施（V1–V7，`810623a`
  起）"（2026-09-13 核对 git，状态行仍写"待做"）；README docs 表补 17/18/19/20 四行；`.agents/docs`
  按 ask-swiss 的流程刷新 mcp/tunnels/jobs/host/panel/style-design 六份。

## 6. 门禁与交付顺序

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d                        # 不得出现新的双份
# 面板（在 ../local-mcp-gateway 里，只跑既有 vitest；不改 Node 服务端）
npx vitest run test/admin-pages.test.ts test/admin-groups.test.ts
```

顺序 **G1 → G2 → G3 → G4 → G5 → G6 → G7 → G8**，一项一个提交（G2 允许拆成 "Rust 侧 + 退役" 与
"面板复制" 两个，与 docs/13 N3/N4 同款，因为面板复制提交历来单独成行）。依赖：G2–G7 全部依赖 G1；
G3 依赖 G2（Group 字段用 `groups.js` 的纯函数）；G4–G7 互不依赖，可换序；G8 最后。G6/G7 是用户
"都要"的直接结果，但也是最可推迟的两项——若时间不够，先到 G5 也是一个完整可部署的状态。

每个提交：行为变化的测试先红后绿；不加 crate；不 fmt 整仓；注释英文说"为什么"；不提交
`gateway.config.json`/`.env`/`managed.json`/`tunnels.json`/`secrets.json`/`master.key`/`*.log`。
19999 不停不重启不部署；实测全在 19998（`scripts/test-instance.ps1`）。

## 7. 不做什么（留痕）

- Node 服务端的 `/api/groups/{scope}` 同款实现——用户决定只做本项目；记这里，与 docs/19 §"Node 侧"同款。
- 嵌套组、跨作用域组、多选拖拽、按组的批量操作（Start all in group）——后者有价值，但是另一份 spec。
- 组的颜色/图标——颜色只给状态（swiss-design 规则），组靠名字与位置区分。
- 把一级导航（docs/13）也换成 `groups.js`——那是按 pluginId 的只读分组，没有增删改名，不是同一个东西。

## 8. ADR-015（提议；实施时落入 docs/07）

**分组是宿主机制：一个类型、一个作用域注册表、一族路由、一个面板模块。**

三项测试都满足：难以回退（六个作用域、三个状态文件、八条退役路由）；没有上下文会觉得奇怪（"为什么
宿主挂着 jobs 的分组路由？"——答案是分组不是 jobs 的业务，是所有列表的机制，和 `/api/tokens` 宿主
所有的理由同款，`src/builtin.rs:773-799` 有那条断言）；真实取舍如下。

| 选项 | 代价 |
| --- | --- |
| A. 每个插件各自实现分组与路由（现状的延长线） | 第三份代码起就开始分叉；六份测试；面板六个适配器 |
| B. **宿主一个 `Groups` 类型 + `GroupScope` 注册表 + 一族路由**（选定） | 插件要实现一个 6 方法的 trait；宿主多一个模块；旧路由退役 |
| C. 只统一面板，服务端保留两套 | 便宜，但 rename 的 body 都不一样，面板适配器要永远维护差异；Jobs/Secrets/Tokens 还是要新写 |

建议 B。成本：`swiss-host` 增一个模块（≈300 行含测试）；六个 `GroupScope` 实现各 ≈40 行；面板净减
（两份拖拽合一）。

## 9. 怎么验：截图对照

实施完成后在 19998 上截这些（亮/暗各一），与 §0 的截图并排放进 `docs/assets/20/`：

1. MCP 侧栏：三组（一个空组、一个折叠组），选中一行。
2. 同上，拖拽中：一行悬在空组头上（drop-into 环）。
3. Tunnels / Port Forwards：两组。
4. Jobs：两组，一个折叠。
5. "New job" 弹层：从组头 `+` 打开，标题带组名，Group 字段预填。
6. Data 下拉展开：optgroup。
7. Secrets 与 Tokens 页：内联表单里的 Group select。
