# docs/31 — MCP Logs 调用日志搜索

提出:操作者(2026-09-16)"redis 的我要搜索 GET 的,能给我出来这个"。worktree mcp,测试 19997。

## 问题

Logs 页(Tool calls)只能按页翻。想找"最近哪些调用带 GET"只能肉眼扫。

## 方案

**服务端过滤**(仓库先例:read_tool_history 的 `q` —— 对完整存储内容匹配,不信任裁剪后的行预览):

- `GET /api/mcps/{name}/calls?page=N&q=TEXT`:q 非空时全量扫描该 MCP 的调用索引(半年级留存,有界),
  保留**工具名 / 完整参数 / 存储的回复文本**任一含 TEXT(大小写不敏感)的条目,再按 page/pageSize 分页;
  `more` 相对过滤后序列计算。q 为空时路径与现状完全一致(tail 定长读)。
- 匹配面 = 页面行上看得见的内容;被裁剪只留预览的回复,匹配的是预览(诚实,不假装搜过全文)。

**面板**:

- sec-head 加搜索框(`#callsQ`,type=search,placeholder "Search calls");输入防抖 300ms 后带 q 重载并回到第 0 页;
  Escape 清空。
- `renderCallsOnly` 的重绘签名加入 callsQ,保证过滤变化必然重绘;重绘时**把旧输入框节点换回新标记**,
  保住焦点与光标(结果回来不夺焦)。
- 空态文案区分:"No calls matching \"X\"." vs 原"还没有调用"。

## 测试

- Rust:q 跨 tool/args/output 的大小写不敏感匹配;过滤后分页与 more;q 空走原路径。
- vitest:logsBody 渲染搜索框(带当前值)、带 q 的空态文案。

## 交付

一次提交:docs/31 + calls.rs + adminapi.rs 路由 + logs.js/detail.js/run-history.js + base.css + 测试。
