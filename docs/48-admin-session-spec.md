# 48 — 管理面登录：每次启动的一次性登录令牌 + 会话 cookie

> 状态：已实施（2026-09-28）。需求原文（用户）："确保这个项目能够正确开源，仅能本地使用，并且之后，
> 每次后台启动，会默认像 RH 那样，注入每次启动时生成的 Token … 如果你有更好的方案，你来决定"。
> 代码注释与 UI 文案一律英文；文档散文中文。

## 1. 问题

`/api/*` 与面板此前**没有任何凭据**：边界只有回环绑定、对端地址、`Host` 与 `Origin` 四道检查
（`swiss_host::local_only`）。它们挡得住别的机器和 DNS rebinding 网页，挡不住**本机上任意进程**——
一条 `curl http://127.0.0.1:19999/api/tunnels` 就能读出全部连接、MCP 定义、远程目标，并能执行远程命令。
本机其他 OS 用户、容器里的进程（WSL mirrored 网络下 Windows 回环可达）都在其中。

## 2. 参照：RH（reference harness）

RH 的 Web UI：每次宿主进程启动生成随机令牌，只打印一次 `http://127.0.0.1:3080/?token=…`；只有
`GET /?token=` 能把它换成浏览器 cookie 并重定向到干净的 `/`；API 路径与 `Authorization` 头**从不**
接受这个令牌；签名密钥持久化，所以普通重启后已登录的浏览器不掉线。

## 3. 方案（在 RH 之上的两处改进）

| 部件 | 做法 |
| --- | --- |
| 登录令牌 | **一次性、120 秒过期**，而不是"进程生命周期内有效"。`swiss start` 启动后立刻铸一张并打开浏览器（即"注入"），`swiss open` 随时再铸一张。泄露在终端回滚、浏览器历史里的链接用过即废 |
| 兑换 | 只有 `GET /?token=<t>`：有效 → `303` 到 `/` 并下发 cookie；无效/已用/过期 → 401 锁屏页 |
| 浏览器会话 | cookie `swiss_session_<port>`（端口入名：19998 与 19999 同在 127.0.0.1，cookie 不按端口隔离），值 `v1.<签发毫秒>.<随机>.<HMAC-SHA256>`，`HttpOnly; SameSite=Strict; Path=/; Max-Age=30 天` |
| 签名密钥 | 32 字节，存于 `SWISS_HOME/session.json`（与其他状态文件同一密封信封：主密钥 DPAPI 保护，AES-256-GCM），**跨重启保留** |
| CLI 凭据 | 同一 `session.json` 里的 `cliKey`，**每次守护进程启动轮换**。CLI 读密封文件、以 `X-Swiss-Key` 头发送。只有同机同用户能解封 |
| 受保护面 | 所有 `/api/*`（两棵路由树、终端 WebSocket 升级）。`/` 无有效 cookie 时回锁屏页；`/admin/*` 静态资源、`/health` 不设防（无数据）；`/mcp/*` 仍由各客户端 bearer 令牌把守，不变 |
| 进程内请求 | 没有 `ConnectInfo` 的请求（测试经 `oneshot` 直驱路由）按"本地"放行——与回环守卫同一约定；真实监听一律 `into_make_service_with_connect_info` |
| 失败即关 | 带 `ConnectInfo` 的请求到达而会话未装配（`AppContext.session` 空）→ 401，绝不放行 |

令牌铸造：`POST /api/session/ticket`（本身受保护：CLI 用 `cliKey`，已登录浏览器用 cookie）→
`{ "token", "url" }`。未兑换的令牌最多保留 16 张，过期即清。

## 4. 面板

`api()` 收到 401 → `location.assign("/")`，由服务器回锁屏页。锁屏页是服务器内嵌的一小段中英双语
HTML，只说一件事：在终端运行 `swiss open`。

## 5. 不做什么

- 不做用户名/密码：这是单用户本机工具，凭据就是"能以本用户身份解封 `session.json`"。
- 不做"登出"：关掉浏览器 cookie 或删 `session.json`（签名密钥随之重生，所有会话失效）即可。
- 不改变回环规则：会话是**叠加**在四道本地检查之上的，不替代它们。
