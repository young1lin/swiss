# 35 — 分组列表：一种画法（色带组头）、组头整条可拖、内联表单一行

> 状态：**已实施（两轮：`3836153` 页面密度卡片化 + 整头可拖；第二轮侧栏同画法、内联表单、行动作归 ⋯）**。
> 基线 `8569240`（2026-09-18）。设计的规范化文本在 skill §16.5 / §16.8 / §19。
> 前置阅读：`.agents/skills/swiss-ui-design/SKILL.md` §16.5 / §16.8（本文改写了这两条），
> `docs/20-groups-and-hierarchy-spec.md` §4（组件、API、存储不变，只改容器画法与拖拽面）。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-09-18，附 Remote 页截图）："现在这个项目 UI 巨丑，我希望你的审核来重构下，
> 并且分组，应该是鼠标按住整个分组的那块就能拖动，而不是专门的地方可以拖动。"

## 0. 审核结论：页面密度上的 tree 模型不成立

docs/20 §4 的第一次修订把组头改成「树节点」：透明底、chevron + folder + 名称 + 计数，`+`/`⋯`/抓手
hover 才现，成员缩进一格、左侧 1px 引导线。侧栏 248px 宽时这是对的（Finder / VS Code 的 source
list）。页面密度（Tunnels、Remote、Jobs、Secrets、Tokens）把同一套画在 `--measure-wide` 1180px 上，
截图里能看到的全部问题都出自这一点：

1. **`+` 离名字一屏远**：组头横跨 1180px，`+` 贴右缘（x≈1495），名字在 x≈170，两者看不出是一件事。
2. **没有容器轮廓**：透明组头 + 缩进 54px 的白卡，组从哪儿开始到哪儿结束全靠猜；空组只剩一行
   小字和一段 15px 的引导线残段，像渲染错误。
3. **拖组要先找抓手**：抓手 14px、hover 才现、在组头最右端——用户的原话就是对它的投诉。
4. Remote 页说明文字三行，状态行 "serving · 3 endpoints served by tunnels" 把 serving 说了两遍。

侧栏密度没有这些毛病，不动。

## 1. 决定

### 1.1 两种密度，两种容器；同一解剖

| | 侧栏（`.grp--side`） | 页面（`.grp--page`） |
| --- | --- | --- |
| 容器 | 树节点：透明组头，成员缩进 42px，`.grp--side::before` 引导线 | **组即卡片**：`.grp` 自身带 `.group`（views.css 的环、圆角、裁切），成员通栏铺在组头下 |
| 组头 | 28px；chevron + folder + 名称 + 计数 | 36px `--sep-soft` 色带；chevron + 名称 + 计数（**不画 folder**——卡片边缘已经说明了「这是容器」，去掉后名字正好对齐行名：组头 12+12+8=32，行 16+6+12=34） |
| hover | `--hover` | `--sep`（比色带深一级；`--hover` 比 `--sep-soft` 还浅，会读成「色带熄灭」） |
| `+` / `⋯` | 原样 | 取 `.btn.icon` 的 32×24 盒，与行尾 `⋯` 同一列 |
| 空组行 | 28px caption | 42px（一行的高度）、`--f-label`、行内边距 16px——空组是一个空行，不是塞在色带下的小字 |
| 组间距 | `--s1` | `--s4` |

`jobs.js` 用 `pane.querySelector(".group")` 判断「列表画过没有」，`.grp--page` 自带 `.group` 正好让它
继续成立，不用改。

### 1.2 组头整条可拖

- `head.draggable = true`，`dragstart` 时若 `e.target.closest(".grp-add, .grp-more")` 则
  `preventDefault()`：取消的 dragstart 等于一次普通的 mouse-up，点击照常落地。这是 docs/20 §4.1
  「按钮不能住在可拖元素里」的另一半解法——不是把拖拽面缩成抓手，而是让按钮退出拖拽。
- 浏览器自带拖拽阈值，所以 toggle 上的单击仍是折叠/展开。
- 光标：`.grp-head[draggable="true"] { cursor: grab }`，toggle 继承；`+`/`⋯` 保持 pointer。
  `data-view.js` 手搭的 pg schema 组头没有 draggable，不显示 grab。
- `.grp.dragging` 整组半透明（不再只是组头）。
- `i-grip` sprite 删除；`⋯` 菜单的 Move up / Move down 仍是键盘与精确路径。

### 1.3 落点

| 拖的是 | 落在 | 结果 |
| --- | --- | --- |
| 组 | 另一组的**整块** `.grp`（组头或成员都算），按整块的上下半 | before / after，`.grp.drop-before/after` 画在整块上（页面密度保留卡片环：`var(--shadow-card), inset …`） |
| 行 | 组头 **或空组行** | drop-into（1.5px accent 环）——"drop here" 那一行自己得接受 drop |
| 行 | 另一行 | 不变（docs/20：排序 + 改组一次完成） |

事件分工：行只处理行拖，组头/空行只处理行的 drop-into，组拖一律冒泡到 `.grp`（`wireGroupDrop`）。
`dragleave` 用 `relatedTarget` 判断是否真的离开整块，避免从组头挪到行时高亮闪烁。

### 1.4 Remote 页

- 说明缩成两句（是什么、谁还会写它）；拖拽由列表自己教，不写进说明。
- 状态行只在 presence 不是 `serving` 时才带前缀（"Tunnels stopping · 0 endpoints served by tunnels"）。
- 顺手修的一个坑：sheet 里 Label 写着 optional，store 却拒绝空 label
  （`swiss-remote/src/target.rs`）——留空时以 alias 作 label 提交；行里 label 等于 alias 时不重复画。

## 2. 验收（2026-09-18，19998，agent-browser，真实 CDP 指针事件）

- [x] Tunnels / Remote / Jobs / Secrets 页面密度全部以卡片渲染；侧栏（MCP）树形不变。
- [x] Remote：New group ×2、Add target ×2 经 sheet 落库；组头 `+` 打开 sheet 且 Group 预选该组
      （`sheet.hidden === false`、computed display block、rect 高 519px）。
- [x] 按住组名拖 `prod` 到 `default` 上半：dragstart/dragover/drop 触发，
      `/api/remote/targets` 的 groups 变为 `[prod, default, test]`。
- [x] 拖 `prod` 组头到 `test` 卡片**下半的行**上：拖拽中 `test` 卡带 `drop-after`、`prod` 带
      `dragging`；落地后 groups `[default, test, prod]`。
- [x] 行 `flash` 拖到 `test` 组头 → group=test；行 `build` 拖到 `prod` 空行 → group=prod。
- [x] 组头 `⋯` 用原始 mouse down/up 打开菜单（可拖组头内的按钮仍可点）；Move up 生效。
- [x] toggle 单击折叠：`.collapsed`、body `display:none`、`aria-expanded=false`、localStorage 持久。
- [x] 侧栏：按住 `learn` 组名拖到 `ForTest` 上半 → `drop-before` 高亮，顺序变为
      `[default, learn, ForTest]`。
- [x] 深色主题、900px 宽（无横向溢出，rail 与 context bar 在位）。
- [x] vitest 59 files / 535 tests 通过；`admin-groups-tree.test.ts` 改为钉住新契约（整头可拖、
      按钮 dragstart 退出、页面密度自带 `.group`、无 folder、CSS 数字）。
- [ ] 未验证：Secrets / Tokens 页的行内表单在卡片内的表现只看了渲染（组数正确），没有走完
      新增流程。

## 3. 第二轮（同日）：侧栏同画法、内联表单、行动作

> 用户："这个是不是太过紧凑了，也要重构下？我记得有很多地方都是这样的，太过紧凑了。……这种分组的
> （侧栏），你也要重构下。"

### 3.1 侧栏分组改为同一种色带组头

第一轮把页面密度改成卡片后，侧栏仍是树节点（透明头、folder、引导线、42px 缩进），两处"分组"长得
不像一件事。现在**只有一种画法**：`--sep-soft` 色带 + chevron + 名称 + 计数 + `+`/`⋯`，侧栏 28px、
页面 36px。folder 图标与引导线一并删除（`i-folder` sprite 删；`folder-plus` 保留给"新建组"）。
侧栏成员缩进一格（`--s4`），成员的圆点落在组名正下方；组间距 `--s2`；色带下留 `--s1` 的空气，
避免第一行的 hover 与色带粘连。`data-view.js` 手搭的 pg schema 组头用的是同一组 class，自动跟上。

### 3.2 内联表单：一行

Tokens 页的表单用了 `.two`，而 `.two` 在 sheet 之外没有任何规则——于是输入框独占一行、按钮换行挤在
下面、列表直接贴上来。Secrets 的 `.vault-store` 是自己的一套。现在两页共用 `views.css .inline-form`：
`[字段…] [字段…] [group ▾] [主按钮]` 一行，flex-wrap，与列表之间 `--s5`；"New group" 移到
`.pane-head` 右侧的 `.pane-actions`——和 Remote / Tunnels 一致，页面级动作永远在右上。

### 3.3 行动作归 ⋯

Tokens 行有 Use / Rotate / **Revoke（红）**，Secrets 行有 Copy ref / **Delete（红）**，违反 skill
§16.4（一行最多一个非图标按钮，红色不上行）。现在：Tokens 行 = `Use`（正在使用的那枚没有）+ `⋯`
（Rotate secret ─ Revoke）；Secrets 行 = `Copy ref` + `⋯`（Delete）。行尾的 `⋯` 与组头的 `⋯` 同一列。

### 3.4 验收（19998，真实指针）

- [x] MCP 侧栏：色带组头；按住 `ForTest` 组名拖到 `learn` 的成员行上 → `drop-after`，顺序
      `[default, learn, ForTest]`。深色正常。
- [x] Tokens：内联表单一行、表单到列表 20px；Create 落库并弹出一次性 secret 框；行无红按钮；
      `⋯`（原始 mouse down/up）→ "Rotate secret / Revoke"；Rotate 出新 secret；Revoke 经 confirm 删除。
- [x] Secrets：同上；Store 落库；`⋯` → Delete 经 confirm 删除。
- [x] vitest 59 / 535 通过（icons 测试改钉 `i-folder-plus`；secrets 测试改钉 `data-skmore`）。
