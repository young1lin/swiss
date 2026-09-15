# 27 — SSH 隧道:代理拨号(HTTP CONNECT / SOCKS5)与跳板引用

> 状态:**待实施**(spec 定稿 2026-09-15;基线 `a0fed265`;实施从
> [27-ssh-proxy-and-jump-prompt.md](27-ssh-proxy-and-jump-prompt.md) 起步,走 swiss-add-plugin →
> swiss-verify → swiss-live-verify → swiss-review 流程)。
> 前置阅读:`AGENTS.md`(载重规则高于本文)、[docs/05](05-wire-compatibility.md)(本文不动密封格式)、
> [docs/07](07-decisions.md) ADR-007/ADR-019、[docs/19](19-secret-vault-spec.md) D4(连接边界的严格凭证合同)、
> [docs/25](25-vault-ref-envelope-spec.md)(`${...}` 信封语义)、[docs/09](09-toolbox-plugin-architecture.md)(插件边界)、
> [docs/14](14-terminal-plugin-spec.md) §4(新增 SSH 能力不引新依赖的先例)。
> 代码注释与 UI 文案一律英文;文档散文中文。

> 需求原文(用户,2026-09-15):"我想知道,这个,要怎么做,就是 SSH Tunnel 不应该有 Proxy 选项吗?走代理的选项,
> 正常的 SSH 连接就这么简单?我想走代理怎么办?还有其他的情况怎么办?我们讨论下"
> 同日讨论定案(Q2 追问):"我在执行前替换掉密码不行吗?我只是替换密码,这也不行?" → 澄清为
> "保存的时候,就是分开保存的,只有在真的执行的时候,才会拼接这些内容放到一起,拼接前,替换掉" ——
> 落为 §2.4 的实现红线:分字段存储、连接时解析、凭证以组件直达握手,永不物化含密码的 URL 字符串。
> 追加定案(同日):"这个 Proxy 的内容,属于 SSH Advanced 配置里面的内容,advanced 内容默认折叠" ——
> §4 落为连接 sheet 的 Advanced `<details>` 默认折叠区,proxy 与 jump 同居其中。
> Round-1 其余五问(范围/跳板建模/失败语义/面板/不动 MCP 侧)未逐条作答,全部按推荐值执行(§0.3 注明)。

## 0. 现状与缺口(为什么是它、为什么是现在)

> 本节坐标是基线 `a0fed265` 的函数名;实施后按函数名找,不按行号找。

### 0.1 现状

- `swiss-tunnels/src/tunnel/ssh.rs` 的 `dial()` 直接 `russh::client::connect(config, (host, port), handler)`
  —— russh 自己开一条裸 TCP 连到 host:port。TCP + kex + auth 在 `READY_TIMEOUT` 15 s 总预算内;
  keepalive 15 s × 3;host key TOFU(`check_server_key`);authType 只有 key/password,
  密码/口令在 `authenticate()`/`read_key()` 里 `refs::resolve`(docs/19 D4:缺引用 → `Config`,先于网络)。
- 连接定义 `types.rs` 的 `SshConnDef`:id/name/host/port/username/authType/group/keyPath/passphrase/password/hostKey,
  `to_json` 键序逐字节复刻 Node;新增字段必须 absent-when-unset、追加在既有键序之后(§1.2)。
- `store.rs`:tunnels.json 是密封信封(`read_secure_json`/`write_secure_json`,docs/05 冻结);
  保存期校验在 `valid_conn`。
- `api.rs`:`mask_conn`/`unmask_conn` 复用 `swiss-host/src/mask.rs` 的哨兵往返 ——
  `mask.rs` 的 `is_secret_key` 是词表子串匹配(含 "password"),`is_url_key` 精确匹配
  `url|connectionstring|dsn|proxy` 并只糊 URL 内的密码段(`split_url_password` + `mask_url`)。
- 代理先例:MCP `http`/`rest` 适配器的 `proxy` 字符串(`swiss-mcp/src/adapters/proxy.rs` 的
  `assert_proxy_url`,只收 http/https、明确拒绝 socks5 —— reqwest 的域;无代理凭证支持)。
  tunnels 侧文案族对齐它,但 scheme 集合不同(§1.3)。

### 0.2 缺口

只能直连 host:port。走代理(本机 clash/v2ray、公司 HTTP 代理)与跳板机(堡垒机)是标准开发者场景,
今天都做不到 —— 用户要么放弃,要么在 swiss 外面手动开 `ssh -L`,恰是本产品要吞掉的那类小工具。

russh 早已给出全部所需接口(以下均为 registry 源码 russh-0.63.2 的事实,已核对):

- `client::connect_stream(config, stream, handler)` 接受任何
  `AsyncRead + AsyncWrite + Unpin + Send + 'static` 的流;`client::connect()` 只是
  "开 TcpStream(顺手 set_nodelay)再调 connect_stream" 的糖。
- `server` 模块(`pub mod server`,无 feature gate,crypto 后端已由既有 `ring` feature 满足)
  的 Handler 有 `channel_open_direct_tcpip` —— 测试里能起真 SSH 服务器,**零 manifest 变更**。
- `channel.into_stream()` 是转发路径已在用的 ByteStream 形态(`open_channel()`)。

因此:代理 = 自己连代理 + 手写 CONNECT/SOCKS5 握手(各约百行,零新依赖)→ `connect_stream`;
跳板 = 对跳板连接 `open_channel(host, port)` → 在 channel 流上跑下一层 SSH(OpenSSH `-J` 同构)。

### 0.3 宪法检查

- **Ruthlessly small**:C1–C3 全部纯代码,运行时依赖零新增(`cargo tree -d` 空跑);空闲成本不变。
- **Plugin-shaped**:全部改动收在 swiss-tunnels(tunnel 模块 + 既有 api/store/面板文件);host 侧只在
  mask 词表已覆盖的范围内免费获得行为,不改 host。
- **Hot-pluggable**:连接的启停/编辑重连语义复用 manager 既有机制;禁用 tunnels 插件照旧释放一切。
- **凭证规则**:proxyUsername/proxyPassword 整字段 `${ENV}`/`${secret://name}` 引用、连接时解析、
  面板哨兵往返 —— 与 password/passphrase 逐字同型,零新机制。
- Loopback 边界不动:代理是出站方向,与 MCP http 适配器的 proxy 同向。

Round-1 六问按推荐执行,其中两处在讨论中被用户收紧后定稿:代理凭证分字段存(不嵌 URL),
拼接终点是握手组件而非 URL 字符串(§2.4 红线)。

**无新 ADR**:字段是可逆增量;依赖决策是"零依赖";信封/引用语义是 ADR-019/docs/25 的直接应用,
不新立决策。若实施中发现不得不引依赖,按 swiss-dependency-review 单独报。

## 1. Item 1 — 连接定义扩展与保存期校验(提交 C1)

### 1.1 字段(`SshConnDef` + `api.rs` 的 `conn_input`)

| 字段 | 类型 | 语义 |
| --- | --- | --- |
| `proxy` | `Option<String>` | 代理 URL:scheme ∈ {http, socks5};host 必填;port 可省(保存期归一:http→80,socks5→1080,**存归一后的值**);**禁 userinfo、禁 path/query** |
| `proxyUsername` | `Option<String>` | 整字段 `${...}` 引用或字面量,连接时解析 |
| `proxyPassword` | `Option<String>` | 同上 |
| `jump` | `Option<String>` | 另一条连接的 id;跳板本身是普通连接(自己的认证/TOFU/Test/面板行) |

互斥:**同一连接上 `proxy` 与 `jump` 不得并存**(保存期拒绝)。每跳自身的代理配在跳板连接上,
链式组合天然成立:B(jump=A) 且 A(proxy=clash) ⇒ B over A over clash,即 OpenSSH `-J` 的每跳配置模型。

### 1.2 存储(tunnels.json,密封不动)

- absent-when-unset;`to_json` 在既有键序(id…hostKey, group)之后按 proxy → proxyUsername →
  proxyPassword → jump 追加 —— 历史前缀逐字节不变,旧文件(无新字段)原样加载。
- docs/05 的封套格式零改动;新字段只是信封内 JSON 的增量键。

### 1.3 保存期校验(`valid_conn` 扩展;错误文案英文,与 `assert_proxy_url` 文案族同风格)

- 无 scheme / scheme 不在集合 → `proxy must be an http:// or socks5:// URL, got "…"`
  (`socks5h://` 单独文案:swiss 永远把主机名交给代理解析,即 socks5h 语义,写 `socks5://` 即可;
  `https://` 单独文案:与代理之间建 TLS 不在本期,见 §6)
- 含 userinfo(`user[:pass]@`)→ `proxy URL must not carry credentials; use the proxy username and password fields`
- host 为空 / port 非法 / 带 path 或 query → 各自点名
- `jump` 指向不存在的连接 → `jump connection not found: <id>`
- `jump` 自指 → `a connection cannot jump through itself`
- 成环(沿 jump 链走 visited 集)→ `jump cycle detected: A -> B -> A`(点名整条链)
- `proxy` 与 `jump` 并存 → `a connection can have a proxy or a jump, not both; put the proxy on the jump connection if it needs one`

### 1.4 API 回显

> **勘误(C1 实施发现,2026-09-15)**:原稿称 proxyPassword 命中 "password" 子串免费被糊 —— 错。
> `mask.rs` 的 `SECRET_KEYS` 是整键精确表(["password","pass","passphrase","secret","token"],
> `contains(&lower.as_str())`),不含子串匹配。哨兵往返因此实现在 `api.rs` 的 `mask_conn`/`unmask_conn` 内
> (出向先对 proxyPassword 置 MASK 再过 mask_def;入向先还原再过 unmask_body;游离哨兵仍由既有 drop 兜底),
> `mask.rs` 不动。`proxy` 命中 `is_url_key` → `mask_url` 只糊 URL 内密码段,而保存期已禁 userinfo,实际原样往返;
> proxyUsername、jump 明文往返。

### 1.5 验收测试(先红后绿;store/api 层,无网络)

1. `to_json`:未设新字段时键集与键序 == 现状(复刻 `rule_json_keeps_node_field_order` 的同型断言);
   设置时按 §1.2 顺序追加;portless URL 归一后落盘。
2. 旧文件(无新字段)加载 → def 等价于现状构造。
3. `valid_conn`:§1.3 每条规则一个用例(socks5 合法、portless 归一、user:pass@ 拒、socks5h 拒、
   https 拒、自指拒、两环节、三环节、指向不存在拒、proxy+jump 并存拒)。
4. api round-trip:proxyPassword 哨兵、proxy 原样、jump 原样;编辑无关字段不破坏哨兵(既有 unmask 语义)。

## 2. Item 2 — 代理拨号器(提交 C2)

### 2.1 新模块 `crates/swiss-tunnels/src/tunnel/proxy.rs`

```rust
pub struct ResolvedProxy { scheme, host, port, username: Option<String>, password: Option<String> }
pub fn parse(url: &str) -> Result<ResolvedProxy, String>        // 最小手写解析(见下)
pub async fn dial(p: &ResolvedProxy, host: &str, port: u16) -> Result<TcpStream, TunnelError>
```

- 解析手写:文法已被保存期校验收紧为 `scheme://host[:port]`,几十行足够;
  不引 url crate(swiss-tunnels 目前不依赖它;若实施倾向换 url,走 swiss-dependency-review)。
- 拨号顺序(**先解析引用,后开 socket**):`refs::resolve` username/password → 失败即 `Config`、
  不重试、不拨号、错误点名引用(docs/19 D4 同型);解析后的 user/pass 各 ≤ 255 字节
  (RFC 1929 长度限制),超长 → `Config`。
- `TcpStream::connect` 到代理 host:port,置 TCP_NODELAY(与 `russh::client::connect` 的直连行为对齐),
  然后按 scheme 握手,得到的流转交 `connect_stream`。

### 2.2 HTTP CONNECT(scheme http)

1. 发 `CONNECT <host>:<port> HTTP/1.1\r\nHost: <host>:<port>\r\n` +
   (有凭证时)`Proxy-Authorization: Basic base64(user:pass)\r\n` + `\r\n`。
2. 读到 \r\n\r\n;状态行 2xx → 流就绪;否则 → `Network`,
   `proxy <phost>:<pport> refused CONNECT: HTTP <code>`。

### 2.3 SOCKS5(scheme socks5)

1. `[05, nmethods, methods]`:无凭证 {00},有 {00, 02}。应答 00 → 直接过;02 → RFC 1929 子协商
   `[01, ulen, user…, plen, pass…]`,应答须 `[01, 00]`,否则 `Network`
   `proxy <phost>:<pport> rejected socks5 credentials`;FF → `Network`
   `proxy <phost>:<pport> offers no supported socks5 auth method`。
2. CONNECT:`[05, 01, 00, ATYP, addr, port_be]`。目标 host 按配置原样:**本端不解析目标主机名** ——
   名字 → ATYP 03 + 长度前缀(socks5h 语义,代理解析);IPv4 字面量 → ATYP 01;IPv6 字面量 → ATYP 04。
   应答 `[05, 00, …]` → 就绪(BND.ADDR/BND.PORT 丢弃);REP≠0 → `Network`
   `proxy <phost>:<pport> socks5 reply <rep>`。

### 2.4 实现红线(来自需求讨论,违者 review 打回)

**凭证只以组件形态到达握手。** SOCKS5 吃原始 user/pass 字节,HTTP Basic 吃 base64(user:pass);
任何代码路径不得物化"含密码的完整 URL 字符串"(无 `format!("{}://{}:{}@…", …)`)。
本拨号器是凭证的唯一潜在消费者、而它不吃 URL,所以该中间物在构造上不存在。
若未来出现只吃 URL 的库:拼装时 percent-encode,且永不进日志/错误信息。
既有 `classify_message` 靠子串扫 "auth"/"host key"。不变量是:代理失败**一律以显式 `FailureKind::Network`
构造,从不流经 `classify_message`** —— §2.3 的固定串 "…socks5 auth method" 确实含 "auth",这不构成问题,
因为拨号器错误从不走字符串分类;kind 由拨号器测试的 `kind == network` 断言钉死(§2.6)。
(**勘误**:C2 实施发现,2026-09-15 修订;原稿"固定文案不含这些词"与 §2.3 自相矛盾。)

### 2.5 其余语义

- 失败分类:代理 TCP 拒/超时、CONNECT 4xx、SOCKS5 REP≠0、方法协商失败 → 一律 `Network`
  (`is_retryable` 已放行,auto-reconnect 语义自动适用)。
- `READY_TIMEOUT` 15 s 总预算覆盖整链(代理握手 + kex + auth);不做每跳独立超时(§6)。
- keepalive 不动(russh 层,不感知下层传输)。
- `ssh.rs` 的 `dial()`:无 proxy 时保持今天的直连路径;有 proxy 时 `proxy::dial(…)` → `connect_stream`。
  `SshConnection::test` 的 throwaway 客户端自动走同一路径,不需改。

### 2.6 验收测试(in-process 假服务器,ephemeral 127.0.0.1 端口;先红后绿)

1. **russh server 测试底座**(共享 dev 模块):起真 SSH 服务器(任意凭证接受),
   实现 `channel_open_direct_tcpip` 为回连请求地址并双向泵字节;本项与 Item 3 复用。
2. SOCKS5 假代理:断言收到的方法协商、RFC 1929 blob、CONNECT 的 ATYP=03 + 域名字节 + 网络序端口;
   回 REP 00 后透传 —— 客户端侧经底座真 SSH 服务器完成握手,连接 `connected`。
3. HTTP 假代理:断言 CONNECT 行、Host 头、Basic 头(base64 内容);回 `HTTP/1.1 200` → 同上;
   回 407 → `kind == network` 且文案含 "proxy" 与 "HTTP 407"。
4. SOCKS5 REP≠0 → `network`,文案含 "socks5 reply"。
5. proxyPassword 为缺失的 vault 引用 → `Config`、先于拨号(断言错误点名引用,同既有 passphrase 测试型)。
6. manager/router 级:带 proxy 的连接 start→connected→停止释放本地端口,走既有端口绑定不变量。

## 3. Item 3 — 跳板引用(提交 C3)

### 3.1 语义

`jump` = 另一条连接的 id。传输栈递归定义:

```
transport(conn) = 若 conn.jump = Some(j):
                    live_dial(j) 的 open_channel(conn.host, conn.port)   // SSH-over-SSH
                  否则:
                    conn.proxy ? proxy::dial(conn) : 直连 TcpStream        // §2
```

即 OpenSSH `-J` 的每跳模型:最内层跳板用自己的(可能走代理的)传输拨入,外层连接在
内层的 direct-tcpip channel 上跑自己的 SSH。多级链 = 引用即链(A 跳 B 跳 C);
环在保存期已拒(§1.3),运行时无需检测。

### 3.2 生命周期(manager)

- 活连接:目标 `dial()` 经 manager 的连接表取各跳的共享 `SshConnection`(refcount 复用,
  "Start all" 的单飞语义照旧);目标的 `Live` 对**每个中间跳**持有一个 hold(同 `open_shell`
  的 hold 模式),目标断开/失联时逐个释放 —— 跳板连接不会因目标消失而泄漏。
- Test(`POST /connections/:id/test`):私有 throwaway 链 —— 每跳都是一次性客户端、
  不落 hostKey(沿用现有 TOFU 注记:对无指纹跳板的成功 Test 什么也不存)。
- 跳板失联 → 目标的传输随 channel 死亡 → 目标按既有 watcher 报 `network` 失联、释放本地端口,
  auto-reconnect 对整链重新求值 —— "本地端口只在隧道可承载时被绑定" 不破。
- 编辑被引用为跳板的连接(`set_def`):除其自身规则外,**依赖它的连接一并停止并重连**
  (依赖者 = defs 中 jump == 该 id 的连接;保守重连,不 diff 字段)。
- 删除被引用为跳板的连接 → 400,文案列出依赖者名(与环校验同一 visited 思路)。

### 3.3 host key 与失败

- 每跳 TOFU 独立;目标连接的 hostKey 是**最终 SSH 服务器**的指纹;跳板的指纹存在跳板自己的 def 上。
- 跳板的 auth/hostkey 失败保持原 kind(auth/hostkey 永不重试),错误信息前缀跳板连接名:
  `via <jump-name>: <inner message>`,面板一眼可辨是哪一跳。
- 目标自身的 auth/hostkey 语义不变。

### 3.4 验收测试(复用 §2.6 底座;先红后绿)

1. 端到端:跳板服务器 S1(底座,direct-tcpip 回连)+ 目标 SSH 服务器 S2(底座);
   连接 A(host=S1)、B(jump=A, host=S2)→ B `connected`;经 B `open_channel` 到 S2 侧 echo 端口,
   字节往返 —— 完整 SSH-over-SSH。
2. 两级链 A→B→C 同型(证明递归与 hold 不泄漏:断言停 C 后 A、B 的 refs 归零)。
3. 失联传播:杀 S1 → B 报 `network` 失联、本地端口释放;重启 S1 后 auto-reconnect 恢复。
4. 跳板坏密码 → B 的错误 kind == `auth`、文案含 `via <A 的名字>: …`、不重试。
5. 编辑 A(改密码)→ B 自动重连成功(§3.2);删除 A 被 400 拒且列出 B。
6. 保存期环/自指/不存在/并存已在 §1.5 覆盖,此处不重复。

## 4. Item 4 — 面板(提交 C4)

`tunnel-sheets.js` 的 `openConnSheet` + `views/tunnels.js`(徽标),全部沿用 house 惯例
(add-sheet/closeSheet、`$("sheet").hidden = false` 先于 innerHTML、monochrome tags、util.js esc)。

- **Advanced 折叠区**(追加定案:proxy 属 SSH Advanced 配置,默认折叠):连接 sheet 表单
  底部一个 `<details>`(无 `open` 属性 → 默认收起;原生 details/summary 是 house 折叠原语,
  `data-value.js` 的 JSON 树同款。注意规则 sheet 里 `.cap` "Advanced" 是平铺标题、不可折叠,
  不适用此场景)。区内两组:
  - **Proxy**:Proxy URL 输入(placeholder `socks5://127.0.0.1:7890`)、Proxy username、
    Proxy password(type=password);空值 == 未设。保存走既有 POST/PUT,哨兵往返不破坏(proxyPassword)。
  - **Via connection (jump)**:下拉列出其它连接(排除自己);后端环校验兜底,400 文案内联显示
    在 sheet 里 —— 单一真源在后端,面板不预计算链。
  - summary 在有配置时带 chip(`proxy`、`via <name>`,docs/18 单色 tag 形态):默认折叠不等于
    隐藏状态 —— 不展开也能看见"这条连接走代理/经过跳板";无配置时 summary 只有 "Advanced" 裸字。
- **徽标**:连接行有 proxy → tag `proxy`;有 jump → tag `via <name>`。
- 空状态:无任何新字段时,sheet 与现状的唯一可见差异是那行收起的 Advanced summary —— 其余逐像素一致。

### 4.1 验收测试

1. vitest(`crates/swiss-panel/panel-tests/`,新增或扩展 tunnels sheet 用例):
   Advanced `<details>` 默认无 `open`、有配置时 summary chip 渲染且无配置时不渲染、
   展开后字段渲染、保存 payload 含可选字段且缺省不送、徽标渲染、环 400 内联、
   哨兵字段回显不破坏(proxyPassword)。
2. `node --check` 每个改动文件。
3. **panel-proof-of-life 全清单**(`.agents/rules/panel-proof-of-life.md`):19998 新起实例
   (`scripts/test-instance.ps1 -Fresh`,CARGO_TARGET_DIR=target-test),真浏览器新页面加载,
   真 CDP 点击:Advanced summary 点击后 `details.open === true` 且字段可见(真点击,
   非 `.click()`);新建带代理连接 → Test → 结果可见;跳板下拉可选可存;每个 sheet 开启时
   `sheet.hidden === false` + computed display 断言;空状态渲染。无法验证的条目
   (无真代理/真跳板环境)如实标注 NOT verified 及原因 —— 不许打勾。
   有 clash/v2ray 或真堡垒机时补真实成功路径;没有就标注。

## 5. 交付顺序与门禁

每项一提交、测试先行(改前红、改后绿),依赖:C1 → C2 → C3 → C4(C3 复用 C2 的底座与流交接点;
C4 依赖 C1–C3 的全部字段与行为)。

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d        # 运行时依赖零新增 —— 必须与基线同样干净
# 面板(C4):node --check 每个改动的 js 文件;vitest 全量(crates/swiss-panel/panel-tests)
```

live 验证在 19998(`scripts/test-instance.ps1`);**19999 全程不动**;部署是操作者步骤,
只走 `scripts/deploy.ps1`,且只在用户过目 19998 之后(swiss-deploy)。
部署后按 swiss-memory-record 惯例在 docs/01 记一行日期读数(纯代码增量,预期 ±0)。

## 6. 显式不做(out of scope)

- **ssh-agent 认证**(第三种 authType)—— backlog,真需求来了单独立项。
- **-D 动态转发**(本地起 SOCKS 口)—— 另议。
- **ProxyCommand / ~/.ssh/config 解析** —— 永不:子进程规则 + 无限文法。
- **https:// 代理(与代理之间 TLS)** —— 需要 TLS 客户端进 tunnels crate,本期不做;
  scheme 白名单只收 http/socks5,保存期文案说明。
- **SOCKS4/4a、代理链**(同一连接多代理串联)—— 跳板链已覆盖组合需求。
- **每跳独立超时** —— 15 s 总预算保持;链深时用户看到的是 timeout,不是新旋钮。
- **MCP http/rest 代理放开 socks5** —— reqwest feature 重量,另一个插件的域;真需求单独立项。

## 7. README

本文档已加入 README 的 docs 表(随本 spec 同一提交)。
