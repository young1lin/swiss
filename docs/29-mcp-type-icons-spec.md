# docs/29 — MCP 类型图标(行尾 chip 图形化)

状态:已批准实施(操作者当面提出,2026-09-16)。测试端口 19997。

## 问题

侧边栏行尾的 launch chip 是纯文本(mysql / redis / pg / http / figma / npx …)。同类工具一眼认图标,
纯文本要读;操作者要求:有对应品牌图标的换成 SVG(海豚、Redis 菱形堆、Figma 标…),**没有图标的类型
才回退文本展示**。

## 方案

- 沿用 docs/18 V2 sprite 机制与 `i-mcp` 先例:官方标记的单色重绘,进同一个 24×24 sprite,
  `currentColor` 跟随主题。品牌路径取自 Simple Icons(CC0,24 网格,fill 版):mysql、redis、
  postgresql、figma、docker;http/rest/proc/npx/uvx 用通用字形(globe / plug / terminal / package,
  Lucide 风格手绘描边)。智谱用其品牌本体几何 Z(官方 logo.svg 是 30×30 多层渐变稿,不适
  14px chip,取 Z 字形以 house 描边风格手绘;操作者点名要求 Z)。Redis 用 Simple Icons 官方
  菱形堆路径。
- **操作者修订(实施中)**:MySQL 不要字样 —— Simple Icons 的 mysql 是海豚+字标的整版
  锁定稿,换成 devicon 的 mysql-original(纯海豚,MIT,128 网格);**新增 mariadb 类型**:
  类型进 DIRECT_FIELDS 与 adapter 分发(复用 MysqlEngine,MySQL 线协议),面板表单为 mysql
  字段集的逐字副本,图标用 Simple Icons 官方海豹(sea lion)。
- `util.js` 增 `TYPE_ICONS`(tag → sprite 名)与 `typeTagHtml(tag)`:有映射返回 `icon(name, tag)`
  (aria-label 带原词,读屏不丢信息);无映射返回转义文本 —— 即现状 chip,样式零改动。
- `menu.js` patch pass 的 `.side-type` 改写 innerHTML(替代 textContent);`data-tag` 属性保留。
- tag 词表 = 服务端 `tag_of` 的产出:非 proc 是类型名,proc 是命令首词 —— 任意词都可能出现,
  所以映射是白名单,白名单外一律文本兜底(echo、node、python …)。
- 图标 chip 收紧内边距(`:has(.ic)`),wash 背景保留 —— 仍是"launch method"这个信息级的同一种 chip。
- 详情页副标题、Config 页的类型行保持文本(有空间,文字更准确);只动行尾 chip。

## 商标说明

单色简化重绘 + CC0 路径,非嵌入品牌素材;用于标识"连接的是什么",指称性使用,同 `i-mcp` 的处理。

## 测试

- vitest:`typeTagHtml` 白名单命中(含 aria-label)、未命中回退文本、TYPE_ICONS 键位钉死。
- Rust 无改动(纯面板 + sprite)。
- 19997 CDP 实机:各类型行图标可见、主题切换、echo/未知词仍文本。

## 交付

一次提交:docs/29 + sprite + util + menu + css + vitest + mariadb 后端(类型表/分发/E2E)。
