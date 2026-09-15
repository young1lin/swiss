# 24 — http MCP 的 OAuth 授权（含 Figma 远程 MCP 接入）：随时可发起的认证

> 需求原文（用户，2026-09-15）："我想做一个 figma 的 MCP"；"我记得它是假装 Claude Code 客户端，然后能实现 Figma 的 MCP 的，我这里好像还要加个随时可以认证授权的机制"。
>
> 调研结论（2026-09-15 实测 + 两个参考实现，已克隆 `<vendor>`）：Figma 官方远程 MCP
> （`https://mcp.figma.com/mcp`）走 OAuth 2.1 + RFC 7591 动态客户端注册（DCR），且**注册端点按
> 精确 `client_name` 白名单放行**——`"Claude Code"`、`"Codex"` 返回 200，其余名字一律 403
> （hermes-agent `tools/mcp_oauth.py` 实证；mcp-remote issue #186 同结论）。第三方客户端
> （Qoder〔用户原文拼写〕/Hermes/pi）接入的"伪装"就是 DCR 请求体里报 `client_name:
> "Claude Code"`。Figma 另有一个怪癖：DCR 响应返回 `client_secret` 而（hermes 观测时）元数据声明
> `auth_method=none`，token 交换却强制要求 secret——必须按 `client_secret_post` 把 secret 放 body。
> 参考：`<vendor>/hermes-agent`（`tools/mcp_oauth.py` 的 provider 默认值与人话错误、
> `tools/mcp_dashboard_oauth.py` 的面板授权桥）、`<vendor>/mcp-remote`
> （`src/lib/node-oauth-client-provider.ts`，最完整的 TS OAuth 客户端：发现/DCR/PKCE/刷新单飞）。

## 0. 现状与缺口（为什么是它、为什么是现在）

- **http 适配器只有静态 headers**（`crates/swiss-mcp/src/adapters/http.rs`，${ENV}/`secret://` 引用
  展开后逐请求带上）。托管 MCP 端点（Figma/Linear/Sentry/Atlassian…）要求 OAuth 2.1；Figma 连静态
  token 选项都没有——论坛至今还在求 PAT（forum.figma.com "let a static token authenticate"）。
- **Figma 特有的门槛**是 DCR 白名单（上）：不"报对名字"，流程在浏览器打开前就死了。
- **用户点名要"随时认证授权"**：token 过期、失效、换号之后，从面板任意时刻一键重授权，而不是
  改配置文件重启。
- loopback-only 边界不受影响：OAuth 回调监听绑 127.0.0.1（本来就是 loopback）；授权 URL 由面板在
  **用户的浏览器**里打开——gateway 不 spawn 浏览器（AGENTS：No subprocess where a syscall exists，
  开浏览器连 syscall 都不需要）。

## 1. 目标与非目标

**目标**

- http MCP 配置面新增 `auth: "oauth"`：gateway 完成 OAuth 发现（RFC 9728/8414）、DCR（RFC 7591，
  带 provider 默认值）、PKCE S256 授权码流程（loopback 回调）、token 交换与刷新。端点全部经发现
  取得，代码里不硬编码任何 Figma URL。
- Figma 开箱即用：provider 默认值表命中 `mcp.figma.com` → `client_name: "Claude Code"`（def 可
  覆盖，如 `"Codex"`）、scope `mcp:connect`、token 端点 `client_secret_post`。
- **随时授权**：`POST /api/mcps/:name/authorize` 发起 flow 并返回授权 URL；GET 同路径轮询状态；
  完成即落盘 + 自动拉起该 MCP；面板详情页 Authorize 按钮与状态徽标。
- 凭据纪律（docs/19 延伸到 OAuth）：access/refresh token、client_secret、code、verifier、state
  永不进 managed.json、面板响应、日志、错误文本。
- 零新依赖：PKCE S256 = SHA-256（sha2 已在）+ base64url（base64 已在）+ 随机（rand 已在）+ HTTP
  （reqwest 已在）。
- `swiss export`/`import` 打通（refresh token 顺带迁移；换机重新授权也是合法路径）。

**非目标**

- 不做 device flow、client_credentials、jwt-bearer grant（mcp-remote 都有；本仓库用不上）。
- 不做通用 provider 配置 UI：默认值表在代码里，今天只有 Figma 一行。
- 不做 PAT 版 figma REST 适配器——那是另一条路线（官方 REST API + personal access token），
  需要时另开 spec，不与 OAuth 混在一个 def 类型里。
- 不做多账号：每个 MCP 注册名一份凭据。
- 不做 token reveal / 过期提醒 / 自动重授权：刷新失败就停下来等人点按钮——浏览器登录是人的
  动作，自动化它只会把人挡在流程外。
- 不给 proc/rest 加 OAuth：rest 的 API key 走 headers 引用已够；proc 子进程自己的 OAuth 归它自己。

## 2. 设计

### D1 配置面与状态文件

- def：`{"type":"http","url":"https://mcp.figma.com/mcp","auth":"oauth"}`；可选 `oauthClientName`
  覆盖 provider 默认 client_name（值必须是目标 AS 放行的名字，如 `"Codex"`）。
- 密封状态文件 `mcp-oauth.json`（`statefile.rs` 同款封印，docs/05 格式不动），按 MCP 注册名键控：
```json
{ "figma": { "client_id": "…", "client_secret": "…", "access_token": "…",
             "refresh_token": "…", "expires_at": 1760000000, "scope": "mcp:connect", "at": 1760000000 } }
```
- 为什么不放密钥库（docs/19 的 vault）：vault 是操作者手存的名字→值，"值只进不出、忘了重存"是它的
  人机契约；OAuth 凭据是**机器轮换**的结构化运行时状态——用户手滑覆盖一个 access_token 会打断整条
  刷新链。独立文件、同封印、同设备绑定，互不越界。

### D2 协议流程（端点全经发现；括号内为 Figma 实测值，2026-09-15）

1. `GET {resource origin}/.well-known/oauth-protected-resource`（RFC 9728）
   → `authorization_servers[0]`（Figma：`https://api.figma.com`；同响应还带
   `scopes_supported:["mcp:connect"]` 与 `resource` 字段，后者进 RFC 8707 resource 参数）。
2. `GET {as}/.well-known/oauth-authorization-server`（RFC 8414）→ authorization_endpoint
   （`https://www.figma.com/oauth/mcp`）、token_endpoint（`https://api.figma.com/v1/oauth/token`）、
   registration_endpoint（`https://api.figma.com/v1/oauth/mcp/register`）、
   `code_challenge_methods_supported:["S256"]`、`require_state_parameter:true`。
3. **DCR**：POST registration_endpoint，body：
```json
{ "client_name": "Claude Code", "redirect_uris": ["http://127.0.0.1:{port}/callback"],
  "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
  "token_endpoint_auth_method": "client_secret_post", "scope": "mcp:connect" }
```
   - **每次 authorize flow 重新注册**：loopback 端口每次随机，而 DCR client_id 绑定注册时的
     redirect_uri——旧 client_id 配新端口会被 `redirect_uri does not match` 拒绝（hermes
     `mcp_oauth.py` 同结论）。新 client_id 落盘时旧 token 作废（见 D6）。
   - 403 → 人话错误："'figma' 的注册端点只放行特定 client_name（如 \"Claude Code\"、\"Codex\"），
     可用 oauthClientName 覆盖"（hermes `humanize_oauth_registration_error` 同款）。
   - Figma 怪癖：DCR 响应可能带 `client_secret`（hermes 观测时元数据声明 `none`；本日 AS 元数据列
     的是 basic/post）。一律保存 secret；token 请求若注册响应给了别的 auth method，按响应的走，
     默认 `client_secret_post`。
4. 授权 URL（**面板**打开）：authorization_endpoint + `response_type=code`、`client_id`、
   `redirect_uri`、`scope`、`state`（32 字节随机，强制——Figma require_state）、
   `code_challenge`/`code_challenge_method=S256`、`resource={resource}`（RFC 8707）。
5. 回调：flow 内起一次性监听 `127.0.0.1:0`（随机端口，与 DCR 注册端口一致），`GET /callback`：
   校验 `state`（constant-time）；有 `code` → 200 小页（"已授权，回到面板"）；有 `error` →
   4xx + 原因。flow 结束监听即拆，不留任务。
6. token POST（`application/x-www-form-urlencoded`）：`grant_type=authorization_code`、`code`、
   `code_verifier`、`redirect_uri`、`client_id`、`client_secret`（body）、`resource` →
   access/refresh/expires_in；`expires_at = now + expires_in` 落盘。
7. 刷新：`grant_type=refresh_token`、`refresh_token`、`client_id`、`client_secret`、`resource`；
   `invalid_grant`/`invalid_client` → 清凭据 → needs-auth。

### D3 provider 默认值表（代码内，host 匹配）

| host 含 | client_name | scope | token 端点 auth |
|---|---|---|---|
| `mcp.figma.com` 或 `figma.com/mcp` | `"Claude Code"` | `mcp:connect` | `client_secret_post` |
| 其余 | `"swiss"` | 发现的 `scopes_supported`（空则省略） | 同上 |

- 判定同 hermes `_is_figma_remote_mcp`（URL 含即命中；名字含 figma 且 URL 为空/含 figma 也命中）。
- def `oauthClientName` 永远赢过默认值（操作者唯一的覆盖面，刻意小）。

### D4 http 适配器接入（http.rs 的接缝）

- `RemoteMcpClient` 注入可选 Bearer：`apply_headers` 在 def headers 之后加
  `Authorization: Bearer <access>`（协议头 session/version 仍最后，保证路由优先）。def 手写
  `Authorization` 且 `auth=oauth` → 构造期拒绝（配置错误宁可启动失败，不静默叠加）。
- `build()/connect()`：`auth=oauth` 时先取凭据——access 未过期（60s 裕量）→ 直连；过期有
  refresh → 刷一次再连；无凭据/刷新失败 → start error `needs authorization`（registry 既有错误
  通道呈现，面板据此亮按钮）。
- 请求路径 401：刷一次 → 重试原请求一次；再 401 → 内存标 needs-auth + 清凭据落盘，错误上浮措辞
  `needs authorization`。
- 刷新单飞：tokio `Mutex` 护 refresh——并发 401 共享一次刷新（mcp-remote in-flight refresh 同款），
  刷出的新 token 对后来者直接复用。

### D5 随时授权：API 与面板

- `POST /api/mcps/:name/authorize`：起 flow（同名同时仅一个，进行中再 POST → 返回既有 flow 状态），
  DCR 完成后立即返回 `{flowId, status:"authorization_required", authorizationUrl}`。面板
  `window.open(authorizationUrl)`（不 spawn 浏览器；尊重弹窗拦截，同时显示可点链接）。
- `GET /api/mcps/:name/authorize`：`{status: starting|authorization_required|approved|error,
  error?, toolsCount?}`；approved = token 落盘 + 该 MCP 自动 start（若 enabled）+ tools/list 数。
- flow 上限 5 分钟等回调；超时/交换失败 → error。错误文本过 D7 脱敏。
- `/api/mcps` 列表项与 detail 增加 `oauth: "authorized"|"needs-auth"|null`（附 `expiresAt`）；
  面板：http+oauth 的 MCP 详情头部徽标 + Authorize 按钮（needs-auth 时醒亮）；授权中 3s 轮询 GET。
- add-sheet/编辑表单（fields.js）：http 类型加 `auth` 开关（"Use OAuth (browser login)"）；勾选后
  出现可选 `oauthClientName`（placeholder：`Claude Code`）。

### D6 并发与重入

- 同名 authorize flow 单飞；授权期间该 MCP 的调用照旧（旧 access 一直用到新凭据**原子替换**落盘）。
- client_id 变更（新 flow 新 DCR）→ 先落新 client 再清旧 token，不留半态（hermes
  `_invalidate_tokens_on_client_change` 同款）。

### D7 日志与脱敏纪律

- access/refresh token、client_secret、code、code_verifier、state 不进日志、错误文本、API 响应、
  calls.rs 调用记录。OAuth 的 HTTP 交换（发现/DCR/token）不进 MCP 调用日志——那是工具调用的日志，
  不是传输凭据的日志。
- 错误措辞模板（测试锁死）：
  - 注册被拒：`figma: OAuth registration refused (HTTP 403) — the server only accepts specific client names; try oauthClientName "Claude Code" or "Codex"`
  - 待授权：`figma: needs authorization — open the panel and click Authorize`

### D8 内存与生命周期

- 无常驻任务：flow 对象只在授权窗口存在（≤5 分钟），回调监听随 flow 拆；token 表 O(MCP 数)；
  密封文件读写只在凭据变更时发生。空闲成本预期为零增量（swiss-memory-record 实测记档）。

## 3. 实施顺序与门禁

测试先行（改前红、改后绿）；每阶段 `cargo test --workspace` +
`cargo clippy --workspace --all-targets -- -D warnings`；面板改动 vitest 全量 + 19998 实测。

- **Phase 1（oauth core，`swiss-mcp/src/oauth.rs`）**：发现/DCR/PKCE/交换/刷新；**假 AS 集成测**
  （本地 axum 扮演 protected-resource + AS 元数据 + DCR + token 端点，内置 Figma 怪癖副本：
  client_name 白名单 403、DCR 发 secret、token 端点校验 client_secret_post、require_state、
  S256 校验、RFC 8707 resource 校验）；state 读写单测（封印往返、原子替换、client 变更清 token）。
- **Phase 2（http 接入）**：Bearer 注入 + 401→刷新→重试集成测（假 AS + 假 MCP 端点：无凭据
  needs-auth、有凭据直连、过期自动刷、刷新失败 needs-auth、def 手写 Authorization 拒绝）。
- **Phase 3（admin API + 面板）**：authorize 两路由 oneshot 集成测（flow 生命周期、单飞、错误措辞、
  响应全文无 token 值）；fields.js/add-sheet/detail vitest（OAuth 开关、按钮状态机、轮询、脱敏）。
- **Phase 4（收尾）**：export/import 加段 + 回归；`scripts/test-instance.ps1` 的 `$StateFiles` 加
  `mcp-oauth.json`；19998 按 §4 实测；ADR 入 docs/07；memory 记录（预期零增量）。

## 4. 验收清单（19998，全部要过；真实 Figma 授权一步不缺）

1. 面板添加 figma（http + auth oauth）→ 列表 `oauth: needs-auth`，详情亮 Authorize。
2. 点 Authorize → 浏览器完成 Figma 登录授权 → 回调页提示 → 面板 approved，MCP started，
   tools/list 出 Figma 工具集（以当日实数为准；hermes 2026-07 实测 26 个）。
3. managed.json / 面板响应 / 日志全文搜不到 token；`mcp-oauth.json` 原始字节非明文（封印）。
4. 重启 19998 实例 → figma 直接 authorized（凭据持久）。
5. 使 access 过期（等或改 expires_at）→ 任一工具调用自动刷新成功。
6. 作废 refresh token（改坏）→ 调用失败 reason `needs authorization`，按钮重现；重新 Authorize
   一键恢复。
7. 假 AS：DCR 白名单 403 → 人话错误（集成测锁措辞）。
8. `swiss export` 含 oauth 段；import 后重启凭据照常解析。

## 5. 不做什么（重申）

device flow、client_credentials、通用 provider UI、PAT REST 适配器、token reveal、自动重授权、
多账号、非 loopback 回调、gateway 侧开浏览器、新依赖。

## 6. 交给实施模型的 Prompt

按 docs/24 实施。核心事实：Figma 远程 MCP（`https://mcp.figma.com/mcp`）的 DCR 按**精确
client_name 白名单**放行（"Claude Code"/"Codex" 200，其余 403），DCR 响应带 client_secret 而
token 交换必须 `client_secret_post`；端点一律经 RFC 9728/8414 发现取得，不硬编码。配置面是
http def 的 `auth: "oauth"` + 可选 `oauthClientName`；凭据存密封文件 `mcp-oauth.json`（按 MCP
名键控，与 docs/19 vault 分立）；"随时授权" = `POST/GET /api/mcps/:name/authorize` + 面板 Authorize
按钮 + flow 状态轮询。
先读锚点：`crates/swiss-mcp/src/adapters/http.rs`（RemoteMcpClient/apply_headers/connect 的接缝）、
`crates/swiss-mcp/src/adapters/mod.rs`（make_adapter）、`crates/swiss-mcp/src/registry.rs`
（start error 通道）、`crates/swiss-core/src/secure/statefile.rs`（封印读写）、
`crates/swiss-panel/src/admin_assets/js/fields.js|add-sheet.js|detail.js`（表单与详情）、
docs/19（凭据纪律）、docs/08（测试模式）。参考实现（只读）：`<vendor>/hermes-agent`
`tools/mcp_oauth.py`（provider 默认值/人话错误/client 变更清 token）与
`tools/mcp_dashboard_oauth.py`（flow 状态机：starting → authorization_required → approved/error）、
`<vendor>/mcp-remote` `src/lib/node-oauth-client-provider.ts`（PKCE/DCR/刷新单飞的完整对照）。
按 §3 四阶段推进，每阶段一提交、测试先行、门禁全绿才进下一阶段；偏差记录进提交信息与最终汇报。
集成测试走本地 axum 假 AS + tower oneshot，不起真端口、不碰真 Figma；面板测试沿用 fake-DOM 契约
测试。19998 实测按 §4 清单逐步留证；**19999 不动**。
