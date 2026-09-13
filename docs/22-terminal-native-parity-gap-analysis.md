# 22 — 终端"本地终端手感"差距分析与路线图(已探索 9 个参考项目)

状态:**分析完成;P0 全部 + P1 的滚回搜索已在 `terminal-parity` 分支实施**(2026-09-13,
面板四提交:`e3d7d9a` 纯函数层、`ebb8540` P0 接线、`839a7c3` P1 搜索 + 修复并发编辑丢失的
定义块;Rust 侧仅整树拷贝 `admin_assets/`,字节校验通过)。P1 其余(设置页、shell 下拉)与
P2–P4 未动。本文是路线图,不是规范;实施每期时再出对应 spec/prompt。
基线:2026-09 的 swiss 终端插件(docs/14 已实施 + docs/15 补丁)。

## 0. 方法与来源

九个开源终端项目 shallow-clone 到仓库外的 `../terminals-ref/`(与本仓库平级,不进 git),
每个项目派一个独立探索 agent,按同一份 brief 产出差距报告:swiss 现状清单(不把已有的当缺口)、
四条硬约束(内存预算 / 纯文本资源管道 / 单用户回环 / 克制设计),输出按"价值÷成本"排序的清单
+ 前三名实现深挖 + 明确不抄清单。本文件是九份报告的交叉汇总;`../terminals-ref/` 克隆在本机持续可用,
深挖引用的路径都在其中,可用 `git clone --depth 1` 复现。

| 项目 | 定位 | 对 swiss 的最大贡献 |
|---|---|---|
| ttyd | C+xterm.js 极简 Web 终端 | overlay 提示层、选中即复制、URL 偏好层、基线确认(swiss 全面超过它) |
| wetty | Node 浏览器直连 SSH | schema 驱动选项 UI、data-URI 响铃、OSC 5i/4i 流内文件下载;认证全是反参照 |
| sshx | Rust 协作终端(云) | chunk 游标可恢复输出环、typeahead 预测回显、ack-window 加固、RTT 测量 |
| tabby | Electron 全功能终端 | 搜索 UI 细节、pin-to-bottom 滚动机理、OSC 52/1337 中间件、客户端写流控 |
| xterm.js | 我们 vendor 的上游生态 | **addon 兼容性实测表**、核心 API 清单、若干事实纠正 |
| VS Code 终端 | 内嵌终端金标准 | **OSC 633 shell 集成(拱心石)**、find 契约、键位门(Shift+Tab 坑)、预启动输入队列 |
| Warp | Rust AI 终端(今年开源) | shell 集成标记协议(OSC 9278)、退出码语义、JSONL 历史、**录制脱敏** |
| Wave | Go+React 桌面终端 | CSI 2026 重绘回底、serialize 快照恢复、OSC 52 硬上限、滚回导出 |
| asciinema-player | cast 回放器 | 面板内最小播放器可行(~350 行移植),播放器本体不可 vendor(wasm) |

## 1. 基线结论

swiss 的会话机器(多标签、60s 宽限重连 + 64KB 追赶、背压 + stall、多路 attach、asciicast 录制、
票据 WS、WT 键位)已经**超过** ttyd/wetty 这类经典 Web 终端的架构层。九份报告一致的判断是:
差距不在协议层,在"本地终端手感"的最后一公里——标题、搜索、响铃、滚动语义、剪贴板互通、
shell 集成元数据、录制的可见性。

## 2. 多源交叉的共识清单

按"独立得出的报告数"排序(数字为来源数/9):

1. **OSC 0/2 标题 → 标签页标题**(7:ttyd、tabby、VS Code、sshx、xterm.js、Wave、wetty)。
   共识细节:手动重命名 > OSC 标题 > 连接名(三级优先);空标题回落;被收养的会话从列表行
   取初值;PowerShell/cmd 常不发 OSC 0/2,不得阻塞 UI。tabLabel() 纯函数放 terminal-core.js。
2. **滚回搜索**(4 + xterm.js 实测):vendor `@xterm/addon-search@0.16.0`(77KB,纯 JS,
   与 vendored 5.5.0 零 `_core` 冲突,实测),Ctrl+Shift+F,Enter/Shift+Enter,regex/case/
   wholeWord,i/n 计数走 `onDidChangeResults`(异步!);regex 自行 try/catch;内部已去抖
   200ms 勿重复;装饰色是内联样式,选克制单色;Esc 关 + clearDecorations + 回焦终端。
3. **响铃**(5+):5.5 **没有** bellStyle/bellSound 选项(实测,xterm.js 报告)——onBell 事件
   → 标签页徽章(复用现有 notice 色,焦点清除)+ 可选声音。声音两条合法路:WebAudio 振荡器
   (~10 行)或 wetty 的 ~400 字符 base64 MP3 data-URI(纯文本)。按本仓库"新特性默认开"规矩:
   徽章默认 on,声音默认 off(可设)。
4. **选中即复制**(3):带 trim 尾随空白 + **搜索框聚焦守卫**(搜索导航的程序化选区不得触发
   复制,tabby/waveterm 都踩过)。做成设置项。
5. **标签页快捷键与重命名**(4):Alt+1..9 跳标签、Ctrl+Shift+W 关、Ctrl+Shift+Tab 切换、
   双击重命名(覆盖 OSC 标题,同 Windows Terminal 的 suppress application title 语义)。
6. **主题/光标/外观设置**(5):3-4 套命名调色板(18 色对象)+ 跟随面板深浅色;cursorStyle/
   cursorBlink/scrollback;`term.options = {...}` 活应用(xterm≥5 动态赋值,即时生效)。
   全部 localStorage(屏幕偏好先例:fontSize)。参考 wetty 的 schema 模式但**不抄** iframe。
7. **每标签 shell Profile**(3):后端 `POST /sessions {shell}` 已支持、targets 已返回候选清单,
   面板只是没发——下拉选 shell + MRU 记忆,等于白捡 Windows Terminal 的 Profile 概念。
8. **多行粘贴确认**(1 但实现完整,tabby):CRLF/LF→CR 规范化、去单个尾随换行、仅非备用屏
   且开启时确认(预览前 1000 字符)、bracketed paste 按当前模式包裹。
9. **OSC 52 远程剪贴板**(4):vendor `@xterm/addon-clipboard@0.2.0`(6.2KB,纯 JS,实测兼容)。
   write-only provider(拒绝 `?` 读回查询——剪贴板窃取向量);硬上限 128KB 原文/75KB 解码后
   (Wave 的数);注入复用 Ctrl+V 同一剪贴板代码路径。
10. **滚动语义 + 新输出指示**(3):pin-to-bottom 机理(tabby 深挖,关键坑:xterm onScroll 只对
    内容滚动触发,用户滚轮必须 capture 阶段监听 + rAF 判定 viewportY>=baseY-1;write 前
    捕获 pin 状态)+ baseY 增量计数的"N new lines ↓"角标 + CSI 2026 配 CSI 3 J 的全屏重绘
    回底(Wave,修"退出 vim 卡滚动态")。
11. **shell 集成标记**(3,拱心石):VS Code 的 OSC 633(全套 MIT 脚本 ps1/bash/zsh/fish +
    nonce 信任模型)、Warp 的 OSC 9278(十六进制 JSON;**Windows 必须 OSC,ConPTY 吞 DCS**)、
    Wave 的 OSC 16162/7。统一策略:解析 OSC 633/133(+7/9;9 免费生态兼容),解锁:退出码
    徽章(**130=Ctrl-C、141=SIGPIPE 不算失败**,Warp:否则红徽章说谎)、cwd 进标签、
    Ctrl+↑/↓ 命令导航、长命令完成通知、带元数据的命令历史。
12. **录制可见性**(3):recordings 列表/下载 API;面板内最小播放器(asciinema 引擎纯 JS 可移植
    ~350 行,含 16ms 批写窗与 idle 压缩;**录制器头补写 `idle_time_limit: 2`**,一行后端改动);
    滚回文本导出"下载转录"(Wave bufferLinesToText);**录制流脱敏**(Warp secret_redaction 概念
    ——我们自己的 AGENTS.md 就警告过 cast 里有密码)。

## 3. 实测事实纠正(避免按错误假设实施)

- **canvas renderer addon 不存在**。canvas 是 xterm 核心内部渲染器,5.x→6.0 已删;5.5 的正确
  组合就是 webgl + DOM 回退(现状正确,ttyd 报告的建议作废)。
- **5.5 无 bellStyle/bellSound**——声音必须面板侧生成(见共识 3)。
- **addon 兼容性实测表**(与 vendored xterm-5.5.0 逐 `_core` 内部引用比对):search 0.16.0 ✓、
  clipboard 0.2.0 ✓、serialize 0.14.0 ✓(`_inputHandler`/`_themeService` 存在且有守卫)、
  progress 0.2.0 ✓;全部纯 JS 零 wasm。unicode-graphemes 0.4.0 ✓ 但 56.6KB(unicode11 的
  4.7 倍)——defer。image/ligatures/web-fonts——reject。
- **getOption/setOption 在 5.5 已移除**,只有 `options.*` 活存取(实测,字符串不在 bundle 里)。
- **Unicode11 顺序**:swiss 现有代码先 loadAddon 再设 activeVersion,正确(VS Code 警告的反例)。
- **Shift+Tab 必须 preventDefault 但返回 true**(键要到达终端,否则浏览器焦点逃逸,VS Code
  issue #188329);Alt+F4(Windows)放行关闭。
- **xterm onScroll 不触发于用户滚轮/按键**(xterm.js #3864/#3201)——pin 状态必须另建
  (tabby 的 capture 监听方案)。
- **ConPTY 吞 DCS**——shell 集成标记在 Windows 用 OSC(Warp 用 9278 的原因)。
- **搜索装饰需要 allowProposedApi: true**(swiss 已开)+ 高亮上限 finite(highlightLimit,
  VS Code 用 20000,默认 1000)。

## 4. 分期路线图

每期独立可交付、可验收;P0-P3 纯面板/纯前端(P2 的注入脚本是文本资产),P4 动后端。

### P0 — 手感包(零 vendor、零后端)

1. 标题同步 + 重命名优先级(共识 1;~20 行核心 + 标签渲染)
2. 响铃 → 标签徽章(+可选声音,共识 3)
3. pin-to-bottom + "N new lines ↓" 角标 + CSI 2026 回底(共识 10)
4. 标签快捷键 + 双击重命名(共识 5)
5. overlay 提示层(ttyd 73 行 helper:resize 显示 cols×rows、复制 ✂、重连状态——按面板色
   重制,无阴影)
6. 多行粘贴确认 + 粘贴规范化(共识 8)
7. 选中即复制(带 trim + 守卫,设置项,共识 4)
8. 小加固包:预启动输入队列(spawn 延迟期间排队,VS Code)、重连后重置终端模式
   (`ESC[?1000l…?2004l`,tabby,防追赶回放把模式泄漏成文字)、resize 去抖器(VS Code 95 行:
   小缓冲立即、列 100ms 单独去抖)、字体缩放后 `clearTextureAtlas()`(xterm.js)

验收:`test/admin-terminal.test.ts` 钉 terminal-core.js 纯函数;19998 浏览器实测(vim 退出后
回底、bell 徽章、Ctrl+Shift+F 前置 wiring 不在本期)。

### P1 — vendor 包(addon 落地 + 设置扩展)

1. `addon-search-0.16.0`(懒加载,Ctrl+Shift+F 打开即 import)——find 条 UI 按设计语言
   (顶部右侧浮层,Esc 关闭回焦)
2. `addon-clipboard-0.2.0` OSC 52(write-only provider + Wave 硬上限 + 复用粘贴代码路径)
3. `addon-progress-0.2.0`(1.4KB,OSC 9;4 → 标签页进度点;Windows 生态 clink/WinGet 原生发)
4. 终端设置 sheet 扩展:调色板(跟随面板 + ttyd 现行 + Campbell + 一套浅色)、光标样式/闪烁、
   滚回行数(默认 5000,1k-50k)、bell 模式、copyOnSelect;`term.options` 活应用 + localStorage
5. 每标签 shell 下拉(Profile;targets 候选 + MRU;开后端已支持)

验收:vitest + 19998(本地 pwsh/cmd/Git Bash 各开一标签;OSC 52 从 tunnels SSH 会话内 vim
复制到本机剪贴板)。

### P2 — shell 集成(拱心石)

1. OSC 633/133/7/9;9 解析模块(`parser.registerOscHandler`,同一 seam;handler 一律 return true)
2. 注入:pwsh(改自 VS Code MIT 脚本,nonce 模型)+ git-bash(`--init-file`);cmd/未知 shell
   **fail-open 回落到纯 133/直通**(Warp 同款分层);注入噪声用 bootstrap 阶段状态机挡在滚回外;
   **标记字节必须从 asciicast 录制流里滤掉**
3. UI:退出码徽章(130/141 语义)、cwd 进标签(OSC 7 兜底)、Ctrl+↑/↓ 命令导航
   (VS Code markNavigationAddon 的 ¼ 视口定位)、长命令(>50ms)完成通知
   (`document.visibilityState` 闪标签)
4. 命令历史:JSONL {ts, command, exit, pwd, session},封顶 2k 条 drop-oldest,懒加载;
   去重保最新(Warp 规则)。不上 sqlite。

验收:pwsh 会话徽章随命令红/绿;Ctrl+↑ 在滚回中按提示符跳;`exit 1` 后徽章红、`Ctrl-C` 中断
不红。

### P3 — 录制与恢复

1. `GET /api/terminal/recordings`(列表)+ 下载 + DELETE;面板"录制"区
2. 最小 cast 播放器(asciinema 引擎移植:JSONL 解析 ~40 行 + 自追调度 + 16ms 批写 +
   seek-by-replay;播放/暂停/倍速/进度条;不 port 其 wasm VT 与 solid-js UI)
3. 录制器头写 `idle_time_limit: 2`(一行后端);滚回转录下载按钮(bufferLinesToText 移植)
4. serialize 快照恢复(前端 5s idle 快照 + ptyoffset,POST 存盘,收养时精确恢复——替代
   转义尾重放的近似)
5. 录制脱敏(regex shapes 移植;先 warn 日志统计命中数,默认开)

验收:19998 上开→关→重开标签,滚回与颜色无损;cast 在面板内回放流畅;含 `export SECRET=…`
的会话录制文件里该值被 mask。

### P4 — 协议升级(动后端,逐项量 RSS 记 docs/01)

1. sshx chunk 环替换 64KB 一次性追赶:per-session `Vec<Bytes>` + (seqnum, chunk_offset,
   byte_offset) 三元组 + Subscribe{sid, chunk};客户端游标跨重连存活;环 ~128KB 内
   (留在 256KB/会话预算线内)
2. RTT 徽章(2s ping,中位数 10 样本,<80/<300ms 两档)
3. SSH 标签 typeahead(sshx 1961 行独立 fork,需一次性 tsc 编译成纯 JS vendor;延迟门控
   >50ms 才启用,本地标签自动休眠;先标题同步落地再做——它靠标题排除 vim/tmux)

## 5. 明确不做(九源共识 + docs/14 §11)

sixel/image、ligatures(需字体文件)、web-fonts、zmodem(缓:Windows 罕见 lrzsz)、serial
(原生绑定)、分屏(缓:最大单项,独立评估)、协作/E2E 加密/Redis mesh(单用户回环无意义)、
AI 功能(留 seam:记录结构里加可选 opaque metadata 字段即可)、桌面 chrome(托盘/quake/
触摸栏/更新器)、二维码/无限画布 UX。

## 6. 实施纪律

- 面板改动全部在 `../local-mcp-gateway/src/admin` 做,纯逻辑进 terminal-core.js 并由
  `test/admin-terminal.test.ts` 钉死;vitest 过后 `recopy-panel.ps1` 整树复制,cargo 门禁,
  19998 实测(swiss-live-verify),最后才 deploy.ps1 上 19999。
- P0-P3 不动后端适配器与依赖,不量内存;P4 每项按 swiss-memory-record 量边际 RSS 记 docs/01。
- 新特性默认开(响铃徽章、粘贴确认、脱敏);显式关闭手段保留在设置。
- vendor addon 一律 npm dist(`lib/*.js` UMD)+ 既有 index.js 剥壳模式,目录名带版本号;
  **绝不**从 xterm.js 仓库源码树构建(那是 6.0-dev,内部 API 不兼容)。