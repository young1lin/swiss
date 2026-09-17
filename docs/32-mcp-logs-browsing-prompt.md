# 32 — MCP Logs 分页与浏览连续性 — 实施交接提示词

工作目录：`<repo>`（Windows，Cargo workspace）。你是
**全新实施会话**，不带写 spec 会话的上下文；本文件和 docs/32 是任务事实源。不要重新发明交互，也不要
部署 19999。

## 先读（按顺序）

1. `AGENTS.md` 与 `.agents/rules/panel-proof-of-life.md`：19998/19999、LF、panel test、
   rust_embed 与真浏览器纪律高于一切。
2. 加载 `swiss-debug` 与 `swiss-ui-design` skill；根因已在 spec §0 给出，但仍先建立会红的
   scroll/page loop。完成代码后依次加载并执行 `swiss-verify`、`swiss-live-verify`、`swiss-review`。
3. `docs/32-mcp-logs-browsing-spec.md`：B1–B4 的行为、英文 copy、验收与提交边界。
4. `docs/31-mcp-logs-search-spec.md`：搜索 q、300 ms debounce、Escape、focus/caret/IME 合同，不得回退。
5. `crates/swiss-panel/src/admin_assets/js/logs.js`、`detail.js:149-269`、
   `run-history.js:580-619,722-750`、`views/mcps.js`、`pane.js:117-151`。
6. `crates/swiss-panel/src/admin_assets/styles/views.css:17-28,177-205`、
   `styles/base.css` 的 token/controls；只复用现有视觉词汇。
7. `crates/swiss-panel/panel-tests/test/admin-logs-search.test.ts`，以及 scroll restore 先例
   `admin-data-loaders.test.ts:251-277` / `js/data-sql.js:289-299`。
8. 菜单先例：`js/menu.js::popupMenu` 与任一现有 danger menu。不要造第二套 menu。

开始前跑 `git status --short`；工作树若有用户改动，保留并避开，不得 reset/checkout 覆盖。

## 任务摘要（不得扩 scope）

修复单个 MCP detail 的 **Logs L3 tab**：现在点击底部 Newer/Older 会先把 rows 换成一行 Loading，
使滚动容器 `#pane` 的高度塌缩并把用户夹回顶部。

固定方案：

- 继续 20 条有界 offset page；不改 Rust API、calls.rs、disk/wire、retention 或 search 语义；
- 切页是事务：pending 时原位 patch pager/status（不替换 `#tabbody`），旧 rows 与 live focus 留在原位且明确 busy；成功才提交 page+rows，失败留原页并 Retry；
- 以 pager viewport top 做 scroll anchor，重绘后补偿 pane.scrollTop；键盘 focus 回到等价按钮；
- 只在 page 0 做 6 秒 calls poll，旧页阅读期间不自行漂移；request generation 丢弃乱序响应；
- 顶部 Clear 改成 `⋯` → danger `Clear logs…` → spec 指定的 confirm；
- 不做 sticky toolbar、第二个 pager、Load older、infinite scroll、Traffic/Jobs/Data 改造或通用组件抽象。

如果实现发现必须改后端 response 或引入 cursor，**停止并先修 spec**；不能暗中越界。

## 测试先行与交付顺序

严格一项一提交，测试先红、实现后绿：

1. **B1**：新建 `admin-logs-pagination.test.ts`，覆盖 pending 不清 rows、成功原子提交、失败回滚/
   Retry、乱序 response；再改 state/load/wiring。
2. **B2**：先加 geometry/focus 红灯，再实现 pager anchor 与 focus restore；不能用全页
   `scrollIntoView`。
3. **B3**：先加 page0/old-page poll、race、HTTP/fetch error 红灯，再改 poll/error path。
4. **B4**：先改/加 menu、confirm、aria、900px 合同测试，再做最小 markup/CSS。

每项提交信息写 B-item；不要攒成一次“大 UI cleanup”。代码注释一律英文。不要新增依赖、framework、
bundler、Unicode icon、literal color 或新 sprite；ellipsis 已存在。

## 必须保住的现有合同

- Newest first；Newer 朝 page 0，Older 朝更早记录。
- 搜索是服务端 q、300 ms debounce、Escape 清空、回 page 0；重绘保留同一个 live input、focus、caret、IME。
- 展开 row 与 full result 行为不变；stderr 独立 section 不被 page error 抹掉。
- Logs 与其他 MCP tabs 共用标准 `--measure`，切 tab 不横向跳。
- 初次无 rows 可以显示 `Loading calls…`；已有 rows 的 page transition 不能塌成它。
- copy 逐字以 spec 为准：`Could not load calls.`、`Retry`、`Clear logs…`、confirm 与 toast。

## Gates

每个 B-item 至少跑：

```powershell
cd crates/swiss-panel/panel-tests
npm test
node --check ../src/admin_assets/js/logs.js
node --check ../src/admin_assets/js/detail.js
node --check ../src/admin_assets/js/run-history.js
```

最终必须跑（`--workspace` 不可省）：

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d
```

没有 Cargo/CSS 后端改动也不能用“只是 JS”跳过最终仓库 gates。`cargo tree -d` 记录结果；本任务不应
产生任何 dependency diff。

## 19998 真浏览器 proof of life

vitest 没有布局引擎，scroll bug 只有真实浏览器能判。最终 panel assets 改完后：

```powershell
scripts/test-instance.ps1 -Stop
(Get-Item crates/swiss-panel/src/lib.rs).LastWriteTime = Get-Date
$env:CARGO_TARGET_DIR = "target-test"; cargo build --release
scripts/test-instance.ps1 -Fresh
```

然后从全新页面导航：MCP → Servers → 一个至少 4 页 calls 的 MCP → Logs。全部用 CDP/agent-browser 的
**真指针事件**，不能 `element.click()`：

1. 滚到底部，连续 Older 三次；每次 pager 仍在 viewport、pane 不到顶部、Page 与 rows 对应成功响应；
2. 连续 Newer 回去；键盘 Enter/Space 翻一页并检查 focus；
3. 输入搜索、等待结果、Escape；确认 focus/caret 不丢；展开 row 后等一次 6 秒 poll，展开状态不被误关；
4. 在旧页等超过 6 秒，确认没有 calls page request/rows 漂移；回 page 0 立即刷新；
5. 打开 `⋯`：Cancel 不发 DELETE；在**19998 test home**确认 Clear logs 后才发 DELETE，UI 回干净 page 0；
6. light/dark 各走一次；窗口再缩到约 900px，搜索/menu/pager 可操作且 pane 无横向 overflow；
7. 检查 console 无 module/link/runtime error，network 请求参数与次数符合 spec。

Retry 的真实错误路径若没有安全方法注入，明确报告“NOT live verified — covered by vitest”，不要伪造勾选。
截图/数值记录 pager 点击前后 `#pane.scrollTop` 和 pager bounding rect，证明的是可见状态变化，不只是 DOM
存在。

19999 是用户生产实例，全程不 stop/restart/deploy。用户没有要求上线；不要调用 `scripts/deploy.ps1`。

## 完成定义

- B1–B4 各自测试先红后绿、各自提交；
- panel tests、node checks、workspace test/clippy/tree 全绿；
- 19998 新 binary 的真实点击证明通过，light/dark/900px 走完；
- 最后经 `swiss-review` 审 diff：没有 backend/wire/disk/dependency/scope 漂移；
- 最终汇报列 commit、改动文件、gates、浏览器实证与任何 NOT verified 项。
