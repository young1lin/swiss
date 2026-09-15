# 25 — 密钥引用换信封语法：${secret://name}（docs/19 D1 修订）

> 状态：**已实施**（2026-09-15，已合并 master）：Item 1 文法 `7b49a82`、Item 2 host 迁移 `dc70477`、
> Item 3 tunnels+jobs `4bf2566`、Item 4 面板 `44a49a5`、Item 5 文档收尾（本提交）。基线 `05e21e5`。
> 配套交接提示词：[25-vault-ref-envelope-prompt.md](25-vault-ref-envelope-prompt.md)。
> 决策记录：ADR-019（supersede ADR-014 的语法条款；ADR-014 的存储/rev/write-only/隔离条款不动）。
> 前置阅读：`AGENTS.md`（规则高于本文）、[docs/19](19-secret-vault-spec.md)（父 spec——仅 D1 语法条款被本文取代，D2-D9 全部有效）、
> [docs/07 ADR-014/ADR-007](07-decisions.md)、[docs/05](05-wire-compatibility.md)（本文不动密封格式）。
> 调研底稿：`<vendor>/model-apikey-config-survey.md`（同类产品密钥设置/读取形态；结论已内联 §0）。
> 代码注释与 UI 文案一律英文；文档散文中文。

> 需求原文（用户，2026-09-15）："找到相关的密钥设置相关的，我建议改成 ${secret://api-key} 这种形式。
> 1. 主要的字符串和这个不一样。 2. https://test.com/secret://aaa/test 碰到这种你怎么办？你根本就没办法解析，懂吗"
> 同日追加："你要不用 zhipu_search 调研下？放到 @<vendor> 下，其他的模型怎么设置的，怎么取这个设置的？"
> ——调研完成后拍板："那你落新的 spec"。Round-1 六问未逐条作答，全部按推荐值执行（§2 各条注明）。

## 0. 现状与缺口（为什么是它、为什么是现在）

> 本节行号是基线 `05e21e5` 的证据坐标，实施后按函数名找，不按行号找。

docs/19 D1 把 vault 引用定为裸 URI scheme 型 `secret://name`，已随 ADR-014 全量落地。缺口两个，都在语法层，
都不动 vault 的存储、API、隔离与失败语义：

1. **无定界符 → 引用与正文不可区分**。扫描器（`crates/swiss-core/src/secure/refs.rs` 的 `resolve`，基线 40-61 行）
   对字符串里任何位置的 `secret://` 后随 kebab run 都按引用处理：`https://test.com/secret://aaa/test` 里的
   `secret://aaa` 会被圈走——名字在库里，URL 被静默改写且密钥被塞进 URL；不在库里，整条配置硬失败。
   现有放行仅覆盖"后随标点/结尾"（基线 206-208 行测试），恰护不住 URL 情形。这不是实现 bug，是裸 scheme 在
   "token 嵌在任意字符串里"场景下的语法不可判定——用户例 2 指出的正是它。
2. **与主力字符串形态不同框**。本库凭证串的主形态是 `${...}` 插值族（`Bearer ${TOKEN}`，用户例 1）；裸 scheme
   自成一边界体系，肉眼与文法都不同框。

调研结论（底稿见 header）：凡"引用嵌在更长字符串里"的产品全部用成对定界符信封——MCPHost `${env://VAR}`
（信封+scheme，与本提案同构）、Cursor `${env:NAME}`、Continue CLI `${{ secrets.X }}`、LibreChat/Kiro `${VAR}`、
Spring `${NAME:default}`；裸 scheme 只活在专用整字段（LiteLLM `os.environ/`，且 issue #8919 持续有人踩坑）或
结构化字段（k8s `valueFrom.secretKeyRef`）。无人在任意字符串里裸扫 scheme。

## 1. 目标与非目标

**目标**

- vault 引用改为 `${secret://name}`：`${...}` 信封给 token 边界，信封内 scheme 保留自描述（错误文案、grep、
  未来 `?version=` 扩展）。
- 无信封的 `secret://` 一律字面量直通——用户例 2 的 URL 字节原样，与 vault 状态无关。
- 一次性迁移：加载时**整值精确匹配**自动改写；盘上惰性落盘。
- 信封内文法硬校验：`${secret://` 开头即意图声明，非合法 kebab 名硬失败。
- 其余一切不动：`${ENV}` 宽松语义、secret 硬失败语义、docs/19 D4 六使用面合同、封印格式、/api/secrets、
  面板 Secrets 区结构、export/import。

**非目标**

- 不做双文法长期共存（URL 歧义永存，等于没改——Round-1 Q1(b) 否决）。
- 不做混合串（`Bearer secret://x`）自动迁移：自动改写混合串需要做那个歧义扫描本身；面板手工重存。
- 不加 `$${...}` 转义：env 族今天也没有，口径一致，照字面输出。
- 不动 docs/19 §5 继承的全部"不做"（reveal、轮换、token 迁移等）。

## 2. 设计

### E1 文法与扫描器（refs.rs：vault 分支移入信封）

一个扫描器，两条信封内文法，**信封外无引用**：

- 遇 `$` 且下一字符为 `{`：向后找**第一个** `}`，`content` 为其间文本（不含定界符）：
  - `content` 以 `secret://` 开头：其余部分匹配 `^[a-z][a-z0-9-]{0,63}$` → vault 引用。命中：替换为值，
    越过 `}`；缺失：硬失败 `references secret://{name} which is not in the vault`（措辞不变）。不匹配
    （`BadName`、空名、带空格）：硬失败 `invalid reference '${…}' — secret names are lowercase kebab
    ([a-z][a-z0-9-]{0,63})`——宁拒不猜。
  - 否则 `content` 匹配现行 env 文法（非空 `[A-Z0-9_]+`）→ env 展开，缺失空串（不变）。
  - 否则：`$` 照抄，前进**一个**字符——advance-one 语义不变，`${lowercase}` 等字面量行为由既有测试锁定。
- 信封外一切照抄，**包括裸 `secret://`**。替换结果不再扫描（不变）。仍为手写字符扫描，不进 regex（ADR-007）。

判定表（单测照此表驱动，缺一行不算数）：

| 输入 | vault 状态 | 结果 |
|---|---|---|
| `${secret://api-key}` | 有 | 替换为值 |
| `Bearer ${secret://api-key}` | 有 | `Bearer <值>`（混合前缀，用户例 1 的形态） |
| `https://x/?k=${secret://api-key}&t=1` | 有 | query 内替换，前后段不动 |
| `https://test.com/secret://aaa/test` | `aaa` 有与无**各测一次** | **字节原样直通**（用户例 2） |
| `${API_KEY}` / `${missing_env}` | — | env 展开 / 空串（不变） |
| `${lowercase}` / `$NOPE {a}` | — | 字面量（不变，既有测试） |
| `${secret://BadName}` / `${secret://}` / `${secret://a b}` | — | 硬失败，文案讲 lowercase kebab |
| `${secret://api-key`（未闭合） | 任意 | 字面量直通 |
| `${${secret://a}}` | 有 `a` | `{$<值>}`（外层 `$` 字面量 + 内层替换） |
| 替换产物含 `${X}` | — | 不再扫描（既有测试迁形） |

前进兼容性质保留：旧构建读 `${secret://x}`（不匹配 `[A-Z0-9_]+`）→ 字面量直通 → 可见的坏（401），不静默泄密。

### E2 迁移：加载时整值改写，盘上惰性落盘（Round-1 Q1(a)、Q2 推荐值已采纳）

- `refs.rs` 增 `migrate_legacy(v: &mut Value)`：树遍历同 `resolve_value`，只改字符串叶；整值**精确**匹配
  `^secret://[a-z][a-z0-9-]{0,63}$` → `${secret://name}`，其余一律不动；幂等。
- 接入三个加载点（加载后、入内存前调用）：swiss-host（gateway.config.json / managed.json 的 defs 加载）、
  swiss-tunnels（tunnels.json）、swiss-jobs（jobs.json）。每个文件打一行 info 日志：
  `managed.json: 2 legacy secret:// reference(s) migrated to ${secret://...}`。
- **不强制回写盘**：内存已是新形态，功能立即正确；盘上引用在下次任何保存时自然落盘为新形态。
- 混合串（`Bearer secret://x`）**不迁移**（见非目标）；面板编辑表单手工重存——E4 的复制按钮让这活是一次粘贴。
- export/import 不改：bundle 读盘上文件，未落盘的旧引用随 bundle 走，导入侧加载时同样迁移（幂等）。
- `is_env_ref`（`crates/swiss-host/src/config.rs`，基线 107-120 行）：整值 `${secret://name}` 认定为引用；
  整值裸 `secret://name` **过渡期继续认**——未迁移盘文件经 API 显示时仍是引用串，不进掩码值。

### E3 失败语义与六使用面（不变，重申）

docs/19 D4 的通用合同与六使用面清单原样有效：http/rest 的 header/url/body、proc 的 args/env、隧道的
password/keyPassphrase 与 rule 字段、job 的 command/env、面板 Test 端点、mcpmatch。失败语义不变：
`${ENV}` 缺失空串；`${secret://x}` 缺失硬失败，错误点名使用面与 JSON 路径，永不带值（文案里引用名保持
裸名 `secret://context7`，与措辞现状一致）。D2/D3 存储、D5 API、D7 export、D8 隔离全部不动。

### E4 面板与文案

- `js/views/secrets.js`：desc 行（基线 55）、复制按钮（182，复制 `${secret://name}`）、存储 toast（209）、
  删除确认（215）、空态 hint（105）——引用展示统一为 `${secret://name}`。Secrets 区结构不动。
- `js/fields.js` 两处 hint（基线 64、80）：改为 `Prefer a reference — ${secret://name} (store it once on the
  Plugins page) or ${ENV_VAR} ...`。
- vitest：secrets 视图既有断言（复制内容、toast 文案）随改；"整值引用显示不打码"契约在两形态上回归。

### E5 文档与决策记录（随实施收尾提交）

- docs/19 D1 加注：语法条款由 docs/25 修订为 `${secret://name}`，D1 的 survey 与理由保留为历史。
- ADR-019 入 docs/07：选项表（裸 scheme / 信封+scheme / 双文法）、结论、代价（一次性迁移 + 混合串手工重存）；
  ADR-014 标注语法条款 superseded by ADR-019，其余条款不动。
- AGENTS.md 载重规则句更新为两形态：凭证是 `${ENV_VAR}` 或 `${secret://name}` 引用，绝非字面量。
- README docs 表 docs/25 行随 spec 提交即加。

## 3. 实施顺序与门禁

每项一提交、测试先行（改前红、改后绿）。门禁全套：

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -d            # 无新依赖，此项为空跑
```

面板改动另跑 `crates/swiss-panel/panel-tests/` vitest 全量 + `node --check`。19998 实测按 §4
（先 scripts/test-instance.ps1 -Fresh）；19999 全程不动，部署照旧最后一步且只走 scripts/deploy.ps1。

- **Item 1（core：文法）**：refs.rs 信封文法 + E1 判定表单测 + `migrate_legacy` 及其单测。无依赖。
- **Item 2（host）**：`is_env_ref` 双形态 + defs 加载迁移 + boot 日志；swiss-host 既有测试的裸引用 fixture
  改写。依赖 Item 1。
- **Item 3（tunnels+jobs）**：两加载点迁移；ssh.rs、mcpmatch.rs、runner.rs 的 fixture 改写。依赖 Item 1，
  与 Item 2 可并行。
- **Item 4（panel）**：E4 文案与复制 + vitest。依赖 Item 2。
- **Item 5（docs 收尾）**：E5 全部 + README 行状态更新 + 19998 验收记录。依赖 Item 1-4。

既有测试改写清单（基线 grep `secret://` 于 *.rs 与 admin_assets 的全部命中）：refs.rs 测试、config.rs、
actions.rs（基线 740-756）、ssh.rs（903/911）、mcpmatch.rs（131）、runner.rs（14，注释）、adapters/mod.rs
（286，注释）、secrets.js / fields.js 与对应 vitest。改写原则：引用一律 `${secret://name}`；原"裸 scheme 后随
标点直通"用例改写为用户例 2 的 URL 直通用例（vault 有/无名两种状态）。

## 4. 验收清单（19998，全部要过）

1. 面板存 `context7`；managed.json 手写整值 `secret://context7` → 起 19998：MCP Test 通过，boot 日志一行
   迁移记录，API 返回的 def 显示 `${secret://context7}`。
2. 面板编辑该条：表单显示引用串；保存后盘上是 `${secret://context7}`。
3. header 写 `Bearer ${secret://context7}` → Test 通过（混合前缀）。
4. url 填 `https://test.com/secret://aaa/test`：请求原样发出（echo 探针断言字节不变）；`aaa` 在库时同样
   直通——有/无 `aaa` 各验一次。
5. 删除 `context7` → 启动被拒，reason 点名 `secret://context7`；值不出现在任何输出。
6. `Bearer ${secret://BadName}` → 硬失败文案讲 lowercase kebab。
7. 回归：job `cmd /c set` 输出不含库内密钥；export 含 secrets 段、import 后引用照常解析（docs/19 §4 第 3/6 条）。

## 5. 不做什么（重申）

双文法长期共存；混合串自动迁移；`$${...}` 转义；reveal；轮换/过期；封印格式；`${ENV}` 语义；token 迁移；
docs/19 §5 的全部"不做"照旧继承。

## 6. 交接

实施从 [25-vault-ref-envelope-prompt.md](25-vault-ref-envelope-prompt.md) 起新会话（本会话只写 spec，不实施）。
本文与 docs/19 的冲突以本文为准，且仅限 D1 语法条款。
