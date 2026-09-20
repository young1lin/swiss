# 38 — 面板国际化：文/A 一键切换，英文原文为 key，中文是第一门外语

> 状态：**待实施**。基线 `d040640`（2026-09-20，master）——docs/37 R0–R6 已落地：面板 61 个自有
> 模块 19,150 行 TypeScript，零 `innerHTML` 写入，每个视图从 state 重画并答一个委托监听器，
> 69 个测试文件绿，`npm run check` 是门禁。本文的机制建立在 R5 之上，R5 之前做不起。
> 实施在分支 `panel-i18n`（worktree `.agents/worktrees/panel-i18n`，panel-ts 的先例）：**全部
> 落地、扫描闸绿、19998 中文走查完，再合并 master、部署一次**——中间态切到中文会半中半英，不进生产。
> 前置阅读：`docs/36-panel-typescript-spec.md` §1.2（D2 发射、D3 产物提交、D10 零 any 继续有效）、
> `docs/37-panel-modern-typescript-spec.md` §7（`h()` 与事件委托——本文的重画靠它）、
> `.agents/rules/panel-proof-of-life.md`、`AGENTS.md` 四条产品属性。
> 代码注释与源码里的 key 一律英文；中文只存在于字典 `panel/src/locales/zh.ts` 和本文散文。

> 需求原文（用户，2026-09-20）："看看我这个项目，怎么加下国际化的 文/A 那种 SVG 图标，可以切换
> 语言的，现在默认是英语，我需要加中文的"
> 追加确认（同日，六项决定全部按推荐）：默认英文、**不**按浏览器语言探测；切换**不** reload、重挂
> 当前页；偏好存 localStorage、每浏览器一份；第一期只翻面板自己拥有的文案，Rust 端来的字串不翻；
> 词表见 §1.3；分支上做完再合并部署。"按你的推荐来，写 spec 我给其他的模型开干。"

## 0. 现状与缺口

### 0.1 面板没有任何 i18n 机制（实测，基线 `d040640`）

`grep -rn "i18n\|locale" panel/src` 只命中 `localeCompare` 与 vendor 类型。文案全部内联：

| 计数 | 位置 | 例 |
| --- | --- | --- |
| **1,229** 处 `h()` 调用，其中文本子节点直接写英文 | 61 个 `.ts` | `h("div", { class: "empty" }, "Loading " + page.label + "…")`（`page-registry.ts:279`） |
| **~700** 个去重后的英文字面量（正则 `"[A-Z][a-zA-Z0-9 ,.':;!?()/-]{2,}"`，按出现次数 **~1,390**） | 最重的五个：`data-grid` 98、`jobs` 76、`tunnel-sheets` 71、`fields` 71、`run-history` 69 | — |
| **144** `toast(`、**26** `confirm(`、**5** `prompt(` | 到处 | `toast("Imported " + n + (s ? ", skipped " + s : ""))` |
| **15** 处手写英文复数 | `data-*`、`data-sql` | `n + " row" + (n > 1 ? "s" : "")` |
| **~10** 处 `toLocaleString()` 不带 locale | `data-grid`、`data-csv`、`data-filters`；`util.ts whenLabel` | 用的是浏览器默认语言，不是面板选的语言 |
| **14** 条静态属性文案 + 2 个文本节点 | `index.html` 97–133 | `placeholder="Search"`、`title="New group"`、`<div id="sideCap">MCPs</div>`、`mem …` |
| `<html lang="en">` 写死 | `index.html:18` | 浏览器靠它选中日韩字形变体，不是装饰 |
| `--sans` 无 CJK 字体 | `styles/base.css:68` | 中文掉到 `sans-serif` 兜底 |

文案落在六类位置，本文每一类都要接管：`h()` 文本子节点；`h()` 的 `title` / `placeholder` /
`aria.label` 属性；`toast` / `confirm` / `prompt` 的消息；`emptyNode({ title, hint, action })`；
模块顶层的标签表（`page-registry.ts:26-39` 的 `legacy[].label`、`GROUP_LABELS`）；`index.html`
的静态 chrome。

### 0.2 为什么现在

docs/37 R5 之后，每个视图都从 state 重画，且 `page-registry.ts:249` 的 `navigatePage(id, force)`
本来就会对当前页走一遍 `unmount → mount`，还带 `canLeave` 守卫。**切语言 = 重挂当前页**在今天是一
个函数调用；在 `innerHTML` 时代它意味着重写每一处字符串拼接。R5 买回来的东西，这是第一笔支出。

### 0.3 三个便宜的事实

- `t` 在 29 个文件里是局部变量名（`for (const t of targets)`）——helper 不能叫 `t`。叫 `tr`。
- 工具链不拦路：`tsconfig` 是 `es2022` / `esnext`，模块顶层 `await` 直接发射；面板已有四处动态
  `import()` 按 URL 解析（docs/37 §1.3）；`admin.rs` 是 `#[folder]` 整目录嵌入，`js/i18n.js` 与
  `js/locales/zh.js` 落进去就被服务。`cargo build` 仍不需要 node。
- 主题切换（`main.ts:95-133`）已经是"每浏览器一份偏好 + `#appZone` 里一个翻转按钮 + 首帧前脚本"的
  完整样板。语言按钮照着它写，一行不多发明。

## 1. 决定

### 1.1 宪法核对与 ADR

四条产品属性只拉到 **Ruthlessly small**，且有量：`zh.ts` 约 900 条 ≈ 50 KB，进 exe 一次（嵌入树
+50 KB），**运行时按需 `import()`**——偏好不是中文的浏览器一个字节都不多下；`i18n.ts` 本身 ≈ 3 KB。
**Plugin-shaped**：文案跟着视图模块走（key 写在视图里），字典是面板级一张表；进程内没有任何东西
变。**Hot-pluggable**、**三种接入方式**不涉及。docs/05：盘上与线上什么都不动——偏好在浏览器的
localStorage，`/api/*` 形状不变，`~/.swiss` 下不新增文件。

**不写 ADR。** 三条件缺两条：可逆（删掉 `tr()` 包裹就回到今天）、不意外（主题偏好已经走过同一条路）、
没有真正的取舍（备选项——reload、服务端存偏好、`en.json`——都是更贵而不是不同的权衡）。

### 1.2 十二条决定

| # | 决定 | 理由 |
| --- | --- | --- |
| L1 | **英文原文即 key，不建 `en.json`**。`tr("No targets yet")`；字典缺条目 → 原样返回英文，永不出 `undefined` 或 key 名 | 700 条已经在代码里，改法是机械的 `"x"` → `tr("x")`，diff 一眼可审；69 个测试文件里 334 处断言英文文本一个不用动 |
| L2 | **两个函数**：`tr(key, vars?)` 与 `trn(n, one, other, vars?)`；占位符 `{name}`；**可见文案不再用 `+` 拼接**。`trn` 用 `Intl.PluralRules(locale)` 选形，`{n}` 自动注入 | 中文语序不同，`"Cancelled pid " + row.pid` 翻不了；15 处 `(n > 1 ? "s" : "")` 是 `trn` 的账 |
| L3 | **字典 `panel/src/locales/zh.ts`**，`export default` 一个 `Record<string, string>`，按区域分段、段首注释写模块名；**中文的复数规则只有 `other`（`Intl.PluralRules("zh-CN")`），字典只要 `other` 形**——`trn` 的 `one` key 中文不需要条目 | 一张表、一次 `import()`；不为中文写两遍同一句 |
| L4 | **偏好 `localStorage["swiss_lang"]`**：无 → `en`；`"zh-CN"` → 中文；其他值 → `en`。不读 `navigator.language`。`<html lang>` 由首帧前脚本（`index.html:26-38` 那段扩一行）和切换时同时设成 `en` / `zh-CN` | 用户原话"默认是英语"；每浏览器一份，和主题同理——它描述这块屏幕 |
| L5 | **按钮 `#langBtn`** 放 `#appZone`，紧跟 `#themeBtn`；sprite 加 `i-languages`（Lucide `languages`，与现有 24×24 stroke 1.5 同一套）；两种语言是**翻转**，`title` 写点了会变成什么（`Switch to English` / `切换到中文`）；第三种语言出现时才改成菜单 | `main.ts:118-133` 的主题按钮就是样板 |
| L6 | **切换 = `setLang(next)` → `<html lang>` → `paintChrome()` → `navigatePage(currentView(), true)`**。不 `location.reload()`。`canLeave` 拦得住的（未保存表单）照拦——拦住时偏好**不写**，按钮不变 | R5 买回来的能力；reload 丢表单、断终端重连 |
| L7 | **模块顶层不许调 `tr()`**。标签表存 key（用 `tk("SSH Connections")` 标记——恒等函数，只为让扫描器看见字面量），画的时候 `tr(p.label)` | 模块加载时求值一次的字符串切语言不会变；`import` 先于 `await loadLocale()` 执行，顶层 `tr()` 永远拿到英文 |
| L8 | **`locale()` 返回 BCP-47 标签**（`"en"` / `"zh-CN"`）；`toLocaleString()` / `toLocaleTimeString()` 全部传它；`whenLabel` 同 | 不传 locale 用的是浏览器语言，面板选了中文数字日期还是英文格式 |
| L9 | **范围：面板自己拥有的文案。** Rust 端来的字串（`errText` 的 API 错误、插件 `name` / `lastError`、状态词 `ok` / `down`）原样嵌入占位符：`tr("endpoint {state}", { state: st })`。标识符不翻：分组名 `default`、连接名、表名、SQL、路径、`<code>` 里的命令 | 两边改是另一期；`default` 是名字不是词 |
| L10 | **两道机器闸进 `npm run check`**：(a) **完整性扫描**（vitest，`typescript` AST，非正则）——每个 `tr` / `tk` 字面量 key 与每个 `trn` 的 `other` key 在 `zh.ts` 有条目，`zh.ts` 无孤儿条目；(b) **覆盖 ratchet**（eslint `no-restricted-syntax`）——可见位置里的裸英文字面量是 error，**最后一个扫完的提交落地，白名单为零**（docs/37 R5 的做法） | (a) 保证有 key 就有译文；(b) 保证可见文案都成了 key。缺一条，覆盖率就靠 review |
| L11 | **每个扫完的区域一次提交**，带：该视图在中文下挂载的 vitest 断言（中文出现、英文不出现——用户看得见的状态变化，不是存在性断言）+ 19998 上**切成中文**走一遍 proof-of-life | 房子的规则：行为改动配用例，面板改动配真点 |
| L12 | **不做上下文机制**（`tr("Name", { ctx })` 之类）。同一英文两种含义要两种译文时，把其中一句英文改得更具体 | 700 条里这种碰撞一只手数得过来，机制比问题大 |

### 1.3 词表（所有译文的依据）

| 英文 | 中文 | 备注 |
| --- | --- | --- |
| MCP / MCPs | MCP | 产品名，不翻；复数丢掉 |
| Token | Token | 开发者术语，页名照旧 |
| SQL / DDL / PK / FK / TTL / CSV / JSON | 原样 | — |
| SSH | SSH | `SSH Connections` → `SSH 连接` |
| Secret（`${secret://name}` 语法、`secret://` 引用） | 原样 | 语法不翻；页名 **Secrets → 密钥** |
| Tunnel / Tunnels | 隧道 | — |
| Port forward / Port Forwards | 端口转发 | — |
| Job / Jobs | 任务 | — |
| Run（一次执行）/ Run history | 运行 / 运行历史 | — |
| Traffic | 流量 | — |
| Data | 数据 | — |
| Plugins | 插件 | — |
| Remote / Remote Targets | 远程 / 远程目标 | — |
| Terminal | 终端 | — |
| Group（分组容器） | 分组 | `default` 分组的名字不翻 |
| System | 系统 | — |
| Focus mode | 专注模式 | — |
| Appearance | 外观 | — |
| Connection（数据库/SSH 连接） | 连接 | — |
| Table / Column / Row / Index | 表 / 列 / 行 / 索引 | — |
| Query / Console | 查询 / 控制台 | — |
| Save / Cancel / Delete / Add / New / Edit / Close / Copy / Test | 保存 / 取消 / 删除 / 添加 / 新建 / 编辑 / 关闭 / 复制 / 测试 | 按钮两到四字 |
| Enable / Disable / Enabled / Disabled | 启用 / 停用 / 已启用 / 已停用 | — |
| Start / Stop / Restart / Running / Stopped | 启动 / 停止 / 重启 / 运行中 / 已停止 | — |

译文风格：简体；句中全角标点，句末英文有句号的中文用"。"，按钮与标签无句号；中文与英文/数字之间加
一个半角空格（`{n} 行`、`pid {pid} 已结束`）；提示句（hint）说做什么，不说是什么——和英文原文同一
口气；不出现"您"。

### 1.4 不变的东西（写明，免得"顺手"）

- 发射管线（docs/36 D2 / D3、docs/37 M2）一字不动：ts-blank-space、逐行保号、产物提交、无 bundler。
- `index.html` 的英文静态文案**保留**（它就是英文版；`paintChrome()` 在英文下是空操作），主脚本挂
  之前的首帧仍有 chrome。只加 `#langBtn`、`i-languages` 与首帧脚本的一行。
- 服务路径、`admin.rs`、`/api/*` 形状、Rust 任何一行、19999。
- 69 个测试文件全部保留；断言英文文本的用例**不用改**（默认英文）。不许删测试来过关。
- `crates/swiss-jobs/src/jobs/api.rs:358` 的 `include_str!` 断言 `jobs.js` 含 `/api/jobs/` 与
  `probeJobs`：扫 `jobs.ts` 时这两个符号保留。
- `localeCompare` 的排序行为不动；`js/vendor/**`（xterm 等）的文案不翻。
- 视图的画法、tokens、间距——除 `--sans` 补 CJK 字体外，不碰 CSS。

## 2. I0 — 机制：`i18n.ts`、字典骨架、按钮、chrome、完整性闸

一次提交（可拆两次：机制 + 按钮）。做完它，按钮能用，只有 chrome 变中文。

### 2.1 `panel/src/i18n.ts`

```ts
export type Lang = "en" | "zh-CN";
export const LANG_KEY = "swiss_lang";                       // absent = "en"; the preference, like THEME_KEY
export function langPref(): Lang;                           // localStorage → "en" | "zh-CN"; throws → "en"
export function locale(): string;                           // BCP-47 for Intl / toLocale*: "en" | "zh-CN"
export function setLang(l: Lang): void;                     // store ("en" removes the key), set <html lang>
export function nextLang(): Lang;                           // the flip target: en → zh-CN → en
export async function loadLocale(): Promise<void>;          // pref zh-CN → await import("./locales/zh.js"); install
export function install(lang: Lang, dict: Record<string, string> | null): void;  // tests + loadLocale
export function tr(key: string, vars?: Record<string, string | number>): string;
export function trn(n: number, one: string, other: string, vars?: Record<string, string | number>): string;
export function tk(key: string): string;                    // identity; marks a key stored for a later tr()
```

行为：`tr` 查当前字典，缺则用 key 本身，再替换 `{name}`；未提供的占位符**原样保留**（不吞、不抛）。
`trn` 按 `new Intl.PluralRules(locale()).select(n)` 取 `one` 或 `other` 为 key，中文永远是 `other`；
`{n}` 由 `trn` 注入，`vars` 可覆盖。`install(lang, null)` 回到英文（测试用）。字典对象是模块内一个
`let`，没有全局、没有 `window.*`。

`main.ts` 顶层、所有会画东西的调用之前：`await loadLocale();`（`es2022` 顶层 `await` 直接发射）。

### 2.2 切换与 chrome（`i18n.ts` 里的 `initLangButton()` + `paintChrome()`，`main.ts` 只调用）

`paintChrome()` 用 `tr()` 重设 `index.html` 的那十几处静态文案：`#filter` 的 placeholder / aria-label、
`#addBtn` 与 `#themeBtn` / `#expandBtn` 的 title / aria-label、`#langBtn` 的 aria-label（title 见下）、
`#sideCap`、`#memChip` 的 title、rail 的 `aria-label`。`paintThemeBtn()` 的两句 title 也走 `tr`。

`#langBtn` 点击：`if (!pageCanLeave()) return;`（page-registry 导出一个查询，`canLeave` 拦住就什么
都不做）→ `setLang(nextLang())` → `await loadLocale()`（切到中文时才真的下载字典）→ `paintChrome()`
→ `await navigatePage(currentView(), true)` → `paintLangBtn()`。切回英文时 `install("en", null)`。

`paintLangBtn()` 的 title 是全面板**唯一一句写在目标语言里的文案**：英文界面上是 `切换到中文`，中文界面上
是 `Switch to English`——在读不懂当前语言的人眼里，只有目标语言自己的名字是可读的。两句字面量写在
`paintLangBtn()` 里、不进字典，注释写明这个理由；按钮的 `aria-label="Language"` 照常走 `tr`（→ `语言`）。

`index.html`：`#appZone` 里 `themeBtn` 之后加

```html
<button class="btn ghost icon" id="langBtn" title="切换到中文" aria-label="Language"><svg class="ic" aria-hidden="true"><use href="#i-languages"></use></svg></button>
```

sprite（`index.html:43-89` 的 `<svg hidden>` 内）加 Lucide `languages`：

```html
<symbol id="i-languages" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="m5 8 6 6"/><path d="m4 14 6-6 2-3"/><path d="M2 5h12"/><path d="M7 2h1"/><path d="m22 22-5-10-5 10"/><path d="M14 18h6"/></symbol>
```

首帧前脚本（`index.html:26-38`）加一行：读 `swiss_lang`，是 `zh-CN` 就
`document.documentElement.lang = "zh-CN"`（默认 `en` 已写在 `<html>` 上）。

### 2.3 `panel/src/locales/zh.ts` 骨架

I0 只放 chrome 的十几条（`Search` → `搜索`、`New group` → `新建分组`、`MCPs` → `MCP`、
`Appearance` → `外观`、`Switch to light` → `切换到浅色`、`Focus mode — hide app navigation (Esc
exits)` → `专注模式——隐藏应用导航（Esc 退出）`、`Language` → `语言`……）。段首注释 `/* --- chrome (index.html, main.ts) --- */`，后面每个区域一段。

### 2.4 完整性扫描 `panel/test/i18n-complete.test.ts`（L10a）

照 `non-null-ratchet.test.ts` 用 `typescript` 走 AST（`build.mjs` 的 `listSources()` 给文件表）：

- 收集所有 `CallExpression`：callee 是 `tr` / `tk` 且首参是 `StringLiteral` / `NoSubstitutionTemplateLiteral`
  → key；callee 是 `trn` 且第二、三参是字面量 → `other` key 必须有条目，`one` key 记为 en-only。
- 断言 1：每个收集到的 key 在 `zh` 里有条目，缺的列全名报"missing zh: …"。
- 断言 2：`zh` 的每个 key 都被某处字面量用到，多的列"orphan zh: …"。
- 断言 3：`zh` 没有条目的值等于 key（忘了翻直接复制英文）。
- `tr(x)` 首参不是字面量的调用**不报错**（动态 key 来自 `tk()` 标记的表，靠 review）。

### 2.5 验收（I0）

`panel/test/i18n.test.ts`（node 环境，stub `localStorage`）：

- `tr("No targets yet")` 英文下原样；`install("zh-CN", { "No targets yet": "还没有目标" })` 后为中文；
  未收录的 key 原样返回。
- `tr("Imported {n}, skipped {s}", { n: 3, s: 1 })` → `Imported 3, skipped 1`；缺 `{s}` 时保留字面
  `{s}`。
- `trn(1, "{n} row", "{n} rows")` → `1 row`；`trn(2, …)` → `2 rows`；中文字典只含 `"{n} rows": "{n} 行"`
  时 `trn(1, …)` → `1 行`。
- `langPref()`：无存储 → `en`；`"zh-CN"` → `zh-CN`；垃圾值 → `en`；`localStorage` 抛异常 → `en`。
- `setLang("zh-CN")` 写入并设 `document.documentElement.lang`；`setLang("en")` 删 key；`nextLang()` 翻转。

`panel/test/i18n-toggle.test.ts`（happy-dom，`index.html` 骨架，同 `admin-data-structure-fk-ref.test.ts`
的搭法）：`paintChrome()` 英文下 `#filter.placeholder === "Search"`；`install("zh-CN", zh)` 后
`=== "搜索"`、`#sideCap.textContent === "MCP"`；`canLeave` 为 false 时点 `#langBtn` 不写偏好、
`lang` 不变。

19998（proof-of-life，真实指针事件）：冷加载英文；点 文/A → `document.documentElement.lang ===
"zh-CN"`、chrome 中文、当前页重挂（Remote 页的分组还在）、`localStorage.swiss_lang === "zh-CN"`；
刷新仍中文、没有英文闪一下再变（首帧脚本生效）；再点回英文；Network 面板里英文状态下**没有**
`locales/zh.js` 请求。

## 3. I1 — 壳与共享模块

`page-registry`（32：`legacy[].label` / `GROUP_LABELS` 改 `tk()`，`paintNavigation` 与 `navigatePage`
的 `Loading {page}…` / `{page} unavailable` / `This plugin is disabled…`）、`plugin-palette` 21、
`pane` 29、`page-core` 8、`menu` 7、`dropdown` 11、`groups` 20、`group-logic` 4、`add-sheet` 33、
`fields` 71、`polling` 14、`connect` 7、`sidebar` 8、`immersive` 10、`util` 4（`emptyNode` 本身不翻
——它收的 `title` / `hint` / `action` 由调用点 `tr()`；`whenLabel` 传 `locale()`）。约 280 处。

**验收：** 这批没有自己的页，用例挂在已有的测试上：`admin-page-registry*` 类用例在中文字典下断言
rail / 导航文本；`groups` 的"新建分组"提示；`add-sheet` 的标题。19998：中文下每个顶层导航、分组
菜单、加号 sheet 的标题与按钮。

## 4. I2 – I8 — 按视图逐个扫（顺序按风险从低到高，docs/37 R5 同理）

| 阶段 | 视图 / 模块 | 出现次数 | 该区域的坑 |
| --- | --- | --- | --- |
| I2 | `views/plugins` 13、`views/secrets` 18、`views/system` 17 | ~50 | 插件 `name` / `lastError` 来自 Rust，原样进占位符（L9）；密钥**值**永不进文案 |
| I3 | `views/tokens` 27、`traffic` 33、`views/traffic` 2 | ~60 | `label + " copied — the token is embedded."` → `tr("{label} copied — the token is embedded.", …)` |
| I4 | `detail` 36、`logs` 60、`run` 9、`views/mcps` 2、`mcp-state` / `ui-state` 各 2 | ~110 | `logs` 的搜索状态句（docs/31）有计数与时间，全部占位符；`<code>` 里的 `claude mcp add` 不翻 |
| I5 | `views/remote` 28、`views/remote-runs` 28 | ~56 | 分组 `default` 是名字；`No items - drop here or press +` 这类空分组行是文案 |
| I6 | `tunnels` 49、`tunnel-sheets` 71、`views/tunnels` / `tunnel-forwards` / `tunnel-state` 各 2 | ~125 | 表单校验消息（`Name is required`）集中在 sheets；`18989 → 127.0.0.1:18989 via …` 是数据不是文案 |
| I7 | `jobs` 76、`run-history` 69、`jobs-v2` 3、`job-state` 2、`views/jobs` 4 | ~155 | `include_str!` 守的两个符号（§1.4）；cron 描述句里的时间词 |
| I8a | `data-grid` 98、`data-view` 55、`data-sql` 42、`data-ddl` 38、`data-structure` 33 | ~265 | 15 处手写复数全在这里和 I8b → `trn`；`first–to of total` 分页句；`Number(x).toLocaleString(locale())` |
| I8b | `data-browsers` 56、`data-csv` 52、`data-form` 26、`data-edit` 19、`data-cell` 14、`data-activity` 13、`data-filters` 11、`data-suggest` 10、`data-value` 7、`db-state` 2、`views/data` 4 | ~215 | redis 的 `Commit {n} commands to {key}?` 多行 confirm；CSV 导入的 `{n} rows` |
| I9 | `views/terminal` 44、`views/terminal-settings` 11、`terminal-core` 3、`term-overlay` 2 | ~60 | xterm 是 vendor，不翻；只翻工具栏、设置 sheet、提示条 |

每个阶段的做法固定：

1. 该区域每个可见字面量 → `tr()` / `trn()`；拼接改占位符；顶层表改 `tk()`；`toLocale*` 传 `locale()`。
2. `zh.ts` 加该区域一段；`npm run check` 的完整性扫描指出漏的。
3. 该视图的测试文件加一个中文挂载用例：`install("zh-CN", zh)` → `mount` → 断言一句中文出现且对应英文
   不出现；`afterEach` 里 `install("en", null)`。
4. `npm run build` → `touch crates/swiss-panel/src/lib.rs` → `target-test` release 重建 → 19998 上切中文，
   按 `.agents/rules/panel-proof-of-life.md` 全清单走该视图；提交说明写清走了哪些、哪些没走及原因。

## 5. I10 — 覆盖 ratchet、字体、文档

### 5.1 覆盖 ratchet（L10b，`eslint.config.*`）

`no-restricted-syntax` 一条规则，最后一个视图扫完时落地为 **error、白名单为零**（在此之前可以先以
warn 挂着当进度表）。选中"可见位置"里含至少两个连续字母的字符串字面量：

- `h(tag, props, ...children)` 的 children 位置（第三参起），**`tag` 为 `"code"` / `"kbd"` / `"pre"`
  的除外**；
- `h()` props 里 `title` / `placeholder` / `aria.label` 的值；
- `toast` / `confirm` / `prompt` / `say` 的首参；
- `emptyNode({ title, hint, action })` 三个属性的值。

不选中：`class`、`id`、`data.*`、`href`、`type` 等非文案属性；只含符号 / 空白 / 数字的字面量
（`" · "`、`"—"`、`"…"`）；`tr` / `trn` / `tk` 自己的参数。选择器写不进的边角（比如
`String.fromCharCode`）不要为它加豁免，改写法。

### 5.2 字体与 `<html lang>`

`styles/base.css:68` 的 `--sans` 在 `system-ui` 之后、`sans-serif` 之前加
`"PingFang SC", "Hiragino Sans GB", "Microsoft YaHei UI", "Microsoft YaHei", "Noto Sans CJK SC"`。
Inter 仍在最前——拉丁字形来自 Inter，CJK 字形落到系统中文字体，这是想要的结果。

### 5.3 文档

- `AGENTS.md` "Making changes" 加一条：面板的可见文案都经 `tr()`，中文条目是改动的一部分，两道闸
  在 `npm run check` 里。
- `.agents/rules/panel-proof-of-life.md` 清单第 4 条加半句：涉及文案的改动，走查在**中文**下再来一遍。
- `README.md` docs 表加 docs/38 一行（docs/37 那行之后）。
- 本文状态头改"已实施"，写上落地提交与 §7 的终值。

## 6. 验收与门禁命令

```
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + lint（含覆盖 ratchet）+ build:check + vitest（含完整性扫描）

# Rust（仓库根；--workspace 不可省）
cargo test --workspace # 含 swiss-jobs 对 jobs.js 的 include_str! 断言
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d          # 本文不加 cargo 依赖，这条是回归守卫
```

实机（proof-of-life，19998）：I0 的切换流程；I1–I9 每个阶段该视图在**中文**下的全清单走查，真实指针
事件。**走不了的流程（缺凭据、外部端点）如实列为未验证并写明原因。**

### 6.1 交付顺序与每提交硬性要求

I0 → I1 → I2 → I3 → I4 → I5 → I6 → I7 → I8a → I8b → I9 → I10，不许跳。每个提交：

- 只做一件事（一个区域）；提交说明写清"扫了哪些文件、几条 key、哪些用例覆盖、19998 走了什么"；
- `npm run check` 与 `cargo test --workspace` 在提交前跑过；
- 不许手改 `crates/swiss-panel/src/admin_assets/js/**`——改 `panel/src/*.ts` 后 `npm run build`；
- 不许删测试来过关；不许整文件 `eslint-disable`；单行 disable 必须带理由注释；
- 不碰 19999。合并与部署是 I10 之后 owner 的一次决定。

## 7. 要记录的数字（每阶段的提交说明里）

基线（`d040640`）：可见位置裸英文字面量约 **1,390** 处（§0.1 的正则；I0 起改用 ratchet 规则的
warn 计数）、`zh.ts` **0** 条、发射自有 JS 字节（`npm run build` 后 `js/**` 非 vendor 总和）、
`swiss.exe` 字节。每阶段记：剩余裸字面量数、`zh.ts` 条目数、`zh.js` 字节、发射字节增量。
I10 时四个数的终值进本文状态头。

## 8. 不做什么

- 不翻 Rust 端任何字串：`swiss` CLI 输出、API 错误文本、插件 `name` / 描述 / `lastError`、状态词。
  要翻是另一期，走 wire、动两边。
- 不自动探测浏览器语言；不做服务端偏好；不做第三种语言（机制允许——加一个 `locales/xx.ts`，按钮
  改菜单——但本文只到中文）。
- 不做 RTL、不做上下文机制（L12）、不改排序（`localeCompare`）。
- 不翻 `js/vendor/**`（xterm 等）、不翻 `<code>` 里的命令与语法、不翻标识符（分组名、连接名、表名）。
- 不 bundle、不引任何 i18n 库、不引 ICU MessageFormat——`{name}` 替换加 `Intl.PluralRules` 够用。
- 不改 `index.html` 的英文静态文案、不改 `/api/*`、不碰 19999。
