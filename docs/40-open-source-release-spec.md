# 40 — 开源前的清场：身份、脱敏、归属、标配、卫生

> 状态：**已实施**（2026-09-21，master，本地为止）。O1–O5 各一个 commit，O6 是分支/worktree/
> 产物的删除（无 tracked 变更），O7 是三遍 `git filter-repo`（第一遍主重写；第二、三遍补
> commit message 里的字面量和 JSON 转义写法的路径），之后一个 commit 把 37 个 doc 里引用的
> 91 个旧短 hash 换成重写后的值。§3 验收全部通过：520 个 commit 只有一个 author/committer、
> 0 条 trailer、工作树与全历史 0 命中、四张截图与 build 树从历史消失、无 >5 MB blob、pack
> 110 MB → 7 MB、`cargo test --workspace` 1365 通过、clippy `-D warnings` 与 `npm run check`
> （76 文件 682 用例）绿、`cargo deny check` 四项 ok。重写前的完整备份：
> `../swiss-pre-oss.bundle`（所有分支、旧 hash）。GitHub 建仓、push、CI 首跑、
> tag 不在本 spec 内——owner 另行决定何时做。
>
> 增补（2026-09-26，第二遍清场）：O2 的替换表只管身份（邮箱、路径、四张截图），漏了**真实环境
> 指纹**——真实开发库的库名、表名、DDL 注释与订单号（docs/assets/42 mockup、面板与 Rust 测试
> fixture、docs/43），真实 Redis 的一个 key、一条读自其上的文案和一个会话 key 前缀，真实隧道
> 布局（主机、用户、连接名，docs/assets/46 mockup，9/23 在重写之后才进库），owner 的英文名
> （测试 fixture、一处 Windows 路径、commit message），以及 docs/43 M3/M4 四张截图的首版
> （拍到了真实库列表和数据行，后来的 fixture 重拍只是盖在上面）。处理同 O2/O7：工作树换成中性
> fixture（`acme_app_*`、`shop_*`、`jdoe`，替换与原词等长的地方保持等长，宽度与 ratchet 不动），
> `docs/assets/43/redis-utf8-decode.png` 退出 tracking 并 ignore；再一遍 `git filter-repo`
> （同一张表过全部 blob 与 commit message，按 id 剥离四个首版截图 blob，删掉那张 Redis 截图的
> 路径），之后一个 commit 重映射文档里的旧短 hash。替换表只在本机，不入库（理由同 O2）。
> 重写前的完整备份：`../swiss-pre-scrub-2026-09-26.bundle`。
>
> 增补（2026-09-29，第三遍清场，owner：「改历史，push」）：审计又找到三处。① 9/20 的四个 commit
> 把 commit message 草稿（根目录四个 `.tmp`，各带一条模型署名 trailer，违反 D3）一起提交了，
> 后一个 commit 才删——按路径从历史剥离。② 本文 §0 与上一条增补、docs/20 §9、docs/43 T2 对测试
> 环境来历写得过细，改成中性说法（「真实环境」「旧邮箱」）。③ 一个还没 push 的 commit
> （`04e2be4`）在面板测试 fixture 里写进了本机登录名与主机名，`11d8cc2` 才在树上换成占位身份——
> 占位身份回填进前者，后者只剩一段注释，message 随之改写。一遍 `git filter-repo`（按路径剥离、
> replace-text、一条 message 改写），之后一个 commit 把 15 个文件里 22 处旧短 hash 等长换成新值
> （含三处代码注释的 ts/js 对与 vendored shlex 的头注）。替换表由环境变量现场生成、跑完即删。
> 验收：全部对象里登录名与四个草稿路径 0 命中、0 条 trailer、master 强推覆盖旧 master。GitHub 上
> Dependabot 的分支与 `refs/pull/*` 仍连着旧历史，直到删仓重建——那一步由 owner 决定。
> 重写前的完整备份：`../swiss-pre-scrub-2026-09-29.bundle`。

## 0. 审计结论（动手前的事实）

- 树和全历史里**没有** secret 形态的 token（sk-/ghp_/AKIA/PEM/JWT 都扫了），`.env`/config/key
  文件从未进过历史，`tests/fixtures` 用的是文档化的固定测试 key，IP 全是私网/文档段。
- 514 个 commit（含所有分支）全部署名 owner 的**旧邮箱**（`<英文名>@<域名>`）。154 条
  `Co-Authored-By:` trailer 署给五个模型账号。
- 四张截图带真实环境数据：`docs/assets/20/03-tunnels-groups-{light,dark}.png`（内网 IP、
  SSH 连接名、端口布局）、`docs/assets/20/06-data-dropdown-{light,dark}.png`（真实系统的
  Redis 会话 key 前缀）。其余截图是 g4/g6/g7 fixture 或只含 MCP 名。
- 382 个源文件头写 `Copyright 2026 The swiss authors`；17 个源文件没有头。
- `crates/swiss-mcp/src/adapters/zai_prompts.rs` 逐字来自 `@z_ai/mcp-server` 0.1.5
  （Apache-2.0，author Z.AI），头注却只写 swiss 自己。vendored xterm.js 5.5.0 + 五个 addon、
  cronstrue 2.52.0（MIT）：minified `.js` 不带 license 文本，只有 `xterm.css` 带。
- 50 处个人绝对路径（本机 `C:\Users\<user>\...` 三种写法，加上一台远端主机的
  `/home/<user>/<project>`），其中 `src/skill_assets/SKILL.md` 随二进制发布（后续拆分：现为
  `SKILL.md` + `remote/SKILL.md` 两份，见 docs/41 顶部）。
- Logo 是红盾白十字，SVG 注释自称 "the pocket-knife emblem"——Victorinox 注册徽标的构图；
  tagline "Swiss Army knife" 是其商标。
- 分支 `terminal-parity`（未合并，领先 94 commit）的历史里提交过
  `crates/swiss-panel/panel-tests/target-test/` 整棵 build 树（单个 rlib 21 MB，pack 110 MB）。
  九个已合并分支和七个 worktree（全部干净）还在。
- 依赖 license 全部宽松（MIT/Apache/BSD/ISC/Zlib/Unicode/CDLA-Permissive；`option-ext`
  MPL-2.0 是文件级 copyleft，二进制分发无义务）。无 GPL。
- crates.io 的 `swiss` 与 `github.com/young1lin/swiss` 都是 404（名字空着，代码已硬编码后者）。

## 1. 决定（owner，2026-09-21）

| # | 决定 | 落地 |
|---|---|---|
| D1 | 作者名 `young1lin`（owner 2026-09-21 追加：名字不用英文名；2026-09-23 追加：**任何文件不写邮箱**，签名一律裸名） | 历史重写 mailmap + replace-text 连邮箱一并剥离；Cargo `authors`、头注、NOTICE 均为裸名；联系方式走 GitHub 私有渠道 |
| D2 | 真实环境数据 git ignore 掉 | 四张截图退出 tracking + `.gitignore` + 从历史清除；不重拍 |
| D3 | 全局 Apache-2.0，**作者只能是 owner** | 头注改 `Copyright 2026 young1lin`；`Co-Authored-By` trailer 从历史剥离，新 commit 不再加；第三方归属**保留**（那是 license 义务，不是作者署名，见 D7） |
| D4 | 绝对路径剔除 | 工作树 + 全历史 `--replace-text` |
| D5 | 补标配文件 | NOTICE、THIRD_PARTY_NOTICES、SECURITY、CONTRIBUTING、CODE_OF_CONDUCT、CHANGELOG、issue/PR 模板、dependabot、cargo-deny、`rust-version` |
| D6 | 闭环——**只做本地部分** | CI 收窄权限 + deny job；建仓/push/tag 不做（owner 2026-09-21 追加："不要急着 github 登录"） |
| D7 | 不侵权 | 换 logo（去盾去十字）、tagline 不再用 "Swiss Army knife"；Z.AI / xterm / cronstrue 归属补齐 |
| D8 | 仓库卫生 | 已合并分支与其 worktree 删除；`terminal-parity` 保留分支、清其历史里的 build 树、移除 worktree 目录；根目录 ignore 的旧产物删除 |

## 2. 工作项

每项一个 commit，author `young1lin`，无 trailer。顺序是内容先、历史重写后
（重写把新 commit 一并覆盖，作者与替换规则对新旧一致）。

### O1 归属与头注（D3、D7）

- `scripts/add-apache-headers.ps1` 的 `$notice` 第一行改为 `Copyright 2026 young1lin`；
  全树 `Copyright 2026 The swiss authors` → 同一行（382 处，`sed`）。
- 跑一遍脚本补 17 个无头文件；脚本的扫描根加上 `.agents/skills/*/scripts`。
- `zai_prompts.rs` 头注下加 `Portions Copyright Z.AI — derived from @z_ai/mcp-server 0.1.5 (Apache-2.0)`；
  `zai.rs` 模块注释指向 THIRD_PARTY_NOTICES。
- `crates/swiss-panel/src/admin_assets/js/vendor/xterm/LICENSE`、`.../vendor/cronstrue/LICENSE`：上游
  MIT 原文（随 rust_embed 一起嵌入，和副本同行）。
- 根目录 `NOTICE`（Apache 惯例：产品名 + 版权行 + 指向第三方清单）。
- 根目录 `THIRD_PARTY_NOTICES.md`：三段手写（xterm、cronstrue、@z_ai/mcp-server）+ 一段从
  `cargo metadata` 生成的 crate → license → 仓库 表。
- Cargo：`[workspace.package]` 承载 `authors`/`license`/`repository`/`rust-version`，九个成员改为
  `.workspace = true`；`panel/package.json` 加 `license`、`author`。

### O2 脱敏（D2、D4）

- `.gitignore` 加两条 glob，`git rm --cached` 四张截图；docs/20 §9 注明第 3、6 项的截图不入库。
- 工作树替换（一张字面量表，同一张表在 O7 里喂给 `--replace-text`，本文故意不抄旧值——
  抄了就会被自己的规则改写）：
  - 本仓库的本机绝对路径（反斜杠 / 正斜杠 / WSL `/mnt/c` 三种写法）→ `<repo>`
  - 已退役 Node 兄弟仓的本机路径 → `<node-repo>`
  - 其余本机 home 前缀（三种写法）→ `~`
  - 远端示例路径 `/home/<user>/<project>` → `/home/dev/app`
  - 旧邮箱 → D1 邮箱
- 三个把路径当代码用的文件手改：`.agents/.mcp.json`（`npx -y chrome-devtools-mcp@latest`）、
  `.agents/skill-eval/check-syntax.sh`（`cd "$(dirname "$0")/../.."`）、
  `scripts/extract-zai-prompts.js`（SRC 取 argv/HOME，OUT 相对脚本自身）。
- `.png`/`.exe`/`Cargo.lock` 之外的 tracked 文件全部过表；改动 24 个文件。

### O3 标配文件（D5）

`SECURITY.md`（私密披露：GitHub private vulnerability reporting 或 D1 邮箱；范围：loopback
边界、密文存储、SSH/DB 凭据）、`CONTRIBUTING.md`（前置：Rust stable ≥ rust-version、Node 24
仅面板；三道 gate；面板改 `panel/src` 不改 emit；指向 AGENTS.md）、`CODE_OF_CONDUCT.md`（短版）、
`CHANGELOG.md`（Keep a Changelog，0.1.0 Unreleased）、`.github/ISSUE_TEMPLATE/{bug_report,feature_request}.md`、
`.github/PULL_REQUEST_TEMPLATE.md`、`.github/dependabot.yml`（cargo / npm / actions，每周）、
`deny.toml`（license allowlist = §0 实测集合；advisories deny）。

> 增补（2026-09-22，`062928a`）：CONTRIBUTING 的 The gates 段后来加入第四条——真库门
> `cargo test -p swiss-it --features it`（需要 Docker 或 `SWISS_IT_*_URL`）；上文的三道 gate 是
> 2026-09-21 创建时的记录。权威记录 docs/44 §2.8。

### O4 品牌（D7）

- 新 mark：红色圆角方块（`#DA291C` 保留——颜色不是商标）+ 白色"展开的三把工具"扇形
  （三条圆头短条从左下枢轴呈 0°/30°/60° 张开，枢轴一个白点）。没有盾，没有十字。
  三份同源：`assets/logo.svg`、`assets/logo-wordmark.svg`、`crates/swiss-panel/src/admin_assets/logo.svg`；
  `base.css` 里描述 logo 的注释同步。
- tagline：`A developer's Swiss Army knife` → `A developer's pocket multitool`（Cargo.toml、README、
  AGENTS.md ×2、`.agents/docs/README.md`、`.agents/docs/style-design.md`）。名字 `swiss` 不动。
- 验证：新 SVG 用真浏览器渲染成 PNG 目视一次（16px favicon 尺寸 + 64px）。

### O5 CI 与发布链路的本地部分（D6）

`.github/workflows/build.yml`：顶层 `permissions: contents: read`，只有 `release` job 提升到
`write`；新增 `deny` job（`EmbarkStudios/cargo-deny-action@v2`）。不建仓、不 push、不打 tag。

### O6 卫生（D8）

- 删除已合并分支 `figma-type mcp panel-i18n panel-ts ssh ssh-tunnel refactor-ui secret skill-eval-r1`
  及其 worktree（六个，均干净）。`task-b`（5 个 lmg 时代的 wip commit）与 `terminal-parity`
  保留分支；`terminal-parity` 的 worktree 目录移除（分支在，`git worktree add` 随时回来）。
- 删除根目录被 ignore 的旧产物：`lmg-dev.exe`、`term-*.png`、`one-sessions.log`；`conpty_ref.rs`
  是 owner 显式 ignore 的参考件，不动。`.gitignore` 去掉四条 `lmg-*`/`swiss*.exe` 里已无对象的项——
  不做，留着无害。

### O7 历史重写（D1、D2、D3、D4、D8）

先 `git bundle create ../swiss-pre-oss.bundle --all`（完整备份，含所有分支与
未重写的 hash），再一次 `git filter-repo --force`：

- `--mailmap`：旧邮箱 → `young1lin`（author 与 committer 一并）。
- `--message-callback`：删除所有 `Co-Authored-By:` 行（154 条）。
- `--invert-paths --path docs/assets/20/03-tunnels-groups-light.png ...`（四张）
  `--path crates/swiss-panel/panel-tests/target-test/`（terminal-parity 的 build 树）。
- `--replace-text`：O2 的同一组字面量。
- 之后：`.git/filter-repo/commit-map` 把 docs/AGENTS/.agents/README 里引用的**旧短 hash**映射成
  新短 hash（spec 状态头"哪个 commit 落地"是这个仓库的记账方式，不能变成谎言）；单独一个 commit。
- `git reflog expire --expire=now --all && git gc --prune=now --aggressive`。

## 3. 验收

1. `git log --all --format='%an <%ae>' | sort -u` 只有一行 `young1lin`。
2. `git log --all --format=%B | grep -ci co-authored-by` = 0。
3. 用 O2 那张表的**左列**做 `git grep -i`（工作树）和 `git log --all -p | grep -i`（全历史），
   本机用户名、远端用户名、旧邮箱域名三个词的命中都为 0（本文不写出这三个词，理由同 O2）。
4. `git log --all -- docs/assets/20/03-tunnels-groups-light.png` 为空；四张文件仍在磁盘、`git status` 不列出。
5. `git rev-list --objects --all | git cat-file --batch-check | awk '$3>5000000'` 为空。
6. 每个 `.rs/.ts/.js/.mjs/.mts/.css/.ps1/.sh/.yml` 源文件（vendor、emit 除外）头三行含 `Copyright 2026 young1lin`。
7. `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`npm run check` 绿。
8. `cargo deny check` 绿（本地装了就跑；没装则由 CI 首跑证明，记为未验）。
9. 新 logo 的 PNG 渲染目视通过。
10. `git branch` = `master task-b terminal-parity`；`git worktree list` 只有主 checkout。
