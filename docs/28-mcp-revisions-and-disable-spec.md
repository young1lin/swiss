# 28 · MCP 同名替换(revisions)与禁用正名 — spec

**状态:Approved for implementation(2026-09-16),基线 `a178e3d`。** swiss-spec 流程产出;
执行会话(同日)按 `docs/28-mcp-revisions-and-disable-prompt.md` 起跑。

**验证端口:19997**(操作者指定,压缩也不许忘):agent 的实机验证实例一律 -Port 19997,
脚本默认 19998 保持不动(repo 文档不受影响);19999 仍是生产,照旧只读。

> 2026-09-16,操作者原话:
> 「我想再创建个 zai-vision,不过这个是新的,不是原来的 proc,但是呢,我怕这个会失败,
> 我想 disable 这个,创建个同名的,不行吗?或者改名字,这不都可以吗?但是一个都没有。
> 我建议加个可以改名的。还有加个 disabled,同名可以,但是同名只有一个可以生效。」
> 追问确认:禁用后「客户端肯定不能要」(必须不可用);范围「都要写」。

## 0. 现状与缺口(证据先行)

| 诉求 | 现状 | 缺口 |
| --- | --- | --- |
| 改名 | **已有全链路**:`POST /api/mcps/{name}/rename`(adminapi.rs:1271,registry+store+tunnel 关联+OAuth 凭据同步改),面板在详情页 ⋯ More actions → Rename…(pane.js:214) | 只藏在详情菜单,侧边栏行上没有 — 发现性 |
| 禁用 | **Stop 就是**:面板 Stop 持久化 `enabled:false`(adminapi.rs:1236),boot 不复活(server.rs:140 + 两个 boot 测试),资源真释放;客户端请求全被拒 — `handler_for` 对 stopped 返回 None(app.rs:161),mcp_post 落 503(app.rs:378) | 词不达意:按钮叫 Stop,503 文案说 not started;用户不知道它=禁用 |
| 同名替换 | **没有**:add 撞名 409(adminapi.rs:977) | 全部 — 本 spec 的主体 |

结论:真正要建的是**同名替换与回滚**(D1);禁用(D2)是把既有行为正名;改名(D3)是把既有
能力放到眼前。

## 1. 先例(检索核过,非凭记忆)

- **Kubernetes Deployment** — rollout 历史默认保留,`revisionHistoryLimit` 截尾,
  `kubectl rollout undo` 一键回滚(kubernetes.io Deployment 文档)。借的是:版本挂在
  身份名下、只存 spec 快照、上限截尾、回滚也是一次普通发布。
- **systemd** — `disable`(boot 不再拉起)与 `stop`(现在停掉)是两个正交动词,
  `enable/disable` 管要不要,`start/stop` 管现在(askubuntu / linux-audit 常答)。借的是:
  持久的关需要自己的名字,不能只叫 stop。
- **Home Assistant** — config entry 有 disabled 态:保留配置、不加载、不服务
  (Disabled by config entry)。借的是:禁用=数据留下、行为全无。
- **mcp-router**(github.com/mcp-router/mcp-router)— 面板一键 toggle MCP server on/off、
  per-tool enable/disable。借的是:MCP 世界里这套就是一等公民操作,swiss 缺的不是能力是表达。
- **蓝绿部署** — 同一入口名下双版本,cutover 一瞬完成,回滚=切回(harness.io 等)。借的是:
  替换的原子性:新版本先验证、旧版本零删除、切换一次成型。

综合先例得出本 spec 的核心判定:**名字是逻辑服务,def 只是它的一次修订**。OAuth 凭据、
调用日志、分组、tunnel 关联都挂在**名字**上(它们描述的是逻辑服务,不属于某一代 def);
revision 只是 def 快照,永远不运行。这样同名只有一个生效是构造不变量,而非运行期仲裁
——先前凭据打架的顾虑(两个同名 def 抢同一份凭据)不成立:revision 根本不跑。

## 2. D1 同名替换与回滚(revisions)

### 2.1 存储

`managed.json`(密封态文件;docs/05 冻结的是封印格式,不是键集)新增平级键:

```json
{ "mcps": [ "<原有内容不动>" ],
  "revisions": {
    "zai-vision": [
      { "def": { "type": "proc", "command": "node ..." },
        "at": 1789467260822, "note": "proc 版,迁移前" } ] } }
```

- 每名字最多 **5** 条(k8s 默认 10 偏多,这是开发者本机工具);入栈挤掉最旧。
- `note` 可选;面板 Replace 表单带一行备注框。
- config 来源的 MCP 同样适用:快照照存;restore 走**既有 override 机制**(adminapi.rs:1525
  一带,managed 覆盖 config),不改写 gateway.config.json。
- rename 携带 revisions(名字改,历史跟着);delete 连 revisions 一起清。
- boot 只读 `mcps`;revisions 一律不启动、不注册。

### 2.2 API(全部挂 admin 面,命名沿用现有风格)

| 路由 | 行为 |
| --- | --- |
| `GET /api/mcps/{name}/revisions` | `{ revisions: [{ index, at, note, type }] }`,旧→新 |
| `POST /api/mcps/{name}/replace` | body 同 Add(build_def 同一入口)。**先 build 后落盘**:make_adapter+resolve 失败 → 400,现状零改动;成功 → 当前 def 入栈 revisions、活动 def 换新、registry 重注册;原本 running 的重启一次,原本 stopped 的**保持 stopped**(沿用 adminapi.rs:1561 的编辑规则)。换定后启动失败 **不算失败**:200 + `restartError`(对齐 add 路由——回滚路径必须可达) |
| `POST /api/mcps/{name}/revisions/{index}/restore` | 同一事务反向:当前 def 入栈,目标 revision 转正并按原状态启动/保持 |
| `DELETE /api/mcps/{name}/revisions/{index}` | 丢弃一条,200 `{deleted:true}`(house 风格 JSON;空表 404) |

### 2.3 面板

- 详情 Config 页新增 **Replace definition…**:打开与编辑同款表单(预填当前 def,名字锁死),
  Save → replace → toast「已替换,旧 def 已存为修订 N」。
- Config 页新增 **Saved revisions (n)** 列表:每行 type/at/note + Restore / Delete;
  Restore 有 confirm(当前 def 会被存为修订,目标修订转正)。
- 侧边栏行不出现 revisions(保持行轻);一切在详情页。

## 3. D2 禁用正名(不改机制,只改表达)

- 面板:Stop → **Disable**,Start → **Enable**(UI copy 英文,规矩如此);状态串 stopped →
  disabled。API 动词 **start/stop 不改**(不为措辞折腾 wire)。
- 503 文案:app.rs:385 一带的 not started (state: …) →
  `MCP '{name}' is disabled — enable it from the panel`。
- 语义已被测试钉死的部分(stopped 跨 boot 不复活、edit 不复活)不动一行;新增断言只钉新文案。

## 4. D3 发现性

- MCP 侧边栏行加**右键菜单**(实现期修正:行本身是 `<button>`,嵌省略号按钮非法 HTML;
  右键锚点沿用 data-csv.js 的 ctx-menu 模式,tooltip 提示 "right-click for actions"),
  菜单项:Rename… / Disable|Enable(按态,verb 读活态)/ Delete。Rename 复用 renameMcp,不新写。
- 详情 ⋯ 菜单在 Restart 旁加 Disable|Enable;详情头主按钮 Stop→Disable、Start→Enable(即 D2)。

## 5. 验收(行为变化配测试,先红后绿)

Rust(tests/adminapi.rs + registry/server 单测):
1. replace:建 echo → replace 成 mysql 假端口 def → 行 type 变、revisions 有一条旧 def;
   details 活动指新 def。
2. replace 失败原子性:坏 def(如缺 apiKey 的 zai-vision)→ 400,活动 def 与 revisions 均不变。
3. replace 对 stopped 的 MCP:替换后仍 stopped(不因替换复活)。
4. restore:replace 后 restore(0) → def 回旧值,当前 def 进了 revisions。
5. 上限:塞 6 条,最旧被挤,恒 ≤5。
6. rename 携带:改名后 revisions 键跟随、旧键消失;delete 清栈。
7. boot:有 revisions 的名字只启动活动 def(扩展现有 boot 测试)。
8. 503 新文案:对 stopped MCP 的 tools/list 断言 body 含 disabled。

vitest(crates/swiss-panel/panel-tests/):
9. Config 页渲染 Replace definition… 与 Saved revisions;Restore 出 confirm。
10. 行省略号菜单存在,点 Rename 调 renameMcp、点 Disable 调 act(name, stop)。
11. 按钮/状态文案 Disable/Enable/disabled。

CDP 实机(19997,全规矩;脚本传 -Port 19997):
12. 真 walk:行菜单 Rename 真改名;Disable 后 /mcp/<name> 503 含 disabled;Enable 恢复;
    Replace definition 全程(用 echo/mysql 假端口这类不花钱类型演),Restore 回滚成功。

## 6. 交付顺序与门禁

```
cargo test --workspace        # 唯一组合,--workspace 不许省
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d                 # 本 spec 零新依赖,任何重复栈即失败
```

0. scripts/test-instance.ps1 参数化 -Port(默认 19998 不变;agent 验证传 19997)。
1. D1 后端:存储 + 四条路由 + Rust 测试 1–8 + ADR-023(docs/07,核心判定见 §1)。
2. D1 面板:Replace 表单 + revisions 列表 + vitest 9。
3. D2 正名:文案 + 503 串 + vitest 11(一次提交两侧同改)。
4. D3 行菜单 + 详情菜单项 + vitest 10。
每步一提交;之后 swiss-live-verify(12)→ swiss-review → 合并/部署按操作者指令。

## 7. 明确不做

- 同名双活(蓝绿的 live 流量分摊):revision 永不运行,那是另一个量级的机制。
- revision diff 视图、自动回滚定时器、按客户端可见性规则。
- API 动词改名(start/stop 保持)。

