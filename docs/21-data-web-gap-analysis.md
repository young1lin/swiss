# 21 — Data 模块对标成熟 Web 数据库工具:差距分析与弥补路线

> 方法:zhipu Search 选型后,五个成熟项目 `--depth 1` 克隆到仓库外的 `../terminals-ref/`(不进 git),五个子代理分别只读探索并逐条核对我方代码后出报告:Adminer(单文件 PHP 全家桶)、DbGate(Svelte Web 跨库,含 Redis)、CloudBeaver CE(DBeaver 官方 Web 版,天花板参照)、pgAdmin4(PG 结构编辑最全)、pgweb(Go 极简只读,对照组)。本文是五份报告的合并结论;每条差距都标注最佳参照与代码位置,可直接按图索骥。

## 一、跨项目共识(信号最强的发现)

1. **可视化 DDL 编辑器不是标配**。CloudBeaver CE 的 DDL tab 只读(`plugin-ddl-viewer/.../DDLViewerTabPanel.tsx:57`,全仓无 EntityEditor),pgweb 根本没有。pgAdmin 有,但其可取部分是"状态→SQL"的数据流,不是对话框引擎。→ 要做也只做最小集,见批次 4。
2. **进程列表 + kill 是最无争议的刚需**:Adminer(`processlist.inc.php`,全 64 行)、pgAdmin(`dashboard/__init__.py:637-688`)、pgweb(`api.go:497`)三家都有。实现极廉价。
3. **查询耗时 4/5 家都有**(Adminer `sql.inc.php:246`、dbgate `handleQueryStream.js:131`、pgweb `result.go:51-58`、pgAdmin 运行时显示),近乎免费。
4. **补全的两条路线**:pgAdmin/pgcli 按连接全量缓存表/列/函数(`sqlautocomplete/autocomplete.py`,O(对象数) 内存,反面教材)vs CloudBeaver 服务端补全(全文+光标→候选,前端零词法,`SqlEditorService.ts:128-148`)。→ 我们走后者:惰性 KB 级列名缓存或前缀 LIKE 查询。
5. **Redis 只有 DbGate 认真做了**:类型化缓冲编辑 → 命令预览 → 单 pipeline 提交(`ChangeSetRedis.ts:67-202`、`RedisKeyDetailTab.svelte:142-196,372-413`)。参照系就是它;Adminer 的"键列表=虚拟表"思路(`plugins/drivers/redis.php:379-446`)可作键级操作的补充参照。
6. **乐观并发没有工具做真版本号**;CloudBeaver 的 source+diff(原行值寻址+只发变化列,`ResultSetEditAction.ts:112-127`)是最佳实践,Adminer 的 unique_idf(无 PK 表无状态寻址,`select.inc.php:440-472`)补上无 PK 场景。
7. **我们的流式导出已经领先 pgweb**(它 `result.go:131-162` 全量 buffer);Adminer 的 1MB 攒批多值 INSERT(`adminer.inc.php:980-1078`)是 SQL dump 的模板——唯一"补功能还降内存"的项。
8. **网格手感是 dbgate 的领地**:GridConfig 单对象 localStorage 持久化(`datalib/src/GridConfig.ts:18-36`)、隐藏 input 键盘导航(`DataGridCore.svelte:2358-2374`)、单一纯函数多 intent 单元格渲染(`tools/src/stringTools.ts:284`)。
9. CloudBeaver 的过滤语法词汇(`elementsTreeNameFilter.ts:117-139`:逗号=AND、`|`=OR、`*`=通配,15 行纯函数)零成本可抄。

## 二、差距总表

定级:0=接线级(后端已备) / 1=小 / 2=中 / 3=大 / ✗=共识不做。"面板"指纯前端,"后端"指纯 Rust,"两端"都要动。

| # | 差距 | 定级 | 端 | 最佳参照 | 我方现状锚点 |
|---|---|---|---|---|---|
| 1 | 导出格式写死 CSV(后端已支持 NDJSON) | 0 | 面板 | dbgate `fileformats.ts:3-80` | `data-grid.js` `var fmt = "csv"` |
| 2 | 导出不携带当前 filters | 0 | 两端 | pgAdmin \\copy 烘进 query(`cmd.sql:1`);CB 导出=同一 resultsId | `dbbrowser_api.rs` export 无 filter 参数 |
| 3 | Redis type 过滤(后端已支持 TYPE) | 0 | 面板 | dbgate `driver.js:56-71` | `redis_browser.rs:56,71` 已备;`data-browsers.js:20` 未传 |
| 4 | 查询耗时显示 | 0 | 两端 | pgweb `result.go:51-58`(服务端计时进响应) | `data-sql.js` 无耗时 |
| 5 | EXPLAIN ANALYZE 选项 | 0 | 面板+后端放行 | pgAdmin `explain_plan.sql:2-20`(ANALYZE/VERBOSE/BUFFERS) | `data-sql.js:141` 只拼 EXPLAIN 前缀 |
| 6 | PG schema 侧栏不可见/不可过滤 | 1 | 两端 | dbgate `SchemaSelector.svelte:48-56`;pgweb objects.sql 折叠树 | `pg_browser.rs:122` 已返回 schema;面板不分组不显示 |
| 7 | IN / NOT IN(BETWEEN 顺带) | 1 | 两端 | Adminer 全驱动有(`mysql.inc.php:218`、`editing.inc.php:254`) | `data-filters.js:53-59` 算子表 |
| 8 | Redis 键操作按钮 DEL/RENAME/EXPIRE | 1 | 两端小 | dbgate `RedisKeyDetailTab.svelte:142-196`;Adminer 虚拟表 | 命令通道已有,拒绝名单需加白 |
| 9 | 列值统计(distinct/分布/min-max-avg) | 1 | 面板 | pgweb `app.js:980-1007`(右键拼聚合查询;列名须走白名单) | 无 |
| 10 | filter-by-value(右键单元格一键过滤) | 1 | 面板 | pgweb `app.js:1268-1277`(15 行);pgAdmin Filter Dialog | 无 |
| 11 | 过滤语法升级(AND/OR/通配) | 1 | 两端 | CB `elementsTreeNameFilter.ts:117-139` | grep 单子串 |
| 12 | 编辑后回读落库值(截断/规范化可见) | 1 | 两端 | Adminer `sql.inc.php:60-73`;pgAdmin select.sql 回读 | `edits` 只回 affected |
| 13 | 空行分块/光标语句执行 | 1 | 面板 | pgweb `app.js:839-875` | 单语句整框执行 |
| 14 | 列宽拖调 + 列隐藏(localStorage) | 2 | 面板 | dbgate GridConfig(`GridConfig.ts:18-36` + 表头 splitter) | 无 |
| 15 | 键盘导航 + TSV 粘贴 + 行复制 | 2 | 面板 | dbgate 隐藏 focus input(`DataGridCore.svelte:2358`;paste :1988-2056) | 无 |
| 16 | 类型感知单元格渲染(JSON 折叠/URL/千分位/NULL 样式) | 2 | 面板 | dbgate 单纯函数多 intent(`stringTools.ts:284`) | 仅 NULL 样式 + 编辑器内 JSON 格式化 |
| 17 | int64 精度守卫(超 JS 安全整数转 string) | 2 | 后端 | pgweb `result.go:76-114`(NaN→null、二进制 hex) | **未核对,优先验证我方是否丢精度** |
| 18 | SQL 自动补全 | 2 | 两端 | CB 服务端补全(`SqlEditorService.ts:128-148`);dbgate 客户端 dbinfo(`codeCompletion.ts:82-214`) | 无 |
| 19 | 活动监控 + Cancel/Kill | 2 | 两端 | pgAdmin 一页三查 + 两端点(`dashboard/__init__.py:637-688`);MySQL 对应 processlist + KILL QUERY | 无 |
| 20 | Redis 结构化编辑(hash/zset/list 表格化→命令预览→pipeline) | 3 | 两端 | dbgate `ChangeSetRedis.ts:67-202` | 值视图只读 |
| 21 | 无 PK 表可编辑(全列寻址 + LIMIT 1 + MD5 长值) | 3 | 两端 | Adminer unique_idf(`select.inc.php:440-472`) | 无 PK 即只读 |
| 22 | 乐观并发(source 原行 + diff 变化列,affected=0→409) | 3 | 两端 | CB `ResultSetEditAction.ts:112-127` | 仅 PK 寻址,静默覆盖 |
| 23 | 多结果 tab | 3 | 两端 | dbgate `ResultTabs.svelte`;CB resultTabs state | 一次运行覆盖上次 |
| 24 | nextPage 探测(limit+1,免强求 COUNT) | 1 | 后端 | pgAdmin fetch_window 思路(`sqleditor/__init__.py:1330`) | 每页 COUNT total |
| 25 | 流式 SQL dump 导出格式 | 3 | 后端 | Adminer 1MB 攒批(`adminer.inc.php:980-1078`)→ `Body::from_stream` | 10 万行封顶 CSV/NDJSON(已流式) |
| 26 | CSV 导入 upsert | 3 | 两端 | Adminer insertUpdate(`driver.inc.php:206-223`;MySQL ON DUPLICATE / PG ON CONFLICT) | 仅 INSERT |
| 27 | DDL 最小集(建表/加列/建索引,表单→SQL→预览) | 3 | 两端 | pgAdmin msql 双端点(`utils.py:1517-1549`:预览=保存同函数)+ 集合 diff 分类器(`utils.py:1562-1571`) | 仅 rename/truncate/drop |
| 28 | 右键生成 SQL(SELECT/INSERT/UPDATE/DELETE 模板) | 1 | 面板 | CB `sqlGenerateResultSetQuery` + GenerateSQL Actions | 无 |
| 29 | 单记录表单视图 | ✗/远期 | 面板 | dbgate formview;CB singleEntity | 无(宽表痛点,优先级让位于上表) |
| 30 | FK 跳转 / 值查看器(hex/图片) / 收藏 / 格式化器 | ✗/远期 | — | dbgate openReferenceForm;CB MIME 注册表 | 无 |
| 31 | 可视化表 designer 全家桶 | ✗ | — | (CB CE 也没有;pgAdmin 的可取处已并入 #27) | — |
| 32 | pgcli 式全量缓存补全 / Ace/CM6 编辑器 | ✗ | — | 内存红线 + 无构建约束 | — |
| 33 | 虚拟滚动大网格 / react-data-grid fork | ✗ | — | 500 行/页分页无此痛点 | — |
| 34 | fork 子进程连接 / pg_dump 子进程 / pickle 会话 | ✗ | — | 内存 + no-subprocess 规则 | — |
| 35 | ERD / schema diff / 数据传输处理器框架 / 跨连接拷表 | ✗ | — | 与"本机瑞士军刀"定位冲突 | — |

## 三、分批落地

每批独立可交付;面板改动一律先改 `../local-mcp-gateway/src/admin` 再整树拷回(字节级一致测试强制),行为变化带测试,19998 实测。

**批次 0 — 接线级(合计约 1~2 天)**:#1 导出格式下拉;#2 export 带 filters(后端把 filter→WHERE 抽成 rows/export 共用函数);#3 Redis type 下拉;#4 耗时(后端 Instant 计时进响应);#5 EXPLAIN ANALYZE 前缀选项。全部零新增常驻内存。

**批次 1 — 小件(每项半天内)**:#6 PG schema 分组+过滤;#7 IN/NOT IN/BETWEEN 算子;#8 Redis 键操作按钮(键名键入确认);#9 列值统计;#10 filter-by-value;#12 编辑回读;#13 空行分块执行;#24 nextPage;#28 生成 SQL 模板。

**批次 2 — 网格手感(纯面板,一次拉平)**:#14 列宽/隐藏;#15 键盘导航+TSV 粘贴;#16 类型感知渲染纯函数;#17 int64 精度核对(先测后改)。

**批次 3 — 中件**:#18 服务端补全((全文,光标)→候选;惰性列缓存 per-connection,DDL 后失效,或前缀 LIKE 无缓存);#19 活动监控+kill(PG 三查 + pg_cancel/terminate;MySQL processlist + KILL QUERY);#20 Redis 结构化编辑(照 ChangeSetRedis:缓冲→命令预览→单 pipeline)。

**批次 4 — 大件(按需)**:#21 无 PK 编辑;#22 乐观并发;#23 多结果 tab;#25 流式 SQL dump(内存反而降);#26 导入 upsert;#27 DDL 最小集(表单 JSON→Rust 单函数→SQL,预览=保存;只做 CREATE TABLE / ADD COLUMN / CREATE INDEX)。

## 四、参照项目索引

| 目录 | 定位 | 本分析主要贡献 |
|---|---|---|
| `../terminals-ref/adminer` | 单文件 PHP,20 年 | 无状态行寻址、结果集可编辑、流式 dump、进程列表、upsert |
| `../terminals-ref/dbgate` | Svelte Web 跨库(含 Redis) | Redis 结构化编辑、GridConfig、键盘导航/粘贴、单元格渲染纯函数 |
| `../terminals-ref/cloudbeaver` | DBeaver Web CE | 服务端补全、乐观并发 source+diff、过滤语法、CE 无 DDL 编辑的边界 |
| `../terminals-ref/pgadmin4` | PG Web 管控台 | msql 预览=保存数据流、活动监控、\\copy 导出、fetch+1 翻页 |
| `../terminals-ref/pgweb` | Go 极简只读 PG | 最小集校准、列值统计、耗时、filter-by-value、int64 精度守卫;CSV 全量 buffer 反例 |

克隆均为 `git clone --depth 1 --single-branch`,只读参照,不参与构建。
