# 24 — MCP 路径领地:实施交接提示词

> 本文件是给**全新实施 session** 的自足提示词。写 spec 的 session 不实施(仓库纪律)。按顺序读、按顺序做。

## 工作目录

<repo>(如需隔离,建 worktree;直接在 master 上做也可,一 commit 一条)。

## 任务一句话

MCP 端点从根级 `/{name}` 迁到 `/mcp/{name}`(P1),主机保留字两份退役(P2),面板 URL 单点改(P3),
旧地址 404 带"已迁移"提示(P4,可砍),文档与台账(P5)。规格即 docs/24,ADR-018 已记录决策,不要重新讨论。

## 先读(顺序)

1. `AGENTS.md` —— 规则高于一切;注意 19999 是生产、测试实例走 19998/19997、`--workspace` 不可省。
2. `docs/24-mcp-path-domain-spec.md` —— 全文;§0 的行号坐标是基线 `83bc83c` 的,按函数名找。
3. `docs/07-decisions.md` 的 ADR-018 —— 决策已定:硬切、无别名、领地内无保留字。
4. `src/app.rs` 的 `build_app` / `mcp_post` / `mcp_delete` / `fallback_404`。
5. `src/adminapi.rs` 的 `RESERVED` 与名字校验;`crates/swiss-mcp/src/mcp_import.rs` 的 `RESERVED` /
   `unique_name` / `is_gateway_url`(最后这个零改动,别碰)。
6. `crates/swiss-panel/src/admin_assets/js/connect.js` 的 `endpointUrl` —— 面板唯一 URL 出口。
7. `.agents/rules/panel-proof-of-life.md` —— P3 的完成标准(vitest 只是入场券,真浏览器才算)。

## 交付顺序(一 commit 一条,每条先写红测试)

P1 路由迁移 → P2 保留字退役(P2 依赖 P1 的可达性断言)→ P3 面板(可与 P2 并行)→ P4 迁移提示 →
P5 文档。每个 commit 的信息写清前因后果(参考 8832fa6 的样式)。

## 门禁(每条 commit 前,不省略)

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d    # 本改动不应出现任何新重复依赖
```

P3 另加:`node --check` + `crates/swiss-panel/panel-tests` 全量 vitest,然后按
`scripts/test-instance.ps1` 起临时实例(19998),真浏览器逐项点击(panel-proof-of-life 清单)。

## 红线

- 19999 全程不动;部署仅当用户明说,且只走 `scripts/deploy.ps1`。
- 任何实例不用 `--port`/`-p`(会写进 config);环境变量 `SWISS_PORT`/`SWISS_HOME` 才是正道。
- 不引入别名路由 —— 用户已拍板硬切;想改主意,回去找用户,不要自己加。
- 不动 `is_gateway_url`(按端口识别自身,与路径无关);不动密封格式;不加依赖。
- 代码注释一律英文;面板 UI 文案英文;文档散文中文。
- 收尾时给用户列一份**需要改 URL 的客户端清单**(面板 connect sheet 可逐个抄)。

## 完成定义

全部条目合并、门禁绿、19998 真浏览器走查过、docs/08 台账同步、用户拿到客户端迁移清单。
实施 session 完成后走 swiss-verify → swiss-live-verify → swiss-review 流程。
