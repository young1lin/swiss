/*
 * Copyright 2026 The swiss authors
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */


/* The Chinese dictionary (docs/38 L3): one flat table, English source strings as keys,
   section comments naming the area the keys below belong to. A plural's entry is the
   "other" form only — Chinese has no "one" category under Intl.PluralRules("zh-CN"), so
   trn() never looks for one. Values carry {name} placeholders exactly where the English
   key does. The completeness scanner (test/i18n-complete.test.ts) holds both directions
   honest: every key used, no orphan entries, no value equal to its key. */

const zh: Record<string, string> = {
  /* --- chrome (index.html, main.ts) --- */
  "Search": "搜索",
  "Filter MCPs": "筛选 MCP",
  "New group": "新建分组",
  "Appearance": "外观",
  "Switch to dark": "切换到深色",
  "Switch to light": "切换到浅色",
  "Focus mode — hide app navigation (Esc exits)": "专注模式——隐藏应用导航(Esc 退出)",
  "Focus mode": "专注模式",
  "Language": "语言",
  "MCPs": "MCP",
  "Hosted MCPs": "托管的 MCP",
  "Plugins": "插件",
  "swiss resident set": "swiss 常驻内存",
  "mem …": "内存 …",
  "A new panel version is ready — it will load once you finish editing": "面板新版本已就绪——完成编辑后会自动加载",

  /* --- shell: page registry, rail, palette (page-registry.ts, plugin-palette.ts) --- */  "Token": "Token", // brand name, identical in zh (scanner PASSTHROUGH)
  "MCP": "MCP", // product name, identical in zh (scanner PASSTHROUGH)
  "{label} — {error}": "{label} — {error}", // em-dash skeleton, identical in zh (scanner PASSTHROUGH)

  "Traffic": "流量",
  "SSH Connections": "SSH 连接",
  "Port Forwards": "端口转发",
  "Data": "数据",
  "Jobs": "任务",
  "Secrets": "密钥",
  "System": "系统",
  "Tunnels": "隧道",
  "Settings": "设置",
  "More": "更多",
  "All plugins": "所有插件",
  "Switch {label} page": "切换 {label} 页面",
  " · off": " · 已停用",
  "· off": "· 已停用",
  "Plugin disabled": "插件已停用",
  "Loading {page}…": "正在加载 {page}…",
  "{page} unavailable": "{page} 不可用",
  "This plugin is disabled. Manage it in Plugins.": "该插件已停用。请在插件页管理。",
  "Cannot load plugin inventory: HTTP {status}": "无法加载插件清单:HTTP {status}",
  "Pinned": "已固定",
  "Search plugins…": "搜索插件…",
  "Search plugins": "搜索插件",
  "No plugins match.": "没有匹配的插件。",
  "Open {label}": "打开 {label}",
  "Unpin {label}": "取消固定 {label}",
  "Pin {label}": "固定 {label}",
  "Remove from the rail": "从导航栏移除",
  "Pin to the rail": "固定到导航栏",

  /* --- shell: page-core registry errors --- */
  "Invalid page id": "无效的页面 id",
  "Page label is required": "缺少页面标签",
  "Page entry must be a local admin module": "页面入口必须是本地 admin 模块",
  "Pages must be an array": "pages 必须是数组",
  "Duplicate page: {id}": "页面重复:{id}",
  "Unknown page: {id}": "未知页面:{id}",

  /* --- shell: pane.ts (detail header, tabs, empty states, client menu) --- */
  "Tools": "工具",
  "Resources": "资源",
  "Prompts": "提示",
  "Run": "运行",
  "Config": "配置",
  "Logs": "日志",
  "Select an MCP": "选择一个 MCP",
  "Its tools, resources and configuration appear here.": "它的工具、资源和配置会显示在这里。",
  "No MCPs registered": "尚未注册 MCP",
  "Add one with the + on a group header.": "点击分组头上的 + 添加一个。",
  "Add an MCP": "添加 MCP",
  "Reauthorize": "重新授权",
  "Authorize": "授权",
  "Disable": "停用",
  "Enable": "启用",
  "More actions": "更多操作",
  "since {time}": "自 {time} 起",
  "oauth: {state}": "oauth:{state}",
  "disabled": "已停用",
  "Connect a client": "连接客户端",
  "Copy Claude Code command": "复制 Claude Code 命令",
  "Copy Codex command": "复制 Codex 命令",
  "Copy .mcp.json entry": "复制 .mcp.json 条目",
  "Copy endpoint URL": "复制端点 URL",
  "Group": "分组",
  "New group…": "新建分组…",
  "Restart": "重启",
  "Edit configuration…": "编辑配置…",
  "Rename…": "重命名…",
  "Delete": "删除",

  /* --- shell: menu.ts, util.ts dots and shared toasts --- */
  "idle — lazy: no child yet, wakes on the first request": "空闲——还没有子进程,首次请求时唤醒",
  "right-click for actions": "右键查看操作",
  "{n} MCPs · {up} up": "{n} MCP · {up} 运行中",
  "{n} MCPs · {up} up · {bad} down": "{n} MCP · {up} 运行中 · {bad} 已停止",
  "{n} matching": "{n} 个匹配",
  "up · {n} ms": "运行中 · {n} 毫秒",
  "up": "运行中",
  "idle — starts on first request": "空闲——首次请求时启动",
  "error: {reason}": "错误:{reason}",
  "error": "错误",
  "request failed — is the gateway running?": "请求失败——网关在运行吗?",

  /* --- shell: groups.ts, group-logic.ts --- */
  "Group {name} created": "已创建分组 {name}",
  "Add to {group}": "添加到 {group}",
  "Move, rename or delete this group": "移动、重命名或删除该分组",
  "Group actions": "分组操作",
  "Move up": "上移",
  "Move down": "下移",
  "Delete group": "删除分组",
  "Renamed {from} → {to}": "已重命名 {from} → {to}",
  "Renamed {from} → {to} ({n} moved)": "已重命名 {from} → {to}(移动了 {n} 项)",
  "At least one group must remain": "至少要保留一个分组",
  "Deleted group {name}": "已删除分组 {name}",
  "Delete group '{name}'?\n\nIts {n} {noun}s move to '{sink}'. Nothing is removed.": "删除分组“{name}”吗?\n\n其中的 {n} 个{noun}会移动到“{sink}”。不会删除任何内容。",
  "New {noun} in {group}": "在 {group} 中新建{noun}",
  "No items — drop here or press +": "暂无条目——拖到这里或按 +",
  "No items": "暂无条目",

  /* --- shell: add-sheet.ts --- */
  "Name": "名称",
  "Type": "类型",
  "Start it now": "立即启动",
  "Import .mcp.json": "导入 .mcp.json",
  "Cancel": "取消",
  "Test connection": "测试连接",
  "Add": "添加",
  "Rename group": "重命名分组",
  "Create": "创建",
  "Rename": "重命名",
  "Name is required": "必须填写名称",
  "Command is required": "必须填写命令",
  "Could not read file": "无法读取文件",
  "Not valid JSON": "不是有效的 JSON",
  "Imported {n}, skipped {s}": "已导入 {n} 个,跳过 {s} 个",
  "Imported {n}": "已导入 {n} 个",
  "Added {name} ({state})": "已添加 {name}({state})",

  /* --- shell: connect.ts --- */
  "{label} copied — the token is embedded.": "已复制 {label}——令牌已嵌入。",
  "{label} copied": "已复制 {label}",
  "Claude Code command": "Claude Code 命令",
  "Codex block": "Codex 配置块",
  ".mcp.json entry": ".mcp.json 条目",
  "Endpoint URL": "端点 URL",

  /* --- shell: immersive.ts --- */
  "Exit Terminal fullscreen (Esc)": "退出终端全屏(Esc)",
  "Exit focus mode (Esc)": "退出专注模式(Esc)",
  "Terminal fullscreen — fill the Swiss window (Esc exits)": "终端全屏——填满 Swiss 窗口(Esc 退出)",
  "Exit Terminal fullscreen": "退出终端全屏",
  "Exit focus mode": "退出专注模式",
  "Terminal fullscreen": "终端全屏",

  /* --- shell: polling.ts (memory chip, job/rule/conn rows) --- */  "HTTP {n}": "HTTP {n}", // unit/code form, identical in zh (scanner PASSTHROUGH)
  "{n} MB": "{n} MB", // unit form, identical in zh (scanner PASSTHROUGH)
  "· {user} · {auth}": "· {user} · {auth}", // middle-dot skeleton, identical in zh (scanner PASSTHROUGH)

  "{n} MB · {p} procs": "{n} MB · {p} 个进程",
  "gateway RSS {n} MB (heap {a}/{b} MB, external {c} MB)": "网关 RSS {n} MB(堆 {a}/{b} MB,外部 {c} MB)",
  "proc-MCP children {n} MB across {p} processes": "proc MCP 子进程 {n} MB,共 {p} 个进程",
  "Child processes could not be measured — click to retry.": "子进程无法测量——点击重试。",
  "Click — refresh the memory reading. Press r — refresh memory and the current view.": "单击——刷新内存读数。按 r——刷新内存和当前视图。",
  "Run now": "立即运行",
  "Row actions": "行操作",
  "· last {when}": "· 上次 {when}",
  "· last {when} · failed": "· 上次 {when} · 失败",
  "· next {when}": "· 下次 {when}",
  "{n} jobs · {on} on": "{n} 个任务 · {on} 个启用",
  "{n} jobs · {on} on · {bad} failing": "{n} 个任务 · {on} 个启用 · {bad} 个失败",
  "via {conn}": "经 {conn}",
  "· serves ": "· 服务于 ",
  "proxy": "代理",
  "no MCP named {name}": "没有名为 {name} 的 MCP",
  "MCP {name} is {state}": "MCP {name} 的状态是 {state}",
  "Stop": "停止",
  "Start": "启动",
  "Test": "测试",
  "· {user} · {auth} · {n} rules": "· {user} · {auth} · {n} 条规则",

  /* --- fields.ts: labels, hints, placeholders --- */  "Base URL": "Base URL", // product vocabulary, identical in zh (scanner PASSTHROUGH)

  "Description": "描述",
  "What this MCP is for — e.g. the order service's Redis, used by the checkout backend": "这个 MCP 的用途——例如:订单服务的 Redis,由结算后端使用",
  "Describes the MCP itself. Sent to clients as the server's instructions, together with the connection target.": "描述 MCP 本身。作为服务器说明随连接目标一起发送给客户端。",
  "Start automatically at boot": "开机自动启动",
  "Off by default: the first client request spawns the child, and an idle one is reaped after 10 min (idleMs tunes). Adding it here still starts it now.": "默认关闭:首个客户端请求会启动子进程,空闲 10 分钟后回收(可用 idleMs 调整)。在这里添加后仍会立即启动。",
  "Off: idle at boot — the first client request starts it.": "关闭:启动时空闲——首个客户端请求会启动它。",
  "Command": "命令",
  "Working directory": "工作目录",
  "Environment (KEY=VALUE per line)": "环境变量(每行一个 KEY=VALUE)",
  "Expose resources": "暴露资源",
  "Turn off for a child that publishes thousands of resources.": "子进程发布数千个资源时请关闭。",
  "Expose prompts": "暴露提示",
  "Call timeout (ms)": "调用超时(毫秒)",
  "How long one tool call may run. Raise it for slow work — image analysis and long scrapes routinely pass a minute.": "单次工具调用允许运行的时间。慢任务请调大——图像分析和长抓取经常超过一分钟。",
  "Host": "主机",
  "Port": "端口",
  "User": "用户",
  "Password": "密码",
  "Database": "数据库",
  "Timezone": "时区",
  "Default row limit": "默认行数上限",
  "DB index": "DB 序号",
  "Allow FLUSHALL / FLUSHDB": "允许 FLUSHALL / FLUSHDB",
  "Allow Lua (EVAL / FCALL)": "允许 Lua(EVAL / FCALL)",
  "A script is opaque to every other rule here — it can reach anything they refuse.": "脚本不受这里其他规则约束——它们拒绝的操作脚本都能做到。",
  "A literal, or a ${ENV_VAR} / ${secret://name} ref — the ref is stored, the value never is.": "字面量,或 ${ENV_VAR} / ${secret://name} 引用——存储的是引用,永不存值。",
  "Options (k=v per line)": "选项(每行一个 k=v)",
  "Headers (NAME=VALUE per line)": "请求头(每行一个 NAME=VALUE)",
  "Where the remote's API key goes. Prefer a reference — ${secret://name} (store it once on the Plugins page) or ${ENV_VAR} — so neither this panel nor managed.json ever holds the value.": "远程 API 密钥写在这里。优先使用引用——${secret://name}(在插件页存一次)或 ${ENV_VAR}——这样面板和 managed.json 都不会持有值。",
  "Prefer a reference — ${secret://name} (store it once on the Plugins page) or ${ENV_VAR} — so neither this panel nor managed.json ever holds the value.": "优先使用引用——${secret://name}(在插件页存一次)或 ${ENV_VAR}——这样面板和 managed.json 都不会持有值。",
  "OAuth authorization (the gateway owns the token)": "OAuth 授权(由网关管理令牌)",
  "For remote MCPs behind OAuth (Figma: https://mcp.figma.com/mcp). Save first, then Authorize once from the detail view — the gateway registers a client, opens the consent page and refreshes tokens itself.": "用于 OAuth 后面的远程 MCP(Figma:https://mcp.figma.com/mcp)。先保存,然后在详情视图点一次授权——网关会注册客户端、打开授权页并自动刷新令牌。",
  "OAuth client name": "OAuth 客户端名称",
  "empty = Claude Code": "留空 = Claude Code",
  "Figma's registration accepts only \"Claude Code\" or \"Codex\". Leave empty for the default.": "Figma 的注册只接受“Claude Code”或“Codex”。留空使用默认值。",
  "Proxy": "代理",
  "Route THIS MCP's requests through an HTTP(S) proxy — for an endpoint this machine cannot reach directly. Other MCPs are not affected; a ${ENV_VAR} reference works here too.": "把这个 MCP 的请求路由到 HTTP(S) 代理——用于本机无法直连的端点。不影响其他 MCP;这里也可以用 ${ENV_VAR} 引用。",
  "Route THIS MCP's requests through an HTTP(S) proxy — for an endpoint this machine cannot reach directly.": "把这个 MCP 的请求路由到 HTTP(S) 代理——用于本机无法直连的端点。",
  "Route THIS MCP's requests through an HTTP(S) proxy — for an API this machine cannot reach directly. Other MCPs are not affected.": "把这个 MCP 的请求路由到 HTTP(S) 代理——用于本机无法直连的 API。不影响其他 MCP。",
  "API key": "API 密钥",
  "A ${ENV_VAR} or ${secret://name} reference — the server refuses a literal key. Store it once on the Plugins page or in the sealed env store.": "${ENV_VAR} 或 ${secret://name} 引用——服务器拒绝字面量密钥。在插件页或密封环境变量存储中存一次。",
  "Mode": "模式",
  "ZHIPU = open.bigmodel.cn (default) · ZAI = api.z.ai international.": "ZHIPU = open.bigmodel.cn(默认)· ZAI = api.z.ai 国际站。",
  "Model": "模型",
  "Optional — the default is the model the deployed upstream used.": "可选——默认是部署上游所用的模型。",
  "Base URL override": "Base URL 覆盖",
  "optional — a self-hosted GLM endpoint": "可选——自托管的 GLM 端点",
  "Leave empty to use the mode's official endpoint.": "留空使用该模式的官方端点。",
  "Timeout (ms)": "超时(毫秒)",
  "Vision generations are slow; the upstream default is 300 s.": "视觉生成较慢;上游默认 300 秒。",
  "Tool declarations (JSON)": "工具声明(JSON)",
  "The request block is the vendor's own example with {{arg}} in the slots you want the model to fill. Note {{arg}} for tool arguments — ${VAR} means an environment variable.": "request 块照抄厂商示例,在想让模型填写的位置写 {{arg}}。注意 {{arg}} 是工具参数——${VAR} 才是环境变量。",

  /* --- fields.ts: type labels --- */
  "proc — spawn a command and proxy it": "proc——启动命令并代理",
  "mysql — in-process driver": "mysql——进程内驱动",
  "redis — in-process driver": "redis——进程内驱动",
  "postgres — in-process driver": "postgres——进程内驱动",
  "http — proxy a remote MCP endpoint": "http——代理远程 MCP 端点",
  "figma — Figma's official remote MCP (OAuth fully managed, authorize once)": "figma——Figma 官方远程 MCP(OAuth 全托管,只需授权一次)",
  "zai-vision — Zhipu GLM vision tools, compiled in (replaces the Node child)": "zai-vision——智谱 GLM 视觉工具,原生内置(取代 Node 子进程)",
  "rest — declare tools over a plain HTTP API": "rest——在普通 HTTP API 上声明工具",

};

export default zh;