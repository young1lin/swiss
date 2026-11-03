// @vitest-environment happy-dom
/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
/* SPEC §mcp.calls: the Run popover must address a recorded call by its public seq,
   show its input/output, and refill without executing it. An absent seq must never
   become /calls/NaN or leave the preview spinning forever. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setMcpDetail, setSelectedMcp } from "../src/mcp-state.js";
import type { McpDetail } from "../src/types/state.js";
import type { ApiMcpCallRow } from "../src/types/api.js";

const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src/admin_assets/index.html"), "utf8");
const ids = [...new Set([...shell.matchAll(/id="([a-zA-Z0-9_-]+)"/g)].map(m => m[1]))];
document.body.innerHTML = ids.map(id => '<div id="' + id + '"></div>').join("");
const history = await import("../src/run-history.js");
const requests: string[] = [];
const full: ApiMcpCallRow = { seq: 37, at: "2026-11-02T03:00:00Z", tool: "mysql_query", via: "mcp", client: "fixture",
  ok: true, ms: 12, args: '{"sql":"SELECT 37","limit":10}', output: '{"rows":[{"n":37}]}', chars: 19 };
let detail: McpDetail;

beforeEach(() => {
  document.body.innerHTML = ids.map(id => '<div id="' + id + '"></div>').join("");
  document.getElementById("pane")!.innerHTML = '<button id="r-hist"></button><textarea id="r-arg-sql" data-kind="string"></textarea><input id="r-arg-limit" data-kind="number">';
  const kind = { items: [], loaded: true, loading: false, error: null, cursors: [""], pageSize: 50, nextCursor: undefined, total: 0 };
  detail = {
    name: "mysql", tab: "run", config: null, editing: false, editType: null, editVals: null, oauthBusy: false,
    tools: { ...kind, items: [{ name: "mysql_query", inputSchema: { type: "object", properties: { sql: { type: "string" }, limit: { type: "number" } } } }] },
    resources: { ...kind }, prompts: { ...kind },
    run: { tool: "mysql_query", result: null, running: false, hist: [full], histTool: "mysql_query", histLoading: false,
      histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsPage: 0, callsMore: false, callsFull: {}, callsQ: "", callsGone: {}, callsAll: {},
    callsPendingPage: null, callsError: "", callsErrStatus: "", callsRetryTarget: null, callsRetryDir: null, callsSwitch: null,
    callsRequest: 0, callsActive: 0,
  };
  setMcpDetail(detail);
  setSelectedMcp("mysql");
  requests.length = 0;
  vi.stubGlobal("fetch", vi.fn(async (url: unknown) => {
    const path = String(url);
    requests.push(path);
    return { ok: true, status: 200, json: async () => path.includes("/tool-history?") ? { entries: [full] } : { call: full } };
  }));
});

afterEach(() => { history.histClose(); setMcpDetail(null); setSelectedMcp(null); vi.unstubAllGlobals(); });

describe("MCP Run history preview and refill", () => {
  it("opens the selected call's arguments and output, then refills without running it", async () => {
    detail.run.histTool = null;
    await history.loadRunHistory("mysql", "mysql_query");
    history.histOpen();
    const row = document.querySelector<HTMLButtonElement>("#r-hist-list .hist-row")!;
    expect(row.dataset.seq).toBe("37");
    row.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    await vi.waitFor(() => expect(document.getElementById("r-hist-view")!.textContent).toContain("rows"));
    expect(document.getElementById("r-hist-view")!.textContent).toContain("SELECT 37");
    row.click();
    await vi.waitFor(() => expect((document.getElementById("r-arg-sql") as HTMLTextAreaElement).value).toBe("SELECT 37"));
    expect((document.getElementById("r-arg-limit") as HTMLInputElement).value).toBe("10");
    expect(requests.filter(p => p.includes("/calls/"))).toEqual(["/api/mcps/mysql/calls/37", "/api/mcps/mysql/calls/37"]);
    expect(requests.some(p => p.endsWith("/call"))).toBe(false);
    expect(document.getElementById("r-hist-pop")).toBeNull();
  });

  it("refuses missing or invalid sequences before requesting a body or showing a spinner", async () => {
    history.histOpen();
    for (const seq of [NaN, Infinity, -1, 0, 1.5]) {
      await history.histPreview(seq);
      await history.applyRunHistory(seq);
    }
    expect(requests).toEqual([]);
    expect(document.querySelector("#r-hist-view .spinner")).toBeNull();
    expect(document.getElementById("r-hist-view")!.textContent).toContain("could not load that run");
  });
});

it("keeps the edited form when a recorded body was pruned", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ call: { ...full, args: "", output: "", bodyGone: true } }) })));
  const input = document.getElementById("r-arg-sql") as HTMLTextAreaElement;
  input.value = "SELECT keep_me";
  history.histOpen();
  await history.histPreview(37);
  expect(document.getElementById("r-hist-view")!.textContent).toContain("cleared");
  await history.applyRunHistory(37);
  expect(input.value).toBe("SELECT keep_me");
});

it("retries a failed index when the popover is reopened", async () => {
  detail.run.histTool = null;
  vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce({ ok: false, status: 503 }).mockResolvedValue({ ok: true, json: async () => ({ entries: [full] }) }));
  await history.loadRunHistory("mysql", "mysql_query");
  history.histOpen();
  await vi.waitFor(() => expect(document.querySelector("#r-hist-list .hist-row")?.getAttribute("data-seq")).toBe("37"));
});
