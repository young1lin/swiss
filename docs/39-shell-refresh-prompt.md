# docs/39 实施提示词 — 壳的减法（图标 rail、下划线页签、一种读数、记住上次的页）

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

worktree **已经建好**，直接进：

```
<repo>\.agents\worktrees\refactor-ui
```

分支 `refactor-ui`，基线 `f959b12`（master，2026-09-20）。规范 `docs/39-shell-refresh-spec.md` 与视觉
参考 `docs/assets/39/shell-mockup.html` 都在这个 worktree 里。面板的 npm 家在 `crates/swiss-panel/panel`，
`node_modules` 不进 git——第一件事在那里 `npm ci`，否则 `npm run check` 报 `'tsc' is not recognized`。

**不要 `cd` 回主 checkout，不要碰 `.agents/worktrees/panel-i18n`（另一个会话在那里做 docs/38）。**
PowerShell 里 `cd` 混在链式命令中会打断后面每一个相对路径——构建与实例脚本各起一条命令，从 worktree 根跑。

## 任务

`docs/39-shell-refresh-spec.md` 定义的 P0 → P3。一句话：面板的壳换形不换主——rail 只留图标（48px，
9px 标签删掉）；context bar 左侧从带框的 `MCP / Servers ▾` 菜单改成 **glyph + 插件名** 的标题加
**下划线页签**（多页插件），页签装不下时进末尾 `⋯`；右侧 count 与内存读数统一成一种字号一种灰；侧栏
类型 glyph 去掉灰底方块；**每个插件记住上次访问的页**，rail 座位和 palette 点回去时落在那一页。
L1 / L2 / L3 的归属、seg、页体、Terminal 的 Focus dock 一个都不动。

先双击打开 `docs/assets/39/shell-mockup.html`，按 `3`：那就是目标（preset C）。按 `1` 是今天。

## 先读这些（按顺序，别跳）

1. `docs/39-shell-refresh-spec.md` —— 全文。§1.2 S1–S12 是决定，§1.3 是不许动的清单，§2 是每个阶段的
   文件、代码形状、用例与 19998 走查项，§3 是要抄的 CSS。
2. `.agents/rules/panel-proof-of-life.md` —— 全文，逐条。每个阶段在 19998 上真点。
3. `.claude/skills/swiss-ui-design/SKILL.md` §1–§8、§16 规则 1 / 2 / 9 / 15 / 18、§18 清单。P3 要改它的
   §3 / §4 / §13 / §17。
4. `crates/swiss-panel/panel/src/page-registry.ts` —— 全文（300 行）。`railSeat` :85、`paintPluginRail`
   :103、`pageMenuItems` :138、`paintPluginContext` :150–208、`paintNavigation` :211、`navigatePage`
   :249–283、`initPages` 里的 `hashchange` 监听 :241。
5. `crates/swiss-panel/panel/src/plugin-palette.ts` —— `GLYPHS` / `glyphNode` :36–51（标题要用同一个
   glyph）、:133 的 `go(g.pages[0].id)`（P0 改它）。
6. `crates/swiss-panel/src/admin_assets/index.html` :114–124（`#ctxBar`）与 `styles/base.css` :257–290
   （rail）、:292（`#memChip`）、:315–348（ctxbar）、:349–370（immersive）、:412–420（`.side-type`）。
7. `crates/swiss-panel/panel/test/admin-navigation.test.ts` —— 你要改的用例都在这里；`FakeNode` 的搭法
   决定了断言怎么写（markup 字符串 + `hidden` 状态）。
8. `crates/swiss-panel/panel/src/immersive.ts` 头部注释 —— 确认你不需要动它。

## 交付顺序

一个阶段一个提交，不许跳：

| 阶段 | 内容 | 关键风险 |
| --- | --- | --- |
| P0 | `last-page.ts`（`loadLastPages` / `rememberLastPage` / `targetPageFor`）+ `navigatePage` 记一笔 + rail 座位与 palette 点击走 `targetPageFor` + `last-page.test.ts` + `admin-navigation` 加一条 | 座位的 `data-view` 属性**保持** `pages[0].id`（`jobs.ts:61` 与 `admin-navigation.test.ts:162` 靠它）；记的是 `currentGroup()!.id`，不是 `page.pluginId`；`canLeave` 拦住时不记 |
| P1 | `#ctxBar` 新 markup（`#pageTitle` + `#pageTabs`，删 `#pageBtn` / `#pageLoc`）+ `paintPluginContext` 重写 + `fitTabs()` + `layoutTabs()` + `⋯` 菜单 + `ResizeObserver` + immersive 隐藏表 + §3 的 CSS + `fit-tabs.test.ts` + `admin-navigation` 逐条改 | tab 是 `<a href>` 走 hash，不是 button 直调；happy-dom 无布局，`offsetWidth` 为 0 → 全部可见，别 mock；`paintPluginContext` 每次重画前 `disconnect()` observer |
| P2 | rail 48px / 36px / 18px、删 `.rail-btn-label` 与两处 `h("span", { class: "rail-btn-label" })`、读数统一、`.side-type:has(.ic)` 去底 | 只加 48 / 36 / 18 三个尺寸字面量，零颜色字面量；每条改过的 CSS 规则旁边的注释改成新理由 |
| P3 | `swiss-ui-design` skill §3 / §4 / §13 / §17、`.agents/docs/style-design.md`、`swiss-add-plugin` skill 两条、README 一行、spec 状态头 + 数字、`docs/assets/39/` 四张截图 | 旧的"已实施"描述一条不留 |

每个阶段的固定动作：改 `panel/src` / CSS → 用例 → `npm run check` → 重建 19998 → 真点走查（spec §2 该阶段
的清单，1440 与 960 两个宽度，明暗两套）→ 提交。

## 门禁（每次提交前）

```
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + lint + build:check + vitest

# worktree 根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

实机验证（每个阶段）：

```
scripts/test-instance.ps1 -Stop
# 先在 crates/swiss-panel/panel 下 npm run build，再 touch crates/swiss-panel/src/lib.rs，
# 否则 rust_embed 的指纹不变，release 二进制里还是旧面板
$env:CARGO_TARGET_DIR="target-test"; cargo build --release
scripts/test-instance.ps1 -Fresh
```

然后用 agent-browser 在 19998 上走**真实指针事件**（CDP input，不是 `element.click()`），冷加载：

- P0：MCP → Traffic，点 Jobs 座位，点 MCP 座位 → 落在 Traffic；刷新后重复仍 Traffic；palette 里点 MCP
  → Traffic；地址栏 `#tokens` → Token；Data 页造一个未保存编辑再点 MCP → 留在 Data。
- P1：七个座位每个点一遍，标题 glyph + 名字对；MCP / Tunnels / Settings 的每个 tab 点一遍，`aria-current`
  与 hash 跟着走；Data / Jobs / Terminal 无 tab，量 `#pane` 的 `getBoundingClientRect().top` 与 MCP 页
  相等；Focus 模式标题与 tab 隐藏、Terminal 的 dock 收成 0；把窗口压到 480px 逼出 `⋯`，点开菜单切页——
  **提交说明里写明这是压到支持底线以下逼出的，真实插件到不了**。
- P2：rail 量宽 48、hover 每座有 tooltip；读数一行同色；960px 下 mem 与 `·` 消失；Terminal 页 count 为空
  时不画 `·`；侧栏 glyph 无底、hover 变亮。

对照发射产物核对服务字节（面板服务在 `/`）。**走不了的流程如实列为未验证并写明原因。**

agent-browser 的已知坑：冷启动后第一次 `open` 可能让包装器超时，但会话已经建好——下一条命令用同一个
`AGENT_BROWSER_SESSION` 接着走；`set viewport 1440 820` / `set viewport 960 820` 换宽度；没有对话框时
`dialog accept` 报错是正常的。

## 提交规则

- 一个提交一个阶段；提交说明写清"改了哪些文件、哪些用例覆盖、19998 走了什么、没走什么及原因"；P2 起
  贴 spec §4 的数字（`base.css` 字节、发射 JS 字节、`swiss.exe` 字节、一屏矩形数）。
- 行为改动必须点名并配用例；不许删测试来过关；`admin-navigation.test.ts` 的旧断言是**改成新契约**，不是
  删掉；不许整文件 `eslint-disable`（单行 disable 必须带理由注释）。
- 不许手改 `crates/swiss-panel/src/admin_assets/js/**`——那是发射产物，改 `panel/src/*.ts` 后 `npm run build`。
- CSS 值走 token；每条改过的规则旁边的注释说的是**现在的**理由。
- 提交信息结尾按仓库惯例署名。

## 不做什么

见规范 §5。特别是：不拆 seg、不动页体、不做宽 rail / hover 展开、不给 `/api/plugins` 加 icon 字段、不加
token 与 sprite、不改 Terminal dock 与 Focus 位置、不改 `RAIL_LIMIT`、**不碰 19999、不碰 panel-i18n
worktree**——合并 master 与部署是 P3 之后 owner 的一次决定，不在本会话里做。docs/38 若先进了
master，rebase 时本文的两条新文案改 `tr()`；否则不管它。

## 完成之后

`swiss-verify` → `swiss-live-verify` → `swiss-review` 三步走完，再向 owner 汇报：四个数、19998 走了哪些、
哪些未验证及原因（溢出 `⋯` 一定在这一栏）。
