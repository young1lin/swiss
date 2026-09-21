# 28 · 实现会话起跑提示 — MCP revisions 与禁用正名

你是全新实现会话,不带前史。本文件 + `docs/28-mcp-revisions-and-disable-spec.md` 是全部输入。

## 工作目录与分支

- worktree:`.agents/worktrees/mcp`(分支 `mcp`,已含基线 `a178e3d`)。shell 一律以
  `.agents/worktrees/mcp` 为 workdir;文件工具路径前缀 `.agents/worktrees/mcp/`。
- 嵌套 powershell 脚本一律绝对路径(相对路径会漂到别的 worktree —— 踩过的坑)。

## 任务一句话

给 MCP 加同名替换与回滚(revisions)、把 Stop 正名为 Disable、把 Rename/Disable 放到行菜单。

## 先读(按序)

1. `docs/28-mcp-revisions-and-disable-spec.md` — 需求、判定、验收、交付顺序,全部以它为准。
2. `AGENTS.md` — 四条产品性质与载重规则;`docs/07` 的 ADR-021/022 看行文格式。
3. 缝隙文件:`src/adminapi.rs`(build_def/add_managed/lifecycle_route/1525 一带的 override)、
   `crates/swiss-host/src/managed.rs`(managed.json 读写,新键 revisions 加在这)、
   `crates/swiss-mcp/src/registry.rs`(register/rename/remove —— replace 要重注册)、
   `src/app.rs` 378–388(503 文案)、`crates/swiss-panel/src/admin_assets/js/pane.js`
   (菜单)、`detail.js`(renameMcp/act)、`groups.js`+`menu.js`(行省略号菜单的 docs/18 V5 模式)。

## 交付顺序(spec §6,一提交一步)

1. D1 后端 + Rust 测试 1–8 + ADR-023;2. D1 面板 + vitest 9;3. D2 + vitest 11;4. D3 + vitest 10。

## 纪律

- 英文代码注释;测试先红后绿;`cargo test --workspace` 与 clippy `-D warnings` 每步全绿;
  零新依赖。
- 面板改动走 panel-proof-of-life 全清单(真 CDP 点击;验证实例端口 **19997** ——
  `scripts/test-instance.ps1 -Port 19997`,构建 `CARGO_TARGET_DIR=target-test`,绝对路径脚本;
  19999 是生产,只许读 /health;19998 留给操作者,别占)。
- revision 只是 def 快照,永不运行、boot 不注册 —— 这是 ADR-023 的判定,别把它做成第二套注册表。
- 凭据仍走 `${...}` 引用;revisions 里的 def 快照原样存引用,不展开。

