# 41 — swiss remote：UTF-8 到底、七天可溯源、SKILL.md 讲清楚

> 状态：**已实施，真机已验**（2026-09-21，master：U1 699b305、U2 8306993、U3+U4 835c7c7、A1 d67db29、
> A2 755eb9e、A3 21cfc8d、S1+D1 c4d764e、A4 801c834、U5 324a656、A5 见 §4.3）。owner 的三句话："强制输入和输出都是 UTF-8，避免乱码"、
> "可审计，可以溯源最近七天的内容"、"SKILL.md 关于 swiss 的介绍你自己看看怎么做"。本文先把
> 代码里的现状说清（§0），再定契约（§1），再列工作项（§2）和验收（§3）。docs/34 是 remote 的
> 总契约，本文只改它没说的和说错的地方。

## 0. 现状（读代码得出，逐条给 file:line）

**UTF-8 —— 四处会把一个多字节字符切成两半：**

- `crates/swiss-host/src/services/runs.rs:159` `RunOutputBuffer::read_from`：live 输出按**任意
  字节偏移**切窗口，`from_utf8_lossy` 后一个 3 字节的汉字跨在 `max` 边界上就变成两个 U+FFFD
  （CLI 每 128 KiB 轮询一次；面板同一条路）。被驱逐（`start` 前移）后的起点也可能落在字符中间。
- `crates/swiss-remote/src/actions.rs:852` `remote.cat`：**逐 chunk** `from_utf8_lossy`，SSH
  读块边界上的汉字必坏。`actions.rs:92` outcome 的 tail 环：按字节截头，尾部半个字符变 U+FFFD。
- `crates/swiss-remote/src/history.rs:165、:317`：记录的 64 KiB tail 环和文件窗口读，同样的
  字节边界问题。
- 远端 locale：`exec_command_string`（`swiss-tunnels/src/tunnel/remote.rs:88`）只 export 调用者
  给的 env；远端登录用户若 `LANG` 为空或非 UTF-8，`ls`/`git`/`python` 输出转义或问号，
  argv 里的中文路径按远端 locale 解释。没有任何一处声明"这条链路是 UTF-8"。
- 本机侧：CLI argv 由 Rust 从 UTF-16 转 UTF-8，`write` 的 stdin 按字节透传，stdout 直写
  控制台用 WriteConsoleW——本身没问题；**PowerShell 5 / 旧 pwsh 用 OEM 代码页解码原生
  程序输出**才是 Windows 上"乱码"的常见来源，这条只能文档化。

**审计 —— 记录有了，但缺三样：**

- 已有：`logs/remote/runs.jsonl` 一行一个已结束的 remote.* run（target、argv、cwd、env **键**、
  exit、时长、字节数），输出全文在 `logs/remote/out/<id>.txt`；30 天 / 500 MiB / 5000 条三个预算
  （history.rs:38-46）。`GET /api/remote/runs` 从末尾分页，可按 target 过滤。
- 缺 1 **actor**：记录里没有"谁"。`/api` 无凭据（loopback 即边界，auth.rs:19），CLI 和面板提交
  的 run 只有 `owner = manual`；MCP 工具提交的只有 `label = mcp:<action>`——而 `calls.rs:381`
  `current_source()` 明明知道是哪个 token（client label）。
- 缺 2 **窗口保证**：预算驱逐一律最老先删（history.rs:428），输出文件大就会把七天内的记录连
  索引行一起删掉。"七天可溯源"现在不是承诺。
- 缺 3 **查询面**：没有按时间窗过滤（`since`/`until`），没有按 actor 过滤，CLI 只有
  `swiss run status|logs|cancel <id>`，没有"最近七天都干了什么"的一条命令。

**SKILL.md**（`src/skill_assets/SKILL.md`，随二进制发布，`swiss skill install` 装给 agent）：只讲
remote 的命令，对 swiss 是什么、边界在哪、UTF-8 与审计的契约一个字没有；"Everything else
the gateway does … ask the user" 让 agent 面对 MCP/数据库/任务时两眼一抹黑。

## 1. 契约

### 1.1 UTF-8（U）

1. **字节边界永远落在字符边界上。** 任何把字节窗口变成文本的地方——live 窗口、历史窗口、
   tail 环、cat——起点跳过续字节，终点退到最后一个完整字符；跨窗口的字符在下一次读到，
   不产生 U+FFFD。只有真正非法的字节才 lossy。窗口不会因此停摆：退无可退时（`max` 小于
   一个字符）照旧 lossy 返回，游标仍前进。
2. **远端命令在 UTF-8 locale 下运行。** `remote.exec` 默认 export `LANG=C.UTF-8`、
   `LC_ALL=C.UTF-8`（`export` 发生在登录 shell 之后、`exec` 之前，所以 shell 自身不会因不存在
   的 locale 打警告；程序拿不到该 locale 时静默回落 C，输出仍是 argv 里的原始 UTF-8 字节）。
   调用者 env 里显式给的 `LANG`/`LC_ALL`/`LC_*` **优先**，一个字都不覆盖。这是 docs/34 §13
   "`exec_command_string` 是唯一的 argv→string 步骤"之内的一次追加，不是第二条路。
3. **本机侧不改代码，改文档**：SKILL.md 写明 Windows PowerShell 要
   `[Console]::OutputEncoding = [Text.Encoding]::UTF8`（或 pwsh 7.4+ 默认即可），`swiss remote
   write` 的 stdin 按字节透传所以文件本身必须是 UTF-8。

### 1.2 审计（A）

1. **每个 run 带 actor。** `SubmitRequest.actor: String`，序列化为 `actor`，进 `runs.jsonl`。
   来源：CLI 自报 `cli:<os user>@<hostname>`（loopback 上自报即可信，文档说明）；MCP 工具
   用 `current_source()`：`mcp:<token label>`（这个是认证过的）；面板 `panel`；其它 API 调用者
   缺省 `api`。
2. **七天保护窗。** `AUDIT_WINDOW_MS = 7 天`：驱逐永远不删七天内的**索引行**。字节预算压过来
   时先删最老 run 的**输出文件**（索引行改写为 `outputEvicted: true`，tail 仍在），只有更老的
   记录才整条删除；条数预算对窗内记录同样让路（索引行 ~1 KB，窗内涨不到哪里去）。30 天总
   保留期不变。
3. **查询面。** `GET /api/remote/runs` 加 `since`、`until`（ms 或 ISO）、`actor`；CLI 加
   `swiss run audit [--since 7d|<ISO>] [--until …] [--target t] [--actor a] [--json]`：一行一个
   run（时间、actor、target、动作、argv 引号原样、exit、时长、字节、id），默认最近七天；
   `--export <dir>` 把窗内索引行和输出文件复制出去（溯源材料交给人）。
4. **面板**：Remote › Runs 每行的 meta 加 actor（不翻译，它是标识符）。

### 1.3 SKILL.md（S）

一份 agent 第一次见 swiss 就够用的介绍：swiss 是什么（一个 loopback 进程；MCP / 数据 / 隧道 /
任务 / 终端 / 远程六件事）、边界（永远 loopback、密文只进不出、workspaceRoot 是护栏不是沙箱）、
remote 的命令参考（保留现有）、**UTF-8 契约**（§1.1 三条的 agent 版）、**审计契约**（每次
remote 动作都留痕、`swiss run audit`、七天）、面板和 `swiss --help` 在哪。frontmatter 的
`disable-model-invocation: true` 是 owner 的选择，不动。

## 2. 工作项（每项一个 commit，均已落地；与 §1 的差异记在这里）

- **U1** `swiss-core::util::utf8`：`char_boundary_end(bytes) -> usize`（去掉尾部不完整序列后的
  长度）、`char_boundary_start(bytes) -> usize`（跳过开头续字节后的偏移）、`window(bytes) ->
  &[u8]`。纯函数，2/3/4 字节和非法序列各有用例。
- **U2** `RunOutputBuffer::read_from` 用 U1 收窗；`truncated` 起点跳续字节；空窗保护。测试：
  一个汉字跨 `max` 边界的两次读拼起来无 U+FFFD。
- **U3** remote：cat 收齐再解码一次；outcome tail、history tail 环、history 文件窗口用 U1。
- **U4** locale 默认值：`actions.rs` 组 env 时在**前面**插入 `LANG`/`LC_ALL` 缺省，调用者的同名
  键优先（Vec 顺序：缺省在前，后者 export 覆盖前者）；MCP 工具描述和 CLI usage 提一句。
- **A1** actor：`SubmitRequest.actor`、`RunView.actor`、`to_json`；`submit_run` 读 body.actor
  缺省 `api`（空白或超过 128 字节同样回落）；CLI 发 `cli:<user>@<host>`；MCP 从 server 在
  factory 时捕获的 `CallSource` 取 `mcp:<client>` / `panel`——**不能**在 `run_plan` 里调
  `current_source()`，rmcp 从自己的 task 驱动 call_tool，task-local 在那里是空的（实施时踩到，
  测试 `the_run_is_booked_to_the_token_that_made_the_call` 钉住）；jobs 发 `jobs`。
- **A2** 七天窗：`evict` 两阶段；`LineFacts` 学会 `outputEvicted`（驱逐后按盘上 0 字节计）；
  `usage()` 口径不变。§1.2.2 没说的一条：预算在窗内仍超时（例如七天内 5000+ 条），一次
  什么也删不掉的 pass 会在 ledger 记 `hold_until_ms`（最老受保护行离开窗口的时刻），此前
  的读写不再重扫索引；字节超预算时新落盘的输出文件解除 hold（它本身可驱逐）。API 的
  `limits` 多了 `auditWindowMs`；面板预算行写"最近 7 天必可溯源"，被驱逐的行展开时说明
  输出已清出、记录仍在（不再显示"没有产生任何输出"）。
- **A3** 查询：`history.query(PageQuery)`（`page` 保留旧签名）加 since/until/actor 谓词（仍从
  末尾走，遇到早于 since 的行即停；since 含、until 不含，都对 `endedAt`）；API 三个参数，
  时间格式错给 400 而不是悄悄放宽窗口；活动行也按 actor/until 过滤；`swiss run audit` 一行
  一个 run（时间到秒、actor、target、动作、argv 按 POSIX 单引号规则、结果、时长、字节——
  输出被驱逐的带 `*`——、`#id`），`--since` 另收 `7d/36h/90m/30s`，`--export DIR` 写
  `runs.jsonl` + `out/<id>.txt`。
- **A4** 面板 meta 加 actor + vitest + 真浏览器走查（proof-of-life 规则）。
- **S1** SKILL.md 重写（六件事、三条边界、命令参考原样、`/mcp/remote` 五个工具、UTF-8 契约、
  记录契约与 `swiss run audit`）；`skill_install.rs` 的两条断言仍成立。
- **D1** docs/34 状态头加一段指向本文；`swiss --help` 终于列出 `remote` 和 `run`；
  `swiss remote help` 与 `remote_exec` 的 MCP 描述各加一句 locale 与记录。AGENTS.md 未提
  remote 记录口径，不动。

## 3. 验收

1. `cargo test --workspace`、`clippy -D warnings`、`npm run check` 绿；新增用例：U1 边界、U2
   跨窗、U3 cat 拼接、U4 缺省与覆盖、A1 三种 actor 落盘、A2 窗内行在字节压力下存活且输出被
   驱逐、A3 since/until/actor 过滤。
2. 19998 真机：`swiss remote exec <t> -- printf '中文\n'` 与 `-- python3 -c "print('中文')"`
   原样回显；`swiss remote cat` 一个含中文、大于一个 SSH 读块的文件无 U+FFFD；`swiss run audit`
   列出这些 run 且 actor 为 `cli:…`；面板 Runs 行显示 actor（英/中各看一次）。**前提是有一台
   可用的 SSH 目标**；没有则用 fake transport 的集成测试代替真机，并如实记为"真机未验"。
3. `swiss skill install` 装出来的 SKILL.md 与源一致。

## 4. 验收记录（2026-09-21）

1. 门禁：`cargo test --workspace --locked`（32 个 test 二进制全绿，含 A1 三种 actor 落盘、A2 窗内
   行在字节压力下存活且输出被驱逐、A3 since/until/actor）、`clippy -D warnings`、`npm run check`
   （77 文件 690 用例）。
2. 19998 真机（release 构建，`target-test`）——**记录是种进去的，远端未跑**：owner 快照里的三个
   SSH 连接是真实机器，未经许可不在上面执行命令；改为在测试 home 的 `logs/remote/runs.jsonl`
   种 9 条与 `history.rs` 落盘形状一致的行（actor 覆盖 cli/mcp/panel/jobs/api，含中文 argv 与
   中文输出、一条 `outputEvicted`、一条 131107 字节且第 131071–131073 字节是一个"："的输出）。
   在此之上：
   - `swiss run audit` 默认七天九行、`--since 36h` 五行、`--actor mcp:claude-code` 三行、
     `--since ISO --until ISO` 两行、`--since yesterday` 退出 1 并说明格式、`--json` 九个对象；
     `--export` 写出 `runs.jsonl` + 7 个输出文件，`out/17.txt`、`out/12.txt` 与记录中的原文件
     SHA-256 相同——131072 字节读窗在"："前停下（`nextCursor` 131071），第二窗从它开始，
     两窗都无 U+FFFD。
   - 面板 Remote › Runs（agent-browser，真实点击，新开页面）：每行 meta 在 id 与结果之间显示
     actor；预算行"kept 30 days · the last 7 days always traceable"；#14 展开显示"The output
     (24 B) was evicted…"而不是"No output was produced"；#12 展开中文输出无 U+FFFD；#17 第一页
     到"日志"为止，Load more 后拼成"日志：编译完成"。文/A 切到 zh-CN 后同样一遍：
     "最近 7 天必可溯源"、"输出(24 B)已被容量预算清出;记录本身保留 30 天。"、"#13 · panel · 成功"
     （走查发现无退出码的 sync/cat 行 state 原样英文，顺手补了 stateSucceeded/Failed/Running/
     Queued 四个键）。480px：页面不横向滚动，行可点开；meta 在行内被裁掉是 `.call` 行与
     Traffic 共用的既有布局，未动。
3. **真机（owner 在 19998 上建的 `ubuntu-1`，root `/tmp/swiss`，2026-09-21 07:25–07:29 UTC，
   `swiss run audit --since 30m` 能列出全部 13 条，actor 均为 `cli:<user>@<host>`——A1 的 `cli:<本机用户名>@<主机名>` 记法，此处脱敏）：**
   - `exec -- sh -c 'echo LANG=$LANG LC_ALL=$LC_ALL; locale charmap'` → `LANG=C.UTF-8 LC_ALL=C.UTF-8`
     / `UTF-8`：U4 的缺省到了远端。
   - `exec -- printf '中文\n'` 回来的字节是 `e4 b8 ad e6 96 87 0a`；`python3 -c "print('中文 ✓ émoji 😀')"`
     原样回显（python 看不到 UTF-8 locale 时会在 😀 上抛 UnicodeEncodeError）。
   - `write ubuntu-1 'swiss-utf8-小文件.txt'`（中文文件名，37 B 中文内容）再 `cat` 回来 `cmp` 相同。
   - `write` 一个 121393 B 的中文文件（3500 行"第 N 行：编译完成 ✓ ok"），远端 `sha256sum` 与本地一致
     （`ab7f6f60…4d24`）；`cat` 回来**第一次只剩最后 65534 字节**——这暴露了一个 docs/41 之外的既有缺陷
     （**U5**）：run 在 CLI 第一次轮询前就结束时，live 窗口已按 `KEEP_FINISHED_OUTPUT_BYTES` 压到最后
     64 KiB，`stream_run` 把 `truncated: true` 当没看见，把尾巴当全文打印。修法：cursor 0 且 `truncated`
     且 terminal 时改从记录（`/api/remote/runs/{id}/output`，记录在 run 变 terminal 之前已写完）打印全文；
     没有记录（非 remote run）则照旧。`recorded::a_run_that_finished_before_the_first_poll_is_whole_in_the_record`
     钉住这条 API 契约；修后再 `cat` 回来 121393 B `cmp` 相同、无 U+FFFD。测试文件已从 `/tmp/swiss` 删除。
   - 顺带看到并修掉的既有限制（**A5**）：run id 原是每次进程启动从 0 计的内存计数，记录按 `runId` 索引，
     重启后同号的行会重复。现在 `RunCoordinator::persist_sequence(<home>/runs.seq)` 把下一个 id 跨重启
     保存（每次分配 tmp+rename 写一次），且 `RunHistorySink::last_run_id()` 让记录里的最高 id 成为编号
     的下限（升级后第一次启动、seq 文件丢失都不会撞号）。19998 上：记录最高 #19 → 启动后 seq 已是 20 →
     跑一条 #20 → 重启 → 下一条 #21。owner 问"为什么不是页面上设置的唯一 alias"：alias 标识的是**目标**
     （哪台机器），每条记录和 audit 的 target 列本来就带；run 需要自己的号，因为一个目标有很多次运行——
     坏的是号码跨重启不唯一，现在唯一了。
4. `swiss skill install` 装出的 SKILL.md 与源一致（`skill_install.rs` 两条断言）。
