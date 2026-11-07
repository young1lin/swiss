// @vitest-environment happy-dom

import { beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { ApiTrafficClientRow, ApiTrafficPage } from "../src/types/api.js";

const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src/admin_assets/index.html"), "utf8");
const shellIds = Array.from(new Set(Array.from(shell.matchAll(/id="([a-zA-Z0-9_-]+)"/g), m => m[1])));

let traffic: typeof import("../src/traffic.js");
let page: ApiTrafficPage & { clients: ApiTrafficClientRow[] };
let requests: { path: string; method: string }[];
const settle = async (): Promise<void> => { await vi.waitFor(() => expect(document.querySelector("#trClient")).not.toBeNull()); };
const clientSelect = (): HTMLSelectElement => document.getElementById("trClient") as HTMLSelectElement;
const choose = (value: string): void => {
  clientSelect().value = value;
  clientSelect().dispatchEvent(new Event("change", { bubbles: true }));
};

beforeEach(async () => {
  vi.resetModules();
  document.body.innerHTML = shellIds.map(id => '<div id="' + id + '"></div>').join("");
  requests = [];
  page = {
    entries: [{ seq: 1, at: "2026-11-06T08:00:00.000Z", mcp: "echo", method: "tools/call", params: "echo", ok: true, ms: 4, hasResponse: true }],
    clients: [
      { key: "n:dsh/one", label: "dsh 1", tokens: ["test"], mcps: ["echo"], count: 2, lastAt: "2026-11-06T08:00:00.000Z", lastSeq: 2 },
      { key: "n:other", label: "<other> 1", tokens: [], mcps: ["echo"], count: 1, lastAt: "2026-11-06T07:00:00.000Z", lastSeq: 1 },
    ], total: 1, totalUnfiltered: 3, more: true, page: 0, pageSize: 50,
  };
  vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
    const method = init?.method || "GET";
    requests.push({ path: String(url), method });
    if (method === "DELETE") {
      page = { ...page, entries: [], total: 0, totalUnfiltered: 1, more: false,
        clients: page.clients.map(c => ({ ...c, count: c.key === "n:dsh/one" ? 0 : c.count })) };
    }
    return { ok: true, status: 200, json: async () => page } as Response;
  });
  traffic = await import("../src/traffic.js");
  (await import("../src/ui-state.js")).setCurrentView("traffic");
  await traffic.loadTraffic();
});

describe("Traffic uses the Remote Runs content-page filter", () => {
  it("puts the client dropdown and Actions/Everything in the page head, above one event list", () => {
    const head = document.querySelector("#pane .pane-head");
    expect(head?.querySelector(".pane-desc")?.textContent).toBe("Every MCP request, with its client, server and reply.");
    expect(head?.querySelector(".pane-sub")?.textContent).toBe("1 of 3 interactions");
    expect(head?.querySelector("#trClient")).toBe(clientSelect());
    expect(head?.querySelector("[data-filter=actions]")).not.toBeNull();
    expect(head?.querySelector("#trMore")).not.toBeNull();
    expect(clientSelect().getAttribute("aria-label")).toBe("Filter by client");
    expect(Array.from(clientSelect().options, o => [o.value, o.textContent])).toEqual([
      ["", "All clients"], ["n:dsh/one", "dsh 1 · 2"], ["n:other", "<other> 1 · 1"],
    ]);
    expect(document.querySelector(".cli-row, .cli-head, .sec-head, #pane h1")).toBeNull();
    expect(document.querySelector("#pane .tl-item")).not.toBeNull();
    expect(document.querySelector("#trClient other")).toBeNull();
  });

  it("filters on the server, resets paging, and All clients removes only the client filter", async () => {
    traffic.trafficPageStep(1);
    await vi.waitFor(() => expect(requests.at(-1)?.path).toContain("page=1"));
    choose("n:dsh/one");
    await vi.waitFor(() => expect(requests.at(-1)?.path).toBe("/api/traffic?page=0&actions=1&client=n%3Adsh%2Fone"));
    await settle();
    expect(clientSelect().value).toBe("n:dsh/one");
    (document.querySelector("[data-filter=all]") as HTMLElement).click();
    await vi.waitFor(() => expect(requests.at(-1)?.path).toBe("/api/traffic?page=0&client=n%3Adsh%2Fone"));
    choose("");
    await vi.waitFor(() => expect(requests.at(-1)?.path).toBe("/api/traffic?page=0"));
  });

  it("clears only the selected client and keeps its zero-count option", async () => {
    choose("n:dsh/one");
    await settle();
    (document.getElementById("trMore") as HTMLElement).click();
    const clear = Array.from(document.querySelectorAll<HTMLButtonElement>("#menu button")).find(b => b.textContent === "Clear client");
    expect(clear).toBeDefined();
    clear?.click();
    await vi.waitFor(() => expect(requests).toContainEqual({ path: "/api/traffic?client=n%3Adsh%2Fone", method: "DELETE" }));
    await vi.waitFor(() => expect(clientSelect().value).toBe(""));
    expect(Array.from(clientSelect().options, o => o.textContent)).toEqual(["All clients", "dsh 1 · 0", "<other> 1 · 1"]);
  });

  it("keeps keyboard focus on the client filter after its server response repaints the head", async () => {
    const before = clientSelect();
    before.focus();
    choose("n:dsh/one");
    await vi.waitFor(() => expect(clientSelect()).not.toBe(before));
    await vi.waitFor(() => expect(document.activeElement).toBe(clientSelect()));
    expect(clientSelect().value).toBe("n:dsh/one");
  });

  it("refreshes client counts even when an Actions-only page has no new action rows", async () => {
    page = { ...page, entries: [], total: 0, more: false };
    await traffic.loadTraffic();
    page = { ...page, totalUnfiltered: 4, clients: page.clients.map(c => ({ ...c, count: c.count + 1 })) };
    await traffic.loadTraffic();
    expect(clientSelect().options[1].textContent).toBe("dsh 1 · 3");
    expect(document.querySelector(".pane-sub")?.textContent).toBe("0 of 4 interactions");
  });

  it("keeps an open client picker in place during live updates, then catches up after it closes", async () => {
    const { styleSelect, closeSelect } = await import("../src/ui/select.js");
    const before = clientSelect();
    styleSelect(before);
    (before.nextElementSibling as HTMLButtonElement).click();
    expect(document.querySelector('[role="listbox"]')).not.toBeNull();
    page = { ...page, clients: page.clients.map(c => ({ ...c, count: c.count + 1 })) };
    await traffic.loadTraffic();
    expect(clientSelect()).toBe(before);
    expect(before.isConnected).toBe(true);
    closeSelect();
    await traffic.loadTraffic();
    expect(clientSelect().options[1].textContent).toBe("dsh 1 · 3");
  });

  it("localizes the dropdown and keeps the same controls when the archive is empty", async () => {
    const { install } = await import("../src/i18n.js");
    install("zh-CN", (await import("../src/locales/zh.js")).default);
    page = { ...page, entries: [], clients: [], total: 0, totalUnfiltered: 0, more: false };
    traffic.resetTrafficSig();
    await traffic.loadTraffic();
    expect(clientSelect().options[0].textContent).toBe("所有客户端");
    expect(clientSelect().getAttribute("aria-label")).toBe("按客户端筛选");
    expect(document.querySelector(".pane-desc")?.textContent).toBe("每次 MCP 请求及其客户端、服务和响应。");
    expect(document.querySelector("#pane .empty")).not.toBeNull();
  });
});
