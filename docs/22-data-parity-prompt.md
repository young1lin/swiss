# 22 hand-off — Data 全量对齐实施提示词(fresh session 起点)

> 你是一个全新会话,从零实施 [docs/22-data-parity-spec.md](22-data-parity-spec.md)。本文件自包含;spec 是唯一任务书。

## 工作区

- 仓库:`<repo>`(建议自建 worktree,分支名 `data-parity-w<批次>`)。
- Node 兄弟仓(面板唯一真源):`<node-repo>`,面板在 `src/admin/`。**先改 Node 侧 → 整树拷回 `crates/swiss-panel/src/admin_assets/` → 字节级测试必须过**;直接改 admin_assets 是违规。
- 参照库(只读,勿改勿重建):`~\dev\terminals-ref\{adminer,dbgate,cloudbeaver,pgadmin4,pgweb}`;spec 里 `<repo>/path:N` 都指这里。

## 先读(顺序)

1. AGENTS.md(仓库根)——四性质与红线,尤其 no-subprocess、no-new-deps、current_thread。
2. docs/21(差距总表,每个 W 项的"参照"都能在这里找到上下文)。
3. docs/22(spec 正文,含 §1 通用约束与 §11 交付顺序)。
4. skills:`swiss-design`(面板宪法)、`swiss-node-reference`、`swiss-verify`、`swiss-live-verify`、`swiss-review`。
5. 代码:`crates/swiss-data/src/dbbrowser_api.rs`、`crates/swiss-host/src/dbbrowser.rs`、`crates/swiss-mcp/src/adapters/{mysql,pg,redis}_browser.rs`、面板 `data-*.js` 十个文件。

## 交付

- 按 spec §11 顺序,一个 W 项一个 commit,标题 `(docs/22 Wx.y)`。
- 每项提交前门禁:`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo tree -d`,加面板字节测试。
- 行为变化带测试(失败于前、通过于后);面板纯函数在 Node 仓补用例。
- Live 验证:19998(`scripts/test-instance.ps1`,AGENTS.md 全流程);**绝不碰 19999**。
- 每完成一批:swiss-review;W4.4 完成后按 swiss-memory-record 记录内存读数进 docs/01;全部完成:README 状态行更新 + ADR-015(§10)写进 docs/07。

## 本机纪律(会话内约定)

- subagent 并发 ≤ 2(排队分批)。
- 不修改 `~/.rh/AGENTS.md` 或任何全局配置/记忆文件——会话级约定留在会话里。
- 测试实例一律 19998(仓库标准);19997/19999 都不属于你。

## 红线速查

无新 crate 依赖;无子进程;`serde_json::Value` 不上转发路径;不引入配置开关(新特性默认启用);SQL 一切标识符走白名单引号;Redis 命令守卫的拒绝名单只按 spec 放白(DEL/RENAME/EXPIRE/PERSIST),其余不动;错误消息保持"驱动原文即答案"。
