# docs/30 — PostgreSQL 表单拆解(连接 URL 分字段编辑)

状态:操作者当面提出,直接实施(2026-09-16)。worktree mcp,测试端口 19997。

## 问题

pg 类型的配置是**一条连接 URL**(postgresql://user:pass@host:port/db?sslmode=…)全部挤在
一个文本域里:改个 IP、换个密码,要在一长串里找位置,密码明文混在其中。

## 方案(纯面板,def 线格式不变)

- def / API / 引擎**照旧持有 url 串**(DIRECT_FIELDS 不动,mask/unmask 机制不动)。
- 表单拆为:host / port / user / password / database / Options(k=v 每行)+ 原有的
  description / maxRows / autostart。
- **打开编辑时**解析 url → 各字段;**保存时**重组回 url。四个出口全走同一对
  parse/serialize:saveEdit、saveReplace、submitAdd、runConnTest(连接测试也按分字段提交)。
- 引用与哨兵逐字搬运:
  - `${ENV_VAR}` / `${secret://name}` 可出现在任何字段(密码最常用)——解析前先用占位符
    摘出 ${...}(避开 secret:// 里的冒号斜杠),重组时还原;
  - 服务端打码哨兵 `••••••••`(url 内密码)原样进密码框、原样回传,PUT 时由既有
    unmask_url 从存量 def 还原——"只改 IP 不碰密码"天然成立。
- 解析失败兜底:极少数不合语法的 url,表单退回整条 url 文本域(`__pgRaw`),保存原样
  提交——宁可旧体验也不丢数据。
- 显示页(非编辑)保持现状。

## 测试

- vitest:parse/serialize 往返(全字段、无密码、无端口、哨兵、${secret://} 引用、
  多行参数)、translatePg(组装并删分键;mysql 不动;__pgRaw 直通)。
- 19997 实机:shop(pg)编辑表单分字段可见;改 host 保存 → def url 其余原样。

## 交付

一次提交:docs/30 + fields/detail/run-history/add-sheet + vitest。
