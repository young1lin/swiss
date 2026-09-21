# docs/42 实施提示词 — Data 的对象页签

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

worktree **已经建好**，直接进：

```
<repo>\.agents\worktrees\data
```

分支 `data-full-access`。它建于 `755eb9e`，而 master 已到 `801c834`（中间三个提交是 docs/41 的 remote
工作，与本文无关）——**第一件事是 rebase 到 master**：

```bash
# worktree 之间共享分支 ref，master 直接可解析——不要 fetch（主 checkout 正占着 master，写不动）
git rebase master
```

面板的 npm 家在 `crates/swiss-panel/panel`，`node_modules` 不进 git——**第二件事在那里 `npm ci`**，
否则 `npm run check` 直接报 `'tsc' is not recognized`。

**不要 `cd` 回主 checkout。** PowerShell 里 `cd` 混在链式命令中会打断后面每一个相对路径——构建脚本与
实例脚本各起一条命令，从 worktree 根跑。

## 任务

`docs/42-data-object-tabs-spec.md` 定义的 **T1 → T5**。一句话：Data 页面从"一次只能握住一个对象"改成
"打开的对象成为页签"——换表不再丢过滤、分页和缓冲编辑，FK 跳转开新页签而不是把源表顶掉，SQL 控制台
与 Activity 监控从开关变成页签；顺带侧栏成树（SQL 分 Tables/Views/Routines，Redis 按 `:` 折叠命名空间）、
工具条只画当前页签的控件、分页与耗时下沉到底部状态条。

App Shell（rail / context bar / focus 模式）、`/api/db` 的 12 条路由与任何响应形状、连接租借、
凭据纪律——**一个都不动**。

先双击打开 `docs/assets/42/data-layout-mockup.html`，按 `2`：那就是目标（Study B）。按 `1` 是今天的形状。
Engine 下拉切 MySQL / PostgreSQL / Redis，State 下拉覆盖 30 种状态——实施时逐个对照。

## 先读这些（按顺序，别跳）

1. `docs/42-data-object-tabs-spec.md` —— **全文**。§1.4 是决定清单，§1.5 是不许动的清单，
   §2 是状态切分表（逐字段，本文的心脏），§3–§7 是每阶段的做法、用例与 19998 走查项，§8 是门禁与顺序。
2. `.agents/rules/panel-proof-of-life.md` —— 全文，逐条。**每个阶段都要在 19998 上真点**。
   vitest 绿 + `node --check` 是入场券，不是证明。
3. `.claude/skills/swiss-ui-design/SKILL.md` —— §1–§8（四层与归属，本文一层都不新增，理由在 spec §1.2）、
   §16 规则 3 / 4 / 5 / 11 / 16 / 18 / 19、§13 清单。T5 要改它的 §7。
4. `crates/swiss-panel/panel/src/db-state.ts` —— **全文（155 行）**。49 个字段就是你要切的东西；
   文件头的注释解释了 docs/37 R4 为什么把它做成"永不为 null 的单记录"——那个契约保留。
5. `crates/swiss-panel/panel/src/data-view.ts` —— 全文（930 行）。`renderDbView` 的骨架、
   `dbOpenTable` :872 与 `dbFkOpen` :916（两份几乎逐行相同的拷贝，T2 合流成 `dbOpenTab`）、
   `dbPending` :57 / `dbOkToDrop` :65 / `dbDropEdits` :71、`renderDbTables`、尾部的单一 `export {}` 块。
6. `crates/swiss-panel/panel/src/data-grid.ts:455 renderDbToolbar()` —— 那个过载的 `.db-head-ctl`，T4 拆它。
7. `crates/swiss-panel/panel/src/views/data.ts` —— 40 行，`hasPendingChanges` / `canLeave` 在这里，T2 改它。
8. `crates/swiss-panel/panel/test/admin-data-state.test.ts` —— 生命周期契约 + `FakeNode` 的搭法，
   决定了你新用例怎么写。
9. `docs/22-data-parity-spec.md` —— **不用全读**，但要知道 W0–W5 三十二项功能**已经全部交付**：
   本文不是补功能，是搬位置。搬的过程中任何一项行为变了，都是 bug。
10. `docs/21-data-web-gap-analysis.md` §"不做" —— #31–#35 本文不翻案。

## 交付顺序

一个阶段一个提交，不许跳。**T1 结束时页面必须和今天看起来一模一样**——这是整个计划里最重要的一条。

| 阶段 | 内容 | 关键风险 |
| --- | --- | --- |
| **T1** | 状态切成 `DbConnState` + `DbTab[]`（判别联合），四个访问器取代 `dbView()`，131 处调用按 spec §2.2 归类，`tab`→`pane` / `sqlTab`→`resultTab` 两处改名。页签数组恒长 1，**零可见变化** | 既有 31 个测试文件（4,454 行）必须**只改访问器名**就全绿；要改**断言**说明你漏改了行为，回去查，不许改断言迁就实现。`data-browsers.ts` 里有同名局部变量，**别全仓库正则替换**。`dbView()` 删掉，不留兼容别名 |
| **T2** | `dbOpenTab` 合流、页签条 UI、开/关/切、上限 8 与淘汰、`sqlOpen` 与 `activity: boolean` 退休、守卫遍历所有页签 | 淘汰先写纯函数 `dbEvictTarget` 再接 UI；`#pane` 的委派监听器是**属性式**赋值（`pane.onclick = …`），重画不许改成 `addEventListener`（会堆叠）；**8 个全脏时是拒绝开新页签，不是静默丢编辑** |
| **T3** | 侧栏成树：SQL 分 Tables/Views/Routines（复用 `groups.ts` 的 `mountGroup(cfg, slice)`，`density: "side"`），Redis `redisNamespaceTree` 按 `:` 分段 + 单子节点路径压缩；侧栏行由两行改一行 | **复用 `groups.ts` 的 `mountGroup(cfg, slice)` + `density: "side"`，不要另造一套组件**（skill §16 规则 5）；折叠状态用它自带的 `loadCollapsed` / `saveCollapsed`；`ApiDbTableRow.type` 是可选字段，缺失归 `Tables`；happy-dom 无布局，别依赖 `offsetWidth` |
| **T4** | 工具条只画当前页签的控件、底部状态条、`DB_TABS` 六折四（Columns/Indexes/FK 并入 Structure） | 规则 4 的"非图标 `.btn` ≤ 1"写成用例，是硬门禁；状态条与既有 `.db-bar`（提交条）是**两条**，职责不许混；新文案要 `en.ts` + `zh.ts` 两份 |
| **T5** | skill §7、`.agents/docs/data.md`、`docs/21` 标注、README 一行、spec 状态头 + 数字 + 四张截图、**ADR-026**（草案在 spec §10） | 旧的"已实施"描述一条不留 |

每阶段固定动作：

```
改 panel/src → 加/改用例 → npm run check → npm run build
→ touch crates/swiss-panel/src/lib.rs → release 构建 → 重启 19998
→ 真浏览器走查（spec 里该阶段的清单，1440 与 900 两宽，明暗两套）→ 提交
```

## 门禁（每次提交前）

```bash
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + eslint + emit 新鲜度 + vitest

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

## 三个会让你白干的陷阱

1. **rust_embed 的指纹不含资产内容。** 改了 `admin_assets` **不会**触发 swiss-panel 重编译——release
   构建会说 "Finished"，而二进制里embed 的还是旧面板。每次重建 19998 前先
   `touch crates/swiss-panel/src/lib.rs`，然后**验服务出来的字节**，不是只看构建状态。
2. **vitest 抓不到 link-time 的导入错误。** `import { closeSheet } from "../util.js"`（而 export 其实在
   `add-sheet.js`）能过全部测试，却在浏览器里让整个模块死掉（ES module 拒绝 link），页面一片空白。
   所以每阶段必须真浏览器走查。
3. **合成 `.click()` 不算走查。** 它直接调 handler，绕过坐标命中、遮挡和焦点。用真 CDP 指针事件。
   判断可见性用 `hidden === false` + 计算 `display` 或 bounding rect——`position: fixed` 会让
   `offsetParent` 为 null，别拿它当可见性探针。

## 实例纪律

19998 是隔离测试实例（`scripts/test-instance.ps1 -Stop` / `-Fresh`，`CARGO_TARGET_DIR=target-test`）。
**19999 是 owner 的，一次都不许碰。** 19998 若被别的会话占着，换 19997（同脚本、独立测试 home）。

## 提交与报告

- 提交信息照仓库现行风格：一句小写祈使句 + `(docs/42 T<N>)` 后缀，正文说清**为什么**。
- 代码注释与 UI 文案**全部英文**；中文只出现在 `docs/42-*.md` 与 `zh.ts` 的文案值里。
- **诚实报告**：走不通的流程（缺凭据、外部端点不可达）列为**未验证**并写明原因，绝不打勾。
  "做完了"却是一个死页面，代价是 owner 的信任和他替你当测试员的时间。
- 每阶段提交后简报：改了什么、用例增减、19998 上真点了哪些、哪些没验证及为什么。
