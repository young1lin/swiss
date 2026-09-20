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
    expect(tr("Tools")).toBe("工具");
    expect(tr("Logs")).toBe("日志");
    expect(tr("Copy Claude Code command")).toBe("复制 Claude Code 命令");
  });

  it("fills placeholders and keeps counts where the English key has them", () => {
    expect(tr("{n} matching", { n: 5 })).toBe("5 个匹配");
    expect(tr("{n} MCPs · {up} up · {bad} down", { n: 8, up: 6, bad: 2 })).toBe("8 MCP · 6 运行中 · 2 已停止");
    expect(tr("Loading {page}…", { page: tr("Jobs") })).toBe("正在加载 任务…");
  });

  it("routes zh plurals through the single other form", () => {
    // Chinese has no "one" category: both counts must land on the same entry.
    expect(trn(1, "{n} job · {on} on", "{n} jobs · {on} on", { on: 1 })).toBe("1 个任务 · 1 个启用");
    expect(trn(3, "{n} job · {on} on", "{n} jobs · {on} on", { on: 2 })).toBe("3 个任务 · 2 个启用");
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

  it("speaks the plugins row's requirements and off markers", async () => {
    const { requiresBadge, rowNode } = await import("../src/views/plugins.js");
    const met = requiresBadge({ requires: ["connection-catalog"], requiresMet: true } as never);
    expect(Array.isArray(met) ? met.join("") : met).toContain("· 依赖 connection-catalog");
    const unmet = requiresBadge({ requires: ["a", "b"], requiresMet: false } as never) as (string | HTMLElement)[];
    expect(unmet.join("")).toContain("· 需要 a, b ");
    const warn = unmet.find((n) => { return typeof n !== "string"; }) as HTMLElement;
    expect(warn.textContent).toBe("(无提供方)");
    const row = rowNode({ id: "mcp", label: "MCP", enabled: false, state: "active", pages: [] } as never);
    expect(row.textContent).toContain("· 已停用");
    expect(row.textContent).toContain("· 无页面");
  });

  it("names the start-at-sign-in row and its toggle in Chinese", async () => {
    const { startupRowNode } = await import("../src/views/plugins.js");
    const row = startupRowNode({ enabled: false, detail: "HKCU\\...\\Run" })!;
    expect(row.textContent).toContain("登录时启动 swiss");
    expect(row.textContent).toContain("· 已停用");
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
  });

  it("routes the secrets count through the zh other form", () => {
    expect(trn(1, "{n} secret", "{n} secrets")).toBe("1 个密钥");
    expect(trn(3, "{n} secret", "{n} secrets")).toBe("3 个密钥");
  });

  it("fills the secret delete confirm with the name twice (text and reference)", () => {
    const key = 'Delete secret "{name}"? Everything referencing ${secret://{name}} starts failing until it is re-stored.';
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

  it("builds the traffic meta skeleton with Chinese ok/err", async () => {
    await loadLocale();
    const { trafficRowNode } = await import("../src/traffic.js");
    const row = trafficRowNode({
      seq: 1, method: "tools/list", params: "{}", ok: true, ms: 5,
      clientName: "claude-code", clientVersion: "1.0", mcp: "mcp", at: new Date().toISOString(),
    } as never) as HTMLElement;
    expect(row.textContent).toContain("claude-code 1.0  ·  /mcp  ·  成功  ·  5ms");
    const bad = trafficRowNode({
      seq: 2, method: "tools/call", params: "", ok: false, ms: 12, mcp: "mcp", at: new Date().toISOString(),
    } as never) as HTMLElement;
    expect(bad.textContent).toContain("—  ·  /mcp  ·  失败  ·  12ms");
  });

  it("composes the activity count with grouped numbers", async () => {
    await loadLocale();
    expect(tr("{n} of {all} interactions", { n: "12", all: "1,024" })).toBe("12 / 1,024 次交互");
    expect(trn(3, "{n} interaction", "{n} interactions", { n: "3" })).toBe("3 次交互");
    expect(tr("Page {n}", { n: 2 })).toBe("第 2 页");
    expect(tr("Newer")).toBe("较新");
  });

  it("carries the token page's row vocabulary and confirm", async () => {
    await loadLocale();
    expect(tr("copies use this")).toBe("复制时使用");
    expect(tr("Rotate or revoke")).toBe("轮换或吊销");
    expect(tr("Revoke this token? Clients using it stop working immediately.")).toBe(
      "吊销该令牌?使用它的客户端会立即停止工作。",
    );
    expect(tr("id {id}", { id: "t_9" })).toBe("id t_9");
    expect(trn(2, "{n} token", "{n} tokens")).toBe("2 个令牌");
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
    expect(tr("{verb} → {state}", { verb: tr("enable"), state: "started" })).toBe("启用 → started");
    expect(tr("{verb} failed: {error}", { verb: tr("disable"), error: "boom" })).toBe("停用 失败:boom");
    expect(tr("{name}: {msg}", { name: "redis", msg: tr("config saved → restarted") })).toBe("redis: 配置已保存 → 已重启");
  });

  it("formats char counts and the call meta skeleton in Chinese", async () => {
    await loadLocale();
    const { fmtChars, callNode } = await import("../src/logs.js");
    expect(fmtChars(500)).toBe("500 字符");
    expect(fmtChars(1500)).toBe("1.5k 字符");
    const call = callNode({ callsOpen: {}, callsFull: {} } as never, {
      seq: 7, at: "2026-02-03T04:05:06Z", via: "redis", client: "cc", ms: 3, chars: 120,
      tool: "GET", args: "k", ok: true, output: "1", preview: false,
    } as never) as HTMLElement;
    expect(call.textContent).toContain("  ·  redis  ·  cc  ·  3 ms  ·  120 字符");
    expect(call.textContent).toContain("参数");
    expect(call.textContent).toContain("结果");
  });

  it("fills the delete confirm and the run-form refusals", async () => {
    await loadLocale();
    expect(tr("Delete '{name}'?\n\nThis stops it and removes it permanently.", { name: "mysql" })).toBe(
      "删除“mysql”?\n\n这会停止它并永久移除。",
    );
    expect(tr("\u0060{k}\u0060 is required", { k: "sql" })).toBe("\u0060sql\u0060 为必填项");
    expect(tr("Not started — start it to list {kind}.", { kind: "resources" })).toBe(
      "尚未启动——启动后才能列出 resources。",
    );
  });

  it("reads the pager and the test-connection outcomes in Chinese", async () => {
    await loadLocale();
    expect(tr("Page {n} of {m}", { n: 2, m: 5 })).toBe("第 2 页,共 5 页");
    expect(tr("✓ connected — these values work ({ms} ms)", { ms: 42 })).toBe("✓ 已连接——这些值可用(42 毫秒)");
    expect(tr("Show full result ({chars})", { chars: "1.5k 字符" })).toBe("显示完整结果(1.5k 字符)");
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
    expect(tr("endpoint {state}", { state: "serving" })).toBe("端点 serving");
    expect(tr("Tunnels {state} · ", { state: "none" })).toBe("隧道 none · ");
    expect(tr("exit {n}", { n: 2 })).toBe("退出码 2");
    expect(tr("{n}ms", { n: 40 })).toBe("40 毫秒");
    expect(tr("{m}m {s}s", { m: 2, s: 5 })).toBe("2 分 5 秒");
  });

  it("speaks the budget line and the count chips", async () => {
    await loadLocale();
    expect(trn(3, "{n} run recorded · {bytes}", "{n} runs recorded · {bytes}", { bytes: "1.5 KB" })).toBe("已记录 3 次运行 · 1.5 KB");
    expect(tr(" of {cap} · kept {d} days", { cap: "500 MB", d: 30 })).toBe(" 上限 500 MB · 保留 30 天");
    expect(trn(4, "{n} target", "{n} targets")).toBe("4 个目标");
    expect(trn(2, "{n} run", "{n} runs")).toBe("2 次运行");
  });

  it("fills the remote sheet's labels and the delete confirm", async () => {
    await loadLocale();
    expect(tr("Workspace root (absolute POSIX path)")).toBe("工作区根目录(绝对 POSIX 路径)");
    expect(tr("Alias (the name commands call: swiss remote exec <alias>)")).toBe(
      "别名(命令调用所用的名字:swiss remote exec <alias>)",
    );
    expect(tr("Delete target {id}? The Tunnels connection and any files on the machine are not touched.", { id: "dev" })).toBe(
      "删除目标 dev?隧道连接和机器上的文件都不会受影响。",
    );
    expect(tr("No output was produced.")).toBe("没有产生任何输出。");
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
    expect(tr("{names} depend on this tunnel.\n\nStop it anyway?", { names: "redis, mysql" })).toBe(
      "redis, mysql 依赖这条隧道。\n\n仍要停止吗?",
    );
    expect(tr("Connected in {ms} ms — {banner}", { ms: 42, banner: "SSH-2.0-OpenSSH_9" })).toBe(
      "已在 42 毫秒内连上——SSH-2.0-OpenSSH_9",
    );
    expect(tr("{error} ({kind})", { error: "connection refused", kind: "io" })).toBe("connection refused(io)");
    expect(tr("Saved {name}", { name: "开发机" })).toBe("已保存 开发机");
    expect(tr(" ({state})", { state: "up" })).toBe("(up)");
  });

  it("speaks the sheets' field copy", async () => {
    await loadLocale();
    expect(tr("Passphrase (optional)")).toBe("口令(可选)");
    expect(tr("key — private key file")).toBe("key——私钥文件");
    expect(tr("Serves MCPs")).toBe("服务于 MCP");
    expect(tr("New SSH connection in {group}", { group: "default" })).toBe("在 default 中新建 SSH 连接");
    expect(tr("Defaults to ")).toBe("默认使用 ");
    expect(tr(" when left empty.")).toBe("(留空时)。");
  });

  it("counts rules and connections", async () => {
    await loadLocale();
    expect(trn(2, "{n} rule", "{n} rules")).toBe("2 条规则");
    expect(trn(3, "{n} connection", "{n} connections")).toBe("3 条连接");
    expect(trn(2, "{n} rule", "{n} rules") + tr(", {n} active", { n: 1 })).toBe("2 条规则,1 个活动");
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
    expect(tr("Every {n} {unit}.", { n: 2, unit: tr("hours") })).toBe("每 2 小时。");
    expect(tr("the interval needs a number of {unit} (at least 1)", { unit: tr("minutes") })).toBe(
      "间隔需要 分钟 的数量(至少 1)",
    );
    expect(tr("the time must be HH:MM (24-hour)")).toBe("时间必须是 HH:MM(24 小时制)");
    expect(tr("weekly needs at least one day picked")).toBe("每周需要至少选择一天");
    expect(trn(3, "{n} value", "{n} values")).toBe("3 项");
    expect(tr("Saved revisions ({n})", { n: 2 })).toBe("已保存的修订(2)");
  });

  it("keeps the host's run words raw around translated frames", async () => {
    await loadLocale();
    expect(tr("{name}: {state}", { name: "backup", state: "succeeded" })).toBe("backup:succeeded");
    expect(tr("Runs — {name}", { name: "backup" })).toBe("运行 —— backup");
    expect(tr("attempt {a}/{b}", { a: 2, b: 3 })).toBe("第 2/3 次尝试");
    expect(tr("{n} more missed", { n: 4 })).toBe("另有 4 次未触发");
    expect(tr("({outcome} — no output recorded)", { outcome: "missed" })).toBe("(missed —— 未记录输出)");
  });

  it("fills the run tab's own controls (the I4 runBtn mystery, solved)", async () => {
    await loadLocale();
    expect(tr("↺ Past runs ({n})", { n: 12 })).toBe("↺ 过往运行(12)");
    expect(tr("↺ No past runs")).toBe("↺ 没有过往运行");
    expect(tr("Running…")).toBe("运行中…");
    expect(tr("Run")).toBe("运行");
    expect(tr("Arguments")).toBe("参数");
    expect(tr("(no output)")).toBe("(无输出)");
  });
});
