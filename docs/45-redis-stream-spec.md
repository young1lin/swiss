# 45 — Data 页的 Redis Stream：最新优先、游标翻页、Follow 环形缓冲、消费组只读

> 状态：**待实施**（spec 定稿 2026-09-22；基线 master `f3b6899`，即 `integration-harness` 合并进 master 的那次
> merge——seed 与 gate 2 都在里面，docs/44）。**实施前提**：docs/40 二次清理的 filter-repo 已在 master 上做完——
> 否则又是一条分支叠在待清理的历史上（D5）；filter-repo 之后这个短 hash 会变，按 subject 找。实施从
> [45-redis-stream-prompt.md](45-redis-stream-prompt.md) 起步，走 swiss-add-plugin → swiss-verify →
> swiss-live-verify → swiss-review 流程。
>
> 需求原文（owner，2026-09-22）：
> 「这个 data 的 Redis 的 Stream key 的测试做了吗？怎么显示推送的内容，而网页不会被撑爆呢？我有一堆行情信息
> 推送的。还有，页面怎么设计呢？这些有做吗？」
> 追加（同日）：「所有的功能点，都要有测试的哦，都要保证有集成测试，单元测试，才能通过的。以及设计思路，不仅
> 要写在 spec 中，还要在相关代码汇总有注释，为什么这么设计之类的」「要求有有丰富的测试」
> 五个决定按推荐通过（「可以」，见 §1）。
>
> 增补（2026-09-22，HEAD `13ec65c` 复核）：§0 的 file:line 逐条核对，全部仍指所说内容；唯一错处
> 是工具名——代码里没有 `redis_get`（全仓零匹配），该 MCP 工具实名 `redis_read`（`redis.rs:77`），
> §0.1、D8、§4 第 7 条、§5 四处已改正；行号引用未动。

## 0. 现状与缺口（读代码得出，逐条给 file:line）

### 0.1 后端：能读，但读的是最老的一端，且没有翻页

- `crates/swiss-mcp/src/adapters/redis.rs:538-566`：`type_aware_read` 的 `"stream"` 分支——`XLEN` +
  `XRANGE key - + COUNT (offset+limit)`，再在内存里 `skip(offset).take(limit)`。**方向是从最老的条目起**，
  `offset` 越大 XRANGE 拉得越多（"one bounded overshoot"）。
- `crates/swiss-mcp/src/adapters/redis_browser.rs:266-269`：浏览器的 `read_key` 写死
  `type_aware_read(.., 0, 1000)`——面板打开一个 stream 键，拿到的永远是**最早的 1000 条**，`truncated: true`
  以外没有任何继续读的手段（`RedisBrowser` trait 没有第二个读法，
  `crates/swiss-host/src/dbbrowser.rs:758-777`）。
- MCP 工具 `redis_read`（`redis.rs:82-93`）有 `offset/limit`（上限 1000），也是按"最老起的排名"分页。
- `redis.rs:146-147`：`XREAD` / `XREADGROUP` 在阻塞命令黑名单里——**这是对的**（docs/22：一条共享连接，
  阻塞命令会把整个浏览器挂住），本文不动它；后果是"追新"只能轮询，见 D6。
- `redis.rs:331-350` 的 `stream_entries`（把 `[id, [f, v, …]]` 变成 `{ id, fields }`）**没有单元测试**。

### 0.2 面板：stream 落在兜底分支，一堵 JSON 墙

- `crates/swiss-panel/panel/src/data-browsers.ts:156-159`：`DB_REDIS_TYPES` 只配置了 hash / zset /
  list / set；stream 没有表格配置。
- `data-browsers.ts:325-340`："Stream and module types stay read-only" 之后，把整个 value
  `JSON.stringify(value, null, 2)` 塞进一个 `<pre class="db-ddl">`。1000 条 × 每条十几个字段的缩进 JSON
  是几百 KB 的文字，只读、不刷新、看不到消费组。
- `panel/src/types/api.d.ts:756-766`：`ApiDbRedisValue.value` 是 `unknown`——面板对 stream 的形状一无所知。
- 树和过滤已经认识 stream：`db-state.ts:56` 的 SCAN TYPE 过滤含 `stream`，`data-tree.ts:76` 把
  `stream:orders` 当普通键挂在命名空间下。也就是说，**找得到、打不开**。

### 0.3 测试：零

- swiss-it 的 seed `crates/swiss-it/seed/redis/keys.txt` 是五种类型（string/hash/list/set/zset），**没有
  stream 键**——docs/44 §2.4 写"五种类型"时漏了它，本文 S0 补上。
- `crates/swiss-mcp` 里没有针对 stream 分支的单测；面板 vitest 只有 cell 菜单提到一次 `"stream"`
  （`admin-data-redis-cellmenu.test.ts`）。
- docs/22 W3.3 对 stream 的全部设计是一句 "stay read-only"。

### 0.4 对行情场景意味着什么

一个持续 XADD 的行情 stream（几百万条，每秒几十到几百条）：页面**不会撑爆**（硬上限 1000 条），但

1. 看到的是**最早的** 1000 条，对 feed 毫无用处；
2. 1000 条缩进 JSON 是一堵墙，找不到字段列；
3. 不会刷新，看不出还在不在推；
4. 看不到消费组的 pending / lag——排查"消费端卡住了"要靠 `redis_command` 手敲 `XINFO`。

### 0.5 宪法检查（AGENTS.md 四条属性）

| 属性 | 本文怎么守 |
| --- | --- |
| Ruthlessly small | 网关侧**什么都不持有**：没有订阅、没有服务端定时器、没有缓存；Follow 是面板的一个 `setInterval`，只在 tab 活跃且页面可见时跑（`data-activity.ts:28-47` 的既有规矩），每 tick 最多 3 条 redis 命令（§2.2 有证明测试）；DOM 里最多 500 行（D1） |
| Plugin-shaped | 全部落在 swiss-mcp 的 redis browser、swiss-data 的路由、面板的 `data-*.ts`；host 只在 `RedisBrowser` trait 上加两个**带默认实现**的方法（docs/43 M3 `list_databases` 的先例），不加 match arm |
| Hot-pluggable | 连接 stop 之后没有任何新东西要释放（没持有）；tab 关闭定时器随之停（同 activity 监视器） |
| Three ways | 不涉及 |

docs/05：磁盘与线上格式不动；两条新 HTTP 路由是**新增**，旧 `/key` 的响应对非 stream 类型逐字不变。

## 1. 决定（owner，2026-09-22，全部按推荐通过）

| # | 决定 | 落在 |
| --- | --- | --- |
| D1 | 默认窗口 **100** 条；面板 DOM 上限 **500** 行（环形缓冲，超出从底部丢） | §2.2 §2.3 |
| D2 | Follow 轮询默认 **1 s**，页面可选 1 / 2 / 5 s；只在 tab 活跃且 `document.hidden === false` 时跑 | §2.3 |
| D3 | 列 = 窗口内字段的**并集，首见顺序**；缺字段的行显示空；Follow 期间并集累积 | §2.2 §2.3 |
| D4 | 消费组：只读的 `XINFO GROUPS` 面板（name / consumers / pending / lag / last-delivered-id）；**不做** XACK / XCLAIM / XTRIM / XDEL 的 UI | §2.4 |
| D5 | 排期：`integration-harness` 合并 + master 的 filter-repo 之后再开 | 状态头 |
| D6 | （事实推出，owner 知悉）`XREAD` / `XREADGROUP` 保持禁用，追新用 `XREVRANGE` 轮询——共享连接不能被阻塞命令占住（docs/22） | §2.1 |
| D7 | （owner 规则）**每个功能点单测 + 集成测试都要有才算过**；设计理由 spec 里一段、代码旁边再一段 | §2.5、§3 每项 |
| D8 | MCP 工具 `redis_read` 的 stream 语义**不动**（offset/limit 从最老起）；新能力只走浏览器 API。原因：工具合同已发布给 agent，改方向是破坏性变更，另开 spec | §5 |

## 2. 设计

### 2.1 服务端：一个窗口函数，两个方向，一条命令

```rust
// crates/swiss-host/src/dbbrowser.rs — RedisBrowser gains two methods, both defaulted so every
// existing implementation and every StubRedis keeps compiling (docs/43 M3 did list_databases
// the same way).
pub trait RedisBrowser: Send + Sync {
    // ... existing ...
    /// docs/45: a newest-first window of a stream key. `o` = { before?: id, after?: id, count?: n }.
    async fn read_stream(&self, key: &str, o: &Value) -> Result<Value, String> {
        Err("stream windows are not supported by this connection".into())
    }
    /// docs/45: XINFO GROUPS, read-only.
    async fn stream_groups(&self, key: &str) -> Result<Value, String> {
        Err("stream groups are not supported by this connection".into())
    }
}
```

**方向**：两个方向都用 `XREVRANGE`，只有边界不同——最新窗口 `XREVRANGE key + - COUNT n`；往老翻页
`XREVRANGE key (<before> - COUNT n`；追新 `XREVRANGE key + (<after> COUNT n`。排他边界 `(` 是 Redis 6.2 的
语法，这是本文的最低服务器版本（seed 引擎是 7）；老服务器会回 `ERR syntax`，原样透传并带上键名。
**为什么不用 XRANGE 追新**：追新时如果积压超过 `count`，用户要的是**最新**的那 `count` 条而不是最老的——
`XREVRANGE + (after` 天然给最新的，多出来的用 `more: true` 告诉面板"中间有跳过"。

**响应形状**（`/key` 对 stream 键、`/stream` 两个方向共用）：

```json
{
  "key": "stream:ticks", "type": "stream", "ttl": -1,
  "length": 10000, "firstId": "1700000000000-0", "lastId": "1700000999900-0",
  "entries": [
    { "id": "1700000999900-0", "ts": "2023-11-14T22:29:59.900Z", "fields": { "sym": "AAA", "px": "1.2345", "qty": "10", "side": "b" } }
  ],
  "columns": ["sym", "px", "qty", "side"],
  "more": true
}
```

- `entries` **永远最新在前**（两个方向都是），面板不再排序。
- `ts` 由服务端从 id 的毫秒段算出（ISO 8601，毫秒精度）——agent 和面板都用得上，只算一次。
- `columns` 由服务端算：本页字段名并集、首见顺序（D3）。
- `more`：本页条数 == `count` 即 true（"可能还有"，不是精确计数——精确计数要多一条 XLEN 比较，不值）。
- `length` / `firstId` / `lastId` 来自 `XLEN` + `XINFO STREAM`（`first-entry` / `last-entry`；空 stream 两者为
  null）。每次窗口都带——追新时面板用 `length` 的差值显示速率。
- `count` 默认 100、上限 1000、`<= 0` 或非数字 → 400；`before` 与 `after` 同时给 → 400；键不是 stream →
  400 `"stream:x is a hash, not a stream"`；键不存在 → `type: "none"`（与 `/key` 一致）。

**每 tick 的命令数**：`XREVRANGE` + `XLEN` + `XINFO STREAM` = 3 条，`pipeline` 一次往返。这是 §2.5 里
`follow_polling_costs_three_commands_per_tick` 用 `INFO commandstats` 钉住的数字。

**`read_key` 的委托**：`RedisBrowser::read_key` 遇到 `TYPE == stream` 时改为返回 `read_stream(key, {})`
（最新 100 条）——面板打开键时不知道类型，第一次请求仍走 `/key`，拿到 `type: "stream"` 就切到 stream 视图。
`type_aware_read` 的 stream 分支保留给 MCP 工具（D8）。

**纯函数**（都在 `redis.rs`，各自单测）：

| 函数 | 做什么 | 单测覆盖 |
| --- | --- | --- |
| `stream_id_ms(id: &str) -> Option<i64>` | `"<ms>-<seq>"` → ms | 正常、`0-0`、seq 很大、`ms` 溢出 i64、没有 `-`、空串、`$`/`+`/`-` 特殊值 → None |
| `stream_id_to_ts(id) -> Option<String>` | ms → ISO 8601 毫秒 | 已知毫秒对应的固定字符串；None 透传 |
| `stream_window_args(key, bound: Bound, count) -> Vec<String>` | 三种边界拼 XREVRANGE 参数 | `Newest` → `+ -`；`Before(id)` → `(id -`；`After(id)` → `+ (id`；count 钉在参数里 |
| `stream_columns(entries) -> Vec<String>` | 并集、首见顺序 | 空、单条、交错字段、重复字段名不重复出现 |
| `stream_entries(reply)`（已有） | RESP → `{ id, fields }` | 正常、奇数长度字段数组（丢尾）、非数组回复 → 空、字段值含非 UTF-8（走 `value_string`） |
| `parse_xinfo_groups(reply) -> Vec<Group>` | RESP2 平铺 map → 结构 | 有 `lag`（7.x）、无 `lag`（6.2）→ null、`pending` 缺失 → 0、空数组 |
| `clamp_count(v: Option<&Value>) -> Result<i64, String>` | 默认/上限/非法 | None→100、1000→1000、1001→1000、0→Err、`"abc"`→Err、负数→Err |

每个纯函数上方一段注释写**为什么**（D7）——例如 `stream_window_args` 上写"为什么两个方向都是 XREVRANGE"，
`clamp_count` 上写"为什么上限是 1000（与 `type_aware_read` 的既有上限一致，一页 1000 条 × 20 字段约 200 KB
JSON，是浏览器一次 fetch 不卡的经验线）"。

**路由**（`crates/swiss-data/src/dbbrowser_api.rs`，与 `/key` 同一批 `lease_redis`）：

```
GET /api/db/{name}/stream?key=K&count=N                  最新窗口
GET /api/db/{name}/stream?key=K&before=ID&count=N        更早一页
GET /api/db/{name}/stream?key=K&after=ID&count=N         追新
GET /api/db/{name}/stream/groups?key=K                   消费组
```

审计：与 `/key` 同级（读操作不进 `data view redis commit` 那条日志）。

### 2.2 面板：表格、最新在顶、"加载更早"

新文件 `panel/src/data-stream.ts`（渲染 + 纯函数），从 `data-browsers.ts:325` 的兜底分支分流：
`if (v.type === "stream") { dbRenderStream(wrap, v); return; }`，兜底分支只剩 module 类型。

- **头部 meta**（沿用 `db-detail-meta`）：`key · stream · 10,000 entries · first 2023-11-14 22:13:20 · last …`，
  右侧：Follow 开关（`btn`，按下态）、间隔选择（1 s / 2 s / 5 s，D2）、"消费组"折叠开关、既有的
  rename/delete 菜单（`data-rkeymenu`，不变）。
- **表格**（`db-grid`）：列 = `id` | `time` | `columns…`。`time` 用 `ts` 按本地时区显示到毫秒；`id` 单元格
  等宽字体。单元格文本经 `dbRedisDisplayText`（docs/43 M2 的解码规则，`data-browsers.ts:432`）；右键沿用
  `dbRedisCellMenu`（复制值 / 整行 JSON）。**只读**：没有编辑态、没有 add 按钮（D4）。
- **底部**："加载更早"按钮（`more === true` 时显示），点击 `GET /stream?before=<最底行 id>&count=100`，
  追加到底部；追加后若总行数 > 500，**从顶部丢**（往老翻页时用户在看底部，丢顶部才不打断）。到头
  （`more === false`）显示"已到最早"。
- **空 stream**：`length === 0` → `emptyNode`（docs/18 V7 的共享空态）：标题"stream 为空"，提示 Follow
  仍可开。
- **列的变化**：翻页或追新带来新字段时，列并集扩大（D3），表头重画一次；已有单元格不动。

状态放 `db-state.ts` 的 tab 状态里（每个 tab 一个 stream 缓冲：`rows`、`columns`、`follow`、`intervalMs`、
`pending`（未合并的新条目数）），切 tab 回来不用重拉。

**为什么是表格不是 JSON**（写进 `data-stream.ts` 顶部注释）：行情条目的字段集稳定，表格一眼能对列；
JSON 墙的问题不是大小而是没有列。**为什么不虚拟滚动**：D1 把 DOM 钉在 500 行，500 行 × 20 列 = 1 万个单元格
在 2026 年的浏览器里是毫秒级重绘，虚拟滚动带来的复杂度（行高测量、滚动锚定）在这个规模没有回报。

### 2.3 Follow：环形缓冲 + 顶部钉住

- 开关打开：`setInterval`（`intervalMs`）每 tick `GET /stream?after=<顶行 id>&count=100`。追新的 `count`
  **固定 100**（D1 的窗口），一 tick 超过 100 条就 `more: true`，面板在顶部显示一条 "有更多，已跳过中间部分 ·
  跳到最新" 横条（点击 = 重开最新窗口）。**为什么固定 100**：追新的目的是"看见现在"，不是"补齐历史"；
  行情每秒几百条时补齐等于永远追不上。
- 收到新条目：`streamMerge(rows, incoming, 500)`（纯函数，vitest）——按 id 去重、最新在前、超过 500 从**底部**丢。
- **顶部钉住**：`isTopPinned(scrollTop)`（纯函数：`scrollTop <= 一行高度`），钉住时新行直接插到顶、滚动位置
  保持在顶；用户往下滚了就**不插**，只累计 `pending`，头部显示 "↑ N 条新" 药丸；点击药丸 = 合并 + 回顶。
  这是终端 `terminal-core.ts:289` 的 "pinned to bottom"（docs/22 §2.10）反过来用——理由一样：错误地钉住只是
  多滚一下，错误地不钉会把用户正在看的行拽走。
- **只在看得见的时候跑**：tab 不活跃、`document.hidden`、tab 关闭、连接 stop → `clearInterval`（同
  `data-activity.ts:181-186` 的自守规则）。回到 tab 时若 Follow 仍开着，先补一次追新再恢复定时器。
- **速率**：头部显示 `entries/s`——`streamRate(prevLength, prevAt, length, now)`（纯函数），来自两次 tick 的
  `length` 差值，不是从 id 时间戳算（XTRIM 会让 id 时间戳失真，`length` 差值不会）。
- **错误**：一次 tick 失败（连接断、键被删、类型变了）→ 停 Follow、头部显示原因、开关复位；不重试风暴。
- **内存**：Follow 开着 60 s、每秒 50 条推送后，DOM 行数 ≤ 500、JS 堆增量 < 10 MB（§2.5 真浏览器走查里
  用 CDP `Performance.getMetrics` 的 `JSHeapUsedSize` 采两点）。

### 2.4 消费组（只读）

头部"消费组"折叠开时，`GET /stream/groups?key=K`，表格：`group | consumers | pending | lag | last-delivered-id`。
Follow 开着时每第 5 个 tick 顺带刷新一次（消费组变化慢，不值每秒一次）。`lag` 为 null（6.2 服务器）显示
"—"。**为什么只读**（D4）：XACK / XCLAIM 改变的是别人的消费进度，是运维动作不是浏览动作，误点的代价
（消息丢失）与本页"看"的定位不对等；要做也是另一张有确认对话的页。

### 2.5 测试矩阵（D7：每个功能点四层都有）

| 功能点 | 单测（Rust `#[test]` / vitest 纯函数） | 集成（swiss-it gate 2，真 redis 7） | 面板 DOM（vitest jsdom） | 真浏览器走查 |
| --- | --- | --- | --- | --- |
| 最新窗口 | `stream_window_args` Newest；`clamp_count` | `read_key_on_a_stream_returns_the_newest_window_first`：100 条、id 严格递减、首条 == `lastId`、`columns == [sym,px,qty,side]`、`more`、`length == 10000` | 打开 stream 键渲染表格，首行 id == `lastId`，列头顺序 | 19998 打开 `stream:ticks`，看首行、列 |
| 往老翻页 | `stream_window_args` Before | `read_stream_pages_older_by_cursor_without_loss_or_duplication`：100 页走完 10,000 条，不重不漏，最后一页 `more == false` | 点"加载更早"追加、>500 从顶丢、到头显示"已到最早" | 连点到头 |
| 追新 | `stream_window_args` After | `read_stream_after_returns_only_newer_entries_newest_first`：XADD 3 → 恰好 3，递减；`after == lastId` → 0 | `streamMerge` 去重/上限/顺序（vitest 纯函数 6 例） | 生成器 50 条/s，看顶部滚动 |
| 追新积压 | — | `read_stream_after_caps_and_flags_more`：XADD 1,500 → 100 条 + `more` | 横条出现、点击重开最新 | 生成器 500 条/s 一次 |
| 顶部钉住 | `isTopPinned` 4 例 | — | 滚下去后新条目进 `pending`、药丸计数、点击合并回顶 | 真滚轮 |
| 只在可见时跑 | — | `follow_polling_costs_three_commands_per_tick`：`INFO commandstats` 在邻居索引上前后对比，10 tick → `xrevrange +10 / xlen +10 / xinfo +10`（第 5、10 tick 各多一次 GROUPS） | 切 tab / `document.hidden` 后定时器为 null；回来补一次 | 切到别的 tab 30 s，`docker`/`CLIENT LIST` 看无请求 |
| 速率 | `streamRate` 4 例（首次、零差、负差（XTRIM）→ 0、正常） | — | 头部数字 | 与生成器速率对得上 |
| `ts` | `stream_id_ms` 7 例、`stream_id_to_ts` 2 例 | `read_stream_ts_derives_from_the_id`：seed 的固定 id → 固定 ISO | `time` 列本地化格式 | — |
| 列并集 | `stream_columns` 4 例 | `read_stream_ragged_fields_union_columns_in_first_seen_order`（`stream:ragged`） | 新字段到达表头重画、旧单元格不动 | — |
| 空 / 单条 | — | `read_stream_empty_and_single_entry_streams`（`stream:empty`、`stream:one`） | 空态渲染 | 打开 `stream:empty` |
| 错误 | `clamp_count` 非法；before+after → Err | `read_stream_refuses_non_stream_keys_and_both_cursors`：hash 键 → 含 "is a hash"；不存在 → `none`；两个游标 → Err | tick 失败 → Follow 复位 + 原因 | 删掉键再看 |
| 消费组 | `parse_xinfo_groups` 4 例 | `stream_groups_reports_pending_and_lag`：`feed` pending 7、consumers 1、lag == 10000-7 | 折叠开/关、"—" 显示 | 打开看数字 |
| 路由 | `dbbrowser_api.rs` StubRedis 加 `stream`/`groups` 字段：参数校验 5 例、审计不记 | L2：`gateway::stream_route_round_trips` 经 `/api/db/{name}/stream` 走真 redis（三种方向各一次） | — | — |
| 内存 | — | — | — | Follow 60 s @ 50/s：DOM ≤ 500 行、`JSHeapUsedSize` 增量 < 10 MB（CDP 两点采样） |
| 中文 | — | — | 新文案全部经 `tr()`，i18n 守卫过 | 第二遍走查切 `zh-CN`（docs/38） |

每组提交前做 §2.6 的 mutation。

### 2.6 mutation 检查（docs/44 §2.9 的规矩）

每项至少一个：S1 把 `stream_window_args` 的 `(` 前缀去掉 → 翻页测试"重复一条"红；S2 把 `streamMerge` 的
去重去掉 → vitest 红；S3 把 `isTopPinned` 写死 true → "滚下去不插"的 DOM 测试红；S4 把 `parse_xinfo_groups`
的 `pending` 读错字段 → 集成测试红。红的测试名进提交说明，revert 后再绿。

### 2.7 seed（swiss-it，补 docs/44 §2.4 漏掉的一行）

`crates/swiss-it/src/seed.rs` 的 redis 加载在 `keys.txt` 之外**生成**四个 stream（1 万行 XADD 不进文本
文件；生成器与 mysql 的递归 CTE / pg 的 `generate_series` 同理）：

| 键 | 内容 | 给谁用 |
| --- | --- | --- |
| `stream:ticks` | 10,000 条，id 显式 `1700000000000 + i*100`-`0`（时间戳确定，`ts` 测试才能钉死），字段 `sym`（5 个循环：`AAA`…`EEE`）、`px`（`(i % 1000) / 100` 两位小数）、`qty`（`1 + i % 50`）、`side`（`b`/`s`）；消费组 `feed` 从 `0` 建，`XREADGROUP GROUP feed c1 COUNT 7` 读走 7 条不 ACK → pending 7 | 窗口、翻页、追新、消费组 |
| `stream:empty` | `XGROUP CREATE … $ MKSTREAM` 建出的 0 条 stream | 空态 |
| `stream:one` | 1 条 | 单条边界 |
| `stream:ragged` | 5 条，字段集交错（`a` / `a,b` / `b,c` / `c` / `a,c,d`） | 列并集 |

`SEED_VERSION` 升到 3；`seed.rs`/`redis.rs`/`seed` 守卫里三处 `3016` 收成一个常量 `REDIS_SEED_KEYS = 3020`。
守卫测试加：`XLEN stream:ticks == 10000`、`XINFO GROUPS` pending 7、四个键 `TYPE == stream`。
docs/44 §2.4 的 redis 行补 "stream ×4（1 万条 + 消费组）"。

## 3. 工作项（每项一个 commit，顺序即依赖）

| # | 内容 | 验收 |
| --- | --- | --- |
| S0 | seed 四个 stream + 消费组 + `SEED_VERSION 3` + 守卫；docs/44 §2.4 补行 | 守卫绿；gate 2 其余 66 条不变（`3020` 替换处全对） |
| S1 | 服务端：7 个纯函数 + 单测；`RedisBrowser::read_stream` / `stream_groups`（默认实现）；redis browser 实现（pipeline 三命令）；`read_key` 对 stream 的委托；两条路由 + StubRedis 单测；swiss-it L1 9 条 + L2 1 条；每个纯函数与两个方法上的"为什么"注释 | §2.5 前 12 行的"单测 + 集成"两列全绿；mutation 记录 |
| S2 | 面板窗口视图：`data-stream.ts`（渲染、`streamMerge`、`streamColumns`、`isTopPinned`、`streamRate` 纯函数）+ vitest（纯函数 + DOM）+ 类型 `ApiDbStreamWindow` + 分流 + 空态 + "加载更早" | vitest 绿；真浏览器走查前四行；`npm run check` 绿；文件头注释写清表格/不虚拟滚动的理由 |
| S3 | Follow + 环形缓冲 + 顶部钉住 + 速率 + 只在可见时跑 + 错误复位 + 消费组面板 | §2.5 其余各行；内存走查两点采样数字进提交说明；中文第二遍 |
| S4 | 记录：docs/22 W3.3 那句改为指向本文；README 表行 → Shipped；docs/44 §2.4；本文状态头逐项填 commit；不写 ADR（§6） | 文档与代码一致 |

门禁（每次提交前）：

```
cargo test --workspace
cargo test -p swiss-it --features it
cargo clippy --workspace --all-targets --features it -- -D warnings
cargo tree -d -e normal,build
cargo deny check
cd crates/swiss-panel/panel && npm run check          # S2 起
```

S2/S3 另加 `.agents/rules/panel-proof-of-life.md` 的真浏览器走查（真点击、fresh load、zh-CN 第二遍），
`touch crates/swiss-panel/src/lib.rs` 后再编 19998。

## 4. 验收（整体）

1. 打开一个 100 万条的 stream，首屏 < 1 s，显示最新 100 条，`more`；往老翻 10 页不重不漏。
2. Follow 开着、每秒 50 条推 60 s：DOM ≤ 500 行、堆增量 < 10 MB、顶部持续滚动；滚下去后不被拽走、药丸计数正确。
3. tab 切走 30 s 期间 redis 侧零请求（`CLIENT LIST` / commandstats）。
4. 每 tick 恰好 3 条命令（第 5 tick 4 条）——集成测试钉住。
5. 消费组表显示 pending / lag，与 `XINFO GROUPS` 手敲一致。
6. `cargo test --workspace` 在无 Docker 的 shell 仍绿；gate 2 新增 ≥ 11 条全绿；vitest 新增 ≥ 20 条全绿。
7. `redis_read` 工具对 stream 的行为与基线逐字相同（`tests/` 既有用例不动即证）。
8. 新代码里每个纯函数、两个 trait 方法、`data-stream.ts` 文件头都有"为什么这么设计"的注释（review 时逐个点名）。

## 5. 不做什么

- `XREAD` / `XREADGROUP` / 服务端推送（SSE / WebSocket）：共享连接不能阻塞（docs/22），网关不持有订阅（宪法）。
- XACK / XCLAIM / XTRIM / XDEL / XGROUP 的 UI（D4）；`redis_command` 控制台照旧能敲，走既有的 destructive 门。
- 虚拟滚动、图表、字段级过滤 / 搜索、导出（先看得见，再谈筛）。
- `redis_read` 工具的 stream 方向（D8）——如果 agent 侧也要最新优先，另开一条 spec 改工具合同。
- Redis < 6.2（排他区间语法）；哨兵 / 集群（既有连接模型之外）。

## 6. ADR

不写。三条件（难以回退 / 无上下文会惊讶 / 真实取舍）里只有第三条勉强成立（轮询 vs 订阅），而它是 docs/22
"一条共享连接、禁阻塞命令"既有决定的直接推论，不是新取舍；路由与面板文件都是可删的增量。docs/07 不动。
