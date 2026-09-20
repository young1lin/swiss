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