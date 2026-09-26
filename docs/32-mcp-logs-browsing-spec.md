# 32 — MCP Logs 分页与浏览连续性 — spec

**状态：已实施（B1–B4，2026-09-17 同日落地；起草时为 Draft for operator review），基线 `e50525b`。** 由 swiss-spec
会话产出；实施会话从 `docs/32-mcp-logs-browsing-prompt.md` 起跑。本文只定行为、测试与
交付顺序，不含实现。

> 需求原文（操作者，2026-09-17）：
> 「我这个 MCP 的 logs 分页很奇怪，点了下一页，然后跳到最上面，又得往下拉，点击。你看看，
> 整体的 UI ，要怎么优化才行呢？ /swiss-spec  我建议写个 Spec 文档」
>
> 首轮追问覆盖翻页模型、实时刷新、Traffic 范围与 Clear 位置；等待窗口内未收到追加确认，本文先按
> 推荐默认值落稿：**有界分页 + 原位锚定、旧页暂停轮询、只改单个 MCP 的 Logs、Clear logs 进
> `⋯` 且二次确认**。操作者后续答复优先于这些默认值，实施前须据此修订本文。

## 0. 现状与缺口（证据先行）

这不是浏览器偶发行为，而是现在的两阶段重绘必然产生的结果：

1. `#pane` 才是滚动容器（`index.html:134`；`styles/views.css:18` 的
   `overflow-y: auto`），`#tabbody` 本身不滚动。
2. Logs 的分页器在 20 条 call row **之后**（`js/logs.js:90-94`）。操作者点 `Older`
   时通常已在 pane 底部。
3. `callsPageStep` 先提交新页码，再把 `d.calls` 设为 `null`
   （`js/detail.js:222-230`）；`logsBody(null)` 只剩一行 `Loading calls…`
   （`js/logs.js:72-73`）。
4. `renderCallsOnly` 用 `innerHTML` 把整块换短（`js/run-history.js:722-750`）。pane 的
   scrollHeight 瞬间缩小，浏览器把 scrollTop 夹回接近 0；响应回来后内容重新长高，但代码只恢复
   搜索框节点、焦点与光标，不恢复滚动锚点。

现有注释已经承认同类风险：6 秒 poll 若无变化仍重绘，会“scroll an open result back to the top”
（`run-history.js:729-731`）。Data 页已有“重绘前记住 scrollTop、重绘后恢复”的先例
（`data-sql.js:289-299`）及回归测试（`admin-data-loaders.test.ts:251-277`），Logs 没有。

还有三个相邻缺口应在同一行为边界收口：

| 缺口 | 当前行为 | 用户后果 |
| --- | --- | --- |
| 失败不是事务 | 页码先改；非 2xx / 网络失败无可见错误 | 可能永久停在 `Loading calls…`，也不知道 Retry 在哪 |
| 旧页仍每 6 秒重取 | `views/mcps.js::poll` 对任意 Logs 页调用 `loadCalls` | offset 页会被新调用向后推，阅读中的历史页可能重复、漏看或移动 |
| Clear 是显式立即动作 | `#callsClear` 直接 DELETE，无确认 | 一次误触同时删索引与仍保留的 full reply 文件；不符合 Swiss 的破坏性操作规则 |

当前测试只覆盖搜索框与空态的纯 HTML（`admin-logs-search.test.ts`）；没有真实翻页状态、失败回滚、
滚动、焦点、轮询或 Clear 确认。Vitest 的 Node 环境没有布局引擎，因此 scroll clamping 最终必须在
19998 的真实浏览器里用真指针点击证明。

## 1. 宪法核对与设计结论

### 1.1 四个产品属性

- **Ruthlessly small**：继续每页 20 条、DOM 最多一页；不做 infinite scroll、虚拟列表或新依赖。
  pending 期间复用当前 20 条节点，不另缓存历史页。
- **Plugin-shaped**：行为留在 MCP 自己的 panel 模块；不把日志业务或分页状态搬进 shell / host。
- **Hot-pluggable**：不新增 task、timer 或跨插件状态；MCP 页卸载后仍由现有 page lifecycle 停止参与。
- **Three ways in**：proc / http / compiled adapter 都经过同一 call log，本改动不区分接入方式。

四项均不被牺牲，可以继续。

### 1.2 载重规则与边界

- `calls.rs` 的双层落盘、180 天/字节留存、2 KiB preview、最新 50 个 full body 均不动。
- `GET /api/mcps/{name}/calls?page=N&q=TEXT`、`more` 与默认 pageSize=20 均不动；不新增
  total、cursor、snapshot 或 response key。
- 不动密封格式、配置、认证、转发路径与 secret masking；docs/05 无 wire/disk 影响。
- 不新增 Cargo/npm 运行时依赖，不引入 framework、bundler 或通用 Pager 抽象。
- Logs 仍是 MCP resource 内的 L3 tab；不升格成 L2 页，不改 App Shell、Context Bar 或 sidebar。

### 1.3 一句话方案

**页切换改成事务：旧页在 pending 期间留在原位并明确标忙；成功才同时提交页码与新 rows，失败仍留在
原页；每次替换以分页器为视觉锚点补偿 pane.scrollTop，因此用户点完 Older 后，按钮还在鼠标附近。**

不采用 sticky toolbar：问题可以在原层级、原布局内解决，永久占据 viewport 的新 chrome 没有必要。
不采用“Load older”：它会让 DOM 随浏览增长，和本产品的内存取向相反。

## 2. B1 — 翻页成为事务，不再先清空页面

### 2.1 状态合同

在 detail state 增加最小状态（名字可在实现时等价调整，但语义不能合并掉）：

- `callsPendingPage: number | null`：正在请求但尚未提交的目标页；
- `callsError: string`：最近一次前台 load 的可见错误；
- `callsRequest: number`：单调 request generation，防止旧响应覆盖新 MCP / 新 q / 新页。

`callsPage` 只代表**屏幕上已经提交的页**。`callsPageStep(delta)` 计算 target 后：

1. 不改 `callsPage`，不把 `calls` 设为 null；
2. 设置 pending，给当前 log region 加 `aria-busy="true"`；
3. Newer/Older 都 disabled，Page 文案变为 `Page 4 · Loading…`（数字仍是当前可见页）；
4. 请求 target；成功且 generation/detail/q 仍匹配时，一次提交 target + calls + more + stderr；
5. 失败时清 pending，保留当前页、展开状态与滚动，显示 `Could not load calls.` + `Retry`。

第一次打开 Logs 尚无可显示页时，现有 `Loading calls…` 空壳可以保留；**只有已经有 rows 的前台切页
不得把内容压成空壳**。pending paint 必须原位 patch 当前 pager/status，不能替换整个 `#tabbody`；现有
call nodes 与 live focus 不得在请求发出时被 detach。旧 rows 留在 pending 状态并不假装它们是新页：
`aria-busy`、disabled pager 与可见 spinner 同时说明“正在换页”。

搜索仍 300 ms debounce、Escape 清空、重置目标页为 0；同一 request generation 规则保证慢的旧 needle
不能覆盖新的 needle。搜索框 live node、focus、caret 与 IME 保留合同（docs/31）不得回退。

### 2.2 测试先行与验收

新建 `crates/swiss-panel/panel-tests/test/admin-logs-pagination.test.ts`，先写红：

- 点击 Older 后，当前 call rows 仍存在，`callsPage` 仍是旧值，pending target 正确；
- 请求 URL 使用 target page 与当前 q，pending 时两方向按钮 disabled，region 是 `aria-busy=true`；
- 成功才提交 page/rows/more；失败不提交、旧 rows 仍在并出现 Retry；
- Retry 请求同一 target，成功后错误消失；
- 两个响应乱序时，旧 generation 不得覆盖新的 q/detail/page；
- 首次进入无 rows 时仍显示 `Loading calls…`，避免“空页像成功”的歧义。

测试 seam 驱动真实的 `callsPageStep → loadCalls → renderCallsOnly`，不能只对 `logsBody` 字符串做断言。

## 3. B2 — 分页器是滚动锚点，焦点跟着动作走

### 3.1 锚定算法

前台页切换发出前记录：

- pane；
- 当前 pager 的 viewport top；
- 触发方向（`newer` / `older`）与按钮是否持有 focus。

成功重绘并重新 wiring 后，若新页仍有 pager：

`pane.scrollTop += newPagerTop - oldPagerTop`

目标不是保存一个可能已失效的绝对 scrollTop，而是让分页器在屏幕上的位置不动。新页正常仍是 20 条时，
用户可连续点 Older；最后一页不足 20 条时，浏览器允许的最大 scrollTop 仍是上限，但分页器必须留在
可见区，不能跳到 pane 顶部。

如果动作由键盘触发，重绘后把 focus 放回同方向按钮；该方向在边界页 disabled 时，focus 放到另一个
可用方向，两个都不可用才放到 pager 的 status。不得调用全页 `scrollIntoView`，那会拖动 App Shell。

后台 poll 与搜索不冒充“翻页动作”，不使用 pager anchor：

- 搜索继续以 live input 为 focus anchor；
- page 0 的 poll 沿用“签名不变不重绘”；确有新 rows 时不得把 pane 主动设为 0。

### 3.2 测试先行与验收

同一 vitest 文件增加可控 geometry seam（参考 Data 的 scroll regression）：

- 模拟旧/new pager rect，断言 scrollTop 只补偿二者差值；
- 最后一页高度变短时，pager 仍在 viewport 可见范围；
- 鼠标翻页不抢到搜索框，键盘翻页后 focus 回到等价按钮；
- 搜索 repaint 仍保住 input node/caret；展开行 state 不因翻页以外的 poll 被清。

真实浏览器是最终判定：在至少 4 页 call 的 MCP 上滚到底部，用 CDP 真指针连续点 Older 三次，每次
按钮在 viewport 内、pane 不回顶部；再连续点 Newer。用 `element.click()` 或只看 DOM 存在不算证据。

## 4. B3 — 历史页稳定，错误诚实

### 4.1 轮询

- `callsPage === 0`、无 pending 时，保持现有 6 秒 poll；这是 newest page 的实时语义。
- `callsPage > 0` 时不 poll calls（MCP 列表本身仍照常 poll）；历史页只由 Newer/Older/Retry 的显式
  动作更新。这样 offset page 至少在阅读期间不自行漂移。
- 回到 page 0 后立即取一次最新数据，再恢复 6 秒节奏。
- 搜索中的 page 0 同样可 poll；request generation 负责丢弃过时响应，不允许 `callsLoading` 的简单
  early-return 把一次输入永久留在 loading。

本 item 不把 offset API 改成 cursor：那是另一份后端一致性设计，且不是本次“点击后跳顶”的必要条件。

### 4.2 错误与空态

- 前台失败：内联 `Could not load calls.` + `Retry`，不 toast-only；失败原因可安全显示时放在
  次级文本，但不得把 response body 原样灌进 UI。
- page 0 初次加载失败：同一错误块替代无限 spinner，并给 Retry。
- filtered empty / no calls yet / past-end page 的三种既有文案保持区分。
- stderr section 与 full-result fetch 的现有行为不变；page 错误不能抹掉已显示 stderr。

### 4.3 测试先行与验收

- MCP poll 在 page 0 发 calls 请求，在 page > 0 不发；回到 0 立即请求；
- search/poll/page 请求竞争时只有最新 generation 可提交；
- HTTP 非 2xx 与 fetch reject 都落到可见错误，不出现永久 spinner；
- 错误 Retry 可由 Enter/Space 操作并只发一次请求。

## 5. B4 — 工具栏、Clear logs、响应式与无障碍

### 5.1 工具栏

保留 `Tool calls · newest first`、现有搜索框和底部分页器；不复制第二套 pager，也不把 toolbar sticky。
顶部显式 `Clear` 改为现有 sprite 的 ellipsis icon button：

- `aria-label="More log actions"`；
- 用 `popupMenu`，菜单最后一项 `Clear logs…`，`danger: true`；
- 点击后沿用 house `confirm()`，逐字文案：
  `Clear all recorded tool calls for “<name>”? This removes the call history and stored full replies. The MCP configuration is not changed.`
- 确认后才 DELETE；成功回 page 0、清 rows/open/full/error/pending，toast `Call log cleared`；取消零请求。

只有这一项菜单时仍使用 `⋯`：破坏性操作不应为了省一次点击长期占据日志过滤栏。

### 5.2 布局与语义

- 继续标准 `--measure`；切 Config ↔ Logs 不得横向位移，不恢复旧的 `--measure-wide` 特例。
- `pager` 用 `role="navigation" aria-label="Call log pages"`；页码/Loading/Error 状态进入
  `aria-live="polite"`，不得每 6 秒无变化播报。
- 900px 宽度下 sec-head 可换行：caption 与 `⋯` 留在首行，搜索框占可用宽度；不得产生 pane 横滚。
- light/dark 都只用现有 tokens；不加颜色 literal、阴影、pill、Unicode icon 或新 sprite。
- `Newer` 永远朝 page 0，`Older` 永远朝更早记录；copy 与 Traffic 的既有方向一致。

### 5.3 测试先行与验收

- markup 测试钉住 menu opener 的 aria-label、pager nav/status 语义，不再期待 `#callsClear`；
- menu 测试：Clear logs 是 danger/末项，Cancel 不 DELETE，Confirm 才 DELETE，成功状态完整重置；
- 900px 的真实浏览器不横滚，搜索、菜单、Newer/Older 都能由真点击与键盘操作；
- light/dark 各截图一张，展开一条 call 后 payload 不溢出标准 measure。

## 6. 交付顺序与提交边界

依赖顺序固定，每项一提交；下一项只建立在前一项已绿的行为上：

1. **B1** — 新 vitest 红灯 → transactional page/request state → 失败回滚；
2. **B2** — geometry/focus 红灯 → pager anchor 与 focus restoration；
3. **B3** — poll/race/error 红灯 → old-page pause 与 honest retry；
4. **B4** — menu/a11y/responsive 红灯 → toolbar/Clear/polish。

不得把四项揉成一次“大 UI cleanup”；每个提交信息写明对应 B-item。实现偏离本文的命名可以，偏离行为、
英文 copy 或测试 seam 必须先修 spec。

每项 panel gate：

```powershell
cd crates/swiss-panel/panel-tests
npm test
node --check ../src/admin_assets/js/logs.js
node --check ../src/admin_assets/js/detail.js
node --check ../src/admin_assets/js/run-history.js
```

最终仓库 gates（`--workspace` 不可省）：

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

真实浏览器验证严格走 19998：先停测试实例，touch `crates/swiss-panel/src/lib.rs` 让 rust_embed 重新
收进资产，以 `CARGO_TARGET_DIR=target-test` rebuild，再 `scripts/test-instance.ps1 -Fresh`。19999
全程不动；用户没有要求部署。

## 7. 明确不做

- 不做 infinite scroll、virtual list、Load more append、页大小选择、页码直跳、总页数或日期跳转；
- 不改 calls/traffic 的 API、磁盘文件、retention、preview/full body 策略或搜索匹配面；
- 不改 Traffic、Jobs run history、Data activity 或通用 `.pager` 的其他使用者；
- 不重排 call row 的字段，不新增列、筛选器、排序模式或导出；
- 不改 MCP L1/L2/L3 导航、standard measure、Focus Mode 或 sidebar；
- 不部署 19999。

## 8. ADR 判定

不新增 ADR。该决定是 panel-local、可逆、不改 wire/disk，也没有一个“看起来反常但必须长期解释”的
架构 trade-off；在 docs/32 记录根因、交互合同和“不做 infinite scroll”的产品取舍已经足够。
