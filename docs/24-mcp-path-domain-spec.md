# 24 — MCP 路径领地：/mcp/<name>

> 状态：**已实施**（2026-10）。P1 路由迁移 `7defb84`、P2 保留字退役 `e0f6ab5`、P3 面板 URL `f7eae76`、P4 迁移提示 `57bc760`、P5 文档台账收尾。基线 `edb6018`。配套交接提示词：[24-mcp-path-domain-prompt.md](24-mcp-path-domain-prompt.md)。
> 决策记录：[ADR-018](07-decisions.md)（路径空间划分 + 硬切 + 领地内无保留字）。
> 前置阅读：`AGENTS.md`（规则高于本文）、[docs/09](09-toolbox-plugin-architecture.md)（插件契约——本文是它的路径面推论）、
> [docs/05](05-wire-compatibility.md)（本文不动密封格式；客户端 URL 是另一类契约，见 §0）、
> [docs/08](08-testing.md)（测试纪律）。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-10）："mcp 的地址，应该都要改成 localhost:19999/mcp/redis 这样的地址，因为如果之后
> 新加其他 plugin，我这个就不会受这个影响。看看，这个改动大不大，要怎么改。"
> 同日确认：(1) 旧地址兼容——用户选**硬切**（不设别名路由）；(2) 领地内命名——用户原话："不是，/mcp 表示
> 属于 mcp 这个 plugin 的 domain 的内容，/mcp/redis、/mcp/mysql 想怎么取名，都是这个 plugin 说了算啊"
> ——即 `/mcp/*` 是 MCP 插件领地，领地内**不设主机保留字**（连现有的 api/health/admin 三个也退役）。

## 0. 现状与缺口（为什么是它、为什么是现在）

> 本节的行号是基线 `edb6018` 时的证据坐标，实施后按函数名找，不按行号找。

- **MCP 端点是根级单段 catch-all**：`src/app.rs` 的 `build_app` 挂 `route("/{path}", post(mcp_post).delete(mcp_delete).get(fallback_404))`。
  根路径因此被 MCP 家族占住：未来任何想认领根路径的插件（一个终端网页、一个 webhook 入口）都要跟 MCP
  名字搏斗，或者反过来把某个 MCP 挤下线。这正是用户要消除的相互影响。
- **为防撞名，主机保留字硬编码了两份**：`src/adminapi.rs` `RESERVED = ["api","health","admin"]`（建/改名拒绝）
  与 `crates/swiss-mcp/src/mcp_import.rs` 同名常量（导入改名/skip）。两份手工同步，且每在根上加一条路由
  都要记得回来加保留字——这是一颗持续计时的雷。
- **面板构造客户端 URL 只有一处**：`crates/swiss-panel/src/admin_assets/js/connect.js` 的
  `endpointUrl(name) = location.origin + "/" + name`；claude/codex/.mcp.json 三种片段与 Copy URL 按钮全部经它。
- **导入器无需改动**：`mcp_import.rs` 的 `is_gateway_url` 只按回环主机 + 端口识别"已指向本网关"的条目
  （防导入自代理），与路径无关。旧形状、新形状的客户端条目识别行为完全一致。
- **CLI 不打印 MCP 端点 URL**（`src/bootstrap.rs` 只构造面板地址）；全仓唯一 URL 出口就是 connect.js。
- **测试面**：`tests/adminapi.rs`（39 处）、`tests/app.rs`（8 处）、`tests/memory.rs`（2 处）直接 POST 根级
  MCP 路径，随路由迁移机械更新。

**宪法核对**（AGENTS.md 四属性）：不碰 loopback 边界、密封格式、RawValue 转发、内存预算（一条路由模式改写，
零常驻成本）；与 **Plugin-shaped** 同向——本文就是把"MCP 的地盘"从根上收进它的前缀，让根成为未来插件的空地。
无属性被交易。

## 1. 条目与验收（P1→P5，一 commit 一条，每条红→绿）

### P1 路由迁移：`/mcp/{name}` 是唯一 MCP 端点形状

`src/app.rs`：`"/{path}"` → `"/mcp/{path}"`。POST/DELETE 语义、鉴权顺序（401 先于一切）、
`GET → fallback_404` 的现状全部原样平移；`mcp_post`/`mcp_delete` 的 Path 提取不变。
根级单段路径从此**没有路由**——旧形状 POST `/redis` 落进 `fallback_404`（`{"error":"no route for POST /redis"}`）。

验收（新增，实施前红）：
- `new_path_serves_the_endpoint`：POST `/mcp/echo`（bearer + initialize）走通既有断言；
- `old_root_path_is_no_longer_an_mcp_endpoint`：POST `/echo` 得 404 JSON error 形状；
- 既有 49 处路径调用点机械更新后全绿；`Unknown MCP path` 503 的报错文案把名字带全
  （`/mcp/nope` 仍报 `Unknown MCP path: nope`——名字本身，不带前缀）。

### P2 保留字退役：领地内名字由插件定

删 `adminapi.rs` 与 `mcp_import.rs` 的两份 `RESERVED`；`unique_name` 的占用判定只剩 `taken`。
`NAME_RE`（字符集与长度）不动——那是路径安全，不是撞名防御。改名动词（rename）同步不再查保留字。

验收（新增，实施前红——旧代码拒绝建名）：
- `plugin_domain_names_need_no_host_reservation`：经 API 建名为 `health`、`api`、`admin`、`mcp`
  的 MCP 全部成功，且 POST `/mcp/health` 真实可达（可达性断言依赖 P1，故 P2 排在 P1 后）；
- 导入测试：`.mcp.json` 里名为 `health` 的条目**不再被改名**（`wanted == name`）。

### P3 面板：所有片段只出 `/mcp/` 地址

`connect.js` `endpointUrl` 改为 `location.origin + "/mcp/" + name`。三片段 + Copy endpoint URL
全部经此一处，无其他改动。vitest（`panel-tests/`）快照断言同步。

验收：vitest 全绿 + **panel-proof-of-life 规则**（`.agents/rules/panel-proof-of-life.md`，真浏览器、19998、
真实点击）：详情页 Copy endpoint URL 出 `/mcp/` 地址，connect sheet 三种片段文本逐字核对。

### P4 旧地址 404 的迁移提示（推荐项，可砍）

`fallback_404` 无状态；实现注记：在根级显式挂一条带 State 的 404 处理（或让 fallback 取 state），
当方法是 POST/DELETE、路径是单段、且名字存在于 registry 时，404 body 指明
`moved to /mcp/<name> — update the client URL`。硬切后用户改自己客户端配置时的自助排错线索。

验收（新增，实施前红）：POST `/redis`（redis 是已注册 MCP）→ 404 body 含 `/mcp/redis`；
POST `/nope`（未注册）→ 普通 404 形状不变。

### P5 文档与台账

AGENTS.md 首段"every MCP server on an HTTP path under 127.0.0.1:19999"改为"under the `/mcp/` path
prefix"；docs/02 路由图补前缀；docs/04 加一行 Node 形状（`/{name}`）的历史注记；docs/07 落 ADR-018；
README docs 表加本文行；docs/08 台账按实际新增测试数同步。

## 2. 门禁与交付顺序

```bash
cargo test --workspace          # 唯一合法的"套件通过"
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d                   # 本改动不应引入任何依赖
```

P1 → P2（P2 的可达性断言依赖 P1）→ P3（可与 P2 并行）→ P4 → P5。P3 完成后按
`scripts/test-instance.ps1` 起临时实例（19998/19997），**19999 全程不动**；部署走 `scripts/deploy.ps1`，
仅当用户明说上线。

## 3. 明确不做（out of scope）

- **别名路由**——用户拍板硬切。不写 `/{name}` 兼容层，不写弃用期。
- **把 mcp_routes 挪进插件 descriptor 路由**——docs/09 的更远方向（MCP 插件自报路由），本文只做前缀
  划分，路由仍由组合层挂载。记录方向，不实施。
- **导入器逻辑**——`is_gateway_url` 按端口识别，与新路径无关（§0 已核）；其测试原样保留。
- **其他 API 路径**——tunnels/jobs/db/terminal 全部已在 `/api/*` 下，与根无关。
- **重写 docs 里叙述 Node 时代 URL 的历史段落**——ADR-016 的原则：历史是出处，不是待改的墙。

## 4. 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| 硬切后用户既有客户端配置静默断连 | P4 的 404 提示给出确切新地址；面板 connect sheet 即抄即用；实施 session 收尾时列出用户需改的客户端清单 |
| 未来有人在根上认领路径时忘了这段历史 | ADR-018 写明根的归属规则：根 = host chrome（`/`、`/admin`、`/health`、`/api/*`）+ 未来插件认领；插件领地一律 `/<plugin-domain>/*` |
| 两份 RESERVED 删除后有人想加回 | P2 的验收测试就是防倒退：建 `health` 成功且可达，加回保留字必红 |
