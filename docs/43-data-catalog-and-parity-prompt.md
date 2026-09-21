# docs/43 实施提示词 — Data：页签溢出、多库目录，与 mockup B 的最后一段路

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

worktree **已经建好**，直接进：

```
<repo>\.agents\worktrees\data
```

分支 `data-full-access`。**不要 `cd` 回主 checkout。** PowerShell 里 `cd` 混在链式命令中会打断后面每一个
相对路径——构建脚本与实例脚本各起一条命令，从 worktree 根跑。

面板的 npm 家在 `crates/swiss-panel/panel`，`node_modules` 不进 git——**第一次先在那里 `npm ci`**，
否则 `npm run check` 直接报 `'tsc' is not recognized`。

**第一件事：worktree 是脏的。** docs/42 的 T2（页签条）已经写完、门禁全绿、19998 上走查完毕，但**还没提交**。
先在 `crates/swiss-panel/panel` 跑 `npm run check` 确认全绿（基线：78 个文件 / 729 个用例），然后在
worktree 根把它提交掉。**不要把你自己的改动叠在这堆未提交的改动上面**——混进一个 diff 就没法只回滚一半。

## 任务

`docs/43-data-catalog-and-parity-spec.md` 定义的 **M1 → M5**。一句话：把 Data 页收尾——

- **M1**：页签开太多时条上要有出口（钉死的 `+` 与溢出菜单、活动页签自动滚进可视区、去掉冗余的库名前缀、
  淘汰不再静默、上限 8 → 12、中键关闭、批量关闭）。
- **M2**：侧栏从平铺列表变成树（SQL 分 Tables / Views / Routines，pg 再叠一层 schema，Redis 按 `:` 折
  命名空间，行由两行改一行）。**这是 docs/42 T3，一行没做过。**
- **M3**：一个 MCP 配的是单库，但**以配置的库为主、同实例上其他能查到的库为辅**——新增一条
  `GET /api/db/{name}/databases`，MySQL 的 `schema` 参数从"忽略"改成"库名"（辅库只读），侧栏顶多一行库选择器。
  **本文唯一动后端的一项**，契约在 spec §4.2 写死，不许扩。
- **M4**：工具条只画当前页签的控件，分页/耗时/可编辑性下沉到底部状态条，六个结构 tab 折成四个。
  **这是 docs/42 T4，一行没做过。**
- **M5**：文档 + ADR-026（页签）与 ADR-027（多库）。

App Shell（rail / context bar / focus 模式）、`page-registry`、`last-page.ts`、连接租借、凭据纪律、
docs/22 已交付的 32 项语义——**一个都不动**。`/api/db` 除 spec §4.2 写死的那一条新路由与那一处参数语义，
**任何别的改动都要先停下来报告**。

先双击打开 `docs/assets/42/data-layout-mockup.html`，按 `2`：那就是目标（Study B）。Engine 下拉切
MySQL / PostgreSQL / Redis，State 下拉覆盖 30 种状态——实施时逐个对照。spec §1 有一张**逐条对照表**，
列清了 mockup 的每个零件今天长什么样、差在哪、归哪个阶段。

## 先读这些（按顺序，别跳）

1. `docs/43-data-catalog-and-parity-spec.md` —— **全文**。§0 是交接（已落地的别重做、环境、代码地图、
   九条房规），§1 是与 mockup 的对照表，§2–§6 是每阶段的做法、用例与 19998 走查项，§7 是门禁与顺序。
2. `.agents/rules/panel-proof-of-life.md` —— 全文，逐条。**每个阶段都要在 19998 上真点。**
   vitest 绿 + `node --check` 是入场券，不是证明。
3. `.claude/skills/swiss-ui-design/SKILL.md` —— §1–§8（四层与归属；本文一层都不新增）、
   §16 规则 2 / 4 / 5 / 10 / 11 / 16 / 18 / 19、§13 清单。M5 要改它的 §7。
4. `docs/42-data-object-tabs-spec.md` —— §1.5「一个都不动」、§2 状态切分表、§5/§6（本文 M2/M4 的前身）、
   §9 不做清单、§10 ADR-026 草案。
5. `crates/swiss-panel/panel/src/data-tabs.ts` —— **全文**。M1 的主战场：`DB_TAB_MAX`(:59)、
   `dbEvictTarget`(:111)、`dbTabTitle`(:151)、`renderDbTabs`(:175)、`dbOpenTab`(:316)、`dbActivateTab`。
6. `crates/swiss-panel/panel/src/data-view.ts` —— `renderDbView` 的骨架（侧栏那 7 个控件）、
   `renderDbSide`(:605)、`dbLoadTables`、`renderDbTables`(:704)、`dbTableRow`(:811)、
   `dbChromeChange` 里的连接切换(:426) 与 `dbOkToLeave()`。M2/M3 都在这里。
7. `crates/swiss-panel/panel/src/groups.ts` —— `mountGroup(cfg, slice)` 的 cfg 文档（:145–:172）与
   `loadCollapsed` / `saveCollapsed`。M2 **复用它**，不要另造 band。
8. `crates/swiss-panel/panel/src/menu.ts` —— `popupMenu(anchor, items)`，`items = { label, fn, danger,
   sep, pick, on }`，自带键盘（首项聚焦、方向键、Esc）。M1 的溢出菜单与 M3 的两个选择器都用它。
9. `crates/swiss-panel/panel/src/data-grid.ts:455 renderDbToolbar()` —— 那条过载的 `.db-head-ctl`，M4 拆它。
10. `crates/swiss-data/src/dbbrowser_api.rs` —— 路由表在 :1082；`browsable_connections`(:78) 是
    `/api/db` 的行形状；文件尾部 `#[cfg(test)] mod tests` 里有一个桩浏览器 + 真路由的测试法，M3 照着加。
11. `crates/swiss-host/src/dbbrowser.rs:708` —— `DbBrowser` trait（M3 在这里加带默认实现的 `list_databases`），
    `RedisBrowser` 在它下面。
12. `crates/swiss-mcp/src/adapters/mysql_browser.rs` / `pg_browser.rs` / `redis_browser.rs` —— 三个方言实现。
    读之前先看 spec §4.1 那张表：MySQL 的 `schema` 今天被忽略、pg 的 `schema` 是库内 schema、
    Redis 的 `SELECT` 在禁用名单里。**这三条事实决定了 M3 的范围，别推翻，先照做。**

## 交付顺序

一个阶段一个提交，不许跳。

| 序 | 阶段 | 关键风险 |
| --- | --- | --- |
| 0 | **提交 T2**（worktree 里的既有改动） | 别和 M1 混进一个 diff |
| 1 | **M1** 页签溢出 | `scrollIntoView` 在 happy-dom 下不存在（调用前做存在性守卫）；`+` 移出滚动区时别把键盘顺序弄反；"全脏就拒绝开新页签"的语义**不许改** |
| 2 | **M2** 侧栏成树 | 复用 `mountGroup`，`draggable: false`；`ApiDbTableRow.type` 是可选字段，缺失归 `Tables`；折叠状态走组件自带的 load/save；happy-dom 无布局，别依赖 `offsetWidth` |
| 3 | **M3** 多库目录（唯一动后端） | 白名单校验**先于**拼 SQL；写路径（`/edits`、`/ddl`、`/import`）一行不动；既有 `/tables`、`/data` 的测试断言**一条都不许改**；`list_databases` 用**带默认实现**的 trait 方法，别逼所有实现者跟着改 |
| 4 | **M4** 工具条 + 状态条 | 规则 4 的"非图标 `.btn` ≤ 1"写成用例，是硬门禁；状态条与既有 `.db-bar`（提交条）是**两条**，职责不许混 |
| 5 | **M5** 文档 + ADR-026 + ADR-027 | 旧的"已实施"描述一条不留 |

每阶段固定动作：

```
改 panel/src → 加/改用例 → npm run check → npm run build
→ touch crates/swiss-panel/src/lib.rs → release 构建 → 重启 19998
→ 真浏览器走查（spec 里该阶段的清单，1440 与 900 两宽，明暗两套）→ 提交
```

改了可见文案的阶段，走查要走**第二遍中文**：点 `文/A`，确认 `document.documentElement.lang === "zh-CN"`，
把新字读一遍（docs/38）。

## 门禁（每次提交前）

```bash
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + eslint + emit 新鲜度 + vitest

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

## 四个会让你白干的陷阱

1. **rust_embed 的指纹不含资产内容。** 改了 `admin_assets` **不会**触发 swiss-panel 重编译——release
   构建会说 "Finished"，而二进制里 embed 的还是旧面板。每次重建 19998 前先
   `touch crates/swiss-panel/src/lib.rs`，然后**验服务出来的字节**（curl 下来 grep 新符号），
   不是只看构建状态。
2. **vitest 抓不到 link-time 的导入错误。** `import { closeSheet } from "../util.js"`（而 export 其实在
   `add-sheet.js`）能过全部测试，却在浏览器里让整个模块死掉（ES module 拒绝 link），页面一片空白。
   所以每阶段必须真浏览器走查。
3. **合成 `.click()` 不算走查。** 它直接调 handler，绕过坐标命中、遮挡和焦点。用真 CDP 指针事件
   （`agent-browser`，先 `agent-browser skills get core`）。判断可见性用 `hidden === false` + 计算
   `display` 或 bounding rect——`position: fixed` 会让 `offsetParent` 为 null，别拿它当可见性探针。
4. **两条本机踩过的浏览器坑**：`wait --load networkidle` 在这个面板上会挂死（用 `wait --text` 或
   `wait <毫秒>`）；页面里 `confirm()` 弹出时 `eval` 会阻塞（先 `agent-browser dialog accept`）。
   M1 的批量关闭和 M3 的切库都会弹 `confirm`，走查这两步时留意。

## 实例纪律

19998 是隔离测试实例（`scripts/test-instance.ps1 -Stop` / `-Fresh`，`CARGO_TARGET_DIR=target-test`，
令牌 `MCP_GATEWAY_TOKEN=acceptance-token-for-1998`）。
**19999 是 owner 正在用的生产实例，一次都不许碰**——唯一允许的是只读 `GET /health`。
停实例**按端口的属主 PID 停**（`scripts/test-instance.ps1 -Stop`），**绝不按进程名**：
`Get-Process swiss` 会把 owner 的 19999 一起杀掉。19998 若被别的会话占着，换 19997（同脚本、独立测试 home）。

本机可用的连接：`mysql`（库 `acme_app_dev`，1,7xx 张表）、`shop`（PostgreSQL）、`redis`、`shop-redis`。
**本机没有任何带外键的表**（shop 报 `foreignKeys: 0`），所以 FK 相关的走查项照实写"未验证 + 原因"。

## 提交与报告

- 提交信息照仓库现行风格：一句小写祈使句 + `(docs/43 M<N>)` 后缀，正文说清**为什么**，
  结尾带 `Co-Authored-By:` 行。
- 代码注释与 UI 文案**全部英文**；中文只出现在 `docs/43-*.md` 与 `zh.ts` 的文案值里。
- **诚实报告**：走不通的流程（缺凭据、要动 owner 的真数据、外部端点不可达）列为**未验证**并写明原因，
  绝不打勾。"做完了"却是一个死页面，代价是 owner 的信任和他替你当测试员的时间。
- 每阶段提交后简报：改了什么、用例增减、19998 上真点了哪些、哪些没验证及为什么。
