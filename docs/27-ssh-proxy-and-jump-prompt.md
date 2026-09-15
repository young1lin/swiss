# 27 — SSH 隧道:代理拨号与跳板引用 — 实施交接提示词

工作目录:`<repo>`(cargo workspace,Windows)。你是全新会话,不带前文;
本文件 + docs/27 是你全部的任务上下文。按顺序先读:

1. `AGENTS.md` —— 载重规则高于一切(loopback、引用不落值、lazy proc、封印冻结、current_thread、
   LF、测试纪律、19998/19999 纪律)。
2. `docs/27-ssh-proxy-and-jump-spec.md` —— 任务本体;§1.3 错误文案、§2.2/§2.3 握手字节、
   §2.4 实现红线、§3.1 传输栈递归是验收的单一事实源。
3. `docs/25-vault-ref-envelope-spec.md` + `crates/swiss-core/src/secure/refs.rs` —— 信封解析:
   proxyUsername/proxyPassword 整字段引用,`refs::resolve` 的严格合同原样复用。
4. `crates/swiss-tunnels/src/tunnel/ssh.rs` —— `dial()`/`authenticate()`/`read_key()`/`open_channel()`:
   拨号改造点与 hold/watcher 模式;§2.5 的直连路径必须原样保留。
5. `crates/swiss-tunnels/src/tunnel/types.rs` / `store.rs` / `api.rs` —— `SshConnDef::to_json` 键序、
   `valid_conn`(校验扩展点)、`mask_conn`/`unmask_conn`(不改,§1.4 说明为何免费)。
6. `crates/swiss-tunnels/src/tunnel/manager.rs` —— 连接表/refcount/编辑重连;§3.2 的依赖者重连挂点。
7. `crates/swiss-panel/src/admin_assets/js/tunnel-sheets.js` + `js/views/tunnels.js` —— sheet 惯例与徽标;
   vitest 在 `crates/swiss-panel/panel-tests/`。

任务摘要:SSH 连接定义新增 proxy(http CONNECT / socks5,分字段凭证,连接时解析,组件直达握手)与
jump(另一连接 id,SSH-over-SSH,即 OpenSSH -J);保存期校验(scheme 白名单、禁 URL 内凭证、环检测、
proxy/jump 互斥);面板 Advanced `<details>` 默认折叠区(proxy 与 jump 同居其中,summary 有配置时带 chip)+ 徽标。零运行时新依赖 —— russh 的 `connect_stream` 与
无 feature gate 的 `server` 模块已覆盖全部需求(registry 源码 russh-0.63.2 已核对,见 spec §0.2)。

交付顺序(spec §5,每项一提交、测试先行 —— 改前红、改后绿):
Item 1 config 层(types/store/api + russh server 测试底座可后置)→ Item 2 proxy 拨号器 + 底座
→ Item 3 jump 链 → Item 4 面板。

每提交门禁:`cargo test --workspace`;`cargo clippy --workspace --all-targets -- -D warnings`;
`cargo tree -d`(运行时依赖零新增,空跑);面板改动另跑 vitest 全量 + `node --check`。
红线(违者自查打回):凭证永不物化成含密码的 URL 字符串(spec §2.4);不改 mask.rs;
不动 swiss-mcp 的 proxy;不碰密封格式(docs/05);错误文案逐字用 spec §1.3/§2 的英文串。

面板验证按 `.agents/rules/panel-proof-of-life.md`:真浏览器、真点击、19998 新起实例
(`scripts/test-instance.ps1 -Fresh`,迭代构建用 `CARGO_TARGET_DIR=target-test`),验收照 spec §4.1,
含 sheet 可见性断言(`sheet.hidden === false` + computed display);无法验证的条目如实标注 NOT verified。
**19999 是用户的实例,全程不动**;部署只走 `scripts/deploy.ps1`,是最后一步,且只在用户看过 19998 之后。

代码注释一律英文;LF;不新增运行时依赖;偏差(措辞、命名、结构)记进提交信息与最终汇报;
颜色/文案类留给用户过目。写 spec 的会话不实施 —— 你从本文件开始,不要去找它要上下文。
