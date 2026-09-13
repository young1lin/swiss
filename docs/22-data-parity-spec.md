# 22 — Data 视图全量对齐:六个批次(W0–W5)

> **状态:已交付(待 master 合并)— 全部六批 32 项落齐,集成分支 `data-parity` @ `6aa6761`(含全部外来面板对齐),每批独立审计 PASS。批账:W0 `6685d93..477aab3`、W1 `f072bc5..ed1dfae`、W2 `9206053..d8a3430`、W3 `6ed4c08..ed995b2`、W4.4/4.5 `48cbf4d..b731adc`、W4.1/4.2 `f9001b9..e8c09f3`、W4.6 `b8fcf8d..8d6a75c`、W4.3+minors `68b141c..a1c5e39`、W5 `a88d360..88fff4d`;Node 面板侧对应提交散于其 main(23a5de8 起)。已知不拦 minor:W4d hex 角落反例、超大 JSON 无截断、W4.6 comment 上限已修仅 PG 回滚无自动化测试(记 live 清单)。Hand-off prompt:[docs/22-data-parity-prompt.md](docs/22-data-parity-prompt.md)。**
> 基线:`3d5f42e`(docs/21 落库,master,2026-09-13)。
>
> 需求原话(2026-09-13):"/swiss-spec 我需要所有的功能,开始写 spec 吧,注意,需要和 /swiss-design 保持风格一致,然后 EnterWorktree 单独开个 worktree 进去,开始写这部分内容。有一点我需要你注意,vendor 被忽略了,worktree 里面可能引用的 js 没了,我的建议是把 clone 的项目,挪到其他项目下……"(参照库问题同日由用户归位:`../terminals-ref/`。)
>
> 参照库:`../terminals-ref/`(仓库外共享目录,`--depth 1` 只读克隆)。本文所有 `<repo>/path:N` 引用相对 `~\dev\terminals-ref\`,五个 repo = adminer / dbgate / cloudbeaver / pgadmin4 / pgweb。逐条差距依据见 [docs/21](21-data-web-gap-analysis.md)。

## 0 现状与差距(为什么做)

docs/21 用五个成熟参照逐条核对了 swiss Data 模块,35 条差距定级。用户点名"所有的功能":本 spec 把其中 #1–#30 组织成六批 W0–W5(原"远期"两项转正进 W5),#31–#35 维持不做(§9)。

四性质自查(AGENTS.md):

- **Ruthlessly small** — 全程仅两处非常驻零:W3.1 的惰性列名缓存(上限 64 KB,DDL 后失效)与 W4.4 把导出内存从 O(全表) 降到 O(chunk)(反向收益)。无新线程、无新子进程、无新 crate 依赖。
- **Plugin-shaped** — 改动只落 swiss-data 路由、swiss-host dbbrowser 契约、swiss-mcp 各 `*_browser` 适配器与面板 data-* 模块;host 只在契约扩展处动。
- **Hot-pluggable** — 无新生命周期;新路由沿用 CatalogRegistry 请求级 lease(禁用 MCP 即 503 的既有语义不变)。
- **三种接入** — 不新增 stdio/http 工具,全部编译期内建。

## 1 通用约束(每个 W 项适用,不逐条重复)

1. **面板源在 Node 仓**:`../local-mcp-gateway/src/admin/` 先改、整树拷回 `crates/swiss-panel/src/admin_assets/`;`the_tree_is_byte_for_byte_the_node_builds` 必须过。禁止直接改 admin_assets。
2. **测试**:后端行为变化带 Rust 测试(失败于前、通过于后,集成测试走 `tower::ServiceExt::oneshot`);面板纯函数沿用导出习惯(先例:`dbNextSort` 的 "Pure so the cycle can be tested without a DOM")并在 Node 仓补用例;交互项验收 = 19998 实测清单(swiss-live-verify / agent-browser,亮暗双主题 + 1440/900 两宽度)。
3. **门禁**(每项提交前):`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo tree -d`。
4. **提交粒度**:一个 W 项一个 commit,标题带 `(docs/22 Wx.y)`;批次内顺序即依赖顺序(§11)。
5. **设计语言**:swiss-design skill 是宪法;面板词汇只用其词表 + 本文 §2 登记的新词;每个面板 commit 过该 skill 的拷回前检查清单(无新字面量颜色/尺寸、双主题截图、无 Unicode 图标、行内最多一个非图标按钮、危险项红字居 popup menu 末位)。
6. **默认启用**:不引入配置开关,所有行为直接生效(用户既有规则)。
7. **红线**:不碰 19999;不加依赖;`serde_json::Value` 不上转发路径;失败必须带回滚语义。

## 2 词汇登记(唯一新词;其余复用现有词表)

| 词 | 是什么 | 类名 |
| --- | --- | --- |
| **suggest list** | SQL 控制台补全下拉:绝对定位在光标下方,≤8 项,↑↓ 移动、Tab/Enter 采纳、Esc 关闭;候选项 monospace(都是"你会复制的值") | `db-suggest` |

复用:多结果 = **segmented control**;值查看器 = **sheet**;活动监控 = db-grid 表 + section caption;schema 分组头 = docs/20 §4 的容器层级词汇(28px band、`--f-body` 600 组名、`--text-3` tnum 计数、子项缩进 20px、1px `--sep` 引导线)。

---

## W0 接线级 —— 后端已备,纯接线(合计 ≈2 天)

### W0.1 导出格式选择(CSV / NDJSON)
参照:dbgate `packages/web/src/plugins/fileformats.ts:3-80`。
改动:面板 Export 按钮改 popupMenu("Export CSV…" / "Export NDJSON…"),保留现有确认框;删除 `data-grid.js` 写死的 `var fmt = "csv"`(`data-grid.js:153` 附近)。后端零改动(`format=json` 已支持)。
验收:19998 选 NDJSON,落盘 `json-<table>` 文件、内容 NDJSON;`x-export-rows` 行为不变;字节测试过。

### W0.2 导出携带当前 filters
参照:pgadmin `tools/import_export/templates/import_export/sql/default/cmd.sql:1`(\\copy 把过滤烘进查询);cloudbeaver `exportDataFromResults.gql`(导出=当前结果集)。
改动:面板把行网格同款 filters JSON 附到 /export;后端 `export()` 解析 filters(复用 `parse_filters`)并传给适配器;适配器侧把 filter→WHERE 组装抽成 `browse_where()`,rows 与 export 共用同一函数(COUNT 同源)。行帽与 `x-export-capped` 不变。
验收:Rust 单测(带 filter 的导出 SQL 含 WHERE 且值走绑定参数);19998:过滤后导出仅含命中行,`x-export-rows` = 过滤总数。

### W0.3 Redis type 过滤
参照:dbgate `plugins/dbgate-plugin-redis? no — packages 侧 driver.js:56-71`。
改动:键列表 pattern 输入旁加 select(All types / string / hash / list / set / zset / stream),请求附 `&type=`。后端已支持(`redis_browser.rs:56,71`)。
验收:19998:选 hash 仅列 hash 键;不传 type 行为与今日逐字节一致(回归)。

### W0.4 查询耗时
参照:pgweb `result.go:51-58`(服务端计时进响应)。
改动:`run_query` / redis `run_command` 以 `Instant` 计时,响应加 `elapsedMs`(整数);面板结果 meta 行尾 ` · 12 ms`(`db-meta`)。
验收:Rust 伪浏览器断言字段存在且 ≥0;19998 控制台与 Redis 命令均显示耗时。

### W0.5 EXPLAIN ANALYZE
参照:pgadmin `tools/sqleditor/templates/sqlediter/sql/default/explain_plan.sql:2-20`。
改动:Explain 按钮改 popupMenu("Explain" / "Explain ANALYZE");`dbWithExplain(sql, mode)`。后端零改动:单语句守卫照常;PG 原生支持,MySQL 8.0.18+ 支持,旧版把驱动错误原样展示——"驱动消息即答案"是本模块既定哲学。
验收:19998 PG 两项分别出计划;不支持的服务器报错文本直显;结果标题仍走 "Execution plan"。

---

## W1 小件 —— 每项 ≤ 半天

### W1.1 PG schema 分组与过滤
参照:dbgate `packages/web/src/widgets/SchemaSelector.svelte:48-56`;pgweb `static/js/app.js:135-187`。
改动:后端 /tables 增可选 `schema` 参数(在该 schema 内 grep;不传 = 全部,行为不变;MySQL 恒单库不分组、面板不显示组头)。面板:表列表按 schema 分组渲染(§2 的组头词汇);grep 行上方加 schema select(All schemas + 各 schema 计数),切换即请求。
验收:Rust:`pg_browser` schema 参数进 WHERE 的单测;19998:多 schema 实例分组显示、按 schema 过滤;MySQL 视觉零变化。

### W1.2 IN / NOT IN / BETWEEN 算子
参照:adminer `adminer/drivers/mysql.inc.php:218`、`adminer/include/editing.inc.php:254`。
改动:swiss-host `BROWSE_FILTER_OPS` 增 `in`/`notIn`/`between`;值按逗号拆分、逐个绑定;BETWEEN 取 `lo,hi` 两值。面板 `DB_FILTER_OPS` 同步三项。
验收:Rust 两方言 SQL 构建单测(含转义与空值拒绝);19998:`in = 1,2,3` 命中三行、`between` 闭区间。

### W1.3 Redis 键操作(Rename / TTL / Delete)
参照:dbgate `apps? no — packages/web/src/tabs/RedisKeyDetailTab.svelte:142-196`;adminer `adminer/plugins/drivers/redis.php:379-446`(键=虚拟表)。
改动:键值面板头部加 `⋯`(popupMenu):"Rename…"、"Set TTL…"、分隔线、"Delete…"(红字末位)。Rename/TTL 走 sheet(单输入 + 一个主按钮);Delete 键入键名确认(沿用 `dbTypedConfirm` 词汇)。后端:DEL/RENAME/EXPIRE/PERSIST 若在 redis 命令守卫拒绝名单则移出(独立 commit,守卫单测同步改);不加新路由,复用 /command;操作后 `dbLoadKeys`/重读该键。
验收:守卫单测;19998:改名即刷列表、TTL 面板可见、删除需键名确认且键消失。

### W1.4 列值统计
参照:pgweb `static/js/app.js:980-1007`。
改动:表头右键菜单加 "Value distribution…"(`SELECT col, COUNT(*) … GROUP BY 1 ORDER BY 2 DESC LIMIT 50`)与 "Numeric stats…"(`COUNT/MIN/MAX/AVG`);列名经标识符白名单引号(严禁裸拼);结果走现有 /query 展示在控制台结果网格。
验收:19998:string 列出分布表、数值列出统计;含保留字的列名正确转义。

### W1.5 filter-by-value
参照:pgweb `static/js/app.js:1268-1277`;pgadmin `web/pgadmin/utils/filter_dialog.py:70`。
改动:单元格右键菜单加 "Filter = value" / "Filter ≠ value" / "Filter contains"(值非 NULL 时);推入 `d.filters` 并立即应用(NULL 单元格不显示这三项)。
验收:19998:右键即过滤,过滤器行同步出现该条件。

### W1.6 过滤语法(逗号 AND、`|` OR、`*` 通配)
参照:cloudbeaver `webapp/packages/core-navigation-tree? no — plugin-navigation-tree/src/elementsTreeNameFilter.ts:117-139`。
改动:15 行纯函数 `dbFilterMatches(tokens, name)`(AND/OR/通配,大小写不敏感)进面板 grep 框(占位提示改 `a*, b|c`);后端表列表 grep 同语义(拆多 LIKE 组合)。行过滤值里的 `*` 转义为 LIKE 通配。
验收:纯函数 Node 仓用例;19998:grep `user*|account` 命中两类表;单子串行为回归不变。

### W1.7 编辑回读落库值
参照:adminer `adminer/sql.inc.php:60-73`;pgadmin 插入后 select 回读。
改动:/edits 成功响应携带每个 update/insert 的落库行(同事务内 `SELECT … WHERE pk`);面板 Commit 后用回读值刷新受影响格——库的静默截断/规范化立即可见。
验收:Rust:mock 浏览器断言回读行;19998:varchar(3) 填超长,Commit 后格子显示库内截断值。

### W1.8 空行分块执行
参照:pgweb `static/js/app.js:839-875`。
改动:`dbRunSql` 前以纯函数 `dbSubqueryAt(text, caret)` 按空行切段、取光标所在段;仍单语句/次(段内多语句照旧被后端拒)。控制台 hint 更新一句。
验收:纯函数用例;19998:三段脚本光标在第二段 Ctrl+Enter 只跑第二段。

### W1.9 nextPage 探测(limit+1)
参照:pgadmin `web/pgadmin/tools/sqleditor/__init__.py:1330-1391`(fetch_window 思路)。
改动:read_table 取 limit+1 行、回传 `nextPage: bool`;本期不禁 COUNT(保守,COUNT 优化留待实测大表后再议)。面板仅用其修正翻页箭头 disabled 态。
验收:Rust:limit+1 与 nextPage 字段单测;19998:翻页行为回归不变。

### W1.10 右键生成 SQL 模板
参照:cloudbeaver `sqlGenerateResultSetQuery` + GenerateSQL Actions。
改动:表 `⋯` 菜单加 "Generate SELECT/INSERT/UPDATE/DELETE":用 describe_table 列集拼模板填入 SQL 框(列名白名单引号,值位用 `?` 占位 + 一行注释提示)。
验收:19998:生成 SELECT 含全部列;保留字列名正确转义;模板进历史可复跑(替换 ? 后)。

---

## W2 网格手感 —— 纯面板,一批拉平

### W2.1 列宽拖调 + 列隐藏
参照:dbgate `packages/datalib/src/GridConfig.ts:18-36` + `packages/web/src/utility/useGridConfig.ts:7-34`。
改动:per-connection 单对象存 localStorage(`mcp_gateway_db_grid_<conn>_<schema.table>`):`{widths:{},hidden:[]}`;表头右缘 4px 拖手柄调宽(inline width);表头 `⋯` 菜单 "Hide column",隐藏列后有恢复入口("Show all columns";全部隐藏时显示空态提示行)。
验收:19998:拖宽与隐藏刷新后仍在;恢复入口可达;900px 宽度表头不溢出。

### W2.2 键盘导航 + TSV 粘贴 + 行复制
参照:dbgate `packages/web/src/datagrid/DataGridCore.svelte:2358-2374`(键盘)、`:1988-2056`(粘贴)。
改动:网格容器加 0 尺寸 focus input 承接 keydown:方向键移动焦点格、Enter/F2 进编辑、Esc 退出、Home/End、打字即编辑;Ctrl+C = 选中行 CSV(等价右键);onpaste TSV 从起点格逐格写入缓冲(insert 进 values / update 进 changes,仍走 Commit)。焦点格样式 = 2px `--accent` 边(选择词汇第 15 条)。
验收:19998:纯键盘完成"改一格 → Commit";粘贴 3×4 TSV 成新行缓冲;Ctrl+C 粘进 Excel 列对齐。

### W2.3 类型感知单元格渲染(纯函数)
参照:dbgate `packages/tools/src/stringTools.ts:104-284`(单函数多 intent)。
改动:`data-cell.js` 新增纯函数 `dbCellView(value, colType)` → `{text, cls, title}`:JSON >100 字符 → `(JSON)` + title 全文;URL(`http` 开头)→ 可点链接(noopener);数字列 tnum;布尔显 TRUE/FALSE;二进制显 `binary, N bytes`(内容进值 sheet 看,W5.3)。NULL 样式沿用。
验收:纯函数用例;19998:JSON 列折叠、URL 可点、数字右对齐、布尔成词。

### W2.4 int64 精度守卫(先测后改)
参照:pgweb `pkg/api/result.go:76-114`。
改动:第一步只写测试核实三处路径(rows / query / export)中 i64/u64 超 2^53 经 serde_json → JS 是否丢精度;若确认:适配器层统一"超 2^53 的整数序列化为字符串"(NaN → null 一并处理),响应形状不变、仅值形态;CSV/NDJSON 导出本就是文本不受影响。若现状已安全,记录结论关闭该项。
验收:Rust:构造 9223372036854775807,API 返回字符串且面板原样显示(或测试证明无需改动)。

---

## W3 中件

### W3.1 服务端 SQL 补全(suggest list)【ADR-015 候选】
参照:cloudbeaver `webapp/packages/plugin-sql-editor/src/SqlEditorService.ts:128-148`(数据流);dbgate `packages/web/src/query/codeCompletion.ts:82-214`(触发)。反面教材:pgadmin `web/pgadmin/utils/sqlautocomplete/autocomplete.py`(全量缓存,不抄)。
改动:后端 `POST /api/db/{name}/completion`,体 `{sql, caret}` → `{items:[{label,kind,detail}]}`。候选 = 方言关键字表(静态 ~150 词)+ 表名 + 当前表列名(以 sql 中 `FROM <table>` 就近匹配决定"当前表",匹配不到就只出关键字+表名)。缓存:per-connection 列名 `Vec<String>` 惰性建、TTL 10 分钟、DDL/结构查询后失效、总量上限 64 KB(超限退化仅关键字+表名)。内存代价与取舍写 ADR-015(§10)。
面板:textarea input 防抖 150ms;取光标前 token(`[A-Za-z0-9_.$]+$`)作前缀;复用高亮 overlay 的坐标计算定位 `db-suggest`;↑↓/Tab/Enter/Esc;Redis 连接不启用。
验收:Rust:伪浏览器补全端点(FROM 上下文出列名)、缓存失效单测;19998:SELECT 后输入列前缀出候选、Tab 采纳、Esc 关闭、Ctrl+Enter 仍运行。

### W3.2 活动监控 + Cancel / Kill
参照:adminer `adminer/processlist.inc.php`(64 行结构);pgadmin `web/pgadmin/tools/dashboard/__init__.py:637-688`;pgweb `static/js/app.js:398-405`。
改动:`GET /api/db/{name}/activity` → `{rows}`(PG:`pg_stat_activity` + `pg_blocking_pids`;MySQL:`information_schema.processlist`);`POST /api/db/{name}/activity-kill` 体 `{pid, mode}`(PG:`pg_cancel_backend`/`pg_terminate_backend`;MySQL:`KILL QUERY <id>`/`KILL <id>`)。面板:头部 `⋯` 加 "Activity…" 打开 section 页(db-grid:pid、user、state、wait、时长、query 截断 + title 全文),每行 `⋯`:Cancel / Terminate(红字末位);本连接行加 chip 标注;页面打开期间 5s 轮询,关闭即停。Redis 无此页(能力位隐藏)。
验收:Rust:两方言 SQL 构建单测;19998 PG:对 `pg_sleep` 会话 Cancel 后状态变化;MySQL:KILL QUERY 后行消失;19999 全程未动。

### W3.3 Redis 结构化编辑
参照:dbgate `packages/datalib/src/ChangeSetRedis.ts:67-202` + `packages/web/src/tabs/RedisKeyDetailTab.svelte:372-413`。
改动:值视图升级为类型化表格(hash:field/value;zset:member/score;list:index/value;set:member),缓冲编辑(插入/改值/删除)→ 命令列表预览(HSET/HDEL/ZADD/ZREM/LSET/RPUSH/SADD/SREM;复用待提交 SQL 预览的展示词汇)→ Commit 走新增 `POST /api/db/{name}/redis-pipeline`(数组命令一次往返,逐条仍过守卫);string 直接编辑(SET);TTL 行内改(EXPIRE)。hash per-field TTL(HEXPIRE,Redis 7.4+)明确不做。
验收:Rust:pipeline 端点守卫单测(拒绝名单照拦);19998:hash 加字段→预览 HSET→Commit 重读生效;zset 改分;string 改值;预览与实际执行逐字一致。

---

## W4 大件

### W4.1 无 PK 表可编辑(unique_idf)
参照:adminer `adminer/select.inc.php:440-472` + `adminer/include/functions.inc.php:326-339,414-418`。
改动:`build_edit_statements` 支持无 PK:寻址列 = 全列(要求值全非 NULL 的行集);>64 字节文本/二进制列改 `MD5(col)=?`(MySQL/PG 同式)寻址;MySQL UPDATE 尾随 `LIMIT 1`,PG 若 affected>1 视为行不唯一 → 该批回滚并报错。面板 editable 放宽,editNote 改述"rows addressed by all columns; ambiguous rows are refused"。
验收:Rust:无 PK 表 update/delete(唯一命中成功、双胞胎行拒绝);19998:测试表改一格 Commit 成功;造重复行后编辑被拒且提示可读。

### W4.2 乐观并发(source + diff)
参照:cloudbeaver `webapp/packages/plugin-data-viewer/src/DatabaseDataModel/Actions/ResultSet/ResultSetEditAction.ts:112-127`。
改动:面板 update 缓冲已带 `meta.orig`;Commit 载荷 update 项加 `source`(原行完整值),后端 WHERE = PK AND 各变化列原值比较;affected=0 → 409 消息指明冲突列,整批回滚,面板标红该格、缓冲保留可改后重提。无 PK 表与 W4.1 全列寻址天然合并。
验收:Rust:两连接并发改同一行,后提交方 409;19998:双浏览器窗口复现冲突并成功重提。

### W4.3 多结果 tab(segmented control)
参照:dbgate `packages/web/src/query/ResultTabs.svelte:49-132`;cloudbeaver resultTabs。
改动:**后端不动**(单语句契约保留),面板把整框文本按 `;` 切分逐条发送;每结果一个 tab(segmented control,≤8 个、超出横向滚动),tab 名 = 语句首词 + 行数(`SELECT · 42`);每 tab 独立选择键空间;EXPLAIN 结果照旧形态;历史记整框原文。
验收:19998:`SELECT 1; SELECT 2` 出两 tab,各自选择/复制/导出;单语句路径回归不变。

### W4.4 流式 SQL dump 导出格式
参照:adminer `adminer/adminer.inc.php:980-1078`(1MB 攒批多值 INSERT)。
改动:export_table 增 `format=sql`:头部 `CREATE TABLE`(复用 describe_table 的 DDL)+ 会话级关闭外键检查(MySQL `SET FOREIGN_KEY_CHECKS=0`;PG 以注释说明顺序),正文逐行 fetch + 攒批多值 INSERT(满 1 MB flush),axum `Body::from_stream` 分块产出;`EXPORT_ROW_CAP` 不变,`x-export-format` 头;面板 W0.1 菜单加第三项 "Export SQL dump…"。顺带复核现有 csv/json 路径确无整表物化。(实测更正:csv 路径仍为 O(table) 整表物化,属存量行为,本轮未改动,以 docs/01 的 2026-09-13 对照读数留档;json 路径已分页。)
验收:Rust:伪连接断言多次 flush(峰值内存 < 2×chunk);19998:导出 .sql 可重放进临时表;`x-export-rows` 正确;完成后按 swiss-memory-record 记录一次内存读数。

### W4.5 CSV 导入 upsert
参照:adminer `adminer/include/editing.inc.php:212-246` + `adminer/drivers/driver.inc.php:206-223`。
改动:import 增 `mode: insert(默认)| upsert`;MySQL `ON DUPLICATE KEY UPDATE`(映射列);PG `ON CONFLICT (pk) DO UPDATE`(无 pk 时报错并回落 insert,提示写明)。面板导入向导头部加 segmented control(Insert / Upsert)+ 一句说明。
验收:Rust:两方言 upsert SQL 单测;19998:同文件导两次,upsert 模式行数不翻倍、默认模式不变。

### W4.6 DDL 最小集(表单 → 单函数 → SQL,预览 = 保存)
参照:pgadmin `web/pgadmin/utils/__init__.py:1517-1549`(msql 数据流:预览与保存同函数)+ `:1562-1571`(集合 diff 三桶分类);adminer `adminer/create.inc.php:55-148`。
改动:只做三件事——CREATE TABLE、ADD COLUMN、CREATE INDEX。后端 `POST /api/db/{name}/ddl-preview` 体 `(op, payload)` → `{sql}`(纯函数 `table_ddl()/column_ddl()/index_ddl()`,两方言各一套,标识符一律白名单引号);执行复用现有 /ddl(op 扩展)。面板:表列表头部 `+` → "New table…" sheet(列迷你网格:name/type/nullable/default/comment;type 为文本输入 + 常见类型 datalist);SQL 预览区实时调 ddl-preview(展示词汇同待提交 SQL 预览);Commit = 预览文本原样执行。结构页 Columns/Indexes 各加 `+`(同表单,预填旧态走 diff)。**明确不做**:改列类型、约束管理、视图/触发器/存储过程(§9)。
验收:Rust:三函数 × 两方言快照测试 + 非法标识符拒绝;19998:建表 → 加列 → 建索引 → 数据页可见;断言预览与执行 SQL 同源逐字相同。

---

## W5 远期转正(用户点名"所有功能")

### W5.1 单记录表单视图
参照:dbgate `packages/web/src/formview/SqlFormView.svelte`;cloudbeaver singleEntity。
改动:数据页 segmented control 增 "Form":当前行竖排两列(列名/值),`‹ ›` 切换相邻行;可编辑(同缓冲语义);编辑控件复用 W2 词汇(NULL 按钮/布尔切换/长值开 sheet)。
验收:19998:切换、编辑、Commit 与网格侧一致;缓冲计数两视图同步。

### W5.2 FK 跳转
参照:adminer `adminer/select.inc.php:491-509`;dbgate `packages/web/src/formview/openReferenceForm.ts`。
改动:describe_table 已回 FK;FK 列头加小箭头按钮(`aria-label` "Jump to referenced row"),点击 = 打开目标表并预置 filter = 当前值(复用 W1.5 通道)。结构页 FK tab 的目标表名变可点(只读导航)。
验收:19998:点外键格跳目标表且行已过滤到位;反向引用链接可达。

### W5.3 值查看器 sheet
参照:cloudbeaver `webapp/packages/plugin-data-viewer/src/ValuePanelPresentation/TextValue/TextValuePresentationBootstrap.ts:18-45,53-81`(MIME 注册表)。
改动:单元格右键加 "View value…" 打开 sheet:JSON(尝试 parse → `<details>` 折叠树)、文本(pre 全文)、URL(外链打开)、hex(二进制;适配器把 bytes 转 hex 前 512 B + 截断标志,复用现有截断词汇)。只读为主,编辑仍走现有编辑 sheet。
验收:19998:四类值各开一次,双主题截图;hex 截断标志可见。

### W5.4 收藏查询 + 轻量格式化
参照:cloudbeaver 脚本资源化(只取"可保存"这一步,不做服务端存储)。
改动:History 下拉旁 `★`(`aria-label` "Save to favorites");localStorage `mcp_gateway_db_favorites`(上限 50,名 = 首行截断);下拉分组 History / Favorites。"Format" 按钮:纯函数 `dbFormatSql` 在现有词法上做缩进(子句换行 + 两空格缩进;不动大小写、不解析语义)。
验收:纯函数用例(格式化仅改变空白);19998:保存/召回/格式化往返,运行结果与格式化前一致。

---

## 9 明确不做(docs/21 #31–#35)

- **可视化表 designer 全家桶**(改列类型/约束/视图/触发器):CloudBeaver CE 也没有;msql 数据流(W4.6)已覆盖安全子集。
- **pgcli 式全量元数据缓存补全 / Ace / CodeMirror**:内存与"无构建"红线。
- **虚拟滚动大网格**:500 行/页 + 服务端 LIMIT/OFFSET 是既定契约,无此痛点。
- **fork 子进程连接 / pg_dump 子进程 / pickle 会话**:no-subprocess 与内存红线。
- **ERD / schema diff / 跨连接数据传输框架**:与"本机瑞士军刀"定位冲突。

## 10 ADR 候选(实施落库时写入 docs/07)

- **ADR-015 补全架构**:服务端 (全文,光标) → 候选;惰性列名缓存 ≤64 KB、TTL 10 min、DDL 后失效;拒绝客户端全量 dbinfo(pgadmin 反例,O(对象数) 内存)。难逆转(API 形状)+ 有真实取舍 + 无上下文会惊讶——三条全中。
- (视实施争议)**ADR-016 DDL 只做最小集**:msql 数据流 + 三操作;若实施中"改列类型"压力显著再记。

## 11 交付顺序与依赖

W0(W0.2 建议最先——它抽出 `browse_where()` 共用函数,W1.2 直接复用)→ W1(W1.1 与 W1.6 同触表列表,实现可同手、提交分开;任意序)→ W2(独立,可与 W1 并行)→ W3(W3.3 依赖 W1.3 的守卫加白)→ W4(W4.2 依赖 W4.1 寻址统一;W4.3/W4.4/W4.5 独立;W4.6 最后)→ W5(W5.2 依赖 W1.5,W5.3 依赖 W2.3,W5.1 独立)。每完成一批:swiss-review;全部完成:README 状态更新 + ADR 落 docs/07。
