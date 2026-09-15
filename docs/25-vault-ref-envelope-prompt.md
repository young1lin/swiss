# 25 — 密钥引用信封语法 ${secret://name} — 实施交接提示词

工作目录：`<repo>`（cargo workspace，Windows）。你是全新会话，不带前文；
本文件 + docs/25 是你全部的任务上下文。按顺序先读：

1. `AGENTS.md` —— 载重规则高于一切（loopback、引用不落值、lazy proc、封印冻结、current_thread、转发路径
   无 `serde_json::Value`、LF、测试纪律）。Item 5 会更新其中凭证规则句。
2. `docs/25-vault-ref-envelope-spec.md` —— 任务本体；§2 E1 的判定表是验收的单一事实源，逐行落成表驱动单测。
3. `docs/19-secret-vault-spec.md` —— 父 spec；仅 D1 语法条款被修订，D2-D9 全部有效。
4. `crates/swiss-core/src/secure/refs.rs` —— 要改的扫描器：vault 分支移入 `${...}` 信封（env 分支语义不变），
   新增 `migrate_legacy`。注意模块注释里的既有约定（advance-one、不重扫替换产物）。
5. `crates/swiss-host/src/config.rs` —— `resolve_env_refs`（宽松包装，勿动语义）、`is_env_ref`（双形态）、
   `resolve_def_checked`（严格合同）。
6. 三个加载点：swiss-host 的 defs 加载（gateway.config.json / managed.json）、`crates/swiss-tunnels`
   （tunnels.json）、`crates/swiss-jobs/src/jobs/runner.rs`（jobs.json）——`migrate_legacy` 的接入位。
7. 面板：`crates/swiss-panel/src/admin_assets/js/views/secrets.js`（复制按钮/toast/删除确认/文案）、
   `js/fields.js`（两处 hint）；vitest 在 `crates/swiss-panel/panel-tests/`。

任务摘要：vault 引用从裸 `secret://name` 改为 `${secret://name}` 信封语法；无信封的 `secret://` 一律字面量
直通（含 URL，与 vault 状态无关）；加载时整值精确匹配自动迁移（不强制回写盘）；信封内 `secret://` 文法硬
校验；失败语义与六使用面合同不变。

交付顺序（spec §3，每项一提交、测试先行——改前红、改后绿）：
Item 1 core 文法 → Item 2 host ‖ Item 3 tunnels+jobs → Item 4 panel → Item 5 docs。

每提交门禁：`cargo test --workspace`；`cargo clippy --workspace --all-targets -- -D warnings`；`cargo tree -d`
（无新依赖，空跑）；面板改动另跑 vitest 全量 + `node --check`。
既有测试里所有裸引用 fixture 一并改写（spec §3 末的 grep 清单）——漏一个就是红灯，不是可选项。

面板验证按 `.agents/rules/panel-proof-of-life.md`：真浏览器、真点击、19998 新起实例（`scripts/test-instance.ps1
 -Fresh`），验收照 spec §4 七条，截图留证；无法验证的条目如实标注 NOT verified。
**19999 是用户的实例，全程不动**；部署只走 `scripts/deploy.ps1`，是最后一步，且只在用户看过 19998 之后。

代码注释一律英文；LF；不新增依赖；不碰封印格式（docs/05）。偏差（措辞、命名、结构）记进提交信息与最终汇报；
颜色/文案类留给用户过目。
