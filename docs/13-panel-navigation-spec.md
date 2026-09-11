# 13 — 面板导航二级化实施规范

> 状态：**已实施**。完成基线 `ce409be`（Rust 侧 N4；Node 侧 N1–N2 为 `4c2c964`、`9021853`，N3 为 `f88d7e2`）。原设计基线 `394bd44`。
> 前置阅读：`AGENTS.md`（它的规则高于本文任何便利）、`docs/09-toolbox-plugin-architecture.md` §6
> （页面契约）、`docs/07-decisions.md` ADR-009（面板只读）与 ADR-010（八个 crate）。
> **本仓库的 `crates/lmg-panel/src/admin_assets/` 一个字节都不能改。** 面板改动先落在
> `../local-mcp-gateway/src/admin/`，跑完 Node 侧的 vitest，再整目录复制回来。
> 新写的代码注释一律英文；文档散文中文。

## 0. 怎么用这份文档

分成 N1–N5 五个阶段，**一个阶段一个提交**，每个阶段独立可验收、独立可回退。行为变化先有测试：
改动前失败、改动后通过。

阶段顺序不能换：N1 是纯函数与它的单元测试，N2 才让 DOM 用上它，N3 才把树复制回 Rust 仓库。反过来
做的话，字节比对测试会在一个还没定型的树上失败十几次，而失败的原因每次都不一样。

两个仓库的门禁分别是：

```bash
# ../local-mcp-gateway （面板的事实来源）
npx vitest run test/admin-pages.test.ts test/admin-panel.test.ts
npx vitest run                                    # 全量，别只跑改动的那两个

# 本仓库（复制回来之后）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 1. 现状

面板顶栏只有一行 tab，由 `js/page-registry.js` 的 `paintNavigation()` 渲染进 `index.html` 的
`<div class="seg" id="viewSeg" role="tablist">`：

```
MCPs | Traffic | Tunnels | Data | Jobs | Terminal | Plugins
```

这一行是把 `/api/plugins` 的 `pages[]` 按 `order` 排序后平铺的结果。当前的贡献是（`src/builtin.rs`
与 `src/plugins/terminal.rs`，docs/14）：

| pluginId | 插件 label | page id | page label | order | sidebar |
| --- | --- | --- | --- | --- | --- |
| `mcp` | MCPs | `mcps` | MCPs | 10 | ✅ |
| `mcp` | MCPs | `traffic` | Traffic | 20 | |
| `tunnels` | Tunnels | `tunnels` | Tunnels | 30 | |
| `data` | Data | `data` | Data | 40 | |
| `jobs` | Jobs | `jobs` | Jobs | 50 | |
| `terminal` | Terminal | `terminal` | Terminal | 70 | |
| （无，前端合成） | — | `plugins` | Plugins | 1000 | |
| `process` | Process | —（不贡献页面） | | | |

**问题不是「太长」，是层级信息在渲染时被丢掉了。** `pages[].pluginId` 已经说清楚 Traffic 和 MCPs
属于同一个插件，而这一行把它们并列成两个同级入口。结果有三处代价：

1. Traffic 读起来像是与 Tunnels、Data 平级的一个子系统，而它其实是 MCP 的一个视图 —— 它的
   `countText()` 甚至就是 MCP 的计数（`views/traffic.js` 与 `views/mcps.js` 都返回 `mcpChipText()`）。
2. 一个插件想贡献第二个页面，就得在这一行里再挤一格。今天七格，装到十二格就没法看了 —— 而插件
   宿主存在的意义正是「加插件是加一个工厂加一行 register」（docs/09 §9），导航不该是那个瓶颈。
3. 顶栏的宽度被 tab 行吃掉，右侧 `memChip` / 刷新 / 主题 / Token 在窄窗口下先被挤走。

## 2. 目标形态

两级：**一级 = 插件，二级 = 该插件贡献的页面**。

```
┌──────────────────────────────────────────────────────────────────────────┐
│ MCP Gateway   [ MCP | Tunnels | Data | Jobs | HTTP | Plugins ]  … mem …  │  ← 顶栏，一级
├──────────────────────────────────────────────────────────────────────────┤
│ [ Servers | Traffic ]                                                    │  ← 二级，仅当 ≥2 页时出现
├───────────────┬──────────────────────────────────────────────────────────┤
│  sidebar      │  pane                                                    │
```

今天只有 `mcp` 一个插件贡献两个页面，所以**这次改动之后，二级栏只在 MCP 组出现**，其余六格与现在
一模一样，没有多余的空条、没有布局跳动。这正是要的：结构先立起来，容量后面自然有。

用户可见的差异只有三处：

- 一级第一格从 `MCPs` 变成 `MCP`（插件名，不是页面名）；
- 选中 MCP 组时多出一行 `Servers | Traffic`；
- `Traffic` 不再出现在一级行。

`#mcps`、`#traffic` 这些 hash 路由**一个都不变**（见 D3）。

## 3. 设计决定

### D1 — 分组的数据来源已经全在 `/api/plugins` 里，后端不加字段

`inventory.pages[]` 每条都带 `pluginId`，`inventory.plugins[]` 每行都带 `label`。分组要的两样东西
都有了。**不要**新增 `groupId`、`parentId`、`section` 之类的字段：那是把一个纯渲染问题倒灌进
`PageDescriptor`，而 `PageDescriptor` 是插件契约的一部分，改它要所有插件跟着改。

一级顺序 = 组内 `min(page.order)`。这样一个插件不能靠「补一个 order 很小的次要页面」把自己顶到前面，
也不需要再发明一个 `groupOrder`。用当前数据算出来是 `mcp`(10) → `tunnels`(30) → `data`(40) →
`jobs`(50) → `terminal`(70) → `host`(1000)，与今天的一行顺序完全一致。

### D2 — 分组是纯函数，住在 `page-core.js`

`page-core.js` 是面板里唯一有 Node 侧单元测试的模块（`test/admin-pages.test.ts`，无 DOM）。分组逻辑
放进去，就自动获得「改动前失败、改动后通过」的能力；放进 `page-registry.js` 的 `paintNavigation()`
里就只能靠肉眼。

```js
/* Group page descriptors by the plugin that contributed them. Pure data in, pure data out:
   the shell renders the result, so this stays unit-testable without a DOM.
   - group order is the SMALLEST page order in the group, so a plugin cannot jump the row by
     contributing one late page, and no groupOrder field has to exist;
   - inside a group, page order decides, and replace() order breaks ties;
   - a page whose pluginId has no inventory row (the client-side "plugins" page, pluginId
     "host") still gets a group: fallback label first, then the page's own label. Never drop
     a page because its plugin row is missing - a page that cannot be reached is worse than
     a group with an ugly name. */
function groupPages(pages, plugins, fallbackLabels) {
  var byId = new Map((plugins || []).map(function (p) { return [p.id, p]; }));
  var groups = new Map();
  pages.forEach(function (page) {
    var gid = page.pluginId || page.id;
    var group = groups.get(gid);
    if (!group) {
      var known = byId.get(gid);
      var label = (known && known.label) || (fallbackLabels && fallbackLabels[gid]) || page.label;
      group = { id: gid, label: label, order: page.order, pages: [] };
      groups.set(gid, group);
    }
    if (page.order < group.order) group.order = page.order;
    group.pages.push(page);
  });
  return Array.from(groups.values())
    .map(function (g) { g.pages.sort(function (a, b) { return a.order - b.order; }); return g; })
    .sort(function (a, b) { return a.order - b.order; });
}
```

导出它，并在 `createPageRegistry` 的返回对象上加一个 `groups(plugins, fallbackLabels)`，让
`page-registry.js` 不必自己再取一次 `list()`。

### D3 — 页面 id 与 hash 路由一个都不改

二级化是**渲染**的变化，不是路由的变化。`#traffic` 保持 `#traffic`，不要变成 `#mcp/traffic`。

理由不是省事，是三处已有的耦合会一起断掉：

- `js/polling.js` 的 `setView(v)` 直接 `navigatePage(v)`，参数是页面 id；
- `js/page-registry.js` 的 hashchange 分支用 `registry.get(id)` 判定；
- 用户和文档里存下来的 `#traffic` 书签。

而分层路由换来的是什么？没有东西。同一个 id 空间已经是扁平且唯一的（`page-core.js` 的
`replace()` 明确拒绝重复 id）。

### D4 — 一级按钮必须同时带 `data-group` 和 `data-view`

`js/jobs.js:35` 有一行藏得很深的耦合：

```js
var b = document.querySelector('#viewSeg [data-view="jobs"]');
if (b) b.hidden = true;   // 探测到 /api/jobs 不可用时，把 Jobs 这一格藏掉
```

如果一级按钮只带 `data-group="jobs"`，这个选择器会静默失配 —— 不报错，只是那一格再也藏不掉，
于是点进去得到一个 503 的空页面。所以一级按钮的属性是：

```html
<button role="tab" data-group="mcp" data-view="mcps" aria-selected="true">MCP</button>
```

`data-view` 取该组的**默认页面**，即排序后的第一个（`group.pages[0].id`）。点一级 = 进默认页；
`jobs.js` 的选择器继续命中；`#viewSeg` 的 click 委托也不用改判定条件。

单页面的组（Tunnels / Data / Jobs / HTTP / Plugins）里，`data-group` 与 `data-view` 各自对应组 id
与唯一页面 id，行为与今天完全相同。

### D5 — 二级栏在只有一个页面时整条不渲染

不是渲染一条只有一格的 seg，也不是渲染一条空的：**整个容器 `hidden`**。`base.css` 已有
`[hidden]` 的处理，`jobs.js` 就是靠它藏 tab 的。

判定写死一条：`current.pages.length < 2 → sub.hidden = true`。别加「配置项」或「总是显示以免跳动」的
折中 —— 六个组里五个是单页，常显就是常年一条空条。

### D6 — 二级栏是 `.shell` 之上的一条，不是 pane 里的一条

三个候选位置，取第二个：

| | 位置 | 为什么不 |
| --- | --- | --- |
| A | 顶栏内，一级右边再放一段 seg | 顶栏已经 44px 高、七个元素争宽，两段 seg 并排在窄窗口下先挤掉 Token 按钮 |
| **B** | **顶栏与 `.shell` 之间，整宽一条** | **选它** |
| C | pane 的头部 | pane 的 innerHTML 归各个 view 模块所有（`views/*.js` 全都直接写 `$("pane").innerHTML`），shell 往里塞东西就得跟七个模块约定不许覆盖 —— 那是把契约建在别人的字符串拼接上 |

`index.html` 因此多一个兄弟节点：

```html
  <div class="subbar" id="subBar" hidden>
    <div class="seg" id="subSeg" role="tablist" aria-label="Subsection">
      <!-- The active plugin's page contributions land here; hidden when it has only one. -->
    </div>
  </div>
```

### D7 — 顺手把 `calc(100vh - 44px)` 改成 flex，而不是改成 `calc(100vh - 44px - 33px)`

`base.css:228` 现在是：

```css
.shell { display: flex; height: calc(100vh - 44px); }
```

那个 `44px` 是顶栏高度的手抄副本。加第二条栏之后再抄一个高度进去，就有了两个副本，下一次谁调
padding 谁就制造一个滚动条。**改成一次性的正确写法**：

```css
/* The shell fills whatever the bars leave. Previously this subtracted a hand-copied 44px, which
   was a second source of truth for the toolbar's height; a second bar would have made it three.
   min-height:0 is what lets the inner scrollers actually scroll inside a flex column. */
#app { display: flex; flex-direction: column; height: 100vh; }
.shell { display: flex; flex: 1; min-height: 0; }
```

`.toolbar` 的 `position: sticky; top: 0` 在 flex 列里不再起作用，但也不再需要 —— 页面本身不滚了，
滚的是 `.side-list` 和 `.pane`。保留声明无害，删掉也行，二选一并在提交信息里说一句。

`.subbar` 的样式跟着 `.toolbar` 的语汇走，不要发明新颜色：

```css
.subbar {
  display: flex; align-items: center; gap: var(--s3);
  padding: var(--s1) var(--s4);
  background: var(--sidebar); border-bottom: 1px solid var(--sep);
}
```

### D8 — Node 构建的 fallback 清单要同步长出组标签

`page-registry.js` 顶上的 `legacy` 数组是 `/api/plugins` 返回 404 时（也就是跑在 Node 网关上时）
用的清单。它有 `pluginId`，但没有插件 label，所以要一张同样写死在前端的表：

```js
/* Group labels used when the host serves no plugin inventory (an older gateway answers 404 on
   /api/plugins), plus the one group that has no inventory row at all: the management page is
   synthesized here, not contributed by a plugin. */
var GROUP_LABELS = { mcp: "MCP", tunnels: "Tunnels", data: "Data", jobs: "Jobs", host: "Gateway" };
```

注意 `host` 这一行对**两种**情况都成立：Node 上没有 inventory，Rust 上有 inventory 但里面没有
`host` 这个插件（`management` 页面是前端合成的）。`groupPages` 的 fallback 顺序（inventory label →
`GROUP_LABELS` → page label）因此两边都落在 `Gateway` 上。

## 4. Rust 侧要改的唯一东西：两个 label

后端不加字段、不改路由、不改 `/api/plugins` 的形状。改的只有两个字符串，因为一级现在显示的是插件
名，而 MCP 插件的插件名今天叫 `MCPs`（与它的页面重名）：

`src/builtin.rs:103`

```rust
label: "MCPs".into(),      // →  label: "MCP".into(),
```

`src/builtin.rs:108`

```rust
page("mcps", MCP_ID, "MCPs", 10, true),   // →  page("mcps", MCP_ID, "Servers", 10, true),
```

一级 `MCP` / 二级 `Servers | Traffic`。`Servers` 而不是 `List` / `MCPs`：这一页是被托管的 MCP 服务器
清单，`Servers` 说的是内容，`List` 说的是控件。

一级显示的是插件 `label`（终端插件两级同为 `Terminal`；此前的 `http-tools` 一级是 `HTTP Tools`、
平铺时是 `HTTP`——这正是二级化要修的差异）。**不要改插件或页面的 label 来迁就显示。**

## 5. 阶段

### N1 — `page-core.js` 的分组纯函数（Node 仓库）

- `../local-mcp-gateway/src/admin/js/page-core.js`：加 `groupPages()`（D2），导出它，并在
  `createPageRegistry` 的返回对象上加 `groups(plugins, fallbackLabels)`。
- `../local-mcp-gateway/test/admin-pages.test.ts`：加一个 `describe("page grouping")`，至少覆盖
  1. 两页同 `pluginId` 合成一组，组内按 `order` 排；
  2. 组顺序按组内最小 `order`，且「某组补一个 order=1 的页面」会把该组顶到最前（证明用的是 min）；
  3. `plugins` 里没有对应行时用 `fallbackLabels`，两者都没有时用页面自己的 label，**页面绝不丢失**；
  4. `plugins` 传 `null`（Node 网关的 404 路径）时整个函数照常工作；
  5. 空数组进、空数组出。
- 门禁：Node 仓库 `npx vitest run`。
- **不碰** `page-registry.js`、`index.html`、CSS。这一阶段结束时面板行为零变化。

### N2 — 两级渲染（Node 仓库）

- `index.html`：加 D6 的 `#subBar` / `#subSeg`。
- `styles/base.css`：D7 的 `#app` / `.shell` / `.subbar`。
- `js/page-registry.js`：
  - `GROUP_LABELS`（D8）；
  - `paintNavigation()` 改成算一次 `groups`，一级画进 `#viewSeg`（属性见 D4），找出含
    `state.view` 的组画二级进 `#subSeg`，并按 D5 决定 `#subBar.hidden`；
  - `#subSeg` 的 click 委托与 `#viewSeg` 同款（`closest("[data-view]")` → `navigatePage`）；
  - 插件不可用时的 `· off` 标记与 `title` 提示留在**页面**按钮上（现有 `unavailable(page)` 逻辑
    不动）；一级按钮在**该组所有页面都不可用**时才标 `· off`。
- 无障碍：两条都是 `role="tablist"`；一级 `aria-selected` 跟随当前组，二级跟随当前页；
  `#subSeg` 带 `aria-label="Subsection"`（`#viewSeg` 现有的是 `aria-label="Section"`）。
- 手动验收（Node 网关，`/api/plugins` 走 404 分支）：五格一级、MCP 组下两格二级、`#traffic`
  直接打开时二级栏已经在且选中 Traffic、Tunnels/Data/Jobs 无二级栏且无空条。
- 门禁：Node 仓库 `npx vitest run`（`admin-panel.test.ts` 里凡是断言 `#viewSeg` 内容的用例要跟着更新）。

### N3 — 整树复制回本仓库

```powershell
Remove-Item -Recurse -Force crates\lmg-panel\src\admin_assets
Copy-Item -Recurse ..\local-mcp-gateway\src\admin crates\lmg-panel\src\admin_assets
```

- 复制**整棵树**，不要挑文件（ADR-009）。
- 立刻跑 `cargo test -p lmg-panel`：`the_tree_is_byte_for_byte_the_node_builds` 必须通过，且必须是
  真的比对过 —— 它现在会 `assert_eq!(compared, PanelAssets::iter().count())`，比对数为 0 会失败。
- 这一提交里**只有** `admin_assets/` 的变化，不掺任何 Rust 改动。这样将来 `git log -- admin_assets`
  就是一份干净的「面板从 Node 复制过来」的历史。

### N4 — 两个 label（本仓库）

- `src/builtin.rs` 的两处（§4）。
- 加一个断言 label 契约的测试（`src/builtin.rs` 的 `#[cfg(test)]` 里）：MCP 插件的 label 与它任何
  一个页面的 label **不相等** —— 一级和二级重名正是这次要修的病，让它下次在测试里就被顶回来。
- 顺带核对 `tests/plugin_host.rs` / `tests/adminapi.rs` 里有没有把 `"MCPs"` 写死的断言。
- 四条门禁全绿。

### N5 — 文档

- `docs/09` §6：页面契约那节补一句「一级由 `pluginId` 分组，组顺序取组内最小 `order`，宿主不新增
  字段」，并指向本文。
- `docs/02`：模块图里 `admin_assets` 那行提一句两级导航。
- `README.md` 文档表格里 `docs/13` 那行的状态从「Proposal, not implemented」改掉。
- 本文档状态行改成「已实施」，并写上完成基线的提交号。

## 6. 验收

一次性检查清单，全部机械可查：

1. Node 仓库 `npx vitest run` 全绿，`admin-pages.test.ts` 里 D2 列的五条都在。
2. 本仓库四条门禁全绿。
3. `cargo test -p lmg-panel the_tree_is_byte_for_byte` 通过（不是 skip）。
4. `curl -s localhost:19999/api/plugins` 的 `pages[].pluginId` 输出未变 —— 后端契约没动。
5. 浏览器里：`#traffic` 直接打开 → 一级 MCP 选中、二级 Traffic 选中；`#tunnels` → 无二级栏；
   窗口高度改变时 pane 与 sidebar 各自滚动，页面本身不出现滚动条。
6. 把 `gateway.config.json` 里 `plugins.jobs.disabled` 设为 `true` 重启 → 一级 Jobs 那格带
   `· off` 与 tooltip，点进去是那条结构化 503 文案。

## 7. 不做什么

- **不做侧边栏归属的重构。** 现在 `page.sidebar` 是按页面给的，而 `.sidebar` 的内容由
  `js/sidebar.js` 固定渲染 MCP 列表 —— 于是历史上 `http-tools`（`sidebar: true`）会显示一列
  MCP。该插件已删除，现存页面里只有 `mcps` 打开侧边栏，疣子暂无实例；但机制还在，与二级化无关，
  两件事混在一个提交里会让面板复制的历史读不懂。要修就单开一份规范，让页面能声明自己的侧边栏内容。
- **不做 tab 溢出折叠**（「更多 ▾」）。六格离拥挤还很远，先有结构再谈容量。
- **不做路由分层**（D3）。
- **不给 `PageDescriptor` 加字段**（D1）。
- **不改 `/api/plugins` 的响应形状**。它是 Node 面板的契约，改它就得先改 Node 网关。
