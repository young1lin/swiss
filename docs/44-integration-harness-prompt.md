# docs/44 实施提示词 — 集成测试底座（真库、自建 MCP、任何机器一条命令）

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

先建 worktree，再进去，之后所有命令从 worktree 根跑：

```
git worktree add .agents\worktrees\it -b integration-harness master
cd .agents\worktrees\it
```

基线 master `946b921`（2026-09-22；如果 owner 已经做完 docs/40 二次清理的 ref 交换，这个短 hash 会变——
按 subject `oss: the record catches up with the tree` 找）。规范 `docs/44-integration-harness-spec.md`、
Docker 手册 `docs/44-wsl-docker-setup.md` 都在树里。

**不要 `cd` 回主 checkout；不要碰 `.agents/worktrees/data`（另一个会话的 `data-full-access`）；不要碰
19999。** PowerShell 里 `cd` 混在链式命令中会打断后面每一个相对路径——每条命令自己从 worktree 根起。
这次没有面板改动，不需要 `npm ci`。

第一件事，验 Docker：

```powershell
$env:DOCKER_HOST                                   # 期望 tcp://127.0.0.1:2375
curl.exe -s http://127.0.0.1:2375/_ping            # 期望 OK
```

不通就 `wsl -d <distro> --exec true` 再试一次；还不通说明 owner 还没装（那是他们的事，手册在
docs/44-wsl-docker-setup.md）。**没有 Docker 也能做 I0**——I0 的验收①正是"无 Docker 时红得正确"；
I1 起需要容器，做到那里就停下报告，不要用假容器、不要把测试改成跳过。

## 任务

`docs/44-integration-harness-spec.md` 定义的 I0 → I7。一句话：新建 dev-only crate `crates/swiss-it`，
feature `it` 默认关；用 testcontainers 起 `mysql:8.4` / `postgres:17` / `redis:7`，每条测试自己的库从
仓库里的 seed 还原；L1 三个 browser 打真库，L2 三个 adapter 经真 rmcp client 走 `/mcp/<name>`（含
凭证引用与 **Disable 之后容器里连接归零** 的证明），L3 proc 用仓库自带的 `it-mcp-server`（stdio）；
AGENTS.md 门禁变两条，CI 加 `integration` job。二进制的依赖图**一根毛不动**。

## 先读这些（按顺序，别跳）

1. `AGENTS.md` 全文——四条产品属性与载重规则高于规范。
2. `docs/44-integration-harness-spec.md` 全文，§0.5 宪法检查、§2.8 依赖重量、§2.9 mutation 检查三段
   读两遍。
3. `tests/adminapi.rs:50-112`（`sandbox()` / `setup()`——`gateway.rs` 抽的就是它）和 `tests/http_adapter.rs`
   （进程内远端的写法）。
4. `crates/swiss-host/src/dbbrowser.rs:743-` 两个 trait 的每个方法签名——L1 的每条测试对应一个。
5. `crates/swiss-mcp/src/adapters/mysql.rs:265`、`pg.rs:765`、`redis.rs:766`——def 字段名从这里抄，
   不要发明。
6. `crates/swiss-mcp/src/adapters/proc.rs`——懒起、idle、杀树的实现在哪，L3 才知道往哪里打。
7. `.agents/skills/swiss-dependency-review/SKILL.md`——每个新 dev-dep 都要按它写理由。
8. `docs/08-testing.md` 第 30–40 行（环境规则）与"例外表"。
9. `docs/40-open-source-release-spec.md` D2——seed 的红线。

## 交付顺序

I0 → I1 → I2 → I3 → I4 → I5 → I6 → I7，每项一个 commit，不合并、不跳。I2/I3/I4 互相独立，但
按序做——I2 踩的坑 I3 直接受益。

- **I0**：先写 `engine.rs` 的失败路径再写成功路径——三段失败信息（解析顺序每步为何没成、
  `DOCKER_HOST` 现值、指向手册 §4 的那句）是第一条通过的测试。`cargo tree -e normal,build -p swiss`
  的前后输出各存一份，diff 为空贴进提交说明。CI 的 dup-check 改 `-e normal,build` 在这一项。
- **I1**：seed 的每一行都要能说出它对应 §2.4 表里的哪一格；写完先跑 seed 守卫测试。
- **I2–I4**：一个 trait 方法一条测试起步，命名 `<方法>_<证明什么>`（例：`fetch_keeps_bigint_past_2_53_as_text`）。
  每组提交前做一次 §2.9 的 mutation，红的测试名进提交说明。
- **I5**：`Gateway::boot` 真监听 `127.0.0.1:0`。连接归零那条测试若红，**不改弱**——开 issue、
  提交说明写明、测试留红。
- **I6**：`it-mcp-server` 只做规范里的五个工具；`blob` 的断言是字节相同，不是 JSON 相等。
- **I7**：ADR-028 按 docs/07 现有条目的形状（Status / Context 选项表 / Decision / Consequences /
  what shipped / cost）；README 表加 docs/44 一行；docs/08 的"例外表"把 `mysql_browser.rs` 那行改成
  指向 swiss-it；本文状态头改"已实施"并逐项填 commit。

## 门禁（每次提交前）

```
cargo test --workspace
cargo test -p swiss-it --features it
cargo clippy --workspace --all-targets --features it -- -D warnings
cargo tree -d -e normal,build
cargo deny check
```

第一条在**没有** `DOCKER_HOST` 的 shell 里也跑一次（`$env:DOCKER_HOST=$null` 后开新 shell），证明单元门
不依赖 Docker——这是 §4.3。

## 提交规则

- 一个提交一项；提交说明写清"改了哪些文件、哪些用例覆盖、在哪台机器上跑了哪条门、没跑什么及原因、
  mutation 记录（I2 起）"。I0 贴 `cargo tree -e normal,build -p swiss` 的空 diff。
- 每个新 dev-dependency 在提交说明里有 swiss-dependency-review 要求的那几行（为什么要、feature 最小集、
  `cargo tree -d` 结果、license）。
- 不许删测试来过关；不许用 `#[ignore]`；不许把"Docker 不在"变成跳过。
- seed、测试名、断言文本里不许出现任何真实系统的名字（`git grep -i -E 'acme_|ops_dev' crates/swiss-it`
  必须为零）。
- 代码注释英文，提交说明按仓库现有 log 的口吻；结尾不加任何 Co-Authored-By trailer（docs/40 D3）。
- 提交作者是仓库本地已配置的身份，不改。

## 不做什么

见规范 §5。特别是：不引 `postgresql_embedded` 之类嵌入式方案、不装 Docker Desktop、不做 MariaDB、
不做 L4 golden capture、不让面板 vitest 打真库、不在 WSL 里跑 cargo、不动 `tests/adminapi.rs` 与
`tests/http_adapter.rs`、不碰 19999、不碰 data worktree、不合并 master——合并是 I7 之后 owner 的一次
决定。

## 完成之后

在最后一条回复里给：每项的 commit hash；两边（本机 Windows、以及如果 owner 已建仓则 CI）的用例数；
§4 七条验收逐条的证据；连接归零那条测试的状态；没做到的事和原因。然后停下——不部署、不合并、不 push。
