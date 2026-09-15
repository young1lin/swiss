# 26 — 密钥行序可拖：vault 增加第三张表 order（docs/20 G6 / docs/19 D6 修订）

> 状态：**已实施**（2026-09-15，随本 spec 一并提交）。前置：[docs/19](19-secret-vault-spec.md)（D5/D6）、
> [docs/20](20-groups-and-hierarchy-spec.md)（G-模型）、[docs/05](05-wire-compatibility.md)（密封格式不动）。
> 需求原文（用户，2026-09-15）："全局就应该要这样，这就是可拖动的，因为我想把某个 API Key 放到第一位，
> 好复制，不行吗？我就要可拖动，可以跨组，可以拖动"

## 0 — 为什么推翻 docs/20 的旧取舍

docs/20 曾为 secrets 选择"无手动序"（行按名字序，order 路由 400），理由是"为 nothing 付第三张表"。
用户现在给出了那个 something：把常用的 API Key 拖到第一位，好复制。这是产品决定——成本照付（§1 的
减免设计把它压到一次完整模型写的加段），行为对齐全局：六个作用域里 secrets/tokens 是仅有的两个不可拖
作用域，secrets 从此退出这组少数派（tokens 维持不变：token 行没有"拖到第一位去复制"的用例）。

## 1 — 语义（E1）

- vault 文件形状从 `{rev, secrets, groups, secretGroups}` 增加为 `{..., "order": [name...]}`——
  密封格式（docs/05）不动，动的只是里面的 JSON。
- order 是第三张表，与 groups/secretGroups 同一次 rev 检查写落盘（family 的每一次 mutation 仍是
  一次完整模型写；值写入 PUT secret / import 不动 order，新名字自然落到已提及之后）。
- 排序规则与 tunnels store 的 rank 一致：order 提到的名字按槽位排；未提及的排在其后、彼此按名字序；
  **order 为空 = 名字序**（老文件零迁移，行为不变）。
- 容忍与修剪：order 里的陈旧名字（已删密钥）不报错、渲染时忽略；DELETE 修剪自己的名字；set_order
  丢弃未知名字与重复（保首个槽位）——stale 面板无法植入幽灵行。

## 2 — API（E2）

- `GET /api/secrets` 增加 `"order": [...]`（原样返回存储列表，含陈旧名）。
- `PUT /api/groups/secrets/order` 从 400 变 200：body `{order: [name...]}`，响应 `{order: [...落地后]}`。
  族协议不变——一个族六个作用域，同一形状。

## 3 — 面板（E3）

- secrets 作用域从 `draggable: false` 切到组件的完整家族契约：行可拖（组内换位）、可跨组（一次手势
  同时落 order + membership）、抓手仍拖组。
- `moveSecretRow`：本地先动、立即重渲、PUT 平铺 order、随后 loadSecrets 重取——order PUT 会 bump
  rev，下一次值写入必须拿到新 rev，reload 不是装饰。
- 空组文案自动升级（组件按 draggable 选择 "drop here" 版本）。

## 4 — 验收

- rust：secretstore order round-trip / DELETE 修剪 / 未知名容忍；adminapi：order PUT 200 + 未知名
  被丢弃 + GET 反映。
- vitest：按存储序渲染（未提及按名字序垫后）；moveSecretRow 发出平铺 order PUT 并 reload。
- 19998 实机：真实拖拽把一个 key 拖到第一位（跨组一次手势），刷新后位置保持；Store 新 key 落末尾；
  DELETE 后列表与 order 一致。
