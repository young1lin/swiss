# 更新日志(中文)

swiss 的所有显著变更都记录在此;格式遵循 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
版本遵循[语义化版本](https://semver.org/)。本文件是 [CHANGELOG.md](CHANGELOG.md) 的完整中文版,
与英文版同步维护。

## [未发布]

## [0.2.2] - 2026-11-12

### 新增

- 进程页的添加面板新增"模板"下拉:选择 dsh 的启动方式 —— `dsh web`,或"用 swiss 启动"时的
  `dsh web --no-open` —— 名称、程序、参数、端口和镜像随之填好。端口从 3080 起始且始终归你:
  修改端口(3080 可能被占),参数里的 `--port` 随之跟随;端口、`--port` 和镜像清空时一起离开。
  dsh 经 npm shim 运行在 `node.exe` 后面,表单按程序名(`dsh`)猜的镜像会把运行中的应用
  误判为自我冲突,模板直接指定 `node`。首次保存后下拉锁定:被警告留住的已开面板编辑的是
  已保存的应用。同一个下拉还提供 `cc-otel start`:`~/.cc-otel/cc-otel` 的启动与停止、
  cc-otel.yaml 的端口(4317/4318/8899)和 UI 链接;程序路径开头的 `~/` 现在也像目录与
  日志一样按主目录展开(SPEC §process.panel)。
- 面板活在事件总线上,不再靠轮询:全局 6 秒定时器已移除。每个页面在 WELCOME 时重读,之后跟随
  自己的主题 —— 内存芯片、令牌、凭据库、导航的插件列表、作业列表与"立即运行"、隧道表、MCP
  调用日志/流量/健康、终端会话、四个 LLM 页面,以及 Remote 的目标页与运行页 —— 正在看同一份
  数据的页面共享一次重读(SPEC §bus)。
- 一次运行会说"它为什么运行"。`swiss remote exec`、`sync`、`write` 接受 `--why`;目标在
  第一条命令前给出简报(`swiss remote brief`);CLI 会说出运行它的是哪个编码代理 —— Claude Code、
  Codex、opencode 或 DSH —— 依据自身标记或祖先进程的环境,即使启动器的变量随后被清理,记录
  依然说得出是谁在操作(SPEC §remote.history)。
- Why 轨迹:打开一条运行可以看到代理当时被要求做什么、执行了什么,内容读自代理自己的会话
  日志 —— 读取按该日志的路径形状把关,存储与返回前先脱敏。轨迹搜索在网关重启后可以继续;
  Codex 的 code-mode 单元格即其执行的命令,只计一次;Claude Code 工作期间输入的消息就是轨迹
  的提问;opencode 的轨迹重新读取其数据库;`swiss run audit --session <id>` 把审计收窄到
  一个代理会话提交的运行(SPEC §remote.history)。
- 隧道自愈(SPEC tunnels.selfheal):通道打开最多等待 10 秒;连续三次打开无应答判定会话卡死;
  判定前先探测会话 —— 迟到的应答会正确关闭而不是把会话挂死。
- 工作区分隔条的边缘可以拖动。

### 修复

- 打开一条带 stdin 的运行行会显示它执行过的脚本。stdin 一直都有封存,但该行此前只显示
  "Stdin sent 2.3 KB",要点区块最末端一个纯图标按钮才看得到,一条 `bash -s` 的运行读起来
  就像 stdin 丢了。现在打开该行即在输出上方解封显示;隐藏可收起,重读后保持收起;关闭该行
  忘记文本,直到再次打开。进行中的行不再说文本"未保留":它说文本随记录而来,记录归档后即可
  显示。写入请求的正文仍需点击"显示"(SPEC §remote.panel)。
- 在运行页读取期间归档的运行只出现一次,不再同时出现在页面的两个列表里。
- JOBS.LIST 跟随事实表,而不是它的文件。
- MCP 健康:`start_timer` 会重新武装 `stop_timer` 中止后的健康持有器;按龄清理会公布
  它删除了什么。
- 导入把凭据库并入 bundle 的同一批次落盘;新的内存订阅者能听到未变化的数字。
- 启动发现端口被占时会点出持有者:失败信息说明是哪个 pid 与镜像占着端口("端口 3080 由
  pid M(node.exe)持有,而非镜像 dsh"),并给出镜像修正建议;冲突不会被"用 swiss 启动"重试。
- 以 CRLF 结尾的脚本经管道传给 stdin shell 时按 LF 发送。
- 清空运行历史前会同时查询两个存储,再决定它是否真的什么都没移动。
- 工作区窗格收窄,而不是把终端挤出窗口之外。

## [0.2.1] - 2026-11-08

### 修复

- 分析账本和控制台存储位于网关的 home 目录之下。`AnalysisStore::home()` 把 `llm/` 拼到了用户的
  home 目录而非网关的 home 目录上,于是 0.2.0 把控制台和分析封存进了 `~/llm`,紧挨着拥有其余
  每一字节状态的唯一 home(`~/.swiss`, SPEC §core.home);事件总线的控制台存储走的也是同一个
  根。启动会收编 0.2.0 留下的 `~/llm`:默认 home 上的网关会在任一存储打开之前,把 `analysis`
  和 `consoles` 移入 `~/.swiss/llm`,放到已有记录的旁边,绝不覆盖任何一条;与已有条目同名的
  游离条目留在 `~/llm`,并在日志中点名(SPEC §core.home, §data.analysis, §data.tabs)。
- 一个 Redis pub/sub 拒绝若带着 redis-rs 不认识的错误码(代理自己的码,作为扩展错误归档),现在
  会读成与 `-ERR` 和 ACL 的 `NOPERM` 相同的那句 "the server refused SUBSCRIBE"(SPEC §data.pubsub)。
- MongoDB 命令文档如今无论键以何种顺序到来都能执行。服务器把第一个键当作命令,而模型会按任意顺序
  写键,于是 Data 代理暂存的 `{"documents": [...], "insert": "orders"}` 曾在控制台里以
  "no such command: 'documents'" 失败。控制台、代理的读取与暂存以及 `mongo_command` 现在会在
  任何闸门读取之前先把命令放到最前,并拒绝两个命令都可能打头的文档。代理的工具还会解开嵌套在第二
  个 `command` 键里的命令,并提取出写在文档内部的 `db`(SPEC §data.mongo-panel, §data.agent)。
- 收到裸数字 `{"$numberDecimal": 299.00}` 的数字包装器会保住其类型。控制台的解析器曾把它打印成
  `NumberDecimal("[object Object]")`,网关曾把它当作无效的 Extended JSON 拒绝。控制台现在按读取
  `NumberDecimal("299.00")` 的方式读取它,数字保持键入的原样;网关保住类型,其数字是 JSON 解析
  器把该数字解析成的 double 的数字(`299.0`),于是字符串形式保持精确的那一个。代理的工具描述和
  提示词给出字符串形式和一个插入示例(SPEC §data.mongo-panel)。
- Data 代理的 `describe_table` 现在按照 `list_tables` 打印它的方式来接受表。给定
  `demo.user`(或 `` `demo`.`user` ``)时,它曾查找一张字面名为 `demo.user` 的表并失败;现在
  名字会拆成它的 schema —— 在 MySQL 上即数据库 —— 和表名。MySQL 的 "no such table" 会点明它
  查找过的数据库,而不是连接的默认数据库(SPEC §data.agent)。
- Redis 控制台的命令目录在 redis 6 上不再为空。不带名字的 `COMMAND INFO` 只从 redis 7 起才会
  列出每条命令;答案为空时,网关会再问一个裸 `COMMAND`,于是在 6.x 上名字和键位置也能补全
  (SPEC §data.redis-console)。
- `remote sync`/`push` 会重发让文件字节数保持不变的编辑。此前的判定只看大小,于是同大小的编辑
  看起来已是最新,从未跨线传输。快速检查如今是 rsync 的那一套 —— 大小相等加上整秒级 mtime
  相等,绝不比较谁新谁旧,于是时钟不一致也无法造成循环,而没有 mtime 的 stat 仍保持只看大小的
  规则。每次上传都把本地 mtime 盖到远程文件上:整文件通过一个新的 transport set-mtime
  (SFTP SETSTAT),增量则由脚本在其重命名之前以 utime 盖戳。规则生效前上传的文件,其 mtime 携带
  的是上传时间,于是大小匹配而 mtime 不匹配的文件会先做内容校验 —— 每 200 条路径一批执行一次
  `sha256sum`(argv,绝不是 shell 字符串)—— 匹配的文件保留其字节,只修正 mtime,并计为最新;
  没有 `sha256sum` 的目标会把整批整体发送,并附一条说明(SPEC §remote.actions)。

## [0.2.0] - 2026-11-08

### 新增

- `swiss setup guide` 打印 `SETUP.md`。同一份文件随每个发布归档一起提供,也作为单独的资产提供,两条技能安装路径都会把它写到 `swiss` 技能旁边(SPEC §host.setup, §release)。
- `swiss connect <claude|codex|cursor>` 把该客户端指向本网关的 MCP。bearer token 是一个标记为 `client-<client>` 的令牌,复用或新造,且从不打印。清单的 `clients` 节做同样的事(SPEC §host.setup)。
- `swiss secret ask <name>...` 在这些名称上打开机密信息页面,并且不打印登录链接。它立即以 4 退出,或在 `--wait` 看到每个名称都已存储时以 0 退出。在没有桌面的环境里,它会给出端口转发做法和 `swiss secret set`(SPEC §host.setup)。
- `swiss setup show` 把本网关的状态打印成一份清单,机密信息名称写作 `ask`,凭据所在之处放脱敏哨兵。引用会被保留。把这份清单交回 `plan` 会被拒绝,因为掩码不是凭据本身(SPEC §host.setup)。
- 清单的 `machine`、`tunnels` 和 `remote` 节配置一台机器的其余部分。`machine` 通过面板的路由设置用户的 PATH 和登录时自启动,并安装它点名的技能,而且从不移除一个。`tunnels` 添加 SSH 连接 —— 跳板跃点在前 —— 以及转发规则,这些规则只在主机密钥被固定后按 `start: true` 启动。`remote` 按连接名称添加目标(SPEC §host.setup)。
- 清单的 `plugins` 节打开或关闭一个插件,并在它的行上合并一个补丁,保留补丁没有点名的部分,因此 `swiss setup` 可以添加一个作业或一个 LLM 提供商。`plan` 通过新的 `PUT /api/plugins/{id}/config?dryRun=1` 询问插件自己的校验器,提供商由它的模型列表检查 —— 这不产生计费 —— 而 `swiss setup schema plugins.<id>` 打印一行接受什么(SPEC §host.setup)。
- `swiss setup apply <file|->` 让清单的 `secrets` 和 `mcps` 在本网关上成真,然后检查每个 MCP。它只添加,因此同一份文件应用两次什么也不写,退出码说明它是收敛了、失败了还是在等待人(SPEC §host.setup)。
- `swiss setup plan <file|->` 读取一份由 `secrets` 和 `mcps` 组成的 JSON 清单,或客户端自己的 `.mcp.json`,并作为一份 JSON 文档说明它会在本网关上改变什么,不写任何东西。`swiss setup schema` 说明清单可以持有什么。键入清单的凭据会被拒绝且不被引用,所有问题一次性列出(SPEC §host.setup)。
- `swiss secret set <name> [--from env:VAR|file:PATH|dotenv:PATH#KEY]` 把一条凭据库机密信息存入凭据库,其值不经过命令行,而 `swiss api --body-file <path|->` 从文件或标准输入取请求体 —— Windows PowerShell 5.1 在那里没法剥掉它的引号(SPEC §host.cli)。
- SQL 分析持久化业务条件、修订、执行和结果页面。Data 代理和工作区共享稳定的任务/结果引用,用于追问、重跑、检查、图表、对比、保存的工作和导出(SPEC §data.analysis)。
- System 在受支持的客户端目录里公开捆绑技能的安装与状态;Plugins 把插件管理从机器设置中独立出来掌管(SPEC §panel.settings, §host.cli)。

- `swiss gui` 在 Windows 和 macOS 上把面板打开在它自己的窗口里(SPEC §host.gui, ADR-038):同样是 `open` 铸造的那次一次性登录,由一个带托盘(关闭即隐藏、打开/退出)的 WebView2/WKWebView 窗口花掉,每个网关一个窗口 —— 以端口和 home 命名 —— 因此另一个 `SWISS_HOME` 下切换配置是另一个窗口,绝不是常驻的那个,而同一网关的第二个启动器会把它抬到前台,而不是悄悄退出。显式命令像 `swiss start` 一样脱离并返回;`swiss gui -f` 留在前台。窗口和托盘带着品牌标记本身,取自 `assets/logo.svg`;macOS 还发布签名的 `.app` 包和由 Keychain 支撑的主密钥存储。Linux 保持无头:网关服务,浏览器看。
- Data 页面在 MySQL、PostgreSQL、MongoDB 或 Redis 连接上的控制台旁有一个代理(SPEC §data.agent, §llm.agents, §llm.api, §panel.ui)。

  在此变更之前,查询必须在控制台里手写。

  现在:
  - 头部的火花按钮在页面的末端边缘打开一个窗格,在那之前它一直折叠着。用你自己的话问。
  - 该窗格与一个 DB 代理一起工作:一个拥有自己运行时的代理,`db`,专为此页面打造,别无他用。它的提示词和工具是固定的,每种连接各有一套;它自己的提示词(如果有)附在其后。没有时,*Make a DB agent* 打开 Agents 页面的 Add 表单并预设到它。Memory 的任务和 Agents Test 从不使用 DB 代理,页面也从不借用任务代理。
  - 代理读懂数据是怎么摆放的,为该连接写出语句或命令并运行它。它运行的东西落进它自己的 **Agent** 控制台标签 —— 一条语句、一条以控制台 shell 语法写成的 MongoDB 命令、一行 Redis —— 结果落在结果标签里。你自己的控制台不受影响。你的问题按你键入的样子显示,回答以它最后运行的内容收尾,与运行时一字不差。
  - 在 MySQL 和 PostgreSQL 上,一次运行是只读事务里的一条读语句,事务总会回滚。写或 DDL 只在控制台里为你暂存,由你来运行。
  - 在 MongoDB 上,代理列出集合,从文档样本描述其中一个,并运行一个只读命令文档(find、aggregate、count、distinct、...)。带 `$out` 或 `$merge` 的聚合会被拒绝,写只被暂存。
  - 在 Redis 上,代理用 SCAN 翻页键空间,读取任意类型的键,并运行一行只读命令。一个扫描页会一直走下去,直到攒够 200 个匹配的键或两秒钟过去,因此稀疏的模式也只需一次调用。KEYS、写和脚本从不运行;写只被暂存。它还会按最新在前的顺序连同消费组读取流,列出有订阅者的发布/订阅通道(`PUBSUB` 读会通过它的门),并监听通道几秒钟,看看发布了什么。开场提示词会说明有多少通道有订阅者。
  - 要一个新标签,代理就会以某个名字打开一个它自己的控制台标签(`open_console`)。它运行或暂存的点名那个标签的语句落在那里,它们的结果落在其下;其余留在 Agent 标签里。再被要时,它把同一个标签带回来。在此变更之前,它运行的一切都落在 Agent 标签里,它没法把一个报告的各条查询分开。此变更前打开的会话从下一次发送起拥有这个工具;New 带来提示词里对何时使用它的说明。
  - 一个控制台标签按它的 id 寻址,而不只是按名字:`list_console_tabs` 列出该连接的标签 —— 打开着的,然后是仍在 Recently closed 里的 —— `read_console_tab` 逐条读取语句。`edit_console_sql` 和 `delete_console_sql` 按标签的 id 和语句的索引改写或删除某个标签的一条语句,`open_console` 在名字之外也接受 tabId。id 活过重命名、关闭和重新加载,名字则不然:两个叫 *SQL* 的标签曾是同一个地址,标签里的一条语句根本无法更改,读者改了名的标签也不再应答代理打开它时用的名字。
  - 提示词让一个控制台标签只装一种工作:当另一种工作开始,或正在用的标签已装了很多语句时,代理打开一个以该工作命名的新标签,而当那种工作回来时按名字回到那个标签。你关掉的那个代理的标签,在代理再点到它的名字时会带着原有文本回来。
  - 代理找到并描述的东西显示在页面上:`list_tables` 或 `list_collections` 把它的 grep 放进侧栏的搜索,于是树展示代理读过的列表;一次表 `describe_table` 读取在它的标签里打开到 Structure,一次集合 `describe_collection` 读取打开到 Schema,像 Analyze 采样那样在那里采样。在此变更之前,这两者在页面上什么都不显示。
  - 在 MySQL 和 PostgreSQL 上,模型旁边的按钮默认为 *Staged*,可为连接打开 *Auto writes*(SPEC §data.agent Writes)。在自动模式下,代理运行一条 INSERT 或 REPLACE,或一条 WHERE 点到某列的 UPDATE 或 DELETE,并在它改动不超过 1000 行时提交;超过这个数,写被回滚。没有 WHERE 的 DELETE 或 UPDATE、带 `1=1` 或另一条不点名任何列的 WHERE 的那种、DDL,以及对服务器自身 schema 的写从不自己运行:它们未运行地落进控制台,toast 说明原因。已提交写的结果读作 *Committed · 3 rows changed* 和插入的 id。MongoDB 和 Redis 的写保持暂存。在此变更之前,每一次写都为你暂存待跑。
  - 代理在 Redis 上读到的东西,一边被读一边在页面上打开:键或流在它自己的标签里,通道列表在 Pub/Sub 页面按代理的模式显示,那里还有一次监听 —— 名字被加进监视集(页面继续实时监听),听到的内容以 *Agent* 行记进日志。那份订阅流是页面的,所以代理用 `stop_listen` 结束它 —— 即读者自己的那个 Stop,外加一行说明是谁要求的 —— 并说这个请求已发出,而不是宣称一个它看不见的订阅流已经关闭。
  - 每个连接保持自己的会话,因此两个数据库绝不会混进同一段对话。窗格打开时会话被恢复;New 重新开始,Stop 取消。
  - 会话上方一条折叠的行点出模型可以调用的工具。
  - 回复流式输出期间你发的话,在输入框上方的队列里等待(*Queue*),如同 Codex。可逐条编辑或删除,或用 ↑ 立即把它插进回复(Ctrl+Enter 全部插入);代理在它的各步之间读取被插入的一条。回复结束后,第一条自动接着发,每轮一条。Stop 保留队列。
  - *History* 列出该连接的会话。打开一条来读,用 *Continue here* 把它接着进行下去。
  - *Compact*,或 `/compact` 加上要保留什么,把会话压缩成一个检查点并从它继续。失败的压缩让会话保持原样。在输入框里键入 `/` 会列出该命令供补全(方向键、Tab 或 Enter,Escape)。
  - 输入框随你的键入长高,到窗口的 40 % 为止,然后开始滚动。
  - 输入框下方,如同 dsh,一条居中的安静行停在底部,统计着这场会话。轮次、模型的步骤和它的输出速度打开 Session statistics:LLM 时间、工具时间、到首个令牌的平均时间、每秒令牌数。使用的令牌和缓存命中率打开 Token usage:未缓存输入、缓存输入、缓存写入和输出,每一项都精确。它末端的圆环显示下一个请求相对代理 Max context 有多满 —— 提供商对上一轮的计数对比一个固定上限(默认 *124K / 256K*),除非会话刚被压缩,否则绝不是猜测 —— 点开可见是什么填满了它:系统提示词、工具定义、消息。
  - 复制按钮:在你的每条消息上、在每个回答上(整段 markdown),以及回答里的每个代码块上。
  - 代理会坚持:它重试超时或断链的调用,修好一个错误再跑,一条路堵住时换另一条,并且不再结束一轮、提出一个它自己就能做的重试。一次 Redis 监听会重试超时的订阅,一共三次。此变更前打开的会话需要 New。
  - 代理用朴素的散文回答:没有表情符号、装饰性符号或感叹号,答案在前,表格只用于表格化数据,它最后运行的语句放在代码块里,结尾没有一串提议的清单。它很少保存笔记 —— 一条查询经过几次查找才验证过的路径、一个让一次尝试付之东流的坑,或你让它记住的东西 —— 并且从不在回答里提到保存过。此变更前打开的会话需要 New。
  - Send 旁边,一个按钮显示模型和思考投入,如同 dsh:*Model* 带搜索框列出每个提供商的聊天模型,*Effort* 从 Low 到 Max。选择按代理保存在这个浏览器里;它从代理自己的模型开始,投入取代理的设定,否则取 Max。进行中的会话在下一次发送或压缩时改用所选。它较早的各轮回放时不带另一个模型的签名思考,并且模型会被告知一次它变了。stream、compact 和 reopen 路由接受 `provider`、`model` 和 `effort`。
  - 开场提示词只在表不超过 100 张时点名数据库的表;超过则给出数量,代理按名字找表。
  - 每个连接为代理保有一份自己的记忆:关于它的数据住在哪里、怎么到达的笔记,比如哪张表存着一个活动房间绑定的用户,以及怎么从那里走到用户。一条笔记是 `~/.swiss/llm/db-memory/<connection>/` 下的一个 markdown 文件,`MEMORY.md` 作为索引。这与 swiss memory 是分开的。新会话先读索引,打开一条覆盖该问题的笔记并沿它的路线走。当一条路线是实打实找出来的,代理就保存它(`read_memory`、`save_memory`、`delete_memory`);它保存的是路,绝不是数据。头部的 *Memory* 按钮列出这些笔记:打开一条来读,或在那里添加、编辑或删除笔记。此变更前打开的会话没有索引也照样继续;New 开启一个带索引的。
  - 面板库新增 `chatPane`(带一个 `queued` 槽、一个会长高的输入框、`commands` 的 "/" 补全,以及一行 `stats`)、`chatQueue`、`chatStats`、`copyBtn` 和 `copyText`;一段对话轮次接受 `copy`,`markdownNode` 为其代码块接受 `copy`;`grip` 新增 `end`、`folded` 和 `gripFold`。
  - Agents Test 和 Data 代理的流在每轮模型之后发送一个 `usage` 事件,`GET /api/llm/db/context` 称量一场会话的下一个请求。

- Data 控制台关掉的标签可以找回来(SPEC §data.tabs)。装有文本的控制台在它的 × 关掉它、一次批量关闭带走它、数据库切换,或页面重载或关闭时被归档;标签条的溢出菜单按连接在 *Recently closed* 下列出它们,选中一条便带着它的文本和名字重新打开。在此变更之前,关掉的控制台就永远没了。

- 代理的 Test 在读和 shell 之外还有 write、edit、glob 和 grep 工具,它的会话以系统提示词里的一份环境快照开场 —— 工作目录、仓库和分支、home、平台、shell、日期、代理和模型 —— 目录移动过会在随后一轮里告知(SPEC §llm.agents, §llm.api)。
- 内置代理的 shell 命令可以按引用使用凭据库机密信息(SPEC §llm.agents Secrets, §llm surfaces Agents)。

  在此变更之前,只有模型看见凭据,凭据才能到达代理的命令。

  现在:
  - 代理的 `secrets` 授予凭据库名称、`group:<group>` 或 `*`。默认什么都不授予。Agents 表单有一个 *Secrets* 字段。
  - `bash` / `powershell` 命令里的一个 `${secret://name}`,在 shell 会展开变量的地方,通过该命令的环境到达命令。它绝不到达命令行、运行记录或模型。
  - 在单引号里,引用保持文本,因此一个文件可以保存引用本身。
  - 写工具、记忆保存和 `remote.write` 从不解析引用。
  - 授权未覆盖的引用在命令运行前被拒绝。
  - Test 的第一轮点名被授予的机密信息,让模型知道它可以用什么。
  - 内置代理的模型读到的每一个工具回答都会对照每一个已存储的值脱敏,`remote.exec` 和 `remote.cat` 的输出也一样。
  - 本地命令的 stderr 和它的 stdout 一样被脱敏。
- 代理的 Test 对话框在回复流式输出期间接受消息(SPEC §llm.api, §llm surfaces Agents)。

  在此变更之前,*Ask* 在回复结束前是死的。

  现在:
  - 回复流式输出期间,*Ask* 变为 *Queue*。排队的消息在输入框上方的队列里等待,带 Edit、Delete 和 ↑(立即插入);Ctrl+Enter 全部插入。
  - 被插入的消息进入正在进行的运行的收件箱(`POST agents/test/steer`);删除它就撤回它(`…/steer/withdraw`)。
  - 内置循环在一轮的工具回答之后、再次发问之前读取它。一个 `steer` 事件把它描绘成你的一轮,历史也把它归档为一轮。
  - 干净的一次回复后,排队的第一条消息自动作为下一轮发出,每轮一条。
  - Stop 或失败保留队列;之后 ↑ 发送一条。
- 进行中的代理运行,以及一次记忆同步的代理运行,现在可以在它们工作时被跟随(SPEC §llm.history Live, §memory.sync)。

  在此变更之前:
  - 一次运行的各轮只在它结束时归档,因此一个干了十五分钟活的内置代理除了它的调用什么也显示不出来;
  - 一个运行中的同步运行只说 *running*。

  现在:
  - LLM 历史把进行中的运行列在最前,每个都带着它的轮次、它在等什么(模型,连同目前已流出的字符数,或一个点名的工具)以及等了多久。
  - 进行中运行的轨迹每 3 秒重读一次,并保持读者展开过的步骤。它最后那个脉动的步骤就是正在进行的工作。
  - `GET /api/llm/history/runs?source=memory:<sync run id>` 列出一个作业启动的运行。
  - 同步运行在答案出来前说明它发送什么、问的是谁:阶段 `ask`,带着它的提交、提示词大小、运行器和模型,然后是阶段 `apply`。
  - 记忆同步表单在每个运行下面列出它的代理运行,进行中的在前,每一行都在 LLM 页面上打开那个运行的轨迹。
- redis 连接的通道有了自己的页面,Pub/Sub(SPEC §data.pubsub):通道不是键,因此键列表从不显示通道,控制台也拒绝 `SUBSCRIBE`。`GET /api/db/{name}/pubsub?pattern=` 列出 `PUBSUB CHANNELS`(已排序,至多 1000 条)及每条通道的 `NUMSUB` 和 `NUMPAT` 计数;`POST /api/db/{name}/pubsub/subscribe {channels?, patterns?}` 在自己的一条连接上订阅,并以 NDJSON 流式输出(`subscribed`、`message`、每秒超过 200 条后 `dropped`、每 15 秒一个 `tick` 好让死掉的隧道终止订阅流、带原因的 `closed`),每条连接至多 4 个订阅流。面板的 Pub/Sub 标签(从 redis 树的第一行进入,固定在键上方,或从连接或某个键标签的 ⋯ 进入)把实时订阅流、监视集和 *Watch everything* 放在通道列表和一行发布旁边;标签转入后台时订阅流停止,回来时把空档记进日志。Recent 给出这个浏览器此前在该连接上监视过的名字(20 个,最新的在前;*Clear recent* 忘掉它们),一条打开的消息提供 *Copy channel* 和 *Watch only this channel*,日志在其中文本被选中期间暂停重绘,而一条也被某个被监视模式匹配的通道会被提示为到达两次。
- 一次记忆传送是一份知识 PR,合并之前先读(SPEC §memory.store, §memory.cli, §memory.api, §memory.panel):导出带有一个说明它属于谁的头部(项目、领域、谁读它、一个人确认了多少条笔记),`swiss memory export --domain PATH` 和 `GET export?domain=` 写出一个领域连同其子树,而导入(`--into P|.`、`--dry-run`、`--strategy fail`;`POST /api/knowledge/import`)把每条笔记落到同路径的领域之下,并报告它创建了什么、把什么当作重复跳过,以及它拦下的每一个冲突 —— 同一 id 的另一修订,或同标题不同文本的一条活笔记 —— 两侧都会展示。Memory 页面的项目菜单新增 *Export…* 和 *Import…*(Check 是一次试运行)。此前,导入对存储里已持有其 id 或标题的笔记一声不吭地跳过。
- Memory 的同步在 Claude Code 之外还读 DSH 会话(SPEC §memory.sync):转录目录按其布局被识别,DSH 的 `--<key>--` 会话目录按其最新一代日志逐帧读取纯文本或 zstd 压缩的内容,只保留这个人键入的内容和代理的文本回答 —— 绝不保留推理、工具调用、注入的指令或子代理的会话。默认的 `transcriptDirs` 增加 `$DSH_HOME/sessions`(设置时)和 `~/.dsh/sessions`。
- 使用中失败的检索会被记录下来,并作为评估查询回来(SPEC §memory.evaluation, §memory.mcp):`memory_missed`(第十二个 MCP 工具)、`GET|POST misses` 和 `swiss memory misses list|add|drop` 按项目保存一条未命中 —— 查询或任务、展示的笔记、误导的那几条、想要的那几条,或用话语说出的想要什么 —— 最新 500 条,绝不随笔记一起导出。`swiss memory misses export` 写出一份评估会读回的夹具,因此一次未命中会在那里像在使用中那样失败。存储迁移到 schema v11。
- 一次搜索和一份任务简报都以评判所找到的东西收尾(SPEC §memory.search, §memory.brief):一行结束语称命中为强(第一条命中包含查询,持有者同属一个领域家族)、混合(只有部分命中包含它,或持有者被分开归档)或弱(每条命中至多共享一两个词),各带下一步做什么 —— 从第一条命中作答、确认它或说出是哪个领域、换种问法。每条命中都带它的词份额(线上字段为 `wordShare`)。
- 一份聚焦的简报在它所指之处点名尝试过又被放弃的东西(SPEC §memory.brief):在 *Rejected before* 之下,至多三条已被取代或已失效的决策 —— 它们被某条给定路径锚定,或握着任务一半的词 —— 各附其继承链在何处终结。一条只是细化前驱主张的继承者不会列入。此前,代理可能再次提出项目早已换掉的 Redis 缓存。
- 一次保存,其标题与同一文件上某条活笔记的主张相反时,会点名那条笔记(SPEC §memory.capture):"Never retry the refund webhook" 旁边是 "Always retry the refund webhook" 时,以 `contradicts` 回答,附上较旧的笔记和标记其过时或把两者链起来的命令。是提示,绝不是拒绝;英文和中文的否定都一样。
- 用户给出的规则按他们的话保留(SPEC §memory.capture):保存策略、同步的规则和技能会请求一个偏好,或用户做过的一个决定,用他们所说的话开场(带引号并注明日期),而缺少引号或日期的保存会收到 `user_words` 成形提示。`swiss memory lint` 对任何草稿问这个;面板保存就是用户自己的话。
- 一个项目可以每天一次复查代码变了的笔记(SPEC §memory.init, §memory.settings, §memory.panel):`swiss memory review --resolve --changed` 和同步表单的 *Review what changed* 只评判锚定文件之下被改动的模型笔记,项目的 `autoReview` 一天中的时刻(`--auto-review`)按网关的时钟排一个 —— 网关停掉的那天被跳过,绝不补上 —— 而 `autoReviewRuns`(1 到 10,默认 3;*Daily review runs*)让时钟在同一天再排一个,只要上一个在其 200 条之外还留着问题。空的复查会删除它之前的空复查,因此安静的一周只留一行;第二个复查请求并入已排队的那个。
- Memory 页面说明一个项目如何跟上它的代码,并把它设在一个地方(SPEC §memory.panel):项目头部的 *Auto-update* 按钮读作 *Auto-update · daily 21:30* 或 *off*,并打开自动更新表单 —— 每日复查的开关、时刻、一天次数和工作线程由一次 PATCH 保存,还有什么在等(收件箱里变了的笔记,模型的与人的分开列出),时钟下次何时查看以及上一次复查结果如何,*Review now*,以及共享同步块的自动开关和间隔,挨着安装钩子的命令。Review 标签的横条在模型的改动笔记等待时标上 *no daily review*,离开启只差一击。同步表单的项目折叠不再持有每日复查的字段:它说明复查的现状并打开该表单。
- 从一个链接工作树问出的问题读那个工作树的代码,包括其分支的新文件,而项目仍是主检出(SPEC §memory.scope, §memory.rag):`memory_ask` 和 `swiss memory ask --worktree` 接受工作树的路径或一个已检出的分支,锚定到仅分支文件上的代理笔记在自己的工作树里被评判,而不是被当作缺失。
- 一次保存会提示一条笔记复述了什么(SPEC §memory.capture):一行的文字大多来自其锚定文件自身话语的笔记,会得到提醒性的 `restates_code` 成形提示,并显示在面板里。
- figma MCP 定义带上自己的 `proxy`(SPEC §mcp.figma, §mcp.oauth),面板的 figma 表单增加 Proxy 字段:OAuth 握手的发现、注册和令牌交换现在经由该定义的代理发出,否则经由系统代理,因此无法直连 Figma 的网络也能完成授权。ADR-021 的修正案否决了集中的代理设置页。
- `swiss-memory` 是项目记忆读与写共用的那一个技能(SPEC §memory.mcp The skill):除了简报、保存、枢纽和 CLI,它现在还说明哪个读工具适合代理手里的东西 —— `memory_search`、`memory_related`、`memory_domain`、`memory_get`,以及从记忆回答一个问题的 `memory_ask` —— 如何搜索和阅读一个项目保存的文档、在代码索引里查找一个名字,以及哪些运行让记忆保持最新(仅在用户请求时排队)。它的工具表点名全部十一个工具,一个测试让它与 MCP 的列表对齐,而 `swiss skill install` 的用法、README 和发布归档把它带在 `swiss` 和 `swiss-remote` 旁边。
- 人可以把评审收件箱交给同步代理(SPEC §memory.init The review run):`swiss memory review --resolve [--dry-run] [--wait] [--lang TAG] [--max-asks N] [--agent A[,B…]|off]`、带 `review: true` 的 `POST /api/knowledge/sync` 和同步面板上的 **处理评审收件箱** 都会排队一次评审运行(`trigger: "review"`)。
  这是一次复查,触达每一模型写下、带标记的存活笔记,已确认的也在内;每个问题都会说明笔记为何在等(变更的文件、它抵触的笔记、消失的依赖、未检查的天数),被 `contradicts` 链接连起的笔记最先并排提问。
  代码证实的内容(一次保留,或一次已落地的修复)按人的请求予以确认;两边都被扣留的抵触,除非答案再次链接它们,否则予以解除。被代码检查扣留的笔记保持扣留,人写的笔记绝不被评判、改写或确认。
  范围的 `recheck` 计数新增 `confirmed` 和 `lifted`。评审运行会在同一运行内解决自己的答案引出的抵触:一条把笔记链接为抵触收件箱之外已确认笔记的答案,不再等第二次运行 —— 收件箱会再读一遍,
  模型两条笔记之间的每个抵触并排提问,至多 3 轮、一对至多两次,都在 `maxAsks` 之内;仍然成立的计入 `open`,连同所用的 `rounds` 一起在摘要和面板行里说明。
- 复查也会解决抵触,完整运行收尾的复查同样如此(SPEC §memory.init The recheck):模型两条未确认笔记之间、其中一条未评判的 `contradicts` 链接会最先并排提问 —— 评判前已检查过的一方摆在旁边 ——
  当答案保留或修复双方且不再链接它们时即解除。在计划的问题之后,运行会在同一运行里问出自己的答案引出的、或仍立在模型两条未确认笔记之间、其中一条被运行评判或链接的每个抵触:
  至多 3 轮,一对至多两次,都在 `maxAsks` 之内。复查不确认任何东西;与人写的或已确认笔记的抵触,或运行从未触碰的两条笔记之间的抵触,在评审收件箱里等待。
  范围的 `lifted`、`rounds` 和 `open` 会为复查计数,并在其摘要、CLI 的进度行以及复查或完整运行的面板行里说明。
- 项目把它的文档完整保留(SPEC §memory.docs):设计文档、计划、陷阱日志和参考(`md`、`markdown`、`rst`、`adoc`)放在其设置点名的源下(`docs`,检出内至多 8 条路径;`swiss memory docs add|remove PATH…`)。
  文档运行(`swiss memory docs sync`、带 `docs: true` 的 `POST /api/knowledge/sync`)读取它们 —— 新增或变更的文件脱敏后按标题切成小节并以 CJK 二元组索引,消失的文件删除 ——
  然后为每份新增或变更的文档向工作代理要一张卡片:种类(`reference`、`design`、`plan`、`proposal`、`rejected`、`pitfalls`、`other`)、摘要、所属领域、英文与运行语言的搜索关键词,
  以及它谈到的代码名字,每个都在代码索引里查过,命中即说明代码在哪里。`swiss memory docs search|read`、`GET /api/knowledge/docs[?q=]` 和 `GET /api/knowledge/docs/read` 提供它们,
  提问端(§memory.rag)获得 `docs`、`doc` 和 `code` 工具 —— 一次性运行时把匹配的小节内联进去 —— 于是"让一份拷贝在闲置 30 天后暂停的代码"一次搜索加一次读取就能到达作业的文件。
  设计、计划、提案和被否决的文档被声明为意图,而不是代码的行为。笔记种类不变。存储迁移到 schema v9(`memory_docs`、`memory_doc_sections`、`memory_doc_fts`);v8 的构建会拒绝它。
- 一个问题,从项目的记忆里得到回答(SPEC §memory.rag):`POST /api/knowledge/ask`、MCP 的 `memory_ask` 工具、`swiss memory ask "QUESTION" [--agent A] [--json]` 和 Memory 页面的提问框。
  代理由与运行提问相同的各层选出(提问自身的 `agent`、项目设置、同步块、最新的代理 —— 这是一次读取,因此没有轻推规则);内置代理在只读记忆工具(search、related、get、domain、brief)上以有界循环处理问题,
  拉起的运行时拿到内联好的网关检索结果并作答一次,答案会点名它所依据的笔记 id。一个问题至多 4000 字符;一个 cwd 以注册在其上的项目设置发起提问;没有代理可作答、或代理失败,即 `memory_agent_unavailable`(503)。
- MCP 的 `memory_related` 工具(SPEC §memory.mcp):从一条笔记或一段文字出发的图谱 —— `id`/`text` 恰好其一,`depth` 至多 3,`limit` 至多 50 —— 相似层未运行时附上语义状态。
- Memory 页面的同步面板说的是分层层级汇聚后的世界(SPEC §memory.panel、§memory.settings):概览点名每一位在用的工作代理和领队,各自标明选出它的那一层(项目设置、同步块、插件的最新代理)以及运行失败时会带出的那句话;
  同步块把代理编辑做成三选 —— 继承(回写为空,绝不悄悄写成 off)、off、或 llm 插件各代理的复选池(丢失的名字仍显示并保持勾选)—— 领队则是继承、off 或一个名字;
  项目自身的设置(代理、领队、嵌入)放在自己的折叠区,一次 PATCH 保存并按应答重绘;地图运行、完整运行或复查运行携带本次运行自己的工作代理与领队选择(复查运行绝不携带领队);而提问过的运行会点名每位工作代理及其表现。
- 项目可以为自己关闭嵌入(SPEC §memory.embedding、§memory.settings):项目设置里的 `embedding` —— 在 `swiss memory project settings --embedding` 上是 on|off|inherit,在设置 API 上是 true|false|null,全局项目也可以设置。
  关闭后,该项目的笔记不再是嵌入输入(索引器跳过它们,状态里的 indexed/pending 从不把它们计入),它的搜索、简报任务小节和关联只剩词法一层,每个答案都会说明原因(`semantic.state "off"` 及原因),
  完整计划的范围与完整运行的范围也会说 `off`。向量保留而不是丢弃 —— 重新打开时只对发生变化的内容重新嵌入。相似层未运行时,related 的应答会附上 `semantic`。
- 记忆的运行会询问其各层点名的代理,并汇成池(SPEC §memory.settings、ADR-037):工作代理和领队各自由第一个开口的层决定 —— 运行自身的请求(`POST /api/knowledge/sync` 上的 `agents`/`lead`,CLI 上的 `--agent`/`--lead`)、
  项目自身的设置(`GET|PATCH /api/knowledge/projects/{id}/settings`、`swiss memory project settings`)、同步块(`sync.agent` 现在是一个名字、一个列表或 "off"),以及仅对工作代理生效的最后添加的代理。
  多个工作代理组成一个池:每个问题交给自身并发下余量最大的成员,失败的问题换一个成员再问一次,运行记录每个成员的已问/失败计数(`runner.workers`)。拿到多个工作代理的领队会读到一份名册,
  并可以点名委派任务交给哪个工作代理(`delegate` 的 `agent` 字段)。同步概览应答 `workers`、`lead`、`chosen`(哪一层做的决定)和 `settings`,运行的范围携带自己的选择。
- 轻推发起的运行只询问有人点过名的代理(SPEC §memory.settings):Claude Code 会话之后的自动同步绝不回退到最新的代理 —— 轻推 API 应答 `{"queued": false, "reason": "no-agent"}` —— 领队也绝不回退。
- 对记忆初始化写下的笔记做一次复查(SPEC §memory.init The recheck):现在每个完整运行都以一次复查收尾,而 `swiss memory init --recheck`、同步面板上的 *复查笔记* 或带 `recheck: true` 的 `POST /api/knowledge/sync` 会单独运行一次。
  工作代理(没有工作代理时是同步代理)看到的是模型写下、此后无人评判的笔记,每次至多 20 条、每个问题一个领域,笔记点名的代码内联其中(每个问题 128 KiB,每个文件 48 KiB —— 更长的文件只显示其笔记点名的方法、字段和常量附近的行),
  每个问题一次请求、按工作代理的并发执行,并对照那段代码逐一评判:保留(验证通过)、修复(先走普通更新再验证)、并入已经说到它的那条笔记(superseded,附一条 `supersedes` 链接)或丢弃(标记为错误并附原因),相关的笔记会被链接。
  被代码检查扣留的笔记,只有引文经代码证实的修复才能释放它。人写的笔记或已确认的笔记绝不被触碰。`maxAsks` 为问题数设上限(200);试运行只计数;运行的范围和面板都会展示结果。
- 一次性确认整个记忆领域(SPEC §memory.domains):领域上的 *全部确认*、`swiss memory domain confirm PATH --all` 或带 `all` 的 `POST /api/knowledge/domains/{id}/confirm`。
  该领域、其下的每个领域以及它们未确认的活跃笔记一次确认完毕,于是 init 画出的项目只需每个领域点一下,而不是几千条笔记逐条来。需要评审的笔记保持未确认并被计数,等待裁决。
- 不必关掉插件就能停止一次记忆运行(SPEC §memory.sync Cancel):同步面板上的 *停止*、`swiss memory sync cancel ID` 或 `POST /api/knowledge/sync/runs/{id}/cancel`。
  排队中的运行不再启动;运行中的运行丢下手头的问题(拉起的运行器进程树一并丢弃)并以 `cancelled` 结束,保留它落下的地图和保存的笔记。下一次初始化会跳过任何已结束的完整运行回答过的分片,被取消或失败的也算,
  因此一个长运行停下再重启会从原处继续。运行期间范围会说出它的阶段(`review`、`areas`、`knowledge`、`wrapup`、`recheck`);`sync status` 连同至此的区域、分片、笔记和运行时长一起打印,面板也会用文字说明。
- 代理的思考 `effort` 和 `concurrency`(SPEC §llm.agents):内置或 chat 代理可设置 `low`、`medium`、`high` 或 `max`,每个代理还可设置一个消费方一次并行运行它的多少个任务(1..16;内置与 chat 为 4,claude 与 pi 为 1)—— 消费方即记忆的地图区域与初始化分片。代理页面可同时编辑两者。
- Anthropic 协议是 swiss 自己实现的(SPEC §llm.providers),与 OpenAI 那条一样:effort 以自适应思考的形式、经 `output_config.effort` 到达 Anthropic 兼容端点背后的 GLM、Kimi 或 DeepSeek(较老的 Claude 得到一个令牌预算),思考模型带签名的思考会在它的工具调用之前一并传回。
- 记忆完整运行的领队(SPEC §memory.init The lead):`sync.lead` 点名第二个 llm 代理,设置为更用力地思考,它回答地图的评审,并在分片读完后收束运行保存过笔记的每个领域 —— 刷新中枢的摘要与关键词,以及至多三条横跨该领域文件的笔记。
  内置领队会使用新的 `delegate` 工具(SPEC §llm.agents):每次调用至多 8 个任务,每个任务带自己的文件和领队选定的力度,由同步代理按自身并发作答。同步面板选择领队并说明它收束了什么;`sync status` 统计领域数。

- 进程历史存储(SPEC §process.apps、§host.history):托管应用的结果从 `logs/process-history.jsonl` 轮转迁到模块自有的存储接缝上,成为 `logs/process.db` —— mcp.db、remote.db、terminal.db 和 llm.db 乘坐的同一个席位,带同样的有界写队列与代际闸门。
  留存从 1 MiB 轮转改为行数上限(最新的 5,000 行,每 64 次接纳清扫一次),并且读取先过屏障,使刚记录的结果对紧随其后的读取立即可见,一如同步的 jsonl 追加从来那样。
  旧的 jsonl 树在首次打开时一次性导入(最旧在前,按自然键 `INSERT OR IGNORE`,在 `meta` 里盖戳,源文件原地保留、由操作者自行退役),被拒绝的历史写入仍只警告一次,绝不让它所记录的动作失败。
  进程模块以 10,000,000 字节的份额加入共享物理预算(约为其 ~1 MiB 稳态的十倍),声明的预算增长到 610,000,000 字节。

- LLM 插件(SPEC §llm):工具箱会请求的每一个模型,一次配置好,分立在三个页面上。**提供商**是命名的端点 —— OpenAI 的 chat 或 Anthropic 的 messages 协议、端点服务的模型(每个是聊天或嵌入模型,每种的第一个为默认)、密钥只能是 `${secret://name}`/`${ENV_VAR}` 引用、可选的代理 —— 附带 Ollama、GLM 和 OpenAI 预设,以及对着选定聊天模型一次提问的 Test。
  *发现* 读取端点自己的 `/models` 列表,把提供商缺少的模型摆出来供操作者确认,种类靠猜测。Anthropic 风格的提供商会同时在 `x-api-key` 和 `Authorization: Bearer` 上发送密钥(GLM 的兼容端点只读后者)。
  每个消费方都从提供商已列出的同种类模型中挑一个,绝不允许手敲 id:选了未列出的会被拒绝并报出已列出的那些,会让某个消费方悬空的提供商编辑也会被拒绝并点名该消费方。
  **嵌入**指定一个 OpenAI 风格的提供商和它的一个嵌入模型,连同所有消费方共享的宽度、批量、前缀和相似度下限;表单摆在生效块的旁边。**代理**说明谁来执行一个用工具的任务:`builtin`(swiss 自己的、跑在提供商上的工具循环,由 genai 承载)、`chat`(一次请求,无工具)、
  `claude`(拉起一个 Claude Code)或 `pi`(拉起一个 pi-coding-agent),各自带自己的限额、自己的系统提示词(没有时显示运行时的默认;追加在 CLI 自己的之后),以及它从自己运行时目录中拿取的工具 —— 在面板里勾选,只会收窄任务提供的工具,绝不放宽。
  一行可展开看到生效的提示词和工具;Test 会与一个向提供商发起请求的代理进行实时对话(即下面的 Test 对话),并检查拉起型代理的二进制。
- 代理遥测(SPEC §llm.telemetry):代理运行以 OTLP 日志上报,走 gRPC、HTTP/protobuf 或 HTTP/JSON,用 Claude Code 自己的事件词汇(`user_prompt`、`api_request`、`api_error`、`tool_decision`、`tool_result`,附 `session.id`、`prompt.id` 和 `swiss.agent`),
  于是为 Claude Code 而建的采集器 —— cc-otel —— 原样可读。`claude` 代理的子进程获得 OTEL 环境变量并自行上报;pi 没有钩子。导出器排队而不阻塞运行,重试 OTLP 认为可重试的失败,它的 Test 是一次不存储任何内容的空导出。
- 记忆插件的同步引擎(SPEC §memory.sync):提交和 Claude Code 会话蒸馏成记忆。`sync` 块点名一个 LLM 代理;它读取自同步头以来的遍历 —— 提交、补丁、转录摘要,全部脱敏 —— 并给出一个提案,服务像应用任何代理写入一样应用它,保持未确认。
  `builtin` 代理获得记忆的 `read`(钉在检出上)、`bash`(只读 git)和 `sync`(每次调用记录一个已检查的操作)工具;`read` 和 `bash` 拒绝凭据命名的文件,并像遍历一样脱敏它们的应答。
  `swiss memory sync` 排队并跟踪运行,`--nudge` 合并突发,`sync hooks install` 接上 Git 的 post-merge/pre-push;面板的同步面板显示代理、同步头、等待中的内容和运行,并编辑该块。
- `swiss memory init --map`(SPEC §memory.map):第一张按代码现状画出的项目地图,尺寸为小模型而设。程序从代码索引和清单(包、目录、横跨技术层的业务区域)读出区域,然后按区域向同步代理提出一个有界问题 —— 子区域先于父区域 ——
  答案填充固定字段(标题、摘要、概览、概念、入口、流程、代码、数据、依赖、关键词),在程序组装中枢前先对照索引检查。区域之间的关系是一个区域使用、另一个区域定义的名字,只在同一种语言内计数,跨包也只在清单声明依赖处计数;
  根 `.gitattributes` 标记为生成或 vendored 的文件不属于任何区域。`--lang` 以团队的语言书写中枢;`--plan` 离线展示区域,而当地图已经拥有代码时,它会说明不会画出新的东西(面板的预览同样);没有代理时,中枢就是索引的提纲。
  已在地图上的区域保留,从未同步过的项目从画好的地图开始同步。面板的同步面板上也有一模一样的 *从代码绘制地图*,Claude Code 钩子片段会在 `SessionEnd` 轻推同步。
- 地图按业务切分,而不是按最外层文件夹(SPEC §memory.map):勘察会越过源码包装目录(`src/main/java/com/acme/...`、`src`、`lib`、`app`)走到包路径,把业务名横跨 controller/service/repository/model 各层切出,并把 `sql`、脚本之类保留为业务领域旁边的技术区域。
  一个业务拥有它的层所在目录之下的文件(绝不用覆盖整个检出的 `**/Name*` 通配),`dal`、`remote` 和 `job` 也在层之列,层目录以它命名(`web/res/invoice`、`service/ecpay`);它的部分是它更长的词干(`invoice/line`),绝不是一个层的整个目录;
  缩写(`EcPay`、`Pm`)也是业务,至多 48 个连同另外 14 个区域一起进入评审。当将画出三个或更多新的顶层区域时,同步代理会在任何区域被描述之前对草稿评审一次 —— 分组、标题、合并(当合并后的区域拥有的条目超过一个领域的容量时改为嵌套)—— 它的错误由程序丢弃;评审失败则保留草稿。
  `--redraw` 删除早前地图运行画出、此后无人或后续写入触碰过的领域(最深优先,笔记保留并重新归档)并重新绘制。
- `swiss memory init --full`(SPEC §memory.init):一个运行初始化整个项目的知识 —— 先地图运行,然后每个代码和文档文件按现状读取,按领域分片(每片 48 KiB、12 个文件,长文件分段整体读),同步代理按片回答一个有界问题,问出代码教的陷阱、约束、决策和知识。
  每条笔记走普通的已检查保存,锚定在读过的文件里,引文对照代码检查(代码里没有的引文留待评审)。之后的运行跳过文本未变的分片;`--max-asks` 为问题数设上限(默认 200)。
  LLM 插件上有嵌入模型时,笔记像任何笔记一样被索引,运行说出索引的状态;没有则说明笔记按字词找到。`--plan` 不问任何模型就展示分片和索引;`--wait` 跟踪运行、然后跟踪索引。
  面板的同步面板(现在是 *从代码绘制地图并初始化*)有 *重新绘制*、*预览初始化*、*初始化记忆* 和 *最大问题数*,显示完整运行的笔记和索引,并每 3 秒跟踪一次排队中或运行中的运行。一个真实 e2e(`e2e/memory-init.test.mjs`)对着 `.env` 点名的模型运行它,没有则跳过。
- 在途中失败的记忆问题(网络、请求超时、429、5xx)会在 2 秒后、代理的期限之内重发一次:曾有一条连接断开时,其上在途的四个问题一起死掉,留下未描述的地图区域。拒绝绝不重发,流式提问也不会。
- 笔记和领域的搜索关键词(SPEC §memory.model):至多 16 个词条,英文与团队语言并排,在关键词搜索中于标题与内容之间加权,CJK 层和简报按标题计数,因此任一语言的查询在没有嵌入模型时也能找到笔记。
  CLI 上的 `--keywords`、MCP 工具上的 `keywords`、API 与同步提案,以及面板笔记和领域表单里的关键词字段。存储迁移到 schema v7。同步会看到被触碰中枢的关键词,并在变更带来可用于搜索的新词条时扩展它们。
- 回答为空、或停在 `maxTokens`(思考模型把预算花在了思考上)的 `chat` 或 `builtin` 同步代理,会被再问一次并告知原因;仍然失败的运行会点名 `maxTokens`。
- 记忆的语义搜索从 LLM 插件的嵌入页面取模型;齿轮面板显示生效的模型并链到那里。
- Memory 和 LLM 页面的名字进入了面板的线上标签(此前显示的是下发的原始文本)。
- UI 库新增 `editList`/`editLine`(表单就地编辑的列表)、`checkGrid`(有界滚动框里的多个勾选)和 `split`(表单旁边放生效块),各自都进了画廊。
- 记忆插件(SPEC §memory):一张本地的、跨代理的、项目范围的笔记图 —— 陷阱、约束、决策、偏好、持久事实 —— 带修订、出处和锚点,归档在项目地图的领域之下;
  确定性的搜索分层(精确、符号、单词和 CJK 频段)在设置了嵌入模型后与语义融合。每个入口背后是同一个引擎和同一套规则:离线的 `swiss memory`、面向 MCP 客户端的 `/mcp/memory`、`/api/knowledge` 和 Memory 页面。
- 模块自有的历史(SPEC §host.history、ADR-033/034):MCP 的调用和流量迁入模块自己的 SQLite 数据库 `logs/mcp.db` —— 封存的正文分块、没有行数上限的归档(环形缓冲仍是活动窗口)、
  带诚实游标的字面深度搜索(预算耗尽是一个带继续入口的 INCOMPLETE 页面,绝不说 "no matches"),以及每个模块的物理配额。旧的 jsonl 树和正文文件在首次打开时幂等导入并原地保留。
- 终端录制加入模块历史存储(SPEC §terminal.recording、ADR-035):会话及其封存的 asciicast 事件行住进 `logs/terminal.db`,`.cast` 变成由路由从存储的行逐字节如实重建的导出 —— 首次打开幂等导入每个旧 `.cast`(自然键 = 会话 id)并把文件原地保留。
  100 MB 的份额和 90 天的清扫取代了没有任何留存策略的文件夹。
- 侧栏的默认席位(SPEC §panel.nav):每个被服务的组默认都在列,至多十五个席位(`RAIL_LIMIT`)—— 新模块一开始服务就上了侧栏,而不是在调色板里等一次固定。默认只在还没有存储选择时生效:已经固定过席位的操作者,其列表保持原样。
- `llm.history`(SPEC §llm.history):每一次提供商往返 —— 每个聊天请求与回复(思考在内),以及每次嵌入调用连同其完整输入文本和端点应答的向量(每个分量保留六位小数),使一次往返能从记录复现 —— 还有每场代理对话,一轮一轮,记录在模块自己的 `logs/llm.db` 里。
  嵌入可能泄露它嵌入的文本,因此记录只对会话之后的管理员开放,超出每行 2 KiB 内联预览的部分封存,受份额配额约束,180 天清扫,绝不导出。只读的历史页面把它读回来:*调用* 最新在前,封存正文只差一行;*会话* 每轮完整文本一次展开即得,并标注对话是开放还是已关闭。
- Test 对话(SPEC §llm.api、§llm.history):`builtin` 或 `chat` 代理的 Test 是一场真实的、持久的、流式的对话。增量以服务器发送事件到达(`POST agents/test/stream`),落地即绘制 —— 推理变暗、工具请求渲染成小块、收尾答案渲染成 markdown。
  每条提示作为运行的下一个用户轮归档,已归档的轮绝不重写;面板会续上代理的开放对话,*停止* 取消流(`POST agents/test/cancel`)并保留已归档内容,*新建* 先关闭运行(`POST history/runs/{id}/close`,不删除任何东西)再重新开始。
  续写在任何提供商请求之前就被一个 409 拒绝并点名原因(`open-run-exists`、`gone`、`closed`、`context-changed`、`unresolved-tail`、`over-budget`),绝不悄悄分叉。DeepSeek 的官方思考模式在重放中拿回之前每一轮的 `reasoning_content`。
  OpenAI 流由 `sse-stream` 在字节与帧上限之后解析,每个拒绝 —— 聊天、嵌入、遥测 —— 都清除了密钥和代理的凭据,包括被消息截断切成两半的密钥。
- 远程运行搜索(SPEC §remote.panel):搜索框领在页面头部,始终显示,搜索的是记录在案的运行打印过的内容(不是命令 —— `sh -s` 脚本是封存的 stdin)。输入在停顿后搜索,回车立即搜索,Escape 找回列表。
  结果替换列表,最新在前,每行带着第一个匹配所在的行并标记匹配;扫描按有界轮次从最旧开始走记录,*继续搜索* 让它继续。尚未完成且一无所获的扫描会说明记录较新的部分还没搜到,绝不说 "no matches"。

- **托管应用与进程页面**(SPEC §process):`swiss-process`(配置、状态、控制、日志捕获、历史)、带自己面板页面(应用表、按所有者的子进程、日志跟随面板、保存即整个应用 PUT 的添加/编辑表单)的进程插件,以及 `swiss process` —— `status [--json]`(仅当每个自启动应用都在运行时退出码 0)、
  `start|stop|restart <name> [--force]`(轮询到结果,网关不可达或插件关闭时退出码 2)、`logs <name> [--app] [-n N] [-f]`(带 after 游标跟随)。每个动作都会在历史里点名它的执行者(`cli` 或 `panel`);插件的配置 PUT 会以"可存储但不可运行"的警告作答。

- 提供商的模型会写明它的显示名称、上下文窗口、最大输出令牌数、是否读取图像以及是否接受思考力度(SPEC §llm.providers, §llm surfaces Providers, §data.agent)。

  在此变更之前,提供商表单里的模型只是一个 id 加一个种类,再无其他。

  现在:
  - 每个模型行都有 *详情*:*显示名称*、*上下文窗口* 和 *最大输出令牌数*(可敲成 `1M`、`256K` 或 `131072`)、*输入类型*(文本、图像)和 *思考*。id 下方一行安静的小字说明模型写明的内容。
  - 每个预设都按其厂商的说明填写:DeepSeek V4.1 Flash 读 1M 上下文、384K 输出、图像输入;GPT-6 和 Claude 5.5 的输出封顶 128K;Qwen3.8 Flash 和 MiniMax M3 不接受力度。
  - *填充已知详情* 会填上以前保存过的提供商的行,Discover 带回的条目遇到已知 id 也会带上它们。
  - 代理的回答至多请求模型的最大输出:默认的 131 072 曾超过 GPT-6 和 Claude 5.5 的 128K。不接受思考力度的模型不会再被发送力度。
  - Data 代理的窗格以显示名称称呼模型,为不接受力度的模型显示无力度,当模型的窗口小于代理的预算时,上下文环按模型的窗口读数。

### 变更

- 哪些 AI 技能落地,现在由你选择(SPEC §host.cli)。此变更之前,`swiss skill
  install` 把全部三个技能写入 `.agents`、`.claude` 和 `.cursor`,而面板读取的是
  `.claude`、`.codex` 和 `.agents`。

  现在:
  - `swiss skill install` 在终端上逐个技能询问,默认为否。没有终端时它以退出码 2 拒绝并列出它们,所以**运行裸命令的脚本现在会失败**:
    脚本要么点名技能,要么传 `--all`。
  - 技能为 Claude Code 写入 `.claude`(设置了 `$CLAUDE_CONFIG_DIR` 时是
    `$CLAUDE_CONFIG_DIR/skills`),为其他每个客户端写入 `.agents`。旧构建留在
    `.cursor` 或 `.codex` 里的副本保持原样,不会刷新,可以手动删除。
  - `swiss skill list` 显示每个技能做什么、是否自行加载,以及每份副本是否仍与
    这个二进制匹配。替换可执行文件后,`swiss skill install --installed` 刷新你
    已有的副本,一个也不新增。它也会替换你手动编辑过的 `SKILL.md`。
- MCP 定义在添加、编辑、替换和连接测试时按名字拒绝该类型不接受的键(SPEC
  §mcp.defs)。此变更之前,这样的键被悄悄丢弃,拼错了也不起任何作用。proc MCP 的
  Call 超时字段现在生效,`idleMs` 在每种类型上都有效。
- 回读把 `<scheme> ${reference}`(例如 `Bearer ${secret://x}`)按原样保留,因为它
  不携带任何凭据(SPEC §host.refs)。引用之外的其他内容保持脱敏。
- 网关日志改走 stderr。现在,任何在 stdout 上读取 `swiss serve` 的东西都看不到
  日志行了。
- LLM 页面的 *Add provider* 为一次安装会用到的那批厂商提供预设(SPEC
  §llm surfaces Providers)。此变更之前,它只提供 Ollama、GLM 和 OpenAI(带
  `gpt-4.1-mini`)。现在它还提供 Anthropic、Google Gemini、DeepSeek、Kimi、
  Qwen(Alibaba Cloud Model Studio)、MiniMax、Doubao(Volcengine Ark)和
  OpenRouter。每个预设填好协议、base URL、一个 `${secret://<preset>}` 密钥引用
  和该厂商当前的模型,日常款排在最前: DeepSeek V4.1 Flash(`deepseek-flash`)和
  V4 Pro、Kimi K3.1 Pro 和它的高速孪生款、GLM-5.3 Flash 和 GLM-5.3、Qwen3.8
  Flash 和 Max、MiniMax M3.1 Flash 和 M3、Doubao Seed 2.1、OpenAI 的 GPT-6
  系列、Claude 5.5,以及 Gemini 3.8 Flash 和 4 Argon。中国厂商的预设使用其
  中国大陆主机。
- 提供商的名字可以带大写字母(`GLM`、`DeepSeek`、`OpenRouter`)(SPEC
  §llm.config)。此变更之前,名字只能由小写字母、数字和连字符组成。代理名字
  保持小写。只有大小写不同的两个提供商名字(`glm` 和 `GLM`)会被拒绝。预设
  现在按厂商书写自己名字的方式为每个提供商命名。
- 内置 `list` 工具没有了:`glob` 按模式找文件,保存的 `list` 会读作 `glob`(SPEC
  §llm.agents)。
- 一轮思考在历史页和 Agents 的 Test 流中显示为 dsh 的 Think 行 —— 脑图标、
  Think 和思考内容的第一行 ——(SPEC §llm surfaces)。
- 代理的轨迹和记忆同步的运行显示为一种流,而不是一堵文字墙(SPEC
  §panel.ui, §llm.history, §memory.sync)。

  此变更之前:
  - LLM 历史的运行弹窗使用事件列表,每一步都是一行密集的日志;
  - 记忆同步弹窗把每次运行的计数、摘要和失败放进一张卡片,每个完成的运行下面
    有一个 *Agent runs* 按钮,等待中的会话显示为原始字节数
    (*348772267 unread conversation byte(s)*)。

  现在:
  - 一个新的库形状,流(`ui/flow.ts`),为每一步画一条安静的行: 一个符号或状态
    圆点、名称、灰色的一行摘要、右侧的时长,以及步骤之间用留白代替分隔线。
  - 运行弹窗的头部是三行事实。一轮占一行,模型的文字作为正文跟在其下。一次
    工具调用展开为它的参数和结果,放在同一个代码面上,内容超过 280 px 即滚动。
    进行中的那一步泛着微光。
  - 一次同步运行是一行: 它的状态圆点、让它排队的来源、它的阶段或标记、它的
    历时,判定写在其下。它的计数和代理运行就地展开。运行中的运行自动展开;
    完成的运行在第一次被打开时才读取它的代理运行。它的代理运行是同一种流的
    行,点开一行即打开那次运行的轨迹。
  - 等待中的会话显示为一个大小(*332.6 MiB of conversations unread*)。

- 面对积压的一次记忆同步会把它能发的提交都发出去,补丁最先让位(SPEC
  §memory.sync): 过去提示词一超过预算,一次运行就只发一半的提交,于是一个
  大型项目提示词的固定部分(它那 159 个领域的地图、被触及的 hub 和笔记)每次
  运行只留下两个提交的余地,68 个等待中的提交要跑大约 34 次运行、每次约
  14 分钟;现在补丁会重新塑形,填进提示词其余部分留下的空间,只有仍然超出的
  提示词才少发提交。预算是代理请求上限的十六分之五(按 1 MiB 默认为 320 KiB,
  过去至多 160 KiB),补丁把代码排在文档和数据之前。

- 内置或聊天代理的回答可以写到 128K 令牌,除非它自己说得更少(SPEC
  §llm.agents): `maxTokens` 过去默认 8192、上限 32 768,思考模型在思考中途被
  截断,记忆同步的循环结束时一条笔记也没归档;两者现在都是 131 072,这是 GLM
  接受的最大值(它拒绝更大的 `max_tokens`),LLM 页面的代理表单最高可填到此值。

- 内置或聊天代理的请求预算独立且更大(SPEC §llm.agents,
  §memory.sync): 每个请求和每个续接会话过去停下的那 512 KiB,现在是代理的
  `maxContextKiB`(64 到 8192,未设置时为 1024),在 LLM 页面的代理表单上以
  Request budget (KiB) 设置,并显示在代理的设置里。超过它的循环轮次仍会在
  起飞前被拒,现在会指名是哪个预算,记录已越过它的会话不再接受后续发送。
  一次记忆同步运行保留它 160 KiB 的提示词,更大预算的其余部分留给读取和
  思考;预算低于 512 KiB 时,提示词取其中的十六分之五。

- 记忆页面在大型项目上更快(SPEC §memory.store, §memory.freshness,
  §memory.related, §memory.panel)。一个项目的笔记和链接 —— 审查收件箱、列表、
  地图、简报、关系图和关联读取的都是完整内容 —— 现在在读取之间保留,直到有
  任何东西提交到存储(在缓存自有的一条连接上查 `PRAGMA data_version`,所以
  CLI 或代理的写入也会被看见),十分钟无读取后释放。一个文件的哈希在它的
  大小和修改时间未变时复用。关联只为它保留的笔记构建理由,不再为每个候选
  构建。一条笔记在它的相关笔记被找到之前就能打开。在一个 1,962 条笔记项目
  的副本上(release 构建),审查收件箱在首次读取后从 444 ms 降到 65 ms,关联
  从 137 ms 到 52 ms,关系图从 65 ms 到 1 ms 以内,一个 200 条笔记的列表和
  地图降到几毫秒。
- 插件包装成了自己的 crate,`swiss-plugins`(SPEC §arch.crates): 把 terminal、
  remote、process、memory 和 llm 放上宿主的那些描述符、路由和适配器,原本是
  根 crate 的 `src/plugins/` —— 38k 行里的 20k —— 现在构建为一个只有根 crate
  依赖的 crate,于是对根 crate 自身文件(CLI、admin API、服务器引导)的编辑
  不再重新编译它们。无行为变更。
- 没人使用数据库 MCP 时,它不持有任何连接(SPEC §mcp.registry, §mcp.adapters)。
  15 秒一次的健康探测只在有人读取健康状态(MCP 列表、某个 MCP 的页面、Data
  页面的连接列表)时运行,并在最后一次读取 30 秒后停驻,于是启动不再为了
  ping 而建起每个数据库连接池;间歇后的第一次 MCP 列表读取会等它自己启动的
  那一轮,至多一秒,而不是把每个数据库显示为 unknown。十分钟未使用的连接会
  被关闭(mysql 和 pg 优雅关闭),下一次调用打开一条新的。空闲回收器朝它最早
  的截止时间睡眠,而不是每秒扫一遍。在 19998 上对一份真实状态快照做 A/B
  测量,空闲 6.7 分钟、无任何读取(SPEC §product.memory): 常驻数据库连接 0
  对 5,网关在整个窗口的 CPU 31 ms 对 171 ms,工作集 27.5 MB 对 29.1,私有
  内存不变;两分钟的 MCP 列表读取让五条连接回来,最后一次读取十分钟后五条
  全部再次关闭。
- 作业调度器朝它最早的下一个到期项睡眠,而不是在进程的整个生命周期里每秒
  醒来一次(SPEC §jobs.triggers): 一旦有项到期,至少睡 1 秒,扫描因此不会
  空转,启动仍能在一秒内发现已逾期的作业;至多 60 秒,所以时钟跳变或从睡眠
  中恢复最迟一分钟内被捕捉;没有定义任何作业时停驻。应用一份配置会唤醒它。
- 面板以 gzip 压缩发布,不再每个请求复制一次(SPEC §panel): release 构建把
  `admin_assets/` 以 gzip 嵌入(6.96 MB 的文件压到 2.00 MB;二进制从 23.49 MiB
  到 18.78 MiB),把存下的字节按 `Content-Encoding: gzip` 发送,只为拒绝 gzip
  的客户端解压。每个响应体都从二进制里借用,而过去每个请求要把它的文件复制
  进一个新字符串(mermaid 一次加载 2.8 MB)。资源改为带 ETag 的 `no-cache`,
  不再是 `no-store`,于是重新加载仍会重新校验每个模块,但每个未变的模块得到
  一个无响应体的 304;外壳仍是 `no-store`。面板版本戳改为哈希每个文件的
  SHA-256 而不是其长度,所以等长的编辑也会让它翻转。在一个临时主目录上的
  A/B 测量(SPEC §product.memory): 空闲不变,整个面板加载三次后工作集
  21.99 MB 对 26.83(峰值 21.99 对 29.57),私有内存 7.93 对 8.61 MB,一次完整
  加载上线 1.90 MB 对 6.63。
- 任务简报只显示有帮助的(SPEC §memory.brief, §memory.evaluation): 第一个
  之后只与任务共享一两个词的命中,是 *Also matching your task* 下面的一行,
  不追踪它的链接,强的读出会略去它;*In <path>* 统计在用领域的笔记数,而不是
  每一轮都列出来。一次经评判的运行发现,简报显示为有帮助的 8-10 条笔记里
  只有约四分之一确有帮助;简报现在显示 4-5 条笔记,两套夹具上完全正确的占比
  从 0.17/0.36 升到 0.44/0.52,召回不变。
- 记忆的搜索把领域 hub 链接或点名的笔记排得略高(SPEC
  §memory.search): 平局不再落到随机 id,加成保持在词语档位之内,绝不会把
  一条排到精确命中之上。
- 一个词命中必须占到查询词权重的五分之一,搜索和简报的任务命中都如此(SPEC
  §memory.search, §memory.brief): 五条的上下文不再被每条只共享一个词的其他
  领域笔记填满。查询里的路径只计入为它部件命名的词(`…/store.rs` 只算
  `store`,不算 `crates`、`src` 和 `rs`),简报的链接笔记跟随主题而不是偶然的
  词命中,被已确认笔记反驳的未确认规则会离开简报的 Rules。搜索的错误领域
  上下文从 0.55/0.44 降到 0.43/0.29,召回不变。
- 链接工作树的根是主检出(SPEC §memory.scope): 它的第一次写入以仓库的名字
  命名项目,并注册那个比分支活得更久的检出,从子目录提问会在那个根上读取,
  存储里曾指向某个工作树的根会在下次写入时自愈。裸仓库的工作树和子模块保持
  各自为根。
- `swiss skill install` 也会写入 `$CLAUDE_CONFIG_DIR/skills`(SPEC §host.cli):
  设置了它时,Claude Code 从那里读取用户技能,所以拥有自己配置目录的账号过去
  得不到 swiss 技能。一个恰好是 `~/.claude` 的配置目录只写入一次。
- 一个记忆问题会把记忆对照代码检验(SPEC §memory.rag): 回答它的内置代理还会
  带着复核的 `read` 和 `bash` 走查范围的检出 —— 钉在那里、凭据文件被拒、脱敏、
  git 只读 —— 它的提示词要求它在陈述每个具体细节(一条路由及其类、一个文件
  或脚本路径、一个计数、一次重试)之前先核对,只有在表明文件此刻确实在那里
  之后才引用它,并在一条笔记或一份文档与代码不一致时报出代码,把那一份点名
  为过时。此前,一个回答可能重复过时的文档: 代码里从不存在的重试计数、一个
  已删除的 DDL 脚本、把一条 v4 路由归到 v3 控制器名下。没有检出的范围仍和
  从前一样只凭记忆回答。记忆自相矛盾时(一条笔记说某步从不重试,一份文档的
  表格说三次),由代码裁决,在执行该步骤的方法里读代码。
- 记忆代理的 `bash` 像 shell 那样拆分命令(SPEC §memory.sync): 带引号的模式
  去掉引号后到达 git,未加引号的管道、`;` 或重定向会被明确拒绝,未命中会说
  "nothing matched"。此前,词按空格拆分,所以 `git grep -n "open/real"` 连引号
  也一起搜索,空手而回,代理把这读成了未命中。
- 脱敏(`secrets::redact`, SPEC §memory.sync)现在也接受短密码: 跟在
  `password`、`passwd`、`pwd` 或 `密码` 以及 `:`、`=` 或 `：` 之后的值,当它
  是 6 到 64 个可打印字节、含一个数字或一个符号,并且读起来既不是代码、路径
  也不是占位符(示例响应里的 `"Password": "…"`,deeplink 里的
  `&password=…`,`${DB_PASSWORD:…}` 的默认值),以及 URL `user:password@` 里
  的密码。此前只取 12 个及以上字母数字的值,所以设计文档里的示例账号密码
  到达了为它建卡片的模型。两条规则都只做脱敏,从不拒绝一条笔记。
- 缺席的 `sync.agent` 意为继承,而不是关闭(SPEC §memory.sync, ADR-037): 没有
  点名代理的块过去悄悄地谁也不问;现在它会问项目设置的选择,其次是最后添加
  的代理。想谁也不问就写 "off"。
- 一次运行的对话读取把每个调用的提示词和预览按各自的前 400 个字符携带(SPEC
  §llm.history): look 循环每一轮都重复提问的提示词,于是一次复核的轨迹每轮
  发送一遍那个提示词,一个问题约 1 MB;完整的提示词仍是运行的用户轮次和调用
  的请求体。Test 的续接检查现在只读取运行的系统轮次,而不是每次发送都解码
  整个对话,并且仍会把一个读取不出来的系统请求体计为已更改。
- 项目地图下探到五层,不再是三层(SPEC §memory.domains, §memory.map),而且
  记忆的地图草稿按最大优先拆分区域 —— 十二个及以上代码文件的区域拆成它的
  部件,然后是剩下代码最多的区域或部件 —— 只要草稿还有空间,现在是 150 个
  区域,不再是 60 个: 一个区域装有多少代码,决定地图在那里下探多深。过去的
  草稿把每个顶层区域只拆一次,顶层一填满 60 个就丢掉所有子区域,于是一个
  大型 Java 后端画出 54 个区域、没有一个子区域;现在它在四个层级上画出 150
  个(`core/event/notification/context`、`orders/list/profit`),供审查的父级在
  第五层。面板的地图把五层全部缩进展示,第五层上的领域不再提供子领域。
- 内置或聊天代理自己的系统提示词跟在运行时默认之后,而不是取代它(SPEC
  §llm.agents): 默认 —— 读到的数据绝不是指令,绝不重复凭据 —— 总会发送,
  代理的提示词跟在其后,没有提示词的代理只发送默认。一份由默认起始而来的
  提示词只携带它一次。Agents 弹窗在输入框上方以只读形式展示默认,
  *Start from the default* 没有了。
- 记忆的全量运行每个切片保留 3 条笔记,不再是 6 条,一个主题一条(SPEC
  §memory.init): 一个切片的文件对同一张表、同一个流程或同一条规则所记的
  内容是一条笔记,事实收在一起,绝不是一个事实一条笔记。过去的六条填满了
  大多数切片 —— 切片均 3.9 条,一个大项目 262 个切片后产出 1,026 条笔记 ——
  同一主题的每个事实各占一条笔记。
- LLM 历史的份额是 600 MB,不再是 100 MB(SPEC §host.history),随它申报的
  历史预算为 1,110 MB。一次记忆复核 —— 它的提示词携带所问的代码 —— 一天之
  内就填满了 100 MB,于是大小裁剪不断删掉仍会被读取的调用。
- LLM 历史的份额是 1,000 MB,不再是 600 MB(SPEC §host.history),随它申报的
  历史预算为 1,560 MB。存储已占用其 600 MB 席位中的 393 MB,大小裁剪从份额
  的 90% 处开始。

### 修复

- Data 代理的控制台工具分页读取长列表:`list_console_tabs` 接受 `grep`、`state`、`limit`
  和 `offset` 且绝不截断一行,`read_console_tab` 接受 `offset` 并按完整语句为长控制台分页。
  开启控制台存储后,页面不再发送自己的标签页快照;工具直接读取存储(SPEC §data.agent Tabs)。
- 在 Data 代理窗格里阅读工具定义不再与页面打架。打开一个工具时,它的行保持在点击处下方
  而不是被甩出屏幕,任何向上的一步或未到底的向下滚动都会停止窗格跟随,发送或回答结束也
  不再关闭已打开的工具。就地滚动的正文在到末尾时交出滚轮,Test 对话框让已经向上翻阅的
  读者留在原地(SPEC §panel.ui)。
- 关闭一个空的 Data 控制台不再把它归入 Recently closed,无论其名称或所有者是什么,因此
  打开一个再关闭不会留下任何东西。列表和存储里已有的空白行会在下一次访问该连接时离开
  (SPEC §data.tabs)。
- Data 代理打开或操作的控制台不再抢走插入符。它来到前台,而读者继续在提问框里打字;以前
  每一帧都会把读者接下来打的字移进控制台(SPEC §data.agent)。
- 在 Tunnels 页面编辑 SSH 连接会保留其存储的密码与密钥口令。页面读取的行以前把它们漏掉
  了,于是保存表单会将其抹掉(SPEC §tunnels.store)。
- 在 Linux 和 macOS 上,由 `swiss start` 启动的网关拥有自己的会话,因此比启动它的终端
  或 Agent shell 存活得更久。它以前共享那个会话并随之消亡(SPEC §host.daemon)。
- 没有端口的管理应用在网关重启后仍被记住。以前每次启动都会在读文件之前保存一条空的跟踪
  记录,导致运行中的应用被读作 `unknown`、`swiss process stop` 无法停止它,而随 swiss
  启动的应用每次重启都会多出一个运行中的副本(SPEC §process.apps)。
- 在 Linux 上,登录时启动让网关保持运行。systemd 单元以前把 `swiss start` 的退出当作
  服务的结束,于是管理器在网关刚起来后就将其杀掉,没有重启也没有报错。该单元现在像
  Windows 和 macOS 一样只运行一次 `swiss start`,`systemctl --user stop swiss` 会执行
  `swiss stop` 并让管理应用继续运行。崩溃的网关保持停机,直到有人启动它。
  `swiss autostart on` 会重写已安装的单元(SPEC §host.cli)。
- `echo` 定义接受 `description`,与其他所有类型一致。全新 home 预置的那条以前会被
  `swiss setup show` 丢掉,于是该清单被规划为一次更新,应用它便删掉了那段文字
  (SPEC §mcp.defs)。
- Claude 设置列出 MCP 时不再带有不受支持的 scope 选项。Codex 设置与从面板复制的片段改用
  `http_headers`,让客户端发送网关 bearer(SPEC §host.setup)。
- 引导尚未到达的插件会说自己尚未启动,而不是说被 `gateway.config.json` 禁用了 —— 一条
  没有人写过的行(SPEC §host.plugins)。
- `mcp`、`tunnels`、`data` 和 `terminal` 在保存时拒绝它们不读取的配置键,而不是
  存下来再无视。旧构建保存过的行仍能启动(SPEC §host.config)。
- `swiss export > bundle.json` 写出单个 JSON 文档(SPEC §host.cli)。以前日志行会落进
  它,于是导出以 0 退出,导入随后失败。现在读不了的文件会让导出以退出码 1 停止,stdout
  上什么也不留。
- 无法读取的状态文件不再被当作空文件覆盖写入(SPEC §core.sealed)。在此变更之前,损坏
  或外来的 `env.json` 会在启动时被替换成新的令牌,导出和导入也可能覆盖它。现在,缺失
  意味着首次运行,任何其他失败都算错误并保留文件。**`env.json` 无法读取的网关拒绝启动
  并记录原因**:恢复主密钥,或把文件移开。
- `swiss import` 先检查每个部分与目标,然后把文件作为一次变更落地。
- `swiss skill install` 不再删除你保留在技能旁边的内容,失败的替换保留旧副本
  (SPEC §host.cli)。每次写入只替换 `SKILL.md` 本身。扫描自己 skills 目录的客户端不再
  能让 Windows 上的安装失败:重试现在覆盖它实际遇到的共享冲突。
- 面板 e2e 套件在真实浏览器中走查后续工作添加的三个视图。Data 走查注册一个指向无人应答
  端口的惰性 mysql 定义 —— 分析台账的 list 与 create 从不触碰数据库(analysis_api.rs)
  —— 然后走查头部的 Analyses 按钮、可见打开的弹层、诚实的空状态,以及 New analysis 把
  第一个任务种成连接自己的 SQL 标签页。Mcps 走查在最后一次完整加载之后打桩桌面窗口的两
  个全局变量(desktop.ts 的确切契约),并走查桌面首帧:总览与内置 echo 一起绘制出来,
  其 Open 按钮真正把窗格带到 echo 的详情。Settings 走查断言 System 页面的捆绑技能连同其
  按目标的安装动作一起渲染;安装按钮本身没有被点击 —— 它们会写进操作者真实的 profile
  根目录,位于隔离的 e2e home 之外。
- 后续工作添加的十个工具在 Data 代理的折叠区与 Agents 页面的目录中重新得到翻译。分析
  工具、四个按 id 的控制台标签工具、stop_listen 以及 Test 的 write/edit/glob/grep 从未
  加入面板的 TOOL_SUMMARIES 表,于是它们的条目落进目录的英文文案,而所有旧工具都显示为
  已翻译;run_sql 和 open_console 还保留着自动写入与标签 id 之前的措辞。一个棘轮测试
  现在直接从 agent.rs 读取 swiss-llm 的 BUILTIN_TOOLS 和 DB_TOOLS,当被服务的工具缺少
  映射条目、已退役的工具滞留在映射里,或任一 locale 缺少某个映射键时让构建失败
  (SPEC §panel.i18n)。
- 包含 `&` 的 URL 在 Windows 上完整地在浏览器中打开。打开器以前运行 `cmd /c start`,
  而 cmd 会重新解析它的命令行:OAuth 授权 URL 在第一个 `&` 处被切开,浏览器只看到第一
  个查询对,其后的内容都被当作命令执行 —— 而从 `swiss gui` 来的这些 URL 出自模型文本
  和数据库单元格。打开器现在改为 `rundll32 url.dll,FileProtocolHandler`:同样的默认
  处理器,但 URL 作为一个完整的 CreateProcess 参数传递,中间没有 shell。
- 桌面端的 Back 不再回到启动时的空白页。桌面上下文菜单使用面板本地化的外观与编辑动作,
  同时保留页面自有的菜单。

- Data 聊天在底部时跟随流式回复,提供跳到最新消息的入口,并在完成或停止后折叠工作详情。
  嵌套的调用详情在关闭时回到其会话历史弹层。
- 桌面端导航保持在选定的网关上;登录 URL 从票据在本地构造。导出的完成与失败由原生下载
  处理器报告。
- Skill API 测试除网关状态外还隔离客户端 home 目录。登录票据与 cookie 过期的边界有了
  确定性的回归覆盖。

- 请求新窗口的链接现在能从 `swiss gui` 到达浏览器(SPEC §host.gui)。在此变更之前,
  每次 `target="_blank"` 点击都被无声丢弃:webview 没有新窗口处理器,而两个引擎在没有
  设置时都会拒绝该请求。面板打开的 MCP 授权页被告知 "opened",随后在五分钟后超时;回答
  里的链接、URL 单元格、Process 页面的 *Open UI* 和记忆引用则毫无反应。只有 `http` 和
  `https` 会被交出 —— 面板渲染模型输出和数据库单元格,`file:` 和 `javascript:`
  正是打开器绝不能收到的载荷。`swiss open` 现在在 macOS 上也能用了,它以前在那里调用
  `xdg-open`。
- 窗口随它所展示的网关一同离去(SPEC §host.gui)。面板的 Quit 以前会停止网关而让外壳
  留在原地:其页面再也加载不出来,又因为外壳仍持有单实例声明,下一次 `swiss gui` 只能
  唤起那个死窗口 —— 托盘的 Exit 是唯一的出路。外壳现在随网关一起关闭,而一次启动若发现
  某窗口所属的网关无人应答,会等它离开并接管其身份。
- `swiss start` 认得它自己的网关,而不只是一个 200(SPEC §host.daemon)。健康探测以前
  只读状态码,于是端口上的开发服务器或任何其他应用都会答 "already running",`stop`
  拒绝行动,面板链接则为一个并非面板的页面铸造。`/health` 点名进程
  (`{"ok":true,"build":…}`);探测现在会读它。
- 竞争中的 `swiss start` 不再抹掉胜者刚写下的 pid 文件(SPEC §host.daemon)。同一端口
  上的两次启动以前都写记录;败者的子进程死于 EADDRINUSE,随后败者删掉了那个文件 —— 那
  时它已属于胜者 —— 于是 `swiss stop` 对着一个活着的网关回答 "not running",
  `restart` 则是空操作。点名一个现存进程的记录不属于任何别人去替换或删除;真正服务该
  端口的启动写自己的记录。
- `swiss gui` 不再把临时的 `SWISS_PORT` 变成永久端口(SPEC §host.gui)。外壳以前解析
  端口并交给启动,而被交接的端口会持久化进 `gateway.config.json`:在该变量下的一次启动
  就把这个端口钉给了之后每次启动。外壳现在一个也不传,启动则完全按外壳原先的方式解析。
- 控制台在桌面窗口中保持自己的字号(SPEC §panel.design)。窗口的缩放监听器在
  `document` 上以捕获阶段运行,在 xterm 的 textarea 看到之前就抢走了 Ctrl+= / Ctrl+-
  / Ctrl+0,于是终端页面的缩放在应用里失效,而在浏览器标签页里正常。在控制台内输入的
  按键留给它处理;在 Mac 上由窗口接管 Cmd —— 那是平台的修饰键,而且没有菜单项可以兜底。
- 技能的状态与安装读取 `$CLAUDE_CONFIG_DIR`(SPEC §host.skills)。批量安装尊重该变量;
  按技能的路径与面板的行没有,于是在设置了它的机器上,行会对 `~/.claude` 显示
  "installed" —— 而 Claude Code 在那里什么也不读 —— *Install* 也写到那里。
- Data 控制台的头部在悬停时说明控制台依据什么运行(SPEC §data.tabs):这条规则曾是每个
  编辑器上方的一段两行文字,现在它是标题旁的一个帮助标记,文字放在其提示里。*Run* 在
  头部如今仅有的这一行上承担主操作的分量。
- DB 代理的窗格保留开场引导直到第一个问题(SPEC §data.agent)。`render` 依据的是转录
  *容器* —— 它从第一帧绘制起就存在 —— 而 `follow` 在同一 tick 里把那行字藏掉,于是
  空对话从未说过该输入什么。
- 当脚本返回很多结果时,Data 控制台的结果标签页全部保持可达(SPEC §data.console)。在此
  变更之前,标签页会折成两行,最后几个被藏到该栏的事实与按钮之下。现在每个标签页在一条
  可横向滚动的条带里保持单行 —— 鼠标滚轮也行 —— 当前显示的标签会滚动进入视野,其后还
  有一个 *All results* 列表点名每个结果、正在显示的那个打勾,并带有一个能匹配表或语句
  的查找框。
- 以 `-max` 结尾的模型 id 完整地到达提供商(SPEC §llm.agents)。在此变更之前,该后缀被
  读作 Max effort 并在代理未设置 effort 时被切掉,于是 `qwen3.8-max` 以 `qwen3.8`
  发出,`qwen-max` 或 `gpt-5.1-codex-max` 同样如此。`-low`、`-medium` 和
  `-high` 后缀仍算作 effort。
- OpenAI 线路上的 GPT-6 模型获得 `max_completion_tokens`(SPEC §llm.providers)。在此
  变更之前,只有 `gpt-5*` 和 o 系列如此,于是 `gpt-6-astra` 或 `gpt-6.1-terra`
  得到的是 `max_tokens`,而 OpenAI 对其推理模型拒绝该参数。现在从 5 起的每个
  `gpt-<N>` 都得到它;`gpt-4o` 和 `gpt-oss` 保持 `max_tokens`。

- `swiss remote`(以及对管理 API 的每一次 CLI 调用)在清洗环境的沙箱里运行时也能工作
  了。USERPROFILE 与 HOME 都未设置时,home 以前退回到工作目录,于是 CLI 在它运行所在的
  项目下寻找 `.swiss/session.json`,不发送 CLI 密钥并得到 `401 not signed in`
  (Codex 的代码在另一个项目里运行,2026-10-29)。home 现在退回到操作系统对该账户
  profile 的记录,即 Node 的 `os.homedir()` 所用的顺序;空变量视同未设置。

- Data 页面上 JSON 单元格的编辑弹层以语法颜色显示其值(SPEC §panel.code)。该弹层以前
  只有一个裸 textarea,JSON 值以单一颜色显示。它现在是 SQL 控制台的分层编辑器:键、字符
  串、数字和 `true`/`false`/`null` 呈代码配色,在输入时、点 Format JSON 时和切换
  布尔值时重绘。其他文本保持朴素。JSON 值现在以更高的 JSON 高度打开。两个编辑器现在也
  在文本层为滚动条预留槽位,于是当框开始滚动时,长长的折行不再让其下方的颜色错位。
- 代理的 `api_request` 遥测记录现在带有 `ttft_ms`,即首令牌时间(SPEC
  §llm.telemetry)。cc-otel 之类的收集器以前为每个 swiss 请求把 TTFT 显示为 "–",而
  Claude Code 自己的行有它。该时间从请求开始量到模型在流上的第一段思考、文本或工具调用
  输出,开头的帧不计入。没有输出的回复不带它。
- Test 流与历史中的工具调用在给出自身描述时按该描述显示(SPEC §llm.history,
  §llm surfaces Agents)。在此变更之前,即使代理已说明命令做什么,那一行仍显示原始命
  令。命令仍保留在展开的步骤里。
- 代理的 Test 对话按发生的顺序流式展示其工作(SPEC §llm surfaces Agents Test):以前,
  模型在每次工具请求之前说的话被追加到整个 "Working" 折叠区之下,于是每一轮的文字乱序
  堆在工作下面,而思考与工具步骤立在折叠区里面。现在一轮的文字立在该轮工作之内、位于其
  思考与其工具请求之间,并在该轮过去后渲染成 markdown —— 历史弹层早已这样展示;收尾的
  回答仍在下方出现一次。
- 代理的 Test 对话不再在任何请求接近代理的 `maxContextKiB` 之前很久,就以超出
  "continuation budget" 为由拒绝下一次发送(SPEC §llm.agents):守卫以前把每次封存调用
  的正文全部求和,而每次调用都会重发其前的对话,于是总和随轮数平方增长 —— lead 的运行
  1889 在 16 次调用、最大请求 215 KB 的情况下于 1.58 MB 处被拒,而每请求上限是 1024
  KiB。守卫现在衡量下一次发送要重放的内容(规范化的事前上下文)是否超上限,循环仍衡量
  每一轮的确切请求,400 轮与 512 次调用的上限保持不变;拒绝会点名越过的是哪条上限。
- 内置代理在思考中耗尽回答的同步运行不再把它的提交记为已同步(SPEC §memory.sync):
  循环以前把一条在 `maxTokens` 处截断、没有文字的收尾回答当作结束,于是私有仓库上的
  r-a8315d1b 一条笔记也没归档,却被记录为 ok 并越过它的两个提交,其摘要把责任推给从未
  触及的 24 轮上限(实际七轮)。现在没有收尾行就结束的循环会说明原因 —— 令牌上限、轮数
  上限或都不是 —— 而当一条笔记也没归档时,运行失败,提交等待下一次运行。
- 处理大量积压的同步运行放得进它的内置代理(SPEC §memory.sync):提示词以前只按各部分
  的上限定大小,于是同一仓库的 40 个提交做出 315 KB 的提示词,代理的读取加上它每轮重放
  的思考在十分钟后越过 512 KiB 的请求上限,运行失败且什么也没写入。提示词现在保持在
  160 KiB 之内:超过它时运行取一半数量的提交,最少一个,其余等待下一次运行。
- 覆盖合并历史的同步运行发送它选出的每一个提交(SPEC §memory.sync):它以前结束在反向
  拓扑序中最后一个选中的提交并读取 `base..that`,于是在合并 HEAD 之后另一侧掉了出去
  —— 仓库选了 68 个提交只发送了 40 个,下一次运行本会跨过合并做 diff。现在一次运行结束
  在 HEAD 首父线上的一个提交上 —— 提交放得下的最远那个 —— 且已同步的提交不再算作等待。
- Jobs 的 `retention.maxHistoryBytes`(默认 64 MiB)现在得到执行(SPEC §jobs.runlog):
  它以前被接受、被存储,却在任何地方都没有被执行,于是历史的唯一界限是
  `maxBytesPerJob` 乘以作业数。超过它时,整个 `logs/jobs/` 目录中最旧的记录先离去
  —— 无论哪个作业写的 —— 直到剩上限的四分之三;每小时的清扫检查每个文件,一次追加则
  检查自启动以来写过的文件。
- 数据库 MCP 被裁到输出预算(1000 项、256 KB)的回复仍是合法 JSON(SPEC §mcp.adapters):
  裁剪说明以前以一段散文跟在 JSON 之后,于是解析 `content[0].text` 的调用方抛出异常,
  而以空列表兜底的那类会把最大的答案读成没有行。账目现在装在回复里 ——
  `truncated: true` 和 `outputBudget: { shown, total, note }` —— 列表保留放得下的
  最多行数,而不是在此之下再被砍半;比预算还宽的一行保留其他字段,只裁剪长字符串
  (`…[+N bytes]`);而没有任何可丢弃内容的值以 `partial` 字符串返回。
- 发布构建不再嵌入过期的面板(SPEC §panel):上次构建之后编辑过的资源可能被漏掉,已删除
  的仍留在嵌入里,于是 `touch crates/swiss-panel/src/lib.rs` 是每次发布构建前的固定
  杂务。swiss-panel 的构建脚本现在跟踪资源树:编辑或新增的文件按目录跟踪,删除则按名称
  逐个文件跟踪。
- 输进远程命令的密码不再落进每一条运行记录(SPEC §remote.security, §remote.history,
  §mcp.calls):`bash -c` 一个词里的 `REDISCLI_AUTH=<value>` 以前会坐在远端的 `ps`、
  Runs 页面、`swiss run audit` 及其导出里。`remote.exec` 现在拒绝以文本形式进入 argv
  或 cwd 的凭据 —— 一条赋值、一个 `--password` 式选项、一个 `Authorization:` 头、
  一个 JSON 成员、URL 的密码、`redis-cli -a`、`sshpass -p`、`mysql -p` —— 点名
  那个词,从不说出值,并指向凭据库与 `--env`。记录与 MCP 调用日志对此类值脱敏,之前
  写下的记录会在下次启动时补上脱敏。远程技能把 `sudo` 放在唯一需要 root 的那条命令
  上,而不是放在 `python3 -` 或 `bash -s` 上,并用 `--preserve-env` 让机密信息穿过
  它。
- 穿过隧道的 redis 遍历不再在半秒处死掉(SPEC §mcp.db, §data.redis):连接以前在 swiss
  的 10 s `commandTimeout` 之下保留 redis-rs 自带的 1 s 连接与 500 ms 回复等待,于是
  穿过 SSH 隧道时,缓慢的 `SCAN` 轮次回来是 "timed out",`redis_scan` 和 Data 视图
  停在键空间半路。连接现在为 5 s,`commandTimeout` 是回复的唯一界限。Data 视图的键
  页面越过一无所获的轮次继续走,`COUNT` 翻倍至最高 1000,而不是每轮一个空页面加一次
  *More* 点击。
- `redis_scan` 的类型过滤器在低于 6.0 的服务器上也能工作(SPEC §mcp.db):
  `SCAN … TYPE` 是 6.0 才有的,于是对 5.x 的带类型扫描在第一轮就以语法错误死掉。引擎
  现在一次性询问服务器的版本,在较老的服务器上于一条 pipeline 中按 `TYPE` 过滤每一
  页。
- 当 Windows 暂时拒绝分阶段的重命名时,`swiss skill install` 会重试(SPEC §host.cli):
  在有 AI 客户端盯着 `~/.agents/skills` 时,安装曾在移除旧副本之后以 "Access is
  denied" 失败,那里没留下任何 swiss 技能。移除与重命名现在每逢拒绝就每 25 ms 重试一
  次,持续约一秒。
- 简报中已确认的笔记不再读作被它否决的未确认猜测 "⚠ contradicted by"(SPEC
  §memory.brief);猜测在显示之处保留其 `contradicts` 行,而相互冲突的两条已确认笔记
  保持标记。
- 部署的构建戳点名它所构建的树(SPEC §host.ops):`-dirty` 标记可能比引起它的杂散文件
  存在得更久,而构建之后的一次编辑却保留着干净的戳。当上一次构建的戳与树不符时,部署
  脚本会 touch `build.rs`;当新二进制仍带着别的戳时,它会在停止生产之前失败。
- 代码索引收录 Java 家族的方法(SPEC §memory.code):Java、Kotlin、Scala 或 C# 主体中
  一段缩进的 `Type name(args)` —— 越过注解、修饰符与类型参数 —— 是一条 `method`
  定义,于是 `swiss memory code find` 与应答者的 `code` 工具能说出哪个文件哪一行定义
  了它。以前只索引类型,于是关于方法的问题得到 "no definition",答案点名一个从类名猜来
  的文件。扫描现在带版本号:旧扫描读过的索引在其下次刷新时逐文件重读,而一次 find 会先
  刷新超过其 30 s 间隔的索引。应答者只把代码放在工具指明的位置,没有定义时就点名提及该
  名称的文件。
- 代码索引区分只在注释里点名某物的文件与代码真正使用它的文件(SPEC §memory.code,
  §memory.rag):`swiss memory code find`、应答者的 `code` 工具和
  `GET /api/knowledge/code/find`(`inComments`、`inCommentsTotal`)先列出代码点名
  的文件,其余的分开列出,标记为不使用它的文件。以前一条注释里点名了某作业的通知器被
  与该作业的调用方并列,一条回答说通知器发送了作业的推送。应答者只依据定义或工具展示的
  行来说一个文件做什么,否则称其为提及该名称的文件;并且只有当代码显示该情形使用了规则
  所指之物时,才把文档的一般规则应用到单个情形。索引的 schema 为 2:它会在下一次
  `code index` 时被丢弃并重建。
- 被脱敏的私钥块之后的定义或文档小节位于它自己所在的那一行(SPEC §memory.code,
  §memory.docs):脱敏以前为整个块保留一行,于是其后每一行的编号都偏低。代码索引会把
  每个文件重读一次;文档在下一次变更时重新编号。
- 文档的卡片跟随代码(SPEC §memory.docs):它保留自己给出的代码名称,每次 docs 运行都
  会在刷新后的代码索引里重新查找它们,于是此后被索引的定义 —— 新代码,或旧扫描漏掉的
  方法 —— 加入卡片,移过位的定义位于它现在的位置,不询问模型(`R card(s)
  re-linked`)。卡片写入时索引里还缺少的名称以前会被永久丢弃。存储迁移到 schema 10;
  以前写下的卡片会在下一次 docs 运行时重新请求。
- 复查把超过 2 KiB 的行分段展示(SPEC §memory.init The recheck):行的头部、其笔记所
  点名的代码名称前后的区段,中间以 `⋯ N bytes not shown ⋯` 相隔。单行 SQL 转储的
  `INSERT` 以前独自占满一个问题,于是关于其数据行的笔记从未被评判,一直留在审查收件
  箱里。模型抄进证据摘录的复查 `⋯ … not shown ⋯` 标记,现在在代码检查中被读作省略
  (`excerpt_mismatch`,SPEC §memory.code),而不是扣住笔记。
- `swiss memory ask` 为答案最多等待 20 分钟,而不是其他所有网关调用得到的 30 秒:应答
  代理借记忆的工具琢磨问题要好几分钟,而 CLI 以前在网关仍在作答时就放弃了。
- 每日 cron 作业不再在网关重启前后丢日子(SPEC §jobs.triggers)。认领记录以前保留时钟
  读数的毫秒位,于是重启之后,发生分钟之内的第一个 tick 可能读成 "before" 它而丢掉那
  一天,表现为晚一天的 `missed` 行(或一次带着昨天之键的运行);调度现在以整本地分钟
  工作,旧认领在读取时向下取整。网关停机期间错过的发生,现在在启动后的第一个 tick 就被
  记账,而不是晚一个周期。在 `misfire: "skip"` 下,迟到超过一分钟的发生被记为
  missed 且绝不迟跑,而准时的发生即使在间隔之后也照常运行(以前会连同摘要一起被跳过);
  Jobs 历史不再打印 "0 more missed"。
- 复查的修复现在释放仅因模型引用方式而被代码检查扣住的笔记(SPEC §memory.code):摘录
  比较能跨过 SQL 或 JSON 字符串的转义层级(`lang=\"en\"` 被引用为 `lang="en"`),
  也能跨过代表模型从未见过的值的 `[redacted:<rule>]` 标记;在锚定文件里开启一族键的
  名称(`shop_email_coupon` 之于 `shop_email_coupon_name_1..9`)不再是未知名称。在
  一个大型 Java 项目上,五次复查曾重写而未释放的 26 条笔记中有 24 条通过;留下的两条是
  真的引用错了。脱敏不再把长 camelCase 方法(`handleOrderNoticeV1`)当成键,于是笔记
  点名的是方法而不是标记。
- LLM 代理校验独立检查字面凭据片段,不再受 URL 其他位置引用的影响;它拒绝字面量与引用
  混用的 userinfo,且不回显机密信息。
- 禁用记忆时,停止会关闭其笔记与代码索引的数据库池,而不是保留一个工厂生命周期的服务。
  重新启用在同一数据上使用新的惰性池;过期的调用方无法重新打开已停止的服务,关闭一个
  未使用的服务不会创建任何数据库。

- 记忆同步弹层用面板的语言把一次复查的结果只说一遍(SPEC §memory.panel)。以前在中文
  下,它在同一运行的中文计数上方显示引擎的英文摘要。现在复查运行、或非 dry 运行的完整
  运行,把该摘要保留为首行的悬停内容。dry 复查用文字说出它将问什么,空的结果则说没有
  笔记在等待。
- 打开一条记忆笔记约需 0.3 s,而不是 1-3 s(SPEC §memory.search)。词法层的查询现在只
  运行一次全文 MATCH,再逐条查找每条命中对应的笔记。SQLite 3.51 以前把执行计划做反了:
  它遍历项目的每条笔记,并对每一条重跑 MATCH。这让一个 2,000 条笔记的项目上每次相关
  笔记查询花 1.3 s,该笔记的其他请求都排在后面。带 CJK 以外词语的搜索也走了同一条路。
- LLM 历史在超过其 100 MB 份额之后仍继续记录(SPEC §llm.history)。一旦文件超过份额的
  90%,最旧的调用与已完成的运行先离去,直到活数据降到 70% 以内,然后是再无任何东西指名
  的片段,最后压实文件。打开中的会话及其调用绝不修剪。一次因份额被拒的写入会在一分钟内
  启动该修剪。在此之前,只有 180 天的清扫会删除任何东西,于是一个代理一周记录了 146 MB
  的 home 从那以后每次 LLM 调用与运行都被拒绝。
- 记忆的地图草稿按分层项目的业务所拥有的文件为其排序,包括以它们命名的目录
  (SPEC §memory.init)。它以前按名称匹配到的文件排序,于是主要住在自己目录里的业务被切
  到保留的 48 个之外。那是一个大型分层 Java 项目里的 `Pm`:以它命名的类只有几个,数十
  个类在 `web/req/pm`、`web/res/pm` 和 `remote/pm` 里。它的文件未被归档,它的笔记
  排在最后。
- 绘制领域的地图运行会按锚点归档未归档的笔记,于是在其代码拥有领域之前保存的笔记会跟随
  该领域(SPEC §memory.init Redraw)。
- 给定一条没有领域认领的路径时,简报不把调用方放进任何领域(SPEC §memory.brief)。它
  以前退回到任务的命中,于是 `PmService.java` 曾被算作 "in" 直播间。
- 样式化 select 的展开列表在页面或弹层滚动时跟随其触发器,而不是停在展开时的位置。
- `chat` 代理的 Test 在每条路径上都像其运行时那样提问:一次请求、不带工具、提示词按
  原样给出。实时对话以前向它提供 Test 的 `read`/`list` 工具和内置的轮数上限,而阻塞
  式 Test 归档的运行没有任何轮次。超出期限的 Test 或记忆代理询问,现在把被丢弃的往返
  记录到自己名下的运行之下,而不是记成一条没有运行认领的调用。派生代理的 Test 无论检查
  结果如何都会关闭其运行行,包括程序无法解析的情况。提供商不再可解析的实时 Test 会得到
  一个点名它的 400 拒绝,而不是事件读取器无法解析的 200。
- data 与 process 历史从首次启动起就坐上每小时的维护席位(SPEC §host.history):启动时
  注册,停止时注销;该趟清扫扫到行数界限,压实超过份额的文件(带 WAL 截断的 VACUUM),
  并刷新打开时所测得的配额快照。卡在自己份额上的存储以前会在重启后再次拒绝每一次写入,
  因为没有任何东西重新测量快照仍然持有的文件;启动趟现在把它带回份额之内,准入恢复。

## [0.1.4] - 2026-10-07

### 新增

- `remote.exec` 接受 `stdin`:最多 1 MiB 的文本,原样送入命令的标准输入,随后关闭 ——
  从不用引号包裹,因此给 `psql` 的 SQL 或给 `sh -s` 的脚本无需为任何 shell 转义。
  `${secret://}` 引用会在其中解析;运行记录只保留它的大小。没有它 stdin 会立即关闭,
  读取 stdin 的命令随即结束,而不是把时限等满。
- `swiss remote exec --stdin-file PATH|-` 以这种方式发送一个文件(或它自己的 stdin);
  UTF-8 BOM 会被丢弃,带 BOM 的 UTF-16 文件会被解码。会被本地 shell 在引号处拆开的
  `sh -c` 脚本,在任何内容运行之前即被拒绝,并给出两条出路。

### 修复

- `swiss remote exec`(以及 `sync`、`push`、`swiss run logs`)不再丢失快速命令输出的
  连续片段:实时窗口漏掉的间隙会从运行记录补印出来。

## [0.1.3] - 2026-10-03

### 新增

- 代码配色方案:每个代码块(JSON 视图、Mongo 文档卡片、SQL 控制台、DDL 窗格与预览)
  都是一块编辑器界面,可选 IntelliJ Light、Darcula、IntelliJ Dark 或 GitHub,从顶栏的
  调色板按钮挑选;默认跟随主题。SQL 按 DataGrip 的方式高亮 —— 关键字与类型、函数、列、
  注释 —— DDL 窗格为各行编号。
- MongoDB 连接(`"type": "mongo"`、`mongodb://` URL、MongoDB 4.4 或更高版本):六个工具 ——
  `mongo_list_collections`、`mongo_find`、`mongo_aggregate`、`mongo_describe_collection`、
  `mongo_command`(可破坏服务器;会话与游标命令被拒;删除数据需要 `allowDestructive`)
  和 `mongo_inspect`(活动、慢查询、按集合耗时、未使用的索引、存储、服务器、复制)——
  外加 `mongo://` 资源。
- 数据页面为 MongoDB 准备的文档工作区:集合按 Collections / Views / System 条带分组,
  查询栏采用 shell 语法(filter、projection、sort、skip、字段补全),文档可以列表、
  规范 JSON 或表格呈现,只有在读取之后无人改动过该文档时才写回的编辑器,副本集上以
  单个事务完成的多文档写入,批量更新与删除,带逐阶段预览的聚合管道构建器,模式分析,
  索引及使用情况,带集合扫描结论的 explain 计划,校验规则,JSON / NDJSON / CSV 导出
  与导入,以及带模板的命令控制台。

### 修复

- SQL 控制台不再在紧跟标点之后的字符串内部的 `;` 处拆分语句(`VALUES ('a;b')`),
  高亮器也把 `` (`id`) `` 读作引号括起的名称。

## [0.1.2] - 2026-10-02

### 新增

- `swiss path [on|off]` 与面板里的“Put swiss on your PATH”开关:Windows 上把本 exe
  所在的文件夹加入当前用户的 PATH,其他平台则是 `~/.local/bin/swiss` 符号链接。手工
  添加的 PATH 条目也算作已开启。
- `pg_inspect` 与 `mysql_inspect`:每次调用一项具名诊断检查 —— 正在运行的语句、带
  阻塞者的锁等待、top 查询、顺序扫描、未使用的索引与没有主键的表、缓存命中率、连接数,
  外加 vacuum(PostgreSQL)或碎片化(MySQL)。缺少权限时,答复会附上能修复它的授权
  语句。
- `mysql_describe_table`:表自己的 `CREATE` 语句,加上指向它的外键。

### 变更

- `pg_describe_table` 以一次查询作答,含主键、索引、双向外键与约束;`schema` 可选,
  经 `search_path` 解析。
- `mysql_list_tables` 接受一个 `database`,并在回复开头把库名点出一次。
- `redis_scan` 改用 `limit`(要收集的键数,默认 100)代替 `count`,并一直扫描到凑够
  为止 —— 稀疏模式不再返回空页。

### 修复

- 运行测试套件不再在执行它的机器上关掉“登录时启动”;测试不会写入任何属于操作者的
  OS 注册项。
- `swiss autostart` 不再把它的标签写进它所打印的那个注册表位置。

## [0.1.1] - 2026-10-02

### 修复

- Linux 归档在 Ubuntu 22.04 上构建,可运行于 glibc 2.35 或更高版本(Ubuntu 22.04、
  Debian 12 及更高);0.1.0 的归档需要 glibc 2.38。

## [0.1.0] - 2026-10-02

首次发布。

### 新增
- 网关:AI 客户端所需的每一个 MCP 都在 bearer token 保护下经 `/mcp/<name>` 提供 ——
  stdio 子进程(空闲直到第一个请求,闲置后被回收)、带 OAuth 的远程 HTTP、配置中声明的
  REST API、MySQL、PostgreSQL、Redis 与 zai-vision。**Import .mcp.json** 把既有的
  客户端配置搬迁过来,每个 MCP 都可复制一条现成的客户端命令。
- 管理面板,嵌入二进制,提供英文与简体中文。它需要登录:`swiss start` / `swiss open`
  给出的单次使用链接会设置一个只有面板自己的页面才能使用的会话 cookie;CLI 以每次启动
  都轮换的密钥为它的调用签名(脚本用 `swiss api`)。
- 数据:带缓冲编辑的表与行、带服务端补全的 SQL 控制台、导出与导入、结构操作、实时
  会话;Redis 键、带 Follow 与只读消费者组的流,以及补全命令与键的控制台。
- 隧道:SSH 连接、转发规则、代理拨号与跳板机,以及搭乘隧道的 MCP。
- 作业:配置驱动的计划作业,带 overlap、misfire 与 capture 模式,以及一份运行历史。
- 终端:浏览器里的本地 PTY 与远程 SSH shell,带录制。
- 远程执行:声明了能力的目标,`swiss remote exec / sync / push / cat / write / pull`
  与 `swiss run`,带审计的运行日志,`/mcp/remote`,以及目标页与运行页;每个目标在
  自己的车道里运行。
- 机密信息凭据库:`${secret://name}`(可选 `:default`)与 `${ENV_VAR}` 引用只在
  使用的时刻解析;存储的值只写不读,凡可能回流之处一律脱敏,`swiss export` 也不例
  外。远程地址在面板里保持脱敏,直到被揭示。
- 每个列表的分组、登录时启动(`swiss autostart`)、检查新版本的 `swiss update`、
  迁移机器用的 `swiss export` / `import`,以及内嵌的 `swiss` 与 `swiss-remote`
  AI 技能(`swiss skill install`)。

[Unreleased]: https://github.com/young1lin/swiss/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/young1lin/swiss/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/young1lin/swiss/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/young1lin/swiss/releases/tag/v0.1.0
