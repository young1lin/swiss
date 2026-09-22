# 43 — Data：页签溢出、多库目录，与 mockup B 的最后一段路

> 状态：**已实施**（M1–M5 一阶段一提交：页签溢出、侧栏成树、多库目录、工具条收敛+状态条、文档与 ADR）。
> 增补（实施后 owner 三条）：侧栏连接/数据库选择器由浮层菜单改为**抽屉**（行内展开推移树，grid-rows 动画）；
> 页签条 "+" **每次必新开一个 SQL 页签**（绕过去重；上限只在有可驱逐页时收缩，全忙时越限而不拒开）；
> 卡片**右键三件套**：重命名（custom 覆盖派生标题，Enter 提交/Esc 取消/失焦提交）、关闭左侧、关闭右侧
> （以被右键的卡片为轴，复用批量关闭的一次性脏确认）。
> 增补四（200 行之墙倒下）：侧栏树**一次拉全目录**（limit=2000，服务端目录上限 1000→5000）；分页器退役，
> 各节带渲染上限 200 行 + 「再显示 N 项 / 收起」行（记忆键 schema/节，换库即清）；搜索结果不受上限；
> 脚注改为「共 N 项」，拉断时提示用搜索过滤。
> 终态数字：`npm run check` 84 个文件 / 781 个用例全绿（typecheck ×2 + eslint + 发射新鲜度 + vitest）；
> `base.css` 36,533 B + `views.css` 90,864 B；发射 JS 917,490 B / 51 个文件；`swiss.exe` 10,294,784 B。
> 截图在 `docs/assets/43/`（页签溢出、成树侧栏、多库目录、工具条，明暗各一）。基线 `4b08c43`（master，2026-09-21）。实施分支 `data-full-access`
> （worktree `.agents/worktrees/data`）。本文承接 `docs/42-data-object-tabs-spec.md`：42 的 T1（状态切分）
> 与 T2（页签条）已经做完，**T3（侧栏成树）与 T4（工具条收敛 + 状态条）一行没动**——本文把它们原样收编，
> 再加上 owner 在 T2 走查后提的两条新需求。视觉参考仍是 `docs/assets/42/data-layout-mockup.html`
> （双击打开，键 `1`–`4` 切方案，**`2` 就是目标**；Engine / State 两个下拉覆盖 MySQL、PostgreSQL 各 18 态，
> Redis 12 态）。
>
> 前置阅读（不读会白写）：`AGENTS.md`；`.agents/rules/panel-proof-of-life.md` **全文**；
> `.claude/skills/swiss-ui-design/SKILL.md` §1–§8 与 §16 规则 2 / 3 / 4 / 5 / 10 / 11 / 16 / 18 / 19；
> `docs/42-data-object-tabs-spec.md`（尤其 §1.5「一个都不动」、§2 状态切分表、§5/§6 即本文 M2/M4 的前身）；
> `docs/22-data-parity-spec.md`（W0–W5 三十二项**已全部交付**——本文不是补功能）；
> `docs/37-panel-modern-typescript-spec.md` §7（`h()` 与重画）与 R4；`docs/38`（i18n 的两道门禁）。
> **代码注释与 UI 文案一律英文；中文只在本文这类文档里。**

> 需求原文（owner，2026-09-21，T2 真机走查之后，附两张截图）：
>
> "1. 表打开太多情况没考虑到怎么处理 [截图：页签条被 8 张 `acme_app_dev.xxx` 卡片塞满，
> 尾部的 `+ SQL` 被挤出可视区，条上没有任何"还有更多"的提示]
> 2. 虽然 MCP 设置的是单个库，但是应该是以设置的库为主，设置的 database 为主，然后其他能查询到的
> database 为辅。
> 3. 切换的内容，没有完全按照 HTML 那样显示，[截图：mockup 方案 B 的侧栏树] 有 tables 有 views 这种，
> 总之还有很多地方没有匹配上，我建议你写个零上下文的文档，交接下，我给其他的模型开始写"

---

## 0. 交接

### 0.1 一段话

Data 页的对象页签（多开表 / 控制台 / Redis key / Activity，各自持有自己的筛选、分页与编辑缓冲）已经
落地并真机走查过。剩下三件事：**页签开太多时条上没有出口**（M1）；**侧栏还是一个平铺列表，不是 mockup
里那棵 Tables / Views / Routines 的树**（M2）；**一个 MCP 连接只认死一个 database，同实例上其他库既看不
见也进不去**（M3，唯一需要动后端的一项）。然后是 docs/42 欠的工具条收敛与状态条（M4）与文档（M5）。

### 0.2 已经落地的，不要重做

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| docs/42 T1 | `DbState`（49 字段）切成 `DbConnState` + `DbTab[]` 判别联合，`db-state.ts` 四个访问器 | 已提交 `a07c80f` |
| docs/42 T2 | 页签条：开 / 关 / 激活 / LRU 淘汰、`Ctrl+Tab`、卡片脏点与筛选计数、FK 跳转开新页签 | **代码完成、门禁全绿、走查完毕，但尚未提交**——见 §0.3 第 1 步 |
| docs/42 T3 | 侧栏成树 | **未动** → 本文 M2 |
| docs/42 T4 | 工具条收敛 + 状态条 | **未动** → 本文 M4 |

**T2 真机走查抓到并已修好的四个缺陷**（都配了用例，别改回去）：

1. 切走再切回的页签，缓冲的单元格改动被静默丢弃 —— `dbLoadData(keepOffset, keepEdits)` 加了第二个参数，
   后台页签回来时走 `dbRestoreData()`（`keepEdits: true`）。"重载是新基线"（docs/22）与"回填是把切走时
   丢掉的那页读回来"是两件事，函数名把这条线画出来了。
2. 条重画后键盘焦点掉到 `<body>`，方向键只能按动一次 —— `renderDbTabs()` 里先
   `strip.contains(document.activeElement)` 再决定要不要 `dbFocusActiveTab()`；**焦点在输入框里时绝不抢**。
3. Redis 连接上的控制台卡片写着 "SQL"，而那个框只吃 Redis 命令 —— `dbTabTitle` 按 `dbIsRedis()` 分支，
   新增 `dataTabs.command` / `dataTabs.newCommandConsoleTitle` 两个词条（en + zh）。
4. 有缓冲改动时点 `文/A` 什么都不发生 —— `i18n.ts` 在 `pageHasPendingChanges()` 为真时 toast
   `i18n.pendingBlocksSwitch`，不再沉默。

**T2 未能验证的三条**（诚实报告，不许打勾）：FK 跳转（本机没有任何连接带外键）；Redis 脏点
（要对 owner 的真实 Redis 写 `Set`）；`DROP TABLE` 关掉该表全部页签（要在 owner 的真实库上真建真删一张表）。
三条都有纯函数用例覆盖。

### 0.3 上手的头三件事

1. **先把 T2 提交掉**（worktree 里是脏的）：在 `crates/swiss-panel/panel` 下跑 `npm run check`，
   全绿后在 worktree 根 `git add -A && git commit`。提交信息按仓库习惯写。
   **不要在未提交的 T2 上面叠 M1 的改动**——两件事混进一个 diff，回滚就没法只退一半。
2. **读 `.agents/rules/panel-proof-of-life.md`。** 这个仓库对面板改动只认一种"完成"：真浏览器、
   新加载的页面、真指针事件点过每一个可见控件。vitest 绿 + `node --check` 只是入场券。
3. **确认端口纪律**：19998 是隔离测试实例，19999 **是 owner 正在用的生产实例，一次都不许碰**
   （唯一允许的是只读 `GET /health`）。停 19998 要按端口的属主 PID 停（`scripts/test-instance.ps1 -Stop`），
   **绝不能按进程名**（`Get-Process swiss` 会把 owner 的 19999 一起杀掉）。

### 0.4 环境（逐条可复制）

```bash
# 工作目录：worktree 根，所有命令都从这里起。不要 cd 回主 checkout。
C:\Users\young1lin\dev\local-mcp-gateway-rust\.agents\worktrees\data      # 分支 data-full-access

# 面板（TypeScript 源）与它的门禁 —— 单独一条命令，不要和别的命令用 && 串起来跑：
#   PowerShell 里 cd 混进链式命令会打断后面每一个相对路径。
cd crates/swiss-panel/panel && npm run check
#   = tsc -p tsconfig.json + tsc -p tsconfig.test.json + eslint . + build:check（发射新鲜度）+ vitest run
#   基线：78 个文件 / 729 个用例全绿，eslint 0 error。
npm run build            # 把 src/*.ts 发射到 ../src/admin_assets/js（发射产物是要提交的）

# 后端门禁（worktree 根）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d

# 起测试实例（19998）
scripts/test-instance.ps1 -Stop                 # 先停干净，别叠在旧实例上（旧实例 = 旧二进制）
$env:CARGO_TARGET_DIR = "target-test"; cargo build --release
scripts/test-instance.ps1                       # 或 -Fresh 清掉测试 home
# 令牌：MCP_GATEWAY_TOKEN = "acceptance-token-for-1998"；SWISS_HOME = %LOCALAPPDATA%\swiss-test-home
curl http://127.0.0.1:19998/health               # build.hash 必须等于这次构建的 exe
```

**rust_embed 陷阱（写进肌肉记忆）**：资产内容不进指纹。改了 `crates/swiss-panel/src/admin_assets/**`
**不会**触发 swiss-panel 重编译——release 构建会说 "Finished"，而二进制里还是旧面板。所以顺序永远是：
`npm run build` → **`touch crates/swiss-panel/src/lib.rs`** → `cargo build --release` → 重启 19998 →
**验服务出来的字节**（`curl http://127.0.0.1:19998/admin/js/<模块>.js` 里 grep 新符号），不是只看构建状态。

**真机走查的工具**：`agent-browser`（CDP，真命中测试的点击）。先 `agent-browser skills get core` 读用法。
两条本机踩过的坑：`wait --load networkidle` 在这个面板上会挂死（用 `wait --text` 或 `wait <毫秒>`）；
页面里 `confirm()` 弹出时 `eval` 会阻塞（先 `agent-browser dialog accept`）。`element.click()` 不算走查，
要真指针事件。

**本机可用的四个连接**（19998 的快照状态里就有）：`mysql`（库 `acme_app_dev`，1,7xx 张表）、
`shop`（PostgreSQL）、`redis`、`shop-redis`。

### 0.5 代码地图

```
crates/swiss-panel/panel/src/          面板 TypeScript 源（改这里）
  db-state.ts        DbConnState（连接域）+ DbTab[]（对象域）+ freshTab/dbTab/dbTabs/dbActiveIndex
  data-view.ts       骨架、侧栏渲染、/api/db 拉取、连接切换、表列表（renderDbSide / renderDbTables / dbTableRow）
  data-tabs.ts       页签条：开关切换、LRU 淘汰、卡片渲染、键盘（本文 M1 的主战场）
  data-grid.ts       工具条 renderDbToolbar()、网格、分页、数据拉取（本文 M4 的主战场）
  data-browsers.ts / data-filters.ts / data-edit.ts / data-structure.ts / data-form.ts /
  data-sql.ts / data-cell.ts / data-value.ts / data-csv.ts / data-ddl.ts / data-activity.ts / data-suggest.ts
  groups.ts          band 组件 mountGroup(cfg, slice)：唯一的容器头形状（M2 复用它，别另造）
  menu.ts            popupMenu(anchor, items)，items = { label, fn, danger, sep, pick, on }
  h.ts / util.ts     h(tag, props, ...kids) / fill(host, ...kids) / iconNode(name) / emptyNode / toast
  i18n.ts + locales/en.ts + locales/zh.ts    每一句可见文案两份，缺一个门禁就红
crates/swiss-panel/src/admin_assets/   发射产物（js/）与手写 CSS（styles/base.css、styles/views.css）
                                       + index.html 里的 SVG sprite
crates/swiss-panel/panel/test/         vitest（78 个文件）——db-tabs.test.ts / admin-data-*.test.ts 是近邻

crates/swiss-data/src/dbbrowser_api.rs 面板的 /api/db 路由层（本文 M3 要加一条路由）
crates/swiss-host/src/dbbrowser.rs     DbBrowser / RedisBrowser 两个 trait + 共享的 SQL 构造器
crates/swiss-mcp/src/adapters/         mysql_browser.rs / pg_browser.rs / redis_browser.rs 三个方言实现
```

### 0.6 房规：这些错误已经让人赔过工时，别再犯

1. **只改 `panel/src/*.ts`，永远不要手改 `admin_assets/js/`**（那是发射产物，`build:check` 会抓不新鲜）。
2. **`innerHTML` 是 eslint error。** 用 `h()` 建节点、`fill()` 重画容器。`HProps` 是 DOM 属性名
   （`tabIndex`，不是 `tabindex`）。
3. **`#pane` 的委派监听器是属性式赋值**（`pane.onclick = …`），重画时**不许**改成 `addEventListener`
   （会叠加，一次点击跑两遍）。
4. **vitest 抓不到的三类 bug**（就是 panel-proof-of-life 这条规则存在的原因）：具名导入在 link 期不校验
   （`import { x } from "./a.js"` 而 `x` 其实在 `b.js` → 浏览器里整个模块死，页面白给）；
   `querySelector(".x")` 存在 ≠ 可见（容器还 `hidden` 时断言照样为真）；`element.click()` 绕过命中测试与遮挡。
5. **happy-dom 没有布局**：`offsetWidth`、`getBoundingClientRect()` 返回 0，`scrollIntoView` 可能不存在。
   用例里不要依赖它们；生产代码调用前做存在性守卫（`if (typeof el.scrollIntoView === "function")`）。
6. **可见性探针不要用 `offsetParent`**：`position: fixed` 会让它为 null。用 `hidden === false` + 计算
   `display`，或者读 bounding rect。
7. **每一句新文案都要 `en.ts` + `zh.ts` 两份**，硬编码可见字面量会被 docs/38 的门禁拦下。
8. **改了可见文案的改动要走第二遍中文**：点 `文/A`，确认 `document.documentElement.lang === "zh-CN"`，
   再把字读一遍。
9. **诚实报告**：走不通的流程（缺凭据、要动 owner 的真数据）写成"未验证 + 原因"，绝不打勾。

---

## 1. 与 mockup B 的逐条对照（owner 第 3 条："还有很多地方没有匹配上"）

打开 `docs/assets/42/data-layout-mockup.html`，按 `2`。下表左列是 mockup 里的零件，右列是今天 19998 上
的实际形状。**这张表就是"哪里没匹配上"的完整答案**，每一行都指派了阶段。

| # | mockup B 的零件 | 今天的面板 | 差在哪 | 阶段 |
| --- | --- | --- | --- | --- |
| 1 | 侧栏顶：连接行 = 状态点 + 连接名 + dialect chip + chevron（`connRow`，mockup:649） | `<select id="dbConn">` 原生下拉，选项按 group 折成 optgroup（`data-view.ts:605 renderDbSide`） | 形状不同；没有状态点、没有 dialect chip；更关键的是**这行容不下"当前是哪个 database"** | M3 |
| 2 | （mockup 没画，owner 第 2 条要求）database 行 | 不存在。库名只以 `acme_app_dev.` 前缀的形式出现在每一行表名里 | 见 M3 | M3 |
| 3 | 侧栏顶：**一个**搜索框（`searchBox`，mockup:656） | 三行控件：`#dbGrep` + pg 的 `#dbSchema` 下拉 + `.db-sortrow`（排序 select + 方向按钮） | B 把 schema 变成树里的 band，把排序收进列表头的 `⋯` | M2 |
| 4 | 树：`Tables` / `Views` / `Routines` 三节 band（chevron + 名 + 计数，mockup:732–737） | 一个 `#dbListHead`（写死 "Tables" + 一个 `+`），底下平铺全部行 | **owner 第 3 条正中这里**：视图和表混在一起，看不出哪些是 view | M2 |
| 5 | pg：schema band 在类型 band 之上（mockup:726–734） | pg 已按 schema 分组（docs/22 W1.1），但没有类型层 | 缺一半 | M2 |
| 6 | Redis：按 `:` 折命名空间（mockup:707–721） | 平铺 + 客户端排序 | 450 个 key 的平铺列表没法读 | M2 |
| 7 | 行：**单行**，名字 + 右对齐的次要信息（`tableRow(..., true)`） | 两行：`.db-table-meta` 是块级第二行（`data-view.ts:737` / `:819`） | 行高翻倍，一屏能看到的表少一半 | M2 |
| 8 | 侧栏脚：`1–200 of 1,712` + 上一页 / 下一页（`db-sidefoot`） | `#dbTablesPager` 已经是这个形状 | **已匹配** | — |
| 9 | 对象页签条（`.otabs`） | 已交付（docs/42 T2，`.db-tabstrip`） | 形状匹配，但**满了以后没有出口** | M1 |
| 10 | 一条 viewrow：标题 + 视图 seg + **一个**主动作 | `.db-head-ctl` 一行里 11 个同权重控件（seg + 页长 + 分页读数 + 上下页 + Refresh + Row + CSV + Export + Import + SQL + ⋯） | docs/42 §0.1 的四条硬伤，一条没修 | M4 |
| 11 | 结构 tab 四个：Data / Form / Structure / DDL | 六个：Data / Form / Columns / Indexes / Foreign Keys / DDL | Columns / Indexes / FK 是同一件事的三张表 | M4 |
| 12 | 底部状态条：行数区间 + 分页 + 页长 + 耗时 + 可编辑性 + 连接名（mockup:1275） | 不存在；分页读数挤在工具条里 | | M4 |
| 13 | 提交条 `.commitbar` | `.db-bar` 已经是这个形状 | **已匹配**（和状态条是**两条**，职责不许混） | — |
| 14 | dock（下半屏 SQL / Value / Row / Messages / Activity） | 不做 | docs/42 §9 定的第二期，本文**不翻案** | — |

---

## 2. M1 — 页签开太多（owner 第 1 条）

### 2.1 今天到底会发生什么（事实，带行号）

- 上限 `DB_TAB_MAX = 8`（`data-tabs.ts:59`）。开第 9 个时 `dbOpenTab()` 找一个"最久没碰过、且不是当前
  页、且**不持有工作**"的页签淘汰（`dbEvictTarget`，`data-tabs.ts:111`）；全部 8 个都持有工作时**拒绝**
  打开并 toast `dataTabs.fullCloseOne`。这一层逻辑是对的，**不要改它的语义**。
- 但**淘汰是静默的**：一张干净的后台卡片就这么没了，用户不会被告知是哪一张。
- 条本身 `overflow-x: auto` 且**滚动条被隐藏**（`views.css:528`：`scrollbar-width: none` +
  `::-webkit-scrollbar { display: none }`）。于是溢出**没有任何视觉提示**。
- 尾部的 `+ SQL` 按钮（`.db-tab-add`）**在滚动容器里面**，所以它会跟着卡片一起滚出可视区——
  owner 截图里正是这个状态：想开控制台，却找不到 `+`。
- 激活一个滚动区外的页签（`Ctrl+Tab` / 方向键 / FK 跳转 / 侧栏点击）后**不会把它滚进可视区**——
  代码里没有任何 `scrollIntoView`。
- 卡片名恒定带 schema 前缀（`dbTabTitle`，`data-tabs.ts:151`：`(t.schema ? t.schema + "." : "") + t.table`）。
  在 MySQL 上那就是每张卡片先花掉 `acme_app_dev.` 这 13 个字符，而 `.db-tab { max-width: 210px }`
  一共就那么宽——真正的表名被截断成 `acme_app_dev.shop_orde…`。
- 没有中键关闭，没有"关闭其他 / 关闭右侧"，没有列出全部已开对象的溢出菜单。

### 2.2 决定

| # | 决定 | 为什么 |
| --- | --- | --- |
| D1 | 条拆成**可滚动区 + 钉死的尾部**：`.db-tabstrip` 变成外层 flex，里面 `.db-tabstrip-scroll`（卡片，`overflow-x: auto`）+ `.db-tabstrip-end`（`flex: none`，装溢出按钮和 `+`），尾部带一条左侧 hairline 与滚动区分开 | 最频繁的手势（开控制台、找某个页签）不能随内容滚走（swiss-ui-design 规则 19） |
| D2 | 激活后把活动卡片滚进可视区：`scrollIntoView({ block: "nearest", inline: "nearest" })`，调用前做存在性守卫 | 键盘走条时，看不见的"当前页签"等于没有当前页签。happy-dom 没有这个方法（房规 5） |
| D3 | 卡片名去掉**冗余**的限定前缀：`t.schema` 等于当前作用域（MySQL 的当前 database / pg 的当前 schema）时只画表名；**跨库或跨 schema 时才画 `schema.table`**，`title` 永远是全名 | 210px 得留给真正区分两张卡片的那部分。跨库以后前缀才携带信息（M3 依赖这条） |
| D4 | 尾部加一个溢出按钮（`chevron-down`，`aria-haspopup`），`popupMenu` 列出**全部**已开对象：类型图标 + 名 + 脏点，当前项打勾（`pick`/`on`）；菜单末尾一条分隔线后是 `Close others` / `Close to the right` / `Close all`，其中任何一条会关掉带缓冲改动的页签时，走**既有的关闭确认**，不许静默丢 | 条滚出去以后，菜单是唯一还能看见全貌的地方；三条批量关闭是浏览器页签的通用习语 |
| D5 | **淘汰不再静默**：LRU 关掉一张干净后台卡片时 toast 说出它的名字（新词条 `dataTabs.evictedForRoom`，英文形如 `Closed "orders" to make room — it had no unsaved changes.`） | 东西凭空消失是"页面坏了"的最常见误判来源 |
| D6 | 上限 8 → **12**，并把理由写进 `DB_TAB_MAX` 上方的注释：真正的保护是"淘汰只吃干净页签 + 全脏就拒绝"，8 是在条还不能滚、也没有溢出菜单时定的保守值 | owner 的场景就是"表打开太多"。12 张 × 单卡片 ≥ 96px，在 1440px 下配合滚动与菜单是可用的 |
| D7 | 条上的竖向滚轮转成横向滚动（`wheel` 里 `scrollLeft += deltaY`，仅当确实可滚动时 `preventDefault`） | 横条上的竖滚轮否则是个死手势 |
| D8 | 中键关闭（`auxclick`，`button === 1`），与 `×` 复用同一条关闭路径（同样的脏页签确认） | 浏览器页签习语；实现成本是一个分支 |
| D9 | **不做**：页签拖拽重排、页签分屏、把页签持久化到下次开面板 | docs/42 §9 已经定过，本文不翻案 |

### 2.3 验收

**vitest**（扩 `test/db-tabs.test.ts`，纯函数优先）：

- `DB_TAB_MAX === 12`，且开到第 13 个时 `dbEvictTarget` 仍只挑"干净且非当前"的受害者；全脏 → 返回 `null`
  → `dbOpenTab` 拒绝并 toast（既有用例保持绿）。
- 淘汰发生时 toast 被调用**一次**且消息里带受害者的名字（D5）。
- 卡片名：`schema` 等于当前作用域 → 只有表名；不等 → `schema.table`；两种情况下 `title` 都是全名（D3）。
- 溢出菜单的条目由 `dbTabs()` 一一对应生成，顺序与条一致，当前项被标记（D4）。
- `Close others` 留下且只留下当前页签；`Close to the right` 只砍当前之后的；两者遇到带缓冲改动的页签
  都会触发确认路径（用 spy 断言确认被问过，而不是断言"关掉了"）。
- 中键与 `×` 走的是同一个关闭函数（D8）。
- 重画后不抢输入框焦点这条既有断言**必须仍然绿**（T2 的教训 2）。

**19998 真机走查**（`panel-proof-of-life` 第 4 条，真指针事件，新加载的页面）：

1. 在 `mysql` 连接上连开 12 张表 + 1 个控制台 → 确认第 13 个触发淘汰，**toast 说出被关掉的是谁**。
2. 条滚到最右 / 最左时，`+` 与溢出按钮**始终可见**（读它们的 bounding rect，不要用 `offsetParent`）。
3. 溢出菜单：打开 → 列出 13 项 → 点其中一个滚动区外的 → 该卡片被激活**并滚进可视区**（断言其 rect 在
   条的 rect 内）。
4. `Ctrl+Tab` 走一圈，每一步活动卡片都在可视区内；焦点始终留在条上。
5. 在某张表上改一个单元格（卡片出现琥珀色脏点）→ `Close others` → **确认对话框出现**；取消后那张页签
   与它的改动都还在。
6. 中键点一张干净卡片 → 关掉；中键点脏卡片 → 先问。
7. 条上滚竖向滚轮 → 条横向移动。
8. 1440px 与 900px 两个宽度，明暗两套主题。
9. **第二遍中文**：`文/A` → `document.documentElement.lang === "zh-CN"` → 新 toast 与菜单项读一遍。

---

## 3. M2 — 侧栏成树（= docs/42 T3，原样收编 + 补强）

### 3.1 做什么

1. **SQL 连接按对象类型分节**：`Tables` / `Views` / `Routines` 三个 band。数据源是
   `ApiDbTableRow.type`（**可选字段**，服务端不保证给；缺失一律归 `Tables`）。
2. **band 一律复用 `groups.ts` 的 `mountGroup(cfg, slice)`，`density: "side"`**（28px 头、`.side-row` 子行）。
   **不要另造一套 band 的 CSS 或 DOM**——swiss-ui-design 规则 5 只允许一种容器头形状。配置要点：
   - `scope: "dbtree"`（折叠状态走组件自带的 `loadCollapsed` / `saveCollapsed`，localStorage 键由 scope 派生）；
   - **`draggable: false`**：这棵树的分组是从目录**推导**出来的，不是操作者命名的容器，拖它没有意义；
   - `filtered: true` 当侧栏搜索框有内容时——命中的节强制展开，没有命中的节隐藏；
   - `onAdd` 只在 `Tables` 节上给出（`New table…`，接今天 `#dbNewTable` 那条流程），`Views` / `Routines`
     节不给 `+`。
3. **pg 两层**：schema band（操作者命名的容器）在外，类型 band 在内。今天 pg 已经按 schema 分组
   （docs/22 W1.1），把它改成 `mountGroup` 的嵌套，并且**干掉侧栏顶那个 `#dbSchema` 下拉**——schema 已经
   是树里可见的一层，再留一个下拉就是两套机制说同一件事。`d.schemaFilter` 这个状态字段**保留**
   （`/tables` 的 `schema` 查询参数还要用它），只是不再由下拉驱动。
4. **Redis 按 `:` 折命名空间**：新增纯函数 `redisNamespaceTree(keys)`，逐段分组并做**单子节点路径压缩**——
   `user:1001:profile` + `user:1002:profile` → 一个 `user` 节点两个孩子；`stream:orders` 独一份 → 直接
   坐在根上一行，不造空文件夹。
5. **行变单行**（对照表第 7 行）：名字 + 右对齐次要信息，`--text-3`、`tnum`、`--f-caption`。
   `.db-table-meta` 从块级改成行内右对齐。完整信息进 `title`（Redis 行今天已经这么做了，抄它）。
6. **排序从侧栏顶收走**：`.db-sortrow` 的排序键与方向进 `Tables` 节 band 头的 `⋯`（`mountGroup` 的
   header 自带 `⋯` 位）。搜索框留在顶上，**顶上就只剩连接行（M3 接管）+ 搜索框**。

### 3.2 验收

**vitest**（新增 `test/db-tree.test.ts`，全是纯函数）：

- `redisNamespaceTree([])` → `[]`；
- 两个同前缀键 → 一个节点两个孩子；
- 不含 `:` 的键坐在根上；
- 单子节点链压缩成一行（`a:b:c` 独一份 → 一行 `a:b:c`，不是三层空文件夹）；
- 1,000 个键的分组是**稳定序**（口径与既有的 `dbRedisCompare` 一致）；
- SQL 分节：`type` 缺失的行归 `Tables`，`type: "view"` 归 `Views`；
- 搜索命中时 `filtered` 让含命中项的节展开、不含的节不出现。

**19998 走查**：MySQL（1,7xx 张表）三节展开 / 折叠，`grep` 之后树仍成立，刷新页面后折叠状态被记住；
pg 的 schema × 类型双层不塌，`#dbSchema` 下拉已消失而 schema 过滤仍生效；Redis 命名空间展开到叶子并能
开成页签；单行行在 900px 下不换行、`title` 给出全文。1440 / 900 两宽，明暗两套，**第二遍中文**。

---

## 4. M3 — 多库目录：设置的库为主，其他为辅（owner 第 2 条）

**这是本文唯一需要动后端的一项。** docs/42 §9 明文写着"发现确实需要新端点就停下来报告，不要顺手加"——
现在就是那次报告的结果：owner 直接要了这个能力，所以本文**授权**这一条新路由和一处参数语义的扩展，
范围严格限定在 §4.2 写死的契约里。

### 4.1 今天后端的真实形状（三方言逐条，别猜）

| | MySQL | PostgreSQL | Redis |
| --- | --- | --- | --- |
| 库名从哪来 | 适配器配置/DSN 的 `database`（`mysql.rs`）。**没有它就根本不提供浏览器**：`browser()` 里 `let database = self.database.clone()?;`（`mysql.rs:844`） | `parse_pg_url` 从 URL path 取（`pg.rs:341`，字段 `pg.rs:759`）。**`PgBrowser` 结构体里压根没存库名**（`pg_browser.rs:43`，只有 `label`） | 连接 URL 的库号（默认 0） |
| `list_tables` 的 `schema` 参数 | **被忽略**，一律用 `self.database`（`mysql_browser.rs:134/141`；pg 侧注释 `pg_browser.rs:175` 明写 "MySQL has no such parameter and ignores it"） | **就是 schema**，在**当前库之内**过滤（docs/22 W1.1） | 不适用 |
| 每行返回的 `schema` 字段 | `self.database`——**MySQL 的"schema"在这套 API 里语义上就是 database**（`mysql_browser.rs:150`） | 真的 schema | 不适用 |
| 换一个库要付什么 | **零**：同一条连接就能 `db.table` 限定，读写语句本来就是用 `Some(&self.database)` 去限定的（`mysql_browser.rs:175/186/207/477/491…`），把这个值换掉即可 | **一条新连接**：`PgPool` 绑死在一个 database 上，换库必须新建池 | **一条新连接**：`SELECT` 在共享连接上是禁用命令（`redis.rs:135` 的 `CONNECTION_BREAKING` 名单——它会把池里那条连接永久改模式） |
| 库清单怎么查 | `information_schema.schemata`（再用 `information_schema.tables` 一次 group by 拿表数） | `pg_database`（`datallowconn AND NOT datistemplate`） | `INFO keyspace` 给出 `db0:keys=…` 各库键数；当前库号用 `CLIENT INFO` 的 `db=` 字段（`CLIENT INFO` 在允许名单里，`redis.rs:217`） |

**结论**：MySQL 能真正做到"主库 + 辅库都能浏览"；pg 与 Redis 在不新建连接的前提下**只能把辅库列出来**，
不能打开。本文照这个事实定范围——把做不到的事写成 UI 上一句诚实的理由，比偷偷加一套连接池安全得多
（连接租借与凭据纪律是 docs/42 §1.5 的"一个都不动"）。

### 4.2 后端契约（只加这些，不许再多）

**新路由** `GET /api/db/{name}/databases`，挂进 `dbbrowser_router`（`dbbrowser_api.rs:1082`）：

```jsonc
{
  "primary":  "acme_app_dev",     // 这个 MCP 配置里写死的库；null = 该方言不认识"库"这个轴
  "current":  "acme_app_dev",     // 当前这条连接实际连着的库
  "databases": [
    { "name": "acme_app_dev", "primary": true,  "browsable": true,  "system": false, "tables": 1712 },
    { "name": "acme_app_uat", "primary": false, "browsable": true,  "system": false, "tables": 1680 },
    { "name": "mysql",        "primary": false, "browsable": true,  "system": true,  "tables": 31   },
    // pg / redis 的非当前库：
    { "name": "template_app", "primary": false, "browsable": false, "system": false,
      "reason": "A Postgres connection is bound to one database; browsing this one needs its own connection." }
  ]
}
```

- `tables` 可选（pg 跨库拿不到就不给）。`reason` 是**服务端给的英文句子**，直接显示——先例是
  `ApiDbDataPage.editNote`，同样是服务端英文文案（docs/38 的 bare-literal 门禁只管面板源码里的字面量）。
- trait 上加 `async fn list_databases(&self) -> Result<Value, String>`，**带默认实现**返回
  `{"primary": null, "current": null, "databases": []}`，这样 `DbBrowser` / `RedisBrowser` 两个 trait 的
  其他实现与测试桩不用改一行就能编译。
- 三个方言各自实现：MySQL / pg 用 §4.1 那两条 SQL；Redis 用 `INFO keyspace` + `CLIENT INFO`。
  系统库（MySQL 的 `information_schema` / `mysql` / `performance_schema` / `sys`，pg 的 `postgres`）
  标 `system: true`，**照样返回**，由面板排在最后并压暗。

**一处参数语义扩展**（不是新参数）：**MySQL 的 `schema` 参数从"忽略"改成"库名"**。

- 影响 `/tables`、`/data`、`/schema`、`/export` 四条读路由。缺省 = `self.database`，行为与今天逐字节相同。
- **给了值就必须先查白名单**：拿 `list_databases` 的结果比对，不在清单里 → 400。白名单通过之后
  仍然走既有的 `quote_ident`（两道，不是二选一）。
- **写路径一个都不动**：`/edits`、`/ddl`、`/import` 继续用 `self.database` 限定。当 `schema` 指向非主库时，
  `/data` 返回 `editable: false` 并把 `editNote` 写成
  `Read-only: <db> is not this connection's configured database (<primary>).`——面板已经在读这两个字段，
  于是"辅库只读"这条规则不需要面板再加一层判断。
- pg 的 `schema` 语义**不变**（仍是库内 schema）。Redis 不接这个参数。

### 4.3 面板做什么

1. **`DbConnState` 加两个字段**：`database: string`（当前选中的库，空串 = 用连接的默认库）与
   `databases: ApiDbDatabase[] | null`（懒拉，第一次打开选择器时才发请求）。类型进
   `panel/src/types/api.d.ts`（`ApiDbDatabase`）。
2. **侧栏顶两行**（接 M2 留下的空位）：
   - 第一行：**连接行**——状态点 + 连接名 + dialect chip + chevron，点开 `popupMenu` 列出全部连接，
     **保留 docs/20 G5 的分组语义**（今天是 `<optgroup>`，菜单里用 `sep` 分隔并把组名作为不可点的标题项）。
     这一行取代 `<select id="dbConn">`。
   - 第二行：**database 行**——库图标 + 当前库名 + chevron；只在 `databases` 非空时出现。菜单里
     **主库排第一并带一个 `primary` chip**，一条分隔线之后是其余库（`system: true` 的排最后、压暗），
     `browsable: false` 的条目**禁用**，`title` 就是服务端给的 `reason`。
   - 两行都是 band 形状，和 M2 的树是同一套视觉（swiss-ui-design 规则 5）。**不要做成面包屑**——
     chevron 必须让它一眼看出是下拉（§13 清单第 6 条）。
3. **切库 = 切连接的轻量版**：走**既有的** `dbOkToLeave()` 确认（`data-view.ts:433` 那条，按整条页签条的
   缓冲改动总数发问），然后清 `tables` / `tablesPage` / `grep` / `sort`，`dbResetTabsForConn()` 关掉全部
   对象页签（它们属于旧库），重新拉表列表。**理由**：一张 `orders` 页签在换库之后指向的是另一张表，
   留着它比关掉危险得多。
4. **表名前缀跟着走**：M1 的 D3 规则在这里兑现——浏览辅库时卡片名与侧栏行显示 `db.table`，浏览主库时
   只显示 `table`。
5. **辅库只读要说出来**：状态条（M4）读 `editNote`；M4 之前先在网格既有的只读提示位置显示它。

### 4.4 验收

**Rust（`cargo test --workspace`）**：

- `dbbrowser_api.rs` 的桩浏览器测试模块里加一条：`GET /api/db/{name}/databases` 透传浏览器的 JSON；
  浏览器返回 `Err` 时按既有 `reply_with` 规则映射状态码。
- 默认实现：一个没实现 `list_databases` 的桩浏览器返回空目录（证明 trait 默认值编译且语义正确）。
- MySQL `list_tables` 的 `schema` 参数：给主库名 → 与不给时**同一条 SQL、同一组绑定**；给白名单外的名字
  → 400，且这个名字**没有被拼进任何 SQL**（断言"查白名单发生在构造语句之前"）。
- MySQL `/data` 在 `schema != primary` 时返回 `editable: false` 且 `editNote` 含主库名。
- pg：`list_databases` 里除 `current` 外全部 `browsable: false` 且带 `reason`。
- 既有的 `/tables`、`/data` 测试**一条断言都不许改**（缺省行为逐字节不变）。

**vitest**：

- `ApiDbDatabase` 排序纯函数：主库第一，其余按名字，`system: true` 垫底。
- 选择器菜单：`browsable: false` 的项 `disabled` 且 `title === reason`。
- 切库调用 `dbOkToLeave()`；返回 false 时**什么都没变**（库名、页签、表列表原样）。
- 切库成功后 `dbTabs()` 只剩占位页签，`grep` / `sort` 已复位。

**19998 走查**：在 `mysql` 上打开库选择器 → 看到 `acme_app_dev` 带 `primary` chip 排第一，其他库在分隔线
下方，系统库压暗垫底 → 切到另一个库 → 侧栏换成那个库的表 → 打开一张表 → **网格只读且说出原因** →
切回主库 → 可编辑恢复。在 `shop`（pg）上打开选择器 → 其他库列出但点不动，悬停给出英文理由。
在 `redis` 上 → 列出 `db0…dbN` 与各自键数，非当前库点不动。**带缓冲改动时切库 → 确认对话框出现**。
1440 / 900，明暗两套，**第二遍中文**。

---

## 5. M4 — 工具条收敛 + 状态条（= docs/42 T4，原样收编）

### 5.1 做什么

1. **工具条只画当前页签的控件**：
   - `table` 页签：标题 + `pane` 的 seg + **一个**主动作 + `⋯`；
   - `sql` 页签：`Run` + `⋯`（Explain / Format / History / 收藏全进 `⋯`）；
   - `key` 页签：随类型变的主动作（hash → `+ Field`，list → `+ Item`，zset → `+ Member`，string 无主动作）+ `⋯`；
   - `activity` 页签：`Refresh` + `⋯`。
2. **状态条**：页体底部一条，承载 `1–50 of 550,968`、上下页、页长、耗时、可编辑性/只读原因
   （`ApiDbDataPage.editNote`）、连接名。今天的 `.db-bar`（提交条）**职责不变**，状态条是**另一条**。
3. **六折四**：`DB_TABS` → `Data / Form / Structure / DDL`；Structure 内部再分 Columns / Indexes /
   Foreign Keys（`data-structure.ts` 已经有三张表的渲染，只是换个容器）。
4. `Refresh` / `CSV` / `Export…` / `Import…` 全部进 `⋯`。

### 5.2 验收

新增 `test/db-toolbar.test.ts`：

- **四种页签下，工具条里的非图标 `.btn` 数量 ≤ 1**（swiss-ui-design 规则 4 的机器门禁——这条比任何截图都硬）；
- `sql` 页签的工具条里没有分页器、没有 `+ Row`；
- `DB_TABS.length === 4`；
- 状态条文本在 `editable: false` 时带上 `editNote` 的原文（M3 的辅库只读在这里露面）；
- Redis `key` 页签的主动作随类型变。

**19998 走查**：四种页签各自的工具条逐个按钮真点；`⋯` 展开后 Export / Import 仍可达并完成一次真导出；
只读连接下状态条说出原因；1440 / 900，明暗两套，**第二遍中文**。

---

## 6. M5 — 文档与 ADR

1. `swiss-ui-design` skill §7：补一句"Data 的资源导航用对象页签，因为它要同时持有多个对象"（docs/42 §1.2 的结论）。
2. `.agents/docs/data.md` 的面板模块地图按新结构重写。
3. `docs/21-data-web-gap-analysis.md` 的差距表：被本文关掉的条目标注 `→ docs/43`。
4. README 文档表加一行（本文）。
5. docs/42 与本文的状态头改"已实施"，各附最终数字（`npm run check` 的文件/用例数、`base.css` + `views.css`
   字节、发射 JS 字节、`swiss.exe` 字节），截图进 `docs/assets/43/`。
6. **ADR-026**（docs/42 §10 的草案：对象页签是 L3 的一半，不是新的一层）与 **ADR-027**（下）一起落进
   `docs/07-decisions.md`。

**ADR-027 草案 —— Data 的多库：主库可写，辅库只读，跨库连接不建**

- **题**：一个数据库 MCP 配置的是单库，但同一台实例上通常还有别的库。面板要不要、以及如何让它们可达。
- **选项**：(a) 维持现状，一个连接一个库；(b) 只对 MySQL 开放同连接跨库浏览，其余方言只列不开；
  (c) 为每个库建独立连接池，三方言一致。
- **取 (b)**。理由：MySQL 的跨库限定是零成本的（语句本来就带库限定），而 pg / Redis 要付一整套
  "按库缓存连接池 + 租借 + 凭据"的代价，换来的只是浏览便利；(c) 与 docs/42 §1.5 的"连接租借不动"直接冲突。
- **代价**：方言之间能力不对称。用一句服务端给的英文理由把这件事对用户讲明白，而不是把按钮灰在那里不解释。
- **写入侧的边界**：辅库一律只读，写路径继续钉在配置的主库上——"配置里写的那个库"仍然是这条连接的
  权限边界，能浏览别的库不等于获得了在别的库上写的授权。

---

## 7. 门禁与交付顺序

### 7.1 门禁（每个提交前，全绿才提交）

```bash
# 面板 —— 在 crates/swiss-panel/panel 下，单独一条命令
npm run check          # typecheck ×2 + eslint + emit 新鲜度 + vitest

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

`npm run check` 顺带跑 docs/38 的两道门禁：字典完整性与 bare-literal 零门槛。本文新增的每一句 UI 文案
都要 `en.ts` + `zh.ts` 两份。

### 7.2 顺序：一阶段一提交，不许跳

| 序 | 阶段 | 为什么在这个位置 | 关键风险 |
| --- | --- | --- | --- |
| 0 | 提交 T2 | 见 §0.3 | 不要和 M1 混进一个 diff |
| 1 | **M1** 页签溢出 | 纯前端、纯局部，与其余各项无耦合；owner 的第 1 条 | `scrollIntoView` 在 happy-dom 下不存在；`+` 从滚动区移出去时别把既有的键盘顺序弄反 |
| 2 | **M2** 侧栏成树 | M3 的选择器要挂在树的顶上，树先立起来 | 复用 `mountGroup`，`draggable: false`；`ApiDbTableRow.type` 是可选字段；折叠状态走组件自带的 load/save |
| 3 | **M3** 多库目录 | 唯一动后端的一项，独立成一个提交便于回滚 | 白名单校验**先于**拼 SQL；写路径一行不动；既有 `/tables`、`/data` 的测试断言一条不许改 |
| 4 | **M4** 工具条 + 状态条 | 状态条要显示 M3 的只读原因 | 规则 4 的 `.btn` 计数用例是硬门禁；状态条与 `.db-bar` 是**两条** |
| 5 | **M5** 文档 + 两条 ADR | 最后 | 旧的"已实施"描述一条不留 |

每阶段的固定动作：改 `panel/src` → 写用例 → `npm run check` → `npm run build` →
**`touch crates/swiss-panel/src/lib.rs`** → release 构建 → 重启 19998 → **真浏览器走查该阶段的清单**
（1440 / 900 两宽，明暗两套；改了文案就走第二遍中文）→ 提交。

### 7.3 实例纪律

19998 是隔离测试实例（`scripts/test-instance.ps1 -Stop` / `-Fresh`，`CARGO_TARGET_DIR=target-test`）。
**19999 是 owner 的生产实例，一次都不许碰**（唯一允许的是只读 `GET /health`）。停实例按端口属主 PID 停，
**绝不按进程名**。19998 被别的会话占着就换 19997（同脚本、独立测试 home）。

---

## 8. 不在范围内

| 不做 | 为什么 |
| --- | --- |
| mockup D 的 dock（上网格下 dock） | docs/42 §9 定的第二期，本文不翻案 |
| `⌘K` 命令面板 | 桌面习语，对偶尔打开的面板是净损失 |
| 页签拖拽重排、分屏、工作区持久化 | docs/42 §9；`last-page.ts` 的契约不扩大 |
| 为 pg / Redis 建按库的连接池 | ADR-027 取 (b) 的直接后果；要做就是另一篇 spec + owner 点头 |
| 在辅库上写（编辑 / DDL / 导入） | ADR-027 的边界：配置里的库是这条连接的权限边界 |
| 虚拟滚动、可视化建表器、ERD / schema diff、完整元数据缓存 | docs/21 #31–#35 的"不做"清单 |
| App Shell、`page-registry`、`last-page.ts`、连接租借、凭据纪律、docs/22 已交付的 32 项语义 | docs/42 §1.5「一个都不动」，本文继承 |
| 除 §4.2 写死的那一条路由与那一处参数语义之外的任何 `/api/db` 改动 | 发现还需要别的，**停下来报告**，不要顺手加 |
