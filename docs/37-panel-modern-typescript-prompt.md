# docs/37 实施提示词 — 面板成为真正的现代 TypeScript

把下面整段交给一个**全新的**实施会话。写规范的会话不实施。

---

## 工作目录

`<repo>\.agents\worktrees\panel-ts`（worktree，分支
`panel-ts`，基线 `fddd33b`）。面板的 npm 家在 `crates/swiss-panel/panel`。

**不要 `cd` 回主 checkout。** PowerShell 里 `cd` 混在链式命令中会打断后面每一个相对路径——构建与
实例脚本各起一条命令，从仓库根跑。

## 任务

`docs/37-panel-modern-typescript-spec.md` 定义的 R0 → R6。一句话：docs/36 把面板搬进了 TypeScript，
但 D9（"一个 token 都不许改"）逼着类型去迁就没动过的 2015 年 JS——五处全局内建增补、37 处开放索引
签名、两个被关掉的 strict 子项、1,041 个 `!`。本文撤销 D9，让写法和类型一起归位，并用 eslint
ratchet 接替 D9 当机器闸。

## 先读这些（按顺序，别跳）

1. `docs/37-panel-modern-typescript-spec.md` —— 全文。§0.2 的三组实测是你要消掉的账单，
   §1.2 的 M1–M12 是决定，§9 的 ratchet 表是每阶段要上锁的规则。
2. `docs/36-panel-typescript-spec.md` §1.2 与 §10 —— 你要推翻的是 D9 与"不改写法"，
   **其余十二条决定继续有效**（尤其 D2 发射、D3 产物提交、D10 零 any、D11 类型即 API 契约）。
   注意：docs/36 的两个 md 目前只在主 checkout 里未跟踪存在，R6 要把它们一并入库。
3. `.agents/rules/panel-proof-of-life.md` —— 全文，逐条。面板改动被判"done"的唯一标准。
4. `AGENTS.md` —— 四条产品属性，以及第 90 行起"The panel is edited here, directly"（R6 要改写它）。
5. `crates/swiss-panel/panel/tsconfig.json` 与 `src/types/dom.d.ts` —— 账单本身，注释里写着
   每处妥协的理由，读懂了再动手。
6. `crates/swiss-panel/panel/build.mjs` —— 发射器。**它不是障碍**：ts-blank-space 逐行保号，
   `const` / 箭头 / 可选链 / `class` / 括号 cast 今天就能发射。

## 交付顺序

一个阶段一个或多个提交，不许跳阶段：

| 阶段 | 内容 | 关键风险 |
| --- | --- | --- |
| R0 | 文字撤销 D9、eslint 落地、73 处冗余 `$<HTMLElement>` | 无运行时面 |
| R1 | 49 个 `this` handler → `e.currentTarget`、99 个 `catch` → `errText`、删五处全局增补、恢复两个 strict 子项 | **`RegExp.test(null)` 今天靠强制转字符串工作**，逐点决策 + 配用例 |
| R2 | `var` → `const`/`let`、回调改箭头、`!` 收敛到 100 以下 | 箭头改 `this`；必须先做完 R1 |
| R3 | ambient 类型 → `export` + `import type`；37 处索引签名按 Rust serde 补齐 | `types/*.ts` 会被发射成空 js——文件名保持 `.d.ts` |
| R4 | `PanelState` 35 字段拆七片 | 每片一次实机走查 |
| R5 | `innerHTML` / 282 个内联 handler → 构造 + 事件委托，按视图逐个 | 最贵最险，每视图 proof-of-life |
| R6 | deploy / CI 门禁、AGENTS.md、ADR-024 与 ADR-025、README 两行 | 含 docs/36 欠下的 T5 |

## 门禁（每次提交前）

```
# 面板（在 crates/swiss-panel/panel 下，自己一条命令）
npm run check          # typecheck ×2 + lint + build:check + vitest 553

# 仓库根
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

实机验证（R2 / R4 / R5 每个阶段）：

```
scripts/test-instance.ps1 -Stop
# 改过 admin_assets 就先 npm run build，再 touch crates/swiss-panel/src/lib.rs，
# 否则 rust_embed 的指纹不变，release 二进制里还是旧面板
$env:CARGO_TARGET_DIR="target-test"; cargo build --release
scripts/test-instance.ps1 -Fresh
```

然后用 agent-browser 在 19998 上走**真实指针事件**（CDP input，不是 `element.click()`）：每层导航
能到、每个 seg 切换、每个 sheet `hidden === false` 且量得到 bounding rect、每个主操作发出请求且
UI 反映结果、空状态渲染。**走不了的流程（缺凭据、外部端点）如实列为未验证并写明原因。**

## 提交规则

- 一个提交一件事；提交说明写清"改了什么写法、为什么、哪些用例覆盖它"，并贴 §11 的计数。
- **行为改动必须点名并配用例**——D9 撤销后，这是唯一剩下的证明手段。
- 不许删测试来过关；不许整文件 `eslint-disable`（单行 disable 必须带理由注释）。
- 不许手改 `crates/swiss-panel/src/admin_assets/js/**`——那是发射产物，改 `panel/src/*.ts` 后
  `npm run build`。
- 提交信息结尾按仓库惯例署名。

## 不做什么

见规范 §12。特别是：不 bundle、不引 prettier、不引框架、不换发射器、不改 `/api/*` 形状、
不碰 19999、不碰 UI 的画法。

## 完成之后

`swiss-verify` → `swiss-live-verify` → `swiss-review` 三步走完，再向 owner 汇报每阶段的计数表。
