/*
 * Copyright 2026 young1lin
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
// @vitest-environment happy-dom

/* The I1 shell sweep's Chinese direction (docs/38 stage I1): the same builders the
 * English tests pin must produce Chinese once the zh dictionary is installed — the
 * sentence-level group strings, the plural chip text, and the field form's labels.
 * This is the acceptance pair of admin-groups.test.ts, not a duplicate of it. */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { addTitle, deleteConfirmMsg, emptyLineText } from "../src/group-logic.js";
import { fieldsNode } from "../src/fields.js";
import { loadLocale, setLang, tr, trn } from "../src/i18n.js";
import en from "../src/locales/en.js";

describe("the shell in Chinese (stage I1)", () => {
  beforeEach(async () => {
    setLang("zh-CN");
    await loadLocale();
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("translates the pane's tab and menu vocabulary", () => {
    expect(tr("pane.tools")).toBe("工具");
    expect(tr("pane.logs")).toBe("日志");
    expect(tr("pane.copyClaudeCodeCommand")).toBe("复制 Claude Code 命令");
  });

  it("fills placeholders and keeps counts where the English key has them", () => {
    expect(tr("menu.nMatching", { n: 5 })).toBe("5 个匹配");
    expect(tr("polling.nMcpsBadDown", { n: 8, up: 6, bad: 2 })).toBe("8 MCP · 6 运行中 · 2 已停止");
    expect(tr("pageRegistry.loadingPage", { page: tr("pageRegistry.jobs") })).toBe("正在加载 任务…");
  });

  it("routes zh plurals through the single other form", () => {
    // Chinese has no "one" category: both counts must land on the same entry.
    expect(trn(1, "polling.nJobs.one", "polling.nJobs.other", { on: 1 })).toBe("1 个任务 · 1 个启用");
    expect(trn(3, "polling.nJobs.one", "polling.nJobs.other", { on: 2 })).toBe("3 个任务 · 2 个启用");
  });

  it("speaks the group delete confirm and create title as sentences", () => {
    expect(deleteConfirmMsg("Docs", ["default", "Docs"], 1, "MCP")).toBe(
      "删除分组“Docs”吗?\n\n其中的 1 个MCP会移动到“default”。不会删除任何内容。",
    );
    expect(addTitle("MCP", "learn")).toBe("在 learn 中新建MCP");
    expect(emptyLineText(true)).toBe("暂无条目——拖到这里或按 +");
  });

  it("paints the add form's field labels from the dictionary", () => {
    const nodes = fieldsNode("mysql", null, "x-");
    const labels = (nodes as HTMLElement[]).flatMap((n: HTMLElement) => {
      return Array.from(n.querySelectorAll("label.field > span:first-child"));
    }).map((s: Element) => { return s.textContent; });
    expect(labels).toEqual(["描述", "主机", "端口", "用户", "密码", "数据库", "时区", "默认行数上限"]);
    // The autostart control is a checkbox (label.check inside its own .fld), not a field row.
    const checks = (nodes as HTMLElement[]).flatMap((n: HTMLElement) => {
      return Array.from(n.querySelectorAll("label.check"));
    });
    expect(checks).toHaveLength(1);
    expect(checks[0].textContent).toContain("开机自动启动");
  });

  it("shows the type select's long labels in Chinese", async () => {
    const { TYPE_LABELS } = await import("../src/fields.js");
    expect(tr(TYPE_LABELS["zai-vision"])).toBe("zai-vision——智谱 GLM 视觉工具,原生内置(取代 Node 子进程)");
  // run-history splits the adapter label on the separator the LOCALE carries (" — " in
  // en, "——" in zh) to extract the kind tail, so both shapes must stay present in copy.
  const enLabel = en[TYPE_LABELS["zai-vision"]];
  expect(enLabel).toContain(" — ");
  expect(tr(TYPE_LABELS["zai-vision"])).toContain("——");
  });
});

describe("the I2 views in Chinese (plugins, secrets, system)", () => {
  beforeEach(async () => {
    setLang("zh-CN");
    await loadLocale();
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the plugins row's requirements and page count", async () => {
    const { requiresBadge, rowNode } = await import("../src/views/plugins.js");
    const met = requiresBadge({ requires: ["connection-catalog"], requiresMet: true } as never);
    expect(met).toBe("依赖 connection-catalog");
    const unmet = requiresBadge({ requires: ["a", "b"], requiresMet: false } as never) as (string | HTMLElement)[];
    expect(unmet.join("")).toContain("需要 a, b ");
    const warn = unmet.find((n) => { return typeof n !== "string"; }) as HTMLElement;
    expect(warn.textContent).toBe("无提供方");
    // The separators are the layout's, the words are the locale's (docs/46 P3).
    const off = rowNode({ id: "mcp", label: "MCP", enabled: false, state: "disabled", pages: [] } as never);
    expect(off.querySelector(".lrow-sub")?.textContent).toBe("mcp · 无页面");
    const three = rowNode({ id: "mcp", label: "MCP", enabled: true, state: "active", pages: ["a", "b", "c"] } as never);
    expect(three.querySelector(".lrow-sub")?.textContent).toBe("mcp · 3 个页面");
  });

  it("names the start-at-sign-in row and its toggle in Chinese", async () => {
    const { startupRowNode } = await import("../src/views/plugins.js");
    const row = startupRowNode({ enabled: false, detail: "HKCU\\...\\Run" })!;
    expect(row.textContent).toContain("登录时启动 swiss");
    expect((row.querySelector("[data-autostart-toggle]") as HTMLElement).getAttribute("aria-checked")).toBe("false");
    expect((row.querySelector("[data-autostart-toggle]") as HTMLElement).getAttribute("aria-label")).toBe("切换开机自启");
  });

  it("builds the quit sheet in Chinese", async () => {
    // system.js -> add-sheet.js wires #addBtn at import time: give it the shell skeleton,
    // every id the served index.html carries (the house shellSkeleton idiom).
    const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
    document.body.innerHTML = Array.from(new Set(Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1])))
      .map((id) => '<div id="' + id + '"></div>').join("");
    const { quitSheetNode } = await import("../src/views/system.js");
    const sheet = quitSheetNode();
    expect(sheet.querySelector("h2")!.textContent).toBe("退出 swiss?");
    const buttons = Array.from(sheet.querySelectorAll("button")).map((b) => { return b.textContent; });
    expect(buttons).toEqual(["取消", "退出 swiss"]);
    expect(sheet.textContent).toContain("这会断开所有 MCP 客户端");
    // The sentence ends in the locale, not in a "." the code appended (found on the docs/46 P5
    // walk: "需要时用 swiss start." - an English full stop and no verb).
    expect(sheet.querySelector(".hint")!.textContent).toBe("配置和日志会保留。需要时用 swiss start 重新启动。");
  });

  it("routes the secrets count through the zh other form", () => {
    expect(trn(1, "secrets.nSecrets.one", "secrets.nSecrets.other")).toBe("1 个密钥");
    expect(trn(3, "secrets.nSecrets.one", "secrets.nSecrets.other")).toBe("3 个密钥");
  });

  it("fills the secret delete confirm with the name twice (text and reference)", () => {
    const key = "secrets.deleteSecretNameEverything";
    expect(tr(key, { name: "db-pass" })).toBe(
      "删除密钥“db-pass”吗?所有引用 ${secret://db-pass} 的地方都会开始失败,直到重新存入。",
    );
  });
});

describe("the I3 views in Chinese (tokens, traffic)", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the relative-time family used across traffic and clients", async () => {
    await loadLocale();
    const { ago } = await import("../src/traffic.js");
    expect(ago(new Date(Date.now() - 1000).toISOString())).toBe("刚刚");
    expect(ago(new Date(Date.now() - 42000).toISOString())).toBe("42 秒前");
    expect(ago(new Date(Date.now() - 300000).toISOString())).toBe("5 分钟前");
  });

  it("builds a traffic row as an event-list item: who is client · MCP, a failure says 失败", async () => {
    await loadLocale();
    const { trafficItem, trafficBodyNode } = await import("../src/traffic.js");
    const ok = trafficItem({
      seq: 1, method: "tools/list", params: "{}", ok: true, ms: 5,
      clientName: "claude-code", clientVersion: "1.0", mcp: "mcp", at: new Date().toISOString(),
    } as never);
    expect(ok.who).toBe("claude-code 1.0 · mcp");
    expect(ok.status).toBeUndefined();
    const bad = trafficItem({
      seq: 2, method: "tools/call", params: "", ok: false, ms: 12, mcp: "mcp", at: new Date().toISOString(),
    } as never);
    expect(bad.who).toBe("— · mcp");
    expect(bad.status!.text).toBe("失败");
    const host = document.createElement("div");
    host.append(...([trafficBodyNode({ seq: 2, method: "tools/call", ok: false, ms: 12, mcp: "mcp", clientName: "cc", at: new Date().toISOString() } as never)].flat() as Node[]).filter(Boolean));
    expect(host.querySelector(".tl-meta")!.textContent).toBe("客户端 cc · /mcp/mcp");
  });

  it("composes the activity count with grouped numbers", async () => {
    await loadLocale();
    expect(tr("traffic.nAllInteractions", { n: "12", all: "1,024" })).toBe("12 / 1,024 次交互");
    expect(trn(3, "traffic.nInteractions.one", "traffic.nInteractions.other", { n: "3" })).toBe("3 次交互");
    expect(tr("remoteRuns.pageN", { n: 2 })).toBe("第 2 页");
    expect(tr("remoteRuns.newer")).toBe("较新");
  });

  it("carries the token page's row vocabulary and confirm", async () => {
    await loadLocale();
    expect(tr("tokens.copiesUse")).toBe("复制时使用");
    expect(tr("tokens.descOneLine")).toBe("每个客户端一个令牌;密钥只在创建或轮换时显示一次。");
    expect(tr("tokens.revokeTokenClientsUsing")).toBe(
      "吊销该令牌?使用它的客户端会立即停止工作。",
    );
    expect(trn(2, "tokens.nTokens.one", "tokens.nTokens.other")).toBe("2 个令牌");
  });
});

describe("the I4 modules in Chinese (detail, logs, run)", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("composes the lifecycle action note with a translated verb and a raw state word", async () => {
    await loadLocale();
    expect(tr("detail.verbState", { verb: tr("detail.enable"), state: "started" })).toBe("启用 → started");
    expect(tr("detail.verbFailedError", { verb: tr("detail.disable"), error: "boom" })).toBe("停用 失败:boom");
    expect(tr("detail.nameMsg", { name: "redis", msg: tr("detail.configSavedRestarted") })).toBe("redis: 配置已保存 → 已重启");
  });

  it("formats char counts and an open call's meta and blocks in Chinese", async () => {
    await loadLocale();
    const { fmtChars, callBodyNode, callItem } = await import("../src/logs.js");
    expect(fmtChars(500)).toBe("500 字符");
    expect(fmtChars(1500)).toBe("1.5k 字符");
    const row = {
      seq: 7, at: "2026-02-03T04:05:06Z", via: "redis", client: "cc", ms: 3, chars: 120,
      tool: "GET", args: "k", ok: false, output: "1", preview: false,
    };
    const host = document.createElement("div");
    host.append(...(callBodyNode({ callsOpen: {}, callsFull: {} } as never, [row, { ...row, seq: 6 }] as never) as HTMLElement[]).filter(Boolean));
    const metas = [...host.querySelectorAll(".tl-meta")].map((n) => n.textContent);
    expect(metas[0]).toBe("经 redis · 客户端 cc · 回复 120 字符");
    expect(metas[1]).toContain("2 次相同的调用");
    expect(host.textContent).toContain("参数");
    expect(host.textContent).toContain("错误");
    expect(host.querySelector("[data-blkmore]")!.getAttribute("aria-label")).toBe("参数的更多操作");
    expect(callItem(row as never).status!.text).toBe("失败");
  });

  it("fills the delete confirm and the run-form refusals", async () => {
    await loadLocale();
    expect(tr("detail.deleteNameStopsRemovesPermanently", { name: "mysql" })).toBe(
      "删除“mysql”?\n\n这会停止它并永久移除。",
    );
    expect(tr("run.kRequired", { k: "sql" })).toBe("\u0060sql\u0060 为必填项");
    expect(tr("logs.startedStartListKind", { kind: "resources" })).toBe(
      "尚未启动——启动后才能列出 resources。",
    );
  });

  it("reads the pager and the test-connection outcomes in Chinese", async () => {
    await loadLocale();
    expect(tr("logs.pageNM", { n: 2, m: 5 })).toBe("第 2 页,共 5 页");
    expect(tr("detail.connectedTheseValuesWorkMsMs", { ms: 42 })).toBe("✓ 已连接——这些值可用(42 毫秒)");
    expect(tr("logs.showFullResultChars", { chars: "1.5k 字符" })).toBe("显示完整结果(1.5k 字符)");
  });
});

describe("the I5 views in Chinese (remote targets, remote runs)", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("keeps the host's state words raw around translated frames", async () => {
    await loadLocale();
    expect(tr("remote.endpointState", { state: "serving" })).toBe("端点 serving");
    expect(tr("remote.tunnelsState", { state: "none" })).toBe("隧道 none · ");
    expect(tr("remoteRuns.exitN", { n: 2 })).toBe("退出码 2");
    // docs/46 P6-2: durations are the timeline's (ui.ms / ui.sec); the Targets column and the
    // folded-run line frame the host's words the same way.
    expect(tr("remote.lastRunAt", { when: "2026/9/24 09:12:03", what: "make -j8" })).toBe("上次运行：2026/9/24 09:12:03 · make -j8");
    expect(tr("remoteRuns.nIdenticalRuns", { n: 3 })).toBe("3 次相同的运行");
    expect(tr("remoteRuns.cappedAtLast", { cap: "16.0 MB", tail: "15 B" })).toBe("输出在 16.0 MB 处截断；这是最后 15 B");
  });

  it("speaks the budget line and the count chips", async () => {
    await loadLocale();
    expect(trn(3, "remoteRuns.nRunsRecordedBytes.one", "remoteRuns.nRunsRecordedBytes.other", { bytes: "1.5 KB" })).toBe("已记录 3 次运行 · 1.5 KB");
    expect(tr("remoteRuns.capKeptDDays", { cap: "500 MB", d: 30 })).toBe(" 上限 500 MB · 保留 30 天");
    expect(trn(4, "remote.nTargets.one", "remote.nTargets.other")).toBe("4 个目标");
    expect(trn(2, "remoteRuns.nRuns.one", "remoteRuns.nRuns.other")).toBe("2 次运行");
  });

  it("fills the remote sheet's labels and the delete confirm", async () => {
    await loadLocale();
    expect(tr("remote.workspaceRootAbsolutePosix")).toBe("工作区根目录(绝对 POSIX 路径)");
    expect(tr("remote.alias")).toBe("别名");
    expect(tr("remote.aliasHint")).toBe("命令调用所用的名字：swiss remote exec <alias>");
    expect(tr("remote.deleteTargetIdTunnels", { id: "dev" })).toBe(
      "删除目标 dev?隧道连接和机器上的文件都不会受影响。",
    );
    expect(tr("remoteRuns.outputProduced")).toBe("没有产生任何输出。");
  });
});

describe("the I6 tunnels pages in Chinese", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("frames the host's words: state, banner, error, kind", async () => {
    await loadLocale();
    expect(tr("tunnels.namesDependTunnelStop.other", { names: "redis, mysql" })).toBe(
      "redis, mysql 依赖这条隧道。\n\n仍要停止吗?",
    );
    expect(tr("tunnels.connectedMsMsBanner", { ms: 42, banner: "SSH-2.0-OpenSSH_9" })).toBe(
      "已在 42 毫秒内连上——SSH-2.0-OpenSSH_9",
    );
    expect(tr("tunnels.errorKind", { error: "connection refused", kind: "io" })).toBe("connection refused(io)");
    expect(tr("tunnelSheets.savedName", { name: "开发机" })).toBe("已保存 开发机");
    expect(tr("tunnelSheets.state", { state: "up" })).toBe("(up)");
  });

  it("speaks the sheets' field copy", async () => {
    await loadLocale();
    expect(tr("tunnelSheets.passphraseOptional")).toBe("口令(可选)");
    expect(tr("tunnelSheets.keyPrivateKeyFile")).toBe("key——私钥文件");
    expect(tr("tunnelSheets.servesMcps")).toBe("服务于 MCP");
    expect(tr("tunnelSheets.newSshConnectionGroup", { group: "default" })).toBe("在 default 中新建 SSH 连接");
    expect(tr("tunnelSheets.defaults")).toBe("默认使用 ");
    expect(tr("tunnelSheets.whenLeftEmpty")).toBe("(留空时)。");
  });

  it("counts rules and connections", async () => {
    await loadLocale();
    expect(trn(2, "tunnels.nRules.one", "tunnels.nRules.other")).toBe("2 条规则");
    expect(trn(3, "tunnels.nConnections.one", "tunnels.nConnections.other")).toBe("3 条连接");
    expect(trn(2, "tunnels.nRules.one", "tunnels.nRules.other") + tr("tunnels.nActive", { n: 1 })).toBe("2 条规则,1 个活动");
  });
});

describe("the I7 jobs machinery in Chinese", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the schedule builder and its refusals", async () => {
    await loadLocale();
    expect(tr("jobs.everyNUnit", { n: 2, unit: tr("jobs.hours") })).toBe("每 2 小时。");
    // The every-1 sentence singularises in English; zh carries the same word for both.
    expect(tr("jobs.everyNUnit", { n: 1, unit: tr("jobs.hour") })).toBe("每 1 小时。");
    expect(tr("jobs.intervalNeedsNumberUnit", { unit: tr("jobs.minutes") })).toBe(
      "间隔需要 分钟 的数量(至少 1)",
    );
    expect(tr("jobs.timeMustHhMm")).toBe("时间必须是 HH:MM(24 小时制)");
    expect(tr("jobs.weeklyNeedsLeastOne")).toBe("每周需要至少选择一天");
    expect(trn(3, "runHistory.nValues.one", "runHistory.nValues.other")).toBe("3 项");
    expect(tr("runHistory.savedRevisionsN", { n: 2 })).toBe("已保存的修订(2)");
  });

  it("keeps the host's run words raw around translated frames", async () => {
    await loadLocale();
    expect(tr("jobs.nameState", { name: "backup", state: "succeeded" })).toBe("backup:succeeded");
    expect(tr("jobs.runsName", { name: "backup" })).toBe("运行 —— backup");
    expect(tr("jobsV2.attemptB", { a: 2, b: 3 })).toBe("第 2/3 次尝试");
    expect(tr("jobsV2.nMoreMissed", { n: 4 })).toBe("另有 4 次未触发");
    expect(tr("jobs.outcomeOutputRecorded", { outcome: "missed" })).toBe("(missed —— 未记录输出)");
  });

  it("fills the run tab's own controls (the I4 runBtn mystery, solved)", async () => {
    await loadLocale();
    // fix-plan #14: the history glyph is the i-history sprite at the paint sites, so the
    // zh copies carry words only.
    expect(tr("runHistory.pastRunsN", { n: 12 })).toBe("过往运行(12)");
    expect(tr("runHistory.pastRuns2")).toBe("没有过往运行");
    expect(tr("runHistory.running")).toBe("运行中…");
    expect(tr("run.run")).toBe("运行");
    expect(tr("runHistory.arguments2")).toBe("参数");
    expect(tr("runHistory.output")).toBe("(无输出)");
  });
});

describe("the I8a data machinery in Chinese", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the commit bar's plurals and frames", async () => {
    await loadLocale();
    expect(trn(3, "dataSql.nUpdates.one", "dataSql.nUpdates.other")).toBe("3 个更新");
    expect(trn(1, "dataSql.nInserts.one", "dataSql.nInserts.other")).toBe("1 个插入");
    expect(tr("dataSql.partsLocalOnlyRedis", { parts: "3 个更新" })).toContain("仅在本地");
    expect(tr("dataSql.commitN1Transaction")).toBe("提交(1 个事务)");
    expect(tr("dataSql.discard")).toBe("放弃");
  });

  it("keeps the grid's data frames raw and groups counts the panel's way", async () => {
    await loadLocale();
    expect(tr("dataGrid.note", { note: "server said so" })).toBe(" · server said so");
    expect(tr("dataGrid.bT", { a: "1", b: "50", t: "1,234" })).toBe("1–50,共 1,234");
    expect(tr("dataGrid.nRows2", { n: "1,234" })).toBe("1,234 行");
    expect(tr("dataGrid.editableNote", { note: tr("dataGrid.rowsAddressedAllColumns") })).toBe("可编辑——行以所有列定位");
    expect(tr("dataGrid.exportedNRows", { n: "12,345" })).toBe("已导出 12,345 行");
  });

  it("names the ddl sheet's parts", async () => {
    await loadLocale();
    // dataView.newTable retired with the old list header's + (docs/43 M2): the Tables
    // band's button carries newTable2, which this section keeps asserting below.
    expect(tr("dataView.newTable2")).toBe("新建表…");
    expect(tr("dataDdl.newTableT2", { t: "public" })).toBe("在 public 中新建表");
    expect(tr("dataDdl.addColumnT", { t: "events" })).toBe("在 events 中添加列");
    expect(tr("dataDdl.sqlPreviewCommitRuns")).toBe("SQL 预览——提交将原样运行这些语句。");
    expect(tr("dataSql.typeCommandFirst")).toBe("请先输入命令");
  });
});

describe("the I8b data surfaces in Chinese", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the redis commit and discard guards", async () => {
    await loadLocale();
    expect(trn(3, "dataBrowsers.nCommands.one", "dataBrowsers.nCommands.other")).toBe("3 条命令");
    expect(tr("dataBrowsers.committedN", { n: trn(2, "dataBrowsers.nCommands.one", "dataBrowsers.nCommands.other") })).toBe("已提交 2 条命令");
    expect(tr("dataBrowsers.discardNNothingBeen", { n: trn(4, "dataBrowsers.nBufferedChanges.one", "dataBrowsers.nBufferedChanges.other") })).toContain("4 个缓冲的更改");
  });

  it("names the filter operators and the form stepper", async () => {
    await loadLocale();
    expect(tr("dataFilters.null")).toBe("为 NULL");
    expect(tr("dataFilters.list2")).toBe("不在列表中");
    expect(tr("dataFilters.shownShownTotalKeyspace", { shown: "10", total: "1,024" })).toBe("已显示 10 · 键空间共 1,024");
    expect(tr("dataForm.rowINPage", { i: 3, n: 50 })).toBe("本页第 3 条,共 50 条");
    expect(tr("dataForm.newRowIN", { i: 1, n: 2 })).toBe("新行 1/2 · 已缓冲");
  });

  it("fills the import and cell sheets' words", async () => {
    await loadLocale();
    expect(tr("dataCsv.importCsvIntoT", { t: "public.events" })).toBe("向 public.events 导入 CSV");
    expect(tr("dataCsv.insert")).toBe("插入");
    expect(tr("dataCsv.upsert")).toBe("插入或更新");
    expect(tr("dataCsv.mapLeastOneColumn")).toBe("请至少映射一列");
    expect(tr("dataCell.saveBuffer")).toBe("保存到缓冲");
    expect(tr("dataCell.pkPk", { pk: "id=7" })).toBe(" · 主键 id=7");
  });
});

describe("the I9 terminal surfaces in Chinese", () => {
  beforeEach(() => {
    setLang("zh-CN");
  });
  afterEach(() => {
    setLang("en");
    localStorage.clear();
  });

  it("speaks the shortcuts sheet", async () => {
    await loadLocale();
    expect(tr("terminal.terminalShortcuts")).toBe("终端快捷键");
    expect(tr("terminal.keys")).toBe("键");
    expect(tr("terminal.findSessionsBuffer")).toBe("在此会话的缓冲区中查找");
    expect(tr("terminal.asksFirstPasteWhole")).toBe("先询问——要么整段粘贴,要么不粘贴");
    expect(tr("terminal.closeEsc")).toBe("关闭(Esc)");
  });

  it("speaks the bar and the empty state", async () => {
    await loadLocale();
    expect(tr("terminal.openSession")).toBe("打开会话");
    expect(tr("terminal.localShellOffTurn")).toBe("本地 shell 已关闭——点此开启");
    expect(tr("terminal.shortcutsGestures")).toBe("快捷键与手势");
    expect(tr("terminal.session")).toBe("还没有会话");
    expect(tr("terminal.droppedSocketEndSession")).toContain("宽限窗口");
    expect(tr("terminal.closed")).toBe(" · 已关闭");
  });

  it("speaks the settings sheet and its save cost", async () => {
    await loadLocale();
    expect(tr("terminalSettings.localShell")).toBe("本地 shell");
    expect(tr("terminalSettings.emptyPlatformDefaultWhich", { which: "pwsh.exe" })).toContain("留空 = 平台默认(pwsh.exe)");
    expect(trn(3, "terminalSettings.nSessions.one", "terminalSettings.nSessions.other")).toBe("3 个会话");
    expect(tr("terminalSettings.savingRestartsTerminalPlugin", { n: trn(3, "terminalSettings.nSessions.one", "terminalSettings.nSessions.other") })).toBe("保存会重启终端插件并关闭 3 个会话。");
    expect(tr("terminalSettings.savedTerminalPluginRestarted")).toContain("已保存");
  });
});
